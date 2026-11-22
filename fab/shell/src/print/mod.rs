//! Print: the slice job's settings, the slice's status and the animated print
//! preview, as the Print editor and the viewport see them.
//!
//! The app owns the slicer: it answers [`PrintAction::Slice`] on a background
//! thread and hands back a [`PrintPreview`] (the moves as one GPU instance
//! publication, built on that thread) plus a [`PrintTimeline`] that places the
//! nozzles at any time. Everything here is plain data the panel edits through
//! [`PrintAction`]s and the viewport draws from; the playback clock is
//! [`tick`], called once per frame by the app while a preview plays.
//!
//! Main-loop cost: nothing without a preview; while playing, one binary search
//! for the drawn count and one timeline query per frame, plus one draw per live bead chunk
//! (no geometry rebuild).

pub mod bead;
pub mod calibrate;
pub mod colour_map;
pub mod colours;
pub mod follow;
pub mod job_card;
pub mod job_stats;
pub mod job_list;
pub mod jobs;
pub mod panel;
pub mod resin;
pub mod stepper;
pub mod timeline;
pub mod support_list;
pub mod trays;
pub mod trees;

pub use trees::PlateTree;

use makepad_widgets_core::makepad_platform::retained_instances::RetainedInstances;
use makepad_widgets_core::*;
use std::sync::{Arc, OnceLock};

pub use bead::Bead;

/// Floats per preview instance ([`bead`]): `seg_a` (x, y, z, t start), `seg_b` (x, y, z, and
/// the duration and the speed), `seg_k` (width and height, layer, role, kind, filament, tag),
/// positions in printer millimetres.
pub const PREVIEW_SLOTS: usize = bead::SLOTS;

static DRAWN_HOOK: OnceLock<fn(u64)> = OnceLock::new();

/// Set the app's preview draw notification once at startup.
pub fn set_drawn_hook(f: fn(u64)) {
    let _ = DRAWN_HOOK.set(f);
}

/// A viewport drew this preview.
pub fn drawn(id: u64) {
    if let Some(f) = DRAWN_HOOK.get() {
        f(id);
    }
}

/// Built on the model or slice worker, shared by every viewport.
pub fn issue_markers(report: &fab_printability::Report) -> RetainedInstances {
    let mut data = Vec::new();
    for (index, issue) in report.issues.iter().enumerate() {
        let [x, y, z] = issue.at_mm.map(|v| v as f32);
        for (dx, dy) in [(2.0, 0.0), (0.0, 2.0)] {
            let layer = issue.layer.map(|l| l as f32).unwrap_or(-1.0);
            data.extend_from_slice(&Bead { a: [x - dx, y - dy, z], b: [x + dx, y + dy, z], width: 0.8, height: 0.8, layer, role: issue.severity as u8 as f32, tag: index as f32, kind: 3.0, ..Bead::default() }.pack());
        }
    }
    RetainedInstances::new(PREVIEW_SLOTS, data.into()).expect("whole marker instances")
}

/// One short-lived frame around the selected issue. Four retained preview segments,
/// built only on a press; every viewport shares the publication and start time.
#[derive(Clone)]
pub struct IssueSpot {
    pub instances: RetainedInstances,
    pub started: f64,
}

impl IssueSpot {
    pub const FADE_SECONDS: f64 = 2.0;
    pub const MARGIN_MM: f64 = 1.0;

    fn new(issue: &fab_printability::Issue, z: f32, now: f64) -> Self {
        Self::framing(issue.footprint.min, issue.footprint.max, z, issue.layer, issue.severity, now)
    }

    /// The frame round an XY box (printer mm) at height `z`, on `layer` (none: whatever layers
    /// show), in `severity`'s colour.
    fn framing(min: [f64; 2], max: [f64; 2], z: f32, layer: Option<u32>, severity: fab_printability::Severity, now: f64) -> Self {
        let lo = min.map(|v| (v - Self::MARGIN_MM) as f32);
        let hi = max.map(|v| (v + Self::MARGIN_MM) as f32);
        let corners = [[lo[0], lo[1]], [hi[0], lo[1]], [hi[0], hi[1]], [lo[0], hi[1]]];
        let mut data = Vec::with_capacity(4 * PREVIEW_SLOTS);
        for i in 0..4 {
            let (a, b) = (corners[i], corners[(i + 1) % 4]);
            let layer = layer.map_or(-1.0, |l| l as f32);
            data.extend_from_slice(&Bead { a: [a[0], a[1], z], b: [b[0], b[1], z], width: 0.6, layer, role: severity as u8 as f32, tag: -2.0, kind: 3.0, ..Bead::default() }.pack());
        }
        Self { instances: RetainedInstances::new(PREVIEW_SLOTS, data.into()).expect("four issue frame segments"), started: now }
    }

    pub fn opacity(&self, now: f64) -> f32 {
        (1.0 - (now - self.started) / Self::FADE_SECONDS).clamp(0.0, 1.0) as f32
    }
}

/// `seg_k.w` values.
/// Instance kinds (the `kind` slot holds `kind + MODE_STRIDE * mode`, the move's
/// experimental mode).
pub mod kind {
    pub const EXTRUDE: f32 = 0.0;
    pub const TRAVEL: f32 = 1.0;
    pub const WIPE: f32 = 2.0;
    /// A Z hop: drawn as travel, told apart in playback.
    pub const ZHOP: f32 = 5.0;
    /// A planar terrace of a non-planar top (the before of the before/after).
    pub const GHOST: f32 = 4.0;
    pub const MODE_STRIDE: f32 = 8.0;
}

/// A wall bead's role slot (`seg_k.x`) also says how its wall stacks:
/// `role + ROLE_STRIDE * (below + 256 * above)`. Each of the two is a step: how far the wall of the
/// layer below or above stands out to the bead's right, in 127ths of the bead's width, with
/// [`stack::IN_LINE`] directly over it and [`stack::NONE`] meaning no wall there. The Sliced view
/// uses the steps to draw the creases between the beads where they are.
///
/// A skin's or a solid fill's bead (roles 5..7) says in the same bits where its own layer's beads
/// lie beside it: `role + ROLE_STRIDE * (right + 256 * left)`, each the distance across to the
/// nearest bead running with it on that side, in 127ths of its width ([`stack::side`]),
/// [`stack::NONE`] where none lies within reach.
pub mod stack {
    pub const ROLE_STRIDE: f32 = 32.0;
    pub const NONE: u32 = 0;
    pub const IN_LINE: u32 = 128;

    /// The step of a wall standing `offset` (mm, to the right) beside a bead `width` wide.
    pub fn step(offset: f32, width: f32) -> u32 {
        (IN_LINE as f32 + (offset / width.max(1e-3) * 127.0).round()).clamp(1.0, 255.0) as u32
    }

    /// The side neighbour `distance` (mm, across) from a skin's bead `width` wide.
    pub fn side(distance: f32, width: f32) -> u32 {
        (distance / width.max(1e-3) * 127.0).round().clamp(1.0, 255.0) as u32
    }

    /// Whether a role's slot holds its side neighbours ([`side`]), not a wall's steps: solid fill
    /// and skins.
    pub fn sides(role: f32) -> bool {
        (5.0..=7.0).contains(&role)
    }

    /// A role with its stack's steps (or its side neighbours: `below` the right, `above` the left).
    pub fn pack(role: f32, below: u32, above: u32) -> f32 {
        role + ROLE_STRIDE * (below + 256 * above) as f32
    }

    /// The role of a role slot.
    pub fn role(slot: f32) -> f32 {
        slot % ROLE_STRIDE
    }
}

/// Palette texture layout (one RGBA texel each): roles first, then filaments,
/// nozzles, travel, wipe and the two nozzle heads.
pub mod palette {
    pub const ROLES: usize = 0;
    pub const ROLE_COUNT: usize = 32;
    pub const FILAMENTS: usize = 32;
    pub const FILAMENT_COUNT: usize = 16;
    pub const NOZZLES: usize = 48;
    pub const TRAVEL: usize = 50;
    pub const WIPE: usize = 51;
    /// The two nozzle heads (left, right), each in its nozzle's colour whatever it does: the
    /// one printing stands on the bead, ringed, the one waiting is dimmed.
    pub const HEADS: usize = 52;
    /// One colour per experimental mode (record `mode`), then the terrace ghost.
    pub const MODES: usize = 64;
    pub const MODE_COUNT: usize = 9;
    pub const GHOST: usize = 73;
    pub const WIDTH: usize = 80;
}

/// The two nozzles' colours in Nozzle mode (left, right).
pub const NOZZLE_COLORS: [[f32; 4]; 2] = [[0.27, 0.56, 1.0, 1.0], [1.0, 0.56, 0.16, 1.0]];
pub const TRAVEL_COLOR: [f32; 4] = [0.36, 0.78, 0.40, 1.0];
pub const WIPE_COLOR: [f32; 4] = [0.85, 0.85, 0.30, 1.0];
/// Colour by mode: standard, texture, vase, scarf, adaptive, ironing, interlock, arc front, non-planar.
pub const MODE_COLORS: [[f32; 4]; 9] = [
    [0.55, 0.56, 0.60, 1.0], [0.90, 0.45, 0.20, 1.0], [0.25, 0.70, 0.95, 1.0], [0.95, 0.80, 0.20, 1.0], [0.55, 0.85, 0.35, 1.0],
    [0.85, 0.85, 0.90, 1.0], [0.80, 0.35, 0.80, 1.0], [0.95, 0.35, 0.40, 1.0], [0.20, 0.90, 0.75, 1.0],
];
pub const GHOST_COLOR: [f32; 4] = [0.70, 0.62, 0.50, 1.0];
pub const MODE_NAMES: [&str; 9] = ["Standard", "Wall texture", "Spiral vase", "Scarf seam", "Adaptive layers", "Ironing", "Interlock", "Arc overhang", "Non-planar top"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ColorBy {
    #[default]
    Feature,
    Speed,
    Nozzle,
    Filament,
    Layer,
    /// The experimental mode of each move (texture, vase, non-planar ...).
    Mode,
}

impl ColorBy {
    pub const ALL: [ColorBy; 6] = [ColorBy::Feature, ColorBy::Speed, ColorBy::Nozzle, ColorBy::Filament, ColorBy::Layer, ColorBy::Mode];

    pub fn label(self) -> &'static str {
        match self {
            ColorBy::Feature => "Feature",
            ColorBy::Speed => "Speed",
            ColorBy::Nozzle => "Nozzle",
            ColorBy::Filament => "Filament",
            ColorBy::Layer => "Layer",
            ColorBy::Mode => "Mode",
        }
    }

    pub fn shader_mode(self) -> f32 {
        match self {
            ColorBy::Feature => 0.0,
            ColorBy::Speed => 1.0,
            ColorBy::Nozzle => 2.0,
            ColorBy::Filament => 3.0,
            ColorBy::Layer => 4.0,
            ColorBy::Mode => 5.0,
        }
    }

    pub fn id(self) -> LiveId {
        LiveId::from_str(self.label())
    }
}

/// One material of the job: a printer position the print colours map to (tool `i + 1` is
/// `materials[i]`). Derived from the colour mapping, never edited as such.
#[derive(Clone, Debug, PartialEq)]
pub struct JobMaterial {
    /// The filament preset its tray's material resolves to (in Fab defaults: a label only).
    pub preset: String,
    /// Its colour (linear RGBA) as Sliced and Print draw it: the colour its tray reports,
    /// else its print colour's own colour.
    pub color: [f32; 4],
    /// The nozzle it prints with (the machine's index: left 0, right 1).
    pub nozzle: usize,
    /// The printer tray it feeds from (`None`: an opened plate's own filament, no tray).
    pub tray: Option<fab_printer_link::model::TrayId>,
    /// Its tray's path shares nothing with the other slots (`fab_dedicated_path`): its changes
    /// purge nothing.
    pub dedicated: bool,
}

/// What the Print panel and a refused slice say when no bed plate suits every job material.
pub const NO_PLATE_FITS: &str = "No bed plate fits all materials";

/// The job the next Slice runs.
#[derive(Clone, Debug, PartialEq)]
pub struct PrintSettings {
    /// The user's installed slicer profiles (when found); otherwise Fab's
    /// own defaults.
    pub use_installed: bool,
    pub machine: String,
    /// The bed plate: as picked, else a default every material suits; empty only when no
    /// plate fits all materials ([`NO_PLATE_FITS`]), and then nothing slices.
    pub plate: String,
    pub process: String,
    /// The job's materials, tool by tool (the colour mapping's positions).
    pub materials: Vec<JobMaterial>,
    /// The global layer height as it resolves (the project's, else the process's): what the
    /// finish preview and the test parts read. The slicer's value comes from the job.
    pub layer_height: f64,
}

impl Default for PrintSettings {
    fn default() -> Self {
        PrintSettings {
            use_installed: false,
            machine: String::new(),
            plate: String::new(),
            process: String::new(),
            materials: Vec::new(),
            layer_height: 0.2,
        }
    }
}

