//! The deck of the four kept looks on the CPU (cloud_gpu's export/deck.rs draws it; the look runner's rounds of
//! 2026-11-04/05, the owner's picks): the Stratocumulus (its low-sun look, round 50, a turbulence deck; its high-sun
//! look, round 22, a cell deck), the Altocumulus (ac2) and the Cumulus (cu3) are one density with each type's numbers
//! ([`DeckLook`]). This is the GPU's twin, the same field from the same hash (`noise::hash3_u32`, the shader's `tbh`),
//! so Cover's solve, the sun's grids and the fallback march see what the GPU draws.
//!
//! - **The turbulence deck** ([`DeckKind::Turbulence`]): a turbulence sum of |gradient noise| (five octaves, lacunarity
//!   2.6434, gain 0.5; the first `fold` octaves fold at zero, their creases the gaps between cells, the finer ones signed
//!   bumps round the same mean), plus broad masses that thin and thicken it, lowered toward both faces by the vertical
//!   profile; the density is the field over a soft cut (the solved threshold), its water growing with height, with a
//!   fine texture, the cells drawn out along the wind (rolls). The cut rises toward the sun (the deck opens there, as
//!   in the real sky 103), and a veil of thin cloud fills the gaps away from the sun, faded far away.
//! - **The cell deck** ([`DeckKind::Cells`]): Worley billows (three octaves) over value-noise masses (three octaves),
//!   the top standing from `top_fall` of the layer where the masses are weak to all of it where strong, the field
//!   lowered toward both faces, over the cut by the ramp; the full reads eroded at their edges by billowy detail, the
//!   inside modulated by it, the underside hanging in lumps.
//! - **The footprint grid** holds an upper bound of the field over its column: the maximum over NH heights of the
//!   octaves the grid can hold (a period of at least GRID_PERIODS of its cells), plus what the finer octaves can add
//!   over their mean and MARGIN for the heights between. Where it passes the threshold cloud may stand (the max
//!   pyramid's skips, the walk's envelope); the cloud itself is the field over the cut (the threshold), the grid only
//!   says where to look. The quantile tables are measured on the field's own column maximum (`column_max`), and
//!   Cover's solve measures the marched density.
//! - **The light** is the slab family's through the column (cloud_volume_light.rs), each point's own dome the column
//!   over it read coarsely (two octaves, three reads leaning out sideways by half the layer: diffused light spreads
//!   about the thickness; the cell deck straight up), `Cheap::up`. On the GPU the deck's section holds the shares and
//!   its own sun march; the slab share goes up as `sc_r`.

use super::*;
use crate::cloud_volume_gpu::UploadBuilder;
use crate::noise::hash3_u32;

/// Which density a deck is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum DeckKind {
    Turbulence,
    Cells,
}

/// A type's numbers for the deck (the runner's parameter rows at the kept look; cloud_gpu's `tb_*` uniforms).
#[derive(Clone, Copy, Debug)]
pub(crate) struct DeckLook {
    pub kind: DeckKind,
    /// The cells' cycles a km, the masses' cycles a km and their strength.
    pub cells_per_km: f32,
    pub mass_per_km: f32,
    pub mass: f32,
    /// The cut's edge (field units), the ramp (the field over the cut at density 1), the vertical squash of the cells
    /// (1: cells as tall as wide), the profile's strength (how far the field is lowered toward the faces).
    pub cut_edge: f32,
    pub ramp: f32,
    pub squash: f32,
    pub profile: f32,
    /// The base's rise (of the layer), the top's fall from (the cell deck: the top's lowest stand), the cut's rise toward
    /// the sun over how many km (0: no edge).
    pub base_rise: f32,
    pub top_fall: f32,
    pub sun_rise: f32,
    pub sun_km: f32,
    /// The veil: its density (a share of sigma; 0 off), the field band under the cut it fills, its noise's cycles a km,
    /// the share it covers.
    pub veil: f32,
    pub veil_band: f32,
    pub veil_per_km: f32,
    pub veil_share: f32,
    /// The water at the base (a share of the full density; 0: the flat profile), full at this height of the layer; the
    /// rolls' stretch along the wind (1 none); the texture's share.
    pub water_base: f32,
    pub water_full: f32,
    pub stretch: f32,
    pub texture: f32,
    /// The far fade's reach (m), how many octaves fold.
    pub far_from_m: f32,
    pub far_to_m: f32,
    pub fold: f32,
    /// The cell deck's erosion at the edges, its inside's modulation by the detail, its underside's lift (of the local
    /// top).
    pub erosion: f32,
    pub inside: f32,
    pub lift: f32,
    /// Its extinction a metre at Density 1 (the runner's density, /m), and the light's forward lobe.
    pub sigma: f32,
    pub forward_g: f32,
}

