//! The sun's optical depth for the cloud volumes (design §12.1 item 5 and "Held shows the same clouds"), and the
//! optical depth straight up for their dome light (design §2.3).
//!
//! - **A hybrid** ([`SunDepthGrid::sun_tau`]). A cloud's sunlit skin is one or two mean free paths deep (17 m at a
//!   Cumulus's extinction), finer than any grid a bake can afford. So the depth near a point is marched live, in two
//!   short steps along the sun through the caller's density without detail, and the depth beyond them is read from a
//!   grid baked once a bake.
//! - **The read must not straddle a surface** (design §12.1 item 5's blocker; the sun light test). A grid's cells
//!   round the read point, q, that stand inside the cloud p is on would put that cloud's depth on p's lit skin; so
//!   would a lobe beside q, and the air in a lobe's shadow would darken p where the shadow does not reach. So each
//!   node knows whether it stands in cloud, and a read in air weighs only the air nodes round it (shadow maps'
//!   depth-aware filtering, our own form); a read deep in cloud weighs all. Where q ends in a cloud's soft edge, or
//!   in air beside a cloud (cloud nodes carrying more than BESIDE_SHARE of its weight), the near part takes more
//!   steps toward the sun, up to MORE_STEPS, before it reads.
//! - **The sun-depth grid** ([`SunDepthGrid`]): the sun's own frame. Rays along the sun (held at least LIGHT_MIN_SIN
//!   up, as the classic light grid) sit on a square lattice across it, and each ray keeps a node every so far along
//!   it. A node holds the optical depth from it toward the sun to the top of the shell, a running sum down its ray
//!   by the trapezoid rule at sub-samples about every SUB_SAMPLE_M (the coarse level's every SUB_SAMPLE_COARSE_M,
//!   design §12.6 item 5) that end on each node (so a node's own extinction is read and says whether it stands in
//!   cloud). A ray (or a tau_up column) whose path the caller's [`ClearSpan`] says meets no cloud is left at 0
//!   unread (design §12.6 item 6): the caller's test is exact, so the grid is the plain fill's bit for bit. A ray is summed through the margin toward the sun too, but keeps
//!   nodes only where the shell crosses the level's square round the viewer, so a low sun's margin costs fill time,
//!   not memory. Two levels: a fine one, cells the caller gives across (47 m x Size) and twice that along, out to a
//!   radius it gives (12-16 km); a coarse one, four cells both ways, out to COARSE_RADIUS_M. The fine one fades into
//!   the coarse over two coarse cells at its edge. Reads are trilinear: smooth weights across the sun's plane (v,
//!   horizontal), linear along the sun and up across it (s, w), which carry a uniform deck's depth exactly (smooth
//!   weights there would bend a gradient of up to 0.4 a metre at a low sun into steps).
//! - **tau_up** ([`TauUpGrid`]): an unleaned coarse grid, a vertical running sum, for the dome's attenuation.
//! - **Held** ([`SunDepthGrid::shared`] with `held`): only the coarse level is filled (the same law); the fine level
//!   is used when a released bake left it for the same key (a light-only drag), and is filled on release.
//! - **Caches**: levels and tau_up grids are kept process-wide by key (the sun, the caller's shape key, Cover, seed and
//!   the grid's own geometry; tau_up without the sun), what live maps hold plus the newest of the rest by their bytes
//!   (SUN_LEVEL_BUDGET and TAU_UP_BUDGET: about the last 14 Stratocumulus presets' light grids, down to about 4 of
//!   the largest), logged, never failing.
//!
//! Positions are metres round the viewer: x east, y up, z south, as the footprint grid's.

use makepad_draw::*;
use crate::caster::RowRunner;
use crate::cloud_volume::{kept_by_key, rows_by, shared_by_bytes, GridCache};
use crate::cloud_volume_light::POWDER_REACH_M;
use crate::volumetric::{Cancelled, LIGHT_MIN_SIN};
use std::sync::{Arc, Mutex};

/// The caller's extinction (1/m) at a point: its density without detail times its sigma. Zero outside the shell.
pub type Extinction<'a> = &'a (dyn Fn(Vec3f) -> f32 + Sync);
/// The caller's test that the straight path from one point to another meets no cloud: every extinction read on it is
/// 0 (the footprint's max pyramid gives it, design §12.6 item 6). It must be exact: a path it calls clear is left at 0
/// without a read, so a grid filled with it is the plain fill's bit for bit.
pub type ClearSpan<'a> = &'a (dyn Fn(Vec3f, Vec3f) -> bool + Sync);

/// The fine level's cell at Size 1 (design §12.1 item 5): the shape noise's own texel, 6 km / 128.
pub const SUN_CELL_M: f32 = 47.0;
/// How far the coarse level reaches round the viewer: the volumes' range.
pub const COARSE_RADIUS_M: f32 = 32_000.0;
/// The fine level's node spacing along the sun, in its cells across.
const FINE_ALONG: f32 = 2.0;
/// The coarse level's cell, across the sun and along it, in fine cells.
const COARSE_CELLS: f32 = 4.0;
/// The fine level's sub-samples along the sun are about this far apart (design §12.1 item 5); the coarse level's
/// about SUB_SAMPLE_COARSE_M (design §12.6 item 5: the same light on the test fields at 2.5 times fewer reads).
const SUB_SAMPLE_M: f32 = 20.0;
const SUB_SAMPLE_COARSE_M: f32 = 47.0;
/// The near part's two steps together are this many fine cells long (about 15 and 44 m at 47 m cells).
const NEAR_CELLS: f32 = 1.25;
/// A point counts as in cloud above this share of the body's extinction (`SunGridSpec::body_extinction`): about
/// 1e-3 a metre on a Cumulus, a faint fringe.
const FRINGE_SHARE: f32 = 0.05;
/// Deep in cloud: over this many times the body's extinction.
const DEEP_BODIES: f32 = 2.0;
/// While the near part ends in a cloud's soft edge (between the fringe and DEEP_BODIES bodies), or in air with cloud
/// nodes carrying more than BESIDE_SHARE of the read's weight, it takes up to MORE_STEPS more steps along the sun; but
/// only while they can change the light: the near part under STEP_WHILE_NEAR (past it the sun term barely moves) and
/// the read's nodes spread over more than STEP_SPREAD in depth (the most a read among them can be off).
const MORE_STEPS: usize = 6;
const BESIDE_SHARE: f32 = 0.1;
const STEP_WHILE_NEAR: f32 = 3.0;
const STEP_SPREAD: f32 = 0.25;
/// A node's depth has its sign bit set where the node stands in cloud (its extinction over the fringe's).
const CLOUD_BIT: u32 = 0x8000_0000;
/// A read over the nodes in air joins the read over all eight at this weight (`SunLevel::read`).
const AIR_EASE: f32 = 0.05;
/// The read in air averages transmittance; this floors the mean before its depth is taken (a depth of 69).
const MIN_READ_T: f32 = 1.0e-30;
/// A read's results: over the nodes in air, over all eight, the weight of those in cloud, and the spread of the eight
/// depths (most less least).
const IN_AIR: usize = 0;
const ALL: usize = 1;
const CLOUD_WEIGHT: usize = 2;
const SPREAD: usize = 3;
const READS: usize = 4;
/// The fine level fades into the coarse over this many coarse cells at its edge.
const BLEND_CELLS: f32 = 2.0;
/// tau_up's cell (design §12.1 item 5: coarse; the ambient is soft).
pub const TAU_UP_CELL_M: f32 = 188.0;
/// tau_up keeps a level every half cell and sums two sub-samples between levels.
const TAU_UP_LEVEL_M: f32 = 0.5 * TAU_UP_CELL_M;
const TAU_UP_SUBS: usize = 2;

/// What a sun-depth grid is built for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SunGridSpec {
    /// Toward the sun (any length); it is held at least LIGHT_MIN_SIN up.
    pub sun: Vec3f,
    /// The shell: no cloud stands outside base_m..top_m, metres above the ground.
    pub base_m: f32,
    pub top_m: f32,
    /// The fine level's cell, metres (SUN_CELL_M x Size).
    pub cell_m: f32,
    /// The fine level's reach round the viewer (12-16 km), and the coarse level's (COARSE_RADIUS_M).
    pub fine_radius_m: f32,
    pub coarse_radius_m: f32,
    /// The extinction (1/m) above which a point is inside a cloud's body rather than its fringe: about a third of the
    /// layer's full extinction (sigma x Density / 3).
    pub body_extinction: f32,
}

/// The sun's optical depth at a point, in its parts ([`SunDepthGrid::sun_tau`]): tau_sun is `near + far`
/// ([`SunTau::total`]); the powder's local depth (design §2.3) is `local`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SunTau {
    /// The first two live steps' depth, over 2h, scaled to POWDER_REACH_M: the powder's local depth. It is the same
    /// two steps at every point, so the powder does not jump where the extra steps start or stop.
    pub local: f32,
    /// The live part: the first two steps and any taken past them.
    pub near: f32,
    /// The grid's read where the live part ends.
    pub far: f32,
}

impl SunTau {
    /// tau_sun: the live part and the grid's.
    #[inline]
    pub fn total(&self) -> f32 {
        self.near + self.far
    }
}

/// The sun's frame: `l` toward the sun; `h` horizontal toward its azimuth; `v` horizontal across it; `w` across the
/// sun in its vertical plane, upward. A point's light coordinates are (s, v, w) = (p.l, p.v, p.w).
#[derive(Clone, Copy, Debug, PartialEq)]
struct SunFrame {
    l: Vec3f,
    h: Vec3f,
    v: Vec3f,
    w: Vec3f,
    sin_e: f32,
    cos_e: f32,
}

impl SunFrame {
    fn new(sun: Vec3f) -> SunFrame {
        let l = vec3f(sun.x, sun.y.max(LIGHT_MIN_SIN * sun.length()), sun.z).normalize();
        let flat = (l.x * l.x + l.z * l.z).sqrt();
        let h = if flat > 1.0e-6 { vec3f(l.x / flat, 0.0, l.z / flat) } else { vec3f(1.0, 0.0, 0.0) };
        let (sin_e, cos_e) = (l.y, flat);
        SunFrame { l, h, v: vec3f(-h.z, 0.0, h.x), w: vec3f(-sin_e * h.x, cos_e, -sin_e * h.z), sin_e, cos_e }
    }
}