/// What the installed profiles offer (names only).
#[derive(Clone, Debug, Default)]
pub struct PrintProfiles {
    pub available: bool,
    pub machines: Vec<String>,
    pub plates: Vec<String>,
    pub processes: Vec<String>,
    pub filaments: Vec<String>,
    /// Why the installed profiles are not used, or where they came from.
    pub note: String,
}

/// What a running status's stage starts with while a part's grown pattern grows toward the state
/// that prints: the slice waits for it, and the stage counts its steps.
pub const GROWING: &str = "Growing the pattern";

/// A running live slice's stage while its supports plan (its layers are cut): the progress is
/// the plan's, and the chip says so.
pub const PLANNING_SUPPORTS: &str = "Planning supports";

#[derive(Clone, Debug, Default, PartialEq)]
pub enum SliceStatus {
    #[default]
    Idle,
    Running { progress: f32, stage: String },
    /// Wall time.
    Done { seconds: f64 },
    Failed(String),
}

/// One layer of the preview, a stretch of the print's time on it.
#[derive(Clone, Copy, Debug)]
pub struct PreviewLayer {
    pub layer: u32,
    pub z: f32,
    pub t_start: f64,
    pub t_end: f64,
    /// Where its beads start in each stream of [`PrintPreview::beads`], by [`BeadStream`]. Each
    /// runs to the next layer's start in its stream.
    pub starts: [u32; 3],
}

/// The streams of a preview's beads ([`PrintPreview::beads`]), each in print order. Inside a part
/// closed in on every side (`MoveRecord::flags`' `CORE`), its fills are its core and its inner
/// walls their lining; the rest is the surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BeadStream {
    Surface,
    Lining,
    Core,
}

impl BeadStream {
    /// The streams in their order: the one a bead is in is its index here.
    pub const ALL: [BeadStream; 3] = [BeadStream::Surface, BeadStream::Lining, BeadStream::Core];
}

/// A legend row: an extrusion role, its colour and its share of the time.
#[derive(Clone, Debug)]
pub struct PreviewRole {
    pub id: u8,
    pub name: String,
    pub color: [f32; 4],
    pub seconds: f64,
    /// Extruded path length (mm).
    pub length_mm: f64,
}

/// A row of the feature table for time at the tool changes, which draws no bead: the head
/// switching nozzles, a nozzle changing filament.
#[derive(Clone, Debug)]
pub struct PreviewChange {
    pub name: String,
    pub seconds: f64,
    /// What the time is made of: "119 × 5 s".
    pub detail: String,
}

/// One filament slot's use in the slice.
#[derive(Clone, Copy, Debug, Default)]
pub struct FilamentUse {
    /// 0-based slot (the plate's filament `slot + 1`).
    pub slot: usize,
    /// All the slot feeds: what it prints and what its changes purge.
    pub grams: f64,
    pub metres: f64,
    /// The part of `grams` its changes purge.
    pub purge_grams: f64,
    /// The time its moves take.
    pub seconds: f64,
}

/// Where the print is at one time.
#[derive(Clone, Debug, Default)]
pub struct PrintNow {
    /// Both nozzle tips (left, right) in printer millimetres.
    pub nozzles: [[f32; 3]; 2],
    /// The printing nozzle of the two drawn (0 = left).
    pub active: usize,
    /// The printing nozzle in the machine's list, of any count.
    pub head: usize,
    /// What it is doing: a role name, "Travel", a firmware step.
    pub doing: String,
    /// Project filament slot (1-based, 0 = none).
    pub filament: u8,
    /// Target temperature per nozzle, in the machine's order: two at least.
    pub nozzle_temps: Vec<i16>,
    pub bed_temp: i16,
    pub fan: u8,
    /// Commanded speed (mm/s).
    pub speed: f32,
}

impl PrintNow {
    /// What the head does and with which nozzle: "Travel on the left nozzle", "Travel on Tool 3".
    pub fn doing_text(&self) -> String {
        match self.nozzle_temps.len() {
            heads if heads > 2 => format!("{} on {}", self.doing, fab_plate::machine::nozzle_name(heads, self.head)),
            _ => format!("{} on the {} nozzle", self.doing, if self.active == 0 { "left" } else { "right" }),
        }
    }

    /// The nozzles' target temperatures: "nozzles 200 / 0 °C" of a two-nozzle head, each head
    /// by name of more.
    pub fn temps_text(&self) -> String {
        let t = &self.nozzle_temps;
        match t.len() {
            heads if heads > 2 => t.iter().enumerate().map(|(i, c)| format!("{} {c} °C", fab_plate::machine::nozzle_name(heads, i))).collect::<Vec<_>>().join("  ·  "),
            _ => format!("nozzles {} / {} °C", t.first().copied().unwrap_or(0), t.get(1).copied().unwrap_or(0)),
        }
    }
}

/// A live slice's print time and filament from its estimate, ahead of its playing preview: a step
/// back or forth brings the kept totals at once, the preview's times once the planner has run again.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EstimateTotals {
    pub seconds: f64,
    /// Of `seconds`, the printer routines (homing, levelling, heating).
    pub prepare_seconds: f64,
    pub grams: f64,
}

/// Places the print at a time (the app's records behind it).
pub trait PrintTimeline: Send + Sync {
    fn now(&self, t: f64) -> PrintNow;
}

/// One test print's share of a slice: what its row and `fab.test_print`'s read say.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PieceFigures {
    pub name: String,
    pub seconds: f64,
    pub grams: f64,
    /// Of `grams`, what goes into supports and brim.
    pub support_grams: f64,
    /// Of `support_grams`, what goes into its brim.
    pub brim_grams: f64,
    /// The printed extent on the bed (mm).
    pub bed: [[f64; 3]; 2],
    /// The first and last layer it prints on.
    pub layers: (u32, u32),
    /// Its first layer's box (mm), area and shorter side.
    pub footprint: [f64; 2],
    pub height_over_footprint: f64,
    /// The objects its parts print as: what the slice's issues name.
    pub objects: Vec<String>,
}

/// A stop for items placed into the print (`insert(.., embed: true)`): the layer it comes before
/// (0-based), that layer's print z (mm) and what goes in, by part name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PreviewPause {
    pub layer: u32,
    pub z: f32,
    pub items: Vec<String>,
}

impl PreviewPause {
    /// "Pause before layer 36, at 7.00 mm: place M3 nut": a step of the print in words.
    pub fn step(&self) -> String {
        format!("Pause before layer {}, at {:.2} mm: place {}", self.layer + 1, self.z, self.items.join(", "))
    }
}

/// Heat-set inserts melted into a part after the print (`insert(.., melt: heat_insert(..))`):
/// how many of one size go into which part, and the soldering iron's temperature in °C, the print
/// temperature of the part's filament (0: not known).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PreviewMelt {
    pub size: String,
    pub count: u32,
    pub into: String,
    pub iron: u32,
}

impl PreviewMelt {
    /// "After the print: melt in 4 × M3 inserts into Base, the iron at 215 °C": a step in words.
    pub fn step(&self) -> String {
        let iron = if self.iron > 0 { format!("the iron at {} °C", self.iron) } else { "the iron at the filament's print temperature".to_string() };
        format!("After the print: melt in {} × {} insert{} into {}, {iron}", self.count, self.size, if self.count == 1 { "" } else { "s" }, self.into)
    }
}

/// A sliced plate ready to play.
pub struct PrintPreview {
    /// A test plate's pieces, one per test print: none for a whole plate.
    pub pieces: Vec<PieceFigures>,
    /// A test plate's time and grams shared by its pieces (start, purge, the moves between them).
    pub shared: (f64, f64),
    /// What tells this slice's G-code from another's, once it has any.
    pub gcode_key: Option<u64>,
    pub printability: Arc<fab_printability::Report>,
    pub markers: RetainedInstances,
    /// One instance per printed segment, [`PREVIEW_SLOTS`] floats each, in three streams
    /// ([`BeadStream`]), each in time order and its own publication. What is drawn at a time is a
    /// binary search of each stream's start times ([`BeadStreams::cut_at`]).
    pub beads: BeadStreams,
    /// Travel and wipe moves in time order, their own stream: drawn only while Travel is on, so
    /// they cost nothing otherwise (a quarter of a plate's moves).
    pub travel: RetainedInstances,
    /// `beads` in runs near each other on the plate, so a camera or a layer range that can't
    /// show a run skips it whole (`PreviewBlock::of`): the surface's, then the lining's, then
    /// the core's.
    pub blocks: Vec<PreviewBlock>,
    /// Per part (the beads' part index): its lowest and highest layer (`PreviewBlock::of`), so a
    /// frame tells which parts show whole (`PrintFrame::closed`). A part with no beads has none;
    /// a live part not yet whole in one content has infinity for both, so it never shows whole.
    pub part_layers: Vec<[f32; 2]>,
    /// Live bead chunks, drawn after `instances` and cut by the layer range, each in its own
    /// draw (and GPU buffer): an unchanged chunk keeps its buffer. Of each, the runs a camera
    /// can't show, or a closed part's core, are left out as the timeline's `blocks` are.
    pub chunks: Vec<LiveChunk>,
    pub layers: Vec<PreviewLayer>,
    pub total: f64,
    /// A preview still growing ([`PrintState::grow_preview`]): its beads, layers and playback
    /// reach this far (s) of `total`.
    pub grown: Option<f64>,
    /// The extrusions' bounds (printer mm): what a fresh preview frames.
    pub bounds: [[f32; 3]; 2],
    /// Commanded speed range of the extrusions (mm/s).
    pub speed_range: [f32; 2],
    pub filament_colors: Vec<[f32; 4]>,
    pub roles: Vec<PreviewRole>,
    /// The feature table's rows after the roles: time at the tool changes.
    pub changes: Vec<PreviewChange>,
    pub timeline: Arc<dyn PrintTimeline>,
    /// Filament used per slot.
    pub filament_use: Vec<FilamentUse>,
    /// The changes that purge, and the time the purges take (inside `total`).
    pub purges: u32,
    pub purge_seconds: f64,
    /// Printer routines (homing, levelling, heating) inside `total`.
    pub prepare_seconds: f64,
    /// Key / value rows for the panel (time, filament, layers, changes).
    pub summary: Vec<(String, String)>,
    /// Where the print waits for items to be placed into it, in print order.
    pub pauses: Vec<PreviewPause>,
    /// The heat-set inserts to melt in after the print.
    pub melts: Vec<PreviewMelt>,
    /// Per experimental mode present: its print time, filament and steepest Z rate.
    pub modes: Vec<ModeStats>,
    /// The fastest Z of any sloped extrusion (mm/s): f · |dz| / L.
    pub max_z_rate: f32,
    /// The non-planar tops of the plate (none: no part has one).
    pub nonplanar: Option<NpSummary>,
    /// The planar terraces the non-planar tops replace, as bead instances (kind GHOST):
    /// drawn instead of the non-planar beads while comparing.
    pub ghosts: RetainedInstances,
    /// Distinguishes previews (palette and GPU caches key on it).
    pub id: u64,
    /// Per part (the beads' part index): the XY shift (mm) onto the plate of `chunks` built in
    /// the part's own frame (a live preview's, so a part that only moves keeps every chunk).
    /// Empty: every bead is placed.
    pub part_offsets: Arc<[[f32; 2]]>,
    /// Per part (the beads' part index): its object. The plate as it was sliced: parts the
    /// shown plate has since reordered, or turned off, are found by it.
    pub part_objects: Arc<[String]>,
    /// The colour mapping's job tool (0-based) each filament slot here is: a slice prints only
    /// the job's tools that print. Empty: slot `i` is tool `i`.
    pub tools: Vec<usize>,
}

/// Every instance of a publication of preview instances, in order, read segment by segment: a big
/// print's preview is many segments, and reading it never makes a contiguous copy of them.
pub fn each_instance(instances: &RetainedInstances) -> impl Iterator<Item = &[f32]> {
    instances.segments().iter().flat_map(|segment| segment.data().chunks_exact(PREVIEW_SLOTS))
}

/// How many of `instances` (in time order) start by time `t`: a binary search of their start
/// times where they are, each step finding its segment by its offset.
pub fn started_by(instances: &RetainedInstances, t: f64) -> usize {
    started_in(instances, 0..instances.count() as u32, t) as usize
}

