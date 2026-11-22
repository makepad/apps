//! The program baked for the views: a sparse brick volume per part, and every part's volume
//! packed into one float texture the shaders read (`shader.rs`).
//!
//! Lattice: sample `g` sits at `g * cell` in the part's frame; brick `b` holds samples
//! `8b .. 8b + 7` per axis. A sample is `slot * 65536 + q` ([`sample_value`]), `q` the distance
//! to the nearest boundary over the ramp (`RAMP` cells) in 0..65535: exact in a float, and fine
//! enough that an edge read many pixels to a cell sits where it is to a small part of a pixel. A
//! brick no boundary comes within a ramp of is one slot in the index; only bricks a boundary
//! passes hold samples. A view reads the paint only on the part's surface, so with the part's
//! field given ([`Surface`]) a brick the surface does not come near is not read at all: one
//! unpainted slot.

use crate::program::{Channel, Program, Region, Sample, P3};
use makepad_csg_sdf::{bounds::map_bounds, cut, same, Bounds, Cut, SdfNode, SdfShape};
use std::sync::{Arc, Mutex, OnceLock};

/// Runs `f(i)` for every `i in 0..len`, on the workers its host has, and returns when they have
/// run. A host may skip the rest when the result is no longer wanted: a bake missing a brick
/// gives nothing.
pub type Fan<'a> = &'a (dyn Fn(usize, &(dyn Fn(usize) + Sync)) + Sync);

/// A row-major affine `p' = m p`.
pub type Affine = [[f64; 4]; 3];

fn apply(m: &Affine, p: P3) -> P3 {
    std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3])
}

fn invert(m: &Affine) -> Option<Affine> {
    let a = |r: usize, c: usize| m[r][c];
    let det = a(0, 0) * (a(1, 1) * a(2, 2) - a(1, 2) * a(2, 1)) - a(0, 1) * (a(1, 0) * a(2, 2) - a(1, 2) * a(2, 0)) + a(0, 2) * (a(1, 0) * a(2, 1) - a(1, 1) * a(2, 0));
    if det.abs() < 1e-12 {
        return None;
    }
    let mut out = [[0.0; 4]; 3];
    for i in 0..3 {
        for j in 0..3 {
            let (r1, r2, c1, c2) = ((j + 1) % 3, (j + 2) % 3, (i + 1) % 3, (i + 2) % 3);
            out[i][j] = (a(r1, c1) * a(r2, c2) - a(r1, c2) * a(r2, c1)) / det;
        }
    }
    for i in 0..3 {
        out[i][3] = -(0..3).map(|k| out[i][k] * m[k][3]).sum::<f64>();
    }
    Some(out)
}

/// Where a part's surface is, in the program's frame. A view reads the paint at points on it,
/// each the 4³ lattice points about it (two cells either way), so a brick whose samples the
/// surface comes no nearer than a brick to (room for a traced point's own error) is never read.
pub struct Surface {
    root: Arc<SdfNode>,
    /// The program's frame into the field's.
    into: Affine,
    /// What its last read found.
    last: Mutex<Option<Arc<Read>>>,
    /// The field before an edit and what its last read found ([`Surface::after`]), until a read
    /// takes it.
    before: Mutex<Option<(Arc<SdfNode>, Arc<Read>)>>,
}

/// What a read found: per brick of its lattice (from brick `min`, `dim` bricks a side, pitch
/// `cell`, x fastest) whether a view can read it; `kept` of those it reads as the read before
/// found them.
struct Read {
    min: [i32; 3],
    dim: [i32; 3],
    cell: f64,
    bricks: Vec<bool>,
    kept: usize,
}

impl Read {
    fn at(&self, b: [i32; 3]) -> usize {
        (((b[2] - self.min[2]) * self.dim[1] + (b[1] - self.min[1])) * self.dim[0] + (b[0] - self.min[0])) as usize
    }
}

/// What a block of a read found: the bricks the surface passes, and the boxes of bricks (`lo`,
/// `hi`) the read before found the same in.
#[derive(Default)]
struct Found {
    bricks: Vec<[i32; 3]>,
    kept: Vec<([i32; 3], [i32; 3])>,
}

/// The bricks from `at`, `size` a side, within the lattice from `min` to `end`: None if none.
fn clip(at: [i32; 3], size: i32, min: [i32; 3], end: [i32; 3]) -> Option<([i32; 3], [i32; 3])> {
    let lo: [i32; 3] = std::array::from_fn(|k| at[k].max(min[k]));
    let hi: [i32; 3] = std::array::from_fn(|k| (at[k] + size).min(end[k]));
    (0..3).all(|k| lo[k] < hi[k]).then_some((lo, hi))
}

/// The box the bricks `lo..hi` read in (pitch `cell`): theirs and their neighbours' samples.
fn reach(lo: [i32; 3], hi: [i32; 3], cell: f64) -> (P3, P3) {
    (lo.map(|v| ((v - 1) * BS) as f64 * cell), hi.map(|v| ((v + 1) * BS - 1) as f64 * cell))
}

fn straddles(c: &Cut) -> bool {
    c.lo <= 0.0 && c.hi >= 0.0
}

impl Surface {
    /// The part's field `shape`, `to_program` taking its frame into the program's; `None` for a
    /// frame that flattens it.
    pub fn new(shape: &SdfShape, to_program: &Affine) -> Option<Surface> {
        Some(Surface { into: invert(to_program)?, root: Arc::new(shape.root.clone()), last: Mutex::new(None), before: Mutex::new(None) })
    }

    /// This surface after an edit of `was`'s field (in the same frame): its next read takes
    /// what `was`'s last one found wherever the edit cannot change it.
    pub fn after(self, was: &Surface) -> Surface {
        let last = lock(&was.last).clone();
        if let Some(read) = last.filter(|_| was.into.iter().flatten().map(|v| v.to_bits()).eq(self.into.iter().flatten().map(|v| v.to_bits()))) {
            *lock(&self.before) = Some((was.root.clone(), read));
        }
        self
    }

    /// Box `b` (the program's frame) in the field's.
    fn in_field(&self, b: &(P3, P3)) -> Bounds {
        map_bounds(b, |p| apply(&self.into, p))
    }

    /// Whether the surface can pass through box `b` (the program's frame).
    pub fn passes(&self, b: &(P3, P3)) -> bool {
        straddles(&cut(&self.root, &self.in_field(b)))
    }

    /// Which bricks of the lattice from brick `min`, `dim` bricks a side (pitch `cell`, x
    /// fastest) a view can read: the lattice in blocks of [`BLOCK`] bricks a side (on a grid
    /// of its own, from brick 0) on the workers `fan` runs them on, a block the surface does not
    /// pass skipped whole and the rest split in eight down to bricks, each brick with its
    /// neighbours (the 4³ points a view reads about a point near its side). Each box's test reads
    /// the field cut to the box round it (a cut of a cut is the tree's), so after an edit
    /// ([`Surface::after`]) a box where the field before it cuts to the same tree, its read
    /// before having come to it, holds what that read found there: only the rest is read, and
    /// the bricks found are a whole read's, bit for bit. Nothing if `fan` skipped any block.
    pub fn read(&self, min: [i32; 3], dim: [i32; 3], cell: f64, fan: Fan) -> Option<Vec<bool>> {
        let end: [i32; 3] = std::array::from_fn(|k| min[k] + dim[k]);
        let before = lock(&self.before).take().filter(|(_, r)| r.cell.to_bits() == cell.to_bits());
        let first: [i32; 3] = min.map(|v| v.div_euclid(BLOCK));
        let blocks: [i32; 3] = std::array::from_fn(|k| (end[k] - 1).div_euclid(BLOCK) - first[k] + 1);
        let found: Vec<OnceLock<Found>> = (0..blocks[0] * blocks[1] * blocks[2]).map(|_| OnceLock::new()).collect();
        fan(found.len(), &|i| {
            let i = i as i32;
            let at = [i % blocks[0], (i / blocks[0]) % blocks[1], i / (blocks[0] * blocks[1])];
            let mut out = Found::default();
            let was = before.as_ref().map(|(root, r)| (&**root, &**r));
            self.split(std::array::from_fn(|k| (first[k] + at[k]) * BLOCK), BLOCK, (min, end), cell, &self.root, was, &mut out);
            let _ = found[i as usize].set(out);
        });
        let mut read = Read { min, dim, cell, bricks: vec![false; (dim[0] * dim[1] * dim[2]) as usize], kept: 0 };
        for f in found {
            let f = f.into_inner()?;
            for b in f.bricks {
                let i = read.at(b);
                read.bricks[i] = true;
            }
            let Some((_, was)) = &before else { continue };
            for (lo, hi) in f.kept {
                for z in lo[2]..hi[2] {
                    for y in lo[1]..hi[1] {
                        for x in lo[0]..hi[0] {
                            let (i, held) = (read.at([x, y, z]), was.bricks[was.at([x, y, z])]);
                            read.bricks[i] = held;
                            read.kept += held as usize;
                        }
                    }
                }
            }
        }
        let bricks = read.bricks.clone();
        *lock(&self.last) = Some(Arc::new(read));
        Some(bricks)
    }

    /// How many of the bricks its last read found it took as the read before it found them
    /// (none after a whole read).
    pub fn kept(&self) -> usize {
        lock(&self.last).as_ref().map_or(0, |r| r.kept)
    }

