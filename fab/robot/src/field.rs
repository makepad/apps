//! The as-built field: a sparse grid of signed distances (mm, negative inside), clamped to a
//! narrow band around the surface, in bricks of 8³ samples (the SDF engine's lattice: sample
//! `g` sits at `g * cell`, brick `b` holds samples `8b .. 8b + 7` per axis). Each sample also
//! carries how sure the field is of it (0: never seen).
//!
//! The simulator keeps the true part in one (exact bead and cut shapes go in by min and max);
//! the planner keeps its estimate in another, built only from what the sensors report
//! (height scans fuse in as columns). A brick that lies wholly inside collapses to `Solid`
//! (no samples), one wholly outside is never stored, so memory follows the surface.

use crate::math::*;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// Samples per brick edge.
pub const BRICK: i32 = 8;
const N: usize = (BRICK * BRICK * BRICK) as usize;
/// The band, in cells: values are clamped to ±band.
pub const BAND_CELLS: f64 = 4.0;

/// A fixed hasher (FNV-1a): the same bricks in the same order on every run.
#[derive(Default)]
pub struct Fnv(u64);

impl Hasher for Fnv {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        let mut h = if self.0 == 0 { 0xcbf2_9ce4_8422_2325 } else { self.0 };
        for b in bytes {
            h = (h ^ *b as u64).wrapping_mul(0x100_0000_01b3);
        }
        self.0 = h;
    }
}

type Map<K, V> = HashMap<K, V, BuildHasherDefault<Fnv>>;

#[derive(Clone)]
struct Data {
    d: [f32; N],
    seen: [u8; N],
}

#[derive(Clone)]
enum Brick {
    /// Wholly inside: every sample at −band, fully known.
    Solid,
    Data(Box<Data>),
}

/// One deposited bead: a flat rounded ribbon (`width` across, `height` up, round edges of
/// radius `height / 2`) along `a`..`b`, where `a` and `b` are on the surface it sits on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bead {
    pub a: V3,
    pub b: V3,
    pub width: f64,
    pub height: f64,
    /// The direction the bead grows in (the surface normal it was laid on; up for a layer).
    pub up: V3,
}

/// A volume the field takes in or gives up: its distance and its box.
pub trait Shape {
    fn distance(&self, p: V3) -> f64;
    fn reach(&self) -> (V3, V3);
}

impl Bead {
    /// The bead with its frame worked out, for many distance queries.
    pub fn shape(&self) -> BeadShape {
        let r = 0.5 * self.height;
        let lift = scale(self.up, r);
        let a = add(self.a, lift);
        let ab = sub(add(self.b, lift), a);
        let l = len(ab);
        let u = if l > 1e-9 { scale(ab, 1.0 / l) } else { col(&frame_z(self.up, [1.0, 0.0, 0.0]), 0) };
        let v = norm(cross(self.up, u));
        let w = cross(u, v);
        BeadShape { a, u, v, w, l, flat: 0.5 * (self.width - self.height).max(0.0), r, bead: *self }
    }
}

/// A [`Bead`] ready for distance queries.
#[derive(Clone, Copy, Debug)]
pub struct BeadShape {
    a: V3,
    u: V3,
    v: V3,
    w: V3,
    l: f64,
    flat: f64,
    r: f64,
    bead: Bead,
}

impl Shape for BeadShape {
    fn distance(&self, p: V3) -> f64 {
        let d = sub(p, self.a);
        let (x, y, z) = (dot(d, self.u), dot(d, self.v), dot(d, self.w));
        let dx = if x < 0.0 { x } else if x > self.l { x - self.l } else { 0.0 };
        let dy = (y.abs() - self.flat).max(0.0);
        (dx * dx + dy * dy + z * z).sqrt() - self.r
    }

    fn reach(&self) -> (V3, V3) {
        // The ribbon's box: half its width across, its height up, its round ends along.
        let b = &self.bead;
        let e: V3 = std::array::from_fn(|i| 0.5 * b.width * self.v[i].abs() + self.r * (self.u[i].abs() + self.w[i].abs()));
        let (a, c) = (add(b.a, scale(b.up, self.r)), add(b.b, scale(b.up, self.r)));
        (std::array::from_fn(|i| a[i].min(c[i]) - e[i]), std::array::from_fn(|i| a[i].max(c[i]) + e[i]))
    }
}

impl Shape for Bead {
    fn distance(&self, p: V3) -> f64 {
        self.shape().distance(p)
    }

