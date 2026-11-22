//! Fab's toolpaths: per layer and object, the walls, solid shells and
//! infill as extrusion paths with role, width and height.
//!
//! Pipeline per object: layer heights → contours (any `ContourSource`) →
//! XY compensation → non-planar clipping → classic fixed-width walls →
//! surface classes (top, bottom, bridge, internal solid, sparse) with shell
//! propagation → infill lines.

pub mod adaptive;
pub mod arachne;
pub mod arcfront;
pub mod brim;
pub mod exp;
pub mod nonplanar;
pub mod clipper;
pub mod fill;
pub mod flow;
pub mod footing;
pub mod given;
pub mod infill;
pub mod interlock;
pub mod ironing;
#[cfg(test)]
mod infill_tests;
#[cfg(test)]
mod exp_tests;
pub mod layers;
pub mod lightning;
pub mod normal_support;
pub mod paint_split;
pub mod par;
pub mod pattern;
pub mod printcheck;
#[cfg(test)]
mod printcheck_tests;
#[cfg(test)]
mod gap_tests;
pub mod ramp;
mod raft;
pub mod patterns;
pub mod prof;
pub mod role;
pub mod coral;
mod settings_check;
pub mod skeleton;
pub mod stages;
pub mod support;
pub mod support_paint;
pub mod support_rules;
pub mod texture;
pub mod tree;
pub mod walls;
pub mod witness;

pub use brim::BrimKind;
pub use role::Role;
pub use walls::SeamMode;

use makepad_fab_settings::machine::Job;
use makepad_fab_slicer::clip::{difference_ex, intersection_ex, union_ex};
use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::offset::{offset_ex, offset_polygons, opening, Join, DEFAULT_MITER};
use makepad_fab_slicer::source::{BeadLine, Classes, ContourSource, IslandWalls, WallShape};
use std::sync::Arc;

/// Validate the job settings before planning.
pub fn check(job: &Job) -> Result<(), String> {
    brim::check(job)?;
    pattern::check(job)?;
    settings_check::check(job)
}

#[derive(Clone, Debug)]
pub struct ExtrusionPath {
    pub role: Role,
    pub pts: Vec<Pt>,
    /// A closed loop: `pts` starts at the seam and stops short of it by the
    /// seam gap.
    pub closed: bool,
    /// The whole ring from the seam, wiped along after `pts`: on a closed loop,
    /// and on the last run of a loop cut into runs (it too stops the seam gap
    /// short of the seam). Empty on every other path.
    pub loop_pts: Vec<Pt>,
    pub width: f64,
    pub height: f64,
    /// mm³ per mm of path.
    pub area: f64,
    /// Planned speed (mm/s) before cooling slowdowns.
    pub speed: f64,
    /// Segments moved without extruding (segment i ends at `pts[i + 1]`);
    /// empty when every segment extrudes.
    pub dry: Vec<bool>,
    /// Per-point Z offset (mm) from the layer's print z: a 3D path (spiral
    /// vase, scarf seam, non-planar block). Empty = planar at the layer z.
    /// The writer emits Z words for it and never arc-fits it.
    pub z: Vec<f64>,
    /// Per-segment flow multiplier on `area` (segment i ends at `pts[i + 1]`):
    /// ramps and variable-width beads. Empty = 1 everywhere.
    pub flow: Vec<f64>,
    /// The experimental mode it belongs to (`makepad_fab_gcode::record::modes`; 0 = none).
    pub mode: u8,
    /// A wall's overhang degree (0 supported … 5 over air), for the
    /// overhang fan.
    pub overhang: f32,
    /// Inside a part closed in on every side (two walls around, skins above and below): a fill
    /// there is the part's core, which the preview leaves out while the part shows whole, an
    /// inner wall the core's lining. Carried into the G-code as `; FAB_CORE:` and into
    /// `MoveRecord::flags`. False: the part's surface, drawn always.
    pub core: bool,
}

impl Default for ExtrusionPath {
    fn default() -> Self {
        ExtrusionPath { role: Role::None, pts: Vec::new(), closed: false, loop_pts: Vec::new(), width: 0.0, height: 0.0, area: 0.0, speed: 0.0, dry: Vec::new(), z: Vec::new(), flow: Vec::new(), mode: 0, core: false, overhang: 0.0 }
    }
}

impl ExtrusionPath {
    pub fn length_mm(&self) -> f64 {
        unscaled(Polyline::new(self.pts.clone()).length() as i64).max(0.0)
    }
    pub fn first(&self) -> Pt {
        self.pts[0]
    }
    pub fn last(&self) -> Pt {
        *self.pts.last().unwrap()
    }

    /// Its points and per-point values without the spare room their building left.
    pub fn exact(&mut self) {
        self.pts.shrink_to_fit();
        self.loop_pts.shrink_to_fit();
        self.dry.shrink_to_fit();
        self.z.shrink_to_fit();
        self.flow.shrink_to_fit();
    }
}

/// One island of one object on one layer, in print order.
#[derive(Clone, Debug, Default)]
pub struct Island {
    pub walls: Vec<ExtrusionPath>,
    pub fills: Vec<ExtrusionPath>,
}

impl Island {
    /// Its paths without the spare room their building left: a kept plan holds them exact.
    pub fn exact(&mut self) {
        for p in self.walls.iter_mut().chain(&mut self.fills) {
            p.exact();
        }
        self.walls.shrink_to_fit();
        self.fills.shrink_to_fit();
    }
}

#[derive(Clone, Debug, Default)]
pub struct ObjectLayer {
    pub object: usize,
    /// 0-based filament slot printing this part of the object (a painted
    /// object has one `ObjectLayer` per filament on a layer).
    pub filament: usize,
    pub islands: Vec<Arc<Island>>,
    pub slices: Arc<Vec<ExPolygon>>,
    /// A separate support layer; its slices are a support footprint, not
    /// model regions (normal supports may use a different layer height).
    pub support_only: bool,
}

impl ObjectLayer {
    /// Regions that vanish at the selected width remain available to
    /// analysis, but must not schedule a filament change or a tower wall.
    pub fn has_paths(&self) -> bool {
        self.islands.iter().any(|i| !i.walls.is_empty() || !i.fills.is_empty())
    }
}

#[derive(Clone, Debug, Default)]
pub struct Layer {
    pub index: usize,
    pub print_z: f64,
    pub height: f64,
    pub objects: Vec<ObjectLayer>,
}