    /// The bricks from `at`, `size` a side, within `lattice` (its first brick and its end), the
    /// surface passes with their neighbours, into `out`: `node` the field cut to the box round
    /// the block holding them. `was`: the field before an edit cut to that box in its read's
    /// lattice, and that read, where it came to the block.
    fn split(&self, at: [i32; 3], size: i32, lattice: ([i32; 3], [i32; 3]), cell: f64, node: &SdfNode, was: Option<(&SdfNode, &Read)>, out: &mut Found) {
        let Some((lo, hi)) = clip(at, size, lattice.0, lattice.1) else { return };
        let c = cut(node, &self.in_field(&reach(lo, hi, cell)));
        if !straddles(&c) {
            return;
        }
        // the read before at this box, if it came to it: its cut there (alike only over the same
        // bricks: a box its lattice clips otherwise is another box)
        let o = was.and_then(|(w, r)| {
            let (a, z) = clip(at, size, r.min, std::array::from_fn(|k| r.min[k] + r.dim[k]))?;
            let o = cut(w, &self.in_field(&reach(a, z, cell)));
            straddles(&o).then_some((o, (a, z) == (lo, hi), r))
        });
        if let Some((o, true, _)) = &o {
            if same(&c.node, &o.node) {
                out.kept.push((lo, hi));
                return;
            }
        }
        if size == 1 {
            out.bricks.push(lo);
            return;
        }
        let h = size / 2;
        for k in 0..8 {
            let was = o.as_ref().map(|(o, _, r)| (&o.node, *r));
            self.split([at[0] + (k & 1) * h, at[1] + ((k >> 1) & 1) * h, at[2] + (k >> 2) * h], h, lattice, cell, &c.node, was, out);
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The bricks a side of the blocks [`Surface::read`] runs at once (a power of two).
const BLOCK: i32 = 16;

/// The ramp the distance is kept over, in cells.
pub const RAMP: f64 = 2.5;
/// Samples per brick edge.
pub const BS: i32 = 8;
/// Texels a brick takes (512 samples, four per texel).
pub const BRICK_TEXELS: usize = 128;
/// At most this many sample bricks per volume (2 KB each): a coarser lattice past it.
pub const MAX_BRICKS: usize = 32_768;

/// A sample's slot step: the distance's levels below it.
const SLOT_STEP: f32 = 65536.0;
/// The distance's top level: a ramp or more.
const FAR: f32 = 65535.0;

/// A lattice sample: `slot`, `over` its distance over the ramp (at most 1).
pub fn sample_value(slot: u8, over: f64) -> f32 {
    slot as f32 * SLOT_STEP + (over.clamp(0.0, 1.0) * FAR as f64).round() as f32
}

/// A sample's slot and its distance over the ramp (0..1).
pub fn sample_parts(v: f32) -> (u8, f64) {
    let slot = (v / SLOT_STEP).floor();
    (slot as u8, (v - slot * SLOT_STEP) as f64 / FAR as f64)
}

/// The threshold a blend's share passes to show its other material at lattice point `g`, 0..1:
/// the R3 low-discrepancy sequence over the lattice, so any share spreads its points evenly with
/// no repeating pattern.
pub fn dither(g: [i32; 3]) -> f32 {
    // the powers of 1 / 1.2207440846 (the root of x^4 = x + 1), in 32-bit fixed point
    const A: [u32; 3] = [3518319155, 2882110345, 2360945575];
    let h = (g[0] as u32).wrapping_mul(A[0]).wrapping_add((g[1] as u32).wrapping_mul(A[1])).wrapping_add((g[2] as u32).wrapping_mul(A[2]));
    (h >> 8) as f32 / (1u32 << 24) as f32
}

#[derive(Clone, Debug, Default)]
pub struct Volume {
    pub cell: f64,
    pub min: [i32; 3],
    pub dim: [i32; 3],
    /// Per brick: `-1 - slot` for a whole brick, else its sample brick.
    pub index: Vec<f32>,
    /// 512 samples per sample brick.
    pub samples: Vec<f32>,
    /// Per brick: whether it holds the program's paint (a bake leaves the bricks no view reads
    /// unpainted: `-1`, as a whole brick of slot 0).
    pub baked: Vec<bool>,
}

impl Volume {
    pub fn bricks(&self) -> usize {
        self.samples.len() / 512
    }
    pub fn bytes(&self) -> usize {
        self.samples.len() * 4 + self.index.len() * 4
    }

    /// The lattice pitch for a program: a fifth of its finest stroke radius, or half its finest
    /// raster's cell (two samples a cell), snapped to 0.4, 0.2 or 0.1 mm so a brick nests in the
    /// program's grid cell (a strokes file's index brick); 0.4 mm without either.
    pub fn pitch(program: &Program) -> f64 {
        let finest = program
            .layers
            .iter()
            .filter_map(|l| match (&l.region, &l.op) {
                (Region::Stroke { radius, .. }, _) => Some(radius * 0.2),
                (Region::Strokes { strokes, .. }, _) => strokes.finest_paint().map(|r| r * 0.2),
                (_, crate::program::Op::Mix { mix, .. }) => match &mix.field {
                    crate::program::MixField::Raster(r) => r.grid.as_ref().map(|g| 0.5 * g.cell),
                    _ => None,
                },
                _ => None,
            })
            .fold(None, |a: Option<f64>, r| Some(a.map_or(r, |a| a.min(r))));
        let coarse = crate::program::CELL / BS as f64;
        finest.map_or(coarse, |want| {
            let k = (coarse / want.clamp(0.1, coarse)).log2().round().clamp(0.0, 2.0);
            coarse / 2f64.powi(k as i32)
        })
    }

    /// The program's `channel` over `part` (its box in the program's frame), at `cell` or
    /// coarser when the sample bricks would pass [`MAX_BRICKS`]: only the bricks round its
    /// `surface` when that is given, all of the box without.
    pub fn bake(program: &Program, channel: Channel, part: (P3, P3), cell: f64, surface: Option<&Surface>, fan: Fan) -> Option<Volume> {
        let mut cell = cell;
        loop {
            let (min, dim) = Self::lattice(part, cell);
            let n = (dim[0] * dim[1] * dim[2]) as usize;
            let at = |i: usize| {
                let i = i as i32;
                [min[0] + i % dim[0], min[1] + (i / dim[0]) % dim[1], min[2] + i / (dim[0] * dim[1])]
            };
            let read = match surface {
                Some(s) => Some(s.read(min, dim, cell, fan)?),
                None => None,
            };
            let baked = bricks(program, channel, cell, n, |i| (at(i), read.as_ref().is_none_or(|r| r[i])), fan)?;
            let fine = baked.iter().filter(|(_, s)| s.is_some()).count();
            if fine > MAX_BRICKS {
                cell *= 2.0;
                continue;
            }
            let mut v = Volume { cell, min, dim, index: Vec::with_capacity(n), samples: Vec::with_capacity(fine * 512), baked: read.unwrap_or_else(|| vec![true; n]) };
            for (e, s) in baked {
                match s {
                    Some(s) => {
                        v.index.push(v.bricks() as f32);
                        v.samples.extend(s);
                    }
                    None => v.index.push(e),
                }
            }
            return Some(v);
        }
    }

    /// [`Volume::bake`] after an edit, from the volume `was` baked before it: a brick's paint is
    /// the program's alone (its field only decides which bricks a view reads), so a brick read
    /// now that `was` holds keeps what it holds unless it meets `changed` (where the program
    /// paints otherwise than the one `was` is of: [`Program::changed_from`]), grown by how far
    /// off a region a brick reads (its samples' neighbours a cell past it, their distance a ramp
    /// out). Only the rest bake: the volume a whole bake makes, byte for byte. Also the bricks
    /// that baked. None: `fan` skipped any; Some(Err): bake whole (`was` is at another cell, or
    /// the lattice passes [`MAX_BRICKS`] at this one).
    pub fn bake_again(program: &Program, channel: Channel, part: (P3, P3), cell: f64, surface: Option<&Surface>, was: &Volume, changed: &[(P3, P3)], fan: Fan) -> Option<Result<(Volume, Vec<[i32; 3]>), ()>> {
        if was.cell != cell || was.baked.len() != was.index.len() {
            return Some(Err(()));
        }
        let (min, dim) = Self::lattice(part, cell);
        let n = (dim[0] * dim[1] * dim[2]) as usize;
        let at = |i: usize| {
            let i = i as i32;
            [min[0] + i % dim[0], min[1] + (i / dim[0]) % dim[1], min[2] + i / (dim[0] * dim[1])]
        };
        let read = match surface {
            Some(s) => s.read(min, dim, cell, fan)?,
            None => vec![true; n],
        };
        let reach = (RAMP + 1.0) * cell;
        let changed: Vec<(P3, P3)> = changed.iter().map(|(lo, hi)| (lo.map(|v| v - reach), hi.map(|v| v + reach))).collect();
        // the brick `was` holds at lattice point `b`, unless the change meets it
        let kept = |b: [i32; 3]| -> Option<usize> {
            let l: [i32; 3] = std::array::from_fn(|k| b[k] - was.min[k]);
            if (0..3).any(|k| !(0..was.dim[k]).contains(&l[k])) {
                return None;
            }
            let i = ((l[2] * was.dim[1] + l[1]) * was.dim[0] + l[0]) as usize;
            let (lo, hi): (P3, P3) = (b.map(|v| (v * BS) as f64 * cell), b.map(|v| (v * BS + BS - 1) as f64 * cell));
            (was.baked[i] && !changed.iter().any(|c| (0..3).all(|k| lo[k] <= c.1[k] && hi[k] >= c.0[k]))).then_some(i)
        };
        let from: Vec<Option<usize>> = (0..n).map(|i| if read[i] { kept(at(i)) } else { None }).collect();
        let todo: Vec<usize> = (0..n).filter(|&i| read[i] && from[i].is_none()).collect();
        let baked = bricks(program, channel, cell, todo.len(), |k| (at(todo[k]), true), fan)?;
        let fine = from.iter().flatten().filter(|&&i| was.index[i] >= 0.0).count() + baked.iter().filter(|(_, s)| s.is_some()).count();
        if fine > MAX_BRICKS {
            return Some(Err(()));
        }
        let mut v = Volume { cell, min, dim, index: Vec::with_capacity(n), samples: Vec::with_capacity(fine * 512), baked: read };
        let mut fresh = baked.into_iter();
        for i in 0..n {
            if !v.baked[i] {
                v.index.push(-1.0);
                continue;
            }
            match from[i] {
                Some(j) if was.index[j] >= 0.0 => {
                    let at = was.index[j] as usize * 512;
                    v.index.push(v.bricks() as f32);
                    v.samples.extend_from_slice(&was.samples[at..at + 512]);
                }
                Some(j) => v.index.push(was.index[j]),
                None => match fresh.next().expect("a brick for each one to bake") {
                    (_, Some(s)) => {
                        v.index.push(v.bricks() as f32);
                        v.samples.extend(s);
                    }
                    (e, None) => v.index.push(e),
                },
            }
        }
        Some(Ok((v, todo.into_iter().map(at).collect())))
    }

    fn lattice(part: (P3, P3), cell: f64) -> ([i32; 3], [i32; 3]) {
        let ramp = RAMP * cell;
        let bs = BS as f64 * cell;
        let (lo, hi) = (part.0.map(|v| v - ramp), part.1.map(|v| v + ramp));
        let min: [i32; 3] = std::array::from_fn(|k| (lo[k] / bs).floor() as i32);
        let max: [i32; 3] = std::array::from_fn(|k| (hi[k] / bs).floor() as i32);
        (min, std::array::from_fn(|k| max[k] - min[k] + 1))
    }
}

/// A brick as [`brick`] gives it.
type Baked = (f32, Option<Vec<f32>>);

/// The `len` bricks `at` names (each with whether a view reads it: one it doesn't is unpainted),
/// on the workers `fan` runs them on: nothing if it skipped any.
fn bricks(program: &Program, channel: Channel, cell: f64, len: usize, at: impl Fn(usize) -> ([i32; 3], bool) + Sync, fan: Fan) -> Option<Vec<Baked>> {
    let out: Vec<OnceLock<Baked>> = (0..len).map(|_| OnceLock::new()).collect();
    fan(len, &|i| {
        let _ = out[i].set(match at(i) {
            (b, true) => brick(program, channel, cell, b),
            _ => (-1.0, None),
        });
    });
    out.into_iter().map(OnceLock::into_inner).collect()
}

/// One brick: whole (`-1 - slot`) or its 512 samples.
fn brick(program: &Program, channel: Channel, cell: f64, b: [i32; 3]) -> (f32, Option<Vec<f32>>) {
    let ramp = RAMP * cell;
    // A brick's samples and the gap to the next brick's first sample: half its diagonal.
    let reach = 0.5 * 8.0 * cell * 3f64.sqrt() + ramp;
    let c: P3 = std::array::from_fn(|k| (b[k] as f64 * 8.0 + 3.5) * cell);
    let s = program.sample_within(channel, c, reach);
    if s.dist > reach {
        return (-1.0 - s.slot as f32, None);
    }
    // the brick's samples read at once (a kernel runs once a layer for them all)
    let samples: Vec<Sample> = program.sample_near_many(channel, &(0..512).map(|j| lattice(b, j).map(|v| v as f64 * cell)).collect::<Vec<_>>(), ramp);
    // a field's material comes with no distance (0): the boundary its neighbours show
    let fielded: Vec<usize> = (0..512).filter(|&j| samples[j].mix.is_none() && samples[j].dist == 0.0).collect();
    let mut field = field_distances(program, channel, cell, b, &samples, &fielded).into_iter();
    let mut out = vec![0f32; 512];
    for (j, (o, s)) in out.iter_mut().zip(&samples).enumerate() {
        *o = match s.mix {
            // a blend shows dithered, one material per lattice point, its share of them the other's
            Some(_) => sample_value(shown(s, lattice(b, j)), 1.0),
            None => {
                let dist = if s.dist == 0.0 { field.next().expect("each fielded sample's distance") } else { s.dist };
                sample_value(s.slot, dist / ramp)
            }
        };
    }
    // a brick of one slot all deeper than the ramp is a whole brick
    let first = sample_parts(out[0]).0;
    if out.iter().all(|v| sample_parts(*v) == (first, 1.0)) {
        return (-1.0 - first as f32, None);
    }
    (0.0, Some(out))
}

/// The material lattice point `g` shows: a blend's dithered one, else its own.
fn shown(s: &Sample, g: [i32; 3]) -> u8 {
    match s.mix {
        Some((other, share)) if share > dither(g) => other,
        _ => s.slot,
    }
}

/// Sample `j` of brick `b` on the lattice (x fastest).
fn lattice(b: [i32; 3], j: usize) -> [i32; 3] {
    [b[0] * BS + (j & 7) as i32, b[1] * BS + ((j >> 3) & 7) as i32, b[2] * BS + (j >> 6) as i32]
}

/// How far each lattice point `js` of brick `b` (showing its slot by a field, which gives no
/// distance) is from the nearest boundary, over the ramp at most: along each axis the nearer
/// neighbour showing another material, the change found between them by bisection, and the
/// three crossings taken as a plane's (exact for one). A blend's neighbour whose nearer
/// material is the point's meets it halfway. `samples` are the brick's, for neighbours inside
/// it; the neighbours past it are read at once, and each bisection step of them all at once
/// (they need only the material, not a distance).
fn field_distances(program: &Program, channel: Channel, cell: f64, b: [i32; 3], samples: &[Sample], js: &[usize]) -> Vec<f64> {
    let ramp = RAMP * cell;
    // each point's six neighbours: in the brick, or the index of a read past it
    let mut past: Vec<P3> = Vec::new();
    let sides: Vec<[(usize, i32, [i32; 3], Result<usize, usize>); 6]> = js
        .iter()
        .map(|&j| {
            let g = lattice(b, j);
            std::array::from_fn(|c| {
                let (k, step) = (c / 2, if c % 2 == 0 { -1i32 } else { 1 });
                let mut n = g;
                n[k] += step;
                let l: [i32; 3] = std::array::from_fn(|i| n[i] - b[i] * BS);
                let at = if l.iter().all(|&v| (0..BS).contains(&v)) {
                    Ok((l[0] + (l[1] + l[2] * BS) * BS) as usize)
                } else {
                    past.push(n.map(|v| v as f64 * cell));
                    Err(past.len() - 1)
                };
                (k, step, n, at)
            })
        })
        .collect();
    let read = program.sample_near_many(channel, &past, 0.0);
    // per point and axis the nearest change; the bisections still open: point, axis, side, lo, hi
    let mut nearest = vec![[f64::INFINITY; 3]; js.len()];
    let mut open: Vec<(usize, usize, i32, f64, f64)> = Vec::new();
    for (q, (&j, sides)) in js.iter().zip(&sides).enumerate() {
        let slot = samples[j].slot;
        for &(k, step, n, at) in sides {
            let s = match at {
                Ok(i) => samples[i],
                Err(i) => read[i],
            };
            if shown(&s, n) == slot {
                continue;
            }
            if s.slot == slot {
                // a blend's point dithered to the other material
                nearest[q][k] = nearest[q][k].min(0.5 * cell);
                continue;
            }
            open.push((q, k, step, 0.0, 1.0));
        }
    }
    for _ in 0..6 {
        let at: Vec<P3> = open
            .iter()
            .map(|&(q, k, step, lo, hi)| {
                let m = 0.5 * (lo + hi);
                let mut p = lattice(b, js[q]).map(|v| v as f64 * cell);
                p[k] += step as f64 * m * cell;
                p
            })
            .collect();
        for (o, s) in open.iter_mut().zip(program.sample_near_many(channel, &at, 0.0)) {
            let m = 0.5 * (o.3 + o.4);
            if s.slot == samples[js[o.0]].slot {
                o.3 = m
            } else {
                o.4 = m
            }
        }
    }
    for (q, k, _, lo, hi) in open {
        nearest[q][k] = nearest[q][k].min(0.5 * (lo + hi) * cell);
    }
    nearest
        .iter()
        .map(|n| {
            let mut inverse = 0.0;
            for d in n.iter().filter(|d| d.is_finite()) {
                inverse += 1.0 / (d * d);
            }
            if inverse > 0.0 { (1.0 / inverse.sqrt()).min(ramp) } else { ramp }
        })
        .collect()
}

impl Volume {
    /// The bricks of `region` (the program's frame) baked again from `program`: what a stroke
    /// being drawn touches. Samples a brick no longer uses stay until [`Volume::bake`] runs; the
    /// result says false when they pass the live bricks (bake whole then), and is nothing, with
    /// the volume as it was, when `fan` skipped any.
    pub fn rebake(&mut self, program: &Program, channel: Channel, region: (P3, P3), fan: Fan) -> Option<bool> {
        self.rebake_boxes(program, channel, &[region], fan)
    }

    /// [`Volume::rebake`] of every brick one of `boxes` reaches (a stroke's capsules, each its own
    /// box: a long diagonal stroke bakes its path, not its bounding box). A brick a sample
    /// brick no longer uses keeps its samples until the volume is packed again
    /// ([`Volume::compact`]); false when they pass the live ones (compact or bake whole then).
    pub fn rebake_boxes(&mut self, program: &Program, channel: Channel, boxes: &[(P3, P3)], fan: Fan) -> Option<bool> {
        let bs = BS as f64 * self.cell;
        let ramp = RAMP * self.cell;
        let mut seen = std::collections::HashSet::new();
        let mut todo: Vec<[i32; 3]> = Vec::new();
        for region in boxes {
            let lo: [i32; 3] = std::array::from_fn(|k| (((region.0[k] - ramp) / bs).floor() as i32).max(self.min[k]));
            let hi: [i32; 3] = std::array::from_fn(|k| (((region.1[k] + ramp) / bs).floor() as i32).min(self.min[k] + self.dim[k] - 1));
            for z in lo[2]..=hi[2] {
                for y in lo[1]..=hi[1] {
                    for x in lo[0]..=hi[0] {
                        if seen.insert([x, y, z]) {
                            todo.push([x, y, z]);
                        }
                    }
                }
            }
        }
        let cell = self.cell;
        let baked = if todo.len() < 16 {
            todo.iter().map(|b| brick(program, channel, cell, *b)).collect()
        } else {
            bricks(program, channel, cell, todo.len(), |i| (todo[i], true), fan)?
        };
        for (b, r) in todo.into_iter().zip(baked) {
            let i = (((b[2] - self.min[2]) * self.dim[1] + (b[1] - self.min[1])) * self.dim[0] + (b[0] - self.min[0])) as usize;
            if let Some(baked) = self.baked.get_mut(i) {
                *baked = true;
            }
            let old = self.index[i];
            match r {
                (e, None) => self.index[i] = e,
                // a brick that holds samples already takes the new ones in place
                (_, Some(samples)) if old >= 0.0 => {
                    let at = old as usize * 512;
                    self.samples[at..at + 512].copy_from_slice(&samples);
                }
                (_, Some(samples)) => {
                    self.index[i] = self.bricks() as f32;
                    self.samples.extend(samples);
                }
            }
        }
        let live = self.index.iter().filter(|e| **e >= 0.0).count();
        Some(self.bricks() <= 2 * live.max(64) && self.bricks() <= MAX_BRICKS * 2)
    }

    /// The samples no brick uses dropped (after rebakes).
    pub fn compact(&mut self) {
        let mut samples = Vec::with_capacity(self.samples.len());
        for e in &mut self.index {
            if *e >= 0.0 {
                let at = *e as usize * 512;
                let n = samples.len() / 512;
                samples.extend_from_slice(&self.samples[at..at + 512]);
                *e = n as f32;
            }
        }
        self.samples = samples;
    }

    /// Its paint on a lattice twice as coarse, for an atlas the device's largest texture cannot
    /// hold at this one: every second sample along each axis (an eighth of them), each sample's
    /// distance over the coarser ramp (a ramp or more stays a ramp or more). A brick whose
    /// samples all keep one slot a ramp off is whole. Its samples stand where they stood: the
    /// paint reads where it did, its boundaries to the coarser lattice's cell.
    pub fn coarser(&self) -> Volume {
        let min: [i32; 3] = std::array::from_fn(|a| self.min[a].div_euclid(2));
        let dim: [i32; 3] = std::array::from_fn(|a| (self.min[a] + self.dim[a] + 1).div_euclid(2) - min[a]);
        // the fine lattice's points and its brick at one
        let (lo, hi): ([i32; 3], [i32; 3]) = (self.min.map(|m| m * BS), std::array::from_fn(|a| (self.min[a] + self.dim[a]) * BS - 1));
        let brick = |b: [i32; 3]| ((b[2] - self.min[2]) * self.dim[1] + b[1] - self.min[1]) * self.dim[0] + b[0] - self.min[0];
        let fine = |g: [i32; 3]| {
            let g: [i32; 3] = std::array::from_fn(|a| g[a].clamp(lo[a], hi[a]));
            let e = self.index[brick(g.map(|v| v.div_euclid(BS))) as usize];
            if e < 0.0 {
                return sample_value((-1.0 - e) as u8, 1.0);
            }
            let l = g.map(|v| v.rem_euclid(BS));
            self.samples[e as usize * 512 + ((l[2] * BS + l[1]) * BS + l[0]) as usize]
        };
        let n = (dim[0] * dim[1] * dim[2]) as usize;
        let mut v = Volume { cell: self.cell * 2.0, min, dim, index: Vec::with_capacity(n), samples: Vec::new(), baked: Vec::with_capacity(n) };
        for i in 0..n as i32 {
            let b = [min[0] + i % dim[0], min[1] + (i / dim[0]) % dim[1], min[2] + i / (dim[0] * dim[1])];
            // the fine bricks under it (those in the fine volume: the rest read its edge)
            let under: Vec<usize> = (0..8)
                .map(|k| [2 * b[0] + (k & 1), 2 * b[1] + ((k >> 1) & 1), 2 * b[2] + (k >> 2)])
                .filter(|f| (0..3).all(|a| f[a] >= self.min[a] && f[a] < self.min[a] + self.dim[a]))
                .map(|f| brick(f) as usize)
                .collect();
            v.baked.push(under.iter().any(|&f| self.baked.get(f).copied().unwrap_or(true)));
            let whole = self.index[under[0]];
            if whole < 0.0 && under.iter().all(|&f| self.index[f] == whole) {
                v.index.push(whole);
                continue;
            }
            let samples: Vec<f32> = (0..512)
                .map(|j| {
                    let (slot, over) = sample_parts(fine(lattice(b, j).map(|g| 2 * g)));
                    sample_value(slot, if over >= 1.0 { 1.0 } else { 0.5 * over })
                })
                .collect();
            let first = sample_parts(samples[0]);
            if first.1 >= 1.0 && samples.iter().all(|&s| s == samples[0]) {
                v.index.push(-1.0 - first.0 as f32);
            } else {
                v.index.push(v.bricks() as f32);
                v.samples.extend(samples);
            }
        }
        v
    }
}

/// One part's entry in the atlas.
pub struct AtlasPart<'a> {
    pub volume: &'a Volume,
    /// Rows of the affine from the shader's position to the volume's frame (mm).
    pub from: [[f32; 4]; 3],
    /// Linear RGB per slot (slot 0 unused).
    pub palette: &'a [[f32; 3]],
    /// The least light its paint shows at, as a share of its colour at the renderer's exposure
    /// (`PaintLookup::paint_shade`): 0 shades it like the part, lit as the scene lights it; a
    /// mark that must read on a face no light reaches (a support tint under an overhang) more.
    pub floor: f32,
}

/// Every part's volume as RGBA32F texels starting at texel `base` of a texture `width` wide
/// (the host's own table may sit before it). Per volume a header: `(cell, ramp, min.x, min.y)`,
/// `(min.z, dim.x, dim.y, dim.z)`, `(index, samples, palette, floor)` texel bases and the part's
/// light floor, three affine rows;
/// then the brick index (four per texel), the palette, the samples (128 texels a brick). All
/// texel numbers are absolute, each stored as [`texel_number`] makes it (the headers too).
#[derive(Clone, Debug, Default)]
pub struct Atlas {
    /// The texels from `base` on (whole rows: the texture's rows from `base / width`).
    pub data: Vec<f32>,
    /// Each part's header texel, in the order given.
    pub headers: Vec<f32>,
}

/// Texel `n` as the atlas stores a texel number: the float whose bits are the smallest normal
/// float's plus `n`, which `mod.draw.PaintLookup::paint_number` reads back whole. An f32 holds
/// every whole number only to 2^24 (16.7M texels, a 268 MB atlas: a fine Force overlay's reach);
/// these stay exact through 2^31 - 2^23, as far as the shader's `u32` reckons, and every one is
/// above zero, so a negative value still says none.
pub fn texel_number(n: usize) -> f32 {
    f32::from_bits(0x0080_0000 + n as u32)
}

/// The texel a stored number names ([`texel_number`]), None for none.
pub fn texel_at(v: f32) -> Option<usize> {
    (v > 0.0).then(|| (v.to_bits() - 0x0080_0000) as usize)
}

/// How many colours an overlay's palette ramps over, evenly from value 0 to value 1.
pub const OVERLAY_RAMP: usize = 33;

/// A host's lattice in an overlay: one value per texel's channel, x fastest (four per texel).
#[derive(Clone, Copy, Debug)]
pub struct OverlayLattice<'a> {
    /// The corner of cell (0, 0, 0) and the cell's side, in the overlay's frame.
    pub origin: [f32; 3],
    pub h: f32,
    /// Cells per axis.
    pub cells: [usize; 3],
    pub data: &'a [f32],
}

/// A lattice of an overlay's that holds its data ([`OverlayLattice`] borrows it): a host's
/// lattice made coarser for an atlas the device cannot hold.
#[derive(Clone, Debug)]
pub struct OverlayGrid {
    pub origin: [f32; 3],
    pub h: f32,
    pub cells: [usize; 3],
    pub data: Vec<f32>,
}

impl OverlayGrid {
    pub fn lattice(&self) -> OverlayLattice<'_> {
        OverlayLattice { origin: self.origin, h: self.h, cells: self.cells, data: &self.data }
    }
}

