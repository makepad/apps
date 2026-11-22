//! The plate's layers written out for a resin printer: a folder holding one greyscale PNG per
//! layer, the image the printer's screen shows, and a manifest of the job.
//!
//! - An image has the screen's grey levels and its mounting: the bed seen from above with the
//!   back at the top, mirrored and turned as the printer's profile says ([`oriented`]).
//! - Writing an image costs what its lit rows hold, not the whole screen ([`png`]): the rows
//!   around the band of lit ones are empty, and their compressed form is made once per export
//!   ([`Blank`]).
//! - The manifest gives the images' size and pitch, the layers, the exposures and peels, the
//!   print time and the resin ([`manifest`]).
//!
//! This is a generic stack. A printer's own container would be another writer over the same
//! images.

use crate::estimate::{print_time, resin_ml};
use crate::grid::PixelGrid;
use crate::image::LayerStack;
use crate::printer::{Lift, ResinProfile};
use crate::spans::Spans;
use fab_plate::machine::ResinScreen;
use makepad_fab_slicer::par;
use makepad_fast_inflate::deflate::compress_part_to_vec;
use makepad_fast_inflate::{adler32, crc32};
use makepad_strict_json::{obj, Value};
use std::ops::Range;
use std::path::Path;

/// The manifest's file in an exported stack.
pub const MANIFEST: &str = "manifest.json";
/// The manifest's `format`. A folder whose manifest has it is an export that a new one may replace.
pub const FORMAT: &str = "fab-sla-layers";

/// Layer `k`'s image file (0 is on the plate).
pub fn layer_file(k: u32) -> String {
    format!("layer_{k:05}.png")
}

/// `bed` (row-major, `pixels` wide and tall, row 0 at the bed's front) as the screen shows it.
/// It is seen from above with the back at the top, mirrored along X and Y as `mirror` says,
/// then turned `quarter_turns` counter-clockwise. Returns the image's size and its pixels.
pub fn oriented(bed: &[u8], pixels: [u32; 2], mirror: [bool; 2], quarter_turns: u8) -> ([u32; 2], Vec<u8>) {
    let (w, h) = (pixels[0] as usize, pixels[1] as usize);
    assert_eq!(bed.len(), w * h, "the bed image is its pixels");
    // from above with the back at the top: row r is the bed's row h - 1 - r, or row r once
    // mirrored along Y; mirrored along X, each row runs backward
    let mut top = Vec::with_capacity(w * h);
    for r in 0..h {
        let row = &bed[if mirror[1] { r } else { h - 1 - r } * w..][..w];
        if mirror[0] {
            top.extend(row.iter().rev());
        } else {
            top.extend_from_slice(row);
        }
    }
    match quarter_turns % 4 {
        0 => ([w as u32, h as u32], top),
        2 => {
            top.reverse();
            ([w as u32, h as u32], top)
        }
        turn => {
            // a quarter turn swaps the sides. Turned counter-clockwise, row r is the column
            // w - 1 - r read down; turned clockwise, it is the column r read up.
            let mut out = vec![0u8; w * h];
            for (r, line) in out.chunks_exact_mut(h).enumerate() {
                for (c, d) in line.iter_mut().enumerate() {
                    *d = if turn == 1 { top[c * w + (w - 1 - r)] } else { top[(h - 1 - c) * w + r] };
                }
            }
            ([h as u32, w as u32], out)
        }
    }
}

/// The layer `spans` on `screen` as the printer shows it: its size, and each pixel's grey level.
pub fn screen_image(spans: &Spans, screen: &ResinScreen) -> ([u32; 2], Vec<u8>) {
    let bed = spans.image(PixelGrid::of(screen).all(), screen.grey_levels);
    oriented(&bed, screen.pixels, screen.mirror, screen.quarter_turns)
}

