//! The print's colours on the printer: each print colour ([`fab_plate::PrintColor`]) maps to
//! one printer position (a toolhead and a tray of a unit on it), and that position is the
//! job's material: its nozzle, its type and its per-material settings. There is no other
//! layer: the job's tools are the distinct positions the used colours map to, and a tool's
//! nozzle is its position's head, so a colour cannot print on another nozzle than its tray's.
//!
//! [`layout`] reads the trays from the linked printer's reports (each unit says which nozzle
//! it feeds); with no reports, from the last loadout it reported (kept on disk, editable by
//! hand), else from the machine's layout (by default, the dual-nozzle printer with a unit
//! and an external spool per toolhead, states unknown). [`resolve`] maps every colour: the saved
//! position for this printer, else one saved for another printer when that tray exists here, else an
//! automatic pick (the wanted material type first, then a nozzle of its own while one is left,
//! then the nearest colour, a tie between two types left to the person). Where each nozzle
//! gets at most one used colour, the print changes heads only: no swap on a nozzle, no flush,
//! no tower. A colour whose spool looks as right on two nozzles prints on the one that reaches
//! its parts ([`reaching`]).
//! Runs on publications and edits, never per frame: O(colours * trays + parts * nozzles).

use fab_plate::machine::{nozzle_word, unit_letters, unit_tray, Machine, Slot, SlotPath};
use fab_plate::{flexible, PrintColor};
use fab_printer_link::model::{CloudNozzleId, PrinterState, TrayId};

/// A printer position: a toolhead and the tray on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    /// The machine's nozzle index, also the job's extruder (0-based).
    pub head: usize,
    pub tray: TrayId,
}

impl Position {
    /// The tray as the printer labels it: "A2" (unit A, tray 2), "HT1", "Ext".
    pub fn tray_text(&self) -> String {
        tray_text(self.tray)
    }

    /// The head's mark and tray, or just the tray for a single toolhead.
    pub fn label(&self, heads: &[String]) -> String {
        if heads.len() == 1 { self.tray_text() } else { format!("{} · {}", head_mark(&heads[self.head]), self.tray_text()) }
    }

    /// From a saved toolhead word and tray label. The dual-nozzle printer's link numbers its
    /// external spools 254 and 255; further toolheads' spools are told apart by their head.
    pub fn parse(heads: &[String], word: &str, tray: &str) -> Option<Position> {
        let word = nozzle_word(word);
        let head = heads.iter().position(|name| nozzle_word(name) == word)?;
        let t = tray.trim();
        let tray = if t.eq_ignore_ascii_case("ext") {
            let id = if head == 1 { 255 } else { 254 };
            TrayId { ams: id, slot: id }
        } else if let Some(n) = t.strip_prefix("HT").and_then(|n| n.parse::<u8>().ok()).filter(|n| (1..=126).contains(n)) {
            TrayId { ams: 127 + n, slot: 0 }
        } else {
            // Within the units and trays a label numbers, so both fit the link's numbering.
            let (unit, tray) = unit_tray(t)?;
            TrayId { ams: unit as u8, slot: (tray - 1) as u8 }
        };
        Some(Position { head, tray })
    }
}

/// The toolhead word saved with positions and loadouts: the head's name as a script name
/// ("left", "tool_1").
pub fn head_word(heads: &[String], i: usize) -> String {
    nozzle_word(&heads[i])
}

/// The initial, uppercased, followed by any trailing ASCII digits.
pub fn head_mark(name: &str) -> String {
    let mut mark = name.chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default();
    let digits = name.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    mark.push_str(&name[digits..]);
    mark
}

pub fn tray_text(t: TrayId) -> String {
    match t.ams {
        254 | 255 => "Ext".into(),
        a @ 128..=253 => format!("HT{}", a - 127),
        a => format!("{}{}", unit_letters(a as usize), t.slot as usize + 1),
    }
}

/// A unit as the printer labels it: "A", "E", "AA", "HT1".
pub fn unit_label(ams: u8) -> String {
    match ams {
        a @ 128..=253 => format!("HT{}", a - 127),
        a => unit_letters(a as usize),
    }
}

/// What the printer says is in a tray.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayState {
    Loaded,
    Empty,
    /// No report (no printer linked, or the printer has not said).
    Unknown,
}

/// One tray as the mapping shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct TrayView {
    pub pos: Position,
    /// "AMS A", "AMS HT1", "External".
    pub unit: String,
    /// The material type ("PLA", "PETG", "PLA-S").
    pub material: Option<String>,
    /// The material's full name (the profile it matches), when known.
    pub name: Option<String>,
    /// Its colour (linear RGBA).
    pub colour: Option<[f32; 4]>,
    pub state: TrayState,
    /// Percent left.
    pub remain: Option<f64>,
    /// The nozzle temperature range the spool asks for.
    pub temps: Option<(f64, f64)>,
    /// How its filament reaches the nozzle: the printer profile's slot ([`layout`]).
    pub path: SlotPath,
}

impl TrayView {
    /// Its filament feeds the nozzle on a path of its own, not through a shared unit: the path a
    /// flexible filament takes (an external spool's, or a dedicated one).
    pub fn own_path(&self) -> bool {
        self.path != SlotPath::Shared
    }

    /// Its path shares nothing with the other slots: a change into or out of its filament
    /// purges nothing.
    pub fn dedicated(&self) -> bool {
        self.path == SlotPath::Dedicated
    }

    /// A short type label for chips: "PLA", "PETG", "SUP" for support materials.
    pub fn type_label(&self) -> String {
        match self.material.as_deref().map(str::trim) {
            None | Some("") => "?".into(),
            Some(m) if m.to_ascii_uppercase().contains("-S") || m.to_ascii_uppercase().starts_with("SUP") || m.eq_ignore_ascii_case("PVA") => "SUP".into(),
            Some(m) => m.split(['-', ' ']).next().unwrap_or(m).to_ascii_uppercase(),
        }
    }
}

/// The printer's trays, by nozzle: what the mapping can pick from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    /// Toolhead names in machine nozzle order.
    pub heads: Vec<String>,
    /// The printer the positions are saved for (the default dual-nozzle printer without one).
    pub printer: String,
    /// The trays come from the printer's reports.
    pub live: bool,
    /// Not live: the trays are the last loadout the printer reported, or one set by hand
    /// (else the machine's layout, contents unknown).
    pub known: bool,
    pub trays: Vec<TrayView>,
}

/// A hand edit of a tray of the offline loadout.
#[derive(Clone, Debug, PartialEq)]
pub enum TrayEdit {
    /// The material type it holds ("PLA").
    Material(String),
    /// Its colour (linear RGBA).
    Colour([f32; 4]),
    /// Nothing loaded.
    Empty,
}

/// Material types a tray can be set to by hand.
pub const MATERIALS: &[&str] = &["PLA", "PETG", "ABS", "ASA", "TPU", "PA", "PC", "PLA-S", "PVA"];

/// Filament colours a tray can be set to by hand (name, sRGB).
pub const FILAMENT_COLOURS: &[(&str, u32)] = &[
    ("White", 0xf4f4f0),
    ("Black", 0x161616),
    ("Grey", 0x8e9089),
    ("Silver", 0xa6a9aa),
    ("Red", 0xc12e1f),
    ("Orange", 0xf0742c),
    ("Yellow", 0xf4d03a),
    ("Green", 0x2e9b4b),
    ("Teal", 0x2a9d8f),
    ("Blue", 0x1f5fd0),
    ("Purple", 0x6c3fa6),
    ("Pink", 0xe78fb0),
    ("Brown", 0x7a5232),
    ("Beige", 0xd9c7a1),
];

