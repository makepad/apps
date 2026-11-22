//! Evaluating a Fab program: its solids built by the typed solid graph
//! (`makepad_csg_graph`: primitives, sketches, modifiers, booleans and SDF
//! fields, each node once per key, so an edit rebuilds only what it changed),
//! then every placed solid turned into the printer frame and laid out on the
//! plate.
//!
//! Host-generated requests resolve at `evaluate_unguarded`, before final mesh evaluation:
//! pass one + cached panel bases + cached generators + pass two. An unchanged request
//! never reruns its generator, and no resolution work runs per frame. A failed or
//! cancelled second pass leaves the last complete result in place; only the initial
//! build may show the empty placeholders and uncut bases, with a warning.
//!
//! Frames: the model frame is Y up, millimetres, the ground plane at Y = 0, +Z
//! towards the front. The printer frame is Z up with X to the right and Y to the
//! back, so `printer = (x, -z, y)`, a proper rotation (windings and normals
//! carry over unchanged).

use crate::mech::Mech;
use crate::program::{part_id, Compiled, Placed};
use makepad_model_script::lang::Program;
use fab_plate::machine::{self, Machine, PartReport, Rect};
use fab_plate::{Bounds, ExcludedPart, FieldFloor, Part, PartMesh, Plate};
pub use fab_plate::{PartHandles, RenderMesh};
pub use makepad_csg_sdf::Extent;
use makepad_core_util::sha256::Sha256;
use makepad_csg_graph::{Graph, NodeId, SplitNoRoom, StepNote, TriMesh};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// A free spot on the bed for a new part of footprint `size` (mm), `height` tall, that prints
/// with `nozzles` ([`Evaluated::nozzles_for`]): right of everything, else the spot nearest the
/// bed's centre off every part, either one where all of them reach ([`Machine::reach`]) what
/// the job prints round it ([`Evaluated::rim`]), off every keep-out zone. None when no spot
/// there fits it.
pub fn free_spot(ev: &Evaluated, size: [f64; 2], height: f64, nozzles: &[usize]) -> Option<[f64; 2]> {
    let parts: Vec<Bounds> = ev.plate.placed_parts().map(|p| p.bounds()).collect();
    free_spot_off(ev, &parts, size, height, nozzles)
}

/// [`free_spot`] off the footprints `parts` (plate mm) instead of `ev`'s parts: for parts on
/// their way to the plate, the parts that stay and the new ones given a spot before this one.
pub fn free_spot_off(ev: &Evaluated, parts: &[Bounds], size: [f64; 2], height: f64, nozzles: &[usize]) -> Option<[f64; 2]> {
    let bed = &ev.machine.bed;
    let fits = |c: [f64; 2]| spot_reached(ev, c, size, height, nozzles);
    let right = parts.iter().map(|b| b.max[0]).fold(f64::MIN, f64::max);
    let beside = [right + 10.0 + size[0] * 0.5, (bed.min[1] + bed.max[1]) * 0.5];
    if right > f64::MIN && fits(beside) {
        return Some(beside);
    }
    // No room to the right: the spot nearest the bed's centre, off every part, in 10 mm steps.
    let centre = ev.machine.bed_center();
    let steps = ((bed.max[0] - bed.min[0]).max(bed.max[1] - bed.min[1]) / SPOT_GAP) as i32;
    let mut spots: Vec<[f64; 2]> = (-steps..=steps).flat_map(|i| (-steps..=steps).map(move |j| [i as f64 * SPOT_GAP, j as f64 * SPOT_GAP])).map(|d| [centre[0] + d[0], centre[1] + d[1]]).filter(|c| fits(*c)).collect();
    let dist = |c: &[f64; 2]| (c[0] - centre[0]).powi(2) + (c[1] - centre[1]).powi(2);
    spots.sort_by(|a, b| dist(a).total_cmp(&dist(b)));
    spots.into_iter().find(|c| clear_of(parts.iter().copied(), *c, size, false))
}

/// The clearance [`free_spot`] keeps between a new part and every placed one (mm).
const SPOT_GAP: f64 = 10.0;

/// How far a new part's footprint keeps off the bed's edges at least (mm).
const BED_MARGIN: f64 = 5.0;

/// Whether a new part of footprint `size` (mm), `height` tall, centred at `c`, keeps what the
/// job prints round it ([`Evaluated::rim`]) where all of `nozzles` reach and off every
/// keep-out zone ([`Machine::keep_out`]), and its footprint that far (at least [`BED_MARGIN`])
/// inside the bed's edges.
pub fn spot_reached(ev: &Evaluated, c: [f64; 2], size: [f64; 2], height: f64, nozzles: &[usize]) -> bool {
    let rim = ev.rim.past(size, height);
    let (rx, ry) = (size[0] * 0.5, size[1] * 0.5);
    let footprint = Bounds { min: [c[0] - rx, c[1] - ry, 0.0], max: [c[0] + rx, c[1] + ry, 0.0] };
    on_bed(ev, c, size, rim.max(BED_MARGIN)) && inset_on(ev.machine.reach(nozzles), ev.machine.bed, rim).contains_footprint(&footprint, machine::EDGE_SLACK) && off_keep_out(ev, c, size, rim)
}

/// Whether a footprint of `size` (mm) centred at `c`, grown by `rim`, covers none of the
/// printer's keep-out zones.
fn off_keep_out(ev: &Evaluated, c: [f64; 2], size: [f64; 2], rim: f64) -> bool {
    let (rx, ry) = (size[0] * 0.5 + rim, size[1] * 0.5 + rim);
    let printed = Bounds { min: [c[0] - rx, c[1] - ry, 0.0], max: [c[0] + rx, c[1] + ry, 0.0] };
    !ev.machine.keep_out.iter().any(|r| r.overlaps_footprint(&printed))
}

/// Whether a footprint of `size` (mm) centred at `c` stays `margin` inside the bed's edges.
fn on_bed(ev: &Evaluated, c: [f64; 2], size: [f64; 2], margin: f64) -> bool {
    let bed = &ev.machine.bed;
    let (rx, ry) = (size[0] * 0.5, size[1] * 0.5);
    c[0] - rx >= bed.min[0] + margin && c[0] + rx <= bed.max[0] - margin && c[1] - ry >= bed.min[1] + margin && c[1] + ry <= bed.max[1] - margin
}

/// Whether a footprint of `size` (mm) centred at `c` keeps more than [`SPOT_GAP`] from every
/// placed part, or at least that with `at_gap` (the spot right of everything lies exactly there).
fn clear_of_parts(ev: &Evaluated, c: [f64; 2], size: [f64; 2], at_gap: bool) -> bool {
    clear_of(ev.plate.placed_parts().map(|p| p.bounds()), c, size, at_gap)
}

/// [`clear_of_parts`] of these footprints.
fn clear_of(mut parts: impl Iterator<Item = Bounds>, c: [f64; 2], size: [f64; 2], at_gap: bool) -> bool {
    let (rx, ry) = (size[0] * 0.5, size[1] * 0.5);
    let apart = |a: f64, b: f64| if at_gap { a <= b } else { a < b };
    parts.all(|b| {
        apart(c[0] + rx + SPOT_GAP, b.min[0]) || apart(b.max[0], c[0] - rx - SPOT_GAP) || apart(c[1] + ry + SPOT_GAP, b.min[1]) || apart(b.max[1], c[1] - ry - SPOT_GAP)
    })
}

/// Whether a spot [`free_spot`] gave for a footprint of `size` (mm) is still free: on the bed,
/// off every keep-out zone and clear of every placed part, so something placed there stays
/// where it is.
pub fn spot_still_free(ev: &Evaluated, c: [f64; 2], size: [f64; 2]) -> bool {
    on_bed(ev, c, size, BED_MARGIN) && off_keep_out(ev, c, size, 0.0) && clear_of_parts(ev, c, size, true)
}

/// Why a part of footprint `size` (mm) that prints with `nozzles` has no spot: no room on the
/// bed, or none where they all reach.
pub fn no_room(machine: &Machine, name: &str, size: [f64; 2], nozzles: &[usize]) -> String {
    let names: Vec<&str> = nozzles.iter().filter_map(|&n| machine.nozzles.get(n)).map(|z| z.name.as_str()).collect();
    let (w, d) = (size[0], size[1]);
    match names.as_slice() {
        _ if machine.reach(nozzles) == machine.bed => format!("no room on the bed for '{name}' ({w:.0} × {d:.0} mm)"),
        [one] => format!("no room for '{name}' ({w:.0} × {d:.0} mm) where the {one} nozzle reaches"),
        [rest @ .., last] => format!("no room for '{name}' ({w:.0} × {d:.0} mm) where the {} and {last} nozzles {} reach", rest.join(", "), if rest.len() == 1 { "both" } else { "all" }),
        [] => format!("no room on the bed for '{name}' ({w:.0} × {d:.0} mm)"),
    }
}

/// The implicit print colour of a part in a project without colours.
const FIRST_COLOUR: &str = "Colour 1";

/// Distinct default colours of print colours, by position (linear RGB).
const PALETTE: [[f32; 4]; 8] = [
    [0.80, 0.80, 0.78, 1.0],
    [0.70, 0.12, 0.05, 1.0],
    [0.05, 0.25, 0.70, 1.0],
    [0.85, 0.55, 0.02, 1.0],
    [0.08, 0.45, 0.12, 1.0],
    [0.02, 0.02, 0.02, 1.0],
    [0.45, 0.10, 0.55, 1.0],
    [0.95, 0.95, 0.95, 1.0],
];

/// The default colour of the `index`th print colour (0-based).
pub fn default_color(index: usize) -> [f32; 4] {
    PALETTE[index % PALETTE.len()]
}

/// The name an implicit print colour of a design colour gets: its hex.
/// The print colour a paint material prints as: the one the placement pairs it with, else the
/// one it names, else an implicit colour of its own design colour. A material names the colour
/// it asks for: its albedo picks the colour's tray (and shows until a tray reports one).
fn swatch_color(pairs: &[([f32; 3], String)], w: &crate::paint::Swatch, colors: &mut Vec<fab_plate::PrintColor>) -> String {
    let key = crate::program::color_srgb(w.rgb) | 0xff;
    let name = pairs.iter().find(|(c, _)| crate::program::color_srgb(*c) | 0xff == key).map(|(_, n)| n.clone()).or_else(|| w.name.clone()).unwrap_or_else(|| implicit_name(w.rgb));
    match colors.iter_mut().find(|c| c.name == name) {
        Some(c) if c.rgb.is_none() && c.design.is_none() => {
            c.design = Some(w.rgb);
            c.fallback = w.rgb;
        }
        Some(_) => {}
        None => colors.push(fab_plate::PrintColor { name: name.clone(), rgb: None, fallback: w.rgb, design: Some(w.rgb), at: Vec::new(), material: None, implicit: true }),
    }
    name
}

pub fn implicit_name(rgb: [f32; 3]) -> String {
    let c = crate::program::color_srgb(rgb);
    format!("#{:06x}", c >> 8)
}

/// The project's print colours for the plate (linear RGB).
pub fn print_colors(project: &crate::project::Project) -> Vec<fab_plate::PrintColor> {
    project.colors.iter().enumerate().map(|(i, c)| {
        let d = default_color(i);
        fab_plate::PrintColor {
            name: c.name.clone(),
            rgb: c.rgb.map(|v| { let l = crate::program::color_linear(v); [l[0], l[1], l[2]] }),
            fallback: [d[0], d[1], d[2]],
            design: None,
            at: c.at.clone(),
            material: c.material.clone(),
            implicit: false,
        }
    }).collect()
}

/// The job's print colours on the printer: the colour each one's material shows (linear RGB)
/// and the job tool (1-based) and nozzle it prints with. The app sets it from the colour
/// mapping; parts and paint take it without a rebuild ([`Evaluated::set_mapped`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mapped {
    pub inks: HashMap<String, [f32; 3]>,
    pub tools: HashMap<String, (u32, usize)>,
}

/// What the job prints round the parts on the first layer, past their footprints: a brim round
/// each part and the skirt round them all. The sliced check reads both as extrusions of the
/// part's nozzles, so the layout keeps them where those nozzles reach ([`arrange`] through
/// [`layout`], [`free_spot`]). The app sets it from the print setup; the default (no skirt, no
/// brim) keeps the footprints alone.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rim {
    /// How far the skirt's outer edge lies past the parts and their brims (mm): its distance and
    /// its loops' widths; 0 without a skirt.
    pub skirt: f64,
    pub brim: Brim,
}

/// The brim the job gives a part, outside its footprint.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Brim {
    /// None outside it (no brim, or an inner one).
    #[default]
    None,
    /// An outer brim reaching this far past the part (mm): its width and its gap to the part.
    Outer(f64),
    /// The slicer's automatic brim, by the part's height and footprint
    /// ([`makepad_fab_toolpath::brim::auto_brim_width`]), past this gap (mm).
    Auto(f64),
}

impl Rim {
    /// How far the job prints past a part's footprint of `size` (mm) that stands `height` tall.
    pub fn past(&self, size: [f64; 2], height: f64) -> f64 {
        let brim = match self.brim {
            Brim::None => 0.0,
            Brim::Outer(reach) => reach,
            Brim::Auto(gap) => {
                let outline = makepad_fab_slicer::ExPolygon::new(makepad_fab_slicer::Polygon::rect(makepad_fab_slicer::Pt::from_mm(0.0, 0.0), makepad_fab_slicer::Pt::from_mm(size[0], size[1])));
                let width = makepad_fab_toolpath::brim::auto_brim_width(&[outline], height);
                if width > 0.0 { width + gap } else { 0.0 }
            }
        };
        self.skirt + brim
    }
}

/// The part of `region` on `bed`, `inset` (mm) in from every edge.
fn inset_on(region: Rect, bed: Rect, inset: f64) -> Rect {
    Rect::new(region.min[0].max(bed.min[0]) + inset, region.min[1].max(bed.min[1]) + inset, region.max[0].min(bed.max[0]) - inset, region.max[1].min(bed.max[1]) - inset)
}

/// The job tool (1-based) and nozzle of each print colour the printed parts use: the app's
/// mapping, or, in a session without one (no printer view), the used colours in project
/// order on alternating nozzles.
pub fn job_tools(mapped: &Mapped, colors: &[fab_plate::PrintColor], parts: &[Part], part_colors: &[String]) -> HashMap<String, (u32, usize)> {
    used_tools(mapped, colors, used_colors(parts, part_colors))
}

/// The print colours the printed parts use: each one's own and its paint's.
fn used_colors<'a>(parts: &'a [Part], part_colors: &'a [String]) -> Vec<&'a str> {
    let mut used: Vec<&str> = part_colors.iter().take(parts.len()).map(String::as_str).collect();
    for p in parts {
        if let Some(sp) = &p.paint {
            used.extend(sp.colors.iter().skip(1).filter_map(|c| c.color.as_deref()));
        }
    }
    used
}

/// The job tool and nozzle of each of the print colours `used` ([`job_tools`]).
fn used_tools(mapped: &Mapped, colors: &[fab_plate::PrintColor], mut used: Vec<&str>) -> HashMap<String, (u32, usize)> {
    if !mapped.tools.is_empty() {
        return mapped.tools.clone();
    }
    let order = |n: &str| colors.iter().position(|c| c.name == n).unwrap_or(usize::MAX);
    used.sort_by_key(|n| (order(n), *n));
    used.dedup();
    used.into_iter().enumerate().map(|(k, n)| (n.to_string(), (k as u32 + 1, k % 2))).collect()
}

/// A placed part's paint over its own colour, made before the layout places the part: the
/// layout keeps the part where the nozzles of its colours reach ([`object_nozzles`]) and the
/// paint step paints with these colours, so the plate's check ([`machine::check_part`]) tests
/// the nozzles the layout placed it for.
struct PaintPlan {
    /// What its script paints (over a whole fill, what goes after it), with each picture's
    /// filaments added to its swatches after the script's own, in its layers' order.
    op: crate::paint::ObjectPaint,
    /// Per picture (its spec's key): the filament set it prints with and its filaments' slots.
    pictures: Vec<(u64, fab_colour::FilamentSet, Vec<u8>)>,
    /// Its colours: its own, then each swatch's print colour.
    colors: Vec<fab_plate::paint::PaintColor>,
}

/// The nozzles each object (`group[i]` is part i's) prints with, before the layout places it
/// ([`arrange`]): per part its own colour and its paint's ([`PaintPlan::colors`], a picture's
/// filaments among them), each on its colour's job tool ([`job_tools`], over the printed
/// parts). A colour no tool takes needs none.
fn object_nozzles(plans: &[Option<PaintPlan>], part_colors: &[String], printed: &[bool], attached: &[bool], group: &[String], colors: &[fab_plate::PrintColor], mapped: &Mapped) -> BTreeMap<String, Vec<usize>> {
    let colours = |i: usize| std::iter::once(part_colors[i].as_str()).chain(plans[i].iter().flat_map(|p| p.colors.iter().skip(1).filter_map(|c| c.color.as_deref())));
    let used = (0..plans.len()).filter(|&i| printed[i]).flat_map(colours).collect();
    let tools = used_tools(mapped, colors, used);
    let mut out: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, g) in group.iter().enumerate() {
        let nozzles = out.entry(g.clone()).or_default();
        if attached[i] { continue; }
        nozzles.extend(colours(i).filter_map(|n| tools.get(n)).map(|t| t.1));
        nozzles.sort_unstable();
        nozzles.dedup();
    }
    out
}

/// Each paint colour's job tool and nozzle.
fn paint_tools(p: &mut Option<Arc<PartPaint>>, tools: &HashMap<String, (u32, usize)>) {
    if let Some(p) = p {
        let mut q = (**p).clone();
        for c in q.colors.iter_mut() {
            c.map(&|n| tools.get(n).copied());
        }
        *p = Arc::new(q);
    }
}

/// The colour a print colour shows: its material's (`inks`), unless it has an override, else
/// its own default.
pub fn shown_color(colors: &[fab_plate::PrintColor], inks: &HashMap<String, [f32; 3]>, name: &str) -> [f32; 3] {
    match colors.iter().find(|c| c.name == name) {
        Some(c) => c.rgb.or_else(|| inks.get(name).copied()).unwrap_or(c.fallback),
        None => inks.get(name).copied().unwrap_or([0.8, 0.8, 0.78]),
    }
}

/// The colour a print colour asks for in the design: its override, else its design colour, else
/// its own default; what a render shows unless it is asked for the trays' colours.
pub fn design_color(colors: &[fab_plate::PrintColor], name: &str) -> [f32; 3] {
    match colors.iter().find(|c| c.name == name) {
        Some(c) => c.rgb.or(c.design).unwrap_or(c.fallback),
        None => [0.8, 0.8, 0.78],
    }
}

/// A program that could not be built: where, which solid, why.
#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    /// The script (empty: the project).
    pub file: String,
    pub line: u32,
    pub col: u32,
    /// The solid (empty for a program error).
    pub id: String,
    pub op: String,
    /// The solid's B-rep step lost a selector: its place among the names the step selects by
    /// ([`Lost`]).
    pub lost: Option<usize>,
    pub message: String,
}

impl Failure {
    pub fn program(d: &makepad_model_script::lang::Diag) -> Failure {
        Failure { lost: None, file: String::new(), line: d.line, col: d.col, id: String::new(), op: String::new(), message: d.message.clone() }
    }

    /// The build stopped: its job was cancelled (or its memory ran over, which says so instead).
    fn cancelled() -> Failure {
        Failure { lost: None, file: String::new(), line: 1, col: 1, id: String::new(), op: String::new(), message: "cancelled".into() }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let file = if self.file.is_empty() { String::new() } else { format!("{}:", self.file) };
        if self.id.is_empty() {
            write!(f, "{file}{}:{}: {}", self.line, self.col, self.message)
        } else {
            write!(f, "{file}{}:{}: {} ({}): {}", self.line, self.col, self.id, self.op, self.message)
        }
    }
}

/// What the exact engine knows of a body.
#[derive(Clone, Debug, PartialEq)]
pub struct ExactBody {
    pub measure: makepad_csg_graph::BrepMeasure,
    pub edges: Vec<makepad_csg_graph::BrepEntity>,
    pub faces: Vec<makepad_csg_graph::BrepEntity>,
    /// The script's name of each part a face name comes from, by its step's key: `b` of
    /// `b.top`.
    pub parts: HashMap<makepad_csg_graph::Key, String>,
}

/// One solid of the program after evaluation.
#[derive(Clone, Debug, PartialEq)]
pub struct ObjectInfo {
    /// The solid's id.
    pub name: String,
    pub printed: bool,
    /// Index into the plate's parts when printed.
    pub part: Option<usize>,
    pub triangles: usize,
    /// The frame its own geometry is written in, in the model frame (row-major): the
    /// placements (`at`, `rotate`, `scale`) of it and of every solid it is part of.
    pub world: [[f64; 4]; 4],
    /// Its mesh's bounds in the model frame (`None` when empty or not built).
    pub bounds: Option<([f64; 3], [f64; 3])>,
    /// An exact body's exact properties (not measured during a drag).
    pub exact: Option<makepad_csg_graph::BrepMeasure>,
    /// Its named frame (`frame: Frame{name at rotate}`) in the model frame, row-major: the frame
    /// as written with `world` applied, so it follows the part's placements.
    pub frame: Option<(String, [[f64; 4]; 3])>,
}

/// The evaluated model: the plate, its render meshes and the bed check.
#[derive(Clone, Debug)]
pub struct Evaluated {
    /// One per plate part, same order: how its `place(...)` call reads.
    pub handles: Vec<PartHandles>,
    pub printability: Arc<fab_printability::Report>,
    /// The hash of the sources the plate is made from: its scripts with their parameters and
    /// placements, the strokes files they read and the print colours.
    pub content: [u8; 32],
    pub plate: Plate,
    /// One per plate part, same order.
    pub render: Vec<RenderMesh>,
    pub objects: Vec<ObjectInfo>,
    pub reports: Vec<PartReport>,
    pub machine: Machine,
    /// What the job prints round the parts, as the layout kept it in reach ([`Rim`]).
    pub rim: Rim,
    pub warnings: Vec<String>,
    pub millis: f64,
    /// One per plate part: the model frame to the plate (printer mm, Z up, as laid out),
    /// row-major `p' = m * p`. Feature anchors go through it to the view.
    pub model_to_plate: Vec<[[f64; 4]; 3]>,
    /// Meshes built for this evaluation (the rest came from the cache).
    pub built: usize,
    /// One per plate part: its mesh as `local + offset`.
    pub frames: Vec<PartFrame>,
    /// One per plate part: what changed since this evaluator's previous evaluation (the
    /// slicer recuts only there).
    pub changes: Vec<PartChange>,
    /// Where the evaluation's time went: (phase, milliseconds), in order.
    pub phases: Vec<(&'static str, f64)>,
    /// One per plate part: its distance field, for a part that is one SDF solid (moved,
    /// turned or uniformly scaled, no mesh boolean after it). The slicer and the renderer
    /// use the field; the part's mesh is then for display and export.
    pub fields: Vec<Option<PartField>>,
    /// One per plate part: its paint (`crate::paint`), for a part its script paints.
    pub paint: Vec<Option<Arc<PartPaint>>>,
    /// One per plate part: its named features (model frame), what the fill tool picks.
    pub features: Vec<Arc<Vec<crate::paint::Feature>>>,
    /// Paint and fill features in `plate.excluded` order, kept for editing those placements.
    excluded_paint: Vec<Option<Arc<PartPaint>>>,
    /// The fields in `plate.excluded` order: the views keep drawing them, and the volume a part
    /// has baked outlives its eye.
    excluded_fields: Vec<Option<PartField>>,
    excluded_features: Vec<Arc<Vec<crate::paint::Feature>>>,
    /// One per plate part: its object's force study (model frame), none without one.
    pub studies: Vec<Option<Arc<crate::study::Study>>>,
    excluded_studies: Vec<Option<Arc<crate::study::Study>>>,
    /// One per plate part: where hands hold it (model frame, mm), its script's anchors held by hand when
    /// those are all its script's joints ([`crate::mech::hands`]). Mechanics starts it in them.
    pub hands: Vec<Vec<[f64; 3]>>,
    /// Each instance's joints applied: its jointed parts, for the joint checks and the sim.
    pub mech: Vec<Arc<crate::mech::Mech>>,
    /// The plate parts made of separate links (chains, chainmail), for their checks, the sim and the view.
    pub weaves: Vec<crate::weave::WeavePart>,
    /// The print profile it was built for (joints and links are sized from it).
    pub profile: makepad_csg_mech::Profile,
    /// The print colours: the project's, then implicit ones (a design colour no colour names).
    pub colors: Vec<fab_plate::PrintColor>,
    /// Per placed part (printed, then excluded): the print colour it prints as.
    pub part_colors: Vec<String>,
    /// The material colours and job tools it was mapped with (`Evaluator::mapped`).
    pub mapped: Arc<Mapped>,
    /// How its parts' display meshes show ([`View`]).
    pub view: View,
    /// Per placed part (printed, then excluded): the body its display mesh follows the view
    /// from, if it does.
    shapes: Vec<Option<ViewShape>>,
    /// Per placed part: its display mesh is the view's own (not its plate mesh's, so it has its
    /// own overhang degrees).
    viewed: Vec<bool>,
    /// What the exact solids' written selections took at this build.
    pub taken: Vec<Taken>,
    /// The selector report: the selections that took another count or kind of edge or face
    /// than at the build before ([`selector_report`]), their lines also in `warnings`.
    pub drifted: Vec<Drift>,
    /// A draft's statements whose selector names nothing on its body any more: each built as
    /// its input ([`crate::FabModel::preview`]). Empty for a commit, which such a statement fails.
    pub lost: Vec<Lost>,
}

/// A statement of a draft that lost its selector: built as if it were not there, and marked
/// where a person looks (its line, its tree row, the view) until a pick names its edge or face
/// again.
#[derive(Clone, Debug, PartialEq)]
pub struct Lost {
    pub file: String,
    /// The solid whose step lost it, and the selector's place among the names that step
    /// selects by ([`makepad_model_script::lang::Solid::selectors`]).
    pub id: String,
    pub index: usize,
    pub line: u32,
    /// What a pick must take to stand in for it: an edge or a face.
    pub kind: &'static str,
    /// The view's words: "`r`'s fillet lost its edge: pick the edge it should round".
    pub hint: String,
    /// What a commit of the draft says.
    pub message: String,
}

/// What statement `id` (a step of `proto`) lost, in words: "`bracket`'s fillet lost its edge".
pub fn lost_what(id: &str, proto: &str) -> String {
    let (op, kind, _) = lost_words(proto);
    format!("`{id}`'s {op} lost its {kind}")
}

/// A step that selects by names: what it is called, what it selects and what it does to it.
pub fn lost_words(proto: &str) -> (&'static str, &'static str, &'static str) {
    match proto {
        "BrepChamfer" => ("chamfer", "edge", "bevel"),
        "BrepShell" => ("shell", "face", "leave open"),
        _ => ("fillet", "edge", "round"),
    }
}

/// A written selection that took another count or kind of edge or face than at the build before.
#[derive(Clone, Debug, PartialEq)]
pub struct Drift {
    /// Its solid, `instance/solid`.
    pub part: String,
    /// What its tree row says: `edge(top, front) took 2 lines, was 1 line`.
    pub short: String,
    /// The report's line, for the AI: where, which solid, what changed.
    pub line: String,
}

/// What the written selections of an exact solid (the edges a fillet or a chamfer names by their
/// faces, the faces a shell leaves open) took at a build, in the order written.
#[derive(Clone, Debug, PartialEq)]
pub struct Taken {
    /// `instance/solid`.
    pub part: String,
    pub file: String,
    pub line: u32,
    pub took: Vec<makepad_csg_graph::BrepTaken>,
}

/// The selector report between two builds of a document: each written selection that took
/// another count or kind of edge or face than at `before` (a bore split the edge it names, the
/// side it names is now an arc), by its solid. A selection written differently is a new one,
/// not drift. Never blocking: the build stands, the line says what to look at.
pub fn selector_report(before: &[Taken], after: &[Taken]) -> Vec<Drift> {
    fn words(kinds: &[&str]) -> String {
        let mut out: Vec<String> = Vec::new();
        let mut i = 0;
        while i < kinds.len() {
            let n = kinds[i..].iter().take_while(|k| **k == kinds[i]).count();
            out.push(format!("{n} {}{}", kinds[i], if n == 1 { "" } else { "s" }));
            i += n;
        }
        if out.is_empty() { "nothing".into() } else { out.join(" and ") }
    }
    let mut out = Vec::new();
    for t in after {
        let Some(b) = before.iter().find(|b| b.part == t.part) else { continue };
        for (i, now) in t.took.iter().enumerate() {
            let Some(was) = b.took.get(i).filter(|was| was.named == now.named) else { continue };
            if was.kinds != now.kinds {
                let solid = t.part.rsplit_once('/').map_or(t.part.as_str(), |(_, s)| s);
                let short = format!("{} took {}, was {}", now.named, words(&now.kinds), words(&was.kinds));
                let line = format!("{}:{}: `{solid}`'s {} took {} at the build before, now {}: check it still names what was meant", t.file, t.line, now.named, words(&was.kinds), words(&now.kinds));
                out.push(Drift { part: t.part.clone(), short, line });
            }
        }
    }
    out
}

/// How the view shows the parts: none of it is what the model, its checks or its slice see.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    /// The chord exact parts are shown at (mm).
    pub chord: f64,
    /// Field parts shown as meshes (the Mesh look); else they draw from their fields.
    pub meshes: bool,
}

/// The engine that builds a part's solid (the guide's three).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    /// A mesh (CSG): also a solid of another engine that a boolean took through the mesh.
    Mesh,
    /// An exact body (B-rep).
    Exact,
    /// A distance field (SDF).
    Field,
}