/// One level of the sun-depth grid: rays along the sun on a square lattice across it, each keeping its nodes where
/// the shell crosses the level's square.
struct SunLevel {
    frame: SunFrame,
    /// Across the sun (w and v) and along it (s), metres.
    across_m: f32,
    ds_m: f32,
    /// The level's square round the viewer: |h| and |v| up to this.
    half_m: f32,
    /// Row r of rays is at w = w0 + r across, column c at v = v0 + c across.
    w0: f32,
    v0: f32,
    rows: usize,
    cols: usize,
    /// Row r keeps nodes k_lo[r] .. k_lo[r] + counts[r] along the sun (node k at s = k ds), each of its rays its own
    /// run of them from offsets[r] + column x counts[r], k rising toward the sun.
    k_lo: Vec<i32>,
    counts: Vec<u32>,
    offsets: Vec<usize>,
    /// Per node, the depth toward the sun, its sign bit set where the node stands in cloud (CLOUD_BIT).
    tau: Vec<f32>,
    /// The density reads its fill took.
    samples: u64,
    /// The key it is kept under (`level_key`), or one no other level has where it was built for one map
    /// (`uncached_key`): the GPU's copy of it goes by this (`SunDepthGrid::gpu_key`).
    key: u64,
}

impl SunLevel {
    /// The level of `frame` over the square of `half_m` round the viewer and the shell base_m..top_m, cells `across_m`
    /// across the sun and `ds_m` along it, sub-samples about `sub_target_m` apart, filled from `extinction`; a ray
    /// `clear` says meets no cloud is left at 0 unread. Each row of rays a call of `run`, `cancelled` polled between
    /// calls. Any runner fills it bit for bit the same, with `clear` or without.
    fn build(frame: SunFrame, base_m: f32, top_m: f32, across_m: f32, ds_m: f32, sub_target_m: f32, half_m: f32, body: f32, extinction: Extinction, clear: Option<ClearSpan>, cancelled: &dyn Fn() -> bool, run: RowRunner) -> Result<SunLevel, Cancelled> {
        let (sin_e, cos_e) = (frame.sin_e, frame.cos_e);
        // A point in the square and the shell has w = -h sin + y cos within these, and v within the square.
        let w_min = -half_m * sin_e + base_m * cos_e;
        let w_max = half_m * sin_e + top_m * cos_e;
        let w0 = w_min - across_m;
        let rows = ((w_max + across_m - w0) / across_m).ceil() as usize + 1;
        let v0 = -half_m - across_m;
        let cols = ((2.0 * half_m + 2.0 * across_m) / across_m).ceil() as usize + 1;
        // Along a ray at w the shell spans s in [(base - w cos) / sin, (top - w cos) / sin] and the square (h = s cos
        // - w sin) s in [(w sin - half) / cos, (w sin + half) / cos]. A row keeps the nodes any read between it and a
        // neighbouring row weighs: the span over w within a cell of it, and a node beyond either end.
        let shell = |w: f32| ((base_m - w * cos_e) / sin_e, (top_m - w * cos_e) / sin_e);
        let square = |w: f32| if cos_e > 1.0e-6 { ((w * sin_e - half_m) / cos_e, (w * sin_e + half_m) / cos_e) } else { (f32::NEG_INFINITY, f32::INFINITY) };
        let mut k_lo = Vec::with_capacity(rows);
        let mut counts = Vec::with_capacity(rows);
        let mut offsets = Vec::with_capacity(rows);
        let mut total = 0usize;
        for r in 0..rows {
            let w = w0 + r as f32 * across_m;
            let (a, b) = (w - across_m, w + across_m);
            let lo = shell(a).0.min(shell(b).0).max(square(a).0.min(square(b).0));
            let hi = shell(a).1.max(shell(b).1).min(square(a).1.max(square(b).1));
            let (first, count) = if lo <= hi {
                let first = (lo / ds_m).floor() as i32 - 1;
                (first, ((hi / ds_m).ceil() as i32 + 1 - first + 1) as u32)
            } else {
                (0, 0)
            };
            k_lo.push(first);
            counts.push(count);
            offsets.push(total);
            total += count as usize * cols;
        }
        // The fill: down each ray from where it leaves the shell's top toward the sun, the depth summed by the
        // trapezoid rule at sub-samples ds / ceil(ds / SUB_SAMPLE_M) apart that end on each kept node, so a node's
        // own extinction is sampled and says whether it stands in cloud; under the shell's base nothing is read.
        let per_gap = (ds_m / sub_target_m).ceil().max(1.0);
        let sub_m = ds_m / per_gap;
        let fringe = FRINGE_SHARE * body;
        let samples = std::sync::atomic::AtomicU64::new(0);
        let filled = rows_by(rows, cancelled, run, &|r| {
            let (count, first) = (counts[r] as usize, k_lo[r]);
            let mut out = vec![0.0f32; count * cols];
            if count == 0 {
                return out;
            }
            let w = w0 + r as f32 * across_m;
            let (s_base, s_top) = shell(w);
            let mut taken = 0u64;
            for c in 0..cols {
                let origin = frame.w * w + frame.v * (v0 + c as f32 * across_m);
                // The reads run from the shell's top down to the lowest kept node, none under the base: a clear path
                // leaves every depth 0 and every node in air, as the reads would.
                if let Some(clear) = clear {
                    let low = (first as f32 * ds_m).max(s_base).min(s_top);
                    if clear(origin + frame.l * s_top, origin + frame.l * low) {
                        continue;
                    }
                }
                let (mut tau, mut at, mut last) = (0.0f32, s_top, extinction(origin + frame.l * s_top));
                taken += 1;
                for k in (0..count).rev() {
                    let s = (first + k as i32) as f32 * ds_m;
                    if s < at {
                        let n = ((at - s) / sub_m).ceil().max(1.0);
                        let step = (at - s) / n;
                        for m in 1..=n as usize {
                            let x = at - m as f32 * step;
                            let sigma = if x >= s_base {
                                taken += 1;
                                extinction(origin + frame.l * x)
                            } else {
                                0.0
                            };
                            tau += 0.5 * (last + sigma) * step;
                            last = sigma;
                        }
                        at = s;
                    }
                    // A node above the shell's top is in air; else `last` is its own extinction.
                    let cloud = s < s_top && last > fringe;
                    out[c * count + k] = if cloud { f32::from_bits(tau.to_bits() | CLOUD_BIT) } else { tau };
                }
            }
            samples.fetch_add(taken, std::sync::atomic::Ordering::Relaxed);
            out
        })?;
        Ok(SunLevel { frame, across_m, ds_m, half_m, w0, v0, rows, cols, k_lo, counts, offsets, tau: filled.concat(), samples: samples.into_inner(), key: uncached_key() })
    }

    /// The depth toward the sun at `q` (inside the shell's heights), or None where the level keeps no nodes round it:
    /// [the read over the nodes in air, the read over all eight, the weight of the nodes in cloud, the spread of the
    /// eight depths]. Both reads weigh the cell's eight nodes trilinearly (smooth across the sun's plane). The read in
    /// air keeps the nodes in air and divides by their weight, with the read over all eight joined in at weight
    /// AIR_EASE, so it stays continuous where a cell holds few or none in air (shadow maps' depth-aware filtering, our
    /// own form); and it averages the nodes' transmittance, e^-depth, not their depth (percentage-closer filtering, our
    /// own form): a shadow cast from far along the sun crosses a cell with an edge sharper than it, and a mean depth
    /// there darkens the lit side of the edge as much as the shadowed one (design §12.6 item 8: on a real Cumulus at a
    /// 9 degree sun it halved the worst points' error).
    #[inline]
    fn read(&self, q: Vec3f) -> Option<[f32; READS]> {
        let f = &self.frame;
        let gw = (q.dot(f.w) - self.w0) / self.across_m;
        let gv = (q.dot(f.v) - self.v0) / self.across_m;
        let gk = q.dot(f.l) / self.ds_m;
        let (fw, fv, fk) = (gw.floor(), gv.floor(), gk.floor());
        if !(fw >= 0.0 && fv >= 0.0 && fk.abs() < 1.0e9) {
            return None;
        }
        let (r, c, k) = (fw as usize, fv as usize, fk as i32);
        if r + 1 >= self.rows || c + 1 >= self.cols {
            return None;
        }
        let row = |r: usize| -> Option<(usize, usize)> {
            let at = k - self.k_lo[r];
            let n = self.counts[r] as i32;
            if at < 0 || at + 1 >= n {
                None
            } else {
                Some((self.offsets[r] + at as usize, n as usize))
            }
        };
        let ((a, n), (b, m)) = (row(r)?, row(r + 1)?);
        let (tw, tv, tk) = (gw - fw, smooth01(gv - fv), gk - fk);
        let (mut air, mut air_weight, mut all, mut cloud_weight, mut all_t) = (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32);
        let (mut lo, mut hi) = (f32::INFINITY, 0.0f32);
        for (start, wr) in [(a + c * n, (1.0 - tw) * (1.0 - tv)), (a + (c + 1) * n, (1.0 - tw) * tv), (b + c * m, tw * (1.0 - tv)), (b + (c + 1) * m, tw * tv)] {
            for (i, wk) in [(start, 1.0 - tk), (start + 1, tk)] {
                let bits = self.tau[i].to_bits();
                let depth = f32::from_bits(bits & !CLOUD_BIT);
                let wt = wr * wk;
                // Over the nodes in air the read averages transmittance, not depth (design §12.6 item 8).
                let t = (-depth).exp();
                if bits & CLOUD_BIT != 0 {
                    cloud_weight += wt;
                } else {
                    air += wt * t;
                    air_weight += wt;
                }
                all += wt * depth;
                all_t += wt * t;
                lo = lo.min(depth);
                hi = hi.max(depth);
            }
        }
        let in_air = -((air + AIR_EASE * all_t) / (air_weight + AIR_EASE)).max(MIN_READ_T).ln();
        Some([in_air, all, cloud_weight, hi - lo])
    }

    /// Whether `q` stands in the level's square, and how deep: its distance in from the square's edge, metres.
    #[inline]
    fn inside_m(&self, q: Vec3f) -> f32 {
        self.half_m - q.dot(self.frame.h).abs().max(q.dot(self.frame.v).abs())
    }

    fn bytes(&self) -> usize {
        self.tau.len() * 4 + self.k_lo.len() * 4 + self.counts.len() * 4 + self.offsets.len() * std::mem::size_of::<usize>()
    }

