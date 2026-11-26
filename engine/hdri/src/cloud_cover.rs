//! Cover as a measured share of sky (design §3.1).
//!
//! Each modern recipe gives a noise n in [0, 1]. Its threshold for a cover c is read off a table of n's quantiles,
//! measured once per recipe shape and whole octave count over MEASURE_POINTS fixed points at MEASURE_SEED, so the share
//! of sky with cloud follows c whatever the recipe's own spread, and a Cover drag, Dice or a sun move measures nothing.
//!
//! - **The remap.** With the edge width w = 0.02 + 0.38 softness, a(c) = q(1 - c) - w c and density = smoothstep(a,
//!   a + w, n): closed at c = 1 (a = -w), and the share with any cloud at least c.
//! - **The tail.** A 4096-point table pins no quantile past its 64 highest points: with the table's line up to q(1) = 1
//!   the share drawn was nothing at c = 1e-4 and 1e-3 and a tenth of c at 1e-2 (measured). So below TAIL_SHARE (1/64)
//!   the threshold holds at a(TAIL_SHARE), and a TailGate thins the clouds by lattice points whose on-share is set from
//!   c alone: the share with any cloud stays about c (1.3 to 1.9 c measured for four recipes at 1e-4, 1e-3 and 1e-2),
//!   and at c = 0 nothing is on.
//! - **The cache.** Sets are kept process-wide by fingerprint: every set a live Env holds, and the 256 newest of the
//!   rest; past that the oldest of those go, TRIM_BATCH at a time, and the log says so. The lock is taken to look up or
//!   to insert, never while a table is measured (NOISE's pattern, volumetric.rs:381-400).

use makepad_draw::*;
use crate::noise::{hash01, hash_u32};
use crate::CloudGenus;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// 64 x 64 cells, one point each at a fixed jitter.
pub const MEASURE_POINTS: usize = 4096;
/// The seed every table is measured at, so a table depends on its recipe's shape alone: Dice measures nothing.
pub const MEASURE_SEED: u32 = 0x5eed_c10d;
/// The cover at and above which the threshold is the table's: 64 of the 4096 points stand above it.
pub const TAIL_SHARE: f32 = 1.0 / 64.0;
/// The tail gate's lattice spacing, in the recipe's cells, and each lattice point's fade-in as a share of the gate's
/// range.
const GATE_CELLS: f32 = 4.0;
const GATE_FADE: f32 = 0.15;
/// Sets no Env holds that the cache keeps (the newest), and how many go at once past that.
const KEEP_UNHELD: usize = 256;
const TRIM_BATCH: usize = 32;
/// The most whole octave counts a set measures (noise.rs's FBM_MAX_OCTAVES).
const MAX_OCTAVES: usize = 12;

/// q[k] is the k/64 quantile of a recipe's noise over the measurement's points: the (64 k)th smallest for k in 1..=63,
/// and the ends pinned to the noise's bounds, q[0] = 0 and q[64] = 1.
#[derive(Clone, Debug, PartialEq)]
pub struct Quantiles(pub [f32; 65]);

impl Quantiles {
    /// The table of `n` (x and y in the recipe's cells; held to [0, 1], NaN as 0) over the measurement's points.
    pub fn measure(n: &dyn Fn(f32, f32) -> f32) -> Quantiles {
        // The jitter has a stream of its own. value2's lattice value at (i, j) is hash01(i, j, 0, seed), so a jitter
        // drawn from MEASURE_SEED would stand each point where its cell's own corner value puts it, near the low
        // corners and away from the high ones, and a recipe on that stream would measure about 0.02 low.
        let jitter = hash_u32(MEASURE_SEED ^ 0xbb67_ae85);
        let mut values = Vec::with_capacity(MEASURE_POINTS);
        for j in 0..64 {
            for i in 0..64 {
                let x = i as f32 + hash01(i, j, 0, jitter);
                let y = j as f32 + hash01(i, j, 1, jitter);
                let v = n(x, y);
                values.push(if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) });
            }
        }
        values.sort_by(|a, b| a.total_cmp(b));
        let mut q = [0.0f32; 65];
        for k in 1..64 {
            q[k] = values[64 * k];
        }
        q[64] = 1.0;
        Quantiles(q)
    }

    /// The quantile at u (held to [0, 1], NaN as 0), on the straight line between the table's entries: q[k] exactly at
    /// u = k / 64.
    pub fn at(&self, u: f32) -> f32 {
        let u = if u.is_nan() { 0.0 } else { u.clamp(0.0, 1.0) };
        if u >= 1.0 {
            return self.0[64];
        }
        let x = u * 64.0;
        let k = (x.floor() as usize).min(63);
        let (lo, hi) = (self.0[k], self.0[k + 1]);
        lo + (hi - lo) * (x - k as f32)
    }
}