/// A result as a plan makes it, for progressive display: how many region
/// streams the object plans (`Regions`, first), then each layer of each
/// region complete (`Layer`, in the order the layers finish). A painted
/// object plans one region per filament it prints in, `region` 0..count
/// (0: the object's own); every region stream covers every layer index.
pub enum Emit<'a> {
    Regions { count: usize },
    Layer { region: usize, index: usize, z: f64, h: f64, layer: &'a ObjectLayer },
    /// The part's supports are planned: [`stages::PartPlan::supports`] holds them from now on,
    /// without waiting for its [`stages::Stage::Finish`].
    Supports,
    /// A tree stage made one of the `of` shares of its work it tells (each once, in any order):
    /// how far the trees are, and `grown`, what the view shows of them on layer `layer` as they
    /// grow, in the part's frame.
    /// `frame`: where the part's frame's origin is in the plan's (what moves `grown` there).
    Tree { stage: tree::Stage, layer: usize, of: usize, grown: tree::Grown<'a>, frame: Pt },
}

/// An object to slice.
pub struct ObjectInput {
    pub name: String,
    pub source: Box<dyn ContourSource + Send + Sync>,
    /// 0-based filament slot printing this object.
    pub filament: usize,
    /// Per-part print setting overrides (key, value) on top of the job, e.g.
    /// an experimental mode for this part only. Empty = the job as it is.
    pub overrides: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
pub struct ObjectInfo {
    pub name: String,
    pub filament: usize,
    pub min: [f64; 3],
    pub max: [f64; 3],
    /// Its own print settings (what the G-code stage applies per object, such as acceleration).
    pub overrides: Vec<(String, String)>,
    /// The items printed into it ([`makepad_fab_slicer::embed`]): none for most objects.
    pub embeds: Vec<ObjectEmbed>,
}

impl ObjectInfo {
    /// Object `o`'s info as its plan has it: its source's box, and its items with the raft's
    /// lift `raft_lift` (mm) on their heights.
    pub fn of(o: &ObjectInput, raft_lift: f64) -> ObjectInfo {
        let (min, max) = o.source.bounds();
        let lift = |p: [f64; 3]| [p[0], p[1], p[2] + raft_lift];
        let embeds = o.source.embeds().into_iter().map(|e| ObjectEmbed { name: e.name, min: lift(e.min), max: lift(e.max), cover: e.cover + raft_lift, layer: None }).collect();
        ObjectInfo { name: o.name.clone(), filament: o.filament, min, max, overrides: o.overrides.clone(), embeds }
    }
}

/// An item printed into an object, where the print has it: its box and its cover
/// ([`makepad_fab_slicer::embed::Embed::cover`]) in print millimetres, raft included.
#[derive(Clone, Debug, PartialEq)]
pub struct ObjectEmbed {
    pub name: String,
    pub min: [f64; 3],
    pub max: [f64; 3],
    pub cover: f64,
    /// The plate layer of the object's first layer over it, the one the print pauses before for
    /// it to be placed ([`merge_named`] sets it). None: nothing of the object prints over it.
    pub layer: Option<usize>,
}

/// A pause in the print for items to be placed into their objects.
#[derive(Clone, Debug, PartialEq)]
pub struct Pause {
    /// The plate layer it comes before: the first that prints over its items.
    pub layer: usize,
    /// That layer's print height (mm).
    pub z: f64,
    /// Its items, as (object, index into the object's [`ObjectInfo::embeds`]).
    pub items: Vec<(usize, usize)>,
}

#[derive(Clone, Debug, Default)]
pub struct Toolpaths {
    pub layers: Vec<Layer>,
    pub objects: Vec<ObjectInfo>,
    /// Non-planar top blocks (NONPLANAR-DESIGN §3), by layer then object.
    pub np_blocks: Vec<NpBlock>,
}

impl Toolpaths {
    /// The print's pauses for the items printed into its objects, in layer order: one before each
    /// plate layer that first prints over an item, its items in object order.
    pub fn pauses(&self) -> Vec<Pause> {
        let mut out: Vec<Pause> = Vec::new();
        for (oi, info) in self.objects.iter().enumerate() {
            for (k, e) in info.embeds.iter().enumerate() {
                let Some(layer) = e.layer.filter(|&l| l < self.layers.len()) else { continue };
                match out.iter_mut().find(|p| p.layer == layer) {
                    Some(p) => p.items.push((oi, k)),
                    None => out.push(Pause { layer, z: self.layers[layer].print_z, items: vec![(oi, k)] }),
                }
            }
        }
        out.sort_by_key(|p| p.layer);
        out
    }
}

/// One region of a part's non-planar top: printed as one block right after
/// the planar layer at `z` (the last whose top is at or below the region's
/// schedule z). In a plate's [`Toolpaths`], `object` and `layer` index its
/// objects and layers; in an [`ObjectPlan`], `object` is 0 and `layer` is the
/// plan's own layer index.
#[derive(Clone, Debug)]
pub struct NpBlock {
    pub object: usize,
    pub layer: usize,
    pub z: f64,
    /// The region's id in the part's plan.
    pub region: usize,
    /// Its rings in print order, lowest shell first (`nonplanar::block_paths`):
    /// closed 3D paths (first point = last), `z` offsets from `z`, per-segment
    /// flow, never arc-fitted.
    pub paths: Vec<ExtrusionPath>,
}

impl NpBlock {
    /// Every path moved by `d` (exact: integer units).
    pub fn translate(&mut self, d: Pt) {
        for p in &mut self.paths {
            for q in p.pts.iter_mut() {
                *q = q.add(d);
            }
        }
    }
}

/// The planar layers of a part with a non-planar top leave out the plan's cap
/// clip: walls follow the clip edge (at or below the cap, under the shells),
/// and the area under the clip classifies as top skin under the shells.
/// The layer cut at `z` (its print z less any raft lift).
fn np_clip(np: &makepad_fab_slicer::nonplanar::NpPlan, z: f64, slices: Vec<ExPolygon>) -> Vec<ExPolygon> {
    if slices.is_empty() {
        return slices;
    }
    let clip = np.cap_clip(z);
    if clip.is_empty() {
        return slices;
    }
    // The clip leaves hair-thin slivers where its edge runs close to the part's outline or
    // to itself; no bead fits in them (they would plan degenerate widths). Those next to
    // the clip go; the rest of the layer keeps its exact outline.
    let rest = difference_ex(&slices, &clip);
    let thin = 0.1 * SCALE;
    let slivers = difference_ex(&rest, &makepad_fab_slicer::offset::opening(&rest, thin, thin, DEFAULT_MITER));
    if slivers.is_empty() {
        return rest;
    }
    let near = intersection_ex(&slivers, &offset_ex(&clip, 1.5 * thin, DEFAULT_MITER));
    if near.is_empty() { rest } else { difference_ex(&rest, &near) }
}

/// A part's non-planar regions as blocks after its planar layers.
fn np_blocks(np: &makepad_fab_slicer::nonplanar::NpPlan, heights: &[(f64, f64)], p: &Params) -> Vec<NpBlock> {
    let s = &np.settings;
    let area = flow::cross_section(s.line_w, s.layer_h);
    let speed = p.speed(Role::TopSurface, area, false);
    let tops: Vec<f64> = heights.iter().map(|h| h.0 - p.raft_lift).collect();
    let mut out = Vec::new();
    for r in &np.regions {
        // The last planar layer whose top is at or below the schedule z.
        let k = tops.partition_point(|&z| z <= r.schedule_z + 1e-9);
        if k == 0 {
            continue;
        }
        let z = heights[k - 1].0;
        let paths = nonplanar::block_paths(np, r, tops[k - 1], &tops, speed);
        out.push(NpBlock { object: 0, layer: k - 1, z, region: r.id, paths });
    }
    out
}

/// Settings the toolpath stage reads, resolved for one object's extruder
/// and filament.
#[derive(Clone, Debug)]
pub struct Params {
    pub layer_height: f64,
    pub first_layer_height: f64,
    pub nozzle: f64,
    pub w_outer: f64,
    pub w_inner: f64,
    pub w_sparse: f64,
    pub w_solid: f64,
    pub w_top: f64,
    pub w_first: f64,
    pub brim_kind: BrimKind,
    pub brim_width: f64,
    pub brim_gap: f64,
    pub wall_loops: usize,
    pub top_layers: usize,
    pub bottom_layers: usize,
    pub density: f64,
    pub sparse_pattern: pattern::Sparse,
    pub solid_pattern: pattern::Surface,
    pub top_pattern: pattern::Surface,
    pub bottom_pattern: pattern::Surface,
    /// Pattern details beyond the pattern names.
    pub sparse_opts: infill::SparseOpts,
    pub infill_direction: f64,
    pub bridge_direction: f64,
    pub bridge_flow: f64,
    /// The extra volume factor of an internal bridge (its bead is still sized by `bridge_flow`).
    pub internal_bridge_flow: f64,
    pub thick_bridges: bool,
    pub infill_overlap: f64,
    /// Monotonic top lines run on into the wall by this share of their width.
    pub travel_into_wall: f64,
    /// Floating vertical shells under internal bridges.
    pub floating_shells: bool,
    pub vertical_shells: bool,
    pub narrow_solid: bool,
    pub vertical_shell_speed: f64,
    pub seam_gap: f64,
    pub seam_mode: SeamMode,
    pub elephant_foot: f64,
    pub min_sparse_area: f64,
    pub top_one_wall: bool,
    pub one_wall_first: bool,
    pub raft_layers: usize,
    /// Raft tops and heights, empty without a raft.
    pub raft_heights: Vec<(f64, f64)>,
    pub raft_base_layers: usize,
    pub raft_gap: f64,
    pub raft_lift: f64,
    pub raft_expansion: f64,
    pub raft_first_expansion: f64,
    pub raft_first_density: f64,
    /// `wall_sequence`.
    pub wall_order: walls::WallOrder,
    pub precise_outer: bool,
    /// `wall_generator = arachne` (never in spiral vase mode).
    pub arachne: Option<arachne::Settings>,
    /// The bead rules classic gap fill runs on, and the narrowest gap it fills.
    pub gap_fill: arachne::Settings,
    pub overhang: walls::OverhangSpeeds,
    pub support: Option<support::SupportParams>,
    /// Use coral for automatic support.
    pub coral: bool,
    pub normal_support: Option<normal_support::NormalParams>,
    pub support_width: f64,
    pub support_speed: f64,
    pub support_interface_speed: f64,
    pub support_interface_spacing: f64,
    pub support_base_spacing: f64,
    /// Line density of the first support layer.
    pub support_first_density: f64,
    pub anchor: Anchor,
    pub anchor_max: Anchor,
    pub speeds: Speeds,
    /// Experimental modes (EXPSLICE); all off by default.
    pub exp: exp::Modes,
    /// How deep under the part's surface (mm) its paint prints as it is
    /// (`fab_paint_skin_depth`): the walls and surfaces always do, a fill this near too.
    pub paint_skin: f64,
    /// Deeper, a fill prints the paint in cells this large (mm), one material each; `None`:
    /// as it is through the whole part (`fab_paint_inside_exact`, for a part cut open).
    pub paint_inside_cell: Option<f64>,
    /// The first layer prints the paint too (`fab_paint_first_layer`: a picture printed face
    /// down); else it prints in the part's own filament.
    pub paint_first_layer: bool,
    /// Solid layers lay their lines on one grid through the part's middle, a spacing apart
    /// (`fab_solid_on_grid`: a picture, whose cells are its lines); else spread to fit.
    pub solid_on_grid: bool,
}

/// The skin of a painted part where its paint prints as it is, by default (mm).
pub const PAINT_SKIN: f64 = 0.5;
/// The cells a painted part's inside prints its paint in, one material each (mm).
pub const PAINT_INSIDE_CELL: f64 = 12.0;

/// Length of the anchor at a free infill line end.
#[derive(Clone, Copy, Debug)]
pub enum Anchor {
    Mm(f64),
    /// A multiple of the line spacing.
    OfSpacing(f64),
}

impl Anchor {
    pub fn resolve(self, spacing: f64) -> f64 {
        match self {
            Anchor::Mm(v) => v,
            Anchor::OfSpacing(k) => k * spacing,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Speeds {
    pub outer: f64,
    pub inner: f64,
    pub sparse: f64,
    pub solid: f64,
    pub top: f64,
    pub bridge: f64,
    pub gap: f64,
    pub first_layer: f64,
    pub first_layer_infill: f64,
    pub max_volumetric: f64,
    /// A bridge over sparse infill inside the part, when the job sets its own speed (a percent is
    /// of the bridge speed): those lines become internal bridges. None keeps them bridges.
    pub internal_bridge: Option<f64>,
}

/// Whether a job key is read by the supports alone: the support planners read it, and a part's
/// slice reads it only through what [`Params::without_supports`] keeps (none of the support
/// planners' parameters). A support setting therefore changes a part's supports, never its
/// slice. The raft's keys are the part's: a raft lifts every layer.
pub fn is_support_key(key: &str) -> bool {
    key.starts_with("support_") || key.starts_with("tree_support_") || key.starts_with("fab_tree_") || matches!(key, "enable_support" | "fab_support_inside" | "independent_support_layer_height")
}

pub(crate) fn line_width(job: &Job, key: &str, nozzle: f64, fallback: f64) -> f64 {
    let raw = job.cfg.raw(key, 0);
    let v = job.cfg.f64(key);
    let w = if raw.ends_with('%') { nozzle * v / 100.0 } else { v };
    if w <= 0.0 {
        fallback
    } else {
        w
    }
}

impl Params {
    pub fn from_job(job: &Job, filament: usize) -> Self {
        let e = job.ext_of(filament);
        let nozzle = job.nozzle_diameter(e).max(0.1);
        let lw = line_width(job, "line_width", nozzle, nozzle * 1.05);
        let lh = job.f("layer_height").max(0.01);
        let first_height = job.f("initial_layer_print_height").max(0.01);
        let raft_layers = job.cfg.i64_at("raft_layers", 0).max(0) as usize;
        let coral = job.s("support_type") == "coral(auto)";
        let mut raft_heights = Vec::new();
        let (mut raft_gap, mut raft_lift, mut raft_expansion, mut raft_first_expansion, mut raft_first_density) = (0.0, 0.0, 0.0, 0.0, 0.0);
        if raft_layers > 0 {
            let mut z = 0.0;
            for i in 0..raft_layers {
                let h = if i == 0 { first_height } else { lh.max(0.75 * nozzle) };
                z += h;
                raft_heights.push((z, h));
            }
            raft_gap = if job.f("support_top_z_distance") == 0.0 { 0.0 } else { job.f("raft_contact_distance").max(0.0) };
            if raft_gap > 0.0 && !job.b("independent_support_layer_height") { raft_gap = (raft_gap / lh - 1e-9).ceil().max(1.0) * lh; }
            raft_lift = z + raft_gap;
            raft_expansion = if coral || job.s("support_type").starts_with("tree") || job.s("support_style").starts_with("tree_") { 3.0 } else { job.f("raft_expansion") };
            raft_first_expansion = { let v = job.f("raft_first_layer_expansion"); if v < 0.0 { 2.0 } else { v } };
            raft_first_density = if job.cfg.has("raft_first_layer_density") { (job.f("raft_first_layer_density") / 100.0).clamp(0.0, 1.0) } else { 0.9 };
        }
        let support_width = if raft_layers > 0 || job.b("enable_support") { line_width(job, "support_line_width", nozzle, lw) } else { lw };
        let top_layers = (job.cfg.i64_at("top_shell_layers", 0).max(0) as usize).max((job.f("top_shell_thickness") / lh - 1e-6).ceil() as usize);
        let bottom_layers = (job.cfg.i64_at("bottom_shell_layers", 0).max(0) as usize).max((job.f("bottom_shell_thickness") / lh - 1e-6).ceil() as usize);
        let sp = |k: &str| job.ext(k, e);
        let overhang_override = job.cfg.bool_at("override_process_overhang_speed", filament);
        let overhang_speed = |k: &str| if overhang_override { job.eff(k, filament) } else { sp(k) };
        let mut p = Params {
            layer_height: lh,
            first_layer_height: first_height,
            nozzle,
            w_outer: line_width(job, "outer_wall_line_width", nozzle, lw),
            w_inner: line_width(job, "inner_wall_line_width", nozzle, lw),
            w_sparse: line_width(job, "sparse_infill_line_width", nozzle, lw),
            w_solid: line_width(job, "internal_solid_infill_line_width", nozzle, lw),
            w_top: line_width(job, "top_surface_line_width", nozzle, lw),
            w_first: line_width(job, "initial_layer_line_width", nozzle, lw),
            brim_kind: if raft_layers > 0 { BrimKind::NoBrim } else { brim::kind_of(job) },
            brim_width: if job.cfg.has("brim_width") { job.f("brim_width") } else { 5.0 },
            brim_gap: if job.cfg.has("brim_object_gap") { job.f("brim_object_gap") } else { 0.1 },
            wall_loops: job.cfg.i64_at("wall_loops", 0).max(0) as usize,
            top_layers,
            bottom_layers,
            density: job.f("sparse_infill_density") / 100.0,
            sparse_pattern: pattern::sparse_of(job, "sparse_infill_pattern"),
            solid_pattern: pattern::surface_of(job, "internal_solid_infill_pattern"),
            top_pattern: pattern::surface_of(job, "top_surface_pattern"),
            bottom_pattern: pattern::surface_of(job, "bottom_surface_pattern"),
            sparse_opts: infill::SparseOpts::from_job(job, nozzle, lw),
            infill_direction: job.f("infill_direction").to_radians(),
            bridge_direction: job.f("bridge_angle").to_radians(),
            bridge_flow: job.f("bridge_flow"),
            internal_bridge_flow: if job.cfg.has("internal_bridge_flow") { job.f("internal_bridge_flow") } else { 1.0 },
            thick_bridges: job.b("thick_bridges"),
            infill_overlap: job.f("infill_wall_overlap") / 100.0,
            travel_into_wall: job.f("monotonic_travel_into_wall") / 100.0,
            floating_shells: job.b("detect_floating_vertical_shell"),
            vertical_shells: job.s("ensure_vertical_shell_thickness") != "disabled",
            narrow_solid: job.b("detect_narrow_internal_solid_infill"),
            vertical_shell_speed: job.cfg.abs_or_pct("vertical_shell_speed", e, sp("internal_solid_infill_speed")),
            seam_gap: job.cfg.abs_or_pct("seam_gap", 0, nozzle),
            seam_mode: match job.s("seam_position") {
                "back" => SeamMode::Back,
                "random" => SeamMode::Random,
                "nearest" => SeamMode::Nearest,
                _ => SeamMode::Aligned,
            },
            elephant_foot: job.f("elefant_foot_compensation"),
            min_sparse_area: job.f("minimum_sparse_infill_area"),
            top_one_wall: job.s("top_one_wall_type") != "not apply",
            one_wall_first: job.b("only_one_wall_first_layer"),
            raft_layers,
            raft_heights,
            raft_base_layers: raft_layers / 2,
            raft_gap,
            raft_lift,
            raft_expansion,
            raft_first_expansion,
            raft_first_density,
            wall_order: match job.s("wall_sequence") {
                "outer wall/inner wall" => walls::WallOrder::OuterInner,
                "inner-outer-inner wall" => walls::WallOrder::InnerOuterInner,
                _ => walls::WallOrder::InnerOuter,
            },
            precise_outer: job.b("precise_outer_wall") && job.s("wall_sequence") == "inner wall/outer wall",
            arachne: arachne::Settings::from_job(job),
            gap_fill: {
                let mut s = arachne::Settings::of_job(job);
                s.min_feature_size = s.min_feature_size.max(job.f("filter_out_gap_fill"));
                s
            },
            overhang: walls::OverhangSpeeds {
                enabled: job.cfg.raw("detect_overhang_wall", 0) != "0",
                slowdown: if overhang_override { job.eff_bool("enable_overhang_speed", filament) } else { job.cfg.bool_at("enable_overhang_speed", e) },
                quarter: [overhang_speed("overhang_1_4_speed"), overhang_speed("overhang_2_4_speed"), overhang_speed("overhang_3_4_speed"), overhang_speed("overhang_4_4_speed")],
                totally: overhang_speed("overhang_totally_speed"),
                bridge: overhang_speed("bridge_speed"),
                nozzle,
                width: line_width(job, "inner_wall_line_width", nozzle, lw),
                smooth: if job.b("smooth_speed_discontinuity_area") { job.f("smooth_coefficient").max(0.0) } else { 0.0 },
                max_volumetric: job.fil("filament_max_volumetric_speed", filament),
            },
            support: if (coral && job.b("enable_support")) || plans_trees(job) {
                Some(support::SupportParams {
                    threshold_deg: { let a = job.f("support_threshold_angle"); if a > 0.0 { a } else { 30.0 } },
                    xy_gap: job.f("support_object_xy_distance"),
                    top_gap: job.f("support_top_z_distance"),
                    bottom_gap: { let b = job.f("support_bottom_z_distance"); if b > 0.0 { b } else { job.f("support_top_z_distance") } },
                    interface_top_layers: job.cfg.i64_at("support_interface_top_layers", 0).max(0) as usize,
                    line_width: support_width,
                    wall_width: line_width(job, "outer_wall_line_width", nozzle, lw),
                    tip_diameter: 0.8,
                    branch_diameter: { let d = job.f(TREE_BRANCH_DIAMETER); if d > 0.0 { d } else { 2.0 } },
                    taper_deg: job.f("tree_support_branch_diameter_angle"),
                    angle_max_deg: { let a = job.f(TREE_BRANCH_ANGLE); if a > 0.0 { a } else { 40.0 } },
                    angle_slow_deg: 25.0,
                    build_plate_only: job.b("support_on_build_plate_only"),
                    remove_small: job.b("support_remove_small_overhang"),
                    // A negative setting means the default, 2 mm.
                    first_expansion: if raft_layers > 0 { 0.0 } else { let e = job.f("raft_first_layer_expansion"); if e < 0.0 { 2.0 } else { e } },
                    stand_inside: job.b("fab_support_inside"),
                })
            } else {
                None
            },
            coral,
            normal_support: if job.b("enable_support") && job.s("support_type").starts_with("normal") && job.s("support_type").contains("auto") {
                Some(normal_support::NormalParams::from_job(job, filament, support_width))
            } else { None },
            support_width,
            support_speed: sp("support_speed"),
            support_interface_speed: sp("support_interface_speed"),
            support_interface_spacing: job.f("support_interface_spacing"),
            support_base_spacing: job.f("support_base_pattern_spacing"),
            support_first_density: { let d = job.f("raft_first_layer_density"); if d > 0.0 { d / 100.0 } else { 0.9 } },
            anchor: if job.cfg.is_percent("sparse_infill_anchor", 0) { Anchor::OfSpacing(job.f("sparse_infill_anchor") / 100.0) } else { Anchor::Mm(job.f("sparse_infill_anchor")) },
            anchor_max: if job.cfg.is_percent("sparse_infill_anchor_max", 0) { Anchor::OfSpacing(job.f("sparse_infill_anchor_max") / 100.0) } else { Anchor::Mm(job.f("sparse_infill_anchor_max")) },
            speeds: Speeds {
                outer: sp("outer_wall_speed"),
                inner: sp("inner_wall_speed"),
                sparse: sp("sparse_infill_speed"),
                solid: sp("internal_solid_infill_speed"),
                top: sp("top_surface_speed"),
                bridge: sp("bridge_speed"),
                gap: sp("gap_infill_speed"),
                first_layer: sp("initial_layer_speed"),
                first_layer_infill: sp("initial_layer_infill_speed"),
                max_volumetric: job.fil("filament_max_volumetric_speed", filament),
                internal_bridge: job.cfg.has("internal_bridge_speed").then(|| job.cfg.abs_or_pct("internal_bridge_speed", e, sp("bridge_speed"))),
            },
            exp: exp::Modes::from_job(job),
            paint_skin: if job.cfg.has("fab_paint_skin_depth") { job.f("fab_paint_skin_depth").max(0.0) } else { PAINT_SKIN },
            paint_inside_cell: (!job.b("fab_paint_inside_exact")).then_some(PAINT_INSIDE_CELL),
            paint_first_layer: job.b("fab_paint_first_layer"),
            solid_on_grid: job.b("fab_solid_on_grid"),
        };
        // A spiral vase is one (or, double-walled, two) walls with a solid
        // bottom: no tops, no infill.
        // Adaptive layers stay within the nozzle's printable range.
        if let Some(a) = p.exp.adaptive.as_mut() {
            let lo = job.ext("min_layer_height", e);
            let hi = job.ext("max_layer_height", e);
            a.max = if hi > 0.0 { hi.min(0.75 * nozzle) } else { 0.7 * nozzle };
            a.min = if lo > 0.0 { lo.max(0.04) } else { 0.2 * nozzle }.min(a.max);
        }
        if let Some(v) = &p.exp.vase {
            p.wall_loops = if v.variant == exp::VaseVariant::DoubleWall { 2 } else { 1 };
            p.arachne = None;
            p.top_layers = 0;
            p.density = 0.0;
        }
        p
    }

    /// These parameters as a part's slice reads them: without what only the supports and the
    /// raft under them read ([`is_support_key`]). The raft's heights and its gap to the part
    /// stay: they lift every layer of the part.
    pub fn without_supports(&self) -> Params {
        Params {
            support: None,
            coral: false,
            normal_support: None,
            support_width: 0.0,
            support_speed: 0.0,
            support_interface_speed: 0.0,
            support_interface_spacing: 0.0,
            support_base_spacing: 0.0,
            support_first_density: 0.0,
            raft_expansion: 0.0,
            ..self.clone()
        }
    }

    pub fn first(&self, index: usize) -> bool { index == 0 && self.raft_layers == 0 }

    /// Speed of a role, capped by the filament's volumetric limit.
    pub fn speed(&self, role: Role, area: f64, first_layer: bool) -> f64 {
        let s = &self.speeds;
        let mut v = if first_layer {
            if matches!(role, Role::BottomSurface | Role::InternalSolidInfill | Role::SparseInfill) {
                s.first_layer_infill
            } else {
                s.first_layer
            }
        } else {
            match role {
                Role::OuterWall => s.outer,
                Role::InnerWall => s.inner,
                Role::SparseInfill => s.sparse,
                Role::InternalSolidInfill | Role::BottomSurface => s.solid,
                Role::FloatingVerticalShell => s.bridge,
                Role::TopSurface => s.top,
                Role::InternalBridge => s.internal_bridge.unwrap_or(s.bridge),
                Role::Bridge | Role::OverhangWall => s.bridge,
                Role::GapFill => s.gap,
                _ => s.solid,
            }
        };
        if s.max_volumetric > 0.0 && area > 0.0 {
            v = v.min(s.max_volumetric / area);
        }
        v
    }
}

/// A layer's cut plane in the source frame.
pub fn cut_z(p: &Params, z: f64, h: f64) -> f64 { z - h / 2.0 - p.raft_lift }

/// A rest of a layer stack thinner than this (mm) is slack, not material: rounding, or how far a
/// box that holds its solid may reach past it (a field part's proven box, up to 0.01 mm).
pub const SLACK: f64 = 0.01;

/// Print heights (mm) of the layers of an object `height` tall: (top z, height), `first`, then
/// whole layers of `h`. A layer is cut at its middle, so the stack keeps each layer whose middle
/// is below the top: it ends on the layer boundary nearest the object's height, at most half a
/// layer over or under it, and every layer is as thick as planned.
pub fn layer_heights(height: f64, first: f64, h: f64) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    if height <= 0.0 {
        return out;
    }
    let mut z = first;
    out.push((z, z));
    while h > 0.0 && z + 0.5 * h < height - 1e-9 {
        let nz = z + h;
        out.push((nz, nz - z));
        z = nz;
    }
    out
}

/// The source's regions at each height, cut in parallel layer chunks.
pub fn contours(ctl: par::Ctl, src: &(dyn ContourSource + Send + Sync), zs: &[f64]) -> Result<Vec<Vec<ExPolygon>>, par::Cancelled> {
    contours_some(ctl, src, zs).into_iter().map(|v| v.ok_or(par::Cancelled)).collect()
}

/// [`contours`] that keeps what ran: after a cancel, the layers already cut hold their
/// regions and the others None.
pub fn contours_some(ctl: par::Ctl, src: &(dyn ContourSource + Send + Sync), zs: &[f64]) -> Vec<Option<Vec<ExPolygon>>> {
    if zs.is_empty() {
        return Vec::new();
    }
    // Chunks of layers (a source shares work along a chunk), small enough
    // that every thread stays busy and progress moves per chunk.
    let t = par::threads();
    let size = if t <= 1 { (zs.len() / 32).max(1) } else { (zs.len() / (t * 4)).clamp(1, 4) };
    let chunks: Vec<&[f64]> = zs.chunks(size).collect();
    // Chunks nearest the focus layer first; progress counts layers.
    let tick = ctl.tick;
    let ctl = par::Ctl { focus: ctl.focus.map(|f| f / size), tick: None, ..ctl };
    par::try_map_some(ctl, chunks.len(), |c| {
        let v = src.contours(chunks[c]);
        if let Some(t) = tick {
            t(chunks[c].len());
        }
        v
    })
    .into_iter()
    .zip(&chunks)
    .flat_map(|(v, c)| match v {
        Some(v) => v.into_iter().map(Some).collect::<Vec<_>>(),
        None => vec![None; c.len()],
    })
    .collect()
}

/// Slice and plan every object; layers of all objects merge by print z.
pub fn generate(objects: &[ObjectInput], job: &Job) -> Toolpaths {
    generate_ctl(par::Ctl::default(), objects, job).expect("not cancellable")
}

/// [`generate`] under a cancel check and focus layer.
pub fn generate_ctl(ctl: par::Ctl, objects: &[ObjectInput], job: &Job) -> Result<Toolpaths, par::Cancelled> {
    // Copies of one mesh on one filament are planned once and placed.
    let classes = instances(objects);
    let mut plans: Vec<Option<Arc<ObjectPlan>>> = vec![None; objects.len()];
    for (oi, o) in objects.iter().enumerate() {
        if classes[oi].0 == oi {
            plans[oi] = Some(Arc::new(plan_one(ctl, oi, o, job)?));
        }
    }
    let placed = (0..objects.len()).map(|oi| (plans[classes[oi].0].clone().expect("a representative is planned"), classes[oi].1)).collect();
    let names: Vec<String> = objects.iter().map(|o| o.name.clone()).collect();
    Ok(merge_named(placed, &names))
}

/// Per object: its representative and the representative-to-copy offset
/// (`instance::classes` over the objects' meshes, on equal filaments and print settings).
pub fn instances(objects: &[ObjectInput]) -> Vec<(usize, Pt)> {
    let with_mesh: Vec<usize> = (0..objects.len()).filter(|&i| objects[i].source.mesh().is_some()).collect();
    let meshes: Vec<&makepad_fab_slicer::mesh::Mesh> = with_mesh.iter().map(|&i| objects[i].source.mesh().unwrap()).collect();
    let mut out: Vec<(usize, Pt)> = (0..objects.len()).map(|i| (i, Pt::default())).collect();
    for (k, (r, d)) in makepad_fab_slicer::instance::classes(&meshes).into_iter().enumerate() {
        let (i, rep) = (with_mesh[k], with_mesh[r]);
        if rep != i && objects[rep].filament == objects[i].filament && objects[rep].overrides == objects[i].overrides {
            out[i] = (rep, d);
        }
    }
    out
}

/// One object's sliced and planned layers (print z, height, layer), in the
/// frame its source cuts in. Independent of every other object.
#[derive(Clone, Debug)]
pub struct ObjectPlan {
    pub info: ObjectInfo,
    /// The XY box its patterns are centred on ([`ContourSource::pivot_box`]).
    pub pivot: ([f64; 2], [f64; 2]),
    pub layers: Vec<(f64, f64, ObjectLayer)>,
    /// First-layer outline after XY compensation and non-planar clipping,
    /// grown by the brim gap before paint and elephant-foot compensation.
    /// Placed copies use it to clip brims against the rest of the plate.
    /// Empty when brim generation is disabled.
    pub brim_keep_out: Vec<ExPolygon>,
    /// Its non-planar top blocks (none without a plan).
    pub np_blocks: Vec<NpBlock>,
    /// Its supports as solid geometry, from the same run as their paths, and what they tell.
    pub skeleton: Supports,
}

impl ObjectPlan {
    /// What it holds that `seen` (the addresses of what was weighed) does not (bytes), and `seen`
    /// gains it: what plans share counts once.
    pub fn weigh(&self, seen: &mut std::collections::HashSet<usize>) -> usize {
        let layers: usize = self.layers.iter().map(|(_, _, l)| l.weigh(seen)).sum();
        layers + self.skeleton.weigh(seen)
    }
}

/// What `regions` hold (bytes).
pub fn regions_bytes(regions: &[ExPolygon]) -> usize {
    regions.iter().map(|e| size_of::<ExPolygon>() + e.rings().map(|r| size_of::<Polygon>() + r.pts.len() * size_of::<Pt>()).sum::<usize>()).sum()
}

/// A plan's supports as solid geometry ([`skeleton::Skeleton`]), and what they tell: the rules
/// they break, how much of the overhang they hold, what they leave bare. A plan has them told at
/// once, but for trees [`plan_support_paths`] made, whose paths go out first: their report
/// ([`TreeReport`]) is told once [`Supports::tell`] has measured it. The plan's copies share it.
#[derive(Clone, Default)]
pub struct Supports(Arc<SupportsCell>);

#[derive(Default)]
struct SupportsCell {
    now: std::sync::Mutex<Told>,
    /// Held while one measures the report: another waits for it, then finds it told.
    telling: std::sync::Mutex<()>,
}

enum Told {
    Told(Arc<skeleton::Skeleton>),
    /// The geometry, and what tells it.
    Later(Arc<skeleton::Skeleton>, Arc<TreeReport>),
}

impl Default for Told {
    fn default() -> Self {
        Told::Told(Arc::default())
    }
}

impl Supports {
    /// What they hold that `seen` does not (bytes), and `seen` gains it: the geometry, and what
    /// tells it until that is in.
    fn weigh(&self, seen: &mut std::collections::HashSet<usize>) -> usize {
        let now = self.0.now.lock().unwrap();
        let (Told::Told(shape) | Told::Later(shape, _)) = &*now;
        let geometry = if seen.insert(Arc::as_ptr(shape) as usize) { shape.bytes() } else { 0 };
        geometry + match &*now {
            Told::Later(_, report) if seen.insert(Arc::as_ptr(report) as usize) => report.bytes(),
            _ => 0,
        }
    }

    /// `skeleton`, told once `report` has told it (at once without one).
    pub fn of(skeleton: skeleton::Skeleton, report: Option<Arc<TreeReport>>) -> Supports {
        let shape = Arc::new(skeleton);
        let now = match report {
            Some(report) => Told::Later(shape, report),
            None => Told::Told(shape),
        };
        Supports(Arc::new(SupportsCell { now: std::sync::Mutex::new(now), telling: std::sync::Mutex::new(()) }))
    }

    /// Their solid geometry, whether or not what they tell is in.
    pub fn shape(&self) -> Arc<skeleton::Skeleton> {
        match &*self.0.now.lock().unwrap() {
            Told::Told(s) | Told::Later(s, _) => s.clone(),
        }
    }

    /// With what they tell, once it is in.
    pub fn told(&self) -> Option<Arc<skeleton::Skeleton>> {
        match &*self.0.now.lock().unwrap() {
            Told::Told(s) => Some(s.clone()),
            Told::Later(..) => None,
        }
    }

    /// With what they tell, measured now when it is not in: one measure at a time, and one
    /// cancelled leaves it to the next.
    pub fn tell(&self, ctl: par::Ctl) -> Result<Arc<skeleton::Skeleton>, par::Cancelled> {
        // A measure that panicked left it to the next, as a cancelled one does.
        let _one = self.0.telling.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (shape, report) = match &*self.0.now.lock().unwrap() {
            Told::Told(s) => return Ok(s.clone()),
            Told::Later(s, r) => (s.clone(), r.clone()),
        };
        let told = Arc::new(report.tell(ctl, &shape)?);
        *self.0.now.lock().unwrap() = Told::Told(told.clone());
        Ok(told)
    }
}

impl std::fmt::Debug for Supports {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match &*self.0.now.lock().unwrap() {
            Told::Told(s) => f.debug_tuple("Told").field(s).finish(),
            Told::Later(s, _) => f.debug_tuple("Later").field(s).finish(),
        }
    }
}

/// Slice and plan object `oi`.
pub fn plan_one(ctl: par::Ctl, oi: usize, o: &ObjectInput, job: &Job) -> Result<ObjectPlan, par::Cancelled> {
    Ok(stages::plan(ctl, oi, o, job, &plan_heights(o, job), None)?.0)
}

/// Object `o`'s layers (print z, height) under its own settings: adaptive layers read the
/// part's slope profile, a trace of the whole part, so whoever plans one shape in windows reads
/// them once per shape and hands them to every [`plan_window`].
pub fn plan_heights(o: &ObjectInput, job: &Job) -> Vec<(f64, f64)> {
    let own;
    let job = if o.overrides.is_empty() { job } else { own = job.with_overrides(&o.overrides); &own };
    layers::object_heights(&*o.source, &Params::from_job(job, o.filament))
}

/// The seam chain reaches four layers below the layer being planned.
const SEAM_WINDOW: usize = 4;
/// Cross-layer infill shares one aux per block, with context on either side.
const AUX_BLOCK: usize = 16;
const AUX_REACH: usize = 8;

/// Model layers `lo..hi` of `heights` (the part's [`plan_heights`]), exactly as in
/// [`plan_one`] without supports, both returned and emitted with their absolute layer
/// indices. Painted parts return every filament region at those heights. A vase's ramp is
/// cumulative: it is planned whole and this function returns and emits all its layers.
pub fn plan_window(ctl: par::Ctl, oi: usize, o: &ObjectInput, job: &Job, heights: &[(f64, f64)], lo: usize, hi: usize) -> Result<Vec<(f64, f64, ObjectLayer)>, par::Cancelled> {
    Ok(stages::plan(ctl, oi, o, job, heights, Some((lo, hi)))?.1)
}

/// Contours read by a model window. Painted sources additionally read the
/// whole part to discover their region streams; a vase always plans whole.
pub fn plan_reads(p: &Params, lo: usize, hi: usize, n: usize) -> (usize, usize) {
    if p.exp.vase.is_some() { return (0, n); }
    let hi = hi.min(n);
    if lo >= hi { return (hi, hi); }
    plan_context(p, lo, hi, n)
}

/// The outline after XY compensation and non-planar clipping, used by model
/// planning, supports and the finished plan. `job` is before object overrides.
pub fn plan_outline(ctl: par::Ctl, o: &ObjectInput, job: &Job, heights: &[(f64, f64)], slices: Vec<Vec<ExPolygon>>) -> Result<Vec<Vec<ExPolygon>>, par::Cancelled> {
    let own;
    let job = if o.overrides.is_empty() { job } else { own = job.with_overrides(&o.overrides); &own };
    let xy = XyComp::of(job, o);
    let np = o.source.np_plan();
    let lift = if job.cfg.i64_at("raft_layers", 0) > 0 { Params::from_job(job, o.filament).raft_lift } else { 0.0 };
    let slices: Vec<std::sync::Mutex<Vec<ExPolygon>>> = slices.into_iter().map(std::sync::Mutex::new).collect();
    par::try_map(ctl, slices.len(), |l| stages::outline_layer(xy, np.as_deref(), lift, heights[l].0, split_pinches(std::mem::take(&mut *slices[l].lock().unwrap()))))
}

/// XY compensation as one material offset plus a hole-only rest.
///
/// `shift` (`xy_contour_compensation`) moves every boundary outward, holes
/// included, so a field source can take it as an iso shift; holes then grow
/// by `hole_extra` more (`xy_hole_compensation` + `shift`). Zero for painted
/// parts printed with several filaments.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct XyComp { pub shift: f64, pub hole_extra: f64 }

impl XyComp {
    pub fn of(job: &Job, o: &ObjectInput) -> Self {
        if job.filaments() > 1 && (o.source.painted() || o.source.paint_at().is_some()) { return Self::default(); }
        let shift = job.f("xy_contour_compensation");
        Self { shift, hole_extra: job.f("xy_hole_compensation") + shift }
    }

    /// Polygons: grow, then shrink, per island (contours by `shift`, holes
    /// by `hole_extra - shift`), so each island's holes stay local to each pass.
    pub fn apply(&self, layer: &[ExPolygon]) -> Vec<ExPolygon> {
        let (contour, hole) = (self.shift, self.hole_extra - self.shift);
        let mut out = layer.to_vec();
        for (contour, hole) in [(contour.max(0.0), hole.max(0.0)), (contour.min(0.0), hole.min(0.0))] {
            if contour == 0.0 && hole == 0.0 { continue; }
            let mut next = Vec::new();
            for e in &out {
                let outer = offset_polygons(&[e.contour.clone()], contour * SCALE, DEFAULT_MITER);
                if e.holes.is_empty() {
                    next.extend(outer);
                } else {
                    let mut holes = e.holes.clone();
                    for h in &mut holes { h.reverse(); }
                    let cut = offset_polygons(&holes, hole * SCALE, DEFAULT_MITER);
                    next.extend(difference_ex(&outer, &cut));
                }
            }
            out = if out.len() > 1 { union_ex(&next) } else { next };
        }
        out
    }
}

/// Assemble the completed model regions and support additions. The outline
/// is the whole part after XY compensation and non-planar clipping, before
/// paint and elephant-foot compensation.
pub fn finish_plan(o: &ObjectInput, job: &Job, heights: &[(f64, f64)], outline: &[Vec<ExPolygon>], mut layers: Vec<(f64, f64, ObjectLayer)>, supports: PlannedSupports) -> ObjectPlan {
    let own;
    let job = if o.overrides.is_empty() { job } else { own = job.with_overrides(&o.overrides); &own };
    let params = Params::from_job(job, o.filament);
    let info = ObjectInfo::of(o, params.raft_lift);
    let painted = o.source.painted();
    if painted {
        layers.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    }
    let raft = if params.raft_layers > 0 { raft::layers(&params, o.filament, outline.first().map(Vec::as_slice).unwrap_or(&[]), &layers, &supports.additions) } else { Vec::new() };
    merge_supports(&mut layers, supports.additions, o.filament, painted);
    if !raft.is_empty() {
        layers.extend(raft);
        layers.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| (a.2.filament != o.filament).cmp(&(b.2.filament != o.filament))));
    }
    let brim_keep_out = if params.brim_kind == BrimKind::NoBrim { Vec::new() } else { brim::keep_out(outline.first().map(Vec::as_slice).unwrap_or(&[]), params.brim_gap) };
    let mut np_blocks = if heights.is_empty() { Vec::new() } else { o.source.np_plan().map(|np| np_blocks(&np, heights, &params)).unwrap_or_default() };
    if params.raft_layers > 0 {
        for b in &mut np_blocks {
            if let Some(i) = layers.iter().position(|l| !l.2.support_only && (l.0 - b.z).abs() < 1e-6) { b.layer = i; }
        }
    }
    ObjectPlan { info, pivot: o.source.pivot_box(), layers, brim_keep_out, np_blocks, skeleton: Supports::of(supports.skeleton, supports.report) }
}

/// Contour context for the requested layers, including the classes read by
/// each aux block. Shell propagation reads an outline one layer beyond its
/// skin; internal bridges and their floating shells read the adjacent class.
fn plan_context(p: &Params, lo: usize, hi: usize, n: usize) -> (usize, usize) {
    let (a, b) = if p.sparse_pattern.cross_layer() {
        ((lo / AUX_BLOCK * AUX_BLOCK).saturating_sub(AUX_REACH), (((hi - 1) / AUX_BLOCK + 1) * AUX_BLOCK + AUX_REACH).min(n))
    } else { (lo, hi) };
    let reach = p.top_layers.max(p.bottom_layers) + 2;
    (a.saturating_sub(reach).min(lo.saturating_sub(SEAM_WINDOW)) & !1, b.saturating_add(reach).min(n))
}

/// Paths on adaptive layers that no other mode marked show as adaptive.
fn mark_adaptive(params: &Params, ol: &mut ObjectLayer) {
    if params.exp.adaptive.is_none() {
        return;
    }
    for isl in &mut ol.islands {
        let isl = Arc::make_mut(isl);
        for p in isl.walls.iter_mut().chain(isl.fills.iter_mut()) {
            if p.mode == exp::mode::NONE {
                p.mode = exp::mode::ADAPTIVE;
            }
        }
    }
}

impl ObjectLayer {
    /// What it holds that `seen` (the addresses of what was weighed) does not (bytes), and `seen`
    /// gains it: what layers share counts once.
    fn weigh(&self, seen: &mut std::collections::HashSet<usize>) -> usize {
        let path = |p: &ExtrusionPath| size_of::<ExtrusionPath>() + (p.pts.len() + p.loop_pts.len()) * size_of::<Pt>() + (p.z.len() + p.flow.len()) * size_of::<f64>() + p.dry.len();
        let islands: usize = self.islands.iter().filter(|i| seen.insert(Arc::as_ptr(i) as usize)).map(|i| size_of::<Island>() + i.walls.iter().chain(&i.fills).map(path).sum::<usize>()).sum();
        islands + if seen.insert(Arc::as_ptr(&self.slices) as usize) { regions_bytes(&self.slices) } else { 0 }
    }

    /// Every region and path moved by `d` (exact: integer units).
    pub fn translate(&mut self, d: Pt) {
        for e in Arc::make_mut(&mut self.slices) {
            e.translate(d);
        }
        for island in &mut self.islands {
            let island = Arc::make_mut(island);
            for p in island.walls.iter_mut().chain(island.fills.iter_mut()) {
                for q in p.pts.iter_mut().chain(p.loop_pts.iter_mut()) {
                    *q = q.add(d);
                }
            }
        }
    }
}

/// The plate's toolpaths: plan `i` is object `i`, placed at its offset
/// (integer units, exact); layers of all objects merge by print z.
pub fn merge(plans: Vec<(Arc<ObjectPlan>, Pt)>) -> Toolpaths {
    let names: Vec<String> = plans.iter().map(|p| p.0.info.name.clone()).collect();
    merge_named(plans, &names)
}

/// [`merge`], object `i` named `names[i]` (a placed copy keeps its own name).
pub fn merge_named(plans: Vec<(Arc<ObjectPlan>, Pt)>, names: &[String]) -> Toolpaths {
    let _t = prof::scope(prof::Stage::Merge);
    // Brims are planned in each representative's frame. Only placement
    // knows where the other objects (including copies) obstruct them.
    let mut brim_keep_out = Vec::new();
    for (plan, d) in &plans {
        for e in &plan.brim_keep_out {
            let mut e = e.clone();
            e.translate(*d);
            brim_keep_out.push(e);
        }
    }
    let brim_keep_out = if brim_keep_out.is_empty() { brim_keep_out } else { union_ex(&brim_keep_out) };
    let mut infos = Vec::with_capacity(plans.len());
    let mut zs: Vec<f64> = plans.iter().flat_map(|(p, _)| p.layers.iter().map(|x| x.0)).collect();
    zs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    // With materials to change, heights within [`SLACK`] are one layer, at the highest: two parts
    // a rounding apart in height must not leave a step no nozzle prints with a change on it,
    // which the prime tower would then size for. One material has nothing to change there.
    let mut materials = plans.iter().flat_map(|(p, _)| p.layers.iter().map(|x| x.2.filament));
    let first = materials.next();
    let near = if materials.all(|f| Some(f) == first) { 1e-6 } else { SLACK };
    let mut bottom = f64::MIN;
    let mut tops: Vec<f64> = Vec::with_capacity(zs.len());
    for z in zs {
        match tops.last_mut() {
            Some(top) if z - *top < 1e-6 => {}
            Some(top) if z - bottom < near => *top = z,
            _ => {
                bottom = z;
                tops.push(z);
            }
        }
    }
    let mut layers: Vec<Layer> = tops.iter().enumerate().map(|(i, z)| Layer { index: i, print_z: *z, height: 0.0, objects: Vec::new() }).collect();
    let layer_at = |z: f64| {
        let li = tops.partition_point(|top| *top < z - 1e-6);
        (li < tops.len() && tops[li] - z < near).then_some(li)
    };
    let mut np_blocks = Vec::new();
    for (oi, (plan, d)) in plans.into_iter().enumerate() {
        // Each layer placed on the plate, in parallel: a plan the caller still shares (the live
        // engine's part cache keeps it for the next edit) is copied, one it hands over moves.
        let (mut info, blocks, placed) = match Arc::try_unwrap(plan) {
            Ok(p) => (p.info, p.np_blocks, par::map_vec(p.layers, |_, (z, h, l)| (z, h, placed_layer(l, oi, d, &brim_keep_out)))),
            Err(p) => (p.info.clone(), p.np_blocks.clone(), par::map(p.layers.len(), |k| {
                let (z, h, l) = &p.layers[k];
                (*z, *h, placed_layer(l.clone(), oi, d, &brim_keep_out))
            })),
        };
        for mut b in blocks {
            let Some(li) = layer_at(b.z) else { continue };
            b.object = oi;
            b.layer = li;
            if d != Pt::default() {
                b.translate(d);
            }
            np_blocks.push(b);
        }
        info.name = names[oi].clone();
        if d != Pt::default() {
            let (dx, dy) = (unscaled(d.x), unscaled(d.y));
            info.min = [info.min[0] + dx, info.min[1] + dy, info.min[2]];
            info.max = [info.max[0] + dx, info.max[1] + dy, info.max[2]];
            for e in &mut info.embeds {
                e.min = [e.min[0] + dx, e.min[1] + dy, e.min[2]];
                e.max = [e.max[0] + dx, e.max[1] + dy, e.max[2]];
            }
        }
        // An item's pause comes before the object's first layer whose plane is at or above its
        // cover: the first its cut leaves whole.
        for e in &mut info.embeds {
            let over = placed.iter().filter(|(z, h, l)| !l.support_only && z - 0.5 * h >= e.cover - makepad_fab_slicer::embed::ON).map(|(z, ..)| *z).min_by(f64::total_cmp);
            e.layer = over.and_then(layer_at);
        }
        infos.push(info);
        for (z, h, l) in placed {
            if let Some(li) = layer_at(z) {
                let layer = &mut layers[li];
                layer.height = layer.height.max(h);
                layer.objects.push(l);
            }
        }
    }
    np_blocks.sort_by_key(|b| (b.layer, b.object));
    Toolpaths { layers, objects: infos, np_blocks }
}

/// Layer `l` of object `oi` placed at `d` on the plate, its brim clipped by the other objects'
/// keep-out ([`merge_named`]).
fn placed_layer(mut l: ObjectLayer, oi: usize, d: Pt, brim_keep_out: &[ExPolygon]) -> ObjectLayer {
    l.object = oi;
    if d != Pt::default() {
        l.translate(d);
    }
    if !brim_keep_out.is_empty() {
        l.islands.retain_mut(|island| {
            if island.walls.first().is_some_and(|p| p.role == Role::Brim) {
                let island = Arc::make_mut(island);
                island.walls = brim::clip_paths(std::mem::take(&mut island.walls), brim_keep_out);
                !island.walls.is_empty() || !island.fills.is_empty()
            } else {
                true
            }
        });
    }
    l
}

/// How many walls an island gets: at most one on the first layer when so
/// set, and one where it is all top surface (`above`: the outline of the
/// layer above, None on the top layer).
pub fn wall_loops(p: &Params, island: &ExPolygon, above: Option<&[ExPolygon]>, first: bool) -> usize {
    p.wall_loops.min(loops_cap(p, island, above, first))
}

/// The most walls an island takes, whatever its count ([`wall_loops`]): one on the first layer
/// when so set, and where it is all top surface; else no limit.
pub fn loops_cap(p: &Params, island: &ExPolygon, above: Option<&[ExPolygon]>, first: bool) -> usize {
    if first && p.one_wall_first {
        return 1;
    }
    if p.top_one_wall && !first {
        let all_top = match above {
            Some(above) => mostly_uncovered(island, above),
            None => true,
        };
        if all_top {
            return 1;
        }
    }
    usize::MAX
}

/// Whether `above` leaves more than 99% of `island`'s area uncovered: the answer of
/// `total_area_mm2(&difference_ex(&[island], above)) > 0.99 * island.area_mm2()`.
///
/// A region of `above` whose edges keep clear of the island's ([`walls::Apart`]), that does not
/// lie inside the island and does not hold it, misses the island. The boolean without those
/// regions has the same true area as the full one. Each boolean's snap rounding moves every edge
/// by under 0.71 units, so its area lies within twice the edges' length plus a little per vertex
/// of that true area. An answer farther than both margins from the threshold decides: first a
/// lower bound on the area the island shares with `above` ([`shares_at_least`]), then the
/// boolean without the regions that miss; one within the margins runs the full boolean.
fn mostly_uncovered(island: &ExPolygon, above: &[ExPolygon]) -> bool {
    let island_area = island.area_mm2();
    let threshold = 0.99 * island_area;
    let full = || total_area_mm2(&difference_ex(std::slice::from_ref(island), above)) > threshold;
    // clip's `near` rule: a region whose bounds keep two units from the island's takes no part
    let ib = island.bbox();
    let g = BBox { min: Pt::new(ib.min.x - 2, ib.min.y - 2), max: Pt::new(ib.max.x + 2, ib.max.y + 2) };
    let apart = walls::Apart::new(island);
    let q = island.contour.pts[0];
    let mut length: f64 = island.rings().map(Polygon::perimeter).sum();
    let mut points: usize = island.rings().map(|r| r.pts.len()).sum();
    let mut kept = Vec::new();
    for r in above {
        let mut b = r.bbox();
        for h in &r.holes {
            b.merge(&h.bbox());
        }
        if !g.overlaps(&b) {
            continue;
        }
        length += r.rings().map(Polygon::perimeter).sum::<f64>();
        points += r.rings().map(|r| r.pts.len()).sum::<usize>();
        let misses = apart.rings(r.rings()) && !island.contains(r.contour.pts[0]) && !r.contains(q);
        if !misses {
            kept.push(r.clone());
        }
    }
    // both booleans' rounding, in mm²
    let margin = 2.0 * (2.0 * length + 4.0 * points as f64) / (SCALE * SCALE);
    if !kept.is_empty() && shares_at_least(&apart, &kept, island_area - threshold + margin) {
        return false;
    }
    let area = if kept.is_empty() { island_area } else { total_area_mm2(&difference_ex(std::slice::from_ref(island), &kept)) };
    let decided = if area > threshold + margin {
        Some(true)
    } else if area < threshold - margin {
        Some(false)
    } else {
        None
    };
    decided.unwrap_or_else(full)
}

/// Whether the island (`apart`'s region) and `above` share more than `need` mm²: disjoint
/// disks, centred on the points of a lattice over the island's bounds that lie inside both,
/// each as wide as the distance to the nearest edge of either allows. A disk with no edge of
/// either set in it lies inside both, so their areas add up to at most the shared area. The
/// points come in bit-reversed order (spread over the bounds from the start) and the sum stops
/// once past `need`; false when the lattice runs out first.
fn shares_at_least(apart: &walls::Apart, above: &[ExPolygon], need: f64) -> bool {
    const SIDE: u32 = 64;
    let b = apart.bbox();
    let (w, h) = ((b.max.x - b.min.x) as f64, (b.max.y - b.min.y) as f64);
    let step = w.max(h) / SIDE as f64;
    // Disks a unit narrower than half the step stay apart (the centres round down to units).
    let cap = step / 2.0;
    if cap <= 2.0 {
        return false;
    }
    let (nx, ny) = (((w / step).ceil() as u32).clamp(1, SIDE), ((h / step).ceil() as u32).clamp(1, SIDE));
    let island = apart.grid();
    let other = makepad_fab_slicer::grid::SegGrid::of_regions(above, |k| k as u32);
    let mut shared = 0.0;
    for t in 0..SIDE * SIDE {
        let k = t.reverse_bits() >> (32 - 12);
        let (i, j) = (k % SIDE, k / SIDE);
        if i >= nx || j >= ny {
            continue;
        }
        let p = Pt::new(b.min.x + ((i as f64 + 0.5) * step) as i64, b.min.y + ((j as f64 + 0.5) * step) as i64);
        if !island.contains_any(p) || !other.contains_any(p) {
            continue;
        }
        let mut d = cap;
        island.nearest_with(p, |a, c, _| {
            d = d.min(dist_point_segment(p, a, c));
            d
        });
        other.nearest_with(p, |a, c, _| {
            d = d.min(dist_point_segment(p, a, c));
            d
        });
        let r = (d - 1.0) / SCALE;
        if r > 0.0 {
            shared += std::f64::consts::PI * r * r;
            if shared > need {
                return true;
            }
        }
    }
    false
}

/// One island's walls and the fill region inside them, made from its
/// outline: the planner's own, and what walls a source makes are held to.
/// `base` is the island, or on the first layer what elephant-foot
/// compensation leaves of it.
pub fn island_walls(p: &Params, base: &[ExPolygon], loops: usize, first: bool, h: f64) -> IslandWalls {
    let (w_ext, w_in) = if first { (p.w_first, p.w_first) } else { (p.w_outer, p.w_inner) };
    // A textured outer wall cuts inward: its walls start on the inset island.
    let textured: Vec<ExPolygon>;
    let pieces = match p.exp.texture.as_ref().filter(|_| !first) {
        Some(t) => {
            textured = base.iter().flat_map(|b| texture::inset_island(b, t)).collect();
            &textured[..]
        }
        None => base,
    };
    if let Some(a) = &p.arachne {
        let cfg = a.config(loops, w_ext, w_in, h, p.precise_outer);
        let (mut lines, mut fill) = (Vec::new(), Vec::new());
        for piece in pieces {
            let w = arachne::walls(piece, &cfg);
            lines.extend(w.lines);
            fill.extend(w.inner);
        }
        return IslandWalls { shape: WallShape::Beads(lines), fill, gaps: Vec::new(), thin: Vec::new(), pieces: Vec::new() };
    }
    let (mut rings, mut fill, mut thin, mut between): (Vec<Vec<Polygon>>, Vec<ExPolygon>, Vec<ExPolygon>, Vec<ExPolygon>) = Default::default();
    for piece in pieces {
        let r = walls::rings(piece, loops, w_ext, w_in, h, p.precise_outer);
        for (k, ring) in r.rings.into_iter().enumerate() {
            if rings.len() <= k {
                rings.push(Vec::new());
            }
            rings[k].extend(ring);
        }
        fill.extend(r.fill);
        thin.extend(r.thin);
        between.extend(r.between);
    }
    // One continuous spiral has no room for lines of its own.
    if p.exp.vase.is_some() || loops == 0 {
        return IslandWalls { shape: WallShape::Rings(rings), fill, gaps: Vec::new(), thin: Vec::new(), pieces: Vec::new() };
    }
    // What no fill line's centre can reach.
    let w_fill = if first { p.w_first } else { p.w_solid };
    let reach = fill_reach(p, h, first) * SCALE;
    if reach > 0.0 {
        between.extend(difference_ex(&fill, &opening(&fill, reach, reach, Join::Miter(3.0))));
    }
    // Where the island is too thin for the outer wall to go round, the wall is one bead down it,
    // as wide as the island, to where the island gets narrower than the narrowest printable bead:
    // the wall closes the island off there. The sliver past that point prints nothing. A line
    // into it would be thinner than the nozzle lays, or widened past the model. A line's widths
    // are spacings, each its bead's width less the rounding of its sides.
    let least = flow::spacing(p.gap_fill.min_bead_width, h);
    let mut memo = arachne::Memo::new(&gap_config(p, w_fill, h));
    let thin = gap_lines_memo(&thin, &mut memo).into_iter().flat_map(|l| cut_narrow(l, least)).collect();
    IslandWalls { shape: WallShape::Rings(rings), fill, gaps: gap_lines_memo(&between, &mut memo), thin, pieces: Vec::new() }
}

/// [`island_walls`] of an island some of whose places take wall counts of their own (regions'
/// settings, `m.settings`): `cells`, each an area of the layer and its count (capped as the
/// island's: [`loops_cap`]), and `loops` everywhere else. The loops every place has run round
/// whole; a loop only some places have runs only there, cut where a region's edge crosses it,
/// and a piece of it shorter than a line is none. The fill is what lies inside the innermost
/// loop at each place, its gap fill and thin walls each place's own.
pub fn zoned_walls(p: &Params, island: &ExPolygon, loops: usize, cells: &[(&[ExPolygon], usize)], first: bool, h: f64) -> IslandWalls {
    let whole = std::slice::from_ref(island);
    // each count and where on the island it holds
    let mut counts: Vec<(usize, Vec<ExPolygon>)> = Vec::new();
    let mut taken: Vec<ExPolygon> = Vec::new();
    for (area, n) in cells {
        let at = intersection_ex(whole, area);
        if at.is_empty() || *n == loops {
            continue;
        }
        taken.extend(at.iter().cloned());
        match counts.iter_mut().find(|c| c.0 == *n) {
            Some(c) => c.1.extend(at),
            None => counts.push((*n, at)),
        }
    }
    if counts.is_empty() {
        return island_walls(p, whole, loops, first, h);
    }
    for c in &mut counts {
        if c.1.len() > 1 {
            c.1 = union_ex(&c.1);
        }
    }
    let rest = difference_ex(whole, &taken);
    if !rest.is_empty() {
        counts.push((loops, rest));
    }
    let made: Vec<(usize, IslandWalls, Vec<ExPolygon>)> = counts.into_iter().map(|(n, at)| (n, island_walls(p, whole, n, first, h), at)).collect();
    let least = made.iter().map(|m| m.0).min().expect("a count");
    let most = made.iter().max_by_key(|m| m.0).expect("a count");
    // where loop `r` runs: the places with more loops than `r`
    let runs = |r: usize| -> Vec<ExPolygon> {
        let at: Vec<ExPolygon> = made.iter().filter(|m| m.0 > r).flat_map(|m| m.2.iter().cloned()).collect();
        if made.iter().filter(|m| m.0 > r).count() > 1 { union_ex(&at) } else { at }
    };
    let (w_ext, w_in) = if first { (p.w_first, p.w_first) } else { (p.w_outer, p.w_inner) };
    let shortest = w_in * SCALE;
    let mut pieces = Vec::new();
    let shape = match &most.1.shape {
        WallShape::Rings(rings) => {
            let mut kept: Vec<Vec<Polygon>> = Vec::with_capacity(rings.len());
            for (r, ring) in rings.iter().enumerate() {
                if r < least {
                    kept.push(ring.clone());
                    continue;
                }
                let mut clipper = clipper::Clipper::new(&runs(r));
                let spacing = flow::spacing(if r == 0 { w_ext } else { w_in }, h);
                let mut whole_rings = Vec::new();
                for poly in ring {
                    let widths = vec![spacing; poly.pts.len()];
                    match cut_line(&poly.pts, &widths, true, &mut clipper, shortest) {
                        Cut::Whole => whole_rings.push(poly.clone()),
                        Cut::Pieces(cut) => pieces.extend(cut.into_iter().map(|(pts, widths)| BeadLine { inset: r, closed: false, odd: false, pts, widths })),
                    }
                }
                kept.push(whole_rings);
            }
            while kept.last().is_some_and(Vec::is_empty) {
                kept.pop();
            }
            WallShape::Rings(kept)
        }
        WallShape::Beads(lines) => {
            let mut clippers: Vec<Option<clipper::Clipper>> = Vec::new();
            let mut out = Vec::with_capacity(lines.len());
            for l in lines {
                if l.inset < least {
                    out.push(l.clone());
                    continue;
                }
                if clippers.len() <= l.inset {
                    clippers.resize_with(l.inset + 1, || None);
                }
                let clipper = clippers[l.inset].get_or_insert_with(|| clipper::Clipper::new(&runs(l.inset)));
                match cut_line(&l.pts, &l.widths, l.closed, clipper, shortest) {
                    Cut::Whole => out.push(l.clone()),
                    Cut::Pieces(cut) => out.extend(cut.into_iter().map(|(pts, widths)| BeadLine { closed: false, pts, widths, ..l.clone() })),
                }
            }
            WallShape::Beads(out)
        }
    };
    // each place's fill, gap fill and thin walls
    let mut fill = Vec::new();
    let (mut gaps, mut thin) = (Vec::new(), Vec::new());
    for (_, w, at) in &made {
        fill.extend(intersection_ex(&w.fill, at));
        let mut clipper = clipper::Clipper::new(at);
        for (from, to) in [(&w.gaps, &mut gaps), (&w.thin, &mut thin)] {
            for l in from {
                match cut_line(&l.pts, &l.widths, l.closed, &mut clipper, 0.0) {
                    Cut::Whole => to.push(l.clone()),
                    Cut::Pieces(cut) => to.extend(cut.into_iter().map(|(pts, widths)| BeadLine { closed: false, pts, widths, ..l.clone() })),
                }
            }
        }
    }
    IslandWalls { shape, fill: union_ex(&fill), gaps, thin, pieces }
}

/// A bead line cut to a region ([`cut_line`]).
enum Cut {
    /// All of it is inside.
    Whole,
    /// Its pieces inside (none: all outside), each with its widths.
    Pieces(Vec<(Vec<Pt>, Vec<f64>)>),
}

/// The pieces of a bead line (`pts`, its `widths` at each; `closed`: back to its start) inside
/// the clipper's region, each at least `shortest` long (scaled units), their widths taken along
/// the line where the region's edge cuts it.
fn cut_line(pts: &[Pt], widths: &[f64], closed: bool, clipper: &mut clipper::Clipper, shortest: f64) -> Cut {
    let line: Vec<Pt> = if closed { pts.iter().chain(pts.first()).copied().collect() } else { pts.to_vec() };
    let width: Vec<f64> = if closed { widths.iter().chain(widths.first()).copied().collect() } else { widths.to_vec() };
    let mut got = clipper::Pieces::default();
    clipper.clip(&line, &mut got);
    if got.len() == 1 && got.ea[0] == clipper::FREE && got.eb[0] == clipper::FREE {
        return Cut::Whole;
    }
    // each piece's points along the line, in order: the width where each lies
    let mut seg = 0;
    let mut out: Vec<(Vec<Pt>, Vec<f64>)> = Vec::with_capacity(got.len());
    for i in 0..got.len() {
        let piece = got.piece(i);
        let mut w = Vec::with_capacity(piece.len());
        for &q in piece {
            while seg + 1 < line.len() - 1 && dist_point_segment(q, line[seg], line[seg + 1]) > 2.0 {
                seg += 1;
            }
            let (a, b) = (line[seg], line[seg + 1]);
            let l = a.dist(b);
            let t = if l > 0.0 { (q.dist(a) / l).clamp(0.0, 1.0) } else { 0.0 };
            w.push(width[seg] + (width[seg + 1] - width[seg]) * t);
        }
        out.push((piece.to_vec(), w));
    }
    // a closed line that starts inside: its last piece runs on into its first
    if closed && out.len() > 1 && got.ea[0] == clipper::FREE && got.eb[got.len() - 1] == clipper::FREE {
        let (first_pts, first_w) = out.remove(0);
        let last = out.last_mut().expect("a last piece");
        last.0.extend_from_slice(&first_pts[1..]);
        last.1.extend_from_slice(&first_w[1..]);
    }
    out.retain(|(pts, _)| pts.windows(2).map(|s| s[0].dist(s[1])).sum::<f64>() >= shortest);
    Cut::Pieces(out)
}

/// The pieces of `line` at least `least` wide (spacing, mm), cut where its width crosses that;
/// a piece shorter than [`GAP_MIN_LENGTH`] is dropped.
fn cut_narrow(line: BeadLine, least: f64) -> Vec<BeadLine> {
    let n = line.pts.len();
    if line.widths.iter().all(|&w| w >= least) {
        return vec![line];
    }
    // A closed line starts at a narrow point, so every piece is open.
    let start = if line.closed { line.widths.iter().position(|&w| w < least).expect("a narrow point") } else { 0 };
    let run = if line.closed { n + 1 } else { n };
    let at = |k: usize| (line.pts[(start + k) % n], line.widths[(start + k) % n]);
    let mut out = Vec::new();
    let mut piece: Vec<(Pt, f64)> = Vec::new();
    let mut close = |piece: &mut Vec<(Pt, f64)>| {
        let length: f64 = piece.windows(2).map(|s| s[0].0.dist(s[1].0)).sum();
        if piece.len() >= 2 && length / SCALE >= GAP_MIN_LENGTH {
            out.push(BeadLine { inset: line.inset, closed: false, odd: line.odd, pts: piece.iter().map(|q| q.0).collect(), widths: piece.iter().map(|q| q.1).collect() });
        }
        piece.clear();
    };
    for k in 0..run {
        let (q, w) = at(k);
        if k > 0 {
            let (a, wa) = at(k - 1);
            if (wa >= least) != (w >= least) {
                // Where the width crosses `least` along the segment.
                let cut = (lerp(a, q, (least - wa) / (w - wa)), least);
                if wa >= least {
                    piece.push(cut);
                    close(&mut piece);
                } else {
                    piece.push(cut);
                }
            }
        }
        if w >= least {
            piece.push((q, w));
        }
    }
    close(&mut piece);
    out
}

/// How far a fill line's centre reaches towards the edge of its place (mm): the lines sit half a
/// spacing in, less their overlap into the walls. A place narrower than twice this is the walls'
/// gap fill ([`island_walls`]).
fn fill_reach(p: &Params, h: f64, first: bool) -> f64 {
    let w_fill = if first { p.w_first } else { p.w_solid };
    (flow::spacing(w_fill, h) / 2.0 - p.infill_overlap * flow::spacing(if first { p.w_first } else { (p.w_outer + p.w_inner) / 2.0 }, h).max(0.0)).max(0.0)
}

/// What elephant-foot compensation leaves of a first layer: the outline shrunk, with what the
/// shrink would wipe out kept as it is. Printing a fin from the second layer up, with nothing on
/// the plate under it, would be worse than its foot.
pub fn foot_compensated(p: &Params, raw: &[ExPolygon]) -> Vec<ExPolygon> {
    let foot = p.elephant_foot * SCALE;
    let mut out = offset_ex(raw, -foot, DEFAULT_MITER);
    let lost = difference_ex(raw, &offset_ex(&out, foot, DEFAULT_MITER));
    let min = p.gap_fill.min_feature_size / 2.0 * SCALE;
    out.extend(opening(&lost, min, min, DEFAULT_MITER));
    out
}

/// A gap fill line shorter than this (mm) is a speck, not worth the travel to it.
const GAP_MIN_LENGTH: f64 = 0.2;
/// The steepest taper a gap fill line follows (degrees between the place's two sides). The walls'
/// own transition angle decides where their bead count may change; a place that tapers more than
/// that would get broken pieces of a line, or none. A right-angled corner stays no place for one.
const GAP_TAPER_ANGLE: f64 = 45.0;
/// A gap fill line keeps its shape and width to these (mm) when points are dropped: it fills a
/// place, it shows no surface, and every point left is a move.
const GAP_SIMPLIFY: (f64, f64) = (0.04, 0.05);

/// Centre lines through `regions`, as wide as each place is. A region narrower than the job's
/// narrowest gap holds none.
fn gap_lines(p: &Params, regions: &[ExPolygon], w: f64, h: f64) -> Vec<BeadLine> {
    gap_lines_memo(regions, &mut arachne::Memo::new(&gap_config(p, w, h)))
}

/// [`gap_lines`] with the memo's settings (from [`gap_config`]).
fn gap_lines_memo(regions: &[ExPolygon], memo: &mut arachne::Memo) -> Vec<BeadLine> {
    let min = memo.cfg().min_feature_size / 2.0 * SCALE;
    let mut out = lines_through(opening(regions, min, min, Join::Miter(3.0)), memo);
    for l in &mut out {
        arachne::simplify(l, GAP_SIMPLIFY.0, GAP_SIMPLIFY.1);
    }
    out
}

/// Centre lines through places whose sides are other beads. A bead's section is short of width
/// times height by the rounding of its sides, which its spacing states, and `arachne` keeps its
/// whole width inside the outline, as a part's outside needs. Beads touch at spacing, so the place
/// grows by half that difference: the line's spacing is the place's width, and its volume the
/// place's area times the height. A line's narrow ends would set its width once the points between
/// them are dropped, so each line keeps the volume it had.
fn place_lines(p: &Params, regions: &[ExPolygon], w: f64, h: f64) -> Vec<BeadLine> {
    let cfg = gap_config(p, w, h);
    let min = cfg.min_feature_size / 2.0 * SCALE;
    let rounding = (w - flow::spacing(w, h)) / 2.0 * SCALE;
    let mut out = lines_through(offset_ex(&opening(regions, min, min, Join::Miter(3.0)), rounding, Join::Miter(3.0)), &mut arachne::Memo::new(&cfg));
    for l in &mut out {
        let area = line_area(l);
        arachne::simplify(l, GAP_SIMPLIFY.0, GAP_SIMPLIFY.1);
        let kept = line_area(l);
        if kept > 0.0 {
            l.widths.iter_mut().for_each(|w| *w *= area / kept);
        }
    }
    out
}

/// A gap's line is as wide as its place: widened, it would run into the beads on either side,
/// or past the model.
fn gap_config(p: &Params, w: f64, h: f64) -> arachne::Config {
    let mut cfg = p.gap_fill.config(1, w, w, h, false);
    cfg.widen = false;
    cfg.transition_angle = GAP_TAPER_ANGLE.to_radians();
    cfg
}

/// Each piece's centre lines with the memo's settings; an identical part (in local units) is
/// taken from the memo.
fn lines_through(pieces: Vec<ExPolygon>, memo: &mut arachne::Memo) -> Vec<BeadLine> {
    let mut out = Vec::new();
    for piece in pieces {
        out.extend(arachne::walls_memo(&piece, memo).lines.into_iter().filter(|l| {
            let length: f64 = l.pts.windows(2).map(|s| s[0].dist(s[1])).sum();
            length / SCALE >= GAP_MIN_LENGTH
        }));
    }
    out
}

/// What a bead line lays per layer height (mm2): its width, in spacing, along its length.
fn line_area(l: &BeadLine) -> f64 {
    let n = l.pts.len();
    let run = if l.closed { n } else { n.saturating_sub(1) };
    (0..run).map(|i| {
        let k = (i + 1) % n;
        l.pts[i].dist(l.pts[k]) / SCALE * (l.widths[i] + l.widths[k]) / 2.0
    }).sum()
}


/// What a bead covers, as one rectangle per segment: the place the rules below take from their
/// areas.
fn bead_strips(path: &ExtrusionPath, strips: &mut Vec<ExPolygon>) {
    let half = path.width / 2.0 * SCALE;
    let mut strip = |a: Pt, b: Pt| {
        let (dx, dy) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
        let len = dx.hypot(dy);
        if len > 0.0 {
            // Past each end by its half width too: the next piece's corner is the bead's.
            let (nx, ny) = ((-dy / len * half) as i64, (dx / len * half) as i64);
            let (a, b) = (Pt::new(a.x - ny, a.y + nx), Pt::new(b.x + ny, b.y - nx));
            strips.push(ExPolygon::new(Polygon::new(vec![Pt::new(a.x + nx, a.y + ny), Pt::new(a.x - nx, a.y - ny), Pt::new(b.x - nx, b.y - ny), Pt::new(b.x + nx, b.y + ny)])));
        }
    };
    for s in path.pts.windows(2) {
        strip(s[0], s[1]);
    }
    if let (true, Some(&end), Some(&start)) = (path.closed, path.pts.last(), path.pts.first()) {
        strip(end, start);
    }
}

/// The beads a concentric piece's own `beads` leave room for, in its role. A round's rings keep
/// their two sides a spacing apart, so a place narrower than two spacings holds no ring: a neck
/// between wider parts, or a whole strip that bends, whose extent says nothing of its thickness.
/// A place narrower than twice the fill's reach is the walls' gap fill, already among `gaps`; from
/// there up, and between a piece's own beads at any width gap fill would take, the place gets a
/// centre line with the place's width as its spacing, or two sharing it. So no width falls between
/// the rules, none gets a bead laid twice, and the lines' volume is what the place holds.
fn left_beads(p: &Params, role: Role, piece: &ExPolygon, beads: &[ExtrusionPath], gaps: &[ExtrusionPath], h: f64, first: bool) -> Vec<ExtrusionPath> {
    let bb = piece.bbox();
    let near = |g: &&ExtrusionPath| bb.overlaps(&BBox::of(&g.pts));
    let mut strips = Vec::new();
    for path in beads.iter().chain(gaps.iter().filter(near)) {
        bead_strips(path, &mut strips);
    }
    let left = difference_ex(std::slice::from_ref(piece), &strips);
    if left.is_empty() {
        return Vec::new();
    }
    let from = beads.last().and_then(|b| b.pts.last()).copied().unwrap_or(bb.min);
    let mut out = walls::gap_paths(&place_lines(p, &left, if first { p.w_first } else { p.w_solid }, h), h, from);
    for b in &mut out {
        b.role = role;
        b.height = h;
        b.area = flow::cross_section(b.width, h);
        b.speed = p.speed(role, b.area, first);
    }
    out
}

/// Top, bottom and bridge surfaces: their lines leave places that get gap fill.
fn is_surface(role: Role) -> bool {
    matches!(role, Role::TopSurface | Role::BottomSurface | Role::Bridge | Role::InternalBridge)
}

/// The gap fill of an island's surfaces: what the surface lines among `fills` leave of `areas`,
/// together, as a place can lie across two of them. Narrower than a bead is left alone: that is
/// the teeth between line ends and the corners a line stops short of, which any outline has and
/// the beads around them close. A place no line reached is wider.
fn surface_gaps(p: &Params, areas: &[ExPolygon], fills: &[ExtrusionPath], h: f64, first: bool, center: Pt) -> Vec<ExtrusionPath> {
    let mut strips = Vec::new();
    let mut from = center;
    for path in fills.iter().filter(|f| is_surface(f.role)) {
        from = path.pts.last().copied().unwrap_or(from);
        bead_strips(path, &mut strips);
    }
    let open = 0.5 * p.w_solid * SCALE;
    let left = opening(&difference_ex(&union_ex(areas), &strips), open, open, DEFAULT_MITER);
    let beads = gap_lines(p, &left, p.w_solid, h);
    let mut gaps = walls::gap_paths(&beads, h, from);
    for g in gaps.iter_mut() {
        g.height = h;
        g.area = flow::cross_section(g.width, h);
        g.speed = p.speed(g.role, g.area, first);
    }
    gaps
}

/// A part's planned supports, from [`plan_supports`].
#[derive(Clone, Debug, Default)]
pub struct PlannedSupports {
    /// A model layer index and islands to append, or `usize::MAX` and a separate
    /// normal-support layer with its own z/height.
    pub additions: Vec<(usize, f64, f64, ObjectLayer)>,
    /// The same supports as solid geometry, and what they tell unless `report` is still to.
    pub skeleton: skeleton::Skeleton,
    /// What tells a tree's report ([`TreeReport::tell`]), when [`plan_support_paths`] left it
    /// for after the paths.
    pub report: Option<Arc<TreeReport>>,
}

/// The part's supports, and what they tell.
/// The outline is the whole part after XY compensation and non-planar clipping,
/// before paint and elephant-foot compensation. Supports always belong to
/// the object's own filament and never enter the model's windowed passes.
/// `trees` is what the part's tree supports kept from its run before ([`tree::Cache`]), and
/// keeps this run's.
pub fn plan_supports(ctl: par::Ctl, oi: usize, o: &ObjectInput, job: &Job, outline: &[Vec<ExPolygon>], heights: &[(f64, f64)], trees: &mut tree::Cache) -> Result<PlannedSupports, par::Cancelled> {
    let mut supports = plan_support_paths(ctl, oi, o, job, outline, heights, trees)?;
    if let Some(report) = supports.report.take() {
        supports.skeleton = report.tell(ctl, &supports.skeleton)?;
    }
    Ok(supports)
}

/// The part's supports as [`plan_supports`] plans them, but a tree's report: what the rules
/// check and the hold measure tell of it is left to its [`PlannedSupports::report`], so its
/// paths can go out first.
pub fn plan_support_paths(ctl: par::Ctl, oi: usize, o: &ObjectInput, job: &Job, outline: &[Vec<ExPolygon>], heights: &[(f64, f64)], trees: &mut tree::Cache) -> Result<PlannedSupports, par::Cancelled> {
    let own;
    let job = if o.overrides.is_empty() { job } else { own = job.with_overrides(&o.overrides); &own };
    let p = Params::from_job(job, o.filament);
    let (lo, hi) = o.source.bounds();
    let (a, b) = o.source.pivot_box();
    let center = Pt::from_mm((a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0);
    if ctl.cancelled() {
        return Err(par::Cancelled);
    }
    // The items printed into the part count as part here: no support stands where one goes, and
    // the layer that closes one in rests on it.
    let with_items;
    let outline = if o.source.embeds().is_empty() {
        outline
    } else {
        with_items = par::try_map(ctl, outline.len(), |l| {
            let cavity = heights.get(l).map(|&(z, h)| o.source.cavity(cut_z(&p, z, h))).unwrap_or_default();
            if cavity.is_empty() { outline[l].clone() } else { union_ex(&[outline[l].as_slice(), &cavity].concat()) }
        })?;
        &with_items[..]
    };
    let mut out = Vec::new();
    let mut skeleton = skeleton::Skeleton::default();
    let mut report = None;
    let floor = if p.raft_layers > 0 { heights.first().map(|&(z, h)| z - h) } else { None };
    let paint = match o.source.support_paint().filter(|_| p.support.is_some() || p.normal_support.is_some()) {
        Some(paint) => {
            let zs: Vec<f64> = heights.iter().map(|&(z, h)| cut_z(&p, z, h)).collect();
            support_paint::Masks::of(ctl, Some(paint), &zs, (lo, hi), paint_reach(job, (lo, hi)))?
        }
        None => support_paint::Masks::default(),
    };
    if let Some(sp) = p.support.as_ref().filter(|_| p.coral) {
        let _t = prof::scope(prof::Stage::CoralSupport);
        let supports = coral::generate(ctl, outline, heights, sp, paint.block_layers())?;
        skeleton.given_up = supports.given_up;
        skeleton.blocked = supports.blocked;
        skeleton.hold = Some(supports.hold);
        let layers = par::try_map_vec(ctl, supports.layers, |li, mut paths| {
            let (z, h) = heights[li];
            let h = floor.map_or(h, |floor| h.min(z - floor));
            if h <= 0.0 { paths.clear(); }
            if p.first(li) {
                for path in &mut paths { path.width = p.w_first; }
            }
            let island = coral_paths(&p, &paths, h, p.first(li));
            let mut layer = ObjectLayer { object: oi, filament: o.filament, islands: vec![Arc::new(island)], ..Default::default() };
            mark_adaptive(&p, &mut layer);
            let (roof, body): (Vec<_>, Vec<_>) = paths.into_iter().partition(|p| p.role == coral::material::Role::Interface);
            let slab = skeleton::Slab { z: z - p.raft_lift, height: h.max(0.0), region: coral::material::material(&body) };
            ((li, z, h, layer), coral::material::material(&roof), slab)
        })?;
        let mut roofs = Vec::with_capacity(layers.len());
        for (addition, roof, slab) in layers {
            if addition.3.has_paths() { out.push(addition); }
            roofs.push(roof);
            // Keep empty layers too: only consecutive slabs can join.
            skeleton.slabs.push(slab);
        }
        let at_part: Vec<(f64, f64)> = heights.iter().map(|&(z, h)| (z - p.raft_lift, h)).collect();
        skeleton.finish_tree(&at_part, roofs);
        // Tree rules describe filled footprints; the sampled coral proof stays out of slicing.
    } else if let Some(sp) = &p.support {
        let _t = prof::scope(prof::Stage::TreeSupport);
        let (additions, told) = tree_supports(ctl, oi, o, job, &p, sp, outline, heights, &paint, floor, &mut skeleton, trees)?;
        out.extend(additions.into_iter().filter(|a| a.3.has_paths()));
        report = Some(Arc::new(told));
    }
    if let Some(sp) = &p.normal_support {
        let _t = prof::scope(prof::Stage::NormalSupport);
        for (li, mut layer) in normal_support::generate(ctl, outline, heights, &p, sp, center, &paint, &mut skeleton)?.into_iter().enumerate() {
            if ctl.cancelled() {
                return Err(par::Cancelled);
            }
            if let Some(floor) = floor {
                layer.height = layer.height.min(layer.z - floor);
                if layer.height <= 0.0 { continue; }
            }
            let island = normal_support::paths(&p, sp, &layer, li, center);
            if !island.fills.is_empty() {
                let footprint = if p.raft_layers > 0 {
                    offset_ex(&union_ex(&[layer.base.clone(), layer.interface.clone(), layer.bottom.clone(), layer.transition.clone()].concat()), sp.width / 2.0 * SCALE, Join::Miter(3.0))
                } else if p.first(li) {
                    let base = offset_ex(&layer.base, p.w_first / 2.0 * SCALE, Join::Miter(3.0));
                    let roof = offset_ex(&layer.interface, sp.width / 2.0 * SCALE, Join::Miter(3.0));
                    union_ex(&[base, roof].concat())
                } else { Vec::new() };
                let mut ol = ObjectLayer { object: oi, filament: o.filament, islands: vec![Arc::new(island)], slices: Arc::new(footprint), support_only: true };
                mark_adaptive(&p, &mut ol);
                out.push((usize::MAX, layer.z, layer.height, ol));
                skeleton.slabs.push(skeleton::Slab { z: layer.z - p.raft_lift, height: layer.height, region: [layer.base, layer.interface, layer.bottom, layer.transition].concat() });
            }
        }
    }
    if ctl.cancelled() { Err(par::Cancelled) } else { Ok(PlannedSupports { additions: out, skeleton, report }) }
}

/// The steps of a part's tree pass, in order: what each tells as it goes ([`par::Ctl::phase`]),
/// and its share of the pass's time as measured on the demo plate's parts (each part's wall time
/// per step, summed, by the tree_shares measurement that printed this table, now in history).
/// The tree's own stages ([`tree::Stage`]) tick as they make their layers; the steps after them
/// tick by their maps.
const TREE_STEPS: [(&str, f64); 10] = [
    ("preparing the slices", 0.032),
    ("finding overhangs", 0.108),
    ("laying tips and roofs", 0.228),
    ("growing trees", 0.303),
    ("smoothing the branches", 0.043),
    ("drawing the branches", 0.041),
    ("laying the lines", 0.047),
    ("laying paths", 0.043),
    ("checking the rules", 0.126),
    ("measuring what it holds", 0.029),
];

/// The pass's step that tells for a tree stage.
fn tree_step(stage: tree::Stage) -> usize {
    match stage {
        tree::Stage::Slices => 0,
        tree::Stage::Overhangs => 1,
        tree::Stage::Tips => 2,
        tree::Stage::Descent => 3,
        tree::Stage::Smooth => 4,
        tree::Stage::Draw => 5,
        tree::Stage::Paths => 6,
    }
}

/// Where in the tree's pass step `k` begins and ends, as shares of it.
fn tree_span(k: usize) -> (f64, f64) {
    let all: f64 = TREE_STEPS.iter().map(|s| s.1).sum();
    let before: f64 = TREE_STEPS[..k].iter().map(|s| s.1).sum();
    (before / all, (before + TREE_STEPS[k].1) / all)
}

/// How far a part's tree stages are, from what they tell ([`Emit::Tree`]): each stage's shares
/// counted as they come, told to the pass's [`par::Ctl::phase`] at most once per thousandth of
/// the pass.
struct TreeTold<'a> {
    ctl: par::Ctl<'a>,
    done: [std::sync::atomic::AtomicUsize; 7],
    told: std::sync::atomic::AtomicUsize,
}

impl<'a> TreeTold<'a> {
    fn new(ctl: par::Ctl<'a>) -> Self {
        TreeTold { ctl, done: Default::default(), told: std::sync::atomic::AtomicUsize::new(0) }
    }

    /// One more of the `of` shares `stage` tells is made.
    fn tick(&self, stage: tree::Stage, of: usize) {
        use std::sync::atomic::Ordering;
        let k = tree_step(stage);
        let done = (self.done[k].fetch_add(1, Ordering::Relaxed) + 1).min(of);
        let (lo, hi) = tree_span(k);
        let at = lo + (hi - lo) * done as f64 / of.max(1) as f64;
        let mille = (at * 1000.0) as usize;
        if self.told.fetch_max(mille, Ordering::Relaxed) < mille || done == of {
            self.ctl.phase(TREE_STEPS[k].0, Some((done as u32, of as u32)), at);
        }
    }
}

/// The part's tree supports as R14 builds them ([`tree::generate`]), from the job's keys
/// ([`tree_keys`], [`tree_print`]) and the part's paint, on the bed wherever the part stands
/// ([`bed_area`]). The tree is built in the part's frame (R14 §1.4): the plan's frame moved so
/// its mesh box's XY centre, in whole nm, is at (0, 0), and what it makes is moved back by
/// exactly as much. Its layers stand on the part's own bed: a raft lifts the print, not the
/// part's frame. Per model layer, what it prints as one island, cut at the raft's floor. In
/// `skeleton`: its material (the base as body, the first layer's with its pad, and the interface
/// as roofs); what it tells is its [`TreeReport`]'s. It keeps out of what [`keep_out`] gives.
fn tree_supports(ctl: par::Ctl, oi: usize, o: &ObjectInput, job: &Job, p: &Params, sp: &support::SupportParams, outline: &[Vec<ExPolygon>], heights: &[(f64, f64)], paint: &support_paint::Masks, floor: Option<f64>, skeleton: &mut skeleton::Skeleton, trees: &mut tree::Cache) -> Result<(Vec<(usize, f64, f64, ObjectLayer)>, TreeReport), par::Cancelled> {
    let n = outline.len().min(heights.len());
    let ((lo, hi), (a, b)) = (o.source.bounds(), o.source.pivot_box());
    let at = Pt::new(((a[0] + b[0]) / 2.0 * SCALE).trunc() as i64, ((a[1] + b[1]) / 2.0 * SCALE).trunc() as i64);
    let outside = job.b(OUTSIDE_FOOTPRINT);
    // What the trees grow from, as the part's run before left it when that was made from the
    // same, else afresh.
    let prep = match trees.prep.take() {
        Some(prep) if prep.made_from(outline, heights, sp, paint, outside, at) => prep,
        was => match TreePrep::new(ctl, outline, heights, sp, paint, outside, at, was.as_ref()) {
            Ok((prep, made)) => {
                trees.work.locked += made;
                prep
            }
            Err(err) => {
                trees.prep = was;
                return Err(err);
            }
        },
    };
    let TreePrep { slices, footprint, keep, enforce, block, kept, room, .. } = &prep;
    // The sliced checks never tell what lies over the keep-out as uncovered.
    skeleton.keep_out = keep.clone();
    let at_part: Vec<(f64, f64)> = heights[..n].iter().map(|&(z, h)| (z - p.raft_lift, h)).collect();
    // The tips the user deleted on the part, in the trees' frame as `at_part` is.
    let dropped: Vec<[f64; 3]> = dropped_tips(job).into_iter().map(|[x, y, z]| [x - at.x_mm(), y - at.y_mm(), z - p.raft_lift]).collect();
    let part = BBox { min: Pt::from_mm(lo[0], lo[1]).add(Pt::new(-at.x, -at.y)), max: Pt::from_mm(hi[0], hi[1]).add(Pt::new(-at.x, -at.y)) };
    let bed = bed_area(job, &part);
    ctl.phase(TREE_STEPS[0].0, None, 0.0);
    let input = tree::Input { keys: tree_keys(job, p, sp), print: tree_print(p, sp), slices, heights: &at_part, enforce, block, keep_out: kept, bed: &bed, dropped: &dropped };
    // The stages tell how far they are, and what they make goes on to the view, moved into the
    // plan's frame.
    let told = TreeTold::new(ctl);
    let grown = |e: Emit| {
        if let Emit::Tree { stage, layer, of, grown, .. } = e {
            if !matches!(grown, tree::Grown::Trees { .. }) {
                told.tick(stage, of);
            }
            if let Some(emit) = ctl.emit {
                emit(Emit::Tree { stage, layer, of, grown, frame: at });
            }
        }
    };
    let g = tree::generate(par::Ctl { emit: Some(&grown), ..ctl }, &input, trees);
    let report = (footprint.clone(), keep.clone(), room.as_ref().map_or_else(Vec::new, |r| r.locked.clone()));
    // What the trees grew from stays for the part's next run, whether this one ran through or not.
    trees.prep = Some(prep);
    let g = g?;
    let moved = |set: &[ExPolygon]| -> Vec<ExPolygon> {
        set.iter().map(|e| {
            let mut e = e.clone();
            e.translate(at);
            e
        }).collect()
    };
    // Its layers as the rules read them: the base as a filled footprint, the first layer's with
    // its pad, and the interface as its roof.
    let none: &[ExPolygon] = &[];
    let layers = par::try_map(ctl, n, |l| {
        let base = g.layers.base.get(l).map_or(none, Vec::as_slice);
        let base = if l == 0 && !g.layers.pad.is_empty() { union_ex(&[base, g.layers.pad.as_slice()].concat()) } else { base.to_vec() };
        support::SupportLayer { base: moved(&base), interface: moved(g.layers.interface.get(l).map_or(none, Vec::as_slice)), ..Default::default() }
    })?;
    let (lo, hi) = tree_span(7);
    let step = par::Step::new(ctl, TREE_STEPS[7].0, lo, hi, n);
    let tick = |k: usize| step.tick(k);
    let layer_of = |island| ObjectLayer { object: oi, filament: o.filament, islands: vec![island], ..Default::default() };
    let from = |height| TreeIslandFrom { at, height, max_volumetric: p.speeds.max_volumetric, adaptive: p.exp.adaptive.is_some() };
    let last = &trees.islands;
    let additions = par::try_map(par::Ctl { tick: Some(&tick), ..ctl }, n, |li| {
        let (z, h) = heights[li];
        let h = floor.map_or(h, |floor| h.min(z - floor));
        let paths = g.paths.get(li).cloned().unwrap_or_default();
        // What the part's last plan printed from the same stays as it is: plans next to each
        // other hold it once.
        if let Some(same) = last.get(li).filter(|t| Arc::ptr_eq(&t.paths, &paths) && t.from == from(h)) {
            return (li, z, h, layer_of(same.island.clone()));
        }
        let mut island = Island::default();
        if h > 0.0 {
            island.fills = paths.iter().map(|path| {
                let mut path = path.clone();
                for q in path.pts.iter_mut().chain(path.loop_pts.iter_mut()) {
                    *q = q.add(at);
                }
                path.height = h;
                path.area = flow::cross_section(path.width, h);
                if p.speeds.max_volumetric > 0.0 {
                    path.speed = path.speed.min(p.speeds.max_volumetric / path.area);
                }
                path
            }).collect();
        }
        let mut layer = layer_of(Arc::new(island));
        mark_adaptive(p, &mut layer);
        (li, z, h, layer)
    })?;
    trees.islands = additions.iter().map(|(li, _, h, layer)| TreeIsland { paths: g.paths.get(*li).cloned().unwrap_or_default(), from: from(*h), island: layer.islands[0].clone() }).collect();
    // Empty layers too: only consecutive slabs can join.
    skeleton.slabs.extend(par::try_map(ctl, n, |l| skeleton::Slab { z: at_part[l].0, height: additions[l].2.max(0.0), region: layers[l].base.clone() })?);
    skeleton.finish_tree(&at_part, par::try_map(ctl, n, |l| layers[l].interface.clone())?);
    use coral::material::{of_extrusion, Role as Material};
    let printed = par::try_map(ctl, n, |l| additions[l].3.islands.iter().flat_map(|isl| of_extrusion(&isl.fills, |e| if e.role == Role::SupportInterface { Material::Interface } else { Material::Body }, 0)).collect())?;
    let (footprint, keep, locked) = report;
    let report = TreeReport { outline: outline.to_vec(), heights: heights.to_vec(), sp: sp.clone(), paint: paint.clone(), layers, printed, keep, footprint, locked };
    Ok((additions, report))
}

/// What a part's trees grow from ([`tree_supports`]), with what it was made from: the part's
/// slices in the trees' frame (round the part's middle, `at`), the footprint the part keeps its
/// trees out of and the keep-out, the paint's enforced faces and blocks in that frame, and where a
/// tree would be locked in the part. A tree deleted changes none of it, so the part's next run
/// on the same outline, heights, support, paint and footprint setting takes it as it is
/// ([`tree::Cache`]).
pub(crate) struct TreePrep {
    outline: Vec<Vec<ExPolygon>>,
    heights: Vec<(f64, f64)>,
    sp: support::SupportParams,
    paint: support_paint::Masks,
    outside: bool,
    at: Pt,
    slices: Vec<Vec<ExPolygon>>,
    footprint: Vec<Vec<ExPolygon>>,
    keep: Arc<Vec<Vec<ExPolygon>>>,
    enforce: Vec<Vec<ExPolygon>>,
    block: Vec<Vec<ExPolygon>>,
    kept: Vec<Vec<ExPolygon>>,
    keeps: Vec<Arc<Vec<ExPolygon>>>,
    room: Option<support::Locked>,
}

impl TreePrep {
    fn new(ctl: par::Ctl, outline: &[Vec<ExPolygon>], heights: &[(f64, f64)], sp: &support::SupportParams, paint: &support_paint::Masks, outside: bool, at: Pt, was: Option<&TreePrep>) -> Result<(TreePrep, usize), par::Cancelled> {
        let n = outline.len().min(heights.len());
        let into = |sets: &[Vec<ExPolygon>]| -> Vec<Vec<ExPolygon>> {
            sets.iter().map(|set| set.iter().map(|e| {
                let mut e = e.clone();
                e.translate(Pt::new(-at.x, -at.y));
                e
            }).collect()).collect()
        };
        let slices = into(&outline[..n]);
        let footprint = if outside { footprint_below(ctl, &outline[..n])? } else { Vec::new() };
        let keep = keep_out(ctl, n, &footprint, paint.no_trees_layers())?;
        // The paint's enforced faces, not its regions, block winning
        // ([`support_paint::Masks::enforced`]).
        let (enforce, block, kept) = (into(&paint.enforced(ctl, &outline[..n])?), into(paint.block_layers()), into(&keep));
        let keep = Arc::new(keep);
        // Where a tree would be locked in the part ([`support::locked_in`]), round what keeps
        // support off. Unless the part lets its supports be, R14 is told to block what of the part
        // lies over it where a contact under that would print, so no tip goes there, and what of
        // the overhang is bare there is told as locked ([`TreeReport::tell`]). None where the part
        // keeps its trees out of its footprint: every cell it closes lies in that.
        let legal = support_rules::Legal::new(outline, heights, sp, paint.block_layers(), &[]);
        let (keeps, room, made) = if footprint.is_empty() {
            let was = was.filter(|b| b.heights == heights && b.sp == *sp && b.footprint.is_empty() && b.room.is_some());
            let same = match was {
                Some(b) => par::try_map(ctl, n, |l| b.outline.get(l) == outline.get(l))?,
                None => vec![false; n],
            };
            let taken = match was {
                Some(b) => par::try_map(ctl, n, |l| legal.reads(l).all(|k| same[k]) && b.paint.block_layers().get(l) == paint.block_layers().get(l))?,
                None => vec![false; n],
            };
            let keeps = par::try_map(ctl, n, |l| match was.filter(|_| taken[l]) {
                Some(b) => b.keeps[l].clone(),
                None => Arc::new(legal.keeps(l)),
            })?;
            let room = sp.xy_gap + (sp.branch_diameter / 2.0).max(sp.tip_diameter / 2.0).max(0.05);
            let refs: Vec<&[ExPolygon]> = keeps.iter().map(|k| k.as_slice()).collect();
            let room = support::locked_in(ctl, &refs, &taken, was.and_then(|b| b.room.as_ref()), &outline[..n], support::window(sp), room)?;
            (keeps, Some(room), taken.iter().filter(|&&t| !t).count())
        } else {
            (Vec::new(), None, 0)
        };
        let locked = room.as_ref().map_or(&[][..], |r| r.locked.as_slice());
        let block = if sp.stand_inside || locked.iter().all(Vec::is_empty) {
            block
        } else {
            let over = into(&par::try_map(ctl, n, |d| match legal.contact(d).map(|c| locked[c].as_slice()).filter(|x| !x.is_empty()) {
                Some(x) => intersection_ex(&outline[d], x),
                None => Vec::new(),
            })?);
            par::try_map(ctl, n, |d| {
                let set = block.get(d).map_or(&[][..], Vec::as_slice);
                match (set.is_empty(), over[d].is_empty()) {
                    (_, true) => set.to_vec(),
                    (true, false) => over[d].clone(),
                    (false, false) => union_ex(&[set, over[d].as_slice()].concat()),
                }
            })?
        };
        Ok((TreePrep { outline: outline.to_vec(), heights: heights.to_vec(), sp: sp.clone(), paint: paint.clone(), outside, at, slices, footprint, keep, enforce, block, kept, keeps, room }, made))
    }

    fn made_from(&self, outline: &[Vec<ExPolygon>], heights: &[(f64, f64)], sp: &support::SupportParams, paint: &support_paint::Masks, outside: bool, at: Pt) -> bool {
        self.at == at && self.outside == outside && self.heights == heights && self.sp == *sp && self.paint == *paint && self.outline == outline
    }
}

/// What tells a part's tree report, after its paths ([`plan_support_paths`]): the part, its paint
/// and keep-out, the trees' layers as the rules read them, and what each layer prints.
pub struct TreeReport {
    outline: Vec<Vec<ExPolygon>>,
    heights: Vec<(f64, f64)>,
    sp: support::SupportParams,
    paint: support_paint::Masks,
    /// Per layer, the base as a filled footprint (the first layer's with its pad) and the
    /// interface as its roof.
    layers: Vec<support::SupportLayer>,
    /// Per layer, what it prints, as material.
    printed: Vec<Vec<coral::material::SupportPath>>,
    /// Per layer, where the trees may not go ([`keep_out`]).
    keep: Arc<Vec<Vec<ExPolygon>>>,
    /// Per layer, of that, the part's footprint where it keeps its trees out of it: none when it
    /// does not.
    footprint: Vec<Vec<ExPolygon>>,
    /// Per layer, where a support would be locked in the part ([`support::locked_in`]).
    locked: Vec<Vec<ExPolygon>>,
}

impl std::fmt::Debug for TreeReport {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.debug_struct("TreeReport").field("layers", &self.layers.len()).finish_non_exhaustive()
    }
}

