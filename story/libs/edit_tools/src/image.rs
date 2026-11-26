//! Pictures made from grabbed frames, on the CPU and deterministic: PNG
//! encoding, a labelled contact sheet (`strip`) and an onion-skin motion
//! trail (`onion`). Frames are composited over black, as a render encodes
//! a transparent frame.

use makepad_zune_png::makepad_zune_core::bit_depth::BitDepth;
use makepad_zune_png::makepad_zune_core::colorspace::ColorSpace;
use makepad_zune_png::makepad_zune_core::options::EncoderOptions;
use makepad_zune_png::PngEncoder;

/// Tight top-down RGBA.
#[derive(Clone, Debug, PartialEq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Picture {
    pub fn new(width: u32, height: u32, fill: [u8; 4]) -> Self {
        Self { width, height, rgba: fill.iter().copied().cycle().take(width as usize * height as usize * 4).collect() }
    }

    /// The same picture over black, fully opaque.
    pub fn flattened(&self) -> Picture {
        let rgba = self
            .rgba
            .chunks_exact(4)
            .flat_map(|p| {
                let a = p[3] as u32;
                [(p[0] as u32 * a / 255) as u8, (p[1] as u32 * a / 255) as u8, (p[2] as u32 * a / 255) as u8, 255]
            })
            .collect();
        Picture { width: self.width, height: self.height, rgba }
    }

    /// Area-averaged down so neither side exceeds `max_side` (never up);
    /// e.g. 1024 for a model's picture input.
    pub fn fit_within(&self, max_side: u32) -> Picture {
        let longest = self.width.max(self.height);
        if longest <= max_side || max_side == 0 {
            return self.clone();
        }
        let width = ((self.width as u64 * max_side as u64) / longest as u64).max(1) as u32;
        match makepad_story_edit_render::thumbnail::scale(&self.rgba, self.width, self.height, width) {
            Ok((rgba, width, height)) => Picture { width, height, rgba },
            Err(_) => self.clone(),
        }
    }

    fn put(&mut self, x: i64, y: i64, color: [u8; 4]) {
        if x >= 0 && y >= 0 && (x as u32) < self.width && (y as u32) < self.height {
            let i = (y as usize * self.width as usize + x as usize) * 4;
            self.rgba[i..i + 4].copy_from_slice(&color);
        }
    }

    fn fill(&mut self, x: i64, y: i64, w: i64, h: i64, color: [u8; 4]) {
        for yy in y..y + h {
            for xx in x..x + w {
                self.put(xx, yy, color);
            }
        }
    }

    /// Copy `src` (opaque) with its top-left at `(x, y)`.
    fn blit(&mut self, src: &Picture, x: i64, y: i64) {
        for sy in 0..src.height as i64 {
            let ty = y + sy;
            if ty < 0 || ty >= self.height as i64 {
                continue;
            }
            for sx in 0..src.width as i64 {
                let tx = x + sx;
                if tx < 0 || tx >= self.width as i64 {
                    continue;
                }
                let s = (sy as usize * src.width as usize + sx as usize) * 4;
                let t = (ty as usize * self.width as usize + tx as usize) * 4;
                self.rgba[t..t + 4].copy_from_slice(&src.rgba[s..s + 4]);
            }
        }
    }
}

pub fn encode_png(picture: &Picture) -> Result<Vec<u8>, String> {
    let expected = picture.width as usize * picture.height as usize * 4;
    if picture.width == 0 || picture.height == 0 || picture.rgba.len() != expected {
        return Err(format!("a {}x{} picture of {} bytes cannot be encoded", picture.width, picture.height, picture.rgba.len()));
    }
    let options = EncoderOptions::default()
        .set_width(picture.width as usize)
        .set_height(picture.height as usize)
        .set_depth(BitDepth::Eight)
        .set_colorspace(ColorSpace::RGBA);
    let mut encoder = PngEncoder::new(&picture.rgba, options);
    let mut out = Vec::new();
    encoder.encode(&mut out).map_err(|e| format!("png: {e:?}"))?;
    Ok(out)
}

