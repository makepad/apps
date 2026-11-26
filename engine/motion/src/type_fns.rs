//! Typography functions for documents (PDOOM-PARITY F2–F5), from
//! `makepad-typography`: measure and place type, trace its outlines, fit it,
//! and write it with a single-stroke pen, all in document units with y down,
//! x 0 at the text's start and y 0 at the font's ascender line. `size` is a
//! `Text`'s size (points: the em is `size * 4 / 3` document units), so a
//! function and a `Text` of the same font and size agree.
//!
//! ```text
//! let GROT = Font{family: "Roboto Flex" weight: 900 axes: {wdth: 125}}
//! text_layout("HARBOR", GROT, 160)                                // width, height, cap/x-height, glyphs [{x y w ink char word line}]
//! glyph_x("OUR HARBOR", 4, INTER, 96)                             // where char 4 starts (split runs line up)
//! text_path("LIGHTS", INTER, 200)                                 // the outlines as path data: Path{svg: … trim_end: …} traces them
//! text_outline("O", INTER, 200)                                   // [{glyph hole points: [[x y] …]}]
//! fit_text("THE LONG LINE", Font{family: "Anton"}, 400, 1200, 300) // {size lines: [{text size x y width}]}
//! stroke_path("open the loop", 48)                                // single-stroke lettering: Path{svg: … trim_end: t} writes it on
//! stroke_head("open the loop", 0.4, 48)                           // the pen at 40 % written: [x y angle] (an anchor for a spark)
//! text_points("RISK", Font{family: "Archivo" weight: 900}, 112.5, 6.2, 7) // [x, y, …]: points jittered over the ink (left edge, baseline; y down)
//! stroke_text("r = 0.08", 75, Font{family: "ems_tech"}, @holes)   // {strokes: [[x0, y0, x1, y1, …], …] chars width}: y down, baseline 0; chars[i]: stroke i's character
//! ```
//!
//! `font` is a `Font{family weight italic axes}` value: a document font
//! (`makepad_doc_fonts::FAMILIES`), its weight (a variable font's `wght`, or a static font's
//! file), italic, and its axes `{wdth: 125 …}`; Inter 400 when left out.
//! Stroke fonts: `"technical"` (Makepad Technical, bundled; spaced optically)
//! and `"ems_tech"` (EMS Tech, SIL OFL; `kerning: @holes`). `kerning`:
//! `@optical` (the default), `@plain` (the advances), `@holes` (the advances,
//! kerned only where a pair leaves a hole).

use makepad_script::*;
use makepad_typography::fit::{fit, Fit, Measure};
use makepad_typography::font::OutlineFont;
use makepad_typography::layout::{layout, TextLayout, TextStyle};
use makepad_typography::outline::{is_hole, outlines};
use makepad_typography::stroke::{stroke_layout, write_on, StrokeFont, StrokeKerning, StrokeStyle};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

/// The file a document font reads from, by name, weight and italic (an
/// installed external family's face, or a bundled font's file).
fn font_file(name: &str, weight: f64, italic: bool) -> Option<PathBuf> {
    makepad_doc_fonts::font_file(name, weight, italic)
}

/// A document font at a weight and axis values (bytes read once).
pub(crate) fn font(name: &str, weight: f64, italic: bool, axes: &[(String, f64)]) -> Result<OutlineFont, String> {
    static BYTES: OnceLock<Mutex<HashMap<PathBuf, OutlineFont>>> = OnceLock::new();
    let path = font_file(name, weight, italic).ok_or_else(|| format!("`{name}` is not a document font ({})", makepad_doc_fonts::bundled().collect::<Vec<_>>().join(", ")))?;
    let bytes = BYTES.get_or_init(Default::default);
    let known = bytes.lock().unwrap_or_else(|e| e.into_inner()).get(&path).cloned();
    let base = match known {
        Some(f) => f,
        None => {
            // Read without the lock: in the browser a worker's read waits for
            // the page to fetch the file, and the page may want this lock.
            let f = OutlineFont::new(makepad_doc_fonts::read_font(&path)?, 0).map_err(|e| format!("{}: {e}", path.display()))?;
            bytes.lock().unwrap_or_else(|e| e.into_inner()).entry(path).or_insert(f).clone()
        }
    };
    let mut list: Vec<(&str, f32)> = vec![("wght", weight as f32)];
    list.extend(axes.iter().map(|(t, v)| (t.as_str(), *v as f32)));
    Ok(base.with_axes(&list))
}

