//! Volumetric clouds (phase 1b): the 2D deck's footprint grown into a volume
//! and marched.
//!
//! - **Where.** The deck's own density map (`CloudLayer::deck_density`) is
//!   the coverage: a cloud stands wherever the 2D layer would draw one, between
//!   `base_m` and `top_m`, taller where the cover is solid, and a column hides
//!   the sky as the deck does on its footprint (`deck_opacity`): a thin
//!   footprint, which the deck draws translucent, stands as a veil at the
//!   bottom of the layer, and solid cloud takes over as the footprint thickens.
//! - **Shape.** A tiling 3D "Perlin-Worley" volume (Schneider, "The Real-time
//!   Volumetric Cloudscapes of Horizon Zero Dawn", 2015: Perlin fbm dilated by
//!   Worley noise and cut by a finer Worley fbm) decides how much of that
//!   stands at each point, and a smaller Worley detail volume erodes the
//!   edges. Both are built once per seed and shared.
//! - **March.** A view ray meets the shell between the two heights on a
//!   curved Earth and takes 32 to 64 steps through it (`steps / 2` overhead up
//!   to `steps` at the horizon), double-stepping through clear air. Each step
//!   attenuates by Beer-Lambert and scatters in sunlight (through a
//!   low-resolution grid of optical depths toward the sun, multiple-scattering
//!   octaves of a dual-lobe Henyey-Greenstein phase, a powder term) and the
//!   ambient from the sky table by direction: the dome above, strongest at
//!   the top, and the ground below, lit by the dome, at the base.
//!
//! The techniques are reimplemented from the papers and from Inigo Quilez's
//! articles on noise; no code is copied.
//!
//! Directions are in the map's own frame (before the map rotation). Lengths
//! are metres. A march returns four scalars so that the light's colours stay
//! outside: opacity, and in-scattered light per unit of sun irradiance, of
//! the dome's radiance and of the ground's radiance; `shade` applies the
//! colours.

use makepad_draw::*;
use makepad_render_material::ibl;
use super::clouds::{deck_opacity, hg_phase, CloudLayer, CloudParts};
use super::noise::hash01;
use super::{arr, vec, CloudParams, HdriParams, VolumetricParams};
use crate::caster::{serial_rows, RowRunner};
use crate::sky::luminance;
use std::sync::{Arc, Mutex, OnceLock};

/// Bakes wider than this march the clouds once at this width and read the
/// result bilinearly: the march is the expensive part of a map, and clouds
/// hold no detail finer than 2K.
pub const MARCH_MAX_WIDTH: usize = 2048;

/// Texels along an edge of the tiling shape volume; the detail volume is half
/// that. One size in every build: tests run optimised with debug assertions
/// on, and a smaller volume there made other clouds than the ones they check.
const SHAPE_SIZE: usize = 128;
/// The light grid: LIGHT_XZ x LIGHT_XZ columns over the range, LIGHT_Y levels
/// through the layer.
const LIGHT_XZ: usize = 128;
const LIGHT_Y: usize = 8;
/// Steps of the march toward the sun, each twice as long as the one before.
const LIGHT_STEPS: usize = 6;
/// How far toward the sun the light march reaches, in metres.
const LIGHT_REACH_M: f32 = 6000.0;
/// The light grid marches toward a sun at least this high (sine of 8.6 deg),
/// so a low sun's path through the layer stays finite.
pub(crate) const LIGHT_MIN_SIN: f32 = 0.15;
/// Clouds stand within this horizontal distance of the viewer and fade out
/// over its last quarter. From a 1500 m base that is 2.7 degrees above the
/// horizon, under the horizon fade.
pub(crate) const RANGE_M: f32 = 32_000.0;
pub(crate) const PLANET_M: f32 = 6_360_000.0;
/// The viewer's height above the ground.
pub(crate) const EYE_M: f32 = 2.0;
/// sin(elevation) at which the clouds reach full strength, as the 2D deck's.
const HORIZON_FADE: f32 = 0.12;
/// Metres per tile of the shape noise at `scale` 1 (its broadest billows are
/// a quarter of that), and of the detail noise.
pub(crate) const SHAPE_TILE_M: f32 = 6000.0;
pub(crate) const DETAIL_TILE_M: f32 = 1400.0;
/// Extinction per metre at density 1: a kilometre of solid cloud is far past
/// opaque, as cumulus is.
const SIGMA: f32 = 0.03;
/// How far full erosion cuts into the density.
const EROSION_DEPTH: f32 = 0.35;
/// Footprints up to SOLID_FROM (the deck's density) stand as a translucent
/// veil, from SOLID_AT on as solid cloud, and in between as a mix of the two:
/// the deck draws 0.15 about 53 % opaque and 0.4 about 87 %.
const SOLID_FROM: f32 = 0.15;
const SOLID_AT: f32 = 0.4;
/// The veil fills the layer from its base to this height (0..1 through the
/// layer). VEIL_LENGTH is the slab's length along a vertical ray as a share
/// of the layer: its two soft edges (the bottom 0.1 of the layer, the top
/// 40 % of the slab) count half.
const VEIL_TOP: f32 = 0.4;
const VEIL_LENGTH: f32 = 0.8 * VEIL_TOP - 0.05;
/// Below this transmittance the march stops and the ray counts as opaque.
pub(crate) const MIN_TRANSMITTANCE: f32 = 0.01;
/// Multiple-scattering octaves (Wrenninge's approximation): each sees half
/// the extinction, carries half the energy and has a rounder phase.
pub(crate) const OCTAVES: usize = 3;
/// The dual-lobe phase: a forward lobe (the silver lining) and a back lobe.
pub(crate) const G_FORWARD: f32 = 0.8;
pub(crate) const G_BACK: f32 = -0.3;
pub(crate) const FORWARD_SHARE: f32 = 0.75;
/// Octaves still underestimate a cloud's many scatterings; this lifts the
/// sunlit sides. Under heavy cover the volume stays darker than the 2D deck
/// (about 0.4x at coverage 1, 0.8x at 0.3, sun at 40 degrees, measured
/// before BOUNCE_SHARE's ground light was added, which lifts the base by
/// about ground_color x the dome): its base gets AMBIENT_FLOOR of the
/// ambient where the deck gets all of it.
pub(crate) const SUN_GAIN: f32 = 2.0;
/// Strength of the powder effect (dark edges where the sun's side is seen
/// from behind), and the depth it starts from.
pub(crate) const POWDER: f32 = 0.6;
const POWDER_FLOOR: f32 = 0.3;
/// The share of the sky's ambient that reaches a cloud's base; the top gets all.
pub(crate) const AMBIENT_FLOOR: f32 = 0.45;
/// The share of a cloud base's view that is the ground below it (the lower
/// hemisphere); the share falls to nothing at the top, which sees only sky.
pub(crate) const BOUNCE_SHARE: f32 = 0.5;
/// Seeds whose noise is kept: the current one and the one before.
const NOISE_KEEP: usize = 2;
/// The footprint's stand-in (`footprint_cover_toward`) reads it at points this many cells apart along a look's path
/// through the shell, and at no more than STAND_IN_POINTS of them: a quarter cell resolves the footprint's finer
/// octaves, and the longest paths (a few degrees up) are spaced wider.
const STAND_IN_SPACING_CELLS: f32 = 0.25;
const STAND_IN_POINTS: usize = 12;

/// What one march saw.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CloudMarch {
    /// Opacity along the ray, 0..1.
    pub alpha: f32,
    /// In-scattered sunlight per unit of sun irradiance (1/sr), already
    /// weighted by the opacity in front of it.
    pub sun: f32,
    /// In-scattered sky light per unit of the dome's radiance (from above),
    /// weighted likewise.
    pub ambient: f32,
    /// In-scattered ground light per unit of the ground's radiance (from
    /// below, mostly at the base), weighted likewise.
    pub bounce: f32,
}

/// A cube of tiling noise, one byte per texel. Its edge is a power of two (`fill_tile` asserts it), so a lookup wraps
/// with a mask (design §12.1).
pub(crate) struct Tile3 {
    size: usize,
    data: Vec<u8>,
}

impl Tile3 {
    /// Texels along an edge.
    pub(crate) fn size(&self) -> usize {
        self.size
    }

    /// Its mean value over the tile, 0..1 (what `sample` averages to over a whole tile).
    pub(crate) fn mean(&self) -> f32 {
        let sum: u64 = self.data.iter().map(|&b| b as u64).sum();
        sum as f32 / (self.data.len().max(1) as f32 * 255.0)
    }

    /// Trilinear and wrapping; coordinates in tiles, 0..1 over the cube.
    pub(crate) fn sample(&self, x: f32, y: f32, z: f32) -> f32 {
        let n = self.size as i32;
        // Texel centres sit at (i + 0.5) / n. `v - v.floor()`, not fract: it
        // is right for negative coordinates too.
        let axis = |v: f32| {
            let t = (v - v.floor()) * n as f32 - 0.5;
            let i = t.floor();
            (i as i32, t - i)
        };
        let (ix, tx) = axis(x);
        let (iy, ty) = axis(y);
        let (iz, tz) = axis(z);
        // The edge is a power of two: the mask is rem_euclid's wrap, negative i included (two's complement).
        let mask = n - 1;
        let wrap = |i: i32| (i & mask) as usize;
        let (x0, x1, y0, y1, z0, z1) = (wrap(ix), wrap(ix + 1), wrap(iy), wrap(iy + 1), wrap(iz), wrap(iz + 1));
        let size = self.size;
        let at = |x: usize, y: usize, z: usize| self.data[(z * size + y) * size + x] as f32;
        let near = mix_f32(mix_f32(at(x0, y0, z0), at(x1, y0, z0), tx), mix_f32(at(x0, y1, z0), at(x1, y1, z0), tx), ty);
        let far = mix_f32(mix_f32(at(x0, y0, z1), at(x1, y0, z1), tx), mix_f32(at(x0, y1, z1), at(x1, y1, z1), tx), ty);
        mix_f32(near, far, tz) * (1.0 / 255.0)
    }

    /// The tile as the GPU export reads it (design §12.3, its 2D layout of a 3D tile): texel (z n + y) n + x holds the
    /// bytes at (x, y, z), (x + 1, y, z), (x, y, z + 1) and (x + 1, y, z + 1), wrapped, as floats 0..255, so two nearest
    /// taps (rows y and y + 1) give `sample` its eight bytes and the shader takes `sample`'s own weights.
    pub(crate) fn gpu_texels(&self) -> impl Iterator<Item = [f32; 4]> + '_ {
        let n = self.size;
        let at = move |x: usize, y: usize, z: usize| self.data[(z * n + y) * n + x] as f32;
        (0..n * n * n).map(move |i| {
            let (x, y, z) = (i % n, (i / n) % n, i / (n * n));
            let (x1, z1) = ((x + 1) & (n - 1), (z + 1) & (n - 1));
            [at(x, y, z), at(x1, y, z), at(x, y, z1), at(x1, y, z1)]
        })
    }
}

/// The billow tile (design §12.9 item 1): inverted Worley at BILLOW_CELLS cells a tile, so a level set of it is balls of
/// three sizes (Schneider's inverted-Worley billows); each smaller octave stands BILLOW_HEIGHTS of the larger's height
/// and lifts the field at most BILLOW_LIFT over the larger balls' own value there, so a small ball sits on a larger
/// one's skin. As a plain union of the octaves the small balls stood free of the larger ones and floated off the rims.
const BILLOW_CELLS: [usize; 3] = [4, 8, 16];
const BILLOW_HEIGHTS: [f32; 2] = [0.85, 0.7];
const BILLOW_LIFT: f32 = 0.08;

