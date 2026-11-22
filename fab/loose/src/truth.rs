//! A solid's true signed distance, for building its grid: the grid's nodes take these values, so no
//! node carries a field's value over its gradient from afar.
//!
//! A mesh's values are its distance already. A field's only bound it (an ellipsoid's, a blend's,
//! a shell's are not distances), so its truth comes from its surface: the field's zero set is found
//! on a lattice `finest` apart, in the cubes its clearance can't prove empty, as the root on every
//! lattice edge whose ends differ in sign, bracketed on the field, and on every edge a sheet
//! thinner than the step may cross whole, searched until its ends' clearance covers it. Where the
//! lattice's block of 2 x 2 x 2 cubes holds a plane, or two planes meeting at an edge, that is the
//! surface there, cut to the block; elsewhere each seed stands for the surface about it as a disc
//! on its tangent plane. A point's distance is to the nearest of these; where that one's nearest
//! point is off the surface by more than the point's tolerance (a bend, a corner, a face's end
//! not known) the surface is found on the field from there. The seeds are build data (points
//! with their normals, in a tree), dropped with the build: nothing is meshed or shown.

use crate::solid::Solid;
use makepad_csg_mech::GAP_TOL;
use makepad_fab_toolpath::par::{self, Cancelled, Ctl};

/// Root cubes along the box's longest side, at most: a layout constant that changes no distance.
const ROOT_CUBES: usize = 16;
/// Cubes or edges per batch of field values.
const CHUNK: usize = 2048;
/// A seed's bracket is this narrow (mm) when its root is taken.
const SEED_WIDTH: f64 = 1e-6;
/// Seeds per leaf of the tree.
const LEAF_SEEDS: usize = 8;

type P = [f64; 3];

fn sub(a: P, b: P) -> P {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: P, b: P) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn len(a: P) -> f64 {
    dot(a, a).sqrt()
}
fn along(p: P, d: P, t: f64) -> P {
    [p[0] + d[0] * t, p[1] + d[1] * t, p[2] + d[2] * t]
}
fn cross(a: P, b: P) -> P {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn unit(a: P) -> P {
    let l = len(a);
    a.map(|v| v / l)
}

/// A solid's true signed distance (below zero inside).
pub struct Truth<'a> {
    solid: &'a Solid,
    /// Points on the surface: a field's seeds; a mesh's corners (no normal) and its triangles'
    /// middles.
    seeds: Seeds,
    /// The solid's values are its distance (a mesh): no reads from the seeds.
    exact: bool,
    /// The cubes looked into finer than the seeds, by level and place.
    fine: Vec<std::sync::Mutex<std::collections::HashMap<(u32, [i64; 3]), std::sync::Arc<Fine>>>>,
}

#[cfg(test)]
thread_local! {
    /// The reads on this thread that went to [`Truth::bound`].
    static BOUNDED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The finer cubes' map is in this many parts, each behind its own lock.
const FINE_SHARDS: usize = 64;
/// A finer cube's roots are taken to a bracket this narrow (mm).
const FINE_WIDTH: f64 = GAP_TOL / 64.0;
/// The least tolerance a read near an edge or a bend is found to (mm): finer, its cubes would
/// get as fine as their roots' brackets.
const LEAST_TOL: f64 = GAP_TOL / 16.0;

fn shards() -> Vec<std::sync::Mutex<std::collections::HashMap<(u32, [i64; 3]), std::sync::Arc<Fine>>>> {
    (0..FINE_SHARDS).map(|_| Default::default()).collect()
}

/// The seeds' lattice's least corner: off the box's by LATTICE_OFF steps.
fn origin(solid: &Solid, finest: f64) -> P {
    std::array::from_fn(|k| solid.lo[k] - LATTICE_OFF * finest)
}

struct Seeds {
    finest: f64,
    /// The lattice's least corner.
    o: P,
    /// The farthest a point of the surface is from a seed that stands for it: half a lattice
    /// cube's diagonal from a root on its edge, a block's diagonal from a block's one seed.
    reach: f64,
    at: Vec<P>,
    /// Per seed, those of the blocks that hold it (see [`holders`]) that are not planes, as bits:
    /// a seed stands for the surface about it as a disc where any is, and that surface is in them.
    disc: Vec<u8>,
    /// The seeds that stand for a whole block (by index), and the block.
    blocks: Vec<(u32, [i64; 3])>,
    /// Zero where the surface has none (a mesh's corner).
    normal: Vec<P>,
    /// The field's gradient's length at each seed: a value near it over this is how far off the
    /// surface the value's point is.
    slope: Vec<f64>,
    tree: Tree,
    /// The surface where a block holds a plane or two meeting at an edge: polygons, each cut to
    /// its block, with their corners in `corners`.
    patches: Vec<Patch>,
    corners: Vec<P>,
    /// Per corner: what the side from it to the next is.
    sides: Vec<Side>,
    /// Per corner: how near a patch across its side continues it (see [`joins`]); 0 on a side that is
    /// no block's.
    joins: Vec<f64>,
    /// Per corner: the one or two patches that give its `join` (u32::MAX: none).
    covers: Vec<[u32; 2]>,
    /// Each patch's block's centre, in a tree: a patch is within half a block's diagonal of it.
    centres: Vec<P>,
    patch_tree: Tree,
}

/// A flat patch of the surface: a convex polygon on a plane (its unit normal out of the solid and
/// its offset), its corners `corners[from..to]` in turn round the normal, and the field's gradient's
/// length on it.
struct Patch {
    normal: P,
    offset: f64,
    slope: f64,
    from: u32,
    to: u32,
}

/// What a patch's side is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    /// Its block's side: the face runs on past it, or turns at an edge the seeds did not show (a
    /// sheet they missed), so a foot on it is a foot on the surface only as far as a patch across
    /// continues the face (its `join`).
    Block,
    /// Where another plane meets it along an edge of the surface that is [`straight`].
    Edge,
    /// Where another plane cut it: the chord of the surface's own edge there (a rim), which may
    /// be curved.
    Cut,
}

/// A point's nearest patch or disc: its distance, its point nearest the point, whether that may be
/// taken as the point's foot on the surface (not a disc's rim, not a patch's block side), and the
/// field's gradient's length there.
struct Near {
    d: f64,
    foot: P,
    square: bool,
    /// The foot is on a patch's cut side: the surface's own edge there is curved, and may run
    /// past the chord.
    cut: bool,
    /// How far off the face's run past the foot may be: its side's `join` where that is its block's
    /// side, else 0.
    join: f64,
    /// Where the foot is on a block side: the patches that carry the face on past it (its join's).
    cover: [u32; 2],
    slope: f64,
}

