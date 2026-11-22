//! Rays from the view, coarse to fine: tiles of the surface in a part's own frame, published for
//! the GPU to draw. A cone from each coarse tile finds where the surface can start, five samples
//! fit a plane to it, and a tile the plane does not fit splits in four. The work runs on the task
//! pool, on at most [`WORKERS`] threads and on none once the view has converged.

use makepad_csg_sdf::lipschitz::{ellipsoid_slope, ellipsoid_slope_near};
use makepad_csg_sdf::{transform_out_of, SdfDeform, SdfNode, SdfOp, SdfShape, TAPER_MIN};
use makepad_draw::makepad_platform::retained_instances::RetainedInstances;
use makepad_draw::makepad_platform::thread::{checkpoint, Class, TaskHandle, TaskPool};
use makepad_draw::makepad_platform::SignalToUI;
use makepad_draw::Mat4f;
use crate::field::{compile, CompiledField};
use crate::surface::{Chunk, Gaze, QuadBox, Surface, CHUNKS};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Floats per tile: five positions, each with its packed normal, and a word for the shader.
pub const TILE_SLOTS: usize = 24;

pub use fab_sdf_engine::CUT_SLOT;

/// The part and the section's bodies, when `root` is a part under a section.
pub fn under_section(root: &SdfNode) -> Option<(&SdfNode, &[SdfNode])> {
    match root {
        SdfNode::Combine { op: SdfOp::Intersection, of, .. } if of.iter().any(|n| matches!(n, SdfNode::Material { slot: CUT_SLOT, .. })) => Some((&of[0], &of[1..])),
        _ => None,
    }
}

/// Two vertices' cut values in one float a tile carries, as the shader reads them.
pub(crate) fn pack_cut(a: u16, b: u16) -> f32 {
    (a as u32 * 4096 + b as u32) as f32
}

const WORKERS: usize = 8;
pub(crate) const BATCH: usize = 1024;
const BASE: u32 = 16;
const LEVELS: usize = 5;
/// How close to the surface a sample ends, in pixels.
const HIT_PX: f64 = 0.1;
/// A still camera takes the finer tolerance after this long: three frames of a 120 Hz display,
/// so no display waits longer than its three frames did.
const SETTLE: Duration = Duration::from_millis(25);
/// A head has to rest for longer than a hand: it is often just passing through slowly. 18 frames
/// of a 120 Hz headset (no headset waits longer than its 18 frames did).
const HEAD_SETTLE: Duration = Duration::from_millis(150);
/// A head at rest still trembles: within this of where it stopped (a quarter of a degree, as a
/// tangent) it hasn't moved. Turning uncovers nothing but the edge of the view, and at this
/// little sideways an edge of a part uncovers less than a pixel of what lies behind it.
const HEAD_REST: f64 = 0.0044;
/// A part unedited this long has rested: its one mesh may be built.
const EDIT_QUIET_MS: u128 = 150;
const EDIT_QUIET: Duration = Duration::from_millis(EDIT_QUIET_MS as u64);
/// A drag's versions of view tiles go down to 2 px, the size at which they have no gaps, as long
/// as a version takes no longer than this, and each size coarser costs about this much less.
const DRAG_LEVELS: usize = 4;
const DRAG_SLOW_MS: f64 = 120.0;
const DRAG_LEVEL_COST: f64 = 3.0;
const DRAG_VERSIONS: usize = 3;

pub(crate) type V = [f64; 3];

pub(crate) fn add(a: V, b: V) -> V { [a[0] + b[0], a[1] + b[1], a[2] + b[2]] }
pub(crate) fn sub(a: V, b: V) -> V { [a[0] - b[0], a[1] - b[1], a[2] - b[2]] }
pub(crate) fn mul(a: V, s: f64) -> V { [a[0] * s, a[1] * s, a[2] * s] }
pub(crate) fn dot(a: V, b: V) -> f64 { a[0] * b[0] + a[1] * b[1] + a[2] * b[2] }
pub(crate) fn cross(a: V, b: V) -> V { [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]] }
pub(crate) fn len(a: V) -> f64 { dot(a, a).sqrt() }
pub(crate) fn norm(a: V) -> V { mul(a, 1.0 / len(a).max(1e-30)) }

/// The view in a part's own frame.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Cam {
    pub(crate) eye: V,
    f: V,
    r: V,
    u: V,
    /// Half the view per unit of depth, or (orthographic) per unit.
    half: [f64; 2],
    /// Where its middle is per unit of depth: a headset's eye looks off its axis.
    off: [f64; 2],
    pub(crate) ortho: bool,
    pub(crate) w: f64,
    pub(crate) h: f64,
}

impl Cam {
    /// From the pass's matrices and the part's placement (rigid, with one uniform scale). A still
    /// view's sub-pixel jitter is in the matrix too, and isn't a different view.
    pub fn new(view: &Mat4f, proj: &Mat4f, world_from_obj: &Mat4f, w: f64, h: f64) -> Cam {
        let i = Mat4f::mul(view, world_from_obj).invert();
        let col = |k: usize| -> V { [i.v[k * 4] as f64, i.v[k * 4 + 1] as f64, i.v[k * 4 + 2] as f64] };
        let ortho = proj.v[11] == 0.0;
        let scale = if ortho { len(col(0)) } else { 1.0 };
        Cam {
            eye: col(3),
            f: mul(norm(col(2)), -1.0),
            r: norm(col(0)),
            u: norm(col(1)),
            half: [scale / proj.v[0] as f64, scale / proj.v[5] as f64],
            off: [0.0; 2],
            ortho,
            w,
            h,
        }
    }

    /// A headset eye's: it looks off its axis.
    pub fn of_eye(view: &Mat4f, proj: &Mat4f, world_from_obj: &Mat4f, w: f64, h: f64) -> Cam {
        Cam { off: [(proj.v[8] / proj.v[0]) as f64, (proj.v[9] / proj.v[5]) as f64], ..Cam::new(view, proj, world_from_obj, w, h) }
    }

    #[cfg(test)]
    pub(crate) fn looking(eye: V, at: V, w: f64, h: f64) -> Cam {
        let f = norm(sub(at, eye));
        let r = norm(cross(f, [0.0, 0.0, 1.0]));
        let t = (20f64).to_radians().tan();
        Cam { eye, f, r, u: cross(r, f), half: [t * w / h, t], off: [0.0; 2], ortho: false, w, h }
    }
    /// Whether a head looking from here has not moved from `at`, as seen from `part`.
    fn rests_at(&self, at: &Cam, part: V) -> bool {
        let cos = 1.0 / (1.0 + HEAD_REST * HEAD_REST).sqrt();
        let close = |a: [f64; 2], b: [f64; 2]| (a[0] - b[0]).abs() <= HEAD_REST && (a[1] - b[1]).abs() <= HEAD_REST;
        close(self.half, at.half)
            && close(self.off, at.off)
            && (self.w - at.w).abs() <= at.w * 0.01
            && (self.h - at.h).abs() <= at.h * 0.01
            && dot(self.f, at.f) >= cos
            && dot(self.r, at.r) >= cos
            && len(sub(self.eye, at.eye)) <= HEAD_REST * len(sub(part, at.eye))
    }

    /// The view from between two eyes, holding what either sees at infinity, at their pixel size.
    pub fn between(a: &Cam, b: &Cam) -> Cam {
        let f = norm(add(a.f, b.f));
        let sum = add(a.r, b.r);
        let r = norm(sub(sum, mul(f, dot(sum, f))));
        let u = cross(r, f);
        let mut half = [0.0f64; 2];
        for c in [a, b] {
            for (x, y) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                let d = add(c.f, add(mul(c.r, x * c.half[0] + c.off[0]), mul(c.u, y * c.half[1] + c.off[1])));
                let z = dot(d, f).max(1e-6);
                half = [half[0].max(dot(d, r).abs() / z), half[1].max(dot(d, u).abs() / z)];
            }
        }
        let size = [(a.w * half[0] / a.half[0]).ceil(), (a.h * half[1] / a.half[1]).ceil()];
        Cam { eye: mul(add(a.eye, b.eye), 0.5), f, r, u, half, off: [0.0; 2], ortho: false, w: size[0], h: size[1] }
    }
    pub(crate) fn ray(&self, x: f64, y: f64) -> (V, V) {
        let (ndx, ndy) = (x / self.w * 2.0 - 1.0, 1.0 - y / self.h * 2.0);
        let across = add(mul(self.r, ndx * self.half[0] + self.off[0]), mul(self.u, ndy * self.half[1] + self.off[1]));
        if self.ortho {
            return (add(self.eye, across), self.f);
        }
        (self.eye, norm(add(self.f, across)))
    }
    /// How far along the view a point lies.
    pub(crate) fn depth(&self, p: V) -> f64 {
        dot(sub(p, self.eye), self.f)
    }
    /// From the eye toward a point.
    pub(crate) fn dir_to(&self, p: V) -> V {
        if self.ortho { self.f } else { norm(sub(p, self.eye)) }
    }
    /// Whether a box can show in this view: whether the ball round it reaches the frustum.
    pub fn sees_box(&self, b: &QuadBox) -> bool {
        let (lo, hi) = (b.lo.map(|x| x as f64), b.hi.map(|x| x as f64));
        self.sees(mul(add(lo, hi), 0.5), len(sub(hi, lo)) * 0.5)
    }

    /// A ball reaches the view's frustum.
    pub(crate) fn sees(&self, c: V, r: f64) -> bool {
        let q = sub(c, self.eye);
        let z = dot(q, self.f);
        if z < -r {
            return false;
        }
        let (x, y) = ((dot(q, self.r) - z * self.off[0]).abs(), (dot(q, self.u) - z * self.off[1]).abs());
        if self.ortho {
            return x <= self.half[0] + r && y <= self.half[1] + r;
        }
        let steep = |k: usize| (self.half[k] + self.off[k].abs()) * (self.half[k] + self.off[k].abs());
        x - z * self.half[0] <= r * (1.0 + steep(0)).sqrt() && y - z * self.half[1] <= r * (1.0 + steep(1)).sqrt()
    }
    /// What a pixel spans at distance t.
    pub(crate) fn pw(&self, t: f64) -> f64 {
        if self.ortho { 2.0 * self.half[1] / self.h } else { t * 2.0 * self.half[1] / self.h }
    }
    pub(crate) fn proj(&self, p: V) -> Option<(f64, f64, f64)> {
        let q = sub(p, self.eye);
        let z = dot(q, self.f);
        let d = if self.ortho { 1.0 } else if z < 1e-6 { return None } else { z };
        let (x, y) = ((dot(q, self.r) / d - self.off[0]) / self.half[0], (dot(q, self.u) / d - self.off[1]) / self.half[1]);
        Some(((x + 1.0) * 0.5 * self.w, (1.0 - y) * 0.5 * self.h, z))
    }
    /// The cone holding the tile (x0, y0) of side s: origin, axis, radius r0 + tan t.
    /// How far along a ray of direction `d` the view's depth is z.
    pub(crate) fn along(&self, d: V, z: f64) -> f64 {
        if self.ortho { z } else { z / dot(d, self.f) }
    }
    pub(crate) fn cone(&self, x0: f64, y0: f64, s: f64) -> (V, V, f64, f64) {
        let (o, d) = self.ray(x0 + s * 0.5, y0 + s * 0.5);
        if self.ortho {
            return (o, d, s * std::f64::consts::SQRT_2 * 0.5 * self.pw(1.0), 0.0);
        }
        let mut tan = 0.0f64;
        for (cx, cy) in [(0.0, 0.0), (s, 0.0), (s, s), (0.0, s)] {
            let (_, c) = self.ray(x0 + cx, y0 + cy);
            tan = tan.max(len(cross(d, c)) / dot(d, c).max(1e-9));
        }
        (o, d, 0.0, tan)
    }
}

pub(crate) fn slab(o: V, d: V, lo: V, hi: V) -> Option<(f64, f64)> {
    let (mut a, mut b) = (f64::NEG_INFINITY, f64::INFINITY);
    for i in 0..3 {
        if d[i].abs() < 1e-12 {
            if o[i] < lo[i] || o[i] > hi[i] {
                return None;
            }
            continue;
        }
        let (t0, t1) = ((lo[i] - o[i]) / d[i], (hi[i] - o[i]) / d[i]);
        a = a.max(t0.min(t1));
        b = b.min(t0.max(t1));
    }
    (b >= a.max(0.0)).then_some((a.max(0.0), b))
}

/// Runs `f` over `items` in batches on at most [`WORKERS`] threads. A worker another job wants
/// leaves at the end of its batch.
pub(crate) fn par<T: Send>(pool: &TaskPool, items: &mut [T], batch: usize, f: impl Fn(&mut [T]) + Sync) {
    if items.is_empty() {
        return;
    }
    if items.len() <= batch {
        f(items);
        return;
    }
    let chunks: Vec<Mutex<&mut [T]>> = items.chunks_mut(batch).map(Mutex::new).collect();
    pool.fan_out_on(WORKERS, chunks.len(), |i| f(&mut chunks[i].lock().unwrap()));
}

/// Points as the compiled field reads them.
fn floats(pts: &[V]) -> Vec<f32> {
    pts.iter().flat_map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect()
}

const B_EMPTY: u8 = 0;
const B_INSIDE: u8 = 1;
const B_SURFACE: u8 = 2;

/// A part under a section and the section's bodies, each its own field: what a surface point shows
/// is the part's, whatever the section does there.
pub(crate) struct Sides {
    part: CompiledField,
    bodies: Vec<CompiledField>,
}

/// A node's field for the tracer. A node that does not compile says so in the log, and its part
/// does not draw.
fn compile_or_log(node: &SdfNode, slots: bool) -> Option<CompiledField> {
    match compile(node, slots) {
        Ok(c) => {
            if LOG_PASSES.load(Ordering::Relaxed) && c.compile_ms > 0.0 {
                makepad_draw::makepad_platform::log!("sdf field compiled: {:.1} ms", c.compile_ms);
            }
            Some(c)
        }
        Err(why) => {
            makepad_draw::makepad_platform::log!("sdf tiles: the field did not compile: {why}");
            None
        }
    }
}

/// A shape's field with its bricks: the one entry the tracer evaluates through.
pub struct Field {
    /// What every point is evaluated by.
    compiled: CompiledField,
    /// Several materials: each tile takes its slot, and splits where slots meet.
    pub(crate) slots: bool,
    /// A part under a section, on its own: each vertex says how far it is on the section's face.
    pub(crate) sides: Option<Sides>,
    /// The points evaluated so far.
    pub(crate) evals: AtomicU64,
    /// Multiplies the value into a 1-Lipschitz field.
    pub(crate) step: f64,
    /// How far from the origin `step` is proven; 0: it is only a measured slope, which proves nothing.
    pub(crate) proven: f64,
    /// The slope of all but the ellipsoids.
    flat: f64,
    /// The ellipsoids: `step` holds clear of these balls; the most a value can be off across them, scaled.
    spots: Vec<Spot>,
    jump: f64,
    /// |grad| of the scaled value near the surface.
    pub(crate) slope: f64,
    pub(crate) lo: V,
    pub(crate) hi: V,
    pub(crate) cell: f64,
    dim: [usize; 3],
    pub(crate) bsize: f64,
    bricks: Vec<u8>,
    skip: Vec<f32>,
    /// The part's features, for a highlight.
    pub(crate) ids: Ids,
}

/// The features of a part ([`SdfNode::Id`]), numbered in tree order from 1 (0: none): each tile
/// holds the ordinal of the one whose surface it shows, as a pick names it
/// ([`makepad_csg_sdf::distance_id`]: tint and pick agree, ties included), and the view resolves
/// an ordinal against `table`. A part of fewer than two evaluates nothing: its ordinal is
/// `constant`.
pub(crate) struct Ids {
    /// Ordinal - 1 to the feature's id.
    pub(crate) table: Arc<[u32]>,
    /// The part (not a section's cut of it), when it has two or more.
    node: Option<SdfNode>,
    /// (id, ordinal), by id.
    order: Vec<(u32, u16)>,
    pub(crate) constant: u16,
    /// Points looked up, and the time spent on them (ns), for the look's log.
    pub(crate) evals: AtomicU64,
    pub(crate) nanos: AtomicU64,
}

impl Ids {
    fn of(part: &SdfNode) -> Ids {
        let mut table = Vec::new();
        let mut seen = std::collections::HashSet::new();
        id_walk(part, &mut table, &mut seen);
        // past what a tile's float holds exactly a feature has no ordinal: nothing tints it
        table.truncate(u16::MAX as usize - 1);
        let mut order: Vec<(u32, u16)> = table.iter().enumerate().map(|(i, id)| (*id, i as u16 + 1)).collect();
        order.sort_unstable();
        let node = (table.len() >= 2).then(|| part.clone());
        let constant = if table.len() == 1 { 1 } else { 0 };
        Ids { table: table.into(), node, order, constant, evals: AtomicU64::new(0), nanos: AtomicU64::new(0) }
    }

    /// Points looked up so far and the time they took (ms).
    pub(crate) fn spent(&self) -> (u64, f64) {
        (self.evals.load(Ordering::Relaxed), self.nanos.load(Ordering::Relaxed) as f64 / 1e6)
    }

    fn ordinal(&self, id: u32) -> u16 {
        self.order.binary_search_by_key(&id, |e| e.0).map_or(0, |i| self.order[i].1)
    }
}

/// The distinct ids under `n`, first met first ([`makepad_csg_sdf::distance_id`]'s view of the
/// tree: a material's region is not walked).
fn id_walk(n: &SdfNode, table: &mut Vec<u32>, seen: &mut std::collections::HashSet<u32>) {
    if let SdfNode::Id { id, .. } = n {
        if seen.insert(*id) {
            table.push(*id);
        }
    }
    match n {
        SdfNode::Combine { of, .. } => of.iter().for_each(|c| id_walk(c, table, seen)),
        SdfNode::Shell { of, .. } | SdfNode::Round { of, .. } | SdfNode::Transform { of, .. } | SdfNode::Instance { of, .. } | SdfNode::Bone { of, .. } | SdfNode::Deform { of, .. } | SdfNode::Id { of, .. } | SdfNode::Sculpt { of, .. } | SdfNode::Texture { of, .. } | SdfNode::Material { of, .. } => id_walk(of, table, seen),
        _ => {}
    }
}

#[cfg(test)]
pub(crate) fn raw(node: &SdfNode, pts: &[V]) -> Vec<f64> {
    use makepad_csg_math::Vec3d;
    let q: Vec<Vec3d> = pts.iter().map(|p| Vec3d::new(p[0], p[1], p[2])).collect();
    let mut out = vec![0.0; pts.len()];
    makepad_csg_sdf::distance_many(node, &q, &mut out);
    out
}

/// An ellipsoid in the root's frame. Its estimate jumps at its centre with the direction a point
/// leaves in, so no slope holds in the ball `radius` about it; away from it [`ellipsoid_slope`] does.
#[derive(Clone, Copy)]
pub(crate) struct Spot {
    at: V,
    /// Its axes in the root's frame, at right angles, each at least `scale` long (longer under
    /// a stretch).
    axes: [V; 3],
    scale: f64,
    radii: [f64; 3],
    radius: f64,
    /// The most the value can differ, on top of the slope, for a path across it.
    jump: f64,
}

