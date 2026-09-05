//! Noise_NNpsk0_25519_ChaChaPoly_SHA256 framing, byte-compatible with ESPHome's
//! `api_frame_helper_noise.cpp` and aioesphomeapi's `noise.py`.
//!
//! Wire sequence (client speaks first):
//! 1. client hello frame: `01 | len u16 BE | payload` (payload empty today; it is appended to the
//!    prologue `"NoiseAPIInit" + u16 BE len + payload`)
//! 2. server hello frame: `01 | len | 0x01 | name NUL | mac12 NUL`
//! 3. client handshake frame: `01 | len | 0x00 | noise msg 1 (e + tag)`
//! 4. server handshake frame: `01 | len | 0x00 | noise msg 2` (or `0x01 | reason` on rejection)
//! 5. data frames: `01 | len | ciphertext( type u16 BE | len u16 BE | protobuf ) + tag`

use base64::Engine;
use noise_protocol::patterns::noise_nn_psk0;
use noise_protocol::{CipherState, ErrorKind, HandshakeState, HandshakeStateBuilder};
use noise_rust_crypto::{ChaCha20Poly1305, Sha256, X25519};

use crate::frame::{RawMessage, NOISE_INDICATOR};
use crate::{Error, Result};

const PROLOGUE_INIT: &[u8] = b"NoiseAPIInit";
const HANDSHAKE_OK: u8 = 0x00;
const HANDSHAKE_REJECT: u8 = 0x01;
const MAX_HANDSHAKE_FRAME: usize = 128;
const MAX_DATA_FRAME: usize = 32768;
/// Exact text aioesphomeapi matches to raise `InvalidEncryptionKeyAPIError` (no NUL).
pub const REJECT_MAC_FAILURE: &str = "Handshake MAC failure";

type Hs = HandshakeState<X25519, ChaCha20Poly1305, Sha256>;

enum State {
    ClientHello,
    Handshake(Box<Hs>),
    Transport { send: CipherState<ChaCha20Poly1305>, recv: CipherState<ChaCha20Poly1305> },
    Failed,
}

pub struct NoiseCodec {
    psk: [u8; 32],
    name: String,
    mac12: String,
    state: State,
    pending: Vec<Vec<u8>>,
}

/// Decode the base64 PSK as configured in HA / ESPHome (`api: encryption: key:`).
pub fn parse_psk(b64: &str) -> std::result::Result<[u8; 32], String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|e| format!("invalid base64: {e}"))?;
    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| format!("key must decode to 32 bytes, got {}", bytes.len()))
}

/// Generate a fresh random PSK, base64 encoded, for provisioning.
pub fn generate_psk_b64() -> String {
    use noise_protocol::DH;
    let k = X25519::genkey(); // 32 random bytes from the OS RNG
    base64::engine::general_purpose::STANDARD.encode(k.as_ref() as &[u8])
}

impl NoiseCodec {
    /// `name` is the node name; `mac12` the MAC as 12 lowercase hex chars without separators.
    pub fn new(psk: [u8; 32], name: &str, mac12: &str) -> NoiseCodec {
        NoiseCodec { psk, name: name.into(), mac12: mac12.to_lowercase(), state: State::ClientHello, pending: Vec::new() }
    }

    pub fn ready(&self) -> bool {
        matches!(self.state, State::Transport { .. })
    }