    fn reach(&self) -> (V3, V3) {
        self.shape().reach()
    }
}

/// A cut: a cylinder of `radius` along the tool `axis` (unit, from the tip into the tool),
/// `length` long from its tip, swept from tip `a` to tip `b`. Everything inside is removed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cut {
    pub a: V3,
    pub b: V3,
    pub axis: V3,
    pub radius: f64,
    pub length: f64,
}

impl Shape for Cut {
    fn distance(&self, p: V3) -> f64 {
        let ax = self.axis;
        let off = |v: V3| sub(v, scale(ax, dot(v, ax)));
        // Across the axis: the distance to the swept segment, less the radius.
        let (pa, ab) = (off(sub(p, self.a)), off(sub(self.b, self.a)));
        let l2 = dot(ab, ab);
        let t = if l2 > 1e-18 { (dot(pa, ab) / l2).clamp(0.0, 1.0) } else { 0.0 };
        let across = len(sub(pa, scale(ab, t))) - self.radius;
        // Along it: from the tip (at that point of the sweep) to the length.
        let s = dot(sub(p, lerp(self.a, self.b, t)), ax);
        let along = (-s).max(s - self.length);
        let (ca, cl) = (across.max(0.0), along.max(0.0));
        (ca * ca + cl * cl).sqrt() + across.max(along).min(0.0)
    }

    fn reach(&self) -> (V3, V3) {
        let e = [self.a, self.b, add(self.a, scale(self.axis, self.length)), add(self.b, scale(self.axis, self.length))];
        let lo = std::array::from_fn(|i| e.iter().map(|p| p[i]).fold(f64::MAX, f64::min) - self.radius);
        let hi = std::array::from_fn(|i| e.iter().map(|p| p[i]).fold(f64::MIN, f64::max) + self.radius);
        (lo, hi)
    }
}

/// An oriented box: `half` extents along the columns of `r` round `centre`. What a laser cut
/// removes in one step: the kerf along the beam and the offcut beside it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slab {
    pub centre: V3,
    pub r: M3,
    pub half: V3,
}

impl Shape for Slab {
    fn distance(&self, p: V3) -> f64 {
        let l = mtv(&self.r, sub(p, self.centre));
        let q: V3 = std::array::from_fn(|i| l[i].abs() - self.half[i]);
        let o = len([q[0].max(0.0), q[1].max(0.0), q[2].max(0.0)]);
        o + q[0].max(q[1]).max(q[2]).min(0.0)
    }

    fn reach(&self) -> (V3, V3) {
        let e: V3 = std::array::from_fn(|i| (0..3).map(|k| (self.r[i][k] * self.half[k]).abs()).sum());
        (sub(self.centre, e), add(self.centre, e))
    }
}

/// The as-built field.
#[derive(Clone)]
pub struct BuildField {
    cell: f64,
    band: f64,
    bricks: Map<[i32; 3], Brick>,
    /// The box every brick ever stored lies in (mm): nothing lies outside it.
    extent: Option<(V3, V3)>,
    revision: u64,
}

impl std::fmt::Debug for BuildField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BuildField({} mm, {} bricks, rev {})", self.cell, self.bricks.len(), self.revision)
    }
}

fn floor_div(a: i32, b: i32) -> i32 {
    a.div_euclid(b)
}

impl BuildField {
    /// An empty field of `cell` mm samples (0.25 by default; 0.1 resolves a 0.1 mm arm).
    pub fn new(cell: f64) -> BuildField {
        BuildField { cell, band: BAND_CELLS * cell, bricks: Map::default(), extent: None, revision: 0 }
    }

    pub fn cell(&self) -> f64 {
        self.cell
    }
    pub fn band(&self) -> f64 {
        self.band
    }
    /// Bumped by every change.
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn brick_count(&self) -> usize {
        self.bricks.len()
    }
    pub fn is_empty(&self) -> bool {
        self.bricks.is_empty()
    }

    fn split(g: [i32; 3]) -> ([i32; 3], usize) {
        let b = [floor_div(g[0], BRICK), floor_div(g[1], BRICK), floor_div(g[2], BRICK)];
        let l = [g[0] - b[0] * BRICK, g[1] - b[1] * BRICK, g[2] - b[2] * BRICK];
        (b, ((l[2] * BRICK + l[1]) * BRICK + l[0]) as usize)
    }

