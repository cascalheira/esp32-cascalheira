//! Frame codec over a byte buffer: turns raw stream bytes into messages and back.
//! `Plaintext` is implemented here; `Noise` wraps the encrypted variant.

use crate::frame::{RawMessage, MAX_PAYLOAD, NOISE_INDICATOR, PLAINTEXT_INDICATOR};
use crate::{varint, Error, Result};

pub enum Codec {
    Plaintext,
    Noise(Box<crate::noise::NoiseCodec>),
}

impl Codec {
    /// Try to take one complete message off the front of `buf`. `Ok(None)` = need more bytes.
    pub fn decode(&mut self, buf: &mut Vec<u8>) -> Result<Option<RawMessage>> {
        match self {
            Codec::Plaintext => decode_plaintext(buf),
            Codec::Noise(n) => n.decode(buf),
        }
    }

    /// Bytes to put on the wire for `msg`.
    pub fn encode(&mut self, msg: &RawMessage) -> Result<Vec<u8>> {
        match self {
            Codec::Plaintext => Ok(crate::frame::encode_plaintext(msg)),
            Codec::Noise(n) => n.encode(msg),
        }
    }

    /// Frames the codec wants to send before any application message (Noise handshake).
    pub fn take_pending(&mut self) -> Vec<Vec<u8>> {
        match self {
            Codec::Plaintext => Vec::new(),
            Codec::Noise(n) => n.take_pending(),
        }
    }

    /// True once application messages can flow.
    pub fn ready(&self) -> bool {
        match self {
            Codec::Plaintext => true,
            Codec::Noise(n) => n.ready(),
        }
    }
}

fn decode_plaintext(buf: &mut Vec<u8>) -> Result<Option<RawMessage>> {
    let Some(&ind) = buf.first() else { return Ok(None) };
    if ind == NOISE_INDICATOR {
        return Err(Error::Frame("client wants encryption but no key is configured"));
    }
    if ind != PLAINTEXT_INDICATOR {
        return Err(Error::Frame("bad indicator byte"));
    }
    let mut pos = 1;
    let Some((len, n)) = varint::parse(&buf[pos..])? else { return Ok(None) };
    pos += n;
    let Some((id, n)) = varint::parse(&buf[pos..])? else { return Ok(None) };
    pos += n;
    if len > MAX_PAYLOAD {
        return Err(Error::Frame("payload too large"));
    }
    let end = pos + len as usize;
    if buf.len() < end {
        return Ok(None);
    }
    let payload = buf[pos..end].to_vec();
    buf.drain(..end);
    Ok(Some(RawMessage { id, payload }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::encode_plaintext;
    use crate::proto::HelloRequest;

    #[test]
    fn decodes_incrementally() {
        let msg = RawMessage::encode(&HelloRequest { client_info: "abc".into(), api_version_major: 1, api_version_minor: 10 });
        let wire = encode_plaintext(&msg);
        let mut codec = Codec::Plaintext;
        let mut buf = Vec::new();
        for (i, b) in wire.iter().enumerate() {
            buf.push(*b);
            let got = codec.decode(&mut buf).unwrap();
            if i + 1 < wire.len() {
                assert!(got.is_none(), "should wait for more at byte {i}");
            } else {
                assert_eq!(got.unwrap(), msg);
                assert!(buf.is_empty());
            }
        }
        // Two frames back to back, second partially present.
        buf.extend(&wire);
        buf.extend(&wire[..3]);
        assert_eq!(codec.decode(&mut buf).unwrap().unwrap(), msg);
        assert!(codec.decode(&mut buf).unwrap().is_none());
        assert_eq!(buf.len(), 3);
    }

    #[test]
    fn plaintext_rejects_noise_client() {
        let mut buf = vec![0x01, 0x00, 0x00];
        assert!(matches!(Codec::Plaintext.decode(&mut buf), Err(Error::Frame(_))));
    }
}