    pub fn take_pending(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.pending)
    }

    fn frame(payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(payload.len() + 3);
        out.push(NOISE_INDICATOR);
        out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    fn reject(&mut self, reason: &str) -> Error {
        let mut p = vec![HANDSHAKE_REJECT];
        p.extend_from_slice(reason.as_bytes());
        self.pending.push(Self::frame(&p));
        self.state = State::Failed;
        Error::Handshake(reason.to_string())
    }

    /// Take one complete frame from `buf`, advancing the handshake or yielding a message.
    pub fn decode(&mut self, buf: &mut Vec<u8>) -> Result<Option<RawMessage>> {
        loop {
            if matches!(self.state, State::Failed) {
                return Err(Error::Handshake("connection rejected".into()));
            }
            if buf.len() < 3 {
                return Ok(None);
            }
            if buf[0] != NOISE_INDICATOR {
                // A plaintext client on an encrypted device: tell it so HA can prompt for the key.
                return Err(self.reject("Bad indicator byte"));
            }
            let len = u16::from_be_bytes([buf[1], buf[2]]) as usize;
            let limit = if self.ready() { MAX_DATA_FRAME } else { MAX_HANDSHAKE_FRAME };
            if len > limit {
                return Err(self.reject(if self.ready() { "Bad data packet" } else { "Bad handshake packet len" }));
            }
            if buf.len() < 3 + len {
                return Ok(None);
            }
            let payload: Vec<u8> = buf[3..3 + len].to_vec();
            buf.drain(..3 + len);

            match std::mem::replace(&mut self.state, State::Failed) {
                State::ClientHello => {
                    let mut prologue = PROLOGUE_INIT.to_vec();
                    prologue.extend_from_slice(&(len as u16).to_be_bytes());
                    prologue.extend_from_slice(&payload);
                    let mut b = HandshakeStateBuilder::<X25519>::new();
                    b.set_pattern(noise_nn_psk0()).set_is_initiator(false).set_prologue(&prologue);
                    let mut hs: Hs = b.build_handshake_state();
                    hs.push_psk(&self.psk);
                    // Server hello: chosen proto, node name, mac (both NUL terminated).
                    let mut hello = vec![0x01u8];
                    hello.extend_from_slice(self.name.as_bytes());
                    hello.push(0);
                    hello.extend_from_slice(self.mac12.as_bytes());
                    hello.push(0);
                    self.pending.push(Self::frame(&hello));
                    self.state = State::Handshake(Box::new(hs));
                }
                State::Handshake(mut hs) => {
                    if payload.is_empty() {
                        return Err(self.reject("Empty handshake message"));
                    }
                    if payload[0] != HANDSHAKE_OK {
                        return Err(self.reject("Bad handshake error byte"));
                    }
                    if let Err(e) = hs.read_message_vec(&payload[1..]) {
                        let reason = match e.kind() {
                            ErrorKind::Decryption => REJECT_MAC_FAILURE,
                            _ => "Handshake error",
                        };
                        return Err(self.reject(reason));
                    }
                    let msg2 = hs.write_message_vec(&[]).map_err(|e| Error::Handshake(e.to_string()))?;
                    let mut p = vec![HANDSHAKE_OK];
                    p.extend_from_slice(&msg2);
                    self.pending.push(Self::frame(&p));
                    if !hs.completed() {
                        return Err(Error::Handshake("handshake did not complete".into()));
                    }
                    // (initiator->responder, responder->initiator)
                    let (recv, send) = hs.get_ciphers();
                    self.state = State::Transport { send, recv };
                }
                State::Transport { send, mut recv } => {
                    let pt = match recv.decrypt_vec(&payload) {
                        Ok(pt) => pt,
                        Err(()) => return Err(Error::Handshake("data decryption failed".into())),
                    };
                    self.state = State::Transport { send, recv };
                    if pt.len() < 4 {
                        return Err(Error::Frame("bad data packet"));
                    }
                    let id = u16::from_be_bytes([pt[0], pt[1]]) as u32;
                    let dlen = u16::from_be_bytes([pt[2], pt[3]]) as usize;
                    if dlen > pt.len() - 4 {
                        return Err(Error::Frame("bad data packet length"));
                    }
                    return Ok(Some(RawMessage { id, payload: pt[4..4 + dlen].to_vec() }));
                }
                State::Failed => unreachable!(),
            }
        }
    }

    pub fn encode(&mut self, msg: &RawMessage) -> Result<Vec<u8>> {
        let State::Transport { send, .. } = &mut self.state else {
            return Err(Error::Handshake("cannot send before handshake completes".into()));
        };
        if msg.payload.len() > u16::MAX as usize {
            return Err(Error::Frame("message too large for noise frame"));
        }
        let mut pt = Vec::with_capacity(msg.payload.len() + 4);
        pt.extend_from_slice(&(msg.id as u16).to_be_bytes());
        pt.extend_from_slice(&(msg.payload.len() as u16).to_be_bytes());
        pt.extend_from_slice(&msg.payload);
        let ct = send.encrypt_vec(&pt);
        Ok(Self::frame(&ct))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::HelloRequest;

    fn client_hs(psk: &[u8; 32]) -> Hs {
        let mut b = HandshakeStateBuilder::<X25519>::new();
        b.set_pattern(noise_nn_psk0()).set_is_initiator(true).set_prologue(b"NoiseAPIInit\x00\x00");
        let mut hs: Hs = b.build_handshake_state();
        hs.push_psk(psk);
        hs
    }

    #[test]
    fn full_handshake_and_data_round_trip() {
        let psk = parse_psk(&generate_psk_b64()).unwrap();
        let mut server = NoiseCodec::new(psk, "relay6-test", "1051DB47ABB0");
        let mut client = client_hs(&psk);
        let mut buf = Vec::new();

        // 1. client hello (empty) + 3. client handshake msg 1, in one write like aioesphomeapi.
        buf.extend_from_slice(&[0x01, 0x00, 0x00]);
        let msg1 = client.write_message_vec(&[]).unwrap();
        let mut f = vec![0x00];
        f.extend(&msg1);
        buf.extend(NoiseCodec::frame(&f));

        assert!(server.decode(&mut buf).unwrap().is_none());
        assert!(server.ready());
        let pending = server.take_pending();
        assert_eq!(pending.len(), 2);
        // 2. server hello
        let hello = &pending[0];
        assert_eq!(hello[0], 0x01);
        let body = &hello[3..];
        assert_eq!(body[0], 0x01);
        assert_eq!(&body[1..], b"relay6-test\x001051db47abb0\x00");
        // 4. server handshake msg 2
        let hs2 = &pending[1];
        assert_eq!(hs2[3], 0x00);
        client.read_message_vec(&hs2[4..]).unwrap();
        assert!(client.completed());
        let (mut c_send, mut c_recv) = client.get_ciphers();

        // Data: client -> server
        let req = RawMessage::encode(&HelloRequest { client_info: "c".into(), api_version_major: 1, api_version_minor: 16 });
        let mut pt = (req.id as u16).to_be_bytes().to_vec();
        pt.extend((req.payload.len() as u16).to_be_bytes());
        pt.extend(&req.payload);
        let ct = c_send.encrypt_vec(&pt);
        buf.extend(NoiseCodec::frame(&ct));
        let got = server.decode(&mut buf).unwrap().unwrap();
        assert_eq!(got, req);

        // Data: server -> client
        let resp = RawMessage { id: 2, payload: vec![1, 2, 3] };
        let wire = server.encode(&resp).unwrap();
        assert_eq!(wire[0], 0x01);
        let pt = c_recv.decrypt_vec(&wire[3..]).unwrap();
        assert_eq!(&pt[..4], &[0, 2, 0, 3]);
        assert_eq!(&pt[4..], &[1, 2, 3]);
    }

    #[test]
    fn wrong_psk_gets_exact_reject_text() {
        let good = parse_psk(&generate_psk_b64()).unwrap();
        let bad = parse_psk(&generate_psk_b64()).unwrap();
        let mut server = NoiseCodec::new(good, "n", "000000000000");
        let mut client = client_hs(&bad);
        let mut buf = vec![0x01, 0x00, 0x00];
        let mut f = vec![0x00];
        f.extend(client.write_message_vec(&[]).unwrap());
        buf.extend(NoiseCodec::frame(&f));
        assert!(matches!(server.decode(&mut buf), Err(Error::Handshake(_))));
        let pending = server.take_pending();
        let reject = &pending[1];
        assert_eq!(reject[3], 0x01);
        assert_eq!(&reject[4..], REJECT_MAC_FAILURE.as_bytes());
    }

    #[test]
    fn plaintext_client_is_told_to_encrypt() {
        let mut server = NoiseCodec::new([0u8; 32], "n", "000000000000");
        let mut buf = vec![0x00, 0x05, 0x01, 1, 2, 3, 4, 5];
        assert!(server.decode(&mut buf).is_err());
        let p = server.take_pending();
        assert_eq!(&p[0][4..], b"Bad indicator byte");
    }

    #[test]
    fn psk_parsing() {
        assert!(parse_psk("MDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDA=").is_ok());
        assert!(parse_psk("short").is_err());
        assert_eq!(generate_psk_b64().len(), 44);
    }
}