impl Engine {
    pub fn name(self) -> &'static str {
        match self {
            Engine::Mesh => "mesh",
            Engine::Exact => "exact",
            Engine::Field => "field",
        }
    }
}

/// A placed part's body that its display mesh follows the view from.
#[derive(Clone, Debug)]
enum ViewShape {
    /// An exact body: meshed at the view's chord.
    Exact(Arc<Graph>, NodeId),
    /// A distance field: meshed in the Mesh look.
    Field(Arc<Graph>, NodeId),
}

/// A plate part's paint: its script's program for the part's object (model frame, mm), the
/// colour each slot shows and prints in, and the frames between model and printer.
#[derive(Clone, Debug)]
pub struct PartPaint {
    pub program: Arc<fab_paint_field::Program>,
    /// Slot 0 is the part's own colour (its filament); the rest are the script's materials,
    /// with the project's colour-to-filament pairs applied.
    pub colors: Vec<fab_plate::paint::PaintColor>,
    /// The region settings its settings slots index (slot `i` is `settings[i - 1]`;
    /// `m.settings`).
    pub settings: Arc<Vec<crate::paint::RegionSettings>>,
    pub to_printer: [[f64; 4]; 3],
    pub from_printer: [[f64; 4]; 3],
    /// How far `from_printer` stretches a length ([`fab_plate::stretch`]).
    pub stretch: f64,
    /// What the views show where it differs from what prints: a picture's face in the colours
    /// its stacks show, not its filaments' (the program, and the colour of each of its slots).
    pub seen: Option<(Arc<fab_paint_field::Program>, Vec<[f32; 3]>)>,
}

impl PartPaint {
    /// The colour of each slot of what the views show (`seen`, else its own program), slot 0
    /// the part's own.
    pub fn palette(&self) -> Vec<[f32; 3]> {
        self.seen.as_ref().map_or_else(|| self.colors.iter().map(|c| c.rgb).collect(), |(_, colors)| colors.clone())
    }

    /// The program of what the views show (`seen`, else its own), whose slots [`Self::palette`]
    /// colours.
    pub fn shown(&self) -> &Arc<fab_paint_field::Program> {
        self.seen.as_ref().map_or(&self.program, |(program, _)| program)
    }

    /// The colour slot at a printer point (mm).
    pub fn slot_at(&self, p: [f64; 3]) -> u8 {
        let m = &self.from_printer;
        let q = std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3]);
        self.program.sample(fab_paint_field::Channel::Color, q).slot
    }
    /// The colours the views show at printer points (mm): a picture's stack colours where it has
    /// them ([`PartPaint::seen`]), else each slot's print colour as it shows. Read a few
    /// thousand points at a time, so a scripted region's kernel runs once for each batch.
    pub fn shown_at(&self, ps: &[[f64; 3]]) -> Vec<[f32; 3]> {
        let program = self.seen.as_ref().map_or(&self.program, |(p, _)| p);
        let colors = self.palette();
        let m = &self.from_printer;
        // a slot past the list is the part's own (slot 0)
        let own = colors[0];
        ps.chunks(4096)
            .flat_map(|chunk| {
                let qs: Vec<[f64; 3]> = chunk.iter().map(|p| std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3])).collect();
                program.sample_near_many(fab_paint_field::Channel::Color, &qs, 0.0).into_iter().map(|s| colors.get(s.slot as usize).copied().unwrap_or(own)).collect::<Vec<_>>()
            })
            .collect()
    }

    /// What the colour paint holds at a printer point (mm): its slot, a lower bound of the
    /// distance (printer mm) to the nearest point whose slot differs, and a blend there.
    pub fn sample_at(&self, p: [f64; 3]) -> fab_plate::PaintSample {
        let m = &self.from_printer;
        let q = std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3]);
        let s = self.program.sample(fab_paint_field::Channel::Color, q);
        fab_plate::PaintSample { slot: s.slot, dist: s.dist / self.stretch, mix: s.mix }
    }
    /// What the paint is to the slice: equal keys paint the same (the slicer recuts a part whose
    /// key moved). The tools a job puts its colours on are not in it, as the slicer reads those
    /// off the colours: a paint built anew keys as it did when the job's tools came after its
    /// build. The no-trees areas are the supports' alone ([`PartPaint::no_trees_key`]): 0 for a
    /// paint of nothing else, as for none.
    pub fn key(&self) -> u64 {
        let cut: Vec<&fab_paint_field::Layer> = self.program.layers.iter().filter(|l| l.op.channel() != fab_paint_field::Channel::NoTrees).collect();
        if cut.is_empty() {
            return 0;
        }
        let colors: Vec<(&[f32; 3], &Option<String>)> = self.colors.iter().map(|c| (&c.rgb, &c.color)).collect();
        let text = format!("{:?}|{:?}|{:?}|{:?}", cut, colors, self.settings, self.to_printer);
        text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3)) | 1
    }
    /// What its support channels and regions' print settings are where they stand on the printer
    /// ([`fab_plate::SpatialPaint::regions_key`]): every layer but the colour's (which no other
    /// channel's sample reads), the settings its regions set and its frame; 0 for none.
    pub fn regions_key(&self) -> u64 {
        let layers: Vec<&fab_paint_field::Layer> = self.program.layers.iter().filter(|l| l.op.channel() != fab_paint_field::Channel::Color).collect();
        if layers.is_empty() {
            return 0;
        }
        let text = format!("{layers:?}|{:?}|{:?}", self.settings, self.to_printer);
        text.bytes().fold(0x7265_6769_6f6e_7321u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3)) | 1
    }
    /// What the views' paint volume of it is made of: the layers of the program they show
    /// ([`Self::shown`]), its no-trees areas aside. Equal keys bake the same volume wherever the
    /// part stands and whatever its colours and region settings (drawn or sliced, not baked);
    /// 0 for none.
    pub fn shown_key(&self) -> u64 {
        let layers: Vec<&fab_paint_field::Layer> = self.shown().layers.iter().filter(|l| l.op.channel() != fab_paint_field::Channel::NoTrees).collect();
        if layers.is_empty() {
            return 0;
        }
        format!("{layers:?}").bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3)) | 1
    }
    /// What its no-trees areas (`m.no_trees`) are as the part's supports meet them: equal keys
    /// keep the trees out of the same places of the part however far it moved over the bed; 0
    /// for none.
    pub fn no_trees_key(&self) -> u64 {
        let areas: Vec<&fab_paint_field::Layer> = self.program.layers.iter().filter(|l| l.op.channel() == fab_paint_field::Channel::NoTrees).collect();
        if areas.is_empty() {
            return 0;
        }
        // The frame less its move over the bed: the areas stand where they do on the part.
        let m = &self.to_printer;
        let frame = [m[0][0], m[0][1], m[0][2], m[1][0], m[1][1], m[1][2], m[2][0], m[2][1], m[2][2], m[2][3]];
        let text = format!("{areas:?}|{frame:?}");
        text.bytes().fold(0x6e6f_7472_6565_7321u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3)) | 1
    }
    /// The printer heights (mm) outside which [`PartPaint::sample_at`] answers as `old`'s does,
    /// slot and edge distance: none when the two paint alike, `None` anywhere. Each is a box's
    /// that may answer differently, grown by two bricks: a strokes layer's strokes past the two
    /// logs' common prefix (drawn, undone, redone), where a point's brick lists them; a feature's
    /// or a field's region whose field an edit moved, round where it moved
    /// ([`fab_paint_field::Program::layer_changed`]); any other change of a boxed layer (a
    /// pattern grown anew, another slot), all of its box, old and new; a layer added or taken
    /// away, all of its box. A box whose heights are held already is not searched.
    pub fn changed_since(&self, old: &PartPaint) -> Option<Vec<(f64, f64)>> {
        use fab_paint_field::strokes::BRICK;
        use fab_paint_field::Changes;
        let same_frame = (0..3).all(|r| (0..4).all(|c| self.to_printer[r][c].to_bits() == old.to_printer[r][c].to_bits()));
        if !same_frame || self.colors != old.colors || self.settings != old.settings {
            return None;
        }
        let (now, was) = (&self.program, &old.program);
        let (a, b) = (&now.layers, &old.program.layers);
        // The layers read in order, so the two pair in order: layer for layer when as many, else
        // the leading and the trailing ones alike (the same op, kind of region and box), the
        // layers between them added or taken away.
        let (head, tail) = if a.len() == b.len() {
            (a.len(), 0)
        } else {
            let bits = |p: &fab_paint_field::Program, li: usize| p.layer_box(li).map(|(lo, hi)| (lo.map(f64::to_bits), hi.map(f64::to_bits)));
            let alike = |i: usize, j: usize| {
                std::mem::discriminant(&a[i].region) == std::mem::discriminant(&b[j].region) && bits(now, i) == bits(was, j) && format!("{:?}", a[i].op) == format!("{:?}", b[j].op)
            };
            let n = a.len().min(b.len());
            let head = (0..n).take_while(|&k| alike(k, k)).count();
            let tail = (0..n - head).take_while(|&k| alike(a.len() - 1 - k, b.len() - 1 - k)).count();
            (head, tail)
        };
        let mut heights = Heights { to_printer: self.to_printer, grow: 2.0 * BRICK, spans: Vec::new() };
        for (li, oi) in (0..head).map(|k| (k, k)).chain((0..tail).map(|k| (a.len() - 1 - k, b.len() - 1 - k))) {
            match (&a[li].region, &b[oi].region) {
                (fab_paint_field::Region::Strokes { file: fa, strokes: sa, slots: la }, fab_paint_field::Region::Strokes { file: fb, strokes: sb, slots: lb })
                    if fa == fb && la == lb && format!("{:?}", a[li].op) == format!("{:?}", b[oi].op) =>
                {
                    sa.changed_boxes(sb, true).into_iter().for_each(|b| heights.add(b));
                }
                _ => {
                    if !now.layer_changed(li, was, oi, &mut heights) {
                        return None;
                    }
                }
            }
        }
        for (p, between) in [(now, head..a.len() - tail), (was, head..b.len() - tail)] {
            for li in between {
                heights.add(p.layer_box(li)?);
            }
        }
        Some(heights.spans)
    }
    /// The printer box (mm) a model box `(lo, hi)` lies in.
    fn printer_box(&self, (lo, hi): ([f64; 3], [f64; 3])) -> ([f64; 3], [f64; 3]) {
        let m = &self.to_printer;
        let (mut min, mut max) = ([f64::MAX; 3], [f64::MIN; 3]);
        for corner in 0..8 {
            let p: [f64; 3] = std::array::from_fn(|k| if corner & (1 << k) == 0 { lo[k] } else { hi[k] });
            for r in 0..3 {
                let v = m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3];
                min[r] = min[r].min(v);
                max[r] = max[r].max(v);
            }
        }
        (min, max)
    }
    /// Where `channel` holds, as the slicer reads it: negative inside, its size a lower bound of
    /// the distance (printer mm) to the edge.
    fn support_region(self: &Arc<Self>, channel: fab_paint_field::Channel) -> Option<fab_plate::SupportRegion> {
        if !self.program.has(channel) {
            return None;
        }
        let paint = self.clone();
        let at = Arc::new(move |p: [f64; 3]| {
            let s = paint.program.sample(channel, crate::weave::affine(&paint.from_printer, p));
            let d = s.dist / paint.stretch;
            if s.slot != 0 { -d } else { d }
        });
        Some(fab_plate::SupportRegion { at, bounds: self.program.channel_bounds(channel).map(|b| self.printer_box(b)) })
    }
    /// The regions with print settings of their own (`m.settings`) as the slicer reads them, in
    /// the script's order: each statement's own region, exact (the slicer cuts each layer by it),
    /// whatever a later one sets over it.
    fn settings_regions(self: &Arc<Self>) -> Vec<fab_plate::SettingsRegion> {
        use fab_paint_field::{Channel, Op};
        self.program.layers.iter().enumerate().filter_map(|(li, l)| match l.op {
            Op::Paint { channel: Channel::Settings, slot } => {
                let values = self.settings.get(usize::from(slot).checked_sub(1)?)?.clone();
                let paint = self.clone();
                let at = Arc::new(move |p: [f64; 3]| paint.program.region_distance(li, crate::weave::affine(&paint.from_printer, p)) / paint.stretch);
                let bounds = self.program.layer_box(li).map(|b| self.printer_box(b));
                Some(fab_plate::SettingsRegion { region: fab_plate::SupportRegion { at, bounds }, values })
            }
            _ => None,
        }).collect()
    }
}

/// A part's paint as the plate's samplers read it (the slicer's and the checks').
fn spatial_paint(p: &Arc<PartPaint>) -> fab_plate::SpatialPaint {
    let a = p.clone();
    fab_plate::SpatialPaint {
        sample_at: Arc::new(move |q| a.sample_at(q)),
        support_block: p.support_region(fab_paint_field::Channel::SupportBlock),
        support_enforce: p.support_region(fab_paint_field::Channel::SupportEnforce),
        no_trees: p.support_region(fab_paint_field::Channel::NoTrees),
        settings: p.settings_regions(),
        colors: p.colors.clone(),
        key: p.key(),
        no_trees_key: p.no_trees_key(),
        regions_key: p.regions_key(),
        source: p.clone(),
    }
}

impl fab_plate::PaintChange for PartPaint {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn changed(&self, old: &dyn fab_plate::PaintChange) -> Option<Vec<(f64, f64)>> {
        self.changed_since(old.as_any().downcast_ref()?)
    }
}

/// The printer heights a paint change search has found ([`PartPaint::changed_since`]): each
/// model box's, grown by `grow`, in order and merged where they meet.
struct Heights {
    to_printer: [[f64; 4]; 3],
    grow: f64,
    spans: Vec<(f64, f64)>,
}

impl Heights {
    /// The heights model box `(lo, hi)` spans on the printer, grown.
    fn of(&self, (lo, hi): &([f64; 3], [f64; 3])) -> (f64, f64) {
        let m = &self.to_printer[2];
        let (lo, hi) = (lo.map(|v| v - self.grow), hi.map(|v| v + self.grow));
        (0..8).fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), c| {
            let q = [if c & 1 == 0 { lo[0] } else { hi[0] }, if c & 2 == 0 { lo[1] } else { hi[1] }, if c & 4 == 0 { lo[2] } else { hi[2] }];
            let z = m[0] * q[0] + m[1] * q[1] + m[2] * q[2] + m[3];
            (a.min(z), b.max(z))
        })
    }
}

impl fab_paint_field::Changes for Heights {
    fn holds(&self, b: &([f64; 3], [f64; 3])) -> bool {
        let (a, z) = self.of(b);
        self.spans.iter().any(|s| s.0 <= a && z <= s.1)
    }
    fn add(&mut self, b: ([f64; 3], [f64; 3])) {
        let (mut a, mut z) = self.of(&b);
        // the spans it meets join it
        self.spans.retain(|s| {
            let meets = s.0 <= z && a <= s.1;
            if meets {
                (a, z) = (a.min(s.0), z.max(s.1));
            }
            !meets
        });
        let at = self.spans.partition_point(|s| s.0 < a);
        self.spans.insert(at, (a, z));
    }
}

/// A flood from `seed` (model frame) over a part's surface, in the model frame: over its field
/// when it is one (nothing meshed; the gradient gives the normals), else over its mesh's
/// triangles. `None` when the seed is not on this part.
fn flood_on(part: &Part, field: Option<&PartField>, from_printer: &[[f64; 4]; 3], seed: [f64; 3], angle: f64) -> Option<fab_paint_field::Patch> {
    let apply = |m: &[[f64; 4]; 3], p: [f64; 3]| -> [f64; 3] { std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3]) };
    let near = crate::paint::FLOOD_SEED_REACH;
    match field {
        Some(f) => {
            // model -> shape, and the shape's distances in model millimetres
            let shape_from_model = crate::weave::compose(&invert_affine(&f.to_printer)?, &invert_affine(from_printer)?);
            let scale = (0..3).map(|r| shape_from_model[r][0].powi(2)).sum::<f64>().sqrt();
            if !(scale > 0.0) {
                return None;
            }
            let sdf = f.shape.field();
            let field: fab_paint_field::flood::Field = Arc::new(move |p| {
                let q = apply(&shape_from_model, p);
                makepad_csg_sdf::Sdf3::distance(&*sdf, makepad_csg_math::Vec3d::new(q[0], q[1], q[2])) / scale
            });
            fab_paint_field::Patch::flood_field(field, seed, angle, near, FLOOD_PITCH)
        }
        None => {
            let pos: Vec<[f64; 3]> = part.mesh.positions.iter().map(|p| apply(from_printer, *p)).collect();
            let tris: Vec<[[f64; 3]; 3]> = part.mesh.triangles.iter().map(|t| t.map(|i| pos[i as usize])).collect();
            let normals: Vec<[f64; 3]> = tris
                .iter()
                .map(|t| {
                    let (u, v) = (std::array::from_fn::<f64, 3, _>(|k| t[1][k] - t[0][k]), std::array::from_fn::<f64, 3, _>(|k| t[2][k] - t[0][k]));
                    let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
                    let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
                    if l > 0.0 { n.map(|x| x / l) } else { n }
                })
                .collect();
            fab_paint_field::Patch::flood(tris, &normals, seed, angle, near)
        }
    }
}

/// The lattice a field flood walks (mm): finer than the paint's own lattice.
const FLOOD_PITCH: f64 = 0.3;

fn apply_affine(m: &[[f64; 4]; 3], p: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3])
}

/// A mix's pattern grown on a part's shape in the model frame: a field part's own field, else
/// its mesh as built (`mesh`, model frame). Keyed by the shape alone, so a part moved or turned
/// on the plate keeps its pattern.
fn grow_on(field: Option<&PartField>, mesh: Option<&Arc<PartMesh>>, g: &fab_paint_field::pattern::Grown, part: &str, k: usize, wait: bool, stop: &dyn Fn() -> bool) -> fab_paint_field::pattern::Grown {
    use fab_paint_field::pattern::{band::Surface, live};
    let fan = |n: usize, f: &(dyn Fn(usize) + Sync)| makepad_fab_slicer::par::fan(n, f);
    // a field's distance at many points in one call
    let many = |field: Arc<dyn makepad_csg_sdf::Sdf3 + Send + Sync>| -> fab_paint_field::pattern::band::Many {
        Arc::new(move |ps: &[[f64; 3]], out: &mut [f64]| {
            let ps: Vec<makepad_csg_math::Vec3d> = ps.iter().map(|p| makepad_csg_math::Vec3d::new(p[0], p[1], p[2])).collect();
            makepad_csg_sdf::Sdf3::distance_many(&*field, &ps, out);
        })
    };
    let fnv = |h: u64, bytes: &[u8]| bytes.iter().fold(h, |h, b| (h ^ *b as u64).wrapping_mul(0x100_0000_01b3));
    // where it shows: the part's `k`-th pattern
    let slot = fnv(fnv(0xcbf2_9ce4_8422_2325, part.as_bytes()), &(k as u64).to_le_bytes());
    match (field, mesh) {
        (Some(f), _) => {
            let shape = f.shape.clone();
            let key = fnv(0xcbf2_9ce4_8422_2325, &shape.key());
            let surface = || {
                let sdf = shape.field();
                Some(Surface { distance: many(sdf), clearance: shape.clearance(), bounds: (shape.min, shape.max), key })
            };
            live::resolve(&g.spec, key, slot, &surface, wait, &fan, stop)
        }
        (None, Some(mesh)) => {
            let mut key = 0xcbf2_9ce4_8422_2325u64;
            for p in &mesh.positions {
                for v in p {
                    key = fnv(key, &v.to_bits().to_le_bytes());
                }
            }
            for t in &mesh.triangles {
                for v in t {
                    key = fnv(key, &v.to_le_bytes());
                }
            }
            let surface = || {
                let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
                for p in &mesh.positions {
                    for k in 0..3 {
                        lo[k] = lo[k].min(p[k]);
                        hi[k] = hi[k].max(p[k]);
                    }
                }
                let data = makepad_csg_sdf::mesh_field::MeshData { positions: mesh.positions.iter().map(|p| p.map(|v| v as f32)).collect(), triangles: mesh.triangles.clone() };
                let field = Arc::new(makepad_csg_sdf::mesh_field::MeshField::new(data));
                Some(Surface { distance: many(field), clearance: makepad_csg_sdf::Clearance::uniform(1.0), bounds: (lo, hi), key })
            };
            live::resolve(&g.spec, key, slot, &surface, wait, &fan, stop)
        }
        (None, None) => g.clone(),
    }
}

/// The inverse of a row-major 3x4 affine map.
fn invert_affine(m: &[[f64; 4]; 3]) -> Option<[[f64; 4]; 3]> {
    let a = |r: usize, c: usize| m[r][c];
    let det = a(0, 0) * (a(1, 1) * a(2, 2) - a(1, 2) * a(2, 1)) - a(0, 1) * (a(1, 0) * a(2, 2) - a(1, 2) * a(2, 0)) + a(0, 2) * (a(1, 0) * a(2, 1) - a(1, 1) * a(2, 0));
    if det.abs() < 1e-12 {
        return None;
    }
    let mut r = [[0.0; 4]; 3];
    for i in 0..3 {
        for j in 0..3 {
            let (i1, i2, j1, j2) = ((j + 1) % 3, (j + 2) % 3, (i + 1) % 3, (i + 2) % 3);
            r[i][j] = (a(i1, j1) * a(i2, j2) - a(i1, j2) * a(i2, j1)) / det;
        }
    }
    for i in 0..3 {
        r[i][3] = -(r[i][0] * m[0][3] + r[i][1] * m[1][3] + r[i][2] * m[2][3]);
    }
    Some(r)
}

/// A plate part as `local + offset` (`Part::mesh` is `local` translated by
/// `offset`, computed the same way).
#[derive(Clone, Debug)]
pub struct PartFrame {
    pub local: Arc<PartMesh>,
    pub offset: [f64; 3],
}

/// A part's solid as a distance field: `shape` (the typed SDF shape, in
/// the model frame) and `to_printer` mapping it onto the plate as laid out.
/// Node bounds, culling and change regions: `makepad_csg_sdf::bounds (re-exported by makepad_csg_graph)`.
#[derive(Clone, Debug)]
pub struct PartField {
    /// The Field node in the compiled program, before its transforms.
    pub node: NodeId,
    pub shape: Arc<makepad_csg_graph::SdfShape>,
    /// Row-major, `p_printer = to_printer * p_shape`.
    pub to_printer: [[f64; 4]; 3],
    /// The same map before the plate offset ([`PartFrame::offset`]): `p_part = to_part * p_shape`,
    /// in the part's frame ([`PartFrame::local`]). Its bits never depend on where the part sits,
    /// so moving a part keeps the slicer's content key (taking the offset back off `to_printer`
    /// rounds differently at each position).
    pub to_part: [[f64; 4]; 3],
    /// The solid's box in the part's frame (printer mm before the plate offset): it holds the
    /// solid, and is the range the slicer slices. None: no solid found.
    pub bounds: Option<Bounds>,
    /// How far the box reaches below the lowest solid point its proof found (printer mm): solid
    /// the proof could not rule out, at most [`EXTENT_TOL`] where it closed. A dropped part rests
    /// on that point, so it may sit up to this far into the bed. None: no solid point found (it
    /// rests on its box).
    pub bottom_gap: Option<f64>,
    /// [`makepad_csg_graph::SdfShape::key`].
    pub key: [u8; 32],
    /// Its solid's volume (mm³, in the part's frame), read from its field at its cell: pending
    /// until read ([`FieldVolume`]). None in a drag's draft or a still picture's build.
    pub volume: Option<FieldVolume>,
    /// The field as a distance under a placement that stretches it (a part scaled by axis):
    /// see [`PartField::distance`]. None: its placement keeps it a distance as it is.
    pub stretched: Option<Arc<Stretched>>,
}

/// A field part under a placement that stretches it, as a distance: its shape with the stretch
/// along its own axes in it ([`makepad_csg_sdf::stretched`], a lower bound on the distance), that
/// shape's key, and the maps the stretch leaves (each a turn, a move and one uniform scale).
#[derive(Clone, Debug)]
pub struct Stretched {
    pub shape: Arc<makepad_csg_graph::SdfShape>,
    pub key: [u8; 32],
    pub to_printer: [[f64; 4]; 3],
    pub to_part: [[f64; 4]; 3],
}

impl Stretched {
    /// `shape` under `to_part` (and `to_printer`, the same map moved), when that map stretches it.
    fn of(shape: &makepad_csg_graph::SdfShape, to_printer: &[[f64; 4]; 3], to_part: &[[f64; 4]; 3]) -> Option<Arc<Stretched>> {
        let (Some(shape), rest) = makepad_csg_sdf::stretched(shape, to_part)? else { return None };
        let to_printer = std::array::from_fn(|r| [rest[r][0], rest[r][1], rest[r][2], to_printer[r][3]]);
        Some(Arc::new(Stretched { key: shape.key(), shape: Arc::new(shape), to_printer, to_part: rest }))
    }
}

/// A field part's solid volume (mm³, in the part's frame), read from its field at its cell
/// ([`crate::field_volume::field_volume`]). A shape read before is known at once; a new one is
/// pending until something reads it ([`FieldVolume::read`]: a host does, on a background task,
/// once the build is in), so no build and no answer waits for it, and every copy of the build
/// shares the one read. A pending volume shows as pending, never as a number
/// ([`FieldVolume::known`]).
#[derive(Clone)]
pub struct FieldVolume {
    read: Arc<VolumeRead>,
    /// The part's frame's volume over the field's.
    scale: f64,
}

struct VolumeRead {
    v: std::sync::OnceLock<f64>,
    read: Option<Box<dyn Fn() -> f64 + Send + Sync>>,
}

impl FieldVolume {
    fn known_at(v: f64) -> Self {
        FieldVolume { read: Arc::new(VolumeRead { v: std::sync::OnceLock::from(v), read: None }), scale: 1.0 }
    }

    fn pending(read: impl Fn() -> f64 + Send + Sync + 'static) -> Self {
        FieldVolume { read: Arc::new(VolumeRead { v: std::sync::OnceLock::new(), read: Some(Box::new(read)) }), scale: 1.0 }
    }

    /// Scaled by `k` (a frame's determinant).
    fn scaled(self, k: f64) -> Self {
        FieldVolume { scale: self.scale * k, ..self }
    }

    /// The volume, read first (on this thread) if it is pending.
    pub fn read(&self) -> f64 {
        let r = &self.read;
        *r.v.get_or_init(|| r.read.as_ref().map_or(0.0, |read| read())) * self.scale
    }

    /// The volume once it has been read: None while it is pending. Never reads.
    pub fn known(&self) -> Option<f64> {
        self.read.v.get().map(|v| v * self.scale)
    }
}

impl std::fmt::Debug for FieldVolume {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.known() {
            Some(v) => write!(f, "FieldVolume({v})"),
            None => f.write_str("FieldVolume(pending)"),
        }
    }
}

/// Distance fields meshed in this process so far (an evaluation with
/// [`Evaluator::mesh_fields`], a ghost, an inspection tool: every one).
pub fn fields_meshed() -> u64 {
    makepad_csg_sdf::meshes_built()
}

impl PartField {
    /// The field as a distance on the plate: (shape, key, to_printer, to_part), each map a turn,
    /// a move and one uniform scale, under which the shape's value stays a distance (a lower
    /// bound when its placement stretches it: [`Stretched`]). What a reader of distances takes
    /// (the slicer, the views' tracer, clearance, supports); `shape` under `to_printer` is the
    /// model and its whole placement (the frame of its paint, sculpt strokes and studies, and
    /// of its extent proofs, which take any affine map).
    pub fn distance(&self) -> (&Arc<makepad_csg_graph::SdfShape>, &[u8; 32], &[[f64; 4]; 3], &[[f64; 4]; 3]) {
        match &self.stretched {
            Some(s) => (&s.shape, &s.key, &s.to_printer, &s.to_part),
            None => (&self.shape, &self.key, &self.to_printer, &self.to_part),
        }
    }

    /// Its solid's extent on the plate after `m` (plate mm): `m` after `to_printer`.
    pub fn extent_under(&self, m: &[[f64; 4]; 3]) -> Option<Extent> {
        extent_of(&self.shape, self.key, &affine_mul(m, &self.to_printer))
    }

    /// Its solid's distance field on the plate (mm, negative inside), from [`Self::distance`]:
    /// None if that map flattens it.
    pub fn on_plate(&self) -> Option<impl Fn(f64, f64, f64) -> f64 + Send + Sync> {
        let (shape, _, to_printer, _) = self.distance();
        let from = invert_affine(to_printer)?;
        let col = |c: usize| [from[0][c], from[1][c], from[2][c]];
        let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        let k2 = dot(col(0), col(0));
        let even = (0..3).all(|i| (0..3).all(|j| (dot(col(i), col(j)) - if i == j { k2 } else { 0.0 }).abs() <= 1e-9 * k2));
        let (sdf, k) = (shape.field(), k2.sqrt());
        even.then(move || move |x: f64, y: f64, z: f64| {
            let q = apply_affine(&from, [x, y, z]);
            makepad_csg_sdf::Sdf3::distance(&*sdf, makepad_csg_math::Vec3d::new(q[0], q[1], q[2])) / k
        })
    }
}

