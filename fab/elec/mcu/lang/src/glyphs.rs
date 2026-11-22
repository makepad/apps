//! A firmware's glyph atlas. `let glyphs = glyph_atlas(16, 28)` names a read-only image of
//! the Latin-1 characters 32..=255 in cells of that size, sixteen cells to a row. Character
//! `c` sits at cell `c - 32`, so at x `(c - 32) % 16 * w` and y `(c - 32) / 16 * h`. Cell 95
//! (character 127, which draws nothing) is a box for characters the program can't show.
//!
//! The glyphs come from the monospaced font the widgets ship, JetBrains Mono 2.304 (its name
//! table: SIL Open Font License 1.1; credited in Fab's CREDITS.md), at its default instance,
//! white on black. makepad-font-core rasterises them when the image is built. Nothing generated is stored in a project or the repository. A process
//! keeps each atlas it made, keyed by the font, the cell, the characters and this renderer's
//! version, so a rebuild reuses it.

use crate::assets::Image as Image565;
use makepad_font_core::font_face::{FontData, FontFace};
use makepad_font_core::geom::{Point, Rect, Size};
use makepad_font_core::image::{Image, R};
use std::collections::HashMap;
use makepad_script_compute::parse::{Expr, ExprKind, Item};
use makepad_script_compute::ShaderError;
use std::sync::{Arc, Mutex, OnceLock};

const FONT_NAME: &str = "JetBrains Mono 2.304";
const FONT: &[u8] = include_bytes!("../../../../../../../widgets/resources/jetbrains_mono_variable.ttf");
const FIRST: u32 = 32;
const LAST: u32 = 255;
const COLUMNS: u32 = 16;
/// The cell of the box for characters outside the atlas: character 127, which has no glyph.
const BOX: u32 = 127;
/// Bump when the rasterisation changes, so no cached atlas outlives it.
const RENDERER: u32 = 1;

type Key = (&'static str, u32, u32, u32, u32, u32);

/// An atlas's size in pixels for cells of `w` x `h`.
pub fn glyph_atlas_size(w: u32, h: u32) -> (u32, u32) {
    (w * COLUMNS, h * (LAST - FIRST + 1).div_ceil(COLUMNS))
}

/// The atlas for cells of `w` x `h` as RGB565 pixels, row after row
/// (`glyph_atlas_size(w, h)`), made once per process.
pub fn glyph_atlas(w: u32, h: u32) -> Arc<Vec<u16>> {
    static MADE: OnceLock<Mutex<HashMap<Key, Arc<Vec<u16>>>>> = OnceLock::new();
    let key = (FONT_NAME, w, h, FIRST, LAST, RENDERER);
    let made = MADE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(atlas) = made.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return atlas.clone();
    }
    // Rasterised outside the lock: a build waits only for the map, never for another's atlas.
    let atlas = Arc::new(render_glyph_atlas(w, h));
    made.lock().unwrap_or_else(|e| e.into_inner()).entry(key).or_insert(atlas).clone()
}