/// A `0xRRGGBB` sRGB colour, linear.
pub fn linear_of(srgb: u32) -> [f32; 4] {
    let f = |s: u32| {
        let v = ((srgb >> s) & 0xff) as f32 / 255.0;
        if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    [f(16), f(8), f(0), 1.0]
}

/// Apply a hand edit to the tray at `pos`; false when the layout has no such tray.
pub fn edit_tray(layout: &mut Layout, pos: Position, edit: &TrayEdit) -> bool {
    let Some(t) = layout.trays.iter_mut().find(|t| t.pos == pos) else { return false };
    match edit {
        TrayEdit::Material(m) => {
            t.material = Some(m.clone());
            t.name = None;
            t.state = TrayState::Loaded;
        }
        TrayEdit::Colour(c) => {
            t.colour = Some(*c);
            t.state = TrayState::Loaded;
        }
        TrayEdit::Empty => {
            t.material = None;
            t.name = None;
            t.colour = None;
            t.state = TrayState::Empty;
        }
    }
    t.remain = None;
    t.temps = None;
    layout.known = true;
    true
}

/// The same trays holding the same (what is left aside): a loadout worth saving again when not.
pub fn same_loadout(a: &Layout, b: &Layout) -> bool {
    a.printer == b.printer
        && a.trays.len() == b.trays.len()
        && a.trays.iter().zip(&b.trays).all(|(x, y)| x.pos == y.pos && x.unit == y.unit && x.material == y.material && x.colour == y.colour && x.state == y.state)
}

/// The loadout as saved: the printer's name, then one tray per line (toolhead word, tray, unit, state,
/// material, linear colour), tab separated.
pub fn loadout_text(l: &Layout) -> String {
    let mut out = format!("printer\t{}\n", l.printer.replace(['\t', '\n'], " "));
    for t in &l.trays {
        let state = match t.state { TrayState::Loaded => "loaded", TrayState::Empty => "empty", TrayState::Unknown => "unknown" };
        let colour = t.colour.map(|c| format!("{:.5} {:.5} {:.5}", c[0], c[1], c[2])).unwrap_or_else(|| "-".into());
        out.push_str(&format!("{}\t{}\t{}\t{state}\t{}\t{colour}\n", head_word(&l.heads, t.pos.head), t.pos.tray_text(), t.unit.replace(['\t', '\n'], " "), t.material.as_deref().unwrap_or("-").replace(['\t', '\n'], " ")));
    }
    out
}

/// A saved loadout ([`loadout_text`]); `None` when it is not one. Its trays are known, not live.
pub fn parse_loadout(text: &str, heads: &[String]) -> Option<Layout> {
    let mut lines = text.lines();
    let printer = lines.next()?.strip_prefix("printer\t")?.to_string();
    let mut trays = Vec::new();
    for line in lines.filter(|l| !l.trim().is_empty()) {
        let f: Vec<&str> = line.split('\t').collect();
        let [word, tray, unit, state, material, colour] = f.as_slice() else { return None };
        let pos = Position::parse(heads, word, tray)?;
        let state = match *state { "loaded" => TrayState::Loaded, "empty" => TrayState::Empty, "unknown" => TrayState::Unknown, _ => return None };
        let colour = match *colour {
            "-" => None,
            c => {
                let v: Vec<f32> = c.split(' ').map(|x| x.parse().ok()).collect::<Option<_>>()?;
                let [r, g, b] = v.as_slice() else { return None };
                Some([*r, *g, *b, 1.0])
            }
        };
        let material = (*material != "-").then(|| material.to_string());
        trays.push(TrayView { pos, unit: unit.to_string(), material, name: None, colour, state, remain: None, temps: None, path: SlotPath::Shared });
    }
    Some(Layout { printer, heads: heads.to_vec(), live: false, known: true, trays })
}

/// The name positions are saved under when no printer is linked (the dual-nozzle printer's).
pub const OFFLINE_PRINTER: &str = "H2D";

/// The printer the mapping falls back on with no reports and no known loadout: the name
/// positions are saved under, and the material slots its profile names.
#[derive(Clone, Debug, PartialEq)]
pub struct Offline {
    pub heads: Vec<String>,
    pub printer: String,
    pub slots: Vec<Slot>,
}

impl Offline {
    /// A printer's: its slots, saved under [`OFFLINE_PRINTER`] when they are the dual-nozzle
    /// printer's own, else under its name.
    pub fn of(machine: &Machine) -> Offline {
        let printer = if machine.slots == Machine::dual_nozzle_slots() { OFFLINE_PRINTER.to_string() } else { machine.name.clone() };
        Offline { printer, heads: machine.nozzles.iter().map(|n| n.name.clone()).collect(), slots: machine.slots.clone() }
    }
}

/// A slot's position: its toolhead and its tray.
fn slot_position(s: &Slot, heads: &[String]) -> Option<Position> {
    Position::parse(heads, heads.get(s.nozzle)?, &s.tray)
}

/// How the filament in the tray at `pos` reaches the nozzle: as the profile's slot there says,
/// else an external spool's own path, else through its unit.
fn path_at(pos: Position, slots: &[Slot], heads: &[String]) -> SlotPath {
    match slots.iter().find(|s| slot_position(s, heads) == Some(pos)) {
        Some(s) => s.path,
        None if matches!(pos.tray.ams, 254 | 255) => SlotPath::Own,
        None => SlotPath::Shared,
    }
}

/// A unit's name as the mapping shows it: "AMS A", "AMS HT1", "External".
fn unit_name(t: TrayId) -> String {
    match t.ams {
        254 | 255 => "External".into(),
        a @ 128..=253 => format!("AMS HT{}", a - 127),
        a => format!("AMS {}", unit_label(a)),
    }
}

impl Layout {
    pub fn tray(&self, pos: Position) -> Option<&TrayView> {
        self.trays.iter().find(|t| t.pos == pos)
    }

    pub fn on(&self, head: usize) -> impl Iterator<Item = &TrayView> {
        self.trays.iter().filter(move |t| t.pos.head == head)
    }
}

/// The trays the mapping picks from: the linked printer's reports; without them `known` (the
/// last loadout it reported, or one set by hand); else the printer profile's slots (`offline`;
/// by default, the dual-nozzle printer's units and an external spool on each toolhead), all
/// unknown. Each tray's path is its slot's in the profile. `name_of` gives a tray's full
/// material name from its profile id.
pub fn layout(printer: Option<(&str, &PrinterState)>, known: Option<&Layout>, offline: &Offline, name_of: &dyn Fn(&str) -> Option<String>) -> Layout {
    let mut l = trays_of(printer, known, offline, name_of);
    for t in &mut l.trays {
        t.path = path_at(t.pos, &offline.slots, &offline.heads);
    }
    l
}

/// Leave out the trays on a toolhead the layout's printer profile does not have (a report from
/// a printer with more toolheads): every position is then one of its heads. Whether any went.
pub fn drop_missing_heads(l: &mut Layout) -> bool {
    let n = l.trays.len();
    l.trays.retain(|t| t.pos.head < l.heads.len());
    l.trays.len() != n
}

/// Map a unit's reported head, or the first family's external spools, to machine indices.
fn report_head(state: &PrinterState, extruder: Option<CloudNozzleId>, tray: Option<TrayId>) -> Option<usize> {
    extruder.and_then(|id| state.head_of(id)).or_else(|| {
        if state.numbered_heads { return None; }
        match tray.map(|t| t.ams) { Some(254) => Some(0), Some(255) => Some(1), _ => None }
    })
}

fn trays_of(printer: Option<(&str, &PrinterState)>, known: Option<&Layout>, offline: &Offline, name_of: &dyn Fn(&str) -> Option<String>) -> Layout {
    let Some((printer, state)) = printer.filter(|(_, s)| !s.ams.is_empty() || !s.external.is_empty()) else {
        if let Some(k) = known {
            return Layout { heads: offline.heads.clone(), live: false, known: true, ..k.clone() };
        }
        let trays = offline.slots.iter().filter_map(|s| {
            let pos = slot_position(s, &offline.heads)?;
            Some(TrayView { pos, unit: unit_name(pos.tray), material: None, name: None, colour: None, state: TrayState::Unknown, remain: None, temps: None, path: s.path })
        }).collect();
        return Layout { printer: printer.map_or(offline.printer.as_str(), |(n, _)| n).to_string(), heads: offline.heads.clone(), live: false, known: false, trays };
    };
    let numbered_heads = state.numbered_heads;
    let view = |head: usize, unit: String, t: &fab_printer_link::model::Tray| -> Option<TrayView> {
        let id = t.id?;
        let known = t.material.as_deref().is_some_and(|m| !m.trim().is_empty());
        let state = match t.present {
            Some(false) => TrayState::Empty,
            Some(true) if known || t.colour.is_some() => TrayState::Loaded,
            Some(true) => TrayState::Unknown,
            None if known => TrayState::Loaded,
            None => TrayState::Unknown,
        };
        let loaded = state != TrayState::Empty;
        // An external spool has no tag to read what is left from: it reports 0.
        let external = matches!(id.ams, 254 | 255);
        Some(TrayView {
            pos: Position { head, tray: id },
            unit,
            material: t.material.clone().filter(|m| loaded && !m.trim().is_empty()),
            name: t.info_idx.as_deref().filter(|_| loaded).and_then(name_of),
            colour: t.colour.filter(|_| loaded).map(|c| c.linear()),
            state,
            remain: t.remain.filter(|r| loaded && !external && !numbered_heads && *r >= 0.0),
            temps: t.nozzle_temp_min.zip(t.nozzle_temp_max).filter(|_| loaded),
            path: SlotPath::Shared,
        })
    };
    let mut trays = Vec::new();
    for unit in &state.ams {
        // A unit that does not say which nozzle it feeds cannot be mapped to one.
        let Some(head) = report_head(state, unit.extruder, None) else { continue };
        let name = if state.numbered_heads {
            state.head_name(unit.extruder.unwrap()).unwrap()
        } else { match unit.id {
            Some(a @ 128..=253) => format!("AMS HT{}", a - 127),
            Some(a) => format!("AMS {}", unit_label(a)),
            None => "AMS".into(),
        }};
        trays.extend(unit.trays.iter().filter_map(|t| view(head, name.clone(), t)));
    }
    for t in &state.external {
        let Some(head) = report_head(state, None, t.id) else { continue };
        trays.extend(view(head, "External".into(), t));
    }
    Layout { printer: printer.to_string(), heads: offline.heads.clone(), live: true, known: false, trays }
}

/// Why a mapping row needs a look.
#[derive(Clone, Debug, PartialEq)]
pub enum Warning {
    /// Used, but no tray: pick one.
    Unmapped,
    /// Two trays fit equally (same colour, other types): pick one.
    Choose,
    /// The saved tray is not on this printer.
    Missing,
    /// The tray is empty.
    Empty,
    /// The tray holds another material type than the job asks for.
    Mismatch { want: String, have: String },
    /// Its nozzle prints other colours of the same layers too: the nozzle swaps material.
    SharedNozzle { others: Vec<String> },
}

impl Warning {
    pub fn text(&self) -> String {
        match self {
            Warning::Unmapped => "No material: pick a tray".into(),
            Warning::Choose => "Two trays fit: pick one".into(),
            Warning::Missing => "Its tray is not on this printer".into(),
            Warning::Empty => "Its tray is empty".into(),
            Warning::Mismatch { want, have } => format!("The tray holds {have}, the job asks for {want}"),
            Warning::SharedNozzle { others } => format!("Shares its nozzle with {}: the nozzle swaps material", others.join(", ")),
        }
    }
}

/// How a row got its position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How {
    /// Saved with the project for this printer (or another, with the same tray here).
    Saved,
    /// Picked automatically (not saved).
    Auto,
    None,
}