/// One recipe shape's tables, one per whole octave count, each measured on first use.
pub struct QuantileSet {
    tables: [OnceLock<Quantiles>; MAX_OCTAVES],
}

impl QuantileSet {
    fn new() -> QuantileSet {
        QuantileSet { tables: std::array::from_fn(|_| OnceLock::new()) }
    }

    /// The table of `octaves` (held to 1..=12), measured through n(octaves, x, y) on first use: `n` is the recipe at
    /// MEASURE_SEED's streams, x and y in its cells. A thread that needs a table another is measuring waits for it.
    pub fn table(&self, octaves: u32, n: &dyn Fn(u32, f32, f32) -> f32) -> &Quantiles {
        let octaves = octaves.clamp(1, MAX_OCTAVES as u32);
        self.tables[octaves as usize - 1].get_or_init(|| {
            #[cfg(test)]
            count_table();
            Quantiles::measure(&|x, y| n(octaves, x, y))
        })
    }
}

/// The cache: (fingerprint, set), oldest first.
static SETS: Mutex<Vec<(u64, Arc<QuantileSet>)>> = Mutex::new(Vec::new());

/// The tables of the recipe shape `fingerprint`, shared process-wide (design §3.1): a Cover drag, Dice or a sun move
/// finds the same set and measures nothing. A lookup makes the set the newest. A poisoned lock is taken over: every
/// change to the list leaves it whole, and nothing that can panic runs under the lock.
pub fn quantile_set(fingerprint: u64) -> Arc<QuantileSet> {
    let (set, trimmed) = {
        let mut sets = SETS.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(i) = sets.iter().position(|(key, _)| *key == fingerprint) {
            let found = sets.remove(i);
            let set = found.1.clone();
            sets.push(found);
            return set;
        }
        let set = Arc::new(QuantileSet::new());
        sets.push((fingerprint, set.clone()));
        let dropped = trim(&mut sets);
        (set, (dropped > 0).then(|| (sets.len(), dropped)))
    };
    if let Some((kept, dropped)) = trimmed {
        log!("hdri clouds: quantile tables trimmed to {} ({} dropped)", kept, dropped);
    }
    set
}

/// Past KEEP_UNHELD + TRIM_BATCH sets that only the cache holds, the oldest TRIM_BATCH of them go (design §3.1), and
/// says how many went. A set an Env holds (two or more holders) is never dropped.
fn trim(sets: &mut Vec<(u64, Arc<QuantileSet>)>) -> usize {
    let unheld = sets.iter().filter(|(_, set)| Arc::strong_count(set) == 1).count();
    if unheld <= KEEP_UNHELD + TRIM_BATCH {
        return 0;
    }
    let mut dropped = 0usize;
    sets.retain(|(_, set)| {
        let goes = dropped < TRIM_BATCH && Arc::strong_count(set) == 1;
        dropped += goes as usize;
        !goes
    });
    dropped
}

/// FNV-1a over little-endian bytes.
fn fnv(h: u64, bytes: [u8; 4]) -> u64 {
    bytes.iter().fold(h, |h, &b| (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3))
}

