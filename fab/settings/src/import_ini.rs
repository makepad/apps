//! The INI preset import. Values are decoded only after their keys pass the
//! privacy filter. Inheritance is resolved before translating option values.

use crate::config::{Config, Value};
use crate::import::{ImportBatch, ImportedPreset, KeyReport};
use crate::options::{options, Kind, PresetKind};
use crate::profiles::{ProfileStore, RawPreset};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead};
use std::path::Path;

// Format identifiers, not executable templates or shipped profiles.
pub(crate) const CONFIG_PATH: &str = "Metadata/Slic3r_PE.config";
pub(crate) const CONFIG_BEGIN: &str = "; prusaslicer_config = begin";
pub(crate) const CONFIG_END: &str = "; prusaslicer_config = end";
const KEY_MAP: &str = include_str!("ini_keys.tsv");
const SECTIONS: &[(&str, &str)] = &[("printer", "machine"), ("print", "process"), ("filament", "filament")];
const SKIP_SECTIONS: &[&str] = &["physical_printer", "presets"];
const RESIN_SECTIONS: &[&str] = &["sla_print", "sla_material"];
const MODEL: &str = "printer_model";
const VARIANT: &str = "printer_variant";
const VARIANTS: &str = "variants";
const VENDOR: &str = "vendor";
const OBSOLETE: &str = "obsolete_presets";
const INHERITS: &str = "inherits";
const ALIAS: &str = "alias";
const RENAMED: &str = "renamed_from";
const CUSTOM: &str = "custom_parameters_";
const COMPAT: &str = "compatible_printers";
const CONDITION: &str = "compatible_printers_condition";
const PLAIN: &[&str] = &[INHERITS, VARIANTS, "default_materials", "printer_notes", CONDITION, "compatible_prints_condition"];
const IDS: &[(&str, &str)] = &[("printer_settings_id", "machine"), ("print_settings_id", "process"), ("filament_settings_id", "filament")];
const PRIVATE: &[&str] = &["login", "user", "token", "password", "apikey", "cloud"];
const HOST: &str = "print_host";
const HOST_PREFIX: &str = "printhost_";
const GCODE_SUFFIX: &str = "_gcode";
const UNHEATED: &str = "fab_bed_unheated";
const BED: &str = "bed_temperature";
const FIRST_BED: &str = "first_layer_bed_temperature";
const SUPPORT: &str = "support_material";
const SUPPORT_AUTO: &str = "support_material_auto";
const SUPPORT_STYLE: &str = "support_material_style";
const SUPPORT_LAYERS: &str = "support_material_enforce_layers";
const SUPPORT_TYPE: &str = "support_type";
const STYLE_OUT: &str = "support_style";
const SUPPORT_TYPES: &[&str] = &["normal(manual)", "normal(auto)", "tree(manual)", "tree(auto)"];
const TREE_STYLES: &[&str] = &["organic", "tree"];
const ORGANIC: &str = "organic";
const GRID: &str = "grid";
const IRON: &str = "ironing";
const IRON_TYPE: &str = "ironing_type";
const NO_IRON: &str = "no ironing";
const RAMP: &str = "travel_ramping_lift";
const LIFT: &str = "retract_lift";
const MAX_LIFT: &str = "travel_max_lift";
const SLOPE: &str = "travel_slope";
const HOP: &str = "z_hop";
const HOP_TYPES: &str = "z_hop_types";
const NORMAL_LIFT: &str = "Normal Lift";
const SLOPE_LIFT: &str = "Slope Lift";
const LIFT_BOUNDS: &[&str] = &["retract_lift_above", "retract_lift_below"];
const ENFORCE: &str = "retract_lift_enforce";
const ALL_SURFACES: &str = "All Surfaces";
const LABEL: &str = "gcode_label_objects";
const EXCLUDE: &str = "exclude_object";
const LABEL_VALUES: &[&str] = &["disabled", "octoprint", "firmware"];
const REGIONS: &[(&str, &[&str])] = &[
    ("perimeter_extruder", &["outer_wall_filament_id", "inner_wall_filament_id"]),
    ("infill_extruder", &["sparse_infill_filament_id"]),
    ("solid_infill_extruder", &["internal_solid_filament_id", "top_surface_filament_id", "bottom_surface_filament_id"]),
];
// Row, source, destination, enumerated value translations. Empty maps retain text.
const VALUE_MAPS: &[(u8, &str, &str, &[(&str, &str)])] = &[
    (1, "external_perimeters_first", "wall_sequence", &[("0", "inner wall/outer wall"), ("1", "outer wall/inner wall")]),
    (2, "complete_objects", "print_sequence", &[("0", "by layer"), ("1", "by object")]),
    (3, SUPPORT, "enable_support", &[]),
    (3, SUPPORT_LAYERS, "enforce_support_layers", &[]),
    (4, "ensure_vertical_shell_thickness", "ensure_vertical_shell_thickness", &[("enabled", "ensure_all"), ("partial", "ensure_all"), ("disabled", "ensure_critical_only")]),
    (6, "seam_position", "seam_position", &[("rear", "back")]),
    (7, "top_one_perimeter_type", "only_one_wall_top", &[("none", "0"), ("top", "1"), ("topmost", "1")]),
    (7, "only_one_perimeter_first_layer", "only_one_wall_first_layer", &[]),
    (9, "machine_limits_usage", "emit_machine_limits_to_gcode", &[("emit_to_gcode", "1"), ("time_estimate_only", "0"), ("ignore", "0")]),
];
const PATTERNS: &[(&str, &str)] = &[
    ("fill_pattern", "sparse_infill_pattern"), ("top_fill_pattern", "top_surface_pattern"),
    ("bottom_fill_pattern", "bottom_surface_pattern"),
];
const PATTERN_ALIASES: &[(&str, &str)] = &[("solid_fill_pattern", "top_fill_pattern"), ("external_fill_pattern", "bottom_fill_pattern")];
const PATTERN_VALUES: &[(&str, &str)] = &[("monotoniclines", "monotonicline"), ("stars", "tri-hexagon")];

#[derive(Clone, Default)]
struct Section {
    kind: String,
    name: String,
    values: Config,
    withheld: BTreeSet<String>,
}

fn private(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key == HOST || key.starts_with(HOST_PREFIX) || PRIVATE.iter().any(|s| key.contains(s))
}

// Consume a withheld tail directly from the input buffer, without allocating,
// decoding, copying or inspecting its contents except to find the line boundary.
fn discard_line(r: &mut impl BufRead) -> io::Result<()> {
    loop {
        let b = r.fill_buf()?;
        if b.is_empty() { return Ok(()); }
        let end = b.iter().position(|&c| c == b'\n');
        let n = end.map_or(b.len(), |i| i + 1);
        r.consume(n);
        if end.is_some() { return Ok(()); }
    }
}

fn byte(r: &mut impl BufRead) -> io::Result<Option<u8>> {
    let b = r.fill_buf()?.first().copied();
    if b.is_some() { r.consume(1); }
    Ok(b)
}

/// A key or section type, stopping before its value or section label.
fn prefix(r: &mut impl BufRead, mut commented: bool) -> io::Result<(Vec<u8>, Option<u8>)> {
    let mut out = Vec::new();
    while let Some(c) = byte(r)? {
        if matches!(c, b'=' | b'\n') || (out.first() == Some(&b'[') && matches!(c, b':' | b']')) {
            return Ok((out, Some(c)));
        }
        if out.is_empty() && matches!(c, b' ' | b'\t' | b'\r') { continue; }
        if out.is_empty() && commented && c == b';' { commented = false; continue; }
        if out.is_empty() && c == b'#' { discard_line(r)?; return Ok((out, Some(b'\n'))); }
        out.push(c);
    }
    Ok((out, None))
}

