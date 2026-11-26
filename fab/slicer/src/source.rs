//! Contour sources: anything that can answer "what is the solid region of
//! this object at height z". Meshes are the first source; exact SDF and CSG
//! cross-sections plug in through the same trait.

use crate::geom::*;
use crate::mesh::Mesh;
use crate::paint::{leaves, split_layers, LayerPaint, PaintParams};
use crate::slice::{SliceIndex, SliceParams};

/// Paint read at a point in the source's frame (mm).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaintHere {
    /// Its state (0: the object's own filament, N: filament N); in a blend the nearer one.
    pub state: u32,
    /// A lower bound of the distance (mm) to the nearest point whose state differs (0 when
    /// unknown, and in a blend).
    pub dist: f64,
    /// A blend of two: the other state and its share (over 0, at most one half), which the
    /// paths dither.
    pub mix: Option<(u32, f32)>,
}

impl PaintHere {
    /// One state, `dist` from the nearest other.
    pub fn solid(state: u32, dist: f64) -> PaintHere {
        PaintHere { state, dist, mix: None }
    }
}

/// What a part's paint holds at a point in the source's frame (mm).
pub type PaintAt = std::sync::Arc<dyn Fn(f64, f64, f64) -> PaintHere + Send + Sync>;

/// A region of a part's paint, in the source's frame (mm): a support channel, or where a
/// region's print settings hold.
#[derive(Clone)]
pub struct SupportRegion {
    /// At a point: negative inside the region, and its size a lower bound of the distance (mm)
    /// to the region's edge.
    pub at: std::sync::Arc<dyn Fn(f64, f64, f64) -> f64 + Send + Sync>,
    /// The box it holds in, (min, max); `None`: it may hold anywhere.
    pub bounds: Option<([f64; 3], [f64; 3])>,
    /// Its outlines made so far, shared by its clones.
    pub outlines: RegionOutlines,
}

impl SupportRegion {
    /// The region `at` holds, within `bounds`, none of its outlines made yet.
    pub fn new(at: std::sync::Arc<dyn Fn(f64, f64, f64) -> f64 + Send + Sync>, bounds: Option<([f64; 3], [f64; 3])>) -> Self {
        SupportRegion { at, bounds, outlines: RegionOutlines::default() }
    }
}

/// A region's outlines as they are made, each by its plane and the box it was cut to (the bits of
/// z and of that box's corners): every part that holds the region (its clones), each layer of
/// theirs at the same plane, reads the one outline. The parts of an assembly share their frame and
/// their paint, so each takes the outlines the first made. They go with the region.
#[derive(Clone, Default)]
pub struct RegionOutlines(std::sync::Arc<std::sync::Mutex<std::collections::HashMap<[u64; 5], std::sync::Arc<Vec<ExPolygon>>>>>);

impl RegionOutlines {
    /// The outline at `key`, made by `make` unless it is in already. Two threads may both make one
    /// new outline; it is the same either way, and the first in stays.
    pub fn get_or_make(&self, key: [u64; 5], make: impl FnOnce() -> Vec<ExPolygon>) -> std::sync::Arc<Vec<ExPolygon>> {
        if let Some(made) = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&key) {
            return made.clone();
        }
        let made = std::sync::Arc::new(make());
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).entry(key).or_insert(made).clone()
    }
}

/// Where a part's paint says supports go.
#[derive(Clone, Default)]
pub struct SupportPaint {
    /// Supports never go here.
    pub block: Option<SupportRegion>,
    /// Supports always go here.
    pub enforce: Option<SupportRegion>,
    /// No tree support stands or grows here (`m.no_trees`): the trees keep out of it as they
    /// keep out of the part.
    pub no_trees: Option<SupportRegion>,
}

/// A region of a part with print settings of its own (`m.settings`): inside `region` the part
/// prints with these values (`fab_plate::REGION_SETTINGS` keys, checked: "100%", "gyroid", "4").
/// Where two overlap, the later one's value of a key wins.
#[derive(Clone)]
pub struct SettingsZone {
    pub region: SupportRegion,
    pub values: Vec<(String, String)>,
}

