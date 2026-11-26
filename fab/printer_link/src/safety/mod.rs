//! The print safety gate. A job reaches the printer only as a [`ValidatedJob`],
//! and only [`validate`] or [`validate_gcode`] makes one, from the exact bytes checked. There is
//! no override, flag or environment variable.
//!
//! The check simulates the G-code's modal state, it does not read Z literals:
//! absolute/relative moves (G90/G91), extrusion modes (M82/M83), position
//! resets (G92), homing (G28), the active filament and its nozzle (T), and
//! arcs (G2/G3, with their extremes). The frame is the printer's homed plate
//! frame: Z = 0 is what the printer's own start sequence (homing, probing,
//! mesh and plate compensation, all done by the firmware) makes the plate.
//!
//! The installed profile's templates (start, end, filament change, layer
//! change, filament start/end, timelapse) are trusted only where the file's
//! lines match them literally ([`template`]); lines that came through a
//! placeholder are checked like Fab's own. Everything outside those blocks is
//! Fab's toolpath and must:
//! - use only motion, extrusion, fan, temperature, bounded pressure advance,
//!   progress and label commands;
//! - move only after the Z axis is homed, with a known position;
//! - never go below the plate (effective Z < 0.1 mm), never reset X/Y/Z with G92;
//! - start its first layer between 0.1 and 0.4 mm;
//! - stay inside the active nozzle's printable area and height;
//! - keep travel lifts small (at most 3 mm above the highest layer so far);
//! - not travel across the printed first layer at first-layer height;
//! - extrude in planes, unless the installed profile has a measured head envelope:
//!   a move that pushes or pulls filament may not change Z, and an extruding arc may
//!   not carry a Z word. With a measured envelope, non-planar extrusion (straight
//!   moves only) must keep the slope and rise caps, the Z speed and jerk, and the
//!   head must clear the printed material ([`nonplanar`]). A dry helical arc (the
//!   spiral lift) is travel. A monotone ramp needs no head geometry: a straight
//!   extrusion that only rises, starts at most a small dip below everything printed,
//!   keeps a 5 deg pitch and the machine's Z speed (scarf seams, spiral vases);
//! - extrude with a sane flow (gross errors only): bead area at most 3 d^2 for nozzle d,
//!   at most 2.5x the filament's volumetric speed, in-place pushes no longer than a
//!   retraction + 5 mm.
//! The file's own settings must name the installed machine's areas, heights,
//! nozzle offsets, nozzle diameters, model and templates.

pub mod paths;
pub mod nonplanar;
pub mod template;

use makepad_fab_settings::family::{self, Flavor};
use std::sync::Arc;
use template::{code, Template};

/// The installed system machine profile the file must match, never from the file.
#[derive(Clone, Debug)]
pub struct MachineProfile {
    pub flavor: Flavor,
    pub printer_model: String,
    pub printable_area: Vec<[f64; 2]>,
    pub printable_height: f64,
    /// Per extruder (slicer order: index 0 = left, 1 = right).
    pub extruder_areas: Vec<Vec<[f64; 2]>>,
    pub extruder_heights: Vec<f64>,
    pub extruder_offsets: Vec<[f64; 2]>,
    pub nozzle_diameters: Vec<f64>,
    /// (setting name, template text): machine templates and each installed filament's start/end.
    pub templates: Vec<(String, String)>,
    /// The user's measured head geometry and non-planar limits (never from the file).
    /// None: non-planar moves are refused.
    pub head: Option<nonplanar::HeadEnvelope>,
    /// `machine_max_speed_z` (mm/s). None: rising-Z extrusion (ramps) is refused.
    pub max_speed_z: Option<f64>,
    /// Per filament: diameter (mm) and maximum volumetric speed (mm^3/s). No diameter: extrusion is refused.
    pub filament_diameters: Vec<f64>,
    pub max_volumetric: Vec<f64>,
    /// Per job filament (in order), from the installed filament presets.
    pub filament_types: Vec<String>,
    pub nozzle_temp_ranges: Vec<[f64; 2]>,
    pub print_temps: Vec<[f64; 2]>,
    pub required_hrc: Vec<f64>,
    pub chamber_temps: Vec<f64>,
    /// The firmware tool index of each extruder (missing: the identity).
    pub physical_extruder_map: Vec<usize>,
    /// The machine's motion limits.
    pub limits: MotionLimits,
    /// The most filament an in-place (E-only) move may push: the longest retraction, plus a 5 mm prime.
    pub prime_allowance: f64,
}

impl Default for MachineProfile {
    fn default() -> Self {
        Self {
            flavor: Flavor::Mask,
            printer_model: Default::default(),
            printable_area: Default::default(),
            printable_height: Default::default(),
            extruder_areas: Default::default(),
            extruder_heights: Default::default(),
            extruder_offsets: Default::default(),
            nozzle_diameters: Default::default(),
            templates: Default::default(),
            head: Default::default(),
            max_speed_z: Default::default(),
            filament_diameters: Default::default(),
            max_volumetric: Default::default(),
            filament_types: Default::default(),
            nozzle_temp_ranges: Default::default(),
            print_temps: Default::default(),
            required_hrc: Default::default(),
            chamber_temps: Default::default(),
            physical_extruder_map: Default::default(),
            limits: Default::default(),
            prime_allowance: Default::default(),
        }
    }
}

/// The largest installed limits; axes are X, Y, Z, E.
#[derive(Clone, Debug, Default)]
pub struct MotionLimits {
    pub acceleration: [Option<f64>; 4],
    pub speed: [Option<f64>; 4],
    pub jerk: [Option<f64>; 4],
    pub move_acceleration: Option<f64>,
}

/// Settings keys whose template text the file must carry unchanged.
pub const MACHINE_TEMPLATES: &[&str] = &[
    "machine_start_gcode", "machine_end_gcode", "change_filament_gcode", "layer_change_gcode",
    "before_layer_change_gcode", "time_lapse_gcode", "wrapping_detection_gcode", "printing_by_object_gcode",
];

pub fn parse_points(text: &str) -> Vec<[f64; 2]> {
    text.split(',').map(|p| {
        let (x, y) = p.trim().split_once('x')?;
        Some([finite(x)?, finite(y)?])
    }).collect::<Option<Vec<_>>>().unwrap_or_default()
}

impl MachineProfile {
    /// From profile values: `get(key)` gives a setting's elements as text.
    pub fn from_settings(get: &dyn Fn(&str) -> Vec<String>, filament_templates: Vec<(String, String)>) -> Result<Self, Refusal> {
        let one = |k: &str| get(k).into_iter().next().unwrap_or_default();
        let nums = |k: &str| get(k).iter().map(|s| finite(s)).collect::<Option<Vec<f64>>>().unwrap_or_default();
        let pairs = |a: &str, b: &str| {
            let (a, b) = (nums(a), nums(b));
            if a.len() != b.len() { Vec::new() } else { a.into_iter().zip(b).map(|(a, b)| [a, b]).collect() }
        };
        let largest = |k: &str| nums(k).into_iter().reduce(f64::max);
        let axes = |prefix: &str| ["x", "y", "z", "e"].map(|a| largest(&format!("{prefix}_{a}")));
        let nozzle_diameters = nums("nozzle_diameter");
        let map = get("physical_extruder_map");
        let physical_extruder_map = if map.is_empty() { (0..nozzle_diameters.len()).collect() }
            else { map.iter().map(|s| s.trim().parse::<usize>()).collect::<Result<Vec<_>, _>>().unwrap_or_default() };
        let area = get("printable_area").join(",");
        let mut templates: Vec<(String, String)> = MACHINE_TEMPLATES.iter().map(|k| (k.to_string(), one(k))).filter(|t| !t.1.trim().is_empty()).collect();
        templates.extend(filament_templates.into_iter().filter(|t| !t.1.trim().is_empty()));
        // Always an installed printer's profile: it names its model.
        let printer_model = one("printer_model");
        if printer_model.is_empty() {
            return Err(Refusal(vec![Violation { line: 0, rule: "machine", detail: family::NO_MODEL.into() }]));
        }
        let flavor = family::of_model(&printer_model).ok_or_else(unsupported_model)?.flavor;
        let printable_area = parse_points(&area);
        let printable_height = finite(&one("printable_height")).unwrap_or(0.0);
        let (extruder_areas, extruder_heights) = per_nozzle(flavor, areas(&get("extruder_printable_area").join("#")), nums("extruder_printable_height"), &printable_area, printable_height, nozzle_diameters.len());
        Ok(Self {
            flavor,
            printer_model,
            printable_area,
            printable_height,
            extruder_areas,
            extruder_heights,
            extruder_offsets: parse_points(&get("extruder_offset").join(",")),
            nozzle_diameters,
            templates,
            head: None,
            max_speed_z: nums("machine_max_speed_z").into_iter().reduce(f64::min),
            filament_diameters: nums("filament_diameter"),
            max_volumetric: nums("filament_max_volumetric_speed"),
            filament_types: get("filament_type"),
            nozzle_temp_ranges: pairs("nozzle_temperature_range_low", "nozzle_temperature_range_high"),
            print_temps: pairs("nozzle_temperature_initial_layer", "nozzle_temperature").into_iter().map(|p| [p[0].min(p[1]), p[0].max(p[1])]).collect(),
            required_hrc: nums("required_nozzle_HRC"),
            chamber_temps: nums("chamber_temperatures"),
            physical_extruder_map,
            limits: MotionLimits {
                acceleration: axes("machine_max_acceleration"), speed: axes("machine_max_speed"), jerk: axes("machine_max_jerk"),
                move_acceleration: ["extruding", "retracting", "travel", "x", "y"].iter().filter_map(|a| largest(&format!("machine_max_acceleration_{a}"))).reduce(f64::max),
            },
            prime_allowance: nums("retraction_length").into_iter().chain(nums("retraction_distances_when_cut")).chain(nums("retract_length_toolchange")).fold(0.0, f64::max) + 5.0,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Violation { pub line: usize, pub rule: &'static str, pub detail: String }

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.line > 0 { write!(f, "line {}: {}: {}", self.line, self.rule, self.detail) } else { write!(f, "{}: {}", self.rule, self.detail) }
    }
}

/// Why a file may not be printed (the first problems found).
#[derive(Clone, Debug, PartialEq)]
pub struct Refusal(pub Vec<Violation>);

fn unsupported_model() -> Refusal {
    Refusal(vec![Violation { line: 0, rule: "machine", detail: family::UNSUPPORTED.into() }])
}

impl Refusal {
    pub fn message(&self) -> String {
        let mut s = format!("Refused by the print safety check ({} problem{}): ", self.0.len(), if self.0.len() == 1 { "" } else { "s" });
        s.push_str(&self.0.iter().take(3).map(|v| v.to_string()).collect::<Vec<_>>().join("; "));
        s
    }
    pub fn has(&self, rule: &str) -> bool { self.0.iter().any(|v| v.rule == rule) }
}

/// One job filament as checked: what the printer's tray and nozzle must agree with.
#[derive(Clone, Debug, PartialEq)]
pub struct FilamentFacts {
    /// The installed preset's `filament_type` ("PLA", "PETG-CF").
    pub kind: String,
    /// The extruder it prints on (0 = left, 1 = right), from the file's filament map.
    pub extruder: usize,
    /// Its print temperatures in the installed preset (lower, higher; °C).
    pub print_temps: [f64; 2],
    /// It needs an abrasion-resistant nozzle: its type names a fibre fill (contains "CF" or "GF",
    /// case-insensitive) or its preset's required nozzle hardness is above a stainless nozzle's (HRC 20).
    pub needs_hardened: bool,
}

/// What the check established about a job (for the live re-check at print start).
#[derive(Clone, Debug, PartialEq)]
pub struct JobFacts {
    /// One entry per job filament, in file order.
    pub filaments: Vec<FilamentFacts>,
    pub used_mm: Vec<f64>,
    pub used_g: Vec<f64>,
    /// Per extruder (0 = left, 1 = right).
    pub nozzle_diameters: Vec<f64>,
    /// Extruders the toolpath prints with.
    pub extruders_used: Vec<usize>,
    /// The file's plate type; printer reports carry no plate type to compare it with.
    pub bed_type: String,
    pub first_layer_z: f64,
    pub max_z: f64,
    pub moves: usize,
    /// Non-planar extrusions, their steepest pitch (degrees), the least head clearance seen
    /// where the head was checked, and the head envelope they were checked against.
    pub nonplanar_moves: usize,
    pub max_pitch_deg: f64,
    pub min_head_clearance: Option<f64>,
    pub head_version: Option<String>,
    /// Monotone ramps (rising extrusion allowed without head geometry).
    pub ramp_moves: usize,
}

/// A print file that passed [`validate`] or [`validate_gcode`]. Its bytes cannot change: printing a
/// changed file needs a new validation. Only this module can make one.
#[derive(Clone)]
pub struct ValidatedJob {
    bytes: Arc<[u8]>,
    sha256: [u8; 32],
    md5: String,
    facts: JobFacts,
    config: Vec<(String, String)>,
    name: String,
}

impl std::fmt::Debug for ValidatedJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValidatedJob").field("name", &self.name).field("bytes", &self.bytes.len()).field("facts", &self.facts).finish()
    }
}

impl ValidatedJob {
    pub fn bytes(&self) -> &[u8] { &self.bytes }
    pub fn facts(&self) -> &JobFacts { &self.facts }
    pub fn name(&self) -> &str { &self.name }
    pub fn config_value(&self, key: &str) -> Option<&str> {
        self.config.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
    /// Uppercase MD5 of the exact checked file.
    pub fn md5(&self) -> &str { &self.md5 }
    /// The bytes are still the ones checked (they cannot change; this is the belt to that brace).
    pub fn intact(&self) -> bool { makepad_network::digest::sha256_hash(&self.bytes) == self.sha256 }
}

/// Checks a job's file as its family's link sends it: the first family's package, or the
/// second family's plain G-code under the file name it is uploaded as. The job keeps Fab's
/// name `name` either way.
pub fn validate_job(file: Vec<u8>, profile: &MachineProfile, name: &str) -> Result<ValidatedJob, Refusal> {
    match profile.flavor {
        Flavor::Mask => validate(file, profile, name),
        Flavor::Named => validate_gcode(file, profile, &crate::job::plain_file_name(name)).map(|job| ValidatedJob { name: name.to_string(), ..job }),
    }
}

/// Checks a `.gcode.3mf` against the installed default-family profile.
pub fn validate(package: Vec<u8>, profile: &MachineProfile, name: &str) -> Result<ValidatedJob, Refusal> {
    let refuse = |rule, detail: String| Refusal(vec![Violation { line: 0, rule, detail }]);
    if profile.flavor != Flavor::Mask {
        return Err(refuse("family format", "the second family requires a plain G-code file".into()));
    }
    let mut cursor = std::io::Cursor::new(&package[..]);
    let dir = makepad_zip_file::zip_read_central_directory(&mut cursor).map_err(|_| refuse("package", "not a readable 3MF package".into()))?;
    let plates: Vec<_> = dir.file_headers.iter().filter(|h| h.file_name.starts_with("Metadata/plate_") && h.file_name.ends_with(".gcode")).collect();
    if plates.len() != 1 || plates[0].file_name != "Metadata/plate_1.gcode" {
        return Err(refuse("package", "the package must hold exactly one plate, Metadata/plate_1.gcode".into()));
    }
    let gcode = plates[0].extract(&mut cursor).map_err(|_| refuse("package", "cannot read the plate G-code".into()))?;
    if let Some(h) = dir.file_headers.iter().find(|h| h.file_name == "Metadata/plate_1.gcode.md5") {
        let md5 = h.extract(&mut cursor).map_err(|_| refuse("package", "cannot read the G-code checksum".into()))?;
        let want: String = makepad_network::digest::md5_hash(&gcode).iter().map(|b| format!("{b:02X}")).collect();
        if !String::from_utf8_lossy(&md5).trim().eq_ignore_ascii_case(&want) {
            return Err(refuse("package", "the G-code checksum does not match the G-code".into()));
        }
    }
    let text = std::str::from_utf8(&gcode).map_err(|_| refuse("package", "the G-code is not UTF-8 text".into()))?;
    let facts = check_gcode(text, profile)?;
    let config = header_config(text);
    let bytes: Arc<[u8]> = package.into();
    let md5 = makepad_network::digest::md5_hash(&bytes).iter().map(|b| format!("{b:02X}")).collect();
    Ok(ValidatedJob { sha256: makepad_network::digest::sha256_hash(&bytes), md5, bytes, facts, config, name: name.to_string() })
}

/// Checks a plain `.gcode` file against the installed second-family profile.
pub fn validate_gcode(gcode: Vec<u8>, profile: &MachineProfile, name: &str) -> Result<ValidatedJob, Refusal> {
    let refuse = |rule, detail: &str| Refusal(vec![Violation { line: 0, rule, detail: detail.into() }]);
    if profile.flavor != Flavor::Named {
        return Err(refuse("family format", "the default family requires a package"));
    }
    if name.is_empty() || name.len() > 200 || !name.ends_with(".gcode") || name.starts_with('.')
        || name.contains("..") || name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | '\'' | '"')) {
        return Err(refuse("file name", "a plain G-code name must be a safe basename of at most 200 bytes ending in .gcode"));
    }
    let text = std::str::from_utf8(&gcode).map_err(|_| refuse("G-code text", "the G-code is not UTF-8 text"))?;
    let facts = check_gcode(text, profile)?;
    let config = header_config(text);
    let bytes: Arc<[u8]> = gcode.into();
    let md5 = makepad_network::digest::md5_hash(&bytes).iter().map(|b| format!("{b:02X}")).collect();
    Ok(ValidatedJob { sha256: makepad_network::digest::sha256_hash(&bytes), md5, bytes, facts, config, name: name.to_string() })
}

