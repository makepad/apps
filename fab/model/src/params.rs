//! The parameter model: every value of a Fab program a person (or a handle in
//! the view) can tune, with enough meaning to build its control and its handle.
//!
//! Two sources, one model, both read from the program's syntax tree and its
//! evaluation (never from text patterns):
//!
//! - **Declared parameters**: `let height = param(70mm, min: 40mm, max: 120mm,
//!   label: "Height", group: "Vase", kind: "radius")`. Its site is the first
//!   argument's literal.
//! - **Part fields**: the number literals written in a part's builder call
//!   (`radius: 2.3mm`, an element of `size: [40mm 10mm 30mm]`), typed by the
//!   prototype's field table. Fields written as expressions are worked out and
//!   not listed.
//!
//! A parameter belongs to the plate parts whose solids use it (directly, through
//! other `let`s, or inside an SDF field's fn). Lengths show in millimetres and
//! angles in degrees. Its handle is its field's (an extrusion's height arrow, a
//! cylinder's radius ring, a position's point) in the model frame, its `rate` the
//! handle's travel per unit of value, found by evaluating the program with the
//! parameter moved (an override of the evaluation, not a text change). An edit
//! is one [`Edit::Set`] of the site: one undo step. A solid's placement (`at`, `rotate`,
//! `scale`) is a value too, written whole: what the view's gizmo drags ([`placed`]).
//!
//! A cage (`m.cage`) lists its points one row each, a point written as three numbers taking
//! a point handle, and its faces as read-out rows; its [`Wire`] is the cage drawn over the
//! smooth surface.

use crate::eval::Evaluated;
use crate::program::{self, part_id, split_id, Compiled, Instance, Script, UNIT};
use crate::{Change, Edit};
use makepad_csg_graph::{Op, Plane};
use makepad_model_script::lang::{edit, protos, Program, Solid};
use makepad_script::syntax::{Kind as SKind, Node};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

pub type V3 = [f64; 3];

/// What a parameter measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Length,
    Radius,
    Diameter,
    Angle,
    Count,
    Number,
    /// A factor (scale), shown in percent.
    Ratio,
    Bool,
    Choice,
    Point,
    Profile,
    /// A whole number that picks one of many (a pattern's seed): its row reseeds too.
    Seed,
    /// A grown pattern's progress: shown live, with pause and resume; never written.
    Growth,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "length" => Kind::Length,
            "radius" => Kind::Radius,
            "diameter" => Kind::Diameter,
            "angle" => Kind::Angle,
            "count" => Kind::Count,
            "number" => Kind::Number,
            "ratio" => Kind::Ratio,
            "bool" => Kind::Bool,
            "choice" => Kind::Choice,
            "seed" => Kind::Seed,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Kind::Length => "length",
            Kind::Radius => "radius",
            Kind::Diameter => "diameter",
            Kind::Angle => "angle",
            Kind::Count => "count",
            Kind::Number => "number",
            Kind::Ratio => "ratio",
            Kind::Bool => "bool",
            Kind::Choice => "choice",
            Kind::Point => "point",
            Kind::Profile => "profile",
            Kind::Seed => "seed",
            Kind::Growth => "growth",
        }
    }
    pub fn default_unit(self) -> &'static str {
        match self {
            Kind::Length | Kind::Radius | Kind::Diameter | Kind::Point | Kind::Profile => "mm",
            Kind::Angle => "°",
            Kind::Ratio => "%",
            _ => "",
        }
    }
    pub fn is_numeric(self) -> bool {
        matches!(self, Kind::Length | Kind::Radius | Kind::Diameter | Kind::Angle | Kind::Count | Kind::Number | Kind::Ratio | Kind::Seed)
    }
    fn is_length(self) -> bool {
        matches!(self, Kind::Length | Kind::Radius | Kind::Diameter)
    }
}

/// A parameter's value (lengths in millimetres, angles in degrees).
#[derive(Clone, Debug, PartialEq)]
pub enum PValue {
    Num(f64),
    Bool(bool),
    /// Index into [`Param::choices`].
    Choice(usize),
    Point(Vec<f64>),
    Profile(Vec<[f64; 2]>),
}

impl PValue {
    pub fn num(&self) -> Option<f64> {
        match self {
            PValue::Num(x) => Some(*x),
            _ => None,
        }
    }
}

/// A handle's geometry in the model frame (millimetres, Y up). `rate` is how far the
/// handle moves (mm, or degrees for an angle) per unit of the parameter's value.
#[derive(Clone, Debug, PartialEq)]
pub enum Anchor {
    /// An arrow at `to` pointing away from `from`: the handle slides along that line.
    Linear { from: V3, to: V3, rate: f64 },
    /// A ring of `radius` about `axis` through `center`.
    Radius { center: V3, axis: V3, radius: f64, rate: f64 },
    /// An arc about `axis` through `center`, from the direction `from` (angle 0) to `angle`
    /// degrees (right-handed about `axis`).
    Angle { center: V3, axis: V3, from: V3, radius: f64, angle: f64, rate: f64 },
    /// A point handle at `at`; `axes[i]` is the model displacement per unit of coordinate i.
    Point { at: V3, axes: Vec<V3> },
    /// A 2D profile drawn in the plane `origin + x*u + y*v`; one point handle per point.
    Profile { origin: V3, u: V3, v: V3, points: Vec<[f64; 2]> },
    /// + and - buttons at `at`, along `axis`.
    Count { at: V3, axis: V3 },
}

/// The frame of a solid with none of its own.
const IDENTITY: [[f64; 4]; 4] = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];

/// The longest list a field's elements are value sites of; longer ones are data.
pub const MAX_LIST_SITES: usize = 512;

fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn scale(a: V3, k: f64) -> V3 {
    [a[0] * k, a[1] * k, a[2] * k]
}
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn len(a: V3) -> f64 {
    dot(a, a).sqrt()
}
fn unit(a: V3) -> V3 {
    let l = len(a);
    if l > 1e-12 {
        scale(a, 1.0 / l)
    } else {
        [0.0, 1.0, 0.0]
    }
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
/// Any unit vector perpendicular to `a`.
fn perp(a: V3) -> V3 {
    let t = if a[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 0.0, 1.0] };
    unit(cross(a, t))
}

/// A point through a 4x4 or 3x4 row-major affine.
pub fn xf_point(m: &[[f64; 4]], p: V3) -> V3 {
    std::array::from_fn(|i| m[i][0] * p[0] + m[i][1] * p[1] + m[i][2] * p[2] + m[i][3])
}
/// A direction through the linear part.
pub fn xf_dir(m: &[[f64; 4]], d: V3) -> V3 {
    std::array::from_fn(|i| m[i][0] * d[0] + m[i][1] * d[1] + m[i][2] * d[2])
}

/// `d` turned by `radians` about the unit `axis`.
pub fn rotate(d: V3, axis: V3, radians: f64) -> V3 {
    crate::eval::rotate_about(d, [0.0; 3], unit(axis), radians)
}

impl Anchor {
    /// The same handle through an affine `m` (an object's frame, a part's
    /// model-to-plate frame). Lengths scale with `m`; angles do not.
    pub fn transformed(&self, m: &[[f64; 4]]) -> Anchor {
        match self {
            Anchor::Linear { from, to, rate } => {
                let d = unit(sub(*to, *from));
                Anchor::Linear { from: xf_point(m, *from), to: xf_point(m, *to), rate: rate * len(xf_dir(m, d)) }
            }
            Anchor::Radius { center, axis, radius, rate } => {
                let s = len(xf_dir(m, perp(*axis)));
                Anchor::Radius { center: xf_point(m, *center), axis: unit(xf_dir(m, *axis)), radius: radius * s, rate: rate * s }
            }
            Anchor::Angle { center, axis, from, radius, angle, rate } => {
                let s = len(xf_dir(m, unit(*from)));
                Anchor::Angle { center: xf_point(m, *center), axis: unit(xf_dir(m, *axis)), from: unit(xf_dir(m, *from)), radius: radius * s, angle: *angle, rate: *rate }
            }
            Anchor::Point { at, axes } => Anchor::Point { at: xf_point(m, *at), axes: axes.iter().map(|a| xf_dir(m, *a)).collect() },
            Anchor::Profile { origin, u, v, points } => Anchor::Profile { origin: xf_point(m, *origin), u: xf_dir(m, *u), v: xf_dir(m, *v), points: points.clone() },
            Anchor::Count { at, axis } => Anchor::Count { at: xf_point(m, *at), axis: unit(xf_dir(m, *axis)) },
        }
    }

    /// Where the handle is grabbed (its tip, ring point, arc end or point).
    pub fn grip(&self) -> V3 {
        match self {
            Anchor::Linear { to, .. } => *to,
            Anchor::Radius { center, axis, radius, .. } => add(*center, scale(perp(*axis), *radius)),
            Anchor::Angle { center, axis, from, radius, angle, .. } => add(*center, scale(rotate(*from, *axis, angle.to_radians()), *radius)),
            Anchor::Point { at, .. } | Anchor::Count { at, .. } => *at,
            Anchor::Profile { origin, .. } => *origin,
        }
    }

    fn rate(&self) -> Option<f64> {
        match self {
            Anchor::Linear { rate, .. } | Anchor::Radius { rate, .. } | Anchor::Angle { rate, .. } => Some(*rate),
            _ => None,
        }
    }

    fn with_rate(self, rate: f64) -> Anchor {
        match self {
            Anchor::Linear { from, to, .. } => Anchor::Linear { from, to, rate },
            Anchor::Radius { center, axis, radius, .. } => Anchor::Radius { center, axis, radius, rate },
            Anchor::Angle { center, axis, from, radius, angle, .. } => Anchor::Angle { center, axis, from, radius, angle, rate },
            other => other,
        }
    }

    /// The handle's position measure for finite differences.
    fn measure(&self, dir: V3) -> Option<f64> {
        match self {
            Anchor::Linear { from, to, .. } => Some(dot(sub(*to, *from), dir)),
            Anchor::Radius { radius, .. } => Some(*radius),
            Anchor::Angle { angle, .. } => Some(*angle),
            _ => None,
        }
    }
}

/// One tunable value.
#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    /// `instance/site`: the site is a param's name, or `solid.field[.i]`.
    pub key: String,
    /// The instance it belongs to.
    pub instance: String,
    pub label: String,
    /// The panel group: a declared parameter's `group` (default "Parameters"), else the
    /// solid its field belongs to.
    pub group: String,
    pub kind: Kind,
    pub unit: String,
    pub value: PValue,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    /// Choice labels and the Splash they write (`@xz`).
    pub choices: Vec<(String, String)>,
    /// The literal's bytes in the instance's script.
    pub site: Range<usize>,
    pub declared: bool,
    /// Worked out from other parameters (shown, not edited).
    pub derived: bool,
    /// Segments, rings and cells: tuned rarely, listed last.
    pub detail: bool,
    pub hint: String,
    /// The solids it shapes (`instance/solid`, to highlight them).
    pub objects: Vec<String>,
    /// The handle, in the model frame (`None`: panel only).
    pub anchor: Option<Anchor>,
}

/// The parameters of one plate part, panel order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PartParams {
    pub part: usize,
    pub object: String,
    pub params: Vec<Param>,
    /// Every solid the part is built from.
    pub objects: Vec<String>,
    /// Its cages' wireframes.
    pub wires: Vec<Wire>,
}

/// A cage's wireframe in the model frame: its points, its faces' edges (each once, by point
/// index) and each point's row key (`None`: a point worked out, with no row).
#[derive(Clone, Debug, PartialEq)]
pub struct Wire {
    pub object: String,
    pub points: Vec<V3>,
    pub edges: Vec<[u32; 2]>,
    pub keys: Vec<Option<String>>,
}

