//! The fake print of a virtual printer: the G-code of a job, reduced once to a [`Schedule`], and
//! the job's clock ([`Run`]) that runs along it. Pure: time is passed in, nothing here sleeps,
//! opens a file or reaches the network.

use crate::model::{LoadedSlot, PrinterState, Temperature, TrayId};
use makepad_fab_gcode::process::{PartStream, Processor};
use makepad_fab_gcode::record::{MoveKind, MoveRecord};
use makepad_fab_settings::config::Config;
use makepad_fab_settings::family::{self, Package};
use makepad_fab_settings::machine::Job;
use std::borrow::Cow;
use std::sync::{Arc, Mutex};

/// The G-code of a job's file as its printer's family sends it: the plate's G-code inside a
/// package, or the file itself. `model` is the printer model the job was sliced for.
pub fn package_gcode<'a>(model: &str, package: &'a [u8]) -> Result<Cow<'a, [u8]>, String> {
    match family::of_model(model).ok_or(family::UNSUPPORTED)?.package {
        Package::GcodeThreeMf { .. } => {
            let mut cursor = std::io::Cursor::new(package);
            let dir = makepad_zip_file::zip_read_central_directory(&mut cursor).map_err(|_| "not a readable package".to_string())?;
            let header = dir.file_headers.iter().find(|h| h.file_name == "Metadata/plate_1.gcode").ok_or("no plate G-code in the package")?;
            let plate = header.extract(&mut cursor).map_err(|_| "cannot read the plate G-code".to_string())?;
            Ok(Cow::Owned(plate))
        }
        Package::PlainGcode => Ok(Cow::Borrowed(package)),
    }
}

/// How fast a virtual job's clock runs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Speed {
    /// This many job seconds per second. A value that is not a positive number runs at 1.
    Times(f64),
    /// As fast as it goes: the job runs to its next stop for an insert, or to its end.
    ToEnd,
}

impl Default for Speed {
    fn default() -> Self { Self::Times(1.0) }
}

impl Speed {
    /// Job seconds per second (None: to the end).
    pub fn rate(self) -> Option<f64> {
        match self {
            Self::Times(x) if x.is_finite() && x > 0.0 => Some(x),
            Self::Times(_) => Some(1.0),
            Self::ToEnd => None,
        }
    }
}

/// A tool or filament change: the job holds `filament` on `nozzle` (Fab's numbering, 1-based
/// project filament) from `t`, and is changing until `until`, the next extrusion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Change { pub t: f64, pub until: f64, pub nozzle: u8, pub filament: u8 }

/// The targets the job sets at `t`: one per nozzle in Fab's numbering, and the bed's.
#[derive(Clone, Debug, PartialEq)]
pub struct TempEvent { pub t: f64, pub nozzles: Vec<i16>, pub bed: i16 }

/// What a job's G-code comes to once its moves are timed: all the fake print needs, in seconds
/// on the print's clock (the time model's, the one a preview's timeline uses).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Schedule {
    pub total: f64,
    /// The end of the start sequence.
    pub prepare: f64,
    /// The start of each layer, by layer index (never before `prepare`).
    pub layer_starts: Vec<f64>,
    pub changes: Vec<Change>,
    pub temps: Vec<TempEvent>,
    /// Where the print waits for items to be placed in it (`; FAB_PAUSE:`).
    pub pauses: Vec<f64>,
}

#[derive(Default)]
struct Builder {
    table: Vec<Vec<i16>>,
    layer_starts: Vec<f64>,
    changes: Vec<Change>,
    /// The last change waits for the next extrusion to end.
    open: bool,
    held: Option<(u8, u8)>,
    /// Where the run of firmware macros before this record began.
    macro_start: Option<f64>,
    temps: Vec<TempEvent>,
    last_temps: Option<(u32, i16)>,
    pauses: Vec<f64>,
}

impl Builder {
    fn feed(&mut self, part: &[MoveRecord], entries: Vec<Vec<i16>>) {
        self.table.extend(entries);
        for r in part {
            while self.layer_starts.len() <= r.layer as usize { self.layer_starts.push(r.t_start); }
            if self.last_temps != Some((r.temps, r.t_bed)) {
                self.last_temps = Some((r.temps, r.t_bed));
                if let Some(nozzles) = self.table.get(r.temps as usize) {
                    self.temps.push(TempEvent { t: r.t_start, nozzles: nozzles.clone(), bed: r.t_bed });
                }
            }
            if r.kind == MoveKind::Pause { self.pauses.push(r.t_start); }
            if r.kind == MoveKind::FirmwareMacro {
                self.macro_start.get_or_insert(r.t_start);
                continue;
            }
            if r.filament > 0 && self.held != Some((r.nozzle, r.filament)) {
                self.held = Some((r.nozzle, r.filament));
                let t = self.macro_start.unwrap_or(r.t_start);
                self.changes.push(Change { t, until: t, nozzle: r.nozzle, filament: r.filament });
                self.open = true;
            }
            self.macro_start = None;
            if self.open && r.kind == MoveKind::Extrude {
                self.open = false;
                if let Some(change) = self.changes.last_mut() { change.until = r.t_start; }
            }
        }
    }