/// A recipe shape's key (design §3.1): the genus, the recipe's part (a type's second noise, as the Nimbostratus'
/// scud), and the bits of the fields that change its noise's spread, in declaration order: never the cover, the seed,
/// the wind, the drift, the edge width or the light rows. The octave count indexes the set's tables.
pub fn fingerprint(genus: CloudGenus, part: u32, fields: &[f32]) -> u64 {
    let mut h = fnv(0xcbf2_9ce4_8422_2325, (genus as u32).to_le_bytes());
    h = fnv(h, part.to_le_bytes());
    for field in fields {
        h = fnv(h, field.to_bits().to_le_bytes());
    }
    h
}

/// The threshold a(c) = q(1 - c) - w c (design §3.1) for a cover c and an edge width w; below TAIL_SHARE it holds at
/// a(TAIL_SHARE) and the TailGate thins the clouds instead. NaN is cover 0.
pub fn cover_threshold(q: &Quantiles, c: f32, w: f32) -> f32 {
    let c = if c.is_nan() { TAIL_SHARE } else { c.clamp(TAIL_SHARE, 1.0) };
    q.at(1.0 - c) - w * c
}

/// The density at noise n for the threshold a and the edge width w: smoothstep(a, a + w, n), 1 for every n >= 0 at
/// c = 1 (a = -w).
pub fn cover_density(n: f32, a: f32, w: f32) -> f32 {
    smoothstep(a, a + w, n)
}

/// What thins a cover below TAIL_SHARE: lattice points GATE_CELLS apart, each switching on over GATE_FADE of the gate's
/// range as y passes its hashed value, blended over its cells by the quintic fade. y is set from c so that the share of
/// points with a lattice point on among their four corners is about c / TAIL_SHARE: 1 - (1 - y)^4 = c / TAIL_SHARE, the
/// corners being independent. So the share with any cloud follows c however thin the noise's own tail is; at
/// TAIL_SHARE every point is fully on (y = 1), and at 0 none is.
#[derive(Clone, Copy, Debug)]
pub struct TailGate {
    y: f32,
    seed: u32,
}

impl TailGate {
    /// The gate of cover c (NaN as 0) on the lattice of `seed` (the layer's own stream, so Dice moves where the first
    /// clouds stand). Built once per entry; powf is the build's, not a texel's.
    pub fn new(c: f32, seed: u32) -> TailGate {
        let r = if c.is_nan() { 0.0 } else { (c / TAIL_SHARE).clamp(0.0, 1.0) };
        let y = if r >= 1.0 { 1.0 } else { 1.0 - (1.0 - r).powf(0.25) };
        TailGate { y, seed: hash_u32(seed ^ 0x6a09_e667) }
    }

    /// At or above TAIL_SHARE: every point is on.
    pub fn is_open(&self) -> bool {
        self.y >= 1.0
    }

    /// The gate at a point of the recipe's plane (its cells), in [0, 1].
    pub fn at(&self, p_cells: Vec2f) -> f32 {
        if self.y >= 1.0 {
            return 1.0;
        }
        let (gx, gz) = (p_cells.x / GATE_CELLS, p_cells.y / GATE_CELLS);
        if !(self.y > 0.0) || !gx.is_finite() || !gz.is_finite() {
            return 0.0;
        }
        let (fx, fz) = (gx.floor(), gz.floor());
        let (tx, tz) = (fade(gx - fx), fade(gz - fz));
        let (ix, iz) = (fx as i32, fz as i32);
        let on = |i: i32, j: i32| {
            let u = hash01(i, j, 7, self.seed) * (1.0 - GATE_FADE);
            smoothstep(u, u + GATE_FADE, self.y)
        };
        let (ix1, iz1) = (ix.wrapping_add(1), iz.wrapping_add(1));
        let (on00, on10, on01, on11) = (on(ix, iz), on(ix1, iz), on(ix, iz1), on(ix1, iz1));
        let bottom = on00 + (on10 - on00) * tx;
        let top = on01 + (on11 - on01) * tx;
        bottom + (top - bottom) * tz
    }
}