/// How many heights the footprint's column maximum is read at, and its margin (field units) for the field between them.
const NH: usize = 7;
const MARGIN: f32 = 0.01;
/// The finest octave the footprint grid holds has a period of at least this many of its 66 m cells (finer ones are
/// bounded instead), and how far an octave's term can stand over its mean, a share of its amplitude (|n| of the
/// gradient noise reaches about 0.66 over its mean 0.16, a signed term 0.8 n about 0.53).
const GRID_PERIODS: f32 = 4.0;
const GRID_CELL_M: f32 = 66.40625;
const OCTAVE_EXCESS: f32 = 0.66;
/// The soak on the export march (`sc_r.z`, the look runner's): the sun diffused through the cloud, SOAK of the sun's light
/// times its height (at least 0.05), the deck's light where the octaves leave nothing (a thick deck under a low sun, the
/// faces turned from the sun). The CPU fallback lights a deck with the slab family's diffuse share instead.
const SOAK: f32 = 1.0;
/// The shape key's part.
const DECK_KEY_PART: f32 = 29.0;
/// The CPU march's lattice in air is never coarser than this, metres: a deck is a wide sheet.
const LATTICE_M: f32 = 80.0;

impl DeckLook {
    /// The cell deck's bound past its sampled heights and the grid's spline (its Worley billows are a few grid cells
    /// across; 0 for the turbulence deck, whose bound is `grid_octaves`).
    fn cells_margin(&self) -> f32 {
        if self.kind == DeckKind::Cells { 0.12 } else { 0.0 }
    }

    /// The octaves of the turbulence the footprint grid holds (their period at least GRID_PERIODS of its cells), and
    /// how far the octaves it leaves out can raise the field over their mean.
    fn grid_octaves(&self) -> (u32, f32) {
        if self.kind == DeckKind::Cells {
            return (5, 0.0);
        }
        let finest = 1000.0 / (GRID_PERIODS * GRID_CELL_M);
        let (mut held, mut excess, mut f, mut a) = (0u32, 0.0f32, self.cells_per_km, 0.5f32);
        for o in 0..5u32 {
            if f <= finest && held == o {
                held += 1;
            } else {
                excess += a * OCTAVE_EXCESS;
            }
            f *= 2.6434;
            a *= 0.5;
        }
        (held.max(1), excess)
    }
}

/// What a deck volume adds to its `VolumeShape`: its look, its seed and place in the noise, its wind frame and the sun.
#[derive(Clone)]
pub(crate) struct DeckShape {
    look: DeckLook,
    seed: u32,
    /// The deck's offset in the noise, km.
    offset: Vec3f,
    /// The wind's direction on the plane and the drift along it, metres.
    down: Vec2f,
    drift_m: f32,
    /// The sun's direction on the plane (unit; 0 with no edge toward the sun).
    sun_xz: Vec2f,
}

use crate::patterns::smooth as ss;

#[inline]
fn floor3(p: Vec3f) -> Vec3f {
    vec3f(p.x.floor(), p.y.floor(), p.z.floor())
}

/// The corner `c` (integers as floats) hashed: the shader's `tbh`.
#[inline]
fn corner(c: Vec3f, seed: u32) -> u32 {
    hash3_u32(c.x as i32, c.y as i32, c.z as i32, seed)
}

/// A corner's gradient, uniform in the cube: the shader's `tbg`.
#[inline]
fn grad(h: u32) -> Vec3f {
    vec3f((h & 1023) as f32, ((h >> 10) & 1023) as f32, ((h >> 20) & 1023) as f32) * (1.0 / 511.5) - vec3f(1.0, 1.0, 1.0)
}