/// One print colour on the printer.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub name: String,
    /// Printed parts or paint print in it.
    pub used: bool,
    /// What it shows (linear RGBA): its override, else its material's colour, else its default.
    pub shown: [f32; 4],
    /// What the design asks for: its override, imported design colour, else palette colour.
    pub design: [f32; 3],
    /// The design colour set apart from the material (an override).
    pub over: Option<[f32; 3]>,
    /// The spool's colour when it visibly differs from an override or imported design colour.
    pub mismatch: Option<[f32; 4]>,
    pub pos: Option<Position>,
    pub how: How,
    pub tray: Option<TrayView>,
    pub warnings: Vec<Warning>,
    pub implicit: bool,
    /// The material type the colour asks for itself ("TPU"); `None`: the job's.
    pub material: Option<String>,
    /// Picked automatically on the head a choice of heads gave it, for the print to change
    /// filament less (moved there, or kept so the others' moves hold).
    pub moved: Option<Moved>,
}

/// The head an automatically picked colour prints on because the print's filament changes take
/// less time so (each layer's colours weighed by the G-code writer's own plan of its changes),
/// and the time and purge that saves. A colour the choice kept where it was (`stays`) is held
/// there too, so the moves of the others hold.
#[derive(Clone, Debug, PartialEq)]
pub struct Moved {
    pub name: String,
    pub head: usize,
    pub stays: bool,
    pub seconds: f64,
    pub grams: f64,
}

impl Moved {
    /// What the row says of it: "it changes filament less there, saving 7 min and 5 g of purge".
    pub fn why(&self) -> String {
        let minutes = (self.seconds / 60.0).round();
        let time = if minutes >= 1.0 { format!("{minutes:.0} min") } else { format!("{:.0} s", self.seconds) };
        let purge = if self.grams >= 0.5 { format!(" and {:.0} g of purge", self.grams) } else { String::new() };
        format!("the print changes filament less there, saving {time}{purge}")
    }
}

impl Row {
    /// The tray's reported colour (linear RGBA).
    pub fn spool(&self) -> Option<[f32; 4]> {
        self.tray.as_ref().and_then(|t| t.colour)
    }

    /// No tray picked, or the saved tray is absent from this printer's layout.
    pub fn unassigned(&self) -> bool {
        self.pos.is_none() || self.tray.is_none()
    }

    /// The type label of its material ("PLA"), "?" when unknown, "" when unmapped.
    pub fn type_label(&self) -> String {
        self.tray.as_ref().map(|t| t.type_label()).unwrap_or_default()
    }
}

/// Every print colour on the printer, and the job's tools.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mapping {
    pub layout: Layout,
    pub rows: Vec<Row>,
    /// The job's tools in order (tool `i + 1`): the distinct positions of the used colours.
    pub tools: Vec<Position>,
    /// The material type the job asks for where a colour names none.
    pub want: String,
}

impl Mapping {
    pub fn row(&self, name: &str) -> Option<&Row> {
        self.rows.iter().find(|r| r.name == name)
    }

    /// A colour's job tool (1-based) and nozzle index.
    pub fn tool_of(&self, name: &str) -> Option<(u32, usize)> {
        let pos = self.row(name)?.pos?;
        let t = self.tools.iter().position(|p| *p == pos)?;
        Some((t as u32 + 1, pos.head))
    }

    /// The material colours the print colours show (linear RGB), for the views.
    pub fn inks(&self) -> std::collections::HashMap<String, [f32; 3]> {
        self.rows.iter().filter_map(|r| r.tray.as_ref().and_then(|t| t.colour).map(|c| (r.name.clone(), [c[0], c[1], c[2]]))).collect()
    }

    /// Tools per nozzle head among the used colours.
    pub fn tools_on(&self, head: usize) -> usize {
        self.tools.iter().filter(|p| p.head == head).count()
    }
}

fn oklab(c: [f32; 3]) -> [f32; 3] {
    fab_plate::paint::oklab(c)
}

pub(super) fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (a, b) = (oklab(a), oklab(b));
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

/// Squared Oklab distance: a difference of 0.03 is visible in the swatches.
pub const VISIBLE_DIFF: f32 = 0.0009;

/// A short colour word for a linear RGB colour.
pub fn colour_word(c: [f32; 3]) -> &'static str {
    let [l, a, b] = oklab(c);
    let chroma = a.hypot(b);
    if chroma < 0.035 {
        return match l {
            l if l >= 0.94 => "white",
            l if l >= 0.78 => "light grey",
            l if l >= 0.50 => "grey",
            l if l >= 0.28 => "dark grey",
            _ => "black",
        };
    }
    let hue = b.atan2(a).to_degrees().rem_euclid(360.0);
    if (40.0..110.0).contains(&hue) && l < 0.60 {
        return "brown";
    }
    match hue {
        h if h < 15.0 => "pink",
        h if h < 40.0 => if l > 0.75 { "pink" } else { "red" },
        h if h < 75.0 => "orange",
        h if h < 110.0 => "yellow",
        h if h < 165.0 => "green",
        h if h < 220.0 => "teal",
        h if h < 285.0 => "blue",
        h if h < 330.0 => "purple",
        _ => "pink",
    }
}

/// A linear RGB colour as an sRGB hex triplet.
pub fn hex(c: [f32; 3]) -> String {
    let b = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        let e = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
        (e * 255.0).round() as u32
    };
    format!("#{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]))
}

/// One line naming the nozzle, tray and what is loaded, omitting unknown details.
pub fn mapping_text(row: &Row, heads: &[String]) -> String {
    if row.unassigned() {
        return "No tray".into();
    }
    let mut parts = Vec::new();
    if let Some(pos) = row.pos {
        parts.push(pos.label(heads));
    }
    let mut material = Vec::new();
    let kind = row.type_label();
    if !kind.is_empty() && kind != "?" {
        material.push(kind);
    }
    if let Some([r, g, b, _]) = row.spool() {
        material.push(colour_word([r, g, b]).into());
    }
    if !material.is_empty() {
        parts.push(material.join(" "));
    }
    parts.join(" · ")
}

pub const UNASSIGNED_TIP: &str = "No tray is picked for this colour: it prints as the first material until you pick one (press to choose).";

/// The design-to-spool explanation shared by colour rows.
pub fn mapping_tip(row: &Row, heads: &[String]) -> String {
    if row.used && row.unassigned() {
        return UNASSIGNED_TIP.into();
    }
    let design = format!("Design colour {}.", hex(row.design));
    if let Some([r, g, b, _]) = row.mismatch {
        return format!("{design} It prints on a {} spool ({}): the slice and the print show the spool's colour; Normal shows the design colour.", colour_word([r, g, b]), hex([r, g, b]));
    }
    match (&row.pos, &row.tray) {
        (Some(pos), Some(t)) => {
            let mut what = Vec::new();
            if let Some(m) = t.material.as_deref().filter(|m| !m.trim().is_empty()) {
                what.push(m.to_string());
            }
            what.push(t.colour.map(|[r, g, b, _]| format!("{} spool", colour_word([r, g, b]))).unwrap_or_else(|| "spool colour not reported".into()));
            format!("{design} Prints on tray {}: {}.", pos.label(heads), what.join(", "))
        }
        _ => format!("{design} No tray picked."),
    }
}

