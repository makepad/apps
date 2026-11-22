//! The comparison: each arm option's time for a part, against Fab's own estimate for the same
//! part on the H2D at its maximum speed. Plastic: every print arm with the chosen head and
//! nozzle, timed by the planner's dry run of Fab's plan for it. Metal: one 850 laying the part
//! with its weld head, and laying and trimming it with the cut. For each the arm's lowest mode
//! where it works, and what limits it: each limit is lifted in turn (the melt flow, the
//! acceleration, the cooling slowdown; a metal step's time taken away) and the one whose lifting
//! saves the most is the limit. A row whose cell cannot reach all the work gives no time (one
//! would leave that work out) but what it cannot reach. Deterministic: the same inputs give the
//! same table.

use crate::fdm::{plan, printer_time, FdmCell, FdmPlan, Part};
use crate::lab::{Cell, Session, Setup};
use crate::printer::{h2d, h2d_lifted, Lift, H2D_FRAME_HZ, H2D_LABEL, SHEET_H2D};
use fab_arms::design::Design;
use fab_arms::fdm::{Fdm, FdmOpts};
use fab_arms::path::Motion;
use fab_arms::planner::{LoopMode, Report};
use fab_arms::route::{DedTrim, DedTrimOpts, TaskKind, BEAM_BLOCKED, CUT_UNREACHED, W_MIN};
use fab_cell_sim::Errors;
use fab_robot::arm::{SheetRow, Tag, ToolKind};
use fab_robot::dynamics::first_mode;
use fab_robot::presets::{FDM_ARMS, HEADS, SHEET_850};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Printer,
    Plastic,
    Metal,
}

impl Group {
    pub fn label(self) -> &'static str {
        match self {
            Group::Printer => "Printer",
            Group::Plastic => "Plastic",
            Group::Metal => "Metal",
        }
    }
}

/// What a job's time is spent against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limit {
    MeltFlow,
    Accel,
    Cooling,
    /// The moves between layers: joint moves, their settle, the setpoint ticks.
    LayerChanges,
    Deposition,
    Scanning,
    Cutting,
    /// Joint moves and head changes between a metal job's tasks.
    Moves,
    /// The positioner's turns and tilts between arcs, their settles included.
    Positioner,
}

impl Limit {
    pub fn label(self) -> &'static str {
        match self {
            Limit::MeltFlow => "melt flow",
            Limit::Accel => "acceleration",
            Limit::Cooling => "layer cooling",
            Limit::LayerChanges => "layer changes",
            Limit::Deposition => "deposition rate",
            Limit::Scanning => "scans",
            Limit::Cutting => "cuts",
            Limit::Moves => "moves, head changes",
            Limit::Positioner => "positioner moves",
        }
    }
}

/// One option in the table.
#[derive(Clone, Debug)]
pub struct Row {
    pub group: Group,
    pub name: String,
    /// The job's time (s), or why there is none (short, for the table; `notes` say more).
    pub time: Result<f64, String>,
    /// The lowest mode where it works (Hz).
    pub mode_hz: f64,
    /// Each limit and the time lifting it saves (s), the largest first: the first is the limit.
    pub weighed: Vec<(Limit, f64)>,
    /// What else to know (the work out of reach, why a time is missing).
    pub notes: Vec<String>,
    /// Where its figures come from.
    pub sheet: Vec<(String, String, Tag)>,
}

impl Row {
    pub fn limit(&self) -> Option<(Limit, f64)> {
        self.weighed.first().copied()
    }
}

/// The table for one part: the H2D first (the reference), then the plastic arms, then metal.
#[derive(Clone, Debug)]
pub struct Table {
    pub part: Part,
    /// The plastic arms' head, as a label.
    pub head: String,
    pub rows: Vec<Row>,
}

impl Table {
    /// The H2D's time (s).
    pub fn h2d(&self) -> Option<f64> {
        self.rows.first().and_then(|r| r.time.as_ref().ok().copied())
    }

    /// A row's time over the H2D's.
    pub fn ratio(&self, row: &Row) -> Option<f64> {
        Some(*row.time.as_ref().ok()? / self.h2d().filter(|t| *t > 0.0)?)
    }
}

/// The plastic arms in the table's order: the 850 on its controller and with its own loop, the
/// IRB 1100, the industrial arm, the desktop QDD.
/// The metal rows.
pub const WELD_ONLY: &str = "One 850: weld only";
pub const WELD_AND_CUT: &str = "One 850: weld, then cut";

pub const PLASTIC_ORDER: [&str; 5] = ["uf850_stream", "uf850_own", "irb1100", "industrial_10kg", "desktop_qdd_754"];

fn static_sheet(rows: &[SheetRow]) -> Vec<(String, String, Tag)> {
    rows.iter().map(|(k, v, t)| (k.to_string(), v.to_string(), *t)).collect()
}