/// The font `text` is measured in: the named one, or for text with Chinese
/// or Japanese characters the CJK face (LXGW WenKai, bold from 600), as the
/// drawing side falls back to it for those characters.
pub(crate) fn font_for(name: &str, weight: f64, italic: bool, axes: &[(String, f64)], text: &str) -> Result<OutlineFont, String> {
    if crate::typography::has_cjk(text) {
        if let Ok(f) = font("LXGW WenKai", if weight >= 600.0 { 700.0 } else { 400.0 }, false, &[]) {
            return Ok(f);
        }
    }
    font(name, weight, italic, axes)
}

/// Read range defaults once at load, through the same cached font as text
/// measurement. Evaluation never opens or parses a font.
pub(crate) fn font_axes(name: &str, weight: f64, italic: bool) -> Result<Vec<makepad_typography::font::Axis>, String> {
    Ok(font(name, weight, italic, &[])?.axes())
}

fn stroke_font(name: &str) -> Result<Arc<StrokeFont>, String> {
    static FONTS: OnceLock<Mutex<HashMap<String, Arc<StrokeFont>>>> = OnceLock::new();
    let mut map = FONTS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    if let Some(f) = map.get(name) {
        return Ok(f.clone());
    }
    let f = Arc::new(StrokeFont::bundled(name).ok_or_else(|| format!("`{name}` is not a stroke font (bundled: technical, ems_tech)"))?);
    map.insert(name.to_string(), f.clone());
    Ok(f)
}

// ---- arguments ------------------------------------------------------------

fn num(vm: &ScriptVm, args: ScriptObject, key: LiveId, default: f64) -> f64 {
    vm.bx.heap_ref().value(args, key.into(), NoTrap).as_number().filter(|n| n.is_finite()).unwrap_or(default)
}

fn text(vm: &ScriptVm, args: ScriptObject, key: LiveId) -> Option<String> {
    let value = vm.bx.heap_ref().value(args, key.into(), NoTrap);
    // Heap strings and short inline ones alike.
    if let Some(s) = vm.bx.heap_ref().string_with(value, |_, s| s.to_string()) {
        return Some(s);
    }
    if let Some(id) = value.as_id() {
        return id.as_string(|s| s.map(str::to_string));
    }
    value.as_number().map(|n| crate::value::fmt_num(n))
}

fn fail(vm: &mut ScriptVm, message: String) -> ScriptValue {
    script_err_invalid_args!(vm.trap(), "{message}")
}

fn new_obj(vm: &mut ScriptVm, fields: &[(LiveId, ScriptValue)]) -> ScriptValue {
    let o = vm.bx.heap_mut().new_object();
    for (k, v) in fields {
        vm.bx.heap_mut().set_value_def(o, (*k).into(), *v);
    }
    o.into()
}

fn nums(vm: &mut ScriptVm, values: &[f64]) -> ScriptValue {
    let a = vm.bx.heap_mut().new_array();
    for v in values {
        vm.bx.heap_mut().array_push(a, (*v).into(), NoTrap);
    }
    a.into()
}

fn string(vm: &mut ScriptVm, s: &str) -> ScriptValue {
    vm.new_string_with(|_, out| out.push_str(s))
}

/// The face `args.font` names (a `Font{…}`; Inter when left out).
fn font_arg(vm: &ScriptVm, args: ScriptObject, text: &str) -> Result<OutlineFont, String> {
    let v = vm.bx.heap_ref().value(args, id!(font).into(), NoTrap);
    let spec = if v.is_nil() {
        crate::font_spec::FontSpec { family: crate::font_spec::font_or_error("Inter")?, weight: 400.0, italic: false, axes: Vec::new() }
    } else {
        crate::font_spec::font_spec(vm, v)?
    };
    font_for(spec.family, spec.weight, spec.italic, &spec.axes, text)
}

