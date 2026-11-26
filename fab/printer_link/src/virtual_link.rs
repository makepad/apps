//! An in-memory printer. Its layout supplies the same tray and head report as a linked machine.

use crate::model::{AmsUnit, CloudNozzleId, Colour, Extruder, LoadedSlot, Nozzle, PrinterState, Temperature, Tray, TrayId};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub struct VirtualSetup {
    pub name: String,
    pub model: String,
    /// Numbered tools have one spool each. Otherwise the trays feed one or two nozzles.
    pub numbered_heads: bool,
    /// Nozzle diameters in mm, in profile head order.
    pub nozzle_diameters: Vec<f64>,
    pub trays: Vec<VirtualTray>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VirtualTray {
    pub head: usize,
    pub id: TrayId,
    pub material: Option<String>,
    /// sRGB, 0xRRGGBB.
    pub colour: Option<u32>,
}

impl VirtualTray {
    fn report(&self) -> Tray {
        let colour = self.colour.map(|rgb| {
            let [_, r, g, b] = rgb.to_be_bytes();
            Colour([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0])
        });
        Tray {
            id: Some(self.id), present: Some(self.material.is_some()), material: self.material.clone(),
            colour, ..Default::default()
        }
    }
}

/// The idle machine. A numbered tool reports its own unit, while the two nozzle sides can
/// each have several units and an external spool.
pub fn report(setup: &VirtualSetup) -> PrinterState {
    let mut state = PrinterState {
        numbered_heads: setup.numbered_heads, gcode_state: Some("IDLE".into()), storage: Some(true),
        bed: Temperature { current: Some(20.0), target: Some(0.0) }, details: vec![("Printer".into(), "virtual".into())],
        ..Default::default()
    };
    let mut units = BTreeMap::new();
    for (head, &diameter) in setup.nozzle_diameters.iter().enumerate() {
        let id = if setup.numbered_heads {
            let Ok(h) = u8::try_from(head) else { break };
            let tray = Tray { id: Some(TrayId { ams: h, slot: 0 }), present: Some(false), ..Default::default() };
            let id = CloudNozzleId(h);
            state.ams.push(AmsUnit { id: Some(h), extruder: Some(id), trays: vec![tray], ..Default::default() });
            id
        } else {
            match head { 0 => CloudNozzleId(1), 1 => CloudNozzleId(0), _ => break }
        };
        state.extruders.push(Extruder { id: Some(id), ..Default::default() });
        state.nozzles.push(Nozzle { id: Some(id), diameter: Some(diameter), kind: Some("hardened steel".into()) });
    }
    for slot in &setup.trays {
        let Some(extruder) = state.extruders.get_mut(slot.head) else { continue };
        let tray = slot.report();
        if setup.numbered_heads {
            extruder.loaded = slot.material.is_some().then_some(LoadedSlot::Tray(slot.id));
            state.ams[slot.head].trays[0] = tray;
        } else if matches!(slot.id.ams, 254 | 255) {
            state.external.push(tray);
        } else {
            let unit = units.entry(slot.id.ams).or_insert_with(|| AmsUnit {
                id: Some(slot.id.ams), extruder: extruder.id, ..Default::default()
            });
            unit.trays.push(tray);
        }
    }
    state.ams.extend(units.into_values());
    state
}

#[cfg(not(target_arch = "wasm32"))]
use crate::{job, link::{publish, Shared}, virtual_run::{self, End, Run, Speed}, Command, CommandState, ConnectionStatus, JobStage, JobStatus, PrintJob, Snapshot};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::{atomic::Ordering, Arc};
#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, Instant};

/// How often a running job's progress goes out at the most: a layer or state change goes out
/// at once.
#[cfg(not(target_arch = "wasm32"))]
const CADENCE: Duration = Duration::from_millis(100);

/// The job on the printer: its clock, and the name the family's printer reports it by.
#[cfg(not(target_arch = "wasm32"))]
struct Active {
    run: Run,
    name: String,
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct VirtualPrinter {
    setup: VirtualSetup,
    snapshot: Snapshot,
    plate_check: bool,
    /// A job taken whose G-code is not read yet: that happens on the link thread, after the
    /// printer has said it is preparing.
    pending: Option<PrintJob>,
    /// The running job, or the last one until the next print: a printer keeps showing it.
    active: Option<Active>,
}

#[cfg(not(target_arch = "wasm32"))]
impl VirtualPrinter {
    fn new(setup: VirtualSetup) -> Self {
        let snapshot = Snapshot {
            virtual_printer: true, name: setup.name.clone(), model: setup.model.clone(),
            status: ConnectionStatus::Connected, state: Arc::new(report(&setup)), ..Default::default()
        };
        Self { setup, snapshot, plate_check: false, pending: None, active: None }
    }

    /// The name the family's real printer reports a job by: a toolchanger names it by the
    /// file it was uploaded as.
    fn reported_name(&self, name: &str) -> String {
        if self.setup.numbered_heads { job::plain_file_name(name) } else { name.to_string() }
    }

    /// The report: the layout's idle printer, with the job laid over it.
    fn refresh(&mut self) {
        let mut state = report(&self.setup);
        if let Some(active) = &self.active {
            active.run.overlay(&mut state, &active.name, self.snapshot.virtual_speed);
        } else if let Some(print) = &self.pending {
            state.gcode_state = Some("PREPARE".into());
            state.job_name = Some(self.reported_name(print.job.name()));
        }
        self.snapshot.state = Arc::new(state);
    }

    /// The job's stage as Fab's tracker shows it for a real printer.
    fn sync_stage(&mut self) {
        let stage = if self.pending.is_some() {
            JobStage::Starting
        } else if let Some(active) = &self.active {
            match active.run.end() {
                Some(End::Finished) => JobStage::Finished,
                Some(End::Stopped) => JobStage::Cancelled,
                None if active.run.held() => JobStage::Paused,
                None if active.run.gcode_state() == "PREPARE" => JobStage::Accepted,
                None => JobStage::Printing,
            }
        } else {
            return;
        };
        if let Some(job) = &mut self.snapshot.job { job.stage = stage; }
    }

    fn reconfigure(&mut self, setup: VirtualSetup) -> bool {
        if self.setup == setup { return false; }
        self.snapshot.name = setup.name.clone();
        self.snapshot.model = setup.model.clone();
        self.setup = setup;
        self.refresh();
        true
    }

    /// The job's clock runs on to `now` (seconds on any steady clock). True when the report
    /// moved on in its state, layer or stage, which goes out at once.
    fn tick(&mut self, now: f64) -> bool {
        let Some(active) = &mut self.active else { return false };
        if !active.run.advance(now, self.snapshot.virtual_speed) { return false; }
        let ended = active.run.end().is_some();
        let key = |s: &crate::PrinterState| (s.gcode_state.clone(), s.layer, s.stage);
        let before = key(&self.snapshot.state);
        self.refresh();
        self.sync_stage();
        if ended { self.plate_check = true; }
        key(&self.snapshot.state) != before
    }

    /// The clock runs: the job's progress goes out on a beat.
    fn ticking(&self) -> bool { self.active.as_ref().is_some_and(|a| a.run.ticking()) }

    /// Reads the taken job's G-code and starts its clock. This is the heavy step (one pass of
    /// the time model over the file): the link thread does it, after the report says the
    /// printer is preparing. `clock` gives the time once the schedule is built.
    fn start_pending(&mut self, clock: impl Fn() -> f64) -> bool {
        let Some(print) = self.pending.take() else { return false };
        let built = virtual_run::package_gcode(print.job.config_value("printer_model").unwrap_or(""), print.job.bytes())
            .and_then(|gcode| virtual_run::schedule(&gcode));
        match built {
            Ok(schedule) => {
                let name = self.reported_name(print.job.name());
                self.active = Some(Active { run: Run::new(Arc::new(schedule), print.trays, clock()), name });
            }
            Err(why) => {
                if let Some(job) = &mut self.snapshot.job { job.stage = JobStage::Failed(why); }
            }
        }
        self.refresh();
        self.sync_stage();
        true
    }

    fn set_speed(&mut self, speed: Speed, now: f64) -> bool {
        if speed == self.snapshot.virtual_speed { return false; }
        self.tick(now);
        self.snapshot.virtual_speed = speed;
        self.refresh();
        true
    }

    /// A pause, resume or stop on the job: why not, when it cannot.
    fn control(&mut self, now: f64, act: impl FnOnce(&mut Run, f64) -> Result<(), &'static str>) -> Result<(), String> {
        if self.pending.is_some() { return Err("the job is still being read".into()); }
        let Some(active) = &mut self.active else { return Err("no job is running".into()) };
        act(&mut active.run, now)?;
        self.refresh();
        self.sync_stage();
        Ok(())
    }

    fn command(&mut self, command: Command, now: f64) -> bool {
        let mut changed = self.tick(now);
        let (name, outcome) = match command {
            Command::Print(print) => {
                self.snapshot.job_sends += 1;
                if matches!(self.snapshot.command, Some(("print", _))) { self.snapshot.command = None; }
                let state = &self.snapshot.state;
                let why = if self.plate_check && !print.options.plate_clear { Some("Confirm the plate is clear first".into()) }
                    else if self.setup.numbered_heads { job::heads_fit(state, print.job.facts(), &print) }
                    else { job::preflight(state, &print) };
                let name = print.job.name().to_string();
                let stage = match why {
                    Some(why) => JobStage::Failed(why),
                    None => {
                        if print.options.plate_clear { self.plate_check = false; }
                        self.active = None;
                        self.pending = Some(print);
                        JobStage::Starting
                    }
                };
                self.snapshot.job = Some(JobStatus { name, file: String::new(), stage });
                self.refresh();
                return true;
            }
            Command::Pause => ("pause", self.control(now, |run, _| run.pause())),
            Command::Resume => ("resume", self.control(now, |run, at| run.resume(at))),
            Command::Stop => {
                if self.pending.take().is_some() {
                    if let Some(job) = &mut self.snapshot.job { job.stage = JobStage::Cancelled; }
                    self.refresh();
                    ("stop", Ok(()))
                } else {
                    let outcome = self.control(now, |run, _| run.stop());
                    if outcome.is_ok() { self.plate_check = true; }
                    ("stop", outcome)
                }
            }
            Command::ClearError => ("clean_print_error", Err("there is no printer error to clear".into())),
            Command::CheckStorage => {
                let why = "a virtual printer keeps no files";
                self.snapshot.storage_files = Some(Err(why.into()));
                ("check_storage", Err(why.into()))
            }
        };
        // A command that is done has moved the job; a refusal only changes the answer.
        let done = outcome.is_ok();
        let answer = Some((name, match outcome { Ok(()) => CommandState::Done, Err(why) => CommandState::Failed(why) }));
        changed |= done || answer != self.snapshot.command;
        self.snapshot.command = answer;
        changed
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn run(shared: Arc<Shared>, setup: VirtualSetup) {
    let origin = Instant::now();
    let seconds = || origin.elapsed().as_secs_f64();
    let mut printer = VirtualPrinter::new(setup);
    publish(&shared, &printer.snapshot);
    let mut published = Instant::now();
    while !shared.stop.load(Ordering::Acquire) {
        let mut changed = false;
        for command in shared.queued() { changed |= printer.command(command, seconds()); }
        let pending = shared.pending_virtual.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(setup) = pending { changed |= printer.reconfigure(setup); }
        let speed = shared.pending_speed.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(speed) = speed { changed |= printer.set_speed(speed, seconds()); }
        if changed {
            publish(&shared, &printer.snapshot);
            published = Instant::now();
        }
        if printer.start_pending(&seconds) {
            publish(&shared, &printer.snapshot);
            published = Instant::now();
        }
        let moved = printer.tick(seconds());
        if moved || (printer.ticking() && published.elapsed() >= CADENCE) {
            publish(&shared, &printer.snapshot);
            published = Instant::now();
        }
        if printer.ticking() {
            std::thread::park_timeout(CADENCE.saturating_sub(published.elapsed()).max(Duration::from_millis(1)));
        } else {
            std::thread::park();
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::{NozzleSide, PrintJob, PrinterLink};
    use std::time::{Duration, Instant};

    fn slot(head: usize, ams: u8, slot: u8) -> VirtualTray {
        VirtualTray { head, id: TrayId { ams, slot }, material: Some("PLA".into()), colour: Some(0x123456) }
    }

    fn setup(numbered_heads: bool, heads: usize, trays: Vec<VirtualTray>) -> VirtualSetup {
        VirtualSetup { name: "Virtual printer".into(), model: "Virtual model".into(), numbered_heads, nozzle_diameters: vec![0.4; heads], trays }
    }

    #[test]
    fn numbered_tools_report_their_spools() {
        let mut tools = setup(true, 4, (0..4).map(|h| slot(h, h as u8, 0)).collect());
        tools.trays[2].material = Some("PETG".into());
        tools.trays[3].material = None;
        let state = report(&tools);
        assert!(state.numbered_heads && state.ready_for_job() && state.storage == Some(true));
        assert_eq!((state.ams.len(), state.extruders.len()), (4, 4));
        for (h, unit) in state.ams.iter().enumerate() {
            let id = TrayId { ams: h as u8, slot: 0 };
            assert_eq!((unit.id, unit.trays.len(), unit.trays[0].id), (Some(id.ams), 1, Some(id)));
            assert_eq!(state.head_of(unit.extruder.unwrap()), Some(h));
            assert_eq!(state.extruders[h].loaded, (h != 3).then_some(LoadedSlot::Tray(id)));
        }
        let tray = state.tray(TrayId { ams: 2, slot: 0 }).unwrap();
        assert_eq!((tray.material.as_deref(), tray.colour), (Some("PETG"), Colour::parse("123456FF")));
        assert_eq!(state.ams[3].trays[0].present, Some(false));
    }

    #[test]
    fn nozzle_units_and_external_spools_report_their_sides() {
        let mut trays: Vec<_> = (0..4).map(|s| slot(0, 0, s)).collect();
        trays.push(slot(0, 254, 254));
        let mut sides = setup(false, 1, trays);
        let state = report(&sides);
        assert!(!state.numbered_heads);
        assert_eq!(state.nozzles[0].id, Some(CloudNozzleId(1)));
        assert_eq!(state.extruders[0].id, state.nozzles[0].id);
        assert_eq!((state.ams.len(), state.ams[0].trays.len(), state.external.len()), (1, 4, 1));
        assert_eq!(state.head_of(state.ams[0].extruder.unwrap()), Some(0));
        assert_eq!(state.external[0].id, Some(TrayId { ams: 254, slot: 254 }));
        sides.nozzle_diameters = vec![0.6, 0.25];
        sides.trays.extend([slot(1, 1, 0), slot(1, 255, 255)]);
        let state = report(&sides);
        assert_eq!(state.nozzles.iter().map(|n| n.diameter).collect::<Vec<_>>(), [Some(0.6), Some(0.25)]);
        assert_eq!(state.external.len(), 2);
        assert_eq!(state.head_of(state.ams[1].extruder.unwrap()), Some(1));
        for (head, side) in [NozzleSide::Left, NozzleSide::Right].into_iter().enumerate() {
            let id = (254 + head) as u8;
            assert_eq!(state.head_of(state.extruders[head].id.unwrap()), Some(head));
            assert!(state.trays().any(|(feed, t)| feed == Some(side) && t.id == Some(TrayId { ams: id, slot: id })));
        }
    }

    fn print_of(setup: &VirtualSetup, tray: usize) -> PrintJob {
        PrintJob { job: crate::safety::tests::checked_square("square"), trays: vec![Some(setup.trays[tray].id)], used: vec![0], options: Default::default() }
    }

    #[test]
    fn commands_use_the_reported_material_and_keep_their_answers() {
        for numbered in [false, true] {
            let mut s = setup(numbered, 2, vec![slot(0, 0, 0), slot(1, 1, 0)]);
            s.trays[1].material = Some("PETG".into());
            let print = print_of(&s, 1);
            let mut printer = VirtualPrinter::new(s.clone());
            printer.command(Command::Pause, 0.0);
            assert_eq!(printer.snapshot.command, Some(("pause", CommandState::Failed("no job is running".into()))));
            printer.command(Command::Print(print.clone()), 0.0);
            let job = printer.snapshot.job.clone().unwrap();
            let reason = if !numbered && !crate::printing_supported() { crate::UNSUPPORTED_PRINTING } else { "holds PETG" };
            assert!(matches!(&job.stage, JobStage::Failed(why) if why.contains(reason)), "{:?}", job.stage);
            assert_eq!(printer.snapshot.job_sends, 1);
            assert!(!printer.start_pending(|| 0.0), "a refused job is not read");
            s.trays[1].material = Some("PLA".into());
            s.trays[1].colour = Some(0xff2200);
            printer.reconfigure(s);
            assert_eq!(printer.snapshot.state.tray(print.trays[0].unwrap()).unwrap().colour, Colour::parse("FF2200FF"));
            assert_eq!(printer.snapshot.job.as_ref(), Some(&job));
            assert_eq!(printer.snapshot.command.as_ref().unwrap().0, "pause");
            printer.command(Command::Print(print), 0.0);
            assert_eq!(printer.snapshot.job_sends, 2);
            if !numbered && !crate::printing_supported() {
                assert!(matches!(&printer.snapshot.job.as_ref().unwrap().stage, JobStage::Failed(why) if why.contains(crate::UNSUPPORTED_PRINTING)));
            } else {
                assert_eq!(printer.snapshot.job.as_ref().unwrap().stage, JobStage::Starting);
                assert_eq!((printer.snapshot.state.gcode_state.as_deref(), printer.snapshot.state.job_name.as_deref()), (Some("PREPARE"), Some(if numbered { "square.gcode" } else { "square" })));
            }
        }
    }

    #[test]
    fn a_job_runs_through_its_stages_to_finish() {
        for numbered in [false, true] {
            if !numbered && !crate::printing_supported() { continue; }
            let s = setup(numbered, 2, vec![slot(0, 0, 0), slot(1, 1, 0)]);
            let mut printer = VirtualPrinter::new(s.clone());
            printer.command(Command::Print(print_of(&s, 1)), 0.0);
            assert!(printer.start_pending(|| 10.0));
            let state = printer.snapshot.state.clone();
            assert_eq!((state.gcode_state.as_deref(), state.print_seconds, state.layer), (Some("PREPARE"), Some(0.0), Some(0)));
            assert_eq!(printer.snapshot.job.as_ref().unwrap().stage, JobStage::Accepted);
            assert!(printer.ticking());
            printer.set_speed(Speed::ToEnd, 10.0);
            assert_eq!(printer.snapshot.virtual_speed, Speed::ToEnd);
            assert!(printer.tick(10.5), "finishing is a state change");
            let state = printer.snapshot.state.clone();
            assert_eq!(state.gcode_state.as_deref(), Some("FINISH"));
            assert_eq!((state.layer, state.progress, state.print_rate), (state.total_layers, Some(100.0), Some(0.0)));
            assert!(state.layer.unwrap() > 0 && state.print_seconds.unwrap() > 0.0, "{state:?}");
            assert_eq!(printer.snapshot.job.as_ref().unwrap().stage, JobStage::Finished);
            assert!(!printer.ticking());
            // The finished print stays on the report, and the next one waits for the plate.
            printer.command(Command::Print(print_of(&s, 1)), 11.0);
            assert!(matches!(&printer.snapshot.job.as_ref().unwrap().stage, JobStage::Failed(why) if why.contains("plate is clear")), "{:?}", printer.snapshot.job);
            assert_eq!(printer.snapshot.state.gcode_state.as_deref(), Some("FINISH"));
        }
    }

    #[test]
    fn pause_resume_and_stop_act_on_the_job() {
        for numbered in [false, true] {
            if !numbered && !crate::printing_supported() { continue; }
            let s = setup(numbered, 2, vec![slot(0, 0, 0), slot(1, 1, 0)]);
            let mut printer = VirtualPrinter::new(s.clone());
            printer.command(Command::Print(print_of(&s, 1)), 0.0);
            printer.command(Command::Pause, 0.0);
            assert_eq!(printer.snapshot.command, Some(("pause", CommandState::Failed("the job is still being read".into()))));
            printer.start_pending(|| 0.0);
            printer.command(Command::Resume, 0.1);
            assert_eq!(printer.snapshot.command, Some(("resume", CommandState::Failed("the printer is not paused".into()))));
            printer.command(Command::Pause, 0.2);
            assert_eq!(printer.snapshot.command, Some(("pause", CommandState::Done)));
            let held = printer.snapshot.state.print_seconds;
            assert_eq!((printer.snapshot.state.gcode_state.as_deref(), printer.snapshot.job.as_ref().unwrap().stage.clone()), (Some("PAUSE"), JobStage::Paused));
            printer.tick(500.0);
            assert_eq!(printer.snapshot.state.print_seconds, held, "no time passes while paused");
            printer.command(Command::Resume, 500.0);
            assert_eq!(printer.snapshot.command, Some(("resume", CommandState::Done)));
            assert_ne!(printer.snapshot.state.gcode_state.as_deref(), Some("PAUSE"));
            printer.command(Command::Stop, 500.1);
            assert_eq!(printer.snapshot.command, Some(("stop", CommandState::Done)));
            assert_eq!((printer.snapshot.state.gcode_state.as_deref(), printer.snapshot.job.as_ref().unwrap().stage.clone()), (Some("FAILED"), JobStage::Cancelled));
            printer.command(Command::Stop, 502.0);
            assert_eq!(printer.snapshot.command, Some(("stop", CommandState::Failed("no job is running".into()))));
            let mut again = print_of(&s, 1);
            printer.command(Command::Print(again.clone()), 503.0);
            assert!(matches!(&printer.snapshot.job.as_ref().unwrap().stage, JobStage::Failed(why) if why.contains("plate is clear")), "{:?}", printer.snapshot.job);
            again.options.plate_clear = true;
            printer.command(Command::Print(again), 504.0);
            assert_eq!(printer.snapshot.job.as_ref().unwrap().stage, JobStage::Starting);
        }
    }

    fn until(link: &mut PrinterLink, what: &str, ready: impl Fn(&Snapshot) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if link.poll().is_some_and(|s| ready(&s)) { return; }
            assert!(Instant::now() < deadline, "waiting for {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn link_publishes_reconfigured_spools() {
        let mut s = setup(false, 1, vec![slot(0, 0, 0)]);
        let mut link = PrinterLink::virtual_printer(s.clone());
        until(&mut link, "virtual printer", |s| s.status == ConnectionStatus::Connected && s.virtual_printer);
        s.trays[0].colour = Some(0xabcdef);
        link.set_virtual(s);
        until(&mut link, "edited tray", |s| s.state.tray(TrayId { ams: 0, slot: 0 }).and_then(|t| t.colour) == Colour::parse("ABCDEFFF"));
    }

    #[test]
    fn link_runs_a_job_to_its_finish() {
        let s = setup(true, 2, vec![slot(0, 0, 0), slot(1, 1, 0)]);
        let mut link = PrinterLink::virtual_printer(s.clone());
        until(&mut link, "virtual printer", |s| s.status == ConnectionStatus::Connected && s.virtual_printer);
        link.set_virtual_speed(Speed::ToEnd);
        link.send(Command::Print(print_of(&s, 1)));
        until(&mut link, "the job's finish", |s| s.virtual_speed == Speed::ToEnd
            && s.state.gcode_state.as_deref() == Some("FINISH") && s.job.as_ref().is_some_and(|j| j.stage == JobStage::Finished));
    }
}