fn read_sections(r: &mut impl BufRead, gcode: bool, commented: bool, batch: &mut ImportBatch) -> io::Result<(Vec<Section>, bool)> {
    let mut sections = vec![Section::default()];
    let mut skip = false;
    let mut active = !gcode;
    let mut found = false;
    let (marker_key, begin) = CONFIG_BEGIN.split_once('=').unwrap();
    let (_, end) = CONFIG_END.split_once('=').unwrap();
    // A byte-order mark is not part of the first key: left on, it hides that key from the privacy filter.
    if r.fill_buf()?.starts_with(b"\xef\xbb\xbf") { r.consume(3); }
    loop {
        let (head, delim) = prefix(r, commented)?;
        if head.is_empty() && delim.is_none() { break; }
        let Ok(head) = std::str::from_utf8(&head) else { if delim != Some(b'\n') { discard_line(r)?; } continue };
        let key = head.trim();
        if gcode && key == marker_key.trim().trim_start_matches(';').trim() && delim == Some(b'=') {
            let mut tail = String::new();
            r.read_line(&mut tail)?;
            if tail.trim() == begin.trim() { active = true; found = true; }
            if active && tail.trim() == end.trim() { return Ok((sections, found)); }
            continue;
        }
        if !active { if delim != Some(b'\n') && delim.is_some() { discard_line(r)?; } continue; }
        if let Some(kind) = key.strip_prefix('[') {
            skip = SKIP_SECTIONS.contains(&kind) || RESIN_SECTIONS.contains(&kind) || kind == OBSOLETE;
            if SKIP_SECTIONS.contains(&kind) { batch.notes.push(format!("section {kind}: withheld")); }
            if RESIN_SECTIONS.contains(&kind) { batch.notes.push(format!("section {kind}: not imported")); }
            if skip {
                if delim != Some(b'\n') { discard_line(r)?; }
                continue;
            }
            let mut tail = String::new();
            if delim != Some(b'\n') { r.read_line(&mut tail)?; }
            let name = if delim == Some(b':') { tail.trim().strip_suffix(']').unwrap_or(tail.trim()) } else { "" };
            sections.push(Section { kind: kind.into(), name: name.into(), ..Default::default() });
            continue;
        }
        if delim != Some(b'=') { continue; }
        if private(key) || skip {
            if private(key) {
                batch.notes.push(format!("withheld key: {key}"));
                if !skip { sections.last_mut().unwrap().withheld.insert(key.into()); }
            }
            discard_line(r)?;
            continue;
        }
        let mut value = String::new();
        r.read_line(&mut value)?;
        sections.last_mut().unwrap().values.set_str(key, value.trim());
    }
    if gcode && found { batch.errors.push("INI presets: configuration block has no end marker".into()); }
    Ok((sections, found))
}

fn mapped(key: &str) -> &str {
    static MAP: std::sync::OnceLock<BTreeMap<&str, &str>> = std::sync::OnceLock::new();
    MAP.get_or_init(|| KEY_MAP.lines().filter_map(|l| l.split_once('\t')).collect()).get(key).copied().unwrap_or(key)
}

fn strings(text: &str) -> Vec<String> {
    string_elements(text, false)
}

fn string_elements(text: &str, escaped_bare: bool) -> Vec<String> {
    let mut chars = text.chars().peekable();
    let mut out = Vec::new();
    while chars.peek().is_some() {
        while chars.peek().is_some_and(|c| matches!(c, ' ' | '\t')) { chars.next(); }
        let quoted = chars.peek() == Some(&'"');
        if quoted { chars.next(); }
        let mut item = String::new();
        while let Some(c) = chars.next() {
            if quoted && c == '"' { break; }
            if !quoted && c == ';' { break; }
            if quoted && c == '\\' {
                match chars.next() {
                    Some('n') => item.push('\n'), Some('r') => item.push('\r'),
                    Some('"') => item.push('"'), Some('\\') => item.push('\\'),
                    Some(c) => { item.push('\\'); item.push(c); }, None => item.push('\\'),
                }
            } else { item.push(c); }
        }
        if quoted {
            while chars.peek().is_some_and(|c| matches!(c, ' ' | '\t')) { chars.next(); }
            if chars.peek() == Some(&';') { chars.next(); }
        }
        out.push(if quoted { item } else if escaped_bare { crate::config::unescape_cstyle(item.trim()) } else { item.trim().to_string() });
    }
    out
}

fn decode(key: &str, text: &str) -> Value {
    if PLAIN.contains(&key) { return Value::Scalar(text.into()); }
    match options().kind(key) {
        Some(Kind::Strs | Kind::Enums) => Value::Vector(string_elements(text, key.ends_with(GCODE_SUFFIX))),
        Some(k) if k.is_vector() => Value::Vector(text.split(',').map(|s| s.trim().to_string()).collect()),
        Some(_) if key.ends_with(GCODE_SUFFIX) => Value::Scalar(crate::config::unescape_cstyle(text)),
        _ => Value::Scalar(text.into()),
    }
}

fn put(out: &mut Config, report: &mut KeyReport, key: &str, value: Value, row: Option<u8>) {
    if options().get(key).is_some() {
        if !out.has(key) { report.known += 1; }
    } else {
        let entry = row.map_or_else(|| key.to_string(), |r| format!("{key} (row {r})"));
        if !report.unknown.contains(&entry) { report.unknown.push(entry); }
    }
    out.set(key, value);
}

fn normalize_aliases(raw: &mut Config) {
    for &(alias, canonical) in PATTERN_ALIASES {
        if let Some(value) = raw.remove(alias) {
            if !raw.has(canonical) { raw.set(canonical, value); }
        }
    }
}

fn translate(raw: &Config) -> (Config, KeyReport) {
    translate_over(raw, &Config::new())
}

fn translate_over(raw: &Config, base: &Config) -> (Config, KeyReport) {
    let (mut values, report) = convert_over(raw, base);
    values.map.retain(|key, _| options().get(key).is_some());
    (values, report)
}

#[cfg(test)]
fn convert(raw: &Config) -> (Config, KeyReport) {
    convert_over(raw, &Config::new())
}