/// Sorted, the largest saving first; savings under a millisecond dropped.
fn rank(mut w: Vec<(Limit, f64)>) -> Vec<(Limit, f64)> {
    w.retain(|x| x.1 > 1e-3);
    w.sort_by(|a, b| b.1.total_cmp(&a.1));
    w
}

/// One independent piece of a table's work: each runs on its own (the app spreads them over
/// its pool) and [`assemble`] puts them together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Work {
    /// Fab's estimate for the H2D, with one of its limits lifted or none.
    H2d(Option<Lift>),
    /// Fab's plan for the head, then every plastic arm's dry run.
    Plastic,
    /// One 850 laying the part, or laying and trimming it.
    Metal { trim: bool },
}

pub const WORK: [Work; 7] = [
    Work::H2d(None),
    Work::H2d(Some(Lift::MeltFlow)),
    Work::H2d(Some(Lift::Accel)),
    Work::H2d(Some(Lift::Cooling)),
    Work::Plastic,
    Work::Metal { trim: false },
    Work::Metal { trim: true },
];

/// What a piece of work brings.
#[derive(Clone, Debug)]
pub enum Done {
    H2d(Option<Lift>, Result<f64, String>),
    Plastic(Vec<Row>),
    Metal(Row),
}

/// Do `work` for `part`, the plastic arms with `head` (index into `HEADS`) at `nozzle` tenths.
pub fn run(work: Work, part: Part, head: u8, nozzle: u8) -> Done {
    let design = part.design();
    match work {
        Work::H2d(lift) => Done::H2d(lift, printer_time(&lift.map_or_else(h2d, h2d_lifted), &design).map(|p| p.model)),
        Work::Plastic => {
            let first = FdmCell { arm: 0, head, nozzle };
            let plan = plan(&h2d(), &design, &first.extruder());
            let row = |id: &&str| {
                let arm = FDM_ARMS.iter().position(|a| a.id == *id).expect("a listed arm") as u8;
                let cell = FdmCell { arm, ..first };
                match &plan {
                    Ok(p) => plastic_row(cell, part, p),
                    Err(e) => Row { group: Group::Plastic, name: FDM_ARMS[arm as usize].label.into(), time: Err("no plan".into()), mode_hz: 0.0, weighed: Vec::new(), notes: vec![format!("Fab could not plan the part for the head: {e}")], sheet: Vec::new() },
                }
            };
            Done::Plastic(PLASTIC_ORDER.iter().map(row).collect())
        }
        Work::Metal { trim } => Done::Metal(metal_row(&design, trim)),
    }
}

/// The table from every piece of [`WORK`] done (in any order): the H2D, the plastic arms, the
/// metal rows weld-only first.
pub fn assemble(part: Part, head: u8, nozzle: u8, done: Vec<Done>) -> Table {
    let mut times = Vec::new();
    let mut plastic = Vec::new();
    let mut metal = Vec::new();
    for d in done {
        match d {
            Done::H2d(lift, t) => times.push((lift, t)),
            Done::Plastic(rows) => plastic = rows,
            Done::Metal(row) => metal.push(row),
        }
    }
    metal.sort_by_key(|r| r.name != WELD_ONLY);
    let mut notes = vec!["Fab's own estimate for the part, printing (no start sequence)".to_string()];
    let time = match times.iter().find(|t| t.0.is_none()) {
        Some((_, Ok(t))) => Ok(*t),
        Some((_, Err(e))) => {
            notes.push(format!("Fab's estimate failed: {e}"));
            Err("no estimate".to_string())
        }
        None => Err("not timed".to_string()),
    };
    let mut weighed = Vec::new();
    if let Ok(base) = time {
        for (lift, limit) in [(Lift::MeltFlow, Limit::MeltFlow), (Lift::Accel, Limit::Accel), (Lift::Cooling, Limit::Cooling)] {
            if let Some((_, Ok(t))) = times.iter().find(|t| t.0 == Some(lift)) {
                weighed.push((limit, base - t));
            }
        }
    }
    let printer = Row { group: Group::Printer, name: H2D_LABEL.into(), time, mode_hz: H2D_FRAME_HZ, weighed: rank(weighed), notes, sheet: static_sheet(SHEET_H2D) };
    let mut rows = vec![printer];
    rows.extend(plastic);
    rows.extend(metal);
    let h = &HEADS[head as usize];
    Table { part, head: format!("{}, {:.1} mm nozzle", lower_first(h.label), nozzle as f64 / 10.0), rows }
}

/// The whole table, its pieces one after another.
/// "Filament hot end" as it reads mid-sentence.
fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_lowercase().chain(c).collect()).unwrap_or_default()
}

pub fn table(part: Part, head: u8, nozzle: u8) -> Table {
    assemble(part, head, nozzle, WORK.iter().map(|w| run(*w, part, head, nozzle)).collect())
}

