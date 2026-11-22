//! A part's distance in bricks, sampled once when a check starts: a sim step reads hundreds to
//! thousands of distances at 240 Hz, and a way-out search millions. The bricks' nodes hold the
//! part's true distance ([`Truth`]), and a brick is split until it reads within a tolerance of the
//! truth ([`Tol`]): bricks are large on flat faces and far off, small at edges and in narrow gaps.
//! Bricks side by side differ by at most one split, and a finer brick's nodes on a coarser one's
//! side read as the coarser one does there, so the read is continuous.

use super::{Tol, PAST_BOX};
use crate::solid::Solid;
use crate::truth::Truth;
use makepad_csg_mech::GAP_TOL;
use makepad_fab_toolpath::par::{self, Cancelled, Ctl};

/// Root cubes along the box's longest side, at most: a layout constant that changes no read.
const ROOT_CUBES: f64 = 16.0;
/// Cells along a brick's side, and nodes.
const CELLS: usize = 4;
const SIDE: usize = CELLS + 1;
/// A brick's nodes (x fastest) and cells.
const NODES: usize = SIDE * SIDE * SIDE;
const CENTRES: usize = CELLS * CELLS * CELLS;
/// A slot holding a brick (its index below this bit), not the first of eight children's slots.
const LEAF: u32 = 1 << 31;
/// How far (degrees) a read's normal may be off the surface's at a point of it, in bricks whose
/// cells are coarser than the printer's resolution. A trilinear read's gradient on a curved face
/// is off by about kh/2 (k the curvature, h the cell): GAP_TOL alone would let a 2.5 mm roller
/// have 0.63 mm cells, 7 degrees off, and a roller rolling on normals that far off kicks. Set on
/// the bearing at 1, 2 and 4 degrees: the least that keeps it asleep by its time.
const NORMAL_TOL: f64 = 2.0;
/// The share of the tolerance a node's truth may be off by; the rest is the read's between them.
const TRUTH_SHARE: f64 = 0.25;
/// The finest cells, as a share of `finest` (the printer's layer): a brick of cells this size is
/// tested as any other but never split, and one that fails its test is flagged. A printer knows
/// a surface to about half a layer and rounds a corner to about its bead, so a model's kink has
/// no sharper print. Cells of half a layer read a crease of two flat faces within a quarter layer
/// (worst along an axis, and never over the truth), a right-angled edge within 0.15 of a layer,
/// and any shape at their centres within 0.87 of a cell. Holding edges to GAP_TOL instead takes
/// several times the bricks, to a closeness the printed part does not have.
const KINK_CELL: f64 = 0.5;
/// Bricks per batch of truths, and points.
const BATCH: usize = 16;
const POINTS: usize = 512;

type P = [f64; 3];

#[derive(Clone, Debug)]
pub(super) struct Bricks {
    /// The root lattice: its least corner, its cubes' side (mm), and how many along each axis.
    lo: [f32; 3],
    root: f32,
    roots: [usize; 3],
    /// A slot per root cube, x fastest, then eight per split cube (by octant: x, y, z bits): a
    /// brick, or the first of its eight children's slots.
    slots: Vec<u32>,
    /// Each brick's NODES distances.
    values: Vec<f32>,
    /// A bit per brick: it failed its test with cells of KINK_CELL x `finest`, and reads within
    /// 0.87 of a cell (and its nodes' share of the tolerance) of the truth, not within [`Tol`].
    flagged: Vec<u64>,
    /// The worst error at a cell's centre among those.
    worst: f32,
    tol: Tol,
}

/// A brick while the grid is built.
struct Brick {
    level: u32,
    /// Its least corner, in bricks of its level from the root lattice's corner.
    at: [u64; 3],
    /// Its nodes' truths; none once it is split.
    nodes: Vec<f64>,
    /// The truths at its cells' centres, once asked.
    centres: Option<Vec<f64>>,
    slot: u32,
    /// It failed its test at the finest cells (see [`Fate::Flag`]).
    flagged: bool,
}