/// The file's own settings block (`; key = value`, templates with `\n` escapes).
pub fn header_config(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if line.starts_with("; CONFIG_BLOCK_START") { inside = true; continue; }
        if line.starts_with("; CONFIG_BLOCK_END") { break; }
        if !inside { continue; }
        if let Some((k, v)) = line.strip_prefix("; ").and_then(|l| l.split_once(" = ")) {
            out.push((k.trim().to_string(), v.to_string()));
        } else if let Some(k) = line.strip_prefix("; ").and_then(|l| l.strip_suffix(" =")) {
            out.push((k.trim().to_string(), String::new()));
        }
    }
    out
}

/// The second family has one unambiguous trailer and at most one header.
fn named_blocks(text: &str) -> Result<(), Refusal> {
    let mut out = Vec::new();
    for (block, required, rule) in [("CONFIG", true, "config block"), ("HEADER", false, "header block")] {
        let (start, end) = (format!("; {block}_BLOCK_START"), format!("; {block}_BLOCK_END"));
        let (mut opened, mut count, mut closed) = (false, 0, false);
        let mut keys = std::collections::HashSet::new();
        for (i, line) in text.lines().enumerate() {
            let line = line.trim();
            let mut reason = None;
            if line.starts_with(&start) {
                if line != start || opened || count > 0 { reason = Some("repeated or malformed block start"); }
                opened = true;
                count += 1;
            } else if line.starts_with(&end) {
                if line != end || !opened { reason = Some("unpaired or malformed block end"); }
                opened = false;
                closed = true;
            } else if required && (opened || closed) && !code(line).is_empty() {
                reason = Some("the config block must follow the last command");
            } else if opened {
                let comment = line.strip_prefix(';').unwrap_or("").trim();
                let pair = comment.split_once('=').or_else(|| (!required).then(|| comment.split_once(':')).flatten());
                if let Some((key, _)) = pair {
                    if !keys.insert(key.trim().to_string()) { reason = Some("duplicate key"); }
                }
            }
            if let Some(why) = reason { out.push(Violation { line: i + 1, rule, detail: why.into() }); }
        }
        if opened || (required && count != 1) || count > 0 && !closed {
            out.push(Violation { line: 0, rule, detail: "missing or unclosed block".into() });
        }
    }
    if out.is_empty() { Ok(()) } else { Err(Refusal(out)) }
}

fn usage(text: &str, unit: &str, filaments: usize) -> Option<Vec<f64>> {
    let key = format!("filament used [{unit}]");
    let mut lines = text.lines().filter_map(|l| l.trim().strip_prefix(';')?.trim().split_once('='))
        .filter(|(k, _)| k.trim() == key);
    let (_, value) = lines.next()?;
    if lines.next().is_some() { return None; }
    let values = value.split(',').map(|v| finite(v).filter(|v| *v >= 0.0)).collect::<Option<Vec<_>>>()?;
    (values.len() == filaments).then_some(values)
}

fn unescape(v: &str) -> String {
    let mut out = String::new();
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() { Some('n') => out.push('\n'), Some('r') => {}, Some('t') => out.push('\t'), Some('\\') => out.push('\\'), Some('"') => out.push('"'), Some(o) => { out.push('\\'); out.push(o) } None => out.push('\\') }
        } else { out.push(c); }
    }
    out
}

/// A finite setting or header number.
fn finite(v: &str) -> Option<f64> { v.trim().parse::<f64>().ok().filter(|v| v.is_finite()) }

fn nums(v: &str) -> Vec<f64> { v.split(',').map(finite).collect::<Option<Vec<_>>>().unwrap_or_default() }

fn close(a: &[f64], b: &[f64]) -> bool { a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6) }

fn areas(v: &str) -> Vec<Vec<[f64; 2]>> {
    v.split('#').map(|s| { let a = parse_points(s); (!a.is_empty()).then_some(a) }).collect::<Option<Vec<_>>>().unwrap_or_default()
}

/// Each nozzle's printable area and height. A second-family printer whose heads all reach the
/// whole bed lists none (its height list is empty or a single 0), and neither does a
/// single-nozzle printer of the first family (its profile leaves the per-nozzle keys at their
/// defaults): each nozzle then has the printable area and height, as the slicer reads them. A
/// first-family printer with several nozzles must state each one's area, and every other list
/// is kept as stated.
fn per_nozzle(flavor: Flavor, areas: Vec<Vec<[f64; 2]>>, heights: Vec<f64>, area: &[[f64; 2]], height: f64, nozzles: usize) -> (Vec<Vec<[f64; 2]>>, Vec<f64>) {
    let whole_bed = flavor == Flavor::Named || nozzles == 1;
    if !whole_bed || !areas.is_empty() || !(heights.is_empty() || heights == [0.0]) || area.is_empty() {
        return (areas, heights);
    }
    (vec![area.to_vec(); nozzles], vec![height; nozzles])
}

fn same_area(a: &[[f64; 2]], b: &[[f64; 2]]) -> bool { a.len() == b.len() && a.iter().zip(b).all(|(p, q)| (p[0] - q[0]).abs() < 1e-6 && (p[1] - q[1]).abs() < 1e-6) }

fn bounds(area: &[[f64; 2]]) -> [f64; 4] {
    area.iter().fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |b, p| [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])])
}

fn inside(area: &[[f64; 2]], x: f64, y: f64) -> bool {
    // Convex or not: even-odd ray test, with the boundary inside.
    if !x.is_finite() || !y.is_finite() || area.is_empty() { return false; }
    let b = bounds(area);
    if x < b[0] - 1e-6 || x > b[2] + 1e-6 || y < b[1] - 1e-6 || y > b[3] + 1e-6 { return false; }
    if area.len() == 4 && b[0] < b[2] && b[1] < b[3]
        && [[b[0], b[1]], [b[0], b[3]], [b[2], b[1]], [b[2], b[3]]].iter().all(|p| area.iter().filter(|q| *q == p).count() == 1) { return true; }
    let mut odd = false;
    for i in 0..area.len() {
        let (p, q) = (area[i], area[(i + 1) % area.len()]);
        if (p[1] > y) != (q[1] > y) && x < (q[0] - p[0]) * (y - p[1]) / (q[1] - p[1]) + p[0] { odd = !odd; }
    }
    odd
}

const FIRST_LAYER: [f64; 2] = [0.1, 0.4];
const MIN_Z: f64 = 0.1 - 1e-6;
const MAX_LIFT: f64 = 3.0;
/// The longest dwell in Fab's toolpath, in seconds.
const MAX_DWELL_S: f64 = 10.0;
/// The bed temperature ceiling, in °C.
const MAX_BED_TEMP: f64 = 120.0;
/// How far an arc's end may be off the radius it starts on (mm): the 3-decimal words leave
/// ~0.002 mm (the references and Fab's slices).
const ARC_RADIUS_SLACK: f64 = 0.02;
/// An arc of more than half a turn is the wrong way round when its filament per swept mm is under
/// this share of the bead while the other way round's is within it (and its inverse).
const ARC_WAY: f64 = 0.8;
/// Monotone ramps: the steepest pitch, and how far below the print a ramp may start (the
/// tip zone: a scarf seam starts a layer below the walls printed before it).
const RAMP_SLOPE_DEG: f64 = 5.0;
const RAMP_DIP: f64 = 0.3;
/// Flow sanity catches gross errors (a wrong flow compensation), not tuning. The largest bead
/// cross-section, in nozzle diameters squared (tower ramming lays 1.0 mm lines on a 0.4 nozzle),
/// and the largest multiple of the filament's volumetric speed (the vendor's own slices reach
/// 1.5x on the golden cases, Fab's 2.0x).
const FLOW_MAX_AREA: f64 = 3.0;
const FLOW_MAX_VOLUMETRIC: f64 = 2.5;
/// How far a first-layer extrusion may reach past the bed probe's area (mm): the vendor probes the regions, its
/// lines run inside them.
const PROBE_SLACK: f64 = 0.5;
/// Commands Fab's own toolpath may use.
const TOOLPATH_COMMANDS: &[&str] = &[
    "G0", "G1", "G2", "G3", "G4", "G17", "G21", "G90", "G91", "G92", "M73", "M82", "M83", "M104", "M106", "M107", "M109", "M140", "M190",
    "M201", "M203", "M204", "M205", "M400", "M624", "M625", "M632", "M633", "M900", "M981", "M1003",
];