/// Gradient noise with the quintic fade, about -0.5..0.5: the shader's `tbn`.
fn gnoise(p: Vec3f, seed: u32) -> f32 {
    let i = floor3(p);
    let f = p - i;
    let fade = |t: f32| t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
    let u = vec3f(fade(f.x), fade(f.y), fade(f.z));
    let n = |dx: f32, dy: f32, dz: f32| grad(corner(i + vec3f(dx, dy, dz), seed)).dot(f - vec3f(dx, dy, dz));
    let (n000, n100, n010, n110) = (n(0.0, 0.0, 0.0), n(1.0, 0.0, 0.0), n(0.0, 1.0, 0.0), n(1.0, 1.0, 0.0));
    let (n001, n101, n011, n111) = (n(0.0, 0.0, 1.0), n(1.0, 0.0, 1.0), n(0.0, 1.0, 1.0), n(1.0, 1.0, 1.0));
    let x00 = n000 + (n100 - n000) * u.x;
    let x10 = n010 + (n110 - n010) * u.x;
    let x01 = n001 + (n101 - n001) * u.x;
    let x11 = n011 + (n111 - n011) * u.x;
    let y0 = x00 + (x10 - x00) * u.y;
    let y1 = x01 + (x11 - x01) * u.y;
    y0 + (y1 - y0) * u.z
}

/// Value noise 0..1 on the corner hash: the shader's `tbv`.
fn vnoise(p: Vec3f, seed: u32) -> f32 {
    let i = floor3(p);
    let f0 = p - i;
    let f = vec3f(f0.x * f0.x * (3.0 - 2.0 * f0.x), f0.y * f0.y * (3.0 - 2.0 * f0.y), f0.z * f0.z * (3.0 - 2.0 * f0.z));
    let v = |dx: f32, dy: f32, dz: f32| (corner(i + vec3f(dx, dy, dz), seed) >> 8) as f32 * (1.0 / 16_777_216.0);
    let (a, b, c, d) = (v(0.0, 0.0, 0.0), v(1.0, 0.0, 0.0), v(0.0, 1.0, 0.0), v(1.0, 1.0, 0.0));
    let (e, g, h, j) = (v(0.0, 0.0, 1.0), v(1.0, 0.0, 1.0), v(0.0, 1.0, 1.0), v(1.0, 1.0, 1.0));
    let x0 = a + (b - a) * f.x;
    let x1 = c + (d - c) * f.x;
    let x2 = e + (g - e) * f.x;
    let x3 = h + (j - h) * f.x;
    let y0 = x0 + (x1 - x0) * f.y;
    let y1 = x2 + (x3 - x2) * f.y;
    y0 + (y1 - y0) * f.z
}

/// Worley noise, inverted: 1 at a site, 0 a cell away: the shader's `tbw`.
fn worley(p: Vec3f, seed: u32) -> f32 {
    let i = floor3(p);
    let f = p - i;
    let mut d1 = 9.0f32;
    for z in 0..3 {
        for y in 0..3 {
            for x in 0..3 {
                let c = i + vec3f(x as f32 - 1.0, y as f32 - 1.0, z as f32 - 1.0);
                let h = corner(c, seed);
                let o = vec3f((h & 1023) as f32, ((h >> 10) & 1023) as f32, ((h >> 20) & 1023) as f32) * (1.0 / 1023.0);
                let r = c - i + o - f;
                d1 = d1.min(r.dot(r));
            }
        }
    }
    1.0 - d1.sqrt().clamp(0.0, 1.0)
}

/// Where the shape's cut stands: the solved threshold and, for the turbulence deck, its rise toward the sun.
struct Cut {
    cut: f32,
    open: f32,
}

impl DeckShape {
    /// The turbulence at `p` (cells) with `oct` octaves read, the rest standing in by their mean: the shader's `tbturb`.
    fn turb(&self, p0: Vec3f, oct: u32) -> f32 {
        let (mut p, mut t, mut a) = (p0, 0.0f32, 0.5f32);
        for o in 0..5u32 {
            if o < oct {
                let n = gnoise(p, self.seed);
                t += a * if (o as f32) < self.look.fold { n.abs() } else { 0.16 + 0.8 * n };
            } else {
                t += a * 0.16;
            }
            p = p * 2.6434 + vec3f(1.7, 9.2, 4.4);
            a *= 0.5;
        }
        t
    }

    /// The turbulence deck's masses at q (km): the shader's `tbmass`.
    fn mass(&self, q: Vec3f) -> f32 {
        let k = self.look.mass_per_km;
        gnoise(q * k + vec3f(3.3, 3.3, 3.3), self.seed) * 0.7 + gnoise(q * k * 2.3 + vec3f(7.1, 7.1, 7.1), self.seed) * 0.3
    }

    /// The cell deck's masses: the shader's `tbmass2`.
    fn mass2(&self, q: Vec3f) -> f32 {
        let k = self.look.mass_per_km;
        vnoise(q * k + vec3f(3.3, 3.3, 3.3), self.seed) * 0.6 + vnoise(q * k * 2.3 + vec3f(7.1, 7.1, 7.1), self.seed) * 0.3 + vnoise(q * k * 5.1 + vec3f(1.9, 1.9, 1.9), self.seed) * 0.1
    }

