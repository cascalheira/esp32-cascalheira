//! Device-side implementation of the ESPHome native API.
//!
//! Transport-agnostic: the server logic works over any `std::io::Read + Write`, so it is
//! unit-tested on the host (against the official `aioesphomeapi` client) and used unchanged
//! on the ESP32 over `std::net::TcpStream`.

pub mod codec;
pub mod device;
pub mod entity;
pub mod frame;
pub mod noise;
pub mod runner;
pub mod server;
pub mod session;
pub mod varint;

pub use device::Device;
pub use entity::{Kind, Meta, Registry, State, UpdateState};
pub use server::Server;
pub use session::{Command, Event, Session};

pub mod proto {
    #![allow(clippy::all, non_camel_case_types)]
    pub mod api {
        include!("proto/api.rs");
    }
    pub mod ids {
        include!("proto/ids.rs");
    }
    pub use api::*;
    pub use ids::message_name;
}

/// Every API message carries a numeric type id (from `option (id)` in `api.proto`).
pub trait ApiMessage: prost::Message + Default {
    const ID: u32;
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("protobuf decode: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("bad frame: {0}")]
    Frame(&'static str),
    #[error("noise handshake failed: {0}")]
    Handshake(String),
    #[error("peer closed the connection")]
    Closed,
}

pub type Result<T> = std::result::Result<T, Error>;