/// A 5x7 pixel font for labels: digits and the few signs times need.
fn glyph(c: char) -> Option<[u8; 7]> {
    Some(match c {
        '0' => [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
        '1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        '2' => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
        '3' => [0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110],
        '4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        '5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
        '6' => [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
        '7' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
        '8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        '9' => [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
        '.' => [0, 0, 0, 0, 0, 0b01100, 0b01100],
        's' => [0, 0, 0b01110, 0b10000, 0b01110, 0b00001, 0b11110],
        '@' => [0b01110, 0b10001, 0b10111, 0b10101, 0b10111, 0b10000, 0b01110],
        '#' => [0b01010, 0b01010, 0b11111, 0b01010, 0b11111, 0b01010, 0b01010],
        ':' => [0, 0b01100, 0b01100, 0, 0b01100, 0b01100, 0],
        '-' => [0, 0, 0, 0b11111, 0, 0, 0],
        '=' => [0, 0, 0b11111, 0, 0b11111, 0, 0],
        't' => [0b01000, 0b01000, 0b11100, 0b01000, 0b01000, 0b01001, 0b00110],
        'f' => [0b00110, 0b01001, 0b01000, 0b11100, 0b01000, 0b01000, 0b01000],
        ' ' => [0; 7],
        'A' => [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'B' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110],
        'C' => [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110],
        'D' => [0b11100, 0b10010, 0b10001, 0b10001, 0b10001, 0b10010, 0b11100],
        'E' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111],
        'F' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000],
        'G' => [0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111],
        'H' => [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'I' => [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        'J' => [0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100],
        'K' => [0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001],
        'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111],
        'M' => [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001],
        'N' => [0b10001, 0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001],
        'O' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'P' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000],
        'Q' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101],
        'R' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001],
        'S' => [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110],
        'T' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
        'U' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'V' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
        'W' => [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b10101, 0b01010],
        'X' => [0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001],
        'Y' => [0b10001, 0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100],
        'Z' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111],
        '_' => [0, 0, 0, 0, 0, 0, 0b11111],
        '[' => [0b01110, 0b01000, 0b01000, 0b01000, 0b01000, 0b01000, 0b01110],
        ']' => [0b01110, 0b00010, 0b00010, 0b00010, 0b00010, 0b00010, 0b01110],
        '(' => [0b00010, 0b00100, 0b01000, 0b01000, 0b01000, 0b00100, 0b00010],
        ')' => [0b01000, 0b00100, 0b00010, 0b00010, 0b00010, 0b00100, 0b01000],
        '/' => [0b00001, 0b00010, 0b00010, 0b00100, 0b01000, 0b01000, 0b10000],
        _ => return None,
    })
}

/// Draw `text` at `(x, y)` (its top-left), each font pixel `scale` wide.
/// Characters the font lacks are left out.
pub fn draw_label(picture: &mut Picture, x: i64, y: i64, text: &str, scale: i64, color: [u8; 4]) {
    let mut pen = x;
    for c in text.chars() {
        let Some(rows) = glyph(c) else { continue };
        for (row, bits) in rows.iter().enumerate() {
            for col in 0..5 {
                if bits & (0b10000 >> col) != 0 {
                    picture.fill(pen + col * scale, y + row as i64 * scale, scale, scale, color);
                }
            }
        }
        pen += 6 * scale;
    }
}

/// The pixel width of `text` in the label font.
/// (Names are drawn in capitals: [`draw_names`].)
pub fn label_width(text: &str, scale: i64) -> i64 {
    text.chars().filter(|c| glyph(*c).is_some()).count() as i64 * 6 * scale - scale
}

const SHEET_BG: [u8; 4] = [27, 27, 31, 255];
const LABEL: [u8; 4] = [235, 235, 240, 255];
const GAP: i64 = 8;
const LABEL_SCALE: i64 = 2;

/// Columns for `count` frames of `w` x `h` so the sheet comes out roughly
/// square.
pub fn sheet_columns(count: usize, w: u32, h: u32) -> usize {
    let aspect = h.max(1) as f64 / w.max(1) as f64;
    ((count as f64 * aspect).sqrt().round() as usize).clamp(1, count.max(1))
}

/// Frames in a grid, left to right, each with its label under it.
pub fn contact_sheet(frames: &[(Picture, String)], columns: usize) -> Picture {
    let columns = columns.clamp(1, frames.len().max(1));
    let rows = frames.len().div_ceil(columns).max(1);
    let (cw, ch) = frames.iter().fold((1i64, 1i64), |(w, h), (p, _)| (w.max(p.width as i64), h.max(p.height as i64)));
    let label_h = 7 * LABEL_SCALE + 6;
    let (cell_w, cell_h) = (cw + GAP, ch + label_h + GAP);
    let mut sheet = Picture::new((columns as i64 * cell_w + GAP) as u32, (rows as i64 * cell_h + GAP) as u32, SHEET_BG);
    for (i, (picture, label)) in frames.iter().enumerate() {
        let (x, y) = (GAP + (i % columns) as i64 * cell_w, GAP + (i / columns) as i64 * cell_h);
        sheet.blit(&picture.flattened(), x, y);
        let lx = x + (picture.width as i64 - label_width(label, LABEL_SCALE)).max(0) / 2;
        draw_label(&mut sheet, lx, y + picture.height as i64 + 4, label, LABEL_SCALE, LABEL);
    }
    sheet
}