    /// The level as the GPU export reads it (design §12.3, §12.1 item 5): its nodes in `out`'s scalars, a node in cloud
    /// as -(depth + 1) (no bit test in the shader, and a cloud node at depth 0 stays apart from air), and per row of rays
    /// (k_lo, count, its first node as a texel x and y of the scalar buffer: offsets past 2^24 stay exact) in its
    /// vectors. Its uniforms `<name>_a` (w0, v0, across, ds) and `<name>_b` (rows, cols, half, table offset); the
    /// shader's read mirrors `read`.
    fn gpu_layout(&self, out: &mut crate::cloud_volume_gpu::UploadBuilder, a: &'static str, b: &'static str) {
        use crate::cloud_volume_gpu::{index, BUFFER_WIDTH};
        let first = out.scalars(self.tau.iter().map(|&t| {
            let bits = t.to_bits();
            if bits & CLOUD_BIT != 0 {
                -(f32::from_bits(bits & !CLOUD_BIT) + 1.0)
            } else {
                t
            }
        }));
        let table = out.vectors((0..self.rows).map(|r| {
            let start = first + self.offsets[r];
            [self.k_lo[r] as f32, self.counts[r] as f32, (start % BUFFER_WIDTH) as f32, (start / BUFFER_WIDTH) as f32]
        }));
        out.uniform(a, [self.w0, self.v0, self.across_m, self.ds_m]);
        out.uniform(b, [self.rows as f32, self.cols as f32, self.half_m, index(table)]);
    }
}

