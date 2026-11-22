//! A part's surface gathered from the rays of every frame.
//!
//! Where a ray lands, a cell of a lattice fixed to the part comes to exist, about as large as the
//! rays are far apart. A cell's vertex depends on the field and the cell, never on the ray or the
//! view that reached it, so a second view extends or sharpens what is there and re-rolls nothing.
//! Nothing exists where no ray landed, but for surface just past edge-on, facing away from the
//! view, walked on from what landed: a small turn of the view shows it, and no ray of this view can
//! land there. It draws nothing from this view. A lattice's hidden layers are never built.
//!
//! The mesh is the octree's dual: a vertex per cell the surface crosses, a quad per lattice edge
//! with a sign change, around the cells that share the edge. Neighbours of different sizes share
//! those edges, so no crack opens between levels, and a quad only joins cells that touch, so none
//! runs across a jump in depth. One leaf holds any point, so nothing coarser draws over anything
//! finer and nothing z-fights.
//!
//! Rays decide visibility, the mesh only saves them. A ray goes wherever the mesh leaves the view
//! uncovered. Where it covers it, a check reads the field at the mesh's point on the ray: off the
//! surface, the mesh is wrong there (a hole it does not have yet, a bump a coarse cell cuts off), so
//! the ray goes on to what is really there and the cell splits. A cone per tile finds what stands
//! in front of the mesh, which a check at the mesh's depth can't.
//!
//! A cell with no sign change at its corners holds no quads, but only a bound calls space empty: a
//! ray's march. A ray that lands in such a cell splits it until the corners see the surface.

use crate::trace::{add, cones_batch, mul, norm, oct_pack, pack_cut, par, rays_batch, slab, sub, dot, Cam, Cone, Field, Ray, Tolerance, BATCH, TILE_SLOTS, V};
use makepad_csg_math::fast_hash::FastMap;
use makepad_draw::makepad_platform::retained_instances::{InstanceSegment, RetainedInstances};
use makepad_draw::makepad_platform::thread::TaskPool;
use std::collections::hash_map::Entry;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Draw chunks a part's mesh is cut into: a change uploads only its chunks.
pub const CHUNKS: usize = 512;
const CHUNK_DEPTH: u32 = 3;
/// A chunk is 64 segments.
const SEGMENT_DEPTH: u32 = CHUNK_DEPTH + 2;
/// Bits of a lattice coordinate: the finest cell is the root's side over 2^BITS.
const BITS: u32 = 20;
const HALF_DIAG: f64 = 0.866_025_403_784_438_6;
/// The first rays of a look at the view, this many pixels apart.
const FIRST_STRIDE: u32 = 16;
/// A new cell is this many first-ray spacings across: a later ray lands in it or the next one.
/// Only a check that finds the mesh off the surface makes a cell smaller.
const CELL_SPACINGS: f64 = 2.0;
/// From this far apart a round's samples have checked what the rounds before it built, and what
/// it adds itself is small: until then a view's new cells are far larger than any ray has tested,
/// and a thin wall comes out as long wrong triangles.
const WHOLE_STRIDE: u32 = 4;
/// A cell on screen larger than a whole round's own is split while its corners stray more than this
/// part of that size from the plane its vertex lies on: it holds more than one piece of surface, or
/// a sharply curved one, and its quads would be long wrong triangles from wherever they get seen
/// first.
const FLAT: f64 = 0.5;
/// A cell's corners' stray is kept in this many parts of its side.
const STRAY_PARTS: f64 = 64.0;
/// A cell at the edge of what the mesh shows, a sample out, comes down to this many leaves.
const EDGE_LEAVES: f64 = 2.0;
/// Steeper than this, neighbouring samples of the mesh's depth count as a jump: this many of their
/// spacings in depth.
const JUMP_SLOPE: f64 = 4.0;
/// A ray that lands where the corners see nothing splits its cell down to this part of the leaf.
const OPEN_FLOOR: f64 = 0.5;
/// Rows to a task in a look's last stage.
const ROWS: usize = 16;

const UNREAD: u8 = 0;
const OPEN: u8 = 1;
const SURFACE: u8 = 2;

const PLACED: u8 = 1;
/// A ray landed in it or in the cell it split from, or it faces just past edge-on: the cells around
/// its edges are read, and so placed.
const SHOWN: u8 = 2;
/// Split from a shown cell: shown once read, if the surface crosses it.
const INHERIT: u8 = 4;

const NO_SAMPLE: u32 = u32::MAX;
/// A look's rays to a thread at a time. A round asks few of them (a part in a small part of the
/// view, a thousand or so a look), and as many as a whole [`BATCH`] would march on one thread.
const RAY_BATCH: usize = 32;
/// A surface facing away from the view by this little, as the sine of the angle (about 3.4 degrees),
/// is walked on from what rays landed on.
const EDGE_ON: f64 = 0.06;

pub type Stale<'a> = &'a (dyn Fn() -> bool + Sync);
/// The size from which a cell at a point has to be flat.
type Coarse<'a> = &'a (dyn Fn(V) -> f64 + Sync);
/// Whether a surface at a point, with a normal, faces just past edge-on to the view.
type EdgeOn<'a> = &'a dyn Fn(V, V) -> bool;

#[derive(Clone, Copy, Default)]
struct Cell {
    x: u32,
    y: u32,
    z: u32,
    /// The first of eight children, 0 for a leaf.
    kids: u32,
    p: [f32; 3],
    n: [f32; 3],
    depth: u8,
    kind: u8,
    /// Bit k: corner k (bit 0 x, bit 1 y, bit 2 z) is inside.
    signs: u8,
    flags: u8,
    slot: u8,
    /// How far its corners stray from the plane its vertex lies on, in [`STRAY_PARTS`] of its side.
    stray: u8,
    /// Its value on a section's face ([`Field::cuts_at`]), a field under a section.
    cut: u16,
    /// The ordinal of the feature its vertex is on ([`Field::ords_at`]).
    ord: u16,
}

fn point(lo: V, unit: f64, key: u64) -> V {
    let m = (1u64 << 21) - 1;
    [lo[0] + (key & m) as f64 * unit, lo[1] + ((key >> 21) & m) as f64 * unit, lo[2] + ((key >> 42) & m) as f64 * unit]
}

fn key(i: u64, j: u64, k: u64) -> u64 {
    i | j << 21 | k << 42
}

/// Whether a cell of side `h` is to split, at a place where `size` is the size that has to be flat.
/// The finest cell stays, whatever it holds.
fn too_coarse(depth: u8, h: f64, stray: u8, size: f64) -> bool {
    depth as u32 + 1 < BITS && h > size && stray as f64 * h / STRAY_PARTS > size * FLAT
}

/// Corner `k` of a cell, on the lattice.
fn corner(c: &Cell, k: usize) -> u64 {
    let s = BITS - c.depth as u32;
    key((c.x as u64 + (k & 1) as u64) << s, (c.y as u64 + ((k >> 1) & 1) as u64) << s, (c.z as u64 + ((k >> 2) & 1) as u64) << s)
}

/// A shown cell's edge where the neighbour `at` of `depth` is cut finer: its children on the sides
/// `side_b`, `side_e` of axes `b`, `e` touch the edge.
struct Cut {
    at: [i64; 3],
    depth: u8,
    b: u8,
    e: u8,
    side_b: u8,
    side_e: u8,
}

enum Around {
    Leaf(u32),
    Finer,
    Outside,
}

/// A ray landed at `p`: the cell holding it, of side `side` or less. `floor`: how far it may split
/// to find the surface when the cell's corners do not see it.
#[derive(Clone, Copy)]
struct Landing {
    p: V,
    side: f64,
    floor: f64,
}

/// What a surface holds, for the log.
#[derive(Default, Clone, Copy)]
pub struct Held {
    pub cells: usize,
    pub quads: usize,
    pub chunks: usize,
    pub cpu_bytes: usize,
    pub gpu_bytes: usize,
}

pub struct Surface {
    lo: V,
    unit: f64,
    cells: Vec<Cell>,
    values: FastMap<u64, f32>,
    /// Surface leaves with no vertex yet.
    unplaced: Vec<u32>,
    /// Shown leaves whose edges are still to walk: newly shown, or beside a cell split finer.
    newly: Vec<u32>,
    /// Children of shown cells, to read.
    inherited: Vec<u32>,
    /// Placed leaves too coarse for what they hold.
    rough: Vec<u32>,
    /// Placed leaves not shown: the edge of what the mesh holds.
    ring: Vec<u32>,
    /// Per chunk, its segments to walk again.
    dirty: Vec<u64>,
    /// Per chunk, what its segments hold: a change walks only its segments, and the rest is shared.
    segments: Vec<Option<Box<[Option<InstanceSegment>; 64]>>>,
    shown: Vec<Option<Chunk>>,
    /// Per chunk, the box round all it has held: a mesh's quads only get finer in place.
    boxes: Vec<QuadBox>,
    /// Per segment (a chunk's 64 in turn), the emit that last showed it, and the box round all it
    /// has held.
    shown_at: Vec<u64>,
    seg_boxes: Vec<QuadBox>,
    /// Emits so far.
    emits: u64,
    /// Which surface this is, to a cover drawn from it.
    id: u64,
    /// Moves when the tree does.
    changes: u64,
}

static SURFACES: AtomicU64 = AtomicU64::new(0);

impl Surface {
    /// Nothing yet, and nothing evaluated: rays bring it.
    pub fn new(f: &Field) -> Surface {
        let ext = (0..3).map(|i| f.hi[i] - f.lo[i]).fold(0.0f64, f64::max) * 1.0001;
        Surface { lo: f.lo, unit: ext / (1u64 << BITS) as f64, cells: vec![Cell::default()], values: FastMap::default(), ring: Vec::new(), unplaced: Vec::new(), newly: Vec::new(), rough: Vec::new(), inherited: Vec::new(), dirty: vec![0; CHUNKS], segments: (0..CHUNKS).map(|_| None).collect(), boxes: vec![QuadBox::EMPTY; CHUNKS], shown: vec![None; CHUNKS], shown_at: vec![0; CHUNKS * 64], seg_boxes: vec![QuadBox::EMPTY; CHUNKS * 64], emits: 0, id: SURFACES.fetch_add(1, Ordering::Relaxed), changes: 0 }
    }

    fn alloc(&mut self) -> u32 {
        let at = self.cells.len() as u32;
        self.cells.resize(at as usize + 8, Cell::default());
        at
    }

    fn size(&self, depth: u8) -> f64 {
        self.unit * (1u64 << (BITS - depth as u32)) as f64
    }

    fn bounds(&self, c: &Cell) -> (V, V) {
        let h = self.size(c.depth);
        let lo = add(self.lo, [c.x as f64 * h, c.y as f64 * h, c.z as f64 * h]);
        (lo, add(lo, [h; 3]))
    }

    fn octant(&self, c: &Cell, p: V) -> u32 {
        let h = self.size(c.depth) * 0.5;
        let m = add(self.lo, [(c.x as f64 * 2.0 + 1.0) * h, (c.y as f64 * 2.0 + 1.0) * h, (c.z as f64 * 2.0 + 1.0) * h]);
        (p[0] >= m[0]) as u32 | ((p[1] >= m[1]) as u32) << 1 | ((p[2] >= m[2]) as u32) << 2
    }

    fn leaf_at(&self, p: V) -> u32 {
        let mut n = 0u32;
        loop {
            let c = &self.cells[n as usize];
            if c.kids == 0 {
                return n;
            }
            n = c.kids + self.octant(c, p);
        }
    }

    /// The field's values at `keys`, evaluating those not known yet.
    fn ensure(&mut self, f: &Field, pool: &TaskPool, keys: &[u64], stale: Stale) -> Option<()> {
        let mut jobs: Vec<(u64, f32)> = Vec::new();
        for k in keys {
            if let Entry::Vacant(e) = self.values.entry(*k) {
                e.insert(f32::NAN);
                jobs.push((*k, 0.0));
            }
        }
        let (lo, unit) = (self.lo, self.unit);
        par(pool, &mut jobs, BATCH, |c| {
            if stale() {
                return;
            }
            let pts: Vec<V> = c.iter().map(|j| point(lo, unit, j.0)).collect();
            for (j, v) in c.iter_mut().zip(f.value(&pts)) {
                j.1 = (v * f.step) as f32;
            }
        });
        if stale() {
            // nothing was learned: whoever asks next evaluates these
            for (k, _) in jobs {
                self.values.remove(&k);
            }
            return None;
        }
        for (k, v) in jobs {
            self.values.insert(k, v);
        }
        Some(())
    }

    fn split(&mut self, id: u32) -> u32 {
        let kids = self.alloc();
        let p = self.cells[id as usize];
        self.cells[id as usize].kids = kids;
        self.touch(&p);
        self.changes += 1;
        let inherit = p.flags & SHOWN != 0;
        for k in 0..8u32 {
            self.cells[(kids + k) as usize] = Cell { x: p.x * 2 + (k & 1), y: p.y * 2 + ((k >> 1) & 1), z: p.z * 2 + ((k >> 2) & 1), depth: p.depth + 1, slot: p.slot, ord: p.ord, flags: if inherit { INHERIT } else { 0 }, ..Cell::default() };
            if inherit {
                self.inherited.push(kids + k);
            }
        }
        if !inherit {
            self.beside_split(&p);
        }
        kids
    }

    /// Whether a leaf beside `c` (across a face, an edge or a corner) that was read is coarser than
    /// `limit`; each such goes into `splits`, at a point inside it.
    fn coarse_beside(&self, c: &Cell, limit: f64, splits: &mut Vec<(V, f64)>) -> bool {
        let h = self.size(c.depth);
        let mut any = false;
        for k in 0..27i64 {
            let d = [k % 3 - 1, (k / 3) % 3 - 1, k / 9 - 1];
            if d == [0; 3] {
                continue;
            }
            let at = [c.x as i64 + d[0], c.y as i64 + d[1], c.z as i64 + d[2]];
            let Around::Leaf(n) = self.around(c.depth as u32, at) else { continue };
            let o = &self.cells[n as usize];
            if o.kind == SURFACE && self.size(o.depth) > limit {
                splits.push((add(self.lo, at.map(|v| (v as f64 + 0.5) * h)), limit));
                any = true;
            }
        }
        any
    }

    /// A cell that does not show split: its children cut the edges it shares with the shown leaves
    /// beside it, no finer than it, and own those edges' quads now. Those leaves walk their edges
    /// again, to show the children along them.
    fn beside_split(&mut self, p: &Cell) {
        for k in 0..27i64 {
            let d = [k % 3 - 1, (k / 3) % 3 - 1, k / 9 - 1];
            let off = d.iter().filter(|v| **v != 0).count();
            // across a face or an edge: a corner shares no edge
            if off == 0 || off == 3 {
                continue;
            }
            if let Around::Leaf(n) = self.around(p.depth as u32, [p.x as i64 + d[0], p.y as i64 + d[1], p.z as i64 + d[2]]) {
                let c = &self.cells[n as usize];
                if c.kind == SURFACE && c.flags & SHOWN != 0 {
                    self.newly.push(n);
                }
            }
        }
    }

    fn show(&mut self, id: u32) {
        let c = &mut self.cells[id as usize];
        if c.flags & SHOWN != 0 {
            return;
        }
        c.flags |= SHOWN;
        let c = *c;
        self.touch(&c);
        self.newly.push(id);
        self.changes += 1;
    }

