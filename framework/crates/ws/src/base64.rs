//! Standard base64 encoding (RFC 4648, `+/` alphabet, `=` padding) — just
//! enough to turn the 20-byte SHA-1 handshake digest into the textual
//! `Sec-WebSocket-Accept` header. Encode only; the server never decodes base64.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encode bytes as standard, padded base64.
pub fn encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        // Pack up to three bytes into a 24-bit big-endian group.
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let group = (b0 << 16) | (b1 << 8) | b2;

        out.push(ALPHABET[(group >> 18 & 0x3f) as usize] as char);
        out.push(ALPHABET[(group >> 12 & 0x3f) as usize] as char);
        // The third and fourth symbols become '=' when their source bytes are
        // absent (1 or 2 trailing input bytes).
        out.push(if chunk.len() > 1 {
            ALPHABET[(group >> 6 & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(group & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc4648_vectors() {
        // The canonical progression that exercises every padding case.
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foob"), "Zm9vYg==");
        assert_eq!(encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn encodes_high_bytes() {
        assert_eq!(encode(&[0xff, 0xff, 0xff]), "////");
        assert_eq!(encode(&[0x00, 0x00, 0x00]), "AAAA");
    }
}
