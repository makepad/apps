//! `fab.select`: what the person pointed at in the 3D view, each pick also a chip on their chat
//! line, read and set by the AI so it can show what it means. A pick says what to write (an
//! edit's binding, the statement that joins two edits at a crease, the value sites that size or
//! round them), what it is (builder, size, the side pointed at, a crease's angle and radius, the
//! wall there) and where (point and normal in the model frame). Picks are view state, not
//! document: after each build they are described again by their names, and a pick whose solid is
//! gone drops with a note.
//!
//! On a part drawn from its field a pick is an edit (the binding whose surface wins there), a
//! crease (where two edits meet: the pair, standing for the op that joins them), an edit's own
//! edges (its rounding) or a spot. On an exact part (`m.brep`) it is a face, an edge (where two
//! faces meet) or its tangent chain, each by the names a script writes for it (`b.top`,
//! `m.edge(b.top, b.front)`), found again by them after a build; or the feature a face comes
//! from. On any other part it is a feature (the smallest solid of the part round the point) or a
//! spot. The whole printed object is a body.
use crate::out::{arr, int, mm, mm3, n, obj, s, S};
use crate::surface::{add_scaled, dot, gradient, number, onto, slab, trace_field, unit, v};
use crate::{frame_scale, inspect, point_arg, str_arg, Reply, Session};
use fab_model::params::{xf_dir, xf_point, PValue};
use makepad_csg_sdf::{SdfNode, SdfOp};
use makepad_model::json::Value;
use std::sync::Arc;

type P3 = [f64; 3];

/// The tool's name, for the AI and for the view's own picks.
pub const TOOL: &str = "select";

/// How many pixels from the pointer a crease or an edge still counts as pointed at.
const NEAR_PX: f64 = 6.0;
/// How far the surface's normal turns within that distance at an edit's own edge (degrees).
const EDGE_TURN: f64 = 30.0;
/// The most values a description lists per solid.
const VALUES: usize = 6;
/// The longest source line a description quotes.
const LINE_CHARS: usize = 100;
/// How far in the wall is looked for (mm).
const WALL_MAX: f64 = 400.0;

/// What a pick stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The binding whose surface wins there.
    Edit,
    /// Where two edits meet: the op that joins them.
    Crease,
    /// An edit's own edges: its rounding.
    Edges,
    /// A point on a surface.
    Spot,
    /// The smallest solid round the point, on a part not drawn from its field.
    Feature,
    /// The whole printed object.
    Body,
    /// An exact part's face.
    Face,
    /// An exact part's edge: where two faces meet.
    Edge,
    /// An exact part's edge and the edges continuing it tangent.
    Chain,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Edit => "edit",
            Kind::Crease => "crease",
            Kind::Edges => "edges",
            Kind::Spot => "spot",
            Kind::Feature => "feature",
            Kind::Body => "body",
            Kind::Face => "face",
            Kind::Edge => "edge",
            Kind::Chain => "chain",
        }
    }
    fn parse(t: &str) -> Option<Kind> {
        [Kind::Edit, Kind::Crease, Kind::Edges, Kind::Spot, Kind::Feature, Kind::Body, Kind::Face, Kind::Edge, Kind::Chain].into_iter().find(|k| k.name() == t)
    }
}

/// An exact pick's selector as the names it is made of, to write it with the names another place
/// in the code has for its parts ([`Written::text`]): a script fn's code names them its own way.
#[derive(Clone, Debug, PartialEq)]
pub struct Written {
    /// The id of each part its names come from, by the key of the part's step.
    pub parts: std::collections::HashMap<makepad_csg_graph::Key, String>,
    /// Direct Path references for the parts this selector uses in their original scope.
    pub sides: std::collections::HashMap<makepad_csg_graph::Key, Vec<Option<String>>>,
    pub names: WrittenNames,
}

#[derive(Clone, Debug, PartialEq)]
pub enum WrittenNames {
    Face(makepad_csg_graph::edges::FaceName),
    /// `m.edge(a, b)`: its two faces, the point near it, the chain.
    Edge([makepad_csg_graph::edges::FaceName; 2], Option<P3>, bool),
}

impl Written {
    fn new(b: &fab_model::eval::ExactBody, names: WrittenNames) -> Written {
        fn origins(n: &makepad_csg_graph::edges::FaceName, out: &mut Vec<makepad_csg_graph::Key>) {
            use makepad_csg_graph::edges::Role;
            out.push(n.origin);
            match &n.role {
                Role::Inner(Some(outer)) => origins(outer, out),
                Role::Blend(a, c) => {
                    origins(a, out);
                    origins(c, out);
                }
                Role::Corner(round) => round.iter().for_each(|x| origins(x, out)),
                _ => {}
            }
        }
        let mut keys = Vec::new();
        match &names {
            WrittenNames::Face(f) => origins(f, &mut keys),
            WrittenNames::Edge([a, c], ..) => {
                origins(a, &mut keys);
                origins(c, &mut keys);
            }
        }
        let parts: std::collections::HashMap<_, _> = keys.into_iter().filter_map(|k| Some((k, b.parts.get(&k)?.clone()))).collect();
        let sides = parts.keys().filter_map(|k| Some((*k, b.sides.get(k)?.clone()))).collect();
        Written { parts, sides, names }
    }

    /// The selector with each part's id written as `name` gives it. Err: the id of a part `name`
    /// gives none for.
    pub fn text(&self, name: impl Fn(&str) -> Option<String>) -> Result<String, String> {
        let mut parts = std::collections::HashMap::new();
        let mut sides = std::borrow::Cow::Borrowed(&self.sides);
        for (key, id) in &self.parts {
            let local = name(id).ok_or_else(|| id.clone())?;
            if local != *id && sides.contains_key(key) {
                sides.to_mut().remove(key);
            }
            parts.insert(*key, local);
        }
        Ok(match &self.names {
            WrittenNames::Face(f) => inspect::written(f, &parts, &sides),
            WrittenNames::Edge([a, c], near, chain) => {
                let mut pair = [inspect::written(a, &parts, &sides), inspect::written(c, &parts, &sides)];
                pair.sort();
                inspect::edge_text(&pair[0], &pair[1], *near, *chain)
            }
        })
    }
}

/// An exact part's face or edge as a script names it, and how a pick finds it again after a
/// build: a face's name (`b.top`), or the names of an edge's two faces.
#[derive(Clone, Debug, PartialEq)]
pub enum Named {
    Face(String),
    Edge([String; 2]),
}

/// One pick.
#[derive(Clone, Debug, PartialEq)]
pub struct Pick {
    /// Its name while it lasts (`p3`): what the chat's chip and `remove` say.
    pub id: String,
    pub kind: Kind,
    /// The printed object it is on (`instance/object`).
    pub object: String,
    /// The solid this pick resolves on (`instance/solid`). For a whole-part pick this is `object`.
    pub on: String,
    /// The solids it names (`instance/solid`): two for a crease, else one.
    pub solids: Vec<String>,
    /// Where it was pointed at, in `on`'s model frame: the point and the outward normal.
    pub at: Option<(P3, P3)>,
    /// The first solid's box in the model frame, to draw a pick without a point.
    pub bounds: Option<(P3, P3)>,
    /// The chip's words.
    pub label: String,
    /// What to write: the bindings, the statements and the value sites.
    pub write: Vec<String>,
    /// What it is.
    pub what: String,
    /// On an exact part, what a script writes for it: `b.top`, `m.edge(b.top, b.front)`.
    pub selector: Option<String>,
    /// The same as the names it is made of, to write it where the code names its parts otherwise.
    pub written: Option<Written>,
    /// How it is found again after a build, on an exact part.
    pub named: Option<Named>,
    /// What the view draws of it, in `on`'s model frame: an edge's curve, a face's outline.
    pub outline: Vec<Vec<P3>>,
    /// On a part drawn from its field, the features the view tints and how strongly
    /// ([`makepad_csg_sdf::SdfNode::Id`]): an edit's, or a crease's two at half.
    pub tint: Vec<(u32, f32)>,
    /// A crease's way at its point (model frame, unit): the view draws a short tick along it.
    pub tick: Option<(P3, P3)>,
}

impl Pick {
    /// What the model is told with the person's line.
    pub fn detail(&self) -> String {
        let mut out = format!("write: {}. {}", self.write.join("; "), self.what);
        if self.on != self.object {
            out.push_str(&format!(" Picked on step `{}`.", short(&self.on)));
        }
        if let Some((p, nm)) = self.at {
            out.push_str(&format!(" At [{:.2} {:.2} {:.2}] mm, normal [{:.3} {:.3} {:.3}], in {}'s model frame (Y up).", p[0], p[1], p[2], nm[0], nm[1], nm[2], self.on));
        }
        out
    }

    fn value(&self) -> S {
        let mut f = vec![("id", s(&self.id)), ("kind", s(self.kind.name())), ("label", s(&self.label)), ("object", s(&self.object)), ("solids", arr(self.solids.iter().map(s))), ("write", arr(self.write.iter().map(s))), ("what", s(&self.what))];
        if self.on != self.object {
            f.push(("on", s(&self.on)));
        }
        if let Some(sel) = &self.selector {
            f.push(("select", s(sel)));
        }
        if let Some((p, nm)) = self.at {
            f.push(("point", mm3(p)));
            f.push(("normal", arr(nm.iter().map(|x| n(*x)))));
        }
        obj(f)
    }
}