fn plastic_row(cell: FdmCell, part: Part, p: &FdmPlan) -> Row {
    let a = &FDM_ARMS[cell.arm as usize];
    let setup = Setup { cell: Cell::Fdm(cell), mode: LoopMode::Live, errors: Errors::TYPICAL, seed: 1, max_time: 1.0e6 };
    let mut s = Session::fdm(setup, part, p.clone());
    let total = s.dry_run();
    let mut notes = Vec::new();
    let time = timed(&s.planner.report, total, &format!("the {}", a.label), &mut notes);
    if let Some(low) = s.planner.report.low_line() {
        notes.push(format!("Near a singularity (manipulability under {W_MIN}): {low}."));
    }
    let motion = cell.motion();
    let route = |opts: FdmOpts, m: &Motion| Fdm::new(p.layers.clone(), opts).time(m);
    let base = route(p.opts, &motion);
    let fast = Motion { accel: 1.0e3 * motion.accel, jerk: 1.0e3 * motion.jerk, ..motion };
    let weighed = vec![
        (Limit::MeltFlow, base - route(FdmOpts { flow_max: f64::INFINITY, ..p.opts }, &motion)),
        (Limit::Accel, base - route(p.opts, &fast)),
        (Limit::Cooling, base - route(FdmOpts { min_layer_time: 0.0, ..p.opts }, &motion)),
        (Limit::LayerChanges, total - base),
    ];
    let c = cell.setup();
    let f1 = first_mode(&c.arms[0], &cell.park());
    let e = cell.extruder();
    let mut sheet = static_sheet(a.sheet);
    sheet.extend(static_sheet(HEADS[cell.head as usize].sheet));
    sheet.push(("Print acceleration".into(), format!("{:.2} m/s²: a 0.05 mm ring on its {f1:.1} Hz mode at park", motion.accel / 1e3), Tag::Estimated));
    sheet.push(("Melt flow; layers; lines".into(), format!("{:.0} mm³/s; {:.2} mm; {:.2} mm", e.flow_max, e.layer(), e.line()), Tag::Estimated));
    Row { group: Group::Plastic, name: a.label.into(), time, mode_hz: f1, weighed: rank(weighed), notes, sheet }
}

fn metal_row(design: &Design, trim: bool) -> Row {
    let cell = Cell::Single850;
    let opts = DedTrimOpts { trim, ..DedTrimOpts::default() };
    let setup = Setup { cell, mode: LoopMode::Live, errors: Errors::TYPICAL, seed: 1, max_time: 1.0e6 };
    let mut s = Session::new(setup, design, Box::new(DedTrim::new(design, opts)), None);
    let total = s.dry_run();
    let r = &s.planner.report;
    let rest = total - r.deposit_time - r.scan_time - r.cut_time - r.table_time;
    let mut weighed = vec![(Limit::Deposition, r.deposit_time), (Limit::Scanning, r.scan_time), (Limit::Moves, rest), (Limit::Positioner, r.table_time)];
    if trim {
        weighed.push((Limit::Cutting, r.cut_time));
    }
    let c = cell.setup();
    let f1 = first_mode(&c.arms[0], &cell.park());
    let mut notes = Vec::new();
    let time = timed(r, total, "the one 850", &mut notes);
    if let Some(leans) = leaned(r) {
        let level = if r.leaned[0].0 > 0 { " A leaned bead is taken at the level bead's shape (the bead model has no torch angle)." } else { "" };
        notes.push(format!("Leaned off their axes to keep the wrist clear of its singularity: {leans}.{level}"));
    }
    if let Some(low) = r.low_line() {
        notes.push(format!("Near a singularity (manipulability under {W_MIN}): {low}."));
    }
    if trim {
        // The same job with the table turning only between cuts, for the time the turning saves.
        let mut still = Session::new(setup, design, Box::new(DedTrim::new(design, opts)), None);
        still.planner.set_coordinate(false);
        let between = still.dry_run();
        notes.push(format!("{} cuts run with the table turning under them, the head nearly still: {total:.1} s; with the table turning only between cuts {between:.1} s.", r.coordinated));
    }
    let mut sheet = static_sheet(SHEET_850);
    let wire = c.arms[0].heads.iter().find(|h| h.wire_rate().is_some()).expect("the 850 carries a weld head");
    if let ToolKind::LaserWire { power, rate, .. } = wire.kind {
        sheet.push(("Weld head: power; deposition".into(), format!("{power:.1} kW; {rate:.2} kg/h of steel (0.2-0.5 on a 1-2 kW cobot head)"), Tag::Estimated));
    }
    let speed = opts.deposit_speed(wire).expect("a weld head lays wire");
    sheet.push(("Beads".into(), format!("{:.1} × {:.1} mm, {:.1} mm apart, at {speed:.1} mm/s", opts.bead_width, opts.layer, opts.spacing), Tag::Estimated));
    sheet.push(("Scans".into(), format!("a line scanner at {:.0} mm/s, every {} layers", opts.scan_speed, opts.layers_per_cut), Tag::Estimated));
    if trim {
        let kerf = c.arms[0].heads.iter().find_map(|h| if let ToolKind::LaserCut { kerf, .. } = h.kind { Some(kerf) } else { None }).expect("the 850 carries a cut head");
        sheet.push(("Cut".into(), format!("{:.0} mm/s, a {kerf:.1} mm kerf, the {:.1} mm margin off every {} layers", opts.cut_speed, opts.margin, opts.layers_per_cut), Tag::Estimated));
        sheet.push(("Head change".into(), "0.5 s beam switch".into(), Tag::Estimated));
    }
    let name = if trim { WELD_AND_CUT } else { WELD_ONLY };
    Row { group: Group::Metal, name: name.into(), time, mode_hz: f1, weighed: rank(weighed), notes, sheet }
}

