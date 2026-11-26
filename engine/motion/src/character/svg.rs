//! Character models as SVG: importing a drawn character whose groups carry
//! the part ids, and exporting a kit character as such an SVG (a model
//! sheet to start a custom character from).
//!
//! The ids a character SVG uses (any editor keeps them: Inkscape's object
//! ids, Illustrator's layer names exported as ids):
//!
//! - Skeleton: `hips`, `torso`, `head`, `arm_l_upper`, `arm_l_lower`,
//!   `hand_l`, `arm_r_upper`, `arm_r_lower`, `hand_r`, `leg_l_upper`,
//!   `leg_l_lower`, `foot_l`, `leg_r_upper`, `leg_r_lower`, `foot_r`. The
//!   character is drawn standing, arms and legs hanging straight down,
//!   facing screen-right; `_l` / `_r` are the screen-left / screen-right
//!   limbs.
//! - Face slots: `eyes_open`, `eyes_closed` (blinks), `eyes_<expression>`;
//!   `brows_neutral`, `brows_<expression>`; `mouth_REST`, `mouth_MBP`,
//!   `mouth_TDSKN`, `mouth_EEIY`, `mouth_AHA`, `mouth_OH`, `mouth_OOWR`,
//!   `mouth_FV`, `mouth_LTH` (the visemes), `mouth_<expression>` (at rest).
//! - Extras: `mic` and `prop` (in the right hand, shown by the poses that
//!   hold them), `hair…`, `cloth…`, `scarf…`, `tail…`, `antenna…`,
//!   `medallion`, `chain…` (these sway), `glasses`, `hat`, `cap`,
//!   `headphones`, or any `part_<name>`.
//! - Pivots: an element `pivot_<part>` (a small circle; its centre is the
//!   pivot). Without one, a limb turns at the top of its bounds, the head
//!   and torso at the bottom, everything else at its centre.
//!
//! A part's parent is the part it is nested in; a part nested in none gets
//! the skeleton's parent (a hand hangs off its forearm). Draw order is the
//! document's order.

use super::pose::Joint;
use super::{Geom, Part, Rig, Shape, Slot};
use crate::geometry::{PathCmd, VectorPath};
use crate::model::{Cap, Color, Join};
use makepad_svg::document::{SvgNode, SvgPaint, SvgStyle, Transform2d};
use makepad_svg::path::{LineCap, LineJoin};

const EXTRA_PREFIXES: &[&str] = &["hair", "cloth", "scarf", "tail", "antenna", "medallion", "chain", "necklace", "glasses", "hat", "cap", "beanie", "headphones", "ear", "nose", "part_", "sway"];

/// Whether an id names a part.
pub fn is_part_name(id: &str) -> bool {
    Joint::from_name(id).is_some()
        || matches!(id, "eyes" | "brows" | "mouth" | "costume" | "mic" | "prop")
        || id.starts_with("prop_")
        || Slot::of_part(id).is_some()
        || EXTRA_PREFIXES.iter().any(|p| id.starts_with(p))
}

fn paint(paint: &Option<SvgPaint>, style: &SvgStyle, alpha: f32, doc: &makepad_svg::document::SvgDocument) -> Option<Color> {
    let c = match paint.as_ref()? {
        SvgPaint::None => return None,
        SvgPaint::Color(r, g, b, a) => [*r, *g, *b, *a],
        SvgPaint::CurrentColor => [style.color.0, style.color.1, style.color.2, style.color.3],
        // A gradient becomes its middle stop's colour.
        SvgPaint::GradientRef(id) => {
            let stops = &doc.defs.gradients.get(id)?.stops;
            let s = stops.get(stops.len() / 2)?;
            let a = s.color[3].max(1e-6);
            [s.color[0] / a, s.color[1] / a, s.color[2] / a, s.color[3]]
        }
    };
    Some([c[0] as f64, c[1] as f64, c[2] as f64, (c[3] * alpha * style.opacity) as f64])
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

fn node_style(node: &SvgNode) -> Option<&SvgStyle> {
    Some(match node {
        SvgNode::Path(n) => &n.style,
        SvgNode::Rect(n) => &n.style,
        SvgNode::Circle(n) => &n.style,
        SvgNode::Ellipse(n) => &n.style,
        SvgNode::Line(n) => &n.style,
        SvgNode::Polyline(n) => &n.style,
        SvgNode::Polygon(n) => &n.style,
        SvgNode::Group(_) | SvgNode::Use(_) => return None,
    })
}

fn transformed(path: &VectorPath, xf: &Transform2d) -> VectorPath {
    let mut out = VectorPath::new();
    for cmd in &path.cmds {
        out.cmds.push(match cmd {
            PathCmd::MoveTo(x, y) => {
                let (x, y) = xf.apply(*x, *y);
                PathCmd::MoveTo(x, y)
            }
            PathCmd::LineTo(x, y) => {
                let (x, y) = xf.apply(*x, *y);
                PathCmd::LineTo(x, y)
            }
            PathCmd::BezierTo(a, b, c, d, x, y) => {
                let (a, b) = xf.apply(*a, *b);
                let (c, d) = xf.apply(*c, *d);
                let (x, y) = xf.apply(*x, *y);
                PathCmd::BezierTo(a, b, c, d, x, y)
            }
            other => other.clone(),
        });
    }
    out
}

struct Reader<'a> {
    doc: &'a makepad_svg::document::SvgDocument,
    parts: Vec<Part>,
    pivots: Vec<(String, [f64; 2])>,
    loose: usize,
}

