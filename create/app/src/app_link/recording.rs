use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

/// Wall-clock capture stamps the actual present times. A gap in presents contributes to the
/// movie's duration without contributing to the frame count. Read only container headers.
pub(super) fn duration(path: &Path) -> Result<f64, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut end = file.metadata().map_err(|e| e.to_string())?.len();
    let mut pos = 0;
    let mut movie = false;
    while end.saturating_sub(pos) >= 8 {
        file.seek(SeekFrom::Start(pos)).map_err(|e| e.to_string())?;
        let mut bytes = [0u8; 16];
        file.read_exact(&mut bytes[..8])
            .map_err(|e| e.to_string())?;
        if bytes[..4] == 1u32.to_be_bytes() {
            file.read_exact(&mut bytes[8..])
                .map_err(|e| e.to_string())?;
        }
        let header = makepad_mp4_index::parse_box_header(&bytes, pos).map_err(|e| e.to_string())?;
        let size = header.size.unwrap_or(end - pos);
        if size < header.header_len || size > end - pos {
            return Err("invalid movie box size".into());
        }
        if !movie && header.kind == *b"moov" {
            movie = true;
            end = pos + size;
            pos += header.header_len;
            continue;
        }
        if movie && header.kind == *b"mvhd" {
            let mut data = [0u8; 32];
            let n = (size - header.header_len).min(32) as usize;
            file.read_exact(&mut data[..n]).map_err(|e| e.to_string())?;
            let (scale, ticks) = match (data[0], n) {
                (0, 20..) => (
                    u32::from_be_bytes(data[12..16].try_into().unwrap()),
                    u64::from(u32::from_be_bytes(data[16..20].try_into().unwrap())),
                ),
                (1, 32..) => (
                    u32::from_be_bytes(data[20..24].try_into().unwrap()),
                    u64::from_be_bytes(data[24..32].try_into().unwrap()),
                ),
                _ => return Err("invalid movie time header".into()),
            };
            if scale == 0 {
                return Err("invalid movie timescale".into());
            }
            return Ok(ticks as f64 / f64::from(scale));
        }
        pos += size;
    }
    Err("movie has no duration header".into())
}
