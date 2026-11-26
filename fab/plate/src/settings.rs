//! The slicer settings a person, the AI and a project can set: one table the panel's rows,
//! the AI's keys and the project's check all come from. A key is listed only when the slicer
//! really reads it. The plate has one global set; an object overrides any setting of
//! [`Scope::Object`] for itself (`place(..., settings: {...})`). A setting of [`Scope::Plate`]
//! is the plate's alone, and says why.

/// How a setting is edited.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SettingKind {
    /// A switch: "1" on, "0" off.
    Bool,
    /// A number in [min, max] with a step and a unit suffix. A "%" unit writes the value
    /// with its sign ("15%"), as the slicer reads it.
    Number { min: f64, max: f64, step: f64, unit: &'static str },
    /// One of the listed (value, label).
    Choice(&'static [(&'static str, &'static str)]),
}

/// The panel's groups, in its order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SettingGroup {
    Quality,
    Walls,
    Infill,
    TopBottom,
    Support,
    Speed,
    Material,
}

impl SettingGroup {
    pub const ALL: [SettingGroup; 7] = [SettingGroup::Quality, SettingGroup::Walls, SettingGroup::Infill, SettingGroup::TopBottom, SettingGroup::Support, SettingGroup::Speed, SettingGroup::Material];
    pub fn title(self) -> &'static str {
        match self {
            SettingGroup::Quality => "Quality",
            SettingGroup::Walls => "Walls",
            SettingGroup::Infill => "Infill",
            SettingGroup::TopBottom => "Top and bottom",
            SettingGroup::Support => "Support and brim",
            SettingGroup::Speed => "Speed",
            SettingGroup::Material => "Material",
        }
    }
    /// What the group cannot set, and where that lives instead.
    pub fn note(self) -> &'static str {
        match self {
            SettingGroup::Material => "Temperature, cooling and retraction belong to the material: each print colour prints the material in its tray, with that material's settings (the Colours list in the Job panel). One nozzle holds one temperature at a time, so two objects at two temperatures need colours on two nozzles.",
            _ => "",
        }
    }
}

/// The settings a region of a part can set for itself (`m.settings(region, {...})`), in the
/// values `place(..., settings: {...})` takes; every other is the whole part's.
pub const REGION_SETTINGS: &[&str] = &["sparse_infill_density", "sparse_infill_pattern", "wall_loops"];

/// Where a setting can differ.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SettingScope {
    /// Any object may override it.
    Object,
    /// The plate's alone; why, in words the panel shows.
    Plate(&'static str),
}

/// One slicer setting.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PrintSetting {
    pub group: SettingGroup,
    /// The sub-heading it sits under ("" for the group's plain rows); a mode's rows share one.
    pub mode: &'static str,
    pub key: &'static str,
    pub label: &'static str,
    /// One short line: what it does.
    pub hint: &'static str,
    pub kind: SettingKind,
    /// Choices shown first in the dropdown, in order, with a note on when to use each.
    pub common: &'static [(&'static str, &'static str)],
    /// The value when neither the profile nor the project has one.
    pub default: &'static str,
    /// Folded away until asked for.
    pub advanced: bool,
    pub scope: SettingScope,
    /// Shown only while this setting is on (a mode's parameters under its switch); under the
    /// support type, while the chosen type reads it ([`SUPPORT_READS`], [`needs_met`]).
    pub needs: &'static str,
}

const fn num(min: f64, max: f64, step: f64, unit: &'static str) -> SettingKind {
    SettingKind::Number { min, max, step, unit }
}

const SPARSE: &[(&str, &str)] = &[
    ("grid", "Grid"), ("cubic", "Cubic"), ("gyroid", "Gyroid"), ("tpmsd", "TPMS-D"), ("tpmsfk", "TPMS-FK"), ("zig-zag", "Rectilinear"), ("alignedrectilinear", "Aligned rectilinear"), ("line", "Line"),
    ("zigzag", "Zig zag"), ("crosszag", "Cross zag"), ("lockedzag", "Locked zag"), ("crosshatch", "Crosshatch"), ("2dlattice", "2D lattice"), ("triangles", "Triangles"),
    ("tri-hexagon", "Tri-hexagon"), ("adaptivecubic", "Adaptive cubic"), ("supportcubic", "Support cubic"), ("honeycomb", "Honeycomb"), ("3dhoneycomb", "3D honeycomb"),
    ("lightning", "Lightning"), ("hilbertcurve", "Hilbert curve"), ("archimedeanchords", "Archimedean chords"), ("octagramspiral", "Octagram spiral"), ("concentric", "Concentric"),
];
const COMMON_SPARSE: &[(&str, &str)] = &[
    ("gyroid", "strong in every direction, the all-rounder"),
    ("cubic", "strong and quick"),
    ("grid", "simple and fast"),
    ("lightning", "least plastic, only holds up the top"),
    ("adaptivecubic", "saves plastic in big parts"),
    ("zig-zag", "fastest"),
];
const SURFACE: &[(&str, &str)] = &[
    ("monotonic", "Monotonic"), ("monotonicline", "Monotonic line"), ("zig-zag", "Rectilinear"), ("alignedrectilinear", "Aligned rectilinear"), ("concentric", "Concentric"),
    ("hilbertcurve", "Hilbert curve"), ("archimedeanchords", "Archimedean chords"), ("octagramspiral", "Octagram spiral"),
];
const TOP_SURFACE: &[(&str, &str)] = &[
    ("monotonic", "Monotonic"), ("monotonicline", "Monotonic line"), ("globalmonotonicline", "Global monotonic line"), ("zig-zag", "Rectilinear"), ("alignedrectilinear", "Aligned rectilinear"),
    ("concentric", "Concentric"), ("hilbertcurve", "Hilbert curve"), ("archimedeanchords", "Archimedean chords"), ("octagramspiral", "Octagram spiral"),
];