/// The end of the beads of `run` (a stretch of `instances` in time order) started by time `t`.
fn started_in(instances: &RetainedInstances, run: std::ops::Range<u32>, t: f64) -> u32 {
    let (mut lo, mut hi) = (run.start as usize, run.end as usize);
    while lo < hi {
        let mid = (lo + hi) / 2;
        let at = mid * PREVIEW_SLOTS;
        let t0 = instances.data_slices(at..at + PREVIEW_SLOTS).next().map_or(f32::INFINITY, |(_, s)| bead::t0(s));
        if t0 as f64 <= t {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo as u32
}

/// Instance `i` of `instances`.
pub fn instance(instances: &RetainedInstances, i: usize) -> Option<[f32; PREVIEW_SLOTS]> {
    instances.data_slices(i * PREVIEW_SLOTS..(i + 1) * PREVIEW_SLOTS).next().map(|(_, s)| s.try_into().expect("a whole instance"))
}

/// A preview's beads ([`PrintPreview::beads`]): its three streams ([`BeadStream`]), each in time
/// order and its own publication, so each draws from its own buffer and a preview that grows
/// appends to each.
#[derive(Clone, Debug)]
pub struct BeadStreams(pub [RetainedInstances; 3]);

impl BeadStreams {
    /// No beads.
    pub fn none() -> BeadStreams {
        BeadStreams(std::array::from_fn(|_| RetainedInstances::new(PREVIEW_SLOTS, Vec::<f32>::new().into()).expect("no beads")))
    }

    /// `surface` as the surface, with no lining or core.
    pub fn surface(surface: RetainedInstances) -> BeadStreams {
        let mut streams = BeadStreams::none();
        streams.0[BeadStream::Surface as usize] = surface;
        streams
    }

    /// The beads of stream `s`.
    pub fn of(&self, s: BeadStream) -> &RetainedInstances {
        &self.0[s as usize]
    }

    /// How many beads the streams hold.
    pub fn count(&self) -> usize {
        self.0.iter().map(RetainedInstances::count).sum()
    }

    /// Every bead, stream by stream, each in time order.
    pub fn each(&self) -> impl Iterator<Item = &[f32]> {
        self.0.iter().flat_map(each_instance)
    }

    /// Every bead in time order: the streams merged by their beads' starts (the earlier stream
    /// first at a tie).
    pub fn each_in_time(&self) -> impl Iterator<Item = &[f32]> {
        let mut runs = self.0.each_ref().map(|r| each_instance(r).peekable());
        std::iter::from_fn(move || {
            let next = (0..3).filter_map(|k| runs[k].peek().map(|b| (bead::t0(b), k))).min_by(|a, b| a.partial_cmp(b).expect("bead times are numbers"))?.1;
            runs[next].next()
        })
    }

    /// What has started by time `t`: each stream is in time order, so each is a binary search of
    /// its start times.
    pub fn cut_at(&self, t: f64) -> RunCut {
        RunCut(self.0.each_ref().map(|r| started_in(r, 0..r.count() as u32, t)))
    }

    /// The latest bead to start by `t` ([`Self::cut_at`]): the latest of the streams' last
    /// started beads.
    pub fn last_started(&self, t: f64) -> Option<[f32; PREVIEW_SLOTS]> {
        self.0.iter().zip(self.cut_at(t).0).filter_map(|(r, n)| n.checked_sub(1).and_then(|i| instance(r, i as usize)))
            .max_by(|a, b| (bead::t0(a), bead::t1(a)).partial_cmp(&(bead::t0(b), bead::t1(b))).expect("bead times are numbers"))
    }
}

/// What of a preview's beads has started by a time ([`BeadStreams::cut_at`]): of each stream
/// ([`BeadStream`]), how many of its first beads.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunCut(pub [u32; 3]);

impl RunCut {
    /// All of `beads` started.
    pub fn all(beads: &BeadStreams) -> RunCut {
        RunCut(beads.0.each_ref().map(|r| r.count() as u32))
    }

    /// The beads drawn: each stream's started ones.
    pub fn ranges(&self) -> [std::ops::Range<u32>; 3] {
        self.0.map(|n| 0..n)
    }

    /// How many beads have started.
    pub fn count(&self) -> u32 {
        self.0.iter().sum()
    }
}

/// A live chunk ([`PrintPreview::chunks`]): some layers of one part, in its frame.
#[derive(Clone)]
pub struct LiveChunk {
    pub beads: RetainedInstances,
    /// The part its beads are of (the beads' part index).
    pub part: usize,
    /// Its beads in runs, layer by layer ([`PreviewBlock::of_layer`]), in the part's frame: a
    /// run's `first` and `end` are its beads' places in the chunk.
    pub runs: Arc<[PreviewBlock]>,
}

/// A run of a stream of [`PrintPreview::beads`] close together.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreviewBlock {
    /// Its first bead and the one after its last, in its stream.
    pub first: u32,
    pub end: u32,
    /// All its beads reach, in mm: their ends, section and overlap.
    pub bounds: [[f32; 3]; 2],
    /// Its lowest and highest layer (negative: printer moves outside any layer).
    pub layers: [f32; 2],
    /// Its widest bead (mm): how far away it is a pixel or less across.
    pub width: f32,
    /// The stream its beads are in.
    pub stream: BeadStream,
    /// In the lining or the core: the part all its beads are of.
    pub part: Option<usize>,
}

/// A square joint ([`join_beads`]): a free end, or a straight run.
const JOINT_SQUARE: f32 = 127.0;
/// The joints a right angle's shear takes either side of square ([`join_beads`]).
const JOINT_STEPS: f32 = 100.0;
/// The joints past them that tip, down to a turn back on itself ([`join_beads`]).
const JOINT_SHARP_STEPS: f32 = 25.0;

/// How each bead of a stretch of moves meets its neighbours, written above its filament
/// ([`Bead::filament`]): filament + 256 * (the start's joint + 256 * the end's). A joint of 127 is square (a
/// free end, or a straight run). Up to a right angle (to the step, either side of square), 27..=227
/// shears the bead's end onto the plane halfway between the two beads, tan(turn / 2) = (joint - 127)
/// / 100, a left turn reaching the right edge forward. Past it, the joints beyond those end both
/// beads in one tip on the outside of the corner, cos(turn / 2) = (126 - |joint - 127|) / 25 *
/// sqrt(1 / 2). `DrawFabPath`'s vertex shader draws them.
/// A negative width or height says that end is free, as in `PreviewBuilder::add_placed`.
pub fn join_beads(beads: &mut [f32]) {
    let mut start = JOINT_SQUARE;
    for i in 0..beads.len() / PREVIEW_SLOTS {
        let (bead, rest) = beads[i * PREVIEW_SLOTS..].split_at_mut(PREVIEW_SLOTS);
        let turn = rest.get(..PREVIEW_SLOTS).map_or(0.0, |next| joint_turn(bead, next));
        let end = JOINT_SQUARE + turn;
        let filament = bead::filament(bead);
        bead::set_filament(bead, filament + 256.0 * (start + 256.0 * end));
        start = JOINT_SQUARE - turn;
    }
}

/// The joint from `bead` to `next` ([`join_beads`]), less a square one's: 0 where they do not
/// join or run straight on, a shear up to [`JOINT_STEPS`] either way, past it a tip.
fn joint_turn(bead: &[f32], next: &[f32]) -> f32 {
    if !(bead::end_joins(bead) && bead::start_joins(next)) {
        return 0.0;
    }
    let (ax, ay) = (bead[4] - bead[0], bead[5] - bead[1]);
    let (bx, by) = (next[4] - next[0], next[5] - next[1]);
    let lengths = ((ax * ax + ay * ay) * (bx * bx + by * by)).sqrt();
    let (sin, cos) = (ax * by - ay * bx, ax * bx + ay * by);
    if lengths <= 0.0 {
        return 0.0;
    }
    // A right angle shears whatever side of square the moves' rounding leaves it.
    let shear = (sin / (lengths + cos) * JOINT_STEPS).round();
    if shear.abs() <= JOINT_STEPS {
        return shear;
    }
    let out = ((1.0 + cos / lengths) * 0.5).max(0.0).sqrt() * std::f32::consts::SQRT_2;
    (JOINT_STEPS + 1.0 + ((1.0 - out) * JOINT_SHARP_STEPS).round()).copysign(sin)
}

/// Past a right angle a path's two beads end in one tip on the outside of the corner
/// ([`join_beads`]): the bead shader moves the top and bottom of their sections there. A bead
/// has vertices at its two ends only, so that move would lean a long bead its whole length. An
/// end that tips, on a printed bead longer than two widths, goes into a bead of its own one width
/// long, straight on from the rest (a square joint) and its share of the bead's time: the tip
/// leans that bead only. Before [`join_beads`], on the same stretch; `ends` are the beads' end
/// times as their moves have them (s), their packed durations' rounding aside, so each bead
/// ends where it should but for its own; `inside`'s stretches of bead indices follow the beads.
pub fn split_tips(beads: &mut Vec<f32>, ends: &[f64], inside: &mut [(std::ops::Range<u32>, BeadStream)]) {
    let n = beads.len() / PREVIEW_SLOTS;
    let at = |i: usize| &beads[i * PREVIEW_SLOTS..(i + 1) * PREVIEW_SLOTS];
    let tips = |i: usize| i + 1 < n && joint_turn(at(i), at(i + 1)).abs() > JOINT_STEPS;
    // Each bead's cuts, as fractions along it: after its start's tip, before its end's.
    let cuts = |i: usize, tip_before: bool, tip_after: bool| {
        let b = at(i);
        if !(tip_before || tip_after) || bead::kind(b) % kind::MODE_STRIDE != kind::EXTRUDE {
            return (0.0, 1.0);
        }
        let w = bead::section(b).0.abs();
        let d = [b[4] - b[0], b[5] - b[1], b[6] - b[2]];
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        if len <= 2.0 * w {
            return (0.0, 1.0);
        }
        (if tip_before { w / len } else { 0.0 }, if tip_after { 1.0 - w / len } else { 1.0 })
    };
    let pieces = |c: (f32, f32)| 1 + (c.0 > 0.0) as u32 + (c.1 < 1.0) as u32;
    let mut each = Vec::new();
    let mut tip_before = false;
    for i in 0..n {
        let tip_after = tips(i);
        let c = cuts(i, tip_before, tip_after);
        if pieces(c) > 1 && each.is_empty() {
            each = vec![(0.0, 1.0); n];
        }
        if !each.is_empty() {
            each[i] = c;
        }
        tip_before = tip_after;
    }
    if each.is_empty() {
        return;
    }
    // Where each bead's first piece goes.
    let mut first = Vec::with_capacity(n + 1);
    let mut k = 0;
    for &c in &each {
        first.push(k);
        k += pieces(c);
    }
    first.push(k);
    for (r, _) in inside.iter_mut() {
        *r = first[r.start as usize]..first[r.end as usize];
    }
    let mut out = Vec::with_capacity(k as usize * PREVIEW_SLOTS);
    for (i, &(f0, f1)) in each.iter().enumerate() {
        let b = at(i);
        if (f0, f1) == (0.0, 1.0) {
            out.extend_from_slice(b);
            continue;
        }
        let t0 = bead::t0(b) as f64;
        let time = |f: f32| if f < 1.0 { t0 + (ends[i] - t0) * f as f64 } else { ends[i] };
        let point = |f: f32| [b[0] + (b[4] - b[0]) * f, b[1] + (b[5] - b[1]) * f, b[2] + (b[6] - b[2]) * f];
        for (fa, fb) in [(0.0, f0), (f0, f1), (f1, 1.0)] {
            if fb <= fa {
                continue;
            }
            let mut s: [f32; PREVIEW_SLOTS] = b.try_into().expect("a bead's slots");
            if fa > 0.0 {
                s[..3].copy_from_slice(&point(fa));
                s[3] = time(fa) as f32;
            }
            if fb < 1.0 {
                s[4..7].copy_from_slice(&point(fb));
            }
            let dur = (time(fb) - s[3] as f64) as f32;
            bead::set_dur(&mut s, dur);
            // joined where it was cut: its width and height above zero there
            let mut signs = s[8].to_bits();
            if fa > 0.0 {
                signs &= !0x8000;
            }
            if fb < 1.0 {
                signs &= !0x8000_0000;
            }
            s[8] = f32::from_bits(signs);
            out.extend_from_slice(&s);
        }
    }
    *beads = out;
}

impl PreviewBlock {
    /// About a layer's beads: a run is as far as the skipping goes, and a frame walks them all.
    const BEADS: u32 = 1024;
    /// A move this far from a run starts the next: a jump to another part, or across it.
    const JUMP_MM: f32 = 10.0;

    /// The runs of `beads`, read segment by segment (never copied whole), their bounds on the
    /// plate: `part_offsets` places the beads built in their part's frame
    /// ([`PrintPreview::part_offsets`]). One in the lining or the core holds one part's beads: a
    /// part closed in leaves them out by its blocks. With them, per part (the beads' part
    /// index), its lowest and highest layer ([`PrintPreview::part_layers`]).
    pub fn of(beads: &BeadStreams, part_offsets: &[[f32; 2]]) -> (Vec<PreviewBlock>, Vec<[f32; 2]>) {
        let mut blocks = BlocksMaker::default();
        for s in BeadStream::ALL {
            for bead in each_instance(beads.of(s)) {
                blocks.push(s, bead, part_offsets);
            }
        }
        blocks.blocks()
    }

    /// The runs of one layer's beads of a part, in its frame (a live tile's): `counts` of them in
    /// each stream in turn, the surface's, the lining's, then the core's. A run's `first` and
    /// `end` are its beads' places among them.
    pub fn of_layer<'a>(beads: impl Iterator<Item = &'a [f32]>, counts: [u32; 3]) -> Vec<PreviewBlock> {
        let mut blocks = BlocksMaker::default();
        let mut streams = BeadStream::ALL.into_iter().zip(counts).flat_map(|(s, n)| std::iter::repeat_n(s, n as usize));
        for bead in beads {
            blocks.push(streams.next().expect("a stream for each bead"), bead, &[]);
        }
        let starts = [0, counts[0], counts[0] + counts[1]];
        let (mut runs, _) = blocks.blocks();
        for b in &mut runs {
            b.first += starts[b.stream as usize];
            b.end += starts[b.stream as usize];
        }
        runs
    }
}