impl<'a> Truth<'a> {
    /// The truth of `solid`. A field's surface is found on a lattice `finest` (mm) apart, and a sheet
    /// across any of its edges however thin, to LEAST_TOL: only a feature under `finest` across in
    /// two directions (a needle, a speck, the last tip of a wedge) can sit between the lattice's
    /// lines and go unseen.
    pub fn of(ctl: Ctl, solid: &'a Solid, finest: f64) -> Result<Truth<'a>, Cancelled> {
        let edge = 0.5 * 3f64.sqrt() * finest;
        if let Some(mesh) = &solid.mesh {
            let corner = |i: u32| mesh.data.positions[i as usize].map(|v| v as f64);
            let mut at: Vec<P> = (0..mesh.data.positions.len() as u32).map(corner).collect();
            let mut normal = vec![[0.0; 3]; at.len()];
            for t in &mesh.data.triangles {
                let [a, b, c] = t.map(corner);
                let (u, v) = (sub(b, a), sub(c, a));
                let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
                let l = len(n);
                if l > 0.0 {
                    at.push(std::array::from_fn(|k| (a[k] + b[k] + c[k]) / 3.0));
                    normal.push(n.map(|v| v / l));
                }
            }
            let tree = Tree::new(&at);
            let seeds = Seeds { finest, o: solid.lo, reach: edge, at, disc: Vec::new(), blocks: Vec::new(), normal, slope: Vec::new(), tree, patches: Vec::new(), corners: Vec::new(), sides: Vec::new(), joins: Vec::new(),
                covers: Vec::new(), centres: Vec::new(), patch_tree: Tree::new(&[]) };
            return Ok(Truth { solid, seeds, exact: true, fine: shards() });
        }
        let found = seeds(ctl, solid, finest)?;
        let tree = Tree::new(&found.at);
        let reach = if found.blocks.is_empty() { edge } else { BLOCK_REACH * finest };
        let o = origin(solid, finest);
        let made = patches(ctl, solid, &found, &tree, o, finest)?;
        let patch_tree = Tree::new(&made.centres);
        let seeds = Seeds { finest, o, reach, at: found.at, disc: made.disc, blocks: found.blocks, normal: found.normal, slope: found.slope, tree, patches: made.patches, corners: made.corners, sides: made.sides, joins: made.joins,
            covers: made.covers, centres: made.centres, patch_tree };
        Ok(Truth { solid, seeds, exact: false, fine: shards() })
    }

    /// How many points of a field's surface were found (none for a mesh).
    pub fn seed_count(&self) -> usize {
        if self.exact { 0 } else { self.seeds.at.len() }
    }

    /// Each point of the surface known in the box `lo`..`hi`, with its unit normal (zero where the
    /// surface has none).
    pub fn surface_in(&self, lo: P, hi: P, f: &mut dyn FnMut(P, P)) {
        self.seeds.tree.in_box(&self.seeds.at, lo, hi, &mut |i| f(self.seeds.at[i], self.seeds.normal[i]));
    }

    /// [`Truth::at`] at points in runs of `per`, each run close together (a brick's points): a
    /// mesh's distances are read a run at a time.
    pub fn at_runs(&self, points: &[P], per: usize, tol: &(dyn Fn(f64) -> f64 + Sync), out: &mut [f64]) {
        if self.exact {
            self.solid.values_in_runs(points, per, out);
        } else {
            self.at(points, tol, out);
        }
    }

    /// The signed distance at each point, within `tol(d)` (mm) of the truth `d`.
    pub fn at(&self, points: &[P], tol: &(dyn Fn(f64) -> f64 + Sync), out: &mut [f64]) {
        self.solid.values(points, out);
        let seeds = &self.seeds;
        if self.exact || seeds.at.is_empty() {
            // a distance already, or no surface anywhere: the value is all there is
            return;
        }
        let signed = |v: f64, d: f64| if v < 0.0 { -d } else { d };
        // The nearest seed is a point of the surface, and every point of the surface is within
        // reach of a seed: the truth is at most the seed's distance and at least that less the
        // reach. Far off, the tolerance there covers the reach, and the seed's distance is the
        // read; nearer, the nearest patch or disc.
        let near: Vec<(f64, Option<Near>)> = points.iter().zip(out.iter()).map(|(&x, &v)| {
            let (_, nearest) = seeds.tree.nearest(&seeds.at, x);
            if tol(signed(v, (nearest - seeds.reach).max(0.0))) >= seeds.reach {
                (nearest, None)
            } else {
                (nearest, Some(seeds.near(x, nearest)))
            }
        }).collect();
        let feet: Vec<P> = near.iter().filter_map(|c| c.1.as_ref()).map(|c| c.foot).collect();
        let mut off = vec![0.0; feet.len()];
        self.solid.values(&feet, &mut off);
        // a foot on a block side that its join's whole width keeps out but half would not: the
        // value where its cover comes nearest it (line 2 below), all in one batch
        let mut covered = Vec::new();
        let wants: Vec<bool> = near.iter().zip(out.iter()).filter_map(|((nearest, c), v)| Some((*nearest, c.as_ref()?, *v))).zip(&off).map(|((nearest, c, v), off)| {
            let (t, a) = (tol(signed(v, c.d)), off.abs() / c.slope);
            let want = c.square && !c.cut && c.d <= nearest + 0.5 * t && a + c.join > 0.5 * t && a + 0.5 * c.join <= 0.5 * t;
            if want {
                covered.push(seeds.cover_point(c.foot, c.cover));
            }
            want
        }).collect();
        let mut cover_off = vec![0.0; covered.len()];
        self.solid.values(&covered, &mut cover_off);
        let (mut off, mut wants, mut cover_off) = (off.into_iter(), wants.into_iter(), cover_off.into_iter());
        for ((x, v), (nearest, near)) in points.iter().zip(out.iter_mut()).zip(&near) {
            let d = match near {
                None => *nearest,
                Some(c) => {
                    let t = tol(signed(*v, c.d));
                    let off = off.next().expect("a value at each foot");
                    let cover_off = if wants.next().expect("a want at each foot") { Some(cover_off.next().expect("a value at each cover")) } else { None };
                    let a = off.abs() / c.slope;
                    // A foot on a patch or square under a disc, on the surface to within its
                    // value over the slope there, and no farther than a point of the surface
                    // known: its distance is the truth's to within that. A foot on a disc's rim:
                    // the surface may come nearer past it (an edge, a gap between seeds). A foot
                    // on a patch's block side: the face runs on past it, carried on by its cover
                    // to within the join, and the strip between them is no wider than the join, so
                    // no point of it is farther than half that from a patch. The read is good to
                    // the larger of the two patches' values there over the slope, plus half the
                    // join (line 2, a value at the cover's point nearest the foot); the side's own
                    // value and the whole join need no more (line 1). A foot on a patch's cut
                    // side, the chord of a curved rim: the rim may run past it, so only where the
                    // point's own value over the slope comes to as much.
                    let fits = a + c.join <= 0.5 * t || cover_off.is_some_and(|q| a.max(q.abs() / c.slope) + 0.5 * c.join <= 0.5 * t);
                    if c.square && c.d <= nearest + 0.5 * t && fits && (!c.cut || v.abs() / c.slope >= c.d - 0.5 * t) { c.d } else {
                        #[cfg(test)]
                        BOUNDED.with(|n| n.set(n.get() + 1));
                        self.bound(seeds, *x, t, *v)
                    }
                }
            };
            *v = signed(*v, d);
        }
    }

    /// The distance from `x` (of value `value`) to the surface where its nearest patch or disc is
    /// no good (past an edge, a bend, a rim), to within `t` (no less than LEAST_TOL): the least
    /// over points of the surface, found in finer and finer lattice cubes. It starts from the
    /// lattice cubes about the seeds that could hold a point nearer than the best by more than
    /// `t`; each gets the roots on its half-size cubes' edges, kept for every later read, and
    /// those of its half-size cubes that are not proven clear of the surface go on. A sliver
    /// thinner than a cube can cross none of its edges, so a cube is dropped only on proof: a
    /// corner's value clears a ball round the whole cube. A value's size proves only how far the
    /// surface is at least, never that it is near: an open cube under `t`/2 across with no root
    /// ends there. It ends once nothing left can come within `t` of the best, or the value proves
    /// the surface no nearer than that.
    fn bound(&self, seeds: &Seeds, x: P, t: f64, value: f64) -> f64 {
        let t = t.max(LEAST_TOL);
        let clear = self.solid.clearance.radius(value);
        let (_, mut best) = seeds.tree.nearest(&seeds.at, x);
        // the cubes about every seed that a cube nearer than the best by `t` has on its edges
        let h = seeds.finest;
        let mut start: Vec<[i64; 3]> = Vec::new();
        seeds.tree.within(&seeds.at, x, best - t + seeds.reach.max(3f64.sqrt() * h), &mut |i| {
            if let Ok(b) = seeds.blocks.binary_search_by_key(&(i as u32), |b| b.0) {
                let c = seeds.blocks[b].1;
                start.extend((0..8).map(|q| [c[0] + (q & 1), c[1] + (q >> 1 & 1), c[2] + (q >> 2 & 1)]));
                return;
            }
            let u: P = std::array::from_fn(|k| (seeds.at[i][k] - seeds.o[k]) / h);
            let ways: [Vec<i64>; 3] = std::array::from_fn(|k| {
                let r = u[k].round();
                if (u[k] - r).abs() < 1e-6 { vec![r as i64 - 1, r as i64] } else { vec![u[k].floor() as i64] }
            });
            for &a in &ways[0] {
                for &b in &ways[1] {
                    for &c in &ways[2] {
                        start.push([a, b, c]);
                    }
                }
            }
        });
        start.sort_unstable();
        start.dedup();
        let mut open = std::collections::BinaryHeap::new();
        for c in start {
            let near = seeds.box_distance(x, 0, c);
            if near < best - t {
                open.push(Open(near, 0, c));
            }
        }
        while let Some(Open(near, level, c)) = open.pop() {
            if near >= best - t || best - clear <= t {
                break;
            }
            let fine = self.fine(seeds, level, c);
            for p in &fine.roots {
                best = best.min(len(sub(x, *p)));
            }
            let across = 3f64.sqrt() * h / (1u64 << level) as f64;
            if fine.roots.is_empty() && across < 0.5 * t {
                continue;
            }
            for q in 0..8 {
                if fine.open & 1 << q != 0 {
                    let kid = [2 * c[0] + (q & 1), 2 * c[1] + (q >> 1 & 1), 2 * c[2] + (q >> 2 & 1)];
                    let near = seeds.box_distance(x, level + 1, kid);
                    if near < best - t {
                        open.push(Open(near, level + 1, kid));
                    }
                }
            }
        }
        best
    }

    /// The surface in the lattice cube `c` at `level`: the roots on its half-size cubes' edges,
    /// and which of those cubes are not proven clear of it. Where no edge crosses but a node's
    /// value over its gradient is within the cube's diagonal, the surface may be a sliver between
    /// the nodes: a step along the gradient from the node of least value, and a root there if the
    /// field changes sign about where it lands. Made once and kept: it depends on the cube alone,
    /// so no read depends on which read asked first.
    fn fine(&self, seeds: &Seeds, level: u32, c: [i64; 3]) -> std::sync::Arc<Fine> {
        let shard = &self.fine[(key(c) as usize ^ level as usize).wrapping_mul(0x9e37_79b9) % FINE_SHARDS];
        if let Some(f) = shard.lock().unwrap().get(&(level, c)) {
            return f.clone();
        }
        let h = seeds.finest / (2u64 << level) as f64;
        let nodes: Vec<P> = (0..27i64).map(|q| {
            let l = [q % 3, q / 3 % 3, q / 9];
            std::array::from_fn(|k| seeds.o[k] + (2 * c[k] + l[k]) as f64 * h)
        }).collect();
        let mut v = vec![0.0; 27];
        self.solid.values(&nodes, &mut v);
        let at = |l: [usize; 3]| l[0] + 3 * l[1] + 9 * l[2];
        // a half-size cube is clear when a corner's value clears a ball over all of it
        let across = 3f64.sqrt() * h;
        let clears: Vec<bool> = v.iter().map(|&v| self.solid.clearance.radius(v) >= across).collect();
        let mut open = 0u8;
        for q in 0..8usize {
            if !(0..8).any(|k| clears[at([(q & 1) + (k & 1), (q >> 1 & 1) + (k >> 1 & 1), (q >> 2) + (k >> 2)])]) {
                open |= 1 << q;
            }
        }
        let mut edges = Vec::new();
        for axis in 0..3usize {
            for q in 0..18usize {
                let (s, u, w) = (q % 2, q / 2 % 3, q / 6);
                let mut from = [0usize; 3];
                from[axis] = s;
                from[(axis + 1) % 3] = u;
                from[(axis + 2) % 3] = w;
                let mut to = from;
                to[axis] += 1;
                let (fa, fb) = (v[at(from)], v[at(to)]);
                if (fa < 0.0) != (fb < 0.0) {
                    edges.push((nodes[at(from)], nodes[at(to)], fa, fb));
                }
            }
        }
        if edges.is_empty() {
            let m = (0..27).min_by(|&a, &b| v[a].abs().total_cmp(&v[b].abs())).expect("27 nodes");
            let l = [m % 3, m / 3 % 3, m / 9];
            // the gradient from the nodes beside it, on each axis
            let g: P = std::array::from_fn(|k| {
                let step = |d: usize| {
                    let mut n = l;
                    n[k] = d;
                    v[at(n)]
                };
                match l[k] {
                    0 => (step(1) - step(0)) / h,
                    1 => (step(2) - step(0)) / (2.0 * h),
                    _ => (step(2) - step(1)) / h,
                }
            });
            let gg = dot(g, g);
            if gg > 0.0 && v[m].abs() / gg.sqrt() <= 2.0 * across {
                let landed = along(nodes[m], g, -v[m] / gg);
                let u = g.map(|c| c / gg.sqrt());
                let ends = [along(landed, u, -FINE_WIDTH), along(landed, u, FINE_WIDTH)];
                let mut fv = [0.0; 2];
                self.solid.values(&ends, &mut fv);
                if (fv[0] < 0.0) != (fv[1] < 0.0) {
                    edges.push((ends[0], ends[1], fv[0], fv[1]));
                }
            }
        }
        let fine = std::sync::Arc::new(Fine { roots: edge_roots(self.solid, &edges, FINE_WIDTH), open });
        shard.lock().unwrap().entry((level, c)).or_insert(fine).clone()
    }
}

/// The surface in a lattice cube, finer than the seeds: see [`Truth::fine`].
struct Fine {
    roots: Vec<P>,
    /// Bit q: the half-size cube at octant q (x, y, z bits) is not proven clear of the surface.
    open: u8,
}

/// A cube to look in, by how near it comes to the point: nearest first.
struct Open(f64, u32, [i64; 3]);

impl PartialEq for Open {
    fn eq(&self, o: &Self) -> bool {
        self.cmp(o) == std::cmp::Ordering::Equal
    }
}

impl Eq for Open {}

impl PartialOrd for Open {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for Open {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        o.0.total_cmp(&self.0).then(o.1.cmp(&self.1)).then(o.2.cmp(&self.2))
    }
}

impl Seeds {
    /// How near `x` the lattice cube `c` at `level` comes.
    fn box_distance(&self, x: P, level: u32, c: [i64; 3]) -> f64 {
        let h = self.finest / (1u64 << level) as f64;
        let d: P = std::array::from_fn(|k| {
            let lo = self.o[k] + c[k] as f64 * h;
            (lo - x[k]).max(x[k] - (lo + h)).max(0.0)
        });
        len(d)
    }