pub trait ContourSource {
    /// A key of what this source cuts: sources with equal keys are cut as one, so a cache may
    /// give one's contours, or what is made from them, to the other. None when it has none.
    fn content_key(&self) -> Option<u64> { None }
    /// Object bounds in mm, (min, max).
    fn bounds(&self) -> ([f64; 3], [f64; 3]);
    /// The XY box (min, max) the planner centres the part's patterns on: its infill, seam,
    /// ironing and texture, and its supports' anchor. The box of [`Self::bounds`]; a field
    /// part's own tree's, which no proof of its extent moves.
    fn pivot_box(&self) -> ([f64; 2], [f64; 2]) {
        let (lo, hi) = self.bounds();
        ([lo[0], lo[1]], [hi[0], hi[1]])
    }
    /// The solid regions at each plane height (mm), in bed coordinates.
    fn contours(&self, zs: &[f64]) -> Vec<Vec<ExPolygon>>;
    /// Whether parts of the object carry another filament than its own.
    fn painted(&self) -> bool {
        false
    }
    /// The mesh the regions are cut from, when there is one (copies of one
    /// mesh are planned once: `instance::classes`).
    fn mesh(&self) -> Option<&Mesh> {
        None
    }
    /// The solid regions per paint state at each plane (state 0 = the
    /// object's filament, N = filament N), given the planes' `contours`.
    fn paint_regions(&self, _zs: &[f64], slices: &[Vec<ExPolygon>], _p: &PaintParams) -> Vec<LayerPaint> {
        slices.iter().map(|s| vec![(0, s.clone())]).collect()
    }
    /// Paint by position, when the object has it: each path then prints in the state the
    /// paint holds where it runs ([`PaintAt`]), split where that changes, instead of by
    /// [`ContourSource::paint_regions`].
    fn paint_at(&self) -> Option<PaintAt> {
        None
    }
    /// Where its paint says supports go, when it says.
    fn support_paint(&self) -> Option<SupportPaint> {
        None
    }
    /// Its regions with print settings of their own (`m.settings`), in order: none for most
    /// parts.
    fn settings_zones(&self) -> Vec<SettingsZone> {
        Vec::new()
    }
    /// The part's non-planar top plan (NONPLANAR-DESIGN §3), in the same
    /// coordinates as its contours, when it has one: its planar layers leave
    /// out the plan's `cap_clip`, and its regions print as blocks.
    fn np_plan(&self) -> Option<std::sync::Arc<crate::nonplanar::NpPlan>> {
        None
    }
    /// The layers it prints when it says them itself (top z, height; bottom up, before a raft's
    /// lift): a test print's piece prints the print's own layers. None: they follow from its
    /// height and the job.
    fn layer_stack(&self) -> Option<std::sync::Arc<[(f64, f64)]>> {
        None
    }
    /// How steep the surface is per height band, for adaptive layer heights
    /// (`None`: unknown, the layers stay uniform). A mesh answers from its
    /// faces; an SDF from its gradient.
    fn slope_profile(&self, step: f64) -> Option<SlopeProfile> {
        self.mesh().map(|m| SlopeProfile::of_mesh(m, step))
    }
    /// The items printed into it ([`crate::embed`]), in the same coordinates as its contours:
    /// none for most parts. A source that wraps another says its inner one's.
    fn embeds(&self) -> Vec<crate::embed::Embed> {
        Vec::new()
    }
    /// The room its items take at plane `z`, which [`Self::contours`] leaves out: what the
    /// supports count as part, since an item stands there once the print resumes.
    fn cavity(&self, _z: f64) -> Vec<ExPolygon> {
        Vec::new()
    }
}

/// One island's walls and the fill region inside them.
#[derive(Clone, Debug, PartialEq)]
pub struct IslandWalls {
    pub shape: WallShape,
    /// The region inside the last wall.
    pub fill: Vec<ExPolygon>,
    /// Gap fill: centre lines through what the walls leave between them and the fill (classic
    /// walls only; variable-width walls take those places in their own beads).
    pub gaps: Vec<BeadLine>,
    /// The outer wall where the island is too thin for it to go round: one bead across, as wide
    /// as the island, none narrower than the narrowest printable bead (classic walls only).
    pub thin: Vec<BeadLine>,
    /// Classic wall loops only some places have (a region's own wall count, `m.settings`): their
    /// open pieces there, each its loop's `inset`. Variable-width walls keep theirs in `shape`.
    pub pieces: Vec<BeadLine>,
}