    fn finish(mut self, total: f64, prepare: f64) -> Schedule {
        for start in &mut self.layer_starts { *start = start.max(prepare); }
        for change in &mut self.changes {
            change.t = change.t.max(prepare);
            change.until = change.until.max(change.t);
        }
        if self.open {
            if let Some(change) = self.changes.last_mut() { change.until = total.max(change.t); }
        }
        Schedule { total, prepare, layer_starts: self.layer_starts, changes: self.changes, temps: self.temps, pauses: self.pauses }
    }
}

/// One pass of the time model over `lines`, reduced to a schedule. The records are streamed
/// through the builder as the model settles them, so a long print never holds them all.
fn build<'a>(cfg: Config, lines: impl Iterator<Item = &'a str>) -> Schedule {
    let mut model = Processor::new(&Job::new(cfg));
    let builder = Arc::new(Mutex::new(Builder::default()));
    let sink = builder.clone();
    model.stream_to(PartStream {
        send: Box::new(move |part, entries| {
            sink.lock().unwrap_or_else(|e| e.into_inner()).feed(&part, entries);
            Some(part)
        }),
    });
    for line in lines { model.line(line); }
    let timeline = model.finish();
    let builder = std::mem::take(&mut *builder.lock().unwrap_or_else(|e| e.into_inner()));
    builder.finish(timeline.total, timeline.prepare)
}

