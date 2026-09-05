//! One client connection's protocol state, free of any I/O. Feed it bytes, get back events.

use std::sync::Arc;

use crate::codec::Codec;
use crate::entity::State;
use crate::frame::RawMessage;
use crate::proto::{self, message_name};
use crate::server::Shared;
use crate::{ApiMessage, Error, Result};

pub const API_VERSION_MAJOR: u32 = 1;
pub const API_VERSION_MINOR: u32 = 16;

/// Something the application must act on.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Switch { key: u32, on: bool },
    Number { key: u32, value: f32 },
    Select { key: u32, option: String },
    Button { key: u32 },
    Service { key: u32, args: Vec<proto::ExecuteServiceArgument> },
    /// HA answered our `GetTimeRequest`.
    Time { epoch_seconds: u32, timezone: String },
    /// A client finished its handshake and subscribed to states (emitted by the runner).
    ClientSubscribed { client_info: String },
    /// A connection ended (emitted by the runner).
    ClientGone { subscribed: bool },
}

#[derive(Debug)]
pub enum Event {
    /// Bytes to write to the peer.
    Send(Vec<u8>),
    /// Client identified itself (`HelloRequest.client_info`).
    Hello(String),
    /// Client subscribed to state updates.
    Subscribed,
    Command(Command),
    /// Close the connection after flushing.
    Disconnect,
    /// Protocol failure: flush earlier `Send` events (e.g. a Noise rejection), then close.
    Error(Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    AwaitHello,
    Connected,
    Closing,
}

pub struct Session {
    shared: Arc<Shared>,
    codec: Codec,
    buf: Vec<u8>,
    phase: Phase,
    subscribed: bool,
    client_info: String,
}

impl Session {
    pub fn new(shared: Arc<Shared>, codec: Codec) -> Session {
        Session { shared, codec, buf: Vec::with_capacity(1024), phase: Phase::AwaitHello, subscribed: false, client_info: String::new() }
    }

    pub fn subscribed(&self) -> bool {
        self.subscribed
    }

    pub fn client_info(&self) -> &str {
        &self.client_info
    }

    /// Feed bytes from the wire; returns events in order. An `Event::Error` or
    /// `Event::Disconnect` is always last and means the connection must be closed after the
    /// preceding `Send` events have been written.
    pub fn on_bytes(&mut self, data: &[u8]) -> Vec<Event> {
        self.buf.extend_from_slice(data);
        let mut events = Vec::new();
        loop {
            let decoded = self.codec.decode(&mut self.buf);
            // Handshake frames (or a rejection) the codec wants on the wire, in order.
            for f in self.codec.take_pending() {
                events.push(Event::Send(f));
            }
            let msg = match decoded {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(e) => {
                    events.push(Event::Error(e));
                    break;
                }
            };
            if let Err(e) = self.handle(msg, &mut events) {
                events.push(Event::Error(e));
                break;
            }
            if self.phase == Phase::Closing {
                break;
            }
        }
        events
    }

    /// Wire bytes for a state change, if this client is subscribed.
    pub fn state_update(&mut self, key: u32, state: &State) -> Result<Option<Vec<u8>>> {
        if !self.subscribed {
            return Ok(None);
        }
        match self.shared.registry.get(key).and_then(|e| e.state_message(state)) {
            Some(m) => Ok(Some(self.codec.encode(&m)?)),
            None => Ok(None),
        }
    }

    /// Wire bytes for a keepalive ping.
    pub fn ping_request(&mut self) -> Result<Vec<u8>> {
        self.codec.encode(&RawMessage::encode(&proto::PingRequest::default()))
    }

    /// Wire bytes asking HA for the current time.
    pub fn get_time_request(&mut self) -> Result<Vec<u8>> {
        self.codec.encode(&RawMessage::encode(&proto::GetTimeRequest::default()))
    }

    fn send<M: ApiMessage>(&mut self, msg: &M, events: &mut Vec<Event>) -> Result<()> {
        let bytes = self.codec.encode(&RawMessage::encode(msg))?;
        events.push(Event::Send(bytes));
        Ok(())
    }

