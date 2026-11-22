//! A print job's life after the user confirms it: upload to the printer's
//! storage, the start command, the printer taking it, then its progress until
//! it finishes, fails or is stopped. Pure state: the link worker feeds it the
//! upload result, acknowledgements, reports and the time.

use crate::model::{hardened, CloudNozzleId, NozzleSide, PrinterState, TrayId};
#[cfg(not(target_arch = "wasm32"))]
use crate::model::Ack;
#[cfg(not(target_arch = "wasm32"))]
use crate::mqtt::ProjectFile;
use crate::safety::ValidatedJob;
#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, Instant};

/// The plain G-code file a second-family job is uploaded as, and the name that printer reports
/// it by: the first family's safe stem with `.gcode` (a name that ends in `.gcode` keeps one).
pub fn plain_file_name(name: &str) -> String {
    let stem = name.strip_suffix(".gcode").unwrap_or(name);
    crate::ftps::job_file_name(stem).trim_end_matches(".3mf").to_string()
}

/// What the user confirmed to print. The file is a [`ValidatedJob`]: only the
/// safety check makes one, so nothing unchecked can be uploaded or started.
#[derive(Clone, Debug)]
pub struct PrintJob {
    pub job: ValidatedJob,
    /// Per project filament (slicer order): the tray that feeds it.
    pub trays: Vec<Option<TrayId>>,
    /// The filaments the plate prints with (0-based); only these need a tray.
    pub used: Vec<usize>,
    pub options: PrintOptions,
}

/// What the user set in the confirm row; only flow calibration and timelapse
/// go to the printer. Homing and bed levelling are not options: the
/// profile's own start sequence always homes and probes (that is what makes
/// Z = 0 the plate), and the start command always asks for levelling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PrintOptions {
    pub flow_calibration: bool,
    pub timelapse: bool,
    /// The user confirmed the plate is clear (needed after a finished or failed print).
    pub plate_clear: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum JobStage {
    Uploading { sent: u64, total: u64 },
    /// Uploaded; the start command is out, waiting for the printer's answer.
    Starting,
    /// The start may have arrived; awaiting fresh status, never resending it.
    StartReplyLost,
    /// The printer accepted the command and is preparing.
    Accepted,
    Printing,
    Paused,
    Finished,
    Failed(String),
    Cancelled,
}

impl JobStage {
    pub fn active(&self) -> bool { !matches!(self, Self::Finished | Self::Failed(_) | Self::Cancelled) }