impl Reader<'_> {
    fn add_part(&mut self, name: &str, parent: Option<usize>) -> usize {
        if let Some(at) = self.parts.iter().position(|p| p.name == name) {
            return at;
        }
        let z = self.parts.len() as f64;
        self.parts.push(Part { name: name.into(), parent, pivot: [f64::NAN, f64::NAN], z, shapes: Vec::new(), variant: None, joint: None, sways: false });
        self.parts.len() - 1
    }

    fn walk(&mut self, nodes: &[SvgNode], acc: &Transform2d, current: Option<usize>) {
        for node in nodes {
            let id = node_id(node).map(str::to_string);
            if let Some(name) = id.as_deref().and_then(|id| id.strip_prefix("pivot_")) {
                let at = match node {
                    SvgNode::Circle(c) => Some(c.transform.then(acc).apply(c.cx, c.cy)),
                    SvgNode::Ellipse(e) => Some(e.transform.then(acc).apply(e.cx, e.cy)),
                    other => crate::icons::node_path(other).and_then(|(xf, p)| {
                        let b = crate::geometry::bounds(&crate::geometry::parse_path(&crate::icons::path_data(&transformed(&p, &xf.then(acc)))))?;
                        Some((((b[0] + b[2]) * 0.5) as f32, ((b[1] + b[3]) * 0.5) as f32))
                    }),
                };
                if let Some((x, y)) = at {
                    self.pivots.push((name.to_string(), [x as f64, y as f64]));
                }
                continue;
            }
            let own = id.as_deref().filter(|id| is_part_name(id)).map(|name| self.add_part(name, current));
            match node {
                SvgNode::Group(g) => {
                    let acc = g.transform.then(acc);
                    self.walk(&g.children, &acc, own.or(current));
                }
                SvgNode::Use(_) => {}
                shape => {
                    let target = match own.or(current) {
                        Some(at) => at,
                        None => {
                            self.loose += 1;
                            self.add_part("part_unrigged", None)
                        }
                    };
                    if let Some(s) = self.shape(shape, acc) {
                        self.parts[target].shapes.push(s);
                    }
                }
            }
        }
    }

    fn shape(&self, node: &SvgNode, acc: &Transform2d) -> Option<Shape> {
        let style = node_style(node)?;
        let (xf, path) = crate::icons::node_path(node)?;
        let full = xf.then(acc);
        let scale = (full.a * full.d - full.b * full.c).abs().sqrt() as f64;
        let fill = paint(&style.fill, style, style.fill_opacity, self.doc);
        let stroke = paint(&style.stroke, style, style.stroke_opacity, self.doc).filter(|_| style.stroke_width > 0.0);
        let axis = full.b.abs() < 1e-6 && full.c.abs() < 1e-6;
        let ap = |x: f32, y: f32| {
            let (x, y) = full.apply(x, y);
            [x as f64, y as f64]
        };
        let geom = match node {
            SvgNode::Circle(c) if axis => Geom::Ellipse { center: ap(c.cx, c.cy), radius: [(c.r * full.a).abs() as f64, (c.r * full.d).abs() as f64] },
            SvgNode::Ellipse(e) if axis => Geom::Ellipse { center: ap(e.cx, e.cy), radius: [(e.rx * full.a).abs() as f64, (e.ry * full.d).abs() as f64] },
            SvgNode::Rect(r) if axis && (r.rx - r.ry).abs() < 1e-3 && (full.a - full.d).abs() < 1e-4 => {
                let p0 = ap(r.x, r.y);
                let p1 = ap(r.x + r.width, r.y + r.height);
                Geom::Rect { pos: [p0[0].min(p1[0]), p0[1].min(p1[1])], size: [(p1[0] - p0[0]).abs(), (p1[1] - p0[1]).abs()], radius: (r.rx.max(r.ry) * full.a.abs()) as f64 }
            }
            _ => Geom::Path(transformed(&path, &full).into()),
        };
        let cap = match style.stroke_linecap {
            LineCap::Butt => Cap::Butt,
            LineCap::Square => Cap::Square,
            _ => Cap::Round,
        };
        let join = match style.stroke_linejoin {
            LineJoin::Miter => Join::Miter,
            LineJoin::Bevel => Join::Bevel,
            _ => Join::Round,
        };
        (fill.is_some() || stroke.is_some()).then(|| Shape { geom, fill, stroke, width: style.stroke_width as f64 * scale, cap, join })
    }
}