fn pretty(name: &str) -> String {
    let s = name.replace('_', " ");
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// Lengths to millimetres, angles to degrees.
fn shown(kind: Kind, v: f64) -> f64 {
    if kind.is_length() || kind == Kind::Point {
        v * UNIT
    } else if kind == Kind::Angle {
        v.to_degrees()
    } else {
        v
    }
}

fn kind_of(k: protos::Kind) -> Kind {
    match k {
        protos::Kind::Length | protos::Kind::Bounds => Kind::Length,
        protos::Kind::Radius => Kind::Radius,
        protos::Kind::Diameter => Kind::Diameter,
        protos::Kind::Angle | protos::Kind::Rotation => Kind::Angle,
        protos::Kind::Count => Kind::Count,
        protos::Kind::Ratio | protos::Kind::Scale => Kind::Ratio,
        protos::Kind::Bool => Kind::Bool,
        protos::Kind::Choice(_) => Kind::Choice,
        protos::Kind::Point | protos::Kind::Point2 | protos::Kind::Points | protos::Kind::Points2 => Kind::Length,
        _ => Kind::Number,
    }
}

/// The kind a written literal says (`70mm` a length, `90°` an angle).
fn kind_of_literal(text: &str) -> Kind {
    let t = text.trim();
    if t.ends_with("mm") || t.ends_with("cm") || t.ends_with('m') {
        Kind::Length
    } else if t.ends_with('°') || t.ends_with("deg") || t.ends_with("rad") {
        Kind::Angle
    } else {
        Kind::Number
    }
}

/// `v` (display units) written like `like` (its unit suffix and precision).
pub fn write_like(like: &str, kind: Kind, v: f64) -> String {
    let t = like.trim();
    let num = |x: f64| {
        let r = (x * 10_000.0).round() / 10_000.0;
        let s = format!("{r}");
        if s == "-0" { "0".to_string() } else { s }
    };
    for (suffix, per_mm) in [("mm", 1.0), ("cm", 0.1)] {
        if t.ends_with(suffix) {
            return format!("{}{suffix}", num(v * per_mm));
        }
    }
    if t.ends_with('m') && !t.ends_with("mm") && kind.is_length() {
        return format!("{}m", num(v / 1000.0));
    }
    if t.ends_with('°') {
        return format!("{}°", num(v));
    }
    if t.ends_with("deg") {
        return format!("{}deg", num(v));
    }
    match kind {
        Kind::Length | Kind::Radius | Kind::Diameter => format!("{}mm", num(v)),
        Kind::Angle => format!("{}°", num(v)),
        Kind::Count | Kind::Seed => format!("{}", v.round()),
        _ => num(v),
    }
}

/// The outer names a node uses, read from the syntax tree: names bound inside it
/// (a fn's parameters and lets, a loop's variable) are its own.
fn uses_of(node: &Node, out: &mut BTreeSet<String>) {
    let mut own = BTreeSet::new();
    let mut used = BTreeSet::new();
    node.walk(&Default::default(), &mut |_, n: &Node| match &n.kind {
        SKind::Let { names, .. } => own.extend(names.iter().cloned()),
        SKind::Fn { params, .. } => own.extend(params.iter().map(|p| p.name.clone())),
        _ => {
            if let Some(i) = n.ident() {
                used.insert(i.split('.').next().unwrap_or(i).to_string());
            }
        }
    });
    out.extend(used.into_iter().filter(|u| !own.contains(u)));
}

/// `name` is among [`uses_of`]'s names of `node` (used inside it and bound by none of its lets
/// or fns), found without collecting them.
fn uses_name(node: &Node, name: &str) -> bool {
    let (mut own, mut used) = (false, false);
    node.walk(&Default::default(), &mut |_, n: &Node| match &n.kind {
        SKind::Let { names, .. } => own |= names.iter().any(|x| x == name),
        SKind::Fn { params, .. } => own |= params.iter().any(|p| p.name == name),
        _ => used |= n.ident().is_some_and(|i| i.split('.').next().unwrap_or(i) == name),
    });
    used && !own
}

/// For every name: the params it depends on (through other lets).
fn param_deps(p: &Program) -> HashMap<String, BTreeSet<String>> {
    let params: BTreeSet<String> = p.params.iter().filter_map(|q| q.name.clone()).collect();
    let mut direct: Vec<(String, BTreeSet<String>)> = Vec::new();
    for stmt in p.names.syntax.statements() {
        if let SKind::Let { names, value: Some(v), .. } = &stmt.kind {
            let mut u = BTreeSet::new();
            uses_of(v, &mut u);
            for n in names {
                direct.push((n.clone(), u.clone()));
            }
        }
    }
    let mut deps: HashMap<String, BTreeSet<String>> = HashMap::new();
    // Lets are in order: a name uses only names bound before it.
    for (name, used) in direct {
        let mut d = BTreeSet::new();
        for u in used {
            if params.contains(&u) && u != name {
                d.insert(u.clone());
            }
            if let Some(more) = deps.get(&u) {
                d.extend(more.iter().cloned());
            }
        }
        if params.contains(&name) {
            d.insert(name.clone());
        }
        deps.insert(name, d);
    }
    deps
}

/// The params a solid's code uses: the names in its own bytes, or for a solid made
/// by other code, in the top-level statement that makes it.
fn solid_params(p: &Program, deps: &HashMap<String, BTreeSet<String>>, i: usize) -> BTreeSet<String> {
    let s = &p.solids[i];
    let syn = &p.names.syntax;
    let mut used = BTreeSet::new();
    let span = s.span.clone();
    for stmt in syn.statements() {
        let covers = span.as_ref().is_some_and(|r| stmt.span.start <= r.start && r.end <= stmt.span.end);
        if covers {
            match (&stmt.kind, s.named) {
                (SKind::Let { value: Some(v), .. }, true) => uses_of(v, &mut used),
                _ => uses_of(stmt, &mut used),
            }
        }
    }
    used.iter().flat_map(|u| deps.get(u).cloned().unwrap_or_default()).collect()
}

/// The fields a part's builder call writes: each name and its value node.
fn part_fields<'p>(p: &'p Program, s: &Solid) -> Vec<(String, &'p Node)> {
    let (Ok(call), Some(proto)) = (edit::part_call(p, s), protos::proto(s.proto)) else { return Vec::new() };
    let syn = &p.names.syntax;
    edit::call_fields(syn, &call, proto).into_iter().filter_map(|(name, at)| Some((name, syn.node(at.loc())?))).collect()
}

/// The solid op under a solid's own placement.
fn base_op<'a>(p: &'a Program, i: usize) -> &'a Op {
    let node = p.graph.node(p.solids[i].node);
    match &node.op {
        // A part's own placement wraps its op (no part is itself a transform).
        Op::Transform { source, .. } => &p.graph.node(*source).op,
        op => op,
    }
}

fn plane_w(pl: Plane) -> V3 {
    let (_, _, w) = makepad_csg_graph::eval::plane_frame(pl);
    [w.x, w.y, w.z]
}

/// A field's handle in the solid's own frame (mm), from its built op.
fn field_anchor(op: &Op, field: &str, index: Option<usize>) -> Option<Anchor> {
    const Y: V3 = [0.0, 1.0, 0.0];
    const AXES: [V3; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    Some(match (op, field, index) {
        (Op::Box { size, .. }, "size", Some(i)) if i < 3 => Anchor::Linear { from: [0.0; 3], to: scale(AXES[i], size[i] * 0.5), rate: 0.5 },
        (Op::Cylinder { radius, .. }, "radius", None) => Anchor::Radius { center: [0.0; 3], axis: Y, radius: *radius, rate: 1.0 },
        (Op::Cylinder { height, .. }, "height", None) => Anchor::Linear { from: [0.0; 3], to: [0.0, height * 0.5, 0.0], rate: 0.5 },
        (Op::Sphere { radius, .. }, "radius", None) => Anchor::Radius { center: [0.0; 3], axis: Y, radius: *radius, rate: 1.0 },
        (Op::Torus { radius, .. }, "radius", None) => Anchor::Radius { center: [0.0; 3], axis: Y, radius: *radius, rate: 1.0 },
        (Op::Extrude { plane, height, both, .. }, "height", None) => {
            let w = plane_w(*plane);
            let base = if *both { scale(w, -height * 0.5) } else { [0.0; 3] };
            Anchor::Linear { from: base, to: add(base, scale(w, *height)), rate: if *both { 0.5 } else { 1.0 } }
        }
        (Op::Text { plane, depth, .. }, "depth", None) => Anchor::Linear { from: [0.0; 3], to: scale(plane_w(*plane), *depth), rate: 1.0 },
        (Op::Gear { plane, gear, .. }, "thickness", None) => Anchor::Linear { from: [0.0; 3], to: scale(plane_w(*plane), gear.thickness), rate: 1.0 },
        (Op::Gear { gear, .. }, "teeth", None) => Anchor::Count { at: [gear.module * (gear.teeth as f64 + 2.0) / 2.0, 0.0, 0.0], axis: [1.0, 0.0, 0.0] },
        (Op::Thread { length, .. }, "length", None) => Anchor::Linear { from: [0.0; 3], to: [0.0, *length, 0.0], rate: 1.0 },
        (Op::Revolve { angle, .. }, "angle", None) => Anchor::Angle { center: [0.0; 3], axis: Y, from: [1.0, 0.0, 0.0], radius: 0.0, angle: *angle, rate: 1.0 },
        _ => return None,
    })
}

/// The literal's text at a span.
fn text<'a>(p: &'a Program, r: &Range<usize>) -> &'a str {
    &p.source()[r.clone()]
}

fn literal(node: &Node) -> Option<f64> {
    match &node.kind {
        SKind::Number(v) => Some(*v),
        SKind::Unary { op, operand } if op == "-" => operand.number().map(|v| -v),
        _ => None,
    }
}

/// The parameter model of an evaluated project: one entry per plate part. Keys are
/// `instance/site`: a declared parameter is the instance's own (its value is the project's),
/// a part field is its script's (an edit changes every instance of that script).
pub fn model(c: &Compiled, ev: &Evaluated) -> Vec<PartParams> {
    let mut base = 0;
    let mut all: Vec<Vec<(Param, Vec<usize>)>> = Vec::with_capacity(c.instances.len());
    let mut bases = Vec::with_capacity(c.instances.len());
    for inst in &c.instances {
        all.push(instance_params(inst, ev, base, None));
        bases.push(base);
        base += inst.script.program.solids.len();
    }
    // Per plate part: the params of the solids it is built from.
    let mut out = Vec::new();
    for (pi, part) in ev.plate.placed_parts().enumerate() {
        let Some(ii) = split_id(&part.object).and_then(|(i, _)| c.instances.iter().position(|x| x.name == i)) else { continue };
        let inst = &c.instances[ii];
        let p = &inst.script.program;
        let Some(root) = split_id(&part.object).and_then(|(_, s)| p.solids.iter().position(|x| x.id == s)) else { continue };
        let within = part_solids(p, root);
        let mut params: Vec<Param> = all[ii].iter().filter(|(_, s)| s.iter().any(|i| within.contains(i))).map(|(q, _)| q.clone()).collect();
        params.sort_by_key(|q| (!q.declared, q.detail));
        let wires = cage_wires(inst, ev, bases[ii], &within, &params);
        out.push(PartParams { part: pi, object: part.object.clone(), params, objects: within.iter().map(|i| part_id(&inst.name, &p.solids[*i].id)).collect(), wires });
    }
    out
}

/// The parameter model while one parameter is dragged: `instance`'s parameters and handles as its
/// script `live` (compiled at the dragged value) has them, their rates `old`'s; nothing compiled
/// or evaluated. Its solids' frames are the evaluation's, as the live fields' frames are. The
/// other instances' stay as in `old`.
pub fn model_with(c: &Compiled, ev: &Evaluated, old: &[PartParams], instance: &str, live: &Arc<Script>) -> Vec<PartParams> {
    let Some(ii) = c.instances.iter().position(|x| x.name == instance) else { return old.to_vec() };
    let base: usize = c.instances[..ii].iter().map(|x| x.script.program.solids.len()).sum();
    let inst = Instance { name: instance.to_string(), script_instance: c.instances[ii].script_instance.clone(), link: c.instances[ii].link, script: live.clone(), placed: c.instances[ii].placed.clone() };
    let all = instance_params(&inst, ev, base, Some(old));
    let p = &live.program;
    old.iter()
        .map(|pp| {
            let root = split_id(&pp.object).filter(|(i, _)| *i == instance).and_then(|(_, s)| p.solids.iter().position(|x| x.id == s));
            let Some(root) = root else { return pp.clone() };
            let within = part_solids(p, root);
            let mut params: Vec<Param> = all.iter().filter(|(_, s)| s.iter().any(|i| within.contains(i))).map(|(q, _)| q.clone()).collect();
            params.sort_by_key(|q| (!q.declared, q.detail));
            let wires = cage_wires(&inst, ev, base, &within, &params);
            PartParams { part: pp.part, object: pp.object.clone(), params, objects: within.iter().map(|i| part_id(&inst.name, &p.solids[*i].id)).collect(), wires }
        })
        .collect()
}

