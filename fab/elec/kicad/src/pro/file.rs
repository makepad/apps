//! Located project facts; manufacturing defaults and range checks belong to construction.

use crate::{json::Value, pcb::{Located, Location}, report::ImportReport};

pub const CLASS_FIELDS: [&str; 9] = [
    "clearance", "track_width", "via_diameter", "via_drill", "microvia_diameter",
    "microvia_drill", "diff_pair_width", "diff_pair_gap", "diff_pair_via_gap",
];
pub const CLASS_DEFAULTS: [f64; 9] = [0.2, 0.2, 0.6, 0.3, 0.3, 0.1, 0.2, 0.25, 0.25];

/// Numeric class fields parallel CLASS_FIELDS; missing fields remain unset.
#[derive(Clone, Debug)]
pub struct ProjectClass {
    pub name: Located<String>,
    pub fields: [Option<Located<f64>>; 9],
    /// Integer-valued floats preserve the full finite JSON range; older default priorities are infinite.
    pub priority: Option<Located<f64>>,
    pub pcb_color: Option<Located<String>>,
    /// Kept as JSON because the format does not prescribe a profile's internal shape.
    pub tuning_profile: Option<Value>,
    /// Schematic-only fields are retained without interpretation or loss counts.
    pub wire_width: Option<Value>,
    pub bus_width: Option<Value>,
    pub line_style: Option<Value>,
    pub schematic_color: Option<Value>,
    pub nets: Vec<Located<String>>,
}
impl ProjectClass {
    pub fn number(&self, key: &str) -> Option<&Located<f64>> {
        CLASS_FIELDS.iter().position(|&k| k == key).and_then(|i| self.fields[i].as_ref())
    }
}

#[derive(Clone, Debug)]
pub struct Assignment {
    pub name: Located<String>,
    pub classes: Vec<Located<String>>,
}
#[derive(Clone, Debug)]
pub struct Pattern {
    pub pattern: Located<String>,
    pub class: Located<String>,
}

#[derive(Clone, Debug)]
pub struct ProjectFile {
    pub location: Location,
    pub design_settings: Option<Location>,
    pub design_version: Located<i64>,
    pub net_version: Located<i64>,
    /// Raw numbers, including the old mask/paste values and old spelling of silk thickness.
    pub rules: Vec<(String, Located<f64>)>,
    pub size_presets: Vec<Located<String>>,
    pub exclusions: Option<Located<usize>>,
    pub external_fillets: Option<Located<bool>>,
    pub severities: Vec<(String, Located<String>)>,
    pub classes_location: Option<Location>,
    pub classes: Vec<ProjectClass>,
    pub assignments: Vec<Assignment>,
    pub patterns: Vec<Pattern>,
    pub text_variables: Option<Located<usize>>,
    pub net_chains: Vec<Located<usize>>,
    pub net_colors: Option<Located<usize>>,
    pub component_classes: Option<Located<usize>>,
    pub report: ImportReport,
}
impl ProjectFile {
    pub fn rule(&self, key: &str) -> Option<&Located<f64>> {
        self.rules.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    pub fn severity(&self, key: &str) -> Option<&str> {
        self.severities.iter().find(|(k, _)| k == key).map(|(_, v)| v.value.as_str())
    }
}

/// Table shared by parsing and building: key, default, inclusive minimum and maximum.
pub(crate) const RULES: &[(&str, f64, f64, f64)] = &[
    ("min_track_width", 0.2, 0.0, 25.0), ("min_via_annular_width", 0.5, 0.0, 25.0),
    ("min_through_hole_diameter", 0.3, 0.0, 25.0), ("min_hole_to_hole", 0.25, 0.0, 10.0),
    ("min_hole_clearance", 0.25, 0.0, 100.0), ("min_copper_edge_clearance", 0.5, -0.01, 25.0),
    ("min_silk_clearance", 0.0, -10.0, 100.0), ("min_clearance", 0.0, 0.0, 25.0),
    ("min_via_diameter", 0.5, 0.0, 25.0), ("min_microvia_diameter", 0.2, 0.0, 10.0),
    ("min_microvia_drill", 0.1, 0.0, 10.0), ("min_connection", 0.0, 0.0, 100.0),
    ("min_groove_width", 0.0, 0.0, 25.0), ("min_resolved_spokes", 2.0, 0.0, 99.0),
    ("min_text_height", 0.8, 0.0, 100.0), ("min_silk_line_width", 0.08, 0.0, 25.0),
    ("solder_mask_to_copper_clearance", 0.0, f64::NEG_INFINITY, f64::INFINITY),
];