impl Spot {
    fn slope_near(&self, p: V, ball: f64) -> f64 {
        ellipsoid_slope_near(self.radii, self.local(p), ball / self.scale)
    }
    /// `p` in the ellipsoid's own frame.
    fn local(&self, p: V) -> V {
        let d = sub(p, self.at);
        self.axes.map(|a| dot(d, a) / dot(a, a))
    }
    /// A ball about `p` that stays off the centre, the one point no slope holds at: half way there
    /// in the ellipsoid's own measure.
    fn off_centre(&self, p: V) -> f64 {
        let q = self.local(p);
        let k = (0..3).map(|i| (q[i] / self.radii[i]) * (q[i] / self.radii[i])).sum::<f64>().sqrt();
        0.5 * k * self.radii[0].min(self.radii[1]).min(self.radii[2]) * self.scale
    }
}

/// The most `n`'s value can climb per unit of distance within `ball` of its frame's origin, by each
/// node's own formula, never by sampling. Each ellipsoid goes to `spots`, not into the slope. `out`
/// takes a point of this frame to the root's, and `scale` is a length here in the root's. None:
/// nothing bounds it (see each node).
fn declared_slope(n: &SdfNode, ball: f64, out: &dyn Fn(V) -> V, scale: f64, spots: &mut Vec<Spot>) -> Option<f64> {
    match n {
        // true distances, or built of them (the links', the tubes' and the sketches' own
        // 1-Lipschitz fields)
        SdfNode::Sphere { .. } | SdfNode::Box { .. } | SdfNode::Cylinder { .. } | SdfNode::Torus { .. } | SdfNode::Capsule { .. } | SdfNode::Links(_) | SdfNode::Tubes(_) | SdfNode::Sketch(_) => Some(1.0),
        SdfNode::Ellipsoid { radii } => {
            let lo = radii[0].min(radii[1]).min(radii[2]);
            let hi = radii[0].max(radii[1]).max(radii[2]);
            if hi > lo {
                let at = out([0.0; 3]);
                let axes = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]].map(|e| sub(out(e), at));
                // the ball round its centre the slope does not hold in, as long as its longest axis
                let longest = axes.iter().map(|a| len(*a)).fold(scale, f64::max);
                spots.push(Spot { at, axes, scale, radii: *radii, radius: hi * 0.5 * longest, jump: (hi - lo * 0.5) * scale });
            }
            Some(1.0)
        }
        // |G| / k: each partial squared is at most cos^2 + sin^2 of two different angles (Cauchy), and
        // the three of them sum to 3
        SdfNode::Gyroid { .. } => Some(3f64.sqrt()),
        SdfNode::Kernel(k) => {
            let field = k.splash.as_ref()?;
            let b = fab_sdf_proof::bound(field, &field.params(), [0.0; 3], (ball * (1.0 + 1.0e-6)) as f32)?;
            (b[3] < 1.0e29).then_some(b[3] as f64 * (1.0 + 1.0e-6))
        }
        // the exact distance; the winding number's sign flips only on the triangles of a closed mesh,
        // an open one's flips across its holes, where the value jumps
        SdfNode::Mesh(m) => m.watertight.then_some(1.0),
        // a blend's partials are h and 1 - h: never steeper than its steepest operand
        SdfNode::Combine { of, .. } => of.iter().try_fold(1.0f64, |l, c| Some(l.max(declared_slope(c, ball, out, scale, spots)?))),
        SdfNode::Transform { at, rotate, scale: s, of } => declared_slope(of, (ball + len(*at)) / s, &|q| out(transform_out_of(*at, *rotate, *s, q)), scale * s, spots),
        SdfNode::Instance { places, of } => places.iter().try_fold(1.0f64, |l, p| Some(l.max(declared_slope(of, (ball + len(p.at)) / p.scale, &|q| out(p.out_of(q)), scale * p.scale, spots)?))),
        // the warp's stretch over the ball, against what the node divides by (its own bounds' stretch)
        SdfNode::Deform { deform, lipschitz, of } => {
            let (reach, over) = match deform {
                SdfDeform::Taper { rate } => {
                    let least = 1.0 - rate.abs() * ball;
                    // it clamps within the ball: nothing useful holds the slope there
                    if least <= TAPER_MIN {
                        return None;
                    }
                    let r = ball / least;
                    (r, ([-r, -ball, -r], [r, ball, r]))
                }
                // a stretch is no warp: its operand's frame goes to this one by the stretch,
                // which only lengthens (its least factor is 1), so its slopes and its ellipsoids'
                // hold here, those found where the stretch takes them
                SdfDeform::Stretch { scale: k } => {
                    let least = k[0].min(k[1]).min(k[2]);
                    let l = declared_slope(of, ball / least, &|q| out([q[0] * k[0], q[1] * k[1], q[2] * k[2]]), scale * least, spots)?;
                    return Some(l * deform.lipschitz(Some(&([-ball; 3], [ball; 3]))) / lipschitz);
                }
                _ => (ball, ([-ball; 3], [ball; 3])),
            };
            let spots_before = spots.len();
            let l = declared_slope(of, reach, out, scale, spots)?;
            // a spot doesn't carry through a warp
            (spots.len() == spots_before).then(|| l * deform.lipschitz(Some(&over)) / lipschitz)
        }
        // a point reads only the strokes indexed to its brick, so past a stroke's reach the value may
        // stand for a surface further than the stroke's: nothing ties strokes to the surface
        SdfNode::Sculpt { strokes, of } => strokes.is_empty().then(|| declared_slope(of, ball, out, scale, spots)).flatten(),
        // its divisor covers the pattern, the masks and a 1-Lipschitz source; a steeper source stays
        // as steep. The smoothed gradient turns a jump in the source into any mask: no spot under it
        SdfNode::Texture { tex, of } => {
            let spots_before = spots.len();
            let l = declared_slope(of, ball + tex.reach(), out, scale, spots)?;
            (spots.len() == spots_before || !tex.needs_gradient()).then_some(l.max(1.0))
        }
        SdfNode::Shell { of, .. } | SdfNode::Round { of, .. } | SdfNode::Bone { of, .. } | SdfNode::Id { of, .. } | SdfNode::Material { of, .. } => declared_slope(of, ball, out, scale, spots),
    }
}

impl Field {
    /// An open-ended loop of this field has stopped at its cap.
    pub(crate) fn loop_stopped(&self) -> bool {
        self.compiled.loop_stopped() || self.sides.as_ref().is_some_and(|s| s.part.loop_stopped() || s.bodies.iter().any(|b| b.loop_stopped()))
    }

    pub(crate) fn new(root: &SdfNode, slots: bool, pool: &TaskPool) -> Option<Field> {
        let b = makepad_csg_sdf::node_bounds(root)?;
        let ext = (0..3).map(|i| b.1[i] - b.0[i]).fold(0.0f64, f64::max);
        let cell = (ext / 192.0).clamp(0.08, 1.0);
        let pad = cell * 2.0;
        let lo: V = b.0.map(|x| x - pad);
        let hi: V = b.1.map(|x| x + pad);
        let bsize = cell * 8.0;
        let dim = [0, 1, 2].map(|i| ((hi[i] - lo[i]) / bsize).ceil() as usize);
        let compiled = compile_or_log(root, slots && under_section(root).is_none())?;
        let sides = match under_section(root) {
            Some((part, bodies)) => {
                let part = compile_or_log(part, slots)?;
                let bodies = bodies.iter().map(|b| compile_or_log(b, false)).collect::<Option<Vec<_>>>()?;
                Some(Sides { part, bodies })
            }
            None => None,
        };
        let ids = Ids::of(under_section(root).map_or(root, |(part, _)| part));
        let mut f = Field { compiled, slots, sides, evals: AtomicU64::new(0), step: 1.0, proven: 0.0, flat: 1.0, spots: Vec::new(), jump: 0.0, slope: 1.0, lo, hi, cell, dim, bsize, bricks: Vec::new(), skip: Vec::new(), ids };
        let n = dim[0] * dim[1] * dim[2];
        // a proven bound over twice the part, room for the balls a tile's proof looks from
        let ball = 2.0 * (len(mul(add(lo, hi), 0.5)) + len(sub(hi, lo)) * 0.5);
        let mut spots = Vec::new();
        if let Some(l) = declared_slope(root, ball, &|q| q, 1.0, &mut spots) {
            f.flat = l;
            f.step = 1.0 / spots.iter().fold(l, |l, s| l.max(ellipsoid_slope(s.radii)));
            f.proven = ball;
            f.jump = spots.iter().map(|s| s.jump).fold(0.0, f64::max) * f.step;
            f.spots = spots;
        } else {
            // the steepest slope near the surface, on a grid of the bricks, a tenth more; within 1..8
            let mut jobs: Vec<([V; 4], [f64; 4])> = (0..n)
                .map(|i| {
                    let (l, u) = f.brick_box(i);
                    let c = mul(add(l, u), 0.5);
                    ([c, add(c, [cell, 0.0, 0.0]), add(c, [0.0, cell, 0.0]), add(c, [0.0, 0.0, cell])], [0.0; 4])
                })
                .collect();
            par(pool, &mut jobs, BATCH / 4, |c| {
                let pts: Vec<V> = c.iter().flat_map(|j| j.0).collect();
                let e = f.value(&pts);
                for (j, v) in c.iter_mut().zip(e.chunks(4)) {
                    j.1.copy_from_slice(v);
                }
            });
            let mut l = 1.0f64;
            for (_, c) in &jobs {
                if c[0].abs() > cell * 20.0 {
                    continue;
                }
                let m = len([(c[1] - c[0]) / cell, (c[2] - c[0]) / cell, (c[3] - c[0]) / cell]);
                if m.is_finite() {
                    l = l.max(m);
                }
            }
            f.step = 1.0 / if l <= 1.05 { 1.0 } else { (l * 1.1).clamp(1.0, 8.0) };
        }
        // the bricks, by the bound at their centres
        let r = bsize * 3f64.sqrt() * 0.5;
        let mut jobs: Vec<(usize, u8, f32)> = (0..n).map(|i| (i, B_SURFACE, 0.0)).collect();
        par(pool, &mut jobs, BATCH, |c| {
            let pts: Vec<V> = c.iter().map(|j| { let (l, h) = f.brick_box(j.0); mul(add(l, h), 0.5) }).collect();
            let (mut v, mut bo) = (Vec::new(), Vec::new());
            f.eval(&pts, &mut v, &mut bo);
            for (k, j) in c.iter_mut().enumerate() {
                if bo[k] > r {
                    j.1 = B_EMPTY;
                    j.2 = (bo[k] - r) as f32;
                } else if bo[k] < -r {
                    j.1 = B_INSIDE;
                }
            }
        });
        f.bricks = jobs.iter().map(|j| j.1).collect();
        f.skip = jobs.iter().map(|j| j.2).collect();
        // the slope near the surface
        let surface: Vec<usize> = (0..n).filter(|i| f.bricks[*i] == B_SURFACE).collect();
        let stride = (surface.len() / 4000).max(1);
        let pts: Vec<V> = surface.iter().step_by(stride).map(|i| { let (l, u) = f.brick_box(*i); mul(add(l, u), 0.5) }).collect();
        let (v, grad) = f.gradient(&pts);
        let mut g: Vec<f64> = v.iter().zip(&grad).filter(|(v, _)| (*v * f.step).abs() < cell * 2.0).map(|(_, g)| len(*g) * f.step).collect();
        g.sort_by(|a, b| a.total_cmp(b));
        f.slope = if g.is_empty() { 1.0 } else { g[g.len() / 2].clamp(0.05, 1.0) };
        Some(f)
    }
    /// `node`, counted.
    /// The value, the bound and the solid depth, n words each, as the kernel gives them.
    fn kernel(&self, pts: &[V]) -> Vec<f32> {
        self.evals.fetch_add(pts.len() as u64, Ordering::Relaxed);
        let n = pts.len();
        let mut w = vec![0.0f32; n * 6];
        let (out, xyz) = w.split_at_mut(n * 3);
        for (x, p) in xyz.chunks_exact_mut(3).zip(pts) {
            x[0] = p[0] as f32;
            x[1] = p[1] as f32;
            x[2] = p[2] as f32;
        }
        let (value, rest) = out.split_at_mut(n);
        let (bound, solid) = rest.split_at_mut(n);
        self.compiled.eval(xyz, value, bound, solid);
        w
    }
    /// The field's value per point, its slope not taken out.
    pub(crate) fn value(&self, pts: &[V]) -> Vec<f64> {
        self.kernel(pts)[..pts.len()].iter().map(|v| *v as f64).collect()
    }
    /// The field's value and its gradient per point, its slope not taken out: one eval a point.
    pub(crate) fn gradient(&self, pts: &[V]) -> (Vec<f64>, Vec<V>) {
        self.evals.fetch_add(pts.len() as u64, Ordering::Relaxed);
        let n = pts.len();
        let xyz = floats(pts);
        let mut w = vec![0.0f32; n * 4];
        let (value, grad) = w.split_at_mut(n);
        self.compiled.gradient.eval(&xyz, value, grad, (self.cell * NORMAL_STEP) as f32);
        (value.iter().map(|v| *v as f64).collect(), grad.chunks_exact(3).map(|g| [g[0] as f64, g[1] as f64, g[2] as f64]).collect())
    }
    /// The material slot per point, in the part, whatever a section does there.
    pub(crate) fn slots_at(&self, pts: &[V]) -> Vec<u8> {
        self.evals.fetch_add(pts.len() as u64, Ordering::Relaxed);
        let xyz = floats(pts);
        let mut w = vec![0.0f32; pts.len() * 2];
        let (value, slot) = w.split_at_mut(pts.len());
        self.sides.as_ref().map_or(&self.compiled, |s| &s.part).eval_slots(&xyz, value, slot);
        slot.iter().map(|s| *s as u8).collect()
    }
    /// The ordinal ([`Ids`]) of the feature whose surface each point is on, in the part, whatever
    /// a section does there: interpreted, f64, as the pick finds it.
    pub(crate) fn ords_at(&self, pts: &[V]) -> Vec<u16> {
        let Some(node) = &self.ids.node else { return vec![self.ids.constant; pts.len()] };
        let began = Instant::now();
        let out = pts.iter().map(|p| makepad_csg_sdf::distance_id(node, makepad_csg_math::Vec3d::new(p[0], p[1], p[2])).1.map_or(0, |id| self.ids.ordinal(id))).collect();
        self.ids.evals.fetch_add(pts.len() as u64, Ordering::Relaxed);
        self.ids.nanos.fetch_add(began.elapsed().as_nanos() as u64, Ordering::Relaxed);
        out
    }

    /// Whether tiles look their feature up (two or more): else each holds `ids.constant`.
    pub(crate) fn has_ids(&self) -> bool {
        self.ids.node.is_some()
    }

    /// The table a tile's ordinal resolves against: ordinal - 1 to the feature's id.
    pub fn id_table(&self) -> Arc<[u32]> {
        self.ids.table.clone()
    }

    /// Per surface point, whether it is a section's face, as a value a triangle interpolates: the
    /// section's distance less the part's, above 2047.5 where the section makes the surface. The
    /// branch of the field decides, so a wall grazing the plane is wall, and interpolated this ends
    /// on the crease itself however large the mesh's cells are there.
    pub(crate) fn cuts_at(&self, pts: &[V]) -> Vec<u16> {
        let Some(sides) = &self.sides else { return vec![0; pts.len()] };
        let n = pts.len();
        let xyz = floats(pts);
        let mut w = vec![0.0f32; n * 3];
        let value_of = |field: &CompiledField, w: &mut [f32]| {
            let (value, rest) = w.split_at_mut(n);
            let (bound, solid) = rest.split_at_mut(n);
            field.eval(&xyz, value, bound, solid);
            value[..n].iter().map(|v| *v as f64).collect::<Vec<f64>>()
        };
        let mut section = vec![f64::NEG_INFINITY; n];
        for body in &sides.bodies {
            self.evals.fetch_add(n as u64, Ordering::Relaxed);
            for (s, v) in section.iter_mut().zip(value_of(body, &mut w)) {
                *s = s.max(v);
            }
        }
        self.evals.fetch_add(n as u64, Ordering::Relaxed);
        let part = value_of(&sides.part, &mut w);
        section.iter().zip(&part).map(|(s, p)| (((s - p) / (self.cell * 4.0)).clamp(-1.0, 1.0) * 0.5 + 0.5) * 4095.0).map(|v| v.round() as u16).collect()
    }
    /// The value and a proven lower bound of the distance to the surface, per point.
    fn eval(&self, pts: &[V], value: &mut Vec<f64>, bound: &mut Vec<f64>) {
        let n = pts.len();
        let out = self.kernel(pts);
        value.clear();
        value.extend(out[..n].iter().map(|v| *v as f64 * self.step));
        bound.clear();
        bound.extend(out[n..n * 2].iter().map(|b| *b as f64 * self.step));
        if !self.spots.is_empty() {
            for (b, p) in bound.iter_mut().zip(pts) {
                *b = self.cleared(*p, b.abs()).copysign(*b);
            }
        }
    }
    /// What to multiply a scaled value at `p` by for how far it proves no surface within `ball`
    /// of `p`: an ellipsoid is far less steep near its surface than `step` allows for anywhere.
    fn near(&self, p: V, ball: f64) -> f64 {
        if self.spots.is_empty() {
            return 1.0;
        }
        1.0 / (self.step * self.spots.iter().fold(self.flat, |l, s| l.max(s.slope_near(p, ball).min(1.0 / self.step))))
    }
    /// How far from `p` a scaled value of `v` (>= 0) proves no surface. The slope holds in any ball
    /// clear of the spots, so all of `v` up to the nearest spot; in one, inside a hollow part say,
    /// what the jump leaves across it, or the far steeper slope of a ball short of its centre.
    fn cleared(&self, p: V, v: f64) -> f64 {
        let room = self.spots.iter().fold(f64::INFINITY, |r, s| r.min(len(sub(p, s.at)) - s.radius));
        if v <= room {
            return v;
        }
        let ball = self.spots.iter().fold(f64::INFINITY, |r, s| r.min(s.off_centre(p)));
        let slope = self.spots.iter().fold(self.flat, |l, s| l.max(s.slope_near(p, ball)));
        room.max(v - self.jump).max((v / self.step / slope).min(ball)).max(0.0)
    }
    /// Per point, how far all round it is proven to be solid: the most of its depth by the value
    /// and by the source with the texture at its highest (<= 0 where nothing is proven).
    fn solid(&self, pts: &[V]) -> Vec<f64> {
        let n = pts.len();
        let mut depth: Vec<f64> = self.kernel(pts)[n * 2..n * 3].iter().map(|d| *d as f64 * self.step).collect();
        if !self.spots.is_empty() {
            for (d, p) in depth.iter_mut().zip(pts) {
                *d = self.cleared(*p, *d);
            }
        }
        depth
    }
    fn brick(&self, p: V) -> Option<usize> {
        let mut at = [0usize; 3];
        for i in 0..3 {
            let c = ((p[i] - self.lo[i]) / self.bsize).floor();
            if c < 0.0 || c >= self.dim[i] as f64 {
                return None;
            }
            at[i] = c as usize;
        }
        Some((at[2] * self.dim[1] + at[1]) * self.dim[0] + at[0])
    }
    fn brick_box(&self, i: usize) -> (V, V) {
        let (x, y, z) = (i % self.dim[0], (i / self.dim[0]) % self.dim[1], i / (self.dim[0] * self.dim[1]));
        let lo = [self.lo[0] + x as f64 * self.bsize, self.lo[1] + y as f64 * self.bsize, self.lo[2] + z as f64 * self.bsize];
        (lo, add(lo, [self.bsize; 3]))
    }
}

