//! The G-code interpreter behind the time model and the preview records.
//!
//! Fab's writer feeds every line it emits (its own moves and the expanded
//! printer templates) through this machine state, in memory, before the
//! file is written: the per-move records, the M73 progress lines and the
//! header totals all come from the same pass.

use crate::record::{MoveKind, MoveRecord, Role};
use crate::time::{make_block, plan, Block, Limits};
use makepad_fab_settings::change::ChangeModel;
use makepad_fab_settings::machine::Job;
use std::collections::{BTreeMap, HashMap};

/// Time charged for machine macros (R1 §10.3 item 9).
#[derive(Clone, Copy, Debug)]
pub struct MacroTimes {
    pub prepare_compensation: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Timeline {
    /// Time at the end of each fed line.
    pub line_end: Vec<f64>,
    /// The records in print order, in parts: a big print's list is never one allocation,
    /// so it never moves while it grows and frees as it is consumed.
    pub parts: Vec<Vec<MoveRecord>>,
    pub total: f64,
    /// Time until the end of the start sequence.
    pub prepare: f64,
    pub role_times: BTreeMap<String, f64>,
    /// (layer index, print z, time spent in the layer).
    pub layer_times: Vec<(u32, f32, f64)>,
    /// Filament used per slot (mm of filament, positive extrusion only).
    pub filament_mm: Vec<f64>,
    pub tool_changes: u32,
    /// Tool changes onto the other nozzle: the head moves, its nozzle may still hold its filament.
    pub head_changes: u32,
    /// Tool changes where a nozzle takes another filament out: the ones that purge.
    pub filament_swaps: u32,
    /// Times a nozzle loads a filament it does not hold (its first one included).
    pub filament_loads: u32,
    /// Filament purged at the changes per slot (mm of filament, inside `filament_mm`): the
    /// printer's change model ([`ChangeModel`]) for every change the G-code makes.
    pub purge_mm: Vec<f64>,
    /// The changes that purge.
    pub purges: u32,
    /// Per slot: the seconds its moves take after the start sequence (purges and the changes'
    /// own time apart).
    pub filament_s: Vec<f64>,
    /// The nozzles' target temperatures a record's `temps` names: one target (°C) per nozzle
    /// in Fab's numbering, each set of targets once.
    pub nozzle_temps: Vec<Vec<i16>>,
    /// Where the print waits for items to be placed into it (`; FAB_PAUSE:`), in print order.
    pub pauses: Vec<Pause>,
}

/// A stop for items placed into the print, as its G-code says it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pause {
    /// The layer it comes before (0-based) and that layer's print z (mm).
    pub layer: u32,
    pub z: f64,
    /// What goes in, by name.
    pub items: Vec<String>,
}

impl Pause {
    /// `layer 3 z 0.6: place M3 nut, Board`, the tag's text.
    pub fn parse(t: &str) -> Option<Pause> {
        let (at, what) = t.split_once(": place ")?;
        let mut w = at.split_whitespace();
        let layer = match (w.next(), w.next()) { (Some("layer"), Some(n)) => n.parse::<u32>().ok()?.checked_sub(1)?, _ => return None };
        let z = match (w.next(), w.next()) { (Some("z"), Some(z)) => z.parse().ok()?, _ => return None };
        Some(Pause { layer, z, items: what.split(", ").map(str::to_string).collect() })
    }
}

impl Timeline {
    /// Every record in print order.
    pub fn moves(&self) -> impl Iterator<Item = &MoveRecord> {
        self.parts.iter().flatten()
    }

    pub fn moves_mut(&mut self) -> impl Iterator<Item = &mut MoveRecord> {
        self.parts.iter_mut().flatten()
    }

    pub fn move_count(&self) -> usize {
        self.parts.iter().map(Vec::len).sum()
    }

    /// One flat list, for tools and tests.
    pub fn flat_moves(&self) -> Vec<MoveRecord> {
        self.moves().copied().collect()
    }

    /// The tool-change rows of the time breakdown that took time, after the features (see
    /// [`HEAD_SWITCH`]).
    pub fn change_rows(&self) -> Vec<ChangeRow> {
        change_rows(&self.role_times, self.head_changes, self.filament_loads, self.purges)
    }
}

/// The time breakdown's row for the head moving onto the other nozzle at a tool change,
/// [`FILAMENT_CHANGE`]'s for a nozzle unloading and loading filament, and [`PURGE`]'s for the
/// filament a change runs through the nozzle. The Sliced legend's feature table and
/// `slice estimate` list them, from [`change_rows`].
pub const HEAD_SWITCH: &str = "Head switch";
/// See [`HEAD_SWITCH`].
pub const FILAMENT_CHANGE: &str = "Filament change";
/// See [`HEAD_SWITCH`].
pub const PURGE: &str = "Purge";

/// A tool-change row of the time breakdown.
#[derive(Clone, Debug, PartialEq)]
pub struct ChangeRow {
    pub name: &'static str,
    pub seconds: f64,
    /// What the time is made of: "119 × 5 s".
    pub detail: String,
}

/// The rows of [`HEAD_SWITCH`], [`FILAMENT_CHANGE`] and [`PURGE`] in `role_times` that took
/// time, with their counts.
pub fn change_rows(role_times: &BTreeMap<String, f64>, head_changes: u32, filament_loads: u32, purges: u32) -> Vec<ChangeRow> {
    [(HEAD_SWITCH, head_changes), (FILAMENT_CHANGE, filament_loads), (PURGE, purges)]
        .into_iter()
        .filter_map(|(name, n)| {
            let seconds = role_times.get(name).copied().filter(|t| *t > 0.0 && n > 0)?;
            let each = seconds / n as f64;
            let each = if (each - each.round()).abs() < 0.05 { format!("{each:.0} s") } else { format!("{each:.1} s") };
            Some(ChangeRow { name, seconds, detail: format!("{n} × {each}") })
        })
        .collect()
}

/// The time a chamber heat-up wait (`M191 S<t>`) costs.
pub const CHAMBER_WAIT_S: f64 = 720.0;

/// Moves the time model queues before it plans them.
const MAX_RUN: usize = 1 << 17;

/// Records in a part of [`Timeline::parts`]: every part holds this many but the last.
const PART_MOVES: usize = 1 << 16;

#[derive(Clone, Copy)]
struct Pending {
    block: Block,
    line: u32,
    rec: u32,
}

/// Where a model's parts of [`Timeline::parts`] go as soon as every record in them is planned
/// ([`Processor::stream_to`]), in print order and on the print's clock. Their buffers come back
/// as `send`'s return value to be filled again.
pub struct PartStream {
    /// Takes each part, with the entries of the nozzles' targets (`Timeline::nozzle_temps`) made
    /// since the part before it: its records' `temps` name the entries so far. Returns the
    /// part's buffer when done with it, or `None` when it keeps it.
    pub send: Box<dyn FnMut(Vec<MoveRecord>, Vec<Vec<i16>>) -> Option<Vec<MoveRecord>> + Send>,
}

