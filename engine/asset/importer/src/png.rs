//! PNG encode / decode shared by every importer path (thumbnails, sheets,
//! previews, classic conversions): 8-bit RGB/RGBA, fixed-level deflate.

// ---------------------------------------------------------------------------
// PNG encode / decode (8-bit RGBA / RGB, deflate)
// ---------------------------------------------------------------------------

/// Fixed deflate level: reruns over identical pixels must be byte-identical,
/// so the level is never a tuning knob a caller can vary.
const PNG_COMPRESSION_LEVEL: u32 = 6;

pub fn encode_png_rgba(rgba: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    encode_png(rgba, w, h, 6, 4)
}

/// Decode an 8-bit, non-interlaced RGB/RGBA PNG (everything
/// [`encode_png_rgba`] writes, plus ordinary compressed PNGs from game packs).
pub fn decode_png_stored(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    if bytes.len() < 33 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("not a png".into());
    }
    let mut off = 8usize;
    let mut w = 0u32;
    let mut h = 0u32;
    let mut color = 0u8;
    let mut depth = 0u8;
    let mut interlace = 0u8;
    let mut zlib = Vec::new();
    while off + 12 <= bytes.len() {
        let n = u32::from_be_bytes(bytes[off..off + 4].try_into().unwrap()) as usize;
        let typ = &bytes[off + 4..off + 8];
        if off + 12 + n > bytes.len() {
            break;
        }
        let data = &bytes[off + 8..off + 8 + n];
        if typ == b"IHDR" && data.len() >= 13 {
            w = u32::from_be_bytes(data[0..4].try_into().unwrap());
            h = u32::from_be_bytes(data[4..8].try_into().unwrap());
            depth = data[8];
            color = data[9];
            interlace = data[12];
        } else if typ == b"IDAT" {
            zlib.extend_from_slice(data);
        } else if typ == b"IEND" {
            break;
        }
        off += 12 + n;
    }
    if w == 0 || h == 0 || zlib.len() < 8 {
        return Err("png missing ihdr/idat".into());
    }
    if w > 16384 || h > 16384 {
        return Err("png dimensions out of range".into());
    }
    let bpp = match color {
        2 => 3,
        6 => 4,
        _ => return Err("png color type".into()),
    };
    if depth != 8 {
        return Err("png bit depth".into());
    }
    if interlace != 0 {
        return Err("png interlace".into());
    }
    let stride = w as usize * bpp;
    let mut raw = inflate_zlib(&zlib, (stride + 1) * h as usize)?;
    if raw.len() < (stride + 1) * h as usize {
        return Err("png row truncated".into());
    }
    unfilter_png(&mut raw, stride, h as usize, bpp)?;
    let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
    for y in 0..h as usize {
        let row = 1 + y * (stride + 1);
        for x in 0..w as usize {
            let p = row + x * bpp;
            if bpp == 4 {
                rgba.extend_from_slice(&raw[p..p + 4]);
            } else {
                rgba.extend_from_slice(&[raw[p], raw[p + 1], raw[p + 2], 255]);
            }
        }
    }
    Ok((rgba, w, h))
}

fn inflate_zlib(zlib: &[u8], expect: usize) -> Result<Vec<u8>, String> {
    makepad_fast_inflate::zlib_decompress_vec_with_hint(zlib, expect)
        .map_err(|e| format!("png zlib: {e:?}"))
}

/// Undo the per-row PNG filters in place (the filter byte stays, so row `y`
/// keeps starting at `1 + y * (stride + 1)`).
fn unfilter_png(raw: &mut [u8], stride: usize, rows: usize, bpp: usize) -> Result<(), String> {
    for y in 0..rows {
        let row = y * (stride + 1);
        let filter = raw[row];
        let prev = if y == 0 { None } else { Some((y - 1) * (stride + 1) + 1) };
        for x in 0..stride {
            let i = row + 1 + x;
            let a = if x >= bpp { raw[i - bpp] } else { 0 };
            let b = prev.map_or(0, |p| raw[p + x]);
            let c = match prev {
                Some(p) if x >= bpp => raw[p + x - bpp],
                _ => 0,
            };
            raw[i] = match filter {
                0 => raw[i],
                1 => raw[i].wrapping_add(a),
                2 => raw[i].wrapping_add(b),
                3 => raw[i].wrapping_add((((a as u16) + (b as u16)) / 2) as u8),
                4 => raw[i].wrapping_add(paeth(a, b, c)),
                _ => return Err("png filter type".into()),
            };
        }
    }
    Ok(())
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

pub fn encode_png_rgb(rgb: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    encode_png(rgb, w, h, 2, 3)
}

pub fn encode_png(pixels: &[u8], w: u32, h: u32, color_type: u8, bpp: usize) -> Result<Vec<u8>, String> {
    let expected = w as usize * h as usize * bpp;
    if pixels.len() != expected {
        return Err(format!(
            "png pixel length {} != {expected}",
            pixels.len()
        ));
    }
    let stride = w as usize * bpp;
    let mut raw = Vec::with_capacity((stride + 1) * h as usize);
    for y in 0..h as usize {
        raw.push(0);
        raw.extend_from_slice(&pixels[y * stride..y * stride + stride]);
    }
    let zlib = deflate_zlib(&raw);
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, color_type, 0, 0, 0]);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    push_png_chunk(&mut out, b"IHDR", &ihdr);
    push_png_chunk(&mut out, b"IDAT", &zlib);
    push_png_chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

/// Real deflate at a fixed level. Sprite sheets and 128²-tile preview strips
/// are mostly flat transparent padding — stored blocks made a 1024x256 strip
/// a 1 MB file, so every classic PNG the importer writes is compressed.
fn deflate_zlib(raw: &[u8]) -> Vec<u8> {
    makepad_fast_inflate::zlib_compress(raw, PNG_COMPRESSION_LEVEL)
}

pub fn push_png_chunk(out: &mut Vec<u8>, typ: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(typ);
    out.extend_from_slice(data);
    let mut crc_src = Vec::with_capacity(4 + data.len());
    crc_src.extend_from_slice(typ);
    crc_src.extend_from_slice(data);
    out.extend_from_slice(&png_crc(&crc_src).to_be_bytes());
}

pub fn png_crc(data: &[u8]) -> u32 {
    makepad_fast_inflate::crc32(data)
}
