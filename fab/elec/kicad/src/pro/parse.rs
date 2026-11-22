//! Project parsing only refuses malformed JSON; unsupported facts stay in the report.

use super::file::*;
use crate::{json::{self, Value}, pcb::Located, report::{GapKind, ImportReport, Refusal, SourceFile}};
use std::collections::{HashMap, HashSet};

fn bad(report: &mut ImportReport, value: &Value, path: &str) {
    report.gap_value("project.bad_value", GapKind::Skipped, value.at, path.into());
}
/// The format writes `null` for an empty map or list; that is the same as leaving the key out.
fn present(value: Option<&Value>) -> Option<&Value> {
    value.filter(|v| !matches!(v.kind, json::Kind::Null))
}
fn object<'a>(value: Option<&'a Value>, path: &str, report: &mut ImportReport) -> Option<&'a Value> {
    present(value).filter(|v| { if v.object().is_some() { true } else { bad(report, v, path); false } })
}
fn array<'a>(value: Option<&'a Value>, path: &str, report: &mut ImportReport) -> Option<&'a [Value]> {
    present(value).and_then(|v| v.array().or_else(|| { bad(report, v, path); None }))
}
fn number(value: Option<&Value>, path: &str, report: &mut ImportReport) -> Option<Located<f64>> {
    present(value).and_then(|v| v.number().map(|value| Located { at: v.at, value }).or_else(|| { bad(report, v, path); None }))
}
fn string(value: Option<&Value>, path: &str, report: &mut ImportReport) -> Option<Located<String>> {
    present(value).and_then(|v| v.string().map(|s| Located { at: v.at, value: s.into() }).or_else(|| { bad(report, v, path); None }))
}
fn count_object(value: Option<&Value>, path: &str, report: &mut ImportReport) -> Option<Located<usize>> {
    object(value, path, report).and_then(|v| v.object().map(|o| Located { at: v.at, value: o.len() }))
}
fn version(value: Option<&Value>, path: &str, at: (u32, u32), report: &mut ImportReport) -> Located<i64> {
    let meta = object(value.and_then(|v| v.get("meta")), &format!("{path}>meta"), report);
    number(meta.and_then(|v| v.get("version")), &format!("{path}>meta>version"), report)
        .map_or(Located { at, value: 0 }, |v| Located { at: v.at, value: v.value as i64 })
}