/// What [`Processor::finish`] makes of the records part by part, in print order, wherever a
/// part is settled (streamed as it is final, or at the end): the same sums in the same order.
#[derive(Default)]
struct Settle {
    /// Parts settled so far.
    parts: usize,
    /// Entries of the nozzles' targets sent with them.
    temps: usize,
    /// The print's clock at the end of the records settled.
    t: f64,
    /// Each role's moves' time: the role's charges (`role_times`) go on top at the end.
    roles: Vec<(&'static str, f64)>,
    /// The layer being summed, and the layers summed before it.
    layer: Option<(u32, (f32, f64))>,
    layers: BTreeMap<u32, (f32, f64)>,
}

impl Settle {
    /// The records' times made absolute (they hold their durations until now), summed by
    /// filament after the start (`prepare`), role and layer.
    fn part(&mut self, part: &mut [MoveRecord], prepare: f64, filament_s: &mut [f64]) {
        for r in part {
            let d = r.t_end;
            r.t_start = self.t;
            self.t += d;
            r.t_end = self.t;
            if r.kind != MoveKind::FirmwareMacro {
                if r.filament > 0 && r.t_start >= prepare {
                    if let Some(s) = filament_s.get_mut(r.filament as usize - 1) {
                        *s += d;
                    }
                }
                let name: &'static str = if r.kind == MoveKind::Extrude { r.role.name() } else { "Travel" };
                match self.roles.iter_mut().find(|e| std::ptr::eq(e.0, name) || e.0 == name) {
                    Some(e) => e.1 += d,
                    None => self.roles.push((name, d)),
                }
            }
            match &mut self.layer {
                Some((l, e)) if *l == r.layer => e.1 += d,
                _ => {
                    if let Some((l, e)) = self.layer.take() {
                        self.layers.insert(l, e);
                    }
                    let mut e = self.layers.get(&r.layer).copied().unwrap_or((r.z_layer, 0.0));
                    e.1 += d;
                    self.layer = Some((r.layer, e));
                }
            }
        }
        self.parts += 1;
    }
}

pub struct Processor {
    /// The records since the last whole part. `Pending::rec` numbers a record in the whole
    /// timeline: record `i` is in part `i / PART_MOVES`, this one if the timeline has no such part.
    part: Vec<MoveRecord>,
    lim: Vec<Limits>,
    mt: MacroTimes,
    // Machine state.
    pos: [f64; 4],
    abs_xyz: bool,
    abs_e: bool,
    feed: f64,
    accel_print: f64,
    accel_travel: f64,
    accel_retract: f64,
    filament: i32,
    extruder: usize,
    fil_ext: Vec<usize>,
    role: Role,
    width: f32,
    height: f32,
    /// The print layer, 0 the first (the start G-code's moves are on it too), as the
    /// toolpaths and the G-code's `total layer number` count them.
    layer: u32,
    /// A `CHANGE_LAYER` was read: the next one starts the layer after `layer`.
    in_layer: bool,
    z_layer: f32,
    object: u16,
    /// Inside a block printed after its layer (`; BLOCK: n` .. `; BLOCK_END`).
    block: u16,
    /// The experimental mode (`; FAB_MODE: n`).
    mode: u8,
    /// Inside a closed-in fill (`; FAB_CORE: 1`), until the next `; FEATURE:` or `; FAB_CORE: 0`.
    core: bool,
    /// Object label ids by plate part index (records carry the index).
    labels: Vec<u32>,
    fan: [u8; 3],
    /// The nozzles' targets now, and their entry in the timeline's table.
    t_nozzle: Vec<i16>,
    temps: u32,
    temp_entries: HashMap<Vec<i16>, u32>,
    t_bed: i16,
    in_wipe: bool,
    /// Between `; VFLUSH_START` and `; VFLUSH_END`: a template's own account of its change's
    /// purge, which the change model has counted already.
    in_flush: bool,
    /// The next change's purge the G-code prints into paths that don't show (`; FLUSH_INTO:`,
    /// mm³): the printer purges the rest.
    flush_into: f64,
    changes: ChangeModel,
    fil_area: Vec<f64>,
    in_custom: bool,
    in_j1: bool,
    /// Inside an `M622` block the template's `M622.1 S0` defaulted off: its
    /// waits are not timed (the printer skips the block by default).
    in_j_off: bool,
    /// The current condition (`M1002 judge_flag`) defaults off.
    j_default_off: bool,
    /// An `M622.1 S0` names the default of the next condition.
    j_pending_off: Option<bool>,
    start_done: bool,
    // Planning.
    run: Vec<Pending>,
    /// The run's blocks as the planner takes them: one buffer for every run.
    blocks: Vec<Block>,
    /// Each fed line's time so far (s), by line: its planned moves' and its charges'.
    line_s: Vec<f64>,
    /// Those times summed in the order they came.
    timed_s: f64,
    out: Timeline,
    lines: u32,
    phys_map: Vec<usize>,
    /// The filament in each physical extruder (-1 = unknown).
    loaded: Vec<i32>,
    /// Where final parts go ([`Processor::stream_to`]); none: they stay in the timeline.
    stream: Option<PartStream>,
    /// The buffer returned by the last `PartStream::send`, to hold a later part.
    spare: Option<Vec<MoveRecord>>,
    settle: Settle,
}

/// Adds `t` (s) to line `line`'s time and to the sum of every time so far.
fn add_time(line_s: &mut Vec<f64>, sum: &mut f64, line: u32, t: f64) {
    let line = line as usize;
    if line_s.len() <= line {
        line_s.resize(line + 1, 0.0);
    }
    line_s[line] += t;
    *sum += t;
}

fn parse_word(rest: &str, c: char) -> Option<f64> {
    if rest.is_ascii() && c.is_ascii() {
        // The same words as `split_whitespace` on ASCII text.
        for w in rest.as_bytes().split(|b| b.is_ascii_whitespace() || *b == 0x0b) {
            if w.first() == Some(&(c as u8)) {
                // A word of ASCII bytes is valid UTF-8.
                if let Some(f) = crate::fmt::parse_f64(std::str::from_utf8(&w[1..]).unwrap_or("")) {
                    return Some(f);
                }
            }
        }
        return None;
    }
    for w in rest.split_whitespace() {
        let mut ch = w.chars();
        if ch.next() == Some(c) {
            let v = ch.as_str();
            if let Ok(f) = v.parse::<f64>() {
                return Some(f);
            }
        }
    }
    None
}

/// The motion words of one line (X Y Z E F I J P), read in one pass: for
/// each letter, the first word starting with it that parses, as
/// [`parse_word`] finds it.
#[derive(Clone, Copy)]
pub(crate) struct Words {
    v: [f64; 8],
    set: u8,
}

const LETTERS: &[u8; 8] = b"XYZEFIJP";

#[inline]
fn slot(c: u8) -> Option<usize> {
    match c {
        b'X' => Some(0),
        b'Y' => Some(1),
        b'Z' => Some(2),
        b'E' => Some(3),
        b'F' => Some(4),
        b'I' => Some(5),
        b'J' => Some(6),
        b'P' => Some(7),
        _ => None,
    }
}

impl Words {
    /// The words of a line the writer formats as `K<v>` with `d` decimals, as reading that
    /// text back gives them ([`crate::fmt::rounded`]), without the text.
    pub(crate) fn of_values(words: &[(&str, f64, usize)]) -> Words {
        let mut w = Words { v: [0.0; 8], set: 0 };
        for &(key, v, d) in words {
            let Some(k) = key.as_bytes().first().and_then(|&c| slot(c)) else { continue };
            if w.set & (1 << k) != 0 {
                continue;
            }
            // (a word written `E-` with its length reads back negative)
            let sign = if key.ends_with('-') { -1.0 } else { 1.0 };
            w.v[k] = sign * crate::fmt::rounded(v, d);
            w.set |= 1 << k;
        }
        w
    }

