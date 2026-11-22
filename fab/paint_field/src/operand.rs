//! A feature of a part's field as a paint region: the surface it makes of the part's, and round
//! that surface the space where it is the nearest.
//!
//! A fill of a feature (an operand of the part's field: a blended limb, a carved pupil) paints
//! what its own surface makes: in a blend's fillet the side nearer it, so the fillet parts where
//! its operands are equally near; a carve's cutter the faces it cuts; the base of a carve all it
//! is. Off the surface the region goes on as it does on it, so a lattice point in the air beside
//! the surface reads what the surface there shows, never the part's unpainted colour (a cell of
//! three slots: specks and spikes along the edge).
//!
//! Its value is negative inside and a lower bound of the distance to its boundary: each step on
//! the way from the part's root to the feature has a term over the slopes of what it takes, and
//! the largest decides (the feature must make the surface at every step).
//!
//! Every feature of a part comes from one [`Body`] of its field: each combine on the way to a
//! feature has its operands placed in the part's frame once, shared by every feature under it,
//! and nothing is read until a point is. An operand reads through chunks of the part's box, each
//! its tree reduced to what can change it there ([`Chunks`]). A read takes each operand's bounds
//! where it reads (its chunk's for one point, the box of a batch: a brick's) and leaves out what
//! cannot decide there, exactly: an operand of a union above the nearest one, a cutter or a base
//! under the face that makes the surface, a step under another's term, a feature above another
//! of the same region. A feature far from a costly leaf (a mesh) never reads it.

use crate::program::P3;
use makepad_csg_math::Vec3d;
use makepad_csg_sdf::{cut, distance_many, node_bounds, same, Bounds, Chunks, Cut, Sdf3, SdfKernel, SdfNode, SdfOp, SdfPlace, SdfShape};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// How far past a cutter's surface its region reaches into what it cuts and what it leaves
/// (mm): past a lattice cell's diagonal and the bake's ramp at the coarsest pitch, so every
/// lattice point round a cut face reads it.
pub const PAD: f64 = 1.7;

/// An operand's chunks (mm a side): two of the program's grid cells, so a chunk away from a
/// costly leaf leaves it out.
const CHUNK: f64 = 2.0 * crate::program::CELL;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    /// The feature is one operand of a union: it holds where it is nearer than the others.
    Union,
    /// The feature cuts the first operand (an operand after the first of a difference): it holds
    /// where its own surface is the part's, within [`PAD`] of that surface.
    Cut,
    /// The part is made from the feature by an op with no distance of its own (an extrusion, a
    /// fillet of an exact body): the feature holds its own surface's band, `grow` out.
    Band { grow: f64 },
}

/// The kernels' Lipschitz bounds by key, each measured over its own box the first time an
/// operand holding it is read.
#[derive(Default)]
struct Slopes(Mutex<HashMap<u64, f64>>);

impl Slopes {
    fn of(&self, k: &SdfKernel) -> f64 {
        if let Some(s) = self.0.lock().unwrap().get(&k.key) {
            return *s;
        }
        let s = match k.bounds {
            Some((min, max)) => SdfShape { root: SdfNode::Kernel(k.clone()), min, max, materials: Vec::new() }.lipschitz(),
            None => 1.0,
        };
        self.0.lock().unwrap().insert(k.key, s);
        s
    }
}

/// The kernels in `n`.
fn kernels<'a>(n: &'a SdfNode, out: &mut Vec<&'a SdfKernel>) {
    match n {
        SdfNode::Kernel(k) => out.push(k),
        SdfNode::Combine { of, .. } => of.iter().for_each(|c| kernels(c, out)),
        SdfNode::Material { within, of, .. } => {
            if let Some(w) = within {
                kernels(w, out);
            }
            kernels(of, out);
        }
        SdfNode::Shell { of, .. } | SdfNode::Round { of, .. } | SdfNode::Transform { of, .. } | SdfNode::Instance { of, .. } | SdfNode::Bone { of, .. } | SdfNode::Deform { of, .. } | SdfNode::Id { of, .. } | SdfNode::Sculpt { of, .. } | SdfNode::Texture { of, .. } => kernels(of, out),
        _ => {}
    }
}

/// One operand of a combine on the way to features, placed in the part's frame: shared by
/// every feature under the combine.
pub struct Of {
    node: Arc<SdfNode>,
    bounds: Option<Bounds>,
    /// The box its chunks cover: where its features read it.
    region: Bounds,
    slopes: Arc<Slopes>,
    slope: OnceLock<f64>,
    chunks: OnceLock<Chunks>,
}

impl std::fmt::Debug for Of {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Of").field("bounds", &self.bounds).finish_non_exhaustive()
    }
}

/// An operand over a batch: what its value can be there, and what reads it.
struct Read<'a> {
    lo: f64,
    hi: f64,
    how: How<'a>,
}

enum How<'a> {
    /// The operand cut to the batch's box: its value there, bit for bit.
    Node(SdfNode),
    /// Its chunks (one point: its own chunk's tree).
    Chunks(&'a Chunks),
}

impl Read<'_> {
    fn values(&self, at: &[Vec3d]) -> Vec<f64> {
        let mut out = vec![0.0; at.len()];
        match &self.how {
            How::Node(n) => distance_many(n, at, &mut out),
            How::Chunks(c) => c.distance_many(at, &mut out),
        }
        out
    }
}

impl Of {
    fn new(node: SdfNode, region: Bounds, slopes: Arc<Slopes>) -> Of {
        Of { bounds: node_bounds(&node), node: Arc::new(node), region, slopes, slope: OnceLock::new(), chunks: OnceLock::new() }
    }