/// The noise volumes of one seed: the two every volumetric map reads, and the billow tile a cloud volume reads, built
/// on first use (a classic map never builds it).
pub(crate) struct NoiseVolume {
    /// Perlin-Worley: how much cloud the footprint keeps at a point.
    pub(crate) shape: Tile3,
    /// Worley fbm at a finer scale: what erodes the edges.
    pub(crate) detail: Tile3,
    seed: u32,
    billow: OnceLock<Arc<Tile3>>,
}

impl NoiseVolume {
    /// The two volumes of `seed`, `size` texels along the shape's edge.
    /// `cancelled` is polled before each slice of either tile (D15): this is
    /// about 2 s in a release build on one thread, the slowest part of building
    /// a volumetric map, and a newer request must be able to stop it. Each
    /// slice's rows are spread by `run` (`fill_tile`); the bytes are the same
    /// whatever spreads them.
    fn build(seed: u32, size: usize, cancelled: &dyn Fn() -> bool, run: RowRunner) -> Result<NoiseVolume, Cancelled> {
        let size = size.max(4);
        // One stream per lattice, so no two share their hashes.
        let stream = |k: u32| seed.wrapping_mul(16).wrapping_add(k);
        let perlin = [PerlinTile::new(4, stream(0)), PerlinTile::new(8, stream(1)), PerlinTile::new(16, stream(2))];
        let worley = [WorleyTile::new(4, stream(3)), WorleyTile::new(8, stream(4)), WorleyTile::new(16, stream(5))];
        let shape = fill_tile(
            size,
            |x, y, z| {
                let billows = (perlin[0].at(x, y, z) + 0.5 * perlin[1].at(x, y, z) + 0.25 * perlin[2].at(x, y, z)) / 1.75;
                let soft = (0.5 + 0.8 * billows).clamp(0.0, 1.0);
                let round = worley[0].at(x, y, z);
                let cut = 0.65 * worley[1].at(x, y, z) + 0.35 * worley[2].at(x, y, z);
                // Perlin dilated by Worley (round tops), then cut by the finer
                // Worley fbm: remap(dilated, cut - 1, 1, 0, 1).
                let dilated = round + soft * (1.0 - round);
                (dilated + 1.0 - cut) / (2.0 - cut)
            },
            cancelled,
            run,
        )?;
        let fine = [WorleyTile::new(6, stream(6)), WorleyTile::new(12, stream(7)), WorleyTile::new(24, stream(8))];
        let detail = fill_tile(
            (size / 2).max(2),
            |x, y, z| 0.625 * fine[0].at(x, y, z) + 0.25 * fine[1].at(x, y, z) + 0.125 * fine[2].at(x, y, z),
            cancelled,
            run,
        )?;
        Ok(NoiseVolume { shape, detail, seed, billow: OnceLock::new() })
    }

    /// The seed its tiles are built from (the GPU's copy of them goes by it, design §12.11 item 4.6).
    pub(crate) fn seed(&self) -> u32 {
        self.seed
    }

    /// The billow tile (BILLOW_CELLS), the shape tile's size: built on the first call, `cancelled` polled before each of
    /// its slices and its rows spread by `run` as `build`'s; a build that is stopped keeps nothing. About as long as the
    /// shape tile's build (its three Worley octaves).
    pub(crate) fn billow(&self, cancelled: &dyn Fn() -> bool, run: RowRunner) -> Result<Arc<Tile3>, Cancelled> {
        if let Some(tile) = self.billow.get() {
            return Ok(tile.clone());
        }
        let stream = |k: u32| self.seed.wrapping_mul(16).wrapping_add(k);
        let balls = [WorleyTile::new(BILLOW_CELLS[0], stream(9)), WorleyTile::new(BILLOW_CELLS[1], stream(10)), WorleyTile::new(BILLOW_CELLS[2], stream(11))];
        let tile = fill_tile(
            self.shape.size,
            |x, y, z| {
                let large = balls[0].at(x, y, z);
                let middle = large.max((BILLOW_HEIGHTS[0] * balls[1].at(x, y, z)).min(large + BILLOW_LIFT));
                middle.max((BILLOW_HEIGHTS[1] * balls[2].at(x, y, z)).min(middle + BILLOW_LIFT))
            },
            cancelled,
            run,
        )?;
        // Two threads may have built it; the first one's stays (the same bytes).
        Ok(self.billow.get_or_init(|| Arc::new(tile)).clone())
    }
}

/// A build of the cloud volume that its caller stopped (`VolumetricClouds::build`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cancelled;

/// Evaluates `f` at every texel centre of a size^3 tile (coordinates 0..1)
/// and stores it stretched between its own 2nd and 98th percentiles, so the
/// bytes use their whole range whatever the noise's own statistics are.
/// `cancelled` is polled before each of its `size` slices; the first poll
/// that says so stops it. A slice's rows are spread by `run` (`run(n, row)`
/// calls `row(0..n)`, each once: the bake pool's fan_out in a job, a plain
/// loop elsewhere), so the tile is the same whatever spreads them; a runner
/// that skips a row (its job overtaken) stops it as a cancel does. `size` is
/// a power of two (`Tile3::sample` wraps with a mask).
fn fill_tile(size: usize, f: impl Fn(f32, f32, f32) -> f32 + Sync, cancelled: &dyn Fn() -> bool, run: RowRunner) -> Result<Tile3, Cancelled> {
    assert!(size.is_power_of_two(), "a noise tile's edge is a power of two, not {size}");
    let step = 1.0 / size as f32;
    let mut values = Vec::with_capacity(size * size * size);
    let mut rows: Vec<OnceLock<Vec<f32>>> = (0..size).map(|_| OnceLock::new()).collect();
    for k in 0..size {
        if cancelled() {
            return Err(Cancelled);
        }
        let z = (k as f32 + 0.5) * step;
        run(size, &|j| {
            let y = (j as f32 + 0.5) * step;
            let _ = rows[j].set((0..size).map(|i| f((i as f32 + 0.5) * step, y, z)).collect());
        });
        // Taken in order, which leaves the rows empty for the next slice.
        for row in rows.iter_mut() {
            values.extend_from_slice(&row.take().ok_or(Cancelled)?);
        }
    }
    let (mut min, mut max) = (f32::MAX, f32::MIN);
    for &v in &values {
        min = min.min(v);
        max = max.max(v);
    }
    let span = (max - min).max(1.0e-6);
    const BINS: usize = 1024;
    let mut bins = [0usize; BINS];
    for &v in &values {
        bins[(((v - min) / span) * (BINS - 1) as f32) as usize] += 1;
    }
    let quantile = |share: f32| {
        let target = (share * values.len() as f32) as usize;
        let mut seen = 0usize;
        for (i, &count) in bins.iter().enumerate() {
            seen += count;
            if seen > target {
                return min + span * i as f32 / (BINS - 1) as f32;
            }
        }
        max
    };
    let (lo, hi) = (quantile(0.02), quantile(0.98));
    let data = values.iter().map(|&v| (remap01(v, lo, hi) * 255.0 + 0.5) as u8).collect();
    Ok(Tile3 { size, data })
}

/// Index of lattice point (x, y, z) in a period^3 table, wrapping.
fn tile_index(period: usize, x: i32, y: i32, z: i32) -> usize {
    let p = period as i32;
    ((z.rem_euclid(p) * p + y.rem_euclid(p)) * p + x.rem_euclid(p)) as usize
}

/// Lattice cell and position inside it.
fn lattice(v: f32) -> (i32, f32) {
    let floor = v.floor();
    (floor as i32, v - floor)
}

/// Perlin's quintic fade: no creases along cell edges.
fn fade(t: f32) -> f32 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// Tiling gradient noise: `period` cells per tile, one of Perlin's twelve
/// cube-edge gradients per lattice point.
struct PerlinTile {
    period: usize,
    gradients: Vec<u8>,
}

impl PerlinTile {
    fn new(period: usize, seed: u32) -> PerlinTile {
        let mut gradients = Vec::with_capacity(period * period * period);
        for z in 0..period {
            for y in 0..period {
                for x in 0..period {
                    gradients.push((hash01(x as i32, y as i32, z as i32, seed) * 12.0) as u8);
                }
            }
        }
        PerlinTile { period, gradients }
    }

    /// About -1..1 at a point of the tile (0..1 per axis, wrapping).
    fn at(&self, x: f32, y: f32, z: f32) -> f32 {
        let p = self.period as f32;
        let (ix, fx) = lattice(x * p);
        let (iy, fy) = lattice(y * p);
        let (iz, fz) = lattice(z * p);
        let (u, v, w) = (fade(fx), fade(fy), fade(fz));
        let g = |dx: i32, dy: i32, dz: i32| {
            let code = self.gradients[tile_index(self.period, ix + dx, iy + dy, iz + dz)];
            gradient_dot(code, fx - dx as f32, fy - dy as f32, fz - dz as f32)
        };
        let near = mix_f32(mix_f32(g(0, 0, 0), g(1, 0, 0), u), mix_f32(g(0, 1, 0), g(1, 1, 0), u), v);
        let far = mix_f32(mix_f32(g(0, 0, 1), g(1, 0, 1), u), mix_f32(g(0, 1, 1), g(1, 1, 1), u), v);
        mix_f32(near, far, w)
    }
}

/// Dot product of an offset with one of the twelve cube-edge gradients.
fn gradient_dot(code: u8, x: f32, y: f32, z: f32) -> f32 {
    match code {
        0 => x + y,
        1 => -x + y,
        2 => x - y,
        3 => -x - y,
        4 => x + z,
        5 => -x + z,
        6 => x - z,
        7 => -x - z,
        8 => y + z,
        9 => -y + z,
        10 => y - z,
        _ => -y - z,
    }
}

/// Tiling Worley noise: `period` cells per tile, one feature point per cell.
struct WorleyTile {
    period: usize,
    points: Vec<[f32; 3]>,
}

impl WorleyTile {
    fn new(period: usize, seed: u32) -> WorleyTile {
        let mut points = Vec::with_capacity(period * period * period);
        for z in 0..period {
            for y in 0..period {
                for x in 0..period {
                    let (x, y, z) = (x as i32, y as i32, z as i32);
                    // Three coordinates from three streams of the pinned hash.
                    points.push([hash01(x, y, z, seed), hash01(x, y, z, seed ^ 0x68e3_1da4), hash01(x, y, z, seed ^ 0xb529_7a4d)]);
                }
            }
        }
        WorleyTile { period, points }
    }

    /// Inverted F1 at a point of the tile: 1 at a feature point, 0 a cell away.
    fn at(&self, x: f32, y: f32, z: f32) -> f32 {
        let p = self.period as f32;
        let (ix, fx) = lattice(x * p);
        let (iy, fy) = lattice(y * p);
        let (iz, fz) = lattice(z * p);
        let mut nearest = f32::MAX;
        // The result is capped a cell away, so the 3x3x3 block around the cell is exact.
        for dz in -1..=1i32 {
            for dy in -1..=1i32 {
                for dx in -1..=1i32 {
                    let point = self.points[tile_index(self.period, ix + dx, iy + dy, iz + dz)];
                    let (ox, oy, oz) = (dx as f32 + point[0] - fx, dy as f32 + point[1] - fy, dz as f32 + point[2] - fz);
                    nearest = nearest.min(ox * ox + oy * oy + oz * oz);
                }
            }
        }
        1.0 - nearest.sqrt().min(1.0)
    }
}

