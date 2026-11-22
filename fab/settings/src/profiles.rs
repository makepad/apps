//! Reading the user's own installed slicer profiles (vendor JSON presets),
//! resolving inheritance and include fragments, and collapsing the nozzle
//! variant vectors into one job configuration.
//!
//! Nothing here ships profile data: presets are read at runtime from the
//! user's install (or from files the user points at).

use crate::config::{parse_strings, Config, Value};
use crate::machine::{FIRST_TREE_PLATE_INDICES, FORK_PLATE_INDICES, MAINLINE_PLATE_INDICES, PLATES};
use crate::options::{options, Axis, Kind, PresetKind};
use makepad_strict_json as json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The slicer whose app settings give Fab's starting selection.
pub const VENDOR_APP_DIR: &str = "BambuStudio";

/// The data folders of the desktop slicers whose installed presets Fab reads, in lookup order.
pub const SLICER_APPS: &[&str] = &[VENDOR_APP_DIR, "Snapmaker_Orca", "OrcaSlicer"];

/// Header fields of a preset file (not options).
const HEADER_KEYS: &[&str] = &[
    "type", "name", "inherits", "include", "from", "instantiation", "setting_id", "filament_id", "base_id", "version", "description", "alias", "renamed_from", "updated_time", "user_id",
];

#[derive(Clone, Debug, Default)]
pub struct RawPreset {
    pub path: PathBuf,
    pub kind: String,
    pub name: String,
    pub inherits: String,
    /// Former names of this preset, never inherited by its children.
    pub renamed_from: Vec<String>,
    pub include: Vec<String>,
    pub from: String,
    pub instantiation: bool,
    pub setting_id: String,
    pub filament_id: String,
    pub values: Config,
    /// A project's preset: the keys it changed from its parent (the rest
    /// follow the parent). `None`: every key it carries counts.
    pub changed: Option<Vec<String>>,
    /// An imported preset's slicer data folder (`fab_tree`, written by Fab's import): the
    /// slicer that made the file it came from, when the file said ([`crate::trees`]).
    pub tree: String,
}

fn json_scalar(v: &json::Value) -> String {
    match v {
        json::Value::Str(s) => s.clone(),
        json::Value::Int(i) => i.to_string(),
        json::Value::F64(f) => f.to_string(),
        json::Value::Bool(b) => if *b { "1".into() } else { "0".into() },
        json::Value::Null => String::new(),
        _ => String::new(),
    }
}

/// A template escaped once more (one line, its newlines and quotes as C escapes) C-unescaped
/// once; real multiline text is left alone.
fn unescape_template(s: String) -> String {
    if !s.contains('\n') && s.contains("\\n") {
        crate::config::unescape_cstyle(&s)
    } else {
        s
    }
}

pub fn parse_preset(bytes: &[u8], path: &Path) -> Result<RawPreset, String> {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let v = json::parse_depth_last_key_wins(bytes, 32).map_err(|e| format!("{}: {e}", path.display()))?;
    let json::Value::Obj(fields) = v else {
        return Err(format!("{}: not a JSON object", path.display()));
    };
    let mut p = RawPreset { path: path.to_path_buf(), instantiation: true, ..Default::default() };
    for (k, v) in fields {
        match k.as_str() {
            "type" => p.kind = json_scalar(&v),
            "name" => p.name = json_scalar(&v),
            "inherits" => p.inherits = json_scalar(&v),
            "renamed_from" => p.renamed_from = match &v {
                json::Value::Arr(a) => a.iter().map(json_scalar).collect(),
                _ => parse_strings(&json_scalar(&v)),
            },
            "fab_tree" => p.tree = json_scalar(&v),
            "from" => p.from = json_scalar(&v),
            "instantiation" => p.instantiation = json_scalar(&v) != "false",
            "setting_id" => p.setting_id = json_scalar(&v),
            "filament_id" => p.filament_id = json_scalar(&v),
            "include" => match &v {
                json::Value::Arr(a) => p.include = a.iter().map(json_scalar).collect(),
                _ => {
                    let s = json_scalar(&v);
                    if !s.is_empty() {
                        p.include = vec![s];
                    }
                }
            },
            _ if HEADER_KEYS.contains(&k.as_str()) => {}
            _ => {
                let template = |s: String| if k.ends_with("_gcode") { unescape_template(s) } else { s };
                let val = match &v {
                    json::Value::Arr(a) => Value::Vector(a.iter().map(|x| template(json_scalar(x))).collect()),
                    other => {
                        let s = json_scalar(other);
                        // A vector option written as a single string uses the
                        // profile text syntax, which decodes its own escapes.
                        match options().kind(&k) {
                            Some(Kind::Strs) => Value::Vector(parse_strings(&s)),
                            Some(kind) if kind.is_vector() => Value::Vector(if s.is_empty() { Vec::new() } else { s.split(',').map(|x| x.to_string()).collect() }),
                            _ => Value::Scalar(crate::spellings::read(&k, template(s))),
                        }
                    }
                };
                p.values.map.insert(k, val);
            }
        }
    }
    Ok(p)
}

pub fn load_preset_file(path: &Path) -> Result<RawPreset, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse_preset(&bytes, path)
}

/// A resolved preset: every key its chain sets, flattened.
#[derive(Clone, Debug, Default)]
pub struct Preset {
    pub name: String,
    pub kind: String,
    pub setting_id: String,
    pub filament_id: String,
    /// The nearest system ancestor's name ("" when this is a system preset).
    pub system_parent: String,
    pub values: Config,
    /// The slicer data folder the preset was read from, whose defaults fill what it leaves
    /// out ([`crate::trees`]); "" for Fab's own defaults.
    pub tree: String,
}

impl Preset {
    /// A preset from an already flattened file (no `inherits`), with Fab's own defaults.
    pub fn from_flat(raw: RawPreset) -> Self {
        Preset { name: raw.name, kind: raw.kind, setting_id: raw.setting_id, filament_id: raw.filament_id, system_parent: String::new(), values: raw.values, tree: String::new() }
    }

    /// The preset as read from the slicer data folder `folder`: that slicer's key names read as
    /// Fab's, and its defaults fill what the preset leaves out.
    pub fn read_from(mut self, folder: &str) -> Self {
        crate::trees::read_names(folder, &mut self.values);
        self.tree = folder.to_string();
        self
    }
}

/// The printer, plate, process and filaments selected in the installed app.
#[derive(Clone, Debug, Default)]
pub struct Selection {
    pub machine: String,
    pub plate: String,
    pub process: String,
    pub filaments: Vec<String>,
    /// `#RRGGBB` per filament, as the app shows them.
    pub colours: Vec<String>,
    /// Nozzle flow type per extruder ("Standard", "High Flow"), empty when
    /// the app does not say.
    pub nozzle_volume_type: Vec<String>,
}

// Selection fields in the settings format. The per-printer selection array
// uses an unnumbered first filament followed by two-digit slot suffixes.
const PRESETS: &str = "presets";
const APP: &str = "app";
const PER_MACHINE: &str = "orca_presets";
const MACHINE: &str = "machine";
const PROCESS: &str = "process";
const FILAMENTS: &str = "filaments";
const FILAMENT: &str = "filament";
const FILAMENT_PREFIX: &str = "filament_";
const COLOURS: &str = "filament_colors";
const BED_TYPE: &str = "curr_bed_type";
const NOZZLE_TYPES: &str = "nozzle_volume_types";

fn numbered_filament(key: &str) -> bool {
    key.strip_prefix(FILAMENT_PREFIX).is_some_and(|suffix| suffix.len() == 2 && suffix.bytes().all(|b| b.is_ascii_digit()))
}