/// One instance's parameters and the solids each shapes (`base`: its first solid's index in
/// `ev.objects`). Handle rates come from `rates` when supplied, otherwise they are measured.
fn instance_params(inst: &Instance, ev: &Evaluated, base: usize, rates: Option<&[PartParams]>) -> Vec<(Param, Vec<usize>)> {
    let p = &inst.script.program;
    let q_id = |site: &str| part_id(&inst.name, site);
    let deps = param_deps(p);
    let used_by: Vec<BTreeSet<String>> = (0..p.solids.len()).map(|i| solid_params(p, &deps, i)).collect();
    let mut all: Vec<(Param, Vec<usize>)> = Vec::new();
    // Declared parameters, in program order.
    for q in &p.params {
        let (Some(name), Some(span)) = (&q.name, &q.value_span) else { continue };
        let written = text(p, span);
        let kind = q.call.kind.as_deref().and_then(Kind::parse).unwrap_or_else(|| kind_of_literal(written));
        let mut solids: Vec<usize> = (0..p.solids.len()).filter(|i| used_by[*i].contains(name)).collect();
        // One no solid reads steers something else (a load's size, say): it shows on all of them.
        if solids.is_empty() {
            solids = (0..p.solids.len()).collect();
        }
        let v = |x: Option<f64>| x.map(|x| shown(kind, x));
        let value = shown(kind, q.call.value);
        let (min, max) = (v(q.call.min).unwrap_or(if value > 0.0 { 0.0 } else { value * 2.0 }), v(q.call.max).unwrap_or(value.abs().max(1.0) * 4.0));
        all.push((
            Param {
                key: q_id(name),
                instance: inst.name.clone(),
                label: q.call.label.clone().unwrap_or_else(|| pretty(name)),
                group: q.call.group.clone().unwrap_or_else(|| "Parameters".into()),
                kind,
                unit: kind.default_unit().to_string(),
                value: PValue::Num(value),
                min,
                max,
                step: v(q.call.step).unwrap_or(if matches!(kind, Kind::Count | Kind::Seed) { 1.0 } else { 0.1 }),
                choices: Vec::new(),
                site: span.clone(),
                declared: true,
                derived: false,
                detail: false,
                hint: q.call.hint.clone().unwrap_or_default(),
                objects: solids.iter().map(|i| q_id(&p.solids[*i].id)).collect(),
                anchor: None,
            },
            solids,
        ));
    }
    // Each solid's fields, found once for the literals and the declared params' handles.
    let fields_of: Vec<Vec<(String, &Node)>> = p.solids.iter().map(|s| part_fields(p, s)).collect();
    // Solid fields written as literals.
    for (i, s) in p.solids.iter().enumerate() {
        let Some(proto) = protos::proto(s.proto) else { continue };
        let fields = &fields_of[i];
        let frame = &ev.objects.get(base + i).map_or(IDENTITY, |o| o.world);
        let op = base_op(p, i);
        let choice_rows = matches!(proto.name, "Texture" | "Voronoi");
        // A part a script fn makes is set at its call: its fields are what the call passes.
        let group = edit::call_label(p, s).unwrap_or_else(|| pretty(&s.id));
        for &(ref name, written) in fields {
            let name = name.as_str();
            let Some(def) = proto.field(name) else { continue };
            // A fn's part: the field as its call passes it, else (`shared`) each value in the
            // fn's code followed to its call on its own.
            let (value, shared) = match s.call {
                None => (written, false),
                Some(_) => edit::part_value(p, s, name).map_or((written, true), |v| (v, false)),
            };
            // (the path under the solid, its field, the list index, the literal)
            let mut sites: Vec<(String, &protos::Field, Option<usize>, &Node)> = Vec::new();
            match &value.kind {
                // A long list (a stroke's path, sampled data) is data, never value sites.
                SKind::Array { items, .. } if items.len() > MAX_LIST_SITES => {}
                SKind::Array { .. } if shared && proto.name == "Cage" && matches!(name, "points" | "faces") => {}
                SKind::Array { items, .. } if proto.name == "Cage" && matches!(name, "points" | "faces") => {
                    all.extend(cage_rows(inst, s, i, name, items, op, def).into_iter().map(|(mut q, at)| {
                        q.anchor = q.anchor.map(|a| a.transformed(&frame[..]));
                        (q, at)
                    }));
                    continue;
                }
                SKind::Array { items, .. } => sites.extend(items.iter().enumerate().map(|(k, n)| (format!("{name}.{k}"), def, Some(k), n))),
                // A pattern written in place (`Knurl{pitch: 1mm ..}`): its own literal fields.
                SKind::Object { ty: Some(ty), fields: inner, .. } if def.kind == protos::Kind::Pattern => {
                    let Some(pattern) = protos::proto(ty) else { continue };
                    for e in inner {
                        let Some(fd) = e.name.as_deref().and_then(|n| pattern.field(n)) else { continue };
                        sites.push((format!("{name}.{}", fd.key), fd, None, &e.value));
                    }
                }
                _ => sites.push((name.to_string(), def, None, value)),
            }
            for (path, def, index, n) in sites {
                let n = match shared {
                    true => match edit::part_value(p, s, &path) {
                        Ok(n) => n,
                        Err(_) => continue,
                    },
                    false => n,
                };
                let key = q_id(&format!("{}.{path}", s.id));
                // A texture's @choices (its mapping, axis, a knurl's kind): the field's choices.
                if let (true, protos::Kind::Choice(list), SKind::Name(chosen)) = (choice_rows, def.kind, &n.kind) {
                    let Some(at) = list.iter().position(|c| *c == chosen.trim_start_matches('@')) else { continue };
                    all.push((
                        Param {
                            key,
                            instance: inst.name.clone(),
                            label: def.label.to_string(),
                            group: group.clone(),
                            kind: Kind::Choice,
                            unit: String::new(),
                            value: PValue::Choice(at),
                            min: 0.0,
                            max: 0.0,
                            step: 0.0,
                            choices: list.iter().map(|c| (pretty(c), format!("@{c}"))).collect(),
                            site: n.span.clone(),
                            declared: false,
                            derived: false,
                            detail: false,
                            hint: def.help.to_string(),
                            objects: vec![q_id(&s.id)],
                            anchor: None,
                        },
                        vec![i],
                    ));
                    continue;
                }
                let Some(raw) = literal(n) else { continue };
                let kind = if def.kind == protos::Kind::Scale { Kind::Ratio } else { kind_of(def.kind) };
                let axis = ["X", "Y", "Z"];
                let label = match index {
                    // A point's or a placement's coordinates; other lists (a size per point) count.
                    Some(k) if k < 3 && matches!(def.kind, protos::Kind::Point | protos::Kind::Point2 | protos::Kind::Rotation | protos::Kind::Scale) => format!("{} {}", def.label, axis[k]),
                    Some(k) => format!("{} {k}", def.label),
                    None => def.label.to_string(),
                };
                let value = shown(kind, raw);
                let (min, max) = if def.max > def.min { (shown(kind, def.min), shown(kind, def.max)) } else { (value.min(0.0), value.abs().max(1.0) * 4.0) };
                let mut anchor = field_anchor(op, name, index).map(|a| a.transformed(&frame[..]));
                if name == "at" {
                    if let Some(k) = index.filter(|k| *k < 3) {
                        let parent = s.parent.and_then(|j| ev.objects.get(base + j)).map(|o| o.world).unwrap_or(*frame);
                        let at = [frame[0][3], frame[1][3], frame[2][3]];
                        let mut axes = vec![[0.0; 3]; 3];
                        axes[k] = xf_dir(&parent[..], [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]][k]);
                        anchor = Some(Anchor::Point { at, axes });
                    }
                }
                all.push((
                    Param {
                        key,
                        instance: inst.name.clone(),
                        label,
                        group: group.clone(),
                        kind,
                        unit: kind.default_unit().to_string(),
                        value: PValue::Num(value),
                        min,
                        max,
                        step: if def.step > 0.0 { shown(kind, def.step) } else { 0.1 },
                        choices: Vec::new(),
                        site: n.span.clone(),
                        declared: false,
                        derived: false,
                        detail: matches!(name, "segments" | "rings" | "sides" | "cell"),
                        hint: def.help.to_string(),
                        objects: vec![q_id(&s.id)],
                        anchor,
                    },
                    vec![i],
                ));
            }
        }
    }
    let calls = call_params(inst, &all);
    all.extend(calls);
    all.extend(pattern_params(inst));
    all.extend(settings_params(inst));
    // Declared params take their written handle or a field's, with reused or measured rates.
    for k in 0..all.len() {
        if !all[k].0.declared {
            continue;
        }
        let name = split_id(&all[k].0.key).map(|(_, n)| n.to_string()).unwrap_or_default();
        if let Some(a) = declared_anchor(&inst.script, &fields_of, ev, base, &name, &all[k].0.key, rates) {
            all[k].0.anchor = Some(a);
        }
    }
    all
}

/// The arguments the top-level calls of script fns pass, each a site under its call
/// (`bow.len`: the part the call makes, and the fn's parameter) where no field in `listed` is
/// that value already: a part a fn makes is set by what its call passes, the fn's code being
/// every call's.
fn call_params(inst: &Instance, listed: &[(Param, Vec<usize>)]) -> Vec<(Param, Vec<usize>)> {
    let p = &inst.script.program;
    let q_id = |site: &str| part_id(&inst.name, site);
    let seen: HashSet<(usize, usize)> = listed.iter().map(|(q, _)| (q.site.start, q.site.end)).collect();
    // Each call and the parts it makes, its value first.
    let mut calls: Vec<(&Range<usize>, Vec<usize>)> = Vec::new();
    for (i, s) in p.solids.iter().enumerate() {
        let Some(call) = &s.call else { continue };
        match calls.iter_mut().find(|(span, _)| **span == call.span) {
            Some((_, made)) => made.push(i),
            None => calls.push((&call.span, vec![i])),
        }
    }
    let mut out = Vec::new();
    for (_, mut made) in calls {
        made.sort_by_key(|&j| !p.solids[j].call.as_ref().is_some_and(|c| c.value));
        let first = &p.solids[made[0]];
        let group = edit::call_label(p, first).unwrap_or_else(|| pretty(&first.id));
        for (param, arg) in edit::call_args(p, first) {
            let values: Vec<(String, &Node)> = match &arg.kind {
                SKind::Array { items, .. } if items.len() <= MAX_LIST_SITES => items.iter().enumerate().map(|(k, n)| (format!("{param}.{k}"), n)).collect(),
                _ => vec![(param.to_string(), arg)],
            };
            for (path, n) in values {
                let Some(raw) = literal(n).filter(|_| !seen.contains(&(n.span.start, n.span.end))) else { continue };
                // The part's name and the parameter must name this argument.
                let Some(owner) = made.iter().map(|&j| &p.solids[j]).find(|o| edit::site_span(p, &format!("{}.{path}", o.id)).is_ok_and(|r| r == n.span)) else { continue };
                let written = text(p, &n.span);
                let kind = match kind_of_literal(written) {
                    Kind::Number if path == param && !written.contains(['.', 'e']) => Kind::Count,
                    kind => kind,
                };
                let value = shown(kind, raw);
                out.push((
                    Param {
                        key: q_id(&format!("{}.{path}", owner.id)),
                        instance: inst.name.clone(),
                        label: pretty(&path.replace('.', " ")),
                        group: group.clone(),
                        kind,
                        unit: kind.default_unit().to_string(),
                        value: PValue::Num(value),
                        min: if value > 0.0 { 0.0 } else { value * 2.0 },
                        max: value.abs().max(1.0) * 4.0,
                        step: if kind == Kind::Count { 1.0 } else { 0.1 },
                        choices: Vec::new(),
                        site: n.span.clone(),
                        declared: false,
                        derived: false,
                        detail: false,
                        hint: format!("what {group} passes"),
                        objects: made.iter().map(|&j| q_id(&p.solids[j].id)).collect(),
                        anchor: None,
                    },
                    made.clone(),
                ));
            }
        }
    }
    out
}

