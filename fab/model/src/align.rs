//! Align a limb: which of an object's solids turn together, and where they turn about.
//!
//! A limb is a solid of the object and whatever hangs off it. Which is which comes from the
//! program's own solids: take the limb away and the rest falls into pieces that touch each other;
//! the biggest piece touching the limb is what it hangs from, and where they meet is its joint.
//! The others go with it. A limb the rest holds at both ends can't turn by itself, and a part with
//! no separate solids has no limb to turn: both say so, and the other fixes still apply.

use crate::paint::{sdf_of, within, Form};
use crate::program::Emitted;
use makepad_csg_graph::eval::transform_matrix;
use makepad_csg_math::Vec3d;
use makepad_csg_sdf::{distance, node_bounds, SdfNode};
use makepad_model_script::lang::Program;

/// Solids this close print as one (mm): under a line width.
pub const TOUCH: f64 = 0.5;
/// A limb's far end starts at this share of its length: contact out there holds that end too.
const FAR_END: f64 = 0.75;
/// How much a field's distance can run ahead of the true one: the search only skips what's
/// out of reach by this factor.
const SLOPE: f64 = 2.0;

type Frame = [[f64; 4]; 3];

/// A limb the program lets turn.
#[derive(Clone, Debug, PartialEq)]
pub struct Limb {
    /// The solids that turn: the limb's own, then what it carries.
    pub solids: Vec<String>,
    /// The carried ones alone.
    pub carried: Vec<String>,
    /// The biggest solid of what holds it: what a joint's said to be with.
    pub held_by: String,
    /// Where it meets the rest (model mm).
    pub pivot: [f64; 3],
    /// From the pivot out along the limb (unit).
    pub out: [f64; 3],
}

/// Solid `i`'s frame in the model: its ancestors' placements, its own left out.
pub(crate) fn parent_world(p: &Program, i: usize) -> Frame {
    let mut m: Frame = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
    let mut at = p.solids[i].parent;
    while let Some(j) = at {
        if let Some((a, r, s)) = crate::eval::own_placement(p, j) {
            m = crate::weave::compose(&transform_matrix(a, r, s), &m);
        }
        at = p.solids[j].parent;
    }
    m
}

struct Piece {
    solid: usize,
    field: SdfNode,
    /// From the model into the frame the field is in, and the size of a unit there in mm.
    from_model: Frame,
    unit: f64,
    lo: [f64; 3],
    hi: [f64; 3],
}

impl Piece {
    fn new(p: &Program, solid: usize) -> Result<Piece, String> {
        let field = sdf_of(&p.graph, p.solids[solid].node, Form::Exact).map_err(|why| format!("'{}': {why}", p.solids[solid].id))?;
        let (lo, hi) = node_bounds(&field).ok_or_else(|| format!("'{}' has no bounds", p.solids[solid].id))?;
        let world = parent_world(p, solid);
        let (mut min, mut max) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        for c in 0..8 {
            let q = [if c & 1 == 0 { lo[0] } else { hi[0] }, if c & 2 == 0 { lo[1] } else { hi[1] }, if c & 4 == 0 { lo[2] } else { hi[2] }];
            let m = crate::params::xf_point(&world, q);
            for k in 0..3 {
                min[k] = min[k].min(m[k]);
                max[k] = max[k].max(m[k]);
            }
        }
        let unit = (0..3).map(|c| (0..3).map(|r| world[r][c] * world[r][c]).sum::<f64>().sqrt()).fold(f64::INFINITY, f64::min);
        Ok(Piece { solid, field, from_model: crate::weave::invert(&world), unit, lo: min, hi: max })
    }

    fn distance(&self, q: [f64; 3]) -> f64 {
        let l = crate::params::xf_point(&self.from_model, q);
        distance(&self.field, Vec3d::new(l[0], l[1], l[2])) * self.unit
    }

    fn size(&self) -> f64 {
        (0..3).map(|k| (self.hi[k] - self.lo[k]).max(0.0)).product()
    }
}

/// Where two pieces come within [`TOUCH`], smallest boxes first found: `found` says to stop.
fn meets(a: &Piece, b: &Piece, found: &mut dyn FnMut([f64; 3]) -> bool) {
    let lo: [f64; 3] = std::array::from_fn(|k| a.lo[k].max(b.lo[k]) - TOUCH);
    let hi: [f64; 3] = std::array::from_fn(|k| a.hi[k].min(b.hi[k]) + TOUCH);
    if (0..3).any(|k| lo[k] > hi[k]) {
        return;
    }
    let mut stack = vec![(lo, hi)];
    while let Some((lo, hi)) = stack.pop() {
        let c: [f64; 3] = std::array::from_fn(|k| 0.5 * (lo[k] + hi[k]));
        let half = (0..3).map(|k| (hi[k] - lo[k]).powi(2)).sum::<f64>().sqrt() * 0.5;
        let (da, db) = (a.distance(c), b.distance(c));
        if da > SLOPE * half + TOUCH || db > SLOPE * half + TOUCH {
            continue;
        }
        if half <= TOUCH {
            if da <= TOUCH && db <= TOUCH && found(c) {
                return;
            }
            continue;
        }
        let long = (0..3).max_by(|x, y| (hi[*x] - lo[*x]).total_cmp(&(hi[*y] - lo[*y]))).unwrap_or(0);
        let (mut near, mut far) = (hi, lo);
        near[long] = c[long];
        far[long] = c[long];
        stack.push((far, hi));
        stack.push((lo, near));
    }
}