pub fn parse_kicad_pro(bytes: &[u8]) -> Result<ProjectFile, Refusal> {
    let root = json::parse(bytes)?;
    let mut report = ImportReport::default();
    report.current = SourceFile::Project;
    let doc = object(Some(&root), "kicad_pro", &mut report);
    let board = object(doc.and_then(|v| v.get("board")), "kicad_pro>board", &mut report);
    let ds_path = "kicad_pro>board>design_settings";
    let ds = object(board.and_then(|v| v.get("design_settings")), ds_path, &mut report);
    let ns_path = "kicad_pro>net_settings";
    let ns = object(doc.and_then(|v| v.get("net_settings")), ns_path, &mut report);
    let design_version = version(ds, ds_path, ds.map_or(root.at, |v| v.at), &mut report);
    let net_version = version(ns, ns_path, ns.map_or(root.at, |v| v.at), &mut report);
    let mut out = ProjectFile {
        location: root.at, design_settings: ds.map(|v| v.at), design_version, net_version,
        rules: Vec::new(), size_presets: Vec::new(), exclusions: None, external_fillets: None,
        severities: Vec::new(), classes_location: None, classes: Vec::new(), assignments: Vec::new(), patterns: Vec::new(),
        text_variables: None, net_chains: Vec::new(), net_colors: None, component_classes: None, report,
    };
    if let Some(entries) = ds.and_then(Value::object) {
        for (key, v) in entries {
            let path = format!("{ds_path}>{key}");
            match key.as_str() {
                "rules" => if let Some(rules) = object(Some(v), &path, &mut out.report).and_then(Value::object) {
                    for (key, v) in rules {
                        let path = format!("{path}>{key}");
                        if RULES.iter().any(|r| r.0 == key) || matches!(key.as_str(), "min_text_thickness" | "solder_mask_clearance" | "solder_paste_clearance" | "solder_paste_margin_ratio" | "solder_mask_min_width") {
                            if let Some(n) = number(Some(v), &path, &mut out.report) { out.rules.push((key.clone(), n)); }
                        } else if !matches!(key.as_str(), "max_error" | "use_height_for_length_calcs") { out.report.unknown(path, v.at); }
                    }
                },
                "track_widths" | "via_dimensions" | "diff_pair_dimensions" | "via_stack_presets" => {
                    if array(Some(v), &path, &mut out.report).is_some_and(|a| !a.is_empty()) {
                        out.size_presets.push(Located { at: v.at, value: key.clone() });
                    }
                }
                "drc_exclusions" => {
                    out.exclusions = array(Some(v), &path, &mut out.report).map(|a| Located { at: v.at, value: a.len() });
                }
                "zones_allow_external_fillets" => {
                    out.external_fillets = v.boolean().map(|value| Located { at: v.at, value });
                    if out.external_fillets.is_none() { bad(&mut out.report, v, &path); }
                }
                "rule_severities" => if let Some(entries) = object(Some(v), &path, &mut out.report).and_then(Value::object) {
                    for (key, v) in entries {
                        if let Some(value) = string(Some(v), &format!("{path}>{key}"), &mut out.report) { out.severities.push((key.clone(), value)); }
                    }
                },
                "teardrop_options" | "teardrop_parameters" | "tuning_pattern_settings" | "defaults" | "meta" => (),
                key if key.starts_with("bom_") => (),
                _ => out.report.unknown(path, v.at),
            }
        }
    }
    let mut class_indices = HashMap::new();
    if let Some(value) = ns.and_then(|v| v.get("classes")) {
        if let Some(classes) = array(Some(value), "kicad_pro>net_settings>classes", &mut out.report) {
            out.classes_location = Some(value.at);
            for value in classes {
                let path = "kicad_pro>net_settings>classes";
                let Some(v) = object(Some(value), path, &mut out.report) else { continue };
                let Some(name) = string(v.get("name"), &format!("{path}>name"), &mut out.report) else {
                    if v.get("name").is_none() { bad(&mut out.report, v, &format!("{path}>name")); }
                    continue;
                };
                let mut class = ProjectClass { name, fields: std::array::from_fn(|_| None), priority: None, pcb_color: None, tuning_profile: None, wire_width: None, bus_width: None, line_style: None, schematic_color: None, nets: Vec::new() };
                for (i, key) in CLASS_FIELDS.iter().enumerate() {
                    class.fields[i] = number(v.get(key), &format!("{path}>{key}"), &mut out.report);
                }
                class.priority = number(v.get("priority"), &format!("{path}>priority"), &mut out.report).map(|v| Located { at: v.at, value: v.value.trunc() });
                class.pcb_color = string(v.get("pcb_color"), &format!("{path}>pcb_color"), &mut out.report);
                class.tuning_profile = v.get("tuning_profile").cloned();
                class.wire_width = v.get("wire_width").cloned();
                class.bus_width = v.get("bus_width").cloned();
                class.line_style = v.get("line_style").cloned();
                class.schematic_color = v.get("schematic_color").cloned();
                if out.net_version.value < 3 {
                    if let Some(nets) = array(v.get("nets"), &format!("{path}>nets"), &mut out.report) {
                        for value in nets {
                            if let Some(mut name) = string(Some(value), &format!("{path}>nets"), &mut out.report) {
                                if out.net_version.value < 1 { name.value = overbar(&name.value); }
                                class.nets.push(name);
                            }
                        }
                    }
                }
                for (key, value) in v.object().into_iter().flatten() {
                    if !CLASS_FIELDS.contains(&key.as_str()) && !matches!(key.as_str(), "name" | "priority" | "pcb_color" | "tuning_profile" | "wire_width" | "bus_width" | "line_style" | "schematic_color" | "nets") {
                        out.report.unknown(format!("{path}>{key}"), value.at);
                    }
                }
                out.report.count_read("project.classes");
                if let Some(&i) = class_indices.get(&class.name.value) { out.classes[i] = class; }
                else { class_indices.insert(class.name.value.clone(), out.classes.len()); out.classes.push(class); }
            }
        }
    }
    if out.net_version.value < 4 {
        for (i, class) in out.classes.iter_mut().enumerate() {
            class.priority = Some(Located { at: class.name.at,
                value: if class.name.value == "Default" { f64::INFINITY } else { i as f64 } });
        }
    }
    if let Some(entries) = object(ns.and_then(|v| v.get("netclass_assignments")), "kicad_pro>net_settings>netclass_assignments", &mut out.report).and_then(Value::object) {
        for (name, value) in entries {
            let path = format!("kicad_pro>net_settings>netclass_assignments>{name}");
            let mut classes = Vec::new();
            if let Some(s) = value.string() {
                if !s.is_empty() { classes.push(Located { at: value.at, value: s.into() }); }
            } else if let Some(values) = array(Some(value), &path, &mut out.report) {
                for v in values { if let Some(s) = string(Some(v), &path, &mut out.report) { classes.push(s); } }
            } else { continue; }
            out.assignments.push(Assignment { name: Located { at: value.at, value: if out.net_version.value < 1 { overbar(name) } else { name.clone() } }, classes });
            out.report.count_read("project.assignments");
        }
    }
    let mut seen = HashSet::new();
    for class in &out.classes {
        for name in &class.nets {
            if seen.insert((name.value.clone(), class.name.value.clone())) {
                out.patterns.push(Pattern { pattern: name.clone(), class: class.name.clone() });
            }
        }
    }
    if let Some(values) = array(ns.and_then(|v| v.get("netclass_patterns")), "kicad_pro>net_settings>netclass_patterns", &mut out.report) {
        for v in values {
            let path = "kicad_pro>net_settings>netclass_patterns";
            let Some(v) = object(Some(v), path, &mut out.report) else { continue };
            let pattern = string(v.get("pattern"), &format!("{path}>pattern"), &mut out.report);
            let class = string(v.get("netclass"), &format!("{path}>netclass"), &mut out.report);
            if v.get("pattern").is_none() || v.get("netclass").is_none() { bad(&mut out.report, v, path); }
            if let (Some(pattern), Some(class)) = (pattern, class) {
                if seen.insert((pattern.value.clone(), class.value.clone())) { out.patterns.push(Pattern { pattern, class }); }
            }
        }
    }
    out.report.read.insert("project.patterns".into(), out.patterns.len() as u64);
    out.text_variables = count_object(doc.and_then(|v| v.get("text_variables")), "kicad_pro>text_variables", &mut out.report);
    for key in ["net_chain_classes", "net_chain_netclasses"] {
        if let Some(v) = count_object(ns.and_then(|v| v.get(key)), &format!("{ns_path}>{key}"), &mut out.report) { out.net_chains.push(v); }
    }
    out.net_colors = count_object(ns.and_then(|v| v.get("net_colors")), "kicad_pro>net_settings>net_colors", &mut out.report);
    // The document defines no internal component-class schema. Count assignment containers,
    // not metadata, using an explicit walk so unfamiliar nested settings remain harmless.
    if let Some(v) = object(doc.and_then(|v| v.get("component_class_settings")), "kicad_pro>component_class_settings", &mut out.report) {
        let mut pending = vec![v]; let mut count = 0usize;
        while let Some(v) = pending.pop() {
            if let Some(entries) = v.object() {
                for (key, child) in entries {
                    if key.contains("assignment") {
                        count = count.saturating_add(child.array().map_or_else(|| child.object().map_or(usize::from(!child.is_empty()), |o| o.len()), |a| a.len()));
                    } else { pending.push(child); }
                }
            } else if let Some(a) = v.array() { pending.extend(a); }
        }
        out.component_classes = Some(Located { at: v.at, value: count });
    }
    Ok(out)
}

/// Old overbars toggle on each tilde; a doubled tilde is a literal one. A name with a tilde right
/// before an opening brace is already in the new notation (or ambiguous) and stays as it is.
pub fn overbar(name: &str) -> String {
    if name == "~" || name.contains("~{") { return name.into(); }
    let mut chars = name.chars().peekable(); let mut out = String::new(); let mut open = false;
    while let Some(c) = chars.next() {
        if c == '~' {
            if chars.peek() == Some(&'~') { chars.next(); out.push('~'); }
            else if open { out.push('}'); open = false; }
            else { out.push_str("~{"); open = true; }
        } else {
            if open && matches!(c, ' ' | '}' | ')') { out.push('}'); open = false; }
            out.push(c);
        }
    }
    if open { out.push('}'); }
    out
}
