//! The 2D drawing primitives Motion frames and live hosts share (KERNELS.md
//! §3.11.1 rows 10–11, C2): shapes from their distance field, gauges, and
//! text runs measured, wrapped, fitted and drawn with a shadow or an outline.
//!
//! A Motion frame draws through them into its canvas; a live host (Sandbox's
//! HUD and screens) draws through them straight into its own draw list, with
//! its own draws and faces, every frame. They are drawing only: what a host
//! shows, when, and what input does to it stay the host's.
//!
//! Shapes are one [`DrawMotionShape`] instance each (rounded box, ellipse or
//! arc), so a HUD's plates, tracks, fills, pips and dials batch into one draw
//! call. Colours given to the host-side functions are straight alpha, as a
//! host writes them; [`sdf`] takes them premultiplied, as a frame has them.

use crate::{DrawMotionQuad, DrawMotionShape};
use makepad_motion::frame::Affine;
use makepad_typography::fit::{wrap as fit_wrap, Measure};
use makepad_widgets::makepad_draw::shader::draw_vector::FeatherSide;
use makepad_widgets::*;

/// What [`sdf`] draws in its box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    /// A box with its corners rounded by `radius` (clamped to the short side).
    Rect { radius: f64 },
    /// The ellipse the box holds.
    Ellipse,
    /// A ring of `thickness` inside the box's short side, from `from`
    /// radians (0 = +x, clockwise on screen) over `sweep * frac`, with flat
    /// caps. Nothing when `frac` is about 0.
    Arc { from: f64, sweep: f64, frac: f64, thickness: f64 },
    /// A box cut on the chamfer: each corner's cut (top-left, top-right,
    /// bottom-right, bottom-left), clamped to the short side.
    Chamfer { cuts: [f64; 4] },
}

/// A premultiplied colour from straight alpha.
pub fn premul(c: Vec4f) -> Vec4f {
    vec4(c.x * c.w, c.y * c.w, c.z * c.w, c.w)
}

/// One shape of `size` placed by `m` (box units to target pixels): filled
/// with `fill`, stroked with `stroke` of `width` centred on its edge (both
/// premultiplied; a negative `width` strokes inside the edge instead, a
/// plate's rim). `px` is target pixels per box unit for the edge's
/// antialiasing; 0 reads it from the screen-space derivatives (a host drawing
/// under a view transform it does not know). `sigma` is the edge's Gaussian
/// sigma in target pixels, with its transition on `side` of the edge.
#[allow(clippy::too_many_arguments)]
pub fn sdf(cx: &mut Cx2d, s: &mut DrawMotionShape, m: &Affine, size: [f64; 2], shape: Shape, fill: Vec4f, stroke: Vec4f, width: f64, px: f64, sigma: f64, side: FeatherSide, opacity: f64) {
    s.set_affine(crate::coeffs(m), dvec2(size[0], size[1]));
    s.s_fill = fill;
    s.s_stroke = if width != 0.0 { stroke } else { vec4(0.0, 0.0, 0.0, 0.0) };
    s.s_width = width as f32;
    s.s_px = px as f32;
    s.s_sigma = sigma.max(0.0) as f32;
    s.s_side = match side { FeatherSide::Both => 0.0, FeatherSide::Inner => -1.0, FeatherSide::Outer => 1.0 };
    s.opacity = opacity as f32;
    match shape {
        Shape::Rect { radius } => {
            s.s_kind = 0.0;
            s.s_radius = radius as f32;
        }
        Shape::Ellipse => {
            s.s_kind = 1.0;
            s.s_radius = 0.0;
        }
        Shape::Arc { from, sweep, frac, thickness } => {
            s.s_kind = 2.0;
            s.s_radius = 0.0;
            s.s_param = vec4(from as f32, sweep as f32, frac as f32, thickness as f32);
        }
        Shape::Chamfer { cuts } => {
            s.s_kind = 3.0;
            s.s_radius = 0.0;
            s.s_param = vec4(cuts[0] as f32, cuts[1] as f32, cuts[2] as f32, cuts[3] as f32);
        }
    }
    // The instance covers the box grown by half the stroke, three sigma and AA;
    // the draw rect is its bounds (the vertex shader places the corners).
    let edge = (1.5 + 3.0 * sigma.max(0.0)) / if px > 0.0 { px } else { 1.0 };
    let e = width.abs() * 0.5 + edge;
    let b = crate::aabb(&m.then(&Affine::translate(-e, -e)), [size[0] + 2.0 * e, size[1] + 2.0 * e]);
    s.draw_abs(cx, Rect { pos: dvec2(b[0], b[1]), size: dvec2((b[2] - b[0]).max(1.0), (b[3] - b[1]).max(1.0)) });
}