    /// Sample `g` and its confidence (0..255).
    fn sample(&self, g: [i32; 3]) -> (f32, u8) {
        let (b, i) = Self::split(g);
        match self.bricks.get(&b) {
            None => (self.band as f32, 0),
            Some(Brick::Solid) => (-self.band as f32, 255),
            Some(Brick::Data(d)) => (d.d[i], d.seen[i]),
        }
    }

    /// A lower bound on the distance from `p` to the material (mm), at a body's scale: the
    /// field's own value inside its band; beyond it ([`BuildField::distance`] only says "at
    /// least the band"), the larger of the band and the way to the box every brick lies in.
    pub fn clearance(&self, p: V3) -> f64 {
        let d = self.distance(p);
        if d < self.band {
            return d;
        }
        match self.extent {
            None => f64::INFINITY,
            Some((lo, hi)) => {
                let o: V3 = std::array::from_fn(|k| (lo[k] - p[k]).max(p[k] - hi[k]).max(0.0));
                self.band.max(len(o))
            }
        }
    }

    /// The distance at `p` (trilinear between samples).
    pub fn distance(&self, p: V3) -> f64 {
        let u = [p[0] / self.cell, p[1] / self.cell, p[2] / self.cell];
        let g = [u[0].floor() as i32, u[1].floor() as i32, u[2].floor() as i32];
        let f = [u[0] - g[0] as f64, u[1] - g[1] as f64, u[2] - g[2] as f64];
        // One brick holds all eight corners in most lookups.
        let (b, _) = Self::split(g);
        let inside = (0..3).all(|k| g[k] - b[k] * BRICK < BRICK - 1);
        let mut c = [0.0f64; 8];
        match (inside, self.bricks.get(&b)) {
            (true, None) => return self.band,
            (true, Some(Brick::Solid)) => return -self.band,
            (true, Some(Brick::Data(d))) => {
                for (k, v) in c.iter_mut().enumerate() {
                    let o = [(k & 1) as i32, ((k >> 1) & 1) as i32, ((k >> 2) & 1) as i32];
                    let (_, i) = Self::split([g[0] + o[0], g[1] + o[1], g[2] + o[2]]);
                    *v = d.d[i] as f64;
                }
            }
            (false, _) => {
                for (k, v) in c.iter_mut().enumerate() {
                    let o = [(k & 1) as i32, ((k >> 1) & 1) as i32, ((k >> 2) & 1) as i32];
                    *v = self.sample([g[0] + o[0], g[1] + o[1], g[2] + o[2]]).0 as f64;
                }
            }
        }
        let lx = |a: f64, b: f64| a + (b - a) * f[0];
        let y0 = lx(c[0], c[1]) + (lx(c[2], c[3]) - lx(c[0], c[1])) * f[1];
        let y1 = lx(c[4], c[5]) + (lx(c[6], c[7]) - lx(c[4], c[5])) * f[1];
        y0 + (y1 - y0) * f[2]
    }

    /// How sure the field is at `p` (0..1, the nearest sample's).
    pub fn confidence(&self, p: V3) -> f64 {
        let g = [(p[0] / self.cell).round() as i32, (p[1] / self.cell).round() as i32, (p[2] / self.cell).round() as i32];
        self.sample(g).1 as f64 / 255.0
    }

    /// The gradient at `p` (central differences over a cell).
    pub fn gradient(&self, p: V3) -> V3 {
        let h = self.cell;
        let d = |k: usize| {
            let mut a = p;
            let mut b = p;
            a[k] += h;
            b[k] -= h;
            (self.distance(a) - self.distance(b)) / (2.0 * h)
        };
        [d(0), d(1), d(2)]
    }