#[derive(Clone, Copy)]
pub struct Tolerance {
    pub(crate) leaf: f64,
    pub(crate) tau: f64,
    /// Sample normals further apart than this (a cosine) split a tile.
    pub(crate) normal_cos: f64,
    /// A tile stands only on a [`Proof`]. A moving view may miss what is smaller than a tile; the
    /// still pass looks at each of its tiles again.
    pub(crate) prove: bool,
}
pub(crate) const MOVING: Tolerance = Tolerance { leaf: 2.0, tau: 1.0, normal_cos: -2.0, prove: false };
/// cos 10 degrees: under it a crease shows across a tile's interpolated normals.
pub(crate) const STILL: Tolerance = Tolerance { leaf: 1.0, tau: 0.25, normal_cos: 0.9848, prove: true };
/// A drag at [`DragDetail::Halfway`]: as small as the still's tiles, as close as half a pixel.
const HALFWAY: Tolerance = Tolerance { leaf: 1.0, tau: 0.5, normal_cos: -2.0, prove: false };

/// How fine each version under the hand is (View > Drag Detail). The renderer never waits for
/// one: the last whole version draws until the next is done, so finer only means less often.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum DragDetail {
    /// Tiles of 2 px while a version takes no longer than [`DRAG_SLOW_MS`], coarser when it would.
    #[default]
    Realtime,
    /// Tiles down to 1 px within half a pixel of the surface, however long that takes.
    Halfway,
    /// What the still shows, however long that takes.
    Full,
}

#[derive(Clone, Copy, Default)]
struct Sample {
    p: V,
    t: f64,
    n: V,
    hit: bool,
    has_n: bool,
    /// The material slot (a field of several), -1 until asked.
    slot: i16,
    /// Its value on a section's face ([`Field::cuts_at`]), a field under a section.
    cut: u16,
    /// The ordinal of its feature ([`Field::ords_at`]), `u16::MAX` until asked: asked for the
    /// sample a tile takes its slot from.
    ord: u16,
}

/// A tile asked for. `fresh`: its cone and samples are still to do (else a tile of the moving
/// tolerance the still one looks at again; `tile`, its record, -1: none).
#[derive(Clone, Copy)]
struct Req {
    x: u32,
    y: u32,
    t0: f64,
    tile: i32,
    fresh: bool,
}

pub(crate) struct Cone {
    pub(crate) o: V,
    pub(crate) d: V,
    pub(crate) t: f64,
    pub(crate) t1: f64,
    pub(crate) r0: f64,
    pub(crate) tan: f64,
    pub(crate) state: u8,
    pub(crate) steps: u32,
}

pub(crate) struct Ray {
    pub(crate) sample: u32,
    pub(crate) o: V,
    pub(crate) d: V,
    pub(crate) t: f64,
    pub(crate) t1: f64,
    pub(crate) prev_t: f64,
    pub(crate) prev_v: f64,
    pub(crate) lo: f64,
    pub(crate) hi: f64,
    pub(crate) lo_v: f64,
    pub(crate) hi_v: f64,
    pub(crate) phase: u8,
    pub(crate) polish: u8,
    pub(crate) best_t: f64,
    pub(crate) best_v: f64,
    pub(crate) steps: u32,
    pub(crate) hit: bool,
    pub(crate) done: bool,
}

struct NormalJob {
    sample: u32,
    p: V,
    n: V,
}

/// A leaf with no gradient of its own (a sculpt's strokes, a kernel with no program) differences
/// its own query this many cells along each partial of its point.
pub(crate) const NORMAL_STEP: f64 = 0.05;

/// What a [`Proof`] leaves room for off a tile's plane, in pixels: less cannot change a pixel's centre.
const PROOF_PX: f64 = 0.5;

/// A tile that looks flat asks the bound whether it is. A value of a 1-Lipschitz field proves a
/// ball about its point all air or all solid. A ball in front of the tile's plane and one behind
/// it, each reaching the plane to within the tolerance all over the tile, leave the surface no room
/// outside that slab: no hole, no bump, no strut in it. Samples agreeing prove nothing of the kind.
struct Proof {
    fit: u32,
    /// The tile's centre, its plane's normal toward the view.
    m: V,
    n: V,
    /// How far from the plane the balls' centres sit (a ball touching the plane at the centre
    /// clears it by half the tolerance at the tile's rim, the rest for the samples' own error).
    h: f64,
    /// From the centre to the furthest corner.
    r: f64,
    tau: f64,
    ok: bool,
}

impl Proof {
    fn proves(&self, ball: f64) -> bool {
        ball > self.r && self.h - (ball * ball - self.r * self.r).sqrt() <= self.tau
    }
}

pub(crate) fn cones_batch(f: &Field, cones: &mut [Cone], stale: &(dyn Fn() -> bool + Sync)) {
    let (mut pts, mut live, mut v, mut b) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    loop {
        if stale() {
            return;
        }
        pts.clear();
        live.clear();
        for (i, c) in cones.iter_mut().enumerate() {
            if c.state != 0 {
                continue;
            }
            let mut guard = 0;
            loop {
                if c.t > c.t1 {
                    c.state = 2;
                    break;
                }
                let p = add(c.o, mul(c.d, c.t));
                guard += 1;
                match f.brick(p) {
                    Some(k) if f.bricks[k] == B_EMPTY && guard < 64 => {
                        let room = f.skip[k] as f64 - (c.r0 + c.tan * c.t);
                        if room > 1e-6 {
                            c.t += room / (1.0 + c.tan);
                            continue;
                        }
                        pts.push(p);
                        live.push(i);
                        break;
                    }
                    Some(k) if f.bricks[k] == B_INSIDE => {
                        c.state = 1;
                        break;
                    }
                    _ => {
                        pts.push(p);
                        live.push(i);
                        break;
                    }
                }
            }
        }
        if live.is_empty() {
            return;
        }
        f.eval(&pts, &mut v, &mut b);
        for (k, &i) in live.iter().enumerate() {
            let c = &mut cones[i];
            c.steps += 1;
            let room = b[k] - (c.r0 + c.tan * c.t);
            if room < f.cell * 0.02 || c.steps > 256 {
                c.state = 1;
                continue;
            }
            c.t += room / (1.0 + c.tan);
        }
    }
}

/// Sample rays in lockstep: a footprint stop (half a pixel), then a few evals to the zero. A
/// sample may overstep by the local slope (a bracket catches it).
pub(crate) fn rays_batch(f: &Field, cam: &Cam, rays: &mut [Ray], stale: &(dyn Fn() -> bool + Sync)) {
    let (mut pts, mut live, mut value, mut bound) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    loop {
        if stale() {
            return;
        }
        pts.clear();
        live.clear();
        for (i, r) in rays.iter_mut().enumerate() {
            if r.done {
                continue;
            }
            // empty bricks are stepped through with no eval
            if r.phase == 0 {
                let mut guard = 0;
                while r.t <= r.t1 && guard < 64 {
                    guard += 1;
                    let p = add(r.o, mul(r.d, r.t));
                    match f.brick(p) {
                        Some(b) if f.bricks[b] == B_EMPTY => {
                            let (lo, hi) = f.brick_box(b);
                            let mut exit = f64::INFINITY;
                            for a in 0..3 {
                                if r.d[a].abs() > 1e-12 {
                                    let face = if r.d[a] > 0.0 { hi[a] } else { lo[a] };
                                    exit = exit.min((face - r.o[a]) / r.d[a]);
                                }
                            }
                            r.t = (exit + 1e-6).max(r.t + 1e-6);
                            r.prev_t = -1.0;
                        }
                        _ => break,
                    }
                }
                if r.t > r.t1 {
                    r.done = true;
                    continue;
                }
            }
            pts.push(add(r.o, mul(r.d, r.t)));
            live.push(i);
        }
        if live.is_empty() {
            return;
        }
        f.eval(&pts, &mut value, &mut bound);
        for (k, &i) in live.iter().enumerate() {
            let r = &mut rays[i];
            let (v, b) = (value[k], bound[k]);
            r.steps += 1;
            let pw = cam.pw(r.t.max(1e-6));
            let foot = (f.cell * 0.02).max(pw * 0.5) * f.slope;
            let close = (f.cell * 0.005).max(pw * HIT_PX) * f.slope;
            if v.abs() < r.best_v {
                r.best_v = v.abs();
                r.best_t = r.t;
            }
            if r.steps > 192 {
                r.hit = true;
                r.done = true;
                r.t = r.best_t;
                continue;
            }
            match r.phase {
                0 => {
                    if v <= 0.0 {
                        if r.prev_t >= 0.0 {
                            r.phase = 1;
                            r.lo = r.prev_t;
                            r.lo_v = r.prev_v;
                            r.hi = r.t;
                            r.hi_v = v;
                            r.t = r.lo + (r.hi - r.lo) * (r.lo_v / (r.lo_v - r.hi_v)).clamp(0.1, 0.9);
                        } else {
                            r.hit = true;
                            r.done = true;
                        }
                    } else if v < foot {
                        if v < close {
                            r.hit = true;
                            r.done = true;
                            continue;
                        }
                        r.phase = 2;
                        r.polish = 0;
                        r.prev_t = r.t;
                        r.prev_v = v;
                        r.t += v / f.slope;
                    } else {
                        // only as far as the value proves: a stride past that goes through a thin
                        // wall and out the other side, and the ray reads as clear for good
                        r.prev_t = r.t;
                        r.prev_v = v;
                        r.t += b.max(foot);
                        if r.t > r.t1 {
                            r.done = true;
                        }
                    }
                }
                1 => {
                    if v.abs() < close || r.hi - r.lo < close / f.slope {
                        r.hit = true;
                        r.done = true;
                        continue;
                    }
                    if v > 0.0 {
                        r.lo = r.t;
                        r.lo_v = v;
                    } else {
                        r.hi = r.t;
                        r.hi_v = v;
                    }
                    r.t = r.lo + (r.hi - r.lo) * (r.lo_v / (r.lo_v - r.hi_v)).clamp(0.1, 0.9);
                }
                _ => {
                    // within the footprint: a few steps to the zero; a value rising again is a
                    // ray grazing by, its nearest point the hit
                    r.polish += 1;
                    if v.abs() < close {
                        r.hit = true;
                        r.done = true;
                        continue;
                    }
                    if v <= 0.0 {
                        r.phase = 1;
                        r.lo = r.prev_t;
                        r.lo_v = r.prev_v;
                        r.hi = r.t;
                        r.hi_v = v;
                        r.t = r.lo + (r.hi - r.lo) * (r.lo_v / (r.lo_v - r.hi_v)).clamp(0.1, 0.9);
                    } else if v >= r.prev_v || r.polish >= 4 {
                        r.t = r.best_t;
                        r.hit = true;
                        r.done = true;
                    } else {
                        r.prev_t = r.t;
                        r.prev_v = v;
                        r.t += v / f.slope;
                    }
                }
            }
        }
    }
}

pub(crate) fn oct_pack(n: V) -> f32 {
    let l1 = n[0].abs() + n[1].abs() + n[2].abs();
    let (mut x, mut y) = (n[0] / l1.max(1e-12), n[1] / l1.max(1e-12));
    if n[2] < 0.0 {
        let (ax, ay) = (x.abs(), y.abs());
        x = (1.0 - ay) * if x >= 0.0 { 1.0 } else { -1.0 };
        y = (1.0 - ax) * if y >= 0.0 { 1.0 } else { -1.0 };
    }
    let q = |v: f64| ((v * 0.5 + 0.5).clamp(0.0, 1.0) * 4095.0).round();
    (q(x) * 4096.0 + q(y)) as f32
}

#[cfg(test)]
pub(crate) fn oct_unpack(v: f32) -> V {
    let v = v as u32;
    let u = |q: u32| q as f64 / 4095.0 * 2.0 - 1.0;
    let (mut x, mut y) = (u(v >> 12), u(v & 4095));
    let z = 1.0 - x.abs() - y.abs();
    if z < 0.0 {
        let (ax, ay) = (x.abs(), y.abs());
        x = (1.0 - ay) * if x >= 0.0 { 1.0 } else { -1.0 };
        y = (1.0 - ax) * if y >= 0.0 { 1.0 } else { -1.0 };
    }
    norm([x, y, z])
}

/// A view's tiles: what is drawn, and what the finer tolerance will look at again.
struct Pass {
    cam: Cam,
    samples: Vec<Sample>,
    tiles: Vec<[f32; TILE_SLOTS]>,
    /// Each tile's size in pixels, for the tests.
    #[cfg(test)]
    sizes: Vec<f32>,
    alive: Vec<bool>,
    /// The tiles standing in for the level running now.
    standing: Vec<u32>,
    deferred: [Vec<Req>; LEVELS],
    moving_done: bool,
}

/// Half-pixel keys to samples, a stamp per view so that nothing clears.
#[derive(Default)]
struct Grid {
    w: usize,
    h: usize,
    stamp: u32,
    cells: Vec<(u32, u32)>,
}

impl Grid {
    fn begin(&mut self, w: f64, h: f64) {
        let across = |v: f64| ((v / BASE as f64).ceil() as usize) * BASE as usize * 2 + 1;
        let (w, h) = (across(w), across(h));
        if (w, h) != (self.w, self.h) {
            self.w = w;
            self.h = h;
            self.stamp = 0;
            self.cells = vec![(0, 0); w * h];
        }
        self.stamp += 1;
    }
    fn get(&self, x: u32, y: u32) -> Option<u32> {
        let (s, i) = self.cells[y as usize * self.w + x as usize];
        (s == self.stamp).then_some(i)
    }
    fn set(&mut self, x: u32, y: u32, i: u32) {
        let w = self.w;
        self.cells[y as usize * w + x as usize] = (self.stamp, i);
    }
}

fn key(q: &Req, k: usize, level: usize) -> (u32, u32) {
    let s2 = (BASE >> level) * 2;
    let (x, y) = (q.x * s2, q.y * s2);
    match k {
        0 => (x, y),
        1 => (x + s2, y),
        2 => (x + s2, y + s2),
        3 => (x, y + s2),
        _ => (x + s2 / 2, y + s2 / 2),
    }
}

struct Fit {
    idx: [u32; 5],
    hits: u32,
    ng: V,
    split: bool,
    normals: bool,
    /// The corner leaves the surface on its way from the centre: a jump in depth between them.
    cut: [bool; 4],
    /// Corners with no centre between them, and a jump among them.
    torn: bool,
}

/// An edge of a tile longer than this many of its footprints is asked whether it keeps to the
/// surface (a face this long is past 70 degrees to the view).
const STRETCH: f64 = 3.0;
/// An edge whose middle sits further than this part of its length off the surface crosses a jump.
const OFF_SURFACE: f64 = 0.1;
/// No edge is kept longer than this many of its footprints (a face past 80 degrees to the view),
/// or this many pixels of the view that traced it: on the surface or not, it would stretch across
/// the next view.
const EDGE_FOOTPRINTS: f64 = 6.0;
const EDGE_PIXELS: f64 = 24.0;

/// An edge asked: the fit, which corner (4 up: corners with no centre, the pair from `corner - 4`),
/// its middle and length, and the value there (infinite: too long to keep, not asked).
struct EdgeJob {
    fit: u32,
    corner: u8,
    mid: V,
    len: f64,
    value: f64,
}