const NAMED_DENIED: &[&str] = &[
    "SAVE_CONFIG", "FIRMWARE_RESTART", "RESTART", "M112", "FORCE_MOVE", "SET_KINEMATIC_POSITION",
    "SDCARD_PRINT_FILE", "SDCARD_PRINT_FILE_WITH_PARAMETERS", "SDCARD_RESET_FILE",
];

#[derive(Clone, Copy)]
struct Axis { pos: Option<f64>, shift: f64 }

struct Sim<'a> {
    profile: &'a MachineProfile,
    relative: bool,
    axes: [Axis; 3],
    homed_z: bool,
    homed_any: bool,
    filament: Option<usize>,
    filament_map: Vec<usize>,
    objects: std::collections::HashSet<String>,
    open_object: Option<String>,
    other_command: bool,
    first_z: Option<f64>,
    layer_top: f64,
    max_z: f64,
    moves: usize,
    used: Vec<usize>,
    first_layer: Vec<[f64; 4]>,
    first_layer_lines: Vec<usize>,
    /// The current line is support (a `; FEATURE:` comment says so).
    support: bool,
    /// The current line's `; FEATURE:`, `; LINE_WIDTH:` and `; LAYER_HEIGHT:`.
    feature: String,
    width: Option<f64>,
    height: Option<f64>,
    /// Per filament (the file's `filament_diameter`).
    filament_diameters: Vec<f64>,
    /// What each height has printed so far (the crossing rule).
    printed: paths::Printed,
    /// M82 (absolute E) and the E position it counts from.
    e_absolute: bool,
    e_pos: f64,
    /// Every first-layer extrusion of the toolpath (support and prime tower too): min x, min y, max x, max y.
    first_extent: Option<[f64; 4]>,
    /// The bed probe areas of the start sequence (`G29 ... X Y I J`): line, min x, min y, max x, max y.
    probes: Vec<(usize, [f64; 4])>,
    grid: std::collections::HashMap<(i32, i32), Vec<usize>>,
    out: Vec<Violation>,
    /// How many violations to keep.
    cap: usize,
    /// The modal feed (mm/min).
    feed: Option<f64>,
    /// Whether the installed head allows non-planar moves (why not, if it does not).
    head_ok: Result<(), String>,
    /// The printed height field (only with a usable head).
    heights: Option<nonplanar::HeightMap>,
    /// The previous extrusion of the current run: Z speed share (sin of the pitch), speed (mm/s), non-planar.
    run_prev: Option<(f64, f64, bool)>,
    nonplanar_moves: usize,
    max_pitch: f64,
    min_clear: Option<f64>,
    ramp_moves: usize,
    /// The current run's own trail, joined to the height field once the tip is
    /// nonplanar::TRAIL behind it (the tip shapes its own fresh bead).
    trail: std::collections::VecDeque<([f64; 3], [f64; 3])>,
    trail_len: f64,
}

/// One uppercase command, stopping before its first word.
fn split_cmd(code: &str) -> Result<(&str, &str), String> {
    let b = code.as_bytes();
    if !matches!(b.first(), Some(b'G' | b'M' | b'T')) { return Err("expected an uppercase G, M or T command".into()); }
    let mut i = 1;
    while i < b.len() && b[i].is_ascii_digit() { i += 1; }
    if i == 1 { return Err("command has no digits".into()); }
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() { i += 1; }
        if i == start { return Err("command fraction has no digits".into()); }
    }
    Ok((&code[..i], &code[i..]))
}

/// Ordinary compact G-code still has a numeric command followed by its words.
fn second_split(code: &str) -> (&str, &str) {
    let first = code.split_ascii_whitespace().next().unwrap_or("");
    if first.as_bytes().first().is_some_and(|b| b"GMTgmt".contains(b))
        && first.as_bytes().get(1).is_some_and(u8::is_ascii_digit) {
        let mut end = 1;
        while code.as_bytes().get(end).is_some_and(|b| b.is_ascii_digit() || *b == b'.') { end += 1; }
        (&code[..end], &code[end..])
    } else { code.split_once(char::is_whitespace).unwrap_or((code, "")) }
}

/// Per letter: absent, bare flag, or finite decimal value.
#[derive(Clone, Debug, Default)]
struct Words { values: [Option<Option<f64>>; 26] }

impl Words {
    fn get(&self, letter: u8) -> Option<f64> { self.values[(letter - b'A') as usize].flatten() }
    fn has(&self, letter: u8) -> bool { self.values[(letter - b'A') as usize].is_some() }
}

/// Decimal words only; an adjacent E is ambiguous to firmware.
fn parse_words(rest: &str, flags: bool) -> Result<Words, String> {
    let b = rest.as_bytes();
    let mut words = Words::default();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_whitespace() { i += 1; continue; }
        let letter = b[i];
        if !letter.is_ascii_uppercase() { return Err(format!("expected an uppercase word at {:?}", &rest[i..])); }
        let slot = &mut words.values[(letter - b'A') as usize];
        if slot.is_some() { return Err(format!("repeated {} word", letter as char)); }
        i += 1;
        let start = i;
        if i < b.len() && matches!(b[i], b'+' | b'-') { i += 1; }
        let digits = i;
        while i < b.len() && b[i].is_ascii_digit() { i += 1; }
        let mut count = i - digits;
        if b.get(i) == Some(&b'.') {
            i += 1;
            let fraction = i;
            while i < b.len() && b[i].is_ascii_digit() { i += 1; }
            count += i - fraction;
        }
        if count == 0 {
            if !flags || i != start { return Err(format!("{} needs a decimal number", letter as char)); }
            *slot = Some(None);
            continue;
        }
        if matches!(b.get(i), Some(b'E' | b'e')) { return Err("an exponent or a number directly followed by E is not allowed".into()); }
        let v = rest[start..i].parse::<f64>().map_err(|_| format!("invalid {} value", letter as char))?;
        if !v.is_finite() { return Err(format!("non-finite {} value", letter as char)); }
        *slot = Some(Some(v));
    }
    Ok(words)
}

/// Only the named commands below accept key/value words; never reinterpret ordinary G-code.
fn named_words<'a>(rest: &'a str, allowed: &[&'a str]) -> Result<Vec<(&'a str, &'a str)>, String> {
    let mut words = Vec::new();
    for word in rest.split_ascii_whitespace() {
        let (key, value) = word.split_once('=').ok_or_else(|| format!("expected KEY=VALUE, got {word}"))?;
        let key = allowed.iter().copied().find(|k| k.eq_ignore_ascii_case(key)).unwrap_or(key);
        if !allowed.contains(&key) || value.is_empty() || words.iter().any(|(k, _)| *k == key) {
            return Err(format!("unknown, empty or repeated word {word}"));
        }
        words.push((key, value));
    }
    Ok(words)
}

fn named_number(value: &str) -> Option<f64> {
    let words = parse_words(&format!("S{value}"), false).ok()?;
    if words.values.iter().flatten().count() != 1 { return None; }
    words.get(b'S')
}

fn object_point(value: &str) -> Option<[f64; 2]> {
    let (x, y) = value.split_once(',')?;
    Some([named_number(x)?, named_number(y)?])
}

fn object_polygon(value: &str) -> Option<Vec<[f64; 2]>> {
    let inner = value.strip_prefix("[[")?.strip_suffix("]]")?;
    let points = inner.split("],[").map(object_point).collect::<Option<Vec<_>>>()?;
    (points.len() >= 3).then_some(points)
}

/// The words emitted for each ordinary toolpath command.
fn allowed_words(cmd: &str) -> &[u8] {
    match cmd {
        "G0" | "G1" => b"XYZEF", "G2" | "G3" => b"XYZEFIJPR",
        "G4" => b"PS", "G92" => b"XYZE", "M73" => b"PREL", "M104" | "M109" => b"STN",
        "M140" | "M190" => b"S", "M106" => b"PS", "M107" => b"P",
        "M201" | "M203" | "M205" => b"XYZE", "M204" => b"SPRT", "M981" => b"SP", "M1003" => b"S",
        _ => b"",
    }
}

/// Template commands whose words affect the check.
fn reads_words(cmd: &str) -> bool {
    cmd.starts_with('T') || matches!(cmd, "G0" | "G1" | "G2" | "G3" | "G28" | "G90" | "G91" | "G92" | "M82" | "M83" | "G29" | "M9711"
        | "M104" | "M109" | "M140" | "M190" | "M141" | "M191" | "G150" | "G383" | "G383.3" | "M620.17" | "M620.10"
        | "M201" | "M203" | "M204" | "M205" | "G4" | "M106" | "M107" | "M73" | "M981" | "M1003")
}

impl<'a> Sim<'a> {
    fn bad(&mut self, line: usize, rule: &'static str, detail: String) {
        if self.out.len() < self.cap { self.out.push(Violation { line, rule, detail }); }
    }

