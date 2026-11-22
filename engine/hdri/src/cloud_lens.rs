//! A Lenticular (design §2.1's Lens class, §4.3): smooth lens plates stacked over each other at (azimuth, distance),
//! marched as a placed volume (`cloud_placed::PlacedVolume`) where a ray meets their bounds.
//!
//! - **The plates** ([`Lens`]), in the lens' own frame (along its wind, up, across it): Plates plates, each a lens a
//!   Length across the wind and a Width along it, Plate thickness thick at its middle and thinning to a crisp rim as
//!   (1 - r^2)^LENS_POWER, its domed top TOP_SHARE of that over its middle and its flatter underside the rest; each
//!   one up stands a Plate gap over the one under it and is PLATE_SHRINK of its size (its thickness the root of that),
//!   moved off the stack's axis by up to PLATE_DRIFT of the lowest plate's size. Cover scales them by its root (design
//!   §3.1) and builds nothing at 0.
//! - **The density**: the plates' distance in metres (vertical over a plate, outward past its rim), moved by the shape
//!   tile's slow undulations (Smoothness 1 none) and by Striations' rings (concentric ridges RINGS a radius, faint
//!   bands along a plate seen from the side), then a skin SKIN_LEAST to SKIN_MOST deep with Smoothness, never under 1.5
//!   texel footprints. The GPU export's `ln_dens` (makepad-cloud-gpu, export/lenticular.rs) is the same function.
//! - **Light**: the shared law, as a storm's (its march, `PlacedVolume::march`, and the export shader's lens march).

use makepad_draw::*;
use crate::cloud_placed::{smoothstep, Lod, Read, Tiles};
use crate::cloud_volume_cumulus::CUMULUS_SIGMA;
use crate::noise::hash_u32;
use crate::volumetric::shell_distance;
use crate::CloudLayerParams;

/// The most plates a lens stacks (`LenticularParams::plates`, rounded).
pub const PLATES: usize = 6;
/// A plate's thickness falls from its middle to its rim as (1 - r^2)^LENS_POWER (an ellipsoid's 0.5 drew a round,
/// blunt rim, and 0.7 still a saucer's; a lens thins to a sharp edge that its skin makes translucent).
const LENS_POWER: f32 = 1.1;
/// The share of a plate's thickness over its middle plane: a domed top over a flatter underside.
const TOP_SHARE: f32 = 0.6;
/// Each plate up is PLATE_SHRINK of the one under it across and along, give or take PLATE_VARY of that by the seed (so a
/// plate may be longer than the one under it), its thickness the root of that; round 3's even 0.85 stacked perfect
/// centred saucers.
const PLATE_SHRINK: f32 = 0.85;
const PLATE_VARY: f32 = 0.3;
/// Each plate's length (across the wind) is besides LENGTH_LEAST to LENGTH_MOST of its size, by the seed: plates of
/// clearly different lengths (round 4's plates differed by their size alone and read as a stack of saucers).
const LENGTH_LEAST: f32 = 0.6;
const LENGTH_MOST: f32 = 1.5;
/// How far the plates stand off the stack's axis: PLATE_DRIFT of the lowest plate's half width along the wind and
/// PLATE_DRIFT_ACROSS of its half length across (round 3's 0.25 and 0.125 stood them centred).
const PLATE_DRIFT: f32 = 0.45;
const PLATE_DRIFT_ACROSS: f32 = 0.25;
/// A plate is DOWNWIND_STRETCH times longer downwind of its middle than upwind (a lens is blunt where the air comes in
/// and drawn out where it leaves), and over its downwind rim, from WISP_FROM of its radius, it frays into wisps: the
/// shape tile stretched along the wind (WISP_TILE_M along, up, across) cuts its density into strands by up to WISP_CUT
/// and thins it by up to WISP_THIN, and its skin there is WISP_SOFT times softer. (Moving the surface up and down by the
/// strands, as this round's first try did, stood them on the plates' tops as fur; cut through, they combed the rims.)
const DOWNWIND_STRETCH: f32 = 1.6;
const WISP_FROM: f32 = 0.6;
const WISP_THIN: f32 = 0.6;
const WISP_SOFT: f32 = 3.0;
const WISP_CUT: f32 = 0.5;
const WISP_TILE_M: [f32; 3] = [4000.0, 500.0, 300.0];
/// Smoothness 0 moves the surface by up to ROUGH_SHARE of the lowest plate's thickness, over the shape tile read
/// ROUGH_TILE_M across and ROUGH_TILE_UP_M up; Smoothness 1 not at all.
const ROUGH_SHARE: f32 = 0.4;
const ROUGH_TILE_M: f32 = 3000.0;
const ROUGH_TILE_UP_M: f32 = 1500.0;
/// Striations: RINGS concentric ridges from a plate's middle to its rim, each RING_SHARE of the plate's thickness at
/// Striations 1, wandering by the shape tile read RING_WANDER_M along the plate; and the plate thinner between them by
/// RING_THIN (the translucent rims show the bands).
const RINGS: f32 = 5.0;
const RING_SHARE: f32 = 0.3;
const RING_WANDER_M: f32 = 6000.0;
const RING_THIN: f32 = 0.6;
/// The skin: SKIN_LEAST m deep at Smoothness 0 (a crisp rim), SKIN_MOST at 1 (at 48 m the plates' edges and the gaps
/// between them were hard lines).
const SKIN_LEAST: f32 = 8.0;
const SKIN_MOST: f32 = 110.0;
/// A lens' extinction at Density 1, a share of the Cumulus' (a thin smooth plate of fine droplets: at 0.6 a 250 m
/// plate was about 18 optical depths through, and its underside, all a side view shows of a plate 5 km up, came out
/// mid grey where the photos' plates are white).
const SIGMA_SHARE: f32 = 0.35;
/// What the bounds keep round the plates, metres.
const BOUNDS_MARGIN_M: f32 = 200.0;
/// A march may move this share of the plates' distance where it is safe (the distance is vertical over a plate: near
/// its rim the surface is steep, so the true distance is shorter).
const SAFE_SHARE: f32 = 0.5;