fn selection_pick(path: &[json::Step<'_>]) -> json::Pick {
    use json::{Pick, Step::{Index, Key}};
    match path {
        [] | [Key(PRESETS | APP | PER_MACHINE | NOZZLE_TYPES)] => Pick::Open,
        [Key(PRESETS), Key(FILAMENTS)] | [Key(PER_MACHINE), Index(_)] => Pick::Open,
        [Key(PRESETS), Key(MACHINE | PROCESS | COLOURS)]
        | [Key(PRESETS), Key(FILAMENTS), Index(_)]
        | [Key(APP), Key(BED_TYPE)]
        | [Key(NOZZLE_TYPES), Key(_)] => Pick::KeepString,
        [Key(PER_MACHINE), Index(_), Key(key)]
            if [MACHINE, PROCESS, FILAMENT, COLOURS, BED_TYPE].contains(key) || numbered_filament(key) => Pick::KeepString,
        _ => Pick::Skip,
    }
}

fn selection_values(bytes: &[u8]) -> Option<json::Value> {
    // A checksum line may follow the closing object.
    let end = bytes.iter().rposition(|&b| b == b'}')? + 1;
    json::parse_picked(&bytes[..end], 32, true, &selection_pick).ok()
}

fn selection_string<'a>(value: &'a json::Value, key: &str) -> &'a str {
    value.get(key).and_then(json::Value::as_str).unwrap_or("")
}

/// The selection in `data_dir`'s app settings file, if there is one. The settings file is named
/// after its app (`<app>.conf`), and that name, not the folder's, says which tree's format it is:
/// a portable install or a folder given by hand has another name.
pub fn installed_selection(data_dir: &Path) -> Option<Selection> {
    let (tree, bytes) = SLICER_APPS.iter().enumerate().find_map(|(tree, app)| std::fs::read(data_dir.join(format!("{app}.conf"))).ok().map(|bytes| (tree, bytes)))?;
    let v = selection_values(&bytes)?;
    let presets = v.get(PRESETS)?;
    let machine = selection_string(presets, MACHINE).to_string();
    let (selected, filaments, plate_index, plate_indices) = if tree == 0 {
        let filaments = presets.get(FILAMENTS).and_then(json::Value::as_arr)
            .map(|items| items.iter().filter_map(json::Value::as_str).map(str::to_string).collect()).unwrap_or_default();
        let plate = v.get(APP).map(|app| selection_string(app, BED_TYPE)).unwrap_or("");
        (presets, filaments, plate, FIRST_TREE_PLATE_INDICES)
    } else {
        let selected = v.get(PER_MACHINE)?.as_arr()?.iter().find(|p| selection_string(p, MACHINE) == machine)?;
        let mut filaments = Vec::new();
        if let Some(first) = selected.get(FILAMENT).and_then(json::Value::as_str) {
            filaments.push(first.to_string());
            // The format has exactly two digits, so 99 is its last slot key.
            for slot in 1..=99 {
                let key = format!("{FILAMENT_PREFIX}{slot:02}");
                let Some(name) = selected.get(&key).and_then(json::Value::as_str) else { break };
                filaments.push(name.to_string());
            }
        }
        (selected, filaments, selection_string(selected, BED_TYPE), if tree == 1 { FORK_PLATE_INDICES } else { MAINLINE_PLATE_INDICES })
    };
    let plate = plate_indices.iter().find(|(index, _)| *index == plate_index).map(|(_, i)| PLATES[*i].0.to_string()).unwrap_or_default();
    let colours = selection_string(selected, COLOURS).split(',').filter(|c| !c.is_empty()).map(str::to_string).collect();
    let nozzle_volume_type = v.get(NOZZLE_TYPES).and_then(|n| n.get(&machine)).and_then(json::Value::as_str)
        .map(|t| t.split(',').map(str::trim).filter(|t| !t.is_empty()).map(str::to_string).collect()).unwrap_or_default();
    let sel = Selection { machine, plate, process: selection_string(selected, PROCESS).to_string(), filaments, colours, nozzle_volume_type };
    if sel.machine.is_empty() || sel.process.is_empty() || sel.filaments.is_empty() {
        return None;
    }
    Some(sel)
}

/// The nozzle flow type of each of `machine`'s extruders: as the user set it
/// for that printer in the app, else the printer preset's default, else
/// Standard. It picks the slot of every speed, flow and temperature value.
pub fn nozzle_flow_types(machine: &Preset, selection: Option<&Selection>) -> Vec<String> {
    let n = machine.values.vec_len("nozzle_diameter").max(1);
    let from_app = selection.filter(|s| s.machine == machine.name || machine.system_parent == s.machine).map(|s| s.nozzle_volume_type.clone()).unwrap_or_default();
    let default = machine.values.get("default_nozzle_volume_type").map(|v| v.as_vec()).unwrap_or_default();
    (0..n)
        .map(|e| from_app.get(e).or_else(|| default.get(e)).or_else(|| default.first()).cloned().filter(|t| !t.is_empty()).unwrap_or_else(|| "Standard".into()))
        .collect()
}

/// The user's slicer data directories and a name index of their presets, with the presets Fab
/// carries itself.
pub struct ProfileStore {
    pub roots: Vec<PathBuf>,
    index: HashMap<String, (PathBuf, usize)>,
    groups: Vec<HashMap<String, PathBuf>>,
    /// Each group's slicer data folder name ("" for an extra folder).
    group_trees: Vec<String>,
    /// Presets given in memory ([`ProfileStore::with_builtin`]), by name. A preset file of the
    /// same name wins over one of these.
    builtin: HashMap<String, RawPreset>,
}

impl ProfileStore {
    /// The first slicer's data folder, used for the starting selection and CLI default.
    pub fn default_data_dir() -> Option<PathBuf> {
        Some(crate::home::vendor_data_dir(VENDOR_APP_DIR))
    }

    /// The installed desktop slicers' data folders, in lookup order.
    pub fn default_data_dirs() -> Vec<PathBuf> {
        SLICER_APPS.iter().map(|app| crate::home::vendor_data_dir(app)).collect()
    }

    /// [`open`](Self::open) with the presets imported into Fab's own profile folder.
    pub fn open_with_imports(data_dirs: &[PathBuf]) -> Self {
        Self::open(data_dirs, &[crate::import::fab_profile_dir()])
    }

    /// Index each data folder's `system/` and `user/` trees as one group,
    /// then each extra folder as its own group. The first group wins by name.
    pub fn open(data_dirs: &[PathBuf], extra: &[PathBuf]) -> Self {
        let mut roots = Vec::new();
        let mut groups = Vec::new();
        let mut index = HashMap::new();
        let group_trees: Vec<String> = data_dirs.iter().map(|dir| dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).chain(extra.iter().map(|_| String::new())).collect();
        for folders in data_dirs.iter().map(|dir| vec![dir.join("system"), dir.join("user")]).chain(extra.iter().map(|dir| vec![dir.clone()])) {
            let mut group = HashMap::new();
            for r in &folders {
                index_dir(r, &mut group, 0);
            }
            for (name, path) in &group {
                index.entry(name.clone()).or_insert_with(|| (path.clone(), groups.len()));
            }
            roots.extend(folders);
            groups.push(group);
        }
        Self { roots, index, groups, group_trees, builtin: HashMap::new() }
    }

    /// The store with `presets` beside its files: presets Fab carries itself, for printers whose
    /// numbers come from their manuals rather than from an installed slicer. A preset file of the
    /// same name, installed or imported, is the one the name finds.
    pub fn with_builtin(mut self, presets: Vec<RawPreset>) -> Self {
        for p in presets {
            self.builtin.entry(p.name.clone()).or_insert(p);
        }
        self
    }