impl Pass {
    fn new(cam: Cam, grid: &mut Grid) -> Pass {
        grid.begin(cam.w, cam.h);
        Pass { cam, samples: Vec::new(), tiles: Vec::new(), #[cfg(test)] sizes: Vec::new(), alive: Vec::new(), standing: Vec::new(), deferred: Default::default(), moving_done: false }
    }

    /// The coarsest cells with a surface brick under them (proven empty otherwise, no eval).
    fn roots(&self, f: &Field) -> Vec<Req> {
        let s = BASE as f64;
        let cam = &self.cam;
        let (nx, ny) = ((cam.w / s).ceil() as usize, (cam.h / s).ceil() as usize);
        let mut mask = vec![false; nx * ny];
        for b in 0..f.bricks.len() {
            if f.bricks[b] != B_SURFACE {
                continue;
            }
            let (lo, hi) = f.brick_box(b);
            let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
            let mut behind = false;
            for k in 0..8 {
                let p = [if k & 1 == 0 { lo[0] } else { hi[0] }, if k & 2 == 0 { lo[1] } else { hi[1] }, if k & 4 == 0 { lo[2] } else { hi[2] }];
                match cam.proj(p) {
                    Some((x, y, _)) => {
                        x0 = x0.min(x);
                        y0 = y0.min(y);
                        x1 = x1.max(x);
                        y1 = y1.max(y);
                    }
                    None => behind = true,
                }
            }
            if behind {
                x0 = 0.0;
                y0 = 0.0;
                x1 = cam.w;
                y1 = cam.h;
            }
            let (a, c) = ((x0 / s).floor().max(0.0) as usize, ((x1 / s).floor() as isize).min(nx as isize - 1));
            let (b0, d) = ((y0 / s).floor().max(0.0) as usize, ((y1 / s).floor() as isize).min(ny as isize - 1));
            if c < 0 || d < 0 {
                continue;
            }
            for y in b0..=d as usize {
                for x in a..=c as usize {
                    mask[y * nx + x] = true;
                }
            }
        }
        let mut out = Vec::new();
        for y in 0..ny {
            for x in 0..nx {
                if mask[y * nx + x] {
                    out.push(Req { x: x as u32, y: y as u32, t0: 0.0, tile: -1, fresh: true });
                }
            }
        }
        out
    }

    /// One tile's record for the draw: a fan around the centre. A corner the rays missed sits on
    /// the centre (its triangles collapse); a centre that missed sits between the corners.
    fn record(&self, fit: &Fit, size: f64, ord: u16) -> Option<[f32; TILE_SLOTS]> {
        let s: [Sample; 5] = std::array::from_fn(|k| self.samples[fit.idx[k] as usize]);
        let mut v: [V; 5] = std::array::from_fn(|k| s[k].p);
        let mut n: [V; 5] = std::array::from_fn(|k| if s[k].has_n && s[k].hit { s[k].n } else { fit.ng });
        let hit: [bool; 4] = std::array::from_fn(|k| s[k].hit);
        if s[4].hit {
            let pw = self.cam.pw(s[4].t.max(1e-6));
            let along = |k: usize, t: f64| {
                let d = if self.cam.ortho { self.cam.f } else { norm(sub(s[k].p, self.cam.eye)) };
                add(s[k].p, mul(d, t - s[k].t))
            };
            // A tile that splits stands for its pieces while they are still to come, or for good
            // in a coarse version: across a jump its samples do not tell which of its pixels see
            // the nearer surface, so it shows that one, at its depth, over all of them. The
            // farther one would show what is behind it.
            let nearer = (0..4).filter(|&k| fit.split && fit.cut[k] && s[k].t < s[4].t).min_by(|a, b| s[*a].t.total_cmp(&s[*b].t));
            if let Some(near) = nearer {
                for k in 0..5 {
                    if s[k].hit && s[k].t > s[near].t {
                        v[k] = along(k, s[near].t);
                    }
                    n[k] = n[near];
                }
            } else {
                // A corner across a jump comes along its own ray to the centre's depth: the same
                // pixels from this view, and from any other a flap of a pixel, no triangle along
                // the ray.
                for k in 0..4 {
                    if fit.cut[k] {
                        v[k] = along(k, s[4].t);
                        n[k] = n[4];
                    }
                }
            }
            if hit.iter().all(|h| *h) {
                // neighbours of other sizes leave no crack: grown by about a pixel, on the screen
                // and no more than two in the world: a grazing face's corner stays near its surface
                if size > 1.0 {
                    let grow = 1.0 / (size * std::f64::consts::FRAC_1_SQRT_2);
                    for k in 0..4 {
                        let e = sub(v[k], v[4]);
                        let l = len(e);
                        v[k] = add(v[k], mul(e, grow.min(2.0 * pw / l.max(1e-30))));
                    }
                }
            } else {
                let any = (0..4).any(|k| hit[k] && hit[(k + 1) % 4]);
                if !any {
                    // a leaf whose centre alone hit: its pixel's square, facing the view
                    let d = pw * size * 0.5;
                    let (r, u) = (mul(self.cam.r, d), mul(self.cam.u, d));
                    v[0] = sub(sub(s[4].p, r), u);
                    v[1] = sub(add(s[4].p, r), u);
                    v[2] = add(add(s[4].p, r), u);
                    v[3] = add(sub(s[4].p, r), u);
                    let centre = n[4];
                    n[..4].fill(centre);
                } else {
                    for k in 0..4 {
                        if !hit[k] {
                            v[k] = v[4];
                            n[k] = n[4];
                        }
                    }
                }
            }
        } else {
            let count = hit.iter().filter(|h| **h).count();
            if count < 3 || fit.torn {
                return None;
            }
            if count == 4 {
                v[4] = mul(add(add(v[0], v[1]), add(v[2], v[3])), 0.25);
            } else {
                let missed = hit.iter().position(|h| !*h).unwrap();
                v[4] = mul(add(v[(missed + 1) % 4], v[(missed + 3) % 4]), 0.5);
                v[missed] = v[4];
            }
        }
        let mut out = [0.0f32; TILE_SLOTS];
        for k in 0..5 {
            out[k * 4] = v[k][0] as f32;
            out[k * 4 + 1] = v[k][1] as f32;
            out[k * 4 + 2] = v[k][2] as f32;
            out[k * 4 + 3] = oct_pack(n[k]);
        }
        let shown = s.iter().rev().find(|a| a.hit);
        out[20] = shown.map_or(0, |a| a.slot.max(0)) as f32;
        let cut = |k: usize| if s[k].hit { s[k].cut } else { s[4].cut };
        out[21] = pack_cut(cut(0), cut(1));
        out[22] = pack_cut(cut(2), cut(3));
        // its feature: the sample its slot is from (`ord`: the part's one, or none)
        out[23] = shown.map_or(ord, |a| if a.ord == u16::MAX { ord } else { a.ord }) as f32;
        Some(out)
    }

    /// One level's tiles, in batches; what they split into. None: the shape moved on.
    fn level(&mut self, f: &Field, pool: &TaskPool, grid: &mut Grid, level: usize, reqs: &[Req], tol: Tolerance, stale: &(dyn Fn() -> bool + Sync)) -> Option<Vec<Req>> {
        let cam = self.cam;
        let size = (BASE >> level) as f64;
        let far = len(sub(mul(add(f.lo, f.hi), 0.5), cam.eye)) + len(sub(f.hi, f.lo)) * 0.5;
        let mut cones: Vec<Cone> = reqs
            .iter()
            .map(|q| {
                let (o, d, r0, tan) = cam.cone(q.x as f64 * size, q.y as f64 * size, size);
                if !q.fresh {
                    return Cone { o, d, t: q.t0, t1: 0.0, r0, tan, state: 1, steps: 0 };
                }
                let g = [r0 + tan * far; 3];
                match slab(o, d, sub(f.lo, g), add(f.hi, g)) {
                    Some((a, b)) => Cone { o, d, t: q.t0.max(a), t1: b, r0, tan, state: 0, steps: 0 },
                    None => Cone { o, d, t: 0.0, t1: 0.0, r0, tan, state: 2, steps: 0 },
                }
            })
            .collect();
        par(pool, &mut cones, BATCH, |c| cones_batch(f, c, stale));
        if stale() {
            return None;
        }
        // the samples they need
        let mut rays: Vec<Ray> = Vec::new();
        for (i, q) in reqs.iter().enumerate() {
            if cones[i].state != 1 {
                continue;
            }
            for k in 0..5 {
                let (kx, ky) = key(q, k, level);
                if grid.get(kx, ky).is_none() {
                    let sample = self.samples.len() as u32;
                    self.samples.push(Sample::default());
                    grid.set(kx, ky, sample);
                    let (o, d) = cam.ray(kx as f64 * 0.5, ky as f64 * 0.5);
                    let t1 = slab(o, d, f.lo, f.hi).map_or(-1.0, |(_, b)| b);
                    let t = cones[i].t;
                    rays.push(Ray { sample, o, d, t, t1, prev_t: -1.0, prev_v: 0.0, lo: 0.0, hi: 0.0, lo_v: 0.0, hi_v: 0.0, phase: 0, polish: 0, best_t: t, best_v: f64::INFINITY, steps: 0, hit: false, done: false });
                }
            }
        }
        par(pool, &mut rays, BATCH, |c| rays_batch(f, &cam, c, stale));
        if stale() {
            return None;
        }
        for r in &rays {
            self.samples[r.sample as usize] = Sample { p: add(r.o, mul(r.d, r.t)), t: r.t, n: [0.0; 3], hit: r.hit, has_n: false, slot: -1, cut: 0, ord: u16::MAX };
        }
        // the fit, geometry first
        let mut fits: Vec<Option<Fit>> = Vec::with_capacity(reqs.len());
        let mut jobs: Vec<NormalJob> = Vec::new();
        for (i, q) in reqs.iter().enumerate() {
            if cones[i].state != 1 {
                fits.push(None);
                continue;
            }
            let idx: [u32; 5] = std::array::from_fn(|k| { let (x, y) = key(q, k, level); grid.get(x, y).unwrap() });
            let s: [Sample; 5] = std::array::from_fn(|k| self.samples[idx[k] as usize]);
            let hits = s.iter().filter(|x| x.hit).count() as u32;
            let leaf = (BASE >> level) as f64 <= tol.leaf;
            let (mut e, mut ng) = (f64::INFINITY, cam.f);
            if hits == 5 {
                let [a, b, c, d, m] = [s[0].p, s[1].p, s[2].p, s[3].p, s[4].p];
                let avg = mul(add(add(a, b), add(c, d)), 0.25);
                ng = norm(cross(sub(c, a), sub(d, b)));
                let pw = cam.pw(s[4].t.max(1e-6));
                e = dot(sub(m, avg), ng).abs().max(dot(sub(a, avg), ng).abs()) / pw;
            } else if hits >= 3 {
                let hit: Vec<V> = s.iter().filter(|x| x.hit).map(|x| x.p).collect();
                let c = cross(sub(hit[1], hit[0]), sub(hit[2], hit[0]));
                if len(c) > 1e-18 {
                    ng = norm(c);
                }
                if dot(ng, cam.f) > 0.0 {
                    ng = mul(ng, -1.0);
                }
            } else {
                ng = mul(cam.f, -1.0);
            }
            let mut split = !leaf && !(hits == 5 && e < tol.tau);
            // a strut in front of a flat-looking tile stopped the cone early
            if !split && !leaf && hits == 5 {
                let c = &cones[i];
                let r = c.r0 + c.tan * c.t;
                let near = s.iter().map(|x| x.t).fold(f64::INFINITY, f64::min);
                let margin = r * (1.0 / f.slope) / dot(ng, c.d).abs().max(0.2);
                if near - c.t > 1.5 * margin + f.cell * 0.05 {
                    split = true;
                }
            }
            // a face this grazing is drawn in shorter tiles
            if !split && !leaf && hits == 5 {
                let limit = (EDGE_FOOTPRINTS * size * std::f64::consts::FRAC_1_SQRT_2).min(EDGE_PIXELS) * cam.pw(s[4].t.max(1e-6));
                split = (0..4).any(|k| len(sub(s[k].p, s[4].p)) > limit);
            }
            let normals = !split && hits >= 1;
            if normals {
                for k in 0..5 {
                    let sample = &mut self.samples[idx[k] as usize];
                    if sample.hit && !sample.has_n {
                        sample.has_n = true;
                        jobs.push(NormalJob { sample: idx[k], p: sample.p, n: [0.0; 3] });
                    }
                }
            }
            fits.push(Some(Fit { idx, hits, ng, split, normals, cut: [false; 4], torn: false }));
        }
        // normals: the gradient, once per sample
        par(pool, &mut jobs, BATCH, |c| {
            if stale() {
                return;
            }
            let pts: Vec<V> = c.iter().map(|j| j.p).collect();
            let (_, g) = f.gradient(&pts);
            for (j, g) in c.iter_mut().zip(g) {
                j.n = norm(g);
            }
        });
        if stale() {
            return None;
        }
        for j in &jobs {
            self.samples[j.sample as usize].n = j.n;
        }
        // several materials, or a section: what each sample a tile will show is
        if f.slots || f.sides.is_some() {
            let mut looks: Vec<(u32, V, V, u8, u16)> = Vec::new();
            for fit in fits.iter().flatten() {
                for k in fit.idx {
                    let sample = &mut self.samples[k as usize];
                    if sample.hit && sample.slot < 0 {
                        sample.slot = 0;
                        looks.push((k, sample.p, if sample.has_n { sample.n } else { fit.ng }, 0, 0));
                    }
                }
            }
            par(pool, &mut looks, BATCH / 4, |c| {
                if stale() {
                    return;
                }
                if f.slots {
                    let under: Vec<V> = c.iter().map(|j| sub(j.1, mul(j.2, f.cell * 0.25))).collect();
                    for (j, s) in c.iter_mut().zip(f.slots_at(&under)) {
                        j.3 = s;
                    }
                }
                if f.sides.is_some() {
                    let at: Vec<V> = c.iter().map(|j| j.1).collect();
                    for (j, v) in c.iter_mut().zip(f.cuts_at(&at)) {
                        j.4 = v;
                    }
                }
            });
            if stale() {
                return None;
            }
            for j in &looks {
                let sample = &mut self.samples[j.0 as usize];
                sample.slot = j.3 as i16;
                sample.cut = j.4;
            }
        }
        // a part of several features: the one each tile to record shows, from the sample its slot is
        if f.has_ids() {
            let mut looks: Vec<(u32, V, u16)> = Vec::new();
            for (fit, q) in fits.iter().zip(reqs) {
                let Some(fit) = fit.as_ref().filter(|fit| q.fresh && fit.hits >= 1) else { continue };
                let Some(k) = fit.idx.iter().rev().copied().find(|k| self.samples[*k as usize].hit) else { continue };
                let sample = &mut self.samples[k as usize];
                if sample.ord == u16::MAX {
                    sample.ord = 0;
                    looks.push((k, sample.p, 0));
                }
            }
            par(pool, &mut looks, BATCH / 4, |c| {
                if stale() {
                    return;
                }
                let at: Vec<V> = c.iter().map(|j| j.1).collect();
                for (j, o) in c.iter_mut().zip(f.ords_at(&at)) {
                    j.2 = o;
                }
            });
            if stale() {
                return None;
            }
            for j in &looks {
                self.samples[j.0 as usize].ord = j.2;
            }
        }
        // what looks flat: normals, material, then the proof
        let mut proofs: Vec<Proof> = Vec::new();
        for (i, fit) in fits.iter_mut().enumerate() {
            let Some(fit) = fit.as_mut().filter(|fit| fit.normals && fit.hits == 5 && (BASE >> level) as f64 > tol.leaf) else { continue };
            let s: [Sample; 5] = std::array::from_fn(|k| self.samples[fit.idx[k] as usize]);
            let mut delta = 0.02f64;
            let mut worst = 1.0f64;
            for a in 0..5 {
                delta = delta.max(len(cross(s[a].n, fit.ng)));
                for b in a + 1..5 {
                    worst = worst.min(dot(s[a].n, s[b].n));
                }
            }
            // a flat-looking tile whose normals disagree: its samples missed a ridge
            if ((BASE >> level) as f64 > 2.0 * tol.leaf && delta > 0.42) || worst < tol.normal_cos {
                fit.split = true;
            }
            // a tile has one material
            if f.slots && s.iter().any(|a| a.slot != s[0].slot) {
                fit.split = true;
            }
            if !fit.split && tol.prove {
                let m = s[4].p;
                let away = if cam.ortho { cam.f } else { sub(m, cam.eye) };
                let n = if dot(fit.ng, away) > 0.0 { mul(fit.ng, -1.0) } else { fit.ng };
                let r = (0..4).map(|k| len(sub(s[k].p, m))).fold(0.0, f64::max);
                let tau = PROOF_PX * cam.pw(s[4].t.max(1e-6));
                let h = r * r / tau;
                // no bound there, nothing proven: down to the leaf, a ray for each pixel
                if len(m) + h < f.proven {
                    proofs.push(Proof { fit: i as u32, m, n, h, r, tau, ok: false });
                } else {
                    fit.split = true;
                }
            }
        }
        par(pool, &mut proofs, BATCH / 2, |c| {
            if stale() {
                return;
            }
            let front: Vec<V> = c.iter().map(|p| add(p.m, mul(p.n, p.h))).collect();
            let back: Vec<V> = c.iter().map(|p| sub(p.m, mul(p.n, p.h))).collect();
            let (mut v, mut air) = (Vec::new(), Vec::new());
            f.eval(&front, &mut v, &mut air);
            let solid = f.solid(&back);
            for (k, p) in c.iter_mut().enumerate() {
                p.ok = p.proves(air[k] * f.near(front[k], p.h + p.tau)) && p.proves(solid[k] * f.near(back[k], p.h + p.tau));
            }
        });
        if stale() {
            return None;
        }
        for p in &proofs {
            if !p.ok {
                if let Some(fit) = fits[p.fit as usize].as_mut() {
                    fit.split = true;
                }
            }
        }
        // the tiles' long edges: on the surface, or across a jump
        let mut edges: Vec<EdgeJob> = Vec::new();
        for (i, q) in reqs.iter().enumerate() {
            let Some(fit) = fits[i].as_ref().filter(|fit| q.fresh && (fit.hits >= 3 || (!fit.split && fit.hits >= 1))) else { continue };
            let s: [Sample; 5] = std::array::from_fn(|k| self.samples[fit.idx[k] as usize]);
            let t = if s[4].hit { s[4].t } else { s.iter().filter(|x| x.hit).map(|x| x.t).fold(f64::INFINITY, f64::min) };
            let pw = cam.pw(t.max(1e-6));
            let foot = pw * size * std::f64::consts::FRAC_1_SQRT_2;
            let limit = (EDGE_FOOTPRINTS * foot).min(EDGE_PIXELS * pw);
            // a tile that splits may stand for its pieces in a coarse version: each of its edges
            // is asked, so that one across a jump shows the nearer side ([`Pass::record`])
            let split = fit.split;
            let mut ask = |corner: u8, a: V, b: V| {
                let l = len(sub(a, b));
                if l > STRETCH * foot || split {
                    edges.push(EdgeJob { fit: i as u32, corner, mid: mul(add(a, b), 0.5), len: l, value: if l > limit { f64::INFINITY } else { 0.0 } });
                }
            };
            for k in 0..4 {
                if s[4].hit && s[k].hit {
                    ask(k as u8, s[k].p, s[4].p);
                } else if !s[4].hit && s[k].hit && s[(k + 1) % 4].hit {
                    ask(k as u8 + 4, s[k].p, s[(k + 1) % 4].p);
                }
            }
        }
        par(pool, &mut edges, BATCH / 4, |c| {
            if stale() {
                return;
            }
            let pts: Vec<V> = c.iter().map(|j| j.mid).collect();
            for (j, v) in c.iter_mut().zip(f.value(&pts)) {
                if j.value.is_finite() {
                    j.value = v;
                }
            }
        });
        if stale() {
            return None;
        }
        for e in &edges {
            if (e.value * f.step / f.slope).abs() > OFF_SURFACE * e.len {
                if let Some(fit) = fits[e.fit as usize].as_mut() {
                    if e.corner < 4 {
                        fit.cut[e.corner as usize] = true;
                    } else {
                        fit.torn = true;
                    }
                }
            }
        }
        // tiles and what they split into
        let mut next = Vec::new();
        let mut standing = Vec::new();
        for (i, q) in reqs.iter().enumerate() {
            let Some(fit) = fits[i].as_mut() else { continue };
            let mut tile = q.tile;
            if q.fresh && (fit.hits >= 3 || (!fit.split && fit.hits >= 1)) {
                if let Some(rec) = self.record(fit, size, f.ids.constant) {
                    self.tiles.push(rec);
                    #[cfg(test)]
                    self.sizes.push(size as f32);
                    self.alive.push(true);
                    tile = self.tiles.len() as i32 - 1;
                }
            }
            let t0 = cones[i].t;
            if fit.split {
                if tile >= 0 {
                    standing.push(tile as u32);
                }
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    next.push(Req { x: q.x * 2 + dx, y: q.y * 2 + dy, t0, tile: -1, fresh: true });
                }
            } else if !self.moving_done && (BASE >> level) as f64 > STILL.leaf {
                self.deferred[level].push(Req { x: q.x, y: q.y, t0, tile, fresh: false });
            }
        }
        // the tiles that stood in for this level retire: their children stand now
        for t in std::mem::replace(&mut self.standing, standing) {
            self.alive[t as usize] = false;
        }
        Some(next)
    }

    /// A version's tiles, its levels down to `levels`: the tiles that would split further stand
    /// for their pieces. None: `stale` stopped it.
    fn version(&mut self, f: &Field, pool: &TaskPool, grid: &mut Grid, levels: usize, tol: Tolerance, stale: &(dyn Fn() -> bool + Sync)) -> Option<()> {
        let mut queue = self.roots(f);
        let mut level = 0;
        while !queue.is_empty() && level < levels {
            queue = self.level(f, pool, grid, level, &queue, tol, stale)?;
            level += 1;
        }
        Some(())
    }

    /// The still's second look at what the moving levels left. None: `stale` stopped it.
    fn finish_still(&mut self, f: &Field, pool: &TaskPool, grid: &mut Grid, stale: &(dyn Fn() -> bool + Sync)) -> Option<()> {
        let mut queue: Vec<Req> = Vec::new();
        for level in 0..LEVELS {
            queue.append(&mut self.deferred[level]);
            if queue.is_empty() {
                for t in std::mem::take(&mut self.standing) {
                    self.alive[t as usize] = false;
                }
                continue;
            }
            queue = self.level(f, pool, grid, level, &queue, STILL, stale)?;
        }
        for t in std::mem::take(&mut self.standing) {
            self.alive[t as usize] = false;
        }
        Some(())
    }

