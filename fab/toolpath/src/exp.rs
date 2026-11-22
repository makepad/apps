//! Experimental slicing modes (EXPSLICE): the settings each pass reads,
//! resolved once per object from the job. Every mode is off by default and
//! an object with every mode off plans exactly as before.

use makepad_fab_settings::machine::Job;

/// Which mode made a path (`ExtrusionPath.mode`, carried into the G-code as
/// `; FAB_MODE:` and into `MoveRecord.mode`). `gcode::record::modes` re-exports these.
pub mod mode {
    pub const NONE: u8 = 0;
    pub const TEXTURE: u8 = 1;
    pub const VASE: u8 = 2;
    pub const SCARF: u8 = 3;
    pub const ADAPTIVE: u8 = 4;
    pub const IRONING: u8 = 5;
    pub const INTERLOCK: u8 = 6;
    pub const ARC_FRONT: u8 = 7;
    pub const NONPLANAR: u8 = 8;
    pub const COUNT: u8 = 9;
}

/// Outer-wall texture pattern (applied to the toolpath, never to the model).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pattern {
    /// Coherent 3D noise: neighbouring layers move together.
    Fuzzy,
    /// Diamond knurl from two opposite helices.
    Knurl,
    /// Overlapping fish scales in staggered rows.
    Scales,
    /// Basket weave: alternating over/under strands.
    Woven,
    /// Meandering vertical grain lines.
    WoodGrain,
}