/// A fill region by class, after every rule: their union the fill region,
/// disjoint but where bottom and top overlap (there bottom wins).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Classes {
    pub top: Vec<ExPolygon>,
    /// Bottom surface (the first layer, a raft's contact) or external
    /// bridge, grown into its anchor.
    pub bottom: Vec<ExPolygon>,
    /// Internal solid infill.
    pub solid: Vec<ExPolygon>,
    pub sparse: Vec<ExPolygon>,
    /// Internal bridges: solid resting on sparse infill.
    pub bridge: Vec<ExPolygon>,
    /// Narrow solid strips over sparse infill, printed as concentric loops.
    pub vertical: Vec<ExPolygon>,
    /// The one-line anchor band of a floating shell under an internal bridge.
    pub band: Vec<ExPolygon>,
    /// The sparse infill a region's settings ([`SettingsZone`]) fill otherwise than the part: the
    /// fill's index in its plan and its share of `sparse`. A region that prints solid gives its
    /// share to `solid` instead.
    pub zoned: Vec<(usize, Vec<ExPolygon>)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum WallShape {
    /// Classic walls: each wall's centre-line rings, the outer wall first
    /// and in it the island's contour first (contours counter-clockwise,
    /// holes clockwise); a wall the island has no room for and those inside
    /// it are left out.
    Rings(Vec<Vec<Polygon>>),
    /// Variable-width beads.
    Beads(Vec<BeadLine>),
}

/// A variable-width bead line.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BeadLine {
    /// 0 = the outer wall; higher is further in.
    pub inset: usize,
    pub closed: bool,
    /// A middle bead (an odd bead count).
    pub odd: bool,
    pub pts: Vec<Pt>,
    /// The bead width (spacing, mm) at each point.
    pub widths: Vec<f64>,
}

/// The surface's steepness by height, for adaptive layers.
#[derive(Clone, Debug, PartialEq)]
pub struct SlopeProfile {
    /// Band k covers [z0 + k·step, z0 + (k+1)·step].
    pub z0: f64,
    pub step: f64,
    /// Per band, the largest |n_z| of the surface that is not flat (0 for
    /// vertical walls, near 1 for gentle slopes): the cusp height of a
    /// layer of height h there is about h·|n_z|.
    pub nz: Vec<f32>,
    /// Heights of horizontal faces (flat tops and bottoms), ascending: a
    /// layer boundary belongs exactly there.
    pub flats: Vec<f64>,
}

impl SlopeProfile {
    /// |n_z| above which a face counts as flat (about 1.8° from horizontal).
    pub const FLAT: f64 = 0.9995;

    pub fn empty(z0: f64, z1: f64, step: f64) -> SlopeProfile {
        let n = (((z1 - z0) / step).ceil().max(1.0)) as usize;
        SlopeProfile { z0, step, nz: vec![0.0; n], flats: Vec::new() }
    }

    /// The largest |n_z| over [za, zb].
    pub fn max_nz(&self, za: f64, zb: f64) -> f64 {
        if self.nz.is_empty() {
            return 0.0;
        }
        let k0 = (((za - self.z0) / self.step).floor().max(0.0) as usize).min(self.nz.len() - 1);
        let k1 = (((zb - self.z0) / self.step).ceil().max(0.0) as usize).min(self.nz.len());
        self.nz[k0..k1.max(k0 + 1)].iter().fold(0.0f32, |a, b| a.max(*b)) as f64
    }

    /// Record one surface sample.
    pub fn add(&mut self, za: f64, zb: f64, nz: f64) {
        let nz = nz.abs();
        if nz >= Self::FLAT || self.nz.is_empty() {
            return;
        }
        let k0 = (((za - self.z0) / self.step).floor().max(0.0) as usize).min(self.nz.len() - 1);
        let k1 = (((zb - self.z0) / self.step).ceil().max(0.0) as usize).clamp(k0 + 1, self.nz.len());
        for v in &mut self.nz[k0..k1] {
            *v = v.max(nz as f32);
        }
    }

