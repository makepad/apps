//! Standard base64 (RFC 4648, padded), for pictures in MCP results.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[n as usize & 63] as char } else { '=' });
    }
    out
}

pub fn decode(text: &str) -> Result<Vec<u8>, String> {
    let digits: Vec<u8> = text.bytes().filter(|byte| !byte.is_ascii_whitespace()).collect();
    if digits.len() % 4 != 0 {
        return Err("base64 length is not a multiple of 4".into());
    }
    let value = |byte: u8| -> Result<u32, String> {
        ALPHABET.iter().position(|digit| *digit == byte).map(|index| index as u32).ok_or_else(|| format!("`{}` is not a base64 digit", byte as char))
    };
    let mut out = Vec::with_capacity(digits.len() / 4 * 3);
    for chunk in digits.chunks(4) {
        let pad = chunk.iter().rev().take_while(|byte| **byte == b'=').count();
        if pad > 2 {
            return Err("bad base64 padding".into());
        }
        let mut n = 0u32;
        for byte in &chunk[..4 - pad] {
            n = n << 6 | value(*byte)?;
        }
        n <<= 6 * pad as u32;
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_known_values() {
        assert_eq!(encode(b"Man"), "TWFu");
        assert_eq!(encode(b"Ma"), "TWE=");
        assert_eq!(encode(b"M"), "TQ==");
        for len in 0..40 {
            let bytes: Vec<u8> = (0..len).map(|n| (n * 37 + 11) as u8).collect();
            assert_eq!(decode(&encode(&bytes)).unwrap(), bytes);
        }
        assert!(decode("TQ=").is_err());
        assert!(decode("T!==").is_err());
    }
}