    pub(crate) fn of(rest: &str) -> Words {
        let mut w = Words { v: [0.0; 8], set: 0 };
        if !rest.is_ascii() {
            for (k, &c) in LETTERS.iter().enumerate() {
                if let Some(f) = parse_word(rest, c as char) {
                    w.v[k] = f;
                    w.set |= 1 << k;
                }
            }
            return w;
        }
        for t in rest.as_bytes().split(|b| b.is_ascii_whitespace() || *b == 0x0b) {
            let Some(k) = t.first().and_then(|&c| slot(c)) else { continue };
            if w.set & (1 << k) != 0 {
                continue;
            }
            // A word of ASCII bytes is valid UTF-8.
            if let Some(f) = crate::fmt::parse_f64(std::str::from_utf8(&t[1..]).unwrap_or("")) {
                w.v[k] = f;
                w.set |= 1 << k;
            }
        }
        w
    }
    #[inline]
    fn get(&self, c: u8) -> Option<f64> {
        let k = slot(c)?;
        (self.set & (1 << k) != 0).then_some(self.v[k])
    }
}

/// One line as the interpreter takes it, read apart from the machine (it
/// depends only on the text): a motion command with its words, or any other
/// line, interpreted from its text.
#[derive(Clone, Copy)]
pub(crate) enum Op {
    /// G0/G1 (`arc` false) or G2 (`cw`) / G3.
    Move { arc: bool, cw: bool, w: Words },
    /// Bytes `start..end` of the chunk.
    Text { start: u32, end: u32 },
}

/// Every line of `chunk` (newline-terminated lines, as the writer sends
/// them) as an [`Op`], in order.
pub(crate) fn parse_chunk(chunk: &str, out: &mut Vec<Op>) {
    let mut at = 0usize;
    for l in chunk.split_terminator('\n') {
        let start = at;
        at += l.len() + 1;
        let t = l.trim();
        if !t.starts_with('G') {
            out.push(Op::Text { start: start as u32, end: (start + l.len()) as u32 });
            continue;
        }
        // As `handle` reads a command line.
        let code = t.split(';').next().unwrap_or("").trim();
        let (cmd, rest) = code.split_once(char::is_whitespace).unwrap_or((code, ""));
        let (arc, cw) = match cmd {
            "G0" | "G1" => (false, false),
            "G2" => (true, true),
            "G3" => (true, false),
            _ => {
                out.push(Op::Text { start: start as u32, end: (start + l.len()) as u32 });
                continue;
            }
        };
        out.push(Op::Move { arc, cw, w: Words::of(rest) });
    }
}

/// A machine limit of 0 (or less) is no limit.
fn unlimited_if_unset(v: f64) -> f64 {
    if v > 0.0 {
        v
    } else {
        f64::INFINITY
    }
}

impl Processor {
    pub fn new(job: &Job) -> Self {
        let cfg = &job.cfg;
        let ne = job.extruders();
        let mut lim = Vec::new();
        for e in 0..ne {
            // Machine limits carry (normal, silent) per extruder.
            // A limit the machine profile leaves out (or sets to 0) does not
            // limit.
            let k = |key: &str| unlimited_if_unset(cfg.f64_at(key, 2 * e));
            lim.push(Limits {
                max_speed: [k("machine_max_speed_x"), k("machine_max_speed_y"), k("machine_max_speed_z"), k("machine_max_speed_e")],
                max_accel: [k("machine_max_acceleration_x"), k("machine_max_acceleration_y"), k("machine_max_acceleration_z"), k("machine_max_acceleration_e")],
                max_jerk: [k("machine_max_jerk_x"), k("machine_max_jerk_y"), k("machine_max_jerk_z"), k("machine_max_jerk_e")],
                max_accel_extruding: k("machine_max_acceleration_extruding"),
                max_accel_retracting: k("machine_max_acceleration_retracting"),
                max_accel_travel: k("machine_max_acceleration_travel"),
                min_extruding_rate: cfg.f64_at("machine_min_extruding_rate", e),
                min_travel_rate: cfg.f64_at("machine_min_travel_rate", e),
            });
        }
        let mt = MacroTimes { prepare_compensation: cfg.f64("machine_prepare_compensation_time") };
        let nf = job.filaments();
        Self {
            lim,
            mt,
            pos: [0.0; 4],
            abs_xyz: true,
            abs_e: true,
            feed: 50.0,
            accel_print: 1500.0,
            accel_travel: 1500.0,
            accel_retract: 1250.0,
            filament: -1,
            extruder: 0,
            fil_ext: job.fil_ext.clone(),
            role: Role::Custom,
            width: 0.0,
            height: 0.0,
            layer: 0,
            in_layer: false,
            z_layer: 0.0,
            object: u16::MAX,
            block: 0,
            mode: 0,
            core: false,
            labels: Vec::new(),
            fan: [0; 3],
            t_nozzle: vec![0; ne.max(1)],
            temps: 0,
            temp_entries: HashMap::from([(vec![0; ne.max(1)], 0)]),
            t_bed: 0,
            in_wipe: false,
            in_flush: false,
            flush_into: 0.0,
            changes: ChangeModel::new(job),
            fil_area: (0..nf.max(1)).map(|f| job.filament_area(f)).collect(),
            in_custom: true,
            in_j1: false,
            in_j_off: false,
            j_default_off: false,
            j_pending_off: None,
            start_done: false,
            run: Vec::new(),
            blocks: Vec::new(),
            line_s: Vec::new(),
            timed_s: 0.0,
            out: Timeline { filament_mm: vec![0.0; nf.max(1)], purge_mm: vec![0.0; nf.max(1)], filament_s: vec![0.0; nf.max(1)], nozzle_temps: vec![vec![0; ne.max(1)]], ..Default::default() },
            part: Vec::with_capacity(PART_MOVES),
            lines: 0,
            phys_map: (0..ne).map(|e| job.physical_index(e)).collect(),
            loaded: vec![-1; ne.max(1)],
            stream: None,
            spare: None,
            settle: Settle::default(),
        }
    }

    /// Each part of the timeline goes to `stream` as soon as it is final (every record in it
    /// planned, the start's end known), settled as [`Processor::finish`] settles the rest: the
    /// timeline's parts are then empty. Its buffers come back as `stream.send`'s return value
    /// to hold later parts.
    pub fn stream_to(&mut self, stream: PartStream) {
        self.stream = Some(stream);
    }

    /// Sends the parts that are final ([`Processor::stream_to`]): the run holds none of their
    /// records, and the time the start takes (`prepare`, which the filaments' times start from)
    /// is known. Each buffer comes back as `send`'s return value when its taker is done with it.
    fn release(&mut self) {
        let Some(stream) = &mut self.stream else { return };
        if !self.start_done {
            return;
        }
        let planned = self.run.first().map_or(usize::MAX, |p| p.rec as usize);
        while self.settle.parts < self.out.parts.len() && (self.settle.parts + 1) * PART_MOVES <= planned {
            let mut part = std::mem::take(&mut self.out.parts[self.settle.parts]);
            self.settle.part(&mut part, self.out.prepare, &mut self.out.filament_s);
            let temps = self.out.nozzle_temps[self.settle.temps..].to_vec();
            self.settle.temps = self.out.nozzle_temps.len();
            self.spare = (stream.send)(part, temps);
        }
    }