/// A lens of plates, its numbers as both marches and the GPU's uniforms read them.
#[derive(Clone, Copy, Debug)]
pub struct Lens {
    /// The stack's middle on the ground (x east, z south), metres.
    centre: Vec2f,
    /// Downwind (x, z), a unit.
    wind: Vec2f,
    /// The lowest plate's underside and the top plate's top, metres.
    pub(crate) base: f32,
    pub(crate) top: f32,
    /// The lowest plate's half width (along the wind), half length (across) and thickness, metres.
    size: [f32; 3],
    /// How many plates (1..=PLATES).
    plates: usize,
    /// Each plate: its middle's offset (along, across), its middle plane's height, its scale (PLATE_SHRINK^k).
    plate: [[f32; 4]; PLATES],
    /// Each plate's length besides its scale (LENGTH_LEAST..LENGTH_MOST).
    length: [f32; PLATES],
    /// How far the shape tile moves the surface, metres; Striations (0..1); the skin, metres.
    rough_m: f32,
    striations: f32,
    skin_m: f32,
    /// Extinction per metre at density 1.
    pub(crate) sigma: f32,
    /// Where the noises are read in the shape tile: the roughness, the rings' wander, the downwind wisps.
    place: [Vec3f; 3],
}

impl Lens {
    /// The lens of `layer` at its effective `cover` (> 0), its seed's `stream` and its `wind`.
    pub fn of(layer: &CloudLayerParams, cover: f32, stream: u32, wind: Vec2f) -> Lens {
        let b = layer.lenticular_params();
        let unit = |k: u32| (hash_u32(stream ^ k.wrapping_mul(0x9e37_79b9)) >> 8) as f32 * (1.0 / 16_777_216.0);
        let scale = cover.clamp(0.0, 1.0).sqrt();
        let (half_w, half_l, thick) = (500.0 * b.width_km * scale, 500.0 * b.length_km * scale, b.thickness_m * scale);
        let plates = (b.plates.round() as usize).clamp(1, PLATES);
        let mut plate = [[0.0f32; 4]; PLATES];
        let mut scales = [1.0f32; PLATES];
        let mut length = [1.0f32; PLATES];
        let mut mid = b.base_m + (1.0 - TOP_SHARE) * thick;
        for (k, p) in plate.iter_mut().enumerate().take(plates) {
            let s = if k == 0 { 1.0 } else { PLATE_SHRINK.powi(k as i32) * (1.0 + PLATE_VARY * (2.0 * unit(30 + k as u32) - 1.0)) };
            if k > 0 {
                let under = thick * scales[k - 1].sqrt();
                mid += TOP_SHARE * under + b.gap_m + (1.0 - TOP_SHARE) * thick * s.sqrt();
            }
            scales[k] = s;
            length[k] = LENGTH_LEAST + (LENGTH_MOST - LENGTH_LEAST) * unit(40 + k as u32);
            let drift = if k == 0 { 0.0 } else { 1.0 };
            *p = [drift * PLATE_DRIFT * half_w * (2.0 * unit(10 + k as u32) - 1.0), drift * PLATE_DRIFT_ACROSS * half_l * (2.0 * unit(20 + k as u32) - 1.0), mid, s];
        }
        let last = plate[plates - 1];
        let rough_m = ROUGH_SHARE * thick * (1.0 - b.smoothness);
        let d = 1000.0 * b.distance_km;
        let a = b.azimuth_deg.to_radians();
        let place = |k: u32| vec3f(unit(100 + 3 * k), unit(101 + 3 * k), unit(102 + 3 * k));
        Lens {
            centre: vec2f(d * a.sin(), -d * a.cos()),
            wind,
            base: b.base_m,
            top: last[2] + TOP_SHARE * thick * last[3].sqrt(),
            size: [half_w, half_l, thick],
            plates,
            plate,
            length,
            rough_m,
            striations: b.striations,
            skin_m: SKIN_LEAST + (SKIN_MOST - SKIN_LEAST) * b.smoothness,
            sigma: CUMULUS_SIGMA * SIGMA_SHARE * b.density,
            place: [place(0), place(1), place(2)],
        }
    }