    /// The noise point of the wind-frame metres (`along` past the drift, `across`) at height `h` over the base: the
    /// shader's `q`.
    #[inline]
    fn q_of(&self, along: f32, h: f32, across: f32) -> Vec3f {
        let qa = along / self.look.stretch.max(0.01);
        vec3f(qa * 0.001 + self.offset.x, h * 0.001 + self.offset.y, across * 0.001 + self.offset.z)
    }

    /// The wind-frame metres of the plane point (x, z): the shader's, the drift taken off along the wind.
    #[inline]
    fn frame_of(&self, x: f32, z: f32) -> (f32, f32) {
        (x * self.down.x + z * self.down.y - self.drift_m, z * self.down.x - x * self.down.y)
    }

    /// The turbulence deck's raw field at q (km): the shader's `tbraw`.
    fn raw(&self, q: Vec3f, oct: u32) -> f32 {
        let l = &self.look;
        self.turb(vec3f(q.x, q.y * l.squash, q.z) * l.cells_per_km, oct) + self.mass(q) * l.mass
    }

    /// The field over the cut and the density (a share of sigma) at the wind-frame point, with `oct` octaves (the
    /// turbulence deck: 5 the full field, 3 the sun's far reads, 2 the dome's column; the cell deck: 5 with the
    /// erosion, less without); the shader's `tbfield` and `tbfield2`. `cut` is the cut there. (density, field - cut)
    fn field_at(&self, along: f32, h: f32, across: f32, hn: f32, dist: f32, cut: &Cut, oct: u32) -> (f32, f32) {
        let l = &self.look;
        let q = self.q_of(along, h, across);
        match l.kind {
            DeckKind::Turbulence => {
                let prof = ss(0.0, l.base_rise, hn) * (1.0 - ss(l.top_fall, 1.0, hn));
                let raw = self.raw(q, oct);
                let field = raw - (1.0 - prof) * l.profile;
                let ramp = l.ramp;
                let mut d = (field - cut.cut + 0.05 * ramp / 0.3) * ss(cut.cut, cut.cut + l.cut_edge, field) / ramp;
                let far = ss(l.far_from_m, l.far_to_m, dist);
                let mut dv = 0.0;
                if l.veil > 0.0 {
                    let band = ss(cut.cut - l.veil_band, cut.cut, field);
                    let mut tex = 0.5;
                    if oct > 2 {
                        let n = gnoise(q * l.veil_per_km + vec3f(5.1, 5.1, 5.1), self.seed) + 0.5 * gnoise(q * l.veil_per_km * 2.7 + vec3f(2.3, 2.3, 2.3), self.seed);
                        tex = 1.0 - ss(-0.18, 0.18, n - 0.45 * (2.0 * l.veil_share - 1.0));
                    }
                    dv = band * tex * l.veil * (1.0 - cut.open) * (1.0 - far);
                }
                if oct > 4 && l.texture > 0.0 {
                    let ft = gnoise(q * l.cells_per_km * 7.0 + vec3f(3.7, 3.7, 3.7), self.seed) * 0.6 + gnoise(q * l.cells_per_km * 18.0 + vec3f(1.3, 1.3, 1.3), self.seed) * 0.4;
                    d *= (1.0 + 2.0 * l.texture * ft).clamp(0.2, 2.0);
                }
                let lw = if l.water_base > 0.0 { l.water_base + (1.0 - l.water_base) * ss(0.0, l.water_full, hn) } else { 1.0 };
                (d.clamp(0.0, 1.5) * lw + dv, field - cut.cut)
            }
            DeckKind::Cells => {
                let kc = l.cells_per_km;
                let w = worley(q * kc, self.seed) * 0.625 + worley(q * kc * 2.03 + vec3f(11.7, 11.7, 11.7), self.seed) * 0.25 + worley(q * kc * 4.07 + vec3f(3.1, 3.1, 3.1), self.seed) * 0.125;
                let m = self.mass2(q);
                let raw = w * (1.0 - l.mass) + m * l.mass;
                let topn = l.top_fall + (1.0 - l.top_fall) * ss(0.3, 0.7, m);
                let prof = ss(0.0, l.base_rise, hn) * (1.0 - ss((0.3 * topn).max(0.2).min(0.6 * topn), topn, hn));
                let field = raw - (1.0 - prof) * l.profile;
                let mut d = ((field - cut.cut) / l.ramp).clamp(0.0, 1.0);
                if d > 0.0 && oct > 4 {
                    let det = worley(q * kc * 4.0 + vec3f(1.7, 1.7, 1.7), self.seed) * 0.5 + worley(q * kc * 9.0 + vec3f(4.2, 4.2, 4.2), self.seed) * 0.3 + worley(q * kc * 19.0 + vec3f(7.7, 7.7, 7.7), self.seed) * 0.1;
                    let e = l.erosion * det * (1.0 - 0.5 * d);
                    d = ((d - e) / (1.0 - e).max(0.05)).clamp(0.0, 1.0);
                    d *= 1.0 - l.inside + l.inside * (0.35 + 0.9 * det);
                    d *= ss(0.0, 0.08 * topn, hn - l.lift * topn * (1.0 - det));
                }
                (d, field - cut.cut)
            }
        }
    }

