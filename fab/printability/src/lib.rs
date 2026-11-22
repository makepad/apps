//! Printability in printer millimetres. Both passes belong on workers.
//! Mesh analysis is linear in triangles; slice analysis uses the slicer's
//! integer polygon operations and the same timed records as the preview.

mod contours;
mod inside;
pub mod insert;
mod limb;
mod mesh;
mod orient;
mod overlap;
mod overhang_list;
mod sliced;
mod strength;
pub use strength::*;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod floating_tests;

pub use contours::{contour_checks, vase_checks};
pub use inside::{printed_inside, Printed};
pub use limb::*;
pub use orient::*;
pub use mesh::{above_its_group, analyse_part, analyse_part_with, analyse_plate, analyse_plate_with, combine_plate, overhang_degree, overhang_patches, render_overhang, OverhangPatch, PartAnalysis, PartField};
pub use overhang_list::{OverhangLine, OverhangList, Unheld};
pub use sliced::{analyse_slice, analyse_slice_memo, analyse_slice_with, KeptOut, SliceMemo};
use makepad_fab_toolpath::skeleton::GivenUp;
use makepad_fab_settings::machine::{filament_suits_plate, Job};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity { Error, Warning, Info }

impl Severity {
    pub fn name(self) -> &'static str {
        match self { Self::Error => "error", Self::Warning => "warning", Self::Info => "info" }
    }
}

/// The detector's region in XY, before the view adds its framing margin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Footprint {
    pub min: [f64; 2],
    pub max: [f64; 2],
}

impl Footprint {
    pub fn of_bounds(b: fab_plate::Bounds) -> Self {
        Self { min: [b.min[0], b.min[1]], max: [b.max[0], b.max[1]] }
    }

    pub(crate) fn of_box(b: makepad_fab_slicer::geom::BBox) -> Self {
        Self { min: [b.min.x_mm(), b.min.y_mm()], max: [b.max.x_mm(), b.max.y_mm()] }
    }

    pub(crate) fn of_regions(regions: &[makepad_fab_slicer::geom::ExPolygon]) -> Self {
        Self::of_box(makepad_fab_slicer::geom::bbox_of(regions))
    }

    /// A point detector must state the side length of the square it marks.
    pub fn square(at: [f64; 3], side_mm: f64) -> Self {
        Self { min: [at[0] - side_mm * 0.5, at[1] - side_mm * 0.5], max: [at[0] + side_mm * 0.5, at[1] + side_mm * 0.5] }
    }

    pub fn translated(self, xy: [f64; 2]) -> Self {
        Self { min: [self.min[0] + xy[0], self.min[1] + xy[1]], max: [self.max[0] + xy[0], self.max[1] + xy[1]] }
    }

    pub(crate) fn union(self, other: Self) -> Self {
        Self { min: [self.min[0].min(other.min[0]), self.min[1].min(other.min[1])], max: [self.max[0].max(other.max[0]), self.max[1].max(other.max[1])] }
    }
}

#[derive(Clone, Debug)]
pub struct Issue {
    pub kind: &'static str,
    pub severity: Severity,
    pub object: Option<String>,
    pub at_mm: [f64; 3],
    /// XY bounds in printer millimetres, on the navigation layer for a layer range.
    pub footprint: Footprint,
    /// The move records' layer number, not an index into a compacted list.
    /// For a range, this is its first layer and remains the navigation target.
    pub layer: Option<u32>,
    pub layer_to: Option<u32>,
    pub measure: f64,
    pub message: String,
    pub hint: String,
    /// An available one-click repair, dispatched for this issue's object.
    pub fix: Option<&'static str>,
    pub area_mm2: f64,
    pub lowest_z: f64,
    pub highest_z: f64,
    /// Reach flags: nozzle area, nozzle height, bed, machine height.
    pub flags: u8,
}

/// How near (mm) the given-up overhang of one layer lies to that of the same or the next layer
/// to be one region with it: a face going up leaves a band per layer, each beside the last.
const REGION_REACH_MM: f64 = 1.0;