    /// How far the noise moves the surface out, metres.
    fn reach(&self) -> f32 {
        self.rough_m + RING_SHARE * self.size[2] * self.striations + 0.25 * WISP_SOFT * self.skin_m
    }

    /// The top of everything it draws.
    pub(crate) fn ceiling(&self) -> f32 {
        self.top + self.reach() + BOUNDS_MARGIN_M
    }

    /// The floor under everything it draws.
    pub(crate) fn floor(&self) -> f32 {
        (self.base - self.reach() - BOUNDS_MARGIN_M).max(0.0)
    }

    /// The upright cylinder that holds it: (x, z, radius).
    pub(crate) fn bounds(&self) -> [f32; 3] {
        let reach = self.reach() + BOUNDS_MARGIN_M;
        let r = self.plate[..self.plates].iter().zip(&self.length).map(|(p, l)| (p[0] * p[0] + p[1] * p[1]).sqrt() + (DOWNWIND_STRETCH * self.size[0]).max(self.size[1] * l) * p[3]).fold(0.0f32, f32::max);
        [self.centre.x, self.centre.y, r + reach]
    }

    /// The local frame of a point: (along the wind, up, across).
    #[inline]
    fn local(&self, p: Vec3f) -> (f32, f32, f32) {
        let (qx, qz) = (p.x - self.centre.x, p.z - self.centre.y);
        (qx * self.wind.x + qz * self.wind.y, p.y, qz * self.wind.x - qx * self.wind.y)
    }