impl TreeReport {
    /// What it holds until it has told (bytes): its regions and paths.
    fn bytes(&self) -> usize {
        let regions = |v: &[Vec<ExPolygon>]| v.iter().map(|r| regions_bytes(r)).sum::<usize>();
        let layers: usize = self.layers.iter().map(|l| regions_bytes(&l.base) + regions_bytes(&l.interface) + regions_bytes(&l.solid) + regions_bytes(&l.inside)).sum();
        let printed: usize = self.printed.iter().flatten().map(|p| size_of::<coral::material::SupportPath>() + p.pts.len() * size_of::<Pt>()).sum();
        regions(&self.outline) + regions(&self.footprint) + regions(&self.locked) + layers + printed
    }

    /// `shape`, the part's trees as [`tree_supports`] made them, with what they tell: the rules
    /// they break ([`support_rules::check`]), how much of the overhang they hold
    /// ([`skeleton::Hold`]), what the paint blocks, and every place of the overhang they leave
    /// bare, by why: kept out and not asked of them over the keep-out (by the footprint where it
    /// lies over it, else by a no-trees area); closed in by the part, so no tree gets to it;
    /// where a tree under it would be locked in the part; else given up. Where the part lets its
    /// supports be locked in, what of the trees is.
    pub fn tell(&self, ctl: par::Ctl, shape: &skeleton::Skeleton) -> Result<skeleton::Skeleton, par::Cancelled> {
        let (outline, heights, sp, paint, keep, footprint, locked) = (&self.outline[..], &self.heights[..], &self.sp, &self.paint, &self.keep, &self.footprint, &self.locked);
        let n = outline.len().min(heights.len());
        let mut skeleton = shape.clone();
        let legal = support_rules::Legal::new(outline, heights, sp, paint.block_layers(), &[]);
        let (lo, hi) = tree_span(8);
        let step = par::Step::new(ctl, TREE_STEPS[8].0, lo, hi, n);
        let tick = |k: usize| step.tick(k);
        let printed = &self.printed;
        let prints = |l: usize, piece: &ExPolygon| printed.get(l).is_some_and(|paths| support_rules::prints_in(piece, paths));
        skeleton.breaks = support_rules::check(par::Ctl { tick: Some(&tick), ..ctl }, &legal, &self.layers, &prints)?;
        // What it was asked to hold: the part's overhang under its paint (what block takes is
        // told as that), and what of it lies within half the contact gap of what prints on its
        // contact's layer.
        let demand = support::demand(ctl, outline, heights, sp)?;
        let painted = paint.demand(ctl, outline, demand, sp.line_width, 0.0, &mut skeleton.blocked)?;
        let asked: Vec<Vec<ExPolygon>> = painted.demand.into_iter().enumerate().map(|(l, d)| match painted.added.get(l).filter(|a| !a.is_empty()) {
            Some(a) => union_ex(&[d, a.clone()].concat()),
            None => d,
        }).collect();
        let specks = |set: Vec<ExPolygon>| -> Vec<ExPolygon> { set.into_iter().filter(|e| e.area_mm2() >= support::SPECK).collect() };
        // `set` split by `by`: what lies outside it, and what lies in it.
        let split = |set: Vec<ExPolygon>, by: &[ExPolygon]| -> (Vec<ExPolygon>, Vec<ExPolygon>) {
            if set.is_empty() || by.is_empty() { (set, Vec::new()) } else { (difference_ex(&set, by), intersection_ex(&set, by)) }
        };
        if asked.iter().any(|a| !a.is_empty()) {
            let printed = &self.printed;
            // What prints on layer `l` near `need`: in a box round each of its islands, as near
            // as the hold looks ([`support_rules::HELD_NEAR`]).
            let reach = (support_rules::HELD_NEAR * SCALE).ceil() as i64;
            let near = |l: usize, need: &[ExPolygon]| -> Vec<ExPolygon> {
                let boxes: Vec<BBox> = need.iter().map(|e| {
                    let b = e.bbox();
                    BBox { min: Pt::new(b.min.x - reach, b.min.y - reach), max: Pt::new(b.max.x + reach, b.max.y + reach) }
                }).collect();
                coral::material::material_in(printed.get(l).map_or(&[][..], Vec::as_slice), &boxes)
            };
            let (lo, hi) = tree_span(9);
            let step = par::Step::new(ctl, TREE_STEPS[9].0, lo, hi, asked.len());
            let tick = |k: usize| step.tick(k);
            // Per layer: what is held (mm2), what is bare and not kept out, what is kept out
            // (mm2), and that by the footprint and by a no-trees area.
            let measured = par::try_map(par::Ctl { tick: Some(&tick), ..ctl }, asked.len(), |d| {
                if asked[d].is_empty() {
                    return (0.0, Vec::new(), 0.0, None, None);
                }
                let held = legal.contact(d).map_or_else(Vec::new, |c| support_rules::held(&asked[d], &near(c, &asked[d])));
                let bare = if held.is_empty() { asked[d].clone() } else { difference_ex(&asked[d], &held) };
                // What is bare over the keep-out on the layer under it: no tree got out from under
                // it. Told as kept out, and not asked of the tree: by the footprint where it lies
                // over it, else by the no-trees area it lies over.
                let (bare, kept) = split(bare, d.checked_sub(1).and_then(|b| keep.get(b)).map_or(&[][..], Vec::as_slice));
                let (bare, kept) = (specks(bare), specks(kept));
                let area = total_area_mm2(&kept);
                let below = d.checked_sub(1);
                let over_footprint = below.and_then(|b| footprint.get(b)).filter(|s| !s.is_empty());
                let over_no_trees = below.and_then(|b| paint.no_trees_layers().get(b)).is_some_and(|s| !s.is_empty());
                let (kept_out, no_trees) = match (over_footprint, over_no_trees) {
                    _ if kept.is_empty() => (Vec::new(), Vec::new()),
                    (Some(_), false) => (kept, Vec::new()),
                    (None, _) => (Vec::new(), kept),
                    (Some(over), true) => (specks(intersection_ex(&kept, over)), specks(difference_ex(&kept, over))),
                };
                (total_area_mm2(&held), bare, area, skeleton::GivenUp::of(d, &kept_out), skeleton::GivenUp::of(d, &no_trees))
            })?;
            let need = asked.iter().map(|a| total_area_mm2(a)).sum::<f64>() - measured.iter().map(|m| m.2).sum::<f64>();
            skeleton.hold = Some(skeleton::Hold::of(need, measured.iter().map(|m| m.0).sum(), sp));
            skeleton.kept_out = measured.iter().filter_map(|m| m.3).collect();
            skeleton.no_trees = measured.iter().filter_map(|m| m.4).collect();
            // What of the rest the part closes in: what no column, not even the thinnest, gets
            // down to its footing from within half the contact gap of, round the part alone (what
            // the paint blocks is the paint's to tell; [`support_rules::ledger`] tells it so). Up
            // to the highest layer with any, as nothing over that changes the reach under it.
            let top = measured.iter().rposition(|m| !m.1.is_empty());
            let closed = match top {
                Some(t) => {
                    let m = (t + 1).min(n);
                    Some(support_rules::Legal::new(&outline[..m], &heights[..m], sp, &[], &[]).column_reach(par::Ctl { tick: None, ..ctl })?)
                }
                None => None,
            };
            let by = support_rules::CONTACT_GAP / 2.0 + sp.column();
            // Of what is left, what has its contact where a tree would be locked in, unless the
            // part lets its supports be: no tip went under it ([`tree_supports`]).
            let shut = |c: usize| locked.get(c).map(Vec::as_slice).filter(|x| !sp.stand_inside && !x.is_empty());
            let told = par::try_map(ctl, measured.len(), |d| {
                let Some(c) = legal.contact(d).filter(|_| !measured[d].1.is_empty()) else { return (skeleton::GivenUp::of(d, &measured[d].1), None, None) };
                let (bare, inside) = match &closed {
                    Some(reach) => split(measured[d].1.clone(), &reach.beyond(c, by)),
                    None => (measured[d].1.clone(), Vec::new()),
                };
                let (bare, locked_in) = split(bare, shut(c).unwrap_or(&[]));
                (skeleton::GivenUp::of(d, &specks(bare)), skeleton::GivenUp::of(d, &specks(inside)), skeleton::GivenUp::of(d, &specks(locked_in)))
            })?;
            skeleton.given_up = told.iter().filter_map(|t| t.0).collect();
            skeleton.inside = told.iter().filter_map(|t| t.1).collect();
            skeleton.locked = told.iter().filter_map(|t| t.2).collect();
        }
        // Where the part lets its supports be locked in, what of the trees is: told as that.
        if sp.stand_inside && locked.iter().any(|x| !x.is_empty()) {
            let layers = &self.layers;
            skeleton.unremovable = par::try_map(ctl, n, |l| {
                let (Some(x), Some(sl)) = (locked.get(l).filter(|x| !x.is_empty()), layers.get(l)) else { return None };
                if sl.base.is_empty() && sl.interface.is_empty() {
                    return None;
                }
                skeleton::GivenUp::of(l, &specks(intersection_ex(&[sl.base.as_slice(), sl.interface.as_slice()].concat(), x)))
            })?.into_iter().flatten().collect();
        }
        ctl.phase(TREE_STEPS[9].0, None, 1.0);
        Ok(skeleton)
    }
}