/// Whether two entries' boxes lie within `reach` (mm) of each other.
fn boxes_meet(a: &GivenUp, b: &GivenUp, reach: f64) -> bool {
    a.bounds.min.x_mm() - reach <= b.bounds.max.x_mm() && b.bounds.min.x_mm() - reach <= a.bounds.max.x_mm() && a.bounds.min.y_mm() - reach <= b.bounds.max.y_mm() && b.bounds.min.y_mm() - reach <= a.bounds.max.y_mm()
}

/// The area (mm²) a list of given-up entries leaves without support. A layer's overhang is told
/// once per cause (the support under it taken away, no trunk reaching it), and causes tell the
/// same place: of one layer's entries whose boxes meet, the largest counts. What an item of
/// [`support_issues`] says, and what the Slice panel's overhang list shows for the same.
pub fn told_area(list: &[&GivenUp]) -> f64 {
    let mut order: Vec<&GivenUp> = list.to_vec();
    order.sort_by(|a, b| a.layer.cmp(&b.layer).then(b.area.total_cmp(&a.area)));
    // the counted entries of the layer at hand
    let mut layer: Vec<&GivenUp> = Vec::new();
    let mut area = 0.0;
    for g in order {
        if layer.first().is_some_and(|c| c.layer != g.layer) {
            layer.clear();
        }
        if !layer.iter().any(|c| boxes_meet(c, g, 0.0)) {
            area += g.area;
            layer.push(g);
        }
    }
    area
}

/// A plan's given-up overhang, told per model layer, as the regions it makes: entries on the
/// same or the next layer whose boxes lie within [`REGION_REACH_MM`] of each other are one
/// region. The largest region comes first.
fn regions(list: &[GivenUp]) -> Vec<Vec<&GivenUp>> {
    fn root(lead: &mut [usize], mut i: usize) -> usize {
        while lead[i] != i {
            lead[i] = lead[lead[i]];
            i = lead[i];
        }
        i
    }
    let near = |a: &GivenUp, b: &GivenUp| boxes_meet(a, b, REGION_REACH_MM);
    let mut order: Vec<usize> = (0..list.len()).collect();
    order.sort_by_key(|&i| list[i].layer);
    let mut lead: Vec<usize> = (0..list.len()).collect();
    for (n, &i) in order.iter().enumerate() {
        for &j in order[n + 1..].iter().take_while(|&&j| list[j].layer <= list[i].layer + 1) {
            if near(&list[i], &list[j]) {
                let (a, b) = (root(&mut lead, i), root(&mut lead, j));
                lead[a.max(b)] = a.min(b);
            }
        }
    }
    // Each region in the order of its lowest entry, then the largest first.
    let mut slot = vec![usize::MAX; list.len()];
    let mut out: Vec<Vec<&GivenUp>> = Vec::new();
    for &i in &order {
        let r = root(&mut lead, i);
        if slot[r] == usize::MAX {
            slot[r] = out.len();
            out.push(Vec::new());
        }
        out[slot[r]].push(&list[i]);
    }
    let mut sized: Vec<(f64, Vec<&GivenUp>)> = out.into_iter().map(|r| (told_area(&r), r)).collect();
    sized.sort_by(|a, b| b.0.total_cmp(&a.0));
    sized.into_iter().map(|(_, r)| r).collect()
}