    /// Reads the corners of cells not read yet.
    fn read(&mut self, f: &Field, pool: &TaskPool, ids: &[u32], stale: Stale) -> Option<()> {
        let inherited = std::mem::take(&mut self.inherited);
        let keys: Vec<u64> = ids.iter().chain(&inherited).filter(|id| self.cells[**id as usize].kind == UNREAD).flat_map(|id| { let c = self.cells[*id as usize]; (0..8).map(move |k| corner(&c, k)) }).collect();
        if self.ensure(f, pool, &keys, stale).is_none() {
            self.inherited.extend(inherited);
            return None;
        }
        for id in ids.iter().chain(&inherited).copied() {
            let c = self.cells[id as usize];
            if c.kind != UNREAD {
                continue;
            }
            let signs = (0..8).fold(0u8, |m, k| if self.values[&corner(&c, k)] < 0.0 { m | 1 << k } else { m });
            let surface = signs != 0 && signs != 255;
            let cell = &mut self.cells[id as usize];
            cell.signs = signs;
            cell.kind = if surface { SURFACE } else { OPEN };
            if surface {
                self.unplaced.push(id);
                if c.flags & INHERIT != 0 {
                    self.show(id);
                }
            }
        }
        Some(())
    }

    /// Rays landed: the cells that hold them exist and show. `splits`: cells that strayed from the
    /// surface, split unless down to their floor.
    fn grow(&mut self, f: &Field, pool: &TaskPool, mut ways: Vec<(u32, Landing)>, splits: &[(V, f64)], stale: Stale) -> Option<()> {
        for (p, floor) in splits {
            let id = self.leaf_at(*p);
            let c = self.cells[id as usize];
            if self.size(c.depth) > *floor && c.depth as u32 + 1 < BITS {
                // its children take over what it shows
                self.cells[id as usize].flags |= SHOWN;
                self.split(id);
            }
        }
        let mut read = Vec::new();
        let mut waiting: Vec<(u32, Landing)> = Vec::new();
        loop {
            read.clear();
            waiting.clear();
            for (at, l) in ways.drain(..) {
                let mut n = at;
                loop {
                    let c = self.cells[n as usize];
                    if c.kids != 0 {
                        n = c.kids + self.octant(&c, l.p);
                        continue;
                    }
                    if self.size(c.depth) > l.side && c.depth as u32 + 1 < BITS {
                        n = self.split(n) + self.octant(&c, l.p);
                        continue;
                    }
                    break;
                }
                if self.cells[n as usize].kind == UNREAD {
                    read.push(n);
                }
                waiting.push((n, l));
            }
            read.sort_unstable();
            read.dedup();
            self.read(f, pool, &read, stale)?;
            for (n, l) in waiting.iter().copied() {
                let c = self.cells[n as usize];
                if c.kids != 0 {
                    ways.push((n, l));
                } else if c.kind == SURFACE {
                    self.show(n);
                } else if self.size(c.depth) > l.floor && c.depth as u32 + 1 < BITS {
                    // the surface a ray landed on is thinner than the cell
                    ways.push((n, Landing { side: self.size(c.depth) * 0.5, ..l }));
                }
            }
            if ways.is_empty() {
                return Some(());
            }
        }
    }

    /// The chunks whose quads can read this cell.
    fn touch(&mut self, c: &Cell) {
        let s = BITS - c.depth as u32;
        let shift = BITS - SEGMENT_DEPTH;
        let last = (1u64 << SEGMENT_DEPTH) - 1;
        let range = |x: u32| {
            let lo = (x as u64) << s;
            let hi = (x as u64 + 1) << s;
            (lo.saturating_sub(1) >> shift).min(last)..=(hi >> shift).min(last)
        };
        for k in range(c.z) {
            for j in range(c.y) {
                for i in range(c.x) {
                    let (at, bit) = ((i >> 2) + (j >> 2) * 8 + (k >> 2) * 64, (i & 3) + (j & 3) * 4 + (k & 3) * 16);
                    self.dirty[at as usize] |= 1 << bit;
                }
            }
        }
    }

    /// Whether a change is waiting to be shown.
    pub fn pending(&self) -> bool {
        self.dirty.iter().any(|d| *d != 0) || !self.unplaced.is_empty() || !self.newly.is_empty()
    }

    /// Vertices for the new surface leaves: the middle of their edges' crossings, then one step
    /// along the gradient onto the surface.
    fn place(&mut self, f: &Field, pool: &TaskPool, coarse: Coarse, stale: Stale) -> Option<()> {
        struct Job {
            id: u32,
            p: V,
            n: V,
            lo: V,
            hi: V,
            slot: u8,
            cut: u16,
            ord: u16,
            /// The gradient where it landed, in the corners' measure.
            g: V,
        }
        let ids = std::mem::take(&mut self.unplaced);
        let mut jobs: Vec<Job> = Vec::with_capacity(ids.len());
        for id in ids.iter().copied() {
            let c = self.cells[id as usize];
            if c.kids != 0 || c.kind != SURFACE || c.flags & PLACED != 0 {
                continue;
            }
            let h = self.size(c.depth);
            let (lo, hi) = self.bounds(&c);
            let v: [f32; 8] = std::array::from_fn(|k| self.values[&corner(&c, k)]);
            let (mut sum, mut n) = ([0.0; 3], 0.0);
            for a in 0..3 {
                let (b, e) = ((a + 1) % 3, (a + 2) % 3);
                for pe in 0..2usize {
                    for pb in 0..2usize {
                        let k0 = (pb << b) | (pe << e);
                        let k1 = k0 | (1 << a);
                        if (v[k0] < 0.0) == (v[k1] < 0.0) {
                            continue;
                        }
                        let mut at = [0.0; 3];
                        at[b] = pb as f64;
                        at[e] = pe as f64;
                        at[a] = (v[k0] as f64 / (v[k0] as f64 - v[k1] as f64)).clamp(0.0, 1.0);
                        sum = add(sum, at);
                        n += 1.0;
                    }
                }
            }
            let p = add(lo, mul(sum, h / f64::max(n, 1.0)));
            let g = h * 0.25;
            jobs.push(Job { id, p, n: [0.0, 0.0, 1.0], lo: sub(lo, [g; 3]), hi: add(hi, [g; 3]), slot: 0, cut: 0, ord: f.ids.constant, g: [0.0; 3] });
        }
        par(pool, &mut jobs, BATCH, |c| {
            if stale() {
                return;
            }
            let pts: Vec<V> = c.iter().map(|j| j.p).collect();
            let (v, grad) = f.gradient(&pts);
            for ((j, v), g) in c.iter_mut().zip(v).zip(grad) {
                let gg = dot(g, g);
                // where the field has no slope there is no step to take
                if gg.is_normal() {
                    j.n = mul(g, 1.0 / gg.sqrt());
                    j.g = mul(g, f.step);
                    let q = sub(j.p, mul(g, v / gg));
                    j.p = std::array::from_fn(|i| q[i].clamp(j.lo[i], j.hi[i]));
                }
            }
        });
        if stale() {
            self.unplaced.extend(ids);
            return None;
        }
        if f.slots || f.sides.is_some() {
            par(pool, &mut jobs, BATCH / 4, |c| {
                if stale() {
                    return;
                }
                if f.slots {
                    let under: Vec<V> = c.iter().map(|j| sub(j.p, mul(j.n, f.cell * 0.25))).collect();
                    for (j, s) in c.iter_mut().zip(f.slots_at(&under)) {
                        j.slot = s;
                    }
                }
                if f.sides.is_some() {
                    let at: Vec<V> = c.iter().map(|j| j.p).collect();
                    for (j, v) in c.iter_mut().zip(f.cuts_at(&at)) {
                        j.cut = v;
                    }
                }
            });
            if stale() {
                self.unplaced.extend(ids);
                return None;
            }
        }
        // a part of several features: the one each vertex is on, once per cell
        if f.has_ids() {
            par(pool, &mut jobs, BATCH / 4, |c| {
                if stale() {
                    return;
                }
                let at: Vec<V> = c.iter().map(|j| j.p).collect();
                for (j, o) in c.iter_mut().zip(f.ords_at(&at)) {
                    j.ord = o;
                }
            });
            if stale() {
                self.unplaced.extend(ids);
                return None;
            }
        }
        for j in &jobs {
            let cell = self.cells[j.id as usize];
            let h = self.size(cell.depth);
            let (lo, _) = self.bounds(&cell);
            let off = |k: usize| {
                let at = add(lo, [(k & 1) as f64 * h, ((k >> 1) & 1) as f64 * h, ((k >> 2) & 1) as f64 * h]);
                (dot(sub(at, j.p), j.g) - self.values[&corner(&cell, k)] as f64).abs()
            };
            let stray = ((0..8).map(off).fold(0.0, f64::max) / h * STRAY_PARTS).ceil().min(255.0) as u8;
            if too_coarse(cell.depth, h, stray, coarse(j.p)) {
                self.rough.push(j.id);
            }
            let c = &mut self.cells[j.id as usize];
            c.stray = stray;
            c.p = j.p.map(|x| x as f32);
            c.n = j.n.map(|x| x as f32);
            c.slot = j.slot;
            c.cut = j.cut;
            c.ord = j.ord;
            c.flags |= PLACED;
            let c = *c;
            self.touch(&c);
            if c.flags & SHOWN == 0 {
                self.ring.push(j.id);
            }
        }
        Some(())
    }

    /// Shows the cells of the ring from `from` on that face just past edge-on, and drops those that
    /// have shown or split since.
    fn show_edge_on(&mut self, from: usize, edge_on: EdgeOn) {
        let mut i = from;
        while i < self.ring.len() {
            let id = self.ring[i];
            let c = self.cells[id as usize];
            let gone = c.kids != 0 || c.flags & SHOWN != 0;
            if gone || edge_on(c.p.map(|x| x as f64), c.n.map(|x| x as f64)) {
                if !gone {
                    self.show(id);
                }
                self.ring.swap_remove(i);
            } else {
                i += 1;
            }
        }
    }

    /// Cells placed for a view from further away, too coarse for this one: `finest` is the least
    /// `coarse` gives anywhere.
    fn find_rough(&mut self, finest: f64, coarse: Coarse) {
        let mut stack = vec![0u32];
        while let Some(n) = stack.pop() {
            let c = self.cells[n as usize];
            let h = self.size(c.depth);
            if c.kids != 0 {
                if h * 0.5 > finest {
                    stack.extend((0..8).map(|k| c.kids + k));
                }
            } else if c.kind == SURFACE && c.flags & PLACED != 0 && too_coarse(c.depth, h, c.stray, coarse(c.p.map(|x| x as f64))) {
                self.rough.push(n);
            }
        }
    }

    /// The cell that holds lattice cell (x, y, z) of `depth`, or that is split finer.
    fn around(&self, depth: u32, at: [i64; 3]) -> Around {
        let size = 1i64 << depth;
        if at.iter().any(|v| *v < 0 || *v >= size) {
            return Around::Outside;
        }
        let mut n = 0u32;
        for d in 0..depth {
            let c = &self.cells[n as usize];
            if c.kids == 0 {
                return Around::Leaf(n);
            }
            let s = depth - 1 - d;
            let bit = ((at[0] >> s) & 1) | (((at[1] >> s) & 1) << 1) | (((at[2] >> s) & 1) << 2);
            n = c.kids + bit as u32;
        }
        if self.cells[n as usize].kids != 0 {
            Around::Finer
        } else {
            Around::Leaf(n)
        }
    }