/// The plate's printability as it prints: the parts of a glued group ([`Plate::groups`]) are
/// judged as one body, each field part of one by its field (`fields`, one per plate part), so a
/// facet inside another of its group's parts hints nothing; two objects whose bodies meet are an
/// error, a field part without a mesh judged by its field.
pub fn analyse_plate(machine: &Machine, plate: &Plate, fields: &[Option<PartField>], materials: &fab_printability::Materials) -> fab_printability::Report {
    let grouped: Vec<usize> = plate.groups().into_iter().flatten().collect();
    // A field part with no mesh has no facets to judge by it either way: its field is its only
    // solid, for the overlap check.
    let solid = |i: usize| grouped.contains(&i) || plate.parts.get(i).is_some_and(|p| p.mesh.triangles.is_empty());
    let solids: Vec<_> = fields.iter().enumerate().map(|(i, f)| f.as_ref().filter(|_| solid(i)).and_then(PartField::on_plate)).collect();
    fab_printability::analyse_plate_with(machine, plate, materials, &|i| solids.get(i)?.as_ref().map(|f| f as fab_printability::PartField<'_>), &|| false).expect("nothing stops it")
}

/// What changed in one plate part since the previous evaluation, by object
/// name, in the part's own frame ([`PartFrame::local`]).
#[derive(Clone, Debug, PartialEq)]
pub enum PartChange {
    /// No part of this object was on the previous plate.
    New,
    /// The identical local mesh at the same offset.
    Same,
    /// The identical local mesh at another offset.
    Moved,
    /// The local mesh changed inside this region: the bounds of every
    /// triangle that is in one of the two meshes and not in the other.
    Reshaped(ChangedRegion),
}

/// A region of a part's local frame (printer millimetres, Z up).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChangedRegion {
    pub z: (f64, f64),
    pub xy: Bounds2,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds2 {
    pub min: [f64; 2],
    pub max: [f64; 2],
}

impl Evaluated {
    /// This plate's printability with `materials` ([`analyse_plate`]).
    pub fn analyse(&self, materials: &fab_printability::Materials) -> fab_printability::Report {
        analyse_plate(&self.machine, &self.plate, &self.fields, materials)
    }

    /// Map the print colours anew (`Evaluator::mapped`): parts and paint show their material
    /// colours where a colour has no override, and print with their tools. Nothing is rebuilt.
    pub fn set_mapped(&mut self, mapped: Arc<Mapped>) {
        self.mapped = mapped.clone();
        let colors = self.colors.clone();
        self.recolor(&|name| shown_color(&colors, &mapped.inks, name));
    }

    /// The plate in its design colours ([`design_color`]), not the trays': what a render shows of
    /// the paint and the parts. Their tools stay as mapped.
    pub fn in_design_colors(&self) -> Evaluated {
        let mut ev = self.clone();
        let colors = self.colors.clone();
        ev.recolor(&|name| design_color(&colors, name));
        ev
    }

    /// Parts and paint shown in `shown`'s colour for each print colour, printed with the tools
    /// of the mapping.
    fn recolor(&mut self, shown: &dyn Fn(&str) -> [f32; 3]) {
        let tools = job_tools(&self.mapped, &self.colors, &self.plate.parts, &self.part_colors);
        let n = self.plate.parts.len();
        for i in 0..n + self.plate.excluded.len() {
            let Some(name) = self.part_colors.get(i) else { break };
            let own = shown(name);
            let recolor = |colors: &mut Vec<fab_plate::paint::PaintColor>| {
                for (k, c) in colors.iter_mut().enumerate() {
                    c.rgb = if k == 0 { own } else { c.color.as_deref().map_or(own, shown) };
                    c.map(&|n| tools.get(n).copied());
                }
            };
            if i < n {
                let part = &mut self.plate.parts[i];
                part.color = [own[0], own[1], own[2], 1.0];
                if let Some(Some(p)) = self.paint.get_mut(i) {
                    let mut q = (**p).clone();
                    recolor(&mut q.colors);
                    if let Some(sp) = &mut part.paint {
                        sp.colors = q.colors.clone();
                        sp.key = q.key();
                    }
                    *p = Arc::new(q);
                }
            } else {
                let ex = &mut self.plate.excluded[i - n];
                ex.part.color = [own[0], own[1], own[2], 1.0];
                if let Some(p) = &mut ex.paint {
                    let mut q = (**p).clone();
                    recolor(&mut q.colors);
                    *p = Arc::new(q);
                }
                if let Some(Some(p)) = self.excluded_paint.get_mut(i - n) {
                    let mut q = (**p).clone();
                    recolor(&mut q.colors);
                    *p = Arc::new(q);
                }
            }
        }
        self.plate.map_colors(&self.part_colors, &|n| tools.get(n).copied());
    }

    /// The print colour of a placed part (printed, then excluded).
    pub fn part_color(&self, i: usize) -> Option<&str> {
        self.part_colors.get(i).map(String::as_str)
    }

    /// A placed part's field (printed, then excluded).
    pub fn placed_field(&self, i: usize) -> Option<&PartField> {
        if i < self.plate.parts.len() { self.fields.get(i) } else { self.excluded_fields.get(i - self.plate.parts.len()) }?.as_ref()
    }

    /// Placement handles in the editor's order (printed, then excluded).
    pub fn placed_handles(&self, i: usize) -> Option<&PartHandles> {
        if i < self.plate.parts.len() { self.handles.get(i) }
        else { self.plate.excluded.get(i - self.plate.parts.len()).map(|p| &p.handles) }
    }

    /// The engine that builds placed part `i` (printed, then excluded).
    pub fn engine(&self, i: usize) -> Engine {
        match self.shapes.get(i) {
            Some(Some(ViewShape::Exact(..))) => Engine::Exact,
            Some(Some(ViewShape::Field(..))) => Engine::Field,
            _ => Engine::Mesh,
        }
    }

    /// Placed part `i` (printed, then excluded).
    pub fn placed_part(&self, i: usize) -> &Part {
        match i.checked_sub(self.plate.parts.len()) {
            None => &self.plate.parts[i],
            Some(k) => &self.plate.excluded[k].part,
        }
    }

    /// Its display meshes as `view` shows them (placed index, mesh).
    fn show_as(&mut self, view: View, meshes: Vec<(usize, RenderMesh)>) {
        self.view = view;
        for (i, rm) in meshes {
            self.viewed[i] = true;
            match i.checked_sub(self.plate.parts.len()) {
                None => self.render[i] = rm,
                Some(k) => self.plate.excluded[k].render = rm,
            }
        }
    }

    /// Plate part `i`'s overhang degree per triangle of its display mesh: its plate mesh's
    /// (the printability report's), or those of the mesh the view made (at `limit`, the report's
    /// overhang angle).
    pub fn shown_overhang(&self, i: usize) -> Option<std::borrow::Cow<'_, [u8]>> {
        if self.viewed.get(i) == Some(&true) {
            return self.render.get(i).map(|rm| std::borrow::Cow::Owned(fab_printability::render_overhang(rm, self.printability.overhang_limit)));
        }
        self.printability.overhang.get(i).map(|d| std::borrow::Cow::Borrowed(d.as_slice()))
    }

    pub fn placed_frame(&self, i: usize) -> Option<&[[f64; 4]; 3]> {
        if i < self.plate.parts.len() { self.model_to_plate.get(i) }
        else { self.plate.excluded.get(i - self.plate.parts.len()).map(|p| &p.model_to_plate) }
    }

    /// Placed part `i`'s print colours: its own, then its paint's.
    pub fn part_colours(&self, i: usize) -> Vec<&str> {
        let own = self.part_colors.get(i).map(String::as_str);
        let paint = self.plate.placed_part(i).and_then(|p| p.paint.as_ref()).into_iter().flat_map(|sp| sp.colors.iter().skip(1).filter_map(|c| c.color.as_deref()));
        own.into_iter().chain(paint).collect()
    }

    /// The nozzles the print colours `colours` print with on this plate's job ([`job_tools`])
    /// once a part in them is on it: where such a part may stand ([`free_spot`]). A colour no
    /// tool takes needs none.
    pub fn nozzles_for(&self, colours: &[&str]) -> Vec<usize> {
        let mut used = used_colors(&self.plate.parts, &self.part_colors);
        used.extend_from_slice(colours);
        let tools = used_tools(&self.mapped, &self.colors, used);
        let mut nozzles: Vec<usize> = colours.iter().filter_map(|c| tools.get(*c)).map(|t| t.1).collect();
        nozzles.sort_unstable();
        nozzles.dedup();
        nozzles
    }

    /// The print colour a new object prints in while it names none: the project's first, else
    /// the implicit first colour.
    pub fn new_object_colour(&self) -> &str {
        self.colors.first().filter(|c| !c.implicit).map_or(FIRST_COLOUR, |c| c.name.as_str())
    }

    /// Whether a part's grown pattern shows a state on its way, not the one that prints: a
    /// slice waits for it ([`fab_paint_field::pattern::live`]).
    pub fn patterns_pending(&self) -> bool {
        self.paint.iter().chain(&self.excluded_paint).flatten().any(|p| p.program.patterns_pending())
    }

    /// The plate with its grown patterns' printing states, once their simulations have grown
    /// every one it shows on its way; `None` while one still grows (or none does).
    pub fn with_patterns_grown(&self) -> Option<Evaluated> {
        if !self.patterns_pending() {
            return None;
        }
        let grown = |p: &Arc<PartPaint>| -> Option<Option<Arc<PartPaint>>> {
            if !p.program.patterns_pending() {
                return Some(None);
            }
            let mut all = true;
            let mut grow = |g: &fab_paint_field::pattern::Grown| {
                if g.done() {
                    return g.clone();
                }
                fab_paint_field::pattern::live::grown(g).unwrap_or_else(|| {
                    all = false;
                    g.clone()
                })
            };
            let program = p.program.resolve_patterns(&mut grow);
            let seen = p.seen.as_ref().map(|(s, colors)| (Arc::new(s.resolve_patterns(&mut grow)), colors.clone()));
            all.then(|| Some(Arc::new(PartPaint { program: Arc::new(program), seen, ..(**p).clone() })))
        };
        let mut ev = self.clone();
        for (i, p) in self.paint.iter().enumerate() {
            let Some(p) = p else { continue };
            if let Some(q) = grown(p)? {
                ev.plate.parts[i].paint = Some(spatial_paint(&q));
                ev.paint[i] = Some(q);
            }
        }
        for (i, p) in self.excluded_paint.iter().enumerate() {
            let Some(p) = p else { continue };
            if let Some(q) = grown(p)? {
                ev.excluded_paint[i] = Some(q);
            }
        }
        Some(ev)
    }

    /// Whether every pattern it shows on its way has grown the state it prints
    /// ([`Evaluated::with_patterns_grown`] has them all).
    pub fn patterns_ready(&self) -> bool {
        self.paint.iter().chain(&self.excluded_paint).flatten().all(|p| p.program.patterns().all(|g| g.done() || fab_paint_field::pattern::live::grown(g).is_some()))
    }

    /// How far the patterns still growing are toward the steps they print at: (steps, of), over
    /// them all.
    pub fn patterns_progress(&self) -> (u64, u64) {
        let pending = self.paint.iter().chain(&self.excluded_paint).flatten().flat_map(|p| p.program.patterns().filter(|g| !g.done()).map(fab_paint_field::pattern::live::progress).collect::<Vec<_>>());
        pending.fold((0, 0), |(a, b), (s, o)| (a + s as u64, b + o as u64))
    }

    pub fn placed_paint(&self, i: usize) -> Option<&Arc<PartPaint>> {
        let paint = if i < self.plate.parts.len() { self.paint.get(i) }
        else { self.excluded_paint.get(i - self.plate.parts.len()) };
        paint.and_then(Option::as_ref)
    }

    pub fn placed_features(&self, i: usize) -> Option<&Arc<Vec<crate::paint::Feature>>> {
        if i < self.plate.parts.len() { self.features.get(i) }
        else { self.excluded_features.get(i - self.plate.parts.len()) }
    }

    /// Placed part `i`'s force study (model frame), printed parts first, then excluded ones.
    pub fn placed_study(&self, i: usize) -> Option<&Arc<crate::study::Study>> {
        let s = if i < self.plate.parts.len() { self.studies.get(i) } else { self.excluded_studies.get(i - self.plate.parts.len()) };
        s.and_then(Option::as_ref)
    }

    pub fn blocking_issues(&self) -> usize {
        self.reports.iter().filter(|r| r.blocking()).count()
    }

    /// The plate's report with an error for each bond whose bends were measured and could not
    /// be ([`Mech::measure_flexes`], after the build): the failure's own text, at the flexible
    /// part. True when it changed.
    pub fn add_bend_issues(&mut self) -> bool {
        let mut report = (*self.printability).clone();
        let before = report.issues.len();
        for m in &self.mech {
            let Some(Err(f)) = m.flexes() else { continue };
            let message = f.to_string();
            if report.issues.iter().any(|i| i.kind == BAD_BOND && i.message == message) {
                continue;
            }
            // the flexible part of the bond that failed
            let to = m.bond_calls.iter().position(|(id, _)| *id == f.id).and_then(|b| m.plan.bonds.get(b)).map(|b| b.to);
            let part = to.and_then(|to| m.names.get(to)).and_then(|n| self.objects.iter().find(|o| o.name == *n)).and_then(|o| o.part).and_then(|p| self.plate.parts.get(p));
            let b = part.map(|p| p.bounds());
            let (min, max) = b.map_or(([0.0; 3], [0.0; 3]), |b| (b.min, b.max));
            report.issues.push(fab_printability::Issue {
                kind: BAD_BOND,
                severity: fab_printability::Severity::Error,
                object: part.map(|p| p.object.clone()),
                at_mm: std::array::from_fn(|k| 0.5 * (min[k] + max[k])),
                footprint: fab_printability::Footprint { min: [min[0], min[1]], max: [max[0], max[1]] },
                layer: None,
                layer_to: None,
                measure: 0.0,
                message,
                hint: "Print refuses the plate until the bond is fixed in its script.".into(),
                fix: None,
                area_mm2: 0.0,
                lowest_z: min[2],
                highest_z: max[2],
                flags: 0,
            });
        }
        if report.issues.len() == before {
            return false;
        }
        self.printability = Arc::new(report);
        true
    }
}

/// The report's kind for a bond whose bends cannot be measured ([`Evaluated::add_bend_issues`]).
pub const BAD_BOND: &str = "bad_bond";

#[derive(Clone, Debug)]
struct Placement {
    at: Option<[f64; 2]>,
    /// Degrees about the printer's X, Y and Z (applied X, then Y, then Z) about the part's
    /// model origin.
    rotate: [f64; 3],
    /// Factors along the printer's X, Y and Z of the unturned part, about its model origin
    /// (before the turn).
    scale: [f64; 3],
    drop: bool,
}

/// What the last evaluation built for one instance, for the inspection tools.
#[derive(Clone)]
struct Last {
    graph: Arc<Graph>,
    nodes: Vec<NodeId>,
    /// Per solid: `instance/solid`.
    solids: Vec<String>,
    /// Per solid: its name in the script (`solid`).
    ids: Vec<String>,
}

/// An [`Evaluator`]'s last build as its inspection tools see it ([`Evaluator::inspected`]).
pub struct Inspected(Option<Vec<Last>>);

/// Says why work must stop for memory (the app's budget), or nothing.
pub type MemoryGuard = std::sync::Arc<dyn Fn() -> Option<String> + Send + Sync>;

pub struct Evaluator {
    /// Says why an evaluation must stop for memory: a mesh in progress ends at its next check,
    /// and the evaluation fails with this reason.
    pub memory_guard: Option<MemoryGuard>,
    pub machine: Machine,
    /// What joints are sized for (nozzle, layer, material).
    pub profile: makepad_csg_mech::Profile,
    pub materials: fab_printability::Materials,
    /// The print colours on the printer (material colours, job tools), from the app's
    /// colour mapping.
    pub mapped: Arc<Mapped>,
    /// The calibrated filament sets kept ([`crate::colour_sets`]): what pictures print with.
    pub calibrated: Arc<crate::colour_sets::Sets>,
    /// A live drag's draft: that instance's SDF fields and shell cavities mesh at this many
    /// times their cell (2 is eight times fewer cells); every other instance, and every
    /// committed evaluation, at their cell (so their meshes stay cached).
    pub draft: Option<(String, f64)>,
    /// Mesh the parts made from a distance field too (Mesh shading, an export). Otherwise a
    /// field part is laid out, checked, sliced and drawn from its field alone: no mesh.
    pub mesh_fields: bool,
    /// Build for a still picture (the plates list's): every instance meshes at
    /// [`PICTURE_DRAFT`].
    pub picture: bool,
    /// A grown pattern's state at its steps before the build ends (a slice, a test, a picture);
    /// else the latest its simulation has, which runs on in [`fab_paint_field::pattern::live::work`]
    /// while the views follow it ([`Evaluated::patterns_pending`]).
    pub patterns_wait: bool,
    /// Evaluations run so far.
    pub evaluations: u64,
    /// A hold the job running the next build puts on it, taken by that build: a build for
    /// another job drops it unrun.
    pub gate: Option<BuildGate>,
    /// The tolerance a slice keeps to an exact part's surface (mm): the job's `resolution`.
    pub print_chord: f64,
    /// What the job prints round the parts: the layout keeps it where their nozzles reach.
    pub rim: Rim,
    /// How the view shows the parts ([`Self::set_view_chord`], [`Self::set_view_meshes`]).
    view: View,
    graph: makepad_csg_graph::Evaluator,
    last: Option<Vec<Last>>,
    generated: crate::generated::Cache,
    complete: bool,
    /// The program paired with the most recent successful evaluation.
    evaluated: Option<Compiled>,
    /// The previous evaluation's parts, by solid id: their frames and, a field part, its key.
    last_parts: Vec<(String, PartFrame, Option<[u8; 32]>, u64)>,
}

/// The draft a still picture's build meshes at ([`Evaluator::picture`]): twice the fields' and
/// shells' cells and the exact bodies' chord, still finer than a picture's pixel.
pub const PICTURE_DRAFT: f64 = 2.0;

/// How far past its solid a field part's box may reach where its proof closes (mm): it always
/// holds the solid.
pub const EXTENT_TOL: f64 = 0.01;

/// The extents proven so far ([`makepad_csg_sdf::shape_bounds`]), by shape key and linear map:
/// every evaluator's and the live slicer's ([`field_extent`]). A proof runs once per shape and
/// map, whoever asks first (a live drag's last value is its commit's shape); one asked while
/// it runs waits for it. The [`PROVEN_KEEP`] latest used are kept.
static PROVEN: std::sync::Mutex<Option<Proven>> = std::sync::Mutex::new(None);

/// Extents kept in [`PROVEN`] (each a few hundred bytes).
const PROVEN_KEEP: usize = 256;

type ProvenKey = ([u8; 32], [u64; 9]);

/// The field volumes read so far ([`crate::field_volume::field_volume`]), by shape key and cell,
/// with when each was last used: a field's volume is read once, whoever asks first. The
/// [`PROVEN_KEEP`] latest used are kept.
static VOLUMES: std::sync::Mutex<Option<(HashMap<([u8; 32], u64), (f64, u64)>, u64)>> = std::sync::Mutex::new(None);

/// The volume of the solid of the shape keyed `key` in its own frame, read at `cell`, if it has
/// been read.
fn volume_known(key: [u8; 32], cell: f64) -> Option<f64> {
    let mut guard = VOLUMES.lock().unwrap();
    let (map, tick) = guard.get_or_insert_with(Default::default);
    *tick += 1;
    let tick = *tick;
    map.get_mut(&(key, cell.to_bits())).map(|e| {
        e.1 = tick;
        e.0
    })
}

/// The volume of `shape`'s solid (key `key`) in its own frame, read at `cell`.
fn volume_of(shape: &makepad_csg_graph::SdfShape, key: [u8; 32], cell: f64) -> f64 {
    let k = (key, cell.to_bits());
    if let Some(volume) = volume_known(key, cell) {
        return volume;
    }
    let volume = crate::field_volume::field_volume(&Arc::new(makepad_csg_sdf::Chunks::of_shape(shape)), shape.min, shape.max, cell, shape.lipschitz());
    let mut guard = VOLUMES.lock().unwrap();
    let (map, tick) = guard.get_or_insert_with(Default::default);
    *tick += 1;
    if !map.contains_key(&k) && map.len() >= PROVEN_KEEP {
        if let Some(old) = map.iter().min_by_key(|(_, (_, t))| *t).map(|(k, _)| *k) {
            map.remove(&old);
        }
    }
    map.insert(k, (volume, *tick));
    volume
}

#[derive(Default)]
struct Proven {
    map: HashMap<ProvenKey, (Arc<Proof>, u64)>,
    tick: u64,
}

#[derive(Default)]
struct Proof {
    extent: std::sync::OnceLock<Option<Extent>>,
    bottom: std::sync::OnceLock<Option<makepad_csg_sdf::Bottom>>,
    /// The lowest z of the shape's tree box under the map ([`tree_low`]).
    tree_low: std::sync::OnceLock<f64>,
}

/// The shared proof cells for a shape and linear map.
fn proof(key: [u8; 32], l: &[[f64; 4]; 3]) -> Arc<Proof> {
    let k: ProvenKey = (key, std::array::from_fn(|i| l[i / 3][i % 3].to_bits()));
    {
        let mut guard = PROVEN.lock().unwrap();
        let p = guard.get_or_insert_with(Proven::default);
        p.tick += 1;
        let tick = p.tick;
        if !p.map.contains_key(&k) && p.map.len() >= PROVEN_KEEP {
            if let Some(old) = p.map.iter().min_by_key(|(_, (_, t))| *t).map(|(k, _)| *k) {
                p.map.remove(&old);
            }
        }
        let e = p.map.entry(k).or_insert_with(|| (Arc::new(Proof::default()), tick));
        e.1 = tick;
        e.0.clone()
    }
}

/// The shapes proven last ([`RECENT_KEEP`] of them), with their maps and extents: a new shape's
/// proof keeps each side of theirs that none of its differences reaches (an edit re-proves only
/// the sides it can move).
static RECENT: std::sync::Mutex<Vec<([u64; 9], Arc<makepad_csg_graph::SdfShape>, Extent)>> = std::sync::Mutex::new(Vec::new());

/// Shapes kept in [`RECENT`]: a part's last few edits and the plate's other parts.
const RECENT_KEEP: usize = 8;

/// The extent linear map `l` takes the solid of `shape` (key `key`) into, proven once, each side
/// an earlier shape's where no difference from it reaches the side
/// ([`makepad_csg_sdf::shape_bounds_after`]).
fn proven_extent(shape: &makepad_csg_graph::SdfShape, key: [u8; 32], l: &[[f64; 4]; 3]) -> Option<Extent> {
    proven(&proof(key, l).extent, || {
        let map: [u64; 9] = std::array::from_fn(|i| l[i / 3][i % 3].to_bits());
        let recent: Vec<(Arc<makepad_csg_graph::SdfShape>, Extent)> = RECENT.lock().unwrap().iter().filter(|(m, _, _)| *m == map).map(|(_, s, e)| (s.clone(), *e)).collect();
        let before: Vec<(&makepad_csg_graph::SdfShape, &Extent)> = recent.iter().rev().map(|(s, e)| (&**s, e)).collect();
        let e = makepad_csg_sdf::shape_bounds_after(shape, l, EXTENT_TOL, &before)?;
        if makepad_csg_math::thread_pool::cancelled() {
            return Some(e);
        }
        let mut kept = RECENT.lock().unwrap();
        if kept.len() >= RECENT_KEEP {
            kept.remove(0);
        }
        kept.push((map, Arc::new(shape.clone()), e));
        Some(e)
    })
}

/// A field proof kept in `cell`, proven under the cancel of the job this thread works for: a
/// cancel (its, or the worker's own) stops its search ([`makepad_csg_math::thread_pool::cancelled`])
/// with a looser answer, given to the build that stops with it and never kept.
fn proven<T: Copy>(cell: &std::sync::OnceLock<T>, prove: impl FnOnce() -> T) -> T {
    use makepad_csg_math::thread_pool::{cancelled, with_cancel, CancelToken};
    if let Some(known) = cell.get() {
        return *known;
    }
    let token = makepad_jobs::current().map(|job| CancelToken::of_flag(job.cancel_flag()));
    let answer = match &token {
        Some(token) => with_cancel(token, prove),
        None => prove(),
    };
    if cancelled() || token.is_some_and(|t| t.is_cancelled()) {
        return answer;
    }
    *cell.get_or_init(|| answer)
}

/// The extent `m` maps the solid of `shape` (key `key`) into, None when it holds nothing.
fn extent_of(shape: &makepad_csg_graph::SdfShape, key: [u8; 32], m: &[[f64; 4]; 3]) -> Option<Extent> {
    extent_by(m, &mut |l| proven_extent(shape, key, l))
}

/// Rest-only shapes do not use the other five extent sides. Keep the same canonical frame
/// and arithmetic as `extent_by`; a turn that maps another side to the bottom still uses
/// the full extent, whose witness for that side can depend on the other sides.
fn bottom_of(shape: &makepad_csg_graph::SdfShape, key: [u8; 32], m: &[[f64; 4]; 3]) -> Option<makepad_csg_sdf::Bottom> {
    let l = linear(m);
    let (l, scale) = if let Some(axes) = axis_map(&affine_mul(&l, &FROM_PRINTER)) {
        let (axis, scale) = axes[2];
        if axis != 2 || scale <= 0.0 {
            let e = extent_of(shape, key, m)?;
            return Some(makepad_csg_sdf::Bottom { bound: e.min[2], witness: e.found.map(|f| f.0[2]), work: e.work[0][2] });
        }
        (TO_PRINTER, scale)
    } else {
        (l, 1.0)
    };
    let cell = proof(key, &l);
    let bottom = if let Some(extent) = cell.extent.get() {
        extent.map(|e| makepad_csg_sdf::Bottom { bound: e.min[2], witness: e.found.map(|f| f.0[2]), work: e.work[0][2] })
    } else {
        proven(&cell.bottom, || makepad_csg_sdf::shape_bottom(shape, &l, EXTENT_TOL))
    }?;
    Some(makepad_csg_sdf::Bottom { bound: m[2][3] + scale * bottom.bound, witness: bottom.witness.map(|w| m[2][3] + scale * w), work: bottom.work })
}

/// The lowest z `m` maps `shape`'s tree box to ([`makepad_csg_sdf::tight_bounds`]; the shape's
/// box for a tree without one): no solid point of it lies below. No field read.
fn tree_low(shape: &makepad_csg_graph::SdfShape, key: [u8; 32], m: &[[f64; 4]; 3]) -> f64 {
    let l = linear(m);
    let low = *proof(key, &l).tree_low.get_or_init(|| {
        let (min, max) = makepad_csg_sdf::tight_bounds(&shape.root).unwrap_or((shape.min, shape.max));
        (0..3).map(|k| l[2][k] * if l[2][k] >= 0.0 { min[k] } else { max[k] }).sum()
    });
    low + m[2][3]
}

/// The box `m` maps the solid of `shape` (key `key`) into, None when it holds nothing: the box
/// the layout places a field part by and the slicer slices it over (`PartField::bounds` with `m`
/// its `to_part`), from the same proofs.
pub fn field_extent(shape: &makepad_csg_graph::SdfShape, key: [u8; 32], m: &[[f64; 4]; 3]) -> Option<Bounds> {
    extent_of(shape, key, m).map(|e| Bounds { min: e.min, max: e.max })
}

/// The extent `m` maps a solid into, from `search` (the extent under a linear map, no offset).
fn extent_by(m: &[[f64; 4]; 3], search: &mut dyn FnMut(&[[f64; 4]; 3]) -> Option<Extent>) -> Option<Extent> {
    let l = linear(m);
    // Axes swapped and flipped under one scale from the printer frame's (the model frame, quarter
    // turns): the printer-frame extent, its sides moved along, exactly. One search per shape,
    // and its bottom (proven first) the printer's.
    if let Some(axes) = axis_map(&affine_mul(&l, &FROM_PRINTER)) {
        let e = search(&TO_PRINTER)?;
        let map = |(lo, hi): ([f64; 3], [f64; 3])| -> ([f64; 3], [f64; 3]) {
            (
                std::array::from_fn(|i| { let (j, k) = axes[i]; m[i][3] + if k > 0.0 { k * lo[j] } else { k * hi[j] } }),
                std::array::from_fn(|i| { let (j, k) = axes[i]; m[i][3] + if k > 0.0 { k * hi[j] } else { k * lo[j] } }),
            )
        };
        let (min, max) = map((e.min, e.max));
        let work = std::array::from_fn(|side| std::array::from_fn(|i| {
            let (j, k) = axes[i];
            e.work[if k > 0.0 { side } else { 1 - side }][j]
        }));
        return Some(Extent { min, max, found: e.found.map(map), work, proofs: e.proofs });
    }
    let shift = |p: [f64; 3]| -> [f64; 3] { std::array::from_fn(|i| p[i] + m[i][3]) };
    search(&l).map(|e| Extent { min: shift(e.min), max: shift(e.max), found: e.found.map(|(a, b)| (shift(a), shift(b))), work: e.work, proofs: e.proofs })
}

/// `m`'s linear part (no offset).
fn linear(m: &[[f64; 4]; 3]) -> [[f64; 4]; 3] {
    [[m[0][0], m[0][1], m[0][2], 0.0], [m[1][0], m[1][1], m[1][2], 0.0], [m[2][0], m[2][1], m[2][2], 0.0]]
}

/// Per row of `m`'s linear part, the one axis it reads and its factor, when `m` swaps and flips
/// axes under one scale (entries a trillionth of the largest count as zero: a quarter turn's
/// cosine); None for any other map.
fn axis_map(m: &[[f64; 4]; 3]) -> Option<[(usize, f64); 3]> {
    let big = (0..9).map(|i| m[i / 3][i % 3].abs()).fold(0.0, f64::max);
    if big == 0.0 { return None; }
    let mut used = [false; 3];
    let mut axes = [(0, 0.0); 3];
    for (i, row) in m.iter().enumerate() {
        let mut found = None;
        for j in 0..3 {
            if row[j].abs() > big * 1e-12 {
                if found.is_some() || used[j] { return None; }
                found = Some((j, row[j]));
            }
        }
        let (j, k) = found?;
        if (k.abs() - big).abs() > big * 1e-12 { return None; }
        used[j] = true;
        axes[i] = (j, k.signum() * big);
    }
    Some(axes)
}

/// The model frame (Y up) to the printer frame (Z up): `printer = (x, -z, y)`.
const TO_PRINTER: [[f64; 4]; 3] = [[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, -1.0, 0.0], [0.0, 1.0, 0.0, 0.0]];

/// [`TO_PRINTER`]'s inverse: `model = (x, z, -y)`.
const FROM_PRINTER: [[f64; 4]; 3] = [[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, -1.0, 0.0, 0.0]];

/// An exact part's display mesh: `m` (model frame) placed by `frame` (model to plate), with its
/// per-corner normals turned along.
fn placed_render(m: &TriMesh, frame: &[[f64; 4]; 3]) -> RenderMesh {
    let normals = match m.exact_normals() {
        Some(normals) => normals.iter().map(|n| [n.x, n.y, n.z]).collect(),
        None => corner_normals(m),
    };
    let at = |v: [f64; 3]| -> [f32; 3] { std::array::from_fn(|r| (frame[r][0] * v[0] + frame[r][1] * v[1] + frame[r][2] * v[2] + frame[r][3]) as f32) };
    let turn = |n: [f64; 3]| -> [f32; 3] {
        let t: [f64; 3] = std::array::from_fn(|r| frame[r][0] * n[0] + frame[r][1] * n[1] + frame[r][2] * n[2]);
        let l = (t[0] * t[0] + t[1] * t[1] + t[2] * t[2]).sqrt();
        std::array::from_fn(|r| if l > 0.0 { (t[r] / l) as f32 } else { 0.0 })
    };
    let mut rm = RenderMesh::default();
    for (i, t) in m.triangles.iter().enumerate() {
        for k in 0..3 {
            let v = m.vertices[t[k] as usize];
            rm.indices.push(rm.positions.len() as u32);
            rm.positions.push(at([v.x, v.y, v.z]));
            rm.normals.push(turn(normals[i * 3 + k]));
        }
    }
    rm
}

/// Fallback normals for meshes without exact normals.
/// Each corner averages the faces round its vertex that bend
/// less than 30 degrees from its own face (creases stay sharp), each weighted by its
/// angle at the vertex. A flat face keeps its own normal at a vertex where it spans at
/// least a half-plane round it (its straight edge along a fillet, a chamfer): the true
/// surface there is the plane, and the fillet's first chord would otherwise tilt the
/// face's big triangles in saw-tooth shading across it.
fn corner_normals(m: &TriMesh) -> Vec<[f64; 3]> {
    let unit = |n: makepad_csg_math::Vec3d| {
        let l = n.length();
        if l > 1e-30 { [n.x / l, n.y / l, n.z / l] } else { [0.0; 3] }
    };
    let face: Vec<[f64; 3]> = m
        .triangles
        .iter()
        .map(|t| {
            let (a, b, c) = (m.vertices[t[0] as usize], m.vertices[t[1] as usize], m.vertices[t[2] as usize]);
            unit((b - a).cross(c - a))
        })
        .collect();
    // the triangle's angle at its corner k
    let corner_angle = |t: &[u32; 3], k: usize| {
        let p = m.vertices[t[k] as usize];
        let (e1, e2) = (m.vertices[t[(k + 1) % 3] as usize] - p, m.vertices[t[(k + 2) % 3] as usize] - p);
        let d = e1.length() * e2.length();
        if d > 1e-30 { (e1.dot(e2) / d).clamp(-1.0, 1.0).acos() } else { 0.0 }
    };
    let mut around: Vec<Vec<(u32, u8)>> = vec![Vec::new(); m.vertices.len()];
    for (i, t) in m.triangles.iter().enumerate() {
        for (k, v) in t.iter().enumerate() {
            around[*v as usize].push((i as u32, k as u8));
        }
    }
    let cos = 30f64.to_radians().cos();
    let flat = 0.5f64.to_radians().cos();
    let half_plane = std::f64::consts::PI * 0.98;
    let mut out = Vec::with_capacity(m.triangles.len() * 3);
    for (i, t) in m.triangles.iter().enumerate() {
        let own = face[i];
        for v in t {
            let mut sum = [0.0; 3];
            let mut coplanar = 0.0;
            for &(j, k) in &around[*v as usize] {
                let n = face[j as usize];
                let c = own[0] * n[0] + own[1] * n[1] + own[2] * n[2];
                if c >= cos {
                    let w = corner_angle(&m.triangles[j as usize], k as usize);
                    sum = [sum[0] + n[0] * w, sum[1] + n[1] * w, sum[2] + n[2] * w];
                    if c >= flat {
                        coplanar += w;
                    }
                }
            }
            if coplanar >= half_plane {
                out.push(own);
                continue;
            }
            let l = (sum[0] * sum[0] + sum[1] * sum[1] + sum[2] * sum[2]).sqrt();
            out.push(if l > 0.0 { [sum[0] / l, sum[1] / l, sum[2] / l] } else { own });
        }
    }
    out
}

fn mesh_bounds(m: &TriMesh) -> Option<([f64; 3], [f64; 3])> {
    if m.triangles.is_empty() {
        return None;
    }
    let b = m.bounding_box();
    Some(([b.min.x, b.min.y, b.min.z], [b.max.x, b.max.y, b.max.z]))
}

/// Solid `i`'s own placement as built, its `at` (mm), `rotate` (degrees) and `scale`; none when
/// it has none (a mesh's or an exact body's alike).
pub fn own_placement(p: &Program, i: usize) -> Option<([f64; 3], [f64; 3], [f64; 3])> {
    use makepad_csg_graph::{BrepOp, Op};
    match &p.graph.node(p.solids[i].node).op {
        Op::Transform { at, rotate, scale, .. } | Op::Brep { op: BrepOp::Place { at, rotate, scale }, .. } => Some((*at, *rotate, *scale)),
        _ => None,
    }
}

fn mat4(a: &[[f64; 4]; 3]) -> [[f64; 4]; 4] {
    [a[0], a[1], a[2], [0.0, 0.0, 0.0, 1.0]]
}

/// The plate's layer height (mm): its own setting, else the `layer_height` option's default.
fn layer_height(settings: &[(String, String)]) -> f64 {
    let own = settings.iter().rev().find(|(k, _)| k == "layer_height").and_then(|(_, v)| v.trim().trim_end_matches("mm").trim().parse().ok());
    own.or_else(|| makepad_fab_settings::options::options().get("layer_height").and_then(|d| d.default.parse().ok())).filter(|h: &f64| *h > 0.0).expect("a positive layer height")
}

/// The tolerance a slice keeps to an exact part's surface (mm) when the job names none: the
/// `resolution` option's default.
pub fn default_print_chord() -> f64 {
    makepad_fab_settings::options::options().get("resolution").and_then(|d| d.default.parse().ok()).filter(|r: &f64| *r > 0.0).expect("the resolution option has a positive default")
}

/// The step keys of the exact bodies an evaluation of `c` reads, where `head` is the plate's build
/// as it is: the engine holding every one, the evaluation builds no body. None when that is not
/// known before it builds, or there is no body to ask for: a part that is not exact bodies alone
/// (or whose joints are sized from its meshes) and is changed from `head` builds meshes of its own.
/// An unchanged one reads what the head's build made.
pub fn exact_keys(c: &Compiled, head: &Compiled) -> Option<Vec<makepad_csg_graph::Key>> {
    let solids = |p: &Program| p.solids.iter().map(|s| p.graph.node(s.node).key).collect::<Vec<_>>();
    let mut keys = Vec::new();
    for inst in &c.instances {
        let p = &inst.script.program;
        if !crate::mech::jointed(p) && p.solids.iter().all(|s| matches!(p.graph.node(s.node).op, makepad_csg_graph::Op::Brep { .. })) {
            keys.extend(p.solids.iter().flat_map(|s| makepad_csg_graph::eval::brep_steps(&p.graph, s.node)).map(|step| step.key));
        } else if head.instance(&inst.name).is_none_or(|h| solids(&h.script.program) != solids(p)) {
            return None;
        }
    }
    (!keys.is_empty()).then_some(keys)
}

/// An exact part's slicer mesh at `chord` (mm), cut from the engine's body of the part when a
/// slice asks.
fn print_mesh(steps: Vec<makepad_csg_graph::BrepStep>, m: [[f64; 4]; 3], chord: f64) -> fab_plate::PrintMesh {
    fab_plate::PrintMesh(Arc::new(move || {
        let tri = makepad_csg_brep_model::engine().mesh(steps.clone(), chord, None).map_err(|f| f.message)?;
        let at = |v: &makepad_csg_math::Vec3d, r: &[f64; 4]| r[0] * v.x + r[1] * v.y + r[2] * v.z + r[3];
        Ok(PartMesh {
            positions: tri.vertices.iter().map(|v| [at(v, &m[0]), at(v, &m[1]), at(v, &m[2])]).collect(),
            triangles: tri.triangles.clone(),
        })
    }))
}

const IDENTITY3: [[f64; 4]; 3] = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];