/// The runs of a preview's beads as its streams are made ([`PreviewBlock::of`]): a preview that
/// grows adds its new beads and takes the runs so far.
#[derive(Default)]
pub struct BlocksMaker {
    blocks: [Vec<PreviewBlock>; 3],
    counts: [u32; 3],
    part_layers: Vec<[f32; 2]>,
}

impl BlocksMaker {
    /// The next bead of `stream`.
    pub fn push(&mut self, stream: BeadStream, bead: &[f32], part_offsets: &[[f32; 2]]) {
        let i = self.counts[stream as usize];
        self.counts[stream as usize] += 1;
        let p = bead_part(bead);
        let part = if stream == BeadStream::Surface { None } else { p };
        let layer = bead::layer(bead);
        if let Some(p) = p {
            if self.part_layers.len() <= p {
                self.part_layers.resize(p + 1, [f32::MAX, f32::MIN]);
            }
            self.part_layers[p] = [self.part_layers[p][0].min(layer), self.part_layers[p][1].max(layer)];
        }
        let o = p.and_then(|p| part_offsets.get(p)).copied().unwrap_or([0.0, 0.0]);
        // (a, width, b, height): the section grows a little past the width and height and the
        // ends overlap by half a width, which all stays within this reach.
        let (width, height) = bead::section(bead);
        let reach = width.abs().max(height.abs()) * 1.2;
        let lo = [bead[0].min(bead[4]) + o[0] - reach, bead[1].min(bead[5]) + o[1] - reach, bead[2].min(bead[6]) - reach];
        let hi = [bead[0].max(bead[4]) + o[0] + reach, bead[1].max(bead[5]) + o[1] + reach, bead[2].max(bead[6]) + reach];
        let blocks = &mut self.blocks[stream as usize];
        match blocks.last_mut() {
            Some(b) if b.part == part && i - b.first < PreviewBlock::BEADS && (0..3).all(|k| lo[k] - b.bounds[1][k] < PreviewBlock::JUMP_MM && b.bounds[0][k] - hi[k] < PreviewBlock::JUMP_MM) => {
                b.end = i + 1;
                for k in 0..3 {
                    b.bounds[0][k] = b.bounds[0][k].min(lo[k]);
                    b.bounds[1][k] = b.bounds[1][k].max(hi[k]);
                }
                b.layers = [b.layers[0].min(layer), b.layers[1].max(layer)];
                b.width = b.width.max(width.abs());
            }
            _ => blocks.push(PreviewBlock { first: i, end: i + 1, bounds: [lo, hi], layers: [layer, layer], width: width.abs(), stream, part }),
        }
    }

    /// The runs so far, the surface's, then the lining's, then the core's, and per part its
    /// lowest and highest layer.
    pub fn blocks(&self) -> (Vec<PreviewBlock>, Vec<[f32; 2]>) {
        (self.blocks.concat(), self.part_layers.clone())
    }
}

impl std::fmt::Debug for PrintPreview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrintPreview").field("beads", &self.beads.count()).field("layers", &self.layers.len()).field("total", &self.total).finish()
    }
}

impl PrintPreview {
    /// The colour mapping's job tool (0-based) filament slot `slot` is.
    pub fn job_tool(&self, slot: usize) -> usize {
        self.tools.get(slot).copied().unwrap_or(slot)
    }

    /// Anything to draw: timeline beads or a live chunk with beads (a live preview keeps an
    /// empty chunk for each block not shown yet, so every block keeps its draw slot).
    pub fn has_beads(&self) -> bool {
        self.beads.count() > 0 || self.travel.count() > 0 || self.chunks.iter().any(|c| c.beads.count() > 0)
    }

    /// Beads started by time `t` ([`BeadStreams::cut_at`]).
    pub fn cut_at(&self, t: f64) -> RunCut {
        self.beads.cut_at(t)
    }

    /// Travel instances started by time `t`.
    pub fn travel_count_at(&self, t: f64) -> usize {
        started_by(&self.travel, t)
    }

    /// The layer printing at `t` (index into `layers`).
    pub fn layer_at(&self, t: f64) -> usize {
        self.layers.partition_point(|l| l.t_start <= t).saturating_sub(1)
    }

    /// The top layer (a streaming slice holds each part's layers in turn, not in print order).
    pub fn max_layer(&self) -> u32 {
        self.layers.iter().map(|l| l.layer).max().unwrap_or(0)
    }

    /// The palette texture's texels (see [`palette`]).
    pub fn palette(&self) -> Vec<f32> {
        let mut data = vec![0.5f32; palette::WIDTH * 4];
        let mut put = |i: usize, c: [f32; 4]| data[i * 4..i * 4 + 4].copy_from_slice(&c);
        for r in &self.roles {
            if (r.id as usize) < palette::ROLE_COUNT {
                put(palette::ROLES + r.id as usize, r.color);
            }
        }
        for (i, c) in self.filament_colors.iter().take(palette::FILAMENT_COUNT).enumerate() {
            put(palette::FILAMENTS + i, *c);
        }
        put(palette::NOZZLES, NOZZLE_COLORS[0]);
        put(palette::NOZZLES + 1, NOZZLE_COLORS[1]);
        put(palette::TRAVEL, TRAVEL_COLOR);
        put(palette::WIPE, WIPE_COLOR);
        put(palette::HEADS, NOZZLE_COLORS[0]);
        put(palette::HEADS + 1, NOZZLE_COLORS[1]);
        put(54, [1.0, 0.08, 0.08, 1.0]);
        put(55, [1.0, 0.65, 0.05, 1.0]);
        put(56, [0.15, 0.55, 1.0, 1.0]);
        put(57, [0.20, 0.85, 0.95, 1.0]);
        for (i, c) in MODE_COLORS.iter().enumerate() {
            put(palette::MODES + i, *c);
        }
        put(palette::GHOST, GHOST_COLOR);
        data
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModeStats {
    pub mode: u8,
    pub time_s: f64,
    pub filament_mm: f64,
    pub max_z_rate: f32,
}

/// The plate's non-planar tops, for the panel.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NpSummary {
    /// Of the upward top area, the share printed non-planar.
    pub coverage: f64,
    pub max_slope_deg: f64,
    /// The least head clearance the slicer kept beyond each part's margin (mm).
    pub min_clearance: Option<f64>,
    /// Terrace height along the surface normal (mm), planar vs with the non-planar shells.
    pub terrace_p95_planar: f64,
    pub terrace_p95_np: f64,
    pub terrace_mean_planar: f64,
    pub terrace_mean_np: f64,
    /// The print check passes it (a measured head is installed); otherwise it previews only.
    pub printable: bool,
    /// Why it cannot print, when it cannot.
    pub note: String,
}

impl NpSummary {
    pub fn rows(&self) -> Vec<(String, String)> {
        let mut v = vec![
            ("Non-planar top".to_string(), format!("{:.0}% of the top · up to {:.1}°", 100.0 * self.coverage, self.max_slope_deg)),
            ("Terraces (p95)".to_string(), format!("{:.3} → {:.3} mm", self.terrace_p95_planar, self.terrace_p95_np)),
            ("Terraces (mean)".to_string(), format!("{:.3} → {:.3} mm", self.terrace_mean_planar, self.terrace_mean_np)),
        ];
        if let Some(c) = self.min_clearance { v.push(("Head clearance".into(), format!("{c:.2} mm over the margins"))); }
        if !self.printable { v.push(("Non-planar print".into(), if self.note.is_empty() { "Preview only: needs head measurements to print".into() } else { self.note.clone() })); }
        v
    }
}

/// A bead's `seg_k.y`: its nozzle (0, 1) and the plate part it belongs to, so the shader can
/// move one part's toolpaths while it is dragged (0: none, part `n`: `n + 1`).
pub fn bead_object(nozzle: u8, object: u16) -> f32 {
    let part = if object == u16::MAX { 0.0 } else { object as f32 + 1.0 };
    nozzle as f32 + 2.0 * part
}

/// The plate part a bead belongs to ([`bead_object`]), none for the plate's own moves and the
/// overlay kinds' tags.
pub fn bead_part(bead: &[f32]) -> Option<usize> {
    let part = (bead::tag(bead) / 2.0 + 0.001).floor();
    (part >= 1.0).then(|| part as usize - 1)
}

/// The contours of the plate as edited: per part slot, each layer's beads (the overlay kind,
/// on the part's layer), built once per content, place and layer. A changed layer
/// brings only its own beads (and the GPU its own buffer); `all` joins every layer by
/// reference, for the views that draw them all (the headset).
#[derive(Clone)]
pub struct Contours {
    /// Per part slot, per layer index: its beads (None: not traced yet).
    pub parts: Vec<Arc<[Option<RetainedInstances>]>>,
    pub all: RetainedInstances,
    /// The highest layer index with a contour.
    pub top_layer: u32,
}

impl Contours {
    /// Each part's beads of its layer `layer` (none where it has no contour there).
    pub fn layer(&self, layer: u32) -> impl Iterator<Item = &RetainedInstances> {
        self.parts.iter().filter_map(move |p| p.get(layer as usize)?.as_ref()).filter(|b| b.count() > 0)
    }
}

/// The newest contours of the plate as edited, drawn over the slice until its toolpaths come.
#[derive(Clone)]
pub struct LiveContours {
    pub revision: u64,
    pub contours: Contours,
}

/// A slice as it stands in the view: what [`PrintState::show_slice`] puts back.
pub struct ShownSlice {
    preview: Arc<PrintPreview>,
    last_settled: Option<Arc<PrintPreview>>,
    compare: Option<Arc<PrintPreview>>,
    mode_stats: String,
    contours: Option<LiveContours>,
    pub settled: bool,
    status: SliceStatus,
    unprinted: Vec<String>,
    print_check: Option<Result<String, String>>,
}

/// Playback: what of the preview the viewport shows.
#[derive(Clone, Debug)]
pub struct Playback {
    /// The viewport shows the preview (the Slice render mode).
    pub active: bool,
    pub time: f64,
    pub playing: bool,
    /// Print seconds per real second.
    pub speed: f64,
    /// Lowest and highest layer shown.
    pub layer_lo: u32,
    pub layer_hi: u32,
    pub color: ColorBy,
    pub travel: bool,
    /// The model's parts stay visible under the preview.
    pub show_model: bool,
    /// The parts drawn among the toolpaths (inserts, and the model's parts while they show) are
    /// cut at the top of the slider's layer while it leads; off, they are drawn whole.
    pub cut: bool,
    /// The supports are drawn (their toolpaths, the trees growing while they plan, the branch
    /// picked or hovered); off, the parts' own toolpaths alone, and no branch is picked.
    pub supports: bool,
    /// Non-planar tops shown as the planar terraces they replace (the before/after).
    pub compare_planar: bool,
}

impl Default for Playback {
    fn default() -> Self {
        Playback { active: false, time: 0.0, playing: false, speed: 60.0, layer_lo: 0, layer_hi: u32::MAX, color: ColorBy::Feature, travel: false, show_model: false, cut: false, supports: true, compare_planar: false }
    }
}

#[derive(Default)]
pub struct PrintState {
    /// The Slicer's choice: the ticked test prints instead of the whole plate.
    pub test_mode: bool,
    /// The whole plate's print time and grams, while its slice stands for the plate as it is.
    pub whole_figures: Option<(f64, f64)>,
    /// The live slice's print time ahead of its playing preview, shown where the preview has none
    /// yet ([`EstimateTotals`]).
    pub estimate: Option<EstimateTotals>,
    /// The live slice's playing preview while it grows ([`Self::grow_preview`]).
    pub growing: Option<Arc<PrintPreview>>,
    pub model_markers: Option<RetainedInstances>,
    pub hide_markers: bool,
    pub issue_spot: Option<IssueSpot>,
    /// The issue shown last, by its place in the report's issues: the Printability row of its
    /// kind says which of them it is and steps on from it.
    pub picked_issue: Option<usize>,
    /// The overhang shown last instead: the Slice panel's overhang row it is on.
    pub picked_overhang: Option<support_list::Picked>,
    pub printability: Arc<fab_printability::Report>,
    pub overhangs: bool,
    /// Sliced shows the printed surface instead of the toolpaths: the slice's outside walls and
    /// surfaces in their filament, up to the layer slider's layer (that layer whole), for every
    /// kind of part. The parts themselves are not drawn under it.
    pub finish: bool,
    pub settings: PrintSettings,
    /// The slicer settings as the profile gives them (key, value): what the panel shows where
    /// the project sets nothing, and what a global setting goes back to.
    pub base_values: std::collections::HashMap<String, String>,
    /// Bumped when [`Self::base_values`] change.
    pub base_revision: u64,
    pub profiles: PrintProfiles,
    pub status: SliceStatus,
    /// The parts of the plate being sliced that won't fit the selected printer (off its bed, or
    /// out of a nozzle's reach): they slice anyway, and this says so beside the slice.
    pub fit_warning: Option<String>,
    pub preview: Option<Arc<PrintPreview>>,
    /// The last settled slice with every experimental mode off.
    pub compare: Option<Arc<PrintPreview>>,
    /// The last finished slice: a streaming one replaces `preview` layer by layer, this stays
    /// to seed `compare` when the slice that follows has modes on.
    pub last_settled: Option<Arc<PrintPreview>>,
    pub showing_b: bool,
    /// Comparison text, formatted only when a settled preview changes.
    pub mode_stats: String,
    pub play: Playback,
    /// The plate changed since the shown slice.
    pub stale: bool,
    /// While `stale`: the parts (plate indices) whose toolpaths are out of date, the rest are
    /// as the plate is now. `None`: all of them (settings, parts added or removed).
    pub stale_parts: Option<Arc<[u16]>>,
    /// The shown preview is a slice streaming in (its out-of-date layers are drawn as ghosts).
    pub streaming: bool,
    /// The plate's contours as edited, while its toolpaths are on their way.
    pub live_contours: Option<LiveContours>,
    /// Where the last export went.
    pub exported: Option<std::path::PathBuf>,
    /// Bumped on every change the viewport must redraw for.
    pub revision: u64,
    pub printer: Option<Arc<fab_printer_link::Snapshot>>,
    /// The print colours on the printer (each one's position, its material, its warnings):
    /// made again on a printer report or a project change, never per frame.
    pub colors: colours::Mapping,
    /// The colours the last slice to land prints nothing in: their job tools print no path (a
    /// part's colour its paint covers all of), so the job leaves them out. Until a slice lands,
    /// every colour the plate names is the job's.
    pub unprinted: Vec<String>,
    /// Automatically picked colours moved to another head, because the shown plate's filament
    /// changes take less time there: kept until a better choice replaces them, the colour is
    /// saved to a tray, or the printer's heads change.
    pub head_moves: Vec<colours::Moved>,
    /// The Print button was pressed: the confirm card shows (nothing is sent yet) until Cancel,
    /// Start print or Stop.
    pub confirm_print: bool,
    /// Counts the presses of Print: each one brings the confirm card back into view.
    pub confirm_asked: u64,
    /// While the confirm card shows: what stops Start print now, whatever the slice is doing
    /// (the app's checks, the same ones Start print goes through).
    pub send_blocked: Option<String>,
    /// The confirm card's "Print anyway" tick: the slice (its preview id) whose printability
    /// errors the person accepted. Off on every opening of the card; a new slice is another id.
    pub accept_errors: Option<u64>,
    /// The job whose package is being made for the printer (Start print was pressed).
    pub sending: Option<String>,
    /// Stop was pressed: the confirm row shows.
    pub confirm_stop: bool,
    pub print_options: fab_printer_link::PrintOptions,
    /// The print safety check of the shown slice (Ok: what passed; Err: why it is refused).
    /// Start print is offered only after a pass.
    pub print_check: Option<Result<String, String>>,
    /// Why the last Print could not go out, said at the top of the panel.
    pub print_blocked: Option<String>,
    /// The layer the slice view looks at (the layer slider's top): a live
    /// slicer slices around it first.
    pub focus_layer: Option<u32>,
    /// The shown slice is final (a settled generation). A live, still
    /// refining result is false: Print and Export wait for it.
    pub settled: bool,
    /// The shown slice's printability as it streams in after the slice (the sliced checks,
    /// layer by layer): it supersedes the preview's early report until the next slice.
    pub streamed: Option<StreamedIssues>,
    /// Monitor following the printer's job (`follow`).
    pub follow: follow::Follow,
    /// The Calibrate panel (the app fills it).
    pub calibrate: calibrate::CalibrateState,
    /// The committed slice's overhangs, worst unheld area first.
    pub overhang_list: Option<fab_printability::OverhangList>,
    /// All support removal results for the current shape and settings, once checked.
    pub support_pieces: Option<Vec<crate::api::MechPieceView>>,
    /// The plate's layers when its printer is resin: the Slice render mode shows them in place
    /// of toolpaths (the app cuts them; nothing slices for filament then).
    pub resin: Option<resin::ResinShow>,
    /// The resin layers shown upright, the plate at the bottom; else the stack hangs from the
    /// plate on top, the way it prints (the camera turns over while they show).
    pub upright: bool,
    /// The job on the linked resin printer, from Start print until its card is closed (it
    /// outlives the plate's printer: a print goes on whatever the plate).
    pub resin_job: Option<resin::ResinJob>,
    /// The trees of the plate's supports as the live slice has them now: what the Sliced view
    /// finds branches of under the pointer, picks and deletes.
    pub trees: Arc<[PlateTree]>,
    /// Counts the lists [`Self::trees`] was given: a place in them is a place in that one.
    pub trees_given: u64,
    /// The branch picked in the Sliced view, by its part's object and its tips (it is gone once
    /// the trees are planned without it): Delete takes them out of the part's supports.
    pub picked_branch: Option<(String, Vec<[f64; 3]>)>,
}

/// A slice's printability so far: `done` of `total` layers checked.
pub struct StreamedIssues {
    pub report: Arc<fab_printability::Report>,
    pub markers: RetainedInstances,
    pub done: u32,
    pub total: u32,
}

impl StreamedIssues {
    pub fn new(report: Arc<fab_printability::Report>, done: u32, total: u32) -> StreamedIssues {
        StreamedIssues { markers: issue_markers(&report), report, done, total }
    }

