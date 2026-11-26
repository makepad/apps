//! Importing printer, filament and process profiles the user exported or
//! saved: single preset files (`.json`), preset bundles (zip archives of
//! preset files), project files (`.3mf`: the project's settings and the
//! presets embedded in it) and G-code (its configuration block).
//!
//! An import is copied into Fab's own profile folder (one preset file per
//! preset, in the same JSON form), so the catalogue offers it from then on
//! and resolves it like any installed preset. Every key is accounted for in
//! the import's report: used, unknown to Fab, or withheld (printer access
//! credentials are never imported).

use crate::config::{Config, Value};
use crate::options::{options, Axis, PresetKind};
use crate::profiles::{merge_child, parse_preset, ProfileStore, RawPreset};
use makepad_strict_json as json;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Keys that hold printer access credentials or host addresses: never imported.
pub const CREDENTIAL_KEYS: &[&str] = &["print_host", "print_host_webui", "printhost_apikey", "printhost_cafile", "printhost_password", "printhost_port", "printhost_user", "printhost_authorization_type", "printhost_ssl_ignore_revoke", "host_type"];

/// Keys that say what a preset is (identity, compatibility, bookkeeping)
/// rather than how it prints: neither used nor unknown.
const IDENTITY_KEYS: &[&str] = &[
    "inherits", "compatible_printers", "compatible_printers_condition", "compatible_prints", "compatible_prints_condition", "print_settings_id", "printer_settings_id", "filament_settings_id", "different_settings_to_system", "inherits_group", "compatible_machine_expression_group", "compatible_process_expression_group", "print_compatible_printers", "filament_ids", "filament_self_index", "version", "is_custom_defined",
];

/// Keys of a whole project's configuration that describe its preset slots,
/// not one preset.
const LAYOUT_KEYS: &[&str] = &["printer_extruder_id", "printer_extruder_variant", "print_extruder_id", "print_extruder_variant", "filament_extruder_variant"];

const PROJECT_KEYS: &[&str] = &["inherits_group", "different_settings_to_system", "compatible_machine_expression_group", "compatible_process_expression_group", "print_compatible_printers", "filament_self_index", "filament_ids"];

/// What happened to one imported preset's keys.
#[derive(Clone, Debug, Default)]
pub struct KeyReport {
    /// Keys Fab's settings know (the slicer reads them or carries them).
    pub known: usize,
    /// Keys Fab does not know: kept in the preset file, not used.
    pub unknown: Vec<String>,
    /// Credential keys left out.
    pub withheld: Vec<String>,
    /// Keys from the parent preset kept although the file sets them (a
    /// project's settings outside its list of changed keys).
    pub from_parent: usize,
}

#[derive(Clone, Debug)]
pub struct ImportedPreset {
    pub preset: RawPreset,
    pub report: KeyReport,
    /// Where it came from: file, and entry inside an archive or project.
    pub origin: String,
    /// Notes the user should read (missing parent, renamed on a clash).
    pub notes: Vec<String>,
    /// The same as an installed preset: nothing to copy.
    pub skip: bool,
    /// The name it came with, when it went in under a new one (a preset of that name exists).
    pub renamed: Option<String>,
    /// Undecoded, privacy-filtered input retained until import inheritance is settled.
    pub(crate) ini_values: Option<Config>,
}

#[derive(Clone, Debug, Default)]
pub struct ImportBatch {
    pub presets: Vec<ImportedPreset>,
    /// Entries that could not be read, with why.
    pub errors: Vec<String>,
    /// Format-level omissions, reported without withheld values.
    pub notes: Vec<String>,
    /// The slicer data folder of the slicer that made the file, when the file names it
    /// ([`crate::trees::of_generator`]); "" when it does not.
    pub tree: String,
}

impl ImportBatch {
    /// One line of totals and renames: what the panel shows after an import.
    pub fn summary(&self) -> String {
        let mut s = String::new();
        let (mut known, mut unknown, mut withheld) = (0, 0, 0);
        for p in &self.presets {
            known += p.report.known;
            unknown += p.report.unknown.len();
            withheld += p.report.withheld.len();
        }
        s.push_str(&format!("{} preset{} imported: {known} settings used", self.presets.len(), if self.presets.len() == 1 { "" } else { "s" }));
        if unknown > 0 {
            s.push_str(&format!(", {unknown} not used by Fab"));
        }
        if withheld > 0 {
            s.push_str(&format!(", {withheld} printer access settings left out"));
        }
        if !self.errors.is_empty() {
            s.push_str(&format!(", {} unreadable", self.errors.len()));
        }
        for p in &self.presets {
            if let Some(was) = &p.renamed {
                s.push_str(&format!("; {} \"{was}\" imported as \"{}\" (one of that name exists)", p.preset.kind, p.preset.name));
            }
        }
        s
    }

