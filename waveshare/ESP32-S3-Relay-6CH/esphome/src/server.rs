//! Shared device state and fan-out of state updates to every live connection.

use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use crate::device::Device;
use crate::entity::{Registry, State};

/// Read-mostly data every session needs.
pub struct Shared {
    pub device: Device,
    pub registry: Registry,
    pub server_info: String,
    states: Mutex<HashMap<u32, State>>,
}

impl Shared {
    pub fn snapshot(&self) -> Vec<(u32, State)> {
        let s = self.states.lock().unwrap();
        // Deterministic order helps tests and log reading.
        let mut v: Vec<(u32, State)> = s.iter().map(|(k, v)| (*k, v.clone())).collect();
        v.sort_by_key(|(k, _)| *k);
        v
    }

    pub fn state(&self, key: u32) -> Option<State> {
        self.states.lock().unwrap().get(&key).cloned()
    }
}

/// Message from the server to a connection task.
#[derive(Debug, Clone, PartialEq)]
pub enum Outbound {
    State(u32, State),
    /// A log line (proto `LogLevel`, formatted bytes) for clients that subscribed to logs.
    Log(i32, Arc<Vec<u8>>),
    Close,
}

/// Application-facing handle. Cheap to clone.
#[derive(Clone)]
pub struct Server {
    shared: Arc<Shared>,
    conns: Arc<Mutex<Vec<Sender<Outbound>>>>,
}

impl Server {
    pub fn new(device: Device, registry: Registry, server_info: &str) -> Server {
        Server {
            shared: Arc::new(Shared { device, registry, server_info: server_info.into(), states: Mutex::new(HashMap::new()) }),
            conns: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn shared(&self) -> Arc<Shared> {
        self.shared.clone()
    }

    pub fn registry(&self) -> &Registry {
        &self.shared.registry
    }

    pub fn device(&self) -> &Device {
        &self.shared.device
    }

    /// Record a state and push it to every subscribed client.
    pub fn set_state(&self, key: u32, state: State) {
        {
            let mut s = self.shared.states.lock().unwrap();
            if s.get(&key) == Some(&state) {
                return;
            }
            s.insert(key, state.clone());
        }
        let mut conns = self.conns.lock().unwrap();
        conns.retain(|tx| tx.send(Outbound::State(key, state.clone())).is_ok());
    }

    /// Like `set_state` but always pushes, even if unchanged (for sensors that re-report).
    pub fn publish_state(&self, key: u32, state: State) {
        self.shared.states.lock().unwrap().insert(key, state.clone());
        let mut conns = self.conns.lock().unwrap();
        conns.retain(|tx| tx.send(Outbound::State(key, state.clone())).is_ok());
    }

    pub fn state(&self, key: u32) -> Option<State> {
        self.shared.state(key)
    }

    /// Fan a log line out to every connection; each session filters by its subscribed level.
    /// Cheap when nothing is connected. Never call from inside a connection's send path.
    pub fn log(&self, level: i32, line: Vec<u8>) {
        let mut conns = match self.conns.try_lock() {
            Ok(c) => c,
            Err(_) => return, // never block a logger
        };
        if conns.is_empty() {
            return;
        }
        let line = Arc::new(line);
        conns.retain(|tx| tx.send(Outbound::Log(level, line.clone())).is_ok());
    }

    /// Register a connection's outbound channel. Dropped receivers are pruned lazily.
    pub fn attach(&self, tx: Sender<Outbound>) {
        self.conns.lock().unwrap().push(tx);
    }

    pub fn connection_count(&self) -> usize {
        let mut conns = self.conns.lock().unwrap();
        conns.retain(|tx| tx.send(Outbound::State(u32::MAX, State::Bool(false))).is_ok());
        conns.len()
    }

    pub fn close_all(&self) {
        let mut conns = self.conns.lock().unwrap();
        for tx in conns.drain(..) {
            let _ = tx.send(Outbound::Close);
        }
    }
}