/// The stroke font `args.font` names (a `Font{family: "ems_tech"}`; the
/// technical face when left out).
fn stroke_font_arg(vm: &ScriptVm, args: ScriptObject) -> Result<Arc<StrokeFont>, String> {
    let v = vm.bx.heap_ref().value(args, id!(font).into(), NoTrap);
    if v.is_nil() {
        return stroke_font("technical");
    }
    if let Some(name) = vm.bx.heap_ref().string_with(v, |_, s| s.to_string()) {
        return Err(format!("`font` is a Font value: write `font: Font{{family: \"{name}\"}}`"));
    }
    // Outside a Motion document's VM (a 3D scene running the document's top-level code) a Font is a plain record.
    let motion_vm = vm.host.get::<crate::value::DocHost>().is_some();
    match v
        .as_object()
        .filter(|o| !motion_vm || crate::layers::type_of(vm, *o) == Some("Font"))
    {
        Some(o) => {
            let family = vm.bx.heap_ref().value(o, id!(family).into(), NoTrap);
            stroke_font(&vm.bx.heap_ref().string_with(family, |_, s| s.to_string()).unwrap_or_else(|| "technical".into()))
        }
        None => Err("`font` is a Font value: `Font{family: \"ems_tech\"}`".into()),
    }
}

/// The laid-out text for `args` (`text`, `font`, `size`, `tracking` in ems),
/// and the y of its box top (the first ascender).
fn laid(vm: &ScriptVm, args: ScriptObject) -> Result<(TextLayout, OutlineFont), String> {
    let t = text(vm, args, id!(text)).unwrap_or_default();
    let f = font_arg(vm, args, &t)?;
    let style = TextStyle { size: (num(vm, args, id!(size), 72.0).max(0.01) * PX_PER_PT) as f32, tracking: num(vm, args, id!(tracking), 0.0) as f32, ..TextStyle::default() };
    Ok((layout(&f, &t, &style), f))
}

/// Points scattered over a text's ink: a grid `step` px apart over the text's box (padded by 0.3 of the
/// em, 1.6 em tall, the baseline at 1.15 em), each point jittered by up to 0.4 step (an LCG from `seed`),
/// kept where the pixel it falls in is inside the letters (nonzero winding, as a canvas fills them).
/// Flat [x, y, …] from the text's left edge and baseline (y down), in grid order.
fn text_points(contours: &[makepad_typography::outline::Contour], width: f64, px: f64, step: f64, s: &mut u32) -> Vec<f64> {
    let boxes: Vec<[f32; 4]> = contours
        .iter()
        .map(|c| c.points.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, p| [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])]))
        .collect();
    let winding = |x: f32, y: f32| -> i32 {
        let mut w = 0;
        for (c, b) in contours.iter().zip(&boxes) {
            if x < b[0] || x > b[2] || y < b[1] || y > b[3] {
                continue;
            }
            let n = c.points.len();
            for i in 0..n {
                let a = c.points[i];
                let b = c.points[(i + 1) % n];
                if a[1] <= y {
                    if b[1] > y && (b[0] - a[0]) * (y - a[1]) - (x - a[0]) * (b[1] - a[1]) > 0.0 {
                        w += 1;
                    }
                } else if b[1] <= y && (b[0] - a[0]) * (y - a[1]) - (x - a[0]) * (b[1] - a[1]) < 0.0 {
                    w -= 1;
                }
            }
        }
        w
    };
    let pad = (px * 0.3).ceil();
    let w = (width + pad * 2.0).ceil();
    let h = (px * 1.6).ceil();
    let base = (px * 1.15).round();
    let mut rnd = || {
        *s = s.wrapping_mul(1664525).wrapping_add(1013904223);
        *s as f64 / 4294967296.0
    };
    let mut out = Vec::new();
    let mut y = 0.0;
    while y < h {
        let mut x = 0.0;
        while x < w {
            let jx = x + (rnd() - 0.5) * step * 0.8;
            let jy = y + (rnd() - 0.5) * step * 0.8;
            let ix = jx.round().clamp(0.0, w - 1.0);
            let iy = jy.round().clamp(0.0, h - 1.0);
            // the pixel's centre, in the font's frame (y up, baseline 0)
            if winding((ix + 0.5 - pad) as f32, (base - iy - 0.5) as f32) != 0 {
                out.push(jx - pad);
                out.push(jy - base);
            }
            x += step;
        }
        y += step;
    }
    out
}