impl OverlayLattice<'_> {
    /// Values at the nodes (cells + 1 per axis) on cells twice the side, from the same corner:
    /// every second node along each axis, the last node standing in past the lattice's end.
    pub fn coarser_nodes(&self) -> OverlayGrid {
        let cells = self.cells.map(|c| c.div_ceil(2));
        let n = self.cells.map(|c| c + 1);
        let mut data = Vec::with_capacity((cells[0] + 1) * (cells[1] + 1) * (cells[2] + 1));
        for k in 0..=cells[2] {
            for j in 0..=cells[1] {
                for i in 0..=cells[0] {
                    let f = [(2 * i).min(self.cells[0]), (2 * j).min(self.cells[1]), (2 * k).min(self.cells[2])];
                    data.push(self.data.get((f[2] * n[1] + f[1]) * n[0] + f[0]).copied().unwrap_or(-1.0));
                }
            }
        }
        OverlayGrid { origin: self.origin, h: self.h * 2.0, cells, data }
    }

    /// Marks per cell on cells twice the side, from the same corner: each the first mark among
    /// the eight cells it covers, else none.
    pub fn coarser_cells(&self) -> OverlayGrid {
        let cells = self.cells.map(|c| c.div_ceil(2));
        let at = |i: usize, j: usize, k: usize| self.data.get((k * self.cells[1] + j) * self.cells[0] + i).copied().unwrap_or(0.0);
        let mut data = Vec::with_capacity(cells[0] * cells[1] * cells[2]);
        for k in 0..cells[2] {
            for j in 0..cells[1] {
                for i in 0..cells[0] {
                    let under = (0..8).map(|q| (2 * i + (q & 1), 2 * j + ((q >> 1) & 1), 2 * k + (q >> 2))).filter(|&(x, y, z)| x < self.cells[0] && y < self.cells[1] && z < self.cells[2]);
                    data.push(under.map(|(x, y, z)| at(x, y, z)).find(|&m| m > 0.0).unwrap_or(0.0));
                }
            }
        }
        OverlayGrid { origin: self.origin, h: self.h * 2.0, cells, data }
    }
}