    /// The quads this placed leaf owns: a sign change along one of its edges, the edge no finer cell
    /// cuts, of the cells around it this the first of the finest, and all four placed. An edge whose
    /// cells are all placed is the mesh's, whichever of them a ray showed: no pinhole is left where
    /// the rays between landings were covered. With no `data`, a shown leaf's walk that reads:
    /// `reach`, cells around such an edge not read down to it yet, which the first of the finest
    /// cells that show reads.
    fn quads_of(&self, id: u32, mut data: Option<&mut Vec<f32>>, reach: &mut Vec<([i64; 3], u8)>, mut finer: Option<&mut Vec<Cut>>) {
        let c = &self.cells[id as usize];
        let depth = c.depth as u32;
        let at = [c.x as i64, c.y as i64, c.z as i64];
        for a in 0..3usize {
            let (b, e) = ((a + 1) % 3, (a + 2) % 3);
            for pe in 0..2usize {
                'edge: for pb in 0..2usize {
                    let k0 = (pb << b) | (pe << e);
                    let k1 = k0 | (1 << a);
                    let (s0, s1) = ((c.signs >> k0) & 1, (c.signs >> k1) & 1);
                    if s0 == s1 {
                        continue;
                    }
                    let (eb, ee) = (at[b] + pb as i64, at[e] + pe as i64);
                    let me = (1 - pb) + 2 * (1 - pe);
                    let mut around = [0u32; 4];
                    let mut places = [[0i64; 3]; 4];
                    for i in 0..4usize {
                        if i == me {
                            around[i] = id;
                            continue;
                        }
                        let mut q = at;
                        q[b] = eb - 1 + (i & 1) as i64;
                        q[e] = ee - 1 + (i >> 1) as i64;
                        places[i] = q;
                        match self.around(depth, q) {
                            Around::Leaf(n) => {
                                let o = &self.cells[n as usize];
                                if o.depth as u32 == depth && i < me && (data.is_some() || o.flags & SHOWN != 0) {
                                    continue 'edge;
                                }
                                // drawn, an edge with a cell not placed has no quad
                                if data.is_some() && (o.kind != SURFACE || o.flags & PLACED == 0) {
                                    continue 'edge;
                                }
                                around[i] = n;
                            }
                            // a finer cell owns this edge's quads: it must show
                            Around::Finer => {
                                if let Some(cuts) = finer.as_deref_mut() {
                                    cuts.push(Cut { at: q, depth: depth as u8, b: b as u8, e: e as u8, side_b: 1 - (i & 1) as u8, side_e: 1 - (i >> 1) as u8 });
                                }
                                continue 'edge;
                            }
                            Around::Outside => continue 'edge,
                        }
                    }
                    let mut whole = true;
                    for i in 0..4usize {
                        let o = &self.cells[around[i] as usize];
                        if o.kind != SURFACE {
                            reach.push((places[i], depth as u8));
                            whole = false;
                        } else if o.flags & PLACED == 0 {
                            whole = false;
                        }
                    }
                    let Some(data) = data.as_deref_mut() else { continue };
                    if !whole {
                        continue;
                    }
                    // around the edge, counterclockwise from its high end when its low end is inside
                    let order = if s0 == 1 { [around[0], around[1], around[3], around[2]] } else { [around[2], around[3], around[1], around[0]] };
                    let v: [&Cell; 4] = order.map(|n| &self.cells[n as usize]);
                    let mut out = [0.0f32; TILE_SLOTS];
                    let (mut pc, mut nc) = ([0.0f64; 3], [0.0f64; 3]);
                    for (k, o) in v.iter().enumerate() {
                        out[k * 4..k * 4 + 3].copy_from_slice(&o.p);
                        let n = o.n.map(|x| x as f64);
                        out[k * 4 + 3] = oct_pack(n);
                        pc = add(pc, o.p.map(|x| x as f64));
                        nc = add(nc, n);
                    }
                    let pc = mul(pc, 0.25);
                    out[16] = pc[0] as f32;
                    out[17] = pc[1] as f32;
                    out[18] = pc[2] as f32;
                    out[19] = oct_pack(norm(nc));
                    out[20] = c.slot as f32;
                    out[21] = pack_cut(v[0].cut, v[1].cut);
                    out[22] = pack_cut(v[2].cut, v[3].cut);
                    out[23] = c.ord as f32;
                    data.extend_from_slice(&out);
                }
            }
        }
    }

    /// Reads the cells around the newly shown ones' edges, and sets the vertices of all that is new.
    fn settle(&mut self, f: &Field, pool: &TaskPool, coarse: Coarse, edge_on: EdgeOn, stale: Stale) -> Option<()> {
        loop {
            let newly = std::mem::take(&mut self.newly);
            let (mut reach, mut cuts) = (Vec::new(), Vec::new());
            for id in newly.iter().copied() {
                if self.cells[id as usize].kids == 0 {
                    self.quads_of(id, None, &mut reach, Some(&mut cuts));
                }
            }
            if reach.is_empty() && self.inherited.is_empty() && cuts.is_empty() {
                let from = self.ring.len();
                self.place(f, pool, coarse, stale)?;
                self.show_edge_on(from, edge_on);
                if self.rough.is_empty() && self.newly.is_empty() {
                    return Some(());
                }
                for id in std::mem::take(&mut self.rough) {
                    if self.cells[id as usize].kids == 0 {
                        // its children take over what it shows
                        self.cells[id as usize].flags |= SHOWN;
                        self.split(id);
                    }
                }
                continue;
            }
            // the leaves along a shown cell's edges where a neighbour is finer
            let mut along = Vec::new();
            for cut in cuts {
                let mut n = 0u32;
                for d in 0..cut.depth as u32 {
                    let s = cut.depth as u32 - 1 - d;
                    n = self.cells[n as usize].kids + (((cut.at[0] >> s) & 1) | (((cut.at[1] >> s) & 1) << 1) | (((cut.at[2] >> s) & 1) << 2)) as u32;
                }
                let mut stack = vec![n];
                while let Some(n) = stack.pop() {
                    let c = &self.cells[n as usize];
                    if c.kids == 0 {
                        along.push(n);
                        continue;
                    }
                    for k in 0..8u32 {
                        if (k >> cut.b) & 1 == cut.side_b as u32 && (k >> cut.e) & 1 == cut.side_e as u32 {
                            stack.push(c.kids + k);
                        }
                    }
                }
            }
            along.sort_unstable();
            along.dedup();
            // stopped, the walk is owed again: what it did so far it finds done
            if self.read(f, pool, &along, stale).is_none() {
                self.newly.extend(newly);
                return None;
            }
            for id in along {
                if self.cells[id as usize].kind == SURFACE {
                    self.show(id);
                }
            }
            reach.sort_unstable();
            reach.dedup();
            let mut read = Vec::with_capacity(reach.len());
            for (q, depth) in reach {
                let mut n = 0u32;
                for d in 0..depth as u32 {
                    let c = self.cells[n as usize];
                    let kids = if c.kids == 0 { self.split(n) } else { c.kids };
                    let s = depth as u32 - 1 - d;
                    n = kids + (((q[0] >> s) & 1) | (((q[1] >> s) & 1) << 1) | (((q[2] >> s) & 1) << 2)) as u32;
                }
                read.push(n);
            }
            if self.read(f, pool, &read, stale).is_none() {
                self.newly.extend(newly);
                return None;
            }
        }
    }

    /// The quads of one segment. A leaf larger than a segment belongs to the one at its low corner.
    fn segment_quads(&self, at: [usize; 3], data: &mut Vec<f32>, reach: &mut Vec<([i64; 3], u8)>) {
        let mut n = 0u32;
        for d in 0..SEGMENT_DEPTH {
            let c = &self.cells[n as usize];
            if c.kids == 0 {
                let s = SEGMENT_DEPTH - d;
                if c.kind == SURFACE && c.flags & PLACED != 0 && (c.x as usize) << s == at[0] && (c.y as usize) << s == at[1] && (c.z as usize) << s == at[2] {
                    self.quads_of(n, Some(data), reach, None);
                }
                return;
            }
            let s = SEGMENT_DEPTH - 1 - d;
            n = c.kids + (((at[0] >> s) & 1) | (((at[1] >> s) & 1) << 1) | (((at[2] >> s) & 1) << 2)) as u32;
        }
        let mut stack = vec![n];
        while let Some(n) = stack.pop() {
            let c = &self.cells[n as usize];
            if c.kids != 0 {
                stack.extend((0..8).map(|k| c.kids + k));
            } else if c.kind == SURFACE && c.flags & PLACED != 0 {
                self.quads_of(n, Some(data), reach, None);
            }
        }
    }

    /// The chunks that changed, to draw.
    fn emit(&mut self, pool: &TaskPool) -> Vec<(usize, Option<Chunk>)> {
        struct Job {
            chunk: u32,
            bit: u8,
            data: Vec<f32>,
            reach: Vec<([i64; 3], u8)>,
            quads: QuadBox,
        }
        let mut jobs: Vec<Job> = Vec::new();
        let mut chunks = Vec::new();
        for chunk in 0..CHUNKS {
            let mut left = std::mem::take(&mut self.dirty[chunk]);
            if left != 0 {
                chunks.push(chunk);
            }
            while left != 0 {
                let bit = left.trailing_zeros();
                left &= left - 1;
                jobs.push(Job { chunk: chunk as u32, bit: bit as u8, data: Vec::new(), reach: Vec::new(), quads: QuadBox::EMPTY });
            }
        }
        {
            let me = &*self;
            par(pool, &mut jobs, 16, |c| {
                for j in c.iter_mut() {
                    let (ch, bit) = (j.chunk as usize, j.bit as usize);
                    let at = [(ch & 7) << 2 | (bit & 3), ((ch >> 3) & 7) << 2 | ((bit >> 2) & 3), (ch >> 6) << 2 | (bit >> 4)];
                    me.segment_quads(at, &mut j.data, &mut j.reach);
                    j.quads = QuadBox::of(&j.data);
                }
            });
        }
        self.emits += 1;
        for j in jobs {
            self.boxes[j.chunk as usize].take_in(&j.quads);
            let at = j.chunk as usize * 64 + j.bit as usize;
            self.seg_boxes[at].take_in(&j.quads);
            self.shown_at[at] = self.emits;
            let seg = if j.data.is_empty() { None } else { Some(InstanceSegment::new(Arc::from(j.data))) };
            if seg.is_some() || self.segments[j.chunk as usize].is_some() {
                self.segments[j.chunk as usize].get_or_insert_with(|| Box::new(std::array::from_fn(|_| None)))[j.bit as usize] = seg;
            }
        }
        let mut out = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            let list: Vec<InstanceSegment> = self.segments[chunk].iter().flat_map(|s| s.iter().flatten().cloned()).collect();
            let inst = if list.is_empty() { None } else { RetainedInstances::from_segments(TILE_SLOTS, list).ok() }.map(|tiles| Chunk { tiles, quads: self.boxes[chunk] });
            self.shown[chunk] = inst.clone();
            out.push((chunk, inst));
        }
        out
    }

    pub fn held(&self) -> Held {
        let quads = self.shown.iter().flatten().map(|d| d.count()).sum();
        Held {
            cells: self.cells.len(),
            quads,
            chunks: self.shown.iter().flatten().count(),
            cpu_bytes: self.cells.capacity() * std::mem::size_of::<Cell>() + self.values.capacity() * (std::mem::size_of::<(u64, f32)>() + 1) + self.ring.capacity() * std::mem::size_of::<u32>(),
            gpu_bytes: quads * TILE_SLOTS * 4,
        }
    }

    fn chunk_in_view(&self, cam: &Cam, chunk: usize) -> bool {
        let h = self.size(CHUNK_DEPTH as u8);
        let at = |i: usize| (i as f64 + 0.5) * h;
        cam.sees(add(self.lo, [at(chunk & 7), at((chunk >> 3) & 7), at(chunk >> 6)]), h * HALF_DIAG)
    }
}

/// The mesh's nearest depth on a grid of the view, front faces only, as it draws. No quad at a
/// sample: the mesh shows nothing there. Filled again for the view and surface it shows, it draws
/// again only the tiles of the grid that segments shown since reach.
#[derive(Default)]
pub struct Cover {
    w: usize,
    h: usize,
    step: f64,
    /// The samples, row by row `w` wide (the buffer is the largest grid's: the pixel stage's grid
    /// and the coarser rounds' take turns on it). Outside what `rows` holds, every sample is bare.
    depth: Vec<AtomicU32>,
    /// Per row, the columns its quads reach, first and past the last (none: `u32::MAX`, 0), or
    /// more: where a sample of the row can show anything.
    rows: Vec<(AtomicU32, AtomicU32)>,
    /// What it shows: the view, the surface and that surface's emits.
    of: Option<(Cam, u64, u64)>,
    /// Per tile of the grid, whether the last fill drew it again; empty when it drew all.
    again: Vec<bool>,
}

const UNCOVERED: u32 = 0x7f80_0000;
/// A tile of the grid, drawn again on its own: its side in samples, as a shift.
const COVER_TILE: usize = 3;

impl Cover {
    pub(crate) fn fill(&mut self, cam: &Cam, step: f64, s: &Surface, pool: &TaskPool) {
        let (w, h) = ((cam.w / step).ceil() as usize, (cam.h / step).ceil() as usize);
        let since = self.of.filter(|o| o.0 == *cam && o.1 == s.id && self.step == step && (w, h) == (self.w, self.h)).map(|o| o.2);
        self.of = Some((*cam, s.id, s.emits));
        self.step = step;
        self.again.clear();
        if let Some(since) = since {
            // the tiles the segments shown since reach, with all they held before (their boxes only grow)
            let tw = w.div_ceil(1 << COVER_TILE);
            self.again.resize(tw * h.div_ceil(1 << COVER_TILE), false);
            for i in (0..CHUNKS).filter(|&i| s.chunk_in_view(cam, i)) {
                for at in (i * 64..i * 64 + 64).filter(|&at| s.shown_at[at] > since) {
                    if let Some([x0, y0, x1, y1]) = self.tiles_of(cam, &s.seg_boxes[at]) {
                        for y in y0..=y1 {
                            self.again[y * tw + x0..=y * tw + x1].fill(true);
                        }
                    }
                }
            }
            let n = self.again.iter().filter(|a| **a).count();
            if n == 0 {
                return;
            }
            // most of the grid is drawn whole, as a new view is
            if n * 2 <= self.again.len() {
                self.draw_again(cam, s, pool);
                return;
            }
            self.again.clear();
        }
        // bare again where the last fill's quads reached (on its grid), and nowhere else: a view
        // that shows little of the part costs little
        for (y, (lo, hi)) in self.rows.iter().enumerate().take(self.h) {
            let (lo, hi) = (lo.swap(u32::MAX, Ordering::Relaxed) as usize, hi.swap(0, Ordering::Relaxed) as usize);
            if lo < hi {
                self.depth[y * self.w + lo..y * self.w + hi].iter().for_each(|d| d.store(UNCOVERED, Ordering::Relaxed));
            }
        }
        (self.w, self.h) = (w, h);
        if self.depth.len() < w * h {
            self.depth.resize_with(w * h, || AtomicU32::new(UNCOVERED));
        }
        if self.rows.len() < h {
            self.rows.resize_with(h, || (AtomicU32::new(u32::MAX), AtomicU32::new(0)));
        }
        let mut jobs: Vec<&[f32]> = (0..CHUNKS).filter(|&i| s.chunk_in_view(cam, i)).filter_map(|i| s.shown[i].as_ref()).flat_map(|c| c.segments().iter().map(|g| g.data())).collect();
        self.draw(cam, &mut jobs, pool);
    }

    /// The tiles marked again: cleared, and every segment in view that draws on them drawn.
    fn draw_again(&self, cam: &Cam, s: &Surface, pool: &TaskPool) {
        let (w, h, side) = (self.w, self.h, 1 << COVER_TILE);
        let tw = w.div_ceil(side);
        for (k, _) in self.again.iter().enumerate().filter(|(_, a)| **a) {
            let (x0, y0) = ((k % tw) * side, (k / tw) * side);
            for y in y0..(y0 + side).min(h) {
                self.depth[y * w + x0..y * w + (x0 + side).min(w)].iter().for_each(|d| d.store(UNCOVERED, Ordering::Relaxed));
            }
        }
        let reaches = |b: &QuadBox| self.tiles_of(cam, b).is_some_and(|[x0, y0, x1, y1]| (y0..=y1).any(|y| self.again[y * tw + x0..=y * tw + x1].contains(&true)));
        let mut jobs: Vec<&[f32]> = Vec::new();
        for i in (0..CHUNKS).filter(|&i| s.chunk_in_view(cam, i) && s.shown[i].is_some() && reaches(&s.boxes[i])) {
            let Some(segs) = s.segments[i].as_deref() else { continue };
            for (b, g) in segs.iter().enumerate() {
                if let Some(g) = g.as_ref().filter(|_| reaches(&s.seg_boxes[i * 64 + b])) {
                    jobs.push(g.data());
                }
            }
        }
        self.draw(cam, &mut jobs, pool);
    }

    fn draw(&self, cam: &Cam, jobs: &mut [&[f32]], pool: &TaskPool) {
        par(pool, jobs, 4, |c| {
            for data in c.iter() {
                for q in data.chunks_exact(TILE_SLOTS) {
                    self.quad(cam, q);
                }
            }
        });
    }