    fn publish(&self, shared: &Shared, key: [u8; 32]) {
        let count = self.alive.iter().filter(|a| **a).count();
        let mut data = Vec::with_capacity(count * TILE_SLOTS);
        for (t, a) in self.tiles.iter().zip(&self.alive) {
            if *a {
                data.extend_from_slice(t);
            }
        }
        let quads = QuadBox::of(&data);
        let inst = if data.is_empty() { None } else { RetainedInstances::new(TILE_SLOTS, Arc::from(data)).ok() }.map(|tiles| Chunk { tiles, quads });
        shared.send(vec![(0, inst)], key, false);
    }
}

/// Each pass logs what it took (`FAB_SDF_PROF`, or `FAB_SDF_LOG` for these lines without the GPU
/// recording, whose split frames cost the frames being measured).
pub static LOG_PASSES: AtomicBool = AtomicBool::new(false);

/// What the worker keeps between its runs.
#[derive(Default)]
struct Kept {
    key: Option<[u8; 32]>,
    field: Option<Arc<Field>>,
    grid: Grid,
    pass: Option<Pass>,
    surface: Option<Surface>,
    /// The shape `surface` is of.
    surface_of: Option<[u8; 32]>,
    /// The mesh has taken over from the view tiles.
    mesh_shown: bool,
    /// Where its first look is from.
    look: Option<Cam>,
    /// How many levels of tiles a drag's next version goes to; 0: not set yet.
    drag_levels: usize,
    /// What the versions at that size have taken.
    drag_ms: Vec<f64>,
    gaze: Gaze,
    /// A headset's: each eye's own look at what is there, the eye whose turn it is, and the view of
    /// each that has been looked at to its end.
    eye_gazes: [Gaze; 2],
    eye: usize,
    seen: [Option<(Cam, u64)>; 2],
    /// The view and fineness of a moving look not done yet.
    held: Option<(Cam, bool)>,
    /// When the shape began, until its first version goes out; where the view began: evals, time.
    fresh: Option<(u64, Instant)>,
    /// When work on this shape began; what its mesh has to show, until it holds anything.
    unsent: Vec<(usize, Option<Chunk>)>,
    began: Option<(Cam, u64, u64, Instant, (u64, f64))>,
    /// The scan under way, while it has views left.
    scan: Option<scan::Scan>,
}

struct Want {
    cam: Option<Cam>,
    /// A headset: `cam` is the view from between these two.
    eyes: Option<[Cam; 2]>,
    key: [u8; 32],
    shape: Option<Arc<SdfShape>>,
    still: bool,
    running: bool,
    cancelled: bool,
    failed: bool,
    converged: bool,
    /// How far the still look at the view the camera is at has come ([`Gaze::share`]): 0 while
    /// a moving look holds it, 1 once it is done.
    traced: f32,
    /// The shape has rested a while.
    quiet: bool,
    kept: Option<Kept>,
    /// The part's up, in its own frame: what its scan keeps above.
    up: V,
    /// The way to the light, in its own frame: its scan looks from there first.
    to_light: Option<V>,
    /// The part is drawn, so worth a scan; and selected, so first on the queue.
    scannable: bool,
    selected: bool,
    /// A round of its scan holds `kept`; what the view wants meanwhile is owed when it ends.
    scanning: bool,
    /// What the view wants has no worker: a scan round held the part when the view asked, or the
    /// mesh's worker stopped short of it for an edit. The round's end, or the view's next look,
    /// starts it.
    owed: bool,
    /// Its scan has been all the way round since the shape last changed.
    scan_done: bool,
    /// The worker's round that waits for a worker, by its number, and its class: a view that
    /// wants it at a higher class meanwhile queues it again there, and the one left in the queue
    /// ends as it begins ([`Shared::queue_round`]).
    queued: Option<(u64, Class)>,
    /// The rounds queued so far: their numbers.
    rounds: u64,
}

/// What the worker hands the UI: the tiles to draw, the shape they are of, whether the view is done.
struct Out {
    chunks: Vec<(usize, Option<Chunk>)>,
    key: [u8; 32],
    settled: bool,
    /// These are the one mesh, whole for the first time: everything else on screen goes.
    replace: bool,
}

impl Shared {
    fn drag_detail(&self) -> DragDetail {
        match self.detail.load(Ordering::Relaxed) {
            1 => DragDetail::Halfway,
            2 => DragDetail::Full,
            _ => DragDetail::Realtime,
        }
    }

    /// The field the workers trace from now on, and its feature table.
    fn set_field(&self, key: [u8; 32], f: &Arc<Field>) {
        *self.field.lock().unwrap() = Some((key, f.clone()));
        let mut tables = self.tables.lock().unwrap();
        tables.retain(|(k, _)| *k != key);
        if tables.len() >= 4 {
            tables.remove(0);
        }
        tables.push((key, f.id_table()));
    }

    /// Hands the UI what changed, on top of what it has not taken yet.
    fn send(&self, chunks: Vec<(usize, Option<Chunk>)>, key: [u8; 32], settled: bool) {
        {
            let mut out = self.out.lock().unwrap();
            match out.as_mut().filter(|o| o.key == key) {
                Some(o) => {
                    o.chunks.extend(chunks);
                    o.settled |= settled;
                }
                None => *out = Some(Out { chunks, key, settled, replace: false }),
            }
        }
        self.wake_ui();
    }

    /// The one mesh takes over from what is on screen, all at once.
    fn hand_over(&self, chunks: Vec<(usize, Option<Chunk>)>, key: [u8; 32]) {
        *self.out.lock().unwrap() = Some(Out { chunks, key, settled: false, replace: true });
        self.wake_ui();
    }

    /// The view is to look again ([`Tiles::take_woke`]): there is something to draw, or the work
    /// it started cannot go on without it. The UI hears of it as a signal.
    fn wake_ui(&self) {
        self.woke.store(true, Ordering::Release);
        SignalToUI::set_ui_signal();
    }

    /// Runs the part's worker: the one mesh's, or (`mesh` false) the drag's. A worker does round
    /// after round of what the view wants, and goes back in the queue when other work waits for a
    /// worker ([`Shared::goes_on`]); when it stops, the view looks again only if it wants the other worker now, or to hear that the
    /// shape cannot be traced. Only what moves under the hand ranks with what a person asks for:
    /// refining a view at rest, or following a head that never rests, works on what is on screen
    /// already, and gives way to all of that.
    fn start(self: &Arc<Self>, pool: &TaskPool, mesh: bool) -> Result<TaskHandle<()>, ()> {
        let (round, class) = self.queue_round(&mut self.want.lock().unwrap(), mesh);
        self.submit_round(pool, mesh, round, class)
    }

    /// The class of a round of the worker (`mesh`, or the drag's) as the view wants it now.
    fn class_of(w: &Want, mesh: bool) -> Class {
        if mesh && (w.eyes.is_some() || w.still) {
            Class::Visible
        } else {
            Class::Interactive
        }
    }

    /// The next round's number and class, as the one waiting for a worker: whatever was waiting
    /// before it does nothing when it comes up. Under the want's lock, so that a round that has
    /// begun is never queued again beside itself.
    fn queue_round(&self, w: &mut Want, mesh: bool) -> (u64, Class) {
        w.rounds += 1;
        let class = Self::class_of(w, mesh);
        w.queued = Some((w.rounds, class));
        (w.rounds, class)
    }

    fn submit_round(self: &Arc<Self>, pool: &TaskPool, mesh: bool, round: u64, class: Class) -> Result<TaskHandle<()>, ()> {
        let (shared, worker_pool) = (self.clone(), pool.clone());
        let job: fn(Arc<Shared>, TaskPool) -> Then = if mesh { work_surface } else { work };
        pool.submit_named(class, ROUND, move || {
            {
                let mut w = shared.want.lock().unwrap();
                if w.queued.is_none_or(|(r, _)| r != round) {
                    return;
                }
                w.queued = None;
            }
            loop {
                let then = job(shared.clone(), worker_pool.clone());
                if then == Then::Stop {
                    break;
                }
                if !shared.goes_on(&worker_pool, mesh, class, then == Then::Look) {
                    shared.again(&worker_pool, mesh);
                    break;
                }
            }
            let owed = {
                let w = shared.want.lock().unwrap();
                w.failed || (!w.cancelled && (w.quiet != mesh || w.owed))
            };
            if owed {
                shared.wake_ui();
            }
        })
        .map_err(|_| ())
    }

    /// After a round that wants another (a look that never rests: a camera that keeps moving, a
    /// body Mechanics swings, a head), whether its worker goes on with it: while the view wants it
    /// at the class it runs at, and no job waits for a worker of that class or a higher one. Within
    /// a look the other parts' rounds count as nothing: a wait between a look's rounds is a wait
    /// inside every look, and handed over round by round, a headset's looks over 30 parts took
    /// three times as long. Once a look is out (`look_ended`) they count like any job: a wait
    /// between looks is each part's share of the workers, where counted as nothing, the parts
    /// beyond the workers' count got no look for as long as the others' looks went on. Its permit
    /// then gives way to a higher class's or the JIT's ([`checkpoint`]). Why it leaves at all: on
    /// a pool of one worker (a test given two cores) a swinging vase's looks held it for 160 s
    /// while a plate load waited.
    fn goes_on(&self, pool: &TaskPool, mesh: bool, class: Class, look_ended: bool) -> bool {
        Self::class_of(&self.want.lock().unwrap(), mesh) == class
            && pool.waiting(class, if look_ended { ANY_JOB } else { ROUND }) == 0
            && checkpoint().is_ok()
    }

    /// The worker's next round, queued behind whatever waits for a worker of its class or a
    /// higher one ([`Shared::goes_on`]). A round is a look's step, or a drag's version.
    fn again(self: &Arc<Self>, pool: &TaskPool, mesh: bool) {
        if self.start(pool, mesh).is_err() {
            let mut w = self.want.lock().unwrap();
            w.running = false;
            w.failed = true;
        }
    }
}

/// The label a part's rounds are submitted under ([`Shared::goes_on`]).
const ROUND: &str = "sdf tiles";
/// No job's label ([`TaskPool::waiting`] counts every job besides this one): an unnamed job is
/// labelled with its closure's type name.
const ANY_JOB: &str = "";

/// What a part's round leaves its worker to do ([`work`], [`work_surface`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Then {
    /// Nothing: the view has what it wants, or there is nothing to look at.
    Stop,
    /// The rest of the look the round was a step of.
    Round,
    /// Another look: the one the round ended is out.
    Look,
}

struct Shared {
    want: Mutex<Want>,
    out: Mutex<Option<Out>>,
    /// The view is to look again ([`Shared::wake_ui`]).
    woke: AtomicBool,
    /// Moves with the shape: work on the shape before drops at its next batch.
    version: AtomicU64,
    /// The field's step (the bits of an f64): what a walk along a ray multiplies its value by.
    step: AtomicU64,
    /// The field the workers trace, and whose shape it is.
    field: Mutex<Option<([u8; 32], Arc<Field>)>>,
    /// The last shapes' feature tables ([`Field::id_table`]), newest last: what is on screen
    /// resolves its ordinals against its own, also while a newer shape is traced.
    tables: Mutex<Vec<([u8; 32], Arc<[u32]>)>>,
    /// [`DragDetail`], as its discriminant.
    detail: AtomicU8,
    /// The queue its scan is on.
    queue: Option<Arc<scan::Queue>>,
}

/// A drag's worker: until the part has rested, each version under the hand is view tiles, whole by
/// construction. The surface's worker takes over from it. Each round is a whole version: a look.
fn work(shared: Arc<Shared>, pool: TaskPool) -> Then {
    // what is wanted, or nothing and stop
    let (cam, key, shape, version) = {
        let mut w = shared.want.lock().unwrap();
        let Some(cam) = w.cam.filter(|_| !w.cancelled) else {
            w.running = false;
            return Then::Stop;
        };
        (cam, w.key, w.shape.clone(), shared.version.load(Ordering::Acquire))
    };
    let mut kept = shared.want.lock().unwrap().kept.take().unwrap_or_default();
    let Some(shape) = shape else { shared.want.lock().unwrap().running = false; return Then::Stop };
    // a new shape: its field, and the view from nothing
    let field_began = Instant::now();
    if kept.key != Some(key) {
        kept.surface = None;
        kept.surface_of = None;
        match Field::new(&shape.root, shape.materials.len() > 1, &pool) {
            Some(f) => {
                shared.step.store(f.step.to_bits(), Ordering::Relaxed);
                let f = Arc::new(f);
                shared.set_field(key, &f);
                kept.field = Some(f);
                kept.key = Some(key);
                kept.pass = None;
            }
            None => {
                let mut w = shared.want.lock().unwrap();
                w.failed = true;
                w.running = false;
                return Then::Stop;
            }
        }
    }
    let field = kept.field.clone().unwrap();
    let field_ms = field_began.elapsed().as_secs_f64() * 1000.0;
    let same = kept.pass.as_ref().is_some_and(|p| p.cam == cam);
    if !same {
        kept.pass = Some(Pass::new(cam, &mut kept.grid));
    }
    let pass = kept.pass.as_mut().unwrap();
    let newer = || shared.version.load(Ordering::Relaxed) != version;
    // a level under way finishes for a newer shape: a drag faster than a whole trace still
    // puts each trace it starts on the glass
    let detail = shared.drag_detail();
    // a version that may take seconds gives way to the mesh as soon as the part has rested
    let cancelled = || {
        let w = shared.want.lock().unwrap();
        (newer() && w.cancelled) || (detail != DragDetail::Realtime && w.quiet)
    };
    let mut dropped = false;
    let began = Instant::now();
    let ids0 = field.ids.spent();
    // Before a part draws as one mesh, each version under the hand is these tiles down to one
    // size: whole, and as fine as its cost leaves it often enough.
    let (levels, tol) = match detail {
        DragDetail::Realtime if kept.drag_levels == 0 => (DRAG_LEVELS - 1, MOVING),
        DragDetail::Realtime => (kept.drag_levels, MOVING),
        DragDetail::Halfway => (LEVELS, HALFWAY),
        DragDetail::Full => (LEVELS, MOVING),
    };
    if !pass.moving_done {
        dropped = pass.version(&field, &pool, &mut kept.grid, levels, tol, &cancelled).is_none();
        if !dropped && detail == DragDetail::Full {
            dropped = pass.finish_still(&field, &pool, &mut kept.grid, &cancelled).is_none();
        }
        if !dropped {
            pass.moving_done = true;
            pass.publish(&shared, key);
            let ms = field_ms + began.elapsed().as_secs_f64() * 1000.0;
            // the quickest of a few is the real cost, whatever else the machine was doing;
            // a detail that changes every version would flicker
            if detail == DragDetail::Realtime {
                kept.drag_levels = levels;
                kept.drag_ms.push(ms);
            }
            if detail == DragDetail::Realtime && (kept.drag_ms.len() == DRAG_VERSIONS || ms > 4.0 * DRAG_SLOW_MS) {
                let mut cost = kept.drag_ms.iter().fold(f64::MAX, |a, b| a.min(*b));
                kept.drag_ms.clear();
                if cost * DRAG_LEVEL_COST < DRAG_SLOW_MS && levels < DRAG_LEVELS {
                    kept.drag_levels = levels + 1;
                }
                while cost > DRAG_SLOW_MS && kept.drag_levels > 1 {
                    kept.drag_levels -= 1;
                    cost /= DRAG_LEVEL_COST;
                }
            }
            if LOG_PASSES.load(Ordering::Relaxed) {
                let ids = field.ids.spent();
                makepad_draw::makepad_platform::log!("sdf tiles version: {ms:.1} ms, tiles to {} px, {} tiles; ids {} in {:.2} ms", BASE >> (levels - 1), pass.alive.iter().filter(|a| **a).count(), ids.0 - ids0.0, ids.1 - ids0.1);
            }
        }
    }
    if dropped {
        kept.pass = None;
    }
    // the view as wanted now: done, or round again
    let mut w = shared.want.lock().unwrap();
    let moving_done = kept.pass.as_ref().is_some_and(|p| p.moving_done);
    let current = kept.key == Some(w.key) && kept.pass.as_ref().is_some_and(|p| Some(p.cam) == w.cam);
    let done = w.cancelled || w.quiet || (current && moving_done);
    w.converged = false;
    w.kept = Some(kept);
    if done {
        w.running = false;
        return Then::Stop;
    }
    // each round is a whole version
    Then::Look
}