    pub fn len(&self) -> usize {
        self.index.len() + self.builtin.keys().filter(|n| !self.index.contains_key(*n)).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The file a preset is read from (none for a built-in one).
    pub fn path_of(&self, name: &str) -> Option<&PathBuf> {
        self.index.get(name).map(|(path, _)| path)
    }

    /// Whether `name` is a preset Fab carries itself, not one read from a file.
    pub fn is_builtin(&self, name: &str) -> bool {
        !self.index.contains_key(name) && self.builtin.contains_key(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &String> {
        self.index.keys().chain(self.builtin.keys().filter(|n| !self.index.contains_key(*n)))
    }

    /// The preset `name` as it is written, before its parent and includes.
    pub fn raw(&self, name: &str) -> Result<RawPreset, String> {
        self.raw_in_group(name, None).map(|(raw, _)| raw)
    }

    fn raw_in_group(&self, name: &str, group: Option<usize>) -> Result<(RawPreset, Option<usize>), String> {
        if let Some(g) = group {
            if let Some(path) = self.groups[g].get(name) {
                return load_preset_file(path).map(|raw| (raw, Some(g)));
            }
        }
        match (self.index.get(name), self.builtin.get(name)) {
            (Some((path, g)), _) => load_preset_file(path).map(|raw| (raw, Some(*g))),
            (None, Some(p)) => Ok((p.clone(), None)),
            (None, None) => Err(format!("preset not found: {name}")),
        }
    }

    /// Resolve a preset by name: parent first, then include fragments, then
    /// its own keys; variant vectors with `nil` slots keep the parent's.
    pub fn resolve(&self, name: &str) -> Result<Preset, String> {
        let mut p = self.resolve_depth(name, None, 0)?;
        pad_variants(&mut p.values);
        Ok(p)
    }

    /// The slicer data folder a file found in `group` is read under: its group's. A file in a
    /// folder of its own (an import) was written by the slicer it names, or else by the slicer of
    /// the preset it belongs to, `inherited`; a built-in preset is Fab's own.
    fn tree_in(&self, group: Option<usize>, inherited: &str) -> String {
        match group.map(|g| self.group_trees[g].as_str()) {
            Some("") => inherited.to_string(),
            Some(tree) => tree.to_string(),
            None => String::new(),
        }
    }

    fn resolve_depth(&self, name: &str, group: Option<usize>, depth: u32) -> Result<Preset, String> {
        if depth > 32 {
            return Err(format!("preset inheritance too deep at {name}"));
        }
        let (mut raw, group) = self.raw_in_group(name, group)?;
        let mut out = if raw.inherits.is_empty() {
            Preset { name: raw.name.clone(), kind: raw.kind.clone(), tree: self.tree_in(group, &raw.tree), ..Default::default() }
        } else {
            let parent = self.resolve_depth(&raw.inherits, group, depth + 1)?;
            let system_parent = if raw.from.eq_ignore_ascii_case("system") { String::new() } else if parent.system_parent.is_empty() { parent.name.clone() } else { parent.system_parent.clone() };
            let tree = self.tree_in(group, if raw.tree.is_empty() { &parent.tree } else { &raw.tree });
            Preset { name: raw.name.clone(), kind: raw.kind.clone(), setting_id: parent.setting_id, filament_id: parent.filament_id, system_parent, values: parent.values, tree }
        };
        // Each file's keys read as Fab's before they land on its parent's, so a child's key
        // wins over its parent's under either name.
        crate::trees::read_names(&out.tree, &mut raw.values);
        // Vendor files replace whole values (a leaf restates its own slot
        // layout and every variant vector); a user's diff lands slot by slot.
        for inc in &raw.include {
            let (mut f, from) = self.raw_in_group(inc, group).map_err(|_| format!("{name}: include not found: {inc}"))?;
            crate::trees::read_names(&self.tree_in(from, &out.tree), &mut f.values);
            replace_keys(&mut out.values, &f.values);
        }
        if raw.from.eq_ignore_ascii_case("system") {
            replace_keys(&mut out.values, &raw.values);
        } else {
            merge_child(&mut out.values, &raw.values);
        }
        out.name = raw.name;
        if !raw.kind.is_empty() {
            out.kind = raw.kind;
        }
        if !raw.setting_id.is_empty() {
            out.setting_id = raw.setting_id;
        }
        if !raw.filament_id.is_empty() {
            out.filament_id = raw.filament_id;
        }
        Ok(out)
    }
}

fn index_dir(dir: &Path, index: &mut HashMap<String, PathBuf>, depth: u32) {
    if depth > 6 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            index_dir(&p, index, depth + 1);
        } else if p.extension().map(|e| e == "json").unwrap_or(false) {
            // Only the name is needed for the index; read the head of the file.
            if let Ok(bytes) = std::fs::read(&p) {
                if let Some(name) = quick_name(&bytes) {
                    index.entry(name).or_insert(p);
                }
            }
        }
    }
}

fn quick_name(bytes: &[u8]) -> Option<String> {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let text = std::str::from_utf8(bytes).ok()?;
    let i = text.find("\"name\"")?;
    let rest = &text[i + 6..];
    let q0 = rest.find('"')?;
    let rest = &rest[q0 + 1..];
    let q1 = rest.find('"')?;
    Some(rest[..q1].to_string())
}

/// The variant layout keys of a preset kind's variant vectors, by axis: the
/// extruder-id key, the variant-name key and the values per row.
fn layout(axis: Axis) -> Option<(&'static str, &'static str, usize)> {
    match axis {
        Axis::PrinterVariant => Some(("printer_extruder_id", "printer_extruder_variant", 1)),
        Axis::PrinterVariant2 => Some(("printer_extruder_id", "printer_extruder_variant", 2)),
        Axis::ProcessVariant => Some(("print_extruder_id", "print_extruder_variant", 1)),
        Axis::FilamentVariant => Some(("", "filament_extruder_variant", 1)),
        _ => None,
    }
}

const LAYOUT_KEYS: &[&str] = &["printer_extruder_id", "printer_extruder_variant", "print_extruder_id", "print_extruder_variant", "filament_extruder_variant"];

/// The (extruder id, variant) rows of a config's layout.
fn rows(cfg: &Config, id_key: &str, var_key: &str) -> Option<Vec<(String, String)>> {
    let vars = cfg.get(var_key)?.as_vec();
    let ids = if id_key.is_empty() { vec![String::new(); vars.len()] } else { cfg.get(id_key)?.as_vec() };
    (ids.len() == vars.len()).then(|| ids.into_iter().zip(vars).collect())
}

/// Child keys over parent keys, each value replaced whole.
pub fn replace_keys(parent: &mut Config, child: &Config) {
    for (k, v) in &child.map {
        parent.map.insert(k.clone(), v.clone());
    }
}

/// A variant vector shorter than its preset's slot layout is padded with its
/// first entry (a vendor file that does not restate one for every slot).
pub fn pad_variants(cfg: &mut Config) {
    let opts = options();
    let keys: Vec<String> = cfg.map.keys().cloned().collect();
    for k in keys {
        let Some((idk, vark, stride)) = layout(opts.axis(&k)) else { continue };
        let Some(n) = rows(cfg, idk, vark).map(|r| r.len() * stride) else { continue };
        if let Some(Value::Vector(v)) = cfg.map.get_mut(&k) {
            if !v.is_empty() && v.len() < n {
                let first = v[0].clone();
                v.resize(n, first);
            }
        }
    }
}

/// Child keys over parent keys. The parent's variant layout stays: a child's
/// variant vector lands row by row on the parent's rows of the same
/// (extruder, variant), and `nil` slots keep the parent's value.
pub fn merge_child(parent: &mut Config, child: &Config) {
    let opts = options();
    for (k, v) in &child.map {
        if LAYOUT_KEYS.contains(&k.as_str()) && parent.has(k) {
            continue;
        }
        if let (Some((idk, vark, stride)), Value::Vector(cv)) = (layout(opts.axis(k)), v) {
            if let (Some(prow), Some(crow)) = (rows(parent, idk, vark), rows(child, idk, vark)) {
                if let Some(Value::Vector(pv)) = parent.map.get_mut(k) {
                    if pv.len() == prow.len() * stride && cv.len() == crow.len() * stride {
                        for (ci, r) in crow.iter().enumerate() {
                            let Some(pi) = prow.iter().position(|p| p == r) else { continue };
                            for s in 0..stride {
                                let c = &cv[ci * stride + s];
                                if c != "nil" {
                                    pv[pi * stride + s] = c.clone();
                                }
                            }
                        }
                        continue;
                    }
                }
            }
        }
        match (parent.map.get_mut(k), v) {
            (Some(Value::Vector(pv)), Value::Vector(cv)) if cv.iter().any(|e| e == "nil") && pv.len() == cv.len() => {
                for (p, c) in pv.iter_mut().zip(cv.iter()) {
                    if c != "nil" {
                        *p = c.clone();
                    }
                }
            }
            _ => {
                parent.map.insert(k.clone(), v.clone());
            }
        }
    }
}

/// What a job puts together: one printer, one process, a filament per slot,
/// and how the filaments map onto the physical extruders.
#[derive(Clone, Debug)]
pub struct JobSetup {
    pub machine: Preset,
    pub process: Preset,
    pub filaments: Vec<Preset>,
    /// 1-based extruder per filament (1 = left, 2 = right on the dual-nozzle printer).
    pub filament_map: Vec<u32>,
    /// Nozzle flow type per physical extruder ("Standard", "High Flow", ...).
    pub nozzle_volume_type: Vec<String>,
    pub bed_type: String,
    pub filament_map_mode: String,
    /// Colours overriding the presets' `filament_colour` (AMS readout).
    pub filament_colours: Vec<Option<String>>,
}

/// Keys the job configuration never carries.
const NOT_IN_JOB: &[&str] = &["compatible_printers", "compatible_prints", "inherits", "host_type", "printhost_apikey", "printhost_cafile", "printhost_password", "printhost_port", "printhost_user", "printhost_authorization_type", "printhost_ssl_ignore_revoke", "print_host", "print_host_webui"];

fn slot_for(ids: &[String], variants: &[String], extruder: usize, want: &str) -> usize {
    let id = (extruder + 1).to_string();
    if let Some(i) = ids.iter().zip(variants.iter()).position(|(a, v)| *a == id && v == want) {
        return i;
    }
    ids.iter().position(|a| *a == id).unwrap_or(0)
}

fn flow_slot(support: &[String], nozzle_volume_type: &str) -> usize {
    let mode = match nozzle_volume_type {
        "Standard" => "standard",
        "High Flow" => "high_flow",
        _ => return 0,
    };
    support.iter().position(|s| s == mode).unwrap_or(0)
}

impl JobSetup {
    pub fn extruder_count(&self) -> usize {
        self.machine.values.vec_len("nozzle_diameter").max(1)
    }