    /// The nearest patch or disc to `x`, whose nearest seed is `nearest` away: among the patches
    /// whose block's centre is no farther than that and half the block's diagonal, and the discs
    /// whose seed is no farther than that and a disc's radius, a disc no nearer than the blocks
    /// it stands for the surface in. None nearer: the seed's distance, not square.
    fn near(&self, x: P, nearest: f64) -> Near {
        let mut best = Near { d: f64::MAX, foot: x, square: false, cut: false, join: 0.0, cover: [u32::MAX; 2], slope: 1.0 };
        if !self.patches.is_empty() {
            self.patch_tree.within(&self.centres, x, nearest + 3f64.sqrt() * self.finest, &mut |i| {
                let (d, foot, k) = self.to_patch(x, i);
                let side = k.map(|k| self.sides[k]);
                let (join, cover) = k.map_or((0.0, [u32::MAX; 2]), |k| (self.joins[k], self.covers[k]));
                if d < best.d || d == best.d && join < best.join {
                    best = Near { d, foot, square: true, cut: side == Some(Side::Cut), join, cover, slope: self.patches[i].slope };
                }
            });
        }
        let r = DISC * self.finest;
        self.tree.within(&self.at, x, nearest + r, &mut |i| {
            if self.disc[i] == 0 {
                return;
            }
            let v = sub(x, self.at[i]);
            let n = self.normal[i];
            let h = dot(v, n);
            let lateral = along(v, n, -h);
            let side = len(lateral);
            let d = h.hypot((side - r).max(0.0));
            if d >= best.d {
                return;
            }
            // the surface it stands for is in its blocks that are not planes (the rest is their
            // patches): no nearer than they come, and a foot outside them is not its
            let mut away = f64::MAX;
            holders(self.at[i], self.o, self.finest, &mut |bit, b| {
                if self.disc[i] & bit != 0 {
                    let gap: P = std::array::from_fn(|k| {
                        let lo = self.o[k] + b[k] as f64 * self.finest;
                        (lo - x[k]).max(x[k] - (lo + 2.0 * self.finest)).max(0.0)
                    });
                    away = away.min(len(gap));
                }
            });
            if d.max(away) < best.d {
                let foot = if side <= r { along(x, n, -h) } else { along(self.at[i], lateral, r / side) };
                best = Near { d: d.max(away), foot, square: side <= r && away <= d, cut: false, join: 0.0, cover: [u32::MAX; 2], slope: self.slope[i] };
            }
        });
        best
    }

    /// The point of a block side's cover (its patches, see [`joins`]) nearest `x`.
    fn cover_point(&self, x: P, cover: [u32; 2]) -> P {
        cover.iter().filter(|&&i| i != u32::MAX).map(|&i| self.to_patch(x, i as usize)).min_by(|a, b| a.0.total_cmp(&b.0)).map_or(x, |r| r.1)
    }