/// The noise of the seeds used last. Building one takes about 2 s in a
/// release build on one thread (1960 to 2040 ms measured), and every
/// `Env::new_with` of a volumetric sky needs it.
static NOISE: Mutex<Vec<(u32, Arc<NoiseVolume>)>> = Mutex::new(Vec::new());

/// The noise volumes for `seed`: built on first use, then shared. The first
/// build polls `cancelled` (`NoiseVolume::build`); a build it stops keeps
/// nothing, so the next one of the seed starts again. A seed already built
/// has nothing left to stop.
pub(crate) fn noise_for(seed: u32, cancelled: &dyn Fn() -> bool, run: RowRunner) -> Result<Arc<NoiseVolume>, Cancelled> {
    if let Ok(cache) = NOISE.lock() {
        if let Some((_, volume)) = cache.iter().find(|(kept, _)| *kept == seed) {
            return Ok(volume.clone());
        }
    }
    // Built outside the lock, so another seed's lookup does not wait for it.
    let volume = Arc::new(NoiseVolume::build(seed, SHAPE_SIZE, cancelled, run)?);
    if let Ok(mut cache) = NOISE.lock() {
        // Two threads may have built the same seed; the first one's stays.
        if let Some((_, first)) = cache.iter().find(|(kept, _)| *kept == seed) {
            return Ok(first.clone());
        }
        if cache.len() >= NOISE_KEEP {
            cache.remove(0);
        }
        cache.push((seed, volume.clone()));
    }
    Ok(volume)
}

/// What a cloud volume is built from: `build` reads them off a CloudParams and a VolumetricParams as it always did; a
/// lifted layer gives its own (cloud_classic.rs, design §1.2).
#[derive(Clone, Copy, Debug)]
pub(crate) struct VolumeParts {
    /// The deck the volume stands on: its coverage, sharpness, cell and stream (its drift fields unused).
    pub footprint: CloudParts,
    pub base_m: f32,
    pub top_m: f32,
    pub density: f32,
    pub erosion: f32,
    pub steps: u32,
    /// The noise tiles' size factor (the older clouds.scale) and the noise volume's seed.
    pub noise_scale: f32,
    pub noise_seed: u32,
    /// How far the dome's light is greyed in `sky_light`.
    pub greyed: f32,
    /// How far the pattern stands downwind, metres (x east, z south).
    pub drift_m: Vec2f,
}

/// A cloud volume, ready to march. Immutable after `new` apart from the light
/// grid's slabs, which fill in on demand; bake threads share it by reference.
pub struct VolumetricClouds {
    noise: Arc<NoiseVolume>,
    /// The 2D deck: its density map is the footprint the volume stands on.
    deck: CloudLayer,
    /// Metres per noise cell of the deck.
    cell_m: f32,
    sun_dir: Vec3f,
    /// The direction the light grid marches toward: the sun, held at least
    /// LIGHT_MIN_SIN above the horizon.
    light_dir: Vec3f,
    /// The layer's base and top, metres above the ground.
    base: f32,
    top: f32,
    /// Extinction per metre at density 1.
    sigma: f32,
    erosion: f32,
    /// How far the dome's light is greyed (`sky_light`): 0.5 + 0.5 x coverage,
    /// as the 2D deck greys it.
    greyed: f32,
    /// The step budget toward the horizon.
    steps: usize,
    /// Tiles per metre of the two noise volumes.
    shape_scale: f32,
    detail_scale: f32,
    /// Optical depth toward the sun: LIGHT_XZ slabs along z, each LIGHT_XZ
    /// columns by LIGHT_Y levels, built by whichever thread first reads it.
    light: Vec<OnceLock<Vec<f32>>>,
    seed: u32,
    /// How far the pattern stands downwind, metres (0 from `build`).
    drift: Vec2f,
}

impl VolumetricClouds {
    /// None when there is nothing to march: no coverage, or no density.
    /// `clouds` places the footprint (coverage, sharpness, scale); `vol` says
    /// how the volume stands on it.
    pub fn new(clouds: &CloudParams, vol: &VolumetricParams, seed: u32, sun_dir: Vec3f) -> Option<VolumetricClouds> {
        // Nothing cancels it, and its noise is built on this thread.
        VolumetricClouds::build(clouds, vol, seed, sun_dir, &|| false, &serial_rows).unwrap_or(None)
    }

    /// [`Self::new`], stopped when `cancelled` says so while it builds the
    /// seed's noise, the first time a seed is used (about 2 s in release on
    /// one thread, L:229; D15): `Err(Cancelled)`. The noise's rows are spread
    /// by `run` (`NoiseVolume::build`). A volume that was not stopped is
    /// `new`'s, and a seed already built has nothing to stop.
    pub fn build(clouds: &CloudParams, vol: &VolumetricParams, seed: u32, sun_dir: Vec3f, cancelled: &dyn Fn() -> bool, run: RowRunner) -> Result<Option<VolumetricClouds>, Cancelled> {
        // Both as clamp() holds them: their ranges and defaults are written
        // once (L:231), the top 200 m over the base included.
        let (clouds, vol) = (&clouds.clamped(), &vol.clamped());
        let parts = VolumeParts {
            footprint: CloudParts { cirrus: 0.0, ..CloudParts::of(clouds, seed) },
            base_m: vol.base_m,
            top_m: vol.top_m,
            density: vol.density,
            erosion: vol.erosion,
            steps: vol.steps,
            noise_scale: clouds.scale,
            noise_seed: seed,
            greyed: 0.5 + 0.5 * clouds.coverage,
            drift_m: Vec2f::default(),
        };
        VolumetricClouds::from_parts(&parts, sun_dir, cancelled, run)
    }

    /// The volume drawn from `parts` (`build`'s, or a lifted layer's): None with no density or no coverage, both decided
    /// before the noise; Err(Cancelled) when `cancelled` stopped the first build of its seed's noise (D15), whose rows
    /// `run` spreads.
    pub(crate) fn from_parts(parts: &VolumeParts, sun_dir: Vec3f, cancelled: &dyn Fn() -> bool, run: RowRunner) -> Result<Option<VolumetricClouds>, Cancelled> {
        let density = parts.density;
        if density <= 0.0 {
            return Ok(None);
        }
        // The deck without its cirrus: None when there is no coverage, and
        // then there is no volume either.
        let Some(deck) = CloudLayer::from_parts(&CloudParts { cirrus: 0.0, ..parts.footprint }, sun_dir) else {
            return Ok(None);
        };
        let sun = sun_dir.normalize();
        let sun = if sun.is_finite() && sun.length() > 0.5 { sun } else { vec3f(0.0, 1.0, 0.0) };
        let (base, top, scale) = (parts.base_m, parts.top_m, parts.noise_scale);
        Ok(Some(VolumetricClouds {
            noise: noise_for(parts.noise_seed, cancelled, run)?,
            cell_m: deck.cell_m(),
            deck,
            sun_dir: sun,
            light_dir: vec3f(sun.x, sun.y.max(LIGHT_MIN_SIN), sun.z).normalize(),
            base,
            top,
            sigma: SIGMA * density,
            erosion: parts.erosion,
            greyed: parts.greyed,
            steps: parts.steps as usize,
            shape_scale: 1.0 / (SHAPE_TILE_M * scale),
            detail_scale: 1.0 / (DETAIL_TILE_M * scale),
            light: (0..LIGHT_XZ).map(|_| OnceLock::new()).collect(),
            seed: parts.noise_seed,
            drift: parts.drift_m,
        }))
    }

    /// Marches the volume along `dir`. Nothing at and below the horizon.
    pub fn march(&self, dir: Vec3f) -> CloudMarch {
        let mut out = [CloudMarch::default()];
        self.march_core(dir, true, &[], &mut out);
        out[0]
    }

    /// The march along `dir` cut at the heights `cuts` (ascending, inside base..top; design §2.2's split): out[k] is
    /// the stretch from cut k - 1 to cut k (out holds cuts + 1), on the whole march's step grid and jitter, so a sheet
    /// between the volume's base and top is drawn in its place. Each segment is faded as part of the whole, so composed
    /// front to back the segments are `march` (`split_segments`). A shorter `out` keeps its first cuts.
    pub fn march_split(&self, dir: Vec3f, cuts: &[f32], out: &mut [CloudMarch]) {
        if out.is_empty() {
            return;
        }
        let cuts = &cuts[..cuts.len().min(out.len() - 1)];
        self.march_core(dir, true, cuts, &mut out[..cuts.len() + 1]);
    }

    /// The layer's base and top, metres above the ground.
    pub fn shell_m(&self) -> (f32, f32) {
        (self.base, self.top)
    }

    /// The colours on a march: sunlight, the sky's ambient greyed as the 2D
    /// deck greys it (`sky_light`), and `ground`, the radiance of the ground
    /// under the layer, which lights the bases from below (green over grass,
    /// white over snow). The two ambient terms are the sky table taken by
    /// direction: `ambient` is the dome above (A4's hemisphere mean), `ground`
    /// what that dome (as `sky_light` greys it) and the shade leave on the
    /// ground. Premultiplied by the opacity: add it to `base x (1 - alpha)`.
    pub fn shade(&self, m: &CloudMarch, sun_irradiance: Vec3f, ambient: Vec3f, ground: Vec3f) -> Vec3f {
        sun_irradiance * m.sun + self.sky_light(ambient) * m.ambient + ground * m.bounce
    }

    /// The dome's light (`ambient`, the sky table's hemisphere mean) as it
    /// reaches the clouds and the ground under them, greyed as the 2D deck
    /// greys it: clouds are white, and the dome is blue only through the gaps,
    /// other cloud everywhere else, so half greyed under a few clouds and
    /// fully under a closed layer (an overcast is grey, not the clear sky's
    /// blue turned down). Its luminance is the dome's. Crate-private: `shade`
    /// applies it, and `Env::base_layer` lights the ground under the layer
    /// with it.
    pub(crate) fn sky_light(&self, ambient: Vec3f) -> Vec3f {
        let grey = luminance(ambient);
        ambient.mix(vec3f(grey, grey, grey), self.greyed)
    }

    /// The fraction of light that passes the volume along `dir`: 1 - alpha.
    /// The opacity alone is marched; the light grid is not touched.
    pub fn transmittance_toward(&self, dir: Vec3f) -> f32 {
        let mut out = [CloudMarch::default()];
        self.march_core(dir, false, &[], &mut out);
        1.0 - out[0].alpha
    }