const fn row(group: SettingGroup, key: &'static str, label: &'static str, hint: &'static str, kind: SettingKind, default: &'static str) -> PrintSetting {
    PrintSetting { group, mode: "", key, label, hint, kind, common: &[], default, advanced: false, scope: SettingScope::Object, needs: "" }
}
const fn adv(group: SettingGroup, key: &'static str, label: &'static str, hint: &'static str, kind: SettingKind, default: &'static str) -> PrintSetting {
    PrintSetting { group, mode: "", key, label, hint, kind, common: &[], default, advanced: true, scope: SettingScope::Object, needs: "" }
}
/// A row of a mode: under `mode`, shown while `needs` is on ("" for the mode's own switch).
const fn mode(group: SettingGroup, mode: &'static str, needs: &'static str, key: &'static str, label: &'static str, hint: &'static str, kind: SettingKind, default: &'static str) -> PrintSetting {
    PrintSetting { group, mode, key, label, hint, kind, common: &[], default, advanced: true, scope: SettingScope::Object, needs }
}
const fn plate(group: SettingGroup, key: &'static str, label: &'static str, hint: &'static str, kind: SettingKind, default: &'static str, why: &'static str) -> PrintSetting {
    PrintSetting { group, mode: "", key, label, hint, kind, common: &[], default, advanced: false, scope: SettingScope::Plate(why), needs: "" }
}

use SettingGroup::{Infill, Material, Quality, Speed, Support, TopBottom, Walls};