/// Where bed pixel `(i, j)` shows in `screen`'s image ([`oriented`]): its column and row.
fn on_screen(screen: &ResinScreen, i: i64, j: i64) -> (i64, i64) {
    let (w, h) = (screen.pixels[0] as i64, screen.pixels[1] as i64);
    let (mut c, mut r) = (i, h - 1 - j);
    if screen.mirror[0] {
        c = w - 1 - c;
    }
    if screen.mirror[1] {
        r = h - 1 - r;
    }
    match screen.quarter_turns % 4 {
        0 => (c, r),
        1 => (r, w - 1 - c),
        2 => (w - 1 - c, h - 1 - r),
        _ => (h - 1 - r, c),
    }
}

/// A screen image that is black but for the band of rows `rows`.
#[derive(Clone, Debug, PartialEq)]
pub struct Band {
    /// The whole image's width and height.
    pub size: [u32; 2],
    pub rows: Range<u32>,
    /// The band's rows, row-major, the image's width each.
    pub pixels: Vec<u8>,
    /// Whether each of the band's rows has a pixel that isn't black.
    pub lit: Vec<bool>,
}

/// The layer `spans` on `screen`, as [`screen_image`] makes it, but only the band of rows its
/// lit pixels fall in. It costs what the layer covers, not the screen.
pub fn screen_band(spans: &Spans, screen: &ResinScreen) -> Band {
    let size = if screen.quarter_turns % 2 == 1 { [screen.pixels[1], screen.pixels[0]] } else { screen.pixels };
    let rect = spans.rect.intersect(&PixelGrid::of(screen).all());
    if rect.is_empty() {
        return Band { size, rows: 0..0, pixels: Vec::new(), lit: Vec::new() };
    }
    let bed = spans.image(rect, screen.grey_levels);
    // the mounting is affine: one step along the bed's X or Y is one fixed step in the image
    let (i0, j0) = (rect.x0 as i64, rect.y0 as i64);
    let at = on_screen(screen, i0, j0);
    let (along_i, along_j) = (on_screen(screen, i0 + 1, j0), on_screen(screen, i0, j0 + 1));
    let far = on_screen(screen, rect.x1 as i64 - 1, rect.y1 as i64 - 1);
    let (r0, r1) = (at.1.min(far.1), at.1.max(far.1) + 1);
    let w = size[0] as i64;
    let step_i = (along_i.1 - at.1) * w + (along_i.0 - at.0);
    let step_j = (along_j.1 - at.1) * w + (along_j.0 - at.0);
    // the image rows a step along the bed's X and Y moves
    let (row_i, row_j) = (along_i.1 - at.1, along_j.1 - at.1);
    let mut pixels = vec![0u8; ((r1 - r0) * w) as usize];
    let mut lit = vec![false; (r1 - r0) as usize];
    // turned, each bed column is an image row: whether a column is lit, over all bed rows
    let mut across = vec![0u8; if row_i == 0 { 0 } else { rect.width() as usize }];
    let first = (at.1 - r0) * w + at.0;
    for (y, row) in bed.chunks_exact(rect.width() as usize).enumerate() {
        let start = first + y as i64 * step_j;
        if step_i == 1 {
            // a bed row is an image row: copied whole
            pixels[start as usize..][..row.len()].copy_from_slice(row);
        } else {
            for (x, v) in row.iter().enumerate() {
                pixels[(start + x as i64 * step_i) as usize] = *v;
            }
        }
        if row_i == 0 {
            lit[(at.1 - r0 + y as i64 * row_j) as usize] |= row.iter().any(|v| *v != 0);
        } else {
            for (a, v) in across.iter_mut().zip(row) {
                *a |= *v;
            }
        }
    }
    for (x, a) in across.iter().enumerate() {
        if *a != 0 {
            lit[(at.1 - r0 + x as i64 * row_i) as usize] = true;
        }
    }
    Band { size, rows: r0 as u32..r1 as u32, pixels, lit }
}