    /// What the volume hides along `dir`, read off its footprint without marching: the light pass's stand-in for it
    /// (cloud_classic.rs). A column hides the sky as the deck does on its footprint (`density`), so it stands for the
    /// deck's optical depth there, -ln(1 - deck_opacity), spread over the column's cloud height. A look crosses the
    /// shell on a slant, through every column between where it meets the base and where it leaves the top, and
    /// gathers each one's depth by the length it runs inside that column's cloud: straight up it is the column's own
    /// opacity, and low looks, whose long slanted paths cross many columns, hide more, as the march does. The
    /// footprint is read at points STAND_IN_SPACING_CELLS apart along the path, at most STAND_IN_POINTS (a longer path
    /// spaces them wider), and the result fades at the horizon as the march does. Nothing at and below the horizon.
    pub(crate) fn footprint_cover_toward(&self, dir: Vec3f) -> f32 {
        let length = dir.length();
        if !(length > 0.0) {
            return 0.0;
        }
        let d = dir / length;
        if !(d.y > 0.0) {
            return 0.0;
        }
        let horizontal = (1.0 - d.y * d.y).max(0.0).sqrt();
        let near = shell_distance(d.y, self.base);
        let far = shell_distance(d.y, self.top).min(RANGE_M / horizontal.max(1.0e-4));
        if !(far > near) {
            return 0.0;
        }
        let track_cells = (far - near) * horizontal / self.cell_m;
        let points = ((track_cells / STAND_IN_SPACING_CELLS).ceil() as usize).clamp(1, STAND_IN_POINTS);
        let ds = (far - near) / points as f32;
        let thickness = self.top - self.base;
        let mut depth = 0.0f32;
        for k in 0..points {
            let t = near + ds * k as f32;
            let cov = self.coverage(d.x * t, d.z * t);
            if !(cov > 0.0) {
                continue;
            }
            // The share of this stretch that runs below the column's cloud top, and the column's depth per metre of
            // its cloud height.
            let h = height_at(t, d.y);
            let column_m = cloud_top01(cov) * thickness;
            let inside = ((self.base + column_m - h) / (height_at(t + ds, d.y) - h)).clamp(0.0, 1.0);
            if inside > 0.0 {
                let column_depth = -(1.0 - deck_opacity(cov).min(0.999)).ln();
                depth += column_depth * inside * ds / column_m;
            }
        }
        (1.0 - (-depth).exp()) * smooth(0.0, HORIZON_FADE, d.y)
    }

    /// The march along `dir` into `out` (cuts.len() + 1 records): with no cuts out[0] is the whole march, the older
    /// one bit for bit; with cuts, the segments `split_segments` makes of the running totals.
    fn march_core(&self, dir: Vec3f, lit: bool, cuts: &[f32], out: &mut [CloudMarch]) {
        for m in out.iter_mut() {
            *m = CloudMarch::default();
        }
        let length = dir.length();
        // The negated tests also stop NaN.
        if !(length > 0.0) {
            return;
        }
        let d = dir / length;
        if !(d.y > 0.0) {
            return;
        }
        let fade = smooth(0.0, HORIZON_FADE, d.y);
        // The shell between the two heights, cut off at the range.
        let horizontal = (1.0 - d.y * d.y).max(0.0).sqrt();
        let near = shell_distance(d.y, self.base);
        let far = shell_distance(d.y, self.top).min(RANGE_M / horizontal.max(1.0e-4));
        if !(far > near) {
            return;
        }
        // Half the budget overhead, all of it toward the horizon, where the
        // path through the layer is longest.
        let budget = self.steps as f32;
        let steps = (budget * (1.0 - 0.5 * d.y)).round().max(8.0) as usize;
        let ds = (far - near) / steps as f32;
        // A fixed offset per direction turns the step pattern's banding into grain.
        let jitter = hash01((d.x * 4096.0) as i32, (d.y * 4096.0) as i32, (d.z * 4096.0) as i32, self.seed);
        let cos_sun = d.dot(self.sun_dir);
        let phases = if lit { phase_octaves(cos_sun) } else { [0.0; OCTAVES] };
        // The powder effect shows where the sun's side is seen from behind.
        let powder_weight = POWDER * 0.5 * (1.0 - cos_sun);
        let thickness = self.top - self.base;
        let mut t = near + jitter * ds;
        let mut i = 0usize;
        let (mut transmittance, mut sun, mut ambient, mut bounce) = (1.0f32, 0.0f32, 0.0f32, 0.0f32);
        // Clear air is crossed two steps at a time. A double step that lands in
        // cloud goes back one, and the next steps stay single, so a cloud's
        // edge is never jumped.
        let (mut single_left, mut doubled) = (0u32, false);
        // The split: a step's light lands in the segment that holds the point it
        // samples; out[seg] keeps the running totals as segment seg ended.
        let mut seg = 0usize;
        let mut next_cut = cuts.first().map_or(f32::INFINITY, |&h| shell_distance(d.y, h));
        while i < steps && transmittance > MIN_TRANSMITTANCE {
            let (x, z) = (d.x * t, d.z * t);
            let cov = self.coverage(x, z);
            if cov <= 0.0 {
                if single_left > 0 {
                    single_left -= 1;
                    t += ds;
                    i += 1;
                } else {
                    doubled = true;
                    t += 2.0 * ds;
                    i += 2;
                }
                continue;
            }
            if doubled {
                doubled = false;
                single_left = 2;
                t -= ds;
                i -= 1;
                continue;
            }
            single_left = 2;
            let h01 = ((height_at(t, d.y) - self.base) / thickness).clamp(0.0, 1.0);
            let density = self.density(x, z, h01, cov, true);
            if density > 0.0 {
                while t >= next_cut {
                    out[seg] = CloudMarch { alpha: transmittance, sun, ambient, bounce };
                    seg += 1;
                    next_cut = cuts.get(seg).map_or(f32::INFINITY, |&h| shell_distance(d.y, h));
                }
                let step_transmittance = (-density * self.sigma * ds).exp();
                // What this step scatters toward the eye, seen through what is
                // in front of it. Integrated over the step, so thick steps
                // neither gain nor lose energy.
                let scattered = transmittance * (1.0 - step_transmittance);
                if lit {
                    sun += scattered * sun_light(self.light_depth(x, h01, z), &phases, powder_weight);
                    // The sky table by direction: the dome above reaches the
                    // top whole and the base in part; the ground below lights
                    // the base.
                    ambient += scattered * (AMBIENT_FLOOR + (1.0 - AMBIENT_FLOOR) * h01);
                    bounce += scattered * BOUNCE_SHARE * (1.0 - h01);
                }
                transmittance *= step_transmittance;
            }
            t += ds;
            i += 1;
        }
        let opaque = transmittance <= MIN_TRANSMITTANCE;
        if cuts.is_empty() {
            // Past the cut-off the little that is left counts as opaque, so a
            // thick cloud has no faint see-through.
            let alpha = if opaque { 1.0 } else { 1.0 - transmittance };
            out[0] = CloudMarch { alpha: alpha * fade, sun: sun * fade, ambient: ambient * fade, bounce: bounce * fade };
            return;
        }
        split_segments(out, seg, (transmittance, sun, ambient, bounce), opaque, fade);
    }

    /// The footprint at a ground position (metres east, metres south): the
    /// deck's density, faded out over the last quarter of the range (by the
    /// distance from the viewer; drift moves the pattern, not the range).
    fn coverage(&self, x: f32, z: f32) -> f32 {
        let distance = (x * x + z * z).sqrt();
        if !(distance < RANGE_M) {
            return 0.0;
        }
        let (px, pz) = (x - self.drift.x, z - self.drift.y);
        let footprint = self.deck.deck_density(vec2f(px / self.cell_m, pz / self.cell_m));
        footprint * (1.0 - smooth(0.75 * RANGE_M, RANGE_M, distance))
    }

    /// Cloud density at a point, 1 for solid cloud (SIGMA's extinction; a veil
    /// squeezed into a thin layer can go past it): `h01` is its height through
    /// the layer, `cov` the footprint under it. `detail` adds the edge erosion
    /// (the light grid does without it).
    fn density(&self, x: f32, z: f32, h01: f32, cov: f32, detail: bool) -> f32 {
        // A column hides the sky as the deck does on its footprint. The deck
        // draws a thin footprint translucent (0.1 is 40 % opaque); solid cloud
        // is opaque wherever it stands (SIGMA), so on a thin footprint it would
        // either vanish or open the deck's veil into blue gaps, and the sky
        // overhead opened when a held 2D preview was released. So a thin
        // footprint stands as a veil of the deck's own optical depth, and the
        // solid cloud takes over as the footprint thickens.
        let solid_share = smooth(SOLID_FROM, SOLID_AT, cov);
        // The solid cloud is taller where the cover is solid, low toward a
        // cloud's edge; a round base and a top that thins out.
        let top = 0.35 + 0.65 * cov;
        let profile = if solid_share > 0.0 { smooth(0.0, 0.1, h01) * (1.0 - smooth(0.5 * top, top, h01)) } else { 0.0 };
        // The veil fills the bottom of the layer, with soft edges.
        let slab = if solid_share < 1.0 { smooth(0.0, 0.1, h01) * (1.0 - smooth(0.6 * VEIL_TOP, VEIL_TOP, h01)) } else { 0.0 };
        if profile <= 0.0 && slab <= 0.0 {
            return 0.0;
        }
        let h = self.base + h01 * (self.top - self.base);
        let shape = self.noise.shape.sample((x - self.drift.x) * self.shape_scale, h * self.shape_scale, (z - self.drift.y) * self.shape_scale);
        let opacity = deck_opacity(cov);
        let mut d = 0.0;
        if profile > 0.0 {
            // Schneider's remap: the coverage sets how much of the shape noise
            // survives. Even solid cover keeps a threshold, so the tops are
            // carved into billows instead of ending in a flat fade. It reads
            // the deck's opacity, so as much of the sky is hidden as the deck
            // hides (through the density itself a footprint under 0.3 stood
            // almost nowhere).
            let mut solid = remap01(shape * profile, 1.0 - 0.9 * opacity, 1.0) * opacity;
            if solid > 0.0 && detail && self.erosion > 0.0 {
                let fine = self.noise.detail.sample((x - self.drift.x) * self.detail_scale, h * self.detail_scale, (z - self.drift.y) * self.detail_scale);
                // Wispy at the base, billowy higher up: the detail turns over with height.
                let fine = mix_f32(fine, 1.0 - fine, (4.0 * h01).min(1.0));
                solid = remap01(solid, self.erosion * EROSION_DEPTH * fine, 1.0);
            }
            d += solid * solid_share;
        }
        if slab > 0.0 {
            // The deck's optical depth at this footprint, -ln(1 - opacity),
            // spread through the slab (whose length is VEIL_LENGTH of the
            // layer) at density 1, and lumped by the shape noise (2 x shape
            // averages about 1). The erosion leaves it alone: it would cut a
            // density this thin away whole.
            let depth = -(1.0 - opacity.min(0.999)).ln();
            let veil = depth / (SIGMA * VEIL_LENGTH * (self.top - self.base)) * slab * 2.0 * shape;
            d += veil * (1.0 - solid_share);
        }
        d
    }

    /// Optical depth from a point toward the sun, read trilinearly from the
    /// light grid.
    fn light_depth(&self, x: f32, h01: f32, z: f32) -> f32 {
        let n = LIGHT_XZ;
        let grid = |v: f32| ((v / RANGE_M) * 0.5 + 0.5).clamp(0.0, 1.0) * (n - 1) as f32;
        let (gx, gz) = (grid(x), grid(z));
        let gy = h01.clamp(0.0, 1.0) * (LIGHT_Y - 1) as f32;
        let (x0, y0, z0) = ((gx as usize).min(n - 2), (gy as usize).min(LIGHT_Y - 2), (gz as usize).min(n - 2));
        let (tx, ty, tz) = (gx - x0 as f32, gy - y0 as f32, gz - z0 as f32);
        let plane = |slab: &[f32]| {
            let at = |x: usize, y: usize| slab[y * n + x];
            mix_f32(mix_f32(at(x0, y0), at(x0 + 1, y0), tx), mix_f32(at(x0, y0 + 1), at(x0 + 1, y0 + 1), tx), ty)
        };
        mix_f32(plane(self.slab(z0)), plane(self.slab(z0 + 1)), tz)
    }