/// Every setting, in the panel's order: common rows first within a group.
pub const PRINT_SETTINGS: &[PrintSetting] = &[
    // ---- Quality ----
    row(Quality, "layer_height", "Layer height", "Thinner layers look smoother and print slower", num(0.04, 0.6, 0.01, "mm"), "0.2"),
    plate(Quality, "initial_layer_print_height", "First layer height", "Thickness of the first layer on the bed", num(0.08, 0.6, 0.01, "mm"), "0.2", "Every object starts together on one first layer: the bed pass has one height."),
    adv(Quality, "line_width", "Line width (0 = nozzle)", "Default width of a printed line", num(0.0, 2.0, 0.01, "mm"), "0.4"),
    adv(Quality, "initial_layer_line_width", "First layer line width", "Wider lines stick better to the bed", num(0.0, 2.0, 0.01, "mm"), "0.4"),
    adv(Quality, "elefant_foot_compensation", "Elephant foot", "Pulls the first layer in so it does not flare", num(0.0, 1.0, 0.01, "mm"), "0"),
    mode(Quality, "Adaptive layers", "", "fab_adaptive_layers", "Adaptive layers", "Thin layers on slopes, thick on straight walls", SettingKind::Bool, "0"),
    mode(Quality, "Adaptive layers", "fab_adaptive_layers", "fab_adaptive_cusp", "Max step (0 = auto)", "The largest stair step left on a slope", num(0.0, 0.2, 0.01, "mm"), "0"),
    mode(Quality, "Spiral vase", "", "spiral_mode", "Spiral vase", "One continuous climbing wall, no top and no infill", SettingKind::Bool, "0"),
    mode(Quality, "Spiral vase", "spiral_mode", "fab_vase_variant", "Variant", "Single wall, double wall or a wall of changing width", SettingKind::Choice(&[("plain", "Plain"), ("double_wall", "Double wall"), ("variable_width", "Variable width")]), "plain"),
    mode(Quality, "Spiral vase", "spiral_mode", "fab_vase_width_max", "Max width", "Widest line of a variable-width vase", num(0.3, 1.2, 0.05, "mm"), "0.8"),
    mode(Quality, "Spiral vase", "spiral_mode", "fab_vase_rib_spacing", "Rib spacing", "Distance between the ribs of a double wall", num(1.0, 50.0, 0.5, "mm"), "6"),
    // ---- Walls ----
    row(Walls, "wall_loops", "Wall loops", "How many walls around the outline: more is stronger", num(0.0, 20.0, 1.0, ""), "2"),
    row(Walls, "top_one_wall_type", "One wall on top", "A single wall on top surfaces leaves more room for the top fill", SettingKind::Choice(&[("not apply", "Off"), ("all top", "All top"), ("topmost", "Topmost")]), "all top"),
    adv(Walls, "outer_wall_line_width", "Outer line width (0 = default)", "Width of the visible wall", num(0.0, 2.0, 0.01, "mm"), "0"),
    adv(Walls, "inner_wall_line_width", "Inner line width (0 = default)", "Width of the walls inside", num(0.0, 2.0, 0.01, "mm"), "0.4"),
    adv(Walls, "seam_gap", "Seam gap", "How far a loop stops short of its start", num(0.0, 1.0, 0.01, "mm"), "0.06"),
    adv(Walls, "detect_overhang_wall", "Detect overhangs", "Slows the walls that hang over air", SettingKind::Bool, "1"),
    adv(Walls, "ensure_vertical_shell_thickness", "Vertical shell thickness", "Adds solid fill where a sloped wall gets thin", SettingKind::Choice(&[("enabled", "On"), ("partial", "Partial"), ("disabled", "Off")]), "enabled"),
    adv(Walls, "detect_floating_vertical_shell", "Floating shells", "Prints shells over internal bridges slowly", SettingKind::Bool, "1"),
    mode(Walls, "Variable-width walls", "", "wall_generator", "Variable-width walls", "Beads narrow and widen to fill the part: thin features print as one bead", SettingKind::Choice(&[("classic", "Off"), ("arachne", "On")]), "classic"),
    mode(Walls, "Variable-width walls", "wall_generator", "min_bead_width", "Minimum bead width", "The narrowest bead, as a share of the nozzle: a thin feature prints at least this wide", num(5.0, 100.0, 1.0, "%"), "85%"),
    mode(Walls, "Variable-width walls", "wall_generator", "min_feature_size", "Minimum feature size", "Features thinner than this share of the nozzle are left out", num(5.0, 100.0, 1.0, "%"), "25%"),
    mode(Walls, "Variable-width walls", "wall_generator", "wall_transition_length", "Transition length", "Room for the walls to split or join where the bead count changes, as a share of the nozzle", num(1.0, 500.0, 1.0, "%"), "100%"),
    mode(Walls, "Variable-width walls", "wall_generator", "wall_transition_filter_deviation", "Transition margin", "Keeps the bead count from switching back and forth, as a share of the nozzle", num(0.0, 100.0, 1.0, "%"), "25%"),
    mode(Walls, "Variable-width walls", "wall_generator", "wall_transition_angle", "Transition angle", "Wedges sharper than this keep their walls and get no bead in the middle", num(1.0, 59.0, 1.0, "°"), "10"),
    mode(Walls, "Variable-width walls", "wall_generator", "wall_distribution_count", "Distribution count", "How many walls, counted from the middle, share a change in width", num(1.0, 20.0, 1.0, ""), "1"),
    mode(Walls, "Wall texture", "", "fab_wall_texture", "Texture", "A pattern pressed into the outer wall", SettingKind::Choice(&[("none", "None"), ("fuzzy", "Fuzzy"), ("knurl", "Knurl"), ("scales", "Scales"), ("woven", "Woven"), ("wood_grain", "Wood grain")]), "none"),
    mode(Walls, "Wall texture", "fab_wall_texture", "fuzzy_skin_thickness", "Depth", "How deep the texture goes", num(0.05, 1.5, 0.05, "mm"), "0.3"),
    mode(Walls, "Wall texture", "fab_wall_texture", "fab_wall_texture_scale", "Feature size", "Size of one texture feature", num(0.4, 20.0, 0.1, "mm"), "2"),
    mode(Walls, "Wall texture", "fab_wall_texture", "fab_wall_texture_holes", "Texture holes", "Texture the walls of holes as well", SettingKind::Bool, "0"),
    mode(Walls, "Scarf seams", "", "seam_slope_type", "Scarf seams", "Fades a seam in on a slope, so it hardly shows", SettingKind::Choice(&[("none", "Off"), ("external", "Outer walls"), ("all", "All walls")]), "none"),
    mode(Walls, "Scarf seams", "seam_slope_type", "seam_slope_min_length", "Scarf length", "Length of the fade", num(4.0, 40.0, 1.0, "mm"), "10"),
    mode(Walls, "Arc overhangs", "", "fab_arc_overhangs", "Arc overhangs", "Prints steep overhangs as growing arcs, without support", SettingKind::Bool, "0"),
    mode(Walls, "Arc overhangs", "fab_arc_overhangs", "fab_arc_overhang_speed", "Arc speed", "Speed of the arcs", num(1.0, 15.0, 0.5, "mm/s"), "5"),
    // ---- Infill ----
    row(Infill, "sparse_infill_density", "Density", "How full the inside is: more is stronger, slower and heavier", num(0.0, 100.0, 1.0, "%"), "20%"),
    PrintSetting { common: COMMON_SPARSE, ..row(Infill, "sparse_infill_pattern", "Pattern", "The shape of the fill inside", SettingKind::Choice(SPARSE), "gyroid") },
    // Adaptive density: under the pattern while it is one the grading draws ([`GRADED_PATTERNS`]).
    PrintSetting { needs: SPARSE_PATTERN, advanced: false, ..mode(Infill, "Adaptive density", "", "adaptive_infill_mode", "Adaptive density", "Fills the core more thinly than near the surfaces: less plastic, the walls keep their support. Distance grades each body to its deepest point; Lobes grades each lobe, joined by a narrower neck, to its own", SettingKind::Choice(&[("disabled", "Off"), ("distance_warp", "Distance"), ("lobes", "Lobes")]), "disabled") },
    PrintSetting { advanced: false, ..mode(Infill, "Adaptive density", "adaptive_infill_mode", "adaptive_infill_interior_density", "Core density", "How full the deepest part is; the density above is the one near the surfaces", num(1.0, 100.0, 1.0, "%"), "10%") },
    PrintSetting { advanced: false, ..mode(Infill, "Adaptive density", "adaptive_infill_mode", "adaptive_infill_gradient", "Gradient", "How the density changes from the surfaces to the core: quadratic keeps it full deeper, exponential thins it sooner", SettingKind::Choice(&[("linear", "Linear"), ("quadratic", "Quadratic"), ("exponential", "Exponential")]), "linear") },
    row(Infill, "infill_direction", "Direction", "Angle of the fill lines", num(0.0, 360.0, 1.0, "°"), "45"),
    adv(Infill, "infill_wall_overlap", "Wall overlap", "How far the fill reaches into the walls", num(0.0, 100.0, 1.0, "%"), "15%"),
    adv(Infill, "sparse_infill_line_width", "Line width (0 = default)", "Width of the infill lines", num(0.0, 2.0, 0.01, "mm"), "0.4"),
    adv(Infill, "internal_solid_infill_pattern", "Solid fill pattern", "The pattern of solid fill inside the part", SettingKind::Choice(SURFACE), "zig-zag"),
    adv(Infill, "internal_solid_infill_line_width", "Solid line width (0 = default)", "Width of the solid fill lines", num(0.0, 2.0, 0.01, "mm"), "0.4"),
    adv(Infill, "minimum_sparse_infill_area", "Minimum area", "Smaller infill regions print solid", num(0.0, 200.0, 1.0, "mm²"), "15"),
    adv(Infill, "detect_narrow_internal_solid_infill", "Narrow solid as lines", "Fills narrow solid regions concentrically", SettingKind::Bool, "1"),
    adv(Infill, "infill_shift_step", "Shift step", "Moves the fill a little each layer (zig-zag patterns)", num(0.0, 10.0, 0.05, "mm"), "0.4"),
    adv(Infill, "infill_rotate_step", "Rotate step", "Turns the fill a little each layer (zig-zag patterns)", num(0.0, 180.0, 1.0, "°"), "0"),
    adv(Infill, "bridge_angle", "Bridge angle (0 = auto)", "Direction of bridge lines", num(0.0, 360.0, 1.0, "°"), "0"),
    adv(Infill, "bridge_flow", "Bridge flow", "Plastic per bridge line, as a ratio", num(0.1, 2.0, 0.01, ""), "1"),
    adv(Infill, "thick_bridges", "Thick bridges", "Round bridge lines: stronger over long gaps", SettingKind::Bool, "0"),
    // ---- Top and bottom ----
    row(TopBottom, "top_shell_layers", "Top layers", "Solid layers on top: more closes the top better", num(0.0, 30.0, 1.0, ""), "4"),
    row(TopBottom, "bottom_shell_layers", "Bottom layers", "Solid layers underneath", num(0.0, 30.0, 1.0, ""), "3"),
    row(TopBottom, "top_surface_pattern", "Top pattern", "The pattern of the visible top", SettingKind::Choice(TOP_SURFACE), "monotonic"),
    row(TopBottom, "bottom_surface_pattern", "Bottom pattern", "The pattern of the first layers", SettingKind::Choice(SURFACE), "monotonic"),
    adv(TopBottom, "top_shell_thickness", "Minimum top thickness", "Adds top layers until they reach this", num(0.0, 10.0, 0.1, "mm"), "0.6"),
    adv(TopBottom, "bottom_shell_thickness", "Minimum bottom thickness", "Adds bottom layers until they reach this", num(0.0, 10.0, 0.1, "mm"), "0"),
    adv(TopBottom, "top_surface_line_width", "Top line width (0 = default)", "Width of the top lines", num(0.0, 2.0, 0.01, "mm"), "0.4"),
    adv(TopBottom, "monotonic_travel_into_wall", "Top lines into wall", "How far monotonic top lines run on into the wall", num(0.0, 100.0, 1.0, "%"), "0%"),
    mode(TopBottom, "Ironing", "", "ironing_type", "Ironing", "Runs the hot nozzle over the top to smooth it", SettingKind::Choice(&[("no ironing", "Off"), ("top", "All tops"), ("topmost", "Topmost"), ("solid", "All solid")]), "no ironing"),
    mode(TopBottom, "Ironing", "ironing_type", "ironing_flow", "Ironing flow", "Plastic pushed out while ironing", num(1.0, 30.0, 1.0, "%"), "10%"),
    mode(TopBottom, "Ironing", "ironing_type", "ironing_spacing", "Ironing spacing", "Distance between ironing lines", num(0.05, 0.4, 0.01, "mm"), "0.1"),
    mode(TopBottom, "Ironing", "ironing_type", "ironing_speed", "Ironing speed", "Speed of the ironing pass", num(5.0, 100.0, 1.0, "mm/s"), "20"),
    mode(TopBottom, "Non-planar top", "", "fab_nonplanar_top", "Non-planar top", "Follows gentle top curves instead of stair-stepping them", SettingKind::Bool, "0"),
    mode(TopBottom, "Non-planar top", "fab_nonplanar_top", "fab_nonplanar_shells", "Shells", "Curved layers on top", num(1.0, 3.0, 1.0, ""), "2"),
    mode(TopBottom, "Non-planar top", "fab_nonplanar_top", "fab_nonplanar_slope_cap", "Max slope", "The steepest curve it follows", num(2.0, 20.0, 1.0, "°"), "15"),
    mode(TopBottom, "Non-planar top", "fab_nonplanar_top", "fab_nonplanar_rise", "Max rise", "The tallest curve it follows", num(0.5, 6.0, 0.5, "mm"), "3"),
    // ---- Support and brim ----
    row(Support, "enable_support", "Supports", "Builds removable support under overhangs", SettingKind::Bool, "0"),
    PrintSetting { needs: "enable_support", ..row(Support, "support_type", "Type", "Tree supports use less plastic; normal ones hold flat overhangs better", SettingKind::Choice(&[("tree(auto)", "Tree"), ("normal(auto)", "Normal"), ("coral(auto)", "Coral (experimental)")]), "tree(auto)") },
    PrintSetting { needs: "support_type", ..adv(Support, "support_style", "Style", "How the support branches or walls are shaped", SettingKind::Choice(&[("default", "Default"), ("grid", "Grid"), ("snug", "Snug"), ("tree_slim", "Slim tree"), ("tree_strong", "Strong tree"), ("tree_hybrid", "Hybrid tree"), ("tree_organic", "Organic tree")]), "default") },
    PrintSetting { needs: "support_type", ..row(Support, "support_threshold_angle", "Overhang angle", "Overhangs flatter than this get support", num(1.0, 89.0, 1.0, "°"), "30") },
    PrintSetting { needs: "support_type", ..row(Support, "support_on_build_plate_only", "From the bed only", "No support standing on the part itself", SettingKind::Bool, "0") },
    PrintSetting { needs: "support_type", ..row(Support, "fab_tree_outside_footprint", "Only outside the part's footprint", "Trees keep out of what the part covers below them, so none is locked in it; what overhangs there prints unsupported", SettingKind::Bool, "0") },
    PrintSetting { needs: "support_type", ..row(Support, "fab_support_inside", "Allow supports that can't be removed", "Supports may stand where they'd be locked in the part, in hollows and cells with no way out as wide as a column: they hold there, but stay in once printed", SettingKind::Bool, "0") },
    row(Support, "brim_type", "Brim", "The part's own flat rim on the first layer, not the support's: holds tall or small parts down", SettingKind::Choice(&[("auto_brim", "Auto"), ("no_brim", "Off"), ("outer_only", "Outer"), ("inner_only", "Inner"), ("outer_and_inner", "Both")]), "auto_brim"),
    PrintSetting { needs: "brim_type", ..adv(Support, "brim_width", "Brim width", "Width of the part's rim, not the support's", num(0.0, 50.0, 0.5, "mm"), "5") },
    PrintSetting { needs: "brim_type", ..adv(Support, "brim_object_gap", "Brim gap", "Air between the part and its rim, not the support's: more peels off easier", num(0.0, 2.0, 0.01, "mm"), "0.1") },
    PrintSetting { needs: "support_type", ..adv(Support, "support_top_z_distance", "Top gap", "Air between the support and the part above it", num(0.0, 1.0, 0.01, "mm"), "0.2") },
    PrintSetting { needs: "support_type", ..adv(Support, "support_bottom_z_distance", "Bottom gap", "Air between the support and the part below it", num(0.0, 1.0, 0.01, "mm"), "0.2") },
    PrintSetting { needs: "support_type", ..adv(Support, "support_object_xy_distance", "Side gap", "Air between the support and the part beside it", num(0.0, 5.0, 0.01, "mm"), "0.35") },
    PrintSetting { needs: "support_type", ..adv(Support, "support_object_first_layer_gap", "First layer gap", "Air between the support's first layer and the part", num(0.0, 5.0, 0.01, "mm"), "0.2") },
    PrintSetting { needs: "support_type", ..adv(Support, "support_interface_top_layers", "Interface layers", "Dense layers right under the part", num(0.0, 10.0, 1.0, ""), "3") },
    PrintSetting { needs: "support_type", ..adv(Support, "support_interface_spacing", "Interface spacing", "Distance between interface lines", num(0.0, 5.0, 0.05, "mm"), "0.5") },
    PrintSetting { needs: "support_type", ..adv(Support, "support_base_pattern_spacing", "Base spacing", "Distance between support lines", num(0.0, 10.0, 0.1, "mm"), "2.5") },
    PrintSetting { needs: "support_type", ..adv(Support, "support_remove_small_overhang", "Skip small overhangs", "No support under very small overhangs", SettingKind::Bool, "1") },
    PrintSetting { needs: "support_type", ..adv(Support, "support_line_width", "Line width (0 = default)", "Width of the support lines", num(0.0, 2.0, 0.01, "mm"), "0.4") },
    PrintSetting { needs: "support_type", ..adv(Support, "tree_support_branch_diameter_organic", "Tree branch diameter", "Thickness of a tree branch", num(0.5, 20.0, 0.1, "mm"), "2") },
    PrintSetting { needs: "support_type", ..adv(Support, "tree_support_branch_angle_organic", "Tree branch angle", "How far a branch leans", num(1.0, 85.0, 1.0, "°"), "40") },
    PrintSetting { needs: "support_type", ..adv(Support, "tree_support_branch_diameter_angle", "Tree branch taper", "How fast a branch thickens going down", num(0.0, 15.0, 0.5, "°"), "5") },
    PrintSetting { needs: "support_type", ..adv(Support, "support_expansion", "Normal support expansion", "Grows normal support outward", num(0.0, 10.0, 0.1, "mm"), "0") },
    PrintSetting { needs: "support_type", ..adv(Support, "support_angle", "Normal support angle", "Direction of normal support lines", num(0.0, 360.0, 1.0, "°"), "0") },
    // ---- Speed ----
    row(Speed, "outer_wall_speed", "Outer wall", "Slower looks better on the visible wall", num(1.0, 1000.0, 1.0, "mm/s"), "60"),
    row(Speed, "inner_wall_speed", "Inner wall", "Walls that do not show", num(1.0, 1000.0, 1.0, "mm/s"), "60"),
    row(Speed, "sparse_infill_speed", "Infill", "The fill inside", num(1.0, 1000.0, 1.0, "mm/s"), "100"),
    row(Speed, "top_surface_speed", "Top surface", "The visible top", num(1.0, 1000.0, 1.0, "mm/s"), "100"),
    row(Speed, "initial_layer_speed", "First layer", "Slow helps the first layer stick", num(1.0, 500.0, 1.0, "mm/s"), "30"),
    adv(Speed, "internal_solid_infill_speed", "Solid fill", "Solid fill inside the part", num(1.0, 1000.0, 1.0, "mm/s"), "100"),
    adv(Speed, "initial_layer_infill_speed", "First layer fill", "Fill on the first layer", num(1.0, 500.0, 1.0, "mm/s"), "60"),
    adv(Speed, "bridge_speed", "Bridge", "Lines across air", num(1.0, 500.0, 1.0, "mm/s"), "25"),
    adv(Speed, "gap_infill_speed", "Gap fill", "Small gaps between walls", num(1.0, 500.0, 1.0, "mm/s"), "30"),
    adv(Speed, "support_speed", "Support", "The support body", num(1.0, 1000.0, 1.0, "mm/s"), "80"),
    adv(Speed, "support_interface_speed", "Support interface", "The dense layers under the part", num(1.0, 500.0, 1.0, "mm/s"), "80"),
    adv(Speed, "enable_overhang_speed", "Slow overhangs", "Slows down over air", SettingKind::Bool, "1"),
    adv(Speed, "default_acceleration", "Acceleration", "How fast the head speeds up, where nothing below sets it", num(100.0, 50000.0, 100.0, "mm/s²"), "500"),
    adv(Speed, "outer_wall_acceleration", "Outer wall acceleration", "Lower gives cleaner visible walls", num(100.0, 50000.0, 100.0, "mm/s²"), "500"),
    adv(Speed, "inner_wall_acceleration", "Inner wall acceleration (0 = default)", "Walls that do not show", num(0.0, 50000.0, 100.0, "mm/s²"), "0"),
    adv(Speed, "sparse_infill_acceleration", "Infill acceleration", "The fill inside", num(100.0, 50000.0, 100.0, "mm/s²"), "500"),
    adv(Speed, "top_surface_acceleration", "Top acceleration", "The visible top", num(100.0, 50000.0, 100.0, "mm/s²"), "500"),
    // ---- Material ----
    row(Material, "filament_max_volumetric_speed", "Max flow", "The most plastic the hot end melts: caps every speed", num(1.0, 100.0, 0.5, "mm³/s"), "2"),
    plate(Material, "enable_prime_tower", "Wipe tower", "Primes each colour change on a tower beside the parts. On by default only when a nozzle has to swap colours; with one colour per nozzle it is off. Off saves plastic and time, but colours can bleed and blobs can show where a new colour starts", SettingKind::Bool, "0", "The plate has one tower: the colour changes of all its objects prime on it"),
    mode(Material, "Material interlock", "", "interlocking_beam", "Interlock materials", "Teeth where two filaments meet, for a stronger bond", SettingKind::Bool, "0"),
    mode(Material, "Material interlock", "interlocking_beam", "interlocking_beam_width", "Beam width", "Width of a tooth", num(0.4, 3.0, 0.1, "mm"), "0.8"),
    mode(Material, "Material interlock", "interlocking_beam", "interlocking_depth", "Depth", "How deep the teeth reach", num(1.0, 6.0, 1.0, "beams"), "2"),
    adv(Material, "fab_paint_skin_depth", "Paint skin", "How deep under the surface a painted pattern prints exactly; deeper, where nothing shows, it prints in large blocks of one colour, in fewer pieces", num(0.0, 10.0, 0.1, "mm"), "0.5"),
    adv(Material, "fab_paint_inside_exact", "Exact paint inside", "Prints a painted pattern exactly all through the part too, for a part to be cut open: slower, in many more pieces", SettingKind::Bool, "0"),
    adv(Material, "fab_paint_first_layer", "Paint the first layer", "Prints the paint on the first layer too, as a picture printed face down needs; off, the first layer prints in the part's own colour", SettingKind::Bool, "0"),
    adv(Infill, "fab_solid_on_grid", "Solid lines on a grid", "Lays every solid layer's lines on one grid through the part's middle, one spacing apart, as a picture's cells need; off, they spread to fill each area evenly", SettingKind::Bool, "0"),
];