    /// The plates' distance at (along, up, across), metres (vertical over a plate, outward past its rim; below 0
    /// inside), the nearest plate's radius share (0 at its middle, 1 at its rim) and how far downwind of its middle the
    /// point is (0 upwind, 1 from a fifth of its radius on). A plate's outline is a lens pointed at both ends of its
    /// length (b = 1 - a^2 for a = across, b = along, in shares: an ellipse drew rounded ends), so its thickness tapers
    /// to its tips.
    #[inline]
    fn plates_distance(&self, u: f32, y: f32, v: f32) -> (f32, f32, f32) {
        let (mut best, mut rho_best, mut down_best) = (f32::INFINITY, 1.0f32, 0.0f32);
        for (p, l) in self.plate[..self.plates].iter().zip(&self.length) {
            let (half_w, half_l, thick) = (self.size[0] * p[3], self.size[1] * p[3] * l, self.size[2] * p[3].sqrt());
            let along = if u > p[0] { DOWNWIND_STRETCH * half_w } else { half_w };
            let (du, dv) = ((u - p[0]) / along, (v - p[1]) / half_l);
            let rho2 = dv * dv + du * du / (1.0 - dv * dv).max(1.0e-4);
            let f = (1.0 - rho2).max(0.0).powf(LENS_POWER);
            let dy = y - p[2];
            let vertical = if dy > 0.0 { dy - TOP_SHARE * thick * f } else { -dy - (1.0 - TOP_SHARE) * thick * f };
            let rho = rho2.sqrt();
            // the outward distance to the outline, never over the true one (a march's safe move: rho grows without
            // bound beside a tip): to the outline's tangent over the point, past the tips to the tip's tangents
            let (a, b) = (dv.abs(), du.abs());
            let rim = if a < 1.0 { (b - (1.0 - a * a)) / (1.0 + 4.0 * a * a).sqrt() } else { (a - 1.0).max((b + 2.0 * a - 2.0) / 5.0f32.sqrt()) } * half_w.min(half_l);
            let e = vertical.max(rim);
            if e < best {
                best = e;
                rho_best = rho;
                down_best = smoothstep(0.0, 0.2, du);
            }
        }
        (best, rho_best, down_best)
    }

    /// The density at `p` read for a texel `footprint_m` wide at `lod`. The GPU's `ln_dens` mirrors it.
    pub(crate) fn read(&self, tiles: &Tiles, p: Vec3f, footprint_m: f32, lod: Lod) -> Read {
        let (u, y, v) = self.local(p);
        let h01 = ((y - self.base) / (self.top - self.base).max(1.0)).clamp(0.0, 1.0);
        let (e, rho, down) = self.plates_distance(u, y, v);
        let near = e - (self.reach() + 2.0 * self.skin_m.max(1.5 * footprint_m));
        if near > 0.0 {
            return Read { density: 0.0, h01, free_m: SAFE_SHARE * near, safe: true, full: 0.0 };
        }
        let mut surf = e;
        // the slow undulations (Smoothness 1: none) and Striations' rings, with the view and the light's near steps
        let mut ring = 0.5;
        if lod >= Lod::Near {
            let shape = &tiles.noise.shape;
            if self.rough_m > 0.0 {
                let o = self.place[0];
                let n = shape.sample(u / ROUGH_TILE_M + o.x, y / ROUGH_TILE_UP_M + o.y, v / ROUGH_TILE_M + o.z);
                surf -= self.rough_m * (2.0 * n - 1.0);
            }
            if self.striations > 0.0 {
                let o = self.place[1];
                let wander = shape.sample(u / RING_WANDER_M + o.x, o.y, v / RING_WANDER_M + o.z);
                ring = 0.5 + 0.5 * (std::f32::consts::TAU * (RINGS * rho + wander)).cos();
                let fade = 1.0 - smoothstep(0.8, 1.0, rho);
                surf -= self.striations * RING_SHARE * self.size[2] * (2.0 * ring - 1.0) * fade;
            }
        }
        // the downwind rim frays into wisps along the wind: strands, thinner, a softer skin
        let wisp = down * smoothstep(WISP_FROM, 1.0, rho);
        let mut strands = 1.0;
        if wisp > 0.001 && lod >= Lod::Near {
            let o = self.place[2];
            let n = tiles.noise.shape.sample(u / WISP_TILE_M[0] + o.x, y / WISP_TILE_M[1] + o.y, v / WISP_TILE_M[2] + o.z);
            strands = (1.0 - WISP_THIN * wisp) * (1.0 - WISP_CUT * wisp * (1.0 - smoothstep(0.3, 0.7, n)));
        }
        let skin = (self.skin_m * (1.0 + (WISP_SOFT - 1.0) * wisp)).max(1.5 * footprint_m);
        let density = (0.25 - surf / skin).clamp(0.0, 1.0) * (1.0 - RING_THIN * self.striations * (1.0 - ring)) * strands;
        if density > 0.0 {
            return Read { density, h01, free_m: 0.0, safe: false, full: 1.0 };
        }
        Read { density: 0.0, h01, free_m: SAFE_SHARE * surf.max(0.0), safe: false, full: 0.0 }
    }