fn same_type(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// The nozzles that reach every printed part of each print colour: per part its footprint `bounds`
/// and the colours it prints in (its own and its paint's), each nozzle by the machine's index,
/// which is the layout's head. A colour no part prints in has no entry. O(parts * nozzles).
pub fn reaching(machine: &Machine, parts: &[(fab_plate::Bounds, Vec<String>)]) -> Vec<(String, Vec<usize>)> {
    let mut out: Vec<(String, Vec<usize>)> = Vec::new();
    for (bounds, colours) in parts {
        let reach: Vec<usize> = (0..machine.nozzles.len())
            .filter(|&n| bounds.is_empty() || machine.nozzles[n].reach.contains_footprint(bounds, fab_plate::machine::EDGE_SLACK)).collect();
        for c in colours {
            match out.iter_mut().find(|(name, _)| name == c) {
                Some((_, heads)) => heads.retain(|h| reach.contains(h)),
                None => out.push((c.clone(), reach.clone())),
            }
        }
    }
    out
}

/// Map every print colour onto the layout. `used` names the colours printed parts and paint
/// print in; `want` is the material type a colour asks for (the job's default material). An
/// automatic pick takes its colour's `moves` head when a free tray there looks as right, and
/// else, between nozzles whose spools look as right, one in `reach` (a colour's nozzles that
/// reach all its parts, [`reaching`]; a colour not listed: any).
pub fn resolve(colors: &[PrintColor], used: &[String], layout: Layout, want: &str, moves: &[Moved], reach: &[(String, Vec<usize>)]) -> Mapping {
    let mut rows: Vec<Row> = Vec::with_capacity(colors.len());
    let mut taken: Vec<Position> = Vec::new();
    // Saved positions first, so automatic picks avoid their trays.
    let saved: Vec<Option<Position>> = colors.iter().map(|c| {
        let mine = c.at.iter().rev().find(|a| a.printer == layout.printer).and_then(|a| Position::parse(&layout.heads, &a.side, &a.tray));
        mine.or_else(|| c.at.iter().rev().filter_map(|a| Position::parse(&layout.heads, &a.side, &a.tray)).find(|p| layout.tray(*p).is_some()))
    }).collect();
    taken.extend(saved.iter().flatten().copied());
    let preferences = auto_heads(colors, used, &saved, &layout, want, reach);
    for ((c, saved), head) in colors.iter().zip(&saved).zip(&preferences) {
        let used = used.contains(&c.name);
        let design = c.rgb.or(c.design).unwrap_or(c.fallback);
        let want = c.material.as_deref().unwrap_or(want);
        let mut warnings = Vec::new();
        let has_design = c.rgb.or(c.design).is_some();
        // A move goes where a free tray takes it.
        let moved = moves.iter().find(|m| m.name == c.name).filter(|_| saved.is_none() && used).and_then(|m| match auto_pick(&layout, &taken, design, has_design, want, Some(m.head)) {
            Pick::One(p) if p.head == m.head && !taken.contains(&p) => Some((p, m.clone())),
            _ => None,
        });
        let (pos, how) = match saved {
            Some(p) => (Some(*p), How::Saved),
            None if !used => (None, How::None),
            None if moved.is_some() => {
                let p = moved.as_ref().unwrap().0;
                taken.push(p);
                (Some(p), How::Auto)
            }
            None => match auto_pick(&layout, &taken, design, has_design, want, *head) {
                Pick::One(p) => {
                    taken.push(p);
                    (Some(p), How::Auto)
                }
                Pick::Tie => {
                    warnings.push(Warning::Choose);
                    (None, How::None)
                }
                Pick::None => {
                    warnings.push(Warning::Unmapped);
                    (None, How::None)
                }
            },
        };
        let tray = pos.and_then(|p| layout.tray(p).cloned());
        if let (Some(_), None, true) = (pos, &tray, layout.live) {
            warnings.push(Warning::Missing);
        }
        if let Some(t) = &tray {
            if used && t.state == TrayState::Empty {
                warnings.push(Warning::Empty);
            }
            if let Some(have) = t.material.as_deref().filter(|m| used && !want.is_empty() && !same_type(m, want) && t.type_label() != "SUP") {
                warnings.push(Warning::Mismatch { want: want.to_string(), have: have.to_string() });
            }
        }
        let material = tray.as_ref().and_then(|t| t.colour);
        let shown = match (c.rgb, material) {
            (Some(o), _) => [o[0], o[1], o[2], 1.0],
            (None, Some(m)) => m,
            (None, None) => [c.fallback[0], c.fallback[1], c.fallback[2], 1.0],
        };
        let mismatch = match (c.rgb.or(c.design), material) {
            (Some(o), Some(m)) if dist(o, [m[0], m[1], m[2]]) > VISIBLE_DIFF => Some(m),
            _ => None,
        };
        rows.push(Row { name: c.name.clone(), used, shown, design, over: c.rgb, mismatch, pos, how, tray, warnings, implicit: c.implicit, material: c.material.clone(), moved: moved.map(|m| m.1) });
    }
    let mut tools: Vec<Position> = Vec::new();
    for r in rows.iter().filter(|r| r.used) {
        if let Some(p) = r.pos {
            if !tools.contains(&p) {
                tools.push(p);
            }
        }
    }
    // A nozzle with two used materials swaps between them.
    for head in 0..layout.heads.len() {
        let on: Vec<String> = rows.iter().filter(|r| r.used && r.pos.is_some_and(|p| p.head == head)).map(|r| r.name.clone()).collect();
        let positions = tools.iter().filter(|p| p.head == head).count();
        if positions > 1 {
            for r in rows.iter_mut().filter(|r| r.used && r.pos.is_some_and(|p| p.head == head)) {
                let others: Vec<String> = on.iter().filter(|n| **n != r.name).cloned().collect();
                r.warnings.push(Warning::SharedNozzle { others });
            }
        }
    }
    Mapping { layout, rows, tools, want: want.to_string() }
}

enum Pick {
    One(Position),
    Tie,
    None,
}

/// The trays an automatic pick chooses from: loaded ones of the wanted type, else any loaded one
/// (for a flexible type, any loaded one on a path of its own). A flexible type takes a dedicated
/// path among them when there is one: its changes purge nothing.
fn auto_pool<'a>(layout: &'a Layout, want: &str) -> Vec<&'a TrayView> {
    let loaded: Vec<&TrayView> = layout.trays.iter().filter(|t| t.state == TrayState::Loaded).collect();
    let typed: Vec<&TrayView> = loaded.iter().copied().filter(|t| t.material.as_deref().is_some_and(|m| same_type(m, want))).collect();
    let mut pool = if !typed.is_empty() {
        typed
    } else if flexible(want) {
        loaded.into_iter().filter(|t| t.own_path()).collect()
    } else {
        loaded
    };
    if flexible(want) && pool.iter().any(|t| t.dedicated()) {
        pool.retain(|t| t.dedicated());
    }
    pool
}

/// The same spool as far as a print can tell: the same reported colour and type.
fn same_spool(a: &TrayView, b: &TrayView) -> bool {
    a.colour.is_some() && a.colour == b.colour && same_type(a.material.as_deref().unwrap_or(""), b.material.as_deref().unwrap_or(""))
}