    /// The tiles of the grid a box draws on, first and last across and down: all of them when a
    /// corner is behind the eye, none when it is empty or off the grid.
    fn tiles_of(&self, cam: &Cam, b: &QuadBox) -> Option<[usize; 4]> {
        if b.lo[0] > b.hi[0] {
            return None;
        }
        let (lo, hi) = (b.lo.map(|x| x as f64), b.hi.map(|x| x as f64));
        let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for k in 0..8 {
            let p = [if k & 1 == 0 { lo[0] } else { hi[0] }, if k & 2 == 0 { lo[1] } else { hi[1] }, if k & 4 == 0 { lo[2] } else { hi[2] }];
            let Some((x, y, _)) = cam.proj(p) else {
                return Some([0, 0, (self.w - 1) >> COVER_TILE, (self.h - 1) >> COVER_TILE]);
            };
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        // a sample past either side: a triangle's samples are those its box rounds out to
        let s = 1.0 / self.step;
        let (x0, y0, x1, y1) = ((x0 * s).floor() - 1.0, (y0 * s).floor() - 1.0, (x1 * s).ceil() + 1.0, (y1 * s).ceil() + 1.0);
        if x1 < 0.0 || y1 < 0.0 || x0 >= self.w as f64 || y0 >= self.h as f64 {
            return None;
        }
        let tile = |v: f64, n: usize| (v.max(0.0) as usize).min(n - 1) >> COVER_TILE;
        Some([tile(x0, self.w), tile(y0, self.h), tile(x1, self.w), tile(y1, self.h)])
    }

    /// Whether a fill draws on the sample: all of them when it draws all.
    fn draws(&self, i: usize, j: usize) -> bool {
        self.again.is_empty() || self.again[(j >> COVER_TILE) * self.w.div_ceil(1 << COVER_TILE) + (i >> COVER_TILE)]
    }

    /// As it draws, one-sided: a fan around its centre, each triangle by its winding. Its
    /// vertices' normals do not say which way it faces: where they part (a quad from one side of a
    /// thin wall to the other) the triangles are seen all the same.
    fn quad(&self, cam: &Cam, q: &[f32]) {
        let at = |k: usize| -> V { [q[k * 4] as f64, q[k * 4 + 1] as f64, q[k * 4 + 2] as f64] };
        let Some(c) = cam.proj(at(4)) else { return };
        let mut p = [(0.0, 0.0, 0.0); 4];
        for (k, v) in p.iter_mut().enumerate() {
            let Some(q) = cam.proj(at(k)) else { return };
            *v = q;
        }
        // within half a sample of its middle all round, it can hold only the sample nearest its
        // middle, and its triangles say whether it does: the samples between such quads are
        // theirs as much as any
        let s = 1.0 / self.step;
        let reach = p.iter().map(|q| (q.0 - c.0) * (q.0 - c.0) + (q.1 - c.1) * (q.1 - c.1)).fold(0.0f64, f64::max) * s * s;
        let only = (reach < 0.25).then(|| ((c.0 * s).floor(), (c.1 * s).floor()));
        for k in 0..4 {
            self.tri(cam, c, p[k], p[(k + 1) % 4], only);
        }
    }

    /// A triangle's samples; `only`: just this one, the one it can hold.
    fn tri(&self, cam: &Cam, a: (f64, f64, f64), b: (f64, f64, f64), c: (f64, f64, f64), only: Option<(f64, f64)>) {
        let s = 1.0 / self.step;
        let (ax, ay, bx, by, cx, cy) = (a.0 * s, a.1 * s, b.0 * s, b.1 * s, c.0 * s, c.1 * s);
        // the view's y runs down: a face toward the view winds negative
        let area = (bx - ax) * (cy - ay) - (cx - ax) * (by - ay);
        if area >= 0.0 {
            return;
        }
        let (x0, y0, x1, y1) = match only {
            Some((x, y)) => (x.max(0.0) as usize, y.max(0.0) as usize, (x as isize).min(self.w as isize - 1), (y as isize).min(self.h as isize - 1)),
            None => (ax.min(bx).min(cx).floor().max(0.0) as usize, ay.min(by).min(cy).floor().max(0.0) as usize, (ax.max(bx).max(cx).ceil() as isize).min(self.w as isize - 1), (ay.max(by).max(cy).ceil() as isize).min(self.h as isize - 1)),
        };
        if x1 < 0 || y1 < 0 || x0 as isize > x1 || y0 as isize > y1 {
            return;
        }
        let clip = !self.again.is_empty();
        for y in y0..=y1 as usize {
            self.rows[y].0.fetch_min(x0 as u32, Ordering::Relaxed);
            self.rows[y].1.fetch_max(x1 as u32 + 1, Ordering::Relaxed);
            let py = y as f64 + 0.5;
            for x in x0..=x1 as usize {
                if clip && !self.draws(x, y) {
                    continue;
                }
                let px = x as f64 + 0.5;
                let e0 = (bx - ax) * (py - ay) - (by - ay) * (px - ax);
                let e1 = (cx - bx) * (py - by) - (cy - by) * (px - bx);
                let e2 = (ax - cx) * (py - cy) - (ay - cy) * (px - cx);
                if e0 <= 0.0 && e1 <= 0.0 && e2 <= 0.0 {
                    let (la, lb, lc) = (e1 / area, e2 / area, e0 / area);
                    let z = if cam.ortho { la * a.2 + lb * b.2 + lc * c.2 } else { 1.0 / (la / a.2 + lb / b.2 + lc / c.2) };
                    self.depth[y * self.w + x].fetch_min((z as f32).to_bits(), Ordering::Relaxed);
                }
            }
        }
    }

    pub(crate) fn at(&self, i: usize, j: usize) -> f32 {
        f32::from_bits(self.depth[j * self.w + i].load(Ordering::Relaxed))
    }

    /// Whether a neighbour on the grid shows something nearer than `z` by more than `by`.
    fn beyond_a_jump(&self, i: usize, j: usize, z: f32, by: f64) -> bool {
        let near = z - by as f32;
        (i > 0 && self.at(i - 1, j) < near) || (i + 1 < self.w && self.at(i + 1, j) < near) || (j > 0 && self.at(i, j - 1) < near) || (j + 1 < self.h && self.at(i, j + 1) < near)
    }

    /// The columns of row `j` where a sample can be in a gap ([`Cover::in_a_gap`]): between what
    /// its own row shows, or between what the rows within `reach` above and below it both show.
    fn gap_columns(&self, j: usize, reach: usize) -> std::ops::Range<usize> {
        let span = |rows: std::ops::Range<usize>| {
            self.rows[rows].iter().fold((usize::MAX, 0), |(lo, hi), r| (lo.min(r.0.load(Ordering::Relaxed) as usize), hi.max(r.1.load(Ordering::Relaxed) as usize)))
        };
        let own = span(j..j + 1);
        let (above, below) = (span(j.saturating_sub(reach)..j), span(j + 1..(j + 1 + reach).min(self.h)));
        let (lo, hi) = (above.0.max(below.0), above.1.min(below.1));
        let (lo, hi) = if lo < hi { (own.0.min(lo), own.1.max(hi)) } else { own };
        lo.min(self.w)..hi.min(self.w)
    }

    /// Whether the mesh shows something nearer than `z` by more than `by` on both sides of this
    /// sample, within `reach` along its row or its column: it is in a gap in what is nearer, as a
    /// hole in a wall is, too narrow for a coarser grid's samples to have fallen in. Bare (`z`
    /// infinite), anything shown is nearer.
    fn in_a_gap(&self, i: usize, j: usize, z: f32, by: f64, reach: usize) -> bool {
        let near = z - by as f32;
        let nearer = |x: usize, y: usize| self.at(x, y) < near;
        let (left, right) = ((1..=reach.min(i)).any(|k| nearer(i - k, j)), (1..=reach).take_while(|k| i + k < self.w).any(|k| nearer(i + k, j)));
        if left && right {
            return true;
        }
        (1..=reach.min(j)).any(|k| nearer(i, j - k)) && (1..=reach).take_while(|k| j + k < self.h).any(|k| nearer(i, j + k))
    }

    /// Whether a neighbour on the grid shows depth `z`, and this sample is on an edge of what it
    /// shows, not a hole in the middle of it: another neighbour shows nothing or is a jump `by`
    /// away. A steep surface's neighbours differ by more than `off` and are no edge.
    fn edge_beside(&self, i: usize, j: usize, z: f64, off: f64, by: f64) -> bool {
        let near = [(i > 0).then(|| (i - 1, j)), (i + 1 < self.w).then(|| (i + 1, j)), (j > 0).then(|| (i, j - 1)), (j + 1 < self.h).then(|| (i, j + 1))];
        let (mut shows, mut other) = (false, false);
        for (x, y) in near.into_iter().flatten() {
            let d = (self.at(x, y) as f64 - z).abs();
            shows |= d <= off;
            other |= !(d <= by);
        }
        shows && other
    }

    /// The nearest and farthest depth over a tile, in pixels; infinite with no quad in it.
    fn range(&self, x: u32, y: u32, size: u32) -> (f64, f64) {
        let g = self.step as u32;
        let (mut near, mut far) = (f64::INFINITY, 0.0f64);
        for j in (y / g) as usize..((y + size).div_ceil(g) as usize).min(self.h) {
            for i in (x / g) as usize..((x + size).div_ceil(g) as usize).min(self.w) {
                let z = self.at(i, j) as f64;
                if z.is_finite() {
                    near = near.min(z);
                    far = far.max(z);
                }
            }
        }
        (near, far)
    }
}

/// What looking at one view has cost so far.
#[derive(Default, Clone, Copy)]
pub struct Counts {
    /// Rays through samples no quad covers, or past a check that strayed.
    pub rays: u64,
    pub landed: u64,
    pub checks: u64,
    pub strayed: u64,
    pub cones: u64,
    /// Samples beyond a jump in the mesh: a ray, not a check.
    pub jumps: u64,
    /// Tiles where a cone stopped well in front of the mesh.
    pub front: u64,
    /// One-thread-of-a-pool wall time by phase, ms: the cover, checks and rays, growing, settling,
    /// the draw data.
    pub ms: [f64; 5],
}

/// The box round some quads, in the part's frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QuadBox {
    pub lo: [f32; 3],
    pub hi: [f32; 3],
}

impl QuadBox {
    pub(crate) const EMPTY: QuadBox = QuadBox { lo: [f32::MAX; 3], hi: [f32::MIN; 3] };

    /// `data`: quads as tiles, their four corners first.
    pub(crate) fn of(data: &[f32]) -> QuadBox {
        let mut b = QuadBox::EMPTY;
        for q in data.chunks_exact(TILE_SLOTS) {
            for p in q[..16].chunks_exact(4) {
                for i in 0..3 {
                    b.lo[i] = b.lo[i].min(p[i]);
                    b.hi[i] = b.hi[i].max(p[i]);
                }
            }
        }
        b
    }

    fn take_in(&mut self, o: &QuadBox) {
        for i in 0..3 {
            self.lo[i] = self.lo[i].min(o.lo[i]);
            self.hi[i] = self.hi[i].max(o.hi[i]);
        }
    }
}

/// A chunk of a part's quads, ready to draw, and the box a view can leave it out by.
#[derive(Clone, Debug)]
pub struct Chunk {
    pub tiles: RetainedInstances,
    pub quads: QuadBox,
}

impl std::ops::Deref for Chunk {
    type Target = RetainedInstances;
    fn deref(&self) -> &RetainedInstances {
        &self.tiles
    }
}

/// A round's result.
pub struct Step {
    pub chunks: Vec<(usize, Option<Chunk>)>,
    /// Nothing is left to do for this view: what the surface holds now can be shown from it, each
    /// pixel coarse at most, none showing what lies behind a wall in front of it.
    pub done: bool,
}

/// Looks at a view, round by round: rays and checks at a stride that halves, a cone per tile.
#[derive(Default)]
pub struct Gaze {
    view: Option<(Cam, u64)>,
    stride: u32,
    cover: Cover,
    /// Per sample: the view it last passed in, the depth that passed (NaN: a ray proved it
    /// clear), and whether a ray passed it (it knows what stands first) or a check (only that
    /// the mesh is on a surface there). The mesh moves as the view goes on, so a depth that
    /// passed may not stand, nor a check once the mesh round the sample comes nearer.
    passed: Vec<(u32, f32, bool)>,
    stamp: u32,
    roll: u32,
    /// The view's cones have gone out and what they found is in.
    probed: bool,
    /// The look's last stage, on a grid coarser than a pixel: a ray for each pixel in a gap in
    /// what is nearer ([`Cover::in_a_gap`]), on a grid of one. Between the coarser grid's samples a
    /// hole in a wall thinner than its cells is never asked about, and would show what is behind
    /// the wall.
    pixels: bool,
    pub counts: Counts,
}

/// The samples of a grid `step` px apart where the part's box can show, from one of every `n`th:
/// first and past the last, across and down. All of them when a corner is behind the eye.
fn box_samples(cam: &Cam, f: &Field, step: f64, w: usize, h: usize, n: usize) -> [usize; 4] {
    let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for k in 0..8 {
        let p = [if k & 1 == 0 { f.lo[0] } else { f.hi[0] }, if k & 2 == 0 { f.lo[1] } else { f.hi[1] }, if k & 4 == 0 { f.lo[2] } else { f.hi[2] }];
        let Some((x, y, _)) = cam.proj(p) else { return [0, w, 0, h] };
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
    }
    let first = |v: f64, len: usize| ((v / step).floor().max(0.0) as usize).min(len) / n * n;
    let past = |v: f64, len: usize| ((v / step).ceil().max(0.0) as usize + 1).min(len);
    [first(x0, w), past(x1, w), first(y0, h), past(y1, h)]
}

/// How far the part's box moved on screen between two views, in pixels.
fn moved(a: &Cam, b: &Cam, f: &Field) -> f64 {
    let mut most = 0.0f64;
    for k in 0..8 {
        let p = [if k & 1 == 0 { f.lo[0] } else { f.hi[0] }, if k & 2 == 0 { f.lo[1] } else { f.hi[1] }, if k & 4 == 0 { f.lo[2] } else { f.hi[2] }];
        match (a.proj(p), b.proj(p)) {
            (Some(x), Some(y)) => most = most.max((x.0 - y.0).abs()).max((x.1 - y.1).abs()),
            _ => return f64::INFINITY,
        }
    }
    most
}

struct Check {
    at: u32,
    z: f32,
    o: V,
    d: V,
    t: f64,
    t0: f64,
    t1: f64,
    v: f64,
}

fn ray(sample: u32, o: V, d: V, t: f64, t1: f64) -> Ray {
    Ray { sample, o, d, t, t1, prev_t: -1.0, prev_v: 0.0, lo: 0.0, hi: 0.0, lo_v: 0.0, hi_v: 0.0, phase: 0, polish: 0, best_t: t, best_v: f64::INFINITY, steps: 0, hit: false, done: false }
}

impl Gaze {
    /// How far its look at the view has come, 0..1: the rounds its stride has halved through,
    /// of those down to one sample of its grid and the last at that ([`Step::done`] tells the
    /// look is done).
    pub fn share(&self) -> f32 {
        let Some((_, leaf)) = self.view else { return 0.0 };
        let grid: u32 = if f64::from_bits(leaf) > 1.0 { 4 } else { 1 };
        let rounds = (FIRST_STRIDE / grid).trailing_zeros() + 1;
        let done = (FIRST_STRIDE / self.stride.clamp(grid, FIRST_STRIDE)).trailing_zeros();
        done as f32 / rounds as f32
    }