/// The floating placement verdict once this part's support plan is complete.
/// `lowest_layer` is its first occupied model slice, in the same layer numbering
/// as the skeleton. No plan (or no occupied slice) cannot certify support.
/// Disabled supports keep the mesh pass's warning, so are not repeated here.
pub fn supported_placement(machine: &fab_plate::machine::Machine, part: &fab_plate::Part, materials: &Materials, skeleton: &makepad_fab_toolpath::skeleton::Skeleton, lowest_layer: Option<usize>) -> Option<Issue> {
    if !materials.of_part(part).supports { return None; }
    let lowest = lowest_layer?;
    let mut issue = mesh::placement(machine, part, None).into_iter().find(|i| i.kind == "mid_air")?;
    // Read the ungrouped entries: a region can join this face to upper ledges,
    // and its widest entry need not be the one under the lowest face.
    let missed = skeleton.given_up.iter().chain(&skeleton.blocked).chain(&skeleton.inside).chain(&skeleton.locked).chain(&skeleton.kept_out).chain(&skeleton.no_trees)
        .any(|g| g.layer == lowest && g.area > 0.0);
    let has_support = skeleton.chains.iter().any(|c| !c.sections.is_empty())
        || skeleton.slabs.iter().any(|s| !s.region.is_empty());
    if has_support && !missed {
        issue.severity = Severity::Info;
        issue.message = format!("stands on its supports {:.2} mm above the plate", issue.measure);
        issue.hint = "The support plan reaches the part's lowest face.".into();
    } else {
        issue.hint = "The support plan leaves the lowest face unsupported. Drop the part onto the bed or change its supports.".into();
    }
    Some(issue)
}