/// Whether the job's supports are trees ([`tree_supports`]).
pub(crate) fn plans_trees(job: &Job) -> bool {
    job.b("enable_support") && job.s("support_type").starts_with("tree") && job.s("support_type").contains("auto")
}

/// The setting that keeps a part's trees out of its footprint ([`footprint_below`]).
pub const OUTSIDE_FOOTPRINT: &str = "fab_tree_outside_footprint";

/// The part's setting that lists the tips of the trees the user deleted ([`dropped_text`]): mm,
/// in the plan's frame, z the print z of the tip's layer. A plate's placement writes it from its
/// support marks.
pub const DROPPED_TIPS: &str = "fab_tree_dropped";

/// The [`DROPPED_TIPS`] text of `tips`: `x y z` per tip, to 0.1 um, `;` between tips.
pub fn dropped_text(tips: &[[f64; 3]]) -> String {
    let mm = |v: f64| {
        let s = format!("{}", (v * 10_000.0).round() / 10_000.0);
        if s == "-0" { "0".to_string() } else { s }
    };
    tips.iter().map(|t| format!("{} {} {}", mm(t[0]), mm(t[1]), mm(t[2]))).collect::<Vec<_>>().join(";")
}

/// The tips a job's [`DROPPED_TIPS`] lists ([`dropped_in`]).
pub fn dropped_tips(job: &Job) -> Vec<[f64; 3]> {
    dropped_in(job.s(DROPPED_TIPS))
}