    /// The full per-preset, per-key report (a text file next to the imports).
    pub fn report(&self) -> String {
        let mut s = self.summary();
        s.push('\n');
        for p in &self.presets {
            s.push_str(&format!("\n{} \"{}\" from {}\n", p.preset.kind, p.preset.name, p.origin));
            if !p.preset.inherits.is_empty() {
                s.push_str(&format!("  parent: {}\n", p.preset.inherits));
            }
            s.push_str(&format!("  used: {}\n", p.report.known));
            if p.report.from_parent > 0 {
                s.push_str(&format!("  taken from the parent (not marked as changed): {}\n", p.report.from_parent));
            }
            if !p.report.unknown.is_empty() {
                s.push_str(&format!("  not used by Fab: {}\n", p.report.unknown.join(", ")));
            }
            if !p.report.withheld.is_empty() {
                s.push_str(&format!("  left out (printer access): {}\n", p.report.withheld.join(", ")));
            }
            for n in &p.notes {
                s.push_str(&format!("  note: {n}\n"));
            }
        }
        for note in &self.notes {
            s.push_str(&format!("\nnote: {note}\n"));
        }
        for e in &self.errors {
            s.push_str(&format!("\nunreadable: {e}\n"));
        }
        s
    }
}

/// Fab's own profile folder: imported presets, one JSON file each under
/// `machine/`, `process/` and `filament/`.
pub fn fab_profile_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("FAB_PROFILES_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    crate::home::user_home().join("Documents").join("Fab").join("profiles")
}

fn report_for(values: &mut Config) -> KeyReport {
    let opts = options();
    let mut r = KeyReport::default();
    for k in CREDENTIAL_KEYS {
        if values.remove(k).is_some() {
            r.withheld.push(k.to_string());
        }
    }
    for k in values.map.keys() {
        if IDENTITY_KEYS.contains(&k.as_str()) {
            continue;
        }
        if opts.get(k).is_some() {
            r.known += 1;
        } else {
            r.unknown.push(k.clone());
        }
    }
    r
}

fn kind_of(p: &RawPreset) -> Option<&'static str> {
    if !p.kind.is_empty() {
        return match p.kind.as_str() {
            "machine" | "printer" => Some("machine"),
            "process" | "print" => Some("process"),
            "filament" => Some("filament"),
            _ => None,
        };
    }
    // Exported user presets name their kind by their id key.
    if p.values.has("printer_settings_id") {
        Some("machine")
    } else if p.values.has("print_settings_id") {
        Some("process")
    } else if p.values.has("filament_settings_id") {
        Some("filament")
    } else {
        None
    }
}

fn one_preset(bytes: &[u8], origin: &str, batch: &mut ImportBatch) {
    let path = PathBuf::from(origin);
    let mut raw = match parse_preset(bytes, &path) {
        Ok(r) => r,
        Err(e) => {
            batch.errors.push(e);
            return;
        }
    };
    if raw.name == "project_settings" || (raw.values.has("print_settings_id") && raw.values.has("printer_settings_id") && raw.values.has("filament_settings_id")) {
        split_project(&raw.values, origin, batch);
        return;
    }
    let Some(kind) = kind_of(&raw) else {
        batch.errors.push(format!("{origin}: not a printer, filament or process preset"));
        return;
    };
    raw.kind = kind.to_string();
    if raw.name.is_empty() {
        let id = match kind {
            "machine" => "printer_settings_id",
            "process" => "print_settings_id",
            _ => "filament_settings_id",
        };
        raw.name = raw.values.raw(id, 0).to_string();
    }
    if raw.name.is_empty() {
        raw.name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    }
    let report = report_for(&mut raw.values);
    batch.presets.push(ImportedPreset { preset: raw, report, origin: origin.to_string(), notes: Vec::new(), skip: false, renamed: None, ini_values: None });
}

/// A whole job configuration (a project's settings, a G-code configuration
/// block) as its printer, process and per-slot filament presets. Keys are
/// sorted by the preset that holds them; per-filament vectors give one value
/// per slot (nozzle-variant rows by `filament_self_index`).
pub fn split_project(cfg: &Config, origin: &str, batch: &mut ImportBatch) {
    // Under Fab's names first, so each key goes to the preset that holds it.
    let mut cfg = cfg.clone();
    let renames = crate::trees::read_names(&batch.tree, &mut cfg);
    let cfg = &cfg;
    let opts = options();
    let n = cfg.vec_len("filament_settings_id").max(cfg.vec_len("filament_colour")).max(1);
    let self_index: Vec<usize> = cfg.get("filament_self_index").map(|v| v.as_vec().iter().filter_map(|s| s.trim().parse().ok()).collect()).unwrap_or_default();
    let mut machine = Config::new();
    let mut process = Config::new();
    let mut fils = vec![Config::new(); n];
    for (k, v) in &cfg.map {
        let Some(def) = opts.get(k) else {
            // Unknown keys go with the process (reported as not used).
            if !IDENTITY_KEYS.contains(&k.as_str()) {
                process.map.insert(k.clone(), v.clone());
            }
            continue;
        };
        match def.preset {
            PresetKind::Machine => {
                machine.map.insert(k.clone(), v.clone());
            }
            PresetKind::Filament => {
                let items = v.as_vec();
                for (slot, f) in fils.iter_mut().enumerate() {
                    let val = if def.axis == Axis::FilamentVariant && !self_index.is_empty() && items.len() == self_index.len() {
                        Value::Vector(items.iter().zip(&self_index).filter(|(_, &si)| si == slot + 1).map(|(x, _)| x.clone()).collect())
                    } else if def.kind.is_vector() {
                        Value::Vector(vec![v.at(slot).to_string()])
                    } else {
                        v.clone()
                    };
                    f.map.insert(k.clone(), val);
                }
            }
            _ => {
                process.map.insert(k.clone(), v.clone());
            }
        }
    }
    // The variant rows of each filament.
    if let Some(Value::Vector(vars)) = cfg.get("filament_extruder_variant") {
        if vars.len() == self_index.len() {
            for (slot, f) in fils.iter_mut().enumerate() {
                f.set_vec("filament_extruder_variant", vars.iter().zip(&self_index).filter(|(_, &si)| si == slot + 1).map(|(x, _)| x.clone()).collect());
            }
        }
    }
    let groups = cfg.get("inherits_group").map(|v| v.as_vec()).unwrap_or_default();
    let diffs = cfg.get("different_settings_to_system").map(|v| v.as_vec()).unwrap_or_default();
    let mut push = |kind: &str, name: String, values: Config, group: usize| {
        let mut raw = RawPreset { kind: kind.to_string(), name, values, instantiation: true, from: "project".into(), ..Default::default() };
        raw.inherits = groups.get(group).cloned().unwrap_or_default();
        // A preset taken as installed has no parent here: it is its own base,
        // and its changed keys (if any) are listed all the same.
        raw.changed = diffs.get(group).map(|d| crate::config::parse_strings(d).into_iter().map(|k| renames.iter().find(|(stored, _)| *stored == k).map_or(k, |(_, fab)| fab.to_string())).collect());
        for k in PROJECT_KEYS {
            raw.values.remove(k);
        }
        let report = report_for(&mut raw.values);
        batch.presets.push(ImportedPreset { preset: raw, report, origin: origin.to_string(), notes: Vec::new(), skip: false, renamed: None, ini_values: None });
    };
    push("machine", cfg.str("printer_settings_id").to_string(), machine, n + 1);
    push("process", cfg.str("print_settings_id").to_string(), process, 0);
    for (i, f) in fils.into_iter().enumerate() {
        push("filament", cfg.raw("filament_settings_id", i).to_string(), f, i + 1);
    }
}

