//! The Stratus as a cloud volume (design §2.2's Stratus row, §7: Density is also its depth): a thin low layer marched
//! on the Cumulus' machinery (the footprint grid, the Cover solve, the sun-depth and tau_up grids, the march, the GPU
//! export's upload), with its own footprint recipe, shell and density. A child of `cloud_volume_cumulus`, so it reads
//! `VolumeShape`'s parts as that module does.
//!
//! - **Shell.** From Base up DEPTH_LEAST_M + DEPTH_PER_DENSITY_M x Density metres (150 to 500).
//! - **Footprint.** A broad mask (MASK_FREQ cycles a cell of Size, MASK_OCTAVES), stretched along the wind by Streaks
//!   (1 + STREAK_STRETCH x Streaks), over the Cover's threshold (design §3); its column channel is a slower unevenness
//!   (THICK_FREQ): the layer is thicker in places (kole_clouds' large-scale density field, design §2.4, as an idea).
//! - **Density.** Inside the footprint the layer fills a band of its shell that closes in where it thins: a patch is
//!   a lens, full only TAPER into the footprint's noise and thinning to a wisp at its edge; Ragged edges tear its thin
//!   outer part with the detail tile (RAGGED_SHARE of Size a tile); the unevenness and Mottling's soft patches of the
//!   shape tile (MOTTLE_SHARE of Size a tile, streaked as the mask) thin it, and toward full Mottling the patches turn
//!   into the billow tile's cells with gaps between them. Where it is thinnest its top falls TOP_FALL and its base
//!   rises BASE_RISE of the depth. In the band: a soft base (BASE_SOFT of the band), a flat top (TOP_SOFT) and more
//!   water up high (WATER_GROWTH); its density follows the tear and the mottle too.
//! - **Extinction.** STRATUS_SIGMA x Density^2 a metre: Density 0.2 is a veil the sun shines through (the key above
//!   0.3), 1 a dark grey ceiling.
//! - On the CPU (the fallback, design §12.12 item 2) the density has no fine detail; the Stratus' export shader
//!   (makepad-cloud-gpu's `export/stratus.rs`, design §12.13) reads these numbers as its `st_*` uniforms and adds the
//!   fine detail.

use super::*;