    /// The object label ids in plate part order, so records name the part
    /// (`MoveRecord::object`). Without them, ids number in order of first
    /// appearance.
    pub fn set_object_labels(&mut self, ids: &[u32]) {
        self.labels = ids.to_vec();
    }

    fn object_index(&mut self, id: u32) -> u16 {
        match self.labels.iter().position(|&l| l == id) {
            Some(i) => i as u16,
            None => {
                self.labels.push(id);
                (self.labels.len() - 1) as u16
            }
        }
    }

    /// The motion limits of the active extruder.
    pub fn limits(&self) -> Limits {
        *self.lim()
    }

    fn lim(&self) -> &Limits {
        &self.lim[self.extruder.min(self.lim.len() - 1)]
    }

    /// Synchronise: plan the queued motion (it ends at rest).
    fn barrier(&mut self) {
        if self.run.is_empty() {
            return;
        }
        let lim = *self.lim();
        self.blocks.clear();
        self.blocks.extend(self.run.iter().map(|p| p.block));
        { let _prof = makepad_fab_toolpath::prof::scope(makepad_fab_toolpath::prof::Stage::GcodeModelPlan); plan(&lim, &mut self.blocks); }
        for (p, b) in self.run.iter().zip(&self.blocks) {
            add_time(&mut self.line_s, &mut self.timed_s, p.line, b.time);
            let i = p.rec as usize;
            let r = match self.out.parts.get_mut(i / PART_MOVES) {
                Some(part) => &mut part[i % PART_MOVES],
                None => &mut self.part[i % PART_MOVES],
            };
            r.v_entry = b.v_entry as f32;
            r.v_cruise = b.v_cruise as f32;
            r.v_exit = b.v_exit as f32;
            r.accel = b.accel as f32;
            r.t_end = b.time; // duration for now; made absolute as the part is settled
        }
        self.run.clear();
        self.release();
    }

    /// Keeps `r` and gives its number in the timeline ([`Processor::part`]): a full part goes to
    /// the timeline before it. A buffer returned by `send` is cleared and filled again.
    fn push_record(&mut self, r: MoveRecord) -> u32 {
        if self.part.len() == PART_MOVES {
            let spare = self.spare.take().map(|mut v| {
                v.clear();
                v
            });
            let full = std::mem::replace(&mut self.part, spare.unwrap_or_else(|| Vec::with_capacity(PART_MOVES)));
            self.out.parts.push(full);
            self.release();
        }
        self.part.push(r);
        (self.out.parts.len() * PART_MOVES + self.part.len() - 1) as u32
    }

    /// A print can go millions of moves without a stop: the queue is planned at a length, one
    /// stop in [`MAX_RUN`] moves in the model's clock, so its memory stays small.
    fn bound_run(&mut self) {
        if self.run.len() >= MAX_RUN {
            self.barrier();
        }
    }

    fn charge(&mut self, t: f64, role: &str) {
        if t <= 0.0 {
            return;
        }
        self.barrier();
        add_time(&mut self.line_s, &mut self.timed_s, self.lines, t);
        *self.out.role_times.entry(role.to_string()).or_insert(0.0) += t;
        let mut r = self.blank_record(MoveKind::FirmwareMacro);
        r.t_end = t;
        r.gcode_line = self.lines;
        self.push_record(r);
    }

    fn blank_record(&self, kind: MoveKind) -> MoveRecord {
        let p = [self.pos[0] as f32, self.pos[1] as f32, self.pos[2] as f32];
        MoveRecord {
            kind,
            role: self.role,
            p0: p,
            p1: p,
            tool: self.phys_map.get(self.extruder).copied().unwrap_or(self.extruder) as u8,
            nozzle: self.extruder as u8,
            filament: (self.filament + 1).max(0) as u8,
            width: self.width,
            height: self.height,
            fan_part: self.fan[0],
            fan_aux: self.fan[1],
            fan_chamber: self.fan[2],
            temps: self.temps,
            t_bed: self.t_bed,
            layer: self.layer,
            z_layer: self.z_layer,
            object: self.object,
            gcode_line: self.lines,
            block: self.block,
            mode: self.mode,
            flags: if self.core && kind == MoveKind::Extrude { crate::record::flags::CORE } else { 0 },
            ..Default::default()
        }
    }

    fn motion(&mut self, target: [f64; 4], feed: f64) {
        let mut delta = [0.0; 4];
        for (a, d) in delta.iter_mut().enumerate() {
            *d = target[a] - self.pos[a];
        }
        let lim = *self.lim();
        let Some(mut b) = make_block(&lim, delta, feed, self.accel_print, self.accel_travel, self.accel_retract) else {
            self.pos = target;
            return;
        };
        b.stop_before = self.run.is_empty();
        let xyz = (delta[0] * delta[0] + delta[1] * delta[1] + delta[2] * delta[2]).sqrt();
        let kind = if self.in_wipe {
            MoveKind::Wipe
        } else if delta[3] < -1e-9 && xyz > 1e-9 {
            MoveKind::Travel
        } else if delta[3] < -1e-9 {
            MoveKind::Retract
        } else if delta[3] > 1e-9 && (delta[0] != 0.0 || delta[1] != 0.0) {
            MoveKind::Extrude
        } else if delta[3] > 1e-9 && xyz <= 1e-9 {
            MoveKind::Unretract
        } else {
            MoveKind::Travel
        };
        let mut r = self.blank_record(kind);
        r.p1 = [target[0] as f32, target[1] as f32, target[2] as f32];
        r.e = delta[3] as f32;
        r.f_cmd = feed as f32;
        if kind == MoveKind::Extrude && self.filament >= 0 {
            if let Some(m) = self.out.filament_mm.get_mut(self.filament as usize) {
                *m += delta[3];
            }
        }
        if kind == MoveKind::Extrude && delta[2].abs() > 1e-7 {
            r.flags |= crate::record::flags::SLOPED;
        }
        self.bound_run();
        let rec = self.push_record(r);
        self.run.push(Pending { block: b, line: self.lines, rec });
        self.pos = target;
    }

