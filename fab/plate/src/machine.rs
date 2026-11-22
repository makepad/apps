//! The printer's build volume and what each nozzle can reach.
//!
//! A dual-nozzle machine does not have one build volume: each nozzle reaches
//! its own strip of the bed and its own height, and an object must fit the
//! volume of every nozzle that prints it. [`check`] reports, per part, where
//! it leaves the bed, a nozzle's reach or a keep-out zone.
//!
//! A printer is one of two kinds ([`Technology`]): filament, whose nozzles lay beads, or masked
//! resin, whose screen cures a whole layer at once through its pixels ([`ResinScreen`]). A resin
//! printer has no nozzles; its bed is the screen's area.

use crate::{Bounds, Part, Plate};

/// A rectangle on the bed, in millimetres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub min: [f64; 2],
    pub max: [f64; 2],
}

impl Rect {
    pub const fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Rect {
        Rect { min: [x0, y0], max: [x1, y1] }
    }
    /// Whether `b`'s footprint lies inside this rectangle (within `slack`).
    pub fn contains_footprint(&self, b: &Bounds, slack: f64) -> bool {
        b.min[0] >= self.min[0] - slack
            && b.min[1] >= self.min[1] - slack
            && b.max[0] <= self.max[0] + slack
            && b.max[1] <= self.max[1] + slack
    }
    pub fn overlaps_footprint(&self, b: &Bounds) -> bool {
        b.min[0] < self.max[0] && b.max[0] > self.min[0] && b.min[1] < self.max[1] && b.max[1] > self.min[1]
    }
    pub fn width(&self) -> f64 {
        self.max[0] - self.min[0]
    }
    pub fn depth(&self) -> f64 {
        self.max[1] - self.min[1]
    }
    /// The rectangle as a printer preset lists an area (`printable_area` and the like): its four
    /// corners as "XxY" in mm, from its least corner round counter-clockwise.
    pub fn corners(&self) -> Vec<String> {
        [[self.min[0], self.min[1]], [self.max[0], self.min[1]], [self.max[0], self.max[1]], [self.min[0], self.max[1]]].iter().map(|[x, y]| format!("{x}x{y}")).collect()
    }
}

/// The name of nozzle `e` of a machine with `count`: "Nozzle" alone, "Left" and "Right" of
/// two, "Tool 1" and up of more.
pub fn nozzle_name(count: usize, e: usize) -> String {
    match (count, e) {
        (1, _) => "Nozzle".into(),
        (2, 0) => "Left".into(),
        (2, _) => "Right".into(),
        _ => format!("Tool {}", e + 1),
    }
}

/// A nozzle's name as a script name: lowercased, with anything but letters and digits as `_`
/// ("Left" is `left`, "Tool 1" is `tool_1`). Scripts and tool answers name nozzles this way.
pub fn nozzle_word(name: &str) -> String {
    name.trim().chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Nozzle {
    pub name: String,
    /// The part of the bed this nozzle can print on.
    pub reach: Rect,
    /// The highest point this nozzle can print, in mm above the bed.
    pub max_height: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Clearance {
    pub max_radius: f64,
    pub height_to_rod: f64,
    pub height_to_lid: f64,
    pub dist_to_rod: f64,
}

/// The screen of a masked resin printer: the pixel grid every layer image is drawn on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResinScreen {
    /// Pixels along the bed's X and Y.
    pub pixels: [u32; 2],
    /// The pixel pitch along X and Y (mm): the screen is `pixels * pitch` mm.
    pub pitch: [f64; 2],
    /// The image is mirrored along the bed's X and Y on its way to the screen.
    pub mirror: [bool; 2],
    /// Quarter turns (0 to 3, counter-clockwise) of the image on its way to the screen.
    pub quarter_turns: u8,
    /// The grey levels a pixel can show: 2 is on or off, 256 is eight bits.
    pub grey_levels: u16,
}

impl ResinScreen {
    /// The screen's area on the bed: from the origin, `pixels * pitch` mm.
    pub fn area(&self) -> Rect {
        Rect::new(0.0, 0.0, self.pixels[0] as f64 * self.pitch[0], self.pixels[1] as f64 * self.pitch[1])
    }
}

/// What a printer makes a part with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Technology {
    /// Fused filament: nozzles lay beads layer by layer.
    Filament,
    /// Masked resin: the part hangs from the build plate, and each layer cures at once through
    /// the screen's lit pixels.
    Resin(ResinScreen),
}

