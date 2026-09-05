//! Protobuf-style unsigned LEB128 varints, used by the plaintext frame header.

use std::io::Read;

use crate::{Error, Result};

pub fn encode(mut v: u32, out: &mut Vec<u8>) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Read one varint byte-by-byte from a stream (headers are tiny, so no buffering needed).
pub fn read<R: Read>(r: &mut R) -> Result<u32> {
    let mut result: u32 = 0;
    let mut shift = 0;
    loop {
        let mut b = [0u8; 1];
        match r.read(&mut b)? {
            0 => return Err(Error::Closed),
            _ => {}
        }
        result |= ((b[0] & 0x7f) as u32) << shift;
        if b[0] & 0x80 == 0 {
            return Ok(result);
        }
        shift += 7;
        if shift >= 32 {
            return Err(Error::Frame("varint too long"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for v in [0u32, 1, 127, 128, 300, 16383, 16384, 0xffff, u32::MAX] {
            let mut buf = Vec::new();
            encode(v, &mut buf);
            let mut cur = std::io::Cursor::new(buf.clone());
            assert_eq!(read(&mut cur).unwrap(), v, "value {v} bytes {buf:?}");
        }
        let mut buf = Vec::new();
        encode(300, &mut buf);
        assert_eq!(buf, vec![0xac, 0x02]);
    }

    #[test]
    fn errors() {
        assert!(matches!(read(&mut std::io::Cursor::new(vec![])), Err(Error::Closed)));
        assert!(matches!(read(&mut std::io::Cursor::new(vec![0x80; 6])), Err(Error::Frame(_))));
    }
}

/// Parse a varint from the start of a buffer. `Ok(None)` if more bytes are needed.
pub fn parse(buf: &[u8]) -> Result<Option<(u32, usize)>> {
    let mut result: u32 = 0;
    for (i, b) in buf.iter().enumerate() {
        if i >= 5 {
            return Err(Error::Frame("varint too long"));
        }
        result |= ((b & 0x7f) as u32) << (7 * i);
        if b & 0x80 == 0 {
            return Ok(Some((result, i + 1)));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    #[test]
    fn partial_and_complete() {
        assert_eq!(parse(&[]).unwrap(), None);
        assert_eq!(parse(&[0xac]).unwrap(), None);
        assert_eq!(parse(&[0xac, 0x02, 0xff]).unwrap(), Some((300, 2)));
        assert!(parse(&[0x80; 6]).is_err());
    }
}