/// What a part's supports leave out, as its plan says, with why: the overhang a support block
/// covers, what no support could reach, what the part is in the way of, so no support gets to
/// it, and what a support would get to only to be locked in the part, each region on its own;
/// what lies in the footprint or a no-trees area the part keeps its trees out of, all of each
/// together; and what of the supports is locked in, where the part lets them be, and any rule the
/// support breaks. Each overhang is told once. `heights`: its model layers (top z, height);
/// `offset`: where its plan's frame lies on the plate. Warnings, and what the person's own
/// settings keep the trees out from under as information: the part still prints, with those
/// faces in the air.
pub fn support_issues(object: &str, skeleton: &makepad_fab_toolpath::skeleton::Skeleton, heights: &[(f64, f64)], offset: [f64; 2]) -> Vec<Issue> {
    // One item for what `list` holds: at its widest layer's widest place, over all its layers.
    // `message`: from its area (mm²), place (plate mm) and first and last layer (0-based).
    let issue = |kind: &'static str, list: &[&GivenUp], message: &dyn Fn(f64, [f64; 3], u32, u32) -> String, hint: &str| {
        let widest = list.iter().max_by(|a, b| a.area.total_cmp(&b.area))?;
        let area = told_area(list);
        let z = |g: &&GivenUp| heights.get(g.layer).map_or(0.0, |h| h.0);
        // What is left out over all its layers, in XY.
        let footprint = list.iter().map(|g| Footprint::of_box(g.bounds)).reduce(Footprint::union)?.translated(offset);
        let at = [widest.at[0] + offset[0], widest.at[1] + offset[1], z(widest)];
        let (from, to) = (list.iter().map(|g| g.layer as u32).min()?, list.iter().map(|g| g.layer as u32).max()?);
        let mut i = Issue::new(kind, Severity::Warning, Some(object), at, footprint, area, message(area, at, from, to), hint);
        i.area_mm2 = area;
        i.layer = Some(from);
        i.layer_to = Some(to);
        i.lowest_z = list.iter().map(&z).fold(f64::MAX, f64::min);
        i.highest_z = list.iter().map(&z).fold(f64::MIN, f64::max);
        Some(i)
    };
    // What the person chose: told, at the severity that never refuses the print.
    let info = |mut i: Issue| {
        i.severity = Severity::Info;
        i
    };
    // Where and how much, as the message says it: layers 1-based, as the view counts them.
    let told = |area: f64, at: [f64; 3], from: u32, to: u32| {
        let layers = if from == to { format!("layer {}", from + 1) } else { format!("layers {}–{}", from + 1, to + 1) };
        format!("{area:.1} mm² at {:.1}, {:.1}, {:.1} mm, {layers}", at[0], at[1], at[2])
    };
    // A plan of normal supports has slabs; a tree plan has none.
    let support = if skeleton.slabs.is_empty() { "tree" } else { "support" };
    // A rule the support breaks, per rule: where and how much (see
    // [`makepad_fab_toolpath::support_rules`]).
    let broken = makepad_fab_toolpath::support_rules::Rule::ALL.into_iter().filter_map(|rule| {
        let list: Vec<&makepad_fab_toolpath::support_rules::Break> = skeleton.breaks.iter().filter(|b| b.rule == rule).collect();
        let widest = list.iter().max_by(|a, b| a.area_mm2.total_cmp(&b.area_mm2))?;
        let area: f64 = list.iter().map(|b| b.area_mm2).sum();
        let z = |b: &makepad_fab_toolpath::support_rules::Break| heights.get(b.layer).map_or(0.0, |h| h.0);
        let footprint = list.iter().map(|b| Footprint::of_box(b.bounds)).reduce(Footprint::union)?.translated(offset);
        let mut i = Issue::new("support_rule", Severity::Warning, Some(object), [widest.at.0 + offset[0], widest.at.1 + offset[1], z(widest)], footprint, area,
            format!("{area:.1} mm² of support breaks {}: {} ({}), in {} place{}", rule.code(), rule.told(), widest.why, list.len(), if list.len() == 1 { "" } else { "s" }),
            "That support may not print as planned: look at these layers in Sliced.");
        i.area_mm2 = area;
        i.layer = list.iter().map(|b| b.layer as u32).min();
        i.layer_to = list.iter().map(|b| b.layer as u32).max();
        i.lowest_z = list.iter().map(|b| z(b)).fold(f64::MAX, f64::min);
        i.highest_z = list.iter().map(|b| z(b)).fold(f64::MIN, f64::max);
        Some(i)
    });
    let blocked: Vec<&GivenUp> = skeleton.blocked.iter().collect();
    let blocked = issue("support_blocked", &blocked, &|a, _, _, _| format!("{a:.0} mm² of overhang has no support: a support block covers it"), "It prints with nothing under it. Shrink the block if this surface has to hold.");
    // Each region on its own, so each can be selected and looked at.
    let unreached = regions(&skeleton.given_up).into_iter().filter_map(|r| {
        issue("support_unreached", &r, &|a, at, from, to| format!("Unsupported overhang: no {support} can reach it ({})", told(a, at, from, to)), "It prints with nothing under it. Allow supports on the part, turn the part, or chamfer under the face.")
    });
    let bed_only = skeleton.hold.is_some_and(|h| h.bed_only);
    let inside = regions(&skeleton.inside).into_iter().filter_map(|r| {
        issue("support_inside", &r, &|a, at, from, to| format!("Unsupported overhang: the part is in the way, no {support} gets to it ({})", told(a, at, from, to)), if bed_only { "It prints with nothing under it. Allow supports on the part, or turn the part." } else { "It prints with nothing under it. Turn the part, or open it up under the face." })
    });
    // The window a support would have had to come out through.
    let window = skeleton.hold.map_or(0.0, |h| h.window);
    let locked = regions(&skeleton.locked).into_iter().filter_map(|r| {
        issue("support_locked", &r, &|a, at, from, to| format!("Unsupported overhang: a {support} under it could not come out, window {window:.1} mm ({})", told(a, at, from, to)), "It prints with nothing under it: supports stay out of where they would be locked in the part. Turn on Allow supports that can't be removed in this part's support settings to hold it anyway.")
    });
    // What the part keeps its trees out from under, by its own settings: its footprint, which a
    // branch reaches into only where it gets back out, and its no-trees areas. The person chose
    // it: told once each, all of it together, never as a fault (the sliced checks tell none of
    // it as uncovered).
    let kept_out: Vec<&GivenUp> = skeleton.kept_out.iter().collect();
    let kept_out = issue("support_footprint", &kept_out, &|a, at, from, to| format!("Overhang inside the footprint, unsupported by your setting ({})", told(a, at, from, to)), "It prints with nothing under it: Only outside the part's footprint keeps the trees out from under the part, and a branch reaches in only where it gets straight back out. Turn it off in this part's support settings to hold it.").map(info);
    let no_trees: Vec<&GivenUp> = skeleton.no_trees.iter().collect();
    let no_trees = issue("support_no_trees", &no_trees, &|a, at, from, to| format!("Overhang inside a no-trees box, unsupported by your setting ({})", told(a, at, from, to)), "It prints with nothing under it: No trees here keeps the trees out of the box, and a branch reaches in over it only from outside. Move or shrink the box to hold it.").map(info);
    let unremovable = regions(&skeleton.unremovable).into_iter().filter_map(|r| {
        issue("support_unremovable", &r, &|a, at, from, to| format!("Support held, not removable through a {window:.1} mm window ({})", told(a, at, from, to)), "This part allows supports that can't be removed: they hold the overhang here, but stay in the part once printed.")
    });
    // Supports that reach only a small share of what the part asks of them: told once, over all
    // they leave, with what lets them reach more.
    let low = skeleton.hold.filter(|h| h.need >= LOW_HOLD_NEED).and_then(|h| {
        let left: Vec<&GivenUp> = skeleton.given_up.iter().chain(&skeleton.inside).chain(&skeleton.locked).collect();
        let held = h.held;
        if held >= LOW_HOLD * h.need {
            return None;
        }
        // The gap as it was set: 0.35 mm, 2 mm.
        let gap = h.side_gap;
        let hint = if h.bed_only {
            format!("From the bed alone, {gap} mm off the part's sides, supports can't get under most of it. Allow supports on the part, or use a smaller side gap.")
        } else {
            format!("Kept {gap} mm off the part's sides, supports can't get under most of it. Use a smaller side gap, or turn the part.")
        };
        issue("support_low_hold", &left, &|_, _, _, _| format!("Supports hold only {:.0} % of this part's overhang ({held:.0} of {:.0} mm²)", 100.0 * held / h.need, h.need), &hint)
    });
    low.into_iter().chain(blocked).chain(unreached).chain(inside).chain(locked).chain(kept_out).chain(no_trees).chain(unremovable).chain(broken).collect()
}