    fn arc(&mut self, w: &Words, cw: bool) {
        let i = w.get(b'I').unwrap_or(0.0);
        let j = w.get(b'J').unwrap_or(0.0);
        let full = w.get(b'P').map(|p| p >= 1.0).unwrap_or(false);
        let mut end = self.pos;
        if let Some(x) = w.get(b'X') {
            end[0] = if self.abs_xyz { x } else { self.pos[0] + x };
        }
        if let Some(y) = w.get(b'Y') {
            end[1] = if self.abs_xyz { y } else { self.pos[1] + y };
        }
        if let Some(z) = w.get(b'Z') {
            end[2] = if self.abs_xyz { z } else { self.pos[2] + z };
        }
        if let Some(e) = w.get(b'E') {
            end[3] = if self.abs_e { e } else { self.pos[3] + e };
        }
        if let Some(f) = w.get(b'F') {
            self.feed = f / 60.0;
        }
        let c = [self.pos[0] + i, self.pos[1] + j];
        let r = (i * i + j * j).sqrt();
        if r <= 1e-9 {
            self.motion(end, self.feed);
            return;
        }
        let a0 = (self.pos[1] - c[1]).atan2(self.pos[0] - c[0]);
        let a1 = (end[1] - c[1]).atan2(end[0] - c[0]);
        let mut sweep = a1 - a0;
        if cw {
            while sweep >= 0.0 {
                sweep -= std::f64::consts::TAU;
            }
        } else {
            while sweep <= 0.0 {
                sweep += std::f64::consts::TAU;
            }
        }
        if full && (end[0] - self.pos[0]).abs() < 1e-6 && (end[1] - self.pos[1]).abs() < 1e-6 {
            sweep = if cw { -std::f64::consts::TAU } else { std::f64::consts::TAU };
        } else if !full && (end[0] - self.pos[0]).abs() < 1e-9 && (end[1] - self.pos[1]).abs() < 1e-9 {
            sweep = if cw { -std::f64::consts::TAU } else { std::f64::consts::TAU };
        }
        // One block for the whole arc (its length along the curve), with the
        // start and end tangents for the junctions on either side.
        let len_xy = sweep.abs() * r;
        let dz = end[2] - self.pos[2];
        let de = end[3] - self.pos[3];
        let len = (len_xy * len_xy + dz * dz).sqrt();
        let tangent = |a: f64| -> [f64; 2] {
            let (s_, c_) = a.sin_cos();
            if cw {
                [s_, -c_]
            } else {
                [-s_, c_]
            }
        };
        let t0 = tangent(a0);
        let t1 = tangent(a0 + sweep);
        let lim = *self.lim();
        let fake = [t0[0] * len_xy, t0[1] * len_xy, dz, de];
        let Some(mut b) = make_block(&lim, fake, self.feed, self.accel_print, self.accel_travel, self.accel_retract) else {
            self.pos = end;
            return;
        };
        let lt = len.max(1e-9);
        let uin = [t0[0] * len_xy / lt, t0[1] * len_xy / lt, dz / lt, de / lt];
        let uout = [t1[0] * len_xy / lt, t1[1] * len_xy / lt, dz / lt, de / lt];
        b.dist = len;
        b.arc = Some((uin, uout));
        b.stop_before = self.run.is_empty();
        let kind = if de > 1e-9 { MoveKind::Extrude } else if self.in_wipe { MoveKind::Wipe } else if dz.abs() > 1e-9 && de.abs() <= 1e-9 { MoveKind::ZHop } else { MoveKind::Travel };
        let mut rec = self.blank_record(kind);
        rec.p1 = [end[0] as f32, end[1] as f32, end[2] as f32];
        rec.e = de as f32;
        rec.f_cmd = self.feed as f32;
        rec.center = [c[0] as f32, c[1] as f32];
        rec.sweep = sweep as f32;
        rec.flags |= crate::record::flags::ARC;
        if kind == MoveKind::Extrude && self.filament >= 0 {
            if let Some(m) = self.out.filament_mm.get_mut(self.filament as usize) {
                *m += de;
            }
        }
        self.bound_run();
        let ri = self.push_record(rec);
        self.run.push(Pending { block: b, line: self.lines, rec: ri });
        self.pos = end;
    }

    /// Interpret one line of G-code.
    pub fn line(&mut self, raw: &str) {
        self.handle(raw.trim());
        self.lines += 1;
    }

    /// Interpret one line read ahead by [`parse_chunk`] (`chunk` is its text).
    pub(crate) fn op(&mut self, op: &Op, chunk: &str) {
        match op {
            Op::Move { arc: false, w, .. } => self.g1(w),
            Op::Move { arc: true, cw, w } => self.arc(w, *cw),
            Op::Text { start, end } => self.handle(chunk[*start as usize..*end as usize].trim()),
        }
        self.lines += 1;
    }

    /// `M204` from its S P R T words.
    fn accel(&mut self, w: &[Option<f64>; 4]) {
        if let Some(s) = w[0] {
            self.accel_print = s;
            self.accel_travel = s;
        }
        if let Some(p) = w[1] {
            self.accel_print = p;
        }
        if let Some(r) = w[2] {
            self.accel_retract = r;
        }
        if let Some(t) = w[3] {
            self.accel_travel = t;
        }
    }

    fn g1(&mut self, w: &Words) {
        let mut t = self.pos;
        let mut any = false;
        for (k, ax) in [b'X', b'Y', b'Z'].iter().enumerate() {
            if let Some(v) = w.get(*ax) {
                t[k] = if self.abs_xyz { v } else { self.pos[k] + v };
                any = true;
            }
        }
        if let Some(e) = w.get(b'E') {
            t[3] = if self.abs_e { e } else { self.pos[3] + e };
            any = true;
        }
        if let Some(f) = w.get(b'F') {
            self.feed = f / 60.0;
        }
        if any {
            self.motion(t, self.feed);
        }
    }

    fn tag(&mut self, t: &str) {
        if let Some(r) = t.strip_prefix("FEATURE: ") {
            self.role = Role::from_name(r.trim());
            self.core = false;
            self.in_custom = self.role == Role::Custom;
            // Plate-level features belong to no part.
            if matches!(self.role, Role::PrimeTower | Role::Flush | Role::Custom | Role::Skirt) {
                self.object = u16::MAX;
            }
        } else if let Some(r) = t.strip_prefix("LINE_WIDTH: ") {
            self.width = r.trim().parse().unwrap_or(self.width);
        } else if let Some(r) = t.strip_prefix("LAYER_HEIGHT: ") {
            self.height = r.trim().parse().unwrap_or(self.height);
        } else if let Some(r) = t.strip_prefix("Z_HEIGHT: ") {
            self.z_layer = r.trim().parse().unwrap_or(self.z_layer);
        } else if t == "CHANGE_LAYER" {
            self.layer += self.in_layer as u32;
            self.in_layer = true;
            self.block = 0;
        } else if let Some(r) = t.strip_prefix("BLOCK: ") {
            self.block = r.trim().parse().unwrap_or(0);
        } else if t == "BLOCK_END" {
            self.block = 0;
        } else if let Some(r) = t.strip_prefix("FAB_MODE: ") {
            self.mode = r.trim().parse().unwrap_or(0);
        } else if let Some(r) = t.strip_prefix("FAB_CORE: ") {
            self.core = r.trim() == "1";
        } else if let Some(r) = t.strip_prefix("FLUSH_INTO: ") {
            self.flush_into = r.trim().parse().unwrap_or(0.0);
        } else if let Some(r) = t.strip_prefix("FAB_PAUSE: ") {
            // The print waits here for items to be placed: a mark at the parked head, of no
            // time the model can know.
            self.barrier();
            let record = self.blank_record(MoveKind::Pause);
            self.push_record(record);
            self.out.pauses.extend(Pause::parse(r));
        } else if t == "WIPE_START" {
            self.in_wipe = true;
        } else if t == "WIPE_END" {
            self.in_wipe = false;
        } else if t == "MACHINE_START_GCODE_END" {
            self.barrier();
            debug_assert!(self.settle.parts == 0, "the start's end is known before a part is settled");
            self.start_done = true;
            self.out.prepare = self.timed_s;
        } else if let Some(r) = t.strip_prefix("OBJECT_ID: ") {
            if let Ok(id) = r.trim().parse() {
                self.object = self.object_index(id);
            }
        } else if t.starts_with("stop printing object, unique label id: ") {
            self.object = u16::MAX;
        } else if t.starts_with("start printing object, unique label id: ") {
            if let Some(id) = t.rsplit(' ').next().and_then(|s| s.parse().ok()) {
                self.object = self.object_index(id);
            }
        }
    }