impl Evaluator {
    pub fn new(machine: Machine) -> Evaluator {
        let graph = makepad_csg_graph::Evaluator::with_brep(makepad_csg_brep_model::engine());
        Evaluator { memory_guard: None, machine, profile: Default::default(), materials: Default::default(), mapped: Default::default(), calibrated: Default::default(), draft: None, mesh_fields: false, picture: false, patterns_wait: true, evaluations: 0, gate: None, print_chord: default_print_chord(), rim: Rim::default(), view: View { chord: graph.brep_chord, meshes: false }, graph, last: None, last_parts: Vec::new(), generated: Default::default(), complete: false, evaluated: None }
    }

    /// A solid of the last evaluated project (`instance/solid`) as a model-frame mesh.
    pub fn object_mesh(&mut self, name: &str) -> Result<TriMesh, String> {
        let last = self.last.as_ref().ok_or("nothing is built yet")?;
        let Some((g, node)) = last.iter().enumerate().find_map(|(g, l)| l.solids.iter().position(|n| n == name).map(|k| (g, l.nodes[k]))) else {
            let names: Vec<&str> = last.iter().flat_map(|l| l.solids.iter().map(|n| n.as_str())).collect();
            return Err(format!("no solid '{name}' (solids: {})", names.join(", ")));
        };
        let last = self.last.as_ref().unwrap();
        self.graph.mesh(&last[g].graph, node, None).map(|m| (*m).clone()).map_err(|f| f.to_string())
    }

    /// The view's chord for exact parts (mm): their display meshes only. The model, its checks
    /// and its slice keep their own (the evaluator's chord, [`Self::print_chord`]): a zoom never
    /// changes what they see. True when it changed.
    pub fn set_view_chord(&mut self, chord: f64) -> bool {
        std::mem::replace(&mut self.view.chord, chord) != chord
    }

    /// Field parts shown as meshes (the Mesh look) or from their fields: their display meshes
    /// only (they are laid out, checked and sliced from their fields either way). True when it
    /// changed.
    pub fn set_view_meshes(&mut self, on: bool) -> bool {
        std::mem::replace(&mut self.view.meshes, on) != on
    }

    /// `ev` with its display meshes as the view shows them; `None` when they show so already.
    pub fn shown_at_view(&mut self, ev: &Evaluated) -> Result<Option<Evaluated>, String> {
        let meshes = self.view_meshes(ev)?;
        if meshes.is_empty() {
            return Ok(None);
        }
        let mut shown = ev.clone();
        shown.show_as(self.view, meshes);
        Ok(Some(shown))
    }

    /// The display meshes of `ev`'s parts (printed, then excluded) that the view shows otherwise
    /// than `ev` does, placed as `ev` places them. A drag's draft keeps the dragged instance's.
    fn view_meshes(&mut self, ev: &Evaluated) -> Result<Vec<(usize, RenderMesh)>, String> {
        let view = self.view;
        let mut out = Vec::new();
        for (i, shape) in ev.shapes.iter().enumerate() {
            let object = &ev.placed_part(i).object;
            let drafted = self.draft.as_ref().is_some_and(|(inst, _)| crate::program::split_id(object).is_some_and(|(name, _)| name == inst));
            let Some(frame) = ev.placed_frame(i).filter(|_| !drafted) else { continue };
            let mesh = match shape {
                Some(ViewShape::Exact(graph, node)) if ev.view.chord != view.chord => match self.graph.brep_mesh(graph, *node, view.chord, None) {
                    Some(m) => m.map_err(|f| format!("{object}: showing it at a {} mm chord: {}", view.chord, f.message))?,
                    None => continue,
                },
                Some(ViewShape::Field(graph, node)) if ev.view.meshes != view.meshes => {
                    if !view.meshes {
                        out.push((i, RenderMesh::default()));
                        continue;
                    }
                    self.graph.mesh(graph, *node, None).map_err(|f| format!("{object}: meshing its field to show it: {}", f.message))?
                }
                _ => continue,
            };
            out.push((i, placed_render(&mesh, frame)));
        }
        Ok(out)
    }

    /// The built graph solid `name` (`instance/solid`) is in, and its node there.
    fn solid_node(&self, name: &str) -> Result<(&Vec<Last>, usize, makepad_csg_graph::NodeId), String> {
        let last = self.last.as_ref().ok_or("nothing is built yet")?;
        let Some((g, node)) = last.iter().enumerate().find_map(|(g, l)| l.solids.iter().position(|n| n == name).map(|k| (g, l.nodes[k]))) else {
            return Err(format!("no solid '{name}'"));
        };
        Ok((last, g, node))
    }

    /// What of exact body `name` (`instance/solid`) is at model point `p`: the face there, and the
    /// sharp edge within `reach` mm with its tangent chain, by the ids of [`Self::object_exact`].
    /// `Ok(None)`: the solid is not an exact body.
    pub fn object_exact_at(&self, name: &str, p: [f64; 3], reach: f64) -> Result<Option<makepad_csg_graph::BrepAt>, String> {
        let (last, g, node) = self.solid_node(name)?;
        match self.graph.brep_point(&last[g].graph, node, p, reach) {
            None => Ok(None),
            Some(at) => at.map(Some).map_err(|f| f.message),
        }
    }

    /// An exact body's properties, edges and faces (`instance/solid`). `Ok(None)`: the solid is
    /// not an exact body, or a mesh operation made it one no longer.
    pub fn object_exact(&self, name: &str) -> Result<Option<ExactBody>, String> {
        let (last, g, node) = self.solid_node(name)?;
        let graph = &last[g].graph;
        let Some(measure) = self.graph.brep_measure(graph, node) else { return Ok(None) };
        let measure = measure.map_err(|f| f.message)?;
        let (edges, faces) = self.graph.brep_entities(graph, node).ok_or("not an exact body")?.map_err(|f| f.message)?;
        // Each part's own steps: its builder's and its placement's (a placed body keeps its faces'
        // names).
        let mut parts = HashMap::new();
        for (&solid, id) in last[g].nodes.iter().zip(&last[g].ids) {
            let mut at = solid;
            loop {
                match &graph.node(at).op {
                    makepad_csg_graph::Op::Tag { source, .. } => at = *source,
                    makepad_csg_graph::Op::Brep { op, inputs } => {
                        parts.entry(graph.node(at).key).or_insert_with(|| id.clone());
                        match (op, inputs.first()) {
                            (makepad_csg_graph::BrepOp::Place { .. }, Some(input)) => at = *input,
                            _ => break,
                        }
                    }
                    _ => break,
                }
            }
        }
        Ok(Some(ExactBody { measure, edges, faces, parts }))
    }

    /// Solids `names` of a compiled script as model-frame meshes (positions, triangles),
    /// built on this evaluator's cache without laying out a plate: a tool ghost at a live value.
    pub fn object_meshes(&mut self, script: &crate::program::Script, names: &[String], cancelled: Option<&dyn Fn() -> bool>) -> Result<Vec<(String, Vec<[f64; 3]>, Vec<[u32; 3]>)>, String> {
        self.graph.draft = 1.0;
        self.graph.begin();
        let mut out = Vec::new();
        for name in names {
            let Some(s) = script.program.solid(name) else { continue };
            let m = self.graph.mesh(&script.program.graph, s.node, cancelled).map_err(|f| f.to_string())?;
            out.push((name.clone(), m.vertices.iter().map(|v| [v.x, v.y, v.z]).collect(), m.triangles.clone()));
        }
        Ok(out)
    }

    /// The last project's solids (`instance/solid`).
    pub fn object_names(&self) -> Vec<String> {
        self.last.as_ref().map(|l| l.iter().flat_map(|l| l.solids.iter().cloned()).collect()).unwrap_or_default()
    }

    /// What the inspection tools see of the last build, to put back ([`Self::put_back`]) when
    /// a build after it does not come on the bed (its job was cancelled once it was built).
    pub fn inspected(&self) -> Inspected {
        Inspected(self.last.clone())
    }

    pub fn put_back(&mut self, was: Inspected) {
        self.last = was.0;
    }

    /// The frame a solid's own fields are written in: its placement after its parents'.
    fn frames(p: &Program) -> Vec<[[f64; 4]; 3]> {
        let own: Vec<[[f64; 4]; 3]> = (0..p.solids.len())
            .map(|i| own_placement(p, i).map_or(IDENTITY3, |(at, rotate, scale)| makepad_csg_graph::eval::transform_matrix(at, rotate, scale)))
            .collect();
        (0..p.solids.len())
            .map(|i| {
                let mut m = own[i];
                let mut at = p.solids[i].parent;
                while let Some(j) = at {
                    m = affine_mul(&own[j], &m);
                    at = p.solids[j].parent;
                }
                m
            })
            .collect()
    }

    /// Evaluate a compiled project: every instance's placed objects, laid out on one plate.
    ///
    /// The job this thread works for (`makepad_jobs::current`) stops it too: a person's cancel
    /// in the status bar ends the build at its next check, and its steps report on that job.
    pub fn evaluate(&mut self, c: &Compiled, cancelled: Option<&dyn Fn() -> bool>) -> Result<Evaluated, Failure> {
        let job = makepad_jobs::current();
        let asked = || cancelled.is_some_and(|c| c()) || job.as_ref().is_some_and(|j| j.is_cancelled());
        let Some(guard) = self.memory_guard.clone() else { return self.evaluate_unguarded(c, Some(&asked)) };
        let over: std::cell::RefCell<Option<String>> = Default::default();
        let last = std::cell::Cell::new(std::time::Instant::now());
        let stop = || {
            if asked() || over.borrow().is_some() {
                return true;
            }
            // reading the process's footprint is a syscall: a few times a second is plenty
            if last.get().elapsed().as_millis() < 100 {
                return false;
            }
            last.set(std::time::Instant::now());
            *over.borrow_mut() = guard();
            over.borrow().is_some()
        };
        self.evaluate_unguarded(c, Some(&stop)).map_err(|mut f| {
            if let Some(why) = over.borrow().clone() {
                f.message = why;
            }
            f
        })
    }

    /// Keep the compiled scripts paired with the generated meshes for callers that
    /// publish both. Bare `evaluate` also resolves requests, for mesh-only callers.
    pub(crate) fn evaluate_compiled(&mut self, c: Compiled, cancelled: Option<&dyn Fn() -> bool>) -> Result<(Compiled, Evaluated), Failure> {
        let ev = self.evaluate(&c, cancelled)?;
        Ok((self.evaluated.take().unwrap_or(c), ev))
    }

    fn evaluate_unguarded(&mut self, c: &Compiled, cancelled: Option<&dyn Fn() -> bool>) -> Result<Evaluated, Failure> {
        let started = std::time::Instant::now();
        self.evaluations += 1;
        self.evaluated = None;
        self.graph.begin();
        self.generated.begin();
        if c.instances.iter().all(|i| i.script.program.gen_requests.is_empty()) {
            let ev = self.evaluate_meshes(c, cancelled)?;
            self.complete = true;
            return Ok(ev);
        }
        let pending = c.instances.iter().any(|i| !i.script.program.gen_requests.is_empty() && i.script.generated.is_empty());
        // An unknown kind is a located authoring error, including on the first build.
        for inst in &c.instances {
            for request in &inst.script.program.gen_requests {
                if !inst.script.env.generators.contains_key(&request.kind) {
                    return Err(crate::generated::failure(&inst.script, request, format!("no generator for '{}'", request.kind)));
                }
            }
        }
        // Never publish this intermediate build over a completed one. With no completed
        // result yet, keep a first-pass image ready if resolution fails or is cancelled.
        // Some operations cannot mesh an empty operand; that does not prevent the
        // completed program from building once its generators have answered.
        let fallback = if pending && !self.complete { self.evaluate_meshes(c, cancelled).ok() } else { None };
        let inspected = self.last.clone();
        let last_parts = self.last_parts.clone();
        self.graph.draft = 1.0;
        let result = self.generated.resolve(c, &mut self.graph, cancelled).and_then(|resolved| {
            let ev = self.evaluate_meshes(&resolved, cancelled)?;
            if cancelled.is_some_and(|stop| stop()) {
                if let Some((script, request)) = resolved.instances.iter().find_map(|i| i.script.program.gen_requests.first().map(|r| (&i.script, r))) {
                    return Err(crate::generated::failure(script, request, "cancelled".into()));
                }
            }
            Ok((resolved, ev))
        });
        match result {
            Ok((resolved, mut ev)) => {
                self.complete = true;
                self.evaluated = Some(resolved);
                ev.millis = started.elapsed().as_secs_f64() * 1000.0;
                Ok(ev)
            }
            Err(failure) => {
                self.last = inspected;
                self.last_parts = last_parts;
                match fallback {
                    Some(mut ev) => {
                        ev.warnings.push(format!("generated parts pending: {failure}"));
                        self.evaluated = Some(c.clone());
                        ev.millis = started.elapsed().as_secs_f64() * 1000.0;
                        Ok(ev)
                    }
                    // The caller's existing failed-build path keeps its complete view,
                    // marked stale; an open request never replaces that view.
                    None => Err(failure),
                }
            }
        }
    }

