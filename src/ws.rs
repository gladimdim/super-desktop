//! Minimal RFC 6455 WebSocket server support for the harness bridge.
//!
//! Deliberately small: the bridge pushes server→client text
//! frames and only needs to understand the client→server frames well enough to
//! notice a close and answer pings. No extensions, no fragmentation of outgoing
//! messages (payloads here are pane dumps of a few KiB, well under any limit
//! the client would negotiate), no compression.
//!
//! Verified against the RFC 6455 handshake vector in the tests below.

use std::io::{self, Read, Write};

const OP_TEXT: u8 = 0x1;
const OP_BINARY: u8 = 0x2;
const OP_CLOSE: u8 = 0x8;
const OP_PING: u8 = 0x9;
const OP_PONG: u8 = 0xA;

/// `Sec-WebSocket-Accept` for a client's `Sec-WebSocket-Key`.
pub fn accept_key(key: &str) -> String {
    tungstenite::handshake::derive_accept_key(key.as_bytes())
}

/// One decoded frame from the peer.
#[derive(Debug, PartialEq, Eq)]
pub enum Frame {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    Close,
    /// Continuation/unknown opcode: ignored.
    Other(u8),
}

/// Write the `101 Switching Protocols` response for an upgrade request.
pub fn handshake<W: Write>(out: &mut W, key: &str) -> io::Result<()> {
    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Accept: {}\r\n\r\n",
        accept_key(key)
    );
    out.write_all(response.as_bytes())?;
    out.flush()
}

/// Read one frame. `Ok(None)` means the peer closed cleanly.
///
/// Frames from a client are always masked (RFC 6455 §5.3); an unmasked frame is
/// a protocol error and is treated as a hang-up.
pub fn read_frame<R: Read>(input: &mut R) -> io::Result<Option<Frame>> {
    let mut header = [0u8; 2];
    match input.read_exact(&mut header) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let opcode = header[0] & 0x0F;
    let masked = header[1] & 0x80 != 0;
    if !masked || header[0] & 0x80 == 0 || header[0] & 0x70 != 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "masked unfragmented frame required"));
    }
    let mut len = (header[1] & 0x7F) as u64;
    if len == 126 {
        let mut buf = [0u8; 2];
        input.read_exact(&mut buf)?;
        len = u16::from_be_bytes(buf) as u64;
    } else if len == 127 {
        let mut buf = [0u8; 8];
        input.read_exact(&mut buf)?;
        len = u64::from_be_bytes(buf);
    }
    if len > 16384 || (opcode >= 8 && len > 125) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut mask = [0u8; 4];
    if masked {
        input.read_exact(&mut mask)?;
    }
    let mut payload = vec![0u8; len as usize];
    if len > 0 {
        input.read_exact(&mut payload)?;
    }
    if masked {
        apply_mask(&mut payload, mask);
    }
    Ok(Some(match opcode {
        OP_TEXT => Frame::Text(String::from_utf8(payload)
            .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())),
        OP_BINARY => Frame::Binary(payload),
        OP_PING => Frame::Ping(payload),
        OP_PONG => Frame::Pong(payload),
        OP_CLOSE => Frame::Close,
        other => Frame::Other(other),
    }))
}

/// Four-byte words expose the repeated XOR to the compiler's vectorizer.
/// Byte-array loads work at any alignment and on either endianness; no CPU
/// extension beyond the selected Rust target's baseline is required.
pub(crate) fn apply_mask(bytes: &mut [u8], mask: [u8; 4]) {
    let mask_word = u32::from_ne_bytes(mask);
    let mut chunks = bytes.chunks_exact_mut(4);
    for chunk in &mut chunks {
        let word = u32::from_ne_bytes(chunk.try_into().unwrap()) ^ mask_word;
        chunk.copy_from_slice(&word.to_ne_bytes());
    }
    for (i, byte) in chunks.into_remainder().iter_mut().enumerate() {
        *byte ^= mask[i];
    }
}

/// Write one unmasked frame (server→client frames must not be masked).
pub fn write_frame<W: Write>(out: &mut W, opcode: u8, payload: &[u8]) -> io::Result<()> {
    let mut header = [0u8; 10];
    header[0] = 0x80 | opcode; // FIN + opcode, no fragmentation
    let len = payload.len();
    let header_len = if len < 126 {
        header[1] = len as u8;
        2
    } else if len <= u16::MAX as usize {
        header[1] = 126;
        header[2..4].copy_from_slice(&(len as u16).to_be_bytes());
        4
    } else {
        header[1] = 127;
        header[2..10].copy_from_slice(&(len as u64).to_be_bytes());
        10
    };
    out.write_all(&header[..header_len])?;
    out.write_all(payload)?;
    out.flush()
}

pub fn write_text<W: Write>(out: &mut W, text: &str) -> io::Result<()> {
    write_frame(out, OP_TEXT, text.as_bytes())
}

/// Raw terminal bytes. Frame boundaries carry no character semantics: the
/// receiving emulator reassembles byte order only, so a UTF-8 character or an
/// escape sequence may be split across frames.
pub fn write_binary<W: Write>(out: &mut W, bytes: &[u8]) -> io::Result<()> {
    write_frame(out, OP_BINARY, bytes)
}

pub fn write_ping<W: Write>(out: &mut W, payload: &[u8]) -> io::Result<()> {
    write_frame(out, OP_PING, payload)
}