    fn handle(&mut self, l: &str) {
        if l.is_empty() {
            return;
        }
        if let Some(c) = l.strip_prefix(';') {
            let c = c.trim_start();
            if c == "VFLUSH_START" || c == "VFLUSH_END" {
                self.in_flush = c == "VFLUSH_START";
                return;
            }
            if (c.starts_with("VG") || c.starts_with("VM")) && !c.starts_with("VT") {
                // A virtual command from a printer template: it counts for the
                // time model (heating) but is a comment to the printer. Its
                // flush extrusion is the change model's.
                if self.in_flush && c.starts_with("VG") {
                    return;
                }
                let role = self.role;
                self.role = Role::Flush;
                self.handle(&c[1..]);
                self.role = role;
                return;
            }
            if let Some(v) = c.strip_prefix("VT") {
                // A virtual tool change (state only).
                if let Some(n) = v.split_whitespace().next().and_then(|s| s.parse::<i32>().ok()) {
                    self.set_filament(n, false);
                }
            } else {
                self.tag(c);
            }
            return;
        }
        let code = l.split(';').next().unwrap_or("").trim();
        if code.is_empty() {
            return;
        }
        let (cmd, rest) = code.split_once(char::is_whitespace).unwrap_or((code, ""));
        match cmd {
            "G0" | "G1" => self.g1(&Words::of(rest)),
            "G2" | "G3" => self.arc(&Words::of(rest), cmd == "G2"),
            "G4" => {
                let t = if self.in_j_off { 0.0 } else { parse_word(rest, 'S').unwrap_or(0.0) + parse_word(rest, 'P').unwrap_or(0.0) / 1000.0 };
                let r = self.role.name().to_string();
                self.charge(t, &r);
                self.barrier();
            }
            "M191" => {
                // Waiting for the chamber to heat: the printer's slicer charges a fixed
                // twelve minutes for it, whatever the target (measured: 45 °C and 60 °C alike).
                if !self.in_j_off && parse_word(rest, 'S').is_some_and(|s| s > 0.0) {
                    let r = self.role.name().to_string();
                    self.charge(CHAMBER_WAIT_S, &r);
                }
            }
            "G90" => self.abs_xyz = true,
            "G91" => self.abs_xyz = false,
            "M82" => self.abs_e = true,
            "M83" => self.abs_e = false,
            "G92" => {
                for (k, ax) in ['X', 'Y', 'Z', 'E'].iter().enumerate() {
                    if let Some(v) = parse_word(rest, *ax) {
                        self.pos[k] = v;
                    }
                }
            }
            "G28" => {
                self.barrier();
            }
            "G29" => {
                self.barrier();
                if self.in_j1 && !self.in_j_off {
                    let t = self.mt.prepare_compensation;
                    self.charge(t, "Custom");
                }
            }
            "M400" => {
                let t = if self.in_j_off { 0.0 } else { parse_word(rest, 'S').unwrap_or(0.0) + parse_word(rest, 'P').unwrap_or(0.0) / 1000.0 };
                self.barrier();
                let r = self.role.name().to_string();
                self.charge(t, &r);
            }
            "M204" => self.accel(&[parse_word(rest, 'S'), parse_word(rest, 'P'), parse_word(rest, 'R'), parse_word(rest, 'T')]),
            "SET_VELOCITY_LIMIT" => {
                let a = rest.split_whitespace().find_map(|w| w.strip_prefix("ACCEL=").and_then(crate::fmt::parse_f64));
                self.accel(&[a, None, None, None]);
            }
            "M201" | "M203" | "M205" => self.limits_override(cmd, rest),
            "M106" => {
                let p = parse_word(rest, 'P').unwrap_or(1.0) as i32;
                let s = parse_word(rest, 'S').unwrap_or(255.0);
                let pct = ((s / 255.0) * 100.0).round().clamp(0.0, 100.0) as u8;
                match p {
                    0 | 1 => self.fan[0] = pct,
                    2 => self.fan[1] = pct,
                    3 => self.fan[2] = pct,
                    _ => {}
                }
            }
            "M107" => self.fan[0] = 0,
            "M104" | "M109" => {
                if let Some(s) = parse_word(rest, 'S') {
                    // `T` is the firmware's number; records keep Fab's nozzle order.
                    let nozzle = match parse_word(rest, 'T').map(|t| t as usize) {
                        Some(t) => self.phys_map.iter().position(|&p| p == t),
                        None if rest.split_whitespace().any(|w| w == "A") => {
                            self.t_nozzle.fill(s as i16);
                            None
                        }
                        None => Some(self.extruder),
                    };
                    if let Some(slot) = nozzle.and_then(|n| self.t_nozzle.get_mut(n)) {
                        *slot = s as i16;
                    }
                    self.temps = match self.temp_entries.get(&self.t_nozzle) {
                        Some(&id) => id,
                        None => {
                            let id = self.out.nozzle_temps.len() as u32;
                            self.out.nozzle_temps.push(self.t_nozzle.clone());
                            self.temp_entries.insert(self.t_nozzle.clone(), id);
                            id
                        }
                    };
                }
                if cmd == "M109" {
                    self.barrier();
                }
            }
            "M140" | "M190" => {
                if let Some(s) = parse_word(rest, 'S') {
                    self.t_bed = s as i16;
                }
                if cmd == "M190" {
                    self.barrier();
                }
            }
            "M622.1" => {
                self.j_pending_off = Some(parse_word(rest, 'S') == Some(0.0));
            }
            "M1002" if rest.trim_start().starts_with("judge_flag") => {
                self.j_default_off = self.j_pending_off.take().unwrap_or(false);
            }
            "M622" => {
                self.in_j1 = rest.split_whitespace().any(|w| w == "J1");
                self.in_j_off = self.j_default_off;
            }
            "M623" => {
                self.in_j1 = false;
                self.in_j_off = false;
            }
            "SYNC" => {
                // A time hint from a printer template; inside its flush block, the flush time
                // is the change model's.
                if !self.in_flush {
                    let t = parse_word(rest, 'T').unwrap_or(0.0);
                    self.charge(t, "Flush");
                }
            }
            "M1020" => {
                if let Some(s) = parse_word(rest, 'S') {
                    self.set_filament(s as i32, true);
                }
            }
            _ => {
                if let Some(n) = cmd.strip_prefix('T') {
                    if let Ok(n) = n.parse::<i32>() {
                        self.set_filament(n, true);
                    }
                }
            }
        }
    }