    #[allow(deprecated)]
    fn handle(&mut self, msg: RawMessage, events: &mut Vec<Event>) -> Result<()> {
        log::trace!("rx {} ({} bytes)", message_name(msg.id), msg.payload.len());
        if self.phase == Phase::AwaitHello {
            if msg.id != proto::HelloRequest::ID {
                return Err(Error::Frame("first message must be HelloRequest"));
            }
            let hello: proto::HelloRequest = msg.decode()?;
            self.client_info = hello.client_info.clone();
            self.phase = Phase::Connected;
            let resp = proto::HelloResponse {
                api_version_major: API_VERSION_MAJOR,
                api_version_minor: API_VERSION_MINOR,
                server_info: self.shared.server_info.clone(),
                name: self.shared.device.name.clone(),
            };
            self.send(&resp, events)?;
            events.push(Event::Hello(hello.client_info));
            return Ok(());
        }

        match msg.id {
            proto::PingRequest::ID => self.send(&proto::PingResponse::default(), events)?,
            // Removed in ESPHome 2026.1; aioesphomeapi still sends it with login=True and
            // never waits for a reply, so ignore it exactly like current ESPHome does.
            proto::AuthenticationRequest::ID => {}
            proto::DeviceCapabilitiesRequest::ID => {
                self.send(&proto::DeviceCapabilitiesResponse::default(), events)?
            }
            proto::PingResponse::ID => {}
            proto::DisconnectRequest::ID => {
                self.send(&proto::DisconnectResponse::default(), events)?;
                self.phase = Phase::Closing;
                events.push(Event::Disconnect);
            }
            proto::DisconnectResponse::ID => {
                self.phase = Phase::Closing;
                events.push(Event::Disconnect);
            }
            proto::DeviceInfoRequest::ID => {
                let info = self.shared.device.device_info();
                self.send(&info, events)?;
            }
            proto::ListEntitiesRequest::ID => {
                let list: Vec<RawMessage> = self.shared.registry.iter().map(|e| e.list_message()).collect();
                for m in list {
                    events.push(Event::Send(self.codec.encode(&m)?));
                }
                self.send(&proto::ListEntitiesDoneResponse::default(), events)?;
            }
            proto::SubscribeStatesRequest::ID => {
                self.subscribed = true;
                let snapshot = self.shared.snapshot();
                for (key, state) in snapshot {
                    if let Some(bytes) = self.state_update(key, &state)? {
                        events.push(Event::Send(bytes));
                    }
                }
                events.push(Event::Subscribed);
                // ESPHome devices ask HA for the time once a client is fully up.
                let t = self.get_time_request()?;
                events.push(Event::Send(t));
            }
            proto::SwitchCommandRequest::ID => {
                let c: proto::SwitchCommandRequest = msg.decode()?;
                events.push(Event::Command(Command::Switch { key: c.key, on: c.state }));
            }
            proto::NumberCommandRequest::ID => {
                let c: proto::NumberCommandRequest = msg.decode()?;
                events.push(Event::Command(Command::Number { key: c.key, value: c.state }));
            }
            proto::SelectCommandRequest::ID => {
                let c: proto::SelectCommandRequest = msg.decode()?;
                events.push(Event::Command(Command::Select { key: c.key, option: c.state }));
            }
            proto::ButtonCommandRequest::ID => {
                let c: proto::ButtonCommandRequest = msg.decode()?;
                events.push(Event::Command(Command::Button { key: c.key }));
            }
            proto::ExecuteServiceRequest::ID => {
                let c: proto::ExecuteServiceRequest = msg.decode()?;
                events.push(Event::Command(Command::Service { key: c.key, args: c.args }));
            }
            proto::GetTimeResponse::ID => {
                let t: proto::GetTimeResponse = msg.decode()?;
                events.push(Event::Command(Command::Time { epoch_seconds: t.epoch_seconds, timezone: t.timezone }));
            }
            proto::GetTimeRequest::ID => {
                // A client asking us for time: answer with what we have (0 = unknown).
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as u32)
                    .unwrap_or(0);
                self.send(&proto::GetTimeResponse { epoch_seconds: now, ..Default::default() }, events)?;
            }
            // Subscriptions we do not serve: HA tolerates silence on these.
            proto::SubscribeLogsRequest::ID
            | proto::SubscribeHomeassistantServicesRequest::ID
            | proto::SubscribeHomeAssistantStatesRequest::ID => {
                log::debug!("ignoring {}", message_name(msg.id));
            }
            other => log::debug!("unhandled message {} ({})", message_name(other), other),
        }
        Ok(())
    }
}