/// The frames blended into one: a weighted mean over black where the
/// latest frame weighs most and the earliest a fifth of that, so moving
/// parts leave a fading trail and still parts stay sharp.
pub fn onion(frames: &[Picture]) -> Result<Picture, String> {
    let first = frames.first().ok_or("no frames to blend")?;
    if frames.iter().any(|p| p.width != first.width || p.height != first.height) {
        return Err("the frames differ in size".into());
    }
    let n = frames.len();
    let weights: Vec<f64> = (0..n).map(|i| if n == 1 { 1.0 } else { 0.2 + 0.8 * i as f64 / (n - 1) as f64 }).collect();
    let total: f64 = weights.iter().sum();
    let flat: Vec<Picture> = frames.iter().map(Picture::flattened).collect();
    let mut rgba = vec![0u8; first.rgba.len()];
    for (i, out) in rgba.iter_mut().enumerate() {
        if i % 4 == 3 {
            *out = 255;
            continue;
        }
        let sum: f64 = flat.iter().zip(&weights).map(|(p, w)| p.rgba[i] as f64 * w).sum();
        *out = (sum / total).round().clamp(0.0, 255.0) as u8;
    }
    Ok(Picture { width: first.width, height: first.height, rgba })
}

impl Picture {
    /// The part `[x, y, w, h]` (pixels, clipped to the picture).
    pub fn cropped(&self, x: i64, y: i64, w: i64, h: i64) -> Picture {
        let (x0, y0) = (x.clamp(0, self.width as i64), y.clamp(0, self.height as i64));
        let (x1, y1) = ((x + w).clamp(x0, self.width as i64), (y + h).clamp(y0, self.height as i64));
        let (cw, ch) = ((x1 - x0).max(1) as u32, (y1 - y0).max(1) as u32);
        let mut out = Picture::new(cw, ch, [0, 0, 0, 255]);
        for row in 0..(y1 - y0) as usize {
            let from = ((y0 as usize + row) * self.width as usize + x0 as usize) * 4;
            let to = row * cw as usize * 4;
            let len = (x1 - x0) as usize * 4;
            out.rgba[to..to + len].copy_from_slice(&self.rgba[from..from + len]);
        }
        out
    }

    /// A one-pixel-wide rectangle outline, `width` pixels thick.
    fn outline(&mut self, b: [i64; 4], width: i64, color: [u8; 4]) {
        let (w, h) = (b[2] - b[0], b[3] - b[1]);
        self.fill(b[0], b[1], w, width, color);
        self.fill(b[0], b[3] - width, w, width, color);
        self.fill(b[0], b[1], width, h, color);
        self.fill(b[2] - width, b[1], width, h, color);
    }
}

/// Outline colours, one per layer in turn.
const NAME_COLORS: [[u8; 4]; 6] = [[255, 196, 0, 255], [0, 220, 255, 255], [255, 64, 160, 255], [120, 255, 80, 255], [255, 120, 40, 255], [180, 140, 255, 255]];