    /// Its Lipschitz bound: 1 for the engine's distances, its steepest kernel's with one in it.
    fn slope(&self) -> f64 {
        *self.slope.get_or_init(|| {
            let mut ks = Vec::new();
            kernels(&self.node, &mut ks);
            ks.iter().map(|k| self.slopes.of(k)).fold(1.0, f64::max)
        })
    }

    fn chunks(&self) -> &Chunks {
        self.chunks.get_or_init(|| Chunks::new(self.node.clone(), self.region, CHUNK))
    }

    /// It over box `b` (one point's own: its chunk's tree and bounds).
    fn read(&self, b: &Bounds) -> Read<'_> {
        let chunks = self.chunks();
        if b.0 == b.1 {
            let (lo, hi) = chunks.index(b.0).map_or((f64::NEG_INFINITY, f64::INFINITY), |i| {
                let c = chunks.chunk(i);
                (c.lo, c.hi)
            });
            return Read { lo, hi, how: How::Chunks(chunks) };
        }
        let c = chunks.cut(b);
        Read { lo: c.lo, hi: c.hi, how: How::Node(c.node) }
    }
}

#[derive(Clone, Debug)]
struct Step {
    kind: Kind,
    /// The combine's operands in the part's frame (the ones that cannot come near the feature
    /// left out).
    of: Vec<Arc<Of>>,
    /// Which of them leads to the feature.
    at: usize,
}

/// A step over a batch: its operands' reads (none: it cannot decide the term there) and the
/// term's bounds.
struct StepRead<'a> {
    step: &'a Step,
    reads: Vec<Option<Read<'a>>>,
    /// The slope the term is over, and the feature's own.
    over: f64,
    own: f64,
    lo: f64,
    hi: f64,
}

/// The least and the most `|x|` over `lo..=hi`.
fn abs_range(lo: f64, hi: f64) -> (f64, f64) {
    let least = if lo <= 0.0 && hi >= 0.0 { 0.0 } else { lo.abs().min(hi.abs()) };
    (least, lo.abs().max(hi.abs()))
}

/// Bounds whose arithmetic met an infinity on both ends: none.
fn sure(lo: f64, hi: f64) -> (f64, f64) {
    (if lo.is_nan() { f64::NEG_INFINITY } else { lo }, if hi.is_nan() { f64::INFINITY } else { hi })
}

impl Step {
    /// The slope its term is over, and the feature's own.
    fn slopes(&self) -> (f64, f64) {
        let own = self.of[self.at].slope();
        let others = self.of.iter().enumerate().filter(|(j, _)| *j != self.at).map(|(_, o)| o.slope()).fold(0.0, f64::max);
        (own + others, own)
    }

    /// Its term's bounds over a box, from its operands' bounds there (`b`: one each) and its
    /// slopes ([`Step::slopes`]).
    fn term_bounds(&self, b: &[(f64, f64)], (over, own): (f64, f64)) -> (f64, f64) {
        let (flo, fhi) = b[self.at];
        let others = || (0..b.len()).filter(|&j| j != self.at);
        let (lo, hi) = match self.kind {
            // the nearest other's
            Kind::Union => {
                let most = others().map(|j| b[j].1).fold(f64::INFINITY, f64::min);
                let least = others().map(|j| b[j].0).fold(f64::INFINITY, f64::min);
                ((flo - most) / over, (fhi - least) / over)
            }
            // the surface as the base and the other cutters make it: the largest of the base and
            // the cutters negated
            Kind::Cut => {
                let span = |j: usize| if j == 0 { b[j] } else { (-b[j].1, -b[j].0) };
                let least = others().map(|j| span(j).0).fold(f64::NEG_INFINITY, f64::max);
                let most = others().map(|j| span(j).1).fold(f64::NEG_INFINITY, f64::max);
                let (a, z) = abs_range(flo, fhi);
                (((least + flo) / over).max(a / own - PAD), ((most + fhi) / over).max(z / own - PAD))
            }
            Kind::Band { grow } => (flo / own - grow, fhi / own - grow),
        };
        sure(lo, hi)
    }

    fn read<'a>(&'a self, b: &Bounds) -> StepRead<'a> {
        let (over, own) = self.slopes();
        let mut reads: Vec<Option<Read>> = self.of.iter().map(|o| Some(o.read(b))).collect();
        let bounds: Vec<(f64, f64)> = reads.iter().flatten().map(|r| (r.lo, r.hi)).collect();
        let (lo, hi) = self.term_bounds(&bounds, (over, own));
        // what can never decide the term is not read: a union's other operand whose least is
        // over another's most is never the nearest; a cut's operand whose most is under
        // another's least never makes the surface
        let others = || (0..bounds.len()).filter(|&j| j != self.at);
        match self.kind {
            Kind::Union => {
                let most = others().map(|j| bounds[j].1).fold(f64::INFINITY, f64::min);
                for j in others().filter(|&j| bounds[j].0 > most) {
                    reads[j] = None;
                }
            }
            Kind::Cut => {
                let span = |j: usize| if j == 0 { bounds[j] } else { (-bounds[j].1, -bounds[j].0) };
                let least = others().map(|j| span(j).0).fold(f64::NEG_INFINITY, f64::max);
                for j in others().filter(|&j| span(j).1 < least) {
                    reads[j] = None;
                }
            }
            Kind::Band { .. } => {}
        }
        StepRead { step: self, reads, over, own, lo, hi }
    }
}