/// t^2 (3 - 2t): the smooth weight between two nodes.
#[inline]
fn smooth01(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// The sun's optical depth round the viewer for one volume (design §12.1 item 5): the near part marched live, the rest
/// read from a two-level grid. Immutable; bake threads share it.
pub struct SunDepthGrid {
    frame: SunFrame,
    base_m: f32,
    top_m: f32,
    coarse: Arc<SunLevel>,
    /// None while held, until a released bake fills it.
    fine: Option<Arc<SunLevel>>,
    blend_m: f32,
    /// The near part: two live steps over `reach_m` along the sun (2h), sampled at h / 2 and 3h / 2.
    reach_m: f32,
    /// The extinction above which a point is in a cloud's body (`SunGridSpec::body_extinction`).
    body: f32,
}

impl SunDepthGrid {
    /// The grid of `spec` filled from `extinction`, not cached: both levels, or with `held` the coarse one alone. A ray
    /// `clear` says meets no cloud is left at 0 unread.
    pub fn build(spec: &SunGridSpec, extinction: Extinction, clear: Option<ClearSpan>, held: bool, cancelled: &dyn Fn() -> bool, run: RowRunner) -> Result<SunDepthGrid, Cancelled> {
        let coarse = Arc::new(Self::level(spec, false, extinction, clear, cancelled, run)?);
        let fine = if held { None } else { Some(Arc::new(Self::level(spec, true, extinction, clear, cancelled, run)?)) };
        Ok(Self::from_levels(spec, coarse, fine))
    }

    /// The grid of `spec` from the process-wide cache, keyed by the sun, `shape` (the caller's key of every field its
    /// extinction reads besides Cover and the seed, Density and sigma included: the depths scale with them),
    /// `cover`, `seed` and the grid's geometry. The coarse level is looked up or filled; the fine one too unless
    /// `held`, and while held it is used when a released bake left it.
    pub fn shared(spec: &SunGridSpec, shape: u64, cover: f32, seed: u32, extinction: Extinction, clear: Option<ClearSpan>, held: bool, cancelled: &dyn Fn() -> bool, run: RowRunner) -> Result<SunDepthGrid, Cancelled> {
        let key = |fine: bool| level_key(spec, fine, shape, cover, seed);
        let built = |level: &SunLevel, ms: f64| {
            format!("{:.0} m level built in {:.0} ms ({:.1} MB, {} density reads)", level.across_m, ms, level.bytes() as f64 / 1.0e6, level.samples)
        };
        let keyed = |fine: bool| Self::level(spec, fine, extinction, clear, cancelled, run).map(|level| SunLevel { key: key(fine), ..level });
        let coarse = shared_by_bytes(&SUN_LEVELS, key(false), &|| keyed(false), "sun-depth grid", &built, SUN_LEVEL_BUDGET, SunLevel::bytes)?;
        let fine = if held { kept_by_key(&SUN_LEVELS, key(true)) } else { Some(shared_by_bytes(&SUN_LEVELS, key(true), &|| keyed(true), "sun-depth grid", &built, SUN_LEVEL_BUDGET, SunLevel::bytes)?) };
        Ok(Self::from_levels(spec, coarse, fine))
    }

    fn level(spec: &SunGridSpec, fine: bool, extinction: Extinction, clear: Option<ClearSpan>, cancelled: &dyn Fn() -> bool, run: RowRunner) -> Result<SunLevel, Cancelled> {
        let frame = SunFrame::new(spec.sun);
        let (across, along, sub, half) = if fine {
            (spec.cell_m, FINE_ALONG * spec.cell_m, SUB_SAMPLE_M, spec.fine_radius_m)
        } else {
            (COARSE_CELLS * spec.cell_m, COARSE_CELLS * spec.cell_m, SUB_SAMPLE_COARSE_M, spec.coarse_radius_m)
        };
        SunLevel::build(frame, spec.base_m, spec.top_m, across, along, sub, half, spec.body_extinction, extinction, clear, cancelled, run)
    }

    fn from_levels(spec: &SunGridSpec, coarse: Arc<SunLevel>, fine: Option<Arc<SunLevel>>) -> SunDepthGrid {
        SunDepthGrid { frame: SunFrame::new(spec.sun), base_m: spec.base_m, top_m: spec.top_m, coarse, fine, blend_m: BLEND_CELLS * COARSE_CELLS * spec.cell_m, reach_m: NEAR_CELLS * spec.cell_m, body: spec.body_extinction.max(1.0e-9) }
    }

    /// The sun's optical depth at `p` ([`SunTau`]): the near part marched live through `extinction` along the sun, the
    /// far part read from the grid where it ends, at q. `here` is the extinction at `p` without detail (the caller's
    /// march has it). tau_sun is near + far; the powder's local depth is `local`, the first two steps alone, so it
    /// does not jump where the extra steps toggle.
    /// - The near part is two live steps over 2h (NEAR_CELLS cells: 59 m at 47 m cells), sampled at h / 2 and 3h / 2
    ///   (about 15 and 44 m), with the extinction at p and at q = p + 2h as end nodes: weights h / 9, 8h / 9, 8h / 9
    ///   and h / 9 make the rule exact for a cubic over the 2h. The end node at p carries a faint fringe that ends
    ///   within the first step, which midpoints alone miss.
    /// - Where q ends in a cloud's soft edge (between the fringe and twice the body's extinction) or in air beside a
    ///   cloud (cloud nodes over BESIDE_SHARE of the read's weight), the near part goes on in steps of 2h (Simpson's
    ///   rule, two reads each), up to MORE_STEPS, toward air clear of cloud or the deep, while that can change the
    ///   light (the near part under STEP_WHILE_NEAR, the read's nodes spread over STEP_SPREAD).
    /// - The read then weighs the air nodes round q where q is in air (to the fringe's extinction) and all of them deep
    ///   in cloud, blended smoothly between by q's extinction, so a surface draws no seam ([`SunLevel::read`]).
    #[inline]
    pub fn sun_tau(&self, p: Vec3f, here: f32, extinction: Extinction) -> SunTau {
        self.sun_tau_steps(p, here, extinction).0
    }

    /// [`Self::sun_tau`], and how many steps it took past the first two.
    #[inline]
    pub(crate) fn sun_tau_steps(&self, p: Vec3f, here: f32, extinction: Extinction) -> (SunTau, usize) {
        let l = self.frame.l;
        let reach = self.reach_m;
        let h = 0.5 * reach;
        let mut q = p + l * reach;
        let mut at_q = extinction(q);
        let mut near = (here + at_q) * (h / 9.0) + (extinction(p + l * (0.5 * h)) + extinction(p + l * (1.5 * h))) * (8.0 * h / 9.0);
        let local = near * (POWDER_REACH_M / reach);
        let (fringe, deep) = (FRINGE_SHARE * self.body, DEEP_BODIES * self.body);
        let mut steps = 0;
        let read = loop {
            let read = self.far(q);
            let edge = at_q > fringe && at_q < deep;
            let beside = at_q <= fringe && read[CLOUD_WEIGHT] > BESIDE_SHARE;
            let worth = near < STEP_WHILE_NEAR && read[SPREAD] > STEP_SPREAD;
            if steps == MORE_STEPS || !((edge || beside) && worth) {
                break read;
            }
            let next = extinction(q + l * reach);
            near += (at_q + 4.0 * extinction(q + l * h) + next) * (reach / 6.0);
            q += l * reach;
            at_q = next;
            steps += 1;
        };
        (SunTau { local, near, far: self.blended(&read, at_q) }, steps)
    }

    /// The grid alone at `p`, no live steps: a cheap read for a view ray already mostly opaque (design §12.1 item 5).
    /// `here` is the extinction at `p`; the read weighs the nodes as [`Self::sun_tau`]'s does where its live part
    /// ends: the air nodes round a point in air, all of them deep in cloud. Measured per pixel (the sun light test),
    /// it is fit past 90 % opacity (a pixel off by up to 0.051 against the hybrid's 0.029); past 30 % it leaves most
    /// of a pixel's light on the grid and puts it off by up to 0.3.
    #[inline]
    pub fn grid_tau(&self, p: Vec3f, here: f32) -> f32 {
        self.blended(&self.far(p), here)
    }

    /// A read at a point of extinction `at`: over the air nodes where the point is in air (to the fringe's
    /// extinction), over all deep in cloud, blended smoothly between, so a surface draws no seam.
    #[inline]
    fn blended(&self, read: &[f32; READS], at: f32) -> f32 {
        let (fringe, deep) = (FRINGE_SHARE * self.body, DEEP_BODIES * self.body);
        // A point in cloud, even its faint fringe, leans on all the nodes by the weight of those in cloud too, ramped in
        // from the fringe (design §12.6 item 8: a read ending in a soft edge among cloud nodes fell to the air nodes').
        let inside = smooth01(((at - fringe) / (deep - fringe)).clamp(0.0, 1.0)).max(read[CLOUD_WEIGHT] * smooth01(((at - fringe) / (2.0 * fringe)).clamp(0.0, 1.0)));
        read[IN_AIR] + (read[ALL] - read[IN_AIR]) * inside
    }

    /// The grid's depths toward the sun at `q` (`SunLevel::read`'s four): 0 above the shell; below it, the depths
    /// where q's path enters the base; the fine level inside its square, fading into the coarse over its last coarse
    /// cells; 0 past the coarse one.
    #[inline]
    fn far(&self, q: Vec3f) -> [f32; READS] {
        if !(q.y < self.top_m) {
            return [0.0; READS];
        }
        let q = if q.y < self.base_m { q + self.frame.l * ((self.base_m - q.y) / self.frame.sin_e) } else { q };
        if let Some(fine) = &self.fine {
            let inside = fine.inside_m(q);
            if inside > 0.0 {
                if let Some(f) = fine.read(q) {
                    if inside >= self.blend_m {
                        return f;
                    }
                    let c = self.coarse.read(q).unwrap_or(f);
                    let t = smooth01(inside / self.blend_m);
                    return [IN_AIR, ALL, CLOUD_WEIGHT, SPREAD].map(|i| if i == SPREAD { c[i].max(f[i]) } else { c[i] + (f[i] - c[i]) * t });
                }
            }
        }
        self.coarse.read(q).unwrap_or([0.0; READS])
    }

    /// The grid's four depths at `q` (`far`'s), for the GPU crate's probe test (design §12.3).
    pub(crate) fn far_read(&self, q: Vec3f) -> [f32; READS] {
        self.far(q)
    }

    /// Toward the sun as the grid and the live steps take it: the caller's sun held at least LIGHT_MIN_SIN up, unit
    /// length. The march's phase and sun height (RayLight::new) take this one, so all three share a direction.
    pub fn light_dir(&self) -> Vec3f {
        self.frame.l
    }

    /// Whether the fine level is filled (a released bake's grid, or a held one that found it).
    pub fn has_fine(&self) -> bool {
        self.fine.is_some()
    }

    /// Per level, fine first when filled: (cell across, nodes, bytes, density reads of its fill).
    pub fn levels(&self) -> Vec<(f32, usize, usize, u64)> {
        self.fine.iter().chain(std::iter::once(&self.coarse)).map(|l| (l.across_m, l.tau.len(), l.bytes(), l.samples)).collect()
    }

    /// The grid as the GPU export reads it (design §12.3): both levels (`SunLevel::gpu_layout`; a grid without its fine
    /// level says so in `sun_g.w`), the sun's frame, the blend, the near part's reach and the body, and the hybrid's
    /// constants, so the shader's `sun_tau`, `grid_tau` and `far` mirror these. The shader's literal loop takes
    /// MORE_STEPS + 1 passes.
    pub(crate) fn gpu_layout(&self, out: &mut crate::cloud_volume_gpu::UploadBuilder) {
        const _: () = assert!(MORE_STEPS == 6, "the shader's sun_tau loop is literal: 7 passes");
        let f = &self.frame;
        out.uniform("sun_l", [f.l.x, f.l.y, f.l.z, f.sin_e]);
        out.uniform("sun_h", [f.h.x, f.h.y, f.h.z, f.cos_e]);
        out.uniform("sun_v", [f.v.x, f.v.y, f.v.z, 0.0]);
        out.uniform("sun_w", [f.w.x, f.w.y, f.w.z, 0.0]);
        out.uniform("sun_g", [self.blend_m, self.reach_m, self.body, self.fine.is_some() as u32 as f32]);
        out.uniform("sun_shell", [self.base_m, self.top_m, 0.0, 0.0]);
        out.uniform("k_sun", [FRINGE_SHARE, DEEP_BODIES, BESIDE_SHARE, STEP_WHILE_NEAR]);
        out.uniform("k_sun2", [STEP_SPREAD, AIR_EASE, MIN_READ_T, MORE_STEPS as f32]);
        self.coarse.gpu_layout(out, "coarse_a", "coarse_b");
        match &self.fine {
            Some(fine) => fine.gpu_layout(out, "fine_a", "fine_b"),
            None => {
                out.uniform("fine_a", [0.0; 4]);
                out.uniform("fine_b", [0.0; 4]);
            }
        }
    }

    /// The key of its GPU copy (design §12.11 item 4.6: kept on the GPU between asks): its levels' keys, which name the
    /// sun, the shell, the cells, the shape, Cover and the seed (everything `gpu_layout` writes), and whether it has
    /// its fine level (a held grid has not).
    pub(crate) fn gpu_key(&self) -> u64 {
        crate::cloud_volume_gpu::key_of(&[self.coarse.key, self.fine.as_ref().map_or(0, |f| f.key), self.fine.is_some() as u64])
    }
}

/// The optical depth straight up from a point to the top of the shell (design §2.3, §12.1 item 5): an unleaned coarse
/// grid, TAU_UP_CELL_M across, a level every TAU_UP_LEVEL_M, filled as a running sum down each column, read with
/// smooth weights across and linear ones up. The dome's light is attenuated by it.
pub struct TauUpGrid {
    half_m: f32,
    cell_m: f32,
    /// Columns along an edge, at x = -half + i cell.
    n: usize,
    base_m: f32,
    level_m: f32,
    /// Levels a column keeps, from the base (0) to the top (levels - 1).
    levels: usize,
    /// Column (i, j) (x fastest) from (j n + i) levels.
    tau: Vec<f32>,
    samples: u64,
    /// The key it is kept under, or one no other grid has where it was built for one map (as `SunLevel::key`).
    key: u64,
}

impl TauUpGrid {
    /// The grid over the square of `half_m` round the viewer and the shell base_m..top_m, filled from `extinction`; a
    /// column `clear` says meets no cloud is left at 0 unread. Each row of columns a call of `run`, `cancelled` polled
    /// between calls. Any runner fills it bit for bit the same, with `clear` or without.
    pub fn build(base_m: f32, top_m: f32, half_m: f32, extinction: Extinction, clear: Option<ClearSpan>, cancelled: &dyn Fn() -> bool, run: RowRunner) -> Result<TauUpGrid, Cancelled> {
        let cell_m = TAU_UP_CELL_M;
        let n = (2.0 * half_m / cell_m).ceil() as usize + 1;
        let gaps = ((top_m - base_m) / TAU_UP_LEVEL_M).ceil().max(1.0) as usize;
        let level_m = (top_m - base_m) / gaps as f32;
        let levels = gaps + 1;
        let sub_m = level_m / TAU_UP_SUBS as f32;
        let filled = rows_by(n, cancelled, run, &|j| {
            let z = -half_m + j as f32 * cell_m;
            let mut out = vec![0.0f32; n * levels];
            for i in 0..n {
                let x = -half_m + i as f32 * cell_m;
                if clear.is_some_and(|clear| clear(vec3f(x, base_m, z), vec3f(x, top_m, z))) {
                    continue;
                }
                let mut tau = 0.0f32;
                for l in (0..gaps).rev() {
                    let top = base_m + (l + 1) as f32 * level_m;
                    for m in 0..TAU_UP_SUBS {
                        tau += extinction(vec3f(x, top - (m as f32 + 0.5) * sub_m, z)) * sub_m;
                    }
                    out[i * levels + l] = tau;
                }
            }
            out
        })?;
        Ok(TauUpGrid { half_m, cell_m, n, base_m, level_m, levels, tau: filled.concat(), samples: (n * n * gaps * TAU_UP_SUBS) as u64, key: uncached_key() })
    }

    /// The grid from the process-wide cache, keyed by `shape`, `cover`, `seed` and its geometry (not the sun: a sun drag
    /// keeps it). `shape` must carry Density and sigma as [`SunDepthGrid::shared`]'s does: this key has no extinction
    /// scale of its own.
    pub fn shared(base_m: f32, top_m: f32, half_m: f32, shape: u64, cover: f32, seed: u32, extinction: Extinction, clear: Option<ClearSpan>, cancelled: &dyn Fn() -> bool, run: RowRunner) -> Result<Arc<TauUpGrid>, Cancelled> {
        let key = key_of(&[2, base_m.to_bits(), top_m.to_bits(), half_m.to_bits(), shape as u32, (shape >> 32) as u32, cover.to_bits(), seed]);
        let built = |grid: &TauUpGrid, ms: f64| format!("{}^2 x {} built in {:.0} ms ({:.1} MB)", grid.n, grid.levels, ms, grid.bytes() as f64 / 1.0e6);
        shared_by_bytes(&TAU_UPS, key, &|| TauUpGrid::build(base_m, top_m, half_m, extinction, clear, cancelled, run).map(|grid| TauUpGrid { key, ..grid }), "tau_up grid", &built, TAU_UP_BUDGET, TauUpGrid::bytes)
    }

    /// The depth straight up from `p` to the shell's top: 0 above it, the base's below it, 0 outside the square.
    #[inline]
    pub fn tau_up(&self, p: Vec3f) -> f32 {
        let gx = (p.x + self.half_m) / self.cell_m;
        let gz = (p.z + self.half_m) / self.cell_m;
        let top = (self.n - 1) as f32;
        if !(gx >= 0.0 && gx <= top && gz >= 0.0 && gz <= top) {
            return 0.0;
        }
        let gy = ((p.y - self.base_m) / self.level_m).clamp(0.0, (self.levels - 1) as f32);
        let (fx, fz, fy) = (gx.floor().min(top - 1.0), gz.floor().min(top - 1.0), gy.floor().min((self.levels - 2) as f32));
        let (tx, tz, ty) = (smooth01(gx - fx), smooth01(gz - fz), gy - fy);
        let (i, j, l) = (fx as usize, fz as usize, fy as usize);
        let t = &self.tau;
        let up = |i: usize, j: usize| {
            let k = (j * self.n + i) * self.levels + l;
            t[k] + (t[k + 1] - t[k]) * ty
        };
        let (a, b) = (up(i, j), up(i + 1, j));
        let (c, d) = (up(i, j + 1), up(i + 1, j + 1));
        let (near, far) = (a + (b - a) * tx, c + (d - c) * tx);
        near + (far - near) * tz
    }

    pub fn bytes(&self) -> usize {
        self.tau.len() * 4
    }

    /// The grid as the GPU export reads it (design §12.3): its columns' levels in `out`'s scalars at (j n + i) levels + l,
    /// and the uniforms `tau_up_a` (half, cell, n, base) and `tau_up_b` (level metres, levels, offset, 0); the shader's
    /// read mirrors `tau_up`.
    pub(crate) fn gpu_layout(&self, out: &mut crate::cloud_volume_gpu::UploadBuilder) {
        let offset = out.scalars(self.tau.iter().copied());
        out.uniform("tau_up_a", [self.half_m, self.cell_m, self.n as f32, self.base_m]);
        out.uniform("tau_up_b", [self.level_m, self.levels as f32, crate::cloud_volume_gpu::index(offset), 0.0]);
    }

    /// The key of its GPU copy (design §12.11 item 4.6: kept on the GPU between asks): the key it is kept under.
    pub(crate) fn gpu_key(&self) -> u64 {
        self.key
    }

    /// The density reads its fill took.
    pub fn samples(&self) -> u64 {
        self.samples
    }
}

/// The sun-depth levels and tau_up grids in use, oldest first.
static SUN_LEVELS: GridCache<SunLevel> = Mutex::new(Vec::new());
static TAU_UPS: GridCache<TauUpGrid> = Mutex::new(Vec::new());
/// The bytes of sun-depth levels no live map holds that their cache keeps, the newest ones: a cloud preset's two levels
/// are 1.8-22.3 MB (a Stratocumulus' 11.2, the storm's 20.1), so about its last 7 (the largest) to 14 (a
/// Stratocumulus) presets' levels, more of the smaller ones. (A count of eight kept the live map's levels and eight
/// more: a first preset's went at about the sixth or seventh.)
const SUN_LEVEL_BUDGET: usize = 160_000_000;
/// The bytes of tau_up grids no live map holds that their cache keeps, the newest ones: 1.9-23.9 MB a cloud preset (a
/// Stratocumulus' 3.7), so about its last 4 (the largest) to 26 (a Stratocumulus) presets' grids (256 MB of light
/// grids in all, with the levels).
const TAU_UP_BUDGET: usize = 96_000_000;

/// A key no other grid has: a level or a tau_up grid built for one map, outside the caches (its GPU copy is never
/// taken for another's).
fn uncached_key() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    0x5eed_0000_0000_0000 ^ NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// FNV-1a over `words`' bytes.
fn key_of(words: &[u32]) -> u64 {
    words.iter().flat_map(|w| w.to_le_bytes()).fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3))
}

