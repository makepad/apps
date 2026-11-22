//! Pixel dimensions from a PNG or JPEG header, without decoding.

/// PNG pixel dimensions from the IHDR chunk (signature + first chunk).
pub fn png_dims(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[0..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    (width > 0 && height > 0).then_some((width, height))
}

/// JPEG pixel dimensions from the first SOF0/1/2 marker.
pub fn jpeg_dims(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return None;
    }
    let mut at = 2usize;
    while at + 9 < bytes.len() {
        if bytes[at] != 0xFF {
            return None;
        }
        let marker = bytes[at + 1];
        // Standalone markers without a length.
        if (0xD0..=0xD9).contains(&marker) || marker == 0x01 {
            at += 2;
            continue;
        }
        let len = u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]) as usize;
        if len < 2 {
            return None;
        }
        if matches!(marker, 0xC0 | 0xC1 | 0xC2) {
            if at + 9 >= bytes.len() {
                return None;
            }
            let height = u16::from_be_bytes([bytes[at + 5], bytes[at + 6]]) as u32;
            let width = u16::from_be_bytes([bytes[at + 7], bytes[at + 8]]) as u32;
            return (width > 0 && height > 0).then_some((width, height));
        }
        at += 2 + len;
    }
    None
}
