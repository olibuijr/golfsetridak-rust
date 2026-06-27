//! Little-endian `f32` byte (de)serialization.
//!
//! Embeddings are stored as opaque byte blobs in the B+tree by the CLI. We use
//! a fixed little-endian `f32` layout so a vector survives a store/load round
//! trip exactly, on any little-endian or big-endian host (we convert
//! explicitly, never `transmute`).

/// Encode a vector as a little-endian `f32` byte blob (4 bytes per element).
pub fn encode(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for &x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

/// Decode a little-endian `f32` byte blob back into a vector.
///
/// Returns `None` if the byte length isn't a multiple of 4 (i.e. the blob is
/// not a whole number of `f32`s), so a corrupt/short blob can never panic the
/// search path.
pub fn decode(bytes: &[u8]) -> Option<Vec<f32>> {
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        let arr = [chunk[0], chunk[1], chunk[2], chunk[3]];
        out.push(f32::from_le_bytes(arr));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_preserves_values() {
        let v = vec![0.0, 1.0, -1.5, 42.125, 1e30, -1e-30, f32::MAX, f32::MIN];
        let bytes = encode(&v);
        assert_eq!(bytes.len(), v.len() * 4);
        assert_eq!(decode(&bytes), Some(v));
    }

    #[test]
    fn round_trip_empty() {
        let v: Vec<f32> = Vec::new();
        let bytes = encode(&v);
        assert!(bytes.is_empty());
        assert_eq!(decode(&bytes), Some(v));
    }

    #[test]
    fn decode_rejects_non_multiple_of_four() {
        assert_eq!(decode(&[0, 1, 2]), None);
        assert_eq!(decode(&[0, 1, 2, 3, 4]), None);
    }

    #[test]
    fn known_byte_layout_is_little_endian() {
        // 1.0f32 == 0x3F800000, little-endian on the wire.
        assert_eq!(encode(&[1.0]), vec![0x00, 0x00, 0x80, 0x3F]);
        assert_eq!(decode(&[0x00, 0x00, 0x80, 0x3F]), Some(vec![1.0]));
    }

    #[test]
    fn handles_negative_zero_and_subnormal() {
        let v = vec![-0.0f32, f32::MIN_POSITIVE / 2.0];
        let back = decode(&encode(&v)).unwrap();
        assert_eq!(back.len(), 2);
        // -0.0 == 0.0 compares true, so check the sign bit survived exactly.
        assert!(back[0].is_sign_negative());
        assert_eq!(back[1], v[1]);
    }
}