    /// Apply `f(position, old) -> new` (distance, confidence) to every sample in the box, in
    /// bricks that `near(brick centre, half diagonal)` says can change.
    fn update(&mut self, lo: V3, hi: V3, near: &dyn Fn(V3, f64) -> bool, f: &dyn Fn(V3, f32, u8) -> (f32, u8), solid_stays: bool) {
        let c = self.cell;
        let g0 = [(lo[0] / c).floor() as i32, (lo[1] / c).floor() as i32, (lo[2] / c).floor() as i32];
        let g1 = [(hi[0] / c).ceil() as i32, (hi[1] / c).ceil() as i32, (hi[2] / c).ceil() as i32];
        let b0 = [floor_div(g0[0], BRICK), floor_div(g0[1], BRICK), floor_div(g0[2], BRICK)];
        let b1 = [floor_div(g1[0], BRICK), floor_div(g1[1], BRICK), floor_div(g1[2], BRICK)];
        let half_diag = 0.5 * (3.0f64).sqrt() * BRICK as f64 * c;
        let band = self.band as f32;
        for bz in b0[2]..=b1[2] {
            for by in b0[1]..=b1[1] {
                for bx in b0[0]..=b1[0] {
                    let key = [bx, by, bz];
                    let centre = [(bx as f64 + 0.5) * BRICK as f64 * c - 0.5 * c, (by as f64 + 0.5) * BRICK as f64 * c - 0.5 * c, (bz as f64 + 0.5) * BRICK as f64 * c - 0.5 * c];
                    if !near(centre, half_diag) {
                        continue;
                    }
                    let entry = self.bricks.get_mut(&key);
                    let data = match entry {
                        Some(Brick::Solid) if solid_stays => continue,
                        Some(Brick::Solid) => {
                            let d = Box::new(Data { d: [-band; N], seen: [255; N] });
                            self.bricks.insert(key, Brick::Data(d));
                            match self.bricks.get_mut(&key) {
                                Some(Brick::Data(d)) => d,
                                _ => unreachable!(),
                            }
                        }
                        Some(Brick::Data(d)) => d,
                        None => {
                            let size = BRICK as f64 * c;
                            let (blo, bhi) = (key.map(|b| b as f64 * size), key.map(|b| (b + 1) as f64 * size));
                            self.extent = Some(match self.extent {
                                None => (blo, bhi),
                                Some((lo, hi)) => (std::array::from_fn(|k| lo[k].min(blo[k])), std::array::from_fn(|k| hi[k].max(bhi[k]))),
                            });
                            self.bricks.insert(key, Brick::Data(Box::new(Data { d: [band; N], seen: [0; N] })));
                            match self.bricks.get_mut(&key) {
                                Some(Brick::Data(d)) => d,
                                _ => unreachable!(),
                            }
                        }
                    };
                    for lz in 0..BRICK {
                        let gz = bz * BRICK + lz;
                        if gz < g0[2] || gz > g1[2] {
                            continue;
                        }
                        for ly in 0..BRICK {
                            let gy = by * BRICK + ly;
                            if gy < g0[1] || gy > g1[1] {
                                continue;
                            }
                            for lx in 0..BRICK {
                                let gx = bx * BRICK + lx;
                                if gx < g0[0] || gx > g1[0] {
                                    continue;
                                }
                                let i = ((lz * BRICK + ly) * BRICK + lx) as usize;
                                let p = [gx as f64 * c, gy as f64 * c, gz as f64 * c];
                                let (d, s) = f(p, data.d[i], data.seen[i]);
                                data.d[i] = d.clamp(-band, band);
                                data.seen[i] = s;
                            }
                        }
                    }
                }
            }
        }
        self.revision += 1;
    }

    /// Union a volume in (`seen`: how sure, 0..255; the simulator's truth is sure).
    pub fn add(&mut self, shape: &dyn Shape, seen: u8) {
        let (mut lo, mut hi) = shape.reach();
        for k in 0..3 {
            lo[k] -= self.band;
            hi[k] += self.band;
        }
        let band = self.band;
        self.update(lo, hi, &|c, r| shape.distance(c) < band + r, &|p, d, s| {
            let b = shape.distance(p) as f32;
            if b < d {
                (b, s.max(seen))
            } else {
                (d, s)
            }
        }, true);
    }

    /// Take a volume out.
    pub fn remove(&mut self, shape: &dyn Shape, seen: u8) {
        let (mut lo, mut hi) = shape.reach();
        for k in 0..3 {
            lo[k] -= self.band;
            hi[k] += self.band;
        }
        let band = self.band;
        self.update(lo, hi, &|c, r| shape.distance(c) < band + r, &|p, d, s| {
            let v = -(shape.distance(p) as f32);
            if v > d {
                (v, s.max(seen))
            } else {
                (d, s)
            }
        }, false);
    }