/// The nozzle each colour's automatic pick prints on: the one with its nearest spool, or, when
/// that one does not reach all the colour's parts (`reach`), a nozzle that does whose spool looks
/// as right; except that a nozzle nothing prints on takes a colour from a nozzle that would swap
/// materials (the colour that loses least by moving, when its spool there looks as right and the
/// move keeps its parts in reach). `None`: no preference, nothing to choose between.
fn auto_heads(colors: &[PrintColor], used: &[String], saved: &[Option<Position>], layout: &Layout, want: &str, reach: &[(String, Vec<usize>)]) -> Vec<Option<usize>> {
    let mut preferences: Vec<Option<usize>> = vec![None; colors.len()];
    let pool = auto_pool(layout, want);
    let heads: Vec<usize> = (0..layout.heads.len()).filter(|s| pool.iter().any(|t| t.pos.head == *s)).collect();
    if (!layout.live && !layout.known) || heads.len() < 2 {
        return preferences;
    }
    // Each colour picks among the trays of the type it asks for.
    let pools: Vec<Vec<&TrayView>> = colors.iter().map(|c| auto_pool(layout, c.material.as_deref().unwrap_or(want))).collect();
    // Colour i's nearest tray on a nozzle, and how far its colour is.
    let nearest = |i: usize, head: usize| -> Option<(f32, &TrayView)> {
        let design = colors[i].rgb.or(colors[i].design);
        let score = |t: &TrayView| t.colour.map_or(f32::MAX, |s| design.map_or(0.0, |d| dist(d, [s[0], s[1], s[2]])));
        pools[i].iter().filter(|t| t.pos.head == head).map(|t| (score(t), *t)).min_by(|a, b| a.0.total_cmp(&b.0).then_with(|| (a.1.pos.tray.ams, a.1.pos.tray.slot).cmp(&(b.1.pos.tray.ams, b.1.pos.tray.slot))))
    };
    // Whether nozzle `head` reaches all of colour i's parts.
    let reaches = |i: usize, head: usize| reach.iter().find(|(n, _)| *n == colors[i].name).is_none_or(|(_, heads)| heads.contains(&head));
    let picks: Vec<usize> = (0..colors.len()).filter(|&i| saved[i].is_none() && used.contains(&colors[i].name)).collect();
    for &i in &picks {
        // On a tie the earlier unit's tray, whichever nozzle it feeds.
        let order = |a: &usize, b: &usize| {
            let (x, y) = (nearest(i, *a), nearest(i, *b));
            let key = |n: &Option<(f32, &TrayView)>| n.map_or((f32::MAX, u8::MAX, u8::MAX), |(d, t)| (d, t.pos.tray.ams, t.pos.tray.slot));
            let (x, y) = (key(&x), key(&y));
            x.0.total_cmp(&y.0).then_with(|| (x.1, x.2).cmp(&(y.1, y.2)))
        };
        let best = heads.iter().copied().min_by(order);
        // A nozzle that reaches the parts, when its spool looks as right as the nearest one.
        let as_right = |b: usize, h: usize| match (nearest(i, b), nearest(i, h)) {
            (Some(there), Some(here)) => here.0 - there.0 <= VISIBLE_DIFF,
            _ => false,
        };
        preferences[i] = match best {
            Some(b) if !reaches(i, b) => heads.iter().copied().filter(|&h| reaches(i, h) && as_right(b, h)).min_by(order).or(best),
            _ => best,
        };
    }
    // The spools a nozzle prints from: its saved trays' and its picks' nearest.
    let saved_trays: Vec<(usize, &TrayView)> = saved.iter().zip(colors).filter(|(_, c)| used.contains(&c.name)).filter_map(|(p, _)| *p).filter_map(|p| layout.tray(p).map(|t| (p.head, t))).collect();
    for head in &heads {
        let printed_on = |s: &[Option<usize>], head: usize| picks.iter().any(|&i| s[i] == Some(head)) || saved_trays.iter().any(|(n, _)| *n == head);
        if printed_on(&preferences, *head) {
            continue;
        }
        let swaps = |head: usize| {
            let mut spools: Vec<&TrayView> = saved_trays.iter().filter(|(n, _)| *n == head).map(|(_, t)| *t).collect();
            for &i in picks.iter().filter(|&&i| preferences[i] == Some(head)) {
                if let Some((_, t)) = nearest(i, head) {
                    if !spools.iter().any(|o| o.pos == t.pos || same_spool(o, t)) {
                        spools.push(t);
                    }
                }
            }
            spools.len() > 1
        };
        let loss = |i: usize| match (nearest(i, *head), preferences[i].and_then(|s| nearest(i, s))) {
            (Some(there), Some(here)) => there.0 - here.0,
            _ => f32::MAX,
        };
        // Never for a spool that looks wrong: a swap costs plastic, a wrong colour the print.
        let keeps_reach = |i: usize| reaches(i, *head) || preferences[i].is_some_and(|s| !reaches(i, s));
        let mover = picks.iter().copied().filter(|&i| preferences[i].is_some_and(|s| swaps(s)) && loss(i) <= VISIBLE_DIFF && keeps_reach(i))
            .min_by(|&a, &b| loss(a).total_cmp(&loss(b)));
        if let Some(i) = mover {
            preferences[i] = Some(*head);
        }
    }
    preferences
}

/// A tray for a colour: loaded trays of the wanted type first (then any loaded tray), on its
/// nozzle when it has one, the nearest colour among them, a free tray before a taken one unless
/// a taken tray on that nozzle holds the same spool (one tray prints both, with no swap). With
/// no reports and no known loadout: the head with fewer colours (of the heads with trays), its
/// next free tray; a flexible colour a free dedicated path first, whichever head.
fn auto_pick(layout: &Layout, taken: &[Position], design: [f32; 3], has_design: bool, want: &str, head: Option<usize>) -> Pick {
    if !layout.live && !layout.known {
        let on = |s: usize| taken.iter().filter(|p| p.head == s).count();
        let free = |t: &&TrayView| !taken.contains(&t.pos);
        // The head with fewer colours of those with a tray to give: a free one of a unit for a
        // rigid colour (a free one at all for a flexible one), else any free one, else any.
        let rigid = !flexible(want);
        let gives: [&dyn Fn(&TrayView) -> bool; 3] = [&|t| free(&t) && !(rigid && t.own_path()), &|t| free(&t), &|_| true];
        let head = head.filter(|&h| layout.on(h).any(|t| gives[1](t))).or_else(|| gives.iter().find_map(|g| {
            (0..layout.heads.len()).filter(|s| layout.on(*s).any(|t| g(t))).min_by_key(|s| on(*s))
        }));
        let Some(head) = head else { return Pick::None };
        // A flexible filament takes a path of its own: a free dedicated one, then the first free
        // one, else any.
        if !rigid {
            let own: Vec<&TrayView> = layout.trays.iter().filter(|t| t.own_path()).collect();
            let pick = own.iter().copied().filter(free).filter(|t| t.dedicated()).min_by_key(|t| t.pos.head != head)
                .or_else(|| own.iter().copied().find(|t| free(t) && t.pos.head == head))
                .or_else(|| own.iter().copied().find(free))
                .or_else(|| own.first().copied());
            return match pick {
                Some(t) => Pick::One(t.pos),
                None => Pick::None,
            };
        }
        // a rigid one a unit's tray while one is free, keeping the paths of their own for the
        // flexible ones
        return match layout.on(head).filter(|t| !t.own_path()).find(free).or_else(|| layout.on(head).find(free)).or_else(|| layout.on(head).next()) {
            Some(t) => Pick::One(t.pos),
            None => Pick::None,
        };
    }
    let mut pool = auto_pool(layout, want);
    if pool.is_empty() {
        return Pick::None;
    }
    if let Some(head) = head {
        pool.retain(|t| t.pos.head == head);
    }
    if pool.is_empty() {
        return Pick::None;
    }
    let free: Vec<&TrayView> = pool.iter().copied().filter(|t| !taken.contains(&t.pos)).collect();
    let pool = if free.is_empty() { pool } else { free };
    let score = |t: &TrayView| t.colour.map_or(f32::MAX, |c| if has_design { dist(design, [c[0], c[1], c[2]]) } else { 0.0 });
    let mut ranked: Vec<(f32, &TrayView)> = pool.iter().map(|t| (score(t), *t)).collect();
    ranked.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| (a.1.pos.tray.ams, a.1.pos.tray.slot).cmp(&(b.1.pos.tray.ams, b.1.pos.tray.slot))));
    // Two trays of the same colour in different types: the person picks.
    if let [a, b, ..] = ranked.as_slice() {
        if has_design && (b.0 - a.0).abs() < 1e-4 && a.1.material.as_deref().map(str::to_ascii_uppercase) != b.1.material.as_deref().map(str::to_ascii_uppercase) {
            return Pick::Tie;
        }
    }
    let best = ranked[0].1;
    let shared = layout.trays.iter().find(|t| taken.contains(&t.pos) && t.pos.head == best.pos.head && same_spool(t, best));
    Pick::One(shared.map_or(best.pos, |t| t.pos))
}

/// Per job tool (`m.tools`), the heads it may print on: its own alone when a colour on it is
/// saved there or prints from a path of its own; else every head with a tray for each of its
/// colours (one that looks as right as its own when the trays report colours). And per head, the
/// tools it has room for: its trays (of the units, a path of its own is for a flexible colour).
pub fn head_choices(m: &Mapping, colors: &[PrintColor]) -> (Vec<Vec<usize>>, Vec<usize>) {
    let l = &m.layout;
    let room: Vec<usize> = (0..l.heads.len()).map(|h| l.on(h).filter(|t| !t.own_path() && (!l.live || t.state == TrayState::Loaded)).count()).collect();
    let choices = m.tools.iter().map(|tool| {
        let rows: Vec<&Row> = m.rows.iter().filter(|r| r.used && r.pos == Some(*tool)).collect();
        if rows.iter().any(|r| r.how != How::Auto || r.tray.as_ref().is_some_and(|t| t.own_path())) {
            return vec![tool.head];
        }
        (0..l.heads.len()).filter(|&h| h == tool.head || rows.iter().all(|r| {
            let Some(c) = colors.iter().find(|c| c.name == r.name) else { return false };
            let want = c.material.as_deref().unwrap_or(&m.want);
            if !l.live && !l.known {
                return l.on(h).any(|t| !t.own_path());
            }
            let design = c.rgb.or(c.design);
            let score = |t: &TrayView| match (design, t.colour) {
                (Some(d), Some(s)) => dist(d, [s[0], s[1], s[2]]),
                (None, _) => 0.0,
                (Some(_), None) => f32::MAX,
            };
            let own = r.tray.as_ref().map_or(f32::MAX, score);
            auto_pool(l, want).iter().any(|t| t.pos.head == h && score(t) <= own.max(VISIBLE_DIFF))
        })).collect()
    }).collect();
    (choices, room)
}