fn zip_entries(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut cur = std::io::Cursor::new(bytes);
    let dir = makepad_zip_file::zip_read_central_directory(&mut cur).map_err(|e| format!("not a readable archive ({e:?})"))?;
    let mut out = Vec::new();
    for h in &dir.file_headers {
        if h.file_name.ends_with('/') {
            continue;
        }
        let data = h.extract(&mut cur).map_err(|e| format!("{}: {e:?}", h.file_name))?;
        out.push((h.file_name.clone(), data));
    }
    Ok(out)
}

/// The `; key = value` configuration block of a G-code file.
pub fn gcode_config(text: &str) -> Option<Config> {
    let start = text.find("; CONFIG_BLOCK_START")?;
    let end = text[start..].find("; CONFIG_BLOCK_END")? + start;
    let mut cfg = Config::new();
    for l in text[start..end].lines().skip(1) {
        let Some(r) = l.strip_prefix("; ") else { continue };
        let (k, v) = match r.split_once(" = ") {
            Some((k, v)) => (k, v),
            None => match r.strip_suffix(" =") {
                Some(k) => (k, ""),
                None => continue,
            },
        };
        cfg.map.insert(k.to_string(), crate::config::parse_text(k, v));
    }
    Some(cfg)
}

/// Read the presets a file holds (nothing is written).
pub fn read_file(path: &Path) -> ImportBatch {
    let mut batch = ImportBatch::default();
    let shown = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.display().to_string());
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if ext == "ini" || ext == "gcode" {
        match std::fs::File::open(path) {
            Ok(file) => {
                let mut reader = std::io::BufReader::new(file);
                if crate::import_ini::read(&mut reader, path, &shown, ext == "gcode", ext == "gcode", &mut batch) {
                    return batch;
                }
            }
            Err(e) => { batch.errors.push(format!("{shown}: {e}")); return batch; }
        }
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            batch.errors.push(format!("{shown}: {e}"));
            return batch;
        }
    };
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "json" => one_preset(&bytes, &shown, &mut batch),
        "gcode" => {
            let text = std::str::from_utf8(&bytes).ok();
            batch.tree = text.and_then(crate::trees::of_generator).unwrap_or_default().to_string();
            match text.and_then(gcode_config) {
                Some(cfg) => split_project(&cfg, &shown, &mut batch),
                None => batch.errors.push(format!("{shown}: no configuration block")),
            }
        }
        "3mf" => match zip_entries(&bytes) {
            Ok(entries) => {
                let has_project = entries.iter().any(|(n, _)| n == "Metadata/project_settings.config");
                if !has_project {
                    if let Some((_, data)) = entries.iter().find(|(name, _)| name == crate::import_ini::CONFIG_PATH) {
                        let origin = format!("{shown} / {}", crate::import_ini::CONFIG_PATH);
                        crate::import_ini::read(&mut data.as_slice(), path, &origin, true, false, &mut batch);
                        return batch;
                    }
                }
                batch.tree = crate::trees::of_project(&entries).unwrap_or_default().to_string();
                for (name, data) in &entries {
                    let origin = format!("{shown} / {name}");
                    if name == "Metadata/project_settings.config" {
                        one_preset(data, &origin, &mut batch);
                    } else if !has_project && name.starts_with("Metadata/plate_") && name.ends_with(".gcode") {
                        if let Some(cfg) = std::str::from_utf8(data).ok().and_then(gcode_config) {
                            split_project(&cfg, &origin, &mut batch);
                        }
                    }
                }
                // The project's own custom presets replace its settings' copy of them.
                for (name, data) in &entries {
                    let embedded = name.starts_with("Metadata/") && name.ends_with(".config") && ["process_settings_", "filament_settings_", "machine_settings_"].iter().any(|t| name.contains(t));
                    if !embedded {
                        continue;
                    }
                    let mut one = ImportBatch::default();
                    one_preset(data, &format!("{shown} / {name}"), &mut one);
                    for ip in one.presets {
                        batch.presets.retain(|q| !(q.preset.kind == ip.preset.kind && q.preset.name == ip.preset.name));
                        batch.presets.push(ip);
                    }
                    batch.errors.extend(one.errors);
                }
                if batch.presets.is_empty() && batch.errors.is_empty() {
                    batch.errors.push(format!("{shown}: the project carries no settings"));
                }
            }
            Err(e) => batch.errors.push(format!("{shown}: {e}")),
        },
        // Preset bundles: zip archives of preset files.
        _ => match zip_entries(&bytes) {
            Ok(entries) => {
                for (name, data) in entries {
                    if name.ends_with(".json") && !name.ends_with("bundle_structure.json") {
                        one_preset(&data, &format!("{shown} / {name}"), &mut batch);
                    }
                }
                if batch.presets.is_empty() && batch.errors.is_empty() {
                    batch.errors.push(format!("{shown}: no presets inside"));
                }
            }
            Err(e) => batch.errors.push(format!("{shown}: {e}")),
        },
    }
    batch
}

