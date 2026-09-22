//! Minimal RFC 6455 WebSocket server support for the harness bridge.
//!
//! Deliberately dependency-free and small: the bridge pushes server→client text
//! frames and only needs to understand the client→server frames well enough to
//! notice a close and answer pings. No extensions, no fragmentation of outgoing
//! messages (payloads here are pane dumps of a few KiB, well under any limit
//! the client would negotiate), no compression.
//!
//! Verified against the RFC 6455 handshake vector in the tests below.

use std::io::{self, Read, Write};

/// The magic GUID from RFC 6455 §4.2.2.
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

const OP_TEXT: u8 = 0x1;
const OP_BINARY: u8 = 0x2;
const OP_CLOSE: u8 = 0x8;
const OP_PING: u8 = 0x9;
const OP_PONG: u8 = 0xA;

/// `Sec-WebSocket-Accept` for a client's `Sec-WebSocket-Key`.
pub fn accept_key(key: &str) -> String {
    base64(&sha1(format!("{key}{WS_GUID}").as_bytes()))
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
        for (i, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[i % 4];
        }
    }
    Ok(Some(match opcode {
        OP_TEXT => Frame::Text(String::from_utf8_lossy(&payload).to_string()),
        OP_BINARY => Frame::Binary(payload),
        OP_PING => Frame::Ping(payload),
        OP_PONG => Frame::Pong(payload),
        OP_CLOSE => Frame::Close,
        other => Frame::Other(other),
    }))
}

/// Write one unmasked frame (server→client frames must not be masked).
pub fn write_frame<W: Write>(out: &mut W, opcode: u8, payload: &[u8]) -> io::Result<()> {
    let mut header = Vec::with_capacity(10);
    header.push(0x80 | opcode); // FIN + opcode, no fragmentation
    let len = payload.len();
    if len < 126 {
        header.push(len as u8);
    } else if len <= u16::MAX as usize {
        header.push(126);
        header.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        header.push(127);
        header.extend_from_slice(&(len as u64).to_be_bytes());
    }
    out.write_all(&header)?;
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

/// SHA-1 (RFC 3174). WebSocket's handshake is the only user, so this stays a
/// tiny local implementation instead of pulling in a hashing crate.
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476, 0xC3D2_E1F0];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let tmp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = tmp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }

    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// Standard base64 with padding.
pub fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(triple >> 18) as usize & 0x3F] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3F] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 0x3F] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 0x3F] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn test_sha1_matches_published_vectors() {
        assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(hex(&sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            hex(&sha1(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
        // 64 bytes: exercises the "padding needs a second block" path.
        assert_eq!(
            hex(&sha1(&vec![b'a'; 64])),
            "0098ba824b5c16427bd7a1122a5a442a25ec644d"
        );
    }

    #[test]
    fn test_base64_pads_correctly() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

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
}