/// The table's row of a key.
pub fn print_setting(key: &str) -> Option<&'static PrintSetting> {
    PRINT_SETTINGS.iter().find(|s| s.key == key)
}

/// The part's own setting its placement's support marks write (the tips of the trees the user
/// deleted on it, which the tree planner reads): no setting of the table, never shown, set or
/// copied as one.
pub const TREE_DROPS: &str = "fab_tree_dropped";

/// The settings a part was given that the user sets: all but [`TREE_DROPS`].
pub fn own_settings(print: &[(String, String)]) -> impl Iterator<Item = &(String, String)> {
    print.iter().filter(|(k, _)| k != TREE_DROPS)
}

/// May an object set this key for itself?
pub fn is_part_print_key(key: &str) -> bool {
    print_setting(key).is_some_and(|s| s.scope == SettingScope::Object)
}

/// The number a value says ("15%" is 15).
pub fn setting_number(value: &str) -> Option<f64> {
    value.trim().trim_end_matches('%').parse().ok().filter(|v: &f64| v.is_finite())
}

/// A value this setting takes, as the slicer reads it; otherwise what is wrong, in words that
/// list what it does take.
pub fn checked_value(key: &str, value: &str) -> Result<String, String> {
    let Some(s) = print_setting(key) else { return Err(format!("'{key}' is not a print setting")) };
    match s.kind {
        SettingKind::Bool => match value.trim() {
            "1" | "true" => Ok("1".into()),
            "0" | "false" => Ok("0".into()),
            v => Err(format!("'{key}' is true or false, not \"{v}\"")),
        },
        SettingKind::Number { min, max, unit, .. } => {
            let Some(v) = setting_number(value) else { return Err(format!("'{key}' is a number ({min} to {max} {unit}), not \"{value}\"")) };
            if v < min || v > max {
                return Err(format!("'{key}' goes from {min} to {max} {unit}, not {v}"));
            }
            Ok(if unit == "%" { format!("{v}%") } else { format!("{v}") })
        }
        SettingKind::Choice(choices) => {
            if choices.iter().any(|(v, _)| *v == value) {
                Ok(value.into())
            } else {
                Err(format!("'{key}' is one of {}, not \"{value}\"", choices.iter().map(|(v, _)| format!("\"{v}\"")).collect::<Vec<_>>().join(", ")))
            }
        }
    }
}