    fn second_command(&mut self, c: &str, n: usize, trusted: bool) {
        let upper = c.to_ascii_uppercase();
        let (cmd, rest) = second_split(&upper);
        if cmd != "EXCLUDE_OBJECT_DEFINE" { self.other_command = true; }
        if NAMED_DENIED.contains(&cmd) { self.bad(n, "denied command", format!("{cmd} is forbidden anywhere")); return; }
        if matches!(cmd, "SET_VELOCITY_LIMIT" | "SET_PRESSURE_ADVANCE" | "EXCLUDE_OBJECT_DEFINE" | "EXCLUDE_OBJECT_START" | "EXCLUDE_OBJECT_END") {
            self.named_command(c, cmd, rest, n);
            return;
        }
        // The speed-factor guard around a tool change: back up, run at 100 %, restore. No other form.
        if cmd == "M220" {
            if !matches!(rest.trim(), "B" | "R" | "S100") { self.bad(n, "words", format!("{c}: expected M220 B, M220 S100 or M220 R")); }
            return;
        }
        // The pause for items placed into their parts (`; FAB_PAUSE:`): the printer's own PAUSE,
        // without words, which the person resumes. Where it leaves the head is the printer's.
        if cmd == "PAUSE" && rest.trim().is_empty() {
            self.axes.iter_mut().for_each(|a| a.pos = None);
            return;
        }
        let tool = cmd.strip_prefix('T').is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        let known = TOOLPATH_COMMANDS.contains(&cmd) || tool || (trusted && reads_words(cmd) && !cmd.starts_with('T'));
        if !known {
            if trusted {
                // Only a proven literal macro reaches here. Its final position is unknown.
                self.axes.iter_mut().for_each(|a| a.pos = None);
            } else {
                let rule = if split_cmd(cmd).is_ok() { "command" } else { "words" };
                self.bad(n, rule, format!("{cmd} is not an allowed command or a literal template macro"));
            }
            return;
        }
        match cmd {
            "M900" => {
                let mut words = rest.split_ascii_whitespace();
                let k = words.next().and_then(|w| w.strip_prefix('K'));
                if !k.is_some_and(|k| template::decimal(k) && finite(k).is_some_and(|v| (0.0..=0.5).contains(&v))) || words.next().is_some() {
                    self.bad(n, "pressure advance", format!("{c}: expected only K in 0..=0.5"));
                }
                return;
            }
            "M624" => {
                let words: Vec<_> = c.split_ascii_whitespace().skip(1).collect();
                if words.len() != 1 || !words[0].bytes().all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b)) {
                    self.bad(n, "words", format!("{c}: expected a single mask"));
                }
                return;
            }
            "M632" => {
                let words: Vec<_> = rest.split_ascii_whitespace().collect();
                if words.len() != 2 || words.get(1) != Some(&"W") || words.first().and_then(|w| w.strip_prefix('S'))
                    .and_then(|f| f.parse::<usize>().ok()).is_none_or(|f| f >= self.filament_map.len()) {
                    self.bad(n, "words", format!("{c}: expected S<job filament> W"));
                }
                return;
            }
            _ => {}
        }
        let flags = trusted && cmd == "G28";
        let w = match parse_words(rest, flags) { Ok(w) => w, Err(why) => { self.bad(n, "words", format!("{c}: {why}")); return; } };
        let allowed = match cmd {
            // G28 may name the point Z homes at (I, J): inside the printable area.
            "G28" => b"XYZIJ".as_slice(),
            "M104" | "M109" => b"STA".as_slice(),
            "M141" | "M191" => b"S".as_slice(),
            _ => allowed_words(cmd),
        };
        if cmd == "G28" && (w.has(b'I') || w.has(b'J')) {
            match (w.get(b'I'), w.get(b'J')) {
                (Some(x), Some(y)) if inside(&self.profile.printable_area, x, y) => {}
                _ => self.bad(n, "outside the build volume", format!("{c}: I and J must name a point inside the printable area")),
            }
        }
        for a in b'A'..=b'Z' {
            if w.has(a) && !allowed.contains(&a) { self.bad(n, "words", format!("{cmd} does not take {}", a as char)); }
        }
        self.values(c, cmd, &w, n, true);
        self.modal(cmd, &w, n, false);
        if matches!(cmd, "G0" | "G1" | "G2" | "G3") { self.motion(c, cmd, &w, n, true, true); }
        else if moves_head(cmd) { self.axes.iter_mut().for_each(|a| a.pos = None); }
    }

    fn named_command(&mut self, c: &str, cmd: &str, rest: &str, n: usize) {
        let keys: &[&str] = match cmd {
            "SET_VELOCITY_LIMIT" => &["ACCEL", "ACCEL_TO_DECEL"],
            "SET_PRESSURE_ADVANCE" => &["ADVANCE"],
            "EXCLUDE_OBJECT_DEFINE" => &["NAME", "CENTER", "POLYGON"],
            _ => &["NAME"],
        };
        let words = match named_words(rest, keys) {
            Ok(words) => words,
            Err(why) => { self.bad(n, "words", format!("{c}: {why}")); return; }
        };
        let get = |key| words.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        if cmd == "SET_VELOCITY_LIMIT" {
            let limit = self.profile.limits.move_acceleration.or_else(|| {
                let [Some(x), Some(y), ..] = self.profile.limits.acceleration else { return None };
                (x.is_finite() && y.is_finite()).then_some(x.min(y))
            });
            let a = get("ACCEL").and_then(named_number);
            if a.is_none_or(|a| a <= 0.0 || limit.is_none_or(|max| !max.is_finite() || a > max)) {
                self.bad(n, "acceleration", format!("{c}: ACCEL must be positive and within the machine's move acceleration limit ({limit:?})"));
            }
            if let Some(b) = get("ACCEL_TO_DECEL") {
                if named_number(b).is_none_or(|b| b <= 0.0 || a.is_none_or(|a| b > a)) {
                    self.bad(n, "acceleration to deceleration", format!("{c}: ACCEL_TO_DECEL must be positive and no greater than ACCEL"));
                }
            }
            return;
        }
        if cmd == "SET_PRESSURE_ADVANCE" {
            // The same bound as the default family's M900 K.
            if get("ADVANCE").is_none_or(|v| !template::decimal(v) || named_number(v).is_none_or(|v| !(0.0..=0.5).contains(&v))) {
                self.bad(n, "pressure advance", format!("{c}: expected only ADVANCE in 0..=0.5"));
            }
            return;
        }
        let Some(name) = get("NAME").filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))) else {
            self.bad(n, "object name", format!("{c}: NAME must use only letters, digits, _, - or ."));
            return;
        };
        let name = name.to_ascii_uppercase();
        let name = name.as_str();
        match cmd {
            "EXCLUDE_OBJECT_DEFINE" => {
                if self.other_command { self.bad(n, "object definition order", format!("{c}: a definition after another command")); }
                if !self.objects.insert(name.into()) { self.bad(n, "object name", format!("{c}: repeated definition of {name}")); }
                let center = get("CENTER").and_then(object_point);
                let polygon = get("POLYGON").and_then(object_polygon);
                match (center, polygon) {
                    (Some(center), Some(polygon)) => {
                        if std::iter::once(&center).chain(&polygon).any(|p| !inside(&self.profile.printable_area, p[0], p[1])) {
                            self.bad(n, "object area", format!("{c}: every center and polygon point must be inside the printable area"));
                        }
                    }
                    _ => self.bad(n, "object geometry", format!("{c}: expected CENTER=x,y and POLYGON=[[x,y],...] with at least three points")),
                }
            }
            "EXCLUDE_OBJECT_START" => {
                if !self.objects.contains(name) { self.bad(n, "object defined", format!("{c}: {name} is not defined")); }
                if self.open_object.is_some() { self.bad(n, "object nesting", format!("{c}: an object is already open")); }
                else { self.open_object = Some(name.into()); }
            }
            "EXCLUDE_OBJECT_END" => {
                if !self.objects.contains(name) { self.bad(n, "object defined", format!("{c}: {name} is not defined")); }
                if self.open_object.as_deref() != Some(name) { self.bad(n, "object pairing", format!("{c}: END must name the open object")); }
                else { self.open_object = None; }
            }
            _ => unreachable!(),
        }
    }

    /// The physical target of a move word: absolute or relative, through G92's shift.
    fn target(&self, axis: usize, value: Option<f64>) -> Result<Option<f64>, ()> {
        let a = self.axes[axis];
        match value {
            None => Ok(a.pos),
            Some(v) if self.relative => a.pos.map(|p| Some(p + v)).ok_or(()),
            Some(v) => Ok(Some(v - a.shift)),
        }
    }

    fn nozzle(&self) -> Option<usize> {
        let f = self.filament?;
        let map = *self.filament_map.get(f).or(self.filament_map.first())?;
        map.checked_sub(1)
    }

    /// The hottest range for the targeted nozzle's job filaments, capped at 350 °C.
    fn nozzle_limit(&self, nozzle: Option<usize>) -> Option<f64> {
        let mapped = nozzle.is_some_and(|e| self.filament_map.iter().any(|m| *m == e + 1));
        self.filament_map.iter().enumerate().filter(|(_, m)| !mapped || Some(**m - 1) == nozzle)
            .filter_map(|(f, _)| self.profile.nozzle_temp_ranges.get(f).map(|r| r[1])).filter(|v| v.is_finite()).reduce(f64::max).map(|v| v.min(350.0))
    }

    /// A bounded non-negative value, with no fallback for an unknown limit.
    fn bounded(&mut self, n: usize, c: &str, letter: u8, value: Option<f64>, max: Option<f64>, rule: &'static str) {
        if let Some(v) = value {
            if !v.is_finite() || v < 0.0 || max.is_none_or(|m| !m.is_finite() || v > m) {
                self.bad(n, rule, format!("{c}: {}{v} exceeds its bounds ({max:?})", letter as char));
            }
        }
    }

    /// A heater's value against the job filament ranges.
    fn nozzle_temp(&mut self, n: usize, c: &str, letter: u8, value: Option<f64>, nozzle: Option<usize>) {
        let max = self.nozzle_limit(nozzle);
        if value.is_some_and(|v| v > 0.0) && max.is_none() {
            self.bad(n, "temperature", format!("{c}: the filament's temperature range is not known"));
        } else { self.bounded(n, c, letter, value, Some(max.unwrap_or(0.0)), "temperature"); }
    }

    /// Value checks shared by own lines and strict template lines.
    fn values(&mut self, c: &str, cmd: &str, w: &Words, n: usize, own: bool) {
        match cmd {
            "M104" | "M109" => {
                if self.profile.flavor == Flavor::Named && w.has(b'A') {
                    // A0 addresses a physical head, past the job's tool map, which only the
                    // start knows: so it may only switch a head off.
                    if w.get(b'A') != Some(0.0) { self.bad(n, "words", format!("{c}: A must be 0")); }
                    if !w.get(b'T').is_some_and(|t| t >= 0.0 && t.fract() == 0.0 && t < self.profile.nozzle_diameters.len() as f64) {
                        self.bad(n, "words", format!("{c}: with A0, T must name a physical head"));
                    }
                    if w.get(b'S') != Some(0.0) { self.bad(n, "temperature", format!("{c}: a head addressed physically may only be switched off (S0)")); }
                    if w.has(b'N') { self.bad(n, "words", format!("{c}: the second family takes only S, T and A")); }
                    return;
                }
                if self.profile.flavor == Flavor::Named {
                    let filament = if w.has(b'T') {
                        w.get(b'T').filter(|t| *t >= 0.0 && t.fract() == 0.0 && *t < self.filament_map.len() as f64).map(|t| t as usize)
                    } else { self.filament };
                    if filament.is_none() || filament.is_some_and(|f| f >= self.filament_map.len()) {
                        self.bad(n, "job filament", format!("{c}: T must name a job filament"));
                    }
                    let max = filament.and_then(|f| self.profile.nozzle_temp_ranges.get(f)).map(|r| r[1].min(350.0));
                    if !w.has(b'S') { self.bad(n, "words", format!("{c}: S temperature is required")); }
                    self.bounded(n, c, b'S', w.get(b'S'), max, "temperature");
                    if w.has(b'N') { self.bad(n, "words", format!("{c}: the second family takes only S and T")); }
                    return;
                }
                let nozzle = if w.has(b'T') {
                    let e = w.get(b'T').and_then(|t| self.profile.physical_extruder_map.iter().position(|p| t == *p as f64));
                    if e.is_none() { self.bad(n, "words", format!("{c}: T must name a physical extruder")); }
                    e
                } else { self.nozzle() };
                if w.has(b'N') && w.get(b'N') != Some(0.0) { self.bad(n, "words", format!("{c}: N must be 0")); }
                self.nozzle_temp(n, c, b'S', w.get(b'S'), nozzle);
            }
            "G150" | "G383" | "G383.3" => self.nozzle_temp(n, c, b'T', w.get(b'T'), None),
            "M620.17" => self.nozzle_temp(n, c, b'S', w.get(b'S'), None),
            "M620.10" => for a in b"TP" { self.nozzle_temp(n, c, *a, w.get(*a), None); },
            "M140" | "M190" => self.bounded(n, c, b'S', w.get(b'S'), Some(MAX_BED_TEMP), "temperature"),
            "M141" | "M191" => self.bounded(n, c, b'S', w.get(b'S'), Some(self.profile.chamber_temps.iter().copied().filter(|v| v.is_finite()).fold(0.0, f64::max)), "temperature"),
            "M201" | "M203" | "M205" => {
                let limits = match cmd { "M201" => self.profile.limits.acceleration, "M203" => self.profile.limits.speed, _ => self.profile.limits.jerk };
                for (i, a) in b"XYZE".iter().enumerate() { self.bounded(n, c, *a, w.get(*a), limits[i], "limit"); }
            }
            "M204" => for a in b"SPRT" { self.bounded(n, c, *a, w.get(*a), self.profile.limits.move_acceleration, "limit"); },
            _ => {}
        }
        if !own { return; }
        match cmd {
            "G4" => {
                self.bounded(n, c, b'P', w.get(b'P'), Some(MAX_DWELL_S * 1000.0), "limit");
                self.bounded(n, c, b'S', w.get(b'S'), Some(MAX_DWELL_S), "limit");
            }
            "M106" | "M107" => {
                self.bounded(n, c, b'P', w.get(b'P'), Some(3.0), "limit");
                if w.get(b'P').is_some_and(|v| v.fract() != 0.0) { self.bad(n, "limit", format!("{c}: P must be an integer")); }
                if cmd == "M106" { self.bounded(n, c, b'S', w.get(b'S'), Some(255.0), "limit"); }
            }
            "M73" => for a in b"PREL" { self.bounded(n, c, *a, w.get(*a), Some(if *a == b'P' { 100.0 } else { f64::MAX }), "limit"); },
            "M981" | "M1003" => {
                if w.get(b'S').is_some_and(|v| v != 0.0 && v != 1.0) { self.bad(n, "limit", format!("{c}: S must be 0 or 1")); }
                if cmd == "M981" { self.bounded(n, c, b'P', w.get(b'P'), Some(f64::MAX), "limit"); }
            }
            "G0" | "G1" | "G2" | "G3" => if w.get(b'F').is_some_and(|v| v <= 0.0) { self.bad(n, "limit", format!("{c}: F must be positive")); },
            _ => {}
        }
    }

    /// The state effects of any line (trusted or not).
    fn modal(&mut self, cmd: &str, w: &Words, n: usize, trusted: bool) {
        match cmd {
            "G90" => self.relative = false,
            "G91" => self.relative = true,
            "M82" => self.e_absolute = true,
            "M83" => self.e_absolute = false,
            "G28" => {
                let axes = [b'X', b'Y', b'Z'];
                let named: Vec<u8> = axes.iter().copied().filter(|a| w.has(*a)).collect();
                let all = named.is_empty();
                self.homed_any = true;
                if all || named.contains(&b'Z') { self.homed_z = true; }
                for (i, a) in axes.iter().enumerate() {
                    if all || named.contains(a) { self.axes[i] = Axis { pos: None, shift: 0.0 }; }
                }
            }
            "G92" => {
                if let Some(v) = w.get(b'E') { self.e_pos = v; }
                for (i, a) in b"XYZ".iter().enumerate() {
                    if let Some(v) = w.get(*a) {
                        if !trusted { self.bad(n, "position reset", format!("G92 {} outside the printer's own sequences", *a as char)); }
                        // The logical position becomes v at the physical position.
                        match self.axes[i].pos { Some(p) => self.axes[i].shift = v - p, None => self.axes[i].pos = None }
                    }
                }
            }
            _ if cmd.starts_with('T') && (self.profile.flavor != Flavor::Named || cmd[1..].bytes().all(|b| b.is_ascii_digit())) => {
                if self.profile.flavor == Flavor::Named {
                    match cmd[1..].parse::<usize>() {
                        Ok(t) if t < self.filament_map.len() => self.filament = Some(t),
                        _ => self.bad(n, "job filament", format!("{cmd}: T must name a job filament")),
                    }
                    // A tool select can park the head; the next move must establish its position.
                    self.axes.iter_mut().for_each(|a| a.pos = None);
                } else if let Ok(t) = cmd[1..].parse::<usize>() { if t < 255 { self.filament = Some(t); } }
            }
            _ => {}
        }
    }

    /// The arc a G2/G3 draws from `from`, as the firmware computes it from its words.
    fn arc(&self, from: [f64; 2], to: [f64; 2], w: &Words, clockwise: bool) -> paths::Arc {
        paths::Arc::new(from, to, [w.get(b'I').unwrap_or(0.0), w.get(b'J').unwrap_or(0.0)], clockwise)
    }

    /// One radius for strict arcs; own arcs also need the feature's sweep and bead flow.
    fn check_arc(&mut self, n: usize, c: &str, w: &Words, arc: &paths::Arc, e: f64, own: bool) {
        if w.has(b'R') {
            self.bad(n, "arc geometry", format!("{c}: radius-form arcs are not checked; Fab writes I J"));
            return;
        }
        if arc.r < 1e-3 {
            self.bad(n, "arc geometry", format!("{c}: no centre offset"));
            return;
        }
        if (arc.r - arc.r_end).abs() > ARC_RADIUS_SLACK {
            self.bad(n, "arc geometry", format!("{c}: starts on radius {:.3} mm, ends on {:.3} mm", arc.r, arc.r_end));
        }
        if !own || e <= 0.0 {
            return;
        }
        let class = paths::class(&self.feature);
        let deg = arc.sweep.to_degrees();
        if deg > class.max_sweep_deg() {
            self.bad(n, "arc sweep", format!("{c}: sweeps {deg:.1}° ({}), more than a {} arc may ({:.0}°)", if arc.clockwise { "clockwise" } else { "counter-clockwise" }, self.feature.trim(), class.max_sweep_deg()));
        }
        let (Some(w), Some(h)) = (self.width, self.height) else {
            self.bad(n, "arc flow", format!("{c}: an extruding arc without a line width and layer height"));
            return;
        };
        let d = self.filament.and_then(|f| self.filament_diameters.get(f).copied()).or(self.filament_diameters.first().copied()).unwrap_or(1.75);
        let want = paths::bead_e_per_mm(w, h, d);
        let flow = e / arc.len() / want;
        // More than half a turn, but the filament is the short way round's: the direction letter
        // is wrong (FABBENCH: 13 to 1,103 such arcs per Fab file, none in the vendor's). Within
        // ARC_WAY of 180° the filament cannot tell the two ways apart (beads run 0.94..1.37 of
        // nominal; the vendor writes real 180-184° arcs), so the long way must be clearly short of
        // filament while the short way fits.
        if arc.sweep > std::f64::consts::PI {
            let short = e / (arc.r * (std::f64::consts::TAU - arc.sweep)).max(1e-9) / want;
            if flow < ARC_WAY && (ARC_WAY..=1.0 / ARC_WAY).contains(&short) {
                self.bad(n, "arc direction", format!("{c}: sweeps {deg:.1}° but its E {e} is the {:.1}° the other way round's ({:.0}% of the bead there, {:.0}% over the sweep)", 360.0 - deg, short * 100.0, flow * 100.0));
            }
        }
        if !(paths::ARC_FLOW[0]..=paths::ARC_FLOW[1]).contains(&flow) {
            self.bad(n, "arc flow", format!("{c}: {:.1} mm swept ({deg:.1}°) with E {e} is {:.0}% of the {} bead's filament: the sweep is not the path it extrudes", arc.len(), flow * 100.0, self.feature.trim()));
        }
    }

    fn crossed(&mut self, x: paths::Crossing) {
        self.bad(x.line, "crossing", format!("the extrusion runs {:.1} mm over beads this layer already printed (line {}), at their height", x.length, x.over_line));
    }

    fn crosses_first_layer(&self, a: [f64; 2], b: [f64; 2], width: f64) -> Option<usize> {
        let cell = 2.0;
        let (x0, x1) = (a[0].min(b[0]), a[0].max(b[0]));
        let (y0, y1) = (a[1].min(b[1]), a[1].max(b[1]));
        let mut seen = std::collections::HashSet::new();
        for gx in ((x0 - width) / cell).floor() as i32..=((x1 + width) / cell).floor() as i32 {
            for gy in ((y0 - width) / cell).floor() as i32..=((y1 + width) / cell).floor() as i32 {
                for &s in self.grid.get(&(gx, gy)).into_iter().flatten() {
                    if !seen.insert(s) { continue; }
                    let seg = self.first_layer[s];
                    // Ignore the segment the travel leaves from or arrives at.
                    let near = |p: [f64; 2]| (p[0] - seg[0]).hypot(p[1] - seg[1]) < width || (p[0] - seg[2]).hypot(p[1] - seg[3]) < width;
                    if near(a) || near(b) { continue; }
                    if segment_distance(a, b, [seg[0], seg[1]], [seg[2], seg[3]]) < width * 0.5 { return Some(self.first_layer_lines[s]); }
                }
            }
        }
        None
    }

    fn add_first_layer(&mut self, a: [f64; 2], b: [f64; 2], line: usize) {
        let i = self.first_layer.len();
        self.first_layer.push([a[0], a[1], b[0], b[1]]);
        self.first_layer_lines.push(line);
        let cell = 2.0;
        for gx in (a[0].min(b[0]) / cell).floor() as i32..=(a[0].max(b[0]) / cell).floor() as i32 {
            for gy in (a[1].min(b[1]) / cell).floor() as i32..=(a[1].max(b[1]) / cell).floor() as i32 {
                self.grid.entry((gx, gy)).or_default().push(i);
            }
        }
    }

    /// Whether a rising extrusion is a monotone ramp: straight, never dropping, starting at
    /// most RAMP_DIP below everything printed, at most RAMP_SLOPE_DEG, inside the Z speed.
    /// The head is then above all printed material but the tip zone, as on planar layers.
    fn ramp(&self, cmd: &str, e: f64, from: [Option<f64>; 3], to: [Option<f64>; 3]) -> Result<(), String> {
        if cmd != "G1" { return Err("not a straight move".into()); }
        if e <= 0.0 { return Err("it pulls filament".into()); }
        let (Some(fx), Some(fy), Some(fz), Some(x), Some(y), Some(z)) = (from[0], from[1], from[2], to[0], to[1], to[2]) else { return Err("unknown position".into()) };
        if z < fz { return Err("Z drops".into()); }
        let d = self.profile.nozzle_diameters.get(self.nozzle().unwrap_or(0)).copied().unwrap_or(0.4);
        let dip = (0.75 * d).min(RAMP_DIP);
        if fz < self.layer_top - dip - 1e-9 { return Err(format!("starts {:.2} mm below the print", self.layer_top - fz)); }
        let (lxy, dz) = ((x - fx).hypot(y - fy), z - fz);
        let pitch = dz.atan2(lxy).to_degrees();
        if pitch > RAMP_SLOPE_DEG + 1e-9 { return Err(format!("pitch {pitch:.1} deg > {RAMP_SLOPE_DEG} deg")); }
        let Some(vmax) = self.profile.max_speed_z else { return Err("the machine's Z speed is not known".into()) };
        let Some(f) = self.feed else { return Err("no feed".into()) };
        let vz = f / 60.0 * dz / (lxy * lxy + dz * dz).sqrt();
        if vz > vmax + 1e-9 { return Err(format!("Z at {vz:.1} mm/s > {vmax} mm/s")); }
        Ok(())
    }

    /// Flow sanity of one extrusion: bead area and volumetric speed.
    fn flow(&mut self, n: usize, from: [f64; 3], to: [f64; 3], arc: Option<([f64; 2], bool)>, e: f64) {
        let Some(pts) = nonplanar::path(from, to, arc, self.sample_bound()) else {
            self.bad(n, "sampling bound", "path samples exceed the build volume's bound".into());
            return;
        };
        let l: f64 = pts.windows(2).map(|w| ((w[1][0] - w[0][0]).powi(2) + (w[1][1] - w[0][1]).powi(2) + (w[1][2] - w[0][2]).powi(2)).sqrt()).sum();
        if l < 1e-6 { return; }
        let fil = self.filament.unwrap_or(0);
        let p = self.profile;
        let Some(df) = p.filament_diameters.get(fil).or_else(|| p.filament_diameters.iter().max_by(|a, b| a.total_cmp(b))).copied() else {
            self.bad(n, "flow", "the filament diameter is not known".into());
            return;
        };
        let d = p.nozzle_diameters.get(self.nozzle().unwrap_or(0)).copied().unwrap_or(0.4);
        let area = e * std::f64::consts::PI * df * df / 4.0 / l;
        if area > FLOW_MAX_AREA * d * d + 1e-9 { self.bad(n, "flow", format!("bead area {area:.3} mm2 > {:.3} mm2 ({FLOW_MAX_AREA} d^2)", FLOW_MAX_AREA * d * d)); }
        let cap = p.max_volumetric.get(fil).or_else(|| p.max_volumetric.iter().max_by(|a, b| a.total_cmp(b))).copied();
        if let (Some(cap), Some(f)) = (cap, self.feed) {
            let q = area * f / 60.0;
            if q > FLOW_MAX_VOLUMETRIC * cap + 1e-9 { self.bad(n, "flow", format!("{q:.1} mm3/s > {FLOW_MAX_VOLUMETRIC} x the filament's {cap} mm3/s")); }
        }
    }

    /// The non-planar rules and the head clearance for one of Fab's own moves (positions
    /// known), then the move's bead into the height field.
    fn head_rules(&mut self, n: usize, from: [f64; 3], to: [f64; 3], arc: Option<([f64; 2], bool)>, e: f64, nonplanar: bool) {
        let (dx, dy, dz) = (to[0] - from[0], to[1] - from[1], to[2] - from[2]);
        let lxy = dx.hypot(dy);
        let l3 = (lxy * lxy + dz * dz).sqrt();
        let v = self.feed.map(|f| f / 60.0);
        let sin = if l3 > 1e-12 { dz / l3 } else { 0.0 };
        if nonplanar {
            let head = self.profile.head.clone().expect("usable head");
            self.nonplanar_moves += 1;
            let pitch = dz.abs().atan2(lxy).to_degrees();
            self.max_pitch = self.max_pitch.max(pitch);
            if pitch > head.max_slope_deg + 1e-9 { self.bad(n, "non-planar slope", format!("pitch {pitch:.1} deg > the {} deg cap", head.max_slope_deg)); }
            match v {
                None => self.bad(n, "Z speed", "a non-planar move with no feed set".into()),
                Some(v) if v * sin.abs() > head.z_speed + 1e-9 => self.bad(n, "Z speed", format!("Z at {:.1} mm/s > {} mm/s", v * sin.abs(), head.z_speed)),
                _ => {}
            }
        }
        // Z jerk at junctions of an extrusion run that has a non-planar move.
        if e > 0.0 {
            if let (Some((ps, pv, pnp)), Some(v)) = (self.run_prev, v) {
                let jump = pv.min(v) * (sin - ps).abs();
                if (nonplanar || pnp) && self.profile.head.as_ref().is_some_and(|h| jump > h.z_jerk + 1e-9) {
                    self.bad(n, "Z jerk", format!("Z speed changes by {jump:.1} mm/s at a junction"));
                }
            }
            self.run_prev = v.map(|v| (sin, v, nonplanar));
        } else {
            self.run_prev = None;
        }
        let profile = self.profile;
        let Some(head) = profile.head.as_ref() else { return };
        // The bead's half width: the file's line width, else 1.125 x the nozzle.
        let half = 0.5 * self.width.unwrap_or_else(|| 1.125 * profile.nozzle_diameters.get(self.nozzle().unwrap_or(0)).copied().unwrap_or(0.4));
        if e <= 0.0 {
            // The run ended: all of its trail is printed material now.
            if let Some(map) = self.heights.as_mut() { for (a, b) in self.trail.drain(..) { map.stamp(a, b, half); } }
            self.trail_len = 0.0;
        }
        let Some(map) = self.heights.as_ref() else { return };
        let parts = self.nozzle().and_then(|k| head.parts.get(k)).cloned().unwrap_or_default();
        let Some(pts) = nonplanar::path(from, to, arc, self.sample_bound()) else {
            self.bad(n, "sampling bound", "path samples exceed the build volume's bound".into());
            return;
        };
        let low = from[2].min(to[2]);
        let mut found: Vec<(&'static str, String)> = Vec::new();
        // At or above everything printed, every part clears by its own height (e >= margin): proven.
        if nonplanar || low < self.layer_top - 1e-9 {
            if parts.is_empty() { found.push(("head clearance", "no head parts for the active nozzle".into())); }
            let (least, worst) = nonplanar::clearance(map, &parts, &pts);
            if least < f64::MAX { self.min_clear = Some(self.min_clear.map_or(least, |m| m.min(least))); }
            if let Some((part, short)) = worst { found.push(("head clearance", format!("the {part} comes {:.2} mm inside its margin of printed material", -short))); }
            if nonplanar {
                let (rh, cap) = (head.rise_half, head.max_rise);
                let rise = pts.iter().map(|p| map.max_rect(p[0] - rh, p[1] - rh, p[0] + rh, p[1] + rh) - p[2]).fold(f64::MIN, f64::max);
                if rise > cap + 1e-9 { found.push(("non-planar rise", format!("printed material {rise:.2} mm above the tip within {rh} mm (cap {cap} mm)"))); }
            }
        }
        for (rule, detail) in found { self.bad(n, rule, detail); }
        if e > 0.0 {
            let map = self.heights.as_mut().expect("height map");
            for w in pts.windows(2) {
                let l = ((w[1][0] - w[0][0]).powi(2) + (w[1][1] - w[0][1]).powi(2) + (w[1][2] - w[0][2]).powi(2)).sqrt();
                self.trail.push_back((w[0], w[1]));
                self.trail_len += l;
            }
            while self.trail_len > nonplanar::TRAIL {
                let Some((a, b)) = self.trail.pop_front() else { break };
                self.trail_len -= ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2) + (b[2] - a[2]).powi(2)).sqrt();
                map.stamp(a, b, half);
            }
        }
    }

    /// Known nozzle: its polygon; unknown nozzle: the intersection of all nozzle bounds.
    fn in_area(&self, x: Option<f64>, y: Option<f64>) -> bool {
        let fits = |e: usize, polygon: bool| {
            let Some(area) = self.profile.extruder_areas.get(e).filter(|a| !a.is_empty()) else { return false };
            let offset = self.profile.extruder_offsets.get(e).copied().unwrap_or([0.0, 0.0]);
            let (x, y) = (x.map(|x| x + offset[0]), y.map(|y| y + offset[1]));
            if let (true, Some(x), Some(y)) = (polygon, x, y) { return inside(area, x, y); }
            let b = bounds(area);
            x.is_none_or(|x| x.is_finite() && x >= b[0] - 1e-6 && x <= b[2] + 1e-6)
                && y.is_none_or(|y| y.is_finite() && y >= b[1] - 1e-6 && y <= b[3] + 1e-6)
        };
        match self.nozzle() { Some(e) => fits(e, true), None => !self.profile.extruder_areas.is_empty() && (0..self.profile.extruder_areas.len()).all(|e| fits(e, false)) }
    }

    fn sample_bound(&self) -> f64 { sample_bound(self.profile) }

    /// Volume rejection precedes flow, head checks, path sampling and grid insertion.
    fn volume(&mut self, c: &str, cmd: &str, w: &Words, n: usize, from: [Option<f64>; 3], to: [Option<f64>; 3]) -> bool {
        let height = self.nozzle().and_then(|e| self.profile.extruder_heights.get(e).copied()).unwrap_or(self.profile.printable_height);
        if to[2].is_some_and(|z| z < MIN_Z) {
            self.bad(n, "below the plate", format!("{c}: effective Z below the plate"));
            return false;
        }
        if to[2].is_some_and(|z| !z.is_finite() || z > height + 1e-6) {
            self.bad(n, "above the build volume", format!("{c}: effective Z exceeds {height} mm"));
            return false;
        }
        if !self.in_area(to[0], to[1]) {
            self.bad(n, "outside the build volume", format!("{c}: X {:.2} Y {:.2} is outside the printable area", to[0].unwrap_or(0.0), to[1].unwrap_or(0.0)));
            return false;
        }
        if matches!(cmd, "G2" | "G3") {
            if let (Some(fx), Some(fy), Some(x), Some(y)) = (from[0], from[1], to[0], to[1]) {
                let mut arc = self.arc([fx, fy], [x, y], w, cmd == "G2");
                if w.get(b'P').is_some_and(|p| p >= 1.0) { arc.sweep = std::f64::consts::TAU; arc.r_end = arc.r; }
                if !arc.len().is_finite() || arc.extremes().iter().any(|p| !self.in_area(Some(p[0]), Some(p[1]))) {
                    self.bad(n, "outside the build volume", format!("{c}: arc leaves the printable area"));
                    return false;
                }
            }
        }
        true
    }

    /// A move on any line; `strict` for Fab's own toolpath and placeholder lines.
    fn motion(&mut self, c: &str, cmd: &str, w: &Words, n: usize, strict: bool, own: bool) {
        let words = [w.get(b'X'), w.get(b'Y'), w.get(b'Z')];
        let is_arc = matches!(cmd, "G2" | "G3");
        let from = [self.axes[0].pos, self.axes[1].pos, self.axes[2].pos];
        let mut to = [None; 3];
        for i in 0..3 {
            match self.target(i, words[i]) {
                Ok(t) => to[i] = t,
                Err(()) => { if strict { self.bad(n, "unknown position", format!("relative {} move from an unknown position", ['X', 'Y', 'Z'][i])); } to[i] = None; }
            }
        }
        let moved = is_arc || words.iter().any(Option::is_some);
        if strict && moved && !self.volume(c, cmd, w, n, from, to) {
            self.axes.iter_mut().for_each(|a| a.pos = None);
            return;
        }
        // A previous unchecked template position must not become an unbounded sampled path.
        if own && moved && (!self.in_area(from[0], from[1]) || from[2].is_some_and(|z| !z.is_finite() || z > self.profile.printable_height)) {
            self.bad(n, "outside the build volume", format!("{c}: move starts outside the build volume"));
            self.axes.iter_mut().for_each(|a| a.pos = None);
            return;
        }
        // The filament this move pushes (M83: the E word; M82: from the last E position).
        let e = match w.get(b'E') {
            Some(v) if self.e_absolute => { let d = v - self.e_pos; self.e_pos = v; d }
            Some(v) => v,
            None => 0.0,
        };
        let extruding = e > 0.0;
        if let Some(f) = w.get(b'F') { self.feed = Some(f); }
        if strict && is_arc {
            if w.has(b'P') && w.get(b'P').is_none_or(|p| p < 0.0 || p.fract() != 0.0 || (own && p > 1.0)) {
                self.bad(n, "arc geometry", format!("{c}: P must be a non-negative integer{}", if own { " no larger than 1" } else { "" }));
            }
            if own && w.has(b'P') && (extruding || (w.get(b'P').is_some_and(|p| p >= 1.0) && (e != 0.0 || w.get(b'E').is_some_and(|v| v != 0.0)))) {
                self.bad(n, "arc geometry", format!("{c}: P is forbidden on extruding arcs, and extra turns must be dry"));
            }
            if from[0].is_none() || from[1].is_none() { self.bad(n, "unknown position", format!("{c}: an arc from an unknown X or Y")); }
        }
        if own && !moved && e > self.profile.prime_allowance + 1e-9 {
            self.bad(n, "flow", format!("an in-place push of {e:.2} mm of filament (at most {:.2})", self.profile.prime_allowance));
        }
        if strict && moved {
            if !self.homed_z || !self.homed_any { self.bad(n, "not homed", "a move before the printer has homed".into()); }
            if own && from[2].is_none() && (w.has(b'X') || w.has(b'Y') || is_arc) {
                self.bad(n, "unknown position", "an XY move from an unknown height: set Z with a Z-only move first".into());
            }
            match to[2] {
                None if own || words[2].is_some() => self.bad(n, "unknown position", "a move with an unknown Z".into()),
                None => {}
                Some(z) if !z.is_finite() => self.bad(n, "words", format!("{c}: non-finite effective Z")),
                Some(z) if z < MIN_Z => self.bad(n, "below the plate", format!("effective Z {z:.3} mm")),
                Some(z) => {
                    let nozzle = self.nozzle();
                    let height = nozzle.and_then(|e| self.profile.extruder_heights.get(e).copied()).unwrap_or(self.profile.printable_height);
                    if z > height + 1e-6 { self.bad(n, "above the build volume", format!("Z {z:.2} mm > {height} mm")); }
                }
            }
            match (to[0], to[1]) {
                (Some(x), Some(y)) => {
                    let mut pts = vec![[x, y]];
                    if is_arc {
                        // Every point of the sweep, not only its ends.
                        if let (Some(fx), Some(fy)) = (from[0], from[1]) {
                            let mut arc = self.arc([fx, fy], [x, y], w, cmd == "G2");
                            self.check_arc(n, c, w, &arc, e, own);
                            let Some(samples) = arc.points(self.sample_bound()) else {
                                self.bad(n, "sampling bound", format!("{c}: arc samples exceed the build volume's bound"));
                                return;
                            };
                            pts.extend(samples);
                            if w.get(b'P').is_some_and(|p| p >= 1.0) {
                                arc.sweep = std::f64::consts::TAU;
                                arc.r_end = arc.r;
                                let Some(samples) = arc.points(self.sample_bound()) else {
                                self.bad(n, "sampling bound", format!("{c}: arc samples exceed the build volume's bound"));
                                return;
                            };
                            pts.extend(samples);
                            }
                        }
                    }
                    let nozzle = self.nozzle();
                    for p in pts {
                        if !self.in_area(Some(p[0]), Some(p[1])) {
                            let side = match nozzle { Some(0) => "the left nozzle's", Some(1) => "the right nozzle's", _ => "the" };
                            self.bad(n, "outside the build volume", format!("X {:.2} Y {:.2} is outside {side} printable area", p[0], p[1]));
                            self.axes.iter_mut().for_each(|a| a.pos = None);
                            return;
                        }
                    }
                }
                (x, y) if !own => {
                    if !self.in_area(x, y) { self.bad(n, "outside the build volume", format!("{c} leaves the nozzle's printable area")); }
                }
                _ if words[0].is_some() || words[1].is_some() || extruding => self.bad(n, "unknown position", "a move with an unknown X or Y".into()),
                _ => {}
            }
        }
        // Non-planar extrusion fails closed. Any non-zero E counts: the extrusion mode is not
        // tracked here, and Fab's planar toolpath never moves Z while it pushes or pulls filament.
        let mut nonplanar = false;
        if own && moved && e != 0.0 {
            let arc_z = matches!(cmd, "G2" | "G3") && words[2].is_some();
            let dz = match (from[2], to[2]) { (Some(a), Some(b)) => (b - a).abs() > 1e-6, _ => words[2].is_some() };
            if arc_z {
                self.bad(n, "non-planar extrusion", format!("{cmd} with Z while extruding: non-planar moves are straight G1 only"));
            } else if dz {
                match (self.ramp(cmd, e, from, to), self.head_ok.clone()) {
                    (Ok(()), _) => self.ramp_moves += 1,
                    (Err(_), Ok(())) => nonplanar = true,
                    (Err(r), Err(why)) => self.bad(n, "non-planar extrusion", format!("{cmd} changes Z while extruding: not a ramp ({r}), and {why}")),
                }
            }
        }
        // A travel, a retraction or a move from an unknown place ends the run of extrusions.
        if own && !(moved && extruding && from.iter().chain(&to).all(Option::is_some)) {
            if let Some(x) = self.printed.finish() { self.crossed(x); }
        }
        if own && moved {
            self.moves += 1;
            if self.nozzle().is_none() && self.profile.extruder_areas.len() > 1 {
                self.bad(n, "unknown nozzle", "a move before a filament (and so a nozzle) is selected".into());
            }
            if let (Some(z), Some(x), Some(y), Some(fx), Some(fy), Some(fz)) = (to[2], to[0], to[1], from[0], from[1], from[2]) {
                let arc = if is_arc { Some(([fx + w.get(b'I').unwrap_or(0.0), fy + w.get(b'J').unwrap_or(0.0)], cmd == "G2")) } else { None };
                self.head_rules(n, [fx, fy, fz], [x, y, z], arc, e, nonplanar);
                if e > 0.0 { self.flow(n, [fx, fy, fz], [x, y, z], arc, e); }
            } else if nonplanar {
                self.bad(n, "non-planar extrusion", "a non-planar move from an unknown position".into());
            }
            if let (Some(z), Some(x), Some(y), Some(fx), Some(fy)) = (to[2], to[0], to[1], from[0], from[1]) {
                if e > 0.0 {
                    match self.first_z {
                        None => {
                            if z < FIRST_LAYER[0] - 1e-6 || z > FIRST_LAYER[1] + 1e-6 { self.bad(n, "first layer", format!("first layer at Z {z:.3} mm, outside 0.1-0.4 mm")); }
                            self.first_z = Some(z);
                        }
                        Some(_) => {}
                    }
                    self.layer_top = self.layer_top.max(z);
                    if let Some(e) = self.nozzle() { if !self.used.contains(&e) { self.used.push(e); } }
                    // Support and the prime tower are sacrificial: travel may cross them; the part's first layer it may not.
                    let on_first = self.first_z.is_some_and(|f| (z - f).abs() < 1e-3) && from[2].is_some_and(|fz| (fz - z).abs() < 1e-3);
                    if on_first {
                        let e = self.first_extent.get_or_insert([fx, fy, fx, fy]);
                        for p in [[fx, fy], [x, y]] { *e = [e[0].min(p[0]), e[1].min(p[1]), e[2].max(p[0]), e[3].max(p[1])]; }
                    }
                    if !self.support && on_first { self.add_first_layer([fx, fy], [x, y], n); }
                    // At one height, the run of extrusions against what that height has printed.
                    if from[2].is_some_and(|fz| (fz - z).abs() < 1e-6) {
                        let path = if is_arc {
                            let Some(path) = self.arc([fx, fy], [x, y], w, cmd == "G2").chords(self.sample_bound()) else {
                                self.bad(n, "sampling bound", format!("{c}: arc chords exceed the build volume's bound"));
                                return;
                            };
                            path
                        } else { vec![[fx, fy], [x, y]] };
                        let width = self.width.unwrap_or_else(|| self.profile.nozzle_diameters.get(self.nozzle().unwrap_or(0)).copied().unwrap_or(0.4));
                        let feature = self.feature.clone();
                        if let Some(x) = self.printed.extrude(z, &path, width, &feature, n) { self.crossed(x); }
                    } else if let Some(x) = self.printed.finish() { self.crossed(x); }
                } else if e == 0.0 && self.first_z.is_some() {
                    let top = self.layer_top.max(self.first_z.unwrap_or(FIRST_LAYER[1]));
                    if z > top + MAX_LIFT + 1e-6 { self.bad(n, "travel lift", format!("travel at Z {z:.2} mm, more than {MAX_LIFT} mm above the print")); }
                    if let Some(f) = self.first_z {
                        let low = from[2].is_some_and(|fz| fz <= f + 0.05) && z <= f + 0.05;
                        let length = (x - fx).hypot(y - fy);
                        let width = self.profile.nozzle_diameters.get(self.nozzle().unwrap_or(0)).copied().unwrap_or(0.4);
                        // A hop shorter than a few widths (a surface pattern's dry connector along its
                        // wall, as the vendor's monotonic-line surfaces have) cannot drag across the part.
                        if low && length > paths::CROSS_WIDTHS * width && self.layer_top <= f + 1e-3 {
                            if let Some(over) = self.crosses_first_layer([fx, fy], [x, y], width) { self.bad(n, "first-layer travel", format!("travel at first-layer height across the first-layer line of line {over}, to X {x:.2} Y {y:.2}")); }
                        }
                    }
                }
                self.max_z = self.max_z.max(z);
            }
        }
        for i in 0..3 { self.axes[i].pos = to[i]; }
    }
}