/// The work `r` left undone as out of reach, for the table ("97 cuts, 3 cut points"): the
/// tasks skipped by kind, the cut points no pose or beam served, else the setpoints whose
/// reach failed mid-path; none when all of it was in reach.
pub fn unreached(r: &Report) -> Option<String> {
    let count = |f: fn(&TaskKind) -> bool| r.skipped.iter().filter(|k| f(k)).count();
    let kinds = [
        (count(|k| matches!(k, TaskKind::Deposit { .. })), "deposit", "deposits"),
        (count(|k| matches!(k, TaskKind::Scan)), "scan", "scans"),
        (count(|k| matches!(k, TaskKind::Cut { .. })), "cut", "cuts"),
        (count(|k| matches!(k, TaskKind::Repair(_))), "repair", "repairs"),
    ];
    let points = r.flags.iter().filter(|f| f.1 == CUT_UNREACHED || f.1 == BEAM_BLOCKED).count();
    let mut parts: Vec<String> = kinds.iter().filter(|k| k.0 > 0).map(|&(n, one, many)| format!("{n} {}", if n == 1 { one } else { many })).collect();
    if points > 0 {
        parts.push(format!("{points} cut point{}", if points == 1 { "" } else { "s" }));
    }
    if !parts.is_empty() {
        Some(parts.join(", "))
    } else if r.unreachable > 0 {
        Some(format!("{} setpoints", r.unreachable))
    } else {
        None
    }
}

/// The deposits and scans `r` leaned off their axes ("2 scans, up to 17.5°"), none when it
/// leaned none.
pub fn leaned(r: &Report) -> Option<String> {
    let kinds = [("deposit", "deposits"), ("scan", "scans")];
    let parts: Vec<String> = r.leaned.iter().zip(kinds).filter(|(l, _)| l.0 > 0).map(|(&(n, most), (one, many))| format!("{n} {}, up to {:.1}°", if n == 1 { one } else { many }, most.to_degrees())).collect();
    (!parts.is_empty()).then(|| parts.join("; "))
}

/// A dry run's time, or none when it left work out of reach or ran a drive past its continuous
/// rating (what it could not reach or which joint, and in `notes` why there is no time).
fn timed(r: &Report, total: f64, who: &str, notes: &mut Vec<String>) -> Result<f64, String> {
    let Some(what) = unreached(r) else {
        // A drive past its continuous rating overheats on the job: its time cannot be held.
        return match r.over_rating_line() {
            Some(over) => {
                notes.push(format!("The job's RMS torque is past a joint's continuous rating on {who} ({over}): its drive would overheat, so the {:.0} min it would take is not given as its time.", total / 60.0));
                Err(over)
            }
            None => Ok(total),
        };
    };
    let first = r.flags.first().map_or_else(String::new, |f| format!(", the first at ({:.0}, {:.0}, {:.0}) mm", f.0[0], f.0[1], f.0[2]));
    // Why, by reason in the order they came: out of reach, no clear way, the table blocked.
    let mut why: Vec<(&str, usize)> = Vec::new();
    for f in &r.flags {
        match why.iter_mut().find(|w| w.0 == f.1) {
            Some(w) => w.1 += 1,
            None => why.push((f.1, 1)),
        }
    }
    let why: Vec<String> = why.iter().map(|(w, n)| format!("{w} ({n})")).collect();
    let why = if why.is_empty() { format!("{} setpoints out of reach", r.unreachable) } else { why.join(", ") };
    notes.push(format!("{what} left undone by {who} as it stands: {why}{first}. A time would leave that work out, so none is given; what it does takes {:.0} min. Re-fixturing (a flip or a re-clamp) is not modelled.", total / 60.0));
    Err(format!("left undone: {what}"))
}