/// One entry's cover remap (design §3.1): its recipe shape's tables, its cover and edge width, its tail gate, and the
/// threshold of each octave count it reads, worked out once (an atomic load a texel).
pub struct CoverRemap {
    set: Arc<QuantileSet>,
    c: f32,
    w: f32,
    gate: TailGate,
    thresholds: [OnceLock<f32>; MAX_OCTAVES],
}

impl CoverRemap {
    pub fn new(set: Arc<QuantileSet>, c: f32, w: f32, gate_seed: u32) -> CoverRemap {
        CoverRemap { set, c, w, gate: TailGate::new(c, gate_seed), thresholds: std::array::from_fn(|_| OnceLock::new()) }
    }

    /// a(c) at `octaves` (held to 1..=12): the set's table, measured through `n` (MEASURE_SEED's streams) on first use.
    pub fn threshold(&self, octaves: u32, n: &dyn Fn(u32, f32, f32) -> f32) -> f32 {
        let o = octaves.clamp(1, MAX_OCTAVES as u32);
        *self.thresholds[o as usize - 1].get_or_init(|| cover_threshold(self.set.table(o, n), self.c, self.w))
    }

    /// The density at noise n and threshold a, at the point p of the recipe's plane (cells: the gate's lattice).
    pub fn density(&self, n: f32, a: f32, p_cells: Vec2f) -> f32 {
        let d = cover_density(n, a, self.w);
        if d > 0.0 && !self.gate.is_open() {
            d * self.gate.at(p_cells)
        } else {
            d
        }
    }

    /// Its tail gate at the point p of the recipe's plane (cells): 1 where the gate is open (Cover at or over
    /// TAIL_SHARE), else the lattice points' on-share (a deck's density takes it alone, its cut set by the threshold).
    pub fn gate_at(&self, p_cells: Vec2f) -> f32 {
        if self.gate.is_open() { 1.0 } else { self.gate.at(p_cells) }
    }

    /// The entry's effective cover c.
    pub fn cover(&self) -> f32 {
        self.c
    }

    /// Its tail gate as the GPU export's uniforms take it (design §12.3): `gate` (y, the gate's seed's low and high 16
    /// bits, the edge width w) and `k_gate` (GATE_CELLS, GATE_FADE); the shader's gate mirrors `TailGate::at` and
    /// `density`.
    pub(crate) fn gpu_layout(&self, out: &mut crate::cloud_volume_gpu::UploadBuilder) {
        let seed = self.gate.seed;
        out.uniform("gate", [self.gate.y, (seed & 0xffff) as f32, (seed >> 16) as f32, self.w]);
        out.uniform("k_gate", [GATE_CELLS, GATE_FADE, 0.0, 0.0]);
    }
}

use crate::noise::fade;

use crate::patterns::smooth as smoothstep;