    /// One round for the view: what it found goes out, whether more rounds are owed.
    pub fn step(&mut self, s: &mut Surface, f: &Field, pool: &TaskPool, cam: Cam, tol: &Tolerance, stale: Stale) -> Option<Step> {
        let coarse: u32 = if tol.leaf > 1.0 { 4 } else { 1 };
        if self.view != Some((cam, tol.leaf.to_bits())) {
            // as far apart as the view moved: what was bare stays that near what was seen
            self.stride = match self.view {
                Some((old, leaf)) if leaf == tol.leaf.to_bits() => moved(&old, &cam, f).min(FIRST_STRIDE as f64).ceil().max(1.0).min(FIRST_STRIDE as f64) as u32,
                _ => FIRST_STRIDE,
            }
            .next_power_of_two()
            .max(coarse);
            self.view = Some((cam, tol.leaf.to_bits()));
            self.stamp += 1;
            self.roll += 1;
            self.counts = Counts::default();
            self.probed = false;
            self.pixels = false;
        }
        let grid = if self.pixels { 1 } else { coarse };
        let whole_side = WHOLE_STRIDE as f64 * CELL_SPACINGS;
        let coarse_at = |p: V| match cam.proj(p) {
            Some((x, y, z)) if x >= 0.0 && y >= 0.0 && x < cam.w && y < cam.h => whole_side * cam.pw(z.max(1e-6)),
            _ => f64::INFINITY,
        };
        let edge_on = |p: V, n: V| coarse_at(p).is_finite() && (0.0..=EDGE_ON).contains(&dot(cam.dir_to(p), n));
        // what a stopped round left is drawn first, as a round of its own: a ray landing on a cell
        // that shows but is not drawn yet would take it for too coarse, and split it to the leaf
        if s.pending() {
            s.settle(f, pool, &coarse_at, &edge_on, stale)?;
            let chunks = s.emit(pool);
            return Some(Step { chunks, done: false });
        }
        if !self.probed {
            let nearest = (0..8).map(|k| cam.depth([if k & 1 == 0 { f.lo[0] } else { f.hi[0] }, if k & 2 == 0 { f.lo[1] } else { f.hi[1] }, if k & 4 == 0 { f.lo[2] } else { f.hi[2] }])).fold(f64::INFINITY, f64::min);
            s.find_rough(whole_side * cam.pw(nearest.max(1e-6)), &coarse_at);
            s.show_edge_on(0, &edge_on);
            if !s.rough.is_empty() || !s.newly.is_empty() {
                s.settle(f, pool, &coarse_at, &edge_on, stale)?;
            }
        }
        let mut clock = Instant::now();
        let mut lap = |c: &mut Counts, i: usize| {
            c.ms[i] += clock.elapsed().as_secs_f64() * 1000.0;
            clock = Instant::now();
        };
        self.cover.fill(&cam, grid as f64, s, pool);
        // the pixel stage's grid and the coarser rounds' take turns: the table keeps the larger
        // (a sample passed in the other grid has an older stamp)
        if self.passed.len() < self.cover.w * self.cover.h {
            self.passed.resize(self.cover.w * self.cover.h, (0, 0.0, false));
        }
        lap(&mut self.counts, 0);
        let stride = if self.pixels { 1 } else { self.stride };
        let n = (stride / grid) as usize;
        let (w, h) = (self.cover.w, self.cover.h);
        let mut rays: Vec<Ray> = Vec::new();
        let mut checks: Vec<Check> = Vec::new();
        if self.pixels {
            // the last stage asks a ray of each pixel in a gap in what is nearer, too narrow for the
            // coarser rounds' samples to have fallen in, that none of its rays asked yet: rows at
            // once, as the whole view is to be looked over, each where what the mesh shows can
            // hold a gap (a view showing little of the part looks at little)
            let (cover, passed, stamp) = (&self.cover, &self.passed, self.stamp);
            let mut found: Vec<(usize, Vec<u32>)> = (0..h).map(|j| (j, Vec::new())).collect();
            par(pool, &mut found, ROWS, |rows| {
                for (j, row) in rows.iter_mut() {
                    for i in cover.gap_columns(*j, coarse as usize) {
                        let at = *j * w + i;
                        let z = cover.at(i, *j);
                        let (when, depth, by_ray) = passed[at];
                        if when == stamp && by_ray && (depth.is_nan() || depth == z) {
                            continue;
                        }
                        if cover.in_a_gap(i, *j, z, if z.is_finite() { cam.pw(z as f64) * JUMP_SLOPE } else { 0.0 }, coarse as usize) {
                            row.push(at as u32);
                        }
                    }
                }
            });
            for at in found.into_iter().flat_map(|(_, row)| row) {
                let (o, d) = cam.ray((at as usize % w) as f64 + 0.5, (at as usize / w) as f64 + 0.5);
                let Some((t0, t1)) = slab(o, d, f.lo, f.hi) else {
                    self.passed[at as usize] = (self.stamp, f32::NAN, true);
                    continue;
                };
                self.counts.jumps += 1;
                rays.push(ray(at, o, d, t0, t1));
            }
        }
        // the coarser rounds: a sample every stride, where the part's box shows (a ray through a
        // sample beside it misses the box)
        let sampled = if self.pixels { 0 } else { h };
        let [i0, i1, j0, j1] = box_samples(&cam, f, grid as f64, w, h, n);
        for j in (j0..j1.min(sampled)).step_by(n) {
            for i in (i0..i1).step_by(n) {
                let at = j * w + i;
                let z = self.cover.at(i, j);
                let (when, depth, by_ray) = self.passed[at];
                // a check that passed on the far side of what is now a jump saw only a surface,
                // maybe one behind a hole: a ray asks again
                let jump = z.is_finite() && self.cover.beyond_a_jump(i, j, z, cam.pw(z as f64) * grid as f64 * JUMP_SLOPE);
                if when == self.stamp && (depth.is_nan() || depth == z) && (by_ray || !jump) {
                    continue;
                }
                let (o, d) = cam.ray((i as f64 + 0.5) * grid as f64, (j as f64 + 0.5) * grid as f64);
                let Some((t0, t1)) = slab(o, d, f.lo, f.hi) else {
                    self.passed[at] = (self.stamp, f32::NAN, true);
                    continue;
                };
                let z = z as f64;
                if z.is_finite() {
                    // beyond a jump in the mesh, it may be the jump that is in the wrong place: a
                    // check on the far surface would pass, so a ray asks what is really first
                    if jump && (grid == 1 || (i + j * 3 + self.roll as usize) % 4 == 0) {
                        self.counts.jumps += 1;
                        rays.push(ray(at as u32, o, d, t0, t1));
                    } else {
                        checks.push(Check { at: at as u32, z: z as f32, o, d, t: cam.along(d, z), t0, t1, v: 0.0 });
                    }
                } else {
                    rays.push(ray(at as u32, o, d, t0, t1));
                }
            }
        }
        // the checks: the field at the mesh's point on each ray
        self.counts.checks += checks.len() as u64;
        par(pool, &mut checks, BATCH, |c| {
            if stale() {
                return;
            }
            let pts: Vec<V> = c.iter().map(|k| add(k.o, mul(k.d, k.t))).collect();
            for (k, v) in c.iter_mut().zip(f.value(&pts)) {
                k.v = v * f.step;
            }
        });
        if stale() {
            return None;
        }
        let mut splits: Vec<(V, f64)> = Vec::new();
        for k in &checks {
            let pw = cam.pw(k.t.max(1e-6));
            if k.v.abs() <= (tol.tau * pw).max(f.cell * 0.005) * f.slope {
                self.passed[k.at as usize] = (self.stamp, k.z, false);
                continue;
            }
            self.counts.strayed += 1;
            splits.push((add(k.o, mul(k.d, k.t)), pw * tol.leaf));
            // off the surface in free space, the ray goes on from there; inside, the surface is
            // somewhere before it
            rays.push(ray(k.at, k.o, k.d, if k.v > 0.0 { k.t } else { k.t0 }, k.t1));
        }
        if !self.probed {
            self.probe(f, pool, &cam, tol, &mut rays, stale)?;
        }
        self.counts.rays += rays.len() as u64;
        par(pool, &mut rays, RAY_BATCH, |c| rays_batch(f, &cam, c, stale));
        if stale() {
            return None;
        }
        let mut ways = Vec::new();
        for r in &rays {
            if !r.hit {
                // a bound proved the ray clear
                if r.sample != NO_SAMPLE {
                    self.passed[r.sample as usize] = (self.stamp, f32::NAN, true);
                }
                continue;
            }
            self.counts.landed += 1;
            let pw = cam.pw(r.t.max(1e-6));
            let p = add(r.o, mul(r.d, r.t));
            let floor = pw * tol.leaf * OPEN_FLOOR;
            // it landed where the mesh shows; or a sample from where an edge of the mesh ends, which
            // need not come as fine as a piece of it that is missing
            let mut limit = pw * tol.leaf;
            let shows_now = if r.sample != NO_SAMPLE { f32::from_bits(self.cover.depth[r.sample as usize].load(Ordering::Relaxed)) } else { f32::NAN };
            if r.sample != NO_SAMPLE {
                let at = r.sample as usize;
                let off = pw * (tol.leaf + tol.tau) * 2.0;
                let landed = cam.depth(p);
                if shows_now.is_finite() && (shows_now as f64 - landed).abs() <= off {
                    self.passed[at] = (self.stamp, shows_now, true);
                    continue;
                }
                // in the last stage the sample is in a gap in what is nearer, so the ray found a piece
                // of a wall the mesh lacks, not an edge a leaf off: down to the leaf, and on to the
                // floor where a cell of the leaf still lacks it (the wall is thinner than the cell)
                if self.pixels {
                    if s.size(s.cells[s.leaf_at(p) as usize].depth) <= limit {
                        limit = floor;
                    }
                } else if self.cover.edge_beside(at % self.cover.w, at / self.cover.w, landed, off, pw * grid as f64 * JUMP_SLOPE) {
                    limit *= EDGE_LEAVES;
                }
            }
            let leaf = s.cells[s.leaf_at(p) as usize];
            let size = s.size(leaf.depth);
            // the mesh had this cell's quads and still left this sample bare: too coarse here, or
            // down to what it can show, and the view need not ask again. The mesh over `p` is the
            // quads of the cells round its cell too: one beside it coarser than the limit leaves
            // the hole as well, its edges striding over a wall thinner than it
            if leaf.kind == SURFACE && leaf.flags & SHOWN != 0 {
                // a cone's ray asks only what stands in front of the mesh: at the mesh's own depth
                // it found nothing the mesh lacks
                let bare = r.sample != NO_SAMPLE || !self.shows(&cam, p, tol, pw);
                if bare && size > limit {
                    splits.push((p, limit));
                } else if !(bare && s.coarse_beside(&leaf, limit, &mut splits)) && r.sample != NO_SAMPLE {
                    self.passed[r.sample as usize] = (self.stamp, shows_now, true);
                }
                continue;
            }
            if leaf.kind == OPEN && size <= floor {
                if !s.coarse_beside(&leaf, limit, &mut splits) && r.sample != NO_SAMPLE {
                    self.passed[r.sample as usize] = (self.stamp, shows_now, true);
                }
                continue;
            }
            ways.push((0, Landing { p, side: stride.max(WHOLE_STRIDE) as f64 * CELL_SPACINGS * pw, floor }));
        }
        lap(&mut self.counts, 1);
        let before = s.changes;
        s.grow(f, pool, ways, &splits, stale)?;
        self.probed = true;
        lap(&mut self.counts, 2);
        // what a stopped round left to show is a change too
        let changed = s.changes != before || s.pending();
        let mut chunks = Vec::new();
        if s.pending() {
            s.settle(f, pool, &coarse_at, &edge_on, stale)?;
            lap(&mut self.counts, 3);
            chunks = s.emit(pool);
            lap(&mut self.counts, 4);
        }
        // done when the finest round of the view found nothing left to change: on a grid coarser
        // than a pixel, then its last stage until it finds nothing either
        let finest = self.pixels || stride == grid;
        if !self.pixels {
            self.stride = if stride > grid { stride / 2 } else { grid };
        }
        if finest && !changed && grid > 1 {
            self.pixels = true;
            self.stamp += 1;
            return Some(Step { chunks, done: false });
        }
        Some(Step { chunks, done: finest && !changed })
    }

    /// Whether the mesh's depth over `p` is about `p`'s own.
    fn shows(&self, cam: &Cam, p: V, tol: &Tolerance, pw: f64) -> bool {
        let Some((x, y, z)) = cam.proj(p) else { return false };
        let (i, j) = ((x / self.cover.step) as isize, (y / self.cover.step) as isize);
        if i < 0 || j < 0 || i as usize >= self.cover.w || j as usize >= self.cover.h {
            return false;
        }
        (self.cover.at(i as usize, j as usize) as f64 - z).abs() <= pw * (tol.leaf + tol.tau) * 2.0
    }