/// A tray's menu id.
pub fn tray_id(pos: Position) -> makepad_widgets_core::LiveId {
    makepad_widgets_core::LiveId::from_str(&format!("fab_tray:{}:{}", pos.head, pos.tray_text()))
}

/// The tray a menu pick names.
pub fn tray_pick(m: &Mapping, id: makepad_widgets_core::LiveId) -> Option<Position> {
    m.layout.trays.iter().map(|t| t.pos).find(|p| tray_id(*p) == id)
}

/// What a tray holds, as its row says it: its material type, "Empty", or nothing when the
/// printer has not said (the tray's tip says why).
pub fn tray_what(t: &TrayView) -> String {
    match t.state {
        TrayState::Empty => "Empty".to_string(),
        _ => t.material.clone().unwrap_or_default(),
    }
}

/// A compact tray row: "A2  PLA grey  80%", or just "A2" when nothing about it is known.
pub fn tray_line(t: &TrayView) -> String {
    let colour = t.colour.map(|[r, g, b, _]| colour_word([r, g, b]).to_string());
    let what = [Some(tray_what(t)), colour].into_iter().flatten().filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
    let remain = t.remain.map(|r| format!("{r:.0}%"));
    [Some(t.pos.tray_text()), Some(what), remain].into_iter().flatten().filter(|p| !p.is_empty()).collect::<Vec<_>>().join("  ")
}

/// A tray's tooltip: where it is, its material's full name and temperatures, what is left.
/// A toolhead as a person reads it: "Left nozzle"; a head already called a nozzle as it is
/// ("Nozzle", never "Nozzle nozzle").
pub fn nozzle_label(head: &str) -> String {
    if head.to_ascii_lowercase().ends_with("nozzle") { head.to_string() } else { format!("{head} nozzle") }
}

pub fn tray_tip(t: &TrayView, layout: &Layout) -> String {
    let mut lines = vec![format!("{} · {} · tray {}", nozzle_label(&layout.heads[t.pos.head]), t.unit, t.pos.tray_text())];
    match t.state {
        TrayState::Empty => lines.push("Empty".into()),
        TrayState::Unknown if !layout.live => lines.push("No printer linked: link it to read its trays, or right-click to set what is loaded".into()),
        TrayState::Unknown => lines.push("The printer does not say what is in it".into()),
        TrayState::Loaded => {}
    }
    match (&t.name, &t.material) {
        (Some(n), Some(m)) => lines.push(format!("{n} ({m})")),
        (None, Some(m)) => lines.push(m.clone()),
        _ => {}
    }
    if let Some([r, g, b, _]) = t.colour {
        lines.push(format!("Spool colour: {} ({})", colour_word([r, g, b]), hex([r, g, b])));
    }
    if let Some((lo, hi)) = t.temps {
        lines.push(format!("Nozzle {lo:.0}–{hi:.0} °C"));
    }
    if let Some(r) = t.remain {
        lines.push(format!("{r:.0}% left"));
    }
    if !layout.live && layout.known {
        lines.push("Last known: the printer is not reporting. Right-click to change it".into());
    }
    lines.join("\n")
}

/// A tray's row: its swatch (an outline when nothing is loaded, none when it is not known), its
/// id, its material type and the print colours mapped there; an empty tray cannot be picked.
pub fn tray_item(m: &Mapping, t: &TrayView, current: Option<Position>) -> crate::ui::popover::MenuItem {
    use crate::ui::popover::{MenuItem, MenuSwatch};
    let names: Vec<&str> = m.rows.iter().filter(|r| r.pos == Some(t.pos)).map(|r| r.name.as_str()).collect();
    let swatch = match (t.state, t.colour) {
        (TrayState::Empty, _) => MenuSwatch::Empty,
        (_, Some(c)) => MenuSwatch::Colour([c[0], c[1], c[2]]),
        (_, None) => MenuSwatch::Unknown,
    };
    MenuItem::new(tray_id(t.pos), &tray_line(t)).swatch(swatch).note(&names.join(", ")).checked(current == Some(t.pos)).enabled_if(t.state != TrayState::Empty)
}

/// A toolhead's named nozzle heading, and where its trays come from when not live.
pub fn head_row(m: &Mapping, head: usize) -> crate::ui::popover::MenuItem {
    let note = if m.layout.live { "" } else if m.layout.known { "last known" } else { "not linked" };
    crate::ui::popover::MenuItem::new(makepad_widgets_core::LiveId::from_str(&format!("fab_tray_head:{}", m.layout.heads[head])), &nozzle_label(&m.layout.heads[head])).note(note).disabled()
}

/// The trays to pick from, by nozzle (every tray of its units, then its external spool), with
/// `current` checked.
pub fn tray_menu(m: &Mapping, current: Option<Position>) -> Vec<crate::ui::popover::MenuItem> {
    use crate::ui::popover::MenuItem;
    let mut items = Vec::new();
    for head in 0..m.layout.heads.len() {
        if !items.is_empty() {
            items.push(MenuItem::sep());
        }
        items.push(head_row(m, head));
        items.extend(m.layout.on(head).map(|t| tray_item(m, t, current)));
    }
    items
}

/// The menu that sets what an offline tray holds: its material type, its colour, or empty.
pub fn tray_edit_menu(t: &TrayView, heads: &[String]) -> Vec<crate::ui::popover::MenuItem> {
    use crate::ui::popover::{MenuItem, MenuSwatch};
    let id = |what: &str| makepad_widgets_core::LiveId::from_str(&format!("fab_tray_edit:{what}"));
    let materials = MATERIALS.iter().map(|k| MenuItem::new(id(&format!("m:{k}")), k).checked(t.material.as_deref().is_some_and(|m| m.eq_ignore_ascii_case(k)))).collect();
    let colours = FILAMENT_COLOURS
        .iter()
        .map(|(name, srgb)| {
            let c = linear_of(*srgb);
            let on = t.colour.is_some_and(|tc| (0..3).all(|i| (tc[i] - c[i]).abs() < 1e-3));
            MenuItem::new(id(&format!("c:{name}")), name).swatch(MenuSwatch::Colour([c[0], c[1], c[2]])).checked(on)
        })
        .collect();
    vec![
        MenuItem::new(id("head"), &format!("{} · tray {}", nozzle_label(&heads[t.pos.head]), t.pos.tray_text())).disabled(),
        MenuItem::new(id("material"), "Material").flyout(materials),
        MenuItem::new(id("colour"), "Colour").flyout(colours),
        MenuItem::sep(),
        MenuItem::new(id("empty"), "Empty").checked(t.state == TrayState::Empty),
    ]
}