/// Under this share of a part's overhang held, its supports are told to hold little.
const LOW_HOLD: f64 = 0.5;

/// Below this much overhang (mm²) a part's supports are not told to hold little: what they leave
/// is told on its own.
const LOW_HOLD_NEED: f64 = 10.0;

/// A textured part's warnings (it still slices): a pattern whose narrowest features (`feature`,
/// mm) are under a line prints blurred, and a depth under a layer barely shows. `textures` holds
/// each texture's narrowest feature and depth.
pub fn texture_issues(object: &str, at_mm: [f64; 3], footprint: Footprint, textures: &[(f64, f64)], materials: &Materials) -> Vec<Issue> {
    let (line, layer) = (materials.line_width, materials.first_layer_height);
    let mut out = Vec::new();
    for &(feature, depth) in textures {
        if feature > 0.0 && feature < line {
            out.push(Issue::new("texture_fine", Severity::Warning, Some(object), at_mm, footprint, feature,
                format!("{object}: its texture's finest features ({feature:.2} mm) are narrower than a line ({line:.2} mm)"),
                "a coarser pitch (or a finer nozzle) prints the pattern; this one prints blurred"));
        } else if depth.abs() < layer {
            out.push(Issue::new("texture_fine", Severity::Warning, Some(object), at_mm, footprint, depth.abs(),
                format!("{object}: its texture is {:.2} mm deep, under a layer ({layer:.2} mm)", depth.abs()),
                "a deeper texture shows on the print; this one barely does"));
        }
    }
    out
}

/// An object whose bodies print as one (`object([..])`, [`fab_plate::Part::body_of`]) that the
/// slicer prints apart all the same, as separate parts: each with walls of its own, and where they
/// overlap each prints it. `bodies` are its parts (plate indices), `why` why.
pub fn bodies_apart(plate: &fab_plate::Plate, bodies: &[usize], why: &str) -> Issue {
    let first = &plate.parts[bodies[0]];
    let object = first.body_of.as_deref().unwrap_or(&first.object);
    let bounds = bodies.iter().fold(fab_plate::Bounds::EMPTY, |b, &i| b.union(&plate.parts[i].bounds()));
    let mut issue = Issue::new("bodies_apart", Severity::Warning, Some(&first.object), bounds.center(), Footprint::of_bounds(bounds), bodies.len() as f64,
        format!("object {object} prints its bodies apart: {why}"),
        "Apart, each body prints its own walls and the overlap twice. Bodies whose colours all paint one way (all by position, or all by regions) print as one.");
    (issue.lowest_z, issue.highest_z) = (bounds.min[2], bounds.max[2]);
    issue
}