/// A cage's rows: each point written as three numbers (X, Y and Z in mm, with a point handle
/// at the built point, in the solid's frame), each face written as indices (a read-out: the
/// points it joins).
fn cage_rows(inst: &Instance, s: &Solid, i: usize, field: &str, items: &[Node], op: &Op, def: &protos::Field) -> Vec<(Param, Vec<usize>)> {
    const AXES: [V3; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let built = match op {
        Op::Cage { points, .. } if points.len() == items.len() => Some(points),
        _ => None,
    };
    let face = field == "faces";
    let rows = items.iter().enumerate().filter_map(|(k, n)| {
        let SKind::Array { items: written, .. } = &n.kind else { return None };
        let raw = written.iter().map(literal).collect::<Option<Vec<f64>>>()?;
        let (label, kind, value, anchor) = if face {
            (format!("Face {k}"), Kind::Count, PValue::Point(raw), None)
        } else if raw.len() == 3 {
            let at = built.map(|b| Anchor::Point { at: b[k], axes: AXES.to_vec() });
            (format!("Point {k}"), Kind::Point, PValue::Point(raw.iter().map(|v| shown(Kind::Point, *v)).collect()), at)
        } else {
            return None;
        };
        let param = Param {
            key: part_id(&inst.name, &format!("{}.{field}.{k}", s.id)),
            instance: inst.name.clone(),
            label,
            group: edit::call_label(&inst.script.program, s).unwrap_or_else(|| pretty(&s.id)),
            kind,
            unit: kind.default_unit().to_string(),
            value,
            min: 0.0,
            max: 0.0,
            step: if face { 1.0 } else { 0.1 },
            choices: Vec::new(),
            site: n.span.clone(),
            declared: false,
            derived: face,
            detail: face,
            hint: def.help.to_string(),
            objects: vec![part_id(&inst.name, &s.id)],
            anchor,
        };
        Some((param, vec![i]))
    });
    rows.collect()
}

/// The wireframes of the cages among a part's solids (`within`), in the model frame; a point
/// with a row in `params` names it.
fn cage_wires(inst: &Instance, ev: &Evaluated, base: usize, within: &[usize], params: &[Param]) -> Vec<Wire> {
    let p = &inst.script.program;
    let rows: HashSet<&str> = params.iter().map(|q| q.key.as_str()).collect();
    let mut out = Vec::new();
    for &i in within {
        let Op::Cage { points, faces, .. } = base_op(p, i) else { continue };
        let frame = ev.objects.get(base + i).map_or(IDENTITY, |o| o.world);
        let id = &p.solids[i].id;
        let mut edges: Vec<[u32; 2]> = faces.iter().flat_map(|f| (0..f.len()).map(|j| {
            let (a, b) = (f[j], f[(j + 1) % f.len()]);
            [a.min(b), a.max(b)]
        })).collect();
        edges.sort_unstable();
        edges.dedup();
        let keys = (0..points.len()).map(|k| Some(part_id(&inst.name, &format!("{id}.points.{k}"))).filter(|key| rows.contains(key.as_str()))).collect();
        out.push(Wire { object: part_id(&inst.name, id), points: points.iter().map(|q| xf_point(&frame[..], *q)).collect(), edges, keys });
    }
    out
}

/// The site of a fill's grown pattern (`vase#turing`, then `vase#turing2`..): a `#` is in no
/// solid's id, so it is no field path.
const PATTERN_SITE: char = '#';

/// Whether `key` is a row whose edit leaves the shape as it is, so nothing previews it: a grown
/// pattern's (an edit regrows it) or a region's print setting (an edit slices again).
pub fn commits_only(key: &str) -> bool {
    split_id(key).is_some_and(|(_, site)| site.contains(PATTERN_SITE))
}

/// The tag of a region's print settings rows (`settings1#settings.wall_loops`, `#settings2` for
/// a second statement on the same solid): no pattern is called that.
const SETTINGS_TAG: &str = "settings";

/// The print setting a region's row (`instance/solid#settings.key`) writes; none for any other.
pub fn region_setting(key: &str) -> Option<&'static str> {
    let (_, site) = split_id(key)?;
    let (_, row) = site.split_once(PATTERN_SITE)?;
    let (tag, setting) = row.split_once('.')?;
    let n = tag.strip_prefix(SETTINGS_TAG)?;
    (n.is_empty() || n.parse::<usize>().is_ok()).then_some(())?;
    fab_plate::REGION_SETTINGS.iter().copied().find(|k| *k == setting)
}

/// The print settings the instance's script sets in volumes (`m.settings(box1, {..})`): each
/// statement on one solid gives a row per region setting ([`fab_plate::REGION_SETTINGS`]: the
/// infill's density and pattern, the wall loops) in that solid's group, beside its size and
/// place. A setting written as a literal is edited at it; one not written holds the table's
/// value here (the panel shows the part's own in its place) and an edit writes it in. One written
/// as code is not listed.
fn settings_params(inst: &Instance) -> Vec<(Param, Vec<usize>)> {
    let p = &inst.script.program;
    let mut out = Vec::new();
    for (solid, tag, opts, _) in settings_statements(p) {
        let SKind::Object { fields, close, .. } = &opts.kind else { continue };
        let s = &p.solids[solid];
        let group = edit::call_label(p, s).unwrap_or_else(|| pretty(&s.id));
        for key in fab_plate::REGION_SETTINGS {
            let Some(def) = fab_plate::print_setting(key) else { continue };
            let (site, text) = match fields.iter().find(|f| f.name.as_deref() == Some(*key)).map(|f| &f.value) {
                None => (*close..*close, def.default.to_string()),
                Some(v) => match &v.kind {
                    SKind::Str(t) => (v.span.clone(), t.clone()),
                    SKind::Number(x) => (v.span.clone(), format!("{x}")),
                    _ => continue,
                },
            };
            let Ok(text) = fab_plate::checked_value(key, &text) else { continue };
            let (kind, value, min, max, step, choices) = match def.kind {
                fab_plate::SettingKind::Choice(list) => {
                    let list: Vec<(String, String)> = list.iter().filter(|(v, _)| crate::paint::region_takes_pattern(v)).map(|(v, l)| (l.to_string(), format!("\"{v}\""))).collect();
                    let Some(at) = list.iter().position(|(_, w)| w.trim_matches('"') == text) else { continue };
                    (Kind::Choice, PValue::Choice(at), 0.0, 0.0, 0.0, list)
                }
                fab_plate::SettingKind::Number { min, max, step, unit } => {
                    let Some(x) = fab_plate::setting_number(&text) else { continue };
                    match unit {
                        "%" => (Kind::Ratio, PValue::Num(x / 100.0), min / 100.0, max / 100.0, step / 100.0, Vec::new()),
                        _ => (Kind::Count, PValue::Num(x), min, max, step, Vec::new()),
                    }
                }
                fab_plate::SettingKind::Bool => continue,
            };
            out.push((
                Param {
                    key: part_id(&inst.name, &format!("{}{PATTERN_SITE}{tag}.{key}", s.id)),
                    instance: inst.name.clone(),
                    label: def.label.to_string(),
                    group: group.clone(),
                    kind,
                    unit: kind.default_unit().to_string(),
                    value,
                    min,
                    max,
                    step,
                    choices,
                    site,
                    declared: false,
                    derived: false,
                    detail: false,
                    hint: def.hint.to_string(),
                    objects: vec![part_id(&inst.name, &s.id)],
                    anchor: None,
                },
                vec![solid],
            ));
        }
    }
    out
}

/// The program's region statements (`m.settings(box1, {..})`), each with its solid, its rows'
/// tag (`settings`, `settings2` for a second on the same solid), its settings object and its
/// numbered id (`settings#3`).
fn settings_statements(p: &Program) -> Vec<(usize, String, &Node, String)> {
    use makepad_model_script::lang::Arg;
    let syn = &p.names.syntax;
    let mut out = Vec::new();
    let mut seen: HashMap<usize, usize> = HashMap::new();
    for n in edit::numbered_roots(p, "settings") {
        let (Some(Arg::Solid(solid)), Some((_, span))) = (n.root.get("region"), n.args.iter().find(|(a, _)| a == "opts")) else { continue };
        let mut opts: Option<&Node> = None;
        syn.walk(&mut |_, x| {
            if x.span == *span && matches!(x.kind, SKind::Object { ty: None, .. }) {
                opts = Some(x);
            }
        });
        let Some(opts) = opts else { continue };
        let k = seen.entry(*solid).or_default();
        *k += 1;
        out.push((*solid, if *k > 1 { format!("{SETTINGS_TAG}{k}") } else { SETTINGS_TAG.to_string() }, opts, n.id.clone()));
    }
    out
}

/// The edit that takes a region's print setting row's setting (`instance/solid#settings.key`)
/// out of its statement: the region prints with the part's own again. A statement's only
/// setting takes the statement with it (a statement sets one at least), and a box's only
/// statement the box, as its delete does. One its statement does not write is refused, saying
/// so.
pub fn reset_region_setting(c: &Compiled, key: &str) -> Result<Change, String> {
    let setting = region_setting(key).ok_or_else(|| format!("'{key}' is no print settings box's setting"))?;
    let (inst, site) = split_id(key).and_then(|(i, site)| Some((c.instance(i)?, site))).ok_or_else(|| format!("no instance for '{key}'"))?;
    let (solid, tag) = site.split_once(PATTERN_SITE).and_then(|(solid, row)| Some((solid, row.split_once('.')?.0))).ok_or_else(|| format!("'{key}' is no print settings box's setting"))?;
    let p = &inst.script.program;
    let statements: Vec<_> = settings_statements(p).into_iter().filter(|(s, ..)| p.solids[*s].id == solid).collect();
    let (_, _, opts, id) = statements.iter().find(|(_, t, ..)| t == tag).ok_or_else(|| format!("no print settings statement on '{solid}'"))?;
    let SKind::Object { fields, open, .. } = &opts.kind else { return Err(format!("no print settings statement on '{solid}'")) };
    let at = fields.iter().position(|f| f.name.as_deref() == Some(setting)).ok_or_else(|| format!("the box does not set '{setting}'"))?;
    if fields.len() == 1 {
        let gone = if statements.len() == 1 { solid.to_string() } else { id.clone() };
        return Ok(Change::Script { file: inst.script.file.clone(), edit: Edit::Remove { solid: gone } });
    }
    // the entry with what follows it up to the next, or (the last) with what goes before it
    let range = match fields.get(at + 1) {
        Some(next) => fields[at].span.start..next.span.start,
        None => at.checked_sub(1).map_or(open + 1, |i| fields[i].span.end)..fields[at].span.end,
    };
    Ok(Change::Script { file: inst.script.file.clone(), edit: Edit::Splice { range, text: String::new() } })
}

/// A region's print setting row's edit: its literal rewritten, or the setting written into the
/// statement's settings. A value out of the setting's range is refused, saying why.
fn settings_edit(c: &Compiled, p: &Param, key: &str, value: &PValue) -> Result<Change, String> {
    let inst = c.instance(&p.instance).ok_or_else(|| format!("no instance '{}'", p.instance))?;
    let text = match value {
        PValue::Num(v) if p.kind == Kind::Ratio => format!("\"{}%\"", (v * 1000.0).round() / 10.0),
        PValue::Num(v) => format!("{}", v.round()),
        PValue::Choice(i) => p.choices.get(*i).map(|(_, s)| s.clone()).ok_or("no such choice")?,
        _ => return Err(format!("'{}' takes a number", p.label)),
    };
    fab_plate::checked_value(key, text.trim_matches('"'))?;
    let text = if p.site.is_empty() { format!(" {key}: {text}") } else { text };
    Ok(Change::Script { file: inst.script.file.clone(), edit: Edit::Splice { range: p.site.clone(), text } })
}