    /// One slab of the light grid, built on first use. `OnceLock` makes the
    /// threads that need the same slab wait for the one building it.
    fn slab(&self, iz: usize) -> &[f32] {
        self.light[iz].get_or_init(|| {
            let n = LIGHT_XZ;
            let position = |i: usize| (i as f32 / (n - 1) as f32 * 2.0 - 1.0) * RANGE_M;
            let z = position(iz);
            let mut slab = Vec::with_capacity(n * LIGHT_Y);
            for iy in 0..LIGHT_Y {
                let h01 = iy as f32 / (LIGHT_Y - 1) as f32;
                for ix in 0..n {
                    slab.push(self.sun_depth(position(ix), h01, z));
                }
            }
            slab
        })
    }

    /// Optical depth from a point to the top of the layer, toward the sun.
    fn sun_depth(&self, x: f32, h01: f32, z: f32) -> f32 {
        let thickness = self.top - self.base;
        let reach = ((1.0 - h01) * thickness / self.light_dir.y).min(LIGHT_REACH_M);
        // LIGHT_STEPS steps, each twice the one before (1 + 2 + .. units), so
        // nearby cloud counts in detail and distant cloud in bulk.
        let mut length = reach / ((1u32 << LIGHT_STEPS) - 1) as f32;
        let (mut t, mut column) = (0.0f32, 0.0f32);
        for _ in 0..LIGHT_STEPS {
            let mid = t + 0.5 * length;
            let (px, pz) = (x + self.light_dir.x * mid, z + self.light_dir.z * mid);
            let cov = self.coverage(px, pz);
            if cov > 0.0 {
                column += self.density(px, pz, h01 + self.light_dir.y * mid / thickness, cov, false) * length;
            }
            t += length;
            length *= 2.0;
        }
        column * self.sigma
    }
}

/// A march of the whole sky at a fixed width, filled row by row on demand, for
/// bakes wider than the march should be. Rows are equirect rows in the map's
/// own frame; a lookup is bilinear (u wraps, v clamps), as `EnvMap::sample_uv`.
/// A cache of a march split at `cuts` (design §2.2) keeps one record per
/// segment for each texel.
pub struct CloudCache {
    width: usize,
    height: usize,
    rows: Vec<OnceLock<Vec<[f32; 4]>>>,
    cuts: Vec<f32>,
}

impl CloudCache {
    /// An empty cache `width` x `width / 2`.
    pub fn new(width: usize) -> CloudCache {
        CloudCache::with_cuts(width, Vec::new())
    }

    /// An empty cache `width` x `width / 2` of the march split at `cuts` (`VolumetricClouds::march_split`).
    pub fn with_cuts(width: usize, cuts: Vec<f32>) -> CloudCache {
        let width = width.max(2);
        let height = (width / 2).max(1);
        CloudCache { width, height, rows: (0..height).map(|_| OnceLock::new()).collect(), cuts }
    }

    /// The march along `dir`, read from the cache; the rows it needs are
    /// marched first if no thread has yet. (Its first segment, of a cache with cuts.)
    pub fn sample(&self, clouds: &VolumetricClouds, dir: Vec3f) -> CloudMarch {
        let mut out = [CloudMarch::default()];
        self.sample_segments(clouds, dir, &mut out);
        out[0]
    }

    /// Every segment's march along `dir` (up to out's length), read bilinearly from the cache.
    pub fn sample_segments(&self, clouds: &VolumetricClouds, dir: Vec3f, out: &mut [CloudMarch]) {
        let segments = self.cuts.len() + 1;
        let uv = ibl::dir_to_equirect_uv(arr(dir));
        let fx = uv[0] * self.width as f32 - 0.5;
        let fy = uv[1] * self.height as f32 - 0.5;
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let column = |x: f32| (x as i64).rem_euclid(self.width as i64) as usize;
        let row = |y: f32| (y as i64).clamp(0, self.height as i64 - 1) as usize;
        let (xa, xb) = (column(x0), column(x0 + 1.0));
        let (top, bottom) = (self.row(clouds, row(y0)), self.row(clouds, row(y0 + 1.0)));
        for (s, m) in out.iter_mut().enumerate().take(segments) {
            let (a, b) = (xa * segments + s, xb * segments + s);
            let mut v = [0.0f32; 4];
            for k in 0..4 {
                v[k] = mix_f32(mix_f32(top[a][k], top[b][k], tx), mix_f32(bottom[a][k], bottom[b][k], tx), ty);
            }
            *m = CloudMarch { alpha: v[0], sun: v[1], ambient: v[2], bounce: v[3] };
        }
    }

    /// How many rows have been marched (for tests and curiosity).
    pub fn rows_marched(&self) -> usize {
        self.rows.iter().filter(|row| row.get().is_some()).count()
    }

    fn row(&self, clouds: &VolumetricClouds, y: usize) -> &[[f32; 4]] {
        self.rows[y].get_or_init(|| {
            let v = (y as f32 + 0.5) / self.height as f32;
            let segments = self.cuts.len() + 1;
            let mut marches = vec![CloudMarch::default(); segments];
            let mut row = Vec::with_capacity(self.width * segments);
            for x in 0..self.width {
                let d = vec(ibl::equirect_uv_to_dir([(x as f32 + 0.5) / self.width as f32, v]));
                if self.cuts.is_empty() {
                    marches[0] = clouds.march(d);
                } else {
                    clouds.march_split(d, &self.cuts, &mut marches);
                }
                row.extend(marches.iter().map(|m| [m.alpha, m.sun, m.ambient, m.bounce]));
            }
            row
        })
    }
}

/// The same settings with the volume switched off: the clouds fall back to the
/// 2D deck on the same footprint. The app bakes this while a control is held,
/// since the march is far too slow to follow a drag.
pub fn layer2d_params(params: &HdriParams) -> HdriParams {
    let mut flat = params.clone();
    flat.sky.volumetric = None;
    flat
}

/// The split march's records (design §2.2). out[..seg] hold the running totals (transmittance, sun, ambient, bounce)
/// as each segment ended, before the march stopped; `last` the totals at its end. With A_k = fade x (1 - P_k) the
/// whole's opacity before segment k (P_k its transmittance there, 0 from the segment the march counted as opaque on),
/// segment k becomes opacity 1 - (1 - A_(k+1)) / (1 - A_k) and light fade x (its share of the totals) / (1 - A_k):
/// composed front to back the segments are the whole march, horizon fade and opaque cut included.
fn split_segments(out: &mut [CloudMarch], seg: usize, last: (f32, f32, f32, f32), opaque: bool, fade: f32) {
    let (mut before_light, mut faded_before) = ((0.0f32, 0.0f32, 0.0f32), 0.0f32);
    for k in 0..out.len() {
        let after = if k < seg { (out[k].alpha, out[k].sun, out[k].ambient, out[k].bounce) } else { last };
        let p_after = if opaque && k >= seg { 0.0 } else { after.0 };
        let faded_after = fade * (1.0 - p_after);
        let open = 1.0 - faded_before;
        out[k] = if open > 0.0 {
            CloudMarch {
                alpha: (1.0 - (1.0 - faded_after) / open).clamp(0.0, 1.0),
                sun: (fade * (after.1 - before_light.0) / open).max(0.0),
                ambient: (fade * (after.2 - before_light.1) / open).max(0.0),
                bounce: (fade * (after.3 - before_light.2) / open).max(0.0),
            }
        } else {
            CloudMarch::default()
        };
        before_light = (after.1, after.2, after.3);
        faded_before = faded_after;
    }
}

/// Distance along a ray of elevation sine `mu` (above 0) from the eye to the
/// shell `height` metres above the ground, on a curved Earth to second order
/// in the distance: height(t) = EYE + t mu + t^2 (1 - mu^2) / 2R. Written in
/// the form that stays exact straight up, where the curvature term vanishes.
/// The cloud sheets and slabs meet their planes here too (cloud_sheets.rs).
pub(crate) fn shell_distance(mu: f32, height: f32) -> f32 {
    let rise = height - EYE_M;
    let curve = (1.0 - mu * mu).max(0.0) / (2.0 * PLANET_M);
    2.0 * rise / (mu + (mu * mu + 4.0 * curve * rise).sqrt())
}

/// The Earth's numbers the GPU export's ray takes (design §12.3): `k_earth` (EYE_M, PLANET_M, RANGE_M,
/// MIN_TRANSMITTANCE); the shader's `shell_distance` and `height_at` mirror these.
pub(crate) fn gpu_layout(out: &mut crate::cloud_volume_gpu::UploadBuilder) {
    out.uniform("k_earth", [EYE_M, PLANET_M, RANGE_M, MIN_TRANSMITTANCE]);
}

/// Height above the ground after `t` metres along a ray of elevation sine `mu`.
pub(crate) fn height_at(t: f32, mu: f32) -> f32 {
    EYE_M + t * mu + t * t * (1.0 - mu * mu).max(0.0) / (2.0 * PLANET_M)
}

/// How high (0..1 through the layer) a column on a footprint of `cov` holds cloud, as `density` stands it: the solid
/// cloud's top, the veil's, mixed by the solid share.
fn cloud_top01(cov: f32) -> f32 {
    let solid_share = smooth(SOLID_FROM, SOLID_AT, cov);
    let solid = 0.35 + 0.65 * cov;
    VEIL_TOP + (solid - VEIL_TOP) * solid_share
}

/// The dual-lobe phase for each multiple-scattering octave: every octave
/// halves the anisotropy.
pub(crate) fn phase_octaves(cos_sun: f32) -> [f32; OCTAVES] {
    let mut phases = [0.0; OCTAVES];
    let mut round = 1.0f32;
    for phase in phases.iter_mut() {
        *phase = FORWARD_SHARE * hg_phase(cos_sun, G_FORWARD * round) + (1.0 - FORWARD_SHARE) * hg_phase(cos_sun, G_BACK * round);
        round *= 0.5;
    }
    phases
}

/// Sunlight reaching a point behind optical depth `tau`, per unit of sun
/// irradiance (1/sr): Beer-Lambert summed over the multiple-scattering
/// octaves, times the powder term.
pub(crate) fn sun_light(tau: f32, phases: &[f32; OCTAVES], powder_weight: f32) -> f32 {
    let (mut extinction, mut energy, mut sum) = (1.0f32, 1.0f32, 0.0f32);
    for phase in phases {
        sum += energy * (-extinction * tau).exp() * phase;
        extinction *= 0.5;
        energy *= 0.5;
    }
    // Powder: light that has entered little cloud has not yet scattered toward
    // the eye. It only darkens, so the result never exceeds Beer-Lambert's.
    let powder = 1.0 - (-2.0 * tau - POWDER_FLOOR).exp();
    SUN_GAIN * sum * (1.0 - powder_weight.clamp(0.0, 1.0) * (1.0 - powder))
}