    /// Still checking: `(done, total)`.
    pub fn checking(&self) -> Option<(u32, u32)> {
        (self.done < self.total).then_some((self.done, self.total))
    }
}

impl PrintState {
    /// The slice in a word and a fraction: `Slicing` with its progress, `Failed`, `Sliced` while
    /// the shown slice is current, `Waiting` when the Slice view is open and the slice is not
    /// there yet, `Off` when nothing asks for one (Sliced, Monitor, Print and Export each slice
    /// when they need to).
    pub fn slice_state(&self) -> (&'static str, f32) {
        match &self.status {
            SliceStatus::Running { progress, .. } => ("Slicing", *progress),
            SliceStatus::Failed(_) => ("Failed", 0.0),
            _ if self.preview.is_some() && !self.stale => ("Sliced", 1.0),
            _ if self.play.active => ("Waiting", 0.0),
            _ => ("Off", 0.0),
        }
    }

    /// The picked branch while the Sliced view shows the plate's current slice with its supports,
    /// as (its tree's place in [`Self::trees`], its lowest disc): what Delete takes out.
    pub fn picked_branch(&self) -> Option<(usize, u32)> {
        let (object, tips) = self.picked_branch.as_ref()?;
        self.branch_of(object, tips)
    }

    /// The branch of the newest trees that carries exactly `tips` on `object` (how a branch is
    /// told apart from one list of trees to the next), while the Sliced view shows the plate's
    /// current slice with its supports: (its tree's place in [`Self::trees`], its lowest disc).
    pub fn branch_of(&self, object: &str, tips: &[[f64; 3]]) -> Option<(usize, u32)> {
        (self.play.active && self.play.supports && !self.stale).then_some(())?;
        let first = tips.first()?;
        self.trees.iter().enumerate().filter(|(_, t)| t.object == object && t.tips.contains(first)).find_map(|(i, t)| Some((i, t.branch_of(tips)?)))
    }

    /// Print and Export take a settled, current slice only.
    pub fn can_send(&self) -> bool {
        self.preview.is_some() && !self.stale && self.settled && !matches!(self.status, SliceStatus::Running { .. })
    }

    /// Why the shown slice cannot start printing. Its printability errors stop it until the person
    /// ticks "Print anyway" for this slice; what [`Self::firm_blocker`] says always does.
    pub fn print_blocker(&self) -> Option<String> {
        self.firm_blocker().or_else(|| if self.errors_accepted() { None } else { self.errors_text() })
    }

    /// What stops the shown slice whatever the person ticks: a used colour with no tray, the
    /// printability check still running.
    pub fn firm_blocker(&self) -> Option<String> {
        // A used print colour with no tray would silently print with the first material's spool.
        if self.printer.is_some() {
            let mut open = self.colors.rows.iter().filter(|r| r.used && r.unassigned());
            if let Some(first) = open.next() {
                let more = open.count();
                let more = if more > 0 { format!(" (and {more} more colours)") } else { String::new() };
                return Some(format!("Pick a tray for print colour {} ({}){more}: with none it prints with the first material's spool", first.name, colours::colour_word(first.design)));
            }
        }
        if !self.stale && self.preview.is_some() {
            if let Some((done, total)) = self.streamed.as_ref().and_then(StreamedIssues::checking) {
                return Some(format!("Checking printability: layer {done} of {total}"));
            }
        }
        None
    }

    /// The printability errors in a sentence: the first, where it is, how many more.
    pub fn errors_text(&self) -> Option<String> {
        let errors = self.print_errors();
        let first = &self.report().issues[*errors.first()?];
        let at = first.layer.map(|l| format!(" on layer {}", l + 1)).unwrap_or_default();
        let more = if errors.len() > 1 { format!(" (and {} more)", errors.len() - 1) } else { String::new() };
        Some(format!("Not printable: {}{at}{more}. {}", first.message, first.hint))
    }

    /// The person ticked "Print anyway" for the slice shown, and it is still the one shown.
    pub fn errors_accepted(&self) -> bool {
        !self.stale && self.accept_errors.is_some() && self.accept_errors == self.preview.as_ref().map(|p| p.id)
    }

    /// What a job started now records of the printability errors it goes with: per kind, its
    /// first layer and how many. Empty unless they were accepted.
    pub fn accepted_errors(&self) -> Vec<jobs::Accepted> {
        let mut out: Vec<jobs::Accepted> = Vec::new();
        if !self.errors_accepted() {
            return out;
        }
        let issues = &self.report().issues;
        for issue in self.print_errors().into_iter().map(|i| &issues[i]) {
            match out.iter_mut().find(|a| a.kind == issue.kind) {
                Some(a) => {
                    a.count += 1;
                    a.layer = match (a.layer, issue.layer) { (Some(a), Some(b)) => Some(a.min(b)), (a, b) => a.or(b) };
                }
                None => out.push(jobs::Accepted { kind: issue.kind.to_string(), layer: issue.layer, count: 1 }),
            }
        }
        out
    }

    /// The printability errors that refuse the print: their places in the report's issues.
    pub fn print_errors(&self) -> Vec<usize> {
        self.report().issues.iter().enumerate().filter(|(_, i)| i.severity == fab_printability::Severity::Error).map(|(at, _)| at).collect()
    }
}

impl PrintState {
    pub fn report(&self) -> &Arc<fab_printability::Report> {
        if !self.stale {
            if let (Some(_), Some(s)) = (&self.preview, &self.streamed) { return &s.report; }
            if let Some(p) = &self.preview { return &p.printability; }
        }
        &self.printability
    }

    pub fn markers(&self) -> Option<&RetainedInstances> {
        if self.hide_markers || self.monitor_holds() { return None; }
        if !self.stale {
            if let (Some(_), Some(s)) = (&self.preview, &self.streamed) { return Some(&s.markers); }
            if let Some(p) = &self.preview { return Some(&p.markers); }
        }
        self.model_markers.as_ref()
    }

    /// The slice the view shows: the last one, also while a newer one is on
    /// its way (the view dims it then).
    pub fn showing(&self) -> Option<&Arc<PrintPreview>> {
        if self.monitor_holds() {
            return self.monitor_preview();
        }
        // a resin printer's layers show in place of any slice
        if self.play.active && self.resin.is_none() {
            self.played_growing().or_else(|| self.selected_preview())
        } else {
            None
        }
    }

    /// The preview growing ([`Self::grow_preview`]) while it plays, or the playhead stands in it
    /// (a scrub, a pause): at the end the plate's slice shows whole.
    fn played_growing(&self) -> Option<&Arc<PrintPreview>> {
        self.growing.as_ref().filter(|g| !self.showing_b && (self.play.playing || self.play.time < g.total))
    }

