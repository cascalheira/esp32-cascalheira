//! Plaintext framing of the ESPHome native API.
//!
//! Every frame is `0x00`, varint payload length, varint message type, then the protobuf bytes.
//! (The Noise-encrypted framing lives in `noise.rs`.)

use std::io::{Read, Write};

use crate::{varint, ApiMessage, Error, Result};

pub const PLAINTEXT_INDICATOR: u8 = 0x00;
pub const NOISE_INDICATOR: u8 = 0x01;
/// Refuse absurd payloads; ESPHome itself caps frames around a few KB.
pub const MAX_PAYLOAD: u32 = 64 * 1024;

/// A decoded frame: message type id and raw protobuf payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawMessage {
    pub id: u32,
    pub payload: Vec<u8>,
}

impl RawMessage {
    pub fn encode<M: ApiMessage>(msg: &M) -> RawMessage {
        RawMessage { id: M::ID, payload: msg.encode_to_vec() }
    }

    pub fn decode<M: ApiMessage>(&self) -> Result<M> {
        Ok(M::decode(self.payload.as_slice())?)
    }
}

/// Serialise a frame with the plaintext header.
pub fn encode_plaintext(msg: &RawMessage) -> Vec<u8> {
    let mut out = Vec::with_capacity(msg.payload.len() + 8);
    out.push(PLAINTEXT_INDICATOR);
    varint::encode(msg.payload.len() as u32, &mut out);
    varint::encode(msg.id, &mut out);
    out.extend_from_slice(&msg.payload);
    out
}

/// Read the rest of a plaintext frame after the indicator byte has already been consumed.
pub fn read_plaintext_body<R: Read>(r: &mut R) -> Result<RawMessage> {
    let len = varint::read(r)?;
    let id = varint::read(r)?;
    if len > MAX_PAYLOAD {
        return Err(Error::Frame("payload too large"));
    }
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload).map_err(map_eof)?;
    Ok(RawMessage { id, payload })
}

/// Read one plaintext frame including its indicator byte.
pub fn read_plaintext<R: Read>(r: &mut R) -> Result<RawMessage> {
    let mut ind = [0u8; 1];
    if r.read(&mut ind)? == 0 {
        return Err(Error::Closed);
    }
    if ind[0] != PLAINTEXT_INDICATOR {
        return Err(Error::Frame("expected plaintext indicator"));
    }
    read_plaintext_body(r)
}

pub fn write_plaintext<W: Write>(w: &mut W, msg: &RawMessage) -> Result<()> {
    w.write_all(&encode_plaintext(msg))?;
    Ok(())
}

pub(crate) fn map_eof(e: std::io::Error) -> Error {
    if e.kind() == std::io::ErrorKind::UnexpectedEof {
        Error::Closed
    } else {
        Error::Io(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{HelloRequest, HelloResponse};

    #[test]
    fn encodes_hello_frame() {
        let hello = HelloRequest { client_info: "test".into(), api_version_major: 1, api_version_minor: 10 };
        let raw = RawMessage::encode(&hello);
        assert_eq!(raw.id, 1);
        let bytes = encode_plaintext(&raw);
        assert_eq!(bytes[0], 0x00);
        assert_eq!(bytes[1] as usize, raw.payload.len());
        assert_eq!(bytes[2], 1);
        let back = read_plaintext(&mut std::io::Cursor::new(bytes)).unwrap();
        assert_eq!(back, raw);
        assert_eq!(back.decode::<HelloRequest>().unwrap(), hello);
    }

    #[test]
    fn multi_frame_stream_and_ids() {
        let a = RawMessage::encode(&HelloResponse { api_version_major: 1, api_version_minor: 10, server_info: "x".into(), name: "n".into() });
        let b = RawMessage::encode(&crate::proto::PingRequest::default());
        assert_eq!(b.id, 7);
        assert!(b.payload.is_empty());
        let mut stream = encode_plaintext(&a);
        stream.extend(encode_plaintext(&b));
        let mut cur = std::io::Cursor::new(stream);
        assert_eq!(read_plaintext(&mut cur).unwrap(), a);
        assert_eq!(read_plaintext(&mut cur).unwrap(), b);
        assert!(matches!(read_plaintext(&mut cur), Err(Error::Closed)));
        assert_eq!(crate::proto::message_name(2), "HelloResponse");
    }

    #[test]
    fn rejects_noise_indicator_and_oversize() {
        assert!(matches!(read_plaintext(&mut std::io::Cursor::new(vec![0x01, 0, 0])), Err(Error::Frame(_))));
        let mut big = vec![0x00];
        varint::encode(MAX_PAYLOAD + 1, &mut big);
        varint::encode(1, &mut big);
        assert!(matches!(read_plaintext(&mut std::io::Cursor::new(big)), Err(Error::Frame(_))));
    }
}