    /// The distance from `x` to patch `i`, its point nearest `x`, and the side that is on (by its
    /// first corner): square under `x` if that is within the polygon (no side), else on its
    /// nearest side.
    fn to_patch(&self, x: P, i: usize) -> (f64, P, Option<usize>) {
        let p = &self.patches[i];
        let h = dot(x, p.normal) - p.offset;
        let q = along(x, p.normal, -h);
        let corners = &self.corners[p.from as usize..p.to as usize];
        let side = |k: usize| (corners[k], corners[(k + 1) % corners.len()]);
        if (0..corners.len()).all(|k| {
            let (a, b) = side(k);
            dot(cross(sub(b, a), sub(q, a)), p.normal) >= 0.0
        }) {
            return (h.abs(), q, None);
        }
        let mut best = (f64::MAX, q, None);
        for k in 0..corners.len() {
            let (a, b) = side(k);
            let e = sub(b, a);
            let t = (dot(sub(x, a), e) / dot(e, e).max(f64::MIN_POSITIVE)).clamp(0.0, 1.0);
            let foot = along(a, e, t);
            let d = len(sub(x, foot));
            if d < best.0 {
                best = (d, foot, Some(p.from as usize + k));
            }
        }
        best
    }
}

/// The brackets of a sheet thinner than the lattice's step on `edges` (lattice edges, their ends
/// and values, of one sign): an edge is clear where its ends' empty balls cover it; else it is
/// halved, its middle valued, and a middle of the other sign makes two brackets, while each half
/// of the same sign is tested the same way. A piece not proven clear by LEAST_TOL long is left:
/// a sheet thinner than that can sit on it unseen. An edge a sheet crosses costs about log2 of
/// the step over the sheet's thickness in values; one that runs beside a face, about the step
/// over its distance from the face.
fn sheets(solid: &Solid, mut open: Vec<(P, P, f64, f64)>) -> Vec<(P, P, f64, f64)> {
    let mut brackets = Vec::new();
    let (mut mids, mut values) = (Vec::new(), Vec::new());
    while !open.is_empty() {
        mids.clear();
        mids.extend(open.iter().map(|e| along(e.0, sub(e.1, e.0), 0.5)));
        values.resize(mids.len(), 0.0);
        solid.values(&mids, &mut values);
        let mut next = Vec::new();
        for (&(a, b, va, vb), (&m, &vm)) in open.iter().zip(mids.iter().zip(&values)) {
            if (vm < 0.0) != (va < 0.0) {
                brackets.extend([(a, m, va, vm), (m, b, vm, vb)]);
                continue;
            }
            let half = 0.5 * len(sub(b, a));
            for piece in [(a, m, va, vm), (m, b, vm, vb)] {
                if solid.clearance.radius(piece.2) + solid.clearance.radius(piece.3) < half && half > LEAST_TOL {
                    next.push(piece);
                }
            }
        }
        open = next;
    }
    brackets
}

/// The point where each edge (its ends and their values, of opposite signs) crosses the surface,
/// to a bracket `width` (mm) wide: regula falsi with the Illinois step on all of them together,
/// never a step out of a bracket.
fn edge_roots(solid: &Solid, edges: &[(P, P, f64, f64)], width: f64) -> Vec<P> {
    let mut br: Vec<(f64, f64, f64, f64, i8)> = edges.iter().map(|e| (0.0, 1.0, e.2, e.3, 0)).collect();
    let widths: Vec<f64> = edges.iter().map(|e| width / len(sub(e.1, e.0))).collect();
    let mut live: Vec<usize> = (0..edges.len()).collect();
    let mut at = Vec::new();
    let mut fv = Vec::new();
    for _ in 0..64 {
        live.retain(|&i| br[i].1 - br[i].0 >= widths[i]);
        if live.is_empty() {
            break;
        }
        at.clear();
        let ts: Vec<f64> = live.iter().map(|&i| {
            let (ta, tb, fa, fb, _) = br[i];
            (ta - fa * (tb - ta) / (fb - fa)).clamp(ta, tb)
        }).collect();
        for (&i, &t) in live.iter().zip(&ts) {
            at.push(along(edges[i].0, sub(edges[i].1, edges[i].0), t));
        }
        fv.resize(at.len(), 0.0);
        solid.values(&at, &mut fv);
        for (k, &i) in live.iter().enumerate() {
            let (ta, tb, fa, fb, s) = br[i];
            let (t, ft) = (ts[k], fv[k]);
            br[i] = if ft == 0.0 {
                (t, t, ft, ft, s)
            } else if (ft < 0.0) == (fa < 0.0) {
                (t, tb, ft, if s == -1 { fb * 0.5 } else { fb }, -1)
            } else {
                (ta, t, if s == 1 { fa * 0.5 } else { fa }, ft, 1)
            };
        }
    }
    edges
        .iter()
        .zip(&br)
        .map(|(e, &(ta, tb, fa, fb, _))| {
            let t = if fb == fa { ta } else { (ta - fa * (tb - ta) / (fb - fa)).clamp(ta, tb) };
            along(e.0, sub(e.1, e.0), t)
        })
        .collect()
}

/// The unit normal at a surface point `p` (its value `at`) and the gradient's length there: the
/// gradient points along the normal whatever the field's scale. Straight up where rounding lost it.
fn unit_gradient(solid: &Solid, p: P, at: f64, finest: f64) -> (P, f64) {
    let h = finest * 1e-3;
    let pts: [P; 3] = std::array::from_fn(|k| {
        let mut a = p;
        a[k] += h;
        a
    });
    let mut v = [0.0; 3];
    solid.values(&pts, &mut v);
    let g = v.map(|v| v - at);
    let l = len(g);
    if l > 0.0 {
        (g.map(|c| c / l), l / h)
    } else {
        ([0.0, 0.0, 1.0], 1.0)
    }
}

/// A block of the lattice by its least corner, in lattice steps.
fn key(c: [i64; 3]) -> u64 {
    (c[2] as u64) << 42 | (c[1] as u64) << 21 | c[0] as u64
}

/// Where the lattice starts below the box, in steps: off the box's corner by a fraction of a step
/// no layer height divides, so a part's faces on its layers' planes (a piece cut by layer) never
/// lie along the lattice's planes, where an edge would cross them only at its ends.
const LATTICE_OFF: f64 = std::f64::consts::SQRT_2;

/// The surface of a field part on a lattice `finest` apart: the points where its lattice edges
/// cross it, with the unit normals and the gradient's length there. Blocks of 2 x 2 x 2 cubes are
/// found breadth first from root blocks, a block dropped once its centre's value proves no
/// surface within its half diagonal; each block left takes its 27 nodes' values at once, and the
/// edges between them that cross the surface.
fn seeds(ctl: Ctl, solid: &Solid, finest: f64) -> Result<Found, Cancelled> {
    let o = origin(solid, finest);
    let steps: [i64; 3] = std::array::from_fn(|k| ((solid.hi[k] - solid.lo[k]) / finest + 2.0 * LATTICE_OFF).ceil() as i64);
    let longest = *steps.iter().max().unwrap();
    let mut side: i64 = 2;
    while longest > side * ROOT_CUBES as i64 {
        side *= 2;
    }
    let mut blocks: Vec<[i64; 3]> = Vec::new();
    for z in (0..steps[2]).step_by(side as usize) {
        for y in (0..steps[1]).step_by(side as usize) {
            for x in (0..steps[0]).step_by(side as usize) {
                blocks.push([x, y, z]);
            }
        }
    }
    let point = |c: [i64; 3], off: f64| -> P { std::array::from_fn(|k| o[k] + (c[k] as f64 + off) * finest) };
    loop {
        let half = 0.5 * side as f64;
        let reach = half * finest * 3f64.sqrt();
        let kept = par::try_map(ctl, blocks.len().div_ceil(CHUNK), |b| {
            let part = &blocks[b * CHUNK..((b + 1) * CHUNK).min(blocks.len())];
            let centres: Vec<P> = part.iter().map(|&c| point(c, half)).collect();
            let mut v = vec![0.0; centres.len()];
            solid.values(&centres, &mut v);
            part.iter().zip(v).filter(|(_, v)| solid.clearance.radius(*v) <= reach).map(|(c, _)| *c).collect::<Vec<_>>()
        })?
        .concat();
        if side == 2 {
            blocks = kept;
            break;
        }
        side /= 2;
        // children past the lattice's own extent hold nothing of the box
        blocks = kept.iter().flat_map(|&c| (0..8).map(move |q| [c[0] + (q & 1) * side, c[1] + (q >> 1 & 1) * side, c[2] + (q >> 2 & 1) * side])).filter(|c| (0..3).all(|k| c[k] < steps[k])).collect();
    }
    let found = tile_seeds(ctl, solid, finest, o, &blocks)?;
    blocks.sort_unstable();
    Ok(split_seeds(found, blocks))
}

/// A field's seeds as found, and the blocks the surface may cross (in order).
struct Found {
    at: Vec<P>,
    normal: Vec<P>,
    slope: Vec<f64>,
    blocks: Vec<(u32, [i64; 3])>,
    kept: Vec<[i64; 3]>,
}

/// A seed: where, its unit normal, the gradient's length there, and the block it stands for alone.
type Seed = (P, P, f64, Option<[i64; 3]>);

/// A block's one seed stands for points of its surface this far off, in lattice steps: the
/// block's diagonal.
const BLOCK_REACH: f64 = 2.0 * 1.732_050_807_568_877_2;
/// A disc's radius, in lattice steps: half a cube's diagonal. A surface the lattice crosses
/// cleanly is this near a seed; where it runs along the lattice's nodes or turns at an edge it
/// need not be, so a foot on a disc's rim is never taken as the truth.
const DISC: f64 = 0.5 * 1.732_050_807_568_877_2;

/// The seeds found, a list per tile, as one list.
fn split_seeds(found: Vec<Vec<Seed>>, kept: Vec<[i64; 3]>) -> Found {
    let count = found.iter().map(Vec::len).sum();
    let mut out = Found { at: Vec::with_capacity(count), normal: Vec::with_capacity(count), slope: Vec::with_capacity(count), blocks: Vec::new(), kept };
    for (p, n, s, block) in found.into_iter().flatten() {
        if let Some(b) = block {
            out.blocks.push((out.at.len() as u32, b));
        }
        out.at.push(p);
        out.normal.push(n);
        out.slope.push(s);
    }
    out
}

/// What [`patches`] makes: the patches, their corners and blocks' centres, and which seeds stand
/// for the surface as discs.
struct Made {
    patches: Vec<Patch>,
    corners: Vec<P>,
    sides: Vec<Side>,
    joins: Vec<f64>,
    covers: Vec<[u32; 2]>,
    centres: Vec<P>,
    disc: Vec<u8>,
}

/// The surface where a block holds planes. A flat block's is its plane; another's, where all its
/// seeds are on one to three planes (see [`planes`]), is those planes, meeting at an edge or a
/// corner. Each plane is cut to its block, and to the side of the others its seeds are on. The
/// rest of the surface is discs:
/// a seed stands for the surface about it unless every block that holds it is planes (a flat
/// block's one seed never: its plane stands for its block). A patch's side where another plane
/// cut it is a [`Side::Cut`] (the chord of a rim that may be curved) unless the edge there is
/// [`straight`]; its block's sides are [`Side::Block`], each with its [`joins`].
fn patches(ctl: Ctl, solid: &Solid, found: &Found, tree: &Tree, o: P, finest: f64) -> Result<Made, Cancelled> {
    let kept = &found.kept;
    let mut stands = vec![false; found.at.len()];
    for &(i, _) in &found.blocks {
        stands[i as usize] = true;
    }
    let mut flat: Vec<([i64; 3], u32)> = found.blocks.iter().map(|&(i, b)| (b, i)).collect();
    flat.sort_unstable();
    // per block: whether it is planes; per patch: plane, slope, block centre, corners (with
    // what the side from each is)
    type Planar = (P, f64, f64, P, Vec<(P, Side)>);
    let parts = par::try_map(ctl, kept.len().div_ceil(CHUNK), |c| {
        let part = &kept[c * CHUNK..((c + 1) * CHUNK).min(kept.len())];
        let mut planar = Vec::with_capacity(part.len());
        let mut out: Vec<Planar> = Vec::new();
        let mut members = Vec::new();
        for &b in part {
            let lo: P = std::array::from_fn(|k| o[k] + b[k] as f64 * finest);
            let hi: P = std::array::from_fn(|k| lo[k] + 2.0 * finest);
            let centre: P = std::array::from_fn(|k| lo[k] + finest);
            if let Ok(f) = flat.binary_search_by_key(&b, |f| f.0) {
                let i = flat[f].1 as usize;
                let n = found.normal[i];
                let offset = dot(n, found.at[i]);
                let corners = polygon(n, offset, lo, hi, &[]);
                if corners.len() >= 3 {
                    out.push((n, offset, found.slope[i], centre, corners));
                }
                planar.push(true);
                continue;
            }
            members.clear();
            // the seeds on its sides too, however their coordinates round
            let near = 1e-6 * finest;
            tree.in_box(&found.at, lo.map(|v| v - near), hi.map(|v| v + near), &mut |i| {
                if !stands[i] {
                    members.push(i);
                }
            });
            match planes(found, &members, finest) {
                Some((faces, slope, sign)) => {
                    let mut sure = [[true; 3]; 3];
                    for g in 0..faces.len() {
                        for h in g + 1..faces.len() {
                            let s = straight(solid, &faces, g, h, sign, lo, hi);
                            (sure[g][h], sure[h][g]) = (s, s);
                        }
                    }
                    for (g, (n, offset, cuts)) in faces.iter().enumerate() {
                        let cuts: Vec<(P, f64, Side)> = cuts.iter().zip((0..faces.len()).filter(|&h| h != g)).map(|(&(m, w), h)| (m, w, if sure[g][h] { Side::Edge } else { Side::Cut })).collect();
                        let corners = polygon(*n, *offset, lo, hi, &cuts);
                        if corners.len() >= 3 {
                            out.push((*n, *offset, slope, centre, corners));
                        }
                    }
                    planar.push(true);
                }
                None => planar.push(false),
            }
        }
        (planar, out)
    })?;
    let mut planar = Vec::with_capacity(kept.len());
    let mut made = Made { patches: Vec::new(), corners: Vec::new(), sides: Vec::new(), joins: Vec::new(), covers: Vec::new(), centres: Vec::new(), disc: Vec::new() };
    for (p, out) in parts {
        planar.extend(p);
        for (normal, offset, slope, centre, corners) in out {
            let from = made.corners.len() as u32;
            made.corners.extend(corners.iter().map(|c| c.0));
            made.sides.extend(corners.iter().map(|c| c.1));
            made.patches.push(Patch { normal, offset, slope, from, to: made.corners.len() as u32 });
            made.centres.push(centre);
        }
    }
    made.disc = par::try_map(ctl, found.at.len().div_ceil(CHUNK), |c| {
        (c * CHUNK..((c + 1) * CHUNK).min(found.at.len())).map(|i| {
            let mut rough = 0;
            if !stands[i] {
                holders(found.at[i], o, finest, &mut |bit, b| {
                    if !kept.binary_search(&b).is_ok_and(|k| planar[k]) {
                        rough |= bit;
                    }
                });
            }
            rough
        }).collect::<Vec<u8>>()
    })?
    .concat();
    (made.joins, made.covers) = joins(&made, kept, &planar, o, finest);
    Ok(made)
}

/// The distance from `x` to patch `p` (its polygon of `corners`): to its plane under `x` where that
/// is within the polygon, else to its nearest side.
fn polygon_distance(p: &Patch, corners: &[P], x: P) -> f64 {
    let h = dot(x, p.normal) - p.offset;
    let q = along(x, p.normal, -h);
    let n = corners.len();
    let side = |k: usize| (corners[k], corners[(k + 1) % n]);
    if (0..n).all(|k| {
        let (a, b) = side(k);
        dot(cross(sub(b, a), sub(q, a)), p.normal) >= 0.0
    }) {
        return h.abs();
    }
    (0..n).map(|k| {
        let (a, b) = side(k);
        let e = sub(b, a);
        let t = (dot(sub(x, a), e) / dot(e, e).max(f64::MIN_POSITIVE)).clamp(0.0, 1.0);
        len(sub(x, along(a, e, t)))
    }).fold(f64::MAX, f64::min)
}

/// Per corner of `made`'s patches, how far any point of the side from it to the next is from a
/// patch that carries the face on past it (mm). The patches that may: those of the planar kept
/// blocks across the side's face of its block and across that face's four edges, whole polygons.
/// One cover: the least, over them, of the larger of the side's corners' distances to one (the
/// distance to a convex polygon is convex along the side: its corners give its worst point). Two
/// covers: over pairs, split at the side's point nearest a corner of either, the largest of the
/// first corner's and the split's distances to one and the split's and the second corner's to the
/// other. Infinite where none covers it: the face's run past the side is unknown. 0 on a side that
/// is no block's. With each, the one or two patches that give it (u32::MAX: none).
fn joins(made: &Made, kept: &[[i64; 3]], planar: &[bool], o: P, finest: f64) -> (Vec<f64>, Vec<[u32; 2]>) {
    let block = |c: P| -> [i64; 3] { std::array::from_fn(|k| ((c[k] - o[k]) / finest - 1.0).round() as i64) };
    let mut by_block: Vec<([i64; 3], u32)> = made.centres.iter().enumerate().map(|(i, &c)| (block(c), i as u32)).collect();
    by_block.sort_unstable();
    let near = 1e-6 * finest;
    let polygon = |i: usize| {
        let p = &made.patches[i];
        (p, &made.corners[p.from as usize..p.to as usize])
    };
    let (mut joins, mut covers) = (vec![0.0; made.corners.len()], vec![[u32::MAX; 2]; made.corners.len()]);
    let mut candidates = Vec::new();
    for (i, p) in made.patches.iter().enumerate() {
        let b = block(made.centres[i]);
        let lo: P = std::array::from_fn(|k| o[k] + b[k] as f64 * finest);
        let hi: P = std::array::from_fn(|k| lo[k] + 2.0 * finest);
        let (from, n) = (p.from as usize, (p.to - p.from) as usize);
        for k in 0..n {
            if made.sides[from + k] != Side::Block {
                continue;
            }
            let (a, c) = (made.corners[from + k], made.corners[from + (k + 1) % n]);
            let face = (0..3).flat_map(|x| [(x, false), (x, true)]).find(|&(x, up)| {
                let w = if up { hi[x] } else { lo[x] };
                (a[x] - w).abs() < near && (c[x] - w).abs() < near
            });
            let mut join = f64::INFINITY;
            let mut cover = [u32::MAX; 2];
            if let Some((axis, up)) = face {
                let mut across = b;
                across[axis] += if up { 2 } else { -2 };
                candidates.clear();
                let mut blocks = vec![across];
                for x in (0..3).filter(|&x| x != axis) {
                    for d in [-2, 2] {
                        let mut q = across;
                        q[x] += d;
                        blocks.push(q);
                    }
                }
                for q in blocks {
                    if kept.binary_search(&q).is_ok_and(|w| planar[w]) {
                        let start = by_block.partition_point(|e| e.0 < q);
                        candidates.extend(by_block[start..].iter().take_while(|e| e.0 == q).map(|e| e.1 as usize));
                    }
                }
                // each candidate's distance from the side's two corners
                let ends: Vec<(f64, f64)> = candidates.iter().map(|&q| {
                    let (pq, cq) = polygon(q);
                    (polygon_distance(pq, cq, a), polygon_distance(pq, cq, c))
                }).collect();
                for (u, e) in ends.iter().enumerate() {
                    if e.0.max(e.1) < join {
                        join = e.0.max(e.1);
                        cover = [candidates[u] as u32, u32::MAX];
                    }
                }
                if join > 0.5 * LEAST_TOL {
                    let e = sub(c, a);
                    let ee = dot(e, e).max(f64::MIN_POSITIVE);
                    for (u, &qu) in candidates.iter().enumerate() {
                        if ends[u].0 >= join {
                            continue;
                        }
                        for (v, &qv) in candidates.iter().enumerate() {
                            if u == v || ends[v].1 >= join {
                                continue;
                            }
                            let ((pu, cu), (pv, cv)) = (polygon(qu), polygon(qv));
                            for &corner in cu.iter().chain(cv.iter()) {
                                let split = along(a, e, (dot(sub(corner, a), e) / ee).clamp(0.0, 1.0));
                                let worst = ends[u].0.max(ends[v].1).max(polygon_distance(pu, cu, split)).max(polygon_distance(pv, cv, split));
                                if worst < join {
                                    join = worst;
                                    cover = [qu as u32, qv as u32];
                                }
                            }
                        }
                    }
                }
            }
            joins[from + k] = join;
            covers[from + k] = cover;
        }
    }
    (joins, covers)
}

/// The blocks (2 x 2 x 2 cubes of the lattice from `o`, `finest` apart) that hold the point `at`:
/// one along a lattice edge it is on, one or two across on each side; each with its own bit.
fn holders(at: P, o: P, finest: f64, f: &mut dyn FnMut(u8, [i64; 3])) {
    let ways: [([i64; 2], usize); 3] = std::array::from_fn(|k| {
        let u = (at[k] - o[k]) / finest;
        let r = u.round();
        if (u - r).abs() < 1e-6 {
            let c = r as i64;
            if c.rem_euclid(2) != 0 { ([c - 1, 0], 1) } else { ([c - 2, c], 2) }
        } else {
            let c = u.floor() as i64;
            ([c - c.rem_euclid(2), 0], 1)
        }
    });
    for (a, &x) in ways[0].0[..ways[0].1].iter().enumerate() {
        for (b, &y) in ways[1].0[..ways[1].1].iter().enumerate() {
            for (c, &z) in ways[2].0[..ways[2].1].iter().enumerate() {
                f(1 << (a + 2 * b + 4 * c), [x, y, z]);
            }
        }
    }
}

/// Two planes turned less than this apart (the cosine) are one to a block of a lattice `finest`
/// apart: a quarter of a cube along the plane, they part by GAP_TOL / 4 at most.
fn turn(finest: f64) -> f64 {
    (2.0 * (GAP_TOL / finest).atan()).cos()
}

/// A face of a block's planes: its plane (unit normal out of the solid, offset), and the half
/// spaces (normal, offset: the points p with normal . p <= offset) of the other planes it keeps to.
type Face = (P, f64, Vec<(P, f64)>);

/// The planes, one to three, the seeds `members` of a block are on, if they are: the seed least
/// like the first, then the one least like both, start a group while it is turned more than
/// [`turn`] from every group's normal; each seed joins the group whose normal is nearest its own,
/// and each group's plane is its seeds' mean normal through their mean offset. Every seed must be
/// within FLAT x GAP_TOL of its group's plane with its normal within [`turn`] of the plane's, two
/// at least in each group (three for one plane), and the planes turned more than that apart. Each
/// group lies behind every other plane (a solid's edge or corner: the solid is behind them all) or
/// in front of every one (a groove: the solid is behind any); its face is cut to that side. Also
/// the least slope among the seeds, and 1 for a solid's edge or corner, -1 for a groove.
fn planes(found: &Found, members: &[usize], finest: f64) -> Option<(Vec<Face>, f64, f64)> {
    if members.len() < 3 {
        return None;
    }
    let turn = turn(finest);
    let normal = |k: usize| found.normal[members[k]];
    let mut groups: Vec<P> = vec![normal(0)];
    loop {
        let likest = |k: usize| groups.iter().map(|&g| dot(normal(k), g)).fold(f64::MIN, f64::max);
        let (k, like) = (0..members.len()).map(|k| (k, likest(k))).min_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
        if like >= turn {
            break;
        }
        if groups.len() == 3 {
            return None;
        }
        groups.push(normal(k));
    }
    let mut group = vec![0usize; members.len()];
    let least = if groups.len() == 1 { 3 } else { 2 };
    for _ in 0..3 {
        for (k, g) in group.iter_mut().enumerate() {
            *g = (0..groups.len()).max_by(|&a, &b| dot(normal(k), groups[a]).total_cmp(&dot(normal(k), groups[b]))).unwrap();
        }
        for (g, n) in groups.iter_mut().enumerate() {
            let mut sum = [0.0; 3];
            let mut count = 0;
            for k in (0..members.len()).filter(|&k| group[k] == g) {
                sum = along(sum, normal(k), 1.0);
                count += 1;
            }
            if count < least || len(sum) == 0.0 {
                return None;
            }
            *n = unit(sum);
        }
    }
    for a in 0..groups.len() {
        for b in a + 1..groups.len() {
            if dot(groups[a], groups[b]) >= turn {
                return None;
            }
        }
    }
    let offsets: Vec<f64> = (0..groups.len()).map(|g| {
        let on: Vec<f64> = (0..members.len()).filter(|&k| group[k] == g).map(|k| dot(groups[g], found.at[members[k]])).collect();
        on.iter().sum::<f64>() / on.len() as f64
    }).collect();
    let tol = FLAT * GAP_TOL;
    let (mut behind, mut ahead, mut sum) = (true, true, 0.0);
    for k in 0..members.len() {
        let p = found.at[members[k]];
        let g = group[k];
        if (dot(groups[g], p) - offsets[g]).abs() > tol || dot(normal(k), groups[g]) < turn {
            return None;
        }
        for h in (0..groups.len()).filter(|&h| h != g) {
            let v = dot(groups[h], p) - offsets[h];
            behind &= v <= tol;
            ahead &= v >= -tol;
            sum += v;
        }
    }
    let s = match (behind, ahead) {
        (true, true) => if sum < 0.0 { 1.0 } else { -1.0 },
        (true, false) => 1.0,
        (false, true) => -1.0,
        (false, false) => return None,
    };
    let faces = (0..groups.len()).map(|g| {
        let cuts = (0..groups.len()).filter(|&h| h != g).map(|h| (groups[h].map(|v| v * s), offsets[h] * s)).collect();
        (groups[g], offsets[g], cuts)
    }).collect();
    let slope = members.iter().map(|&i| found.slope[i]).fold(f64::MAX, f64::min);
    Some((faces, slope, s))
}

/// Whether the edge where faces `g` and `h` of a block's planes meet (in the box `lo`..`hi`, and on
/// the kept side of a third face) is where the surface's own edge is, by the field's signs: at five
/// stations along it, the ends and the quarters, a point a little way into each of the four sectors
/// the two planes make has the sign the planes give it (`sign` 1: the solid is behind them all, -1:
/// behind any). A sign is exact whatever the field's values are (a carve's, a union's, a mesh's),
/// so a straight edge passes however its field was built; a curved rim's chord fails, the face in
/// one sector or the hole in another. A notch or bump narrower than a station's spacing passes, as
/// any feature finer than the lattice may go unseen. No edge in the block: nothing to flag.
fn straight(solid: &Solid, faces: &[Face], g: usize, h: usize, sign: f64, lo: P, hi: P) -> bool {
    let ((a, oa, _), (b, ob, _)) = (&faces[g], &faces[h]);
    let Some((mut from, mut to)) = edge_in(*a, *oa, *b, *ob, lo, hi) else { return true };
    for (n, o, _) in faces.iter().enumerate().filter(|&(k, _)| k != g && k != h).map(|(_, f)| f) {
        let (m, w) = (n.map(|v| v * sign), o * sign);
        let (da, db) = (dot(m, from) - w, dot(m, to) - w);
        if da > 0.0 && db > 0.0 {
            return true;
        }
        if da > 0.0 || db > 0.0 {
            let at = along(from, sub(to, from), da / (da - db));
            if da > 0.0 { from = at } else { to = at }
        }
    }
    let delta = 0.5 * LEAST_TOL;
    let ways = [unit(along(*a, *b, 1.0)), unit(sub(*a, *b))];
    let model = |q: P| {
        let each = faces.iter().map(|(n, o, _)| dot(*n, q) - o);
        if sign > 0.0 { each.fold(f64::MIN, f64::max) } else { each.fold(f64::MAX, f64::min) }
    };
    let probes: Vec<P> = (0..5).flat_map(|i| {
        let c = along(from, sub(to, from), i as f64 / 4.0);
        ways.iter().flat_map(move |&e| [along(c, e, delta), along(c, e, -delta)])
    }).collect();
    let mut v = vec![0.0; probes.len()];
    solid.values(&probes, &mut v);
    // a probe on a third plane (at a corner) says nothing
    probes.iter().zip(&v).all(|(&q, &v)| {
        let m = model(q);
        m.abs() < delta / 8.0 || (v > 0.0 && m > 0.0) || (v < 0.0 && m < 0.0)
    })
}

/// Where the planes a . p = `oa` and b . p = `ob` meet in the box `lo`..`hi`: the ends of the
/// segment, if they meet there.
fn edge_in(a: P, oa: f64, b: P, ob: f64, lo: P, hi: P) -> Option<(P, P)> {
    let c = dot(a, b);
    let k = 1.0 - c * c;
    if k <= 0.0 {
        return None;
    }
    let p = along(a.map(|v| v * (oa - c * ob) / k), b, (ob - c * oa) / k);
    let d = unit(cross(a, b));
    let (mut t0, mut t1) = (f64::MIN, f64::MAX);
    for i in 0..3 {
        if d[i] == 0.0 {
            if p[i] < lo[i] || p[i] > hi[i] {
                return None;
            }
            continue;
        }
        let (u, v) = ((lo[i] - p[i]) / d[i], (hi[i] - p[i]) / d[i]);
        t0 = t0.max(u.min(v));
        t1 = t1.min(u.max(v));
    }
    (t0 < t1).then(|| (along(p, d, t0), along(p, d, t1)))
}

/// The plane `normal` . p = `offset` in the box `lo`..`hi`, on the side of each of `cuts` (with
/// what the side it makes is): a convex polygon's corners in turn round the normal, each with what
/// its side to the next is (fewer than three if it misses).
fn polygon(normal: P, offset: f64, lo: P, hi: P, cuts: &[(P, f64, Side)]) -> Vec<(P, Side)> {
    let mid: P = std::array::from_fn(|k| 0.5 * (lo[k] + hi[k]));
    let c = along(mid, normal, offset - dot(normal, mid));
    let axis = (0..3).min_by(|&a, &b| normal[a].abs().total_cmp(&normal[b].abs())).unwrap();
    let mut e = [0.0; 3];
    e[axis] = 1.0;
    let u = unit(cross(normal, e));
    let v = cross(normal, u);
    let r = 2.0 * len(sub(hi, lo));
    // a square holding the plane's section of the box: the box's planes cut all its sides away
    let mut corners: Vec<(P, Side)> = [(1.0, 1.0), (-1.0, 1.0), (-1.0, -1.0), (1.0, -1.0)].iter().map(|&(a, b)| (along(along(c, u, a * r), v, b * r), Side::Block)).collect();
    for k in 0..3 {
        let mut m = [0.0; 3];
        m[k] = 1.0;
        corners = clip(&corners, m, hi[k], Side::Block);
        m[k] = -1.0;
        corners = clip(&corners, m, -lo[k], Side::Block);
    }
    for &(m, w, side) in cuts {
        corners = clip(&corners, m, w, side);
    }
    corners
}

/// The polygon `corners` on the side m . p <= w of a plane, its corners still in turn, each with
/// what its side to the next is: the new side along the plane is a `cut` one.
fn clip(corners: &[(P, Side)], m: P, w: f64, cut: Side) -> Vec<(P, Side)> {
    let mut out = Vec::with_capacity(corners.len() + 1);
    for k in 0..corners.len() {
        let ((a, side), (b, _)) = (corners[k], corners[(k + 1) % corners.len()]);
        let (da, db) = (dot(m, a) - w, dot(m, b) - w);
        if da <= 0.0 {
            out.push((a, side));
        }
        if (da <= 0.0) != (db <= 0.0) {
            // leaving, the side runs along the plane; coming back, along a to b again
            out.push((along(a, sub(b, a), da / (da - db)), if da <= 0.0 { cut } else { side }));
        }
    }
    out
}

/// Lattice steps along a side of a tile: the blocks left in one are valued together, each node
/// once, and it takes the edges that start in it.
const TILE: i64 = 32;

/// A block whose values are a plane's to within this share of GAP_TOL (over their gradient) is
/// flat, and takes one seed.
const FLAT: f64 = 0.25;

/// The seeds of `blocks` (2 x 2 x 2 cubes of the lattice from `o`, `finest` apart, left by the
/// descent), a tile at a time across the workers. A block whose 27 values are a plane's to
/// within FLAT x GAP_TOL takes one seed, the plane's foot from its centre put on the field; the
/// rest take the roots on the edges between their nodes that cross the surface, a tile taking
/// those that start in it. Every edge of a kept block whose ends have one sign is proven clear
/// or searched for a sheet that crosses it whole ([`sheets`]), and that sheet's roots are seeds
/// like the rest. A tile values each node of its blocks once, and also the blocks of
/// the tiles below that share its sides, to know whether an edge on them is a flat block's.
fn tile_seeds(ctl: Ctl, solid: &Solid, finest: f64, o: P, blocks: &[[i64; 3]]) -> Result<Vec<Vec<Seed>>, Cancelled> {
    let mut sorted: Vec<([i64; 3], [i64; 3])> = blocks.iter().map(|&b| (b.map(|v| v.div_euclid(TILE)), b)).collect();
    sorted.sort_unstable();
    let mut kept: Vec<[i64; 3]> = blocks.to_vec();
    kept.sort_unstable();
    let mut tiles = Vec::new();
    let mut from = 0;
    for i in 1..=sorted.len() {
        if i == sorted.len() || sorted[i].0 != sorted[from].0 {
            tiles.push(from..i);
            from = i;
        }
    }
    // nodes from 2 steps below a tile to its far side; blocks from one below
    const N: usize = TILE as usize + 3;
    const B: usize = TILE as usize / 2 + 1;
    par::try_map(ctl, tiles.len(), |t| {
        let part = &sorted[tiles[t].clone()];
        let base = part[0].0.map(|v| v * TILE);
        let index = |l: [i64; 3]| (l[0] + 2) as usize + N * ((l[1] + 2) as usize + N * (l[2] + 2) as usize);
        let block_index = |c: [i64; 3]| -> usize {
            let l: [usize; 3] = std::array::from_fn(|k| ((c[k] - base[k]) / 2 + 1) as usize);
            l[0] + B * (l[1] + B * l[2])
        };
        let mut below = Vec::new();
        for lower in 1..8usize {
            let ranges: [Vec<i64>; 3] = std::array::from_fn(|k| if lower & 1 << k != 0 { vec![base[k] - 2] } else { (0..TILE / 2).map(|i| base[k] + 2 * i).collect() });
            for &x in &ranges[0] {
                for &y in &ranges[1] {
                    for &z in &ranges[2] {
                        if kept.binary_search(&[x, y, z]).is_ok() {
                            below.push([x, y, z]);
                        }
                    }
                }
            }
        }
        let mut slot = vec![u32::MAX; N * N * N];
        let (mut pts, mut local) = (Vec::new(), Vec::new());
        let all: Vec<[i64; 3]> = part.iter().map(|b| b.1).chain(below.iter().copied()).collect();
        for b in &all {
            for q in 0..27i64 {
                let l = [b[0] - base[0] + q % 3, b[1] - base[1] + q / 3 % 3, b[2] - base[2] + q / 9];
                let i = index(l);
                if slot[i] == u32::MAX {
                    slot[i] = pts.len() as u32;
                    pts.push(std::array::from_fn(|k| o[k] + (base[k] + l[k]) as f64 * finest));
                    local.push(l);
                }
            }
        }
        let mut v = vec![0.0; pts.len()];
        solid.values(&pts, &mut v);
        // each block: absent, kept, or flat with its plane's value at its centre and gradient
        let mut state = vec![0u8; B * B * B];
        let mut flats: Vec<([i64; 3], f64, P)> = Vec::new();
        for (bi, b) in all.iter().enumerate() {
            let value = |q: i64| v[slot[index([b[0] - base[0] + q % 3, b[1] - base[1] + q / 3 % 3, b[2] - base[2] + q / 9])] as usize];
            let mut flat = None;
            let mean = (0..27).map(value).sum::<f64>() / 27.0;
            let g: P = std::array::from_fn(|k| (0..27i64).map(|q| value(q) * ([q % 3, q / 3 % 3, q / 9][k] - 1) as f64).sum::<f64>() / (18.0 * finest));
            let gl = len(g);
            let off = (0..27i64).map(|q| {
                let l = [q % 3, q / 3 % 3, q / 9];
                (value(q) - mean - (0..3).map(|k| g[k] * (l[k] - 1) as f64 * finest).sum::<f64>()).abs()
            }).fold(0.0, f64::max);
            let crossed = (0..27).any(|q| value(q) < 0.0) && (0..27).any(|q| value(q) >= 0.0);
            // the plane's foot from the centre, in steps from it
            let foot: P = std::array::from_fn(|k| -mean * g[k] / (gl * gl).max(f64::MIN_POSITIVE) / finest);
            if crossed && gl > 0.0 && off <= FLAT * GAP_TOL * gl && foot.iter().all(|f| f.abs() <= 1.0) {
                flat = Some((mean, g));
            }
            state[block_index(*b)] = if flat.is_some() { 2 } else { 1 };
            if let (Some((mean, g)), true) = (flat, bi < part.len()) {
                flats.push((*b, mean, g));
            }
        }
        let mut edges: Vec<(P, P, f64, f64)> = Vec::new();
        let mut unproven: Vec<(P, P, f64, f64)> = Vec::new();
        for (n, l) in local.iter().enumerate() {
            if l.iter().any(|&c| c < 0 || c >= TILE) {
                // it starts in another tile
                continue;
            }
            for axis in 0..3 {
                let mut m = *l;
                m[axis] += 1;
                let j = slot[index(m)];
                if j == u32::MAX {
                    continue;
                }
                let (a, b) = (v[n], v[j as usize]);
                let crossed = (a < 0.0) != (b < 0.0);
                // its ends' empty balls cover it: no sheet crosses it
                if !crossed && solid.clearance.radius(a) + solid.clearance.radius(b) >= finest {
                    continue;
                }
                // the blocks that hold the edge: one along it, one or two across on each side
                let ways: [Vec<i64>; 3] = std::array::from_fn(|k| {
                    let c = base[k] + l[k];
                    if k == axis || c % 2 != 0 { vec![c - c.rem_euclid(2)] } else { vec![c - 2, c] }
                });
                let (mut other, mut held) = (false, false);
                for &x in &ways[0] {
                    for &y in &ways[1] {
                        for &z in &ways[2] {
                            let s = state[block_index([x, y, z])];
                            other |= s == 1;
                            held |= s != 0;
                        }
                    }
                }
                if crossed && other {
                    edges.push((pts[n], pts[j as usize], a, b));
                } else if !crossed && held {
                    // a sheet thinner than the step may cross it whole, between its ends
                    unproven.push((pts[n], pts[j as usize], a, b));
                }
            }
        }
        edges.extend(sheets(solid, unproven));
        let roots = edge_roots(solid, &edges, SEED_WIDTH);
        // a flat block's seed: its plane's foot from its centre, on the field along the normal
        let feet: Vec<(P, P, f64)> = flats.iter().map(|&(b, mean, g)| {
            let gl = len(g);
            let n = g.map(|c| c / gl);
            let centre: P = std::array::from_fn(|k| o[k] + (b[k] + 1) as f64 * finest);
            (along(centre, n, -mean / gl), n, gl)
        }).collect();
        let reach = FLAT * GAP_TOL;
        let ends: Vec<P> = feet.iter().flat_map(|&(p, n, _)| [along(p, n, -reach), along(p, n, reach)]).collect();
        let mut ev = vec![0.0; ends.len()];
        solid.values(&ends, &mut ev);
        let mut bracketed = Vec::new();
        let mut put = Vec::with_capacity(feet.len());
        for (i, &(p, _, _)) in feet.iter().enumerate() {
            if (ev[2 * i] < 0.0) != (ev[2 * i + 1] < 0.0) {
                put.push(None);
                bracketed.push((ends[2 * i], ends[2 * i + 1], ev[2 * i], ev[2 * i + 1]));
            } else {
                // the field bends away from the plane between the nodes: the foot is as near as known
                put.push(Some(p));
            }
        }
        let mut on_field = edge_roots(solid, &bracketed, SEED_WIDTH).into_iter();
        let mut on = vec![0.0; roots.len()];
        solid.values(&roots, &mut on);
        let mut out: Vec<Seed> = roots.iter().zip(&on).map(|(&p, &f)| {
            let (n, slope) = unit_gradient(solid, p, f, finest);
            (p, n, slope, None)
        }).collect();
        for (&(b, _, _), (&(_, n, gl), p)) in flats.iter().zip(feet.iter().zip(put)) {
            let p = p.unwrap_or_else(|| on_field.next().expect("a root for each bracket"));
            out.push((p, n, gl, Some(b)));
        }
        out
    })
}

/// A walk's nodes still to visit. The tree halves its seeds at each level, and seeds are counted
/// in u32, so it is under 32 levels deep; a walk holds at most one node a level besides the
/// two children it just pushed.
struct Stack {
    n: usize,
    at: [u32; 64],
}

impl Stack {
    /// The root, to visit first.
    fn root() -> Stack {
        Stack { n: 1, at: [0; 64] }
    }