/// What becomes of a brick once tested.
#[derive(Clone, Copy)]
enum Fate {
    /// It reads within its tolerance.
    Keep,
    /// It does not: eight children take its place.
    Split,
    /// It does not, and its cells are KINK_CELL x `finest`: kept and flagged, with its worst
    /// error at a cell's centre.
    Flag(f64),
}

struct Build<'a> {
    truth: Truth<'a>,
    tol: Tol,
    finest: f64,
    lo: P,
    root: f64,
    roots: [usize; 3],
    slots: Vec<u32>,
    /// The bricks no longer split (none: split since, by the balance).
    leaves: Vec<Brick>,
    /// The worst error at a cell's centre among the flagged bricks.
    worst: f64,
}

impl Bricks {
    /// The grid of `solid`, reading within `tol` of its distance. `finest` (mm): the smallest
    /// feature it promises to see (the printer's resolution) and the finest cells normals are
    /// held to; distances take cells as fine as they need.
    pub(super) fn build(ctl: Ctl, solid: &Solid, finest: f64, tol: Tol) -> Result<Bricks, Cancelled> {
        let say = |step: &str, done: f64| {
            if let Some(phase) = ctl.phase {
                phase(step, None, done);
            }
        };
        say("finding the surface", 0.0);
        let truth = Truth::of(ctl, solid, finest)?;
        let lo: P = std::array::from_fn(|k| solid.lo[k] - PAST_BOX);
        let size: P = std::array::from_fn(|k| solid.hi[k] - solid.lo[k] + 2.0 * PAST_BOX);
        let mut root = finest;
        while size.iter().any(|&s| s > root * ROOT_CUBES) {
            root *= 2.0;
        }
        let roots = size.map(|s| ((s / root).ceil() as usize).max(1));
        let count = roots[0] * roots[1] * roots[2];
        let mut b = Build { truth, tol, finest, lo, root, roots, slots: vec![0; count], leaves: Vec::new(), worst: 0.0 };
        let mut level: Vec<Brick> = (0..count).map(|s| {
            let at = [(s % roots[0]) as u64, (s / roots[0] % roots[1]) as u64, (s / (roots[0] * roots[1])) as u64];
            Brick { level: 0, at, nodes: Vec::new(), centres: None, slot: s as u32, flagged: false }
        }).collect();
        let points: Vec<P> = level.iter().flat_map(|r| (0..NODES).map(|n| b.node(r.level, r.at, n))).collect();
        let truths = b.truths(ctl, &points, NODES)?;
        for (r, t) in level.iter_mut().zip(truths.chunks(NODES)) {
            r.nodes = t.to_vec();
        }
        // Breadth first: a level's bricks are tested together, those that fail split into the next
        // down to cells of KINK_CELL x `finest`. Most end by the level whose cells are `finest`:
        // the share of those done is the progress.
        let levels = (root / (CELLS as f64 * finest)).log2().ceil().max(1.0);
        while !level.is_empty() {
            say("splitting bricks", 0.1 + 0.8 * (level[0].level as f64 / levels).min(1.0));
            b.centres(ctl, &mut level, false)?;
            let fates = par::try_map(ctl, level.len().div_ceil(BATCH), |c| level[c * BATCH..((c + 1) * BATCH).min(level.len())].iter().map(|r| b.fate(r)).collect::<Vec<_>>())?.concat();
            let mut parents = Vec::new();
            for (mut r, fate) in level.into_iter().zip(fates) {
                match fate {
                    Fate::Split => {
                        parents.push(r);
                        continue;
                    }
                    Fate::Flag(e) => {
                        r.flagged = true;
                        b.worst = b.worst.max(e);
                    }
                    Fate::Keep => {}
                }
                b.slots[r.slot as usize] = LEAF | b.leaves.len() as u32;
                b.leaves.push(r);
            }
            level = b.children(ctl, &parents)?;
            for (r, eight) in parents.iter().zip(level.chunks_mut(8)) {
                let first = b.slots.len() as u32;
                b.slots[r.slot as usize] = first;
                b.slots.extend([0; 8]);
                for (o, kid) in eight.iter_mut().enumerate() {
                    kid.slot = first + o as u32;
                }
            }
        }
        say("balancing bricks", 0.9);
        b.balance(ctl)?;
        b.hang(ctl)?;
        Ok(b.finish())
    }