fn placed(rect: Rect) -> (Affine, [f64; 2]) {
    (Affine::translate(rect.pos.x, rect.pos.y), [rect.size.x, rect.size.y])
}

/// A flat rectangle in host pixels.
pub fn fill_rect(cx: &mut Cx2d, s: &mut DrawMotionShape, rect: Rect, color: Vec4f) {
    if color.w <= 0.0 || rect.size.x <= 0.0 || rect.size.y <= 0.0 {
        return;
    }
    let (m, size) = placed(rect);
    sdf(cx, s, &m, size, Shape::Rect { radius: 0.0 }, premul(color), vec4(0.0, 0.0, 0.0, 0.0), 0.0, 0.0, 0.0, FeatherSide::Both, 1.0);
}

/// How a plate's corners are made.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Corners {
    /// Rounded by the plate's `radius` (a disc once it reaches the short
    /// side).
    Round,
    /// Cut on the chamfer, each corner (top-left, top-right, bottom-right,
    /// bottom-left) by that many pixels.
    Cut([f32; 4]),
}

/// A plate: a ground with a rim of `border` pixels inside its edge, its
/// corners rounded by `radius` or cut. A plate whose radius reaches half
/// its short side is a disc (a pill on a long box) either way. Straight-alpha colours.
#[derive(Clone, Copy, Debug)]
pub struct Plate {
    pub fill: Vec4f,
    pub stroke: Vec4f,
    pub border: f32,
    pub radius: f32,
    pub corners: Corners,
}

impl Plate {
    /// A flat ground with no rim.
    pub fn flat(fill: Vec4f, radius: f32, corners: Corners) -> Self {
        Plate { fill, stroke: vec4(0.0, 0.0, 0.0, 0.0), border: 0.0, radius, corners }
    }
}

pub fn plate(cx: &mut Cx2d, s: &mut DrawMotionShape, rect: Rect, p: &Plate) {
    let half = (rect.size.x.min(rect.size.y) * 0.5).max(0.0);
    // A rim is a rim: a width past a quarter of the short side (a colour
    // passed where the width goes reads as a huge number) would flood the
    // plate with the stroke colour.
    let border = (p.border as f64).clamp(0.0, half * 0.5);
    if (p.fill.w <= 0.0 && (border <= 0.0 || p.stroke.w <= 0.0)) || rect.size.x <= 0.0 || rect.size.y <= 0.0 {
        return;
    }
    let shape = if p.radius as f64 >= half - 0.5 {
        // Round all the way: a disc, or a pill on a long box.
        Shape::Rect { radius: half }
    } else {
        match p.corners {
            Corners::Round => Shape::Rect { radius: p.radius as f64 },
            Corners::Cut(c) => Shape::Chamfer { cuts: c.map(|c| c as f64) },
        }
    };
    let (m, size) = placed(rect);
    sdf(cx, s, &m, size, shape, premul(p.fill), premul(p.stroke), -border, 0.0, 0.0, FeatherSide::Both, 1.0);
}