/// The surface's worker: gathers what the rays of the view wanted now find, publishes what changed.
/// A round is one step of a look; the look is out at the step that is done.
fn work_surface(shared: Arc<Shared>, pool: TaskPool) -> Then {
    let (cam, eyes, key, shape, still, version) = {
        let mut w = shared.want.lock().unwrap();
        let Some(cam) = w.cam.filter(|_| !w.cancelled && w.quiet) else {
            // an edit: the drag's worker is wanted now, or the mesh again once the edit is
            // quiet, which the view cannot tell from the mesh it started
            w.owed = !w.cancelled;
            w.running = false;
            return Then::Stop;
        };
        (cam, w.eyes, w.key, w.shape.clone(), w.still, shared.version.load(Ordering::Acquire))
    };
    let mut kept = shared.want.lock().unwrap().kept.take().unwrap_or_default();
    let Some(shape) = shape else { shared.want.lock().unwrap().running = false; return Then::Stop };
    // a newer shape stops it at once: the tiles carry the drag
    let newer = || shared.version.load(Ordering::Relaxed) != version;
    // a shape edit starts the part over: nothing is carried across it
    if kept.key != Some(key) {
        let Some(field) = Field::new(&shape.root, shape.materials.len() > 1, &pool) else {
            let mut w = shared.want.lock().unwrap();
            w.failed = true;
            w.running = false;
            return Then::Stop;
        };
        shared.step.store(field.step.to_bits(), Ordering::Relaxed);
        let field = Arc::new(field);
        shared.set_field(key, &field);
        kept.field = Some(field);
        kept.key = Some(key);
        kept.pass = None;
    }
    let field = kept.field.clone().unwrap();
    if kept.surface_of != Some(key) {
        kept.surface = Some(Surface::new(&field));
        kept.surface_of = Some(key);
        kept.gaze = Gaze::default();
        kept.eye_gazes = Default::default();
        kept.eye = 0;
        kept.seen = [None; 2];
        kept.held = None;
        kept.fresh = Some((0, Instant::now()));
        kept.unsent.clear();
        kept.began = None;
        kept.mesh_shown = false;
        kept.look = None;
        kept.scan = None;
    }
    // Its first look holds still, whatever the camera does: what it shows has to be whole
    // from somewhere. It takes over as soon as that look is done, at the detail of a moving
    // camera: full detail then refines it where it stands, as after a drag. A headset's full
    // detail is its eyes' to look for, and its view from between them can be a lot wider
    // than either.
    let (cam, still) = match (kept.mesh_shown, kept.look) {
        (false, Some(first)) => (first, false),
        (false, None) => {
            kept.look = Some(cam);
            (cam, false)
        }
        (true, _) => (cam, still),
    };
    // in a headset the look from between the eyes takes over whole; after it each eye in turn
    // finishes what only it sees
    let on_eyes = eyes.filter(|_| kept.mesh_shown);
    let wanted = on_eyes.map_or(cam, |e| e[kept.eye]);
    // a moving look keeps its view until it is done: the mesh on screen stays the last that
    // was, however long that takes
    let (looking, fine) = kept.held.unwrap_or((wanted, still));
    let of_now = looking == wanted && fine == still;
    let tol = if fine { STILL } else { MOVING };
    if kept.began.is_none_or(|b| b.0 != looking || b.1 != tol.leaf.to_bits()) {
        kept.began = Some((looking, tol.leaf.to_bits(), field.evals.load(Ordering::Relaxed), Instant::now(), field.ids.spent()));
    }
    let gaze = if on_eyes.is_some() { &mut kept.eye_gazes[kept.eye] } else { &mut kept.gaze };
    let step = gaze.step(kept.surface.as_mut().unwrap(), &field, &pool, looking, &tol, &newer);
    let c = gaze.counts;
    let share = gaze.share();
    let Some(step) = step else {
        // a newer shape stopped it: what the view wants now is owed
        let mut w = shared.want.lock().unwrap();
        w.kept = Some(kept);
        w.owed = !w.cancelled;
        w.running = false;
        return Then::Stop;
    };
    // it takes over from the tiles once its first look is done; after that what a look changes
    // goes out once it is done, a finer piece for a coarser one. A moving look keeps its view
    // till then; a still one gives way to a view that moved, whose look shows what it changed
    kept.held = (kept.mesh_shown && !step.done && !fine).then_some((looking, fine));
    kept.unsent.extend(step.chunks);
    if !kept.mesh_shown && step.done {
        kept.mesh_shown = true;
        shared.hand_over(std::mem::take(&mut kept.unsent), key);
    }
    if kept.mesh_shown {
        if let Some((_, t)) = kept.fresh.take() {
            if LOG_PASSES.load(Ordering::Relaxed) {
                let held = kept.surface.as_ref().unwrap().held();
                makepad_draw::makepad_platform::log!("sdf mesh takes over: {:.1} ms, {} evals, {} quads", t.elapsed().as_secs_f64() * 1000.0, field.evals.load(Ordering::Relaxed), held.quads);
            }
        }
        if step.done && !kept.unsent.is_empty() {
            shared.send(std::mem::take(&mut kept.unsent), key, false);
        }
    }
    if step.done && LOG_PASSES.load(Ordering::Relaxed) {
        let held = kept.surface.as_ref().unwrap().held();
        let (_, _, e0, t0, ids0) = kept.began.unwrap();
        let ids = field.ids.spent();
        makepad_draw::makepad_platform::log!(
            "sdf mesh {}: {:.1} ms {} evals, ids {} in {:.2} ms; {} checks {} strayed, {} rays {} landed, {} cones {} in front; {} cells {} quads, {} draws, cpu {:.1} MB, gpu {:.1} MB",
            if fine { "still" } else { "moving" },
            t0.elapsed().as_secs_f64() * 1000.0,
            field.evals.load(Ordering::Relaxed) - e0,
            ids.0 - ids0.0,
            ids.1 - ids0.1,
            c.checks, c.strayed, c.rays, c.landed, c.cones, c.front,
            held.cells, held.quads, held.chunks,
            held.cpu_bytes as f64 / 1e6,
            held.gpu_bytes as f64 / 1e6
        );
    }
    if step.done && fine && of_now && kept.mesh_shown {
        shared.send(Vec::new(), key, true);
    }
    // both eyes looked to the end from where they are, this finely: nothing more is owed
    let looked = match on_eyes {
        Some(e) => {
            if step.done {
                kept.seen[kept.eye] = Some((looking, tol.leaf.to_bits()));
                kept.eye = 1 - kept.eye;
            }
            let leaf = if still { STILL } else { MOVING }.leaf.to_bits();
            kept.seen == [Some((e[0], leaf)), Some((e[1], leaf))]
        }
        // the look that took over: the eyes' own are still owed
        None => step.done && of_now && eyes.is_none(),
    };
    let mut w = shared.want.lock().unwrap();
    let current = kept.mesh_shown && w.key == key && w.cam == Some(cam) && w.still == still;
    if current && of_now {
        w.traced = match (fine, step.done) {
            (false, _) => 0.0,
            (true, true) => 1.0,
            (true, false) => share,
        };
    }
    let done = w.cancelled || (current && looked);
    w.converged = current && looked && still;
    let rested = w.converged;
    w.kept = Some(kept);
    if done {
        w.running = false;
        drop(w);
        if let Some(q) = shared.queue.as_ref().filter(|_| rested) {
            q.kick(&pool);
        }
        return Then::Stop;
    }
    // another round: on this worker unless other work waits for one ([`Shared::goes_on`]); a
    // look that is out leaves it to other parts' looks that wait
    if step.done {
        Then::Look
    } else {
        Then::Round
    }
}

/// A part's tiles, on the UI's side.
pub struct Tiles {
    shared: Arc<Shared>,
    handle: Option<TaskHandle<()>>,
    drawn: Vec<Option<Chunk>>,
    /// Changes with [`Tiles::drawn`].
    drawn_rev: u64,
    drawn_key: Option<[u8; 32]>,
    settled: bool,
    last_cam: Option<Cam>,
    /// The camera has been where it is since then.
    still_since: Instant,
    /// The camera, still since `still_since`, has rested then: the view looks again at that time
    /// ([`Tiles::look_due_in`]).
    still_at: Option<Instant>,
    /// The shape, unedited since `edit_at`, is quiet then: its one mesh may start.
    quiet_at: Option<Instant>,
    /// Where a head came to rest: the views it keeps while it stays there.
    head_at: Option<(Cam, [Cam; 2])>,
    /// The shape last changed then.
    edit_at: Option<Instant>,
    /// The worker last started is the mesh's.
    job_mesh: bool,
    /// What is drawn is the mesh, not view tiles.
    mesh_drawn: bool,
    /// The shape whose pieces wait in `held` (its mesh's, when `held_mesh`), what is drawn
    /// staying, till its still look is done ([`Tiles::hold_until_whole`]).
    holding: Option<[u8; 32]>,
    held: Vec<(usize, Option<Chunk>)>,
    held_mesh: bool,
    /// The view the hold's first look was from: it goes on when the view moves from there.
    held_from: Option<Cam>,
    /// [`Tiles::frame`] ran since the view's last [`Tiles::unframe`]: a part no longer drawn owes
    /// the view nothing.
    framed: bool,
}

impl Tiles {
    pub fn new() -> Tiles {
        Tiles::with_queue(None)
    }

    /// A part whose scan is on `queue`.
    pub fn in_queue(queue: &scan::ScanQueue) -> Tiles {
        let tiles = Tiles::with_queue(Some(queue.queue()));
        queue.add(&tiles.shared);
        tiles
    }

    fn with_queue(queue: Option<Arc<scan::Queue>>) -> Tiles {
        if std::env::var_os("FAB_SDF_LOG").is_some() {
            LOG_PASSES.store(true, Ordering::Relaxed);
        }
        let want = Want { cam: None, eyes: None, key: [0; 32], shape: None, still: false, running: false, cancelled: false, failed: false, converged: false, traced: 0.0, up: [0.0, 0.0, 1.0], to_light: None, scannable: true, selected: false, scanning: false, owed: false, scan_done: false, quiet: false, kept: None, queued: None, rounds: 0 };
        Tiles { shared: Arc::new(Shared { want: Mutex::new(want), out: Mutex::new(None), woke: AtomicBool::new(false), version: AtomicU64::new(0), step: AtomicU64::new(1f64.to_bits()), field: Mutex::new(None), tables: Mutex::new(Vec::new()), queue, detail: AtomicU8::new(0) }), handle: None, drawn: vec![None; CHUNKS], drawn_rev: 0, drawn_key: None, settled: false, last_cam: None, still_since: Instant::now(), still_at: None, quiet_at: None, head_at: None, edit_at: None, job_mesh: false, mesh_drawn: false, holding: None, held: Vec::new(), held_mesh: false, held_from: None, framed: false }
    }

    /// This frame's view of `shape`: starts what it wants, gives what there is to draw.
    pub fn frame(&mut self, pool: &TaskPool, cam: Cam, key: [u8; 32], shape: &Arc<SdfShape>) -> &[Option<Chunk>] {
        self.look(pool, cam, None, key, shape)
    }

    /// [`Tiles::frame`] for a headset: a head never rests, so the part stays at the moving detail;
    /// what only one eye sees is that eye's own look.
    pub fn frame_eyes(&mut self, pool: &TaskPool, eyes: [Cam; 2], key: [u8; 32], shape: &Arc<SdfShape>) -> &[Option<Chunk>] {
        let head = Cam::between(&eyes[0], &eyes[1]);
        let part = std::array::from_fn(|i| (shape.min[i] + shape.max[i]) * 0.5);
        let (head, eyes) = match self.head_at {
            Some((at, kept)) if head.rests_at(&at, part) => (at, kept),
            _ => (head, eyes),
        };
        self.head_at = Some((head, eyes));
        self.look(pool, head, Some(eyes), key, shape)
    }

    fn look(&mut self, pool: &TaskPool, cam: Cam, eyes: Option<[Cam; 2]>, key: [u8; 32], shape: &Arc<SdfShape>) -> &[Option<Chunk>] {
        self.framed = true;
        // this look takes what woke the view; whatever comes after it wakes the view again
        self.shared.woke.store(false, Ordering::Release);
        let now = Instant::now();
        if self.last_cam != Some(cam) {
            self.last_cam = Some(cam);
            self.still_since = now;
        }
        let rest_at = self.still_since + if eyes.is_some() { HEAD_SETTLE } else { SETTLE };
        let still = now >= rest_at;
        self.still_at = (!still).then_some(rest_at);
        // the hold's first look is from where the part now stands (its frame may have moved a
        // hair as it was committed); a move after that lets go
        let view_moved = self.holding.is_some() && *self.held_from.get_or_insert(cam) != cam;
        let mut start = false;
        let mut raised = None;
        let job_mesh;
        {
            let mut w = self.shared.want.lock().unwrap();
            let new_shape = w.shape.is_none() || w.key != key;
            if new_shape {
                self.edit_at = Some(Instant::now());
                w.key = key;
                w.shape = Some(shape.clone());
                w.failed = false;
                w.scan_done = false;
                self.shared.version.fetch_add(1, Ordering::Release);
            }
            let moved = w.cam != Some(cam) || w.eyes != eyes;
            if moved || new_shape {
                if let Some(q) = &self.shared.queue {
                    q.input_now();
                }
            }
            w.cam = Some(cam);
            w.eyes = eyes;
            let settle_now = still && !w.still;
            w.still = still;
            if moved || new_shape {
                w.converged = false;
                w.traced = 0.0;
            }
            // the one mesh draws once the part has rested; until then each version is view tiles,
            // whole by construction
            let quiet = self.edit_at.is_none_or(|t| t.elapsed().as_millis() >= EDIT_QUIET_MS);
            self.quiet_at = self.edit_at.filter(|_| !quiet).map(|t| t + EDIT_QUIET);
            w.quiet = quiet;
            job_mesh = quiet;
            if !w.running && !w.failed && (moved || new_shape || settle_now || job_mesh != self.job_mesh || (w.kept.is_none() && !w.scanning) || w.owed) {
                if w.scanning {
                    // it stops at its next batch, and starts this then
                    w.owed = true;
                } else {
                    w.owed = false;
                    w.running = true;
                    start = true;
                }
                self.job_mesh = job_mesh;
            }
            // a round waiting for a worker below the class the view wants now goes again at that
            // class: a part refining at rest as the view starts to move would wait behind the
            // moving parts' looks for as long as they move
            if w.queued.is_some_and(|(_, class)| Shared::class_of(&w, self.job_mesh) > class) {
                raised = Some(self.shared.queue_round(&mut w, self.job_mesh));
            }
        }
        if let Some((round, class)) = raised {
            match self.shared.submit_round(pool, self.job_mesh, round, class) {
                Ok(h) => self.handle = Some(h),
                Err(_) => {
                    let mut w = self.shared.want.lock().unwrap();
                    w.running = false;
                    w.failed = true;
                }
            }
        }
        if view_moved {
            // a moving view shows a moving look's detail anyway: what is held goes on
            self.let_go();
        }
        if start {
            match self.shared.start(pool, job_mesh) {
                Ok(h) => self.handle = Some(h),
                Err(_) => {
                    let mut w = self.shared.want.lock().unwrap();
                    w.running = false;
                    w.failed = true;
                }
            }
        }
        self.latest()
    }

    /// The tiles as the tracer last published them, without a look: a view that leaves the camera
    /// to another (a headset's) still shows what it found.
    pub fn latest(&mut self) -> &[Option<Chunk>] {
        if let Some(mut next) = self.shared.out.lock().unwrap().take() {
            let new_shape = self.drawn_key != Some(next.key);
            if self.holding == Some(next.key) && new_shape && self.drawn_key.is_some() && !next.settled {
                // its still look not done: the last shape stays on screen
                if next.replace {
                    self.held = next.chunks;
                    self.held_mesh = true;
                } else {
                    self.held.append(&mut next.chunks);
                }
                return &self.drawn;
            }
            if new_shape {
                // the held shape done (what was held first, then the rest), or another shape that
                // was never held
                let (held, mesh) = (std::mem::take(&mut self.held), std::mem::take(&mut self.held_mesh));
                self.held_from = None;
                if self.holding.take() == Some(next.key) && !next.replace {
                    next.chunks.splice(0..0, held);
                    next.replace |= mesh;
                }
            }
            // a new shape shares nothing with the last, and the mesh nothing with the tiles
            if new_shape || next.replace {
                self.drawn.fill(None);
                self.mesh_drawn = next.replace;
            }
            for (chunk, inst) in next.chunks {
                self.drawn[chunk] = inst;
            }
            self.drawn_rev += 1;
            self.drawn_key = Some(next.key);
            self.settled |= next.settled;
        }
        &self.drawn
    }

    /// The shape `key` stays off screen till its still look is done, what is drawn now staying
    /// till then (or till the view moves, when a moving look's detail shows anyway): a placement
    /// preview's part keeps showing till its committed field is whole and as fine, never a frame
    /// of it half traced or at a moving look's detail.
    pub fn hold_until_whole(&mut self, key: [u8; 32]) {
        if self.holding != Some(key) {
            self.held.clear();
            self.held_mesh = false;
            self.held_from = None;
        }
        self.holding = (self.drawn_key != Some(key)).then_some(key);
    }

    /// What is held goes on screen now, as far as it has come.
    fn let_go(&mut self) {
        self.held_from = None;
        let Some(key) = self.holding.take() else { return };
        let (held, mesh) = (std::mem::take(&mut self.held), std::mem::take(&mut self.held_mesh));
        if held.is_empty() {
            return;
        }
        self.drawn.fill(None);
        self.mesh_drawn = mesh;
        for (chunk, inst) in held {
            self.drawn[chunk] = inst;
        }
        self.drawn_rev += 1;
        self.drawn_key = Some(key);
    }

    /// The view starts a new record of its passes: a part it does not look at in it owes the view
    /// nothing after.
    pub fn unframe(&mut self) {
        self.framed = false;
    }

    /// The tracer is working, has something for the view it has not looked at, owes the view a
    /// look once the camera has rested or the shape is quiet, or has work the view wants that no
    /// worker is doing yet.
    pub fn busy(&self) -> bool {
        let w = self.shared.want.lock().unwrap();
        // a worker that stopped for the other one lets go of the part before it wakes the view:
        // what the view wants next is owed from the moment it lets go
        let owed = w.owed || (!w.running && w.quiet != self.job_mesh);
        !w.failed
            && !w.cancelled
            && (w.running || owed || self.shared.woke.load(Ordering::Acquire) || (self.framed && self.owed_at().is_some()) || self.shared.out.lock().unwrap().is_some())
    }

    /// The view drew the part in its last record and has more to come from it: [`Tiles::busy`],
    /// or views of the part's scan left, whose finds change what the view shows (the shadow, the
    /// edges a cell they split moves) once the user rests. The view owes the scan nothing, so
    /// `busy` leaves it out.
    pub fn more_to_come(&self) -> bool {
        let scan_left = || {
            let w = self.shared.want.lock().unwrap();
            self.shared.queue.is_some() && w.scannable && !w.scan_done && !w.failed && !w.cancelled
        };
        self.framed && (self.busy() || scan_left())
    }

    /// The tracer has something for the view since it last looked (tiles to draw, or work that
    /// cannot go on without a look): the view is to look again now ([`Tiles::frame`]). A signal
    /// comes with it.
    pub fn take_woke(&self) -> bool {
        self.shared.woke.swap(false, Ordering::AcqRel)
    }

    /// How long until the view is to look again though the tracer has nothing for it: until the
    /// camera has rested (the finer tolerance) or the shape is quiet (its one mesh). None: nothing
    /// is owed; the view looks when the tracer wakes it, its camera moves or its shape changes.
    pub fn look_due_in(&self) -> Option<Duration> {
        if !self.framed {
            return None;
        }
        {
            let w = self.shared.want.lock().unwrap();
            if w.failed || w.cancelled {
                return None;
            }
        }
        self.owed_at().map(|at| at.saturating_duration_since(Instant::now()))
    }