/// A preset as a preset file (the vendor JSON form: strings and arrays of
/// strings), with its parent and, for a project's preset, its changed keys.
pub fn preset_json(p: &RawPreset) -> String {
    let mut fields: Vec<(String, json::Value)> = vec![
        ("type".into(), json::s(p.kind.clone())),
        ("name".into(), json::s(p.name.clone())),
        ("from".into(), json::s(if p.from.is_empty() || p.from == "project" { "User".to_string() } else { p.from.clone() })),
        ("instantiation".into(), json::s("true")),
    ];
    if !p.inherits.is_empty() {
        fields.push(("inherits".into(), json::s(p.inherits.clone())));
    }
    if !p.renamed_from.is_empty() {
        fields.push(("renamed_from".into(), json::Value::Arr(p.renamed_from.iter().cloned().map(json::s).collect())));
    }
    if !p.filament_id.is_empty() {
        fields.push(("filament_id".into(), json::s(p.filament_id.clone())));
    }
    if !p.setting_id.is_empty() {
        fields.push(("setting_id".into(), json::s(p.setting_id.clone())));
    }
    if !p.tree.is_empty() {
        fields.push(("fab_tree".into(), json::s(p.tree.clone())));
    }
    for (k, v) in &p.values.map {
        let jv = match v {
            Value::Scalar(s) => json::s(s.clone()),
            Value::Vector(items) => json::Value::Arr(items.iter().map(|e| json::s(e.clone())).collect()),
        };
        fields.push((k.clone(), jv));
    }
    json::Value::Obj(fields).to_json()
}

fn slug(s: &str) -> String {
    let t: String = s.trim().chars().map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ' ' | '@') { c } else { '-' }).collect();
    let t = t.trim_matches(|c| c == '-' || c == '.' || c == ' ').to_string();
    if t.is_empty() { "preset".into() } else { t }
}

fn import_sparse_pattern(values: &mut Config, notes: &mut Vec<String>) {
    const KEY: &str = "sparse_infill_pattern";
    let Some(stored) = values.map.get_mut(KEY) else { return };
    let patterns = match stored {
        Value::Scalar(s) => std::slice::from_mut(s),
        Value::Vector(v) => v.as_mut_slice(),
    };
    let printable = &options().get(KEY).expect("the sparse pattern option").enum_values;
    for pattern in patterns {
        *pattern = crate::spellings::read(KEY, std::mem::take(pattern));
        if printable.contains(pattern) { continue; }
        let substitute = match pattern.as_str() {
            "quartercubic" => "cubic",
            "lateral-honeycomb" => "honeycomb",
            "lateral-lattice" => "2dlattice",
            _ => {
                notes.push(format!("sparse infill pattern {pattern:?} is not one Fab prints: kept, pick another before slicing"));
                continue;
            }
        };
        notes.push(format!("sparse infill pattern {pattern:?} is not one Fab prints: imported as {substitute:?}"));
        *pattern = substitute.into();
    }
}

/// An adaptive infill mode of Orca's that Fab does not build yet is kept as written and named
/// in the import's notes: a TPMS pattern does not slice with it until it is changed.
fn import_adaptive_mode(values: &Config, notes: &mut Vec<String>) {
    let Some(stored) = values.map.get("adaptive_infill_mode") else { return };
    let modes = match stored {
        Value::Scalar(s) => std::slice::from_ref(s),
        Value::Vector(v) => v.as_slice(),
    };
    for mode in modes.iter().filter(|m| !matches!(m.as_str(), "disabled" | "distance_warp" | "lobes")) {
        notes.push(format!("adaptive infill mode {mode:?} is not built in Fab yet: kept, set it to disabled, distance_warp or lobes before slicing a TPMS pattern"));
    }
}