    /// A cone per tile, finer where one stops well in front of the mesh, whose rays land on what
    /// stands there. Moving, a few of the tiles at a time.
    fn probe(&mut self, f: &Field, pool: &TaskPool, cam: &Cam, tol: &Tolerance, rays: &mut Vec<Ray>, stale: Stale) -> Option<()> {
        let size = FIRST_STRIDE;
        let (nx, ny) = ((cam.w / size as f64).ceil() as u32, (cam.h / size as f64).ceil() as u32);
        let share = if tol.leaf > 1.0 { 16 } else { 1 };
        let mut list: Vec<(u32, u32, u32)> = Vec::new();
        for ty in 0..ny {
            for tx in 0..nx {
                if (tx + ty * 3 + self.roll) % share == 0 {
                    list.push((tx * size, ty * size, size));
                }
            }
        }
        let leaf = tol.leaf.ceil() as u32;
        while !list.is_empty() {
            list.retain(|q| self.cover.range(q.0, q.1, q.2).0.is_finite());
            let far = crate::trace::len(sub(mul(add(f.lo, f.hi), 0.5), cam.eye)) + crate::trace::len(sub(f.hi, f.lo)) * 0.5;
            let mut cones: Vec<Cone> = list
                .iter()
                .map(|q| {
                    let (o, d, r0, tan) = cam.cone(q.0 as f64, q.1 as f64, q.2 as f64);
                    let g = [r0 + tan * far; 3];
                    match slab(o, d, sub(f.lo, g), add(f.hi, g)) {
                        Some((a, b)) => Cone { o, d, t: a, t1: b, r0, tan, state: 0, steps: 0 },
                        None => Cone { o, d, t: 0.0, t1: 0.0, r0, tan, state: 2, steps: 0 },
                    }
                })
                .collect();
            self.counts.cones += cones.len() as u64;
            par(pool, &mut cones, BATCH, |c| cones_batch(f, c, stale));
            if stale() {
                return None;
            }
            let mut next = Vec::new();
            for (q, c) in list.iter().zip(&cones) {
                if c.state != 1 {
                    continue;
                }
                let (near, far) = self.cover.range(q.0, q.1, q.2);
                let r = c.r0 + c.tan * c.t;
                let margin = (far - near) + 2.0 * r / f.slope + f.cell * 0.05;
                if cam.along(c.d, near) - c.t <= margin {
                    continue;
                }
                self.counts.front += 1;
                if q.2 <= leaf {
                    let t1 = slab(c.o, c.d, f.lo, f.hi).map_or(-1.0, |(_, b)| b);
                    rays.push(ray(NO_SAMPLE, c.o, c.d, c.t, t1));
                } else {
                    let s = q.2 / 2;
                    next.extend([(q.0, q.1, s), (q.0 + s, q.1, s), (q.0, q.1 + s, s), (q.0 + s, q.1 + s, s)]);
                }
            }
            list = next;
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::{cross, len, oct_unpack, raw, CUT_SLOT, MOVING, STILL};
    use makepad_csg_sdf::{SdfKernel, SdfNode, SdfOp, SdfSplashExpr, SplashPoint};
    use makepad_script::*;
    use std::collections::HashMap;

    const W: f64 = 1600.0;
    const H: f64 = 1000.0;

    fn never() -> bool {
        false
    }

    /// Until the view is done, as the worker would. How many rounds.
    fn look(s: &mut Surface, g: &mut Gaze, f: &Field, pool: &TaskPool, cam: Cam, tol: &Tolerance) -> u32 {
        let mut rounds = 0;
        loop {
            rounds += 1;
            if g.step(s, f, pool, cam, tol, &never).unwrap().done {
                return rounds;
            }
            assert!(rounds < 200, "a view that does not settle");
        }
    }

    fn quads(s: &Surface) -> Vec<[f32; TILE_SLOTS]> {
        s.shown.iter().flatten().flat_map(|d| d.segments().iter().flat_map(|g| g.data().chunks_exact(TILE_SLOTS).map(|q| std::array::from_fn(|i| q[i])).collect::<Vec<_>>()).collect::<Vec<_>>()).collect()
    }

    fn evals(f: &Field) -> u64 {
        f.evals.load(Ordering::Relaxed)
    }

    fn transform(at: V, rotate: V, of: SdfNode) -> SdfNode {
        SdfNode::Transform { at, rotate, scale: 1.0, of: Box::new(of) }
    }

    fn union(of: Vec<SdfNode>) -> SdfNode {
        SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of }
    }

    fn bracket() -> SdfNode {
        let hole = |x: f64| transform([x, 0.0, 0.0], [0.0; 3], SdfNode::Cylinder { radius: 4.0, top_radius: 4.0, height: 40.0 });
        SdfNode::Combine { op: SdfOp::Difference, blend: 0.0, of: vec![SdfNode::Box { size: [40.0, 8.0, 30.0], radius: 0.0 }, hole(-10.0), hole(10.0)] }
    }

    fn ball_behind_a_wall() -> SdfNode {
        union(vec![SdfNode::Box { size: [60.0, 4.0, 40.0], radius: 0.0 }, transform([0.0, 25.0, 0.0], [0.0; 3], SdfNode::Sphere { radius: 8.0 })])
    }

    /// A wall and a wire far thinner than the first rays are apart.
    fn thin_things(wall: f64, wire: f64) -> SdfNode {
        union(vec![SdfNode::Box { size: [50.0, wall, 40.0], radius: 0.0 }, transform([0.0, -15.0, 0.0], [0.0; 3], SdfNode::Cylinder { radius: wire, top_radius: wire, height: 50.0 })])
    }

    /// Where a ray from `o` along `d` truly lands, marching the field's own bound.
    fn truth(root: &SdfNode, o: V, d: V, pw: impl Fn(f64) -> f64) -> Option<f64> {
        let mut t = 0.0;
        for _ in 0..400 {
            let v = raw(root, &[add(o, mul(d, t))])[0];
            if v < pw(t.max(1e-3)) * 0.05 {
                return Some(t);
            }
            t += v;
            if t > 500.0 {
                return None;
            }
        }
        Some(t)
    }

    /// What a ray hits, the mesh shows, at about the depth it hit.
    #[test]
    fn what_a_ray_sees_the_mesh_shows() {
        let pool = TaskPool::closed();
        for (name, root, must) in [("bracket", bracket(), 0.99), ("wall 1.0, wire 0.3", thin_things(1.0, 0.3), 0.98), ("wall 0.2, wire 0.1", thin_things(0.2, 0.1), 0.95), ("wall 0.05, wire 0.03", thin_things(0.05, 0.03), 0.0)] {
            let f = Field::new(&root, false, &pool).unwrap();
            let mut s = Surface::new(&f);
            let mut g = Gaze::default();
            let cam = Cam::looking([30.0, -90.0, 25.0], [0.0; 3], W, H);
            for tol in [MOVING, STILL] {
                let e0 = evals(&f);
                let rounds = look(&mut s, &mut g, &f, &pool, cam, &tol);
                let spent = evals(&f) - e0;
                g.cover.fill(&cam, 1.0, &s, &pool);
                // on the grid of the truth's rays: a pixel beside one that misses is a silhouette's,
                // where a part of a pixel either way is not a missing piece
                let (nx, ny) = (W as usize / 3, H as usize / 3);
                let hit: Vec<Option<f64>> = (0..nx * ny)
                    .map(|i| {
                        let (o, d) = cam.ray((i % nx) as f64 * 3.0 + 0.5, (i / nx) as f64 * 3.0 + 0.5);
                        truth(&root, o, d, |t| cam.pw(t))
                    })
                    .collect();
                let (mut hits, mut shown, mut near, mut pinholes) = (0u32, 0u32, 0u32, 0u32);
                for y in 0..ny {
                    for x in 0..nx {
                        let Some(t) = hit[y * nx + x] else { continue };
                        let (_, d) = cam.ray(x as f64 * 3.0 + 0.5, y as f64 * 3.0 + 0.5);
                        let z = g.cover.at(x * 3, y * 3) as f64;
                        let beside_a_miss = (y.saturating_sub(1)..(y + 2).min(ny)).any(|j| (x.saturating_sub(1)..(x + 2).min(nx)).any(|i| hit[j * nx + i].is_none()));
                        if beside_a_miss {
                            continue;
                        }
                        hits += 1;
                        if z.is_finite() {
                            shown += 1;
                            near += ((cam.along(d, z) - t).abs() <= cam.pw(t) * 4.0) as u32;
                        } else {
                            pinholes += 1;
                        }
                    }
                }
                let share = shown as f64 / hits.max(1) as f64;
                eprintln!("{name}, leaf {}: {hits} rays hit, {:.1}% shown ({pinholes} bare), {:.1}% at its depth; {} quads, {spent} evals, {rounds} rounds", tol.leaf, share * 100.0, near as f64 * 100.0 / hits.max(1) as f64, s.held().quads);
                if tol.leaf == STILL.leaf {
                    assert!(share >= must, "{name}: only {:.1}% of what rays hit is shown", share * 100.0);
                }
            }
        }
    }

    /// A hollow part, plain and stretched along its axes (a part scaled by axis), shows its thin
    /// outer wall wherever a ray meets it: no ray through the wall to the dark inside, near and far,
    /// and at the wall's edges over the inside the mesh's edge within a pixel.
    #[test]
    fn a_stretched_hollow_shows_its_outer_wall_where_rays_meet_it() {
        let pool = TaskPool::closed();
        // a bud vase as its script builds it: a belly and a neck blended, a flat foot cut,
        // hollowed to its wall, a lip on the rim, open at the top
        let body = SdfNode::Combine {
            op: SdfOp::Union,
            blend: 9.0,
            of: vec![
                transform([0.0, 23.8, 0.0], [0.0; 3], SdfNode::Ellipsoid { radii: [21.0, 25.2, 21.0] }),
                transform([0.0, 51.8, 0.0], [0.0; 3], SdfNode::Cylinder { radius: 9.5, top_radius: 9.5, height: 29.4 }),
            ],
        };
        let footed = SdfNode::Combine { op: SdfOp::Difference, blend: 0.0, of: vec![body, transform([0.0, -7.0, 0.0], [0.0; 3], SdfNode::Box { size: [84.0, 20.0, 84.0], radius: 0.0 })] };
        let hollow = SdfNode::Shell { thickness: 0.8, of: Box::new(SdfNode::Round { radius: -0.8, of: Box::new(footed) }) };
        let rimmed = SdfNode::Combine { op: SdfOp::Union, blend: 1.5, of: vec![hollow, transform([0.0, 65.8, 0.0], [0.0; 3], SdfNode::Torus { radius: 10.7, tube: 1.8 })] };
        let mouth = transform([0.0, 74.65, 0.0], [0.0; 3], SdfNode::Cylinder { radius: 7.9, top_radius: 7.9, height: 30.7 });
        let vase = transform([0.0, -35.0, 0.0], [0.0; 3], SdfNode::Combine { op: SdfOp::Difference, blend: 0.0, of: vec![rimmed, mouth] });
        for far in [1.0, 1.6] {
            // from above and in front, as a plate is seen
            let cam = Cam::looking([80.0 * far, 75.0 * far, 160.0 * far], [0.0; 3], W, H);
            for k in [[1.0, 1.0, 1.0], [2.0, 1.0, 1.0], [1.0, 1.7, 1.3]] {
                let stretch = makepad_csg_sdf::SdfDeform::Stretch { scale: k };
                let lipschitz = stretch.lipschitz(None);
                let root = if k == [1.0; 3] { vase.clone() } else { SdfNode::Deform { deform: stretch, lipschitz, of: Box::new(vase.clone()) } };
                let f = Field::new(&root, false, &pool).unwrap();
                let (mut s, mut g) = (Surface::new(&f), Gaze::default());
                for tol in [MOVING, STILL] {
                    look(&mut s, &mut g, &f, &pool, cam, &tol);
                }
                g.cover.fill(&cam, 1.0, &s, &pool);
                // on the grid of the truth's rays, a ray beside one that misses left out (a
                // silhouette's)
                let (nx, ny) = (W as usize / 3, H as usize / 3);
                let hit: Vec<Option<f64>> = (0..nx * ny)
                    .map(|i| {
                        let (o, d) = cam.ray((i % nx) as f64 * 3.0 + 0.5, (i / nx) as f64 * 3.0 + 0.5);
                        truth(&root, o, d, |t| cam.pw(t))
                    })
                    .collect();
                let (mut hits, mut edges, mut through) = (0u32, 0u32, 0u32);
                for y in 1..ny - 1 {
                    for x in 1..nx - 1 {
                        let Some(t) = hit[y * nx + x] else { continue };
                        if (y - 1..y + 2).any(|j| (x - 1..x + 2).any(|i| hit[j * nx + i].is_none())) {
                            continue;
                        }
                        hits += 1;
                        let (_, d) = cam.ray(x as f64 * 3.0 + 0.5, y as f64 * 3.0 + 0.5);
                        let z = g.cover.at(x * 3, y * 3) as f64;
                        if z.is_finite() && cam.along(d, z) - t <= cam.pw(t) * 4.0 {
                            continue;
                        }
                        // within a pixel of an edge of the wall, over what is behind it, the
                        // mesh's edge is as near as that: what it shows, a ray a pixel over meets
                        let spread = (0..9)
                            .filter(|k| *k != 4)
                            .map(|k| {
                                let (o, d) = cam.ray((x * 3 + k % 3) as f64 - 0.5, (y * 3 + k / 3) as f64 - 0.5);
                                truth(&root, o, d, |t| cam.pw(t)).map_or(f64::INFINITY, |n| (n - t).abs())
                            })
                            .fold(0.0f64, f64::max);
                        if z.is_finite() && cam.along(d, z) - t <= cam.pw(t) * 4.0 + spread {
                            edges += 1;
                        } else {
                            through += 1;
                        }
                    }
                }
                eprintln!("from {far}x, stretched {k:?}: {hits} rays meet it, {through} see past its wall ({edges} at its edges); {} quads", s.held().quads);
                assert!(hits > 3_000, "from {far}x, stretched {k:?}: {hits} rays");
                assert_eq!(through, 0, "from {far}x, stretched {k:?}: rays see past its wall");
            }
        }
    }

    /// Rays decide what is hidden: through a hole, what is really behind it; holes as wide as the
    /// first rays' spacing and far narrower.
    #[test]
    fn rays_reach_what_is_behind_a_hole() {
        let pool = TaskPool::closed();
        let eye = [0.0, -90.0, 0.0];
        let cam = Cam::looking(eye, [0.0; 3], W, H);
        let pw = cam.pw(90.0);
        for (width_px, must_moving, must_still) in [(60.0, 0.9, 0.98), (10.0, 0.0, 0.9), (4.0, 0.0, 0.6)] {
            let r = width_px * pw * 0.5;
            let hole = SdfNode::Cylinder { radius: r, top_radius: r, height: 20.0 };
            let wall = SdfNode::Combine { op: SdfOp::Difference, blend: 0.0, of: vec![SdfNode::Box { size: [60.0, 2.0, 40.0], radius: 0.0 }, hole] };
            let root = union(vec![wall, transform([0.0, 30.0, 0.0], [0.0; 3], SdfNode::Sphere { radius: 15.0 })]);
            let f = Field::new(&root, false, &pool).unwrap();
            let mut s = Surface::new(&f);
            let mut g = Gaze::default();
            for (tol, must) in [(MOVING, must_moving), (STILL, must_still)] {
                look(&mut s, &mut g, &f, &pool, cam, &tol);
                g.cover.fill(&cam, 1.0, &s, &pool);
                let (mut through, mut shown) = (0u32, 0u32);
                for y in 0..H as usize {
                    for x in 0..W as usize {
                        let (o, d) = cam.ray(x as f64 + 0.5, y as f64 + 0.5);
                        if dot(sub(add(o, mul(d, 90.0)), eye), [1.0, 0.0, 0.0]).abs() > r * 1.2 {
                            continue;
                        }
                        let Some(t) = truth(&root, o, d, |t| cam.pw(t)) else { continue };
                        if t < 92.0 {
                            continue;
                        }
                        through += 1;
                        let z = g.cover.at(x, y) as f64;
                        shown += (z.is_finite() && (cam.along(d, z) - t).abs() <= cam.pw(t) * 4.0) as u32;
                    }
                }
                let share = shown as f64 / through.max(1) as f64;
                eprintln!("hole {width_px} px, leaf {}: {through} pixels see through, {:.1}% show what is behind; {} quads", tol.leaf, share * 100.0, s.held().quads);
                assert!(through > 0);
                assert!(share >= must, "a {width_px} px hole, leaf {}: {:.1}%", tol.leaf, share * 100.0);
            }
        }
    }

    /// A plane shaving a ball leaves a small face with the ball's wall rising from it slowly: wall
    /// within half a pixel of the plane all around the face, which must still read as wall. The rim
    /// is where the values cross: within three pixels here, on a rim 44 pixels across under cells
    /// that a flat surface lets grow.
    #[test]
    fn a_wall_grazing_a_cut_is_no_cut_face() {
        let pool = TaskPool::closed();
        let keep_from = -19.99;
        let body = transform([0.0, keep_from + 50.0, 0.0], [0.0; 3], SdfNode::Box { size: [100.0; 3], radius: 0.0 });
        let cut = SdfNode::Material { slot: CUT_SLOT, within: None, of: Box::new(body) };
        let root = SdfNode::Combine { op: SdfOp::Intersection, blend: 0.0, of: vec![SdfNode::Sphere { radius: 20.0 }, cut] };
        let f = Field::new(&root, false, &pool).unwrap();
        let cam = Cam::looking([0.0, -60.0, 0.0], [0.0; 3], W, H);
        let (mut s, mut g) = (Surface::new(&f), Gaze::default());
        look(&mut s, &mut g, &f, &pool, cam, &STILL);
        let rim = (20.0f64 * 20.0 - keep_from * keep_from).sqrt();
        let half_pixel = 0.5 * cam.pw(40.0);
        let (mut faces, mut walls_near, mut crossings, mut worst) = (0, 0, 0, 0.0f64);
        for q in quads(&s) {
            let at: [[f64; 3]; 4] = std::array::from_fn(|k| std::array::from_fn(|i| q[k * 4 + i] as f64));
            let value = |k: usize| {
                let w = q[21 + k / 2] as u32;
                (if k % 2 == 0 { w / 4096 } else { w % 4096 }) as f64 - 2047.5
            };
            for k in 0..4 {
                let above = at[k][1] - keep_from;
                if value(k) > 0.0 {
                    faces += 1;
                } else {
                    walls_near += (above.abs() < half_pixel) as u32;
                }
                // where an edge's ends differ the rim crosses it: there, on the true rim
                let (b, vb) = (at[(k + 1) % 4], value((k + 1) % 4));
                if (value(k) > 0.0) != (vb > 0.0) {
                    let t = value(k) / (value(k) - vb);
                    let x: [f64; 3] = std::array::from_fn(|i| at[k][i] + (b[i] - at[k][i]) * t);
                    crossings += 1;
                    worst = worst.max((x[0].hypot(x[2]) - rim).abs());
                }
            }
        }
        eprintln!("{faces} face corners, {walls_near} wall corners within half a pixel ({half_pixel} mm) of the plane, {crossings} crossings, off the rim at {rim} mm by at most {worst} mm");
        assert!(faces > 0 && walls_near > 0 && crossings > 8, "{faces} faces, {walls_near} walls near, {crossings} crossings");
        assert!(worst < 6.0 * half_pixel, "the rim is {worst} mm off, half a pixel is {half_pixel}");
    }

    /// A section opens a hollow ellipsoid to the view: rays cross its middle, about its centre,
    /// where its estimate is steepest, and all that they find there is drawn.
    #[test]
    fn a_section_shows_the_inside_of_a_hollow_ellipsoid() {
        let pool = TaskPool::closed();
        let body = transform([0.0, 50.0, 0.0], [0.0; 3], SdfNode::Box { size: [100.0; 3], radius: 0.0 });
        let cut = SdfNode::Material { slot: CUT_SLOT, within: None, of: Box::new(body) };
        let hollow = SdfNode::Shell { thickness: 0.8, of: Box::new(SdfNode::Ellipsoid { radii: [21.0, 18.0, 25.0] }) };
        let root = SdfNode::Combine { op: SdfOp::Intersection, blend: 0.0, of: vec![hollow, cut] };
        let f = Field::new(&root, false, &pool).unwrap();
        let cam = Cam::looking([0.0, -70.0, 0.0], [0.0; 3], W, H);
        let (mut s, mut g) = (Surface::new(&f), Gaze::default());
        look(&mut s, &mut g, &f, &pool, cam, &MOVING);
        let mut cover = Cover::default();
        cover.fill(&cam, 1.0, &s, &pool);
        let (mut inside, mut bare, mut square, mut biggest) = (0u32, 0u32, vec![0u32; cover.w * cover.h], 0);
        for y in 1..cover.h {
            for x in 1..cover.w {
                let (o, d) = cam.ray(x as f64 + 0.5, y as f64 + 0.5);
                let on = add(o, mul(d, -o[1] / d[1]));
                // in the opening, well in from its rim
                if (on[0] / 20.0).hypot(on[2] / 24.0) < 0.9 {
                    inside += 1;
                    if !cover.at(x, y).is_finite() {
                        bare += 1;
                        let i = y * cover.w + x;
                        square[i] = square[i - 1].min(square[i - cover.w]).min(square[i - cover.w - 1]) + 1;
                        biggest = biggest.max(square[i]);
                    }
                }
            }
        }
        eprintln!("{inside} pixels in the opening, {bare} bare, the biggest {biggest} across");
        assert!(inside > 10_000 && bare * 100 < inside && biggest <= 4, "{inside} pixels, {bare} bare, the biggest {biggest} across");
    }

    /// A cell's vertex is the field's and the cell's, whichever view reached it first.
    #[test]
    fn no_view_re_rolls_a_cell() {
        let pool = TaskPool::closed();
        let root = bracket();
        let f = Field::new(&root, false, &pool).unwrap();
        let (a, b) = (Cam::looking([35.0, -90.0, 40.0], [0.0; 3], W, H), Cam::looking([-70.0, -40.0, 60.0], [0.0; 3], W, H));
        let run = |views: [Cam; 2]| {
            let (mut s, mut g) = (Surface::new(&f), Gaze::default());
            for cam in views {
                look(&mut s, &mut g, &f, &pool, cam, &MOVING);
            }
            s
        };
        let (ab, ba) = (run([a, b]), run([b, a]));
        let of = |s: &Surface| -> HashMap<(u32, u32, u32, u8), [u32; 3]> { s.cells.iter().filter(|c| c.flags & PLACED != 0).map(|c| ((c.x, c.y, c.z, c.depth), c.p.map(f32::to_bits))).collect() };
        let (x, y) = (of(&ab), of(&ba));
        let shared = x.iter().filter(|(k, _)| y.contains_key(*k)).count();
        let same = x.iter().filter(|(k, v)| y.get(*k) == Some(*v)).count();
        eprintln!("{} and {} placed cells, {shared} in both, {same} equal", x.len(), y.len());
        assert!(shared > 100);
        assert_eq!(same, shared);
    }

    /// A cover filled again for the chunks shown since is the cover filled whole, round by round
    /// and view by view, on grids that do and don't divide the view.
    #[test]
    fn a_cover_drawn_again_where_it_changed_is_the_whole_cover() {
        let pool = TaskPool::closed();
        let f = Field::new(&bracket(), false, &pool).unwrap();
        let (mut s, mut g) = (Surface::new(&f), Gaze::default());
        let mut again = [Cover::default(), Cover::default()];
        let (mut fills, mut part) = (0u32, 0u32);
        for cam in [Cam::looking([35.0, -90.0, 40.0], [0.0; 3], W, H), Cam::looking([10.0, -30.0, 12.0], [15.0, 0.0, 0.0], W, H)] {
            for tol in [MOVING, STILL] {
                loop {
                    let done = g.step(&mut s, &f, &pool, cam, &tol, &never).unwrap().done;
                    for (cover, step) in again.iter_mut().zip([1.0, 3.0]) {
                        cover.fill(&cam, step, &s, &pool);
                        let mut whole = Cover::default();
                        whole.fill(&cam, step, &s, &pool);
                        fills += 1;
                        part += (cover.again.contains(&true) && cover.again.contains(&false)) as u32;
                        assert!(cover.depth.iter().zip(&whole.depth).all(|(a, b)| a.load(Ordering::Relaxed) == b.load(Ordering::Relaxed)), "fill {fills}: drawn again, it is not the whole cover");
                    }
                    if done {
                        break;
                    }
                }
            }
        }
        eprintln!("{part} of {fills} fills drew part of the grid again");
        assert!(part * 4 > fills, "only {part} of {fills} fills drew part of the grid again");
    }

    /// Detail stays: a view seen before costs little more than its checks, and takes nothing away.
    #[test]
    fn a_return_view_builds_nothing() {
        let pool = TaskPool::closed();
        let f = Field::new(&bracket(), false, &pool).unwrap();
        let (mut s, mut g) = (Surface::new(&f), Gaze::default());
        let (a, b) = (Cam::looking([35.0, -90.0, 40.0], [0.0; 3], W, H), Cam::looking([-70.0, -40.0, 60.0], [0.0; 3], W, H));
        let e0 = evals(&f);
        look(&mut s, &mut g, &f, &pool, a, &MOVING);
        let (e1, first) = (evals(&f), s.held().quads);
        look(&mut s, &mut g, &f, &pool, b, &MOVING);
        let (e2, cells) = (evals(&f), s.cells.len());
        look(&mut s, &mut g, &f, &pool, a, &MOVING);
        eprintln!("first view {} evals, {first} quads; second {}; return {} ({} checks, {} strayed, {} rays, {} cones) cells {} -> {}", e1 - e0, e2 - e1, evals(&f) - e2, g.counts.checks, g.counts.strayed, g.counts.rays, g.counts.cones, cells, s.cells.len());
        assert!(s.held().quads >= first);
        assert!(evals(&f) - e2 < g.counts.checks * 2, "the return view cost {} evals for {} checks", evals(&f) - e2, g.counts.checks);
        // a cone looks at other tiles each time, and may find a little: a split's worth each
        assert!(s.cells.len() < cells + g.counts.cones as usize * 8, "the return view built {} cells for {} cones", s.cells.len() - cells, g.counts.cones);
    }

    /// Nothing exists where no ray landed: the ball behind the wall is never built.
    #[test]
    fn what_no_ray_reached_does_not_exist() {
        let pool = TaskPool::closed();
        let root = ball_behind_a_wall();
        let f = Field::new(&root, false, &pool).unwrap();
        let (mut s, mut g) = (Surface::new(&f), Gaze::default());
        let cam = Cam::looking([0.0, -90.0, 5.0], [0.0; 3], W, H);
        look(&mut s, &mut g, &f, &pool, cam, &STILL);
        let behind = quads(&s).iter().filter(|q| q[17] > 6.0).count();
        eprintln!("{} quads, {behind} behind the wall", s.held().quads);
        assert!(s.held().quads > 0);
        assert_eq!(behind, 0);
    }

    /// An eye right by a sharp edge: a cell on the edge strays from its plane however small it is,
    /// and what has to be flat there is far smaller than the finest cell. It stops at the finest,
    /// as a landing does.
    #[test]
    fn an_edge_under_the_eye_stops_at_the_finest_cell() {
        let pool = TaskPool::closed();
        let root = SdfNode::Box { size: [300.0; 3], radius: 0.0 };
        let f = Field::new(&root, false, &pool).unwrap();
        let (mut s, mut g) = (Surface::new(&f), Gaze::default());
        look(&mut s, &mut g, &f, &pool, Cam::looking([150.005, -150.005, 0.0], [0.0; 3], 800.0, 500.0), &STILL);
        let deepest = s.cells.iter().map(|c| c.depth as u32).max().unwrap();
        assert!(s.held().quads > 0);
        assert!(deepest < BITS, "a cell at depth {deepest}");
    }

    /// A look stopped at any point goes on to the mesh of one left alone: a scan is stopped by any
    /// input, and whatever it was doing is owed again.
    #[test]
    fn a_look_stopped_anywhere_finishes_like_one_left_alone() {
        let pool = TaskPool::closed();
        let root = bracket();
        let f = Field::new(&root, false, &pool).unwrap();
        let cam = Cam::looking([35.0, -90.0, 40.0], [0.0; 3], W, H);
        let (mut alone, mut g) = (Surface::new(&f), Gaze::default());
        look(&mut alone, &mut g, &f, &pool, cam, &MOVING);
        let (mut s, mut g) = (Surface::new(&f), Gaze::default());
        let (mut asks, mut stops) = (1u32, 0u32);
        loop {
            let left = std::sync::atomic::AtomicU32::new(asks);
            let stale = || left.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1)).is_err();
            match g.step(&mut s, &f, &pool, cam, &MOVING, &stale) {
                None => {
                    stops += 1;
                    asks += 1 + asks / 8;
                }
                Some(step) if step.done => break,
                Some(_) => asks = 1,
            }
        }
        assert!(!s.pending());
        assert!(s.values.values().all(|v| !v.is_nan()), "a value owed reads as known");
        assert!(s.cells.iter().all(|c| c.kids != 0 || c.kind != SURFACE || c.flags & SHOWN == 0 || c.flags & PLACED != 0), "a shown cell has no vertex");
        let (a, b) = (against_rays(&root, &alone, &cam, &pool), against_rays(&root, &s, &cam, &pool));
        let (qa, qb) = (quads(&alone).len(), quads(&s).len());
        eprintln!("{stops} stops; left alone {a:?} {qa} quads, stopped {b:?} {qb} quads");
        assert!(stops > 20);
        assert!(qb.abs_diff(qa) * 50 < qa, "{qb} quads against {qa}");
        assert_eq!(a.0, b.0, "rays that find the same");
    }

    /// A chunk a view leaves out has no corner of any quad in that view, and a close view does leave
    /// some out.
    #[test]
    fn a_chunk_left_out_has_nothing_in_the_view() {
        let pool = TaskPool::closed();
        let root = bracket();
        let f = Field::new(&root, false, &pool).unwrap();
        let (mut s, mut g) = (Surface::new(&f), Gaze::default());
        look(&mut s, &mut g, &f, &pool, Cam::looking([35.0, -90.0, 40.0], [0.0; 3], W, H), &MOVING);
        let close = Cam::looking([10.0, -30.0, 12.0], [15.0, 0.0, 0.0], W, H);
        let mut left_out = 0;
        for c in s.shown.iter().flatten() {
            let corners: Vec<&[f32]> = c.segments().iter().flat_map(|g| g.data().chunks_exact(TILE_SLOTS)).flat_map(|q| q[..16].chunks_exact(4)).collect();
            assert!(corners.iter().all(|p| (0..3).all(|i| c.quads.lo[i] <= p[i] && p[i] <= c.quads.hi[i])), "a corner outside its chunk's box");
            if !close.sees_box(&c.quads) {
                left_out += 1;
                let shows = |p: &&[f32]| close.proj([p[0] as f64, p[1] as f64, p[2] as f64]).is_some_and(|(x, y, _)| x >= 0.0 && y >= 0.0 && x < close.w && y < close.h);
                assert!(!corners.iter().any(|p| shows(p)), "a chunk left out shows in the view");
            }
        }
        assert!(left_out > 0);
    }

    /// A face toward the view stays on the surface, across jumps in depth and between levels.
    #[test]
    fn triangles_keep_to_the_surface() {
        let pool = TaskPool::closed();
        let cam = Cam::looking([35.0, -90.0, 40.0], [0.0; 3], W, H);
        for (name, root) in [("bracket", bracket()), ("ball", ball_behind_a_wall())] {
            let f = Field::new(&root, false, &pool).unwrap();
            let (mut s, mut g) = (Surface::new(&f), Gaze::default());
            for (tol, limit) in [(MOVING, 4.0), (STILL, 2.0)] {
                look(&mut s, &mut g, &f, &pool, cam, &tol);
                g.cover.fill(&cam, 1.0, &s, &pool);
                let mut worst = 0.0f64;
                for q in quads(&s) {
                    let at = |k: usize| -> V { [q[k * 4] as f64, q[k * 4 + 1] as f64, q[k * 4 + 2] as f64] };
                    for k in 0..4 {
                        let (a, b) = (at(k), at((k + 1) % 4));
                        let n = cross(sub(at(2), at(0)), sub(at(3), at(1)));
                        if dot(n, cam.dir_to(a)) >= 0.0 || len(n) < 1e-12 {
                            continue;
                        }
                        // only what shows: what is hidden from this view no check has looked at
                        let mid = mul(add(a, b), 0.5);
                        let Some((x, y, z)) = cam.proj(mid) else { continue };
                        if x < 0.0 || y < 0.0 || x >= W || y >= H || (g.cover.at(x as usize, y as usize) as f64 - z).abs() > cam.pw(z.max(1e-3)) * 2.0 {
                            continue;
                        }
                        let v = raw(&root, &[mid])[0].abs();
                        worst = worst.max(v / cam.pw(z.max(1e-3)));
                    }
                }
                eprintln!("{name}, leaf {}: worst edge middle {worst:.2} px off, {} quads", tol.leaf, s.held().quads);
                assert!(worst <= limit, "{name}: an edge strays {worst:.2} px from the surface");
            }
        }
    }







    /// A ray walks only as far as a point's value proves: across a hollow part a longer stride goes
    /// through a thin wall and out the other side, and the ray reads as clear for good.
    #[test]
    fn a_ray_does_not_stride_through_a_thin_wall() {
        let pool = TaskPool::closed();
        let at = |at: [f64; 3], of: SdfNode| SdfNode::Transform { at, rotate: [0.0; 3], scale: 1.0, of: Box::new(of) };
        let body = SdfNode::Combine { op: SdfOp::Union, blend: 9.0, of: vec![at([0.0, 68.0, 0.0], SdfNode::Ellipsoid { radii: [60.0, 72.0, 60.0] }), at([0.0, 148.0, 0.0], SdfNode::Cylinder { radius: 30.0, top_radius: 30.0, height: 84.0 })] };
        let wall = SdfNode::Shell { thickness: 0.4, of: Box::new(SdfNode::Round { radius: -0.4, of: Box::new(body) }) };
        let root = SdfNode::Combine { op: SdfOp::Difference, blend: 0.0, of: vec![wall, at([0.0, 196.6, 0.0], SdfNode::Cylinder { radius: 29.2, top_radius: 29.2, height: 46.8 })] };
        let f = Field::new(&root, false, &pool).unwrap();
        assert_rays_stop_at_the_wall(&root, &f, 3, 5000);
    }

    /// Rays from all round a thin-walled part: every one that truly goes into the wall stops there,
    /// not walking through it.
    fn assert_rays_stop_at_the_wall(root: &SdfNode, f: &Field, every: usize, at_least: u32) {
        let mid = mul(add(f.lo, f.hi), 0.5);
        for (az, el) in [(30.0f64, 25.0f64), (120.0, 25.0)] {
            let (a, e) = (az.to_radians(), el.to_radians());
            let cam = Cam::looking(add(mid, mul([a.sin() * e.cos(), e.sin(), a.cos() * e.cos()], 430.0)), mid, W, H);
            let mut rays: Vec<Ray> = Vec::new();
            for j in (0..H as usize).step_by(every) {
                for i in (0..W as usize).step_by(every) {
                    let (o, d) = cam.ray(i as f64 + 0.5, j as f64 + 0.5);
                    if let Some((t0, t1)) = slab(o, d, f.lo, f.hi) {
                        rays.push(ray(NO_SAMPLE, o, d, t0, t1));
                    }
                }
            }
            rays_batch(f, &cam, &mut rays, &|| false);
            let (mut hits, mut through) = (0u32, 0u32);
            for r in &rays {
                // a ray that truly goes in, not one grazing past
                let Some(t) = truth(root, r.o, r.d, |t| cam.pw(t)) else { continue };
                if raw(root, &[add(r.o, mul(r.d, t + 0.2))])[0] < 0.0 {
                    hits += 1;
                    through += !r.hit as u32;
                }
            }
            eprintln!("az {az}: {hits} rays go into the wall, {through} of them walked through it");
            assert!(hits > at_least);
            assert_eq!(through, 0, "az {az}: {through} of {hits} rays walked through the wall");
        }
    }

    /// A document's thin-walled vase: its program's proof gives the step, and rays walked by that step
    /// stop at the wall. The same document without a trace proves nothing.
    #[test]
    fn a_documents_thin_wall_walks_on_its_proof() {
        let host = Box::leak(Box::new(ScriptVmHost::new(0i32, ())));
        let mut vm = ScriptVm { host: host.host_context(), bx: Box::new(ScriptVmBase::new()) };
        vm.capture_errors();
        let doc = "use mod.math.*
let field = fn(p) {
    // an ellipsoid-like body (a sphere in a stretched frame) with a neck, as a wall, the neck cut open
    let body = sdf_sphere(vec3(p.x * 1.2, p.y - 68.0, p.z * 1.2), 72.0)
    let neck = sdf_round_box(p - vec3(0.0, 148.0, 0.0), vec3(60.0, 84.0, 60.0), 8.0)
    let wall = sdf_shell(smooth_union(body, neck, 9.0), 0.4)
    return sdf_difference(wall, sdf_box(p - vec3(0.0, 196.6, 0.0), vec3(50.0, 46.8, 50.0)))
}
(field)";
        let doc = vm.eval(ScriptMod::new("", "", "thin wall", 0, 0, doc, &[]));
        assert!(vm.take_errors().is_empty());
        let doc = doc.as_object().expect("a fn");
        let _keep = vm.new_object_ref(doc);
        let bounds = Some(([-100.0, -20.0, -100.0], [100.0, 210.0, 100.0]));
        let part = |field| SdfNode::Kernel(SdfKernel::splash(field, bounds));
        let pool = TaskPool::closed();
        let root = part(SdfSplashExpr::compile_traced(&mut vm, doc, SplashPoint::Vec3, &[]).unwrap());
        let f = Field::new(&root, false, &pool).unwrap();
        assert!(f.proven > 0.0, "the proof gives the step");
        // the stretch of the body's frame, and a blend is no steeper than its steepest
        let slope = 1.0 / f.step;
        assert!((1.2..1.21).contains(&slope), "slope {slope}");
        assert_rays_stop_at_the_wall(&root, &f, 6, 1000);
        let unproven = part(SdfSplashExpr::compile(&mut vm, doc, SplashPoint::Vec3, &[]).unwrap());
        assert_eq!(Field::new(&unproven, false, &pool).unwrap().proven, 0.0, "no trace, no proof");
    }


    /// A side no ray has seen, round by round: coarse cells that hold more than one piece of surface
    /// split at once, so even the first round is no patchwork of long wrong triangles.
    #[test]
    fn a_new_side_is_about_right_from_the_start() {
        let pool = TaskPool::closed();
        let root = SdfNode::Combine { op: SdfOp::Intersection, blend: 0.0, of: vec![SdfNode::Gyroid { period: 13.0, wall: 1.2 }, SdfNode::Box { size: [34.0, 34.0, 34.0], radius: 0.0 }] };
        let f = Field::new(&root, false, &pool).unwrap();
        // from far off first: what it built has to hold from up close too
        let (first, side) = (Cam::looking([200.0, -320.0, 180.0], [0.0; 3], W, H), Cam::looking([-70.0, 60.0, -30.0], [0.0; 3], W, H));
        let (mut s, mut g) = (Surface::new(&f), Gaze::default());
        look(&mut s, &mut g, &f, &pool, first, &MOVING);
        let mut first = None;
        let wrong = |s: &Surface| {
            let (hits, bare, air, off) = against_rays(&root, s, &side, &pool);
            (bare + air + off) as f64 / hits as f64
        };
        let settled = loop {
            let step = g.step(&mut s, &f, &pool, side, &MOVING, &|| false).unwrap();
            if step.done {
                break wrong(&s);
            }
            if first.is_none() {
                first = Some(wrong(&s));
            }
        };
        let first = first.unwrap();
        eprintln!("wrong or bare: first round {:.1}%, settled {:.1}%, {} quads", first * 100.0, settled * 100.0, s.held().quads);
        assert!(first <= settled * 2.0, "the first round at {:.1}%, it settles at {:.1}%", first * 100.0, settled * 100.0);
    }

    /// On a 3 px grid, away from silhouettes: rays that hit, those the mesh leaves bare, the mesh
    /// where a ray hits nothing, and the mesh more than 4 px off the ray's depth.
    fn against_rays(root: &SdfNode, s: &Surface, cam: &Cam, pool: &TaskPool) -> (u32, u32, u32, u32) {
        let mut cover = Cover::default();
        cover.fill(cam, 1.0, s, pool);
        let (nx, ny) = (W as usize / 3, H as usize / 3);
        let hit: Vec<Option<f64>> = (0..nx * ny)
            .map(|i| {
                let (o, d) = cam.ray((i % nx) as f64 * 3.0 + 0.5, (i / nx) as f64 * 3.0 + 0.5);
                truth(root, o, d, |t| cam.pw(t))
            })
            .collect();
        let (mut hits, mut bare, mut air, mut off) = (0, 0, 0, 0);
        for y in 0..ny {
            for x in 0..nx {
                let around = |x0: usize, y0: usize| (y0.saturating_sub(1)..(y0 + 2).min(ny)).flat_map(move |j| (x0.saturating_sub(1)..(x0 + 2).min(nx)).map(move |i| j * nx + i));
                let z = cover.at(x * 3, y * 3) as f64;
                let (_, d) = cam.ray(x as f64 * 3.0 + 0.5, y as f64 * 3.0 + 0.5);
                match hit[y * nx + x] {
                    None => air += (z.is_finite() && around(x, y).all(|i| hit[i].is_none())) as u32,
                    Some(t) => {
                        if around(x, y).any(|i| hit[i].is_none()) {
                            continue;
                        }
                        hits += 1;
                        if !z.is_finite() {
                            bare += 1;
                        } else {
                            off += ((cam.along(d, z) - t).abs() > cam.pw(t) * 4.0) as u32;
                        }
                    }
                }
            }
        }
        (hits, bare, air, off)
    }

    /// Each pixel's mesh as the GPU draws it: the fan triangle there, its depth, its normal and its
    /// point, interpolated across the triangle (perspective correct).
    fn raster(s: &Surface, cam: &Cam) -> Vec<Option<(f64, V, V)>> {
        let (w, h) = (cam.w as usize, cam.h as usize);
        let mut out: Vec<Option<(f64, V, V)>> = vec![None; w * h];
        for q in quads(s) {
            let at = |k: usize| -> V { [q[k * 4] as f64, q[k * 4 + 1] as f64, q[k * 4 + 2] as f64] };
            let pts: [V; 5] = [at(0), at(1), at(2), at(3), at(4)];
            let ns: [V; 5] = std::array::from_fn(|k| oct_unpack(q[k * 4 + 3]));
            for k in 0..4 {
                let tri = [4, k, (k + 1) % 4];
                let Some(sc) = tri.iter().map(|&i| cam.proj(pts[i])).collect::<Option<Vec<_>>>() else { continue };
                let (x0, x1) = (sc.iter().map(|p| p.0).fold(f64::INFINITY, f64::min), sc.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max));
                let (y0, y1) = (sc.iter().map(|p| p.1).fold(f64::INFINITY, f64::min), sc.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max));
                let area = (sc[1].0 - sc[0].0) * (sc[2].1 - sc[0].1) - (sc[2].0 - sc[0].0) * (sc[1].1 - sc[0].1);
                if area.abs() < 1e-12 {
                    continue;
                }
                for y in (y0 - 0.5).ceil().max(0.0) as usize..((y1 - 0.5).floor() + 1.0).clamp(0.0, h as f64) as usize {
                    for x in (x0 - 0.5).ceil().max(0.0) as usize..((x1 - 0.5).floor() + 1.0).clamp(0.0, w as f64) as usize {
                        let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                        let e = |a: usize, b: usize| ((sc[b].0 - sc[a].0) * (py - sc[a].1) - (px - sc[a].0) * (sc[b].1 - sc[a].1)) / area;
                        let b = [e(1, 2), e(2, 0), e(0, 1)];
                        if b.iter().any(|&v| v < -1e-9) {
                            continue;
                        }
                        // perspective correct: each corner's weight over its depth
                        let wz: [f64; 3] = std::array::from_fn(|i| b[i] / sc[i].2);
                        let sum: f64 = wz.iter().sum();
                        let z = 1.0 / sum;
                        if out[y * w + x].is_some_and(|o| o.0 <= z) {
                            continue;
                        }
                        let mut n = [0.0; 3];
                        let mut p = [0.0; 3];
                        for i in 0..3 {
                            n = add(n, mul(ns[tri[i]], wz[i] / sum));
                            p = add(p, mul(pts[tri[i]], wz[i] / sum));
                        }
                        out[y * w + x] = Some((z, norm(n), p));
                    }
                }
            }
        }
        out
    }

    /// The field's own normal at `p`: its gradient by central differences.
    fn true_normal(root: &SdfNode, p: V) -> V {
        let e = 1e-4;
        let v = raw(root, &[add(p, [e, 0.0, 0.0]), sub(p, [e, 0.0, 0.0]), add(p, [0.0, e, 0.0]), sub(p, [0.0, e, 0.0]), add(p, [0.0, 0.0, e]), sub(p, [0.0, 0.0, e])]);
        norm([v[0] - v[1], v[2] - v[3], v[4] - v[5]])
    }

    /// A still look close up against the field itself, per pixel the truth's ray hits clear of a
    /// silhouette: how far the mesh's normal turns from the field's (degrees: median, 90th and 99th
    /// percentiles, most; share over 5 and over 15 degrees) and how far the point a pixel's paint
    /// is read at lies from where its ray meets the surface (px: 99th percentile, most).
    fn still_against_field(root: &SdfNode, cam: Cam, before: &[(Cam, Tolerance)]) -> ([f64; 4], [f64; 2], [f64; 2]) {
        let pool = TaskPool::closed();
        let f = Field::new(root, false, &pool).unwrap();
        let (mut s, mut g) = (Surface::new(&f), Gaze::default());
        for (c, tol) in before {
            look(&mut s, &mut g, &f, &pool, *c, tol);
        }
        look(&mut s, &mut g, &f, &pool, cam, &STILL);
        let drawn = raster(&s, &cam);
        let (w, h) = (cam.w as usize, cam.h as usize);
        let hit: Vec<Option<f64>> = (0..w * h)
            .map(|i| {
                let (o, d) = cam.ray((i % w) as f64 + 0.5, (i / w) as f64 + 0.5);
                truth(root, o, d, |t| cam.pw(t))
            })
            .collect();
        let (mut turn, mut apart) = (Vec::new(), Vec::new());
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                let Some(t) = hit[y * w + x] else { continue };
                if (y - 1..y + 2).any(|j| (x - 1..x + 2).any(|i| hit[j * w + i].is_none())) {
                    continue;
                }
                let Some((_, n, p)) = drawn[y * w + x] else { continue };
                let (o, d) = cam.ray(x as f64 + 0.5, y as f64 + 0.5);
                let at = add(o, mul(d, t));
                turn.push(dot(n, true_normal(root, at)).clamp(-1.0, 1.0).acos().to_degrees());
                apart.push(len(sub(p, at)) / cam.pw(t));
            }
        }
        turn.sort_by(f64::total_cmp);
        apart.sort_by(f64::total_cmp);
        let q = |v: &[f64], f: f64| v[((v.len() - 1) as f64 * f) as usize];
        let share = |v: &[f64], over: f64| v.iter().filter(|&&a| a > over).count() as f64 / v.len() as f64;
        ([q(&turn, 0.5), q(&turn, 0.9), q(&turn, 0.99), q(&turn, 1.0)], [share(&turn, 5.0), share(&turn, 15.0)], [q(&apart, 0.99), q(&apart, 1.0)])
    }

    /// Two ellipsoids side by side along x, blended over `blend`: a valley in the plane x = 0.
    fn valley(blend: f64) -> SdfNode {
        let one = |x: f64| transform([x, 0.0, 0.0], [0.0; 3], SdfNode::Ellipsoid { radii: [14.0, 10.0, 10.0] });
        SdfNode::Combine { op: SdfOp::Union, blend, of: vec![one(-9.0), one(9.0)] }
    }

    /// Close up, a still look's shading normal is the field's: on a smooth face, down a sharp blend
    /// valley and along a hard crease, and the point its paint is read at is where the ray meets
    /// the surface.
    #[test]
    fn a_still_look_close_up_shades_with_the_field_s_normals() {
        // the valley's front is about 7.66 mm out at x = 0
        let cam = Cam::looking([0.0, -16.0, 0.4], [0.0, -7.6, 0.0], 480.0, 320.0);
        // as a view comes to it: the whole part still from afar, then moving in
        let zoom: Vec<(Cam, Tolerance)> = std::iter::once((Cam::looking([0.0, -120.0, 30.0], [0.0; 3], 480.0, 320.0), STILL)).chain((1..6).map(|k| (Cam::looking([0.0, -16.0 - 100.0 / (k * k) as f64, 0.4], [0.0, -7.6, 0.0], 480.0, 320.0), MOVING))).collect();
        for (name, root) in [("ellipsoid", transform([0.0; 3], [0.0; 3], SdfNode::Ellipsoid { radii: [14.0, 10.0, 10.0] })), ("blend 0.5 mm valley", valley(0.5)), ("blend 2 mm valley", valley(2.0)), ("hard crease", valley(0.0))] {
            for (how, before) in [("looked at", &[][..]), ("zoomed in on", &zoom[..])] {
                let (turn, over, apart) = still_against_field(&root, cam, before);
                eprintln!(
                    "{name} {how}: normal off the field's by {:.2} median, {:.2} p90, {:.2} p99, {:.1} most degrees; {:.2}% over 5, {:.2}% over 15; paint read {:.2} px p99, {:.2} px most from the surface",
                    turn[0], turn[1], turn[2], turn[3], over[0] * 100.0, over[1] * 100.0, apart[0], apart[1]
                );
                // a hard crease's own pixels see both faces: a few tenths of a percent
                assert!(turn[2] < 1.5 && over[0] < 0.005, "{name} {how}: the normal is off the field's by {:.2} degrees p99, {:.2}% of pixels over 5", turn[2], over[0] * 100.0);
                assert!(apart[1] < 0.6, "{name} {how}: paint is read {:.2} px from the surface", apart[1]);
            }
        }
    }
}
