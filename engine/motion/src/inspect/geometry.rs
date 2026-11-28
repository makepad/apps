//! Geometry for authoring checks, in document pixels. No rasterisation.

use crate::plain::PrimitiveFrame;
use crate::{geometry, Affine, Align, Blend, Cap, Item, ItemKind, Join, VAlign};
use makepad_typography::{font::OutlineFont, layout::{layout, TextStyle}};
use std::ops::Range;

pub(super) type Box2 = [f64; 4];
pub(super) type Polygon = Vec<[f64; 2]>;

pub(super) struct Ink {
    pub path: String,
    pub bounds: Option<Box2>,
    pub glyphs: Vec<Polygon>,
    pub min_px: Option<f64>,
    pub fill: Vec<Polygon>,
    pub stroke: Vec<Polygon>,
    pub opaque: bool,
    /// Each one-line text drawn: its text, its box on the canvas and its
    /// em in canvas units (for the split-sentence check).
    pub runs: Vec<(String, Box2, f64)>,
    /// How much of the layer shows: its ink's area on the canvas weighted
    /// by its alpha (opacity × paint), in square document pixels. Groups:
    /// their own children's, summed (the continuity check, `layer.pop`).
    pub presence: f64,
    group_visible: Option<bool>,
}

impl Ink {
    fn new(item: &Item) -> Self {
        Self { path: item.path.clone(), bounds: None, glyphs: Vec::new(), min_px: None, fill: Vec::new(), stroke: Vec::new(), opaque: false, runs: Vec::new(), presence: 0.0, group_visible: None }
    }

    pub fn is_group(&self) -> bool {
        self.group_visible.is_some()
    }

    pub fn visible(&self, canvas: Box2) -> bool {
        if let Some(visible) = self.group_visible { return visible }
        if !self.bounds.is_some_and(|b| intersects(b, canvas)) { return false }
        if self.glyphs.is_empty() && self.fill.is_empty() && self.stroke.is_empty() { return true }
        let canvas = quad(canvas, Affine::IDENTITY);
        self.glyphs.iter().chain(&self.fill).chain(&self.stroke).any(|p| overlap(p, &canvas))
    }
}

/// A convex polygon's area clipped to the canvas.
pub(super) fn area_on(polygon: &Polygon, canvas: Box2) -> f64 {
    let p = on_canvas(polygon, canvas);
    if p.len() < 3 { return 0.0 }
    let mut twice = 0.0;
    for i in 0..p.len() {
        let (a, b) = (p[i], p[(i + 1) % p.len()]);
        twice += a[0] * b[1] - b[0] * a[1];
    }
    twice.abs() * 0.5
}

pub(super) fn union(a: Box2, b: Box2) -> Box2 {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}

pub(super) fn bounds(points: &[[f64; 2]]) -> Option<Box2> {
    points.iter().map(|p| [p[0], p[1], p[0], p[1]]).reduce(union)
}

pub(super) fn intersects(a: Box2, b: Box2) -> bool {
    a[2].min(b[2]) - a[0].max(b[0]) > 1e-6 && a[3].min(b[3]) - a[1].max(b[1]) > 1e-6
}

pub(super) fn rect(b: Box2) -> Polygon {
    quad(b, Affine::IDENTITY)
}

fn quad(b: Box2, map: Affine) -> Polygon {
    [[b[0], b[1]], [b[2], b[1]], [b[2], b[3]], [b[0], b[3]]].map(|p| map.apply(p)).to_vec()
}

/// Separating axes of convex polygons, so rotated glyphs do not collide
/// just because their axis-aligned bounds do. Edge contact has no area.
pub(super) fn overlap(a: &Polygon, b: &Polygon) -> bool {
    if !bounds(a).zip(bounds(b)).is_some_and(|(a, b)| intersects(a, b)) {
        return false;
    }
    for p in [a, b] {
        for i in 0..p.len() {
            let (u, v) = (p[i], p[(i + 1) % p.len()]);
            let n = [u[1] - v[1], v[0] - u[0]];
            let len = n[0].hypot(n[1]);
            if len < 1e-9 { continue }
            let project = |q: &Polygon| q.iter().map(|p| (p[0] * n[0] + p[1] * n[1]) / len)
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), x| (lo.min(x), hi.max(x)));
            let (pa, pb) = (project(a), project(b));
            if pa.1.min(pb.1) - pa.0.max(pb.0) <= 1e-6 { return false }
        }
    }
    true
}