/// The layer's depth at Density 0 and per unit of Density (design §2.2: 150 + 350 x Density m).
pub(super) const DEPTH_LEAST_M: f32 = 150.0;
pub(super) const DEPTH_PER_DENSITY_M: f32 = 350.0;
/// Extinction a metre at Density 1, times Density squared: a vertical optical depth of about 0.8 at Density 0.2 (the
/// sun shines through), 3 at the default 0.35 and 40 at 1.
const STRATUS_SIGMA: f32 = 0.12;
/// The mask: cycles a cell of Size (750 m patches at the default 6 km) and its octaves (the grid's 66 m cells hold
/// both from Size 1 up; finer raggedness is the 3D detail's).
const MASK_FREQ: f32 = 8.0;
const MASK_OCTAVES: u32 = 2;
/// The unevenness (the column channel): cycles a cell, and how much it thins the layer where it is low.
const THICK_FREQ: f32 = 1.5;
const UNEVEN: f32 = 0.5;
/// Streaks stretch the mask, the unevenness and the 3D noise along the wind by 1 + this x Streaks (4 at Streaks 1).
const STREAK_STRETCH: f32 = 3.0;
/// A slow domain warp the footprint grid carries in its warp channels (metres east and south, WARP_SHARE of Size at
/// WARP_FREQ cycles a cell): the 3D tiles' reads move by it, so their repeats (the cells' tile is 300 m at the default
/// Size) no longer line up into rays converging on the horizon.
const WARP_SHARE: f32 = 0.03;
const WARP_FREQ: f32 = 2.0;
/// Where the layer is thinnest its top falls this share of the depth and its base rises this one.
const TOP_FALL: f32 = 0.6;
const BASE_RISE: f32 = 0.3;
/// The band's soft base and flat top (shares of the band), and how much more water stands at its top than at its base.
const BASE_SOFT: f32 = 0.3;
const TOP_SOFT: f32 = 0.08;
const WATER_GROWTH: f32 = 0.45;
/// How far into the footprint's noise the layer reaches its full thickness (noise units over the threshold): about
/// a patch's middle, so a patch is a lens that thins over its whole width to a wisp at its edge, not a wall. At low
/// Cover the patches are small and their noise stands little over the threshold: the taper is then the noise's range
/// over the threshold up to its TAPER_TOP quantile, so a small patch still reaches its full thickness and hides what
/// its Cover says (a lens that never thickened hid half its footprint, design §3.4's share with any cloud past 2 c).
const TAPER: f32 = 0.3;
const TAPER_TOP: f32 = 0.95;
const TAPER_LEAST: f32 = 0.02;
/// The ragged edges' tile (a share of Size) and how soft a tear is (a share of the taper); a second tear at
/// TEAR_FINE_FREQ times the tile's frequency (cells of 50, 25 and 12 m at the default Size) takes TEAR_FINE_SHARE of
/// the tear, so an edge is torn at both scales.
const RAGGED_SHARE: f32 = 0.2;
const TEAR_SOFT: f32 = 0.15;
const TEAR_FINE_FREQ: f32 = 4.0;
const TEAR_FINE_SHARE: f32 = 0.4;
/// The mottle's tile (a share of Size: patches of a quarter of it and finer, 150 m at the default 6 km), the window of
/// the shape tile it keeps, and from what Mottling its patches turn into the billow tile's cells (half the tile).
const MOTTLE_SHARE: f32 = 0.1;
const MOTTLE_WINDOW: (f32, f32) = (0.3, 0.7);
const CELLS_FROM: f32 = 0.5;
/// The cells are the larger of two reads of the billow tile, the second at CELLS_SECOND times the first's frequency
/// (incommensurate) and turned by the rotation (CELLS_TURN: cos, sin), at CELLS_SECOND_HEIGHT of its height: one tile
/// repeating every 300 m lined its cells up into rays converging on the horizon.
const CELLS_SECOND: f32 = 0.687;
const CELLS_TURN: (f32, f32) = (0.8, 0.6);
const CELLS_SECOND_HEIGHT: f32 = 0.9;
/// How much of Mottling thins the layer's height (it thins its density by all of it): a mottle that thinned both
/// squared the contrast, and a thin veil's thin patches showed the sky as dark blotches.
const MOTTLE_THICK: f32 = 0.5;
/// The sine of the elevation a Stratus fades out under at the horizon (the compositor's own fade, sine 0.12, is the
/// flat decks'): a low layer reaches the horizon through its far band (`far_band`), so it fades over the last 0.2
/// degrees only.
const HORIZON_FADE_SIN: f32 = 0.004;
/// The far band (design §6.3's, for a layer): a look under the lowest one whose ray meets the layer's middle within
/// FAR_BAND_REACH of the march's range reads the march of a look above that one, mirrored over it and compressed by
/// FAR_BAND_FOLD (the band continues the layer's own texture; one row read down to the horizon drew a bright texel of it
/// as a bar), its depth the layer's true distance (so the haze turns it toward the sky as far off as it stands): the
/// layer reaches the horizon with no clear strip and no march.
const FAR_BAND_REACH: f32 = 0.9;
const FAR_BAND_FOLD: f32 = 0.5;
const EYE_M: f32 = 2.0;
const EARTH_M: f32 = 6_360_000.0;
/// The footprint grid key's part (the Cumulus' is CUMULUS_RECIPE).
const STRATUS_RECIPE: f32 = 7.0;

/// The Stratus footprint recipe: its rows that change the noise's spread.
#[derive(Clone, Copy, Debug)]
pub(super) struct StratusRecipe {
    /// The stretch along the wind (Streaks).
    aspect: f32,
}

impl StratusRecipe {
    /// The mask's noise and the unevenness at `f` (wind frame, cells) with the streams `s`: (noise, column, 0, 0, 1).
    fn at(&self, f: Vec2f, s: &Streams) -> Node {
        let root = self.aspect.sqrt();
        let (u, v) = (f.x / root, f.y * root);
        let n = fbm2(u * MASK_FREQ, v * MASK_FREQ, MASK_OCTAVES, s.0[0]);
        let column = fbm2(u * THICK_FREQ + 4.1, v * THICK_FREQ + 8.3, 2, s.0[1]);
        // the warp in cells (along, across); `recipe_m` turns it onto the plane in metres
        let wa = 2.0 * WARP_SHARE * (fbm2(f.x * WARP_FREQ + 1.7, f.y * WARP_FREQ + 5.3, 2, s.0[5]) - 0.5);
        let wc = 2.0 * WARP_SHARE * (fbm2(f.x * WARP_FREQ + 6.2, f.y * WARP_FREQ + 2.9, 2, s.0[6]) - 0.5);
        [n, column, wa, wc, 1.0]
    }

    /// The recipe at a ground point (metres east and south), its warp turned onto the plane in metres (what the
    /// footprint grid holds).
    fn at_m(&self, frame: &WindFrame, streams: &Streams, cell_m: f32, x: f32, z: f32) -> Node {
        let [n, column, wa, wc, strength] = self.at(frame.of(vec2f(x / cell_m, z / cell_m)), streams);
        let warp = frame.to_plane(vec2f(wa, wc)) * cell_m;
        [n, column, warp.x, warp.y, strength]
    }