impl StepRead<'_> {
    /// The term at each point of `at`, max'd into `c`.
    fn terms(&self, at: &[Vec3d], c: &mut [f64]) {
        let d: Vec<Option<Vec<f64>>> = self.reads.iter().map(|r| r.as_ref().map(|r| r.values(at))).collect();
        let fa = self.step.at;
        let own = d[fa].as_ref().expect("the feature's own values");
        for (k, c) in c.iter_mut().enumerate() {
            let term = match self.step.kind {
                Kind::Union => {
                    let other = (0..d.len()).filter(|&j| j != fa).filter_map(|j| d[j].as_ref().map(|v| v[k])).fold(f64::INFINITY, f64::min);
                    (own[k] - other) / self.over
                }
                Kind::Cut => {
                    // the part's surface as the base and the other cutters make it
                    let surface = (0..d.len()).filter(|&j| j != fa).filter_map(|j| d[j].as_ref().map(|v| if j == 0 { v[k] } else { -v[k] })).fold(f64::NEG_INFINITY, f64::max);
                    ((surface + own[k]) / self.over).max(own[k].abs() / self.own - PAD)
                }
                Kind::Band { grow } => own[k] / self.own - grow,
            };
            *c = c.max(term);
        }
    }
}

/// A feature's region over a batch: the steps that can decide its value there, and its bounds.
pub(crate) struct Over<'a> {
    steps: Vec<StepRead<'a>>,
    pub lo: f64,
    pub hi: f64,
}

impl Over<'_> {
    /// The value at each point of the batch.
    pub fn values(&self, at: &[Vec3d]) -> Vec<f64> {
        if self.steps.is_empty() {
            return vec![self.lo; at.len()];
        }
        let mut c = vec![f64::NEG_INFINITY; at.len()];
        for s in &self.steps {
            s.terms(at, &mut c);
        }
        c
    }
}

/// One feature's region in its part's frame.
#[derive(Clone, Debug)]
pub struct Operand {
    /// The feature's own field, in the part's frame.
    pub own: SdfNode,
    steps: Vec<Step>,
    /// Where it can hold: its own box grown by [`PAD`] and the blends on its way (`None`:
    /// anywhere).
    pub bounds: Option<(P3, P3)>,
}

/// A frame on the way down: what a node under it is placed by.
#[derive(Clone)]
enum Frame {
    Transform { at: P3, rotate: P3, scale: f64 },
    Deform(makepad_csg_sdf::SdfDeform),
    Instance(Vec<SdfPlace>),
}

/// `n` placed by `frames`, outermost first.
fn placed(frames: &[Frame], n: &SdfNode) -> SdfNode {
    frames.iter().rev().fold(n.clone(), |n, f| match f {
        Frame::Transform { at, rotate, scale } => SdfNode::Transform { at: *at, rotate: *rotate, scale: *scale, of: Box::new(n) },
        Frame::Deform(d) => SdfNode::deform(*d, n),
        Frame::Instance(places) => SdfNode::Instance { places: places.clone(), of: Box::new(n) },
    })
}

/// How far a smooth union of `n` operands blending over `blend` reaches past them.
fn blend_reach(blend: f64, n: usize) -> f64 {
    blend * 0.25 * (n.saturating_sub(1)) as f64
}

/// The box of the points `at`.
fn box_of(at: &[Vec3d]) -> Bounds {
    at.iter().fold(([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]), |(lo, hi), p| ([lo[0].min(p.x), lo[1].min(p.y), lo[2].min(p.z)], [hi[0].max(p.x), hi[1].max(p.y), hi[2].max(p.z)]))
}

impl Operand {
    /// A feature whose part is made from it by an op with no distance of its own: its own
    /// surface's band, `grow` out of it.
    pub fn band(own: SdfNode, grow: f64) -> Operand {
        let bounds = node_bounds(&own).map(|(lo, hi)| (lo.map(|v| v - grow), hi.map(|v| v + grow)));
        let region = bounds.map_or(([f64::NEG_INFINITY; 3], [f64::INFINITY; 3]), |(lo, hi)| (lo.map(|v| v - crate::program::CELL), hi.map(|v| v + crate::program::CELL)));
        let of = Arc::new(Of::new(own.clone(), region, Arc::new(Slopes::default())));
        Operand { steps: vec![Step { kind: Kind::Band { grow }, of: vec![of], at: 0 }], own, bounds }
    }