/// What a host shows over a part, read where the surface is (`mod.draw.PaintLookup`): its values
/// at the lattice's nodes (cells + 1 per axis; 0..1 over the palette's ramp, above 1 its over
/// colour, negative none), its marks per cell (0, or a kind 1..=8 + 16 (x + 128 y), x and y the
/// marked surface's normal with z up, at 0..=127 over -1..1).
#[derive(Clone, Copy, Debug)]
pub struct Overlay<'a> {
    pub values: Option<OverlayLattice<'a>>,
    pub marks: Option<OverlayLattice<'a>>,
    /// Linear: the [`OVERLAY_RAMP`] colours, the colour above 1 (`a` 1: shown, else the ramp's
    /// last), the four kinds of mark (5..=8 are 1..=4 picked).
    pub palette: &'a [[f32; 4]],
}

/// One way a renderer sees an overlay's part: `lane` takes its position to the frame its paint
/// lookup is given (`paint_frame`; a lane that passes its point straight never reads it), from
/// which `lattice` goes to the overlay's frame and `paint` to the volume of the paint header
/// `under`.
#[derive(Clone, Copy, Debug)]
pub struct OverlayFrame {
    pub under: Option<f32>,
    pub lane: [[f32; 4]; 3],
    pub lattice: [[f32; 4]; 3],
    pub paint: [[f32; 4]; 3],
}