    fn limits_override(&mut self, cmd: &str, rest: &str) {
        // In-file limits apply to the active extruder's entry (all of them on
        // a single-extruder machine).
        let e = self.extruder.min(self.lim.len() - 1);
        let l = &mut self.lim[e];
        let axes = ['X', 'Y', 'Z', 'E'];
        for (a, ax) in axes.iter().enumerate() {
            if let Some(v) = parse_word(rest, *ax) {
                match cmd {
                    "M201" => l.max_accel[a] = unlimited_if_unset(v),
                    "M203" => l.max_speed[a] = unlimited_if_unset(v),
                    "M205" => l.max_jerk[a] = unlimited_if_unset(v),
                    _ => {}
                }
            }
        }
    }

    fn set_filament(&mut self, n: i32, timed: bool) {
        if !(0..255).contains(&n) || n == self.filament {
            return;
        }
        let new_ext = self.fil_ext.get(n as usize).copied().unwrap_or(self.extruder);
        if self.loaded.len() <= new_ext.max(self.extruder) {
            self.loaded.resize(new_ext.max(self.extruder) + 1, -1);
        }
        if timed {
            // A nozzle that still holds the filament only swaps in; otherwise
            // the old one unloads and the new one loads.
            let held = self.loaded[new_ext];
            let mut change = 0.0;
            if held != n {
                self.out.filament_loads += 1;
                change += self.changes.load_s;
                if held >= 0 {
                    change += self.changes.unload_s;
                }
            }
            let mut switch = 0.0;
            if self.filament >= 0 {
                self.out.tool_changes += 1;
                if new_ext != self.extruder {
                    self.out.head_changes += 1;
                    switch = self.changes.switch_s;
                }
                if held >= 0 && held != n {
                    self.out.filament_swaps += 1;
                }
            }
            self.barrier();
            self.charge(switch, HEAD_SWITCH);
            self.charge(change, FILAMENT_CHANGE);
        }
        let held = self.loaded[new_ext];
        self.loaded[new_ext] = n;
        self.filament = n;
        self.extruder = new_ext;
        let into = if timed { std::mem::take(&mut self.flush_into) } else { 0.0 };
        if timed && held >= 0 {
            self.purge(held as usize, n as usize, into);
        }
    }

    /// The purge of a nozzle that held `held` taking filament `to`, by the change model, less
    /// what the G-code prints of it into paths (`into`, mm³): its time, and the incoming
    /// filament it feeds (the old one's melt leaves first, but it was fed when that one loaded).
    fn purge(&mut self, held: usize, to: usize, into: f64) {
        let c = self.changes.change(Some(held), to);
        let mm3 = (c.purge_mm3 - into).max(0.0);
        if mm3 <= 0.0 {
            return;
        }
        let purge_s = c.purge_s * mm3 / c.purge_mm3;
        self.out.purges += 1;
        let mm = mm3 / self.fil_area.get(to).copied().unwrap_or(1.0).max(1e-9);
        if let (Some(a), Some(b)) = (self.out.filament_mm.get_mut(to), self.out.purge_mm.get_mut(to)) {
            *a += mm;
            *b += mm;
        }
        let role = std::mem::replace(&mut self.role, Role::Flush);
        self.charge(purge_s, PURGE);
        self.role = role;
        if purge_s > 0.0 {
            if let Some(r) = self.part.last_mut() {
                r.flags |= crate::record::flags::PURGE;
            }
        }
    }