/// The tips a [`dropped_text`] lists; a tip that is not three numbers is no tip.
pub fn dropped_in(text: &str) -> Vec<[f64; 3]> {
    text.split(';').filter_map(|t| {
        let n: Vec<f64> = t.split_whitespace().filter_map(|v| v.parse().ok()).collect();
        (n.len() == 3).then(|| [n[0], n[1], n[2]])
    }).collect()
}

/// Per layer, in the plan's frame, where the part's trees may not go: every stage of the tree
/// takes it as the part on that layer ([`tree::Input::keep_out`]). It is the part's footprint
/// when the part keeps its trees out of it ([`OUTSIDE_FOOTPRINT`]: `footprint`, else none), with
/// the part's no-trees areas (`m.no_trees`, `no_trees` per layer) on top: each none past its end.
/// With no-trees areas, one per layer of the `n` the part has.
fn keep_out(ctl: par::Ctl, n: usize, footprint: &[Vec<ExPolygon>], no_trees: &[Vec<ExPolygon>]) -> Result<Vec<Vec<ExPolygon>>, par::Cancelled> {
    if no_trees.iter().all(Vec::is_empty) {
        return Ok(footprint.to_vec());
    }
    par::try_map(ctl, n, |l| {
        let (a, b) = (footprint.get(l).map_or(&[][..], Vec::as_slice), no_trees.get(l).map_or(&[][..], Vec::as_slice));
        match (a.is_empty(), b.is_empty()) {
            (_, true) => a.to_vec(),
            (true, false) => b.to_vec(),
            (false, false) => union_ex(&[a, b].concat()),
        }
    })
}