/// A round plate whatever its box: a radar, a badge.
pub fn disc(cx: &mut Cx2d, s: &mut DrawMotionShape, rect: Rect, p: &Plate) {
    let d = rect.size.x.min(rect.size.y);
    plate(cx, s, rect, &Plate { radius: (d * 0.5) as f32, ..*p });
}

/// A linear gauge: the track (a plate), what it was a moment ago (`chip`,
/// dim, behind), and the fill, continuous or as `segments` pips.
#[derive(Clone, Copy, Debug)]
pub struct Gauge {
    /// 0..1 of the track lit.
    pub frac: f32,
    pub fill: Vec4f,
    pub track: Vec4f,
    pub stroke: Vec4f,
    pub border: f32,
    pub radius: f32,
    /// The trailing value (drawn when above `frac`), at 35% of the fill.
    pub chip: Option<f32>,
    /// Pips: a segmented gauge reads at a glance (1 or 0 = continuous).
    pub segments: u32,
    /// Fill inset from the track's edge, and the gap between pips.
    pub inset: f32,
    pub gap: f32,
    /// The fill's corner radius.
    pub fill_radius: f32,
    pub corners: Corners,
}

pub fn bar(cx: &mut Cx2d, s: &mut DrawMotionShape, rect: Rect, g: &Gauge) {
    plate(cx, s, rect, &Plate { fill: g.track, stroke: g.stroke, border: g.border, radius: g.radius, corners: g.corners });
    let inset = g.inset as f64;
    let inner = Rect {
        pos: rect.pos + dvec2(inset, inset),
        size: dvec2((rect.size.x - inset * 2.0).max(0.0), (rect.size.y - inset * 2.0).max(0.0)),
    };
    let frac = g.frac.clamp(0.0, 1.0);
    let lit = |cx: &mut Cx2d, s: &mut DrawMotionShape, x: f64, w: f64, color: Vec4f| {
        plate(cx, s, Rect { pos: dvec2(x, inner.pos.y), size: dvec2(w, inner.size.y) }, &Plate::flat(color, g.fill_radius, g.corners));
    };
    if let Some(chip) = g.chip.filter(|c| !c.is_nan() && *c > frac + 0.001) {
        lit(cx, s, inner.pos.x, inner.size.x * chip.clamp(0.0, 1.0) as f64, vec4(g.fill.x, g.fill.y, g.fill.z, 0.35));
    }
    if g.segments > 1 {
        let n = g.segments.min(32);
        let gap = g.gap as f64;
        let seg_w = ((inner.size.x - gap * (n - 1) as f64) / n as f64).max(1.0);
        let on = (frac * n as f32).round() as u32;
        for i in 0..on.min(n) {
            lit(cx, s, inner.pos.x + i as f64 * (seg_w + gap), seg_w, g.fill);
        }
    } else if frac > 0.0 {
        lit(cx, s, inner.pos.x, inner.size.x * frac as f64, g.fill);
    }
}

/// A radial gauge: the full track, then the lit arc over it.
#[derive(Clone, Copy, Debug)]
pub struct Dial {
    pub frac: f32,
    /// Where the arc starts (radians, 0 = +x, clockwise on screen) and how
    /// far a full dial turns.
    pub from: f32,
    pub sweep: f32,
    pub thickness: f32,
    pub fill: Vec4f,
    pub track: Vec4f,
}

pub fn ring(cx: &mut Cx2d, s: &mut DrawMotionShape, rect: Rect, d: &Dial) {
    let (m, size) = placed(rect);
    let none = vec4(0.0, 0.0, 0.0, 0.0);
    let arc = |frac: f32| Shape::Arc { from: d.from as f64, sweep: d.sweep as f64, frac: frac as f64, thickness: d.thickness as f64 };
    if d.track.w > 0.0 {
        sdf(cx, s, &m, size, arc(1.0), premul(d.track), none, 0.0, 0.0, 0.0, FeatherSide::Both, 1.0);
    }
    if d.frac > 0.001 && d.fill.w > 0.0 {
        sdf(cx, s, &m, size, arc(d.frac.clamp(0.0, 1.0)), premul(d.fill), none, 0.0, 0.0, 0.0, FeatherSide::Both, 1.0);
    }
}