    fn evaluate_meshes(&mut self, c: &Compiled, cancelled: Option<&dyn Fn() -> bool>) -> Result<Evaluated, Failure> {
        let started = std::time::Instant::now();
        let mut objects: Vec<ObjectInfo> = Vec::new();
        let mut parts: Vec<Part> = Vec::new();
        let mut printed = Vec::new();
        let mut visible = Vec::new();
        let mut attached = Vec::new();
        let mut inserts = Vec::new();
        let mut links: Vec<Option<String>> = Vec::new();
        let mut named: Vec<Option<(String, [[f64; 4]; 3])>> = Vec::new();
        let mut printable_count = 0;
        let mut render: Vec<RenderMesh> = Vec::new();
        let mut handles: Vec<PartHandles> = Vec::new();
        let mut placements: BTreeMap<String, Placement> = BTreeMap::new();
        // Per plate part: its object (the group it moves and drops with), instance and solid.
        let mut part_group: Vec<String> = Vec::new();
        let mut part_src: Vec<(usize, usize)> = Vec::new();
        // Per plate part: the body its display mesh follows the view from.
        let mut part_shapes: Vec<Option<ViewShape>> = Vec::new();
        // Per plate part: its distance field (a part that is one SDF solid), model frame.
        let mut part_fields: Vec<Option<(NodeId, Arc<makepad_csg_graph::SdfShape>, [u8; 32])>> = Vec::new();
        // Per plate part made from a field: its solid's volume in the field's frame, once the
        // drag is over.
        let mut part_volumes: Vec<Option<FieldVolume>> = Vec::new();
        let mut part_rests = Vec::new();
        let mut part_solids = Vec::new();
        let mut warnings = Vec::new();
        let mut built: Vec<Last> = Vec::with_capacity(c.instances.len());
        let mut mechs: Vec<Mech> = Vec::new();
        // The print colours: the project's, and an implicit one for what names none.
        let mut colors = c.colors.clone();
        let mut part_colors: Vec<String> = Vec::new();
        let mapped = self.mapped.clone();
        let inks = &mapped.inks;
        // The build's steps on the job it works for ([`Steps`]); deep code (the B-rep kernel)
        // reports inside the step it runs in.
        let mut steps = Steps::new(c, self.gate.take(), cancelled);
        // A cancel ends the build at its next part or check: no step begins past it ([`Steps::next`])
        // and none of them runs long past it.
        let halt = || if cancelled.is_some_and(|stop| stop()) { Err(Failure::cancelled()) } else { Ok(()) };
        for (ii, inst) in c.instances.iter().enumerate() {
            let emitted: HashMap<&str, &crate::program::Emitted> = inst.script.objects.iter().map(|o| (o.name.as_str(), o)).collect();
            self.graph.draft = self.draft.as_ref().filter(|(i, _)| *i == inst.name).map_or(if self.picture { PICTURE_DRAFT } else { 1.0 }, |(_, f)| *f);
            let p = &inst.script.program;
            let file = inst.script.file.as_str();
            warnings.extend(p.notes.iter().map(|n| format!("{file}:{}:{}: {}", n.line, n.col, n.message)));
            // where the instance's splits that placed no pins say so, once built
            let notes_end = warnings.len();
            let fail = |id: usize, f: makepad_csg_graph::Failure| {
                // The failing node's solid (the innermost one that owns it), else the placed one.
                // (a placed body's step sits under its placement)
                let placed_on = |n: NodeId| matches!(&p.graph.node(n).op, makepad_csg_graph::Op::Brep { op: makepad_csg_graph::BrepOp::Place { .. }, inputs } if inputs.first() == Some(&f.node));
                let owner = p.solids.iter().position(|s| s.node == f.node || placed_on(s.node)).unwrap_or(id);
                let s = &p.solids[owner];
                let (line, col) = s.loc.unwrap_or((1, 1));
                // A lost selector says which statement and what it can do next.
                let message = match f.lost {
                    Some(_) => {
                        let (op, kind, verb) = lost_words(s.proto);
                        let why = f.message.strip_prefix(&format!("{op}: ")).unwrap_or(&f.message);
                        format!("`{}`'s {op} lost its {kind} ({why}): pick the {kind} it should {verb}, or name a new selector", s.id)
                    }
                    None => f.message,
                };
                Failure { lost: f.lost, file: file.to_string(), line, col, id: s.id.clone(), op: s.proto.to_string(), message }
            };
            // Joints: sized from the parts' own meshes, written into new nodes over them.
            let mech = {
                let graph = &mut self.graph;
                let sizing = steps.next("sizing the joints", joint_solids(p))?;
                // the jointed parts meshed side by side first, each part's mesh sizing its joints
                let jointed: Vec<NodeId> = if crate::mech::jointed(p) { p.joints.iter().flat_map(|j| std::iter::once(j.a).chain(j.b).chain(j.shoe)).map(|s| p.solids[s].node).collect() } else { Vec::new() };
                graph.mesh_ahead(&p.graph, &jointed, cancelled);
                let mut sized = 0;
                let mut mesh = |s: usize| {
                    // each part the joints mesh, its share of the step
                    let _share = share(sizing.as_ref(), sized, joint_solids(p), shown(&p.solids[s].id));
                    sized += 1;
                    graph.mesh(&p.graph, p.solids[s].node, cancelled).map_err(|f| f.message)
                };
                crate::mech::plan(p, &self.profile, &mut mesh).map_err(|e| {
                    let (line, col) = e.loc.unwrap_or((1, 1));
                    Failure { lost: None, file: file.to_string(), line, col, id: e.id, op: "joint".into(), message: e.message }
                })?
            };
            let (graph, nodes): (Arc<Graph>, Vec<NodeId>) = match &mech {
                Some(m) => (Arc::new(m.graph.clone()), m.nodes.clone()),
                None => (Arc::new(p.graph.clone()), p.solids.iter().map(|s| s.node).collect()),
            };
            // A placed solid that is one distance field is not meshed (unless asked), its eye on
            // or off: its field is its geometry for the layout, the checks, the slicer and the views.
            let mut meshes: Vec<(Option<Arc<TriMesh>>, Option<(NodeId, Arc<makepad_csg_graph::SdfShape>, [u8; 32])>)> = Vec::with_capacity(inst.placed.len());
            // The shape strokes of the strokes file the script reads sculpt its fields.
            let sculpt = crate::program::strokes_files(inst.script.source()).into_iter().find_map(|f| c.strokes.get(&f).filter(|l| l.has_shape()).cloned());
            for (k, placed) in inst.placed.iter().enumerate() {
                let of = if inst.placed.len() > 1 { format!(" {}/{}", k + 1, inst.placed.len()) } else { String::new() };
                let _step = steps.enter(format!("building {}{of}", shown(&p.solids[placed.solid].id)), 1)?;
                let field = field_of(&graph, nodes[placed.solid], &p.solids[placed.solid].id).map(|(node, shape)| {
                    let shape = match &sculpt {
                        Some(strokes) => shape.sculpted(strokes.clone()),
                        None => shape,
                    };
                    let key = shape.key();
                    (node, Arc::new(shape), key)
                });
                let mesh = match field {
                    Some(_) if !self.mesh_fields && emitted[placed.object.as_str()].insert.is_none() => None,
                    _ => Some(self.graph.mesh(&graph, nodes[placed.solid], cancelled).map_err(|f| fail(placed.solid, f))?),
                };
                meshes.push((mesh, field));
            }
            // measured one by one, then the solids inside them
            let measuring = steps.next("measuring the parts", inst.placed.len() + 1)?;
            let frames = Self::frames(p);
            let base = objects.len();
            objects.extend(p.solids.iter().enumerate().map(|(i, s)| ObjectInfo { name: part_id(&inst.name, &s.id), printed: false, part: None, triangles: 0, world: mat4(&frames[i]), bounds: None, exact: None, frame: s.frame.as_ref().map(|(name, at, rotate)| (name.clone(), affine_mul(&frames[i], &makepad_csg_graph::eval::transform_matrix(*at, *rotate, [1.0; 3])))) }));
            let fields: Vec<Option<Arc<makepad_csg_graph::SdfShape>>> = meshes.iter().map(|(_, f)| f.as_ref().map(|(_, shape, _)| shape.clone())).collect();
            // each placed solid's print colour (a bond to a flexible one bends)
            let mut solid_colors: Vec<(usize, String)> = Vec::new();
            // the solids the script fuses on purpose: a bond's part and each it bonds, a braked
            // hinge's turning part and its shoe (it prints in that part)
            let fused_pairs: Vec<(usize, usize)> = p.joints.iter().filter_map(|j| match (j.proto, j.b, j.shoe) {
                ("Bond", Some(b), _) => Some((j.a, b)),
                (_, Some(b), Some(shoe)) => Some((b, shoe)),
                _ => None,
            }).collect();
            for (k, (placed, (m, field))) in inst.placed.iter().zip(meshes).enumerate() {
                halt()?;
                let _share = share(measuring.as_ref(), k, inst.placed.len() + 1, shown(&p.solids[placed.solid].id));
                let id = part_id(&inst.name, &p.solids[placed.solid].id);
                let emission = emitted[placed.object.as_str()];
                let group = part_id(&inst.name, emission.insert.as_ref().map_or(placed.object.as_str(), |i| i.into.as_str()));
                let field_box = field.as_ref().and_then(|(_, shape, key)| extent_of(shape, *key, &IDENTITY3));
                let info = &mut objects[base + placed.solid];
                info.triangles = m.as_ref().map_or(0, |m| m.triangles.len());
                info.bounds = match &field {
                    Some(_) => field_box.map(|e| (e.min, e.max)),
                    None => m.as_deref().and_then(mesh_bounds),
                };
                let prints = placed.print && placed.show && emission.insert.is_none();
                info.printed = prints;
                if info.bounds.is_none() {
                    continue;
                }
                // An assembly's bodies are named by their label, else by the object and the body; the
                // first body an object lists is the object itself, so it takes the object's name.
                let listed = inst.script.object(&placed.object);
                let assembly = listed.is_some_and(|o| o.bodies.len() > 1);
                let first = listed.is_some_and(|o| o.glued.first() == Some(&placed.solid));
                let object_name = placed.name.as_deref().unwrap_or(&placed.object);
                let label = match &p.solids[placed.solid].label {
                    _ if !assembly => object_name.to_string(),
                    Some(label) => label.clone(),
                    None if first => object_name.to_string(),
                    None => format!("{object_name} {}", p.solids[placed.solid].id),
                };
                let printer_box = field.as_ref().and_then(|(_, shape, key)| extent_of(shape, *key, &TO_PRINTER)).map(|e| Bounds { min: e.min, max: e.max });
                halt()?;
                // A body filled whole (`m.fill(body, material)`) prints in that material all through,
                // its first layer and its inside too, as its own colour.
                let whole = inst.script.paint.get(&placed.solid).and_then(|op| op.whole()).map(|(w, _)| swatch_color(&placed.paint, &w, &mut colors));
                // else the placement's, else the project's first, else an implicit first colour
                let color = match whole.or_else(|| placed.color.clone()).or_else(|| c.colors.first().map(|k| k.name.clone())) {
                    Some(name) => name,
                    None => {
                        if !colors.iter().any(|k| k.name == FIRST_COLOUR) {
                            let d = default_color(0);
                            colors.push(fab_plate::PrintColor { name: FIRST_COLOUR.into(), rgb: None, fallback: [d[0], d[1], d[2]], design: None, at: Vec::new(), material: None, implicit: true });
                        }
                        FIRST_COLOUR.into()
                    }
                };
                let (mut part, rm) = self.part(&id, &label, placed, shown_color(&colors, inks, &color), m.as_deref(), printer_box, &mut warnings);
                // Reading optics costs O(1) per solid. Each emitted body carries its own record,
                // including an insert; an object list does not share its first body's optics.
                part.optics = p.solids[placed.solid].optics.map(|o| fab_plate::Optics {
                    throughput: o.throughput,
                    source: match o.source {
                        makepad_model_script::lang::OpticsSource::Estimate => fab_plate::OpticsSource::Estimate,
                        makepad_model_script::lang::OpticsSource::Measured => fab_plate::OpticsSource::Measured,
                    },
                });
                // Each body keeps its own host key (a light pipe's names the LED it carries).
                part.link = p.solids[placed.solid].link.clone();
                // Reading keepouts costs O(points) per solid. Each emitted body or insert carries
                // its own records in the board's frame; a boolean does not inherit its operands'.
                part.keepouts = p.solids[placed.solid].keepouts.iter().map(|k| fab_plate::Keepout {
                    board: k.board.clone(),
                    name: k.name.clone(),
                    area: k.area.clone(),
                    z: k.z,
                }).collect();
                // The bodies an object lists print as one: its placement's id glues them.
                if inst.script.object(&placed.object).is_some_and(|o| o.glued.contains(&placed.solid)) {
                    part.body_of = Some(group.clone());
                }
                part.fused = fused_pairs.iter().filter_map(|&(x, y)| if x == placed.solid { Some(y) } else if y == placed.solid { Some(x) } else { None }).map(|s| part_id(&inst.name, &p.solids[s].id)).collect();
                part.fused.sort();
                part.fused.dedup();
                solid_colors.push((placed.solid, color.clone()));
                part_colors.push(color);
                info.part = prints.then_some(printable_count);
                printable_count += usize::from(prints);
                // What a part weighs is worth one more question to the engine, once the drag is over.
                if self.graph.draft == 1.0 {
                    match self.graph.brep_measure(&graph, nodes[placed.solid]) {
                        Some(Ok(m)) => info.exact = Some(m),
                        Some(Err(f)) => warnings.push(format!("{id}: no exact measure: {}", f.message)),
                        None => {}
                    }
                }
                printed.push(prints);
                visible.push(placed.show);
                attached.push(emission.insert.is_some());
                links.push(emission.link.clone());
                named.push(info.frame.clone());
                inserts.push(emission.insert.as_ref().map(|i| fab_plate::Insert {
                    into: part_id(&inst.name, &p.solids[emitted[i.into.as_str()].bodies[0]].id),
                    clearance: i.clearance,
                    embed: i.embed,
                    melt: i.melt.clone(),
                }));
                parts.push(part);
                render.push(rm);
                let node = nodes[placed.solid];
                part_shapes.push(match &graph.node(node).op {
                    makepad_csg_graph::Op::Brep { .. } => Some(ViewShape::Exact(graph.clone(), node)),
                    _ if field.is_some() => Some(ViewShape::Field(graph.clone(), node)),
                    _ => None,
                });
                // Joint booleans mesh their operands, but those vertices approximate the
                // surface. Keep the analytic solid for the bed contact proof.
                part_rests.push(field.as_ref().map(|(_, s, key)| (s.clone(), *key)).or_else(|| crate::assembly_rest::shape(&graph, node).map(|s| { let key = s.key(); (Arc::new(s), key) })));
                part_solids.push(field.as_ref().map(|(_, s, _)| s.clone()).or_else(|| crate::assembly_rest::solid(&graph, node).map(Arc::new)));
                part_volumes.push(field.as_ref().filter(|_| self.graph.draft == 1.0).map(|(_, shape, key)| {
                    let v = |p: [f64; 3]| makepad_csg_math::Vec3d::new(p[0], p[1], p[2]);
                    let cell = field_cell(&graph, node).unwrap_or_else(|| makepad_csg_sdf::default_cell(v(shape.min), v(shape.max), self.graph.finest_cell));
                    match volume_known(*key, cell) {
                        Some(volume) => FieldVolume::known_at(volume),
                        None => {
                            let (shape, key) = (shape.clone(), *key);
                            FieldVolume::pending(move || volume_of(&shape, key, cell))
                        }
                    }
                }));
                part_fields.push(field);
                handles.push(PartHandles { part: Some(id.clone()), place: Some(group.clone()), at: placed.at, rotate: placed.rotate, scale: placed.scale, drop: placed.drop, hidden: placed.hidden });
                placements.insert(group.clone(), Placement { at: placed.at, rotate: placed.rotate, scale: placed.scale, drop: placed.drop });
                part_group.push(group);
                part_src.push((ii, placed.solid));
            }
            let _inside = share(measuring.as_ref(), inst.placed.len(), inst.placed.len() + 1, "the parts inside");
            // Bounds of the solids that are part of placed ones (for highlights and handles), no
            // search (a highlight needs no tight box): inside a placed field, its subtree tagged
            // with the solid (every operand is lowered into the part's field, a carve's box
            // cutter too, and nothing is meshed for them); a field of its own, its node's box.
            for i in 0..p.solids.len() {
                let Some(r) = inst.placed.iter().position(|pl| is_within(p, i, pl.solid)) else { continue };
                if objects[base + i].bounds.is_some() {
                    continue;
                }
                let root_field = fields[r].as_deref();
                if let Some(b) = root_field.and_then(|f| tagged_bounds(&f.root, makepad_csg_sdf::part_id(&p.solids[i].id))) {
                    objects[base + i].bounds = Some(b);
                } else if let Some((_, shape)) = field_of(&graph, nodes[i], &p.solids[i].id) {
                    objects[base + i].bounds = Some((shape.min, shape.max));
                } else if self.mesh_fields || root_field.is_none() {
                    if let Ok(m) = self.graph.mesh(&graph, nodes[i], cancelled) {
                        objects[base + i].triangles = m.triangles.len();
                        objects[base + i].bounds = mesh_bounds(&m);
                    }
                }
            }
            // The areas the script marks (`m.no_trees`, `m.settings`): their own boxes, for
            // their handles and the gizmo.
            for i in crate::program::area_solids(p) {
                if objects[base + i].bounds.is_some() {
                    continue;
                }
                if let Some((_, shape)) = field_of(&graph, nodes[i], &p.solids[i].id) {
                    objects[base + i].bounds = Some((shape.min, shape.max));
                } else if let Ok(m) = self.graph.mesh(&graph, nodes[i], cancelled) {
                    objects[base + i].triangles = m.triangles.len();
                    objects[base + i].bounds = mesh_bounds(&m);
                }
            }
            // A mechanism with a missing body cannot be simulated as the complete assembly.
            let _fitting = steps.enter("fitting the joints", usize::from(crate::mech::jointed(p)))?;
            if let Some(mut m) = mech.filter(|m| m.solids.iter().all(|s| inst.placed.iter().any(|pl| pl.solid == *s && pl.print))) {
                let mut finals = Vec::new();
                for &s in &m.solids {
                    finals.push(self.graph.mesh(&m.graph, m.nodes[s], cancelled).map_err(|f| fail(s, f))?);
                }
                m.finals = finals;
                // a part bonded to one of a flexible material bends about it
                for bond in &mut m.plan.bonds {
                    let solid = m.solids[bond.to];
                    let material = solid_colors.iter().find(|(s, _)| *s == solid).and_then(|(_, name)| colors.iter().find(|k| k.name == *name)).and_then(|k| k.material.as_deref());
                    bond.flexible = material.is_some_and(fab_plate::flexible);
                }
                // how they bend is measured after the build ([`Mech::measure_flexes`])
                m.instance = inst.name.clone();
                m.file = file.to_string();
                m.names = m.names.iter().map(|n| part_id(&inst.name, n)).collect();
                mechs.push(m);
            }
            // A split asked for pins whose cut had room for none says so beside the script's
            // notes (the status line shows the first), with the pin that would stand: a lookup
            // per split piece, nothing built; its pieces say it once.
            let mut pinless: Vec<String> = Vec::new();
            for s in p.solids.iter().filter(|s| s.proto == "Split") {
                let Some(StepNote::SplitWithoutPins(no_room)) = self.graph.note(&p.graph, made_node(&p.graph, s.node)) else { continue };
                let (line, col) = s.loc.unwrap_or((1, 1));
                let text = format!("{file}:{line}:{col}: {}", split_without_pins(no_room));
                if !pinless.contains(&text) {
                    pinless.push(text);
                }
            }
            warnings.splice(notes_end..notes_end, pinless);
            built.push(Last { graph, nodes, solids: p.solids.iter().map(|s| part_id(&inst.name, &s.id)).collect(), ids: p.solids.iter().map(|s| s.id.clone()).collect() });
        }
        if parts.is_empty() && c.instances.iter().any(|i| !i.script.program.solids.is_empty()) {
            warnings.push("nothing is on the plate: a script emits its objects with object(name)".into());
        }
        let meshed_ms = started.elapsed().as_secs_f64() * 1000.0;
        let plate_step = steps.next("laying out the plate", 1)?;
        let print_steps: Vec<Option<Vec<makepad_csg_graph::BrepStep>>> = part_src
            .iter()
            .map(|(ii, s)| {
                let inst = &c.instances[*ii];
                // Only the dragged instance slices its draft; every other part keeps its print
                // mesh (same steps, same mesh: the slicer's caches hit and it does no work).
                if self.draft.as_ref().is_some_and(|(i, _)| *i == inst.name) {
                    return None;
                }
                let (graph, node) = (&inst.script.program.graph, inst.script.program.solids[*s].node);
                matches!(graph.node(node).op, makepad_csg_graph::Op::Brep { .. }).then(|| makepad_csg_graph::eval::brep_steps(graph, node))
            })
            .collect();
        let by_object = c.settings.iter().rev().find(|(k, _)| k == "print_sequence").is_some_and(|(_, v)| v == "by object");
        // A grown pattern grows on its part's own shape in the model frame: a mesh part's before
        // the layout turns and moves it (exactly the model's, under TO_PRINTER), so where it
        // stands never changes the pattern.
        let pattern_meshes: Vec<Option<Arc<PartMesh>>> = parts
            .iter()
            .zip(&part_src)
            .map(|(part, (ii, s))| {
                let inst = &c.instances[*ii];
                let op = inst.script.paint.get(&inst.placed_of(*s)?.solid)?;
                (op.program.has_patterns() && !part.mesh.triangles.is_empty()).then(|| Arc::new(PartMesh { positions: part.mesh.positions.iter().map(|q| apply_affine(&FROM_PRINTER, *q)).collect(), triangles: part.mesh.triangles.clone() }))
            })
            .collect();
        // What each part's paint prints with, before the layout places it.
        let mut picture_errors = Vec::new();
        let plans: Vec<Option<PaintPlan>> = parts
            .iter()
            .zip(&part_src)
            .map(|(part, (ii, s))| {
                let inst = &c.instances[*ii];
                let placed = inst.placed_of(*s)?;
                let op = inst.script.paint.get(&placed.solid)?;
                // a body filled whole prints in that material as its own (its colour, above): the
                // paint is what goes over it after
                let op = match op.whole() {
                    Some((_, after)) if after.program.is_empty() => return None,
                    Some((_, after)) => after,
                    None => op.clone(),
                };
                let op = op.bind(&c.strokes);
                // a picture's raster is made from the filaments it prints with: its first guess's,
                // else the job's print colours (each a filament the printer loads), as they show
                let mut pictures = Vec::new();
                let op = if op.program.has_rasters() {
                    let loaded: Vec<(String, [f32; 3])> = colors.iter().filter(|c| !c.implicit).map(|c| (c.name.clone(), shown_color(&colors, inks, &c.name))).collect();
                    let mut palette = (*op.palette).clone();
                    for spec in crate::picture::raster_specs(&op.program) {
                        let set = match crate::picture::set_of(spec, &loaded, &self.calibrated) {
                            Ok(set) => set,
                            Err(e) => {
                                picture_errors.push(format!("'{}': {e}", part.name));
                                continue;
                            }
                        };
                        let slots: Vec<u8> = set
                            .filaments
                            .iter()
                            .map(|f| {
                                let w = crate::paint::Swatch { rgb: f.colour, name: Some(f.name.clone()) };
                                let i = palette.iter().position(|p| *p == w).unwrap_or_else(|| {
                                    palette.push(w);
                                    palette.len() - 1
                                });
                                (i + 1).min(255) as u8
                            })
                            .collect();
                        pictures.push((fab_paint_field::raster::RasterSpec::key(spec), set, slots));
                    }
                    crate::paint::ObjectPaint { program: op.program, palette: Arc::new(palette), settings: op.settings }
                } else {
                    op
                };
                let own = fab_plate::paint::PaintColor { rgb: [part.color[0], part.color[1], part.color[2]], color: None, filament: None, nozzle: None };
                let colors: Vec<fab_plate::paint::PaintColor> = std::iter::once(own)
                    .chain(op.palette.iter().map(|w| {
                        let name = swatch_color(&placed.paint, w, &mut colors);
                        fab_plate::paint::PaintColor { rgb: shown_color(&colors, inks, &name), color: Some(name), filament: None, nozzle: None }
                    }))
                    .collect();
                Some(PaintPlan { op, pictures, colors })
            })
            .collect();
        // Each object stands where every nozzle its colours print with reaches.
        let nozzles = object_nozzles(&plans, &part_colors, &printed, &attached, &part_group, &colors, &mapped);
        let (model_to_plate, frames, in_part, rest_gaps, unplaced) = layout(&self.machine, &self.rim, by_object, &mut parts, &mut render, &placements, &part_group, &nozzles, &part_fields, &part_rests, &part_solids, &attached, layer_height(&c.settings), &mut |shape, key, m| extent_of(shape, key, m));
        halt()?;
        warnings.extend(unplaced.into_iter().map(|(g, size)| no_room(&self.machine, &g, size, &nozzles[&g])));
        for ((part, steps), m) in parts.iter_mut().zip(print_steps).zip(&model_to_plate) {
            // A drag slices what it shows; the finished part slices at print tolerance.
            part.print_mesh = steps.map(|steps| print_mesh(steps, *m, self.print_chord));
        }
        let fields: Vec<Option<PartField>> = part_fields
            .into_iter()
            .zip(&model_to_plate)
            .zip(&in_part)
            .zip(part_volumes)
            .map(|(((f, m), (to_part, bounds, bottom_gap)), volume)| {
                f.map(|(node, shape, key)| {
                    let stretched = Stretched::of(&shape, m, to_part);
                    PartField { node, key, shape, to_printer: *m, to_part: *to_part, bounds: *bounds, bottom_gap: *bottom_gap, volume: volume.map(|v| v.scaled(det3(to_part).abs())), stretched }
                })
            })
            .collect();
        // How far a field part may sit into the bed is said, never silent.
        for ((part, f), attached) in parts.iter().zip(&fields).zip(&attached) {
            if *attached { continue; }
            match f.as_ref().map(|f| f.bottom_gap) {
                Some(Some(gap)) if gap > EXTENT_TOL => warnings.push(format!("'{}' may sit up to {gap:.3} mm into the bed: its field's bottom is proven only that close (a Splash field's bounds should be its solid's box)", part.name)),
                Some(None) => warnings.push(format!("'{}' rests on its field's box (no solid point of it was found): it may stand off the bed", part.name)),
                _ => {}
            }
        }
        // And an assembly part's, where the shape it rests by may reach below its object's rest.
        for (i, gap) in rest_gaps {
            warnings.push(format!("'{}' may sit up to {gap:.3} mm into the bed: the bottom of the shape it rests on is proven only that close", parts[i].name));
        }
        halt()?;
        let stop = || cancelled.is_some_and(|c| c());
        if let Some(step) = &plate_step {
            step.stage("painting", None, Some(0.4));
        }
        let nozzle = self.profile.nozzle;
        let paint: Vec<Option<Arc<PartPaint>>> = parts
            .iter()
            .zip(&model_to_plate)
            .zip(&part_src)
            .zip(&fields)
            .zip(&pattern_meshes)
            .zip(plans)
            .map(|(((((part, m), (ii, s)), field), pattern_mesh), plan)| {
                let PaintPlan { op, pictures, colors } = plan?;
                // each picture's raster made from its filaments; per picture its tile and the
                // slots of its filaments, for the face the views show
                let mut faces: Vec<(u64, Arc<dyn fab_paint_field::raster::RasterSpec>, fab_colour::tile::Tile, Vec<u8>)> = Vec::new();
                let program = if op.program.has_rasters() {
                    Arc::new(op.program.resolve_rasters(&mut |r| {
                        let spec = r.spec.as_any().downcast_ref::<crate::picture::Spec>()?;
                        let (_, set, slots) = pictures.iter().find(|p| p.0 == r.spec.key())?;
                        let (grid, tile) = crate::picture::grid(spec, set, nozzle);
                        faces.push((r.spec.key(), r.spec.clone(), tile, slots.clone()));
                        Some((grid, slots.clone()))
                    }))
                } else {
                    op.program
                };
                let from_printer = invert_affine(m)?;
                // a smooth-region flood is resolved on this part's current surface
                let program = if program.has_floods() {
                    Arc::new(program.resolve_floods(&mut |seed, angle| flood_on(part, field.as_ref(), &from_printer, seed, angle)))
                } else {
                    program
                };
                // a grown pattern grows on this part's current shape
                let program = if program.has_patterns() {
                    let mut k = 0;
                    Arc::new(program.resolve_patterns(&mut |g| {
                        k += 1;
                        grow_on(field.as_ref(), pattern_mesh.as_ref(), g, &part.object, k, self.patterns_wait, &stop)
                    }))
                } else {
                    program
                };
                // a weave paints whole links: each takes the material the fill gives at its centre
                let program = match crate::weave::links_of(&built[*ii].graph, built[*ii].nodes[*s]) {
                    Some((links, local)) => Arc::new(crate::weave::paint_by_link(&program, &links, &local)),
                    None => program,
                };
                // a field region with no box of its own reads as steep as it is over this part
                let program = if program.has_unboxed_fields() {
                    let b = part.bounds();
                    Arc::new(program.resolve_slopes((b.min, b.max), &from_printer))
                } else {
                    program
                };
                // a picture's face shows its stacks' colours, after the slots it prints with
                let seen = (!faces.is_empty()).then(|| {
                    let mut shown: Vec<[f32; 3]> = colors.iter().map(|c| c.rgb).collect();
                    let program = program.resolve_rasters(&mut |r| {
                        let (_, spec, tile, slots) = faces.iter().find(|f| f.0 == r.spec.key())?;
                        let spec = spec.as_any().downcast_ref::<crate::picture::Spec>()?;
                        let (grid, inks) = crate::picture::face_view(spec, tile, slots.len(), 255usize.saturating_sub(shown.len()));
                        let base = shown.len();
                        shown.extend(&inks);
                        Some((grid, slots.iter().copied().chain((0..inks.len()).map(|k| (base + k) as u8)).collect()))
                    });
                    (Arc::new(program), shown)
                });
                Some(Arc::new(PartPaint { program, colors, settings: op.settings.clone(), to_printer: *m, stretch: fab_plate::stretch(&from_printer), from_printer, seen }))
            })
            .collect();
        for (part, p) in parts.iter_mut().zip(&paint) {
            part.paint = p.as_ref().map(spatial_paint);
        }
        warnings.extend(picture_errors);
        // a picture part prints with the settings its grid needs, under its placement's own
        for (part, (ii, s)) in parts.iter_mut().zip(&part_src) {
            let inst = &c.instances[*ii];
            let Some(arg) = inst.placed_of(*s).and_then(|placed| inst.script.program.solids[placed.solid].picture.as_ref()) else { continue };
            let mut print = crate::picture::settings(arg, nozzle);
            print.retain(|(k, _)| !part.print.iter().any(|(o, _)| o == k));
            print.append(&mut part.print);
            part.print = print;
        }
        // Layout includes every placement so toggling print never shifts its neighbours.
        // Split before checks, fields, mechanics and slicing expose their indexed arrays.
        let printed_hosts: std::collections::HashSet<&str> = parts.iter().zip(&printed)
            .filter(|(_, printed)| **printed).map(|(p, _)| p.object.as_str()).collect();
        for insert in &mut inserts {
            if insert.as_ref().is_some_and(|i| !printed_hosts.contains(i.into.as_str())) {
                *insert = None;
            }
        }
        drop(printed_hosts);
        let mut excluded = Vec::new();
        let mut excluded_paint = Vec::new();
        let mut excluded_fields = Vec::new();
        let mut excluded_src = Vec::new();
        let (mut kept_shapes, mut excluded_shapes) = (Vec::new(), Vec::new());
        let (mut kept_parts, mut kept_render, mut kept_handles, mut kept_matrices, mut kept_frames,
            mut kept_fields, mut kept_paint, mut kept_src) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(),
            Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for (i, (((((((mut part, rm), h), matrix), frame), field), paint), src)) in parts.into_iter()
            .zip(render).zip(handles).zip(model_to_plate).zip(frames).zip(fields).zip(paint).zip(part_src).enumerate() {
            if !visible[i] { continue; }
            if printed[i] { kept_shapes.push(part_shapes[i].clone()) } else { excluded_shapes.push(part_shapes[i].clone()) }
            if printed[i] {
                // a mesh part whose mesh did not change keeps the one it had: what keys a part
                // by its mesh (the Force study's samples) sees the same part, and its change
                // below compares pointers only
                let frame = match self.last_parts.iter().find(|(o, ..)| *o == part.object) {
                    Some((_, old, None, _)) if field.is_none() && !Arc::ptr_eq(&old.local, &frame.local) && *old.local == *frame.local => PartFrame { local: old.local.clone(), offset: frame.offset },
                    _ => frame,
                };
                kept_parts.push(part);
                kept_render.push(rm);
                kept_handles.push(h);
                kept_matrices.push(matrix);
                kept_frames.push(frame);
                kept_fields.push(field);
                kept_paint.push(paint);
                kept_src.push(src);
            } else {
                part.print_mesh = None;
                let colors = paint.as_ref().map(|p| Arc::new(fab_plate::paint::PartPaint { colors: p.colors.clone(), tri: Vec::new() }));
                excluded.push(ExcludedPart { part, link: links[i].take(), frame: named[i].take(), insert: inserts[i].take(), render: rm, handles: h, model_to_plate: matrix, paint: colors });
                excluded_paint.push(paint);
                excluded_fields.push(field);
                excluded_src.push(src);
            }
        }
        let shown_colors = || part_colors.iter().zip(&printed).zip(&visible).filter(|(_, shown)| **shown).map(|(cp, _)| cp);
        let part_colors: Vec<String> = shown_colors().filter(|(_, p)| **p).chain(shown_colors().filter(|(_, p)| !**p)).map(|(c, _)| c.clone()).collect();
        let (parts, render, handles, model_to_plate, frames, fields, mut paint, part_src) =
            (kept_parts, kept_render, kept_handles, kept_matrices, kept_frames, kept_fields, kept_paint, kept_src);
        let tools = job_tools(&mapped, &colors, &parts, &part_colors);
        paint.iter_mut().chain(excluded_paint.iter_mut()).for_each(|p| paint_tools(p, &tools));
        let weaves = part_src
            .iter()
            .zip(&model_to_plate)
            .zip(&paint)
            .enumerate()
            .filter_map(|(k, (((ii, s), m), p))| {
                let b = &built[*ii];
                let (links, local) = crate::weave::links_of(&b.graph, b.nodes[*s])?;
                // each link's colour is its paint's (the paint gives every link one material)
                let slots = p.as_ref().map_or_else(|| vec![0; links.loops.len()], |p| crate::weave::link_slots(&p.program, &links, &local));
                Some(crate::weave::WeavePart::new(k, links, crate::weave::compose(m, &local), slots))
            })
            .collect();
        let features_of = |(ii, s): &(usize, usize)| {
            let inst = &c.instances[*ii];
            let f = inst.placed_of(*s).and_then(|pl| inst.script.object(&pl.object)).map(|o| crate::paint::features(&inst.script.program, o)).unwrap_or_default();
            Arc::new(f)
        };
        let features = part_src.iter().map(&features_of).collect();
        let excluded_features = excluded_src.iter().map(features_of).collect();
        let study_of = |(ii, s): &(usize, usize)| {
            let inst = &c.instances[*ii];
            inst.placed_of(*s).and_then(|pl| inst.script.study.get(&pl.object)).cloned()
        };
        let studies = part_src.iter().map(&study_of).collect();
        let excluded_studies = excluded_src.iter().map(study_of).collect();
        let hands = part_src.iter().map(|(ii, s)| crate::mech::hands(&c.instances[*ii].script.program).into_iter().filter(|(solid, _)| solid == s).map(|(_, at)| at).collect()).collect();
        let paint_keys: Vec<u64> = paint.iter().map(|p: &Option<Arc<PartPaint>>| p.as_ref().map_or(0, |p| p.key())).collect();
        let changes: Vec<PartChange> = parts
            .iter()
            .zip(&frames)
            .zip(&fields)
            .zip(&paint_keys)
            .map(|(((part, frame), field), pk)| match self.last_parts.iter().find(|(o, _, _, _)| *o == part.object) {
                None => PartChange::New,
                // Paint changed: the part slices again, nothing of its shape is rebuilt.
                Some((_, _, _, old_pk)) if old_pk != pk => PartChange::Reshaped(whole(&part.bounds(), frame.offset)),
                Some((_, old, old_key, _)) => match (field, old_key) {
                    // A field part: its field is what changed or not.
                    (Some(f), Some(k)) if f.key == *k => if old.offset == frame.offset { PartChange::Same } else { PartChange::Moved },
                    (Some(_), _) | (None, Some(_)) => PartChange::Reshaped(whole(&part.bounds(), frame.offset)),
                    (None, None) => part_change(old, frame),
                },
            })
            .collect();
        let last_parts: Vec<_> = parts.iter().zip(&frames).zip(&fields).zip(&paint_keys).map(|(((p, f), field), pk)| (p.object.clone(), f.clone(), field.as_ref().map(|f| f.key), *pk)).collect();
        let mut plate = Plate { parts, excluded, settings: c.settings.clone() };
        plate.map_colors(&part_colors, &|n| tools.get(n).copied());
        warnings.extend(melt_warnings(&plate));
        let reports = machine::check(&self.machine, &plate);
        let mut content = Sha256::new();
        for inst in &c.instances {
            content.update(inst.name.as_bytes());
            content.update(&[0]);
            content.update(inst.script.source().as_bytes());
            content.update(&inst.script.generated_key);
            // The host inputs it read and the libraries it ran with: an input that moved rebuilds
            // the same source into other geometry, and that geometry is what a slice is made from.
            for (kind, name) in &inst.script.program.inputs_read {
                content.update(&[1]);
                content.update(kind.as_bytes());
                content.update(&[0]);
                content.update(name.as_bytes());
                content.update(&[0]);
                match inst.script.env.inputs.stamp(&(kind.clone(), name.clone())) {
                    Some(stamp) => content.update(&stamp),
                    None => content.update(&[0]),
                }
            }
            for lib in inst.script.env.libraries.iter() {
                content.update(&[2]);
                content.update(lib.file.as_bytes());
                content.update(&[0]);
                content.update(lib.source.as_bytes());
            }
            // Its parameter values (a project's per-instance values build other geometry from
            // the same source), and a live drag's draft (a draft mesh is not the built one).
            for q in &inst.script.program.params {
                content.update(&q.call.value.to_bits().to_le_bytes());
            }
            let draft = self.draft.as_ref().filter(|(i, _)| *i == inst.name).map_or(1.0, |(_, f)| *f);
            content.update(&draft.to_bits().to_le_bytes());
            content.update(format!("{:?}", inst.placed).as_bytes());
            // The strokes it reads: a stroke recompiles no script and moves nothing placed, yet
            // what it paints or sculpts is what a slice is made from.
            for file in crate::program::strokes_files(inst.script.source()) {
                if let Some(log) = c.strokes.get(&file) {
                    content.update(file.as_bytes());
                    content.update(&log.key().to_le_bytes());
                    content.update(format!("{:?}", log.palette).as_bytes());
                }
            }
        }
        // The project's print colours: what the parts and their paint print and show as.
        content.update(format!("{:?}", c.colors).as_bytes());
        // The calibrated sets its pictures print with.
        content.update(format!("{:?}", self.calibrated).as_bytes());
        // The tolerance its exact parts slice at.
        content.update(&self.print_chord.to_bits().to_le_bytes());
        // What the job prints round the parts: where the layout puts them.
        content.update(format!("{:?}", self.rim).as_bytes());
        let built_count = self.graph.built;
        halt()?;
        if let Some(step) = &plate_step {
            step.stage("checking it prints", None, Some(0.8));
        }
        let mut printability = analyse_plate(&self.machine, &plate, &fields, &self.materials);
        // A texture finer than a line or shallower than a layer: warned, still sliced.
        for (part, field) in plate.parts.iter().zip(&fields) {
            let Some(f) = field else { continue };
            let mut found = Vec::new();
            makepad_csg_sdf::texture::textures(&f.shape.root, &mut found);
            if found.is_empty() {
                continue;
            }
            let b = part.bounds();
            let at = [(b.min[0] + b.max[0]) * 0.5, (b.min[1] + b.max[1]) * 0.5, b.max[2]];
            let found: Vec<(f64, f64)> = found.iter().map(|t| (t.feature, t.depth)).collect();
            printability.issues.extend(fab_printability::texture_issues(&part.object, at, fab_printability::Footprint::of_bounds(b), &found, &self.materials));
            printability.sort();
        }
        // past its last stop, this build is what the next one is told apart from
        self.last_parts = last_parts;
        self.last = Some(built);
        let mut ev = Evaluated {
            view: View { chord: self.graph.brep_chord, meshes: self.mesh_fields },
            viewed: vec![false; kept_shapes.len() + excluded_shapes.len()],
            shapes: kept_shapes.into_iter().chain(excluded_shapes).collect(),
            handles,
            printability: Arc::new(printability),
            content: content.finish(),
            plate,
            render,
            objects,
            reports,
            machine: self.machine.clone(),
            rim: self.rim,
            warnings,
            millis: started.elapsed().as_secs_f64() * 1000.0,
            model_to_plate,
            built: built_count,
            frames,
            changes,
            phases: vec![("solids", meshed_ms), ("plate", started.elapsed().as_secs_f64() * 1000.0 - meshed_ms)],
            fields,
            paint,
            features,
            excluded_paint,
            excluded_fields,
            excluded_features,
            studies,
            excluded_studies,
            hands,
            mech: mechs.into_iter().map(Arc::new).collect(),
            weaves,
            profile: self.profile.clone(),
            colors,
            part_colors,
            mapped: self.mapped.clone(),
            taken: self.taken(c),
            drifted: Vec::new(),
            lost: Vec::new(),
        };
        // Its parts shown as the view shows them: a display mesh that fails says so and shows
        // as the model made it.
        match self.view_meshes(&ev) {
            Ok(meshes) => ev.show_as(self.view, meshes),
            Err(e) => ev.warnings.push(e),
        }
        Ok(ev)
    }