/// Per layer, the part's footprint so far: what it covers at that layer or any under it, holes
/// filled. It only grows going up, so nothing stands on it but at the part's top.
pub fn footprint_below(ctl: par::Ctl, outline: &[Vec<ExPolygon>]) -> Result<Vec<Vec<ExPolygon>>, par::Cancelled> {
    let filled = |set: Vec<ExPolygon>| -> Vec<ExPolygon> { set.into_iter().map(|e| ExPolygon::new(e.contour)).collect() };
    let mut below: Vec<ExPolygon> = Vec::new();
    let mut out = Vec::with_capacity(outline.len());
    for layer in outline {
        if ctl.cancelled() {
            return Err(par::Cancelled);
        }
        if !layer.is_empty() {
            below = filled(union_ex(&[below, filled(layer.clone())].concat()));
        }
        out.push(below.clone());
    }
    Ok(out)
}

/// The branch's diameter (mm): R14's, the reference's organic tree's and the panel's one key.
pub const TREE_BRANCH_DIAMETER: &str = "tree_support_branch_diameter_organic";
/// The most a branch leans (degrees): R14's, the rules' and coral's one key.
pub const TREE_BRANCH_ANGLE: &str = "tree_support_branch_angle_organic";

/// What a job tells R14 of a part, as [`tree_supports`] tells it ([`tree_told`]).
#[derive(Clone, Debug)]
pub struct TreeJob {
    pub keys: tree::settings::Keys,
    /// How the tree prints.
    pub print: tree::interface::Print,
    /// The part keeps its trees out of its footprint ([`OUTSIDE_FOOTPRINT`]).
    pub outside_footprint: bool,
    /// Its supports may stand where they'd be locked in the part; else R14 is told to keep out
    /// of what lies over a cell a tree would be locked in.
    pub stand_inside: bool,
}