    /// The fields its quantile tables are measured for (design §3.1's fingerprint).
    fn fields(&self) -> [f32; 1] {
        [self.aspect]
    }
}

/// A Stratus volume's own numbers beside `VolumeShape`'s.
pub(super) struct StratusShape {
    recipe: StratusRecipe,
    /// The layer's depth (metres), Mottling, Ragged edges.
    depth_m: f32,
    mottling: f32,
    ragged: f32,
    /// Metres a unit of the mask's noise spans at its edge (its wavelength): how far a point outside the footprint
    /// stands from cloud, as the GPU march asks.
    noise_m: f32,
    /// The mask's noise at its TAPER_TOP quantile (the taper's top).
    top: f32,
}

impl StratusShape {
    /// The recipe at a ground point (metres east and south): what the footprint grid holds.
    pub(super) fn recipe_m(&self, frame: &WindFrame, streams: &Streams, cell_m: f32, x: f32, z: f32) -> Node {
        self.recipe.at_m(frame, streams, cell_m, x, z)
    }

    /// The taper at the footprint's `threshold` (TAPER).
    fn taper(&self, threshold: f32) -> f32 {
        (self.top - threshold).clamp(TAPER_LEAST, TAPER)
    }

    /// Its uniforms for the GPU export's Stratus section (`st_a`..`st_e`; the tiles' scales ride in `noise`), its
    /// footprint's threshold `threshold`.
    pub(super) fn gpu_layout(&self, out: &mut crate::cloud_volume_gpu::UploadBuilder, threshold: f32) {
        out.uniform("st_a", [self.depth_m, self.noise_m, self.recipe.aspect, self.mottling]);
        out.uniform("st_b", [self.ragged, TOP_FALL, BASE_RISE, UNEVEN]);
        out.uniform("st_c", [BASE_SOFT, TOP_SOFT, WATER_GROWTH, TEAR_SOFT]);
        out.uniform("st_d", [MOTTLE_WINDOW.0, MOTTLE_WINDOW.1, CELLS_FROM, self.taper(threshold)]);
        out.uniform("st_e", [TEAR_FINE_FREQ, TEAR_FINE_SHARE, MOTTLE_THICK, 0.0]);
        out.uniform("st_f", [CELLS_SECOND, CELLS_TURN.0, CELLS_TURN.1, CELLS_SECOND_HEIGHT]);
    }
}