    pub fn label(&self) -> String {
        match self {
            Self::Uploading { sent, total } => format!("Uploading {:.0} %", 100.0 * *sent as f64 / (*total).max(1) as f64),
            Self::Starting => "Starting the print".into(),
            Self::StartReplyLost => "Start reply lost: waiting for the printer's state".into(),
            Self::Accepted => "Printer is preparing".into(),
            Self::Printing => "Printing".into(),
            Self::Paused => "Paused".into(),
            Self::Finished => "Finished".into(),
            Self::Failed(why) => format!("Failed: {why}"),
            Self::Cancelled => "Stopped".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct JobStatus {
    pub name: String,
    pub file: String,
    pub stage: JobStage,
}

/// Global tray index of the start command's `ams_mapping` (-1: external or none).
pub fn global_tray(tray: Option<TrayId>) -> i32 {
    match tray {
        Some(TrayId { ams: a @ 0..=3, slot: s @ 0..=3 }) => a as i32 * 4 + s as i32,
        Some(TrayId { ams: a @ 128..=253, slot: 0 }) => 16 + (a - 128) as i32,
        _ => -1,
    }
}

/// `ams_mapping2`: AMS trays as they are, the external spools as (254 | 255, 0), none as (255, 255).
pub fn tray_pair(tray: Option<TrayId>) -> (u8, u8) {
    match tray {
        Some(TrayId { ams: a @ (254 | 255), .. }) => (a, 0),
        Some(t) => (t.ams, t.slot),
        None => (255, 255),
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn project_file(job: &PrintJob, file: &str) -> ProjectFile {
    ProjectFile {
        file: file.to_string(), plate: 1, md5: job.job.md5().to_string(), name: job.job.name().to_string(),
        ams_mapping: job.trays.iter().map(|t| global_tray(*t)).collect(),
        ams_mapping2: job.trays.iter().map(|t| tray_pair(*t)).collect(),
        use_ams: job.used.iter().any(|i| job.trays.get(*i).copied().flatten().is_some_and(|t| t.ams < 254)),
        bed_leveling: true, flow_calibration: job.options.flow_calibration,
        vibration_calibration: false, layer_inspect: false, timelapse: job.options.timelapse,
    }
}

/// Why the printer cannot take any job now (None: it can).
pub fn printer_ready(state: &PrinterState) -> Option<String> {
    match state.storage {
        Some(true) => {}
        Some(false) => return Some("Insert a USB drive into the printer: it has no storage for the job".into()),
        None => return Some("The printer has not reported its storage yet".into()),
    }
    if !state.ready_for_job() {
        return Some(match state.gcode_state.as_deref() {
            Some(s) => format!("The printer is busy ({})", s.to_lowercase()),
            None => "The printer has not reported its state yet".into(),
        });
    }
    if state.serious_hms() { return Some("The printer reports a serious error: clear it on the printer first".into()); }
    if let Some(e) = state.print_error.filter(|e| *e != 0) { return Some(format!("The printer shows error {e:08X}: clear it first")); }
    None
}

/// Why the printer cannot take `job` now (None: it can).
pub fn preflight(state: &PrinterState, job: &PrintJob) -> Option<String> {
    if !crate::printing_supported() { return Some(crate::UNSUPPORTED_PRINTING.into()); }
    if let Some(why) = printer_ready(state) { return Some(why); }
    if !job.job.intact() { return Some("The checked file changed: slice and check again".into()); }
    if job.job.bytes().len() > 1 << 30 { return Some("The job is larger than 1 GB".into()); }
    job_fits(state, job.job.facts(), &job.trays, &job.used, &job.options)
}

/// Why the printer's own report disagrees with a checked file's `facts` on these trays
/// (None: it agrees).
pub fn job_fits(state: &PrinterState, facts: &crate::safety::JobFacts, trays: &[Option<TrayId>], used: &[usize], options: &PrintOptions) -> Option<String> {
    if state.numbered_heads { return selected_heads_fit(state, facts, trays, used, options); }
    // Its nozzles, by side.
    for (extruder, want) in facts.nozzle_diameters.iter().enumerate() {
        // Extruder 0 is the left nozzle, 1 the right (cloud ids: right 0, left 1).
        let side = if extruder == 0 { crate::model::NozzleSide::Left } else { crate::model::NozzleSide::Right };
        let reported = state.nozzles.iter().find(|n| n.id.and_then(crate::model::CloudNozzleId::side) == Some(side)).and_then(|n| n.diameter);
        match reported {
            Some(d) if (d - want).abs() < 1e-3 => {}
            Some(d) => return Some(format!("The {} nozzle is {d} mm, the file is for {want} mm", side.label().to_lowercase())),
            None if facts.extruders_used.contains(&extruder) => return Some(format!("The printer has not reported its {} nozzle", side.label().to_lowercase())),
            None => {}
        }
    }
    if used.is_empty() { return Some("The job uses no filament".into()); }
    for &i in used {
        let Some(t) = trays.get(i).copied().flatten() else {
            return Some(format!("Filament {} has no matching tray on its nozzle's side: load it, or change the filament", i + 1));
        };
        if state.tray(t).is_none_or(|tray| tray.present == Some(false)) {
            return Some(format!("Filament {}'s tray is empty or gone", i + 1));
        }
    }
    if let Some(last @ ("FINISH" | "FAILED")) = state.gcode_state.as_deref() {
        if !options.plate_clear {
            return Some(format!("The printer shows its last print as {}: confirm the plate is clear first", if last == "FINISH" { "finished" } else { "failed" }));
        }
    }
    for &i in used {
        let n = i + 1;
        let Some(f) = facts.filaments.get(i) else { return Some(format!("Filament {n} is not in the checked file")); };
        let id = trays[i].unwrap();
        let t = state.tray(id).unwrap();
        let kind = &f.kind;
        let Some(material) = t.material.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
            return Some(format!("Filament {n}'s tray reports no material: set it on the printer"));
        };
        if !material.eq_ignore_ascii_case(kind) {
            return Some(format!("Filament {n} is {kind}, but its tray holds {material}: load {kind} there, or change the filament"));
        }
        if let (Some(lo), Some(hi)) = (t.nozzle_temp_min, t.nozzle_temp_max) {
            let [a, b] = f.print_temps;
            if lo > 0.0 && hi > 0.0 && lo <= hi && (a < lo || b > hi) {
                return Some(format!("Filament {n} prints at {a:.0}-{b:.0} °C, but the spool in its tray is rated {lo:.0}-{hi:.0} °C"));
            }
        }
        let side = if f.extruder == 0 { NozzleSide::Left } else { NozzleSide::Right };
        if let Some(feed) = state.ams.iter().find(|a| a.id == Some(id.ams)).and_then(|a| a.extruder).and_then(CloudNozzleId::side) {
            if feed != side {
                return Some(format!("Filament {n}'s tray feeds the {} nozzle, but the file prints it on the {}", feed.label().to_lowercase(), side.label().to_lowercase()));
            }
        }
        if f.needs_hardened {
            let nozzle = state.nozzles.iter().find(|n| n.id.and_then(CloudNozzleId::side) == Some(side)).and_then(|n| n.kind.as_deref());
            let side = side.label().to_lowercase();
            match nozzle.and_then(hardened) {
                Some(true) => {}
                Some(false) => return Some(format!("Filament {n} ({kind}) needs a hardened nozzle; the {side} nozzle is {}", nozzle.unwrap())),
                None => return Some(format!("Filament {n} ({kind}) needs a hardened nozzle; the printer has not reported what the {side} nozzle is")),
            }
        }
    }
    None
}

/// Why a job cannot print on its selected physical heads (None: it can).
pub fn heads_fit(state: &PrinterState, facts: &crate::safety::JobFacts, job: &PrintJob) -> Option<String> {
    if let Some(why) = printer_ready(state) { return Some(why); }
    if !job.job.intact() { return Some("The checked file changed: slice and check again".into()); }
    if job.job.bytes().len() > 1 << 30 { return Some("The job is larger than 1 GB".into()); }
    selected_heads_fit(state, facts, &job.trays, &job.used, &job.options)
}

fn selected_heads_fit(state: &PrinterState, facts: &crate::safety::JobFacts, trays: &[Option<TrayId>], used: &[usize], options: &PrintOptions) -> Option<String> {
    if matches!(state.gcode_state.as_deref(), Some("FINISH" | "FAILED")) && !options.plate_clear {
        return Some("The last print ended: confirm the plate is clear first".into());
    }
    if used.is_empty() { return Some("The job uses no filament".into()); }
    let mut heads = std::collections::HashSet::new();
    for &i in used {
        let n = i + 1;
        let Some(filament) = facts.filaments.get(i) else { return Some(format!("Filament {n} is not in the checked file")); };
        let Some(id) = trays.get(i).copied().flatten() else { return Some(format!("Filament {n} has no selected head")); };
        let h = u16::from(id.ams) + 1;
        if id.slot != 0 || !state.extruders.iter().any(|e| e.id == Some(CloudNozzleId(id.ams))) {
            return Some(format!("Filament {n}'s head {h} is not reported by the printer"));
        }
        if !heads.insert(id.ams) { return Some(format!("Filament {n} shares head {h} with another filament: pick another head")); }
        let tray = state.tray(id);
        if tray.and_then(|t| t.present) != Some(true) {
            return Some(format!("Filament {n}'s head {h} is empty or has not reported filament: load it there"));
        }
        let Some(material) = tray.and_then(|t| t.material.as_deref()).filter(|s| !s.is_empty()) else {
            return Some(format!("Filament {n}'s head {h} reports no material: set it on the printer"));
        };
        let kind = &filament.kind;
        if !material.eq_ignore_ascii_case(kind) {
            return Some(format!("Filament {n} is {kind}, but head {h} holds {material}: load {kind} there, or pick another head"));
        }
    }
    None
}

/// What the worker should do next for the job.
#[derive(Debug, PartialEq)]
#[cfg(not(target_arch = "wasm32"))]
pub(crate) enum Effect {
    None,
    /// Send the start command.
    Start(ProjectFile),
    /// Ask for a full report (no answer yet).
    PushAll,
}

#[cfg(not(target_arch = "wasm32"))]
const ACK_WAIT: Duration = Duration::from_secs(10);
#[cfg(not(target_arch = "wasm32"))]
const ACCEPT_WAIT: Duration = Duration::from_secs(90);

#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct Tracker {
    pub(crate) status: JobStatus,
    start: Option<ProjectFile>,
    sequence: Option<u64>,
    sent_at: Option<Instant>,
    asked_again: bool,
    stop: bool,
    /// The state before the job, so a stale FINISH/FAILED of the last job is not this one's.
    seen_busy: bool,
}

/// A refusal that names signing or authorisation means control is locked on the printer.
#[cfg(not(target_arch = "wasm32"))]
fn developer_mode_hint(reason: &str) -> String {
    let lower = reason.to_ascii_lowercase();
    if lower.contains("sign") || lower.contains("auth") || lower.contains("verify") {
        format!("{reason}. Turn on LAN-only Developer Mode on the printer")
    } else {
        reason.to_string()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Tracker {
    pub(crate) fn new(job: &PrintJob, file: String) -> Self {
        let total = job.job.bytes().len() as u64;
        Self {
            status: JobStatus { name: job.job.name().to_string(), file: file.clone(), stage: JobStage::Uploading { sent: 0, total } },
            start: Some(project_file(job, &file)), sequence: None, sent_at: None, asked_again: false, stop: false, seen_busy: false,
        }
    }

    pub(crate) fn uploading(&mut self, sent: u64) {
        if let JobStage::Uploading { total, .. } = self.status.stage { self.status.stage = JobStage::Uploading { sent: sent.min(total), total }; }
    }

    /// The upload ended: Ok sends the start command.
    pub(crate) fn uploaded(&mut self, result: Result<(), String>) -> Effect {
        if !matches!(self.status.stage, JobStage::Uploading { .. }) { return Effect::None; }
        match result {
            Ok(()) => {
                self.status.stage = JobStage::Starting;
                self.start.take().map(Effect::Start).unwrap_or(Effect::None)
            }
            Err(e) => { self.status.stage = JobStage::Failed(e); Effect::None }
        }
    }

    pub(crate) fn sent(&mut self, sequence: u64, now: Instant) {
        self.sequence = Some(sequence);
        self.sent_at = Some(now);
    }

    /// The user stopped the job: the printer's FAILED or IDLE that follows is a stop.
    pub(crate) fn stop_requested(&mut self) { self.stop = true; }

    /// The printer refused the stop: what it reports next is not a stop.
    pub(crate) fn stop_refused(&mut self) { self.stop = false; }

    pub(crate) fn fail(&mut self, why: String) {
        if self.status.stage.active() { self.status.stage = JobStage::Failed(why); }
    }

    pub(crate) fn cancel(&mut self) {
        if self.status.stage.active() { self.status.stage = JobStage::Cancelled; }
    }

    pub(crate) fn ack(&mut self, ack: &Ack) {
        if Some(ack.sequence) != self.sequence || ack.command != "project_file" { return; }
        if self.status.stage != JobStage::Starting { return; }
        match ack.ok {
            Some(false) => self.status.stage = JobStage::Failed(developer_mode_hint(&match &ack.reason {
                Some(r) => format!("the printer refused the job ({r})"),
                None => "the printer refused the job".into(),
            })),
            Some(true) => self.status.stage = JobStage::Accepted,
            // An echo says neither: the printer's reports tell whether the job started.
            None => {}
        }
    }

    /// A report or a clock tick.
    pub(crate) fn advance(&mut self, state: &PrinterState, now: Instant) -> Effect {
        let ours = state.job_name.as_deref().is_some_and(|n| n == self.status.name || n == self.status.file || Some(n) == self.status.file.strip_suffix(".gcode.3mf"));
        let gcode = state.gcode_state.as_deref();
        if state.busy() && (ours || self.status.stage == JobStage::Accepted) { self.seen_busy = true; }
        match self.status.stage {
            JobStage::Starting | JobStage::Accepted => {
                if self.seen_busy {
                    self.status.stage = match gcode { Some("PAUSE") => JobStage::Paused, Some("RUNNING") => JobStage::Printing, _ => JobStage::Accepted };
                    return Effect::None;
                }
                let waited = self.sent_at.map(|t| now.duration_since(t)).unwrap_or_default();
                if self.status.stage == JobStage::Starting && waited >= ACK_WAIT && !self.asked_again {
                    // No answer: a report tells whether it started (never resend the start).
                    self.asked_again = true;
                    return Effect::PushAll;
                }
                if waited >= ACCEPT_WAIT {
                    self.status.stage = JobStage::Failed("the printer did not start the job".into());
                }
                Effect::None
            }
            JobStage::Printing | JobStage::Paused => {
                self.status.stage = match gcode {
                    Some("RUNNING" | "PREPARE") => JobStage::Printing,
                    Some("PAUSE") => JobStage::Paused,
                    Some("FINISH") => JobStage::Finished,
                    Some("FAILED" | "IDLE") if self.stop => JobStage::Cancelled,
                    Some("FAILED") => JobStage::Failed(match state.print_error.filter(|e| *e != 0) {
                        Some(e) => format!("the printer stopped with error {e:08X}"),
                        None => "the print stopped".into(),
                    }),
                    Some("IDLE") => JobStage::Failed("the printer went idle without finishing: stopped on the printer, or aborted".into()),
                    _ => self.status.stage.clone(),
                };
                Effect::None
            }
            _ => Effect::None,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::Reports;

    #[test]
    fn uploaded_job_names_keep_the_existing_safe_stem() {
        for name in ["Block", "Block.gcode", "Block.gcode.3mf", "a/b", "", "a b"] {
            let stem = name.strip_suffix(".gcode").unwrap_or(name);
            assert_eq!(plain_file_name(name), crate::ftps::job_file_name(stem).trim_end_matches(".3mf"));
        }
        assert_eq!(plain_file_name("Block"), "Block.gcode");
        assert_eq!(plain_file_name("Block.gcode"), "Block.gcode");
    }

    fn state(json: &str) -> PrinterState { Reports::default().apply(json.as_bytes()).unwrap().unwrap() }
    fn idle() -> PrinterState {
        state(r#"{"print":{"gcode_state":"IDLE","sdcard":true,"device":{"nozzle":{"info":[{"id":0,"diameter":0.4},{"id":1,"diameter":0.4}]}},"ams":{"tray_exist_bits":"1","ams":[{"id":"0","info":"1003","tray":[{"id":"0","tray_type":"PLA","tray_color":"FF0000FF"},{"id":"1"}]}]}}}"#)
    }
    fn job(trays: Vec<Option<TrayId>>) -> PrintJob {
        let used = (0..trays.len()).collect();
        PrintJob { job: crate::safety::tests::checked_square("cube"), trays, used, options: PrintOptions::default() }
    }

    /// A small four-filament file validated through the public safety gate.
    pub(crate) fn heads_job() -> PrintJob {
        use crate::safety::{validate_gcode, MachineProfile};
        let area = crate::safety::parse_points("0x0,40x0,40x40,0x40");
        let profile = MachineProfile {
            flavor: makepad_fab_settings::family::Flavor::Named,
            printer_model: makepad_fab_settings::family::FAMILIES[1].models[0].into(), printable_area: area.clone(), printable_height: 40.0,
            extruder_areas: vec![area; 4], extruder_heights: vec![40.0; 4], extruder_offsets: vec![[0.0, 0.0]; 4],
            nozzle_diameters: vec![0.4; 4], filament_diameters: vec![1.75; 4], max_volumetric: vec![25.0; 4],
            filament_types: vec!["PLA".into(); 4], nozzle_temp_ranges: vec![[190.0, 240.0]; 4], print_temps: vec![[220.0; 2]; 4],
            templates: vec![("machine_start_gcode".into(), "G28\nG90\nM83\n".into())], prime_allowance: 5.0,
            ..Default::default()
        };
        let mut text = String::from("; EXECUTABLE_BLOCK_START\nG28\nG90\nM83\n");
        for i in 0..4 {
            let y = 5 + i * 5;
            text.push_str(&format!("T{i}\nG1 Z0.6 F1200\nG1 X5 Y{y} F3000\nG1 Z0.2\nG1 X15 E0.4 F600\n"));
        }
        text.push_str(&format!("; EXECUTABLE_BLOCK_END\n; filament used [mm] = 0.4, 0.4, 0.4, 0.4\n; filament used [g] = 0.01, 0.01, 0.01, 0.01\n; CONFIG_BLOCK_START\n; printer_model = {}\n; printable_area = 0x0,40x0,40x40,0x40\n; printable_height = 40\n; nozzle_diameter = 0.4,0.4,0.4,0.4\n; extruder_offset = 0x0,0x0,0x0,0x0\n; filament_map = 1,2,3,4\n; machine_start_gcode = G28\\nG90\\nM83\\n\n; CONFIG_BLOCK_END\n", profile.printer_model));
        let job = validate_gcode(text.into_bytes(), &profile, "four.gcode").unwrap_or_else(|e| panic!("{}", e.message()));
        PrintJob { job, trays: (0..4).map(|h| Some(TrayId { ams: h, slot: 0 })).collect(), used: vec![0, 1, 2, 3], options: Default::default() }
    }
    fn heads() -> PrinterState {
        use crate::model::{AmsUnit, Extruder, Tray};
        PrinterState {
            gcode_state: Some("IDLE".into()), storage: Some(true),
            extruders: (0..4).map(|h| Extruder { id: Some(CloudNozzleId(h)), ..Default::default() }).collect(),
            ams: (0..4).map(|h| AmsUnit { id: Some(h), trays: vec![Tray { id: Some(TrayId { ams: h, slot: 0 }), present: Some(true), material: Some("PLA".into()), ..Default::default() }], ..Default::default() }).collect(),
            ..Default::default()
        }
    }
    #[test]
    fn head_holding_wrong_material_refuses_the_named_filament() {
        let j = heads_job();
        let mut s = heads();
        s.ams[2].trays[0].material = Some("PETG".into());
        assert_eq!(heads_fit(&s, j.job.facts(), &j).as_deref(), Some("Filament 3 is PLA, but head 3 holds PETG: load PLA there, or pick another head"));
    }
    #[test]
    fn empty_head_and_unknown_filament_refuse() {
        let j = heads_job();
        for present in [Some(false), None] {
            let mut s = heads(); s.ams[0].trays[0].present = present;
            assert!(heads_fit(&s, j.job.facts(), &j).unwrap().contains("Filament 1's head 1 is empty"));
        }
    }
    #[test]
    fn two_filaments_on_one_head_refuse() {
        let mut j = heads_job(); j.trays[1] = j.trays[0];
        assert!(heads_fit(&heads(), j.job.facts(), &j).unwrap().contains("Filament 2 shares head 1"));
    }
    #[test]
    fn printing_printer_refuses_a_head_job() {
        let j = heads_job(); let mut s = heads(); s.gcode_state = Some("RUNNING".into());
        assert!(heads_fit(&s, j.job.facts(), &j).unwrap().contains("busy"));
    }
    #[test]
    fn finish_and_failure_require_a_clear_plate() {
        let mut j = heads_job(); let mut s = heads();
        for last in ["FINISH", "FAILED"] {
            s.gcode_state = Some(last.into()); j.options.plate_clear = false;
            assert!(heads_fit(&s, j.job.facts(), &j).unwrap().contains("confirm the plate is clear"));
            j.options.plate_clear = true; assert_eq!(heads_fit(&s, j.job.facts(), &j), None);
        }
    }
    #[test]
    fn head_assignment_and_material_must_be_reported() {
        let j = heads_job(); let s = heads();
        assert_eq!(heads_fit(&s, j.job.facts(), &j), None);
        let mut empty = j.clone(); empty.used.clear();
        assert_eq!(heads_fit(&s, j.job.facts(), &empty).as_deref(), Some("The job uses no filament"));
        for id in [None, Some(TrayId { ams: 9, slot: 0 }), Some(TrayId { ams: 0, slot: 1 })] {
            let mut missing = j.clone(); missing.trays[0] = id;
            assert!(heads_fit(&s, j.job.facts(), &missing).unwrap().contains("Filament 1"));
        }
        let mut missing = j.clone(); missing.used = vec![9];
        assert!(heads_fit(&s, j.job.facts(), &missing).unwrap().contains("not in the checked file"));
        let mut s = s; s.ams[0].trays[0].material = None;
        assert!(heads_fit(&s, j.job.facts(), &j).unwrap().contains("head 1 reports no material"));
        s.ams[0].trays[0].material = Some("pla".into());
        assert_eq!(heads_fit(&s, j.job.facts(), &j), None);
        s.extruders.remove(0);
        assert!(heads_fit(&s, j.job.facts(), &j).unwrap().contains("head 1 is not reported"));
    }

    #[test]
    fn mapping_encodings() {
        assert_eq!(global_tray(Some(TrayId { ams: 1, slot: 2 })), 6);
        assert_eq!(global_tray(Some(TrayId { ams: 129, slot: 0 })), 17);
        assert_eq!(global_tray(Some(TrayId { ams: 255, slot: 255 })), -1);
        assert_eq!(global_tray(None), -1);
        assert_eq!(tray_pair(Some(TrayId { ams: 254, slot: 254 })), (254, 0));
        assert_eq!(tray_pair(None), (255, 255));
        let pf = project_file(&job(vec![Some(TrayId { ams: 0, slot: 1 }), Some(TrayId { ams: 255, slot: 255 })]), "cube.gcode.3mf");
        assert_eq!(pf.ams_mapping, [1, -1]);
        assert!(pf.use_ams);
        assert!(!project_file(&job(vec![Some(TrayId { ams: 255, slot: 255 })]), "x").use_ams);
    }

    #[test]
    fn preflight_blocks_without_storage_busy_or_unmapped() {
        let ok = job(vec![Some(TrayId { ams: 0, slot: 0 })]);
        if !crate::printing_supported() {
            assert_eq!(preflight(&idle(), &ok).as_deref(), Some(crate::UNSUPPORTED_PRINTING));
            return;
        }
        assert_eq!(preflight(&idle(), &ok), None);
        let mut s = idle(); s.gcode_state = Some("FINISH".into()); assert!(preflight(&s, &ok).unwrap().contains("plate is clear"));
        let mut clear = ok.clone(); clear.options.plate_clear = true; assert_eq!(preflight(&s, &clear), None);
        let mut s = idle(); s.ams[0].trays[0].material = Some("PETG".into()); assert!(preflight(&s, &ok).unwrap().contains("PETG"));
        let mut s = idle();
        s.storage = Some(false);
        assert!(preflight(&s, &ok).unwrap().contains("USB drive"));
        s.storage = None;
        assert!(preflight(&s, &ok).is_some());
        let mut s = idle();
        s.gcode_state = Some("RUNNING".into());
        assert!(preflight(&s, &ok).unwrap().contains("busy"));
        assert!(preflight(&idle(), &job(vec![None])).unwrap().contains("Filament 1"));
        assert!(preflight(&idle(), &job(vec![Some(TrayId { ams: 0, slot: 1 })])).unwrap().contains("empty"));
        let mut unused = job(vec![Some(TrayId { ams: 0, slot: 0 }), None]);
        unused.used = vec![0];
        assert_eq!(preflight(&idle(), &unused), None, "an unused filament needs no tray");
        // The live report must agree with the checked file.
        let mut s = idle();
        s.nozzles[0].diameter = Some(0.6);
        assert!(preflight(&s, &ok).unwrap().contains("right nozzle is 0.6"));
        let mut s = idle();
        s.nozzles.clear();
        assert!(preflight(&s, &ok).unwrap().contains("not reported"));
        let mut s = idle();
        s.print_error = Some(0x0300800D);
        assert!(preflight(&s, &ok).unwrap().contains("0300800D"));
        let aux = state(r#"{"print":{"gcode_state":"FINISH","aux":"2501004"}}"#);
        assert_eq!(aux.storage, Some(true));
        let none = state(r#"{"print":{"gcode_state":"IDLE","sdcard":false,"aux":"2500004"}}"#);
        assert_eq!(none.storage, Some(false));
    }

    #[test]
    fn tracker_upload_start_accept_print_finish() {
        let t0 = Instant::now();
        let j = job(vec![Some(TrayId { ams: 0, slot: 0 })]);
        let total = j.job.bytes().len() as u64;
        let mut t = Tracker::new(&j, "cube.gcode.3mf".into());
        t.uploading(2);
        assert_eq!(t.status.stage, JobStage::Uploading { sent: 2, total });
        let Effect::Start(pf) = t.uploaded(Ok(())) else { panic!("start after upload") };
        assert_eq!(pf.file, "cube.gcode.3mf");
        assert_eq!(t.uploaded(Ok(())), Effect::None, "one start only");
        t.sent(70005, t0);
        // A stale FINISH of the last job is not ours.
        let finish_old = state(r#"{"print":{"gcode_state":"FINISH","subtask_name":"old"}}"#);
        assert_eq!(t.advance(&finish_old, t0 + Duration::from_secs(1)), Effect::None);
        assert_eq!(t.status.stage, JobStage::Starting);
        t.ack(&Ack { command: "project_file".into(), sequence: 70004, ok: Some(false), reason: None });
        assert_eq!(t.status.stage, JobStage::Starting, "another sequence is not our answer");
        t.ack(&Ack { command: "project_file".into(), sequence: 70005, ok: Some(true), reason: None });
        assert_eq!(t.status.stage, JobStage::Accepted);
        t.advance(&state(r#"{"print":{"gcode_state":"PREPARE","subtask_name":"cube"}}"#), t0 + Duration::from_secs(2));
        assert_eq!(t.status.stage, JobStage::Accepted);
        t.advance(&state(r#"{"print":{"gcode_state":"RUNNING","subtask_name":"cube"}}"#), t0 + Duration::from_secs(30));
        assert_eq!(t.status.stage, JobStage::Printing);
        t.advance(&state(r#"{"print":{"gcode_state":"PAUSE","subtask_name":"cube"}}"#), t0 + Duration::from_secs(31));
        assert_eq!(t.status.stage, JobStage::Paused);
        t.advance(&state(r#"{"print":{"gcode_state":"FINISH","subtask_name":"cube"}}"#), t0 + Duration::from_secs(32));
        assert_eq!(t.status.stage, JobStage::Finished);
        assert!(!t.status.stage.active());
    }

    #[test]
    fn tracker_refusal_silence_and_failures() {
        let t0 = Instant::now();
        let j = job(vec![Some(TrayId { ams: 0, slot: 0 })]);
        let mut t = Tracker::new(&j, "cube.gcode.3mf".into());
        t.uploaded(Err("printer storage is full".into()));
        assert_eq!(t.status.stage, JobStage::Failed("printer storage is full".into()));

        let mut t = Tracker::new(&j, "cube.gcode.3mf".into());
        t.uploaded(Ok(()));
        t.sent(70010, t0);
        t.ack(&Ack { command: "project_file".into(), sequence: 70010, ok: Some(false), reason: Some("signature verify failed".into()) });
        let JobStage::Failed(why) = &t.status.stage else { panic!("refused") };
        assert!(why.contains("Developer Mode"));

        let mut t = Tracker::new(&j, "cube.gcode.3mf".into());
        t.uploaded(Ok(()));
        t.sent(70011, t0);
        let idle = state(r#"{"print":{"gcode_state":"IDLE"}}"#);
        assert_eq!(t.advance(&idle, t0 + Duration::from_secs(11)), Effect::PushAll);
        assert_eq!(t.advance(&idle, t0 + Duration::from_secs(12)), Effect::None, "asks once, never resends the start");
        // Silence, then the printer reports our job running: it started.
        t.advance(&state(r#"{"print":{"gcode_state":"RUNNING","subtask_name":"cube"}}"#), t0 + Duration::from_secs(13));
        assert_eq!(t.status.stage, JobStage::Printing);
        t.advance(&state(r#"{"print":{"gcode_state":"FAILED","print_error":50364429}}"#), t0 + Duration::from_secs(20));
        assert_eq!(t.status.stage, JobStage::Failed("the printer stopped with error 0300800D".into()));

        let mut t = Tracker::new(&j, "cube.gcode.3mf".into());
        t.uploaded(Ok(()));
        t.sent(70012, t0);
        assert_eq!(t.advance(&idle, t0 + Duration::from_secs(91)), Effect::PushAll);
        t.advance(&idle, t0 + Duration::from_secs(92));
        assert_eq!(t.status.stage, JobStage::Failed("the printer did not start the job".into()));
    }

    #[test]
    fn numbered_job_fits_uses_the_selected_heads_check() {
        let mut job = heads_job();
        let mut state = heads();
        state.numbered_heads = true;
        let check = |state: &PrinterState, job: &PrintJob, expected: Option<&str>| {
            let actual = job_fits(state, job.job.facts(), &job.trays, &job.used, &job.options);
            assert_eq!(actual.as_deref(), expected);
            assert_eq!(actual, heads_fit(state, job.job.facts(), job));
        };
        check(&state, &job, None);
        state.ams[2].trays[0].present = Some(false);
        check(&state, &job, Some("Filament 3's head 3 is empty or has not reported filament: load it there"));
        state.ams[2].trays[0].present = Some(true);
        state.ams[2].trays[0].material = Some("PETG".into());
        check(&state, &job, Some("Filament 3 is PLA, but head 3 holds PETG: load PLA there, or pick another head"));
        state.ams[2].trays[0].material = Some("PLA".into());
        job.trays[1] = job.trays[0];
        check(&state, &job, Some("Filament 2 shares head 1 with another filament: pick another head"));
        job.trays[1] = Some(TrayId { ams: 1, slot: 0 });
        for last in ["FINISH", "FAILED"] {
            state.gcode_state = Some(last.into());
            job.options.plate_clear = false;
            check(&state, &job, Some("The last print ended: confirm the plate is clear first"));
            job.options.plate_clear = true;
            check(&state, &job, None);
        }
    }
}