impl Issue {
    pub(crate) fn new(kind: &'static str, severity: Severity, object: Option<&str>, at_mm: [f64; 3], footprint: Footprint, measure: f64, message: impl Into<String>, hint: &str) -> Self {
        Self { kind, severity, object: object.map(str::to_string), at_mm, footprint, layer: None, layer_to: None, measure, message: message.into(), hint: hint.into(), fix: None, area_mm2: 0.0, lowest_z: at_mm[2], highest_z: at_mm[2], flags: 0 }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ObjectStats {
    pub object: String,
    pub overhang_area_mm2: [f64; 6],
    pub max_height_per_nozzle: Vec<f64>,
    pub reaching_nozzles: Vec<usize>,
    pub mass_g: f64,
    pub first_layer_area_mm2: f64,
}

#[derive(Clone, Debug, Default)]
pub struct FilamentStats {
    pub filament: u32,
    pub length_mm: f64,
    pub mass_g: f64,
    pub cost: f64,
    pub flush_mass_g: f64,
    pub peak_flow_mm3_s: f64,
}

#[derive(Clone, Debug, Default)]
pub struct PlateStats {
    pub time_s: f64,
    pub filaments: Vec<FilamentStats>,
    pub mass_g: f64,
    pub cost: f64,
    pub flush_mass_g: f64,
    pub filament_changes: u32,
    pub nozzle_changes: u32,
    /// The changes where a nozzle takes another filament out.
    pub filament_swaps: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub sliced: bool,
    pub issues: Vec<Issue>,
    pub objects: Vec<ObjectStats>,
    /// The print's figures (time, grams, purge, changes), from the G-code's timeline: none on a
    /// report of the plate or of its toolpaths alone, or a settled slice's own (its sliced
    /// report, on its timeline, streams after it). A figure is never a 0 that isn't one.
    pub plate: Option<PlateStats>,
    /// One degree (0..5) per triangle, in plate part order.
    pub overhang: Vec<Vec<u8>>,
    /// The overhang angle (degrees from vertical) the degrees are of.
    pub overhang_limit: f64,
}

impl Report {
    pub fn counts(&self) -> [usize; 3] {
        let mut counts = [0; 3];
        for i in &self.issues { counts[i.severity as usize] += 1; }
        counts
    }
    pub fn sort(&mut self) {
        let mut by_severity = [Vec::new(), Vec::new(), Vec::new()];
        for i in self.issues.drain(..) { by_severity[i.severity as usize].push(i); }
        for bucket in &mut by_severity { self.issues.append(bucket); }
    }
}

#[derive(Clone, Debug)]
pub struct Filament {
    pub kind: String,
    pub printable: u64,
    pub nozzle: Option<usize>,
    pub bed_unheated: bool,
    pub bed_temperature: f64,
    pub first_bed_temperature: f64,
    pub max_flow: f64,
    pub density: f64,
    pub diameter: f64,
    pub cost_per_kg: f64,
}

impl Default for Filament {
    fn default() -> Self {
        Self { kind: "PLA".into(), printable: 3, nozzle: None, bed_unheated: false, bed_temperature: 60.0, first_bed_temperature: 60.0, max_flow: 0.0, density: 1.24, diameter: 1.75, cost_per_kg: 0.0 }
    }
}

/// Resolved print settings, with 1-based filament slots and 0-based nozzles.
#[derive(Clone, Debug)]
pub struct Materials {
    pub filaments: Vec<Filament>,
    pub hardened_nozzles: Vec<bool>,
    pub supports: bool,
    /// Face tilt from vertical, beyond which it needs support.
    pub overhang_angle_deg: f64,
    pub patch_area_mm2: f64,
    pub line_width: f64,
    pub first_layer_height: f64,
    pub max_bridge_length: f64,
    pub support_gap: f64,
}

impl Default for Materials {
    fn default() -> Self {
        Self { filaments: Vec::new(), hardened_nozzles: Vec::new(), supports: false, overhang_angle_deg: 45.0, patch_area_mm2: 20.0, line_width: 0.4, first_layer_height: 0.2, max_bridge_length: 10.0, support_gap: 0.2 }
    }
}

impl Materials {
    pub fn from_job(job: &Job) -> Self {
        let positive = |v: f64, fallback| if v > 0.0 { v } else { fallback };
        let filaments = (0..job.filaments()).map(|f| Filament {
            kind: job.fil_str("filament_type", f).to_string(),
            printable: if job.cfg.has("filament_printable") { job.fil("filament_printable", f) as u64 } else { 3 },
            nozzle: Some(job.ext_of(f)),
            bed_unheated: job.cfg.bool_at("fab_bed_unheated", f),
            bed_temperature: job.bed_temp(f, false), first_bed_temperature: job.bed_temp(f, true),
            max_flow: job.fil("filament_max_volumetric_speed", f),
            density: job.fil("filament_density", f), diameter: job.fil("filament_diameter", f),
            cost_per_kg: job.fil("filament_cost", f),
        }).collect();
        Self {
            filaments,
            hardened_nozzles: (0..job.extruders()).map(|n| job.cfg.raw("nozzle_type", n).to_lowercase().contains("hardened")).collect(),
            supports: job.b("enable_support"),
            // Profiles express the threshold from the horizontal bed.
            overhang_angle_deg: 90.0 - positive(job.f("support_threshold_angle"), 45.0).clamp(1.0, 89.0),
            line_width: positive(job.cfg.abs_or_pct("line_width", 0, job.nozzle_diameter(0)), job.nozzle_diameter(0) * 1.05),
            first_layer_height: positive(job.f("initial_layer_print_height"), 0.2),
            max_bridge_length: positive(job.f("max_bridge_length"), 10.0),
            support_gap: job.f("support_top_z_distance").max(0.0),
            ..Default::default()
        }
    }
    /// These materials as a part with its own support settings sees them.
    pub fn of_part(&self, part: &fab_plate::Part) -> std::borrow::Cow<'_, Materials> {
        let own = |key: &str| part.print.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
        if own("enable_support").is_none() && own("support_threshold_angle").is_none() && own("support_top_z_distance").is_none() {
            return std::borrow::Cow::Borrowed(self);
        }
        let mut m = self.clone();
        if let Some(v) = own("enable_support") {
            m.supports = matches!(v, "1" | "true");
        }
        if let Some(a) = own("support_threshold_angle").and_then(|v| v.parse::<f64>().ok()).filter(|a| *a > 0.0) {
            m.overhang_angle_deg = 90.0 - a.clamp(1.0, 89.0);
        }
        if let Some(g) = own("support_top_z_distance").and_then(|v| v.parse::<f64>().ok()) {
            m.support_gap = g.max(0.0);
        }
        std::borrow::Cow::Owned(m)
    }

    pub fn filament(&self, slot: u32) -> Filament {
        self.filaments.get(slot.saturating_sub(1) as usize).cloned().unwrap_or_default()
    }

    /// The filaments (1-based slots) of one plate that cannot share its bed:
    /// the bed runs at the hottest one's temperature, and a filament whose own
    /// bed temperature is more than [`BED_SPREAD`] below that (PLA on an ABS
    /// bed) softens and fails. `None` when they all fit. The message says
    /// which filaments to print as separate plates.
    pub fn bed_conflict(&self, slots: &[u32]) -> Option<String> {
        let mut used: Vec<(u32, Filament)> = slots.iter().map(|&s| (s, self.filament(s))).collect();
        // With zero heat requested, only explicitly unheated filaments suit the bed.
        used.retain(|(_, f)| !filament_suits_plate(f.bed_unheated, 0.0));
        used.sort_by_key(|u| u.0);
        used.dedup_by_key(|u| u.0);
        for first in [true, false] {
            let t = |f: &Filament| if first { f.first_bed_temperature } else { f.bed_temperature };
            let Some((hs, hot)) = used.iter().max_by(|a, b| t(&a.1).total_cmp(&t(&b.1))) else { return None };
            let cold: Vec<&(u32, Filament)> = used.iter().filter(|(_, f)| t(hot) - t(f) > BED_SPREAD).collect();
            if cold.is_empty() {
                continue;
            }
            let names = |v: &[&(u32, Filament)]| v.iter().map(|(s, f)| format!("filament {s} ({}, {:.0} °C)", f.kind, t(f))).collect::<Vec<_>>().join(", ");
            let warm: Vec<&(u32, Filament)> = used.iter().filter(|(_, f)| t(hot) - t(f) <= BED_SPREAD).collect();
            return Some(format!(
                "{} cannot share the bed with filament {hs} ({}): the bed would run at {:.0} °C{}. Print them as separate plates: one with {}, one with {}.",
                names(&cold), hot.kind, t(hot), if first { " for the first layer" } else { "" }, names(&warm), names(&cold)
            ));
        }
        None
    }
}

/// How far (°C) a filament's own bed temperature may sit below the plate's.
pub const BED_SPREAD: f64 = 15.0;

pub const OVERHANG_COLORS: [[f32; 4]; 6] = [
    [0.42, 0.62, 0.48, 1.0], [0.68, 0.80, 0.20, 1.0],
    [0.90, 0.78, 0.08, 1.0], [1.0, 0.48, 0.04, 1.0],
    [0.95, 0.20, 0.04, 1.0], [0.70, 0.02, 0.12, 1.0],
];

#[cfg(test)]
mod bed_tests {
    use super::*;
    use fab_plate::{machine::Machine, Part, PartMesh, Plate};
    use makepad_fab_settings::{Config, machine::PLATES};