impl VolumeShape {
    /// The Stratus volume of `spec` (design §2.2), as `build` makes a Cumulus': None for no cover or no Density.
    pub(super) fn build_stratus(spec: &VolumeSpec, cancelled: &dyn Fn() -> bool, run: RowRunner) -> Result<Option<VolumeShape>, Cancelled> {
        let b = spec.layer.stratus_params();
        let c = spec.cover;
        if !(c > 0.0) || !(b.density > 0.0) {
            return Ok(None);
        }
        let cell_m = b.size_km * 1000.0;
        let stream = cloud_layer_seed(spec.map_seed, spec.layer.seed, spec.genus);
        let streams = Streams::of(stream);
        let drift_cells = spec.layer.drift_km.unwrap_or(0.0) * 1000.0 / cell_m;
        let frame = WindFrame::new(spec.downwind, drift_cells);
        let density = b.density.clamp(0.0, 1.0);
        let recipe = StratusRecipe { aspect: 1.0 + STREAK_STRETCH * b.streaks.clamp(0.0, 1.0) };
        let depth_m = DEPTH_LEAST_M + DEPTH_PER_DENSITY_M * density;
        let (base_m, top_m) = (b.base_m, b.base_m + depth_m);
        // Cloud stands anywhere in the shell over its footprint: the bounds are the shell's.
        let bounds = move |_: [f32; 2], _: [f32; 2]| (base_m, top_m);
        let margin = SkipMargin { across_m: 1.0, below_m: 0.0, above_m: 0.0 };
        let grid_key = footprint_grid_key(&[STRATUS_RECIPE, b.size_km, recipe.aspect, spec.downwind.x, spec.downwind.y, drift_cells, base_m, top_m], stream, GRID_SIZE, GRID_SPAN_M);
        let at_m = |x: f32, z: f32| recipe.at_m(&frame, &streams, cell_m, x, z);
        let grid = shared_footprint_grid(grid_key, &|| FootprintGrid::build(GRID_SIZE, GRID_SPAN_M, &at_m, &bounds, margin, cancelled, run))?;
        let noise = noise_for(spec.map_seed, cancelled, run)?;
        let billow = noise.billow(cancelled, run)?;
        let place = |k: u32| (hash_u32(stream ^ k.wrapping_mul(0x68e3_1da4)) >> 8) as f32 * (1.0 / 16_777_216.0);
        let sigma = STRATUS_SIGMA * density * density;
        let steps = spec.march_steps.clamp(STEPS_LEAST, STEPS_MOST);
        let bits = f32::from_bits;
        let key = footprint_grid_key(&[STRATUS_RECIPE, b.mottling, b.ragged_edges, density, sigma, bits(spec.map_seed), bits(grid_key as u32), bits((grid_key >> 32) as u32)], stream, TOP_COLS, 0.0);
        let set = quantile_set(fingerprint(spec.genus, FOOTPRINT_PART, &recipe.fields()));
        let (mottle_m, ragged_m) = (MOTTLE_SHARE * cell_m, RAGGED_SHARE * cell_m);
        let mut shape = VolumeShape {
            base_m,
            top_m,
            thickness: depth_m,
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
            // The Cumulus' tables are not read for a Stratus (its own density reads none).
            tops: vec![1.0; TOP_COLS],
            rise: vec![1.0; RISE_ROWS],
            rise_threshold: 0.0,
            levels: vec![0.0; LEVEL_ROWS + 1],
            // The mottle's and the ragged edges' tiles (the GPU's `noise` uniform carries them).
            shape_scale: 1.0 / mottle_m,
            detail_scale: 1.0 / ragged_m,
            detail_texel_m: ragged_m / noise.detail.size() as f32,
            detail_mean: noise.detail.mean(),
            noise,
            billow,
            offset: vec3f(place(1), place(2), place(3)),
            sigma,
            carve: 0.0,
            billow_cut: 0.0,
            span: 0.0,
            erosion: 0.0,
            lean: Vec2f::default(),
            steps,
            lattice_m: LATTICE_MOST_M,
            cover: c,
            stream,
            key,
            genus: spec.genus,
            stratus: None,
            deck: None,
            ns: None,
        };
        // Cover (design §3): the footprint's share solved for the upper sky's views, as a Cumulus'.
        let measured = Streams::of(MEASURE_SEED);
        let n = |_: u32, x: f32, y: f32| recipe.at(vec2f(x, y), &measured)[0];
        let top = set.table(1, &n).at(TAPER_TOP);
        shape.stratus = Some(StratusShape { recipe, depth_m, mottling: b.mottling.clamp(0.0, 1.0), ragged: b.ragged_edges.clamp(0.0, 1.0), noise_m: cell_m / MASK_FREQ, top });
        shape.solve_share(c, &set, &n, cancelled, run)?;
        shape.key = footprint_grid_key(&[bits(key as u32), bits((key >> 32) as u32), shape.threshold, shape.share, shape.gated as u32 as f32], stream, TOP_COLS, 0.0);
        Ok(Some(shape))
    }