/// The schedule of a job's G-code. It runs the time model over the whole file: call it on a
/// worker, never on the UI thread.
pub fn schedule(gcode: &[u8]) -> Result<Schedule, String> {
    let text = std::str::from_utf8(gcode).map_err(|_| "the G-code is not text".to_string())?;
    let mut cfg = Config::new();
    for (key, value) in crate::safety::header_config(text) {
        let value = makepad_fab_settings::config::parse_text(&key, &value);
        cfg.set(&key, value);
    }
    Ok(build(cfg, text.lines()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hold { User, Insert }

/// How a job ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End { Finished, Stopped }

/// A job's clock along its schedule. Time is passed in as seconds on any steady clock.
#[derive(Clone, Debug)]
pub struct Run {
    schedule: Arc<Schedule>,
    /// Per project filament, the tray that feeds it.
    trays: Vec<Option<TrayId>>,
    clock: f64,
    last: f64,
    hold: Option<Hold>,
    inserts: usize,
    end: Option<End>,
}

impl Run {
    pub fn new(schedule: Arc<Schedule>, trays: Vec<Option<TrayId>>, now: f64) -> Self {
        Self { schedule, trays, clock: 0.0, last: now, hold: None, inserts: 0, end: None }
    }

    /// The job's clock: seconds on the print's clock.
    pub fn clock(&self) -> f64 { self.clock }
    pub fn end(&self) -> Option<End> { self.end }
    pub fn held(&self) -> bool { self.hold.is_some() }
    /// The clock is running: a later `now` moves it.
    pub fn ticking(&self) -> bool { self.end.is_none() && self.hold.is_none() }

    /// Brings the clock to `now` at `speed`. A stop for an insert holds it there until
    /// [`Self::resume`]. True when the clock or the state moved.
    pub fn advance(&mut self, now: f64, speed: Speed) -> bool {
        let dt = (now - self.last).max(0.0);
        self.last = now;
        if !self.ticking() { return false; }
        let (clock, hold, end) = (self.clock, self.hold, self.end);
        let total = self.schedule.total;
        let target = speed.rate().map_or(f64::INFINITY, |rate| self.clock + dt * rate);
        let stop = self.schedule.pauses.get(self.inserts).map(|p| p.max(self.clock)).filter(|p| *p <= total);
        match stop {
            Some(at) if at <= target => {
                self.clock = at;
                self.hold = Some(Hold::Insert);
                self.inserts += 1;
            }
            _ if target >= total => {
                self.clock = total;
                self.end = Some(End::Finished);
            }
            _ => self.clock = target,
        }
        (clock, hold, end) != (self.clock, self.hold, self.end)
    }

    pub fn pause(&mut self) -> Result<(), &'static str> {
        if self.end.is_some() { return Err("no job is running"); }
        if self.hold.is_some() { return Err("the printer is already paused"); }
        self.hold = Some(Hold::User);
        Ok(())
    }

    /// Goes on from a pause, or from a stop for an insert; the clock runs on from `now`.
    pub fn resume(&mut self, now: f64) -> Result<(), &'static str> {
        if self.end.is_some() { return Err("no job is running"); }
        if self.hold.take().is_none() { return Err("the printer is not paused"); }
        self.last = now;
        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), &'static str> {
        if self.end.is_some() { return Err("no job is running"); }
        self.end = Some(End::Stopped);
        Ok(())
    }

    /// The layer the job is on, 1-based (0 before the first).
    pub fn layer(&self) -> u32 {
        self.schedule.layer_starts.partition_point(|start| *start <= self.clock) as u32
    }

    /// The printer's `gcode_state`.
    pub fn gcode_state(&self) -> &'static str {
        match (self.end, self.hold) {
            (Some(End::Finished), _) => "FINISH",
            (Some(End::Stopped), _) => "FAILED",
            (None, Some(_)) => "PAUSE",
            (None, None) if self.clock < self.schedule.prepare => "PREPARE",
            (None, None) => "RUNNING",
        }
    }

    /// Job seconds per second the clock runs at now: what a follower runs its own clock at
    /// between reports.
    pub fn rate(&self, speed: Speed) -> f64 {
        if self.ticking() { speed.rate().unwrap_or(0.0) } else { 0.0 }
    }

    fn changes_until_now(&self) -> &[Change] {
        let s = &self.schedule.changes;
        &s[..s.partition_point(|c| c.t <= self.clock)]
    }

    /// What the printer reports of the job: state, layers, progress, the active head, what each
    /// head holds and takes, and the temperatures. `state` is the idle report of the layout.
    pub fn overlay(&self, state: &mut PrinterState, name: &str, speed: Speed) {
        let s = &*self.schedule;
        state.gcode_state = Some(self.gcode_state().into());
        state.job_name = Some(name.into());
        state.total_layers = Some(s.layer_starts.len() as u32);
        state.layer = Some(self.layer());
        state.progress = Some(if s.total > 0.0 { (self.clock / s.total * 100.0).clamp(0.0, 100.0) } else { 100.0 });
        // Like a real printer's, the time left is the wall time to the end: at the job's speed.
        state.remaining_minutes = Some((s.total - self.clock).max(0.0) / speed.rate().unwrap_or(f64::INFINITY) / 60.0);
        state.print_seconds = Some(self.clock);
        state.print_rate = Some(self.rate(speed));
        let seen = self.changes_until_now();
        let changing = seen.last().is_some_and(|c| self.clock < c.until);
        state.stage = match (self.end, self.hold) {
            (None, None) if changing => Some(4),
            (None, None) if self.clock >= s.prepare => Some(0),
            _ => None,
        };
        state.active_extruder = seen.last().and_then(|c| state.extruders.get(c.nozzle as usize)).and_then(|e| e.id);
        let tray_of = |filament: u8| self.trays.get((filament as usize).wrapping_sub(1)).copied().flatten().map(LoadedSlot::Tray);
        for (nozzle, extruder) in state.extruders.iter_mut().enumerate() {
            let mut own = seen.iter().rev().filter(|c| c.nozzle as usize == nozzle);
            let Some(now) = own.next() else { continue };
            let target = tray_of(now.filament);
            if target.is_some() { extruder.target = target; }
            let loaded = if self.clock >= now.until { target } else { own.next().and_then(|c| tray_of(c.filament)).or(extruder.loaded) };
            if loaded.is_some() { extruder.loaded = loaded; }
        }
        let at = s.temps.partition_point(|e| e.t <= self.clock);
        if let Some(event) = at.checked_sub(1).and_then(|i| s.temps.get(i)) {
            let heat = |target: i16| Temperature {
                current: Some(if target > 0 { target as f64 } else { 20.0 }),
                target: Some(target.max(0) as f64),
            };
            for (nozzle, extruder) in state.extruders.iter_mut().enumerate() {
                if let Some(target) = event.nozzles.get(nozzle) { extruder.temperature = heat(*target); }
            }
            state.bed = heat(event.bed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_fab_gcode::process::{FILAMENT_CHANGE, HEAD_SWITCH};

    const RUNS: [Speed; 3] = [Speed::Times(1.0), Speed::Times(10.0), Speed::ToEnd];

    /// Two layers after a 10 s start, a stop for an insert at 55 s, 100 s in all.
    fn plan() -> Arc<Schedule> {
        Arc::new(Schedule {
            total: 100.0, prepare: 10.0, layer_starts: vec![10.0, 40.0, 70.0],
            changes: vec![Change { t: 10.0, until: 10.0, nozzle: 0, filament: 1 }],
            temps: Vec::new(), pauses: vec![55.0],
        })
    }

    fn run() -> Run { Run::new(plan(), vec![Some(TrayId { ams: 0, slot: 0 })], 100.0) }

    #[test]
    fn speeds_that_are_not_numbers_run_at_one() {
        assert_eq!(RUNS.map(Speed::rate), [Some(1.0), Some(10.0), None]);
        for bad in [0.0, -3.0, f64::NAN, f64::INFINITY] { assert_eq!(Speed::Times(bad).rate(), Some(1.0)); }
    }

    #[test]
    fn the_clock_runs_at_the_speed_and_not_while_held() {
        let mut r = run();
        assert!(!r.advance(100.0, Speed::Times(10.0)), "no time passed");
        r.advance(101.0, Speed::Times(10.0));
        assert_eq!(r.clock(), 10.0, "ten job seconds for one second at 10x");
        assert_eq!(r.rate(Speed::Times(10.0)), 10.0);
        r.pause().unwrap();
        r.advance(160.0, Speed::Times(10.0));
        assert_eq!((r.clock(), r.gcode_state(), r.rate(Speed::Times(10.0))), (10.0, "PAUSE", 0.0), "a pause holds the clock");
        r.resume(200.0).unwrap();
        r.advance(201.0, Speed::Times(1.0));
        assert_eq!(r.clock(), 11.0, "the time held is not counted");
        assert_eq!(r.layer(), 1);
    }

    #[test]
    fn a_stop_for_an_insert_holds_the_clock_until_resumed() {
        let mut r = run();
        r.advance(110.0, Speed::Times(10.0));
        assert_eq!((r.clock(), r.gcode_state(), r.held()), (55.0, "PAUSE", true), "10 s at 10x is 100 s: held at the insert, not at 100");
        r.advance(500.0, Speed::ToEnd);
        assert_eq!(r.clock(), 55.0);
        assert_eq!(r.resume(500.0), Ok(()));
        r.advance(500.0, Speed::ToEnd);
        assert_eq!((r.clock(), r.end(), r.gcode_state()), (100.0, Some(End::Finished), "FINISH"));
        assert_eq!(r.layer(), 3);
        assert_eq!(r.resume(501.0), Err("no job is running"));
    }

    #[test]
    fn stop_ends_the_job_as_failed_and_keeps_its_clock() {
        let mut r = run();
        r.advance(105.0, Speed::Times(1.0));
        r.stop().unwrap();
        assert_eq!((r.end(), r.gcode_state(), r.clock(), r.ticking()), (Some(End::Stopped), "FAILED", 5.0, false));
        r.advance(900.0, Speed::ToEnd);
        assert_eq!(r.clock(), 5.0);
        assert_eq!(r.stop(), Err("no job is running"));
    }

    #[test]
    fn the_report_follows_the_clock() {
        let mut idle = PrinterState::default();
        idle.extruders.push(crate::model::Extruder { id: Some(crate::model::CloudNozzleId(1)), ..Default::default() });
        let mut r = run();
        r.advance(105.0, Speed::Times(1.0));
        let mut state = idle.clone();
        r.overlay(&mut state, "plate", Speed::Times(1.0));
        assert_eq!((state.gcode_state.as_deref(), state.layer, state.total_layers, state.print_seconds, state.print_rate), (Some("PREPARE"), Some(0), Some(3), Some(5.0), Some(1.0)));
        r.advance(125.0, Speed::Times(1.0));
        let mut state = idle.clone();
        r.overlay(&mut state, "plate", Speed::Times(1.0));
        assert_eq!((state.gcode_state.as_deref(), state.layer, state.stage, state.progress), (Some("RUNNING"), Some(1), Some(0), Some(25.0)));
        assert_eq!(state.active_extruder, Some(crate::model::CloudNozzleId(1)));
        assert_eq!(state.extruders[0].loaded, Some(LoadedSlot::Tray(TrayId { ams: 0, slot: 0 })));
        assert_eq!(state.remaining_minutes, Some(75.0 / 60.0), "75 job seconds left at 1x");
        r.overlay(&mut state, "plate", Speed::Times(10.0));
        assert_eq!(state.remaining_minutes, Some(7.5 / 60.0), "the time left is wall time at the job's speed");
    }

    /// The lines of a two-nozzle print: a 10 s start, three layers, a change of nozzle and
    /// filament with its time, a stop for an insert and the targets it sets.
    fn lines() -> Vec<&'static str> {
        vec![
            "G90", "G21", "M83", "; MACHINE_START_GCODE_END",
            "G1 X10 Y0 F6000", "M104 T0 S220", "M140 S60", "T0",
            "; CHANGE_LAYER", "; Z_HEIGHT: 0.2", "; LAYER_HEIGHT: 0.2", "; LINE_WIDTH: 0.4", "; FEATURE: Outer wall",
            "G1 X20 E1 F1200",
            "; CHANGE_LAYER", "; Z_HEIGHT: 0.4", "G1 Z0.4", "T1", "M104 T1 S200", "G1 X30 E1",
            "; FAB_PAUSE: layer 3 z 0.6: place nut",
            "; CHANGE_LAYER", "; Z_HEIGHT: 0.6", "G1 Z0.6", "G1 X40 E1",
        ]
    }

    fn two_nozzles() -> Config {
        let mut cfg = Config::new();
        cfg.set_vec("nozzle_diameter", vec!["0.4".into(), "0.4".into()]);
        cfg.set_vec("filament_map", vec!["1".into(), "2".into()]);
        cfg.set_str("machine_switch_extruder_time", "5");
        cfg.set_str("machine_load_filament_time", "20");
        cfg
    }

    /// The schedule is what a direct pass of the time model says: totals, layer starts, the
    /// change and its time, the stop, the targets.
    #[test]
    fn the_schedule_matches_a_direct_pass_of_the_time_model() {
        let got = build(two_nozzles(), lines().into_iter());
        let mut model = Processor::new(&Job::new(two_nozzles()));
        for line in lines() { model.line(line); }
        let direct = model.finish();
        let moves = direct.flat_moves();
        assert_eq!((got.total, got.prepare), (direct.total, direct.prepare));
        assert_eq!(got.layer_starts.len(), 3);
        for (layer, start) in got.layer_starts.iter().enumerate() {
            let first = moves.iter().find(|m| m.layer as usize == layer).map(|m| m.t_start).unwrap();
            assert_eq!(*start, first.max(direct.prepare), "layer {layer}'s start");
        }
        let held: Vec<(u8, u8)> = got.changes.iter().map(|c| (c.nozzle, c.filament)).collect();
        assert_eq!(held, vec![(0, 1), (1, 2)], "{:?}", got.changes);
        let load = direct.role_times.get(FILAMENT_CHANGE).copied().unwrap_or(0.0);
        let switch = direct.role_times.get(HEAD_SWITCH).copied().unwrap_or(0.0);
        let spans: f64 = got.changes.iter().map(|c| c.until - c.t).sum();
        assert!((spans - (load + switch)).abs() < 1e-6, "the changes take the time the model charged for them: {spans} s of {load} + {switch}");
        assert!(got.changes[1].until > got.changes[1].t, "the second change takes time");
        let paused: Vec<f64> = moves.iter().filter(|m| m.kind == MoveKind::Pause).map(|m| m.t_start).collect();
        assert_eq!((got.pauses.clone(), paused.len()), (paused, 1));
        let last = got.temps.last().unwrap();
        assert_eq!((last.nozzles[0], last.nozzles[1], last.bed), (220, 200, 60));
    }

    /// A checked job's G-code, through the family's extraction, gives the totals of a direct
    /// pass: the path a virtual printer takes.
    #[test]
    fn a_checked_job_gives_the_schedule_of_its_gcode() {
        let job = crate::safety::tests::checked_square("square");
        let gcode = package_gcode(job.config_value("printer_model").unwrap_or(""), job.bytes()).unwrap();
        let got = schedule(&gcode).unwrap();
        let text = std::str::from_utf8(&gcode).unwrap();
        let mut cfg = Config::new();
        for (key, value) in crate::safety::header_config(text) { cfg.set(&key, makepad_fab_settings::config::parse_text(&key, &value)); }
        let mut model = Processor::new(&Job::new(cfg));
        for line in text.lines() { model.line(line); }
        let direct = model.finish();
        assert_eq!((got.total, got.prepare), (direct.total, direct.prepare));
        assert_eq!(got.layer_starts.len(), direct.layer_times.len(), "a layer start for each layer the model times");
        assert!(got.total > got.prepare && got.layer_starts.iter().all(|s| *s >= got.prepare && *s <= got.total));
        assert!(package_gcode("Not a printer", job.bytes()).is_err(), "a model no family claims is refused as before");
    }
}