/// The deflate level an image's band is compressed at.
const LEVEL: u8 = 6;
/// Empty rows in a run [`Blank`] compresses at once.
const MANY: usize = 64;

/// The compressed form of empty image rows `width` pixels wide, made once for a stack.
///
/// The parts are self-contained deflate parts, ending on a full flush, so a run of empty rows
/// in any image of that width is the same bytes ([`compress_part_to_vec`]).
pub struct Blank {
    /// A row's bytes: its filter byte and its pixels.
    line: usize,
    one: Vec<u8>,
    many: Vec<u8>,
    /// The stream's end: an empty last part.
    end: Vec<u8>,
}

impl Blank {
    pub fn new(width: u32) -> Blank {
        let line = width as usize + 1;
        let part = |rows: usize| compress_part_to_vec(&vec![0u8; rows * line], LEVEL, false);
        Blank { line, one: part(1), many: part(MANY), end: compress_part_to_vec(&[], LEVEL, true) }
    }

    /// `rows` empty rows onto the stream `z`, and onto its checksum.
    fn rows(&self, rows: usize, z: &mut Vec<u8>, adler: &mut u32) {
        for _ in 0..rows / MANY {
            z.extend_from_slice(&self.many);
        }
        for _ in 0..rows % MANY {
            z.extend_from_slice(&self.one);
        }
        // zeros leave the checksum's first sum and add that sum to the second, once a byte
        let (s1, s2) = (*adler & 0xffff, *adler >> 16);
        let s2 = (s2 as u64 + (rows as u64 * self.line as u64 % 65521) * s1 as u64) % 65521;
        *adler = (s2 as u32) << 16 | s1;
    }
}