fn convert_over(raw: &Config, base: &Config) -> (Config, KeyReport) {
    let mut raw = raw.clone();
    normalize_aliases(&mut raw);
    let mut out = base.clone();
    let mut report = KeyReport::default();
    let mut handled = BTreeSet::new();
    // Store all computed outputs before the option-table gate, including the
    // outputs whose names are currently unknown to the catalogue.
    let mut computed: Vec<(String, Value, u8)> = Vec::new();
    let mut emit = |key: &str, text: &str, row| computed.push((key.into(), decode(key, text), row));
    for &(row, key, target, values) in VALUE_MAPS {
        if raw.has(key) {
            handled.insert(key);
            let text = raw.str(key);
            emit(target, values.iter().find(|(a, _)| *a == text).map_or(text, |(_, b)| b), row);
        }
    }
    for &(key, target) in PATTERNS {
        if raw.has(key) {
            handled.insert(key);
            let text = raw.str(key);
            emit(target, PATTERN_VALUES.iter().find(|(a, _)| *a == text).map_or(text, |(_, b)| b), 5);
        }
    }
    if [SUPPORT, SUPPORT_AUTO, SUPPORT_STYLE, SUPPORT_LAYERS].iter().any(|k| raw.has(k)) {
        handled.extend([SUPPORT_AUTO, SUPPORT_STYLE]);
        let style = if raw.has(SUPPORT_STYLE) { raw.str(SUPPORT_STYLE) } else if base.has(STYLE_OUT) { base.str(STYLE_OUT) } else { GRID };
        let tree = TREE_STYLES.contains(&style);
        let enabled = if raw.has(SUPPORT) { raw.bool(SUPPORT) } else { base.bool("enable_support") };
        let automatic = if raw.has(SUPPORT_AUTO) { raw.bool(SUPPORT_AUTO) } else { !base.has(SUPPORT_TYPE) || base.str(SUPPORT_TYPE).ends_with("(auto)") };
        let auto = enabled && automatic;
        emit(SUPPORT_TYPE, SUPPORT_TYPES[usize::from(tree) * 2 + usize::from(auto)], 3);
        emit(STYLE_OUT, if tree { ORGANIC } else { style }, 3);
    }
    if raw.has(IRON) {
        handled.insert(IRON);
        if !raw.bool(IRON) { handled.insert(IRON_TYPE); emit(IRON_TYPE, NO_IRON, 10); }
    }
    if raw.has(LABEL) {
        handled.insert(LABEL);
        emit(LABEL, if raw.str(LABEL) == LABEL_VALUES[1] { "1" } else { "0" }, 11);
        emit(EXCLUDE, if raw.str(LABEL) == LABEL_VALUES[2] { "1" } else { "0" }, 11);
    }
    for &(key, targets) in REGIONS {
        if raw.has(key) {
            handled.insert(key);
            for target in targets { emit(target, if raw.str(key) == "1" { "0" } else { raw.str(key) }, 12); }
        }
    }
    for (k, v) in &raw.map {
        if !handled.contains(k.as_str()) && ![RAMP, LIFT, MAX_LIFT, SLOPE, BED, FIRST_BED, INHERITS, ALIAS, RENAMED].contains(&k.as_str()) {
            put(&mut out, &mut report, mapped(k), decode(mapped(k), v.at(0)), None);
        }
    }
    for (key, value, row) in computed { put(&mut out, &mut report, &key, value, Some(row)); }
    if [RAMP, LIFT, MAX_LIFT, SLOPE].iter().any(|k| raw.has(k)) {
        // These source values are numeric vectors even when their mapped key
        // is absent. Their output vector cardinality follows the input slots.
        let numbers = |k| raw.str(k).split(',').map(|s| s.trim().to_string()).collect::<Vec<_>>();
        let ramps = numbers(RAMP); let lifts = numbers(LIFT); let max = numbers(MAX_LIFT); let slopes = numbers(SLOPE);
        let n = [ramps.len(), lifts.len(), max.len(), slopes.len(), base.vec_len(HOP)].into_iter().max().unwrap_or(1);
        let at = |v: &Vec<String>, i| v.get(i).unwrap_or(&v[0]).clone();
        let mut heights = Vec::new(); let mut types = Vec::new();
        for i in 0..n {
            let ramp = if raw.has(RAMP) { at(&ramps, i) == "1" } else { base.raw(HOP_TYPES, i) == SLOPE_LIFT };
            heights.push(if ramp && raw.has(MAX_LIFT) { at(&max, i) } else if !ramp && raw.has(LIFT) { at(&lifts, i) } else { base.raw(HOP, i).into() });
            let slope = at(&slopes, i).parse::<f64>().unwrap_or(0.0);
            let sloped = if raw.has(SLOPE) { slope > 0.0 && slope < 90.0 } else { base.raw(HOP_TYPES, i) == SLOPE_LIFT };
            types.push(if ramp && sloped { SLOPE_LIFT } else { NORMAL_LIFT }.into());
        }
        put(&mut out, &mut report, HOP, Value::Vector(heights), Some(8));
        put(&mut out, &mut report, HOP_TYPES, Value::Vector(types), Some(8));
        if ramps.iter().any(|s| s == "1") && raw.has(SLOPE) { put(&mut out, &mut report, SLOPE, Value::Vector(slopes), Some(8)); }
    }
    if LIFT_BOUNDS.iter().any(|k| raw.has(k)) || raw.has(LIFT) || raw.has(RAMP) {
        put(&mut out, &mut report, ENFORCE, Value::Scalar(ALL_SURFACES.into()), Some(8));
    }
    for (source, first) in [(BED, false), (FIRST_BED, true)] {
        if !raw.has(source) { continue; }
        for (_, key) in crate::machine::PLATES {
            let key = if first { format!("{key}_initial_layer") } else { key.to_string() };
            put(&mut out, &mut report, &key, decode(&key, raw.str(source)), Some(13));
        }
    }
    if (raw.has(BED) || raw.has(FIRST_BED)) && out.has("hot_plate_temp") && out.has("hot_plate_temp_initial_layer") {
        let bed = out.get("hot_plate_temp").unwrap().as_vec();
        let first = out.get("hot_plate_temp_initial_layer").unwrap().as_vec();
        let inherited = out.remove(UNHEATED).is_some();
        let cold = |v: &[String], i: usize| v.get(i).or(v.first()).and_then(|s| crate::config::parse_num(s)) == Some(0.0);
        let flags: Vec<String> = (0..bed.len().max(first.len())).map(|i| if cold(&bed, i) && cold(&first, i) { "1" } else { "0" }.into()).collect();
        // Over an unheated parent the preset says either way; on its own it marks only a cold bed.
        if inherited || flags.iter().any(|f| f == "1") { put(&mut out, &mut report, UNHEATED, Value::Vector(flags), Some(13)); }
    }
    (out, report)
}

fn merge(base: &mut Section, child: &Section, inherit: bool) {
    for (key, value) in &child.values.map {
        if inherit && key == RENAMED { continue; }
        if key.starts_with(CUSTOM) {
            if let (Some(old), Ok(makepad_strict_json::Value::Obj(new))) = (base.values.get(key), makepad_strict_json::parse(value.at(0).as_bytes())) {
                if let Ok(makepad_strict_json::Value::Obj(mut fields)) = makepad_strict_json::parse(old.at(0).as_bytes()) {
                    for (k, v) in new { fields.retain(|(old, _)| old != &k); fields.push((k, v)); }
                    base.values.set_str(key, makepad_strict_json::Value::Obj(fields).to_json());
                    continue;
                }
            }
        }
        base.values.set(key, value.clone());
    }
    base.withheld.extend(child.withheld.iter().cloned());
}

fn flatten(index: usize, sections: &[Section], names: &BTreeMap<(String, String), usize>, visiting: &mut BTreeSet<usize>, cache: &mut BTreeMap<usize, Result<Section, String>>) -> Result<Section, String> {
    if let Some(done) = cache.get(&index) { return done.clone(); }
    let child = &sections[index];
    if !visiting.insert(index) { return Err(format!("{}: inheritance cycle", child.name)); }
    let result = (|| {
        let mut flat = Section { kind: child.kind.clone(), name: child.name.clone(), ..Default::default() };
        for parent in child.values.str(INHERITS).split(';').map(str::trim).filter(|s| !s.is_empty()) {
            let i = names.get(&(child.kind.clone(), parent.into())).ok_or_else(|| format!("{}: unknown parent {parent}", child.name))?;
            merge(&mut flat, &flatten(*i, sections, names, visiting, cache)?, true);
        }
        merge(&mut flat, child, false);
        Ok(flat)
    })();
    visiting.remove(&index);
    cache.insert(index, result.clone());
    result
}

fn push(section: Section, origin: &str, batch: &mut ImportBatch) {
    let parent = section.values.str(INHERITS).to_string();
    let (values, mut report) = if parent.is_empty() { translate(&section.values) } else { (Config::new(), KeyReport::default()) };
    report.withheld.extend(section.withheld);
    let name = if section.values.str(ALIAS).is_empty() { section.name } else { section.values.str(ALIAS).into() };
    let notes = compatibility_notes(&values);
    let preset = RawPreset { kind: section.kind, name, values, inherits: parent, renamed_from: strings(section.values.str(RENAMED)), instantiation: true, ..Default::default() };
    batch.presets.push(ImportedPreset { preset, report, origin: origin.into(), notes, skip: false, renamed: None, ini_values: Some(section.values) });
}

/// What a flat preset stands on: the INI keys of its ancestors in this import (its own on
/// top), the values below them, and the installed preset the chain ends on, if any.
#[derive(Default)]
struct Ancestry { raw: Section, base: Config, installed: Option<(String, Config)>, notes: Vec<String> }