/// The atlas drawn afresh, without the cache: what `glyph_atlas` keeps.
pub fn render_glyph_atlas(w: u32, h: u32) -> Vec<u16> {
    let (width, height) = glyph_atlas_size(w, h);
    let mut grey = vec![0u8; (width * height) as usize];
    let face = FontFace::from_data_and_index(FontData::from_vec(FONT.to_vec()), 0)
        .expect("JetBrains Mono parses");
    let (units, ascender, descender, advance) = face.with_ttf_parser_face(|f| {
        let advance = f.glyph_index('M').and_then(|g| f.glyph_hor_advance(g)).unwrap_or(f.units_per_em());
        (f32::from(f.units_per_em()), f32::from(f.ascender()), f32::from(f.descender()), f32::from(advance))
    });
    // The largest size whose advance fits the cell's width and whose ascent and descent fit
    // its height; the glyph box is centred in the cell.
    let scale = (w as f32 / advance).min(h as f32 / (ascender - descender));
    let pen = (w as f32 - advance * scale) / 2.0;
    let baseline = ((h as f32 - (ascender - descender) * scale) / 2.0 + ascender * scale).round();
    for c in FIRST..=LAST {
        let cell = c - FIRST;
        let (x0, y0) = ((cell % COLUMNS * w) as i32, (cell / COLUMNS * h) as i32);
        let mut put = |x: i32, y: i32, a: u8| {
            if (0..w as i32).contains(&x) && (0..h as i32).contains(&y) {
                let at = ((y0 + y) as u32 * width + (x0 + x) as u32) as usize;
                grey[at] = grey[at].max(a);
            }
        };
        if c == BOX {
            // A rectangle from the cap line to the baseline, one pixel wide, inset from the cell.
            let (left, right) = (pen.round() as i32 + 1, (w as f32 - pen).round() as i32 - 2);
            let top = (baseline - ascender * scale * 0.7).round() as i32;
            let bottom = baseline as i32 - 1;
            for x in left..=right { put(x, top, 255); put(x, bottom, 255); }
            for y in top..=bottom { put(left, y, 255); put(right, y, 255); }
            continue;
        }
        let Some(ch) = char::from_u32(c) else { continue };
        let Some(glyph) = face.with_ttf_parser_face(|f| f.glyph_index(ch)) else { continue };
        let Some(outline) = face.glyph_outline(glyph.0, units) else { continue };
        let bounds = outline.bounds_in_ems();
        let dpx = units * scale;
        let px = |ems: f32| ems * dpx;
        let gw = px(bounds.size.width).ceil() as usize + 1;
        let gh = px(bounds.size.height).ceil() as usize + 1;
        let mut image = Image::<R>::new(Size::new(gw, gh));
        outline.rasterize(dpx, &mut image.subimage_mut(Rect::new(Point::new(0, 0), Size::new(gw, gh))));
        // The rasteriser's lowest row is the bottom of the glyph's bounds and lands on the
        // image's last row; its first column is their left.
        let left = (pen + px(bounds.origin.x)).round() as i32;
        let top = (baseline - px(bounds.origin.y)).round() as i32 - gh as i32;
        for y in 0..gh {
            for x in 0..gw {
                put(left + x as i32, top + y as i32, image[Point::new(x, y)].bits);
            }
        }
    }
    grey.iter().map(|&g| {
        let g = u16::from(g);
        ((g >> 3) << 11) | ((g >> 2) << 5) | (g >> 3)
    }).collect()
}

/// Each top-level `let name = glyph_atlas(w, h)` becomes one more image after the folder's
/// pictures, declared with them (`name`, `name_w`, `name_h`), so the program draws a character
/// with the `present` it draws every picture with.
pub(crate) fn declare(items: &mut Vec<Item>, images: &mut Vec<Image565>) -> Result<(), ShaderError> {
    let mut error = None;
    items.retain(|item| {
        let Item::Let { name, value, .. } = item else { return true };
        let ExprKind::Call(form, args) = &value.kind else { return true };
        if form != "glyph_atlas" || error.is_some() { return true; }
        let fail = |message: String| Some(ShaderError::new(value.span.start, value.span.end, message));
        let cell = |arg: Option<&Expr>| match arg.map(|a| &a.kind) {
            Some(&ExprKind::Num(n, _)) if n >= 2.0 && n <= f64::from(u16::MAX) && n % 2.0 == 0.0 => Some(n as u32),
            _ => None,
        };
        let (Some(w), Some(h), 2) = (cell(args.first()), cell(args.get(1)), args.len()) else {
            error = fail("glyph_atlas(w, h) takes the cell's width and height in pixels: even whole numbers, as the panel takes only even windows".into());
            return false;
        };
        let names = |stem: &str| [stem.to_string(), format!("{stem}_w"), format!("{stem}_h")];
        if let Some(other) = images.iter().find(|image| names(&image.name).iter().any(|n| names(name).contains(n))) {
            error = fail(format!("the glyph atlas `{name}` and the image `{}` declare the same names", other.name));
            return false;
        }
        let (width, height) = glyph_atlas_size(w, h);
        images.push(Image565 { name: name.clone(), width, height, pixels: glyph_atlas(w, h).as_ref().clone() });
        false
    });
    error.map_or(Ok(()), Err)
}