/// Settle each preset against the profiles the user has installed: a
/// project's preset becomes its parent plus the keys it marks as changed
/// (the rest follow the parent, as the project's own app reads it); a preset
/// the same as an installed one of its name is left out; a name that clashes
/// with a different installed preset gets the file's name added.
pub fn settle(batch: &mut ImportBatch, store: &ProfileStore, source: &str) {
    crate::import_ini::settle(batch, store);
    for ip in &mut batch.presets {
        let p = &mut ip.preset;
        if p.inherits.is_empty() && p.changed.is_some() {
            if store.path_of(&p.name).is_some() {
                p.inherits = p.name.clone();
            } else {
                p.changed = None;
            }
        }
        if !p.inherits.is_empty() && store.path_of(&p.inherits).is_none() {
            ip.notes.push(format!("its parent \"{}\" is not installed: imported with every value it carries", p.inherits));
            p.inherits.clear();
            p.changed = None;
        }
        // The slicer that made it: the file says, or the preset does (one Fab saved), or else
        // its installed parent's folder. Its keys go in under Fab's names, and the report counts
        // them as used.
        if !batch.tree.is_empty() {
            p.tree = batch.tree.clone();
        } else if p.tree.is_empty() && !p.inherits.is_empty() {
            p.tree = store.resolve(&p.inherits).map(|b| b.tree).unwrap_or_default();
        }
        for (stored, fab) in crate::trees::read_names(&p.tree, &mut p.values) {
            if let Some(changed) = &mut p.changed {
                changed.iter_mut().filter(|c| *c == stored).for_each(|c| *c = fab.to_string());
            }
            if let Some(i) = ip.report.unknown.iter().position(|k| k == stored) {
                ip.report.unknown.remove(i);
                ip.report.known += 1;
            }
        }
        if let Some(changed) = p.changed.take() {
            let before = p.values.map.len();
            p.values.map.retain(|k, _| changed.iter().any(|c| c == k) || k.ends_with("_extruder_id") || k.ends_with("_extruder_variant"));
            ip.report.from_parent = before - p.values.map.len();
            ip.report.known = p.values.map.keys().filter(|k| options().get(k).is_some() && !IDENTITY_KEYS.contains(&k.as_str()) && !LAYOUT_KEYS.contains(&k.as_str())).count();
            ip.report.unknown.retain(|k| p.values.has(k));
        }
        import_sparse_pattern(&mut p.values, &mut ip.notes);
        import_adaptive_mode(&p.values, &mut ip.notes);
        if store.path_of(&p.name).is_none() {
            continue;
        }
        let Ok(installed) = store.resolve(&p.name) else { continue };
        let mut mine = match p.inherits.is_empty() {
            true => Config::new(),
            false => store.resolve(&p.inherits).map(|b| b.values).unwrap_or_default(),
        };
        merge_child(&mut mine, &p.values);
        let differs = mine.map.iter().any(|(k, v)| installed.values.get(k) != Some(v) && !IDENTITY_KEYS.contains(&k.as_str()));
        if !differs {
            ip.skip = true;
            ip.notes.push("the same as the installed preset of that name: not copied".into());
            continue;
        }
        ip.notes.push(format!("renamed from \"{}\": an installed preset has that name", p.name));
        ip.renamed = Some(p.name.clone());
        if p.inherits.is_empty() {
            p.inherits = p.name.clone();
        }
        p.name = format!("{} ({source})", p.name);
    }
}

