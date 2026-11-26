//! `Vector` layers: an SVG drawn as editable parts. The document is parsed
//! once; each frame the parts' offsets, the recolouring and the draw-on are
//! applied to a copy of its tree, which Makepad's SVG renderer draws
//! (gradients, `<use>`, shadows included) into the layer's own target.
//! A frame whose parts, draw-on and pixels are the ones drawn before reuses
//! that picture: a dense static illustration costs one quad a frame.

use super::MotionDraw;
use makepad_motion::frame::{Item, ItemKind, PartFrame};
use makepad_motion::geometry;
use makepad_motion::model::Media;
use makepad_widgets::makepad_draw::svg::{SvgDocument, SvgGroup, SvgNode, SvgPaint, SvgPath, Transform2d};
use makepad_widgets::*;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

#[derive(Default)]
pub(crate) struct Vectors {
    /// Parsed documents by their markup's hash.
    docs: HashMap<u64, Rc<SvgDocument>>,
    /// Each layer's picture: the key it was drawn with, and its target.
    targets: HashMap<String, (u64, RenderTarget)>,
    /// Markup of asset sources the host gave (`MotionDraw::set_svg`).
    pub markups: HashMap<Media, Arc<str>>,
}

/// What applies to a subtree: the parts above it, the recolouring and the
/// draw-on progress.
#[derive(Clone)]
struct Inherit {
    opacity: f32,
    fill: Option<[f64; 4]>,
    stroke: Option<[f64; 4]>,
    width: Option<f64>,
    draw_on: Option<f64>,
}

struct Walk<'a> {
    parts: &'a [(String, PartFrame)],
    shapes: usize,
    next: usize,
    stagger: f64,
    trace: f32,
    scale: f32,
}

fn hash_str(s: &str) -> u64 {
    LiveId::from_str(s).0
}

pub(super) fn render(md: &mut MotionDraw, cx: &mut Cx2d, item: &Item, scale: f64) -> Option<Texture> {
    let ItemKind::Vector { src, size, parts, draw_on, stagger, trace, fill, stroke } = &item.kind else { return None };
    let markup: Arc<str> = match src {
        Media::Inline(markup) => markup.clone(),
        Media::Asset(_) => md.vectors.markups.get(src)?.clone(),
        _ => return None,
    };
    let pad = item.feather * 1.5 * scale;
    let px = ((size[0] * scale + 2.0 * pad).ceil().max(1.0) as u32, (size[1] * scale + 2.0 * pad).ceil().max(1.0) as u32);
    let content = [px.0 as f64 - 2.0 * pad, px.1 as f64 - 2.0 * pad];
    let feather = (item.feather * 0.5 * scale) as f32;
    let doc_key = hash_str(&markup) ^ markup.len() as u64;
    let key = LiveId::from_str(&format!("{doc_key}|{px:?}|{parts:?}|{draw_on:?}|{stagger}|{trace}|{fill:?}|{stroke:?}|{feather}|{:?}", item.feather_side)).0;
    if let Some((drawn, target)) = md.vectors.targets.get(&item.path) {
        if *drawn == key {
            return Some(target.texture.clone());
        }
    }
    let doc = md.vectors.docs.entry(doc_key).or_insert_with(|| Rc::new(makepad_widgets::makepad_draw::svg::parse_svg(&markup))).clone();
    let (dw, dh) = doc.logical_size();
    let unit = (content[0] as f32 / dw.max(1e-3)).min(content[1] as f32 / dh.max(1e-3));
    let mut edited = edit(&doc, parts, *draw_on, *stagger, *trace as f32, fill, stroke, unit);
    md.flush_vector(cx);
    md.note_kind("svg");
    let mut target = match md.vectors.targets.remove(&item.path) {
        Some((_, target)) => target,
        None => RenderTarget::new(cx, "motion_vector"),
    };
    target.ensure(cx, px, RenderTargetFormat::Bgra8);
    target.begin(cx, vec4(0.0, 0.0, 0.0, 0.0));
    // Keep the document's viewport mapping (including preserveAspectRatio)
    // at the target size. DrawSvg then places those output pixels unchanged.
    edited.width = Some(content[0] as f32);
    edited.height = Some(content[1] as f32);
    let svg = &mut md.draws.svg;
    svg.svg_doc = Some(edited);
    svg.content_bounds = (0.0, 0.0, content[0] as f32, content[1] as f32);
    svg.preserve_aspect = false;
    // Motion supplies this animated tree; our target cache owns static
    // reuse, and DrawSvg's per-frame geometry pool owns each fresh draw.
    svg.has_animations = true;
    svg.cache_valid = false;
    svg.set_feather(feather, super::feather_side(item.feather_side));
    svg.render_to_rect(cx, &Rect { pos: dvec2(pad, pad), size: dvec2(content[0], content[1]) }, 0.0);
    svg.clear_feather();
    let full = svg.geometry_full();
    md.geometry_full(full);
    target.end(cx);
    let id = target.pass.draw_pass_id();
    let texture = target.texture.clone();
    md.vectors.targets.insert(item.path.clone(), (key, target));
    md.record(id);
    Some(texture)
}