    /// The distance at `p` (negative inside) and the unit direction it grows in.
    #[inline]
    pub(super) fn sample(&self, p: [f32; 3]) -> (f32, [f32; 3]) {
        let rel: [f32; 3] = std::array::from_fn(|k| p[k] - self.lo[k]);
        let q: [f32; 3] = std::array::from_fn(|k| rel[k].clamp(0.0, self.roots[k] as f32 * self.root));
        let (brick, u) = self.find(q);
        let v = &self.values[brick * NODES..(brick + 1) * NODES];
        let c: [f32; 3] = u.map(|u| u * CELLS as f32);
        let i: [usize; 3] = c.map(|c| (c as usize).min(CELLS - 1));
        let (tx, ty, tz) = (c[0] - i[0] as f32, c[1] - i[1] as f32, c[2] - i[2] as f32);
        let at = i[0] + SIDE * (i[1] + SIDE * i[2]);
        let (sx, sy, sz) = (1, SIDE, SIDE * SIDE);
        let (d000, d100, d010, d110) = (v[at], v[at + sx], v[at + sy], v[at + sx + sy]);
        let (d001, d101, d011, d111) = (v[at + sz], v[at + sx + sz], v[at + sy + sz], v[at + sx + sy + sz]);
        let (a00, a10, a01, a11) = (d000 + (d100 - d000) * tx, d010 + (d110 - d010) * tx, d001 + (d101 - d001) * tx, d011 + (d111 - d011) * tx);
        let (b0, b1) = (a00 + (a10 - a00) * ty, a01 + (a11 - a01) * ty);
        let value = b0 + (b1 - b0) * tz;
        let off: [f32; 3] = std::array::from_fn(|k| rel[k] - q[k]);
        let far = off[0] * off[0] + off[1] * off[1] + off[2] * off[2];
        if far > 0.0 {
            let far = far.sqrt();
            return (value + far, [off[0] / far, off[1] / far, off[2] / far]);
        }
        let (c00, c10, c01, c11) = (d100 - d000, d110 - d010, d101 - d001, d111 - d011);
        let gx = {
            let (e0, e1) = (c00 + (c10 - c00) * ty, c01 + (c11 - c01) * ty);
            e0 + (e1 - e0) * tz
        };
        let gy = {
            let (e0, e1) = (a10 - a00, a11 - a01);
            e0 + (e1 - e0) * tz
        };
        let gz = b1 - b0;
        unit_or_up(value, [gx, gy, gz])
    }

    /// How far a read of `d` may be from the truth: a read less this is never over it.
    pub(super) fn tol(&self, d: f32) -> f32 {
        self.tol.at(d as f64) as f32
    }

    /// The side of the cells at `p` (mm).
    pub(super) fn cell_at(&self, p: [f32; 3]) -> f32 {
        self.brick_at(p).1 / CELLS as f32
    }

    /// The least the true distance can be where the grid reads `d` at `p`: the read less its
    /// tolerance, or in a flagged brick less a cell's half diagonal and its nodes' share of it.
    pub(super) fn least_at(&self, p: [f32; 3], d: f32) -> f32 {
        let (brick, side) = self.brick_at(p);
        if self.flagged[brick / 64] >> (brick % 64) & 1 != 0 {
            d - (0.87 * side / CELLS as f32 + TRUTH_SHARE as f32 * self.tol(d))
        } else {
            d - self.tol(d)
        }
    }

    /// How many bricks are flagged, and the worst error at a cell's centre among them (mm).
    pub(super) fn flagged(&self) -> (usize, f32) {
        (self.flagged.iter().map(|w| w.count_ones() as usize).sum(), self.worst)
    }