/// How a material slot's filament reaches its nozzle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotPath {
    /// Through its unit, the way the unit's other trays go.
    Shared,
    /// On a path of its own into the nozzle (an external spool's): the path a flexible
    /// filament takes. A change on the nozzle still purges it.
    Own,
    /// A path of its own that shares nothing with the other slots (`fab_dedicated_path`):
    /// changes into or out of its filament purge nothing.
    Dedicated,
}

/// A place a filament feeds a nozzle from: a tray of a unit, or a spool on a path of its own.
#[derive(Clone, Debug, PartialEq)]
pub struct Slot {
    /// The nozzle it feeds (an index into [`Machine::nozzles`]).
    pub nozzle: usize,
    /// The tray as the printer labels it: "A1" (unit A, tray 1), "HT1", "Ext".
    pub tray: String,
    pub path: SlotPath,
}

/// The units a tray label numbers by letters (A to DX). The printer link numbers the
/// high-temperature units and the external spools above them.
pub const UNITS: usize = 128;
/// The trays a unit numbers (its tray 1 and up).
pub const UNIT_TRAYS: usize = 256;

/// A unit's letters as its trays are labelled: A to Z, then AA, AB and on.
pub fn unit_letters(unit: usize) -> String {
    let mut n = unit + 1;
    let mut letters = Vec::new();
    while n > 0 {
        n -= 1;
        letters.push(b'A' + (n % 26) as u8);
        n /= 26;
    }
    letters.iter().rev().map(|&b| b as char).collect()
}

/// A unit's tray label ("B3", "E1", "AA2"): the unit (0-based) and the tray (1-based), within
/// [`UNITS`] and [`UNIT_TRAYS`].
pub fn unit_tray(label: &str) -> Option<(usize, usize)> {
    let letters = label.bytes().take_while(u8::is_ascii_alphabetic).count();
    let (unit, tray) = label.split_at(letters);
    if unit.is_empty() || tray.is_empty() || !tray.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let unit = unit.bytes().try_fold(0usize, |n, b| n.checked_mul(26)?.checked_add((b.to_ascii_uppercase() - b'A') as usize + 1))? - 1;
    let tray: usize = tray.parse().ok()?;
    (unit < UNITS && (1..=UNIT_TRAYS).contains(&tray)).then_some((unit, tray))
}