    /// A height seen from above: the surface at `top` over the disc of `radius` round (x, y).
    /// Every sample of those columns within the band takes its height above the surface; the
    /// column below is solid down to where it already was (what the scanner cannot see under a
    /// surface is taken as built: the cell builds upward). `seen` is the observation's
    /// confidence (0..255).
    pub fn fuse_height(&mut self, x: f64, y: f64, top: f64, radius: f64, seen: u8) {
        let band = self.band;
        // Down to the plate, or as deep as one layer could have grown since.
        let floor = (top - band - 4.0).max(-band);
        let lo = [x - radius, y - radius, floor];
        let hi = [x + radius, y + radius, top + band];
        let r2 = radius * radius;
        self.update(lo, hi, &|_, _| true, &|p, d, s| {
            let (dx, dy) = (p[0] - x, p[1] - y);
            if dx * dx + dy * dy > r2 {
                return (d, s);
            }
            let v = (p[2] - top) as f32;
            if v >= -(band as f32) {
                (v, seen)
            } else {
                (d.min(-band as f32), s.max(seen))
            }
        }, true);
    }

    /// The surface below `from` (straight down), within `depth` mm: its height.
    pub fn top_below(&self, from: V3, depth: f64) -> Option<f64> {
        let mut z = from[2];
        let end = from[2] - depth;
        let step_min = 0.25 * self.cell;
        let mut prev = self.distance([from[0], from[1], z]);
        if prev <= 0.0 {
            return None;
        }
        while z > end {
            let step = (prev * 0.9).max(step_min);
            let nz = z - step;
            let d = self.distance([from[0], from[1], nz]);
            if d <= 0.0 {
                // Refine between z (outside) and nz (inside).
                let (mut a, mut b) = (nz, z);
                for _ in 0..20 {
                    let m = 0.5 * (a + b);
                    if self.distance([from[0], from[1], m]) <= 0.0 {
                        a = m;
                    } else {
                        b = m;
                    }
                }
                return Some(0.5 * (a + b));
            }
            prev = d;
            z = nz;
        }
        None
    }

    /// Points on the surface (samples within a cell of it, moved onto it along the gradient),
    /// each with its confidence (0..1), in a fixed order.
    pub fn surface_points(&self, out: &mut Vec<(V3, f64)>) {
        let mut keys: Vec<&[i32; 3]> = self.bricks.keys().collect();
        keys.sort_unstable();
        let c = self.cell;
        let near = 0.87 * c;
        for key in keys {
            let Some(Brick::Data(d)) = self.bricks.get(key) else { continue };
            for i in 0..N {
                let v = d.d[i] as f64;
                if v.abs() > near {
                    continue;
                }
                let l = i as i32;
                let g = [key[0] * BRICK + l % BRICK, key[1] * BRICK + (l / BRICK) % BRICK, key[2] * BRICK + l / (BRICK * BRICK)];
                // Two Newton steps along the gradient (one lands short near a cut's kink).
                let mut p = [g[0] as f64 * c, g[1] as f64 * c, g[2] as f64 * c];
                let mut v = v;
                for _ in 0..2 {
                    p = sub(p, scale(norm(self.gradient(p)), v));
                    v = self.distance(p);
                }
                out.push((p, d.seen[i] as f64 / 255.0));
            }
        }
    }

    /// The box of everything stored (mm), if anything is.
    pub fn bounds(&self) -> Option<(V3, V3)> {
        let mut lo = [i32::MAX; 3];
        let mut hi = [i32::MIN; 3];
        for k in self.bricks.keys() {
            for i in 0..3 {
                lo[i] = lo[i].min(k[i]);
                hi[i] = hi[i].max(k[i]);
            }
        }
        if self.bricks.is_empty() {
            return None;
        }
        let s = BRICK as f64 * self.cell;
        Some((std::array::from_fn(|i| lo[i] as f64 * s), std::array::from_fn(|i| (hi[i] + 1) as f64 * s)))
    }

    /// Collapse bricks that lie wholly inside (every sample two cells or more in) to `Solid`,
    /// and drop those wholly outside.
    pub fn compact(&mut self) {
        let deep = -(2.0 * self.cell) as f32;
        let band = self.band as f32;
        let mut solid = Vec::new();
        let mut empty = Vec::new();
        for (k, b) in &self.bricks {
            if let Brick::Data(d) = b {
                if d.d.iter().all(|v| *v <= deep) {
                    solid.push(*k);
                } else if d.d.iter().all(|v| *v >= band) && d.seen.iter().all(|s| *s == 0) {
                    empty.push(*k);
                }
            }
        }
        for k in solid {
            self.bricks.insert(k, Brick::Solid);
        }
        for k in empty {
            self.bricks.remove(&k);
        }
    }
}