    /// Where the unit direction `d` (d.y > 0) from the eye is inside the bounds: (near, far) metres, None where it
    /// misses them.
    pub(crate) fn span(&self, d: Vec3f) -> Option<(f32, f32)> {
        let c = self.bounds();
        let a = d.x * d.x + d.z * d.z;
        let b = d.x * c[0] + d.z * c[1];
        let cc = c[0] * c[0] + c[1] * c[1] - c[2] * c[2];
        let (t0, t1) = if a > 1.0e-9 {
            let disc = b * b - a * cc;
            if disc < 0.0 {
                return None;
            }
            let s = disc.sqrt();
            ((b - s) / a, (b + s) / a)
        } else if cc > 0.0 {
            return None;
        } else {
            (0.0, f32::INFINITY)
        };
        let (lo, hi) = (shell_distance(d.y, self.floor().max(2.6)), shell_distance(d.y, self.ceiling()));
        let (near, far) = (t0.max(lo).max(0.0), t1.min(hi));
        (far > near).then_some((near, far))
    }

    /// The upload's uniforms (the Lenticular export shader's section reads them by these names).
    pub(crate) fn uniforms(&self, out: &mut crate::cloud_volume_gpu::UploadBuilder) {
        let c = self.bounds();
        out.uniform("ln_a", [self.centre.x, self.centre.y, self.base, self.top]);
        out.uniform("ln_b", [self.wind.x, self.wind.y, self.plates as f32, self.sigma]);
        out.uniform("ln_c", [self.size[0], self.size[1], self.size[2], TOP_SHARE]);
        out.uniform("ln_d", [self.rough_m, self.striations, self.skin_m, self.reach()]);
        out.uniform("ln_e", [LENS_POWER, RINGS, RING_SHARE, RING_THIN]);
        out.uniform("ln_f", [ROUGH_TILE_M, ROUGH_TILE_UP_M, RING_WANDER_M, SAFE_SHARE]);
        out.uniform("ln_g", [self.place[0].x, self.place[0].y, self.place[0].z, 0.0]);
        out.uniform("ln_h", [self.place[1].x, self.place[1].y, self.place[1].z, 0.0]);
        out.uniform("ln_k", [c[0], c[1], c[2], self.ceiling()]);
        out.uniform("ln_l", [self.floor(), 0.0, 0.0, 0.0]);
        out.uniform("ln_q", [DOWNWIND_STRETCH, WISP_THIN, WISP_FROM, WISP_SOFT]);
        out.uniform("ln_r", [WISP_TILE_M[0], WISP_TILE_M[1], WISP_TILE_M[2], WISP_CUT]);
        out.uniform("ln_s", [self.place[2].x, self.place[2].y, self.place[2].z, 0.0]);
        out.uniform("ln_u", [self.length[0], self.length[1], self.length[2], self.length[3]]);
        out.uniform("ln_v", [self.length[4], self.length[5], 0.0, 0.0]);
        let names = ["ln_p0", "ln_p1", "ln_p2", "ln_p3", "ln_p4", "ln_p5"];
        for (k, name) in names.into_iter().enumerate() {
            out.uniform(name, self.plate[k]);
        }
    }

    /// Every number it draws from, for keys.
    pub(crate) fn fields(&self) -> Vec<f32> {
        let mut f = vec![self.centre.x, self.centre.y, self.wind.x, self.wind.y, self.base, self.top, self.plates as f32, self.rough_m, self.striations, self.skin_m, self.sigma];
        f.extend_from_slice(&self.size);
        for p in &self.plate {
            f.extend_from_slice(p);
        }
        f.extend_from_slice(&self.length);
        for p in &self.place {
            f.extend_from_slice(&[p.x, p.y, p.z]);
        }
        f
    }
}
