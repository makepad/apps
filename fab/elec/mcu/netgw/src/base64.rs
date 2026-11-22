//! RFC 4648 base64, with canonical padding and strict decoding.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn encode(input: &[u8]) -> String {
    let mut out = String::new();
    for bytes in input.chunks(3) {
        let a = bytes[0];
        let b = bytes.get(1).copied().unwrap_or(0);
        let c = bytes.get(2).copied().unwrap_or(0);
        out.push(ALPHABET[(a >> 2) as usize] as char);
        out.push(ALPHABET[((a & 3) << 4 | b >> 4) as usize] as char);
        out.push(if bytes.len() > 1 { ALPHABET[((b & 15) << 2 | c >> 6) as usize] as char } else { '=' });
        out.push(if bytes.len() > 2 { ALPHABET[(c & 63) as usize] as char } else { '=' });
    }
    out
}

pub fn decode(input: &str) -> Option<Vec<u8>> {
    if input.len() % 4 != 0 { return None; }
    let mut out = Vec::new();
    for (i, chunk) in input.as_bytes().chunks_exact(4).enumerate() {
        let value = |b| ALPHABET.iter().position(|&c| c == b).map(|n| n as u8);
        let a = value(chunk[0])?;
        let b = value(chunk[1])?;
        let last = (i + 1) * 4 == input.len();
        out.push(a << 2 | b >> 4);
        if chunk[2] == b'=' {
            if !last || chunk[3] != b'=' || b & 15 != 0 { return None; }
        } else {
            let c = value(chunk[2])?;
            out.push(b << 4 | c >> 2);
            if chunk[3] == b'=' {
                if !last || c & 3 != 0 { return None; }
            } else { out.push(c << 6 | value(chunk[3])?); }
        }
    }
    Some(out)
}