/// Clip convex geometry to the canvas for pair checks. Keep the original
/// bounds separately for the cutoff diagnostic.
pub(super) fn on_canvas(polygon: &Polygon, canvas: Box2) -> Polygon {
    let mut p = polygon.clone();
    for (axis, edge, side) in [(0, canvas[0], 1.0), (0, canvas[2], -1.0), (1, canvas[1], 1.0), (1, canvas[3], -1.0)] {
        let mut clipped = Vec::new();
        for i in 0..p.len() {
            let (a, b) = (p[i], p[(i + 1) % p.len()]);
            let (da, db) = ((a[axis] - edge) * side, (b[axis] - edge) * side);
            if da >= 0.0 { clipped.push(a); }
            if (da < 0.0) != (db < 0.0) {
                let t = da / (da - db);
                clipped.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
            }
        }
        p = clipped;
    }
    p
}

fn mesh(path: &geometry::VectorPath, map: Affine, stroke: Option<(f64, Cap, Join)>) -> Vec<Polygon> {
    use makepad_svg::{LineCap, LineJoin, Tessellator};
    let mut tess = Tessellator::default();
    // A quarter output pixel, even when a layer is enlarged.
    let scale = map.a.hypot(map.b).max(map.c.hypot(map.d)).max(1e-6);
    tess.flatten(path, (0.25 / scale) as f32);
    let (mut vertices, mut indices) = (Vec::new(), Vec::new());
    match stroke {
        Some((width, cap, join)) => {
            let cap = match cap { Cap::Butt => LineCap::Butt, Cap::Round => LineCap::Round, Cap::Square => LineCap::Square };
            let join = match join { Join::Miter => LineJoin::Miter, Join::Round => LineJoin::Round, Join::Bevel => LineJoin::Bevel };
            tess.stroke(width as f32, cap, join, 4.0, 0.0, &mut vertices, &mut indices);
        }
        None => tess.fill(0.0, LineJoin::Miter, 10.0, false, &mut vertices, &mut indices),
    }
    indices.chunks_exact(3).filter_map(|ix| {
        let p: Polygon = ix.iter().map(|&i| { let v = vertices[i as usize]; map.apply([v.x as f64, v.y as f64]) }).collect();
        let area = (p[1][0] - p[0][0]) * (p[2][1] - p[0][1]) - (p[1][1] - p[0][1]) * (p[2][0] - p[0][0]);
        (area.abs() > 1e-9).then_some(p)
    }).collect()
}

/// Byte ranges are retained through wrapping: reveal and per-unit ranges
/// refer to the original text, including whitespace and multibyte glyphs.
fn rows(font: &OutlineFont, text: &str, em: f64, wrap: Option<f64>) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut base = 0;
    for para in text.split('\n') {
        let Some(width) = wrap.filter(|w| *w > 0.0) else {
            out.push(base..base + para.len());
            base += para.len() + 1;
            continue;
        };
        let laid = layout(font, para, &TextStyle { size: em as f32, ..TextStyle::default() });
        let bytes: Vec<usize> = para.char_indices().map(|(b, _)| b).chain(std::iter::once(para.len())).collect();
        let mut start = 0;
        let mut start_x = 0.0;
        let mut break_at: Option<(usize, f64, usize)> = None;
        let mut i = 0;
        while i < laid.glyphs.len() {
            let g = &laid.glyphs[i];
            let byte = bytes[g.char_index];
            let space = para[byte..].chars().next().is_some_and(char::is_whitespace);
            if g.x as f64 + g.advance as f64 - start_x > width && !space && byte > start {
                let (end, x, next) = break_at.take().filter(|(b, _, _)| *b > start).unwrap_or((byte, g.x as f64, i));
                out.push(base + start..base + end);
                start = end;
                start_x = x;
                i = next;
                continue;
            }
            if space {
                let end = bytes.get(g.char_index + 1).copied().unwrap_or(para.len());
                break_at = Some((end, g.x as f64 + g.advance as f64, i + 1));
            }
            i += 1;
        }
        out.push(base + start..base + para.len());
        base += para.len() + 1;
    }
    out
}

struct Glyph {
    byte: usize,
    ink: Box2,
    pen: Box2,
    has_ink: bool,
}