/// The solids of feature `feature` of `object` that turn, and where about, for a limb lying along
/// `a`..`b` in the model frame.
pub fn limb_of(p: &Program, object: &Emitted, feature: &str, a: [f64; 3], b: [f64; 3]) -> Result<Limb, String> {
    let i = p.solids.iter().position(|s| s.id == feature).ok_or_else(|| format!("no solid '{feature}'"))?;
    let leaves: Vec<usize> = (0..p.solids.len()).filter(|j| p.solids[*j].children.is_empty() && object.bodies.iter().any(|body| within(p, *j, *body))).collect();
    let mut own = Vec::new();
    let mut rest = Vec::new();
    for j in leaves {
        let piece = Piece::new(p, j).map_err(|why| format!("this part isn't made of separate shapes it can turn ({why}): solid infill or thicken still can"))?;
        if within(p, j, i) {
            own.push(piece);
        } else {
            rest.push(piece);
        }
    }
    if own.is_empty() {
        return Err(format!("'{feature}' isn't a shape of its own here: solid infill or thicken still can"));
    }
    if rest.is_empty() {
        return Err(format!("'{feature}' is the whole part: nothing to turn it against. Fab.orient can turn the part"));
    }
    // the rest, in pieces that touch
    let mut group: Vec<usize> = (0..rest.len()).collect();
    fn root(group: &mut [usize], x: usize) -> usize {
        let mut r = x;
        while group[r] != r {
            r = group[r];
        }
        group[x] = r;
        r
    }
    for x in 0..rest.len() {
        for y in x + 1..rest.len() {
            if root(&mut group, x) != root(&mut group, y) {
                let mut touching = false;
                meets(&rest[x], &rest[y], &mut |_| {
                    touching = true;
                    true
                });
                if touching {
                    let (rx, ry) = (root(&mut group, x), root(&mut group, y));
                    group[ry] = rx;
                }
            }
        }
    }
    // where the limb meets each: how far along it, and how big the group is
    let line: [f64; 3] = std::array::from_fn(|k| b[k] - a[k]);
    let span = (0..3).map(|k| line[k] * line[k]).sum::<f64>();
    if span <= 0.0 {
        return Err(format!("'{feature}' has no length to turn"));
    }
    let along = |q: [f64; 3]| (0..3).map(|k| (q[k] - a[k]) * line[k]).sum::<f64>() / span;
    let groups: Vec<usize> = (0..rest.len()).map(|x| root(&mut group, x)).collect();
    let mut touching: Vec<(usize, f64, Vec<f64>)> = Vec::new();
    for (x, piece) in rest.iter().enumerate() {
        let mut ts = Vec::new();
        for limb in &own {
            meets(limb, piece, &mut |q| {
                ts.push(along(q));
                false
            });
        }
        let at = match touching.iter().position(|t| t.0 == groups[x]) {
            Some(at) => at,
            None => {
                touching.push((groups[x], (0..rest.len()).filter(|y| groups[*y] == groups[x]).map(|y| rest[y].size()).sum(), Vec::new()));
                touching.len() - 1
            }
        };
        touching[at].2.extend(ts);
    }
    touching.retain(|t| !t.2.is_empty());
    let Some(held) = touching.iter().max_by(|x, y| x.1.total_cmp(&y.1)) else {
        return Err(format!("'{feature}' doesn't touch the rest of the part: it can't turn about a joint it doesn't have"));
    };
    let (held_group, ts) = (held.0, &held.2);
    if ts.iter().any(|t| *t <= 1.0 - FAR_END) && ts.iter().any(|t| *t >= FAR_END) {
        return Err(format!("'{feature}' is held at both ends: it can't turn alone. Solid infill or thicken still can"));
    }
    let mean = ts.iter().sum::<f64>() / ts.len() as f64;
    let (pivot_t, far_t) = if mean < 0.5 { (mean.max(0.0), 1.0) } else { (mean.min(1.0), 0.0) };
    let pivot: [f64; 3] = std::array::from_fn(|k| a[k] + line[k] * pivot_t);
    let far: [f64; 3] = std::array::from_fn(|k| a[k] + line[k] * far_t);
    let out: [f64; 3] = std::array::from_fn(|k| far[k] - pivot[k]);
    let length = (0..3).map(|k| out[k] * out[k]).sum::<f64>().sqrt();
    if length <= 0.0 {
        return Err(format!("'{feature}' has no length past its joint"));
    }
    let carried: Vec<String> = (0..rest.len()).filter(|x| groups[*x] != held_group && touching.iter().any(|t| t.0 == groups[*x])).map(|x| p.solids[rest[x].solid].id.clone()).collect();
    let solids = own.iter().map(|piece| p.solids[piece.solid].id.clone()).chain(carried.iter().cloned()).collect();
    let holder = (0..rest.len()).filter(|x| groups[*x] == held_group).max_by(|x, y| rest[*x].size().total_cmp(&rest[*y].size())).map_or(String::new(), |x| p.solids[rest[x].solid].id.clone());
    Ok(Limb { solids, carried, held_by: holder, pivot, out: out.map(|x| x / length) })
}
