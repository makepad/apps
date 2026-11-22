//! The plate's layers written as the printer's archive: the one file a resin printer that takes
//! no link reads from a USB stick (its format's bytes are in [`fab_sla_archive::format`]).
//!
//! It is a ZIP of, in this order: the settings document, the print's totals, the writer's note,
//! two previews, the per-layer table, one image per layer and the scene table ([`docs`],
//! [`preview`], [`runs`], [`scene`]). Every entry is deflated and carries its SHA-256 in a
//! private extra field of the central directory.
//!
//! - Each layer image is the screen's image as [`crate::export::screen_band`] mounts it, as a run
//!   stream of 4-bit greys; its cost is what the layer lights, never the whole screen.
//! - The scene table gives each layer's cured area (its pixels by their levels), its box in mm
//!   from the screen's centre in the image's frame, and its outlines (parts and holes).
//! - The file is written beside its place and renamed into it once whole, so a half-written file
//!   never shows on the stick.
//!
//! What a file holds is checked by the independent reader [`fab_sla_archive::read`], which
//! shares none of this code.

pub mod docs;
pub mod preview;
pub mod runs;
pub mod scene;

use crate::estimate::print_time;
use crate::export::{screen_band, Written};
use crate::image::LayerStack;
use crate::parts::LayerParts;
use crate::printer::{Encoding, ResinProfile};
use docs::Image;
use fab_sla_archive::format::{self, HASH_FIELD_ID, HASH_FIELD_LEN, HASH_FIELD_TAG};
use makepad_core_util::sha256::sha256;
use makepad_fab_slicer::par;
use makepad_fast_inflate::crc32;
use makepad_fast_inflate::deflate::compress_to_vec;
use makepad_zip_file::{Packed, ZipWriter};
use preview::{TopView, PREVIEWS};
use runs::run_layer;
use scene::{scene_table, SceneLayer};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The archive's extension, for a file name (without the dot).
pub use fab_sla_archive::format::EXTENSION;

/// The deflate level of every entry.
const LEVEL: u8 = 6;

/// The central directory's extra field carrying `data`'s SHA-256.
fn hash_field(data: &[u8]) -> Vec<u8> {
    let mut field = Vec::with_capacity(4 + HASH_FIELD_LEN as usize);
    for v in [HASH_FIELD_ID, HASH_FIELD_LEN, HASH_FIELD_TAG, 32] {
        field.extend_from_slice(&v.to_le_bytes());
    }
    field.extend_from_slice(&sha256(data));
    field
}

/// One entry, deflated, with its size and its hash field.
struct Entry {
    name: String,
    size: u64,
    packed: Packed,
    hash: Vec<u8>,
}

impl Entry {
    fn of(name: &str, data: &[u8]) -> Entry {
        Entry { name: name.into(), size: data.len() as u64, packed: Packed::from_deflated(crc32(data), compress_to_vec(data, LEVEL)), hash: hash_field(data) }
    }
}

/// The image the archive's layers hold on `job`'s screen: a quarter turn swaps its sides.
pub fn image_of(job: &ResinProfile) -> Image {
    let s = &job.screen;
    if s.quarter_turns % 2 == 1 {
        Image { pixels: [s.pixels[1], s.pixels[0]], pitch: [s.pitch[1], s.pitch[0]] }
    } else {
        Image { pixels: s.pixels, pitch: s.pitch }
    }
}