/// A part's default pivot from its bounds, by what it is.
fn default_pivot(name: &str, b: [f64; 4]) -> [f64; 2] {
    let (cx, cy) = ((b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5);
    match Joint::from_name(name) {
        Some(Joint::Head | Joint::Torso) => [cx, b[3]],
        Some(Joint::Hips) => [cx, cy],
        Some(_) => [cx, b[1]],
        None if name.starts_with("hair") || name.starts_with("antenna") || name.starts_with("tail") => [cx, b[1]],
        None => [cx, cy],
    }
}

/// A rig from a character SVG, and warnings about what it lacks (missing
/// visemes, no blink, no legs to walk on).
pub fn from_svg(markup: &str) -> Result<(Rig, Vec<String>), String> {
    let doc = makepad_svg::parse_svg(markup);
    if doc.root.is_empty() {
        return Err("the character SVG has no elements (is it `<svg …>…</svg>`?)".into());
    }
    let mut reader = Reader { doc: &doc, parts: Vec::new(), pivots: Vec::new(), loose: 0 };
    reader.walk(&doc.root, &Transform2d::identity(), None);
    let Reader { mut parts, pivots, loose, .. } = reader;
    if parts.iter().all(|p| p.name == "part_unrigged") {
        return Err("no element has a part id: give the groups ids like `head`, `torso`, `arm_r_upper`, `mouth_AHA` (the Character docs list them)".into());
    }
    let mut warnings = Vec::new();
    if loose > 0 {
        warnings.push(format!("{loose} shapes are in no part: they stay with the torso"));
    }
    // Pivots: the markers, else from the bounds of the part and what it holds.
    let n = parts.len();
    let own: Vec<Option<[f64; 4]>> = parts.iter().map(|p| super::shapes_bounds(&p.shapes)).collect();
    for i in 0..n {
        if let Some((_, p)) = pivots.iter().find(|(name, _)| *name == parts[i].name) {
            parts[i].pivot = *p;
            continue;
        }
        let mut b = own[i];
        if b.is_none() {
            // A group of parts: the bounds of its descendants.
            for j in 0..n {
                let mut at = parts[j].parent;
                while let Some(p) = at {
                    if p == i {
                        if let Some(c) = own[j] {
                            b = Some(match b {
                                Some(b) => [b[0].min(c[0]), b[1].min(c[1]), b[2].max(c[2]), b[3].max(c[3])],
                                None => c,
                            });
                        }
                        break;
                    }
                    at = parts[p].parent;
                }
            }
        }
        parts[i].pivot = b.map_or([0.0, 0.0], |b| default_pivot(&parts[i].name, b));
    }
    let mut rig = Rig { name: String::new(), parts, ground: 0.0, center: 0.0, height: 1.0, soles: Vec::new(), ink: [0.07, 0.07, 0.08, 1.0] }.finish();
    // Ground: the feet's bottom, else the drawing's.
    let all: Vec<Shape> = rig.parts.iter().filter(|p| p.variant.is_none() && p.name != "mic" && p.name != "prop").flat_map(|p| p.shapes.iter().cloned()).collect();
    let bounds = super::shapes_bounds(&all).ok_or("the character SVG draws nothing")?;
    rig.ground = rig.soles.iter().map(|(_, p)| p[1]).fold(f64::MIN, f64::max);
    if rig.soles.is_empty() {
        rig.ground = bounds[3];
        warnings.push("no `foot_l`/`foot_r`: the character stands on its lowest point and its walk will slide".into());
    }
    rig.center = match rig.find("hips").or_else(|| rig.find("torso")) {
        Some(at) => rig.parts[at].pivot[0],
        None => (bounds[0] + bounds[2]) * 0.5,
    };
    rig.height = (rig.ground - bounds[1]).max(1e-3);
    let missing: Vec<&str> = super::lipsync::Viseme::ALL.iter().map(|v| v.name()).filter(|v| !rig.variants(Slot::Mouth).iter().any(|m| m.eq_ignore_ascii_case(v))).collect();
    if !missing.is_empty() {
        warnings.push(format!("no mouth shapes {}: lip sync uses the nearest ones it has", missing.iter().map(|m| format!("`mouth_{m}`")).collect::<Vec<_>>().join(" ")));
    }
    if !rig.variants(Slot::Eyes).iter().any(|e| *e == "closed") {
        warnings.push("no `eyes_closed`: the character cannot blink".into());
    }
    for joint in [Joint::Head, Joint::ArmRU, Joint::ArmRL, Joint::LegLU, Joint::LegRU] {
        if rig.joint_part(joint).is_none() {
            warnings.push(format!("no `{}`: poses and walks cannot move it", joint.name()));
        }
    }
    Ok((rig, warnings))
}

fn hex(c: &Color) -> String {
    let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]))
}

