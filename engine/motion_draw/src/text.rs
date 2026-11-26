//! Text items: the whole text laid out once with Makepad's text layouter,
//! each unit (letter, word, line, syllable) found in the layout by its
//! glyphs, and drawn at its own transform. A unit that is only moved and
//! scaled uniformly draws straight into the canvas; one that turns,
//! stretches or blurs is rasterised in its own box first and placed by its
//! affine. Plates draw under the glyphs (one per line, or per unit), a
//! typewriter `reveal` cuts the text at a character, and a caret stands
//! after the last shown one.

use super::MotionDraw;
use makepad_motion::frame::{Affine, Item, ItemKind, PlateFrame};
use makepad_motion::geometry;
use makepad_motion::model::{Align as TextAlign, Cap, Join};
use makepad_widgets::*;

/// Where a unit sits in the text's own box, in document units.
#[derive(Clone, Debug)]
pub(super) struct UnitBox {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// A text item's layout: its box, each unit's, each row's extent and where
/// every glyph ends (for the caret).
#[derive(Clone, Debug)]
pub(super) struct TextLayout {
    pub size: [f64; 2],
    /// The font size drawn: the item's, or smaller to meet its `fit`.
    pub font_size: f64,
    /// Where the box starts relative to `at`, by `align` and `valign`.
    pub shift: [f64; 2],
    pub units: Vec<UnitBox>,
    /// `[x0, x1, top, height]` of each row with glyphs.
    pub rows: Vec<[f64; 4]>,
    /// `(byte just after the glyph, x where it ends, row)`, in text order.
    pub ends: Vec<(usize, f64, usize)>,
    /// Every glyph: `(byte where it starts, x where it starts, row)`.
    pub starts: Vec<(usize, f64, usize)>,
    /// Each layout row's top, relative to the text's box.
    pub row_tops: Vec<f64>,
    /// Whether letter spacing is set (glyphs then draw one by one).
    pub tracked: bool,
}

impl TextLayout {
    /// A unit's characters `range` as pieces of one row each, with where
    /// each piece starts relative to the unit's box `b`: drawn piece by
    /// piece, a unit that spans rows keeps the layout's line breaks and
    /// alignment (one `draw_abs` of the whole range would lay it out
    /// again on one left-aligned line).
    pub fn pieces(&self, text: &str, range: &std::ops::Range<usize>, b: &UnitBox) -> Vec<(std::ops::Range<usize>, [f64; 2])> {
        let mut out: Vec<(std::ops::Range<usize>, [f64; 2])> = Vec::new();
        let mut current: Option<(usize, usize, f64, usize)> = None;
        for &(at, x, row) in self.starts.iter().filter(|g| range.contains(&g.0)) {
            match &mut current {
                // With letter spacing every glyph stands at its own spaced
                // place (a run drawn at once would take the font's own
                // advances and run past its box).
                Some((_, end, _, r)) if *r == row && !self.tracked => *end = at,
                _ => {
                    if let Some((start, end, x0, r)) = current.take() {
                        out.push((start..next_char(text, end), [x0 - b.x, self.rows_top(r) - b.y]));
                    }
                    current = Some((at, at, x, row));
                }
            }
        }
        if let Some((start, end, x0, r)) = current {
            out.push((start..next_char(text, end), [x0 - b.x, self.rows_top(r) - b.y]));
        }
        // Clip each piece to the range (a piece ends after its last glyph).
        for (piece, _) in &mut out {
            piece.end = piece.end.min(range.end);
        }
        out
    }