/// A premultiplied picture (a recorded Motion frame, [`crate::Drawn`]'s
/// texture) over `rect`, at `opacity`: how a host puts a document on its
/// screen.
pub fn picture(cx: &mut Cx2d, q: &mut DrawMotionQuad, texture: &Texture, rect: Rect, opacity: f32) {
    q.quad(cx, texture, None, &Affine::translate(rect.pos.x, rect.pos.y), [rect.size.x, rect.size.y], [0.0, 0.0, 1.0, 1.0], opacity as f64);
}

// ---------------------------------------------------------------------------
// Text runs
// ---------------------------------------------------------------------------

/// A run's metrics as the text engine lays it out: its line box, and the
/// first row's baseline (ascent) and cap height, in host pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TextMetrics {
    pub w: f32,
    pub h: f32,
    pub ascent: f32,
    pub cap: f32,
}

/// `text` at `size` points in `draw`'s face.
pub fn measure(cx: &mut Cx2d, draw: &mut DrawText, size: f32, text: &str) -> TextMetrics {
    draw.text_style.font_size = size;
    let l = draw.layout(cx, 0.0, 0.0, None, false, Align::default(), text);
    let (ascent, cap) = l.rows.first().map_or((0.0, 0.0), |r| (r.ascender_in_lpxs, r.cap_height_in_lpxs));
    TextMetrics { w: l.size_in_lpxs.width, h: l.size_in_lpxs.height, ascent, cap }
}

/// The typography library's measure over a host face at one size.
struct FaceMeasure<'a, 'b, 'c> {
    cx: std::cell::RefCell<&'a mut Cx2d<'b, 'c>>,
    draw: std::cell::RefCell<&'a mut DrawText>,
    size: f32,
}

impl Measure for FaceMeasure<'_, '_, '_> {
    fn width(&self, text: &str) -> f32 {
        measure(&mut self.cx.borrow_mut(), &mut self.draw.borrow_mut(), self.size, text).w
    }
    fn line_height(&self) -> f32 {
        measure(&mut self.cx.borrow_mut(), &mut self.draw.borrow_mut(), self.size, "Ag").h
    }
}

/// `text` broken into rows no wider than `max_w` pixels: its own newlines,
/// then greedily at spaces (the typography library's wrap). Past
/// `max_rows` (at least as many rows as the author wrote) the last row
/// ends in an ellipsis, and so does any row still too wide (a single long
/// word), cut a character at a time.
pub fn wrap(cx: &mut Cx2d, draw: &mut DrawText, size: f32, text: &str, max_w: f64, max_rows: usize) -> Vec<String> {
    let mut rows = {
        let m = FaceMeasure { cx: std::cell::RefCell::new(&mut *cx), draw: std::cell::RefCell::new(&mut *draw), size };
        fit_wrap(&m, text, max_w as f32)
    };
    while rows.len() > 1 && rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    let cap = max_rows.max(text.trim_end().split('\n').count());
    let mut ellipsize = |row: &mut String, force: bool| {
        if !force && measure(cx, draw, size, row).w as f64 <= max_w {
            return;
        }
        loop {
            let with = format!("{row}…");
            if measure(cx, draw, size, &with).w as f64 <= max_w || row.chars().count() <= 1 {
                *row = with;
                return;
            }
            row.pop();
        }
    };
    if rows.len() > cap {
        rows.truncate(cap);
        if let Some(last) = rows.last_mut() {
            ellipsize(last, true);
        }
    }
    for row in rows.iter_mut() {
        ellipsize(row, false);
    }
    rows
}