    /// The cut at the plane point: the solved threshold, risen toward the sun for the turbulence deck.
    #[inline]
    fn cut_at(&self, shape: &VolumeShape, x: f32, z: f32) -> Cut {
        let l = &self.look;
        if l.kind == DeckKind::Cells || !(l.sun_rise != 0.0) {
            return Cut { cut: shape.threshold, open: 0.0 };
        }
        let along = (x * self.sun_xz.x + z * self.sun_xz.y) * 0.001;
        let open = ss(0.0, l.sun_km.max(0.1), along);
        Cut { cut: shape.threshold + l.sun_rise * open, open }
    }

    /// The field at `p` (metres) with `oct` octaves: (the density as a share of sigma, the field over the cut); None
    /// outside the shell.
    fn field(&self, shape: &VolumeShape, p: Vec3f, oct: u32) -> Option<(f32, f32)> {
        let h = p.y - shape.base_m;
        let hn = h / shape.thickness;
        if !(hn > 0.0 && hn < 1.0) {
            return None;
        }
        let (along, across) = self.frame_of(p.x, p.z);
        let cut = self.cut_at(shape, p.x, p.z);
        Some(self.field_at(along, h, across, hn, (p.x * p.x + p.z * p.z).sqrt(), &cut, oct))
    }

    /// The recipe at the wind-frame point `f` (cells): the field's column maximum over NH heights without a cut (the cut
    /// only lowers it) plus MARGIN, what the footprint grid holds and the quantile tables measure. The other channels
    /// are unused.
    pub(super) fn at(&self, f: Vec2f, cell_m: f32, thickness: f32) -> Node {
        let (along, across) = (f.x * cell_m, f.y * cell_m);
        let cut = Cut { cut: 0.0, open: 0.0 };
        let (held, excess) = self.look.grid_octaves();
        let mut most = f32::NEG_INFINITY;
        for k in 0..NH {
            let hn = (k as f32 + 0.5) / NH as f32;
            let (_, over) = self.field_at(along, hn * thickness, across, hn, 0.0, &cut, held.min(4));
            most = most.max(over);
        }
        [most + excess + self.look.cells_margin() + MARGIN, 0.0, 0.0, 0.0, 0.0]
    }

    /// Its footprint at the ground point (x, z), metres (`VolumeShape::footprint_at`): how far its column's field stands
    /// over the cut at its highest over NH heights, in ramps (0 where it never passes the cut: the deck's footprint before
    /// the cell deck erodes its edges, what the quantile tables measure), held to 1, times the tail gate; the field
    /// itself, anywhere on the plane
    /// (the grid that bounds it spans the march's range only).
    pub(super) fn footprint(&self, shape: &VolumeShape, x: f32, z: f32) -> f32 {
        let g = self.gate(shape, vec2f(x, z));
        let mut most = 0.0f32;
        for k in 0..NH {
            let y = shape.base_m + (k as f32 + 0.5) / NH as f32 * shape.thickness;
            if let Some((_, over)) = self.field(shape, vec3f(x, y, z), 5) {
                most = most.max(over / self.look.ramp);
            }
        }
        (most * g).clamp(0.0, 1.0)
    }

    /// The tail gate at the ground point `q`: 1 unless the Cover is under the quantile tables' tail, where CoverRemap's
    /// gate thins the clouds by its lattice points (the shader's gate_at).
    fn gate(&self, shape: &VolumeShape, q: Vec2f) -> f32 {
        if shape.gated {
            shape.remap.gate_at(shape.frame.of(q / shape.cell_m))
        } else {
            1.0
        }
    }