/// The patterns grown by the instance's fills (`m.fill(vase, Mix{.. field: Turing{..}})`, or a
/// `Lichtenberg{..}`, `Scales{..}`, `Physarum{..}` or `Flow{..}`), each a
/// group of rows on the part it fills: its growth (the live step counter, pause and resume), its
/// preset, scale, seed and steps, its model's own rates and its depth. A field written as a
/// literal is edited at it; one written as a declared parameter is that parameter's row; one not
/// written shows its default and an edit writes it in. A field worked out from others is not
/// listed.
fn pattern_params(inst: &Instance) -> Vec<(Param, Vec<usize>)> {
    use fab_paint_field::pattern::{Model, Spec};
    use makepad_model_script::lang::{Arg, MixField};
    let p = &inst.script.program;
    let syn = &p.names.syntax;
    let q_id = |site: &str| part_id(&inst.name, site);
    let mut out = Vec::new();
    let mut seen: HashMap<usize, usize> = HashMap::new();
    for root in p.roots.iter().filter(|r| r.kind == "fill") {
        let (Some(Arg::Solid(solid)), Some(Arg::Mix(mix))) = (root.get("region"), root.get("material")) else { continue };
        let MixField::Pattern(pa) = &mix.field else { continue };
        let (Some(proto), Some(at)) = (protos::proto(pa.kind), pa.loc.and_then(|l| p.names.offset(l))) else { continue };
        // its object: the innermost of its type over where it was read (its brace)
        let mut node: Option<Node> = None;
        syn.walk(&mut |_, n| {
            if n.span.contains(&at) && matches!(&n.kind, SKind::Object { ty: Some(t), .. } if t == pa.kind) && node.as_ref().is_none_or(|o| n.span.len() < o.span.len()) {
                node = Some(n.clone());
            }
        });
        let Some(node) = node else { continue };
        let SKind::Object { fields: written, .. } = &node.kind else { continue };
        let Ok(spec) = Spec::new(Some(pa.preset), &pa.values) else { continue };
        let n = seen.entry(*solid).or_default();
        *n += 1;
        let s = &p.solids[*solid];
        let tag = format!("{}{}", pa.kind.to_lowercase(), if *n > 1 { n.to_string() } else { String::new() });
        let group = if *n > 1 { format!("Pattern {n}") } else { "Pattern".to_string() };
        // an unwritten field goes in before the closing brace
        let end = node.span.end.saturating_sub(1);
        let row = |key: &str, label: &str, kind: Kind, value: PValue, site: Range<usize>, def: Option<&protos::Field>, choices: Vec<(String, String)>| Param {
            key: q_id(&format!("{}{PATTERN_SITE}{tag}.{key}", s.id)),
            instance: inst.name.clone(),
            label: label.to_string(),
            group: group.clone(),
            kind,
            unit: kind.default_unit().to_string(),
            value,
            min: def.map_or(0.0, |d| if kind.is_length() { shown(kind, d.min) } else { d.min }),
            max: def.map_or(0.0, |d| if kind.is_length() { shown(kind, d.max) } else { d.max }),
            step: def.map_or(0.0, |d| if kind.is_length() { shown(kind, d.step) } else { d.step }),
            choices,
            site,
            declared: false,
            derived: kind == Kind::Growth,
            detail: key == "depth",
            hint: def.map_or_else(|| "how far it has grown toward the steps it prints at; pause it to look, resume to go on".to_string(), |d| d.help.to_string()),
            objects: vec![q_id(&s.id)],
            anchor: None,
        };
        // the k-th pattern on its part: what the growth row follows
        out.push((row("growth", "Grown", Kind::Growth, PValue::Num((*n - 1) as f64), end..end, None, Vec::new()), vec![*solid]));
        let family: &[&str] = match spec.model {
            Model::GrayScott { .. } => &["feed", "kill"],
            Model::Activator { .. } => &["saturation"],
            Model::Aggregation { .. } => &["stick", "hits", "roots"],
            Model::Scales { .. } => &["edge", "fill"],
            Model::Physarum { .. } => &["sense", "turn", "spread"],
            Model::Flow { .. } => &["length", "width", "swirl", "taper"],
        };
        for key in ["preset", "scale", "seed", "steps"].iter().chain(family).chain(&["depth"]) {
            let Some(def) = proto.field(key) else { continue };
            let entry = written.iter().find(|e| e.name.as_deref() == Some(*key));
            let site = match entry.map(|e| &e.value) {
                None => end..end,
                Some(v) if matches!(v.kind, SKind::Number(_) | SKind::Name(_)) || literal(v).is_some() => v.span.clone(),
                // a declared parameter's own row, or worked out
                Some(_) => continue,
            };
            let (kind, value, choices) = match *key {
                "preset" => {
                    let protos::Kind::Choice(names) = def.kind else { continue };
                    let at = names.iter().position(|n| *n == spec.preset.name).unwrap_or(0);
                    (Kind::Choice, PValue::Choice(at), names.iter().map(|n| (pretty(n), format!("@{n}"))).collect())
                }
                "scale" => (Kind::Length, PValue::Num(spec.scale), Vec::new()),
                "depth" => (Kind::Length, PValue::Num(spec.depth), Vec::new()),
                "seed" => (Kind::Seed, PValue::Num(spec.seed as f64), Vec::new()),
                "steps" => (Kind::Count, PValue::Num(spec.steps as f64), Vec::new()),
                "sense" | "turn" => match spec.model {
                    Model::Physarum { sense, turn, .. } => (Kind::Angle, PValue::Num(if *key == "sense" { sense } else { turn }), Vec::new()),
                    _ => continue,
                },
                "roots" | "hits" => match spec.model {
                    Model::Aggregation { roots, hits, .. } => (Kind::Count, PValue::Num(if *key == "roots" { roots } else { hits } as f64), Vec::new()),
                    _ => continue,
                },
                _ => {
                    let v = match (spec.model, *key) {
                        (Model::GrayScott { feed, .. }, "feed") => feed,
                        (Model::GrayScott { kill, .. }, "kill") => kill,
                        (Model::Activator { saturation }, _) => saturation,
                        (Model::Aggregation { stick, .. }, "stick") => stick,
                        (Model::Scales { edge, .. }, "edge") => edge,
                        (Model::Scales { fill, .. }, "fill") => fill,
                        (Model::Physarum { spread, .. }, "spread") => spread,
                        (Model::Flow { length, .. }, "length") => length,
                        (Model::Flow { width, .. }, "width") => width,
                        (Model::Flow { swirl, .. }, "swirl") => swirl,
                        (Model::Flow { taper, .. }, "taper") => taper,
                        _ => continue,
                    };
                    (Kind::Number, PValue::Num(v), Vec::new())
                }
            };
            out.push((row(key, def.label, kind, value, site, Some(def), choices), vec![*solid]));
        }
    }
    out
}

/// A pattern row's edit: its literal rewritten, or the field written into its pattern's object.
fn pattern_edit(c: &Compiled, p: &Param, site: &str, value: &PValue) -> Result<Change, String> {
    let inst = c.instance(&p.instance).ok_or_else(|| format!("no instance '{}'", p.instance))?;
    let field = site.rsplit_once('.').map_or(site, |(_, f)| f);
    let written = inst.script.source().get(p.site.clone()).unwrap_or("");
    let text = match value {
        PValue::Num(v) => write_like(written, p.kind, *v),
        PValue::Choice(i) => p.choices.get(*i).map(|(_, s)| s.clone()).ok_or("no such choice")?,
        _ => return Err(format!("'{}' takes a number", p.label)),
    };
    let text = if p.site.is_empty() { format!(" {field}: {text}") } else { text };
    Ok(Change::Script { file: inst.script.file.clone(), edit: Edit::Splice { range: p.site.clone(), text } })
}

/// An instance's script recompiled from `source` with its parameters at their values in
/// this instance (by name), one of them (`moved`) at another.
pub fn compile_pinned(s: &Script, source: &str, moved: Option<(&str, f64)>) -> Result<Script, String> {
    let first = |d: Vec<makepad_model_script::lang::Diag>| d.first().map(|d| d.to_string()).unwrap_or_default();
    // A param is named by its `let`, which the syntax alone gives; only a param call reads the
    // value at its place, so every named place takes its name's, and the source compiles once.
    let names = if source == s.source() { s.program.names.clone() } else { makepad_model_script::lang::names_of(source).map_err(first)? };
    let mut overrides = HashMap::new();
    for (loc, name) in names.named() {
        let v = match moved {
            Some((m, v)) if m == name => v,
            _ => match s.param(name) {
                Some((v, _)) => v,
                None => continue,
            },
        };
        overrides.insert(loc, v);
    }
    program::compile_with(&s.file, source, Some(names), &s.assets, &s.facets, &s.pictures, &s.context, &s.env, overrides, &s.solved, &s.skip, &|f| s.paint_files.get(f).cloned()).map_err(first)
}

/// The solids a part (`root`) is built from, and the areas its script marks
/// ([`program::area_solids`]: the trees kept out, print settings): what its panel and handles
/// edit.
fn part_solids(p: &Program, root: usize) -> Vec<usize> {
    let areas = program::area_solids(p);
    (0..p.solids.len()).filter(|i| is_within(p, *i, root) || areas.contains(i)).collect()
}

fn is_within(p: &Program, i: usize, root: usize) -> bool {
    let mut at = Some(i);
    while let Some(j) = at {
        if j == root {
            return true;
        }
        at = p.solids[j].parent;
    }
    false
}

/// The handle a `handle(name, ...)` call gives a declared param, in the model frame (mm).
fn written_handle(s: &Script, name: &str) -> Option<Anchor> {
    use makepad_model_script::lang::{Arg, V};
    let p = &s.program;
    let syn = &p.names.syntax;
    for root in p.roots.iter().filter(|r| r.kind == "handle") {
        // The parameter it is for: the call's first argument as written.
        let at = root.loc.and_then(|l| p.names.offset(l))?;
        let mut first: Option<String> = None;
        syn.walk(&mut |_, node: &Node| {
            if let SKind::Call { open, args, .. } = &node.kind {
                if (*open == at || node.span.start == at) && first.is_none() {
                    first = args.first().map(|a| a.text(syn.source()).trim().to_string());
                }
            }
        });
        if first.as_deref() != Some(name) {
            continue;
        }
        let Some(Arg::Value(V::Obj(o))) = root.get("shape") else { continue };
        let p3 = |k: &str| -> Option<V3> {
            match o.get(k)? {
                V::Arr(a) if a.len() == 3 => Some([a[0].num()? * UNIT, a[1].num()? * UNIT, a[2].num()? * UNIT]),
                _ => None,
            }
        };
        let dir = |k: &str, d: V3| -> V3 {
            match o.get(k) {
                Some(V::Arr(a)) if a.len() == 3 => [a[0].num().unwrap_or(d[0]), a[1].num().unwrap_or(d[1]), a[2].num().unwrap_or(d[2])],
                _ => d,
            }
        };
        return Some(match o.proto.as_deref()? {
            "Arrow" => Anchor::Linear { from: p3("from").unwrap_or([0.0; 3]), to: p3("to")?, rate: 1.0 },
            "Ring" => Anchor::Radius { center: p3("center").unwrap_or([0.0; 3]), axis: unit(dir("axis", [0.0, 1.0, 0.0])), radius: o.get("radius")?.num()? * UNIT, rate: 1.0 },
            "Stepper" => Anchor::Count { at: p3("at")?, axis: unit(dir("axis", [1.0, 0.0, 0.0])) },
            _ => return None,
        });
    }
    None
}

