//! The option table: every profile key with its type, preset kind, vector
//! axis, default and enum values (`options.tsv`, kept by hand from Fab's own reading of the
//! profile format).

use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Bool,
    Bools,
    Float,
    Floats,
    Int,
    Ints,
    Percent,
    Percents,
    FloatOrPercent,
    FloatsOrPercents,
    Str,
    Strs,
    Enum,
    Enums,
    Point,
    Points,
    PointsGroups,
}

impl Kind {
    pub fn is_vector(self) -> bool {
        matches!(self, Kind::Bools | Kind::Floats | Kind::Ints | Kind::Percents | Kind::FloatsOrPercents | Kind::Strs | Kind::Enums | Kind::Points | Kind::PointsGroups)
    }
    fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "bool" => Kind::Bool,
            "bools" => Kind::Bools,
            "float" => Kind::Float,
            "floats" => Kind::Floats,
            "int" => Kind::Int,
            "ints" => Kind::Ints,
            "percent" => Kind::Percent,
            "percents" => Kind::Percents,
            "float_or_percent" => Kind::FloatOrPercent,
            "floats_or_percents" => Kind::FloatsOrPercents,
            "string" => Kind::Str,
            "strings" => Kind::Strs,
            "enum" => Kind::Enum,
            "enums" => Kind::Enums,
            "point" => Kind::Point,
            "points" => Kind::Points,
            "points_groups" => Kind::PointsGroups,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresetKind {
    Machine,
    Process,
    Filament,
    /// Plate, object or project settings.
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// Scalar, or a vector whose length is its own business.
    None,
    /// One entry per physical extruder.
    PerExtruder,
    /// One entry per (extruder, nozzle variant) slot of the printer.
    PrinterVariant,
    /// Two entries (normal, silent) per printer slot.
    PrinterVariant2,
    /// One entry per (extruder, nozzle variant) slot of the process.
    ProcessVariant,
    /// One entry per nozzle variant of the filament.
    FilamentVariant,
}

#[derive(Clone, Debug)]
pub struct OptionDef {
    pub key: String,
    pub kind: Kind,
    pub preset: PresetKind,
    pub axis: Axis,
    /// Profile text syntax; empty when there is none.
    pub default: String,
    pub enum_values: Vec<String>,
}

pub struct Options {
    pub defs: HashMap<String, OptionDef>,
}

impl Options {
    pub fn get(&self, key: &str) -> Option<&OptionDef> {
        self.defs.get(key)
    }
    pub fn kind(&self, key: &str) -> Option<Kind> {
        self.defs.get(key).map(|d| d.kind)
    }
    pub fn axis(&self, key: &str) -> Axis {
        self.defs.get(key).map(|d| d.axis).unwrap_or(Axis::None)
    }
}

pub fn options() -> &'static Options {
    static OPTS: OnceLock<Options> = OnceLock::new();
    OPTS.get_or_init(|| {
        let text = include_str!("options.tsv");
        let mut defs = HashMap::new();
        for line in text.lines() {
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 5 {
                continue;
            }
            let Some(kind) = Kind::parse(f[1]) else { continue };
            let preset = match f[2] {
                "machine" | "machine(extruder)" | "machine(limits)" => PresetKind::Machine,
                "process" => PresetKind::Process,
                "filament" => PresetKind::Filament,
                _ => PresetKind::Other,
            };
            let axis = match f[3] {
                "per-extruder" => Axis::PerExtruder,
                "printer-variant" => Axis::PrinterVariant,
                "printer-variant x2(silent)" => Axis::PrinterVariant2,
                "process-variant" => Axis::ProcessVariant,
                "filament-variant" => Axis::FilamentVariant,
                _ => Axis::None,
            };
            let enum_values = f.get(5).map(|s| s.split('/').filter(|v| !v.is_empty()).map(|v| v.to_string()).collect()).unwrap_or_default();
            defs.insert(f[0].to_string(), OptionDef { key: f[0].to_string(), kind, preset, axis, default: f[4].to_string(), enum_values });
        }
        // Keys the table does not carry.
        // Fab's experimental modes (EXPSLICE): all off by default. `nil`: no default, so an
        // unset mode stays out of the config (and the G-code's config block); exp.rs falls back.
        for (key, kind, preset, axis, default) in [
            ("nozzle_type", Kind::Enums, PresetKind::Machine, Axis::PrinterVariant, ""),
            // The sixth plate of `machine::PLATES`: a filament's temperatures for it, as for the
            // other five, so every plate is a key Fab knows. No default: a preset sets them.
            ("textured_cool_plate_temp", Kind::Ints, PresetKind::Filament, Axis::None, "nil"),
            ("textured_cool_plate_temp_initial_layer", Kind::Ints, PresetKind::Filament, Axis::None, "nil"),
            // Outer-wall texture: none, fuzzy, knurl, scales, woven, wood_grain.
            // Depth is fuzzy_skin_thickness, the sample step fuzzy_skin_point_distance.
            ("fab_wall_texture", Kind::Str, PresetKind::Process, Axis::None, "nil"),
            ("fab_wall_texture_scale", Kind::Float, PresetKind::Process, Axis::None, "nil"),
            ("fab_wall_texture_holes", Kind::Bool, PresetKind::Process, Axis::None, "nil"),
            // Spiral vase variants (spiral_mode turns the vase on): plain, double_wall, variable_width.
            ("fab_vase_variant", Kind::Str, PresetKind::Process, Axis::None, "nil"),
            ("fab_vase_width_max", Kind::Float, PresetKind::Process, Axis::None, "nil"),
            ("fab_vase_rib_spacing", Kind::Float, PresetKind::Process, Axis::None, "nil"),
            // Adaptive layer height from the surface slope, within min/max_layer_height.
            ("fab_adaptive_layers", Kind::Bool, PresetKind::Process, Axis::None, "nil"),
            ("fab_adaptive_cusp", Kind::Float, PresetKind::Process, Axis::None, "nil"),
            // Arc overhangs: concentric fronts on the first overhang layer.
            ("fab_arc_overhangs", Kind::Bool, PresetKind::Process, Axis::None, "nil"),
            ("fab_arc_overhang_speed", Kind::Float, PresetKind::Process, Axis::None, "nil"),
            // Non-planar top finish (NONPLANAR): `fab_nonplanar_top` turns it on (per part on the plate); these tune it.
            // Shells 1-3 (2), slope cap in degrees (15, at most 20), rise cap in mm (3), chord tolerance in mm (0.005).
            ("fab_nonplanar_top", Kind::Bool, PresetKind::Process, Axis::None, "nil"),
            ("fab_nonplanar_shells", Kind::Int, PresetKind::Process, Axis::None, "nil"),
            ("fab_nonplanar_slope_cap", Kind::Float, PresetKind::Process, Axis::None, "nil"),
            ("fab_nonplanar_rise", Kind::Float, PresetKind::Process, Axis::None, "nil"),
            ("fab_nonplanar_tolerance", Kind::Float, PresetKind::Process, Axis::None, "nil"),
            ("fab_calibration_bands", Kind::Str, PresetKind::Process, Axis::None, ""),
            // Supports, normal and tree, may stand where they'd be locked in the part: hollows,
            // pores and lattice cells with no way out as wide as a column.
            ("fab_support_inside", Kind::Bool, PresetKind::Process, Axis::None, "0"),
            // Tree supports keep out of the part's footprint: what it covers at or below each
            // layer, so none is locked in it; what overhangs there prints unsupported. Unset is
            // off, and stays out of the config block.
            ("fab_tree_outside_footprint", Kind::Bool, PresetKind::Process, Axis::None, "nil"),
            // The tree branch's diameter (mm) and the most it leans (degrees), as the reference's
            // organic trees and its profiles name them: the keys Fab's trees read. Unset, the
            // trees' own defaults (2 mm, 40 degrees).
            ("tree_support_branch_diameter_organic", Kind::Float, PresetKind::Process, Axis::None, "nil"),
            ("tree_support_branch_angle_organic", Kind::Float, PresetKind::Process, Axis::None, "nil"),
            // A painted part's paint prints as it is in its walls, surfaces and a skin this deep
            // (mm, 1 unset), and deeper in large cells of one material each, unless the inside
            // is exact too (a part to be cut open).
            ("fab_paint_skin_depth", Kind::Float, PresetKind::Process, Axis::None, "nil"),
            ("fab_paint_inside_exact", Kind::Bool, PresetKind::Process, Axis::None, "nil"),
            // The first layer prints a painted part's paint too (a picture printed face down);
            // unset, it prints in the part's own filament.
            ("fab_paint_first_layer", Kind::Bool, PresetKind::Process, Axis::None, "nil"),
            // Every solid layer's lines lie on one grid through the part's middle, a spacing
            // apart, never stretched to fit (a picture whose cells are its lines).
            ("fab_solid_on_grid", Kind::Bool, PresetKind::Process, Axis::None, "nil"),
            // The printer's material slots, one entry each: `<nozzle>:<tray>`, the nozzle from 1, then
            // `:own` for a path of its own into the nozzle or `:dedicated` for one that shares nothing
            // ("1:A1", "2:Ext:dedicated"); unset, a unit of four trays and an external spool on
            // each of two nozzles. A filament on a dedicated path (set per job material from its
            // slot) purges nothing at a change into or out of it.
            ("fab_material_slots", Kind::Strs, PresetKind::Machine, Axis::None, "nil"),
            ("fab_dedicated_path", Kind::Bools, PresetKind::Filament, Axis::None, "nil"),
            // The filament prints on an unheated bed: it suits every plate and asks for no bed
            // heat (an imported preset's bed of 0 becomes this).
            ("fab_bed_unheated", Kind::Bools, PresetKind::Filament, Axis::None, "nil"),
            // A masked resin printer (`printer_technology` SLA): its screen, in pixels along
            // the bed's X and Y, their pitch (mm), the image's mirror (none, x, y, xy) and
            // counter-clockwise turn (0, 90, 180, 270 degrees) on its way to the screen, and the
            // grey levels a pixel shows (2: on or off). `printable_area` is the screen's area.
            // None has a default here, so no filament job's configuration carries them; a resin
            // printer leaving out the mirror, turn or grey levels has none, none and 256.
            ("fab_sla_resolution", Kind::Point, PresetKind::Machine, Axis::None, "nil"),
            ("fab_sla_pixel_pitch", Kind::Point, PresetKind::Machine, Axis::None, "nil"),
            ("fab_sla_mirror", Kind::Str, PresetKind::Machine, Axis::None, "nil"),
            ("fab_sla_rotation", Kind::Int, PresetKind::Machine, Axis::None, "nil"),
            ("fab_sla_grey_levels", Kind::Int, PresetKind::Machine, Axis::None, "nil"),
            // What the printer reads and how it gets there: Fab's own folder of layer images
            // (`folder`) or the printer's archive (`archive`), the archive's layers as grey
            // pixels (`raster`) or outlines (`vector`), and the route, Fab's own link (`link`) or
            // a removable drive the person carries over (`drive`). Left out: a folder over the
            // link, raster.
            ("fab_sla_container", Kind::Str, PresetKind::Machine, Axis::None, "nil"),
            ("fab_sla_layer_encoding", Kind::Str, PresetKind::Machine, Axis::None, "nil"),
            ("fab_sla_route", Kind::Str, PresetKind::Machine, Axis::None, "nil"),
            // Its exposure: the first layers' count and seconds of light, every other layer's,
            // and the wait with the light off after each move. `layer_height` is the layer.
            ("fab_sla_bottom_layers", Kind::Int, PresetKind::Process, Axis::None, "nil"),
            ("fab_sla_bottom_exposure", Kind::Float, PresetKind::Process, Axis::None, "nil"),
            ("fab_sla_exposure", Kind::Float, PresetKind::Process, Axis::None, "nil"),
            ("fab_sla_light_off_delay", Kind::Float, PresetKind::Process, Axis::None, "nil"),
            // The peel after each layer: the plate lifts this far (mm) at this speed (mm/s), the
            // first layers with their own; it comes back down at the retract speed (mm/s).
            ("fab_sla_lift_height", Kind::Float, PresetKind::Process, Axis::None, "nil"),
            ("fab_sla_lift_speed", Kind::Float, PresetKind::Process, Axis::None, "nil"),
            ("fab_sla_bottom_lift_height", Kind::Float, PresetKind::Process, Axis::None, "nil"),
            ("fab_sla_bottom_lift_speed", Kind::Float, PresetKind::Process, Axis::None, "nil"),
            ("fab_sla_retract_speed", Kind::Float, PresetKind::Process, Axis::None, "nil"),
            // For the printer's archive: the anti-alias level it says the layers have (1 to 16;
            // left out, 16), the resin's price per bottle, its bottle (ml) and density (g/ml)
            // for the cost line, and which of the maker's settings groups (normal, fast,
            // high_speed) the archive copies the values Fab doesn't set from.
            ("fab_sla_anti_alias", Kind::Int, PresetKind::Process, Axis::None, "nil"),
            ("fab_sla_resin_price", Kind::Float, PresetKind::Process, Axis::None, "nil"),
            ("fab_sla_resin_bottle", Kind::Float, PresetKind::Process, Axis::None, "nil"),
            ("fab_sla_resin_density", Kind::Float, PresetKind::Process, Axis::None, "nil"),
            ("fab_sla_group", Kind::Str, PresetKind::Process, Axis::None, "nil"),
            // What the supports are sized against (left out, standard resin on an FEP film):
            // the peel's pull per area of the newest layer (kPa; the film's), the washed green
            // resin's tensile strength (MPa) and modulus (GPa), the margin a section holds its
            // load by, a tip's least and most diameter (mm; the least never under five pixels,
            // the most still snaps off by hand), and how many times its push back down a pillar
            // holds before it buckles.
            ("fab_sla_peel_pressure", Kind::Float, PresetKind::Machine, Axis::None, "nil"),
            ("fab_sla_green_strength", Kind::Float, PresetKind::Process, Axis::None, "nil"),
            ("fab_sla_green_modulus", Kind::Float, PresetKind::Process, Axis::None, "nil"),
            ("fab_sla_load_margin", Kind::Float, PresetKind::Process, Axis::None, "nil"),
            ("fab_sla_tip_min", Kind::Float, PresetKind::Process, Axis::None, "nil"),
            ("fab_sla_tip_max", Kind::Float, PresetKind::Process, Axis::None, "nil"),
            ("fab_sla_buckling_factor", Kind::Float, PresetKind::Process, Axis::None, "nil"),
        ] {
            defs.entry(key.to_string()).or_insert(OptionDef { key: key.to_string(), kind, preset, axis, default: default.to_string(), enum_values: Vec::new() });
        }
        Options { defs }
    })
}