/// Font y (up, baseline 0) to document y (down, box top 0).
fn down(l: &TextLayout, y: f32) -> f64 {
    (l.metrics.ascender - y) as f64
}

fn path_data(contours: impl Iterator<Item = Vec<[f64; 2]>>, closed: bool) -> String {
    let mut s = String::new();
    for c in contours {
        for (i, p) in c.iter().enumerate() {
            s.push_str(&format!("{}{} {} ", if i == 0 { "M" } else { "L" }, crate::value::fmt_num((p[0] * 100.0).round() / 100.0), crate::value::fmt_num((p[1] * 100.0).round() / 100.0)));
        }
        if closed && !c.is_empty() {
            s.push_str("Z ");
        }
    }
    s.trim_end().to_string()
}

/// The outlines of `text` in a document font at `size` (a `Text`'s size,
/// points) as closed contours in document units, x 0 at the text's start and
/// y down from the font's ascender line: where a `Text` draws the same text
/// (what `Text{stroke}` strokes). `None` when `font` is not a document
/// font. Made once per text and face (a few hundred kept).
pub fn text_outline_contours(text: &str, font_name: &str, size: f64, weight: f64, italic: bool, axes: &[(String, f64)]) -> Option<Arc<Vec<Vec<[f64; 2]>>>> {
    static MADE: OnceLock<Mutex<HashMap<String, Option<Arc<Vec<Vec<[f64; 2]>>>>>>> = OnceLock::new();
    let key = format!("{font_name}|{weight}|{italic}|{axes:?}|{size}|{text}");
    let mut made = MADE.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    if let Some(data) = made.get(&key) {
        return data.clone();
    }
    let data = font(font_name, weight, italic, axes).ok().map(|f| {
        let l = layout(&f, text, &TextStyle { size: (size.max(0.01) * PX_PER_PT) as f32, ..TextStyle::default() });
        Arc::new(outlines(&f, &l, 0.25).into_iter().map(|c| c.points.iter().map(|p| [p[0] as f64, down(&l, p[1])]).collect()).collect())
    });
    if made.len() >= 512 {
        made.clear();
    }
    made.insert(key, data.clone());
    data
}

/// One glyph of a laid-out text: its character, its word and line ordinals
/// and its ink as closed polygons (document units, y down, x 0 at the text's
/// start and y 0 at the top of the first line's ascender).
pub(crate) struct GlyphInk {
    pub ch: char,
    pub word: Option<usize>,
    pub line: usize,
    pub contours: Vec<Vec<[f64; 2]>>,
}

/// A laid-out text with every glyph's ink.
pub(crate) struct InkLayout {
    pub glyphs: Vec<GlyphInk>,
    /// Width of each line (advances plus tracking), document units.
    pub line_widths: Vec<f64>,
    /// From the first line's ascender to the last line's descender.
    pub height: f64,
}

/// The glyphs of `text` in `face` at `size` points (lines `line_spacing`
/// times the font's line height apart), each with its ink.
pub(crate) fn glyph_inks(text: &str, face: &crate::font_spec::FontSpec, size: f64, tracking: f64, line_spacing: f64) -> Result<InkLayout, String> {
    let f = font_for(face.family, face.weight, face.italic, &face.axes, text)?;
    let l = layout(&f, text, &TextStyle { size: (size.max(0.01) * PX_PER_PT) as f32, tracking: tracking as f32, line_spacing: line_spacing as f32, ..TextStyle::default() });
    let chars: Vec<char> = text.chars().collect();
    let mut glyphs: Vec<GlyphInk> = l.glyphs.iter().map(|g| GlyphInk { ch: chars.get(g.char_index).copied().unwrap_or(' '), word: g.word, line: g.line, contours: Vec::new() }).collect();
    for c in outlines(&f, &l, 0.25) {
        if let Some(glyph) = glyphs.get_mut(c.glyph) {
            glyph.contours.push(c.points.iter().map(|p| [p[0] as f64, down(&l, p[1])]).collect());
        }
    }
    Ok(InkLayout { glyphs, line_widths: l.line_widths.iter().map(|w| *w as f64).collect(), height: l.height() as f64 })
}