// A flat file has one parent. Keep its source keys until that parent is
// available; a catalogue parent already supplies canonical values as fallbacks.
fn effective(index: usize, presets: &[ImportedPreset], store: &ProfileStore, visiting: &mut BTreeSet<usize>) -> Result<Ancestry, String> {
    let ip = &presets[index];
    if !visiting.insert(index) { return Err(format!("{}: inheritance cycle", ip.preset.name)); }
    let result = (|| {
        let mut a = Ancestry::default();
        let parent = &ip.preset.inherits;
        if !parent.is_empty() {
            if let Some(i) = presets.iter().position(|p| p.preset.kind == ip.preset.kind && (p.preset.name == *parent || p.preset.renamed_from.contains(parent))) {
                let up = effective(i, presets, store, visiting)?;
                let mut raw = Section::default();
                merge(&mut raw, &up.raw, true);
                a = Ancestry { raw, ..up };
            } else {
                let name = if store.raw(parent).is_ok() { Some(parent.clone()) } else {
                    store.names().find(|name| store.raw(name).is_ok_and(|p| p.kind == ip.preset.kind && p.renamed_from.contains(parent))).cloned()
                };
                if let Some(name) = name {
                    a.base = store.resolve(&name)?.values;
                    a.installed = Some((name, a.base.clone()));
                } else {
                    a.notes.push(format!("its parent \"{parent}\" is not installed: imported with every value it carries"));
                }
            }
        }
        if let Some(raw) = &ip.ini_values {
            merge(&mut a.raw, &Section { values: raw.clone(), ..Default::default() }, false);
        } else {
            // A preset of Fab's own in the chain: its INI ancestors' keys go under its values.
            a.base = translate_over(&a.raw.values, &a.base).0;
            a.raw = Section::default();
            crate::profiles::merge_child(&mut a.base, &ip.preset.values);
        }
        Ok(a)
    })();
    visiting.remove(&index);
    result
}

pub(crate) fn settle(batch: &mut ImportBatch, store: &ProfileStore) {
    if !batch.presets.iter().any(|p| p.ini_values.is_some() && !p.preset.inherits.is_empty()) {
        for ip in &mut batch.presets { ip.ini_values = None; }
        return;
    }
    let sources = batch.presets.clone();
    let mut failed = BTreeSet::new();
    for (index, ip) in batch.presets.iter_mut().enumerate() {
        if ip.ini_values.take().is_none() || ip.preset.inherits.is_empty() { continue; }
        match effective(index, &sources, store, &mut BTreeSet::new()) {
            Ok(a) => {
                let (mut values, mut report) = translate_over(&a.raw.values, &a.base);
                ip.notes.extend(a.notes);
                ip.notes.extend(compatibility_notes(&values));
                // On an installed parent the preset keeps it and holds only what it changes:
                // nothing of the parent's is copied out of the catalogue.
                ip.preset.inherits = match a.installed {
                    Some((name, installed)) => {
                        values.map.retain(|k, v| installed.get(k) != Some(v));
                        name
                    }
                    None => String::new(),
                };
                report.withheld = std::mem::take(&mut ip.report.withheld);
                report.known = values.map.len();
                ip.preset.values = values;
                ip.report = report;
            }
            Err(error) => { batch.errors.push(error); failed.insert(index); }
        }
    }
    let mut index = 0;
    batch.presets.retain(|_| { let keep = !failed.contains(&index); index += 1; keep });
}

fn compatibility_notes(values: &Config) -> Vec<String> {
    let mut notes = Vec::new();
    if values.get(COMPAT).is_none_or(|v| v.as_vec().iter().all(|s| s.is_empty())) && !values.str(CONDITION).is_empty() {
        notes.push("compatible_printers_condition is kept but not evaluated by the catalogue".into());
    }
    notes
}