    /// This job with only its filament slots `kept` (0-based, ascending), in that order, each
    /// on its own nozzle and with its own colour: the slots a slice printed in. A purge matrix
    /// the process gives for every slot keeps the kept pairs.
    pub fn keep(&self, kept: &[usize]) -> JobSetup {
        let mut process = self.process.clone();
        let nf = self.filaments.len();
        let matrix = process.values.get("flush_volumes_matrix").map(|v| v.as_vec()).filter(|m| nf > 0 && !m.is_empty() && m.len() % (nf * nf) == 0);
        if let Some(m) = matrix {
            let kept_pairs = (0..m.len() / (nf * nf)).flat_map(|e| kept.iter().flat_map(move |&a| kept.iter().map(move |&b| e * nf * nf + a * nf + b)));
            process.values.set_vec("flush_volumes_matrix", kept_pairs.map(|i| m[i].clone()).collect());
        }
        JobSetup {
            machine: self.machine.clone(),
            process,
            filaments: kept.iter().map(|&f| self.filaments[f].clone()).collect(),
            filament_map: kept.iter().map(|&f| self.filament_map.get(f).copied().unwrap_or(1)).collect(),
            nozzle_volume_type: self.nozzle_volume_type.clone(),
            bed_type: self.bed_type.clone(),
            filament_map_mode: self.filament_map_mode.clone(),
            filament_colours: kept.iter().map(|&f| self.filament_colours.get(f).cloned().flatten()).collect(),
        }
    }

    /// The variant name of each physical extruder ("Direct Drive Standard").
    pub fn extruder_variants(&self) -> Vec<String> {
        let n = self.extruder_count();
        (0..n)
            .map(|e| {
                let et = self.machine.values.raw("extruder_type", e);
                let et = if et.is_empty() { "Direct Drive" } else { et };
                let nv = self.nozzle_volume_type.get(e).map(|s| s.as_str()).unwrap_or("Standard");
                format!("{et} {nv}")
            })
            .collect()
    }

