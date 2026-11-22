//! A script's force study: where its part is held and loaded, as builder statements kept in
//! the object script beside the shape. A study is no solid and changes no shape: the part is
//! built and sliced the same with or without one; Design's Force render mode solves it, and an
//! edit of the shape or of a load solves it again.
//!
//! - `m.fixed(region)`: held in place (a clamped face). On a hole it holds what a bolt
//!   through it clamps: the face under the head and the face the part is bolted against, out
//!   to twice the hole's radius, the hole's wall free; `m.fixed(holes[1], rigid: true)` holds
//!   the hole's wall itself.
//! - `m.force(region, [x, y, z])`: a total force in newtons (model frame), spread over the
//!   region by area.
//! - `m.pressure(region, p)`: a pressure in MPa pushing into the surface.
//! - `m.torque(region, t)`: a torque in N mm about the region's axis (a hole's), right-handed.
//!
//! A region is
//! - a feature of the object (`holes[1]`, or a list `[holes[1] holes[3]]`): the part's surface
//!   on the feature's own surface (within about a cell of it and facing along it), following
//!   parameter edits;
//! - the object itself: all of its surface;
//! - a region solid outside every object (`m.box({..})`): the part's surface inside it;
//! - `Face{at: [x y z]}`: the face round a point, as the Force tab's face pick grows it;
//! - `HoleWall{at: [x y z]}`: the hole's wall through a point, its cylinder fitted.
//!
//! Every region is found again on the part's current surface at each solve.

use makepad_csg_sdf::{SdfNode, SdfOp};
use makepad_model_script::lang::{Arg, Diag, Program, Root, V};
use fab_paint_field::Region;
use makepad_script::syntax::{Kind as Syn, Node, Syntax};
use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use crate::paint::FEATURE_BAND;
use crate::program::{Emitted, UNIT};

/// The builder calls of a study, in the order the Force tab lists their kinds.
pub const CALLS: [&str; 4] = ["fixed", "force", "pressure", "torque"];

/// What a fixture or load does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// Held in place.
    Fixed,
    /// A total force (N, model frame).
    Force([f64; 3]),
    /// A pressure (MPa) pushing into the surface.
    Pressure(f64),
    /// A torque (N mm) about the region's axis, right-handed.
    Torque(f64),
}

impl Kind {
    /// Its builder call (`fixed` for `m.fixed(..)`).
    pub fn call(&self) -> &'static str {
        match self {
            Kind::Fixed => "fixed",
            Kind::Force(_) => "force",
            Kind::Pressure(_) => "pressure",
            Kind::Torque(_) => "torque",
        }
    }
}

/// Where on the part's surface (model frame, mm).
#[derive(Clone, Debug)]
pub enum Where {
    /// All of it (the object's own solid).
    All,
    /// Its surface on a feature's own surface: the feature's field, grown by [`FEATURE_BAND`].
    Feature(Region),
    /// Its surface inside a region solid.
    Inside(Region),
    /// The face round a point.
    Face([f64; 3]),
    /// The hole's wall through a point.
    HoleWall([f64; 3]),
}

/// One fixture or load as written.
#[derive(Clone, Debug)]
pub struct Load {
    pub kind: Kind,
    pub at: Where,
    /// The region as written (`holes[1]`, `Face{at: [0 6mm 10mm]}`).
    pub on: String,
    /// The solids it names (`holes[1]`): what an optimiser keeps clear.
    pub names: Vec<String>,
    pub loc: Option<(u32, u32)>,
    /// Its call's bytes in the script.
    pub span: Option<Range<usize>>,
    /// The call is a statement of its own at the script's top (the Force tab edits and
    /// removes only those; one in a loop or a function is edited in the code).
    pub top: bool,
    /// The bytes of its value as written (the force, the pressure, the torque).
    pub value_span: Option<Range<usize>>,
    /// A fixture on a hole holds the hole's wall itself, not a bolt's clamp (`rigid: true`).
    pub rigid: bool,
}

/// An emitted object's fixtures and loads, in the order they are written.
#[derive(Clone, Debug, Default)]
pub struct Study {
    pub loads: Vec<Load>,
}

impl Study {
    /// Something holds the part and something loads it.
    pub fn posed(&self) -> bool {
        self.loads.iter().any(|l| l.kind == Kind::Fixed) && self.loads.iter().any(|l| l.kind != Kind::Fixed)
    }
}