/// The viewport's installed step surface hit, in the step's model frame (mm).
#[derive(Clone, Debug)]
pub struct ViewHit {
    pub object: String,
    pub on: String,
    pub at: P3,
    pub normal: P3,
    /// Model displacement per plate mm along the viewing ray.
    pub dir: P3,
    pub depth: f64,
    /// Model mm per pixel at the hit.
    pub pixel: f64,
}

/// Private host operations for the person's view.
pub enum ViewPick {
    Point { hit: Option<ViewHit>, kind: String, gesture: Gesture },
    Select(Value),
    Retain(Option<String>),
}

#[derive(Clone, Copy)]
pub enum Gesture {
    Click { add: bool },
    Hover,
    Along,
}

/// The picks of the document on the bed.
#[derive(Default)]
pub struct Picks {
    pub list: Vec<Pick>,
    /// Moves whenever the list or a pick's description changes.
    pub rev: u64,
    next: u64,
    /// The build they were described on, and the document.
    on: Option<Arc<fab_model::eval::Evaluated>>,
    switched: u64,
    /// The picks a build dropped since the AI last read them: why.
    dropped: Vec<String>,
    /// The exact bodies read on the build `on`, by resolved solid (`None`: not exact).
    exact: Vec<(String, Option<Arc<fab_model::eval::ExactBody>>)>,
    /// Those read on the build before, until the picks follow the newer one: what a renamed
    /// solid's key was called.
    before: Vec<(String, Option<Arc<fab_model::eval::ExactBody>>)>,
}

/// What is asked for: a pick named by the AI, or by the view's ray.
#[derive(Clone, Debug)]
struct Want {
    kind: Kind,
    on: String,
    solids: Vec<String>,
    /// A point in `on`'s model frame to snap onto its surface.
    at: Option<P3>,
    /// On the exact solid `on`, the face or edge by its names.
    named: Option<Named>,
}

/// One Id of a field at a point: its tag, its subtree's distance (the field's frame), the tags
/// round it (outermost first), the Combines above it (their address, which operand, op, blend),
/// and its subtree in the field's frame.
struct IdAt {
    id: u32,
    d: f64,
    within: Vec<u32>,
    joins: Vec<(usize, usize, SdfOp, f64)>,
    node: SdfNode,
}

/// The solid face `face` of exact solid `on` comes from: the part of the name a pick
/// takes (`b` of `b.top`, the fillet of a blend), else of its outermost part's name (an
/// anonymous one).
fn exact_feature(b: &fab_model::eval::ExactBody, face: u32, on: &str) -> Option<String> {
    let names = &b.faces.iter().find(|f| f.id == face)?.names;
    let outer = || names.iter().filter_map(|n| b.order.iter().position(|k| *k == n.origin)).min().map(|i| b.order[i]);
    let key = inspect::face_names(b, face).first().map(|n| n.origin).or_else(outer)?;
    let id = b.parts.get(&key)?;
    Some(match on.rsplit_once('/') {
        Some((instance, _)) => fab_model::program::part_id(instance, id),
        None => id.clone(),
    })
}

/// A selector as `fab.exact` writes it: a face's name (`b.top`), or `m.edge(a, b)` with its
/// options (`{near: [x y z] chain: true}`): the names, the chain, the point near.
fn parse_selector(text: &str) -> Result<(Named, bool, Option<P3>), String> {
    let t = text.trim();
    let Some(inner) = t.strip_prefix("m.edge(").and_then(|r| r.strip_suffix(')')) else {
        if t.is_empty() || t.contains(char::is_whitespace) && !t.contains('(') {
            return Err(format!("'select' is a face's name (`b.top`) or `m.edge(a, b)`, not `{t}`"));
        }
        return Ok((Named::Face(t.to_string()), false, None));
    };
    // the arguments at the top level
    let (mut args, mut depth, mut from) = (Vec::new(), 0i32, 0);
    for (i, c) in inner.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                args.push(inner[from..i].trim());
                from = i + 1;
            }
            _ => {}
        }
    }
    args.push(inner[from..].trim());
    let (a, b) = match args[..] {
        [a, b] | [a, b, _] if !a.is_empty() && !b.is_empty() => (a, b),
        _ => return Err(format!("`m.edge(a, b)` names two faces: `{t}`")),
    };
    let mut pair = [a.to_string(), b.to_string()];
    pair.sort();
    let options = args.get(2).copied().unwrap_or("");
    let chain = options.split(|c: char| c == ',' || c == '{' || c == '}' || c.is_whitespace()).collect::<Vec<_>>().windows(2).any(|w| w[0] == "chain:" && w[1] == "true");
    let near = match options.find("near:") {
        Some(i) => {
            let rest = &options[i + 5..];
            let open = rest.find('[').ok_or("`near:` is a point: [x y z]")?;
            let close = rest.find(']').ok_or("`near:` is a point: [x y z]")?;
            let xs: Vec<f64> = rest[open + 1..close].split(|c: char| c == ',' || c.is_whitespace()).filter(|x| !x.is_empty()).map(|x| x.trim_end_matches("mm").parse::<f64>()).collect::<Result<_, _>>().map_err(|_| "`near:` is a point: [x y z] in mm")?;
            match xs[..] {
                [x, y, z] => Some([x, y, z]),
                _ => return Err("`near:` is a point: [x y z]".into()),
            }
        }
        None => None,
    };
    Ok((Named::Edge(pair), chain, near))
}

/// How far `p` is from a polyline.
pub(crate) fn polyline_distance(points: &[P3], p: P3) -> f64 {
    if let [only] = points {
        return dot(sub(p, *only), sub(p, *only)).sqrt();
    }
    points
        .windows(2)
        .map(|w| {
            let ab = sub(w[1], w[0]);
            let t = (dot(sub(p, w[0]), ab) / dot(ab, ab).max(1e-30)).clamp(0.0, 1.0);
            let q = add_scaled(w[0], ab, t);
            dot(sub(p, q), sub(p, q)).sqrt()
        })
        .fold(f64::MAX, f64::min)
}

fn sub(a: P3, b: P3) -> P3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

/// The node `inner` under the move, copies or warp `w` stands for.
fn rewrap(w: &SdfNode, inner: SdfNode) -> SdfNode {
    match w {
        SdfNode::Transform { at, rotate, scale, .. } => SdfNode::Transform { at: *at, rotate: *rotate, scale: *scale, of: Box::new(inner) },
        SdfNode::Instance { places, .. } => SdfNode::Instance { places: places.clone(), of: Box::new(inner) },
        SdfNode::Deform { deform, .. } => SdfNode::deform(*deform, inner),
        _ => inner,
    }
}