    #[test]
    fn unheated_filaments_pass_part_checks_and_do_not_conflict_with_heat() {
        let mut cfg = Config::new();
        cfg.set_vec("filament_map", vec!["1".into(), "1".into()]);
        cfg.set_vec("filament_type", vec!["PLA".into(), "PLA".into()]);
        let part = |slot| Part { body_of: None, print: Vec::new(), print_mesh: None, paint: None, object: format!("part{slot}"), name: "box".into(), filament: slot, color: [1.0; 4], nozzle: None,
            mesh: PartMesh { positions: vec![[100.0, 100.0, 0.0], [101.0, 101.0, 1.0]], triangles: Vec::new() }, field_bounds: None, field_floor: None, ..Default::default() };
        let plate = Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part(1), part(2)] };
        for (plate_name, key) in PLATES {
            cfg.set_str("curr_bed_type", *plate_name);
            cfg.set_vec(key, vec!["0".into(), "60".into()]);
            cfg.set_vec(&format!("{key}_initial_layer"), vec!["0".into(), "65".into()]);
            for unheated in [false, true] {
                cfg.set_vec("fab_bed_unheated", vec![if unheated { "1" } else { "0" }.into(), "0".into()]);
                let job = Job::new(cfg.clone());
                let materials = Materials::from_job(&job);
                assert_eq!(materials.bed_conflict(&[1, 2]).is_none(), unheated, "{plate_name}");
                let report = combine_plate(&Machine::dual_nozzle_350(), &plate, &materials, std::iter::empty());
                assert_eq!(report.issues.iter().any(|i| i.kind == "bed_temperature"), !unheated, "{plate_name}: {report:?}");
                assert_eq!(report.issues.iter().any(|i| i.kind == "bed_temperature_mix"), !unheated, "{plate_name}");
            }
            // The explicit mark asks for no heat even if a preset also carries a temperature.
            cfg.set_vec(key, vec!["40".into(), "60".into()]);
            let job = Job::new(cfg.clone());
            assert_eq!(job.bed_temp(0, false), 0.0);
            assert_eq!(job.bed_temp(1, false), 60.0);
        }
    }
}