    /// The region over box `b`: the steps that can decide its value there read.
    pub(crate) fn over(&self, b: &Bounds) -> Over<'_> {
        if self.steps.is_empty() {
            return Over { steps: Vec::new(), lo: -1e9, hi: -1e9 };
        }
        let mut steps: Vec<StepRead> = self.steps.iter().map(|s| s.read(b)).collect();
        // the value is the steps' largest term: a step whose most is under another's least
        // never decides it
        let least = steps.iter().map(|s| s.lo).fold(f64::NEG_INFINITY, f64::max);
        steps.retain(|s| s.hi >= least);
        let hi = steps.iter().map(|s| s.hi).fold(f64::NEG_INFINITY, f64::max);
        Over { steps, lo: least, hi }
    }

    /// The region's value at each point: negative inside, a lower bound of the distance to its
    /// boundary.
    pub fn values(&self, ps: &[P3]) -> Vec<f64> {
        if ps.is_empty() {
            return Vec::new();
        }
        let at: Vec<Vec3d> = ps.iter().map(|p| Vec3d::new(p[0], p[1], p[2])).collect();
        self.over(&box_of(&at)).values(&at)
    }

    /// The region's value at one point.
    pub fn value(&self, p: P3) -> f64 {
        self.values(&[p])[0]
    }

    /// Whether it is made as `old` is: the same combines on its way, each with as many operands
    /// and each operand as steep, so that where their operands read alike so do the two.
    pub fn alike_made(&self, old: &Operand) -> bool {
        self.steps.len() == old.steps.len()
            && self.steps.iter().zip(&old.steps).all(|(s, o)| s.kind == o.kind && s.at == o.at && s.of.len() == o.of.len() && s.of.iter().zip(&o.of).all(|(a, b)| a.slope().to_bits() == b.slope().to_bits()))
    }

    /// Where it can read differently from `old` (the same feature an edit apart) in `within`, the
    /// part's frame, at a point where a value at least `cap` from 0 counts only by its side (a
    /// sample's edge distance goes no further): boxes about `leaf` a side, given to `found`, and
    /// none where it holds them already. Past them, at every point, the two values are the same
    /// bit for bit, or both at least `cap` on one side. False, and nothing found, when the two
    /// are not made alike (other combines on the way, another slope), so they may differ
    /// anywhere it holds.
    pub fn changed(&self, old: &Operand, within: &Bounds, leaf: f64, cap: f64, found: &mut dyn Changes) -> bool {
        if !self.alike_made(old) {
            return false;
        }
        if self.steps.iter().zip(&old.steps).all(|(s, o)| s.of.iter().zip(&o.of).all(|(a, b)| Arc::ptr_eq(a, b) || same(&a.node, &b.node))) {
            return true;
        }
        let cuts = |steps: &[Vec<Cut>], b: &Bounds| steps.iter().map(|s| s.iter().map(|c| cut(&c.node, b)).collect()).collect::<Vec<Vec<Cut>>>();
        let whole = |op: &Operand| op.steps.iter().map(|s| s.of.iter().map(|o| cut(&o.node, within)).collect()).collect::<Vec<Vec<Cut>>>();
        // each step's term's bounds over a box, and the value's: the largest term's
        let terms = |op: &Operand, cs: &[Vec<Cut>]| op.steps.iter().zip(cs).map(|(s, c)| s.term_bounds(&c.iter().map(|c| (c.lo, c.hi)).collect::<Vec<_>>(), s.slopes())).collect::<Vec<_>>();
        let value = |t: &[(f64, f64)]| t.iter().fold((f64::NEG_INFINITY, f64::NEG_INFINITY), |a, t| (a.0.max(t.0), a.1.max(t.1)));
        // a step whose most is under another's least never decides the value
        let decides = |t: &[(f64, f64)], i: usize| t[i].1 >= value(t).0;
        // a union's other operand never the nearest (its least over another's most)
        let never = |s: &Step, cs: &[Cut], j: usize| matches!(s.kind, Kind::Union) && j != s.at && cs[j].lo > (0..cs.len()).filter(|&i| i != s.at).map(|i| cs[i].hi).fold(f64::INFINITY, f64::min);
        let apart = |(now, was): &(Vec<Vec<Cut>>, Vec<Vec<Cut>>)| {
            let (tn, tw) = (terms(self, now), terms(old, was));
            let (x, y) = (side(value(&tn), cap), side(value(&tw), cap));
            // both on one side past the cap; or every step that can decide the value in either
            // the same, its operands cut alike or never the nearest in either
            !(x.is_some() && x == y)
                && !self.steps.iter().enumerate().all(|(i, s)| !(decides(&tn, i) || decides(&tw, i)) || (0..s.of.len()).all(|j| same(&now[i][j].node, &was[i][j].node) || (never(s, &now[i], j) && never(s, &was[i], j))))
        };
        apart_boxes(*within, &(whole(self), whole(old)), leaf, &|(n, w), b| (cuts(n, b), cuts(w, b)), &apart, found);
        true
    }

    /// Where this feature's region can read otherwise than `old`'s (the same feature after an
    /// edit of its part's field): each operand on its way that changed, its change
    /// ([`makepad_csg_sdf::changed`]) grown by as much as the feature's box is grown past its own
    /// field (the pad and the blends on its way), as its value is read off its surface; a step
    /// that takes other operands, the feature's whole box then and now. Empty when it reads alike
    /// everywhere; `None` when a change is unbounded.
    pub fn changed_from(&self, old: &Operand) -> Option<Vec<Bounds>> {
        let whole = || -> Option<Vec<Bounds>> {
            let (a, b) = (self.bounds?, old.bounds?);
            Some(vec![(std::array::from_fn(|k| a.0[k].min(b.0[k])), std::array::from_fn(|k| a.1[k].max(b.1[k])))])
        };
        if self.steps.len() != old.steps.len() {
            return whole();
        }
        let grown = |o: &Operand| match (node_bounds(&o.own), o.bounds) {
            (Some(own), Some(b)) => (0..3).map(|k| (own.0[k] - b.0[k]).max(b.1[k] - own.1[k])).fold(PAD, f64::max),
            _ => PAD,
        };
        let reach = grown(self).max(grown(old));
        let mut out = Vec::new();
        for (a, b) in self.steps.iter().zip(&old.steps) {
            if a.kind != b.kind || a.at != b.at || a.of.len() != b.of.len() {
                return whole();
            }
            for (x, y) in a.of.iter().zip(&b.of) {
                if Arc::ptr_eq(x, y) {
                    continue;
                }
                if let Some((lo, hi)) = makepad_csg_sdf::changed(&y.node, &x.node) {
                    if lo.iter().chain(&hi).any(|v| !v.is_finite()) {
                        return None;
                    }
                    out.push((lo.map(|v| v - reach), hi.map(|v| v + reach)));
                }
            }
        }
        Some(out)
    }

    /// Whether its value is steeper than a distance's nowhere: the steps' terms all are, but
    /// over a slope that may be far under 1 (a fillet's operands meeting at an angle), so the
    /// distance to its edge is read by its local slope.
    pub fn sloped(&self) -> bool {
        self.steps.iter().any(|s| !matches!(s.kind, Kind::Band { .. }))
    }
}