    /// The first of the rest and the quiet still ahead of the last look.
    fn owed_at(&self) -> Option<Instant> {
        match (self.still_at, self.quiet_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The tiles of the last [`Tiles::frame`].
    pub fn drawn(&self) -> &[Option<Chunk>] {
        &self.drawn
    }

    /// Changes when [`Tiles::drawn`] does.
    pub fn drawn_rev(&self) -> u64 {
        self.drawn_rev
    }

    /// How many tiles [`Tiles::drawn`] holds.
    pub fn count(&self) -> usize {
        self.drawn.iter().flatten().map(|d| d.count()).sum()
    }

    /// The part's up in its own frame, whether it is drawn and whether it is selected: which way
    /// its scan looks from, and where it stands on the queue.
    pub fn set_scan(&self, up: [f64; 3], drawn: bool, selected: bool) {
        let mut w = self.shared.want.lock().unwrap();
        w.up = up;
        w.scannable = drawn;
        w.selected = selected;
    }

    /// The way to the light, in the part's own frame.
    pub fn set_to_light(&self, to_light: [f64; 3]) {
        self.shared.want.lock().unwrap().to_light = Some(to_light);
    }

    /// How fine each version under the hand is from now on.
    pub fn set_drag_detail(&self, detail: DragDetail) {
        self.shared.detail.store(detail as u8, Ordering::Relaxed);
    }

    /// Its quads wind with their outward side up, so those turned away need not draw.
    pub fn one_sided(&self) -> bool {
        self.mesh_drawn
    }

    /// The shape the tiles of the last [`Tiles::frame`] are of.
    pub fn drawn_key(&self) -> Option<[u8; 32]> {
        self.drawn_key
    }

    /// The feature table of what is drawn: a tile's ordinal `o` (its word 23, `meta.w`) shows
    /// feature `table[o - 1]` ([`makepad_csg_sdf::SdfNode::Id`]); 0 none. None while its shape's
    /// table is not known.
    pub fn drawn_ids(&self) -> Option<Arc<[u32]>> {
        let key = self.drawn_key?;
        self.shared.tables.lock().unwrap().iter().rev().find(|(k, _)| *k == key).map(|(_, t)| t.clone())
    }

    /// The tiles a view ended on, once: the view stayed still and the trace is done.
    pub fn take_settled(&mut self) -> Option<usize> {
        std::mem::take(&mut self.settled).then(|| self.count())
    }

    /// How far the still look at the view of the last [`Tiles::frame`] is traced, 0..=1: its
    /// rounds, a stride halving to one sample, then done. 0 while a moving look holds the view.
    pub fn traced(&self) -> f32 {
        self.shared.want.lock().unwrap().traced
    }

    /// The shape `key` cannot be traced.
    pub fn failed_on(&self, key: &[u8; 32]) -> bool {
        let w = self.shared.want.lock().unwrap();
        w.failed && w.key == *key
    }

    /// An open-ended loop in the field of shape `key` has stopped at its cap.
    pub fn loop_stopped_on(&self, key: &[u8; 32]) -> bool {
        self.shared.field.lock().unwrap().as_ref().is_some_and(|(k, f)| k == key && f.loop_stopped())
    }

    /// What a walk along a ray multiplies the shape's value by, once its field is known (1 before).
    pub fn step(&self) -> f64 {
        f64::from_bits(self.shared.step.load(Ordering::Relaxed))
    }
}

impl Drop for Tiles {
    fn drop(&mut self) {
        let mut w = self.shared.want.lock().unwrap();
        w.cancelled = true;
        self.shared.version.fetch_add(1, Ordering::Release);
    }
}

#[cfg(test)]
#[path = "truth.rs"]
mod truth;

#[path = "scan.rs"]
pub mod scan;

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_csg_sdf::SdfOp;

    /// The tests that hold every worker of the process's one pool take it in turn: two at once
    /// would each hold part of it, waiting for the rest.
    static WHOLE_POOL: Mutex<()> = Mutex::new(());

    /// The pool's workers to this test, as far as the others that hold them all go; a red one
    /// leaves them to the next.
    fn whole_pool() -> std::sync::MutexGuard<'static, ()> {
        WHOLE_POOL.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Runs its closure when dropped, a failing test's unwind too: the jobs a test holds workers
    /// with let them go, so one red test does not starve every test after it.
    struct LetGo<F: FnMut()>(F);

    impl<F: FnMut()> Drop for LetGo<F> {
        fn drop(&mut self) {
            (self.0)()
        }
    }

    /// A perspective view of 1600 x 1000 from `eye` toward `at`, 40 degrees high (z up).
    fn view(eye: V, at: V) -> Cam {
        let f = norm(sub(at, eye));
        let r = norm(cross(f, [0.0, 0.0, 1.0]));
        let h = (20f64).to_radians().tan();
        Cam { eye, f, r, u: cross(r, f), half: [h * 1.6, h], off: [0.0; 2], ortho: false, w: 1600.0, h: 1000.0 }
    }

    /// Runs a pass to its end, and for each triangle the farthest it strays from the surface at
    /// its middle and its edges' middles, in pixels of the view that traced it; the longest edge
    /// from a tile's centre in those pixels.
    fn strays(root: &SdfNode, cam: Cam, still: bool) -> (Vec<f64>, f64) {
        let (_, pass, _) = super::truth::pass_to_the_end(root, cam, still);
        let (mut out, mut longest) = (Vec::new(), 0.0f64);
        for (t, _) in pass.tiles.iter().zip(&pass.alive).filter(|(_, a)| **a) {
            let at = |k: usize| -> V { [t[k * 4] as f64, t[k * 4 + 1] as f64, t[k * 4 + 2] as f64] };
            let c = at(4);
            for k in 0..4 {
                let (a, b) = (at(k), at((k + 1) % 4));
                if len(cross(sub(a, c), sub(b, c))) == 0.0 {
                    continue;
                }
                let pts = [mul(add(add(a, b), c), 1.0 / 3.0), mul(add(a, c), 0.5), mul(add(b, c), 0.5), mul(add(a, b), 0.5)];
                let worst = raw(root, &pts).into_iter().map(f64::abs).fold(0.0, f64::max);
                let pw = cam.pw(len(sub(c, cam.eye)));
                out.push(worst / pw);
                longest = longest.max(len(sub(a, c)).max(len(sub(b, c))) / pw);
            }
        }
        (out, longest)
    }

    fn bracket() -> SdfNode {
        let hole = |x: f64| SdfNode::Transform { at: [x, 0.0, 0.0], rotate: [90.0, 0.0, 0.0], scale: 1.0, of: Box::new(SdfNode::Cylinder { radius: 4.0, top_radius: 4.0, height: 40.0 }) };
        SdfNode::Combine { op: SdfOp::Difference, blend: 0.0, of: vec![SdfNode::Box { size: [40.0, 8.0, 30.0], radius: 0.0 }, hole(-10.0), hole(10.0)] }
    }

    /// The bound about an ellipsoid's centre is a true one: nothing of the surface within it, from
    /// anywhere in the middle of a hollow one, the centre's neighbours most of all; and so under a
    /// stretch along one axis (a part scaled by axis), the ellipsoid's own slope carried through
    /// it, not a guess.
    #[test]
    fn the_bound_in_a_hollow_ellipsoid_holds() {
        let hollow = SdfNode::Shell { thickness: 0.8, of: Box::new(SdfNode::Ellipsoid { radii: [21.0, 25.0, 12.0] }) };
        let stretch = SdfDeform::Stretch { scale: [2.0, 1.0, 1.0] };
        let lipschitz = stretch.lipschitz(None);
        let stretched = SdfNode::Deform { deform: stretch, lipschitz, of: Box::new(hollow.clone()) };
        for (root, k) in [(hollow, [1.0, 1.0, 1.0]), (stretched, [2.0, 1.0, 1.0])] {
            let f = Field::new(&root, false, &TaskPool::closed()).unwrap();
            assert!(f.spots.len() == 1, "its ellipsoid is known for what it is: {} spots", f.spots.len());
            let dirs: Vec<V> = (0..96).map(|i| { let (z, a) = (1.0 - (i as f64 + 0.5) / 48.0, i as f64 * 2.399963); let r = (1.0 - z * z).sqrt(); [r * a.cos(), r * a.sin(), z] }).collect();
            let (mut proven, mut v, mut b) = (0.0, Vec::new(), Vec::new());
            for i in 0..1500u32 {
                let reach = |m: u32, r: f64| (((i * m + 7) % 251) as f64 / 250.0 - 0.5) * 2.0 * r;
                // from far out to very near the centre, down to a thousandth of the way
                let shrink = 0.001f64.powf((i % 16) as f64 / 15.0);
                let p = [reach(13, 18.0 * k[0]) * shrink, reach(29, 22.0 * k[1]) * shrink, reach(53, 10.0 * k[2]) * shrink];
                f.eval(&[p], &mut v, &mut b);
                if v[0] <= 0.0 || b[0] <= 0.0 {
                    continue;
                }
                proven += b[0];
                let around: Vec<V> = dirs.iter().map(|d| add(p, mul(*d, b[0] * 0.999))).collect();
                for (q, value) in around.iter().zip(raw(&root, &around)) {
                    assert!(value > 0.0, "stretched {k:?}: the surface at {q:?} is within the {} proven at {p:?}", b[0]);
                }
            }
            assert!(proven > 600.0, "stretched {k:?}: {proven} mm proven over the points");
        }
    }

    fn ball_before_a_wall() -> SdfNode {
        let ball = SdfNode::Transform { at: [0.0, -25.0, 0.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Sphere { radius: 8.0 }) };
        SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of: vec![SdfNode::Box { size: [60.0, 4.0, 40.0], radius: 0.0 }, ball] }
    }

    #[test]
    fn a_still_views_jitter_is_not_a_move() {
        let proj = Mat4f::perspective(40.0, 1.5, 0.1, 1000.0);
        let mut jittered = proj;
        (jittered.v[8], jittered.v[9]) = (0.001, -0.002);
        let cam = |p: &Mat4f| Cam::new(&Mat4f::identity(), p, &Mat4f::identity(), 1200.0, 800.0);
        assert!(cam(&proj) == cam(&jittered));
    }

    /// A still head with off-axis eyes: each sees the view its matrices give, the mesh ends up
    /// holding the strips of wall behind the ball that only one eye sees, and the worker stops.
    /// On this scene the look from between the eyes already covers them, so what it shows is that
    /// the eyes' looks finish with nothing bare, not that they were what covered it.
    #[test]
    fn a_headset_gets_a_look_for_each_eye_and_then_rests() {
        let pool = makepad_draw::makepad_platform::Cx::new(Box::new(|_, _| {})).task_pool();
        let root = ball_before_a_wall();
        let shape = Arc::new(SdfShape { root: root.clone(), min: [-30.0, -33.0, -20.0], max: [30.0, 2.0, 20.0], materials: Vec::new() });
        let (w, h) = (1000.0, 800.0);
        // along +y, 32 mm to each side, each eye seeing further outward than inward
        let matrices = |side: f32, tremble: f32| {
            let e = [side * 32.0 + tremble, -160.0, 6.0];
            let view = Mat4f { v: [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 1.0, 0.0, 0.0, -e[0], -e[2], e[1], 1.0] };
            let (l, r, b, t) = if side < 0.0 { (-0.5, 0.35, -0.4, 0.3) } else { (-0.35, 0.5, -0.4, 0.3) };
            let mut proj = Mat4f { v: [0.0; 16] };
            proj.v[0] = 2.0 / (r - l);
            proj.v[5] = 2.0 / (t - b);
            proj.v[8] = (r + l) / (r - l);
            proj.v[9] = (t + b) / (t - b);
            proj.v[10] = -1.0;
            proj.v[11] = -1.0;
            proj.v[14] = -0.1;
            (view, proj)
        };
        let at = |tremble: f32| [-1.0f32, 1.0].map(|side| matrices(side, tremble));
        let cams_of = |eyes: [(Mat4f, Mat4f); 2]| eyes.map(|(view, proj)| Cam::of_eye(&view, &proj, &Mat4f::identity(), w, h));
        let eyes = at(0.0);
        let cams = cams_of(eyes);
        // a point lands where its matrices put it, and its pixel's ray comes back through it
        for (c, (view, proj)) in cams.iter().zip(&eyes) {
            let p = [7.0f32, -20.0, 5.0];
            let m = Mat4f::mul(proj, view);
            let clip: [f32; 4] = std::array::from_fn(|i| m.v[i] * p[0] + m.v[4 + i] * p[1] + m.v[8 + i] * p[2] + m.v[12 + i]);
            let q = p.map(|x| x as f64);
            let (x, y, _) = c.proj(q).unwrap();
            assert!((x - (clip[0] / clip[3] * 0.5 + 0.5) as f64 * w).abs() < 0.01 && (y - (0.5 - clip[1] / clip[3] * 0.5) as f64 * h).abs() < 0.01, "at {x} {y}, clip {clip:?}");
            let (o, d) = c.ray(x, y);
            assert!(len(cross(d, sub(q, o))) < 1e-6);
        }
        let head = Cam::between(&cams[0], &cams[1]);
        for c in &cams {
            for (x, y) in [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)] {
                let (_, d) = c.ray(x, y);
                let z = dot(d, head.f);
                assert!(dot(d, head.r).abs() / z <= head.half[0] + 1e-9 && dot(d, head.u).abs() / z <= head.half[1] + 1e-9, "the view from between leaves out a corner");
            }
        }
        let mut tiles = Tiles::new();
        let mut tremble = 0.2;
        loop {
            // a head at rest is never exactly still
            tremble = -tremble;
            tiles.frame_eyes(&pool, cams_of(at(tremble)), [1; 32], &shape);
            if tiles.one_sided() && !tiles.busy() {
                break;
            }
            // a tracer that stops with a look owed leaves every later frame waiting on it
            let w = tiles.shared.want.lock().unwrap();
            assert!(w.running || w.converged || !tiles.job_mesh || !w.still, "the tracer stopped with a look still owed");
            drop(w);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let closed = TaskPool::closed();
        let want = tiles.shared.want.lock().unwrap();
        assert!(!want.running && want.converged && want.still);
        let (field, surface) = want.kept.as_ref().map(|k| (k.field.clone().unwrap(), k.surface.as_ref().unwrap())).unwrap();
        // what a mesh from between the eyes alone would leave bare
        let (mut alone, mut gaze) = (Surface::new(&field), Gaze::default());
        while !gaze.step(&mut alone, &field, &closed, head, &MOVING, &|| false).unwrap().done {}
        let bare = |c: &Cam, s: &Surface| -> usize {
            let mut cover = crate::surface::Cover::default();
            cover.fill(c, 1.0, s, &closed);
            let at: Vec<(f64, f64)> = (0..h as usize / 2).flat_map(|y| (0..w as usize / 2).map(move |x| (x as f64 * 2.0 + 0.5, y as f64 * 2.0 + 0.5))).collect();
            let truth = super::truth::exact(&root, field.lo, field.hi, c, 1.0, &at);
            let (nx, ny) = (w as usize / 2, h as usize / 2);
            (1..ny - 1).flat_map(|y| (1..nx - 1).map(move |x| (x, y))).filter(|&(x, y)| {
                let i = y * nx + x;
                truth[i].is_finite() && !cover.at(x * 2, y * 2).is_finite() && [i - 1, i + 1, i - nx, i + nx].iter().all(|n| truth[*n].is_finite())
            }).count()
        };
        for c in &cams {
            let (with, without) = (bare(c, surface), bare(c, &alone));
            eprintln!("bare samples: {with} with the eye's own look, {without} from between the eyes alone");
            assert!(with <= without && with < 40, "{with} bare with the eye's look, {without} without");
        }
    }