fn n(v: f64) -> String {
    let r = (v * 100.0).round() / 100.0;
    if r == r.trunc() { format!("{}", r as i64) } else { format!("{r}") }
}

fn style_attrs(shape: &Shape) -> String {
    let mut s = String::new();
    match &shape.fill {
        Some(c) => {
            s.push_str(&format!(" fill=\"{}\"", hex(c)));
            if c[3] < 1.0 {
                s.push_str(&format!(" fill-opacity=\"{}\"", n(c[3])));
            }
        }
        None => s.push_str(" fill=\"none\""),
    }
    if let Some(c) = &shape.stroke {
        s.push_str(&format!(" stroke=\"{}\" stroke-width=\"{}\"", hex(c), n(shape.width)));
        if c[3] < 1.0 {
            s.push_str(&format!(" stroke-opacity=\"{}\"", n(c[3])));
        }
        let cap = match shape.cap {
            Cap::Round => "round",
            Cap::Butt => "butt",
            Cap::Square => "square",
        };
        let join = match shape.join {
            Join::Round => "round",
            Join::Miter => "miter",
            Join::Bevel => "bevel",
        };
        s.push_str(&format!(" stroke-linecap=\"{cap}\" stroke-linejoin=\"{join}\""));
    }
    s
}

/// The rig as a character SVG: one group per part in draw order, pivot
/// markers, every face variant (they overlap: hide the ones you are not
/// editing). Imports back to the same rig.
pub fn to_svg(rig: &Rig) -> String {
    let all: Vec<Shape> = rig.parts.iter().flat_map(|p| p.shapes.iter().cloned()).collect();
    let b = super::shapes_bounds(&all).unwrap_or([0.0, -1000.0, 1.0, 0.0]);
    let pad = 20.0;
    let (x, y, w, h) = (b[0] - pad, b[1] - pad, b[2] - b[0] + 2.0 * pad, b[3] - b[1] + 2.0 * pad);
    let mut out = format!("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"{} {} {} {}\" width=\"{}\" height=\"{}\">\n", n(x), n(y), n(w), n(h), n(w), n(h));
    let mut order: Vec<usize> = (0..rig.parts.len()).collect();
    order.sort_by(|&a, &b| rig.parts[a].z.partial_cmp(&rig.parts[b].z).unwrap_or(std::cmp::Ordering::Equal).then(a.cmp(&b)));
    for i in order {
        let part = &rig.parts[i];
        out.push_str(&format!("  <g id=\"{}\">\n", part.name));
        for shape in &part.shapes {
            let attrs = style_attrs(shape);
            let el = match &shape.geom {
                Geom::Path(path) => format!("<path d=\"{}\"{attrs}/>", crate::icons::path_data(path)),
                Geom::Ellipse { center, radius } => format!("<ellipse cx=\"{}\" cy=\"{}\" rx=\"{}\" ry=\"{}\"{attrs}/>", n(center[0]), n(center[1]), n(radius[0]), n(radius[1])),
                Geom::Rect { pos, size, radius } => format!("<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" rx=\"{}\"{attrs}/>", n(pos[0]), n(pos[1]), n(size[0]), n(size[1]), n(*radius)),
                Geom::Capsule { a, b, width } => {
                    let r = width * 0.5;
                    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                    let length = (dx * dx + dy * dy).sqrt();
                    if dx.abs() < 1e-9 {
                        let top = a[1].min(b[1]);
                        format!("<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" rx=\"{}\"{attrs}/>", n(a[0] - r), n(top - r), n(*width), n(length + width), n(r))
                    } else {
                        let angle = dy.atan2(dx).to_degrees();
                        format!(
                            "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" rx=\"{}\" transform=\"rotate({} {} {})\"{attrs}/>",
                            n(a[0] - r), n(a[1] - r), n(length + width), n(*width), n(r), n(angle), n(a[0]), n(a[1])
                        )
                    }
                }
            };
            out.push_str(&format!("    {el}\n"));
        }
        out.push_str(&format!("    <circle id=\"pivot_{}\" cx=\"{}\" cy=\"{}\" r=\"0\" fill=\"none\"/>\n", part.name, n(part.pivot[0]), n(part.pivot[1])));
        out.push_str("  </g>\n");
    }
    out.push_str("</svg>\n");
    out
}