/// Which side of 0 `(lo, hi)` lies on, at least `cap` from it: `None` when it may come nearer.
pub(crate) fn side((lo, hi): (f64, f64), cap: f64) -> Option<bool> {
    if lo >= cap {
        Some(true)
    } else if hi <= -cap {
        Some(false)
    } else {
        None
    }
}

/// What a search for where two versions of a paint read apart has found: boxes in the part's
/// frame. A box it holds already needs no search.
pub trait Changes {
    /// Whether finding box `b` would add nothing to what it holds.
    fn holds(&self, b: &Bounds) -> bool;
    /// Box `b` may read apart.
    fn add(&mut self, b: Bounds);
}

/// Every box found, none held before.
impl Changes for Vec<Bounds> {
    fn holds(&self, _: &Bounds) -> bool {
        false
    }
    fn add(&mut self, b: Bounds) {
        self.push(b);
    }
}

/// The boxes of `b`, halved along their longest side down to `leaf` a side, in which two
/// versions of a region may read apart, given to `found` (one it holds already is not searched):
/// `apart` tells it from their trees cut to a box (`at`, cut to `b`; `cut_to` cuts them to a
/// box within).
pub(crate) fn apart_boxes<T>(b: Bounds, at: &T, leaf: f64, cut_to: &dyn Fn(&T, &Bounds) -> T, apart: &dyn Fn(&T) -> bool, found: &mut dyn Changes) {
    if found.holds(&b) || !apart(at) {
        return;
    }
    let k = (0..3).max_by(|&i, &j| (b.1[i] - b.0[i]).total_cmp(&(b.1[j] - b.0[j]))).expect("three axes");
    if !(b.1[k] - b.0[k] > leaf) {
        found.add(b);
        return;
    }
    let mid = 0.5 * (b.0[k] + b.1[k]);
    for half in [(b.0, std::array::from_fn(|i| if i == k { mid } else { b.1[i] })), (std::array::from_fn(|i| if i == k { mid } else { b.0[i] }), b.1)] {
        apart_boxes(half, &cut_to(at, &half), leaf, cut_to, apart, found);
    }
}

/// Several features' region at the points `ps`: inside any of them. A feature whose least over
/// their box is over another's most is never the nearest there, and is not read. Where a value
/// past `enough` off the edge only counts as that far (a bound of it past it standing for it),
/// one whose least is past it is not read either, and none is when one holds them all deeper.
pub(crate) fn union_values(of: &[Operand], ps: &[P3], enough: f64) -> Vec<f64> {
    if ps.is_empty() {
        return Vec::new();
    }
    let at: Vec<Vec3d> = ps.iter().map(|p| Vec3d::new(p[0], p[1], p[2])).collect();
    // one point read only as far as `enough`: its bounds over the box that far round it
    let b = match (ps, enough.is_finite()) {
        ([p], true) => (p.map(|v| v - enough), p.map(|v| v + enough)),
        _ => box_of(&at),
    };
    let overs: Vec<Over> = of.iter().map(|o| o.over(&b)).collect();
    let most = overs.iter().map(|o| o.hi).fold(f64::INFINITY, f64::min);
    if most < -enough {
        return vec![most; ps.len()];
    }
    let mut v = vec![f64::INFINITY; ps.len()];
    for o in overs.iter().filter(|o| o.lo <= most) {
        if o.lo > enough {
            v.iter_mut().for_each(|v| *v = v.min(o.lo));
            continue;
        }
        for (v, c) in v.iter_mut().zip(o.values(&at)) {
            *v = v.min(c);
        }
    }
    v
}

/// A combine on the way to features that they must make the surface at.
struct Along {
    blend: f64,
    kind: Kind,
    /// Its operands placed in the root's frame, shared by the features under it.
    of: Arc<Vec<Arc<Of>>>,
    /// Which of them leads to the feature.
    at: usize,
}

/// A tagged feature: its own field placed in the root's frame, and the combines on its way.
struct Found {
    own: SdfNode,
    path: Vec<Along>,
}

/// A part's field and the regions of the features tagged in it ([`SdfNode::Id`]), built in one
/// walk of it for them all: each combine's operands on the way to a feature are placed once.
pub struct Body {
    found: HashMap<u32, Found>,
    placed: usize,
}

impl Body {
    pub fn new(root: &SdfNode) -> Body {
        // the operands read where their features can hold, round the root's box
        let reach = PAD + crate::program::CELL;
        let region = node_bounds(root).map_or(([f64::NEG_INFINITY; 3], [f64::INFINITY; 3]), |(lo, hi)| (lo.map(|v| v - reach), hi.map(|v| v + reach)));
        let slopes = Arc::new(Slopes::default());
        let mut body = Body { found: HashMap::new(), placed: 0 };
        let mut place = |n: SdfNode| Arc::new(Of::new(n, region, slopes.clone()));
        walk(root, &mut Vec::new(), &mut body, &mut place);
        body
    }

    /// How many operands it placed for its features' combines: each combine's once, however
    /// many features are under it.
    pub fn placed(&self) -> usize {
        self.placed
    }