/// A tray label a slot can carry: a unit's tray ("B3", "E1"), "HT1" to "HT126", or "Ext".
fn tray_label(t: &str) -> bool {
    if t.eq_ignore_ascii_case("ext") {
        return true;
    }
    if let Some(n) = t.strip_prefix("HT") {
        return n.parse::<u8>().is_ok_and(|n| (1..=126).contains(&n));
    }
    unit_tray(t).is_some()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Machine {
    pub name: String,
    /// Filament or resin.
    pub technology: Technology,
    /// The printable area of the bed.
    pub bed: Rect,
    /// The build volume's height (the tallest nozzle's reach).
    pub max_height: f64,
    pub clearance: Clearance,
    pub nozzles: Vec<Nozzle>,
    /// Areas no object may cover (sensor and purge zones).
    pub keep_out: Vec<Rect>,
    /// The places filaments feed its nozzles from, in the printer's order (none on a resin
    /// printer).
    pub slots: Vec<Slot>,
}

/// How far (mm) a part may overhang an edge before it counts as outside:
/// absorbs float noise from meshing and booleans, nothing a nozzle could miss.
pub const EDGE_SLACK: f64 = 0.01;

impl Machine {
    /// The dual-nozzle printer Fab targets first: a 350 × 320 mm bed and
    /// 325 mm of height. The left nozzle reaches X 0–325 up to 320 mm (it is
    /// the one that lifts when idle, 5 mm lower), the right nozzle X 25–350 up
    /// to 325 mm; both reach all of Y. So the strips X < 25 and X > 325 are
    /// single-nozzle zones. A keep-out rectangle at the back (X 145–256,
    /// Y 310–326) is the nozzle-wrap detection area.
    pub fn dual_nozzle_350() -> Machine {
        Machine {
            name: "Dual nozzle 350 × 320 × 325".into(),
            technology: Technology::Filament,
            bed: Rect::new(0.0, 0.0, 350.0, 320.0),
            max_height: 325.0,
            clearance: Clearance { max_radius: 96.0, height_to_rod: 47.4, height_to_lid: 201.0, dist_to_rod: 50.0 },
            nozzles: vec![
                Nozzle { name: "Left".into(), reach: Rect::new(0.0, 0.0, 325.0, 320.0), max_height: 320.0 },
                Nozzle { name: "Right".into(), reach: Rect::new(25.0, 0.0, 350.0, 320.0), max_height: 325.0 },
            ],
            keep_out: vec![Rect::new(145.0, 310.0, 256.0, 326.0)],
            slots: Machine::dual_nozzle_slots(),
        }
    }

    /// A masked resin printer: its bed is the screen's area, `max_height` mm of travel, no
    /// nozzles, no keep-out zones.
    pub fn resin(name: &str, screen: ResinScreen, max_height: f64) -> Machine {
        Machine {
            name: name.into(),
            technology: Technology::Resin(screen),
            bed: screen.area(),
            max_height,
            clearance: Clearance { max_radius: 0.0, height_to_rod: 0.0, height_to_lid: 0.0, dist_to_rod: 0.0 },
            nozzles: Vec::new(),
            keep_out: Vec::new(),
            slots: Vec::new(),
        }
    }

    /// The resin printer's screen; None for a filament printer.
    pub fn resin_screen(&self) -> Option<&ResinScreen> {
        match &self.technology {
            Technology::Resin(s) => Some(s),
            Technology::Filament => None,
        }
    }

    /// Same kind, bed, height, nozzles and keep-out zones: everything placement depends on (a
    /// renamed printer with the same volume needs no new plate).
    pub fn same_volume(&self, other: &Machine) -> bool {
        (self.technology, self.bed, self.max_height, &self.nozzles, &self.keep_out) == (other.technology, other.bed, other.max_height, &other.nozzles, &other.keep_out)
    }

    /// The bed point a new model is centred on.
    pub fn bed_center(&self) -> [f64; 2] {
        [(self.bed.min[0] + self.bed.max[0]) * 0.5, (self.bed.min[1] + self.bed.max[1]) * 0.5]
    }

    /// The nozzles that can print all of `b`.
    pub fn nozzles_reaching(&self, b: &Bounds) -> Vec<usize> {
        (0..self.nozzles.len())
            .filter(|&n| {
                let nz = &self.nozzles[n];
                nz.reach.contains_footprint(b, EDGE_SLACK) && b.max[2] <= nz.max_height + EDGE_SLACK
            })
            .collect()
    }

    /// The part of the bed every one of `nozzles` reaches (the whole bed for none): where a
    /// part printing with all of them can stand. Its max lies below its min where they share
    /// none.
    pub fn reach(&self, nozzles: &[usize]) -> Rect {
        let mut r = self.bed;
        for nz in nozzles.iter().filter_map(|&n| self.nozzles.get(n)) {
            for k in 0..2 {
                r.min[k] = r.min[k].max(nz.reach.min[k]);
                r.max[k] = r.max[k].min(nz.reach.max[k]);
            }
        }
        r
    }
}

/// One placement problem of one part.
#[derive(Clone, Debug, PartialEq)]
pub enum Issue {
    /// Some of the part is off the bed's printable area.
    OffBed,
    /// Taller than the build volume.
    TooTall { height: f64, limit: f64 },
    /// Below the plate (Z < 0): the part has not been dropped onto the bed.
    BelowPlate { depth: f64 },
    /// Not touching the plate: the lowest point floats above it.
    Floating { gap: f64 },
    /// The part's assigned nozzle cannot reach all of it.
    OutOfReach { nozzle: usize },
    /// The part's assigned nozzle cannot reach its top.
    AboveNozzle { nozzle: usize, height: f64, limit: f64 },
    /// No single nozzle reaches the whole part (it spans both single-nozzle
    /// strips, or is too tall for the only nozzle reaching its strip).
    NoNozzle,
    /// Only these nozzles reach the part: fine for one filament, but it limits
    /// which filaments it can use. Informational.
    OnlyNozzles { nozzles: Vec<usize> },
    /// Covers a keep-out area.
    KeepOut { zone: usize },
}

impl Issue {
    /// Whether the issue stops the part from printing (a warning in the bed
    /// view); informational issues only explain a restriction.
    pub fn is_blocking(&self) -> bool {
        !matches!(self, Issue::OnlyNozzles { .. } | Issue::Floating { .. })
    }
    pub fn describe(&self, machine: &Machine) -> String {
        let nozzle = |n: &usize| machine.nozzles.get(*n).map(|z| z.name.as_str()).unwrap_or("?").to_string();
        match self {
            Issue::OffBed => "partly off the bed".into(),
            Issue::TooTall { height, limit } => format!("{height:.1} mm tall, the build volume is {limit:.0} mm"),
            Issue::BelowPlate { depth } => format!("{depth:.2} mm below the plate (drop it onto the bed)"),
            Issue::Floating { gap } => format!("floats {gap:.2} mm above the plate"),
            Issue::OutOfReach { nozzle: n } => format!("outside the {} nozzle's reach", nozzle(n)),
            Issue::AboveNozzle { nozzle: n, height, limit } => {
                format!("{height:.1} mm tall, the {} nozzle reaches {limit:.0} mm", nozzle(n))
            }
            Issue::NoNozzle => "no single nozzle reaches all of it".into(),
            Issue::OnlyNozzles { nozzles } => {
                let names: Vec<String> = nozzles.iter().map(nozzle).collect();
                format!("only the {} nozzle reaches it", names.join(" and "))
            }
            Issue::KeepOut { zone } => format!("covers keep-out zone {}", zone + 1),
        }
    }
}

/// The issues of one part.
#[derive(Clone, Debug, PartialEq)]
pub struct PartReport {
    pub part: usize,
    pub bounds: Bounds,
    pub issues: Vec<Issue>,
}

impl PartReport {
    pub fn blocking(&self) -> bool {
        self.issues.iter().any(Issue::is_blocking)
    }
}

/// Check every part of `plate` against `machine`.
pub fn check(machine: &Machine, plate: &Plate) -> Vec<PartReport> {
    plate
        .parts
        .iter()
        .enumerate()
        .map(|(i, part)| PartReport { part: i, bounds: part.bounds(), issues: check_part(machine, part) })
        .collect()
}

pub fn check_part(machine: &Machine, part: &Part) -> Vec<Issue> {
    let b = part.bounds();
    let mut issues = Vec::new();
    if b.is_empty() {
        return issues;
    }
    if !machine.bed.contains_footprint(&b, EDGE_SLACK) {
        issues.push(Issue::OffBed);
    }
    // where it rests, as the layout drops it: a field part's box may reach below its lowest
    // found solid point by what its proof left open (a warning of its own, no failure). Above
    // the plate it floats unless it stands on printed material that is held. A pending floor
    // (a part above its object's rest), or whether it is held, is told by the analysis that
    // proves it.
    if let Some(floor) = part.known_floor() {
        if floor < -EDGE_SLACK {
            issues.push(Issue::BelowPlate { depth: -floor });
        } else if floor > EDGE_SLACK && part.known_held() == Some(false) {
            issues.push(Issue::Floating { gap: floor });
        }
    }
    let height = b.max[2];
    if height > machine.max_height + EDGE_SLACK {
        issues.push(Issue::TooTall { height, limit: machine.max_height });
    }
    for (zone, rect) in machine.keep_out.iter().enumerate() {
        if rect.overlaps_footprint(&b) {
            issues.push(Issue::KeepOut { zone });
        }
    }
    // A resin printer has no nozzles: the screen's area and the height are all it limits.
    if machine.nozzles.is_empty() {
        return issues;
    }
    // Every nozzle it prints with reaches all of it: its own colour's and its paint's, the
    // nozzles the layout placed it for.
    let nozzles = part.nozzles();
    for &n in &nozzles {
        if let Some(nz) = machine.nozzles.get(n) {
            if !nz.reach.contains_footprint(&b, EDGE_SLACK) {
                issues.push(Issue::OutOfReach { nozzle: n });
            }
            if height > nz.max_height + EDGE_SLACK && height <= machine.max_height + EDGE_SLACK {
                issues.push(Issue::AboveNozzle { nozzle: n, height, limit: nz.max_height });
            }
        }
    }
    // Not mapped yet: which nozzles could print it, only meaningful while the part is on the
    // bed and within height.
    if nozzles.is_empty() && !issues.iter().any(|i| matches!(i, Issue::OffBed | Issue::TooTall { .. })) {
        let reaching = machine.nozzles_reaching(&b);
        if reaching.is_empty() {
            issues.push(Issue::NoNozzle);
        } else if reaching.len() < machine.nozzles.len() {
            issues.push(Issue::OnlyNozzles { nozzles: reaching });
        }
    }
    issues
}

impl Machine {
    /// The dual-nozzle printer's slots: a unit of four trays on each nozzle (unit B on the
    /// left, unit A on the right) and an external spool on each, on a path of its own.
    pub fn dual_nozzle_slots() -> Vec<Slot> {
        let mut slots = Vec::with_capacity(10);
        for (nozzle, unit) in [(0, 'B'), (1, 'A')] {
            for tray in 1..=4 {
                slots.push(Slot { nozzle, tray: format!("{unit}{tray}"), path: SlotPath::Shared });
            }
            slots.push(Slot { nozzle, tray: "Ext".into(), path: SlotPath::Own });
        }
        slots
    }

    /// A printer profile's material slots (`fab_material_slots`), one entry each:
    /// `<nozzle>:<tray>`, the nozzle counted from 1, then `:own` for a path of its own or
    /// `:dedicated` for one that shares nothing ("1:A1", "2:Ext:dedicated"). An external
    /// spool is on a path of its own unless it says `dedicated`. `nozzles`: the printer's
    /// toolhead count.
    pub fn parse_slots(entries: &[String], nozzles: usize) -> Result<Vec<Slot>, String> {
        let mut slots: Vec<Slot> = Vec::with_capacity(entries.len());
        for e in entries.iter().map(|e| e.trim()).filter(|e| !e.is_empty()) {
            let bad = |why: &str| format!("material slot `{e}`: {why}");
            let f: Vec<&str> = e.split(':').map(str::trim).collect();
            let (n, tray, path) = match f.as_slice() {
                [n, t] => (*n, *t, None),
                [n, t, p] => (*n, *t, Some(*p)),
                _ => return Err(bad("not <nozzle>:<tray>[:own|:dedicated]")),
            };
            let nozzle = n.parse::<usize>().ok().filter(|n| (1..=nozzles).contains(n)).ok_or_else(|| bad(&format!("the nozzle is 1 to {}", nozzles)))? - 1;
            if !tray_label(tray) {
                return Err(bad("the tray is a unit's tray (A1, B3, E1), HT1 to HT126 or Ext"));
            }
            let ext = tray.eq_ignore_ascii_case("ext");
            let path = match path {
                None if ext => SlotPath::Own,
                None => SlotPath::Shared,
                Some("own") => SlotPath::Own,
                Some("dedicated") => SlotPath::Dedicated,
                Some(_) => return Err(bad("the path is own or dedicated")),
            };
            let tray = if ext { "Ext".to_string() } else { tray.to_string() };
            if slots.iter().any(|s| s.nozzle == nozzle && s.tray == tray) {
                return Err(bad("named twice"));
            }
            slots.push(Slot { nozzle, tray, path });
        }
        Ok(slots)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{test_box, test_cube};

    /// Units are lettered past D (E, Z, then AA), and a label reads back to its unit and tray.
    #[test]
    fn tray_labels_letter_any_unit() {
        assert_eq!([0, 3, 4, 8, 25, 26, 27, 127].map(unit_letters), ["A", "D", "E", "I", "Z", "AA", "AB", "DX"]);
        for unit in 0..UNITS {
            assert_eq!(unit_tray(&format!("{}1", unit_letters(unit))), Some((unit, 1)));
        }
        assert_eq!(unit_tray("e2"), Some((4, 2)));
        for bad in ["", "A", "1", "A0", "A+1", "DY1", "A257", "Ext", "A 1"] {
            assert_eq!(unit_tray(bad), None, "{bad:?}");
        }
        assert!(tray_label("I1") && tray_label("HT2") && tray_label("Ext") && !tray_label("HT0"));
    }

    /// A profile's slots: two, four or nine, on one nozzle or two, an external spool on a path of
    /// its own or a dedicated one; a nozzle past the printer's, a label no printer uses, a slot
    /// named twice or an unknown path is refused.
    #[test]
    fn a_profile_names_its_material_slots() {
        let parse = |e: &[&str], n: usize| Machine::parse_slots(&e.iter().map(|s| s.to_string()).collect::<Vec<_>>(), n);
        let two = parse(&["1:A1", "1:Ext:dedicated"], 1).unwrap();
        assert_eq!(two, vec![Slot { nozzle: 0, tray: "A1".into(), path: SlotPath::Shared }, Slot { nozzle: 0, tray: "Ext".into(), path: SlotPath::Dedicated }]);
        let four = parse(&["1:A1", "1:A2", "1:A3", "1:ext"], 1).unwrap();
        assert_eq!(four[3], Slot { nozzle: 0, tray: "Ext".into(), path: SlotPath::Own });
        let nine: Vec<String> = (1..=4).map(|i| format!("1:A{i}")).chain((1..=4).map(|i| format!("2:B{i}"))).chain(["2:Ext:dedicated".to_string()]).collect();
        let nine = Machine::parse_slots(&nine, 2).unwrap();
        assert_eq!((nine.len(), nine.iter().filter(|s| s.nozzle == 1).count()), (9, 5));
        assert_eq!(Machine::dual_nozzle_slots().len(), 10);
        for bad in [&["2:A1"][..], &["1:DY1"], &["1:A1", "1:A1"], &["1:A1:fast"], &["A1"]] {
            assert!(parse(bad, 1).is_err(), "{bad:?}");
        }
    }

    fn part(min: [f64; 3], size: f64, nozzle: Option<usize>) -> Part {
        Part { body_of: None, print: Vec::new(), print_mesh: None, paint: None,
            object: "p".into(),
            name: "p".into(),
            filament: 1,
            color: [1.0; 4],
            nozzle,
            mesh: test_cube(min, size),
            field_bounds: None, field_floor: None,
            ..Default::default()
        }
    }

    #[test]
    fn centred_cube_is_clean() {
        let m = Machine::dual_nozzle_350();
        assert!(check_part(&m, &part([150.0, 150.0, 0.0], 20.0, None)).is_empty());
    }

    #[test]
    fn left_strip_is_left_nozzle_only() {
        let m = Machine::dual_nozzle_350();
        let issues = check_part(&m, &part([5.0, 100.0, 0.0], 10.0, None));
        assert_eq!(issues, vec![Issue::OnlyNozzles { nozzles: vec![0] }]);
        let issues = check_part(&m, &part([5.0, 100.0, 0.0], 10.0, Some(1)));
        assert_eq!(issues, vec![Issue::OutOfReach { nozzle: 1 }]);
    }

    #[test]
    fn right_strip_and_left_height() {
        let m = Machine::dual_nozzle_350();
        // 322 mm tall in the middle: only the right nozzle reaches the top.
        let tall = |nozzle| Part { mesh: test_box([150.0, 100.0, 0.0], [20.0, 20.0, 322.0]), ..part([0.0; 3], 1.0, nozzle) };
        let issues = check_part(&m, &tall(None));
        assert_eq!(issues, vec![Issue::OnlyNozzles { nozzles: vec![1] }]);
        let issues = check_part(&m, &tall(Some(0)));
        assert_eq!(issues, vec![Issue::AboveNozzle { nozzle: 0, height: 322.0, limit: 320.0 }]);
        // Spanning both single-nozzle strips: no single nozzle reaches it.
        let wide = Part { mesh: test_box([10.0, 10.0, 0.0], [330.0, 20.0, 10.0]), ..part([0.0; 3], 1.0, None) };
        assert_eq!(check_part(&m, &wide), vec![Issue::NoNozzle]);
        let too_tall = Part { mesh: test_box([100.0, 10.0, 0.0], [20.0, 20.0, 330.0]), ..part([0.0; 3], 1.0, None) };
        assert!(check_part(&m, &too_tall).contains(&Issue::TooTall { height: 330.0, limit: 325.0 }));
    }

    #[test]
    fn off_bed_below_and_keep_out() {
        let m = Machine::dual_nozzle_350();
        let issues = check_part(&m, &part([345.0, 10.0, -1.0], 10.0, None));
        assert!(issues.contains(&Issue::OffBed));
        assert!(issues.contains(&Issue::BelowPlate { depth: 1.0 }));
        let issues = check_part(&m, &part([150.0, 305.0, 0.0], 10.0, None));
        assert!(issues.contains(&Issue::KeepOut { zone: 0 }), "{issues:?}");
    }

    /// A resin printer limits the screen's area and the height only: no nozzle issue.
    #[test]
    fn a_resin_printer_has_no_nozzle_issues() {
        let screen = ResinScreen { pixels: [2000, 1000], pitch: [0.05, 0.05], mirror: [false; 2], quarter_turns: 0, grey_levels: 256 };
        let m = Machine::resin("resin", screen, 150.0);
        assert_eq!(m.bed, Rect::new(0.0, 0.0, 100.0, 50.0));
        assert!(check_part(&m, &part([10.0, 10.0, 0.0], 20.0, None)).is_empty());
        assert_eq!(check_part(&m, &part([90.0, 10.0, 0.0], 20.0, None)), vec![Issue::OffBed]);
        assert!(!m.same_volume(&Machine::dual_nozzle_350()));
    }

    /// A field part is checked where the layout rests it: its lowest found solid point, its box
    /// reaching lower by what the proof left open.
    #[test]
    fn a_field_part_rests_on_its_floor() {
        let m = Machine::dual_nozzle_350();
        let field = |floor: f64| Part { field_bounds: Some(Bounds { min: [150.0, 150.0, -0.5], max: [170.0, 170.0, 20.0] }), field_floor: Some(crate::FieldFloor::proven(floor)), ..part([150.0, 150.0, 0.0], 20.0, None) };
        assert!(check_part(&m, &field(0.0)).is_empty());
        assert_eq!(check_part(&m, &field(-0.25)), vec![Issue::BelowPlate { depth: 0.25 }]);
        assert_eq!(check_part(&m, &field(0.5)), vec![Issue::Floating { gap: 0.5 }]);
        // A pending floor: no verdict until it is proven, then the proven one, moved with the part.
        let mut part = Part { field_floor: Some(crate::FieldFloor::pending(|| Some(0.25))), ..field(0.0) };
        part.field_floor.as_mut().unwrap().raise(0.5);
        assert!(check_part(&m, &part).is_empty());
        let copy = part.clone();
        part.prove_floor();
        assert_eq!(check_part(&m, &copy), vec![Issue::Floating { gap: 0.75 }], "a copy shares the proof");
    }

    /// A part above the plate that stands on printed material is not floating: its gap waits for
    /// that proof, and floats when the proof finds it held by nothing.
    #[test]
    fn a_part_standing_on_held_material_does_not_float() {
        struct Proof(std::sync::OnceLock<bool>, bool);
        impl crate::Held for Proof {
            fn held(&self) -> bool {
                *self.0.get_or_init(|| self.1)
            }
            fn known(&self) -> Option<bool> {
                self.0.get().copied()
            }
        }
        let m = Machine::dual_nozzle_350();
        let standing = |held: bool| {
            let mut floor = crate::FieldFloor::proven(2.0);
            floor.held_by(std::sync::Arc::new(Proof(std::sync::OnceLock::new(), held)));
            Part { field_bounds: Some(Bounds { min: [150.0, 150.0, 2.0], max: [160.0, 160.0, 8.0] }), field_floor: Some(floor), ..part([150.0, 150.0, 2.0], 6.0, None) }
        };
        for (held, issues) in [(true, vec![]), (false, vec![Issue::Floating { gap: 2.0 }])] {
            let part = standing(held);
            assert!(check_part(&m, &part).is_empty(), "no verdict before its proof");
            part.prove_floor();
            assert_eq!(check_part(&m, &part), issues, "held: {held}");
        }
    }
}