    fn push(&mut self, node: u32) {
        self.at[self.n] = node;
        self.n += 1;
    }

    fn pop(&mut self) -> Option<u32> {
        self.n = self.n.checked_sub(1)?;
        Some(self.at[self.n])
    }
}

/// The seeds in a tree of boxes, for the nearest and for all within a reach.
struct Tree {
    order: Vec<u32>,
    nodes: Vec<Node>,
}

struct Node {
    lo: P,
    hi: P,
    /// seeds order[start..end]
    start: u32,
    end: u32,
    /// children, none for a leaf
    kids: Option<(u32, u32)>,
}

impl Tree {
    fn new(at: &[P]) -> Tree {
        let mut t = Tree { order: (0..at.len() as u32).collect(), nodes: Vec::new() };
        if !at.is_empty() {
            t.split(at, 0, at.len());
        }
        t
    }

    fn split(&mut self, at: &[P], start: usize, end: usize) -> u32 {
        let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
        for &i in &self.order[start..end] {
            for k in 0..3 {
                lo[k] = lo[k].min(at[i as usize][k]);
                hi[k] = hi[k].max(at[i as usize][k]);
            }
        }
        let me = self.nodes.len() as u32;
        self.nodes.push(Node { lo, hi, start: start as u32, end: end as u32, kids: None });
        if end - start > LEAF_SEEDS {
            let axis = (0..3).max_by(|&a, &b| (hi[a] - lo[a]).total_cmp(&(hi[b] - lo[b]))).unwrap();
            let mid = (start + end) / 2;
            self.order[start..end].select_nth_unstable_by(mid - start, |&a, &b| at[a as usize][axis].total_cmp(&at[b as usize][axis]));
            let l = self.split(at, start, mid);
            let r = self.split(at, mid, end);
            self.nodes[me as usize].kids = Some((l, r));
        }
        me
    }