/// (v - lo) / (hi - lo), clamped to 0..1; a step when the range is empty.
fn remap01(v: f32, lo: f32, hi: f32) -> f32 {
    if !(hi - lo > 1.0e-6) {
        return if v >= hi { 1.0 } else { 0.0 };
    }
    ((v - lo) / (hi - lo)).clamp(0.0, 1.0)
}

/// smoothstep under its own name, so no glob can make it ambiguous.
fn smooth(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dir_from_az_el;

    fn sun() -> Vec3f {
        dir_from_az_el(180.0, 40.0)
    }

    fn cover(coverage: f32) -> CloudParams {
        CloudParams { coverage, ..CloudParams::default() }
    }

    /// 24 steps keep the debug tests short.
    fn quick() -> VolumetricParams {
        VolumetricParams { steps: 24, ..VolumetricParams::default() }
    }

    fn volume(coverage: f32) -> VolumetricClouds {
        VolumetricClouds::new(&cover(coverage), &quick(), 1, sun()).expect("a volume")
    }

    /// Directions over the upper sky, clear of the horizon fade.
    fn sky_dirs() -> Vec<Vec3f> {
        let mut dirs = Vec::new();
        for el in (15..=85).step_by(10) {
            for az in (0..360).step_by(30) {
                dirs.push(dir_from_az_el(az as f32, el as f32));
            }
        }
        dirs
    }

    #[test]
    fn the_noise_tiles_spans_its_range_and_follows_its_seed() {
        let a = NoiseVolume::build(3, 16, &|| false, &serial_rows).unwrap();
        let b = NoiseVolume::build(3, 16, &|| false, &serial_rows).unwrap();
        assert_eq!(a.shape.data, b.shape.data, "the same seed builds the same bytes");
        assert_eq!(a.detail.data, b.detail.data);
        assert_ne!(a.shape.data, NoiseVolume::build(4, 16, &|| false, &serial_rows).unwrap().shape.data);
        // Rows spread over threads build the same bytes as one thread's loop, and a runner that skips rows (its job
        // overtaken) builds nothing, as a cancel does.
        let spread = NoiseVolume::build(3, 16, &|| false, &|n, row| on_threads(4, n, row)).unwrap();
        assert_eq!((&spread.shape.data, &spread.detail.data), (&a.shape.data, &a.detail.data), "a parallel build is the serial one");
        let skipping = |n: usize, row: &(dyn Fn(usize) + Sync)| (0..n).filter(|j| j % 2 == 0).for_each(row);
        assert_eq!(NoiseVolume::build(3, 16, &|| false, &skipping).err(), Some(Cancelled));
        assert_eq!((a.shape.size, a.detail.size), (16, 8));
        // The billow tile follows its seed too, and rows on threads build its bytes.
        let billow = a.billow(&|| false, &serial_rows).unwrap();
        assert_eq!(billow.data, spread.billow(&|| false, &|n, row| on_threads(4, n, row)).unwrap().data);
        assert_ne!(billow.data, NoiseVolume::build(4, 16, &|| false, &serial_rows).unwrap().billow(&|| false, &serial_rows).unwrap().data);
        for tile in [&a.shape, &a.detail, &*billow] {
            let mean = tile.data.iter().map(|&v| v as f32).sum::<f32>() / tile.data.len() as f32 / 255.0;
            let low = *tile.data.iter().min().unwrap();
            let high = *tile.data.iter().max().unwrap();
            assert!((0.3..0.7).contains(&mean), "mean {mean}");
            assert!(low <= 5 && high >= 250, "the bytes span {low}..{high}");
        }
        // It wraps: whole tiles further on is the same place, and the seam is continuous.
        for &(x, y, z) in &[(0.13f32, 0.52f32, 0.81f32), (0.97, 0.03, 0.5), (0.4, 0.99, 0.01)] {
            let here = a.shape.sample(x, y, z);
            assert!((0.0..=1.0).contains(&here), "{here}");
            assert!((here - a.shape.sample(x + 1.0, y - 2.0, z + 3.0)).abs() < 1.0e-3);
        }
        assert!((a.shape.sample(0.9999, 0.3, 0.3) - a.shape.sample(0.0001, 0.3, 0.3)).abs() < 0.02);
    }

    /// D15 (L:229): the first build of a seed's noise (about 2.2 s in
    /// release, inside `Env::new_with`, which the app's jobs checked their
    /// token only after) stops when its caller cancels: it polls before each
    /// slice of its two tiles and gives up at the first poll that says so.
    /// It keeps nothing, so the next build of the seed builds it whole, the
    /// same bytes as a build nothing stopped. A volume with nothing to march
    /// builds no noise, so it has nothing to stop. The billow tile a cloud
    /// volume reads is built on its first call, counted and stopped the same way
    /// (design §12.9 item 1). (Seeds no other test uses: the kept noise is
    /// shared by every thread.)
    #[test]
    fn the_first_noise_build_stops_when_cancelled() {
        use std::cell::Cell;
        let seed = 0x4837_0229;
        let polls = Cell::new(0usize);
        let at_the_third = || {
            polls.set(polls.get() + 1);
            polls.get() >= 3
        };
        assert_eq!(noise_for(seed, &at_the_third, &serial_rows).err(), Some(Cancelled));
        assert_eq!(polls.get(), 3, "it stops at the first poll that says so");
        let whole = Cell::new(0usize);
        let never = || {
            whole.set(whole.get() + 1);
            false
        };
        let noise = noise_for(seed, &never, &serial_rows).expect("nothing stopped it");
        assert_eq!(whole.get(), SHAPE_SIZE + SHAPE_SIZE / 2, "nothing was kept: one poll per slice of both tiles");
        let unstopped = NoiseVolume::build(seed, SHAPE_SIZE, &|| false, &serial_rows).unwrap();
        assert_eq!((&noise.shape.data, &noise.detail.data), (&unstopped.shape.data, &unstopped.detail.data));
        // The billow tile (design §12.9 item 1) is not built with them: a cloud volume's first call builds it, one poll
        // a slice, and one that is stopped keeps nothing, so the next builds it whole, the bytes of a build nothing
        // stopped; once built it has nothing left to stop.
        assert!(noise.billow.get().is_none(), "no billow tile until a volume asks for it");
        let polls = Cell::new(0usize);
        let at_the_fifth = || {
            polls.set(polls.get() + 1);
            polls.get() >= 5
        };
        assert_eq!(noise.billow(&at_the_fifth, &serial_rows).err(), Some(Cancelled));
        assert!(noise.billow.get().is_none() && polls.get() == 5, "a stopped billow build keeps nothing");
        let slices = Cell::new(0usize);
        let counted = || {
            slices.set(slices.get() + 1);
            false
        };
        let billow = noise.billow(&counted, &serial_rows).expect("nothing stopped it");
        assert_eq!(slices.get(), SHAPE_SIZE, "one poll per slice of the billow tile");
        assert!(Arc::ptr_eq(&billow, &noise.billow(&|| true, &serial_rows).expect("built: nothing left to stop")));
        assert_eq!(billow.data, unstopped.billow(&|| false, &serial_rows).unwrap().data);
        // The volume itself.
        let fresh = seed + 1;
        assert_eq!(VolumetricClouds::build(&cover(0.6), &quick(), fresh, sun(), &|| true, &serial_rows).err(), Some(Cancelled));
        assert!(matches!(VolumetricClouds::build(&cover(0.0), &quick(), fresh, sun(), &|| true, &serial_rows), Ok(None)), "no cover: no noise to stop");
        assert!(matches!(VolumetricClouds::build(&cover(0.6), &quick(), fresh, sun(), &|| false, &serial_rows), Ok(Some(_))));
        assert!(noise_for(fresh, &|| true, &serial_rows).expect("kept").billow.get().is_none(), "a classic volume builds no billow tile");
    }

    #[test]
    fn the_shell_is_met_where_the_curved_earth_puts_it() {
        // Straight up: the height itself, less the eye's own.
        assert!((shell_distance(1.0, 1500.0) - (1500.0 - EYE_M)).abs() < 0.5);
        // At 30 degrees of elevation, twice as far (the curve takes about a metre off).
        assert!((shell_distance(0.5, 1500.0) - 2.0 * (1500.0 - EYE_M)).abs() < 5.0);
        // Along the horizon a flat Earth never meets the layer; the sphere does at sqrt(2 R h).
        let level = shell_distance(1.0e-4, 1500.0);
        let geometric = (2.0 * PLANET_M * (1500.0 - EYE_M)).sqrt();
        assert!((level / geometric - 1.0).abs() < 0.02, "{level} vs {geometric}");
        // The two helpers agree: the height at a shell's distance is that shell's height.
        for &mu in &[1.0f32, 0.7, 0.3, 0.1, 0.02] {
            for &height in &[400.0f32, 1500.0, 4000.0, 12_000.0] {
                let back = height_at(shell_distance(mu, height), mu);
                assert!((back - height).abs() < 0.01 * height, "mu {mu}, height {height}: {back}");
            }
        }
    }

    #[test]
    fn no_cover_or_no_density_is_no_volume() {
        assert!(VolumetricClouds::new(&cover(0.0), &quick(), 1, sun()).is_none());
        assert!(VolumetricClouds::new(&cover(0.6), &VolumetricParams { density: 0.0, ..quick() }, 1, sun()).is_none());
        // Cirrus alone is the 2D veil's business.
        assert!(VolumetricClouds::new(&CloudParams { cirrus: 1.0, ..cover(0.0) }, &quick(), 1, sun()).is_none());
        assert!(VolumetricClouds::new(&cover(0.6), &quick(), 1, sun()).is_some());
    }

    #[test]
    fn the_march_is_empty_at_the_horizon_and_opaque_under_full_cover() {
        let clouds = volume(1.0);
        for az in (0..360).step_by(60) {
            for el in [0.0f32, -0.5, -30.0, -90.0] {
                let m = clouds.march(dir_from_az_el(az as f32, el));
                assert_eq!((m.alpha, m.sun, m.ambient, m.bounce), (0.0, 0.0, 0.0, 0.0), "az {az} el {el}");
            }
        }
        let up = clouds.march(vec3f(0.0, 1.0, 0.0));
        assert!(up.alpha > 0.95, "full cover is opaque overhead: {}", up.alpha);
        assert!(up.sun > 0.0 && up.ambient > 0.0 && up.sun.is_finite() && up.ambient.is_finite(), "{up:?}");
        // Seen from below, the base is what shows: it sees the ground.
        assert!(up.bounce > 0.0 && up.bounce.is_finite(), "{up:?}");
        assert!(clouds.transmittance_toward(vec3f(0.0, 1.0, 0.0)) < 0.05);
        assert_eq!(clouds.transmittance_toward(vec3f(0.0, -1.0, 0.0)), 1.0);
        // A zero or broken direction is no cloud, not a NaN.
        assert_eq!(clouds.march(Vec3f::default()), CloudMarch::default());
        assert_eq!(clouds.march(vec3f(f32::NAN, 1.0, 0.0)), CloudMarch::default());
    }

    #[test]
    fn more_coverage_hides_more_sky() {
        let share = |coverage: f32| {
            let clouds = volume(coverage);
            let dirs = sky_dirs();
            dirs.iter().map(|&d| clouds.march(d).alpha).sum::<f32>() / dirs.len() as f32
        };
        let (low, mid, high) = (share(0.2), share(0.5), share(0.8));
        assert!(low < mid && mid < high, "{low} {mid} {high}");
        assert!(high > 0.5, "0.8 coverage hides most of the sky: {high}");
    }

    #[test]
    fn the_volume_stands_on_the_decks_footprint() {
        let clouds = volume(0.5);
        let deck = CloudLayer::new(&cover(0.5), 1, sun()).unwrap();
        let cell = deck.cell_m();
        let (mut covered, mut clear) = (0, 0);
        // 32 km across, inside the range's fade.
        for i in -8..=8 {
            for j in -8..=8 {
                let (x, z) = (i as f32 * 2000.0, j as f32 * 2000.0);
                let footprint = deck.deck_density(vec2f(x / cell, z / cell));
                assert_eq!(clouds.coverage(x, z), footprint, "({x}, {z})");
                if footprint > 0.0 {
                    covered += 1;
                } else {
                    clear += 1;
                }
            }
        }
        assert!(covered > 20 && clear > 20, "half cover has both: {covered} covered, {clear} clear");
        // Straight up the march stays over one point of the deck: no footprint there, no cloud.
        let overhead = clouds.march(vec3f(0.0, 1.0, 0.0)).alpha;
        assert!(overhead == 0.0 || clouds.coverage(0.0, 0.0) > 0.0, "cloud overhead without a footprint");
        // Past the range there is no cloud at all.
        assert_eq!(clouds.coverage(RANGE_M, 0.0), 0.0);
        assert_eq!(clouds.coverage(0.0, -2.0 * RANGE_M), 0.0);
    }

    /// What a vertical column of the volume over the ground point (x, z) hides
    /// of the sky straight above it: its optical depth as an opacity.
    fn column_alpha(clouds: &VolumetricClouds, x: f32, z: f32) -> f32 {
        let cov = clouds.coverage(x, z);
        if cov <= 0.0 {
            return 0.0;
        }
        const LEVELS: usize = 48;
        let dh = (clouds.top - clouds.base) / LEVELS as f32;
        let density: f32 = (0..LEVELS).map(|i| clouds.density(x, z, (i as f32 + 0.5) / LEVELS as f32, cov, true)).sum();
        1.0 - (-density * clouds.sigma * dh).exp()
    }

    /// An opacity that reads as cloud: it hides more than a quarter of the sky behind it.
    const SEEN: f32 = 0.25;

    /// What the deck and the volume hide, summed over samples: the opacities,
    /// and how many of them read as cloud.
    #[derive(Default)]
    struct Hidden {
        deck: f32,
        volume: f32,
        deck_seen: f32,
        volume_seen: f32,
    }

    impl Hidden {
        fn add(&mut self, deck: f32, volume: f32) {
            self.deck += deck;
            self.volume += volume;
            self.deck_seen += (deck > SEEN) as u32 as f32;
            self.volume_seen += (volume > SEEN) as u32 as f32;
        }

        /// The volume over the deck: (opacity, share read as cloud).
        fn ratios(&self) -> (f32, f32) {
            (self.volume / self.deck, self.volume_seen / self.deck_seen)
        }
    }

    #[test]
    fn a_footprint_hides_as_much_sky_as_the_deck_draws_on_it() {
        // Column by column, the volume hides the sky above it as the deck does
        // on the same footprint: as much light, and as much that reads as
        // cloud. A thin footprint, which the deck draws translucent, must not
        // vanish (the sky would open on release) nor stand as fewer solid
        // clouds (the deck's veil would open into blue gaps). Columns 1 km
        // apart across the middle of the range, inside its fade; seeds 1 and 2
        // only (the noise keeps two).
        for coverage in [0.3f32, 0.5, 0.75] {
            let mut hidden = Hidden::default();
            for seed in [1u32, 2] {
                let clouds = VolumetricClouds::new(&cover(coverage), &quick(), seed, sun()).unwrap();
                for i in -16..=16 {
                    for j in -16..=16 {
                        let (x, z) = (i as f32 * 1000.0, j as f32 * 1000.0);
                        hidden.add(deck_opacity(clouds.coverage(x, z)), column_alpha(&clouds, x, z));
                    }
                }
            }
            let (opacity, seen) = hidden.ratios();
            assert!((opacity - 1.0).abs() < 0.1 && (seen - 1.0).abs() < 0.1, "coverage {coverage}: volume over deck {opacity} (opacity), {seen} (read as cloud)");
        }
    }

    #[test]
    fn the_upper_sky_is_as_cloudy_as_the_decks() {
        // A held drag shows the 2D deck and its release the volume, on the same
        // footprint, so the sky overhead must neither open nor close on
        // release. Above 45 degrees a ray crosses about one column of the
        // layer, so the volume hides there what the deck hides, sky by sky
        // (lower down it hides more: the towers' sides, seen across the field,
        // close the sky toward the horizon).
        let mut off = Vec::new();
        for seed in [1u32, 2] {
            for coverage in [0.35f32, 0.5, 0.65, 0.75] {
                let layer = CloudLayer::new(&cover(coverage), seed, sun()).unwrap();
                let clouds = VolumetricClouds::new(&cover(coverage), &quick(), seed, sun()).unwrap();
                let mut hidden = Hidden::default();
                for el in (45..=85).step_by(5) {
                    for az in (0..360).step_by(10) {
                        let dir = dir_from_az_el(az as f32, el as f32);
                        hidden.add(layer.sample(dir).alpha, 1.0 - clouds.transmittance_toward(dir));
                    }
                }
                let n = 9.0 * 36.0;
                let (deck, volume) = (hidden.deck / n, hidden.volume / n);
                let (deck_seen, volume_seen) = (hidden.deck_seen / n, hidden.volume_seen / n);
                if (volume - deck).abs() > 0.12 || (volume_seen - deck_seen).abs() > 0.1 {
                    off.push(format!(
                        "seed {seed} coverage {coverage}: opacity {deck:.2} deck, {volume:.2} volume; read as cloud {deck_seen:.2} deck, {volume_seen:.2} volume"
                    ));
                }
            }
        }
        assert!(off.is_empty(), "{off:#?}");
    }

    #[test]
    fn marching_is_deterministic() {
        let (a, b) = (volume(0.6), volume(0.6));
        for d in sky_dirs() {
            let (ma, mb) = (a.march(d), b.march(d));
            assert_eq!(ma.alpha.to_bits(), mb.alpha.to_bits());
            assert_eq!(ma.sun.to_bits(), mb.sun.to_bits());
            assert_eq!(ma.ambient.to_bits(), mb.ambient.to_bits());
            assert_eq!(ma.bounce.to_bits(), mb.bounce.to_bits());
        }
        // The noise is built once per seed and shared.
        assert!(Arc::ptr_eq(&a.noise, &b.noise));
        // Another seed is another sky.
        let other = VolumetricClouds::new(&cover(0.6), &quick(), 2, sun()).unwrap();
        let difference: f32 = sky_dirs().iter().map(|&d| (a.march(d).alpha - other.march(d).alpha).abs()).sum();
        assert!(difference > 1.0, "seeds 1 and 2 gave nearly the same clouds: {difference}");
    }

    #[test]
    fn light_reaches_the_top_of_the_layer_before_its_base() {
        let clouds = volume(1.0);
        let (top, base) = (clouds.light_depth(0.0, 1.0, 0.0), clouds.light_depth(0.0, 0.0, 0.0));
        assert!(top < 0.01, "nothing stands between the top and the sun: {top}");
        assert!(base > 2.0, "the whole layer stands between the base and the sun: {base}");
        let phases = phase_octaves(0.0);
        assert!(sun_light(top, &phases, 0.0) > 10.0 * sun_light(base, &phases, 0.0));
        // The powder term only ever darkens, most where little cloud is in the way.
        for tau in [0.0f32, 0.5, 2.0, 8.0] {
            assert!(sun_light(tau, &phases, 1.0) <= sun_light(tau, &phases, 0.0));
        }
        assert!(sun_light(0.0, &phases, 1.0) < 0.9 * sun_light(0.0, &phases, 0.0));
        // Forward scattering: looking at the sun through a cloud's edge is the brightest.
        assert!(phase_octaves(0.99)[0] > 5.0 * phase_octaves(0.0)[0]);
    }

    #[test]
    fn the_cache_marches_rows_on_demand_and_matches_the_march() {
        let clouds = volume(0.6);
        let cache = CloudCache::new(32);
        assert_eq!(cache.rows_marched(), 0);
        // At a texel centre the bilinear lookup is that texel: the march itself.
        let centre = vec(ibl::equirect_uv_to_dir([5.5 / 32.0, 3.5 / 16.0]));
        let (cached, direct) = (cache.sample(&clouds, centre), clouds.march(centre));
        assert!((cached.alpha - direct.alpha).abs() < 1.0e-3, "{cached:?} vs {direct:?}");
        assert!((cached.sun - direct.sun).abs() < 1.0e-3 * direct.sun.max(1.0), "{cached:?} vs {direct:?}");
        assert!(cache.rows_marched() <= 2, "one lookup marches only the two rows it reads");
        // Anywhere else it stays a valid sample.
        for d in sky_dirs() {
            let m = cache.sample(&clouds, d);
            assert!((0.0..=1.0).contains(&m.alpha) && m.sun >= 0.0 && m.ambient >= 0.0 && m.bounce >= 0.0, "{m:?}");
        }
        // 15..85 degrees of elevation live in the top eight of the sixteen rows.
        assert!(cache.rows_marched() <= 8, "{}", cache.rows_marched());
    }

    #[test]
    fn layer2d_params_drop_only_the_volume() {
        let mut p = HdriParams::default();
        p.sky.clouds.coverage = 0.6;
        p.sky.volumetric = Some(quick());
        let flat = layer2d_params(&p);
        assert_eq!(flat.sky.volumetric, None);
        let mut want = p.clone();
        want.sky.volumetric = None;
        assert_eq!(flat, want);
        assert_eq!(layer2d_params(&flat), flat);
    }

    /// The split (design §2.2): the march cut at heights inside the layer, each segment on the whole march's step grid
    /// and jitter and faded as part of the whole, composes front to back into the whole march within 1e-6 (of the
    /// larger of 1 and the value), through the horizon fade and the opaque cut alike. The cuts fall where they are asked
    /// for: cut at the shell's own base and top, the stretches below and above it are empty and the one between is the
    /// whole march, and a cut at 2000 m leaves cloud on both sides of it. A cache keeps every segment.
    #[test]
    fn a_split_march_composes_to_the_whole() {
        let near = |a: f32, b: f32| (a - b).abs() <= 1.0e-6 * a.abs().max(b.abs()).max(1.0);
        let cut_sets: [&[f32]; 3] = [&[2000.0], &[1800.0, 2600.0, 3500.0], &[1501.0, 3999.0]];
        let mut dirs = sky_dirs();
        dirs.extend([1.0f32, 2.0, 4.0, 6.0, 9.0].iter().map(|&el| dir_from_az_el(40.0, el)));
        dirs.push(vec3f(0.0, 1.0, 0.0));
        let mut split = 0usize;
        for coverage in [0.4f32, 0.8, 1.0] {
            let clouds = volume(coverage);
            // Cut at the shell's own ends: nothing below its base, nothing above its top, the whole march between.
            let (base, top) = clouds.shell_m();
            let mut ends = [CloudMarch::default(); 3];
            for &d in &dirs {
                clouds.march_split(d, &[base, top], &mut ends);
                let whole = clouds.march(d);
                assert!(ends[0] == CloudMarch::default() && ends[2] == CloudMarch::default(), "coverage {coverage}, {d:?}: outside the shell {:?} and {:?}", ends[0], ends[2]);
                let m = ends[1];
                assert!(near(m.alpha, whole.alpha) && near(m.sun, whole.sun) && near(m.ambient, whole.ambient) && near(m.bounce, whole.bounce), "coverage {coverage}, {d:?}: the shell's segment {m:?} against {whole:?}");
                let mut halves = [CloudMarch::default(); 2];
                clouds.march_split(d, &[2000.0], &mut halves);
                split += (halves[0].alpha > 0.0 && halves[1].alpha > 0.0) as usize;
            }
            for cuts in cut_sets {
                let mut out = vec![CloudMarch::default(); cuts.len() + 1];
                for &d in &dirs {
                    clouds.march_split(d, cuts, &mut out);
                    let whole = clouds.march(d);
                    let (mut t, mut sun, mut ambient, mut bounce) = (1.0f32, 0.0f32, 0.0f32, 0.0f32);
                    for m in &out {
                        assert!((0.0..=1.0).contains(&m.alpha) && m.sun >= 0.0 && m.ambient >= 0.0 && m.bounce >= 0.0, "coverage {coverage}, cuts {cuts:?}, {d:?}: {m:?}");
                        sun += t * m.sun;
                        ambient += t * m.ambient;
                        bounce += t * m.bounce;
                        t *= 1.0 - m.alpha;
                    }
                    assert!(
                        near(1.0 - t, whole.alpha) && near(sun, whole.sun) && near(ambient, whole.ambient) && near(bounce, whole.bounce),
                        "coverage {coverage}, cuts {cuts:?}, {d:?}: composed ({}, {sun}, {ambient}, {bounce}) against {whole:?}",
                        1.0 - t
                    );
                }
            }
        }
        assert!(split >= 20, "a cut at 2000 m leaves cloud on both sides along only {split} directions");
        let clouds = volume(0.6);
        let cuts = vec![2200.0f32];
        let cache = CloudCache::with_cuts(32, cuts.clone());
        let centre = vec(ibl::equirect_uv_to_dir([5.5 / 32.0, 3.5 / 16.0]));
        let (mut cached, mut direct) = ([CloudMarch::default(); 2], [CloudMarch::default(); 2]);
        cache.sample_segments(&clouds, centre, &mut cached);
        clouds.march_split(centre, &cuts, &mut direct);
        for (c, d) in cached.iter().zip(&direct) {
            assert!((c.alpha - d.alpha).abs() < 1.0e-3 && (c.sun - d.sun).abs() < 1.0e-3 * d.sun.max(1.0), "{c:?} against {d:?}");
        }
    }

    /// Runs every index on `threads` scoped threads, interleaved, as the bake pool's fan_out would.
    fn on_threads(threads: usize, n: usize, f: &(dyn Fn(usize) + Sync)) {
        std::thread::scope(|scope| {
            for k in 0..threads {
                scope.spawn(move || {
                    let mut i = k;
                    while i < n {
                        f(i);
                        i += threads;
                    }
                });
            }
        });
    }
}