fn text_ink(item: &Item, opacity: f64, canvas: Box2, out: &mut Ink) -> Result<(), String> {
    let ItemKind::Primitive(PrimitiveFrame::Text { text, font, weight, italic, axes, size, align, valign, line_height, tracking, wrap, fit, units, reveal, path, outline, .. }) = &item.kind else { return Ok(()) };
    if text.trim().is_empty() || *reveal == Some(0) || *size <= 0.0 { return Ok(()) }
    let family = makepad_doc_fonts::drawn_family(font);
    let weight = family.map_or(*weight, |f| makepad_doc_fonts::drawn_weight(f, *weight, *italic));
    let font = crate::type_fns::font_for(font, weight, *italic, axes, text)?;
    let af = match align { Align::Left => 0.0, Align::Center => 0.5, Align::Right => 1.0 };
    let vf = match valign { VAlign::Top => 0.0, VAlign::Center => 0.5, VAlign::Bottom => 1.0 };
    let mut em = size * 4.0 / 3.0;
    let (glyphs, box_size) = loop {
        let style = TextStyle { size: em as f32, ..TextStyle::default() };
        let ranges = rows(&font, text, em, *wrap);
        let laid: Vec<_> = ranges.iter().map(|r| layout(&font, &text[r.clone()], &style)).collect();
        let width = laid.iter().map(|l| l.width() as f64 + tracking * l.glyphs.len().saturating_sub(1) as f64).fold(0.0, f64::max);
        if let Some(fit) = fit.filter(|f| *f > 0.0 && width > *f + 0.01 && em > 4.0 / 3.0) {
            let next = (em * fit / width * 0.999).max(4.0 / 3.0);
            if next < em { em = next; continue }
        }
        let metrics = font.metrics().scaled(em as f32);
        let asc = metrics.ascender as f64 + family.map_or(0.0, |f| f.ascent as f64) * em;
        let height = asc - metrics.descender as f64;
        let line_step = (height + metrics.line_gap as f64) * line_height;
        let mut glyphs = Vec::new();
        let natural_width = wrap.unwrap_or_else(|| laid.iter().map(|l| l.width() as f64).fold(0.0, f64::max));
        for (row, (r, l)) in ranges.iter().zip(&laid).enumerate() {
            let bytes: Vec<usize> = text[r.clone()].char_indices().map(|(b, _)| r.start + b).collect();
            let shift = (natural_width - l.width() as f64) * af;
            let top = row as f64 * line_step;
            for (k, g) in l.glyphs.iter().enumerate() {
                let x = shift + tracking * k as f64;
                glyphs.push(Glyph {
                    byte: bytes[g.char_index],
                    ink: [x + g.ink[0] as f64, top + asc - g.ink[3] as f64, x + g.ink[2] as f64, top + asc - g.ink[1] as f64],
                    pen: [x + g.x as f64, top, x + g.x as f64 + g.advance as f64, top + height],
                    has_ink: g.has_ink(),
                });
            }
        }
        break (glyphs, [width, height + (ranges.len().saturating_sub(1)) as f64 * line_step]);
    };
    let base = item.world.then(&Affine::translate(-box_size[0] * af, -box_size[1] * vf)).then(&item.local.affine(box_size));
    let end = reveal.and_then(|n| text.char_indices().nth(n).map(|(b, _)| b)).unwrap_or(text.len());
    let line = path.as_ref().map(|(data, offset)| (crate::Polyline::from_path(data), *offset));
    let first_glyph = out.glyphs.len();
    for unit in units {
        let alpha = unit.color[3].max(outline.as_ref().map_or(0.0, |(c, w)| if *w > 0.0 { c[3] } else { 0.0 }));
        if opacity * unit.opacity * alpha <= super::VISIBLE_ALPHA { continue }
        let Some(b) = glyphs.iter().filter(|g| unit.range.contains(&g.byte)).map(|g| g.pen).reduce(union) else { continue };
        let (w, h) = (b[2] - b[0], b[3] - b[1]);
        let own = Affine::about([w * 0.5, h * 0.5], &Affine::rotate(unit.rotate).then(&Affine::scale(unit.scale[0], unit.scale[1])));
        let map = if let Some((line, offset)) = &line {
            let length = line.length().max(1e-9);
            let s = offset * length + b[0] + w * 0.5;
            if s < 0.0 || s > length { continue }
            let u = s / length;
            let p = line.point(u);
            let (p0, p1) = (line.point((u - 0.002).max(0.0)), line.point((u + 0.002).min(1.0)));
            let angle = (p1[1] - p0[1]).atan2(p1[0] - p0[0]).to_degrees();
            item.world.then(&Affine::translate(p[0] + unit.offset[0], p[1] + unit.offset[1]))
                .then(&Affine::rotate(angle)).then(&Affine::translate(-w * 0.5, -h * 0.8)).then(&own)
        } else {
            base.then(&Affine::translate(b[0] + unit.offset[0], b[1] + unit.offset[1])).then(&own)
        }.then(&Affine::translate(-b[0], -b[1]));
        if (map.a * map.d - map.b * map.c).abs() <= 1e-12 { continue }
        for glyph in glyphs.iter().filter(|g| g.has_ink && g.byte < end && unit.range.contains(&g.byte)) {
            let p = quad(glyph.ink, map);
            if let Some(b) = bounds(&p) {
                out.presence += area_on(&p, canvas) * opacity * unit.opacity * alpha;
                out.bounds = Some(out.bounds.map_or(b, |old| union(old, b)));
                out.glyphs.push(p);
                let px = em * map.c.hypot(map.d);
                out.min_px = Some(out.min_px.map_or(px, |old| old.min(px)));
            }
        }
    }
    if !text.contains('\n') && path.is_none() {
        if let Some(b) = out.glyphs[first_glyph..].iter().filter_map(|p| bounds(p)).reduce(union) {
            out.runs.push((text.clone(), b, em * base.c.hypot(base.d)));
        }
    }
    Ok(())
}