/// A trusted command that may move the head by itself (probing, wiping, tool and filament
/// changes, camera moves): the position is unknown after it.
fn moves_head(cmd: &str) -> bool {
    if matches!(cmd, "G0" | "G1" | "G2" | "G3" | "G4" | "G17" | "G21" | "G90" | "G91" | "G92" | "G28" | "G29.1" | "G29.2" | "G29.7") { return false; }
    cmd.starts_with('G') || matches!(cmd, "M620" | "M621" | "M9711" | "M972" | "M974") || cmd.starts_with("M970") || (cmd.starts_with('T') && cmd != "T")
}

fn segment_distance(a: [f64; 2], b: [f64; 2], c: [f64; 2], d: [f64; 2]) -> f64 {
    let cross = |o: [f64; 2], p: [f64; 2], q: [f64; 2]| (p[0] - o[0]) * (q[1] - o[1]) - (p[1] - o[1]) * (q[0] - o[0]);
    let (d1, d2, d3, d4) = (cross(c, d, a), cross(c, d, b), cross(a, b, c), cross(a, b, d));
    if ((d1 > 0.0) != (d2 > 0.0)) && ((d3 > 0.0) != (d4 > 0.0)) { return 0.0; }
    let point = |p: [f64; 2], s: [f64; 2], e: [f64; 2]| {
        let (vx, vy) = (e[0] - s[0], e[1] - s[1]);
        let l = vx * vx + vy * vy;
        let t = if l == 0.0 { 0.0 } else { (((p[0] - s[0]) * vx + (p[1] - s[1]) * vy) / l).clamp(0.0, 1.0) };
        (p[0] - s[0] - t * vx).hypot(p[1] - s[1] - t * vy)
    };
    point(a, c, d).min(point(b, c, d)).min(point(c, a, b)).min(point(d, a, b))
}