impl Pattern {
    pub fn parse(s: &str) -> Option<Pattern> {
        match s.trim() {
            "fuzzy" => Some(Pattern::Fuzzy),
            "knurl" => Some(Pattern::Knurl),
            "scales" => Some(Pattern::Scales),
            "woven" => Some(Pattern::Woven),
            "wood_grain" | "woodgrain" | "grain" => Some(Pattern::WoodGrain),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Pattern::Fuzzy => "fuzzy",
            Pattern::Knurl => "knurl",
            Pattern::Scales => "scales",
            Pattern::Woven => "woven",
            Pattern::WoodGrain => "wood_grain",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Texture {
    pub pattern: Pattern,
    /// Peak-to-valley depth (mm); the texture only cuts inward from the nominal surface.
    pub depth: f64,
    /// Feature size (mm): one knurl diamond, scale or weave cell, grain spacing, noise cell.
    pub scale: f64,
    /// Resample step along the wall (mm).
    pub step: f64,
    /// Texture hole walls too (off: bores keep their size).
    pub holes: bool,
    /// Machine XY acceleration limit (mm/s²): caps the wall speed so the wiggle is not smeared.
    pub max_accel: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VaseVariant {
    Plain,
    /// Two walls per revolution (outer, then inner), one continuous climb.
    DoubleWall,
    /// The bead width swells into vertical ribs; the outer surface stays exact.
    VariableWidth,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Vase {
    pub variant: VaseVariant,
    /// Widest bead of the variable-width variant (mm).
    pub width_max: f64,
    /// Rib spacing of the variable-width variant (mm, around the part).
    pub rib_spacing: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Scarf {
    /// Outer walls only (false: inner walls too).
    pub outer_only: bool,
    /// Ramp length (mm).
    pub length: f64,
    /// Where the ramp starts, as a share of the layer height above the layer below (0..1).
    pub start: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Adaptive {
    /// Largest stair-step allowed on the surface (mm).
    pub cusp: f64,
    /// Layer height bounds (mm), from the machine's min/max layer height.
    pub min: f64,
    pub max: f64,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Modes {
    pub texture: Option<Texture>,
    pub vase: Option<Vase>,
    pub scarf: Option<Scarf>,
    pub adaptive: Option<Adaptive>,
    pub ironing: Option<Ironing>,
    pub interlock: Option<Interlock>,
    pub arc: Option<ArcOverhang>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArcOverhang {
    /// Front speed (mm/s).
    pub speed: f64,
    /// Overhang the walls print unaided, as a share of the bead width: only
    /// what hangs out further gets fronts.
    pub allow: f64,
    /// Smallest overhang worth fronts (mm²).
    pub min_area: f64,
    /// Front spacing as a share of the bead spacing (< 1: fronts press into each other).
    pub spacing: f64,
}

fn arc_from_job(job: &Job) -> Option<ArcOverhang> {
    if !job.b("fab_arc_overhangs") {
        return None;
    }
    let speed = { let s = job.f("fab_arc_overhang_speed"); if s > 0.0 { s } else { 5.0 } }.clamp(1.0, 30.0);
    Some(ArcOverhang { speed, allow: 0.6, min_area: 3.0, spacing: 0.9 })
}

#[derive(Clone, Debug, PartialEq)]
pub struct Interlock {
    /// Beam width (mm).
    pub beam_width: f64,
    /// How many beams deep the band reaches into each side.
    pub depth: usize,
    /// Layers per beam direction before it turns 90°.
    pub layers_per_direction: usize,
    /// First beam direction (radians).
    pub orientation: f64,
}

fn interlock_from_job(job: &Job) -> Option<Interlock> {
    if !job.b("interlocking_beam") {
        return None;
    }
    let beam_width = { let w = job.f("interlocking_beam_width"); if w > 0.0 { w } else { 0.8 } }.clamp(0.3, 5.0);
    let depth = job.cfg.i64_at("interlocking_depth", 0).clamp(1, 10) as usize;
    let layers_per_direction = job.cfg.i64_at("interlocking_beam_layer_count", 0).clamp(1, 20) as usize;
    let orientation = job.f("interlocking_orientation").to_radians();
    Some(Interlock { beam_width, depth, layers_per_direction, orientation })
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ironing {
    /// Only the object's topmost layer.
    pub only_topmost: bool,
    /// Every solid surface, not only tops.
    pub solid: bool,
    /// Share of a normal bead's flow (0..1).
    pub flow: f64,
    /// Line spacing (mm).
    pub spacing: f64,
    pub speed: f64,
    /// Extra inset from the surface's edge (mm).
    pub inset: f64,
    /// Line direction (radians).
    pub angle: f64,
}

fn ironing_from_job(job: &Job) -> Option<Ironing> {
    let (only_topmost, solid) = match job.s("ironing_type").trim() {
        "top" => (false, false),
        "topmost" => (true, false),
        "solid" => (false, true),
        _ => return None,
    };
    let raw = job.cfg.raw("ironing_flow", 0);
    let v = job.f("ironing_flow");
    let flow = if v > 0.0 { if raw.trim().ends_with('%') || v > 1.0 { v / 100.0 } else { v } } else { 0.1 };
    let spacing = { let s = job.f("ironing_spacing"); if s > 0.0 { s } else { 0.1 } }.clamp(0.03, 1.0);
    let speed = { let s = job.f("ironing_speed"); if s > 0.0 { s } else { 20.0 } }.clamp(2.0, 300.0);
    let angle = job.f("ironing_direction").to_radians();
    Some(Ironing { only_topmost, solid, flow: flow.clamp(0.0, 0.5), spacing, speed, inset: job.f("ironing_inset").max(0.0), angle })
}

impl Modes {
    pub fn from_job(job: &Job) -> Modes {
        Modes { texture: texture_from_job(job), vase: vase_from_job(job), scarf: scarf_from_job(job), adaptive: adaptive_from_job(job), ironing: ironing_from_job(job), interlock: interlock_from_job(job), arc: arc_from_job(job) }
    }
    pub fn any(&self) -> bool {
        self.texture.is_some() || self.vase.is_some() || self.scarf.is_some() || self.adaptive.is_some() || self.ironing.is_some() || self.interlock.is_some() || self.arc.is_some()
    }
}

fn adaptive_from_job(job: &Job) -> Option<Adaptive> {
    if !job.b("fab_adaptive_layers") {
        return None;
    }
    // Unset: the stair-step the nominal layer height leaves on a 45° slope,
    // so steep walls print faster and gentle slopes finer than uniform layers.
    let lh = { let h = job.f("layer_height"); if h > 0.0 { h } else { 0.2 } };
    let cusp = { let c = job.f("fab_adaptive_cusp"); if c > 0.0 { c } else { lh * std::f64::consts::FRAC_1_SQRT_2 } }.clamp(0.005, 0.3);
    // Bounds per nozzle are set by Params (they need the extruder).
    Some(Adaptive { cusp, min: 0.08, max: 0.28 })
}

fn vase_from_job(job: &Job) -> Option<Vase> {
    if !job.b("spiral_mode") {
        return None;
    }
    let variant = match job.s("fab_vase_variant").trim() {
        "double_wall" => VaseVariant::DoubleWall,
        "variable_width" => VaseVariant::VariableWidth,
        _ => VaseVariant::Plain,
    };
    let width_max = { let w = job.f("fab_vase_width_max"); if w > 0.0 { w } else { 0.8 } }.clamp(0.2, 2.0);
    let rib_spacing = { let s = job.f("fab_vase_rib_spacing"); if s > 0.0 { s } else { 6.0 } }.clamp(1.0, 100.0);
    Some(Vase { variant, width_max, rib_spacing })
}

fn scarf_from_job(job: &Job) -> Option<Scarf> {
    let outer_only = match job.s("seam_slope_type").trim() {
        "external" => true,
        "all" => false,
        _ => return None,
    };
    let length = { let l = job.f("seam_slope_min_length"); if l > 0.0 { l } else { 10.0 } }.clamp(4.0, 60.0);
    let raw = job.cfg.raw("seam_slope_start_height", 0);
    let v = job.f("seam_slope_start_height");
    // A share of the layer height ("10%"), or mm (taken against 0.2 mm layers).
    let start = if raw.trim().ends_with('%') { v / 100.0 } else if v > 0.0 { v / 0.2 } else { 0.1 };
    Some(Scarf { outer_only, length, start: start.clamp(0.0, 0.9) })
}

fn texture_from_job(job: &Job) -> Option<Texture> {
    let own = Pattern::parse(&job.s("fab_wall_texture"));
    // An imported project's fuzzy skin on the outer walls maps onto the fuzzy texture.
    let fuzzy = matches!(job.s("fuzzy_skin").trim(), "external" | "all" | "allwalls");
    let pattern = own.or(fuzzy.then_some(Pattern::Fuzzy))?;
    let depth = { let d = job.f("fuzzy_skin_thickness"); if d > 0.0 { d } else { 0.3 } }.clamp(0.05, 1.5);
    let scale = { let s = job.f("fab_wall_texture_scale"); if s > 0.0 { s } else { 2.0 } }.clamp(0.4, 50.0);
    // The step resolves the pattern (≥ 8 samples per feature) without flooding the planner.
    let user_step = job.f("fuzzy_skin_point_distance");
    let step = if own.is_some() || user_step <= 0.0 { (scale / 8.0).clamp(0.1, 0.4) } else { user_step.clamp(0.1, 2.0) };
    let ax = job.f("machine_max_acceleration_x");
    let ay = job.f("machine_max_acceleration_y");
    let max_accel = if ax > 0.0 && ay > 0.0 { ax.min(ay) } else { 10_000.0 };
    Some(Texture { pattern, depth, scale, step, holes: job.b("fab_wall_texture_holes"), max_accel })
}