#[cfg(test)]
mod env_tests {
    use super::*;
    use crate::Env;

    fn cloudy(coverage: f32, volumetric: bool) -> HdriParams {
        let mut p = HdriParams::default();
        p.sky.sun.mode = "manual".to_string();
        p.sky.sun.elevation_deg = 40.0;
        p.sky.sun.azimuth_deg = 180.0;
        p.sky.clouds.coverage = coverage;
        if volumetric {
            p.sky.volumetric = Some(VolumetricParams { steps: 24, ..VolumetricParams::default() });
        }
        p
    }

    fn serial(n: usize, f: &(dyn Fn(usize) + Sync)) {
        for i in 0..n {
            f(i)
        }
    }

    #[test]
    fn the_volume_replaces_the_deck_and_bakes_the_same_every_time() {
        let flat = Env::new(&cloudy(0.6, false)).bake(32);
        let marched = Env::new(&cloudy(0.6, true)).bake(32);
        assert_ne!(marched, flat, "the volume is not the 2D deck");
        assert_eq!(marched, Env::new(&cloudy(0.6, true)).bake(32), "the same settings bake the same map");
        assert!(marched.data.iter().all(|t| t.iter().all(|v| v.is_finite() && *v >= 0.0)));
        // No cover: both modes are the clear sky.
        assert_eq!(Env::new(&cloudy(0.0, true)).bake(32), Env::new(&cloudy(0.0, false)).bake(32));
        // Cirrus alone: the veil is the 2D one in both modes.
        let mut veil = cloudy(0.0, true);
        veil.sky.clouds.cirrus = 0.8;
        let mut flat_veil = veil.clone();
        flat_veil.sky.volumetric = None;
        assert_eq!(Env::new(&veil).bake(32), Env::new(&flat_veil).bake(32));
    }