/// A declared param's handle: the one its `handle(...)` call writes, else the anchor of the
/// first solid field written as its name (or an expression of it). Its rate comes from `rates`
/// when supplied, otherwise from evaluating the program with it moved.
fn declared_anchor(s: &Script, fields_of: &[Vec<(String, &Node)>], ev: &Evaluated, base: usize, name: &str, key: &str, rates: Option<&[PartParams]>) -> Option<Anchor> {
    let p = &s.program;
    let q = p.params.iter().find(|q| q.name.as_deref() == Some(name))?;
    let old_rate = |old: &[PartParams]| old.iter().flat_map(|pp| pp.params.iter()).find(|p| p.key == key).and_then(|p| p.anchor.as_ref()).and_then(Anchor::rate);
    if let Some(a0) = written_handle(s, name) {
        if let Some(old) = rates {
            return Some(match old_rate(old) {
                Some(rate) => a0.with_rate(rate),
                None => a0,
            });
        }
        let kind = q.call.kind.as_deref().and_then(Kind::parse).unwrap_or_else(|| kind_of_literal(q.value_span.as_ref().map(|r| text(p, r)).unwrap_or("")));
        let h = if kind.is_length() { 0.0001 } else { 0.01 };
        let moved = compile_pinned(s, p.source(), Some((name, q.call.value + h))).ok()?;
        let a1 = written_handle(&moved, name)?;
        let dir = match &a0 {
            Anchor::Linear { from, to, .. } => unit(sub(*to, *from)),
            _ => [0.0; 3],
        };
        return match (a0.measure(dir), a1.measure(dir)) {
            (Some(m0), Some(m1)) => {
                let rate = (m1 - m0) / shown(kind, h);
                (rate.abs() > 1e-9).then(|| a0.with_rate(rate))
            }
            _ => Some(a0),
        };
    }
    // The field: a solid's field whose value names the param.
    let mut target: Option<(usize, String, Option<usize>)> = None;
    for (i, fields) in fields_of.iter().enumerate() {
        for &(ref fname, value) in fields {
            let fname = fname.as_str();
            let items: Vec<(Option<usize>, &Node)> = match &value.kind {
                SKind::Array { items: a, .. } => a.iter().enumerate().map(|(k, n)| (Some(k), n)).collect(),
                _ => vec![(None, value)],
            };
            for (k, n) in items {
                if uses_name(n, name) && field_anchor(base_op(p, i), fname, k).is_some() {
                    target = Some((i, fname.to_string(), k));
                    break;
                }
            }
            if target.is_some() {
                break;
            }
        }
        if target.is_some() {
            break;
        }
    }
    let (i, field, index) = target?;
    let frame = ev.objects.get(base + i)?.world;
    let a0 = field_anchor(base_op(p, i), &field, index)?.transformed(&frame[..]);
    if let Some(old) = rates {
        return Some(a0.with_rate(old_rate(old)?));
    }
    // Move the param by a small step and see how far the handle goes.
    let kind = q.call.kind.as_deref().and_then(Kind::parse).unwrap_or_else(|| kind_of_literal(q.value_span.as_ref().map(|r| text(p, r)).unwrap_or("")));
    let h = if kind.is_length() { 0.001 / UNIT * 100.0 } else { 0.01 };
    let moved = compile_pinned(s, p.source(), Some((name, q.call.value + h))).ok()?;
    let mi = moved.program.solids.iter().position(|s| s.id == p.solids[i].id)?;
    let a1 = field_anchor(base_op(&moved.program, mi), &field, index)?.transformed(&frame[..]);
    let dir = match &a0 {
        Anchor::Linear { from, to, .. } => unit(sub(*to, *from)),
        _ => [0.0; 3],
    };
    let (m0, m1) = (a0.measure(dir)?, a1.measure(dir)?);
    let rate = (m1 - m0) / shown(kind, h);
    if rate.abs() < 1e-9 {
        return None;
    }
    Some(a0.with_rate(rate))
}

/// A display value (mm, degrees) as the script reads it (metres, radians).
fn script_value(kind: Kind, v: f64) -> f64 {
    if kind.is_length() || kind == Kind::Point {
        v / UNIT
    } else if kind == Kind::Angle {
        v.to_radians()
    } else {
        v
    }
}

/// The change that sets parameter `key` to `value` (display units): a declared parameter's
/// value in its instance, or a part field's literal in its script.
pub fn edit(c: &Compiled, parts: &[PartParams], key: &str, value: &PValue) -> Result<Change, String> {
    let Some(p) = parts.iter().flat_map(|pp| pp.params.iter()).find(|q| q.key == key) else {
        return placement_edit(c, key, value).unwrap_or_else(|| Err(format!("no parameter '{key}'")));
    };
    if p.derived {
        return Err(format!("'{}' is worked out from other parameters", p.label));
    }
    let (inst, site) = split_id(key).and_then(|(i, site)| Some((c.instance(i)?, site))).ok_or_else(|| format!("no instance for '{key}'"))?;
    if let Some(setting) = region_setting(key) {
        return settings_edit(c, p, setting, value);
    }
    if site.contains(PATTERN_SITE) {
        return pattern_edit(c, p, site, value);
    }
    if p.declared {
        let v = match value {
            PValue::Num(v) => script_value(p.kind, *v),
            PValue::Bool(b) => if *b { 1.0 } else { 0.0 },
            _ => return Err(format!("'{}' takes a number", p.label)),
        };
        return Ok(Change::Param { instance: inst.name.clone(), name: site.to_string(), value: Some(v) });
    }
    let written = inst.script.source().get(p.site.clone()).unwrap_or("");
    let text = match value {
        PValue::Num(v) => write_like(written, p.kind, *v),
        PValue::Bool(b) => format!("{b}"),
        PValue::Choice(i) => p.choices.get(*i).map(|(_, s)| s.clone()).ok_or("no such choice")?,
        PValue::Point(v) if p.kind == Kind::Point => point_text(&inst.script.program, p, site, v)?,
        _ => return Err("points and profiles are edited one coordinate at a time".into()),
    };
    Ok(Change::Script { file: inst.script.file.clone(), edit: Edit::Set { site: site.to_string(), value: text } })
}

/// The fields that place a solid in the frame it is placed in: where its origin sits, and how
/// it is turned and scaled about it.
const PLACEMENT: [&str; 3] = ["at", "rotate", "scale"];

/// The change that writes a solid's placement field whole, in builder form: `hole.at` to a
/// point (mm), `hole.rotate` to degrees, `hole.scale` to a ratio or three. An element written as
/// an expression keeps it, offset by the difference (`20mm + tab/2` becomes `20mm + tab/2 + 5mm`,
/// a trailing number taking it); a number is rewritten in its own unit. `None`: `key` names no
/// solid's placement field.
fn placement_edit(c: &Compiled, key: &str, value: &PValue) -> Option<Result<Change, String>> {
    let (inst, site) = split_id(key).and_then(|(i, site)| Some((c.instance(i)?, site)))?;
    let (id, field) = site.rsplit_once('.').filter(|(_, f)| PLACEMENT.contains(f))?;
    let p = &inst.script.program;
    let i = p.solids.iter().position(|s| s.id == id)?;
    let s = &p.solids[i];
    let (at, rotate, scale) = crate::eval::own_placement(p, i).unwrap_or(([0.0; 3], [0.0; 3], [1.0; 3]));
    let (old, kind) = match field {
        "at" => (at, Kind::Length),
        "rotate" => (rotate, Kind::Angle),
        _ => (scale, Kind::Ratio),
    };
    let new = match value {
        PValue::Point(v) if v.len() == 3 => [v[0], v[1], v[2]],
        PValue::Num(k) if field == "scale" => [*k; 3],
        _ => return Some(Err(format!("'{id}.{field}' takes {}", if field == "scale" { "a ratio or three" } else { "three numbers" }))),
    };
    // A part a script fn makes is placed by what its call passes (the fn's code is every call's).
    let written = match s.call {
        None => part_fields(p, s).into_iter().find(|(n, _)| n == field).map(|(_, n)| n),
        Some(_) => match edit::part_value(p, s, field) {
            Ok(n) => Some(n),
            Err(x) => return Some(Err(x)),
        },
    };
    let code = match (value, written.map(|n| &n.kind)) {
        (PValue::Num(k), _) => fresh(kind, *k),
        (_, Some(SKind::Array { items, .. })) if items.len() == 3 => list((0..3).map(|k| element(p, &items[k], kind, old[k], new[k])).collect()),
        _ => list(new.map(|v| fresh(kind, v)).to_vec()),
    };
    Some(Ok(Change::Script { file: inst.script.file.clone(), edit: Edit::Field { solid: s.id.clone(), field: field.to_string(), code } }))
}

/// A capsule's ends, under what names and paints it.
fn capsule_ends(n: &makepad_csg_sdf::SdfNode) -> Option<(V3, V3)> {
    use makepad_csg_sdf::SdfNode;
    match n {
        SdfNode::Capsule { a, b, .. } => Some((*a, *b)),
        SdfNode::Id { of, .. } | SdfNode::Material { of, .. } => capsule_ends(of),
        _ => None,
    }
}

/// The changes that turn solid `id` of `instance` by `turn` about `pivot`, both in the model frame:
/// a capsule with no placement of its own moves its ends, anything else takes the placement a
/// turn about that point makes. What's written as an expression keeps it, offset by the difference.
pub fn turned_about(c: &Compiled, instance: &str, id: &str, pivot: V3, turn: &[[f64; 3]; 3]) -> Result<Vec<Change>, String> {
    let inst = c.instance(instance).ok_or_else(|| format!("no instance '{instance}'"))?;
    let p = &inst.script.program;
    let i = p.solids.iter().position(|s| s.id == id).ok_or_else(|| format!("no solid '{id}' in '{instance}'"))?;
    let s = &p.solids[i];
    // the turn and the pivot where the solid is placed
    let world = crate::align::parent_world(p, i);
    let l: [[f64; 3]; 3] = std::array::from_fn(|r| [world[r][0], world[r][1], world[r][2]]);
    let li = inverse3(&l).ok_or_else(|| format!("'{id}' is placed in a flat frame"))?;
    let local = orthonormal(&mat3_mul(&li, &mat3_mul(turn, &l)));
    let q = xf_point(&crate::weave::invert(&world), pivot);
    let moved = |x: V3| -> V3 {
        let d = sub(x, q);
        add(q, std::array::from_fn(|r| dot(local[r], d)))
    };
    let placement = crate::eval::own_placement(p, i);
    if let (None, Op::Field { shape, .. }) = (&placement, base_op(p, i)) {
        if let Some((a, b)) = capsule_ends(&shape.root) {
            let field = |name: &str, old: V3| -> Result<Change, String> {
                let written = match s.call {
                    None => part_fields(p, s).into_iter().find(|(n, _)| n == name).map(|(_, n)| n),
                    Some(_) => Some(edit::part_value(p, s, name)?),
                };
                let new = moved(old);
                let code = match written.map(|n| &n.kind) {
                    Some(SKind::Array { items, .. }) if items.len() == 3 => list((0..3).map(|k| element(p, &items[k], Kind::Length, old[k], new[k])).collect()),
                    _ => list(new.map(|v| fresh(Kind::Length, v)).to_vec()),
                };
                Ok(Change::Script { file: inst.script.file.clone(), edit: Edit::Field { solid: id.to_string(), field: name.to_string(), code } })
            };
            return Ok(vec![field("a", a)?, field("b", b)?]);
        }
    }
    let (at, rotate, _) = placement.unwrap_or(([0.0; 3], [0.0; 3], [1.0; 3]));
    let to = crate::eval::euler_xyz(&mat3_mul(&local, &crate::eval::rotation_xyz(rotate)));
    let key = |f: &str| part_id(instance, &format!("{id}.{f}"));
    let write = |f: &str, v: V3| placement_edit(c, &key(f), &PValue::Point(v.to_vec())).unwrap_or_else(|| Err(format!("'{id}' takes no {f}")));
    Ok(vec![write("at", moved(at))?, write("rotate", to)?])
}

/// A list written whole: a space keeps plain values apart; an expression or a negative value
/// takes a comma, or a minus after the first value would read as a subtraction.
fn list(elements: Vec<String>) -> String {
    let sep = if elements.iter().any(|t| t.contains(char::is_whitespace) || t.starts_with('-')) { ", " } else { " " };
    format!("[{}]", elements.join(sep))
}