    /// What the exact solids that name edges or faces took at this build: one lookup in the
    /// engine's kept steps per such solid, nothing built.
    fn taken(&self, c: &Compiled) -> Vec<Taken> {
        use makepad_csg_graph::{BrepOp, Open, Op};
        let mut out = Vec::new();
        for inst in &c.instances {
            let p = &inst.script.program;
            let mut seen = Vec::new();
            for s in &p.solids {
                // a placed body's step sits under its placement
                let node = match &p.graph.node(s.node).op {
                    Op::Brep { op: BrepOp::Place { .. }, inputs } if !inputs.is_empty() => inputs[0],
                    _ => s.node,
                };
                let names = match &p.graph.node(node).op {
                    Op::Brep { op: BrepOp::Fillet { edges, .. } | BrepOp::Chamfer { edges, .. }, .. } => !edges.named.is_empty(),
                    Op::Brep { op: BrepOp::Shell { open: Open::Faces(_), .. }, .. } => true,
                    _ => false,
                };
                if !names || seen.contains(&node) {
                    continue;
                }
                seen.push(node);
                if let Some(took) = self.graph.brep_taken(&p.graph, node) {
                    out.push(Taken { part: part_id(&inst.name, &s.id), file: inst.script.file.clone(), line: s.loc.map_or(1, |l| l.0), took });
                }
            }
        }
        out
    }

    /// A placed solid as a plate part (printer frame, welded) and its render mesh. A field
    /// part (`field_box`: its box in the printer frame) has a mesh only when one was built.
    fn part(&self, id: &str, name: &str, placed: &Placed, color: [f32; 3], m: Option<&TriMesh>, field_box: Option<Bounds>, warnings: &mut Vec<String>) -> (Part, RenderMesh) {
        let to_printer = |p: [f64; 3]| [p[0], -p[2], p[1]];
        let empty = TriMesh::new();
        let m = m.unwrap_or(&empty);
        let normals = match m.exact_normals() {
            Some(normals) => normals.iter().map(|n| [n.x, n.y, n.z]).collect(),
            None => corner_normals(m),
        };
        let mut print = PartMesh::default();
        let mut weld: HashMap<[u64; 3], u32> = HashMap::with_capacity(m.vertices.len());
        let ids: Vec<u32> = m
            .vertices
            .iter()
            .map(|v| {
                let q = to_printer([v.x, v.y, v.z]);
                let next = print.positions.len() as u32;
                *weld.entry([q[0].to_bits(), q[1].to_bits(), q[2].to_bits()]).or_insert_with(|| {
                    print.positions.push(q);
                    next
                })
            })
            .collect();
        let mut rm = RenderMesh::default();
        for (i, t) in m.triangles.iter().enumerate() {
            let w = [ids[t[0] as usize], ids[t[1] as usize], ids[t[2] as usize]];
            if w[0] == w[1] || w[1] == w[2] || w[0] == w[2] {
                continue;
            }
            print.triangles.push(w);
            for k in 0..3 {
                let v = m.vertices[t[k] as usize];
                let q = to_printer([v.x, v.y, v.z]);
                let n = to_printer(normals[i * 3 + k]);
                rm.indices.push(rm.positions.len() as u32);
                rm.positions.push([q[0] as f32, q[1] as f32, q[2] as f32]);
                rm.normals.push([n[0] as f32, n[1] as f32, n[2] as f32]);
            }
        }
        let open = if !placed.print || field_box.is_some() { 0 } else { print.open_edges() };
        if open > 0 {
            warnings.push(format!("'{id}' is not closed ({open} open edges): it will not slice as a solid"));
        }
        let part = Part { body_of: None, print_mesh: None, paint: None, print: placed.settings.clone(), object: id.to_string(), name: name.to_string(), filament: 1, color: [color[0], color[1], color[2], 1.0], nozzle: None, mesh: print, field_bounds: field_box, field_floor: None, ..Default::default() };
        (part, rm)
    }
}

/// A hold a job puts on its own build ([`Evaluator::gate`]): a test's or an agent's, for one
/// that acts on a build still on its way.
pub struct BuildGate {
    /// The job whose build it holds: a build for any other job never runs it.
    pub job: makepad_jobs::JobId,
    /// Runs once on the build's thread as its second step begins, its bar past 0 at that
    /// step's start; the build goes on when it returns.
    pub at_second_step: Box<dyn FnOnce() + Send + Sync>,
}

/// A build's steps on the job it works for ([`makepad_jobs::current`]), each its share of the
/// job's bar by its weight: every instance's joints sized (a share for each part they mesh),
/// each placed solid built, its parts measured (a share each) and its joints fitted, then the
/// plate laid out, painted and checked. A step says what it does: "building the wheels 6/8".
/// No step begins once the build's cancel is raised: each looks first, and the second again once
/// past the gate (a load its cancel lets go of stops there).
struct Steps<'a> {
    job: Option<makepad_jobs::JobReporter>,
    /// The build's own gate, when its job put one on it.
    gate: Option<Box<dyn FnOnce() + Send + Sync>>,
    /// The build's cancel.
    cancelled: Option<&'a dyn Fn() -> bool>,
    /// The weight done and the whole's.
    done: usize,
    of: usize,
}

impl<'a> Steps<'a> {
    fn new(c: &Compiled, gate: Option<BuildGate>, cancelled: Option<&'a dyn Fn() -> bool>) -> Steps<'a> {
        let weights = c.instances.iter().map(|i| {
            let p = &i.script.program;
            joint_solids(p) + i.placed.len() + i.placed.len() + 1 + usize::from(crate::mech::jointed(p))
        });
        let job = makepad_jobs::current();
        let gate = gate.filter(|g| job.as_ref().is_some_and(|j| j.id() == g.job)).map(|g| g.at_second_step);
        Steps { job, gate, cancelled, done: 0, of: 1 + weights.sum::<usize>() }
    }

    /// The build's cancel, as a failure.
    fn halt(&self) -> Result<(), Failure> {
        if self.cancelled.is_some_and(|stop| stop()) { Err(Failure::cancelled()) } else { Ok(()) }
    }

    /// The next step, `what`, of `weight` (none: no step): its share of the job, to report in.
    /// The second passes the build's gate once its bar shows where it starts. A cancel raised
    /// before it begins, or while it waits at the gate, ends the build there.
    fn next(&mut self, what: impl Into<String>, weight: usize) -> Result<Option<makepad_jobs::JobReporter>, Failure> {
        if weight == 0 {
            return Ok(None);
        }
        self.halt()?;
        let first = self.done == 0;
        let (lo, hi) = (self.done as f32 / self.of as f32, ((self.done + weight) as f32 / self.of as f32).min(1.0));
        self.done += weight;
        let step = self.job.as_ref().map(|j| j.part(lo, hi, what));
        if let Some(gate) = if first { None } else { self.gate.take() } {
            gate();
            self.halt()?;
        }
        Ok(step)
    }

    /// [`Self::next`] as the thread's job until the scope it returns ends: what runs there (a
    /// kernel deep down) reports inside the step.
    fn enter(&mut self, what: impl Into<String>, weight: usize) -> Result<Option<makepad_jobs::CurrentScope>, Failure> {
        Ok(self.next(what, weight)?.map(|step| makepad_jobs::enter(Some(step))))
    }
}

/// The `k`th of a step's `n` even shares, `what`, as the thread's job until the scope ends.
fn share(step: Option<&makepad_jobs::JobReporter>, k: usize, n: usize, what: impl Into<String>) -> Option<makepad_jobs::CurrentScope> {
    let n = n.max(1);
    let k = k.min(n - 1);
    step.map(|s| makepad_jobs::enter(Some(s.part(k as f32 / n as f32, (k + 1) as f32 / n as f32, what))))
}

/// A solid's id as a step names it: "wheels" for `wheels.union#2`, "tie rod" for `tie_rod`.
fn shown(id: &str) -> String {
    id.split(['.', '#']).next().unwrap_or(id).replace('_', " ")
}

/// How many parts a program's joints name: each is meshed to size them (none when hands holding its parts
/// are all its joints).
fn joint_solids(p: &Program) -> usize {
    if !crate::mech::jointed(p) {
        return 0;
    }
    let mut solids: Vec<usize> = Vec::new();
    for j in &p.joints {
        for s in std::iter::once(j.a).chain(j.b).chain(j.shoe) {
            if !solids.contains(&s) {
                solids.push(s);
            }
        }
    }
    solids.len()
}

fn triangle_key(m: &PartMesh, t: [u32; 3]) -> [u64; 9] {
    let p = t.map(|i| m.positions[i as usize].map(f64::to_bits));
    let k = (0..3).min_by_key(|&k| p[k]).unwrap();
    let mut key = [0u64; 9];
    for j in 0..3 {
        key[j * 3..j * 3 + 3].copy_from_slice(&p[(k + j) % 3]);
    }
    key
}

/// A part's whole box (plate frame, `offset` taken off) as a changed region.
fn whole(b: &Bounds, offset: [f64; 3]) -> ChangedRegion {
    ChangedRegion { z: (b.min[2] - offset[2], b.max[2] - offset[2]), xy: Bounds2 { min: [b.min[0] - offset[0], b.min[1] - offset[1]], max: [b.max[0] - offset[0], b.max[1] - offset[1]] } }
}

/// How a part's frame changed: the same mesh (moved or not), or the bounds
/// of the triangles one mesh has and the other has not.
fn part_change(old: &PartFrame, new: &PartFrame) -> PartChange {
    if Arc::ptr_eq(&old.local, &new.local) || *old.local == *new.local {
        return if old.offset == new.offset { PartChange::Same } else { PartChange::Moved };
    }
    // Both triangle lists sorted by a 64-bit mix of their keys (then by
    // the full key), and one merge walk: exact, without hashing the keys
    // into a set.
    let keyed = |m: &PartMesh| -> (Vec<[u64; 9]>, Vec<(u64, u32)>) {
        let keys: Vec<[u64; 9]> = m.triangles.iter().map(|&t| triangle_key(m, t)).collect();
        let mut order: Vec<(u64, u32)> = keys
            .iter()
            .enumerate()
            .map(|(i, k)| (k.iter().fold(0x9e37_79b9_7f4a_7c15u64, |h, &x| (h ^ x).wrapping_mul(0x0100_0000_01b3).rotate_left(29)), i as u32))
            .collect();
        order.sort_unstable_by(|x, y| x.0.cmp(&y.0).then_with(|| keys[x.1 as usize].cmp(&keys[y.1 as usize])));
        (keys, order)
    };
    let ((ka, a), (kb, b)) = (keyed(&old.local), keyed(&new.local));
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    let mut grow = |k: &[u64; 9]| {
        for c in 0..3 {
            for i in 0..3 {
                let x = f64::from_bits(k[c * 3 + i]);
                lo[i] = lo[i].min(x);
                hi[i] = hi[i].max(x);
            }
        }
    };
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        let order = match (a.get(i), b.get(j)) {
            (Some(x), Some(y)) => x.0.cmp(&y.0).then_with(|| ka[x.1 as usize].cmp(&kb[y.1 as usize])),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, _) => std::cmp::Ordering::Greater,
        };
        match order {
            std::cmp::Ordering::Equal => {
                i += 1;
                j += 1;
            }
            std::cmp::Ordering::Less => {
                grow(&ka[a[i].1 as usize]);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                grow(&kb[b[j].1 as usize]);
                j += 1;
            }
        }
    }
    if lo[0] > hi[0] {
        // The same triangles in another order or indexing: the same solid,
        // another mesh (what a mesh hash sees), so all of it.
        let b = new.local.bounds();
        return PartChange::Reshaped(ChangedRegion { z: (b.min[2], b.max[2]), xy: Bounds2 { min: [b.min[0], b.min[1]], max: [b.max[0], b.max[1]] } });
    }
    PartChange::Reshaped(ChangedRegion { z: (lo[2], hi[2]), xy: Bounds2 { min: [lo[0], lo[1]], max: [hi[0], hi[1]] } })
}

/// An instance's placed distance fields from its live script alone (nothing meshed):
/// (instance/solid id, shape, key). A live drag shows them before the plate is rebuilt.
pub fn part_fields(inst: &crate::program::Instance, live: &crate::program::Script, strokes: &crate::StrokeFiles) -> Vec<(String, Arc<makepad_csg_graph::SdfShape>, [u8; 32])> {
    let p = &live.program;
    let sculpt = crate::program::strokes_files(live.source()).into_iter().find_map(|f| strokes.get(&f).filter(|l| l.has_shape()).cloned());
    inst.placed
        .iter()
        .filter_map(|pl| {
            let id = &inst.script.program.solids[pl.solid].id;
            let s = p.solid(id)?;
            let (_, shape) = field_of(&p.graph, s.node, &s.id)?;
            let shape = Arc::new(match &sculpt {
                Some(strokes) => shape.sculpted(strokes.clone()),
                None => shape,
            });
            let key = shape.key();
            Some((part_id(&inst.name, &s.id), shape, key))
        })
        .collect()
}

/// The box of a box moved: its corners moved (a placement's `at`, `rotate`, uniform `scale`).
fn moved_box((lo, hi): ([f64; 3], [f64; 3]), at: [f64; 3], rotate: [f64; 3], scale: f64) -> ([f64; 3], [f64; 3]) {
    let m = makepad_csg_graph::eval::transform_matrix(at, rotate, [scale; 3]);
    let (mut min, mut max) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for c in 0..8 {
        let q = [if c & 1 == 0 { lo[0] } else { hi[0] }, if c & 2 == 0 { lo[1] } else { hi[1] }, if c & 4 == 0 { lo[2] } else { hi[2] }];
        for i in 0..3 {
            let x = m[i][0] * q[0] + m[i][1] * q[1] + m[i][2] * q[2] + m[i][3];
            min[i] = min[i].min(x);
            max[i] = max[i].max(x);
        }
    }
    (min, max)
}

/// The box, in `n`'s frame, of the subtree of field `n` tagged `id`: a solid lowered into its
/// part's field, found by its tag, moved by every placement above it.
fn tagged_bounds(n: &makepad_csg_graph::SdfNode, id: u32) -> Option<([f64; 3], [f64; 3])> {
    use makepad_csg_graph::SdfNode as N;
    match n {
        N::Id { id: tag, .. } if *tag == id => makepad_csg_sdf::node_bounds(n),
        N::Transform { at, rotate, scale, of } => tagged_bounds(of, id).map(|b| moved_box(b, *at, *rotate, *scale)),
        N::Instance { places, of } => {
            let b = tagged_bounds(of, id)?;
            places.iter().map(|pl| makepad_csg_sdf::bounds::map_bounds(&b, |q| pl.out_of(q))).reduce(|a, b| (std::array::from_fn(|k| a.0[k].min(b.0[k])), std::array::from_fn(|k| a.1[k].max(b.1[k]))))
        }
        N::Deform { deform, of, .. } => tagged_bounds(of, id).map(|b| makepad_csg_sdf::deform_bounds(deform, &b)),
        N::Combine { of, .. } => of.iter().find_map(|o| tagged_bounds(o, id)),
        N::Id { of, .. } | N::Material { of, .. } | N::Bone { of, .. } | N::Shell { of, .. } | N::Round { of, .. } | N::Sculpt { of, .. } | N::Texture { of, .. } => tagged_bounds(of, id),
        _ => None,
    }
}

/// The subtree of field `n` tagged `id` as a field of its own, in `n`'s frame: a solid lowered
/// into its part's field, with every move, copy and warp above it, and none of what its
/// neighbours or the nodes above it make of it (a blend's fillet, a shell, a carve's cut).
pub fn tagged_field(n: &makepad_csg_graph::SdfNode, id: u32) -> Option<makepad_csg_graph::SdfNode> {
    use makepad_csg_graph::SdfNode as N;
    match n {
        N::Id { id: tag, .. } if *tag == id => Some(n.clone()),
        N::Transform { at, rotate, scale, of } => tagged_field(of, id).map(|f| N::Transform { at: *at, rotate: *rotate, scale: *scale, of: Box::new(f) }),
        N::Instance { places, of } => tagged_field(of, id).map(|f| N::Instance { places: places.clone(), of: Box::new(f) }),
        N::Deform { deform, of, .. } => tagged_field(of, id).map(|f| N::deform(*deform, f)),
        N::Combine { of, .. } => of.iter().find_map(|o| tagged_field(o, id)),
        N::Id { of, .. } | N::Material { of, .. } | N::Bone { of, .. } | N::Shell { of, .. } | N::Round { of, .. } | N::Sculpt { of, .. } | N::Texture { of, .. } => tagged_field(of, id),
        _ => None,
    }
}

/// A node's distance field under rigid moves and uniform scales (in the model frame).
fn field_of(g: &Graph, id: NodeId, solid_id: &str) -> Option<(NodeId, makepad_csg_graph::SdfShape)> {
    use makepad_csg_graph::{Op, SdfNode, SdfShape};
    match &g.node(id).op {
        Op::Field { shape, .. } => Some((id, shape.clone())),
        Op::Links { links, .. } => Some((
            id,
            SdfShape {
                root: SdfNode::Id { id: makepad_model_script::lang::csg_sdf::part_id(solid_id), of: Box::new(SdfNode::Links(links.clone())) },
                min: links.min,
                max: links.max,
                materials: Vec::new(),
            },
        )),
        Op::Transform { source, at, rotate, scale } if scale[0] == scale[1] && scale[1] == scale[2] && scale[0] > 0.0 => {
            let (node, inner) = field_of(g, *source, solid_id)?;
            let (min, max) = moved_box((inner.min, inner.max), *at, *rotate, scale[0]);
            Some((node, SdfShape { root: SdfNode::Transform { at: *at, rotate: *rotate, scale: scale[0], of: Box::new(inner.root) }, min, max, materials: inner.materials }))
        }
        _ => None,
    }
}

/// The cell [`field_of`]'s field meshes at, in the frame of its shape: None when it takes the
/// default for its box.
fn field_cell(g: &Graph, id: NodeId) -> Option<f64> {
    use makepad_csg_graph::Op;
    match &g.node(id).op {
        Op::Field { cell, .. } => *cell,
        Op::Transform { source, scale, .. } => field_cell(g, *source).map(|cell| cell * scale[0]),
        _ => None,
    }
}

/// The determinant of a map's linear part.
fn det3(m: &[[f64; 4]; 3]) -> f64 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0]) + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

/// Solid `i` is `root` or part of it.
fn is_within(p: &Program, i: usize, root: usize) -> bool {
    let mut at = Some(i);
    while let Some(j) = at {
        if j == root {
            return true;
        }
        at = p.solids[j].parent;
    }
    false
}

/// Lay the parts out by object (`group[i]` is part i's). An object's placement is its own frame,
/// whatever its shape: it turns and scales about its model origin, and its origin sits at its
/// `at` on the bed, so an edit of its shape (a cutter's eye, a param) moves no point the edit
/// leaves alone. A free object (no `at`) sits with its footprint centre at its slot in a shelf
/// pack of all objects (biggest first, centred on the bed). Either drops onto the plate by its
/// lowest body (an assembly's bodies keep their arrangement).
/// A field part (`fields[i]`) lays out by its field's extent under its frame (`extent`), never a
/// mesh: its footprint by its box, and it rests on the lowest solid point its proof found (its
/// box may reach a little lower, by what the proof left open: the part's `bottom_gap`).
/// A free object's slot lies where all of its `nozzles` reach; the free objects no such slot
/// fits come last, with their footprints (they keep the slot the bed alone gives them).
/// Attached inserts share their host's group and placement, but never size its slot or set
/// its floor. Hidden hosts stay through this step solely to supply that same frame.
fn layout(
    machine: &Machine,
    rim: &Rim,
    by_object: bool,
    parts: &mut [Part],
    render: &mut [RenderMesh],
    placements: &BTreeMap<String, Placement>,
    group: &[String],
    nozzles: &BTreeMap<String, Vec<usize>>,
    fields: &[Option<(NodeId, Arc<makepad_csg_graph::SdfShape>, [u8; 32])>],
    rests: &[Option<(Arc<makepad_csg_graph::SdfShape>, [u8; 32])>],
    solids: &[Option<Arc<makepad_csg_graph::SdfShape>>],
    attached: &[bool],
    layer: f64,
    extent: &mut dyn FnMut(&makepad_csg_graph::SdfShape, [u8; 32], &[[f64; 4]; 3]) -> Option<Extent>,
) -> (Vec<[[f64; 4]; 3]>, Vec<PartFrame>, Vec<([[f64; 4]; 3], Option<Bounds>, Option<f64>)>, Vec<(usize, f64)>, Vec<(String, [f64; 2])>) {
    let mut part_frames = Vec::with_capacity(parts.len());
    // Each part's frame map, field box and bottom gap before its plate offset (see
    // `PartField::to_part`).
    let mut in_part = Vec::with_capacity(parts.len());
    // Model (x, y, z) to printer (x, -z, y), then each step below on the left.
    let mut frames = vec![TO_PRINTER; parts.len()];
    let bounds_of = |parts: &[Part]| -> BTreeMap<&str, Bounds> {
        let mut out: BTreeMap<&str, Bounds> = BTreeMap::new();
        for (i, (part, g)) in parts.iter().zip(group).enumerate() {
            if attached.get(i) == Some(&true) { continue; }
            let b = out.entry(g.as_str()).or_insert(Bounds::EMPTY);
            *b = b.union(&part.bounds());
        }
        out
    };
    let before: BTreeMap<String, Bounds> = bounds_of(parts).into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    for ((((part, rm), frame), g), field) in parts.iter_mut().zip(render.iter_mut()).zip(frames.iter_mut()).zip(group).zip(fields) {
        let Some(p) = placements.get(g) else { continue };
        if p.rotate == [0.0; 3] && p.scale == [1.0; 3] {
            continue;
        }
        // About the model's origin (the printer frame's, under TO_PRINTER): scaled along the
        // printer's axes, then turned (R S). A field part's extent is proven under the whole map
        // (its sides in its own frame), so a stretch needs no factor there.
        let r = rotation_xyz(p.rotate);
        let s = p.scale;
        let k = s[0];
        let uniform = s[1] == k && s[2] == k;
        let m: [[f64; 4]; 3] = if uniform {
            std::array::from_fn(|i| [k * r[i][0], k * r[i][1], k * r[i][2], 0.0])
        } else {
            std::array::from_fn(|i| [r[i][0] * s[0], r[i][1] * s[1], r[i][2] * s[2], 0.0])
        };
        *frame = affine_mul(&m, frame);
        if let Some((_, shape, key)) = field {
            part.field_bounds = extent(shape, *key, frame).map(|e| Bounds { min: e.min, max: e.max });
        }
        let turn = |q: [f64; 3]| -> [f64; 3] {
            if uniform {
                std::array::from_fn(|i| k * (r[i][0] * q[0] + r[i][1] * q[1] + r[i][2] * q[2]))
            } else {
                std::array::from_fn(|i| m[i][0] * q[0] + m[i][1] * q[1] + m[i][2] * q[2])
            }
        };
        for q in &mut part.mesh.positions {
            *q = turn(*q);
        }
        for q in &mut rm.positions {
            let t = turn([q[0] as f64, q[1] as f64, q[2] as f64]);
            *q = [t[0] as f32, t[1] as f32, t[2] as f32];
        }
        // A normal turns with the part; under a stretch it goes by the inverse transpose (R S⁻¹).
        for n in &mut rm.normals {
            let m = [n[0] as f64, n[1] as f64, n[2] as f64];
            if uniform {
                *n = std::array::from_fn(|i| (r[i][0] * m[0] + r[i][1] * m[1] + r[i][2] * m[2]) as f32);
            } else {
                let t: [f64; 3] = std::array::from_fn(|i| r[i][0] * m[0] / s[0] + r[i][1] * m[1] / s[1] + r[i][2] * m[2] / s[2]);
                let l = (t[0] * t[0] + t[1] * t[1] + t[2] * t[2]).sqrt().max(1e-30);
                *n = t.map(|c| (c / l) as f32);
            }
        }
    }
    let after: BTreeMap<String, Bounds> = bounds_of(parts).into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    // Each part's lowest found solid point (printer z, as turned): its floor. What each object
    // rests on: its parts' lowest floor (the plate check tests the same). A field part's floor
    // comes with its box's proof. A part resting by a field alone (an assembly part's rest shape)
    // is proven lowest tree box first: one whose tree box lies above a floor its object already
    // has cannot set the rest, so its floor waits for the analysis that tells its gap
    // ([`FieldFloor::pending`]), never in the evaluate.
    let mut found: BTreeMap<&str, f64> = BTreeMap::new();
    let mut by_low = Vec::new();
    let mut proven = Vec::new();
    for (i, ((part, frame), (shape, field))) in parts.iter_mut().zip(&frames).zip(rests.iter().zip(fields)).enumerate() {
        if attached.get(i) == Some(&true) { continue; }
        match shape {
            Some((shape, key)) if field.is_none() => {
                by_low.push((tree_low(shape, *key, frame), i));
                continue;
            }
            Some((shape, key)) => part.field_floor = extent(shape, *key, frame).and_then(|e| e.found).map(|f| FieldFloor::proven(f.0[2])),
            None => {}
        }
        let f = found.entry(group[i].as_str()).or_insert(f64::INFINITY);
        *f = f.min(part.floor());
    }
    by_low.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (low, i) in by_low {
        let Some((shape, key)) = rests[i].clone() else { continue };
        let frame = frames[i];
        let f = found.entry(group[i].as_str()).or_insert(f64::INFINITY);
        if low > *f {
            parts[i].field_floor = Some(FieldFloor::pending(move || bottom_of(&shape, key, &frame).and_then(|b| b.witness)));
        } else {
            let bottom = bottom_of(&shape, key, &frame);
            parts[i].field_floor = bottom.and_then(|b| b.witness).map(FieldFloor::proven);
            *f = f.min(parts[i].floor());
            proven.push((i, bottom));
        }
    }
    // The rest from the proven floors in part order (a pending one lies above it).
    let mut rest: BTreeMap<&str, f64> = BTreeMap::new();
    for (i, (part, g)) in parts.iter().zip(group).enumerate() {
        if attached.get(i) == Some(&true) { continue; }
        let r = rest.entry(g.as_str()).or_insert(f64::INFINITY);
        if let Some(floor) = part.known_floor() {
            *r = r.min(floor);
        }
    }
    // A part whose rest shape's bottom proof left its solid free to reach more than the tolerance
    // below its object's rest (the proof ran out of budget): how far into the bed the object may
    // sit. A pending part's tree box lies above the rest.
    let rest_gaps: Vec<(usize, f64)> = proven.into_iter().filter_map(|(i, bottom)| {
        let gap = rest[group[i].as_str()] - bottom?.bound;
        (gap > EXTENT_TOL).then_some((i, gap))
    }).collect();
    // Where each object goes: its origin to its `at`, else its footprint centre to its slot in
    // the shelf pack of every object (a placed one keeps its slot, so placing one never moves
    // another).
    let mut slot: BTreeMap<&str, [f64; 2]> = BTreeMap::new();
    // Slots are sized by the objects as built (before their turns and scales), so turning or
    // scaling one never moves another either.
    // A slot keeps what the job prints round the object (its brim and the skirt) where the
    // object's nozzles reach, and as far off the bed's edges.
    let reach = |g: &str, size: [f64; 2], height: f64| inset_on(machine.reach(nozzles.get(g).map_or(&[][..], Vec::as_slice)), machine.bed, rim.past(size, height));
    let mut all: Vec<(&str, [f64; 2], f64, Rect)> = before.iter().map(|(g, b)| {
        let (size, height) = ([b.max[0] - b.min[0], b.max[1] - b.min[1]], b.max[2] - b.min[2]);
        (g.as_str(), size, height, reach(g, size, height))
    }).collect();
    let (slots, no_slot) = arrange(machine, &mut all, by_object);
    for (g, at) in slots {
        slot.insert(g, at);
    }
    // A placed object stands where it was put.
    let unplaced: Vec<(String, [f64; 2])> = no_slot.into_iter().filter(|g| placements.get(*g).is_none_or(|p| p.at.is_none())).map(|g| (g.to_string(), [before[g].max[0] - before[g].min[0], before[g].max[1] - before[g].min[1]])).collect();
    for (i, (part, rm)) in parts.iter_mut().zip(render.iter_mut()).enumerate() {
        let g = group[i].as_str();
        let xy = match (placements.get(g).and_then(|p| p.at), slot.get(g)) {
            (Some(at), _) => at,
            (None, Some(at)) => { let b = after[g]; [at[0] - b.center()[0], at[1] - b.center()[1]] },
            (None, None) => [0.0; 2],
        };
        // 0.0 - rest, never -rest: a rest of +0.0 would move by -0.0, which keeps a vertex's -0.0
        // where +0.0 makes every zero +0.0.
        let dz = if placements.get(g).map_or(true, |p| p.drop) { 0.0 - rest.get(g).copied().unwrap_or(0.0) } else { 0.0 };
        let by = [xy[0], xy[1], dz];
        let gap = part.field_floor.as_ref().and_then(|f| f.known().flatten()).zip(part.field_bounds).map(|(low, b)| low - b.min[2]);
        in_part.push((frames[i], part.field_bounds, gap));
        for (row, d) in frames[i].iter_mut().zip(by) {
            row[3] += d;
        }
        part_frames.push(PartFrame { local: Arc::new(part.mesh.clone()), offset: by });
        part.mesh.translate(by);
        if let Some(b) = part.field_bounds.as_mut() {
            *b = Bounds { min: std::array::from_fn(|k| b.min[k] + by[k]), max: std::array::from_fn(|k| b.max[k] + by[k]) };
        }
        if let Some(f) = part.field_floor.as_mut() {
            f.raise(by[2]);
        }
        for q in &mut rm.positions {
            q[0] = (q[0] as f64 + by[0]) as f32;
            q[1] = (q[1] as f64 + by[1]) as f32;
            q[2] = (q[2] as f64 + by[2]) as f32;
        }
    }
    // Whether each part above the plate stands on printed material: proven from the parts'
    // solids where they now stand, when an analysis asks for the part's gap. A mesh part with a
    // solid is judged too, at its box's bottom.
    for (part, solid) in parts.iter_mut().zip(solids) {
        if part.field_floor.is_none() && solid.is_some() {
            part.field_floor = Some(FieldFloor::proven(part.bounds().min[2]));
        }
    }
    let stack = Arc::new(Stack {
        layer,
        parts: parts.iter().zip(solids).zip(&frames).enumerate().map(|(i, ((part, solid), frame))| Standing {
            shape: solid.as_ref().filter(|_| attached.get(i) != Some(&true)).zip(invert_affine(frame)).map(|(shape, from_printer)| (shape.clone(), from_printer)),
            field: std::sync::OnceLock::new(),
            bounds: part.bounds(),
            floor: part.field_floor.clone(),
            held: std::sync::OnceLock::new(),
        }).collect(),
    });
    for (i, part) in parts.iter_mut().enumerate() {
        if let (Some(floor), Some(_)) = (part.field_floor.as_mut(), &stack.parts[i].shape) {
            floor.held_by(Arc::new(PartHeld(stack.clone(), i)));
        }
    }
    (frames, part_frames, in_part, rest_gaps, unplaced)
}