/// Is a switch-like value on (a mode's first choice is its off)?
pub fn setting_on(setting: &PrintSetting, value: &str) -> bool {
    match setting.kind {
        SettingKind::Bool => matches!(value.trim(), "1" | "true"),
        SettingKind::Choice(choices) => choices.first().is_some_and(|(first, _)| *first != value),
        SettingKind::Number { .. } => setting_number(value).is_some_and(|v| v != 0.0),
    }
}

/// The support setting that picks the kind of support: the rows under it are what one kind reads.
pub const SUPPORT_TYPE: &str = "support_type";

/// The support rows each support type reads, by its value: while supports are on, a row under
/// the support type shows only while the chosen type reads it. A type not listed makes no
/// support and reads none. The trees' are what R14 is told (every one changes it, and no other
/// does); a new type lists its own.
pub const SUPPORT_READS: &[(&str, &[&str])] = &[
    ("tree(auto)", &[
        "support_threshold_angle", "support_on_build_plate_only", "fab_tree_outside_footprint", "fab_support_inside", "support_top_z_distance", "support_bottom_z_distance",
        "support_object_xy_distance", "support_object_first_layer_gap", "support_interface_top_layers", "support_interface_spacing", "support_remove_small_overhang", "support_line_width",
        "tree_support_branch_diameter_organic", "tree_support_branch_angle_organic", "tree_support_branch_diameter_angle",
    ]),
    ("normal(auto)", &[
        "support_style", "support_threshold_angle", "support_on_build_plate_only", "fab_support_inside", "support_top_z_distance", "support_bottom_z_distance",
        "support_object_xy_distance", "support_object_first_layer_gap", "support_interface_top_layers", "support_interface_spacing", "support_base_pattern_spacing", "support_line_width",
        "support_expansion", "support_angle",
    ]),
    ("coral(auto)", &[
        "support_threshold_angle", "support_on_build_plate_only", "support_top_z_distance", "support_bottom_z_distance", "support_object_xy_distance",
        "support_line_width", "tree_support_branch_angle_organic",
    ]),
];