/// A cage point row (`site`: `solid.points.k`) written at `v` (mm): each coordinate in its
/// own unit, those that did not move as written.
fn point_text(p: &Program, row: &Param, site: &str, v: &[f64]) -> Result<String, String> {
    let PValue::Point(old) = &row.value else { return Err(format!("'{}' is not a point", row.label)) };
    let (id, k) = site.rsplit_once('.').and_then(|(head, k)| Some((head.strip_suffix(".points")?, k.parse::<usize>().ok()?))).ok_or_else(|| format!("'{site}' is no cage point"))?;
    let solid = p.solids.iter().find(|s| s.id == id).ok_or_else(|| format!("no solid '{id}'"))?;
    let points = match solid.call {
        None => part_fields(p, solid).into_iter().find(|(n, _)| n == "points").map(|(_, n)| n),
        Some(_) => Some(edit::part_value(p, solid, "points")?),
    };
    let Some(SKind::Array { items, .. }) = points.map(|n| &n.kind) else { return Err(format!("'{id}' has no written points")) };
    let written = match items.get(k).map(|n| &n.kind) {
        Some(SKind::Array { items, .. }) if items.len() == v.len() && old.len() == v.len() => items,
        _ => return Err(format!("'{}' takes {} numbers", row.label, old.len())),
    };
    Ok(list(written.iter().zip(old).zip(v).map(|((n, a), b)| element(p, n, Kind::Length, *a, *b)).collect()))
}

/// A placement value written new: `0`, `12.5mm`, `30°`, `1.5`.
fn fresh(kind: Kind, v: f64) -> String {
    if v.abs() < 1e-9 && kind != Kind::Ratio {
        return "0".into();
    }
    write_like("", kind, v)
}

/// One element of a written placement at its new value (display units).
fn element(p: &Program, n: &Node, kind: Kind, old: f64, new: f64) -> String {
    let written = text(p, &n.span);
    if (new - old).abs() < 1e-9 {
        return written.to_string();
    }
    if literal(n).is_some() {
        return write_like(written, kind, new);
    }
    let d = new - old;
    // Arithmetic ending in `+ number` (or `-`): the number takes the difference (nothing binds
    // looser than its sign).
    if let SKind::Chain { rest, .. } = &n.kind {
        let arithmetic = rest.iter().all(|l| matches!(l.op.as_str(), "+" | "-" | "*" | "/" | "%"));
        if let Some(last) = rest.last().filter(|l| arithmetic && (l.op == "+" || l.op == "-")) {
            if let Some(v) = literal(&last.operand) {
                let t = if last.op == "-" { -shown(kind, v) } else { shown(kind, v) } + d;
                let head = p.source()[n.span.start..last.op_span.start].trim_end();
                if t.abs() < 1e-9 {
                    return head.to_string();
                }
                return format!("{head} {} {}", if t < 0.0 { "-" } else { "+" }, write_like(text(p, &last.operand.span), kind, t.abs()));
            }
        }
    }
    format!("{written} {} {}", if d < 0.0 { "-" } else { "+" }, fresh(kind, d.abs()))
}

/// What the view's gizmo does to a feature, on the plate, from where its drag began.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Gesture {
    /// Moved by this much (printer mm).
    Move(V3),
    /// Turned about the printer's vertical through the feature's origin (degrees,
    /// counter-clockwise seen from above).
    Turn(f64),
    /// Scaled about its origin.
    Scale(f64),
}

fn mat3_mul(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()))
}