    pub fn finish(mut self) -> Timeline {
        self.barrier();
        let last = std::mem::take(&mut self.part);
        self.out.parts.push(last);
        // Each line's end time, its times and the ones before it summed in place; then the
        // records' times absolute.
        let n = self.lines as usize;
        let mut line_end = std::mem::take(&mut self.line_s);
        line_end.resize(n + 1, 0.0);
        let mut acc = 0.0;
        for t in line_end.iter_mut().take(n) {
            acc += *t;
            *t = acc;
        }
        self.out.total = acc + line_end[n];
        line_end.truncate(n);
        // The parts not yet settled, as they would have been streamed.
        while self.settle.parts < self.out.parts.len() {
            let p = self.settle.parts;
            self.settle.part(&mut self.out.parts[p], self.out.prepare, &mut self.out.filament_s);
            if let Some(stream) = &mut self.stream {
                let temps = self.out.nozzle_temps[self.settle.temps..].to_vec();
                self.settle.temps = self.out.nozzle_temps.len();
                self.spare = (stream.send)(std::mem::take(&mut self.out.parts[p]), temps);
            }
        }
        if self.stream.take().is_some() {
            self.out.parts.clear();
        }
        let settle = std::mem::take(&mut self.settle);
        for (name, v) in settle.roles {
            let charged = self.out.role_times.get(name).copied().unwrap_or(0.0);
            self.out.role_times.insert(name.to_string(), charged + v);
        }
        let mut layers = settle.layers;
        if let Some((l, e)) = settle.layer {
            layers.insert(l, e);
        }
        self.out.layer_times = layers.into_iter().map(|(l, (z, t))| (l, z, t)).collect();
        self.out.line_end = line_end;
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_fab_settings::config::Config;

    /// The time model fills the buffers its taker returns instead of allocating one per part.
    #[test]
    fn streamed_parts_reuse_the_takers_buffers() {
        let mut cfg = Config::new();
        cfg.set_vec("nozzle_diameter", vec!["0.4".into()]);
        let mut p = Processor::new(&Job::new(cfg));
        let addresses = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = addresses.clone();
        p.stream_to(PartStream {
            send: Box::new(move |records, _| {
                seen.lock().unwrap().push(records.as_ptr() as usize);
                Some(records)
            }),
        });
        p.line("; MACHINE_START_GCODE_END");
        p.line("G91");
        for _ in 0..6 * PART_MOVES {
            p.line("G1 X1");
        }
        p.finish();
        let addresses = addresses.lock().unwrap();
        assert!(addresses.len() >= 6, "the time model streamed at least six parts");
        let distinct: std::collections::BTreeSet<_> = addresses.iter().copied().collect();
        assert!(distinct.len() <= 3, "the time model allocates a buffer per part: the taker's buffers do not come back");
    }

    /// Each head keeps its own target temperature, however many there are: by its tool word, by
    /// the head that prints, and all at once.
    #[test]
    fn four_heads_keep_their_own_temperatures() {
        let mut cfg = Config::new();
        cfg.set_vec("nozzle_diameter", vec!["0.4".into(); 4]);
        cfg.set_vec("filament_map", (1..=4).map(|h| h.to_string()).collect());
        let mut p = Processor::new(&Job::new(cfg));
        for l in ["M104 T2 S215", "M104 T3 S170", "T1", "M104 S205", "G1 X10 F600", "M104 A S0", "G1 X20"] {
            p.line(l);
        }
        let tl = p.finish();
        let travel: Vec<_> = tl.moves().filter(|m| m.kind == MoveKind::Travel).map(|m| tl.nozzle_temps[m.temps as usize].clone()).collect();
        assert_eq!(travel, [vec![0, 205, 215, 170], vec![0; 4]]);
        // Heads numbered apart from Fab's order: a tool word is the firmware's number, the
        // records are in Fab's order, and a number no head has sets nothing.
        let mut cfg = Config::new();
        cfg.set_vec("nozzle_diameter", vec!["0.4".into(); 4]);
        cfg.set_vec("physical_extruder_map", ["1", "2", "3", "0"].map(String::from).to_vec());
        let mut p = Processor::new(&Job::new(cfg));
        for l in ["M104 T0 S100", "M104 T1 S200", "M104 T3 S170", "M104 T4 S1", "G1 X10 F600"] {
            p.line(l);
        }
        let tl = p.finish();
        assert_eq!(tl.moves().filter(|m| m.kind == MoveKind::Travel).map(|m| tl.nozzle_temps[m.temps as usize].clone()).collect::<Vec<_>>(), [vec![200, 0, 170, 100]]);
        // Nine heads, each its own target, and a set seen before is the same entry.
        let mut cfg = Config::new();
        cfg.set_vec("nozzle_diameter", vec!["0.4".into(); 9]);
        let mut p = Processor::new(&Job::new(cfg));
        for l in ["M104 T8 S190", "M104 T4 S230", "G1 X10 F600", "M104 T4 S0", "G1 X20", "M104 T4 S230", "G1 X30"] {
            p.line(l);
        }
        let tl = p.finish();
        let travel: Vec<_> = tl.moves().filter(|m| m.kind == MoveKind::Travel).collect();
        assert_eq!(tl.nozzle_temps[travel[0].temps as usize], [0, 0, 0, 0, 230, 0, 0, 0, 190]);
        assert_eq!(tl.nozzle_temps[travel[1].temps as usize][4], 0);
        assert_eq!(travel[2].temps, travel[0].temps);
    }

    /// A head switch and a nozzle loading filament are their own rows of the time, with their
    /// counts; a nozzle that never takes another filament out purges nothing.
    #[test]
    fn tool_changes_are_timed_apart_from_flush() {
        let mut cfg = Config::new();
        cfg.set_vec("nozzle_diameter", vec!["0.4".into(), "0.4".into()]);
        cfg.set_vec("filament_map", vec!["1".into(), "2".into()]);
        cfg.set_str("machine_switch_extruder_time", "5");
        cfg.set_str("machine_load_filament_time", "20");
        let mut p = Processor::new(&Job::new(cfg));
        for l in ["T0", "G1 X10 F600", "T1", "G1 X20", "T0", "G1 X30", "T1", "G1 X40"] {
            p.line(l);
        }
        let tl = p.finish();
        assert_eq!((tl.head_changes, tl.filament_loads, tl.filament_swaps), (3, 2, 0));
        assert_eq!((tl.role_times.get(PURGE), tl.purges), (None, 0), "nothing purged: {:?}", tl.role_times);
        let rows = tl.change_rows();
        assert_eq!(rows, vec![
            ChangeRow { name: HEAD_SWITCH, seconds: 15.0, detail: "3 × 5 s".into() },
            ChangeRow { name: FILAMENT_CHANGE, seconds: 40.0, detail: "2 × 20 s".into() },
        ]);
        assert_eq!(tl.role_times.get(HEAD_SWITCH), Some(&15.0));
    }

    /// Every change purges what the printer's change model says, in filament and in time: the
    /// same-nozzle swaps of a two-nozzle head and of one nozzle fed from four slots, nothing on
    /// a tool changer. A template's own account of its flush is not counted again.
    #[test]
    fn changes_purge_by_the_change_model() {
        const COLOURS: [&str; 4] = ["#FFFFFF", "#000000", "#FF0000", "#0000FF"];
        let run = |nozzles: usize, map: &[u32], tools: &[usize], template: &[&str]| {
            let mut cfg = Config::new();
            cfg.set_vec("nozzle_diameter", vec!["0.4".into(); nozzles]);
            cfg.set_vec("filament_map", map.iter().map(|m| m.to_string()).collect());
            cfg.set_vec("filament_colour", COLOURS.iter().map(|c| c.to_string()).collect());
            cfg.set_vec("filament_diameter", vec!["1.75".into(); 4]);
            cfg.set_vec("filament_max_volumetric_speed", vec!["10".into(); 4]);
            let job = Job::new(cfg);
            let mut p = Processor::new(&job);
            p.line("M83");
            for (k, t) in tools.iter().enumerate() {
                p.line(&format!("T{t}"));
                for l in template {
                    p.line(l);
                }
                p.line(&format!("G1 X{} Y10 E1 F600", 10 * (k + 1)));
            }
            (p.finish(), job.filament_area(0))
        };
        let v = |a: usize, b: usize| makepad_fab_settings::flush::purge_volume(COLOURS[a], COLOURS[b]).round();
        // Two nozzles: slots 1 and 3 on the left, 2 and 4 on the right.
        let (tl, area) = run(2, &[1, 2, 1, 2], &[0, 1, 2, 3, 0], &[]);
        assert_eq!((tl.head_changes, tl.filament_swaps, tl.purges), (4, 3, 3), "the swaps purge, the head switches alone do not");
        let want = [v(2, 0), 0.0, v(0, 2), v(1, 3)];
        for (f, w) in want.iter().enumerate() {
            assert!((tl.purge_mm[f] * area - w).abs() < 1e-6, "slot {} purges {w} mm³, not {}", f + 1, tl.purge_mm[f] * area);
            assert!((tl.filament_mm[f] - tl.purge_mm[f] - if f == 0 { 2.0 } else { 1.0 }).abs() < 1e-6, "slot {}'s filament is its prints and its purge", f + 1);
        }
        let total: f64 = want.iter().sum();
        assert!((tl.role_times[PURGE] - total / 10.0).abs() < 1e-6, "the purge at the flush speed: {:?}", tl.role_times);
        let row = tl.change_rows().pop().expect("a purge row");
        assert!(row.name == PURGE && row.detail.starts_with("3 × "), "{row:?}");
        assert!(tl.total > total / 10.0);
        // One nozzle from four slots: every change after the first load purges.
        let (tl, area) = run(1, &[1, 1, 1, 1], &[0, 1, 2, 3, 0], &[]);
        assert_eq!(tl.purges, 4);
        assert!((tl.purge_mm.iter().sum::<f64>() * area - (v(0, 1) + v(1, 2) + v(2, 3) + v(3, 0))).abs() < 1e-6);
        // A tool changer: each tool carries its own filament.
        let (tl, _) = run(4, &[1, 2, 3, 4], &[0, 1, 2, 3, 0], &[]);
        assert_eq!((tl.head_changes, tl.purges), (4, 0));
        assert!(tl.purge_mm.iter().all(|m| *m == 0.0) && !tl.role_times.contains_key(PURGE));
        // A template that states its own flush: the model's purge, once.
        let flush = ["; VFLUSH_START", ";VG1 E50 F100", "SYNC T30", "; VFLUSH_END"];
        let (tl, area) = run(1, &[1, 1, 1, 1], &[0, 1], &flush);
        assert!((tl.purge_mm[1] * area - v(0, 1)).abs() < 1e-6 && (tl.filament_mm[1] - tl.purge_mm[1] - 1.0).abs() < 1e-6, "{:?}", tl.filament_mm);
        assert_eq!(tl.role_times.get("Flush"), None, "{:?}", tl.role_times);
        // Its time with the slot that printed it, after the start sequence.
        assert!(tl.filament_s[0] > 0.0 && tl.filament_s[1] > 0.0 && tl.filament_s[2] == 0.0, "{:?}", tl.filament_s);
    }
}
