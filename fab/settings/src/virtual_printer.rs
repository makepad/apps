//! A virtual printer has a bed, heads and loaded trays. Fab builds its machine
//! profile from that layout, or uses the catalogue machine that the printer stands in for.

use crate::{family, machine::PLATES, profiles::RawPreset};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind { Nozzles, Toolchanger }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotPath { Shared, Own, Dedicated }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slot {
    /// Machine head, counted from zero.
    pub head: usize,
    pub tray: String,
    pub path: SlotPath,
    pub material: Option<String>,
    /// sRGB, 0xRRGGBB.
    pub colour: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Generated,
    Twin { preset: String, model: String, source: Option<String> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VirtualPrinter {
    pub name: String,
    pub title: String,
    pub source: Source,
    /// Configured printer that owns this twin, in either mode.
    pub configured_for: Option<String>,
    pub kind: Kind,
    pub heads: usize,
    pub slots: Vec<Slot>,
}

// fab_plate::machine::UNITS. Higher unit ids are reserved for HT and external spools.
const UNITS: usize = 128;
// The swatches in shell's print::colours::FILAMENT_COLOURS.
const BLACK: u32 = 0x161616;
const PALETTE: &[u32] = &[
    0xf4f4f0, BLACK, 0x8e9089, 0xa6a9aa, 0xc12e1f, 0xf0742c, 0xf4d03a,
    0x2e9b4b, 0x2a9d8f, 0x1f5fd0, 0x6c3fa6, 0xe78fb0, 0x7a5232, 0xd9c7a1,
];

fn unit_letters(unit: usize) -> String {
    let mut n = unit + 1;
    let mut letters = Vec::new();
    while n > 0 {
        n -= 1;
        letters.push(char::from(b'A' + (n % 26) as u8));
        n /= 26;
    }
    letters.into_iter().rev().collect()
}

impl Kind {
    fn check_heads(self, heads: usize) -> Result<(), &'static str> {
        match self {
            Self::Nozzles if !(1..=2).contains(&heads) => Err("a tray printer has one or two nozzles"),
            Self::Toolchanger if !(1..=UNITS).contains(&heads) => Err("tools need one labelled unit each, up to 128"),
            _ => Ok(()),
        }
    }
}

impl VirtualPrinter {
    /// Change the feed paths. Spools stay in the trays that keep their head and label.
    pub fn layout(&self, kind: Kind, heads: usize, trays_per_nozzle: usize, tpu: bool) -> Result<Self, String> {
        if self.source != Source::Generated { return Err("A twin uses its machine preset's layout".into()); }
        kind.check_heads(heads)?;
        let mut places = Vec::new();
        match kind {
            Kind::Toolchanger => {
                for head in 0..heads { places.push((head, format!("{}1", unit_letters(head)), SlotPath::Own)); }
            }
            Kind::Nozzles => {
                let units = trays_per_nozzle.div_ceil(4);
                if units > UNITS / heads { return Err("the trays need more than 128 labelled units".into()); }
                for head in 0..heads {
                    for tray in 0..trays_per_nozzle {
                        places.push((head, format!("{}{}", unit_letters(head * units + tray / 4), tray % 4 + 1), SlotPath::Shared));
                    }
                }
                if tpu { places.push((heads - 1, "Ext".into(), SlotPath::Dedicated)); }
            }
        }
        if places.is_empty() { return Err("a printer needs at least one slot".into()); }
        Ok(self.with_slots(kind, heads, places))
    }

    fn with_slots(&self, kind: Kind, heads: usize, places: Vec<(usize, String, SlotPath)>) -> Self {
        let old: HashMap<_, _> = self.slots.iter().map(|s| ((s.head, s.tray.as_str()), s)).collect();
        // Reserve every surviving colour, even when its tray follows the new ones.
        let mut counts = HashMap::<u32, usize>::new();
        for (head, tray, path) in &places {
            let colour = match old.get(&(*head, tray.as_str())) {
                Some(slot) => slot.colour,
                None if *path == SlotPath::Dedicated => Some(BLACK),
                None => None,
            };
            if let Some(c) = colour { *counts.entry(c).or_default() += 1; }
        }
        let slots = places.into_iter().map(|(head, tray, path)| {
            let (material, colour) = if let Some(slot) = old.get(&(head, tray.as_str())) {
                (slot.material.clone(), slot.colour)
            } else if path == SlotPath::Dedicated {
                (Some("TPU".into()), Some(BLACK))
            } else {
                // Once every swatch is in use, pick the least repeated one.
                let colour = *PALETTE.iter().min_by_key(|c| counts.get(*c).copied().unwrap_or(0)).unwrap();
                *counts.entry(colour).or_default() += 1;
                (Some("PLA".into()), Some(colour))
            };
            Slot { head, tray, path, material, colour }
        }).collect();
        Self { kind, heads, slots, ..self.clone() }
    }

    pub fn fab_material_slots(&self) -> Vec<String> {
        self.slots.iter().map(|s| {
            let suffix = match s.path { SlotPath::Shared => "", SlotPath::Own => ":own", SlotPath::Dedicated => ":dedicated" };
            format!("{}:{}{suffix}", s.head + 1, s.tray)
        }).collect()
    }

    /// The catalogue supplies the model and feed paths. Seed the stock of matching trays
    /// from the retained loadout, including trays the person deliberately left empty.
    pub fn twin(name: String, preset: &str, model: &str, heads: usize, places: Vec<(usize, String, SlotPath)>, stock: Vec<Slot>) -> Result<Self, String> {
        let family = family::of_model(model).filter(|_| !model.is_empty()).ok_or(family::UNSUPPORTED)?;
        let kind = match family.spools { family::Spools::Units => Kind::Nozzles, family::Spools::OnePerHead => Kind::Toolchanger };
        kind.check_heads(heads)?;
        let printer = Self {
            name, title: format!("{preset} (virtual)"), kind, heads, slots: stock, configured_for: None,
            source: Source::Twin { preset: preset.into(), model: model.into(), source: None },
        };
        Ok(printer.with_slots(kind, heads, places))
    }

    pub fn model(&self) -> String {
        if let Source::Twin { model, .. } = &self.source { return model.clone(); }
        let prefix = match self.kind { Kind::Nozzles => family::VIRTUAL_NOZZLES, Kind::Toolchanger => family::VIRTUAL_TOOLCHANGER };
        format!("{prefix} {}", self.name)
    }

    pub fn picker_title(&self) -> String {
        match self.source { Source::Generated => format!("{} (virtual)", self.title), Source::Twin { .. } => self.title.clone() }
    }

    pub fn machine_preset_name(&self) -> String {
        match &self.source { Source::Generated => self.picker_title(), Source::Twin { preset, .. } => preset.clone() }
    }
}

pub fn presets() -> Vec<VirtualPrinter> {
    let printer = |name: &str, title: &str| VirtualPrinter {
        name: name.into(), title: title.into(), source: Source::Generated, configured_for: None, kind: Kind::Nozzles, heads: 1, slots: Vec::new(),
    };
    let mut printers: Vec<_> = [
        ("two-materials", "Two materials", Kind::Nozzles, 1, 2, false),
        ("four-materials", "Four materials", Kind::Nozzles, 1, 4, false),
        ("nine-materials", "Nine materials", Kind::Nozzles, 1, 8, true),
        ("four-tools", "Four-tool toolchanger", Kind::Toolchanger, 4, 0, false),
    ].into_iter().map(|(name, title, kind, heads, trays, tpu)| {
        printer(name, title).layout(kind, heads, trays, tpu).expect("built-in printer layout")
    }).collect();
    let mut places = Vec::new();
    for (head, unit) in [(0, 'B'), (1, 'A')] {
        for tray in 1..=4 { places.push((head, format!("{unit}{tray}"), SlotPath::Shared)); }
        places.push((head, "Ext".into(), SlotPath::Own));
    }
    printers.push(printer("dual-nozzle", "Dual nozzle").with_slots(Kind::Nozzles, 2, places));
    printers
}

pub const PROCESS: &str = "Fab virtual 0.20 mm";
pub fn filament_preset(material: &str) -> String { format!("Fab virtual {material}") }

fn preset(kind: &str, name: String) -> RawPreset {
    RawPreset { kind: kind.into(), name, from: "system".into(), instantiation: true, ..Default::default() }
}

fn machine(printer: &VirtualPrinter) -> RawPreset {
    let mut m = preset("machine", printer.machine_preset_name());
    let v = &mut m.values;
    v.set_str("printer_model", printer.model());
    v.set_str("printer_technology", "FFF");
    let (flavor, start) = match printer.kind {
        Kind::Nozzles => ("marlin", "G90\nM83\nG28\nT[initial_tool]\nG1 Z1 F600\nM140 S50\nM104 S200\nM190 S50\nM109 S200\n"),
        Kind::Toolchanger => {
            v.set_str("thumbnails", "48x48/PNG");
            ("klipper", "G28\nG90\nM83\nT[initial_tool]\nG1 Z1 F600\nM140 S50\nM104 S200 T[initial_tool]\nM190 S50\nM109 S200 T[initial_tool]\n")
        }
    };
    v.set_str("gcode_flavor", flavor);
    v.set_str("machine_start_gcode", start);
    v.set_str("machine_end_gcode", "M104 S0\nM140 S0\nM107\n");
    if printer.slots.len() > 1 { v.set_str("change_filament_gcode", "T[next_extruder]\n"); }
    let bed = ["0x0", "350x0", "350x320", "0x320"];
    v.set_vec("printable_area", bed.map(String::from).to_vec());
    v.set_str("printable_height", "325");
    v.set_vec("fab_material_slots", printer.fab_material_slots());
    v.set_vec("extruder_printable_area", vec![bed.join(","); printer.heads]);
    for (key, value) in [("nozzle_diameter", "0.4"), ("extruder_printable_height", "325"), ("extruder_offset", "0x0")] {
        v.set_vec(key, vec![value.into(); printer.heads]);
    }
    // Fab's default motion limits accommodate the accelerations its process requests.
    for (key, value) in [
        ("machine_max_speed_x", "1000"), ("machine_max_speed_y", "1000"), ("machine_max_speed_z", "30"), ("machine_max_speed_e", "120"),
        ("machine_max_acceleration_x", "20000"), ("machine_max_acceleration_y", "20000"), ("machine_max_acceleration_z", "1500"), ("machine_max_acceleration_e", "5000"),
        ("machine_max_acceleration_extruding", "20000"), ("machine_max_acceleration_retracting", "5000"), ("machine_max_acceleration_travel", "9000"),
        ("machine_max_jerk_x", "9"), ("machine_max_jerk_y", "9"), ("machine_max_jerk_z", "3"), ("machine_max_jerk_e", "2.5"),
        // A parked virtual head retracts the same short length it primes on return.
        ("retraction_length", "0.5"), ("retract_length_toolchange", "0.5"), ("retract_lift_above", "0"),
    ] { v.set_vec(key, vec![value.into()]); }
    m
}

/// The printers and the process and filament presets all of them can use.
pub fn builtin_presets(printers: &[VirtualPrinter]) -> Vec<RawPreset> {
    let mut all: Vec<_> = printers.iter().filter(|p| p.source == Source::Generated).map(machine).collect();
    let names: Vec<_> = all.iter().map(|m| m.name.clone()).collect();
    let mut process = preset("process", PROCESS.into());
    process.values.set_vec("compatible_printers", names.clone());
    for (key, value) in [
        ("layer_height", "0.2"), ("initial_layer_print_height", "0.2"), ("wall_loops", "2"),
        ("top_shell_layers", "2"), ("bottom_shell_layers", "2"), ("sparse_infill_density", "10%"), ("enable_support", "0"),
        ("line_width", "0.42"), ("brim_type", "no_brim"), ("skirt_loops", "0"),
        ("outer_wall_speed", "150"), ("inner_wall_speed", "250"), ("sparse_infill_speed", "250"), ("internal_solid_infill_speed", "220"),
        ("top_surface_speed", "150"), ("gap_infill_speed", "150"), ("initial_layer_speed", "50"), ("initial_layer_infill_speed", "80"),
        ("travel_speed", "400"), ("default_acceleration", "8000"), ("outer_wall_acceleration", "4000"), ("travel_acceleration", "9000"),
    ] { process.values.set_str(key, value); }
    all.push(process);
    // A generated filament prints on every head of every generated printer. The option's own
    // default (3) allows only the first two nozzles, which refused a tool changer's third and
    // fourth heads. A bit past a machine's own nozzles is never read.
    let heads = printers.iter().filter(|p| p.source == Source::Generated).map(|p| p.heads).max().unwrap_or(1);
    let printable = (u64::MAX >> (64 - heads.clamp(1, 64))).to_string();
    // Material, density, nozzle temperature and range, flow limit, bed temperature.
    for (kind, density, nozzle, low, high, flow, bed) in [
        ("PLA", "1.24", "210", "190", "230", "18", "55"),
        ("PETG", "1.27", "240", "225", "255", "12", "75"),
        ("ABS", "1.04", "245", "230", "260", "12", "100"),
        ("ASA", "1.07", "250", "235", "265", "12", "100"),
        ("TPU", "1.21", "225", "210", "240", "3", "40"),
        ("PA", "1.14", "260", "245", "280", "8", "80"),
        ("PC", "1.20", "275", "260", "295", "8", "110"),
        ("PLA-S", "1.24", "210", "190", "230", "8", "55"),
        ("PVA", "1.19", "200", "185", "215", "6", "50"),
    ] {
        let mut f = preset("filament", filament_preset(kind));
        f.values.set_vec("compatible_printers", names.clone());
        for (key, value) in [
            ("filament_type", kind), ("filament_density", density), ("filament_diameter", "1.75"), ("filament_flow_ratio", "1"),
            ("filament_max_volumetric_speed", flow), ("filament_volume_type", "standard"), ("nozzle_temperature_range_low", low),
            ("nozzle_temperature_range_high", high), ("nozzle_temperature_initial_layer", nozzle), ("nozzle_temperature", nozzle),
            ("required_nozzle_HRC", "0"), ("chamber_temperatures", "0"), ("filament_printable", printable.as_str()),
        ] { f.values.set_vec(key, vec![value.into()]); }
        for (_, key) in PLATES {
            f.values.set_vec(key, vec![bed.into()]);
            f.values.set_vec(&format!("{key}_initial_layer"), vec![bed.into()]);
        }
        all.push(f);
    }
    all
}

pub fn path() -> PathBuf { crate::home::user_home().join(".makepad/fab/virtual_printers.txt") }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Saved {
    pub printers: Vec<VirtualPrinter>,
    pub picked: Option<String>,
    /// Separate from the record so a missing twin still keeps its printer disconnected.
    pub disconnected: BTreeSet<String>,
    /// Explicit machine choices, keyed by the configured printer id. Absence means automatic.
    pub bindings: BTreeMap<String, String>,
}

impl Default for Saved {
    fn default() -> Self { Self { printers: presets(), picked: None, disconnected: BTreeSet::new(), bindings: BTreeMap::new() } }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

// Return the unit a tray belongs to. Each HT unit holds one spool. External spools belong
// directly to a nozzle. Unit tray numbers are one-based and fit the link's zero-based u8.
fn tray_unit(label: &str) -> Result<Option<&str>, &'static str> {
    if label == "Ext" { return Ok(None); }
    let (letters, digits) = label.split_at(label.bytes().take_while(u8::is_ascii_uppercase).count());
    let number: usize = digits.parse().map_err(|_| "invalid tray label")?;
    if number.to_string() != digits { return Err("invalid tray number"); }
    if letters == "HT" && (1..=126).contains(&number) { return Ok(Some(label)); }
    let unit = letters.bytes().try_fold(0usize, |n, b| n.checked_mul(26)?.checked_add(usize::from(b - b'A') + 1));
    if !unit.is_some_and(|id| (1..=UNITS).contains(&id)) || !(1..=256).contains(&number) {
        return Err("invalid unit or tray number");
    }
    Ok(Some(letters))
}

pub fn valid_config(name: &str) -> bool {
    !name.trim().is_empty() && !name.contains(['\t', '\r', '\n'])
}

impl Saved {
    pub fn twin_for(&self, config: &str) -> Option<&VirtualPrinter> {
        self.printers.iter().find(|p| p.configured_for.as_deref() == Some(config))
    }

    pub fn to_text(&self) -> String {
        let mut text = String::new();
        if let Some(pick) = &self.picked { writeln!(text, "picked\t{pick}").unwrap(); }
        for config in &self.disconnected { writeln!(text, "disconnected\t{config}").unwrap(); }
        for (id, preset) in &self.bindings { writeln!(text, "binding\t{id}\t{preset}").unwrap(); }
        for printer in &self.printers {
            let kind = match printer.kind { Kind::Nozzles => "nozzles", Kind::Toolchanger => "toolchanger" };
            writeln!(text, "printer\t{}\t{}\t{kind}\t{}", printer.name, printer.title, printer.heads).unwrap();
            if let Source::Twin { preset, model, source } = &printer.source {
                write!(text, "twin\t{preset}\t{model}").unwrap();
                if let Some(id) = source { write!(text, "\t{id}").unwrap(); }
                text.push('\n');
            }
            if let Some(config) = &printer.configured_for { writeln!(text, "configured\t{config}").unwrap(); }
            for slot in &printer.slots {
                let path = match slot.path { SlotPath::Shared => "shared", SlotPath::Own => "own", SlotPath::Dedicated => "dedicated" };
                let colour = slot.colour.map_or_else(|| "-".to_string(), |c| format!("{c:06X}"));
                writeln!(text, "slot\t{}\t{}\t{path}\t{}\t{colour}", slot.head + 1, slot.tray, slot.material.as_deref().unwrap_or("-")).unwrap();
            }
        }
        text
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let mut saved = Self { printers: Vec::new(), picked: None, disconnected: BTreeSet::new(), bindings: BTreeMap::new() };
        let mut unit_heads = HashMap::new();
        let mut trays = std::collections::HashSet::new();
        let mut configs = HashSet::new();
        let mut titles = HashSet::new();
        let mut header = 0;
        let mut check_slots = |p: &VirtualPrinter, line| {
            if p.configured_for.is_none() && !titles.insert(p.title.clone()) { Err(format!("line {line}: duplicate printer title")) }
            else if p.slots.is_empty() { Err(format!("line {line}: a printer needs at least one slot")) }
            else if p.kind == Kind::Toolchanger && p.slots.len() != p.heads { Err(format!("line {line}: each tool needs its own spool")) }
            else { Ok(()) }
        };
        for (index, line) in text.lines().enumerate() {
            let line_no = index + 1;
            let bad = |why: &str| format!("line {line_no}: {why}");
            match line.split('\t').collect::<Vec<_>>().as_slice() {
                ["picked", value] => {
                    if saved.picked.is_some() { return Err(bad("printer picked twice")); }
                    if !value.strip_prefix("virtual:").is_some_and(valid_name)
                        && !value.strip_prefix("printer:").is_some_and(valid_config) {
                        return Err(bad("expected virtual:<name> or printer:<name>"));
                    }
                    saved.picked = Some((*value).into());
                }
                ["disconnected", config] => {
                    if !valid_config(config) || !saved.disconnected.insert((*config).into()) {
                        return Err(bad("invalid or repeated disconnected printer"));
                    }
                }
                ["binding", id, preset] => {
                    if !id.strip_prefix("printer:").is_some_and(valid_config) || !valid_config(preset)
                        || saved.bindings.insert((*id).into(), (*preset).into()).is_some() {
                        return Err(bad("invalid or repeated machine binding"));
                    }
                }
                ["configured", config] => {
                    let p = saved.printers.last_mut().ok_or_else(|| bad("configured before twin"))?;
                    if p.source == Source::Generated || p.configured_for.is_some() || !valid_config(config) || !configs.insert(*config) {
                        return Err(bad("expected one configured printer per twin"));
                    }
                    p.configured_for = Some((*config).into());
                }
                ["printer", name, title, kind, heads] => {
                    if let Some(last) = saved.printers.last() { check_slots(last, header)?; }
                    if !valid_name(name) || title.trim().is_empty() { return Err(bad("invalid printer name or title")); }
                    if saved.printers.iter().any(|p| p.name == *name) { return Err(bad("duplicate printer name")); }
                    let kind = match *kind {
                        "nozzles" => Kind::Nozzles, "toolchanger" => Kind::Toolchanger, _ => return Err(bad("unknown printer kind")),
                    };
                    let heads = heads.parse().map_err(|_| bad("invalid head count"))?;
                    kind.check_heads(heads).map_err(bad)?;
                    saved.printers.push(VirtualPrinter { name: (*name).into(), title: (*title).into(), source: Source::Generated, configured_for: None, kind, heads, slots: Vec::new() });
                    header = line_no;
                    unit_heads.clear();
                    trays.clear();
                }
                ["twin", preset, model, origin @ ..] => {
                    let printer = saved.printers.last_mut().ok_or_else(|| bad("twin before printer"))?;
                    if preset.trim().is_empty() || model.trim().is_empty() || printer.source != Source::Generated {
                        return Err(bad("expected one machine preset and model for the twin"));
                    }
                    let source = match origin {
                        [] => None,
                        [id] if id.strip_prefix("printer:").is_some_and(valid_config)
                            || id.strip_prefix("virtual:").is_some_and(valid_name) => Some((*id).into()),
                        _ => return Err(bad("invalid twin source")),
                    };
                    printer.source = Source::Twin { preset: (*preset).into(), model: (*model).into(), source };
                }
                ["slot", head, tray, path, material, colour] => {
                    let printer = saved.printers.last_mut().ok_or_else(|| bad("slot before printer"))?;
                    let head = head.parse::<usize>().ok().filter(|n| (1..=printer.heads).contains(n)).ok_or_else(|| bad("invalid head number"))? - 1;
                    let path = match *path {
                        "shared" => SlotPath::Shared, "own" => SlotPath::Own, "dedicated" => SlotPath::Dedicated, _ => return Err(bad("unknown slot path")),
                    };
                    if printer.kind == Kind::Toolchanger && (path != SlotPath::Own || *tray != format!("{}1", unit_letters(head))) {
                        return Err(bad("a tool needs its own unit with one tray"));
                    }
                    if let Some(unit) = tray_unit(tray).map_err(bad)? {
                        if unit_heads.insert(unit, head).is_some_and(|h| h != head) { return Err(bad("a unit feeds only one nozzle")); }
                    } else if path == SlotPath::Shared { return Err(bad("an external spool needs its own or a dedicated path")); }
                    if !trays.insert((head, *tray)) { return Err(bad("duplicate slot")); }
                    if material.trim().is_empty() { return Err(bad("use - for an empty tray")); }
                    let colour = match *colour {
                        "-" => None,
                        c if c.len() == 6 && c.bytes().all(|b| b.is_ascii_hexdigit()) => Some(u32::from_str_radix(c, 16).unwrap()),
                        _ => return Err(bad("expected RRGGBB or -")),
                    };
                    printer.slots.push(Slot { head, tray: (*tray).into(), path, material: (*material != "-").then(|| (*material).into()), colour });
                }
                _ => return Err(bad("unknown record or wrong field count")),
            }
        }
        if let Some(last) = saved.printers.last() { check_slots(last, header)?; }
        Ok(saved)
    }
}

pub fn load_from(path: &Path) -> Result<Saved, String> {
    let mut text = fs::read_to_string(path);
    if text.as_ref().is_err_and(|e| e.kind() == io::ErrorKind::NotFound) {
        // Recovery stages the safe modes before moving the unreadable file. A launch
        // between the move and the final rename can finish installing those modes.
        let pending = recovery_path(path);
        match fs::hard_link(&pending, path) {
            Ok(()) => { let _ = fs::remove_file(pending); }
            Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::AlreadyExists) => {}
            Err(e) => return Err(e.to_string()),
        }
        text = fs::read_to_string(path);
    }
    match text {
        Ok(text) => Saved::parse(&text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Saved::default()),
        Err(e) => Err(e.to_string()),
    }
}