/// A capsule from `a` to `b`, `width` across, as a quad.
fn capsule(a: [f64; 2], b: [f64; 2], width: f64, map: Affine) -> Polygon {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len = dx.hypot(dy);
    let h = width * 0.5;
    let (ux, uy) = if len > 1e-9 { (dx / len * h, dy / len * h) } else { (h, 0.0) };
    let (nx, ny) = (-uy, ux);
    [[a[0] - ux + nx, a[1] - uy + ny], [b[0] + ux + nx, b[1] + uy + ny], [b[0] + ux - nx, b[1] + uy - ny], [a[0] - ux - nx, a[1] - uy - ny]]
        .map(|p| map.apply(p)).to_vec()
}

fn contours_path(contours: &[Vec<[f64; 2]>]) -> geometry::VectorPath {
    let mut data = String::new();
    for c in contours.iter().filter(|c| c.len() >= 3) {
        for (i, p) in c.iter().enumerate() {
            data.push_str(&format!("{} {} {} ", if i == 0 { "M" } else { "L" }, p[0], p[1]));
        }
        data.push_str("Z ");
    }
    geometry::parse_path(&data)
}

/// What a Draw2D picture paints: capsules as strokes, filled contours as
/// fills, text runs as glyph boxes (one per run, from its measured width).
fn draw2d_ink(list: &crate::draw2d::Draw2dList, map: Affine, opacity: f64, canvas: Box2, out: &mut Ink) {
    use crate::draw2d::{Baseline, Op};
    let visible = |alpha: f64| alpha * opacity > super::VISIBLE_ALPHA;
    // Clip regions (their bounding boxes, in output units) the ops inside them stay in.
    let mut clips: Vec<Box2> = Vec::new();
    let clipped = |p: Polygon, clips: &[Box2]| -> Option<Polygon> {
        let p = clips.last().map_or(p.clone(), |c| on_canvas(&p, *c));
        (p.len() >= 3).then_some(p)
    };
    for op in &list.ops {
        match op {
            Op::Segs { start, end, .. } => {
                for seg in list.segs[*start..(*end).min(list.segs.len())].iter().filter(|s| visible(s.color[3] as f64) && s.width > 0.0) {
                    let quad = capsule([seg.a[0] as f64, seg.a[1] as f64], [seg.b[0] as f64, seg.b[1] as f64], seg.width as f64, map);
                    if let Some(q) = clipped(quad, &clips) {
                        out.presence += area_on(&q, canvas) * opacity * seg.color[3] as f64;
                        out.stroke.push(q);
                    }
                }
            }
            Op::Fill { contours, color } if visible(color[3]) => {
                for p in mesh(&contours_path(contours), map, None).into_iter().filter_map(|p| clipped(p, &clips)) {
                    out.presence += area_on(&p, canvas) * opacity * color[3];
                    out.fill.push(p);
                }
            }
            Op::Gradient { contours, alpha, .. } if visible(*alpha) => {
                for p in mesh(&contours_path(contours), map, None).into_iter().filter_map(|p| clipped(p, &clips)) {
                    out.presence += area_on(&p, canvas) * opacity * alpha;
                    out.fill.push(p);
                }
            }
            Op::ClipBegin { contours } => {
                let pts: Vec<[f64; 2]> = contours.iter().flatten().map(|p| map.apply(*p)).collect();
                let b = bounds(&pts).unwrap_or([0.0; 4]);
                let b = clips.last().map_or(b, |c| [b[0].max(c[0]), b[1].max(c[1]), b[2].min(c[2]), b[3].min(c[3])]);
                clips.push(b);
            }
            Op::ClipEnd => { clips.pop(); }
            Op::Text(t) if visible(t.color[3]) && !t.text.trim().is_empty() => {
                let Ok(width) = crate::type_fns::text_width(&t.text, &t.font, t.px, t.weight, t.italic, &t.axes) else { continue };
                let width = width + t.ls * t.text.chars().count().saturating_sub(1) as f64;
                let x0 = t.x - width * t.align;
                // Cap height above the baseline, a little descender below.
                let top = match t.baseline { Baseline::Alphabetic => -0.72, Baseline::Top => 0.0, Baseline::Middle => -0.36, Baseline::Bottom => -0.92 } * t.px;
                let own = Affine { a: t.m[0], b: t.m[1], c: t.m[2], d: t.m[3], e: t.m[4], f: t.m[5] };
                let Some(quad) = clipped(quad([x0, t.y + top, x0 + width, t.y + top + 0.92 * t.px], map.then(&own)), &clips) else { continue };
                let px = t.px * (own.c.hypot(own.d)) * map.c.hypot(map.d);
                out.min_px = Some(out.min_px.map_or(px, |old| old.min(px)));
                if let Some(b) = bounds(&quad) {
                    out.runs.push((t.text.clone(), b, px));
                }
                out.presence += area_on(&quad, canvas) * opacity * t.color[3];
                out.glyphs.push(quad);
            }
            _ => {}
        }
    }
    out.bounds = out.glyphs.iter().chain(&out.fill).chain(&out.stroke).filter_map(|p| bounds(p)).reduce(union);
}