    /// The collapsed job configuration: defaults, then the printer, process
    /// and filament presets, with every variant vector reduced to one entry
    /// per extruder (or per filament) and the job keys filled in.
    pub fn build_config(&self) -> Config {
        let opts = options();
        let n_ext = self.extruder_count();
        let nf = self.filaments.len().max(1);
        let variants = self.extruder_variants();
        let mut cfg = Config::new();
        // Defaults for every known key.
        for d in opts.defs.values() {
            if d.default == "nil" {
                continue;
            }
            cfg.map.insert(d.key.clone(), crate::config::parse_text(&d.key, &d.default));
        }
        // The defaults of the slicer the printer's presets come from, where they differ.
        let tree = crate::trees::defaults(&self.machine.tree);
        let tree_default = |k: &str| tree.iter().filter(|(key, v)| *key == k && !v.starts_with('=')).last().map(|(_, v)| *v);
        for (k, v) in tree.iter().filter(|(_, v)| !v.starts_with('=')) {
            cfg.map.insert(k.to_string(), crate::config::parse_text(k, v));
        }
        // Printer and process, collapsed by slot.
        let m = &self.machine.values;
        let p = &self.process.values;
        let m_ids = m.get("printer_extruder_id").map(|v| v.as_vec()).unwrap_or_default();
        let m_var = m.get("printer_extruder_variant").map(|v| v.as_vec()).unwrap_or_default();
        let p_ids = p.get("print_extruder_id").map(|v| v.as_vec()).unwrap_or_default();
        let p_var = p.get("print_extruder_variant").map(|v| v.as_vec()).unwrap_or_default();
        let p_flow = if p.has("print_extruder_id") { Vec::new() } else { p.get("process_flow_support").map(|v| v.as_vec()).unwrap_or_default() };
        let m_slots: Vec<usize> = (0..n_ext).map(|e| slot_for(&m_ids, &m_var, e, &variants[e])).collect();
        let p_slots: Vec<usize> = (0..n_ext).map(|e| slot_for(&p_ids, &p_var, e, &variants[e])).collect();
        for (is_process, src) in [(false, m), (true, p)] {
            for (k, v) in &src.map {
                let axis = opts.axis(k);
                let val = match (axis, v) {
                    (Axis::PrinterVariant, Value::Vector(items)) if items.len() == m_ids.len() && !m_ids.is_empty() => Value::Vector(m_slots.iter().map(|&s| items[s].clone()).collect()),
                    (Axis::PrinterVariant2, Value::Vector(items)) if items.len() == 2 * m_ids.len() && !m_ids.is_empty() => Value::Vector(m_slots.iter().flat_map(|&s| [items[2 * s].clone(), items[2 * s + 1].clone()]).collect()),
                    (Axis::ProcessVariant, Value::Vector(items)) if items.len() == p_ids.len() && !p_ids.is_empty() => Value::Vector(p_slots.iter().map(|&s| items[s].clone()).collect()),
                    (Axis::ProcessVariant, Value::Vector(items)) if is_process && !p_flow.is_empty() && items.len() == p_flow.len() => {
                        Value::Vector((0..n_ext).map(|e| items[flow_slot(&p_flow, self.nozzle_volume_type.get(e).map(String::as_str).unwrap_or("Standard"))].clone()).collect())
                    }
                    _ => v.clone(),
                };
                cfg.map.insert(k.clone(), val);
            }
        }
        let ext_ids: Vec<String> = (1..=n_ext).map(|e| e.to_string()).collect();
        cfg.set_vec("printer_extruder_id", ext_ids.clone());
        cfg.set_vec("printer_extruder_variant", variants.clone());
        cfg.set_vec("print_extruder_id", ext_ids);
        cfg.set_vec("print_extruder_variant", variants.clone());

        // Filaments: one entry per filament slot.
        let fmap: Vec<u32> = (0..nf).map(|f| self.filament_map.get(f).copied().unwrap_or(1).max(1)).collect();
        let mut fil_keys: Vec<String> = Vec::new();
        for f in &self.filaments {
            for k in f.values.map.keys() {
                if !fil_keys.contains(k) {
                    fil_keys.push(k.clone());
                }
            }
        }
        for k in &fil_keys {
            let def = opts.get(k);
            let is_vec = def.map(|d| d.kind.is_vector()).unwrap_or(true);
            let axis = def.map(|d| d.axis).unwrap_or(Axis::None);
            if !is_vec {
                // A scalar filament option: the first filament's value.
                if let Some(v) = self.filaments[0].values.get(k) {
                    cfg.map.insert(k.clone(), v.clone());
                }
                continue;
            }
            let mut items = Vec::with_capacity(nf);
            for (fi, f) in self.filaments.iter().enumerate() {
                let fv = &f.values;
                let ext = (fmap[fi] as usize - 1).min(n_ext - 1);
                let item = match fv.get(k) {
                    Some(Value::Vector(v)) if axis == Axis::FilamentVariant => {
                        let fvars = fv.get("filament_extruder_variant").map(|x| x.as_vec()).unwrap_or_default();
                        let support = if fv.has("filament_extruder_variant") { Vec::new() } else { fv.get("filament_flow_support").map(|x| x.as_vec()).unwrap_or_default() };
                        let slot = if !support.is_empty() && v.len() == support.len() {
                            flow_slot(&support, self.nozzle_volume_type.get(ext).map(String::as_str).unwrap_or("Standard"))
                        } else {
                            fvars.iter().position(|x| *x == variants[ext]).unwrap_or(0)
                        };
                        v.get(slot).or_else(|| v.first()).cloned().unwrap_or_default()
                    }
                    Some(v) => v.at(0).to_string(),
                    None => tree_default(k).or(def.map(|d| d.default.as_str())).map(|text| crate::config::parse_text(k, text).at(0).to_string()).unwrap_or_default(),
                };
                items.push(item);
            }
            cfg.map.insert(k.clone(), Value::Vector(items));
        }
        cfg.set_vec("filament_extruder_variant", fmap.iter().map(|&e| variants[(e as usize - 1).min(n_ext - 1)].clone()).collect());
        // Filament overrides that no filament sets are left out.
        let drop: Vec<String> = cfg
            .map
            .iter()
            .filter(|(k, v)| {
                opts.get(k).map(|d| d.preset == PresetKind::Filament && d.default == "nil").unwrap_or(false) && v.as_vec().iter().all(|e| e == "nil" || e.is_empty())
            })
            .map(|(k, _)| k.clone())
            .collect();
        for k in drop {
            cfg.remove(&k);
        }
        // One colour per filament slot: presets that set no colour leave the option's single
        // default, which every slot would then read (paint by colour would find only slot 1).
        if let Some(Value::Vector(v)) = cfg.map.get_mut("filament_colour") {
            if let Some(last) = v.last().cloned() {
                v.resize(v.len().max(nf), last);
            }
            for (i, c) in self.filament_colours.iter().enumerate() {
                if let (Some(c), Some(slot)) = (c, v.get_mut(i)) {
                    *slot = c.clone();
                }
            }
        }

        // Defaults that are another key's value, where no preset sets the key.
        let set = |k: &str| m.has(k) || p.has(k) || self.filaments.iter().any(|f| f.values.has(k));
        for (k, v) in &tree {
            if let Some(from) = v.strip_prefix('=').filter(|_| !set(k)) {
                if let Some(val) = cfg.get(from).cloned() {
                    cfg.map.insert(k.to_string(), val);
                }
            }
        }

        // Job keys.
        cfg.set_vec("filament_map", fmap.iter().map(|e| e.to_string()).collect());
        cfg.set_vec("filament_map_2", fmap.iter().map(|e| (e - 1).to_string()).collect());
        cfg.set_vec("filament_nozzle_map", fmap.iter().map(|e| (e - 1).to_string()).collect());
        cfg.set_vec("filament_volume_map", vec!["0".to_string(); nf]);
        cfg.set_vec("filament_self_index", (1..=nf).map(|i| i.to_string()).collect());
        cfg.set_vec("filament_ids", self.filaments.iter().map(|f| f.filament_id.clone()).collect());
        cfg.set_vec("filament_settings_id", self.filaments.iter().map(|f| f.name.clone()).collect());
        cfg.set_str("print_settings_id", self.process.name.clone());
        cfg.set_str("printer_settings_id", self.machine.name.clone());
        cfg.set_vec("nozzle_volume_type", (0..n_ext).map(|e| self.nozzle_volume_type.get(e).cloned().unwrap_or_else(|| "Standard".into())).collect());
        cfg.set_vec("extruder_nozzle_stats", (0..n_ext).map(|e| format!("{}#1", self.nozzle_volume_type.get(e).map(|s| s.as_str()).unwrap_or("Standard"))).collect());
        cfg.set_str("curr_bed_type", self.bed_type.clone());
        if !self.filament_map_mode.is_empty() {
            cfg.set_str("filament_map_mode", self.filament_map_mode.clone());
        }
        if let Some(cp) = p.get("compatible_printers") {
            cfg.set("print_compatible_printers", cp.clone());
        }
        let mut groups = vec![self.process.system_parent.clone()];
        groups.extend(self.filaments.iter().map(|f| f.system_parent.clone()));
        groups.push(self.machine.system_parent.clone());
        cfg.set_vec("inherits_group", groups);
        cfg.set_vec("different_settings_to_system", vec![String::new(); nf + 2]);
        for k in NOT_IN_JOB {
            cfg.remove(k);
        }
        cfg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAKE_TOKEN: &str = "UNSELECTED-TOKEN-XYZ";

    /// The selection of a data folder holding `<app>.conf`. The folder has another name, as a
    /// portable install's has: the settings file's name says which tree's format it is.
    fn selection_fixture(app: &str, case: &str, text: &str) -> Option<Selection> {
        let root = std::env::temp_dir().join(format!("selection-{case}-{}", std::process::id()));
        let dir = root.join("data_dir");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{app}.conf")), text).unwrap();
        let result = installed_selection(&dir);
        std::fs::remove_dir_all(&root).unwrap();
        assert!(!format!("{result:?}").contains(FAKE_TOKEN), "selection excludes private fields");
        assert!(!selection_values(text.as_bytes()).unwrap().to_json().contains(FAKE_TOKEN), "projection excludes private fields");
        result
    }

    fn first_tree_fixture(plate: &str) -> String {
        r##"{
            "user":{"token":"UNSELECTED-TOKEN-XYZ"},
            "presets":{"machine":"M","process":"P","filaments":["F1","F2"],
                "filament_colors":"#112233,#445566","curr_bed_type":"5"},
            "app":{"curr_bed_type":"PLATE","access_token":"UNSELECTED-TOKEN-XYZ",
                "filament_colors":"UNSELECTED-TOKEN-XYZ"},
            "orca_presets":[{"machine":"M","access_token":"UNSELECTED-TOKEN-XYZ"}],
            "nozzle_volume_types":{"M":"Standard, High Flow"}
        }"##.replace("PLATE", plate)
    }

    fn per_machine_fixture(plate: &str) -> String {
        r##"{
            "user":{"token":"UNSELECTED-TOKEN-XYZ"},
            "presets":{"machine":"M","process":"wrong","filaments":[],"filament_colors":"wrong"},
            "app":{"curr_bed_type":"4","access_token":"UNSELECTED-TOKEN-XYZ",
                "filament_colors":"UNSELECTED-TOKEN-XYZ"},
            "orca_presets":[
                {"machine":"other","process":"other-process","filament":"other-filament",
                    "filament_colors":"other-colour","curr_bed_type":"1"},
                {"machine":"M","process":"P","filament":"F1","filament_01":"F2",
                    "filament_02":"F3","filament_04":"not-contiguous",
                    "filament_colors":"#112233,#445566,#778899","curr_bed_type":"PLATE",
                    "access_token":"UNSELECTED-TOKEN-XYZ",
                    "extra":{"secret":"UNSELECTED-TOKEN-XYZ"}}
            ],
            "nozzle_volume_types":{"other":"other-flow","M":"Standard, High Flow"}
        }"##.replace("PLATE", plate)
    }