/// The advance width of `text` in a document font at `px` (the em in
/// document units, as a Canvas2D font size): what `Draw2D`'s `c.measure`
/// gives.
pub fn text_width(text: &str, font_name: &str, px: f64, weight: f64, italic: bool, axes: &[(String, f64)]) -> Result<f64, String> {
    let f = font_for(font_name, weight, italic, axes, text)?;
    Ok(layout(&f, text, &TextStyle { size: px.max(0.01) as f32, ..TextStyle::default() }).width() as f64)
}

struct DocMeasure {
    font: OutlineFont,
    tracking: f32,
}

impl Measure for DocMeasure {
    fn width(&self, t: &str) -> f32 {
        layout(&self.font, t, &TextStyle { size: 1.0, tracking: self.tracking, ..TextStyle::default() }).width()
    }
    fn line_height(&self) -> f32 {
        self.font.metrics().scaled(1.0).line_height()
    }
}

/// Document units per point of `size` (a `Text`'s size is in points).
const PX_PER_PT: f64 = 4.0 / 3.0;

fn stroke_style(vm: &ScriptVm, args: ScriptObject) -> StrokeStyle {
    let kerning = match vm.bx.heap_ref().value(args, id!(kerning).into(), NoTrap).as_id() {
        Some(k) if k == id!(plain) => StrokeKerning::Plain,
        Some(k) if k == id!(holes) => StrokeKerning::Holes,
        _ => StrokeKerning::Optical,
    };
    StrokeStyle { size: (num(vm, args, id!(size), 72.0).max(0.01) * PX_PER_PT) as f32, spacing: num(vm, args, id!(spacing), 0.12) as f32, word_space: num(vm, args, id!(word_space), 1.0) as f32, kerning }
}

/// Stroke-text layout: font y (up) to document y (down, the cap line at 0).
fn stroke_down(f: &StrokeFont, size: f32, y: f32) -> f64 {
    (f.cap_height * size / f.em - y) as f64
}

