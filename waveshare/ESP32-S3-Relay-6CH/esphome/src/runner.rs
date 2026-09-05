//! Blocking `std::net` driver for one connection. Used by the host example and the firmware.

use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::sync::mpsc::{channel, Sender, TryRecvError};
use std::time::{Duration, Instant};

use crate::codec::Codec;
use crate::server::{Outbound, Server};
use crate::session::{Command, Event, Session};
use crate::{Error, Result};

pub const READ_POLL: Duration = Duration::from_millis(50);
pub const KEEPALIVE_IDLE: Duration = Duration::from_secs(60);
pub const KEEPALIVE_DEAD: Duration = Duration::from_secs(150);

/// Serve one client until it disconnects or errors. Commands go to `cmd_tx`, which may carry
/// any type convertible from [`Command`] so applications can multiplex their own events.
pub fn serve<T: From<Command>>(mut stream: TcpStream, server: &Server, codec: Codec, cmd_tx: &Sender<T>) -> Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(READ_POLL))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();

    let (tx, rx) = channel::<Outbound>();
    server.attach(tx);
    let mut session = Session::new(server.shared(), codec);
    // Noise sessions may have nothing to send up front; the client speaks first either way.

    let mut last_rx = Instant::now();
    let mut ping_sent = false;
    let mut chunk = [0u8; 1024];
    let result = loop {
        match stream.read(&mut chunk) {
            Ok(0) => break Err(Error::Closed),
            Ok(n) => {
                last_rx = Instant::now();
                ping_sent = false;
                let events = session.on_bytes(&chunk[..n]);
                let client_info = session.client_info().to_string();
                if let Err(e) = dispatch(&mut stream, events, cmd_tx, &client_info) {
                    break Err(e);
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) => break Err(e.into()),
        }

        // Fan-out from the application.
        loop {
            match rx.try_recv() {
                Ok(Outbound::State(u32::MAX, _)) => {}
                Ok(Outbound::State(key, state)) => {
                    if let Some(bytes) = session.state_update(key, &state)? {
                        stream.write_all(&bytes)?;
                    }
                }
                Ok(Outbound::Close) => return Ok(()),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }

        // Keepalive.
        let idle = last_rx.elapsed();
        if idle > KEEPALIVE_DEAD {
            break Err(Error::Frame("keepalive timeout"));
        }
        if idle > KEEPALIVE_IDLE && !ping_sent {
            stream.write_all(&session.ping_request()?)?;
            ping_sent = true;
        }
    };
    log::info!("connection from {} ({}) ended: {:?}", peer, session.client_info(), result.as_ref().err());
    let _ = cmd_tx.send(Command::ClientGone { subscribed: session.subscribed() }.into());
    result
}

fn dispatch<T: From<Command>>(stream: &mut TcpStream, events: Vec<Event>, cmd_tx: &Sender<T>, client_info: &str) -> Result<()> {
    for ev in events {
        match ev {
            Event::Send(bytes) => stream.write_all(&bytes)?,
            Event::Hello(info) => log::info!("client connected: {}", info),
            Event::Subscribed => {
                let _ = cmd_tx.send(Command::ClientSubscribed { client_info: client_info.to_string() }.into());
            }
            Event::Command(c) => {
                let _ = cmd_tx.send(c.into());
            }
            Event::Disconnect => {
                let _ = stream.flush();
                let _ = stream.shutdown(std::net::Shutdown::Both);
                return Err(Error::Closed);
            }
            Event::Error(e) => {
                // Earlier Send events (e.g. the Noise rejection) are already written.
                let _ = stream.flush();
                let _ = stream.shutdown(std::net::Shutdown::Write);
                return Err(e);
            }
        }
    }
    Ok(())
}