/// A single arc in the volume is shorter than its bounding diagonal times one turn;
/// include the full height for helical and straight travel.
fn sample_bound(profile: &MachineProfile) -> f64 {
    let b = bounds(&profile.printable_area);
    (b[2] - b[0]).hypot(b[3] - b[1]) * std::f64::consts::TAU
        + profile.extruder_heights.iter().copied().fold(profile.printable_height, f64::max)
}

/// The G-code check (the plate's text): settings against the installed profile, then the simulation.
pub fn check_gcode(text: &str, profile: &MachineProfile) -> Result<JobFacts, Refusal> {
    run_check(text, profile, 50)
}

fn run_check(text: &str, profile: &MachineProfile, cap: usize) -> Result<JobFacts, Refusal> {
    family::of_model(&profile.printer_model).ok_or_else(unsupported_model)?;
    if profile.flavor == Flavor::Named { named_blocks(text)?; }
    let mut out = Vec::new();
    let mut bad = |rule: &'static str, detail: String| out.push(Violation { line: 0, rule, detail });
    let header = header_config(text);
    let get = |k: &str| header.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
    // ---- the file's settings name the installed machine ----
    if get("printer_model").map(str::trim) != Some(profile.printer_model.as_str()) {
        bad("machine", format!("the file is for {:?}, the installed machine is {:?}", get("printer_model").unwrap_or("?"), profile.printer_model));
    }
    if !close(&get("nozzle_diameter").map(nums).unwrap_or_default(), &profile.nozzle_diameters) {
        bad("nozzle diameter", format!("the file says {:?}, the machine has {:?}", get("nozzle_diameter").unwrap_or("?"), profile.nozzle_diameters));
    }
    let file_offsets: Vec<[f64; 2]> = get("extruder_offset").map(parse_points).unwrap_or_default();
    if !same_area(&file_offsets, &profile.extruder_offsets) {
        bad("nozzle offset", format!("the file says {:?}, the machine has {:?}", get("extruder_offset").unwrap_or("?"), profile.extruder_offsets));
    }
    if !same_area(&get("printable_area").map(parse_points).unwrap_or_default(), &profile.printable_area) {
        bad("build volume", "the file's printable area is not the machine's".into());
    }
    if profile.flavor == Flavor::Named && get("printable_height").and_then(finite).is_none_or(|h| !profile.printable_height.is_finite() || (h - profile.printable_height).abs() >= 1e-6) {
        bad("build volume", "the file's printable height is not the machine's".into());
    }
    // The file's per-nozzle areas, read as the profile's are.
    let (file_areas, file_heights) = per_nozzle(
        profile.flavor,
        get("extruder_printable_area").map(areas).unwrap_or_default(),
        get("extruder_printable_height").map(nums).unwrap_or_default(),
        &get("printable_area").map(parse_points).unwrap_or_default(),
        get("printable_height").and_then(finite).unwrap_or(0.0),
        get("nozzle_diameter").map(nums).unwrap_or_default().len(),
    );
    if file_areas.len() != profile.extruder_areas.len() || !file_areas.iter().zip(&profile.extruder_areas).all(|(a, b)| same_area(a, b)) {
        bad("build volume", "the file's per-nozzle printable areas are not the machine's".into());
    }
    if !close(&file_heights, &profile.extruder_heights) {
        bad("build volume", "the file's per-nozzle heights are not the machine's".into());
    }
    for (key, want) in profile.templates.iter().filter(|(k, _)| MACHINE_TEMPLATES.contains(&k.as_str())) {
        if get(key).map(unescape).as_deref().map(str::trim) != Some(want.trim()) {
            bad("foreign sequence", format!("the file's {key} is not the installed profile's"));
        }
    }
    if profile.nozzle_diameters.is_empty() || profile.extruder_areas.is_empty() || profile.printable_area.is_empty() {
        bad("machine", "the installed machine profile lacks its areas or nozzles".into());
    }
    let filament_map: Vec<usize> = get("filament_map").and_then(|v| v.split(',').map(|s| s.trim().parse().ok()).collect::<Option<Vec<_>>>()).unwrap_or_default();
    if filament_map.is_empty() || filament_map.iter().any(|m| *m == 0 || *m > profile.nozzle_diameters.len()) {
        bad("nozzle map", "the file does not map its filaments to existing nozzles".into());
    }
    let (mut used_mm, mut used_g) = (Vec::new(), Vec::new());
    if profile.flavor == Flavor::Named {
        match (usage(text, "mm", filament_map.len()), usage(text, "g", filament_map.len())) {
            (Some(mm), Some(g)) => { used_mm = mm; used_g = g; }
            _ => bad("filament statistics", "one finite non-negative mm and g value per job filament is required, once each".into()),
        }
    }
    for f in 0..filament_map.len() {
        if profile.filament_types.get(f).is_none_or(|s| s.trim().is_empty())
            || profile.nozzle_temp_ranges.get(f).is_none_or(|r| r.iter().any(|v| !v.is_finite()))
            || profile.print_temps.get(f).is_none_or(|r| r.iter().any(|v| !v.is_finite())) {
            bad("filament", format!("filament {f} lacks an installed type, temperature range or print temperatures"));
        }
    }
    if !out.is_empty() { return Err(Refusal(out)); }

    // ---- the simulation ----
    let templates: Vec<Template> = profile.templates.iter().map(|(k, t)| Template::for_family(k, t, profile.flavor)).filter(|t| !t.is_empty()).collect();
    let start = templates.iter().position(|t| t.name == "machine_start_gcode");
    let end = templates.iter().position(|t| t.name == "machine_end_gcode");
    let head_ok = match &profile.head { None => Err("no head geometry is installed".to_string()), Some(h) => h.usable() };
    let heights = if head_ok.is_ok() {
        let b = bounds(&profile.printable_area);
        Some(nonplanar::HeightMap::new([b[0], b[1]], [b[2], b[3]])
            .ok_or_else(|| Refusal(vec![Violation { line: 0, rule: "sampling bound", detail: "height field exceeds the bed's representable size".into() }]))?)
    } else { None };
    let mut sim = Sim {
        profile, relative: false, axes: [Axis { pos: None, shift: 0.0 }; 3], homed_z: false, homed_any: false,
        objects: Default::default(), open_object: None, other_command: false,
        filament: None, filament_map, first_z: None, layer_top: 0.0, max_z: 0.0, moves: 0, used: Vec::new(),
        first_layer: Vec::new(), first_layer_lines: Vec::new(), support: false, first_extent: None, probes: Vec::new(), grid: Default::default(), out: Vec::new(),
        feature: String::new(), width: None, height: None, e_absolute: false, e_pos: 0.0,
        filament_diameters: get("filament_diameter").map(nums).unwrap_or_default(), printed: paths::Printed::new(sample_bound(profile)), cap,
        feed: None, head_ok, heights, run_prev: None, nonplanar_moves: 0, max_pitch: 0.0, min_clear: None, ramp_moves: 0, trail: Default::default(), trail_len: 0.0,
    };
    // Executable lines: (line number, command part).
    let mut lines: Vec<(usize, &str)> = Vec::new();
    // Per line, the index of its tags (feature, line width, layer height) in `tags`.
    let mut line_tags: Vec<usize> = Vec::new();
    let mut tags: Vec<(String, Option<f64>, Option<f64>)> = vec![(String::new(), None, None)];
    // The lines checked must be the lines a firmware runs, which may take a lone CR or another
    // control character for a line end.
    let mut at = 1;
    for (i, b) in text.bytes().enumerate() {
        match b {
            b'\n' => at += 1,
            b'\t' => {}
            b'\r' if text.as_bytes().get(i + 1) == Some(&b'\n') => {}
            0..=0x1f | 0x7f => sim.bad(at, "control character", format!("byte {b:#04x} in a line")),
            _ => {}
        }
    }
    let (mut executing, mut ended) = (false, false);
    for (i, l) in text.lines().enumerate() {
        if profile.flavor == Flavor::Named {
            let cmd = second_split(code(l)).0.to_ascii_uppercase();
            if NAMED_DENIED.contains(&cmd.as_str()) { sim.bad(i + 1, "denied command", format!("{cmd} is forbidden anywhere")); }
        }
        if !ended && l.starts_with("; EXECUTABLE_BLOCK_START") { executing = true; continue; }
        if l.starts_with("; EXECUTABLE_BLOCK_END") { executing = false; ended = true; continue; }
        if !executing {
            // Only the block is simulated, so nothing outside it may run.
            let c = code(l);
            if !c.is_empty() { sim.bad(i + 1, "outside the block", format!("{c}: a command outside the executable block")); }
            continue;
        }
        let tag = |v: &str| finite(v).filter(|v| *v > 0.0);
        let mut t = tags[tags.len() - 1].clone();
        let changed = if let Some(f) = l.strip_prefix("; FEATURE:") { t.0 = f.trim().to_string(); true }
            else if let Some(w) = l.strip_prefix("; LINE_WIDTH:") { t.1 = tag(w); true }
            else if let Some(h) = l.strip_prefix("; LAYER_HEIGHT:") { t.2 = tag(h); true } else { false };
        if changed { tags.push(t); }
        let c = code(l);
        if !c.is_empty() { lines.push((i + 1, c)); line_tags.push(tags.len() - 1); }
    }
    if lines.is_empty() { sim.bad(0, "package", "the file has no executable G-code".into()); }
    let codes: Vec<&str> = lines.iter().map(|(_, c)| *c).collect();
    let mut i = 0;
    let (mut seen_start, mut seen_end) = (false, false);
    while i < lines.len() {
        if seen_end {
            let cmd = split_cmd(codes[i]).map(|(cmd, _)| cmd).unwrap_or_else(|_| codes[i].split_whitespace().next().unwrap_or(""));
            if if profile.flavor == Flavor::Named { !cmd.eq_ignore_ascii_case("M73") } else { cmd != "M73" } {
                sim.bad(lines[i].0, "foreign sequence", format!("{cmd} after the end sequence"));
                i += 1;
                continue;
            }
        }
        // A block of the installed profile's own sequence?
        let mut best: Option<(usize, usize, Vec<Option<usize>>)> = None;
        for (ti, t) in templates.iter().enumerate().filter(|_| !seen_end) {
            if !t.could_start(codes[i]) { continue; }
            if let Some((len, matched)) = t.match_at(&codes[i..]) {
                if best.as_ref().is_none_or(|b| len > b.1) { best = Some((ti, len, matched)); }
            }
        }
        match best {
            Some((ti, len, matched)) => {
                let marks: Vec<bool> = matched.iter().map(|t| len == 1 || t.is_some_and(|t| templates[ti].lines[t].placeholder)).collect();
                if (len > 1 || profile.flavor == Flavor::Named) && Some(ti) == start {
                    if seen_start || sim.moves > 0 { sim.bad(lines[i].0, "foreign sequence", "the start sequence is not at the start".into()); }
                    seen_start = true;
                }
                // A printer sequence ends the run of extrusions.
                if let Some(x) = sim.printed.finish() { sim.crossed(x); }
                for k in 0..len {
                    let (n, c) = lines[i + k];
                    if profile.flavor == Flavor::Named {
                        if let Some(t) = matched[k] {
                            let cmd = second_split(c).0;
                            if !templates[ti].literal_command(t, cmd) {
                                sim.bad(n, "template command", format!("{cmd}: a template capture cannot supply a command"));
                                continue;
                            }
                            if c.split_ascii_whitespace().count() > templates[ti].word_count(t) {
                                sim.bad(n, "template words", format!("{c}: more words than the installed template line"));
                                continue;
                            }
                            if !templates[ti].captures_valid(t, c) {
                                sim.bad(n, "template capture", format!("{c}: every capture must be one strict decimal value, or one of its placeholder's fixed choices"));
                                continue;
                            }
                        }
                        let (feature, width, height) = &tags[line_tags[i + k]];
                        sim.support = matches!(paths::class(feature), paths::Class::Support | paths::Class::Tower);
                        sim.feature = feature.clone();
                        (sim.width, sim.height) = (*width, *height);
                        sim.second_command(c, n, matched[k].is_some());
                        continue;
                    }
                    if marks[k] {
                        if let Some(t) = matched[k] {
                            let captures = templates[ti].captures(t, c);
                            if captures.as_ref().is_none_or(|captures| captures.iter().any(|s| s.split(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != '_')
                                .any(|s| ["nan", "inf", "infinity", "nil"].iter().any(|bad| s.eq_ignore_ascii_case(bad))))) {
                                sim.bad(n, "words", format!("{c}: non-finite template placeholder"));
                                continue;
                            }
                        }
                    }
                    let split = split_cmd(c);
                    let cmd = split.as_ref().map(|(cmd, _)| *cmd).unwrap_or_else(|_| c.split_whitespace().next().unwrap_or(""));
                    let own = matched[k].is_none();
                    let w = if own || (reads_words(cmd) && (profile.flavor != Flavor::Named || split.is_ok())) {
                        match split.and_then(|(_, rest)| parse_words(rest, !own)) {
                            Ok(w) => w,
                            Err(why) => { sim.bad(n, "words", format!("{c}: {why}")); continue; }
                        }
                    } else { Words::default() };
                    if own {
                        for a in b'A'..=b'Z' { if w.has(a) && !allowed_words(cmd).contains(&a) { sim.bad(n, "words", format!("{cmd} does not take {}", a as char)); } }
                    }
                    if marks[k] || own { sim.values(c, cmd, &w, n, own); }
                    sim.modal(cmd, &w, n, !own);
                    if matches!(cmd, "G0" | "G1" | "G2" | "G3") { sim.motion(c, cmd, &w, n, marks[k] || own, own); }
                    else if marks[k] && matches!(cmd, "G29" | "M9711") {
                        // Placeholder coordinates of a probe or camera move: inside the plate.
                        if let (Some(x), Some(y)) = (w.get(b'X'), w.get(b'Y')) {
                            if !inside(&profile.printable_area, x, y) { sim.bad(n, "outside the build volume", format!("{cmd} at X {x:.2} Y {y:.2}")); }
                        }
                        if let Some(z) = w.get(b'Z') { if z < MIN_Z || z > profile.printable_height { sim.bad(n, "below the plate", format!("{cmd} Z {z:.3}")); } }
                        // A bed probe area (corner X Y, size I J): checked against the first layer at the end.
                        if let (true, Some(x), Some(y), Some(i), Some(j)) = (cmd == "G29", w.get(b'X'), w.get(b'Y'), w.get(b'I'), w.get(b'J')) {
                            sim.probes.push((n, [x, y, x + i, y + j]));
                        }
                    } else if moves_head(cmd) || (profile.flavor == Flavor::Named && split_cmd(c).is_err()) {
                        // A firmware routine: where it leaves the head is the printer's.
                        sim.axes.iter_mut().for_each(|a| a.pos = None);
                    }
                }
                if (len > 1 || profile.flavor == Flavor::Named) && Some(ti) == end { seen_end = true; }
                i += len;
                continue;
            }
            _ => {}
        }
        // Fab's own toolpath.
        let (n, c) = lines[i];
        let (feature, width, height) = &tags[line_tags[i]];
        sim.support = matches!(paths::class(feature), paths::Class::Support | paths::Class::Tower);
        if sim.feature != *feature { sim.feature = feature.clone(); }
        (sim.width, sim.height) = (*width, *height);
        if profile.flavor == Flavor::Named {
            sim.second_command(c, n, false);
            i += 1;
            continue;
        }
        let (cmd, rest) = match split_cmd(c) {
            Ok(split) => split,
            Err(why) => { sim.bad(n, "words", format!("{c}: {why}")); i += 1; continue; }
        };
        let tool = profile.flavor == Flavor::Named && cmd.strip_prefix('T').is_some_and(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()));
        let allowed = TOOLPATH_COMMANDS.contains(&cmd) || tool;
        if !allowed { sim.bad(n, "command", format!("{cmd} is not allowed outside the printer's own sequences")); }
        match cmd {
            // The pause for items placed into their parts (`; FAB_PAUSE:`): the printer waits for
            // the person. No other M400 takes a word.
            "M400" if rest.trim() == "U1" => {}
            "M900" => {
                let mut words = c.split_whitespace().skip(1);
                let k = words.next().and_then(|w| w.strip_prefix('K'));
                // A decimal K only: exponent letters could be read as extra
                // G-code words by firmware, even though Rust parses them.
                let valid = k.is_some_and(|v| v.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'+' | b'-'))
                    && v.parse::<f64>().is_ok_and(|v| (0.0..=0.5).contains(&v)));
                if !valid || words.next().is_some() {
                    sim.bad(n, "pressure advance", format!("{c}: expected M900 K<value> with K in 0..=0.5 and no other words"));
                }
            }
            "M624" => {
                let words: Vec<_> = c.split_ascii_whitespace().collect();
                if words.len() != 2 || words[0] != cmd || !words[1].bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=')) {
                    sim.bad(n, "words", format!("{c}: expected M624 <base64 mask>"));
                }
            }
            "M632" => {
                let words: Vec<_> = c.split_ascii_whitespace().collect();
                let filament = words.get(1).and_then(|w| w.strip_prefix('S')).filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).and_then(|s| s.parse::<usize>().ok());
                if words.len() != 3 || words[0] != cmd || words.get(2) != Some(&"W") || filament.is_none_or(|f| f >= sim.filament_map.len()) {
                    sim.bad(n, "words", format!("{c}: expected M632 S<job filament index> W"));
                }
            }
            _ => match parse_words(rest, false) {
                Err(why) => sim.bad(n, "words", format!("{c}: {why}")),
                Ok(w) if allowed => {
                    for a in b'A'..=b'Z' { if w.has(a) && !allowed_words(cmd).contains(&a) { sim.bad(n, "words", format!("{cmd} does not take {}", a as char)); } }
                    sim.values(c, cmd, &w, n, true);
                    sim.modal(cmd, &w, n, false);
                    if matches!(cmd, "G0" | "G1" | "G2" | "G3") { sim.motion(c, cmd, &w, n, true, true); }
                }
                Ok(_) => {}
            },
        }
        i += 1;
    }
    if let Some(x) = sim.printed.finish() { sim.crossed(x); }
    if sim.printed.sample_error { sim.bad(0, "sampling bound", "crossing samples exceed the bed size".into()); }
    if let Some(name) = sim.open_object.take() { sim.bad(0, "object pairing", format!("EXCLUDE_OBJECT_START NAME={name}: object still open at the end of the file")); }
    if !seen_start { sim.bad(0, "foreign sequence", "the installed profile's start sequence is missing".into()); }
    if !seen_end && end.is_some() { sim.bad(0, "foreign sequence", "the installed profile's end sequence is missing".into()); }
    if sim.first_z.is_none() { sim.bad(0, "first layer", "the file prints nothing".into()); }
    // The bed mesh must cover everything the first layer lays down (support and brim included): outside the probed
    // area the nozzle follows an unmeasured bed.
    if let Some(e) = sim.first_extent {
        for (n, p) in sim.probes.clone() {
            if e[0] < p[0] - PROBE_SLACK || e[1] < p[1] - PROBE_SLACK || e[2] > p[2] + PROBE_SLACK || e[3] > p[3] + PROBE_SLACK {
                sim.bad(n, "bed probe", format!("the probed area X {:.1}-{:.1} Y {:.1}-{:.1} does not cover the first layer X {:.1}-{:.1} Y {:.1}-{:.1}", p[0], p[2], p[1], p[3], e[0], e[2], e[1], e[3]));
            }
        }
    }
    if !sim.out.is_empty() { return Err(Refusal(sim.out)); }
    let mut used = sim.used.clone();
    used.sort();
    Ok(JobFacts {
        used_mm, used_g,
        filaments: sim.filament_map.iter().enumerate().map(|(f, e)| {
            let kind = profile.filament_types[f].clone();
            let upper = kind.to_ascii_uppercase();
            FilamentFacts { kind, extruder: e - 1, print_temps: profile.print_temps[f],
                needs_hardened: upper.contains("CF") || upper.contains("GF") || profile.required_hrc.get(f).is_some_and(|h| *h > 20.0) }
        }).collect(),
        nozzle_diameters: profile.nozzle_diameters.clone(), extruders_used: used, bed_type: get("curr_bed_type").unwrap_or_default().trim().to_string(),
        first_layer_z: sim.first_z.unwrap_or(0.0), max_z: sim.max_z, moves: sim.moves,
        nonplanar_moves: sim.nonplanar_moves, max_pitch_deg: sim.max_pitch, min_head_clearance: sim.min_clear,
        ramp_moves: sim.ramp_moves,
        head_version: (sim.nonplanar_moves > 0).then(|| profile.head.as_ref().map(|h| h.version.clone())).flatten(),
    })
}

#[cfg(test)]
pub(crate) mod tests;