    fn box_dist(n: &Node, x: P) -> f64 {
        let d: P = std::array::from_fn(|k| (n.lo[k] - x[k]).max(x[k] - n.hi[k]).max(0.0));
        len(d)
    }

    /// The nearest seed and its distance.
    fn nearest(&self, at: &[P], x: P) -> (usize, f64) {
        let mut best = (0usize, f64::MAX);
        let mut stack = Stack::root();
        while let Some(n) = stack.pop() {
            let node = &self.nodes[n as usize];
            if Self::box_dist(node, x) >= best.1 {
                continue;
            }
            match node.kids {
                None => {
                    for &i in &self.order[node.start as usize..node.end as usize] {
                        let d = len(sub(x, at[i as usize]));
                        if d < best.1 {
                            best = (i as usize, d);
                        }
                    }
                }
                Some((l, r)) => {
                    // the nearer child last, so it is searched first
                    let (dl, dr) = (Self::box_dist(&self.nodes[l as usize], x), Self::box_dist(&self.nodes[r as usize], x));
                    if dl < dr {
                        stack.push(r);
                        stack.push(l);
                    } else {
                        stack.push(l);
                        stack.push(r);
                    }
                }
            }
        }
        best
    }

    /// Every seed in the box `lo`..`hi`.
    fn in_box(&self, at: &[P], lo: P, hi: P, f: &mut dyn FnMut(usize)) {
        if self.nodes.is_empty() {
            return;
        }
        let mut stack = Stack::root();
        while let Some(n) = stack.pop() {
            let node = &self.nodes[n as usize];
            if (0..3).any(|k| node.hi[k] < lo[k] || node.lo[k] > hi[k]) {
                continue;
            }
            match node.kids {
                None => {
                    for &i in &self.order[node.start as usize..node.end as usize] {
                        let p = at[i as usize];
                        if (0..3).all(|k| p[k] >= lo[k] && p[k] <= hi[k]) {
                            f(i as usize);
                        }
                    }
                }
                Some((l, r)) => {
                    stack.push(l);
                    stack.push(r);
                }
            }
        }
    }

