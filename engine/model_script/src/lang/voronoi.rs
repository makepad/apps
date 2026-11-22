//! The cells of `m.voronoi`: a Voronoi diagram of seeds that lie in the skin, spaced over the skin.
//!
//! Cells of a lattice through the volume cut a thin skin anywhere: a cell whose seed lies off the
//! skin leaves a sliver of a section, and a section's size has little to do with the cell's. The
//! diagram of seeds in the skin has one even cell per seed, measured over the skin, whose faces
//! cross it nearly square however it curves.
//!
//! So the lowering walks the source's surface: samples a few to a cell, on the sheet the web's holes
//! run through ([`Spec::depth_of`]); seeds among them no closer than [`SPACING`], which leaves no
//! sample further than that from one; then a few steps of moving each seed to the middle of its
//! samples. A cell with no room for a hole across it (on a rod, a thin rib) stays solid, and one
//! that folds over a sharp edge has no pointed top. It reads the source's field a few times per
//! sample, which is what a live edit pays.
//!
//! All of it runs in the cells' own space, `q`: the part's points with the axis on Y, over the cell's
//! width and height, so a cell is about 1 across every way. [`Cells::table`] packs the seeds for the
//! kernels of `texture.splash`, per bucket of a grid the seeds near it.

use super::{csg_sdf, P3};
use makepad_csg_math::Vec3d;
use std::collections::HashMap;

/// Samples per cell width: enough for a cell's middle to be its middle.
const SAMPLING: f64 = 3.0;
/// The closest two seeds start (q): an even diagram of cells 1 wide after relaxing.
const SPACING: f64 = 0.88;
/// Bucket width of the seed table (q).
const BUCKET: f64 = 0.75;
/// A bucket grid of no more words than this: a larger part takes wider buckets.
const MAX_BUCKETS: f64 = 4_000_000.0;
/// Words of one seed in a list: x y z (q), and the height (kernel units) its hole ends at.
pub(super) const STRIDE: usize = 4;
/// A cell folds when a sample's normal turns more than 45 degrees from its seed's (the cosine squared).
const FOLDED: f64 = 0.5;
/// A list's word in its bucket is an f32, exact up to here: a longer table starts its lists on a
/// multiple, and the word counts those.
const EXACT_WORD: usize = 1 << 24;
/// Half a GB of lists: past this the buckets widen, until a bucket spans a list's reach both ways.
/// That lists each seed about seven times, and every list is longer to walk.
const WIDEN_WORDS: f64 = (1u64 << 27) as f64;
/// What a kernel's int can index, with room for the buckets and padding: lists past this widen
/// further, however long they get to walk.
const INDEX_WORDS: f64 = (1u64 << 30) as f64;

pub(super) struct Spec {
    pub size: f64,
    pub stretch: f64,
    /// The cells' tall axis (0 x, 1 y, 2 z).
    pub axis: u8,
    pub skin: f64,
    pub random: f64,
    pub salt: u64,
    /// The solid bands along the axis (from the part's lowest and highest point): no seeds there.
    pub base: f64,
    pub rim: f64,
    /// The thickest strut.
    pub strut: f64,
    /// The thinnest: a hole's top is under its cell's roof by half of it, so a thin strut's hole ends
    /// in its point, and a thicker strut cuts that point off a little.
    pub thinnest: f64,
    /// Rounding where struts meet.
    pub round: f64,
    /// The tan of the shallowest roof from the vertical; None: holes as their cells make them.
    pub tan: Option<f64>,
    /// Model units per kernel unit (a seed's height is kept in kernel units).
    pub unit: f64,
}

impl Spec {
    /// `p` with the axis on Y, a swap either way (the kernels' `vor_up`).
    fn up(&self, p: P3) -> P3 {
        match self.axis {
            0 => [p[1], p[0], p[2]],
            2 => [p[0], p[2], p[1]],
            _ => p,
        }
    }

    fn scale(&self) -> P3 {
        [self.size, self.size * self.stretch, self.size]
    }