/// The edit a pick of [`tray_edit_menu`] names.
pub fn tray_edit_pick(id: makepad_widgets_core::LiveId) -> Option<TrayEdit> {
    let is = |what: &str| makepad_widgets_core::LiveId::from_str(&format!("fab_tray_edit:{what}")) == id;
    if is("empty") {
        return Some(TrayEdit::Empty);
    }
    if let Some(k) = MATERIALS.iter().find(|k| is(&format!("m:{k}"))) {
        return Some(TrayEdit::Material(k.to_string()));
    }
    FILAMENT_COLOURS.iter().find(|(n, _)| is(&format!("c:{n}"))).map(|(_, c)| TrayEdit::Colour(linear_of(*c)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A head's saved word is a script name, and reads back to that head, for side names and
    /// numbered tools alike.
    #[test]
    fn head_words_are_script_names_that_read_back() {
        let tools: Vec<String> = (1..=4).map(|n| format!("Tool {n}")).collect();
        assert_eq!(head_word(&tools, 2), "tool_3");
        for word in ["tool_3", "Tool 3"] {
            assert_eq!(Position::parse(&tools, word, "C1"), Some(Position { head: 2, tray: TrayId { ams: 2, slot: 0 } }), "{word}");
        }
        let sides = vec!["Left".to_string(), "Right".to_string()];
        assert_eq!((head_word(&sides, 0).as_str(), head_word(&sides, 1).as_str()), ("left", "right"));
        assert_eq!(Position::parse(&sides, "right", "A2"), Some(Position { head: 1, tray: TrayId { ams: 0, slot: 1 } }));
    }

    /// Units past D keep their letters both ways: a nine-tool printer's I1 is unit 8, and a
    /// unit's label is its letters, or HT and its number.
    #[test]
    fn trays_past_unit_d_read_and_label() {
        let tools: Vec<String> = (1..=9).map(|n| format!("Tool {n}")).collect();
        for (unit, label) in [(4u8, "E1"), (8, "I1"), (26, "AA1")] {
            let pos = Position::parse(&tools, "tool_9", label).unwrap();
            assert_eq!((pos.head, pos.tray), (8, TrayId { ams: unit, slot: 0 }), "{label}");
            assert_eq!(pos.tray_text(), label);
        }
        assert_eq!(Position::parse(&tools, "tool_1", "DY1"), None);
        assert_eq!(Position::parse(&tools, "tool_1", "A256").unwrap().tray_text(), "A256");
        assert_eq!([0, 5, 26, 128].map(unit_label), ["A", "F", "AA", "HT1"]);
    }

    const BLACK: [f32; 4] = [0.008, 0.008, 0.008, 1.0];
    const GREY: [f32; 4] = [0.533, 0.533, 0.533, 1.0];
    const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
    const PURPLE: [f32; 4] = [0.35, 0.045, 0.93, 1.0];
    const RED: [f32; 4] = [0.9, 0.02, 0.02, 1.0];
    const GREEN: [f32; 4] = [0.02, 0.7, 0.05, 1.0];
    const BLUE: [f32; 4] = [0.02, 0.05, 0.9, 1.0];

    fn tray(head: usize, ams: u8, slot: u8, material: &str, colour: [f32; 4]) -> TrayView {
        let pos = Position { head, tray: TrayId { ams, slot } };
        TrayView { pos, unit: "AMS".into(), material: Some(material.into()), name: None, colour: Some(colour), state: TrayState::Loaded, remain: None, temps: None, path: path_at(pos, &[], &[]) }
    }

    fn known(trays: Vec<TrayView>) -> Layout {
        Layout { heads: vec!["Left".into(), "Right".into()], printer: "H2D".into(), live: false, known: true, trays }
    }

    /// A unit on each default nozzle: head 1 black, grey, grey, black; head 0 a PETG grey, purple, grey, grey.
    fn loadout() -> Layout {
        known(vec![
            tray(1, 0, 0, "PLA", BLACK), tray(1, 0, 1, "PLA", GREY), tray(1, 0, 2, "PLA", GREY), tray(1, 0, 3, "PLA", BLACK),
            tray(0, 1, 0, "PETG", GREY), tray(0, 1, 1, "PLA", PURPLE), tray(0, 1, 2, "PLA", GREY), tray(0, 1, 3, "PLA", GREY),
        ])
    }

    fn colour(name: &str, rgb: [f32; 3]) -> PrintColor {
        PrintColor { name: name.into(), rgb: Some(rgb), fallback: [0.5; 3], design: None, at: Vec::new(), material: None, implicit: false }
    }

    fn of(c: [f32; 4]) -> [f32; 3] {
        [c[0], c[1], c[2]]
    }

    fn map(colors: &[PrintColor], layout: Layout) -> Mapping {
        let used: Vec<String> = colors.iter().map(|c| c.name.clone()).collect();
        resolve(colors, &used, layout, "PLA", &[], &[])
    }

    fn sharing(m: &Mapping) -> usize {
        m.rows.iter().filter(|r| r.warnings.iter().any(|w| matches!(w, Warning::SharedNozzle { .. }))).count()
    }

    #[test]
    fn four_toolheads_print_a_colour_each() {
        let mut machine = Machine::dual_nozzle_350();
        machine.nozzles = (1..=4).map(|i| {
            let mut nozzle = machine.nozzles[0].clone();
            nozzle.name = format!("Tool {i}");
            nozzle
        }).collect();
        machine.slots = Machine::parse_slots(&["1:Ext", "2:Ext", "3:Ext", "4:Ext"].map(str::to_string), 4).unwrap();
        let offline = Offline::of(&machine);
        let colors = [colour("Red", of(RED)), colour("Green", of(GREEN)), colour("Blue", of(BLUE)), colour("White", of(WHITE))];
        let m = map(&colors, layout(None, None, &offline, &|_| None));
        assert_eq!(m.tools.len(), 4, "one tool per colour");
        assert_eq!(m.tools.iter().map(|p| p.head).collect::<std::collections::BTreeSet<_>>(), [0, 1, 2, 3].into_iter().collect(), "one colour per toolhead");
        assert_eq!(m.tools.iter().map(|p| p.label(&m.layout.heads)).collect::<Vec<_>>(), ["T1 · Ext", "T2 · Ext", "T3 · Ext", "T4 · Ext"]);
        let loaded = parse_loadout(&loadout_text(&m.layout), &m.layout.heads).unwrap();
        assert_eq!(loaded.heads, m.layout.heads, "the machine's toolhead names");
        assert_eq!(loaded.trays.iter().map(|t| t.pos).collect::<Vec<_>>(), m.layout.trays.iter().map(|t| t.pos).collect::<Vec<_>>(), "every position survives a loadout round trip");
        let mut saved = colors[0].clone();
        saved.at = vec![fab_plate::PrintColorAt { printer: offline.printer.clone(), side: "tool 3".into(), tray: "Ext".into() }];
        let m = map(&[saved], m.layout);
        assert_eq!(m.rows[0].pos.unwrap().head, 2, "saved toolhead word resolves to its machine index");
        assert_eq!(m.rows[0].how, How::Saved);
    }

    /// The dual-nozzle printer's trays, laid over a one-nozzle profile, offer only the trays of the
    /// nozzle the profile has; the dual printer's kept loadout names no head of it.
    #[test]
    fn trays_on_a_head_the_printer_lacks_are_left_out() {
        let mut machine = Machine::dual_nozzle_350();
        machine.nozzles.truncate(1);
        machine.nozzles[0].name = "Nozzle".into();
        machine.slots = Machine::parse_slots(&["1:Ext".to_string()], 1).unwrap();
        let mut l = layout(None, Some(&loadout()), &Offline::of(&machine), &|_| None);
        assert_eq!(l.heads, ["Nozzle"]);
        assert!(drop_missing_heads(&mut l), "head 1's unit goes");
        assert_eq!(l.trays.len(), 4, "head 0's unit only");
        assert!(l.trays.iter().all(|t| t.pos.head == 0));
        assert!(!drop_missing_heads(&mut l), "nothing more to drop");
        assert!(parse_loadout(&loadout_text(&loadout()), &l.heads).is_none(), "the dual printer's words name no head of this one");
    }

    #[test]
    fn two_colours_print_on_a_nozzle_each() {
        // Both nearest on head 1: the grey has its match on head 0 too, so it goes there.
        let m = map(&[colour("Dark", of(BLACK)), colour("Light", of(GREY))], loadout());
        assert_eq!(m.rows[0].pos.unwrap().head, 1);
        assert_eq!(m.rows[1].pos.unwrap().head, 0);
        assert_eq!(m.tools.len(), 2);
        assert_eq!(sharing(&m), 0);
    }

    /// A plate of two parts, one only the left nozzle reaches and one only the right, each colour
    /// with the same spool on both nozzles: each colour prints on the nozzle that reaches its part.
    /// The nearest-spool pick alone (the earlier unit's tray, then the first colour moved to the
    /// idle nozzle) puts both out of reach.
    #[test]
    fn a_colour_on_both_nozzles_prints_on_the_one_that_reaches_its_part() {
        let machine = Machine::dual_nozzle_350();
        let at = |x: f64| fab_plate::Bounds { min: [x, 100.0, 0.0], max: [x + 20.0, 120.0, 20.0] };
        let plate = [(at(2.0), vec!["Dark".to_string()]), (at(328.0), vec!["Light".to_string()])];
        let reach = reaching(&machine, &plate);
        assert_eq!(reach, [("Dark".to_string(), vec![0]), ("Light".to_string(), vec![1])], "each part in one nozzle's reach");
        let colors = [colour("Dark", of(BLACK)), colour("Light", of(WHITE))];
        let both = || known(vec![tray(0, 0, 0, "PLA", BLACK), tray(0, 0, 1, "PLA", WHITE), tray(1, 1, 0, "PLA", BLACK), tray(1, 1, 1, "PLA", WHITE)]);
        let used: Vec<String> = colors.iter().map(|c| c.name.clone()).collect();
        let heads = |m: &Mapping| m.rows.iter().map(|r| r.pos.map(|p| p.head)).collect::<Vec<_>>();
        assert_eq!(heads(&resolve(&colors, &used, both(), "PLA", &[], &[])), [Some(1), Some(0)], "without reach, both out of it");
        let m = resolve(&colors, &used, both(), "PLA", &[], &reach);
        assert_eq!(heads(&m), [Some(0), Some(1)], "each on the nozzle that reaches its part");
        assert_eq!(sharing(&m), 0, "and no nozzle swaps");
    }

    /// Two design colours with no spool of their own, both nearest the same grey spool, on trays
    /// of their own they would swap between two identical spools for nothing.
    #[test]
    fn colours_nearest_the_same_spool_print_from_one_tray() {
        let colors = [colour("Blue", [0.0103, 0.0782, 0.6867]), colour("Yellow", [0.791, 0.527, 0.004]), colour("Teal", [0.023, 0.337, 0.275])];
        let m = map(&colors, loadout());
        assert_eq!(m.rows[0].pos.unwrap().label(&m.layout.heads), "L · B2", "the purple spool is the nearest for blue");
        assert_eq!(m.rows[1].pos.unwrap().label(&m.layout.heads), "R · A2");
        assert_eq!(m.rows[2].pos, m.rows[1].pos, "the same grey spool, no change between them");
        assert_eq!(m.tools.len(), 2);
        assert_eq!(sharing(&m), 0);
    }

    #[test]
    fn a_colour_keeps_its_spool_rather_than_a_nozzle_with_a_worse_one() {
        let m = map(&[colour("Red", of(RED)), colour("Blue", of(BLUE))], known(vec![tray(1, 0, 0, "PLA", RED), tray(1, 0, 1, "PLA", BLUE), tray(0, 1, 0, "PLA", WHITE)]));
        assert!(m.rows.iter().all(|r| r.pos.unwrap().head == 1), "not white for the sake of a nozzle");
        assert_eq!(sharing(&m), 2);
    }

    #[test]
    fn colours_past_the_nozzles_swap_on_one() {
        let colors = [colour("Red", of(RED)), colour("Green", of(GREEN)), colour("Blue", of(BLUE))];
        let m = map(&colors, known(vec![tray(1, 0, 0, "PLA", RED), tray(1, 0, 1, "PLA", GREEN), tray(0, 1, 0, "PLA", BLUE)]));
        assert_eq!(m.rows[2].pos.unwrap().head, 0);
        assert_eq!(m.tools.len(), 3);
        assert_eq!(sharing(&m), 2);
    }

    #[test]
    fn a_saved_colour_keeps_its_tray_and_the_other_takes_the_free_nozzle() {
        let mut saved = colour("Light", of(GREY));
        saved.at = vec![fab_plate::PrintColorAt { printer: "H2D".into(), side: "right".into(), tray: "A1".into() }];
        let m = map(&[saved, colour("Other", of(GREY))], loadout());
        assert_eq!(m.rows[0].pos.unwrap().label(&m.layout.heads), "R · A1", "kept, black spool and all");
        assert_eq!(m.rows[1].pos.unwrap().head, 0);
        assert_eq!(sharing(&m), 0);
    }

    #[test]
    fn the_wanted_material_comes_before_a_nozzle_of_its_own() {
        let trays = vec![tray(0, 1, 0, "PLA", GREY), tray(0, 1, 1, "PLA", BLACK), tray(1, 0, 0, "PETG", GREY)];
        let m = map(&[colour("Dark", of(BLACK)), colour("Light", of(GREY))], known(trays));
        assert!(m.rows.iter().all(|r| r.pos.unwrap().head == 0));
        assert_eq!(sharing(&m), 2);
    }

    /// A profile's slots (`fab_material_slots`) are the trays with no printer linked: two, four
    /// or nine, on one nozzle or two, saved under the profile's name. The rigid colours take a
    /// unit's trays, a slot each; the flexible one, wherever it comes in the list, the dedicated
    /// path, whichever nozzle it feeds.
    #[test]
    fn a_flexible_colour_takes_the_profiles_dedicated_path() {
        let profile = |entries: Vec<String>, nozzles: usize| {
            let mut m = Machine::dual_nozzle_350();
            m.name = format!("{} slots", entries.len());
            m.slots = Machine::parse_slots(&entries, nozzles).unwrap();
            Offline::of(&m)
        };
        let named = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let nine: Vec<String> = (1..=4).map(|i| format!("1:A{i}")).chain((1..=4).map(|i| format!("2:B{i}"))).chain(["2:Ext:dedicated".to_string()]).collect();
        let cases = [(profile(named(&["1:A1", "1:Ext:dedicated"]), 1), 1), (profile(named(&["1:A1", "1:A2", "1:A3", "1:Ext:dedicated"]), 1), 3), (profile(nine, 2), 8)];
        for (offline, rigid) in cases {
            let l = layout(None, None, &offline, &|_| None);
            assert_eq!((l.trays.len(), l.printer.as_str()), (offline.slots.len(), offline.printer.as_str()));
            let mut colors: Vec<PrintColor> = (0..rigid).map(|i| colour(&format!("C{i}"), [0.1 * i as f32, 0.5, 0.5])).collect();
            let mut cord = colour("Cord", of(BLACK));
            cord.material = Some("TPU".into());
            colors.insert(1.min(colors.len()), cord);
            let m = map(&colors, l);
            let on = |name: &str| m.row(name).and_then(|r| r.tray.clone());
            assert!(on("Cord").is_some_and(|t| t.dedicated()), "{}: the cord on {:?}", offline.printer, m.row("Cord").and_then(|r| r.pos));
            for c in colors.iter().filter(|c| c.name != "Cord") {
                assert!(on(&c.name).is_some_and(|t| !t.own_path()), "{}: {} on {:?}", offline.printer, c.name, m.row(&c.name).and_then(|r| r.pos));
            }
            assert_eq!(m.tools.len(), colors.len(), "{}: a slot each", offline.printer);
        }
        assert_eq!(Offline::of(&Machine::dual_nozzle_350()).printer, OFFLINE_PRINTER);
    }

    /// A colour that asks for TPU takes a TPU spool; with none known it takes an external spool,
    /// the path a flexible filament feeds on, while the rigid colour keeps to a unit; saved on a
    /// PLA tray it says the tray holds the wrong type.
    #[test]
    fn a_flexible_colour_takes_its_own_type_on_a_path_of_its_own() {
        let mut spine = colour("Spine", of(BLACK));
        spine.material = Some("TPU".into());
        let external = |head, id, material, c| TrayView { unit: "External".into(), ..tray(head, id, id, material, c) };
        let m = map(&[colour("Body", of(GREY)), spine.clone()], known(vec![tray(0, 1, 0, "PLA", GREY), tray(1, 0, 0, "PLA", BLACK), external(1, 255, "TPU", BLACK)]));
        assert_eq!(m.rows[1].pos.unwrap().label(&m.layout.heads), "R · Ext", "the TPU spool, not the black PLA");
        assert_eq!(m.rows[1].material.as_deref(), Some("TPU"));
        assert_eq!(m.rows[0].pos.unwrap().label(&m.layout.heads), "L · B1");
        let m = map(&[colour("Body", of(GREY)), spine.clone()], layout(None, None, &Offline::of(&Machine::dual_nozzle_350()), &|_| None));
        assert!(m.rows[1].tray.as_ref().is_some_and(|t| t.own_path()), "{:?}", m.rows[1].pos);
        assert!(m.rows[0].tray.as_ref().is_some_and(|t| !t.own_path()), "{:?}", m.rows[0].pos);
        let mut saved = spine;
        saved.at = vec![fab_plate::PrintColorAt { printer: "H2D".into(), side: "right".into(), tray: "A1".into() }];
        let m = map(&[saved], loadout());
        assert!(m.rows[0].warnings.iter().any(|w| matches!(w, Warning::Mismatch { want, have } if want == "TPU" && have == "PLA")), "{:?}", m.rows[0].warnings);
    }

    #[test]
    fn numbered_report_maps_each_heads_own_tray() {
        let state = super::super::panel::tests::numbered_report();
        let mut machine = Machine::dual_nozzle_350();
        machine.nozzles = (1..=4).map(|h| {
            let mut nozzle = machine.nozzles[0].clone();
            nozzle.name = format!("Tool {h}");
            nozzle
        }).collect();
        machine.slots = (0..4).map(|h| Slot {
            nozzle: h, tray: tray_text(TrayId { ams: h as u8, slot: 0 }), path: SlotPath::Own,
        }).collect();
        let layout = layout(Some(("Fixture", &state)), None, &Offline::of(&machine), &|_| None);
        assert_eq!(layout.trays.len(), 4);
        for h in 0..4 {
            let tray = &layout.trays[h];
            assert_eq!(tray.pos, Position { head: h, tray: TrayId { ams: h as u8, slot: 0 } });
            assert_eq!(tray.unit, format!("Tool {}", h + 1));
            assert_eq!(tray.path, SlotPath::Own);
            assert_eq!(tray.remain, None);
        }
    }
}