/// An 8-bit greyscale PNG of `band` (its rows unfiltered). The empty rows above and below it, and
/// its own runs of empty rows, are `blank`'s parts; only its lit rows are compressed.
pub fn png(band: &Band, blank: &Blank) -> Vec<u8> {
    let [w, h] = band.size;
    assert_eq!(blank.line, w as usize + 1, "the blank rows are the image's width");
    assert_eq!(band.pixels.len(), band.rows.len() * w as usize, "the band is its rows");
    let mut z = vec![0x78, 0x01];
    let mut adler = 1u32;
    blank.rows(band.rows.start as usize, &mut z, &mut adler);
    // the band's lit rows compressed, and its runs of MANY or more empty rows (between parts
    // apart on the plate) blank like the rows around it
    let rows: Vec<&[u8]> = band.pixels.chunks_exact(w as usize).collect();
    assert_eq!(band.lit.len(), rows.len(), "a lit flag per row of the band");
    let empty: Vec<bool> = band.lit.iter().map(|l| !l).collect();
    let mut y = 0;
    while y < rows.len() {
        let mut lit_end = y;
        while lit_end < rows.len() {
            let run = empty[lit_end..].iter().take_while(|e| **e).count();
            if run >= MANY || lit_end + run == rows.len() {
                break;
            }
            lit_end += run.max(1);
        }
        if lit_end > y {
            let mut raw = Vec::with_capacity((lit_end - y) * blank.line);
            for row in &rows[y..lit_end] {
                raw.push(0);
                raw.extend_from_slice(row);
            }
            z.extend(compress_part_to_vec(&raw, LEVEL, false));
            adler = adler32(adler, &raw);
        }
        let gap = empty[lit_end..].iter().take_while(|e| **e).count();
        blank.rows(gap, &mut z, &mut adler);
        y = lit_end + gap;
    }
    blank.rows((h - band.rows.end) as usize, &mut z, &mut adler);
    z.extend_from_slice(&blank.end);
    z.extend_from_slice(&adler.to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&w.to_be_bytes());
    header.extend_from_slice(&h.to_be_bytes());
    // 8 bits of grey, deflate, rows filtered by type, no interlace
    header.extend_from_slice(&[8, 0, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// A PNG chunk: its length, kind, data and checksum.
pub(crate) fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// The manifest of `stack` printed as `job`, with `resin_ml` of resin in all its layers.
pub fn manifest(stack: &LayerStack, job: &ResinProfile, resin_ml: f64) -> String {
    let s = &job.screen;
    // a quarter turn swaps the images' sides
    let sides = |v: [f64; 2]| if s.quarter_turns % 2 == 1 { [v[1], v[0]] } else { v };
    let size = sides([s.pixels[0] as f64, s.pixels[1] as f64]).map(|v| Value::Int(v as i64));
    let pitch = sides(s.pitch).map(Value::F64);
    let mirror = match s.mirror {
        [false, false] => "none",
        [true, false] => "x",
        [false, true] => "y",
        [true, true] => "xy",
    };
    let lift = |l: Lift| obj(vec![("height_mm", Value::F64(l.height)), ("speed_mm_s", Value::F64(l.speed))]);
    let count = stack.layers.count;
    obj(vec![
        ("format", Value::Str(FORMAT.into())),
        ("version", Value::Int(1)),
        ("printer", Value::Str(job.name.clone())),
        // the images: their pixels and pitch along their own width and height
        ("image_size_px", Value::Arr(size.into())),
        ("pixel_pitch_mm", Value::Arr(pitch.into())),
        ("mirror", Value::Str(mirror.into())),
        ("rotation_deg", Value::Int(90 * (s.quarter_turns % 4) as i64)),
        ("grey_levels", Value::Int(s.grey_levels as i64)),
        ("layer_height_mm", Value::F64(stack.layers.height)),
        ("layer_count", Value::Int(count as i64)),
        ("bottom_layers", Value::Int(job.bottom_layers as i64)),
        ("bottom_exposure_s", Value::F64(job.bottom_exposure)),
        ("exposure_s", Value::F64(job.exposure)),
        ("light_off_delay_s", Value::F64(job.light_off_delay)),
        ("bottom_lift", lift(job.bottom_lift)),
        ("lift", lift(job.lift)),
        ("retract_speed_mm_s", Value::F64(job.retract_speed)),
        ("print_time_s", Value::F64(print_time(job, count))),
        ("resin_ml", Value::F64(resin_ml)),
        ("layers", Value::Arr((0..count).map(|k| Value::Str(layer_file(k))).collect())),
    ])
    .to_json()
}

/// What an export wrote.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Written {
    pub layers: u32,
    /// The files' bytes, the manifest's too.
    pub bytes: u64,
    pub resin_ml: f64,
    pub print_seconds: f64,
}

/// Whether `dir` holds an export: a manifest of [`FORMAT`].
pub fn is_export(dir: &Path) -> bool {
    let Ok(text) = std::fs::read(dir.join(MANIFEST)) else { return false };
    makepad_strict_json::parse(&text).ok().and_then(|v| v.get("format").and_then(Value::as_str).map(|f| f == FORMAT)).unwrap_or(false)
}

/// Writes `stack`, printed as `job`, into the folder `dir`: an image per layer and the manifest.
/// An export already at `dir` is replaced whole, so none of its layers stay behind. Anything
/// else at `dir` stops the export. The new stack is written beside `dir` and moved there once
/// it is whole.
pub fn write(dir: &Path, stack: &LayerStack, job: &ResinProfile) -> Result<Written, String> {
    if stack.grid.pixels != job.screen.pixels {
        return Err(format!("the layers are on a {:?} pixel screen, the printer's is {:?}", stack.grid.pixels, job.screen.pixels));
    }
    if dir.exists() && !is_export(dir) {
        return Err(format!("{} is in the way: it is not a layer export", dir.display()));
    }
    let name = dir.file_name().ok_or_else(|| format!("{} names no folder", dir.display()))?.to_string_lossy().to_string();
    let part = dir.with_file_name(format!(".{name}.writing-{}", std::process::id()));
    let written = write_into(&part, stack, job).and_then(|w| {
        if dir.exists() {
            std::fs::remove_dir_all(dir).map_err(|e| format!("the last export at {} stays: {e}", dir.display()))?;
        }
        std::fs::rename(&part, dir).map_err(|e| format!("moving the layers to {}: {e}", dir.display()))?;
        Ok(w)
    });
    if written.is_err() {
        let _ = std::fs::remove_dir_all(&part);
    }
    written
}

/// The images, then the manifest, into the new folder `dir`.
fn write_into(dir: &Path, stack: &LayerStack, job: &ResinProfile) -> Result<Written, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("making {}: {e}", dir.display()))?;
    let count = stack.layers.count;
    let s = &job.screen;
    let blank = Blank::new(if s.quarter_turns % 2 == 1 { s.pixels[1] } else { s.pixels[0] });
    let layers: Vec<Result<(u64, f64), String>> = par::map(count as usize, |k| {
        let layer = stack.layer(k as u32);
        let bytes = png(&screen_band(&layer.spans, s), &blank);
        let path = dir.join(layer_file(k as u32));
        std::fs::write(&path, &bytes).map_err(|e| format!("writing {}: {e}", path.display()))?;
        Ok((bytes.len() as u64, layer.spans.covered()))
    });
    let (mut bytes, mut covered) = (0u64, 0.0);
    for l in layers {
        let (b, c) = l?;
        bytes += b;
        covered += c;
    }
    let ml = resin_ml(covered, &stack.grid, stack.layers.height);
    let text = manifest(stack, job, ml);
    std::fs::write(dir.join(MANIFEST), &text).map_err(|e| format!("writing the manifest: {e}"))?;
    Ok(Written { layers: count, bytes: bytes + text.len() as u64, resin_ml: ml, print_seconds: print_time(job, count) })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::printer::simulated;
    use crate::section::Section;
    use crate::tests::field;
    use makepad_csg_sdf::SdfNode;
    use crate::grid::PixelRect;
    use makepad_zune_png::makepad_zune_core::bit_depth::BitDepth;
    use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
    use makepad_zune_png::makepad_zune_core::colorspace::ColorSpace;
    use makepad_zune_png::makepad_zune_core::options::EncoderOptions;
    use makepad_zune_png::{PngDecoder, PngEncoder};
    use std::sync::Arc;

    /// A 3 x 2 glyph, its front row 1 2 3 and its back row 4 5 6, on every mounting: seen from
    /// above the back is on top; a mirror flips its side; a counter-clockwise turn puts the right
    /// column on top, a clockwise one the left column read up.
    #[test]
    fn the_screen_shows_the_bed_mirrored_and_turned_as_the_printer_mounts_it() {
        let bed = [1, 2, 3, 4, 5, 6];
        let on = |mirror, turns| oriented(&bed, [3, 2], mirror, turns);
        assert_eq!(on([false, false], 0), ([3, 2], vec![4, 5, 6, 1, 2, 3]));
        assert_eq!(on([true, false], 0), ([3, 2], vec![6, 5, 4, 3, 2, 1]));
        assert_eq!(on([false, true], 0), ([3, 2], vec![1, 2, 3, 4, 5, 6]));
        assert_eq!(on([true, true], 0), ([3, 2], vec![3, 2, 1, 6, 5, 4]));
        assert_eq!(on([false, false], 1), ([2, 3], vec![6, 3, 5, 2, 4, 1]));
        assert_eq!(on([false, false], 2), on([true, true], 0), "half a turn is both mirrors");
        assert_eq!(on([false, false], 3), ([2, 3], vec![1, 4, 2, 5, 3, 6]));
        assert_eq!(on([true, false], 1), ([2, 3], vec![4, 1, 5, 2, 6, 3]));
    }

    /// A small screen (35 x 25 mm), turned a quarter and mirrored along Y, with 0.5 mm layers.
    pub(crate) fn small() -> ResinProfile {
        let mut p = simulated();
        p.screen.pixels = [700, 500];
        p.screen.quarter_turns = 1;
        p.layer_height = 0.5;
        p
    }

    /// A block over the bed's X 6..14 mm and Y 4..8 mm, `tall` mm tall.
    pub(crate) fn block(tall: f64) -> Arc<dyn Section> {
        Arc::new(field(SdfNode::Box { size: [8.0, tall, 4.0], radius: 0.0 }, [10.0, 6.0, tall / 2.0], ([5.9, 3.9, -0.1], [14.1, 8.1, tall])))
    }

    fn decoded(path: &Path) -> ([u32; 2], Vec<u8>) {
        decode(&std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
    }

    fn decode(bytes: &[u8]) -> ([u32; 2], Vec<u8>) {
        let mut d = PngDecoder::new(ZCursor::new(bytes));
        let pixels = d.decode_raw().unwrap_or_else(|e| panic!("a PNG that decodes: {e:?}"));
        let (w, h) = d.dimensions().expect("decoded dimensions");
        ([w as u32, h as u32], pixels)
    }

    /// The image through the in-repo PNG encoder, every row filtered: the encoder the export
    /// used before its band encoder.
    fn reference_png(size: [u32; 2], grey: &[u8]) -> Vec<u8> {
        let options = EncoderOptions::default().set_colorspace(ColorSpace::Luma).set_width(size[0] as usize).set_height(size[1] as usize).set_depth(BitDepth::Eight);
        let mut out = Vec::new();
        PngEncoder::new(grey, options).encode(&mut out).expect("the reference encodes");
        out
    }

    /// Every layer of a block and a ball (grey edges all round), and an empty layer, on every
    /// mirror and turn, at 16 and 256 grey levels. The band PNG decodes to the screen's image
    /// pixel for pixel, as the reference encoder's PNG does: only the bytes differ.
    #[test]
    fn every_layer_png_decodes_to_the_reference_encoders_pixels() {
        let mut p = small();
        let ball: Arc<dyn Section> = Arc::new(field(SdfNode::Sphere { radius: 3.0 }, [26.0, 17.0, 3.0], ([22.9, 13.9, 0.0], [29.1, 20.1, 6.0])));
        let stack = LayerStack::new(vec![block(4.0), ball], &p);
        let mut layers: Vec<Spans> = (0..stack.layers.count).map(|k| stack.layer(k).spans).collect();
        layers.push(Spans::of(PixelRect::EMPTY, &[]));
        let mut checked = 0;
        for mirror in [[false, false], [true, false], [false, true], [true, true]] {
            for turns in 0..4 {
                for levels in [16, 256] {
                    p.screen.mirror = mirror;
                    p.screen.quarter_turns = turns;
                    p.screen.grey_levels = levels;
                    let blank = Blank::new(if turns % 2 == 1 { p.screen.pixels[1] } else { p.screen.pixels[0] });
                    for (k, spans) in layers.iter().enumerate() {
                        let (size, image) = screen_image(spans, &p.screen);
                        let band = screen_band(spans, &p.screen);
                        let on = format!("layer {k}, mirror {mirror:?}, {turns} turns, {levels} levels");
                        assert!(decode(&png(&band, &blank)) == (size, image.clone()), "{on}: the band PNG");
                        assert!(decode(&reference_png(size, &image)) == (size, image), "{on}: the reference PNG");
                        checked += 1;
                    }
                }
            }
        }
        assert_eq!((layers.len(), checked), (13, 32 * 13), "the ball's 12 layers and an empty one, on 32 mountings");
    }

    /// The images read back as the layers on the screen. The block lands where the printer's
    /// mirror and turn put it, and the manifest gives the job. A new export replaces the last
    /// whole, and a folder that isn't an export stays as it is.
    #[test]
    fn an_exported_stack_reads_back_as_its_layers_on_the_screen() {
        let base = std::env::temp_dir().join(format!("fab-sla-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("block.layers");
        let p = small();
        let stack = LayerStack::new(vec![block(4.0)], &p);
        let w = write(&dir, &stack, &p).expect("the export");
        assert_eq!(w.layers, 8, "4 mm in 0.5 mm layers");
        assert!((w.resin_ml - 0.128).abs() < 0.002, "8 x 4 x 4 mm: {} ml", w.resin_ml);
        for k in 0..8 {
            let (size, pixels) = decoded(&dir.join(layer_file(k)));
            assert_eq!(size, [500, 700], "a quarter turn stands the 700 x 500 screen up");
            assert_eq!((size, pixels.clone()), screen_image(&stack.layer(k).spans, &p.screen), "layer {k}");
            // the bed's columns 120..280 and rows 80..160, mirrored along Y and turned
            // counter-clockwise: rows 699 - 279 ..= 699 - 120 and columns 80..160
            let lit: Vec<(usize, usize)> = (0..pixels.len()).filter(|i| pixels[*i] > 0).map(|i| (i % 500, i / 500)).collect();
            let (c0, c1) = (lit.iter().map(|l| l.0).min().unwrap(), lit.iter().map(|l| l.0).max().unwrap());
            let (r0, r1) = (lit.iter().map(|l| l.1).min().unwrap(), lit.iter().map(|l| l.1).max().unwrap());
            assert!(c0.abs_diff(80) <= 1 && c1.abs_diff(159) <= 1 && r0.abs_diff(420) <= 1 && r1.abs_diff(579) <= 1, "layer {k}: lit over columns {c0}..={c1}, rows {r0}..={r1}");
        }
        let m = makepad_strict_json::parse(&std::fs::read(dir.join(MANIFEST)).unwrap()).expect("the manifest is JSON");
        let num = |k: &str| match m.get(k) {
            Some(Value::F64(v)) => *v,
            Some(Value::Int(v)) => *v as f64,
            v => panic!("{k}: {v:?}"),
        };
        assert_eq!(m.get("format").and_then(Value::as_str), Some(FORMAT));
        assert_eq!(m.get("mirror").and_then(Value::as_str), Some("y"));
        assert_eq!((num("layer_count"), num("rotation_deg"), num("grey_levels")), (8.0, 90.0, 16.0));
        assert_eq!((num("layer_height_mm"), num("bottom_layers"), num("exposure_s")), (0.5, 4.0, 2.5));
        assert_eq!(m.get("image_size_px").map(Value::to_json).as_deref(), Some("[500,700]"));
        assert_eq!(m.get("layers").and_then(Value::as_arr).map(|l| l.len()), Some(8));
        assert!((num("print_time_s") - w.print_seconds).abs() < 1e-9 && (num("resin_ml") - w.resin_ml).abs() < 1e-9);

        // a shorter block over it: its 4 layers, and nothing of the 8
        let short = LayerStack::new(vec![block(2.0)], &p);
        assert_eq!(write(&dir, &short, &p).expect("the second export").layers, 4);
        let mut files: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
        files.sort();
        assert_eq!(files, ["layer_00000.png", "layer_00001.png", "layer_00002.png", "layer_00003.png", MANIFEST]);
        assert_eq!(std::fs::read_dir(&base).unwrap().count(), 1, "no folder written beside it stays");

        // a folder of something else is no export's to replace
        let other = base.join("notes.layers");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("notes.txt"), "mine").unwrap();
        let e = write(&other, &short, &p).expect_err("a folder in the way");
        assert!(e.contains("in the way"), "{e}");
        assert_eq!(std::fs::read_to_string(other.join("notes.txt")).unwrap(), "mine");
        std::fs::remove_dir_all(&base).unwrap();
    }
}