/// Texels in an overlay's header: `(-1, under or -1, values or -1, marks or -1)` with absolute
/// texel bases ([`texel_number`]), the values' `(origin, h)` and `(cells, palette)`, three
/// `lane` rows, three `lattice` rows, the marks' `(origin, h)` and `(cells, 0)`, three `paint`
/// rows.
const OVERLAY_HEADER: usize = 14;

impl Atlas {
    /// `overlay` after what is packed, one header per frame sharing its data: their texels. `base`
    /// and `width` as [`Atlas::pack`]'s.
    pub fn overlay(&mut self, base: usize, width: usize, overlay: &Overlay, frames: &[OverlayFrame]) -> Vec<f32> {
        let at = base + self.data.len() / 4;
        let start = self.data.len();
        let len = |l: &Option<OverlayLattice>| l.as_ref().map_or(0, |l| l.data.len());
        let texels = Self::overlay_texels(width, frames.len(), overlay.palette.len(), len(&overlay.values), len(&overlay.marks));
        // straight into the atlas, reserved once: the headers (filled below), the palette, then
        // each lattice four values a texel, its last texel's missing values -1 (none)
        self.data.reserve_exact(texels * 4);
        self.data.resize(start + frames.len() * OVERLAY_HEADER * 4, 0.0);
        let palette = at + frames.len() * OVERLAY_HEADER;
        self.data.extend_from_slice(overlay.palette.as_flattened());
        let lattice = |data: &mut Vec<f32>, l: &Option<OverlayLattice>| match l {
            Some(l) => {
                let first = at + (data.len() - start) / 4;
                data.extend_from_slice(l.data);
                data.resize(start + (data.len() - start).next_multiple_of(4), -1.0);
                texel_number(first)
            }
            None => -1.0,
        };
        let values = lattice(&mut self.data, &overlay.values);
        let marks = lattice(&mut self.data, &overlay.marks);
        let spec = |l: &Option<OverlayLattice>, last: f32| match l {
            Some(l) => ([l.origin[0], l.origin[1], l.origin[2], l.h], [l.cells[0] as f32, l.cells[1] as f32, l.cells[2] as f32, last]),
            None => ([0.0; 4], [1.0, 1.0, 1.0, last]),
        };
        let mut headers = Vec::with_capacity(frames.len());
        for (k, f) in frames.iter().enumerate() {
            let h = k * OVERLAY_HEADER;
            headers.push(texel_number(at + h));
            let (v0, v1) = spec(&overlay.values, texel_number(palette));
            let (m0, m1) = spec(&overlay.marks, 0.0);
            let header: [[f32; 4]; OVERLAY_HEADER] = [
                [-1.0, f.under.unwrap_or(-1.0), values, marks],
                v0,
                v1,
                f.lane[0],
                f.lane[1],
                f.lane[2],
                f.lattice[0],
                f.lattice[1],
                f.lattice[2],
                m0,
                m1,
                f.paint[0],
                f.paint[1],
                f.paint[2],
            ];
            self.data[start + h * 4..][..OVERLAY_HEADER * 4].copy_from_slice(header.as_flattened());
        }
        self.data.resize(start + texels * 4, 0.0);
        headers
    }

    /// The texels [`Atlas::overlay`] adds, in whole rows of `width`: `frames` headers, a palette
    /// of `palette` colours and lattices of `values` and `marks` values (0: none). An atlas is
    /// sized by it before it packs.
    pub fn overlay_texels(width: usize, frames: usize, palette: usize, values: usize, marks: usize) -> usize {
        let width = width.max(1);
        (frames * OVERLAY_HEADER + palette + values.div_ceil(4) + marks.div_ceil(4)).div_ceil(width) * width
    }

    /// The texels [`Atlas::pack`] packs `parts` into, in whole rows of `width`.
    pub fn pack_texels(parts: &[AtlasPart], width: usize) -> usize {
        // each volume once: its index (whole texels), its palette (slot 0 a texel too), its samples
        let size = |p: &AtlasPart| p.volume.index.len().div_ceil(4) + p.palette.len().max(1) + p.volume.samples.len() / 4;
        let mut texels = 0;
        for (i, p) in parts.iter().enumerate() {
            texels += 6 + if parts[..i].iter().any(|q| std::ptr::eq(q.volume, p.volume)) { 0 } else { size(p) };
        }
        texels.div_ceil(width.max(1)) * width.max(1)
    }

    /// Parts that share a volume (one part seen through two frames: its mesh and its field)
    /// share its index and samples. `base` is a multiple of `width`. Its texels are written in
    /// place, the samples a copy each: the whole atlas packs again on the frame a bake comes in.
    pub fn pack(parts: &[AtlasPart], base: usize, width: usize) -> Atlas {
        Self::pack_onto(Vec::new(), parts, base, width)
    }