/// A block of `text` wrapped at `wrap_w` pixels (one row when 0 or when it
/// fits): the rows, and the block's metrics (its widest row by one line
/// height per row, the first row's baseline and cap).
pub fn block(cx: &mut Cx2d, draw: &mut DrawText, size: f32, text: &str, wrap_w: f32, max_rows: usize) -> (Vec<String>, TextMetrics) {
    let line = measure(cx, draw, size, text);
    if wrap_w <= 0.0 || line.w <= wrap_w {
        return (vec![text.to_string()], line);
    }
    let rows = wrap(cx, draw, size, text, wrap_w as f64, max_rows);
    let mut m = line;
    m.w = rows.iter().map(|r| measure(cx, draw, size, r).w).fold(0.0f32, f32::max);
    m.h = line.h * rows.len() as f32;
    (rows, m)
}

/// Where a line starts (its line box's top) so its capitals are centred on
/// `mid_y`: how a number sits in a gauge, a ring or a button.
pub fn cap_centred_top(m: &TextMetrics, mid_y: f64) -> f64 {
    let cap = if m.cap > 0.0 { m.cap } else { m.h * 0.6 };
    mid_y - (m.ascent - cap * 0.5) as f64
}

/// What keeps a run legible over a picture it does not control.
#[derive(Clone, Copy, Debug)]
pub enum Lift {
    None,
    /// One copy under it, offset (a soft drop shadow). The colour's alpha
    /// is scaled by the run's.
    Shadow { color: Vec4f, offset: DVec2 },
    /// Four copies around it, `width` pixels out (an outline for a banner
    /// or a nametag over a bright sky). The colour's alpha is scaled by
    /// the run's.
    Outline { color: Vec4f, width: f64 },
}

/// One run at `pos` (its line box's top-left) in `color`, lifted.
pub fn text(cx: &mut Cx2d, draw: &mut DrawText, pos: DVec2, text: &str, color: Vec4f, lift: Lift) {
    match lift {
        Lift::None => {}
        Lift::Shadow { color: c, offset } => {
            draw.color = vec4(c.x, c.y, c.z, c.w * color.w);
            draw.draw_abs(cx, pos + offset, text);
        }
        Lift::Outline { color: c, width } => {
            draw.color = vec4(c.x, c.y, c.z, c.w * color.w);
            for o in [dvec2(-width, 0.0), dvec2(width, 0.0), dvec2(0.0, -width), dvec2(0.0, width)] {
                draw.draw_abs(cx, pos + o, text);
            }
        }
    }
    draw.color = color;
    draw.draw_abs(cx, pos, text);
}

/// A run placed by an anchor: `align` 0 puts its left edge on `x`, 0.5 its
/// centre, 1 its right edge; its top at `y`. Returns its metrics.
#[allow(clippy::too_many_arguments)]
pub fn text_at(cx: &mut Cx2d, draw: &mut DrawText, size: f32, x: f64, y: f64, align: f64, s: &str, color: Vec4f, lift: Lift) -> TextMetrics {
    let m = measure(cx, draw, size, s);
    text(cx, draw, dvec2(x - m.w as f64 * align, y), s, color, lift);
    m
}

// ---------------------------------------------------------------------------
// Counters
// ---------------------------------------------------------------------------

/// A counter's text: `value` with `decimals` digits (whole numbers round
/// half away from zero), `grouping` between thousands, as a Motion
/// document's counters and odometers write it.
pub fn counter(value: f64, decimals: u32, grouping: Option<&str>) -> String {
    let v = if decimals == 0 { value.round() } else { value };
    makepad_motion::format_number(v, decimals, grouping)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_round_and_group_like_documents() {
        assert_eq!(counter(2.5, 0, None), "3");
        assert_eq!(counter(-2.5, 0, None), "-3");
        assert_eq!(counter(1234567.0, 0, Some(",")), "1,234,567");
        assert_eq!(counter(3.14159, 2, None), "3.14");
    }

    #[test]
    fn premultiplies() {
        assert_eq!(premul(vec4(1.0, 0.5, 0.0, 0.5)), vec4(0.5, 0.25, 0.0, 0.5));
    }
}