    #[test]
    fn first_tree_selection_uses_app_plate_and_selection_colours() {
        for (index, expected) in [("0", ""), ("1", "Cool Plate"), ("2", "Engineering Plate"),
            ("3", "High Temp Plate"), ("4", "Textured PEI Plate"), ("5", "Supertack Plate"), ("6", ""), ("bad", "")] {
            let text = first_tree_fixture(index);
            let selected = selection_fixture(VENDOR_APP_DIR, "first", &text).unwrap();
            assert_eq!(selected.machine, "M");
            assert_eq!(selected.process, "P");
            assert_eq!(selected.filaments, ["F1", "F2"]);
            assert_eq!(selected.colours, ["#112233", "#445566"]);
            assert_eq!(selected.nozzle_volume_type, ["Standard", "High Flow"]);
            assert_eq!(selected.plate, expected, "first tree index {index}");
        }
        let text = first_tree_fixture("4").replace(r#""curr_bed_type":"4","#, "");
        assert_eq!(selection_fixture(VENDOR_APP_DIR, "first", &text).unwrap().plate, "", "the presets plate is ignored");
        let checksum = format!("{}\nchecksum: fixture\n", first_tree_fixture("4"));
        assert_eq!(selection_fixture(VENDOR_APP_DIR, "first", &checksum).unwrap().plate, "Textured PEI Plate");
        assert!(selection_fixture("unknown", "unknown", &checksum).is_none(), "no app's settings file, no selection");
        let no_app = r#"{"presets":{"machine":"M","process":"P","filaments":["F"],"curr_bed_type":"4"}}"#;
        let selected = selection_fixture(VENDOR_APP_DIR, "no-app", no_app).unwrap();
        assert_eq!((selected.machine.as_str(), selected.plate.as_str()), ("M", ""), "without an app section the plate is left to the person");
    }

    #[test]
    fn mainline_selection_matches_machine_and_stops_at_the_first_gap() {
        for (index, expected) in [("0", ""), ("1", "Cool Plate"), ("2", "Engineering Plate"),
            ("3", "High Temp Plate"), ("4", "Textured PEI Plate"), ("5", "Textured Cool Plate"),
            ("6", "Supertack Plate"), ("7", "")] {
            let text = per_machine_fixture(index);
            let selected = selection_fixture(SLICER_APPS[2], "mainline", &text).unwrap();
            assert_eq!(selected.machine, "M");
            assert_eq!(selected.process, "P");
            assert_eq!(selected.filaments, ["F1", "F2", "F3"]);
            assert_eq!(selected.colours, ["#112233", "#445566", "#778899"]);
            assert_eq!(selected.nozzle_volume_type, ["Standard", "High Flow"]);
            assert_eq!(selected.plate, expected, "mainline index {index}");
            assert!(!format!("{selected:?}").contains("other"));
        }
    }

    #[test]
    fn fork_selection_maps_plates_without_app_fallback() {
        for (index, expected) in [("0", "Cool Plate"), ("1", "Cool Plate"), ("2", "Engineering Plate"),
            ("3", "High Temp Plate"), ("4", "Textured PEI Plate"), ("5", "Textured Cool Plate"),
            ("6", ""), ("7", "Supertack Plate"), ("8", ""), ("", "")] {
            let text = per_machine_fixture(index);
            // This tree does not write nozzle flow types.
            let text = text.replace(r#",
            "nozzle_volume_types":{"other":"other-flow","M":"Standard, High Flow"}"#, "");
            let selected = selection_fixture(SLICER_APPS[1], "fork", &text).unwrap();
            assert_eq!(selected.plate, expected, "fork index {index}");
            assert_eq!(selected.filaments, ["F1", "F2", "F3"]);
            assert!(selected.nozzle_volume_type.is_empty());
        }
        for tree in [1, 2] {
            let text = per_machine_fixture("").replace(r#""curr_bed_type":"","#, "");
            assert_eq!(selection_fixture(SLICER_APPS[tree], "missing-plate", &text).unwrap().plate, "");
        }
    }

    #[test]
    fn selection_requires_strings_and_never_keeps_nested_private_values() {
        for (key, value) in [("machine", "M"), ("process", "P"), ("filaments", "")] {
            let text = first_tree_fixture("4");
            let original = if key == "filaments" { r#""filaments":["F1","F2"]"#.to_string() } else { format!(r#""{key}":"{value}""#) };
            for replacement in ["null", "42", "false", r#"{"secret":"UNSELECTED-TOKEN-XYZ"}"#, r#"["UNSELECTED-TOKEN-XYZ"]"#] {
                // A filament array is the expected container; its strings are selection data.
                if key == "filaments" && replacement.starts_with('[') { continue; }
                let text = text.replace(&original, &format!(r#""{key}":{replacement}"#));
                assert!(selection_fixture(VENDOR_APP_DIR, "wrong-type", &text).is_none());
            }
        }
        let text = first_tree_fixture("4")
            .replace(r#""filaments":["F1","F2"]"#, r#""filaments":[{"secret":"UNSELECTED-TOKEN-XYZ"},"F2"]"#)
            .replace(r##""filament_colors":"#112233,#445566""##, r#""filament_colors":{"secret":"UNSELECTED-TOKEN-XYZ"}"#)
            .replace(r#""curr_bed_type":"4""#, r#""curr_bed_type":{"secret":"UNSELECTED-TOKEN-XYZ"}"#)
            .replace(r#""M":"Standard, High Flow""#, r#""M":{"secret":"UNSELECTED-TOKEN-XYZ"}"#);
        let selected = selection_fixture(VENDOR_APP_DIR, "wrong-leaf", &text).unwrap();
        assert_eq!(selected.filaments, ["F2"]);
        assert!(selected.colours.is_empty() && selected.plate.is_empty() && selected.nozzle_volume_type.is_empty());
        for tree in [1, 2] {
            for (key, value) in [("machine", "M"), ("process", "P"), ("filament", "F1")] {
                let text = per_machine_fixture("5").replace(&format!(r#""{key}":"{value}""#), &format!(r#""{key}":{{"secret":"UNSELECTED-TOKEN-XYZ"}}"#));
                assert!(selection_fixture(SLICER_APPS[tree], "wrong-per-machine", &text).is_none());
            }
            let text = per_machine_fixture("5").replace(r#""filament_01":"F2""#, r#""filament_01":{"secret":"UNSELECTED-TOKEN-XYZ"}"#);
            assert_eq!(selection_fixture(SLICER_APPS[tree], "wrong-slot", &text).unwrap().filaments, ["F1"]);
        }
        for text in [first_tree_fixture("4").replace(r#""machine":"M""#, r#""machine":"""#),
            first_tree_fixture("4").replace(r#""process":"P""#, r#""process":"""#),
            first_tree_fixture("4").replace(r#"["F1","F2"]"#, "[]")] {
            assert!(selection_fixture(VENDOR_APP_DIR, "empty", &text).is_none());
        }
    }

    #[test]
    fn presets_resolve_their_parents_in_their_own_slicers_folder() {
        let root = std::env::temp_dir().join(format!("fab-settings-groups-{}", std::process::id()));
        let dirs = [root.join("first"), root.join("second")];
        for (dir, height) in dirs.iter().zip(["100", "200"]) {
            let folder = dir.join("system/V/machine");
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join("base.json"), format!(r#"{{"type":"machine","name":"base","from":"system","printable_height":"{height}"}}"#)).unwrap();
            std::fs::write(folder.join("fragment.json"), format!(r#"{{"name":"fragment","machine_max_speed_x":["{height}"]}}"#)).unwrap();
        }
        let first = dirs[0].join("system/V/machine");
        let second = dirs[1].join("system/V/machine");
        std::fs::write(second.join("child.json"), r#"{"type":"machine","name":"child","inherits":"base","include":["fragment"]}"#).unwrap();
        std::fs::write(first.join("fallback.json"), r#"{"type":"machine","name":"fallback","inherits":"base"}"#).unwrap();
        let user = dirs[1].join("user/test/machine");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(user.join("mine.json"), r#"{"type":"machine","name":"mine","inherits":"child"}"#).unwrap();
        std::fs::write(user.join("other.json"), r#"{"type":"machine","name":"other","inherits":"fallback"}"#).unwrap();
        let extra = root.join("extra");
        std::fs::create_dir_all(&extra).unwrap();
        std::fs::write(extra.join("base.json"), r#"{"type":"machine","name":"base","printable_height":"300"}"#).unwrap();
        std::fs::write(extra.join("imported.json"), r#"{"type":"machine","name":"imported","inherits":"base"}"#).unwrap();
        let store = ProfileStore::open(&dirs, std::slice::from_ref(&extra));
        assert_eq!(store.roots, [dirs[0].join("system"), dirs[0].join("user"), dirs[1].join("system"), dirs[1].join("user"), extra]);
        assert_eq!(store.len(), 7);
        assert_eq!(store.names().count(), 7);
        assert_eq!(store.path_of("base"), Some(&first.join("base.json")));
        assert_eq!(store.raw("base").unwrap().values.str("printable_height"), "100");
        assert_eq!(store.resolve("base").unwrap().values.str("printable_height"), "100", "the first folder wins by name");
        let child = store.resolve("child").unwrap();
        assert_eq!(child.values.str("printable_height"), "200", "a child uses its own folder's parent");
        assert_eq!(child.values.raw("machine_max_speed_x", 0), "200", "includes use the same group");
        assert_eq!(store.resolve("mine").unwrap().values.str("printable_height"), "200", "user and system trees share a group");
        assert_eq!(store.resolve("other").unwrap().values.str("printable_height"), "100", "a fallback parent follows its own group");
        assert_eq!(store.resolve("imported").unwrap().values.str("printable_height"), "300", "an extra folder is its own group");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_byte_order_mark_does_not_hide_a_preset() {
        let dir = std::env::temp_dir().join(format!("fab-settings-bom-{}", std::process::id()));
        let folder = dir.join("system/V/machine");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("marked.json"), b"\xef\xbb\xbf{\"type\":\"machine\",\"name\":\"marked\",\"printable_height\":\"123\"}").unwrap();
        let store = ProfileStore::open(std::slice::from_ref(&dir), &[]);
        assert_eq!(store.len(), 1);
        assert_eq!(store.resolve("marked").unwrap().values.str("printable_height"), "123");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn escaped_templates_get_their_newlines_back() {
        let raw = parse_preset(br#"{
            "type":"machine", "name":"templates",
            "machine_start_gcode":"G28\\nM117 \\\"hi\\\"\\nG1 Z5",
            "machine_end_gcode":"G28\nM117 \\\"hi\\\"\\nG1 Z5",
            "filament_start_gcode":["G28\\nM117 \\\"hi\\\"", "G28\nM117 \\\"hi\\\"\\n"],
            "machine_pause_gcode":"M117 \\\"hi\\\"",
            "notes":"keep\\nthis"
        }"#, Path::new("templates.json")).unwrap();
        let store = ProfileStore::open(&[], &[]).with_builtin(vec![raw]);
        let p = store.resolve("templates").unwrap();
        assert_eq!(p.values.str("machine_start_gcode"), "G28\nM117 \"hi\"\nG1 Z5");
        assert_eq!(p.values.str("machine_end_gcode"), "G28\nM117 \\\"hi\\\"\\nG1 Z5", "real multiline templates stay untouched");
        assert_eq!(p.values.get("filament_start_gcode").unwrap().as_vec(), ["G28\nM117 \"hi\"", "G28\nM117 \\\"hi\\\"\\n"]);
        assert_eq!(p.values.str("machine_pause_gcode"), r#"M117 \"hi\""#, "quotes alone do not trigger decoding");
        assert_eq!(p.values.str("notes"), r"keep\nthis", "only template keys are decoded");
    }

    #[test]
    fn flow_mode_vectors_take_each_toolheads_slot() {
        let presets = [
            r#"{"type":"machine","name":"toolchanger","nozzle_diameter":["0.4","0.4","0.4","0.4"]}"#,
            r#"{"type":"process","name":"base","outer_wall_speed":"100"}"#,
            r#"{"type":"process","name":"process","inherits":"base","process_flow_support":["standard","high_flow"],"outer_wall_speed":["200","500"],"inner_wall_speed":["70"],"layer_height":"0.2"}"#,
            r#"{"type":"filament","name":"filament","filament_flow_support":["standard","high_flow"],"filament_max_volumetric_speed":["15","25"]}"#,
        ].into_iter().map(|s| parse_preset(s.as_bytes(), Path::new("flow.json")).unwrap()).collect();
        let store = ProfileStore::open(&[], &[]).with_builtin(presets);
        let mut job = JobSetup {
            machine: store.resolve("toolchanger").unwrap(), process: store.resolve("process").unwrap(),
            filaments: vec![store.resolve("filament").unwrap()], filament_map: vec![2],
            nozzle_volume_type: ["Standard", "High Flow", "Standard", "Standard"].map(String::from).to_vec(),
            bed_type: String::new(), filament_map_mode: String::new(), filament_colours: Vec::new(),
        };
        let cfg = job.build_config();
        assert_eq!(cfg.get("outer_wall_speed").unwrap().as_vec(), ["200", "500", "200", "200"]);
        assert_eq!(cfg.get("filament_max_volumetric_speed").unwrap().as_vec(), ["25"]);
        assert_eq!(cfg.get("inner_wall_speed").unwrap().as_vec(), ["70"], "a different vector length is unchanged");
        assert_eq!(cfg.str("layer_height"), "0.2", "scalar keys are unchanged");
        job.nozzle_volume_type = vec!["Standard".into(); 4];
        let cfg = job.build_config();
        assert_eq!(cfg.get("outer_wall_speed").unwrap().as_vec(), ["200", "200", "200", "200"]);
        assert_eq!(cfg.get("filament_max_volumetric_speed").unwrap().as_vec(), ["15"]);
        job.process.values.set_vec("process_flow_support", vec!["high_flow".into(), "standard".into()]);
        job.filaments[0].values.set_vec("filament_flow_support", vec!["high_flow".into(), "standard".into()]);
        let cfg = job.build_config();
        assert_eq!(cfg.get("outer_wall_speed").unwrap().as_vec(), ["500"; 4], "the support list defines the order");
        assert_eq!(cfg.get("filament_max_volumetric_speed").unwrap().as_vec(), ["25"]);
        job.process.values.set_vec("process_flow_support", vec!["other".into(), "high_flow".into()]);
        job.filaments[0].values.set_vec("filament_flow_support", vec!["other".into(), "high_flow".into()]);
        let cfg = job.build_config();
        assert_eq!(cfg.get("outer_wall_speed").unwrap().as_vec(), ["200"; 4], "a missing mode uses slot zero");
        assert_eq!(cfg.get("filament_max_volumetric_speed").unwrap().as_vec(), ["15"]);
        job.nozzle_volume_type[1] = "High Flow".into();
        job.process.values.set_vec("print_extruder_id", vec!["1".into(), "2".into()]);
        job.process.values.set_vec("print_extruder_variant", vec!["Direct Drive Standard".into(); 2]);
        job.filaments[0].values.set_vec("filament_extruder_variant", vec!["Direct Drive High Flow".into(), "Direct Drive Standard".into()]);
        let cfg = job.build_config();
        assert_eq!(cfg.get("outer_wall_speed").unwrap().as_vec(), ["200", "500", "200", "200"], "explicit extruder rows keep their path");
        assert_eq!(cfg.get("filament_max_volumetric_speed").unwrap().as_vec(), ["15"], "explicit variants take precedence over flow modes");
    }

    /// A printer read from another slicer's data folder starts from that slicer's defaults: its
    /// own key names and spellings read as Fab's, and what its presets leave out takes that
    /// slicer's value, a reference included.
    #[test]
    fn another_slicers_presets_take_its_defaults_names_and_spellings() {
        let dir = std::env::temp_dir().join(format!("fab-settings-tree-{}", std::process::id())).join("Snapmaker_Orca");
        let _ = std::fs::remove_dir_all(&dir);
        let system = dir.join("system").join("test");
        std::fs::create_dir_all(&system).unwrap();
        std::fs::write(system.join("m.json"), r#"{"type": "machine", "name": "M", "from": "system", "nozzle_diameter": ["0.4", "0.4"], "machine_tool_change_time": "5"}"#).unwrap();
        std::fs::write(system.join("p.json"), r#"{"type": "process", "name": "P", "from": "system", "internal_solid_infill_pattern": "rectilinear", "only_one_wall_top": "1", "wipe_tower_extra_rib_length": "8", "wall_infill_order": "inner wall/outer wall/infill"}"#).unwrap();
        std::fs::write(system.join("f.json"), r#"{"type": "filament", "name": "F", "from": "system", "filament_type": ["PLA"]}"#).unwrap();
        // A preset in a folder of its own (an import) over one of them, setting a renamed key again.
        let imports = dir.parent().unwrap().join("imports");
        std::fs::create_dir_all(&imports).unwrap();
        std::fs::write(imports.join("c.json"), r#"{"type": "process", "name": "C", "inherits": "P", "wipe_tower_extra_rib_length": "12"}"#).unwrap();
        let mut built = Config::new();
        built.set_str("wipe_tower_extra_rib_length", "4");
        let built = RawPreset { kind: "process".into(), name: "B".into(), inherits: "P".into(), instantiation: true, values: built, ..RawPreset::default() };
        let store = ProfileStore::open(std::slice::from_ref(&dir), std::slice::from_ref(&imports)).with_builtin(vec![built]);
        let builtin = store.resolve("B").unwrap();
        assert_eq!(builtin.tree, "", "a built-in preset keeps Fab's own defaults over any parent");
        assert_eq!((builtin.values.str("prime_tower_extra_rib_length"), builtin.values.str("wipe_tower_extra_rib_length")), ("8", "4"), "its own keys are Fab's names, unread");
        let machine = store.resolve("M").unwrap();
        assert_eq!(machine.tree, "Snapmaker_Orca");
        let child = store.resolve("C").unwrap();
        assert_eq!(child.tree, "Snapmaker_Orca", "an import is read as its parents' slicer wrote it");
        assert_eq!(child.values.str("prime_tower_extra_rib_length"), "12", "the child's key wins over its parent's");
        assert!(!child.values.has("wipe_tower_extra_rib_length"));
        let job = JobSetup {
            machine, process: store.resolve("P").unwrap(), filaments: vec![store.resolve("F").unwrap()], filament_map: vec![1],
            nozzle_volume_type: vec!["Standard".into(); 2], bed_type: String::new(), filament_map_mode: String::new(), filament_colours: Vec::new(),
        };
        let cfg = job.build_config();
        assert_eq!(cfg.str("internal_solid_infill_pattern"), "zig-zag");
        assert_eq!(cfg.str("sub_top_surface_pattern"), "zig-zag", "a reference reads the job's value");
        assert_eq!(cfg.str("top_one_wall_type"), "all top");
        assert_eq!(cfg.str("prime_tower_extra_rib_length"), "8");
        assert_eq!(cfg.str("wall_sequence"), "inner wall/outer wall");
        assert_eq!(cfg.str("machine_switch_extruder_time"), "5");
        assert_eq!(cfg.get("travel_acceleration").unwrap().as_vec(), ["10000"]);
        assert_eq!(cfg.get("initial_layer_travel_acceleration").unwrap().as_vec(), ["10000"], "a reference reads the job's value");
        assert_eq!(cfg.raw("filament_cooling_before_tower", 0), "0");
        assert!(!cfg.has("only_one_wall_top") && !cfg.has("wall_infill_order"));
        // The same presets with Fab's own defaults.
        let mut own = job.clone();
        own.machine.tree.clear();
        let cfg = own.build_config();
        assert_eq!(cfg.get("travel_acceleration").unwrap().as_vec(), ["500"]);
        assert_eq!(cfg.str("sub_top_surface_pattern"), "monotonic");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    /// A built-in preset is offered and resolves beside the files, and inherits from one; a
    /// preset file of the same name is the one its name finds.
    #[test]
    fn builtin_presets_sit_beside_the_files_and_a_file_of_their_name_wins() {
        let dir = std::env::temp_dir().join(format!("fab-settings-builtin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let folder = dir.join("system").join("test").join("process");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("base.json"), r#"{"type": "process", "name": "Base", "from": "system", "layer_height": "0.2", "wall_loops": "3"}"#).unwrap();
        std::fs::write(folder.join("mine.json"), r#"{"type": "process", "name": "Both", "from": "system", "layer_height": "0.3"}"#).unwrap();
        let preset = |name: &str, inherits: &str, height: &str| {
            let mut values = Config::new();
            values.set_str("layer_height", height);
            RawPreset { kind: "process".into(), name: name.into(), inherits: inherits.into(), from: "system".into(), instantiation: true, values, ..RawPreset::default() }
        };
        let store = ProfileStore::open(std::slice::from_ref(&dir), &[]).with_builtin(vec![preset("Built", "Base", "0.05"), preset("Both", "", "0.1")]);
        let mut names: Vec<&String> = store.names().collect();
        names.sort();
        assert_eq!(names, ["Base", "Both", "Built"]);
        assert_eq!(store.len(), 3);
        assert!(store.is_builtin("Built") && !store.is_builtin("Both") && !store.is_builtin("Base"));
        let built = store.resolve("Built").expect("the built-in resolves");
        assert_eq!((built.values.str("layer_height"), built.values.str("wall_loops")), ("0.05", "3"), "its own height over its parent's walls");
        assert_eq!(store.resolve("Both").unwrap().values.str("layer_height"), "0.3", "the file of its name");
        assert!(store.raw("Neither").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_job_of_some_of_its_filaments_keeps_theirs() {
        let preset = |name: &str| Preset { name: name.into(), ..Preset::default() };
        let mut process = preset("P");
        // Two nozzles of three slots each: nozzle e's purge from slot i to slot j is 100e + 10i + j.
        process.values.set_vec("flush_volumes_matrix", (0..2).flat_map(|e| (0..3).flat_map(move |i| (0..3).map(move |j| format!("{}", 100 * e + 10 * i + j)))).collect());
        let setup = JobSetup {
            machine: preset("M"),
            process,
            filaments: vec![preset("A"), preset("B"), preset("C")],
            filament_map: vec![1, 2, 1],
            nozzle_volume_type: vec!["Standard".into(), "High Flow".into()],
            bed_type: "Textured PEI Plate".into(),
            filament_map_mode: "Manual".into(),
            filament_colours: vec![Some("#A".into()), None, Some("#C".into())],
        };
        let kept = setup.keep(&[1, 2]);
        assert_eq!(kept.filaments.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["B", "C"]);
        assert_eq!(kept.filament_map, [2, 1], "each on its own nozzle");
        assert_eq!(kept.filament_colours, [None, Some("#C".to_string())], "each with its own colour");
        assert_eq!(kept.process.values.get("flush_volumes_matrix").unwrap().as_vec(), ["11", "12", "21", "22", "111", "112", "121", "122"], "the kept pairs of each nozzle");
        assert_eq!((kept.nozzle_volume_type, kept.bed_type.as_str(), kept.filament_map_mode.as_str()), (setup.nozzle_volume_type.clone(), "Textured PEI Plate", "Manual"));
    }
}