    pub fn bump(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// The resin layers the view shows: in the Slice render mode on a resin printer.
    pub fn resin_shown(&self) -> Option<&resin::ResinShow> {
        self.resin.as_ref().filter(|_| self.play.active && !self.monitor_holds())
    }

    /// The view hangs the plate on top: resin layers show and not upright.
    pub fn resin_hangs(&self) -> bool {
        self.resin_shown().is_some() && !self.upright
    }

    /// The manually selected A/B result, independent of the workspace.
    fn selected_preview(&self) -> Option<&Arc<PrintPreview>> {
        if self.showing_b { self.compare.as_ref().or(self.preview.as_ref()) } else { self.preview.as_ref() }
    }

    /// The slice playback runs over: Monitor's job, or the selected one.
    fn played_preview(&self) -> Option<&Arc<PrintPreview>> {
        self.monitor_preview().or_else(|| self.growing.as_ref().filter(|_| !self.showing_b)).or_else(|| self.selected_preview())
    }

    /// The contours of the plate as edited, over the plate's own slice.
    pub fn contours(&self) -> Option<&LiveContours> {
        self.live_contours.as_ref().filter(|_| !self.monitor_holds())
    }

    /// The views a new slice of the plate moves: the other workspaces' while Monitor shows, and
    /// Monitor's unless a job holds it.
    fn slice_views(&mut self) -> impl Iterator<Item = &mut Playback> + '_ {
        let held = self.monitor_holds();
        let monitor = self.follow.monitor;
        [(!held).then_some(&mut self.play), monitor.then_some(&mut self.follow.hidden_play)].into_iter().flatten()
    }

    /// Whether the live slice's print time stands ahead of its playing preview ([`EstimateTotals`]):
    /// the playback waits for the preview, and says so.
    pub fn making_preview(&self) -> bool {
        self.estimate.is_some() && self.growing.is_none()
    }

    /// A slice of the whole plate tells what the test prints stand next to.
    fn note_figures(&mut self, preview: &PrintPreview) {
        if preview.pieces.is_empty() {
            self.whole_figures = Some((preview.total, preview.filament_use.iter().map(|f| f.grams).sum()));
        }
    }

    /// The slice as it stands in the view, when one does: whole, and of what's on the plate.
    pub fn shown_slice(&self) -> Option<ShownSlice> {
        if self.stale || matches!(self.status, SliceStatus::Running { .. }) {
            return None;
        }
        Some(ShownSlice {
            preview: self.preview.clone()?,
            last_settled: self.last_settled.clone(),
            compare: self.compare.clone(),
            mode_stats: self.mode_stats.clone(),
            contours: self.live_contours.clone(),
            settled: self.settled,
            status: self.status.clone(),
            unprinted: self.unprinted.clone(),
            print_check: self.print_check.clone(),
        })
    }

    /// A slice brought back as it stood, of the engine's input `revision`: nothing slices.
    pub fn show_slice(&mut self, shown: ShownSlice, revision: u64) {
        self.keep_layer(&shown.preview);
        self.last_settled = shown.last_settled;
        self.compare = shown.compare;
        self.mode_stats = shown.mode_stats;
        self.preview = Some(shown.preview);
        self.live_contours = shown.contours.map(|c| LiveContours { revision, contours: c.contours });
        self.settled = shown.settled;
        self.status = shown.status;
        self.unprinted = shown.unprinted;
        self.print_check = shown.print_check;
        self.exported = None;
        self.streamed = None;
        self.stale = false;
        self.stale_parts = None;
        self.streaming = false;
        self.bump();
    }

    /// A fresh, settled generation: show it whole, from the start of the top layer.
    /// Live/partial previews must not be published through this method.
    pub fn set_preview(&mut self, preview: Arc<PrintPreview>) {
        self.note_figures(&preview);
        self.estimate = None;
        self.growing = None;
        if preview.modes.iter().any(|m| m.mode != 0) {
            if let Some(previous) = self.last_settled.as_ref().filter(|p| p.modes.iter().all(|m| m.mode == 0)) {
                self.compare = Some(previous.clone());
            }
        } else {
            self.compare = None;
        }
        self.showing_b = false;
        self.mode_stats.clear();
        if let Some(off) = &self.compare {
            let seconds = (preview.total - off.total).round() as i64;
            let sign = if seconds < 0 { "−" } else { "+" };
            let seconds = seconds.unsigned_abs();
            let time = format!("{sign}{}m{:02}s", seconds / 60, seconds % 60);
            // Reuse the summary's per-slot mass (length × area × that slot's density).
            let grams = preview.filament_use.iter().map(|f| f.grams).sum::<f64>()
                - off.filament_use.iter().map(|f| f.grams).sum::<f64>();
            self.mode_stats = format!("Modes on vs off: {time}, {grams:+.1} g, {} → {} layers", off.layers.len(), preview.layers.len());
            if preview.max_z_rate > 0.0 {
                self.mode_stats.push_str(&format!(", max Z rate {:.1} mm/s", preview.max_z_rate));
            }
        }
        self.keep_layer(&preview);
        // The render mode is the person's: a new slice never pulls the model views into it.
        self.last_settled = Some(preview.clone());
        self.preview = Some(preview);
        self.streamed = None;
        self.stale = false;
        self.stale_parts = None;
        self.streaming = false;
        self.settled = true;
        self.live_contours = None;
        self.bump();
    }

    /// The layers the view looks at stay across a reslice: the top one when it looked at the
    /// whole print, else the same layer, or the new top one if the print got lower.
    fn keep_layer(&mut self, preview: &PrintPreview) {
        let top = preview.max_layer();
        let old_top = self.preview.as_ref().map(|p| p.max_layer());
        for play in self.slice_views() {
            if old_top.is_none_or(|t| play.layer_hi >= t) {
                play.layer_hi = top;
                play.time = preview.total;
            } else {
                play.layer_hi = play.layer_hi.min(top);
                play.layer_lo = play.layer_lo.min(play.layer_hi);
                play.time = preview.layers.iter().filter(|l| l.layer <= play.layer_hi).map(|l| l.t_end).fold(0.0, f64::max);
            }
            play.playing = false;
        }
        self.focus_layer = Some(self.play.layer_hi);
    }

    /// The whole live slice with the planner's times (no G-code text): it plays and follows the
    /// printer like a settled slice, and Print and Export still settle it. The live slice's
    /// issues stay with it ([`Self::live_issues`]).
    pub fn time_preview(&mut self, preview: Arc<PrintPreview>) {
        self.note_figures(&preview);
        self.estimate = None;
        let issues = match self.streamed.take() {
            _ if self.stale => None,
            Some(newest) => Some(newest),
            None => self.preview.as_ref().map(|p| StreamedIssues { report: p.printability.clone(), markers: p.markers.clone(), done: 0, total: 0 }),
        };
        // Played while it grew, it plays on from where it is, up to its top if it went to the top
        // grown.
        match self.growing.take().filter(|g| self.play.playing || self.play.time < g.total) {
            Some(grown) if self.play.layer_hi >= grown.max_layer() => self.play.layer_hi = preview.max_layer(),
            Some(_) => {}
            None => self.keep_layer(&preview),
        }
        self.preview = Some(preview);
        self.streamed = issues;
        self.stale = false;
        self.stale_parts = None;
        self.streaming = false;
        self.bump();
    }

    /// The live slice's playing preview as far as it has grown ([`PrintPreview::grown`]), its
    /// print time and summary the kept estimate's: it plays and scrubs up to where it has grown,
    /// and the plate's slice stays shown whole at the end, until the whole preview comes
    /// ([`Self::time_preview`]).
    pub fn grow_preview(&mut self, preview: Arc<PrintPreview>) {
        if self.growing.is_none() {
            // The view stands at the end: playing starts it over.
            self.play.time = preview.total;
            self.play.playing = false;
        }
        self.growing = Some(preview);
        self.bump();
    }

    /// The live slice's newest issues: its checks stream on after the tiles that carried the last
    /// ones, and the planner's preview ([`Self::time_preview`]) brings none of its own.
    pub fn live_issues(&mut self, report: &Arc<fab_printability::Report>, markers: &RetainedInstances) {
        if self.streamed.as_ref().is_some_and(|s| Arc::ptr_eq(&s.report, report)) {
            return;
        }
        self.streamed = Some(StreamedIssues { report: report.clone(), markers: markers.clone(), done: 0, total: 0 });
        self.bump();
    }