    /// The brick at `p` (the lattice's nearest where `p` is past it) and its side (mm).
    fn brick_at(&self, p: [f32; 3]) -> (usize, f32) {
        let q: [f32; 3] = std::array::from_fn(|k| (p[k] - self.lo[k]).clamp(0.0, self.roots[k] as f32 * self.root));
        let mut s = self.slots[self.root_slot(q)];
        let mut side = self.root;
        let mut f: [f32; 3] = std::array::from_fn(|k| q[k] / self.root - self.root_cube(q, k) as f32);
        while s & LEAF == 0 {
            let o = octant(&mut f);
            s = self.slots[s as usize + o];
            side *= 0.5;
        }
        ((s & !LEAF) as usize, side)
    }

    /// How many bricks it holds, and the bytes they and its slots take.
    pub(super) fn bricks(&self) -> usize {
        self.values.len() / NODES
    }
    pub(super) fn bytes(&self) -> usize {
        4 * (self.values.len() + self.slots.len()) + 8 * self.flagged.len()
    }

    fn root_cube(&self, q: [f32; 3], k: usize) -> usize {
        ((q[k] / self.root) as usize).min(self.roots[k] - 1)
    }

    fn root_slot(&self, q: [f32; 3]) -> usize {
        self.root_cube(q, 0) + self.roots[0] * (self.root_cube(q, 1) + self.roots[1] * self.root_cube(q, 2))
    }

    /// The brick at `q` (mm from the lattice's corner, inside it), and where `q` is across it.
    #[inline]
    fn find(&self, q: [f32; 3]) -> (usize, [f32; 3]) {
        let mut s = self.slots[self.root_slot(q)];
        let mut f: [f32; 3] = std::array::from_fn(|k| (q[k] / self.root - self.root_cube(q, k) as f32).clamp(0.0, 1.0));
        while s & LEAF == 0 {
            let o = octant(&mut f);
            s = self.slots[s as usize + o];
        }
        ((s & !LEAF) as usize, f)
    }
}

/// `value` and the unit direction of `g` (up where it has none).
#[inline]
fn unit_or_up(value: f32, g: [f32; 3]) -> (f32, [f32; 3]) {
    let l = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt();
    if l > 1e-9 {
        (value, [g[0] / l, g[1] / l, g[2] / l])
    } else {
        (value, [0.0, 0.0, 1.0])
    }
}

/// Where cell `c`'s centre is across its brick (0..1 on each axis).
fn centre(c: usize) -> P {
    let j = [c % CELLS, c / CELLS % CELLS, c / (CELLS * CELLS)];
    std::array::from_fn(|k| (j[k] as f64 + 0.5) / CELLS as f64)
}

/// The octant of `f` (0..1 on each axis), and `f` across it: exact, as it only doubles.
#[inline]
fn octant(f: &mut [f32; 3]) -> usize {
    let mut o = 0;
    for k in 0..3 {
        f[k] *= 2.0;
        if f[k] >= 1.0 {
            o |= 1 << k;
            f[k] -= 1.0;
        }
    }
    o
}

/// [`octant`] in f64, as the build finds its bricks.
fn octant_f64(f: &mut P) -> usize {
    let mut o = 0;
    for k in 0..3 {
        f[k] *= 2.0;
        if f[k] >= 1.0 {
            o |= 1 << k;
            f[k] -= 1.0;
        }
    }
    o
}

