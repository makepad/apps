//! A small RGBA image with line drawing and a PNG encoder (zlib-wrapped
//! deflate from the in-repo compressor).

use makepad_fast_inflate::crc32;

#[derive(Clone)]
pub struct Image {
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
    depth: Vec<f32>,
}

impl Image {
    pub fn new(w: usize, h: usize) -> Self {
        Self { w, h, rgba: vec![0; w * h * 4], depth: vec![f32::MIN; w * h] }
    }

    pub fn put(&mut self, x: i64, y: i64, d: f32, c: [u8; 4]) {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return;
        }
        let i = y as usize * self.w + x as usize;
        if d < self.depth[i] {
            return;
        }
        self.depth[i] = d;
        self.rgba[i * 4..i * 4 + 4].copy_from_slice(&c);
    }

    /// A thick line with depth (larger depth wins).
    pub fn line(&mut self, a: (f64, f64, f32), b: (f64, f64, f32), r: f64, c: [u8; 4]) {
        self.line_band(a, b, r, c, 0);
    }

    /// [`Self::line`] into the band of a taller image whose first row is `y0`: every pixel of
    /// the band gets exactly the writes, in the same order, it gets drawn whole, so bands
    /// drawn apart and stacked are the image drawn whole.
    pub fn line_band(&mut self, a: (f64, f64, f32), b: (f64, f64, f32), r: f64, c: [u8; 4], y0: i64) {
        let rr = r.max(0.5);
        let ri = rr.ceil() as i64;
        let y1 = y0 + self.h as i64;
        // Rounding keeps the order of the steps' rows: a line off the band writes none of it.
        if a.1.max(b.1).round() as i64 + ri + 1 < y0 || a.1.min(b.1).round() as i64 - ri - 1 >= y1 {
            return;
        }
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = (dx * dx + dy * dy).sqrt();
        let steps = (len * 1.5).ceil().max(1.0) as usize;
        for s in 0..=steps {
            let t = s as f64 / steps as f64;
            let x = a.0 + dx * t;
            let y = a.1 + dy * t;
            let d = a.2 + (b.2 - a.2) * t as f32;
            let (xr, yr) = (x.round() as i64, y.round() as i64);
            for oy in (-ri).max(y0 - yr)..=ri.min(y1 - 1 - yr) {
                for ox in -ri..=ri {
                    if (ox * ox + oy * oy) as f64 <= rr * rr {
                        self.put(xr + ox, yr + oy - y0, d, c);
                    }
                }
            }
        }
    }

    /// Bands, top first, stacked into one image `w` by `h`.
    pub fn stacked(w: usize, h: usize, bands: Vec<Image>) -> Image {
        let mut out = Image::new(w, h);
        let mut at = 0;
        for b in bands {
            assert_eq!(b.w, w, "a band is the image's width");
            let n = (b.h * w * 4).min(out.rgba.len() - at);
            out.rgba[at..at + n].copy_from_slice(&b.rgba[..n]);
            out.depth[at / 4..(at + n) / 4].copy_from_slice(&b.depth[..n / 4]);
            at += n;
        }
        assert_eq!(at, out.rgba.len(), "the bands cover the image");
        out
    }

    /// Box-filter down by an integer factor.
    pub fn downscale(&self, f: usize) -> Image {
        let (w, h) = (self.w / f, self.h / f);
        let mut out = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let mut acc = [0u32; 4];
                for sy in 0..f {
                    for sx in 0..f {
                        let i = ((y * f + sy) * self.w + x * f + sx) * 4;
                        for c in 0..4 {
                            acc[c] += self.rgba[i + c] as u32;
                        }
                    }
                }
                let n = (f * f) as u32;
                let o = (y * w + x) * 4;
                for c in 0..4 {
                    out.rgba[o + c] = (acc[c] / n) as u8;
                }
            }
        }
        out
    }

    pub fn encode_png(&self) -> Vec<u8> {
        let mut raw = Vec::with_capacity((self.w * 4 + 1) * self.h);
        for y in 0..self.h {
            raw.push(0); // no filter
            raw.extend_from_slice(&self.rgba[y * self.w * 4..(y + 1) * self.w * 4]);
        }
        let deflated = makepad_fast_inflate::deflate::compress_to_vec(&raw, 6);
        let mut z = vec![0x78, 0x9c];
        z.extend_from_slice(&deflated);
        z.extend_from_slice(&adler32(&raw).to_be_bytes());
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&(self.w as u32).to_be_bytes());
        ihdr.extend_from_slice(&(self.h as u32).to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        chunk(&mut out, b"IHDR", &ihdr);
        chunk(&mut out, b"IDAT", &z);
        chunk(&mut out, b"IEND", &[]);
        out
    }
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut c = Vec::with_capacity(4 + data.len());
    c.extend_from_slice(kind);
    c.extend_from_slice(data);
    out.extend_from_slice(&c);
    out.extend_from_slice(&crc32(&c).to_be_bytes());
}

fn adler32(d: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in d.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::Image;

    #[test]
    fn bands_drawn_apart_are_the_image_drawn_whole() {
        // Lines across band edges, wide and thin, deeper and shallower, over each other.
        let mut lines = Vec::new();
        let mut seed = 7u64;
        let mut next = || { seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); (seed >> 33) as f64 / (1u64 << 31) as f64 };
        for i in 0..200 {
            let a = (next() * 140.0 - 10.0, next() * 140.0 - 10.0, next() as f32);
            let b = (next() * 140.0 - 10.0, next() * 140.0 - 10.0, next() as f32);
            lines.push((a, b, 0.3 + next() * 9.0, [i as u8, (i * 7) as u8, 3, 255]));
        }
        // Every stamp of every step, clipped by `put` alone (how lines were drawn before bands).
        let mut whole = Image::new(128, 128);
        for &(a, b, r, c) in &lines {
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let steps = ((dx * dx + dy * dy).sqrt() * 1.5).ceil().max(1.0) as usize;
            let rr = r.max(0.5);
            let ri = rr.ceil() as i64;
            for s in 0..=steps {
                let t = s as f64 / steps as f64;
                let (x, y, d) = (a.0 + dx * t, a.1 + dy * t, a.2 + (b.2 - a.2) * t as f32);
                for oy in -ri..=ri {
                    for ox in -ri..=ri {
                        if (ox * ox + oy * oy) as f64 <= rr * rr { whole.put(x.round() as i64 + ox, y.round() as i64 + oy, d, c); }
                    }
                }
            }
        }
        let mut drawn = Image::new(128, 128);
        for &(a, b, r, c) in &lines { drawn.line(a, b, r, c); }
        assert!(drawn.rgba == whole.rgba && drawn.depth == whole.depth, "a line drawn whole");
        for rows in [7, 64] {
            let bands = (0..128usize.div_ceil(rows)).map(|bi| {
                let mut band = Image::new(128, rows.min(128 - bi * rows));
                for &(a, b, r, c) in &lines { band.line_band(a, b, r, c, (bi * rows) as i64); }
                band
            }).collect();
            let stacked = Image::stacked(128, 128, bands);
            assert!(stacked.rgba == whole.rgba && stacked.depth == whole.depth, "bands of {rows} rows");
        }
    }
}