    /// The field's own column maximum at the wind-frame point `f` (cells) over NH heights, all its octaves, no margin:
    /// what Cover's quantile tables measure, so the share of the plane whose column holds cloud follows the threshold
    /// (measured on the grid's bound, which stands wider than the cloud, a tiny Cover drew a fraction of its share).
    fn column_max(&self, f: Vec2f, cell_m: f32, thickness: f32) -> f32 {
        let (along, across) = (f.x * cell_m, f.y * cell_m);
        let cut = Cut { cut: 0.0, open: 0.0 };
        let mut most = f32::NEG_INFINITY;
        for k in 0..NH {
            let hn = (k as f32 + 0.5) / NH as f32;
            most = most.max(self.field_at(along, hn * thickness, across, hn, 0.0, &cut, 5).1);
        }
        most
    }

    /// The footprint after the cover remap at the ground point `q` on the noise `n` (over the threshold).
    fn cover(&self, shape: &VolumeShape, q: Vec2f, n: f32) -> f32 {
        if shape.gated {
            shape.remap.density(n, shape.threshold, shape.frame.of(q / shape.cell_m))
        } else {
            cover_density(n, shape.threshold, FOOTPRINT_EDGE)
        }
    }

    /// The optical depth of the column over `p` read coarsely (two octaves), three reads up to the top leaning out
    /// sideways by up to half the layer (the cell deck straight up): the shader's `tbup`, times the gate.
    fn column(&self, shape: &VolumeShape, p: Vec3f, gate: f32) -> f32 {
        let top_m = shape.top_m - p.y;
        let mut up = 0.0f32;
        for k in 0..3 {
            let a = 2.0944 * k as f32;
            let rr = if self.look.kind == DeckKind::Cells { 0.0 } else { 0.5 * shape.thickness * (k as f32 + 0.5) / 3.0 };
            let o3 = vec3f(a.cos() * rr, (k as f32 + 0.5) / 3.0 * top_m, a.sin() * rr);
            if let Some((den, _)) = self.field(shape, p + o3, 2) {
                up += den * top_m / 3.0;
            }
        }
        up * shape.sigma * gate
    }

    /// `VolumeShape::cheap` for a deck: the density where the footprint (the column maximum) passes the threshold, no
    /// detail (its skin weight 0), the envelope where the field stands within the veil's band under the cut, its own
    /// dome the column over the point. The sun's field is the view's.
    pub(super) fn cheap(&self, shape: &VolumeShape, p: Vec3f, footprint_m: f32) -> Cheap {
        let h01 = (p.y - shape.base_m) / shape.thickness;
        let q = vec2f(p.x, p.z);
        if !(h01 > 0.0 && h01 < 1.0) || !(p.x.abs() < shape.grid.half_m() && p.z.abs() < shape.grid.half_m()) {
            return Cheap { h01, q, ..Cheap::default() };
        }
        let n = shape.grid.sample(q.x, q.y, footprint_m)[0];
        if !(n > shape.threshold) {
            return Cheap { h01, q, ..Cheap::default() };
        }
        // the grid bounds the field: cloud may stand here (the walk looks for it in short probes), and the field over
        // the cut is the cloud; the tail gate thins it at a tiny Cover
        let g = self.gate(shape, q);
        let f = self.cover(shape, q, n);
        if !(g > 0.0) {
            return Cheap { h01, q, f, ..Cheap::default() };
        }
        let air = Cheap { h01, q, f, envelope: true, ..Cheap::default() };
        let Some((den, _)) = self.field(shape, p, 5) else {
            return air;
        };
        let view = den * g;
        if !(view > 0.0) {
            return air;
        }
        Cheap { view, h01, q, f, skin: 0.0, sun: view, envelope: true, up: Some(self.column(shape, p, g)) }
    }

    /// The sun's field its grids and live sun steps read (`VolumeShape::extinction` over sigma): the field at three
    /// octaves (the sun march's far reads), gated as the view.
    pub(super) fn sun_field(&self, shape: &VolumeShape, p: Vec3f) -> f32 {
        let h01 = (p.y - shape.base_m) / shape.thickness;
        if !(h01 > 0.0 && h01 < 1.0) || !(p.x.abs() < shape.grid.half_m() && p.z.abs() < shape.grid.half_m()) {
            return 0.0;
        }
        let n = shape.grid.sample(p.x, p.z, 0.0)[0];
        if !(n > shape.threshold) {
            return 0.0;
        }
        let g = self.gate(shape, vec2f(p.x, p.z));
        if !(g > 0.0) {
            return 0.0;
        }
        self.field(shape, p, 3).map_or(0.0, |(den, _)| den * g)
    }