    /// The feature tagged `id`; `None` when no node carries it.
    pub fn operand(&self, id: u32) -> Option<Operand> {
        let f = self.found.get(&id)?;
        // its box: its own, grown by the pad and by each blend's reach on its way
        let reach: f64 = f.path.iter().map(|a| blend_reach(a.blend, a.of.len())).sum();
        let bounds = node_bounds(&f.own).map(|(lo, hi)| (lo.map(|v| v - PAD - reach), hi.map(|v| v + PAD + reach)));
        let near = |o: &Of| match (bounds, o.bounds) {
            (Some((lo, hi)), Some((a, b))) => (0..3).all(|k| a[k] <= hi[k] && b[k] >= lo[k]),
            _ => true,
        };
        let steps = f
            .path
            .iter()
            .rev()
            .filter_map(|a| {
                let cut = matches!(a.kind, Kind::Cut);
                // a cut's base first (it decides where the cutter's face is the part's), then
                // the feature, then the other operands that can come near it
                let mut of = Vec::new();
                if cut {
                    of.push(a.of[0].clone());
                }
                let at = of.len();
                of.push(a.of[a.at].clone());
                for (j, o) in a.of.iter().enumerate() {
                    if j != a.at && !(cut && j == 0) && near(o) {
                        of.push(o.clone());
                    }
                }
                // a union operand alone near the feature: it holds all round
                (cut || of.len() > 1).then_some(Step { kind: a.kind, of, at })
            })
            .collect();
        Some(Operand { own: f.own.clone(), steps, bounds })
    }
}