pub fn write_pong<W: Write>(out: &mut W, payload: &[u8]) -> io::Result<()> {
    write_frame(out, OP_PONG, payload)
}

pub fn write_close<W: Write>(out: &mut W, code: u16, reason: &str) -> io::Result<()> {
    let mut payload = code.to_be_bytes().to_vec();
    payload.extend_from_slice(reason.as_bytes());
    write_frame(out, OP_CLOSE, &payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handshake_accept_key_rfc_example() {
        // RFC 6455 §1.3: the sample key's expected accept value.
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn test_text_frame_layout() {
        let mut out = Vec::new();
        write_text(&mut out, "hi").unwrap();
        // FIN + text, length 2, unmasked payload.
        assert_eq!(out, vec![0x81, 0x02, b'h', b'i']);

        let mut long = Vec::new();
        write_text(&mut long, &"x".repeat(200)).unwrap();
        assert_eq!(long[0], 0x81);
        assert_eq!(long[1], 126);
        assert_eq!(u16::from_be_bytes([long[2], long[3]]), 200);
        assert_eq!(long.len(), 4 + 200);
    }

    #[test]
    fn outgoing_headers_cover_all_lengths_and_short_writes() {
        #[derive(Default)]
        struct ShortWriter { bytes: Vec<u8>, flushed: bool }
        impl Write for ShortWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let n = bytes.len().min(7);
                self.bytes.extend_from_slice(&bytes[..n]);
                Ok(n)
            }
            fn flush(&mut self) -> io::Result<()> {
                self.flushed = true;
                Ok(())
            }
        }
        for length in [0, 1, 125, 126, 127, 65535, 65536, 131072] {
            for opcode in [OP_TEXT, OP_BINARY] {
                let payload: Vec<_> = (0..length).map(|i| i as u8).collect();
                let mut expected = vec![0x80 | opcode];
                if length < 126 {
                    expected.push(length as u8);
                } else if length <= u16::MAX as usize {
                    expected.push(126);
                    expected.extend_from_slice(&(length as u16).to_be_bytes());
                } else {
                    expected.push(127);
                    expected.extend_from_slice(&(length as u64).to_be_bytes());
                }
                expected.extend_from_slice(&payload);
                let mut out = ShortWriter::default();
                write_frame(&mut out, opcode, &payload).unwrap();
                assert_eq!(out.bytes, expected);
                assert!(out.flushed);
            }
        }
        struct FailedWriter;
        impl Write for FailedWriter {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> { panic!("flushed after failed write") }
        }
        assert_eq!(write_text(&mut FailedWriter, "x").unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn test_read_maskted_client_frame() {
        // "hello" masked with key 0x37FA213D (RFC 6455 §5.7 example bytes).
        let mut frame = vec![0x81, 0x85, 0x37, 0xFA, 0x21, 0x3D];
        frame.extend_from_slice(&[0x7F, 0x9F, 0x4D, 0x51, 0x58]);
        let parsed = read_frame(&mut frame.as_slice()).unwrap().unwrap();
        assert_eq!(parsed, Frame::Text("Hello".to_string()));
    }

    #[test]
    fn test_read_close_and_clean_eof() {
        let mut close = vec![0x88, 0x82, 0x00, 0x00, 0x00, 0x00];
        close.extend_from_slice(&[0x03, 0xE8]);
        assert_eq!(read_frame(&mut close.as_slice()).unwrap(), Some(Frame::Close));
        // A peer that just drops the connection reads as a clean hang-up.
        assert_eq!(read_frame(&mut [].as_slice()).unwrap(), None);
    }

    #[test]
    fn masking_matches_bytes_for_unaligned_inputs_and_tails() {
        for mask in [[0; 4], [255; 4], [0x37, 0xfa, 0x21, 0x3d], [1, 2, 4, 8]] {
            for offset in 0..32 {
                for length in (0..=129).chain([255, 256, 257, 16383, 16384]) {
                    let mut bytes: Vec<u8> = (0..offset + length).map(|i| i as u8).collect();
                    let mut expected = bytes.clone();
                    for (i, byte) in expected[offset..].iter_mut().enumerate() {
                        *byte ^= mask[i % 4];
                    }
                    apply_mask(&mut bytes[offset..], mask);
                    assert_eq!(bytes, expected, "offset {offset}, length {length}");
                }
            }
        }
    }

    #[test]
    fn optimized_frame_reader_preserves_payloads_and_limits() {
        let mask = [0x37, 0xfa, 0x21, 0x3d];
        for size in [0, 1, 31, 32, 33, 125, 126, 127, 16384, 16385] {
            let payload: Vec<u8> = (0..size).map(|i| i as u8).collect();
            let mut frame = vec![0x82];
            if size < 126 {
                frame.push(0x80 | size as u8);
            } else {
                frame.push(0xfe);
                frame.extend_from_slice(&(size as u16).to_be_bytes());
            }
            frame.extend_from_slice(&mask);
            frame.extend(payload.iter().enumerate().map(|(i, &b)| b ^ mask[i % 4]));
            let result = read_frame(&mut frame.as_slice());
            if size > 16384 {
                assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
            } else {
                assert_eq!(result.unwrap(), Some(Frame::Binary(payload.clone())));
                frame[0] = 0x81;
                assert_eq!(read_frame(&mut frame.as_slice()).unwrap(),
                    Some(Frame::Text(String::from_utf8_lossy(&payload).into_owned())));
            }
        }
    }
}