    /// Every seed within `reach` of `x`.
    fn within(&self, at: &[P], x: P, reach: f64, f: &mut dyn FnMut(usize)) {
        let mut stack = Stack::root();
        while let Some(n) = stack.pop() {
            let node = &self.nodes[n as usize];
            if Self::box_dist(node, x) > reach {
                continue;
            }
            match node.kids {
                None => {
                    for &i in &self.order[node.start as usize..node.end as usize] {
                        if len(sub(x, at[i as usize])) <= reach {
                            f(i as usize);
                        }
                    }
                }
                Some((l, r)) => {
                    stack.push(l);
                    stack.push(r);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_csg_sdf::{SdfNode, SdfOp, SdfShape};

    /// The surface's lattice: the reads near it are found on the field, so they hold at any.
    const FINEST: f64 = 0.4;

    fn field(root: SdfNode, half: P) -> Solid {
        let shape = SdfShape { root, min: half.map(|v| -v), max: half, materials: vec![] };
        Solid::of_field(&shape, [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]], None)
    }

    /// The distance from (x, z) to the ellipse x^2/a^2 + z^2/c^2 = 1, by a fine walk round it.
    fn ellipse_distance(x: f64, z: f64, a: f64, c: f64) -> f64 {
        (0..=200_000).map(|i| {
            let t = i as f64 / 200_000.0 * std::f64::consts::TAU;
            (x - a * t.cos()).hypot(z - c * t.sin())
        }).fold(f64::MAX, f64::min)
    }

    /// The truth where a grid read the field's value over its gradient: a box with a flat
    /// ellipsoid cut out, its hollow narrowing to a 0.3 neck where its rim breaks the wall. Then
    /// a sphere and a box as fields, at points scattered about their surfaces; and the surface
    /// found is a few seeds per lattice cube it crosses, not a meshing of the volume.
    #[test]
    fn a_field_reads_its_true_distance_inside_a_flat_hollow_and_about_a_sphere_and_a_box() {
        let tight = |_: f64| 0.004;
        let (neck, c) = (0.3f64, 1.0f64);
        let wall = 10.0 * (1.0 - (neck / c) * (neck / c)).sqrt();
        let hollow = field(SdfNode::Combine { op: SdfOp::Difference, blend: 0.0, of: vec![SdfNode::Box { size: [2.0 * wall, 24.0, 20.0], radius: 0.0 }, SdfNode::Ellipsoid { radii: [10.0, 10.0, c] }] }, [wall, 12.0, 10.0]);
        let truth = Truth::of(Ctl::default(), &hollow, FINEST).unwrap();
        let mut d = [0.0; 3];
        truth.at(&[[5.0, 0.0, 0.0], [wall + 0.001, 0.0, 0.0], [5.0, 0.0, 0.4]], &tight, &mut d);
        let want = [ellipse_distance(5.0, 0.0, 10.0, c), 0.001f64.hypot(neck), ellipse_distance(5.0, 0.4, 10.0, c)];
        for (what, (got, want)) in ["the hollow's middle", "the neck", "off the middle"].iter().zip(d.iter().zip(want)) {
            assert!((got - want).abs() < 0.005, "{what}: read {got:.4}, truth {want:.4}");
        }
        // on the wall's face at the rim, where the face is a sliver between the rim's chord and
        // its curve, thinner than the lattice's cubes: on the surface
        let rim = [[-wall, -0.6, -0.3], [-wall, 0.0, -0.3], [-wall, 0.6, -0.3]];
        truth.at(&rim, &tight, &mut d);
        for (p, got) in rim.iter().zip(d) {
            assert!(got.abs() < 0.005, "the rim at {p:?}: read {got:.4}, truth 0");
        }
        let mut rng = 0x2545f4914f6cdd1du64;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng >> 11) as f64 / (1u64 << 53) as f64
        };
        let r = 5.0;
        let sphere = field(SdfNode::Sphere { radius: r }, [r; 3]);
        let half = [4.0, 3.0, 2.0];
        let cube = field(SdfNode::Box { size: half.map(|v| 2.0 * v), radius: 0.0 }, half);
        let box_distance = |p: P| {
            let q: P = std::array::from_fn(|k| p[k].abs() - half[k]);
            len(q.map(|v| v.max(0.0))) + q[0].max(q[1]).max(q[2]).min(0.0)
        };
        let sphere_distance = |p: P| len(p) - r;
        let shapes: [(&str, &Solid, &dyn Fn(P) -> f64, f64); 2] = [("sphere", &sphere, &sphere_distance, r + 2.0), ("box", &cube, &box_distance, 6.0)];
        for (name, solid, exact, reach) in shapes {
            let truth = Truth::of(Ctl::default(), solid, FINEST).unwrap();
            let pts: Vec<P> = (0..200).map(|_| std::array::from_fn(|_| (2.0 * next() - 1.0) * reach)).filter(|&p| exact(p).abs() < 2.0).collect();
            let mut got = vec![0.0; pts.len()];
            truth.at(&pts, &tight, &mut got);
            for (p, g) in pts.iter().zip(&got) {
                assert!((g - exact(*p)).abs() < 0.005, "{name} at {p:?}: read {g:.4}, truth {:.4}", exact(*p));
            }
            assert!(pts.len() > 50, "{name}: {} points near its surface", pts.len());
        }
        // a gentle cap, whose blocks are planes: a point 1 mm out over a block's side reads its
        // distance from the patches, the face running on across that side, without `bound`
        let centre = [0.0, 0.0, -39.0];
        let cap = field(SdfNode::Transform { at: centre, rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Sphere { radius: 40.0 }) }, [3.0, 3.0, 3.0]);
        let truth = Truth::of(Ctl::default(), &cap, FINEST).unwrap();
        let (o, f) = (truth.seeds.o, truth.seeds.finest);
        let x = o[0] + 2.0 * ((0.3 - o[0]) / (2.0 * f)).round() * f;
        let y = o[1] + (2.0 * ((0.2 - o[1]) / (2.0 * f)).floor() + 1.0) * f;
        let on = [x, y, centre[2] + (40.0f64 * 40.0 - x * x - y * y).sqrt()];
        let over = along(on, unit(sub(on, centre)), 1.0);
        let loose = |d: f64| crate::grid::Tol::LOOSE.at(d);
        BOUNDED.with(|n| n.set(0));
        truth.at(&[over], &loose, &mut d[..1]);
        assert!((d[0] - 1.0).abs() <= loose(1.0), "1 mm over a block's side at {over:?}: read {:.4}, truth 1", d[0]);
        assert_eq!(BOUNDED.with(|n| n.get()), 0, "the read 1 mm over a block's side went to bound");
        // a sphere of radius 5 has about 314 mm^2 of surface: some 2000 cubes of 0.4 mm
        let seeds = Truth::of(Ctl::default(), &sphere, FINEST).unwrap().seed_count();
        assert!((1_000..8_000).contains(&seeds), "the sphere's seeds: {seeds}");
    }