/// The features tagged under `n` (placed by `frames`) that it found first: each one's own field
/// kept, and the combines on the way pushed onto its path as the walk comes back up (innermost
/// first).
fn walk(n: &SdfNode, frames: &mut Vec<Frame>, body: &mut Body, place: &mut dyn FnMut(SdfNode) -> Arc<Of>) -> Vec<u32> {
    let mut under = |frame: Frame, of: &SdfNode, body: &mut Body, place: &mut dyn FnMut(SdfNode) -> Arc<Of>| {
        frames.push(frame);
        let r = walk(of, frames, body, place);
        frames.pop();
        r
    };
    match n {
        SdfNode::Id { id, of } => {
            // the first node carrying a tag is its feature, as a search from the root meets it
            let first = !body.found.contains_key(id);
            if first {
                body.found.insert(*id, Found { own: placed(frames, of), path: Vec::new() });
            }
            let mut tags = walk(of, frames, body, place);
            if first {
                tags.push(*id);
            }
            tags
        }
        SdfNode::Material { of, .. } | SdfNode::Bone { of, .. } | SdfNode::Round { of, .. } | SdfNode::Shell { of, .. } | SdfNode::Sculpt { of, .. } | SdfNode::Texture { of, .. } => walk(of, frames, body, place),
        SdfNode::Transform { at, rotate, scale, of } => under(Frame::Transform { at: *at, rotate: *rotate, scale: *scale }, of, body, place),
        SdfNode::Deform { deform, of, .. } => under(Frame::Deform(*deform), of, body, place),
        SdfNode::Instance { places, of } => under(Frame::Instance(places.clone()), of, body, place),
        SdfNode::Combine { op, blend, of } => {
            let mut shared: Option<Arc<Vec<Arc<Of>>>> = None;
            let mut all = Vec::new();
            for (k, child) in of.iter().enumerate() {
                let tags = walk(child, frames, body, place);
                if tags.is_empty() {
                    continue;
                }
                let kind = match op {
                    SdfOp::Union if of.len() > 1 => Some(Kind::Union),
                    SdfOp::Difference if k > 0 => Some(Kind::Cut),
                    _ => None,
                };
                if let Some(kind) = kind {
                    let ops = match &shared {
                        Some(s) => s.clone(),
                        None => {
                            let s: Arc<Vec<Arc<Of>>> = Arc::new(of.iter().map(|c| place(placed(frames, c))).collect());
                            body.placed += s.len();
                            shared = Some(s.clone());
                            s
                        }
                    };
                    for t in &tags {
                        body.found.get_mut(t).expect("a tag found below").path.push(Along { blend: *blend, kind, of: ops.clone(), at: k });
                    }
                }
                all.extend(tags);
            }
            all
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bake::{Surface, Volume};
    use crate::program::{Channel, Layer, Op, Program, Region};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A ball's distance that counts the points it answers: a costly leaf (an imported mesh, a
    /// subdivided cage) in a part's field.
    struct Counted {
        r: f64,
        reads: Arc<AtomicUsize>,
    }

    impl Sdf3 for Counted {
        fn distance(&self, p: Vec3d) -> f64 {
            self.reads.fetch_add(1, Ordering::Relaxed);
            p.length() - self.r
        }
    }

    fn at(p: P3, n: SdfNode) -> SdfNode {
        SdfNode::Transform { at: p, rotate: [0.0; 3], scale: 1.0, of: Box::new(n) }
    }

    fn blend(k: f64, of: Vec<SdfNode>) -> SdfNode {
        SdfNode::Combine { op: SdfOp::Union, blend: k, of }
    }

    fn id(id: u32, n: SdfNode) -> SdfNode {
        SdfNode::Id { id, of: Box::new(n) }
    }

    /// The bow's two costly leaves: their middles and radius.
    const LEAVES: [P3; 2] = [[0.0, 18.0, 0.0], [5.0, 17.0, 0.0]];
    const LEAF: f64 = 3.0;

    /// A part as a figure is made: an arm (a box) blended onto a head, the head a ball with a
    /// bow of two costly leaves blended on top; the arm, the ball and the bow are features. The
    /// bow is in the arm's way (inside the head, the arm's neighbour) and the ball's (its
    /// neighbour), yet it can decide nothing far from itself.
    fn figure(reads: &Arc<AtomicUsize>) -> SdfNode {
        figure_with_bow_moved(reads, 0.0)
    }

    /// [`figure`] with its bow moved `dx` along X.
    fn figure_with_bow_moved(reads: &Arc<AtomicUsize>, dx: f64) -> SdfNode {
        let leaf = |key: u64, p: P3| at([p[0] + dx, p[1], p[2]], SdfNode::Kernel(SdfKernel { key, field: Arc::new(Counted { r: LEAF, reads: reads.clone() }), splash: None, bounds: Some(([-LEAF; 3], [LEAF; 3])), solid: None }));
        let bow = blend(0.5, vec![leaf(1, LEAVES[0]), leaf(2, LEAVES[1])]);
        let head = blend(1.0, vec![id(3, SdfNode::Sphere { radius: 16.0 }), id(4, bow)]);
        let arm = at([-29.0, 0.0, 0.0], SdfNode::Box { size: [30.0, 8.0, 8.0], radius: 0.0 });
        blend(1.0, vec![id(1, arm), id(2, head)])
    }

    /// The figure's fills of `tags` (tag, slot), its field's surface, and the leaves' slopes
    /// measured (on a region's first read: once, before a bake).
    fn painted(root: &SdfNode, tags: &[(u32, u8)]) -> (Program, Surface, Bounds) {
        let body = Body::new(root);
        let layers: Vec<Layer> = tags.iter().map(|&(t, slot)| Layer { region: Region::Operand(Arc::new(vec![body.operand(t).expect("tagged")])), op: Op::Paint { channel: Channel::Color, slot } }).collect();
        let program = Program::new(layers);
        for p in [[-29.0, 0.0, 4.0], [0.0, 0.0, 16.0], [0.0, 21.0, 0.0]] {
            program.sample(Channel::Color, p);
        }
        let (lo, hi) = node_bounds(root).expect("bounded");
        let shape = SdfShape { root: root.clone(), min: lo, max: hi, materials: Vec::new() };
        let surface = Surface::new(&shape, &[[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]]).expect("a frame");
        (program, surface, (lo, hi))
    }

    const CELL: f64 = 0.2;

    fn serial(len: usize, f: &(dyn Fn(usize) + Sync)) {
        (0..len).for_each(f)
    }

    /// A volume's sample bricks and their samples.
    fn sample_bricks(v: &Volume) -> HashMap<[i32; 3], &[f32]> {
        let b = |i: usize| {
            let i = i as i32;
            [v.min[0] + i % v.dim[0], v.min[1] + (i / v.dim[0]) % v.dim[1], v.min[2] + i / (v.dim[0] * v.dim[1])]
        };
        v.index.iter().enumerate().filter(|(_, e)| **e >= 0.0).map(|(k, e)| (b(k), &v.samples[*e as usize * 512..*e as usize * 512 + 512])).collect()
    }

    fn middle(b: [i32; 3]) -> P3 {
        b.map(|k| (k as f64 * 8.0 + 3.5) * CELL)
    }

    /// Every feature of a body comes from one walk of it: the combines' operands are placed once
    /// for them all, and nothing is read until a point is.
    #[test]
    fn a_body_s_features_share_their_combines_and_read_nothing_until_asked() {
        let reads = Arc::new(AtomicUsize::new(0));
        let body = Body::new(&figure(&reads));
        let features: Vec<Operand> = [1, 2, 3, 4].iter().map(|t| body.operand(*t).expect("tagged")).collect();
        // the root's two operands and the head's two, whatever the features under them, and
        // the features' regions read those between them
        assert_eq!(body.placed(), 4);
        assert_eq!(reads.load(Ordering::Relaxed), 0, "building the regions read the costly leaves");
        let mut read: Vec<*const Of> = features.iter().flat_map(|f| f.steps.iter().flat_map(|s| s.of.iter().map(Arc::as_ptr))).collect();
        read.sort();
        read.dedup();
        assert_eq!(read.len(), 4, "the features placed operands of their own");
    }

    /// The arm's fill parts where the arm meets the head, far from the bow: its bake reads none
    /// of the bow's costly leaves, though the head (the bow in it) is the arm's neighbour. And
    /// it bakes only the bricks round the surface: the region's edge runs on through the inside
    /// and out into the air, where no view reads it.
    #[test]
    fn a_fill_s_bake_reads_no_costly_leaf_far_from_its_edge() {
        let reads = Arc::new(AtomicUsize::new(0));
        let root = figure(&reads);
        let (program, surface, part) = painted(&root, &[(1, 1)]);
        reads.store(0, Ordering::Relaxed);
        let v = Volume::bake(&program, Channel::Color, part, CELL, Some(&surface), &serial).expect("baked");
        let n = reads.load(Ordering::Relaxed);
        let whole = Volume::bake(&program, Channel::Color, part, CELL, None, &serial).expect("baked");
        println!("the arm's fill: {} sample bricks round the surface ({} over the whole box), the costly leaves read {n} times", v.bricks(), whole.bricks());
        assert!(v.bricks() > 0);
        assert_eq!(n, 0, "the arm's bake read the bow's leaves");
        // the bricks the whole box holds samples in round the surface, the same samples, and
        // only those: the rest the surface comes no nearer than a brick to
        let (round, all) = (sample_bricks(&v), sample_bricks(&whole));
        let reach = |b: [i32; 3]| (b.map(|v| ((v - 1) * 8) as f64 * CELL), b.map(|v| ((v + 2) * 8 - 1) as f64 * CELL));
        for (b, samples) in &all {
            match round.get(b) {
                Some(s) => assert_eq!(s, samples, "brick {b:?} baked other samples round the surface"),
                None => assert!(!surface.passes(&reach(*b)), "brick {b:?} the surface passes was not baked"),
            }
        }
        assert!(round.keys().all(|b| all.contains_key(b)));
        let half = 0.5 * 24.0 * CELL * 3f64.sqrt();
        for b in round.keys() {
            let c = middle(*b);
            let d = makepad_csg_sdf::distance(&root, Vec3d::new(c[0], c[1], c[2]));
            assert!(d.abs() <= half, "a sample brick at {c:?} is {d:.2} mm off the surface");
        }
        assert!(round.len() < all.len(), "the whole box held no brick the surface does not pass");
    }

    /// All three fills baked: the costly leaves are read only round the bow, where the bow's and
    /// the ball's edges are; at most once a point per region operand holding them (the bow's
    /// own, the ball's head and bow), for the point and each of its six slope probes, and once
    /// per brick for its middle. A brick far from the bow, baked again, reads none.
    #[test]
    fn a_bake_reads_a_costly_leaf_only_round_where_it_can_own_a_point() {
        let reads = Arc::new(AtomicUsize::new(0));
        let root = figure(&reads);
        let (program, surface, part) = painted(&root, &[(1, 1), (3, 2), (4, 3)]);
        reads.store(0, Ordering::Relaxed);
        let v = Volume::bake(&program, Channel::Color, part, CELL, Some(&surface), &serial).expect("baked");
        let n = reads.load(Ordering::Relaxed);
        // the sample bricks within a few millimetres of a leaf (the blends' and the pad's reach)
        let off = |c: P3| LEAVES.iter().map(|l| (0..3).map(|k| (c[k] - l[k]).powi(2)).sum::<f64>().sqrt() - LEAF).fold(f64::INFINITY, f64::min);
        let bricks: Vec<[i32; 3]> = sample_bricks(&v).into_keys().collect();
        let near = bricks.iter().filter(|b| off(middle(**b)) < 8.0).count();
        println!("three fills: {} sample bricks, {near} of them round the bow; the costly leaves read {n} times", bricks.len());
        assert!(n <= near * 512 * 2 * 3 * 7 + v.index.len() * 2 * 3, "the costly leaves were read {n} times for {near} bricks round the bow");
        // the sample bricks a chunk's reach and more from the leaves, baked again
        let far: Vec<(P3, P3)> = bricks.iter().filter(|b| off(middle(**b)) > 12.0).map(|b| (middle(*b), middle(*b))).collect();
        assert!(far.len() * 2 > bricks.len(), "{} of {} bricks are far from the bow", far.len(), bricks.len());
        let mut again = v.clone();
        reads.store(0, Ordering::Relaxed);
        again.rebake_boxes(&program, Channel::Color, &far, &serial).expect("baked");
        assert_eq!(reads.load(Ordering::Relaxed), 0, "{} bricks far from the bow read its leaves", far.len());
    }

    /// A bow moved along the head changes its part's fills only in the boxes the change gives:
    /// the arm's reads alike everywhere, the head's and the ball's in less than half of each
    /// fill's box, the bow's own anywhere in its box; outside those, every point reads the value
    /// it read before, bit for bit, or both values lie past the cap on one side.
    #[test]
    fn a_moved_bow_changes_its_parts_fills_only_in_the_boxes_it_gives() {
        let reads = Arc::new(AtomicUsize::new(0));
        let (was, now) = (Body::new(&figure(&reads)), Body::new(&figure_with_bow_moved(&reads, 1.0)));
        let (cap, leaf) = (4.8, 1.6);
        let (mut differ, mut alike) = (0, 0);
        for tag in [1, 2, 3, 4] {
            let (a, b) = (now.operand(tag).expect("tagged"), was.operand(tag).expect("tagged"));
            let (lo, hi) = a.bounds.expect("bounded");
            let within: Bounds = (lo.map(|v| v - 1.0), hi.map(|v| v + 1.0));
            let mut boxes: Vec<Bounds> = Vec::new();
            assert!(a.changed(&b, &within, leaf, cap, &mut boxes), "the same feature, its bow moved");
            if tag == 1 {
                assert!(boxes.is_empty(), "the arm's fill changed at {boxes:?}");
            }
            let volume = |b: &Bounds| (0..3).map(|k| b.1[k] - b.0[k]).product::<f64>();
            let share = boxes.iter().map(volume).sum::<f64>() / volume(&within);
            assert!(tag == 4 || share < 0.5, "feature {tag}'s fill changed in {:.0}% of its box", 100.0 * share);
            let inside = |p: P3| boxes.iter().any(|(lo, hi)| (0..3).all(|k| p[k] >= lo[k] && p[k] <= hi[k]));
            let step = 0.7;
            let n: [usize; 3] = std::array::from_fn(|k| ((within.1[k] - within.0[k]) / step) as usize);
            for i in 0..=n[0] {
                for j in 0..=n[1] {
                    for k in 0..=n[2] {
                        let p = [within.0[0] + i as f64 * step, within.0[1] + j as f64 * step, within.0[2] + k as f64 * step];
                        let (x, y) = (a.value(p), b.value(p));
                        let same = x.to_bits() == y.to_bits() || (x >= cap && y >= cap) || (x <= -cap && y <= -cap);
                        if inside(p) {
                            differ += usize::from(!same);
                        } else {
                            alike += 1;
                            assert!(same, "feature {tag} at {p:?}: {x} against {y}");
                        }
                    }
                }
            }
        }
        assert!(alike > 100_000 && differ > 100, "{alike} points read alike outside the changes, {differ} inside them differ");
    }
}