/// The plate's parts where the layout stands them, for whether each one above the plate stands
/// on printed material ([`fab_plate::Held`]): its first layer over a held part's material in the
/// layer below, by at least [`CONTACT_AREA`]. Held material reaches down to the plate, so a part
/// on a part that floats floats too. What hangs past the contact is the overhang checks'.
struct Stack {
    /// The plate's layer height (mm).
    layer: f64,
    parts: Vec<Standing>,
}

/// One part of a [`Stack`].
struct Standing {
    /// Its rest shape and the map from the printer frame to the shape's; None: it is not judged
    /// (it holds nothing up, and floats when it rests above the plate).
    shape: Option<(Arc<makepad_csg_graph::SdfShape>, [[f64; 4]; 3])>,
    field: std::sync::OnceLock<Arc<dyn makepad_csg_sdf::Sdf3 + Send + Sync>>,
    bounds: Bounds,
    floor: Option<FieldFloor>,
    held: std::sync::OnceLock<bool>,
}

/// The least area (mm²) of a part's first layer that must stand on held material: a few lines
/// of it. An edge or a corner touching is less.
const CONTACT_AREA: f64 = 1.0;
/// The grid a contact is measured on (mm), coarser on a part so wide it would take more than
/// [`CONTACT_POINTS`] points.
const CONTACT_CELL: f64 = 0.1;
const CONTACT_POINTS: f64 = 10000.0;

impl Stack {
    /// Whether part `i`'s solid holds the printer point `p` (none without a shape).
    fn inside(&self, i: usize, p: [f64; 3]) -> bool {
        let part = &self.parts[i];
        let Some((shape, from_printer)) = &part.shape else { return false };
        let field = part.field.get_or_init(|| shape.field());
        let q = apply_affine(from_printer, p);
        makepad_csg_sdf::Sdf3::distance(&**field, makepad_csg_math::Vec3d::new(q[0], q[1], q[2])) <= 0.0
    }

    /// Whether part `i` is held: it rests on the plate, or stands on held parts.
    fn held(&self, i: usize) -> bool {
        *self.parts[i].held.get_or_init(|| self.prove(i))
    }

    fn prove(&self, i: usize) -> bool {
        let part = &self.parts[i];
        let b = part.bounds;
        let floor = part.floor.as_ref().and_then(FieldFloor::z).unwrap_or(b.min[2]);
        if floor <= machine::EDGE_SLACK {
            return true;
        }
        if part.shape.is_none() {
            return false;
        }
        let h = self.layer;
        // The parts in the layer below its first: each has its solid under the floor, so its own
        // floor is lower and proving it never comes back here.
        let under: Vec<usize> = (0..self.parts.len())
            .filter(|&j| {
                let o = self.parts[j].bounds;
                j != i && self.parts[j].shape.is_some() && o.min[2] < floor && o.max[2] >= floor - h && o.min[0] < b.max[0] && o.max[0] > b.min[0] && o.min[1] < b.max[1] && o.max[1] > b.min[1]
            })
            .collect();
        if under.is_empty() {
            return false;
        }
        let (w, d) = (b.max[0] - b.min[0], b.max[1] - b.min[1]);
        let cell = CONTACT_CELL.max((w * d / CONTACT_POINTS).sqrt());
        let need = (CONTACT_AREA / (cell * cell)).ceil() as usize;
        let mut held_under: Vec<Option<bool>> = vec![None; under.len()];
        let mut found = 0;
        for ix in 0..(w / cell).ceil() as usize {
            for iy in 0..(d / cell).ceil() as usize {
                let (x, y) = (b.min[0] + (ix as f64 + 0.5) * cell, b.min[1] + (iy as f64 + 0.5) * cell);
                // its first layer here, low or high in it, over held material in the layer below
                if !self.inside(i, [x, y, floor + 0.1 * h]) && !self.inside(i, [x, y, floor + 0.9 * h]) {
                    continue;
                }
                let on = under.iter().zip(&mut held_under).any(|(&j, held)| self.inside(j, [x, y, floor - 0.5 * h]) && *held.get_or_insert_with(|| self.held(j)));
                if on {
                    found += 1;
                    if found >= need {
                        return true;
                    }
                }
            }
        }
        false
    }
}

/// Part `.1` of a [`Stack`], as its floor's [`fab_plate::Held`].
struct PartHeld(Arc<Stack>, usize);

impl fab_plate::Held for PartHeld {
    fn held(&self) -> bool {
        self.0.held(self.1)
    }
    fn known(&self) -> Option<bool> {
        self.0.parts[self.1].held.get().copied()
    }
}

/// Shelf-pack footprints, leaving room for sequential printing when requested. `items`: each
/// object's name, footprint, height and the part of the bed its footprint must lie in (the bed,
/// where every nozzle it prints with reaches: [`Machine::reach`]). Rows run along X, 5 mm inside the
/// bed's edges, and a row takes the next footprint while there is room for all of its own in
/// their regions. A row stands centred on the bed, and the rows stand centred one behind
/// another, unless that puts a footprint outside its region: then as near it as their regions
/// allow. Returns each footprint's centre, and the objects whose region has no room for theirs
/// (they stand where the bed alone puts them).
pub fn arrange<'a>(machine: &Machine, items: &mut [(&'a str, [f64; 2], f64, Rect)], by_object: bool) -> (Vec<(&'a str, [f64; 2])>, Vec<&'a str>) {
    // By object: the toolhead's clearance diameter, plus room for a brim or skirt on both
    // sides and the spiral lift's sweep (the slice check measures between extrusion hulls).
    let gap = if by_object { machine.clearance.max_radius + 24.0 } else { 8.0 };
    let bed = &machine.bed;
    let (bw, bd) = (bed.max[0] - bed.min[0], bed.max[1] - bed.min[1]);
    // Where a footprint may go as the rows are packed: its region, 5 mm inside the bed's edges.
    let packed = |r: &Rect| Rect::new(r.min[0].max(bed.min[0] + 5.0), r.min[1].max(bed.min[1] + 5.0), r.max[0].min(bed.max[0] - 5.0), r.max[1].min(bed.max[1] - 5.0));
    // The span of starts (`lo`, `hi`) where a footprint `at` past the start lies in `r`, and
    // that span met with another.
    let span = |r: &Rect, k: usize, at: f64, size: f64| [r.min[k] - at, r.max[k] - at - size];
    let meet = |a: [f64; 2], b: [f64; 2]| [a[0].max(b[0]), a[1].min(b[1])];
    let open = |a: [f64; 2]| a[0] <= a[1] + machine::EDGE_SLACK;
    // The centred start where it is in the span (within the check's slack), else the span's
    // nearest where it has one.
    let start = |centred: f64, a: [f64; 2]| if !open(a) || (a[0] - machine::EDGE_SLACK..=a[1] + machine::EDGE_SLACK).contains(&centred) { centred } else { centred.clamp(a[0], a[1].max(a[0])) };
    items.sort_by(|a, b| (b.1[0] * b.1[1]).total_cmp(&(a.1[0] * a.1[1])).then_with(|| a.0.cmp(b.0)));
    let tallest = items.iter().max_by(|a, b| a.2.total_cmp(&b.2).then_with(|| a.0.cmp(b.0))).map(|i| i.0);
    struct Row<'a> {
        width: f64,
        depth: f64,
        cells: Vec<(&'a str, f64, [f64; 2], Rect)>,
        // the starts along X its cells' regions leave it
        span: [f64; 2],
    }
    let mut rows: Vec<Row> = Vec::new();
    let mut isolated = false;
    for (g, s, h, region) in items.iter() {
        let alone = by_object && *h > machine.clearance.height_to_rod && Some(*g) != tallest;
        let fits = !alone && !isolated && rows.last().is_some_and(|r| r.width + gap + s[0] <= bw - 10.0 && open(meet(r.span, span(&packed(region), 0, r.width + gap, s[0]))));
        if !fits {
            rows.push(Row { width: 0.0, depth: 0.0, cells: Vec::new(), span: [f64::NEG_INFINITY, f64::INFINITY] });
        }
        let r = rows.last_mut().unwrap();
        let x = if r.cells.is_empty() { 0.0 } else { r.width + gap };
        r.span = meet(r.span, span(&packed(region), 0, x, s[0]));
        r.cells.push((g, x, *s, *region));
        r.width = x + s[0];
        r.depth = r.depth.max(s[1]);
        isolated = alone;
    }
    // The rows one behind another, each cell centred on its row: the starts along Y their
    // regions leave the rows.
    let mut y_span = [f64::NEG_INFINITY, f64::INFINITY];
    let mut y = 0.0;
    for r in &rows {
        for (_, _, s, region) in &r.cells {
            y_span = meet(y_span, span(&packed(region), 1, y + (r.depth - s[1]) * 0.5, s[1]));
        }
        y += r.depth + gap;
    }
    let total_d: f64 = rows.iter().map(|r| r.depth).sum::<f64>() + gap * rows.len().saturating_sub(1) as f64;
    let mut y = start(bed.min[1] + (bd - total_d).max(0.0) * 0.5, y_span);
    let mut out = Vec::new();
    let mut unplaced = Vec::new();
    for r in &rows {
        let x0 = start(bed.min[0] + (bw - r.width).max(0.0) * 0.5, r.span);
        for (g, x, s, region) in &r.cells {
            let c = [x0 + x + s[0] * 0.5, y + r.depth * 0.5];
            let footprint = Bounds { min: [c[0] - s[0] * 0.5, c[1] - s[1] * 0.5, 0.0], max: [c[0] + s[0] * 0.5, c[1] + s[1] * 0.5, 0.0] };
            if !region.contains_footprint(&footprint, machine::EDGE_SLACK) {
                unplaced.push(*g);
            }
            out.push((*g, c));
        }
        y += r.depth + gap;
    }
    (out, unplaced)
}

/// `a * b` of row-major affines.
pub fn affine_mul(a: &[[f64; 4]; 3], b: &[[f64; 4]; 3]) -> [[f64; 4]; 3] {
    std::array::from_fn(|i| {
        let mut row = [0.0; 4];
        for j in 0..4 {
            row[j] = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j] + if j == 3 { a[i][3] } else { 0.0 };
        }
        row
    })
}

/// `p` turned by `radians` about the unit `axis` through `center` (right-handed).
pub fn rotate_about(p: [f64; 3], center: [f64; 3], axis: [f64; 3], radians: f64) -> [f64; 3] {
    let d = [p[0] - center[0], p[1] - center[1], p[2] - center[2]];
    let (s, c) = radians.sin_cos();
    let k = axis;
    let kd = k[0] * d[0] + k[1] * d[1] + k[2] * d[2];
    let cross = [k[1] * d[2] - k[2] * d[1], k[2] * d[0] - k[0] * d[2], k[0] * d[1] - k[1] * d[0]];
    std::array::from_fn(|i| center[i] + d[i] * c + cross[i] * s + k[i] * kd * (1.0 - c))
}

/// The turn that takes unit `n` onto unit `to` (row-major), the shortest one.
pub fn turn_onto(n: [f64; 3], to: [f64; 3]) -> [[f64; 3]; 3] {
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-12);
    let a = n.map(|x| x / len);
    let c = a[0] * to[0] + a[1] * to[1] + a[2] * to[2];
    let v = [a[1] * to[2] - a[2] * to[1], a[2] * to[0] - a[0] * to[2], a[0] * to[1] - a[1] * to[0]];
    if c < -1.0 + 1e-9 {
        // Opposite: half a turn about any axis across them.
        let axis = if a[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
        let d = axis[0] * a[0] + axis[1] * a[1] + axis[2] * a[2];
        let u = [axis[0] - d * a[0], axis[1] - d * a[1], axis[2] - d * a[2]];
        let l = (u[0] * u[0] + u[1] * u[1] + u[2] * u[2]).sqrt();
        let u = u.map(|x| x / l);
        return std::array::from_fn(|i| std::array::from_fn(|j| 2.0 * u[i] * u[j] - if i == j { 1.0 } else { 0.0 }));
    }
    let vx: [[f64; 3]; 3] = [[0.0, -v[2], v[1]], [v[2], 0.0, -v[0]], [-v[1], v[0], 0.0]];
    let k = 1.0 / (1.0 + c);
    std::array::from_fn(|i| {
        std::array::from_fn(|j| {
            let vx2: f64 = (0..3).map(|n| vx[i][n] * vx[n][j]).sum();
            (if i == j { 1.0 } else { 0.0 }) + vx[i][j] + k * vx2
        })
    })
}

/// `R = Rz Ry Rx` of degrees `[x, y, z]` (row-major): a `place`'s orientation.
pub fn rotation_xyz(d: [f64; 3]) -> [[f64; 3]; 3] {
    let (sx, cx) = d[0].to_radians().sin_cos();
    let (sy, cy) = d[1].to_radians().sin_cos();
    let (sz, cz) = d[2].to_radians().sin_cos();
    [
        [cz * cy, cz * sy * sx - sz * cx, cz * sy * cx + sz * sx],
        [sz * cy, sz * sy * sx + cz * cx, sz * sy * cx - cz * sx],
        [-sy, cy * sx, cy * cx],
    ]
}

/// The degrees `[x, y, z]` of a rotation `R = Rz Ry Rx` (row-major), [`rotation_xyz`]'s inverse.
pub fn euler_xyz(r: &[[f64; 3]; 3]) -> [f64; 3] {
    let sy = (-r[2][0]).clamp(-1.0, 1.0);
    let y = sy.asin();
    let (x, z) = if sy.abs() < 0.999_999 {
        (r[2][1].atan2(r[2][2]), r[1][0].atan2(r[0][0]))
    } else {
        // gimbal lock: X and Z turn about the same axis; put it all in Z
        (0.0, (-r[0][1]).atan2(r[1][1]))
    };
    [x.to_degrees(), y.to_degrees(), z.to_degrees()]
}

/// How much a heat-set insert's measure may fall short of what it wants (mm): the facets of a
/// round hole and boss.
const MELT_TOL: f64 = 0.05;

/// The heat-set inserts whose part has no hole for them, or holds them with less wall round the
/// hole or less floor under it than their size wants. Cost: per such insert 41 rays through its
/// part's triangles, once per evaluation on the evaluator's worker.
fn melt_warnings(plate: &Plate) -> Vec<String> {
    let mut out = Vec::new();
    for e in &plate.excluded {
        let (Some(insert), Some(melt)) = (e.insert.as_ref(), e.melt()) else { continue };
        let Some(host) = plate.parts.iter().find(|p| p.object == insert.into) else { continue };
        if host.mesh.triangles.is_empty() {
            continue;
        }
        let (name, part, size) = (&e.part.name, &host.name, &melt.size);
        match melt.seat(&host.mesh) {
            fab_plate::melt::Seat::NoHole => out.push(format!("'{name}' has no hole in '{part}': seat it (heat_seat(body, [insert])) so the {size} insert melts into one")),
            fab_plate::melt::Seat::Held { wall, floor } => {
                if wall < melt.wall - MELT_TOL {
                    out.push(format!("'{name}' has {wall:.1} mm of '{part}' round its hole; an {size} insert wants {:.1} mm: give it a boss (heat_seat adds one unless boss: false) or move it in from the edge", melt.wall));
                }
                if floor <= 0.0 {
                    out.push(format!("'{name}''s hole runs through '{part}'; an {size} insert wants {:.1} mm under it: thicken the part there or make the hole shallower (depth)", melt.floor));
                } else if floor < melt.floor - MELT_TOL {
                    out.push(format!("'{name}' has {floor:.1} mm of '{part}' under its hole; an {size} insert wants {:.1} mm: thicken the part there or make the hole shallower (depth)", melt.floor));
                }
            }
        }
    }
    out
}

/// The node a solid's own call made, under its placement.
fn made_node(g: &Graph, mut id: NodeId) -> NodeId {
    use makepad_csg_graph::{BrepOp, Op};
    loop {
        id = match &g.node(id).op {
            Op::Brep { op: BrepOp::Place { .. }, inputs } if !inputs.is_empty() => inputs[0],
            Op::Transform { source, .. } | Op::Tag { source, .. } => *source,
            _ => return id,
        };
    }
}

/// What a split whose cut had no room for its pins says: the pin it tried and the one that would
/// stand, as their widths, with the radius to give it (`pin`, rounded down so that it stands, and
/// no less than the field takes).
fn split_without_pins(no_room: SplitNoRoom) -> String {
    let width = |r: f64| (200.0 * r).round() / 100.0;
    let tried = width(no_room.radius);
    match no_room.fits.map(|r| (10.0 * r).floor() / 10.0).filter(|r| *r >= 0.5) {
        Some(r) => format!("split without pins: the cut has no room for a {tried} mm pin; a {} mm one fits (pin: {r}mm, its radius)", width(r)),
        None => format!("split without pins: the cut has no room for a {tried} mm pin, nor for a 1 mm one"),
    }
}

#[cfg(test)]
mod extent_tests {
    use super::*;
    use makepad_csg_graph::{SdfNode, SdfShape};

    /// A field's box in the printer frame and under a scaled quarter turn is its model-frame box
    /// with the axes swapped and flipped: one search per shape, the box that search finds.
    #[test]
    fn swapped_axes_reuse_the_model_box() {
        let box_ = SdfNode::Box { size: [20.0, 10.0, 6.0], radius: 0.0 };
        let shape = SdfShape { root: SdfNode::Transform { at: [1.0, 2.0, 3.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(box_) }, min: [-15.0, -10.0, -10.0], max: [16.0, 13.0, 13.0], materials: Vec::new() };
        let key = shape.key();
        let model = extent_of(&shape, key, &IDENTITY3).unwrap();
        let printer = extent_of(&shape, key, &TO_PRINTER).unwrap();
        let r = rotation_xyz([0.0, 0.0, 90.0]);
        let turn: [[f64; 4]; 3] = std::array::from_fn(|i| [2.0 * r[i][0], 2.0 * r[i][1], 2.0 * r[i][2], 5.0]);
        let turned = extent_of(&shape, key, &turn).unwrap();
        let searches = PROVEN.lock().unwrap().as_ref().map_or(0, |p| p.map.keys().filter(|(k, _)| *k == key).count());
        assert_eq!(searches, 1, "one search per shape");
        assert_eq!((printer.min, printer.max), ([model.min[0], -model.max[2], model.min[1]], [model.max[0], -model.min[2], model.max[1]]));
        // x' = -2y + 5, y' = 2x + 5, z' = 2z + 5
        let near = |a: [f64; 3], b: [f64; 3], tol: f64| (0..3).all(|k| (a[k] - b[k]).abs() <= tol);
        assert!(near(turned.min, [5.0 - 2.0 * model.max[1], 5.0 + 2.0 * model.min[0], 5.0 + 2.0 * model.min[2]], 1e-9), "{turned:?}");
        assert!(near(turned.max, [5.0 - 2.0 * model.min[1], 5.0 + 2.0 * model.max[0], 5.0 + 2.0 * model.max[2]], 1e-9), "{turned:?}");
        let Extent { min: lo, max: hi, .. } = makepad_csg_sdf::shape_bounds(&shape, &TO_PRINTER, EXTENT_TOL).unwrap();
        assert!(near(lo, printer.min, 2.0 * EXTENT_TOL) && near(hi, printer.max, 2.0 * EXTENT_TOL), "{lo:?} {hi:?} vs {printer:?}");
        assert!(near(model.min, [-9.0, -3.0, 0.0], 2.0 * EXTENT_TOL) && near(model.max, [11.0, 7.0, 6.0], 2.0 * EXTENT_TOL), "{model:?}");
    }

    /// A proof its job's cancel stops comes back open, reading nothing, and is never kept: the
    /// next one, no cancel raised, reads the field and closes every side to the tolerance.
    #[test]
    fn a_cancelled_proof_is_never_kept() {
        let ball = SdfNode::Sphere { radius: 4.25 };
        let shape = SdfShape { root: SdfNode::Transform { at: [0.5, -1.5, 2.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(ball) }, min: [-9.0; 3], max: [9.0; 3], materials: Vec::new() };
        let key = shape.key();
        let job = makepad_jobs::JobSpec::new("a proof").start();
        assert!(makepad_jobs::board().cancel(job.reporter().id()));
        let stopped = makepad_jobs::with_current(Some(job.reporter()), || extent_of(&shape, key, &IDENTITY3)).unwrap();
        job.cancelled();
        let proven = extent_of(&shape, key, &IDENTITY3).unwrap();
        let sides = |e: &Extent| e.work.iter().flatten().map(|w| (w.evaluations, w.budget_exhausted)).collect::<Vec<_>>();
        assert!(stopped.work.iter().flatten().all(|w| w.evaluations == 0 && w.budget_exhausted), "a cancelled proof reads nothing: {:?}", sides(&stopped));
        assert!(proven.work.iter().flatten().all(|w| w.evaluations > 0 && !w.budget_exhausted), "the proof after it is its own, every side read: {:?}", sides(&proven));
        let near = |a: [f64; 3], b: [f64; 3]| (0..3).all(|k| (a[k] - b[k]).abs() <= 2.0 * EXTENT_TOL);
        assert!(near(proven.min, [-3.75, -5.75, -2.25]) && near(proven.max, [4.75, 2.75, 6.25]), "{proven:?}");
    }

    /// A part above the plate floats unless it stands on printed material: a block on a block is
    /// held, and so is one on it in turn; one more than a layer above it, or on its edge alone,
    /// floats, and so does one standing on a block that floats.
    #[test]
    fn a_part_standing_on_another_is_held_and_one_a_gap_above_it_floats() {
        // A block by its model-frame half size and centre (the printer's z is the model's y).
        let block = |half: [f64; 3], at: [f64; 3]| {
            let root = SdfNode::Transform { at, rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Box { size: half.map(|h| 2.0 * h), radius: 0.0 }) };
            let shape = SdfShape { root, min: std::array::from_fn(|k| at[k] - half[k]), max: std::array::from_fn(|k| at[k] + half[k]), materials: Vec::new() };
            let key = shape.key();
            (Arc::new(shape), key)
        };
        let machine = Machine::dual_nozzle_350();
        let base = block([10.0, 2.0, 10.0], [0.0, 2.0, 0.0]);
        // (on the base's top at 4 mm: the blocks, whether each is held)
        let stacks = [
            (vec![block([4.0, 2.0, 4.0], [0.0, 6.0, 0.0]), block([2.0, 1.0, 2.0], [0.0, 9.0, 0.0])], [true, true]),
            (vec![block([4.0, 2.0, 4.0], [0.0, 6.3, 0.0]), block([2.0, 1.0, 2.0], [0.0, 9.3, 0.0])], [false, false]),
            (vec![block([4.0, 2.0, 4.0], [14.0, 6.0, 0.0])], [false, false]),
        ];
        for (blocks, held) in stacks {
            let shapes: Vec<_> = std::iter::once(base.clone()).chain(blocks).collect();
            let mut parts: Vec<Part> = shapes.iter().enumerate().map(|(i, (s, _))| {
                let mesh = PartMesh { positions: vec![[s.min[0], -s.max[2], s.min[1]], [s.max[0], -s.min[2], s.max[1]]], triangles: Vec::new() };
                Part { body_of: None, print_mesh: None, paint: None, print: Vec::new(), object: "stack".into(), name: format!("p{i}"), filament: 1, color: [1.0; 4], nozzle: None, mesh, field_bounds: None, field_floor: None, ..Default::default() }
            }).collect();
            let mut render = vec![RenderMesh::default(); parts.len()];
            let rests: Vec<_> = shapes.iter().map(|s| Some(s.clone())).collect();
            let (n, layer) = (parts.len(), 0.2);
            layout(&machine, &Rim::default(), false, &mut parts, &mut render, &BTreeMap::new(), &vec!["stack".to_string(); n], &BTreeMap::new(), &vec![None; n], &rests, &rests.iter().map(|r| r.as_ref().map(|(s, _)| s.clone())).collect::<Vec<_>>(), &[], layer, &mut |shape, key, m| extent_of(shape, key, m));
            for (k, part) in parts.iter().enumerate().skip(1) {
                assert!(machine::check_part(&machine, part).is_empty(), "no verdict before its proof");
                part.prove_floor();
                let floats = machine::check_part(&machine, part).iter().any(|i| matches!(i, machine::Issue::Floating { .. }));
                assert_eq!(floats, !held[k - 1], "block {k} of {:?}", part.mesh.bounds());
            }
            assert!(machine::check_part(&machine, &parts[0]).is_empty(), "the base rests on the plate");
        }
    }

    /// An assembly part whose tree box lies above a floor its object already has cannot set the
    /// object's rest: the layout leaves its floor pending (no proof runs in the evaluate), and the
    /// analysis that proves it gets the bottom proof's figure, dropped with the part.
    #[test]
    fn a_raised_part_proves_its_floor_when_asked() {
        // A block by its model-frame size and centre (the printer's z is the model's y).
        let block = |size: [f64; 3], at: [f64; 3]| {
            let root = SdfNode::Transform { at, rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Box { size, radius: 0.0 }) };
            let shape = SdfShape { root, min: std::array::from_fn(|k| at[k] - size[k]), max: std::array::from_fn(|k| at[k] + size[k]), materials: Vec::new() };
            let key = shape.key();
            (Arc::new(shape), key)
        };
        let part = |name: &str, (shape, _): &(Arc<SdfShape>, [u8; 32])| {
            let (lo, hi) = (shape.min, shape.max);
            let mesh = PartMesh { positions: vec![[lo[0], -hi[2], lo[1]], [hi[0], -lo[2], hi[1]]], triangles: Vec::new() };
            Part { body_of: None, print_mesh: None, paint: None, print: Vec::new(), object: "assembly".into(), name: name.into(), filament: 1, color: [1.0; 4], nozzle: None, mesh, field_bounds: None, field_floor: None, ..Default::default() }
        };
        let witness = |(shape, key): &(Arc<SdfShape>, [u8; 32])| bottom_of(shape, *key, &TO_PRINTER).and_then(|b| b.witness).unwrap();
        let machine = Machine::dual_nozzle_350();
        let base = block([10.0, 4.0, 10.0], [0.0, 2.0, 0.0]);
        let raised = block([4.0, 4.0, 4.0], [0.0, 20.0, 0.0]);
        let reaching = block([4.0, 4.0, 4.0], [12.0, 1.0, 0.0]);
        // (its parts, the one that sets the rest)
        for (shapes, rests_on) in [([&base, &raised], 0), ([&base, &reaching], 1)] {
            let mut parts: Vec<Part> = shapes.iter().enumerate().map(|(i, s)| part(&format!("p{i}"), s)).collect();
            let mut render = vec![RenderMesh::default(); 2];
            let group = vec!["assembly".to_string(); 2];
            let fields = vec![None, None];
            let rests: Vec<_> = shapes.iter().map(|s| Some((*s).clone())).collect();
            let (_, _, _, gaps, _) = layout(&machine, &Rim::default(), false, &mut parts, &mut render, &BTreeMap::new(), &group, &BTreeMap::new(), &fields, &rests, &rests.iter().map(|r| r.as_ref().map(|(s, _)| s.clone())).collect::<Vec<_>>(), &[], 0.2, &mut |shape, key, m| extent_of(shape, key, m));
            assert!(gaps.is_empty(), "a block's bottom closes: {gaps:?}");
            let (setter, other) = (rests_on, 1 - rests_on);
            assert_eq!(parts[setter].known_floor(), Some(0.0), "the lowest part is proven and rests on the bed");
            assert_eq!(parts[other].known_floor(), None, "the part above it is left pending");
            assert!(machine::check_part(&machine, &parts[other]).iter().all(|i| !matches!(i, machine::Issue::Floating { .. })), "no verdict before its proof");
            let copy = parts[other].clone();
            copy.prove_floor();
            // The drop is the setter's floor: the other's proven floor, moved by it.
            let gap = witness(shapes[other]) + -witness(shapes[setter]);
            assert_eq!(parts[other].known_floor(), Some(gap), "copies share the proof");
            assert_eq!(parts[other].floor(), gap);
            if gap > 0.5 {
                assert!(machine::check_part(&machine, &parts[other]).contains(&machine::Issue::Floating { gap }));
            }
        }
    }