fn inverse3(m: &[[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let c = |r: usize, k: usize| m[(r + 1) % 3][(k + 1) % 3] * m[(r + 2) % 3][(k + 2) % 3] - m[(r + 1) % 3][(k + 2) % 3] * m[(r + 2) % 3][(k + 1) % 3];
    let det = (0..3).map(|k| m[0][k] * c(0, k)).sum::<f64>();
    (det.abs() > 1e-12).then(|| std::array::from_fn(|i| std::array::from_fn(|j| c(j, i) / det)))
}

/// The nearest rotation to `m` (its columns made orthonormal).
fn orthonormal(m: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let col = |k: usize| [m[0][k], m[1][k], m[2][k]];
    let x = unit(col(0));
    let y = unit(sub(col(1), scale(x, dot(col(1), x))));
    let z = cross(x, y);
    std::array::from_fn(|r| [x[r], y[r], z[r]])
}

/// The placement field gesture `g` on `feature` (`instance/solid`, a solid of plate part `part`)
/// writes and its new value, as [`edit`] takes them: `solid.at` (mm, in the frame the solid is
/// placed in), `solid.rotate` (degrees) or `solid.scale`. A feature turns and scales about its own
/// origin, the point its `at` places, so a gesture changes that one field and nothing else: the
/// part stays where it is.
pub fn placed(c: &Compiled, ev: &Evaluated, part: usize, feature: &str, g: Gesture) -> Result<(String, PValue), String> {
    let (name, id) = split_id(feature).ok_or_else(|| format!("'{feature}' is not a feature"))?;
    let inst = c.instance(name).ok_or_else(|| format!("no instance '{name}'"))?;
    let p = &inst.script.program;
    let i = p.solids.iter().position(|s| s.id == id).ok_or_else(|| format!("no solid '{id}' in '{name}'"))?;
    let plate = ev.placed_frame(part).ok_or_else(|| format!("part {part} is not on the plate"))?;
    // The frame the solid is placed in, on the plate: its parent's (none: the model's).
    let parent = match p.solids[i].parent {
        Some(j) => ev.objects.iter().find(|o| o.name == part_id(name, &p.solids[j].id)).map(|o| o.world).ok_or_else(|| format!("'{}' has no frame", p.solids[j].id))?,
        None => IDENTITY,
    };
    let l: [[f64; 3]; 3] = std::array::from_fn(|r| std::array::from_fn(|k| (0..3).map(|m| plate[r][m] * parent[m][k]).sum()));
    let li = inverse3(&l).ok_or_else(|| format!("'{id}' is placed in a flat frame"))?;
    let (at, rotate, factor) = crate::eval::own_placement(p, i).unwrap_or(([0.0; 3], [0.0; 3], [1.0; 3]));
    let key = |f: &str| part_id(name, &format!("{id}.{f}"));
    Ok(match g {
        Gesture::Move(d) => {
            (key("at"), PValue::Point((0..3).map(|r| at[r] + (0..3).map(|k| li[r][k] * d[k]).sum::<f64>()).collect()))
        }
        Gesture::Turn(deg) => {
            let (s, co) = deg.to_radians().sin_cos();
            let turn = [[co, -s, 0.0], [s, co, 0.0], [0.0, 0.0, 1.0]];
            let r = crate::eval::rotation_xyz(rotate);
            let e = crate::eval::euler_xyz(&orthonormal(&mat3_mul(&li, &mat3_mul(&turn, &mat3_mul(&l, &r)))));
            (key("rotate"), PValue::Point(e.to_vec()))
        }
        Gesture::Scale(f) => {
            let s = factor.map(|x| x * f);
            (key("scale"), if s[0] == s[1] && s[1] == s[2] { PValue::Num(s[0]) } else { PValue::Point(s.to_vec()) })
        }
    })
}

/// Clamp and round a numeric value to a parameter's range and kind.
pub fn clamp_value(p: &Param, value: PValue) -> PValue {
    match value {
        PValue::Num(x) => {
            let x = if p.kind == Kind::Count { x.round() } else { x };
            PValue::Num(if p.max > p.min { x.clamp(p.min, p.max) } else { x })
        }
        other => other,
    }
}

/// One parameter as a Splash value: its key, label, kind, value with its unit, range and
/// whether it has a handle.
fn param_text(p: &Param) -> String {
    let q = makepad_script::syntax::write::quote;
    let num = |x: f64| {
        let r = (x * 1000.0).round() / 1000.0;
        format!("{r}")
    };
    let v = match &p.value {
        PValue::Num(x) => format!("{}{}", num(*x), p.unit),
        PValue::Bool(b) => format!("{b}"),
        PValue::Choice(i) => p.choices.get(*i).map(|c| c.1.clone()).unwrap_or_default(),
        PValue::Point(c) => list(c.iter().map(|x| format!("{}{}", num(*x), p.unit)).collect()),
        PValue::Profile(_) => "nil".into(),
    };
    let range = if p.kind.is_numeric() && p.value.num().is_some() { format!(" min: {}{u} max: {}{u}", num(p.min), num(p.max), u = p.unit) } else { String::new() };
    format!(
        "{{key: {} label: {} group: {} kind: @{} value: {v}{range}{}{}}}",
        q(&p.key),
        q(&p.label),
        q(&p.group),
        p.kind.name(),
        if p.derived { " derived: true" } else { "" },
        if p.anchor.is_some() { " handle: true" } else { "" }
    )
}

/// The parameter model as a Splash value (what `fab.params` answers asked for a solid or a key
/// prefix): per part, each parameter ([`param_text`]); `detail` lists the rarely tuned ones
/// (segments, rings, cells) too.
pub fn model_text(parts: &[PartParams], ev: &Evaluated, detail: bool) -> String {
    let q = makepad_script::syntax::write::quote;
    let mut out = String::from("[\n");
    for pp in parts {
        let name = ev.plate.placed_part(pp.part).map(|p| p.name.clone()).unwrap_or_default();
        out.push_str(&format!("    {{part: {} name: {} params: [\n", q(&pp.object), q(&name)));
        for p in pp.params.iter().filter(|p| detail || !p.detail) {
            out.push_str(&format!("        {}\n", param_text(p)));
        }
        out.push_str("    ]}\n");
    }
    out.push(']');
    out
}

/// The parameter model in short (what `fab.params` answers unfiltered): per part its declared
/// parameters in full, how many fields it has besides, and each solid that has some, by name
/// and count, in program order.
pub fn summary_text(parts: &[PartParams], ev: &Evaluated) -> String {
    let q = makepad_script::syntax::write::quote;
    let mut out = String::from("[\n");
    for pp in parts {
        let name = ev.plate.placed_part(pp.part).map(|p| p.name.clone()).unwrap_or_default();
        out.push_str(&format!("    {{part: {} name: {} params: [\n", q(&pp.object), q(&name)));
        let mut solids: Vec<(&str, usize)> = Vec::new();
        let mut at: HashMap<&str, usize> = HashMap::new();
        for p in &pp.params {
            if p.declared {
                out.push_str(&format!("        {}\n", param_text(p)));
                continue;
            }
            let solid = p.objects.first().map_or("", String::as_str);
            match at.get(solid) {
                Some(&k) => solids[k].1 += 1,
                None => {
                    at.insert(solid, solids.len());
                    solids.push((solid, 1));
                }
            }
        }
        let fields: usize = solids.iter().map(|(_, n)| n).sum();
        let listed: Vec<String> = solids.iter().map(|(s, n)| format!("{{solid: {} fields: {n}}}", q(s))).collect();
        out.push_str(&format!("    ] fields: {fields} solids: [{}]}}\n", listed.join(" ")));
    }
    out.push(']');
    out
}

/// The solids `solid` names (`instance/solid`, or a solid's id in each instance that has one)
/// with every solid built inside them (their inner calls, a blend's operands), as
/// `instance/solid` ids.
fn solids_within(c: &Compiled, solid: &str) -> BTreeSet<String> {
    let (only, id) = match split_id(solid) {
        Some((i, s)) => (Some(i), s),
        None => (None, solid),
    };
    let mut out = BTreeSet::new();
    for inst in c.instances.iter().filter(|i| only.is_none_or(|o| o == i.name)) {
        let p = &inst.script.program;
        let Some(root) = p.solids.iter().position(|s| s.id == id) else { continue };
        out.extend((0..p.solids.len()).filter(|j| is_within(p, *j, root)).map(|j| part_id(&inst.name, &p.solids[j].id)));
    }
    out
}

/// `parts` narrowed to the parameters that shape `solid` or a solid inside it, and whose key, or
/// site within its instance, starts with `prefix`; parts left with none are left out.
pub fn filtered(c: &Compiled, parts: &[PartParams], solid: Option<&str>, prefix: Option<&str>) -> Result<Vec<PartParams>, String> {
    let within = match solid {
        Some(s) => {
            let w = solids_within(c, s);
            if w.is_empty() {
                return Err(format!("no solid '{s}' (fab.tree lists them)"));
            }
            Some(w)
        }
        None => None,
    };
    let keep = |q: &Param| {
        within.as_ref().is_none_or(|w| q.objects.iter().any(|o| w.contains(o)))
            && prefix.is_none_or(|x| q.key.starts_with(x) || split_id(&q.key).is_some_and(|(_, site)| site.starts_with(x)))
    };
    Ok(parts
        .iter()
        .filter_map(|pp| {
            let params: Vec<Param> = pp.params.iter().filter(|q| keep(q)).cloned().collect();
            (!params.is_empty()).then(|| PartParams { part: pp.part, object: pp.object.clone(), params, objects: pp.objects.clone(), wires: pp.wires.clone() })
        })
        .collect())
}

/// The printed part (`instance/solid`) solid `i` of `inst` is in, if any.
fn printed_part(inst: &Instance, ev: &Evaluated, i: usize) -> Option<String> {
    let p = &inst.script.program;
    let mut at = Some(i);
    while let Some(j) = at {
        let id = part_id(&inst.name, &p.solids[j].id);
        if ev.plate.parts.iter().any(|part| part.object == id) {
            return Some(id);
        }
        at = p.solids[j].parent;
    }
    None
}

/// A stroke's points in the model frame (mm), as `m.stroke` reads them.
fn stroke_points(root: &makepad_model_script::lang::Root) -> Vec<[f64; 3]> {
    use makepad_model_script::lang::{Arg, V};
    let Some(Arg::Value(V::Arr(points))) = root.get("points") else { return Vec::new() };
    points
        .iter()
        .filter_map(|v| match v {
            V::Arr(a) if a.len() == 3 => Some([a[0].num()? * UNIT, a[1].num()? * UNIT, a[2].num()? * UNIT]),
            _ => None,
        })
        .collect()
}

/// How far `x` is outside `bounds` (0 inside).
fn outside(bounds: &([f64; 3], [f64; 3]), x: &[f64; 3]) -> f64 {
    (0..3).map(|k| (bounds.0[k] - x[k]).max(x[k] - bounds.1[k]).max(0.0).powi(2)).sum::<f64>().sqrt()
}

/// The paint statements of one kind in the scripts, `fill` (`m.fill(region, material)`) or
/// `stroke` (`m.stroke(points, radius, material)`), numbered in script order as edits name them
/// (`toy/fill#2` the second: the order they paint in, each over the ones before), as
/// `fab.params` lists them: each one's key, the printed part it paints (a fill's region's, a
/// stroke's nearest its first point) and its arguments as written. `instance` keeps one
/// instance's; `solid` (`instance/solid` or a solid's id) those painting that solid (a fill's
/// region is it, inside it or holds it; a stroke has a point on its bounds); `prefix` those whose
/// key, or name within the instance with an argument (`fill#2.material`), starts with it.
pub fn paint_text(c: &Compiled, ev: &Evaluated, kind: &str, instance: Option<&str>, solid: Option<&str>, prefix: Option<&str>) -> Vec<String> {
    use makepad_model_script::lang::Arg;
    let q = makepad_script::syntax::write::quote;
    let (only, id) = match solid.map(|s| split_id(s).map_or((None, s), |(i, s)| (Some(i), s))) {
        Some((i, s)) => (i, Some(s)),
        None => (None, None),
    };
    let mut out = Vec::new();
    for inst in c.instances.iter().filter(|i| instance.is_none_or(|o| o == i.name) && only.is_none_or(|o| o == i.name)) {
        let p = &inst.script.program;
        let painted = match id {
            Some(id) => match p.solids.iter().position(|s| s.id == id) {
                Some(s) => Some(s),
                None => continue,
            },
            None => None,
        };
        let bounds_of = |name: &str| ev.objects.iter().find(|o| o.name == name).and_then(|o| o.bounds);
        for stmt in makepad_model_script::lang::edit::numbered_roots(p, kind) {
            let (part, paints) = match kind {
                "stroke" => {
                    let points = stroke_points(stmt.root);
                    let radius = stmt.root.get("radius").and_then(|a| match a {
                        Arg::Value(v) => v.num().map(|r| r * UNIT),
                        _ => None,
                    });
                    let printed = ev.objects.iter().filter(|o| o.part.is_some() && split_id(&o.name).is_some_and(|(i, _)| i == inst.name));
                    let near = |o: &&crate::eval::ObjectInfo| o.bounds.as_ref().zip(points.first()).map_or(f64::INFINITY, |(b, x)| outside(b, x));
                    let part = printed.min_by(|a, b| near(a).total_cmp(&near(b))).map(|o| o.name.clone());
                    let paints = painted.is_none_or(|s| {
                        bounds_of(&part_id(&inst.name, &p.solids[s].id)).is_some_and(|b| points.iter().any(|x| outside(&b, x) <= radius.unwrap_or(0.0)))
                    });
                    (part, paints)
                }
                _ => {
                    let regions = match stmt.root.get("region") {
                        Some(Arg::Solid(r)) => vec![*r],
                        Some(Arg::Solids(rs)) => rs.clone(),
                        _ => Vec::new(),
                    };
                    let part = regions.iter().find_map(|&r| printed_part(inst, ev, r));
                    (part, painted.is_none_or(|s| regions.iter().any(|&r| is_within(p, r, s) || is_within(p, s, r))))
                }
            };
            if !paints {
                continue;
            }
            let key = part_id(&inst.name, &stmt.id);
            let keyed = |x: &str| [&key, &stmt.id].iter().any(|k| k.starts_with(x) || stmt.args.iter().any(|(n, _)| format!("{k}.{n}").starts_with(x)));
            if prefix.is_some_and(|x| !keyed(x)) {
                continue;
            }
            let part = part.map_or(String::new(), |o| format!(" part: {}", q(&o)));
            let args: Vec<String> = stmt.args.iter().map(|(n, at)| format!("{n}: {}", q(text(p, at)))).collect();
            out.push(format!("{{key: {}{part} {}}}", q(&key), args.join(" ")));
        }
    }
    out
}

/// One print colour of the plate, as the AI reads it: its name, its colour and what uses it.
#[derive(Clone, Debug, PartialEq)]
pub struct PaletteColor {
    pub name: String,
    /// What it shows as (linear RGB): its override, else the colour the design asks, else its
    /// default.
    pub rgb: [f32; 3],
    /// Named only by a material, not a project `color(..)`.
    pub implicit: bool,
    /// The placed parts that print in it as their own colour (`toy/toy`).
    pub parts: Vec<String>,
    /// The placed parts whose paint prints in it.
    pub paint: Vec<String>,
    /// The fill and stroke statements whose `Material{name}` names it (`toy/fill#2`).
    pub materials: Vec<String>,
}

/// The plate's print colours (the project's, then those only a material names) and what uses
/// each: what a rename (`rename(color: .., to: ..)`) renames.
pub fn palette(c: &Compiled, ev: &Evaluated) -> Vec<PaletteColor> {
    use makepad_model_script::lang::{Arg, V};
    fn names(v: &V, out: &mut Vec<String>) {
        match v {
            V::Obj(o) if o.proto.as_deref() == Some("Material") => {
                if let Some(V::Str(n)) = o.get("name") {
                    out.push(n.trim().to_string());
                }
            }
            V::Obj(o) => {
                if let Some(m) = o.get("materials") {
                    names(m, out);
                }
            }
            V::Arr(items) => items.iter().for_each(|v| names(v, out)),
            _ => {}
        }
    }
    let mut materials: Vec<(String, String)> = Vec::new();
    for inst in &c.instances {
        for kind in ["fill", "stroke"] {
            for stmt in edit::numbered_roots(&inst.script.program, kind) {
                let mut named = Vec::new();
                if let Some(Arg::Value(v)) = stmt.root.get("material") {
                    names(v, &mut named);
                }
                materials.extend(named.into_iter().map(|n| (n, part_id(&inst.name, &stmt.id))));
            }
        }
    }
    ev.colors
        .iter()
        .map(|k| {
            let parts = ev.plate.parts.iter().enumerate().filter(|(i, _)| ev.part_color(*i) == Some(k.name.as_str())).map(|(_, p)| p.object.clone()).collect();
            let paint = ev.plate.parts.iter().enumerate()
                .filter(|(i, _)| ev.paint.get(*i).and_then(Option::as_ref).is_some_and(|p| p.colors.iter().skip(1).any(|c| c.color.as_deref() == Some(k.name.as_str()))))
                .map(|(_, p)| p.object.clone())
                .collect();
            let mut used: Vec<String> = Vec::new();
            for (_, key) in materials.iter().filter(|(n, _)| *n == k.name) {
                if !used.contains(key) {
                    used.push(key.clone());
                }
            }
            PaletteColor { name: k.name.clone(), rgb: k.rgb.or(k.design).unwrap_or(k.fallback), implicit: k.implicit, parts, paint, materials: used }
        })
        .collect()
}

/// A name Fab gives a new colour (`Colour 3`): one to rename for what it colours.
pub fn default_color_name(name: &str) -> bool {
    name.strip_prefix("Colour ").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// How a tool body takes part in its boolean.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolRole {
    Subtract,
    Intersect,
}

/// The solids a parameter shapes that are tool bodies (cut away or intersected by a
/// boolean), with their role: what a ghost shows while the parameter changes. Names are
/// the script's own (the instance's solids).
pub fn tool_objects(c: &Compiled, p: &Param) -> Vec<(String, ToolRole)> {
    let Some(inst) = c.instance(&p.instance) else { return Vec::new() };
    let prog = &inst.script.program;
    let mut out: Vec<(String, ToolRole)> = Vec::new();
    for o in p.objects.iter().filter_map(|o| split_id(o).map(|(_, s)| s.to_string())) {
        let o = &o;
        let Some(s) = prog.solid(o) else { continue };
        for n in &prog.graph.nodes {
            let Op::Boolean { mode, b, .. } = &n.op else { continue };
            let role = match mode {
                makepad_csg_graph::BoolMode::Difference => ToolRole::Subtract,
                makepad_csg_graph::BoolMode::Intersection => ToolRole::Intersect,
                _ => continue,
            };
            if b.contains(&s.node) && !out.iter().any(|(x, _)| x == o) {
                out.push((o.clone(), role));
            }
        }
        // A distance field carved by it (`m.carve(body, [cut1], 0mm)`): an operand after the first.
        let carved = s.parent.map(|i| &prog.solids[i]).is_some_and(|par| par.proto == "Carve" && par.children.first().is_some_and(|c| prog.solids[*c].id != s.id));
        if carved && !out.iter().any(|(x, _)| x == o) {
            out.push((o.clone(), ToolRole::Subtract));
        }
    }
    out
}

/// The frames of the parts a tool solid of `instance` belongs to, out of `frames` (by part).
pub fn tool_frames<'a>(model: &[PartParams], instance: &str, solid: &str, frames: &'a [(String, [[f64; 4]; 3])]) -> Vec<&'a [[f64; 4]; 3]> {
    let id = part_id(instance, solid);
    model
        .iter()
        .filter(|pp| pp.objects.contains(&id))
        .filter_map(|pp| frames.iter().find(|(object, _)| *object == pp.object).map(|(_, f)| f))
        .collect()
}

/// Whether a solid is, or is built from, a helical body (a thread, a helical or twisted
/// sweep): every turn folds, so its folds say nothing about its outline.
pub fn is_helical(c: &Compiled, instance: &str, solid: &str) -> bool {
    let Some(prog) = c.instance(instance).map(|i| &i.script.program) else { return false };
    let Some(s) = prog.solid(solid) else { return false };
    let mut stack = vec![s.node];
    let mut seen = HashSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let node = prog.graph.node(id);
        match &node.op {
            Op::Thread { .. } => return true,
            Op::Sweep { path: makepad_csg_graph::Path3::Helix { .. }, .. } => return true,
            Op::Sweep { twist, .. } if *twist != 0.0 => return true,
            _ => {}
        }
        stack.extend(node.op.inputs());
    }
    false
}

/// The instance's script with parameter `key` at `value`, compiled (nothing evaluated,
/// nothing committed): what a ghost builds while the parameter moves.
pub fn with_value(c: &Compiled, parts: &[PartParams], key: &str, value: &PValue) -> Result<Script, String> {
    let (inst, _) = split_id(key).and_then(|(i, site)| Some((c.instance(i)?, site))).ok_or_else(|| format!("no instance for '{key}'"))?;
    let s = &inst.script;
    match edit(c, parts, key, value)? {
        Change::Param { name, value, .. } => compile_pinned(s, s.source(), value.map(|v| (name.as_str(), v))),
        Change::Script { edit, .. } => {
            let source = makepad_model_script::lang::edit::apply(&s.program, &edit)?;
            compile_pinned(s, &source, None)
        }
        _ => Err("not a parameter".into()),
    }
}