/// Import a file into Fab's profile folder `dir`: read, settle against the
/// installed `store`, write one preset file each and the import's report.
pub fn import_into(path: &Path, store: &ProfileStore, dir: &Path) -> ImportBatch {
    let mut batch = read_file(path);
    let source = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    settle(&mut batch, store, &source);
    for ip in batch.presets.iter_mut().filter(|p| !p.skip) {
        let sub = dir.join(&ip.preset.kind);
        if let Err(e) = std::fs::create_dir_all(&sub) {
            batch.errors.push(format!("{}: {e}", sub.display()));
            continue;
        }
        let mut file = sub.join(format!("{}.json", slug(&ip.preset.name)));
        let mut contents = preset_json(&ip.preset);
        let collision = match std::fs::read(&file) {
            Ok(existing) if existing == contents.as_bytes() => {
                ip.skip = true;
                ip.notes.push("already imported: not copied".into());
                continue;
            }
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => store.path_of(&ip.preset.name).is_some(),
            Err(e) => {
                batch.errors.push(format!("{}: {e}", file.display()));
                continue;
            }
        };
        if collision {
            let original = ip.preset.name.clone();
            let mut suffix = 2;
            loop {
                let name = format!("{original} {suffix}");
                file = sub.join(format!("{}.json", slug(&name)));
                if store.path_of(&name).is_none() && !file.exists() {
                    ip.preset.name = name;
                    break;
                }
                suffix += 1;
            }
            ip.notes.push(format!("renamed from \"{original}\": a preset with that name or file already exists"));
            ip.renamed.get_or_insert(original);
            contents = preset_json(&ip.preset);
        }
        // A concurrent import must not replace a preset created after the check.
        if let Err(e) = std::fs::OpenOptions::new().write(true).create_new(true).open(&file).and_then(|mut f| f.write_all(contents.as_bytes())) {
            batch.errors.push(format!("{}: {e}", file.display()));
        }
    }
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(dir.join(format!("import-{}.txt", slug(&source))), batch.report());
    batch
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fab-import-test-{}-{name}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d
    }

    #[test]
    fn sparse_pattern_imports_explain_substitutions_and_keep_unknown_names() {
        let store = ProfileStore::open(&[], &[]);
        for (source, expected, note) in [
            ("quartercubic", "cubic", Some(r#"imported as "cubic""#)),
            ("lateral-honeycomb", "honeycomb", Some(r#"imported as "honeycomb""#)),
            ("lateral-lattice", "2dlattice", Some(r#"imported as "2dlattice""#)),
            ("gyroid", "gyroid", None),
            ("rectilinear", "zig-zag", None),
            ("future-pattern", "future-pattern", Some("kept, pick another before slicing")),
        ] {
            let mut batch = ImportBatch { tree: "OrcaSlicer".into(), ..ImportBatch::default() };
            let json = format!(r#"{{"type":"process","name":"Standalone","sparse_infill_pattern":"{source}"}}"#);
            one_preset(json.as_bytes(), "process.json", &mut batch);
            let mut cfg = Config::new();
            cfg.set_str("print_settings_id", "Project");
            cfg.set_str("sparse_infill_pattern", source);
            split_project(&cfg, "Metadata/project_settings.config", &mut batch);
            settle(&mut batch, &store, "test");
            let notes: Vec<_> = note.map(|n| format!("sparse infill pattern {source:?} is not one Fab prints: {n}")).into_iter().collect();
            for name in ["Standalone", "Project"] {
                let p = batch.presets.iter().find(|p| p.preset.name == name).unwrap();
                assert_eq!(p.preset.values.str("sparse_infill_pattern"), expected, "{name}: {source}");
                assert_eq!(p.notes, notes, "{name}: {source}");
            }
        }
    }

    #[test]
    fn unbuilt_adaptive_modes_are_kept_and_named() {
        let store = ProfileStore::open(&[], &[]);
        for (mode, noted) in [("smooth_blend", true), ("normal_z", true), ("lobes", false), ("distance_warp", false), ("disabled", false)] {
            let mut batch = ImportBatch { tree: "OrcaSlicer".into(), ..ImportBatch::default() };
            let json = format!(r#"{{"type":"process","name":"Graded","sparse_infill_pattern":"gyroid","adaptive_infill_mode":"{mode}"}}"#);
            one_preset(json.as_bytes(), "process.json", &mut batch);
            settle(&mut batch, &store, "test");
            let p = batch.presets.iter().find(|p| p.preset.name == "Graded").unwrap();
            assert_eq!(p.preset.values.str("adaptive_infill_mode"), mode, "{mode} is kept as written");
            let notes: Vec<_> = noted.then(|| format!("adaptive infill mode {mode:?} is not built in Fab yet: kept, set it to disabled, distance_warp or lobes before slicing a TPMS pattern")).into_iter().collect();
            assert_eq!(p.notes, notes, "the notes for {mode}");
        }
    }

    #[test]
    fn a_byte_order_mark_does_not_prevent_import() {
        let root = tmp("bom");
        let export = root.join("marked.json");
        std::fs::write(&export, b"\xef\xbb\xbf{\"type\":\"process\",\"name\":\"marked\",\"layer_height\":\"0.2\"}").unwrap();
        let imported = root.join("imported");
        let store = ProfileStore::open(&[], &[]);
        let batch = import_into(&export, &store, &imported);
        assert!(batch.errors.is_empty(), "{:?}", batch.errors);
        assert_eq!(batch.presets.len(), 1);
        let store = ProfileStore::open(&[], &[imported]);
        assert_eq!(store.resolve("marked").unwrap().values.str("layer_height"), "0.2");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A G-code file from another slicer: its keys go in under Fab's names, the report counts
    /// them as used, and the imported presets read with that slicer's defaults.
    #[test]
    fn another_slicers_gcode_imports_under_fabs_names_and_its_defaults() {
        let root = tmp("tree");
        let export = root.join("plate.gcode");
        let block = ["printer_settings_id = Test printer", "print_settings_id = Test process", "filament_settings_id = \"Fil A\"", "nozzle_diameter = 0.4", "only_one_wall_top = 1", "wipe_tower_extra_rib_length = 8", "layer_height = 0.2", "machine_tool_change_time = 5"];
        let text = format!("; HEADER_BLOCK_START\n; generated by Snapmaker Orca 2.4.1 on 2026-11-06 at 10:00:00\n; HEADER_BLOCK_END\nG28\n; CONFIG_BLOCK_START\n{}\n; CONFIG_BLOCK_END\n", block.iter().map(|l| format!("; {l}")).collect::<Vec<_>>().join("\n"));
        std::fs::write(&export, text).unwrap();
        let imported = root.join("imported");
        let b = import_into(&export, &ProfileStore::open(&[], &[]), &imported);
        assert!(b.errors.is_empty(), "{:?}", b.errors);
        assert_eq!(b.tree, "Snapmaker_Orca");
        let process = b.presets.iter().find(|p| p.preset.kind == "process").unwrap();
        assert!(process.report.unknown.is_empty(), "renamed keys are used: {:?}", process.report.unknown);
        assert_eq!(process.preset.values.str("top_one_wall_type"), "all top");
        let machine = b.presets.iter().find(|p| p.preset.kind == "machine").unwrap();
        assert_eq!(machine.preset.values.str("machine_switch_extruder_time"), "5", "a renamed key goes to the preset that holds it");
        let store = ProfileStore::open(&[], &[imported]);
        let p = store.resolve("Test process").unwrap();
        assert_eq!((p.tree.as_str(), p.values.str("prime_tower_extra_rib_length")), ("Snapmaker_Orca", "8"));
        assert!(!p.values.has("only_one_wall_top"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A project's list of changed keys follows the renames, and a preset Fab saved keeps the
    /// slicer it records when it is imported again.
    #[test]
    fn renamed_keys_stay_changed_and_a_saved_preset_keeps_its_slicer() {
        let mut cfg = Config::new();
        cfg.set_str("printer_settings_id", "Test printer");
        cfg.set_str("print_settings_id", "Test process");
        cfg.set_vec("filament_settings_id", vec!["Fil A".into()]);
        cfg.set_str("only_one_wall_top", "1");
        cfg.set_vec("different_settings_to_system", vec!["only_one_wall_top;layer_height".into(), "".into(), "".into()]);
        let mut b = ImportBatch { tree: "Snapmaker_Orca".into(), ..ImportBatch::default() };
        split_project(&cfg, "test", &mut b);
        let process = b.presets.iter().find(|p| p.preset.kind == "process").unwrap();
        assert_eq!(process.preset.changed, Some(vec!["top_one_wall_type".to_string(), "layer_height".to_string()]));
        let root = tmp("saved");
        let saved = root.join("saved.json");
        std::fs::write(&saved, r#"{"type":"process","name":"Saved","fab_tree":"Snapmaker_Orca","only_one_wall_top":"1"}"#).unwrap();
        let b = import_into(&saved, &ProfileStore::open(&[], &[]), &root.join("imported"));
        assert_eq!(b.presets[0].preset.tree, "Snapmaker_Orca");
        assert_eq!(b.presets[0].preset.values.str("top_one_wall_type"), "all top");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_job_configuration_splits_into_its_presets() {
        let mut cfg = Config::new();
        cfg.set_str("printer_settings_id", "Test printer");
        cfg.set_str("print_settings_id", "Test process");
        cfg.set_vec("filament_settings_id", vec!["Fil A".into(), "Fil B".into()]);
        cfg.set_vec("nozzle_diameter", vec!["0.4".into(), "0.4".into()]);
        cfg.set_str("layer_height", "0.12");
        cfg.set_vec("nozzle_temperature", vec!["220".into(), "250".into()]);
        cfg.set_vec("filament_self_index", vec!["1".into(), "2".into()]);
        cfg.set_vec("filament_extruder_variant", vec!["Direct Drive Standard".into(), "Direct Drive Standard".into()]);
        cfg.set_str("printhost_apikey", "secret");
        cfg.set_str("some_future_key", "1");
        let mut b = ImportBatch::default();
        split_project(&cfg, "test", &mut b);
        assert_eq!(b.presets.len(), 4);
        let m = &b.presets[0].preset;
        assert_eq!((m.kind.as_str(), m.name.as_str()), ("machine", "Test printer"));
        assert!(m.values.has("nozzle_diameter"));
        let p = &b.presets[1];
        assert_eq!(p.preset.values.str("layer_height"), "0.12");
        assert!(p.report.unknown.contains(&"some_future_key".to_string()));
        let fb = &b.presets[3].preset;
        assert_eq!(fb.name, "Fil B");
        assert_eq!(fb.values.raw("nozzle_temperature", 0), "250");
        assert!(b.presets.iter().all(|p| !p.preset.values.has("printhost_apikey")));
        assert!(b.presets.iter().any(|p| p.report.withheld.contains(&"printhost_apikey".to_string())));
    }

    #[test]
    fn exported_presets_import_and_resolve_against_their_parent() {
        let root = tmp("store");
        let sys = root.join("system/V/process");
        std::fs::create_dir_all(&sys).unwrap();
        std::fs::write(sys.join("base.json"), r#"{"type":"process","name":"Base 0.20","from":"system","instantiation":"true","layer_height":"0.2","wall_loops":"2","print_extruder_id":["1","1"],"print_extruder_variant":["Direct Drive Standard","Direct Drive High Flow"],"outer_wall_speed":["200","300"]}"#).unwrap();
        let export = root.join("Mine.json");
        std::fs::write(&export, r#"{"name":"Mine","inherits":"Base 0.20","from":"User","print_settings_id":"Mine","wall_loops":"3","print_extruder_id":["1"],"print_extruder_variant":["Direct Drive High Flow"],"outer_wall_speed":["150"],"printhost_password":"x"}"#).unwrap();
        let store = ProfileStore::open(std::slice::from_ref(&root), &[]);
        let fab = root.join("fab");
        let b = import_into(&export, &store, &fab);
        assert!(b.errors.is_empty(), "{:?}", b.errors);
        assert_eq!(b.presets.len(), 1);
        assert_eq!(b.presets[0].report.withheld, vec!["printhost_password".to_string()]);
        let store = ProfileStore::open(std::slice::from_ref(&root), &[fab.clone()]);
        let p = store.resolve("Mine").unwrap();
        assert_eq!(p.values.str("wall_loops"), "3");
        assert_eq!(p.values.str("layer_height"), "0.2");
        // The child's one row lands on the parent's High Flow row.
        assert_eq!(p.values.get("outer_wall_speed").unwrap().as_vec(), vec!["200".to_string(), "150".to_string()]);
        assert!(!p.values.has("printhost_password"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn changed_imports_keep_both_presets_and_identical_imports_are_skipped() {
        let root = tmp("collisions");
        let fab = root.join("fab");
        let export = root.join("Mine.json");
        let original = r#"{"type":"process","name":"Mine","wall_loops":"2"}"#;
        let store = ProfileStore::open(std::slice::from_ref(&root), &[]);
        std::fs::write(&export, original).unwrap();
        let first = import_into(&export, &store, &fab);
        assert!(first.errors.is_empty(), "{:?}", first.errors);
        let first_file = fab.join("process/Mine.json");
        let first_contents = std::fs::read(&first_file).unwrap();

        std::fs::write(&export, r#"{"type":"process","name":"Mine","wall_loops":"3"}"#).unwrap();
        let second = import_into(&export, &store, &fab);
        assert!(second.errors.is_empty(), "{:?}", second.errors);
        assert_eq!(second.presets[0].preset.name, "Mine 2");
        assert!(second.summary().contains("\"Mine\" imported as \"Mine 2\""));
        assert!(second.report().contains("renamed from \"Mine\""));
        assert_eq!(std::fs::read(&first_file).unwrap(), first_contents);
        let imported = ProfileStore::open(std::slice::from_ref(&root), std::slice::from_ref(&fab));
        assert_eq!(imported.resolve("Mine").unwrap().values.str("wall_loops"), "2");
        assert_eq!(imported.resolve("Mine 2").unwrap().values.str("wall_loops"), "3");

        std::fs::write(&export, original).unwrap();
        let third = import_into(&export, &store, &fab);
        assert!(third.errors.is_empty(), "{:?}", third.errors);
        assert!(third.presets[0].skip);
        assert!(third.report().contains("already imported"));
        assert_eq!(std::fs::read(&first_file).unwrap(), first_contents);
        assert_eq!(std::fs::read_dir(fab.join("process")).unwrap().count(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_project_keeps_only_what_it_changed_from_the_installed_presets() {
        let root = tmp("project");
        let sys = root.join("system/V/process");
        std::fs::create_dir_all(&sys).unwrap();
        std::fs::write(sys.join("std.json"), r#"{"type":"process","name":"Std","from":"system","instantiation":"true","layer_height":"0.2","wall_loops":"2","enable_support":"0"}"#).unwrap();
        let store = ProfileStore::open(std::slice::from_ref(&root), &[]);
        let mut cfg = Config::new();
        cfg.set_str("printer_settings_id", "Not installed printer");
        cfg.set_str("print_settings_id", "Std");
        cfg.set_vec("filament_settings_id", vec!["F".into()]);
        cfg.set_str("layer_height", "0.2");
        cfg.set_str("wall_loops", "5");
        cfg.set_str("enable_support", "1");
        cfg.set_vec("different_settings_to_system", vec!["enable_support".into(), "".into(), "".into()]);
        let mut b = ImportBatch::default();
        split_project(&cfg, "p.3mf", &mut b);
        settle(&mut b, &store, "p");
        let pr = b.presets.iter().find(|p| p.preset.kind == "process").unwrap();
        assert_eq!(pr.preset.name, "Std (p)");
        assert_eq!(pr.preset.inherits, "Std");
        // wall_loops is not marked as changed: it follows the installed preset.
        assert!(!pr.preset.values.has("wall_loops"));
        assert_eq!(pr.preset.values.str("enable_support"), "1");
        let m = b.presets.iter().find(|p| p.preset.kind == "machine").unwrap();
        assert!(m.preset.inherits.is_empty() && !m.skip);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_vendor_leaf_restates_its_slot_layout() {
        let root = tmp("layout");
        let sys = root.join("system/V/process");
        std::fs::create_dir_all(&sys).unwrap();
        std::fs::write(sys.join("common.json"), r#"{"type":"process","name":"common","from":"system","instantiation":"false","print_extruder_id":["1","1","2","2"],"print_extruder_variant":["Direct Drive Standard","Direct Drive High Flow","Direct Drive Standard","Direct Drive High Flow"],"outer_wall_speed":["1","2","3","4"],"inner_wall_speed":["9"]}"#).unwrap();
        std::fs::write(sys.join("leaf.json"), r#"{"type":"process","name":"leaf","inherits":"common","from":"system","instantiation":"true","print_extruder_id":["1","1","1","2","2","2"],"print_extruder_variant":["Direct Drive Standard","Direct Drive High Flow","Direct Drive E3D High Flow","Direct Drive Standard","Direct Drive High Flow","Direct Drive E3D High Flow"],"outer_wall_speed":["200","500","200","200","500","200"]}"#).unwrap();
        let store = ProfileStore::open(std::slice::from_ref(&root), &[]);
        let p = store.resolve("leaf").unwrap();
        assert_eq!(p.values.vec_len("print_extruder_id"), 6);
        assert_eq!(p.values.raw("outer_wall_speed", 4), "500");
        // A vector the leaf does not restate is padded to its layout with its first entry.
        assert_eq!(p.values.get("inner_wall_speed").unwrap().as_vec(), vec!["9".to_string(); 6]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