    /// Under a drag every drawn version is whole view tiles: the part never goes blank, and a mesh
    /// shows only when the part has rested, with its whole first look there at once.
    #[test]
    fn a_drag_never_shows_a_partial_mesh() {
        let pool = makepad_draw::makepad_platform::Cx::new(Box::new(|_, _| {})).task_pool();
        let cam = view([35.0, -90.0, 40.0], [0.0; 3]);
        let ball = |r: f64| {
            let root = SdfNode::Sphere { radius: r };
            Arc::new(SdfShape { root, min: [-r; 3], max: [r; 3], materials: Vec::new() })
        };
        let radius = |i: u8| 10.0 + i as f64 * 0.1;
        let mut edited = std::collections::HashMap::new();
        let mut shown = false;
        let mut frame = |tiles: &mut Tiles, i: u8| {
            let key = [i; 32];
            edited.entry(key).or_insert_with(Instant::now);
            tiles.frame(&pool, cam, key, &ball(radius(i)));
            shown |= tiles.count() > 0;
            assert!(!shown || tiles.count() > 0, "a blank frame");
            if tiles.one_sided() {
                let t: &Instant = &edited[&tiles.drawn_key().unwrap()];
                assert!(t.elapsed().as_millis() >= EDIT_QUIET_MS, "a mesh under the drag");
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        let mut tiles = Tiles::new();
        for i in 0..60 {
            frame(&mut tiles, i);
        }
        while tiles.busy() && !tiles.one_sided() {
            frame(&mut tiles, 59);
        }
        assert!(tiles.one_sided(), "the part rested and no mesh came");
        let f = Field::new(&ball(radius(59)).root, false, &TaskPool::closed()).unwrap();
        let (mut whole, mut gaze) = (Surface::new(&f), Gaze::default());
        while !gaze.step(&mut whole, &f, &TaskPool::closed(), cam, &MOVING, &|| false).unwrap().done {}
        assert!(tiles.count() >= whole.held().quads, "{} drawn, {} in a whole look", tiles.count(), whole.held().quads);
        // the next edit takes the whole mesh away
        frame(&mut tiles, 60);
        while tiles.busy() && tiles.drawn_key() != Some([60; 32]) {
            frame(&mut tiles, 60);
        }
        assert_eq!(tiles.drawn_key(), Some([60; 32]));
    }

    /// A shape held till whole (a placement preview's committed field) never shows half traced or
    /// at a moving look's detail: what was drawn stays, the part never blank, till the new
    /// shape's still look is done and its mesh takes over at once; a view that moves meanwhile
    /// shows it at once, as far as it has come.
    #[test]
    fn a_held_shape_shows_only_whole() {
        let pool = makepad_draw::makepad_platform::Cx::new(Box::new(|_, _| {})).task_pool();
        let cam = view([35.0, -90.0, 40.0], [0.0; 3]);
        let ball = |r: f64| {
            let root = SdfNode::Sphere { radius: r };
            Arc::new(SdfShape { root, min: [-r; 3], max: [r; 3], materials: Vec::new() })
        };
        let (first, next) = (ball(10.0), ball(14.0));
        let mut tiles = Tiles::new();
        tiles.frame(&pool, cam, [1; 32], &first);
        while tiles.busy() && tiles.drawn_key() != Some([1; 32]) {
            std::thread::sleep(std::time::Duration::from_millis(1));
            tiles.frame(&pool, cam, [1; 32], &first);
        }
        assert_eq!(tiles.drawn_key(), Some([1; 32]), "the first shape was never drawn");
        tiles.hold_until_whole([2; 32]);
        let mut held_frames = 0;
        loop {
            tiles.frame(&pool, cam, [2; 32], &next);
            assert!(tiles.count() > 0, "a blank frame");
            if tiles.drawn_key() == Some([2; 32]) {
                assert!(tiles.one_sided() && tiles.take_settled().is_some(), "the held shape showed before its still look was done");
                break;
            }
            assert_eq!(tiles.drawn_key(), Some([1; 32]));
            held_frames += 1;
            assert!(tiles.busy(), "the held shape never came");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(held_frames > 0, "nothing was held");
        // held, then the view moves: the new shape shows at once, as far as it has come
        tiles.hold_until_whole([3; 32]);
        let third = ball(12.0);
        loop {
            tiles.frame(&pool, cam, [3; 32], &third);
            if !tiles.held.is_empty() {
                break;
            }
            assert_eq!(tiles.drawn_key(), Some([2; 32]), "the held shape showed before it was whole");
            assert!(tiles.busy(), "nothing of the held shape came");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        tiles.frame(&pool, view([40.0, -90.0, 40.0], [0.0; 3]), [3; 32], &third);
        assert_eq!(tiles.drawn_key(), Some([3; 32]), "a moving view still held the shape back");
        assert!(tiles.count() > 0, "a blank frame");
    }

    /// A still look's share of its rounds (what the chat says of a render's tracing) climbs as
    /// its stride halves to one sample, never back, and stands at its last round's when the look
    /// is done.
    #[test]
    fn a_still_look_s_share_climbs_with_its_rounds() {
        let closed = TaskPool::closed();
        let f = Field::new(&SdfNode::Sphere { radius: 20.0 }, false, &closed).unwrap();
        let cam = view([35.0, -90.0, 40.0], [0.0; 3]);
        let (mut surface, mut gaze) = (Surface::new(&f), Gaze::default());
        assert_eq!(gaze.share(), 0.0, "no look yet");
        let mut shares = Vec::new();
        loop {
            let step = gaze.step(&mut surface, &f, &closed, cam, &STILL, &|| false).unwrap();
            shares.push(gaze.share());
            if step.done {
                break;
            }
        }
        assert!(shares.windows(2).all(|w| w[1] >= w[0]) && shares.iter().all(|s| *s < 1.0) && *shares.last().unwrap() >= 0.8, "{shares:?}");
    }

    /// An edit that stops the mesh's worker leaves the edited shape owed: the view's next look
    /// starts it, though by then the edit has gone quiet and the mesh it started last is what the
    /// view wants again.
    #[test]
    fn an_edit_that_stops_the_mesh_is_traced_once_it_is_quiet() {
        let pool = makepad_draw::makepad_platform::Cx::new(Box::new(|_, _| {})).task_pool();
        let cam = view([35.0, -90.0, 40.0], [0.0; 3]);
        let ball = |r: f64| {
            let root = SdfNode::Sphere { radius: r };
            Arc::new(SdfShape { root, min: [-r; 3], max: [r; 3], materials: Vec::new() })
        };
        let mut tiles = Tiles::new();
        while !tiles.one_sided() || tiles.busy() {
            tiles.frame(&pool, cam, [1; 32], &ball(10.0));
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        // the mesh's worker is at work when the view sees the edit, and stops for it at its next
        // round: the edit is not quiet yet
        tiles.shared.want.lock().unwrap().running = true;
        tiles.frame(&pool, cam, [2; 32], &ball(11.0));
        let worker = tiles.shared.start(&pool, true).unwrap();
        while !worker.is_finished() {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(!tiles.shared.want.lock().unwrap().running);
        // the view looks next once the edit is quiet
        tiles.edit_at = tiles.edit_at.map(|t| t - EDIT_QUIET);
        while tiles.busy() && tiles.drawn_key() != Some([2; 32]) {
            tiles.frame(&pool, cam, [2; 32], &ball(11.0));
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(tiles.drawn_key(), Some([2; 32]), "the edited shape was never traced");
    }

    /// The drag's worker stops for an edit gone quiet, and lets go of the part before it wakes the
    /// view: in between, the one mesh is owed and the tracer is busy, so a view that waits on it
    /// looks again and starts the mesh.
    #[test]
    fn a_worker_that_stops_for_the_mesh_leaves_the_tracer_busy() {
        let pool = makepad_draw::makepad_platform::Cx::new(Box::new(|_, _| {})).task_pool();
        let cam = view([35.0, -90.0, 40.0], [0.0; 3]);
        let shape = Arc::new(SdfShape { root: SdfNode::Sphere { radius: 10.0 }, min: [-10.0; 3], max: [10.0; 3], materials: Vec::new() });
        let mut tiles = Tiles::new();
        // a new shape's first look starts the drag's worker
        tiles.frame(&pool, cam, [1; 32], &shape);
        while !tiles.handle.as_ref().unwrap().is_finished() {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        // the view looks once the edit is quiet and the camera has rested, with the drag's worker
        // still at work
        tiles.shared.want.lock().unwrap().running = true;
        tiles.edit_at = tiles.edit_at.map(|t| t - EDIT_QUIET);
        tiles.still_since -= SETTLE;
        tiles.frame(&pool, cam, [1; 32], &shape);
        // it stops for the quiet edit, as its next round does, and has not woken the view yet
        tiles.shared.want.lock().unwrap().running = false;
        assert!(tiles.busy(), "the mesh is owed and the tracer says it is done");
        while tiles.busy() {
            tiles.frame(&pool, cam, [1; 32], &shape);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(tiles.shared.want.lock().unwrap().converged, "the view stopped waiting before the one mesh");
    }

    /// The view as the viewport has it: it looks only when the tracer wakes it or a look it owes
    /// falls due, at a camera that stays where it is. Each thing the tracer publishes wakes it,
    /// and so does the drag's worker handing over to the mesh's; once the part has rested,
    /// nothing wakes it and nothing falls due: a still, idle view records nothing between the
    /// tracer's tiles.
    #[test]
    fn a_still_view_at_rest_is_asked_for_nothing() {
        let pool = makepad_draw::makepad_platform::Cx::new(Box::new(|_, _| {})).task_pool();
        let cam = view([35.0, -90.0, 40.0], [0.0; 3]);
        let shape = Arc::new(SdfShape { root: bracket(), min: [-20.0, -4.0, -15.0], max: [20.0, 4.0, 15.0], materials: Vec::new() });
        let mut tiles = Tiles::new();
        let look = |tiles: &mut Tiles| {
            tiles.frame(&pool, cam, [1; 32], &shape);
            tiles.look_due_in().map(|d| Instant::now() + d)
        };
        let asked = |tiles: &Tiles, due: Option<Instant>| tiles.take_woke() || due.is_some_and(|at| Instant::now() >= at);
        let began = Instant::now();
        let (mut due, mut looks) = (look(&mut tiles), 1);
        // the part rests: its one mesh at the finer tolerance, the work that found it over
        let rested = |tiles: &Tiles| {
            let w = tiles.shared.want.lock().unwrap();
            w.converged && !w.running && tiles.shared.out.lock().unwrap().is_none() && tiles.handle.as_ref().is_none_or(|h| h.is_finished())
        };
        // what it is still at, should it never rest
        let unrested = |tiles: &Tiles| {
            let w = tiles.shared.want.lock().unwrap();
            format!(
                "converged {}, running {}, a send waiting {}, its job over {}, woken {}",
                w.converged,
                w.running,
                tiles.shared.out.lock().unwrap().is_some(),
                tiles.handle.as_ref().is_none_or(|h| h.is_finished()),
                tiles.shared.woke.load(Ordering::Acquire)
            )
        };
        while !rested(&tiles) || tiles.shared.woke.load(Ordering::Acquire) {
            assert!(began.elapsed() < Duration::from_secs(60), "the part never rested ({}); {}", unrested(&tiles), pool.summary());
            if asked(&tiles, due) {
                due = look(&mut tiles);
                looks += 1;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(tiles.one_sided() && tiles.count() > 0, "it rested with no mesh drawn");
        let settled = looks;
        eprintln!("{settled} looks to rest in {:.0} ms", began.elapsed().as_secs_f64() * 1e3);
        // longer than a head takes to rest and a shape to go quiet
        let idle = Instant::now();
        while idle.elapsed() < (HEAD_SETTLE + EDIT_QUIET) * 2 {
            if asked(&tiles, due) {
                due = look(&mut tiles);
                looks += 1;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(looks, settled, "a still view at rest was asked to look again");
        assert!(!tiles.busy() && tiles.look_due_in().is_none());
    }

    /// What a person asks for starts while a moving head's looks are traced, and while a moving
    /// camera's are, and the looks go on. Why (user): a plate sat at "waiting for a free worker
    /// thread" for as long as the headset was on; and on a pool of one worker a plate load waited
    /// 160 s behind the looks of a vase Mechanics swung. A part for each worker and one more, so
    /// their looks hold them all and a part's round always waits for one, and a head or camera
    /// that has moved on by each part's next look, so no look is ever its last: the waiting part's
    /// look comes once another's is out (a part beyond the workers' count waited for as long as the
    /// head moved). A moving camera's looks rank with a person's job: it goes first only as it
    /// waited first.
    #[test]
    fn a_moving_heads_looks_leave_a_worker_for_a_load() {
        let _whole = whole_pool();
        let pool = makepad_draw::makepad_platform::Cx::new(Box::new(|_, _| {})).task_pool();
        let shape = Arc::new(SdfShape { root: bracket(), min: [-20.0, -4.0, -15.0], max: [20.0, 4.0, 15.0], materials: Vec::new() });
        let mut parts: Vec<Tiles> = (0..pool.worker_count() + 1).map(|_| Tiles::new()).collect();
        let head = |turn: u32| {
            let (s, c) = (turn as f64 * 0.02).sin_cos();
            [-3.2, 3.2].map(|side| view([side * c + 90.0 * s, side * s - 90.0 * c, 40.0], [0.0; 3]))
        };
        let mut turn = 0;
        let mut turn_head = |parts: &mut [Tiles]| {
            turn += 1;
            for (i, tiles) in parts.iter_mut().enumerate() {
                tiles.frame_eyes(&pool, head(turn), [i as u8 + 1; 32], &shape);
            }
        };
        let began = Instant::now();
        while !parts.iter().all(|t| t.one_sided() && t.count() > 0) {
            assert!(
                began.elapsed() < Duration::from_secs(120),
                "the parts' meshes never took over ({} of {} have theirs); {}",
                parts.iter().filter(|t| t.one_sided() && t.count() > 0).count(),
                parts.len(),
                pool.summary()
            );
            turn_head(&mut parts);
        }
        let started = Arc::new(AtomicBool::new(false));
        let load = started.clone();
        pool.submit_named(Class::Visible, "a load", move || load.store(true, Ordering::Release)).unwrap().detach();
        let waiting = Instant::now();
        while !started.load(Ordering::Acquire) {
            assert!(waiting.elapsed() < Duration::from_secs(60), "the load found no worker while the head moved");
            turn_head(&mut parts);
        }
        // the next looks' pieces still come
        let revs: Vec<u64> = parts.iter().map(Tiles::drawn_rev).collect();
        let going = Instant::now();
        while parts.iter().zip(&revs).any(|(t, rev)| t.drawn_rev() == *rev) {
            assert!(going.elapsed() < Duration::from_secs(120), "the head's looks stopped at the load");
            turn_head(&mut parts);
        }
        // a camera that keeps moving, its looks at a person's job's class
        let mut turn_camera = |parts: &mut [Tiles]| {
            turn += 1;
            let (s, c) = (turn as f64 * 0.02).sin_cos();
            for (i, tiles) in parts.iter_mut().enumerate() {
                tiles.frame(&pool, view([90.0 * s, -90.0 * c, 40.0], [0.0; 3]), [i as u8 + 1; 32], &shape);
            }
        };
        let revs: Vec<u64> = parts.iter().map(Tiles::drawn_rev).collect();
        let moving = Instant::now();
        while parts.iter().zip(&revs).any(|(t, rev)| t.drawn_rev() == *rev) {
            assert!(moving.elapsed() < Duration::from_secs(120), "the moving camera's looks never came");
            turn_camera(&mut parts);
        }
        let started = Arc::new(AtomicBool::new(false));
        let job = started.clone();
        pool.submit_named(Class::Interactive, "a person's job", move || job.store(true, Ordering::Release)).unwrap().detach();
        let waiting = Instant::now();
        while !started.load(Ordering::Acquire) {
            assert!(waiting.elapsed() < Duration::from_secs(60), "a person's job found no worker while the camera moved");
            turn_camera(&mut parts);
        }
        let revs: Vec<u64> = parts.iter().map(Tiles::drawn_rev).collect();
        let going = Instant::now();
        while parts.iter().zip(&revs).any(|(t, rev)| t.drawn_rev() == *rev) {
            assert!(going.elapsed() < Duration::from_secs(120), "the camera's looks stopped at the job");
            turn_camera(&mut parts);
        }
    }

    /// A view at rest refines behind work of the class a load runs at, queued before it, where it
    /// used to go ahead of all of that. Every worker is held, and one is let go: it takes the load.
    #[test]
    fn a_view_at_rest_refines_behind_what_waits() {
        use std::sync::atomic::AtomicUsize;
        let _whole = whole_pool();
        let pool = makepad_draw::makepad_platform::Cx::new(Box::new(|_, _| {})).task_pool();
        let cam = view([35.0, -90.0, 40.0], [0.0; 3]);
        let shape = Arc::new(SdfShape { root: bracket(), min: [-20.0, -4.0, -15.0], max: [20.0, 4.0, 15.0], materials: Vec::new() });
        let key = [1; 32];
        let mut tiles = Tiles::new();
        // the drag's worker first, as for any new shape
        tiles.frame(&pool, cam, key, &shape);
        while !tiles.handle.as_ref().unwrap().is_finished() {
            std::thread::sleep(Duration::from_millis(1));
        }
        let gate = Arc::new((Mutex::new(0usize), std::sync::Condvar::new()));
        let held = Arc::new(AtomicUsize::new(0));
        let workers = pool.worker_count();
        // every held worker goes on the way out
        let _let_go = LetGo(|| {
            *gate.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = workers;
            gate.1.notify_all();
        });
        for _ in 0..workers {
            let (g, h) = (gate.clone(), held.clone());
            pool.submit_named(Class::Visible, "holds a worker", move || {
                h.fetch_add(1, Ordering::AcqRel);
                let mut tickets = g.0.lock().unwrap();
                while *tickets == 0 {
                    tickets = g.1.wait(tickets).unwrap();
                }
                *tickets -= 1;
            })
            .unwrap()
            .detach();
        }
        let began = Instant::now();
        while held.load(Ordering::Acquire) < workers {
            assert!(began.elapsed() < Duration::from_secs(120), "every worker never came to be held");
            std::thread::sleep(Duration::from_millis(1));
        }
        // whether the mesh's worker has yet touched the part, as the load finds it
        let (shared, load_saw) = (tiles.shared.clone(), Arc::new(Mutex::new(None)));
        let saw = load_saw.clone();
        pool.submit_named(Class::Visible, "a load", move || {
            let w = shared.want.lock().unwrap();
            *saw.lock().unwrap() = Some(!w.kept.as_ref().is_some_and(|k| k.surface_of == Some(key)));
        })
        .unwrap()
        .detach();
        // the edit quiet and the camera at rest: the mesh's worker is owed, and queues
        tiles.edit_at = tiles.edit_at.map(|t| t - EDIT_QUIET);
        tiles.still_since -= SETTLE;
        tiles.frame(&pool, cam, key, &shape);
        assert!(tiles.job_mesh && tiles.shared.want.lock().unwrap().still, "the mesh's worker at a view at rest");
        *gate.0.lock().unwrap() = 1;
        gate.1.notify_all();
        let waiting = Instant::now();
        let first = loop {
            if let Some(first) = *load_saw.lock().unwrap() {
                break first;
            }
            assert!(waiting.elapsed() < Duration::from_secs(60), "the load found no worker");
            std::thread::sleep(Duration::from_millis(1));
        };
        assert!(first, "the view at rest went ahead of the load");
    }

    /// Tracing work gives a worker up at the end of a batch, not when all its batches are done.
    /// Why (user): a resize waited for workers for minutes behind the tracer's helpers, each of
    /// which held on until the fan-out it joined had no batch left. Every worker is held here, some
    /// by the batches and the rest by jobs that stay; only a helper can let the job in.
    #[test]
    fn a_worker_on_a_batch_leaves_for_a_job_of_a_higher_class() {
        use std::sync::atomic::AtomicUsize;
        let _whole = whole_pool();
        let pool = makepad_draw::makepad_platform::Cx::new(Box::new(|_, _| {})).task_pool();
        let on_batches = WORKERS.min(pool.worker_count());
        let stay = pool.worker_count() - on_batches;
        // (the round of batches let go, all let go)
        let round = Arc::new((Mutex::new((0u64, false)), std::sync::Condvar::new()));
        let hold = |round: &(Mutex<(u64, bool)>, std::sync::Condvar), every_round: bool| {
            let mut at = round.0.lock().unwrap();
            let began = at.0;
            while !at.1 && (!every_round || at.0 == began) {
                at = round.1.wait(at).unwrap();
            }
        };
        // every held worker goes on the way out
        let _let_go = LetGo(|| {
            round.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).1 = true;
            round.1.notify_all();
        });
        let (in_batch, staying) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let mut jobs = Vec::new();
        for _ in 0..stay {
            let (r, n) = (round.clone(), staying.clone());
            jobs.push(pool.submit_named(Class::Visible, "stays", move || {
                n.fetch_add(1, Ordering::AcqRel);
                hold(&r, false);
            }).unwrap());
        }
        let (r, n, inner) = (round.clone(), in_batch.clone(), pool.clone());
        jobs.push(pool.submit_named(Class::Visible, "tracing", move || {
            let mut items = vec![0u8; on_batches * 4096];
            par(&inner, &mut items, 1, |_| {
                n.fetch_add(1, Ordering::AcqRel);
                hold(&r, true);
                n.fetch_sub(1, Ordering::AcqRel);
            });
        }).unwrap());
        let began = Instant::now();
        while staying.load(Ordering::Acquire) < stay || in_batch.load(Ordering::Acquire) < on_batches {
            assert!(began.elapsed() < Duration::from_secs(120), "the work never held every worker");
            std::thread::sleep(Duration::from_millis(1));
        }
        let started = Arc::new(AtomicBool::new(false));
        let start = started.clone();
        pool.submit_named(Class::Interactive, "a person's job", move || start.store(true, Ordering::Release)).unwrap().detach();
        // the batches in hand end, and no others
        round.0.lock().unwrap().0 += 1;
        round.1.notify_all();
        let waiting = Instant::now();
        while !started.load(Ordering::Acquire) {
            assert!(waiting.elapsed() < Duration::from_secs(60), "the job found no worker: the batches' helpers kept on to the next");
            std::thread::sleep(Duration::from_millis(1));
        }
        round.0.lock().unwrap().1 = true;
        round.1.notify_all();
        let ending = Instant::now();
        while !jobs.iter().all(TaskHandle::is_finished) {
            assert!(ending.elapsed() < Duration::from_secs(120), "the batches never ended");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// A version under the hand is whole at each detail, and each is finer than the last.
    #[test]
    fn more_drag_detail_makes_a_version_finer() {
        let pool = makepad_draw::makepad_platform::Cx::new(Box::new(|_, _| {})).task_pool();
        let cam = view([35.0, -90.0, 40.0], [0.0; 3]);
        let first_version = |detail: DragDetail| {
            let mut tiles = Tiles::new();
            tiles.set_drag_detail(detail);
            // edits keep coming: a part at rest would go on to its mesh
            for i in 0u8.. {
                let r = 10.0 + i as f64 * 0.001;
                let shape = Arc::new(SdfShape { root: SdfNode::Sphere { radius: r }, min: [-r; 3], max: [r; 3], materials: Vec::new() });
                tiles.frame(&pool, cam, [i; 32], &shape);
                if tiles.count() > 0 {
                    assert!(!tiles.one_sided(), "{detail:?}: a mesh under the drag");
                    return tiles.count();
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            panic!("{detail:?}: no version in 255 edits");
        };
        let (realtime, halfway, full) = (first_version(DragDetail::Realtime), first_version(DragDetail::Halfway), first_version(DragDetail::Full));
        assert!(realtime < halfway && halfway < full, "tiles: realtime {realtime}, halfway {halfway}, full {full}");
    }

    /// Everywhere: a surface seen from one view is still that surface from another, so no
    /// triangle spans a jump in depth or sticks out of a face past its edge.
    #[test]
    fn triangles_keep_to_the_surface() {
        let eye = [35.0, -90.0, 40.0];
        let mut failed = false;
        for (name, root) in [("bracket", bracket()), ("ball", ball_before_a_wall())] {
            for still in [false, true] {
                let (d, longest) = strays(&root, view(eye, [0.0; 3]), still);
                assert!(!d.is_empty());
                let worst = d.iter().copied().fold(0.0, f64::max);
                eprintln!("{name} still {still}: {} triangles, worst {worst:.1} px off, longest edge {longest:.1} px", d.len());
                failed |= worst > 6.0 || longest > EDGE_PIXELS + 2.0;
            }
        }
        assert!(!failed, "triangles stray more than 6 px, or run too long");
    }
}