    /// The deck's numbers as the GPU export's deck reads them (cloud_gpu's export/deck.rs), and its shares of the light
    /// law.
    pub(super) fn gpu_layout(&self, shape: &VolumeShape, sun_up: f32, out: &mut UploadBuilder) {
        let l = &self.look;
        // the export doubles the shell's extinction (xa.x, the density scale): it is given half the deck's
        out.uniform("shell", [shape.base_m, shape.top_m, shape.thickness, 0.5 * shape.sigma]);
        out.uniform("sc_r", [0.0, 0.0, SOAK * sun_up.max(0.05), 0.0]);
        out.uniform("tb_a", [0.0, l.cells_per_km, l.mass_per_km, l.mass]);
        out.uniform("tb_b", [l.cut_edge, l.ramp, l.squash, l.profile]);
        out.uniform("tb_c", [l.base_rise, l.top_fall, l.sun_rise, l.sun_km]);
        out.uniform("tb_d", [l.veil, l.veil_band, l.veil_per_km, l.veil_share]);
        out.uniform("tb_e", [l.water_base, l.water_full, l.stretch, l.texture]);
        out.uniform("tb_f", [l.far_from_m, l.far_to_m, l.fold, (self.seed & 0xffff) as f32]);
        out.uniform("tb_g", [0.0, 0.0, self.offset.y, self.offset.z]);
        out.uniform("tb_h", [self.offset.x, 0.0, 0.0, (self.seed >> 16) as f32]);
        out.uniform("tb_i", [if l.kind == DeckKind::Cells { 1.0 } else { 0.0 }, l.erosion, l.inside, l.lift]);
        crate::cloud_volume_light::gpu_forward_lobe(out, l.forward_g);
    }
}