/// Each layer's box outlined and its name written on a dark tag (in
/// capitals) above its top-left corner, or, for a group (`true`: its box
/// often matches a child's), inside its bottom-left corner. Boxes are
/// `[x0, y0, x1, y1]` in picture pixels; a tag that would leave the
/// picture moves inside it.
pub fn draw_names(picture: &mut Picture, boxes: &[(String, [f64; 4], bool)], scale: i64) {
    for (i, (name, b, group)) in boxes.iter().enumerate() {
        let color = NAME_COLORS[i % NAME_COLORS.len()];
        let b = [b[0].floor() as i64, b[1].floor() as i64, b[2].ceil() as i64, b[3].ceil() as i64];
        picture.outline(b, scale.max(1) / 2 + 1, color);
        let text = name.to_uppercase();
        let (tw, th) = (label_width(&text, scale) + 2 * scale, 9 * scale);
        let x = b[0].clamp(0, (picture.width as i64 - tw).max(0));
        let y = match group {
            true => b[3] - th,
            false if b[1] >= th => b[1] - th,
            false => b[1],
        }
        .clamp(0, (picture.height as i64 - th).max(0));
        picture.fill(x, y, tw, th, [12, 12, 16, 255]);
        draw_label(picture, x + scale, y + scale, &text, scale, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_round_trips_through_the_decoder() {
        let mut picture = Picture::new(3, 2, [10, 20, 30, 255]);
        picture.put(2, 1, [200, 100, 50, 255]);
        let png = encode_png(&picture).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        let mut decoder = makepad_zune_png::PngDecoder::new(makepad_zune_png::makepad_zune_core::bytestream::ZCursor::new(&png[..]));
        let pixels = decoder.decode_raw().unwrap();
        assert_eq!(pixels, picture.rgba);
        assert!(encode_png(&Picture { width: 2, height: 2, rgba: vec![0; 3] }).is_err());
    }

    #[test]
    fn a_contact_sheet_lays_frames_out_with_labels() {
        let frames: Vec<(Picture, String)> = (0..5).map(|i| (Picture::new(40, 20, [i * 50, 0, 0, 255]), format!("@{i}"))).collect();
        assert_eq!(sheet_columns(5, 40, 20), 2);
        let sheet = contact_sheet(&frames, 3);
        // 3 columns, 2 rows; cells 40+8 wide, 20+20+8 tall.
        assert_eq!((sheet.width, sheet.height), (3 * 48 + 8, 2 * 48 + 8));
        let at = |p: &Picture, x: u32, y: u32| p.rgba[((y * p.width + x) * 4) as usize..][..4].to_vec();
        assert_eq!(at(&sheet, 8, 8), vec![0, 0, 0, 255]);
        assert_eq!(at(&sheet, 8 + 48, 8), vec![50, 0, 0, 255]);
        assert_eq!(at(&sheet, 8, 8 + 48), vec![150, 0, 0, 255]);
        assert_eq!(at(&sheet, 2, 2), SHEET_BG.to_vec());
        // Some label pixels under the first frame.
        let label_rows = (8 + 20 + 4)..(8 + 20 + 4 + 14);
        assert!(label_rows.clone().any(|y| (8..48).any(|x| at(&sheet, x, y) == LABEL.to_vec())));
    }

    #[test]
    fn onion_weighs_the_latest_frame_most() {
        let a = Picture::new(2, 1, [0, 0, 0, 255]);
        let b = Picture::new(2, 1, [250, 250, 250, 255]);
        let blended = onion(&[a.clone(), b.clone()]).unwrap();
        // weights 0.2 and 1.0: 250 / 1.2.
        assert_eq!(blended.rgba[0], 208);
        assert_eq!(blended.rgba[3], 255);
        assert_eq!(onion(&[b.clone()]).unwrap().rgba, b.rgba);
        // Transparent pixels count as black.
        let clear = Picture::new(2, 1, [250, 250, 250, 0]);
        assert_eq!(onion(&[clear]).unwrap().rgba[0], 0);
        assert!(onion(&[a, Picture::new(1, 1, [0, 0, 0, 255])]).is_err());
    }

    #[test]
    fn crops_keep_their_pixels_and_names_are_drawn_on_their_boxes() {
        let mut p = Picture::new(40, 30, [0, 0, 0, 255]);
        p.fill(10, 10, 5, 5, [255, 0, 0, 255]);
        let c = p.cropped(8, 8, 10, 10);
        assert_eq!((c.width, c.height), (10, 10));
        assert_eq!(&c.rgba[(2 * 10 + 2) * 4..(2 * 10 + 2) * 4 + 4], &[255, 0, 0, 255]);
        // Clipped to the picture.
        assert_eq!((p.cropped(35, 25, 20, 20).width, p.cropped(35, 25, 20, 20).height), (5, 5));
        let mut named = Picture::new(200, 100, [0, 0, 0, 255]);
        draw_names(&mut named, &[("logo".into(), [50.0, 40.0, 150.0, 90.0], false)], 1);
        // The outline's corner, and the tag above the box carries ink.
        assert_eq!(&named.rgba[(40 * 200 + 50) * 4..(40 * 200 + 50) * 4 + 4], &NAME_COLORS[0]);
        let tag = (31..40).any(|y| (50..80).any(|x| named.rgba[(y * 200 + x) * 4..(y * 200 + x) * 4 + 4] == NAME_COLORS[0]));
        assert!(tag);
        assert!(label_width("LOGO", 1) > 0 && label_width("logo", 1) < label_width("LOGO", 1), "lowercase is drawn as capitals by draw_names");
    }

    #[test]
    fn pictures_fit_within_a_side() {
        let big = Picture::new(2000, 500, [9, 9, 9, 255]);
        let small = big.fit_within(1024);
        assert_eq!((small.width, small.height), (1024, 256));
        assert_eq!(&small.rgba[..4], &[9, 9, 9, 255]);
        assert_eq!(Picture::new(10, 10, [0; 4]).fit_within(1024).width, 10);
    }
}