    /// Live tiles replace the shown slice as they arrive; the finished slice comes with
    /// [`Self::set_preview`].
    pub fn stream_preview(&mut self, preview: Arc<PrintPreview>) {
        // The slider is the person's: a live preview never pulls it down (a partial one has few
        // layers yet). Only a slider at the top of the stack (or none yet) follows a taller part up.
        let old_top = self.preview.as_ref().map(|p| p.max_layer());
        for play in self.slice_views() {
            if old_top.is_none_or(|t| play.layer_hi >= t) && preview.max_layer() > play.layer_hi {
                play.layer_hi = preview.max_layer();
            }
            play.time = preview.total;
            play.playing = false;
        }
        self.focus_layer = Some(self.play.layer_hi);
        self.preview = Some(preview);
        self.streamed = None;
        self.stale = false;
        self.stale_parts = None;
        self.streaming = true;
        self.settled = false;
        self.bump();
    }
}

/// Everything the Print editor asks for. Settings and Slice/Export are the
/// app's (it owns the slicer); playback is [`apply`]'s.
#[derive(Clone, Debug)]
pub enum PrintAction {
    /// Whole plate (false) or the ticked test prints (true): what the Slicer shows, slices and prints.
    TestMode(bool),
    Overhangs(bool),
    /// Sliced: the printed surface (true) or the toolpaths.
    PrintedSurface(bool),
    Markers(bool),
    SelectIssue(usize),
    /// Show an overhang the Slice panel lists (the app frames it in view): one patch the way a
    /// selected issue is shown, the layers up to the one it starts on and framed there for a
    /// moment; a whole cluster is only framed.
    PickOverhang(support_list::Picked),
    OpenSupportCheck,
    Slice,
    Export,
    UseInstalled(bool),
    SetMachine(String),
    SetPlate(String),
    SetProcess(String),
    // ---- playback ----
    Preview(bool),
    Play(bool),
    Seek(f64),
    /// Move the playhead without changing the shown layers (the move slider).
    Scrub(f64),
    SetSpeed(f64),
    /// Show layers up to this one (the time moves to its end).
    SetTopLayer(u32),
    SetBottomLayer(u32),
    SetColorBy(ColorBy),
    ShowTravel(bool),
    /// Draw the supports, or the parts' own toolpaths alone (a view's choice: the slice and its
    /// G-code stay as they are).
    ShowSupports(bool),
    /// Keep the model's parts visible under the preview (the app swaps the
    /// scene; this only records the choice).
    ShowModel(bool),
    /// Cut the parts drawn among the toolpaths at the shown layer (`Playback::cut`).
    CutAtLayer(bool),
    /// Show non-planar tops as the planar terraces they replace.
    ComparePlanar(bool),
    /// Show the retained modes-off slice (B), or the current slice (A).
    Compare(bool),
    /// Resin layers upright, the plate at the bottom (else they hang from it).
    ResinUpright(bool),
    // ---- the linked printer ----
    /// Show (or close) the confirm row of Print / Stop. Nothing is sent.
    AskPrint(bool),
    AskStop(bool),
    SetPrintOption(PrintOption, bool),
    /// The confirm card's "Print anyway" tick: Start print goes with the shown slice's
    /// printability errors.
    AcceptErrors(bool),
    /// The USB stick a resin print is saved to, by its root folder.
    PickStick(std::path::PathBuf),
    /// The resin printer's orientation test: saved to the stick (the app's).
    SaveOrientationTest,
    /// Confirmed: upload the sliced plate and start it (the app's).
    SendToPrinter,
    /// Cancel while the package is being made: nothing goes to the printer.
    CancelSend,
    /// Monitor's list: show this job (None: the printer's, or the last).
    PickJob(Option<String>),
    /// A queued job's Start: ask about the plate first (None: not now).
    AskStartJob(Option<String>),
    /// Start this queued job now: the plate is clear.
    StartJob(String),
    /// Take this queued job out of the queue, off the disk too.
    RemoveJob(String),
    /// The job is over and read: its card goes.
    CloseJob,
    /// Pause, resume, stop (confirmed), clear the error, list the storage (the app's).
    PrinterControl(PrinterControl),
    /// Monitor follows the printer's job (on by default); manual playback turns it off.
    FollowPrinter(bool),
    /// Set what a tray of the offline loadout holds (the app's; a reporting printer's trays
    /// are its reports).
    EditTray(colours::Position, colours::TrayEdit),
    /// Pick a branch of the shown slice's supports (its tree's place in [`PrintState::trees`],
    /// its lowest disc), or none.
    PickBranch(Option<(usize, u32)>),
}

impl PrintAction {
    /// A step of the playhead (a layer or a time): it changes what the slice view, its layer
    /// slider and its transport show ([`crate::ui::stage::PLAYHEAD`]), nothing in the panels.
    pub fn moves_playhead(&self) -> bool {
        matches!(self, PrintAction::Seek(_) | PrintAction::Scrub(_) | PrintAction::SetTopLayer(_) | PrintAction::SetBottomLayer(_))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrintOption { FlowCalibration, Timelapse, PlateClear }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrinterControl { Pause, Resume, Stop, ClearError, CheckStorage }

/// The playback half of [`PrintAction`]. Returns true when the view changed.
pub fn apply(state: &mut PrintState, action: &PrintAction) -> bool {
    if let PrintAction::PickBranch(at) = action {
        let picked = at.and_then(|(i, low)| Some((state.trees.get(i)?, low))).filter(|(t, low)| (*low as usize) < t.discs.len())
            .map(|(t, low)| (t.object.clone(), t.branch_tips(low))).filter(|(_, tips)| !tips.is_empty());
        if picked == state.picked_branch {
            return false;
        }
        state.picked_branch = picked;
        state.bump();
        return true;
    }
    if let PrintAction::SelectIssue(i) = action {
        let Some(issue) = state.report().issues.get(*i) else { return false };
        let layer = issue.layer;
        let z = layer.and_then(|l| state.preview.as_ref()?.layers.iter().find(|p| p.layer == l).map(|p| p.z))
            .unwrap_or(issue.at_mm[2] as f32);
        let spot = IssueSpot::new(issue, z, Cx::time_now());
        // A mesh check's issue has no layer: the slice's layers show up to the one its height
        // prints on, so stepping from an issue higher up does not leave it hidden.
        let layer = layer.or_else(|| {
            let layers = &state.preview.as_ref()?.layers;
            layers.iter().find(|l| l.z as f64 >= issue.at_mm[2] - 1e-6).or(layers.last()).map(|l| l.layer)
        });
        state.issue_spot = Some(spot);
        state.picked_issue = Some(*i);
        state.picked_overhang = None;
        state.hide_markers = false;
        state.showing_b = false;
        if let Some(layer) = layer {
            // a slice's layer, or a resin printer's
            state.play.active = state.preview.is_some() || state.resin.is_some();
            apply(state, &PrintAction::SetTopLayer(layer));
        }
        state.bump();
        return true;
    }
    if let PrintAction::PickOverhang(pick) = action {
        let Some(shown) = support_list::picked_line(state, pick) else { return false };
        let (min, max, cluster) = (shown.min, shown.max, !shown.members.is_empty());
        state.picked_overhang = Some(*pick);
        state.picked_issue = None;
        if cluster {
            state.bump();
            return true;
        }
        // the layers shown up to the one it starts on, the frame on that layer
        let layer = state.preview.as_ref().and_then(|p| p.layers.iter().find(|l| l.z as f64 >= min[2] - 1e-6)).map(|l| (l.layer, l.z));
        let z = layer.map_or(min[2] as f32, |(_, z)| z);
        let spot = IssueSpot::framing([min[0], min[1]], [max[0], max[1]], z, layer.map(|(l, _)| l), fab_printability::Severity::Warning, Cx::time_now());
        state.issue_spot = Some(spot);
        state.showing_b = false;
        if let Some((layer, _)) = layer {
            state.play.active = true;
            apply(state, &PrintAction::SetTopLayer(layer));
        }
        state.bump();
        return true;
    }

    if let PrintAction::FollowPrinter(on) = action {
        state.follow.on = *on;
        state.bump();
        follow::place(state, Cx::time_now());
        resin::follow(state);
        return true;
    }
    if (follow::manual(action) || matches!(action, PrintAction::Compare(_))) && (state.following().is_some() || state.resin_follows().is_some()) {
        state.follow.on = false;
    }
    let focus_before = state.focus_layer;
    let changed = apply_play(state, action);
    if changed {
        // a resin printer's layers show in place of any slice: the focus is their top one
        if state.resin_shown().is_some() {
            state.focus_layer = Some(state.play.layer_hi);
        } else if let Some(pv) = state.showing().or_else(|| state.played_preview()) {
            let at = pv.layers.get(pv.layer_at(state.play.time)).map(|l| l.layer).unwrap_or(0);
            state.focus_layer = Some(at.min(state.play.layer_hi));
        }
        if state.focus_layer != focus_before {
            state.bump();
        }
    }
    changed
}

fn apply_play(state: &mut PrintState, action: &PrintAction) -> bool {
    if let PrintAction::Compare(off) = action {
        if (*off && state.compare.is_none()) || state.monitor_holds() { return false; }
        state.showing_b = *off;
        if let Some(pv) = state.selected_preview().cloned() {
            state.play.time = pv.total;
            state.play.layer_lo = 0;
            state.play.layer_hi = pv.max_layer();
            state.play.playing = false;
        }
        state.bump();
        return true;
    }
    // The print time ahead of its playing preview: nothing plays or scrubs until the preview comes.
    if state.making_preview() && matches!(action, PrintAction::Play(true) | PrintAction::Seek(_) | PrintAction::Scrub(_)) {
        return false;
    }
    let preview = state.played_preview().cloned();
    // While a preview grows, the view at rest shows the plate's whole slice, and the slider walks
    // its layers ([`PrintState::grow_preview`]).
    let whole_top = preview.as_ref().filter(|pv| pv.grown.is_some()).and(state.selected_preview()).map(|pv| pv.max_layer());
    let resin_layers = state.resin_shown().map(|r| r.count);
    let p = &mut state.play;
    match action {
        PrintAction::Markers(on) => state.hide_markers = !*on,
        PrintAction::ResinUpright(on) => state.upright = *on,
        PrintAction::PrintedSurface(on) => {
            state.finish = *on;
            crate::view_prefs::save_surface(*on);
        }
        PrintAction::Overhangs(on) => {
            state.overhangs = *on;
            if *on { p.active = false; p.playing = false; }
        }
        PrintAction::Preview(on) => {
            p.active = *on;
            if !*on { p.playing = false; }
        }
        PrintAction::Play(on) => {
            p.playing = *on;
            if *on {
                p.active = true;
                // (a resin printer's layers, shown in place of the slice, are not the slice's)
                if let (Some(pv), None) = (&preview, resin_layers) {
                    // The playhead is in (or past) the chosen top layer: the
                    // print carries on upwards from there.
                    let hi_start = pv.layers.iter().find(|l| l.layer == p.layer_hi).map(|l| l.t_start).unwrap_or(pv.total);
                    if p.layer_hi < pv.max_layer() && p.time >= hi_start - 1e-6 {
                        p.layer_hi = pv.max_layer();
                    }
                    // Playing from the end starts over.
                    if p.time >= pv.total - 1e-6 {
                        p.time = pv.layers.iter().find(|l| l.layer >= p.layer_lo).map(|l| l.t_start).unwrap_or(0.0);
                    }
                }
            }
        }
        PrintAction::Seek(t) => {
            let total = preview.as_ref().map(|p| p.grown.unwrap_or(p.total)).unwrap_or(0.0);
            p.time = t.clamp(0.0, total);
            p.active = preview.is_some();
            // Scrubbing the time shows everything printed by then (of the slice, not of the resin
            // layers shown in its place).
            if let (Some(pv), None) = (&preview, resin_layers) {
                p.layer_hi = pv.max_layer();
            }
        }
        PrintAction::Scrub(t) => {
            let total = preview.as_ref().map(|p| p.grown.unwrap_or(p.total)).unwrap_or(0.0);
            p.time = t.clamp(0.0, total);
            p.playing = false;
        }
        PrintAction::SetSpeed(s) => p.speed = s.max(0.1),
        // the layers shown: a resin printer's in place of any slice (as the slider walks them)
        PrintAction::SetTopLayer(l) => {
            if let Some(count) = resin_layers {
                let l = (*l).min(count.saturating_sub(1));
                p.layer_hi = l;
                p.layer_lo = p.layer_lo.min(l);
            } else if let Some(pv) = &preview {
                let l = (*l).min(whole_top.unwrap_or(0).max(pv.max_layer()));
                p.layer_hi = l;
                p.layer_lo = p.layer_lo.min(l);
                // A layer the growing preview has not reached shows in the whole slice, the
                // playhead at the end; the whole preview keeps it when it comes.
                p.time = if l > pv.max_layer() { pv.total } else { pv.layers.iter().filter(|x| x.layer <= l).map(|x| x.t_end).fold(0.0, f64::max) };
                p.playing = false;
            }
        }
        PrintAction::SetBottomLayer(l) => {
            p.layer_lo = *l;
            p.layer_hi = p.layer_hi.max(*l);
        }
        PrintAction::SetColorBy(c) => p.color = *c,
        PrintAction::ShowTravel(on) => p.travel = *on,
        PrintAction::ShowSupports(on) => p.supports = *on,
        PrintAction::ShowModel(on) => p.show_model = *on,
        PrintAction::CutAtLayer(on) => p.cut = *on,
        PrintAction::ComparePlanar(on) => p.compare_planar = *on,
        PrintAction::AskPrint(on) => {
            state.confirm_stop = false;
            if *on {
                // Already open: it stays as it is (a tick on the plate check too), back in view.
                if !state.confirm_print {
                    state.print_options.plate_clear = false;
                    state.accept_errors = None;
                    if let Some(r) = state.resin.as_mut() {
                        r.accepted = None;
                    }
                }
                state.print_blocked = None;
                state.confirm_asked += 1;
            }
            state.confirm_print = *on;
        }
        PrintAction::AskStop(on) => { state.confirm_stop = *on; state.confirm_print = false; }
        // a resin printer's tick is for its stack
        PrintAction::AcceptErrors(on) if state.resin.is_some() => {
            let r = state.resin.as_mut().unwrap();
            r.accepted = on.then_some(r.stack);
        }
        PrintAction::AcceptErrors(on) => state.accept_errors = state.preview.as_ref().filter(|_| *on && !state.stale).map(|p| p.id),
        PrintAction::PickStick(root) => {
            if let Some(r) = state.resin.as_mut() {
                r.stick = Some(root.clone());
            }
        }
        PrintAction::SaveOrientationTest => {
            if let Some(r) = state.resin.as_mut() {
                r.test = resin::TestFile::Saving { stick: None };
            }
        }
        PrintAction::SetPrintOption(option, on) => match option {
            PrintOption::FlowCalibration => state.print_options.flow_calibration = *on,
            PrintOption::Timelapse => state.print_options.timelapse = *on,
            PrintOption::PlateClear => state.print_options.plate_clear = *on,
        },
        PrintAction::SendToPrinter => { state.confirm_print = false; state.print_options.plate_clear = false; }
        PrintAction::CancelSend => {}
        PrintAction::CloseJob => {
            let held = state.monitor_holds();
            state.print_blocked = None;
            state.follow.picked = None;
            state.follow.confirm_start = None;
            state.follow.closed = true;
            if held && !state.monitor_holds() {
                state.release_monitor();
            }
            follow::place(state, Cx::time_now());
        }
        PrintAction::PickJob(id) => {
            let held = state.monitor_holds();
            state.follow.picked = id.clone();
            state.follow.confirm_start = None;
            state.follow.closed = false;
            state.follow.on = true;
            if held && !state.monitor_holds() {
                state.release_monitor();
            }
            follow::place(state, Cx::time_now());
        }
        PrintAction::AskStartJob(id) => state.follow.confirm_start = id.clone(),
        PrintAction::StartJob(_) => state.follow.confirm_start = None,
        PrintAction::RemoveJob(_) => {}
        PrintAction::PrinterControl(PrinterControl::Stop) => state.confirm_stop = false,
        _ => return false,
    }
    state.bump();
    true
}

/// The playback clock: advance by `dt` real seconds while playing. Returns
/// true while it keeps playing (the caller asks for the next frame).
pub fn tick(state: &mut PrintState, dt: f64) -> bool {
    if !state.play.playing || !state.play.active {
        return false;
    }
    let Some(pv) = state.played_preview().cloned() else {
        state.play.playing = false;
        state.bump();
        return false;
    };
    let was_playing = state.play.playing;
    let layer = timeline::shown_top_layer(state, &pv);
    let dt = dt.clamp(0.0, 0.1);
    let mut t = state.play.time + dt * state.play.speed;
    // Played past the top layer: stop there.
    let end = if state.play.layer_hi >= pv.max_layer() {
        pv.total
    } else {
        pv.layers.iter().filter(|l| l.layer <= state.play.layer_hi).map(|l| l.t_end).fold(0.0, f64::max)
    };
    // A preview growing plays to where it has grown and waits there for the rest.
    match pv.grown.filter(|g| *g < end) {
        Some(grown) => t = t.min(grown),
        None if t >= end => {
            t = end;
            state.play.playing = false;
        }
        None => {}
    }
    state.play.time = t;
    // Layer changes and stopping affect panels; in-layer motion only affects playback.
    if state.play.playing != was_playing || timeline::shown_top_layer(state, &pv) != layer {
        state.bump();
    }
    state.play.playing
}

/// `h:mm:ss` (or `m:ss`).
pub fn format_time(s: f64) -> String {
    let s = s.max(0.0).round() as u64;
    let (h, m, sec) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m}:{sec:02}")
    }
}

pub fn script_mod(vm: &mut ScriptVm) {
    trays::script_mod(vm);
    colour_map::script_mod(vm);
    timeline::script_mod(vm);
    support_list::script_mod(vm);
    job_card::script_mod(vm);
    job_stats::script_mod(vm);
    job_list::script_mod(vm);
    panel::script_mod(vm);
    calibrate::script_mod(vm);
    resin::script_mod(vm);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hover line names the printing nozzle and every nozzle's temperature: a two-nozzle head
    /// by side, numbered heads each by name.
    #[test]
    fn the_line_of_the_head_names_its_own_nozzles() {
        let two = PrintNow { doing: "Travel".into(), active: 1, head: 1, nozzle_temps: vec![200, 0], ..Default::default() };
        assert_eq!((two.doing_text(), two.temps_text()), ("Travel on the right nozzle".into(), "nozzles 200 / 0 °C".into()));
        let four = PrintNow { doing: "Outer wall".into(), active: 1, head: 2, nozzle_temps: vec![0, 0, 215, 170], ..Default::default() };
        assert_eq!(four.doing_text(), "Outer wall on Tool 3");
        assert_eq!(four.temps_text(), "Tool 1 0 °C  ·  Tool 2 0 °C  ·  Tool 3 215 °C  ·  Tool 4 170 °C");
    }

    /// The panel reads the report: a live slice's newest issues, also after the planner's preview
    /// replaces the one that carried older ones.
    #[test]
    fn the_newest_issues_outlive_the_preview_they_came_after() {
        let mut state = PrintState::default();
        state.stream_preview(preview());
        let newest = Arc::new(fab_printability::Report::default());
        let markers = RetainedInstances::new(PREVIEW_SLOTS, Vec::<f32>::new().into()).unwrap();
        state.live_issues(&newest, &markers);
        assert!(Arc::ptr_eq(state.report(), &newest));
        state.time_preview(preview());
        assert!(Arc::ptr_eq(state.report(), &newest), "the timed preview keeps them");
    }

    struct Still;
    impl PrintTimeline for Still {
        fn now(&self, _t: f64) -> PrintNow {
            PrintNow::default()
        }
    }

    fn preview() -> Arc<PrintPreview> {
        // Three layers of two segments, one second each.
        let mut data = Vec::new();
        let mut layers = Vec::new();
        for l in 0..3u32 {
            layers.push(PreviewLayer { layer: l, z: 0.2 * (l + 1) as f32, t_start: l as f64 * 2.0, t_end: l as f64 * 2.0 + 2.0, starts: [2 * l, 0, 0] });
            for k in 0..2 {
                let t = l as f64 * 2.0 + k as f64;
                data.extend_from_slice(&Bead { a: [0.0, 0.0, 0.2], b: [10.0, 0.0, 0.2], t0: t as f32, dur: 1.0, width: 0.4, height: 0.2, value: 100.0, layer: l as f32, role: 1.0, filament: 1.0, ..Bead::default() }.pack());
            }
        }
        Arc::new(PrintPreview {
            pieces: Vec::new(),
            shared: (0.0, 0.0),
            gcode_key: None,
            printability: Arc::new(fab_printability::Report::default()),
            markers: RetainedInstances::new(PREVIEW_SLOTS, Vec::<f32>::new().into()).unwrap(),
            beads: BeadStreams::surface(RetainedInstances::new(PREVIEW_SLOTS, data.into()).unwrap()),
            travel: RetainedInstances::new(PREVIEW_SLOTS, Vec::<f32>::new().into()).unwrap(),
            blocks: Vec::new(),
            part_layers: Vec::new(),
            chunks: Vec::new(),
            layers,
            total: 6.0,
            grown: None,
            bounds: [[0.0; 3], [10.0, 0.0, 0.6]],
            speed_range: [100.0, 100.0],
            filament_colors: vec![[1.0; 4]],
            roles: Vec::new(),
            changes: Vec::new(),
            timeline: Arc::new(Still),
            summary: Vec::new(),
            pauses: Vec::new(),
            melts: Vec::new(),
            modes: Vec::new(),
            max_z_rate: 0.0,
            nonplanar: None,
            ghosts: RetainedInstances::new(PREVIEW_SLOTS, Vec::<f32>::new().into()).unwrap(),
            filament_use: Vec::new(),
            prepare_seconds: 0.0,
            purges: 0,
            purge_seconds: 0.0,
            id: 1,
            part_offsets: Arc::from([]),
            part_objects: Arc::from([]),
            tools: Vec::new(),
        })
    }

    #[test]
    fn counts_and_layers_follow_time() {
        let p = preview();
        assert_eq!(p.cut_at(-1.0).count(), 0);
        assert_eq!(p.cut_at(0.0).count(), 1);
        assert_eq!(p.cut_at(2.5).count(), 3);
        assert_eq!(p.cut_at(99.0).count(), 6);
        assert_eq!(p.layer_at(0.5), 0);
        assert_eq!(p.layer_at(4.0), 2);
        assert_eq!(p.palette().len(), palette::WIDTH * 4);
    }

    #[test]
    fn comparison_keeps_only_a_settled_modes_off_preview() {
        let mut off = preview();
        Arc::get_mut(&mut off).unwrap().filament_use = vec![
            FilamentUse { slot: 0, grams: 1.0, metres: 0.3, ..Default::default() },
            FilamentUse { slot: 1, grams: 2.0, metres: 0.4, ..Default::default() },
        ];
        let mut on = preview();
        let pv = Arc::get_mut(&mut on).unwrap();
        pv.total += 192.0;
        pv.layers.push(PreviewLayer { layer: 3, z: 0.8, t_start: 6.0, t_end: pv.total, starts: [6, 0, 0] });
        pv.modes = vec![ModeStats { mode: 1, ..Default::default() }, ModeStats { mode: 8, ..Default::default() }];
        pv.filament_use = vec![
            FilamentUse { slot: 0, grams: 1.2, metres: 0.4, ..Default::default() },
            FilamentUse { slot: 1, grams: 2.2, metres: 0.5, ..Default::default() },
        ];
        pv.max_z_rate = 1.2;

        let mut s = PrintState::default();
        // A live modes-off preview must never seed B, even when A settles.
        s.preview = Some(off.clone());
        s.settled = false;
        s.set_preview(on.clone());
        assert!(s.compare.is_none());
        assert!(s.mode_stats.is_empty());

        s.set_preview(off.clone());
        s.set_preview(on.clone());
        assert!(Arc::ptr_eq(s.compare.as_ref().unwrap(), &off));
        assert_eq!(s.mode_stats, "Modes on vs off: +3m12s, +0.4 g, 3 → 4 layers, max Z rate 1.2 mm/s");
        // Later partial results cannot replace the retained, settled baseline.
        s.preview = Some(preview());
        s.settled = false;
        s.set_preview(on.clone());
        assert!(Arc::ptr_eq(s.compare.as_ref().unwrap(), &off));
        s.set_preview(on);
        assert!(Arc::ptr_eq(s.compare.as_ref().unwrap(), &off));

        let mut planar = preview();
        Arc::get_mut(&mut planar).unwrap().modes = vec![ModeStats { mode: 1, ..Default::default() }];
        s.set_preview(planar);
        assert_eq!(s.mode_stats, "Modes on vs off: +0m00s, -3.0 g, 3 → 3 layers");
        s.set_preview(off);
        assert!(s.compare.is_none());
        assert!(s.mode_stats.is_empty());
    }

    /// A resin printer's issue goes to its layer: the Slice view shows the layers, the slider on
    /// that one.
    #[test]
    fn a_resin_issue_shows_its_layer() {
        let mut s = PrintState { resin: Some(resin::ResinShow { count: 240, ..Default::default() }), ..Default::default() };
        let footprint = fab_printability::Footprint::square([30.0, 30.0, 11.0], 2.0);
        let issue = fab_printability::Issue {
            kind: "unconnected_island",
            severity: fab_printability::Severity::Error,
            object: None,
            at_mm: [30.0, 30.0, 11.0],
            footprint,
            layer: Some(220),
            layer_to: None,
            measure: 0.0,
            message: "a 1.00 mm² island cures onto the film".into(),
            hint: String::new(),
            fix: None,
            area_mm2: 0.0,
            lowest_z: 11.0,
            highest_z: 11.0,
            flags: 0,
        };
        s.printability = Arc::new(fab_printability::Report { issues: vec![issue], ..Default::default() });
        assert!(apply(&mut s, &PrintAction::SelectIssue(0)));
        assert!(s.resin_shown().is_some(), "the layers show");
        assert_eq!(s.play.layer_hi, 220);
    }

    /// While a resin printer's layers show, a layer set by hand is one of theirs: a slice of the
    /// plate held beside them, with fewer layers, does not clamp it, and the focus is that layer.
    #[test]
    fn a_resin_layer_set_by_hand_is_not_clamped_by_a_slice() {
        let mut s = PrintState { resin: Some(resin::ResinShow { count: 160, ..Default::default() }), ..Default::default() };
        s.set_preview(follow::tests::preview_of(10));
        s.play.active = true;
        assert!(s.resin_shown().is_some() && s.played_preview().is_some_and(|pv| pv.max_layer() == 9), "the resin layers show, a slice of ten layers beside them");
        assert!(apply(&mut s, &PrintAction::SetTopLayer(80)));
        assert_eq!((s.play.layer_hi, s.focus_layer), (80, Some(80)), "the layer moved to, on the resin stack");
        apply(&mut s, &PrintAction::Seek(25.0));
        assert_eq!((s.play.layer_hi, s.focus_layer), (80, Some(80)), "a seek in the slice's time leaves the resin layer");
        assert!(apply(&mut s, &PrintAction::SetTopLayer(500)));
        assert_eq!(s.play.layer_hi, 159, "at most the stack's top layer");
    }

    #[test]
    fn playback_plays_to_the_top_layer_and_stops() {
        let mut s = PrintState::default();
        s.set_preview(preview());
        assert_eq!(s.play.layer_hi, 2);
        assert!(apply(&mut s, &PrintAction::SetTopLayer(1)));
        assert_eq!(s.play.time, 4.0);
        // Playing from the shown end starts at the bottom layer.
        // Scrubbing shows everything printed by then; a top layer set after it holds.
        apply(&mut s, &PrintAction::Seek(0.0));
        assert_eq!(s.play.layer_hi, 2);
        s.play.layer_hi = 1;
        apply(&mut s, &PrintAction::SetSpeed(10.0));
        apply(&mut s, &PrintAction::Play(true));
        assert!(tick(&mut s, 0.1));
        assert!((s.play.time - 1.0).abs() < 1e-9);
        for _ in 0..10 {
            tick(&mut s, 0.1);
        }
        assert_eq!(s.play.time, 4.0, "stops at the end of layer 1");
        assert!(!s.play.playing);
        assert_eq!(format_time(3725.0), "1:02:05");
        assert_eq!(format_time(65.0), "1:05");
    }

    /// A branch is picked by its part and tips: Delete finds it while the Sliced view shows the
    /// current slice with its supports, in the newest trees, and not once the trees are planned
    /// without it.
    #[test]
    fn a_picked_branch_is_found_by_its_tips() {
        // A stem at x carrying two alike twigs: each twig is a branch, the stem with both another.
        let tree = |object: &str, x: f64| {
            let xf = x as f32;
            let discs = vec![[xf, 0.0, 0.2, 0.2, 1.0], [xf - 1.0, 0.0, 0.4, 0.2, 0.5], [xf + 1.0, 0.0, 0.4, 0.2, 0.5]];
            PlateTree::new(object.into(), vec![[x - 1.0, 0.0, 0.4], [x + 1.0, 0.0, 0.4]], vec![1, 2], discs, &[[0, 1], [0, 2]])
        };
        let mut s = PrintState { trees: vec![tree("a", 1.0), tree("a", 5.0), tree("b", 5.0)].into(), ..Default::default() };
        assert!(apply(&mut s, &PrintAction::PickBranch(Some((2, 2)))));
        assert_eq!(s.picked_branch, Some(("b".into(), vec![[6.0, 0.0, 0.4]])), "the twig's tip");
        assert!(!apply(&mut s, &PrintAction::PickBranch(Some((2, 2)))), "picked already");
        assert_eq!(s.picked_branch(), None, "not while the slice is not shown");
        s.play.active = true;
        assert_eq!(s.picked_branch(), Some((2, 2)));
        s.stale = true;
        assert_eq!(s.picked_branch(), None, "not while the slice is out of date");
        s.stale = false;
        assert!(apply(&mut s, &PrintAction::ShowSupports(false)));
        assert_eq!(s.picked_branch(), None, "not while the supports are hidden");
        assert!(apply(&mut s, &PrintAction::ShowSupports(true)));
        assert_eq!(s.picked_branch(), Some((2, 2)), "the pick stays for when they show again");
        s.trees = vec![tree("b", 5.0), tree("a", 1.0)].into();
        assert_eq!(s.picked_branch(), Some((0, 2)), "found again in the newest trees");
        assert!(apply(&mut s, &PrintAction::PickBranch(Some((0, 0)))));
        assert_eq!(s.picked_branch.as_ref().map(|p| p.1.len()), Some(2), "the stem: both tips");
        assert_eq!(s.picked_branch(), Some((0, 0)));
        s.trees = vec![tree("a", 1.0)].into();
        assert_eq!(s.picked_branch(), None, "gone with its tree");
        assert!(apply(&mut s, &PrintAction::PickBranch(None)));
        assert_eq!(s.picked_branch, None);
    }
}