/// A level's key: the sun's held direction, the shell, the cell, the level's reach (the fine radius or the coarse
/// one), the body's extinction (its nodes' cloud flags), the caller's shape key, Cover and the seed.
fn level_key(spec: &SunGridSpec, fine: bool, shape: u64, cover: f32, seed: u32) -> u64 {
    let l = SunFrame::new(spec.sun).l;
    let half = if fine { spec.fine_radius_m } else { spec.coarse_radius_m };
    key_of(&[
        fine as u32,
        l.x.to_bits(),
        l.y.to_bits(),
        l.z.to_bits(),
        spec.base_m.to_bits(),
        spec.top_m.to_bits(),
        spec.cell_m.to_bits(),
        half.to_bits(),
        spec.body_extinction.to_bits(),
        shape as u32,
        (shape >> 32) as u32,
        cover.to_bits(),
        seed,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caster::serial_rows;
    use crate::cloud_light::LightFamily;
    use crate::cloud_volume_light::RayLight;
    use crate::noise::hash01;
    use crate::volumetric::FORWARD_SHARE;

    /// A Cumulus's extinction at Density 1 (design §2.2) and a Stratocumulus deck's.
    const CUMULUS_SIGMA: f32 = 0.06;
    const DECK_SIGMA: f32 = 0.05;

    /// Runs every index on four scoped threads, interleaved, as the bake pool's fan_out would.
    fn four_threads(n: usize, f: &(dyn Fn(usize) + Sync)) {
        std::thread::scope(|scope| {
            for t in 0..4 {
                scope.spawn(move || {
                    let mut i = t;
                    while i < n {
                        f(i);
                        i += 4;
                    }
                });
            }
        });
    }

    fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
        smooth01(((x - a) / (b - a)).clamp(0.0, 1.0))
    }

    /// A soft ellipsoid: `centre`, `radii`, full extinction `sigma` inside, falling to 0 over the outer `edge` share
    /// of its radius (about 75 m on a Cumulus's).
    fn ellipsoid(p: Vec3f, centre: Vec3f, radii: Vec3f, sigma: f32, edge: f32) -> f32 {
        let d = (p - centre) / radii;
        sigma * smoothstep(0.0, edge, 1.0 - d.length())
    }

    /// A synthetic Cumulus, 1.6 km wide and 1.5 km tall, standing 1250-2750 m up at `centre` (x, z): a main body and a
    /// tower on its shoulder (a crease between them), soft edges.
    fn cumulus(p: Vec3f, centre: Vec2f) -> f32 {
        let body = ellipsoid(p, vec3f(centre.x, 1900.0, centre.y), vec3f(800.0, 650.0, 700.0), CUMULUS_SIGMA, 0.1);
        let tower = ellipsoid(p, vec3f(centre.x + 250.0, 2300.0, centre.y - 150.0), vec3f(380.0, 450.0, 360.0), CUMULUS_SIGMA, 0.15);
        body.max(tower)
    }

    /// A Stratocumulus deck everywhere, 1300-1600 m, its top and base soft over 30 m.
    fn deck(p: Vec3f) -> f32 {
        DECK_SIGMA * smoothstep(1300.0, 1330.0, p.y) * (1.0 - smoothstep(1570.0, 1600.0, p.y))
    }

    /// The sun `elevation` degrees up at an azimuth off the axes.
    fn sun_at(elevation: f32) -> Vec3f {
        let (e, a) = (elevation.to_radians(), 200.0f32.to_radians());
        vec3f(e.cos() * a.sin(), e.sin(), -e.cos() * a.cos())
    }

    /// The reference: the depth toward the sun from `p` in 5 m steps to the shell's top, or over the first `reach`
    /// metres.
    fn fine_march(p: Vec3f, l: Vec3f, top: f32, reach: f32, extinction: Extinction) -> f32 {
        fine_march_to(p, l, top, reach, f32::INFINITY, extinction)
    }

    /// `fine_march`, given up once the depth passes `most`.
    fn fine_march_to(p: Vec3f, l: Vec3f, top: f32, reach: f32, most: f32, extinction: Extinction) -> f32 {
        let mut tau = 0.0f32;
        let mut t = 2.5f32;
        loop {
            let q = p + l * t;
            if q.y > top || t > reach || tau > most {
                return tau;
            }
            tau += extinction(q) * 5.0;
            t += 5.0;
        }
    }

    /// Light from a depth toward the sun and the powder's local depth: the light law's octave sum of the sun's term
    /// for a view along `view` (unit), as a share of the unshadowed sun's.
    fn light(view: Vec3f, l: Vec3f, tau: f32, local: f32) -> f32 {
        let ray = RayLight::new(view.dot(l), l.y, FORWARD_SHARE, LightFamily::Heap);
        ray.sun_light(tau, local) / ray.sun_light(0.0, 1.0e3)
    }

    /// A surface point leaves at least 0.05 of the sun: its depth toward the sun is under -ln 0.05.
    const SURFACE_TAU: f32 = 3.1;

    /// Up to `want` points of `field` on its sun-facing surfaces, where the reference depth toward the sun leaves
    /// 0.05-0.95 of the sun: picked from `candidate(k)`.
    fn surface_points(want: usize, l: Vec3f, top: f32, floor: f32, extinction: Extinction, candidate: &dyn Fn(i32) -> Vec3f) -> Vec<(Vec3f, f32, f32)> {
        let mut points = Vec::new();
        for k in 0..200 * want as i32 {
            let p = candidate(k);
            if extinction(p) <= floor {
                continue;
            }
            let tau = fine_march_to(p, l, top, f32::INFINITY, SURFACE_TAU, extinction);
            let t = (-tau).exp();
            if (0.05..=0.95).contains(&t) {
                points.push((p, tau, fine_march(p, l, top, POWDER_REACH_M, extinction)));
                if points.len() == want {
                    break;
                }
            }
        }
        points
    }

    /// The real Cumulus population a sun (design §12.8 item 1): surface points, a view ray through each.
    const POPULATION: usize = 2400;
    /// The renderer's march is checked on this many of the population's rays (a 2 m reference march each).
    const RENDERER_RAYS: usize = 150;
    /// What the landed field reaches per sun (azimuth, elevation): the point error's 99th percentile and mean, the
    /// pixel error's, and the renderer's march's (the larger of 512 and 1024), a regression lock (design §12.8 item 1),
    /// re-locked at 1.05 x what a changed field reaches (item 9). Task 6b's re-locks rose past item 9's 10 % a sun and
    /// §12.9 item 6's 25 % for a lobe change; they stand as the CPU path's regression lock (§12.10 item 1). PRE_6B is
    /// the lock before 6b, which Task 13's live sun march must bring each number back under (§12.9 item 6); the test
    /// prints every reached number over it. After 6b, 35 of its 36 numbers had risen (all but the renderer p99 at
    /// 200/60, x0.71), each re-lock moving them so (a number's rise at each step, over the six suns):
    /// - The recipe's seeded offset (§12.9 item 2) only moves the clouds, yet re-samples this population: over four
    ///   offsets the six-sun pixel and renderer p99 averages ran 0.0255-0.0298 and 0.0292-0.0390 (before, 0.0254 and
    ///   0.0314) and one sun's renderer p99 swung -64 to +82 %. Point p99 -13 to +15 %, pixel p99 -10 to +18 %, renderer
    ///   p99 -37 to +91 % (65/30) and +66 % (65/60), the means -16 to +10 %.
    /// - The weak satellite clouds (§12.9 item 3) are lit through thin fringes, where the 47 m grid is least exact.
    ///   Point p99 0 to +50 %, pixel p99 -9 to +72 % (65/30; 65/9 +46 %), renderer p99 -34 to +78 % (200/9; 65/9
    ///   +56 %), the means -8 to +76 %.
    /// - The billows (§12.9 items 1 and 6) carve lobes down to 47 m apart, finer than the 47 m grid and the two live
    ///   steps can follow. Point p99 +57 to +341 %, pixel p99 +56 to +115 %, renderer p99 +7 to +189 %, the point and
    ///   pixel means +58 to +220 %, the renderer's +14 to +81 %.
    /// - Task 6c, not ruled yet: these re-locks miss item 9 (the renderer p99's six-sun average rose, and single suns
    ///   more than 10 %); the controller rules whether they stand, and until then they are not to land. The view sees
    ///   no mist and fills its gaps toward the core and over its base (`FILL_FROM`, `BASE_FILL`); the sun's field is the
    ///   view's with the mist between its lobes (`MIST_SHARE`), so the sun crosses as solid the fill the view draws,
    ///   finer than the 47 m grid. Against 6b's reached values the six-sun averages are pixel p99 0.0547 to 0.0520 and
    ///   renderer p99 0.0534 to 0.0636 (+19 %). Every number that rose, all for that one reason: point p99 65/9 +28 %;
    ///   point mean 200/30 +13 %, 200/60 +16 %, 65/9 +17 %, 65/30 +10 %, 65/60 +13 %; pixel p99 200/30 +20 %, 200/60
    ///   +14 %, 65/9 +10 %; pixel mean 200/30 +7 %, 200/60 +10 %, 65/9 +4 %, 65/60 +3 %; renderer p99 200/60 +125 %,
    ///   65/9 +69 %, 65/30 +47 %, 65/60 +42 %; renderer mean 200/60 +49 %, 65/9 +33 %, 65/30 +29 %, 65/60 +18 %. The
    ///   renderer's worst rays at 200/60 and 65/30 meet the clouds low (1246-1310 m), on the filled base.
    ///
    /// Over PRE_6B (after Task 6c, all 36 numbers): point p99 x1.7-5.1, point mean x2.4-5.3, pixel p99 x1.5-2.4,
    /// pixel mean x1.8-2.2, renderer p99 x1.5-2.6, renderer mean x1.4-2.0. More live steps on the skin (§12.8 item 2: MORE_STEPS 12, STEP_SPREAD 0.1)
    /// left every pixel p99 within 2 %, far from item 2's 0.03, and was not kept; nor was the strength read from two
    /// sums (the 6b review's S2: renderer p99 +4 to +52 %, `STRENGTH_CLEAR`).
    const HELD: [(f32, f32, f32, f32, f32, f32, f32, f32); 6] = [
        (200.0, 9.0, 0.2140, 0.0221, 0.0745, 0.0077, 0.0941, 0.0128),
        (200.0, 30.0, 0.1283, 0.0125, 0.0516, 0.0055, 0.0365, 0.0082),
        (200.0, 60.0, 0.0868, 0.0112, 0.0270, 0.0048, 0.0496, 0.0075),
        (65.0, 9.0, 0.2571, 0.0264, 0.1036, 0.0095, 0.1226, 0.0155),
        (65.0, 30.0, 0.1353, 0.0139, 0.0401, 0.0052, 0.0536, 0.0086),
        (65.0, 60.0, 0.0942, 0.0131, 0.0310, 0.0050, 0.0447, 0.0068),
    ];
    /// The lock before Task 6b (2d534998), in HELD's order: what Task 13 brings each number back under.
    const PRE_6B: [(f32, f32, f32, f32, f32, f32, f32, f32); 6] = [
        (200.0, 9.0, 0.1229, 0.0088, 0.0481, 0.0040, 0.0487, 0.0085),
        (200.0, 30.0, 0.0459, 0.0036, 0.0240, 0.0026, 0.0229, 0.0053),
        (200.0, 60.0, 0.0163, 0.0020, 0.0128, 0.0023, 0.0311, 0.0041),
        (65.0, 9.0, 0.1107, 0.0094, 0.0453, 0.0041, 0.0588, 0.0074),
        (65.0, 30.0, 0.0553, 0.0044, 0.0156, 0.0023, 0.0203, 0.0049),
        (65.0, 60.0, 0.0231, 0.0028, 0.0144, 0.0024, 0.0163, 0.0041),
    ];

    /// `f` at every index on the machine's threads, in order.
    fn spread_map<T: Send + Default + Clone>(n: usize, f: &(dyn Fn(usize) -> T + Sync)) -> Vec<T> {
        let threads = std::thread::available_parallelism().map_or(4, |t| t.get());
        let out: Vec<Mutex<T>> = (0..n).map(|_| Mutex::new(T::default())).collect();
        let next = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..threads {
                scope.spawn(|| loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if i >= n {
                        break;
                    }
                    *out[i].lock().unwrap() = f(i);
                });
            }
        });
        out.into_iter().map(|m| m.into_inner().unwrap()).collect()
    }

    /// A population's errors: the 99th percentile, the mean, and the three worst with the points they sit at.
    struct Spread {
        p99: f32,
        mean: f32,
        worst: Vec<(f32, Vec3f)>,
    }

    impl Spread {
        fn of(errors: impl Iterator<Item = f32>, points: &[(Vec3f, f32, f32)]) -> Spread {
            let mut all: Vec<(f32, Vec3f)> = errors.zip(points).map(|(e, p)| (e, p.0)).collect();
            all.sort_by(|a, b| b.0.total_cmp(&a.0));
            let n = all.len();
            Spread { p99: all[n / 100].0, mean: all.iter().map(|e| e.0).sum::<f32>() / n as f32, worst: all.into_iter().take(3).collect() }
        }

        fn line(&self, what: &str) -> String {
            let worst: Vec<String> = self.worst.iter().map(|(e, p)| format!("{e:.4} at ({:.0}, {:.0}, {:.0})", p.x, p.y, p.z)).collect();
            format!("{what} error {:.4} at the 99th percentile, {:.4} on average; the worst {}", self.p99, self.mean, worst.join(", "))
        }
    }

    /// Every node of both levels as bits.
    fn bits(grid: &SunDepthGrid) -> Vec<u32> {
        grid.fine.iter().chain(std::iter::once(&grid.coarse)).flat_map(|l| l.tau.iter().map(|t| t.to_bits())).collect()
    }

    /// Design §12.1 item 5, §12.4: the sun's light (the light law's octave sum of the sun term, not the depth) from
    /// [`SunDepthGrid::sun_tau`] against a 5 m reference march along the sun, at points on the sun-facing surfaces
    /// of a synthetic Cumulus (one in the fine level, one past it in the coarse) and of a deck, where the reference
    /// leaves 0.05-0.95 of the sun and the point is in cloud (over the fringe's extinction), the sun 9, 30 and 60
    /// degrees up: max and mean error printed, the max under 0.05, and the steps taken past the first two. Printed
    /// beside it, to show why the hybrid is built as it is: the grid alone at the point (no live steps), and the two
    /// live steps with a plain read over all eight nodes a cell and a quarter on (no air-aware read, no more steps).
    /// The powder is on, its local depth the first two steps against a 5 m march over POWDER_REACH_M. Per pixel, view
    /// rays through the points, the hybrid throughout and `grid_tau` past 90 % opacity are held to the reference;
    /// the grid or the two steps alone past 30 % are printed beside them.
    /// A grid filled on four threads is the serial one bit for bit (both levels, and tau_up), and a cancel or a
    /// skipped row stops a fill. tau_up is a 5 m march up within 5 %. The cache keeps one level per key; a held grid
    /// takes the coarse level and the fine one a released grid left, and fills only its coarse level for a new sun.
    ///
    /// Grown with the real Cumulus density (design §12.6 item 8, §12.8 item 1): a Cumulus volume's own field and grid
    /// spec (the production radii), the sun 9, 30 and 60 degrees up at two azimuths, a population of POPULATION
    /// sun-facing surface points each: the 99th percentile and the mean of the error per point and per pixel (view rays
    /// from the viewer, the march's switch to the grid at 90 % opacity), and of the renderer's own march
    /// (`CloudVolume::march`) at 512 and 1024 against a 2 m march through the same density on RENDERER_RAYS of them,
    /// held at what the landed field reaches (HELD), the gate's 0.05 and 0.03 and the worst samples printed; and the
    /// fill that leaves the paths the footprint's max pyramid calls clear is the plain fill bit for bit, on four threads
    /// as serially (design §12.6 item 6).
    #[test]
    fn the_sun_light_matches_a_fine_march() {
        let near_cloud = vec2f(2600.0, -3100.0);
        let far_cloud = vec2f(-7200.0, 6900.0);
        let field = move |p: Vec3f| cumulus(p, near_cloud).max(cumulus(p, far_cloud));
        let deck_field = |p: Vec3f| deck(p);
        for elevation in [9.0f32, 30.0, 60.0] {
            let sun = sun_at(elevation);
            for (name, base, top, sigma, extinction) in [("cumulus", 1200.0f32, 2800.0f32, CUMULUS_SIGMA, &field as Extinction), ("deck", 1200.0, 1700.0, DECK_SIGMA, &deck_field as Extinction)] {
                let spec = SunGridSpec { sun, base_m: base, top_m: top, cell_m: SUN_CELL_M, fine_radius_m: 6000.0, coarse_radius_m: 12_000.0, body_extinction: sigma / 3.0 };
                let grid = SunDepthGrid::build(&spec, extinction, None, false, &|| false, &serial_rows).expect("nothing cancels");
                let l = grid.frame.l;
                let clouds: Vec<(&str, Box<dyn Fn(i32) -> Vec3f>)> = if name == "cumulus" {
                    // A point on the body's or the tower's surface facing the sun, then up to 150 m in.
                    let on = move |centre: Vec2f| {
                        move |k: i32| {
                            let r = |salt: u32| hash01(k, 7, 3, salt);
                            let (u, a) = (2.0 * r(1) - 1.0, std::f32::consts::TAU * r(2));
                            let n = vec3f((1.0 - u * u).sqrt() * a.cos(), u, (1.0 - u * u).sqrt() * a.sin());
                            let n = if n.dot(l) < 0.0 { n - l * (2.0 * n.dot(l)) } else { n };
                            let (c, radii) = if r(3) < 0.7 { (vec3f(centre.x, 1900.0, centre.y), vec3f(800.0, 650.0, 700.0)) } else { (vec3f(centre.x + 250.0, 2300.0, centre.y - 150.0), vec3f(380.0, 450.0, 360.0)) };
                            c + n * radii - n * (150.0 * r(4))
                        }
                    };
                    vec![("fine", Box::new(on(near_cloud)) as Box<dyn Fn(i32) -> Vec3f>), ("coarse", Box::new(on(far_cloud)))]
                } else {
                    // Up to 80 m under the deck's top, inside the fine level clear of its fade.
                    vec![("fine", Box::new(move |k: i32| vec3f(4000.0 * (2.0 * hash01(k, 1, 0, 9) - 1.0), 1600.0 - 80.0 * hash01(k, 2, 0, 9), 4000.0 * (2.0 * hash01(k, 3, 0, 9) - 1.0))) as Box<dyn Fn(i32) -> Vec3f>)]
                };
                for (level, candidate) in clouds {
                    let points = surface_points(300, l, top, FRINGE_SHARE * spec.body_extinction, extinction, &*candidate);
                    assert!(points.len() >= 100, "{name} ({level}), sun {elevation}: {} points", points.len());
                    let (mut worst, mut sum, mut steps) = (0.0f32, 0.0f32, 0usize);
                    let (mut alone_worst, mut alone_sum, mut plain_worst, mut plain_sum) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
                    let mut worst_at = (Vec3f::default(), 0.0f32, 0.0f32);
                    for &(p, tau, local) in &points {
                        let view = p.normalize();
                        let want = light(view, l, tau, local);
                        let (got, more) = grid.sun_tau_steps(p, extinction(p), extinction);
                        steps += more;
                        let off = (light(view, l, got.total(), got.local) - want).abs();
                        if off > worst {
                            worst_at = (p, tau, got.total());
                        }
                        worst = worst.max(off);
                        sum += off;
                        let alone = (light(view, l, grid.grid_tau(p, extinction(p)), local) - want).abs();
                        alone_worst = alone_worst.max(alone);
                        alone_sum += alone;
                        // The two live steps alone and a plain read where they end. The powder's local depth is these
                        // two steps whatever the steps past them, so it cannot jump where they toggle.
                        let (h, q) = (0.5 * grid.reach_m, p + l * grid.reach_m);
                        let two = (extinction(p) + extinction(q)) * (h / 9.0) + (extinction(p + l * (0.5 * h)) + extinction(p + l * (1.5 * h))) * (8.0 * h / 9.0);
                        assert_eq!(got.local.to_bits(), (two * (POWDER_REACH_M / grid.reach_m)).to_bits(), "the powder's depth is the first two steps");
                        let plain = (light(view, l, two + grid.far(q)[ALL], got.local) - want).abs();
                        plain_worst = plain_worst.max(plain);
                        plain_sum += plain;
                    }
                    let n = points.len() as f32;
                    println!(
                        "{name} ({level} level), sun {elevation}: {} points; light off by {worst:.4} at most, {:.4} on average, {:.2} steps past the first two; the grid alone {alone_worst:.4} / {:.4}; two steps and a plain read {plain_worst:.4} / {:.4}; worst at {:?} (depth {:.3} against {:.3})",
                        points.len(),
                        sum / n,
                        steps as f32 / n,
                        alone_sum / n,
                        plain_sum / n,
                        worst_at.0,
                        worst_at.2,
                        worst_at.1
                    );
                    assert!(worst < 0.05, "{name} ({level}), sun {elevation}: the light is off by {worst} at {:?}", worst_at.0);
                    // A pixel's sun light: a view ray through each of the first PIXELS points, marched in 4 m steps
                    // (the share of the unshadowed sun the pixel shows), against the same march on the 5 m reference:
                    // the hybrid throughout, and past 30 % or 90 % opacity the grid alone (all eight nodes, or blended
                    // as `grid_tau`) or the first two steps with the blended read where they end. The hybrid and
                    // `grid_tau` past 90 % are held to it.
                    const PIXELS: usize = 150;
                    let eye = if name == "deck" { vec3f(0.0, 3000.0, 0.0) } else { Vec3f::default() };
                    let (h, reach) = (0.5 * grid.reach_m, grid.reach_m);
                    let pixel = |d: Vec3f, past: usize, switch: f32, depth: &dyn Fn(Vec3f, f32, usize) -> (f32, f32)| -> f32 {
                        let (ta, tb) = ((base - eye.y) / d.y, (top - eye.y) / d.y);
                        let (mut t, end) = (ta.min(tb).max(0.0), ta.max(tb));
                        let (mut seen, mut pixel) = (1.0f32, 0.0f32);
                        while t < end && seen > 0.005 {
                            let x = eye + d * (t + 2.0);
                            let here = extinction(x);
                            if here > 0.0 {
                                let (tau, local) = depth(x, here, if seen < 1.0 - switch { past } else { 0 });
                                let opacity = 1.0 - (-4.0 * here).exp();
                                pixel += seen * opacity * light(d, l, tau, local);
                                seen *= 1.0 - opacity;
                            }
                            t += 4.0;
                        }
                        pixel
                    };
                    let depth = |x: Vec3f, here: f32, past: usize| -> (f32, f32) {
                        match past {
                            0 => {
                                let got = grid.sun_tau(x, here, extinction);
                                (got.total(), got.local)
                            }
                            1 => (grid.far(x)[ALL], here * POWDER_REACH_M),
                            2 => (grid.grid_tau(x, here), here * POWDER_REACH_M),
                            _ => {
                                let q = x + l * reach;
                                let at_q = extinction(q);
                                let two = (here + at_q) * (h / 9.0) + (extinction(x + l * (0.5 * h)) + extinction(x + l * (1.5 * h))) * (8.0 * h / 9.0);
                                (two + grid.blended(&grid.far(q), at_q), two * (POWDER_REACH_M / reach))
                            }
                        }
                    };
                    let reference = |x: Vec3f, _: f32, _: usize| (fine_march(x, l, top, f32::INFINITY, extinction), fine_march(x, l, top, POWDER_REACH_M, extinction));
                    let rays: Vec<Vec3f> = points.iter().take(PIXELS).map(|&(p, _, _)| (p - eye).normalize()).collect();
                    let wanted: Vec<f32> = rays.iter().map(|&d| pixel(d, 0, 0.0, &reference)).collect();
                    let mut line = format!("  pixels ({} rays, mean {:.3}), off by at most / on average", rays.len(), wanted.iter().sum::<f32>() / rays.len() as f32);
                    for (past, switch, way) in [(0, 0.0, "the hybrid"), (1, 0.3, "past 30 % grid, all nodes"), (2, 0.3, "grid, blended"), (3, 0.3, "two steps"), (1, 0.9, "past 90 % grid, all nodes"), (2, 0.9, "grid, blended"), (3, 0.9, "two steps")] {
                        let offs: Vec<f32> = rays.iter().zip(&wanted).map(|(&d, &want)| (pixel(d, past, switch, &depth) - want).abs()).collect();
                        let most = offs.iter().cloned().fold(0.0f32, f32::max);
                        line += &format!("; {way} {most:.4} / {:.4}", offs.iter().sum::<f32>() / offs.len() as f32);
                        if past == 0 || (past == 2 && switch > 0.5) {
                            let held_to = if past == 0 { 0.05 } else { 0.06 };
                            assert!(most < held_to, "{name} ({level}), sun {elevation}: a pixel ({way}, switch {switch}) is off by {most}");
                        }
                    }
                    println!("{line}");
                }
            }
        }
        // The real Cumulus field (design §12.6 item 8, §12.8 item 1; cloud_volume_cumulus.rs): a Cumulus volume at
        // Cover 0.5, its own grid spec (the production radii), the sun 9, 30 and 60 degrees up at two azimuths. Per point:
        // the light at its sun-facing surface points within 8 km (reference T_sun 0.05-0.95). Per pixel: view rays from
        // the viewer through each, the hybrid until the ray is 90 % opaque and the grid past it (the march's switch).
        // Single samples move with any change of the field, so the population is held, not its worst sample. With them
        // the fill that leaves the paths the footprint's max pyramid calls clear is the plain fill bit for bit (both
        // levels and tau_up).
        {
            use crate::cloud_classic::downwind;
            use crate::cloud_volume_cumulus::{CloudVolume, VolumeSpec};
            use crate::cloud_volume_light::{step, VolumeMarch};
            use crate::volumetric::MIN_TRANSMITTANCE;
            use crate::{dir_from_az_el, CloudGenus, CloudLayerParams};
            let mut layer = CloudLayerParams::new(CloudGenus::Cumulus, &[], 1);
            layer.cover = Some(0.5);
            for azimuth in [200.0f32, 65.0] {
                for elevation in [9.0f32, 30.0, 60.0] {
                    let spec = VolumeSpec { layer: &layer, genus: CloudGenus::Cumulus, cover: 0.5, map_seed: 1, downwind: downwind(30.0), sun_dir: dir_from_az_el(azimuth, elevation), march_steps: 48 };
                    let volume = CloudVolume::build(&spec, false, &|| false, &four_threads).unwrap().expect("a Cumulus volume");
                    let shape = volume.shape();
                    let extinction = |p: Vec3f| shape.extinction(p);
                    let clear = |a: Vec3f, b: Vec3f| shape.span_is_clear(a, b);
                    let gspec = shape.sun_grid_spec(spec.sun_dir);
                    let grid = volume.sun_grid();
                    let (l, top, base) = (grid.frame.l, gspec.top_m, gspec.base_m);
                    if azimuth == 200.0 && elevation == 30.0 {
                        let plain = SunDepthGrid::build(&gspec, &extinction, None, false, &|| false, &four_threads).unwrap();
                        assert!(bits(&grid) == bits(&plain), "the fill that skips clear paths is the plain fill bit for bit");
                        let serial = SunDepthGrid::build(&gspec, &extinction, Some(&clear), false, &|| false, &serial_rows).unwrap();
                        assert!(bits(&grid) == bits(&serial), "the skipping fill on four threads is the serial one bit for bit");
                        let reads = |g: &SunDepthGrid| g.levels().iter().map(|l| l.3).sum::<u64>();
                        let (up, up_plain) = (TauUpGrid::build(base, top, COARSE_RADIUS_M, &extinction, Some(&clear), &|| false, &four_threads).unwrap(), TauUpGrid::build(base, top, COARSE_RADIUS_M, &extinction, None, &|| false, &four_threads).unwrap());
                        assert!(up.tau.iter().map(|t| t.to_bits()).eq(up_plain.tau.iter().map(|t| t.to_bits())), "tau_up skipping clear columns is the plain one bit for bit");
                        println!("real Cumulus, sun 30: the fill read {} points skipping clear paths against {} plain", reads(&grid), reads(&plain));
                        assert!(reads(&grid) < reads(&plain), "the fill that skips clear paths reads no fewer points");
                    }
                    let body = gspec.body_extinction;
                    let candidate = |k: i32| {
                        let r = |salt: u32| hash01(k, salt as i32, 11, 0x0c0f_fee5);
                        vec3f(16_000.0 * (r(1) - 0.5), base + (top - base) * r(2), 16_000.0 * (r(3) - 0.5))
                    };
                    let points = surface_points(POPULATION, l, top, FRINGE_SHARE * body, &extinction, &candidate);
                    assert!(points.len() == POPULATION, "real Cumulus, sun {elevation} at {azimuth}: {} points", points.len());
                    let eye = vec3f(0.0, 2.0, 0.0);
                    // The pixels: 4 m steps from where the ray meets the shell, flat.
                    let pixel = |d: Vec3f, depth: &dyn Fn(Vec3f, f32, f32) -> (f32, f32)| -> f32 {
                        let (ta, tb) = ((base - eye.y) / d.y, (top - eye.y) / d.y);
                        let (mut t, end) = (ta.max(0.0), tb);
                        let (mut seen, mut pixel) = (1.0f32, 0.0f32);
                        while t < end && seen > 0.005 {
                            let x = eye + d * (t + 2.0);
                            let here = extinction(x);
                            if here > 0.0 {
                                let (tau, local) = depth(x, here, seen);
                                let opacity = 1.0 - (-4.0 * here).exp();
                                pixel += seen * opacity * light(d, l, tau, local);
                                seen *= 1.0 - opacity;
                            }
                            t += 4.0;
                        }
                        pixel
                    };
                    let reference = |x: Vec3f, _: f32, _: f32| (fine_march(x, l, top, f32::INFINITY, &extinction), fine_march(x, l, top, POWDER_REACH_M, &extinction));
                    let march = |x: Vec3f, here: f32, seen: f32| {
                        if seen >= 0.1 {
                            let got = grid.sun_tau(x, here, &extinction);
                            (got.total(), got.local)
                        } else {
                            (grid.grid_tau(x, here), here * POWDER_REACH_M)
                        }
                    };
                    // Per point (the light at the point) and per pixel (a view ray from the viewer through it: the hybrid
                    // until the ray is 90 % opaque, the grid past it, the march's switch), against the reference.
                    let offs: Vec<(f32, f32)> = spread_map(points.len(), &|i| {
                        let (p, tau, local) = points[i];
                        let d = (p - eye).normalize();
                        let got = grid.sun_tau(p, extinction(p), &extinction);
                        ((light(d, l, got.total(), got.local) - light(d, l, tau, local)).abs(), (pixel(d, &march) - pixel(d, &reference)).abs())
                    });
                    let point = Spread::of(offs.iter().map(|o| o.0), &points);
                    let pixels = Spread::of(offs.iter().map(|o| o.1), &points);
                    // The renderer's own march (`CloudVolume::march`: its lattice, halved entries, its steps in cloud,
                    // blue-noise start, detail and the switch at 90 % opacity) at the bake widths 512 and 1024,
                    // against a 2 m march through the same density at that width (its law's, `law_angle`), the sun's
                    // depth from the 5 m reference at both ends of each step: the sun's share a pixel shows (the light
                    // law's lobes, as `light`, and the heap's diffusion, which reads T_d of the whole depth: the reference
                    // marches to the shell's top, not only to a depth of 40 as before the diffusion, which put 0.0025 on
                    // the mean at sun 9), over RENDERER_RAYS of the population's rays spread over its point errors.
                    let sun_share = |m: &VolumeMarch, d: Vec3f| {
                        let ray = RayLight::new(d.dot(l), l.y, FORWARD_SHARE, LightFamily::Heap);
                        (FORWARD_SHARE * (m.forward_low + m.forward_high) + (1.0 - FORWARD_SHARE) * (m.back_low + m.back_high)) / ray.sun_light(0.0, 1.0e3)
                    };
                    let fine_pixel = |d: Vec3f, width: usize| -> f32 {
                        const H: f32 = 2.0;
                        let Some((near, far)) = shape.ray_span(d) else {
                            return 0.0;
                        };
                        let ray = RayLight::new(d.dot(l), l.y, FORWARD_SHARE, LightFamily::Heap);
                        let angle = crate::cloud_volume_cumulus::law_angle(width);
                        let sun_at = |t: f32| {
                            let p = shape.on_ray(d, angle, t).2;
                            (fine_march(p, l, top, f32::INFINITY, &extinction), fine_march(p, l, top, POWDER_REACH_M, &extinction))
                        };
                        let (mut m, mut t, mut kept) = (VolumeMarch::CLEAR, near, None::<(f32, (f32, f32))>);
                        while t < far && m.transmittance > MIN_TRANSMITTANCE {
                            let (full, c, _) = shape.on_ray(d, angle, t + 0.5 * H);
                            if full > 0.0 {
                                let a = match kept {
                                    Some((at, v)) if at == t => v,
                                    _ => sun_at(t),
                                };
                                let b = sun_at(t + H);
                                step(&mut m, &ray, full * shape.sigma(), H, 0.5 * (a.0 + b.0), (b.0 - a.0) / H, 0.0, 0.5 * (a.1 + b.1), c.h01, t, 1.0);
                                kept = Some((t + H, b));
                            }
                            t += H;
                        }
                        sun_share(&m, d)
                    };
                    let mut by_point: Vec<usize> = (0..points.len()).collect();
                    by_point.sort_by(|&a, &b| offs[a].0.total_cmp(&offs[b].0));
                    let chosen: Vec<(Vec3f, f32, f32)> = by_point.iter().rev().step_by(points.len() / RENDERER_RAYS).take(RENDERER_RAYS).map(|&i| points[i]).collect();
                    let mut renderer = Vec::new();
                    for width in [512usize, 1024] {
                        let errs = spread_map(chosen.len(), &|k| {
                            let d = (chosen[k].0 - eye).normalize();
                            let mut out = [VolumeMarch::CLEAR];
                            volume.march(d, width, &[], &mut out);
                            (sun_share(&out[0], d) - fine_pixel(d, width)).abs()
                        });
                        renderer.push((width, Spread::of(errs.into_iter(), &chosen)));
                    }
                    // The gate (design §12.6 item 8, §12.8 item 1): per sun, the 99th percentile and the mean of the point
                    // and pixel errors over the population, and of the renderer's march over its rays, held at what the
                    // landed field reaches (HELD), so they cannot slip; the gate's own 0.05 a point and 0.03 a pixel
                    // printed beside them, and the single worst samples with where they sit.
                    let held = HELD.iter().find(|h| h.0 == azimuth && h.1 == elevation).expect("a held row per sun");
                    let margin = |got: f32, gate: f32| format!("{} the gate {gate} by {:.4}", if got <= gate { "meets" } else { "misses" }, (got - gate).abs());
                    println!("real Cumulus, sun {elevation} at {azimuth}: {} points: {}; the 99th percentile {}", points.len(), point.line("point"), margin(point.p99, 0.05));
                    println!("real Cumulus, sun {elevation} at {azimuth}: {} pixels: {}; the 99th percentile {}", points.len(), pixels.line("pixel"), margin(pixels.p99, 0.03));
                    for (width, r) in &renderer {
                        println!("real Cumulus, sun {elevation} at {azimuth}: the renderer's march at {width}, {} rays: {}", chosen.len(), r.line("pixel"));
                    }
                    let checks = [
                        ("point p99", point.p99, held.2),
                        ("point mean", point.mean, held.3),
                        ("pixel p99", pixels.p99, held.4),
                        ("pixel mean", pixels.mean, held.5),
                        ("renderer 512 p99", renderer[0].1.p99, held.6),
                        ("renderer 512 mean", renderer[0].1.mean, held.7),
                        ("renderer 1024 p99", renderer[1].1.p99, held.6),
                        ("renderer 1024 mean", renderer[1].1.mean, held.7),
                    ];
                    let before = PRE_6B.iter().find(|h| h.0 == azimuth && h.1 == elevation).expect("a pre-6b row per sun");
                    let firsts = [before.2, before.3, before.4, before.5, before.6, before.7, before.6, before.7];
                    let over: Vec<String> = checks.iter().zip(firsts).map(|((what, got, _), first)| format!("{what} x{:.2}", got / first)).collect();
                    println!("real Cumulus, sun {elevation} at {azimuth}: over the pre-6b lock: {}", over.join(", "));
                    for (what, got, held_to) in checks {
                        assert!(got <= held_to, "real Cumulus, sun {elevation} at {azimuth}: the {what} is {got}, held to {held_to}");
                    }
                }
            }
        }
        // A fill on four threads is the serial one bit for bit, tau_up's too; a cancel stops it, and so does a runner
        // that skips a row.
        let spec = SunGridSpec { sun: sun_at(30.0), base_m: 1200.0, top_m: 2800.0, cell_m: SUN_CELL_M, fine_radius_m: 5000.0, coarse_radius_m: 10_000.0, body_extinction: CUMULUS_SIGMA / 3.0 };
        let serial = SunDepthGrid::build(&spec, &field, None, false, &|| false, &serial_rows).unwrap();
        let spread = SunDepthGrid::build(&spec, &field, None, false, &|| false, &four_threads).unwrap();
        assert!(bits(&serial) == bits(&spread), "a fill on four threads is the serial one bit for bit");
        assert!(SunDepthGrid::build(&spec, &field, None, false, &|| true, &serial_rows).is_err());
        let skipping = |n: usize, row: &(dyn Fn(usize) + Sync)| (0..n).filter(|j| j % 2 == 0).for_each(row);
        assert!(SunDepthGrid::build(&spec, &field, None, true, &|| false, &skipping).is_err());
        let (up, up_spread) = (TauUpGrid::build(1200.0, 2800.0, 10_000.0, &field, None, &|| false, &serial_rows).unwrap(), TauUpGrid::build(1200.0, 2800.0, 10_000.0, &field, None, &|| false, &four_threads).unwrap());
        assert!(up.tau.iter().map(|t| t.to_bits()).eq(up_spread.tau.iter().map(|t| t.to_bits())), "tau_up on four threads is the serial one bit for bit");
        // tau_up is the column above a point: through the near cloud's middle, against a 5 m march up.
        let p = vec3f(near_cloud.x, 1600.0, near_cloud.y);
        let want: f32 = (0..240).map(|i| field(vec3f(p.x, 1602.5 + 5.0 * i as f32, p.z)) * 5.0).sum();
        assert!((up.tau_up(p) - want).abs() <= 0.05 * want, "tau_up {} against {want}", up.tau_up(p));
        // The cache: one level per key, the held grid takes the coarse level a released one left, and the fine one.
        let key = 0x5eed_c0de_u64;
        let released = SunDepthGrid::shared(&spec, key, 0.5, 3, &field, None, false, &|| false, &serial_rows).unwrap();
        let held = SunDepthGrid::shared(&spec, key, 0.5, 3, &field, None, true, &|| -> bool { panic!("a kept level is not filled again") }, &serial_rows).unwrap();
        assert!(Arc::ptr_eq(&released.coarse, &held.coarse) && held.has_fine());
        let moved = SunDepthGrid::shared(&SunGridSpec { sun: sun_at(31.0), ..spec }, key, 0.5, 3, &field, None, true, &|| false, &serial_rows).unwrap();
        assert!(!moved.has_fine(), "a held grid for another sun fills only its coarse level");
    }
}