/// The volume of a deck type at `look` (design §2.2): its shell `base_m` to `top_m`, Density `density`, its cells
/// `size_km` apart (the wind frame's cell; the look's frequencies are its own), `recipe_id` in its grid's key. None at
/// no cover or no density: its footprint grid (the field's column maximum) looked up or built, the seed's tiles, and
/// Cover's share solved as the Cumulus' is (design §3), on the marched density.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build(spec: &VolumeSpec, look: DeckLook, density: f32, base_m: f32, top_m: f32, size_km: f32, recipe_id: f32, cancelled: &dyn Fn() -> bool, run: RowRunner) -> Result<Option<VolumeShape>, Cancelled> {
    let c = spec.cover;
    if !(c > 0.0) || !(density > 0.0) {
        return Ok(None);
    }
    let cell_m = size_km * 1000.0;
    let stream = cloud_layer_seed(spec.map_seed, spec.layer.seed, spec.genus);
    let streams = Streams::of(stream);
    let drift_cells = spec.layer.drift_km.unwrap_or(0.0) * 1000.0 / cell_m;
    let frame = WindFrame::new(spec.downwind, drift_cells);
    let top_m = top_m.max(base_m + 1.0);
    let thickness = top_m - base_m;
    let place = |k: u32| (hash_u32(stream ^ k.wrapping_mul(0x68e3_1da4)) >> 8) as f32 * (1.0 / 16_777_216.0);
    let [down_x, down_z, drift] = frame.gpu();
    let sun_xz = {
        let s = vec2f(spec.sun_dir.x, spec.sun_dir.z);
        let len = s.length();
        if len > 1.0e-6 { s / len } else { Vec2f::default() }
    };
    let deck = DeckShape { look, seed: hash_u32(stream ^ 0x7b1d_c0de), offset: vec3f(place(1), place(2), place(3)) * 16.0, down: vec2f(down_x, down_z), drift_m: drift * cell_m, sun_xz };
    // Cover's tables are measured on the deck at MEASURE_SEED (its noise seed and offset from that stream), as every
    // other type's are: the set is shared by the shape's fingerprint, which holds no seed, so a table measured on the
    // layer's own seed was whichever layer of that shape measured it first in the process (its 4096 points differ by
    // seed), and a stack's threshold hung on what had been built before it.
    let measured = {
        let place = |k: u32| (hash_u32(MEASURE_SEED ^ k.wrapping_mul(0x68e3_1da4)) >> 8) as f32 * (1.0 / 16_777_216.0);
        DeckShape { seed: hash_u32(MEASURE_SEED ^ 0x7b1d_c0de), offset: vec3f(place(1), place(2), place(3)) * 16.0, ..deck.clone() }
    };
    let l = &look;
    let kind = if l.kind == DeckKind::Cells { 1.0 } else { 0.0 };
    // the grid's key: everything its column maximum reads
    let grid_key = footprint_grid_key(
        &[recipe_id, kind, size_km, l.cells_per_km, l.mass_per_km, l.mass, l.squash, l.profile, l.base_rise, l.top_fall, l.stretch, l.fold, spec.downwind.x, spec.downwind.y, drift_cells, base_m, top_m, deck.offset.x, deck.offset.y, deck.offset.z, f32::from_bits(deck.seed)],
        stream,
        GRID_SIZE,
        GRID_SPAN_M,
    );
    let bounds = move |_: [f32; 2], _: [f32; 2]| (base_m, top_m);
    let margin = SkipMargin { across_m: 1.0, below_m: 0.0, above_m: 0.0 };
    let at_m = |x: f32, z: f32| deck.at(frame.of(vec2f(x / cell_m, z / cell_m)), cell_m, thickness);
    let grid = shared_footprint_grid(grid_key, &|| FootprintGrid::build(GRID_SIZE, GRID_SPAN_M, &at_m, &bounds, margin, cancelled, run))?;
    let noise = noise_for(spec.map_seed, cancelled, run)?;
    let billow = noise.billow(cancelled, run)?;
    let sigma = l.sigma * density;
    let steps = spec.march_steps.clamp(STEPS_LEAST, STEPS_MOST);
    let bits = f32::from_bits;
    let key = footprint_grid_key(&[density, sigma, l.sun_rise, sun_xz.x, sun_xz.y, bits(spec.map_seed), bits(grid_key as u32), bits((grid_key >> 32) as u32)], stream, TOP_COLS, DECK_KEY_PART);
    // the quantile tables: the column maximum's distribution, which these fields set (the thickness through the squash
    // and the vertical sampling)
    let fields = [kind, l.cells_per_km * size_km / l.stretch, l.cells_per_km * size_km, l.mass_per_km * size_km, l.mass, l.squash * thickness * 0.001 * l.cells_per_km, l.profile, l.base_rise, l.top_fall, l.fold];
    let set = quantile_set(fingerprint(spec.genus, FOOTPRINT_PART, &fields));
    let mut shape = VolumeShape {
        base_m,
        top_m,
        thickness,
        cell_m,
        frame,
        recipe: VolumeRecipe::Cumulus(CumulusRecipe { clustering: 0.0, streets: 0.0 }),
        streams,
        grid,
        grid_key,
        remap: CoverRemap::new(set.clone(), c, FOOTPRINT_EDGE, stream),
        threshold: 0.0,
        gated: false,
        share: c,
        k: 1.0,
        // The Cumulus' tables: none read for a deck (the GPU upload sends them at their lengths).
        tops: vec![0.0; TOP_COLS],
        rise: vec![0.0; RISE_ROWS],
        rise_threshold: 0.0,
        levels: vec![0.0; LEVEL_ROWS + 1],
        shape_scale: 1.0 / (SHAPE_TILE_M * VOLUME_SHAPE_TILE * size_km),
        detail_scale: 1.0 / (DETAIL_TILE_M * size_km),
        detail_texel_m: DETAIL_TILE_M * size_km / noise.detail.size() as f32,
        detail_mean: noise.detail.mean(),
        noise,
        billow,
        offset: vec3f(place(4), place(5), place(6)),
        sigma,
        carve: 1.0,
        billow_cut: 0.5,
        span: 0.3,
        erosion: 0.0,
        lean: Vec2f::default(),
        steps,
        lattice_m: LATTICE_M,
        cover: c,
        stream,
        key,
        genus: spec.genus,
        stratus: None,
        deck: Some(deck),
        ns: None,
    };
    // the tables are measured on the deck's own column maximum (its seed and offset do not move its distribution)
    let n = |_: u32, x: f32, y: f32| measured.column_max(vec2f(x, y), cell_m, thickness);
    shape.solve_share(c, &set, &n, cancelled, run)?;
    shape.key = footprint_grid_key(&[bits(key as u32), bits((key >> 32) as u32), shape.threshold, shape.share, shape.gated as u32 as f32], stream, TOP_COLS, DECK_KEY_PART);
    Ok(Some(shape))
}