/// A copy of `doc` with the frame's parts, colours and draw-on applied.
#[allow(clippy::too_many_arguments)]
fn edit(doc: &SvgDocument, parts: &[(String, PartFrame)], draw_on: Option<f64>, stagger: f64, trace: f32, fill: &Option<[f64; 4]>, stroke: &Option<[f64; 4]>, unit: f32) -> SvgDocument {
    let mut out = doc.clone();
    let shapes = count_shapes(&out.root);
    let mut walk = Walk { parts, shapes, next: 0, stagger, trace, scale: unit };
    let inherit = Inherit { opacity: 1.0, fill: *fill, stroke: *stroke, width: None, draw_on };
    let nothing_to_do = parts.is_empty() && draw_on.is_none() && fill.is_none() && stroke.is_none();
    if !nothing_to_do {
        edit_nodes(&mut out.root, &inherit, &mut walk);
    }
    out
}

fn count_shapes(nodes: &[SvgNode]) -> usize {
    nodes.iter().map(|node| match node {
        SvgNode::Group(g) => count_shapes(&g.children),
        _ => 1,
    }).sum()
}

fn node_id(node: &SvgNode) -> Option<&str> {
    match node {
        SvgNode::Group(n) => n.id.as_deref(),
        SvgNode::Path(n) => n.id.as_deref(),
        SvgNode::Rect(n) => n.id.as_deref(),
        SvgNode::Circle(n) => n.id.as_deref(),
        SvgNode::Ellipse(n) => n.id.as_deref(),
        SvgNode::Line(n) => n.id.as_deref(),
        SvgNode::Polyline(n) => n.id.as_deref(),
        SvgNode::Polygon(n) => n.id.as_deref(),
        SvgNode::Use(n) => n.id.as_deref(),
    }
}

fn node_transform(node: &mut SvgNode) -> &mut Transform2d {
    match node {
        SvgNode::Group(n) => &mut n.transform,
        SvgNode::Path(n) => &mut n.transform,
        SvgNode::Rect(n) => &mut n.transform,
        SvgNode::Circle(n) => &mut n.transform,
        SvgNode::Ellipse(n) => &mut n.transform,
        SvgNode::Line(n) => &mut n.transform,
        SvgNode::Polyline(n) => &mut n.transform,
        SvgNode::Polygon(n) => &mut n.transform,
        SvgNode::Use(n) => &mut n.transform,
    }
}

/// A node's bounds in its parent's coordinates.
fn node_bounds(node: &SvgNode) -> Option<[f32; 4]> {
    let (xf, points): (Transform2d, Vec<(f32, f32)>) = match node {
        SvgNode::Group(g) => {
            let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
            for child in &g.children {
                if let Some(c) = node_bounds(child) {
                    for (x, y) in [(c[0], c[1]), (c[2], c[1]), (c[0], c[3]), (c[2], c[3])] {
                        let (x, y) = g.transform.apply(x, y);
                        b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
                    }
                }
            }
            return (b[0] <= b[2]).then_some(b);
        }
        other => {
            let (xf, path) = makepad_motion::icons::node_path(other)?;
            let pts = path.cmds.iter().filter_map(|cmd| match cmd {
                geometry::PathCmd::MoveTo(x, y) | geometry::PathCmd::LineTo(x, y) | geometry::PathCmd::BezierTo(_, _, _, _, x, y) => Some((*x, *y)),
                _ => None,
            }).collect();
            (xf, pts)
        }
    };
    let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for (x, y) in points {
        let (x, y) = xf.apply(x, y);
        b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
    }
    (b[0] <= b[2]).then_some(b)
}

fn paint(c: [f64; 4]) -> SvgPaint {
    SvgPaint::Color(c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32)
}