    /// A field's value is a distance only where it says so: a carve's is less than the distance
    /// past its convex edges, and a union of slabs that meet reads zero where they meet unless a
    /// mask bridges them. The edges' patches still give the reads, and `bound` finds no surface
    /// where there is none.
    #[test]
    fn a_carve_reads_its_edges_from_their_patches_and_a_pieces_slabs_hold_no_surface_between() {
        let tight = |_: f64| 0.004;
        // a cube with a quarter carved out along z: a convex edge at x 1, y 0 that the carve's
        // max makes, and a concave one at x 0, y 0
        let quarter = SdfNode::Transform { at: [1.0, 1.0, 0.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Box { size: [2.0, 2.0, 4.0], radius: 0.0 }) };
        let carved = field(SdfNode::Combine { op: SdfOp::Difference, blend: 0.0, of: vec![SdfNode::Box { size: [2.0; 3], radius: 0.0 }, quarter] }, [1.0; 3]);
        let truth = Truth::of(Ctl::default(), &carved, FINEST).unwrap();
        let (mut pts, mut want) = (Vec::new(), Vec::new());
        for i in 0..25 {
            let (a, b, z) = (0.05 + 0.01 * i as f64, 0.3 - 0.01 * i as f64, 0.024 * i as f64 - 0.3);
            pts.extend([[1.0 + a, b, z], [-a, -b, z]]);
            want.extend([a.hypot(b), -a.hypot(b)]);
        }
        BOUNDED.with(|n| n.set(0));
        let mut got = vec![0.0; pts.len()];
        truth.at(&pts, &tight, &mut got);
        for ((p, g), w) in pts.iter().zip(&got).zip(&want) {
            assert!((g - w).abs() < 0.005, "the carve's edge at {p:?}: read {g:.4}, truth {w:.4}");
        }
        assert_eq!(BOUNDED.with(|n| n.get()), 0, "reads by the carve's edges that went past their patches");
        // a cube as a piece of two runs that meet at z 0, neither's outline within the other's:
        // on the plane where they meet, 0.2 inside the cube's face
        use crate::apart::{masked, Affine, MaskRun};
        use makepad_fab_slicer::geom::{ExPolygon, Polygon, Pt};
        let square = |x0: f64, x1: f64| ExPolygon::new(Polygon::new(vec![Pt::from_mm(x0, -3.0), Pt::from_mm(x1, -3.0), Pt::from_mm(x1, 3.0), Pt::from_mm(x0, 3.0)]));
        let runs = [MaskRun { lo: -2.0, hi: 0.0, regions: Some(vec![square(-3.0, 2.0)]) }, MaskRun { lo: 0.0, hi: 2.0, regions: Some(vec![square(-2.0, 3.0)]) }];
        let cube = SdfShape { root: SdfNode::Box { size: [2.0; 3], radius: 0.0 }, min: [-1.0; 3], max: [1.0; 3], materials: vec![] };
        let to_plate: Affine = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
        let piece = Solid::of_field(&masked(&cube, &to_plate, &runs, ([-1.0; 3], [1.0; 3])).unwrap(), to_plate, None);
        let truth = Truth::of(Ctl::default(), &piece, FINEST).unwrap();
        let x = [0.8, 0.0, 0.0];
        let mut v = [0.0];
        piece.values(&[x], &mut v);
        let far = truth.bound(&truth.seeds, x, 0.004, v[0]);
        assert!((far - 0.2).abs() <= 0.004 && v[0] < 0.0, "inside the piece where its runs meet: bound {far:.4} (value {:.4}), truth -0.2", v[0]);
    }

    /// A sheet thinner than the lattice's step, between two of its planes, is found: no lattice
    /// edge has its ends on two sides of it. The slot of the grid's test (an ellipsoid cut out of
    /// a box, its hollow breaking the box's end face at a 0.3 neck) leaves a wedge of the wall
    /// some 0.03 thick behind the face: the face there reads 0, and the wedge's middle no more
    /// than its thickness inside. A plate 0.15 thick beside a cube, between the lattice's planes:
    /// its faces read 0, and a point over it its distance.
    #[test]
    fn a_sheet_thinner_than_the_lattices_step_is_found_between_its_planes() {
        let tight = |_: f64| 0.004;
        let axes = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
        let (neck, c) = (0.3f64, 1.0f64);
        let wall = 3.0 * (1.0 - (neck / c) * (neck / c)).sqrt();
        let half = [wall, 1.0, 1.5];
        let slot = field(SdfNode::Combine { op: SdfOp::Difference, blend: 0.0, of: vec![SdfNode::Box { size: half.map(|v| 2.0 * v), radius: 0.0 }, SdfNode::Ellipsoid { radii: [3.0, 3.0, c] }] }, half);
        let truth = Truth::of(Ctl::default(), &slot, 0.2).unwrap();
        let face = [[-wall, 0.9, 0.1], [-wall, 1.0, 0.1], [-wall, 0.9, 0.2], [-wall, 1.0, 0.2]];
        let mut d = [0.0; 4];
        truth.at(&face, &tight, &mut d);
        for (p, got) in face.iter().zip(d) {
            assert!(got.abs() < 0.005, "the slot's end face over its hollow at {p:?}: read {got:.4}, truth 0");
        }
        let middle = [-wall + 0.05, 0.95, 0.15];
        let mut d = [0.0];
        truth.at(&[middle], &tight, &mut d);
        assert!((-0.01..=0.0).contains(&d[0]), "in the wedge behind the slot's end face at {middle:?}: read {:.4}", d[0]);
        // the lattice starts LATTICE_OFF steps below the box: the box's foot puts it at z -0.5, so
        // its planes pass at -0.1 and 0.1, the plate's faces at -0.075 and 0.075
        let foot = 0.5 - LATTICE_OFF * 0.2;
        let cube = SdfNode::Transform { at: [0.0, 0.0, 2.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Box { size: [1.0; 3], radius: 0.0 }) };
        let root = SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of: vec![SdfNode::Box { size: [2.0, 2.0, 0.15], radius: 0.0 }, cube] };
        let plate = Solid::of_field(&SdfShape { root, min: [-1.0, -1.0, -foot], max: [1.0, 1.0, 2.5], materials: vec![] }, axes, None);
        let truth = Truth::of(Ctl::default(), &plate, 0.2).unwrap();
        let pts = [[-0.5, 0.3, 0.075], [0.0, 0.0, 0.075], [0.6, -0.4, 0.075], [-0.5, 0.3, -0.075], [0.0, 0.0, -0.075], [0.6, -0.4, -0.075], [0.2, 0.1, 0.375]];
        let want = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.3];
        let mut got = [0.0; 7];
        truth.at(&pts, &tight, &mut got);
        for ((p, g), w) in pts.iter().zip(got).zip(want) {
            assert!((g - w).abs() < 0.005, "the plate between the lattice's planes, at {p:?}: read {g:.4}, truth {w:.4}");
        }
    }
}