/// Whether the support type `support_type` reads the support row `key` ([`SUPPORT_READS`]).
pub fn support_reads(support_type: &str, key: &str) -> bool {
    SUPPORT_READS.iter().any(|(t, keys)| *t == support_type && keys.contains(&key))
}

/// The setting that picks the part's rim: the rows under it are what one kind reads.
pub const BRIM_TYPE: &str = "brim_type";

/// The rim rows each kind of rim reads, by its value: Auto works its width out from the part, and
/// a kind not listed makes no rim and reads none.
pub const BRIM_READS: &[(&str, &[&str])] = &[
    ("auto_brim", &["brim_object_gap"]),
    ("outer_only", &["brim_width", "brim_object_gap"]),
    ("inner_only", &["brim_width", "brim_object_gap"]),
    ("outer_and_inner", &["brim_width", "brim_object_gap"]),
];

/// The sparse pattern: the rows under it show while it is one they apply to.
pub const SPARSE_PATTERN: &str = "sparse_infill_pattern";

/// The sparse patterns adaptive density grades (the TPMS patterns).
pub const GRADED_PATTERNS: &[&str] = &["gyroid", "tpmsd", "tpmsfk"];

/// Whether `s` shows, with `value` giving a setting's shown value: under a switch while it is on,
/// under the support type or the rim while the chosen kind reads it, under the sparse pattern
/// while it is one adaptive density grades, and only while what it is under shows.
pub fn needs_met(s: &PrintSetting, value: &dyn Fn(&PrintSetting) -> String) -> bool {
    if s.needs.is_empty() {
        return true;
    }
    let Some(parent) = print_setting(s.needs) else { return false };
    let v = value(parent);
    let on = match parent.key {
        SUPPORT_TYPE => support_reads(&v, s.key),
        BRIM_TYPE => BRIM_READS.iter().any(|(kind, keys)| *kind == v && keys.contains(&s.key)),
        SPARSE_PATTERN => GRADED_PATTERNS.contains(&v.as_str()),
        _ => setting_on(parent, &v),
    };
    on && needs_met(parent, value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_consistent() {
        for s in PRINT_SETTINGS {
            assert_eq!(PRINT_SETTINGS.iter().filter(|o| o.key == s.key).count(), 1, "{} twice", s.key);
            assert!(!s.label.is_empty() && !s.hint.is_empty(), "{}", s.key);
            for (i, (value, _)) in s.common.iter().enumerate() {
                assert!(matches!(s.kind, SettingKind::Choice(c) if c.iter().any(|(v, _)| v == value)), "{} has no choice {value}", s.key);
                assert!(!s.common[..i].iter().any(|(v, _)| v == value), "{} lists {value} twice among its common choices", s.key);
            }
            if !s.needs.is_empty() {
                let parent = print_setting(s.needs).unwrap_or_else(|| panic!("{} needs {}", s.key, s.needs));
                assert_eq!(parent.group, s.group, "{}", s.key);
            }
            checked_value(s.key, s.default).unwrap_or_else(|e| panic!("{}'s default: {e}", s.key));
        }
        // Each support type is one the type row offers, and reads only rows under it; each row
        // under it some type reads.
        let SettingKind::Choice(types) = print_setting(SUPPORT_TYPE).unwrap().kind else { panic!("the support type is a choice") };
        for (t, keys) in SUPPORT_READS {
            assert!(types.iter().any(|(v, _)| v == t), "{t} is not a support type");
            for k in *keys {
                assert_eq!(print_setting(k).map(|s| s.needs), Some(SUPPORT_TYPE), "{t} reads {k}, not a row under the support type");
            }
        }
        for s in PRINT_SETTINGS.iter().filter(|s| s.needs == SUPPORT_TYPE) {
            assert!(SUPPORT_READS.iter().any(|(_, keys)| keys.contains(&s.key)), "no support type reads {}", s.key);
        }
        // The same of the part's rim.
        let SettingKind::Choice(rims) = print_setting(BRIM_TYPE).unwrap().kind else { panic!("the rim is a choice") };
        for (t, keys) in BRIM_READS {
            assert!(rims.iter().any(|(v, _)| v == t), "{t} is not a rim");
            for k in *keys {
                assert_eq!(print_setting(k).map(|s| s.needs), Some(BRIM_TYPE), "{t} reads {k}, not a row under the rim");
            }
        }
        for s in PRINT_SETTINGS.iter().filter(|s| s.needs == BRIM_TYPE) {
            assert!(BRIM_READS.iter().any(|(_, keys)| keys.contains(&s.key)), "no rim reads {}", s.key);
        }
    }

    /// Whether the row `key` shows with these values, the rest at their defaults.
    fn shown(key: &str, values: &[(&str, &str)]) -> bool {
        let value = |s: &PrintSetting| values.iter().find(|(k, _)| *k == s.key).map_or(s.default.to_string(), |(_, v)| v.to_string());
        needs_met(print_setting(key).unwrap(), &value)
    }

    /// A support row shows while supports are on and the chosen type reads it.
    #[test]
    fn support_rows_show_for_the_type_that_reads_them() {
        let tree = [("enable_support", "1"), ("support_type", "tree(auto)")];
        assert!(shown("tree_support_branch_diameter_organic", &tree) && shown("fab_tree_outside_footprint", &tree) && shown("support_remove_small_overhang", &tree));
        assert!(shown("fab_support_inside", &tree) && !shown("support_style", &tree) && !shown("support_base_pattern_spacing", &tree) && !shown("support_expansion", &tree));
        let normal = [("enable_support", "1"), ("support_type", "normal(auto)")];
        assert!(shown("support_style", &normal) && shown("fab_support_inside", &normal) && !shown("tree_support_branch_angle_organic", &normal));
        let off = [("enable_support", "0"), ("support_type", "tree(auto)")];
        assert!(!shown("support_threshold_angle", &off) && !shown("support_type", &off), "none of the support shows with supports off");
        assert!(shown("support_type", &tree) && shown("brim_type", &off));
        assert!(shown("support_object_first_layer_gap", &tree) && shown("support_object_first_layer_gap", &normal) && !shown("support_object_first_layer_gap", &off));
    }

    /// The part's rim rows show while the chosen rim reads them, supports on or off.
    #[test]
    fn rim_rows_show_for_the_rim_that_reads_them() {
        for (rim, width, gap) in [("auto_brim", false, true), ("no_brim", false, false), ("outer_only", true, true), ("inner_only", true, true), ("outer_and_inner", true, true)] {
            for supports in ["0", "1"] {
                let values = [("enable_support", supports), ("brim_type", rim)];
                assert_eq!((shown("brim_width", &values), shown("brim_object_gap", &values)), (width, gap), "{rim}");
            }
        }
    }

    /// Adaptive density shows under a pattern it grades, and its own rows while it is on.
    #[test]
    fn adaptive_rows_show_under_the_graded_patterns() {
        let SettingKind::Choice(patterns) = print_setting(SPARSE_PATTERN).unwrap().kind else { panic!("the sparse pattern is a choice") };
        for graded in GRADED_PATTERNS {
            assert!(patterns.iter().any(|(v, _)| v == graded), "{graded} is not a sparse pattern");
        }
        for (pattern, mode, switch, rows) in [("gyroid", "disabled", true, false), ("tpmsd", "distance_warp", true, true), ("tpmsfk", "lobes", true, true), ("grid", "distance_warp", false, false)] {
            let values = [(SPARSE_PATTERN, pattern), ("adaptive_infill_mode", mode)];
            assert_eq!(shown("adaptive_infill_mode", &values), switch, "the switch with {pattern}");
            for key in ["adaptive_infill_interior_density", "adaptive_infill_gradient"] {
                assert_eq!(shown(key, &values), rows, "{key} with {pattern} and {mode}");
            }
        }
    }

    #[test]
    fn values_are_checked_and_written_as_the_slicer_reads_them() {
        assert_eq!(checked_value("sparse_infill_density", "35").unwrap(), "35%");
        assert_eq!(checked_value("sparse_infill_density", "35%").unwrap(), "35%");
        assert_eq!(checked_value("wall_loops", "3").unwrap(), "3");
        assert_eq!(checked_value("enable_support", "true").unwrap(), "1");
        assert_eq!(checked_value("enable_prime_tower", "false").unwrap(), "0");
        assert!(checked_value("sparse_infill_density", "140").unwrap_err().contains("0 to 100") || checked_value("sparse_infill_density", "140").unwrap_err().contains("from 0 to 100"));
        assert!(checked_value("sparse_infill_pattern", "stars").unwrap_err().contains("gyroid"));
        assert!(checked_value("no_such_key", "1").is_err());
    }

    #[test]
    fn plate_settings_say_why() {
        assert!(!is_part_print_key("initial_layer_print_height"));
        assert!(is_part_print_key("sparse_infill_density"));
        for s in PRINT_SETTINGS {
            if let SettingScope::Plate(why) = s.scope {
                assert!(why.len() > 10, "{}", s.key);
            }
        }
    }
}