/// The study statements of a compiled program, per emitted object name (an object with none
/// has no entry). A statement on a feature belongs to the feature's object, any other to
/// every object of the script.
pub fn compile(p: &Program, objects: &[Emitted]) -> Result<HashMap<String, Arc<Study>>, Vec<Diag>> {
    let mut loads: Vec<(Load, Option<usize>)> = Vec::new();
    let mut errors = Vec::new();
    for root in p.roots.iter().filter(|r| CALLS.contains(&r.kind)) {
        match load(p, objects, root) {
            Ok(l) => loads.push(l),
            Err(message) => {
                let (line, col) = root.loc.unwrap_or((1, 1));
                errors.push(Diag { line, col, message: format!("m.{}: {message}", root.kind) });
            }
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(objects
        .iter()
        .enumerate()
        .filter_map(|(oi, o)| {
            let own: Vec<Load> = loads.iter().filter(|(_, only)| only.is_none_or(|k| k == oi)).map(|(l, _)| l.clone()).collect();
            (!own.is_empty()).then(|| (o.name.clone(), Arc::new(Study { loads: own })))
        })
        .collect())
}

fn p3(v: &V) -> Option<[f64; 3]> {
    match v {
        V::Arr(a) if a.len() == 3 => Some([a[0].num()? * UNIT, a[1].num()? * UNIT, a[2].num()? * UNIT]).filter(|p| p.iter().all(|x| x.is_finite())),
        _ => None,
    }
}

/// The value argument's name per call (also its second position).
fn value_key(kind: &str) -> Option<&'static str> {
    match kind {
        "force" => Some("newtons"),
        "pressure" => Some("mpa"),
        "torque" => Some("nmm"),
        _ => None,
    }
}

fn load(p: &Program, objects: &[Emitted], root: &Root) -> Result<(Load, Option<usize>), String> {
    let number = |key: &str| match root.get(key) {
        Some(Arg::Value(v)) => v.num().filter(|x| x.is_finite()),
        _ => None,
    };
    let kind = match root.kind {
        "fixed" => Kind::Fixed,
        "force" => match root.get("newtons") {
            Some(Arg::Value(V::Arr(a))) if a.len() == 3 && a.iter().all(|v| v.num().is_some_and(f64::is_finite)) => Kind::Force([a[0].num().unwrap(), a[1].num().unwrap(), a[2].num().unwrap()]),
            _ => return Err("give it the force in newtons, model frame: m.force(region, [0, -100, 0]) (write a negative part after a comma: a space before the minus subtracts)".into()),
        },
        "pressure" => Kind::Pressure(number("mpa").ok_or("give it the pressure in MPa: m.pressure(region, 0.2)")?),
        "torque" => Kind::Torque(number("nmm").ok_or("give it the torque in N mm: m.torque(region, 500)")?),
        _ => return Err("not a study statement".into()),
    };
    let rigid = match root.get("rigid") {
        None => false,
        Some(Arg::Value(V::Bool(b))) => *b,
        Some(_) => return Err("rigid is true or false: m.fixed(holes[1], rigid: true) holds the hole's wall itself, not a bolt's clamp".into()),
    };
    let (at, names, owner) = place(p, objects, root)?;
    let source = p.source();
    let call = root.loc.and_then(|loc| p.names.offset(loc)).and_then(|at| call_at(&p.names.syntax, at));
    let args = call.and_then(|c| match &c.kind {
        Syn::Call { args, .. } => Some(args.as_slice()),
        _ => None,
    });
    // an argument's value node, by its name or its position
    let arg = |key: &str, k: usize| -> Option<&Node> {
        let args = args?;
        let named = args.iter().find_map(|a| match &a.kind {
            Syn::Entry { name, value, .. } if name == key => Some(&**value),
            _ => None,
        });
        named.or_else(|| args.get(k).filter(|a| !matches!(a.kind, Syn::Entry { .. })))
    };
    let on = arg("region", 0).map_or_else(String::new, |n| source[n.span.clone()].to_string());
    let value_span = value_key(root.kind).and_then(|key| arg(key, 1)).map(|n| n.span.clone());
    let span = call.map(|c| c.span.clone());
    let top = span.as_ref().is_some_and(|s| p.names.syntax.statements().iter().any(|st| st.span == *s));
    Ok((Load { kind, at, on, names, loc: root.loc, span, top, value_span, rigid }, owner))
}

/// The call written at byte `at` (where the VM reports it: its start or its `(`).
fn call_at(syntax: &Syntax, at: usize) -> Option<&Node> {
    let mut out = None;
    syntax.walk(&mut |_, n| {
        if out.is_none() {
            if let Syn::Call { open, .. } = &n.kind {
                if n.span.start == at || *open == at {
                    out = Some(n);
                }
            }
        }
    });
    out
}

const WHERE: &str = "a feature of the object (holes[1]), the object itself, a region (m.box({..})), Face{at: [x y z]} or HoleWall{at: [x y z]}";

/// Where a statement holds or loads the part, the solids it names, and for a feature the
/// object it belongs to.
fn place(p: &Program, objects: &[Emitted], root: &Root) -> Result<(Where, Vec<String>, Option<usize>), String> {
    let id = |i: usize| p.solids[i].id.clone();
    match root.get("region") {
        Some(Arg::Solid(i)) => one(p, objects, *i).map(|(w, o)| (w, vec![id(*i)], o)),
        Some(Arg::Solids(ids)) => {
            let (mut features, mut regions, mut owner) = (Vec::new(), Vec::new(), None);
            for &i in ids {
                match one(p, objects, i)? {
                    (Where::Feature(Region::Field { node, .. }), o) => {
                        features.push(node);
                        owner = owner.or(o);
                    }
                    (Where::Inside(Region::Field { node, .. }), _) => regions.push(node),
                    _ => return Err("a list holds features of the object or regions; hold or load the whole object by its name".into()),
                }
            }
            let union = |of: Vec<SdfNode>| SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of };
            let at = match (features.is_empty(), regions.is_empty()) {
                (false, true) => Where::Feature(Region::Field { node: union(features), grow: FEATURE_BAND }),
                (true, false) => Where::Inside(Region::Field { node: union(regions), grow: 0.0 }),
                _ => return Err("a list is all features of the object or all regions, not both".into()),
            };
            Ok((at, ids.iter().map(|&i| id(i)).collect(), owner))
        }
        Some(Arg::Value(V::Obj(o))) if matches!(o.proto.as_deref(), Some("Face" | "HoleWall")) => {
            let face = o.proto.as_deref() == Some("Face");
            let at = o.get("at").and_then(p3).ok_or_else(|| format!("a {}'s at is a point on the part's surface [x y z] with units", if face { "Face" } else { "HoleWall" }))?;
            Ok((if face { Where::Face(at) } else { Where::HoleWall(at) }, Vec::new(), None))
        }
        Some(Arg::Value(V::Obj(o))) if o.proto.as_deref() == Some("Flood") => Err("a Flood is a paint region; hold or load a face with Face{at: [x y z]}".into()),
        _ => Err(format!("give it where: {WHERE}")),
    }
}