/// What a job tells R14 of a part. None when the job asks for no tree.
pub fn tree_told(job: &Job) -> Option<TreeJob> {
    let p = Params::from_job(job, 0);
    let sp = p.support.as_ref().filter(|_| !p.coral)?;
    Some(TreeJob { keys: tree_keys(job, &p, sp), print: tree_print(&p, sp), outside_footprint: job.b(OUTSIDE_FOOTPRINT), stand_inside: sp.stand_inside })
}

/// R14's keys (§1.1) from the job: each key the reference reads, its own default where the job
/// has none (Fab has no setting for the tip diameter, the organic branch's line gap, the slow
/// angle, the top rate and the smallest roof); the support line, the outer wall, the side and top
/// gaps, the roof layers, the overhang angle and bed only as the support's settings read them.
/// Fewer than no floor layers are as many as the roof's, and fewer than no walls the automatic
/// choice, as none. The narrowest the walls print is their smallest feature, as their bead rules
/// read it.
fn tree_keys(job: &Job, p: &Params, sp: &support::SupportParams) -> tree::settings::Keys {
    let d = tree::settings::Keys::new(p.layer_height, sp.line_width, sp.wall_width);
    let or = |key: &str, default: f64| if job.cfg.has(key) { job.f(key) } else { default };
    let floors = job.cfg.i64_at("support_interface_bottom_layers", 0);
    tree::settings::Keys {
        tip_diameter: or("tree_support_tip_diameter", d.tip_diameter),
        branch_diameter: or(TREE_BRANCH_DIAMETER, d.branch_diameter),
        branch_diameter_angle_deg: or("tree_support_branch_diameter_angle", d.branch_diameter_angle_deg),
        branch_angle_deg: or(TREE_BRANCH_ANGLE, d.branch_angle_deg),
        angle_slow_deg: or("tree_support_angle_slow", d.angle_slow_deg),
        top_rate: or("tree_support_top_rate", d.top_rate),
        branch_distance: or("tree_support_branch_distance_organic", d.branch_distance),
        wall_count: job.cfg.i64_at("tree_support_wall_count", 0).max(0) as usize,
        xy_distance: sp.xy_gap,
        first_layer_gap: or("support_object_first_layer_gap", d.first_layer_gap),
        top_distance: sp.top_gap,
        bottom_distance: or("support_bottom_z_distance", d.bottom_distance),
        roof_layers: sp.interface_top_layers,
        floor_layers: if floors < 0 { sp.interface_top_layers } else { floors as usize },
        interface_spacing: or("support_interface_spacing", d.interface_spacing),
        interface_min_area: or("support_interface_min_area", d.interface_min_area),
        threshold_deg: sp.threshold_deg,
        bed_only: sp.build_plate_only,
        remove_small: if job.cfg.has("support_remove_small_overhang") { job.b("support_remove_small_overhang") } else { d.remove_small },
        first_layer_density: 100.0 * p.support_first_density,
        resolution: or("resolution", d.resolution),
        narrowest: p.arachne.as_ref().unwrap_or(&p.gap_fill).min_feature_size,
        ..d
    }
}

/// How R14's tree prints (§9.5): the support's speeds, and its first layer's line and speed. On a
/// raft the support's first layer is not the print's: it prints in the support's line and speed.
fn tree_print(p: &Params, sp: &support::SupportParams) -> tree::interface::Print {
    let on_bed = p.raft_layers == 0;
    tree::interface::Print {
        speed: p.support_speed,
        interface_speed: p.support_interface_speed,
        first_speed: if on_bed { p.speeds.first_layer } else { p.support_speed },
        first_line: if on_bed { p.w_first } else { sp.line_width },
    }
}

/// The bed in the part's frame, wherever on it the part stands. A part is planned once for every
/// place it can be put (it keeps its plan when it moves, and its copies share it), so the bed is
/// the box of the job's `printable_area` slid everywhere round the part's box `part` (in the
/// part's frame) that keeps the part on it: what is on the bed for some place the part fits. Cut
/// to it (R14 §8.3, §9.4), a tree that stays clear of the bed's edge is cut as the reference cuts
/// it; one that reaches past the edge where the part stands is not cut there. A job that plans
/// trees names its bed: [`check`] refuses one that does not.
fn bed_area(job: &Job, part: &BBox) -> Vec<ExPolygon> {
    assert!(job.cfg.points("printable_area").len() >= 3, "printable_area lists no bed for the trees to stand on; toolpath::check the job before planning");
    match bed_box(job, part) {
        Some(BBox { min: lo, max: hi }) => vec![ExPolygon::new(Polygon::new(vec![lo, Pt::new(hi.x, lo.y), hi, Pt::new(lo.x, hi.y)]))],
        None => Vec::new(),
    }
}

/// [`bed_area`]'s box, in the frame of the part's box `part`: none when the job names no bed or
/// the part fits on it nowhere.
fn bed_box(job: &Job, part: &BBox) -> Option<BBox> {
    let area = job.cfg.points("printable_area");
    if area.len() < 3 {
        return None;
    }
    let mut b = BBox::empty();
    area.into_iter().for_each(|(x, y)| b.add(Pt::from_mm(x, y)));
    let (w, d) = (b.max.x - b.min.x, b.max.y - b.min.y);
    let (lo, hi) = (Pt::new(part.max.x - w, part.max.y - d), Pt::new(part.min.x + w, part.min.y + d));
    (lo.x < hi.x && lo.y < hi.y).then_some(BBox { min: lo, max: hi })
}

/// Where a part's support paint can matter (mm, in the frame of its box `lo`..`hi`): on the bed
/// wherever the part stands on it ([`bed_box`]), as its trees do, or round the part when the job
/// names no bed (supports other than trees stand under it). Each paint region is cut to it before
/// it is read: a region drawn or typed metres across is read where supports can be, no further.
fn paint_reach(job: &Job, (lo, hi): ([f64; 3], [f64; 3])) -> ([f64; 2], [f64; 2]) {
    match bed_box(job, &BBox { min: Pt::from_mm(lo[0], lo[1]), max: Pt::from_mm(hi[0], hi[1]) }) {
        Some(b) => ([b.min.x_mm(), b.min.y_mm()], [b.max.x_mm(), b.max.y_mm()]),
        None => support_paint::round_the_part((lo, hi)),
    }
}

fn merge_supports(layers: &mut Vec<(f64, f64, ObjectLayer)>, supports: Vec<(usize, f64, f64, ObjectLayer)>, filament: usize, painted: bool) {
    // The base filament's model layers occur once each, in height order.
    let model: Vec<usize> = layers.iter().enumerate().filter(|(_, l)| l.2.filament == filament && !l.2.support_only).map(|(i, _)| i).collect();
    for (index, z, h, layer) in supports {
        if index == usize::MAX {
            layers.push((z, h, layer));
        } else {
            layers[model[index]].2.islands.extend(layer.islands);
        }
    }
    if painted {
        // Normal supports used to follow the base region, before other
        // filaments at equal z; preserve that order in the combined plan.
        layers.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap().then_with(|| (a.2.filament != filament).cmp(&(b.2.filament != filament))));
    }
}

/// What a plan printed of the trees on a layer, and what it printed that from: their paths, which
/// it holds so that no others come to their address, and how it placed them.
pub(crate) struct TreeIsland {
    paths: Arc<Vec<ExtrusionPath>>,
    from: TreeIslandFrom,
    island: Arc<Island>,
}

#[derive(PartialEq)]
struct TreeIslandFrom {
    at: Pt,
    height: f64,
    max_volumetric: f64,
    adaptive: bool,
}

/// Coral's centrelines print in their given widths, with the support speeds.
fn coral_paths(p: &Params, paths: &[coral::material::SupportPath], h: f64, first: bool) -> Island {
    let mut island = Island::default();
    for path in paths {
        if path.pts.len() < 2 { continue; }
        let role = match path.role {
            coral::material::Role::Pad | coral::material::Role::Body => Role::Support,
            coral::material::Role::Interface => Role::SupportInterface,
        };
        let w = if first { p.w_first } else { path.width };
        let area = flow::cross_section(w, h);
        let speed = if first { p.speeds.first_layer } else if role == Role::SupportInterface { p.support_interface_speed } else { p.support_speed };
        let speed = if p.speeds.max_volumetric > 0.0 { speed.min(p.speeds.max_volumetric / area) } else { speed };
        island.fills.push(ExtrusionPath { role, pts: path.pts.clone(), closed: false, loop_pts: Vec::new(), width: w, height: h, area, speed, dry: Vec::new(), z: Vec::new(), flow: Vec::new(), mode: 0, core: false, overhang: 0.0 });
    }
    island
}

/// The bridge direction that anchors the most line length on the layer
/// below (both ends resting on it). R1 §4.4 uses 5° candidates and outline
/// edges, breaking near-ties by the shortest longest span.
fn bridge_angle(region: &[ExPolygon], lower: &[ExPolygon], spacing: f64, center: Pt, fallback: f64) -> f64 {
    let booleans = prof::scope(prof::Stage::InfillBooleans);
    let anchors = offset_ex(lower, 0.5 * SCALE, Join::Miter(3.0));
    let grown = offset_ex(region, 0.6 * SCALE, Join::Miter(3.0));
    drop(booleans);
    // Anchored ends, looked up by the edges near them (the same answer as every anchor's own test).
    let anchor_grid = makepad_fab_slicer::grid::SegGrid::of_regions(&anchors, |k| k as u32);
    let anchored_at = |p: Pt| anchor_grid.contains_any(p);
    // With no two-ended anchors (a supported cap around a central stem),
    // output evidence favours an axis-aligned shortest span, Y on a tie.
    let mut angles = vec![std::f64::consts::FRAC_PI_2];
    angles.extend((0..36).map(|k| k as f64 * std::f64::consts::PI / 36.0));
    for e in region {
        for r in e.rings() {
            for i in 0..r.pts.len() {
                let a = r.pts[i];
                let b = r.pts[(i + 1) % r.pts.len()];
                if a != b {
                    angles.push(((b.y - a.y) as f64).atan2((b.x - a.x) as f64).rem_euclid(std::f64::consts::PI));
                }
            }
        }
    }
    let measure = |a: f64| {
        let segs = fill::family(&grown, a, spacing, center, fill::Align::Grid);
        let mut anchored = 0.0;
        let mut longest = 0.0f64;
        for sg in &segs {
            longest = longest.max(sg.a.dist(sg.b));
            if anchored_at(sg.a) && anchored_at(sg.b) {
                anchored += sg.a.dist(sg.b);
            }
        }
        (anchored, longest)
    };
    // Edges along one direction give the same angle again, which measures the same.
    let mut measured: std::collections::HashMap<u64, (f64, f64)> = std::collections::HashMap::new();
    let mut best = (fallback, -1.0f64, f64::INFINITY);
    for a in angles {
        let (anchored, longest) = *measured.entry(a.to_bits()).or_insert_with(|| measure(a));
        if anchored > best.1 * 1.0001 || ((anchored - best.1).abs() <= best.1.abs() * 0.0001 && longest < best.2 - 1.0) {
            best = (a, anchored, longest);
        }
    }
    best.0
}

/// Concentric solid fill, with the final pair of beads sharing the remaining
/// width. A core narrower than one-and-a-half spacings gets a single bead.
/// These transitions are inferred from the narrow rectangular output areas.
/// Each round at the spacing takes its rings and next cores from one inset of
/// `region` (insets do not compose exactly: one inset per depth is the accurate
/// one, and the GPU field's levels are the same).
pub fn concentric_solid(p: &Params, role: Role, region: &ExPolygon, h: f64, first: bool) -> Vec<ExtrusionPath> {
    bead_paths(p, role, &concentric_beads(p, region, h, first), h, first)
}

/// A holed core's thickness steps (mm): its thickness is the deepest multiple of the step whose
/// half inset leaves anything.
pub const THICKNESS_STEP: f64 = 0.005;
/// A holed core's thickness (mm, the most k THICKNESS_STEP up to `short` whose half inset
/// leaves anything) and the insets it took. A thickness of at least `enough` (mm) is all the
/// caller tells apart: it comes back as the first step at or past `enough`, which one inset
/// decides. Below that, the search starts at the core's mean width (twice its area over its
/// perimeter: an even band's thickness), gallops to the steps either side of the answer and
/// bisects between them. Deeper insets leave less, so the order the steps are tried in does
/// not change the answer; trying them near it skips the insets far past it, which are also the
/// dearest.
pub fn core_thickness(core: &ExPolygon, short: f64, enough: f64) -> (f64, usize) {
    // lo leaves something (the core itself), hi nothing (past `short`)
    let top = (short / THICKNESS_STEP).floor() as i64;
    let (mut lo, mut hi) = (0i64, top + 1);
    let insets = std::cell::Cell::new(0);
    let empty = |k: i64| {
        insets.set(insets.get() + 1);
        offset_ex(std::slice::from_ref(core), -(k as f64 * THICKNESS_STEP / 2.0) * SCALE, DEFAULT_MITER).is_empty()
    };
    // the first step at or past `enough` (past `top`: none to try)
    let mut wide = ((enough / THICKNESS_STEP).ceil() as i64).clamp(0, top + 1);
    while wide > 0 && (wide - 1) as f64 * THICKNESS_STEP >= enough { wide -= 1; }
    while wide <= top && (wide as f64) * THICKNESS_STEP < enough { wide += 1; }
    if wide < hi {
        if !empty(wide) { return (wide as f64 * THICKNESS_STEP, insets.get()); }
        hi = wide;
    }
    let perimeter = core.rings().map(Polygon::perimeter).sum::<f64>() / SCALE;
    if hi - lo > 1 && perimeter > 0.0 {
        let k = ((2.0 * core.area_mm2() / perimeter / THICKNESS_STEP).round() as i64).clamp(lo + 1, hi - 1);
        let mut g = 1;
        if empty(k) {
            hi = k;
            while hi - lo > 1 {
                let k = (hi - g).max(lo + 1);
                if empty(k) { hi = k } else { lo = k; break }
                g *= 2;
            }
        } else {
            lo = k;
            while hi - lo > 1 {
                let k = (lo + g).min(hi - 1);
                if empty(k) { hi = k; break }
                lo = k;
                g *= 2;
            }
        }
    }
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if empty(mid) { hi = mid } else { lo = mid }
    }
    (lo as f64 * THICKNESS_STEP, insets.get())
}

/// The paths of beads (a region's [`concentric_beads`], or a GPU's of it) in role `role`.
pub fn bead_paths(p: &Params, role: Role, beads: &[given::Bead], h: f64, first: bool) -> Vec<ExtrusionPath> {
    beads.iter().map(|b| match b {
        given::Bead::Loop { pts, seam, width } => {
            let area = flow::cross_section(*width, h);
            let mut ep = walls::loop_path(pts, *seam, role, *width, p.seam_gap);
            ep.height = h;
            ep.area = area;
            ep.speed = p.speed(role, area, first);
            ep
        }
        given::Bead::Line { pts, width } => {
            let area = flow::cross_section(*width, h);
            ExtrusionPath { role, pts: pts.clone(), closed: false, loop_pts: Vec::new(), width: *width, height: h, area, speed: p.speed(role, area, first), dry: Vec::new(), z: Vec::new(), flow: Vec::new(), mode: 0, core: false, overhang: 0.0 }
        }
    }).collect()
}

/// The beads of [`concentric_solid`]: where its paths run and how wide, in order.
pub fn concentric_beads(p: &Params, region: &ExPolygon, h: f64, first: bool) -> Vec<given::Bead> {
    concentric_rounds(p, region, vec![(region.clone(), 0.0)], h, first)
}

/// How much narrower than the last pair of a concentric fill a place may be and still hold it (mm,
/// per side).
const LAST_PAIR_SLACK: f64 = 0.01;