/// The cell of a brick at `u` (0..1 across it): its eight corners' nodes (x, y, z bits), their
/// trilinear weights there, and the weights' gradients (per cell).
fn weights(u: P) -> ([usize; 8], [f64; 8], [P; 8]) {
    let c: P = u.map(|u| (u * CELLS as f64).clamp(0.0, CELLS as f64));
    let i: [usize; 3] = c.map(|c| (c as usize).min(CELLS - 1));
    let t: P = std::array::from_fn(|k| c[k] - i[k] as f64);
    let mut nodes = [0; 8];
    let mut weights = [0.0; 8];
    let mut slopes = [[0.0; 3]; 8];
    for corner in 0..8 {
        let s = [corner & 1, corner >> 1 & 1, corner >> 2];
        let w: P = std::array::from_fn(|k| if s[k] == 1 { t[k] } else { 1.0 - t[k] });
        nodes[corner] = i[0] + s[0] + SIDE * (i[1] + s[1] + SIDE * (i[2] + s[2]));
        weights[corner] = w[0] * w[1] * w[2];
        slopes[corner] = std::array::from_fn(|k| if s[k] == 1 { 1.0 } else { -1.0 } * w[(k + 1) % 3] * w[(k + 2) % 3]);
    }
    (nodes, weights, slopes)
}