/// One solid as a region: the object itself, a feature of it (with its object), or a region
/// solid outside every object.
fn one(p: &Program, objects: &[Emitted], i: usize) -> Result<(Where, Option<usize>), String> {
    match crate::paint::solid_region(p, objects, i, crate::paint::Form::Meshed) {
        Ok((Region::All, o)) => Ok((Where::All, o)),
        Ok((r, Some(o))) => Ok((Where::Feature(r), Some(o))),
        Ok((r, None)) => Ok((Where::Inside(r), None)),
        Err(why) => {
            let s = &p.solids[i];
            Err(format!("'{}' has no spatial form to find on the surface: {why}; use a region, Face{{at: [x y z]}} or HoleWall{{at: [x y z]}}", if s.id.is_empty() { s.proto } else { &s.id }))
        }
    }
}

/// The feature of `object` whose own surface passes within `reach` of `at` (model mm), as a
/// script names it (`hole`, `holes[1]`): the smallest such, if any. What a click on a hole
/// writes, so the load follows the feature through parameter edits.
pub fn feature_at(p: &Program, objects: &[Emitted], object: &Emitted, at: [f64; 3], reach: f64) -> Option<String> {
    (0..p.solids.len())
        .filter(|&i| !object.bodies.contains(&i) && crate::volumes::writable(p, &p.solids[i].id))
        // a feature's own distance only: a click is no time to mesh every feature
        .filter_map(|i| match crate::paint::solid_region(p, objects, i, crate::paint::Form::Exact) {
            Ok((r, Some(o))) if objects[o].name == object.name => {
                let Region::Field { node, grow } = &r else { return None };
                let d = r.distance(at) + grow;
                let (lo, hi) = makepad_csg_sdf::node_bounds(node)?;
                (d.abs() <= reach).then(|| ((0..3).map(|k| (hi[k] - lo[k]).max(0.0)).product::<f64>(), i))
            }
            _ => None,
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, i)| p.solids[i].id.clone())
}