    fn rows_top(&self, row: usize) -> f64 {
        self.row_tops.get(row).copied().unwrap_or(0.0)
    }
}

fn next_char(text: &str, at: usize) -> usize {
    at + text.get(at..).and_then(|rest| rest.chars().next()).map_or(1, char::len_utf8)
}

/// Extra room around a rasterised unit for glyph overhang, in ems.
const PAD_EMS: f64 = 0.25;

/// Note the glyphs of `text` at a face and pixel size. A glyph drawn for
/// the first time may not be in the atlas the draw samples until the next
/// frame, so a recording that draws one is not exact. In realtime (a
/// preview) a glyph counts once per face whatever its size: text at a
/// zooming camera's sizes would otherwise make every frame inexact (and
/// redraw the kept static layers every frame); the text draw asks for a
/// redraw itself when a glyph is not ready.
pub(crate) fn note_glyphs(md: &mut MotionDraw, font: &str, weight: f64, italic: bool, axes: &[(String, f64)], px: f64, text: &str) {
    let face = LiveId(axes_key(drawn(font), weight, italic, axes));
    let face = if md.locked { face.bytes_append(&((px * 100.0).round() as i64).to_le_bytes()).0 } else { face.0 };
    for c in text.chars().filter(|c| !c.is_whitespace()) {
        if md.glyphs_seen.insert((face, c)) {
            md.fresh_glyphs = true;
            md.fresh_why = "a glyph's first draw at this face and size";
        }
    }
}

/// The glyph size a unit scaled by `s` draws at: the scale snapped to
/// 1/32-octave steps (at most 1.1% off, unseen while it moves; 1/6
/// below half size), so a
/// letter scaling in reuses the glyph sizes other letters and frames
/// rasterised instead of asking for new ones every frame. Unscaled text
/// (`s` = 1) is exact.
fn glyph_px(size: f64, s: f64) -> f64 {
    if (s - 1.0).abs() < 1e-9 || s <= 0.0 {
        return size * s.max(0.0);
    }
    // Coarser below half size, where a letter is small and in flight.
    let steps = if s < 0.5 { 6.0 } else { 32.0 };
    size * ((s.log2() * steps).round() / steps).exp2()
}

/// The family name `font` draws in (`makepad_doc_fonts::drawn_family`: the
/// default family for one not installed, its own once installed), so the
/// faces kept here follow an install.
fn drawn(font: &str) -> &str {
    makepad_doc_fonts::drawn_family(font).map_or(font, |family| family.name)
}

/// A face with its variable-axis values, as a key.
fn axes_key(font: &str, weight: f64, italic: bool, axes: &[(String, f64)]) -> u64 {
    let mut id = LiveId::from_str(font).bytes_append(&weight.to_bits().to_le_bytes()).bytes_append(&[italic as u8]);
    for (tag, value) in axes {
        id = id.str_append(tag).bytes_append(&value.to_bits().to_le_bytes());
    }
    id.0
}

/// The face `font` draws in at `weight`: the family's file in
/// `makepad_doc_fonts` read through a text style made once per face and
/// whole weight (Inter behind it for the symbols it lacks, where the family
/// says so); a family that is not installed draws in the default face.
/// `None` for a file that does not load (said once).
pub(crate) fn face(cx: &mut Cx, font: &str, weight: f64, italic: bool) -> Option<TextStyle> {
    let font = drawn(font);
    let weight = makepad_doc_fonts::family(font).map_or(weight, |family| makepad_doc_fonts::drawn_weight(family, weight, italic));
    let key = axes_key(font, weight, italic, &[]);
    if let Some(face) = cx.global::<Faces>().file.get(&key) {
        return face.clone();
    }
    let made = file_face(cx, font, weight, italic);
    let made = made.map_err(|error| error!("font \"{font}\": {error}; its text is not drawn")).ok();
    cx.global::<Faces>().file.insert(key, made.clone());
    made
}

/// The faces made so far, one set for every draw of the app: a face is
/// its file read and evaluated into a text style, so a draw made for
/// another plate (or the next frame of a new one) reads none again.
#[derive(Default)]
struct Faces {
    /// Installed external fonts' faces by font, weight and italic
    /// (`None`: the file did not load; its text is not drawn).
    file: std::collections::HashMap<u64, Option<TextStyle>>,
    /// Faces read at variable-axis values, by [`axes_key`].
    axes: std::collections::HashMap<u64, TextStyle>,
}

/// A text style reading the family's file for `weight` and `italic`
/// (`weight` on a variable file's `wght`), Inter behind it.
fn file_face(cx: &mut Cx, font: &str, weight: f64, italic: bool) -> Result<TextStyle, String> {
    use makepad_doc_fonts::{self as fonts, Place};
    let family = fonts::family(font).ok_or_else(|| format!("{font} is no document font"))?;
    let file = fonts::pick(family, weight, italic).ok_or_else(|| format!("{} has no face", family.name))?;
    let mut resource_crate = None;
    let resource = match file.place {
        Place::Resource(resource) => {
            let (krate, path) = resource.split_once(':').ok_or_else(|| format!("{resource} is no crate resource"))?;
            resource_crate = fonts::root_of(krate).map(|root| (krate.to_string(), root.to_string_lossy().into_owned()));
            format!("crate_resource({:?})", format!("{krate}:{path}"))
        }
        Place::Download { .. } => {
            let path = fonts::local_path(file).ok_or_else(|| format!("{} is not installed", family.name))?;
            #[cfg(target_arch = "wasm32")]
            let resource_method = "http_resource";
            #[cfg(not(target_arch = "wasm32"))]
            let resource_method = "file_resource";
            format!("mod.res.{resource_method}({:?})", path.to_string_lossy().replace('\\', "/"))
        }
    };
    let ascent = family.ascent;
    let weight_of_file = if fonts::variable(file) { format!(" weight: {weight}") } else { String::new() };
    let symbols = if family.symbols {
        format!(" symbols := FontMember{{res: crate_resource(\"makepad_widgets:resources/Inter.ttf\") asc: {ascent:?} desc: 0.0 weight: {weight}}}")
    } else {
        String::new()
    };
    // Chinese and Japanese characters the family lacks draw in LXGW WenKai
    // (an output drawn in another language; a document's own CJK text).
    let cjk_file = if weight >= 600.0 { "LXGWWenKaiBold.ttf" } else { "LXGWWenKaiRegular.ttf" };
    let cjk = format!(" cjk := FontMember{{res: crate_resource(\"makepad_widgets:resources/{cjk_file}\") asc: {ascent:?} desc: 0.0}}");
    let code = format!(
        "use mod.prelude.widgets_internal.*\nTextStyle{{font_family: FontFamily{{latin := FontMember{{res: {resource} asc: {ascent:?} desc: 0.0{weight_of_file}}}{symbols}{cjk}}} line_spacing: 1.2}}\n"
    );
    cx.with_vm(|vm| {
        // The crate's resources are where the font table says (the VM's own
        // entry for a crate can have been moved by another module of it).
        if let Some((krate, root)) = resource_crate {
            vm.register_crate_manifest(&krate, &root);
        }
        vm.capture_errors();
        let value = vm.eval(ScriptMod::new("", "", "motion://font", 0, 0, &code, &[]));
        let errors = vm.take_errors();
        if value.is_err() || !errors.is_empty() {
            return Err(if errors.is_empty() { "the face did not evaluate".to_string() } else { errors.join("; ") });
        }
        Ok(TextStyle::script_from_value(vm, value))
    })
}

/// The text style a document font draws in at `weight`, `italic` and the
/// variable `axes` (`wdth`, `slnt`, `opsz`, …), as the picture draws it:
/// for an editor that shows a family in its own face (a font picker's
/// rows). Made once per face and kept, like the picture's: a family that is
/// not installed in the default face. `None` for a file that does not load.
pub fn face_style(cx: &mut Cx, font: &str, weight: f64, italic: bool, axes: &[(String, f64)]) -> Option<TextStyle> {
    let font = drawn(font);
    if axes.is_empty() {
        return face(cx, font, weight, italic);
    }
    let key = axes_key(font, weight, italic, axes);
    if let Some(face) = cx.global::<Faces>().axes.get(&key) {
        return Some(face.clone());
    }
    let mut face = face(cx, font, weight, italic)?;
    let tags: Vec<(u32, f32)> = axes.iter().filter_map(|(tag, value)| axis_tag(tag).map(|t| (t, *value as f32))).collect();
    face.font_family = face.font_family.with_axes(&tags);
    cx.global::<Faces>().axes.insert(key, face.clone());
    Some(face)
}

/// Set the text draw's face and size; false when the face's file does not
/// load (nothing is drawn in another face). The made face is read in place: a
/// draw that already holds the family keeps it, so a repeated text costs a
/// comparison, not a copy of the style.
pub(crate) fn set_face(md: &mut MotionDraw, cx: &mut Cx, font: &str, weight: f64, italic: bool, axes: &[(String, f64)], size: f64) -> bool {
    let font = drawn(font);
    let key = if axes.is_empty() {
        let drawn = makepad_doc_fonts::family(font).map_or(weight, |family| makepad_doc_fonts::drawn_weight(family, weight, italic));
        axes_key(font, drawn, italic, &[])
    } else {
        axes_key(font, weight, italic, axes)
    };
    let made = {
        let faces = cx.global::<Faces>();
        if axes.is_empty() { faces.file.contains_key(&key) } else { faces.axes.contains_key(&key) }
    };
    if !made && face_style(cx, font, weight, italic, axes).is_none() {
        return false;
    }
    let faces = cx.global::<Faces>();
    let Some(face) = (if axes.is_empty() { faces.file.get(&key).and_then(|f| f.as_ref()) } else { faces.axes.get(&key) }) else { return false };
    let style = &mut md.draws.text.text_style;
    if style.font_family != face.font_family {
        style.clone_from(face);
    }
    style.font_size = size.max(0.01) as f32;
    style.line_spacing = face.line_spacing;
    style.top_drop = face.top_drop;
    true
}

pub(super) fn layout(md: &mut MotionDraw, cx: &mut Cx, item: &Item) -> Option<TextLayout> {
    let ItemKind::Text { text, font, weight, italic, axes, size, align, valign, line_height, tracking, wrap, fit, units, .. } = &item.kind else { return None };
    let af = match align {
        TextAlign::Left => 0.0,
        TextAlign::Center => 0.5,
        TextAlign::Right => 1.0,
    };
    // `fit`: shrink the size until the widest line fits (glyph advances
    // scale with the size, so this settles in a step or two; the few
    // passes keep it exact where hinting or a re-wrap moves a line).
    let mut font_size = *size;
    let mut laid;
    let mut pass = 0;
    loop {
        if !set_face(md, cx, font, *weight, *italic, axes, font_size) {
            return None;
        }
        md.draws.text.text_style.line_spacing = *line_height as f32;
        laid = md.draws.text.layout(cx, 0.0, 0.0, wrap.map(|w| w as f32), wrap.is_some(), Align { x: af, y: 0.0 }, text);
        let Some(fit) = fit.filter(|f| *f > 0.0) else { break };
        let width = laid.rows.iter().map(|row| row.width_in_lpxs as f64 + tracking * (row.glyphs.len().saturating_sub(1)) as f64).fold(0.0, f64::max);
        if width <= fit || pass >= 4 || font_size <= 1.0 {
            break;
        }
        font_size = (font_size * fit / width * 0.999).max(1.0);
        pass += 1;
    }
    // Explicit lines (no `wrap`) centred or right-aligned: the engine
    // aligns rows inside a width, so give it the widest row's.
    if wrap.is_none() && af > 0.0 && laid.rows.len() > 1 {
        let widest = laid.rows.iter().map(|row| row.width_in_lpxs).fold(0.0f32, f32::max);
        laid = md.draws.text.layout(cx, 0.0, 0.0, Some(widest + 1.0), true, Align { x: af, y: 0.0 }, text);
    }
    let base = laid.text.as_ptr() as usize;
    let base_width = laid.rows.iter().map(|row| row.width_in_lpxs as f64 + tracking * (row.glyphs.len().saturating_sub(1)) as f64).fold(0.0, f64::max);
    let has_unit_axes = units.iter().any(|unit| !unit.axes.is_empty());
    let mut width = 0.0f64;
    // Every glyph with its byte offset in `text`, x, advance and row.
    let mut glyphs: Vec<(usize, f64, f64, usize)> = Vec::new();
    let mut row_boxes: Vec<(f64, f64)> = Vec::new();
    // Per-unit axes reflow advances within the original rows; wrapping is
    // not recomputed.
    for (r, row) in laid.rows.iter().enumerate() {
        let row_start = (row.text.as_ptr() as usize).saturating_sub(base);
        let glyph_at = |cluster| if cluster < row.text.len() { row_start + cluster } else { cluster };
        let top = row.origin_in_lpxs.y as f64 - row.ascender_in_lpxs as f64;
        let height = (row.ascender_in_lpxs - row.descender_in_lpxs) as f64;
        row_boxes.push((top, height));
        let row_glyph_start = glyphs.len();
        let mut delta = 0.0;
        let mut k = 0;
        while k < row.glyphs.len() {
            let glyph = &row.glyphs[k];
            let at = glyph_at(glyph.cluster);
            let x = row.origin_in_lpxs.x as f64 + glyph.origin_in_lpxs.x as f64 + tracking * k as f64 + delta;
            let unit = if has_unit_axes {
                units.get(units.partition_point(|unit| unit.range.end <= at))
                    .filter(|unit| unit.range.contains(&at) && !unit.axes.is_empty())
            } else { None };
            let Some(unit) = unit else {
                glyphs.push((at, x, glyph.advance_in_lpxs() as f64, r));
                k += 1;
                continue;
            };
            let start = unit.range.start.max(row_start);
            let end = unit.range.end.min(row_start + row.text.len());
            let piece = &text[start..end];
            let plain = md.draws.text.layout(cx, 0.0, 0.0, None, false, Align { x: 0.0, y: 0.0 }, piece);
            let style = md.draws.text.text_style.clone();
            if !set_face(md, cx, font, *weight, *italic, &unit.axes, font_size) {
                return None;
            }
            md.draws.text.text_style.line_spacing = *line_height as f32;
            let varied = md.draws.text.layout(cx, 0.0, 0.0, None, false, Align { x: 0.0, y: 0.0 }, piece);
            md.draws.text.text_style = style;
            let plain = &plain.rows[0];
            let varied = &varied.rows[0];
            let first = k;
            while k < row.glyphs.len() && unit.range.contains(&glyph_at(row.glyphs[k].cluster)) {
                k += 1;
            }
            let unit_glyph_start = glyphs.len();
            for (j, glyph) in varied.glyphs.iter().enumerate() {
                glyphs.push((start + glyph.cluster, x + glyph.origin_in_lpxs.x as f64 + tracking * j as f64, glyph.advance_in_lpxs() as f64, r));
            }
            // Preserve the surrounding run's spacing while changing this
            // unit's advance by the measured difference between the faces.
            let last = &row.glyphs[k - 1];
            let advance = last.origin_in_lpxs.x as f64 + last.advance_in_lpxs() as f64 - row.glyphs[first].origin_in_lpxs.x as f64;
            if let Some(last) = glyphs[unit_glyph_start..].last_mut() {
                last.2 += advance - plain.width_in_lpxs as f64;
            }
            delta += varied.width_in_lpxs as f64 - plain.width_in_lpxs as f64
                + tracking * (varied.glyphs.len() as f64 - (k - first) as f64);
        }
        for glyph in &mut glyphs[row_glyph_start..] {
            glyph.1 -= af * delta;
        }
        width = width.max(row.width_in_lpxs as f64 + tracking * row.glyphs.len().saturating_sub(1) as f64 + delta);
    }
    // Resize the text box without applying its alignment a second time.
    if has_unit_axes && af > 0.0 {
        for glyph in &mut glyphs {
            glyph.1 += af * (width - base_width);
        }
    }
    let top0 = row_boxes.first().map_or(0.0, |r| r.0);
    let height = row_boxes.last().map_or(0.0, |r| r.0 + r.1) - top0;
    let boxes = units.iter().map(|unit| {
        let inside: Vec<&(usize, f64, f64, usize)> = glyphs.iter().filter(|g| unit.range.contains(&g.0)).collect();
        match inside.first() {
            Some(_) => {
                // Every row the unit's glyphs are on (a wrapped line is one
                // unit over several rows).
                let x0 = inside.iter().map(|g| g.1).fold(f64::MAX, f64::min);
                let x1 = inside.iter().map(|g| g.1 + g.2).fold(f64::MIN, f64::max);
                let top = inside.iter().map(|g| row_boxes[g.3].0).fold(f64::MAX, f64::min);
                let bottom = inside.iter().map(|g| row_boxes[g.3].0 + row_boxes[g.3].1).fold(f64::MIN, f64::max);
                UnitBox { x: x0, y: top - top0, w: (x1 - x0).max(0.0), h: bottom - top }
            }
            None => UnitBox { x: 0.0, y: 0.0, w: 0.0, h: 0.0 },
        }
    }).collect();
    let rows = row_boxes.iter().enumerate().filter_map(|(r, (top, h))| {
        let row: Vec<&(usize, f64, f64, usize)> = glyphs.iter().filter(|g| g.3 == r).collect();
        let x0 = row.iter().map(|g| g.1).fold(f64::MAX, f64::min);
        let x1 = row.iter().map(|g| g.1 + g.2).fold(f64::MIN, f64::max);
        (x1 > x0).then_some([x0, x1, top - top0, *h])
    }).collect();
    let ends = glyphs.iter().map(|g| (g.0 + text.get(g.0..).and_then(|rest| rest.chars().next()).map_or(1, char::len_utf8), g.1 + g.2, g.3)).collect();
    let vf = match valign {
        makepad_motion::model::VAlign::Top => 0.0,
        makepad_motion::model::VAlign::Center => 0.5,
        makepad_motion::model::VAlign::Bottom => 1.0,
    };
    let starts = glyphs.iter().map(|g| (g.0, g.1, g.3)).collect();
    let row_tops = row_boxes.iter().map(|(top, _)| top - top0).collect();
    Some(TextLayout { starts, row_tops, size: [width, height], font_size, shift: [-width * af, -height * vf], units: boxes, rows, ends, tracked: tracking.abs() > 1e-9 })
}

/// Where the camera flies into letter `letter` (a character index) of a
/// Text item: its left stem (so a round letter's counter does not take
/// the frame), half way down its line, in pass pixels.
pub(super) fn letter_point(md: &mut MotionDraw, cx: &mut Cx2d, item: &Item, letter: usize) -> Option<[f64; 2]> {
    let layout = layout(md, cx, item)?;
    let ItemKind::Text { text, .. } = &item.kind else { return None };
    let byte = text.char_indices().nth(letter).map(|(b, _)| b)?;
    let k = layout.starts.iter().position(|s| s.0 == byte)?;
    let (x0, row) = (layout.starts[k].1, layout.starts[k].2);
    let x1 = layout.ends.get(k).map_or(x0 + layout.font_size * 0.5, |e| e.1);
    let top = layout.row_tops.get(row).copied().unwrap_or(0.0);
    let h = layout.rows.iter().find(|r| (r[2] - top).abs() < 1e-6).map_or(layout.font_size * 1.2, |r| r[3]);
    Some(text_box(md, item, &layout).apply([x0 + (x1 - x0) * 0.14, top + h * 0.55]))
}

/// The map from the text's own box to pass pixels.
fn text_box(md: &MotionDraw, item: &Item, layout: &TextLayout) -> Affine {
    md.base
        .then(&item.world)
        .then(&Affine::translate(layout.shift[0], layout.shift[1]))
        .then(&item.local.affine(layout.size))
}

/// Each unit's pixel map: from its own box (0..w, 0..h) to pass pixels.
fn unit_maps(md: &MotionDraw, item: &Item, layout: &TextLayout) -> Vec<Affine> {
    let ItemKind::Text { units, path, .. } = &item.kind else { return Vec::new() };
    // Text on a path: each unit's centre rides the path at its distance
    // along the line, turned to the path's direction.
    if let Some((data, offset)) = path {
        let line = makepad_motion::Polyline::from_path(data);
        let total = line.length().max(1e-9);
        let base = md.base.then(&item.world);
        return units.iter().zip(&layout.units).map(|(unit, b)| {
            let s = offset * total + b.x + b.w * 0.5;
            // Letters not on the path yet (or past its end) are not drawn.
            if s < 0.0 || s > total {
                return Affine::scale(0.0, 0.0);
            }
            let u = s / total;
            let p = line.point(u);
            let (p0, p1) = (line.point((u - 0.002).max(0.0)), line.point((u + 0.002).min(1.0)));
            let angle = (p1[1] - p0[1]).atan2(p1[0] - p0[0]).to_degrees();
            let own = Affine::rotate(unit.rotate).then(&Affine::scale(unit.scale[0], unit.scale[1]));
            base.then(&Affine::translate(p[0] + unit.offset[0], p[1] + unit.offset[1]))
                .then(&Affine::rotate(angle))
                .then(&Affine::translate(-b.w * 0.5, -b.h * 0.8))
                .then(&Affine::about([b.w * 0.5, b.h * 0.5], &own))
        }).collect();
    }
    let text_box = text_box(md, item, layout);
    units.iter().zip(&layout.units).map(|(unit, b)| {
        let own = Affine::rotate(unit.rotate).then(&Affine::scale(unit.scale[0], unit.scale[1]));
        text_box
            .then(&Affine::translate(b.x + unit.offset[0], b.y + unit.offset[1]))
            .then(&Affine::about([b.w * 0.5, b.h * 0.5], &own))
    }).collect()
}

fn direct(m: &Affine) -> bool {
    m.is_axis_aligned() && m.a > 0.0 && (m.a - m.d).abs() <= 1e-6 * m.a.abs().max(1.0)
}

/// The byte where the shown text ends (a typewriter's `reveal` characters).
fn revealed_end(text: &str, reveal: Option<usize>) -> usize {
    match reveal {
        None => text.len(),
        Some(n) => text.char_indices().nth(n).map_or(text.len(), |(at, _)| at),
    }
}

/// The part of a unit's range that shows.
fn shown(range: &std::ops::Range<usize>, end: usize) -> std::ops::Range<usize> {
    range.start..range.end.min(end).max(range.start)
}

/// Make the face each text of `items` draws in (its file read and evaluated)
/// and lay the text out at its size, as its first draw would: both are kept
/// for every later draw of the app. Returns the texts laid out.
pub(super) fn warm(md: &mut MotionDraw, cx: &mut Cx, items: &[Item]) -> usize {
    let mut laid = 0;
    for item in items {
        match &item.kind {
            ItemKind::Text { .. } => laid += layout(md, cx, item).is_some() as usize,
            ItemKind::Group { children, mask, .. } => {
                laid += warm(md, cx, children);
                if let Some(mask) = mask {
                    laid += warm(md, cx, std::slice::from_ref(mask.as_ref()));
                }
            }
            ItemKind::Fly { inner, .. } | ItemKind::Anchored { inner, .. } => laid += warm(md, cx, std::slice::from_ref(inner.as_ref())),
            _ => {}
        }
    }
    laid
}

/// Rasterise the units that turn, stretch or blur.
pub(super) fn prepare(md: &mut MotionDraw, cx: &mut Cx2d, item: &Item) {
    let Some(layout) = layout(md, cx, item) else { return };
    let ItemKind::Text { text, font, weight, italic, axes, units, reveal, .. } = &item.kind else { return };
    let size = &layout.font_size;
    let end = revealed_end(text, *reveal);
    let maps = unit_maps(md, item, &layout);
    for (k, ((unit, b), m)) in units.iter().zip(&layout.units).zip(&maps).enumerate() {
        let range = shown(&unit.range, end);
        if (direct(m) && unit.blur <= 0.0) || unit.opacity <= 0.0 || b.w <= 0.0 || range.is_empty() || m.mean_scale() <= 0.0 {
            continue;
        }
        let s = m.mean_scale().max(1e-6);
        let pad = size * PAD_EMS + unit.blur * 3.0 + item.feather * 1.5;
        let (content, px) = unit_target_px(b, pad, s);
        let index = md.target(cx, px, "motion_text");
        md.pool[index].begin(cx, vec4(0.0, 0.0, 0.0, 0.0));
        let axes = if unit.axes.is_empty() { axes.as_slice() } else { unit.axes.as_slice() };
        // In the unit's colour at full alpha; the quad applies its alpha.
        let color = [unit.color[0], unit.color[1], unit.color[2], 1.0];
        if item.feather > 0.0 {
            let placed = Affine::scale(s, s).then(&Affine::translate(pad, pad));
            outline_unit(md, cx, &layout, item, axes, &range, b, &placed, color, None, 1.0);
            md.flush_vector(cx);
        } else {
            let glyph = glyph_px(*size, s);
            // (the face loaded: the layout drew with it)
            set_face(md, cx, font, *weight, *italic, axes, glyph);
            note_glyphs(md, font, *weight, *italic, axes, glyph, &text[range.clone()]);
            let lc = md.lin(color);
            md.draws.text.color = vec4(lc[0] as f32, lc[1] as f32, lc[2] as f32, 1.0);
            for (piece, at) in layout.pieces(text, &range, b) {
                md.draws.text.draw_abs(cx, dvec2((pad + at[0]) * s, (pad + at[1]) * s), &text[piece]);
            }
        }
        md.pool[index].end(cx);
        let id = md.pool[index].pass.draw_pass_id();
        md.record(id);
        let texture = md.pool[index].texture.clone();
        let texture = if unit.blur > 0.0 { md.blurred(cx, &texture, px, unit.blur * s) } else { texture };
        let _ = content;
        md.prepared.insert((item.path.clone(), k + 1), texture);
    }
}

/// A blurred or transformed text unit's pixels at scale `s` (its box plus
/// `pad` each side), and the target it draws into: each side rounded up to
/// a step of an eighth of its power of two (16 px at least), so a unit whose
/// size animates reuses its pooled target instead of allocating a new one
/// every frame. The unit fills the target's top left; its quad samples that.
fn unit_target_px(b: &UnitBox, pad: f64, s: f64) -> ((u32, u32), (u32, u32)) {
    let content = (((b.w + 2.0 * pad) * s).ceil().max(1.0) as u32, ((b.h + 2.0 * pad) * s).ceil().max(1.0) as u32);
    let step = |v: u32| {
        let q = (v.next_power_of_two() / 8).max(16);
        v.div_ceil(q) * q
    };
    (content, (step(content.0), step(content.1)))
}

/// A filled box `[x, y, w, h]` of the coordinates `m` maps to pixels.
#[allow(clippy::too_many_arguments)]
fn box_under(md: &mut MotionDraw, cx: &mut Cx2d, m: &Affine, rect: [f64; 4], radius: f64, fill: [f64; 4], stroke: Option<[f64; 4]>, width: f64, opacity: f64) {
    let [x, y, w, h] = rect;
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let path = geometry::transformed(&geometry::rect_path([w, h], radius), &Affine::translate(x, y));
    let item = Item {
        path: String::new(),
        world: Affine::IDENTITY,
        local: makepad_motion::frame::Local { anchor: [0.0, 0.0], scale: [1.0, 1.0], rotate: 0.0 },
        opacity,
        blur: 0.0,
        feather: 0.0,
        feather_side: Default::default(),
        effects: Vec::new(),
        blend: makepad_motion::model::Blend::Normal,
        is_static: false,
        kind: ItemKind::Group { children: Vec::new(), mask: None, mask_mode: Default::default(), size: None },
    };
    // `vector` maps through `base` and the item: here `m` is the whole map.
    let saved = md.base;
    md.base = *m;
    md.vector(cx, &item, &path, [w, h], Some(fill), stroke, width, Cap::Butt, Join::Round, opacity);
    md.base = saved;
    // The glyphs draw next, over it: the shape session ends here, or the
    // plate (drawn when the session flushes) would land on top of the text.
    md.flush_vector(cx);
}

/// A plate hugging `[x, y, w, h]` (padded), at least as wide as it is tall,
/// wiped open from the left to `reveal`.
fn plate(md: &mut MotionDraw, cx: &mut Cx2d, m: &Affine, rect: [f64; 4], plate: &PlateFrame, opacity: f64) {
    if plate.underline {
        // A band under the line, wiping open left to right.
        let band = (rect[3] * 0.08).max(2.0);
        let w = (rect[2] + 2.0 * plate.pad[0]) * plate.reveal;
        box_under(md, cx, m, [rect[0] - plate.pad[0], rect[1] + rect[3] * 0.9, w, band], plate.radius.min(band * 0.5), plate.color, plate.stroke, plate.width, opacity);
        return;
    }
    let (w, h) = (rect[2] + 2.0 * plate.pad[0], rect[3] + 2.0 * plate.pad[1]);
    let w = w.max(h) * plate.reveal;
    box_under(md, cx, m, [rect[0] - plate.pad[0], rect[1] - plate.pad[1], w, h], plate.radius, plate.color, plate.stroke, plate.width, opacity);
}

pub(super) fn draw(md: &mut MotionDraw, cx: &mut Cx2d, item: &Item, opacity: f64) {
    let Some(layout) = layout(md, cx, item) else { return };
    let ItemKind::Text { text, font, weight, italic, axes, units, plate: plate_frame, reveal, caret, outline, .. } = &item.kind else { return };
    let size = &layout.font_size;
    let end = revealed_end(text, *reveal);
    let maps = unit_maps(md, item, &layout);
    let text_map = text_box(md, item, &layout);
    let mut outlines = Vec::new();
    // Plates under the glyphs: per line, or per unit (moving with it).
    if let Some(p) = plate_frame {
        if p.each {
            for ((unit, b), m) in units.iter().zip(&layout.units).zip(&maps) {
                if b.w > 0.0 && unit.opacity > 0.0 {
                    plate(md, cx, m, [0.0, 0.0, b.w, b.h], p, opacity * unit.opacity);
                }
            }
        } else {
            for row in layout.rows.clone() {
                plate(md, cx, &text_map, [row[0], row[2], row[1] - row[0], row[3]], p, opacity);
            }
        }
    }
    for (k, ((unit, b), m)) in units.iter().zip(&layout.units).zip(&maps).enumerate() {
        let alpha = unit.color[3] * unit.opacity * opacity;
        let range = shown(&unit.range, end);
        if b.w <= 0.0 || range.is_empty() || m.mean_scale() <= 0.0 {
            continue;
        }
        // `stroke`: the unit's outlines, over its fill (or alone, hollow).
        if let Some((color, width)) = outline {
            outlines.push((k, range.clone(), b.clone(), *m, *color, *width, unit.opacity * opacity));
        }
        if alpha <= 0.0 {
            continue;
        }
        if direct(m) && unit.blur <= 0.0 {
            let axes = if unit.axes.is_empty() { axes.as_slice() } else { unit.axes.as_slice() };
            if item.feather > 0.0 {
                outline_unit(md, cx, &layout, item, axes, &range, b, m, unit.color, None, unit.opacity * opacity);
                continue;
            }
            let px = glyph_px(*size, m.a);
            set_face(md, cx, font, *weight, *italic, axes, px);
            note_glyphs(md, font, *weight, *italic, axes, px, &text[range.clone()]);
            let lc = md.lin([unit.color[0], unit.color[1], unit.color[2], 1.0]);
            md.draws.text.color = vec4(lc[0] as f32, lc[1] as f32, lc[2] as f32, alpha as f32);
            for (piece, at) in layout.pieces(text, &range, b) {
                let p = m.apply(at);
                md.draws.text.draw_abs(cx, dvec2(p[0], p[1]), &text[piece]);
            }
        } else if let Some(texture) = md.prepared.get(&(item.path.clone(), k + 1)).cloned() {
            // Earlier units' filled outlines must land below this quad.
            md.flush_vector(cx);
            let pad = size * PAD_EMS + unit.blur * 3.0 + item.feather * 1.5;
            // The unit fills the top left of its (size-stepped) target.
            let (content, px) = unit_target_px(b, pad, m.mean_scale().max(1e-6));
            let uv = [0.0, 0.0, content.0 as f64 / px.0 as f64, content.1 as f64 / px.1 as f64];
            let quad = m.then(&Affine::translate(-pad, -pad));
            md.draws.quad.quad(cx, &texture, None, &quad, [b.w + 2.0 * pad, b.h + 2.0 * pad], uv, alpha);
        }
    }
    md.flush_vector(cx);
    for (k, range, b, m, color, width, opacity) in outlines {
        let axes = if units[k].axes.is_empty() { axes.as_slice() } else { units[k].axes.as_slice() };
        outline_unit(md, cx, &layout, item, axes, &range, &b, &m, color, Some(width), opacity);
    }
    // The caret, after the last shown character.
    if let Some((caret_opacity, color, block)) = caret {
        let alpha = caret_opacity * opacity;
        if alpha > 0.0 {
            let (x, row) = layout.ends.iter().filter(|e| e.0 <= end).last().map(|e| (e.1, e.2)).unwrap_or((layout.rows.first().map_or(0.0, |r| r[0]), 0));
            let (top, h) = layout.rows.get(row).map_or((0.0, *size * 1.2), |r| (r[2], r[3]));
            // A bar, or a block the width of a letter.
            let w = if *block { size * 0.55 } else { (size * 0.07).max(1.0) };
            box_under(md, cx, &text_map, [x + size * 0.04, top + h * 0.1, w, h * 0.8], 0.0, *color, None, 0.0, alpha);
        }
    }
}

/// A unit's glyph outlines, filled for feathered text or stroked `width`
/// wide in document units: traced at the layout's size and placed where
/// the piece's glyphs stand. Both use the item's edge coverage.
#[allow(clippy::too_many_arguments)]
fn outline_unit(md: &mut MotionDraw, cx: &mut Cx2d, layout: &TextLayout, item: &Item, axes: &[(String, f64)], range: &std::ops::Range<usize>, b: &UnitBox, m: &Affine, color: [f64; 4], width: Option<f64>, opacity: f64) {
    let ItemKind::Text { text, font, weight, italic, .. } = &item.kind else { return };
    if width.is_some_and(|width| width <= 0.0) || color[3] * opacity <= 0.0 {
        return;
    }
    md.note_kind("vector");
    let bare = Item {
        path: String::new(),
        world: Affine::IDENTITY,
        local: makepad_motion::frame::Local { anchor: [0.0, 0.0], scale: [1.0, 1.0], rotate: 0.0 },
        opacity,
        blur: 0.0,
        feather: item.feather,
        feather_side: item.feather_side,
        effects: Vec::new(),
        blend: makepad_motion::model::Blend::Normal,
        is_static: false,
        kind: ItemKind::Group { children: Vec::new(), mask: None, mask_mode: Default::default(), size: None },
    };
    // The layout's ascender carries the face's adjustment; the outlines
    // hang from the font's own.
    let Some(face) = face(cx, font, *weight, *italic) else { return };
    let lift = face.font_family.ascender_fudge_in_ems() as f64 * layout.font_size * 4.0 / 3.0;
    let saved = md.base;
    for (piece, at) in layout.pieces(text, range, b) {
        let Some(outline) = makepad_motion::text_outline_contours(&text[piece], font, layout.font_size, *weight, *italic, axes) else { continue };
        let mut path = geometry::polygons_path(&outline);
        // Glyph contours overlap in variable fonts: non-zero.
        path.set_fill_rule(makepad_widgets::makepad_draw::vector::FillRule::NonZero);
        // `vector` maps through `base` and the item: here the unit's map
        // moved to the piece is the whole map.
        md.base = m.then(&Affine::translate(at[0], at[1] + lift));
        let (fill, stroke) = if width.is_some() { (None, Some(color)) } else { (Some(color), None) };
        md.vector(cx, &bare, &path, [1.0, 1.0], fill, stroke, width.unwrap_or(0.0), Cap::Round, Join::Round, opacity);
    }
    md.base = saved;
    // Filled units batch until a texture unit, an outline or the end of
    // the text. A stroked unit keeps its place above the text's fills.
    if width.is_some() {
        md.flush_vector(cx);
    }
}

/// A four-letter axis tag (`wdth`, `GRAD`) as the font tables key it.
fn axis_tag(tag: &str) -> Option<u32> {
    let b = tag.as_bytes();
    if b.is_empty() || b.len() > 4 {
        return None;
    }
    let mut t = [b' '; 4];
    t[..b.len()].copy_from_slice(b);
    Some(u32::from_be_bytes(t))
}