/// Depth-first paint order; a group's opacity multiplies its children.
/// Masks are not painted as foreground shapes. `glyphs` names the groups
/// whose paths are letters (`Group{glyphs: true}`: Math, MatchMorph).
pub(super) fn collect(items: &[Item], opacity: f64, normal: bool, canvas: Box2, glyphs: &[String], out: &mut Vec<Ink>, unchecked: &mut Vec<(String, String)>) {
    for item in items {
        let opacity = opacity * item.opacity;
        if opacity <= super::VISIBLE_ALPHA { continue }
        let normal = normal && item.blend == Blend::Normal;
        if let ItemKind::Group { children, mask, .. } = &item.kind {
            if mask.is_some() {
                unchecked.push((item.path.clone(), "masked content needs a rendered frame to determine its visible glyphs".into()));
                continue;
            }
            let start = out.len();
            collect(children, opacity, normal, canvas, glyphs, out, unchecked);
            if glyphs.iter().any(|g| *g == item.path) {
                // A formula is one text: its pieces are checked together.
                let mut text = Ink::new(item);
                for piece in out.drain(start..) {
                    text.bounds = match (text.bounds, piece.bounds) { (Some(a), Some(b)) => Some(union(a, b)), (a, b) => a.or(b) };
                    text.min_px = match (text.min_px, piece.min_px) { (Some(a), Some(b)) => Some(a.min(b)), (a, b) => a.or(b) };
                    if !piece.is_group() { text.presence += piece.presence; }
                    text.glyphs.extend(piece.glyphs);
                    text.stroke.extend(piece.stroke);
                }
                out.push(text);
                continue;
            }
            let mut group = Ink::new(item);
            group.bounds = out[start..].iter().filter_map(|i| i.bounds).reduce(union);
            group.group_visible = Some(out[start..].iter().any(|i| i.visible(canvas)));
            group.presence = out[start..].iter().filter(|i| !i.is_group()).map(|i| i.presence).sum();
            out.push(group);
            continue;
        }
        let mut ink = Ink::new(item);
        let shape = match &item.kind {
            ItemKind::Primitive(PrimitiveFrame::Rect { size, radius, fill, stroke, width, trim, cap, .. }) => Some((geometry::rect_path(*size, *radius), *size, fill, stroke, width, trim, cap, Join::Miter)),
            ItemKind::Primitive(PrimitiveFrame::Ellipse { size, fill, stroke, width, trim, cap, .. }) => Some((geometry::ellipse_path(*size), *size, fill, stroke, width, trim, cap, Join::Round)),
            ItemKind::Primitive(PrimitiveFrame::Path { path: vector, fill, stroke, width, trim, cap, join }) => {
                let path = (**vector).clone();
                let size = geometry::bounds(&path).map_or([0.0, 0.0], |b| [b[2].max(0.0), b[3].max(0.0)]);
                Some((path, size, fill, stroke, width, trim, cap, *join))
            }
            _ => None,
        };
        if let Some((path, size, fill, stroke, width, trim, cap, join)) = shape {
            let map = item.transform(size);
            if fill.is_some_and(|c| c[3] * opacity > super::VISIBLE_ALPHA) {
                ink.fill = mesh(&path, map, None);
                let alpha = fill.map_or(0.0, |c| c[3]) * opacity;
                ink.presence += ink.fill.iter().map(|p| area_on(p, canvas)).sum::<f64>() * alpha;
                ink.opaque = normal && fill.is_some_and(|c| c[3] * opacity >= 1.0 - 1e-6);
            }
            if *width > 0.0 && trim[1] > trim[0] && stroke.is_some_and(|c| c[3] * opacity > super::VISIBLE_ALPHA) {
                let scale = map.a.hypot(map.b).max(map.c.hypot(map.d)).max(1e-6);
                let path = geometry::trim(&path, trim[0], trim[1], 0.25 / scale);
                ink.stroke = if matches!(item.kind, ItemKind::Primitive(PrimitiveFrame::Path { .. })) || *trim != [0.0, 1.0] {
                    // Vector strokes keep a uniform output width; whole
                    // rectangles/ellipses use their local distance field.
                    let path = geometry::transformed(&path, &map);
                    mesh(&path, Affine::IDENTITY, Some((*width * map.mean_scale(), *cap, join)))
                } else {
                    mesh(&path, map, Some((*width, *cap, join)))
                };
                let alpha = stroke.map_or(0.0, |c| c[3]) * opacity;
                ink.presence += ink.stroke.iter().map(|p| area_on(p, canvas)).sum::<f64>() * alpha;
            }
            ink.bounds = ink.fill.iter().chain(&ink.stroke).filter_map(|p| bounds(p)).reduce(union);
            // A letter drawn as a path: its ink box is a glyph.
            if !ink.fill.is_empty() && glyphs.iter().any(|g| item.path.starts_with(g.as_str()) && item.path[g.len()..].starts_with('.')) {
                let b = ink.fill.iter().filter_map(|p| bounds(p)).reduce(union);
                if let Some(b) = b { ink.glyphs.push(quad(b, Affine::IDENTITY)); }
                ink.fill.clear();
                ink.opaque = false;
            }
        } else if let ItemKind::Draw2d { picture, size } = &item.kind {
            draw2d_ink(&picture.list, item.transform(*size), opacity, canvas, &mut ink);
        } else if matches!(item.kind, ItemKind::Primitive(PrimitiveFrame::Text { .. })) {
            if let Err(error) = text_ink(item, opacity, canvas, &mut ink) { unchecked.push((item.path.clone(), error)); }
        } else if let Some(size) = item.box_size() {
            if size[0] > 0.0 && size[1] > 0.0 {
                let q = quad([0.0, 0.0, size[0], size[1]], item.transform(size));
                ink.presence = area_on(&q, canvas) * opacity;
                ink.bounds = bounds(&q);
            }
        } else {
            unchecked.push((item.path.clone(), "this layer's bounds are resolved by the renderer".into()));
        }
        out.push(ink);
    }
}