    /// A field steeper than a distance (twice a box's) with a wide flat bottom, in a box 5 mm
    /// looser below: its bottom proof ends on its budget, short of the tolerance.
    struct Steep;
    impl makepad_csg_sdf::Sdf3 for Steep {
        fn distance(&self, p: makepad_csg_math::Vec3d) -> f64 {
            2.0 * (p.x.abs() - 30.0).max((p.y - 5.0).abs() - 5.0).max(p.z.abs() - 30.0)
        }
    }

    /// An assembly part resting by such a shape is reported with how far below its object's rest
    /// its solid may still reach; one raised above another part's floor cannot and is not.
    #[test]
    fn a_rest_set_by_an_open_bottom_is_reported() {
        let kernel = makepad_csg_graph::SdfKernel { key: 0x57ee9, field: Arc::new(Steep), splash: None, bounds: Some(([-35.0, -5.0, -35.0], [35.0, 15.0, 35.0])), solid: None };
        let steep = |y: f64| {
            let root = SdfNode::Transform { at: [0.0, y, 0.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Kernel(kernel.clone())) };
            let shape = SdfShape { root, min: [-35.0, y - 5.0, -35.0], max: [35.0, y + 15.0, 35.0], materials: Vec::new() };
            let key = shape.key();
            (Arc::new(shape), key)
        };
        let block = {
            let root = SdfNode::Transform { at: [40.0, 2.0, 0.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Box { size: [4.0, 4.0, 4.0], radius: 0.0 }) };
            let shape = SdfShape { root, min: [36.0, -2.0, -4.0], max: [44.0, 6.0, 4.0], materials: Vec::new() };
            let key = shape.key();
            (Arc::new(shape), key)
        };
        let machine = Machine::dual_nozzle_350();
        // (the steep part's height, whether its bottom may reach below the rest)
        for (y, sets) in [(0.0, true), (20.0, false)] {
            let shapes = [steep(y), block.clone()];
            let mut parts: Vec<Part> = shapes.iter().enumerate().map(|(i, (s, _))| {
                let mesh = PartMesh { positions: vec![[s.min[0], -s.max[2], s.min[1]], [s.max[0], -s.min[2], s.max[1]]], triangles: Vec::new() };
                Part { body_of: None, print_mesh: None, paint: None, print: Vec::new(), object: "assembly".into(), name: format!("p{i}"), filament: 1, color: [1.0; 4], nozzle: None, mesh, field_bounds: None, field_floor: None, ..Default::default() }
            }).collect();
            let mut render = vec![RenderMesh::default(); 2];
            let rests: Vec<_> = shapes.iter().map(|s| Some(s.clone())).collect();
            let (_, _, _, gaps, _) = layout(&machine, &Rim::default(), false, &mut parts, &mut render, &BTreeMap::new(), &vec!["assembly".to_string(); 2], &BTreeMap::new(), &[None, None], &rests, &rests.iter().map(|r| r.as_ref().map(|(s, _)| s.clone())).collect::<Vec<_>>(), &[], 0.2, &mut |shape, key, m| extent_of(shape, key, m));
            if sets {
                let witness = |(shape, key): &(Arc<SdfShape>, [u8; 32])| bottom_of(shape, *key, &TO_PRINTER).unwrap();
                let (b, block) = (witness(&shapes[0]), witness(&shapes[1]));
                assert!(b.work.budget_exhausted, "the flat bottom must stay open: {b:?}");
                let rest = b.witness.unwrap().min(block.witness.unwrap());
                assert_eq!(gaps, vec![(0, rest - b.bound)]);
                assert!(gaps[0].1 > EXTENT_TOL);
            } else {
                assert_eq!(parts[0].known_floor(), None, "the raised steep part is left pending");
                assert!(gaps.is_empty(), "{gaps:?}");
            }
        }
    }
}

#[cfg(test)]
mod cancel_tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeSet;

    /// The step a job is in: its stage's head (what runs in a step follows it after a " · ").
    fn step_of(job: makepad_jobs::JobId) -> String {
        let stage = makepad_jobs::board().get(job).map(|s| s.stage).unwrap_or_default();
        stage.split(" · ").next().unwrap_or_default().to_string()
    }

    /// What the next build is told apart from: the built instances' graphs, by identity, and the
    /// parts' frames and keys.
    fn record(e: &Evaluator) -> (Option<Vec<*const Graph>>, String) {
        (e.last.as_ref().map(|l| l.iter().map(|l| Arc::as_ptr(&l.graph)).collect()), format!("{:?}", e.last_parts))
    }

    /// A cancel raised at any check a build makes ends the build cancelled where it was: no step
    /// begins past it (the job's step is still the one it was in), and the evaluator keeps the
    /// record of the build before it, what the next build is told apart from. A held load's cancel,
    /// raised at its gate, stops the step in flight there. The build that sees no cancel completes.
    /// (A proof a cancel cuts short is never kept: [`super::extent_tests`].)
    #[test]
    fn a_cancel_begins_no_step_and_keeps_no_record() {
        // two hinged parts, one filled: every kind of step (joints sized, parts built and
        // measured, joints fitted, the plate laid out, painted and checked)
        let source = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\n\nlet a = m.box({size: [20mm, 6mm, 20mm] at: [0, 3mm, 0]})\nlet b = m.box({size: [20mm, 6mm, 20mm] at: [22mm, 3mm, 0]})\nlet pa = m.union(a, m.cylinder({radius: 4mm height: 6mm at: [11mm, 3mm, 0]}))\nlet pb = m.union(b, m.cylinder({radius: 4mm height: 6mm at: [11mm, 3mm, 0]}))\nlet h = m.hinge(pa, pb, {at: [11mm, 0, 0]})\nobject(pa, name: \"Probe\")\nm.fill(pb, Material{albedo: [0.9 0.7 0.1] name: \"Gold\"})\n";
        let c = crate::FabModel::with_script("test", "t.cad.splash", source).compile().unwrap_or_else(|f| panic!("{f}"));
        let mut e = Evaluator::new(fab_plate::machine::Machine::dual_nozzle_350());
        e.evaluate(&c, None).unwrap_or_else(|f| panic!("{f}"));
        let mut before = record(&e);
        assert!(before.0.is_some(), "a build keeps its record");
        // the cancel raised at the 1st check, then the 2nd, ... until a build makes fewer checks
        let mut cancelled_in: BTreeSet<String> = BTreeSet::new();
        let mut cancels = 0;
        for at in 1.. {
            let job = makepad_jobs::JobSpec::new("a build").start();
            let id = job.reporter().id();
            let (checks, raised_in) = (Cell::new(0), RefCell::new(None));
            let stop = || {
                checks.set(checks.get() + 1);
                if checks.get() == at {
                    *raised_in.borrow_mut() = Some(step_of(id));
                    makepad_jobs::board().cancel(id);
                }
                checks.get() >= at
            };
            let built = makepad_jobs::with_current(Some(job.reporter()), || e.evaluate(&c, Some(&stop)).map(|_| ()));
            let Some(was) = raised_in.take() else {
                // past its last check: no cancel, a whole build, its record the next one's
                built.unwrap_or_else(|f| panic!("{f}"));
                before = record(&e);
                job.done();
                break;
            };
            assert_eq!(step_of(id), was, "a step began after a cancel at check {at}");
            match built {
                Err(f) => {
                    assert_eq!(f.message, "cancelled", "a cancel at check {at}: {f}");
                    assert!(record(&e) == before, "a build cancelled at check {at} wrote its record");
                    cancelled_in.insert(was);
                    cancels += 1;
                    job.cancelled();
                }
                // past its last stop: it completes, its record the next one's
                Ok(()) => {
                    before = record(&e);
                    job.done();
                }
            }
        }
        // from before the first step to the last, in each step that checks in its course (one
        // that does not, its meshes built before, is checked at its start, in the step before)
        let steps: Vec<&str> = cancelled_in.iter().map(String::as_str).collect();
        println!("cancel sweep: {cancels} builds cancelled, in {steps:?}");
        for step in ["", "sizing the joints", "building", "measuring the parts", "laying out the plate"] {
            assert!(steps.iter().any(|s| s.starts_with(step) && (step.is_empty() == s.is_empty())), "no cancel in {step:?}: {steps:?}");
        }

        // a held load's cancel: at its gate, as its second step begins
        let job = makepad_jobs::JobSpec::new("a held build").start();
        let id = job.reporter().id();
        let held_in = Arc::new(std::sync::Mutex::new(String::new()));
        let held = held_in.clone();
        e.gate = Some(BuildGate {
            job: id,
            at_second_step: Box::new(move || {
                *held.lock().unwrap() = step_of(id);
                makepad_jobs::board().cancel(id);
            }),
        });
        let built = makepad_jobs::with_current(Some(job.reporter()), || e.evaluate(&c, None).map(|_| ()));
        assert!(matches!(&built, Err(f) if f.message == "cancelled"), "a build cancelled at its gate: {built:?}");
        let held_in = held_in.lock().unwrap().clone();
        assert!(!held_in.is_empty(), "the gate never ran");
        assert_eq!(step_of(id), held_in, "a step began after the gate's cancel");
        assert!(record(&e) == before, "a build cancelled at its gate wrote its record");
        job.cancelled();
    }
}

#[cfg(test)]
mod paint_change_tests {
    use super::*;
    use fab_paint_field::strokes::BRICK;
    use fab_paint_field::{Program, StrokeOp, StrokeSwatch, Strokes};

    fn paint(log: &Strokes, red: f32) -> PartPaint {
        let program = Arc::new(Program::default().with_strokes_on_top("a.strokes", Arc::new(log.clone()), Arc::new(vec![1])));
        let colors = vec![
            fab_plate::paint::PaintColor { rgb: [1.0; 3], color: None, filament: None, nozzle: None },
            fab_plate::paint::PaintColor { rgb: [red, 0.0, 0.0], color: Some("red".into()), filament: None, nozzle: None },
        ];
        PartPaint { program, colors, settings: Default::default(), to_printer: IDENTITY3, from_printer: IDENTITY3, stretch: 1.0, seen: None }
    }

    /// A paint stroke drawn on a strokes layer changes the paint only near it: outside the
    /// heights [`PartPaint::changed_since`] gives, every sample (slot and distance) is the old
    /// paint's.
    /// A recoloured slot changes it anywhere.
    #[test]
    fn a_paint_stroke_changes_the_paint_near_it_only() {
        let mut log = Strokes::new();
        let slot = log.slot_for(StrokeSwatch { rgb: [1.0, 0.0, 0.0], filament: None });
        log.push(StrokeOp::Paint { slot }, 1.0, 1.0, &[[0.0, 0.0, 2.0], [4.0, 0.0, 2.0]]);
        let old = paint(&log, 1.0);
        log.push(StrokeOp::Paint { slot }, 1.0, 1.0, &[[0.0, 0.0, 20.0], [4.0, 0.0, 20.0]]);
        let new = paint(&log, 1.0);
        let heights = new.changed_since(&old).expect("a stroke changes the paint locally");
        let inside = |q: [f64; 3]| heights.iter().any(|&(a, b)| q[2] >= a && q[2] <= b);
        let on = [2.0, 0.0, 20.0];
        assert!(inside(on) && new.sample_at(on).slot != old.sample_at(on).slot, "the new stroke paints");
        let (lo, hi) = heights.iter().fold((f64::MAX, f64::MIN), |(a, b), &(lo, hi)| (a.min(lo), b.max(hi)));
        assert!(lo >= 20.0 - 1.0 - 3.0 * BRICK && hi <= 20.0 + 1.0 + 3.0 * BRICK, "the change reaches z {lo}..{hi}");
        let mut outside = 0;
        for i in -20..=30 {
            for j in -20..=20 {
                for k in -5..=70 {
                    let q = [i as f64 * 0.4, j as f64 * 0.4, k as f64 * 0.4];
                    if inside(q) {
                        continue;
                    }
                    outside += 1;
                    let (a, b) = (new.sample_at(q), old.sample_at(q));
                    assert!(a.slot == b.slot && a.dist.to_bits() == b.dist.to_bits(), "{q:?}: {a:?} against {b:?}");
                }
            }
        }
        assert!(outside > 10_000, "{outside} samples outside the change");
        assert!(paint(&log, 0.5).changed_since(&new).is_none(), "a recoloured slot repaints anywhere");
    }

    /// A pattern grown anew in a boxed region (another seed) changes the paint only in that box:
    /// outside the heights [`PartPaint::changed_since`] gives (the box's, at a slab's top), every
    /// sample is the old paint's, and a whole-part pattern's change is anywhere.
    #[test]
    fn a_regrown_pattern_changes_the_paint_in_its_region_only() {
        use fab_paint_field::pattern::{band::{each, Surface}, live, Spec};
        use fab_paint_field::{Channel, Layer, Mix, MixField, Op, Region};
        let half = [20.0, 20.0, 20.0];
        let distance = each(move |p: [f64; 3]| {
            let q: [f64; 3] = std::array::from_fn(|k| (p[k] - half[k]).abs() - half[k]);
            let out = q.map(|v| v.max(0.0));
            (out[0] * out[0] + out[1] * out[1] + out[2] * out[2]).sqrt() + q[0].max(q[1]).max(q[2]).min(0.0)
        });
        let surface = Surface { distance, clearance: makepad_csg_sdf::Clearance::uniform(1.0), bounds: ([0.0; 3], [40.0; 3]), key: 7 };
        let fan = |n: usize, f: &(dyn Fn(usize) + Sync)| makepad_fab_slicer::par::fan(n, f);
        let grown = |seed: f64| {
            let spec = Spec::new(Some("spots"), &[("scale", 4.0), ("seed", seed), ("steps", 600.0)]).unwrap();
            live::resolve(&spec, 7, 0, &|| Some(surface.clone()), true, &fan, &|| false)
        };
        use makepad_csg_sdf::SdfNode;
        let corner = SdfNode::Transform { at: [10.0, 10.0, 40.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Box { size: [12.0, 12.0, 12.0], radius: 0.0 }) };
        let paint = |region: Region, seed: f64| {
            let mix = Mix { field: MixField::Pattern(grown(seed)), per_mm: 1.0, slots: vec![1, 2], sharp: 1.0, balance: 0.0 };
            let program = Arc::new(fab_paint_field::Program::new(vec![Layer { region, op: Op::Mix { channel: Channel::Color, mix: Arc::new(mix) } }]));
            let colors = vec![
                fab_plate::paint::PaintColor { rgb: [1.0; 3], color: None, filament: None, nozzle: None },
                fab_plate::paint::PaintColor { rgb: [0.1, 0.6, 0.6], color: Some("teal".into()), filament: None, nozzle: None },
                fab_plate::paint::PaintColor { rgb: [0.1, 0.1, 0.1], color: Some("dark".into()), filament: None, nozzle: None },
            ];
            PartPaint { program, colors, settings: Default::default(), to_printer: IDENTITY3, from_printer: IDENTITY3, stretch: 1.0, seen: None }
        };
        let field = || Region::Field { node: corner.clone(), grow: 0.0 };
        let (old, new) = (paint(field(), 1.0), paint(field(), 2.0));
        let heights = new.changed_since(&old).expect("a pattern in a box changes the paint in it");
        let inside = |q: [f64; 3]| heights.iter().any(|&(a, b)| q[2] >= a && q[2] <= b);
        let (mut outside, mut moved) = (0, 0);
        for i in 0..=100 {
            for j in 0..=100 {
                for k in [0.0, 0.3, 3.0, 10.0, 20.0, 39.7, 40.0] {
                    let q = [i as f64 * 0.4, j as f64 * 0.4, k];
                    let (a, b) = (new.sample_at(q), old.sample_at(q));
                    if inside(q) {
                        moved += (a.slot != b.slot) as usize;
                        continue;
                    }
                    outside += 1;
                    assert!(a.slot == b.slot && a.dist.to_bits() == b.dist.to_bits(), "{q:?}: {a:?} against {b:?}");
                }
            }
        }
        assert!(outside > 20_000 && moved > 50, "{outside} samples outside the change, {moved} inside it moved");
        assert!(paint(Region::All, 2.0).changed_since(&paint(Region::All, 1.0)).is_none(), "a whole-part pattern changes anywhere");
    }

    /// A stem standing on a foot with a bead `bead` mm round on it, along z: features 1, 2 and 3
    /// (foot, stem, bead) filled in order with slots `fills`.
    fn stem_paint(bead: f64, fills: &[(u32, u8)]) -> PartPaint {
        use fab_paint_field::operand::Body;
        use fab_paint_field::{Channel, Layer, Op, Region};
        use makepad_csg_sdf::{SdfNode, SdfOp};
        let at = |z: f64, n: SdfNode| SdfNode::Transform { at: [0.0, 0.0, z], rotate: [0.0; 3], scale: 1.0, of: Box::new(n) };
        let id = |id: u32, n: SdfNode| SdfNode::Id { id, of: Box::new(n) };
        let root = SdfNode::Combine {
            op: SdfOp::Union,
            blend: 1.0,
            of: vec![
                id(1, at(2.0, SdfNode::Box { size: [16.0, 16.0, 4.0], radius: 0.0 })),
                id(2, at(40.0, SdfNode::Box { size: [6.0, 6.0, 80.0], radius: 0.0 })),
                id(3, at(40.0, SdfNode::Sphere { radius: bead })),
            ],
        };
        let body = Body::new(&root);
        let fill = |&(tag, slot): &(u32, u8)| Layer { region: Region::Operand(Arc::new(vec![body.operand(tag).expect("tagged")])), op: Op::Paint { channel: Channel::Color, slot } };
        let program = Arc::new(fab_paint_field::Program::new(fills.iter().map(fill).collect()));
        let colour = |rgb: [f32; 3], name: &str| fab_plate::paint::PaintColor { rgb, color: Some(name.into()), filament: None, nozzle: None };
        let colors = vec![fab_plate::paint::PaintColor { rgb: [1.0; 3], color: None, filament: None, nozzle: None }, colour([0.9, 0.7, 0.1], "amber"), colour([0.1, 0.3, 0.8], "blue")];
        PartPaint { program, colors, settings: Default::default(), to_printer: IDENTITY3, from_printer: IDENTITY3, stretch: 1.0, seen: None }
    }

    /// The heights `new` gives against `old` ([`PartPaint::changed_since`]), after checking they
    /// keep within `reach` and that round the stem every sample past them (slot and distance) is
    /// the old paint's; how many samples are past them, and how many within them changed slot.
    fn alike_past_the_heights(new: &PartPaint, old: &PartPaint, reach: (f64, f64)) -> (usize, usize) {
        let heights = new.changed_since(old).expect("a change with bounds");
        let (lo, hi) = heights.iter().fold((f64::MAX, f64::MIN), |(a, b), &(lo, hi)| (a.min(lo), b.max(hi)));
        assert!(lo >= reach.0 && hi <= reach.1, "the change reaches z {lo:.1}..{hi:.1}");
        let inside = |q: [f64; 3]| heights.iter().any(|&(a, b)| q[2] >= a && q[2] <= b);
        let (mut outside, mut moved) = (0, 0);
        for i in -15..=15 {
            for j in -15..=15 {
                for k in -3..=137 {
                    let q = [i as f64 * 0.6, j as f64 * 0.6, k as f64 * 0.6];
                    let (a, b) = (new.sample_at(q), old.sample_at(q));
                    if inside(q) {
                        moved += usize::from(a.slot != b.slot);
                        continue;
                    }
                    outside += 1;
                    assert!(a.slot == b.slot && a.dist.to_bits() == b.dist.to_bits(), "{q:?}: {a:?} against {b:?}");
                }
            }
        }
        (outside, moved)
    }

    /// A feature's field an edit moves changes its part's feature fills round it only: a bead
    /// grown on a stem repaints the bead's fill and the stem's near the bead, the foot's not at
    /// all, and outside the heights [`PartPaint::changed_since`] gives every sample (slot and
    /// distance) is the old paint's: the layers well below and above the bead print as before.
    #[test]
    fn a_feature_grown_on_its_part_changes_the_fills_round_it_only() {
        let fills = [(1, 1), (2, 2), (3, 1)];
        let (outside, moved) = alike_past_the_heights(&stem_paint(4.5, &fills), &stem_paint(4.0, &fills), (8.0, 72.0));
        assert!(outside > 25_000 && moved > 50, "{outside} samples outside the change, {moved} inside it moved");
    }

    /// A fill added between others, or taken away, changes the paint in its own box only: the
    /// layers before and after it pair as they were, and past the heights
    /// [`PartPaint::changed_since`] gives every sample (slot and distance) is the old paint's.
    #[test]
    fn a_fill_added_between_others_changes_the_paint_in_its_box_only() {
        let (was, now) = (stem_paint(4.0, &[(1, 1), (2, 2)]), stem_paint(4.0, &[(1, 1), (3, 1), (2, 2)]));
        for (new, old, what) in [(&now, &was, "added"), (&was, &now, "taken away")] {
            let (outside, moved) = alike_past_the_heights(new, old, (8.0, 72.0));
            assert!(outside > 25_000 && moved > 50, "a fill {what}: {outside} samples outside the change, {moved} inside it moved");
        }
        let (ahead, after) = (stem_paint(4.0, &[(3, 1), (1, 1), (2, 2)]), stem_paint(4.0, &[(1, 1), (2, 2), (3, 1)]));
        for (new, what) in [(&ahead, "ahead of the others"), (&after, "after them")] {
            let (outside, _) = alike_past_the_heights(new, &was, (8.0, 72.0));
            assert!(outside > 25_000, "a fill {what}: {outside} samples outside the change");
        }
    }
}

/// One part of a plate of test prints as the views show it ([`Evaluated::of_pieces`]).
pub struct ShownPiece {
    /// The plate part it is cut from.
    pub of: usize,
    /// Where it stands: its id and name, a mesh part's mesh cut and closed, a field part's box.
    pub part: Part,
    /// Its display mesh, for a part that shows one.
    pub render: RenderMesh,
    /// A field part's shape, cut.
    pub field: Option<makepad_csg_graph::SdfShape>,
    /// From the plate to where it stands (row-major, rigid).
    pub to_bed: [[f64; 4]; 3],
}

impl Evaluated {
    /// The plate of these pieces, for the views and the checks: what they show of each is its
    /// part's, nothing is built, and none of it is ever edited or sliced.
    pub fn of_pieces(&self, pieces: Vec<ShownPiece>) -> Evaluated {
        let rigid_inverse = |m: &[[f64; 4]; 3]| -> [[f64; 4]; 3] {
            std::array::from_fn(|r| {
                let row = [m[0][r], m[1][r], m[2][r]];
                [row[0], row[1], row[2], -(row[0] * m[0][3] + row[1] * m[1][3] + row[2] * m[2][3])]
            })
        };
        let n = pieces.len();
        let mut parts = Vec::with_capacity(n);
        let mut render = Vec::with_capacity(n);
        let mut model_to_plate = Vec::with_capacity(n);
        let mut frames = Vec::with_capacity(n);
        let mut fields = Vec::with_capacity(n);
        let mut paint = Vec::with_capacity(n);
        let mut part_colors = Vec::with_capacity(n);
        for piece in pieces {
            let ShownPiece { of, part, render: shown, field, to_bed } = piece;
            let field = field.zip(self.fields.get(of).and_then(Option::as_ref)).map(|(shape, f)| {
                let (_, _, to_printer, _) = f.distance();
                let to_printer = affine_mul(&to_bed, to_printer);
                PartField { node: f.node, key: shape.key(), shape: Arc::new(shape), to_printer, to_part: to_printer, bounds: part.field_bounds, bottom_gap: None, volume: None, stretched: None }
            });
            fields.push(field);
            paint.push(self.paint.get(of).and_then(Option::as_ref).map(|p| Arc::new(PartPaint {
                to_printer: affine_mul(&to_bed, &p.to_printer),
                from_printer: affine_mul(&p.from_printer, &rigid_inverse(&to_bed)),
                ..(**p).clone()
            })));
            model_to_plate.push(self.model_to_plate.get(of).map_or(to_bed, |m| affine_mul(&to_bed, m)));
            frames.push(PartFrame { local: Arc::new(part.mesh.clone()), offset: [0.0; 3] });
            part_colors.push(self.part_colors.get(of).cloned().unwrap_or_default());
            parts.push(part);
            render.push(shown);
        }
        Evaluated {
            handles: vec![PartHandles::default(); n],
            printability: Arc::new(fab_printability::Report::default()),
            content: self.content,
            plate: Plate { excluded: Vec::new(), parts, settings: self.plate.settings.clone() },
            render,
            objects: Vec::new(),
            reports: Vec::new(),
            machine: self.machine.clone(),
            rim: self.rim,
            warnings: Vec::new(),
            millis: 0.0,
            model_to_plate,
            built: 0,
            frames,
            changes: vec![PartChange::New; n],
            phases: Vec::new(),
            fields,
            paint,
            features: vec![Arc::new(Vec::new()); n],
            excluded_paint: Vec::new(),
            excluded_fields: Vec::new(),
            excluded_features: Vec::new(),
            studies: vec![None; n],
            excluded_studies: Vec::new(),
            hands: vec![Vec::new(); n],
            mech: Vec::new(),
            weaves: Vec::new(),
            profile: self.profile.clone(),
            colors: self.colors.clone(),
            part_colors,
            mapped: self.mapped.clone(),
            view: self.view,
            shapes: vec![None; n],
            viewed: vec![false; n],
            taken: Vec::new(),
            drifted: Vec::new(),
            lost: Vec::new(),
        }
    }
}