pub fn recovery_path(path: &Path) -> PathBuf { path.with_added_extension("recovering") }

/// Keep the unreadable bytes under the first free name before removing the original.
pub fn set_aside(path: &Path) -> io::Result<PathBuf> {
    let mut suffix = 1u64;
    let mut kept = path.with_added_extension("unreadable");
    loop {
        match fs::hard_link(path, &kept) {
            Ok(()) => {
                fs::remove_file(path)?;
                return Ok(kept);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                suffix += 1;
                kept = path.with_added_extension(format!("unreadable.{suffix}"));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Replace the whole file together so a failed save leaves the previous layout readable.
pub fn save_to(path: &Path, saved: &Saved) -> io::Result<()> {
    let text = saved.to_text();
    if Saved::parse(&text).map_err(io::Error::other)? != *saved {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "a printer field contains a record separator"));
    }
    if let Some(parent) = path.parent() { fs::create_dir_all(parent)?; }
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let (tmp, mut file) = loop {
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = path.with_added_extension(format!("{}.{n}.tmp", std::process::id()));
        match fs::File::create_new(&tmp) {
            Ok(file) => break (tmp, file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    let written = file.write_all(text.as_bytes());
    drop(file);
    let result = written.and_then(|()| fs::rename(&tmp, path));
    if result.is_err() { let _ = fs::remove_file(tmp); }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::ProfileStore;

    #[test]
    fn set_aside_preserves_each_unreadable_file() {
        let dir = std::env::temp_dir().join(format!("fab-set-aside-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("virtual_printers.txt");
        let first = path.with_added_extension("unreadable");
        let second = path.with_added_extension("unreadable.2");
        for (bytes, expected) in [(b"\xff\r\n", &first), (b"\x00\n!", &second)] {
            fs::write(&path, bytes).unwrap();
            assert_eq!(&set_aside(&path).unwrap(), expected, "use the first free backup name");
            assert!(!path.try_exists().unwrap(), "remove the original after keeping it");
        }
        assert_eq!(fs::read(first).unwrap(), b"\xff\r\n", "preserve the first file byte for byte");
        assert_eq!(fs::read(second).unwrap(), b"\x00\n!", "preserve the second file byte for byte");
        assert_eq!(load_from(&path).unwrap(), Saved::default(), "old backups alone do not change a first run");
        let fresh = Saved { picked: Some("printer:No printer".into()), disconnected: ["fixture".into()].into(), ..Saved::default() };
        save_to(&recovery_path(&path), &fresh).unwrap();
        assert_eq!(load_from(&path).unwrap(), fresh, "resume an interrupted recovery before treating the file as missing");
        assert!(!recovery_path(&path).try_exists().unwrap(), "consume the staged replacement");
        assert_eq!(load_from(&path).unwrap(), fresh, "subsequent launches read the installed modes");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn preset_slots_and_profiles() {
        let printers = presets();
        let names: Vec<_> = printers.iter().map(VirtualPrinter::machine_preset_name).collect();
        let store = ProfileStore::open(&[], &[]).with_builtin(builtin_presets(&printers));
        let process = store.resolve(PROCESS).unwrap().values;
        assert_eq!(process.get("compatible_printers").unwrap().as_vec(), names);
        let slots = [
            "1:A1 1:A2", "1:A1 1:A2 1:A3 1:A4",
            "1:A1 1:A2 1:A3 1:A4 1:B1 1:B2 1:B3 1:B4 1:Ext:dedicated",
            "1:A1:own 2:B1:own 3:C1:own 4:D1:own",
            "1:B1 1:B2 1:B3 1:B4 1:Ext:own 2:A1 2:A2 2:A3 2:A4 2:Ext:own",
        ];
        assert_eq!(printers.len(), slots.len());
        for (printer, slots) in printers.iter().zip(slots) {
            assert_eq!(printer.fab_material_slots(), slots.split_whitespace().collect::<Vec<_>>());
            let mut colours = std::collections::HashSet::new();
            for s in &printer.slots {
                assert!(colours.insert(s.colour.unwrap()), "distinct colours in {}", printer.name);
                if s.path == SlotPath::Dedicated {
                    assert_eq!((s.material.as_deref(), s.colour), (Some("TPU"), Some(BLACK)));
                } else { assert_eq!(s.material.as_deref(), Some("PLA")); }
            }
            let m = store.resolve(&printer.machine_preset_name()).unwrap().values;
            assert!(m.f64("machine_max_acceleration_travel") >= process.f64("travel_acceleration"));
            assert!(m.f64("machine_max_acceleration_extruding") >= process.f64("default_acceleration"));
            assert_eq!(m.get("nozzle_diameter").unwrap().as_vec(), vec!["0.4"; printer.heads]);
            assert_eq!(m.get("fab_material_slots").unwrap().as_vec(), printer.fab_material_slots());
            let family = family::of_profile(&m).unwrap();
            let flavor = if printer.kind == Kind::Nozzles { family::Flavor::Mask } else { family::Flavor::Named };
            assert_eq!((family.link, family.flavor), (family::Link::Virtual, flavor));
            assert_eq!(m.str("printer_model"), printer.model());
        }
        for material in ["PLA", "PETG", "ABS", "ASA", "TPU", "PA", "PC", "PLA-S", "PVA"] {
            let f = store.resolve(&filament_preset(material)).unwrap().values;
            assert_eq!(f.str("filament_type"), material);
            assert_eq!(f.get("compatible_printers").unwrap().as_vec(), names);
            for (_, key) in PLATES {
                assert!(f.f64(key) > 0.0);
                assert_eq!(f.str(key), f.str(&format!("{key}_initial_layer")));
            }
        }
    }

    #[test]
    fn changing_layout_preserves_spools_and_empty_trays() {
        let mut p = presets().remove(0);
        p.slots[0].material = None;
        p.slots[0].colour = None;
        p.slots[1].material = Some("ASA".into());
        p.slots[1].colour = Some(0xc12e1f);
        let changed = p.layout(Kind::Nozzles, 2, 5, true).unwrap();
        assert_eq!(changed.slots[..2], p.slots);
        assert_eq!(changed.fab_material_slots().join(" "), "1:A1 1:A2 1:A3 1:A4 1:B1 2:C1 2:C2 2:C3 2:C4 2:D1 2:Ext:dedicated");
        assert_eq!(changed.slots.last().unwrap().material.as_deref(), Some("TPU"));
        assert!(p.layout(Kind::Nozzles, 3, 1, false).is_err());
        assert!(p.layout(Kind::Nozzles, 1, 0, false).is_err());
        assert_eq!(p.layout(Kind::Toolchanger, 9, usize::MAX, true).unwrap().slots[8].tray, "I1");
        assert_eq!(p.layout(Kind::Toolchanger, UNITS, 0, false).unwrap().slots[127].tray, "DX1");
        assert!(p.layout(Kind::Toolchanger, UNITS + 1, 0, false).is_err());
        assert!(p.layout(Kind::Nozzles, 2, usize::MAX, true).is_err());
    }

    #[test]
    fn saved_printers_round_trip_and_identify_bad_records() {
        let dir = std::env::temp_dir().join(format!("fab-virtual-settings-{}", std::process::id()));
        let path = dir.join("printers.txt");
        let mut saved = Saved::default();
        assert_eq!(load_from(&path).unwrap(), saved);
        saved.picked = Some("printer:Workshop".into());
        saved.printers[0].slots[0].material = None;
        saved.printers[0].slots[0].colour = None;
        let text = saved.to_text();
        assert!(text.contains("slot\t1\tA1\tshared\t-\t-\n"));
        assert_eq!(Saved::parse(&text).unwrap(), saved);
        assert_eq!(Saved::parse(&text).unwrap().to_text(), text);
        assert!(Saved::parse(&text.replacen("\tA1\t", "\t?\t", 1)).unwrap_err().contains("line 3:"));
        let legacy = "printer\told\tOriginal\tnozzles\t1\nslot\t1\tA1\tshared\tPLA\tFFFFFF\n";
        let original = Saved::parse(legacy).unwrap();
        assert_eq!(original.printers[0].source, Source::Generated);
        assert_eq!(original.to_text(), legacy);
        assert!(original.bindings.is_empty(), "old settings leave every binding automatic");

        let model = family::FAMILIES[1].models[0];
        let stock = vec![Slot { head: 0, tray: "A1".into(), path: SlotPath::Own, material: None, colour: None }];
        let twin = VirtualPrinter::twin("twin-1".into(), "Fixture 0.6", model, 2, vec![
            (0, "A1".into(), SlotPath::Own), (1, "B1".into(), SlotPath::Own)
        ], stock).unwrap();
        assert_eq!(twin.kind, Kind::Toolchanger);
        assert_eq!(twin.machine_preset_name(), "Fixture 0.6");
        assert_eq!(twin.model(), model);
        assert_eq!(twin.slots[0].material, None);
        assert_eq!(twin.slots[1].material.as_deref(), Some("PLA"));
        assert!(builtin_presets(std::slice::from_ref(&twin)).iter().all(|p| p.kind != "machine"));
        let mut configured = twin.clone();
        configured.name = "configured-twin".into();
        configured.configured_for = Some("Fixture".into());
        saved.printers.push(configured.clone());
        saved.disconnected.insert("Fixture".into());
        saved.printers.push(twin);
        assert_eq!(Saved::parse(&saved.to_text()).unwrap(), saved);
        saved.disconnected.clear();
        assert_eq!(Saved::parse(&saved.to_text()).unwrap().twin_for("Fixture"), Some(&configured));
        for name in ["", "a\tb", "a\rb", "a\nb"] {
            assert!(!valid_config(name));
            assert!(Saved::parse(&format!("disconnected\t{name}\n")).is_err());
            assert!(Saved::parse(&saved.to_text().replace("configured\tFixture\n", &format!("configured\t{name}\n"))).is_err());
        }
        assert!(Saved::parse("disconnected\tFixture\ndisconnected\tFixture\n").is_err());
        assert!(Saved::parse(&format!("{legacy}configured\tFixture\n")).is_err());
        assert!(!saved.to_text().contains("binding\t"), "automatic choices write no binding record");
        saved.bindings.insert("printer:P".into(), "Missing profile".into());
        let old_twin = saved.printers.last().unwrap().clone();
        assert!(matches!(old_twin.source, Source::Twin { source: None, .. }), "old twin records have no origin");
        for name in ["P", "Q"] {
            let mut twin = old_twin.clone();
            twin.name = format!("twin-{}", name.to_lowercase());
            twin.title = format!("{} ({name})", twin.title);
            if let Source::Twin { source, .. } = &mut twin.source { *source = Some(format!("printer:{name}")); }
            saved.printers.push(twin);
        }
        let read = Saved::parse(&saved.to_text()).unwrap();
        assert_eq!(read.bindings.get("printer:P").map(String::as_str), Some("Missing profile"), "the exact explicit pick survives an absent preset");
        assert!(!read.bindings.contains_key("printer:Q"), "another printer remains automatic");
        let twins = &read.printers[read.printers.len() - 2..];
        assert_ne!(twins[0].source, twins[1].source, "same-preset twins of P and Q have distinct origins");
        assert_eq!(read, saved, "bindings and both twin origins round-trip");
        save_to(&path, &saved).unwrap();
        saved.picked = Some("virtual:twin-1".into());
        save_to(&path, &saved).unwrap();
        assert_eq!(load_from(&path).unwrap(), saved);
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