    /// D8: with the Volumetric tick on, Density 0 draws no clouds. The cloud
    /// mode is the tick's, not whether a volume could be built, so dragging
    /// Density to 0 no longer brings the flat deck back at full cover after
    /// the volume thinned to nothing at 0.01 (L:237). The cirrus veil above
    /// stays the 2D one, as at any density.
    #[test]
    fn density_zero_with_the_tick_on_draws_no_clouds() {
        let mut thin = cloudy(0.6, true);
        thin.sky.clouds.cirrus = 0.3;
        thin.sky.volumetric.as_mut().unwrap().density = 0.0;
        let mut veil = thin.clone();
        veil.sky.volumetric = None;
        veil.sky.clouds.coverage = 0.0;
        let env = Env::new(&thin);
        assert!(env.volumetric.is_none(), "nothing to march");
        assert_eq!(env.bake(32), Env::new(&veil).bake(32), "the sky with its cirrus veil, no deck");
        assert_eq!(env.sun(), Env::new(&veil).sun(), "and a key no deck dims");
    }

    #[test]
    fn full_cover_dims_the_sun_key() {
        let clear = Env::new(&cloudy(0.0, true)).sun().expect("the sun is up");
        let overcast = Env::new(&cloudy(1.0, true)).sun().expect("still the key, only dimmed");
        assert!(
            luminance(overcast.radiance) < 0.05 * luminance(clear.radiance),
            "{:?} vs {:?}",
            overcast.radiance,
            clear.radiance
        );
        assert!((overcast.dir - clear.dir).length() < 1.0e-6);
    }

    #[test]
    fn the_ground_lights_the_cloud_bases() {
        // The ambient comes from the sky table by direction: under a closed
        // volume the zenith shows the base, which sees the ground. Snow under
        // it brightens the underside; dark soil hardly does.
        let underside = |albedo: f32| {
            let mut p = cloudy(1.0, true);
            p.sky.sun_disc.visible = false;
            p.sky.atmosphere.ground_color = [albedo; 3];
            luminance(Env::new(&p).radiance(vec3f(0.0, 1.0, 0.0)))
        };
        let (snow, soil) = (underside(0.8), underside(0.05));
        assert!(snow.is_finite() && soil > 0.0, "{snow} {soil}");
        assert!(snow > 1.2 * soil, "snow {snow} vs dark ground {soil}");
    }

    #[test]
    fn a_closed_volume_is_grey_as_a_closed_deck_is() {
        // A closed layer sees no blue sky: the 2D deck greys the dome it
        // scatters by its cover, and the volume greys it the same way, both
        // the dome on the cloud and the dome that lights the ground under it.
        // So an overcast's underside is grey, not the clear sky's blue turned
        // down. Blue over red at the zenith, with a neutral ground under it:
        let tint = |volumetric: bool| {
            let mut p = cloudy(1.0, volumetric);
            p.sky.sun_disc.visible = false;
            p.sky.atmosphere.ground_color = [0.5; 3];
            let c = Env::new(&p).radiance(vec3f(0.0, 1.0, 0.0));
            c.z / c.x
        };
        let (deck, volume) = (tint(false), tint(true));
        assert!(volume.is_finite() && volume < 1.2 && (volume - deck).abs() < 0.2, "deck {deck}, volume {volume}");
    }

    #[test]
    fn past_the_march_cap_the_bake_reads_a_smaller_march() {
        let mut p = cloudy(0.6, true);
        // Without the disc: one texel of it would outweigh the whole sky in a mean.
        p.sky.sun_disc.visible = false;
        let env = Env::new(&p);
        // A 64-wide map whose clouds are marched 32 wide and read bilinearly,
        // against the same map marched texel by texel.
        let upsampled = env.bake_marched(64, 32);
        let direct = env.bake_marched(64, 64);
        assert_eq!(direct, env.bake(64), "under the cap a bake marches every texel");
        assert_ne!(upsampled, direct);
        let mean = |map: &ibl::EnvMap| map.data.iter().map(|t| t[1] as f64).sum::<f64>() / map.data.len() as f64;
        assert!((mean(&upsampled) / mean(&direct) - 1.0).abs() < 0.25, "{} vs {}", mean(&upsampled), mean(&direct));
        assert!(upsampled.data.iter().all(|t| t.iter().all(|v| v.is_finite() && *v >= 0.0)));
        // The row-parallel bake reads the same cache rows and agrees bit for bit.
        assert_eq!(env.bake_par_marched(64, 32, serial), upsampled);
        assert_eq!(env.bake_par(64, serial), direct);
    }

    /// Plan 2's N3 under a volume: the engine's lighting copy (the keyless
    /// bake) reads the same cloud march as the map, past the march cap too, so
    /// it is the map without its sun's disc and nothing else: texel for texel a
    /// bake of the same sky with the disc hidden, refined at the map's hot
    /// spots (plan 2's `every_presets_non_key_lights_survive_in_the_lighting_copy`
    /// compares the same way).
    #[test]
    fn a_volumetric_skys_lighting_copy_loses_only_the_suns_disc() {
        // The disc must show for the copy to lose it: under seed 1 a cloud
        // stands in front of the sun at azimuth 180 (the 2D deck has one there
        // too), so the sun walks round the sky to the first azimuth whose key
        // passes at least half of the clear key, the same for debug and release
        // noise. The footprint does not move with the sun, and the seed stays.
        let clear = luminance(Env::new(&cloudy(0.0, true)).sun().unwrap().radiance);
        let p = (0..12)
            .map(|i| {
                let mut p = cloudy(0.4, true);
                p.sky.sun.azimuth_deg = (180.0 + 30.0 * i as f32) % 360.0;
                p
            })
            .find(|p| Env::new(p).sun().is_some_and(|key| luminance(key.radiance) > 0.5 * clear))
            .expect("an azimuth where the sun shows through the clouds");
        let env = Env::new(&p);
        let map = env.bake_marched(64, 32);
        let keyless = env.bake_keyless_marched(&map, 32, serial).expect("the sun is the key");
        let mut off = p.clone();
        off.sky.sun_disc.visible = false;
        let without = Env::new(&off);
        let mut want = without.bake_marched(64, 32);
        let cache = without.cloud_cache(64, 32);
        let at = crate::Sampling::bake(64, cache.as_ref());
        crate::refine_hot_spots(&mut want, &|d: Vec3f| without.radiance_with(d, None, at), &env.hot_spots());
        assert!(keyless == want, "the keyless copy is the volumetric sky without its disc");
        assert!(keyless != map, "the disc left the copy");
    }
}