    /// Sort and merge the flat heights (within 1 µm).
    pub fn finish(&mut self) {
        self.flats.sort_by(|a, b| a.partial_cmp(b).unwrap());
        self.flats.dedup_by(|a, b| (*a - *b).abs() < 1e-3);
    }

    pub fn of_mesh(m: &Mesh, step: f64) -> SlopeProfile {
        let (lo, hi) = m.bbox();
        let mut p = SlopeProfile::empty(lo[2], hi[2], step);
        let mut flat: Vec<(f64, f64)> = Vec::new();
        for t in &m.tris {
            let [a, b, c] = t.map(|i| m.verts[i as usize]);
            let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
            let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            if l < 1e-12 {
                continue;
            }
            let nz = n[2] / l;
            let (za, zb) = (a[2].min(b[2]).min(c[2]), a[2].max(b[2]).max(c[2]));
            if nz.abs() >= Self::FLAT {
                flat.push((0.5 * (za + zb), 0.5 * l));
                continue;
            }
            p.add(za, zb, nz);
        }
        // Flat heights with at least 1 mm² of face (not slivers).
        flat.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        let mut i = 0;
        while i < flat.len() {
            let (z, mut area) = flat[i];
            let mut j = i + 1;
            while j < flat.len() && flat[j].0 - z < 1e-3 {
                area += flat[j].1;
                j += 1;
            }
            if area >= 1.0 {
                p.flats.push(z);
            }
            i = j;
        }
        p.finish();
        p
    }
}

/// A mesh positioned on the bed.
pub struct MeshSource {
    pub mesh: Mesh,
    pub params: SliceParams,
    /// Built on the first cut and shared by every later one (any thread).
    index: std::sync::OnceLock<SliceIndex>,
}

impl MeshSource {
    pub fn new(mesh: Mesh) -> Self {
        Self { mesh, params: SliceParams::default(), index: std::sync::OnceLock::new() }
    }
}

impl ContourSource for MeshSource {
    fn bounds(&self) -> ([f64; 3], [f64; 3]) {
        self.mesh.bbox()
    }
    fn contours(&self, zs: &[f64]) -> Vec<Vec<ExPolygon>> {
        self.index.get_or_init(|| SliceIndex::new(&self.mesh)).cut(&self.mesh, zs, &self.params)
    }
    fn painted(&self) -> bool {
        self.mesh.paint.iter().any(|p| !p.is_empty())
    }
    fn mesh(&self) -> Option<&Mesh> {
        Some(&self.mesh)
    }
    fn paint_regions(&self, zs: &[f64], slices: &[Vec<ExPolygon>], p: &PaintParams) -> Vec<LayerPaint> {
        split_layers(&leaves(&self.mesh), zs, slices, p)
    }
}

/// A part painted by position: its own contours, its paths split by [`PaintAt`].
pub struct PaintedSource {
    pub inner: std::sync::Arc<dyn ContourSource + Send + Sync>,
    pub paint: PaintAt,
}

impl ContourSource for PaintedSource {
    fn bounds(&self) -> ([f64; 3], [f64; 3]) {
        self.inner.bounds()
    }
    fn pivot_box(&self) -> ([f64; 2], [f64; 2]) {
        self.inner.pivot_box()
    }
    fn contours(&self, zs: &[f64]) -> Vec<Vec<ExPolygon>> {
        self.inner.contours(zs)
    }
    fn painted(&self) -> bool {
        true
    }
    fn mesh(&self) -> Option<&Mesh> {
        // copies of one mesh plan once; a painted copy plans on its own
        None
    }
    fn paint_at(&self) -> Option<PaintAt> {
        Some(self.paint.clone())
    }
    fn np_plan(&self) -> Option<std::sync::Arc<crate::nonplanar::NpPlan>> {
        self.inner.np_plan()
    }
    fn slope_profile(&self, step: f64) -> Option<SlopeProfile> {
        self.inner.slope_profile(step)
    }
    fn embeds(&self) -> Vec<crate::embed::Embed> {
        self.inner.embeds()
    }
    fn cavity(&self, z: f64) -> Vec<ExPolygon> {
        self.inner.cavity(z)
    }
}