pub(crate) fn read(r: &mut impl BufRead, path: &Path, origin: &str, project: bool, gcode: bool, batch: &mut ImportBatch) -> bool {
    let errors_before = batch.errors.len();
    let (mut sections, found) = match read_sections(r, gcode, project || gcode, batch) {
        Ok(s) => s,
        Err(_) => { batch.errors.push(format!("{origin}: unreadable INI presets")); return true; }
    };
    if gcode && !found { return false; }
    if batch.errors.len() != errors_before { return true; }
    for section in &mut sections { normalize_aliases(&mut section.values); }
    if project {
        let flat = &sections[0];
        let (values, report) = translate(&flat.values);
        let start = batch.presets.len();
        crate::import::split_project(&values, origin, batch);
        if let Some(p) = batch.presets.get_mut(start + 1) {
            p.report.unknown.extend(report.unknown);
            p.report.withheld.extend(flat.withheld.iter().cloned());
        }
        for p in &mut batch.presets[start..] { p.notes.extend(compatibility_notes(&p.preset.values)); }
        return true;
    }
    let bundle = sections.iter().any(|s| s.kind == VENDOR);
    if !bundle {
        if sections.len() != 1 { batch.errors.push(format!("{origin}: typed INI presets require a bundle section")); return true; }
        let mut section = sections.into_iter().next().unwrap();
        let folder = path.parent().and_then(Path::file_name).and_then(|s| s.to_str()).unwrap_or("");
        let kind = SECTIONS.iter().find(|(k, _)| *k == folder).map(|(_, k)| *k)
            .or_else(|| IDS.iter().find(|(k, _)| section.values.has(k)).map(|(_, k)| *k))
            .or_else(|| {
                let mut kinds = BTreeSet::new();
                for key in section.values.map.keys() {
                    if let Some(def) = options().get(mapped(key)) {
                        match def.preset { PresetKind::Machine => { kinds.insert("machine"); }, PresetKind::Process => { kinds.insert("process"); }, PresetKind::Filament => { kinds.insert("filament"); }, _ => {} }
                    }
                }
                (kinds.len() == 1).then(|| *kinds.first().unwrap())
            });
        let Some(kind) = kind else { batch.errors.push(format!("{origin}: cannot identify preset kind")); return true; };
        section.kind = kind.into();
        section.name = IDS.iter().find(|(_, k)| *k == kind).map(|(id, _)| section.values.str(id)).filter(|s| !s.is_empty()).map(str::to_string)
            .unwrap_or_else(|| path.file_stem().unwrap_or_default().to_string_lossy().into());
        push(section, origin, batch);
        return true;
    }
    for section in sections.iter().filter(|s| s.kind == VENDOR || s.kind == MODEL) {
        for key in section.values.map.keys() {
            batch.notes.push(format!("section {}: metadata key {key}", section.kind));
        }
    }
    let mut names = BTreeMap::new();
    for (i, s) in sections.iter().enumerate().filter(|(_, s)| SECTIONS.iter().any(|(k, _)| *k == s.kind)) {
        names.insert((s.kind.clone(), s.name.clone()), i);
        for old in strings(s.values.str(RENAMED)) { names.entry((s.kind.clone(), old)).or_insert(i); }
    }
    let mut cache = BTreeMap::new();
    let start = batch.presets.len();
    let mut aliases = BTreeMap::new();
    for (i, s) in sections.iter().enumerate() {
        let Some((_, kind)) = SECTIONS.iter().find(|(k, _)| *k == s.kind) else { continue; };
        let mut flat = match flatten(i, &sections, &names, &mut BTreeSet::new(), &mut cache) {
            Ok(s) => s, Err(e) => { batch.errors.push(e); continue; }
        };
        if s.name.starts_with('*') && s.name.ends_with('*') { continue; }
        if s.kind == SECTIONS[0].0 {
            let model = sections.iter().find(|m| m.kind == MODEL && m.name == flat.values.str(MODEL));
            match model {
                None => { batch.errors.push(format!("{}: undeclared printer model", s.name)); continue; }
                Some(m) if !m.values.str(VARIANTS).split(';').map(str::trim).any(|v| !v.is_empty() && v == flat.values.str(VARIANT)) => {
                    batch.errors.push(format!("{}: undeclared printer variant", s.name)); continue;
                }
                _ => {}
            }
        }
        if s.kind == SECTIONS[0].0 && !flat.values.str(ALIAS).is_empty() {
            aliases.insert(s.name.clone(), flat.values.str(ALIAS).to_string());
        }
        flat.kind = (*kind).into();
        flat.values.remove(INHERITS);
        push(flat, origin, batch);
    }
    for preset in &mut batch.presets[start..] {
        if let Some(Value::Vector(names)) = preset.preset.values.map.get_mut(COMPAT) {
            for name in names { if let Some(alias) = aliases.get(name) { *name = alias.clone(); } }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(pairs: &[(&str, &str)]) -> Config {
        let mut c = Config::new();
        for (k, v) in pairs { c.set_str(k, *v); }
        c
    }

    fn text(pairs: &[(&str, &str)]) -> String {
        pairs.iter().map(|(k, v)| format!("{k} = {v}\n")).collect()
    }

    fn import(text: &str, path: &str) -> ImportBatch {
        let mut batch = ImportBatch::default();
        read(&mut text.as_bytes(), Path::new(path), path, false, false, &mut batch);
        batch
    }

    #[test]
    fn syntax_keeps_plain_strings_and_decodes_typed_vectors() {
        let c = raw(&[
            ("filament_type", r#"PLA;"a b";"a;b";"quote\"slash\\\n\r""#),
            ("nozzle_diameter", "0.4, 0.6"), ("start_gcode", r"G28\nG1 X2\r\\"),
            ("start_filament_gcode", r#"G28\nG1;"G2\nG3\\n""#),
            ("printer_notes", "a; b"), (INHERITS, "a; b"), (CONDITION, "a; b"),
            ("unknown_text", r"a;b\n"),
        ]);
        let (values, report) = translate(&c);
        assert_eq!(values.get("filament_type").unwrap().as_vec(), ["PLA", "a b", "a;b", "quote\"slash\\\n\r"]);
        assert_eq!(values.get("nozzle_diameter").unwrap().as_vec(), ["0.4", "0.6"]);
        assert_eq!(values.str("machine_start_gcode"), "G28\nG1 X2\r\\");
        assert_eq!(values.get("filament_start_gcode").unwrap().as_vec(), ["G28\nG1", "G2\nG3\\n"]);
        assert_eq!(values.str("printer_notes"), "a; b");
        assert_eq!(values.str(CONDITION), "a; b");
        assert_eq!(decode(INHERITS, "a; b"), Value::Scalar("a; b".into()));
        assert_eq!(decode(VARIANTS, "0.4; 0.6"), Value::Scalar("0.4; 0.6".into()));
        assert!(report.unknown.contains(&"unknown_text".into()));
        assert!(!values.has("unknown_text"));
        let b = import("# comment\n\n perimeters = 3\n# another\n", "print/Mine.ini");
        assert_eq!(b.presets[0].preset.values.str("wall_loops"), "3");
    }

    #[test]
    fn every_value_row_is_computed_then_gated_by_the_option_table() {
        let cases: &[(&[(&str, &str)], &[(&str, &str)], u8)] = &[
            (&[("external_perimeters_first", "0")], &[("wall_sequence", "inner wall/outer wall")], 1),
            (&[("external_perimeters_first", "1")], &[("wall_sequence", "outer wall/inner wall")], 1),
            (&[("complete_objects", "0")], &[("print_sequence", "by layer")], 2),
            (&[("complete_objects", "1")], &[("print_sequence", "by object")], 2),
            (&[(SUPPORT, "1"), (SUPPORT_AUTO, "1"), (SUPPORT_STYLE, GRID)], &[("enable_support", "1"), (SUPPORT_TYPE, "normal(auto)"), (STYLE_OUT, GRID)], 3),
            (&[(SUPPORT, "1"), (SUPPORT_AUTO, "0"), (SUPPORT_STYLE, "snug")], &[(SUPPORT_TYPE, "normal(manual)"), (STYLE_OUT, "snug")], 3),
            (&[(SUPPORT, "1"), (SUPPORT_AUTO, "1"), (SUPPORT_STYLE, "tree")], &[(SUPPORT_TYPE, "tree(auto)"), (STYLE_OUT, ORGANIC)], 3),
            (&[(SUPPORT, "1"), (SUPPORT_AUTO, "0"), (SUPPORT_STYLE, ORGANIC)], &[(SUPPORT_TYPE, "tree(manual)"), (STYLE_OUT, ORGANIC)], 3),
            (&[(SUPPORT, "0"), (SUPPORT_AUTO, "1"), (SUPPORT_STYLE, ORGANIC), (SUPPORT_LAYERS, "4")], &[("enable_support", "0"), (SUPPORT_TYPE, "tree(manual)"), ("enforce_support_layers", "4")], 3),
            (&[(SUPPORT_LAYERS, "2")], &[(SUPPORT_TYPE, "normal(manual)"), ("enforce_support_layers", "2")], 3),
            (&[("ensure_vertical_shell_thickness", "enabled")], &[("ensure_vertical_shell_thickness", "ensure_all")], 4),
            (&[("ensure_vertical_shell_thickness", "partial")], &[("ensure_vertical_shell_thickness", "ensure_all")], 4),
            (&[("ensure_vertical_shell_thickness", "disabled")], &[("ensure_vertical_shell_thickness", "ensure_critical_only")], 4),
            (&[("fill_pattern", "monotoniclines")], &[("sparse_infill_pattern", "monotonicline")], 5),
            (&[("top_fill_pattern", "stars"), ("bottom_fill_pattern", "zigzag")], &[("top_surface_pattern", "tri-hexagon"), ("bottom_surface_pattern", "zigzag")], 5),
            (&[("solid_fill_pattern", "monotonic"), ("external_fill_pattern", "rectilinear")], &[("top_surface_pattern", "monotonic"), ("bottom_surface_pattern", "rectilinear")], 5),
            (&[("seam_position", "rear")], &[("seam_position", "back")], 6),
            (&[("top_one_perimeter_type", "none")], &[("only_one_wall_top", "0")], 7),
            (&[("top_one_perimeter_type", "top")], &[("only_one_wall_top", "1")], 7),
            (&[("top_one_perimeter_type", "topmost"), ("only_one_perimeter_first_layer", "1")], &[("only_one_wall_top", "1"), ("only_one_wall_first_layer", "1")], 7),
            (&[(RAMP, "0"), (LIFT, "0.4"), (MAX_LIFT, "2"), (SLOPE, "20")], &[(HOP, "0.4"), (HOP_TYPES, NORMAL_LIFT)], 8),
            (&[(RAMP, "1"), (LIFT, "0.4"), (MAX_LIFT, "2"), (SLOPE, "20")], &[(HOP, "2"), (HOP_TYPES, SLOPE_LIFT), (SLOPE, "20"), (ENFORCE, ALL_SURFACES)], 8),
            (&[(RAMP, "1"), (MAX_LIFT, "2"), (SLOPE, "0")], &[(HOP_TYPES, NORMAL_LIFT)], 8),
            (&[(RAMP, "1"), (MAX_LIFT, "2"), (SLOPE, "90")], &[(HOP_TYPES, NORMAL_LIFT)], 8),
            (&[(RAMP, "1"), (MAX_LIFT, "2"), (SLOPE, "-1")], &[(HOP_TYPES, NORMAL_LIFT)], 8),
            (&[(RAMP, "1"), (MAX_LIFT, "2"), (SLOPE, "91")], &[(HOP_TYPES, NORMAL_LIFT)], 8),
            (&[(LIFT_BOUNDS[0], "1"), (LIFT_BOUNDS[1], "5")], &[(LIFT_BOUNDS[0], "1"), (LIFT_BOUNDS[1], "5"), (ENFORCE, ALL_SURFACES)], 8),
            (&[("machine_limits_usage", "emit_to_gcode")], &[("emit_machine_limits_to_gcode", "1")], 9),
            (&[("machine_limits_usage", "time_estimate_only")], &[("emit_machine_limits_to_gcode", "0")], 9),
            (&[("machine_limits_usage", "ignore")], &[("emit_machine_limits_to_gcode", "0")], 9),
            (&[(IRON, "0"), (IRON_TYPE, "solid")], &[(IRON_TYPE, NO_IRON)], 10),
            (&[(IRON, "1"), (IRON_TYPE, "top")], &[(IRON_TYPE, "top")], 10),
            (&[(IRON, "1"), (IRON_TYPE, "topmost")], &[(IRON_TYPE, "topmost")], 10),
            (&[(IRON, "1"), (IRON_TYPE, "solid")], &[(IRON_TYPE, "solid")], 10),
            (&[(LABEL, LABEL_VALUES[0])], &[(LABEL, "0"), (EXCLUDE, "0")], 11),
            (&[(LABEL, LABEL_VALUES[1])], &[(LABEL, "1"), (EXCLUDE, "0")], 11),
            (&[(LABEL, LABEL_VALUES[2])], &[(LABEL, "0"), (EXCLUDE, "1")], 11),
        ];
        for (input, expected, row) in cases {
            let (computed, _) = convert(&raw(input));
            let (values, report) = translate(&raw(input));
            for (key, value) in *expected {
                assert_eq!(computed.str(key), *value, "row {row}, {key}, {input:?}");
                if options().get(key).is_some() { assert_eq!(values.str(key), *value, "row {row}, {key}"); }
                else {
                    assert!(!values.has(key), "row {row}, {key}");
                    assert!(report.unknown.contains(&format!("{key} (row {row})")), "{report:?}");
                }
            }
        }
        for (source, targets) in REGIONS {
            for (input, expected) in [("1", "0"), ("2", "2"), ("4", "4")] {
                let (computed, _) = convert(&raw(&[(source, input)]));
                let (values, report) = translate(&raw(&[(source, input)]));
                for target in *targets {
                    assert_eq!(computed.str(target), expected);
                    assert!(!values.has(target));
                    assert!(report.unknown.contains(&format!("{target} (row 12)")));
                }
            }
        }
        for pair in [("0", "0"), ("55", "60"), ("0", "60")] {
            let input = raw(&[(BED, pair.0), (FIRST_BED, pair.1)]);
            let (computed, _) = convert(&input);
            let (values, report) = translate(&input);
            for (_, key) in crate::machine::PLATES {
                for (key, temperature) in [(key.to_string(), pair.0), (format!("{key}_initial_layer"), pair.1)] {
                    assert_eq!(computed.str(&key), temperature, "row 13: {key}");
                    // Every plate is a key Fab knows: the pair lands on all of them.
                    assert_eq!(values.str(&key), temperature, "row 13: {key}");
                    assert!(!report.unknown.iter().any(|u| u.starts_with(&format!("{key} "))));
                }
                if pair == ("0", "0") { assert!(crate::machine::filament_suits_plate(values.bool(UNHEATED), values.f64(key))); }
            }
            assert_eq!(values.has(UNHEATED), pair == ("0", "0"));
        }
        let (values, _) = translate(&raw(&[(RAMP, "0,1,1"), (LIFT, "0.4,0.5,0.6"), (MAX_LIFT, "1,2,3"), (SLOPE, "10,30,90")]));
        assert_eq!(values.get(HOP).unwrap().as_vec(), ["0.4", "2", "3"]);
        assert_eq!(values.get(HOP_TYPES).unwrap().as_vec(), [NORMAL_LIFT, SLOPE_LIFT, NORMAL_LIFT]);
        let (values, _) = translate(&raw(&[(BED, "0,60"), (FIRST_BED, "0,65")]));
        assert_eq!(values.get(UNHEATED).unwrap().as_vec(), ["1", "0"]);
    }

    #[test]
    fn bundles_flatten_validate_and_withhold_before_decoding() {
        let section = |kind: &str, name: &str, pairs: &[(&str, &str)]| format!("[{kind}:{name}]\n{}", text(pairs));
        let mut fixture = format!("[{VENDOR}]\n");
        fixture += &section(MODEL, "X", &[(VARIANTS, "0.4; 0.6")]);
        fixture += &section(SECTIONS[0].0, "*common*", &[(MODEL, "X"), (VARIANT, "0.4"), ("max_print_height", "100"), (RENAMED, "old base"), ("custom_parameters_print", r#"{"a":1,"b":1}"#)]);
        fixture += &section(SECTIONS[0].0, "*later*", &[("max_print_height", "200"), ("custom_parameters_print", r#"{"b":2}"#)]);
        fixture += &section(SECTIONS[0].0, "Leaf", &[(INHERITS, "*common*; *later*"), (ALIAS, "Display"), ("nozzle_diameter", "0.6"), (VARIANT, "0.6"), ("custom_parameters_print", r#"{"c":3}"#), (HOST, "private-host-sentinel"), ("printhost_apikey", "private-key-sentinel")]);
        fixture += &section(SECTIONS[1].0, "Fine", &[("perimeters", "3"), (COMPAT, "Leaf;Other"), (CONDITION, "some;condition")]);
        fixture += &section(SECTIONS[2].0, "Cool", &[(BED, "0"), (FIRST_BED, "0"), (CONDITION, "some;condition")]);
        fixture += &section(SECTIONS[0].0, "Loop a", &[(INHERITS, "Loop b")]);
        fixture += &section(SECTIONS[0].0, "Loop b", &[(INHERITS, "Loop a")]);
        fixture += &section(SECTIONS[0].0, "Missing parent", &[(INHERITS, "Absent")]);
        fixture += &section(SECTIONS[0].0, "Missing model", &[(MODEL, "Undeclared"), (VARIANT, "0.4")]);
        fixture += &section(SECTIONS[0].0, "Missing variant", &[(MODEL, "X"), (VARIANT, "0.8")]);
        fixture += &section(SKIP_SECTIONS[0], "private-label-sentinel", &[(HOST, "private-host-sentinel"), ("printhost_apikey", "private-key-sentinel"), ("ordinary", "private-value-sentinel")]);
        fixture += &format!("[{}]\n{}[{}]\n{}[{}:Resin]\n{}", SKIP_SECTIONS[1], text(&[("user_login", "private-value-sentinel")]), OBSOLETE, text(&[("old", "ignored")]), RESIN_SECTIONS[0], text(&[("layer_height", "0.05")]));
        let b = import(&fixture, "bundle.ini");
        assert_eq!(b.presets.len(), 3, "{b:?}");
        let printer = &b.presets[0];
        assert_eq!(printer.preset.name, "Display");
        assert_eq!(printer.preset.values.str("printable_height"), "200");
        assert_eq!(printer.preset.values.str("nozzle_diameter"), "0.6");
        assert!(b.presets.iter().all(|p| !p.preset.name.starts_with('*')));
        for reason in ["inheritance cycle", "unknown parent", "undeclared printer model", "undeclared printer variant"] { assert!(b.errors.iter().any(|e| e.contains(reason)), "{reason}: {b:?}"); }
        assert!(b.presets[1].notes.is_empty(), "the explicit list decides");
        assert_eq!(b.presets[1].preset.values.get(COMPAT).unwrap().as_vec(), ["Display", "Other"]);
        assert!(b.presets[2].notes.iter().any(|n| n.contains("not evaluated")));
        let shown = format!("{b:?}\n{}", b.report());
        for secret in ["private-host-sentinel", "private-key-sentinel", "private-label-sentinel", "private-value-sentinel"] { assert!(!shown.contains(secret)); }
        for key in [HOST, "printhost_apikey", "user_login"] { assert!(b.report().contains(key)); }
        assert!(b.report().contains(SKIP_SECTIONS[0]));
        // The JSON merge precedes the table gate (these keys are unknown to Fab).
        let (sections, _) = read_sections(&mut fixture.as_bytes(), false, false, &mut ImportBatch::default()).unwrap();
        let names = sections.iter().enumerate().map(|(i, s)| ((s.kind.clone(), s.name.clone()), i)).collect();
        let leaf = sections.iter().position(|s| s.name == "Leaf").unwrap();
        let flat = flatten(leaf, &sections, &names, &mut BTreeSet::new(), &mut BTreeMap::new()).unwrap();
        assert!(!flat.values.has(RENAMED));
        let merged = makepad_strict_json::parse(flat.values.str("custom_parameters_print").as_bytes()).unwrap();
        assert_eq!(merged.to_json(), r#"{"a":1,"b":2,"c":3}"#);
        // Invalid UTF-8 in withheld values is never decoded.
        let mut bytes = text(&[("perimeters", "2")]).into_bytes();
        for key in [HOST, "printhost_future", "login_data", "user_name", "token", "password", "apikey", "cloud_id"] {
            bytes.extend_from_slice(format!("{key} = ").as_bytes()); bytes.extend_from_slice(&[0xff, 0xfe, b'\n']);
        }
        let mut b = ImportBatch::default();
        read(&mut bytes.as_slice(), Path::new("print/private.ini"), "private.ini", false, false, &mut b);
        assert!(b.errors.is_empty());
        assert_eq!(b.presets[0].report.withheld.len(), 8);
    }

    #[test]
    fn embedded_sections_are_withheld_before_values_are_read() {
        let safe = text(&[(IDS[1].0, "Fine"), ("perimeters", "3")]);
        let hidden = format!("[{}]\n{}[{}:x]\n{}", SKIP_SECTIONS[1],
            text(&[("print_settings_id", "selection-sentinel")]), SKIP_SECTIONS[0],
            text(&[(HOST, "host-sentinel"), ("printhost_apikey", "key-sentinel"), ("printer_notes", "section-sentinel")]));
        let cfg = format!("{safe}{hidden}");
        let commented = cfg.lines().map(|l| format!("; {l}\n")).collect::<String>();
        for (project, gcode, input) in [
            (false, false, cfg.clone()),
            (true, false, cfg),
            (true, false, commented.clone()),
            (true, true, format!("{CONFIG_BEGIN}\n{commented}{CONFIG_END}\n")),
        ] {
            let mut b = ImportBatch::default();
            read(&mut input.as_bytes(), Path::new("print/Fine.ini"), "fixture", project, gcode, &mut b);
            assert!(b.errors.is_empty(), "{:?}", b.errors);
            assert!(!b.presets.is_empty());
            for shown in [format!("{:?}", b.presets), b.report(), format!("{b:?}")] {
                for hidden in ["selection-sentinel", "host-sentinel", "key-sentinel", "section-sentinel"] {
                    assert!(!shown.contains(hidden), "withheld value escaped");
                }
            }
            for section in SKIP_SECTIONS { assert!(b.report().contains(&format!("section {section}: withheld"))); }
        }
        // Decoding even one withheld value would reject these bytes.
        let input = format!("{CONFIG_BEGIN}\n; perimeters = 3\n; [presets]\n; print_settings_id = ");
        let mut bytes = input.into_bytes();
        bytes.extend_from_slice(&[0xff, b'\n']);
        bytes.extend_from_slice(format!("{CONFIG_END}\n").as_bytes());
        let mut b = ImportBatch::default();
        read(&mut bytes.as_slice(), Path::new("fixture.gcode"), "fixture", true, true, &mut b);
        assert!(b.errors.is_empty());
    }

    #[test]
    fn flat_children_translate_effective_values_from_import_or_catalogue() {
        use crate::import::{preset_json, settle};
        use crate::profiles::ProfileStore;
        let dir = std::env::temp_dir().join(format!("fab-ini-inheritance-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cases: &[(&str, &[(&str, &str)], &[(&str, &str)], &[(&str, &str)])] = &[
            ("filament", &[(BED, "0"), (FIRST_BED, "0")], &[(BED, "60"), (FIRST_BED, "65")], &[("hot_plate_temp", "60"), ("hot_plate_temp_initial_layer", "65")]),
            ("print", &[(SUPPORT, "1"), (SUPPORT_AUTO, "1"), (SUPPORT_STYLE, GRID)], &[(SUPPORT_STYLE, "snug")], &[("enable_support", "1"), (SUPPORT_TYPE, "normal(auto)"), (STYLE_OUT, "snug")]),
            ("printer", &[(RAMP, "1"), (MAX_LIFT, "1"), (LIFT, "0.4"), (SLOPE, "20")], &[(MAX_LIFT, "2")], &[(HOP, "2"), (HOP_TYPES, SLOPE_LIFT)]),
        ];
        for (folder, parent, child, expected) in cases {
            for installed in [false, true] {
                let mut b = import(&text(parent), &format!("{folder}/Base.ini"));
                let store = if installed {
                    std::fs::write(dir.join("base.json"), preset_json(&b.presets[0].preset)).unwrap();
                    b.presets.clear();
                    ProfileStore::open(&[], &[dir.clone()])
                } else { ProfileStore::open(&[], &[]) };
                let child = format!("{}{}", text(&[(INHERITS, "Base")]), text(child));
                read(&mut child.as_bytes(), Path::new(&format!("{folder}/Child.ini")), "child", false, false, &mut b);
                // A child may arrive before its parent in a combined import.
                b.presets.reverse();
                settle(&mut b, &store, "fixture");
                assert!(b.errors.is_empty(), "{:?}", b.errors);
                let child = &b.presets.iter().find(|p| p.preset.name == "Child").unwrap().preset;
                // What the job sees: the installed parent, then the child's own values.
                let mut values = if installed { store.resolve("Base").unwrap().values } else { Config::new() };
                crate::profiles::merge_child(&mut values, &child.values);
                assert_eq!(child.inherits, if installed { "Base" } else { "" }, "{folder}");
                for (key, value) in *expected {
                    assert_eq!(values.str(key), *value, "{folder}, installed={installed}, key={key}");
                }
                assert!(!values.bool(UNHEATED), "heated child must clear inherited unheated mark");
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn installed_parents_stay_in_the_catalogue() {
        use crate::import::{preset_json, settle};
        use crate::profiles::ProfileStore;
        let dir = std::env::temp_dir().join(format!("fab-ini-installed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |kind: &str, name: &str, values: Config| {
            let preset = RawPreset { kind: kind.into(), name: name.into(), values, instantiation: true, ..Default::default() };
            std::fs::write(dir.join(format!("{name}.json")), preset_json(&preset)).unwrap();
        };
        // A parent's unchanged values stay with it.
        write("machine", "Base", raw(&[(HOP, "1"), (HOP_TYPES, SLOPE_LIFT)]));
        // A parent with an empty bed list: a child's first-layer bed alone is no cold bed.
        let mut cold = raw(&[("hot_plate_temp_initial_layer", "0")]);
        cold.set_vec("hot_plate_temp", Vec::new());
        write("filament", "Cold", cold);
        let store = ProfileStore::open(&[], &[dir.clone()]);
        let mut b = ImportBatch::default();
        for (path, pairs) in [("printer/Child.ini", &[(INHERITS, "Base"), (MAX_LIFT, "2")][..]), ("filament/Warm.ini", &[(INHERITS, "Cold"), (FIRST_BED, "65")][..])] {
            read(&mut text(pairs).as_bytes(), Path::new(path), "child", false, false, &mut b);
        }
        settle(&mut b, &store, "fixture");
        assert!(b.errors.is_empty(), "{:?}", b.errors);
        let child = &b.presets.iter().find(|p| p.preset.name == "Child").unwrap().preset;
        assert_eq!((child.inherits.as_str(), child.values.str(HOP)), ("Base", "2"));
        assert!(!child.values.has(HOP_TYPES), "an unchanged value stays with the parent");
        let warm = &b.presets.iter().find(|p| p.preset.name == "Warm").unwrap().preset;
        assert!(!warm.values.bool(UNHEATED));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_child_of_an_installed_parent_holds_no_host_value() {
        use crate::import::{import_into, preset_json};
        use crate::profiles::ProfileStore;
        let dir = std::env::temp_dir().join(format!("fab-ini-host-{}", std::process::id()));
        let (installed, catalogue) = (dir.join("installed"), dir.join("catalogue"));
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::create_dir_all(dir.join("printer")).unwrap();
        let sentinels = ["host-sentinel", "key-sentinel"];
        let parent = RawPreset { kind: "machine".into(), name: "Base".into(), values: raw(&[(HOP, "1"), (HOST, sentinels[0]), ("printhost_apikey", sentinels[1])]), instantiation: true, ..Default::default() };
        std::fs::write(installed.join("Base.json"), preset_json(&parent)).unwrap();
        let child = dir.join("printer/Child.ini");
        std::fs::write(&child, text(&[(INHERITS, "Base"), (MAX_LIFT, "2")])).unwrap();
        let b = import_into(&child, &ProfileStore::open(&[], &[installed]), &catalogue);
        assert!(b.errors.is_empty(), "{:?}", b.errors);
        let stored = std::fs::read_to_string(catalogue.join("machine").join("Child.json")).unwrap();
        assert!(stored.contains("\"Base\"") && stored.contains(HOP), "the child keeps its parent and its own change");
        for shown in [stored, b.report(), format!("{b:?}")] {
            for sentinel in sentinels { assert!(!shown.contains(sentinel), "a parent's host value left the catalogue"); }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_preset_of_fabs_own_in_the_chain_keeps_its_place() {
        use crate::import::settle;
        use crate::profiles::ProfileStore;
        let mut b = import(&text(&[("perimeters", "2")]), "print/Grand.ini");
        let preset = RawPreset { kind: "process".into(), name: "Mid".into(), inherits: "Grand".into(), values: raw(&[("wall_loops", "4")]), instantiation: true, ..Default::default() };
        b.presets.push(ImportedPreset { preset, report: KeyReport::default(), origin: "fixture".into(), notes: Vec::new(), skip: false, renamed: None, ini_values: None });
        read(&mut text(&[(INHERITS, "Mid"), ("layer_height", "0.15")]).as_bytes(), Path::new("print/Child.ini"), "child", false, false, &mut b);
        settle(&mut b, &ProfileStore::open(&[], &[]), "fixture");
        let child = &b.presets.iter().find(|p| p.preset.name == "Child").unwrap().preset;
        assert_eq!((child.values.str("wall_loops"), child.values.str("layer_height")), ("4", "0.15"));
    }

    #[test]
    fn a_byte_order_mark_hides_no_key_from_the_filter() {
        let input = format!("\u{feff}{}", text(&[(HOST, "host-sentinel"), (INHERITS, "Base"), ("perimeters", "3")]));
        let b = import(&input, "print/Fine.ini");
        assert!(b.report().contains(HOST));
        assert!(!format!("{b:?}").contains("host-sentinel"));
    }

    #[test]
    fn child_canonical_patterns_override_parent_aliases() {
        let fixture = format!("[{VENDOR}]\n[print:Base]\nsolid_fill_pattern = stars\n[print:Child]\ninherits = Base\ntop_fill_pattern = monotonic\n");
        let b = import(&fixture, "bundle.ini");
        assert!(b.errors.is_empty());
        let child = &b.presets.iter().find(|p| p.preset.name == "Child").unwrap().preset;
        assert_eq!(child.values.str("top_surface_pattern"), "monotonic");
    }

    #[test]
    fn old_names_resolve_after_reopening_the_imported_catalogue() {
        use crate::import::import_into;
        use crate::profiles::ProfileStore;
        let dir = std::env::temp_dir().join(format!("fab-ini-renames-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("print")).unwrap();
        let bundle = dir.join("bundle.ini");
        std::fs::write(&bundle, format!("[{VENDOR}]\n[print:New]\n{RENAMED} = Old\nperimeters = 4\n")).unwrap();
        let catalogue = dir.join("catalogue");
        let b = import_into(&bundle, &ProfileStore::open(&[], &[]), &catalogue);
        assert!(b.errors.is_empty());
        let child = dir.join("print/Child.ini");
        std::fs::write(&child, text(&[(INHERITS, "Old"), ("layer_height", "0.15")])).unwrap();
        let store = ProfileStore::open(&[], &[catalogue.clone()]);
        let b = import_into(&child, &store, &catalogue);
        assert!(b.errors.is_empty());
        let store = ProfileStore::open(&[], &[catalogue]);
        let child = store.resolve("Child").unwrap();
        assert_eq!(child.values.str("wall_loops"), "4");
        assert_eq!(child.values.str("layer_height"), "0.15");
        let own = store.raw("Child").unwrap();
        assert!(!crate::import::preset_json(&own).contains("Old"), "old names are not inherited");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unheated_mark_reaches_only_its_own_job_filament() {
        use crate::profiles::{JobSetup, Preset};
        let imported = import(&text(&[(BED, "0"), (FIRST_BED, "0")]), "filament/Cold.ini");
        let cold = Preset::from_flat(imported.presets[0].preset.clone());
        let imported = import(&text(&[(BED, "60"), (FIRST_BED, "65")]), "filament/Warm.ini");
        let warm = Preset::from_flat(imported.presets[0].preset.clone());
        let setup = JobSetup {
            machine: Preset { values: raw(&[("nozzle_diameter", "0.4")]), ..Default::default() },
            process: Preset::default(), filaments: vec![cold, warm], filament_map: vec![1, 1],
            nozzle_volume_type: Vec::new(), bed_type: crate::machine::PLATES[0].0.into(),
            filament_map_mode: String::new(), filament_colours: Vec::new(),
        };
        let job = crate::machine::Job::new(setup.build_config());
        assert!(job.cfg.bool_at(UNHEATED, 0));
        assert!(!job.cfg.bool_at(UNHEATED, 1));
        for (_, key) in crate::machine::PLATES {
            for f in 0..2 {
                assert!(crate::machine::filament_suits_plate(job.cfg.bool_at(UNHEATED, f), job.cfg.f64_at(key, f)));
            }
        }
        assert!(!crate::machine::filament_suits_plate(job.cfg.bool_at(UNHEATED, 1), 0.0));
        assert_eq!((job.bed_temp(0, false), job.bed_temp(0, true)), (0.0, 0.0));
        assert_eq!((job.bed_temp(1, false), job.bed_temp(1, true)), (60.0, 65.0));
    }

    #[test]
    fn flat_and_container_configs_import_without_enabling_output() {
        use crate::import::read_file;
        let dir = std::env::temp_dir().join(format!("fab-ini-import-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("print")).unwrap();
        let preset = dir.join("print/Mine.ini");
        std::fs::write(&preset, text(&[("perimeters", "3"), (INHERITS, "Base")])).unwrap();
        let b = read_file(&preset);
        assert_eq!(b.presets[0].preset.kind, "process");
        assert_eq!(b.presets[0].preset.inherits, "Base");
        let pairs = [(IDS[0].0, "X"), (IDS[1].0, "Fine"), (IDS[2].0, "A;B"), (MODEL, "X"), ("nozzle_diameter", "0.4"), ("perimeters", "2"), ("temperature", "200,220"), (BED, "0,60"), (FIRST_BED, "0,65")];
        let cfg = text(&pairs);
        let gcode = dir.join("project.gcode");
        std::fs::write(&gcode, format!("G28\n{CONFIG_BEGIN}\n{}{CONFIG_END}\n", cfg.lines().map(|l| format!("; {l}\n")).collect::<String>())).unwrap();
        let mut zip = makepad_zip_file::ZipWriter::new();
        zip.add(CONFIG_PATH, cfg.as_bytes(), makepad_zip_file::ZipMethod::Store).unwrap();
        let project = dir.join("project.3mf");
        std::fs::write(&project, zip.finish().unwrap()).unwrap();
        for path in [&gcode, &project] {
            let b = read_file(path);
            assert!(b.errors.is_empty(), "{b:?}");
            assert_eq!(b.presets.iter().map(|p| p.preset.kind.as_str()).collect::<Vec<_>>(), ["machine", "process", "filament", "filament"]);
            assert_eq!(b.presets[2].preset.values.str("nozzle_temperature"), "200");
            assert_eq!(b.presets[3].preset.values.str("nozzle_temperature"), "220");
            assert!(b.presets[2].preset.values.bool(UNHEATED));
            assert!(!b.presets[3].preset.values.bool(UNHEATED));
            assert_eq!(crate::family::of_profile(&b.presets[0].preset.values).err(), Some(crate::family::UNSUPPORTED));
            assert!(b.tree.is_empty() && b.presets.iter().all(|p| p.preset.tree.is_empty()));
        }
        for (pairs, reason) in [(vec![(MODEL, "X"), ("nozzle_diameter", "0.4")], crate::family::UNSUPPORTED), (vec![("nozzle_diameter", "0.4")], crate::family::NO_MODEL)] {
            let b = import(&text(&pairs), "printer/Mine.ini");
            assert_eq!(crate::family::of_profile(&b.presets[0].preset.values).err(), Some(reason));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