    /// [`Atlas::pack`] after what `data` holds, whole rows of `width`: texel `base` is `data`'s
    /// first, and the atlas's data is `data` with the atlas after it. The element lookup packs
    /// onto its table's rows, so its texture's data is written once.
    pub fn pack_onto(mut t: Vec<f32>, parts: &[AtlasPart], base: usize, width: usize) -> Atlas {
        let start = t.len();
        let texels = Self::pack_texels(parts, width);
        t.reserve(texels * 4);
        let mut headers = Vec::with_capacity(parts.len());
        let mut packed: Vec<(*const Volume, usize, usize, usize)> = Vec::new();
        for p in parts {
            let v = p.volume;
            let k = t.len();
            let h = base + k / 4;
            headers.push(texel_number(h));
            t.resize(k + 6 * 4, 0.0);
            let (index, palette, samples) = match packed.iter().find(|e| std::ptr::eq(e.0, v)) {
                Some(e) => (e.1, e.2, e.3),
                None => {
                    let index = base + t.len() / 4;
                    t.extend_from_slice(&v.index);
                    t.resize(t.len().next_multiple_of(4), 0.0);
                    let palette = base + t.len() / 4;
                    t.extend_from_slice(&[0.0; 4]);
                    for c in p.palette.iter().skip(1) {
                        t.extend_from_slice(&[c[0], c[1], c[2], 1.0]);
                    }
                    let samples = base + t.len() / 4;
                    t.extend_from_slice(&v.samples);
                    packed.push((v as *const Volume, index, palette, samples));
                    (index, palette, samples)
                }
            };
            let header = [
                [v.cell as f32, (RAMP * v.cell) as f32, v.min[0] as f32, v.min[1] as f32],
                [v.min[2] as f32, v.dim[0] as f32, v.dim[1] as f32, v.dim[2] as f32],
                [texel_number(index), texel_number(samples), texel_number(palette), p.floor],
                p.from[0],
                p.from[1],
                p.from[2],
            ];
            t[k..k + 24].copy_from_slice(header.as_flattened());
        }
        t.resize(start + texels * 4, 0.0);
        Atlas { data: t, headers }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::program::{Layer, Op};

    #[test]
    fn a_stroke_bakes_sparse_and_reads_back() {
        let p = Program::new(vec![Layer { region: Region::Stroke { points: vec![[0.0, 0.0, 0.0], [20.0, 0.0, 0.0]], radius: 2.0 }, op: Op::Paint { channel: Channel::Color, slot: 1 } }]);
        let v = Volume::bake(&p, Channel::Color, ([-5.0, -5.0, -5.0], [25.0, 5.0, 5.0]), Volume::pitch(&p), None, &|len, f| (0..len).for_each(f)).unwrap();
        assert!((v.cell - 0.4).abs() < 1e-12);
        assert!(v.bricks() > 0 && v.bricks() < v.index.len(), "{} of {}", v.bricks(), v.index.len());
        let part = AtlasPart { volume: &v, from: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]], palette: &[[0.0; 3], [1.0, 0.0, 0.0]], floor: 0.0 };
        let parts = [part, AtlasPart { from: [[2.0, 0.0, 0.0, 0.0], [0.0, 2.0, 0.0, 0.0], [0.0, 0.0, 2.0, 0.0]], floor: 0.7, ..AtlasPart { volume: &v, from: [[0.0; 4]; 3], palette: &[[0.0; 3], [1.0, 0.0, 0.0]], floor: 0.0 } }];
        let a = Atlas::pack(&parts, 256, 256);
        assert_eq!(a.data.len() % (256 * 4), 0);
        assert_eq!(a.data.len() / 4, Atlas::pack_texels(&parts, 256), "the size an atlas is sized by before it packs");
        let onto = Atlas::pack_onto(vec![7.0; 256 * 4], &parts, 0, 256);
        assert!(onto.data[..256 * 4] == [7.0; 256 * 4] && onto.data[256 * 4..] == a.data[..] && onto.headers == a.headers, "packed onto a row it keeps, the atlas after it as packed from that texel");
        assert_eq!(texel_at(a.headers[0]), Some(256));
        // the second header shares the first's index and samples
        let bases = |h: f32| &a.data[(texel_at(h).unwrap() - 256 + 2) * 4..][..4];
        assert_eq!(bases(a.headers[1])[0], bases(a.headers[0])[0]);
        // and each keeps its own light floor where the lookup reads it (texel 2's w)
        assert_eq!((bases(a.headers[0])[3], bases(a.headers[1])[3]), (0.0, 0.7));
        // the index, the palette and the samples go in as they are, from the bases it gives
        let at = |k: usize| (texel_at(bases(a.headers[0])[k]).unwrap() - 256) * 4;
        assert_eq!(&a.data[at(0)..][..v.index.len()], &v.index[..]);
        assert_eq!(&a.data[at(2)..][..8], &[0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0]);
        assert_eq!(&a.data[at(1)..][..v.samples.len()], &v.samples[..]);
    }

    /// `n` placed at `p`.
    fn at(p: P3, n: makepad_csg_sdf::SdfNode) -> makepad_csg_sdf::SdfNode {
        makepad_csg_sdf::SdfNode::Transform { at: p, rotate: [0.0; 3], scale: 1.0, of: Box::new(n) }
    }

    fn combine(op: makepad_csg_sdf::SdfOp, blend: f64, of: Vec<makepad_csg_sdf::SdfNode>) -> makepad_csg_sdf::SdfNode {
        makepad_csg_sdf::SdfNode::Combine { op, blend, of }
    }

    fn tag(id: u32, n: makepad_csg_sdf::SdfNode) -> makepad_csg_sdf::SdfNode {
        makepad_csg_sdf::SdfNode::Id { id, of: Box::new(n) }
    }

    /// The machine's threads, or fewer when the run was given a CPU budget (`MAKEPAD_CPU_CORES`, a
    /// devhub lease's width): a test sized from the box runs past the width its run was told.
    pub(crate) fn budget_threads() -> usize {
        let machine = std::thread::available_parallelism().map_or(4, |n| n.get());
        std::env::var("MAKEPAD_CPU_CORES").ok().and_then(|v| v.trim().parse::<usize>().ok()).filter(|n| *n > 0).map_or(machine, |n| n.min(machine))
    }

    /// On every worker at once.
    fn threads(len: usize, f: &(dyn Fn(usize) + Sync)) {
        let next = std::sync::atomic::AtomicUsize::new(0);
        let n = budget_threads();
        std::thread::scope(|s| {
            for _ in 0..n {
                s.spawn(|| loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if i >= len {
                        break;
                    }
                    f(i);
                });
            }
        });
    }

    /// A part's paint as a script fills it: the whole part, then features of its field
    /// (`fills`: tag, slot), then a solid of its own (a band round it) and a brushed stroke, over
    /// its field's surface.
    fn filled(root: &makepad_csg_sdf::SdfNode, fills: &[(u32, u8)], band: &makepad_csg_sdf::SdfNode, stroke: [P3; 2]) -> (Program, Surface, (P3, P3)) {
        use crate::program::Layer;
        let body = crate::operand::Body::new(root);
        let paint = |slot: u8| Op::Paint { channel: Channel::Color, slot };
        let mut layers: Vec<Layer> = fills.iter().map(|&(t, slot)| Layer { region: Region::Operand(std::sync::Arc::new(vec![body.operand(t).expect("tagged")])), op: paint(slot) }).collect();
        layers.push(Layer { region: Region::Field { node: band.clone(), grow: 0.0 }, op: paint(5) });
        layers.push(Layer { region: Region::Stroke { points: stroke.to_vec(), radius: 1.2 }, op: paint(6) });
        (Program::new(layers), surface_of(root), makepad_csg_sdf::node_bounds(root).expect("bounded"))
    }

    /// The surface of field `root` in the program's own frame.
    fn surface_of(root: &makepad_csg_sdf::SdfNode) -> Surface {
        let (lo, hi) = makepad_csg_sdf::node_bounds(root).expect("bounded");
        let shape = SdfShape { root: root.clone(), min: lo, max: hi, materials: Vec::new() };
        Surface::new(&shape, &[[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]]).expect("a frame")
    }

    /// An edit of one feature of a part (a parameter set: an arm's thickness, a vase's lip or
    /// neck) bakes only the bricks round that feature, where its program paints otherwise; every
    /// other brick the volume before it holds is kept, and the volume is the one a whole bake
    /// makes of the edited part, byte for byte: on a figure of blended ellipsoids with a carved
    /// eye and on a vase with a wall 0.8 mm thin, its neck in a blend 4 mm wide. And the surface
    /// is read again only round the edit: the bricks a view reads are the whole read's, most of
    /// them as the read before found them.
    #[test]
    fn an_edit_bakes_only_the_bricks_it_can_change_and_makes_the_whole_bake_s_volume() {
        use makepad_csg_sdf::{SdfNode, SdfOp};
        let figure = |arm: f64| {
            let torso = tag(1, SdfNode::Ellipsoid { radii: [10.0, 14.0, 7.0] });
            let arm = tag(2, at([15.0, 4.0, 0.0], SdfNode::Ellipsoid { radii: [9.0, arm, 3.0] }));
            let eye = tag(4, at([2.5, 22.0, 5.5], SdfNode::Sphere { radius: 1.6 }));
            let head = tag(3, combine(SdfOp::Difference, 0.3, vec![at([0.0, 21.0, 0.0], SdfNode::Ellipsoid { radii: [7.0, 6.0, 6.5] }), eye]));
            tag(9, combine(SdfOp::Union, 1.0, vec![combine(SdfOp::Union, 1.5, vec![torso, arm]), head]))
        };
        let vase = |lip: f64, neck: f64| {
            let body = combine(SdfOp::Union, 4.0, vec![at([0.0, 10.0, 0.0], SdfNode::Ellipsoid { radii: [10.0, 9.0, 10.0] }), tag(2, at([0.0, 24.0, 0.0], SdfNode::Cylinder { radius: neck, top_radius: neck, height: 14.0 }))]);
            let lip = tag(3, at([0.0, 31.0, 0.0], SdfNode::Torus { radius: 4.4, tube: lip }));
            tag(9, combine(SdfOp::Union, 0.6, vec![SdfNode::Shell { thickness: 0.8, of: Box::new(body) }, lip]))
        };
        let band = at([0.0, 6.0, 0.0], SdfNode::Shell { thickness: 1.0, of: Box::new(SdfNode::Box { size: [40.0, 3.0, 40.0], radius: 0.0 }) });
        type Shape = Box<dyn Fn(f64) -> SdfNode>;
        let cases: [(&str, Shape, [f64; 2], &[(u32, u8)], [P3; 2]); 3] = [
            ("figure", Box::new(figure), [2.5, 3.2], &[(9, 1), (2, 2), (3, 3), (4, 4)], [[-6.0, -2.0, 6.5], [-2.0, 4.0, 6.0]]),
            ("vase", Box::new(move |lip| vase(lip, 4.0)), [0.9, 1.2], &[(9, 1), (2, 2), (3, 3)], [[-9.5, 9.0, 3.0], [-7.0, 9.0, 7.0]]),
            ("neck", Box::new(move |neck| vase(0.9, neck)), [4.0, 4.3], &[(9, 1), (2, 2), (3, 3)], [[-9.5, 9.0, 3.0], [-7.0, 9.0, 7.0]]),
        ];
        let cell = 0.2;
        for (name, shape, [before, after], fills, stroke) in cases {
            let (old, old_surface, old_box) = filled(&shape(before), fills, &band, stroke);
            let (new, surface, part) = filled(&shape(after), fills, &band, stroke);
            let was = Volume::bake(&old, Channel::Color, old_box, cell, Some(&old_surface), &threads).expect("baked");
            let whole = Volume::bake(&new, Channel::Color, part, cell, Some(&surface), &threads).expect("baked");
            assert_eq!(whole.cell, cell, "{name}: the lattice coarsened");
            let changed = new.changed_from(&old).expect("a bounded change");
            let read_again = surface_of(&shape(after)).after(&old_surface);
            let (again, baked) = Volume::bake_again(&new, Channel::Color, part, cell, Some(&read_again), &was, &changed, &threads).expect("baked").expect("from the volume before");
            assert_eq!((again.min, again.dim), (whole.min, whole.dim), "{name}: another lattice");
            assert_eq!(again.baked, whole.baked, "{name}: other bricks read");
            let read = whole.baked.iter().filter(|r| **r).count();
            println!("{name}: the surface read again found {read} bricks, {} of them as the read before", read_again.kept());
            assert!(read_again.kept() > 0, "{name}: the read again kept none of {read} bricks");
            let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
            assert!(bits(&again.index) == bits(&whole.index), "{name}: the index differs from the whole bake's");
            assert!(bits(&again.samples) == bits(&whole.samples), "{name}: the samples differ from the whole bake's");
            // only round the edited feature: the head, the torso's far side, the vase's foot keep theirs
            let middles: Vec<P3> = baked.iter().map(|b| b.map(|v| (v as f64 * 8.0 + 3.5) * cell)).collect();
            let (lo, hi) = middles.iter().fold(([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]), |(lo, hi), m| (std::array::from_fn(|k| lo[k].min(m[k])), std::array::from_fn(|k| hi[k].max(m[k]))));
            println!("{name}: baked {} of {read} bricks read again, their middles within {lo:?}..{hi:?}", baked.len());
            // (the neck reaches down the body as far as the blend round it, most of the vase)
            let most = if name == "neck" { 0.8 } else { 1.0 / 3.0 };
            assert!(!baked.is_empty() && (baked.len() as f64) < most * read as f64, "{name}: baked {} of {read} bricks", baked.len());
            match name {
                "figure" => assert!(lo[0] > -3.0 && hi[1] < 16.0, "the arm's edit baked the head's or the torso's far side's bricks: {lo:?}..{hi:?}"),
                "vase" => assert!(lo[1] > 20.0, "the lip's edit baked the body's bricks: {lo:?}..{hi:?}"),
                _ => assert!(lo[1] > 6.0, "the neck's edit baked the foot's bricks: {lo:?}..{hi:?}"),
            }
        }
    }

    struct Place(f64);
    impl makepad_csg_sdf::Sdf3 for Place {
        fn distance(&self, _: makepad_csg_math::Vec3d) -> f64 {
            self.0
        }
    }

    /// A blend bakes dithered: each lattice point one material, the other's share of the points
    /// its share of the blend, so the view shows the mix as it prints.
    #[test]
    fn a_blend_bakes_its_share_of_points() {
        for share in [0.1, 0.3, 0.5] {
            let field = makepad_csg_sdf::SdfKernel { key: 1, field: std::sync::Arc::new(Place(share)), splash: None, bounds: None, solid: None };
            let mix = crate::program::Mix { field: crate::program::MixField::Fn(field), per_mm: 0.001, slots: vec![1, 2], sharp: 0.0, balance: 0.0 };
            let p = Program::new(vec![Layer { region: Region::All, op: Op::Mix { channel: Channel::Color, mix: std::sync::Arc::new(mix) } }]);
            let v = Volume::bake(&p, Channel::Color, ([0.0; 3], [10.0; 3]), 0.4, None, &|len, f| (0..len).for_each(f)).unwrap();
            let (n, other) = (v.samples.len(), v.samples.iter().filter(|s| sample_parts(**s).0 == 2).count());
            assert!(n > 0 && v.samples.iter().all(|s| matches!(sample_parts(*s).0, 1 | 2)));
            let got = other as f64 / n as f64;
            assert!((got - share).abs() < 0.01, "a share of {share} bakes {got:.3} of the points");
        }
    }

    struct Field(fn(makepad_csg_math::Vec3d) -> f64);
    impl makepad_csg_sdf::Sdf3 for Field {
        fn distance(&self, p: makepad_csg_math::Vec3d) -> f64 {
            (self.0)(p)
        }
    }

    /// A brick baked a point at a time, each sample and each bisection step read alone: the
    /// bake as it was before it read a brick at once, kept as the oracle the batched one must
    /// equal.
    fn brick_alone(program: &Program, channel: Channel, cell: f64, b: [i32; 3]) -> Baked {
        let ramp = RAMP * cell;
        let reach = 0.5 * 8.0 * cell * 3f64.sqrt() + ramp;
        let c: P3 = std::array::from_fn(|k| (b[k] as f64 * 8.0 + 3.5) * cell);
        let s = program.sample(channel, c);
        if s.dist > reach {
            return (-1.0 - s.slot as f32, None);
        }
        let samples: Vec<Sample> = (0..512).map(|j| program.sample_near(channel, lattice(b, j).map(|v| v as f64 * cell), ramp)).collect();
        let field_distance = |g: [i32; 3], slot: u8| -> f64 {
            let mut inverse = 0.0;
            for k in 0..3 {
                let mut nearest = f64::INFINITY;
                for step in [-1i32, 1] {
                    let mut n = g;
                    n[k] += step;
                    let l: [i32; 3] = std::array::from_fn(|i| n[i] - b[i] * BS);
                    let s = if l.iter().all(|&v| (0..BS).contains(&v)) { samples[(l[0] + (l[1] + l[2] * BS) * BS) as usize] } else { program.sample(channel, n.map(|v| v as f64 * cell)) };
                    if shown(&s, n) == slot {
                        continue;
                    }
                    if s.slot == slot {
                        nearest = nearest.min(0.5 * cell);
                        continue;
                    }
                    let (mut lo, mut hi) = (0.0, 1.0);
                    for _ in 0..6 {
                        let m = 0.5 * (lo + hi);
                        let mut p = g.map(|v| v as f64 * cell);
                        p[k] += step as f64 * m * cell;
                        if program.sample(channel, p).slot == slot { lo = m } else { hi = m }
                    }
                    nearest = nearest.min(0.5 * (lo + hi) * cell);
                }
                if nearest.is_finite() {
                    inverse += 1.0 / (nearest * nearest);
                }
            }
            if inverse > 0.0 { (1.0 / inverse.sqrt()).min(ramp) } else { ramp }
        };
        let mut out = vec![0f32; 512];
        for (j, (o, s)) in out.iter_mut().zip(&samples).enumerate() {
            let g = lattice(b, j);
            *o = match s.mix {
                Some(_) => sample_value(shown(s, g), 1.0),
                None => {
                    let dist = if s.dist == 0.0 { field_distance(g, s.slot) } else { s.dist };
                    sample_value(s.slot, dist / ramp)
                }
            };
        }
        let first = sample_parts(out[0]).0;
        if out.iter().all(|v| sample_parts(*v) == (first, 1.0)) {
            return (-1.0 - first as f32, None);
        }
        (0.0, Some(out))
    }

    /// A brick read at once bakes exactly as read a point at a time: the same whole bricks, the
    /// same materials and the same distances, through a steep scripted region (its slope read
    /// near its edge), strokes, an erase and a sharp and a soft mix (their edges bisected).
    #[test]
    fn a_brick_read_at_once_bakes_as_read_a_point_at_a_time() {
        use makepad_csg_sdf::{SdfKernel, SdfNode};
        let kernel = |f: fn(makepad_csg_math::Vec3d) -> f64| SdfKernel { key: f as usize as u64, field: std::sync::Arc::new(Field(f)), splash: None, bounds: None, solid: None };
        // three times as steep as a distance: its slope is read near its edge
        let steep = SdfNode::Kernel(kernel(|p| 3.0 * ((p.x - 4.0).powi(2) + (p.y - 5.0).powi(2) + (p.z - 3.0).powi(2)).sqrt() - 12.0));
        let sharp = crate::program::Mix { field: crate::program::MixField::Fn(kernel(|p| (p.x + 0.37 * p.y - 0.21 * p.z) / 0.006)), per_mm: 0.001, slots: vec![3, 4], sharp: 1.0, balance: 0.0 };
        let soft = crate::program::Mix { field: crate::program::MixField::Fn(kernel(|p| (p.y * 400.0).sin() * 0.5 + 0.5)), per_mm: 0.001, slots: vec![1, 2], sharp: 0.0, balance: 0.0 };
        let stroke = |a: P3, b: P3, r: f64| Region::Stroke { points: vec![a, b], radius: r };
        let paint = |slot: u8| Op::Paint { channel: Channel::Color, slot };
        let program = Program::new(vec![
            Layer { region: Region::Field { node: SdfNode::Transform { at: [2.0, 2.0, 2.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Sphere { radius: 3.0 }) }, grow: 0.0 }, op: Op::Mix { channel: Channel::Color, mix: std::sync::Arc::new(soft) } },
            Layer { region: Region::Field { node: steep, grow: 0.5 }, op: paint(1) },
            Layer { region: stroke([-2.0, 6.0, 4.0], [14.0, 2.0, 5.0], 1.7), op: paint(2) },
            Layer { region: Region::Field { node: SdfNode::Transform { at: [6.0, 9.0, 7.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Sphere { radius: 3.3 }) }, grow: 0.0 }, op: Op::Mix { channel: Channel::Color, mix: std::sync::Arc::new(sharp) } },
            Layer { region: stroke([8.0, 1.0, 1.0], [8.0, 12.0, 9.0], 0.9), op: Op::Erase { channel: Channel::Color } },
        ]);
        let cell = 0.4;
        let (min, dim) = Volume::lattice(([0.0; 3], [14.0, 12.0, 10.0]), cell);
        let (mut whole, mut held) = (0, 0);
        for z in 0..dim[2] {
            for y in 0..dim[1] {
                for x in 0..dim[0] {
                    let b = [min[0] + x, min[1] + y, min[2] + z];
                    let (once, alone) = (brick(&program, Channel::Color, cell, b), brick_alone(&program, Channel::Color, cell, b));
                    assert_eq!(once, alone, "brick {b:?}");
                    if once.1.is_some() { held += 1 } else { whole += 1 }
                }
            }
        }
        assert!(whole > 0 && held > 20, "{whole} whole bricks, {held} with samples");
    }

    /// A scripted region with no box, three times as steep as a distance: read as a distance,
    /// its edge reads three times as far off as it is, and the whole-brick test passes bricks
    /// it crosses. Measured over the part, every lattice point bakes the slot a point read of
    /// it gives.
    #[test]
    fn an_unboxed_steep_region_bakes_its_edge_where_a_point_reads_it() {
        use makepad_csg_sdf::{SdfKernel, SdfNode};
        let steep = SdfKernel { key: 7, field: std::sync::Arc::new(Field(|p| 3.0 * (((p.x - 7.0).powi(2) + (p.y - 6.0).powi(2) + (p.z - 5.0).powi(2)).sqrt() - 4.0))), splash: None, bounds: None, solid: None };
        let program = Program::new(vec![Layer { region: Region::Field { node: SdfNode::Kernel(steep), grow: 0.0 }, op: Op::Paint { channel: Channel::Color, slot: 1 } }]);
        assert!(program.has_unboxed_fields());
        let part = ([0.0; 3], [14.0, 12.0, 10.0]);
        let resolved = program.resolve_slopes(part, &[[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]]);
        // the lattice points whose baked slot is not the one a point read gives
        let wrong = |p: &Program| {
            let v = Volume::bake(p, Channel::Color, part, 0.4, None, &|len, f| (0..len).for_each(f)).unwrap();
            let mut n = 0;
            for z in v.min[2] * BS..(v.min[2] + v.dim[2]) * BS {
                for y in v.min[1] * BS..(v.min[1] + v.dim[1]) * BS {
                    for x in v.min[0] * BS..(v.min[0] + v.dim[0]) * BS {
                        let g = [x, y, z];
                        if read(&v, g).0 != p.sample(Channel::Color, g.map(|c| c as f64 * v.cell)).slot {
                            n += 1;
                        }
                    }
                }
            }
            n
        };
        assert!(wrong(&program) > 0, "read as a distance, the steep edge baked where a point reads it");
        assert_eq!(wrong(&resolved), 0, "measured over the part, a lattice point bakes the slot a point read gives");
        // and a program made from it reads it as steep: its support view, a fill on top
        assert_eq!(wrong(&resolved.shown(Channel::Color).expect("it paints")), 0, "its view read the region as a distance again");
        assert_eq!(wrong(&resolved.with_layers_on_top([])), 0, "a program with more on top read the region as a distance again");
    }

    /// A lattice point's material and its distance to the boundary as the volume holds them.
    pub(crate) fn read(v: &Volume, g: [i32; 3]) -> (u8, f64) {
        let b: [i32; 3] = std::array::from_fn(|k| g[k].div_euclid(BS) - v.min[k]);
        let e = v.index[((b[2] * v.dim[1] + b[1]) * v.dim[0] + b[0]) as usize];
        if e < 0.0 {
            return ((-1.0 - e) as u8, RAMP * v.cell);
        }
        let l: [i32; 3] = std::array::from_fn(|k| g[k].rem_euclid(BS));
        let (slot, over) = sample_parts(v.samples[e as usize * 512 + (l[0] + (l[1] + l[2] * BS) * BS) as usize]);
        (slot, over * RAMP * v.cell)
    }

    /// A sharp mix's boundary bakes at its place: a field gives no distance, so each lattice
    /// point near it holds the distance its neighbours find (here to the plane x + 0.37 y = 5 mm),
    /// and the view draws the boundary there, a pixel wide, not a cell-wide blend of the two.
    #[test]
    fn a_sharp_mix_bakes_the_distance_to_its_boundary() {
        let field = makepad_csg_sdf::SdfKernel { key: 1, field: std::sync::Arc::new(Field(|p| (p.x + 0.37 * p.y) / 0.01)), splash: None, bounds: None, solid: None };
        let mix = crate::program::Mix { field: crate::program::MixField::Fn(field), per_mm: 0.001, slots: vec![1, 2], sharp: 1.0, balance: 0.0 };
        let p = Program::new(vec![Layer { region: Region::All, op: Op::Mix { channel: Channel::Color, mix: std::sync::Arc::new(mix) } }]);
        let v = Volume::bake(&p, Channel::Color, ([0.0; 3], [10.0; 3]), 0.4, None, &|len, f| (0..len).for_each(f)).unwrap();
        let (mut near, mut worst) = (0, 0.0f64);
        for x in 0..25 {
            for y in 0..25 {
                let g = [x, y, 7];
                let signed = (x as f64 + 0.37 * y as f64) * v.cell - 5.0;
                let d = signed.abs() / (1.0 + 0.37f64 * 0.37).sqrt();
                let (slot, dist) = read(&v, g);
                if signed.abs() > 1e-6 {
                    assert_eq!(slot, if signed < 0.0 { 1 } else { 2 }, "the material at {g:?}");
                }
                if d < 0.3 * v.cell {
                    near += 1;
                    worst = worst.max((dist - d).abs());
                }
            }
        }
        assert!(near >= 10, "{near} points near the boundary");
        assert!(worst < 0.02, "the baked distance is off by {worst:.3} mm");
    }

    /// An overlay goes after what is packed, each frame its own header over one copy of its data:
    /// a header's bases find the values and the palette, a node with no value reads negative, and
    /// a lane's frame is where `paint_frame` reads it.
    #[test]
    fn an_overlay_packs_after_the_paint_and_its_frames_share_its_data() {
        let id = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
        let turned = [[0.0, 2.0, 0.0, 5.0], [0.0, 0.0, 2.0, 0.0], [2.0, 0.0, 0.0, 0.0]];
        let mut a = Atlas { data: vec![7.0; 8 * 4], headers: Vec::new() };
        let palette = vec![[0.5, 0.5, 0.5, 0.0]; 38];
        let values: Vec<f32> = (0..8).map(|i| if i == 5 { -1.0 } else { i as f32 * 0.1 }).collect();
        let lattice = OverlayLattice { origin: [1.0, 2.0, 3.0], h: 0.5, cells: [1, 1, 1], data: &values };
        let overlay = Overlay { values: Some(lattice), marks: None, palette: &palette };
        let frame = OverlayFrame { under: Some(texel_number(16)), lane: turned, lattice: id, paint: id };
        let h: Vec<usize> = a.overlay(8, 8, &overlay, &[frame, OverlayFrame { under: None, ..frame }]).into_iter().map(|h| texel_at(h).unwrap()).collect();
        assert_eq!(h, vec![16, 30], "after the paint's 8 texels, from texel 8");
        assert_eq!(a.data.len() % (8 * 4), 0, "whole rows");
        assert_eq!(a.data.len() / 4 - 8, Atlas::overlay_texels(8, 2, palette.len(), values.len(), 0), "the size an atlas is sized by before it packs");
        let texel = |i: usize| -> [f32; 4] { std::array::from_fn(|c| a.data[(i - 8) * 4 + c]) };
        let (one, two) = (texel(h[0]), texel(h[1]));
        assert_eq!((one[0], texel_at(one[1]), one[3]), (-1.0, Some(16), -1.0), "its kind, the paint under it, no marks");
        assert_eq!((two[1], two[2]), (-1.0, one[2]), "no paint under the second; the same values");
        assert_eq!(texel(h[0] + 3), turned[0], "the lane's rows where `paint_frame` reads them");
        assert_eq!(texel(h[0] + 1), [1.0, 2.0, 3.0, 0.5]);
        let dim = texel(h[0] + 2);
        assert_eq!(&dim[..3], &[1.0, 1.0, 1.0]);
        assert_eq!(texel(texel_at(dim[3]).unwrap()), palette[0], "the palette's base");
        assert_eq!(texel(texel_at(one[2]).unwrap() + 1), [0.4, -1.0, 0.6, 0.7], "node 5 has no value");
    }
}
