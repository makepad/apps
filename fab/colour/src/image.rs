//! A picture in linear light, read from any picture the app reads (PNG, JPEG, WebP, GIF, BMP:
//! [`decode_rgba`], the one decoder the app's pictures go through); see-through pixels lie on
//! white.

use crate::oklab::{srgb8_of_linear, to_linear};
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    /// Row by row from the top, linear RGB.
    pub pixels: Vec<[f32; 3]>,
}

/// The largest side a picture may have (pixels).
const MAX_SIDE: usize = 16384;

impl Image {
    /// A PNG, JPEG, WebP, GIF or BMP (an animation: its first frame), upright as its EXIF says.
    pub fn decode(bytes: &[u8]) -> Result<Image, String> {
        let (w, h, rgba) = decode_rgba(bytes)?;
        from_rgba(w, h, &rgba)
    }

    /// Its size in pixels as it shows (upright), read from the header alone.
    pub fn size(bytes: &[u8]) -> Result<[usize; 2], String> {
        let (w, h) = makepad_image_decode::image_size_by_data(bytes, Path::new("")).map_err(|e| format!("{}: {e}", NOT_READ))?;
        Ok([w, h])
    }

    /// The pixel at (x, y), held to the picture.
    pub fn at(&self, x: usize, y: usize) -> [f32; 3] {
        self.pixels[y.min(self.height - 1) * self.width + x.min(self.width - 1)]
    }

    /// The picture averaged into `nx` x `ny` cells (each the mean of the pixels it covers, in
    /// linear light), row by row from the top.
    pub fn cells(&self, nx: usize, ny: usize) -> Vec<[f32; 3]> {
        let rows: Vec<[f32; 3]> = (0..self.height).flat_map(|y| resample(&self.pixels[y * self.width..(y + 1) * self.width], nx)).collect();
        let mut out = vec![[0.0; 3]; nx * ny];
        for x in 0..nx {
            let column: Vec<[f32; 3]> = (0..self.height).map(|y| rows[y * nx + x]).collect();
            for (y, c) in resample(&column, ny).into_iter().enumerate() {
                out[y * nx + x] = c;
            }
        }
        out
    }
}

impl Image {
    /// Each pixel `k` times on a side.
    pub fn scaled(&self, k: usize) -> Image {
        let (w, h) = (self.width * k, self.height * k);
        Image { width: w, height: h, pixels: (0..w * h).map(|i| self.at(i % w / k, i / w / k)).collect() }
    }

    /// This picture and `other` side by side, `gap` pixels of `background` between, their tops
    /// level.
    pub fn beside(&self, other: &Image, gap: usize, background: [f32; 3]) -> Image {
        let (w, h) = (self.width + gap + other.width, self.height.max(other.height));
        let pixels = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                if x < self.width && y < self.height {
                    self.pixels[y * self.width + x]
                } else if x >= self.width + gap && y < other.height {
                    other.pixels[y * other.width + x - self.width - gap]
                } else {
                    background
                }
            })
            .collect();
        Image { width: w, height: h, pixels }
    }

    /// This picture over `other`, `gap` pixels of `background` between, their lefts level.
    pub fn above(&self, other: &Image, gap: usize, background: [f32; 3]) -> Image {
        let (w, h) = (self.width.max(other.width), self.height + gap + other.height);
        let pixels = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                if y < self.height && x < self.width {
                    self.pixels[y * self.width + x]
                } else if y >= self.height + gap && x < other.width {
                    other.pixels[(y - self.height - gap) * other.width + x]
                } else {
                    background
                }
            })
            .collect();
        Image { width: w, height: h, pixels }
    }

    /// The picture as a PNG (sRGB, 8 bits a channel).
    pub fn png(&self) -> Result<Vec<u8>, String> {
        use makepad_zune_png::makepad_zune_core::bit_depth::BitDepth;
        use makepad_zune_png::makepad_zune_core::colorspace::ColorSpace;
        use makepad_zune_png::makepad_zune_core::options::EncoderOptions;
        let rgb: Vec<u8> = self.pixels.iter().flat_map(|p| srgb8_of_linear(*p)).collect();
        let options = EncoderOptions::default().set_width(self.width).set_height(self.height).set_depth(BitDepth::Eight).set_colorspace(ColorSpace::RGB);
        let mut out = Vec::new();
        makepad_zune_png::PngEncoder::new(&rgb, options).encode(&mut out).map_err(|e| format!("the PNG does not write: {e:?}"))?;
        Ok(out)
    }
}

/// A line of pixels averaged into `n` cells: each cell the pixels it overlaps, by how much.
fn resample(line: &[[f32; 3]], n: usize) -> Vec<[f32; 3]> {
    let k = line.len() as f64 / n as f64;
    (0..n)
        .map(|i| {
            let (a, b) = (i as f64 * k, (i + 1) as f64 * k);
            let mut sum = [0.0f64; 3];
            let mut weight = 0.0;
            let mut j = a.floor() as usize;
            while (j as f64) < b && j < line.len() {
                let w = (b.min(j as f64 + 1.0) - a.max(j as f64)).max(0.0);
                for c in 0..3 {
                    sum[c] += line[j][c] as f64 * w;
                }
                weight += w;
                j += 1;
            }
            sum.map(|s| (s / weight.max(1e-12)) as f32)
        })
        .collect()
}

/// RGBA pixels ([`decode_rgba`]) in linear light, each on white as far as it is see-through.
fn from_rgba(w: usize, h: usize, rgba: &[u8]) -> Result<Image, String> {
    if rgba.len() < w * h * 4 {
        return Err("the picture is cut short".into());
    }
    let lut: Vec<f32> = (0..256).map(|v| to_linear(v as f32 / 255.0)).collect();
    let pixels = rgba
        .chunks_exact(4)
        .take(w * h)
        .map(|p| {
            let a = p[3] as f32 / 255.0;
            [p[0], p[1], p[2]].map(|v| lut[v as usize] * a + (1.0 - a))
        })
        .collect();
    Ok(Image { width: w, height: h, pixels })
}

/// What a picture's bytes that do not read are told.
const NOT_READ: &str = "not a picture Fab reads (a PNG, JPEG, WebP, GIF or BMP)";

/// A picture's pixels as RGBA bytes, row by row from the top: its width, height and pixels.
/// The app's one picture decoder (`makepad_image_decode`): what the chat reads, Fab's picture
/// tools read.
pub fn decode_rgba(bytes: &[u8]) -> Result<(usize, usize, Vec<u8>), String> {
    let (w, h, rgba) = makepad_image_decode::decode_rgba(bytes).map_err(|e| format!("{NOT_READ}: {e}"))?;
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
        return Err(format!("the picture is {w} x {h} pixels: give one of 1 to {MAX_SIDE} on a side"));
    }
    Ok((w, h, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Averaging into cells keeps each cell's mean: a picture of a black left half and a white
    /// right half in three cells across is black, grey, white.
    #[test]
    fn cells_average_the_pixels_they_cover() {
        let pixels: Vec<[f32; 3]> = (0..4 * 6).map(|i| if i % 6 < 3 { [0.0; 3] } else { [1.0; 3] }).collect();
        let img = Image { width: 6, height: 4, pixels };
        let c = img.cells(3, 2);
        assert_eq!(c.len(), 6);
        assert!((c[0][0] - 0.0).abs() < 1e-6 && (c[1][0] - 0.5).abs() < 1e-6 && (c[2][0] - 1.0).abs() < 1e-6, "{c:?}");
        assert_eq!(c[3], c[0]);
    }
}