/// Every Id of field `root` read at `p`. Per pick, not per frame: a subtree is copied per Id
/// above it.
fn ids_at(root: &SdfNode, p: P3) -> Vec<IdAt> {
    fn walk<'a>(n: &'a SdfNode, wraps: &mut Vec<&'a SdfNode>, within: &mut Vec<u32>, joins: &mut Vec<(usize, usize, SdfOp, f64)>, p: P3, out: &mut Vec<IdAt>) {
        match n {
            SdfNode::Id { id, of } => {
                let node = wraps.iter().rev().fold(n.clone(), |inner, w| rewrap(w, inner));
                out.push(IdAt { id: *id, d: makepad_csg_sdf::distance(&node, v(p)), within: within.clone(), joins: joins.clone(), node });
                within.push(*id);
                walk(of, wraps, within, joins, p, out);
                within.pop();
            }
            SdfNode::Transform { of, .. } | SdfNode::Instance { of, .. } | SdfNode::Deform { of, .. } => {
                wraps.push(n);
                walk(of, wraps, within, joins, p, out);
                wraps.pop();
            }
            SdfNode::Combine { op, blend, of } => {
                for (k, c) in of.iter().enumerate() {
                    joins.push((n as *const SdfNode as usize, k, *op, *blend));
                    walk(c, wraps, within, joins, p, out);
                    joins.pop();
                }
            }
            SdfNode::Material { of, .. } | SdfNode::Bone { of, .. } | SdfNode::Shell { of, .. } | SdfNode::Round { of, .. } | SdfNode::Sculpt { of, .. } | SdfNode::Texture { of, .. } => walk(of, wraps, within, joins, p, out),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(root, &mut Vec::new(), &mut Vec::new(), &mut Vec::new(), p, &mut out);
    out
}

/// What a field shows at a surface point: the edit that wins, the nearest other edit with how far
/// its surface is, the op joining the two (op and blend), and whether the surface turns sharply
/// within `tol` there (an edit's own edge).
struct FieldAt {
    winner: Option<u32>,
    runner: Option<(u32, f64)>,
    joint: Option<(SdfOp, f64)>,
    /// The two edits' own outward normals at the point.
    normals: Option<(P3, P3)>,
    sharp: bool,
}

fn field_at(root: &SdfNode, p: P3, normal: P3, tol: f64) -> FieldAt {
    let winner = makepad_csg_sdf::distance_id(root, v(p)).1;
    let ids = ids_at(root, p);
    let w = winner.and_then(|w| ids.iter().find(|e| e.id == w));
    let runner = w.and_then(|w| ids.iter().filter(|e| e.id != w.id && !w.within.contains(&e.id)).min_by(|a, b| a.d.abs().total_cmp(&b.d.abs())));
    let joint = match (w, runner) {
        (Some(w), Some(r)) => w.joins.iter().zip(&r.joins).take_while(|(a, b)| a.0 == b.0).find(|(a, b)| a.1 != b.1).map(|(a, _)| (a.2, a.3)),
        _ => None,
    };
    let normals = match (w, runner) {
        (Some(w), Some(r)) => match (unit(gradient(&w.node, p)), unit(gradient(&r.node, p))) {
            // a cutter's surface faces into its hole
            (Some(a), Some(b)) => Some((a, if matches!(joint, Some((SdfOp::Difference, _))) { b.map(|x| -x) } else { b })),
            _ => None,
        },
        _ => None,
    };
    // An edge of the winner's own: round the point, at `tol`, the surface (still the winner's)
    // faces another way.
    let sharp = winner.is_some() && {
        let side = unit(if normal[0].abs() < 0.9 { [0.0, -normal[2], normal[1]] } else { [normal[2], 0.0, -normal[0]] }).unwrap_or([1.0, 0.0, 0.0]);
        let other = [normal[1] * side[2] - normal[2] * side[1], normal[2] * side[0] - normal[0] * side[2], normal[0] * side[1] - normal[1] * side[0]];
        (0..8).any(|k| {
            let a = k as f64 * std::f64::consts::FRAC_PI_4;
            let q = onto(root, add_scaled(add_scaled(p, side, tol * a.cos()), other, tol * a.sin()));
            makepad_csg_sdf::distance_id(root, v(q)).1 == winner && unit(gradient(root, q)).is_some_and(|m| dot(m, normal).clamp(-1.0, 1.0).acos().to_degrees() > EDGE_TURN)
        })
    };
    FieldAt { winner, runner: runner.map(|r| (r.id, r.d)), joint, normals, sharp }
}

/// How far into field `root` the wall at `p` goes along `-normal` (mm), when it ends in reach.
fn wall(root: &SdfNode, p: P3, normal: P3) -> Option<f64> {
    let mut t = 1e-3;
    while t < WALL_MAX {
        let d = makepad_csg_sdf::distance(root, v(add_scaled(p, normal, -t)));
        if d > 0.0 {
            return Some(t);
        }
        t += d.abs().max(1e-2);
    }
    None
}

/// The side of box `lo..hi` a point on it with outward `normal` is on, in words: "its top (+Y),
/// toward its -X end".
fn side_words(lo: P3, hi: P3, p: P3, normal: P3) -> String {
    const AXES: [char; 3] = ['X', 'Y', 'Z'];
    let k = (0..3).max_by(|a, b| normal[*a].abs().total_cmp(&normal[*b].abs())).unwrap();
    let up = normal[k] >= 0.0;
    let face = match (k, up) {
        (1, true) => "top (+Y)".to_string(),
        (1, false) => "bottom (-Y)".to_string(),
        (0, true) => "+X side".to_string(),
        (0, false) => "-X side".to_string(),
        (_, true) => "+Z side (front)".to_string(),
        (_, false) => "-Z side (back)".to_string(),
    };
    let toward: Vec<String> = (0..3)
        .filter(|a| *a != k && hi[*a] - lo[*a] > 1e-6)
        .filter_map(|a| {
            let f = (p[a] - lo[a]) / (hi[a] - lo[a]);
            if f < 1.0 / 3.0 {
                Some(format!("-{}", AXES[a]))
            } else if f > 2.0 / 3.0 {
                Some(format!("+{}", AXES[a]))
            } else {
                None
            }
        })
        .collect();
    match toward.as_slice() {
        [] => format!("its {face}, near the middle"),
        [one] => format!("its {face}, toward its {one} end"),
        _ => format!("its {face}, toward its {} corner", toward.join(" ")),
    }
}

/// What a description says of one solid.
pub(crate) struct Facts {
    pub(crate) id: String,
    pub(crate) short: String,
    pub(crate) builder: String,
    /// `file:line` and the line itself.
    statement: Option<(String, String)>,
    bounds: Option<(P3, P3)>,
    values: Vec<String>,
    /// The solid it is part of, then that one's, outward.
    pub(crate) parents: Vec<String>,
    /// The printed object it is in, and that part's index on the plate.
    object: String,
    part: usize,
    /// Its feature's id in its part's field ([`makepad_csg_sdf::part_id`]).
    tag: u32,
}

fn short(id: &str) -> &str {
    id.rsplit_once('/').map_or(id, |(_, s)| s)
}

fn value_text(p: &fab_model::params::Param) -> Option<String> {
    let r = |x: f64| (x * 1000.0).round() / 1000.0;
    let v = match &p.value {
        PValue::Num(x) => format!("{}{}", r(*x), p.unit),
        PValue::Bool(b) => format!("{b}"),
        PValue::Choice(i) => p.choices.get(*i).map(|c| c.1.clone())?,
        PValue::Point(c) => format!("[{}]", c.iter().map(|x| format!("{}{}", r(*x), p.unit)).collect::<Vec<_>>().join(" ")),
        PValue::Profile(_) => return None,
    };
    Some(format!("{} = {v}", p.key))
}

fn tol_of(pixel: Option<(f64, f64)>, depth: f64, scale: f64) -> f64 {
    let (fixed, per_depth) = pixel.unwrap_or((0.0, 0.002));
    NEAR_PX * (fixed + per_depth * depth.max(0.0)) / scale.max(1e-9)
}

impl Session {
    /// The committed solid the tool's picks were resolved on.
    pub(crate) fn build_on(&self, picks: &[&Pick]) -> Result<(&fab_model::program::Instance, usize), String> {
        let first = picks.first().ok_or("no picks to build on")?;
        if let Some(other) = picks.iter().find(|p| p.on != first.on) {
            return Err(format!("the picks are on different steps: `{}` and `{}`", first.on, other.on));
        }
        self.compiled.as_ref().ok_or("nothing is built yet")?.part(&first.on).ok_or_else(|| format!("no solid '{}'", first.on))
    }

    /// Solid `name`'s facts for a description.
    pub(crate) fn facts(&mut self, name: &str) -> Result<Facts, String> {
        let located = self.locate(name)?;
        let (ev, c) = (self.shown_last().ok_or("nothing is built yet")?, self.shown_compiled().ok_or("nothing is built yet")?);
        let (inst, i) = c.part(&located.name).ok_or_else(|| format!("no solid '{}'", located.name))?;
        let program = &inst.script.program;
        let solid = &program.solids[i];
        let statement = solid.loc.map(|(line, _)| {
            let text = self.model.state().scripts.get(&inst.script.file).and_then(|t| t.lines().nth(line.saturating_sub(1) as usize)).unwrap_or("").trim();
            let text: String = text.chars().take(LINE_CHARS).collect();
            (format!("{}:{line}", inst.script.file), text)
        });
        let mut parents = Vec::new();
        let mut at = solid.parent;
        while let Some(j) = at {
            parents.push(fab_model::program::part_id(&inst.name, &program.solids[j].id));
            at = program.solids[j].parent;
        }
        let prefix = format!("{}.", located.name);
        // its sizes first, where it is placed last
        let mut values: Vec<(bool, String)> = self.params().map(|model| model.iter().filter(|pp| pp.part == located.part).flat_map(|pp| pp.params.iter()).filter(|p| p.key.starts_with(&prefix) && !p.derived).filter_map(|p| Some((p.key[prefix.len()..].starts_with("at"), value_text(p)?))).collect()).unwrap_or_default();
        values.sort_by_key(|v| v.0);
        let values = values.into_iter().map(|v| v.1).take(VALUES).collect();
        Ok(Facts {
            short: short(&located.name).to_string(),
            builder: solid.proto.to_lowercase(),
            statement,
            bounds: located.bounds,
            values,
            parents,
            object: ev.plate.parts[located.part].object.clone(),
            part: located.part,
            tag: makepad_csg_sdf::part_id(&solid.id),
            id: located.name,
        })
    }

    /// The field part `k` is drawn from (its model frame), when it is.
    fn part_field(&self, k: usize) -> Option<SdfNode> {
        self.shown_last()?.fields.get(k)?.as_ref().map(|f| f.shape.root.clone())
    }

    /// The tags of the solids of the instance printed object `object` is in, by name.
    fn tag_names(&self, object: &str) -> Vec<(u32, String)> {
        self.shown_compiled().as_ref().and_then(|c| c.part(object)).map(|(inst, _)| inst.script.program.solids.iter().map(|s| (makepad_csg_sdf::part_id(&s.id), fab_model::program::part_id(&inst.name, &s.id))).collect()).unwrap_or_default()
    }

    /// A pick of `want` described: what to write, what it is, where.
    fn describe(&mut self, want: &Want) -> Result<Pick, String> {
        self.describe_hit(want, None)
    }

    fn describe_hit(&mut self, want: &Want, normal: Option<P3>) -> Result<Pick, String> {
        let first = self.facts(&want.solids[0])?;
        let second = match want.solids.get(1) {
            Some(b) => Some(self.facts(b)?),
            None => None,
        };
        // A feature can also belong to another printed part. The pick belongs to the part hit.
        let part = if want.on == first.object { first.part } else { self.locate(&want.on)?.part };
        let object = self.shown_last().ok_or("nothing is built yet")?.plate.parts[part].object.clone();
        // A step supplies its own surface. Field features of a whole part snap to their individual solids.
        let surface = if want.on == object { &first.id } else { &want.on };
        let root = if want.on == object { self.part_field(part) } else { None };
        let at = match (want.at, normal) {
            (Some(p), Some(n)) => Some((p, n)),
            (Some(p), None) => {
                let q = match (want.kind, &root) {
                    (Kind::Crease | Kind::Body, Some(root)) => {
                        let q = onto(root, p);
                        Some((q, unit(gradient(root, q)).unwrap_or([0.0, 1.0, 0.0])))
                    }
                    _ => self.surface_query(surface).ok().and_then(|sq| sq.nearest_model(p)),
                };
                q.or(Some((p, [0.0, 1.0, 0.0])))
            }
            (None, _) => None,
        };
        let solids: Vec<String> = std::iter::once(first.id.clone()).chain(second.as_ref().map(|b| b.id.clone())).collect();
        let wall_there = match (&root, at) {
            (Some(root), Some((p, nm))) => wall(root, p, nm),
            _ => None,
        };
        let statement = |f: &Facts| f.statement.as_ref().map_or(String::new(), |(place, text)| format!(" at {place} `{text}`"));
        let values = |f: &Facts| if f.values.is_empty() { String::new() } else { format!("; values: {}", f.values.join(", ")) };
        let size = |f: &Facts| f.bounds.map_or(String::new(), |(lo, hi)| format!(" {:.1} x {:.1} x {:.1} mm", hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]));
        let side = |f: &Facts| match (f.bounds, at) {
            (Some((lo, hi)), Some((p, nm))) => format!(" Pointed at {}.", side_words(lo, hi, p, nm)),
            _ => String::new(),
        };
        let wall_words = wall_there.map_or(String::new(), |t| format!(" The wall there is about {t:.1} mm thick along the normal."));
        // on a part drawn from its field: the edits the view lights up
        let tint = match (want.kind, &root, &second) {
            (Kind::Edit | Kind::Edges, Some(_), _) => vec![(first.tag, 1.0)],
            (Kind::Crease, Some(_), Some(b)) => vec![(first.tag, 0.5), (b.tag, 0.5)],
            _ => Vec::new(),
        };
        let mut tick = None;
        let (label, write, what) = match want.kind {
            Kind::Edit | Kind::Feature => (
                first.short.clone(),
                vec![format!("`{}`{}{}", first.short, statement(&first), values(&first))],
                format!("{} `{}`: {}{}.{}{}", if want.kind == Kind::Edit { "The edit" } else { "The feature" }, first.short, first.builder, size(&first), side(&first), wall_words),
            ),
            Kind::Edges => {
                let rounding = first.values.iter().filter(|v| v.contains(".round =") || v.contains(".radius =")).cloned().collect::<Vec<_>>();
                (
                    first.short.clone(),
                    vec![format!("`{}`{}{}", first.short, statement(&first), values(&first))],
                    format!(
                        "The edges of `{}` ({}{}): a field edit's edges round all alike, by its rounding: {}.{}",
                        first.short,
                        first.builder,
                        size(&first),
                        if rounding.is_empty() { "it has no rounding value yet (sharp)".to_string() } else { rounding.join(", ") },
                        side(&first)
                    ),
                )
            }
            Kind::Crease => {
                let b = second.as_ref().ok_or("a crease names two solids")?;
                // the statement joining them: their nearest common solid
                let joint = first.parents.iter().find(|p| b.parents.contains(p)).cloned();
                let joint_facts = match &joint {
                    Some(j) => Some(self.facts(j)?),
                    None => None,
                };
                let mut write = Vec::new();
                match &joint_facts {
                    Some(j) => write.push(format!("joined by `{}`{}{}", j.short, statement(j), values(j))),
                    None => write.push(format!("they meet in `{}`, the object's own statement", short(&first.object))),
                }
                write.push(format!("`{}`{}", first.short, statement(&first)));
                write.push(format!("`{}`{}", b.short, statement(b)));
                let mut what = format!("The crease where `{}` and `{}` meet.", first.short, b.short);
                if let (Some(root), Some((p, nm))) = (&root, at) {
                    let tol = 1e-3;
                    let f = field_at(root, p, nm, tol);
                    // along the crease: across both surfaces' normals
                    tick = f.normals.and_then(|(a, c)| unit([a[1] * c[2] - a[2] * c[1], a[2] * c[0] - a[0] * c[2], a[0] * c[1] - a[1] * c[0]])).map(|d| (p, d));
                    if let Some((op, blend)) = f.joint {
                        // a union's crease is an inner corner; a cut's and an intersection's a rim
                        let (op, shape) = match op {
                            SdfOp::Union => ("a union", "concave: rounding it is a fillet"),
                            SdfOp::Difference => ("a cut", "convex: rounding it rounds the rim"),
                            SdfOp::Intersection => ("an intersection", "convex: rounding it rounds the rim"),
                        };
                        let sharp = if blend > 0.0 { format!("blended {blend:.2} mm") } else { "sharp".to_string() };
                        what.push_str(&format!(" They are joined by {op}, {sharp}; the crease is {shape}."));
                    }
                    if let Some((a, c)) = f.normals {
                        let turn = dot(a, c).clamp(-1.0, 1.0).acos().to_degrees();
                        what.push_str(&format!(" The two surfaces meet at about {:.0} deg.", 180.0 - turn));
                    }
                }
                what.push_str(&wall_words);
                (format!("{} + {}", first.short, b.short), write, what)
            }
            Kind::Spot => (
                format!("on {}", first.short),
                vec![format!("a point on `{}`{} (a new edit goes `at:` the point)", first.short, statement(&first))],
                format!("A spot on `{}` ({}{}).{}{}", first.short, first.builder, size(&first), side(&first), wall_words),
            ),
            Kind::Body => (
                first.short.clone(),
                vec![format!("`{}`{}{}", first.short, statement(&first), values(&first))],
                format!("The whole printed object `{}` ({}{}).{}", first.short, first.builder, size(&first), wall_words),
            ),
            Kind::Face | Kind::Edge | Kind::Chain => {
                let (label, write, what, (selector, written), outline) = self.describe_exact(want, &first, want.at)?;
                return Ok(Pick { id: String::new(), kind: want.kind, object, on: want.on.clone(), solids, at, bounds: first.bounds, label, write, what: format!("{what}{wall_words}"), selector: Some(selector), written, named: want.named.clone(), outline, tint, tick });
            }
        };
        Ok(Pick { id: String::new(), kind: want.kind, object, on: want.on.clone(), solids, at, bounds: first.bounds, label, write, what, selector: None, written: None, named: None, outline: Vec::new(), tint, tick })
    }

    /// What the view's ray (plate frame) points at, as `kind` asks (`None`: as it lands).
    /// `pixel`: plate mm per pixel at the eye and per mm of depth.
    fn want_at_ray(&mut self, from: P3, dir: P3, pixel: Option<(f64, f64)>, kind: Option<Kind>) -> Result<Option<Want>, String> {
        let Some(hit) = self.tracer()?.hit(from, dir) else { return Ok(None) };
        let ev = self.shown_last().ok_or("nothing is built yet")?;
        let to_plate = ev.fields.get(hit.part).and_then(|f| f.as_ref()).map_or(ev.model_to_plate[hit.part], |f| f.to_printer);
        let tol = tol_of(pixel, hit.depth, frame_scale(&to_plate));
        let at = Some(hit.point);
        let want = |kind, solids| Want { kind, on: hit.object.clone(), solids, at, named: None };
        let body = want(Kind::Body, vec![hit.object.clone()]);
        if kind == Some(Kind::Body) {
            return Ok(Some(body));
        }
        let Some(root) = self.part_field(hit.part) else {
            // an exact part: its faces and edges by their names
            if kind != Some(Kind::Spot) {
                if let Some(w) = self.exact_want(&hit.object, &hit.object, hit.point, kind, tol)? {
                    return Ok(Some(w));
                }
            }
            // a mesh part: the smallest of its solids round the point
            let feature = self.feature_at(hit.part, &hit.object, hit.point, tol);
            return Ok(Some(match (kind, feature) {
                (Some(Kind::Spot), f) => want(Kind::Spot, vec![f.unwrap_or(hit.object.clone())]),
                (Some(Kind::Crease | Kind::Edges | Kind::Edge | Kind::Chain), _) => return Err("creases and edges are picked on parts drawn from their field or exact (m.brep) parts; on this part pick a feature or a spot".into()),
                (_, Some(f)) => want(Kind::Feature, vec![f]),
                (_, None) => body,
            }));
        };
        let names = self.tag_names(&hit.object);
        let name = |tag: u32| names.iter().find(|(t, _)| *t == tag).map(|(_, n)| n.clone());
        let reach = if kind == Some(Kind::Crease) { 3.0 * tol } else { tol };
        let f = field_at(&root, hit.point, hit.normal, tol);
        let Some(winner) = f.winner.and_then(name) else {
            return Ok(Some(if kind == Some(Kind::Spot) { want(Kind::Spot, vec![hit.object.clone()]) } else { body }));
        };
        let crease = f.runner.filter(|(_, d)| d.abs() <= reach + f.joint.map_or(0.0, |j| j.1)).and_then(|(t, _)| name(t));
        // A field part has no faces: a face is the edit there, an edge the crease or its own edges.
        Ok(Some(match kind {
            Some(Kind::Spot) => want(Kind::Spot, vec![winner]),
            Some(Kind::Edit | Kind::Feature | Kind::Face) => want(Kind::Edit, vec![winner]),
            Some(Kind::Edges) => want(Kind::Edges, vec![winner]),
            Some(Kind::Crease) => match crease {
                Some(other) => want(Kind::Crease, vec![winner, other]),
                None => return Err(format!("no crease within {NEAR_PX} pixels: point nearer where two edits meet")),
            },
            Some(Kind::Edge | Kind::Chain) => match crease {
                Some(other) => want(Kind::Crease, vec![winner, other]),
                None if f.sharp => want(Kind::Edges, vec![winner]),
                None => return Err(format!("no crease or edge within {NEAR_PX} pixels: point nearer an edge")),
            },
            _ => match crease {
                Some(other) => want(Kind::Crease, vec![winner, other]),
                None if f.sharp => want(Kind::Edges, vec![winner]),
                None => want(Kind::Edit, vec![winner]),
            },
        }))
    }

    /// Solid `on`'s exact body on this build, read once per build; None when it is
    /// not exact.
    pub(crate) fn exact_body(&mut self, on: &str) -> Result<Option<Arc<fab_model::eval::ExactBody>>, String> {
        let current = match (&self.picks.on, &self.shown_last()) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        };
        if !current && !self.picks.exact.is_empty() {
            self.picks.before = std::mem::take(&mut self.picks.exact);
        }
        if let Some((_, b)) = self.picks.exact.iter().find(|(o, _)| o == on) {
            return Ok(b.clone());
        }
        let b = self.eval.object_exact(on)?.map(Arc::new);
        if current {
            self.picks.exact.push((on.to_string(), b.clone()));
        }
        Ok(b)
    }

    /// On exact solid `on`, what model point `p` points at as `kind` asks (`None`:
    /// as it lands: the edge within `tol` mm, else the face): a face or an edge by its names, its
    /// chain, or the feature the face comes from. None when the part is not exact, or its faces
    /// have no names (the pick is then a feature by its box).
    fn exact_want(&mut self, object: &str, on: &str, p: P3, kind: Option<Kind>, tol: f64) -> Result<Option<Want>, String> {
        let Some(b) = self.exact_body(on)? else { return Ok(None) };
        let edgy = matches!(kind, Some(Kind::Edge | Kind::Chain | Kind::Crease | Kind::Edges));
        let reach = if edgy { 3.0 * tol } else { tol };
        let Some(at) = self.eval.object_exact_at(on, p, reach)? else { return Ok(None) };
        let face = at.face.and_then(|f| inspect::face_pick(&b, f));
        let edge = at.edge.and_then(|id| b.edges.iter().find(|e| e.id == id)).and_then(|e| inspect::edge_pair(&b, e));
        let want = |kind: Kind, named: Named| Some(Want { kind, on: on.to_string(), solids: vec![object.to_string()], at: Some(p), named: Some(named) });
        Ok(match kind {
            Some(Kind::Body) => None,
            Some(Kind::Edit | Kind::Feature) => at.face.and_then(|f| exact_feature(&b, f, on)).map(|f| Want { kind: Kind::Feature, on: on.to_string(), solids: vec![f], at: Some(p), named: None }),
            Some(Kind::Face) => face.and_then(|f| want(Kind::Face, Named::Face(f))),
            Some(Kind::Chain) => match edge {
                Some(pair) => want(Kind::Chain, Named::Edge(pair)),
                None => return Err(format!("no edge within {} pixels: point nearer an edge", 3.0 * NEAR_PX)),
            },
            Some(_) if edgy => match edge {
                Some(pair) => want(Kind::Edge, Named::Edge(pair)),
                None => return Err(format!("no edge within {} pixels: point nearer an edge", 3.0 * NEAR_PX)),
            },
            _ => match (edge, face) {
                (Some(pair), _) => want(Kind::Edge, Named::Edge(pair)),
                (None, Some(f)) => want(Kind::Face, Named::Face(f)),
                _ => None,
            },
        })
    }

    /// An exact pick (face, edge or chain) found by its names on this build: what to write, what it
    /// is, its selector (as text and as names) and outline. `at`: where it was pointed at (it
    /// chooses among an edge's pieces).
    fn describe_exact(&mut self, want: &Want, first: &Facts, at: Option<P3>) -> Result<(String, Vec<String>, String, (String, Option<Written>), Vec<Vec<P3>>), String> {
        let b = self.exact_body(&want.on)?.ok_or_else(|| format!("`{}` is no longer an exact solid", short(&want.on)))?;
        let named = want.named.clone().ok_or("an exact pick without its names")?;
        let statement = |f: &Facts| f.statement.as_ref().map_or(String::new(), |(place, text)| format!(" at {place} `{text}`"));
        let of = format!("`{}`{}", first.short, statement(first));
        let feature_words = |me: &mut Session, face: u32| -> String {
            let Some(name) = exact_feature(&b, face, &want.on).filter(|f| *f != first.id) else { return String::new() };
            match me.facts(&name) {
                Ok(f) => format!("; it comes from `{}` ({}){}", f.short, f.builder, statement(&f)),
                Err(_) => String::new(),
            }
        };
        match named {
            Named::Face(name) => {
                let pieces: Vec<&makepad_csg_graph::BrepEntity> = b.faces.iter().filter(|f| inspect::face_holds(&b, f.id, &name)).collect();
                let Some(face) = pieces.first().copied() else { return Err(format!("`{}` has no face `{name}` any more", short(&want.on))) };
                let ids: Vec<u32> = pieces.iter().map(|f| f.id).collect();
                let outline = b.edges.iter().filter(|e| e.faces.iter().any(|f| ids.contains(f))).map(|e| e.points.clone()).collect();
                let area: f64 = pieces.iter().map(|f| f.size).sum();
                let normal = face.normal.map_or(String::new(), |n| format!(", facing [{:.2} {:.2} {:.2}]", n[0], n[1], n[2]));
                let split = if pieces.len() > 1 { format!(", in {} pieces", pieces.len()) } else { String::new() };
                let comes = feature_words(self, face.id);
                let write = vec![format!("`{name}`: a face of {of}{comes}. A shell opens it with `{{open: {name}}}`; an edge of it is `m.edge({name}, <the other face>)`")];
                let what = format!("The face `{name}` of `{}`: {}, {:.1} mm²{normal}{split}.", short(&want.on), face.kind, area);
                let written = inspect::face_named(&b, face.id, &name).map(|n| Written::new(&b, WrittenNames::Face(n.clone())));
                Ok((name.clone(), write, what, (name, written), outline))
            }
            Named::Edge(pair) => {
                let pieces = inspect::edges_between(&b, &pair[0], &pair[1]);
                let d = |e: &makepad_csg_graph::BrepEntity| at.map_or(0.0, |p| polyline_distance(&e.points, p));
                let Some(edge) = pieces.iter().copied().min_by(|x, y| d(x).total_cmp(&d(y))) else {
                    return Err(format!("`{}` and `{}` no longer meet on `{}`", pair[0], pair[1], short(&want.on)));
                };
                let chain = want.kind == Kind::Chain;
                let selector = inspect::edge_selector(&b, &pair, edge, chain);
                let edges: Vec<&makepad_csg_graph::BrepEntity> = if chain {
                    let ids = self.eval.object_exact_at(&want.on, edge.at, 1e-3)?.map(|a| a.chain).unwrap_or_default();
                    b.edges.iter().filter(|e| ids.contains(&e.id)).collect()
                } else {
                    vec![edge]
                };
                let outline = edges.iter().map(|e| e.points.clone()).collect();
                let comes = edge.faces.first().map_or(String::new(), |f| feature_words(self, *f));
                let radius = edge.radius.map_or(String::new(), |r| format!(" of radius {r:.2} mm"));
                let shape = format!("{} {:.1} mm long{radius}, {}", edge.kind, edge.size, if edge.convex { "convex (an outside edge)" } else { "concave (an inside corner)" });
                let (write, what) = if chain {
                    let length: f64 = edges.iter().map(|e| e.size).sum();
                    (
                        vec![format!("`{selector}`: {} edges of {of} continuing tangent from where `{}` meets `{}`{comes}. Round them all with `m.brep.fillet(<part>, <radius>, {{edges: {selector}}})`", edges.len(), pair[0], pair[1])],
                        format!("A chain of {} edges, {:.1} mm in all, starting at the edge where `{}` meets `{}` on `{}` ({shape}).", edges.len(), length, pair[0], pair[1], short(&want.on)),
                    )
                } else {
                    (
                        vec![format!("`{selector}`: an edge of {of}{comes}. Round or bevel it with `m.brep.fillet(<part>, <radius>, {{edges: {selector}}})` or `m.brep.chamfer`")],
                        format!("The edge where `{}` meets `{}` on `{}`: {shape}.", pair[0], pair[1], short(&want.on)),
                    )
                };
                let named = |text: &str| edge.faces.iter().find_map(|f| inspect::face_named(&b, *f, text)).cloned();
                let written = named(&pair[0]).zip(named(&pair[1])).map(|(a, c)| Written::new(&b, WrittenNames::Edge([a, c], inspect::edge_near(&b, [pair[0].as_str(), pair[1].as_str()], edge), chain)));
                Ok((pair.join(", "), write, what, (selector, written), outline))
            }
        }
    }

    /// The smallest solid of plate part `k` (not the object itself) whose box holds model point
    /// `p` (within `tol`).
    fn feature_at(&mut self, k: usize, object: &str, p: P3, tol: f64) -> Option<String> {
        let ev = self.shown_last()?;
        let model = self.params().ok()?;
        let pp = model.iter().find(|pp| pp.part == k)?;
        pp.objects
            .iter()
            .filter(|o| o.as_str() != object)
            .filter_map(|o| {
                let (lo, hi) = ev.objects.iter().find(|i| &i.name == o)?.bounds?;
                (0..3).all(|a| p[a] >= lo[a] - tol && p[a] <= hi[a] + tol).then(|| (o.clone(), (0..3).map(|a| (hi[a] - lo[a]).max(0.1)).product::<f64>()))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(o, _)| o)
    }

    /// Select Other: everything along the view's ray, front to back: the edits whose own surface
    /// it crosses (a field part's), or the features whose box it passes (any other's), the
    /// creases near where it lands, and the body. Each row as `add` takes it.
    fn along(&mut self, from: P3, dir: P3, pixel: Option<(f64, f64)>) -> Result<Vec<(Want, f64)>, String> {
        let Some(hit) = self.tracer()?.hit(from, dir) else { return Ok(Vec::new()) };
        let ev = self.shown_last().ok_or("nothing is built yet")?;
        let field = ev.fields.get(hit.part).and_then(|f| f.as_ref());
        let to_plate = field.map_or(ev.model_to_plate[hit.part], |f| f.to_printer);
        let to_model = fab_model::weave::invert(&to_plate);
        let (o, d) = (xf_point(&to_model, from), xf_dir(&to_model, dir));
        let scale = dot(d, d).sqrt();
        let d = d.map(|x| x / scale);
        let tol = tol_of(pixel, hit.depth, frame_scale(&to_plate));
        let mut rows: Vec<(Want, f64)> = Vec::new();
        match field {
            Some(f) => {
                let names = self.tag_names(&hit.object);
                let name = |tag: u32| names.iter().find(|(t, _)| *t == tag).map(|(_, n)| n.clone());
                for e in ids_at(&f.shape.root, hit.point) {
                    let Some(solid) = name(e.id) else { continue };
                    if rows.iter().any(|(w, _)| w.solids[0] == solid) {
                        continue;
                    }
                    if let Some(t) = trace_field(&e.node, o, d, f.shape.min, f.shape.max) {
                        rows.push((Want { on: hit.object.clone(), kind: Kind::Edit, solids: vec![solid], at: Some(add_scaled(o, d, t)), named: None }, t / scale));
                    }
                }
                rows.sort_by(|a, b| a.1.total_cmp(&b.1));
                let at = field_at(&f.shape.root, hit.point, hit.normal, tol);
                if let (Some(w), Some((r, dist))) = (at.winner.and_then(name), at.runner) {
                    if dist.abs() <= 3.0 * tol + at.joint.map_or(0.0, |j| j.1) {
                        if let Some(r) = name(r) {
                            rows.push((Want { on: hit.object.clone(), kind: Kind::Crease, solids: vec![w, r], at: Some(hit.point), named: None }, hit.depth));
                        }
                    }
                }
            }
            None => {
                // an exact part: the face there, the edge near it and its chain, first
                rows = self.exact_rows(&hit.object, &hit.object, hit.point, hit.depth, tol)?;
                let exact_rows = rows.len();
                let model = self.params()?;
                if let Some(pp) = model.iter().find(|pp| pp.part == hit.part) {
                    for solid in pp.objects.iter().filter(|s| **s != hit.object) {
                        let Some((lo, hi)) = ev.objects.iter().find(|i| &i.name == solid).and_then(|i| i.bounds) else { continue };
                        if let Some((t0, _)) = slab(o, d, lo, hi) {
                            rows.push((Want { on: hit.object.clone(), kind: Kind::Feature, solids: vec![solid.clone()], at: Some(add_scaled(o, d, t0.max(0.0))), named: None }, t0 / scale));
                        }
                    }
                }
                rows[exact_rows..].sort_by(|a, b| a.1.total_cmp(&b.1));
            }
        }
        rows.push((Want { on: hit.object.clone(), kind: Kind::Body, solids: vec![hit.object.clone()], at: Some(hit.point), named: None }, hit.depth));
        Ok(rows)
    }

    fn exact_rows(&mut self, object: &str, on: &str, at_point: P3, depth: f64, tol: f64) -> Result<Vec<(Want, f64)>, String> {
        let mut rows = Vec::new();
        if let Some(b) = self.exact_body(on)? {
            if let Some(at) = self.eval.object_exact_at(on, at_point, 3.0 * tol)? {
                let row = |kind: Kind, named: Named| (Want { on: on.to_string(), kind, solids: vec![object.to_string()], at: Some(at_point), named: Some(named) }, depth);
                if let Some(face) = at.face.and_then(|f| inspect::face_pick(&b, f)) {
                    rows.push(row(Kind::Face, Named::Face(face)));
                }
                if let Some(pair) = at.edge.and_then(|id| b.edges.iter().find(|e| e.id == id)).and_then(|e| inspect::edge_pair(&b, e)) {
                    rows.push(row(Kind::Edge, Named::Edge(pair.clone())));
                    if at.chain.len() > 1 {
                        rows.push(row(Kind::Chain, Named::Edge(pair)));
                    }
                }
                if on != object {
                    if let Some(id) = at.face.and_then(|f| exact_feature(&b, f, on)) {
                        rows.push((Want { kind: Kind::Feature, on: on.into(), solids: vec![id], at: Some(at_point), named: None }, depth));
                    }
                }
            }
        }
        Ok(rows)
    }

    /// Resolve a step relative to its printed object's instance, including on a later build.
    fn pick_step(&mut self, object: &str, step: &str) -> Result<String, String> {
        let (instance, _) = fab_model::program::split_id(object).ok_or_else(|| format!("no instance for '{object}'"))?;
        let on = if step.contains('/') { step.to_string() } else { fab_model::program::part_id(instance, step) };
        if on != object {
            let reason = || format!("'{on}' is not a step of '{object}'");
            let target = self.facts(&on).map_err(|e| format!("{}: {e}", reason()))?;
            let compiled = self.shown_compiled().ok_or("nothing is built yet")?;
            let (inst, body) = compiled.part(object).ok_or_else(reason)?;
            let solids = &inst.script.program.solids;
            let mut spine = std::iter::successors(Some(&solids[body]), |s| s.inputs.first().map(|&i| &solids[i]));
            if target.object != object || !spine.any(|s| s.id == target.short) {
                return Err(reason());
            }
        }
        if self.exact_body(&on)?.is_none() {
            return Err(format!("'{on}' of '{object}' is not an exact solid"));
        }
        Ok(on)
    }

    /// A pick as the AI or a row of Select Other names it: a solid's name (the edit, or on a
    /// part not drawn from its field the feature; the printed object itself is the body), two
    /// names (the crease where they meet), or `{solid, at, kind}` (`at`: a model point, snapped
    /// onto the surface; `kind` defaults to spot; `solids` for a crease).
    fn want_of(&mut self, v: &Value, view_row: bool) -> Result<Want, String> {
        let kind_for = |me: &Session, f: &Facts| {
            if f.id == f.object { Kind::Body } else if me.part_field(f.part).is_some() { Kind::Edit } else { Kind::Feature }
        };
        match v {
            Value::Str(name) => {
                let f = self.facts(name)?;
                Ok(Want { kind: kind_for(self, &f), on: f.object, solids: vec![f.id], at: None, named: None })
            }
            Value::Arr(pair) => {
                let names: Vec<String> = pair.iter().filter_map(|x| x.as_str().map(str::to_string)).collect();
                if names.len() != 2 || pair.len() != 2 {
                    return Err("a crease is two solids' names: [\"body\" \"knob\"]".into());
                }
                Ok(Want { kind: Kind::Crease, on: self.facts(&names[0])?.object, solids: names, at: None, named: None })
            }
            Value::Obj(_) => {
                let at = match v.get("at") {
                    Some(a) => Some(point_arg::<3>(a).map_err(|e| format!("'at': {e}"))?),
                    None => None,
                };
                let solids: Vec<String> = match (str_arg(v, "solid"), v.get("solids").and_then(Value::as_arr)) {
                    (Some(one), _) => vec![one.to_string()],
                    (None, Some(list)) => list.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
                    _ => return Err("a pick object names its 'solid' (or 'solids' for a crease)".into()),
                };
                let kind = match str_arg(v, "kind") {
                    Some(k) => Some(Kind::parse(k).ok_or_else(|| format!("'kind' is one of edit, crease, edges, spot, feature, body, face, edge, chain; not '{k}'"))?),
                    None => None,
                };
                let first = self.facts(solids.first().ok_or("a pick needs a solid")?)?;
                let on = match v.get("on") {
                    Some(step) => self.pick_step(&first.object, step.as_str().ok_or("'on' names a step of the printed object")?)?,
                    None => first.object.clone(),
                };
                // an exact part's face or edge by what a script writes for it
                if let Some(text) = str_arg(v, "select") {
                    let (named, chain, near) = parse_selector(text)?;
                    let kind = match (&named, kind) {
                        (Named::Face(_), None | Some(Kind::Face)) => Kind::Face,
                        (Named::Edge(_), None) => if chain { Kind::Chain } else { Kind::Edge },
                        (Named::Edge(_), Some(k @ (Kind::Edge | Kind::Chain))) => k,
                        (_, Some(k)) => return Err(format!("'select' `{text}` is not a {}", k.name())),
                    };
                    return Ok(Want { kind, on, solids: vec![first.object], at: near.or(at), named: Some(named) });
                }
                let kind = match kind {
                    Some(k) => k,
                    None if solids.len() == 2 => Kind::Crease,
                    None if at.is_some() => Kind::Spot,
                    None => kind_for(self, &first),
                };
                if (kind == Kind::Crease) != (solids.len() == 2) {
                    return Err("a crease names two solids, every other pick one".into());
                }
                if matches!(kind, Kind::Face | Kind::Edge | Kind::Chain) || (!view_row && at.is_some() && v.get("on").is_some() && matches!(kind, Kind::Feature | Kind::Edit)) {
                    let p = at.ok_or("a face, edge or chain is named by 'select' (as fab.exact writes it), or pointed at with 'at'")?;
                    // a point the AI names has no pixel: an edge within a millimetre
                    return self.exact_want(&first.object, &on, p, Some(kind), 1.0 / 3.0)?.ok_or_else(|| format!("`{}` is not an exact part with named faces: pick a feature or a spot", short(&on)));
                }
                Ok(Want { kind, on, solids, at, named: None })
            }
            _ => Err("a pick is a solid's name, two names (a crease), or {solid at kind}".into()),
        }
    }

    fn add_pick(&mut self, mut pick: Pick) {
        self.picks.next += 1;
        pick.id = format!("p{}", self.picks.next);
        self.picks.list.push(pick);
        self.picks.rev += 1;
    }

    /// The picks follow a build: each described again by its names on the new build (its point
    /// snapped back onto the surface); a pick whose solid is gone drops, and the next read says
    /// why. A name through a solid renamed by a step written after it (`r.face(..)` once `r` is
    /// `r_chamfer` and `r` a pull of it) reads the new name. Another document on the bed clears
    /// them. Per build: one description per pick.
    pub fn picks_follow_build(&mut self) {
        if self.picks.switched != self.switched {
            self.picks.switched = self.switched;
            if !self.picks.list.is_empty() {
                self.picks.list.clear();
                self.picks.rev += 1;
            }
            self.picks.dropped.clear();
        }
        let same = match (&self.picks.on, &self.shown_last()) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.picks.on = self.shown_last();
        let mut before = std::mem::take(&mut self.picks.exact);
        if before.is_empty() {
            before = std::mem::take(&mut self.picks.before);
        }
        self.picks.before.clear();
        if self.picks.list.is_empty() {
            return;
        }
        let list = std::mem::take(&mut self.picks.list);
        let mut changed = false;
        for old in list {
            let described = (|| {
                if old.on != old.object {
                    self.pick_step(&old.object, &old.on)?;
                }
                let named = old.named.as_ref().and_then(|n| self.renamed(&before, &old.on, n)).or_else(|| old.named.clone());
                self.describe(&Want { kind: old.kind, on: old.on.clone(), solids: old.solids.clone(), at: old.at.map(|a| a.0), named })
            })();
            match described {
                Ok(mut pick) => {
                    pick.id = old.id.clone();
                    changed |= pick != old;
                    self.picks.list.push(pick);
                }
                Err(e) => {
                    changed = true;
                    self.picks.dropped.push(format!("{} ({} {}): {e}", old.id, old.kind.name(), old.label));
                }
            }
        }
        if changed {
            self.picks.rev += 1;
        }
    }

    /// Name `named` on solid `on` on the build before (its exact body in `before`) read with
    /// the solids since renamed (a key whose solid has another id now); None when none was.
    fn renamed(&mut self, before: &[(String, Option<Arc<fab_model::eval::ExactBody>>)], on: &str, named: &Named) -> Option<Named> {
        let old = before.iter().find(|(o, _)| o == on)?.1.clone()?;
        let new = self.exact_body(on).ok()??;
        let ids: Vec<(&str, &str)> = old.parts.iter().filter_map(|(k, was)| new.parts.get(k).filter(|now| *now != was).map(|now| (was.as_str(), now.as_str()))).collect();
        if ids.is_empty() {
            return None;
        }
        // each solid's id read through (`r.` in `r.face(..)`) the id it has now
        let rename = |t: &str| {
            let mut out = String::with_capacity(t.len());
            let mut rest = t;
            while let Some(c) = rest.chars().next() {
                if c.is_alphabetic() || c == '_' {
                    let n = rest.find(|ch: char| !(ch.is_alphanumeric() || ch == '_')).unwrap_or(rest.len());
                    let word = &rest[..n];
                    let dotted = rest[n..].starts_with('.');
                    out.push_str(ids.iter().find(|(was, _)| dotted && *was == word).map_or(word, |(_, now)| now));
                    rest = &rest[n..];
                } else {
                    out.push(c);
                    rest = &rest[c.len_utf8()..];
                }
            }
            out
        };
        Some(match named {
            Named::Face(t) => Named::Face(rename(t)),
            Named::Edge([a, b]) => Named::Edge([rename(a), rename(b)]),
        })
    }

    fn view_rows(&mut self, rows: Vec<(Want, f64)>, normal: Option<P3>) -> S {
        let mut out = Vec::new();
        for (want, depth) in rows {
            let Ok(p) = self.describe_hit(&want, normal.filter(|_| want.kind != Kind::Feature)) else { continue };
            let mut f = vec![("kind", s(p.kind.name())), ("label", s(&p.label)), ("depth", mm(depth))];
            let mut spec = vec![("kind", s(p.kind.name())), ("solids", arr(p.solids.iter().map(s)))];
            if p.on != p.object {
                spec.push(("on", s(&p.on)));
            }
            if let Some((at, _)) = p.at {
                spec.push(("at", arr(at.iter().map(|x| n(*x)))));
            }
            if let Some(sel) = &p.selector {
                spec.push(("select", s(sel)));
            }
            f.push(("pick", obj(spec)));
            out.push(obj(f));
        }
        obj(vec![("rows", arr(out))])
    }

    fn point_result(&mut self, want: Option<Want>, gesture: Gesture, normal: Option<P3>, drain: bool) -> Result<S, String> {
        if matches!(gesture, Gesture::Hover) {
            // the view draws what a click would take: its outline too
            return Ok(match want {
                Some(w) => {
                    let pick = self.describe_hit(&w, normal)?;
                    let mut f = vec![("hover", pick.value())];
                    if !pick.outline.is_empty() {
                        f.push(("outline", arr(pick.outline.iter().map(|line| arr(line.iter().map(|p| arr(p.iter().map(|x| n(*x)))))))));
                    }
                    // the view's own: the features it tints, a crease's tick
                    if !pick.tint.is_empty() {
                        f.push(("tint", arr(pick.tint.iter().map(|(id, w)| arr([int(*id), n(*w as f64)])))));
                    }
                    if let Some((p, d)) = pick.tick {
                        f.push(("tick", arr([arr(p.iter().map(|x| n(*x))), arr(d.iter().map(|x| n(*x)))])));
                    }
                    obj(f)
                }
                None => obj(vec![("hover", S::Nil)]),
            });
        }
        let adding = matches!(gesture, Gesture::Click { add: true });
        let Some(want) = want else {
            if !adding && !self.picks.list.is_empty() {
                self.picks.list.clear();
                self.picks.rev += 1;
            }
            return Ok(self.picks_value(drain));
        };
        let pick = self.describe_hit(&want, normal)?;
        if !adding {
            self.picks.list.clear();
        }
        // Picking what is picked again takes it off (Shift), or keeps it alone.
        let again = self.picks.list.iter().position(|p| p.on == pick.on && p.kind == pick.kind && p.solids == pick.solids && p.selector == pick.selector);
        match again {
            Some(i) if adding => {
                self.picks.list.remove(i);
                self.picks.rev += 1;
            }
            _ => self.add_pick(pick),
        }
        Ok(self.picks_value(drain))
    }

    /// Drop picks of a surface the person has left. Only an AI read consumes the reasons.
    fn retain_picks_for_view(&mut self, target: Option<&str>) {
        let before = self.picks.list.len();
        self.picks.list.retain(|p| {
            let keep = p.on == target.unwrap_or(&p.object);
            if !keep {
                self.picks.dropped.push(format!("{} ({} {}): marker moved from {} to {}", p.id, p.kind.name(), p.label, p.on, target.unwrap_or("the whole part")));
            }
            keep
        });
        if self.picks.list.len() != before {
            self.picks.rev += 1;
        }
    }

    /// A host operation, never dispatched by tool name. A marked view supplies its own hit.
    pub fn view_pick(&mut self, request: ViewPick) -> Reply {
        self.picks_follow_build();
        let result = match request {
            ViewPick::Retain(on) => {
                self.retain_picks_for_view(on.as_deref());
                Ok(S::Nil)
            }
            ViewPick::Select(args) => self.select(&args, false),
            ViewPick::Point { hit, kind, gesture } => self.view_point(hit.as_ref(), &kind, gesture),
        };
        match result {
            Ok(v) => Reply::ok(v),
            Err(e) => Reply::err(e),
        }
    }

    fn view_point(&mut self, hit: Option<&ViewHit>, filter: &str, gesture: Gesture) -> Result<S, String> {
        let Some(hit) = hit else {
            return if matches!(gesture, Gesture::Along) { Ok(self.view_rows(Vec::new(), None)) } else { self.point_result(None, gesture, None, false) };
        };
        let on = self.pick_step(&hit.object, &hit.on)?;
        let tol = NEAR_PX * hit.pixel;
        let want = |kind| Want { on: on.clone(), kind, solids: vec![hit.object.clone()], at: Some(hit.at), named: None };
        if matches!(gesture, Gesture::Along) {
            let mut rows = self.exact_rows(&hit.object, &on, hit.at, hit.depth, tol)?;
            let first_feature = rows.len();
            let c = self.shown_compiled().ok_or("nothing is built yet")?;
            let ev = self.shown_last().ok_or("nothing is built yet")?;
            let (inst, k) = c.part(&on).ok_or("the shown step is gone")?;
            let solids = &inst.script.program.solids;
            let mut inputs = solids[k].inputs.clone();
            let mut seen = std::collections::HashSet::new();
            let from = add_scaled(hit.at, hit.dir, -hit.depth);
            while let Some(i) = inputs.pop() {
                if !seen.insert(i) { continue; }
                inputs.extend_from_slice(&solids[i].inputs);
                let id = fab_model::program::part_id(&inst.name, &solids[i].id);
                if rows[..first_feature].iter().any(|(w, _)| w.kind == Kind::Feature && w.solids[0] == id) { continue; }
                let Some((lo, hi)) = ev.objects.iter().find(|o| o.name == id).and_then(|o| o.bounds) else { continue };
                if let Some((t, _)) = slab(from, hit.dir, lo, hi) {
                    rows.push((Want { solids: vec![id], at: Some(add_scaled(from, hit.dir, t.max(0.0))), ..want(Kind::Feature) }, t));
                }
            }
            rows[first_feature..].sort_by(|a, b| a.1.total_cmp(&b.1));
            rows.push((want(Kind::Body), hit.depth));
            return Ok(self.view_rows(rows, Some(hit.normal)));
        }
        let kind = Kind::parse(filter);
        let w = match kind {
            Some(k @ (Kind::Body | Kind::Spot)) => Some(want(k)),
            _ => self.exact_want(&hit.object, &on, hit.at, kind, tol)?,
        };
        self.point_result(w, gesture, Some(hit.normal), false)
    }

    fn picks_value(&mut self, drain: bool) -> S {
        let mut f = vec![("picks", arr(self.picks.list.iter().map(Pick::value)))];
        if drain && !self.picks.dropped.is_empty() {
            f.push(("dropped", arr(std::mem::take(&mut self.picks.dropped).into_iter().map(s))));
        }
        if self.picks.list.is_empty() {
            f.push(("note", s("nothing is picked: the person picks in the Edit shape mode (E); `set` picks for them")));
        }
        obj(f)
    }

    /// `fab.select`: read the picks; `set` or `add` picks (a solid's name, two names for a crease,
    /// `{solid at kind}`, or `{camera x y}` a pixel of a render's view); `remove` one by id;
    /// `clear`. The view's own: `ray` `{from dir}` (plate mm) with `pixel` `[fixed per_depth]`,
    /// `kind` and `add`, `hover` (what a pick there would be, nothing changes) and `along`
    /// (Select Other's rows).
    pub(crate) fn select_tool(&mut self, args: &Value) -> Reply {
        self.picks_follow_build();
        match self.select(args, true) {
            Ok(v) => Reply::ok(v),
            Err(e) => Reply::err(e),
        }
    }

    fn select(&mut self, args: &Value, drain: bool) -> Result<S, String> {
        if args.get("camera").is_some() && args.get("on").is_some() {
            return Err("a pixel picks what the view shows: name the step's face with 'select', or point at it with 'at'".into());
        }
        let kind = match str_arg(args, "kind") {
            None | Some("any") => None,
            Some(k) => Some(Kind::parse(k).ok_or_else(|| format!("'kind' is any, edit, crease, edges, spot, feature, body, face, edge or chain; not '{k}'"))?),
        };
        let adding = args.get("add").and_then(Value::as_bool) == Some(true);
        // The view's ray, or a pixel of a render's view.
        let ray = match (args.get("ray"), args.get("camera")) {
            (Some(r), _) => {
                let from = point_arg::<3>(r.get("from").ok_or("'ray' is {from dir}")?).map_err(|e| format!("'ray.from': {e}"))?;
                let dir = unit(point_arg::<3>(r.get("dir").ok_or("'ray' is {from dir}")?).map_err(|e| format!("'ray.dir': {e}"))?).ok_or("'ray.dir' has no length")?;
                let pixel = match args.get("pixel") {
                    Some(p) => Some(point_arg::<2>(p).map(|p| (p[0], p[1])).map_err(|e| format!("'pixel': {e}"))?),
                    None => None,
                };
                Some((from, dir, pixel))
            }
            (None, Some(camera)) => {
                let cam = crate::annotate::Cam::from_value(camera)?;
                let (Some(x), Some(y)) = (args.get("x").and_then(number), args.get("y").and_then(number)) else { return Err("with 'camera', 'x' and 'y' are the pixel in that view's picture".into()) };
                let (from, dir) = cam.ray(x, y)?;
                let pixel = match (cam.fov, cam.across) {
                    (Some(fov), _) => (0.0, 2.0 * (fov.to_radians() * 0.5).tan() / cam.h.max(1.0)),
                    (_, Some(across)) => (across / cam.w.max(1.0), 0.0),
                    _ => (0.0, 0.002),
                };
                Some((from, dir, Some(pixel)))
            }
            _ => None,
        };
        if let Some((from, dir, pixel)) = ray {
            if args.get("along").and_then(Value::as_bool) == Some(true) {
                let rows = self.along(from, dir, pixel)?;
                return Ok(self.view_rows(rows, None));
            }
            let want = self.want_at_ray(from, dir, pixel, kind)?;
            let gesture = if args.get("hover").and_then(Value::as_bool) == Some(true) { Gesture::Hover } else { Gesture::Click { add: adding } };
            return self.point_result(want, gesture, None, drain);
        }
        if let Some(r) = args.get("remove") {
            let ids: Vec<&str> = match r {
                Value::Arr(list) => list.iter().filter_map(Value::as_str).collect(),
                other => other.as_str().into_iter().collect(),
            };
            let before = self.picks.list.len();
            self.picks.list.retain(|p| !ids.contains(&p.id.as_str()));
            if self.picks.list.len() == before {
                return Err(format!("no pick {}: the picks are {}", ids.join(", "), self.picks.list.iter().map(|p| p.id.as_str()).collect::<Vec<_>>().join(", ")));
            }
            self.picks.rev += 1;
            return Ok(self.picks_value(drain));
        }
        if args.get("clear").and_then(Value::as_bool) == Some(true) {
            if !self.picks.list.is_empty() {
                self.picks.list.clear();
                self.picks.rev += 1;
            }
            return Ok(self.picks_value(drain));
        }
        let (list, replace) = match (args.get("set"), args.get("add")) {
            (Some(list), _) => (list, true),
            (None, Some(list)) if !matches!(list, Value::Bool(_)) => (list, false),
            _ => return Ok(self.picks_value(drain)),
        };
        // a list of picks, or one
        let items: Vec<&Value> = match list {
            Value::Arr(items) => items.iter().collect(),
            one => vec![one],
        };
        // Every pick resolves, or nothing changes.
        let mut picks = Vec::new();
        let mut problems = Vec::new();
        for (i, item) in items.iter().enumerate() {
            match self.want_of(item, !drain).and_then(|w| self.describe(&w)) {
                Ok(p) => picks.push(p),
                Err(e) => problems.push(format!("pick {}: {e}", i + 1)),
            }
        }
        if !problems.is_empty() {
            return Err(format!("{}. Nothing was changed", problems.join("; ")));
        }
        if replace {
            self.picks.list.clear();
            self.picks.rev += 1;
        }
        for p in picks {
            self.add_pick(p);
        }
        Ok(self.picks_value(drain))
    }
}