impl Build<'_> {
    /// The point `q` / (`per` x 2^`level`) root cubes from the lattice's corner, on each axis:
    /// a node is the same point however it is named.
    fn point(&self, level: u32, q: [u64; 3], per: u64) -> P {
        let whole = (per << level) as f64;
        std::array::from_fn(|k| self.lo[k] + self.root * (q[k] as f64 / whole))
    }

    /// A brick's node `n`.
    fn node(&self, level: u32, at: [u64; 3], n: usize) -> P {
        let i = [n % SIDE, n / SIDE % SIDE, n / (SIDE * SIDE)];
        self.point(level, std::array::from_fn(|k| at[k] * CELLS as u64 + i[k] as u64), CELLS as u64)
    }

    /// A brick's read at `u` (0..1 across it) and its gradient (per mm): its nodes' trilinear
    /// read.
    fn read(&self, r: &Brick, u: P) -> (f64, P) {
        let cell = self.cell(r.level);
        let (corners, w, dw) = weights(u);
        let (mut v, mut g) = (0.0, [0.0; 3]);
        for k in 0..8 {
            v += w[k] * r.nodes[corners[k]];
            for i in 0..3 {
                g[i] += dw[k][i] * r.nodes[corners[k]] / cell;
            }
        }
        (v, g)
    }

    /// The truths at `points`, to within their share of the tolerance: in runs of `per`, each a
    /// brick's (a mesh reads a run as one).
    fn truths(&self, ctl: Ctl, points: &[P], per: usize) -> Result<Vec<f64>, Cancelled> {
        let tol = |d: f64| TRUTH_SHARE * self.tol.at(d);
        let batch = POINTS.div_ceil(per) * per;
        Ok(par::try_map(ctl, points.len().div_ceil(batch), |c| {
            let part = &points[c * batch..((c + 1) * batch).min(points.len())];
            let mut out = vec![0.0; part.len()];
            self.truth.at_runs(part, per, &tol, &mut out);
            out
        })?
        .concat())
    }

    /// A brick's cells' side (mm).
    fn cell(&self, level: u32) -> f64 {
        self.root / (1u64 << level) as f64 / CELLS as f64
    }

    /// Whether a brick is tested. A brick reads within its nodes' share of the tolerance and a
    /// cell's half diagonal of the truth anywhere, the truth being a distance: once that is within
    /// the rest of GAP_TOL, it needs no test, and is never split.
    fn tested(&self, r: &Brick) -> bool {
        0.87 * self.cell(r.level) > (1.0 - TRUTH_SHARE) * GAP_TOL
    }

    /// The truths at the cells' centres of the bricks that are tested (`all`: every brick).
    fn centres(&self, ctl: Ctl, bricks: &mut [Brick], all: bool) -> Result<(), Cancelled> {
        let asked: Vec<usize> = (0..bricks.len()).filter(|&i| bricks[i].centres.is_none() && (all || self.tested(&bricks[i]))).collect();
        let points: Vec<P> = asked.iter().flat_map(|&i| {
            let r = &bricks[i];
            (0..CENTRES).map(move |c| {
                let j = [c % CELLS, c / CELLS % CELLS, c / (CELLS * CELLS)];
                self.point(r.level, std::array::from_fn(|k| r.at[k] * 2 * CELLS as u64 + 2 * j[k] as u64 + 1), 2 * CELLS as u64)
            })
        }).collect();
        let truths = self.truths(ctl, &points, CENTRES)?;
        for (&i, t) in asked.iter().zip(truths.chunks(CENTRES)) {
            bricks[i].centres = Some(t.to_vec());
        }
        Ok(())
    }

    /// Whether a brick reads too far from the truth: at a cell's centre by more than its share of
    /// the tolerance, at a point of the surface by more than GAP_TOL, or, while its cells are
    /// coarser than `finest`, with a normal more than NORMAL_TOL off the surface's.
    fn splits(&self, r: &Brick) -> bool {
        let Some(centres) = &r.centres else { return false };
        for (c, &t) in centres.iter().enumerate() {
            let (read, _) = self.read(r, centre(c));
            if (read - t).abs() > (1.0 - TRUTH_SHARE) * self.tol.at(t) {
                return true;
            }
        }
        let lo = self.node(r.level, r.at, 0);
        let hi = self.node(r.level, r.at, NODES - 1);
        let side = hi[0] - lo[0];
        let normals = self.cell(r.level) > self.finest;
        let least = NORMAL_TOL.to_radians().cos();
        let mut split = false;
        self.truth.surface_in(lo, hi, &mut |p, n| {
            if split {
                return;
            }
            let (d, g) = self.read(r, std::array::from_fn(|k| (p[k] - lo[k]) / side));
            let along = (g[0] * n[0] + g[1] * n[1] + g[2] * n[2]).abs();
            let l = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt();
            split = d.abs() > GAP_TOL || normals && n != [0.0; 3] && along < least * l;
        });
        split
    }

    /// What becomes of a brick: kept where it reads within its tolerance (see [`Build::splits`]),
    /// else split, unless its cells are KINK_CELL x `finest` already: then flagged.
    fn fate(&self, r: &Brick) -> Fate {
        if !self.splits(r) {
            return Fate::Keep;
        }
        if self.cell(r.level) > KINK_CELL * self.finest {
            return Fate::Split;
        }
        let centres = r.centres.as_ref().expect("a brick that fails was tested");
        Fate::Flag(centres.iter().enumerate().map(|(c, &t)| (self.read(r, centre(c)).0 - t).abs()).fold(0.0, f64::max))
    }

    /// Each brick's eight children, their nodes' truths taken from the brick's nodes and centres
    /// where they are the same points.
    fn children(&self, ctl: Ctl, parents: &[Brick]) -> Result<Vec<Brick>, Cancelled> {
        const FINE: usize = 2 * CELLS + 1;
        let new = |a: usize| {
            let l = [a % FINE, a / FINE % FINE, a / (FINE * FINE)];
            !(l.iter().all(|v| v % 2 == 0) || l.iter().all(|v| v % 2 == 1))
        };
        let points: Vec<P> = parents.iter().flat_map(|r| {
            (0..FINE * FINE * FINE).filter(|&a| new(a)).map(move |a| {
                let l = [a % FINE, a / FINE % FINE, a / (FINE * FINE)];
                self.point(r.level, std::array::from_fn(|k| r.at[k] * 2 * CELLS as u64 + l[k] as u64), 2 * CELLS as u64)
            })
        }).collect();
        let per = points.len() / parents.len().max(1);
        let truths = self.truths(ctl, &points, per.max(1))?;
        let mut out = Vec::with_capacity(8 * parents.len());
        for (r, t) in parents.iter().zip(truths.chunks(per.max(1))) {
            let centres = r.centres.as_ref().expect("a split brick was tested");
            let mut fine = vec![0.0; FINE * FINE * FINE];
            let mut next = t.iter();
            for (a, v) in fine.iter_mut().enumerate() {
                let l = [a % FINE, a / FINE % FINE, a / (FINE * FINE)];
                *v = if l.iter().all(|v| v % 2 == 0) {
                    r.nodes[l[0] / 2 + SIDE * (l[1] / 2 + SIDE * (l[2] / 2))]
                } else if l.iter().all(|v| v % 2 == 1) {
                    centres[l[0] / 2 + CELLS * (l[1] / 2 + CELLS * (l[2] / 2))]
                } else {
                    *next.next().expect("a truth per new node")
                };
            }
            for o in 0..8usize {
                let s = [o & 1, o >> 1 & 1, o >> 2];
                let nodes = (0..NODES).map(|n| {
                    let i = [n % SIDE, n / SIDE % SIDE, n / (SIDE * SIDE)];
                    fine[s[0] * CELLS + i[0] + FINE * (s[1] * CELLS + i[1] + FINE * (s[2] * CELLS + i[2]))]
                }).collect();
                out.push(Brick { level: r.level + 1, at: std::array::from_fn(|k| r.at[k] * 2 + s[k] as u64), nodes, centres: None, slot: 0, flagged: false });
            }
        }
        Ok(out)
    }

    /// The brick at `q` (root cubes from the lattice's corner).
    fn leaf_at(&self, q: P) -> usize {
        let i: [usize; 3] = std::array::from_fn(|k| (q[k].max(0.0) as usize).min(self.roots[k] - 1));
        let mut f: P = std::array::from_fn(|k| (q[k] - i[k] as f64).clamp(0.0, 1.0));
        let mut s = self.slots[i[0] + self.roots[0] * (i[1] + self.roots[1] * i[2])];
        while s & LEAF == 0 {
            let o = octant_f64(&mut f);
            s = self.slots[s as usize + o];
        }
        (s & !LEAF) as usize
    }

    fn inside(&self, q: P) -> bool {
        (0..3).all(|k| q[k] >= 0.0 && q[k] <= self.roots[k] as f64)
    }

    /// Splits `parents` (leaves): their children take their slots, as leaves.
    fn split_leaves(&mut self, ctl: Ctl, parents: &[usize]) -> Result<(), Cancelled> {
        let mut taken: Vec<Brick> = parents.iter().map(|&i| {
            let r = &mut self.leaves[i];
            Brick { level: r.level, at: r.at, nodes: std::mem::take(&mut r.nodes), centres: r.centres.take(), slot: r.slot, flagged: false }
        }).collect();
        self.centres(ctl, &mut taken, true)?;
        let mut kids = self.children(ctl, &taken)?.into_iter();
        for r in &taken {
            let first = self.slots.len() as u32;
            self.slots[r.slot as usize] = first;
            for o in 0..8 {
                let mut kid = kids.next().expect("eight children a brick");
                kid.slot = first + o;
                self.slots.push(LEAF | self.leaves.len() as u32);
                self.leaves.push(kid);
            }
        }
        Ok(())
    }

    /// Splits bricks until no two that touch (on a side, an edge or a corner) are more than one
    /// split apart.
    fn balance(&mut self, ctl: Ctl) -> Result<(), Cancelled> {
        loop {
            let leaves = &self.leaves;
            let found = par::try_map(ctl, leaves.len().div_ceil(POINTS), |c| {
                let mut coarse = Vec::new();
                for r in &leaves[c * POINTS..((c + 1) * POINTS).min(leaves.len())] {
                    if r.nodes.is_empty() || r.level < 2 {
                        continue;
                    }
                    let whole = (1u64 << r.level) as f64;
                    for d in 0..27 {
                        let dir = [d % 3, d / 3 % 3, d / 9].map(|v| v as f64 - 1.0);
                        if dir == [0.0; 3] {
                            continue;
                        }
                        let q: P = std::array::from_fn(|k| (r.at[k] as f64 + 0.5 + 0.75 * dir[k]) / whole);
                        if self.inside(q) {
                            let n = self.leaf_at(q);
                            if leaves[n].level + 1 < r.level {
                                coarse.push(n);
                            }
                        }
                    }
                }
                coarse
            })?;
            let mut coarse: Vec<usize> = found.concat();
            coarse.sort_unstable();
            coarse.dedup();
            if coarse.is_empty() {
                return Ok(());
            }
            self.split_leaves(ctl, &coarse)?;
        }
    }

    /// A brick's nodes on the side of a coarser one take the coarser one's read there, coarsest
    /// first, so a read crossing from one to the other doesn't jump.
    fn hang(&mut self, ctl: Ctl) -> Result<(), Cancelled> {
        let deepest = self.leaves.iter().filter(|r| !r.nodes.is_empty()).map(|r| r.level).max().unwrap_or(0);
        for level in 1..=deepest {
            let ids: Vec<usize> = (0..self.leaves.len()).filter(|&i| self.leaves[i].level == level && !self.leaves[i].nodes.is_empty()).collect();
            let fixes = par::try_map(ctl, ids.len().div_ceil(BATCH), |c| {
                let mut fixes = Vec::new();
                for &id in &ids[c * BATCH..((c + 1) * BATCH).min(ids.len())] {
                    let r = &self.leaves[id];
                    let whole = (CELLS as u64) << level;
                    for n in 0..NODES {
                        let i = [n % SIDE, n / SIDE % SIDE, n / (SIDE * SIDE)];
                        let ways: [&[f64]; 3] = i.map(|i| -> &[f64] {
                            match i {
                                0 => &[0.0, -1.0],
                                CELLS => &[0.0, 1.0],
                                _ => &[0.0],
                            }
                        });
                        if ways.iter().all(|w| w.len() == 1) {
                            continue;
                        }
                        let q: P = std::array::from_fn(|k| (r.at[k] * CELLS as u64 + i[k] as u64) as f64 / whole as f64);
                        'dirs: for &dx in ways[0] {
                            for &dy in ways[1] {
                                for &dz in ways[2] {
                                    if [dx, dy, dz] == [0.0; 3] {
                                        continue;
                                    }
                                    let step = 0.25 / whole as f64;
                                    let near = [q[0] + dx * step, q[1] + dy * step, q[2] + dz * step];
                                    if !self.inside(near) {
                                        continue;
                                    }
                                    let other = &self.leaves[self.leaf_at(near)];
                                    if other.level < level {
                                        let w = (1u64 << other.level) as f64;
                                        let u: P = std::array::from_fn(|k| q[k] * w - other.at[k] as f64);
                                        let (corners, weights, _) = weights(u);
                                        let mut value = 0.0;
                                        for k in 0..8 {
                                            value += weights[k] * other.nodes[corners[k]];
                                        }
                                        fixes.push((id, n, value));
                                        break 'dirs;
                                    }
                                }
                            }
                        }
                    }
                }
                fixes
            })?;
            for (id, n, v) in fixes.into_iter().flatten() {
                self.leaves[id].nodes[n] = v;
            }
        }
        Ok(())
    }

    fn finish(self) -> Bricks {
        let mut index = vec![u32::MAX; self.leaves.len()];
        let mut values = Vec::new();
        let mut flagged = vec![0u64; self.leaves.len().div_ceil(64)];
        for (i, r) in self.leaves.iter().enumerate() {
            if !r.nodes.is_empty() {
                let n = values.len() / NODES;
                index[i] = n as u32;
                flagged[n / 64] |= (r.flagged as u64) << (n % 64);
                values.extend(r.nodes.iter().map(|&v| v as f32));
            }
        }
        flagged.truncate((values.len() / NODES).div_ceil(64));
        let slots = self.slots.iter().map(|&s| if s & LEAF != 0 { LEAF | index[(s & !LEAF) as usize] } else { s }).collect();
        Bricks {
            lo: self.lo.map(|v| v as f32),
            root: self.root as f32,
            roots: self.roots,
            slots,
            values,
            flagged,
            worst: self.worst as f32,
            tol: self.tol,
        }
    }
}