/// The archive of `stack` printed as `job`, and what it holds.
pub fn archive(stack: &LayerStack, job: &ResinProfile) -> Result<(Vec<u8>, Written), String> {
    let s = &job.screen;
    if stack.grid.pixels != s.pixels {
        return Err(format!("the layers are on a {:?} pixel screen, the printer's is {:?}", stack.grid.pixels, s.pixels));
    }
    if job.output.encoding != Encoding::Raster {
        return Err(format!("the resin printer {}: its archive's layers are written as raster only for now (fab_sla_layer_encoding raster)", job.name));
    }
    if s.grey_levels != 16 {
        return Err(format!("the resin printer {}: its archive's layers have 16 grey levels (fab_sla_grey_levels 16), not {}", job.name, s.grey_levels));
    }
    let image = image_of(job);
    let [pw, ph] = image.pitch;
    let half = [image.pixels[0] as f64 * pw / 2.0, image.pixels[1] as f64 * ph / 2.0];
    let views = PREVIEWS.map(|(size, _)| TopView::new(size, &stack.grid));
    let count = stack.layers.count;
    // each layer cut, mounted, run-coded and deflated on its own
    let layers: Vec<(Entry, SceneLayer)> = par::map(count as usize, |k| {
        let layer = stack.layer(k as u32);
        for v in &views {
            v.mark(&layer.spans, &stack.grid);
        }
        let run = run_layer(&screen_band(&layer.spans, s));
        let cured = LayerParts::of(&layer.spans);
        let contours = (cured.parts.len() + cured.holes().pixels.len()) as u32;
        let bounds = run.bounds.map_or([0.0; 4], |[x0, y0, x1, y1]| [x0 as f64 * pw - half[0], y0 as f64 * ph - half[1], x1 as f64 * pw - half[0], y1 as f64 * ph - half[1]].map(|v| v as f32));
        let scene = SceneLayer { area: (run.lit * pw * ph) as f32, bounds, contours };
        (Entry::of(&format::layer_entry(k as u32, format::RASTER_EXTENSION), &run.stream), scene)
    });
    let records: Vec<SceneLayer> = layers.iter().map(|l| l.1).collect();
    let lit = records.iter().filter(|r| r.area > 0.0);
    let mut parts = [0.0f32, 0.0, 0.0, 0.0, 0.0, (count as f64 * job.layer_height) as f32];
    for (i, r) in lit.enumerate() {
        let b = r.bounds;
        parts = if i == 0 { [b[0], b[1], 0.0, b[2], b[3], parts[5]] } else { [parts[0].min(b[0]), parts[1].min(b[1]), 0.0, parts[3].max(b[2]), parts[4].max(b[3]), parts[5]] };
    }
    let ml = records.iter().map(|r| r.area as f64).sum::<f64>() * job.layer_height / 1000.0;
    let seconds = print_time(job, count);
    let text = |doc: &makepad_strict_json::Value| docs::text_of(doc).into_bytes();
    let leading = [
        (format::SETTINGS, text(&docs::settings(job, &image))),
        (format::PRINT_INFO, text(&docs::print_info(job, ml, seconds))),
        (format::SOFTWARE, text(&docs::software_info())),
        (format::PREVIEW_SMALL, views[0].png(PREVIEWS[0].1)),
        (format::PREVIEW_LARGE, views[1].png(PREVIEWS[1].1)),
        (format::LAYER_TABLE, text(&docs::layer_table(job, count))),
    ];
    let mut zip = ZipWriter::new();
    let mut add = |e: &Entry| zip.add_deflated(&e.name, e.size, &e.packed, &e.hash).map_err(|e| format!("the archive: {e}"));
    for (name, data) in &leading {
        add(&Entry::of(name, data))?;
    }
    for (entry, _) in &layers {
        add(entry)?;
    }
    add(&Entry::of(format::SCENE, &scene_table(job.layer_height, &records, parts)))?;
    let bytes = zip.finish().map_err(|e| format!("the archive: {e}"))?;
    let written = Written { layers: count, bytes: bytes.len() as u64, resin_ml: ml, print_seconds: seconds };
    Ok((bytes, written))
}

/// A file name the printer lists: `name`'s ASCII letters and digits, `-` and `_` (anything else
/// one `_`), the archive's extension, 60 characters at most in all.
pub fn file_name(name: &str) -> String {
    let ext = format!(".{}", format::EXTENSION);
    let mut stem = String::new();
    for c in name.chars() {
        let c = if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' };
        if !(c == '_' && stem.ends_with('_')) {
            stem.push(c);
        }
    }
    let stem = stem.trim_matches('_');
    let stem: String = if stem.is_empty() { "print".into() } else { stem.chars().take(60 - ext.len()).collect() };
    format!("{stem}{ext}")
}

/// Writes `stack`, printed as `job`, as the archive `path`. The file is written beside it and
/// flushed to the disk, then renamed into place: a file already at `path` is replaced whole,
/// and a half-written one never shows there.
pub fn write(path: &Path, stack: &LayerStack, job: &ResinProfile) -> Result<Written, String> {
    let (bytes, written) = archive(stack, job)?;
    let name = path.file_name().ok_or_else(|| format!("{} names no file", path.display()))?.to_string_lossy().to_string();
    let part: PathBuf = path.with_file_name(format!(".{name}.writing-{}", std::process::id()));
    let put = || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&part)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        std::fs::rename(&part, path)
    };
    put().map_err(|e| {
        let _ = std::fs::remove_file(&part);
        format!("writing {}: {e}", path.display())
    })?;
    Ok(written)
}

/// Saves `stack`, printed as `job`, as the archive `path` ([`write`]), then reads the file back
/// from where it went and checks it with the independent reader: what is there is a file the
/// printer reads, whole.
pub fn save(path: &Path, stack: &LayerStack, job: &ResinProfile) -> Result<Written, String> {
    let written = write(path, stack, job)?;
    let bytes = std::fs::read(path).map_err(|e| format!("reading {} back: {e}", path.display()))?;
    if bytes.len() as u64 != written.bytes {
        return Err(format!("{} reads back as {} bytes, not the {} written", path.display(), bytes.len(), written.bytes));
    }
    let back = fab_sla_archive::read::read(bytes).map_err(|f| format!("{} doesn't read back as the printer reads it: {f}", path.display()))?;
    if back.layers != written.layers {
        return Err(format!("{} reads back with {} layers, not {}", path.display(), back.layers, written.layers));
    }
    Ok(written)
}

#[cfg(test)]
mod tamper_tests;
#[cfg(test)]
pub(crate) mod tests;