/// The rounds over pending cores (each with its depth in `region`, mm), as beads.
fn concentric_rounds(p: &Params, region: &ExPolygon, mut pending: Vec<(ExPolygon, f64)>, h: f64, first: bool) -> Vec<given::Bead> {
    let spacing = flow::spacing(if first { p.w_first } else { p.w_solid }, h);
    // region's insets by depth (nm), each computed once
    let mut insets: Vec<(i64, Vec<ExPolygon>)> = Vec::new();
    let mut inset_within = |e: &ExPolygon, depth: f64| -> Vec<ExPolygon> {
        let key = (depth * SCALE).round() as i64;
        let at = insets.iter().position(|i| i.0 == key).unwrap_or_else(|| {
            insets.push((key, offset_ex(std::slice::from_ref(region), -depth * SCALE, DEFAULT_MITER)));
            insets.len() - 1
        });
        insets[at].1.iter().filter(|c| c.contour.pts.first().is_some_and(|&q| e.contains(q))).cloned().collect()
    };
    let mut out = Vec::new();
    while let Some((e, depth)) = pending.pop() {
        let src = std::slice::from_ref(&e);
        // The region's extent across each edge is reached at corners of its
        // hull: measured over those, not over every point once per edge.
        let hull = convex_hull(&mut e.rings().flat_map(|r| r.pts.iter().copied()).collect());
        let mut short = (f64::INFINITY, 0.0);
        for i in 0..e.contour.pts.len() {
            let a = e.contour.pts[i];
            let b = e.contour.pts[(i + 1) % e.contour.pts.len()];
            if a == b { continue; }
            let angle = ((b.y - a.y) as f64).atan2((b.x - a.x) as f64);
            let (lo, hi) = fill::extent_of(&hull, angle);
            let width = unscaled(hi - lo);
            if width < short.0 { short = (width, angle); }
        }
        if short.0 <= 0.0 || !short.0.is_finite() { continue; }
        // A region with holes (a band round a hole) is as wide as its
        // thickness, not its extent: measured by the deepest inset that
        // leaves anything, so a band one bead wide gets one bead, never two
        // loops on top of each other.
        if !e.holes.is_empty() { short.0 = short.0.min(core_thickness(&e, short.0, 2.5 * spacing).0); }
        let single = short.0 < 1.5 * spacing;
        let s = if single { short.0 } else if short.0 < 2.5 * spacing { short.0 / 2.0 } else { spacing };
        let w = flow::width_for_spacing(s, h);
        if single && !e.holes.is_empty() {
            // One loop along the band's middle: the outline of what an inset
            // to just short of half its thickness leaves. Where the band is
            // thinner than at its thickest, that inset breaks it open, and the
            // outline of what is left runs out along the band and back over
            // itself (one bead laid twice). The inset goes only as deep as
            // leaves the band whole round its holes; the bead is as wide as
            // the band is where it is thinnest.
            let inset = |d: f64| offset_ex(src, -d * SCALE, DEFAULT_MITER);
            let whole = |c: &[ExPolygon]| c.len() == 1 && c[0].holes.len() == e.holes.len();
            let deep = (s / 2.0 - 0.005).max(0.0);
            let mut cores = inset(deep);
            let mut w = w;
            if !whole(&cores) {
                let (mut lo, mut hi) = ((0.0, vec![e.clone()]), deep);
                while hi - lo.0 > 0.0025 {
                    let mid = (lo.0 + hi) / 2.0;
                    let c = inset(mid);
                    if whole(&c) { lo = (mid, c) } else { hi = mid }
                }
                w = flow::width_for_spacing(2.0 * lo.0 + 0.01, h);
                cores = lo.1;
            }
            for core in cores {
                let mut pts = core.contour.pts.clone();
                if pts.len() < 3 { continue; }
                let k = (0..pts.len()).max_by_key(|&i| pts[i].dist2(pts[(i + 1) % pts.len()])).unwrap();
                let a = pts[k];
                let b = pts[(k + 1) % pts.len()];
                pts.insert(k + 1, Pt::new((a.x + b.x) / 2, (a.y + b.y) / 2));
                out.push(given::Bead::Loop { pts, seam: k + 1, width: w });
            }
            continue;
        }
        if single {
            let bb = e.bbox();
            let center = Pt::new((bb.min.x + bb.max.x) / 2, (bb.min.y + bb.max.y) / 2);
            for seg in fill::family(src, short.1, short.0 * 2.0, center, fill::Align::CenteredInset) {
                let mut pl = Polyline::new(vec![seg.a, seg.b]);
                pl.clip_end(s / 2.0 * SCALE);
                pl.pts.reverse();
                pl.clip_end((s / 2.0 + p.seam_gap) * SCALE);
                if pl.pts.len() >= 2 {
                    out.push(given::Bead::Line { pts: pl.pts, width: w });
                }
            }
            continue;
        }
        // A round at the spacing takes one inset of the region; the last pair of beads, whose
        // spacing is the core's own width, insets the core itself, so it closes exactly.
        let wide = short.0 >= 2.5 * spacing;
        let inset = |insets: &mut dyn FnMut(&ExPolygon, f64) -> Vec<ExPolygon>, d: f64| {
            if wide { insets(&e, depth + d) } else { offset_ex(src, -d * SCALE, DEFAULT_MITER) }
        };
        // The rings run half a spacing in, and only where the place holds their two sides that far
        // apart: half a spacing out from the next core. In a narrower place they would lay a bead on
        // top of itself; it gets one line from the piece's `left_beads`. The last pair's own core has
        // no width, so that round looks just short of it.
        let slack = if wide { 0.0 } else { LAST_PAIR_SLACK };
        let held = inset(&mut inset_within, s - slack);
        for path in walls::separate(offset_ex(&held, (s / 2.0 - slack) * SCALE, DEFAULT_MITER), s * SCALE) {
            for ring in path.rings() {
                let mut pts = ring.pts.clone();
                if pts.len() < 3 { continue; }
                let k = (0..pts.len()).max_by_key(|&i| pts[i].dist2(pts[(i + 1) % pts.len()])).unwrap();
                let a = pts[k];
                let b = pts[(k + 1) % pts.len()];
                pts.insert(k + 1, Pt::new((a.x + b.x) / 2, (a.y + b.y) / 2));
                out.push(given::Bead::Loop { pts, seam: k + 1, width: w });
            }
        }
        pending.extend(inset(&mut inset_within, s).into_iter().map(|c| (c, depth + s)));
    }
    out
}

/// One loop along each floating-shell band: in the band's middle, its
/// convex corners rounded, the bead as wide as the band carries.
fn floating_shell(p: &Params, band: &[ExPolygon], gaps: &[ExtrusionPath], h: f64, first: bool) -> Vec<ExtrusionPath> {
    let s = flow::spacing(p.w_solid, h);
    let overlap = p.infill_overlap * flow::spacing((p.w_outer + p.w_inner) / 2.0, h).max(0.0);
    let b = (s - overlap).max(0.05);
    let w = 0.945 * s + h * (1.0 - std::f64::consts::PI / 4.0);
    let area = flow::cross_section(w, h);
    let mut out = Vec::new();
    for e in band {
        if e.holes.is_empty() {
            let beads = concentric_solid(p, Role::FloatingVerticalShell, e, h, first);
            out.extend(left_beads(p, Role::FloatingVerticalShell, e, &beads, gaps, h, first));
            out.extend(beads);
            continue;
        }
        let from = out.len();
        let outer = vec![ExPolygon::new(e.contour.clone())];
        let shrunk = offset_ex(&outer, -b * SCALE, DEFAULT_MITER);
        let path = offset_ex(&shrunk, b / 2.0 * SCALE, Join::Round(0.02 * SCALE));
        for pe in path {
            let ring = &pe.contour.pts;
            if ring.len() < 3 {
                continue;
            }
            // Start on the front-right corner.
            let bb = BBox::of(ring);
            let target = Pt::new(bb.max.x, bb.min.y);
            let k = (0..ring.len()).min_by_key(|&i| ring[i].dist2(target)).unwrap_or(0);
            let mut ep = walls::loop_path(ring, k, Role::FloatingVerticalShell, w, p.seam_gap);
            ep.height = h;
            ep.area = area;
            ep.speed = p.speed(Role::FloatingVerticalShell, area, first);
            out.push(ep);
        }
        let left = left_beads(p, Role::FloatingVerticalShell, e, &out[from..], gaps, h, first);
        out.extend(left);
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn infill(p: &Params, role: Role, region: &[ExPolygon], island: &[ExPolygon], li: usize, h: f64, solid_angle: f64, center: Pt, first: bool, z: f64, lower: Option<&Vec<ExPolygon>>, bridge_loop: bool, aux_start: usize, aux: &infill::Aux, sparse: infill::SparseFill, cores: &[ExPolygon]) -> Vec<ExtrusionPath> {
    let mut paths = Vec::new();
    // R1 §4.4: external bridges follow thick_bridges. The reference output
    // keeps internal bridges round even when external bridges are thin.
    let round_bridge = role == Role::Bridge && (lower.is_none() || p.thick_bridges);
    use pattern::Surface;
    // The surface pattern of solid roles (sparse is dispatched below).
    let (w, surf) = match role {
        Role::TopSurface => (p.w_top, p.top_pattern),
        Role::BottomSurface => (if first { p.w_first } else { p.w_solid }, p.bottom_pattern),
        Role::SparseInfill => (if first { p.w_first } else { p.w_sparse }, Surface::Rectilinear),
        Role::Bridge => (if round_bridge { p.nozzle * p.bridge_flow.sqrt() } else { p.w_solid }, Surface::Rectilinear),
        _ => (if first { p.w_first } else { p.w_solid }, p.solid_pattern),
    };
    // Bridge threads sit 0.05 mm apart.
    let s = if round_bridge { w + 0.05 } else { flow::spacing(w, h) };
    // Line centres sit half a spacing inside the walls, minus the overlap
    // (a share of the outer-to-inner wall spacing).
    let overlap = if role == Role::TopSurface && surf.monotonic() && p.travel_into_wall > 0.0 {
        // Monotonic top lines reach into the wall by the run-on share.
        p.travel_into_wall * s
    } else {
        p.infill_overlap * flow::spacing(if first { p.w_first } else { (p.w_outer + p.w_inner) / 2.0 }, h).max(0.0)
    };
    let adjacent = { let _t = prof::scope(prof::Stage::InfillBooleans); difference_ex(island, region) };
    // A region that prints solid (`cores`, `m.settings`) is a wall to the sparse round it: its
    // lines run into the region's beads by the wall overlap, as into a wall's, and join along its
    // edge.
    let adjacent = if role == Role::SparseInfill && !cores.is_empty() && !adjacent.is_empty() { let _t = prof::scope(prof::Stage::InfillBooleans); difference_ex(&adjacent, cores) } else { adjacent };
    let partial = !adjacent.is_empty();
    let line_region = |s: f64| {
        let _t = prof::scope(prof::Stage::InfillBooleans);
        let expanded = offset_ex(region, -(s / 2.0 - overlap) * SCALE, Join::Miter(3.0));
        if !partial { return expanded; }
        // Overlap bonds to walls, not to a neighbouring fill class. In the
        // reference, adjacent solid and sparse centres remain half of each
        // bead spacing from their shared boundary.
        difference_ex(&expanded, &offset_ex(&adjacent, s / 2.0 * SCALE, Join::Miter(3.0)))
    };
    let lines_region = line_region(s);
    if lines_region.is_empty() {
        return paths;
    }
    let (area, width, height) = if round_bridge { (flow::bridge_section(w), w, w) } else { (flow::cross_section(w, h), w, h) };
    let area = if role == Role::Bridge && !round_bridge { area * p.bridge_flow } else { area };
    let mkw = |pl: Polyline, width: f64, area: f64| -> ExtrusionPath { ExtrusionPath { role, pts: pl.pts, closed: false, loop_pts: Vec::new(), width, height, area, speed: p.speed(role, area, first), dry: Vec::new(), z: Vec::new(), flow: Vec::new(), mode: 0, core: false, overhang: 0.0 } };
    let mk = |pl: Polyline| mkw(pl, width, area);
    match role {
        Role::SparseInfill => {
            // The setting's reach is a share of line width (R1 §4.3).
            // Reference free anchors use the effective bead spacing:
            // 400% gives 1.628 mm at width 0.45 and height 0.2.
            let max = p.anchor_max.resolve(w).max(0.0);
            let anchor = p.anchor.resolve(s).clamp(0.0, max);
            let reach = 2.0 * p.anchor.resolve(w).clamp(0.0, max);
            let inp = infill::SparseIn {
                region: &lines_region,
                width: w,
                spacing: s,
                density: sparse.density,
                ctx: patterns::Ctx { li: li - aux_start, z, pivot: center, dir: p.infill_direction },
                anchor,
                max,
                reach,
                bridge_loop,
                height: h,
            };
            for st in infill::sparse(sparse.pattern, &p.sparse_opts, aux, &inp) {
                paths.push(stroke_path(p, role, st, h, first));
            }
        }
        Role::TopSurface | Role::BottomSurface | Role::InternalSolidInfill if p.solid_on_grid => {
            // on the part's grid: through its middle, a spacing apart, on every solid layer
            let connect = fill::Connect::Boundary { max: s * 3.0, anchor: 0.0 };
            let angle = if surf == Surface::AlignedRectilinear { p.infill_direction } else { solid_angle };
            paths.extend(fill::lines(&lines_region, angle, s, center, fill::Align::Grid, connect).into_iter().filter(|pl| pl.pts.len() >= 2).map(mk));
        }
        _ => {
            if role != Role::Bridge {
                let ctx = patterns::Ctx { li, z, pivot: center, dir: p.infill_direction };
                if let Some(strokes) = infill::surface(surf, &lines_region, s, w, &ctx) {
                    paths.extend(strokes.into_iter().map(|st| stroke_path(p, role, st, h, first)));
                    return paths;
                }
            }
            let connect = if role == Role::TopSurface && surf.monotonic() { fill::Connect::None } else { fill::Connect::Boundary { max: s * 3.0, anchor: 0.0 } };
            let angle = match (role, lower) {
                (Role::Bridge, Some(_)) if p.bridge_direction != 0.0 => p.bridge_direction,
                (Role::Bridge, Some(lower)) => bridge_angle(region, lower, w, center, solid_angle + std::f64::consts::FRAC_PI_2),
                // An internal bridge crosses the sparse infill's direction.
                (Role::Bridge, None) => {
                    let mut bb = BBox::empty();
                    for e in region { bb.merge(&e.bbox()); }
                    let dx = bb.max.x - bb.min.x;
                    let dy = bb.max.y - bb.min.y;
                    // Grid has two directions. Output on elongated internal
                    // bridges spans the short axis; square regions retain
                    // the configured diagonal used by the sparse grid.
                    if p.sparse_pattern == pattern::Sparse::Grid && (dx - dy).abs() > scaled(0.001) {
                        if dx < dy { 0.0 } else { std::f64::consts::FRAC_PI_2 }
                    } else { p.infill_direction + std::f64::consts::FRAC_PI_2 }
                },
                // Aligned solids keep the infill direction on every layer.
                _ if surf == Surface::AlignedRectilinear => p.infill_direction,
                _ => solid_angle,
            };
            if role == Role::TopSurface && connect == fill::Connect::None && p.travel_into_wall > 0.0 {
                // Monotonic top lines joined by dry moves into the wall.
                let segs = fill::family(&lines_region, angle, s, center, fill::Align::CenteredInset);
                for (pl, dry) in fill::monotonic_dry(&segs, p.travel_into_wall * w, 3.0 * s) {
                    let mut ep = mk(pl);
                    ep.dry = dry;
                    paths.push(ep);
                }
            } else if role == Role::TopSurface {
                let polys = fill::lines(&lines_region, angle, s, center, fill::Align::Centered, connect);
                paths.extend(polys.into_iter().filter(|pl| pl.pts.len() >= 2).map(mk));
            } else {
                // Solid layers stretch the spacing to fit the region, and the
                // bead widens to match it.
                let edges = role == Role::Bridge && angle.sin().abs().min(angle.cos().abs()) < 1e-6;
                let sf = if edges {
                    let (lo, hi) = fill::extent(region, angle);
                    let span = unscaled(hi - lo) + 2.0 * overlap;
                    span / (span / s).floor().max(1.0)
                } else if round_bridge {
                    let (lo, hi) = fill::extent(&lines_region, angle);
                    let span = unscaled(hi - lo);
                    // A diagonal outline within 1% of a spacing of a whole
                    // line count must not lose a complete bridge thread to
                    // coordinate rounding (inferred from the cap output).
                    let n = (span / s - 0.6 + 0.01).floor().max(1.0);
                    (span / (n + 0.6)).max(s)
                } else { fill::fit_spacing(&lines_region, angle, s) };
                let (wf, af) = if round_bridge {
                    let wf = sf - 0.05;
                    (wf, flow::bridge_section(wf))
                } else {
                    let wf = flow::width_for_spacing(sf, h);
                    (wf, flow::cross_section(wf, h) * if role == Role::Bridge { p.bridge_flow } else { 1.0 })
                };
                let fitted_region = if edges { line_region(sf) } else { lines_region.clone() };
                let align = if edges { fill::Align::FitEdges } else { fill::Align::Fit { reverse_first: !surf.monotonic() } };
                let polys = fill::lines(&fitted_region, angle, sf, center, align, connect);
                // The chain's free ends run on past the boundary by 0.63
                // spacings (as the reference's solid layers do).
                let ext = 0.63 * sf * SCALE;
                let stretch = |a: Pt, b: Pt| -> Pt {
                    let d = a.dist(b);
                    if d <= 0.0 {
                        return a;
                    }
                    let k = ext / d;
                    Pt::new(a.x + ((a.x - b.x) as f64 * k).round() as i64, a.y + ((a.y - b.y) as f64 * k).round() as i64)
                };
                paths.extend(polys.into_iter().filter(|pl| pl.pts.len() >= 2).map(|mut pl| {
                    if matches!(connect, fill::Connect::Boundary { .. }) {
                        let n = pl.pts.len();
                        let (a, b) = (pl.pts[0], pl.pts[1]);
                        pl.pts[0] = stretch(a, b);
                        let (a, b) = (pl.pts[n - 1], pl.pts[n - 2]);
                        pl.pts[n - 1] = stretch(a, b);
                    }
                    mkw(pl, wf, af)
                }));
            }
        }
    }
    paths
}

/// An extrusion path from a fill stroke: closed strokes start at their
/// rear-right point and stop a seam gap short of it.
fn stroke_path(p: &Params, role: Role, st: infill::Stroke, h: f64, first: bool) -> ExtrusionPath {
    let area = flow::cross_section(st.width, h);
    let speed = p.speed(role, area, first);
    if st.closed && st.pts.len() >= 3 {
        let k = (0..st.pts.len()).max_by_key(|&i| (st.pts[i].x + st.pts[i].y, st.pts[i].x)).unwrap_or(0);
        let mut ep = walls::loop_path(&st.pts, k, role, st.width, p.seam_gap);
        ep.height = h;
        ep.area = area;
        ep.speed = speed;
        return ep;
    }
    ExtrusionPath { role, pts: st.pts, closed: false, loop_pts: Vec::new(), width: st.width, height: h, area, speed, dry: Vec::new(), z: Vec::new(), flow: Vec::new(), mode: 0, core: false, overhang: 0.0 }
}

#[cfg(test)]
mod np_tests;
#[cfg(test)]
mod pinch_tests;
#[cfg(test)]
mod sliver_tests;
#[cfg(test)]
mod plate_tests;
#[cfg(test)]
mod given_tests;

/// The validator's placeholder head (left nozzle active) as non-planar
/// planner boxes, for tests of the non-planar hooks.
#[doc(hidden)]
pub fn np_test_head() -> Vec<makepad_fab_slicer::nonplanar::NpBox> {
    let b = |name: &str, ox: f64, hx: f64, hy: f64, hole: f64, e: f64, margin: f64| makepad_fab_slicer::nonplanar::NpBox { name: name.into(), ox, oy: 0.0, hx, hy, hole, e, margin };
    let rise = 1.0 / 40f64.to_radians().tan();
    let edges = [1.1, 1.4, 1.8, 2.3, 2.9, 3.6, 4.2, 4.8];
    let margins = [0.15, 0.2, 0.25, 0.3, 0.4, 0.5, 0.5];
    let mut v: Vec<makepad_fab_slicer::nonplanar::NpBox> = edges.windows(2).enumerate().map(|(i, w)| b(&format!("nozzle cone {}", i + 1), 0.0, w[1], w[1], w[0], (((w[0] - 0.5) * rise).min(4.0) - 0.02).max(margins[i]), margins[i])).collect();
    v.extend([
        b("heater block", 0.0, 12.0, 14.0, 0.0, 4.0, 1.0),
        b("cooling duct", 0.0, 20.0, 20.0, 0.0, 6.0, 1.0),
        b("head body", 12.5, 47.5, 40.0, 0.0, 12.0, 1.5),
        b("other nozzle", 25.0, 4.5, 4.5, 0.0, 4.5, 1.0),
        b("other heater block", 25.0, 12.0, 14.0, 0.0, 8.5, 1.0),
    ]);
    v
}