    /// The Stratus' density at `p` without fine detail (the module's head), read at the footprint grid's level of
    /// `footprint_m`, as `cheap` gives it: no skin (the CPU does not erode it), the envelope the band the layer fills.
    pub(super) fn stratus_cheap(&self, st: &StratusShape, p: Vec3f, footprint_m: f32) -> Cheap {
        let h01 = (p.y - self.base_m) / self.thickness;
        let q = vec2f(p.x, p.z);
        let outside = Cheap { h01, q, ..Cheap::default() };
        if !(h01 > 0.0 && h01 < 1.0) || !(p.x.abs() < self.grid.half_m() && p.z.abs() < self.grid.half_m()) {
            return outside;
        }
        let [n, column, wx, wz, _] = self.grid.sample(p.x, p.z, footprint_m);
        if !(n > self.threshold) {
            return outside;
        }
        let f = if self.gated { self.remap.density(n, self.threshold, self.frame.of(q / self.cell_m)) } else { cover_density(n, self.threshold, FOOTPRINT_EDGE) };
        if !(f > 0.0) {
            return outside;
        }
        let air = Cheap { f, ..outside };
        // How far into the footprint: 0 at its edge, 1 from TAPER over the threshold (the tail gate's thinning kept).
        let gate = if self.gated { f / cover_density(n, self.threshold, FOOTPRINT_EDGE).max(1.0e-6) } else { 1.0 };
        let e = ((n - self.threshold) / st.taper(self.threshold)).clamp(0.0, 1.0) * gate;
        // The wind frame in metres at the warped point (the tiles' reads), the streaks' stretch.
        let w = self.frame.of(vec2f(q.x + wx, q.y + wz) / self.cell_m) * self.cell_m;
        let root = st.recipe.aspect.sqrt();
        let (u, v) = (w.x / root, w.y * root);
        // Ragged edges tear the thin outer part of each patch.
        let o = self.offset;
        let ds = self.detail_scale;
        let rg = self.noise.detail.sample(u * ds + o.x, p.y * ds + o.y, v * ds + o.z);
        let fs = ds * TEAR_FINE_FREQ;
        let rg2 = self.noise.detail.sample(u * fs + o.y, p.y * fs + o.z, v * fs + o.x);
        let tear = st.ragged * (rg + (rg2 - rg) * TEAR_FINE_SHARE);
        let keep = smoothstep(tear, tear + TEAR_SOFT, e);
        if !(keep > 0.0) {
            return air;
        }
        // Mottling: soft patches, turning into cells toward full Mottling; they thin the layer and its density.
        let ms = self.shape_scale;
        let patches = self.noise.shape.sample(u * ms + o.z, p.y * ms + o.x, v * ms + o.y);
        let cells = smoothstep(CELLS_FROM, 1.0, st.mottling);
        let m = if cells > 0.0 {
            let first = self.billow.sample(2.0 * u * ms + o.y, 2.0 * p.y * ms + o.z, 2.0 * v * ms + o.x);
            let k = 2.0 * CELLS_SECOND * ms;
            let (tu, tv) = (CELLS_TURN.0 * u - CELLS_TURN.1 * v, CELLS_TURN.1 * u + CELLS_TURN.0 * v);
            let second = CELLS_SECOND_HEIGHT * self.billow.sample(tu * k + o.x, p.y * k + o.y, tv * k + o.z);
            patches + (first.max(second) - patches) * cells
        } else {
            patches
        };
        let thin = 1.0 - smoothstep(MOTTLE_WINDOW.0, MOTTLE_WINDOW.1, m);
        let mottle = 1.0 - st.mottling * thin;
        // The band the layer fills: it closes in where it thins.
        let thick = keep * (1.0 - UNEVEN * (1.0 - column.clamp(0.0, 1.0))) * (1.0 - MOTTLE_THICK * st.mottling * thin);
        let (lo, hi) = (BASE_RISE * (1.0 - thick), 1.0 - TOP_FALL * (1.0 - thick));
        if !(h01 > lo && h01 < hi) {
            return air;
        }
        let x = (h01 - lo) / (hi - lo).max(1.0e-3);
        let profile = smoothstep(0.0, BASE_SOFT, x) * (1.0 - smoothstep(1.0 - TOP_SOFT, 1.0, x)) * (1.0 - WATER_GROWTH + WATER_GROWTH * x);
        let dens = keep.sqrt() * profile * mottle;
        Cheap { view: dens, h01, q, f, skin: 0.0, sun: dens, envelope: true, up: None }
    }
}

/// A Stratus' fade toward the horizon at the sine of a look's elevation (the compositor's `horizon_fade` for the other
/// types): over HORIZON_FADE_SIN, so its far band whitens into the haze instead of fading to the clear horizon.
pub(crate) fn horizon_fade(sin_el: f32) -> f32 {
    smoothstep(0.0, HORIZON_FADE_SIN, sin_el)
}

impl VolumeShape {
    /// The far band of a Stratus (design §6.3, for a layer): a look `d` (unit) under the lowest one whose ray meets the
    /// layer's middle within FAR_BAND_REACH of the march's range reads the look mirrored over that one (FAR_BAND_FOLD),
    /// with the layer's true distance along `d` (its march's depth); None above it (or for a look that is not up).
    pub(crate) fn far_band(&self, d: Vec3f) -> Option<(Vec3f, f32)> {
        if !(d.y > 0.0) || self.stratus.is_none() {
            return None;
        }
        // The sine at which the ray meets the layer's middle at the reach (the curved Earth drops it t^2 / 2R).
        let t = FAR_BAND_REACH * RANGE_M;
        let middle = self.base_m + 0.5 * self.thickness;
        let low = ((middle - EYE_M - t * t / (2.0 * EARTH_M)) / t).clamp(1.0e-4, 0.999);
        if !(d.y < low) {
            return None;
        }
        let horizontal = (d.x * d.x + d.z * d.z).sqrt().max(1.0e-6);
        let y = low + FAR_BAND_FOLD * (low - d.y);
        let k = (1.0 - y * y).sqrt() / horizontal;
        let raised = vec3f(d.x * k, y, d.z * k);
        Some((raised, shell_distance(d.y, middle)))
    }
}