    fn q_of(&self, p: P3) -> P3 {
        let (u, s) = (self.up(p), self.scale());
        [u[0] / s[0], u[1] / s[1], u[2] / s[2]]
    }

    fn p_of(&self, q: P3) -> P3 {
        let s = self.scale();
        self.up([q[0] * s[0], q[1] * s[1], q[2] * s[2]])
    }

    /// The most a distance in q shrinks to, per model unit.
    fn fastest(&self) -> f64 {
        1.0 / self.size
    }

    /// How far under the surface [`Spec::depth_of`] looks: a wall under twice this is one sheet.
    fn probe(&self) -> f64 {
        self.skin.max(0.75 * self.size)
    }

    /// How deep under the surface a seed sits, from the source's value `below` a probe under it:
    /// half a skin, or the middle of a wall of less than two probes, so one sheet of cells runs
    /// through it and its holes through the wall. The value is then that wall's far side:
    /// it reads `probe - thickness`.
    fn depth_of(&self, below: f64) -> f64 {
        let probe = self.probe();
        if below < -0.8 * probe {
            0.5 * self.skin
        } else {
            (0.5 * (probe - below)).clamp(0.0, probe)
        }
    }
}

fn mix64(x: u64) -> u64 {
    let x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    let x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

fn hash3(c: [i64; 3], salt: u64) -> u64 {
    let mut h = mix64(salt.wrapping_add(0x9e37_79b9_7f4a_7c15));
    for v in c {
        h = mix64(h ^ v as u64);
    }
    h
}

/// Three numbers in -0.5..0.5 from a hash.
fn jitter(h: u64) -> P3 {
    [h & 0xfffff, (h >> 20) & 0xfffff, (h >> 40) & 0xfffff].map(|v| v as f64 / 1048575.0 - 0.5)
}

fn dist2(a: P3, b: P3) -> f64 {
    (a[0] - b[0]) * (a[0] - b[0]) + (a[1] - b[1]) * (a[1] - b[1]) + (a[2] - b[2]) * (a[2] - b[2])
}

fn read(of: &csg_sdf::SdfNode, ps: &[P3], out: &mut Vec<f64>) {
    let vs: Vec<Vec3d> = ps.iter().map(|p| Vec3d::new(p[0], p[1], p[2])).collect();
    out.clear();
    out.resize(vs.len(), 0.0);
    csg_sdf::distance_many(of, &vs, out);
}

/// Points by the bucket they are in, for "who is near".
struct Near {
    width: f64,
    buckets: HashMap<[i32; 3], Vec<u32>>,
}

impl Near {
    fn new(width: f64) -> Near {
        Near { width, buckets: HashMap::new() }
    }

    fn bucket(&self, q: P3) -> [i32; 3] {
        q.map(|x| (x / self.width).floor() as i32)
    }

    fn add(&mut self, i: usize, q: P3) {
        let b = self.bucket(q);
        self.buckets.entry(b).or_default().push(i as u32);
    }

    /// Every point of the buckets `reach` round `q`'s.
    fn around(&self, q: P3, reach: i32, mut f: impl FnMut(usize)) {
        let b = self.bucket(q);
        for z in -reach..=reach {
            for y in -reach..=reach {
                for x in -reach..=reach {
                    if let Some(list) = self.buckets.get(&[b[0] + x, b[1] + y, b[2] + z]) {
                        for i in list {
                            f(*i as usize);
                        }
                    }
                }
            }
        }
    }
}

/// A point of the sheet the holes run through.
#[derive(Clone, Copy)]
struct Sample {
    q: P3,
    /// The surface point over it, and its normal there.
    surface: P3,
    normal: P3,
    /// How upright the surface is here, 0..1: how much of a cell's height it runs along the axis.
    upright: f64,
    /// Its turn to become a seed, from its place on the lattice of space.
    rank: u64,
}

/// Points on the source's surface, as samples of their sheet, and how far the surface spans along
/// the axis (model units): the source's own lowest and highest points.
fn sheet(of: &csg_sdf::SdfNode, bounds: (P3, P3), s: &Spec) -> (Vec<Sample>, (f64, f64)) {
    // the source's box in q
    let (a, b) = (s.q_of(bounds.0), s.q_of(bounds.1));
    let lo: P3 = std::array::from_fn(|i| a[i].min(b[i]));
    let span: P3 = std::array::from_fn(|i| a[i].max(b[i]) - lo[i]);
    // cells that can hold surface, halving down from a few thousand over the box
    let fine = 1.0 / SAMPLING;
    let mut level = 0;
    while (span[0] * span[1] * span[2]) / (fine * (1u64 << level) as f64).powi(3) > 4096.0 {
        level += 1;
    }
    let step0 = fine * (1u64 << level) as f64;
    // On one lattice through all of space, whatever the box: a small edit of the part keeps these
    // points where they are, and so its seeds. (Cells numbered from the box's corner would all
    // change with it, and the web would jump at every step of a drag.)
    let top: P3 = std::array::from_fn(|i| lo[i] + span[i]);
    let lo: P3 = lo.map(|x| (x / step0).floor() * step0);
    let n0: [i32; 3] = std::array::from_fn(|i| ((top[i] - lo[i]) / step0).ceil().max(1.0) as i32);
    let first = lo.map(|x| (x / fine).round() as i64);
    let lattice = |c: &[i32; 3]| -> [i64; 3] { std::array::from_fn(|i| first[i] + i64::from(c[i])) };
    let mut cells = Vec::with_capacity((n0[0] * n0[1] * n0[2]) as usize);
    for z in 0..n0[2] {
        for y in 0..n0[1] {
            for x in 0..n0[0] {
                cells.push([x, y, z]);
            }
        }
    }
    let scale = s.scale();
    let diagonal = (scale[0] * scale[0] + scale[1] * scale[1] + scale[2] * scale[2]).sqrt();
    let mut ps = Vec::new();
    let mut d = Vec::new();
    for l in (0..=level).rev() {
        let step = fine * (1u64 << l) as f64;
        ps.clear();
        ps.extend(cells.iter().map(|c| s.p_of(std::array::from_fn(|i| lo[i] + (c[i] as f64 + 0.5) * step))));
        read(of, &ps, &mut d);
        let reach = 0.5 * step * diagonal;
        let kept = cells.iter().zip(&d).filter(|(_, d)| d.abs() <= reach).map(|(c, _)| *c);
        if l == 0 {
            cells = kept.collect();
        } else {
            cells = kept.flat_map(|c| (0..8).map(move |k| [2 * c[0] + (k & 1), 2 * c[1] + ((k >> 1) & 1), 2 * c[2] + ((k >> 2) & 1)])).collect();
        }
    }
    // one jittered point per cell, walked onto the surface along its gradient
    let mut xs: Vec<P3> = cells
        .iter()
        .map(|c| {
            let j = jitter(hash3(lattice(c), s.salt));
            s.p_of(std::array::from_fn(|i| lo[i] + (c[i] as f64 + 0.5 + 0.7 * j[i]) * fine))
        })
        .collect();
    let e = 0.02 * s.size;
    const TETRA: [P3; 4] = [[1.0, 1.0, 1.0], [1.0, -1.0, -1.0], [-1.0, 1.0, -1.0], [-1.0, -1.0, 1.0]];
    let longest = fine * diagonal;
    let mut normals = vec![[0.0; 3]; xs.len()];
    let mut on = vec![false; xs.len()];
    for walk in 0..3 {
        ps.clear();
        for x in &xs {
            ps.extend(TETRA.iter().map(|k| [x[0] + e * k[0], x[1] + e * k[1], x[2] + e * k[2]]));
        }
        read(of, &ps, &mut d);
        for (i, x) in xs.iter_mut().enumerate() {
            let v = &d[4 * i..4 * i + 4];
            let value = 0.25 * v.iter().sum::<f64>();
            let g: P3 = std::array::from_fn(|a| (0..4).map(|t| TETRA[t][a] * v[t]).sum::<f64>() / (4.0 * e));
            let gg = g[0] * g[0] + g[1] * g[1] + g[2] * g[2];
            if gg < 0.01 {
                on[i] = false;
                continue;
            }
            if walk < 2 {
                let t = value / gg;
                let move_by = t.abs() * gg.sqrt();
                let t = if move_by > longest { t * longest / move_by } else { t };
                *x = std::array::from_fn(|a| x[a] - g[a] * t);
            } else {
                let l = gg.sqrt();
                normals[i] = g.map(|c| c / l);
                on[i] = value.abs() < 0.03 * s.size;
            }
        }
    }
    // the span along the axis, then each point down to its sheet
    let (mut low, mut high) = (f64::INFINITY, f64::NEG_INFINITY);
    let mut kept: Vec<(P3, P3, u64)> = Vec::with_capacity(xs.len());
    for (i, x) in xs.iter().enumerate() {
        if on[i] {
            let y = s.up(*x)[1];
            low = low.min(y);
            high = high.max(y);
            kept.push((*x, normals[i], mix64(hash3(lattice(&cells[i]), s.salt))));
        }
    }
    ps.clear();
    let probe = s.probe();
    ps.extend(kept.iter().map(|(x, n, _)| std::array::from_fn::<f64, 3, _>(|a| x[a] - n[a] * probe)));
    read(of, &ps, &mut d);
    let samples = kept
        .iter()
        .zip(&d)
        .map(|((x, n, rank), below)| {
            let depth = s.depth_of(*below);
            let ny = s.up(*n)[1];
            Sample { q: s.q_of(std::array::from_fn(|a| x[a] - n[a] * depth)), surface: s.q_of(*x), normal: *n, upright: (1.0 - ny * ny).max(0.0).sqrt(), rank: *rank }
        })
        .collect();
    (samples, (low, high))
}

/// Seeds no closer than [`SPACING`] among `samples`, taken by rank, so no sample stays further than
/// that from one.
fn thin_out(samples: &[Sample]) -> Vec<P3> {
    let mut order: Vec<(u64, u32)> = samples.iter().enumerate().map(|(i, p)| (p.rank, i as u32)).collect();
    order.sort_unstable();
    let mut near = Near::new(SPACING);
    let mut seeds: Vec<P3> = Vec::new();
    for (_, i) in order {
        let q = samples[i as usize].q;
        let mut free = true;
        near.around(q, 1, |j| free &= dist2(seeds[j], q) >= SPACING * SPACING);
        if free {
            near.add(seeds.len(), q);
            seeds.push(q);
        }
    }
    seeds
}

/// The nearest of `seeds` to `q` (through `near`, width 1), with its squared distance.
fn nearest(near: &Near, seeds: &[P3], q: P3) -> Option<(usize, f64)> {
    let mut best: Option<(usize, f64)> = None;
    for reach in [1, 3] {
        near.around(q, reach, |j| {
            let d = dist2(seeds[j], q);
            if best.map_or(true, |b| d < b.1) {
                best = Some((j, d));
            }
        });
        if best.is_some() {
            break;
        }
    }
    best
}

fn near_of(seeds: &[P3]) -> Near {
    let mut near = Near::new(1.0);
    for (i, q) in seeds.iter().enumerate() {
        near.add(i, *q);
    }
    near
}

/// Moves each seed to the sample nearest the middle of those it is the nearest of.
fn relax(seeds: &mut [P3], samples: &[P3]) {
    let near = near_of(seeds);
    let owner: Vec<Option<(usize, f64)>> = samples.iter().map(|q| nearest(&near, seeds, *q)).collect();
    let mut sum = vec![([0.0f64; 3], 0.0f64); seeds.len()];
    for (q, o) in samples.iter().zip(&owner) {
        if let Some((j, _)) = o {
            for a in 0..3 {
                sum[*j].0[a] += q[a];
            }
            sum[*j].1 += 1.0;
        }
    }
    let middle: Vec<P3> = sum.iter().map(|(s, n)| if *n > 0.0 { s.map(|x| x / n) } else { [f64::INFINITY; 3] }).collect();
    let mut closest = vec![f64::INFINITY; seeds.len()];
    for (q, o) in samples.iter().zip(&owner) {
        if let Some((j, _)) = o {
            let d = dist2(*q, middle[*j]);
            if d < closest[*j] {
                closest[*j] = d;
                seeds[*j] = *q;
            }
        }
    }
}

/// What a cell's samples say of it.
#[derive(Clone, Copy, Default)]
struct Shape {
    /// They spread less than two struts across their second-widest way: no room for a hole, which
    /// would cut a rod or a rib in two.
    no_room: bool,
    /// Their normals part over an edge. A pointed top cut from one seed's cone dips where the surface
    /// turns away from it, so its hole would have two tops and a loose tip of solid between.
    folded: bool,
}

const PAIRS: [(usize, usize); 6] = [(0, 0), (0, 1), (0, 2), (1, 1), (1, 2), (2, 2)];

fn shapes(seeds: &[P3], near: &Near, samples: &[Sample], s: &Spec) -> Vec<Shape> {
    let scale = s.scale();
    let mut n = vec![0.0f64; seeds.len()];
    let mut sum = vec![[0.0f64; 3]; seeds.len()];
    let mut outer = vec![[0.0f64; 6]; seeds.len()];
    let mut closest = vec![(f64::INFINITY, [0.0f64; 3]); seeds.len()];
    let owners: Vec<Option<usize>> = samples
        .iter()
        .map(|sample| {
            let (j, d) = nearest(near, seeds, sample.q)?;
            let p: P3 = std::array::from_fn(|a| sample.q[a] * scale[a]);
            n[j] += 1.0;
            for a in 0..3 {
                sum[j][a] += p[a];
            }
            for (k, (a, b)) in PAIRS.into_iter().enumerate() {
                outer[j][k] += p[a] * p[b];
            }
            if d < closest[j].0 {
                closest[j] = (d, sample.normal);
            }
            Some(j)
        })
        .collect();
    let mut out: Vec<Shape> = (0..seeds.len())
        .map(|j| {
            if n[j] < 1.0 {
                return Shape::default();
            }
            let c = sum[j].map(|x| x / n[j]);
            let m: [f64; 6] = std::array::from_fn(|k| outer[j][k] / n[j] - c[PAIRS[k].0] * c[PAIRS[k].1]);
            // a strip W wide spreads W / sqrt(12)
            Shape { no_room: (12.0 * second_eigen(m)).max(0.0).sqrt() < 2.0 * s.strut, folded: false }
        })
        .collect();
    // squared, so both sides of a wall agree
    for (sample, owner) in samples.iter().zip(&owners) {
        if let Some(j) = owner {
            let at = closest[*j].1;
            let along = sample.normal[0] * at[0] + sample.normal[1] * at[1] + sample.normal[2] * at[2];
            out[*j].folded |= along * along < FOLDED;
        }
    }
    out
}

/// The middle eigenvalue of the symmetric matrix xx xy xz yy yz zz.
fn second_eigen(m: [f64; 6]) -> f64 {
    let [a, b, c, d, e, f] = m;
    let p1 = b * b + c * c + e * e;
    let q = (a + d + f) / 3.0;
    let p2 = (a - q) * (a - q) + (d - q) * (d - q) + (f - q) * (f - q) + 2.0 * p1;
    if p2 <= 1e-18 {
        return q;
    }
    let p = (p2 / 6.0).sqrt();
    let det = (a - q) * ((d - q) * (f - q) - e * e) - b * (b * (f - q) - e * c) + c * (b * e - (d - q) * c);
    let phi = (det / (p * p * p) * 0.5).clamp(-1.0, 1.0).acos() / 3.0;
    let high = q + 2.0 * p * phi.cos();
    let low = q + 2.0 * p * (phi + 2.0 * std::f64::consts::FRAC_PI_3).cos();
    3.0 * q - high - low
}

/// A foam's seeds: a jittered lattice through the part's core, deep enough that their cells stay
/// out of the skin (they meet the skin's cells half way down from its sheet).
fn core_seeds(of: &csg_sdf::SdfNode, bounds: (P3, P3), s: &Spec) -> Vec<P3> {
    let (a, b) = (s.q_of(bounds.0), s.q_of(bounds.1));
    let lo: [i64; 3] = std::array::from_fn(|i| a[i].min(b[i]).floor() as i64);
    let hi: [i64; 3] = std::array::from_fn(|i| a[i].max(b[i]).ceil() as i64);
    let n: [i64; 3] = std::array::from_fn(|i| hi[i] - lo[i]);
    let deep = (1.5 * s.skin).max(SPACING * s.size);
    let (mut kept, mut seeds, mut d) = (Vec::new(), Vec::with_capacity((n[0] * n[1]) as usize), Vec::new());
    // a plane at a time: most of a large part's lattice is not kept
    for z in lo[2]..hi[2] {
        seeds.clear();
        for y in lo[1]..hi[1] {
            for x in lo[0]..hi[0] {
                let j = jitter(hash3([x, y, z], s.salt ^ 0x5eed));
                seeds.push([x as f64 + 0.5 + s.random * j[0], y as f64 + 0.5 + s.random * j[1], z as f64 + 0.5 + s.random * j[2]]);
            }
        }
        let ps: Vec<P3> = seeds.iter().map(|q| s.p_of(*q)).collect();
        read(of, &ps, &mut d);
        kept.extend(seeds.iter().zip(&d).filter(|(_, d)| **d < -deep).map(|(q, _)| *q));
    }
    kept
}

/// A part's cells.
pub(super) struct Cells {
    pub seeds: Vec<P3>,
    /// The height (kernel units) each seed's hole ends at: under its own cell when the cell is solid.
    tops: Vec<f64>,
    /// A foam's seeds in the core (with `seeds`, they make the foam's cells).
    pub core: Vec<P3>,
    /// The solid bands' edges along the axis (kernel units): past the part when none, by little: the
    /// kernel's smooth mins read them in f32.
    pub low: f64,
    pub high: f64,
    /// How far from a seed a point of the skin may lie (q), measured over its samples.
    pub farthest: f64,
}

/// How many steps of relaxing the seeds take: more for less randomness (`random` is 0..0.7).
fn relax_steps(random: f64) -> usize {
    (1.0 + 7.0 * (1.0 - random / 0.7)).round() as usize
}

pub(super) fn cells(of: &csg_sdf::SdfNode, bounds: (P3, P3), s: &Spec, foam: bool) -> Cells {
    let (mut samples, (low, high)) = sheet(of, bounds, s);
    let heights = s.scale()[1];
    let (low, high) = if samples.is_empty() { (0.0, 0.0) } else { (low, high) };
    let band_low = low + s.base;
    let band_high = high - s.rim;
    samples.retain(|p| p.q[1] >= band_low / heights - 0.5 / SAMPLING && p.q[1] <= band_high / heights + 0.5 / SAMPLING);
    // No seed where a wall runs up into a band within a part of a cell: its cell would be a sliver
    // above the band, not a hole. A sheet lying along the band's edge has no such cell.
    const CLEAR: f64 = 0.35;
    let seedable: Vec<Sample> = samples
        .iter()
        .filter(|p| {
            let clear = CLEAR * p.upright;
            (s.base <= 0.0 || p.q[1] >= band_low / heights + clear) && (s.rim <= 0.0 || p.q[1] <= band_high / heights - clear)
        })
        .copied()
        .collect();
    let places: Vec<P3> = seedable.iter().map(|p| p.q).collect();
    let mut seeds = thin_out(&seedable);
    for _ in 0..relax_steps(s.random) {
        relax(&mut seeds, &places);
    }
    let near = near_of(&seeds);
    let farthest = samples.iter().filter_map(|p| nearest(&near, &seeds, p.surface)).map(|(_, d)| d).fold(0.0, f64::max).sqrt();
    let shape = shapes(&seeds, &near, &seedable, s);
    // A hole's top: where the seed's vertical meets its cell's nearest roof, under it by half a strut.
    let (my, unit) = (1.0 / heights, s.unit);
    let high_k = if s.rim > 0.0 { band_high } else { high + 8.0 * heights };
    let m = [1.0 / s.size, my, 1.0 / s.size];
    let tops = seeds
        .iter()
        .enumerate()
        .map(|(i, c)| {
            if shape[i].no_room {
                return (low - 8.0 * heights) / unit;
            }
            if s.tan.is_none() || shape[i].folded {
                return (high + 8.0 * heights) / unit;
            }
            let mut rise = f64::INFINITY;
            near.around(*c, 3, |j| {
                if j != i {
                    let n: P3 = std::array::from_fn(|a| seeds[j][a] - c[a]);
                    if n[1] > 1e-6 {
                        let nn = n[0] * n[0] + n[1] * n[1] + n[2] * n[2];
                        let l = ((n[0] * m[0]).powi(2) + (n[1] * m[1]).powi(2) + (n[2] * m[2]).powi(2)).sqrt();
                        rise = rise.min((0.5 * nn - 0.5 * s.thinnest * l) / (my * n[1]));
                    }
                }
            });
            (c[1] * heights + rise.max(0.0)).min(high_k) / unit
        })
        .collect();
    let core = if foam { core_seeds(of, bounds, s) } else { Vec::new() };
    let clear = 4.0 * s.strut;
    Cells { seeds, tops, core, low: (if s.base > 0.0 { band_low } else { low - clear }) / unit, high: (if s.rim > 0.0 { band_high } else { high + clear }) / unit, farthest }
}

impl Cells {
    /// The reach of the web's faces, `r` (q): the kernel's face distance holds exactly up to half of
    /// how much nearer the nearest seed is than `r`, and past that it reads that. So `r` takes the
    /// surface's furthest point from a seed (the sheet is the skin's middle, or under the surface
    /// with the skin between) and clears a strut and its rounding.
    pub fn faces_reach(&self, s: &Spec) -> f64 {
        self.farthest + (s.strut + 2.0 * s.round) * s.fastest()
    }

    /// The web's faces: only the skin's seeds, so its holes open under the skin. A list holds the
    /// seeds `r` from any point of its bucket, and those whose faces can round what `r` leaves.
    pub fn faces_table(&self, s: &Spec) -> Vec<f32> {
        self.faces_table_within(s, EXACT_WORD, WIDEN_WORDS)
    }

    pub(super) fn faces_table_within(&self, s: &Spec, exact: usize, widen: f64) -> Vec<f32> {
        let r = self.faces_reach(s);
        table(&self.seeds, Some(&self.tops), r, r + 2.0 * s.round * s.fastest(), exact, widen)
    }

    /// The foam's cells, every seed in lists of [`Cells::edges_reach`].
    pub fn edges_table(&self, s: &Spec) -> Vec<f32> {
        let r = self.edges_reach(s);
        let all: Vec<P3> = self.seeds.iter().chain(&self.core).copied().collect();
        table(&all, None, r, r, EXACT_WORD, WIDEN_WORDS)
    }

    /// The reach of the foam's edges, `r` (q): an edge within reach of a point has its three seeds
    /// among the point's six nearest, and the seventh is about 1.2 off, so this keeps them in the
    /// lists wherever a strut stands.
    pub fn edges_reach(&self, s: &Spec) -> f64 {
        (self.farthest + (s.strut + 2.0 * s.round) * s.fastest()).max(1.75)
    }
}

/// The table the kernels read (`vor_list`): the frame (origin xyz, 1 / bucket, bucket counts xyz, the
/// seeds' reach `r`, the multiple lists start on), each bucket's word of its list (that many
/// multiples from the table's start), an empty list, and the lists: a count, then the seeds in one
/// order for every bucket, so a seed in two lists counts the same in both. `list` is at least `r`.
/// The grid's edge buckets are empty, and a point off the grid reads them.
///
/// A bucket's word is exact up to `exact`, and the lists' words widen the buckets past `widen`.
fn table(seeds: &[P3], tops: Option<&[f64]>, r: f64, list: f64, exact: usize, widen: f64) -> Vec<f32> {
    let reach = list;
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for q in seeds {
        for a in 0..3 {
            lo[a] = lo[a].min(q[a]);
            hi[a] = hi[a].max(q[a]);
        }
    }
    if seeds.is_empty() {
        (lo, hi) = ([0.0; 3], [0.0; 3]);
    }
    let mut width = BUCKET;
    let dims = |width: f64| -> [usize; 3] { std::array::from_fn(|a| ((hi[a] - lo[a] + 2.0 * reach) / width).ceil() as usize + 3) };
    // A seed is listed by the buckets it reaches: those in a bucket grown by `reach`, over a bucket.
    let listed = |width: f64| -> f64 {
        let x = reach / width;
        seeds.len() as f64 * STRIDE as f64 * (1.0 + 6.0 * x + std::f64::consts::PI * (3.0 * x * x + 4.0 / 3.0 * x * x * x))
    };
    let fits = |width: f64| -> bool {
        dims(width).iter().map(|n| *n as f64).product::<f64>() <= MAX_BUCKETS && listed(width) <= if width < 2.0 * reach { widen } else { INDEX_WORDS }
    };
    while !fits(width) && dims(width).iter().any(|n| *n > 4) {
        width *= 1.25;
    }
    let n = dims(width);
    let origin: P3 = std::array::from_fn(|a| lo[a] - reach - 1.5 * width);
    let total = n[0] * n[1] * n[2];
    let mut lists: HashMap<usize, Vec<u32>> = HashMap::new();
    for (i, q) in seeds.iter().enumerate() {
        let first: [usize; 3] = std::array::from_fn(|a| ((q[a] - reach - origin[a]) / width).floor() as usize);
        let last: [usize; 3] = std::array::from_fn(|a| ((q[a] + reach - origin[a]) / width).floor() as usize);
        for z in first[2]..=last[2] {
            for y in first[1]..=last[1] {
                for x in first[0]..=last[0] {
                    // how far the bucket's nearest point is from the seed
                    let c = [x, y, z];
                    let gap: f64 = (0..3)
                        .map(|a| {
                            let b0 = origin[a] + c[a] as f64 * width;
                            let g = (b0 - q[a]).max(q[a] - (b0 + width)).max(0.0);
                            g * g
                        })
                        .sum();
                    if gap <= reach * reach {
                        lists.entry(x + n[0] * (y + n[1] * z)).or_default().push(i as u32);
                    }
                }
            }
        }
    }
    let head = 9 + total;
    let in_lists = 1 + lists.values().map(|l| 1 + STRIDE * l.len()).sum::<usize>();
    let list_count = 1 + lists.len();
    let mut every = 1;
    while (head + in_lists + list_count * (every - 1)) / every > exact {
        every *= 2;
    }
    let words = head + in_lists + list_count * (every - 1);
    // a wrapped index would read another field, and that field would print
    assert!(words <= i32::MAX as usize, "{} seeds are more than a kernel's int can index", seeds.len());
    let mut w: Vec<f32> = Vec::with_capacity(words);
    w.extend(origin.map(|x| x as f32));
    w.push((1.0 / width) as f32);
    w.extend(n.map(|x| x as f32));
    w.push(r as f32);
    w.push(every as f32);
    w.resize(head, 0.0);
    // where the next list starts, on a multiple
    let start = |w: &mut Vec<f32>| -> f32 {
        w.resize(w.len().next_multiple_of(every), 0.0);
        (w.len() / every) as f32
    };
    let empty = start(&mut w);
    w[9..head].fill(empty);
    w.push(0.0);
    let mut keys: Vec<usize> = lists.keys().copied().collect();
    keys.sort_unstable();
    for k in keys {
        w[9 + k] = start(&mut w);
        let list = &lists[&k];
        w.push(list.len() as f32);
        for i in list {
            let q = seeds[*i as usize];
            w.extend(q.map(|x| x as f32));
            w.push(tops.map_or(0.0, |t| t[*i as usize]) as f32);
        }
    }
    w
}