#[cfg(test)]
thread_local! {
    static TABLES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn count_table() {
    TABLES.with(|t| t.set(t.get() + 1));
}

/// Tables measured on this thread so far (the pattern of lib.rs's `probe` counters).
#[cfg(test)]
pub(crate) fn tables_measured_on_this_thread() -> usize {
    TABLES.with(|t| t.get())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noise::fbm2;

    /// Design §3.1: q(0) = 0 and q(1) = 1 whatever the noise, the table is the noise's quantiles between (fbm2 of six
    /// octaves against a 262144-point measurement at another seed, within 0.01), it never falls, and NaN or
    /// out-of-range noise is held to the bounds.
    #[test]
    fn the_quantiles_are_pinned_at_their_ends() {
        let flat = Quantiles::measure(&|_, _| 0.5);
        assert_eq!((flat.0[0], flat.0[64]), (0.0, 1.0));
        assert!(flat.0[1..64].iter().all(|&q| q == 0.5), "{:?}", flat.0);
        let q = Quantiles::measure(&|x, y| fbm2(x, y, 6, MEASURE_SEED));
        assert_eq!((q.0[0], q.0[64]), (0.0, 1.0));
        assert!(q.0.windows(2).all(|w| w[0] <= w[1]), "{:?}", q.0);
        assert_eq!((q.at(0.0), q.at(1.0), q.at(-3.0), q.at(f32::NAN)), (0.0, 1.0, 0.0, 0.0));
        for k in 0..=64 {
            assert_eq!(q.at(k as f32 / 64.0), q.0[k], "at({k}/64)");
        }
        let mut big: Vec<f32> = (0..512).flat_map(|j| (0..512).map(move |i| fbm2(i as f32 * 0.731 + 0.3, j as f32 * 0.731 + 0.6, 6, 77))).collect();
        big.sort_by(|a, b| a.total_cmp(b));
        for k in [8usize, 16, 32, 48, 56] {
            let want = big[big.len() * k / 64];
            assert!((q.0[k] - want).abs() < 0.01, "q[{k}] {} against {want}", q.0[k]);
        }
        let wild = Quantiles::measure(&|x, _| if x < 10.0 { f32::NAN } else if x < 30.0 { -3.0 } else { 7.0 });
        assert!(wild.0.iter().all(|q| (0.0..=1.0).contains(q)), "{:?}", wild.0);
    }

    /// Design §3.1: at c = 0 no point has cloud and at c = 1 every point is closed, whatever the noise; the density
    /// never falls as c rises, through the gate and past it; below TAIL_SHARE the gate switches on the share of points
    /// 1 - (1 - y / (1 - GATE_FADE))^4 its y promises (within 10 % over 262144 points one gate cell apart); from
    /// TAIL_SHARE on it is open.
    #[test]
    fn the_remap_is_empty_at_zero_and_closed_at_one() {
        let n = |o: u32, x: f32, y: f32| fbm2(x, y, o, MEASURE_SEED);
        let set = Arc::new(QuantileSet::new());
        let remap = |c: f32| {
            let r = CoverRemap::new(set.clone(), c, 0.21, 9);
            let a = r.threshold(6, &n);
            (r, a)
        };
        let (zero, a0) = remap(0.0);
        let (one, a1) = remap(1.0);
        for j in 0..64 {
            for i in 0..64 {
                let p = vec2f(i as f32 * 1.37 - 40.0, j as f32 * 0.91 - 20.0);
                for v in [0.0f32, 0.1, 0.5, 0.9, 1.0] {
                    assert_eq!(zero.density(v, a0, p), 0.0, "c 0, n {v} at {p:?}");
                    assert_eq!(one.density(v, a1, p), 1.0, "c 1, n {v} at {p:?}");
                }
            }
        }
        let covers = [0.0f32, 1.0e-4, 1.0e-3, 0.01, TAIL_SHARE, 0.05, 0.2, 0.5, 0.8, 0.99, 1.0];
        let remaps: Vec<(CoverRemap, f32)> = covers.iter().map(|&c| remap(c)).collect();
        for j in 0..48 {
            for i in 0..48 {
                let p = vec2f(i as f32 * 3.1 + 0.4, j as f32 * 2.3 + 0.7);
                let v = fbm2(p.x, p.y, 6, 3);
                let mut last = 0.0f32;
                for ((r, a), c) in remaps.iter().zip(covers) {
                    let d = r.density(v, *a, p);
                    assert!(d >= last, "the density fell to {d} at c {c} ({p:?}, n {v})");
                    last = d;
                }
            }
        }
        for c in [1.0e-4f32, 1.0e-3, 1.0e-2] {
            let gate = TailGate::new(c, 9);
            let on = (0..512)
                .flat_map(|j| (0..512).map(move |i| vec2f(i as f32 * GATE_CELLS + 1.3, j as f32 * GATE_CELLS + 2.1)))
                .filter(|&p| gate.at(p) > 0.0)
                .count() as f32
                / 262_144.0;
            let y = 1.0 - (1.0 - c / TAIL_SHARE).powf(0.25);
            let want = 1.0 - (1.0 - (y / (1.0 - GATE_FADE)).min(1.0)).powi(4);
            assert!((on / want - 1.0).abs() < 0.1, "c {c}: {on} of the points against {want}");
        }
        assert!(TailGate::new(TAIL_SHARE, 9).is_open() && TailGate::new(0.5, 9).is_open() && !TailGate::new(0.001, 9).is_open());
        assert_eq!(TailGate::new(0.0, 9).at(vec2f(3.3, 4.4)), 0.0);
        assert_eq!(TailGate::new(TAIL_SHARE, 9).at(vec2f(3.3, 4.4)), 1.0);
        // NaN is cover 0, threshold and all.
        let q = set.table(6, &n);
        assert_eq!(cover_threshold(q, f32::NAN, 0.21).to_bits(), cover_threshold(q, 0.0, 0.21).to_bits());
    }

    /// Design §3.1: a shape's set is found again by its fingerprint and each table is measured once, so a Cover drag
    /// measures nothing new and more cover lowers the thresholds; another shape is another set.
    #[test]
    fn a_cover_drag_measures_nothing_new() {
        let n = |o: u32, x: f32, y: f32| fbm2(x, y, o, MEASURE_SEED);
        let key = fingerprint(CloudGenus::Stratocumulus, 0x7e57_0001, &[0.5, 0.3]);
        let set = quantile_set(key);
        let before = tables_measured_on_this_thread();
        let mut thresholds = Vec::new();
        for c in [0.1f32, 0.35, 0.6, 0.85] {
            let again = quantile_set(key);
            assert!(Arc::ptr_eq(&set, &again), "the same shape finds the same set");
            let remap = CoverRemap::new(again, c, 0.21, 1);
            thresholds.push((remap.threshold(6, &n), remap.threshold(3, &n)));
        }
        assert_eq!(tables_measured_on_this_thread(), before + 2, "the six- and three-octave tables, once each");
        assert!(thresholds.windows(2).all(|w| w[1].0 < w[0].0 && w[1].1 < w[0].1), "more cover, lower thresholds: {thresholds:?}");
        let other = quantile_set(fingerprint(CloudGenus::Stratocumulus, 0x7e57_0001, &[0.5, 0.31]));
        assert!(!Arc::ptr_eq(&set, &other), "another shape");
        other.table(6, &n);
        assert_eq!(tables_measured_on_this_thread(), before + 3, "another shape measures its own");
    }

    /// Design §3.1: a set something holds is never trimmed, and an unheld one with more than the cache keeps after it
    /// is gone (the trim logs "quantile tables trimmed to ...").
    #[test]
    fn the_cache_keeps_what_a_stack_holds() {
        let held = quantile_set(fingerprint(CloudGenus::Cumulus, 0x7e57_0002, &[1.0]));
        let unheld = Arc::downgrade(&quantile_set(fingerprint(CloudGenus::Cumulus, 0x7e57_0003, &[1.0])));
        for i in 0..(KEEP_UNHELD + 2 * TRIM_BATCH) as u32 {
            drop(quantile_set(fingerprint(CloudGenus::Cumulus, 0x7e57_1000 + i, &[i as f32])));
        }
        let found = quantile_set(fingerprint(CloudGenus::Cumulus, 0x7e57_0002, &[1.0]));
        assert!(Arc::ptr_eq(&held, &found), "a held set is never trimmed");
        assert!(unheld.upgrade().is_none(), "an unheld set older than the newest the cache keeps is gone");
        // A panic while the lock is held costs the cache nothing: the held set is still found.
        let _ = std::panic::catch_unwind(|| {
            let _sets = SETS.lock();
            panic!("the quantile cache's lock, poisoned on purpose");
        });
        assert!(SETS.is_poisoned());
        let again = quantile_set(fingerprint(CloudGenus::Cumulus, 0x7e57_0002, &[1.0]));
        assert!(Arc::ptr_eq(&held, &again), "a poisoned lock keeps its sets");
        SETS.clear_poison();
    }
}