/// Adds the typography functions to the document VM's `mod.motion`.
pub(crate) fn install(vm: &mut ScriptVm, module: ScriptObject) {
    vm.add_method(module, id_lut!(text_layout), script_args!(text = NIL, font = NIL, size = 72.0, tracking = 0.0), |vm, args| {
        let (l, _) = match laid(vm, args) {
            Ok(v) => v,
            Err(e) => return fail(vm, e),
        };
        let glyphs = vm.bx.heap_mut().new_array();
        for g in &l.glyphs {
            let ink = if g.has_ink() { [g.ink[0] as f64, down(&l, g.ink[3]), g.ink[2] as f64, down(&l, g.ink[1])] } else { [g.x as f64, down(&l, g.y), g.x as f64, down(&l, g.y)] };
            let ink = nums(vm, &ink);
            let o = new_obj(vm, &[
                (id_lut!(x), (g.x as f64).into()),
                (id_lut!(y), down(&l, g.y).into()),
                (id_lut!(w), (g.advance as f64).into()),
                (id_lut!(ink), ink),
                (id_lut!(char), (g.char_index as f64).into()),
                (id_lut!(word), g.word.map_or(-1.0, |w| w as f64).into()),
                (id_lut!(line), (g.line as f64).into()),
            ]);
            vm.bx.heap_mut().array_push(glyphs, o, NoTrap);
        }
        let m = l.metrics;
        new_obj(vm, &[
            (id_lut!(width), (l.width() as f64).into()),
            (id_lut!(height), (l.height() as f64).into()),
            (id_lut!(ascent), (m.ascender as f64).into()),
            (id_lut!(descent), (-m.descender as f64).into()),
            (id_lut!(cap_height), (m.cap_height as f64).into()),
            (id_lut!(x_height), (m.x_height as f64).into()),
            (id_lut!(line_height), (l.line_height as f64).into()),
            (id_lut!(baseline), (m.ascender as f64).into()),
            (id_lut!(glyphs), glyphs.into()),
        ])
    });
    vm.add_method(module, id_lut!(glyph_x), script_args!(text = NIL, i = 0.0, font = NIL, size = 72.0, tracking = 0.0), |vm, args| {
        let i = num(vm, args, id!(i), 0.0).max(0.0) as usize;
        match laid(vm, args) {
            Ok((l, _)) => (l.glyph_x(i) as f64).into(),
            Err(e) => fail(vm, e),
        }
    });
    vm.add_method(module, id_lut!(text_outline), script_args!(text = NIL, font = NIL, size = 72.0, tracking = 0.0, tolerance = 0.5), |vm, args| {
        let tol = num(vm, args, id!(tolerance), 0.5).max(0.01) as f32;
        let (l, f) = match laid(vm, args) {
            Ok(v) => v,
            Err(e) => return fail(vm, e),
        };
        let cs = outlines(&f, &l, tol);
        let out = vm.bx.heap_mut().new_array();
        for (k, c) in cs.iter().enumerate() {
            let pts = vm.bx.heap_mut().new_array();
            for p in &c.points {
                let pt = nums(vm, &[p[0] as f64, down(&l, p[1])]);
                vm.bx.heap_mut().array_push(pts, pt, NoTrap);
            }
            let o = new_obj(vm, &[(id_lut!(glyph), (c.glyph as f64).into()), (id_lut!(hole), is_hole(&cs, k).into()), (id_lut!(points), pts.into())]);
            vm.bx.heap_mut().array_push(out, o, NoTrap);
        }
        out.into()
    });
    vm.add_method(module, id_lut!(text_points), script_args!(text = NIL, font = NIL, size = 72.0, step = 6.0, seed = 1.0), |vm, args| {
        let (l, f) = match laid(vm, args) {
            Ok(v) => v,
            Err(e) => return fail(vm, e),
        };
        let px = num(vm, args, id!(size), 72.0).max(0.01) * PX_PER_PT;
        let step = num(vm, args, id!(step), 6.0).max(0.5);
        let mut s = num(vm, args, id!(seed), 1.0) as u32;
        let flat = text_points(&outlines(&f, &l, 0.25), l.width() as f64, px, step, &mut s);
        nums(vm, &flat)
    });
    vm.add_method(module, id_lut!(text_path), script_args!(text = NIL, font = NIL, size = 72.0, tracking = 0.0, tolerance = 0.5), |vm, args| {
        let tol = num(vm, args, id!(tolerance), 0.5).max(0.01) as f32;
        let (l, f) = match laid(vm, args) {
            Ok(v) => v,
            Err(e) => return fail(vm, e),
        };
        let data = path_data(outlines(&f, &l, tol).into_iter().map(|c| c.points.iter().map(|p| [p[0] as f64, down(&l, p[1])]).collect()), true);
        string(vm, &data)
    });
    vm.add_method(module, id_lut!(fit_text), script_args!(text = NIL, font = NIL, width = 1000.0, height = NIL, lines = NIL, tracking = 0.0, max_size = 0.0), |vm, args| {
        let t = text(vm, args, id!(text)).unwrap_or_default();
        let f = match font_arg(vm, args, &t) {
            Ok(f) => f,
            Err(e) => return fail(vm, e),
        };
        let m = DocMeasure { font: f, tracking: num(vm, args, id!(tracking), 0.0) as f32 };
        let width = num(vm, args, id!(width), 1000.0) as f32;
        let height = vm.bx.heap_ref().value(args, id!(height).into(), NoTrap).as_number();
        let lines = vm.bx.heap_ref().value(args, id!(lines).into(), NoTrap).as_number();
        // `lines`: a justified column of that many lines; `height`: the
        // largest size wrapped into the area; neither: one line filling
        // the width.
        let how = match (lines, height) {
            (Some(n), _) => Fit::Column { width, lines: n.max(1.0) as usize },
            (None, Some(h)) => Fit::Area { width, height: h as f32 },
            (None, None) => Fit::Width(width),
        };
        let r = fit(&m, &t, how, (num(vm, args, id!(max_size), 0.0) * PX_PER_PT) as f32);
        let out = vm.bx.heap_mut().new_array();
        for line in &r.lines {
            let s = string(vm, &line.text);
            let o = new_obj(vm, &[(id_lut!(text), s), (id_lut!(size), (line.size as f64 / PX_PER_PT).into()), (id_lut!(x), (line.x as f64).into()), (id_lut!(y), (line.y as f64).into()), (id_lut!(width), (line.width as f64).into())]);
            vm.bx.heap_mut().array_push(out, o, NoTrap);
        }
        new_obj(vm, &[(id_lut!(size), (r.size as f64 / PX_PER_PT).into()), (id_lut!(width), (r.width as f64).into()), (id_lut!(height), (r.height as f64).into()), (id_lut!(lines), out.into())])
    });
    vm.add_method(module, id_lut!(stroke_text), script_args!(text = NIL, size = 72.0, font = NIL, kerning = NIL, spacing = 0.12, word_space = 1.0), |vm, args| {
        let f = match stroke_font_arg(vm, args) {
            Ok(f) => f,
            Err(e) => return fail(vm, e),
        };
        let style = stroke_style(vm, args);
        let l = stroke_layout(&f, &text(vm, args, id!(text)).unwrap_or_default(), &style);
        let strokes = vm.bx.heap_mut().new_array();
        for s in &l.strokes {
            let flat: Vec<f64> = s.points.iter().flat_map(|p| [p[0] as f64, -p[1] as f64]).collect();
            let a = nums(vm, &flat);
            vm.bx.heap_mut().array_push(strokes, a, NoTrap);
        }
        // which character of `text` each stroke draws (a pen that writes the text as it is sung)
        let chars: Vec<f64> = l.strokes.iter().map(|s| s.char_index as f64).collect();
        let chars = nums(vm, &chars);
        new_obj(vm, &[(id_lut!(strokes), strokes.into()), (id_lut!(chars), chars), (id_lut!(width), (l.width as f64).into())])
    });
    vm.add_method(module, id_lut!(stroke_path), script_args!(text = NIL, size = 72.0, font = NIL, kerning = NIL, spacing = 0.12, word_space = 1.0), |vm, args| {
        let f = match stroke_font_arg(vm, args) {
            Ok(f) => f,
            Err(e) => return fail(vm, e),
        };
        let style = stroke_style(vm, args);
        let l = stroke_layout(&f, &text(vm, args, id!(text)).unwrap_or_default(), &style);
        let data = path_data(l.strokes.iter().map(|s| s.points.iter().map(|p| [p[0] as f64, stroke_down(&f, style.size, p[1])]).collect()), false);
        string(vm, &data)
    });
    vm.add_method(module, id_lut!(stroke_head), script_args!(text = NIL, progress = 0.0, size = 72.0, font = NIL, kerning = NIL, spacing = 0.12, word_space = 1.0), |vm, args| {
        let f = match stroke_font_arg(vm, args) {
            Ok(f) => f,
            Err(e) => return fail(vm, e),
        };
        let style = stroke_style(vm, args);
        let l = stroke_layout(&f, &text(vm, args, id!(text)).unwrap_or_default(), &style);
        let p = num(vm, args, id!(progress), 0.0).clamp(0.0, 1.0) as f32;
        match write_on(&l, l.length * p).1 {
            Some(h) => nums(vm, &[h.pos[0] as f64, stroke_down(&f, style.size, h.pos[1]), (-h.dir[1]).atan2(h.dir[0]).to_degrees() as f64]),
            None => NIL,
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_fonts_resolve_and_axes_apply() {
        for name in makepad_doc_fonts::bundled() {
            let path = font_file(name, 400.0, false).unwrap_or_else(|| panic!("{name} has no file"));
            assert!(path.exists(), "{}", path.display());
        }
        let narrow = font("Roboto Flex", 900.0, false, &[("wdth".into(), 25.0)]).unwrap();
        let wide = font("Roboto Flex", 900.0, false, &[("wdth".into(), 151.0)]).unwrap();
        let w = |f: &OutlineFont| layout(f, "HARBOR", &TextStyle { size: 100.0, ..TextStyle::default() }).width();
        assert!(w(&wide) > w(&narrow));
        assert!(path_data(vec![vec![[0.0, 0.0], [1.5, 2.25]]].into_iter(), true) == "M0 0 L1.5 2.25 Z");
    }
}