fn edit_nodes(nodes: &mut Vec<SvgNode>, inherit: &Inherit, walk: &mut Walk) {
    let mut out: Vec<SvgNode> = Vec::with_capacity(nodes.len());
    for mut node in nodes.drain(..) {
        let mut inherit = inherit.clone();
        if let Some(id) = node_id(&node).map(str::to_string) {
            if let Some((_, part)) = walk.parts.iter().find(|(pid, _)| *pid == id) {
                inherit.opacity *= part.opacity as f32;
                inherit.fill = part.fill.or(inherit.fill);
                inherit.stroke = part.stroke.or(inherit.stroke);
                inherit.width = part.width.or(inherit.width);
                inherit.draw_on = part.draw_on.or(inherit.draw_on);
                let moved = part.offset != [0.0, 0.0] || part.scale != [1.0, 1.0] || part.rotate != 0.0;
                if moved {
                    if let Some(b) = node_bounds(&node) {
                        let c = ((b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5);
                        let about = Transform2d::translate(-c.0, -c.1)
                            .then(&Transform2d::scale(part.scale[0] as f32, part.scale[1] as f32))
                            .then(&Transform2d::rotate((part.rotate as f32).to_radians()))
                            .then(&Transform2d::translate(c.0 + part.offset[0] as f32, c.1 + part.offset[1] as f32));
                        let xf = node_transform(&mut node);
                        *xf = xf.then(&about);
                    }
                }
            }
        }
        match &mut node {
            SvgNode::Group(group) => {
                edit_nodes(&mut group.children, &inherit, walk);
                out.push(node);
            }
            SvgNode::Use(_) => out.push(node),
            _ => {
                let k = walk.next;
                walk.next += 1;
                out.extend(edit_shape(node, &inherit, walk, k));
            }
        }
    }
    *nodes = out;
}

/// One shape with its colours, opacity and draw-on progress: itself, or
/// its fill (fading in) and its outline traced to the progress.
fn edit_shape(mut node: SvgNode, inherit: &Inherit, walk: &Walk, k: usize) -> Vec<SvgNode> {
    let style = match &mut node {
        SvgNode::Path(n) => &mut n.style,
        SvgNode::Rect(n) => &mut n.style,
        SvgNode::Circle(n) => &mut n.style,
        SvgNode::Ellipse(n) => &mut n.style,
        SvgNode::Line(n) => &mut n.style,
        SvgNode::Polyline(n) => &mut n.style,
        SvgNode::Polygon(n) => &mut n.style,
        SvgNode::Group(_) | SvgNode::Use(_) => return vec![node],
    };
    style.opacity *= inherit.opacity;
    if let Some(c) = inherit.fill {
        if !matches!(style.fill, Some(SvgPaint::None)) {
            style.fill = Some(paint(c));
        }
    }
    if let Some(c) = inherit.stroke {
        if style.stroke.as_ref().is_some_and(|p| !matches!(p, SvgPaint::None)) {
            style.stroke = Some(paint(c));
        }
    }
    if let Some(w) = inherit.width {
        style.stroke_width = w as f32;
    }
    let Some(d) = inherit.draw_on else { return vec![node] };
    // This element's share of the draw-on.
    let n = walk.shapes.max(1) as f64;
    let spread = walk.stagger;
    // Element k draws over [k/n·spread, k/n·spread + 1 − spread·(n−1)/n]:
    // the last one ends at 1, and a lone element takes the whole draw-on.
    let p = ((d - k as f64 / n * spread) / (1.0 - spread * (n - 1.0) / n)).clamp(0.0, 1.0);
    if p >= 1.0 {
        return vec![node];
    }
    if p <= 0.0 || style.opacity <= 0.0 {
        return Vec::new();
    }
    let style = style.clone();
    let Some((xf, path)) = makepad_motion::icons::node_path(&node) else { return vec![node] };
    let mut out = Vec::new();
    let has_fill = style.fill.as_ref().is_some_and(|f| !matches!(f, SvgPaint::None));
    let has_stroke = style.stroke.as_ref().is_some_and(|s| !matches!(s, SvgPaint::None)) && style.stroke_width > 0.0;
    // The fill fades in over the second half of the element's share.
    if has_fill {
        let mut fill_style = style.clone();
        fill_style.fill_opacity *= ((p - 0.5) / 0.5).clamp(0.0, 1.0) as f32;
        fill_style.stroke = None;
        if fill_style.fill_opacity > 0.0 {
            out.push(SvgNode::Path(SvgPath { id: None, style: fill_style, transform: xf.clone(), path: path.clone(), animations: Vec::new(), animate_transforms: Vec::new() }));
        }
    }
    // The outline traced to `p`: its own stroke, or (a fill-only shape) a
    // trace in the fill's colour.
    let trimmed = geometry::trim(&path, 0.0, p, 0.25 / walk.scale.max(1e-3) as f64);
    let mut line = style.clone();
    line.fill = Some(SvgPaint::None);
    if !has_stroke {
        line.stroke = style.fill.clone();
        line.stroke_width = walk.trace;
        line.stroke_opacity = style.fill_opacity;
    }
    if line.stroke.is_some() {
        out.push(SvgNode::Path(SvgPath { id: None, style: line, transform: xf, path: trimmed, animations: Vec::new(), animate_transforms: Vec::new() }));
    }
    let _ = SvgGroup::default();
    out
}
