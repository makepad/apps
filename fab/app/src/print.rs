//! Slicing in Fab: the Print editor's job from the user's installed slicer
//! profiles (or Fab's own defaults), sliced on a background thread with its
//! stages reported, its errors said, and the moves turned into the viewport's
//! print preview on that same thread.
//!
//! - The profile catalogue (names, kinds, compatibility) is read once at
//!   startup on its own thread.
//! - A Slice resolves the presets, applies the panel's values, places the
//!   prime tower clear of the parts, runs the slicer and builds the preview:
//!   one instance publication of every move (arcs as chords), the layer
//!   index, the legend, the summary and the timeline that places the
//!   nozzles at any time.
//! - Export writes the last slice's printer package to `~/Documents/Fab`. The package is made
//!   only then, or when the slice is sent to the printer: on a pool worker, its progress on the
//!   job board ([`Printing::package`]); a settle makes the G-code and its safety check only.
//!
//! Main-loop cost: a Slice hands the job to the slicing engine
//! (`live_slice`, newest request wins, a superseded slice stops at its next
//! layer); geometry, analysis and instance construction run on its pool
//! jobs.

use fab::print::*;
use fab_model::eval::Evaluated;
use fab_plate::machine::{nozzle_name, Clearance, Machine, Nozzle, Rect};
use fab_plate::Plate;
use fab::print::colours::{self, Mapping, TrayView};
use fab::print::jobs::{self, same_printer, CommittedJob, JobMeta, JobState, SavedState};
use fab_model::eval::Mapped;
use fab_printer_link::model::TrayId;
use makepad_fab_gcode::process::Timeline as TimeModel;
use makepad_fab_gcode::record::{flags, MoveKind, MoveRecord, Role};
use makepad_fab_settings::machine::PLATES;
use makepad_fab_settings::virtual_printer::{self, Kind as VirtualKind, Saved, Source as VirtualSource, VirtualPrinter};
use makepad_fab_settings::config::{parse_num, Config};
use makepad_fab_settings::profiles::{installed_selection, JobSetup, Preset, ProfileStore, Selection};
use makepad_widgets::makepad_platform::retained_instances::{InstanceSegment, RetainedInstances};
use makepad_widgets::*;
use std::sync::atomic::{AtomicU64, Ordering};
use makepad_fab_slice::piece;
use std::sync::Arc;
use std::path::{Path, PathBuf};
use fab_calibrate::apply::SlicerCalibration;
use fab_calibrate::results::EffectiveResults;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Machine,
    Process,
    Filament,
}

#[derive(Clone, Debug)]
struct Entry {
    name: String,
    kind: Kind,
    /// The user's own preset (under the profile folder's `user/`).
    user: bool,
    /// Printer names it is made for (empty: any; user presets: as their parent).
    compat: Vec<String>,
    material: Option<String>,
    /// A filament preset's material id (what a printer tray reports it holds).
    filament_id: Option<String>,
    /// Plates this filament preset gives a positive bed temperature.
    plates: Vec<String>,
}

struct TwinProfile {
    printer: VirtualPrinter,
    machine: Arc<BoundMachine>,
    binding: Arc<MachineBinding>,
}

/// The installed profiles: where they are and what they offer.
pub struct Catalog {
    store: Arc<ProfileStore>,
    entries: Vec<Entry>,
    /// Filament layouts resolved on the catalogue worker for the twin picker and restoration.
    twins: HashMap<String, TwinProfile>,
    /// The installed selection when compatible, else the first preset in picker order, by reported model.
    automatic: HashMap<String, Arc<MachineBinding>>,
    resin: std::collections::HashSet<String>,
    selection: Option<Selection>,
    /// Slicer profiles are installed: the presets aren't only the ones Fab carries itself.
    installed: bool,
    /// Ignore a catalogue still in flight for a layout the person has since changed.
    layout_revision: u64,
}

impl std::fmt::Debug for Catalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Catalog").field("entries", &self.entries.len()).finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrintOutcome { Unchanged, Target, RestoredTarget, JobRows, JobPreview, Calibration { setup_changed: bool, slice_changed: bool } }

impl PrintOutcome {
    fn completion(state: &mut PrintState, printer: &PrinterChoice) -> Self {
        // The completing printer's history changed: recheck its stored report (it projects only when shown).
        fab::print::follow::recheck_report(state, printer, Cx::time_now());
        if printer == &state.links.picked {
            Self::Target
        } else {
            Self::JobRows
        }
    }
}

/// What the slicing threads post.
#[cfg_attr(test, derive(Clone))]
#[derive(Debug)]
pub enum PrintEvent {
    CalibrationResolved { seq: u64, effective: Arc<EffectiveCalibration> },
    CalibrationWritten { outcome: CalWriteOutcome },
    Checked { id: u64, report: Arc<fab_printability::Report>, error: Option<String> },
    /// Configurations and original layouts for the printer and reset menus.
    PrinterConfigs { names: Vec<String>, presets: Vec<VirtualPrinter>, loadouts: LoadoutFiles },
    /// Saved layouts arrive with the catalogue needed to construct their twins.
    VirtualPrinters { saved: Saved, catalog: Option<Arc<Catalog>>, kept_as: Option<String> },
    PrinterModesUnreadable(String),
    ReleaseTestPackages,
    #[cfg(test)]
    JobFilesRemoved(String),
    Catalog(Option<Arc<Catalog>>),
    /// A first or changed model report can identify the target after the person picked it.
    TargetModelChanged,
    Progress { gen: u64, progress: f32, stage: String },
    /// How far the engine's input `revision` is, 0..1 over all its stages, and its stage's words
    /// (see [`makepad_fab_slice::live::LiveEvent::Running`]).
    Running { revision: u64, progress: f32, stage: String },
    /// Slice `gen`'s live layer tiles as they arrive.
    Streamed { gen: u64, preview: Arc<PrintPreview> },
    /// The trees of the plate's supports as the live slice has them now.
    Trees(Arc<[fab::print::PlateTree]>),
    Sliced { gen: u64, result: Result<Arc<Sliced>, String>, seconds: f64 },
    /// The printability of slice `gen` so far (it streams after the slice: `done` of `total`
    /// layers checked).
    Issues { gen: u64, report: Arc<fab_printability::Report>, done: u32, total: u32 },
    /// Profiles imported into Fab's profile folder: the report's summary, the
    /// imported (printer, process, filaments) to select, and the catalogue
    /// read again.
    ProfilesImported { summary: String, select: Option<(String, String, Vec<String>)>, catalog: Option<Arc<Catalog>> },
    /// Live revision `revision` whole with the planner's times (no G-code): its preview plays,
    /// and its filament changes (`changes`) weigh other heads for its colours.
    Estimated { revision: u64, preview: Arc<PrintPreview>, changes: Option<Arc<makepad_fab_gcode::heads::Changes>> },
    /// Live revision `revision`'s playing preview as far as it has grown, ahead of the whole one
    /// (`Estimated`): `taken` tells its builder the view has it, so the next may come.
    Growing { revision: u64, preview: Arc<PrintPreview>, taken: Arc<std::sync::atomic::AtomicBool> },
    /// Live revision `revision`'s kept print time, ahead of its playing preview (a step back or
    /// forth: the planner runs again for the preview's times).
    Totals { revision: u64, totals: fab::print::EstimateTotals },
    /// The heads live revision `revision`'s tools (`tools`, the mapping they were weighed on)
    /// print on fastest ([`take_heads`]).
    HeadsChosen { revision: u64, tools: Vec<colours::Position>, choice: makepad_fab_gcode::heads::Choice },
    /// Printer package `id` ([`Printing::package`]) is made, or why not.
    Packaged { id: u64, result: Packaged },
    /// The saved jobs, read when Fab starts.
    JobsRead(Vec<(JobMeta, SavedState)>),
    /// A job's preview, made from its package.
    JobPreview { id: String, result: Result<Arc<PrintPreview>, String> },
    /// A queued job's package, read back and checked against the installed profile for its start.
    JobChecked { id: String, attempt: u64, ticket: PrintTicket, options: fab_printer_link::PrintOptions, result: Result<fab_printer_link::safety::ValidatedJob, String> },
}

/// How a printer package made for a send or an export ended.
#[derive(Clone, Debug)]
pub enum Packaged {
    /// For the printer: the package as the print safety check passed it.
    Checked(fab_printer_link::safety::ValidatedJob),
    /// Exported to this file.
    Written(std::path::PathBuf),
    /// The person cancelled it, or the slice it was for is no longer the shown one.
    Stopped,
    Failed(String),
}

/// A printer package's job: what Print commits.
#[derive(Clone, Debug, PartialEq)]
struct ToPrinter {
    /// The link the person confirmed before the package worker started.
    ticket: PrintTicket,
    /// The options confirmed when Start print was pressed.
    options: fab_printer_link::PrintOptions,
    meta: Arc<JobMeta>,
    /// The plate's scripts as they were.
    scripts: Arc<std::collections::BTreeMap<String, String>>,
    /// The printer was busy: it waits in the queue for a person's press.
    queue: bool,
}

/// Where a printer package goes once made.
#[derive(Clone, Debug, PartialEq)]
enum PackageFor {
    Printer(ToPrinter),
    File(std::path::PathBuf),
}

/// A printer package being made on a pool worker.
struct Packaging {
    id: u64,
    sliced: Arc<Sliced>,
    to: PackageFor,
    /// Raised when the person cancels it.
    stop: Arc<std::sync::atomic::AtomicBool>,
}

/// The refusal while the shown slice is not the current, settled one.
const SLICE_NOT_READY: &str = "Slice the plate first: the shown slice is out of date.";

/// The printer's report in the log as it changes: connection, job, command answer, stage and
/// error (never the access code or the serial: none of these carry them).
fn log_printer_change(old: Option<&fab_printer_link::Snapshot>, new: &fab_printer_link::Snapshot) {
    if old.is_none_or(|o| o.status != new.status) {
        log!("fab printer: {}", new.status.label());
    }
    let stage = |s: &fab_printer_link::Snapshot| s.job.as_ref().map(|j| j.stage.label());
    if let Some(stage) = stage(new).filter(|l| old.is_none_or(|o| stage(o).as_ref() != Some(l))) {
        log!("fab printer: job: {stage}");
    }
    if let Some((command, answer)) = new.command.as_ref().filter(|c| old.is_none_or(|o| o.command.as_ref() != Some(*c))) {
        log!("fab printer: {command}: {answer:?}");
    }
    let state = |s: &fab_printer_link::Snapshot| (s.state.gcode_state.clone(), s.state.stage_name(), s.state.print_error);
    let now = state(new);
    if old.is_none_or(|o| state(o) != now) {
        log!("fab printer: state {} stage {} error {:08X}", now.0.as_deref().unwrap_or("-"), now.1.as_deref().unwrap_or("-"), now.2.unwrap_or(0));
    }
    let errors = |s: &fab_printer_link::Snapshot| (s.state.hms.clone(), s.state.errors.clone());
    let wrong = errors(new);
    if old.is_none_or(|o| errors(o) != wrong) {
        let codes: Vec<String> = wrong.0.iter().map(|h| h.code()).collect();
        log!("fab printer: errors: hms [{}] others {:?}", codes.join(", "), wrong.1);
    }
    // What Monitor's stats show, every tenth of a print, to hold against the printer's screen.
    let tenth = |s: &fab_printer_link::Snapshot| s.state.progress.map(|p| (p / 10.0) as u32);
    if old.is_none_or(|o| o.state.details.is_empty() || tenth(o) != tenth(new)) && !new.state.details.is_empty() {
        log!("fab printer: stats: {}", fab::print::job_stats::printer_lines(new, None).replace('\n', " | "));
    }
    // The fields Fab has no name for: their names only, when they first come.
    let fields = |s: &fab_printer_link::Snapshot| s.state.details.len();
    if fields(new) >= old.map_or(0, fields) + 20 {
        let names: std::collections::BTreeSet<String> = new.state.details.iter().map(|(path, _)| path.split('.').map(|p| if p.bytes().all(|b| b.is_ascii_digit()) { "*" } else { p }).collect::<Vec<_>>().join(".")).collect();
        log!("fab printer: other fields: {}", names.into_iter().collect::<Vec<_>>().join(" "));
    }
}

/// A slice's G-code that passed the print safety check, and the installed profile it passed:
/// its package is checked against the same profile when it is sent (only that check makes a
/// job the printer takes).
#[derive(Clone, Debug)]
pub struct Checked {
    pub facts: fab_printer_link::safety::JobFacts,
    profile: fab_printer_link::safety::MachineProfile,
}

pub struct Sliced {
    /// The settled slice: its G-code and records, what its printer package is made from when
    /// it is sent or exported.
    pub settled: Arc<makepad_fab_slice::live::Settled>,
    pub name: String,
    /// The resolved machine preset's model when this slice was made.
    pub printer_model: String,
    pub preview: Arc<PrintPreview>,
    /// The toolhead it was sliced for, and whether it has two nozzles.
    head: Toolhead,
    dual: bool,
    /// The tray each job tool feeds from.
    pub trays: Vec<Option<TrayId>>,
    /// Colours supplied to the print settings, in settled tool order.
    print_colours: Vec<[f32; 4]>,
    /// The filaments the plate prints with (0-based).
    pub used: Vec<usize>,
    /// The print safety check of the G-code against the installed machine profile.
    pub checked: Result<Checked, String>,
}

impl std::fmt::Debug for Sliced {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sliced").field("gcode_bytes", &self.settled.gcode.len()).finish()
    }
}

const DEFAULT_MACHINE: &str = "Fab dual nozzle 0.4";
const DEFAULT_PROCESS: &str = "Fab 0.20 mm standard";
const DEFAULT_FILAMENT: &str = "Fab PLA";
/// Fab's own flexible filament, for a colour that asks for TPU with no installed profiles: generic
/// soft-TPU values (slow, hot, short retraction), not any vendor's profile. An installed TPU
/// profile replaces it wherever one exists.
const DEFAULT_FLEX_FILAMENT: &str = "Fab TPU";

/// Fab's own filament for a material type (no installed profiles).
fn default_filament(material: Option<&str>) -> &'static str {
    if material.is_some_and(fab_plate::flexible) { DEFAULT_FLEX_FILAMENT } else { DEFAULT_FILAMENT }
}

use makepad_widgets::jobs::{JobHandle, JobId, JobSpec};
use std::collections::HashMap;

const STALE_TICKET: &str = "The printer confirmation changed. Reopen Print before starting.";
const STALE_PREVIEW: &str = "The slice changed since this Start button was shown. Review the new slice before starting.";
const STALE_DISPATCH: &str = "The printer changed since you confirmed: nothing was sent.";
const WRONG_DESTINATION: &str = "Link the printer and mode this job was prepared for before starting it.";
const SIMULATED_PRINTER: &str = "Simulated printer";

struct Linked {
    id: PrinterChoice,
    link: fab_printer_link::PrinterLink,
    virtual_mode: bool,
    epoch: u64,
    watched: Option<(String, usize)>,
}

impl Linked {
    fn refresh_binding(&self, state: &mut PrintState, catalog: Option<&Catalog>, saved: Option<&Saved>) {
        let binding = match catalog {
            Some(cat) => {
                let fixed = self.virtual_mode.then(|| Printing::virtual_record(saved?, &self.id)).flatten();
                let explicit = saved.filter(|_| !self.virtual_mode).and_then(|s| s.bindings.get(&self.id.id()?));
                let model = state.printer_of(&self.id).map_or("", |s| s.model.as_str());
                cat.binding(model, fixed, explicit)
            }
            None => Arc::new(MachineBinding::NoCatalogue),
        };
        state.printers.set_binding(&self.id, binding);
    }

    fn bind_heads(&mut self, state: &PrintState, model: &str) {
        let heads = state.printers.binding(&self.id).and_then(|b| b.machine()).map_or(0, |p| p.geometry.nozzles.len());
        self.watch_heads(model, heads);
    }

    fn keep_background_loadout(&self, state: &PrintState, report: &fab_printer_link::Snapshot, files: &mut LoadoutFiles, worker: Option<&ProfilesWorker>) {
        if report.virtual_printer { return; }
        let Some(profile) = state.printers.binding(&self.id).and_then(|b| b.machine()) else { return };
        // The optional local preset name is only for the picker. Retained trays use the reported material.
        let mut layout = colours::layout(Some((&report.name, &report.state)), None, &profile.layout, &|_| None);
        if layout.live && !colours::drop_missing_heads(&mut layout) {
            remember_loadout(files, worker, &self.id, &loadout_text(&layout, &report.model));
        }
    }

    fn watch_heads(&mut self, model: &str, heads: usize) {
        if self.watched.as_ref().is_some_and(|(previous, count)| previous == model && *count == heads) { return; }
        self.link.set_heads(model, heads);
        self.watched = Some((model.to_owned(), heads));
    }
}

#[derive(Default)]
pub struct Polled {
    pub target: bool,
    pub shown: bool,
    pub others: bool,
}

const LOADING_CALIBRATION: &str = "Loading calibration";
const UPDATING_CALIBRATION_PLATE: &str = "Updating the plate for calibration";

#[derive(Clone, Debug, PartialEq)]
pub enum RebuildFailureKind {
    Joint { parts: Vec<String>, kind: Option<&'static str>, id: String, reason: String },
    Solid { parts: Vec<String>, id: String, op: String, reason: String },
    Program { file: Option<String>, line: u32, col: u32, reason: String },
}

#[derive(Clone, Debug, PartialEq)]
struct ProfileRebuildFailure {
    measured: makepad_csg_mech::Measured,
    layer: f64,
    source: fab_model::eval::Failure,
    kind: RebuildFailureKind,
    message: String,
    redacted_message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PlateGate {
    Wait(&'static str),
    MachineInvalid(MachineError),
    CalibrationFailed(String),
}

struct PendingSend {
    ticket: PrintTicket,
    preview: u64,
}

pub struct Printing {
    pub dirs: Arc<crate::dirs::FabDirs>,
    profile_rebuild_error: Option<ProfileRebuildFailure>,
    pending_send: Option<PendingSend>,
    print_setup_seq: u64,
    setup_deferred: bool,
    /// With the test prints chosen, the ticked ones: what slices in place of the whole plate.
    tests: Option<Arc<[piece::TestPrint]>>,
    /// The slice the person switched away from, whole plate or test prints: it shows again without
    /// slicing when they switch back. It goes when the plate changes, or at the next switch.
    kept: Option<KeptSlice>,
    fleet: Vec<Linked>,
    /// Twin workers to restore if their required preset returns.
    pending_twins: Vec<PrinterChoice>,
    #[cfg(test)]
    real_constructor: Option<Box<dyn Fn(&str) -> fab_printer_link::PrinterLink>>,
    installed_epoch: u64,
    polled_epoch: u64,
    published: usize,
    simulator: Option<std::path::PathBuf>,
    /// The profiles worker serializes saves, imports and catalogue rebuilds.
    profiles_worker: Option<ProfilesWorker>,
    job_writer: Option<JobWriter>,
    /// The saved choices and the original layouts for resetting a printer.
    virtuals: Option<Arc<Saved>>,
    factory: Vec<VirtualPrinter>,
    layout_revision: u64,
    catalog: Option<Arc<Catalog>>,
    calibration_key: CalibrationKey,
    // Compare a report against the last inputs without rebuilding the unique-filament list
    // or following the same twin provenance again.
    calibration_target: Option<(PrinterChoice, Option<Arc<Saved>>)>,
    calibration_slots: Vec<String>,
    calibration_seq: u64,
    calibration_resolved: u64,
    calibration: Arc<EffectiveCalibration>,
    machine: Result<Arc<Machine>, MachineError>,
    /// The colour mapping used to resolve the Print editor's current materials.
    pub(super) mapping: Option<Arc<Mapped>>,
    gen: u64,
    /// Only a requested settle can answer; an edit withdraws it without a new generation.
    slice_gen: Option<u64>,
    /// The settled slice generation whose preview owns incoming `PrintEvent::Issues`.
    issues_gen: Option<u64>,
    /// The live input kept verbatim until Print, Export or Slice asks for a settle.
    live: Option<LiveJob>,
    live_key: Option<(u64, u64)>,
    /// The whole plate's identity its figures in [`PrintState::whole_figures`] are of.
    figures_of: Option<(u64, u64)>,
    last: Option<Arc<Sliced>>,
    /// Export the requested slice when it arrives.
    export_waits: bool,
    /// The active file packaging attempt whose result still applies to the plate.
    export_attempt: Option<u64>,
    /// A slice that Slice, Export or Print was waiting for went with its plate or settings (and
    /// whether an Export waited on it): [`Self::resume_asked`] asks for it again on what stands now.
    reask: Option<bool>,
    started: Option<std::time::Instant>,
    /// The project's global settings as last evaluated.
    pub project_settings: Vec<(String, String)>,
    /// The settled slice (Print, Export, Slice) in flight on the jobs board, beside the Job
    /// panel's status row; a newer revision keeps the same job.
    slice_job: Option<JobHandle>,
    /// The live slice on the jobs board while no chip over the view shows it (the Monitor
    /// workspace with the Slice view closed), so a slice never runs silently. In the Slice view
    /// the chip shows it and there is no job; a slice quicker than the strip's delay never shows.
    live_job: Option<JobHandle>,
    /// How far the live slice is as the engine tells it: its input revision, the share of all its
    /// stages done (its supports a stage of it) and the stage's words.
    running: Option<(u64, f32, String)>,
    /// What the job strip's Retry runs again, by job.
    retries: HashMap<JobId, Retry>,
    /// The trays as the printer last reported them, or as set by hand since: the mapping's
    /// trays while it is not reporting. Read from `loadout_text` with the picked printer's
    /// toolheads.
    loadout: Option<colours::Layout>,
    /// Contents by loadout file, including edits the worker has yet to save.
    loadouts: LoadoutFiles,
    /// The target's retained text. It is read again when a printer profile with
    /// other toolheads is picked, so a loadout kept by a printer with other toolheads is not offered.
    loadout_text: Option<String>,
    /// The bed plate was picked (by hand, or the installed selection names it): no default
    /// replaces it.
    plate_picked: bool,
    /// The plate the selected printer's model records as its default ("" when none).
    printer_plate: String,
    /// The selected printer profile's model ("" for Fab's defaults).
    machine_model: String,
    /// Packages in flight and the last attempt issued for packaging or a queued recheck.
    packaging: Vec<Packaging>,
    last_attempt: u64,
    /// Completions held by the simulator UI test until its mode-switch press has been handled.
    test_packages: Option<Vec<(u64, Packaged)>>,
    /// The plate's scripts by file, as the app has them: what Print commits with a job.
    pub plate_scripts: Arc<std::collections::BTreeMap<String, String>>,
    /// The queued job whose start is being checked.
    starting: Vec<(String, u64)>,
    /// Per job tool the heads it may print on, and per head the tools it has room for
    /// ([`colours::head_choices`]), from the last mapping.
    head_choices: (Vec<Vec<usize>>, Vec<usize>),
    /// The plate the reach below is of (held weakly: its address is never another plate's) and
    /// the nozzles that reach each colour's parts ([`colours::reaching`]), so a printer report
    /// maps again without measuring the parts again.
    reach: Option<(std::sync::Weak<Evaluated>, Arc<Vec<(String, Vec<usize>)>>)>,
}

/// A slice as it stood when the person switched away from it.
struct KeptSlice {
    /// The plate it comes from: its slice goes with it.
    plate: u64,
    /// Its input's identity, settings with it: only an input of this very identity shows it again.
    key: (u64, u64),
    shown: fab::print::ShownSlice,
    toolpaths: crate::live_slice::Sliced,
    last: Option<Arc<Sliced>>,
}

/// Hand a control to the selected worker only while its rendered destination still matches.
pub(super) fn send_filament_control(
    state: &PrintState, dest: &ControlDestination, control: PrinterControl, send: impl FnOnce(fab_printer_link::Command),
) {
    use fab_printer_link::Command;
    if !matches!(dest, ControlDestination::Filament { .. }) || !dest.matches(state) {
        log!("fab: ignored a printer control whose destination changed");
        return;
    }
    send(match control {
        PrinterControl::Pause => Command::Pause,
        PrinterControl::Resume => Command::Resume,
        PrinterControl::Stop => Command::Stop,
        PrinterControl::ClearError => Command::ClearError,
        PrinterControl::CheckStorage => Command::CheckStorage,
    });
}

impl Default for Printing {
    fn default() -> Self {
        Self {
            dirs: Arc::default(),
            fleet: Vec::new(),
            pending_twins: Vec::new(),
            #[cfg(test)]
            real_constructor: None,
            installed_epoch: 0,
            polled_epoch: 0,
            published: 0,
            simulator: None,
            profiles_worker: None,
            profile_rebuild_error: None,
            pending_send: None,
            print_setup_seq: 0,
            setup_deferred: false,
            job_writer: None,
            virtuals: None,
            factory: Vec::new(),
            layout_revision: 0,
            catalog: None,
            calibration_key: CalibrationKey::default(),
            calibration_target: None,
            calibration_slots: Vec::new(),
            calibration_seq: 0,
            calibration_resolved: 0,
            calibration: Arc::default(),
            machine: CheckSetup::fab_defaults().machine_typed().map(Arc::new),
            mapping: None,
            gen: 0,
            slice_gen: None,
            issues_gen: None,
            live: None,
            tests: None,
            kept: None,
            live_key: None,
            figures_of: None,
            last: None,
            export_waits: false,
            export_attempt: None,
            reask: None,
            started: None,
            project_settings: Vec::new(),
            slice_job: None,
            live_job: None,
            running: None,
            retries: HashMap::new(),
            loadouts: LoadoutFiles::new(),
            loadout: None,
            loadout_text: None,
            plate_picked: false,
            printer_plate: String::new(),
            machine_model: String::new(),
            packaging: Vec::new(),
            last_attempt: 0,
            test_packages: None,
            plate_scripts: Arc::default(),
            starting: Vec::new(),
            head_choices: (Vec::new(), Vec::new()),
            reach: None,
        }
    }
}

/// The least time a choice of heads must save before the colours move (s): a smaller gain is not
/// worth slicing the plate again for.
pub const HEADS_SAVE: f64 = 60.0;

/// The heads a plate's tools may print on, weighed over its filament changes (off the UI): the
/// tools' choices padded to the job's filaments (one past the mapping stays where it is).
pub(crate) fn choose_heads(changes: &makepad_fab_gcode::heads::Changes, choices: &[Vec<usize>], room: &[usize]) -> makepad_fab_gcode::heads::Choice {
    let heads = changes.model.nozzles();
    let filaments = changes.sets.iter().flatten().copied().max().map_or(0, |f| f + 1).max(choices.len());
    let mut all: Vec<Vec<usize>> = (0..filaments).map(|f| choices.get(f).cloned().unwrap_or_default()).collect();
    for c in &mut all {
        c.retain(|&h| h < heads);
    }
    makepad_fab_gcode::heads::cheapest_heads(&changes.sets, &changes.model, &all, room)
}

/// Put the automatically picked colours of `tools` (the mapping the choice was weighed on) on
/// the heads `choice` gives, when that saves at least [`HEADS_SAVE`]: those it moves, and those
/// it keeps (held there, or the moves would push them elsewhere). True when a colour moved.
pub(crate) fn take_heads(state: &mut PrintState, tools: &[colours::Position], choice: &makepad_fab_gcode::heads::Choice) -> bool {
    let (seconds, grams) = choice.saves();
    if state.colors.tools != tools || seconds < HEADS_SAVE || tools.iter().zip(&choice.heads).all(|(t, &h)| t.head == h) {
        return false;
    }
    for (tool, &head) in tools.iter().zip(&choice.heads) {
        for r in state.colors.rows.iter().filter(|r| r.used && r.how == colours::How::Auto && r.pos == Some(*tool)) {
            // a colour moved before and kept now stays moved
            let stays = tool.head == head && state.head_moves.iter().find(|m| m.name == r.name).is_none_or(|m| m.stays);
            state.head_moves.retain(|m| m.name != r.name);
            state.head_moves.push(colours::Moved { name: r.name.clone(), head, stays, seconds, grams });
        }
    }
    true
}

impl Catalog {
    /// Select among the machines already resolved by the catalogue worker.
    fn binding(&self, model: &str, fixed: Option<&VirtualPrinter>, explicit: Option<&String>) -> Arc<MachineBinding> {
        if let Some(printer) = fixed {
            let name = printer.machine_preset_name();
            if let Some(profile) = self.twins.get(&name) { return profile.binding.clone(); }
            if self.resin.contains(&name) { return Arc::new(MachineBinding::Resin); }
            return Arc::new(MachineBinding::MissingPreset(name));
        }
        let model = model.trim().to_ascii_lowercase();
        let Some(automatic) = self.automatic.get(&model) else {
            if !model.is_empty() {
                if let Some(pick) = explicit.filter(|n| !self.twins.contains_key(*n) && !self.resin.contains(*n)) {
                    return Arc::new(MachineBinding::MissingPreset(pick.clone()));
                }
                if self.resin.contains(&model) { return Arc::new(MachineBinding::Resin); }
            }
            return Arc::new(MachineBinding::UnknownModel);
        };
        let (Some(pick), MachineBinding::Available { machine: first, choices, .. }) = (explicit, automatic.as_ref()) else { return automatic.clone() };
        let machine = self.twins.get(pick).filter(|p| profile_heads(&model, &p.machine.model, &p.machine.geometry).is_some()).map_or(first, |p| &p.machine);
        Arc::new(MachineBinding::Available { machine: machine.clone(), automatic: first.clone(), choices: choices.clone(), explicit: Some(pick.clone()) })
    }
}

#[derive(Clone, Debug)]
struct CalResolveRequest {
    seq: u64,
    key: CalibrationKey,
}

#[derive(Clone, Debug)]
pub enum CalWrite {
    Save {
        owner: Option<CalibrationOwner>,
        preset: String,
        filament: String,
        coupon: Arc<fab_calibrate::Coupon>,
        answer: fab_calibrate::Answer,
        today: String,
    },
    Clear {
        owner: Option<CalibrationOwner>,
        preset: String,
        filament: String,
        step: fab_calibrate::Step,
    },
}

#[cfg_attr(test, derive(Clone))]
#[derive(Debug)]
pub enum CalWriteOutcome {
    Saved {
        owner: Option<CalibrationOwner>,
        preset: String,
        filament: String,
        step: fab_calibrate::Step,
        values: Vec<(&'static str, f64)>,
    },
    Cleared {
        owner: Option<CalibrationOwner>,
        preset: String,
        step: fab_calibrate::Step,
        has_preset_fallback: bool,
    },
    Failed(String),
}

/// Apply all writes in arrival order before publishing the most recent requested snapshot.
fn run_calibration_work(dir: &Path, writes: Vec<CalWrite>, resolve: Option<CalResolveRequest>, mut post: impl FnMut(PrintEvent)) {
    use fab_calibrate::Results;
    for write in writes {
        let (owner, preset, filament) = write.destination();
        let mut r = match owner {
            Some(owner) => Results::load_owner_in(dir, &owner.id, filament),
            None => Results::load_in(dir, preset, filament),
        };
        let save = |r: &Results| if owner.is_some() { r.save_owner_in(dir) } else { r.save_in(dir) };
        let result = match &write {
            CalWrite::Save { coupon, answer, today, .. } => r.record(coupon, answer, today).and_then(|values| {
                save(&r)?;
                Ok(CalWriteOutcome::Saved { owner: owner.clone(), preset: preset.into(), filament: filament.into(), step: coupon.step, values })
            }),
            CalWrite::Clear { step, .. } => {
                r.clear_step(*step);
                save(&r).map(|_| {
                    let has_preset_fallback = owner.is_some() && {
                        let fallback = Results::load_in(dir, preset, filament);
                        fab_calibrate::results::step_names(*step).iter().any(|name| fallback.get(name).is_some())
                    };
                    CalWriteOutcome::Cleared { owner: owner.clone(), preset: preset.into(), step: *step, has_preset_fallback }
                })
            }
        };
        post(PrintEvent::CalibrationWritten { outcome: result.unwrap_or_else(CalWriteOutcome::Failed) });
    }
    if let Some(request) = resolve {
        let effective = Arc::new(EffectiveCalibration::resolve_in(dir, &request.key));
        post(PrintEvent::CalibrationResolved { seq: request.seq, effective });
    }
}

impl CalWrite {
    fn destination(&self) -> (&Option<CalibrationOwner>, &str, &str) {
        match self {
            Self::Save { owner, preset, filament, .. } | Self::Clear { owner, preset, filament, .. } => (owner, preset, filament),
        }
    }
}

fn write_affects_key(write: &CalWrite, key: &CalibrationKey) -> bool {
    let (owner, preset, filament) = write.destination();
    key.filaments.iter().any(|f| f == filament) && match owner {
        Some(owner) => key.owner.as_ref() == Some(owner),
        None => key.preset == preset,
    }
}

/// Settings snapshots replace earlier pending writes. Imports each need an answer.
#[derive(Default)]
struct ProfileWork {
    cal_writes: Vec<CalWrite>,
    cal_resolve: Option<CalResolveRequest>,
    saved: Option<Arc<Saved>>,
    rebuild: Option<u64>,
    loadouts: LoadoutFiles,
    imports: Vec<(std::path::PathBuf, bool)>,
}

/// A single wake-up covers every pending change. The worker takes the work before doing I/O.
struct ProfilesWorker {
    pending: Arc<std::sync::Mutex<ProfileWork>>,
    wake: std::sync::mpsc::SyncSender<()>,
}

impl ProfilesWorker {
    /// Publish a change without waiting for disk or a busy catalogue reader.
    fn update<T>(&self, edit: impl FnOnce(&mut ProfileWork) -> T) -> T {
        let replaced = edit(&mut self.pending.lock().unwrap_or_else(|e| e.into_inner()));
        match self.wake.try_send(()) {
            Ok(()) | Err(std::sync::mpsc::TrySendError::Full(())) => {}
            Err(std::sync::mpsc::TrySendError::Disconnected(())) => log!("fab: the profiles worker stopped"),
        }
        replaced
    }
}

/// One writer owns the state-file renames. Each job keeps its newest pending state.
struct JobWriter {
    pending: Arc<std::sync::Mutex<HashMap<String, SavedState>>>,
    wake: std::sync::mpsc::SyncSender<()>,
}

impl JobWriter {
    fn channel() -> (Self, std::sync::mpsc::Receiver<()>) {
        let (wake, wakes) = std::sync::mpsc::sync_channel(1);
        (Self { pending: Default::default(), wake }, wakes)
    }

    fn save(&self, saves: Vec<(String, SavedState)>) {
        if saves.is_empty() { return; }
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).extend(saves);
        if let Err(std::sync::mpsc::TrySendError::Disconnected(())) = self.wake.try_send(()) {
            log!("fab jobs: the job writer stopped");
        }
    }

    fn spawn(&self, dir: PathBuf, wakes: std::sync::mpsc::Receiver<()>, on_error: fn(&str, std::io::Error))
        -> std::io::Result<std::thread::JoinHandle<()>>
    {
        let pending = self.pending.clone();
        std::thread::Builder::new().name("fab-jobs".into()).spawn(move || {
            while wakes.recv().is_ok() {
                let batch = std::mem::take(&mut *pending.lock().unwrap_or_else(|e| e.into_inner()));
                for (id, saved) in batch {
                    if let Err(e) = jobs::save_state(&dir, &id, &saved) { on_error(&id, e); }
                }
            }
        })
    }
}

/// A test may supply its own home. A lane or a unit test without one keeps edits in memory.
fn virtual_path() -> Option<std::path::PathBuf> {
    let test_home = std::env::var_os("MAKEPAD_TEST_HOME").filter(|p| !p.is_empty());
    if test_home.is_none() && std::env::var_os("MAKEPAD_TEST_LANE").is_some() { return None; }
    if cfg!(test) {
        return test_home
            .map(|home| std::path::PathBuf::from(home).join("virtual_printers.test.txt"));
    }
    Some(virtual_printer::path())
}

/// Without a readable profile for a configured printer, give it a twin of the first
/// built-in layout. Its configuration is never opened to guess a model.
fn fresh_printer_modes(names: &[String]) -> Saved {
    let mut saved = Saved {
        picked: Some("printer:No printer".into()),
        disconnected: names.iter().cloned().collect(),
        ..Saved::default()
    };
    let template = saved.printers[0].clone();
    let preset = template.machine_preset_name();
    let model = template.model();
    for (index, config) in names.iter().enumerate() {
        saved.printers.push(VirtualPrinter {
            name: format!("twin-{}", index + 1), configured_for: Some(config.clone()),
            source: VirtualSource::Twin { preset: preset.clone(), model: model.clone(), source: PrinterChoice::Real(config.clone()).id() },
            ..template.clone()
        });
    }
    saved
}

fn load_printer_modes(path: Option<&std::path::Path>, names: &[String]) -> Result<(Saved, Option<String>), String> {
    let Some(path) = path else { return Ok((Saved::default(), None)); };
    if let Ok(saved) = virtual_printer::load_from(path) { return Ok((saved, None)); }

    let not_kept = |e: std::io::Error| {
        let kind = e.kind();
        log!("fab: could not keep unreadable printer settings aside ({kind})");
        format!("Fab could not read the saved printer modes. No printer was linked. The file could not be kept aside ({kind}).")
    };
    let fresh = fresh_printer_modes(names);
    let pending = virtual_printer::recovery_path(path);
    virtual_printer::save_to(&pending, &fresh).map_err(not_kept)?;
    let kept = virtual_printer::set_aside(path).map_err(|e| {
        // Another recovery may have just removed the original and need the staged file.
        if e.kind() != std::io::ErrorKind::NotFound { let _ = std::fs::remove_file(&pending); }
        not_kept(e)
    })?;
    let name = kept.file_name().unwrap().to_string_lossy().into_owned();
    if let Err(e) = std::fs::rename(&pending, path) {
        // A concurrent reader can already have installed the staged replacement.
        if e.kind() != std::io::ErrorKind::NotFound || !path.is_file() {
            let kind = e.kind();
            log!("fab: kept unreadable printer settings as {name}, could not install fresh settings ({kind})");
            return Err(format!("Fab kept its unreadable printer settings as {name}. Fresh settings could not be installed ({kind}). No printer was linked."));
        }
    }
    log!("fab: kept unreadable printer settings as {name}");
    Ok((fresh, Some(name)))
}

/// Read and publish modes on the profiles worker. Twins wait for the catalogue.
fn read_printer_profiles(
    path: Option<&std::path::Path>, names: &[String],
    load_catalog: impl FnOnce(&[VirtualPrinter]) -> Option<Arc<Catalog>>, mut post: impl FnMut(PrintEvent),
) -> Arc<Saved> {
    let (mut saved, mut kept_as, modes_known) = match load_printer_modes(path, names) {
        Ok((saved, kept_as)) => (saved, kept_as, true),
        Err(note) => {
            post(PrintEvent::PrinterModesUnreadable(note));
            (Saved::default(), None, false)
        }
    };
    let has_twins = saved.printers.iter().any(|p| p.source != VirtualSource::Generated);
    if modes_known && !has_twins {
        post(PrintEvent::VirtualPrinters { saved: saved.clone(), catalog: None, kept_as: kept_as.take() });
    }
    let catalog = load_catalog(&saved.printers);
    if has_twins && restore_twins(&mut saved, catalog.as_deref()) {
        log!("fab: skipped a virtual twin without a supported filament machine preset");
    }
    if modes_known && has_twins {
        post(PrintEvent::VirtualPrinters { saved: saved.clone(), catalog: catalog.clone(), kept_as });
    }
    post(PrintEvent::Catalog(catalog));
    Arc::new(saved)
}

/// Count ordinary trays on a nozzle separately from external spools.
fn virtual_controls(vp: &VirtualPrinter) -> VirtualControls {
    VirtualControls {
        kind: vp.kind, heads: vp.heads,
        trays: vp.slots.iter().filter(|s| s.head == 0 && s.path == virtual_printer::SlotPath::Shared).count(),
        tpu: vp.slots.iter().any(|s| s.path == virtual_printer::SlotPath::Dedicated),
    }
}

/// Machine head names in the order used by the profile and the link.
fn virtual_heads(vp: &VirtualPrinter) -> Vec<String> {
    (0..vp.heads).map(|i| nozzle_name(vp.heads, i)).collect()
}

/// Translate the validated slot labels into the tray ids a real link would report.
fn setup_of(vp: &VirtualPrinter, catalog: Option<&Catalog>) -> Option<fab_printer_link::VirtualSetup> {
    let nozzle_diameters = match &vp.source {
        VirtualSource::Generated => vec![0.4; vp.heads],
        VirtualSource::Twin { preset, .. } => catalog?.twins.get(preset)?.machine.diameters.clone(),
    };
    let heads = virtual_heads(vp);
    let trays = vp.slots.iter().map(|slot| {
        let pos = colours::Position::parse(&heads, &colours::head_word(&heads, slot.head), &slot.tray)
            .expect("saved layouts have valid head and tray labels");
        fab_printer_link::VirtualTray { head: pos.head, id: pos.tray, material: slot.material.clone(), colour: slot.colour }
    }).collect();
    Some(fab_printer_link::VirtualSetup { name: vp.configured_for.as_ref().unwrap_or(&vp.title).clone(), model: vp.model(), numbered_heads: vp.kind == VirtualKind::Toolchanger, nozzle_diameters, trays })
}

/// Restore the geometry of available twins. Keep missing profiles' records and stock for
/// their return, and disconnect a missing selection. True if any twin is unavailable.
fn restore_twins(saved: &mut Saved, catalog: Option<&Catalog>) -> bool {
    let mut missing = false;
    for p in &mut saved.printers {
        let VirtualSource::Twin { preset, source, .. } = &p.source else { continue };
        let Some(template) = catalog.and_then(|c| c.twins.get(preset)) else {
            missing = true;
            if saved.picked.as_deref().and_then(|id| id.strip_prefix("virtual:")) == Some(p.name.as_str()) {
                saved.picked = Some("printer:No printer".into());
            }
            continue;
        };
        let mut twin = template.printer.clone();
        twin.name.clone_from(&p.name);
        twin.title.clone_from(&p.title);
        if let VirtualSource::Twin { source: origin, .. } = &mut twin.source { origin.clone_from(source); }
        twin.configured_for.clone_from(&p.configured_for);
        seed_twin(&mut twin, &p.slots);
        *p = twin;
    }
    missing
}

fn available_virtual(p: &VirtualPrinter, catalog: Option<&Catalog>) -> bool {
    match &p.source {
        VirtualSource::Generated => true,
        VirtualSource::Twin { preset, .. } => catalog.is_some_and(|c| c.twins.contains_key(preset)),
    }
}

fn twin_name(saved: &Saved) -> String {
    let taken: std::collections::HashSet<_> = saved.printers.iter().map(|p| p.name.as_str()).collect();
    (1usize..).map(|n| format!("twin-{n}")).find(|name| !taken.contains(name.as_str())).unwrap()
}

fn seed_twin(twin: &mut VirtualPrinter, stock: &[virtual_printer::Slot]) {
    let stock: HashMap<_, _> = stock.iter().map(|s| ((s.head, s.tray.as_str()), s)).collect();
    for slot in &mut twin.slots {
        if let Some(s) = stock.get(&(slot.head, slot.tray.as_str())) {
            slot.material.clone_from(&s.material);
            slot.colour = s.colour;
        }
    }
}

fn seed_retained(twin: &mut VirtualPrinter, text: &str) {
    let (model, text) = loadout_model(text);
    let Some(layout) = colours::parse_loadout(text, &virtual_heads(twin)) else { return };
    let to = twin.model();
    if !makepad_fab_settings::family::of_model(&to).is_some_and(|f| f.same_model(model.unwrap_or(&layout.printer), &to)) { return; }
    seed_layout(twin, &layout);
}

fn seed_layout(twin: &mut VirtualPrinter, layout: &colours::Layout) {
    let stock: HashMap<_, _> = layout.trays.iter().filter(|t| t.state != colours::TrayState::Unknown)
        .map(|t| ((t.pos.head, t.pos.tray_text()), t)).collect();
    for slot in &mut twin.slots {
        if let Some(tray) = stock.get(&(slot.head, slot.tray.clone())) {
            slot.material.clone_from(&tray.material);
            slot.colour = tray.colour.map(srgb);
        }
    }
}

fn loadout_text(layout: &colours::Layout, model: &str) -> String {
    let text = colours::loadout_text(layout);
    if model.is_empty() || model.contains(['\r', '\n', '\t']) { text } else { format!("model\t{model}\n{text}") }
}

/// Older files only identify the loadout by its display name.
fn loadout_model(text: &str) -> (Option<&str>, &str) {
    text.strip_prefix("model\t").and_then(|s| s.split_once('\n')).map_or((None, text), |(m, rest)| (Some(m), rest))
}

/// Keys are relative to Fab's data directory. Only the no-printer choice uses the legacy file.
type LoadoutFiles = HashMap<PathBuf, String>;

fn loadout_file(id: &PrinterChoice) -> PathBuf {
    use std::fmt::Write;
    let (prefix, name) = match id {
        PrinterChoice::Real(name) => ("printer-", name),
        PrinterChoice::Virtual(name) => ("virtual-", name),
        PrinterChoice::None => return PathBuf::from("trays.txt"),
    };
    let mut file = String::from(prefix);
    for b in name.bytes() {
        if b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' { file.push(b as char); }
        else { write!(file, "_{b:02x}").unwrap(); }
    }
    file.push_str(".txt");
    Path::new("trays").join(file)
}

fn read_loadouts(dir: &Path) -> LoadoutFiles {
    let legacy = loadout_file(&PrinterChoice::None);
    let mut files = LoadoutFiles::new();
    if let Ok(text) = std::fs::read_to_string(dir.join(&legacy)) { files.insert(legacy, text); }
    if let Ok(entries) = std::fs::read_dir(dir.join("trays")) {
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_file()) { continue; }
            let file = Path::new("trays").join(entry.file_name());
            if file.extension().is_some_and(|ext| ext == "txt") {
                // An unreadable private file has no usable trays, but still rules out legacy adoption.
                let text = std::fs::read_to_string(entry.path()).unwrap_or_else(|e| {
                    log!("fab: could not read tray loadout {}: {e}", file.display());
                    String::new()
                });
                files.insert(file, text);
            }
        }
    }
    files
}

fn write_loadouts(dir: &Path, files: LoadoutFiles) {
    if files.is_empty() { return; }
    for (file, text) in files {
        if let Err(e) = fab_model::project::write_atomic(&dir.join(file), text.as_bytes()) {
            log!("fab: could not keep the tray loadout: {e}");
        }
    }
}

fn remember_loadout(files: &mut LoadoutFiles, worker: Option<&ProfilesWorker>, id: &PrinterChoice, text: &str) {
    let file = loadout_file(id);
    if files.get(&file).is_some_and(|was| was == text) { return; }
    files.insert(file.clone(), text.to_owned());
    if let Some(worker) = worker {
        let text = text.to_owned();
        worker.update(|w| w.loadouts.insert(file, text));
    }
}

/// What a failed print job's Retry runs again.
#[derive(Clone)]
pub enum Retry {
    Slice,
}

struct SliceJob {
    tray_colours: Vec<[f32; 4]>,
    gen: u64,
    check: Option<u64>,
    setup: CheckSetup,
    source: SliceSource,
    name: String,
}

struct LiveJob {
    job: crate::live_slice::PrintJob,
    input: crate::live_slice::SliceInput,
    revision: u64,
    /// When this input went to the engine.
    sent: std::time::Instant,
    /// This revision's live slice is whole (its print time can be estimated).
    whole: bool,
    /// The planner's preview of this revision is shown (its newer tiles only add issues).
    timed: bool,
    /// How long this revision took to be whole, once it was (seconds since `sent`).
    took: Option<f64>,
}

#[derive(Clone)]
enum SliceSource {
    /// The model's plate, with the distance fields of parts being dragged in place of theirs
    /// (none: the plate as committed).
    Model(Arc<Evaluated>, Arc<[crate::live_slice::FieldSwap]>, Option<(usize, crate::live_slice::PartTransform)>),
    /// The plate's ticked test prints, cut from it.
    Tests(Arc<Evaluated>, Arc<[piece::TestPrint]>, Arc<piece::TestPlan>),
}

impl SliceSource {
    /// The plate it comes from.
    fn plate(&self) -> &Arc<Evaluated> {
        match self {
            SliceSource::Model(ev, ..) | SliceSource::Tests(ev, ..) => ev,
        }
    }
}

impl SliceJob {
    /// Live edits and settles have the same identity and the same worker-side input builder.
    fn prepare(self) -> (crate::live_slice::PrintJob, crate::live_slice::SliceInput) {
        let ev = self.source.plate();
        let machine = ev.machine.clone();
        let palettes = tool_colours(ev, Some(&self.tray_colours));
        let setup = self.setup.clone();
        // The plate is copied on the slicer link's thread, not here (the UI's).
        let source = self.source.clone();
        let (plate_id, parts) = match &self.source {
            SliceSource::Model(ev, swaps, moved) => {
                let mut id = crate::live_slice::edited_plate_id(ev, swaps);
                if let Some((part, xf)) = moved {
                    for v in std::iter::once(*part as u64).chain(xf.iter().flatten().map(|v| v.to_bits())) {
                        id = (id ^ v).wrapping_mul(0x100_0000_01b3);
                    }
                }
                (id, ev.plate.parts.len())
            }
            SliceSource::Tests(ev, tests, plan) => (piece::test_plate_id(crate::live_slice::plate_id(ev), tests, plan), plan.parts()),
        };
        let settings_id = setup.id_for(&self.source.plate().plate);
        let built = std::sync::OnceLock::new();
        let make: makepad_fab_slice::live::MakeInput = Arc::new(move || built.get_or_init(|| match &source {
            SliceSource::Model(ev, swaps, moved) => {
                let fields = crate::live_slice::edited_fields(ev, swaps);
                let input = crate::live_slice::input_for(plate_id, &setup, Arc::new(ev.plate.printable()), ev.machine.clone(), Some(&ev.frames[..]), Some(&fields[..]));
                let mut input = crate::live_slice::with_overlaps_of(input, ev);
                if let Some((part, xf)) = moved {
                    input.place(*part, *xf);
                    let (resolve, plate, machine) = (setup.clone(), input.plate.clone(), input.machine.clone());
                    input.setup = Arc::new(move |kept| resolve.job_setup_of(&plate, &machine, kept));
                }
                input
            }
            SliceSource::Tests(ev, tests, plan) => {
                let whole = crate::live_slice::input_for(crate::live_slice::plate_id(ev), &setup, Arc::new(ev.plate.printable()), ev.machine.clone(), Some(&ev.frames[..]), Some(&ev.fields[..]));
                piece::planned_input(&whole, tests, plan)
            }
        }).clone());
        let fallback: Option<Arc<dyn Fn() -> Arc<fab_printability::Report> + Send + Sync>> = if self.check.is_some() {
            let me = Arc::new(self.clone_for_report());
            Some(Arc::new(move || me.mesh_report()))
        } else {
            None
        };
        let head_envelope = self.setup.head_envelope().to_path_buf();
        let catalog = self.setup.catalog;
        let objects: Arc<[String]> = match &self.source {
            SliceSource::Model(ev, ..) => ev.plate.parts.iter().map(|p| p.object.clone()).collect(),
            SliceSource::Tests(ev, _, plan) => plan.objects(&ev.plate.parts.iter().map(|p| p.object.as_str()).collect::<Vec<_>>()).into(),
        };
        let tests = matches!(self.source, SliceSource::Tests(..)).then(|| make.clone());
        let melts = match &self.source {
            SliceSource::Model(ev, ..) => melt_steps(&ev.plate),
            SliceSource::Tests(..) => Vec::new(),
        };
        let job = crate::live_slice::PrintJob { gen: self.gen, head_envelope, settings: self.setup.settings, colours: palettes.material, model_colours: palettes.model, shared_tools: palettes.shared_tools, machine, name: self.name, objects, check: self.check, fallback, catalog, tests, melts };
        (job, crate::live_slice::SliceInput { plate_id, settings_id, parts, make })
    }

    /// Hand the job to the slicing engine: answered as `Sliced` or `Checked`.
    fn send(self) {
        let (job, input) = self.prepare();
        crate::live_slice::settle(job, input);
    }

    fn clone_for_report(&self) -> SliceJob {
        let source = self.source.clone();
        SliceJob { tray_colours: self.tray_colours.clone(), gen: self.gen, check: self.check, setup: self.setup.clone(), source, name: self.name.clone() }
    }

    fn mesh_report(&self) -> Arc<fab_printability::Report> {
        let ev = self.source.plate();
        let (plate, machine) = (&ev.plate, &ev.machine);
        match resolve_tools(&self.setup.settings, self.setup.catalog.as_deref(), &self.setup.calibration, plate, machine, None) {
            Ok(setup) => {
                let materials = fab_printability::Materials::from_job(&makepad_fab_settings::machine::Job::new(setup.build_config()));
                Arc::new(ev.analyse(&materials))
            }
            Err(_) => ev.printability.clone(),
        }
    }
}

/// Maximum characters of a raw `fab_material_slots` entry shown on the person's local UI.
const MAX_LOCAL_SLOT_CHARS: usize = 64;

/// Structured failure from resolving or validating a printer machine preset
/// (`CheckSetup::machine_typed` / `machine_of`), built by construction — never parsed from text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MachineError {
    /// `self.catalog` is `None` while `use_installed` is true.
    CatalogUnavailable,
    /// Virtual printer layout is rebuilding on `ProfilesWorker`.
    UpdatingVirtualLayout,
    /// `Store::resolve(&preset)` failed: missing preset, deleted/unreadable
    /// file, malformed JSON, missing include, or deep inheritance. `detail` may contain a full
    /// local file path and OS error (local UI only).
    ResolvePreset {
        preset: String,
        detail: String,
    },
    /// `fab_sla::printer::resin_machine` rejected a resin preset.
    ResinSpec {
        preset: String,
        detail: String,
    },
    /// Required filament machine geometry key is missing or invalid:
    /// `printable_area`, `printable_height`, `nozzle_diameter`, `extruder_printable_area`,
    /// `extruder_printable_height`, `bed_exclude_area`, or `wrapping_exclude_area`.
    InvalidKey {
        preset: String,
        key: &'static str,
    },
    /// `family::of(values)` did not recognize the preset's printer family.
    UnsupportedFamily {
        preset: String,
    },
    /// One-spool-per-head printer has more toolheads than `fab_plate::machine::UNITS`.
    TooManyHeads {
        preset: String,
    },
    /// `Machine::parse_slots(&named, count)` rejected an entry in `fab_material_slots`.
    /// `raw_entry` and `detail` carry raw preset text (local UI only; never cross boundaries).
    InvalidMaterialSlots {
        preset: String,
        /// 1-based index of the first failing entry in `fab_material_slots`, when located.
        slot_index: Option<usize>,
        /// Trimmed offending raw entry (local display only).
        raw_entry: Option<String>,
        /// Full parser error from `Machine::parse_slots` (local display only).
        detail: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresetRole {
    Printer,
    Process,
    Filament,
}

impl PresetRole {
    fn label(self) -> &'static str {
        match self {
            PresetRole::Printer => "printer",
            PresetRole::Process => "process",
            PresetRole::Filament => "filament",
        }
    }
}

/// Structured failure from `CheckSetup::job_setup_of_typed` / `resolve_tools_with`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetupError {
    Machine(MachineError),
    /// `Catalog::store.resolve(preset)` failed for a filament, printer, or process preset.
    ResolvePreset {
        role: PresetRole,
        preset: String,
        detail: String,
    },
    /// Non-path validation error (bed plate, nozzle count, unsupported family, etc.).
    Validation(String),
}

fn first_line_or(text: &str, empty: &str) -> String {
    text.lines().next().map(str::trim).filter(|s| !s.is_empty()).unwrap_or(empty).to_owned()
}

fn trim_chars(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let mut head: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() { head.push('…'); }
    head
}

impl MachineError {
    pub fn local_message(&self) -> String {
        match self {
            Self::CatalogUnavailable => "The installed printer profiles are unavailable".into(),
            Self::ResolvePreset { preset, detail } => format!("{}: {}", preset_label(preset), first_line_or(detail, "preset could not be loaded")),
            Self::ResinSpec { detail, .. } => first_line_or(detail, "invalid resin printer preset"),
            Self::InvalidMaterialSlots { preset, slot_index, raw_entry, detail } => {
                let mut shown = first_line_or(detail, "invalid fab_material_slots");
                if let Some(raw) = raw_entry.as_ref().filter(|s| s.chars().count() > MAX_LOCAL_SLOT_CHARS) {
                    shown = if shown.contains(raw) {
                        shown.replacen(raw, &trim_chars(raw, MAX_LOCAL_SLOT_CHARS), 1)
                    } else {
                        trim_chars(&shown, 2 * MAX_LOCAL_SLOT_CHARS)
                    };
                }
                let preset = preset_label(preset);
                match slot_index {
                    Some(n) => format!("{preset}: fab_material_slots entry {n} — {shown}"),
                    None => format!("{preset}: {shown}"),
                }
            }
            _ => self.redacted_message(),
        }
    }
}

fn joint_kind_label(proto: &str) -> Option<&'static str> {
    match proto {
        "Hinge" => Some("hinge"),
        "Slider" => Some("slider"),
        "Bearing" => Some("bearing"),
        "Ball" | "BallJoint" => Some("ball joint"),
        "Bond" => Some("bond"),
        "Anchor" => Some("anchor"),
        "String" => Some("string"),
        _ => None,
    }
}


fn script_basename(file: &str) -> String {
    Path::new(file).file_name().and_then(|n| n.to_str()).unwrap_or(file).to_owned()
}

fn format_measured_summary(m: &makepad_csg_mech::Measured, layer: f64) -> String {
    let mut values = Vec::new();
    if let Some(n) = m.c_z_layers { values.push(format!("Z gap {n} layers ({:.2} mm)", n as f64 * layer)); }
    if let Some(c) = m.c_xy { values.push(format!("Clearance {c:.2} mm")); }
    if let Some(u) = m.mu { values.push(format!("Friction {u:.2}")); }
    if values.is_empty() { "default calibration".into() } else { values.join(", ") }
}

fn classify_rebuild_failure(
    f: &fab_model::eval::Failure, ev: Option<&Evaluated>, program: Option<&fab_model::program::Compiled>,
) -> RebuildFailureKind {
    let reason = first_line_or(&f.message, "model failed to build");
    if f.id.is_empty() {
        return RebuildFailureKind::Program { file: (!f.file.is_empty()).then(|| script_basename(&f.file)), line: f.line, col: f.col, reason };
    }
    let instances = || program.into_iter().flat_map(|p| p.instances.iter()).filter(|i| i.script.file == f.file);
    let name_of = |object: &str| {
        let ev = ev?;
        ev.plate.parts.iter().find(|p| p.object == object).map(|p| &p.name)
            .or_else(|| ev.plate.excluded.iter().find(|p| p.part.object == object).map(|p| &p.part.name))
    };
    let mut parts = Vec::<String>::new();
    let mut solids = Vec::new();
    let mut joint = None;
    let mut solid_op = None;
    for inst in instances() {
        let p = &inst.script.program;
        let call = p.joints.iter().filter(|c| c.id == f.id).max_by_key(|c| c.loc == Some((f.line, f.col)));
        let mut add = |index: usize| {
            let id = &p.solids[index].id;
            let object = fab_model::program::part_id(&inst.name, id);
            if let Some(name) = name_of(&object) {
                if !parts.contains(name) { parts.push(name.clone()); }
            }
            if !solids.contains(id) { solids.push(id.clone()); }
        };
        if let Some(c) = call.filter(|_| f.op == "joint") {
            joint = Some(joint_kind_label(c.proto));
            for index in [Some(c.a), c.b, c.shoe].into_iter().flatten() { add(index); }
        } else if let Some(index) = p.solids.iter().position(|solid| solid.id == f.id) {
            solid_op = Some(p.solids[index].proto);
            add(index);
        }
    }
    if parts.is_empty() && f.op == "joint" && joint.is_none() && solid_op.is_none() {
        for m in ev.into_iter().flat_map(|e| e.mech.iter()).filter(|m| m.file == f.file && m.joint_ids.contains(&f.id)) {
            for object in &m.names {
                if let Some(name) = name_of(object) { if !parts.contains(name) { parts.push(name.clone()); } }
            }
        }
    }
    if parts.is_empty() {
        for inst in instances() {
            for placed in &inst.placed {
                let object = fab_model::program::part_id(&inst.name, &inst.script.program.solids[placed.solid].id);
                if let Some(name) = name_of(&object) { if !parts.contains(name) { parts.push(name.clone()); } }
            }
        }
        if parts.is_empty() { parts = solids; }
    }
    if f.op == "joint" && (joint.is_some() || solid_op.is_none()) {
        RebuildFailureKind::Joint { parts, kind: joint.flatten(), id: f.id.clone(), reason }
    } else {
        let op = if f.op == "joint" { solid_op.unwrap_or(&f.op) } else { &f.op };
        RebuildFailureKind::Solid { parts, id: f.id.clone(), op: op.into(), reason }
    }
}

fn format_rebuild_failure(
    kind: &RebuildFailureKind,
    measured: &makepad_csg_mech::Measured,
    layer: f64,
) -> String {
    let cal = format_measured_summary(measured, layer);
    match kind {
        RebuildFailureKind::Joint { parts, kind, id, reason } => {
            let joint = match kind {
                Some(k) => format!("{k} `{id}`"),
                None => format!("joint `{id}`"),
            };
            let head = if parts.is_empty() { joint } else { format!("{} · {joint}", parts.join(", ")) };
            format!("{head} ({cal}): {reason} — edit the model or adjust/clear calibration.")
        }
        RebuildFailureKind::Solid { parts, id, op, reason } => {
            let subject = if op.is_empty() { format!("solid `{id}`") } else { format!("`{id}` ({op})") };
            let head = if parts.is_empty() { subject } else { format!("{} · {subject}", parts.join(", ")) };
            format!("{head} ({cal}): {reason} — edit the model or adjust/clear calibration.")
        }
        RebuildFailureKind::Program { file, line, col, reason } => {
            let head = match file {
                Some(file) if *line > 0 => format!("script `{file}`:{line}:{col}"),
                Some(file) => format!("script `{file}`"),
                None if *line > 1 || *col > 1 => format!("project:{line}:{col}"),
                None => "project build".to_string(),
            };
            format!("{head} ({cal}): {reason} — edit the model or adjust/clear calibration.")
        }
    }
}

fn scrub_known_paths(text: &str, failure: &fab_model::eval::Failure, program: Option<&fab_model::program::Compiled>) -> String {
    let mut out = text.to_string();
    let files = std::iter::once(failure.file.as_str())
        .chain(program.into_iter().flat_map(|p| p.instances.iter().map(|i| i.script.file.as_str())));
    for full in files {
        if full.is_empty() { continue; }
        let base = script_basename(full);
        if base != full && out.contains(full) {
            out = out.replace(full, &base);
        }
    }
    out
}

pub(crate) fn redacted_eval_failure(f: &fab_model::eval::Failure) -> String {
    let file = if f.file.is_empty() { String::new() } else { format!("{}:", script_basename(&f.file)) };
    let msg = scrub_known_paths(&first_line_or(&f.message, "model failed to build"), f, None);
    if f.id.is_empty() {
        format!("{file}{}:{}: {msg}", f.line, f.col)
    } else {
        format!("{file}{}:{}: {} ({}): {msg}", f.line, f.col, f.id, f.op)
    }
}

fn redact_rebuild_kind(kind: &RebuildFailureKind, failure: &fab_model::eval::Failure, program: Option<&fab_model::program::Compiled>) -> RebuildFailureKind {
    let mut redacted = kind.clone();
    let reason = match &mut redacted {
        RebuildFailureKind::Joint { reason, .. } | RebuildFailureKind::Solid { reason, .. } | RebuildFailureKind::Program { reason, .. } => reason,
    };
    *reason = scrub_known_paths(reason, failure, program);
    redacted
}

fn preset_label(preset: &str) -> String {
    let name = first_line_or(preset, "");
    if name.is_empty() {
        "the selected printer preset".to_string()
    } else {
        format!("the printer preset `{name}`")
    }
}

impl MachineError {
    /// Safe for any outbound boundary (cloud AI, MCP agents, `Done::text`, `PrintEvent::Checked`):
    /// names only the failing step and preset name — never a local path, OS error, or raw value.
    pub fn redacted_message(&self) -> String {
        match self {
            MachineError::CatalogUnavailable => "the installed printer profiles are unavailable".into(),
            MachineError::UpdatingVirtualLayout => "Updating the virtual printer layout".into(),
            MachineError::ResolvePreset { preset, .. } => {
                format!("could not read {}", preset_label(preset))
            }
            MachineError::ResinSpec { preset, .. } => {
                format!("{} has invalid resin printer settings", preset_label(preset))
            }
            MachineError::InvalidKey { preset, key } => {
                format!("{}: missing or invalid {key}", preset_label(preset))
            }
            MachineError::UnsupportedFamily { preset } => {
                format!("{}: {}", preset_label(preset), makepad_fab_settings::family::UNSUPPORTED)
            }
            MachineError::TooManyHeads { preset } => {
                format!("{}: this printer has more heads than its trays can be numbered", preset_label(preset))
            }
            MachineError::InvalidMaterialSlots { preset, .. } => {
                format!("{} has an invalid `fab_material_slots` entry", preset_label(preset))
            }
        }
    }
}

impl SetupError {
    pub fn redacted_message(&self) -> String {
        match self {
            SetupError::Machine(m) => m.redacted_message(),
            SetupError::ResolvePreset { role, preset, .. } => {
                let name = first_line_or(preset, "");
                if name.is_empty() {
                    format!("could not read the selected {} preset", role.label())
                } else {
                    format!("could not read the {} preset `{name}`", role.label())
                }
            }
            SetupError::Validation(msg) => msg.clone(),
        }
    }
}

/// The configured printer that owns measurements, including those used by its twins.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CalibrationOwner {
    pub id: String,
    pub title: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CalibrationKey {
    pub owner: Option<CalibrationOwner>,
    pub preset: String,
    pub filaments: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EffectiveCalibration {
    pub owner: Option<CalibrationOwner>,
    pub preset: String,
    pub by_filament: std::collections::BTreeMap<String, EffectiveResults>,
}

impl EffectiveCalibration {
    pub fn resolve_in(dir: &Path, key: &CalibrationKey) -> Self {
        let owner_id = key.owner.as_ref().map(|owner| owner.id.as_str());
        let by_filament = key.filaments.iter().map(|filament| {
            let results = fab_calibrate::Results::load_effective_in(dir, owner_id, &key.preset, filament);
            (filament.clone(), results)
        }).collect();
        Self { owner: key.owner.clone(), preset: key.preset.clone(), by_filament }
    }

    pub fn for_filament(&self, filament: &str) -> Option<&EffectiveResults> {
        self.by_filament.get(filament)
    }

    pub fn first_for(&self, settings: &PrintSettings) -> Option<&EffectiveResults> {
        self.for_filament(settings.materials.first().map_or("", |f| f.preset.as_str()))
    }

    fn mech_measured(&self, settings: &PrintSettings) -> makepad_csg_mech::Measured {
        let fit = self.first_for(settings).map(fab_calibrate::MechFit::from_effective).unwrap_or_default();
        makepad_csg_mech::Measured { c_xy: fit.c_xy, c_z_layers: fit.c_z_layers, mu: fit.mu }
    }

    pub fn slicer_for(&self, filament: &str) -> SlicerCalibration {
        self.for_filament(filament).map(SlicerCalibration::from_effective).unwrap_or_default()
    }

    pub fn hash_slicer_values(&self, settings: &PrintSettings, seed: u64) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        seed.hash(&mut hash);
        if settings.materials.is_empty() {
            self.slicer_for("").hash(&mut hash);
        }
        for filament in &settings.materials {
            self.slicer_for(&filament.preset).hash(&mut hash);
        }
        hash.finish()
    }
}

/// Immutable settings snapshot for the model worker; preset resolution is I/O.
#[derive(Clone)]
pub struct CheckSetup {
    dirs: Arc<crate::dirs::FabDirs>,
    calibration: Arc<EffectiveCalibration>,
    mapping: Option<Arc<Mapped>>,
    settings: PrintSettings,
    catalog: Option<Arc<Catalog>>,
}

fn waits_mapping(mapping: Option<&Arc<Mapped>>, ev: &Evaluated) -> Option<&'static str> {
    let mapping = mapping?;
    (!Arc::ptr_eq(mapping, &ev.mapped) && mapping != &ev.mapped)
        .then_some("Updating the plate for the printer's colours")
}

impl CheckSetup {
    pub fn head_envelope(&self) -> &Path {
        &self.dirs.head_envelope
    }

    /// Fab's own defaults (no installed profiles read yet).
    pub fn fab_defaults() -> Self {
        CheckSetup { dirs: Arc::default(), calibration: Arc::default(), settings: defaults_settings(), catalog: None, mapping: None }
    }

    /// A rebuilt plate must carry the mapping that assigned these materials to nozzles.
    pub fn plate_waits_mapping(&self, ev: &Evaluated) -> Option<&'static str> {
        waits_mapping(self.mapping.as_ref(), ev)
    }

    /// Authoritative profile-mismatch predicate: true when `ev` has printable parts built
    /// with different calibrated mechanism fits (`c_xy`, `c_z_layers`, `mu`) than this setup.
    /// Pure in-memory O(log F) lookup, zero disk I/O.
    pub fn plate_profile_mismatch(&self, ev: &Evaluated) -> bool {
        !ev.plate.parts.is_empty() && ev.profile.measured != self.mech_measured()
    }

    /// Worker-side guard (`model_job`, `live_slice::send_live_plate`): never slice an `ev`
    /// whose colour mapping or calibrated mechanics profile lags behind this setup.
    pub fn plate_waits(&self, ev: &Evaluated) -> Option<&'static str> {
        self.plate_waits_mapping(ev).or_else(|| {
            self.plate_profile_mismatch(ev).then_some(UPDATING_CALIBRATION_PLATE)
        })
    }

    pub fn mech_measured(&self) -> makepad_csg_mech::Measured {
        self.calibration.mech_measured(&self.settings)
    }

    pub fn calibration_owner(&self) -> Option<&CalibrationOwner> {
        self.calibration.owner.as_ref()
    }

    /// The selected print profile's name.
    pub fn process(&self) -> &str {
        &self.settings.process
    }

    /// The selected printer's geometry (reads the inherited preset files).
    pub fn machine(&self) -> Result<Machine, String> {
        self.machine_typed().map_err(|e| e.local_message())
    }

    pub fn machine_typed(&self) -> Result<Machine, MachineError> {
        if !self.settings.use_installed {
            return Ok(Machine::dual_nozzle_350());
        }
        let catalog = self.catalog.as_ref().ok_or(MachineError::CatalogUnavailable)?;
        let preset = catalog.store.resolve(&self.settings.machine).map_err(|detail| MachineError::ResolvePreset {
            preset: self.settings.machine.clone(), detail,
        })?;
        machine_of(&preset.name, &preset.values)
    }

    /// Equal ids slice to the same toolpaths (settings and profile catalogue).
    pub fn id_for(&self, plate: &Plate) -> u64 {
        // The project's global settings slice differently, whatever the setup.
        plate.settings.iter().flat_map(|(k, v)| k.bytes().chain([0u8]).chain(v.bytes()).chain([1u8])).fold(self.id(), |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
    }

    pub fn id(&self) -> u64 {
        let text = format!("{:?}|{:?}", self.settings, self.catalog.as_ref().map(|c| Arc::as_ptr(c) as usize));
        let base = text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
        self.calibration.hash_slicer_values(&self.settings, base)
    }

    /// What a calibration test part is made for (reads preset files: call off the UI thread).
    pub fn coupon_setup(&self) -> fab_calibrate::CouponSetup {
        coupon_setup(&self.settings, self.catalog.as_deref(), self.calibration.first_for(&self.settings))
    }

    /// The mechanisms' print profile: nozzle, layer and filament type of the job, with the clearances and
    /// friction measured on this printer and filament. Preset files are resolved on the worker;
    /// the measurements come from its immutable calibration snapshot.
    pub fn mech_profile(&self) -> makepad_csg_mech::Profile {
        let cs = self.coupon_setup();
        makepad_csg_mech::Profile {
            nozzle: cs.nozzle,
            layer: cs.layer,
            material: makepad_csg_mech::Material::named(&cs.material),
            measured: self.mech_measured(),
            ..Default::default()
        }
    }

    /// The printer and first filament the calibration results are stored under.
    pub fn calibration_names(&self) -> (String, String) {
        (self.settings.machine.clone(), self.settings.materials.first().map(|f| f.preset.clone()).unwrap_or_default())
    }

    /// The job's setup for `plate` (reads preset files: call off the UI thread).
    pub fn job_setup(&self, plate: &Plate, machine: &Machine) -> Result<JobSetup, String> {
        self.job_setup_of(plate, machine, None)
    }

    /// [`Self::job_setup`], with `kept` of only those of its tools (0-based, ascending): the ones
    /// a slice of it printed in.
    pub fn job_setup_of(&self, plate: &Plate, machine: &Machine, kept: Option<&[usize]>) -> Result<JobSetup, String> {
        self.job_setup_of_typed(plate, machine, kept).map_err(|e| e.redacted_message())
    }

    fn job_setup_of_typed(&self, plate: &Plate, machine: &Machine, kept: Option<&[usize]>) -> Result<JobSetup, SetupError> {
        if !self.machine_typed().map_err(SetupError::Machine)?.same_volume(machine) {
            return Err(SetupError::Validation("Updating the plate for the selected printer".into()));
        }
        resolve_tools(&self.settings, self.catalog.as_deref(), &self.calibration, plate, machine, kept)
    }

    /// The job the app would slice a model with: setup, its paint streams (none: a model's
    /// paint is on its parts) and a name for it.
    pub fn model_job(&self, ev: &Evaluated) -> Result<(JobSetup, Vec<Vec<String>>, String), String> {
        if let Some(why) = self.plate_waits(ev) { return Err(why.into()); }
        let paint = Vec::new();
        let job = self.job_setup(&ev.plate, &ev.machine)?;
        Ok((job, paint, format!("the app's print setup: {} / {}", self.settings.machine, self.settings.process)))
    }

    pub fn materials(&self, plate: &Plate, machine: &Machine) -> Result<fab_printability::Materials, String> {
        let setup = self.job_setup(plate, machine)?;
        Ok(fab_printability::Materials::from_job(&makepad_fab_settings::machine::Job::new(setup.build_config())))
    }

    /// The tolerance (mm) the job's slice keeps to an exact part's surface: its `resolution`,
    /// the one its G-code paths keep (one the job leaves unset is the option's default).
    pub fn print_chord(&self, plate: &Plate, machine: &Machine) -> Result<f64, String> {
        let setup = self.job_setup(plate, machine)?;
        let r = makepad_fab_settings::machine::Job::new(setup.build_config()).f("resolution");
        Ok(if r > 0.0 { r } else { fab_model::eval::default_print_chord() })
    }

    /// What the job prints round the parts on the first layer ([`fab_model::eval::Rim`]): its
    /// skirt and each part's brim, which the layout keeps where the parts' nozzles reach (reads
    /// preset files: call off the UI thread).
    pub fn print_rim(&self, plate: &Plate, machine: &Machine) -> Result<fab_model::eval::Rim, String> {
        let setup = self.job_setup(plate, machine)?;
        Ok(rim_of(&makepad_fab_settings::machine::Job::new(setup.build_config()), plate.parts.len()))
    }
}

/// [`CheckSetup::print_rim`] of a resolved job of `objects` objects. The skirt's loops are those
/// the G-code writer plans: round the whole plate, or round each object when it prints object by
/// object, the outer one `skirt_distance` plus its loops' first-layer lines out. The brim is the
/// one its `brim_type` gives (none under a raft): its width and gap, or the automatic one.
fn rim_of(job: &makepad_fab_settings::machine::Job, objects: usize) -> fab_model::eval::Rim {
    use fab_model::eval::{Brim, Rim};
    use makepad_fab_toolpath::brim::BrimKind;
    let loops = job.cfg.i64_at("skirt_loops", 0).max(0) as f64;
    let shield = job.s("draft_shield");
    let loops = if job.s("print_sequence") == "by object" && objects > 1 {
        if job.b("skirt_per_object") && (loops > 0.0 || !matches!(shield, "" | "disabled")) { loops.max(1.0) } else { 0.0 }
    } else if job.cfg.i64_at("skirt_height", 0) <= 0 && shield == "disabled" {
        0.0
    } else {
        loops
    };
    // A skirt line is the first layer's width (of the nozzle, or absolute), else the nozzle's.
    let width = (0..job.filaments().max(1)).map(|f| {
        let nozzle = job.nozzle_diameter(job.ext_of(f)).max(0.1);
        ["initial_layer_line_width", "line_width"].iter().map(|k| job.cfg.abs_or_pct(k, 0, nozzle)).find(|&w| w > 0.0).unwrap_or(nozzle)
    }).fold(0.0, f64::max);
    let skirt = if loops > 0.0 { job.f("skirt_distance").max(0.0) + loops * width } else { 0.0 };
    let p = makepad_fab_toolpath::Params::from_job(job, 0);
    let brim = match p.brim_kind {
        BrimKind::OuterOnly | BrimKind::OuterAndInner if p.brim_width > 0.0 => Brim::Outer(p.brim_width + p.brim_gap),
        BrimKind::AutoBrim => Brim::Auto(p.brim_gap),
        BrimKind::OuterOnly | BrimKind::OuterAndInner | BrimKind::NoBrim | BrimKind::InnerOnly => Brim::None,
    };
    Rim { skirt, brim }
}

impl Printing {
    pub fn machine(&self) -> Result<&Machine, String> {
        self.machine.as_deref().map_err(MachineError::local_message)
    }

    pub fn machine_error(&self) -> Option<&MachineError> {
        self.machine.as_ref().err()
    }

    /// Keep `l` as the loadout, and on disk for the next run (a few hundred bytes, written only
    /// when it changes).
    fn keep_loadout(&mut self, id: &PrinterChoice, l: colours::Layout, reported_model: Option<&str>) {
        let text = loadout_text(&l, reported_model.unwrap_or(&self.machine_model));
        remember_loadout(&mut self.loadouts, self.profiles_worker.as_ref(), id, &text);
        self.loadout_text = Some(text);
        self.loadout = Some(l);
    }

    fn select_loadout(&mut self, state: &PrintState) {
        self.loadout_text = self.loadouts.get(&loadout_file(&state.links.picked)).cloned();
        self.loadout = self.loadout_text.as_deref().and_then(|t| colours::parse_loadout(loadout_model(t).1, &self.offline().heads));
        self.adopt_legacy_loadout(state);
    }

    /// Only the named real configuration can claim the legacy file. When it records a model,
    /// wait for that printer to identify itself: the plate's selected profile can be any machine.
    fn adopt_legacy_loadout(&mut self, state: &PrintState) {
        if self.loadout_text.is_some() { return; }
        let PrinterChoice::Real(name) = &state.links.picked else { return };
        let Some(legacy) = self.loadouts.get(Path::new("trays.txt")) else { return };
        let (model, text) = loadout_model(legacy);
        if text.lines().next().and_then(|line| line.strip_prefix("printer\t")) != Some(name.as_str()) { return; }
        let reported = state.target_printer().filter(|s| !s.virtual_printer);
        if reported.is_some_and(|s| &s.name != name) { return; }
        if let Some(model) = model {
            let Some(report) = reported else { return };
            if report.model.is_empty() || (report.model != model && !same_printer(&report.model, model)) { return; }
        }
        let Some(layout) = colours::parse_loadout(text, &self.offline().heads) else { return };
        let text = legacy.clone();
        remember_loadout(&mut self.loadouts, self.profiles_worker.as_ref(), &state.links.picked, &text);
        self.loadout_text = Some(text);
        self.loadout = Some(layout);
    }

    /// The printer the colours map onto with no reports: the selected printer's slots (none
    /// while its profile does not resolve).
    fn offline(&self) -> colours::Offline {
        match &self.machine {
            Ok(m) => colours::Offline::of(m),
            Err(_) => colours::Offline { printer: colours::OFFLINE_PRINTER.into(), heads: Vec::new(), slots: Vec::new() },
        }
    }

    /// True when `ev` has printable parts whose `ev.profile.measured` differs from the active
    /// target's resolved calibration. O(log F) in-memory lookup, zero disk I/O.
    pub fn plate_profile_mismatch(&self, state: &PrintState, ev: &Evaluated) -> bool {
        !ev.plate.parts.is_empty() && ev.profile.measured != self.calibration.mech_measured(&state.settings)
    }

    /// Active failure message if the current target's calibrated `Measured` already failed to
    /// rebuild on `ModelHost`.
    fn active_calibration_failure(&self, state: &PrintState) -> Option<&ProfileRebuildFailure> {
        self.profile_rebuild_error.as_ref().filter(|f| f.measured == self.calibration.mech_measured(&state.settings))
    }

    /// Non-calibration geometry waits (colour mapping or printer bed volume). Never marks a
    /// settled fixed box stale during Stage 2.
    pub(super) fn plate_waits_geometry(&self, _state: &PrintState, ev: &Evaluated) -> Option<&'static str> {
        waits_mapping(self.mapping.as_ref(), ev).or_else(|| {
            let machine = self.machine.as_ref().ok()?;
            (!machine.same_volume(&ev.machine)).then_some("Updating the plate for the selected printer")
        })
    }

    /// Single authoritative readiness gate for any non-empty `Evaluated`.
    /// Checks the profile mismatch directly — never solely the `calibration_plate_pending` flag.
    pub(super) fn plate_gate(&self, state: &PrintState, ev: &Evaluated) -> Option<PlateGate> {
        if self.calibration_pending() {
            return Some(PlateGate::Wait(LOADING_CALIBRATION));
        }
        if let Err(error) = &self.machine { return Some(PlateGate::MachineInvalid(error.clone())); }
        if self.plate_profile_mismatch(state, ev) {
            if let Some(msg) = self.active_calibration_failure(state).map(|f| f.message.as_str()) {
                return Some(PlateGate::CalibrationFailed(msg.to_owned()));
            }
            if state.calibration_plate_pending {
                return Some(PlateGate::Wait(UPDATING_CALIBRATION_PLATE));
            }
            // Defensive invariant: ev.profile.measured != expected with no rebuild in flight.
            // Never fall through to slicing or packaging old geometry.
            return Some(PlateGate::CalibrationFailed(self.default_mismatch_error(state, Some(ev))));
        }
        self.plate_waits_geometry(state, ev).map(PlateGate::Wait)
    }

    /// Non-empty when `plate` cannot be sliced or packaged due to a calibration profile mismatch
    /// failure (never true during Stage 1 `ProfilesWorker` resolve).
    pub(super) fn calibration_failure(&self, state: &PrintState, plate: Option<&Arc<Evaluated>>) -> Option<String> {
        if self.calibration_pending() {
            return None;
        }
        if let Some(ev) = plate {
            return match self.plate_gate(state, ev) {
                Some(PlateGate::CalibrationFailed(err)) => Some(err),
                _ => None,
            };
        }
        state.calibration_error.clone()
    }

    /// Keep `plate_waits` strictly for `Wait` states (callers that need wait-vs-fail use `plate_gate`).
    pub(super) fn plate_waits(&self, state: &PrintState, ev: &Evaluated) -> Option<&'static str> {
        match self.plate_gate(state, ev) {
            Some(PlateGate::Wait(why)) => Some(why),
            _ => None,
        }
    }
    /// Put the editor into the calibration-rebuild failure state without setting `reask` or
    /// starting any slice or package.
    pub(super) fn fail_for_calibration(&mut self, state: &mut PrintState, message: String, for_export: bool) {
        let for_export = for_export || matches!(&state.export, ExportStatus::Failed(reason) if reason == &message);
        self.stop_slice(state);
        state.calibration_plate_pending = false;
        state.calibration_error = Some(message.clone());
        state.status = SliceStatus::Failed(message.clone());
        if for_export {
            state.export = ExportStatus::Failed(message);
        }
        state.bump();
    }

    /// Clear any active calibration rebuild failure (e.g. calibration corrected/cleared, model
    /// rebuilt with matching `Measured`, printer retargeted, or plate emptied). Only clears
    /// `status` / `export` / `print_blocked` entries that came from this calibration failure.
    pub(super) fn clear_calibration_error(&mut self, state: &mut PrintState) -> bool {
        let old = self.profile_rebuild_error.take().map(|f| f.message)
            .or_else(|| state.calibration_error.clone());
        let had_ui = state.calibration_error.take().is_some();
        let Some(old) = old else { return had_ui };
        if matches!(&state.status, SliceStatus::Failed(m) if *m == old) {
            state.status = SliceStatus::Idle;
        }
        if matches!(&state.export, ExportStatus::Failed(m) if *m == old) {
            state.export = ExportStatus::Idle;
        }
        if state.print_blocked.get(&state.links.picked).is_some_and(|m| m == old.as_str()) {
            state.print_blocked.set(&state.links.picked, None);
        }
        state.bump();
        true
    }
    pub fn sync_calibration_plate(&mut self, state: &mut PrintState, plate: Option<&Evaluated>, has_host: bool) -> bool {
        let mismatch = plate.is_some_and(|ev| self.plate_profile_mismatch(state, ev));
        let mut changed = false;
        if !mismatch {
            // Plate now matches the active target's calibration (or is empty): release any failure.
            changed |= self.clear_calibration_error(state);
        }
        let failed_msg = if mismatch && !self.calibration_pending() {
            self.active_calibration_failure(state).map(|f| f.message.as_str()).map(str::to_owned)
        } else {
            None
        };
        let pending = has_host && self.machine.is_ok() && !self.calibration_pending() && mismatch && failed_msg.is_none();
        if state.calibration_plate_pending != pending {
            state.calibration_plate_pending = pending;
            changed = true;
        }
        if state.calibration_error != failed_msg {
            state.calibration_error = failed_msg;
            changed = true;
        }
        if !pending
            && matches!(&state.status, SliceStatus::Running { stage, .. } if stage == UPDATING_CALIBRATION_PLATE)
        {
            state.status = SliceStatus::Idle;
            changed = true;
        }
        if changed {
            state.bump();
        }
        changed
    }

    pub fn calibration_waiting(&self, state: &PrintState) -> bool {
        self.calibration_pending() || state.calibration_plate_pending
    }

    fn default_mismatch_error(&self, state: &PrintState, plate: Option<&Evaluated>) -> String {
        let layer = if state.settings.layer_height > 0.0 { state.settings.layer_height } else { plate.map_or(0.2, |ev| ev.profile.layer) };
        format!("plate does not match active calibration ({}) — edit the model or adjust/clear calibration.",
            format_measured_summary(&self.check_setup(state).mech_measured(), layer))
    }

    pub fn external_slice_error(&self, state: &PrintState) -> Option<String> {
        if let Err(e) = &self.machine { return Some(e.redacted_message()); }
        if let Some(f) = self.active_calibration_failure(state) { return Some(f.redacted_message.clone()); }
        match &state.status {
            SliceStatus::Failed(error) => Some(first_line_or(error, "internal slicer error")),
            _ => None,
        }
    }

    pub fn profile_rebuild_failed(&mut self, state: &mut PrintState, plate: Option<&Evaluated>, program: Option<&fab_model::program::Compiled>, failure: &fab_model::eval::Failure) {
        let measured = self.check_setup(state).mech_measured();
        let layer = if state.settings.layer_height > 0.0 { state.settings.layer_height } else { plate.map_or(0.2, |e| e.profile.layer) };
        let kind = classify_rebuild_failure(failure, plate, program);
        let message = format_rebuild_failure(&kind, &measured, layer);
        let redacted_message = format_rebuild_failure(&redact_rebuild_kind(&kind, failure, program), &measured, layer);
        self.profile_rebuild_error = Some(ProfileRebuildFailure { measured, layer, source: failure.clone(), kind, message: message.clone(), redacted_message });
        state.calibration_error = Some(message.clone());
        self.reject_held_plate(state, message);
    }

    pub fn machine_preset_failed(&mut self, state: &mut PrintState, err: &MachineError) {
        self.machine = Err(err.clone());
        self.clear_calibration_error(state);
        self.reject_held_plate(state, err.local_message());
    }

    fn reject_held_plate(&mut self, state: &mut PrintState, message: String) {
        state.calibration_plate_pending = false;
        let held = self.pending_send.take().is_some();
        let exporting = self.reask.take() == Some(true) || self.export_waits;
        self.stop_slice(state);
        if held {
            state.print_blocked.set(&state.links.picked, Some(message.clone()));
            state.follow.closed = false;
        }
        if exporting { state.export = ExportStatus::Failed(message.clone()); }
        state.status = SliceStatus::Failed(message);
        state.bump();
    }

    pub fn print_setup_seq(&self) -> u64 { self.print_setup_seq }

    pub fn next_print_setup(&mut self) -> Option<u64> {
        self.setup_deferred = self.calibration_pending();
        if self.setup_deferred { return None; }
        self.print_setup_seq += 1;
        Some(self.print_setup_seq)
    }

    pub fn setup_deferred(&self) -> bool { self.setup_deferred }


    /// Keep the previous slice visible while the plate rebuilds. Print must wait for its replacement.
    pub(super) fn mark_shown_slice_stale(state: &mut PrintState) {
        if state.preview.is_some() {
            state.stale = true;
        }
    }

    pub fn calibration_owner(&self, state: &PrintState) -> Option<CalibrationOwner> {
        let configured = |name: &str| (!name.is_empty() && name != "No printer").then(|| CalibrationOwner { id: format!("printer:{name}"), title: name.into() });
        let mut current = match &state.links.picked {
            PrinterChoice::None => return None,
            PrinterChoice::Real(name) => return configured(name),
            PrinterChoice::Virtual(name) => name.as_str(),
        };
        let saved = self.virtuals.as_deref()?;
        let mut visited = std::collections::HashSet::new();
        while visited.insert(current) {
            let printer = saved.printers.iter().find(|p| p.configured_for.is_none() && p.name == current)?;
            let VirtualSource::Twin { source: Some(source), .. } = &printer.source else { return None };
            if let Some(config) = source.strip_prefix("printer:") { return configured(config); }
            current = source.strip_prefix("virtual:")?;
        }
        None
    }

    pub(super) fn requested_calibration_owner(&self) -> Option<&CalibrationOwner> { self.calibration_key.owner.as_ref() }

    pub fn effective_calibration(&self) -> &Arc<EffectiveCalibration> { &self.calibration }

    pub fn calibration_pending(&self) -> bool {
        self.profiles_worker.is_some() && self.calibration_resolved < self.calibration_seq
    }

    fn sync_calibration_pending(&self, state: &mut PrintState) {
        let pending = self.calibration_pending();
        if state.calibration_pending != pending {
            state.calibration_pending = pending;
            state.bump();
        }
    }

    pub fn sync_calibration(&mut self, state: &mut PrintState) -> bool {
        let picked = &state.links.picked;
        let records = match picked {
            PrinterChoice::Virtual(_) => self.virtuals.as_ref(),
            _ => None,
        };
        let same_target = self.calibration_target.as_ref().is_some_and(|(target, saved)| {
            target == picked && saved.as_ref().map(Arc::as_ptr) == records.map(Arc::as_ptr)
        });
        let key = &self.calibration_key;
        let materials = &state.settings.materials;
        let already_requested = self.calibration_seq > 0 || self.profiles_worker.is_none();
        if same_target && key.preset == state.settings.machine && already_requested
            && self.calibration_slots.iter().eq(materials.iter().map(|m| &m.preset)) {
            return false;
        }
        let owner = self.calibration_owner(state);
        self.calibration_target = Some((picked.clone(), records.cloned()));
        self.calibration_slots = materials.iter().map(|m| m.preset.clone()).collect();
        let mut filaments = self.calibration_slots.clone();
        if filaments.is_empty() { filaments.push(String::new()); }
        filaments.sort();
        filaments.dedup();
        if already_requested && key.owner == owner && key.preset == state.settings.machine && key.filaments == filaments {
            return false;
        }
        self.clear_calibration_error(state);
        self.calibration_key = CalibrationKey { owner, preset: state.settings.machine.clone(), filaments };
        let Some(worker) = &self.profiles_worker else { return false };
        self.calibration_seq += 1;
        worker.update(|work| work.cal_resolve = Some(CalResolveRequest { key: self.calibration_key.clone(), seq: self.calibration_seq }));
        self.sync_calibration_pending(state);
        true
    }

    fn queue_calibration_write(&mut self, state: &mut PrintState, write: CalWrite) {
        let affects = write_affects_key(&write, &self.calibration_key);
        if let Some(worker) = &self.profiles_worker {
            if affects { self.calibration_seq += 1; }
            worker.update(|work| {
                work.cal_writes.push(write);
                if affects {
                    work.cal_resolve = Some(CalResolveRequest { key: self.calibration_key.clone(), seq: self.calibration_seq });
                }
            });
            self.sync_calibration_pending(state);
        }
    }

    pub fn save_calibration(&mut self, state: &mut PrintState, owner: Option<CalibrationOwner>, preset: String, filament: String,
        coupon: Arc<fab_calibrate::Coupon>, answer: fab_calibrate::Answer, today: String) {
        self.sync_calibration(state);
        self.queue_calibration_write(state, CalWrite::Save { owner, preset, filament, coupon, answer, today })
    }

    pub fn clear_calibration(&mut self, state: &mut PrintState, step: fab_calibrate::Step) {
        self.sync_calibration(state);
        let (preset, filament) = self.check_setup(state).calibration_names();
        self.queue_calibration_write(state, CalWrite::Clear { owner: self.calibration_key.owner.clone(), preset, filament, step })
    }

    pub fn check_setup(&self, state: &PrintState) -> CheckSetup {
        CheckSetup { dirs: self.dirs.clone(), calibration: self.calibration.clone(), settings: state.settings.clone(), catalog: if state.settings.use_installed { self.catalog.clone() } else { None }, mapping: self.mapping.clone() }
    }

    pub fn check_slice(&mut self, id: u64, state: &PrintState, ev: &Arc<Evaluated>) {
        if let Some(gate) = self.plate_gate(state, ev) {
            let error = match gate {
                PlateGate::Wait(why) => why.into(),
                PlateGate::MachineInvalid(err) => err.redacted_message(),
                PlateGate::CalibrationFailed(_) => self.active_calibration_failure(state)
                    .map(|f| f.redacted_message.clone()).unwrap_or_else(|| self.default_mismatch_error(state, Some(ev))),
            };
            Cx::post_action(PrintEvent::Checked { id, report: ev.printability.clone(), error: Some(error) });
            return;
        }
        let source = match self.tests.as_ref().map(|tests| piece::test_plan(&ev.plate, &ev.machine, tests).map(|plan| (tests.clone(), plan))) {
            None => SliceSource::Model(ev.clone(), Arc::from([]), None),
            Some(Ok((tests, plan))) => SliceSource::Tests(ev.clone(), tests, Arc::new(plan)),
            Some(Err(e)) => {
                Cx::post_action(PrintEvent::Checked { id, report: ev.printability.clone(), error: Some(e) });
                return;
            }
        };
        SliceJob { tray_colours: reported_tool_colours(state), gen: 0, check: Some(id), setup: self.check_setup(state), source, name: "check".into() }.send();
    }

    /// The app shuts down: the printer link closes, so a relaunch under `makepad run` does not
    /// wait for it (slice jobs run on the shared worker, which ends with the process).
    pub fn stop(&mut self) {
        self.fleet.clear();
        self.pending_twins.clear();
        self.profiles_worker = None;
        self.job_writer = None;
    }

    /// Start the catalogue and job-state workers.
    pub fn start(&mut self, state: &mut PrintState) {
        // The issue markers wait to be asked for.
        state.hide_markers = true;
        state.settled = true;
        state.settings = defaults_settings();
        self.load_base_values(state);
        state.profiles.note = "Reading the installed slicer profiles…".into();
        // A simulated printer (a file of status reports) stands in for the configured one.
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(path) = fab_printer_link::sim::sim_path() {
            // The UI harness drives a separate process. Read its counters on request in a
            // simulator test home, where each comparison spans the actual link workers.
            if std::env::var_os("MAKEPAD_TEST_HOME").is_some() {
                if std::env::var_os("FAB_TEST_HOLD_PACKAGE").is_some() {
                    self.test_packages = Some(Vec::new());
                    makepad_widgets::makepad_platform::remote::register_app_command(
                        "printer-release-packages", "", "release the simulator test's package completion", |_| "",
                        |cx, _, _| { cx.action(PrintEvent::ReleaseTestPackages); vec!["released".into()] },
                    );
                }
                makepad_widgets::makepad_platform::remote::register_app_query(
                    "printer-entry-counts", "", "printer boundary counts for the simulator test", |_| "",
                    |_, _, _| {
                        let counts = fab_printer_link::entry_counts();
                        vec![format!(r#"{{"counts":"{counts:?}","start":{},"mqtt":{}}}"#, counts.start, counts.mqtt_connect)]
                    },
                );
            }
            self.simulator = Some(path);
        }
        if self.job_writer.is_none() {
            let (writer, wakes) = JobWriter::channel();
            let started = writer.spawn(self.dirs.jobs.clone(), wakes, |id, e| log!("fab jobs: {id} could not be saved: {e}"));
            if let Err(e) = started { log!("fab jobs: could not start the job writer: {e}"); }
            self.job_writer = Some(writer);
        }
        let (wake, rx) = std::sync::mpsc::sync_channel(1);
        let pending = Arc::new(std::sync::Mutex::new(ProfileWork::default()));
        self.profiles_worker = Some(ProfilesWorker { wake, pending: pending.clone() });
        let path = virtual_path();
        let dirs = self.dirs.clone();
        let started = std::thread::Builder::new().name("fab-profiles".into()).spawn(move || {
            let names = fab_printer_link::secrets::config_names();
            Cx::post_action(PrintEvent::PrinterConfigs {
                names: names.clone(), presets: virtual_printer::presets(),
                loadouts: read_loadouts(&dirs.app_data),
            });
            let data_dirs = ProfileStore::default_data_dirs();
            let mut saved = read_printer_profiles(path.as_deref(), &names, |printers| {
                Cx::post_action(PrintEvent::JobsRead(jobs::read_all(&dirs.jobs)));
                catalog_in(&data_dirs, &dirs.profiles, printers).map(Arc::new)
            }, Cx::post_action);
            let mut layout_revision = 0;
            while rx.recv().is_ok() {
                let work = std::mem::take(&mut *pending.lock().unwrap_or_else(|e| e.into_inner()));
                if let Some(latest) = work.saved {
                    saved = latest;
                    if let Some(path) = &path {
                        if let Err(e) = virtual_printer::save_to(path, &saved) { log!("fab: could not save virtual printers: {e}"); }
                    }
                }
                write_loadouts(&dirs.app_data, work.loadouts);
                if let Some(revision) = work.rebuild {
                    layout_revision = revision;
                    let catalog = catalog_in(&data_dirs, &dirs.profiles, &saved.printers).map(|mut cat| {
                        cat.layout_revision = revision;
                        Arc::new(cat)
                    });
                    Cx::post_action(PrintEvent::Catalog(catalog));
                }
                for (path, select) in work.imports {
                    import_profiles(&path, select, &data_dirs, &dirs.profiles, &saved.printers, layout_revision);
                }
                run_calibration_work(&dirs.calibration, work.cal_writes, work.cal_resolve, Cx::post_action);
            }
        });
        if let Err(e) = started {
            self.profiles_worker = None;
            self.sync_calibration_pending(state);
            log!("fab: could not start the profiles worker: {e}");
        }
    }

    /// The plate changed: the slice (if any) is now stale (its colours map again in
    /// `refresh_printer`).
    pub fn plate_changed(&mut self, state: &mut PrintState, ev: &Evaluated) {
        // The slice shown (or in flight) was made from this very plate and these settings: an
        // evaluation that lands after it, as a setting's second commit does, changes nothing it
        // depends on, and it does not go stale (in Normal nothing would slice it again).
        let plate = crate::live_slice::plate_id(ev);
        let settings = self.check_setup(state).id_for(&ev.plate);
        let whole = (plate, settings);
        if self.figures_of != Some(whole) {
            state.whole_figures = None;
        }
        if self.kept.as_ref().is_some_and(|k| k.plate != plate) {
            self.kept = None;
        }
        // (a test plate's identity follows its cubes as they stand on this plate)
        let key = match self.tests.as_deref() {
            Some(tests) => piece::test_plan(&ev.plate, &ev.machine, tests).map(|plan| (piece::test_plate_id(plate, tests, &plan), settings)).ok(),
            None => Some(whole),
        };
        let same = key.is_some() && self.live_key == key;
        if !same {
            let waiting_card = state.calibration_plate_pending && state.confirm_print.is_some();
            self.invalidate(state);
            if waiting_card { self.reask.get_or_insert(false); }
        }
        state.printability = ev.printability.clone();
        if self.project_settings != ev.plate.settings {
            self.project_settings = ev.plate.settings.clone();
            self.resolve_layer_height(state);
        }
        if ev.plate.parts.is_empty() {
            // The last printable placement was switched off: no old beads or printable job remain.
            self.stop_slice(state);
            self.last = None;
            state.preview = None;
            state.last_settled = None;
            state.unprinted.clear();
            state.compare = None;
            state.showing_b = false;
            state.live_contours = None;
            state.stale = false;
            state.streaming = false;
            state.settled = false;
            state.print_check = None;
            state.status = SliceStatus::Idle;
            state.revision += 1;
        } else if state.preview.is_some() && !same {
            state.stale = true;
        }
        if let Err(e) = &self.machine {
            state.status = SliceStatus::Failed(e.local_message());
        }
    }

    /// A finished background step. Return which part of the app should handle the change.
    pub fn event(&mut self, state: &mut PrintState, ev: &PrintEvent) -> PrintOutcome {
        let changed = match ev {
            PrintEvent::CalibrationWritten { .. } => return PrintOutcome::Unchanged,
            PrintEvent::CalibrationResolved { seq, effective } => {
                if *seq != self.calibration_seq { return PrintOutcome::Unchanged; }
                let old = self.check_setup(state);
                self.calibration = effective.clone();
                self.calibration_resolved = *seq;
                self.sync_calibration_pending(state);
                let new = self.check_setup(state);
                let slice_changed = old.id() != new.id();
                let measured = new.mech_measured();
                let setup_changed = slice_changed || old.mech_measured() != measured;
                if self.profile_rebuild_error.as_ref().is_some_and(|f| f.measured != measured) {
                    self.clear_calibration_error(state);
                }
                if slice_changed {
                    if self.slice_gen.is_some() || self.live.is_some() { crate::live_slice::cancel(); }
                    self.invalidate(state);
                } else if matches!(&state.status, SliceStatus::Running { stage, .. } if stage == LOADING_CALIBRATION) {
                    state.status = SliceStatus::Idle;
                }
                return PrintOutcome::Calibration { setup_changed, slice_changed };
            }
            PrintEvent::Checked { .. } => false,
            PrintEvent::PrinterModesUnreadable(note) => {
                state.links.note.clone_from(note);
                true
            }
            PrintEvent::PrinterConfigs { names, presets, loadouts } => {
                let mut names = names.clone();
                if self.simulator.is_some() && !names.iter().any(|n| n == SIMULATED_PRINTER) {
                    names.insert(0, SIMULATED_PRINTER.into());
                }
                state.set_configured_printers(&names);
                self.rebuild_virtual_choices(state);
                state.links.presets = presets.iter().map(|p| (p.name.clone(), p.title.clone())).collect();
                self.factory = presets.clone();
                for (file, text) in loadouts {
                    self.loadouts.entry(file.clone()).or_insert_with(|| text.clone());
                }
                self.select_loadout(state);
                true
            }
            PrintEvent::VirtualPrinters { saved, catalog, kept_as } => {
                self.virtuals = Some(Arc::new(saved.clone()));
                if catalog.is_some() { self.set_catalog(state, catalog.clone()); }
                self.rebuild_virtual_choices(state);
                let restore_pick = self.linked(&state.links.picked).is_none();
                if kept_as.is_none() {
                    let configured: Vec<_> = state.links.choices.iter().filter(|(id, _)| matches!(id, PrinterChoice::Real(_)))
                        .map(|(id, _)| id.clone()).collect();
                    for id in configured { let _ = self.ensure_link(state, &id); }
                }
                // Installing a worker never selects it or applies its machine to the plate.
                let mut target_changed = false;
                if restore_pick {
                    let pick = if kept_as.is_some() {
                        PrinterChoice::None
                    } else if let Ok(name) = std::env::var("FAB_PRINTER_VIRTUAL") {
                        PrinterChoice::Virtual(name)
                    } else if let Some(value) = &saved.picked {
                        if let Some(name) = value.strip_prefix("virtual:") { PrinterChoice::Virtual(name.into()) }
                        else if let Some(name) = value.strip_prefix("printer:").filter(|n| *n != "No printer") { PrinterChoice::Real(name.into()) }
                        else { PrinterChoice::None }
                    } else {
                        state.links.choices.iter().find(|(p, _)| matches!(p, PrinterChoice::Real(_))).map(|(p, _)| p.clone()).unwrap_or_default()
                    };
                    let previous = (state.links.picked.clone(), state.links.disconnected);
                    self.link_printer(state, &pick);
                    target_changed = previous.0 != state.links.picked || previous.1 != state.links.disconnected;
                    if target_changed { self.invalidate(state); }
                }
                if let Some(name) = kept_as {
                    state.links.note = format!("Fab could not read its saved printer settings, so it kept them as {name} and started fresh. No printer is linked, and your printers start in Virtual mode until you switch them to Connected.");
                }
                if target_changed { return PrintOutcome::RestoredTarget; }
                true
            }
            PrintEvent::TargetModelChanged => {
                // A pick can precede this notification. Apply only the current target's setup.
                if !Self::target_needs_bound(state) { return PrintOutcome::Unchanged; }
                self.select_bound(state);
                self.load_base_values(state);
                self.invalidate(state);
                return PrintOutcome::RestoredTarget;
            }
            PrintEvent::Catalog(c) => {
                if c.as_ref().is_some_and(|c| c.layout_revision != self.layout_revision) { return PrintOutcome::Unchanged; }
                self.invalidate(state);
                self.set_catalog(state, c.clone());
                self.twin_choices(state);
                match c {
                    Some(cat) => {
                        state.profiles.available = true;
                        state.profiles.note = String::new();
                        let virtual_selected = self.select_virtual(state);
                        if let Some(sel) = cat.selection.as_ref().filter(|_| !virtual_selected) {
                            state.settings.use_installed = true;
                            state.settings.machine = sel.machine.clone();
                            state.settings.plate = sel.plate.clone();
                            self.plate_picked = !sel.plate.is_empty();
                            state.settings.process = sel.process.clone();
                        } else if !virtual_selected && cat.installed {
                            state.profiles.note = "The installed profiles select no printer: pick one, or slice with Fab defaults.".into();
                        } else if !virtual_selected {
                            state.profiles.note = "No installed slicer profiles found: Fab defaults. Fab's built-in printers are under Installed profiles.".into();
                        }
                        if !virtual_selected { self.select_bound(state); }
                        // with only Fab's own presets, Fab defaults keep their lists
                        if cat.installed || state.settings.use_installed {
                            self.refresh_lists(state);
                        }
                    }
                    None => {
                        state.profiles.available = false;
                        state.profiles.note = "No installed slicer profiles found: Fab defaults.".into();
                    }
                }
                self.load_base_values(state);
                true
            }
            PrintEvent::ProfilesImported { summary, select, catalog } => {
                self.invalidate(state);
                if catalog.as_ref().is_some_and(|c| c.layout_revision == self.layout_revision) {
                    self.set_catalog(state, catalog.clone());
                    self.twin_choices(state);
                    state.profiles.available = true;
                }
                if let Some((machine, process, _)) = select.as_ref().filter(|_| self.linked_virtual(state).is_none()) {
                    state.settings.use_installed = true;
                    if !machine.is_empty() {
                        state.settings.machine = machine.clone();
                    }
                    if !process.is_empty() {
                        state.settings.process = process.clone();
                    }
                }
                self.select_bound(state);
                self.load_base_values(state);
                self.refresh_lists(state);
                state.profiles.note = summary.clone();
                true
            }
            PrintEvent::Progress { gen, progress, stage } => {
                if self.slice_gen != Some(*gen) || !matches!(state.status, SliceStatus::Running { .. }) {
                    return PrintOutcome::Unchanged;
                }
                // The board's job says what the print panel says.
                if let Some(job) = &self.slice_job {
                    job.stage(stage.clone(), None, Some(*progress as f32));
                }
                state.status = SliceStatus::Running { progress: *progress, stage: stage.clone() };
                true
            }
            PrintEvent::Running { revision, progress, stage } => {
                // the live slice's own, while no settle runs
                let live = self.slice_gen.is_none() && self.live.as_ref().and_then(|l| crate::live_slice::input_revision(&l.input)) == Some(*revision);
                live && self.running_at(state, *revision, *progress, stage)
            }
            PrintEvent::Trees(trees) => {
                state.trees = trees.clone();
                state.trees_given += 1;
                state.bump();
                true
            }
            PrintEvent::Streamed { gen, preview } => {
                if self.slice_gen != Some(*gen) || !matches!(state.status, SliceStatus::Running { .. }) {
                    return PrintOutcome::Unchanged;
                }
                state.stream_preview(preview.clone());
                true
            }
            PrintEvent::Sliced { gen, result, seconds } => {
                if self.slice_gen != Some(*gen) {
                    log!("fab slice: the answer for gen {gen} is not awaited (awaiting {:?}, status {:?})", self.slice_gen, state.status);
                    return PrintOutcome::Unchanged;
                }
                log!("fab slice: gen {gen} answered ({})", if result.is_ok() { "sliced" } else { "failed" });
                self.started = None;
                self.slice_gen = None;
                self.issues_gen = None;
                let job = self.slice_job.take();
                self.clear_export(state);
                match result {
                    Ok(s) => {
                        if let Some(job) = job {
                            job.done();
                        }
                        state.status = SliceStatus::Done { seconds: *seconds };
                        // The colours on job tools the slice prints nothing in (paint covers them all).
                        let printed = |name: &str| state.colors.tool_of(name).is_none_or(|(t, _)| s.settled.tools.contains(&(t as usize - 1)));
                        state.unprinted = state.colors.rows.iter().filter(|r| r.used && !printed(&r.name)).map(|r| r.name.clone()).collect();
                        state.set_preview(s.preview.clone());
                        state.print_check = Some(s.checked.as_ref().map(|c| format!("passed: first layer {:.2} mm, {} moves checked", c.facts.first_layer_z, c.facts.moves)).map_err(Clone::clone));
                        self.last = Some(s.clone());
                        self.issues_gen = Some(*gen);
                        if std::mem::take(&mut self.export_waits) {
                            self.export(state, None);
                        }
                    }
                    Err(e) => {
                        self.export_waits = false;
                        state.streamed = None;
                        state.settled = false;
                        state.stale = state.preview.is_some();
                        match job {
                            // The person stopped it: nothing failed.
                            Some(job) if job.is_cancelled() => {
                                job.cancelled();
                                state.status = SliceStatus::Idle;
                            }
                            job => {
                                if let Some(job) = job {
                                    self.retries.insert(job.id(), Retry::Slice);
                                    job.fail(e.clone(), true);
                                }
                                state.status = SliceStatus::Failed(e.clone());
                            }
                        }
                    }
                }
                true
            }
            PrintEvent::Issues { gen, report, done, total } => {
                if self.issues_gen != Some(*gen) || state.preview.is_none() || state.stale {
                    return PrintOutcome::Unchanged;
                }
                state.streamed = Some(fab::print::StreamedIssues::new(report.clone(), *done, *total));
                true
            }
            PrintEvent::Estimated { revision, preview, changes } => {
                let Some(live) = self.live.as_mut().filter(|l| l.revision == *revision && l.whole) else { return PrintOutcome::Unchanged };
                live.timed = true;
                state.time_preview(preview.clone());
                // Other heads for the colours, weighed in the background when any could move.
                let (choices, room) = self.head_choices.clone();
                if let Some(changes) = changes.clone().filter(|_| choices.iter().any(|c| c.len() > 1)) {
                    let (revision, tools) = (*revision, state.colors.tools.clone());
                    crate::live_slice::run_background("fab heads", move || {
                        let choice = choose_heads(&changes, &choices, &room);
                        Cx::post_action(PrintEvent::HeadsChosen { revision, tools, choice });
                    });
                }
                true
            }
            PrintEvent::Growing { revision, preview, taken } => {
                taken.store(true, Ordering::Release);
                // Only while the plate's whole preview has not come.
                if !self.live.as_ref().is_some_and(|l| l.revision == *revision && l.whole && !l.timed) {
                    return PrintOutcome::Unchanged;
                }
                state.grow_preview(preview.clone());
                true
            }
            PrintEvent::Totals { revision, totals } => {
                // Only while the plate's playing preview has not come: it brings its own.
                if !self.live.as_ref().is_some_and(|l| l.revision == *revision && l.whole && !l.timed) {
                    return PrintOutcome::Unchanged;
                }
                state.estimate = Some(*totals);
                state.bump();
                true
            }
            PrintEvent::HeadsChosen { revision, tools, choice } => {
                // Only for the plate still shown, as it was mapped then.
                self.live.as_ref().is_some_and(|l| l.revision == *revision) && take_heads(state, tools, choice)
            }
            #[cfg(test)]
            PrintEvent::JobFilesRemoved(_) => false,
            PrintEvent::ReleaseTestPackages => {
                for (id, result) in self.test_packages.take().into_iter().flatten() {
                    Cx::post_action(PrintEvent::Packaged { id, result });
                }
                false
            }
            PrintEvent::Packaged { id, result } => {
                let Some(at) = self.packaging.iter().position(|p| p.id == *id) else { return PrintOutcome::Unchanged };
                if let Some(held) = self.test_packages.as_mut().filter(|_| matches!(self.packaging[at].to, PackageFor::Printer(_))) {
                    held.push((*id, result.clone()));
                    return PrintOutcome::Unchanged;
                }
                let p = self.packaging.remove(at);
                if matches!(p.to, PackageFor::File(_)) {
                    if self.export_attempt != Some(*id) { return PrintOutcome::Unchanged; }
                    self.export_attempt = None;
                }
                // Cancelled by the person: said when it was pressed.
                if let PackageFor::Printer(send) = &p.to {
                    if p.stop.load(Ordering::Relaxed) || state.take_sending(&send.meta.id).is_none() {
                        if matches!(result, Packaged::Checked(_)) { remove_job_files(self.dirs.jobs.clone(), send.meta.id.clone()); }
                        return PrintOutcome::Unchanged;
                    }
                    if self.dispatch_entry(state, &send.ticket).is_err() {
                        state.print_blocked.set(&send.ticket.printer, Some(STALE_DISPATCH.into()));
                        if matches!(result, Packaged::Checked(_)) {
                            if let Some(i) = state.follow.jobs.position(&send.meta.id) {
                                state.follow.jobs.at_mut(i).state = JobState::Queued;
                                state.follow.changed(&send.meta.id);
                            }
                        } else {
                            state.follow.jobs.remove(&send.meta.id);
                        }
                        return PrintOutcome::completion(state, &send.ticket.printer);
                    }
                }
                match (result, &p.to) {
                    (Packaged::Written(path), PackageFor::File(_)) => state.export = ExportStatus::Written(path.clone()),
                    (Packaged::Failed(why), PackageFor::File(_)) => state.export = ExportStatus::Failed(why.clone()),
                    (Packaged::Stopped, PackageFor::File(_)) => state.export = ExportStatus::Idle,
                    (Packaged::Checked(_), PackageFor::Printer(send)) if send.queue => {
                        log!("fab print: {} queued", p.sliced.name);
                        if let Some(mut queued) = state.follow.job_of_mut(&send.meta.id) { queued.state = JobState::Queued; }
                        state.print_blocked.set(&send.ticket.printer, None);
                    }
                    (Packaged::Checked(job), PackageFor::Printer(send)) => match self.send_job(state, &send.ticket, job.clone(), &send.meta, send.options) {
                        Ok(()) => {
                            log!("fab print: {} handed to the printer link", p.sliced.name);
                            state.print_blocked.set(&send.ticket.printer, None);
                        }
                        Err(why) => {
                            log!("fab print: the package is made, the print is refused: {why}");
                            state.print_blocked.set(&send.ticket.printer, Some(why));
                            state.follow.jobs.remove(&send.meta.id);
                            remove_job_files(self.dirs.jobs.clone(), send.meta.id.clone());
                        }
                    },
                    (Packaged::Failed(why), PackageFor::Printer(send)) => {
                        log!("fab print: packaging failed: {why}");
                        state.print_blocked.set(&send.ticket.printer, Some(why.clone()));
                        state.follow.jobs.remove(&send.meta.id);
                    }
                    (Packaged::Stopped, PackageFor::Printer(send)) => {
                        log!("fab print: packaging stopped");
                        state.print_blocked.set(&send.ticket.printer, Some("Stopped while it was being prepared: nothing went to the printer.".into()));
                        state.follow.jobs.remove(&send.meta.id);
                    }
                    _ => {}
                }
                if let PackageFor::Printer(send) = &p.to {
                    return PrintOutcome::completion(state, &send.ticket.printer);
                }
                fab::print::follow::printer_changed(state, Cx::time_now());
                true
            }
            PrintEvent::JobsRead(saved) => {
                let (now, wall) = (Cx::time_now(), jobs::wall_now());
                state.follow.jobs_read = true;
                let read = saved.iter().filter(|(meta, _)| state.follow.jobs.position(&meta.id).is_none())
                    .map(|(meta, saved)| CommittedJob::restored(meta.clone(), saved.clone(), now, wall)).collect();
                state.follow.jobs.merge(read);
                let read: Vec<String> = saved.iter().map(|(meta, s)| format!("{} {}{}", meta.id, s.state.label(), s.started.map_or(String::new(), |t| format!(" {:.0} s ago", wall - t)))).collect();
                log!("fab jobs: {} read: {}", saved.len(), read.join(", "));
                for entry in &self.fleet {
                    if &entry.id != state.shown_choice() {
                        fab::print::follow::recheck_report(state, &entry.id, now);
                    }
                }
                fab::print::follow::printer_changed(state, now);
                true
            }
            PrintEvent::JobPreview { id, result } => {
                let Some(mut job) = state.follow.job_of_mut(id) else { return PrintOutcome::Unchanged };
                job.loading = false;
                match result {
                    Ok(preview) => job.preview = Some(preview.clone()),
                    Err(why) => {
                        log!("fab jobs: {id} has no preview: {why}");
                        job.unreadable = Some(format!("Fab's copy of this job can't be read: {why}."));
                    }
                }
                drop(job);
                fab::print::follow::printer_changed(state, Cx::time_now());
                return PrintOutcome::JobPreview;
            }
            PrintEvent::JobChecked { id, attempt, ticket, options, result } => {
                let Some(at) = self.starting.iter().position(|(waiting, admitted)| waiting == id && admitted == attempt) else {
                    return PrintOutcome::Unchanged;
                };
                self.starting.remove(at);
                state.take_sending(id);
                let Some(meta) = state.follow.job_of(id).map(|j| j.meta.clone()) else { return PrintOutcome::Unchanged };
                let sent = match result {
                    Ok(job) => self.send_job(state, ticket, job.clone(), &meta, *options),
                    Err(why) => Err(self.dispatch_entry(state, ticket).err().unwrap_or(why).to_owned()),
                };
                match sent {
                    Ok(()) => log!("fab print: queued job {} handed to the printer link", meta.name),
                    Err(why) => {
                        log!("fab print: queued job {} refused: {why}", meta.name);
                        state.print_blocked.set(&ticket.printer, Some(why));
                        if let Some(mut job) = state.follow.job_of_mut(id) { job.state = JobState::Queued; }
                    }
                }
                return PrintOutcome::completion(state, &ticket.printer);
            }
        };
        if changed { PrintOutcome::Target } else { PrintOutcome::Unchanged }
    }

    /// Queue dirty job states and keep previews for the jobs being printed or shown.
    pub fn sync_jobs(&mut self, state: &mut PrintState) {
        if let Some(writer) = self.job_writer.as_ref().filter(|_| !state.follow.unsaved.is_empty()) {
            let (now, wall) = (Cx::time_now(), jobs::wall_now());
            let unsaved = std::mem::take(&mut state.follow.unsaved);
            let saves = unsaved.into_iter().filter_map(|id| state.follow.job_of(&id).filter(|j| j.state != JobState::Preparing).map(|j| (id, j.saved(now, wall)))).collect();
            writer.save(saves);
        }
        let f = &mut state.follow;
        let printing = f.job.as_ref().and_then(|j| j.id.as_deref()).and_then(|id| f.jobs.position(id));
        // The shown job keeps its preview in every workspace; only Monitor asks for one to be made.
        let shown = f.shown().and_then(|j| f.jobs.position(&j.meta.id));
        let make_shown = shown.filter(|i| f.monitor && Some(*i) != printing);
        // Clearing a preview may swap the last previewed id into its slot. Visit that last id first.
        for slot in (0..f.jobs.previewed().len()).rev() {
            let i = f.jobs.position(&f.jobs.previewed()[slot]).unwrap();
            if Some(i) != printing && Some(i) != shown && f.jobs[i].preview.is_some() {
                f.jobs.at_mut(i).preview = None;
            }
        }
        for i in printing.into_iter().chain(make_shown) {
            let mut job = f.jobs.at_mut(i);
            if job.preview.is_none() && !job.loading && job.unreadable.is_none() && job.state != JobState::Preparing {
                job.loading = true;
                let (id, meta, dir) = (job.meta.id.clone(), job.meta.clone(), self.dirs.jobs.clone());
                crate::live_slice::run_shown("fab job preview", move || {
                    let began = std::time::Instant::now();
                    let result = jobs::read_package(&dir, &id).map_err(|e| e.to_string()).and_then(|package| preview_of_package(&package, &meta));
                    if let Ok(pv) = &result {
                        log!("fab jobs: {id} preview from its package in {:.0} ms: {} layers (the job says {}), {} beads, {:.0} s (planned {:.0} s)", began.elapsed().as_secs_f64() * 1000.0, pv.max_layer() + 1, meta.layers, pv.beads.count(), pv.total, meta.planned);
                    }
                    Cx::post_action(PrintEvent::JobPreview { id, result });
                });
            }
        }
    }

    fn dispatch_entry(&self, state: &PrintState, ticket: &PrintTicket) -> Result<&Linked, &'static str> {
        self.linked(&ticket.printer).filter(|entry| entry.virtual_mode == ticket.virtual_printer
            && entry.epoch == ticket.epoch && state.printers.epoch_of(&ticket.printer) == Some(ticket.epoch))
            .ok_or(STALE_DISPATCH)
    }

    /// Hand the admitted job to the incarnation whose report the person confirmed.
    fn send_job(&self, state: &mut PrintState, ticket: &PrintTicket, job: fab_printer_link::safety::ValidatedJob, meta: &JobMeta, options: fab_printer_link::PrintOptions) -> Result<(), String> {
        let entry = self.dispatch_entry(state, ticket)?;
        let snapshot = state.printer_of(&ticket.printer).ok_or(STALE_DISPATCH)?;
        if !state.job_destination_matches_for(&ticket.printer, meta, snapshot) { return Err(WRONG_DESTINATION.into()); }
        if state.printers.handed(&ticket.printer).is_some() { return Err("A job is already on its way to the printer.".into()); }
        printer_can_start(snapshot, &options)?;
        trays_fit(snapshot, job.facts(), &meta.trays, &meta.used, &options)?;
        let link_send = snapshot.job_sends + 1;
        log!("fab print: {} goes to the printer link as its send {link_send}", meta.id);
        entry.link.send(fab_printer_link::Command::Print(fab_printer_link::PrintJob { job, trays: meta.trays.clone(), used: meta.used.clone(), options }));
        state.printers.hand_to(&ticket.printer, meta.id.clone(), link_send);
        if let Some(i) = state.follow.jobs.position(&meta.id) {
            let mut sent = state.follow.jobs.at_mut(i);
            sent.state = JobState::Sent;
            sent.restored = false;
            sent.link_send = link_send;
            drop(sent);
            state.follow.changed(&meta.id);
        }
        Ok(())
    }

    /// A person pressed Start on a queued job: its package is read back and checked against the
    /// installed profile on a worker, then it goes.
    fn start_job(&mut self, state: &mut PrintState, id: &str, ticket: &PrintTicket) -> Result<(), String> {
        if !fab_printer_link::printing_supported() { return Err(fab_printer_link::UNSUPPORTED_PRINTING.into()); }
        self.dispatch_entry(state, ticket)?;
        if state.sending_for(&ticket.printer).is_some() { return Err("A job is being prepared for the printer.".into()); }
        let meta = state.follow.job_of(id).filter(|j| j.state == JobState::Queued).map(|j| j.meta.clone()).ok_or("This job isn't queued any more.")?;
        let options = fab_printer_link::PrintOptions { flow_calibration: meta.flow_calibration, timelapse: meta.timelapse, plate_clear: true };
        let snapshot = state.printer_of(&ticket.printer).ok_or("The printer has not reported yet.")?;
        if !state.job_destination_matches_for(&ticket.printer, &meta, snapshot) { return Err(WRONG_DESTINATION.into()); }
        if state.printers.handed(&ticket.printer).is_some() { return Err("A job is already on its way to the printer.".into()); }
        printer_can_start(snapshot, &options)?;
        if !same_printer(&snapshot.model, &meta.printer_model) {
            return Err(format!("This job is for a {}, the linked printer is a {}.", meta.printer_model, snapshot.model.trim()));
        }
        let catalog = self.catalog.clone().ok_or("Fab is still reading the installed profiles: press Start again in a moment.")?;
        log!("fab print: queued job {} started", meta.name);
        state.sending.push(Sending { job: id.to_owned(), printer: ticket.printer.clone() });
        let ticket = ticket.clone();
        let attempt = self.next_attempt();
        self.starting.push((id.to_string(), attempt));
        let now = Cx::time_now();
        if let Some(mut job) = state.follow.job_of_mut(id) {
            job.state = JobState::Preparing;
            job.sent_at = now;
        }
        if ticket.printer == state.links.picked {
            state.follow.picked = None;
            state.follow.closed = false;
            state.follow.on = true;
        }
        state.print_blocked.set(&ticket.printer, None);
        let id = id.to_string();
        let dirs = self.dirs.clone();
        crate::live_slice::run_shown("fab job check", move || {
            let result = job_check(&catalog, &dirs.jobs, &id, &meta, &dirs.head_envelope);
            Cx::post_action(PrintEvent::JobChecked { id, attempt, ticket, options, result });
        });
        Ok(())
    }

    fn configured_title(saved: &Saved, config: &str) -> String {
        if saved.disconnected.contains(config) { format!("{config} (Virtual)") } else { config.to_string() }
    }

    fn rebuild_virtual_choices(&self, state: &mut PrintState) {
        let Some(saved) = &self.virtuals else { return };
        let choices = &mut state.links.choices;
        choices.retain(|(choice, _)| matches!(choice, PrinterChoice::Real(_)));
        for (choice, title) in choices.iter_mut() {
            if let PrinterChoice::Real(config) = choice {
                *title = Self::configured_title(saved, config);
            }
        }
        choices.extend(saved.printers.iter().filter(|p| p.configured_for.is_none() && available_virtual(p, self.catalog.as_deref()))
            .map(|p| (PrinterChoice::Virtual(p.name.clone()), p.picker_title())));
        choices.push((PrinterChoice::None, "No printer".into()));
    }

    fn twin_choices(&mut self, state: &mut PrintState) {
        state.links.twin_presets = self.catalog.iter().flat_map(|c| {
            c.entries.iter().filter(|e| e.kind == Kind::Machine && c.twins.contains_key(&e.name)).map(|e| e.name.clone())
        }).collect();
        // An import can replace a machine under its existing name.
        if let Some(saved) = self.virtuals.as_mut().filter(|s| s.printers.iter().any(|p| p.source != VirtualSource::Generated)) {
            if restore_twins(Arc::make_mut(saved), self.catalog.as_deref()) {
                log!("fab: skipped a virtual twin without a supported filament machine preset");
            }
        }
        // Previously picked twins keep running in the background while their preset exists.
        let mut missing = Vec::new();
        for entry in self.fleet.iter().filter(|e| e.virtual_mode) {
            let record = self.virtuals.as_ref().and_then(|s| Self::virtual_record(s, &entry.id));
            if let Some(twin) = record.filter(|p| available_virtual(p, self.catalog.as_deref())) {
                if twin.source != VirtualSource::Generated {
                    #[cfg(not(target_arch = "wasm32"))]
                    if let Some(setup) = setup_of(twin, self.catalog.as_deref()) { entry.link.set_virtual(setup); }
                }
            } else { missing.push(entry.id.clone()); }
        }
        for id in missing {
            self.retire_link(state, &id, false);
            if id != state.links.picked { self.pending_twins.push(id); }
        }
        for id in std::mem::take(&mut self.pending_twins) {
            // The picked twin is restored below with its settings. A catalogue arrival can
            // restore only a virtual worker, even if the user switched modes in the meantime.
            if id == state.links.picked || !self.virtual_mode(&id) || self.linked(&id).is_some() { continue; }
            if let Ok(link) = self.construct_link(&id) { self.install(state, &id, link, true); }
            else { self.pending_twins.push(id); }
        }
        self.rebuild_virtual_choices(state);
        if let PrinterChoice::Real(_) = &state.links.picked {
            if !self.virtual_mode(&state.links.picked) { return; }
            if !self.linked(&state.links.picked).is_some_and(|entry| entry.virtual_mode) || self.linked_virtual(state).is_none() {
                self.link_printer(state, &state.links.picked.clone());
                return;
            }
        } else if state.links.picked == PrinterChoice::None { return; }
        if self.linked_virtual(state).is_none() { self.link_printer(state, &PrinterChoice::None); }
    }

    fn make_twin(&mut self, state: &mut PrintState, source: &PrinterChoice, preset: &str) -> Option<PrinterChoice> {
        let template = self.catalog.as_ref()?.twins.get(preset)?;
        let saved = self.virtuals.as_mut()?;
        let mut twin = template.printer.clone();
        if let VirtualSource::Twin { source: origin, .. } = &mut twin.source { *origin = source.id(); }
        if let Some(existing) = saved.printers.iter().find(|p| p.configured_for.is_none() && p.source == twin.source) {
            return Some(PrinterChoice::Virtual(existing.name.clone()));
        }
        twin.name = twin_name(saved);
        let source_title = match source {
            PrinterChoice::Real(config) => Some(Self::configured_title(saved, config)),
            PrinterChoice::Virtual(_) => Self::virtual_record(saved, source).map(VirtualPrinter::picker_title),
            PrinterChoice::None => None,
        };
        if let Some(title) = source_title { twin.title = format!("{preset} (virtual of {title})"); }
        while saved.printers.iter().any(|p| p.configured_for.is_none() && p.title == twin.title) {
            twin.title = format!("{} ({})", twin.title, twin.name);
        }
        if *source != PrinterChoice::None {
            if let Some(text) = self.loadouts.get(&loadout_file(source)) { seed_retained(&mut twin, text); }
        }
        let pick = PrinterChoice::Virtual(twin.name.clone());
        Arc::make_mut(saved).printers.push(twin);
        self.rebuild_virtual_choices(state);
        Some(pick)
    }

    /// The saved printer the card controls.
    fn linked_virtual(&self, state: &PrintState) -> Option<&VirtualPrinter> {
        self.virtual_for(&state.links.picked)
    }

    fn virtual_for(&self, choice: &PrinterChoice) -> Option<&VirtualPrinter> {
        let saved = self.virtuals.as_ref()?;
        if !self.linked(choice).is_some_and(|entry| entry.virtual_mode) { return None; }
        Self::virtual_record(saved, choice).filter(|p| available_virtual(p, self.catalog.as_deref()))
    }

    fn virtual_record<'a>(saved: &'a Saved, id: &PrinterChoice) -> Option<&'a VirtualPrinter> {
        match id {
            PrinterChoice::Virtual(name) => saved.printers.iter().find(|p| p.configured_for.is_none() && &p.name == name),
            PrinterChoice::Real(config) => saved.twin_for(config),
            PrinterChoice::None => None,
        }
    }

    /// Use the machine and process that describe the linked layout. Leave a picked plate alone.
    fn select_virtual(&self, state: &mut PrintState) -> bool {
        let Some(vp) = self.linked_virtual(state) else { return false };
        state.settings.machine = vp.machine_preset_name();
        state.settings.use_installed = true;
        if vp.source == VirtualSource::Generated {
            state.settings.process = virtual_printer::PROCESS.into();
        } else {
            self.select_process(state);
        }
        true
    }

    fn target_needs_bound(state: &PrintState) -> bool {
        state.settings.use_installed && matches!(state.links.picked, PrinterChoice::Real(_)) && !state.links.disconnected
            && state.printers.binding(&state.links.picked).and_then(|b| b.machine()).is_some_and(|p| p.geometry.name != state.settings.machine)
    }

    fn select_bound(&self, state: &mut PrintState) -> bool {
        if !state.settings.use_installed { return false; }
        let Some(machine) = state.printers.binding(&state.links.picked).and_then(|b| b.machine()) else { return false };
        state.settings.machine.clone_from(&machine.geometry.name);
        self.select_process(state);
        true
    }

    fn select_process(&self, state: &mut PrintState) {
        self.refresh_lists(state);
        let fits = &state.profiles.processes;
        if !fits.contains(&state.settings.process) {
            state.settings.process = self.catalog.as_ref().and_then(|c| c.selection.as_ref())
                .map(|s| &s.process).filter(|p| fits.contains(*p))
                .or_else(|| fits.iter().find(|p| p.contains("0.20"))).or(fits.first()).cloned().unwrap_or_default();
        }
    }

    fn switch_mode(&mut self, state: &mut PrintState, config: &str, disconnected: bool) -> Result<bool, &'static str> {
        let pick = PrinterChoice::Real(config.into());
        if state.links.picked != pick { return Err("The selected printer changed."); }
        let saved = self.virtuals.as_deref().ok_or("Fab has not read the saved printer modes yet.")?;
        if saved.disconnected.contains(config) == disconnected { return Ok(false); }
        // Sending covers active packages and queued rechecks. A cancelled package may still be unwinding.
        if state.sending_for(&pick).is_some() || self.packaging.iter().any(|p| matches!(&p.to, PackageFor::Printer(s) if s.ticket.printer == pick)) {
            return Err("A job is being prepared for this printer. Wait before switching modes.");
        }
        if !virtual_printer::valid_config(config) { return Err("Invalid printer choice."); }
        if disconnected {
            if let Some(twin) = saved.twin_for(config) {
                if !available_virtual(twin, self.catalog.as_deref()) { return Err("The saved twin's machine preset is unavailable."); }
            } else {
                let preset = state.printers.binding(&pick).and_then(|b| b.machine()).map_or(&state.settings.machine, |p| &p.geometry.name);
                let template = self.catalog.as_ref().and_then(|c| c.twins.get(preset))
                    .ok_or("Select a filament machine preset before switching to Virtual.")?;
                let mut twin = template.printer.clone();
                twin.configured_for = Some(config.into());
                if let VirtualSource::Twin { source, .. } = &mut twin.source { *source = pick.id(); }
                twin.name = twin_name(saved);
                let layout = colours::layout(state.printer_of(&pick).map(|p| (p.name.as_str(), &*p.state)), None, &self.offline(), &|_| None);
                if layout.live { seed_layout(&mut twin, &layout); }
                else if let Some(text) = self.loadouts.get(&loadout_file(&pick)) { seed_retained(&mut twin, text); }
                Arc::make_mut(self.virtuals.as_mut().unwrap()).printers.push(twin);
            }
        }
        let saved = Arc::make_mut(self.virtuals.as_mut().unwrap());
        if disconnected { saved.disconnected.insert(config.into()); } else { saved.disconnected.remove(config); }
        saved.picked = pick.id();
        self.rebuild_virtual_choices(state);
        let linked = self.link_printer(state, &pick);
        self.save_virtual(false);
        Ok(linked)
    }

    fn virtual_mode(&self, choice: &PrinterChoice) -> bool {
        match choice {
            PrinterChoice::Virtual(_) => true,
            PrinterChoice::Real(name) => self.virtuals.as_ref().is_some_and(|s| s.disconnected.contains(name)),
            PrinterChoice::None => false,
        }
    }

    fn construct_link(&self, choice: &PrinterChoice) -> Result<fab_printer_link::PrinterLink, &'static str> {
        let saved = self.virtuals.as_ref().ok_or("Fab has not read the saved printer modes yet.")?;
        if let PrinterChoice::Real(name) = choice {
            if !virtual_printer::valid_config(name) { return Err("Invalid printer choice."); }
            if !saved.disconnected.contains(name) { return Ok(self.real_link(name)); }
        }
        let record = Self::virtual_record(saved, choice).ok_or("The virtual printer has no saved layout.")?;
        let setup = setup_of(record, self.catalog.as_deref()).ok_or("The virtual printer's filament machine preset is unavailable.")?;
        #[cfg(not(target_arch = "wasm32"))]
        { Ok(fab_printer_link::PrinterLink::virtual_printer(setup)) }
        #[cfg(target_arch = "wasm32")]
        { let _ = setup; Err("Virtual printing is unavailable on this platform.") }
    }

    fn real_link(&self, name: &str) -> fab_printer_link::PrinterLink {
        #[cfg(test)]
        if let Some(start) = &self.real_constructor { return start(name); }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(path) = self.simulator.as_ref().filter(|_| name == SIMULATED_PRINTER) {
            return fab_printer_link::PrinterLink::simulate(path.clone());
        }
        fab_printer_link::PrinterLink::start(name)
    }

    fn linked(&self, id: &PrinterChoice) -> Option<&Linked> {
        self.fleet.iter().find(|entry| &entry.id == id)
    }

    fn control_entry(&self, dest: &ControlDestination) -> Option<&Linked> {
        let ControlDestination::Filament { choice, virtual_printer, epoch, .. } = dest else { return None };
        self.linked(choice).filter(|entry| entry.virtual_mode == *virtual_printer && entry.epoch == *epoch)
    }

    fn install(&mut self, state: &mut PrintState, id: &PrinterChoice, link: fab_printer_link::PrinterLink, virtual_mode: bool) {
        self.fleet.retain(|entry| &entry.id != id);
        self.installed_epoch += 1;
        state.printers.install(id, self.installed_epoch, virtual_mode);
        let entry = Linked { id: id.clone(), link, virtual_mode, epoch: self.installed_epoch, watched: None };
        entry.refresh_binding(state, self.catalog.as_deref(), self.virtuals.as_deref());
        self.fleet.push(entry);
    }

    fn retire_link(&mut self, state: &mut PrintState, id: &PrinterChoice, to_connected: bool) {
        let Some(i) = self.fleet.iter().position(|entry| &entry.id == id) else { return };
        let stopped = self.fleet.remove(i).virtual_mode.then_some(if to_connected {
            "The virtual print stopped when the printer switched back to Connected."
        } else {
            "The virtual print stopped when its printer was unlinked."
        });
        fab::print::follow::unlink_printer(state, id, stopped, Cx::time_now());
        if id == state.shown_choice() { self.update_camera(state); }
    }

    /// Install only this printer, in its saved mode, without changing the target.
    fn ensure_link(&mut self, state: &mut PrintState, id: &PrinterChoice) -> Result<(), &'static str> {
        let virtual_mode = self.virtual_mode(id);
        if *id == PrinterChoice::None || self.linked(id).is_some_and(|e| e.virtual_mode == virtual_mode) { return Ok(()); }
        match self.construct_link(id) {
            Ok(link) => {
                self.retire_link(state, id, !virtual_mode);
                self.install(state, id, link, virtual_mode);
                self.pending_twins.retain(|pending| pending != id);
                Ok(())
            }
            Err(why) => {
                if virtual_mode && matches!(id, PrinterChoice::Real(_)) {
                    self.retire_link(state, id, false);
                    if !self.pending_twins.contains(id) { self.pending_twins.push(id.clone()); }
                }
                Err(why)
            }
        }
    }

    fn update_camera(&self, state: &mut PrintState) {
        let shown = state.shown_choice().clone();
        let held_real = self.linked(&shown).is_some_and(|e| !e.virtual_mode);
        state.camera.update_source(&shown, held_real, state.printers.get(&shown).map(Arc::as_ref));
    }

    /// Select a target. Its worker and the old target's worker can both survive the pick.
    fn link_printer(&mut self, state: &mut PrintState, choice: &PrinterChoice) -> bool {
        let previous = state.links.picked.clone();
        let was_virtual = matches!(previous, PrinterChoice::Virtual(_));
        let virtual_mode = self.virtual_mode(choice);
        let unavailable = self.ensure_link(state, choice).err();
        if let Some(why) = unavailable {
            if !(virtual_mode && matches!(choice, PrinterChoice::Real(_))) {
                state.links.note = why.into();
                state.bump();
                return false;
            }
        }
        state.print_options.plate_clear = false;
        state.confirm_print = None;
        state.confirm_stop = None;
        state.follow.confirm_start = None;
        state.head_moves.clear();
        self.pending_send = None;
        state.links.picked = choice.clone();
        if &previous != choice {
            state.shown = None;
            if let Some(snapshot) = state.printer_of(&previous) {
                if let Some(entry) = self.fleet.iter_mut().find(|e| e.id == previous) {
                    entry.bind_heads(state, &snapshot.model);
                    entry.keep_background_loadout(state, snapshot, &mut self.loadouts, self.profiles_worker.as_ref());
                }
            }
        }
        self.select_loadout(state);
        state.links.title = state.links.choices.iter().find(|(p, _)| p == choice).map(|(_, title)| title.clone()).unwrap_or_default();
        state.links.disconnected = virtual_mode && matches!(choice, PrinterChoice::Real(_));
        state.links.note = unavailable.unwrap_or_default().into();
        state.links.layout = self.linked_virtual(state).filter(|p| p.source == VirtualSource::Generated).map(virtual_controls);
        self.update_camera(state);
        fab::print::follow::printer_changed(state, Cx::time_now());
        if unavailable.is_some() { state.bump(); return false; }
        if self.select_virtual(state) {
            // A job without mapped parts still needs a filament for its base values.
            let preset = if self.linked_virtual(state).is_some_and(|p| p.source == VirtualSource::Generated) {
                virtual_printer::filament_preset("PLA")
            } else {
                state.profiles.filaments.first().cloned().unwrap_or_default()
            };
            state.settings.materials = vec![JobMaterial { preset, color: [1.0; 4], nozzle: 0, tray: None, dedicated: false }];
            self.refresh_lists(state);
            self.load_base_values(state);
        } else if was_virtual {
            state.settings = defaults_settings();
            self.plate_picked = false;
            if let Some(catalog) = self.catalog.clone() {
                self.event(state, &PrintEvent::Catalog(Some(catalog)));
            } else { self.load_base_values(state); }
        } else if self.select_bound(state) {
            self.load_base_values(state);
        }
        self.tell_printer_heads(state);
        true
    }

    /// Keep writes in edit order, including a catalogue rebuild after changing feed paths.
    fn save_virtual(&self, rebuild: bool) {
        if let (Some(worker), Some(saved)) = (&self.profiles_worker, &self.virtuals) {
            worker.update(|w| {
                if rebuild { w.rebuild = Some(self.layout_revision); }
                w.saved.replace(saved.clone())
            });
        }
    }

    /// Publish a slot or layout edit to the same report path as a physical printer.
    fn update_virtual(&mut self, state: &mut PrintState, choice: &PrinterChoice, vp: VirtualPrinter, rebuild: bool) {
        let Some(saved) = self.virtuals.as_mut() else { return };
        let Some(stored) = Arc::make_mut(saved).printers.iter_mut().find(|p| match choice {
            PrinterChoice::Virtual(name) => p.configured_for.is_none() && &p.name == name,
            PrinterChoice::Real(name) => p.configured_for.as_ref() == Some(name),
            PrinterChoice::None => false,
        }) else { return };
        #[cfg(not(target_arch = "wasm32"))]
        if let (Some(link), Some(setup)) = (self.fleet.iter().find(|entry| &entry.id == choice).map(|entry| &entry.link), setup_of(&vp, self.catalog.as_deref())) { link.set_virtual(setup); }
        if &state.links.picked == choice {
            state.links.layout = (vp.source == VirtualSource::Generated).then(|| virtual_controls(&vp));
            state.links.note.clear();
        }
        *stored = vp;
        if rebuild {
            self.layout_revision += 1;
            self.set_catalog(state, None);
            self.select_virtual(state);
            self.machine = Err(MachineError::UpdatingVirtualLayout);
        }
        self.save_virtual(rebuild);
    }

    /// The settings half of the Print editor's actions (playback is
    /// `fab::print::apply`). Returns true when the view changed.
    pub fn action(&mut self, state: &mut PrintState, action: &PrintAction, plate: Option<&Arc<Evaluated>>) -> bool {
        if matches!(action, PrintAction::AskPrint(false)) || matches!(action, PrintAction::AskStop(dest, true) if dest.matches(state)) {
            self.pending_send = None;
            if self.reask == Some(false) {
                self.reask = None;
            }
        }
        match action {
            PrintAction::ShowPrinter(choice) => {
                state.shown = choice.as_ref().filter(|p| *p != &state.links.picked).cloned();
                self.update_camera(state);
                fab::print::follow::printer_changed(state, Cx::time_now());
                return false;
            }
            PrintAction::Slice => return self.slice(state, plate),
            PrintAction::Export => return self.export(state, plate),
            // A press while the asked slice runs waits for it; anything else slices now.
            PrintAction::AskPrint(true) if self.calibration_waiting(state) => {
                self.reask.get_or_insert(false);
                return true;
            }
            PrintAction::AskPrint(true) if !state.can_send() => {
                if self.slice_gen.is_some() && matches!(state.status, SliceStatus::Running { .. }) {
                    log!("fab print: Print pressed, the slice is on its way");
                    return false;
                }
                log!("fab print: Print pressed, slicing first");
                return self.slice(state, plate);
            }
            PrintAction::AskPrint(true) => {
                log!("fab print: Print pressed, the slice is ready");
                return false;
            }
            PrintAction::StartJob { id, ticket } => {
                let result = if state.follow.confirm_start.as_ref() == Some(id) { self.start_job(state, id, ticket) } else { Err(STALE_DISPATCH.into()) };
                if let Err(why) = result {
                    log!("fab print: queued job refused: {why}");
                    state.print_blocked.set(&ticket.printer, Some(why));
                }
                return false;
            }
            PrintAction::RemoveJob(id) => {
                if state.follow.job_of(id).is_some_and(|j| j.state == JobState::Queued) {
                    state.follow.jobs.remove(id);
                    if state.follow.picked.as_ref() == Some(id) { state.follow.picked = None; }
                    remove_job_files(self.dirs.jobs.clone(), id.clone());
                }
                return false;
            }
            PrintAction::CancelSend(id) => {
                let Some(send) = state.take_sending(id) else { return false };
                for package in &self.packaging {
                    if matches!(&package.to, PackageFor::Printer(s) if s.meta.id == *id) {
                        package.stop.store(true, Ordering::Relaxed);
                    }
                }
                if let Some(at) = self.starting.iter().position(|(job, _)| job == id) {
                    self.starting.remove(at);
                    if let Some(mut job) = state.follow.job_of_mut(id) { job.state = JobState::Queued; }
                } else if state.follow.jobs.get(id).is_some_and(|job| job.state == JobState::Preparing) {
                    state.follow.jobs.remove(id);
                }
                state.print_blocked.set(&send.printer, Some("Cancelled: nothing went to the printer.".into()));
                return false;
            }
            PrintAction::UseInstalled(_) if self.linked_virtual(state).is_some_and(|p| p.source != VirtualSource::Generated) => return false,
            PrintAction::UseInstalled(on) => {
                let on = *on && self.catalog.is_some();
                if on == state.settings.use_installed {
                    return false;
                }
                if on {
                    let sel = self.catalog.as_ref().and_then(|c| c.selection.clone()).unwrap_or_default();
                    state.settings.use_installed = true;
                    state.settings.machine = sel.machine.clone();
                    state.settings.plate = sel.plate.clone();
                    self.plate_picked = !sel.plate.is_empty();
                    state.settings.process = sel.process.clone();
                    self.select_bound(state);
                    self.load_base_values(state);
                    self.refresh_lists(state);
                } else {
                    let materials = std::mem::take(&mut state.settings.materials);
                    state.settings = defaults_settings();
                    state.settings.materials = materials;
                    self.plate_picked = false;
                    self.load_base_values(state);
                    state.profiles.machines = vec![DEFAULT_MACHINE.into()];
                    state.profiles.processes = vec![DEFAULT_PROCESS.into()];
                    state.profiles.filaments = vec![DEFAULT_FILAMENT.into()];
                }
            }
            PrintAction::SetProcess(n) => {
                state.settings.process = n.clone();
                self.load_base_values(state);
            }
            PrintAction::SetPlate(n) => {
                state.settings.plate = n.clone();
                self.plate_picked = true;
                self.load_base_values(state);
            }
            PrintAction::SendToPrinter { ticket, preview } => {
                if !matches!(&state.confirm_print, Some(PrintConfirm::Filament(card)) if card == ticket) || !self.ticket_current(state, ticket) {
                    state.print_blocked.set(&state.links.picked, Some(STALE_TICKET.into()));
                    return false;
                }
                if self.calibration_waiting(state) {
                    if !self.settled_slice_reusable(state) || !state.preview.as_ref().is_some_and(|p| p.id == *preview) {
                        self.pending_send = None;
                        state.confirm_print = None;
                        state.print_options.plate_clear = false;
                        let why = if self.settled_slice_reusable(state) { STALE_PREVIEW } else { SLICE_NOT_READY };
                        state.print_blocked.set(&state.links.picked, Some(why.into()));
                        state.follow.closed = false;
                    } else {
                        self.pending_send = Some(PendingSend { ticket: ticket.clone(), preview: *preview });
                        state.print_blocked.set(&state.links.picked, None);
                    }
                    return false;
                }
                self.pending_send = None;
                if let Some(err) = self.calibration_failure(state, plate) {
                    state.print_blocked.set(&state.links.picked, Some(err));
                    state.follow.closed = false;
                    return false;
                }
                self.admit_send_to_printer(state, ticket, *preview, state.print_options, plate);
                return false;
            }
            PrintAction::SetMachine(owner, _) | PrintAction::EditTray(owner, ..) | PrintAction::VirtualLayout { owner, .. } |
            PrintAction::ResetVirtual(owner, _) | PrintAction::MakeTwin(owner, _) |
            PrintAction::LinkPrinter(owner, _) | PrintAction::PrinterMode(owner, _) if !owner.matches(state) => {
                let why = "The printer changed since this edit was opened. Nothing was changed.";
                if owner.scope == PrinterScope::Target || owner.choice == state.links.picked { state.links.note = why.into(); }
                else { log!("fab: {why}"); }
                state.bump();
                return false;
            }
            PrintAction::SetMachine(owner, preset) => {
                if self.virtual_mode(&owner.choice) && self.virtual_for(&owner.choice).is_none_or(|p| p.source != VirtualSource::Generated) {
                    return false;
                }
                if let PrinterChoice::Real(_) = &owner.choice {
                    let Some(saved) = self.virtuals.as_mut() else { return false };
                    let id = owner.choice.id().unwrap();
                    if preset.as_deref().is_some_and(|n| !virtual_printer::valid_config(n)) { return false; }
                    if saved.bindings.get(&id) == preset.as_ref() { return false; }
                    let saved = Arc::make_mut(saved);
                    match preset {
                        Some(name) => { saved.bindings.insert(id, name.clone()); }
                        None => { saved.bindings.remove(&id); }
                    }
                    self.save_virtual(false);
                    if let Some(entry) = self.fleet.iter_mut().find(|e| e.id == owner.choice) {
                        entry.refresh_binding(state, self.catalog.as_deref(), self.virtuals.as_deref());
                        if let Some(snapshot) = state.printer_of(&owner.choice) {
                            entry.bind_heads(state, &snapshot.model);
                            if owner.choice != state.links.picked {
                                entry.keep_background_loadout(state, snapshot, &mut self.loadouts, self.profiles_worker.as_ref());
                            }
                        }
                    }
                    if owner.choice != state.links.picked || !self.select_bound(state) { return false; }
                } else {
                    if owner.choice != state.links.picked { return false; }
                    let Some(name) = preset else { return false };
                    state.settings.machine.clone_from(name);
                    self.select_process(state);
                }
                self.load_base_values(state);
            }
            PrintAction::PrinterMode(owner, disconnected) => {
                let PrinterChoice::Real(config) = &owner.choice else { return false };
                match self.switch_mode(state, config, *disconnected) {
                    Ok(true) => {}
                    Ok(false) => return false,
                    Err(why) => { state.links.note = why.into(); state.bump(); return false; }
                }
            }
            PrintAction::MakeTwin(owner, preset) => {
                let Some(pick) = self.make_twin(state, &owner.choice, preset) else { return false };
                return self.action(state, &PrintAction::LinkPrinter(owner.clone(), pick), plate);
            }
            PrintAction::LinkPrinter(_, pick) => {
                let wants_link = *pick != PrinterChoice::None;
                let switching = state.links.picked != *pick;
                let was_disconnected = state.links.disconnected;
                let changed = switching || self.linked(&state.links.picked).is_some_and(|entry| entry.virtual_mode) != self.virtual_mode(pick) || self.linked(&state.links.picked).is_some() != wants_link;
                if changed && !self.link_printer(state, pick) {
                    // A configured Virtual printer stays picked even when its preset is missing.
                    if state.links.picked == *pick && (switching || was_disconnected != state.links.disconnected) {
                        self.invalidate(state);
                        return true;
                    }
                    return false;
                }
                // A space is outside the real configuration filename alphabet.
                let picked = pick.id().unwrap_or_else(|| "printer:No printer".into());
                if let Some(saved) = self.virtuals.as_mut().filter(|s| s.picked.as_ref() != Some(&picked)) {
                    Arc::make_mut(saved).picked = Some(picked);
                    self.save_virtual(false);
                }
                if !changed { return false; }
            }
            PrintAction::VirtualLayout { owner, kind, heads, trays, tpu } => {
                let Some(vp) = self.virtual_for(&owner.choice) else { return false };
                match vp.layout(*kind, *heads, *trays, *tpu) {
                    Ok(layout) => self.update_virtual(state, &owner.choice, layout, true),
                    Err(why) => { state.links.note = why; state.bump(); return false; }
                }
            }
            PrintAction::ResetVirtual(owner, name) => {
                let Some(vp) = self.virtual_for(&owner.choice) else { return false };
                if vp.source != VirtualSource::Generated { return false; }
                let Some(preset) = self.factory.iter().find(|p| &p.name == name) else { return false };
                let layout = VirtualPrinter { name: vp.name.clone(), title: vp.title.clone(), ..preset.clone() };
                self.update_virtual(state, &owner.choice, layout, true);
            }
            PrintAction::EditTray(owner, pos, edit) if self.virtual_for(&owner.choice).is_some() => {
                let mut vp = self.virtual_for(&owner.choice).unwrap().clone();
                let label = pos.tray_text();
                let Some(slot) = vp.slots.iter_mut().find(|s| s.head == pos.head && s.tray == label) else { return false };
                match edit {
                    colours::TrayEdit::Material(m) => slot.material = Some(m.clone()),
                    colours::TrayEdit::Colour(c) => {
                        slot.colour = Some(srgb(*c));
                        slot.material.get_or_insert_with(|| "PLA".into());
                    }
                    colours::TrayEdit::Empty => { slot.material = None; slot.colour = None; }
                }
                self.update_virtual(state, &owner.choice, vp, false);
                return false;
            }
            PrintAction::EditTray(owner, pos, edit) => {
                // A reporting printer's trays are its reports.
                if owner.scope != PrinterScope::Target || state.colors.layout.live {
                    return false;
                }
                let mut l = self.loadout.clone().unwrap_or_else(|| colours::layout(None, None, &self.offline(), &|_| None));
                if !colours::edit_tray(&mut l, *pos, edit) {
                    return false;
                }
                self.keep_loadout(&owner.choice, l, None);
            }
            PrintAction::SetVirtualSpeed(dest, speed) => {
                if let Some(entry) = self.control_entry(dest).filter(|entry| entry.virtual_mode && dest.matches(state)) {
                    entry.link.set_virtual_speed(*speed);
                } else {
                    log!("fab: ignored a printer speed whose destination changed");
                }
                return false;
            }
            PrintAction::PrinterControl(dest, control) => {
                if let Some(entry) = self.control_entry(dest) {
                    send_filament_control(state, dest, *control, |command| entry.link.send(command));
                } else {
                    log!("fab: ignored a printer control whose destination changed");
                }
                return false;
            }
            _ => return false,
        }
        self.invalidate(state);
        true
    }

    fn invalidate(&mut self, state: &mut PrintState) {
        if let Some(held) = self.pending_send.take() {
            if held.ticket.printer == state.links.picked && state.confirm_print.is_some() {
                state.print_blocked.set(&state.links.picked, Some(STALE_PREVIEW.into()));
                state.follow.closed = false;
                self.reask.get_or_insert(false);
            }
        }
        // a settled slice still awaited (`started` is set from its start to its answer)
        if self.slice_gen.is_some() && self.started.is_some() {
            self.reask = Some(self.export_waits || self.reask == Some(true));
        }
        self.clear_export(state);
        if self.reask == Some(true) { state.export = ExportStatus::Exporting; }
        self.slice_gen = None;
        self.issues_gen = None;
        self.export_waits = false;
        self.live = None;
        self.live_key = None;
        self.started = None;
        state.settled = false;
        // No answer is awaited any more: the slice's job is over.
        if let Some(job) = self.slice_job.take() {
            job.cancelled();
        }
        if let Some(job) = self.live_job.take() {
            job.cancelled();
        }
        self.running = None;
        state.estimate = None;
        state.growing = None;
        state.stale = state.preview.is_some();
        state.stale_parts = None;
        if matches!(state.status, SliceStatus::Running { .. }) { state.status = SliceStatus::Idle; }
    }

    fn refresh_lists(&self, state: &mut PrintState) {
        self.refresh_plates(state);
        let Some(cat) = &self.catalog else { return };
        let machine = state.settings.machine.clone();
        let names = |kind: Kind, compat: bool| -> Vec<String> {
            cat
                .entries
                .iter()
                .filter(|e| e.kind == kind && (!compat || e.user || e.compat.is_empty() || e.compat.iter().any(|c| *c == machine)))
                .map(|e| e.name.clone())
                .collect()
        };
        state.profiles.machines = names(Kind::Machine, false);
        state.profiles.processes = names(Kind::Process, true);
        let mut fil = names(Kind::Filament, true);
        // The filaments selected in the installed app lead the list.
        if let Some(sel) = &cat.selection {
            for f in sel.filaments.iter().rev() {
                if let Some(i) = fil.iter().position(|x| x == f) {
                    let n = fil.remove(i);
                    fil.insert(0, n);
                }
            }
        }
        state.profiles.filaments = fil;
    }

    /// The plates every job material has a bed temperature for; a plate nobody picked
    /// follows them and the printer ([`default_plate`]).
    fn refresh_plates(&self, state: &mut PrintState) {
        state.profiles.plates = if state.settings.use_installed {
            match &self.catalog {
                Some(cat) => {
                    let filaments: Option<Vec<&Entry>> = state.settings.materials.iter()
                        .map(|m| cat.entries.iter().find(|e| e.kind == Kind::Filament && e.name == m.preset)).collect();
                    PLATES.iter().filter(|(name, _)| filaments.as_ref().is_some_and(|fs| fs.iter().all(|f| f.plates.iter().any(|p| p.as_str() == *name))))
                        .map(|(name, _)| name.to_string()).collect()
                }
                None => Vec::new(),
            }
        } else {
            PLATES.iter().map(|(name, _)| name.to_string()).collect()
        };
        if !self.plate_picked {
            state.settings.plate = default_plate(&state.profiles.plates, &self.printer_plate);
        }
    }

    /// Check the current plate for admission. Dispatch later checks this job's own report and trays.
    fn settled_slice_reusable(&self, state: &PrintState) -> bool {
        state.preview.is_some() && !state.stale && state.settled && !matches!(state.status, SliceStatus::Running { .. })
            && self.last.as_ref().is_some_and(|s| state.preview.as_ref().is_some_and(|p| p.id == s.preview.id))
    }

    fn admit_send_to_printer(&mut self, state: &mut PrintState, ticket: &PrintTicket, preview: u64, options: fab_printer_link::PrintOptions, plate: Option<&Arc<Evaluated>>) {
        // Its package is made now; it goes to the printer when made and checked. A bond
        // whose bends are known not to measure refuses it at once.
        let mechs = plate.map_or(&[][..], |ev| &ev.mech[..]);
        match self.send_checks(state, &options).and_then(|s| {
            if s.preview.id != preview { return Err(STALE_PREVIEW.into()); }
            bends_refusal(mechs, false).map_or_else(|| Ok(s.clone()), Err)
        }) {
            Ok(sliced) => {
                let queue = state.printer_busy();
                log!("fab print: Start print pressed, {} {}", if queue { "queueing" } else { "packaging" }, sliced.name);
                // Monitor shows this very slice from now on, and follows it.
                let meta = Arc::new(job_meta(&sliced, &ticket.printer, state.target_printer().map(Arc::as_ref), &options, state.accepted_errors()));
                state.sending.push(Sending { job: meta.id.clone(), printer: ticket.printer.clone() });
                if !meta.accepted.is_empty() {
                    log!("fab print: printed anyway: {}", jobs::accepted_line(&meta.accepted));
                }
                state.follow.jobs.insert_newest(CommittedJob { meta: meta.clone(), preview: Some(sliced.preview.clone()), sent_at: Cx::time_now(), ..Default::default() });
                state.follow.picked = None;
                state.follow.closed = false;
                state.follow.on = true;
                self.package(sliced, PackageFor::Printer(ToPrinter { ticket: ticket.clone(), options, meta, scripts: self.plate_scripts.clone(), queue }), mechs.to_vec());
                state.print_blocked.set(&state.links.picked, None);
            }
            Err(why) => {
                log!("fab print: Start print refused: {why}");
                state.print_blocked.set(&state.links.picked, Some(why));
                state.follow.closed = false;
            }
        }
    }

    fn send_checks(&self, state: &PrintState, options: &fab_printer_link::PrintOptions) -> Result<&Arc<Sliced>, String> {
        if !fab_printer_link::printing_supported() { return Err(fab_printer_link::UNSUPPORTED_PRINTING.into()); }
        if self.linked(&state.links.picked).is_none() { return Err("No printer is linked.".into()); }
        if self.calibration_waiting(state) { return Err(SLICE_NOT_READY.into()); }
        if let Some(err) = &state.calibration_error { return Err(err.clone()); }
        if let Some(why) = state.print_blocker() { return Err(why); }
        if state.sending_for(&state.links.picked).is_some() { return Err("A job is being prepared for the printer.".into()); }
        let snapshot = state.target_printer().ok_or("The printer has not reported yet.")?;
        // A busy printer takes the job into the queue; what its start asks of the printer waits.
        let busy = state.printer_busy();
        if busy {
            connected(snapshot)?;
        } else {
            printer_can_start(snapshot, options)?;
        }
        let last = self.last.as_ref()
            .filter(|s| state.can_send() && state.preview.as_ref().is_some_and(|p| p.id == s.preview.id))
            .ok_or(SLICE_NOT_READY)?;
        if snapshot.model.trim().is_empty() {
            return Err("The printer's model is not configured".into());
        }
        if !state.settings.use_installed {
            return Err(format!("Slice with your installed {} printer profile: Fab defaults have no start sequence for this printer.", snapshot.model));
        }
        if !same_printer(&snapshot.model, &last.printer_model) {
            return Err(format!("The slice is for a {}, the linked printer is a {}: pick its printer profile and slice again.", last.printer_model, snapshot.model.trim()));
        }
        let checked = last.checked.as_ref().map_err(Clone::clone)?;
        for &i in &last.used {
            if last.trays.get(i).copied().flatten().is_none() {
                return Err(format!("Material {} has no tray: pick one in the Colours list.", i + 1));
            }
        }
        if !busy {
            trays_fit(snapshot, &checked.facts, &last.trays, &last.used, options)?;
        }
        Ok(last)
    }

    /// The plate changed under the open confirm card and only a live slice follows it: the card
    /// needs a settled one. A failed slice waits for the next press or change. True when asked.
    pub fn confirm_slice(&mut self, state: &mut PrintState, plate: Option<&Arc<Evaluated>>) -> bool {
        let coming = self.slice_gen.is_some() && matches!(state.status, SliceStatus::Running { .. });
        if self.calibration_waiting(state) || state.calibration_error.is_some() || state.can_send() || coming || matches!(state.status, SliceStatus::Failed(_)) || plate.is_none_or(|ev| ev.plate.parts.is_empty()) {
            return false;
        }
        log!("fab print: the plate changed under the confirm card, slicing it");
        self.slice(state, plate)
    }

    fn ticket_current(&self, state: &PrintState, ticket: &PrintTicket) -> bool {
        state.resin.is_none() && state.links.picked == ticket.printer && self.linked(&ticket.printer).is_some_and(|entry|
            entry.epoch == ticket.epoch && entry.virtual_mode == ticket.virtual_printer)
    }

    /// What stops Start print now, for the confirm card; the slice still coming is its question,
    /// not a refusal.
    pub fn send_blocked(&self, state: &PrintState) -> Option<String> {
        if matches!(&state.confirm_print, Some(PrintConfirm::Filament(ticket)) if !self.ticket_current(state, ticket)) {
            return Some(STALE_TICKET.into());
        }
        self.send_checks(state, &state.print_options).err().filter(|why| state.can_send() || why != SLICE_NOT_READY)
    }

    fn next_attempt(&mut self) -> u64 {
        self.last_attempt += 1;
        self.last_attempt
    }

    /// Make the printer package of `sliced` on a pool worker, its progress on the job board, for
    /// `to`: checked for the printer (then sent, [`PrintEvent::Packaged`]) or written to a file.
    /// One at a time per destination and slice. For the printer it first waits for the bends of
    /// the plate's flexing pieces (`bends`) and refuses one that can't be measured.
    fn package(&mut self, sliced: Arc<Sliced>, to: PackageFor, bends: Vec<Arc<fab_model::mech::Mech>>) -> u64 {
        if let Some(p) = self.packaging.iter().find(|p| p.to == to && Arc::ptr_eq(&p.sliced, &sliced)
            && !(matches!(to, PackageFor::File(_)) && p.stop.load(Ordering::Relaxed))) {
            return p.id;
        }
        let id = self.next_attempt();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let title = match &to {
            PackageFor::Printer(_) => format!("Packaging {} for the printer", sliced.name),
            PackageFor::File(path) => format!("Exporting {}", path.file_name().map_or(sliced.name.clone(), |n| n.to_string_lossy().to_string())),
        };
        let job = JobSpec::new(title).stage("the printer package").start();
        self.packaging.push(Packaging { id, sliced: sliced.clone(), to: to.clone(), stop: stop.clone() });
        let jobs_dir = self.dirs.jobs.clone();
        crate::live_slice::run_shown("fab package", move || {
            let reporter = job.reporter();
            let stopped = || stop.load(Ordering::Relaxed) || reporter.is_cancelled();
            if !bends.is_empty() {
                reporter.stage("the bonds' bends", None, None);
            }
            let refused = bends_refusal(&bends, true);
            let result = match refused.map_or_else(|| sliced.settled.package_with(&|f| reporter.fraction(Some(0.9 * f as f32)), &stopped), Err) {
                Err(_) if stopped() => Packaged::Stopped,
                Err(e) => Packaged::Failed(e),
                Ok(package) => match to {
                    PackageFor::Printer(send) => {
                        reporter.stage("the print safety check", None, Some(0.9));
                        match &sliced.checked {
                            Ok(c) => match fab_printer_link::safety::validate_job(package, &c.profile, &sliced.name) {
                                // Cancelled while it was checked: it does not go.
                                Ok(_) if stopped() => Packaged::Stopped,
                                // Committed before anything goes to the printer.
                                Ok(job) => {
                                    let saved = SavedState { state: if send.queue { JobState::Queued } else { JobState::Sent }, started: (!send.queue).then_some(send.meta.created), ..Default::default() };
                                    match jobs::commit(&jobs_dir, &send.meta, &saved, job.bytes(), &send.scripts) {
                                        Ok(()) => {
                                            log!("fab jobs: {} committed: {} bytes, {} layers, planned {:.0} s, {}", send.meta.id, job.bytes().len(), send.meta.layers, send.meta.planned, if send.queue { "queued" } else { "goes to the printer now" });
                                            Packaged::Checked(job)
                                        }
                                        Err(e) => Packaged::Failed(format!("The job could not be saved: {e}. Nothing went to the printer.")),
                                    }
                                }
                                Err(e) => Packaged::Failed(e.message()),
                            },
                            Err(e) => Packaged::Failed(e.clone()),
                        }
                    }
                    PackageFor::File(_) if stopped() => Packaged::Stopped,
                    PackageFor::File(path) => match std::fs::write(&path, &package) {
                        Ok(()) => Packaged::Written(path),
                        Err(e) => Packaged::Failed(format!("{}: {e}", path.display())),
                    },
                },
            };
            match &result {
                Packaged::Stopped => job.cancelled(),
                Packaged::Failed(why) => job.fail(why.clone(), false),
                _ => job.done(),
            }
            Cx::post_action(PrintEvent::Packaged { id, result });
        });
        id
    }

    /// On a quiet event only the global publication counter is read. A new entry also has an unread slot.
    pub fn poll_printer(&mut self, state: &mut PrintState) -> Polled {
        let published = fab_printer_link::published();
        let changed = published != self.published;
        let mut polled = Polled::default();
        if !changed && self.polled_epoch == self.installed_epoch { return polled; }
        self.published = published;
        let previous_epoch = std::mem::replace(&mut self.polled_epoch, self.installed_epoch);
        for entry in &mut self.fleet {
            if !changed && entry.epoch <= previous_epoch { continue; }
            let Some(snapshot) = entry.link.poll() else { continue };
            let id = &entry.id;
            let now = Cx::time_now();
            log_printer_change(state.printer_of(id).map(Arc::as_ref), &snapshot);
            if let Some(line) = fab::print::follow::layer_line(state, id, &snapshot, now) {
                log!("fab printer: {line}");
            }
            let target = *id == state.links.picked;
            state.printers.set(id, entry.epoch, snapshot.clone());
            if entry.watched.as_ref().is_none_or(|(model, _)| model != &snapshot.model) {
                entry.refresh_binding(state, self.catalog.as_deref(), self.virtuals.as_deref());
                entry.bind_heads(state, &snapshot.model);
                if target && Self::target_needs_bound(state) { Cx::post_action(PrintEvent::TargetModelChanged); }
            }
            let id = &entry.id;
            fab::print::follow::reported(state, id, now);
            if id == state.shown_choice() {
                state.camera.update_source(id, !entry.virtual_mode, Some(&snapshot));
                polled.shown = true;
            }
            if target {
                polled.target = true;
            } else {
                entry.keep_background_loadout(state, &snapshot, &mut self.loadouts, self.profiles_worker.as_ref());
                polled.others = true;
            }
        }
        polled
    }

    /// A change of the plate's profile can change only its target's subscription.
    fn tell_printer_heads(&mut self, state: &PrintState) {
        let Some(snapshot) = state.target_printer() else { return };
        if let Some(entry) = self.fleet.iter_mut().find(|entry| entry.id == state.links.picked) {
            entry.bind_heads(state, &snapshot.model);
        }
    }

    fn set_catalog(&mut self, state: &mut PrintState, catalog: Option<Arc<Catalog>>) {
        self.catalog = catalog;
        for entry in &mut self.fleet {
            entry.refresh_binding(state, self.catalog.as_deref(), self.virtuals.as_deref());
            if entry.id == state.links.picked { continue; }
            if let Some(snapshot) = state.printer_of(&entry.id) {
                entry.bind_heads(state, &snapshot.model);
                entry.keep_background_loadout(state, snapshot, &mut self.loadouts, self.profiles_worker.as_ref());
            }
        }
    }

    /// Map the print colours onto the printer's trays (the linked printer's reports, else the
    /// last loadout it reported or one set by hand, else the printer profile's slots) and derive the job's
    /// materials, one per position the used colours map to. A reported loadout that differs
    /// from the kept one is kept. Returns the mapping the plate should print and show with, when
    /// it differs from the one it was built with (the model worker applies it).
    /// O(colours * trays).
    pub fn refresh_printer(&mut self, state: &mut PrintState, evaluated: Option<&Arc<Evaluated>>) -> Option<Arc<Mapped>> {
        self.adopt_legacy_loadout(state);
        let installed = state.settings.use_installed.then_some(()).and(self.catalog.as_deref());
        let name_of = |idx: &str| installed.and_then(|c| preset_of_id(c, &state.settings.machine, idx));
        let snapshot = state.target_printer();
        let printer = snapshot.map(|p| (p.name.as_str(), &*p.state));
        let mut layout = colours::layout(printer, self.loadout.as_ref(), &self.offline(), &name_of);
        // A report with a toolhead the picked profile lacks is another printer's: not kept.
        let foreign = colours::drop_missing_heads(&mut layout);
        if layout.live && !foreign && !snapshot.is_some_and(|p| p.virtual_printer) && self.loadout.as_ref().is_none_or(|k| !colours::same_loadout(k, &layout)) {
            self.keep_loadout(&state.links.picked, layout.clone(), snapshot.map(|p| p.model.as_str()));
        }
        let want = self.wanted_material(state);
        let Some(ev) = evaluated else {
            self.mapping = None;
            state.colors = colours::resolve(&[], &[], layout, &want, &[], &[]);
            self.refresh_plates(state);
            return None;
        };
        // The colours printed parts and their paint print in.
        let n = ev.plate.parts.len();
        let mut used: Vec<String> = ev.part_colors.iter().take(n).cloned().collect();
        for p in &ev.plate.parts {
            for c in p.paint.iter().flat_map(|sp| sp.colors.iter()).filter_map(|c| c.color.clone()) {
                if !used.contains(&c) {
                    used.push(c);
                }
            }
        }
        // The nozzles that reach each colour's parts, so a colour on both nozzles prints on one
        // that reaches them: measured once a plate (O(its parts' vertices)), not per report.
        let reach = match &self.reach {
            Some((of, reach)) if std::sync::Weak::ptr_eq(of, &Arc::downgrade(ev)) => reach.clone(),
            _ => {
                let parts: Vec<(fab_plate::Bounds, Vec<String>)> = ev.plate.parts.iter().zip(&ev.part_colors).map(|(p, own)| {
                    let paint = p.paint.iter().flat_map(|sp| sp.colors.iter()).filter_map(|c| c.color.clone());
                    (p.bounds(), std::iter::once(own.clone()).chain(paint).collect())
                }).collect();
                let reach = Arc::new(colours::reaching(&ev.machine, &parts));
                self.reach = Some((Arc::downgrade(ev), reach.clone()));
                reach
            }
        };
        let mapping = colours::resolve(&ev.colors, &used, layout, &want, &state.head_moves, &reach);
        // A move that no longer applies (the colour saved to a tray, its head or tray gone) goes.
        state.head_moves.retain(|m| mapping.row(&m.name).is_some_and(|r| r.moved.is_some()));
        self.head_choices = colours::head_choices(&mapping, &ev.colors);
        let map_of = |m: &[JobMaterial]| m.iter().map(|m| m.nozzle as u32).collect::<Vec<_>>();
        let had_swaps = swaps_on_a_nozzle(&map_of(&state.settings.materials));
        state.settings.materials = mapping.tools.iter().enumerate().map(|(i, pos)| {
            let tray = mapping.layout.tray(*pos);
            let row = mapping.rows.iter().find(|r| r.pos == Some(*pos));
            let colour = tray.and_then(|t| t.colour).unwrap_or_else(|| {
                let [r, g, b] = fab_model::eval::shown_color(&ev.colors, &HashMap::new(), row.map_or("", |r| &r.name));
                [r, g, b, 1.0]
            });
            let material = row.and_then(|r| r.material.as_deref());
            JobMaterial { preset: self.preset_for(state, tray, i, material), color: colour, nozzle: pos.head, tray: Some(pos.tray), dedicated: tray.is_some_and(|t| t.dedicated()) }
        }).collect();
        if swaps_on_a_nozzle(&map_of(&state.settings.materials)) != had_swaps {
            // Only the tower row changed. A tray report uses the already loaded machine.
            self.refresh_panel_values(state);
        }
        self.refresh_plates(state);
        let mapped = mapped_of(&mapping);
        state.colors = mapping;
        if *ev.mapped == mapped {
            self.mapping = Some(ev.mapped.clone());
            None
        } else {
            let mapped = Arc::new(mapped);
            self.mapping = Some(mapped.clone());
            Some(mapped)
        }
    }

    /// The material type a colour asks for: the installed selection's first filament's, else PLA.
    fn wanted_material(&self, state: &PrintState) -> String {
        if self.linked_virtual(state).is_some_and(|p| p.source == VirtualSource::Generated) { return "PLA".into(); }
        let cat = self.catalog.as_deref().filter(|_| state.settings.use_installed);
        cat.and_then(|c| c.selection.as_ref()?.filaments.first().and_then(|n| c.entries.iter().find(|e| e.kind == Kind::Filament && e.name == *n)).and_then(|e| e.material.clone()))
            .unwrap_or_else(|| "PLA".into())
    }

    /// The filament preset of a tray's material: the preset of the material id it reports, else
    /// one of its type (the installed selection's first), else the selection's `i`th or first.
    /// A tray that reports no type takes the type its colour asks for (`material`), and with no
    /// installed profiles that type is Fab's own filament of it.
    fn preset_for(&self, state: &PrintState, tray: Option<&TrayView>, i: usize, material: Option<&str>) -> String {
        let Some(cat) = self.catalog.as_deref().filter(|_| state.settings.use_installed) else { return default_filament(material).into() };
        let machine = &state.settings.machine;
        if self.linked_virtual(state).is_some_and(|vp| vp.source == VirtualSource::Generated && *machine == vp.machine_preset_name()) {
            return virtual_printer::filament_preset(tray.and_then(|t| t.material.as_deref()).or(material).unwrap_or("PLA"));
        }
        let fits = |e: &&Entry| e.kind == Kind::Filament && (e.user || e.compat.is_empty() || e.compat.iter().any(|c| c == machine));
        let selected: Vec<&String> = cat.selection.iter().flat_map(|s| s.filaments.iter()).collect();
        if let Some(name) = tray.and_then(|t| t.name.clone()) {
            return name;
        }
        if let Some(kind) = tray.and_then(|t| t.material.as_deref()).or(material) {
            let of_kind = |e: &&Entry| e.material.as_deref().is_some_and(|m| m.eq_ignore_ascii_case(kind));
            if let Some(n) = selected.iter().find(|n| cat.entries.iter().any(|e| e.name == ***n && of_kind(&e))) {
                return (*n).clone();
            }
            if let Some(e) = cat.entries.iter().filter(fits).find(of_kind) {
                return e.name.clone();
            }
        }
        selected.get(i).or(selected.first()).map(|n| (*n).clone()).unwrap_or_default()
    }

    /// The slicer settings as the chosen profiles give them (what the panel shows where the
    /// project sets nothing), and the global layer height they resolve to.
    fn load_base_values(&mut self, state: &mut PrintState) {
        let was_invalid = self.machine.is_err();
        let profile = state.settings.use_installed.then(|| {
            state.printers.binding(&state.links.picked).and_then(|b| b.machine())
                .filter(|p| p.geometry.name == state.settings.machine).cloned()
                .or_else(|| self.catalog.as_ref()?.twins.get(&state.settings.machine).map(|p| p.machine.clone()))
        }).flatten();
        self.machine = match &profile {
            Some(p) => Ok(p.geometry.clone()),
            None => self.check_setup(state).machine_typed().map(Arc::new),
        };
        self.adopt_legacy_loadout(state);
        // Read again only for other toolheads: the kept text rounds a reported colour.
        let heads = self.offline().heads;
        if self.loadout.as_ref().is_none_or(|l| l.heads != heads) {
            self.loadout = self.loadout_text.as_deref().and_then(|t| colours::parse_loadout(loadout_model(t).1, &heads));
        }
        // An unpicked plate starts as the printer's own (a settings change, not per frame).
        if let Some(p) = profile {
            self.printer_plate.clone_from(&p.default_plate);
            self.machine_model.clone_from(&p.model);
        } else {
            self.printer_plate = self.catalog.as_deref().filter(|_| state.settings.use_installed).and_then(|c| c.printer_plate(&state.settings.machine)).unwrap_or_default();
            self.machine_model = self.catalog.as_deref().filter(|_| state.settings.use_installed).and_then(|c| c.store.resolve(&state.settings.machine).ok()).map(|p| p.values.str("printer_model").to_string()).unwrap_or_default();
        }
        self.tell_printer_heads(state);
        if was_invalid && self.machine.is_ok() { state.status = SliceStatus::Idle; }
        self.refresh_panel_values(state);
    }

    fn refresh_panel_values(&self, state: &mut PrintState) {
        let machine = match self.machine() {
            Ok(machine) => machine,
            Err(e) => {
                state.status = SliceStatus::Failed(e);
                return;
            }
        };
        self.refresh_plates(state);
        let catalog = if state.settings.use_installed { self.catalog.as_deref() } else { None };
        // A resin printer has no bed plate or filament to resolve: its process is the values,
        // with the resin keys of the printer and the process (the job's exposures and peels, what
        // its supports are sized against), the process's over the printer's.
        if machine.resin_screen().is_some() {
            if let Some(process) = catalog.and_then(|c| c.store.resolve(&state.settings.process).ok()) {
                let mut values: std::collections::HashMap<String, String> = fab_plate::PRINT_SETTINGS.iter().map(|s| s.key).map(|k| (k.to_string(), process.values.raw(k, 0).to_string())).collect();
                let printer = catalog.and_then(|c| c.store.resolve(&state.settings.machine).ok());
                for cfg in printer.iter().map(|p| &p.values).chain([&process.values]) {
                    for k in cfg.map.keys().filter(|k| k.starts_with("fab_sla_")) {
                        let v = cfg.raw(k, 0).trim();
                        if !v.is_empty() && v != "nil" {
                            values.insert(k.clone(), v.to_string());
                        }
                    }
                }
                state.base_values = values;
                state.base_revision += 1;
            }
        } else if let Ok(setup) = resolve_tools_with(&state.settings, catalog, None, &Plate::default(), machine, None) {
            let cfg = setup.build_config();
            state.base_values = fab_plate::PRINT_SETTINGS.iter().map(|s| s.key).chain(["nozzle_diameter"]).map(|k| (k.to_string(), cfg.raw(k, 0).to_string())).collect();
            state.base_revision += 1;
        }
        self.resolve_layer_height(state);
    }

    /// The global layer height: the project's, else the profile's.
    fn resolve_layer_height(&self, state: &mut PrintState) {
        let own = self.project_settings.iter().find(|(k, _)| k == "layer_height").map(|(_, v)| v.as_str());
        let v = own.or_else(|| state.base_values.get("layer_height").map(String::as_str)).and_then(fab_plate::setting_number);
        state.settings.layer_height = v.filter(|h| *h > 0.0).unwrap_or(0.2);
    }

    /// What the job strip's Retry of a failed print job runs again.
    pub fn retry(&mut self, job: JobId) -> Option<Retry> {
        self.retries.remove(&job)
    }

    /// Import the printer, filament and process profiles a file holds (a
    /// preset file, a preset bundle, a project) into Fab's profile folder in
    /// the background; with `select`, the job switches to them.
    pub fn import_profiles(&mut self, path: std::path::PathBuf, select: bool) {
        if let Some(worker) = &self.profiles_worker {
            worker.update(|w| w.imports.push((path, select)));
        }
    }

    /// What slices from now on: the ticked test prints, or none: the whole plate. True when it
    /// changed: the slice shown is not of it, unless the person switched back to the one they left.
    pub fn set_tests(&mut self, state: &mut PrintState, plate: Option<&Arc<Evaluated>>, tests: Option<Vec<piece::TestPrint>>) -> bool {
        if self.tests.as_deref() == tests.as_deref() {
            return false;
        }
        if self.tests.is_some() == tests.is_some() {
            self.tests = tests.map(Arc::from);
            self.invalidate(state);
            return true;
        }
        let left = self.standing_slice(state, plate);
        let last = self.last.take();
        self.tests = tests.map(Arc::from);
        self.stop_slice(state);
        let back = std::mem::replace(&mut self.kept, left.map(|left| KeptSlice { last, ..left }));
        // Only the input that would slice now, settings and all, may show what is kept.
        if let (Some(back), Some((job, input))) = (back, self.next_input(state, plate)) {
            if back.key == (input.plate_id, input.settings_id) {
                self.bring_back(state, job, input, back);
            }
        }
        true
    }

    /// What would slice now, as it would.
    fn next_input(&self, state: &PrintState, plate: Option<&Arc<Evaluated>>) -> Option<(crate::live_slice::PrintJob, crate::live_slice::SliceInput)> {
        if self.plate_gate(state, plate?).is_some() { return None; }
        let (source, name) = self.source(plate, Arc::from([]), None).ok()?;
        Some(self.job(state, source, name).prepare())
    }

    /// The slice standing now: one a person can look at and keep working from.
    fn standing_slice(&self, state: &PrintState, plate: Option<&Arc<Evaluated>>) -> Option<KeptSlice> {
        if !state.settled && !self.live.as_ref().is_some_and(|l| l.timed) {
            return None;
        }
        let (_, input) = self.next_input(state, plate)?;
        let key = (input.plate_id, input.settings_id);
        Some(KeptSlice { plate: crate::live_slice::plate_id(plate?), key, shown: state.shown_slice()?, toolpaths: crate::live_slice::toolpaths_of(key)?, last: None })
    }

    /// A slice left earlier shows again, and the engine holds its input without slicing it.
    fn bring_back(&mut self, state: &mut PrintState, job: crate::live_slice::PrintJob, input: crate::live_slice::SliceInput, kept: KeptSlice) {
        let revision = crate::live_slice::hold_input(&input, &job, kept.toolpaths);
        log!("fab test print: {} shows again with no slice", job.name);
        self.live_key = Some(kept.key);
        self.last = kept.last;
        if !kept.shown.settled {
            self.live = Some(LiveJob { job, input, revision, sent: std::time::Instant::now(), whole: true, timed: true, took: None });
        }
        state.show_slice(kept.shown, revision);
    }

    /// The plate and settings of a live edit, kept until Print, Export or Slice asks to settle.
    pub fn live_input(&mut self, state: &mut PrintState, plate: Option<&Arc<Evaluated>>, swaps: Arc<[crate::live_slice::FieldSwap]>, moved: Option<(usize, crate::live_slice::PartTransform)>) {
        if self.calibration_waiting(state) { return; }
        if let Some(reason) = self.calibration_failure(state, plate) {
            self.fail_for_calibration(state, reason, false);
            return;
        }
        if self.machine.is_err() { return; }
        if self.wait_for_plate(state, plate) {
            return;
        }
        state.fit_warning = self.fit_warning(plate);
        let (source, name) = match self.source(plate, swaps, moved) {
            Ok(s) => s,
            Err(e) => {
                self.stop_slice(state);
                state.status = e.map_or(SliceStatus::Idle, SliceStatus::Failed);
                return;
            }
        };
        let (job, input) = self.job(state, source, name).prepare();
        let key = (input.plate_id, input.settings_id);
        if self.live_key == Some(key) { return; }
        if self.tests.is_none() {
            self.figures_of = Some(key);
        }
        mark_test_slice("fab.test_print.slice.start", &job, key.0);
        // A newer input keeps the slice's job on the board.
        let live_job = self.live_job.take();
        self.invalidate(state);
        self.live_job = live_job;
        self.live_key = Some(key);
        state.streamed = None;
        state.status = SliceStatus::Running { progress: 0.0, stage: "Slicing the view".into() };
        let sent = std::time::Instant::now();
        let revision = crate::live_slice::set_input(&input, &job, sent);
        self.live = Some(LiveJob { job, input, revision, sent, whole: false, timed: false, took: None });
        self.live_progress(state, 0.0, false);
    }

    /// How far the live slice at the engine's input `revision` is (`progress` of all its stages,
    /// `stage` its words): its status while it runs, its supports a stage of the same bar. The
    /// chip shows it by the whole percent, the jobs board's entry with the words. True when the
    /// status moved.
    fn running_at(&mut self, state: &mut PrintState, revision: u64, progress: f32, stage: &str) -> bool {
        self.running = Some((revision, progress, stage.to_string()));
        if let Some(job) = &self.live_job {
            job.stage(stage.to_string(), None, Some(progress));
        }
        if !matches!(state.status, SliceStatus::Running { .. }) {
            return false;
        }
        let status = Self::live_running(self.running.as_ref(), revision, progress);
        let moved = state.status != status;
        state.status = status;
        moved
    }

    /// The live slice's status while it runs at the engine's input `revision`, `layers` of its
    /// layers cut: as far as the engine tells it is (`running`), by the whole percent, its stage
    /// the supports' while they plan.
    fn live_running(running: Option<&(u64, f32, String)>, revision: u64, layers: f32) -> SliceStatus {
        match running {
            Some((r, f, stage)) if *r == revision => {
                let stage = if stage.starts_with("Supports") { PLANNING_SUPPORTS } else { "Slicing the view" };
                SliceStatus::Running { progress: (f * 100.0).round() / 100.0, stage: stage.into() }
            }
            _ => SliceStatus::Running { progress: layers, stage: "Slicing the view".into() },
        }
    }

    /// The live slice's progress on the jobs board while no chip shows it. In the Slice view the
    /// chip does, and a settled slice's own job (Print, Export, Slice) does: one place at a time.
    fn live_progress(&mut self, state: &PrintState, progress: f32, complete: bool) {
        if complete || state.play.active || self.slice_job.is_some() {
            if let Some(job) = self.live_job.take() {
                job.done();
            }
            return;
        }
        // As far as the engine tells this revision is, with its stage's words; its layers' share
        // until it tells.
        let revision = self.live.as_ref().and_then(|l| crate::live_slice::input_revision(&l.input));
        let (progress, words) = match &self.running {
            Some((r, f, words)) if Some(*r) == revision => (*f, words.clone()),
            _ => (progress, "Slicing the view".to_string()),
        };
        if let Some(job) = &self.live_job {
            job.stage(words, None, Some(progress));
            return;
        }
        let Some(name) = self.live.as_ref().map(|l| l.job.name.as_str()) else { return };
        let job = JobSpec::new(format!("Slicing {name}")).stage("Slicing the view").not_cancellable().start();
        job.fraction(Some(progress));
        self.live_job = Some(job);
    }

    /// Take newer tiles and progress of this input without requesting a settle. True when this
    /// revision just became whole: its print time is estimated once edits rest ([`Self::live_whole`]).
    pub fn live_updated(&mut self, state: &mut PrintState, result: &crate::live_slice::LiveResult) -> bool {
        let Some(live) = self.live.as_mut() else { return false };
        let Some(revision) = crate::live_slice::input_revision(&live.input) else { return false };
        if result.revision != revision {
            // A running drag snapshot owns its viewed layer through publication.
            // Newer pending input must not hide that completed layer.
            if crate::live_slice::drag_live() && result.revision < revision {
                if let Some(preview) = result.preview.as_ref().filter(|p| state.preview.as_ref().is_none_or(|shown| shown.id < p.id)) {
                    state.stream_preview(preview.clone());
                }
            }
            return false;
        }
        if live.revision != revision {
            live.revision = revision;
            live.whole = false;
            live.timed = false;
            live.took = None;
            state.estimate = None;
            state.growing = None;
        }
        // The planner's preview stays shown once it came: newer tiles bring only the checks' issues.
        if let Some(preview) = result.preview.as_ref().filter(|p| !live.timed && state.preview.as_ref().is_none_or(|shown| shown.id < p.id)) {
            state.stream_preview(preview.clone());
        }
        if let (Some(report), Some(markers)) = (&result.report, &result.markers) {
            state.live_issues(report, markers);
        }
        let progress = if result.layers_total == 0 { 0.0 } else { result.layers_done as f32 / result.layers_total as f32 };
        // A whole live slice is done (once): shown as sliced; Print and Export settle it when asked.
        if !result.complete {
            state.status = Self::live_running(self.running.as_ref(), revision, progress);
        } else if !live.whole {
            state.status = SliceStatus::Done { seconds: *live.took.get_or_insert_with(|| live.sent.elapsed().as_secs_f64()) };
        }
        let whole = result.complete && !live.whole;
        if whole {
            mark_test_slice("fab.test_print.slice.end", &live.job, revision);
        }
        live.whole |= result.complete;
        self.live_progress(state, progress, result.complete);
        whole
    }

    /// The newest live input's toolpaths are whole where no view takes its tiles (the Mechanics
    /// check waits for them): it shows as sliced and its job leaves the board. Its print time is
    /// estimated once a view shows it ([`Self::live_updated`]). True when the status changed.
    pub fn live_toolpaths(&mut self, state: &mut PrintState, revision: u64) -> bool {
        let Some(live) = self.live.as_mut() else { return false };
        if crate::live_slice::input_revision(&live.input) != Some(revision) {
            return false;
        }
        if live.revision != revision {
            live.revision = revision;
            live.whole = false;
            live.timed = false;
            live.took = None;
            state.estimate = None;
            state.growing = None;
        }
        if live.took.is_some() {
            return false;
        }
        state.status = SliceStatus::Done { seconds: *live.took.insert(live.sent.elapsed().as_secs_f64()) };
        self.live_progress(state, 1.0, true);
        true
    }

    /// The newest live input's slice is whole and still the engine's newest revision.
    pub fn live_whole(&self) -> bool {
        self.live.as_ref().is_some_and(|l| l.whole && crate::live_slice::input_revision(&l.input) == Some(l.revision))
    }

    /// A slice the engine answered but whose answer this editor has not taken (an action lost
    /// on the way): taken now. True when the view changed.
    pub fn reconcile(&mut self, state: &mut PrintState) -> bool {
        if !matches!(state.status, SliceStatus::Running { .. }) {
            return false;
        }
        match crate::live_slice::latest_sliced() {
            Some((gen, result, seconds)) if self.slice_gen == Some(gen) => {
                log!("fab slice: gen {gen}'s answer never reached the editor: taken from the engine");
                self.event(state, &PrintEvent::Sliced { gen, result, seconds }) == PrintOutcome::Target
            }
            _ => false,
        }
    }

    pub fn live_failed(&mut self, state: &mut PrintState, revision: u64, error: &str) {
        if let Some(live) = self.live.as_ref().filter(|l| l.revision == revision) {
            mark_test_slice("fab.test_print.slice.failed", &live.job, revision);
            let job = self.live_job.take();
            // (invalidate stops the supports job: the slice's failure says why, not its card)
            self.invalidate(state);
            if let Some(job) = job {
                job.fail(error, false);
            }
            state.status = SliceStatus::Failed(error.into());
        }
    }

    /// What to slice, or why not: `Err(None)` when there is nothing to slice and that is no
    /// failure (the last part deleted, every part excluded), `Err(Some(why))` otherwise.
    fn source(&self, plate: Option<&Arc<Evaluated>>, swaps: Arc<[crate::live_slice::FieldSwap]>, moved: Option<(usize, crate::live_slice::PartTransform)>) -> Result<(SliceSource, String), Option<String>> {
        self.machine().map_err(Some)?;
        let ev = plate.cloned().ok_or_else(|| Some("there is no model on the plate yet".to_string()))?;
        if ev.plate.parts.is_empty() {
            return Err(ev.plate.excluded.is_empty().then(|| "there is nothing on the plate to slice".to_string()));
        }
        if let Some(tests) = &self.tests {
            if tests.is_empty() {
                return Err(Some("no test print is ticked: tick one, or choose Whole plate".to_string()));
            }
            let plan = piece::test_plan(&ev.plate, &ev.machine, tests).map_err(Some)?;
            let names: Vec<&str> = tests.iter().map(|t| t.name.as_str()).collect();
            return Ok((SliceSource::Tests(ev, tests.clone(), Arc::new(plan)), format!("test prints {}", names.join(", "))));
        }
        // A part off the bed or out of a nozzle's reach slices all the same: the bed check is a
        // warning (`fit_warning`), and Print's safety check of the G-code is what refuses it.
        // The job (its file) is named after the first part's instance: `vase` for `vase/vase`
        // (a file name has no `/`).
        let name = ev.plate.parts.first().map(|p| p.object.split('/').next().unwrap_or(&p.object).to_string()).unwrap_or_else(|| "plate".into());
        Ok((SliceSource::Model(ev, swaps, moved), name))
    }

    /// The bed check of the whole plate, said in the model's terms: each part off the selected
    /// printer's bed or out of a nozzle's reach, sliced anyway (none while test prints slice:
    /// they are cut to fit).
    fn fit_warning(&self, plate: Option<&Arc<Evaluated>>) -> Option<String> {
        let ev = plate.filter(|_| self.tests.is_none())?;
        let parts: Vec<String> = ev.reports.iter().filter(|r| r.blocking()).filter_map(|r| {
            let part = ev.plate.parts.get(r.part)?;
            let issues: Vec<String> = r.issues.iter().filter(|i| i.is_blocking()).map(|i| i.describe(&ev.machine)).collect();
            Some(format!("{}: {} — sliced anyway; it won't fit {}'s bed", part.name, issues.join(", "), ev.machine.name))
        }).collect();
        (!parts.is_empty()).then(|| parts.join("; "))
    }

    fn job(&self, state: &PrintState, source: SliceSource, name: String) -> SliceJob {
        SliceJob { tray_colours: reported_tool_colours(state), gen: self.gen, check: None, setup: self.check_setup(state), source, name }
    }

    fn slice(&mut self, state: &mut PrintState, plate: Option<&Arc<Evaluated>>) -> bool {
        if self.calibration_waiting(state) {
            self.reask.get_or_insert(false);
            return true;
        }
        if let Some(reason) = self.calibration_failure(state, plate) {
            self.fail_for_calibration(state, reason, false);
            return true;
        }
        self.reask = None;
        if self.wait_for_plate(state, plate) {
            // The rebuilt or fully grown plate resumes this request.
            self.reask = Some(self.reask.unwrap_or(false));
            return true;
        }
        if let Err(e) = self.machine() {
            self.stop_slice(state);
            state.status = SliceStatus::Failed(e);
            return true;
        }
        state.fit_warning = self.fit_warning(plate);
        let (job, input) = match self.live.take() {
            Some(live) => (live.job, live.input),
            None => match self.source(plate, Arc::from([]), None) {
                Ok((source, name)) => self.job(state, source, name).prepare(),
                Err(e) => {
                    self.stop_slice(state);
                    state.status = e.map_or(SliceStatus::Idle, SliceStatus::Failed);
                    return true;
                }
            },
        };
        self.start_slice(state, job, input)
    }

    fn resume_pending_send(&mut self, state: &mut PrintState, plate: Option<&Arc<Evaluated>>) -> bool {
        let Some(held) = self.pending_send.take() else { return false };
        if matches!(&state.confirm_print, Some(PrintConfirm::Filament(card)) if card == &held.ticket) && self.ticket_current(state, &held.ticket) {
            let options = state.print_options;
            state.confirm_print = None;
            state.print_options.plate_clear = false;
            self.admit_send_to_printer(state, &held.ticket, held.preview, options, plate);
            return true;
        }
        if state.confirm_print.take().is_some() {
            state.print_options.plate_clear = false;
            state.print_blocked.set(&state.links.picked, Some(STALE_TICKET.into()));
            return true;
        }
        false
    }

    /// A slice Print, Export or Slice was waiting for went with an edit (the plate or the settings
    /// changed under it): it goes on with what stands now, and an Export waiting on it writes the
    /// newer slice. True when the view changed.
    pub fn resume_asked(&mut self, state: &mut PrintState, plate: Option<&Arc<Evaluated>>) -> bool {
        if self.calibration_waiting(state) || self.slice_gen.is_some() { return false; }
        if let Some(reason) = self.calibration_failure(state, plate) {
            self.fail_for_calibration(state, reason, self.reask == Some(true));
            return true;
        }
        let resumed = self.resume_pending_send(state, plate);
        let Some(export) = self.reask.take() else { return resumed };
        if state.can_send() { return (export && self.export(state, plate)) || resumed; }
        let changed = self.slice(state, plate);
        if export { self.await_export(state); }
        changed
    }

    /// Print, Export or Slice awaits the slice in progress: it finishes whatever the view does.
    pub fn slice_asked(&self) -> bool {
        self.slice_gen.is_some() || self.reask.is_some() || self.pending_send.is_some()
    }

    /// Hold the editor's slice until the model worker has applied the printer settings and
    /// its patterns have finished growing. Other views may still own the engine.
    fn wait_for_plate(&mut self, state: &mut PrintState, plate: Option<&Arc<Evaluated>>) -> bool {
        let Some(ev) = plate else { return false };
        let status = match self.plate_waits(state, ev) {
            Some(why) => SliceStatus::Running { progress: 0.0, stage: why.into() },
            None if ev.patterns_pending() => growing_status(ev),
            None => return false,
        };
        let own = self.slice_gen.is_some() || self.live.is_some();
        self.invalidate(state);
        state.streamed = None;
        if own {
            crate::live_slice::cancel();
        }
        state.status = status;
        true
    }

    /// The status of a plate whose patterns still grow, as they grow (a step counter); `false`
    /// when it shows no growth (none grows, or the slice is on to something else).
    pub fn growth_moved(&self, state: &mut PrintState, plate: Option<&Arc<Evaluated>>) -> bool {
        let Some(ev) = plate.filter(|ev| ev.patterns_pending()) else { return false };
        if self.slice_gen.is_some() || self.live.is_some() || !matches!(&state.status, SliceStatus::Running { stage, .. } if stage.starts_with(GROWING)) {
            return false;
        }
        let status = growing_status(ev);
        if status == state.status {
            return false;
        }
        state.status = status;
        true
    }

    /// No slice is wanted now: the one in progress (of an older plate) stops, and its answer
    /// is no longer awaited.
    pub fn stop_slice(&mut self, state: &mut PrintState) {
        // Only this editor's own slice stops: a headset's live slice (or nothing) runs on.
        let own = self.slice_gen.is_some() || self.live.is_some();
        self.invalidate(state);
        if self.reask.take() == Some(true) { state.export = ExportStatus::Idle; }
        state.streamed = None;
        if own {
            crate::live_slice::cancel();
        }
    }

    fn start_slice(&mut self, state: &mut PrintState, mut job: crate::live_slice::PrintJob, input: crate::live_slice::SliceInput) -> bool {
        mark_test_slice("fab.test_print.slice.start", &job, input.plate_id);
        self.live_key = Some((input.plate_id, input.settings_id));
        if self.tests.is_none() {
            self.figures_of = Some((input.plate_id, input.settings_id));
        }
        self.export_waits = false;
        self.gen += 1;
        self.slice_gen = Some(self.gen);
        self.issues_gen = None;
        job.gen = self.gen;
        // The settled slice has its own job (with a cancel) on the board.
        if let Some(job) = self.live_job.take() {
            job.done();
        }
        // A newer requested slice continues the same job; the person's cancel stops it.
        match &self.slice_job {
            Some(board) => board.set_title(format!("Slicing {}", job.name)),
            None => self.slice_job = Some(JobSpec::new(format!("Slicing {}", job.name)).stage("preparing the job").on_cancel(crate::live_slice::cancel).start()),
        }
        self.started = Some(std::time::Instant::now());
        state.settled = false;
        state.status = SliceStatus::Running { progress: 0.0, stage: "Preparing the job".into() };
        crate::live_slice::settle(job, input);
        true
    }

    fn clear_export(&mut self, state: &mut PrintState) {
        for p in &self.packaging {
            if matches!(p.to, PackageFor::File(_)) { p.stop.store(true, Ordering::Relaxed); }
        }
        self.export_attempt = None;
        state.export = ExportStatus::Idle;
    }

    fn await_export(&mut self, state: &mut PrintState) {
        self.export_waits = self.slice_gen.is_some();
        if let Some(waiting) = &mut self.reask { *waiting = true; }
        state.export = if self.export_waits || self.reask.is_some() {
            ExportStatus::Exporting
        } else if matches!(state.status, SliceStatus::Failed(_)) {
            ExportStatus::Idle
        } else {
            ExportStatus::Failed("no printable parts on the plate".into())
        };
    }

    fn export(&mut self, state: &mut PrintState, plate: Option<&Arc<Evaluated>>) -> bool {
        if self.calibration_waiting(state) {
            self.reask = Some(true);
            state.export = ExportStatus::Exporting;
            return true;
        }
        if let Some(reason) = self.calibration_failure(state, plate) {
            self.fail_for_calibration(state, reason, true);
            return true;
        }
        if let Err(err) = &self.machine {
            state.export = ExportStatus::Failed(err.local_message());
            return true;
        }
        if !state.can_send() {
            self.clear_export(state);
            let changed = self.slice(state, plate);
            self.await_export(state);
            return changed;
        }
        let Some(last) = self.last.clone() else {
            self.clear_export(state);
            state.export = ExportStatus::Failed("slice the plate first: Export writes the sliced plate".into());
            return true;
        };
        let to = PackageFor::File(export_path_in(&self.dirs.documents, &last.name, "gcode.3mf"));
        for p in &self.packaging {
            if matches!(p.to, PackageFor::File(_)) && (p.to != to || !Arc::ptr_eq(&p.sliced, &last)) {
                p.stop.store(true, Ordering::Relaxed);
            }
        }
        self.export_attempt = Some(self.package(last, to, Vec::new()));
        state.export = ExportStatus::Exporting;
        true
    }
}

/// Where Export writes the plate `name`'s `ext` (in `~/Documents/Fab`, made if need be): the name
/// with anything but letters, digits, `-` and `_` turned into `-`.
pub(crate) fn export_path(name: &str, ext: &str) -> std::path::PathBuf {
    export_path_in(&crate::model_host::documents_dir(), name, ext)
}

fn export_path_in(dir: &Path, name: &str, ext: &str) -> PathBuf {
    let _ = std::fs::create_dir_all(dir);
    let stem: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    dir.join(format!("{}.{ext}", if stem.trim_matches('-').is_empty() { "plate" } else { stem.trim_matches('-') }))
}

fn defaults_settings() -> PrintSettings {
    PrintSettings {
        use_installed: false,
        machine: DEFAULT_MACHINE.into(),
        plate: "Textured PEI Plate".into(),
        process: DEFAULT_PROCESS.into(),
        ..Default::default()
    }
}

/// The heads the linked printer (configured as `configured`) is watched for: those of the
/// selected profile (of model `printer_model`), when that profile is the printer's.
fn profile_heads(configured: &str, printer_model: &str, machine: &Machine) -> Option<usize> {
    same_printer(configured, printer_model).then_some(machine.nozzles.len())
}

/// The installed filament preset whose material id a tray reports (compatible with `machine`).
fn preset_of_id(cat: &Catalog, machine: &str, id: &str) -> Option<String> {
    let of = |e: &&Entry| e.kind == Kind::Filament && e.filament_id.as_deref() == Some(id);
    let fits = |e: &&Entry| e.user || e.compat.is_empty() || e.compat.iter().any(|c| c == machine);
    cat.entries.iter().filter(of).find(fits).or_else(|| cat.entries.iter().find(of)).map(|e| e.name.clone())
}

/// What the plate prints and shows with under a colour mapping: each colour's material colour
/// and job tool.
pub fn mapped_of(m: &Mapping) -> Mapped {
    Mapped {
        inks: m.inks(),
        tools: m.rows.iter().filter_map(|r| Some((r.name.clone(), m.tool_of(&r.name)?))).collect(),
    }
}

/// The `#RRGGBB` (sRGB) of a linear colour: what the job's `filament_colour` says.
fn colour_hex(c: [f32; 4]) -> String {
    format!("#{:06X}", srgb(c))
}

/// Pack a linear spool colour for the settings file or G-code header.
fn srgb(c: [f32; 4]) -> u32 {
    let b = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        ((if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }) * 255.0).round() as u8
    };
    u32::from_be_bytes([0, b(c[0]), b(c[1]), b(c[2])])
}

/// Import on the profiles worker so the result always includes the latest virtual layouts.
fn import_profiles(path: &std::path::Path, select: bool, dirs: &[std::path::PathBuf], imports: &std::path::Path, printers: &[VirtualPrinter], layout_revision: u64) {
    use makepad_fab_settings::import::import_into;
    let store = ProfileStore::open_with_imports(dirs);
    let batch = import_into(path, &store, imports);
    let pick = |kind: &str| batch.presets.iter().filter(|p| p.preset.kind == kind).map(|p| p.preset.name.clone()).collect::<Vec<_>>();
    let machines = pick("machine");
    let processes = pick("process");
    let filaments = pick("filament");
    let chosen = (select && !batch.presets.is_empty()).then(|| (machines.first().cloned().unwrap_or_default(), processes.first().cloned().unwrap_or_default(), filaments));
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let mut summary = format!("{name}: {}", batch.summary());
    if let Some(e) = batch.errors.first() {
        summary.push_str(&format!(" ({e})"));
    }
    Cx::post_action(PrintEvent::ProfilesImported { summary, select: chosen, catalog: catalog_in(dirs, imports, printers).map(|mut cat| { cat.layout_revision = layout_revision; Arc::new(cat) }) });
}

/// Build the virtual stock from the same offline positions the colour picker offers.
fn twin_profile(preset: &Preset, store: &ProfileStore) -> Option<TwinProfile> {
    use makepad_fab_settings::family::{of_profile, Package};
    if fab_sla::printer::is_resin(&preset.values) { return None; }
    match of_profile(&preset.values).ok()?.package { Package::GcodeThreeMf { .. } | Package::PlainGcode => {} }
    let machine = machine_of(&preset.name, &preset.values).ok()?;
    let offline = colours::Offline::of(&machine);
    let slots = colours::layout(None, None, &offline, &|_| None).trays.into_iter().map(|tray| {
        let path = match tray.path {
            fab_plate::machine::SlotPath::Shared => virtual_printer::SlotPath::Shared,
            fab_plate::machine::SlotPath::Own => virtual_printer::SlotPath::Own,
            fab_plate::machine::SlotPath::Dedicated => virtual_printer::SlotPath::Dedicated,
        };
        (tray.pos.head, tray.pos.tray_text(), path)
    }).collect();
    let diameters = (0..offline.heads.len()).map(|i| parse_num(preset.values.raw("nozzle_diameter", i))
        .filter(|d| d.is_finite() && *d > 0.0)).collect::<Option<Vec<_>>>()?;
    let printer = VirtualPrinter::twin(String::new(), &preset.name, preset.values.str("printer_model"), offline.heads.len(), slots, Vec::new()).ok()?;
    let model = printer.model();
    let default_plate = store.resolve(&model).ok().map(|p| p.values.str("default_bed_type").to_owned()).unwrap_or_default();
    let machine = Arc::new(BoundMachine { geometry: Arc::new(machine), layout: offline, model, diameters, default_plate });
    let binding = Arc::new(MachineBinding::Available { machine: machine.clone(), automatic: machine.clone(), choices: Arc::default(), explicit: None });
    Some(TwinProfile { printer, machine, binding })
}

/// The catalogue of installed and imported files and Fab's built-in machines.
fn catalog_in(dirs: &[std::path::PathBuf], imports: &std::path::Path, printers: &[VirtualPrinter]) -> Option<Catalog> {
    let files = ProfileStore::open(dirs, &[imports.to_path_buf()]);
    let installed = !files.is_empty();
    let store = files.with_builtin(fab_sla::printer::builtin_presets());
    let store = if printers.is_empty() { store } else { store.with_builtin(virtual_printer::builtin_presets(printers)) };
    let mut entries = Vec::new();
    let mut twins = HashMap::new();
    let mut resin = std::collections::HashSet::new();
    let mut names: Vec<&String> = store.names().collect();
    names.sort();
    for name in names {
        let Ok(raw) = store.raw(name) else { continue };
        let (folder, user) = match store.path_of(name) {
            Some(path) => {
                let p = path.to_string_lossy().replace('\\', "/");
                let folder = ["machine", "process", "filament"].into_iter().find(|k| p.contains(&format!("/{k}/")));
                (folder, p.contains("/user/") || path.starts_with(imports))
            }
            None => (Some(raw.kind.as_str()), false),
        };
        let kind = match folder {
            Some("machine") => Kind::Machine,
            Some("process") => Kind::Process,
            Some("filament") => Kind::Filament,
            _ => continue,
        };
        if !user && !raw.instantiation {
            continue;
        }
        if kind == Kind::Machine {
            if let Ok(preset) = store.resolve(name) {
                if fab_sla::printer::is_resin(&preset.values) {
                    resin.insert(name.clone());
                    resin.insert(preset.values.str("printer_model").trim().to_ascii_lowercase());
                } else if let Some(twin) = twin_profile(&preset, &store) {
                    twins.insert(name.clone(), twin);
                }
            }
        }
        let compat = match raw.values.get("compatible_printers") {
            Some(v) => v.as_vec().into_iter().filter(|s| !s.is_empty()).collect(),
            None => Vec::new(),
        };
        // Resolve inherited material once on the catalogue worker, never on the UI.
        let resolved = (kind == Kind::Filament).then(|| store.resolve(name).ok()).flatten();
        let filament = resolved.as_ref();
        let material = filament.map(|p| p.values.raw("filament_type", 0).to_string()).filter(|s| !s.is_empty());
        let plates = PLATES.iter().filter(|(_, key)| filament.is_some_and(|p| makepad_fab_settings::machine::filament_suits_plate(p.values.bool("fab_bed_unheated"), p.values.f64(key)))).map(|(name, _)| name.to_string()).collect();
        let filament_id = filament.map(|p| p.filament_id.clone()).filter(|s| !s.is_empty());
        entries.push(Entry { name: name.clone(), kind, user, compat, material, filament_id, plates });
    }
    // Keep picker order here so a printer pick only filters the list on the UI thread.
    entries.sort_by_cached_key(|e| (!e.user, e.name.to_lowercase()));
    let mut models = HashMap::<String, Vec<String>>::new();
    for profile in entries.iter().filter_map(|e| twins.get(&e.name)) {
        let model = &profile.machine.model;
        let family = makepad_fab_settings::family::of_model(model).expect("validated filament model");
        for alias in std::iter::once(model.as_str()).chain(model.strip_prefix(family.maker)) {
            models.entry(alias.to_ascii_lowercase()).or_default().push(profile.machine.geometry.name.clone());
        }
    }
    let selection = dirs.iter().find_map(|dir| installed_selection(dir));
    let automatic = models.into_iter().map(|(model, names)| {
        let name = selection.as_ref().map(|s| &s.machine).filter(|name| names.contains(name)).unwrap_or(&names[0]);
        let machine = twins[name].machine.clone();
        let binding = MachineBinding::Available { machine: machine.clone(), automatic: machine, choices: names.into(), explicit: None };
        (model, Arc::new(binding))
    }).collect();
    Some(Catalog { layout_revision: 0, selection, store: Arc::new(store), entries, twins, automatic, resin, installed })
}

impl Catalog {
    /// The default plate the model of printer preset `machine` records (reads preset files).
    fn printer_plate(&self, machine: &str) -> Option<String> {
        let model = self.store.resolve(machine).ok()?.values.str("printer_model").to_string();
        let plate = self.store.resolve(&model).ok()?.values.str("default_bed_type").to_string();
        (!plate.is_empty()).then_some(plate)
    }
}

/// A plate nobody picked, among `fits` (the plates every job material suits, in `PLATES`
/// order): the printer's own default, else Textured PEI, else the first; "" when none fits.
fn default_plate(fits: &[String], printer_default: &str) -> String {
    [printer_default, "Textured PEI Plate"].into_iter().find(|p| fits.iter().any(|f| f == p)).or(fits.first().map(String::as_str)).unwrap_or_default().to_string()
}

/// The build volume and nozzle reaches described by a printer preset; a resin printer's
/// screen and height.
fn machine_of(name: &str, values: &Config) -> Result<Machine, MachineError> {
    if fab_sla::printer::is_resin(values) {
        return fab_sla::printer::resin_machine(name, values).map_err(|detail| MachineError::ResinSpec { preset: name.into(), detail });
    }
    let invalid = |key| MachineError::InvalidKey { preset: name.into(), key };
    let bounds = |points: &[(f64, f64)], key: &'static str| -> Result<Rect, MachineError> {
        if points.len() < 3 || points.iter().any(|(x, y)| !x.is_finite() || !y.is_finite()) {
            return Err(invalid(key));
        }
        let mut rect = Rect::new(f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for &(x, y) in points {
            rect.min[0] = rect.min[0].min(x);
            rect.min[1] = rect.min[1].min(y);
            rect.max[0] = rect.max[0].max(x);
            rect.max[1] = rect.max[1].max(y);
        }
        if rect.width() <= 0.0 || rect.depth() <= 0.0 {
            return Err(invalid(key));
        }
        Ok(rect)
    };
    let points = values.points("printable_area");
    let count: usize = values.get("printable_area").map(|v| v.as_vec().iter().map(|s| s.split(',').count()).sum()).unwrap_or(0);
    if points.len() != count {
        return Err(invalid("printable_area"));
    }
    let bed = bounds(&points, "printable_area")?;
    let max_height = parse_num(values.str("printable_height")).filter(|h| *h > 0.0).ok_or_else(|| invalid("printable_height"))?;
    let count = values.vec_len("nozzle_diameter");
    if count == 0 {
        return Err(invalid("nozzle_diameter"));
    }
    // Without per-extruder limits (the key missing or empty) every nozzle reaches the whole volume.
    let areas = values.point_groups("extruder_printable_area");
    let areas = (!areas.iter().all(|g| g.is_empty())).then_some(areas);
    let heights = values.get("extruder_printable_height").map(|v| v.as_vec()).unwrap_or_default();
    let heights = (!heights.iter().all(|h| h.trim().is_empty())).then_some(heights);
    let mut nozzles = Vec::with_capacity(count);
    for e in 0..count {
        let reach = match &areas {
            Some(areas) => bounds(areas.get(e).ok_or_else(|| invalid("extruder_printable_area"))?, "extruder_printable_area")?,
            None => bed,
        };
        let height = match &heights {
            Some(heights) => heights.get(e).and_then(|h| parse_num(h)).filter(|h| *h > 0.0).ok_or_else(|| invalid("extruder_printable_height"))?,
            None => max_height,
        };
        nozzles.push(Nozzle { name: nozzle_name(count, e), reach, max_height: height });
    }
    // Areas no part may cover: the bed's excluded area and the nozzle-wrap detection area,
    // one polygon each (none when the key is empty).
    let mut keep_out = Vec::new();
    for key in ["bed_exclude_area", "wrapping_exclude_area"] {
        let points = values.points(key);
        if !points.is_empty() {
            keep_out.push(bounds(&points, key)?);
        }
    }
    // The toolhead's clearance for printing by object (0 when the preset leaves it out:
    // the by-object check then refuses).
    let num = |key: &str| parse_num(values.str(key)).filter(|v| v.is_finite() && *v >= 0.0).unwrap_or(0.0);
    let clearance = Clearance {
        max_radius: num("extruder_clearance_max_radius"),
        height_to_rod: num("extruder_clearance_height_to_rod"),
        height_to_lid: num("extruder_clearance_height_to_lid"),
        dist_to_rod: num("extruder_clearance_dist_to_rod"),
    };
    // The places filaments feed the nozzles from: the profile's, else the family's layout.
    use makepad_fab_settings::family::{self, Spools};
    let spools = family::of(values).ok_or_else(|| MachineError::UnsupportedFamily { preset: name.into() })?.spools;
    // A head's own spool is labelled by its unit's letters (A1, B1, … Z1, AA1): the trays'
    // numbering has letters for `UNITS` units.
    if spools == Spools::OnePerHead && count > fab_plate::machine::UNITS {
        return Err(MachineError::TooManyHeads { preset: name.into() });
    }
    let named = values.get("fab_material_slots").map(|v| v.as_vec()).unwrap_or_default();
    let slots = if named.iter().all(|s| s.trim().is_empty() || s.trim() == "nil") {
        use fab_plate::machine::{Slot, SlotPath};
        match spools {
            Spools::Units => Machine::dual_nozzle_slots(),
            Spools::OnePerHead => {
                (0..count).map(|head| Slot {
                    nozzle: head,
                    tray: colours::tray_text(TrayId { ams: head as u8, slot: 0 }),
                    path: SlotPath::Own,
                }).collect()
            }
        }
    } else {
        Machine::parse_slots(&named, count).map_err(|detail| {
            // A prefix fails from the first bad entry onward, including a duplicate tray.
            let (mut lo, mut hi) = (1, named.len());
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                if Machine::parse_slots(&named[..mid], count).is_ok() { lo = mid + 1; } else { hi = mid; }
            }
            MachineError::InvalidMaterialSlots { preset: name.into(), slot_index: Some(lo), raw_entry: Some(named[lo - 1].trim().into()), detail }
        })?
    };
    Ok(Machine { name: name.into(), technology: fab_plate::machine::Technology::Filament, bed, max_height, nozzles, keep_out, clearance, slots })
}

/// Fab's own job: the machine facts of the plate's printer (bed, both
/// nozzles' reach and heights), option defaults for the rest.
fn defaults_setup(s: &PrintSettings, machine: &Machine, nf: usize) -> JobSetup {
    let mut m = Preset { name: DEFAULT_MACHINE.into(), kind: "machine".into(), ..Default::default() };
    let n = machine.nozzles.len().max(1);
    let v = &mut m.values;
    v.set_vec("printable_area", machine.bed.corners());
    // Fab's default printer's one keep-out zone is its nozzle-wrap detection area.
    v.set_vec("wrapping_exclude_area", machine.keep_out.first().map(Rect::corners).unwrap_or_default());
    v.set_str("printable_height", format!("{}", machine.max_height));
    for (key, value) in [
        ("extruder_clearance_max_radius", machine.clearance.max_radius),
        ("extruder_clearance_height_to_rod", machine.clearance.height_to_rod),
        ("extruder_clearance_height_to_lid", machine.clearance.height_to_lid),
        ("extruder_clearance_dist_to_rod", machine.clearance.dist_to_rod),
    ] { v.set_str(key, value.to_string()); }
    v.set_vec("extruder_printable_area", machine.nozzles.iter().map(|z| z.reach.corners().join(",")).collect());
    v.set_vec("extruder_printable_height", machine.nozzles.iter().map(|z| format!("{}", z.max_height)).collect());
    v.set_vec("nozzle_diameter", vec!["0.4".into(); n]);
    v.set_vec("extruder_type", vec!["Direct Drive".into(); n]);
    // Firmware tool numbers per extruder: the right nozzle is tool 0.
    v.set_vec("physical_extruder_map", (0..n).map(|i| format!("{}", n - 1 - i)).collect());
    // Motion limits of a fast CoreXY toolhead (normal and silent mode).
    for (k, val) in [
        ("machine_max_speed_x", "1000"),
        ("machine_max_speed_y", "1000"),
        ("machine_max_speed_z", "30"),
        ("machine_max_speed_e", "120"),
        ("machine_max_acceleration_x", "20000"),
        ("machine_max_acceleration_y", "20000"),
        ("machine_max_acceleration_z", "1500"),
        ("machine_max_acceleration_e", "5000"),
        ("machine_max_acceleration_extruding", "20000"),
        ("machine_max_acceleration_retracting", "5000"),
        ("machine_max_acceleration_travel", "9000"),
        ("machine_max_jerk_x", "9"),
        ("machine_max_jerk_y", "9"),
        ("machine_max_jerk_z", "3"),
        ("machine_max_jerk_e", "2.5"),
    ] {
        v.set_vec(k, vec![val.into(), val.into()]);
    }
    let mut process = Preset { name: DEFAULT_PROCESS.into(), kind: "process".into(), ..Default::default() };
    for (k, val) in [
        // The wall generator every vendor process for this machine uses.
        ("wall_generator", "classic"),
        ("outer_wall_speed", "150"),
        ("inner_wall_speed", "250"),
        ("sparse_infill_speed", "250"),
        ("internal_solid_infill_speed", "220"),
        ("top_surface_speed", "150"),
        ("gap_infill_speed", "150"),
        ("initial_layer_speed", "50"),
        ("initial_layer_infill_speed", "80"),
        ("travel_speed", "400"),
        ("default_acceleration", "8000"),
        ("outer_wall_acceleration", "4000"),
        ("travel_acceleration", "9000"),
    ] {
        process.values.set_vec(k, vec![val.into()]);
    }
    for (k, val) in [("layer_height", "0.2"), ("sparse_infill_density", "15%"), ("sparse_infill_pattern", "gyroid"), ("wall_loops", "2"), ("support_type", "tree(auto)")] {
        process.values.set_str(k, val);
    }
    let mut filament = Preset { name: DEFAULT_FILAMENT.into(), kind: "filament".into(), ..Default::default() };
    for (k, val) in [("filament_max_volumetric_speed", "18"), ("filament_density", "1.24"), ("nozzle_temperature", "220"), ("nozzle_temperature_initial_layer", "220"), ("filament_type", "PLA")] {
        filament.values.set_vec(k, vec![val.into()]);
    }
    for (_, key) in PLATES {
        filament.values.set_vec(key, vec!["55".into()]);
        filament.values.set_vec(&format!("{key}_initial_layer"), vec!["55".into()]);
    }
    let mut flex = Preset { name: DEFAULT_FLEX_FILAMENT.into(), kind: "filament".into(), ..Default::default() };
    for (k, val) in [
        ("filament_type", "TPU"),
        ("filament_density", "1.21"),
        ("nozzle_temperature", "225"),
        ("nozzle_temperature_initial_layer", "225"),
        // Soft filament buckles when pushed fast or pulled back far.
        ("filament_max_volumetric_speed", "3"),
        ("filament_retraction_length", "0.4"),
        ("filament_retraction_speed", "20"),
        ("fan_min_speed", "60"),
        ("fan_max_speed", "100"),
    ] {
        flex.values.set_vec(k, vec![val.into()]);
    }
    for (_, key) in PLATES {
        flex.values.set_vec(key, vec!["40".into()]);
        flex.values.set_vec(&format!("{key}_initial_layer"), vec!["40".into()]);
    }
    JobSetup {
        machine: m,
        process,
        filaments: (0..nf).map(|i| if s.materials.get(i).is_some_and(|m| m.preset == DEFAULT_FLEX_FILAMENT) { flex.clone() } else { filament.clone() }).collect(),
        filament_map: vec![1; nf],
        nozzle_volume_type: vec!["Standard".into(); n],
        bed_type: s.plate.clone(),
        filament_map_mode: "Manual".into(),
        filament_colours: vec![None; nf],
    }
}

/// Where the prime tower goes: the free spot nearest the back middle that
/// all nozzles reach, clear of every part and keep-out zone.
fn tower_spot(plate: &Plate, machine: &Machine, size: f64) -> [f64; 2] {
    let pad = 6.0;
    let boxes: Vec<[f64; 4]> = plate
        .parts
        .iter()
        .map(|p| {
            let b = p.bounds();
            [b.min[0] - pad, b.min[1] - pad, b.max[0] + pad, b.max[1] + pad]
        })
        .chain(machine.keep_out.iter().map(|r| [r.min[0], r.min[1], r.max[0], r.max[1]]))
        .collect();
    let lo_x = machine.nozzles.iter().map(|n| n.reach.min[0]).fold(machine.bed.min[0], f64::max) + 5.0;
    let hi_x = machine.nozzles.iter().map(|n| n.reach.max[0]).fold(machine.bed.max[0], f64::min) - size - 5.0;
    let lo_y = machine.nozzles.iter().map(|n| n.reach.min[1]).fold(machine.bed.min[1], f64::max) + 5.0;
    let hi_y = machine.nozzles.iter().map(|n| n.reach.max[1]).fold(machine.bed.max[1], f64::min) - size - 15.0;
    let want = [(machine.bed.min[0] + machine.bed.max[0]) * 0.5 - size * 0.5, hi_y];
    let mut best: Option<([f64; 2], f64)> = None;
    let mut y = lo_y;
    while y <= hi_y {
        let mut x = lo_x;
        while x <= hi_x {
            let r = [x, y, x + size, y + size];
            let clear = boxes.iter().all(|b| r[2] <= b[0] || r[0] >= b[2] || r[3] <= b[1] || r[1] >= b[3]);
            if clear {
                let d = (x - want[0]).powi(2) + (y - want[1]).powi(2);
                if best.map_or(true, |(_, bd)| d < bd) {
                    best = Some(([x, y], d));
                }
            }
            x += 5.0;
        }
        y += 5.0;
    }
    best.map(|b| b.0).unwrap_or(want)
}

/// What a calibration test part is made for: the job's nozzle and layer height, and the first
/// filament as it prints now (its preset with any stored calibration). Reads preset files:
/// call off the UI thread.
pub fn coupon_setup(s: &PrintSettings, catalog: Option<&Catalog>, effective: Option<&EffectiveResults>) -> fab_calibrate::CouponSetup {
    let mut cs = fab_calibrate::CouponSetup { layer: s.layer_height.max(0.05), ..Default::default() };
    let filament = s.materials.first().map(|f| f.preset.clone()).unwrap_or_default();
    if let Some(cat) = catalog {
        if let Some(n) = cat.store.resolve(&s.machine).ok().and_then(|m| parse_num(m.values.raw("nozzle_diameter", 0))) {
            cs.nozzle = n;
        }
        if let Ok(p) = cat.store.resolve(&filament) {
            let v = &p.values;
            if !v.raw("filament_type", 0).is_empty() {
                cs.material = v.raw("filament_type", 0).to_string();
            }
            cs.flow_ratio = parse_num(v.raw("filament_flow_ratio", 0)).filter(|f| *f > 0.0).unwrap_or(cs.flow_ratio);
            cs.nozzle_temp = parse_num(v.raw("nozzle_temperature", 0)).filter(|t| *t > 0.0).unwrap_or(cs.nozzle_temp);
        }
    }
    let measured = |name| effective.and_then(|r| r.get(name));
    use fab_calibrate::results::names;
    cs.flow_ratio = measured(names::FLOW_RATIO).unwrap_or(cs.flow_ratio);
    cs.nozzle_temp = measured(names::NOZZLE_TEMP).unwrap_or(cs.nozzle_temp);
    cs.c_xy = measured(names::C_XY).unwrap_or((0.875 * cs.nozzle).max(0.2));
    cs
}

/// The plate's heat-set inserts (`insert(.., melt: ..)`) as steps after the print: one per size
/// and part, with the job tool (0-based) the part prints in. Cost: O(inserts × parts), once per
/// slice job.
pub(crate) fn melt_steps(plate: &fab_plate::Plate) -> Vec<(fab::print::PreviewMelt, usize)> {
    let mut out: Vec<(fab::print::PreviewMelt, usize)> = Vec::new();
    for e in &plate.excluded {
        let Some((insert, melt)) = e.insert.as_ref().and_then(|i| Some((i, i.melt.as_ref()?))) else { continue };
        let Some(host) = plate.parts.iter().find(|p| p.object == insert.into) else { continue };
        let tool = (host.filament as usize).max(1) - 1;
        match out.iter_mut().find(|(m, t)| m.size == melt.size && m.into == host.name && *t == tool) {
            Some((m, _)) => m.count += 1,
            None => out.push((fab::print::PreviewMelt { size: melt.size.clone(), count: 1, into: host.name.clone(), iron: 0 }, tool)),
        }
    }
    out
}

/// A preview's steps after the print ([`melt_steps`]), each with its iron at `iron(tool)` (the
/// print temperature of the job tool's filament; 0 when not known), and the summary row that
/// lists them. The settled slice and the live estimate both say them.
pub(crate) fn add_melts(pv: &mut fab::print::PrintPreview, melts: &[(fab::print::PreviewMelt, usize)], iron: impl Fn(usize) -> Option<f64>) {
    pv.melts = melts.iter().map(|(m, tool)| {
        let iron = iron(*tool).filter(|t| *t > 0.0);
        fab::print::PreviewMelt { iron: iron.map_or(0, |t| t.round() as u32), ..m.clone() }
    }).collect();
    if !pv.melts.is_empty() {
        let what: Vec<String> = pv.melts.iter().map(|m| format!("{} × {} into {}", m.count, m.size, m.into)).collect();
        pv.summary.push(("Melt in after".to_string(), what.join(" · ")));
    }
}

fn unmapped_colour() -> [f32; 4] {
    let [r, g, b] = fab_plate::UNMAPPED_RGB;
    [r, g, b, 1.0]
}

/// Only a linked printer can report the spool colour shown by a preview.
fn reported_tool_colours(state: &PrintState) -> Vec<[f32; 4]> {
    state.settings.materials.iter().map(|m| {
        state.target_printer().and_then(|_| state.colors.layout.tray(colours::Position { head: m.nozzle, tray: m.tray? }))
            .and_then(|t| t.colour).unwrap_or_else(unmapped_colour)
    }).collect()
}

/// The same job tool has two appearances. Only used print colours compete for its
/// model swatch, in project order. An offline loadout never supplies a material RGB.
pub(crate) fn tool_colours(ev: &Evaluated, trays: Option<&[[f32; 4]]>) -> ToolColours {
    let mut names = HashMap::new();
    let mut count = trays.map_or(1, |t| t.len().max(1));
    for (i, part) in ev.plate.parts.iter().enumerate() {
        let tool = part.filament.max(1) as usize;
        count = count.max(tool);
        if let Some(name) = ev.part_color(i) {
            names.insert(name, tool - 1);
        }
        for paint in part.paint.iter().flat_map(|p| &p.colors) {
            let slot = paint.filament.unwrap_or(part.filament).max(1) as usize;
            count = count.max(slot);
            if let Some(name) = paint.color.as_deref() {
                names.insert(name, slot - 1);
            }
        }
    }
    let mut model = vec![None; count];
    let mut material = vec![unmapped_colour(); count];
    let mut shared = vec![false; count];
    for c in &ev.colors {
        let Some(&i) = names.get(c.name.as_str()) else { continue };
        if model[i].is_some() {
            shared[i] = true;
        } else {
            let [r, g, b] = fab_model::eval::design_color(std::slice::from_ref(c), &c.name);
            model[i] = Some([r, g, b, 1.0]);
        }
        if ev.printer_linked && trays.is_none() {
            if let Some(&[r, g, b]) = ev.mapped.inks.get(&c.name) {
                material[i] = [r, g, b, 1.0];
            }
        }
    }
    if let Some(trays) = trays {
        material[..trays.len()].copy_from_slice(trays);
    }
    for (i, part) in ev.plate.parts.iter().enumerate() {
        model[part.filament.max(1) as usize - 1].get_or_insert_with(|| {
            let Some(name) = ev.part_color(i) else { return part.color };
            if ev.colour_mode == fab_plate::ColourMode::Model { return part.color; }
            let [r, g, b] = fab_model::eval::design_color(&ev.colors, name);
            [r, g, b, 1.0]
        });
    }
    ToolColours {
        model: model.into_iter().map(|c| c.unwrap_or([0.8, 0.8, 0.78, 1.0])).collect(),
        material,
        shared_tools: shared.iter().enumerate().filter_map(|(i, &many)| many.then_some(i)).collect(),
    }
}

/// Does a nozzle print two materials of this tool → nozzle map?
fn swaps_on_a_nozzle(map: &[u32]) -> bool {
    map.iter().enumerate().any(|(i, n)| map[..i].contains(n))
}

#[cfg(test)]
fn resolve_setup(s: &PrintSettings, catalog: Option<&Catalog>, plate: &Plate, machine: &Machine) -> Result<JobSetup, String> {
    resolve_tools_with(s, catalog, None, plate, machine, None).map_err(|e| e.redacted_message())
}

/// [`resolve_setup`], with `kept` of only those of the job's tools (0-based, ascending), each on
/// its own nozzle: the ones a slice printed in.
fn resolve_tools(s: &PrintSettings, catalog: Option<&Catalog>, calibration: &EffectiveCalibration, plate: &Plate, machine: &Machine, kept: Option<&[usize]>) -> Result<JobSetup, SetupError> {
    resolve_tools_with(s, catalog, Some(calibration), plate, machine, kept)
}

/// A job takes the latest calibration measurements. The settings panel uses the preset values.
fn resolve_tools_with(s: &PrintSettings, catalog: Option<&Catalog>, calibration: Option<&EffectiveCalibration>, plate: &Plate, machine: &Machine, kept: Option<&[usize]>) -> Result<JobSetup, SetupError> {
    if s.use_installed && catalog.is_none() {
        return Err(SetupError::Machine(MachineError::CatalogUnavailable));
    }
    if s.plate.is_empty() {
        return Err(SetupError::Validation(NO_PLATE_FITS.into()));
    }
    let (_, bed_temp_key) = PLATES.iter().find(|(name, _)| *name == s.plate).ok_or_else(|| SetupError::Validation(format!("Unknown bed plate {}", s.plate)))?;
    // The job's tools: its materials, and any a plate part or its paint asks for beyond them.
    let painted = plate.parts.iter().flat_map(|p| p.paint.iter().flat_map(|sp| sp.colors.iter().filter_map(|c| c.filament))).max().unwrap_or(0) as usize;
    let nf = plate.parts.iter().map(|p| p.filament as usize).max().unwrap_or(1).max(painted).max(s.materials.len()).max(1);
    let mut setup = match catalog {
        Some(cat) => {
            let resolve = |n: &str, role| cat.store.resolve(n).map_err(|detail| SetupError::ResolvePreset { role, preset: n.into(), detail });
            let mut filaments = Vec::new();
            for i in 0..nf {
                let f = s.materials.get(i).map(|f| f.preset.clone()).unwrap_or_default();
                if f.is_empty() {
                    return Err(SetupError::Validation(format!("material {} has no filament preset: its tray reports none, and the installed profiles select none", i + 1)));
                }
                filaments.push(resolve(&f, PresetRole::Filament)?);
            }
            let machine = resolve(&s.machine, PresetRole::Printer)?;
            makepad_fab_settings::family::of_profile(&machine.values).map_err(|msg| SetupError::Validation(msg.into()))?;
            let nozzle_volume_type = makepad_fab_settings::profiles::nozzle_flow_types(&machine, cat.selection.as_ref());
            JobSetup {
                machine,
                process: resolve(&s.process, PresetRole::Process)?,
                filaments,
                filament_map: vec![1; nf],
                nozzle_volume_type,
                bed_type: s.plate.clone(),
                filament_map_mode: "Manual".into(),
                filament_colours: vec![None; nf],
            }
        }
        None => defaults_setup(s, machine, nf),
    };
    let nozzles = setup.extruder_count();
    // Tool → nozzle: its material's position (the tray's nozzle), never chosen apart from it.
    // A tool past the materials (none mapped yet) prints on its part's nozzle.
    for i in 0..nf {
        let nozzle = s.materials.get(i).map(|m| m.nozzle)
            .or_else(|| plate.parts.iter().find(|p| p.filament as usize == i + 1).and_then(|p| p.nozzle))
            .unwrap_or(0);
        if nozzle >= nozzles {
            return Err(SetupError::Validation(format!("material {} prints on the {} nozzle, but the printer has {nozzles}", i + 1, if nozzle == 1 { "right" } else { "left" })));
        }
        setup.filament_map[i] = nozzle as u32 + 1;
        setup.filament_colours[i] = s.materials.get(i).map(|f| colour_hex(f.color));
        // A material on a dedicated path shares none with the others: its changes purge nothing.
        if s.materials.get(i).is_some_and(|m| m.dedicated) {
            setup.filaments[i].values.set_str("fab_dedicated_path", "1");
        }
    }
    // The panel takes only PRINT_SETTINGS and nozzle_diameter. The measured temperature,
    // flow and pressure advance belong to the slice, which still reads its current results.
    if let Some(calibration) = calibration {
        for (i, f) in setup.filaments.iter_mut().enumerate() {
            let preset = s.materials.get(i).map(|f| f.preset.as_str()).unwrap_or("");
            if let Some(results) = calibration.for_filament(preset) {
                fab_calibrate::apply::apply_filament_effective(results, &mut f.values);
            }
        }
    }
    // The project's global settings on top of the process. A filament's own value is
    // per slot, so it goes on the slot's preset.
    for (k, v) in &plate.settings {
        if makepad_fab_settings::options::options().defs.get(k.as_str()).is_some_and(|d| d.preset == makepad_fab_settings::options::PresetKind::Filament) {
            for f in &mut setup.filaments {
                f.values.set_str(k, v);
            }
        } else {
            setup.process.values.set_str(k, v);
        }
    }
    // Every material suits the plate: it sets a bed temperature for it (0: it does not).
    for (i, f) in setup.filaments.iter().enumerate() {
        if !makepad_fab_settings::machine::filament_suits_plate(f.values.bool("fab_bed_unheated"), f.values.f64(bed_temp_key)) {
            return Err(SetupError::Validation(format!("Material {} ({}) has no bed temperature for the {}: pick another plate", i + 1, f.name, s.plate)));
        }
    }
    let p = &mut setup.process.values;
    if nf > 1 {
        let width = if p.has("prime_tower_width") { p.f64("prime_tower_width").max(20.0) } else { 60.0 };
        let at = tower_spot(plate, machine, width);
        p.set_vec("wipe_tower_x", vec![format!("{:.1}", at[0])]);
        p.set_vec("wipe_tower_y", vec![format!("{:.1}", at[1])]);
    }
    let mut setup = match kept {
        Some(kept) => setup.keep(kept),
        None => setup,
    };
    // A tower is for a nozzle changing material: head changes alone need none, whatever the
    // profile says. The project's own setting still decides.
    if !swaps_on_a_nozzle(&setup.filament_map) && !plate.settings.iter().any(|(k, _)| k == "enable_prime_tower") {
        setup.process.values.set_str("enable_prime_tower", "0");
    }
    Ok(setup)
}

/// A settled slice as the Print editor's result: the plate's printability on the sliced
/// layers and moves, and the preview of every move (on the engine's job, never the UI); a live
/// slice's with its parts' beads in their frames (`part_offsets`, [`build_preview_of_layers`]).
pub fn finish_sliced(job: &crate::live_slice::PrintJob, r: &Arc<makepad_fab_slice::live::Settled>, part_offsets: Arc<[[f32; 2]]>) -> Sliced {
    mark_test_slice("fab.test_print.slice.end", job, job.gen);
    let (s, machine, gen) = (&job.settings, &job.machine, job.gen);
    let setup = &r.setup;
    if gen != 0 {
        Cx::post_action(PrintEvent::Progress { gen, progress: 0.95, stage: "Building the preview".to_string() });
    }
    let mut preview = build_preview_of_layers(&r.layers, &r.timeline, job.palettes().for_tools(&r.tools), machine, &r.stats, part_offsets);
    let pv = Arc::get_mut(&mut preview).expect("unpublished preview");
    pv.part_objects = job.objects.clone();
    pv.tools = r.tools.clone();
    piece_figures(pv, piece_reads(job.tests.as_ref(), r.layers.records(), &r.stats));
    pv.gcode_key = Some(r.gcode.hash());
    let report = r.report.clone();
    pv.markers = issue_markers(&report);
    for f in report.plate.iter().flat_map(|p| &p.filaments) {
        pv.summary.push((format!("Filament {} cost", f.filament), format!("{:.2} · peak {:.2} mm³/s", f.cost, f.peak_flow_mm3_s)));
    }
    pv.printability = Arc::new(report);
    // The inserts to melt in after the print, the iron at the print temperature of the filament
    // their part prints in: the slice's tool that is that job tool.
    add_melts(pv, &job.melts, |tool| r.tools.iter().position(|&t| t == tool).map(|k| r.job.fil("nozzle_temperature", k)));
    // The slice's tools are the job's that print.
    let trays = r.tools.iter().map(|&t| s.materials.get(t).and_then(|m| m.tray)).collect();
    let checked = match &job.catalog {
        Some(cat) => gate_profile(cat, setup, &job.head_envelope).and_then(|profile| match fab_printer_link::safety::check_gcode(&r.gcode.text()?, &profile) {
            Ok(facts) => Ok(Checked { facts, profile }),
            Err(e) => Err(e.message()),
        }),
        None => Err("Fab defaults have no printer start sequence: slice with your installed printer profile to print".into()),
    };
    if let Some(pv) = Arc::get_mut(&mut preview) {
        nonplanar_preview(pv, &r.nonplanar, &checked);
    }
    let print_colours = r.tools.iter().enumerate().map(|(slot, &tool)| {
        s.materials.get(tool).map(|m| m.color).unwrap_or_else(|| {
            let hex = r.job.cfg.raw("filament_colour", slot).trim_start_matches('#');
            u32::from_str_radix(hex, 16).map(colours::linear_of).unwrap_or([0.8, 0.8, 0.78, 1.0])
        })
    }).collect();
    Sliced { print_colours, settled: r.clone(), name: job.name.clone(), printer_model: setup.machine.values.str("printer_model").to_string(), preview, head: Toolhead::of(machine), dual: machine.nozzles.len() > 1, trays, used: r.stats.used.clone(), checked }
}

/// The safety check's printer: the fixed limits of the printer model, from the system
/// preset the job's machine preset descends from (never a user preset's edits or the
/// project's overrides). Keys the machine does not have (filament_diameter, volumetric
/// speeds, temperatures) come one per job filament, in filament order, from its presets.
fn gate_profile(cat: &Catalog, setup: &JobSetup, head_envelope: &Path) -> Result<fab_printer_link::safety::MachineProfile, String> {
    gate_profile_of(cat, system_machine(setup), &setup.filaments, head_envelope)
}

/// The system preset a job's machine preset descends from.
fn system_machine(setup: &JobSetup) -> &str {
    if setup.machine.system_parent.is_empty() { &setup.machine.name } else { &setup.machine.system_parent }
}

fn gate_profile_of(cat: &Catalog, name: &str, filaments: &[Preset], head_envelope: &Path) -> Result<fab_printer_link::safety::MachineProfile, String> {
    let system = cat.store.resolve(name).map_err(|e| format!("the printer's system profile {name}: {e}"))?;
    let get = |k: &str| {
        let m = system.values.get(k).map(|v| v.as_vec()).unwrap_or_default();
        if !m.is_empty() { return m; }
        filaments.iter().filter_map(|f| f.values.get(k).map(|v| v.as_vec()).and_then(|v| v.into_iter().next())).collect()
    };
    let templates = filaments.iter().flat_map(|f| ["filament_start_gcode", "filament_end_gcode"].into_iter().map(move |k| (k.to_string(), f.values.raw(k, 0).to_string()))).collect();
    let mut profile = fab_printer_link::safety::MachineProfile::from_settings(&get, templates).map_err(|e| e.message())?;
    // The user's measured head, if installed (the check decides whether it may allow non-planar moves).
    profile.head = crate::head::installed_from(head_envelope);
    Ok(profile)
}

/// The whole live slice's preview with the planner's times (no G-code), built as the time model
/// makes its records ([`makepad_fab_slice::live::RecordTaker`]): each part goes in as it
/// comes, its walls stacked a layer behind, so little is left once the estimate is done. A test
/// plate keeps its records for its pieces' figures, which read the estimate's totals.
pub struct TimedPreview {
    b: PreviewBuilder,
    kept: Option<Vec<Vec<MoveRecord>>>,
    /// The nozzles' targets the records' `temps` name, as they came.
    temps: Vec<Vec<i16>>,
    /// The runs the growing preview holds ([`Self::grown`]).
    grown: usize,
}

impl TimedPreview {
    /// `part_offsets`: where each part's frame sits on the plate, the beads in their frames
    /// ([`build_preview_of_layers`]).
    pub fn new(machine: &Machine, pieces: bool, part_offsets: Arc<[[f32; 2]]>) -> Self {
        let mut b = PreviewBuilder::new(machine);
        b.part_offsets = part_offsets;
        TimedPreview { b, kept: pieces.then(Vec::new), temps: Vec::new(), grown: 0 }
    }

    /// The time model's next records, in print order, and the entries of the nozzles' targets
    /// made since the last; their buffer back to be filled again, but a test plate's.
    pub fn feed(&mut self, part: Vec<MoveRecord>, temps: Vec<Vec<i16>>) -> Option<Vec<MoveRecord>> {
        self.temps.extend(temps);
        self.b.feed(&part);
        match &mut self.kept {
            Some(kept) => {
                kept.push(part);
                None
            }
            None => Some(part),
        }
    }

    /// The preview as far as it has grown, its length and summary those of the estimate `e`
    /// its records make again, once the layers stacked since the last ([`PreviewBuilder::stack_closed`])
    /// are as many as it held: each holds twice the last's, so all of them copy as much as the
    /// whole preview at most. None till then.
    pub fn grown(&mut self, e: &makepad_fab_slice::live::Estimate, colours: &ToolColours, part_objects: &Arc<[String]>) -> Option<Arc<PrintPreview>> {
        let runs = self.b.stacked;
        if runs == 0 || runs < self.grown {
            return None;
        }
        self.b.publish_runs(runs);
        self.grown += runs;
        self.b.temps = Arc::new(self.temps.clone());
        let mut preview = self.b.grown_preview(Some(&e.timeline), colours.for_tools(&e.tools), &e.stats);
        let pv = Arc::get_mut(&mut preview).expect("unpublished preview");
        pv.part_objects = part_objects.clone();
        pv.tools = e.tools.clone();
        Some(preview)
    }

    /// The preview, once the estimate `e` is done: it plays and has the print's summary; its
    /// issues stay the live slice's.
    pub fn preview(mut self, e: &makepad_fab_slice::live::Estimate, colours: ToolColours, part_objects: Arc<[String]>, tests: Option<&makepad_fab_slice::live::MakeInput>) -> Arc<PrintPreview> {
        let reads = self.kept.take().and_then(|kept| piece_reads(tests, kept.into_iter().flatten(), &e.stats));
        self.b.temps = Arc::new(self.temps);
        let mut preview = self.b.preview(Some(&e.timeline), colours.for_tools(&e.tools), &e.stats);
        let pv = Arc::get_mut(&mut preview).expect("unpublished preview");
        pv.part_objects = part_objects;
        pv.tools = e.tools.clone();
        piece_figures(pv, reads);
        preview
    }
}

/// A test plate's pieces and each one's share of its slice, read from the slice's records.
fn piece_reads(tests: Option<&makepad_fab_slice::live::MakeInput>, records: impl Iterator<Item = MoveRecord>, stats: &makepad_fab_gcode::writer::Stats) -> Option<(Arc<piece::Pieces>, piece::PieceReads)> {
    let pieces = tests.and_then(|make| make().pieces)?;
    let read = piece::read_pieces(records, stats, &pieces);
    Some((pieces, read))
}

/// A test plate's slice: each piece's share of it ([`piece_reads`]).
fn piece_figures(pv: &mut PrintPreview, reads: Option<(Arc<piece::Pieces>, piece::PieceReads)>) {
    let Some((pieces, read)) = reads else { return };
    pv.shared = (read.shared_seconds, read.shared_grams);
    pv.pieces = read.pieces.into_iter().map(|p| fab::print::PieceFigures {
        objects: pieces.parts.iter().filter(|q| q.test == p.test).map(|q| q.part.object.clone()).collect(),
        name: p.name,
        seconds: p.seconds,
        grams: p.grams,
        support_grams: p.support_grams,
        brim_grams: p.brim_grams,
        bed: [p.bed_min, p.bed_max],
        layers: p.layers,
        footprint: p.footprint,
        height_over_footprint: p.height_over_footprint,
    }).collect();
}

/// The flight recorder's line for a test plate's slice.
fn mark_test_slice(what: &str, job: &crate::live_slice::PrintJob, at: u64) {
    if job.tests.is_some() {
        makepad_error_log::flight::mark_ab(what, &job.name, at, job.gen);
    }
}

/// The whole Print path in one call (tests): resolve, slice, check, finish.
#[cfg(test)]
fn run_slice(gen: u64, s: &PrintSettings, catalog: Option<&Arc<Catalog>>, plate: &Plate, machine: &Machine, name: &str) -> Result<Sliced, String> {
    let setup = resolve_setup(s, catalog.map(|c| &**c), plate, machine)?;
    let res = makepad_fab_slice::plate::slice_plate_painted_with(&setup, plate, &[], &|_, _| {})?;
    let job = crate::live_slice::PrintJob { gen, head_envelope: crate::dirs::FabDirs::system().head_envelope, settings: s.clone(), colours: s.materials.iter().map(|m| m.color).collect(), model_colours: Vec::new(), shared_tools: Vec::new(), machine: machine.clone(), name: name.to_string(), objects: plate.parts.iter().map(|p| p.object.clone()).collect(), check: None, fallback: None, catalog: catalog.filter(|_| s.use_installed).cloned(), tests: None, melts: melt_steps(plate) };
    Ok(finish_test_slice(job, setup, res, plate))
}

#[cfg(test)]
fn finish_test_slice(job: crate::live_slice::PrintJob, setup: JobSetup, res: makepad_fab_slice::SliceResult, plate: &Plate) -> Sliced {
    let materials = fab_printability::Materials::from_job(&res.job);
    let mut report = fab_printability::analyse_plate(&job.machine, plate, &materials);
    fab_printability::analyse_slice(&job.machine, plate, &materials, res.layers(), &res.timeline, &mut report);
    let mut timeline = res.timeline;
    let layers = makepad_fab_gcode::layers::LayerTimeline::from_timeline(&mut timeline, None);
    let settled = makepad_fab_slice::live::Settled { gcode: makepad_fab_slice::gcode_text::GcodeText::new(res.gcode), timeline, layers, stats: res.stats, report, tools: (0..setup.filaments.len()).collect(), setup: Arc::new(setup), transformed: false, sliced: Vec::new(), nonplanar: Vec::new(), job: res.job, objects: res.objects, layer_filaments: res.layer_filaments };
    finish_sliced(&job, &Arc::new(settled), Arc::from([]))
}

/// The print colours on the trays, for the printer inspection (chat and agents).
pub fn colours_json(m: &Mapping) -> makepad_strict_json::Value {
    use makepad_strict_json::{obj, s, Value};
    Value::Arr(m.rows.iter().map(|r| obj(vec![
        ("name", s(&r.name)),
        ("used", Value::Bool(r.used)),
        ("nozzle", r.pos.map_or(Value::Null, |p| s(colours::head_word(&m.layout.heads, p.head)))),
        ("tray", r.pos.map_or(Value::Null, |p| s(p.tray_text()))),
        ("material", r.tray.as_ref().and_then(|t| t.material.as_deref()).map_or(Value::Null, s)),
        ("saved", Value::Bool(r.how == colours::How::Saved)),
        ("moved", r.moved.as_ref().filter(|m| !m.stays).map_or(Value::Null, |m| s(m.why()))),
        ("warnings", Value::Arr(r.warnings.iter().map(|w| s(w.text())).collect())),
    ])).collect())
}

/// Fab's colour per extrusion role (the Feature legend).
pub fn role_colour(r: Role) -> [f32; 4] {
    let c = match r {
        Role::None => [0.55, 0.55, 0.55],
        Role::OuterWall => [0.96, 0.45, 0.32],
        Role::InnerWall => [0.72, 0.86, 0.30],
        Role::OverhangWall => [0.22, 0.42, 0.96],
        Role::SparseInfill => [0.82, 0.30, 0.66],
        Role::InternalSolidInfill => [0.55, 0.38, 0.88],
        Role::TopSurface => [0.92, 0.20, 0.24],
        Role::BottomSurface => [0.20, 0.64, 0.88],
        Role::Bridge => [0.20, 0.84, 0.82],
        Role::InternalBridge => [0.56, 0.88, 0.94],
        Role::GapFill => [0.94, 0.94, 0.94],
        Role::Ironing => [0.95, 0.58, 0.72],
        Role::Skirt => [0.12, 0.66, 0.56],
        Role::Brim => [0.18, 0.76, 0.62],
        Role::Support => [0.32, 0.80, 0.32],
        Role::SupportInterface => [0.20, 0.56, 0.26],
        Role::SupportTransition => [0.48, 0.72, 0.36],
        Role::PrimeTower => [0.78, 0.72, 0.56],
        Role::FloatingVerticalShell => [0.56, 0.56, 0.78],
        Role::Flush => [0.86, 0.74, 0.48],
        Role::Custom => [0.50, 0.80, 0.80],
    };
    [c[0], c[1], c[2], 1.0]
}

static PREVIEW_IDS: AtomicU64 = AtomicU64::new(1);

/// Where the idle nozzle waits next to the printing one: the toolhead's
/// nominal geometry (the printer calibrates the real offset; the slicer
/// writes both in the same coordinates). The idle nozzle stands above the
/// printing one by the nozzles' difference in printable height.
#[derive(Clone, Copy)]
struct Toolhead {
    /// X of the right nozzle minus X of the left one (mm).
    offset_x: f32,
    lift: f32,
}

impl Toolhead {
    fn of(machine: &Machine) -> Toolhead {
        let off = match machine.nozzles.as_slice() {
            [l, r, ..] => (r.reach.min[0] - l.reach.min[0]) as f32,
            _ => 0.0,
        };
        let lift = match machine.nozzles.as_slice() {
            [l, r, ..] => (r.max_height - l.max_height).abs() as f32,
            _ => 0.0,
        };
        Toolhead { offset_x: off, lift }
    }
}

/// A stretch of the print no bead draws: a record that lays nothing or leaves the head where a
/// bead has it (a retract or unretract, a wait, a seam mark, a printer routine, a pause, a
/// filament change, an extrusion that lays nothing), with the time model's times.
#[derive(Clone, Copy, Debug, PartialEq)]
struct GapRow {
    t0: f64,
    t1: f64,
    p0: [f32; 3],
    p1: [f32; 3],
    kind: MoveKind,
    role: Role,
    flags: u16,
    /// The commanded speed of an extrusion that lays nothing (mm/s), else none.
    speed: f32,
}

impl GapRow {
    fn of(m: &MoveRecord) -> GapRow {
        GapRow { t0: m.t_start, t1: m.t_end, p0: m.p0, p1: m.p1, kind: m.kind, role: m.role, flags: m.flags, speed: if m.kind == MoveKind::Extrude { m.f_cmd } else { 0.0 } }
    }
}

/// What the head holds from `t` on, as the records say: the nozzles' targets (an entry of the
/// temperature table), the bed's, the part fan, the filament and the nozzle printing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct HeadState {
    t: f64,
    temps: u32,
    bed: i16,
    fan: u8,
    filament: u8,
    nozzle: u8,
}

impl HeadState {
    fn of(m: &MoveRecord) -> HeadState {
        HeadState { t: m.t_start, temps: m.temps, bed: m.t_bed, fan: m.fan_part, filament: m.filament, nozzle: m.nozzle }
    }

    /// Whether it holds what `other` holds, whenever each starts.
    fn holds(&self, other: &HeadState) -> bool {
        (self.temps, self.bed, self.fan, self.filament, self.nozzle) == (other.temps, other.bed, other.fan, other.filament, other.nozzle)
    }
}

/// The preview's playback: where the head is and what it does at a time. Along a bead it moves
/// at one pace from the bead's start to its end; the stretches no bead draws are [`GapRow`]s,
/// and what it holds (temperatures, fan, filament, nozzle) [`HeadState`]s.
struct BeadTimeline {
    beads: BeadStreams,
    travel: RetainedInstances,
    rows: Vec<GapRow>,
    states: Vec<HeadState>,
    /// Where each part's frame sits, for a framed preview's beads ([`PreviewBuilder::part_offsets`]).
    part_offsets: Arc<[[f32; 2]]>,
    head: Toolhead,
    dual: bool,
    /// The machine's nozzles.
    heads: usize,
    /// The nozzles' targets the states' `temps` name.
    temps: Arc<Vec<Vec<i16>>>,
}

/// What plays at a time: a bead's instance or a row.
enum Playing {
    Bead([f32; PREVIEW_SLOTS]),
    Row(GapRow),
}

impl Playing {
    /// When it starts and ends (s): a bead as the shader takes it.
    fn span(&self) -> (f64, f64) {
        match self {
            Playing::Bead(s) => (fab::print::bead::t0(s) as f64, fab::print::bead::t1(s) as f64),
            Playing::Row(r) => (r.t0, r.t1),
        }
    }
}

fn doing(kind: MoveKind, role: Role, flags: u16) -> String {
    match kind {
        MoveKind::Extrude => role.name().to_string(),
        MoveKind::Travel => "Travel".into(),
        MoveKind::Retract => "Retract".into(),
        MoveKind::Unretract => "Unretract".into(),
        MoveKind::Wipe => "Wipe".into(),
        MoveKind::ZHop => "Z hop".into(),
        MoveKind::Dwell => "Wait".into(),
        MoveKind::FirmwareMacro if flags & flags::NOZZLE_CHANGE != 0 => "Nozzle change".into(),
        MoveKind::FirmwareMacro => "Printer routine".into(),
        MoveKind::Seam => "Seam".into(),
        MoveKind::Pause => "Pause".into(),
        MoveKind::ColorChange => "Filament change".into(),
    }
}

/// What a bead's move does: its role, or the move that prints nothing.
fn bead_doing(s: &[f32]) -> String {
    let k = fab::print::bead::kind(s) % kind::MODE_STRIDE;
    let id = stack::role(fab::print::bead::role(s)) as u8;
    match k {
        k if k == kind::EXTRUDE => Role::ALL.iter().find(|r| **r as u8 == id).map_or(Role::None, |r| *r).name().to_string(),
        k if k == kind::WIPE => "Wipe".into(),
        k if k == kind::ZHOP => "Z hop".into(),
        _ => "Travel".into(),
    }
}

/// `p0` to `p1` at `f` (0..1) of the way, `p1` itself at the end.
fn along(p0: [f32; 3], p1: [f32; 3], f: f32) -> [f32; 3] {
    if f >= 1.0 { p1 } else { std::array::from_fn(|i| p0[i] + (p1[i] - p0[i]) * f) }
}

/// How far (0..1) `t` is into `t0..t1`: at its end when it takes no time.
fn fraction(t: f64, t0: f64, t1: f64) -> f32 {
    if t1 > t0 { ((t - t0) / (t1 - t0)).clamp(0.0, 1.0) as f32 } else { 1.0 }
}

impl BeadTimeline {
    /// What plays at `t`. The beads and travel moves go one after another: of them, the latest
    /// to start by then (a bead ends where the next starts but for its times' rounding, so an
    /// earlier one never plays again). Of it and the latest row to start by then, the later to
    /// start plays while it runs, else the other while that runs (a row may sit inside a bead's
    /// stretch: a wait, a seam mark the bead goes over; the bead plays on after it), else the
    /// later to end holds at its end. A bead goes before a row that starts with it.
    fn playing(&self, t: f64) -> Option<Playing> {
        let started = [
            self.beads.last_started(t),
            fab::print::started_by(&self.travel, t).checked_sub(1).and_then(|i| fab::print::instance(&self.travel, i)),
        ];
        let bead = started.into_iter().flatten()
            .max_by(|a, b| (fab::print::bead::t0(a), fab::print::bead::t1(a)).partial_cmp(&(fab::print::bead::t0(b), fab::print::bead::t1(b))).expect("bead times are numbers"));
        let row = self.rows.partition_point(|r| r.t0 <= t).checked_sub(1).map(|i| self.rows[i]);
        Some(match (bead, row) {
            (Some(b), Some(r)) => {
                let (start, end) = (fab::print::bead::t0(&b) as f64, fab::print::bead::t1(&b) as f64);
                match (t < end, t < r.t1) {
                    (true, true) if r.t0 > start => Playing::Row(r),
                    (true, _) => Playing::Bead(b),
                    (false, true) => Playing::Row(r),
                    (false, false) if r.t1 >= end => Playing::Row(r),
                    (false, false) => Playing::Bead(b),
                }
            }
            (Some(b), None) => Playing::Bead(b),
            (None, r) => Playing::Row(r?),
        })
    }
}

impl PrintTimeline for BeadTimeline {
    fn now(&self, t: f64) -> PrintNow {
        let Some(playing) = self.playing(t) else { return PrintNow::default() };
        let state = self.states.get(self.states.partition_point(|s| s.t <= t).saturating_sub(1)).copied().unwrap_or_default();
        let (t0, t1) = playing.span();
        let (pos, doing, speed) = match &playing {
            Playing::Bead(s) => {
                let mut p = along([s[0], s[1], s[2]], [s[4], s[5], s[6]], fraction(t, t0, t1));
                if let Some(o) = fab::print::bead_part(s).and_then(|part| self.part_offsets.get(part)) {
                    p[0] += o[0];
                    p[1] += o[1];
                }
                (p, bead_doing(s), fab::print::bead::value(s))
            }
            Playing::Row(r) => (along(r.p0, r.p1, fraction(t, t0, t1)), doing(r.kind, r.role, r.flags), r.speed),
        };
        let active = (state.nozzle as usize).min(1);
        let mut nozzles = [pos, pos];
        if self.dual {
            let other = 1 - active;
            let dx = if active == 0 { self.head.offset_x } else { -self.head.offset_x };
            nozzles[other] = [pos[0] + dx, pos[1], pos[2] + self.head.lift];
        }
        PrintNow {
            nozzles,
            active,
            head: state.nozzle as usize,
            doing,
            filament: state.filament,
            // Two at least, as a two-nozzle head's line reads them.
            nozzle_temps: {
                let targets = self.temps.get(state.temps as usize).map_or(&[][..], Vec::as_slice);
                (0..self.heads.max(2)).map(|n| targets.get(n).copied().unwrap_or(0)).collect()
            },
            bed_temp: state.bed,
            fan: state.fan,
            speed,
        }
    }
}

/// The moves as preview instances (arcs as chords, [`CHORD_SAGITTA`]),
/// with the layer index, legend and summary.
#[cfg(test)]
pub fn build_preview(
    moves: &[MoveRecord],
    timeline: &TimeModel,
    filament_colors: Vec<[f32; 4]>,
    machine: &Machine,
    stats: &makepad_fab_gcode::writer::Stats,
) -> Arc<PrintPreview> {
    let mut b = PreviewBuilder::new(machine);
    b.feed(moves);
    b.preview(Some(timeline), ToolColours { model: filament_colors.clone(), material: filament_colors, ..Default::default() }, stats)
}

/// The preview of a settled or timed slice: its records stay the layer timeline's. A live
/// slice's (`part_offsets` not empty: where each part's frame sits) has its parts' beads in their
/// frames, each part's layer its own segments, for the live view's tiles to share.
fn build_preview_of_layers(
    layers: &makepad_fab_gcode::layers::LayerTimeline,
    timeline: &TimeModel,
    colours: ToolColours,
    machine: &Machine,
    stats: &makepad_fab_gcode::writer::Stats,
    part_offsets: Arc<[[f32; 2]]>,
) -> Arc<PrintPreview> {
    let mut b = PreviewBuilder::new(machine);
    b.temps = layers.nozzle_temps.clone();
    b.part_offsets = part_offsets;
    for (chunk, &t0) in layers.chunks.iter().zip(&layers.start) {
        b.place(t0, [0.0; 2], None, chunk.moves.iter());
    }
    b.preview(Some(timeline), colours, stats)
}

/// What Print commits about a slice, besides its package; `accepted`: the printability errors the
/// person printed it anyway with.
fn job_meta(sliced: &Sliced, destination: &PrinterChoice, snapshot: Option<&fab_printer_link::Snapshot>, options: &fab_printer_link::PrintOptions, accepted: Vec<jobs::Accepted>) -> JobMeta {
    let (r, created) = (&sliced.settled, jobs::wall_now());
    JobMeta {
        id: JobMeta::new_id(&sliced.name, created),
        printer: destination.id(),
        virtual_printer: matches!(destination, PrinterChoice::Virtual(_)) || snapshot.is_some_and(|p| p.virtual_printer),
        serial_suffix: snapshot.and_then(|p| p.serial_suffix.clone()),
        name: sliced.name.clone(),
        created,
        planned: sliced.preview.total,
        prepare: r.stats.prepare_time,
        layers: sliced.preview.max_layer() + 1,
        max_z: r.stats.max_z,
        tool_changes: r.timeline.tool_changes,
        filament_mm: r.stats.filament_mm.clone(),
        filament_g: r.stats.filament_g.clone(),
        used: sliced.used.clone(),
        trays: sliced.trays.clone(),
        colours: sliced.print_colours.clone(),
        unknown_spool_tools: sliced.preview.colours.material.iter().zip(&sliced.print_colours).enumerate()
            .filter_map(|(i, (shown, printed))| (*shown == unmapped_colour() && shown != printed).then_some(i)).collect(),
        model_colours: sliced.preview.colours.model.clone(),
        shared_colour_tools: sliced.preview.colours.shared_tools.clone(),
        labels: r.stats.label_ids.clone(),
        flow_calibration: options.flow_calibration,
        timelapse: options.timelapse,
        head_offset_x: sliced.head.offset_x,
        head_lift: sliced.head.lift,
        dual: sliced.dual,
        printer_model: sliced.printer_model.clone(),
        machine_preset: system_machine(&r.setup).to_string(),
        filament_presets: r.setup.filaments.iter().map(|f| f.name.clone()).collect(),
        accepted,
        pauses: sliced.preview.pauses.clone(),
        melts: sliced.preview.melts.clone(),
    }
}

/// A job's preview, from its package: the G-code through the time model the writer feeds every
/// line through, then the same preview build as any slice. So it is what the printer has.
fn preview_of_package(package: &[u8], meta: &JobMeta) -> Result<Arc<PrintPreview>, String> {
    let gcode = fab_printer_link::package_gcode(&meta.printer_model, package)?;
    let text = std::str::from_utf8(&gcode).map_err(|_| "the G-code is not text".to_string())?;
    let mut cfg = Config::new();
    for (key, value) in fab_printer_link::safety::header_config(text) {
        let value = makepad_fab_settings::config::parse_text(&key, &value);
        cfg.set(&key, value);
    }
    let heads = cfg.vec_len("nozzle_diameter");
    let mut model = makepad_fab_gcode::process::Processor::new(&makepad_fab_settings::machine::Job::new(cfg));
    model.set_object_labels(&meta.labels);
    for line in text.lines() {
        model.line(line);
    }
    let mut timeline = model.finish();
    let stats = makepad_fab_gcode::writer::Stats {
        total_time: meta.planned,
        prepare_time: meta.prepare,
        model_time: meta.planned - meta.prepare,
        layers: meta.layers as usize,
        max_z: meta.max_z,
        filament_mm: meta.filament_mm.clone(),
        filament_mm3: Vec::new(),
        filament_g: meta.filament_g.clone(),
        used: meta.used.clone(),
        label_ids: meta.labels.clone(),
        first_layer_time: 0.0,
    };
    let mut b = PreviewBuilder::with_head(Toolhead { offset_x: meta.head_offset_x, lift: meta.head_lift }, meta.dual, heads);
    b.temps = Arc::new(std::mem::take(&mut timeline.nozzle_temps));
    for part in std::mem::take(&mut timeline.parts) {
        b.feed(&part);
    }
    let mut material = meta.colours.clone();
    for &slot in &meta.unknown_spool_tools {
        if let Some(c) = material.get_mut(slot) { *c = unmapped_colour(); }
    }
    Ok(b.preview(Some(&timeline), ToolColours { material, model: meta.model_colours.clone(), shared_tools: meta.shared_colour_tools.clone() }, &stats))
}

/// A queued job's package from the disk, checked against the printer's installed profile.
fn job_check(catalog: &Catalog, dir: &Path, id: &str, meta: &JobMeta, head_envelope: &Path) -> Result<fab_printer_link::safety::ValidatedJob, String> {
    let package = jobs::read_package(dir, id).map_err(|e| format!("Fab's copy of this job can't be read: {e}"))?;
    let filaments = meta.filament_presets.iter().map(|name| catalog.store.resolve(name)).collect::<Result<Vec<_>, _>>()?;
    let profile = gate_profile_of(catalog, &meta.machine_preset, &filaments, head_envelope)?;
    fab_printer_link::safety::validate_job(package, &profile, &meta.name).map_err(|e| e.message())
}

fn remove_job_files(dir: PathBuf, id: String) {
    crate::live_slice::run_shown("fab job remove", move || {
        if let Err(e) = jobs::remove(&dir, &id) { log!("fab jobs: {id} could not be removed: {e}"); }
        #[cfg(test)]
        Cx::post_action(PrintEvent::JobFilesRemoved(id));
    });
}

fn connected(snapshot: &fab_printer_link::Snapshot) -> Result<(), String> {
    if snapshot.status == fab_printer_link::ConnectionStatus::Connected {
        return Ok(());
    }
    Err(format!("The printer is not connected: {}.", snapshot.status.label()))
}

/// Why the printer can't start a job now, whichever one.
fn printer_can_start(snapshot: &fab_printer_link::Snapshot, options: &fab_printer_link::PrintOptions) -> Result<(), String> {
    connected(snapshot)?;
    if snapshot.job.as_ref().is_some_and(|j| j.stage.active()) {
        return Err("A job is already on its way to the printer.".into());
    }
    if let Some(why) = fab_printer_link::job::printer_ready(&snapshot.state) { return Err(why); }
    if let Some(last @ ("FINISH" | "FAILED")) = snapshot.state.gcode_state.as_deref() {
        if !options.plate_clear {
            return Err(format!("Confirm the plate is clear first: the printer shows its last print as {}.", if last == "FINISH" { "finished" } else { "failed" }));
        }
    }
    Ok(())
}

/// Why this printer's trays and report don't fit the checked file: every used tool needs a loaded
/// tray, and nozzles, materials and spool ratings must agree.
fn trays_fit(snapshot: &fab_printer_link::Snapshot, facts: &fab_printer_link::safety::JobFacts, trays: &[Option<TrayId>], used: &[usize], options: &fab_printer_link::PrintOptions) -> Result<(), String> {
    for &i in used {
        let tray = trays.get(i).copied().flatten().ok_or_else(|| format!("Material {} has no tray: pick one in the Colours list.", i + 1))?;
        if !snapshot.state.trays().any(|(_, t)| t.id == Some(tray) && t.present != Some(false)) {
            return Err(format!("Tray {} is empty or not on {}: load it, or pick another tray in the Colours list.", colours::tray_text(tray), snapshot.name));
        }
    }
    fab_printer_link::job::job_fits(&snapshot.state, facts, trays, used, options).map_or(Ok(()), Err)
}

/// What a preview sums over its moves.
struct PreviewSums {
    bounds: [[f32; 3]; 2],
    speed: (f32, f32),
    role_mm: std::collections::HashMap<u8, f64>,
    role_s: std::collections::BTreeMap<u8, f64>,
    modes: Vec<fab::print::ModeStats>,
    max_z_rate: f32,
}

impl Default for PreviewSums {
    fn default() -> Self {
        PreviewSums { bounds: [[f32::MAX; 3], [f32::MIN; 3]], speed: (f32::MAX, 0.0), role_mm: Default::default(), role_s: Default::default(), modes: Vec::new(), max_z_rate: 0.0 }
    }
}

/// The newest instance grows over `m` when `m` continues it (the same look, joined end to end)
/// and every point it passes stays within [`CHORD_SAGITTA`] of the longer chord: a curved wall
/// sliced into tiny moves is drawn with the beads the eye can tell apart. `mid` is the middle of
/// an arc `m` drawn as one chord.
fn extend_bead(data: &mut [f32], run: &mut Vec<[f32; 3]>, m: &MoveRecord, mid: Option<[f32; 3]>, look: &[f32; PREVIEW_SLOTS]) -> bool {
    /// Bounds the check per move.
    const MAX_RUN: usize = 64;
    let Some(start) = data.len().checked_sub(PREVIEW_SLOTS) else { return false };
    if run.is_empty() || run.len() >= MAX_RUN || run.last() != Some(&m.p0) {
        return false;
    }
    let bead = &mut data[start..];
    if !fab::print::bead::same_look(bead, look) {
        return false;
    }
    let a = [bead[0], bead[1], bead[2]];
    let d = [m.p1[0] - a[0], m.p1[1] - a[1], m.p1[2] - a[2]];
    let dd = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
    let off_chord = |p: &[f32; 3]| {
        let v = [p[0] - a[0], p[1] - a[1], p[2] - a[2]];
        let f = if dd > 0.0 { ((v[0] * d[0] + v[1] * d[1] + v[2] * d[2]) / dd).clamp(0.0, 1.0) } else { 0.0 };
        let e = [v[0] - d[0] * f, v[1] - d[1] * f, v[2] - d[2] * f];
        e[0] * e[0] + e[1] * e[1] + e[2] * e[2] > CHORD_SAGITTA * CHORD_SAGITTA
    };
    if run.iter().chain(&mid).any(off_chord) {
        return false;
    }
    bead[4..7].copy_from_slice(&m.p1);
    run.extend(mid);
    run.push(m.p1);
    true
}

/// The newest instance's duration, to `t_end` where moves extended it ([`extend_bead`]): packed
/// once it takes no more.
fn close_bead(data: &mut [f32], extended: &mut Option<f64>) {
    if let Some(t_end) = extended.take() {
        let start = data.len() - PREVIEW_SLOTS;
        let bead = &mut data[start..];
        let dur = t_end as f32 - bead[3];
        fab::print::bead::set_dur(bead, dur);
    }
}

/// How far (mm) a preview chord may leave its arc or the moves it stands for.
const CHORD_SAGITTA: f32 = 0.02;

/// The chords an arc move draws with and its point at a fraction of the sweep. Chords within a
/// twentieth of a bead of the arc: small radii stay round, big ones take long chords, and an
/// arc that short takes one.
fn arc_chords(m: &MoveRecord) -> (usize, impl Fn(f32) -> [f32; 3] + '_) {
    let r = ((m.p0[0] - m.center[0]).powi(2) + (m.p0[1] - m.center[1]).powi(2)).sqrt();
    let chord = (8.0 * r * CHORD_SAGITTA).sqrt().max(0.1);
    let n = ((m.sweep.abs() * r / chord).ceil() as usize).clamp(1, 128);
    let a0 = (m.p0[1] - m.center[1]).atan2(m.p0[0] - m.center[0]);
    (n, move |f: f32| [m.center[0] + r * (a0 + m.sweep * f).cos(), m.center[1] + r * (a0 + m.sweep * f).sin(), m.p0[2] + (m.p1[2] - m.p0[2]) * f])
}

/// One part's layer records, made for its tile.
pub struct StreamRecords {
    pub moves: Vec<MoveRecord>,
    pub shift: [f32; 2],
    pub object: u16,
}

/// One layer's beads at each part's placement, independent of the playback clock, the
/// surface's, the lining's, then the core's, and how many of them are in each.
pub fn layer_beads(machine: &Machine, parts: Vec<StreamRecords>) -> (Vec<f32>, [u32; 3]) {
    let mut builder = PreviewBuilder::new(machine);
    for part in parts {
        builder.place(0.0, part.shift, Some(part.object), part.moves.into_iter());
        builder.close_run();
    }
    builder.flush();
    let (beads, _) = builder.published.as_mut().expect("flushed").publish();
    let counts = beads.0.each_ref().map(|s| s.count() as u32);
    let mut data: Vec<f32> = beads.each().flatten().copied().collect();
    for bead in data.chunks_exact_mut(PREVIEW_SLOTS) {
        bead[3] = 0.0;
        fab::print::bead::set_dur(bead, 0.0);
    }
    (data, counts)
}

/// The side of a wall a role prints: the outside (outer and overhang walls) or inside it.
fn wall_side(role: u8) -> Option<bool> {
    match role {
        r if r == Role::OuterWall as u8 || r == Role::OverhangWall as u8 => Some(true),
        r if r == Role::InnerWall as u8 => Some(false),
        _ => None,
    }
}

/// A wall bead: its side, where `bead` is a printed wall's.
fn wall_bead(bead: &[f32]) -> Option<bool> {
    (fab::print::bead::kind(bead) % kind::MODE_STRIDE == kind::EXTRUDE).then(|| wall_side(stack::role(fab::print::bead::role(bead)) as u8)).flatten()
}

/// Whether an instance draws as a move that prints nothing (travel, a wipe): the preview's
/// travel stream, not its beads.
fn travel_bead(bead: &[f32]) -> bool {
    let k = fab::print::bead::kind(bead) % kind::MODE_STRIDE;
    k == kind::TRAVEL || k == kind::WIPE || k == kind::ZHOP
}

/// What feeding records carries from one call to the next ([`PreviewBuilder::feed`]): the run of
/// instances being made, until its layer ends ([`PreviewBuilder::close_run`]).
struct OpenRun {
    /// Its layer: a record of another closes it.
    layer: Option<u32>,
    beads: Vec<f32>,
    /// The points the newest instance has swallowed so far (see `extend_bead`).
    run: Vec<[f32; 3]>,
    /// The newest extrusion's instance while extrusions go on from it: only adjacent extrusions
    /// join at their shared end.
    previous_extrude: Option<usize>,
    /// The last move's look and its packed lanes: the moves after it that look the same reuse them.
    looks: (Bead, [f32; PREVIEW_SLOTS]),
    /// The end time of the newest instance while moves extend it ([`close_bead`]).
    extended: Option<f64>,
    /// Each bead's end time as its records have it (s): its packed duration rounds, and the beads
    /// of its tips ([`fab::print::split_tips`]) take their shares of the time before that.
    ends: Vec<f64>,
    /// Its beads inside parts closed in on every side (`flags::CORE`) as stretches of bead
    /// indices, each the lining's or the core's: the publication puts them in their streams
    /// ([`PreviewBuilder::flush`]).
    inside: Vec<(std::ops::Range<u32>, BeadStream)>,
}

impl Default for OpenRun {
    fn default() -> Self {
        OpenRun { layer: None, beads: Vec::new(), run: Vec::new(), previous_extrude: None, looks: (Bead::default(), Bead::default().pack()), extended: None, ends: Vec::new(), inside: Vec::new() }
    }
}

impl OpenRun {
    /// The stream its newest bead goes in.
    fn newest_stream(&self) -> BeadStream {
        match self.inside.last() {
            Some((r, stream)) if r.end as usize * PREVIEW_SLOTS == self.beads.len() => *stream,
            _ => BeadStream::Surface,
        }
    }
}

/// The instances one run of records made (a layer's, or a part's on a layer), whatever their
/// kind, in the records' order: joining a bead to the one before reads this order.
struct PendingRun {
    beads: Vec<f32>,
    /// Its lining's and core's stretches ([`OpenRun::inside`]).
    inside: Vec<(std::ops::Range<u32>, BeadStream)>,
}

/// The floats of a published segment of preview instances (16 MB). A publication of a whole
/// print is many of them, each made once its share of the runs is in, so a big print's instances
/// are held once and this much more, not copied whole into one buffer.
const SEGMENT_FLOATS: usize = 1 << 22;

/// Instances gathered into segments of [`SEGMENT_FLOATS`] at most; `by_part_layer`: a segment
/// also holds only one part's run of beads on one layer, so a part's layer is whole segments
/// that another publication (a live tile) can hold by reference.
#[derive(Default)]
struct SegmentsBuilder {
    done: Vec<InstanceSegment>,
    open: Vec<f32>,
    by_part_layer: bool,
    /// The open segment's part and layer (its slot's bits).
    key: Option<(Option<usize>, u32)>,
}

impl SegmentsBuilder {
    fn by_part_layer() -> Self {
        SegmentsBuilder { by_part_layer: true, ..Default::default() }
    }

    fn push(&mut self, bead: &[f32]) {
        if self.by_part_layer {
            let key = Some((fab::print::bead_part(bead), fab::print::bead::layer(bead).to_bits()));
            if self.key != key {
                self.close();
                self.key = key;
            }
        }
        if self.open.len() + bead.len() > SEGMENT_FLOATS {
            self.close();
        }
        // A whole print's stream fills segments of the size; a part's layer takes its own.
        if self.open.capacity() == 0 && !self.by_part_layer {
            self.open.reserve_exact(SEGMENT_FLOATS);
        }
        self.open.extend_from_slice(bead);
    }

    fn close(&mut self) {
        if !self.open.is_empty() {
            self.done.push(InstanceSegment::new(std::mem::take(&mut self.open).into()));
        }
    }

    /// The instances so far, the open segment closed: the next go on in a segment of their own.
    fn publish(&mut self) -> RetainedInstances {
        self.close();
        RetainedInstances::from_segments(PREVIEW_SLOTS, self.done.clone()).expect("whole instances")
    }
}

/// A preview's layers, its length and how far it has grown ([`PrintPreview::grown`]), and its
/// playback's rows and holds ([`BeadTimeline`]).
struct Played {
    layers: Vec<PreviewLayer>,
    total: f64,
    grown: Option<f64>,
    rows: Vec<GapRow>,
    states: Vec<HeadState>,
}

/// What a builder has published of its runs ([`PreviewBuilder::flush`]): each stream's segments
/// and beads so far, the travel's, the layer entry of the newest bead and the runs of the beads.
struct Publication {
    streams: [SegmentsBuilder; 3],
    travel: SegmentsBuilder,
    counts: [u32; 3],
    /// The layer entry being published, and its layer as its beads hold it.
    at: Option<(usize, f32)>,
    blocks: fab::print::BlocksMaker,
}

impl Publication {
    /// `framed`: the beads in their parts' frames, a segment per part's run on a layer.
    fn new(framed: bool) -> Self {
        let segments = || if framed { SegmentsBuilder::by_part_layer() } else { SegmentsBuilder::default() };
        Publication { streams: std::array::from_fn(|_| segments()), travel: SegmentsBuilder::default(), counts: [0; 3], at: None, blocks: Default::default() }
    }

    /// The beads and travel so far.
    fn publish(&mut self) -> (BeadStreams, RetainedInstances) {
        (BeadStreams(self.streams.each_mut().map(SegmentsBuilder::publish)), self.travel.publish())
    }
}

/// The bounds (mm) of a [`WallLines`] grid's cell, which is twice the farthest its layer's walls
/// reach ([`STACK_REACH`] of their widest bead): small enough that a bead looks through a few
/// beads' worth of lines, and a fine nozzle's plate still has a small grid.
const WALL_CELL: (f32, f32) = (0.6, 2.4);
/// How far (in bead widths, at most half a cell) a wall in the next layer may stand beside a bead
/// and still be the layer above or below it in the wall: the inside's walls are a width or more
/// from the outside's, and the side is kept apart anyway.
const STACK_REACH: f32 = 1.0;
/// How far (in bead widths, at most half a cell) a bead of its own layer may lie beside a skin's
/// or a solid fill's and still hide part of its front: further, their sections stand apart.
const SIDE_REACH: f32 = 2.0;

/// One layer's walls as lines, gridded, so a bead of the layer below or above finds the wall
/// beside it ([`stack_walls`]).
pub struct WallLines {
    /// Start, unit direction, length, outside.
    lines: Vec<([f32; 2], [f32; 2], f32, bool)>,
    /// The cells' size (mm).
    cell: f32,
    origin: [i32; 2],
    size: [i32; 2],
    /// Per cell (row by row), its lines: `of[at[c]..at[c + 1]]`.
    at: Vec<u32>,
    of: Vec<u32>,
}

impl WallLines {
    /// The walls among one layer's moves, arcs by their preview chords.
    pub fn of_moves<'a>(moves: impl Iterator<Item = &'a MoveRecord>) -> Self {
        let (mut lines, mut width) = (Vec::new(), 0.0f32);
        for m in moves.filter(|m| m.kind == MoveKind::Extrude && m.e > 0.0 && m.width > 0.0) {
            let Some(outside) = wall_side(m.role as u8) else { continue };
            width = width.max(m.width);
            if m.flags & flags::ARC != 0 && m.sweep.abs() > 1e-6 {
                let (n, at) = arc_chords(m);
                for i in 0..n {
                    let (a, b) = (if i == 0 { m.p0 } else { at(i as f32 / n as f32) }, if i + 1 == n { m.p1 } else { at((i + 1) as f32 / n as f32) });
                    lines.push(([a[0], a[1]], [b[0], b[1]], outside));
                }
            } else {
                lines.push(([m.p0[0], m.p0[1]], [m.p1[0], m.p1[1]], outside));
            }
        }
        Self::grid(lines, width, STACK_REACH)
    }

    /// The walls among beads.
    fn of_beads<'a>(beads: impl Iterator<Item = &'a [f32]>) -> Self {
        let mut width = 0.0f32;
        let ends = beads.filter_map(|b| {
            let outside = wall_bead(b)?;
            width = width.max(fab::print::bead::section(b).0.abs());
            Some(([b[0], b[1]], [b[4], b[5]], outside))
        }).collect();
        Self::grid(ends, width, STACK_REACH)
    }

    /// Every printed bead among one layer's beads, for its skins' side neighbours ([`stack_sides`]).
    fn of_prints<'a>(beads: impl Iterator<Item = &'a [f32]>) -> Self {
        let mut width = 0.0f32;
        let ends = beads.filter(|b| fab::print::bead::kind(b) % kind::MODE_STRIDE == kind::EXTRUDE).map(|b| {
            width = width.max(fab::print::bead::section(b).0.abs());
            ([b[0], b[1]], [b[4], b[5]], false)
        }).collect();
        Self::grid(ends, width, SIDE_REACH)
    }

    /// `ends`' lines in cells of twice `reach` (in widths) of their widest bead, `width`.
    fn grid(ends: Vec<([f32; 2], [f32; 2], bool)>, width: f32, reach: f32) -> Self {
        let cell_mm = (2.0 * width * reach).clamp(WALL_CELL.0, WALL_CELL.1);
        let cell = |p: [f32; 2]| [(p[0] / cell_mm).floor() as i32, (p[1] / cell_mm).floor() as i32];
        let (mut lo, mut hi) = ([i32::MAX; 2], [i32::MIN; 2]);
        for (a, b, _) in &ends {
            for c in [cell(*a), cell(*b)] {
                lo = [lo[0].min(c[0]), lo[1].min(c[1])];
                hi = [hi[0].max(c[0]), hi[1].max(c[1])];
            }
        }
        if ends.is_empty() {
            return WallLines { lines: Vec::new(), cell: cell_mm, origin: [0; 2], size: [0; 2], at: vec![0], of: Vec::new() };
        }
        let size = [hi[0] - lo[0] + 1, hi[1] - lo[1] + 1];
        // Each line in the cells of its points half a cell apart: a point within `reach` of the
        // line is within `reach` and a quarter cell of one of them.
        let mut lines = Vec::with_capacity(ends.len());
        let mut pairs: Vec<(u32, u32)> = Vec::with_capacity(ends.len() * 2);
        for (i, (a, b, outside)) in ends.into_iter().enumerate() {
            let d = [b[0] - a[0], b[1] - a[1]];
            let len = (d[0] * d[0] + d[1] * d[1]).sqrt();
            let dir = if len > 1e-6 { [d[0] / len, d[1] / len] } else { [1.0, 0.0] };
            lines.push((a, dir, len, outside));
            let n = (len / (cell_mm * 0.5)).ceil() as usize;
            let mut last = u32::MAX;
            for k in 0..=n {
                let f = if n == 0 { 0.0 } else { k as f32 / n as f32 };
                let c = cell([a[0] + d[0] * f, a[1] + d[1] * f]);
                let c = ((c[1] - lo[1]) * size[0] + (c[0] - lo[0])) as u32;
                if c != last {
                    pairs.push((c, i as u32));
                    last = c;
                }
            }
        }
        pairs.sort_unstable();
        pairs.dedup();
        let mut at = vec![0u32; (size[0] * size[1]) as usize + 1];
        for &(c, _) in &pairs {
            at[c as usize + 1] += 1;
        }
        for c in 1..at.len() {
            at[c] += at[c - 1];
        }
        WallLines { lines, cell: cell_mm, origin: lo, size, at, of: pairs.into_iter().map(|p| p.1).collect() }
    }

    /// How far (mm) the nearest wall of this layer on the same side as a bead from `a` to `b`, and
    /// running with it, stands to its right; `None` if no wall stands within reach.
    fn beside(&self, a: [f32; 2], b: [f32; 2], width: f32, outside: bool) -> Option<f32> {
        let d = [b[0] - a[0], b[1] - a[1]];
        let len = (d[0] * d[0] + d[1] * d[1]).sqrt();
        if len < 1e-6 || self.lines.is_empty() {
            return None;
        }
        let dir = [d[0] / len, d[1] / len];
        let mid = [a[0] + d[0] * 0.5, a[1] + d[1] * 0.5];
        let reach = (width * STACK_REACH).min(self.cell * 0.5);
        // the cells a line's point within reach and a quarter cell of the bead's middle may be in
        let span = |k: usize| {
            let at = |v: f32| (v / self.cell).floor() as i32 - self.origin[k];
            let bound = reach + self.cell * 0.25;
            at(mid[k] - bound).max(0)..(at(mid[k] + bound) + 1).min(self.size[k])
        };
        let mut best: Option<(f32, f32)> = None;
        for y in span(1) {
            for x in span(0) {
                let k = (y * self.size[0] + x) as usize;
                for &i in &self.of[self.at[k] as usize..self.at[k + 1] as usize] {
                    let (p, ldir, llen, side) = self.lines[i as usize];
                    // within 30 degrees of the bead's run, either way round
                    if side != outside || (dir[0] * ldir[1] - dir[1] * ldir[0]).abs() > 0.5 {
                        continue;
                    }
                    let t = ((mid[0] - p[0]) * ldir[0] + (mid[1] - p[1]) * ldir[1]).clamp(0.0, llen);
                    let q = [p[0] + ldir[0] * t - mid[0], p[1] + ldir[1] * t - mid[1]];
                    let dist = (q[0] * q[0] + q[1] * q[1]).sqrt();
                    if dist <= reach && best.is_none_or(|(bd, _)| dist < bd) {
                        best = Some((dist, q[0] * dir[1] - q[1] * dir[0]));
                    }
                }
            }
        }
        best.map(|(_, right)| right)
    }

    /// How far (mm, across) the nearest line running with a bead from `a` to `b` lies to its right
    /// and to its left within [`SIDE_REACH`] of its width; a line in line with it (itself, the
    /// rest of its move) is none.
    fn sides(&self, a: [f32; 2], b: [f32; 2], width: f32) -> (Option<f32>, Option<f32>) {
        let d = [b[0] - a[0], b[1] - a[1]];
        let len = (d[0] * d[0] + d[1] * d[1]).sqrt();
        if len < 1e-6 || self.lines.is_empty() {
            return (None, None);
        }
        let dir = [d[0] / len, d[1] / len];
        let mid = [a[0] + d[0] * 0.5, a[1] + d[1] * 0.5];
        let reach = (width * SIDE_REACH).min(self.cell * 0.5);
        let span = |k: usize| {
            let at = |v: f32| (v / self.cell).floor() as i32 - self.origin[k];
            let bound = reach + self.cell * 0.25;
            at(mid[k] - bound).max(0)..(at(mid[k] + bound) + 1).min(self.size[k])
        };
        let (mut right, mut left): (Option<f32>, Option<f32>) = (None, None);
        for y in span(1) {
            for x in span(0) {
                let k = (y * self.size[0] + x) as usize;
                for &i in &self.of[self.at[k] as usize..self.at[k + 1] as usize] {
                    let (p, ldir, llen, _) = self.lines[i as usize];
                    // within 30 degrees of the bead's run, either way round
                    if (dir[0] * ldir[1] - dir[1] * ldir[0]).abs() > 0.5 {
                        continue;
                    }
                    let t = ((mid[0] - p[0]) * ldir[0] + (mid[1] - p[1]) * ldir[1]).clamp(0.0, llen);
                    let q = [p[0] + ldir[0] * t - mid[0], p[1] + ldir[1] * t - mid[1]];
                    let across = q[0] * dir[1] - q[1] * dir[0];
                    if (q[0] * q[0] + q[1] * q[1]).sqrt() > reach || across.abs() < width * 0.25 {
                        continue;
                    }
                    let side = if across > 0.0 { &mut right } else { &mut left };
                    if side.is_none_or(|s| across.abs() < s) {
                        *side = Some(across.abs());
                    }
                }
            }
        }
        (right, left)
    }
}

/// Each wall bead's steps to the walls of the layers below and above it (`fab::print::stack`):
/// where the beads beside it in the wall stand, from the lines of those layers' walls.
pub fn stack_walls<'a>(beads: impl Iterator<Item = &'a mut [f32]>, below: Option<&WallLines>, above: Option<&WallLines>) {
    for bead in beads {
        stack_wall(bead, below, above);
    }
}

/// Each skin's and solid fill's bead's side neighbours (`fab::print::stack`): where the beads of its
/// own layer (`beads`, one run's) lie beside it, right and left, from their lines. Once a run, on
/// the worker that builds it.
pub fn stack_sides(beads: &mut [f32]) {
    let skin = |b: &[f32]| fab::print::bead::kind(b) % kind::MODE_STRIDE == kind::EXTRUDE && stack::sides(stack::role(fab::print::bead::role(b)));
    if !beads.chunks_exact(PREVIEW_SLOTS).any(skin) {
        return;
    }
    let lines = WallLines::of_prints(beads.chunks_exact(PREVIEW_SLOTS));
    for bead in beads.chunks_exact_mut(PREVIEW_SLOTS).filter(|b| skin(b)) {
        let width = fab::print::bead::section(bead).0.abs();
        let (right, left) = lines.sides([bead[0], bead[1]], [bead[4], bead[5]], width);
        let side = |d: Option<f32>| d.map_or(stack::NONE, |d| stack::side(d, width));
        let role = stack::role(fab::print::bead::role(bead));
        fab::print::bead::set_role(bead, stack::pack(role, side(right), side(left)));
    }
}

fn stack_wall(bead: &mut [f32], below: Option<&WallLines>, above: Option<&WallLines>) {
    let Some(outside) = wall_bead(bead) else { return };
    let (a, b, width) = ([bead[0], bead[1]], [bead[4], bead[5]], fab::print::bead::section(bead).0.abs());
    let step = |lines: Option<&WallLines>| lines.and_then(|l| l.beside(a, b, width, outside)).map_or(stack::NONE, |o| stack::step(o, width));
    let (below, above) = (step(below), step(above));
    let role = stack::role(fab::print::bead::role(bead));
    fab::print::bead::set_role(bead, stack::pack(role, below, above));
}

/// Live tiles share their bead chunks (each built in its part's frame, `part_offsets` placing
/// it), each part's lowest and highest layer where it is whole (`PrintPreview::part_layers`);
/// the playback timeline stays empty.
pub fn live_preview(machine: &Machine, colours: ToolColours, layers: Vec<PreviewLayer>, chunks: Vec<fab::print::LiveChunk>, part_layers: Vec<[f32; 2]>, part_offsets: Arc<[[f32; 2]]>, part_objects: Arc<[String]>) -> Arc<PrintPreview> {
    let mut preview = PreviewBuilder::new(machine).preview(None, colours, &Default::default());
    let p = Arc::get_mut(&mut preview).expect("new preview");
    p.layers = layers;
    p.chunks = chunks;
    p.part_layers = part_layers;
    p.part_offsets = part_offsets;
    p.part_objects = part_objects;
    p.roles = Role::ALL.iter().map(|r| PreviewRole { id: *r as u8, name: r.name().to_string(), color: role_colour(*r), seconds: 0.0, length_mm: 0.0 }).collect();
    preview
}

/// Beads and playback metadata built from placed record chunks.
struct PreviewBuilder {
    head: Toolhead,
    dual: bool,
    heads: usize,
    /// The nozzles' targets the records' `temps` name (their timeline's table).
    temps: Arc<Vec<Vec<i16>>>,
    /// What is published of the runs: the printed beads, the travel and wipe moves.
    published: Option<Publication>,
    /// The runs made since, each a layer's or a part's ([`PendingRun`]), and the one being made.
    pending: Vec<PendingRun>,
    open: OpenRun,
    /// The runs of `pending` whose walls step to their neighbours ([`Self::stack_closed`]), and
    /// the wall lines of the last of them (its layer) and of the run after it, made before their
    /// beads were stacked.
    stacked: usize,
    stacked_lines: Option<(u32, Option<WallLines>)>,
    next_lines: Option<Option<WallLines>>,
    layers: Vec<PreviewLayer>,
    /// The stretches no bead draws and what the head holds, in time order ([`BeadTimeline`]).
    rows: Vec<GapRow>,
    states: Vec<HeadState>,
    t: f64,
    sums: PreviewSums,
    /// Per plate part, where its frame sits on the plate (mm): the published beads of a part go
    /// in its frame, a segment per run of its beads on a layer (the live view's tiles of a settled
    /// slice share them). Empty: every bead on the plate, as the records are.
    part_offsets: Arc<[[f32; 2]]>,
}

impl PreviewBuilder {
    pub fn new(machine: &Machine) -> Self {
        Self::with_head(Toolhead::of(machine), machine.nozzles.len() > 1, machine.nozzles.len())
    }

    fn with_head(head: Toolhead, dual: bool, heads: usize) -> Self {
        PreviewBuilder { head, dual, heads, temps: Arc::default(), published: None, pending: Vec::new(), open: OpenRun::default(), stacked: 0, stacked_lines: None, next_lines: None, layers: Vec::new(), rows: Vec::new(), states: Vec::new(), t: 0.0, sums: PreviewSums::default(), part_offsets: Arc::from([]) }
    }

    /// Records after everything before (`t0` on the preview's clock, `shift` along the plate),
    /// placed, then fed ([`Self::feed`]).
    fn place(&mut self, t0: f64, shift: [f32; 2], object: Option<u16>, moves: impl Iterator<Item = MoveRecord>) {
        // The summing and the instances read them placed.
        let placed: Vec<MoveRecord> = moves.map(|mut m| {
            m.t_start += t0;
            m.t_end += t0;
            m.p0[0] += shift[0];
            m.p0[1] += shift[1];
            m.p1[0] += shift[0];
            m.p1[1] += shift[1];
            m.center[0] += shift[0];
            m.center[1] += shift[1];
            if let Some(object) = object {
                m.object = object;
            }
            m
        }).collect();
        self.feed(&placed);
    }

    /// Sums, instances, rows and holds of records on the preview's clock, at their place, after
    /// everything before. A run of instances goes on from one call to the next and closes where
    /// its layer ends ([`Self::close_run`]), the walls of the one before it then stacked
    /// ([`Self::stack_closed`]): the instances are the same however the records come.
    fn feed(&mut self, moves: &[MoveRecord]) {
        for (i, m) in moves.iter().enumerate() {
            if self.open.layer != Some(m.layer) {
                self.close_run();
                self.stack_closed(false);
                self.open.layer = Some(m.layer);
                // At most an instance a move but for an arc's chords: reserved, not touched.
                self.open.beads.reserve(moves[i..].iter().take_while(|r| r.layer == m.layer).count() * PREVIEW_SLOTS);
            }
            let held = HeadState::of(m);
            if self.states.last().is_none_or(|h| !h.holds(&held)) {
                self.states.push(held);
            }
            let s = &mut self.sums;
            let o = &mut self.open;
            if m.kind == MoveKind::Extrude && m.e > 0.0 {
                *s.role_s.entry(m.role as u8).or_default() += m.t_end - m.t_start;
                let zr = if m.flags & flags::SLOPED != 0 {
                    let d = [m.p1[0] - m.p0[0], m.p1[1] - m.p0[1], m.p1[2] - m.p0[2]];
                    let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
                    if l > 1e-6 { m.f_cmd * d[2].abs() / l } else { 0.0 }
                } else { 0.0 };
                s.max_z_rate = s.max_z_rate.max(zr);
                let st = match s.modes.iter_mut().position(|x| x.mode == m.mode) {
                    Some(k) => &mut s.modes[k],
                    None => {
                        s.modes.push(fab::print::ModeStats { mode: m.mode, ..Default::default() });
                        s.modes.last_mut().unwrap()
                    }
                };
                st.time_s += m.t_end - m.t_start;
                st.filament_mm += m.e as f64;
                st.max_z_rate = st.max_z_rate.max(zr);
            }
            // A record that neither moves the head nor takes the filament back leaves the bead's
            // path whole: an unretract of what the G-code's resolution leaves of a segment (a
            // zigzag's turn on a curved edge), a dwell, a seam mark. A macro, pause or colour
            // change takes the head away.
            if matches!(m.kind, MoveKind::Unretract | MoveKind::Dwell | MoveKind::Seam) {
                self.rows.push(GapRow::of(m));
                continue;
            }
            let extrude = m.kind == MoveKind::Extrude && m.e > 0.0 && m.width > 0.0;
            // Inside a part closed in on every side: an inner wall its lining, a fill its core.
            let stream = match extrude && m.flags & flags::CORE != 0 && m.object != u16::MAX {
                false => BeadStream::Surface,
                true if m.role == Role::InnerWall => BeadStream::Lining,
                true => BeadStream::Core,
            };
            let mut continuing = o.previous_extrude.map_or(false, |start| {
                let bead = &o.beads[start..start + PREVIEW_SLOTS];
                extrude && start + PREVIEW_SLOTS == o.beads.len() && bead[4..7] == m.p0
                    && fab::print::bead::layer(bead) == m.layer as f32 && fab::print::bead::filament(bead) == m.filament as f32
            });
            if !continuing {
                if let Some(start) = o.previous_extrude.take() {
                    fab::print::bead::free_end(&mut o.beads[start..start + PREVIEW_SLOTS]);
                }
            }
            let k = match m.kind {
                MoveKind::Extrude if extrude => kind::EXTRUDE,
                MoveKind::Travel => kind::TRAVEL,
                MoveKind::ZHop => kind::ZHOP,
                MoveKind::Wipe => kind::WIPE,
                _ => {
                    // A retract, or an extrusion that lays nothing, breaks the extrusion run.
                    self.rows.push(GapRow::of(m));
                    o.run.clear();
                    continue;
                }
            };
            if self.layers.last().map_or(true, |l| l.layer != m.layer) {
                if let Some(l) = self.layers.last_mut() {
                    l.t_end = m.t_start;
                }
                self.layers.push(PreviewLayer { layer: m.layer, z: m.z_layer, t_start: m.t_start, t_end: f64::MAX, starts: [0; 3] });
            } else if let Some(l) = self.layers.last_mut() {
                // The first layer's first records are the printer's start sequence, on the plate.
                l.z = l.z.max(m.z_layer);
            }
            let (w, h) = if k == kind::EXTRUDE { (m.width, m.height.max(0.02)) } else { (0.12, 0.12) };
            if k == kind::EXTRUDE {
                for p in [m.p0, m.p1] {
                    for i in 0..3 {
                        s.bounds[0][i] = s.bounds[0][i].min(p[i]);
                        s.bounds[1][i] = s.bounds[1][i].max(p[i]);
                    }
                }
                let d = [m.p1[0] - m.p0[0], m.p1[1] - m.p0[1], m.p1[2] - m.p0[2]];
                let len = if m.flags & flags::ARC != 0 && m.sweep.abs() > 1e-6 {
                    (((m.p0[0] - m.center[0]).powi(2) + (m.p0[1] - m.center[1]).powi(2)).sqrt() * m.sweep.abs()) as f64
                } else {
                    ((d[0] * d[0] + d[1] * d[1] + d[2] * d[2]) as f64).sqrt()
                };
                *s.role_mm.entry(m.role as u8).or_default() += len;
                if m.f_cmd > 0.0 {
                    s.speed.0 = s.speed.0.min(m.f_cmd);
                    s.speed.1 = s.speed.1.max(m.f_cmd);
                }
            }
            let arc = (m.flags & flags::ARC != 0 && m.sweep.abs() > 1e-6).then(|| arc_chords(m));
            let one_chord = arc.as_ref().is_none_or(|(n, _)| *n == 1);
            let mid = arc.as_ref().filter(|_| one_chord).map(|(_, at)| at(0.5));
            // What the move's beads look like, their ends and times aside.
            let look = Bead {
                width: w,
                height: h,
                value: m.f_cmd,
                layer: m.layer as f32,
                role: m.role as u8 as f32,
                tag: fab::print::bead_object(m.nozzle, m.object),
                filament: m.filament as f32,
                kind: k + fab::print::kind::MODE_STRIDE * m.mode.min(8) as f32,
                ..Bead::default()
            };
            if o.looks.0 != look {
                o.looks = (look, look.pack());
            }
            let look = &o.looks.1;
            if one_chord && o.newest_stream() == stream && extend_bead(&mut o.beads, &mut o.run, m, mid, look) {
                o.extended = Some(m.t_end);
                *o.ends.last_mut().expect("the bead it extends") = m.t_end;
                continue;
            }
            close_bead(&mut o.beads, &mut o.extended);
            let data = &mut o.beads;
            let previous_extrude = &mut o.previous_extrude;
            let inside = &mut o.inside;
            let ends = &mut o.ends;
            let mut push = |a: [f32; 3], b: [f32; 3], ta: f64, tb: f64| {
                // Negative width/height mark free starts/ends; magnitudes stay physical.
                let free_start = extrude && !continuing;
                if extrude {
                    *previous_extrude = Some(data.len());
                    continuing = true;
                }
                if stream != BeadStream::Surface {
                    let i = (data.len() / PREVIEW_SLOTS) as u32;
                    match inside.last_mut() {
                        Some((r, s)) if r.end == i && *s == stream => r.end = i + 1,
                        _ => inside.push((i..i + 1, stream)),
                    }
                }
                data.extend_from_slice(&fab::print::bead::placed(look, a, b, ta as f32, (tb - ta) as f32, free_start));
                ends.push(tb);
            };
            if let Some((n, at)) = &arc {
                let n = *n;
                let dt = m.t_end - m.t_start;
                for i in 0..n {
                    let (fa, fb) = (i as f32 / n as f32, (i + 1) as f32 / n as f32);
                    push(if i == 0 { m.p0 } else { at(fa) }, if i + 1 == n { m.p1 } else { at(fb) }, m.t_start + dt * fa as f64, m.t_start + dt * fb as f64);
                }
            } else {
                push(m.p0, m.p1, m.t_start, m.t_end);
            }
            o.run.clear();
            if one_chord {
                o.run.extend(mid);
                o.run.push(m.p1);
            }
        }
        self.t = self.t.max(moves.last().map_or(0.0, |m| m.t_end));
    }

    /// Closes the open run ([`OpenRun`]): its newest instance's duration, its last extrusion's
    /// free end, its tips' own beads ([`fab::print::split_tips`]), its joints, its skins' side
    /// neighbours ([`stack_sides`]). It waits for the publish with the runs before it.
    fn close_run(&mut self) {
        let o = &mut self.open;
        close_bead(&mut o.beads, &mut o.extended);
        if let Some(start) = o.previous_extrude.take() {
            fab::print::bead::free_end(&mut o.beads[start..start + PREVIEW_SLOTS]);
        }
        fab::print::split_tips(&mut o.beads, &o.ends, &mut o.inside);
        fab::print::join_beads(&mut o.beads);
        stack_sides(&mut o.beads);
        o.ends.clear();
        o.run.clear();
        o.layer = None;
        if !o.beads.is_empty() {
            self.pending.push(PendingRun { beads: std::mem::take(&mut o.beads), inside: std::mem::take(&mut o.inside) });
        }
    }

    /// The steps between the layers of the walls closed so far ([`stack_walls`]): a run's walls
    /// step to the layers below and above it, so each waits for the run after it, but the newest
    /// at the end (`all`). The feed's runs are its layers in order, so each is stacked as it would
    /// be with the whole print's walls at hand, one layer behind the feed.
    fn stack_closed(&mut self, all: bool) {
        let lines = |run: &PendingRun| run.beads.chunks_exact(PREVIEW_SLOTS).any(|b| wall_bead(b).is_some()).then(|| WallLines::of_beads(run.beads.chunks_exact(PREVIEW_SLOTS)));
        let layer = |run: &PendingRun| fab::print::bead::layer(&run.beads) as u32;
        let end = if all { self.pending.len() } else { self.pending.len().saturating_sub(1) };
        while self.stacked < end {
            let k = self.stacked;
            let here = self.next_lines.take().unwrap_or_else(|| lines(&self.pending[k]));
            let n = layer(&self.pending[k]);
            let next = self.pending.get(k + 1).map(|run| (layer(run), lines(run)));
            debug_assert!(next.as_ref().is_none_or(|(l, _)| *l > n), "the feed's runs are its layers, in order");
            let below = self.stacked_lines.as_ref().filter(|(l, _)| l + 1 == n).and_then(|(_, w)| w.as_ref());
            let above = next.as_ref().filter(|(l, _)| *l == n + 1).and_then(|(_, w)| w.as_ref());
            stack_walls(self.pending[k].beads.chunks_exact_mut(PREVIEW_SLOTS), below, above);
            self.stacked_lines = Some((n, here));
            self.next_lines = next.map(|(_, w)| w);
            self.stacked += 1;
        }
    }

    /// The runs made since the last publish, split by what draws them and appended to the
    /// published ones ([`Self::publish_runs`]).
    fn flush(&mut self) {
        self.publish_runs(self.pending.len());
    }

    /// The first `runs` of the runs made since the last publish, split by what draws them and
    /// appended to the published ones. Each push's run goes into segments as it is read
    /// ([`SegmentsBuilder`]): the instances are never held twice, whatever the print's size. The
    /// beads go in three streams, each in print order: the surface, the lining, the core
    /// ([`OpenRun::inside`]); each layer's entry in `layers` says where its beads start in each.
    fn publish_runs(&mut self, runs: usize) {
        let part_offsets = &self.part_offsets;
        let p = self.published.get_or_insert_with(|| Publication::new(!part_offsets.is_empty()));
        let mut in_frame = [0.0f32; PREVIEW_SLOTS];
        for run in self.pending.drain(..runs) {
            let mut inside = run.inside.iter().peekable();
            for (i, bead) in run.beads.chunks_exact(PREVIEW_SLOTS).enumerate() {
                // A layer entry starts where the beads' layer changes, as `feed` made them.
                let layer = fab::print::bead::layer(bead);
                if p.at.is_none_or(|(_, at)| layer != at) {
                    let a = p.at.map_or(0, |(a, _)| a + 1);
                    debug_assert!(self.layers.get(a).is_some_and(|l| l.layer as f32 == layer), "the beads' layers follow the layer entries");
                    self.layers[a].starts = p.counts;
                    p.at = Some((a, layer));
                }
                let i = i as u32;
                while inside.peek().is_some_and(|(r, _)| r.end <= i) {
                    inside.next();
                }
                let stream = inside.peek().filter(|(r, _)| r.start <= i).map_or(BeadStream::Surface, |(_, s)| *s);
                // A part's bead into its frame, where the shader's part offset places it back.
                let bead = match fab::print::bead_part(bead).and_then(|p| part_offsets.get(p)) {
                    Some(o) => {
                        in_frame.copy_from_slice(bead);
                        for k in [0, 4] {
                            in_frame[k] -= o[0];
                            in_frame[k + 1] -= o[1];
                        }
                        &in_frame[..]
                    }
                    None => bead,
                };
                if travel_bead(bead) {
                    p.travel.push(bead);
                } else {
                    p.streams[stream as usize].push(bead);
                    p.counts[stream as usize] += 1;
                    p.blocks.push(stream, bead, part_offsets);
                }
            }
        }
        // The layers after the newest bead start past every bead so far.
        for l in &mut self.layers[p.at.map_or(0, |(a, _)| a + 1)..] {
            l.starts = p.counts;
        }
        // The runs published unstacked (a tile's) are never stacked.
        self.stacked = self.stacked.saturating_sub(runs);
    }

    /// The playback of `beads` and `travel` ([`BeadTimeline`]) with the rows and holds `rows`
    /// and `states`.
    fn playback(&self, beads: BeadStreams, travel: RetainedInstances, rows: Vec<GapRow>, states: Vec<HeadState>) -> BeadTimeline {
        BeadTimeline {
            beads,
            travel,
            rows,
            states,
            part_offsets: self.part_offsets.clone(),
            head: self.head,
            dual: self.dual,
            heads: self.heads,
            temps: self.temps.clone(),
        }
    }

    /// The preview of everything pushed so far, its walls' beads with their steps to the layers
    /// above and below (`stack_closed`). `timeline`: the time model's (a finished slice) for the
    /// feature times and tool changes, else the records' own times.
    pub fn preview(mut self, timeline: Option<&TimeModel>, colours: ToolColours, stats: &makepad_fab_gcode::writer::Stats) -> Arc<PrintPreview> {
        self.close_run();
        self.stack_closed(true);
        self.flush();
        let total = self.t;
        let mut layers = std::mem::take(&mut self.layers);
        if let Some(l) = layers.last_mut() {
            l.t_end = total;
        }
        let played = Played { layers, total, grown: None, rows: std::mem::take(&mut self.rows), states: std::mem::take(&mut self.states) };
        self.made(timeline, colours, stats, played)
    }

    /// The preview of what is published so far ([`Self::publish_runs`]), playing to the end of
    /// its layers, of the print `stats` tells ([`PrintPreview::grown`]).
    fn grown_preview(&mut self, timeline: Option<&TimeModel>, colours: ToolColours, stats: &makepad_fab_gcode::writer::Stats) -> Arc<PrintPreview> {
        // The published layers: the newest of them is whole, its runs a layer's.
        let shown = self.published.as_ref().and_then(|p| p.at).map_or(0, |(a, _)| a + 1);
        let layers = self.layers[..shown].to_vec();
        let grown = Some(layers.last().map_or(0.0, |l| l.t_end));
        let played = Played { layers, total: stats.total_time, grown, rows: self.rows.clone(), states: self.states.clone() };
        self.made(timeline, colours, stats, played)
    }

    /// The preview of what is published, as `played` lays it out.
    fn made(&mut self, timeline: Option<&TimeModel>, colours: ToolColours, stats: &makepad_fab_gcode::writer::Stats, played: Played) -> Arc<PrintPreview> {
        let Played { layers, total, grown, rows, states } = played;
        let published = self.published.as_mut().expect("flushed");
        let (beads, travel) = published.publish();
        let (blocks, part_layers) = published.blocks.blocks();
        let playback = Arc::new(self.playback(beads.clone(), travel.clone(), rows, states));
        let s = &self.sums;
        let (smin, smax) = if s.speed.0 > s.speed.1 { (0.0, 1.0) } else { s.speed };
        let own: std::collections::BTreeMap<String, f64>;
        let role_times = match timeline {
            Some(tl) => &tl.role_times,
            None => {
                own = s.role_s.iter().filter_map(|(id, secs)| Role::ALL.iter().find(|r| **r as u8 == *id).map(|r| (r.name().to_string(), *secs))).collect();
                &own
            }
        };
        let roles: Vec<PreviewRole> = Role::ALL
            .iter()
            .filter_map(|r| role_times.get(r.name()).filter(|s| **s > 0.0).map(|secs| PreviewRole { id: *r as u8, name: r.name().to_string(), color: role_colour(*r), seconds: *secs, length_mm: s.role_mm.get(&(*r as u8)).copied().unwrap_or(0.0) }))
            .collect();
        let changes = timeline.map(TimeModel::change_rows).unwrap_or_default();
        let changes = changes.into_iter().map(|c| PreviewChange { name: c.name.to_string(), seconds: c.seconds, detail: c.detail }).collect();
        let (head_changes, filament_swaps) = timeline.map_or((0, 0), |tl| (tl.head_changes, tl.filament_swaps));
        let mut summary = vec![
            ("Print time".to_string(), format_time(stats.total_time)),
            ("Printer routines".to_string(), format_time(stats.prepare_time)),
            ("Layers".to_string(), format!("{} · {:.2} mm tall", stats.layers, stats.max_z)),
        ];
        for (i, g) in stats.filament_g.iter().enumerate() {
            if *g > 0.0 {
                summary.push((format!("Filament {}", i + 1), format!("{:.1} g · {:.2} m", g, stats.filament_mm.get(i).copied().unwrap_or(0.0) / 1000.0)));
            }
        }
        if head_changes + filament_swaps > 0 {
            summary.push(("Head changes".to_string(), format!("{head_changes}")));
            summary.push(("Filament swaps".to_string(), format!("{filament_swaps}")));
        }
        // What each slot feeds, the part of it its changes purge, and the time its moves take.
        let per = |v: Option<&Vec<f64>>, i: usize| v.and_then(|v| v.get(i)).copied().unwrap_or(0.0);
        let filament_use: Vec<FilamentUse> = stats.filament_g.iter().enumerate().filter(|(_, g)| **g > 0.0)
            .map(|(i, g)| {
                let mm = stats.filament_mm.get(i).copied().unwrap_or(0.0);
                let purge_grams = if mm > 0.0 { g * per(timeline.map(|tl| &tl.purge_mm), i).min(mm) / mm } else { 0.0 };
                FilamentUse { slot: i, grams: *g, metres: mm / 1000.0, purge_grams, seconds: per(timeline.map(|tl| &tl.filament_s), i) }
            })
            .collect();
        let purges = timeline.map_or(0, |tl| tl.purges);
        let purge_seconds = timeline.and_then(|tl| tl.role_times.get(makepad_fab_gcode::process::PURGE)).copied().unwrap_or(0.0);
        if purges > 0 {
            let grams: f64 = filament_use.iter().map(|f| f.purge_grams).sum();
            summary.push(("Purge".to_string(), format!("{grams:.1} g, {}", format_time(purge_seconds))));
        }
        let pauses: Vec<PreviewPause> = timeline.map(|tl| tl.pauses.iter().map(|p| PreviewPause { layer: p.layer, z: p.z as f32, items: p.items.clone() }).collect()).unwrap_or_default();
        if !pauses.is_empty() {
            let what: Vec<String> = pauses.iter().map(|p| format!("{} before layer {}", p.items.join(", "), p.layer + 1)).collect();
            summary.push(("Pauses to place".to_string(), what.join(" · ")));
        }
        let mut modes = s.modes.clone();
        modes.sort_by_key(|x| x.mode);
        Arc::new(PrintPreview {
            pieces: Vec::new(),
            shared: (0.0, 0.0),
            gcode_key: None,
            filament_use,
            prepare_seconds: stats.prepare_time,
            purges,
            purge_seconds,
            printability: Arc::new(fab_printability::Report::default()),
            markers: issue_markers(&fab_printability::Report::default()),
            blocks,
            part_layers,
            beads,
            travel,
            chunks: Vec::new(),
            layers,
            total,
            grown,
            bounds: if s.bounds[0][0] <= s.bounds[1][0] { s.bounds } else { [[0.0; 3]; 2] },
            speed_range: [smin, smax],
            colours,
            roles,
            changes,
            timeline: playback,
            summary,
            pauses,
            melts: Vec::new(),
            modes,
            max_z_rate: s.max_z_rate,
            nonplanar: None,
            ghosts: RetainedInstances::new(PREVIEW_SLOTS, Vec::<f32>::new().into()).expect("no ghosts"),
            id: PREVIEW_IDS.fetch_add(1, Ordering::Relaxed),
            part_offsets: self.part_offsets.clone(),
            part_objects: Arc::from([]),
            tools: Vec::new(),
        })
    }
}

/// The plate's non-planar tops in the preview: the panel's summary (the stats of every part's
/// plan, area-weighted; printable when the print check passed) and the planar terraces they
/// replace, as ghost bead instances in plate coordinates.
pub fn nonplanar_preview(pv: &mut PrintPreview, plans: &[(usize, Arc<makepad_fab_slicer::nonplanar::NpPlan>, [f64; 2])], checked: &Result<Checked, String>) {
    if plans.is_empty() { return; }
    let (mut top, mut np, mut slope, mut least) = (0.0, 0.0, 0f64, None::<f64>);
    let (mut p95p, mut p95n, mut meanp, mut meann) = (0f64, 0f64, 0.0, 0.0);
    let mut ghosts: Vec<f32> = Vec::new();
    let h = plans[0].1.settings.layer_h as f32;
    for (_, plan, off) in plans {
        let st = &plan.stats;
        top += st.top_area;
        np += st.np_area;
        slope = slope.max(st.max_slope_deg);
        least = match (least, st.min_clearance) { (Some(a), Some(b)) => Some(a.min(b)), (a, b) => a.or(b) };
        p95p = p95p.max(st.terrace_p95_planar);
        p95n = p95n.max(st.terrace_p95_np);
        meanp += st.terrace_mean_planar * st.top_area;
        meann += st.terrace_mean_np * st.top_area;
        let w = makepad_fab_slicer::nonplanar::GHOST_STEP as f32;
        for g in &plan.ghost {
            let (ox, oy) = (off[0] as f32, off[1] as f32);
            let z = g[3] - 0.5 * h;
            ghosts.extend_from_slice(&Bead { a: [g[0] + ox, g[2] + oy, z], b: [g[1] + ox, g[2] + oy, z], width: w, height: h, layer: -1.0, kind: fab::print::kind::GHOST, ..Bead::default() }.pack());
        }
    }
    let note = match checked {
        Ok(_) => String::new(),
        Err(e) if e.contains("non-planar extrusion") => "Preview only: needs head measurements to print".into(),
        Err(e) => format!("Not printable: {e}"),
    };
    let summary = fab::print::NpSummary {
        coverage: if top > 0.0 { np / top } else { 0.0 },
        max_slope_deg: slope,
        min_clearance: least,
        terrace_p95_planar: p95p,
        terrace_p95_np: p95n,
        terrace_mean_planar: if top > 0.0 { meanp / top } else { 0.0 },
        terrace_mean_np: if top > 0.0 { meann / top } else { 0.0 },
        printable: checked.is_ok(),
        note,
    };
    pv.summary.extend(summary.rows());
    pv.nonplanar = Some(summary);
    pv.ghosts = RetainedInstances::new(PREVIEW_SLOTS, ghosts.into()).expect("whole ghost instances");
}

#[cfg(test)]
#[path = "../tests/common/second_family_profiles.rs"]
mod second_family_profiles;

#[cfg(test)]
mod tests {
    use super::*;
    use fab_plate::{Part, PartMesh};

    #[test]
    fn export_attempts_preserve_the_settled_slice_and_ignore_stale_completions() {
        let mut printing = Printing::default();
        let plate = fleet_plate(&printing);
        let sliced = Arc::new(run_slice(1, &defaults_settings(), None, &plate.plate, &plate.machine, "cube").unwrap());
        let mut state = PrintState::default();
        let done = SliceStatus::Done { seconds: 0.1 };
        state.set_preview(sliced.preview.clone());
        state.status = done.clone();
        assert_eq!(state.export, ExportStatus::Idle);
        assert!(printing.export(&mut state, None));
        assert!(matches!(state.export, ExportStatus::Failed(_)), "missing package source is an export failure");
        assert_eq!(state.status, done);

        // The matching file job is already in flight. Export adopts its id on a repeated press.
        let path = crate::model_host::documents_dir().join("cube.gcode.3mf");
        printing.last = Some(sliced.clone());
        let pending = |id| Packaging { id, sliced: sliced.clone(), to: PackageFor::File(path.clone()), stop: Arc::default() };
        for (id, result, expected) in [
            (1, Packaged::Written(path.clone()), ExportStatus::Written(path.clone())),
            (2, Packaged::Failed("write refused".into()), ExportStatus::Failed("write refused".into())),
            (3, Packaged::Stopped, ExportStatus::Idle),
        ] {
            printing.packaging.push(pending(id));
            assert!(printing.export(&mut state, None));
            assert_eq!(state.export, ExportStatus::Exporting);
            assert_eq!(printing.export_attempt, Some(id), "the same slice and path share an attempt");
            assert_eq!(printing.event(&mut state, &PrintEvent::Packaged { id, result }), PrintOutcome::Target);
            assert_eq!(state.export, expected);
            assert_eq!(state.status, done);
            assert_eq!(state.slice_state(), ("Sliced", 1.0));
            assert!(state.can_send(), "the settled plate can be exported again");
        }
        for previous in [ExportStatus::Written(path.clone()), ExportStatus::Failed("write refused".into())] {
            state.export = previous;
            printing.invalidate(&mut state);
            assert_eq!(state.export, ExportStatus::Idle, "edits clear the previous outcome");
        }
        let old = pending(4);
        let stop = old.stop.clone();
        printing.packaging.push(old);
        printing.export_attempt = Some(4);
        printing.reask = Some(true);
        printing.invalidate(&mut state);
        assert!(stop.load(Ordering::Relaxed), "a replaced file job gets a stop request");
        assert_eq!(printing.export_attempt, None);
        assert_eq!(state.export, ExportStatus::Exporting, "an export waiting for the rebuilt plate stays in progress");
        let late = PrintEvent::Packaged { id: 4, result: Packaged::Written(path) };
        assert_eq!(printing.event(&mut state, &late), PrintOutcome::Unchanged);
        assert_eq!(state.export, ExportStatus::Exporting, "the superseded write cannot change the waiting export");
    }

    #[test]
    fn export_of_an_all_excluded_plate_reports_the_refusal() {
        let mut printing = Printing::default();
        let mut state = PrintState::default();
        let mut model = fab_model::FabModel::with_script("cube", "cube.cad.splash",
            "use mod.fab.*\nobject(mod.model.box({size: [1mm 1mm 1mm]}))");
        let mut evaluator = fab_model::eval::Evaluator::new(printing.machine().unwrap().clone());
        let (compiled, plate) = model.evaluate(&mut evaluator, None).unwrap();
        let action = fab::api::PartAction::SetPrint { objects: vec![plate.plate.parts[0].object.clone()], print: false };
        let edits = crate::plate_edit::edits(&compiled, &plate, &action).unwrap();
        let (_, update) = model.apply(&mut evaluator, "exclude", model.head(), &edits, None).unwrap();
        let plate = Arc::new(update.unwrap().1);
        assert!(plate.plate.parts.is_empty() && !plate.plate.excluded.is_empty());
        assert!(printing.export(&mut state, Some(&plate)));
        assert_eq!(state.export, ExportStatus::Failed("no printable parts on the plate".into()));
        assert_eq!(state.status, SliceStatus::Idle);
        printing.reask = Some(true);
        assert!(printing.resume_asked(&mut state, Some(&plate)));
        assert_eq!(state.export, ExportStatus::Failed("no printable parts on the plate".into()));
    }

    /// A live slice's supports are a stage of its one bar: no job of their own on the board. The
    /// status follows the engine's progress of the live revision by the whole percent, and says
    /// supports while they plan; another revision's progress is not its.
    #[test]
    fn planning_supports_is_a_stage_of_the_slice() {
        let (mut printing, mut state) = (Printing::default(), PrintState::default());
        state.status = SliceStatus::Running { progress: 0.1, stage: "Slicing the view".into() };
        let words = "Supports: Lattice cube (1 of 2) · growing trees 12/48";
        assert!(printing.running_at(&mut state, 7, 0.304, words), "the status moved");
        let planning = |progress: f32| SliceStatus::Running { progress, stage: PLANNING_SUPPORTS.into() };
        assert_eq!(state.status, planning(0.3));
        assert!(!printing.running_at(&mut state, 7, 0.302, words), "within the percent: nothing to redraw");
        // The layers' news keeps it while the engine tells; a newer revision's is its own.
        assert_eq!(Printing::live_running(printing.running.as_ref(), 7, 1.0), planning(0.3));
        assert_eq!(Printing::live_running(printing.running.as_ref(), 8, 0.5), SliceStatus::Running { progress: 0.5, stage: "Slicing the view".into() });
        assert!(printing.running_at(&mut state, 7, 0.9, "G-code and print time"), "on to the next stage");
        assert_eq!(state.status, SliceStatus::Running { progress: 0.9, stage: "Slicing the view".into() });
        // Not running: the status stays the slice's own.
        state.status = SliceStatus::Idle;
        assert!(!printing.running_at(&mut state, 7, 0.95, words));
        assert_eq!(state.status, SliceStatus::Idle);
    }

    #[test]
    fn installed_models_are_required_but_own_defaults_still_slice() {
        use makepad_fab_settings::{family, profiles::RawPreset};
        let machine = Machine::dual_nozzle_350();
        let mut settings = defaults_settings();
        let plate = Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part("box", 1, None, cube([100.0, 100.0, 0.0], 1.0))] };
        let setup = resolve_setup(&settings, None, &plate, &machine).expect("own defaults");
        assert_eq!(setup.machine.values.str("printer_model"), "");
        let sliced = makepad_fab_slice::plate::slice_plate_painted_with(&setup, &plate, &[], &|_, _| {}).expect("own defaults slice");
        assert!(std::ptr::eq(family::of(&sliced.job.cfg).expect("default family"), family::DEFAULT));
        assert!(!sliced.gcode.is_empty());
        settings.use_installed = true;
        settings.materials = vec![JobMaterial { preset: setup.filaments[0].name.clone(), color: [0.9, 0.9, 0.9, 1.0], nozzle: 0, tray: None, dedicated: false }];
        for (model, reason) in [("", family::NO_MODEL), ("Unclaimed", family::UNSUPPORTED)] {
            let mut printer = setup.machine.clone();
            printer.values.set_str("printer_model", model);
            let presets = [printer, setup.process.clone(), setup.filaments[0].clone()].into_iter().map(|p| RawPreset {
                name: p.name, kind: p.kind, values: p.values, from: "system".into(), ..Default::default()
            }).collect();
            let cat = Catalog { twins: HashMap::new(), automatic: HashMap::new(), resin: Default::default(), layout_revision: 0, store: Arc::new(ProfileStore::open(&[], &[]).with_builtin(presets)), selection: None, entries: Vec::new(), installed: true };
            assert_eq!(resolve_setup(&settings, Some(&cat), &plate, &machine).err().as_deref(), Some(reason));
            assert!(!same_printer(model, model));
            if !model.is_empty() {
                let meta = JobMeta { printer_model: model.into(), ..Default::default() };
                assert_eq!(preview_of_package(&[], &meta).err().as_deref(), Some(reason));
            }
        }

        // A single-nozzle printer of the default family, its machine profile as BambuStudio
        // installs a P1S, X1C, A1 or H2S: no per-nozzle areas, heights 0. The gate's printer
        // reaches the whole bed, and the safety check takes its machine.
        let mut one = Machine::dual_nozzle_350();
        one.nozzles.truncate(1);
        one.nozzles[0].reach = one.bed;
        one.slots.retain(|s| s.nozzle == 0);
        let single = defaults_setup(&defaults_settings(), &one, 1);
        let mut printer = single.machine.clone();
        printer.values.set_str("printer_model", "Bambu Lab P1S");
        printer.values.set_vec("extruder_printable_area", Vec::new());
        printer.values.set_vec("extruder_printable_height", vec!["0".into()]);
        printer.values.set_vec("extruder_offset", vec!["0x0".into()]);
        // An installed PLA, as the gate reads one: its type, range and print temperatures.
        let mut pla = single.filaments[0].clone();
        for (key, value) in [("filament_type", "PLA"), ("nozzle_temperature_range_low", "190"), ("nozzle_temperature_range_high", "240"), ("nozzle_temperature_initial_layer", "220"), ("nozzle_temperature", "220")] {
            pla.values.set_vec(key, vec![value.into()]);
        }
        let presets = [printer, single.process.clone(), pla.clone()].into_iter().map(|p| RawPreset {
            name: p.name, kind: p.kind, values: p.values, from: "system".into(), ..Default::default()
        }).collect();
        let cat = Catalog { twins: HashMap::new(), automatic: HashMap::new(), resin: Default::default(), layout_revision: 0, store: Arc::new(ProfileStore::open(&[], &[]).with_builtin(presets)), selection: None, entries: Vec::new(), installed: true };
        let head_envelope = crate::dirs::FabDirs::system().head_envelope;
        let gate = gate_profile_of(&cat, &single.machine.name, std::slice::from_ref(&pla), &head_envelope).expect("the single-nozzle printer's gate profile");
        assert_eq!(gate.extruder_areas, vec![gate.printable_area.clone()], "a single nozzle without an area of its own reaches the whole bed");
        settings.materials = vec![JobMaterial { preset: single.filaments[0].name.clone(), color: [0.9, 0.9, 0.9, 1.0], nozzle: 0, tray: None, dedicated: false }];
        let setup = resolve_setup(&settings, Some(&cat), &plate, &one).expect("the single-nozzle printer's job");
        let sliced = makepad_fab_slice::plate::slice_plate_painted_with(&setup, &plate, &[], &|_, _| {}).expect("the single-nozzle printer slices");
        // The check takes its machine: no refusal of its areas, nozzles or build volume. (The
        // fixture starts with Fab's own default sequence, not a printer's, so the check still
        // refuses its moves; an installed profile carries the printer's own sequence.)
        if let Err(refusal) = fab_printer_link::safety::check_gcode(&sliced.gcode, &gate_profile(&cat, &setup, &head_envelope).unwrap()) {
            let machine: Vec<_> = refusal.0.iter().filter(|v| matches!(v.rule, "machine" | "build volume" | "nozzle offset" | "nozzle diameter" | "nozzle map" | "filament")).collect();
            assert!(machine.is_empty(), "the safety check refuses a single-nozzle printer's machine: {machine:?}");
        }
    }

    #[test]
    fn default_machine_geometry_round_trips() {
        let expected = Machine::dual_nozzle_350();
        let setup = defaults_setup(&defaults_settings(), &expected, 1);
        let actual = machine_of(&setup.machine.name, &setup.machine.values).unwrap();
        assert_eq!(actual.bed, expected.bed);
        assert_eq!(actual.max_height, expected.max_height);
        assert_eq!(actual.nozzles.len(), expected.nozzles.len());
        for (actual, expected) in actual.nozzles.iter().zip(&expected.nozzles) {
            assert_eq!(actual.name, expected.name);
            assert_eq!(actual.reach, expected.reach);
            assert_eq!(actual.max_height, expected.max_height);
        }
        assert_eq!(actual.keep_out, expected.keep_out);
    }

    /// A second-family printer feeds each head from a spool of its own, however many heads it
    /// has, whether the profile names the slots or not.
    #[test]
    fn a_second_family_printer_has_one_spool_per_head() {
        use fab_plate::machine::{Slot, SlotPath};
        let heads = |n: usize, slots: Option<&str>| {
            let mut values = defaults_setup(&defaults_settings(), &Machine::dual_nozzle_350(), 1).machine.values;
            values.set_str("printer_model", makepad_fab_settings::family::FAMILIES[1].models[0]);
            values.set_vec("nozzle_diameter", vec!["0.4".into(); n]);
            values.set_vec("extruder_printable_area", Vec::new());
            values.set_vec("extruder_printable_height", Vec::new());
            if let Some(slots) = slots { values.set_vec("fab_material_slots", vec![slots.into()]); }
            machine_of("second", &values).map_err(|e| e.local_message())
        };
        let own = |labels: &[&str]| labels.iter().enumerate().map(|(h, t)| Slot { nozzle: h, tray: t.to_string(), path: SlotPath::Own }).collect::<Vec<_>>();
        assert_eq!(heads(4, None).unwrap().slots, own(&["A1", "B1", "C1", "D1"]));
        // Six and nine heads: the letters go on past D, and each head is named.
        let six = heads(6, None).unwrap();
        assert_eq!(six.slots, own(&["A1", "B1", "C1", "D1", "E1", "F1"]));
        // The linked printer is watched for the profile's six heads; another printer's profile
        // tells it nothing.
        let model = makepad_fab_settings::family::FAMILIES[1].models[0];
        assert_eq!(profile_heads(model, model, &six), Some(6));
        assert_eq!(profile_heads(model, makepad_fab_settings::family::DEFAULT.models[0], &six), None);
        let nine = heads(9, None).unwrap();
        assert_eq!(nine.slots, own(&["A1", "B1", "C1", "D1", "E1", "F1", "G1", "H1", "I1"]));
        assert_eq!(nine.nozzles.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(), (1..=9).map(|h| format!("Tool {h}")).collect::<Vec<_>>());
        // A profile's own slots stand, and may name a head past the fourth.
        assert_eq!(heads(4, Some("1:A1:own")).unwrap().slots.len(), 1, "a profile's own slots stand");
        assert_eq!(heads(9, Some("9:I1:own")).unwrap().slots, vec![Slot { nozzle: 8, tray: "I1".into(), path: SlotPath::Own }]);
        assert!(heads(fab_plate::machine::UNITS + 1, None).unwrap_err().contains("more heads than its trays can be numbered"));
    }

    /// Every built-in machine and its live report agree on the nozzle and feed path of every tray.
    #[test]
    fn virtual_profiles_and_reports_describe_the_same_trays() {
        use makepad_fab_settings::family::{self, Flavor, Link};
        let printers = virtual_printer::presets();
        let store = ProfileStore::open(&[], &[]).with_builtin(virtual_printer::builtin_presets(&printers));
        for vp in printers {
            let name = vp.machine_preset_name();
            let values = store.resolve(&name).unwrap().values;
            let family = family::of(&values).unwrap();
            assert_eq!(family.link, Link::Virtual, "{name}");
            assert_eq!(family.flavor, if vp.kind == VirtualKind::Toolchanger { Flavor::Named } else { Flavor::Mask }, "{name}");
            let machine = machine_of(&name, &values).unwrap();
            assert_eq!(machine.nozzles.len(), vp.heads, "{name} heads");
            let offline = colours::Offline::of(&machine);
            let setup = setup_of(&vp, None).unwrap();
            let report = fab_printer_link::virtual_link::report(&setup);
            let layout = colours::layout(Some((&setup.name, &report)), None, &offline, &|_| None);
            assert!(layout.live);
            assert_eq!(layout.trays.len(), vp.slots.len(), "{name} live trays");
            assert_eq!(machine.slots.len(), vp.slots.len(), "{name} machine slots");
            for (slot, target) in vp.slots.iter().zip(&setup.trays) {
                let pos = colours::Position { head: target.head, tray: target.id };
                assert_eq!((pos.head, pos.tray_text()), (slot.head, slot.tray.clone()), "{name} position");
                let tray = layout.tray(pos).expect("the report has this slot");
                let profile = machine.slots.iter().find(|s| s.nozzle == pos.head && s.tray == slot.tray).expect("the machine has this slot");
                let path = match slot.path {
                    virtual_printer::SlotPath::Shared => fab_plate::machine::SlotPath::Shared,
                    virtual_printer::SlotPath::Own => fab_plate::machine::SlotPath::Own,
                    virtual_printer::SlotPath::Dedicated => fab_plate::machine::SlotPath::Dedicated,
                };
                assert_eq!((profile.path, tray.path), (path, path), "{name} {}", slot.tray);
                assert_eq!(tray.material, slot.material);
                assert_eq!(tray.colour.map(srgb), slot.colour);
                if slot.material.as_deref() == Some("TPU") { assert!(tray.dedicated(), "{name} TPU feed"); }
            }
        }
    }

    fn remap(printing: &mut Printing, state: &mut PrintState, ev: &mut Evaluated) {
        if let Some(mapped) = printing.refresh_printer(state, Some(&Arc::new(ev.clone()))) {
            ev.set_mapped(mapped);
        }
    }

    // Waits on progress, never on the clock: each round waits for the next publication from
    // any link, so a report that never comes leaves a hung job rather than a timing flake.
    // The counter is read before the check, so a publication during it is never missed.
    fn wait_for(property: &str, mut ready: impl FnMut() -> bool) {
        let mut said = false;
        loop {
            let seen = fab_printer_link::published();
            if ready() { return; }
            if !std::mem::replace(&mut said, true) { eprintln!("fleet test: waiting until {property}"); }
            while fab_printer_link::published() == seen { std::thread::sleep(std::time::Duration::from_millis(2)); }
        }
    }

    #[test]
    fn finished_job_state_survives_a_pending_sent_save() {
        let id = JobMeta::new_id("block", jobs::wall_now());
        let dir = std::env::temp_dir().join(format!("fab-job-writer-{}-{id}", std::process::id()));
        let meta = Arc::new(JobMeta { id: id.clone(), name: "block".into(), ..Default::default() });
        jobs::commit(&dir, &meta, &SavedState { state: JobState::Queued, ..Default::default() }, b"package", &Default::default()).unwrap();

        let (writer, wakes) = JobWriter::channel();
        let mut printing = Printing { job_writer: Some(writer), ..Default::default() };
        let mut state = PrintState::default();
        state.follow.jobs.push(CommittedJob { meta, ..Default::default() });
        // Deliver two UI events before the writer takes its first wake.
        for stage in [JobState::Sent, JobState::Finished] {
            state.follow.jobs.at_mut(0).state = stage;
            state.follow.changed(&id);
            printing.sync_jobs(&mut state);
        }
        let writer = printing.job_writer.take().unwrap();
        let thread = writer.spawn(dir.clone(), wakes, |id, e| panic!("save failed for {id}: {e}")).unwrap();
        drop(writer);
        let drained = thread.join();
        let rows = jobs::read_all(&dir);
        std::fs::remove_dir_all(dir).unwrap();
        drained.expect("the job writer must drain without a save error");
        assert_eq!(rows.first().map(|(_, saved)| &saved.state), Some(&JobState::Finished),
            "the saved job must read back Finished after a pending Sent save");
    }

    #[test]
    fn loadout_filenames_preserve_printer_identity() {
        let real = |name: &str| loadout_file(&PrinterChoice::Real(name.into()));
        assert_ne!(real("A"), real("a"), "upper- and lower-case printers must have distinct loadout files");
        assert_ne!(real("x"), loadout_file(&PrinterChoice::Virtual("x".into())), "real and virtual names must be distinct");
        for (name, encoded) in [("a", "a"), ("A", "_41"), ("a/b", "a_2fb"), ("..", "_2e_2e"),
            ("a b", "a_20b"), ("a-b0", "a-b0"), ("_41", "_5f41"), ("é", "_c3_a9")] {
            assert_eq!(real(name), Path::new("trays").join(format!("printer-{encoded}.txt")), "{name} must form one escaped filename");
        }
        assert_eq!(loadout_file(&PrinterChoice::None), Path::new("trays.txt"), "no printer keeps the original hand-edit file");
    }

    #[test]
    fn loadouts_survive_one_drain_and_restart() {
        use std::fs;
        const CHILD: &str = "FAB_TEST_LOADOUTS_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let home = std::env::temp_dir().join(format!("fab-loadouts-{}", std::process::id()));
            fs::create_dir(&home).unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "print::tests::loadouts_survive_one_drain_and_restart", "--nocapture"])
                .env(CHILD, "1").env("MAKEPAD_TEST_HOME", &home).status().unwrap();
            fs::remove_dir_all(home).unwrap();
            assert!(status.success(), "isolated loadout persistence case failed");
            return;
        }

        let home = PathBuf::from(std::env::var_os("MAKEPAD_TEST_HOME").unwrap());
        let dir = home.join(".makepad/fab");
        // Hold the worker at its wake until all pending updates have arrived.
        let (wake, wakes) = std::sync::mpsc::sync_channel(1);
        let pending = Arc::new(std::sync::Mutex::new(ProfileWork::default()));
        let drain = || {
            wakes.recv().unwrap();
            let work = std::mem::take(&mut *pending.lock().unwrap());
            write_loadouts(&dir, work.loadouts);
        };
        let (a, b) = (PrinterChoice::Real("A".into()), PrinterChoice::Real("B".into()));
        let presets = virtual_printer::presets();
        let single = presets.iter().find(|p| p.name == "two-materials").unwrap();
        second_family_profiles::install(&home);
        let catalog = Arc::new(catalog_in(&ProfileStore::default_data_dirs(), &home.join("imports"), std::slice::from_ref(single)).unwrap());
        let restart = || {
            let worker = ProfilesWorker { pending: pending.clone(), wake: wake.clone() };
            let mut printing = Printing { profiles_worker: Some(worker), catalog: Some(catalog.clone()), ..Default::default() };
            let mut state = PrintState { settings: defaults_settings(), ..Default::default() };
            state.settings.use_installed = true;
            state.settings.machine = single.machine_preset_name();
            state.settings.process = virtual_printer::PROCESS.into();
            printing.load_base_values(&mut state);
            printing.event(&mut state, &PrintEvent::PrinterConfigs {
                names: vec!["A".into(), "B".into()], presets: vec![single.clone()], loadouts: read_loadouts(&dir),
            });
            (printing, state)
        };

        // Both real configurations can report the same model. Only A owns this old file.
        fs::create_dir_all(&dir).unwrap();
        let legacy = format!("model\t{}\nprinter\tA\nnozzle\tA1\tAMS A\tloaded\tABS\t-\n", single.model());
        fs::write(dir.join("trays.txt"), &legacy).unwrap();
        let (mut printing, mut state) = restart();
        assert_eq!(printing.loadout_text.as_ref(), Some(&legacy), "no printer must keep the legacy loadout");
        let identify = |state: &mut PrintState, name: &str, model: String| {
            state.printers.set(&state.links.picked, 1, Arc::new(fab_printer_link::Snapshot {
                name: name.into(), model, ..Default::default()
            }));
        };
        state.links.picked = a.clone();
        identify(&mut state, "A", second_family_profiles::model());
        printing.select_loadout(&state);
        assert!(printing.loadout.is_none(), "a mismatched reported model must not adopt legacy trays");
        identify(&mut state, "other display name", single.model());
        printing.refresh_printer(&mut state, None);
        assert!(printing.loadout.is_none(), "a different reported display name must not adopt legacy trays");
        identify(&mut state, "A", single.model());
        printing.refresh_printer(&mut state, None);
        assert_eq!(printing.loadout_text.as_ref(), Some(&legacy), "A must adopt the legacy loadout when its model matches");
        drain();
        assert_eq!(fs::read(dir.join(loadout_file(&a))).unwrap(), legacy.as_bytes(), "adoption must save A's own copy");
        state.links.picked = b.clone();
        identify(&mut state, "B", single.model());
        printing.select_loadout(&state);
        assert!(printing.loadout_text.is_none() && printing.loadout.is_none(), "B must never inherit A's legacy trays");
        assert_eq!(fs::read(dir.join("trays.txt")).unwrap(), legacy.as_bytes(), "adoption must preserve every byte of trays.txt");

        // A's hand edits and B's changed report arrive before one profiles-worker drain.
        state.links.picked = a.clone();
        printing.select_loadout(&state);
        let pos = printing.loadout.as_ref().unwrap().trays[0].pos;
        let edit = |owner, material: &str| PrintAction::EditTray(owner, pos, colours::TrayEdit::Material(material.into()));
        let owner = EditOwner::new(PrinterScope::Target, &state);
        printing.action(&mut state, &edit(owner.clone(), "PETG"), None);
        printing.action(&mut state, &edit(owner, "PLA"), None);
        let model = fab_model::FabModel::with_script("cube", "cube.cad.splash",
            "use mod.fab.*\nobject(mod.model.box({size: [1.2mm 0.2mm 1.2mm]}))");
        let mut evaluator = fab_model::eval::Evaluator::new(printing.machine().unwrap().clone());
        let (_, mut plate) = model.evaluate(&mut evaluator, None).unwrap();
        remap(&mut printing, &mut state, &mut plate);
        let a_text = printing.loadout_text.clone().unwrap();
        let (layout, mapping, materials) = (printing.loadout.clone(), state.colors.clone(), state.settings.materials.clone());
        assert!(mapping.rows.iter().any(|r| r.used && r.pos == Some(pos)) && !materials.is_empty(), "A's edited tray must map the plate's material");

        let b_file = home.join("background.jsonl");
        fs::write(&b_file, "@head 0 1 PETG EF120AFF\n@head 3 1 TPU 16CF51FF\n").unwrap();
        let b_model = second_family_profiles::model();
        let older = format!("model\t{b_model}\nprinter\tB\ntool_1\tA1\tAMS A\tloaded\tABS\t-\n");
        remember_loadout(&mut printing.loadouts, printing.profiles_worker.as_ref(), &b, &older);
        printing.install(&mut state, &b, fab_printer_link::PrinterLink::simulate_moonraker(b_file, &b_model), false);
        wait_for("B must report its own fourth head", || {
            let flags = printing.poll_printer(&mut state);
            assert!(!flags.target, "B's trays must be routed as a background report");
            state.printer_of(&b).is_some_and(|s| s.state.tray(TrayId { ams: 3, slot: 0 }).is_some_and(|t| t.material.as_deref() == Some("TPU")))
        });
        assert_eq!(printing.loadout, layout, "B's report must leave A's kept layout unchanged");
        assert_eq!(state.colors, mapping, "B's report must leave A's colour mapping unchanged");
        assert_eq!(state.settings.materials, materials, "B's report must leave A's material settings unchanged");
        let b_text = printing.loadouts[&loadout_file(&b)].clone();
        drain();
        let saved = read_loadouts(&dir);
        assert!(saved.get(&loadout_file(&a)) == Some(&a_text) && saved.get(&loadout_file(&b)) == Some(&b_text),
            "one drain must write both printers' own latest trays");
        assert!(a_text.contains("\tPLA\t") && b_text.contains("tool_4\tD1\t") && b_text.contains("\tTPU\t"),
            "each file must contain its own material and B's fourth head");
        drop(printing);

        // Exercise the picker with configured twins so a restart and both picks stay offline.
        let (mut printing, mut state) = restart();
        let mut modes = Saved::default();
        for (config, preset) in [("A", single.machine_preset_name()), ("B", second_family_profiles::MACHINE.into())] {
            let mut twin = catalog.twins[&preset].printer.clone();
            twin.name = format!("twin-{config}");
            twin.configured_for = Some(config.into());
            modes.disconnected.insert(config.into());
            modes.printers.push(twin);
        }
        printing.virtuals = Some(Arc::new(modes));
        for (pick, expected) in [(&a, &a_text), (&b, &b_text), (&a, &a_text)] {
            assert!(printing.link_printer(&mut state, pick), "the configured pick must succeed offline");
            assert_eq!(printing.loadout_text.as_ref(), Some(expected), "a restarted pick must restore that printer's own text");
            let parsed = colours::parse_loadout(loadout_model(expected).1, &printing.offline().heads).unwrap();
            assert_eq!(printing.loadout.as_ref(), Some(&parsed), "the restored trays must use the picked profile's heads");
            if pick == &b {
                let twin = printing.make_twin(&mut state, &b, second_family_profiles::MACHINE).unwrap();
                let PrinterChoice::Virtual(name) = twin else { panic!("a standalone twin has a virtual id") };
                let stock = &printing.virtuals.as_ref().unwrap().printers.iter().find(|p| p.name == name).unwrap().slots;
                assert!(stock.iter().any(|s| s.head == 3 && s.material.as_deref() == Some("TPU")), "B's standalone twin must seed its fourth head from B");
            }
        }
        printing.link_printer(&mut state, &PrinterChoice::None);
        let owner = EditOwner::new(PrinterScope::Target, &state);
        printing.action(&mut state, &edit(owner, "PETG"), None);
        drain();
        let saved = read_loadouts(&dir);
        assert!(saved[Path::new("trays.txt")].contains("\tPETG\t"), "hand editing with no printer must update trays.txt");
        assert_eq!((&saved[&loadout_file(&a)], &saved[&loadout_file(&b)]), (&a_text, &b_text), "no-printer edits must preserve both printers' trays");
    }

    fn owner(name: &str) -> CalibrationOwner {
        CalibrationOwner { id: format!("printer:{name}"), title: name.into() }
    }

    fn test_calibration_setup() -> CheckSetup {
        let mut setup = CheckSetup::fab_defaults();
        setup.settings = settings(1);
        setup.settings.materials[0].preset = "PLA".into();
        setup
    }

    fn calibration_dir(case: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fab-cal-{case}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn calibration_owner_by_target_kind() {
        let mut printing = Printing::default();
        let mut state = PrintState::default();
        let base = virtual_printer::presets().remove(0);
        let twin = |name: &str, source: Option<&str>| VirtualPrinter {
            name: name.into(), source: VirtualSource::Twin { preset: base.machine_preset_name(), model: base.model(), source: source.map(str::to_owned) },
            ..base.clone()
        };
        let mut saved = Saved::default();
        saved.printers = vec![base.clone(), twin("from-p", Some("printer:P")), twin("nested", Some("virtual:from-p")),
            twin("catalogue", None), twin("from-generated", Some(&format!("virtual:{}", base.name))),
            twin("a", Some("virtual:b")), twin("b", Some("virtual:a"))];
        let mut configured = twin("paired", Some("printer:P"));
        configured.configured_for = Some("P".into());
        saved.printers.push(configured);
        printing.virtuals = Some(Arc::new(saved));
        state.links.picked = PrinterChoice::Real("P".into());
        for disconnected in [false, true] {
            state.links.disconnected = disconnected;
            assert_eq!(printing.calibration_owner(&state), Some(owner("P")), "configured P owns both modes");
        }
        let saved = Arc::make_mut(printing.virtuals.as_mut().unwrap());
        if let VirtualSource::Twin { source, .. } = &mut saved.printers.last_mut().unwrap().source { *source = None; }
        assert_eq!(printing.calibration_owner(&state), Some(owner("P")), "legacy configured twin keeps its printer owner");
        for (name, expected) in [("from-p", Some(owner("P"))), ("nested", Some(owner("P"))), ("catalogue", None),
            (&base.name, None), ("from-generated", None), ("a", None)] {
            state.links.picked = PrinterChoice::Virtual(name.into());
            assert_eq!(printing.calibration_owner(&state), expected, "provenance of {name}");
        }
        state.links.picked = PrinterChoice::None;
        assert_eq!(printing.calibration_owner(&state), None, "unlinked uses the preset store");
    }

    #[test]
    fn check_setup_id_hashes_effective_slicer_values() {
        use fab_calibrate::{results::names, Results};
        let dir = calibration_dir("id");
        let mut setup = test_calibration_setup();
        let preset = setup.settings.machine.clone();
        let key = |p| CalibrationKey { owner: Some(owner(p)), preset: preset.clone(), filaments: vec!["PLA".into()] };
        let write = |name: &str, flow: f64, owned: bool| {
            let mut r = Results { printer: name.into(), filament: "PLA".into(), ..Default::default() };
            r.values.insert(names::FLOW_RATIO.into(), flow);
            if owned { r.save_owner_in(&dir) } else { r.save_in(&dir) }.unwrap();
        };
        write("printer:P", 0.91, true);
        write("printer:Q", 1.07, true);
        write(&preset, 0.91, false);
        setup.calibration = Arc::new(EffectiveCalibration::resolve_in(&dir, &key("P")));
        let id = setup.id();
        setup.calibration = Arc::new(EffectiveCalibration::resolve_in(&dir, &key("Q")));
        assert_ne!(id, setup.id(), "different effective flow gives different toolpaths");
        setup.calibration = Arc::new(EffectiveCalibration::resolve_in(&dir, &key("P2")));
        assert_eq!(id, setup.id(), "equal flow hashes identically through preset fallback");
        write("printer:Q", 0.85, true);
        setup.calibration = Arc::new(EffectiveCalibration::resolve_in(&dir, &key("P")));
        assert_eq!(id, setup.id(), "saving Q does not change P's slice key");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn check_setup_serves_job_mech_and_coupon_from_effective_arc() {
        use fab_calibrate::results::{names, EffectiveValue, ValueSource};
        let mut setup = test_calibration_setup();
        let mut r = EffectiveResults::default();
        for (name, value) in [(names::FLOW_RATIO, 0.91), (names::C_XY, 0.35), (names::NOZZLE_TEMP, 215.0)] {
            r.values.insert(name.into(), EffectiveValue { value, source: ValueSource::Owner });
        }
        Arc::make_mut(&mut setup.calibration).by_filament.insert("PLA".into(), r);
        let job = setup.job_setup(&Plate::default(), &Machine::dual_nozzle_350()).unwrap();
        assert_eq!(job.filaments[0].values.raw("filament_flow_ratio", 0), "0.910", "the print job consumes effective flow");
        assert_eq!(job.filaments[0].values.raw("nozzle_temperature", 0), "215", "the print job consumes effective temperature");
        assert_eq!(setup.mech_profile().measured.c_xy, Some(0.35), "joint sizing consumes effective clearance");
        let coupon = setup.coupon_setup();
        assert_eq!((coupon.flow_ratio, coupon.nozzle_temp, coupon.c_xy), (0.91, 215.0, 0.35), "coupon generation consumes the same snapshot");
    }

    fn manual_profiles(printing: &mut Printing) -> std::sync::mpsc::Receiver<()> {
        let (wake, rx) = std::sync::mpsc::sync_channel(1);
        printing.profiles_worker = Some(ProfilesWorker { pending: Default::default(), wake });
        rx
    }

    fn drain_cal(printing: &Printing, dir: &Path) -> Vec<PrintEvent> {
        let work = std::mem::take(&mut *printing.profiles_worker.as_ref().unwrap().pending.lock().unwrap());
        let mut events = Vec::new();
        run_calibration_work(dir, work.cal_writes, work.cal_resolve, |e| events.push(e));
        events
    }

    #[test]
    fn calibration_writes_preserve_fifo_order_and_stale_resolves_drop() {
        use fab_calibrate::{Answer, CouponSetup, Step, results::names};
        let dir = calibration_dir("fifo");
        let mut printing = Printing::default();
        let wakes = manual_profiles(&mut printing);
        let mut state = PrintState { settings: test_calibration_setup().settings, ..Default::default() };
        state.links.picked = PrinterChoice::Real("P".into());
        let preset = state.settings.machine.clone();
        let mut steps = Vec::new();
        for (step, n) in [(Step::Clearance, 5), (Step::Flow, 1)] {
            let cs = CouponSetup { flow_ratio: 0.91 / 0.90, ..Default::default() };
            let coupon = Arc::new(fab_calibrate::make(step, &cs).unwrap());
            printing.save_calibration(&mut state, Some(owner("P")), preset.clone(), "PLA".into(), coupon, Answer::Number(n), "2026-01-01".into());
            steps.push(step);
        }
        assert!(wakes.try_recv().is_ok(), "batched writes wake the worker");
        let events = drain_cal(&printing, &dir);
        let saved: Vec<_> = events.iter().filter_map(|e| match e {
            PrintEvent::CalibrationWritten { outcome: CalWriteOutcome::Saved { step, .. } } => Some(*step), _ => None
        }).collect();
        assert_eq!(saved, steps, "every write is acknowledged once, in FIFO order");
        let stale = events.last().unwrap();
        printing.event(&mut state, stale);
        let r = printing.effective_calibration().for_filament("PLA").unwrap();
        assert_eq!(r.get(names::C_XY), Some(0.3), "clearance survives the subsequent flow save");
        assert!((r.get(names::FLOW_RATIO).unwrap() - 0.91).abs() < 1e-9, "flow persisted with clearance");
        state.links.picked = PrinterChoice::Real("Q".into());
        printing.sync_calibration(&mut state);
        for event in drain_cal(&printing, &dir) { printing.event(&mut state, &event); }
        let q = printing.effective_calibration().clone();
        assert_eq!(printing.event(&mut state, stale), PrintOutcome::Unchanged, "stale resolve is discarded");
        assert!(Arc::ptr_eq(&q, printing.effective_calibration()), "Q keeps its resolved snapshot");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn late_answer_after_retarget_writes_captured_owner_and_leaves_active_target_alone() {
        use crate::calibrate::{CalEvent, Calibrate};
        use fab::print::calibrate::CalibrateAction;
        use fab_calibrate::{Results, Step};
        let dir = calibration_dir("late-answer");
        let base = virtual_printer::presets().remove(0);
        let cat = Arc::new(catalog_in(&[], &dir, std::slice::from_ref(&base)).unwrap());
        let mut saved = Saved::default();
        for name in ["P", "Q"] {
            let mut vp = cat.twins[&base.machine_preset_name()].printer.clone();
            vp.configured_for = Some(name.into());
            vp.name = format!("coupon-{name}");
            saved.printers.push(vp);
            saved.disconnected.insert(name.into());
        }
        let mut printing = Printing { catalog: Some(cat), virtuals: Some(Arc::new(saved)), ..Default::default() };
        let _wake = manual_profiles(&mut printing);
        let mut state = PrintState { settings: defaults_settings(), ..Default::default() };
        assert!(printing.link_printer(&mut state, &PrinterChoice::Real("P".into())));
        printing.sync_calibration(&mut state);
        for e in drain_cal(&printing, &dir) { printing.event(&mut state, &e); }
        let mut panel = Calibrate::default();
        let setup = printing.check_setup(&state);
        let (preset, filament) = setup.calibration_names();
        assert!(!filament.is_empty(), "the captured coupon needs a selected filament");
        let CalEvent::Made(made) = CalEvent::make(Step::Clearance, &setup, [1.0; 4]);
        panel.made(&mut state.calibrate, &made);
        assert!(printing.link_printer(&mut state, &PrinterChoice::Real("Q".into())));
        printing.sync_calibration(&mut state);
        for e in drain_cal(&printing, &dir) { printing.event(&mut state, &e); }
        let cube = fleet_plate(&printing);
        let sliced = Arc::new(run_slice(1, &state.settings, printing.catalog.as_ref(), &cube.plate, &cube.machine, "cube").unwrap());
        state.set_preview(sliced.preview.clone());
        state.status = SliceStatus::Done { seconds: 0.0 };
        state.settled = true;
        printing.last = Some(sliced);
        let before = printing.check_setup(&state);
        assert_eq!(before.calibration_names(), (preset.clone(), filament.clone()), "P and Q share a preset and filament");
        let seq = printing.calibration_seq;
        let a = CalibrateAction::Answer { a: 5, b: 0, angle: 0.0 };
        assert!(panel.action(&mut state, &a, &mut printing, [1.0; 4], None));
        for e in drain_cal(&printing, &dir) {
            printing.event(&mut state, &e);
            if let PrintEvent::CalibrationWritten { outcome, .. } = e {
                panel.written(&mut state.calibrate, &outcome, printing.requested_calibration_owner(), &preset, &filament);
            }
        }
        let after = printing.check_setup(&state);
        assert_eq!(Results::load_owner_in(&dir, "printer:P", &filament).values.get("c_xy"), Some(&0.30), "P owns its coupon's answer");
        assert!(!Results::owner_path_in(&dir, "printer:Q", &filament).exists(), "the active printer gets no write");
        assert!(!Results::path_in(&dir, &preset, &filament).exists(), "a captured printer's result leaves the preset alone");
        assert_eq!((after.id(), printing.calibration_seq), (before.id(), seq), "Q keeps its settled key and resolution");
        assert!(state.can_send() && Arc::ptr_eq(&before.calibration, &after.calibration), "Q stays ready on the same calibration");
        assert!(state.calibrate.status.contains("Saved to P"), "the completion identifies the saved coupon's printer: {}", state.calibrate.status);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rebuild_failure_formats_every_typed_kind_without_parsing() {
        let path = std::env::temp_dir().join("fixture_home/project/hinge.cad.splash");
        let full = path.to_str().unwrap();
        let reason = format!("'{full}' is not open in the project");
        let measured = makepad_csg_mech::Measured { c_z_layers: Some(4), ..Default::default() };
        for (file, line, col, id, op, message, prefix) in [
            (full, 6, 3, "", "", reason.as_str(), "script `hinge.cad.splash`:6:3 ("),
            ("", 1, 1, "", "", "cancelled", "project build ("),
            ("", 4, 2, "", "", "bad expression", "project:4:2 ("),
            ("part.cad.splash", 2, 4, "pa", "BrepFillet", "radius is too large", "`pa` (BrepFillet)"),
            ("part.cad.splash", 3, 5, "h", "joint", "eye too thin", "joint `h`"),
        ] {
            let failure = fab_model::eval::Failure { file: file.into(), line, col, id: id.into(), op: op.into(), lost: None, message: message.into() };
            let kind = classify_rebuild_failure(&failure, None, None);
            let expected = match (id, op) {
                ("", _) => RebuildFailureKind::Program { file: (!file.is_empty()).then(|| "hinge.cad.splash".into()), line, col, reason: message.into() },
                (_, "joint") => RebuildFailureKind::Joint { parts: vec![], kind: None, id: id.into(), reason: message.into() },
                _ => RebuildFailureKind::Solid { parts: vec![], id: id.into(), op: op.into(), reason: message.into() },
            };
            assert_eq!(kind, expected, "structured diagnostic for {prefix}");
            let shown = format_rebuild_failure(&redact_rebuild_kind(&kind, &failure, None), &measured, 0.2);
            assert!(shown.starts_with(prefix), "{prefix} must identify the failed step: {shown}");
            for error in [shown, redacted_eval_failure(&failure)] {
                assert!(!error.contains("fixture_home") && !error.contains(full), "external diagnostic must scrub the script path: {error}");
                if file == full { assert!(error.contains("'hinge.cad.splash' is not open in the project")); }
            }
        }
    }

    #[test]
    fn sliced_printability_issues_block_confirm_card_after_slice_gen_clears() {
        let vp = virtual_printer::presets().remove(0);
        let choice = PrinterChoice::Virtual(vp.name.clone());
        let mut printing = Printing::default();
        let mut state = PrintState { settings: defaults_settings(), ..Default::default() };
        state.links.picked = choice.clone();
        printing.install(&mut state, &choice, fab_printer_link::PrinterLink::virtual_printer(setup_of(&vp, None).unwrap()), true);
        state.printers.set(&choice, printing.installed_epoch, Arc::new(fab_printer_link::Snapshot {
            name: vp.title.clone(),
            model: vp.model(),
            status: fab_printer_link::ConnectionStatus::Connected,
            virtual_printer: true,
            state: Arc::new(fab_printer_link::PrinterState {
                storage: Some(true),
                gcode_state: Some("IDLE".into()),
                ..Default::default()
            }),
            ..Default::default()
        }));
        state.confirm_print = Some(PrintConfirm::Filament(PrintTicket {
            printer: choice,
            virtual_printer: true,
            epoch: printing.installed_epoch,
        }));

        let plate = fleet_plate(&printing);
        let sliced = Arc::new(run_slice(1, &state.settings, None, &plate.plate, &plate.machine, "cube").unwrap());
        let island = Arc::new(fab_printability::Report {
            sliced: true,
            issues: vec![fab_printability::Issue {
                kind: "mid_air",
                severity: fab_printability::Severity::Error,
                object: Some("cube".into()),
                at_mm: [0.0; 3],
                footprint: fab_printability::Footprint { min: [0.0; 2], max: [1.0; 2] },
                layer: Some(11),
                layer_to: None,
                measure: 100.0,
                message: "floating island (100.0 mm²)".into(),
                hint: "Add supports or place it on the bed.".into(),
                fix: None,
                area_mm2: 100.0,
                lowest_z: 2.2,
                highest_z: 2.4,
                flags: 0,
            }],
            ..Default::default()
        });

        // 1. Successful Sliced(1) clears slice_gen (so held Export resumes) and arms issues_gen.
        printing.gen = 1;
        printing.slice_gen = Some(1);
        state.status = SliceStatus::Running { progress: 0.5, stage: "Slicing".into() };
        assert_eq!(
            printing.event(&mut state, &PrintEvent::Sliced { gen: 1, result: Ok(sliced.clone()), seconds: 0.1 }),
            PrintOutcome::Target
        );
        assert_eq!(printing.slice_gen, None, "Sliced clears slice_gen so held Export can resume");
        assert_eq!(printing.issues_gen, Some(1), "Sliced arms issues_gen for streaming printability");
        assert!(state.can_send() && state.print_blocker().is_none());

        // 2. In-progress Issues(1) holds the card while slice_gen is None.
        assert_eq!(
            printing.event(&mut state, &PrintEvent::Issues { gen: 1, report: Arc::default(), done: 3, total: 12 }),
            PrintOutcome::Target
        );
        assert_eq!(printing.slice_gen, None);
        assert_eq!(state.print_blocker().as_deref(), Some("Checking printability: layer 3 of 12"));
        assert_eq!(printing.send_blocked(&state).as_deref(), Some("Checking printability: layer 3 of 12"));

        // 3. Completed Issues(1) with mid_air blocks the confirm card while slice_gen is None.
        assert_eq!(
            printing.event(&mut state, &PrintEvent::Issues { gen: 1, report: island.clone(), done: 12, total: 12 }),
            PrintOutcome::Target
        );
        assert_eq!(printing.slice_gen, None);
        let blocked = printing.send_blocked(&state).expect("floating island must block the confirm card");
        assert!(blocked.starts_with("Not printable: floating island") && blocked.contains("on layer 12"), "{blocked}");
        assert_eq!(printing.send_checks(&state, &state.print_options).err().as_deref(), Some(blocked.as_str()));

        // 4. Another generation's Issues is ignored.
        assert_eq!(
            printing.event(&mut state, &PrintEvent::Issues { gen: 2, report: Arc::default(), done: 12, total: 12 }),
            PrintOutcome::Unchanged
        );
        assert_eq!(printing.send_blocked(&state).as_deref(), Some(blocked.as_str()));

        // 5. Plate change invalidates and drops late Issues(1).
        printing.plate_changed(&mut state, &plate);
        state.streamed = None;
        assert_eq!(printing.issues_gen, None);
        assert_eq!(
            printing.event(&mut state, &PrintEvent::Issues { gen: 1, report: island.clone(), done: 12, total: 12 }),
            PrintOutcome::Unchanged
        );
        assert!(state.streamed.is_none(), "late Issues after plate change must be discarded");

        // 6. Replacement slice gen 2 ignores late Issues(1), and stop_slice clears issues_gen.
        printing.gen = 2;
        printing.slice_gen = Some(2);
        assert_eq!(
            printing.event(&mut state, &PrintEvent::Sliced { gen: 2, result: Ok(sliced.clone()), seconds: 0.1 }),
            PrintOutcome::Target
        );
        assert_eq!((printing.slice_gen, printing.issues_gen), (None, Some(2)));
        assert_eq!(
            printing.event(&mut state, &PrintEvent::Issues { gen: 1, report: island.clone(), done: 12, total: 12 }),
            PrintOutcome::Unchanged
        );
        assert!(state.streamed.is_none() && state.print_blocker().is_none(), "gen 1 report cannot poison gen 2");
        printing.stop_slice(&mut state);
        assert_eq!(printing.issues_gen, None);
        assert_eq!(
            printing.event(&mut state, &PrintEvent::Issues { gen: 2, report: island.clone(), done: 12, total: 12 }),
            PrintOutcome::Unchanged
        );
        assert!(state.streamed.is_none(), "late Issues after stop_slice must be discarded");

        // 7. Marking the preview stale or a failed Sliced(4) rejects subsequent Issues.
        printing.gen = 3;
        printing.slice_gen = Some(3);
        assert_eq!(
            printing.event(&mut state, &PrintEvent::Sliced { gen: 3, result: Ok(sliced), seconds: 0.1 }),
            PrintOutcome::Target
        );
        Printing::mark_shown_slice_stale(&mut state);
        assert_eq!(
            printing.event(&mut state, &PrintEvent::Issues { gen: 3, report: island.clone(), done: 12, total: 12 }),
            PrintOutcome::Unchanged
        );
        assert!(state.streamed.is_none(), "stale preview ignores late Issues");

        printing.gen = 4;
        printing.slice_gen = Some(4);
        assert_eq!(
            printing.event(&mut state, &PrintEvent::Sliced { gen: 4, result: Err("slice failed".into()), seconds: 0.1 }),
            PrintOutcome::Target
        );
        assert_eq!((printing.slice_gen, printing.issues_gen), (None, None));
        assert_eq!(
            printing.event(&mut state, &PrintEvent::Issues { gen: 4, report: island, done: 12, total: 12 }),
            PrintOutcome::Unchanged
        );
        assert!(state.streamed.is_none());
    }

    #[test]
    fn confirm_card_precedence_prefers_calibration_error_over_stale_overlap_and_respects_hold() {
        let vp = virtual_printer::presets().remove(0);
        let choice = PrinterChoice::Virtual(vp.name.clone());
        let snapshot = fab_printer_link::Snapshot { name: vp.title.clone(), virtual_printer: true, ..Default::default() };
        let mut printing = Printing::default();
        let mut p = PrintState::default();
        p.links.picked = choice.clone();
        printing.install(&mut p, &choice, fab_printer_link::PrinterLink::virtual_printer(setup_of(&vp, None).unwrap()), true);
        p.confirm_print = Some(PrintConfirm::Filament(PrintTicket { printer: choice, virtual_printer: true, epoch: printing.installed_epoch }));
        p.stale = true;
        Arc::make_mut(&mut p.printability).issues.push(fab_printability::Issue {
            kind: "overlap", severity: fab_printability::Severity::Error, object: None, at_mm: [0.0; 3],
            footprint: fab_printability::Footprint { min: [0.0; 2], max: [1.0; 2] },
            layer: Some(1), layer_to: None, measure: 1.0, message: "Two bodies overlap".into(),
            hint: "Move them apart.".into(), fix: None, area_mm2: 1.0, lowest_z: 0.0, highest_z: 0.2, flags: 0,
        });
        assert!(p.print_blocker().unwrap().contains("overlap"), "the stale plate has its own refusal");
        let failed = "The hinge cannot rebuild at the measured Z gap.";
        p.calibration_error = Some(failed.into());
        assert_eq!(printing.send_checks(&p, &p.print_options).err().as_deref(), Some(failed), "admission gives the current rebuild failure");
        assert_eq!(printing.send_blocked(&p).as_deref(), Some(failed), "the card shows the same failure");
        assert!(panel::confirm_question(&p, &snapshot, false).0.contains("failed to rebuild"), "the question identifies calibration");
        assert!(panel::slice_status(&p).0.contains(failed), "the status explains how the rebuild failed");
        let _wake = manual_profiles(&mut printing);
        printing.sync_calibration(&mut p);
        p.calibration_error = Some(failed.into());
        assert!(p.calibration_pending, "recovery waits on a real profiles request");
        assert_eq!(printing.send_checks(&p, &p.print_options).err().as_deref(), Some(SLICE_NOT_READY), "admission waits for recovery");
        assert_eq!(printing.send_blocked(&p), None, "the card has no refusal during recovery");
        for text in [panel::confirm_question(&p, &snapshot, false).0, panel::slice_status(&p).0] {
            assert!(text.contains(LOADING_CALIBRATION) && !text.contains(failed) && !text.contains("overlap"), "the hold has one reason: {text}");
        }
        printing.action(&mut p, &PrintAction::AskPrint(true), None);
        assert!(printing.reask == Some(false) && printing.slice_asked(), "held Print survives leaving Sliced view");
        printing.action(&mut p, &PrintAction::AskPrint(false), None);
        assert!(printing.reask.is_none() && !printing.slice_asked(), "closing the Print card releases a held Print wait");
        printing.action(&mut p, &PrintAction::Export, None);
        printing.action(&mut p, &PrintAction::AskPrint(false), None);
        assert!(printing.reask == Some(true) && printing.slice_asked(), "closing the Print card keeps an independent held Export");
        printing.fleet.clear();
        assert_eq!(printing.send_blocked(&p).as_deref(), Some(STALE_TICKET), "a retired ticket is stale");
        assert_eq!(printing.send_checks(&p, &p.print_options).err().as_deref(), Some("No printer is linked."), "a direct admission needs a linked printer");
    }

    const COMPACT_HINGE: &str = r#"use mod.model.*
use mod.fab.*
let m = mod.model
let a = m.box({size: [6mm 2.6mm 6mm] at: [0 1.3mm 0]})
let b = m.box({size: [6mm 2.6mm 6mm] at: [10mm 1.3mm 0]})
let pa = m.union(a, m.cylinder({radius: 3.4mm height: 2.6mm at: [5mm 1.3mm 0]}))
let pb = m.union(b, m.cylinder({radius: 3.4mm height: 2.6mm at: [5mm 1.3mm 0]}))
let h = m.hinge(pa, pb, {at: [5mm 0 0] plate: 0.4mm})
object(pa, name: "Probe")
"#;

    fn copy_done(d: &crate::model_host::Done) -> crate::model_host::Done {
        crate::model_host::Done {
            print_setup_seq: d.print_setup_seq, print_setup_failure: d.print_setup_failure.clone(),
            person: d.person.clone(), id: d.id, ok: d.ok, text: d.text.clone(), images: d.images.clone(),
            review: Default::default(), update: d.update.clone(), status: d.status.clone(), model_changed: d.model_changed,
            check_slice: d.check_slice.clone(), source_files: d.source_files.clone(), source_problem: d.source_problem.clone(),
            capped: d.capped.clone(),
        }
    }

    struct CalibrationFleet {
        app: crate::App,
        fleet: FleetRuntime,
        done: std::sync::mpsc::Receiver<crate::model_host::Done>,
        _wake: std::sync::mpsc::Receiver<()>,
        dir: PathBuf,
        dirs: Arc<crate::dirs::FabDirs>,
        last_setup_seq: Option<u64>,
        coupons: Vec<(fab_calibrate::Step, fab_calibrate::CouponSetup, Arc<fab_calibrate::Coupon>)>,
    }

    impl CalibrationFleet {
        fn new(dir: PathBuf) -> Self {
            let (host_tx, done) = std::sync::mpsc::channel();
            let (tx, events) = std::sync::mpsc::channel();
            // Initialise the process-wide slice pool before the real Cx owns worker actions.
            crate::live_slice::init_for_tests();
            let mut runtime = super::slice_bench::SliceRuntime::with_events(move |event| {
                if let Event::Actions(actions) = event {
                    for a in actions {
                        if let Some(crate::model_host::HostAction::Done(d)) = a.downcast_ref() {
                            host_tx.send(copy_done(d)).unwrap();
                        }
                        if let Some(e) = a.downcast_ref::<PrintEvent>() { tx.send(e.clone()).unwrap(); }
                    }
                }
            });
            runtime.withhold_paint();
            let dirs = Arc::new(crate::dirs::FabDirs::under(&dir));
            let mut app = runtime.cx.with_vm(crate::App::script_new);
            app.dirs = dirs.clone();
            app.host = Some(crate::model_host::ModelHost::start_in(&runtime.cx, false, &dirs));
            let base = virtual_printer::presets().remove(0);
            let cat = Arc::new(catalog_in(&[], &dir, std::slice::from_ref(&base)).unwrap());
            let mut saved = Saved::default();
            for name in ["P", "Q"] {
                let mut twin = cat.twins[&base.machine_preset_name()].printer.clone();
                twin.name = format!("fixture-{name}");
                twin.configured_for = Some(name.into());
                saved.disconnected.insert(name.into());
                saved.printers.push(twin);
            }
            app.printing = Printing { dirs: dirs.clone(), catalog: Some(cat), virtuals: Some(Arc::new(saved)), ..Default::default() };
            app.state.print.settings = defaults_settings();
            let _wake = manual_profiles(&mut app.printing);
            let p = PrinterChoice::Real("P".into());
            let q = PrinterChoice::Real("Q".into());
            assert!(app.printing.link_printer(&mut app.state.print, &p));
            app.printing.ensure_link(&mut app.state.print, &q).unwrap();
            wait_for("both configured printers' stock", || {
                app.printing.poll_printer(&mut app.state.print);
                [&p, &q].into_iter().all(|id| app.state.print.printer_of(id)
                    .is_some_and(|report| report.state.trays().next().is_some()))
            });
            app.printing.sync_calibration(&mut app.state.print);
            for e in drain_cal(&app.printing, &dirs.calibration) { app.printing.event(&mut app.state.print, &e); }
            let mut f = Self { app, fleet: FleetRuntime { runtime, events }, done, _wake, dir, dirs, last_setup_seq: None, coupons: Vec::new() };
            f.app.push_print_setup();
            f
        }

        #[track_caller]
        fn deliver(&mut self, wanted: impl Fn(&crate::model_host::Done) -> bool) -> crate::model_host::Done {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                while let Ok(d) = self.done.try_recv() {
                    if d.print_setup_seq.is_some() {
                        self.last_setup_seq = d.print_setup_seq;
                    }
                    let hit = wanted(&d);
                    self.app.handle_actions(&mut self.fleet.runtime.cx, &[Box::new(crate::model_host::HostAction::Done(Box::new(copy_done(&d))))]);
                    if hit { return d; }
                }
                assert!(std::time::Instant::now() < deadline, "model worker did not deliver the requested completion (seq {}, waiting {}, gate {:?})", self.app.printing.print_setup_seq(), self.app.printing.calibration_pending(), self.app.plate.as_ref().and_then(|p|self.app.printing.plate_gate(&self.app.state.print, p)));
                self.fleet.runtime.turn();
            }
        }

        #[track_caller]
        fn take_setup(&mut self) -> crate::model_host::Done {
            let seq = self.app.printing.print_setup_seq();
            self.deliver(|d| d.print_setup_seq == Some(seq))
        }

        fn open(&mut self, path: PathBuf) -> crate::model_host::Done {
            let id = self.app.host.as_mut().unwrap().open(path, None);
            self.deliver(|d| d.id == id)
        }

        fn resolve(&mut self) {
            for e in drain_cal(&self.app.printing, &self.dirs.calibration) {
                if let PrintEvent::CalibrationWritten { outcome, .. } = &e {
                    match outcome {
                        CalWriteOutcome::Saved { owner, preset, filament, step, values } => {
                            assert!(!preset.is_empty() && !filament.is_empty() && !values.is_empty(), "saved {} under {:?} / {preset} / {filament}", step.title(), owner);
                        }
                        CalWriteOutcome::Cleared { owner, preset, step, has_preset_fallback } => {
                            assert!(!preset.is_empty() && (owner.is_some() || !has_preset_fallback), "cleared {} from the captured target", step.title());
                        }
                        CalWriteOutcome::Failed(error) => panic!("calibration write failed: {error}"),
                    }
                }
                self.app.handle_actions(&mut self.fleet.runtime.cx, &[Box::new(e)]);
            }
        }

        fn save(&mut self, target: &str, step: fab_calibrate::Step, n: usize) {
            let setup = self.app.printing.check_setup(&self.app.state.print);
            let (preset, filament) = setup.calibration_names();
            let cs = setup.coupon_setup();
            let coupon = match self.coupons.iter().find(|(s, c, _)| *s == step && *c == cs) {
                Some((_, _, coupon)) => coupon.clone(),
                None => {
                    let coupon = Arc::new(fab_calibrate::make(step, &cs).unwrap());
                    self.coupons.push((step, cs, coupon.clone()));
                    coupon
                }
            };
            self.app.printing.save_calibration(&mut self.app.state.print, Some(owner(target)), preset, filament, coupon,
                fab_calibrate::Answer::Number(n), "2026-01-01".into());
        }

        fn press(&mut self, action: PrintAction) {
            self.app.handle_actions(&mut self.fleet.runtime.cx, &[Box::new(action)]);
        }

        fn card(&mut self) -> PrintAction {
            self.press(PrintAction::AskPrint(true));
            let p = &mut self.app.state.print;
            p.print_options.plate_clear = true;
            let Some(PrintConfirm::Filament(ticket)) = &p.confirm_print else { panic!("Print opens the target filament card") };
            PrintAction::SendToPrinter { ticket: ticket.clone(), preview: p.preview.as_ref().unwrap().id }
        }

        fn check_hold(&mut self, start: &PrintAction, attempts: u64, gen: u64, stage: &str) {
            self.press(PrintAction::AskPrint(true));
            self.press(start.clone());
            self.press(PrintAction::Export);
            let app = &mut self.app;
            let state = &mut app.state.print;
            assert!(app.printing.last_attempt == attempts && app.printing.packaging.is_empty() && state.sending.is_empty() && app.printing.gen == gen,
                "no old package started while {stage}");
            assert!(!state.can_send(), "the settled plate cannot send while {stage}");
            assert!(state.settled && !state.stale && state.confirm_print.is_some(), "waiting preserves the reviewed slice and card");
            assert_eq!(state.calibration_pending, stage == LOADING_CALIBRATION, "profiles worker stage");
            assert_eq!(state.calibration_plate_pending, stage == UPDATING_CALIBRATION_PLATE, "model worker stage");
            assert!(!app.printing.confirm_slice(state, app.plate.as_ref()), "confirm waits without reslicing");
            assert_eq!(app.printing.send_blocked(state), None, "waiting is not a refusal");
            assert_eq!(state.export, ExportStatus::Exporting, "export stays held");
            let snap = state.printer_of(&state.links.picked).unwrap();
            assert!(panel::confirm_question(state, snap, false).0.contains(stage), "card names {stage}");
            assert_eq!(panel::export_line(state), Some((format!("Exporting · {stage}…"), false)), "export names {stage}");
        }
        fn clear(&mut self, step: fab_calibrate::Step) {
            self.app.printing.clear_calibration(&mut self.app.state.print, step);
            self.resolve();
        }

        fn receive_slice(&mut self) -> Arc<Sliced> {
            let gen = self.app.printing.slice_gen.expect("a fresh slice must have started");
            let event = self.fleet.next(|e| matches!(e, PrintEvent::Sliced { gen: g, .. } if *g == gen));
            self.app.handle_actions(&mut self.fleet.runtime.cx, &[Box::new(event)]);
            assert!(self.app.state.print.can_send(), "the new slice must settle: {:?}", self.app.state.print.status);
            self.app.printing.last.clone().unwrap()
        }

        fn finish_packages(&mut self) {
            while !self.app.printing.packaging.is_empty() {
                let event = self.fleet.next(|e| matches!(e, PrintEvent::Packaged { .. }));
                self.app.handle_actions(&mut self.fleet.runtime.cx, &[Box::new(event)]);
            }
        }

        fn exported(&mut self, slice: &Arc<Sliced>) -> PathBuf {
            assert!(self.app.printing.packaging.iter().any(|p| matches!(p.to, PackageFor::File(_)) && Arc::ptr_eq(&p.sliced, slice)),
                "Export packages the expected settled geometry");
            self.finish_packages();
            let ExportStatus::Written(path) = &self.app.state.print.export else { panic!("Export did not finish: {:?}", self.app.state.print.export) };
            assert!(path.metadata().unwrap().len() > 0, "Export wrote a nonempty package");
            path.clone()
        }

        fn fresh_export(&mut self) -> Arc<Sliced> {
            self.press(PrintAction::Export);
            let slice = if self.app.printing.slice_gen.is_some() { self.receive_slice() } else { self.app.printing.last.clone().unwrap() };
            self.exported(&slice);
            slice
        }

        fn cancel_printer(&mut self) {
            let id = self.app.state.print.sending.first().expect("Start print admitted a package").job.clone();
            self.press(PrintAction::CancelSend(id));
            self.finish_packages();
        }

        fn settle(&mut self) {
            // Opening a plate discovers its colours, resolves its filament and submits its mapping.
            // Dispatch those real worker completions before asking for the first print.
            self.resolve();
            while self.app.printing.plate_gate(&self.app.state.print, self.app.plate.as_deref().unwrap()).is_some() {
                self.deliver(|d| d.update.is_some());
                self.resolve();
            }
            // Wait for any in-flight PrintSetup on the same strand before recording a settled baseline.
            if self.last_setup_seq != Some(self.app.printing.print_setup_seq()) {
                let setup = self.take_setup();
                assert!(setup.ok, "baseline model setup succeeds: {}", setup.text);
            }
            let ev = self.app.plate.clone().unwrap();
            self.fleet.settle(&mut self.app.printing, &mut self.app.state.print, &ev);
        }
    }

    #[test]
    fn calibration_end_to_end_guards() {
        let home = calibration_dir("calibration_end_to_end_guards");
        {
            let mut f = CalibrationFleet::new(home.clone());
            phase_1_saved_plate(&mut f);
            phase_2_hinge_clearance(&mut f);
            phase_3_hinge_failure_and_recovery(&mut f);
            phase_4_navigation_and_box(&mut f);
            phase_5_machine_failures(&mut f);
            f.finish_packages();
            crate::live_slice::wait_idle();
        }
        std::fs::remove_dir_all(&home).unwrap();
    }

    fn assert_hinge_size(ev: &Evaluated, expected_layers: u32, expected_eye: f64, phase: &str) {
        let makepad_csg_mech::JointGeom::Hinge { s0, s1, t_plate, c_z, .. } = &ev.mech[0].plan.joints[0] else {
            panic!("{phase}: expected the calibrated hinge");
        };
        let thickness = s1 - s0;
        let eye = thickness - 2.0 * (t_plate + c_z);
        assert!((eye - expected_eye).abs() < 1e-6 && (thickness / ev.profile.layer - f64::from(expected_layers)).abs() < 1e-5,
            "{phase}: thickness {thickness:.2} mm, plates {t_plate:.2} mm, axial gap {c_z:.2} mm, eye {eye:.2} mm, layer height {:.2} mm", ev.profile.layer);
    }

    fn phase_1_saved_plate(f: &mut CalibrationFleet) {
        let model = fab_model::FabModel::with_script("hinge", "hinge.cad.splash", COMPACT_HINGE);
        let path = f.dir.join("hinge.fab.splash");
        fab_model::project::save(&model, &path).unwrap();
        let open = f.open(path);
        assert!(open.ok, "Phase 1: saved hinge opens: {}", open.text);
        f.settle();
        assert!(f.app.state.print.errors_text().is_none(), "Phase 1: compact hinge has no baseline printability error: {:?}", f.app.state.print.errors_text());
        let app = &mut f.app;
        let gen = app.printing.gen;
        let before = app.plate.clone().unwrap();
        assert_hinge_size(&before, 13, 1.00, "Phase 1 default geometry");
        let slice = app.printing.last.clone().unwrap();
        let revisions = crate::live_slice::revisions();
        assert_eq!(gen, 1, "Phase 1: the first open creates one slice");
        assert!(app.state.print.can_send(), "Phase 1: first open settles");
        let original_parts: Vec<_> = before.plate.parts.iter().map(|p| (&p.object, &p.name)).collect();
        let id = app.host.as_mut().unwrap().reload();
        let reload = f.deliver(|d| d.id == id);
        assert!(reload.ok, "Phase 1: saved hinge reloads: {}", reload.text);
        let after = f.app.plate.as_ref().unwrap();
        assert_eq!((after.content, crate::live_slice::plate_id(after)), (before.content, crate::live_slice::plate_id(&before)), "Phase 1: disk rebuild preserves printable geometry: profiles {:?} -> {:?}, parts {:?} -> {:?}, mapping equal {}", before.profile, after.profile, original_parts,
            after.plate.parts.iter().map(|p| (&p.object, &p.name)).collect::<Vec<_>>(), before.mapped == after.mapped);
        assert_eq!(f.app.printing.gen, gen, "Phase 1: identical disk rebuild starts zero slices");
        assert!(f.app.state.print.can_send() && Arc::ptr_eq(f.app.printing.last.as_ref().unwrap(), &slice), "Phase 1: the settled package remains usable");
        assert_eq!(crate::live_slice::revisions(), revisions, "Phase 1: no-op rebuild leaves the engine revisions unchanged");
        f.app.push_print_setup();
        assert!(f.take_setup().ok, "Phase 1: no-op setup completes");
        assert_eq!(f.app.printing.gen, gen, "Phase 1: no-op setup starts zero slices");
        assert!(f.app.state.print.can_send(), "Phase 1: no-op setup preserves the settled plate");
    }

    fn phase_2_hinge_clearance(f: &mut CalibrationFleet) {
        use fab_calibrate::Step;
        let old = f.app.plate.clone().unwrap();
        assert_eq!(old.profile.measured.c_xy, None, "Phase 2: hinge starts with default radial clearance");
        let old_slice = f.app.printing.last.clone().unwrap();
        let start = f.card();
        let attempts = f.app.printing.last_attempt;
        let gen = f.app.printing.gen;
        f.save("P", Step::Clearance, 5);
        f.check_hold(&start, attempts, gen, LOADING_CALIBRATION);
        f.resolve();
        f.check_hold(&start, attempts, gen, UPDATING_CALIBRATION_PLATE);
        assert!(f.take_setup().ok, "Phase 2: hinge rebuild completes");
        let ev = f.app.plate.as_ref().unwrap();
        assert_eq!(ev.profile.measured.c_xy, Some(0.30), "Phase 2: rebuilt hinge uses the measured radial clearance");
        assert_ne!(ev.content, old.content, "Phase 2: post-joint node keys distinguish the new hinge");
        assert_ne!(crate::live_slice::plate_id(ev), crate::live_slice::plate_id(&old), "Phase 2: changed geometry changes the plate id");
        assert_eq!(f.app.printing.last_attempt, attempts, "Phase 2: changed geometry drops the held start and export waits for the new slice");
        assert!(f.app.state.print.sending.is_empty() && f.app.printing.gen > gen && f.app.printing.export_waits, "Phase 2: Export waits for the new generation and Start stays held back");
        assert_eq!(f.app.state.print.print_blocked.get(&f.app.state.print.links.picked), Some(STALE_PREVIEW), "Phase 2: old preview is refused");
        let new = f.receive_slice();
        assert_ne!(new.preview.id, old_slice.preview.id, "Phase 2: replacement preview has a new id");
        assert_ne!(new.settled.gcode.text().unwrap(), old_slice.settled.gcode.text().unwrap(), "Phase 2: joint clearance changes the sliced hinge");
        f.exported(&new);
        f.app.sync_confirm_card();
        assert!(f.app.state.print.send_blocked.is_none(), "Phase 2: replacement slice enables Start print");
        f.press(start);
        assert!(f.app.state.print.sending.is_empty() && f.app.printing.packaging.is_empty(), "Phase 2: a press carrying the old hinge preview cannot package");
        let start = f.card();
        f.press(start);
        assert!(f.app.printing.packaging.iter().any(|p| matches!(p.to, PackageFor::Printer(_)) && Arc::ptr_eq(&p.sliced, &new)), "Phase 2: the fresh Start packages the new hinge");
        f.cancel_printer();
    }

    fn phase_3_hinge_failure_and_recovery(f: &mut CalibrationFleet) {
        use fab_calibrate::Step;
        assert!(f.app.state.print.can_send(), "Phase 3: Phase 2's settled hinge is ready");
        let start = f.card();
        let gen = f.app.printing.gen;
        let attempt = f.app.printing.last_attempt;
        let before = f.app.plate.clone().unwrap();
        let old_slice = f.app.printing.last.clone().unwrap();
        assert_eq!(before.profile.measured.c_z_layers, None, "Phase 3: baseline hinge uses the default Z gap");
        f.save("P", Step::ZGap, 4);
        f.check_hold(&start, attempt, gen, LOADING_CALIBRATION);
        f.resolve();
        assert!(f.app.state.print.calibration_plate_pending, "Phase 3: Z gap must reach the model worker before releasing held operations");
        let d = f.take_setup();
        f.app.sync_confirm_card();
        assert!(!d.ok && d.update.is_none() && d.print_setup_seq == Some(f.app.printing.print_setup_seq()), "Phase 3: the active rebuild failed without replacing the plate");
        let Some(crate::model_host::PrintSetupFailure::Eval(raw)) = &d.print_setup_failure else {
            panic!("expected typed hinge failure, got {:?}: {}", d.print_setup_failure, d.text);
        };
        assert_eq!((&*raw.id, &*raw.op), ("h", "joint"), "Phase 3: the model worker identifies the failed joint");
        assert!(raw.message.contains("2.60 mm along the pin leaves an eye of 0.20 mm (plates 0.40, gaps 0.80)"), "Phase 3: the four-layer gap leaves a 0.20 mm eye: {}", raw.message);
        let recorded = f.app.printing.profile_rebuild_error.as_ref().expect("the app retains the typed failure");
        assert_eq!(recorded.kind, RebuildFailureKind::Joint { parts: vec!["Probe pa".into(), "Probe pb".into()], kind: Some("hinge"), id: "h".into(),
            reason: raw.message.lines().next().unwrap().trim().into() }, "Phase 3: typed failure identifies the affected printed part");
        assert_eq!(recorded.source, raw.clone(), "Phase 3: typed failure retains the evaluator diagnostic");
        assert_eq!(recorded.measured.c_z_layers, Some(4), "Phase 3: the typed failure retains the four-layer gap");
        assert!((recorded.layer - 0.20).abs() < 1e-6, "Phase 3: the diagnostic uses the active layer height");
        let state = &f.app.state.print;
        let err = state.calibration_error.as_ref().expect("the person can see why the rebuild failed");
        assert!(err.contains("Z gap 4 layers") && err.contains("0.80 mm") && !err.contains('\n'), "Phase 3: the diagnostic includes the axial gap on a single line");
        assert_eq!(state.send_blocked.as_ref(), Some(err), "Phase 3: the confirm card explains the failed rebuild");
        assert!(panel::confirm_question(state, state.target_printer().unwrap(), false).0.contains("failed to rebuild for this calibration"), "Phase 3: the question explains the failed rebuild");
        let status = panel::slice_status(state).0;
        assert!(status.starts_with("Export failed — calibration rebuild failed:") && status.contains("Probe pa") && status.contains("Z gap 4 layers"), "Phase 3: {status}");
        assert!(panel::export_line(state).is_some_and(|(s, error)| error && s.starts_with("Export failed:") && s.contains("Probe pb") && s.contains("Z gap 4 layers")), "Phase 3: Export shows the affected part and gap");
        let blocked = |app: &crate::App| {
            assert_eq!(app.printing.gen, gen, "Phase 3: a failed profile may never slice the old hinge");
            assert_eq!(app.printing.last_attempt, attempt, "Phase 3: a failed profile may never package the old hinge");
            let p = &app.state.print;
            assert!(p.sending.is_empty() && !p.can_send() && p.calibration_error.is_some() && !p.calibration_plate_pending, "Phase 3: failed geometry blocks admission with no work in flight");
            assert!(matches!(p.status, SliceStatus::Failed(_)) && matches!(p.export, ExportStatus::Failed(_)), "Phase 3: slice and export retain the failure");
        };
        blocked(&f.app);
        f.app.slice_view_follow(&mut f.fleet.runtime.cx);
        blocked(&f.app);
        for action in [PrintAction::Export, PrintAction::Slice, PrintAction::AskPrint(true), start] {
            f.press(action);
            blocked(&f.app);
        }
        f.app.request_reslice();
        blocked(&f.app);
        f.app.printing.live_input(&mut f.app.state.print, f.app.plate.as_ref(), Arc::from([]), None);
        blocked(&f.app);
        f.app.printing.confirm_slice(&mut f.app.state.print, f.app.plate.as_ref());
        blocked(&f.app);
        f.press(PrintAction::SetPlate("High Temp Plate".into()));
        let retry = f.take_setup();
        assert!(!retry.ok && retry.update.is_none(), "Phase 3: retry must rebuild even though its profile already matches");
        assert!(matches!(&retry.print_setup_failure, Some(crate::model_host::PrintSetupFailure::Eval(e)) if e.id == "h" && e.op == "joint"), "Phase 3: the retry preserves the typed joint failure");
        f.save("P", Step::Flow, 1);
        f.resolve();
        assert!(f.app.state.print.calibration_error.is_some() && !f.app.state.print.calibration_plate_pending, "Phase 3: Flow alone cannot repair hinge geometry");
        assert_eq!(f.app.printing.gen, gen, "Phase 3: a Flow-only write starts no slice");

        f.save("P", Step::ZGap, 1);
        f.resolve();
        f.app.handle_actions(&mut f.fleet.runtime.cx, &[Box::new(crate::model_host::HostAction::Done(Box::new(copy_done(&retry))))]);
        assert!(f.app.state.print.calibration_error.is_none() && f.app.state.print.calibration_plate_pending, "Phase 3: an older failed completion cannot poison the corrected gap");
        let corrected_setup = f.take_setup();
        assert!(corrected_setup.ok, "Phase 3: one gap layer rebuilds the hinge: {}", corrected_setup.text);
        let ev = f.app.plate.as_ref().unwrap();
        assert_hinge_size(ev, 13, 1.40, "Phase 3 corrected Z gap");
        assert_eq!(ev.profile.measured.c_z_layers, Some(1), "Phase 3: the corrected profile uses one layer");
        assert_ne!(ev.content, before.content, "Phase 3: the corrected gap changes the geometry");
        assert!(f.app.printing.profile_rebuild_error.is_none(), "Phase 3: correcting the gap releases the typed failure");
        let corrected = f.fresh_export();
        assert_ne!(old_slice.settled.gcode.text().unwrap(), corrected.settled.gcode.text().unwrap(), "Phase 3: the corrected eye changes the exported G-code");
        let assert_recovered = |f: &CalibrationFleet| {
            let p = &f.app.state.print;
            assert!(p.calibration_error.is_none() && !p.calibration_plate_pending && f.app.printing.profile_rebuild_error.is_none()
                && f.app.printing.plate_gate(p, f.app.plate.as_ref().unwrap()).is_none(), "Phase 3: the rebuilt plate is ready");
        };
        let fail_again = |f: &mut CalibrationFleet| {
            f.save("P", Step::ZGap, 4);
            f.resolve();
            assert!(!f.take_setup().ok && f.app.state.print.calibration_error.is_some(), "Phase 3: four gap layers fail on the thin hinge");
        };
        fail_again(f);
        f.clear(Step::ZGap);
        assert!(f.take_setup().ok && f.app.plate.as_ref().unwrap().profile.measured.c_z_layers.is_none(), "Phase 3: clearing the gap restores defaults");
        assert!(f.app.printing.profile_rebuild_error.is_none(), "Phase 3: clearing the gap releases the typed failure");
        assert_recovered(f);

        // Recovery D: retarget to Q while P's four-layer gap fails on the 2.6 mm hinge.
        fail_again(f);
        let owner = EditOwner::new(PrinterScope::Target, &f.app.state.print);
        f.press(PrintAction::LinkPrinter(owner, PrinterChoice::Real("Q".into())));
        assert!(f.app.state.print.calibration_pending && f.app.state.print.calibration_error.is_none(), "Phase 3: Q starts with its own calibration");
        f.resolve();
        let q_setup = f.take_setup();
        assert!(q_setup.ok && f.app.plate.as_ref().unwrap().profile.measured == Default::default(), "Phase 3: Q builds with its own default profile: {}", q_setup.text);
        assert!(f.app.printing.profile_rebuild_error.is_none(), "Phase 3: retarget releases P's failed calibration");
        assert_recovered(f);
        f.press(PrintAction::Export);
        assert!(f.app.printing.slice_gen.is_some(), "Phase 3: Q can start exporting its recovered plate");
        f.app.printing.stop_slice(&mut f.app.state.print);

        // Returning to P reproduces P's persisted four-layer gap failure without rebuilding a throwaway baseline.
        let owner = EditOwner::new(PrinterScope::Target, &f.app.state.print);
        f.press(PrintAction::LinkPrinter(owner, PrinterChoice::Real("P".into())));
        f.resolve();
        assert!(!f.take_setup().ok && f.app.state.print.calibration_error.is_some(), "Phase 3: returning to P reproduces its four-layer failure");

        // Recovery C: editing the model to 3.2 mm recovers under P's four-layer gap.
        let id = f.app.host.as_mut().unwrap().set_source("hinge.cad.splash".into(), COMPACT_HINGE.replace("2.6mm", "3.2mm").replace("1.3mm", "1.6mm"));
        let thick = f.deliver(|d| d.id == id);
        assert!(thick.ok && f.app.plate.as_ref().unwrap().profile.measured.c_z_layers == Some(4), "Phase 3: a thicker part builds at four layers: {}", thick.text);
        assert_hinge_size(f.app.plate.as_ref().unwrap(), 16, 0.80, "Phase 3 thicker edit");
        assert!(f.app.printing.profile_rebuild_error.is_none(), "Phase 3: editing the model recovers without changing calibration");
        assert_recovered(f);

        // Plate switch under P's four-layer gap: beta builds cleanly and stays on the bed when reopening the 2.6 mm hinge fails.
        let beta = f.dir.join("beta.cad.splash");
        std::fs::write(&beta, "use mod.fab.*\nobject(mod.model.box({size: [1.2mm 0.2mm 1.2mm]}))").unwrap();
        assert!(f.open(beta).ok && f.app.state.print.calibration_error.is_none(), "Phase 3: another plate builds under the failed hinge's calibration");
        let path = f.dir.join("hinge.fab.splash");
        assert!(!f.open(path).ok, "Phase 3: opening the thin hinge at four layers refuses its old geometry");
    }

    fn phase_4_navigation_and_box(f: &mut CalibrationFleet) {
        use fab_calibrate::Step;
        while f.fleet.events.try_recv().is_ok() {}
        for step in [Step::Clearance, Step::ZGap, Step::Flow] {
            f.app.printing.clear_calibration(&mut f.app.state.print, step);
        }
        f.resolve();
        f.press(PrintAction::SetPlate("Textured PEI Plate".into()));
        f.settle();
        let seq = f.app.printing.calibration_seq;
        let original = f.app.printing.check_setup(&f.app.state.print);
        for shown in [Some(PrinterChoice::Real("Q".into())), None] {
            f.press(PrintAction::ShowPrinter(shown));
            let p = f.app.state.print.links.picked.clone();
            fab::print::follow::reported(&mut f.app.state.print, &p, 5.0);
            let app = &mut f.app;
            assert!(!app.printing.sync_calibration(&mut app.state.print), "Phase 4: an unchanged target report requires no resolution");
            assert_eq!(app.printing.calibration_seq, seq, "Phase 4: navigation and reports never queue a calibration read");
            let current = app.printing.check_setup(&app.state.print);
            assert_eq!(current.calibration_owner(), Some(&owner("P")), "Phase 4: the target owns the calibration");
            assert_eq!(app.state.print.calibrate.owner_id.as_deref(), Some("printer:P"), "Phase 4: the calibration panel stays on the target");
            assert_eq!(current.id(), original.id(), "Phase 4: showing another printer preserves the settled setup");
            assert!(Arc::ptr_eq(&current.calibration, &original.calibration), "Phase 4: the immutable snapshot is retained");
        }
        let start = f.card();
        let id = f.app.printing.check_setup(&f.app.state.print).id();
        let seq = f.app.printing.calibration_seq;
        let original_gen = f.app.printing.gen;
        let original_slice = f.app.printing.last.clone().unwrap();
        f.save("Q", fab_calibrate::Step::Flow, 1);
        assert!(!f.app.state.print.calibration_pending && f.app.state.print.can_send(), "Phase 4: Q's save leaves P ready immediately");
        f.press(PrintAction::AskPrint(true));
        assert_eq!(f.app.printing.send_blocked(&f.app.state.print), None, "Phase 4: Q's queued write leaves P's card ready");
        f.press(PrintAction::Export);
        f.exported(&original_slice);
        f.press(start);
        assert!(f.app.printing.packaging.iter().any(|p| matches!(p.to, PackageFor::Printer(_)) && Arc::ptr_eq(&p.sliced, &original_slice)),
            "Phase 4: P starts the reviewed package while Q's write is queued");
        f.cancel_printer();
        f.resolve();
        let (_, filament) = f.app.printing.check_setup(&f.app.state.print).calibration_names();
        assert_eq!(fab_calibrate::Results::load_owner_in(&f.dirs.calibration, &owner("Q").id, &filament).get(fab_calibrate::results::names::FLOW_RATIO), Some(0.90), "Phase 4: Q owns its flow answer");
        assert_eq!(f.app.printing.gen, original_gen, "Phase 4: P's slice stays settled through Q's write");
        let start = f.card();
        assert_eq!(f.app.printing.calibration_seq, seq, "Phase 4: Q's save never resolves P again");
        assert_eq!(f.app.printing.check_setup(&f.app.state.print).id(), id, "Phase 4: Q's flow leaves P's slice key unchanged");
        let attempts = f.app.printing.last_attempt;
        let gen = f.app.printing.gen;
        let sliced = f.app.printing.last.as_ref().unwrap().clone();
        let plate_id = crate::live_slice::plate_id(f.app.plate.as_ref().unwrap());
        f.save("P", fab_calibrate::Step::Clearance, 5);
        f.check_hold(&start, attempts, gen, LOADING_CALIBRATION);
        f.resolve();
        f.check_hold(&start, attempts, gen, UPDATING_CALIBRATION_PLATE);
        let done = f.take_setup();
        assert!(done.ok, "Phase 4: the box rebuilds: {}", done.text);
        let ev = f.app.plate.as_ref().unwrap();
        assert_eq!(ev.profile.measured.c_xy, Some(0.3), "Phase 4: box is rebuilt under P's clearance");
        assert_eq!(crate::live_slice::plate_id(ev), plate_id, "Phase 4: fixed box geometry is unchanged");
        assert_eq!(f.app.printing.gen, gen, "Phase 4: fixed box reuses the Normal mode slice");
        assert!(f.app.state.print.can_send() && !f.app.state.print.calibration_plate_pending, "Phase 4: matching rebuild releases the card");
        assert_eq!(f.app.state.print.sending.len(), 1, "Phase 4: the held start resumes exactly once");
        let packages = &f.app.printing.packaging;
        assert!(packages.iter().any(|p| matches!(p.to, PackageFor::Printer(_)) && Arc::ptr_eq(&p.sliced, &sliced)), "Phase 4: Start uses the settled box");
        assert!(packages.iter().any(|p| matches!(p.to, PackageFor::File(_)) && Arc::ptr_eq(&p.sliced, &sliced)), "Phase 4: Export uses the settled box");
        use fab_calibrate::Answer;
        f.cancel_printer();
        for (step, target) in [(Step::Flow, Some(owner("P"))), (Step::PressureAdvance, None)] {
            let setup = f.app.printing.check_setup(&f.app.state.print);
            let before = setup.id();
            let (preset, filament) = setup.calibration_names();
            let coupon = Arc::new(fab_calibrate::make(step, &setup.coupon_setup()).unwrap());
            let n = 2;
            let start = f.card();
            let attempts = f.app.printing.last_attempt;
            let gen = f.app.printing.gen;
            if target.is_none() {
                f.app.printing.save_calibration(&mut f.app.state.print, None, "Unrelated preset".into(), filament.clone(),
                    coupon.clone(), Answer::Number(n), "2026-01-01".into());
                assert!(!f.app.state.print.calibration_pending && f.app.state.print.can_send(), "Phase 4: unrelated preset leaves P usable");
                f.resolve();
                assert_eq!(f.app.printing.check_setup(&f.app.state.print).id(), before, "Phase 4: the unrelated preset preserves the key");
            }
            f.app.printing.save_calibration(&mut f.app.state.print, target, preset, filament, coupon.clone(), Answer::Number(n), "2026-01-01".into());
            f.check_hold(&start, attempts, gen, LOADING_CALIBRATION);
            f.resolve();
            assert_ne!(f.app.printing.check_setup(&f.app.state.print).id(), before, "Phase 4: a changed slicer value changes the input key for {step:?}");
            assert_eq!(f.app.printing.last_attempt, attempts, "Phase 4: the old package is rejected before {step:?} reslices");
            assert!(f.app.state.print.sending.is_empty() && f.app.state.print.confirm_print.is_some(), "Phase 4: the superseded Start leaves its confirm card open");
            let p = &f.app.state.print.links.picked;
            assert_eq!(f.app.state.print.print_blocked.get(p), Some(STALE_PREVIEW), "Phase 4: the changed settings require a fresh Start press");
            assert!(f.app.printing.gen > gen && f.app.printing.export_waits, "Phase 4: Export waits for the new calibrated slice");
            let new = f.receive_slice();
            let path = f.exported(&new);
            let mut file = std::fs::File::open(path).unwrap();
            let zip = makepad_zip_file::zip_read_central_directory(&mut file).unwrap();
            let entry = zip.file_headers.iter().find(|e| e.file_name == "Metadata/project_settings.config").unwrap().extract(&mut file).unwrap();
            let config = makepad_strict_json::parse(&entry).unwrap();
            let (key, expected) = match step {
                Step::Flow => ("filament_flow_ratio", format!("{:.3}", coupon.variant(n).unwrap().value)),
                _ => ("pressure_advance", format!("{:.4}", coupon.variant(n).unwrap().value)),
            };
            assert_eq!(config.get(key).and_then(|v| v.as_arr()).and_then(|a| a.first()).and_then(|v| v.as_str()), Some(expected.as_str()), "Phase 4: the exported package uses the resolved {key}");
            if step == Step::Flow {
                f.app.sync_confirm_card();
                assert!(f.app.state.print.send_blocked.is_none(), "Phase 4: the new flow preview is ready to start");
                let new_start = f.card();
                f.press(new_start);
                assert!(f.app.printing.packaging.iter().any(|p| matches!(p.to, PackageFor::Printer(_)) && Arc::ptr_eq(&p.sliced, &new)), "Phase 4: Start packages the new flow slice");
                f.cancel_printer();
            }
        }
    }

    fn phase_5_machine_failures(f: &mut CalibrationFleet) {
        use crate::model_host::{reason, PrintSetupFailure};
        f.press(PrintAction::AskPrint(false));
        f.clear(fab_calibrate::Step::Clearance);
        assert!(f.take_setup().ok, "Phase 5: clear the box clearance before checking machine failures");
        let ev = f.app.plate.clone().unwrap();
        assert_eq!(ev.profile.measured, Default::default(), "Phase 5: the box uses default mechanics");
        let imports = f.dir.join("fixture_home/presets");
        let machines = imports.join("machine");
        let filaments = imports.join("filament");
        std::fs::create_dir_all(&machines).unwrap();
        std::fs::create_dir_all(&filaments).unwrap();
        let base = virtual_printer::presets().remove(0);
        let long = format!("192.0.2.7-{}", "x".repeat(100));
        let cases = [
            ("Home Lab Printer", "home_lab.json", None),
            ("Workshop Short", "workshop_short.json", Some("192.0.2.7")),
            ("Workshop Long", "workshop_long.json", Some(long.as_str())),
        ];
        let write_machine = |name: &str, filename: &str, slot: Option<&str>| {
            let slots = slot.map(|raw| format!(r#","fab_material_slots":["1:A1","{raw}"]"#)).unwrap_or_default();
            std::fs::write(machines.join(filename), format!(r#"{{"type":"machine","name":"{name}","inherits":"{}"{slots}}}"#, base.machine_preset_name())).unwrap();
        };
        for (name, file, slot) in cases { write_machine(name, file, slot); }
        write_machine("Live Preset", "live_preset.json", None);
        std::fs::write(filaments.join("pla.json"), format!(r#"{{"type":"filament","name":"Fab PLA","inherits":"{}"}}"#, virtual_printer::filament_preset("PLA"))).unwrap();
        f.app.printing.catalog = Some(Arc::new(catalog_in(&[], &imports, std::slice::from_ref(&base)).unwrap()));
        std::fs::remove_file(machines.join("home_lab.json")).unwrap();
        let snapshot = f.app.state.print.target_printer().unwrap().clone();
        for (name, filename, slot) in cases {
            let file = machines.join(filename);
            f.app.state.print.settings.use_installed = true;
            f.app.state.print.settings.machine = name.to_owned();
            f.app.printing.sync_calibration(&mut f.app.state.print);
            f.resolve();
            let setup = f.app.printing.check_setup(&f.app.state.print);
            let err = setup.machine_typed().expect_err("invalid installed machine must have a typed reason");
            let expected = if let Some(raw) = slot {
                assert!(matches!(&err, MachineError::InvalidMaterialSlots { preset, slot_index: Some(2), raw_entry: Some(value), .. } if preset == name && value == raw), "Phase 5: {err:?}");
                format!("the printer preset `{name}` has an invalid `fab_material_slots` entry")
            } else {
                assert!(matches!(&err, MachineError::ResolvePreset { preset, detail } if preset == name && detail.contains(file.to_str().unwrap())), "Phase 5: {err:?}");
                format!("could not read the printer preset `{name}`")
            };
            f.app.push_print_setup();
            let d = f.take_setup();
            assert!(matches!(&d.print_setup_failure, Some(PrintSetupFailure::Machine(e)) if e == &err), "Phase 5: worker preserves typed machine error");
            let state = &f.app.state.print;
            assert!(state.calibration_error.is_none() && !state.calibration_plate_pending && !state.can_send(), "Phase 5: machine failure appears on the printer's slice status");
            for local in [err.local_message(), panel::slice_status(state).0, panel::confirm_question(state, &snapshot, false).0] {
                assert!(local.contains(name) && !local.contains('\n'), "Phase 5: the local reason names its preset on one line: {local}");
                if slot.is_none() { assert!(local.contains(file.to_str().unwrap()), "Phase 5: the local reason identifies the missing file: {local}"); }
                else {
                    assert!(local.contains("entry 2") && local.contains("192.0.2.7"), "Phase 5: the local reason identifies the failing slot: {local}");
                    if slot == Some(&*long) { assert!(local.contains('…') && !local.contains(&"x".repeat(80)), "Phase 5: clip only the long slot: {local}"); }
                }
            }
            f.app.printing.check_slice(31, state, &ev);
            let PrintEvent::Checked { error: Some(checked), .. } = f.fleet.next(|e| matches!(e, PrintEvent::Checked { id: 31, .. })) else { panic!("checked error") };
            let external = f.app.printing.external_slice_error(state).unwrap();
            let read = makepad_strict_json::parse(br#"{"action":"read"}"#).unwrap();
            let test_print = f.app.test_print_answer(&mut f.fleet.runtime.cx, &read);
            let node = makepad_script::syntax::parse_value(&test_print.text).unwrap();
            let read_error = node.field("error").unwrap().value.string().unwrap();
            for (exit, result) in [
                ("host completion", reason(&d.text)), ("check", checked), ("slice", external),
                ("test_print read", read_error.into()), ("model job", setup.model_job(&ev).err().unwrap()),
                ("job setup", setup.job_setup_of(&ev.plate, &ev.machine, None).err().unwrap()),
            ] {
                assert_eq!(result, expected, "Phase 5: {exit} carries the failing step and preset");
            }
            for text in std::iter::once(&*test_print.text).chain(f.app.state.ui.notices.iter().map(|n| n.text.as_str())) {
                assert!(!["fixture_home", "home_lab.json", "192.0.2.7", "xxxx"].iter().any(|v| text.contains(v)), "Phase 5: private machine details in outbound context");
            }
        }
        // The preset can disappear between the worker resolving calibration and rebuilding
        // the plate. Exercise the failure with a settled preview already on the card.
        let name = "Live Preset";
        let file = machines.join("live_preset.json");
        f.app.state.print.settings.machine = name.into();
        f.app.state.print.settings.process = virtual_printer::PROCESS.into();
        f.app.printing.load_base_values(&mut f.app.state.print);
        // Apply the installed profile's tray mapping before capturing the coupon's filament.
        f.app.remap();
        f.app.printing.sync_calibration(&mut f.app.state.print);
        f.settle();
        let start = f.card();
        let (gen, attempts) = (f.app.printing.gen, f.app.printing.last_attempt);
        f.save("P", fab_calibrate::Step::Clearance, 5);
        std::fs::remove_file(&file).unwrap();
        f.resolve();
        assert!(f.app.state.print.calibration_plate_pending, "Phase 5: the plate awaits its new clearance");
        let failed = f.take_setup();
        assert!(matches!(failed.print_setup_failure, Some(PrintSetupFailure::Machine(_))), "Phase 5: the worker reports a typed machine failure");
        for action in [PrintAction::Slice, PrintAction::Export, PrintAction::AskPrint(true), start] {
            f.press(action);
            let state = &f.app.state.print;
            assert!(!state.calibration_plate_pending && state.calibration_error.is_none() && !state.can_send(), "Phase 5: the invalid machine blocks the old plate");
            assert_eq!((f.app.printing.gen, f.app.printing.last_attempt), (gen, attempts), "Phase 5: machine failure must never slice or package old geometry");
        }
        std::fs::write(&file, format!(r#"{{"type":"machine","name":"{name}","inherits":"{}"}}"#, base.machine_preset_name())).unwrap();
        f.app.printing.load_base_values(&mut f.app.state.print);
        f.app.remap();
        f.resolve();
        f.app.push_print_setup();
        f.app.printing.sync_calibration_plate(&mut f.app.state.print, f.app.plate.as_deref(), true);
        assert!(f.app.printing.machine_error().is_none() && f.app.state.print.calibration_plate_pending, "Phase 5: restored machine resumes the mismatched plate: error {:?}, phase one {}, plate {:?}, effective {:?}, settings {:?}", f.app.printing.machine_error(), f.app.state.print.calibration_pending, f.app.plate.as_ref().map(|p| &p.profile.measured), f.app.printing.check_setup(&f.app.state.print).mech_measured(), f.app.state.print.settings.machine);
        assert!(f.take_setup().ok, "Phase 5: restored machine can rebuild the new clearance");
        assert_eq!(f.app.plate.as_ref().unwrap().profile.measured.c_xy, Some(0.30), "Phase 5: restoring the machine applies the new clearance");
    }

    fn fleet_plate(printing: &Printing) -> Evaluated {
        // One mesh layer in the plate's first colour is enough to map and settle a print.
        let model = fab_model::FabModel::with_script("cube", "cube.cad.splash",
            "use mod.fab.*\nobject(mod.model.box({size: [1.2mm 0.2mm 1.2mm]}))");
        let mut evaluator = fab_model::eval::Evaluator::new(printing.machine().unwrap().clone());
        model.evaluate(&mut evaluator, None).unwrap().1
    }

    fn poll_fleet(printing: &mut Printing, state: &mut PrintState, plate: &mut Evaluated) -> Polled {
        let polled = printing.poll_printer(state);
        if polled.target {
            state.bump();
            remap(printing, state, plate);
            crate::live_slice::set_print_setup(printing.check_setup(state));
        }
        polled
    }

    struct FleetRuntime {
        runtime: super::slice_bench::SliceRuntime,
        events: std::sync::mpsc::Receiver<PrintEvent>,
    }

    impl FleetRuntime {
        fn next(&mut self, wanted: impl Fn(&PrintEvent) -> bool) -> PrintEvent {
            loop {
                while let Ok(event) = self.events.try_recv() {
                    if wanted(&event) { return event; }
                }
                self.runtime.turn();
            }
        }

        fn settle(&mut self, printing: &mut Printing, state: &mut PrintState, plate: &Evaluated) {
            printing.slice(state, Some(&Arc::new(plate.clone())));
            assert!(printing.slice_job.is_some(), "the mapped plate must reach the slicer: {:?}, gate {:?}", state.status, printing.plate_gate(state, plate));
            while printing.slice_gen.is_some() && !state.settled {
                while let Ok(event) = self.events.try_recv() {
                    if matches!(event, PrintEvent::Sliced { .. }) { printing.event(state, &event); }
                }
                if !state.settled && !printing.reconcile(state) { self.runtime.turn(); }
            }
            assert!(state.settled && matches!(state.status, SliceStatus::Done { .. }), "the plate must have a settled slice: {:?}", state.status);
            crate::live_slice::wait_idle();
            let (input, served) = crate::live_slice::revisions().unwrap();
            assert_eq!(served, input, "the settled input must be the served one once the engine rests");
        }
    }

    fn settle_fleet_plate(printing: &mut Printing, state: &mut PrintState, plate: &Evaluated) -> FleetRuntime {
        crate::live_slice::init_for_tests();
        let (tx, events) = std::sync::mpsc::channel();
        let mut runtime = super::slice_bench::SliceRuntime::with_events(move |event| {
            if let Event::Actions(actions) = event {
                for action in actions {
                    if let Some(event @ (PrintEvent::TargetModelChanged | PrintEvent::Sliced { .. } | PrintEvent::Packaged { .. } |
                        PrintEvent::JobChecked { .. } | PrintEvent::JobFilesRemoved(_) | PrintEvent::JobPreview { .. })) = action.downcast_ref::<PrintEvent>() {
                        tx.send(event.clone()).unwrap();
                    }
                }
            }
        });
        runtime.withhold_paint();
        let mut fleet = FleetRuntime { runtime, events };
        fleet.settle(printing, state, plate);
        fleet
    }

    const WIDE_MACHINE: &str = "Wide bed";

    fn write_wide_machine(imports: &Path, parent: &VirtualPrinter, width: u32, nozzle: f64) {
        let machines = imports.join("machine");
        std::fs::create_dir_all(&machines).unwrap();
        std::fs::write(machines.join("wide.json"), format!(
            r#"{{"type":"machine","name":"{WIDE_MACHINE}","inherits":"{}","printable_area":["0x0","{width}x0","{width}x320","0x320"],"nozzle_diameter":["{nozzle}"]}}"#,
            parent.machine_preset_name())).unwrap();
    }

    fn check_installed_machine_selection(fleet: &mut FleetRuntime, home: &Path, parent: &VirtualPrinter) {
        use makepad_fab_settings::profiles::SLICER_APPS;
        let dir = home.join("selected");
        std::fs::create_dir(&dir).unwrap();
        let conf = dir.join(format!("{}.conf", SLICER_APPS[0]));
        std::fs::write(&conf, format!(r#"{{"presets":{{"machine":"{WIDE_MACHINE}","process":"{}","filaments":["{}"]}}}}"#,
            virtual_printer::PROCESS, virtual_printer::filament_preset("PLA"))).unwrap();
        let catalog = || PrintEvent::Catalog(Some(Arc::new(catalog_in(std::slice::from_ref(&dir), &home.join("imports"), std::slice::from_ref(parent)).unwrap())));
        let selected = catalog();
        let model = parent.model();
        let first = parent.machine_preset_name();
        let report = dir.join("a.jsonl");
        std::fs::write(&report, "@head 0 1 PLA 1969D4FF\n").unwrap();
        let a = PrinterChoice::Real("A".into());
        let mut printing = Printing { virtuals: Some(Default::default()), ..Default::default() };
        let mut state = PrintState { settings: defaults_settings(), ..Default::default() };
        state.set_configured_printers(&["A".into()]);
        printing.install(&mut state, &a, fab_printer_link::PrinterLink::simulate_moonraker(report, &model), false);
        printing.link_printer(&mut state, &a);
        printing.event(&mut state, &selected);
        assert_eq!(state.settings.machine, WIDE_MACHINE, "startup must adopt the installed machine before A identifies itself");
        state.settled = true;
        wait_for("A's first model report", || {
            printing.poll_printer(&mut state);
            state.target_printer().is_some_and(|s| s.model == model)
        });
        let bound = state.printers.binding(&a).unwrap().machine().unwrap();
        assert_eq!((bound.geometry.name.as_str(), bound.diameters.as_slice()), (WIDE_MACHINE, &[0.6][..]),
            "automatic must use the installed second preset and its 0.6 mm nozzle");
        fleet.runtime.cx.handle_action_receiver();
        assert!(!fleet.events.try_iter().any(|ev| matches!(ev, PrintEvent::TargetModelChanged)), "A's first report must not request a machine switch");
        assert!(state.settled && state.settings.machine == WIDE_MACHINE, "A's first binding must preserve the settled plate and the installed selection");
        printing.event(&mut state, &selected);
        assert_eq!(state.settings.machine, WIDE_MACHINE, "a catalogue delivered after A's report must also keep the installed selection");

        let (items, names) = panel::machine_menu(&state);
        assert_eq!(&*names, &[first.clone(), WIDE_MACHINE.into()], "the selected second preset must keep its place in the picker");
        let automatic = items.into_iter().find(|item| item.id == live_id!(automatic_machine)).unwrap();
        assert_eq!((automatic.label, automatic.icon), (format!("Automatic ({WIDE_MACHINE})"), fab::ui::popover::MenuIcon::Check),
            "the checked Automatic item must name the installed selection");
        let owner = EditOwner::new(PrinterScope::Target, &state);
        printing.action(&mut state, &PrintAction::SetMachine(owner.clone(), Some(first.clone())), None);
        assert_eq!(state.settings.machine, first, "an explicit pick must override the slicer's compatible selection");
        printing.action(&mut state, &PrintAction::SetMachine(owner, None), None);
        assert_eq!(state.settings.machine, WIDE_MACHINE, "clearing the explicit pick must restore the slicer's selection");
        std::fs::remove_file(conf).unwrap();
        printing.event(&mut state, &catalog());
        assert_eq!(state.settings.machine, first, "without a slicer selection automatic must return to the first compatible preset");
    }

    /// Continue the settled fleet case with two configured printers of A's model.
    fn check_machine_bindings(printing: &mut Printing, state: &mut PrintState, plate: &mut Evaluated, home: &Path, parent: &VirtualPrinter) {
        use fab_printer_link::PrinterLink;
        use std::{fs, io::Write};
        let imports = home.join("imports");
        let catalog = || Arc::new(catalog_in(&[], &imports, std::slice::from_ref(parent)).unwrap());
        let first_name = parent.machine_preset_name();
        let (wake, _wakes) = std::sync::mpsc::sync_channel(1);
        let pending = Arc::new(std::sync::Mutex::new(ProfileWork::default()));
        let path = home.join("virtual_printers.txt");
        let drain = || {
            if let Some(saved) = pending.lock().unwrap().saved.take() { virtual_printer::save_to(&path, &saved).unwrap(); }
        };
        printing.profiles_worker = Some(ProfilesWorker { pending: pending.clone(), wake });
        printing.virtuals = Some(Default::default());
        let [a, b] = ["A", "B"].map(|n| PrinterChoice::Real(n.into()));
        state.set_configured_printers(&["A".into(), "B".into()]);
        printing.rebuild_virtual_choices(state);
        let pick = |p: &mut Printing, s: &mut PrintState, id: &PrinterChoice| {
            let owner = EditOwner::new(PrinterScope::Target, s);
            assert!(p.action(s, &PrintAction::LinkPrinter(owner, id.clone()), None), "the target pick must apply");
        };
        printing.retire_link(state, &b, false);
        let report = home.join("same-model.jsonl");
        fs::write(&report, "@head 0 1 PLA 1969D4FF\n").unwrap();
        printing.install(state, &b, PrinterLink::simulate_moonraker(report.clone(), &parent.model()), false);
        wait_for("B's same-model report", || {
            printing.poll_printer(state);
            state.printer_of(&b).is_some_and(|s| s.state.trays().next().is_some())
        });
        let bound = |s: &PrintState, id: &PrinterChoice| s.printers.binding(id).unwrap().machine().unwrap().clone();
        let first = bound(state, &a);
        assert_eq!(first.geometry.name, first_name, "automatic must use the first imported preset in case-insensitive picker order");
        assert!(Arc::ptr_eq(&first, &bound(state, &b)), "B must expose the same automatic machine before it is targeted");
        let before = (state.colors.clone(), state.settings.clone(), crate::live_slice::revisions(), crate::live_slice::slice_key(plate), printing.gen);
        let old_preview = state.preview.clone().unwrap();
        let check_plate = |label: &str, p: &Printing, s: &PrintState, plate: &Evaluated| {
            assert_eq!((s.colors.clone(), s.settings.clone(), crate::live_slice::revisions(), crate::live_slice::slice_key(plate), p.gen),
                before, "{label} must preserve A's mapping, materials, live revisions, slice key and generation");
            assert!(s.settled && Arc::ptr_eq(&old_preview, s.preview.as_ref().unwrap()), "{label} must preserve A's settled preview");
        };
        let stale_a = EditOwner::new(PrinterScope::Target, state);
        state.shown = Some(b.clone());
        let edit_b = PrintAction::SetMachine(EditOwner::new(PrinterScope::Shown, state), Some(WIDE_MACHINE.into()));
        assert!(!printing.action(state, &edit_b, None), "a pick owned by B must not request A's remap or slice");
        check_plate("B's explicit pick", printing, state, plate);
        let wide = bound(state, &b);
        assert_eq!((&wide.geometry.name[..], wide.geometry.bed.max[0], &wide.diameters[..]), (WIDE_MACHINE, 380.0, &[0.6][..]), "B must publish its explicit bed and nozzle");
        assert!(Arc::ptr_eq(&first, &bound(state, &a)), "B's explicit pick must change B alone");

        let previous = state.printer_of(&b).unwrap().clone();
        writeln!(fs::OpenOptions::new().append(true).open(&report).unwrap(),
            r#"{{"print_stats":{{"state":"printing","filename":"b.gcode","info":{{"current_layer":3}}}}}}"#).unwrap();
        wait_for("a new report from B", || {
            poll_fleet(printing, state, plate);
            !Arc::ptr_eq(&previous, state.printer_of(&b).unwrap())
        });
        check_plate("B's background report", printing, state, plate);

        pick(printing, state, &b);
        assert_eq!(state.settings.machine, WIDE_MACHINE, "retargeting to B must apply B's explicit preset");
        printing.action(state, &PrintAction::SetMachine(stale_a, Some(WIDE_MACHINE.into())), None);
        assert!(Arc::ptr_eq(&first, &bound(state, &a)) && Arc::ptr_eq(&wide, &bound(state, &b)) && printing.virtuals.as_ref().unwrap().bindings.len() == 1,
            "a stale A menu must leave both bindings untouched after retargeting to B");
        pick(printing, state, &a);
        assert_eq!(state.settings.machine, first_name, "retargeting to A must restore A's automatic preset");

        drain();
        let reload = virtual_printer::load_from(&path).unwrap();
        assert_eq!(reload.bindings, [(b.id().unwrap(), WIDE_MACHINE.to_owned())].into(), "restart must restore B's pick and keep A automatic");
        printing.event(state, &PrintEvent::VirtualPrinters { saved: reload, catalog: printing.catalog.clone(), kept_as: None });
        assert_eq!(bound(state, &b).geometry.name, WIDE_MACHINE, "the loaded record must bind B again");

        let installed_plate = printing.catalog.as_ref().unwrap().selection.as_ref().unwrap().plate.clone();
        pick(printing, state, &PrinterChoice::Virtual(parent.name.clone()));
        printing.action(state, &PrintAction::SetPlate("Cool Plate".into()), None);
        assert_eq!(state.settings.materials[0].preset, virtual_printer::filament_preset("PLA"), "the generated printer must start with its built-in PLA");
        assert_ne!(state.settings.plate, installed_plate, "the generated printer must have its own picked plate");
        pick(printing, state, &b);
        assert_eq!((&state.settings.machine[..], &state.settings.materials, &state.settings.plate), (WIDE_MACHINE, &defaults_settings().materials, &installed_plate),
            "leaving a generated printer must reset materials and plate and apply B's preset");

        let owner = EditOwner::new(PrinterScope::Target, state);
        assert!(printing.action(state, &PrintAction::SetMachine(owner.clone(), None), None)
            && state.settings.machine == first_name, "a target Automatic pick must apply before the action returns");
        assert!(printing.action(state, &PrintAction::SetMachine(owner, Some(WIDE_MACHINE.into())), None)
            && state.settings.machine == WIDE_MACHINE && printing.machine().unwrap().bed.max[0] == 380.0,
            "an explicit target pick must apply its machine before the action returns");
        pick(printing, state, &a);

        let replace = |p: &mut Printing, s: &mut PrintState| {
            p.event(s, &PrintEvent::Catalog(Some(catalog())));
        };
        write_wide_machine(&imports, parent, 410, 0.8);
        replace(printing, state);
        assert_eq!((bound(state, &b).geometry.bed.max[0], bound(state, &b).diameters[0]), (410.0, 0.8), "catalogue replacement must resolve the same pick's new bed and nozzle");
        fs::remove_file(imports.join("machine/wide.json")).unwrap();
        replace(printing, state);
        assert!(matches!(state.printers.binding(&b).unwrap().as_ref(), MachineBinding::Available { machine, explicit: Some(n), .. } if machine.geometry.name == first_name && n == WIDE_MACHINE),
            "a missing explicit preset must keep its name and publish the automatic machine");
        write_wide_machine(&imports, parent, 410, 0.8);
        replace(printing, state);
        assert_eq!(bound(state, &b).geometry.name, WIDE_MACHINE, "the returning explicit preset must take over again");
        state.shown = Some(b.clone());
        let automatic = PrintAction::SetMachine(EditOwner::new(PrinterScope::Shown, state), None);
        printing.action(state, &automatic, None);
        drain();
        assert!(virtual_printer::load_from(&path).unwrap().bindings.is_empty() && bound(state, &b).geometry.name == first_name,
            "Automatic must remove the record and restore the first compatible preset");

        let twins = [&a, &b].map(|id| printing.make_twin(state, id, &first_name).unwrap());
        assert_ne!(twins[0], twins[1], "twins of same-preset printers must be distinct by source");
        for ((id, title), twin) in [(&a, "A"), (&b, "B")].into_iter().zip(twins) {
            assert!(state.links.choices.contains(&(twin.clone(), format!("{first_name} (virtual of {title})"))), "a printer twin must name its source in the picker");
            let record = Printing::virtual_record(printing.virtuals.as_ref().unwrap(), &twin).unwrap();
            assert!(matches!(&record.source, VirtualSource::Twin { source, .. } if *source == id.id()), "each twin must retain its source printer");
        }
        let catalogue_twin = printing.make_twin(state, &PrinterChoice::None, &first_name).unwrap();
        assert!(state.links.choices.contains(&(catalogue_twin, format!("{first_name} (virtual)"))), "a catalogue twin must keep the preset-only title");
    }

    #[test]
    fn preview_delivery_and_target_switches_protect_the_plate() {
        const CHILD: &str = "FAB_TEST_PREVIEW_TARGET_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let home = std::env::temp_dir().join(format!("fab-preview-target-{}", std::process::id()));
            std::fs::create_dir(&home).unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "print::tests::preview_delivery_and_target_switches_protect_the_plate", "--nocapture"])
                .env(CHILD, "1").env("MAKEPAD_TEST_HOME", &home).env_remove("FAB_PRINTER_VIRTUAL").status().unwrap();
            std::fs::remove_dir_all(home).unwrap();
            assert!(status.success(), "the isolated preview and target switch cases must pass");
            return;
        }
        let home = PathBuf::from(std::env::var_os("MAKEPAD_TEST_HOME").unwrap());
        let presets = virtual_printer::presets();
        let catalog = Arc::new(catalog_in(&[], &home, &presets).unwrap());
        let mut twin = catalog.twins[&presets[0].machine_preset_name()].printer.clone();
        twin.name = "watch".into();
        let a = PrinterChoice::Virtual(twin.name.clone());
        let b = PrinterChoice::Real("missing".into());
        let mut missing = twin.clone();
        missing.name = "offline".into();
        missing.configured_for = Some("missing".into());
        missing.source = VirtualSource::Twin { preset: "missing preset".into(), model: twin.model(), source: None };
        let saved = Saved { printers: vec![twin, missing], picked: a.id(), disconnected: ["missing".into()].into(), bindings: Default::default() };
        let mut printing = Printing { virtuals: Some(Arc::new(saved.clone())), catalog: Some(catalog.clone()), ..Default::default() };
        let mut state = PrintState { settings: defaults_settings(), ..Default::default() };
        assert!(printing.link_printer(&mut state, &a), "A must hold its standalone twin");
        let mut plate = fleet_plate(&printing);
        wait_for("A's virtual tray report", || poll_fleet(&mut printing, &mut state, &mut plate).target);
        let mut fleet = settle_fleet_plate(&mut printing, &mut state, &plate);
        let cx = &mut fleet.runtime.cx;
        let mut app = cx.with_vm(crate::App::script_new);
        app.printing = printing;
        app.state.print = state;
        app.plate = Some(Arc::new(plate));
        app.state.apply_core(&fab::ShellAction::SetWorkspace(fab::api::Workspace::Monitor));
        let preview = app.state.print.preview.clone().unwrap();
        let meta = Arc::new(JobMeta { id: "saved-job".into(), printer: a.id(), virtual_printer: true, ..Default::default() });
        app.state.print.follow.picked = Some(meta.id.clone());
        app.state.print.follow.jobs.push(CommittedJob { meta: meta.clone(), loading: true, state: JobState::Finished, ..Default::default() });
        assert!(!app.slice_wanted(), "waiting for a saved job preview must not ask for the plate's slice");
        let state = &app.state.print;
        let rows = state.colors.rows.clone();
        let materials = state.settings.materials.clone();
        let unprinted = state.unprinted.clone();
        let status = state.status.clone();
        let revisions = crate::live_slice::revisions();
        let key = crate::live_slice::slice_key(app.plate.as_ref().unwrap());
        let gen = app.printing.gen;
        assert!(!rows.is_empty() && !materials.is_empty(), "the settled plate must have a colour mapping to protect");
        // A remap would populate this even if it happened to choose exactly the same trays.
        app.printing.reach = None;
        app.handle_actions(cx, &[Box::new(PrintEvent::JobPreview { id: meta.id.clone(), result: Ok(preview.clone()) })]);
        assert!(app.printing.reach.is_none(), "a saved job preview must never remap the target plate");
        let state = &app.state.print;
        assert!(state.monitor_preview().is_some_and(|p| Arc::ptr_eq(p, &preview)), "Monitor must display the delivered preview");
        assert_eq!(state.colors.rows, rows, "preview delivery must preserve mapping rows and positions");
        assert_eq!(state.settings.materials, materials, "preview delivery must preserve plate materials");
        assert_eq!(state.unprinted, unprinted, "preview delivery must preserve unprinted colours");
        assert_eq!(crate::live_slice::revisions(), revisions, "preview delivery must preserve both live revisions");
        assert_eq!(crate::live_slice::slice_key(app.plate.as_ref().unwrap()), key, "preview delivery must preserve the plate and settings key");
        assert_eq!(app.printing.gen, gen, "preview delivery must preserve the slice generation");
        assert_eq!(state.status, status, "preview delivery must preserve the slice status");
        assert!(state.settled && app.printing.slice_job.is_none() && app.printing.live_job.is_none(), "the plate must stay settled with no slice or live job");

        for catalogue in [false, true] {
            let path = if catalogue { "catalogue removal" } else { "missing-preset pick" };
            let state = &mut app.state.print;
            if catalogue {
                let pick_action = PrintAction::LinkPrinter(EditOwner::new(PrinterScope::Target, state), a.clone());
                assert!(app.printing.action(state, &pick_action, None), "A must be picked again before catalogue removal");
            }
            app.printing.slice(state, app.plate.as_ref());
            let old = app.printing.slice_gen.expect("A's slice must be in flight");
            // Let the engine finish, keeping its result out of the editor until the switch.
            let answer = fleet.next(|ev| matches!(ev, PrintEvent::Sliced { gen, .. } if *gen == old));
            crate::live_slice::wait_idle();
            let revision = crate::live_slice::revisions().unwrap().0;
            if catalogue {
                assert_eq!(app.printing.event(state, &PrintEvent::Catalog(None)), PrintOutcome::Target, "catalogue removal must refresh the setup");
                assert_eq!(state.links.picked, PrinterChoice::None, "a vanished standalone twin must leave no printer picked");
                assert_eq!(state.settings.machine, defaults_settings().machine, "losing A's preset must restore the default machine");
            } else {
                let pick_action = PrintAction::LinkPrinter(EditOwner::new(PrinterScope::Target, state), b.clone());
                assert!(app.printing.action(state, &pick_action, None), "an applied missing-preset pick must request the picker refresh");
                assert_eq!(state.links.picked, b, "the unavailable configured printer must stay picked");
            }
            assert_eq!(app.printing.event(state, &answer), PrintOutcome::Unchanged, "{path} must reject the old target's settled slice");
            assert!(!app.printing.live_toolpaths(state, revision), "{path} must reject the old target's live result");
            assert!(!state.settled && app.printing.reask.is_some(), "{path} must leave the interrupted slice awaiting a fresh request");
            app.request_reslice();
            if catalogue {
                // The model host would now deliver the rebuilt plate for the default bed.
                let mut rebuilt = fleet_plate(&app.printing);
                remap(&mut app.printing, &mut app.state.print, &mut rebuilt);
                app.plate = Some(Arc::new(rebuilt));
                app.request_reslice();
            }
            assert!(app.printing.slice_gen.is_some_and(|fresh| fresh > old), "{path} must start the requested slice with a fresh generation");
            assert_eq!(app.printing.event(&mut app.state.print, &answer), PrintOutcome::Unchanged, "{path} must also reject A's answer after the fresh request");
        }
        let last = app.printing.slice_gen.unwrap();
        let answer = fleet.next(|ev| matches!(ev, PrintEvent::Sliced { gen, .. } if *gen == last));
        crate::live_slice::wait_idle();
        // The profiles worker can finish restoring the saved target after a slice was requested.
        let state = &mut app.state.print;
        let restore = PrintEvent::VirtualPrinters { saved, catalog: Some(catalog), kept_as: None };
        assert_eq!(app.printing.event(state, &restore), PrintOutcome::RestoredTarget, "a late restore of A must request its setup");
        assert_eq!(state.links.picked, a, "the late startup restore must pick A");
        assert_eq!(app.printing.event(state, &answer), PrintOutcome::Unchanged, "a late startup restore must discard the previous target's slice");
        assert!(app.printing.reask.is_some(), "a late startup restore must keep the interrupted slice request");

        // Every event runs sync_jobs: the shown job keeps its loaded preview in every workspace,
        // and only Monitor starts a preview load.
        let held = |app: &crate::App, id: &str| app.state.print.follow.job_of(id).and_then(|j| j.preview.clone());
        app.state.print.follow.picked = Some(meta.id.clone());
        app.printing.sync_jobs(&mut app.state.print);
        assert!(app.state.print.follow.monitor && held(&app, &meta.id).is_some_and(|p| Arc::ptr_eq(&p, &preview)), "Monitor must keep the shown job's loaded preview");
        app.state.apply_core(&fab::ShellAction::SetWorkspace(fab::api::Workspace::Design));
        app.printing.sync_jobs(&mut app.state.print);
        assert!(held(&app, &meta.id).is_some_and(|p| Arc::ptr_eq(&p, &preview)), "the shown job must keep its loaded preview after leaving Monitor");
        let unloaded = Arc::new(JobMeta { id: JobMeta::new_id("unloaded", jobs::wall_now()), printer: a.id(), virtual_printer: true, ..Default::default() });
        app.state.print.follow.jobs.push(CommittedJob { meta: unloaded.clone(), state: JobState::Finished, ..Default::default() });
        app.state.print.follow.picked = Some(unloaded.id.clone());
        app.printing.sync_jobs(&mut app.state.print);
        assert!(app.state.print.follow.job_of(&unloaded.id).is_some_and(|j| !j.loading && j.preview.is_none()), "outside Monitor no preview load may start");
        assert!(held(&app, &meta.id).is_none(), "a job neither shown nor printing must drop its preview");
        app.state.apply_core(&fab::ShellAction::SetWorkspace(fab::api::Workspace::Monitor));
        app.printing.sync_jobs(&mut app.state.print);
        assert!(app.state.print.follow.job_of(&unloaded.id).is_some_and(|j| j.loading), "Monitor must start the shown job's preview load");
        // This job has no package: the load ends unreadable, with nothing left loading.
        let answer = fleet.next(|ev| matches!(ev, PrintEvent::JobPreview { id, .. } if *id == unloaded.id));
        assert_eq!(app.printing.event(&mut app.state.print, &answer), PrintOutcome::JobPreview, "the load's answer must reach its job");
        assert!(app.state.print.follow.job_of(&unloaded.id).is_some_and(|j| !j.loading && j.unreadable.is_some()), "a missing package must end the load as unreadable");
    }

    #[test]
    fn showing_a_held_printer_moves_the_camera_and_follower_only() {
        use fab::ui::camera::FabPrinterCamera;
        use fab_printer_link::Command;
        const CHILD: &str = "FAB_TEST_SHOW_PRINTER_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let home = std::env::temp_dir().join(format!("fab-show-printer-{}", std::process::id()));
            std::fs::create_dir(&home).unwrap();
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "print::tests::showing_a_held_printer_moves_the_camera_and_follower_only", "--nocapture"])
                .env(CHILD, "1").env("MAKEPAD_TEST_HOME", &home).env_remove("FAB_PRINTER_VIRTUAL").status().unwrap();
            std::fs::remove_dir_all(home).unwrap();
            assert!(result.success(), "shown navigation must leave the target plate alone");
            return;
        }

        let presets = virtual_printer::presets();
        let home = PathBuf::from(std::env::var_os("MAKEPAD_TEST_HOME").unwrap());
        let catalog = Arc::new(catalog_in(&[], &home, &presets).unwrap());
        let twin = catalog.twins[&presets[0].machine_preset_name()].printer.clone();
        let mut second = twin.clone();
        second.name = "watch".into();
        let (a, b) = (PrinterChoice::Virtual(twin.name.clone()), PrinterChoice::Virtual(second.name.clone()));
        let saved = Saved { printers: vec![twin, second], ..Default::default() };
        let mut printing = Printing { virtuals: Some(Arc::new(saved)), catalog: Some(catalog), ..Default::default() };
        let mut state = PrintState { settings: defaults_settings(), ..Default::default() };
        assert!(printing.link_printer(&mut state, &a));
        printing.ensure_link(&mut state, &b).unwrap();
        let mut plate = fleet_plate(&printing);
        wait_for("both virtual printers' stock", || {
            poll_fleet(&mut printing, &mut state, &mut plate);
            [&a, &b].iter().all(|id| state.printer_of(id).is_some_and(|r| r.state.trays().next().is_some()))
        });
        let mut fleet = settle_fleet_plate(&mut printing, &mut state, &plate);
        // Both links hold the same model. B can run the one-layer package A just sliced.
        let slice = printing.last.as_ref().unwrap();
        let job = std::thread::scope(|s| s.spawn(|| {
            let bytes = slice.settled.package_with(&|_| {}, &|| false).unwrap();
            fab_printer_link::safety::validate_job(bytes, &slice.checked.as_ref().unwrap().profile, &slice.name).unwrap()
        }).join().unwrap());
        let ticket = state.ticket_for(&b).unwrap();
        let options = state.print_options;
        let meta = job_meta(slice, &b, state.printer_of(&b).map(Arc::as_ref), &options, Vec::new());
        printing.send_job(&mut state, &ticket, job, &meta, options).unwrap();
        wait_for("B's busy report before opening it", || {
            printing.poll_printer(&mut state);
            state.printer_of(&b).unwrap().state.busy()
        });

        let cx = &mut fleet.runtime.cx;
        let mut app = cx.with_vm(crate::App::script_new);
        app.printing = printing;
        app.state.print = state;
        app.plate = Some(Arc::new(plate));
        app.state.apply_core(&fab::ShellAction::SetWorkspace(fab::api::Workspace::Monitor));
        app.printing.reach = None;
        let p = &app.state.print;
        let (rows, materials, unprinted, status) = (p.colors.rows.clone(), p.settings.materials.clone(), p.unprinted.clone(), p.status.clone());
        let (revisions, key, gen) = (crate::live_slice::revisions(), crate::live_slice::slice_key(app.plate.as_ref().unwrap()), app.printing.gen);

        // Camera semantics holds a weak reference to its displayed view. It expires on a
        // source change, even when both printers are virtual and neither widget has redrawn.
        let camera = cx.with_vm(|vm| {
            fab::theme::script_mod(vm);
            fab::ui::script_mod_kit(vm);
            fab::viewport::print::script_mod(vm);
            fab::ui::camera::script_mod(vm);
            vm.capture_errors();
            let value = vm.eval(script! { use mod.prelude.widgets_internal.* use mod.widgets.* FabPrinterCamera{width: Fill height: Fill} });
            let camera = WidgetRef::script_from_value(vm, value);
            assert!(vm.take_errors().is_empty(), "the camera widget must instantiate");
            camera
        });
        let pass = DrawPass::new(cx);
        let mut list = DrawList2d::new(cx);
        let mut draw = |cx: &mut Cx, state: &mut fab::AppState| {
            let size = dvec2(120.0, 80.0);
            pass.set_size(cx, size);
            cx.redraw_all();
            let event = std::mem::take(&mut cx.new_draw_event);
            let mut draw = CxDraw::new(cx, &event);
            let mut frame = Cx2d::new(&mut draw);
            frame.begin_pass(&pass, Some(1.0));
            list.begin_always(&mut frame);
            frame.begin_root_turtle(size, Layout::default());
            camera.draw_all(&mut frame, &mut Scope::with_data(state));
            frame.end_pass_sized_turtle();
            list.end(&mut frame);
            frame.end_pass(&pass);
        };
        let cached = |cx: &Cx| {
            let mut node = makepad_widgets::makepad_platform::a11y_tree::Node::default();
            camera.borrow::<FabPrinterCamera>().unwrap().semantics(cx, &mut node);
            !node.value.is_empty()
        };
        let assert_source = |cx: &Cx, p: &mut PrintState, expected: &PrinterChoice| {
            p.camera.update_source(expected, false, p.printers.get(expected).map(Arc::as_ref));
            assert!(cached(cx), "reapplying the expected virtual source {expected:?} must preserve its cached view");
        };
        draw(cx, &mut app.state);
        assert_source(cx, &mut app.state.print, &a);
        let b_layer = app.state.print.printer_of(&b).unwrap().state.layer.unwrap_or(0);
        app.handle_actions(cx, &[Box::new(PrintAction::ShowPrinter(Some(b.clone())))]);
        assert_eq!(app.state.print.follow.reporting_printer.as_ref(), Some(&b), "navigation projects B's held report immediately");
        assert_eq!(app.state.print.follow.job.as_ref().unwrap().layer, b_layer, "B's already running job projects its reported layer");
        assert!(!cached(cx), "switching to B drops A's cached camera view before drawing");
        draw(cx, &mut app.state);
        assert_source(cx, &mut app.state.print, &b);
        app.printing.linked(&a).unwrap().link.send(Command::CheckStorage);
        app.printing.linked(&b).unwrap().link.send(Command::CheckStorage);
        wait_for("both printers to publish again while watching B", || {
            app.printing.poll_printer(&mut app.state.print);
            [&a, &b].iter().all(|id| app.state.print.printer_of(id).unwrap().storage_files.is_some())
        });
        assert!(cached(cx), "polling A and B while B is shown must preserve B's cached camera");
        assert_source(cx, &mut app.state.print, &b);
        app.handle_actions(cx, &[Box::new(PrintAction::ShowPrinter(None))]);
        assert_eq!(app.state.print.follow.reporting_printer.as_ref(), Some(&a), "returning projects A");
        assert!(!cached(cx), "returning to A drops B's cached camera before drawing");
        draw(cx, &mut app.state);
        assert_source(cx, &mut app.state.print, &a);
        let p = &app.state.print;
        assert_eq!(p.colors.rows, rows, "navigation preserves the target's mapping");
        assert_eq!(p.settings.materials, materials, "navigation preserves the target's materials");
        assert_eq!(p.unprinted, unprinted, "navigation preserves unprinted colours");
        assert_eq!(crate::live_slice::revisions(), revisions, "navigation preserves both live revisions");
        assert_eq!(crate::live_slice::slice_key(app.plate.as_ref().unwrap()), key, "navigation preserves the slice key");
        assert_eq!(app.printing.gen, gen, "navigation preserves the generation");
        assert_eq!(p.status, status, "navigation preserves the slice status");
        assert!(p.settled && app.printing.slice_job.is_none() && app.printing.live_job.is_none(), "navigation leaves the settled plate without a slice or live job");
        assert!(app.printing.reach.is_none(), "navigation never remaps");
    }

    #[test]
    fn split_printer_cards_and_tray_edits_reach_their_drawn_links() {
        use fab::print::job_card::{confirm_speed, job_view};
        use fab_printer_link::{Command, Speed};
        const CHILD: &str = "FAB_TEST_SPLIT_PRINTER_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let home = std::env::temp_dir().join(format!("fab-split-controls-{}", std::process::id()));
            std::fs::create_dir(&home).unwrap();
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "print::tests::split_printer_cards_and_tray_edits_reach_their_drawn_links", "--nocapture"])
                .env(CHILD, "1").env("MAKEPAD_TEST_HOME", &home).env_remove("FAB_PRINTER_VIRTUAL").status().unwrap();
            std::fs::remove_dir_all(home).unwrap();
            assert!(result.success(), "target and shown actions must reach their own virtual workers");
            return;
        }
        let home = PathBuf::from(std::env::var_os("MAKEPAD_TEST_HOME").unwrap());
        let presets = virtual_printer::presets();
        let catalog = Arc::new(catalog_in(&[], &home, &presets).unwrap());
        let target = presets.iter().find(|p| p.name == "two-materials").unwrap().clone();
        let mut twin = catalog.twins[&target.machine_preset_name()].printer.clone();
        twin.name = "watch".into();
        let (a, b) = (PrinterChoice::Virtual(target.name.clone()), PrinterChoice::Virtual(twin.name.clone()));
        let saved = Saved { printers: vec![target, twin], ..Default::default() };
        let mut printing = Printing { virtuals: Some(Arc::new(saved)), catalog: Some(catalog), ..Default::default() };
        let mut state = PrintState { settings: defaults_settings(), ..Default::default() };
        assert!(printing.link_printer(&mut state, &a), "A is the plate target");
        printing.ensure_link(&mut state, &b).unwrap();
        let mut plate = fleet_plate(&printing);
        wait_for("both virtual workers' tray reports", || {
            poll_fleet(&mut printing, &mut state, &mut plate);
            [&a, &b].iter().all(|id| state.printer_of(id).is_some_and(|r| r.state.trays().next().is_some()))
        });
        let _runtime = settle_fleet_plate(&mut printing, &mut state, &plate);
        apply(&mut state, &PrintAction::AskPrint(true));
        state.shown = Some(b.clone());
        let slot = state.colors.layout.trays[0].pos;
        let mut owner = EditOwner::default();
        colour_map::capture_tray_owner(&mut owner, &state);
        let b_stock = printing.virtual_for(&b).unwrap().slots.clone();
        let (dest, _) = confirm_speed(&state).expect("A's open Print card offers speed while B is shown");
        printing.action(&mut state, &PrintAction::SetVirtualSpeed(dest, Speed::Times(60.0)), None);
        let id = "queued-b".to_string();
        let meta = JobMeta { id: id.clone(), printer: b.id(), virtual_printer: true, ..Default::default() };
        state.follow.jobs.insert_newest(CommittedJob { meta: Arc::new(meta), state: JobState::Queued, ..Default::default() });
        state.follow.picked = Some(id);
        let dest = job_view(&state, 0.0).unwrap().control_destination(&state).expect("B's job card offers speed");
        printing.action(&mut state, &PrintAction::SetVirtualSpeed(dest, Speed::Times(10.0)), None);
        printing.action(&mut state, &PrintAction::EditTray(owner, slot, colours::TrayEdit::Material("PETG".into())), None);
        // Both storage replies fence the preceding speed and stock commands, including a misrouted one.
        for id in [&a, &b] { printing.linked(id).unwrap().link.send(Command::CheckStorage); }
        wait_for("both workers' storage replies after the presses", || {
            printing.poll_printer(&mut state);
            [&a, &b].iter().all(|id| state.printer_of(id).unwrap().storage_files.is_some())
        });
        assert_eq!(state.printer_of(&a).unwrap().virtual_speed, Speed::Times(60.0), "the Print-card speed reaches A");
        assert_eq!(state.printer_of(&b).unwrap().virtual_speed, Speed::Times(10.0), "the job-card speed reaches B");
        assert_eq!(printing.virtual_for(&a).unwrap().slots.iter().find(|s| s.head == slot.head && s.tray == slot.tray_text()).unwrap().material.as_deref(),
            Some("PETG"), "the colour-map tray edit changes A's saved stock");
        assert_eq!(state.printer_of(&a).unwrap().state.tray(slot.tray).unwrap().material.as_deref(), Some("PETG"), "A's worker reports the edited stock");
        assert_eq!(printing.virtual_for(&b).unwrap().slots, b_stock, "the colour-map edit preserves B's stock");

        let grid_owner = EditOwner::new(PrinterScope::Shown, &state);
        state.links.note = "target link note".into();
        printing.action(&mut state, &PrintAction::EditTray(grid_owner, slot, colours::TrayEdit::Empty), None);
        assert!(printing.virtual_for(&b).unwrap().slots.iter().find(|s| s.head == slot.head && s.tray == slot.tray_text()).unwrap().material.is_none(),
            "editing B's virtual grid changes B's saved stock");
        assert!(state.links.layout.is_some() && state.links.note == "target link note", "B's stock edit preserves A's generated layout controls and link note");
    }

    #[test]
    fn rendered_controls_only_reach_their_own_link() {
        use fab::print::job_card::{confirm_speed, job_view};
        use fab_printer_link::{Command, CommandState, PrinterLink, Speed};
        use std::{fs, io::Write};

        const CHILD: &str = "FAB_TEST_RENDERED_CONTROLS";
        if std::env::var_os(CHILD).is_none() {
            let home = std::env::temp_dir().join(format!("fab-controls-{}", std::process::id()));
            fs::create_dir(&home).unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "print::tests::rendered_controls_only_reach_their_own_link", "--nocapture"])
                .env(CHILD, "1").env("MAKEPAD_TEST_HOME", &home).status().unwrap();
            fs::remove_dir_all(home).unwrap();
            assert!(status.success(), "the isolated rendered-control cases must pass");
            return;
        }
        let home = PathBuf::from(std::env::var_os("MAKEPAD_TEST_HOME").unwrap());
        let [a, b] = ["A", "B"].map(|n| PrinterChoice::Real(n.into()));
        let mut printing = Printing { virtuals: Some(Arc::new(Saved::default())), ..Default::default() };
        let mut state = PrintState::default();
        let model = second_family_profiles::model();
        for (choice, name) in [(&a, "a"), (&b, "b")] {
            let file = home.join(name);
            writeln!(fs::File::create(&file).unwrap(), "{}", r#"{"print_stats":{"state":"printing","filename":"block.gcode"}}"#).unwrap();
            printing.install(&mut state, choice, PrinterLink::simulate_moonraker(file, &model), false);
        }
        let replacement = PrinterLink::simulate_moonraker(home.join("a"), &model);
        let running = |s: &PrintState, id: &PrinterChoice| s.printer_of(id).is_some_and(|p| p.state.gcode_state.as_deref() == Some("RUNNING"));
        let desktop = |s: &PrintState| {
            let card = job_view(s, 0.0).expect("the desktop must draw the running job");
            let dest = card.control_destination(s).expect("the desktop must draw filament controls");
            let paused = card.pause.expect("the desktop must offer Pause or Resume");
            dest.pause_or_resume(paused)
        };
        let wrist = |s: &PrintState| crate::xr::monitor_pause_press(s).expect("the wrist must draw a Pause or Resume");
        assert!(printing.link_printer(&mut state, &a), "A must be picked");
        wait_for("both simulators to report their running jobs", || {
            printing.poll_printer(&mut state);
            running(&state, &a) && running(&state, &b)
        });

        // Both presses survive in the UI while the link with the same name and mode is replaced.
        let stale = [desktop(&state), wrist(&state)];
        printing.install(&mut state, &a, replacement, false);
        wait_for("the replacement A to report its running job", || {
            printing.poll_printer(&mut state);
            running(&state, &a)
        });
        let counts = |s: &PrintState| [&a, &b].map(|id| s.printer_of(id).unwrap().job_sends);
        let sends = counts(&state);
        for action in stale { printing.action(&mut state, &action, None); }

        // CheckStorage is dequeued after the presses, but the HTTP requests can finish out of
        // order. Drain them, then append a layer marker after every possible Pause notification.
        let fence = |p: &mut Printing, s: &mut PrintState, ids: &[(&PrinterChoice, &str)], layer: u32| {
            for (id, _) in ids { p.linked(id).unwrap().link.send(Command::CheckStorage); }
            wait_for("each storage reply and all earlier control requests to finish", || {
                p.poll_printer(s);
                ids.iter().all(|(id, _)| s.printer_of(id).is_some_and(|report|
                    matches!(report.command, Some(("check_storage", CommandState::Done)))
                    && !report.state.details.iter().any(|(key, _)| key == "Waiting")))
            });
            for (_, file) in ids {
                let mut out = fs::OpenOptions::new().append(true).open(home.join(file)).unwrap();
                writeln!(out, "{{\"print_stats\":{{\"info\":{{\"current_layer\":{layer}}}}}}}").unwrap();
            }
            wait_for("the status stream after the storage replies", || {
                p.poll_printer(s);
                ids.iter().all(|(id, _)| s.printer_of(id).unwrap().state.layer == Some(layer))
            });
        };
        fence(&mut printing, &mut state, &[(&a, "a"), (&b, "b")], 1);
        assert!(running(&state, &a), "a desktop or XR Pause drawn for old A reached replacement A");
        assert!(running(&state, &b), "a stale Pause on A reached B");
        assert_eq!(counts(&state), sends, "stale controls must preserve both send counters");

        // Each surface pauses and resumes B. A has its own later status after every press.
        assert!(printing.link_printer(&mut state, &b), "B must be picked for its live controls");
        for (round, draw) in [desktop as fn(&PrintState) -> PrintAction, wrist].into_iter().enumerate() {
            for (i, (command, gcode)) in [("pause", "PAUSE"), ("resume", "RUNNING")].into_iter().enumerate() {
                // Make the next storage reply distinguishable from the preceding one.
                printing.linked(&a).unwrap().link.send(Command::ClearError);
                wait_for("A to acknowledge its new command before the next press", || {
                    printing.poll_printer(&mut state);
                    matches!(state.printer_of(&a).unwrap().command, Some(("clean_print_error", CommandState::Failed(_))))
                });
                let action = draw(&state);
                printing.action(&mut state, &action, None);
                wait_for("B's live Pause or Resume reply and state", || {
                    printing.poll_printer(&mut state);
                    let report = state.printer_of(&b).unwrap();
                    matches!(&report.command, Some((name, CommandState::Done)) if *name == command)
                        && report.state.gcode_state.as_deref() == Some(gcode)
                });
                fence(&mut printing, &mut state, &[(&a, "a")], 2 + (2 * round + i) as u32);
                assert!(running(&state, &a), "a live control drawn for B must leave A printing");
            }
        }

        // The same two configured IDs now hold editable virtual stock. A queued row and an
        // idle Print card both offer speed, so both cached presses can outlive the retarget.
        for name in ["A", "B"] {
            let saved = Arc::make_mut(printing.virtuals.as_mut().unwrap());
            let mut twin = virtual_printer::presets().into_iter().find(|v| v.name == "two-materials").unwrap();
            twin.name = format!("twin-{}", name.to_lowercase());
            twin.configured_for = Some(name.into());
            saved.printers.push(twin);
            saved.disconnected.insert(name.into());
        }
        printing.ensure_link(&mut state, &b).unwrap();
        assert!(printing.link_printer(&mut state, &a), "A must be selected in Virtual mode");
        wait_for("both virtual workers to report their editable stock", || {
            printing.poll_printer(&mut state);
            [&a, &b].iter().all(|id| state.printer_of(id).is_some_and(|p| p.virtual_printer && p.state.trays().next().is_some()))
        });
        let id = "queued-a".to_string();
        state.follow.jobs.push(CommittedJob { meta: Arc::new(JobMeta { id: id.clone(), printer: a.id(), virtual_printer: true, ..Default::default() }), state: JobState::Queued, ..Default::default() });
        state.follow.picked = Some(id);
        let card = job_view(&state, 0.0).expect("A's queued job must draw its card");
        assert!(card.virtual_speed.is_some(), "A's queued card must offer speed");
        let job_speed = PrintAction::SetVirtualSpeed(card.control_destination(&state).unwrap(), Speed::Times(10.0));
        apply(&mut state, &PrintAction::AskPrint(true));
        let (dest, _) = confirm_speed(&state).expect("A's Print card must offer speed");
        let print_speed = PrintAction::SetVirtualSpeed(dest, Speed::Times(60.0));
        let owner = EditOwner::new(PrinterScope::Target, &state);
        let stock = |p: &Printing, id: &PrinterChoice| p.virtual_for(id).unwrap().slots.clone();
        let printer = printing.virtual_for(&a).unwrap();
        let tray = &printer.slots[0];
        let heads = virtual_heads(printer);
        let pos = colours::Position::parse(&heads, &colours::head_word(&heads, tray.head), &tray.tray).unwrap();
        let edit = PrintAction::EditTray(owner, pos, colours::TrayEdit::Material("changed-material".into()));
        let stocks = [&a, &b].map(|id| stock(&printing, id));
        let trays = |s: &PrintState| [&a, &b].map(|id| s.printer_of(id).unwrap().state.trays()
            .map(|(side, t)| (side, t.clone())).collect::<Vec<_>>());
        let before_trays = trays(&state);
        let speeds = |s: &PrintState| [&a, &b].map(|id| s.printer_of(id).unwrap().virtual_speed);
        let before = speeds(&state);
        assert!(printing.link_printer(&mut state, &b), "B must be shown before A's old edits arrive");
        for action in [job_speed, print_speed, edit] { printing.action(&mut state, &action, None); }
        for id in [&a, &b] { printing.linked(id).unwrap().link.send(Command::CheckStorage); }
        wait_for("both virtual workers to answer after A's stale edits", || {
            printing.poll_printer(&mut state);
            [&a, &b].iter().all(|id| state.printer_of(id).unwrap().storage_files.is_some())
        });
        assert_eq!(speeds(&state), before, "stale job-card and Print-card speeds must leave both printers' speeds unchanged");
        assert_eq!([&a, &b].map(|id| stock(&printing, id)), stocks, "a stale tray edit must leave both printers' stock unchanged");
        assert_eq!(trays(&state), before_trays, "both virtual reports must keep their original trays");
        assert!(state.links.note.contains("printer changed"), "a stale tray edit must explain why it was refused");

        apply(&mut state, &PrintAction::AskPrint(true));
        let (current, _) = confirm_speed(&state).expect("B's current Print card must offer speed");
        printing.action(&mut state, &PrintAction::SetVirtualSpeed(current, Speed::Times(60.0)), None);
        wait_for("B's own virtual speed press to reach B", || {
            printing.poll_printer(&mut state);
            state.printer_of(&b).unwrap().virtual_speed == Speed::Times(60.0)
        });
        assert_eq!(speeds(&state), [before[0], Speed::Times(60.0)], "a live speed edit must change only the shown virtual printer");
    }

    #[test]
    fn admission_tickets_keep_completions_and_cancel_with_their_jobs() {
        use fab_printer_link::{Command, PrinterLink};
        use std::fs;

        const CHILD: &str = "FAB_TEST_ADMISSION_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let home = std::env::temp_dir().join(format!("fab-admission-{}", std::process::id()));
            fs::create_dir(&home).unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "print::tests::admission_tickets_keep_completions_and_cancel_with_their_jobs", "--nocapture"])
                .env(CHILD, "1").env("MAKEPAD_TEST_HOME", &home).status().unwrap();
            fs::remove_dir_all(home).unwrap();
            assert!(status.success(), "the isolated admission case must pass");
            return;
        }
        let home = PathBuf::from(std::env::var_os("MAKEPAD_TEST_HOME").unwrap());
        second_family_profiles::install(&home);
        let dirs = [makepad_fab_settings::home::vendor_data_dir_in(&home, makepad_fab_settings::profiles::SLICER_APPS[1])];
        let catalog = Arc::new(catalog_in(&dirs, &home.join("imports"), &[]).unwrap());
        let model = second_family_profiles::model();
        let [a, b] = ["A", "B"].map(|n| PrinterChoice::Real(n.into()));
        let mut state = PrintState { settings: defaults_settings(), ..Default::default() };
        let mut printing = Printing { virtuals: Some(Arc::new(Saved::default())), ..Default::default() };
        printing.event(&mut state, &PrintEvent::Catalog(Some(catalog)));
        for (id, file, spool) in [(&a, "a", "1969D4FF"), (&b, "b", "EF120AFF")] {
            let path = home.join(file);
            fs::write(&path, format!("@head 0 1 PLA {spool}\n")).unwrap();
            printing.install(&mut state, id, PrinterLink::simulate_moonraker(path, &model), false);
        }
        // Warm the replacement in parallel with the two installed links.
        let replacement = PrinterLink::simulate_moonraker(home.join("a"), &model);
        replacement.set_heads(&model, 4);
        printing.link_printer(&mut state, &a);
        apply(&mut state, &PrintAction::AskPrint(true));
        assert!(matches!(&state.confirm_print, Some(PrintConfirm::AwaitingReport(id)) if id == &a),
            "a card opened before A reports must wait for A");
        let mut plate = fleet_plate(&printing);
        let ready = |state: &PrintState, id: &PrinterChoice| state.printer_of(id).is_some_and(|s|
            s.state.storage == Some(true) && s.state.trays().any(|(_, t)| t.present == Some(true) && t.material.as_deref() == Some("PLA")));
        wait_for("both printers' spools and storage", || {
            poll_fleet(&mut printing, &mut state, &mut plate);
            ready(&state, &a) && ready(&state, &b)
        });
        state.bind_print_ticket();
        assert_eq!(state.confirm_print, state.ticket_for(&a).map(PrintConfirm::Filament), "the first report must bind A's open card");
        let mut runtime = settle_fleet_plate(&mut printing, &mut state, &plate);

        let open = |s: &mut PrintState| {
            apply(s, &PrintAction::AskPrint(true));
            apply(s, &PrintAction::AcceptErrors(true));
            apply(s, &PrintAction::SetPrintOption(PrintOption::PlateClear, true));
            let Some(PrintConfirm::Filament(ticket)) = &s.confirm_print else { panic!("the card must show its bound printer") };
            PrintAction::SendToPrinter { ticket: ticket.clone(), preview: s.preview.as_ref().unwrap().id }
        };
        let hold = |p: &mut Printing, s: &mut PrintState, r: &mut FleetRuntime| {
            let press = open(s);
            p.test_packages = Some(Vec::new());
            p.action(s, &press, None);
            apply(s, &press);
            let id = s.sending_for(&s.links.picked).unwrap_or_else(|| panic!("the confirmed job must be admitted: {:?}", s.print_blocked.get(&s.links.picked))).job.clone();
            let done = r.next(|ev| matches!(ev, PrintEvent::Packaged { .. }));
            p.event(s, &done);
            let (attempt, result) = p.test_packages.take().unwrap().pop().expect("a held package result");
            assert!(matches!(result, Packaged::Checked(_)), "the held package must pass validation: {result:?}");
            (id, PrintEvent::Packaged { id: attempt, result })
        };
        let retarget = |p: &mut Printing, s: &mut PrintState, plate: &mut Evaluated, id: &PrinterChoice| {
            let pick_action = PrintAction::LinkPrinter(EditOwner::new(PrinterScope::Target, s), id.clone());
            assert!(p.action(s, &pick_action, None), "retargeting must select the other held link");
            remap(p, s, plate);
            crate::live_slice::set_print_setup(p.check_setup(s));
        };

        let remember_plate = |p: &Printing, s: &PrintState, plate: &Evaluated| {
            let rows = s.colors.rows.clone();
            let materials = s.settings.materials.clone();
            let unprinted = s.unprinted.clone();
            let moves = s.head_moves.clone();
            let revisions = crate::live_slice::revisions();
            let key = crate::live_slice::slice_key(plate);
            let gen = p.gen;
            let status = s.status.clone();
            assert!(s.settled && p.slice_job.is_none() && p.live_job.is_none(), "B must start settled with no slice or live job");
            move |p: &Printing, s: &PrintState, plate: &Evaluated| {
                assert_eq!(s.colors.rows, rows, "A's job must preserve B's mapping rows and positions");
                assert_eq!(s.settings.materials, materials, "A's job must preserve B's materials");
                assert_eq!(s.unprinted, unprinted, "A's job must preserve B's unprinted colours");
                assert_eq!(s.head_moves, moves, "A's job must preserve B's head moves");
                assert_eq!(crate::live_slice::revisions(), revisions, "A's job must preserve B's live revisions");
                assert_eq!(crate::live_slice::slice_key(plate), key, "A's job must preserve B's plate and settings key");
                assert_eq!(p.gen, gen, "A's job must preserve B's slice generation");
                assert_eq!(s.status, status, "A's job must preserve B's slice status");
                assert!(s.settled && p.slice_job.is_none() && p.live_job.is_none(), "B must stay settled without a slice or live job");
            }
        };
        let starts = |file: &str| fs::read_to_string(fab_printer_link::sim::started_path(&home.join(file))).unwrap_or_default();
        let last_start_is = |file: &str, job: &JobMeta| {
            let filename = fab_printer_link::job::plain_file_name(&job.name);
            starts(file).lines().last().is_some_and(|line| line.contains(&format!("FILENAME=\"{filename}\"")))
        };
        let report = |file: &str, text: &str| {
            use std::io::Write;
            writeln!(fs::OpenOptions::new().append(true).open(home.join(file)).unwrap(), "{text}").unwrap();
        };
        let stop_flag = |p: &Printing, id: &str| p.packaging.iter().find_map(|pack|
            matches!(&pack.to, PackageFor::Printer(send) if send.meta.id == id).then(|| pack.stop.clone())).unwrap();

        // Release both held packages in reverse order. Give each plate a distinct uploaded name.
        let (a_job, a_done) = hold(&mut printing, &mut state, &mut runtime);
        retarget(&mut printing, &mut state, &mut plate, &b);
        plate.plate.parts[0].object = "second-plate".into();
        runtime.settle(&mut printing, &mut state, &plate);
        let (b_job, b_done) = hold(&mut printing, &mut state, &mut runtime);
        assert_eq!(state.sending.len(), 2, "each printer must admit its own package");
        let a_before = state.printer_of(&a).unwrap().job_sends;
        let b_before = state.printer_of(&b).unwrap().job_sends;
        // Target B completes while Monitor shows A, and B's stored report predates the hand-off.
        printing.action(&mut state, &PrintAction::ShowPrinter(Some(a.clone())), None);
        assert_eq!(state.follow.reporting_printer.as_ref(), Some(&a), "Monitor must show A before B's completion");
        let (rows, revisions, key, gen) = (state.colors.rows.clone(), crate::live_slice::revisions(), crate::live_slice::slice_key(&plate), printing.gen);
        assert_eq!(printing.event(&mut state, &b_done), PrintOutcome::Target, "B's completion must reach its target");
        assert_eq!(state.follow.reporting_printer.as_ref(), Some(&a), "B's completion must leave Monitor on A");
        assert_eq!(state.follow.job_of(&b_job).map(|j| &j.state), Some(&JobState::Sent), "rechecking B's older stored report must not end the job just handed to B");
        assert!(state.colors.rows == rows && crate::live_slice::revisions() == revisions && crate::live_slice::slice_key(&plate) == key && printing.gen == gen,
            "B's completion while A is shown must leave the plate's mapping, live revisions, key and generation alone");
        printing.action(&mut state, &PrintAction::ShowPrinter(None), None);
        let before_a = remember_plate(&printing, &state, &plate);
        assert_eq!(printing.event(&mut state, &a_done), PrintOutcome::JobRows, "A's background completion must redraw job rows only");
        before_a(&printing, &state, &plate);
        assert_eq!(state.print_blocked.get(&b), None, "B must have no refusal from A");
        for id in [&a_job, &b_job] {
            assert_eq!(state.follow.job_of(id).map(|j| &j.state), Some(&JobState::Sent), "both reverse-order packages must be handed to their confirmed printers");
        }
        assert_ne!(state.follow.job_of(&a_job).unwrap().meta.name, state.follow.job_of(&b_job).unwrap().meta.name, "the simulators must be able to distinguish the two package names");

        // Withhold the UI poll of A's acknowledgement. Even a fast worker cannot close this test window.
        retarget(&mut printing, &mut state, &mut plate, &a);
        runtime.settle(&mut printing, &mut state, &plate);
        assert_eq!(state.printer_of(&a).unwrap().job_sends, a_before, "A's stored report must still predate the hand-off");
        assert!(state.printers.handed(&a).is_some(), "A must hold its unacknowledged hand-off");
        let (quick, quick_done) = hold(&mut printing, &mut state, &mut runtime);
        printing.event(&mut state, &quick_done);
        assert_eq!(state.follow.job_of(&quick).map(|j| &j.state), Some(&JobState::Queued), "a quick second Start must queue behind A's hand-off");
        printing.linked(&a).unwrap().link.send(Command::CheckStorage);
        printing.linked(&b).unwrap().link.send(Command::CheckStorage);
        wait_for("both ordered storage replies after the reverse hand-offs", || {
            printing.poll_printer(&mut state);
            state.printer_of(&a).unwrap().storage_files.is_some() && state.printer_of(&b).unwrap().storage_files.is_some()
        });
        assert_eq!(state.printer_of(&a).unwrap().job_sends, a_before + 1, "A must receive exactly its own reverse-order package");
        assert_eq!(state.printer_of(&b).unwrap().job_sends, b_before + 1, "B must receive exactly its own job");
        wait_for("the simulator starts to reach both report files", || !starts("a").is_empty() && !starts("b").is_empty());
        assert!(last_start_is("a", &state.follow.job_of(&a_job).unwrap().meta), "A must start A's package");
        assert!(last_start_is("b", &state.follow.job_of(&b_job).unwrap().meta), "B must start B's package");
        assert!(state.printers.handed(&a).is_none() && state.printers.handed(&b).is_none(), "reports acknowledging both sends must release both reservations");
        for file in ["a", "b"] { report(file, r#"{"print_stats":{"state":"complete"}}"#); }
        wait_for("both completed printers to accept their next jobs", || {
            printing.poll_printer(&mut state);
            [&a, &b].iter().all(|id| state.printer_of(id).unwrap().state.gcode_state.as_deref() == Some("FINISH") && !state.printer_busy_for(id))
        });

        // Retire a link with a checked package held and a second confirmation still open.
        let (queued, stale_package) = hold(&mut printing, &mut state, &mut runtime);
        let stale = open(&mut state);
        printing.retire_link(&mut state, &a, false);
        printing.install(&mut state, &a, replacement, false);
        wait_for("A's replacement to report its own spools", || {
            printing.poll_printer(&mut state);
            ready(&state, &a)
        });
        let stale_sends = state.printer_of(&a).unwrap().job_sends;
        printing.event(&mut state, &stale_package);
        assert_eq!(state.print_blocked.get(&a), Some(STALE_DISPATCH), "a package confirmed for a retired epoch must be refused");
        assert!(jobs::read_package(&jobs::jobs_dir(), &queued).is_ok(), "a stale dispatch must retain its checked files");
        assert_eq!(state.follow.job_of(&queued).unwrap().state, JobState::Queued, "the replaced link must retain its queued job");
        state.bind_print_ticket();
        assert!(printing.send_checks(&state, &state.print_options).is_ok(), "A's replacement must otherwise accept the settled slice");
        let before = printing.last_attempt;
        printing.action(&mut state, &stale, None);
        assert_eq!(printing.last_attempt, before, "Start from a replaced link epoch must package nothing");
        assert_eq!(state.print_blocked.get(&a), Some(STALE_TICKET), "a replaced link must explain the stale confirmation");
        apply(&mut state, &PrintAction::AskPrint(false));
        let PrintAction::SendToPrinter { ticket, preview } = open(&mut state) else { unreachable!() };
        printing.action(&mut state, &PrintAction::SendToPrinter { ticket, preview: preview.wrapping_add(1) }, None);
        assert_eq!(printing.last_attempt, before, "Start for a different preview must package nothing");
        assert_eq!(state.print_blocked.get(&a), Some(STALE_PREVIEW), "a different preview must explain the stale slice");
        apply(&mut state, &PrintAction::AskPrint(false));

        let initial_a = state.printer_of(&a).unwrap().job_sends;
        assert_eq!(initial_a, stale_sends, "releasing the stale package must preserve the new link's count");
        fs::remove_file(fab_printer_link::sim::started_path(&home.join("a"))).unwrap();
        let dir = jobs::jobs_dir();
        let epoch = state.printers.epoch_of(&a);
        let (spool, spool_done) = hold(&mut printing, &mut state, &mut runtime);
        let sliced = printing.last.as_ref().unwrap();
        let used_tray = sliced.trays[sliced.used[0]].expect("the admitted job must use a loaded tray");
        let material_reported = |s: &PrintState, material: &str| s.printer_of(&a).unwrap().state.trays()
            .any(|(_, t)| t.id == Some(used_tray) && t.material.as_deref() == Some(material) && t.present == Some(true));
        report("a", "@head 0 1 PETG 1969D4FF");
        wait_for("A's used head to report PETG", || { printing.poll_printer(&mut state); material_reported(&state, "PETG") });
        let spool_sends = state.printer_of(&a).unwrap().job_sends;
        assert_eq!(state.printers.epoch_of(&a), epoch, "a spool edit must leave A's incarnation intact");
        printing.event(&mut state, &spool_done);
        let refusal = state.print_blocked.get(&a).unwrap_or_default();
        assert!(refusal.contains("PETG") && refusal.contains("PLA"), "the spool refusal must name the reported and required materials: {refusal}");
        assert!(state.follow.job_of(&spool).is_none(), "a tray mismatch must discard the refused package");
        runtime.next(|ev| matches!(ev, PrintEvent::JobFilesRemoved(id) if id == &spool));
        report("a", "@head 0 1 PLA 1969D4FF");
        wait_for("A's used head to report PLA again", || { printing.poll_printer(&mut state); material_reported(&state, "PLA") });

        // Cancel B while A holds both a package and a genuine refusal. Keep that same A for the opposite cancel.
        let (cancel_a, cancel_a_done) = hold(&mut printing, &mut state, &mut runtime);
        let stop_a = stop_flag(&printing, &cancel_a);
        printing.action(&mut state, &stale, None);
        assert_eq!(state.print_blocked.get(&a), Some(STALE_TICKET), "A must own a real refused press before B cancels");
        retarget(&mut printing, &mut state, &mut plate, &b);
        runtime.settle(&mut printing, &mut state, &plate);
        let (cancel_b, cancel_b_done) = hold(&mut printing, &mut state, &mut runtime);
        printing.action(&mut state, &PrintAction::CancelSend(cancel_b.clone()), None);
        assert!(!stop_a.load(Ordering::Relaxed), "cancelling B must leave A's sibling package running");
        assert!(state.follow.job_of(&cancel_b).is_none() && state.sending_for(&b).is_none(), "B's cancelled row and admission must go");
        assert_eq!(state.follow.job_of(&cancel_a).unwrap().state, JobState::Preparing, "A's sibling row must stay Preparing");
        assert_eq!(state.sending_for(&a).unwrap().job, cancel_a, "A's sibling must keep its admission");
        assert_eq!(state.print_blocked.get(&b), Some("Cancelled: nothing went to the printer."), "B must own its cancellation");
        assert_eq!(state.print_blocked.get(&a), Some(STALE_TICKET), "B's cancel must preserve A's refusal");
        printing.event(&mut state, &cancel_b_done);
        runtime.next(|ev| matches!(ev, PrintEvent::JobFilesRemoved(id) if id == &cancel_b));
        assert!(!dir.join(&cancel_b).exists() && jobs::read_package(&dir, &cancel_a).is_ok(), "only the cancelled package's files must go");

        let (survivor, survivor_done) = hold(&mut printing, &mut state, &mut runtime);
        let cancelled_sends = state.printer_of(&a).unwrap().job_sends;
        let b_before = state.printer_of(&b).unwrap().job_sends;
        printing.action(&mut state, &PrintAction::CancelSend(cancel_a.clone()), None);
        printing.event(&mut state, &cancel_a_done);
        runtime.next(|ev| matches!(ev, PrintEvent::JobFilesRemoved(id) if id == &cancel_a));
        printing.event(&mut state, &survivor_done);
        assert_eq!(state.follow.job_of(&survivor).unwrap().state, JobState::Sent, "B's held package must go out after cancelling A");
        wait_for("B to receive its surviving job", || { printing.poll_printer(&mut state); state.printer_of(&b).unwrap().job_sends > b_before });
        assert!(state.follow.job_of(&cancel_a).is_none() && !dir.join(&cancel_a).exists(), "the cancelled A row and files must go");
        // All negative dispatches on the replacement A precede the same ordered storage reply below.
        let start_queued = |p: &mut Printing, s: &mut PrintState| {
            apply(s, &PrintAction::PickJob(Some(queued.clone())));
            apply(s, &PrintAction::AskStartJob(Some(queued.clone())));
            let press = PrintAction::StartJob { id: queued.clone(), ticket: s.queued_ticket(&s.follow.shown().unwrap().meta).expect("the queued row must resolve its own printer") };
            p.action(s, &press, None);
            apply(s, &press);
            p.starting.last().cloned().expect("the queued job must have a live recheck")
        };
        let before_row_start = remember_plate(&printing, &state, &plate);
        let b_sends = state.printer_of(&b).unwrap().job_sends;
        let first = start_queued(&mut printing, &mut state);
        let old = runtime.next(|ev| matches!(ev, PrintEvent::JobChecked { attempt, .. } if *attempt == first.1));
        assert!(matches!(&old, PrintEvent::JobChecked { result: Ok(_), .. }), "attempt one must have a checked package");
        printing.action(&mut state, &PrintAction::CancelSend(queued.clone()), None);
        assert_eq!(state.follow.job_of(&queued).unwrap().state, JobState::Queued, "cancelling a recheck must keep its queued row");
        let second = start_queued(&mut printing, &mut state);
        assert_ne!(first.1, second.1, "restarting the same job must allocate a new attempt");
        let current = runtime.next(|ev| matches!(ev, PrintEvent::JobChecked { attempt, .. } if *attempt == second.1));
        let sends = state.printer_of(&a).unwrap().job_sends;
        assert_eq!(sends, initial_a, "no rejected or cancelled package may have reached A");
        assert!(state.printer_of(&a).unwrap().storage_files.is_none(), "A must still be waiting for its first storage listing");
        printing.event(&mut state, &old);
        // A same-link command behind any erroneous Print proves the worker processed the queue.
        printing.linked(&a).unwrap().link.send(Command::CheckStorage);
        wait_for("A's storage reply after the old recheck", || {
            printing.poll_printer(&mut state);
            state.printer_of(&a).unwrap().storage_files.is_some()
        });
        let observed = state.printer_of(&a).unwrap().job_sends;
        assert_eq!(observed, stale_sends, "replacing A must send nothing to the new incarnation");
        assert_eq!(observed, spool_sends, "a changed spool must receive no Print");
        assert_eq!(observed, cancelled_sends, "cancelling A must send nothing on A's link");
        assert_eq!(observed, sends, "a cancelled recheck must never hand a job to the link");
        assert_eq!(state.follow.job_of(&queued).unwrap().state, JobState::Preparing, "the old recheck must leave the new row Preparing");
        assert_eq!(printing.starting.last(), Some(&second), "the old recheck must leave attempt two live");
        assert_eq!(state.sending_for(&a).unwrap().job, queued, "the old recheck must preserve attempt two's admission");
        assert_eq!(state.print_blocked.get(&a), None, "the old recheck must leave attempt two's refusal alone");
        assert_eq!(printing.event(&mut state, &current), PrintOutcome::JobRows, "the queued A start from B must update only the rows");
        assert_eq!(state.follow.job_of(&queued).unwrap().state, JobState::Sent, "attempt two must hand off its checked job");
        before_row_start(&printing, &state, &plate);
        wait_for("A to receive attempt two", || {
            printing.poll_printer(&mut state);
            state.printer_of(&a).unwrap().job_sends == sends + 1 && !starts("a").is_empty()
        });
        assert!(last_start_is("a", &state.follow.job_of(&queued).unwrap().meta), "queued A must start its own file on A");
        assert_eq!(state.printer_of(&b).unwrap().job_sends, b_sends, "queued A must send nothing to B");
    }

    #[test]
    fn background_reports_leave_the_target_plate_and_slice_alone() {
        use fab_printer_link::{model::Colour, PrinterLink};
        use std::{fs, io::Write};

        // Both the publication counter and the live slicer belong to the process.
        const CHILD: &str = "FAB_TEST_FLEET_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let home = std::env::temp_dir().join(format!("fab-fleet-{}", std::process::id()));
            fs::create_dir(&home).unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "print::tests::background_reports_leave_the_target_plate_and_slice_alone", "--nocapture"])
                .env(CHILD, "1").env("MAKEPAD_TEST_HOME", &home).status().unwrap();
            fs::remove_dir_all(home).unwrap();
            assert!(status.success(), "isolated two-printer case failed");
            return;
        }
        let home = std::path::PathBuf::from(std::env::var_os("MAKEPAD_TEST_HOME").unwrap());
        let (a_file, b_file) = (home.join("a.jsonl"), home.join("b.jsonl"));
        fs::write(&a_file, "@head 0 1 PLA 1969D4FF\n").unwrap();
        fs::write(&b_file, "@head 0 1 PETG EF120AFF\n@head 3 1 TPU 16CF51FF\n").unwrap();
        let (a, b) = (PrinterChoice::Real("A".into()), PrinterChoice::Real("B".into()));
        let presets = virtual_printer::presets();
        let single = presets.iter().find(|p| p.name == "two-materials").unwrap();
        second_family_profiles::install(&home);
        // Two imported machines of A's model. The parent's preset still slices the reference plate.
        let imports = home.join("imports");
        write_wide_machine(&imports, single, 380, 0.6);
        let raw = virtual_printer::builtin_presets(std::slice::from_ref(single)).remove(0);
        fs::write(imports.join("machine/standard.json"), makepad_fab_settings::import::preset_json(&raw)).unwrap();
        let catalog = catalog_in(&ProfileStore::default_data_dirs(), &home.join("imports"), std::slice::from_ref(single)).unwrap();
        let mut printing = Printing { catalog: Some(Arc::new(catalog)), ..Default::default() };
        let mut state = PrintState { settings: defaults_settings(), ..Default::default() };
        state.links.picked = a.clone();
        state.settings.use_installed = true;
        state.settings.machine = single.machine_preset_name();
        state.settings.process = virtual_printer::PROCESS.into();
        printing.load_base_values(&mut state);
        printing.install(&mut state, &a, PrinterLink::simulate_moonraker(a_file, &single.model()), false);

        let mut plate = fleet_plate(&printing);
        let low = TrayId { ams: 0, slot: 0 };
        let high = TrayId { ams: 3, slot: 0 };
        wait_for("A must report its blue spool through its one watched head", || {
            poll_fleet(&mut printing, &mut state, &mut plate);
            state.target_printer().is_some_and(|s| s.state.extruders.len() == 1
                && s.state.tray(low).is_some_and(|t| t.colour == Colour::parse("1969D4FF")))
        });
        assert!(state.colors.rows.iter().any(|r| r.used && r.pos.is_some() && r.spool().is_some()), "A must have a mapped print colour");

        let mut fleet = settle_fleet_plate(&mut printing, &mut state, &plate);
        let status = state.status.clone();
        let generation = printing.gen;
        let revisions = crate::live_slice::revisions().unwrap();
        let input_key = crate::live_slice::slice_key(&plate);
        assert_eq!(printing.figures_of, Some(input_key), "the saved input key must identify A's settled plate and settings");
        let rows = state.colors.rows.clone();
        let materials = state.settings.materials.clone();
        let unprinted = state.unprinted.clone();
        let moves = state.head_moves.clone();
        let a_report = state.target_printer().unwrap().clone();
        assert!(printing.slice_job.is_none() && printing.live_job.is_none(), "A's settled plate must have no slice jobs on the board");

        // Keep B outside the fleet until it has an unread IDLE publication. A consumes the
        // global change before install, so only a fresh entry can deliver B on the next event.
        let b_model = second_family_profiles::model();
        let mut link = PrinterLink::simulate_moonraker(b_file.clone(), &b_model);
        // The pending report already has a different spool. Only the fleet can widen this watch to head four.
        link.set_heads(&b_model, 1);
        wait_for("B must report its one watched red spool before install", || {
            link.poll().is_some_and(|s| s.state.gcode_state.as_deref() == Some("IDLE") && s.state.extruders.len() == 1
                && s.state.tray(low).is_some_and(|t| t.colour == Colour::parse("EF120AFF")))
        });
        let published = fab_printer_link::published();
        let mut file = fs::OpenOptions::new().append(true).open(b_file).unwrap();
        writeln!(file, r#"{{"print_stats":{{"state":"standby","filename":"cube.gcode","info":{{"total_layer":3}}}}}}"#).unwrap();
        wait_for("B must publish its pending IDLE report before install", || fab_printer_link::published() != published);
        poll_fleet(&mut printing, &mut state, &mut plate);

        let sent = |id: &str, pick: &PrinterChoice| CommittedJob::restored(
            JobMeta { id: id.into(), name: "cube".into(), printer: pick.id(), layers: 3, ..Default::default() },
            SavedState { state: JobState::Sent, ..Default::default() }, 0.0, 0.0);
        state.follow.jobs.push(sent("a-job", &a));
        printing.install(&mut state, &b, link, false);
        let first = poll_fleet(&mut printing, &mut state, &mut plate);
        let check_materials = |state: &PrintState| {
            assert_eq!(state.colors.rows, rows, "B's report must leave A's mapped colour rows and positions unchanged");
            assert_eq!(state.settings.materials, materials, "B's report must leave A's material settings unchanged");
            assert_eq!(state.unprinted, unprinted, "B's report must leave A's unprinted colours unchanged");
            assert_eq!(state.head_moves, moves, "B's report must leave A's head moves unchanged");
        };
        check_materials(&state);
        assert!(first.others && !first.target, "freshly installed B must deliver its pending report without a new publication or a target remap");
        assert_eq!(state.printer_of(&b).unwrap().state.gcode_state.as_deref(), Some("IDLE"), "B's first fleet report must be IDLE");
        assert_eq!(state.follow.job_of("a-job").unwrap().state, JobState::Sent, "B's IDLE must leave A's job Sent");
        wait_for("B's profile must subscribe its link to the fourth head and its green spool", || {
            let flags = poll_fleet(&mut printing, &mut state, &mut plate);
            check_materials(&state);
            assert!(!flags.target, "B's subscribed-head report must only redraw");
            state.printer_of(&b).is_some_and(|s| s.model == b_model && s.state.extruders.len() == 4
                && s.state.tray(high).is_some_and(|t| t.colour == Colour::parse("16CF51FF")))
        });
        state.follow.jobs.push(sent("b-job", &b));
        writeln!(file, r#"{{"print_stats":{{"state":"complete","filename":"cube.gcode","info":{{"total_layer":3}}}}}}"#).unwrap();
        wait_for("B must report FINISH for the same named job", || {
            let flags = poll_fleet(&mut printing, &mut state, &mut plate);
            check_materials(&state);
            assert!(!flags.target, "B's terminal report must only redraw");
            state.printer_of(&b).unwrap().state.gcode_state.as_deref() == Some("FINISH")
        });
        assert_eq!(state.follow.job_of("a-job").unwrap().state, JobState::Sent, "B's same-named FINISH must leave A's job Sent");
        assert_eq!(state.follow.job_of("b-job").unwrap().state, JobState::Finished, "B's FINISH must reconcile B's own job");
        assert!(Arc::ptr_eq(state.target_printer().unwrap(), &a_report), "A must keep its own snapshot");
        assert_eq!(crate::live_slice::revisions(), Some(revisions), "B must leave A's live input and served revisions unchanged");
        assert_eq!(crate::live_slice::slice_key(&plate), input_key, "B must leave A's plate and settings input key unchanged");
        assert_eq!(printing.gen, generation, "B must not ask for a new slice generation");
        assert_eq!(state.status, status, "B must leave A's settled slice status unchanged");
        assert!(state.settled && !state.stale && printing.slice_job.is_none() && printing.live_job.is_none(), "B must neither request nor start a slice for A");
        check_installed_machine_selection(&mut fleet, &home, single);
        check_machine_bindings(&mut printing, &mut state, &mut plate, &home, single);
    }

    #[test]
    fn configured_fleet_starts_in_saved_modes_and_keeps_the_target_isolated() {
        use fab_printer_link::{ConnectionStatus, PrinterLink};
        use std::{cell::RefCell, fs, io::Write, rc::Rc};

        const CHILD: &str = "FAB_TEST_FLEET_STARTUP_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let home = std::env::temp_dir().join(format!("fab-fleet-startup-{}", std::process::id()));
            fs::create_dir(&home).unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "print::tests::configured_fleet_starts_in_saved_modes_and_keeps_the_target_isolated", "--nocapture"])
                .env(CHILD, "1").env("MAKEPAD_TEST_HOME", &home).env_remove("FAB_PRINTER_VIRTUAL").status().unwrap();
            fs::remove_dir_all(home).unwrap();
            assert!(status.success(), "isolated configured fleet case failed");
            return;
        }
        let home = PathBuf::from(std::env::var_os("MAKEPAD_TEST_HOME").unwrap());
        let presets = virtual_printer::presets();
        let single = presets.iter().find(|p| p.name == "two-materials").unwrap();
        let four = presets.iter().find(|p| p.name == "four-tools").unwrap();
        let imports = home.join("imports");
        let catalog = Arc::new(catalog_in(&[], &imports, &[single.clone(), four.clone()]).unwrap());
        let missing = Arc::new(catalog_in(&[], &imports, std::slice::from_ref(single)).unwrap());
        let [a, b, c] = ["A", "B", "C"].map(|name| PrinterChoice::Real(name.into()));
        let startup = |names: &[&str], catalog: &Arc<Catalog>| {
            fs::write(home.join("A"), "@head 0 1 PLA 1969D4FF\n").unwrap();
            fs::write(home.join("C"), "@head 0 1 PETG EF120AFF\n").unwrap();
            let mut twin = four.clone();
            twin.name = "b-twin".into();
            twin.configured_for = Some("B".into());
            twin.source = VirtualSource::Twin { preset: four.machine_preset_name(), model: four.model(), source: None };
            let saved = Saved { picked: a.id(), printers: vec![twin], disconnected: ["B".into()].into(), bindings: Default::default() };
            // An in-memory Moonraker session enters the protocol counters too. Establish
            // the fixture connections before measuring anything Fab does with their handles.
            let model = single.model();
            // Start every simulator before waiting on any: they connect side by side, so their
            // server-info rounds overlap instead of adding up.
            let mut prepared: HashMap<String, PrinterLink> = names.iter().filter(|n| matches!(**n, "A" | "C"))
                .map(|&name| (name.to_string(), PrinterLink::simulate_moonraker(home.join(name), &model))).collect();
            for link in prepared.values_mut() {
                wait_for("both the simulator status socket and its server-info request", || {
                    link.poll().is_some_and(|s| s.status == ConnectionStatus::Connected
                        && !s.state.details.iter().any(|(key, _)| key == "Waiting"))
                });
                link.set_heads(&model, 1); // Publish the subscribed state for the newly installed handle.
            }
            let counts = fab_printer_link::entry_counts();
            let calls = Rc::new(RefCell::new(Vec::<String>::new()));
            let (record, dir, prepared) = (calls.clone(), home.clone(), RefCell::new(prepared));
            let mut printing = Printing { real_constructor: Some(Box::new(move |name| {
                record.borrow_mut().push(name.into());
                prepared.borrow_mut().remove(name).unwrap_or_else(|| PrinterLink::simulate_moonraker(dir.join(name), &model))
            })), ..Default::default() };
            let mut state = PrintState { settings: defaults_settings(), ..Default::default() };
            state.settings.machine = single.machine_preset_name();
            state.settings.process = virtual_printer::PROCESS.into();
            state.settings.use_installed = true;
            printing.event(&mut state, &PrintEvent::PrinterConfigs {
                names: names.iter().map(|n| n.to_string()).collect(), presets: vec![], loadouts: LoadoutFiles::new(),
            });
            assert!(printing.fleet.is_empty() && calls.borrow().is_empty(), "no worker may start before modes are known");
            printing.event(&mut state, &PrintEvent::VirtualPrinters { saved, catalog: Some(catalog.clone()), kept_as: None });
            printing.event(&mut state, &PrintEvent::Catalog(Some(catalog.clone())));
            (printing, state, calls, counts)
        };
        let (_, baseline, _, baseline_counts) = startup(&["A"], &catalog);
        assert_eq!(fab_printer_link::entry_counts(), baseline_counts, "single-printer startup must reuse its injected worker");
        let (mut printing, mut state, calls, counts) = startup(&["A", "B", "C"], &catalog);
        assert_eq!(calls.borrow().as_slice(), ["A", "C"], "only Connected A and C may reach the real constructor");
        assert_eq!(state.links.picked, a, "startup must restore A as the target");
        assert_eq!(printing.fleet.len(), 3, "startup must install exactly the three configured printers");
        assert!(!printing.linked(&a).unwrap().virtual_mode && !printing.linked(&c).unwrap().virtual_mode
            && printing.linked(&b).unwrap().virtual_mode, "A and C must hold Connected workers and B a virtual worker");
        assert_ne!(baseline.settings.machine, four.machine_preset_name(), "the single-printer baseline must distinguish B's machine");
        assert_eq!(state.settings, baseline.settings, "the target machine and materials must match startup with A alone");
        let epoch = |p: &Printing, id: &PrinterChoice| p.linked(id).map(|e| e.epoch);
        let epochs = |p: &Printing| [epoch(p, &a), epoch(p, &b), epoch(p, &c)];
        let initial = epochs(&printing);
        let connected = |p: &PrintState, id: &PrinterChoice| p.printer_of(id).is_some_and(|s| s.status == ConnectionStatus::Connected);
        wait_for("the three workers to report", || {
            printing.poll_printer(&mut state);
            [&a, &b, &c].into_iter().all(|id| connected(&state, id))
        });
        let twin = state.printer_of(&b).unwrap();
        assert!(twin.virtual_printer && twin.name == "B" && twin.model == four.model(), "B must report its own configured twin");
        assert_eq!(fab_printer_link::entry_counts(), counts, "startup must avoid all real network and credential boundaries");

        state.print_options.plate_clear = true;
        state.confirm_print = Some(PrintConfirm::AwaitingReport(state.links.picked.clone()));
        state.confirm_stop = ControlDestination::filament(PrinterScope::Shown, &state);
        assert!(state.confirm_stop.is_some(), "A must have a stop confirmation before the retarget");
        state.follow.confirm_start = Some("a-job".into());
        let pick = |p: &mut Printing, s: &mut PrintState, id: &PrinterChoice| {
            let pick_action = PrintAction::LinkPrinter(EditOwner::new(PrinterScope::Target, s), id.clone());
            assert!(p.action(s, &pick_action, None), "the requested retarget must take effect");
        };
        pick(&mut printing, &mut state, &c);
        assert_eq!(epoch(&printing, &a), initial[0], "A stays in the fleet with its original epoch after picking C");
        assert!(state.confirm_print.is_none() && state.confirm_stop.is_none() && state.follow.confirm_start.is_none()
            && !state.print_options.plate_clear, "retargeting must clear A's confirmations and plate clearance");
        assert_eq!(state.follow.reporting_printer.as_ref(), Some(&c), "Monitor must immediately use C's stored report");
        pick(&mut printing, &mut state, &a);
        assert_eq!(epochs(&printing), initial, "picking A again must preserve every link epoch");
        pick(&mut printing, &mut state, &PrinterChoice::None);
        assert_eq!(epochs(&printing), initial, "No printer must leave the whole fleet linked");
        assert!(state.target_printer().is_none(), "No printer must leave the plate without a target");
        pick(&mut printing, &mut state, &a);
        assert_eq!(calls.borrow().as_slice(), ["A", "C"], "retargeting must never reopen a held link");

        // Consume all of A's subscription changes before testing C's failure.
        let mut plate = fleet_plate(&printing);
        let mut a_file = fs::OpenOptions::new().append(true).open(home.join("A")).unwrap();
        writeln!(a_file, "@head 0 1 PLA 3456ABFF").unwrap();
        wait_for("A's mapped spool after retargeting", || {
            poll_fleet(&mut printing, &mut state, &mut plate);
            state.target_printer().is_some_and(|s| s.state.tray(TrayId { ams: 0, slot: 0 })
                .is_some_and(|t| t.colour == fab_printer_link::model::Colour::parse("3456ABFF")))
        });
        assert!(state.colors.rows.iter().any(|r| r.used && r.spool().is_some()), "A's settled plate must use its reported spool");
        let _runtime = settle_fleet_plate(&mut printing, &mut state, &plate);
        let a_job = JobMeta { id: "a-job".into(), name: "cube".into(), printer: a.id(), ..Default::default() };
        state.follow.jobs.push(CommittedJob::restored(a_job, SavedState { state: JobState::Sent, ..Default::default() }, 0.0, 0.0));
        let snapshot = state.target_printer().unwrap().clone();
        let (mapping, materials) = (state.colors.clone(), state.settings.materials.clone());
        let (unprinted, moves) = (state.unprinted.clone(), state.head_moves.clone());
        let revisions = crate::live_slice::revisions().unwrap();
        let key = crate::live_slice::slice_key(&plate);
        let (generation, status) = (printing.gen, state.status.clone());
        assert_eq!(printing.figures_of, Some(key), "A's settled figures must identify the tested input");
        let mut c_file = fs::OpenOptions::new().append(true).open(home.join("C")).unwrap();
        writeln!(c_file, r#"{{"webhooks":{{"state":"shutdown","state_message":"fixture shutdown"}}}}"#).unwrap();
        wait_for("C's reported firmware shutdown", || {
            let flags = poll_fleet(&mut printing, &mut state, &mut plate);
            assert!(Arc::ptr_eq(state.target_printer().unwrap(), &snapshot), "C's failure must leave A's target snapshot unchanged");
            assert!(!flags.target, "C's failure must not request a target remap");
            flags.others && state.printer_of(&c).is_some_and(|s| matches!(s.status, ConnectionStatus::Offline(_))
                && s.state.errors.iter().any(|(_, text)| text == "fixture shutdown"))
        });
        assert_eq!(state.colors, mapping, "C's failure must preserve A's mapped colours and positions");
        assert_eq!(state.settings.materials, materials, "C's failure must preserve A's materials");
        assert_eq!(state.unprinted, unprinted, "C's failure must preserve A's unprinted colours");
        assert_eq!(state.head_moves, moves, "C's failure must preserve A's head moves");
        assert_eq!(crate::live_slice::revisions(), Some(revisions), "C's failure must preserve the live slice revisions");
        assert_eq!(crate::live_slice::slice_key(&plate), key, "C's failure must preserve the plate and settings input key");
        assert_eq!(printing.gen, generation, "C's failure must preserve the slice generation");
        assert_eq!(state.status, status, "C's failure must preserve A's slice status");
        assert!(state.settled && !state.stale && printing.slice_job.is_none() && printing.live_job.is_none(),
            "C's failure must leave A settled with neither slice job on the board");
        assert_eq!(state.follow.job_of("a-job").unwrap().state, JobState::Sent, "C's failure must leave A's job Sent");

        let b_job = JobMeta { id: "b-job".into(), printer: b.id(), virtual_printer: true, ..Default::default() };
        state.follow.jobs.push(CommittedJob { meta: Arc::new(b_job), state: JobState::Sent, ..Default::default() });
        printing.event(&mut state, &PrintEvent::Catalog(Some(missing.clone())));
        assert_eq!(epochs(&printing), [initial[0], None, initial[2]], "losing B's preset must retire only B");
        assert_eq!(state.links.picked, a, "losing a background preset must preserve the target");
        let stopped = state.follow.job_of("b-job").unwrap();
        assert!(stopped.state == JobState::Stopped && stopped.stopped_reason.as_deref() == Some("The virtual print stopped when its printer was unlinked."),
            "retiring background B must stop B's own virtual job with the unlink reason");
        printing.event(&mut state, &PrintEvent::Catalog(Some(catalog.clone())));
        assert!(epoch(&printing, &b) > initial[1] && printing.linked(&b).unwrap().virtual_mode, "the returning preset must restore B's twin in a new epoch");
        assert_eq!([epoch(&printing, &a), epoch(&printing, &c)], [initial[0], initial[2]], "restoring B must preserve A's and C's workers");
        assert_eq!(calls.borrow().as_slice(), ["A", "C"], "losing and restoring a preset must never reach the real constructor");

        assert_eq!(fab_printer_link::entry_counts(), counts, "retargeting, failure and preset recovery must leave the boundary counters unchanged");

        drop(printing);
        let (mut delayed, mut delayed_state, requests, delayed_counts) = startup(&["A", "B", "C"], &missing);
        let before = epochs(&delayed);
        assert!(before[0].is_some() && before[1].is_none() && before[2].is_some(), "startup without B's preset must leave only B without a worker");
        delayed.event(&mut delayed_state, &PrintEvent::Catalog(Some(catalog)));
        assert!(delayed.linked(&b).is_some_and(|e| e.virtual_mode), "catalogue arrival must install the missing configured twin");
        assert_eq!([epoch(&delayed, &a), epoch(&delayed, &c)], [before[0], before[2]], "late catalogue arrival must preserve both Connected epochs");
        assert_eq!(requests.borrow().as_slice(), ["A", "C"], "waiting for a preset must never fall back to the real constructor");
        assert_eq!(fab_printer_link::entry_counts(), delayed_counts, "starting with a missing preset and restoring it must leave the boundary counters unchanged");
    }

    #[test]
    fn unavailable_configured_twin_never_starts_a_real_link() {
        const CHILD: &str = "FAB_TEST_DISCONNECTED_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "print::tests::unavailable_configured_twin_never_starts_a_real_link", "--nocapture"])
                .env(CHILD, "1").status().unwrap();
            assert!(status.success(), "isolated constructor gate test failed");
            return;
        }
        let config = "fixture";
        let pick = PrinterChoice::Real(config.into());
        let mut printing = Printing::default();
        let mut p = PrintState::default();
        let before = fab_printer_link::entry_counts();
        assert!(!printing.link_printer(&mut p, &pick), "mode not read yet");
        let mut saved = Saved::default();
        saved.disconnected.insert(config.into());
        saved.picked = pick.id();
        printing.event(&mut p, &PrintEvent::VirtualPrinters { saved: saved.clone(), catalog: None, kept_as: None });
        assert_eq!(p.links.picked, pick);
        assert!(printing.fleet.is_empty() && p.links.note.contains("no saved layout"));
        let mut twin = saved.printers[0].clone();
        twin.configured_for = Some(config.into());
        twin.source = VirtualSource::Twin { preset: "unavailable".into(), model: twin.model(), source: None };
        saved.printers = vec![twin];
        printing.event(&mut p, &PrintEvent::VirtualPrinters { saved, catalog: None, kept_as: None });
        assert!(printing.fleet.is_empty() && p.links.note.contains("preset is unavailable"));
        assert_eq!(fab_printer_link::entry_counts(), before);
    }

    #[test]
    fn unreadable_printer_modes_recover_without_a_real_link() {
        // Isolate the process-wide entry counters from other printer tests.
        const CHILD: &str = "FAB_TEST_PRINTER_RECOVERY_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "print::tests::unreadable_printer_modes_recover_without_a_real_link", "--nocapture"])
                .env(CHILD, "1").env("FAB_PRINTER_VIRTUAL", "four-tools").status().unwrap();
            assert!(status.success(), "isolated printer settings recovery failed");
            return;
        }
        let dir = std::env::temp_dir().join(format!("fab-print-recovery-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("virtual_printers.txt");
        let invalid = b"unreadable\r\n";
        std::fs::write(&path, invalid).unwrap();
        let first = path.with_added_extension("unreadable");
        std::fs::write(&first, b"previous").unwrap();
        let names = ["fixture-1".to_owned(), "fixture-2".to_owned()];
        let before = fab_printer_link::entry_counts();
        let mut printing = Printing::default();
        let mut state = PrintState { settings: defaults_settings(), ..Default::default() };
        state.set_configured_printers(&names);
        read_printer_profiles(Some(&path), &names, |ps| catalog_in(&[], &dir, ps).map(Arc::new), |event| {
            printing.event(&mut state, &event);
        });
        assert_eq!(std::fs::read(path.with_added_extension("unreadable.2")).ok().as_deref(), Some(invalid.as_slice()),
            "keep the unreadable settings byte for byte before starting fresh");
        let saved = printing.virtuals.as_deref().expect("post VirtualPrinters after keeping the file");
        assert_eq!(saved.disconnected, names.iter().cloned().collect(), "start every configured printer in Virtual mode");
        assert_eq!(saved.picked.as_deref(), Some("printer:No printer"), "publish an explicit no-printer pick");
        let persisted = virtual_printer::load_from(&path).expect("fresh settings are readable on restart");
        assert_eq!(persisted.disconnected, saved.disconnected, "keep all printers disconnected across restarts");
        assert_eq!(persisted.picked, saved.picked, "keep the no-printer pick across restarts");
        assert_eq!(state.links.picked, PrinterChoice::None, "startup restores no printer");
        assert!(printing.fleet.is_empty(), "handling the published modes must leave the link empty");
        assert_eq!(state.links.note, "Fab could not read its saved printer settings, so it kept them as virtual_printers.txt.unreadable.2 and started fresh. No printer is linked, and your printers start in Virtual mode until you switch them to Connected.",
            "the startup note identifies the actual backup by filename");
        let pick = PrinterChoice::Real(names[1].clone());
        let pick_action = PrintAction::LinkPrinter(EditOwner::new(PrinterScope::Target, &state), pick.clone());
        assert!(printing.action(&mut state, &pick_action, None), "accept an explicit configured pick");
        assert_eq!(state.links.picked, pick, "link the chosen configuration");
        let snapshot = printing.fleet.iter_mut().find(|entry| entry.id == pick).and_then(|entry| entry.link.poll()).expect("the pick creates a printer link");
        assert!(snapshot.virtual_printer, "the chosen link reports a virtual printer");
        assert_eq!(fab_printer_link::entry_counts(), before, "startup and the explicit pick avoid every real printer boundary");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unreadable_printer_modes_stay_unknown_when_set_aside_fails() {
        let dir = std::env::temp_dir().join(format!("fab-print-set-aside-failure-{}", std::process::id()));
        let path = dir.join("virtual_printers.txt");
        // A nonempty directory cannot be hard-linked or replaced with a settings file.
        std::fs::create_dir_all(&path).unwrap();
        let original = path.join("original");
        std::fs::write(&original, b"untouched").unwrap();
        let kind = std::fs::hard_link(&path, path.with_added_extension("unreadable")).unwrap_err().kind();
        let mut printing = Printing::default();
        let mut state = PrintState::default();
        let names = ["fixture".to_owned()];
        state.set_configured_printers(&names);
        read_printer_profiles(Some(&path), &names, |_| None, |event| { printing.event(&mut state, &event); });
        assert!(printing.virtuals.is_none(), "a failed set-aside must not publish fresh modes");
        assert!(printing.fleet.is_empty(), "a failed set-aside must leave startup unlinked");
        assert_eq!(state.links.note, format!("Fab could not read the saved printer modes. No printer was linked. The file could not be kept aside ({kind})."),
            "explain the set-aside failure with only its I/O error kind");
        let pick_action = PrintAction::LinkPrinter(EditOwner::new(PrinterScope::Target, &state), PrinterChoice::Real(names[0].clone()));
        assert!(!printing.action(&mut state, &pick_action, None),
            "a manual pick is still blocked while the modes are unknown");
        assert_eq!(std::fs::read(original).unwrap(), b"untouched", "preserve the original contents");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "leave no backup or fresh settings beside the original");
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 1, "leave the original directory unchanged");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn configured_switch_uses_the_report_and_keeps_its_own_stock() {
        let presets = virtual_printer::presets();
        let cat = catalog_in(&[], &std::env::temp_dir().join("fab-no-imports"), &presets).unwrap();
        let saved = Saved::default();
        let config = SIMULATED_PRINTER;
        let pick = PrinterChoice::Real(config.into());
        let mut p = PrintState { settings: defaults_settings(), ..Default::default() };
        p.links.picked = pick.clone();
        let mut printing = Printing { catalog: Some(Arc::new(cat)), virtuals: Some(Arc::new(saved)),
            simulator: Some(std::env::temp_dir().join("fab-no-reports")), ..Default::default() };
        p.settings.machine.clear();
        assert!(printing.switch_mode(&mut p, config, true).unwrap_err().contains("filament machine preset"));
        let preset = presets[0].machine_preset_name();
        p.settings.machine = preset.clone();
        let mut loaded = presets[0].clone();
        loaded.slots[0].material = Some("PETG".into());
        loaded.slots[0].colour = Some(0x136D8A);
        let report = fab_printer_link::virtual_link::report(&setup_of(&loaded, None).unwrap());
        p.printers.set(&p.links.picked, 1, Arc::new(fab_printer_link::Snapshot { state: Arc::new(report), ..Default::default() }));
        p.sending.push(Sending { job: "block".into(), printer: p.links.picked.clone() });
        assert!(printing.switch_mode(&mut p, config, true).unwrap_err().contains("being prepared"));
        p.sending.clear();
        assert_eq!(printing.switch_mode(&mut p, config, true), Ok(true));
        let twin = printing.linked_virtual(&p).unwrap().clone();
        assert_eq!((&twin.slots[0].material, twin.slots[0].colour), (&loaded.slots[0].material, loaded.slots[0].colour));
        assert_eq!(setup_of(&twin, printing.catalog.as_deref()).unwrap().name, config);
        let source = p.links.picked.clone();
        let standalone = printing.make_twin(&mut p, &source, &preset).unwrap();
        assert_ne!(standalone, PrinterChoice::Virtual(twin.name.clone()));
        assert!(p.links.choices.contains(&(standalone, format!("{preset} (virtual of {config} (Virtual))"))),
            "a twin of a configured Virtual printer must use the source mode shown in the picker");
        assert!(!p.links.choices.iter().any(|(c, _)| c == &PrinterChoice::Virtual(twin.name.clone())));
        assert!(!printing.link_printer(&mut p, &PrinterChoice::Virtual(twin.name.clone())));
        assert_eq!(printing.switch_mode(&mut p, config, false), Ok(true));
        assert!(!printing.linked(&p.links.picked).is_some_and(|entry| entry.virtual_mode));
        assert_eq!(printing.virtuals.as_ref().unwrap().twin_for(config), Some(&twin));
        assert_eq!(printing.switch_mode(&mut p, config, true), Ok(true));
        assert_eq!(printing.linked_virtual(&p), Some(&twin));
        assert_eq!(p.links.picked, pick);
    }

    /// Two mapped spools produce the right physical heads, job colours and checked output file.
    #[test]
    fn virtual_two_colour_plate_slices_on_tools_and_on_trays() {
        let printers = virtual_printer::presets();
        let imports = std::env::temp_dir().join(format!("fab-virtual-slice-{}", std::process::id()));
        std::fs::create_dir(&imports).unwrap();
        let cat = Arc::new(catalog_in(&[], &imports, &printers).unwrap());
        let missing = PrintEvent::Catalog(catalog_in(&[], &imports, &printers[1..]).map(Arc::new));
        std::fs::remove_dir(&imports).unwrap();
        assert!(!cat.twins.contains_key(fab_sla::printer::BUILTIN_PRINTER.0), "no resin printer in the twin picker");
        assert!(printers.iter().all(|p| cat.twins.contains_key(&p.machine_preset_name())), "built-in filament machines can be twinned");
        let mut forged = printers[0].clone();
        forged.source = VirtualSource::Twin { preset: fab_sla::printer::BUILTIN_PRINTER.0.into(), model: forged.model(), source: None };
        let mut filament = cat.twins[&printers[0].machine_preset_name()].printer.clone();
        filament.name = "stock-fixture".into();
        filament.slots[0].material = Some("PETG".into());
        filament.slots[0].colour = Some(0x136D8A);
        let choice = |p: &VirtualPrinter| PrinterChoice::Virtual(p.name.clone());
        let saved = Saved { picked: choice(&forged).id(), printers: vec![filament.clone(), forged.clone(), printers[1].clone()], disconnected: Default::default(), bindings: Default::default() };
        let mut printing = Printing { virtuals: Some(Arc::new(saved.clone())), ..Default::default() };
        let mut state = PrintState { settings: defaults_settings(), ..Default::default() };
        let real = PrinterChoice::Real("Configured fixture".into());
        state.set_configured_printers(&["Configured fixture".into()]);
        let choices = |s: &PrintState| s.links.choices.iter().map(|(c, _)| c.clone()).collect::<Vec<_>>();
        printing.event(&mut state, &missing);
        let kept = printing.virtuals.as_deref().unwrap();
        assert_eq!(kept.printers, saved.printers, "unavailable records and their stock survive catalogue refresh");
        assert_eq!(Saved::parse(&kept.to_text()).unwrap(), *kept, "the next save keeps both hidden twins");
        assert_eq!(kept.picked.as_deref(), Some("printer:No printer"), "a rejected record stays disconnected");
        assert_eq!(choices(&state), [real.clone(), choice(&printers[1]), PrinterChoice::None]);
        for p in [&forged, &filament] {
            assert!(!printing.link_printer(&mut state, &choice(p)), "an unavailable twin cannot link");
        }
        assert!(printing.fleet.is_empty());
        printing.event(&mut state, &PrintEvent::Catalog(Some(cat.clone())));
        assert_eq!(choices(&state), [real, choice(&filament), choice(&printers[1]), PrinterChoice::None]);
        assert_eq!(printing.virtuals.as_ref().unwrap().printers[0], filament, "the returning profile keeps its edited stock");
        assert_eq!(state.links.picked, PrinterChoice::None, "the returning twin waits for an explicit pick");
        let pick_action = PrintAction::LinkPrinter(EditOwner::new(PrinterScope::Target, &state), choice(&filament));
        printing.action(&mut state, &pick_action, None);
        assert!(!printing.fleet.is_empty());
        assert_eq!(state.settings.machine, filament.machine_preset_name());
        printing.event(&mut state, &missing);
        assert!(printing.fleet.is_empty(), "a linked twin disconnects if its preset disappears");
        assert_ne!(state.settings.machine, filament.machine_preset_name(), "a vanished twin's machine is not left selected");
        assert_eq!(state.links.picked, PrinterChoice::None);
        assert_eq!(printing.virtuals.as_ref().unwrap().picked.as_deref(), Some("printer:No printer"));
        let plate = Plate { parts: vec![
            part("first", 1, None, cube([100.0, 100.0, 0.0], 4.0)),
            part("second", 2, None, cube([112.0, 100.0, 0.0], 4.0)),
        ], ..Default::default() };
        printing.virtuals = Some(Arc::new(Saved { printers: printers.clone(), picked: None, disconnected: Default::default(), bindings: Default::default() }));
        for name in ["four-tools", "nine-materials"] {
            let vp = printers.iter().find(|p| p.name == name).unwrap();
            assert!(printing.link_printer(&mut state, &PrinterChoice::Virtual(name.into())));
            printing.event(&mut state, &PrintEvent::Catalog(Some(cat.clone())));
            let machine = printing.machine().unwrap();
            let offline = colours::Offline::of(machine);
            let setup = setup_of(vp, None).unwrap();
            let report = fab_printer_link::virtual_link::report(&setup);
            let layout = colours::layout(Some((&vp.title, &report)), None, &offline, &|_| None);
            let material = |t: &colours::TrayView| JobMaterial {
                preset: printing.preset_for(&state, Some(t), 0, None), color: t.colour.unwrap(),
                nozzle: t.pos.head, tray: Some(t.pos.tray), dedicated: t.dedicated(),
            };
            // Every head may print the generated filaments: a material on each loaded tray resolves
            // to a job whose filaments are printable on their own nozzles (the option's default
            // allows the first two only).
            let mut every = state.settings.clone();
            every.materials = layout.trays.iter().filter(|t| t.colour.is_some()).map(material).collect();
            let job = makepad_fab_settings::machine::Job::new(resolve_setup(&every, Some(&cat), &plate, machine).unwrap().build_config());
            let filaments = fab_printability::Materials::from_job(&job).filaments;
            let nozzles: std::collections::BTreeSet<_> = filaments.iter().filter_map(|f| f.nozzle).collect();
            assert_eq!(nozzles.len(), vp.heads, "{name}: a material on every head");
            for (i, f) in filaments.iter().enumerate() {
                let n = f.nozzle.unwrap();
                assert!(f.printable & (1 << n) != 0, "{name}: filament {} ({}) is not printable on its nozzle {}", i + 1, f.kind, n + 1);
            }
            state.settings.materials = layout.trays.iter().take(2).map(material).collect();
            let sliced = run_slice(0, &state.settings, Some(&cat), &plate, machine, name).unwrap();
            sliced.checked.as_ref().unwrap_or_else(|why| panic!("{name}: {why}"));
            let heads: std::collections::BTreeSet<_> = sliced.preview.beads.each()
                .filter(|b| bead::kind(b) == kind::EXTRUDE).map(|b| bead::tag(b) as usize % 2).collect();
            let tools = vp.kind == VirtualKind::Toolchanger;
            assert_eq!(heads, if tools { [0, 1].into_iter().collect() } else { [0].into_iter().collect() }, "{name} bead nozzles");
            let bytes = sliced.settled.package_with(&|_| {}, &|| false).unwrap();
            if tools {
                assert!(std::str::from_utf8(&bytes).unwrap().lines().any(|l| l.starts_with("G1 ")), "plain G-code");
            } else {
                assert!(bytes.starts_with(b"PK"), "3MF package");
            }
            let cfg = &sliced.settled.job.cfg;
            for i in 0..2 {
                assert_eq!(cfg.raw("filament_colour", i), format!("#{:06X}", vp.slots[i].colour.unwrap()), "{name} filament {}", i + 1);
            }
            if !tools { check_switched_admission(sliced, bytes, vp, cat.clone()); }
        }
    }

    /// Exercise both completions with a checked package, including switching away and back
    /// to the same choice and mode before the queued recheck finishes.
    fn check_switched_admission(sliced: Sliced, bytes: Vec<u8>, template: &VirtualPrinter, catalog: Arc<Catalog>) {
        let sliced = Arc::new(sliced);
        let checked = fab_printer_link::safety::validate_job(bytes, &sliced.checked.as_ref().unwrap().profile, &sliced.name).unwrap();
        let choice = PrinterChoice::Real(SIMULATED_PRINTER.into());
        let mut twin = catalog.twins[&template.machine_preset_name()].printer.clone();
        twin.name = "configured".into();
        twin.configured_for = Some(SIMULATED_PRINTER.into());
        let mut saved = Saved::default();
        saved.printers.push(twin);
        let mut printing = Printing { virtuals: Some(Arc::new(saved)), catalog: Some(catalog),
            simulator: Some(std::env::temp_dir().join("fab-no-reports")), ..Default::default() };
        let mut p = PrintState { settings: defaults_settings(), ..Default::default() };
        for was_virtual in [false, true] {
            let link = |printing: &mut Printing, p: &mut PrintState, virtual_mode| {
                let modes = &mut Arc::make_mut(printing.virtuals.as_mut().unwrap()).disconnected;
                if virtual_mode { modes.insert(SIMULATED_PRINTER.into()); } else { modes.clear(); }
                assert!(printing.link_printer(p, &choice));
                p.printers.set(&p.links.picked, printing.linked(&choice).unwrap().epoch, Arc::new(fab_printer_link::Snapshot { virtual_printer: virtual_mode, ..Default::default() }));
            };
            link(&mut printing, &mut p, was_virtual);
            let ticket = p.ticket_for(&choice).unwrap();
            let options = p.print_options;
            let meta = Arc::new(job_meta(&sliced, &choice, p.target_printer().map(Arc::as_ref), &options, Vec::new()));
            let id = meta.id.clone();
            p.follow.jobs.push(CommittedJob { meta: meta.clone(), ..Default::default() });
            p.sending.push(Sending { job: id.clone(), printer: choice.clone() });
            p.confirm_print = Some(PrintConfirm::Filament(p.ticket_for(&choice).unwrap()));
            p.confirm_stop = ControlDestination::filament(PrinterScope::Shown, &p);
            p.print_options.plate_clear = true;
            p.follow.confirm_start = Some(id.clone());
            let to = PackageFor::Printer(ToPrinter { ticket: ticket.clone(), options, meta: meta.clone(), scripts: Arc::default(), queue: false });
            printing.packaging.push(Packaging { id: 1, sliced: sliced.clone(), to, stop: Arc::default() });
            link(&mut printing, &mut p, !was_virtual);
            assert!(p.confirm_print.is_none() && !p.print_options.plate_clear && p.confirm_stop.is_none() && p.follow.confirm_start.is_none());
            assert_eq!(printing.event(&mut p, &PrintEvent::Packaged { id: 1, result: Packaged::Checked(checked.clone()) }), PrintOutcome::Target);
            assert_eq!(p.print_blocked.get(&choice), Some(STALE_DISPATCH));
            assert_eq!(p.follow.job_of(&id).unwrap().state, JobState::Queued);
            let other_mode = p.ticket_for(&choice).unwrap();
            assert_eq!(printing.start_job(&mut p, &id, &other_mode).unwrap_err(), WRONG_DESTINATION);
            assert_eq!(printing.send_job(&mut p, &other_mode, checked.clone(), &meta, options).unwrap_err(), WRONG_DESTINATION);

            p.print_blocked.set(&choice, None);
            printing.starting.push((id.clone(), 2));
            p.sending.push(Sending { job: id.clone(), printer: choice.clone() });
            link(&mut printing, &mut p, was_virtual);
            assert_eq!(printing.send_job(&mut p, &ticket, checked.clone(), &meta, options).unwrap_err(), STALE_DISPATCH);
            assert_eq!(printing.event(&mut p, &PrintEvent::JobChecked { id: id.clone(), attempt: 2, ticket, options, result: Ok(checked.clone()) }), PrintOutcome::Target);
            assert_eq!(p.print_blocked.get(&choice), Some(STALE_DISPATCH));
            let job = p.follow.job_of(&id).unwrap();
            assert_eq!((job.link_send, &job.state), (0, &JobState::Queued));
        }
    }

    #[test]
    fn virtual_painted_job_packages_identically_in_both_colour_modes() {
        use fab_plate::ColourMode;
        let printers = virtual_printer::presets();
        let vp = printers.iter().find(|p| p.name == "nine-materials").unwrap();
        let destination = PrinterChoice::Virtual(vp.name.clone());
        let mut printing = Printing::default();
        printing.virtuals = Some(Arc::new(Saved { printers: printers.clone(), picked: None, disconnected: Default::default(), bindings: Default::default() }));
        let mut state = PrintState { settings: defaults_settings(), ..Default::default() };
        assert!(printing.link_printer(&mut state, &destination));
        let imports = std::env::temp_dir().join(format!("fab-painted-job-{}", std::process::id()));
        let catalog = Arc::new(catalog_in(&[], &imports, &printers).unwrap());
        printing.event(&mut state, &PrintEvent::Catalog(Some(catalog)));
        let machine = printing.machine().unwrap().clone();
        let heads = colours::Offline::of(&machine).heads;
        let report = fab_printer_link::virtual_link::report(&setup_of(vp, None).unwrap());
        state.printers.set(&state.links.picked, 1, Arc::new(fab_printer_link::Snapshot {
            name: vp.title.clone(), virtual_printer: true, state: Arc::new(report.clone()), ..Default::default()
        }));
        let layout = colours::layout(Some((&vp.title, &report)), None, &colours::Offline::of(&machine), &|_| None);
        let script = r#"use mod.model.*
use mod.fab.*
let m = mod.model
let block = m.box({size: [6mm 0.6mm 3mm]})
object(block)
m.fill(block, Material{albedo: #3030cc name: "Body"})
m.fill(m.box({size: [7mm 0.3mm 4mm] at: [0 0.22mm 0]}), Material{albedo: #f02020 name: "Stripe"})
"#;
        let model = fab_model::FabModel::with_script("block", "block.cad.splash", script);
        let mut evaluator = fab_model::eval::Evaluator::new(machine);
        let (_, mut ev) = model.evaluate(&mut evaluator, None).unwrap();
        ev.part_colors[0] = "Body".into();
        let mut tray_colours = Vec::new();
        let mut fallback = Vec::new();
        for (c, tray) in ev.colors.iter_mut().filter(|c| matches!(c.name.as_str(), "Body" | "Stripe")).zip(&layout.trays) {
            c.at.push(fab_plate::PrintColorAt {
                printer: layout.printer.clone(), side: colours::head_word(&heads, tray.pos.head), tray: tray.pos.tray_text(),
            });
            c.fallback = [0.12, 0.25, 0.41];
            c.rgb = (c.name == "Stripe").then_some([0.65, 0.33, 0.12]);
            assert_ne!(c.design, Some(c.fallback), "design and print fallback must be distinguishable");
            let [r, g, b] = c.rgb.unwrap_or(c.fallback);
            fallback.push([r, g, b, 1.0]);
            tray_colours.push(tray.colour.unwrap());
        }
        assert_eq!(tray_colours.len(), 2);
        remap(&mut printing, &mut state, &mut ev);
        // A painted top over a solid first layer; dry travel lifts clear even on this small part.
        ev.plate.settings.extend([("filament_retraction_minimum_travel".into(), "0".into()),
            ("filament_z_hop_types".into(), "Normal Lift".into()), ("filament_z_hop".into(), "0.4".into())]);
        let package = |printing: &Printing, state: &PrintState, ev: &mut Evaluated, mode, expected: &[[f32; 4]], displayed: &[[f32; 4]]| {
            ev.set_colour_view(mode, state.target_printer().is_some());
            let source = SliceSource::Model(Arc::new(ev.clone()), Arc::from([]), None);
            let (job, input) = printing.job(state, source, "painted".into()).prepare();
            let identity = (input.plate_id, input.settings_id);
            let input = (input.make)();
            let setup = (input.setup)(None).unwrap();
            let hex: Vec<_> = expected.iter().map(|&c| Some(colour_hex(c))).collect();
            assert_eq!(setup.filament_colours, hex, "resolved print colours must use the tray, override or fallback");
            assert!(!hex.contains(&Some(colour_hex(unmapped_colour()))), "the fixture never prints warning red");
            assert_eq!(job.colours, displayed, "the Material preview shows only reported ink");
            let res = makepad_fab_slice::live::slice_frames(&setup, &input.plate, input.frames.as_deref().unwrap(), &input.paint, None).unwrap();
            let sliced = finish_test_slice(job, setup, res, &input.plate);
            let meta = job_meta(&sliced, &destination, state.target_printer().map(Arc::as_ref), &state.print_options, Vec::new());
            assert!(meta.virtual_printer, "confirm captures the virtual destination");
            assert_eq!(meta.used.len(), 2, "both painted filaments printed");
            assert_eq!(meta.colours, expected, "held jobs retain the actual print colours");
            assert_eq!(meta.model_colours.len(), 2);
            let bytes = sliced.settled.package_with(&|_| {}, &|| false).unwrap();
            let mut zip = std::io::Cursor::new(&bytes);
            let dir = makepad_zip_file::zip_read_central_directory(&mut zip).unwrap();
            let member = dir.file_headers.iter().find(|h| h.file_name == "Metadata/plate_1.json").unwrap().extract(&mut zip).unwrap();
            let plate = makepad_strict_json::parse(&member).unwrap();
            let stored: Vec<_> = plate.get("filament_colors").unwrap().as_arr().unwrap().iter().map(|c| c.as_str().unwrap()).collect();
            assert_eq!(stored, meta.used.iter().map(|&i| hex[i].as_deref().unwrap()).collect::<Vec<_>>(), "packaged filament colours");
            if expected == displayed {
                for (model, spool) in meta.model_colours.iter().zip(&meta.colours) {
                    assert_ne!(model, spool, "model and spool differ for each printed tool");
                }
                let checked = fab_printer_link::safety::validate_job(bytes, &sliced.checked.unwrap().profile, &meta.name).unwrap();
                (checked.bytes().to_vec(), identity)
            } else {
                let held = preview_of_package(&bytes, &meta).unwrap();
                assert_eq!(held.colours.material, displayed, "reopened Material preview");
                assert_eq!(held.colours.model, meta.model_colours, "reopened Model preview");
                (bytes, identity)
            }
        };
        let (model, model_input) = package(&printing, &state, &mut ev, ColourMode::Model, &tray_colours, &tray_colours);
        let (material, material_input) = package(&printing, &state, &mut ev, ColourMode::Material, &tray_colours, &tray_colours);
        if model != material {
            let offset = model.iter().zip(&material).position(|(a, b)| a != b).unwrap_or(model.len().min(material.len()));
            panic!("app send package differs at byte {offset}: {:?} vs {:?}", model.get(offset), material.get(offset));
        }

        assert_eq!(model_input, material_input, "the live slice keeps the same plate and settings identity");

        let mut no_ink = report;
        for tray in no_ink.ams.iter_mut().flat_map(|unit| &mut unit.trays).chain(&mut no_ink.external) {
            tray.colour = None;
        }
        Arc::make_mut(state.target_printer_mut().unwrap()).state = Arc::new(no_ink);
        remap(&mut printing, &mut state, &mut ev);
        let red = [unmapped_colour(); 2];
        package(&printing, &state, &mut ev, ColourMode::Material, &fallback, &red);

        state.printers.clear();
        printing.loadout = Some(layout);
        remap(&mut printing, &mut state, &mut ev);
        package(&printing, &state, &mut ev, ColourMode::Material, &tray_colours, &red);

        let mut one = defaults_settings();
        one.materials = state.settings.materials[..1].to_vec();
        assert_eq!(resolve_setup(&one, None, &ev.plate, &ev.machine).unwrap().filament_colours,
            [Some(colour_hex(tray_colours[0])), None], "a tool past the materials uses its preset");

        ev.set_colour_view(ColourMode::Material, true);
        Arc::make_mut(&mut ev.mapped).inks.remove("Stripe");
        let unknown = tool_colours(&ev, None);
        assert_eq!(unknown.material[1], unmapped_colour(), "mapped slot without reported ink");
        // Both names using one tool take the first model colour in project order.
        let many = Arc::make_mut(&mut ev.mapped);
        many.tools.insert("Stripe".into(), (1, 0));
        ev.plate.map_colors(&ev.part_colors, &|name| many.tools.get(name).copied());
        let palettes = tool_colours(&ev, None);
        assert_eq!(palettes.shared_tools, [0]);
        let first = ev.colors.iter().find(|c| c.name == "Body" || c.name == "Stripe").unwrap();
        assert_eq!(palettes.model[0][..3], fab_model::eval::design_color(&ev.colors, &first.name));
    }

    /// A resin printer preset is a resin machine: its screen for a bed, no nozzles.
    #[test]
    fn a_resin_printer_preset_is_a_resin_machine() {
        let values = fab_sla::printer::simulated_config();
        let m = machine_of("resin", &values).unwrap();
        let screen = m.resin_screen().expect("a resin printer");
        assert_eq!(screen.pixels, [2400, 1600]);
        assert_eq!(m.bed, Rect::new(0.0, 0.0, 120.0, 80.0));
        assert!(m.nozzles.is_empty());
    }

    /// With nothing installed, the catalogue offers the resin printer Fab carries itself and its
    /// three processes, made for it. Resolved, its preset is a resin machine whose bed is its
    /// screen of 13312 × 5120 pixels, 16.8 by 24.8 µm.
    #[test]
    fn the_builtin_resin_printer_is_offered_and_its_bed_is_its_screen() {
        use fab_sla::printer::{BUILTIN_PRINTER, BUILTIN_PROCESSES};
        let dir = std::env::temp_dir().join(format!("fab-builtin-catalog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cat = catalog_in(std::slice::from_ref(&dir), &dir.join("imports"), &[]).expect("the built-in presets");
        assert!(!cat.installed, "nothing installed: Fab defaults keep their lists");
        assert!(cat.entries.iter().any(|e| e.kind == Kind::Machine && e.name == BUILTIN_PRINTER.0 && !e.user), "the printer is offered");
        for (name, _) in BUILTIN_PROCESSES {
            assert!(cat.entries.iter().any(|e| e.kind == Kind::Process && e.name == name && e.compat == [BUILTIN_PRINTER.0]), "{name} is offered for it");
        }
        let preset = cat.store.resolve(BUILTIN_PRINTER.0).expect("the printer resolves");
        let m = machine_of(BUILTIN_PRINTER.0, &preset.values).expect("a resin machine");
        let screen = m.resin_screen().expect("a resin printer");
        assert_eq!((screen.pixels, screen.pitch, m.max_height), ([13312, 5120], [0.0168, 0.0248], 230.0));
        assert!((m.bed.width() - 223.6416).abs() < 1e-9 && (m.bed.depth() - 126.976).abs() < 1e-9, "the bed is the screen: {:?}", m.bed);
        assert!(m.nozzles.is_empty());
    }

    #[test]
    fn printer_models_match_exactly() {
        for (configured, printer_model, expected) in [
            ("H2D", "Bambu Lab H2D", true),
            ("A1", "Bambu Lab A1 mini", false),
            ("A1 mini", "Bambu Lab A1 mini", true),
            ("X1", "Bambu Lab X1E", false),
            (" bambu lab h2d ", "Bambu Lab H2D", true),
            ("", "Bambu Lab H2D", false),
        ] {
            assert_eq!(same_printer(configured, printer_model), expected, "{configured:?} / {printer_model:?}");
        }
    }

    #[test]
    fn preview_beads_extend_only_across_continuing_filament() {
        let a = MoveRecord { kind: MoveKind::Extrude, p0: [0.0, 0.0, 0.2], p1: [10.0, 0.0, 0.2],
            width: 0.4, height: 0.2, e: 1.0, layer: 1, z_layer: 0.2, filament: 0, t_end: 1.0, ..Default::default() };
        let b = MoveRecord { p0: a.p1, p1: [20.0, 0.0, 0.2], filament: 1, t_start: 1.0, t_end: 2.0, ..a };
        let c = MoveRecord { p0: b.p1, p1: [20.0, 10.0, 0.2], t_start: 2.0, t_end: 3.0, ..b };
        let moves = [a, b, c];
        let preview = build_preview(&moves, &Default::default(), vec![[1.0; 4]; 2], &Machine::dual_nozzle_350(), &Default::default());
        let beads: Vec<Bead> = all_beads(&preview.beads).chunks_exact(PREVIEW_SLOTS).map(Bead::of).collect();
        assert_eq!(beads.len(), 3);
        assert!(beads[0].width < 0.0, "A starts the path");
        assert!(beads[0].height < 0.0, "A ends at the filament change");
        assert!(beads[1].width < 0.0, "B starts at the filament change");
        assert!(beads[1].height > 0.0, "B continues into C");
        assert!(beads[2].width > 0.0, "C continues from B");
        assert!(beads[2].height < 0.0, "C ends the chunk");
        for (bead, m) in beads.iter().zip(&moves) {
            // the section as its f16s hold it
            let section = Bead::of(&Bead { width: m.width, height: m.height, ..Bead::default() }.pack());
            assert_eq!((bead.width.abs(), bead.height.abs()), (section.width, section.height));
        }
    }

    #[test]
    fn wall_beads_take_their_steps_to_the_walls_above_and_below() {
        // Four layers of a square wall leaning out 0.1 mm a layer, an inner wall inside it, the
        // loops anticlockwise (the outside on the right).
        let machine = Machine::dual_nozzle_350();
        let layer = |k: u32| {
            let mut moves = Vec::new();
            for (role, inset) in [(Role::OuterWall, 0.0), (Role::InnerWall, 0.42)] {
                let r = 5.0 + 0.1 * k as f32 - inset;
                let z = 0.2 * k as f32;
                let corners = [[-r, -r], [r, -r], [r, r], [-r, r], [-r, -r]];
                for c in corners.windows(2) {
                    moves.push(MoveRecord { kind: MoveKind::Extrude, role, p0: [c[0][0], c[0][1], z], p1: [c[1][0], c[1][1], z],
                        width: 0.42, height: 0.2, e: 1.0, layer: k, z_layer: z, ..Default::default() });
                }
            }
            moves
        };
        let layers: Vec<Vec<MoveRecord>> = (1..=4).map(layer).collect();
        let all: Vec<MoveRecord> = layers.concat();
        let preview = build_preview(&all, &Default::default(), vec![[1.0; 4]; 2], &machine, &Default::default());
        let data = all_beads(&preview.beads);
        let beads: Vec<&[f32]> = data.chunks_exact(PREVIEW_SLOTS).collect();
        let steps = |b: &[f32]| {
            let s = (fab::print::bead::role(b) / stack::ROLE_STRIDE).floor() as u32;
            (s % 256, s / 256)
        };
        let out = stack::step(0.1, 0.42);
        let back = stack::step(-0.1, 0.42);
        assert_eq!((out, back), (stack::IN_LINE + 30, stack::IN_LINE - 30));
        assert_eq!(beads.len(), 32);
        for b in &beads {
            let k = fab::print::bead::layer(b) as u32;
            let below = if k == 1 { stack::NONE } else { back };
            let above = if k == 4 { stack::NONE } else { out };
            assert_eq!(steps(b), (below, above), "layer {k}: the wall below stands in, the one above out, none past the ends");
            let role = stack::role(fab::print::bead::role(b));
            assert!(role == Role::OuterWall as u8 as f32 || role == Role::InnerWall as u8 as f32);
        }
        // The live slice's layers take the same steps from their neighbours' moves.
        for k in 1..=4usize {
            let (mut live, _) = layer_beads(&machine, vec![StreamRecords { moves: layers[k - 1].clone(), shift: [0.0; 2], object: 0 }]);
            let lines = |n: usize| (1..=4).contains(&n).then(|| WallLines::of_moves(layers[n - 1].iter()));
            stack_walls(live.chunks_exact_mut(PREVIEW_SLOTS), lines(k - 1).as_ref(), lines(k + 1).as_ref());
            let settled: Vec<f32> = beads.iter().filter(|b| fab::print::bead::layer(b) as usize == k).map(|b| fab::print::bead::role(b)).collect();
            assert_eq!(live.chunks_exact(PREVIEW_SLOTS).map(fab::print::bead::role).collect::<Vec<_>>(), settled, "layer {k}");
        }
    }

    /// A bead's start and end joints ([`fab::print::join_beads`]; 127 is square).
    fn joints(bead: &[f32]) -> (f32, f32) {
        let joints = (Bead::of(bead).filament / 256.0).floor();
        (joints % 256.0, (joints / 256.0).floor())
    }

    /// A bead's direction on the plate.
    fn heading(bead: &[f32]) -> (f32, f32) {
        let (dx, dy) = (bead[4] - bead[0], bead[5] - bead[1]);
        let len = dx.hypot(dy);
        (dx / len, dy / len)
    }

    /// The right and left edges of a bead's end (or start), sheared by its joint as the bead
    /// shader places them: from the end's point, in half widths.
    fn sheared_edges(bead: &[f32], at_end: bool) -> [(f32, f32); 2] {
        let (fx, fy) = heading(bead);
        let (start, end) = joints(bead);
        let shear = ((if at_end { end } else { start }) - 127.0) / 100.0;
        [1.0f32, -1.0].map(|lateral| (fx * lateral * shear + fy * lateral, fy * lateral * shear - fx * lateral))
    }

    /// The one tip two beads end in past a right angle, as the bead shader finds it: from the
    /// corner, in half widths.
    fn joint_tip(bead: &[f32], at_end: bool) -> (f32, f32) {
        let (fx, fy) = heading(bead);
        let (start, end) = joints(bead);
        let turn = (if at_end { end } else { start }) - 127.0;
        assert!(turn.abs() > 100.5, "{turn} is a shear");
        let out = (126.0 - turn.abs()) * 0.028284;
        let (along, side) = ((1.0 - out * out).sqrt(), out.copysign(turn));
        let into = if at_end { -1.0 } else { 1.0 };
        let (a, s) = (-into * along, -into * side);
        (fx * a + fy * s, fy * a - fx * s)
    }

    /// How far apart the sections of bead `a`'s end and the next bead `b`'s start are where they
    /// meet, in half widths: their sheared edges, or past a right angle their tips.
    fn joint_gap(a: &[f32], b: &[f32]) -> f32 {
        if (joints(a).1 - 127.0).abs() <= 100.0 {
            let (ends, starts) = (sheared_edges(a, true), sheared_edges(b, false));
            ends.iter().zip(&starts).map(|(e, s)| (e.0 - s.0).hypot(e.1 - s.1)).fold(0.0, f32::max)
        } else {
            let (e, s) = (joint_tip(a, true), joint_tip(b, false));
            (e.0 - s.0).hypot(e.1 - s.1)
        }
    }

    #[test]
    fn preview_beads_meet_in_one_tube_at_corners() {
        // The two beads of a path turning by `degrees` that meet at the corner.
        let corner = |degrees: f32| {
            let (s, c) = degrees.to_radians().sin_cos();
            let a = MoveRecord { kind: MoveKind::Extrude, p1: [10.0, 0.0, 0.2], p0: [0.0, 0.0, 0.2], width: 0.4, height: 0.2, e: 1.0, layer: 1, z_layer: 0.2, t_end: 1.0, ..Default::default() };
            let b = MoveRecord { p0: a.p1, p1: [10.0 + 10.0 * c, 10.0 * s, 0.2], t_start: 1.0, t_end: 2.0, ..a };
            let preview = build_preview(&[a, b], &Default::default(), vec![[1.0; 4]; 1], &Machine::dual_nozzle_350(), &Default::default());
            let beads: Vec<[f32; PREVIEW_SLOTS]> = all_beads(&preview.beads).chunks_exact(PREVIEW_SLOTS).map(|c| c.try_into().unwrap()).collect();
            // Past a right angle each move's end at the corner is a bead of its own, one width long
            // (`split_tips`), straight on from the rest of the move and its share of the time: the
            // tip leans it alone, not the move's whole 10 mm.
            let at = beads.iter().position(|b| b[4..7] == a.p1[..]).expect("a bead ends at the corner");
            let tip = (joints(&beads[at]).1 - 127.0).abs() > 100.0;
            assert_eq!((beads.len(), at), if tip { (4, 1) } else { (2, 0) }, "{degrees}");
            if tip {
                let length = |b: &[f32; PREVIEW_SLOTS]| (b[4] - b[0]).hypot(b[5] - b[1]);
                for (rest, own) in [(&beads[0], &beads[1]), (&beads[3], &beads[2])] {
                    assert!((length(own) - 0.4).abs() < 1e-3 && (length(rest) - 9.6).abs() < 1e-3, "a turn of {degrees}: the tip's bead {} mm, the rest {} mm", length(own), length(rest));
                }
                assert_eq!((joints(&beads[0]).1, joints(&beads[1]).0, joints(&beads[2]).1, joints(&beads[3]).0), (127.0, 127.0, 127.0, 127.0), "a turn of {degrees}: the tips' beads go straight on from the rest");
                let (own_a, own_b) = (Bead::of(&beads[1]), Bead::of(&beads[2]));
                assert!((own_a.t0 - 0.96).abs() < 1e-3 && (own_a.t1() - 1.0).abs() < 1e-3 && (own_b.t1() - 1.04).abs() < 1e-3, "a turn of {degrees}: the tips' beads take their share of the time: {own_a:?} {own_b:?}");
            }
            [beads[at], beads[at + 1]]
        };
        for degrees in [1.0, 5.0, 10.0, 30.0, 45.0, 60.0, 89.0, 90.0, -5.0, -30.0, -45.0, -90.0] {
            let beads = corner(degrees);
            let gap = joint_gap(&beads[0], &beads[1]);
            assert!(gap < 0.01, "a turn of {degrees} leaves the two sections {gap} apart per unit of half width");
        }
        // A right angle shears whichever side of square the moves' rounding leaves it: a wall's
        // corner draws alike on every layer (a lattice's corner pole had one layer in six a tip).
        for degrees in [89.99f32, 90.0, 90.01, 90.2, -90.0, -90.01] {
            let beads = corner(degrees);
            let (end, start) = (joints(&beads[0]).1, joints(&beads[1]).0);
            assert_eq!(((end - 127.0).abs(), (start - 127.0).abs()), (100.0, 100.0), "a turn of {degrees} is not a right angle's shear: {end} {start}");
        }
        // Past a right angle both beads end in one tip, on the outside of the corner.
        for degrees in [91.0, 100.0, 120.0, 135.0, 150.0, 170.0, 180.0, -100.0, -135.0, -170.0] {
            let beads = corner(degrees);
            let (end, start) = (joint_tip(&beads[0], true), joint_tip(&beads[1], false));
            let gap = joint_gap(&beads[0], &beads[1]);
            assert!(gap < 0.04, "a turn of {degrees} leaves two tips {gap} apart: {end:?} {start:?}");
            // The outside of the corner, halfway between the bead that comes in and the one that leaves.
            let half = (degrees as f32 / 2.0).to_radians();
            let outside = (half.sin().abs(), -half.cos().copysign(degrees));
            let reach = (end.0 - outside.0).hypot(end.1 - outside.1);
            assert!(reach < 0.04, "a turn of {degrees} puts the tip at {end:?}, not at {outside:?}");
        }
        // A stub of microns says nothing about the direction: the wall runs straight through it.
        let a = MoveRecord { kind: MoveKind::Extrude, p0: [0.0, 0.0, 0.2], p1: [0.5, 0.0, 0.2], width: 0.4, height: 0.2, e: 1.0, layer: 1, z_layer: 0.2, t_end: 1.0, ..Default::default() };
        let stub = MoveRecord { p0: a.p1, p1: [0.5, 0.002, 0.2], t_start: 1.0, t_end: 1.1, ..a };
        let c = MoveRecord { p0: stub.p1, p1: [1.0, 0.002, 0.2], t_start: 1.1, t_end: 2.0, ..a };
        let preview = build_preview(&[a, stub, c], &Default::default(), vec![[1.0; 4]; 1], &Machine::dual_nozzle_350(), &Default::default());
        for bead in all_beads(&preview.beads).chunks_exact(PREVIEW_SLOTS) {
            let bead: &[f32; PREVIEW_SLOTS] = bead.try_into().unwrap();
            let (start, end) = joints(bead);
            assert!((start - 127.0).abs() <= 100.0 && (end - 127.0).abs() <= 100.0, "a stub of 2 microns makes a tip: {start} {end} of {} beads", preview.beads.count());
        }
        let beads = corner(45.0);
        assert_eq!((joints(&beads[0]).0, joints(&beads[1]).1), (127.0, 127.0), "the path's own ends stay square");
        assert_eq!((Bead::of(&beads[0]).filament % 256.0, Bead::of(&beads[1]).filament % 256.0), (0.0, 0.0), "the filament stays below the joints");
    }

    /// A round part sliced for real draws each extrusion once, and the beads of one path meet
    /// with no gap at every joint: the short chords round its curves and its top layer included.
    /// Two beads over one move fight for the same pixels; a path broken into free ends shows
    /// seams and gaps between its sections. Nothing on the top layer stacks under a wall.
    #[test]
    fn sliced_beads_draw_each_move_once_and_meet_along_their_paths() {
        let plate = Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part("round", 1, None, cylinder([150.0, 150.0], 8.0, 2.0, 120))] };
        let machine = Machine::dual_nozzle_350();
        let s = run_slice(0, &settings(1), None, &plate, &machine, "round").unwrap();
        let pv = &s.preview;
        let top = pv.max_layer();
        let data = all_beads(&pv.beads);
        let beads: Vec<&[f32]> = data.chunks_exact(PREVIEW_SLOTS).filter(|b| fab::print::bead::kind(b) % kind::MODE_STRIDE == kind::EXTRUDE).collect();
        // No two beads on one span.
        let point = |p: &[f32]| p.iter().map(|v| (v * 1000.0).round() as i64).collect::<Vec<_>>();
        let mut spans = std::collections::HashSet::new();
        for b in &beads {
            let (a, z) = (point(&b[0..3]), point(&b[4..7]));
            assert!(spans.insert(if a <= z { (a, z) } else { (z, a) }), "two beads on one span: {:?}", Bead::of(b));
        }
        // Each layer's beads as long as its extrusions, their arcs as chords within a sagitta.
        let mut lengths: std::collections::BTreeMap<u32, (f64, f64)> = Default::default();
        for m in s.settled.layers.chunks.iter().flat_map(|c| c.moves.iter()).filter(|m| m.kind == MoveKind::Extrude && m.e > 0.0 && m.width > 0.0) {
            let d = [m.p1[0] - m.p0[0], m.p1[1] - m.p0[1], m.p1[2] - m.p0[2]];
            let len = if m.flags & flags::ARC != 0 && m.sweep.abs() > 1e-6 {
                (m.p0[0] - m.center[0]).hypot(m.p0[1] - m.center[1]) * m.sweep.abs()
            } else {
                (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
            };
            lengths.entry(m.layer).or_default().0 += len as f64;
        }
        for b in &beads {
            let d = [b[4] - b[0], b[5] - b[1], b[6] - b[2]];
            lengths.entry(fab::print::bead::layer(b) as u32).or_default().1 += ((d[0] * d[0] + d[1] * d[1] + d[2] * d[2]) as f64).sqrt();
        }
        assert_eq!(lengths.keys().last(), Some(&top));
        for (layer, (extruded, drawn)) in &lengths {
            assert!((drawn - extruded).abs() <= 0.005 * extruded, "layer {layer}: {drawn:.2} mm of beads for {extruded:.2} mm extruded");
        }
        // Where a bead takes up its path at the same point and moment (its end time is its start
        // and its f16 duration: within a millisecond), the two are joined and their sections meet.
        let (mut joined, mut on_top, mut on_curves) = (0usize, 0usize, 0usize);
        let chord = |b: &[f32]| (b[4] - b[0]).hypot(b[5] - b[1]);
        for pair in beads.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let (ba, bb) = (Bead::of(a), Bead::of(b));
            if ba.b != bb.a || (ba.t1() - bb.t0).abs() > 1e-3 || ba.layer != bb.layer || ba.filament % 256.0 != bb.filament % 256.0 {
                continue;
            }
            assert!(ba.height > 0.0 && bb.width > 0.0, "layer {}: the path breaks at {:?}", ba.layer, ba.b);
            let sharp = (joints(a).1 - 127.0).abs() > 100.0;
            let gap = joint_gap(a, b);
            assert!(gap < if sharp { 0.04 } else { 0.01 }, "layer {}: the path's sections are {gap} half widths apart at {:?}", ba.layer, ba.b);
            joined += 1;
            on_top += (ba.layer as u32 == top) as usize;
            on_curves += (chord(a) < 2.0 && chord(b) < 2.0 && joints(a).1 != 127.0) as usize;
        }
        assert!(on_top > 50 && on_curves > 200, "{joined} joints, {on_top} on the top layer, {on_curves} between short chords that turn");
        // A skin's or solid fill's bead says where its layer's beads lie beside it instead.
        let (mut skins, mut sided) = (0usize, 0usize);
        for b in beads.iter().filter(|b| fab::print::bead::layer(b) as u32 == top) {
            let steps = (fab::print::bead::role(b) / stack::ROLE_STRIDE).floor() as u32;
            if stack::sides(stack::role(fab::print::bead::role(b))) {
                skins += 1;
                sided += (steps != 0) as usize;
            } else {
                assert_eq!(steps / 256, stack::NONE, "a bead on the top layer stacks under a wall");
            }
        }
        assert!(skins > 0 && sided * 2 > skins, "{sided} of the top layer's {skins} skin beads have a line beside them");
    }

    #[test]
    fn preview_arcs_take_the_chords_they_need() {
        // On this radius a chord within the sagitta may span 4 mm.
        let on_circle = |a: f32| [100.0 * a.cos(), 100.0 * a.sin(), 0.2];
        let arc = |a0: f32, a1: f32, t_start: f32| MoveRecord { kind: MoveKind::Extrude, flags: flags::ARC, p0: on_circle(a0), p1: on_circle(a1), sweep: a1 - a0,
            width: 0.4, height: 0.2, e: 1.0, layer: 1, z_layer: 0.2, t_start: t_start as f64, t_end: t_start as f64 + 1.0, ..Default::default() };
        let preview = |moves: &[MoveRecord]| build_preview(moves, &Default::default(), vec![[1.0; 4]; 1], &Machine::dual_nozzle_350(), &Default::default());
        let short = [arc(0.0, 0.005, 0.0), arc(0.005, 0.01, 1.0)];
        let joined = preview(&short);
        assert_eq!(joined.beads.count(), 1, "short arcs join into one chord");
        let bead = Bead::of(&all_beads(&joined.beads));
        assert_eq!((bead.b, bead.t1()), (short[1].p1, 2.0), "it ends where the second ends");
        assert_eq!(preview(&[arc(0.0, 0.03, 0.0), arc(0.03, 0.06, 1.0)]).beads.count(), 2, "one chord for both would leave the arc");
        assert_eq!(preview(&[arc(0.0, 0.19, 0.0)]).beads.count(), 5, "a long arc stays round");
    }

    #[test]
    fn a_layers_height_leaves_out_the_start_sequence_under_it() {
        let start = MoveRecord { kind: MoveKind::Travel, p0: [0.0, 0.0, 0.0], p1: [10.0, 0.0, 0.0], layer: 0, z_layer: 0.0, t_end: 1.0, ..Default::default() };
        let bead = MoveRecord { kind: MoveKind::Extrude, p0: [0.0, 0.0, 0.2], p1: [10.0, 0.0, 0.2], width: 0.4, height: 0.2, e: 1.0, layer: 0, z_layer: 0.2, t_start: 1.0, t_end: 2.0, ..Default::default() };
        let preview = build_preview(&[start, bead], &Default::default(), vec![[1.0; 4]; 1], &Machine::dual_nozzle_350(), &Default::default());
        assert_eq!((preview.layers.len(), preview.layers[0].z), (1, 0.2));
    }

    fn cube(min: [f64; 3], size: f64) -> PartMesh {
        let positions = (0..8u32).map(|i| [min[0] + size * (i & 1) as f64, min[1] + size * ((i >> 1) & 1) as f64, min[2] + size * ((i >> 2) & 1) as f64]).collect();
        let triangles = vec![[0, 2, 1], [1, 2, 3], [4, 5, 6], [5, 7, 6], [0, 1, 4], [1, 5, 4], [2, 6, 3], [3, 6, 7], [0, 4, 2], [2, 4, 6], [1, 3, 5], [3, 7, 5]];
        PartMesh { positions, triangles }
    }

    /// A prism of `sides` round a vertical axis: a round part's walls in short moves.
    fn cylinder(centre: [f64; 2], radius: f64, height: f64, sides: u32) -> PartMesh {
        let mut positions = vec![[centre[0], centre[1], 0.0], [centre[0], centre[1], height]];
        for i in 0..sides {
            let (s, c) = (std::f64::consts::TAU * i as f64 / sides as f64).sin_cos();
            positions.extend([[centre[0] + radius * c, centre[1] + radius * s, 0.0], [centre[0] + radius * c, centre[1] + radius * s, height]]);
        }
        let triangles = (0..sides).flat_map(|i| {
            let (b0, t0, b1, t1) = (2 + 2 * i, 3 + 2 * i, 2 + 2 * ((i + 1) % sides), 3 + 2 * ((i + 1) % sides));
            [[0, b1, b0], [1, t0, t1], [b0, b1, t1], [b0, t1, t0]]
        }).collect();
        PartMesh { positions, triangles }
    }

    fn part(name: &str, filament: u32, nozzle: Option<usize>, mesh: PartMesh) -> Part {
        Part { body_of: None, print: Vec::new(), print_mesh: None, paint: None, object: name.into(), name: name.into(), filament, color: [1.0, 0.2, 0.2, 1.0], nozzle, mesh, field_bounds: None, field_floor: None, ..Default::default() }
    }

    /// Fab defaults with `n` materials, alternating left and right.
    fn settings(n: usize) -> PrintSettings {
        let mut s = defaults_settings();
        s.materials = (0..n).map(|i| JobMaterial { preset: String::new(), color: [0.9, 0.9, 0.9, 1.0], nozzle: i % 2, tray: None, dedicated: false }).collect();
        s
    }

    #[test]
    fn calibration_parts_slice_with_their_bands() {
        use fab_calibrate::{CouponSetup, Step};
        let machine = Machine::dual_nozzle_350();
        for step in [Step::Clearance, Step::Snap, Step::Flow, Step::Temperature, Step::PressureAdvance] {
            let c = fab_calibrate::make(step, &CouponSetup { layer: 0.4, ..Default::default() }).unwrap();
            let bounds = c.parts.iter().fold(fab_plate::Bounds::EMPTY, |b, (_, mesh)| b.union(&mesh.bounds()));
            let centre = machine.bed_center();
            let by = [centre[0] - (bounds.min[0] + bounds.max[0]) * 0.5, centre[1] - (bounds.min[1] + bounds.max[1]) * 0.5, -bounds.min[2]];
            let parts = c.parts.iter().map(|(name, mesh)| {
                let mut mesh = mesh.clone();
                mesh.translate(by);
                part(name, 1, None, mesh)
            }).collect();
            let mut plate_settings = vec![("fab_calibration_bands".into(), c.bands_text())];
            plate_settings.extend(c.process.clone());
            let plate = Plate { parts, excluded: Vec::new(), settings: plate_settings };
            let setup = resolve_setup(&settings(1), None, &plate, &machine).unwrap();
            let res = makepad_fab_slice::plate::slice_plate_painted_with(&setup, &plate, &[], &|_, _| {}).unwrap_or_else(|e| panic!("{step:?}: {e}"));
            let g = &res.gcode;
            assert!(g.lines().filter(|l| l.starts_with("G1") && l.contains(" E")).count() > 100, "{step:?} extrudes");
            match step {
                Step::Temperature => {
                    for t in [230, 225, 220, 215, 210, 205] {
                        assert!(g.contains(&format!("M104 S{t} T")), "{step:?}: the {t} band");
                    }
                }
                Step::PressureAdvance => {
                    for k in ["0.000", "0.030", "0.060"] {
                        assert!(g.contains(&format!("M900 K{k}")), "{step:?}: K {k}");
                    }
                }
                _ => assert!(!g.contains("M900"), "{step:?}: no PA without a band or a calibrated filament"),
            }
        }
    }

    /// Each colour prints on its own nozzle, in the file the printer reads and in the slice view:
    /// every extrusion's tool in the package's G-code feeds the nozzle its colour is assigned
    /// (the package's filament map), and at sampled moments of each bead the view draws that
    /// nozzle printing, at the bead, with the other one where the toolhead holds it, each in its
    /// own look. The colours sit crosswise (the first on the right nozzle), so no two numberings
    /// agree by accident.
    #[test]
    fn every_extrusion_prints_on_its_colours_nozzle() {
        let plate = Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part("a", 1, None, cube([100.0, 100.0, 0.0], 10.0)), part("b", 2, None, cube([200.0, 100.0, 0.0], 10.0))] };
        let machine = Machine::dual_nozzle_350();
        let mut st = settings(2);
        st.materials[0].nozzle = 1;
        st.materials[1].nozzle = 0;
        let s = run_slice(0, &st, None, &plate, &machine, "heads").unwrap();
        let package = s.settled.package_with(&|_| {}, &|| false).expect("the package");
        let mut zip = std::io::Cursor::new(package);
        let dir = makepad_zip_file::zip_read_central_directory(&mut zip).expect("a package");
        let mut text = |name: &str| String::from_utf8(dir.file_headers.iter().find(|f| f.file_name == name).unwrap_or_else(|| panic!("{name} in the package")).extract(&mut zip).unwrap()).unwrap();
        let gcode = text("Metadata/plate_1.gcode");
        let info = text("Metadata/slice_info.config");
        // The printer's nozzle per filament: 1 left, 2 right.
        let maps: Vec<usize> = info.split("key=\"filament_maps\" value=\"").nth(1).and_then(|v| v.split('"').next()).expect("the filament map")
            .split_whitespace().map(|v| v.parse::<usize>().unwrap() - 1).collect();
        assert_eq!(maps, [1, 0], "the package maps each filament to its colour's nozzle");
        let mut tool = None;
        let mut printed = [0usize; 2];
        for (i, line) in gcode.lines().enumerate() {
            if let Some(t) = line.strip_prefix('T').and_then(|t| t.split_whitespace().next()).and_then(|t| t.parse::<usize>().ok()) {
                tool = Some(t);
                continue;
            }
            let moves = ["G1 ", "G2 ", "G3 "].iter().any(|g| line.starts_with(g)) && (line.contains(" X") || line.contains(" Y"));
            let e = line.split_whitespace().find_map(|w| w.strip_prefix('E')).and_then(|e| e.parse::<f64>().ok()).unwrap_or(0.0);
            // Before the first tool command only the printer's own start routine extrudes.
            if let (true, true, Some(t)) = (moves, e > 0.0, tool) {
                let nozzle = maps.get(t).copied().unwrap_or_else(|| panic!("line {}: T{t} names no filament", i + 1));
                assert_eq!(nozzle, st.materials[t].nozzle, "line {}: tool {t} extrudes on nozzle {nozzle}, its colour's is {}", i + 1, st.materials[t].nozzle);
                printed[nozzle] += 1;
            }
        }
        assert!(printed.iter().all(|n| *n > 100), "both nozzles print: {printed:?}");
        // What the view draws at moments of each bead: both nozzles, each in its own colour
        // whatever it does; the one printing is the colour's, at the bead, and the other waits
        // where the toolhead holds it.
        let head = Toolhead::of(&machine);
        let mut app = fab::AppState::default();
        app.print.preview = Some(s.preview.clone());
        apply(&mut app.print, &PrintAction::Preview(true));
        let mut shown = [0usize; 2];
        let records: Vec<MoveRecord> = s.settled.layers.records().collect();
        for m in records.iter().filter(|m| m.kind == MoveKind::Extrude && m.filament > 0 && m.t_end - m.t_start > 1e-3).step_by(10) {
            let t = (m.t_start + m.t_end) * 0.5;
            apply(&mut app.print, &PrintAction::Seek(t));
            let frame = fab::viewport::print::PrintFrame::of(&app, None).expect("the slice view draws");
            let want = st.materials[m.filament as usize - 1].nozzle;
            let [l, r] = [0, 1].map(|i| frame.heads.get(i).copied().unwrap_or_else(|| panic!("{t:.3} s: both heads are drawn")));
            assert_eq!((l.nozzle, r.nozzle), (0, 1), "{t:.3} s: each head keeps its nozzle's look");
            let printing = if l.active { l } else { r };
            assert!(l.active != r.active && printing.nozzle == want, "{t:.3} s: filament {} prints on nozzle {want}, the view shows {:?} printing", m.filament, frame.heads);
            // On the bead it prints, where the design puts it (`designed`), but for as far as it
            // goes in the rounding of the bead's times.
            let cut = s.preview.cut_at(t);
            let (k, i) = cut_tails(&cut).rev().find(|&(k, i)| { let (a, e) = ends(&instance(&s.preview.beads.0[k], i)); off_line(printing.tip, a, e) <= ON_LINE })
                .unwrap_or_else(|| panic!("{t:.3} s: the printing nozzle {:?} is on a bead started by then", printing.tip));
            let bead = instance(&s.preview.beads.0[k], i);
            let (a, e) = ends(&bead);
            let (t0, t1) = (fab::print::bead::t0(&bead) as f64, fab::print::bead::t1(&bead) as f64);
            let err = span_rounding(t1 - t0, t);
            let speed = (distance(a, e) as f64 / (t1 - t0).max(f64::MIN_POSITIVE)).max(m.v_cruise.max(m.v_entry).max(m.v_exit) as f64);
            let design = designed(&records, t, &whole_bead(&records, &s.preview.beads.0[k], i, 0.0));
            let bound = ON_LINE + (speed * err) as f32;
            assert!(distance(printing.tip, design) <= bound, "{t:.3} s: the printing nozzle {:?} is where the design puts it, {design:?} (at most {bound} mm off)", printing.tip);
            let ([lx, ly, lz], [rx, ry, rz]) = (l.tip, r.tip);
            assert!((rx - lx - head.offset_x).abs() < 1e-3 && (ry - ly).abs() < 1e-3, "{t:.3} s: the right nozzle {:?} sits {} mm right of the left {:?}", r.tip, head.offset_x, l.tip);
            // The waiting nozzle stands above the printing one, whichever it is.
            let lift = if r.active { head.lift } else { -head.lift };
            assert!(head.lift > 0.0 && (lz - rz - lift).abs() < 1e-3, "{t:.3} s: the waiting nozzle stands {} mm up: left {:?}, right {:?}", head.lift, l.tip, r.tip);
            shown[want] += 1;
        }
        assert!(shown.iter().all(|n| *n > 10), "the view shows both nozzles printing: {shown:?}");
    }

    #[test]
    fn two_parts_on_two_nozzles_slice_and_preview() {
        // Heat-set inserts ride in both cubes: steps after the print, never in it.
        let melt = |into: &str, size: &str| fab_plate::ExcludedPart {
            part: part(&format!("{into} {size}"), 1, None, cube([0.0, 0.0, 0.0], 1.0)), link: None, frame: None,
            insert: Some(fab_plate::Insert { into: into.into(), melt: Some(fab_plate::melt::Melt { size: size.into(), ..Default::default() }), ..Default::default() }),
            render: Default::default(), handles: Default::default(), model_to_plate: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]], paint: None,
        };
        let excluded = vec![melt("a", "M3"), melt("b", "M3"), melt("a", "M3"), melt("a", "M2")];
        let plate = Plate { excluded, settings: Vec::new(), parts: vec![part("a", 1, None, cube([100.0, 100.0, 0.0], 10.0)), part("b", 2, None, cube([200.0, 100.0, 0.0], 10.0))] };
        let machine = Machine::dual_nozzle_350();
        let s = run_slice(0, &settings(2), None, &plate, &machine, "two").unwrap();
        let pv = &s.preview;
        // One step per size and part, the iron at the print temperature of the part's filament.
        let iron = |tool: usize| s.settled.job.fil("nozzle_temperature", s.settled.tools.iter().position(|&t| t == tool).unwrap()).round() as u32;
        let steps: Vec<(&str, u32, &str, u32)> = pv.melts.iter().map(|m| (m.size.as_str(), m.count, m.into.as_str(), m.iron)).collect();
        assert_eq!(steps, [("M3", 2, "a", iron(0)), ("M3", 1, "b", iron(1)), ("M2", 1, "a", iron(0))], "the inserts to melt in after the print");
        assert!(iron(0) > 150 && pv.summary.iter().any(|(k, v)| k == "Melt in after" && v == "2 × M3 into a · 1 × M3 into b · 1 × M2 into a"), "{:?}", pv.summary);
        let figures = pv.printability.plate.as_ref().expect("the sliced figures");
        assert!((figures.time_s-pv.total).abs() < 1e-6);
        assert_eq!(figures.filaments.len(), 2);
        assert_eq!(pv.markers.count(), pv.printability.issues.len()*2);
        // The printer package, made on demand from the settled slice's layer records.
        let package = s.settled.package_with(&|_| {}, &|| false).expect("the package");
        assert_eq!(&package[..2], b"PK");
        let starts = |r: &RetainedInstances| r.data().chunks(PREVIEW_SLOTS).map(fab::print::bead::t0).collect::<Vec<f32>>();
        assert!(pv.beads.count() > 100, "{} beads", pv.beads.count());
        // The surface, lining and core streams, each in time order: merged, they are too.
        assert!(pv.beads.0.iter().all(|r| starts(r).windows(2).all(|w| w[0] <= w[1])), "each stream in time order");
        let merged: Vec<f32> = pv.beads.each_in_time().map(fab::print::bead::t0).collect();
        assert_eq!(merged.len(), pv.beads.count(), "the streams hold every bead");
        assert!(merged.windows(2).all(|w| w[0] <= w[1]), "beads in time order, stream by stream");
        // The cubes are closed in (two walls, skins above and below): their inner walls are the
        // lining and their fills the core.
        let roles = |r: &RetainedInstances| -> Vec<f32> { r.data().chunks(PREVIEW_SLOTS).map(|b| fab::print::bead::role(b) % 32.0).collect() };
        let (lined, cored) = (roles(pv.beads.of(BeadStream::Lining)), roles(pv.beads.of(BeadStream::Core)));
        assert!(!lined.is_empty() && lined.iter().all(|&r| r == 2.0), "the lining holds the cubes' inner walls: roles {:?}", lined.iter().take(8).collect::<Vec<_>>());
        assert!(!cored.is_empty() && cored.iter().all(|&r| [4.0, 5.0, 9.0, 18.0].contains(&r)), "the core holds the cubes' inner fills: roles {:?}", cored.iter().take(8).collect::<Vec<_>>());
        assert!(starts(&pv.travel).windows(2).all(|w| w[0] <= w[1]), "travel in time order");
        let kinds = |r: &RetainedInstances| r.data().chunks(PREVIEW_SLOTS).map(|c| fab::print::bead::kind(c) % kind::MODE_STRIDE).collect::<Vec<f32>>();
        assert!(pv.travel.count() > 0 && kinds(&pv.travel).iter().all(|&k| k == kind::TRAVEL || k == kind::WIPE || k == kind::ZHOP), "moves that print stay out of the travel stream");
        assert!(pv.beads.0.iter().all(|r| kinds(r).iter().all(|&k| k != kind::TRAVEL && k != kind::WIPE && k != kind::ZHOP)), "travel stays out of the printed beads");
        assert!(pv.layers.len() >= 40, "{} layers", pv.layers.len());
        // Real timing: two small parts take minutes, not days.
        assert!(pv.total > 60.0 && pv.total < 4.0 * 3600.0, "print time {} s", pv.total);
        assert!(pv.roles.iter().any(|r| r.name == "Outer wall"));
        // Both nozzles print, and the timeline knows which one is active.
        let noz: Vec<f32> = all_beads(&pv.beads).chunks(PREVIEW_SLOTS).filter(|c| fab::print::bead::kind(c) == kind::EXTRUDE).map(|c| fab::print::bead::tag(c) % 2.0).collect();
        assert!(noz.contains(&0.0) && noz.contains(&1.0));
        let mid = pv.layers[pv.layers.len() / 2];
        let now = pv.timeline.now((mid.t_start + mid.t_end) * 0.5);
        assert!(now.nozzles[0][0] < now.nozzles[1][0], "left nozzle left of the right one");
        plays_the_records(&s, &machine);
        // The tower stays clear of both parts and inside both reaches.
        let at = tower_spot(&plate, &machine, 60.0);
        assert!(at[0] >= 25.0 && at[0] + 60.0 <= 325.0, "{at:?}");
    }

    /// A bead as the design has it ([`whole_bead`]).
    struct Whole {
        /// Where it starts and ends.
        ends: ([f32; 3], [f32; 3]),
        /// When, as its beads have it (s, on the beads' clock).
        times: (f64, f64),
        /// When its first record starts and its last ends (s, on theirs).
        moves: (f64, f64),
    }

    /// The bead `stream`'s `i`th is, as the design has it (`far`: how far the beads' clock is
    /// ahead of the records'): a tip's own bead ([`fab::print::split_tips`]) is a share of the
    /// bead it was cut from, on its line and in its time, so the beads on from it in the stream to
    /// where a record starts as it starts and to where one ends as it ends. The records its times
    /// take in, but for their rounding, start and end there.
    fn whole_bead(records: &[MoveRecord], stream: &RetainedInstances, i: usize, far: f64) -> Whole {
        let at = |k: usize| instance(stream, k);
        // A bead's times on the records' clock, and how far they round at most.
        let span = |b: &[f32]| {
            let (t0, t1) = (fab::print::bead::t0(b) as f64 - far, fab::print::bead::t1(b) as f64 - far);
            (t0, t1, span_rounding(t1 - t0, t1 + far))
        };
        let starts = |b: &[f32]| {
            let (t0, _, err) = span(b);
            records[records.partition_point(|r| r.t_start < t0 - err)..].iter().take_while(|r| r.t_start <= t0 + err).any(|r| r.p0 == ends(b).0)
        };
        let finishes = |b: &[f32]| {
            let (_, t1, err) = span(b);
            records[records.partition_point(|r| r.t_end < t1 - err)..].iter().take_while(|r| r.t_end <= t1 + err).any(|r| r.p1 == ends(b).1)
        };
        let (mut i0, mut i1) = (i, i);
        while i0 > 0 && ends(&at(i0 - 1)).1 == ends(&at(i0)).0 && !starts(&at(i0)) {
            i0 -= 1;
        }
        while i1 + 1 < stream.count() && ends(&at(i1 + 1)).0 == ends(&at(i1)).1 && !finishes(&at(i1)) {
            i1 += 1;
        }
        let (first, last) = (at(i0), at(i1));
        let ((a, _), (_, b)) = (ends(&first), ends(&last));
        let ((t0, _, e0), (_, t1, e1)) = (span(&first), span(&last));
        let (lo, hi) = (records.partition_point(|r| r.t_start < t0 - e0), records.partition_point(|r| r.t_end <= t1 + e1));
        let window = &records[lo..hi.max(lo)];
        let start = window.iter().find(|r| r.p0 == a).unwrap_or_else(|| panic!("the bead from {a:?} starts where a record does"));
        let end = window.iter().rev().find(|r| r.p1 == b).unwrap_or_else(|| panic!("the bead to {b:?} ends where a record does"));
        Whole { ends: (a, b), times: (t0 + far, t1 + far), moves: (start.t_start, end.t_end) }
    }

    /// Where `b` is in `stream` (in time order), if there.
    fn index_of(stream: &RetainedInstances, b: &[f32; PREVIEW_SLOTS]) -> Option<usize> {
        let t0 = fab::print::bead::t0(b);
        (0..fab::print::started_by(stream, t0 as f64)).rev().take_while(|&k| fab::print::bead::t0(&instance(stream, k)) == t0).find(|&k| instance(stream, k) == *b)
    }

    /// Where the T3 design puts the head at `t` inside `bead` ([`whole_bead`]), from the records
    /// alone: an arc that takes several chords, along the chord its time's share is in, at that
    /// share; else along the bead from its first record's start to its last's end, at the share
    /// of their time. How far that is from the records' own point is the design's whole deviation
    /// there.
    fn designed(records: &[MoveRecord], t: f64, bead: &Whole) -> [f32; 3] {
        let r = &records[records.partition_point(|r| r.t_start <= t).saturating_sub(1)];
        if r.flags & flags::ARC != 0 && r.sweep.abs() > 1e-6 && t > r.t_start && t < r.t_end {
            let (n, at) = arc_chords(r);
            if n > 1 {
                let f = (t - r.t_start) / (r.t_end - r.t_start) * n as f64;
                let i = (f as usize).min(n - 1);
                let (fa, fb) = (i as f32 / n as f32, (i + 1) as f32 / n as f32);
                return along(if i == 0 { r.p0 } else { at(fa) }, if i + 1 == n { r.p1 } else { at(fb) }, (f - i as f64) as f32);
            }
        }
        let ((a, b), (t0, t1)) = (bead.ends, bead.moves);
        along(a, b, fraction(t, t0, t1))
    }

    /// A bed point (mm, under 512) is an f32 with steps of 256 ε: the head's point along a bead
    /// (`p0 + (p1 - p0) f`) and a distance to a line each round a few times, four steps each.
    const ON_LINE: f32 = 8.0 * 256.0 * f32::EPSILON;

    /// The step (s) of an f32 clock at `t`: a bead's start is its record's rounded to half of it.
    fn clock_step(t: f64) -> f64 {
        t.abs() * f32::EPSILON as f64
    }

    /// How far (s) a bead's end is off its records' at most: its start's rounding (half a step of
    /// the clock), its f16 duration's (2^-11 of it) and the sum's (half a step).
    fn span_rounding(duration: f64, t: f64) -> f64 {
        duration.abs() * 2f64.powi(-11) + clock_step(t)
    }

    fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
        (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f32>().sqrt()
    }

    /// A builder's playback, as its preview makes it.
    fn played(mut b: PreviewBuilder) -> BeadTimeline {
        b.close_run();
        b.stack_closed(true);
        b.flush();
        let (beads, travel) = b.published.as_mut().expect("flushed").publish();
        let (rows, states) = (std::mem::take(&mut b.rows), std::mem::take(&mut b.states));
        b.playback(beads, travel, rows, states)
    }

    /// How far (mm) `p` is from the segment `a`..`b`.
    fn off_line(p: [f32; 3], a: [f32; 3], b: [f32; 3]) -> f32 {
        let (d, w) = ([0, 1, 2].map(|i| b[i] - a[i]), [0, 1, 2].map(|i| p[i] - a[i]));
        let dd: f32 = d.iter().map(|x| x * x).sum();
        let f = if dd > 0.0 { (w.iter().zip(&d).map(|(x, y)| x * y).sum::<f32>() / dd).clamp(0.0, 1.0) } else { 0.0 };
        (0..3).map(|i| (w[i] - d[i] * f).powi(2)).sum::<f32>().sqrt()
    }

    fn ends(b: &[f32]) -> ([f32; 3], [f32; 3]) {
        ([b[0], b[1], b[2]], [b[4], b[5], b[6]])
    }

    fn instance(stream: &RetainedInstances, i: usize) -> [f32; PREVIEW_SLOTS] {
        stream.data_slices(i * PREVIEW_SLOTS..(i + 1) * PREVIEW_SLOTS).next().expect("an instance").1.try_into().expect("a whole instance")
    }

    /// The last few beads started in each of the cut's streams (the stream, the bead in it),
    /// where the beads started and not ended are.
    fn cut_tails(cut: &fab::print::RunCut) -> impl DoubleEndedIterator<Item = (usize, usize)> {
        cut.ranges().into_iter().enumerate().flat_map(|(k, r)| (r.end.saturating_sub(8)..r.end).map(move |i| (k, i as usize)))
    }

    /// The beads of every stream, one stream after another.
    fn all_beads(beads: &BeadStreams) -> Vec<f32> {
        beads.each().flatten().copied().collect()
    }

    /// The same beads in each stream.
    pub(super) fn same_beads(a: &BeadStreams, b: &BeadStreams) -> bool {
        a.0.iter().zip(&b.0).all(|(a, b)| a.data() == b.data())
    }

    /// The beads the shader cuts at `t` (started by then, not yet ended): of the timeline's beads
    /// and of its travel.
    fn cut_at(tl: &BeadTimeline, t: f64) -> Vec<[f32; PREVIEW_SLOTS]> {
        let n = fab::print::started_by(&tl.travel, t);
        let beads = cut_tails(&tl.beads.cut_at(t)).map(|(k, i)| instance(&tl.beads.0[k], i));
        beads.chain((n.saturating_sub(8)..n).map(|i| instance(&tl.travel, i))).filter(|b| fab::print::bead::t1(b) as f64 > t).collect()
    }

    /// The playback of a slice against the records it was made of (the settled slice's):
    /// - fed whole, by its layers (as its preview was) or seven records at a time: the same beads
    ///   and the same head at every record's start and middle;
    /// - something plays at every moment of the print, or the last thing holds at its end, only
    ///   where the beads' times round (f32 starts, f16 durations);
    /// - in the middle of every record the head holds what the record says and does what it does;
    /// - the bead the shader cuts at the playhead is the one the head is on;
    /// - in the middle and at the end of every record the head is where the design puts it
    ///   (`designed`), but for the rounding of the beads' times;
    /// - twenty hours into a print the same, and the head never goes back.
    /// It prints how far the design is from the playback before beads (the records' points), and
    /// the head from the design.
    fn plays_the_records(s: &Sliced, machine: &Machine) {
        use fab::print::bead;
        let records: Vec<MoveRecord> = s.settled.layers.records().collect();
        let stats = &s.settled.stats;
        let fed = |n: usize, t0: f64| {
            let mut b = PreviewBuilder::new(machine);
            b.temps = s.settled.layers.nozzle_temps.clone();
            for part in records.chunks(n) {
                b.place(t0, [0.0; 2], None, part.iter().copied());
            }
            b
        };
        let head = |n: &PrintNow| (n.nozzles, n.active, n.head, n.doing.clone(), n.filament, n.nozzle_temps.clone(), n.bed_temp, n.fan, n.speed);
        for pv in [fed(records.len(), 0.0).preview(None, Default::default(), stats), fed(7, 0.0).preview(None, Default::default(), stats)] {
            assert!(same_beads(&pv.beads, &s.preview.beads) && pv.travel.data() == s.preview.travel.data(), "the same beads however the records come");
            for m in &records {
                for t in [m.t_start, (m.t_start + m.t_end) * 0.5] {
                    assert_eq!(head(&pv.timeline.now(t)), head(&s.preview.timeline.now(t)), "the same head at {t} s");
                }
            }
        }
        let total = s.preview.total;
        let temps = &s.settled.layers.nozzle_temps;
        // The playback before beads, the reference: the record that starts last by `t` (the
        // first when none does), where it is then.
        let record_now = |t: f64| {
            let r = &records[records.partition_point(|r| r.t_start <= t).saturating_sub(1)];
            (r, if t >= r.t_end { r.p1 } else if t <= r.t_start { r.p0 } else { r.position_at(t) })
        };
        let mut report = Vec::new();
        for far in [0.0, 20.0 * 3600.0] {
            let tl = played(fed(7, far));
            let mut spans: Vec<(f64, f64)> = tl.beads.each().chain(fab::print::each_instance(&tl.travel))
                .map(|b| (bead::t0(b) as f64, bead::t1(b) as f64)).chain(tl.rows.iter().map(|r| (r.t0, r.t1))).collect();
            spans.sort_by(|a, b| a.0.total_cmp(&b.0));
            let (mut reach, mut slack, mut holds) = (far, clock_step(far), 0);
            for &(a, b) in &spans {
                if a > reach {
                    assert!(a - reach <= slack + clock_step(a) / 2.0, "{far} s on: nothing plays from {reach} to {a} s");
                    let t = (a + reach) * 0.5;
                    assert!(tl.playing(t).is_some_and(|p| p.span().1 <= t), "{far} s on: {t} s holds the last move at its end");
                    holds += 1;
                }
                if b > reach {
                    (reach, slack) = (b, span_rounding(b - a, b));
                }
            }
            assert!((reach - far - total).abs() <= slack, "{far} s on: the playback ends at {reach} s, the print at {} s", far + total);
            let (mut worst_design, mut worst_beyond, mut worst_cut) = (0.0f32, 0.0f32, 0.0f32);
            for m in records.iter().filter(|m| m.t_end > m.t_start) {
                // In the middle of a record: what it holds, exactly (the holds' times are the
                // records'); what it does, where its middle is past the rounding of the starts
                // around it (half a step of the clock each side).
                let t = far + (m.t_start + m.t_end) * 0.5;
                let now = tl.now(t);
                let want = (m.nozzle as usize, m.filament, (0..machine.nozzles.len().max(2)).map(|n| temps[m.temps as usize].get(n).copied().unwrap_or(0)).collect::<Vec<_>>(), m.t_bed, m.fan_part);
                assert_eq!((now.head, now.filament, now.nozzle_temps.clone(), now.bed_temp, now.fan), want, "{t} s: the head holds what the record says");
                if m.t_end - m.t_start > clock_step(far + m.t_end) {
                    assert_eq!(now.doing, doing(m.kind, m.role, m.flags), "{t} s: the head does what the record does");
                }
                // The shader cuts the beads started and not ended at the playhead to the head: the
                // one the head is on, exactly on its line; one whose end rounds past the next's
                // start, by as far as the head goes in that rounding.
                let tip = now.nozzles[now.active];
                let playing = tl.playing(t).expect("something plays");
                let pace = |b: &[f32]| { let (a, e) = ends(b); distance(a, e) as f64 / (bead::t1(b) - bead::t0(b)).max(f32::MIN_POSITIVE) as f64 };
                for c in cut_at(&tl, t) {
                    let (a, e) = ends(&c);
                    let off = off_line(tip, a, e);
                    let bound = match &playing {
                        Playing::Bead(b) if *b == c => ON_LINE,
                        Playing::Bead(b) => ON_LINE + (pace(b) * (span_rounding((bead::t1(&c) - bead::t0(&c)) as f64, t) + clock_step(t) / 2.0)) as f32,
                        // A wait or seam mark the bead went over: on its path.
                        Playing::Row(_) => ON_LINE + CHORD_SAGITTA,
                    };
                    assert!(off <= bound, "{t} s: the shader cuts a bead {off} mm off the head (at most {bound})");
                    worst_cut = worst_cut.max(off);
                }
                if let Playing::Bead(b) = &playing {
                    assert!(bead::t1(b) as f64 <= t || cut_at(&tl, t).into_iter().any(|c| c == *b), "{t} s: the shader cuts the bead the head is on");
                }
                // Where the head is, in the middle and at the end of the record: where the design
                // puts it (`designed`; at a bead's or row's ends the records' point), but for as far
                // as it goes in the rounding of the bead's times. How far the design is from the
                // playback before beads is reported, not bounded.
                for t in [t, far + m.t_end] {
                    let (r, want) = record_now(t - far);
                    let now = tl.now(t);
                    let (design, slip) = match tl.playing(t).expect("something plays") {
                        Playing::Bead(b) => {
                            let (t0, t1) = (bead::t0(&b) as f64, bead::t1(&b) as f64);
                            let err = span_rounding(t1 - t0, t);
                            let speed = pace(&b).max(r.v_cruise.max(r.v_entry).max(r.v_exit) as f64);
                            let arc = r.flags & flags::ARC != 0 && t - far > r.t_start && t - far < r.t_end;
                            let (stream, i) = tl.beads.0.iter().chain([&tl.travel]).find_map(|s| index_of(s, &b).map(|i| (s, i))).expect("the bead plays from its stream");
                            let whole = whole_bead(&records, stream, i, far);
                            let (t0, t1) = whole.times;
                            (if arc || (t - t0 > err && t1 - t > err) { designed(&records, t - far, &whole) } else { want }, speed * err)
                        }
                        // Its times are the records': but for a bead's start next to it (half a step).
                        Playing::Row(row) => {
                            (if t > row.t0 && t < row.t1 { along(row.p0, row.p1, fraction(t, row.t0, row.t1)) } else { want }, r.v_cruise.max(r.v_entry).max(r.v_exit) as f64 * clock_step(t) / 2.0)
                        }
                    };
                    let (off, bound) = (distance(now.nozzles[now.active], design), ON_LINE + slip as f32);
                    assert!(off <= bound, "{t} s: the head is {off} mm from where the design puts it (at most {bound}): {:?} at {design:?}, record {:?} {:?} {}..{} {:?} -> {:?}", now.nozzles[now.active], r.kind, r.role, r.t_start, r.t_end, r.p0, r.p1);
                    (worst_design, worst_beyond) = (worst_design.max(distance(design, want)), worst_beyond.max(off));
                }
            }
            // Never back: the latest thing to start plays on (both starts rounded, half a step of
            // the clock each), a bead on after a row inside it.
            let mut last: Option<((f64, f64), bool)> = None;
            for i in 0..=20000 {
                let t = far + total * i as f64 / 20000.0;
                let p = tl.playing(t).expect("something plays");
                let now = (p.span(), matches!(p, Playing::Row(_)));
                if let Some(((s0, e0), row)) = last {
                    let ((s1, e1), _) = now;
                    assert!(s1 + clock_step(s0) >= s0 || (row && !now.1 && e1 >= e0), "{t} s: the head goes back from {:?} to {:?}", (s0, e0), (s1, e1));
                }
                last = Some(now);
            }
            report.push(format!("{far} s on: {} spans, {holds} holds; the design off the records' point up to {worst_design} mm, the head off the design up to {worst_beyond} mm; cut beads off the head {worst_cut} mm", spans.len()));
        }
        eprintln!("fab playback parity: {}", report.join("; "));
    }

    /// More than two heads: the head printing is named by number, with every head's target.
    /// And the order things play in: a nozzle change, a bead on the fourth head with a wait inside
    /// it (the wait plays, then the bead again), a retract, a Z hop (drawn as travel), the end held.
    #[test]
    fn playback_names_one_head_of_many() {
        let rec = |kind: MoveKind, t: [f64; 2], p0: [f32; 3], p1: [f32; 3], nozzle: u8, temps: u32| MoveRecord {
            t_start: t[0], t_end: t[1], kind, role: Role::OuterWall, p0, p1, tool: nozzle, nozzle, filament: nozzle + 1, width: 0.45, height: 0.2,
            e: if kind == MoveKind::Extrude { 0.4 } else { 0.0 }, f_cmd: 100.0, temps, t_bed: 60, fan_part: 50, z_layer: 0.2, object: u16::MAX,
            flags: if kind == MoveKind::FirmwareMacro { flags::NOZZLE_CHANGE } else { 0 }, ..Default::default()
        };
        let records = [
            rec(MoveKind::Travel, [0.0, 1.0], [0.0, 0.0, 0.2], [10.0, 0.0, 0.2], 0, 0),
            rec(MoveKind::FirmwareMacro, [1.0, 5.0], [10.0, 0.0, 0.2], [10.0, 0.0, 0.2], 3, 1),
            rec(MoveKind::Extrude, [5.0, 6.0], [10.0, 0.0, 0.2], [20.0, 0.0, 0.2], 3, 1),
            rec(MoveKind::Dwell, [6.0, 8.0], [20.0, 0.0, 0.2], [20.0, 0.0, 0.2], 3, 1),
            rec(MoveKind::Extrude, [8.0, 9.0], [20.0, 0.0, 0.2], [30.0, 0.0, 0.2], 3, 1),
            rec(MoveKind::Retract, [9.0, 9.5], [30.0, 0.0, 0.2], [30.0, 0.0, 0.2], 3, 1),
            rec(MoveKind::ZHop, [9.5, 10.0], [30.0, 0.0, 0.2], [30.0, 0.0, 0.6], 3, 1),
        ];
        let mut b = PreviewBuilder::with_head(Toolhead { offset_x: 0.0, lift: 0.0 }, true, 4);
        b.temps = Arc::new(vec![vec![220, 0, 0, 0], vec![0, 0, 0, 230]]);
        b.feed(&records);
        let tl = played(b);
        assert_eq!((tl.beads.count(), tl.travel.count()), (1, 2), "one bead over the wait; the travel and the Z hop");
        assert_eq!(fab::print::each_instance(&tl.travel).map(|b| fab::print::bead::kind(b)).collect::<Vec<_>>(), [kind::TRAVEL, kind::ZHOP]);
        let at = |t: f64| {
            let n = tl.now(t);
            (n.doing_text(), n.nozzle_temps.clone(), n.head, n.nozzles[n.active])
        };
        assert_eq!(at(0.5), ("Travel on Tool 1".to_string(), vec![220, 0, 0, 0], 0, [5.0, 0.0, 0.2]));
        assert_eq!(at(3.0), ("Nozzle change on Tool 4".to_string(), vec![0, 0, 0, 230], 3, [10.0, 0.0, 0.2]));
        assert_eq!(at(7.0).0, "Wait on Tool 4");
        assert_eq!(at(7.0).3, [20.0, 0.0, 0.2], "the wait plays where it waits");
        let (doing, _, head, [x, y, z]) = at(8.5);
        assert!(doing == "Outer wall on Tool 4" && head == 3 && x > 20.0 && x < 30.0 && y == 0.0 && z == 0.2, "the bead plays on after the wait: {doing} at {x} {y} {z}");
        assert_eq!(at(9.25).0, "Retract on Tool 4");
        let [x, y, z] = at(9.75).3;
        // z under 1 mm: f32 steps of ε at most, and 0.2 + (0.6 - 0.2) / 2 rounds four times.
        assert!(x == 30.0 && y == 0.0 && (z - 0.4).abs() <= 4.0 * f32::EPSILON, "half way up the Z hop: {z}");
        assert_eq!(at(9.75).0, "Z hop on Tool 4");
        assert_eq!(at(20.0).3, [30.0, 0.0, 0.6], "the end holds");
        assert_eq!(tl.now(-1.0).nozzle_temps, Vec::<i16>::new(), "nothing before the print");
    }

    #[test]
    fn print_eye_removes_and_restores_toolpaths_layers_and_preview_beads() {
        use fab_model::{Change, FabModel, PlaceArg, eval::Evaluator};
        let source = format!("{}let a = m.box({{size: [10mm 8mm 10mm]}})\nlet b = m.box({{size: [10mm 4mm 10mm]}})\nobject(a)\nobject(b)\n", fab_model::program::EMPTY);
        let mut model = FabModel::with_script("eyes", "eyes.cad.splash", &source);
        let machine = Machine::dual_nozzle_350();
        let mut evaluator = Evaluator::new(machine.clone());
        let place = |object: &str, arg| Change::Place { instance: "eyes".into(), object: object.into(), arg };
        model.apply(&mut evaluator, "positions", model.head(), &[
            place("a", PlaceArg::At(Some([100.0, 160.0]))),
            place("b", PlaceArg::At(Some([240.0, 160.0]))),
        ], None).unwrap();
        let mut full_layers = 0;
        let mut old_id = None;
        let mut shown = None;
        for (request, printed) in [("initial", true), ("off", false), ("on", true)] {
            if request != "initial" {
                let (c, ev) = model.evaluate(&mut evaluator, None).unwrap();
                let action = fab::api::PartAction::SetPrint { objects: vec!["eyes/a".into()], print: printed };
                let changes = crate::plate_edit::edits(&c, &ev, &action).unwrap();
                model.apply(&mut evaluator, request, model.head(), &changes, None).unwrap();
            }
            let (_, ev) = model.evaluate(&mut evaluator, None).unwrap();
            let id = crate::live_slice::plate_id(&ev);
            assert_ne!(old_id, Some(id), "a print toggle invalidates the live slice");
            old_id = Some(id);
            let setup = resolve_setup(&settings(1), None, &ev.plate, &machine).unwrap();
            let result = makepad_fab_slice::plate::slice_plate(&setup, &ev.plate).unwrap();
            let expected = if printed { 2 } else { 1 };
            assert_eq!(result.objects.len(), expected);
            assert_eq!(result.toolpaths.objects.len(), expected);
            assert!(result.layers().iter().flat_map(|l| &l.objects).all(|o| o.object < expected));
            let records: Vec<_> = result.timeline.moves().cloned().collect();
            let preview = build_preview(&records, &result.timeline, vec![[0.8, 0.8, 0.8, 1.0]], &machine, &result.stats);
            shown = Some(preview.clone());
            let beads = all_beads(&preview.beads);
            let extrusion: Vec<_> = beads.chunks(PREVIEW_SLOTS).filter(|b| fab::print::bead::kind(b) == kind::EXTRUDE && fab::print::bead::tag(b) >= 2.0).collect();
            assert!(!extrusion.is_empty());
            if printed {
                assert!(extrusion.iter().any(|b| b[0] < 150.0));
                assert!(extrusion.iter().any(|b| b[0] > 200.0));
                if full_layers == 0 { full_layers = result.layers().len(); }
                assert_eq!(result.layers().len(), full_layers);
            } else {
                assert!(result.layers().len() < full_layers);
                assert!(result.timeline.moves().filter(|m| m.kind == MoveKind::Extrude && m.object != u16::MAX).all(|m| m.p0[0] > 200.0 && m.object == 0));
                assert!(extrusion.iter().all(|b| b[0] > 200.0 && (fab::print::bead::tag(b) / 2.0).floor() == 1.0));
                assert!(preview.layers.iter().all(|l| l.z <= 4.001));
            }
        }
        // Excluding the last placement also clears the prior printable preview.
        let (_, update) = model.apply(&mut evaluator, "all-off", model.head(), &[
            place("a", PlaceArg::Print(false)), place("b", PlaceArg::Print(false)),
        ], None).unwrap();
        let (_, ev) = update.unwrap();
        let mut state = PrintState::default();
        state.preview = shown.clone();
        state.compare = shown.clone();
        state.last_settled = shown;
        let mut printing = Printing::default();
        printing.plate_changed(&mut state, &ev);
        assert!(state.preview.is_none() && state.compare.is_none() && state.last_settled.is_none());
        assert!(state.live_contours.is_none() && !state.stale);
        assert_eq!(ev.plate.excluded.len(), 2);
    }

    #[test]
    fn a_part_on_a_nozzle_its_filament_is_not_on_is_an_error() {
        // Material 2 on the left nozzle while part b was mapped to print on the right.
        let plate = Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part("a", 1, None, cube([100.0, 100.0, 0.0], 10.0)), part("b", 2, Some(1), cube([200.0, 100.0, 0.0], 10.0))] };
        let mut st = settings(2);
        st.materials[1].nozzle = 0;
        let e = run_slice(0, &st, None, &plate, &Machine::dual_nozzle_350(), "x").err().unwrap();
        assert!(e.contains("must print on nozzle 2"), "{e}");
    }

    /// Installed profiles whose selection names no plate: the plate is the printer's own
    /// default and slices; while nobody picks one it follows the materials (Textured PEI, else
    /// the first that suits them all), it is empty only when none fits, and a plate picked by
    /// hand stays.
    #[test]
    fn an_unpicked_plate_defaults_to_one_every_material_suits() {
        use makepad_fab_settings::import::preset_json;
        use makepad_fab_settings::profiles::{RawPreset, VENDOR_APP_DIR};
        let dir = std::env::temp_dir().join(format!("fab-plate-default-test-{}", std::process::id()));
        let sys = dir.join("system").join("Test");
        let base = defaults_setup(&defaults_settings(), &Machine::dual_nozzle_350(), 1);
        let write = |kind: &str, name: &str, values: Config| {
            let folder = sys.join(kind);
            std::fs::create_dir_all(&folder).unwrap();
            let raw = RawPreset { kind: kind.into(), name: name.into(), from: "system".into(), values, ..Default::default() };
            std::fs::write(folder.join(format!("{name}.json")), preset_json(&raw)).unwrap();
        };
        let mut printer = base.machine.values.clone();
        printer.set_str("printer_model", "Bambu Lab H2D");
        write("machine", "Test printer 0.4", printer);
        // The printer's model (not a selectable preset) records its default plate.
        std::fs::write(sys.join("machine").join("model.json"), r#"{"type":"machine_model","name":"Bambu Lab H2D","instantiation":"false","default_bed_type":"Engineering Plate"}"#).unwrap();
        write("process", "Test 0.20", base.process.values.clone());
        let suits: [(&str, fn(&str) -> bool); 3] = [
            ("Test PLA", |_| true),
            ("Test PETG", |p| p != "Engineering Plate" && p != "High Temp Plate"),
            ("Test HT", |p| p == "High Temp Plate"),
        ];
        for (name, suits) in suits {
            let mut values = base.filaments[0].values.clone();
            for (plate, key) in PLATES {
                let t = if suits(plate) { "55" } else { "0" };
                values.set_vec(key, vec![t.into()]);
                values.set_vec(&format!("{key}_initial_layer"), vec![t.into()]);
            }
            write("filament", name, values);
        }
        std::fs::write(dir.join(format!("{VENDOR_APP_DIR}.conf")), r#"{"presets":{"machine":"Test printer 0.4","process":"Test 0.20","filaments":["Test PLA"]}}"#).unwrap();
        let cat = Arc::new(catalog_in(std::slice::from_ref(&dir), &dir.join("imports"), &[]).expect("the test profiles"));
        assert_eq!(cat.selection.as_ref().map(|s| s.plate.as_str()), Some(""), "the selection names no plate");
        let (mut printing, mut state) = (Printing::default(), PrintState::default());
        printing.event(&mut state, &PrintEvent::Catalog(Some(cat.clone())));
        let with = |printing: &Printing, state: &mut PrintState, presets: &[&str]| {
            state.settings.materials = presets.iter().enumerate().map(|(i, p)| JobMaterial { preset: p.to_string(), color: [0.9, 0.9, 0.9, 1.0], nozzle: i % 2, tray: None, dedicated: false }).collect();
            printing.refresh_plates(state);
            state.settings.plate.clone()
        };
        let machine = Machine::dual_nozzle_350();
        let plate = Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part("a", 1, None, cube([100.0, 100.0, 0.0], 4.0))] };
        assert_eq!(with(&printing, &mut state, &["Test PLA"]), "Engineering Plate", "the printer's own default plate");
        let sliced = run_slice(0, &state.settings, Some(&cat), &plate, &machine, "default plate").expect("the default plate slices");
        assert!(!sliced.preview.layers.is_empty());
        assert_eq!(with(&printing, &mut state, &["Test PLA", "Test PETG"]), "Textured PEI Plate", "Textured PEI when the printer's default does not suit a material");
        assert_eq!(with(&printing, &mut state, &["Test HT"]), "High Temp Plate", "else the first plate every material suits");
        assert_eq!(with(&printing, &mut state, &["Test PETG", "Test HT"]), "", "no plate suits both");
        assert_eq!(resolve_setup(&state.settings, Some(&*cat), &plate, &machine).err().as_deref(), Some(NO_PLATE_FITS), "nothing slices without a plate");
        printing.action(&mut state, &PrintAction::SetPlate("Cool Plate".into()), None);
        assert_eq!(with(&printing, &mut state, &["Test PETG", "Test HT"]), "Cool Plate", "a plate picked by hand stays");
        let e = resolve_setup(&state.settings, Some(&*cat), &plate, &machine).err().unwrap();
        assert!(e.contains("no bed temperature for the Cool Plate"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unheated_filaments_suit_catalogue_plates_and_slice_setup() {
        use makepad_fab_settings::{family, import::{import_into, preset_json}, profiles::RawPreset};
        let dir = std::env::temp_dir().join(format!("fab-unheated-{}", std::process::id()));
        let imports = dir.join("imports");
        let machine = Machine::dual_nozzle_350();
        let base = defaults_setup(&defaults_settings(), &machine, 1);
        let write = |kind: &str, name: &str, values: Config| {
            let folder = imports.join(kind);
            std::fs::create_dir_all(&folder).unwrap();
            let raw = RawPreset { kind: kind.into(), name: name.into(), values, instantiation: true, ..Default::default() };
            std::fs::write(folder.join(format!("{name}.json")), preset_json(&raw)).unwrap();
        };
        let mut printer = base.machine.values.clone();
        printer.set_str("printer_model", family::DEFAULT.models[0]);
        write("machine", "Printer", printer);
        write("process", "Process", base.process.values.clone());
        let mut fil = base.filaments[0].values.clone();
        for (_, key) in PLATES {
            fil.set_str(key, "0");
            fil.set_str(&format!("{key}_initial_layer"), "0");
        }
        write("filament", "No heat setting", fil.clone());
        fil.set_str("fab_bed_unheated", "1");
        write("filament", "Unheated", fil);
        let cat = catalog_in(&[], &imports, &[]).unwrap();
        assert!(cat.entries.iter().find(|e| e.name == "No heat setting").unwrap().plates.is_empty());
        assert_eq!(cat.entries.iter().find(|e| e.name == "Unheated").unwrap().plates.len(), PLATES.len());
        let mut settings = defaults_settings();
        settings.use_installed = true;
        settings.machine = "Printer".into();
        settings.process = "Process".into();
        settings.materials = vec![JobMaterial { preset: "Unheated".into(), color: [1.0; 4], nozzle: 0, tray: None, dedicated: false }];
        let plate = Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part("box", 1, None, cube([100.0, 100.0, 0.0], 1.0))] };
        for (name, _) in PLATES {
            settings.plate = (*name).into();
            settings.materials[0].preset = "Unheated".into();
            let setup = resolve_setup(&settings, Some(&cat), &plate, &machine).expect("unheated filament suits every plate");
            assert!(setup.filaments[0].values.bool("fab_bed_unheated"));
            settings.materials[0].preset = "No heat setting".into();
            assert!(resolve_setup(&settings, Some(&cat), &plate, &machine).err().unwrap().contains("no bed temperature"));
        }
        // Imported printer presets go through the same family refusal at slice setup.
        std::fs::create_dir_all(dir.join("printer")).unwrap();
        let input = dir.join("printer/Imported.ini");
        settings.materials[0].preset = "Unheated".into();
        for (config, reason) in [("printer_model = X\nnozzle_diameter = 0.4\n", family::UNSUPPORTED), ("nozzle_diameter = 0.4\n", family::NO_MODEL)] {
            std::fs::write(&input, config).unwrap();
            let b = import_into(&input, &cat.store, &imports);
            assert!(b.errors.is_empty(), "{b:?}");
            settings.machine = b.presets[0].preset.name.clone();
            let updated = catalog_in(&[], &imports, &[]).unwrap();
            assert_eq!(resolve_setup(&settings, Some(&updated), &plate, &machine).err().as_deref(), Some(reason));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The end-to-end test's plate, built by the model engine: a rounded box
    /// and a cylinder, sliced with Fab defaults.
    #[test]
    fn a_modelled_plate_slices_with_fab_defaults() {
        let mut session = fab_ai::Session::new(Machine::dual_nozzle_350(), std::env::temp_dir().join("fab-print-test"));
        session.rebuild().unwrap();
        let tower = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet tower = m.box({size: [24mm 30mm 24mm] radius: 2mm})\nobject(tower)\n";
        let post = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet post = m.cylinder({radius: 10mm height: 22mm segments: 64})\nobject(post)\n";
        let q = |t: &str| fab_ai::out::quote(t);
        let args = makepad_model::json::parse(format!("{{\"request_id\":\"tower\",\"expected\":{},\"script\":\"tower.cad.splash\",\"source\":{}}}", q(&session.model.head().text()), q(tower)).as_bytes()).unwrap();
        let r = session.call("script", &args, None);
        assert!(r.ok, "{}", r.text);
        let args = makepad_model::json::parse(format!("{{\"request_id\":\"post\",\"expected\":{},\"script\":\"post.cad.splash\",\"source\":{}}}", q(&session.model.head().text()), q(post)).as_bytes()).unwrap();
        let r = session.call("script", &args, None);
        assert!(r.ok, "{}", r.text);
        let edits = "place(\"tower\", at: [140mm, 150mm])\nplace(\"post\", at: [210mm, 150mm])\n";
        let args = makepad_model::json::parse(format!("{{\"request_id\":\"place\",\"expected\":{},\"edits\":{}}}", q(&session.model.head().text()), q(edits)).as_bytes()).unwrap();
        let r = session.call("place", &args, None);
        assert!(r.ok, "{}", r.text);
        let ev = session.last.clone().unwrap();
        assert_eq!(ev.plate.parts.len(), 2);
        let plate = ev.plate.printable();
        let t0 = std::time::Instant::now();
        let total = run_slice(0, &settings(2), None, &plate, &Machine::dual_nozzle_350(), "t").map(|s| s.preview.total).unwrap();
        println!("sliced in {:.2} s, print {total:.0} s", t0.elapsed().as_secs_f64());

        assert!(total > 60.0 && total < 4.0 * 3600.0);
    }

    /// A material that asks for TPU, with no profiles installed, prints as Fab's own TPU on its
    /// nozzle: its type and temperature, every bead within its volumetric limit (the PLA beside
    /// it runs far faster), its short retraction.
    #[test]
    fn a_tpu_material_prints_as_tpu() {
        let machine = Machine::dual_nozzle_350();
        let mut s = settings(2);
        s.materials[0].preset = default_filament(None).into();
        s.materials[1].preset = default_filament(Some("TPU")).into();
        assert_eq!(s.materials[1].preset, DEFAULT_FLEX_FILAMENT);
        let plate = Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part("rigid", 1, Some(0), cube([120.0, 150.0, 0.0], 12.0)), part("soft", 2, Some(1), cube([200.0, 150.0, 0.0], 12.0))] };
        let job = makepad_fab_settings::machine::Job::new(resolve_setup(&s, None, &plate, &machine).unwrap().build_config());
        assert_eq!((job.fil_str("filament_type", 0), job.fil_str("filament_type", 1)), ("PLA", "TPU"));
        let limit = job.fil("filament_max_volumetric_speed", 1);
        assert!(limit > 0.0 && limit < job.fil("filament_max_volumetric_speed", 0));
        assert!(job.eff("retraction_length", 1) < job.eff("retraction_length", 0), "TPU pulls back less");
        let sliced = run_slice(0, &s, None, &plate, &machine, "tpu").unwrap();
        let records: Vec<MoveRecord> = sliced.settled.layers.records().collect();
        // The plastic a bead pushes through the nozzle per second.
        let area = job.filament_area(1);
        let rate = |m: &MoveRecord| m.e as f64 * area / (m.t_end - m.t_start).max(1e-9);
        let beads = |f: u8| records.iter().filter(move |m| m.kind == MoveKind::Extrude && m.filament == f && m.e > 0.0 && m.t_end > m.t_start);
        let soft_peak = beads(2).map(rate).fold(0.0, f64::max);
        let rigid_peak = beads(1).map(rate).fold(0.0, f64::max);
        let mut on: std::collections::BTreeMap<(u8, u8), usize> = Default::default();
        for m in records.iter().filter(|m| m.kind == MoveKind::Extrude) {
            *on.entry((m.filament, m.nozzle)).or_default() += 1;
        }
        assert!(beads(2).count() > 100 && beads(2).all(|m| m.nozzle == 1), "the TPU prints on its nozzle: (filament, nozzle) beads {on:?}");
        assert!(soft_peak <= limit * 1.05, "TPU beads within {limit} mm3/s: {soft_peak:.2}");
        assert!(rigid_peak > 2.0 * limit, "PLA is not held to TPU's limit: {rigid_peak:.2}");
        let temps = &sliced.settled.layers.nozzle_temps;
        let hot = records.iter().filter(|m| m.filament == 2 && m.kind == MoveKind::Extrude).map(|m| temps[m.temps as usize][1]).max().unwrap_or(0);
        assert_eq!(hot, job.fil("nozzle_temperature", 1).round() as i16, "the TPU nozzle at TPU's temperature");
    }

    /// A parameter that drops a colour from a part's paint: the colours map again onto the ones
    /// it now prints, and the part and each paint colour print on their colour's tool, one of
    /// the job's materials, in the app's order (a remap, then the plate the model worker hands
    /// back, which maps the same).
    #[test]
    fn a_param_that_drops_a_colour_prints_the_rest_on_their_tools() {
        let mut session = fab_ai::Session::new(Machine::dual_nozzle_350(), std::env::temp_dir().join(format!("fab-remap-test-{}", std::process::id())));
        session.rebuild().unwrap();
        let src = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet colours = param(3, min: 2, max: 3, step: 1)\nlet gold = Material{albedo: [0.9 0.7 0.1] name: \"Gold\"}\nlet ember = Material{albedo: [0.7 0.1 0.1] name: \"Ember\"}\nlet body = m.box({size: [40mm 10mm 20mm]})\nif colours >= 3 { m.fill(m.box({size: [8mm 12mm 22mm] at: [-12mm, 0, 0]}), gold) }\nm.fill(m.box({size: [8mm 12mm 22mm] at: [12mm, 0, 0]}), ember)\nobject(body)\n";
        let q = |t: &str| fab_ai::out::quote(t);
        let call = |session: &mut fab_ai::Session, tool: &str, id: &str, rest: String| {
            let args = makepad_model::json::parse(format!("{{\"request_id\":\"{id}\",\"expected\":{},{rest}}}", q(&session.model.head().text())).as_bytes()).unwrap();
            let r = session.call(tool, &args, None);
            assert!(r.ok, "{tool}: {}", r.text);
        };
        call(&mut session, "script", "script", format!("\"script\":\"swatch.cad.splash\",\"source\":{}", q(src)));
        let colours = "color(\"Jade\", rgb: #x13704a)\ncolor(\"Gold\", rgb: #xe0ad2a)\ncolor(\"Ember\", rgb: #xb81c14, material: \"TPU\")\nplace(\"swatch\", at: [150mm, 150mm], color: \"Jade\")\n";
        call(&mut session, "place", "colours", format!("\"edits\":{}", q(colours)));
        let (mut printing, mut state) = (Printing::default(), PrintState::default());
        let mut remap = |session: &mut fab_ai::Session| {
            let ev = session.last.clone().unwrap();
            if let Some(mapped) = printing.refresh_printer(&mut state, Some(&ev)) {
                session.map_colours(mapped);
            }
            let ev = session.last.clone().unwrap();
            assert!(printing.refresh_printer(&mut state, Some(&ev)).is_none(), "the plate handed back maps as it is");
            let n = state.settings.materials.len() as u32;
            let mut on = Vec::new();
            for p in &ev.plate.parts {
                on.push(("part".to_string(), p.filament));
                for c in p.paint.iter().flat_map(|sp| sp.colors.iter()) {
                    if let Some(name) = &c.color {
                        assert_eq!(c.filament, state.colors.tool_of(name).map(|t| t.0), "{name} prints on its tool");
                        on.push((name.clone(), c.filament.unwrap_or(0)));
                    }
                }
            }
            assert!(on.iter().all(|(_, t)| (1..=n).contains(t)), "every colour on one of the job's {n} materials: {on:?}");
            on
        };
        let three = remap(&mut session);
        assert_eq!(three.len(), 3, "{three:?}");
        call(&mut session, "place", "two", format!("\"edits\":{}", q("params(\"swatch\", {colours: 2})")));
        let two = remap(&mut session);
        assert_eq!(two.len(), 2, "{two:?}");
        assert_eq!(state.settings.materials.len(), 2);
    }

    /// Blocks that meet round a TPU spine: the plate builds, its bends can't be measured, and
    /// Print refuses it. Before the measure is in a commit waits for it; after, Start print
    /// refuses at once, with the failure's text.
    #[test]
    fn print_refuses_a_bond_whose_bends_cant_be_measured() {
        let script = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet spine = m.cylinder({radius: 1.5mm height: 50mm at: [25mm 2.5mm 0] rotate: [0, 0, 90°]})\nlet segs = for i in 0..3 { m.box({size: [10mm 5mm 8mm] at: [i * 8mm + 9mm 2.5mm 0]}) }\nlet back = m.bond(spine, segs)\nm.fill(spine, Material{albedo: [0.1 0.1 0.12] name: \"Spine\"})\n";
        let project = "use mod.fab.*\nscript(\"row.cad.splash\")\ncolor(\"Spine\", rgb: #x1a1a20, material: \"TPU\")\n";
        let model = fab_model::FabModel::load("row", project, [("row.cad.splash".to_string(), script.to_string())].into_iter().collect()).unwrap();
        let mut e = fab_model::eval::Evaluator::new(Machine::dual_nozzle_350());
        let ev = model.evaluate(&mut e, None).unwrap_or_else(|f| panic!("the plate builds: {f}")).1;
        assert!(ev.mech.first().is_some_and(|m| m.flexing()), "the spine is TPU");
        assert_eq!(bends_refusal(&ev.mech, false), None, "not measured yet: a commit waits for them");
        let why = bends_refusal(&ev.mech, true).expect("the blocks meet round the spine");
        let Some(Err(f)) = ev.mech[0].flexes() else { panic!("measured") };
        assert!(why.contains(&f.to_string()), "{why}");
        assert_eq!(bends_refusal(&ev.mech, false), Some(why), "then at once");
    }

    /// A part bigger than the printer's bed slices all the same (Assembled and the product shot
    /// need the whole plate): the bed check is a warning beside the slice, in the model's terms
    /// with the printer's name, and never stops it.
    #[test]
    fn a_part_bigger_than_the_bed_slices_with_a_warning() {
        let script = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nobject(m.box({size: [400mm 10mm 1mm]}), name: \"Long\")\n";
        let project = "use mod.fab.*\nscript(\"long.cad.splash\")\n";
        let model = fab_model::FabModel::load("long", project, [("long.cad.splash".to_string(), script.to_string())].into_iter().collect()).unwrap();
        let mut e = fab_model::eval::Evaluator::new(Machine::dual_nozzle_350());
        let ev = Arc::new(model.evaluate(&mut e, None).unwrap_or_else(|f| panic!("the plate builds: {f}")).1);
        assert!(ev.reports.iter().any(|r| r.blocking()), "400 mm is wider than the 350 mm bed: {:?}", ev.reports);
        let (printing, mut state) = (Printing::default(), PrintState::default());
        assert!(printing.source(Some(&ev), Arc::from([]), None).is_ok(), "the plate slices with its part off the bed");
        let warning = printing.fit_warning(Some(&ev)).expect("the part off the bed is a warning");
        assert!(warning.contains("Long") && warning.contains("sliced anyway") && warning.contains(&format!("{}'s bed", ev.machine.name)), "{warning}");
        state.fit_warning = Some(warning.clone());
        state.status = SliceStatus::Done { seconds: 1.0 };
        assert!(fab::print::panel::slice_status(&state).0.contains(&warning), "the slice's status says so");
        let sliced = run_slice(0, &defaults_settings(), None, &ev.plate, &ev.machine, "long").expect("the part off the bed slices");
        assert!(!sliced.preview.layers.is_empty(), "the part off the bed has its layers");
        // Print stops on it until "Print anyway" is ticked (and the G-code safety check still
        // refuses moves off the printable area).
        state.preview = Some(sliced.preview.clone());
        assert!(state.report().issues.iter().any(|i| i.kind == "outside_bed" && i.severity == fab_printability::Severity::Error), "the slice's printability has the part off the bed");
        assert!(state.print_blocker().is_some_and(|why| why.starts_with("Not printable")), "Print stops on it: {:?}", state.print_blocker());
    }

    /// A material whose tray is on a dedicated path says so in the job: a change into or out of
    /// it purges nothing, while a change between the others on the nozzle still does.
    #[test]
    fn a_material_on_a_dedicated_path_changes_with_no_purge() {
        let machine = Machine::dual_nozzle_350();
        let mut s = settings(3);
        // white, red and black on the left nozzle, the black on a dedicated path
        for (m, color) in s.materials.iter_mut().zip([[0.9, 0.9, 0.9, 1.0], [0.8, 0.05, 0.05, 1.0], [0.02, 0.02, 0.02, 1.0]]) {
            m.nozzle = 0;
            m.color = color;
        }
        s.materials[2].dedicated = true;
        let plate = Plate { parts: Vec::new(), excluded: Vec::new(), settings: Vec::new() };
        let setup = resolve_setup(&s, None, &plate, &machine).unwrap();
        let job = makepad_fab_settings::machine::Job::new(setup.build_config());
        let changes = makepad_fab_settings::change::ChangeModel::new(&job);
        assert!(changes.change(Some(0), 1).purge_mm3 > 0.0, "two materials on a shared path purge");
        assert_eq!((changes.change(Some(0), 2).purge_mm3, changes.change(Some(2), 1).purge_mm3), (0.0, 0.0), "into or out of the dedicated path purges nothing");
    }

    /// One material per nozzle has no tower whatever the profile says; a swap on a nozzle keeps
    /// the profile's choice; the project's own setting decides either way.
    #[test]
    fn head_changes_alone_need_no_tower() {
        let machine = Machine::dual_nozzle_350();
        let plate = |settings: Vec<(String, String)>| Plate { parts: Vec::new(), excluded: Vec::new(), settings };
        let tower = |s: &PrintSettings, p: &Plate| resolve_setup(s, None, p, &machine).unwrap().process.values.str("enable_prime_tower").to_string();
        let mut two_heads = settings(2);
        assert_eq!(tower(&two_heads, &plate(Vec::new())), "0");
        assert_eq!(tower(&two_heads, &plate(vec![("enable_prime_tower".into(), "1".into())])), "1");
        two_heads.materials[1].nozzle = 0;
        assert_eq!(tower(&two_heads, &plate(Vec::new())), "", "a swap keeps what the profile says");
        assert_eq!(tower(&two_heads, &plate(vec![("enable_prime_tower".into(), "0".into())])), "0");
    }

    #[test]
    fn colours_round_trip() {
        assert_eq!(colour_hex([1.0, 0.215_86, 0.0, 1.0]), "#FF8000");
    }

    #[test]
    fn printability_markers_and_issue_navigation_share_layer_numbers() {
        let machine = Machine::dual_nozzle_350();
        let plate = Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part("fin", 1, None, cube([100.0,100.0,0.0], 4.0))] };
        let mut report = fab_printability::analyse_plate(&machine, &plate, &Default::default());
        report.issues.push(fab_printability::Issue {
            kind: "thin_wall", severity: fab_printability::Severity::Warning, object: Some("fin".into()),
            at_mm: [100.0,100.0,0.4], footprint: fab_printability::Footprint { min: [99.875, 98.0], max: [100.125, 102.0] }, layer: Some(2), layer_to: None, measure: 0.25, area_mm2: 1.0, lowest_z: 0.4, highest_z: 0.4, flags: 0,
            message: "Thin fin".into(), hint: "Thicken the fin.".into(), fix: None,
        });
        let markers = issue_markers(&report);
        assert_eq!(markers.count(), 2);
        let marker = Bead::of(markers.data());
        assert_eq!((marker.layer, marker.kind), (2.0, 3.0));
        let moves = vec![MoveRecord { kind: MoveKind::Extrude, role: Role::OuterWall, width: 0.4, height: 0.2,
            e: 1.0, p0: [100.0,100.0,0.4], p1: [104.0,100.0,0.4], layer: 2, z_layer: 0.4, t_end: 10.0, ..Default::default() }];
        let mut pv = build_preview(&moves, &Default::default(), vec![[1.0;4]], &machine, &Default::default());
        Arc::get_mut(&mut pv).unwrap().markers = markers;
        Arc::get_mut(&mut pv).unwrap().printability = Arc::new(report);
        let mut state = PrintState::default();
        state.set_preview(pv);
        assert!(fab::print::apply(&mut state, &PrintAction::SelectIssue(0)));
        assert_eq!(state.play.layer_hi, 2);
        assert_eq!(state.play.time, 10.0);
        let spot = state.issue_spot.as_ref().unwrap();
        assert_eq!(spot.instances.count(), 4);
        let frame = Bead::of(spot.instances.data());
        assert_eq!((frame.a, frame.b), ([98.875, 97.0, 0.4], [101.125, 97.0, 0.4]));
        assert!((frame.width - 0.6).abs() < 1e-3, "the frame's line width, to f16: {}", frame.width);
        assert_eq!(spot.opacity(spot.started), 1.0);
        assert_eq!(spot.opacity(spot.started + 1.0), 0.5);
        assert_eq!(spot.opacity(spot.started + 2.0), 0.0);
        let first = spot.instances.id();
        assert!(fab::print::apply(&mut state, &PrintAction::SelectIssue(0)));
        assert_ne!(state.issue_spot.as_ref().unwrap().instances.id(), first, "another press restarts the spot");
        assert_eq!(state.markers().unwrap().count(), 2);
        assert!(fab::print::apply(&mut state, &PrintAction::Markers(false)));
        assert!(state.markers().is_none());
        assert!(fab::print::apply(&mut state, &PrintAction::Markers(true)));
        assert_eq!(state.markers().unwrap().count(), 2);
    }
}

/// Why Print refuses a plate for its bonds: one whose bends can't be measured (the failure's
/// text, as the plate's report has it). `wait`: wait for the bends still being measured (on a
/// worker, when a job is committed), else only those measured so far.
fn bends_refusal(mechs: &[Arc<fab_model::mech::Mech>], wait: bool) -> Option<String> {
    mechs.iter().find_map(|m| match if wait { Some(m.measure_flexes()) } else { m.flexes() } {
        Some(Err(f)) => Some(format!("A bond's bends can't be measured: {f}. Nothing went to the printer.")),
        _ => None,
    })
}

/// Print editor slice integration tests using the app's live engine.
#[cfg(test)]
pub(crate) mod slice_bench {
    use super::*;
    use makepad_fab_slice::live::{LiveEngine, LiveEvent};
    use std::time::Instant;

    // The app's Cx on a native GPU session, pumped as the app pumps it: a slice's continuations
    // run from its events, so waiting for a worker event without pumping would never end.
    pub(crate) struct SliceRuntime {
        pub(super) cx: Cx,
        session: u32,
        paint: bool,
        frame_events: std::rc::Rc<std::cell::Cell<usize>>,
        _guard: std::sync::MutexGuard<'static, ()>,
        _cx_guard: std::sync::RwLockWriteGuard<'static, ()>,
    }
    impl SliceRuntime {
        pub(crate) fn new() -> Self {
            Self::with_events(|_| {})
        }

        pub(super) fn with_events(mut observe: impl FnMut(&Event) + 'static) -> Self {
            use makepad_widgets::makepad_platform::makepad_hal::{Hal, OpenReq, open_flags, backend_id};
            let guard = crate::live_slice::HUB_TESTS.lock().unwrap_or_else(|e| e.into_inner());
            let cx_guard = crate::cx_owned();
            let started = std::rc::Rc::new(std::cell::Cell::new(false));
            let startup = started.clone();
            let mut render: Option<(WindowHandle, DrawPass, DrawList2d)> = None;
            let mut host = crate::live_slice::SliceHost::default();
            let frame_events = std::rc::Rc::new(std::cell::Cell::new(0));
            let frames = frame_events.clone();
            let mut cx = Cx::new(Box::new(move |cx, event| {
                if matches!(event, Event::Startup) {
                    cx.with_vm(|vm| {
                        makepad_widgets::script_mod(vm);
                        makepad_render::script_mod(vm);
                        fab::fab_sdf_tiles::script_mod(vm);
                    });
                    let mut window = WindowHandle::new(cx);
                    window.configure_window(cx, dvec2(32.0, 32.0), dvec2(0.0, 0.0), false, "Print slice test".into());
                    let pass = DrawPass::new(cx);
                    window.set_pass(cx, &pass);
                    render = Some((window, pass, DrawList2d::new(cx)));
                    startup.set(true);
                }
                if matches!(event, Event::Draw(_) | Event::NextFrame(_)) { frames.set(frames.get() + 1); }
                observe(event);
                host.event(cx, event);
                if let (Event::Draw(event), Some((_, pass, root))) = (event, render.as_mut()) {
                    let mut draw = CxDraw::new(cx, event);
                    let mut cx = Cx2d::new(&mut draw);
                    cx.begin_pass(pass, None);
                    root.begin_always(&mut cx);
                    cx.begin_root_turtle(dvec2(32.0, 32.0), Layout::flow_overlay());
                    cx.end_pass_sized_turtle();
                    root.end(&mut cx);
                    cx.end_pass(pass);
                }
            }));
            cx.init_cx_os();
            let mut request = OpenReq::from_env(0, open_flags::OFFSCREEN_WINDOWS, 1.0);
            request.flags |= open_flags::OFFSCREEN_WINDOWS;
            let info = Hal::global().open(&request);
            assert_ne!(info.backend, backend_id::HEADLESS, "Print field tests require a native GPU");
            cx.hal_attach_session(info);
            while !started.get() { assert!(cx.hal_session_turn(1_000_000, true)); }
            // Startup initializes the native memory envelope and worker pool, as in the app; the
            // slicing pool is the process's, which this runtime's quit does not close. No memory
            // stop: the footprint it reads is the whole test process's (live_slice::start).
            crate::live_slice::init_for_tests();
            crate::live_slice::start(&cx);
            Self { cx, session: info.session, paint: true, frame_events, _guard: guard, _cx_guard: cx_guard }
        }
        pub(crate) fn withhold_paint(&mut self) {
            self.paint = false;
            self.frame_events.set(0);
        }
        pub(crate) fn frame_events(&self) -> usize { self.frame_events.get() }
        pub(crate) fn engine(&self) -> (LiveEngine, std::sync::mpsc::Receiver<LiveEvent>) {
            let (tx, rx) = std::sync::mpsc::channel();
            let tx = std::sync::Mutex::new(tx);
            let pool = self.cx.task_pool();
            let version = Arc::new(crate::live_slice::EngineVersion::default());
            let spawn_pool = pool.clone();
            // as a shown slice's (the app's live view or Print editor)
            let engine = LiveEngine::new(Arc::new(move |urgency, job| {
                spawn_pool.submit(crate::live_slice::shown_class(urgency), job).unwrap().detach();
            }), Arc::new(move |e| { let _ = tx.lock().unwrap().send(e); }), version.next());
            crate::live_slice::set_resume(&engine, version, pool, true);
            (engine, rx)
        }
        pub(super) fn turn(&mut self) {
            let present = self.paint && self.cx.frame_completion_serial() >= self.cx.frame_submission_serial();
            assert!(self.cx.hal_session_turn(1_000_000, present));
        }
        pub(crate) fn recv<T>(&mut self, rx: &std::sync::mpsc::Receiver<T>) -> T {
            self.recv_or(rx, || false).expect("the next")
        }
        /// The next of `rx`, or none once `idle` (asked every frame, before `rx`) holds and nothing came.
        pub(crate) fn recv_or<T>(&mut self, rx: &std::sync::mpsc::Receiver<T>, idle: impl Fn() -> bool) -> Option<T> {
            let start = Instant::now();
            loop {
                let idle = idle();
                match rx.try_recv() {
                    Ok(value) => return Some(value),
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => panic!("engine disconnected"),
                    Err(std::sync::mpsc::TryRecvError::Empty) if idle => return None,
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                }
                assert!(start.elapsed().as_secs() < 180, "the engine answers");
                self.turn();
            }
        }
    }
    impl Drop for SliceRuntime {
        fn drop(&mut self) {
            self.cx.quit();
            let start = Instant::now();
            while self.cx.hal_session_turn(1_000_000, false) {
                if start.elapsed().as_secs() >= 5 { break }
            }
            makepad_widgets::makepad_platform::makepad_hal::Hal::global().close(self.session);
        }
    }

    /// A project's text and its scripts' sources (file -> text).
    pub(crate) type Sources = (String, std::collections::BTreeMap<String, String>);

    pub(crate) fn plate((project, scripts): &Sources) -> Arc<Evaluated> {
        let m = fab_model::FabModel::load("bench", project, scripts.clone()).expect("the project loads");
        let mut e = fab_model::eval::Evaluator::new(Machine::dual_nozzle_350());
        Arc::new(m.evaluate(&mut e, None).expect("the plate builds").1)
    }

    /// The plate Fab opens with, without its lattice cube (a Splash-kernel field that costs a
    /// third of a second a layer to slice: its own tests slice it): a bracket and an SDF vase.
    pub(crate) fn demo() -> Sources {
        let project = crate::demo_part::PROJECT.lines().filter(|l| !l.contains("lattice_cube")).collect::<Vec<_>>().join("\n");
        (project, crate::demo_part::SCRIPTS.iter().filter(|(f, _)| !f.starts_with("lattice_cube")).map(|(f, s)| (f.to_string(), s.to_string())).collect())
    }

    /// Fab's defaults with a material for each tool the plate prints with, in the colour it
    /// prints in (so the bench's changes purge what the plate's colours ask), on the two nozzles
    /// in turn.
    pub(crate) fn setup(ev: &Evaluated) -> CheckSetup {
        let mut settings = defaults_settings();
        let colours = tool_colours(ev, None).model;
        settings.materials = colours.into_iter().enumerate().map(|(i, color)| JobMaterial { preset: String::new(), color, nozzle: i % 2, tray: None, dedicated: false }).collect();
        CheckSetup { dirs: Arc::default(), calibration: Arc::default(), settings, catalog: None, mapping: Some(ev.mapped.clone()) }
    }

    /// What one Print editor slice showed: progress and the streamed layers over time.
    pub(crate) struct AppSlice {
        pub progress: Vec<(f64, f32)>,
        /// (seconds, part layers streamed, preview layers).
        pub streamed: Vec<(f64, u32, usize)>,
        /// What the slice view drew of each streamed preview, as the Print editor took it:
        /// (seconds, layers drawn, beads drawn, the progress bar then).
        pub shown: Vec<(f64, usize, usize, f32)>,
        /// Each stage as it began: (seconds, progress, its name).
        pub stages: Vec<(f64, f32, String)>,
        /// The part layers in each band the engine streamed before the complete plate, in order.
        pub bands: Vec<usize>,
        pub seconds: f64,
        pub rebuilds: u64,
    }

    /// A Print slice or live edit of `ev`, retaining the engine's caches from the last.
    pub(crate) fn app_slice_of(ev: &Arc<Evaluated>, setup: CheckSetup, headset: bool, verbose: bool, edit: bool) -> AppSlice {
        use std::sync::atomic::Ordering;
        let mut runtime = SliceRuntime::new();
        let (tx, rx) = std::sync::mpsc::channel();
        *crate::live_slice::OBSERVE.lock().unwrap() = Some(tx);
        let r0 = crate::live_slice::REBUILDS[0].load(Ordering::Relaxed);
        let live = (headset || edit).then(crate::live_slice::LiveSlice::start);
        if let Some(live) = live.as_ref().filter(|_| !edit) {
            live.set_plate(ev.clone(), 0.2, setup.materials(&ev.plate, &ev.machine).expect("materials"));
        }
        let t0 = Instant::now();
        let (e2, s2) = (ev.clone(), setup.clone());
        let make: makepad_fab_slice::live::MakeInput = Arc::new(move || crate::live_slice::input_for(crate::live_slice::plate_id(&e2), &s2, Arc::new(e2.plate.printable()), e2.machine.clone(), Some(&e2.frames), Some(&e2.fields)));
        let job = crate::live_slice::PrintJob { gen: 9, head_envelope: setup.head_envelope().to_path_buf(), settings: setup.settings.clone(), colours: setup.settings.materials.iter().map(|m| m.color).collect(), model_colours: tool_colours(ev, None).model, shared_tools: Vec::new(), machine: ev.machine.clone(), name: "bench".into(), objects: Arc::from([]), check: None, fallback: None, catalog: None, tests: None, melts: Vec::new() };
        let mut last = String::new();
        let mut out = AppSlice { progress: Vec::new(), streamed: Vec::new(), shown: Vec::new(), stages: Vec::new(), bands: Vec::new(), seconds: 0.0, rebuilds: 0 };
        // The Print editor and its slice view, taking events as the app does.
        let mut printing = Printing { gen: 9, slice_gen: Some(9), ..Printing::default() };
        let mut app = fab::AppState::default();
        app.print.settings = setup.settings.clone();
        app.print.status = SliceStatus::Running { progress: 0.0, stage: String::new() };
        apply(&mut app.print, &PrintAction::Preview(true));
        // An edit's run waits after each part's viewed layers (its first band) until the view has
        // taken every publication of them: what the view draws before the rest of the plate is
        // there does not race the engine, which on a quiet machine can finish before the view's
        // first look.
        let mut paced = edit.then(crate::live_slice::gates::pace_runs);
        let mut held = false;
        if edit {
            printing.live_input(&mut app.print, Some(ev), Arc::from([]), None);
            assert_eq!(printing.gen, 9, "an edit is not a Print generation");
            assert!(printing.slice_gen.is_none(), "an edit awaits no Sliced event");
        } else {
            crate::live_slice::settle(job, crate::live_slice::SliceInput { plate_id: crate::live_slice::plate_id(&ev), settings_id: setup.id_for(&ev.plate), parts: ev.plate.parts.len(), make });
        }
        let mut live_complete = false;
        loop {
            // While the run is held no event may come: the view takes the newest publication on
            // every frame, as the app's views do, until it took all of what came.
            let next = match paced.as_ref().filter(|_| held) {
                Some(_) => runtime.recv_or(&rx, || {
                    if let Some(live) = live.as_ref() {
                        let _ = live.latest();
                    }
                    crate::live_slice::gates::publications_taken()
                }),
                None => Some(runtime.recv(&rx)),
            };
            let Some((at, what, preview, result)) = next else {
                assert!(out.shown.iter().any(|f| f.1 > 0), "the view draws the plate's first bands while the rest is to come: {:?}", out.shown);
                // the rest of the plate comes
                paced = None;
                continue;
            };
            let s = (at - t0).as_secs_f64();
            // The view takes the newest publication as the app's views do, on every frame (each
            // event here stands for one): the next waits for that. A take that finds the newest
            // being stored tries again on the next, as a view's does.
            if let Some(live) = live.as_ref() {
                let _ = live.latest();
            }
            if what == "run held" {
                held = true;
                continue;
            }
            if edit && live_complete {
                assert!(!what.starts_with("layers ") && what != "cut", "the requested settle reuses every cut and plan");
            }
            if let Some(n) = what.strip_prefix("layers ").filter(|_| !live_complete) {
                out.bands.push(n.parse().unwrap());
            }
            if let Some(result) = result.filter(|_| edit && printing.live.is_some()) {
                // The view takes a complete publication as the slice done, with what it shows.
                assert!(!result.complete || result.layers_done == result.layers_total, "a publication that says the slice is whole carries every layer: {result:?}");
                let shown_before = app.print.preview.as_ref().map(|p| p.id);
                printing.live_updated(&mut app.print, &result);
                assert!(!app.print.can_send(), "live tiles cannot be printed or exported");
                let current = printing.live.as_ref().is_some_and(|l| crate::live_slice::input_revision(&l.input) == Some(result.revision));
                if let Some(report) = result.report.as_ref().filter(|_| current && app.print.preview.is_some() && !app.print.stale) {
                    assert!(Arc::ptr_eq(app.print.report(), report), "the panel shows what the report says");
                }
                if app.print.preview.as_ref().map(|p| p.id) != shown_before {
                    let bar = result.layers_done as f32 / result.layers_total.max(1) as f32;
                    let (layers, beads) = shown(&app);
                    out.streamed.push((s, result.layers_done, layers));
                    out.shown.push((s, layers, beads, bar));
                }
                if result.complete && printing.live.as_ref().is_some_and(|l| crate::live_slice::input_revision(&l.input) == Some(result.revision)) {
                    live_complete = true;
                    // Explicit Slice reuses the input whose toolpaths the view shows.
                    printing.action(&mut app.print, &PrintAction::Slice, Some(ev));
                }
            } else if let Some(rest) = what.strip_prefix("progress ") {
                let (p, stage) = rest.split_once(' ').unwrap();
                out.progress.push((s, p.parse().unwrap()));
                if stage != last {
                    out.stages.push((s, p.parse().unwrap(), stage.to_string()));
                    last = stage.to_string();
                }
            } else if let Some(rest) = what.strip_prefix("streamed ") {
                let (a, b) = rest.split_once(' ').unwrap();
                out.streamed.push((s, a.parse().unwrap(), b.parse().unwrap()));
                let bar = out.progress.last().map_or(0.0, |p| p.1);
                let gen = printing.gen;
                printing.event(&mut app.print, &PrintEvent::Progress { gen, progress: bar * 0.9, stage: String::new() });
                printing.event(&mut app.print, &PrintEvent::Streamed { gen, preview: preview.expect("the streamed preview") });
                let (layers, beads) = shown(&app);
                if !edit { out.shown.push((s, layers, beads, bar)); }
            } else if what.starts_with("settled") || what.starts_with("failed") {
                if verbose {
                    println!("{s:8.3} s  {what}");
                }
            } else if what.starts_with("sliced") {
                assert_eq!(what, "sliced true", "the settle succeeds");
                out.seconds = s;
                if verbose {
                    println!("{s:8.3} s  {what} (the Print editor has it)");
                }
                // An edit's: its beads become the live tiles' (next).
                if !edit {
                    break;
                }
            } else if let Some(n) = what.strip_prefix("beads adopted ").filter(|_| edit) {
                // Settled as the view showed the complete plate, with the owner busy publishing:
                // it came after the plate's toolpaths, so it knew where the parts are.
                assert!(n.parse::<usize>().unwrap() > 0, "the settled slice's beads become the live tiles' layers (its preview holds them in the parts' frames)");
                break;
            }
        }
        *crate::live_slice::OBSERVE.lock().unwrap() = None;
        assert!(!edit || (held && paced.is_none()), "the edit's run waited after its first band, as the test paces it, and went on");
        out.rebuilds = crate::live_slice::REBUILDS[0].load(Ordering::Relaxed) - r0;
        drop(live);
        out
    }

    /// What the slice view draws of the Print editor's slice: (part layers with a bead
    /// drawn, beads drawn), from the frame the viewport renders.
    fn shown(app: &fab::AppState) -> (usize, usize) {
        let Some(f) = fab::viewport::print::PrintFrame::of(app, None) else { return (0, 0) };
        let [lo, hi] = [f.uniforms[1].1[0], f.uniforms[1].1[1]];
        let mut layers = std::collections::BTreeSet::new();
        let mut beads = 0;
        let started = f.cut.ranges().into_iter().zip(&f.preview.beads.0).flat_map(|(r, b)| b.data_slices(r.start as usize * PREVIEW_SLOTS..r.end as usize * PREVIEW_SLOTS).flat_map(|(_, s)| s.chunks_exact(PREVIEW_SLOTS)));
        for bead in started.chain(f.preview.chunks.iter().flat_map(|c| c.beads.data().chunks_exact(PREVIEW_SLOTS))) {
            let (layer, tag) = (fab::print::bead::layer(bead), fab::print::bead::tag(bead));
            if layer >= lo && layer <= hi {
                layers.insert((tag as u32 / 2, layer as u32));
                beads += 1;
            }
        }
        (layers.len(), beads)
    }

    /// The demo's part layers become drawable before the complete plate, including after
    /// settings change. Refining a tile may change its bead count.
    #[test]
    fn the_users_plate_publishes_tiles_before_the_complete_plate() {
        makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map_or(4, |n| n.get())).min(8));
        // The engine streams the plate in bands, at any speed: several bands each end between a
        // tenth and nine tenths of the plate's layers. The view takes them at its own pace, so
        // how many of them a publication holds depends on the machine; the run waits after its
        // first band until the view took it (`app_slice_of`), so the first drawn holds part of
        // the plate, never all of it, and the last holds every layer.
        let streams = |r: &AppSlice, what: &str| {
            let all: usize = r.bands.iter().sum();
            let mut sum = 0;
            let between = r.bands.iter().filter(|n| {
                sum += **n;
                sum * 10 > all && sum * 10 < all * 9
            }).count();
            assert!(between >= 3, "{what}: the engine streams the layers in bands, not in one go: {:?}", r.bands);
            let shown: Vec<_> = r.shown.iter().filter(|f| f.1 > 0).collect();
            let last = shown.last().unwrap_or_else(|| panic!("{what}: the view shows the stream: {:?}", r.shown)).1;
            assert!(shown[0].1 < last, "{what}: the first tiles drawn come before the complete plate: {:?}", r.shown);
            last
        };
        // Thick layers (the thickest the setting allows): the stream is the same in bands.
        let mut src = demo();
        src.0.push_str("\nsettings({layer_height: 0.6})\n");
        let ev = plate(&src);
        let r = app_slice_of(&ev, setup(&ev), false, false, true);
        streams(&r, "the first slice");
        assert!(r.progress.windows(2).all(|w| w[1].1 >= w[0].1), "progress does not move backwards");
        // Sliced again with other settings: every part is planned anew, so the old layers go
        // and the new tiles appear in view as on the first slice.
        let mut src = demo();
        src.0.push_str("\nsettings({layer_height: 0.6 sparse_infill_density: 35})\n");
        let ev = plate(&src);
        let other = setup(&ev);
        crate::live_slice::set_print_setup(other.clone());
        let r = app_slice_of(&ev, other, false, false, true);
        let all = streams(&r, "the slice with other settings");
        assert!(r.shown.iter().any(|f| f.1 * 10 < all), "the invalidated layers leave: {:?}", r.shown);
    }

    /// A committed job: Monitor draws it from its package, the same beads and head as the slice
    /// that went, and after a restart it finds it on the disk and comes back on its own.
    fn committed_job_round_trip(sliced: &Sliced) {
        use fab::print::follow::{place, printer_changed};
        let dir = std::env::temp_dir().join(format!("fab-job-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let destination = PrinterChoice::Real("test".into());
        let snapshot = fab_printer_link::Snapshot { serial_suffix: Some("0001".into()), ..Default::default() };
        let meta = job_meta(sliced, &destination, Some(&snapshot), &fab_printer_link::PrintOptions::default(), Vec::new());
        assert_eq!(meta.serial_suffix.as_deref(), Some("0001"), "confirm captures the reported suffix");
        // As in the app, package the job and rebuild its preview on workers.
        fn off_ui<T: Send>(f: impl FnOnce() -> T + Send) -> T {
            std::thread::scope(|s| s.spawn(f).join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
        }
        let package = off_ui(|| sliced.settled.package_with(&|_| {}, &|| false).expect("the package"));
        let scripts = std::collections::BTreeMap::from([("plate.fab.splash".to_string(), "the plate".to_string())]);
        let sent = SavedState { state: JobState::Sent, started: Some(meta.created), ..Default::default() };
        jobs::commit(&dir, &meta, &sent, &package, &scripts).expect("committed");
        let saved = jobs::read_all(&dir);
        assert_eq!((saved.len(), &saved[0].0, &saved[0].1), (1, &meta, &sent));
        assert_eq!(jobs::read_package(&dir, &meta.id).expect("the package"), package, "the bytes that went");

        let made = off_ui(|| preview_of_package(&package, &meta)).expect("the preview");
        let had = &sliced.preview;
        assert!(super::tests::same_beads(&had.beads, &made.beads), "the same beads: {} beads, {} now", had.beads.count(), made.beads.count());
        assert!(had.travel.data() == made.travel.data(), "the same travel moves");
        assert_eq!((had.total, had.beads.count(), had.layers.len()), (made.total, made.beads.count(), made.layers.len()));
        for i in 0..500 {
            let at = had.total * i as f64 / 500.0;
            let (a, b) = (had.timeline.now(at), made.timeline.now(at));
            assert_eq!((a.nozzles, a.active, a.doing, a.nozzle_temps, a.fan), (b.nozzles, b.active, b.doing, b.nozzle_temps, b.fan), "the head at {at} s");
        }

        // Restarted: the job from the disk, the printer on layer 2 of it, an elapsed time from the
        // press of Start print. The plate slices on and Monitor doesn't move.
        let (now, wall) = (1000.0, meta.created + 600.0);
        let mut p = PrintState::default();
        p.links.picked = destination;
        p.follow.jobs_read = true;
        p.follow.monitor = true;
        p.follow.jobs.push(CommittedJob::restored(saved[0].0.clone(), saved[0].1.clone(), now, wall));
        let mut reports = fab_printer_link::model::Reports::default();
        let report = format!(r#"{{"print":{{"msg":0,"gcode_state":"RUNNING","stg_cur":0,"subtask_name":"{}","layer_num":2,"total_layer_num":{},"mc_remaining_time":10}}}}"#, meta.name, meta.layers);
        p.printers.set(&p.links.picked, 1, Arc::new(fab_printer_link::Snapshot { status: fab_printer_link::ConnectionStatus::Connected, state: Arc::new(reports.apply(report.as_bytes()).unwrap().unwrap()), ..Default::default() }));
        printer_changed(&mut p, now);
        assert!(p.monitor_holds() && p.monitor_preview().is_none(), "its slice is on its way");
        assert_eq!(p.follow.job.as_ref().and_then(|j| j.id.as_deref()), Some(meta.id.as_str()));
        p.follow.job_of_mut(&meta.id).unwrap().preview = Some(made.clone());
        printer_changed(&mut p, now);
        assert!(Arc::ptr_eq(p.monitor_preview().unwrap(), &made));
        assert_eq!(p.play.layer_hi, 1, "the view is on the printer's layer");
        let at = fab::print::job_card::job_view(&p, now).unwrap();
        assert!(at.times.starts_with("Elapsed 10:00"), "{}", at.times);
        p.set_preview(sliced.preview.clone());
        p.stale = true;
        place(&mut p, now);
        assert!(Arc::ptr_eq(p.monitor_preview().unwrap(), &made), "the plate's slice doesn't reach the job");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Both tests read the same settled demo. The GPU runtime stays on the thread that slices it.
    pub(super) fn with_demo_slice<R>(check: impl FnOnce(&Sliced, usize) -> R) -> R {
        static DEMO: std::sync::OnceLock<(Sliced, usize)> = std::sync::OnceLock::new();
        let (sliced, paths) = DEMO.get_or_init(slice_demo);
        check(sliced, *paths)
    }

    /// The settled bracket and vase from the opening plate, through the live slice engine.
    fn slice_demo() -> (Sliced, usize) {
        makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map_or(4, |n| n.get())).min(8));
        let mut src = demo();
        src.0.push_str("\nsettings({layer_height: 0.6})\n");
        let ev = plate(&src);
        let setup = setup(&ev);
        let input = crate::live_slice::input_for(crate::live_slice::plate_id(&ev), &setup, Arc::new(ev.plate.printable()), ev.machine.clone(), Some(&ev.frames), Some(&ev.fields));
        let (tx, rx) = std::sync::mpsc::channel::<LiveEvent>();
        let tx = std::sync::Mutex::new(tx);
        let mut runtime = SliceRuntime::new();
        let pool = runtime.cx.task_pool();
        let version = Arc::new(crate::live_slice::EngineVersion::default());
        let spawn_pool = pool.clone();
        let engine = LiveEngine::new(Arc::new(move |_, job| {
            spawn_pool.submit(makepad_widgets::makepad_platform::thread::Class::Visible, job).unwrap().detach();
        }), Arc::new(move |e| { let _ = tx.lock().unwrap().send(e); }), version.next());
        crate::live_slice::set_resume(&engine, version, pool, true);
        engine.settle(1, input);
        let mut paths = None;
        let settled = loop {
            match runtime.recv(&rx) {
                LiveEvent::Toolpaths { toolpaths, .. } => paths = Some(toolpaths.read().layers.len()),
                LiveEvent::Settled { tag: 1, result, .. } => break result.expect("sliced"),
                LiveEvent::Failed { error, .. } => panic!("{error}"),
                _ => {}
            }
        };
        let job = crate::live_slice::PrintJob { gen: 0, head_envelope: setup.head_envelope().to_path_buf(), settings: setup.settings.clone(), colours: setup.settings.materials.iter().map(|m| m.color).collect(), model_colours: tool_colours(&ev, None).model, shared_tools: Vec::new(), machine: ev.machine.clone(), name: "count".into(), objects: Arc::from([]), check: None, fallback: None, catalog: None, tests: None, melts: Vec::new() };
        let sliced = finish_sliced(&job, &settled, Arc::from([]));
        (sliced, paths.expect("toolpaths"))
    }

    /// One layer count everywhere: the settled preview's layers (what Monitor matches a
    /// printer's job against), the G-code's `total layer number` (what the printer reports)
    /// and the toolpaths' layers (what the streamed preview shows).
    #[test]
    fn the_preview_the_gcode_and_the_toolpaths_count_the_same_layers() {
        with_demo_slice(|sliced, paths| {
            let header: usize = sliced.settled.gcode.text().unwrap().lines().find_map(|l| l.strip_prefix("; total layer number: ")).and_then(|n| n.trim().parse().ok()).expect("the G-code's layer count");
            let preview = sliced.preview.max_layer() as usize + 1;
            assert_eq!((preview, paths), (header, header), "settled preview and toolpaths against the G-code's layer count");
            committed_job_round_trip(sliced);
        });
    }

    /// The bench prints in the plate's colours, so the numbers it settles to count the purge: a
    /// black, a white and a red block, the black and the red sharing the left nozzle, purge at
    /// every swap there; the black and the white alone, a nozzle each, purge nothing.
    #[test]
    fn the_bench_purges_in_the_plates_colours() {
        let script = "use mod.model.*\nuse mod.fab.*\nlet block = mod.model.sdf({bounds: [[-3mm, 0mm, -3mm], [3mm, 2mm, 3mm]] cell: 0.1mm field: fn(p) { return sdf_box(p - vec3(0, 1mm, 0), vec3(6mm, 2mm, 6mm)) }})\nobject(block)\n";
        let blocks = [("black", "Black", "#x111111"), ("white", "White", "#xf4f4f4"), ("red", "Red", "#xd01010")];
        let settle = |n: usize| {
            let mut project = String::from("use mod.fab.*\n");
            for (i, (file, colour, rgb)) in blocks[..n].iter().enumerate() {
                project += &format!("color(\"{colour}\", rgb: {rgb})\nscript(\"{file}.cad.splash\")\nplace(\"{file}\", object: \"block\", at: [{}mm 165mm], name: \"{colour} block\", color: \"{colour}\")\n", 80 + 20 * i);
            }
            let ev = plate(&(project, blocks[..n].iter().map(|(file, _, _)| (format!("{file}.cad.splash"), script.to_string())).collect()));
            let setup = setup(&ev);
            let (engine, rx) = crate::live_slice::cpu_engine();
            engine.settle(1, crate::live_slice::input_for(crate::live_slice::plate_id(&ev), &setup, Arc::new(ev.plate.printable()), ev.machine.clone(), Some(&ev.frames), Some(&ev.fields)));
            let settled = rx.iter().find_map(|e| match e {
                LiveEvent::Settled { tag: 1, result, .. } => Some(result.expect("sliced")),
                LiveEvent::Failed { error, .. } => panic!("{error}"),
                _ => None,
            }).expect("the engine answers");
            // The settled slice's own report has no figures: its sliced report streams after it,
            // on its timeline, and weighs what the timeline feeds.
            assert!(settled.report.plate.is_none(), "no 0 g that isn't one");
            let report = rx.iter().find_map(|e| match e {
                LiveEvent::Issues { report, done, total, .. } if done == total && total > 0 => Some(report),
                _ => None,
            }).expect("the settled slice's checks");
            let plate = report.plate.as_ref().expect("the sliced figures");
            let fed: f64 = settled.timeline.filament_mm.iter().enumerate().map(|(f, mm)| mm * settled.job.filament_area(f) * settled.job.fil("filament_density", f) / 1000.0).sum();
            let grams: f64 = settled.stats.filament_g.iter().sum();
            assert!((plate.mass_g - fed).abs() < 1e-6 && (grams - fed).abs() < 1e-6, "the report and the slot grams weigh the timeline, purge and all: {} g, {grams} g, {fed} g", plate.mass_g);
            let colours: Vec<[f32; 4]> = setup.settings.materials.iter().map(|m| m.color).collect();
            (colours, plate.flush_mass_g, plate.mass_g, settled.timeline.filament_swaps)
        };
        let (colours, purged, printed, swaps) = settle(3);
        assert_eq!(colours.len(), 3, "a material for each colour");
        let dark = |c: &[f32; 4]| c[..3].iter().all(|&v| v < 0.1);
        assert!(dark(&colours[0]) && colours[1][..3].iter().all(|&v| v > 0.9) && colours[2][0] > 0.5 && colours[2][1] < 0.1, "black, white, red: {colours:?}");
        assert!(swaps > 0 && purged > 0.0, "the left nozzle swaps black and red: {swaps} swaps purge {purged:.2} g of {printed:.2} g");
        let (_, purged, _, swaps) = settle(2);
        assert!(swaps == 0 && purged.abs() < 1e-6, "each nozzle keeps its colour: {swaps} swaps, {purged} g");
    }

    /// The plate's slice and its live estimate's changes, on `engine` (its caches kept).
    fn sliced_and_changes(engine: &makepad_fab_slice::live::LiveEngine, rx: &std::sync::mpsc::Receiver<LiveEvent>, ev: &Arc<Evaluated>, state: &PrintState) -> (Arc<makepad_fab_slice::live::Settled>, Arc<makepad_fab_gcode::heads::Changes>) {
        let mut settings = defaults_settings();
        settings.materials = state.settings.materials.clone();
        let setup = CheckSetup { dirs: Arc::default(), calibration: Arc::default(), settings, catalog: None, mapping: Some(ev.mapped.clone()) };
        engine.settle(1, crate::live_slice::input_for(crate::live_slice::plate_id(ev), &setup, Arc::new(ev.plate.printable()), ev.machine.clone(), Some(&ev.frames), Some(&ev.fields)));
        let (mut settled, mut changes) = (None, None);
        while settled.is_none() || changes.is_none() {
            match rx.recv().expect("the engine answers") {
                LiveEvent::Toolpaths { .. } => engine.estimate(),
                LiveEvent::Settled { tag: 1, result, .. } => settled = Some(result.expect("sliced")),
                LiveEvent::Estimated { estimate, .. } => changes = Some(estimate.changes.clone().expect("a plate printed layer by layer")),
                LiveEvent::Failed { error, .. } => panic!("{error}"),
                _ => {}
            }
        }
        (settled.unwrap(), changes.unwrap())
    }

    /// The model worker is between its old and new colour mappings when the person asks to slice.
    fn check_plate_wait(printing: &mut Printing, state: &mut PrintState, old: &Arc<Evaluated>, rebuilt: &Arc<Evaluated>) {
        let colours = "Updating the plate for the printer's colours";
        let bed = "Updating the plate for the selected printer";
        let waiting = |why: &str| SliceStatus::Running { progress: 0.0, stage: why.into() };
        let setup = printing.check_setup(state);
        assert_eq!(setup.plate_waits(old), Some(colours), "the new materials require the rebuilt plate");
        assert_eq!(crate::live_slice::send_live_plate(old, setup.clone()), Err(colours), "XR skips the old plate before submitting it");
        assert!(state.can_send(), "the finished slice was printable before the wait");
        let shown = state.preview.clone().unwrap();
        Printing::mark_shown_slice_stale(state);
        assert!(!state.can_send(), "the shown slice for the old mapping must not be sent");
        assert!(state.settled && Arc::ptr_eq(state.preview.as_ref().unwrap(), &shown), "the wait keeps the finished preview on screen");

        let (checked, checks) = std::sync::mpsc::channel();
        let (sliced, slices) = std::sync::mpsc::channel();
        let old_report = old.printability.clone();
        {
            // The pool creates its own Cx once. Do that before installing the receiver for these actions.
            let _cx = crate::cx_owned();
            crate::live_slice::init_for_tests();
        }
        let mut runtime = SliceRuntime::with_events(move |event| {
            if let Event::Actions(actions) = event {
                for action in actions {
                    match action.downcast_ref::<PrintEvent>() {
                        Some(PrintEvent::Checked { id: 1, report, error }) if Arc::ptr_eq(report, &old_report) => {
                            checked.send(error.clone()).unwrap();
                        }
                        Some(PrintEvent::Sliced { gen, result, seconds }) => {
                            sliced.send(PrintEvent::Sliced { gen: *gen, result: result.clone(), seconds: *seconds }).unwrap();
                        }
                        _ => {}
                    }
                }
            }
        });
        printing.check_slice(1, state, old);
        assert_eq!(runtime.recv(&checks).as_deref(), Some(colours));
        let mut slice_answer = |generation: u64| loop {
            let event = runtime.recv(&slices);
            if matches!(event, PrintEvent::Sliced { gen, .. } if gen == generation) { break event; }
        };
        let gen_before = printing.gen;
        assert!(printing.action(state, &PrintAction::AskPrint(true), Some(old)), "Print asks for a new slice");
        assert_eq!(state.status, waiting(colours), "an asked slice waits for the printer's colours");
        assert_eq!(printing.reask, Some(false));
        assert_eq!((printing.gen, printing.slice_gen), (gen_before, None), "no slice starts while the printer's colours are applied");
        assert!(printing.slice_asked(), "a held Print press survives leaving Sliced view");

        let mut old_bed = (**rebuilt).clone();
        old_bed.machine.max_height += 1.0;
        printing.resume_asked(state, Some(&Arc::new(old_bed)));
        assert_eq!(state.status, waiting(bed), "the same ask also waits for the printer's bed");
        assert_eq!(printing.reask, Some(false));
        assert_eq!((printing.gen, printing.slice_gen), (gen_before, None), "no slice starts while the printer's bed updates");

        assert_eq!(setup.plate_waits(rebuilt), None, "an equal mapping is ready even in a different Arc");
        assert!(printing.resume_asked(state, Some(rebuilt)));
        assert_eq!(printing.slice_gen, Some(gen_before + 1), "resume_asked starts a fresh slice once the rebuilt plate lands");
        assert!(printing.slice_asked() && printing.reask.is_none());
        assert_eq!(printing.event(state, &slice_answer(printing.gen)), PrintOutcome::Target);
        assert!(matches!(state.status, SliceStatus::Done { .. }), "the resumed slice finished: {:?}", state.status);
        assert!(state.can_send(), "the finished slice for the rebuilt plate can be sent");

        assert!(printing.refresh_printer(state, Some(rebuilt)).is_none(), "the model worker already applied this mapping");
        assert_eq!(printing.check_setup(state).plate_waits(old), Some(colours), "refresh keeps the mapping even without another hand-off");
        printing.live_input(state, Some(old), Arc::from([]), None);
        assert_eq!(state.status, waiting(colours), "a live edit waits for the printer's colours");
        assert!(printing.live.is_none());
        let mut wrong = (**rebuilt).clone();
        let part = &mut wrong.plate.parts[0];
        part.nozzle = Some(1 - part.nozzle.unwrap());
        assert_eq!(printing.plate_waits(state, &wrong), None);
        printing.slice(state, Some(&Arc::new(wrong)));
        assert_eq!(printing.event(state, &slice_answer(printing.gen)), PrintOutcome::Target);
        assert!(matches!(&state.status, SliceStatus::Failed(e) if e.contains("must print on nozzle") && e.contains("is mapped to nozzle")),
            "a real part/material mismatch reaches the slicer: {:?}", state.status);
    }

    /// Two tall blocks an automatic pick puts on one head swap there on every layer: the plate's
    /// own changes weigh the other head, a colour moves to where the plate changes filament
    /// least (its row says why), and the plate slices again once, swapping only where a tall
    /// block meets the short one, as weighed, from the same cuts and plans (the nozzles are
    /// alike: only the writer runs again); weighed again on that slice, nothing moves.
    #[test]
    fn a_colour_moves_to_the_head_where_the_plate_changes_filament_less() {
        let block = |h: &str| format!("use mod.model.*\nuse mod.fab.*\nlet block = mod.model.sdf({{bounds: [[-4mm, 0mm, -4mm], [4mm, {h}, 4mm]] cell: 0.1mm field: fn(p) {{ return sdf_box(p - vec3(0, {h} / 2, 0), vec3(8mm, {h}, 8mm)) }}}})\nobject(block)\n");
        let blocks = [("black", "Black", "#x111111", "6mm"), ("white", "White", "#xf4f4f4", "1.6mm"), ("red", "Red", "#xd01010", "6mm")];
        let mut project = String::from("use mod.fab.*\n");
        for (i, (file, colour, rgb, _)) in blocks.iter().enumerate() {
            project += &format!("color(\"{colour}\", rgb: {rgb})\nscript(\"{file}.cad.splash\")\nplace(\"{file}\", object: \"block\", at: [{}mm 165mm], name: \"{colour} block\", color: \"{colour}\")\n", 80 + 20 * i);
        }
        let scripts = blocks.iter().map(|(file, _, _, h)| (format!("{file}.cad.splash"), block(h))).collect();
        let ev = plate(&(project, scripts));
        let (mut printing, mut state) = (Printing::default(), PrintState::default());
        state.settings = defaults_settings();
        let mapped = printing.refresh_printer(&mut state, Some(&ev)).expect("the plate takes the mapping");
        let mut first_ev = (*ev).clone();
        first_ev.set_mapped(mapped);
        let first_ev = Arc::new(first_ev);
        let (engine, rx) = crate::live_slice::cpu_engine();
        let (first, changes) = sliced_and_changes(&engine, &rx, &first_ev, &state);
        state.set_preview(build_preview_of_layers(&first.layers, &first.timeline, tool_colours(&first_ev, None).for_tools(&first.tools),
            &first_ev.machine, &first.stats, Arc::from([])));
        let (choices, room) = printing.head_choices.clone();
        let choice = choose_heads(&changes, &choices, &room);
        assert!(first.timeline.filament_swaps >= 20, "the tall blocks share a head: {} swaps", first.timeline.filament_swaps);
        assert_eq!(choice.given.swaps, first.timeline.filament_swaps as usize, "weighed by the slice's own changes");
        let tools = state.colors.tools.clone();
        assert!(take_heads(&mut state, &tools, &choice), "a colour moves: {choice:?}");
        let mapped = printing.refresh_printer(&mut state, Some(&first_ev)).expect("the plate maps anew");
        let moved: Vec<&colours::Row> = state.colors.rows.iter().filter(|r| r.moved.as_ref().is_some_and(|m| !m.stays)).collect();
        assert!(!moved.is_empty() && moved.iter().all(|r| r.how == colours::How::Auto && r.moved.as_ref().unwrap().why().contains("saving")), "{:?}", state.colors.rows);
        let mut second_ev = (*ev).clone();
        second_ev.set_mapped(Arc::new((*mapped).clone()));
        let second_ev = Arc::new(second_ev);
        check_plate_wait(&mut printing, &mut state, &first_ev, &second_ev);
        let (planned, traced) = (engine.parts_planned(), engine.traced_count());
        let (second, changes) = sliced_and_changes(&engine, &rx, &second_ev, &state);
        assert_eq!((engine.parts_planned(), engine.traced_count()), (planned, traced), "the nozzles are alike: only the writer runs again");
        assert_eq!(second.timeline.filament_swaps as usize, choice.count.swaps, "the slice swaps as weighed");
        assert!(second.timeline.filament_swaps * 2 < first.timeline.filament_swaps, "{} swaps, from {}", second.timeline.filament_swaps, first.timeline.filament_swaps);
        assert!(second.stats.total_time + HEADS_SAVE < first.stats.total_time, "{} s, from {} s", second.stats.total_time, first.stats.total_time);
        let (choices, room) = printing.head_choices.clone();
        let again = choose_heads(&changes, &choices, &room);
        let tools = state.colors.tools.clone();
        assert!(!take_heads(&mut state, &tools, &again), "nothing moves again: {again:?}");
    }
}

/// The status of a plate whose patterns still grow: how many of their steps they have.
fn growing_status(ev: &Evaluated) -> SliceStatus {
    let (step, of) = ev.patterns_progress();
    SliceStatus::Running { progress: (step as f64 / of.max(1) as f64) as f32, stage: format!("{GROWING}: step {step} of {of}") }
}

/// The desktop Sliced view's far tier (View › Sliced Detail: Fast) against its beads drawn whole,
/// on the native GPU. A patch of top skin sliced to beads ([`build_preview`]), seen across its
/// lines in a parallel projection at 2 pixels a mm, its beads under a pixel across, is drawn into
/// the aux G-buffer as the viewport draws it ([`fab::viewport::print::draw_print`]): once in its
/// tiers (the far tier's quads) and once whole at nine times the pixels across and down (the same
/// rays). The far pixels inside the patch are held against the mean of their 81 whole ones in
/// linear light, the picture a display nine times as fine would show.
#[cfg(test)]
mod far_tier_shading {
    use super::*;
    use fab::fab_sdf_tiles::draw::TileLight;
    use fab::viewport::print::{draw_print, DrawFabFarAux, DrawFabHeadAux, DrawFabPathAux, FarTiers, PrintFrame, PrintPalette};
    use makepad_widgets::makepad_platform::makepad_hal::{backend_id, open_flags, Hal, OpenReq};
    // the draw rectangle, not the machine's bed (`super`'s `Rect`)
    use makepad_widgets::Rect;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    /// The far picture's device pixels, and how many times as fine the whole one is.
    const FAR: (usize, usize) = (40, 30);
    const FINE: usize = 9;
    /// The patch: `LINES` lines along x, each `LINE_MM` long in 1 mm moves (a bead each line): one
    /// run long enough to draw far (`FarTiers` draws a shorter far range near), its middle in view.
    const LINES: usize = 480;
    const LINE_MM: usize = 14;
    const LINE_W: f32 = 0.42;
    const LINE_H: f32 = 0.2;
    /// A pixel of the composite that no bead covers.
    const UNDRAWN: [u8; 3] = [255, 0, 255];
    /// How far the far picture's mean may lie from the whole one's, in 8-bit levels.
    const BAR: f64 = 1.5;

    struct Case {
        name: &'static str,
        /// The lines' spacing over their width.
        spacing: f32,
        /// Toward the sun, in Fab's Z-up frame.
        sun: [f32; 3],
        /// The sun's and the sky's light.
        light: (f32, f32),
        /// The shader maps its light itself (the legacy lane), else the composite's curve does
        /// (the HDR lane; here the identity).
        mapped: bool,
        /// How far its far picture's mean may lie from the whole one's, in 8-bit levels.
        bar: f64,
    }

    const CASES: [Case; 3] = [
        // step 2b: the far tier on the desktop, lines at their nominal spacing
        Case { name: "A: lines a width apart, lit from the eye's side", spacing: 1.0, sun: [0.0, -0.5, 0.866], light: (0.72, 0.28), mapped: true, bar: BAR },
        // F1: each skin bead creases against the line beside it, here a slicer's spacing
        Case { name: "B: lines a slicer's spacing apart", spacing: 0.898, sun: [0.0, -0.5, 0.866], light: (0.72, 0.28), mapped: true, bar: BAR },
        // the linear-light far: on the HDR lane a far bead's taps average in linear light
        Case { name: "D: the HDR lane under a hard light", spacing: 0.898, sun: [0.0, 0.6, 0.8], light: (0.9, 0.04), mapped: false, bar: BAR },
    ];

    /// The patch's lines, `spacing` widths apart, sliced to beads as a top skin.
    fn patch(spacing: f32) -> Arc<PrintPreview> {
        let mut moves = Vec::new();
        let mut t = 0.0;
        for line in 0..LINES {
            let y = 50.0 + line as f32 * spacing * LINE_W;
            for k in 0..LINE_MM {
                let x = 142.0 + k as f32;
                moves.push(MoveRecord { kind: MoveKind::Extrude, role: Role::TopSurface, width: LINE_W, height: LINE_H, e: 0.04, p0: [x, y, LINE_H], p1: [x + 1.0, y, LINE_H], layer: 0, z_layer: LINE_H, t_start: t, t_end: t + 0.02, filament: 1, ..Default::default() });
                t += 0.02;
            }
        }
        build_preview(&moves, &Default::default(), vec![[0.62, 0.62, 0.62, 1.0]], &Machine::dual_nozzle_350(), &Default::default())
    }

    /// The camera across the patch of `spacing`: parallel, 40 degrees up, at 2 pixels a mm.
    fn camera(spacing: f32) -> fab::Camera {
        let target = vec3(0.149, (50.0 + (LINES - 1) as f32 * spacing * LINE_W * 0.5) * 0.001, 0.0001);
        let (c, s) = (40f32.to_radians().cos(), 40f32.to_radians().sin());
        fab::Camera { eye: target + vec3(0.0, -c, s) * 0.2, target, up: vec3(0.0, 0.0, 1.0), ortho: true, ortho_height: FAR.1 as f32 * 0.0005, near: 0.001, far: 1.0, ..Default::default() }
    }

    /// One picture of a case: its aux pass and what draws into it.
    struct Target {
        pass: DrawPass,
        list: DrawList,
        aux: Texture,
        _depth: Texture,
        size: (usize, usize),
        tiers: FarTiers,
        palette: PrintPalette,
        path: Box<DrawFabPathAux>,
        far: Box<DrawFabFarAux>,
        head: Box<DrawFabHeadAux>,
        /// The far ranges the tiers drew (`FarTiers::flips`).
        far_ranges: usize,
    }

    impl Target {
        fn new(cx: &mut Cx, size: (usize, usize)) -> Target {
            let aux = Texture::new_with_format(cx, TextureFormat::RenderRGBAf32 { size: TextureSize::Auto, initial: true });
            let depth = Texture::new_with_format(cx, TextureFormat::DepthD32 { size: TextureSize::Auto, initial: true });
            let pass = DrawPass::new_with_name(cx, "far tier test");
            pass.set_color_texture(cx, &aux, DrawPassClearColor::ClearWith(vec4(0.0, 0.0, 0.0, 0.0)));
            pass.set_depth_texture(cx, &depth, DrawPassClearDepth::ClearWith(1.0));
            // the camera set in each paint, not a 2D pass's own
            pass.set_keep_camera_matrix(cx, true);
            let (path, far, head) = cx.with_vm(|vm| (Box::new(DrawFabPathAux::script_new_with_default(vm)), Box::new(DrawFabFarAux::script_new_with_default(vm)), Box::new(DrawFabHeadAux::script_new_with_default(vm))));
            Target { pass, list: DrawList::new(cx), aux, _depth: depth, size, tiers: FarTiers::default(), palette: PrintPalette::default(), path, far, head, far_ranges: 0 }
        }

        fn ready(&self, cx: &Cx) -> bool {
            [self.path.draw_vars.draw_shader_id, self.far.draw_vars.draw_shader_id].iter().all(|id| id.is_some_and(|id| cx.draw_shader_ready(id, true)))
        }

        /// The print into the aux pass, as the viewport draws it: in its tiers or `exact`.
        fn paint(&mut self, cx: &mut Cx2d, frame: &PrintFrame, light: &TileLight, camera: &fab::Camera, exact: bool) {
            let size = dvec2(self.size.0 as f64, self.size.1 as f64);
            let (view, projection) = (camera.view(), camera.projection((size.x / size.y) as f32));
            cx.make_child_pass(&self.pass);
            cx.begin_pass(&self.pass, Some(1.0));
            self.pass.set_size(cx, size);
            self.pass.set_camera(cx.cx, view, projection);
            self.list.begin_always(cx);
            cx.begin_root_turtle(size, Layout::flow_overlay());
            {
                let cx3d = &mut Cx3d::new(cx.cx);
                cx3d.begin_scene_3d(SceneState3D { time: 0.0, camera_pos: camera.eye, view, projection, viewport_rect: Rect { pos: dvec2(0.0, 0.0), size } });
                let previous = cx3d.set_scene_world_transform_3d(Mat4f::identity());
                draw_print(cx3d, frame, &mut self.palette, light, (&view, &projection), (&mut self.path, &mut self.far, &mut self.tiers, exact), &mut self.head);
                if let Some((_, _, ranges)) = self.tiers.flips() {
                    self.far_ranges = ranges[1];
                }
                if let Some(previous) = previous {
                    let _ = cx3d.set_scene_world_transform_3d(previous);
                }
                cx3d.end_scene_3d();
            }
            cx.end_pass_sized_turtle();
            self.list.end(cx);
            cx.end_pass(&self.pass);
        }
    }

    struct Drawn {
        frame: PrintFrame,
        light: TileLight,
        camera: fab::Camera,
        far: Target,
        whole: Target,
    }

    /// Where a case's whole picture and its far one sit in the window (device pixels).
    fn places(i: usize) -> [(usize, usize); 2] {
        let (x, y) = ((i % 3) * (FAR.0 * (FINE + 1) + 16), (i / 3) * (FAR.1 * FINE + 8));
        [(x, y), (x + FAR.0 * FINE + 8, y)]
    }
    const WINDOW: (usize, usize) = (3 * (FAR.0 * (FINE + 1) + 16), 2 * (FAR.1 * FINE + 8));

    #[test]
    fn far_skins_read_as_their_beads_drawn_whole() {
        std::env::set_var("MAKEPAD_HAL_BACKEND", "offscreen");
        let _gpu = crate::live_slice::HUB_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let _cx = crate::cx_owned();
        let mut previews: Vec<(f32, Arc<PrintPreview>)> = Vec::new();
        for c in &CASES {
            if !previews.iter().any(|(s, _)| *s == c.spacing) {
                previews.push((c.spacing, patch(c.spacing)));
            }
        }
        let done = Rc::new(Cell::new(false));
        let done_in_draw = done.clone();
        let frames = Rc::new(Cell::new((0u32, 0u32)));
        let frames_in_draw = frames.clone();
        // per case, the far ranges its far picture drew
        let far_drawn = Rc::new(RefCell::new(Vec::new()));
        let far_drawn_in_draw = far_drawn.clone();
        let mut state: Option<(WindowHandle, DrawPass, DrawList2d, DrawQuad, Vec<Drawn>, u32)> = None;
        let cx = Rc::new(RefCell::new(Cx::new(Box::new(move |cx, event| {
            if matches!(event, Event::Startup) {
                let show = cx.with_vm(|vm| {
                    makepad_widgets::script_mod(vm);
                    fab::script_mod(vm);
                    // the aux G-buffer's bead colour as it is (`aux_pixel`): what the composite
                    // puts on the display, its curve the identity; no bead: magenta
                    let show = vm.eval(script! {
                        use mod.pod.*
                        use mod.math.*
                        use mod.shader.*
                        use mod.draw.*
                        DrawQuad{
                            aux: texture_2d(float)
                            pixel: fn() {
                                let g = self.aux.sample_nearest(self.pos)
                                if g.y > -1.5 {
                                    return vec4(1.0, 0.0, 1.0, 1.0)
                                }
                                let v = 0.0 - 2.0 - g.y
                                let r = floor(v / 4096.0)
                                let s = floor(g.z / 4096.0)
                                let e = vec3(r, v - r * 4096.0, g.z - s * 4096.0) * (4.0 / 4095.0)
                                let c = e * e / (1.0 + 15.0 * s)
                                return vec4(c.x, c.y, c.z, 1.0)
                            }
                        }
                    });
                    DrawQuad::script_from_value(vm, show)
                });
                let mut window = WindowHandle::new(cx);
                window.configure_window(cx, dvec2(WINDOW.0 as f64, WINDOW.1 as f64), dvec2(0.0, 0.0), false, "Far tier test".into());
                let window_pass = DrawPass::new(cx);
                window.set_pass(cx, &window_pass);
                window_pass.set_window_clear_color(cx, vec4(1.0, 0.0, 1.0, 1.0));
                let drawn = CASES.iter().map(|c| {
                    let pv = previews.iter().find(|(s, _)| *s == c.spacing).expect("a patch of the case's spacing").1.clone();
                    let mut app = fab::AppState::default();
                    app.print.set_preview(pv);
                    fab::print::apply(&mut app.print, &PrintAction::Preview(true));
                    let frame = PrintFrame::of(&app, None).expect("the slice view draws");
                    let s = c.sun;
                    let n = (s[0] * s[0] + s[1] * s[1] + s[2] * s[2]).sqrt();
                    // the renderer's frame is Y up
                    let light = TileLight { dir: [s[0] / n, s[2] / n, -s[1] / n], color: [c.light.0; 3], sky: [c.light.1; 3], ground: [c.light.1; 3], up: [0.0, 1.0, 0.0], display: c.mapped, ..TileLight::default() };
                    Drawn { frame, light, camera: camera(c.spacing), far: Target::new(cx, FAR), whole: Target::new(cx, (FAR.0 * FINE, FAR.1 * FINE)) }
                }).collect();
                state = Some((window, window_pass, DrawList2d::new(cx), show, drawn, 0));
                cx.redraw_all();
            }
            if matches!(event, Event::NextFrame(_)) {
                cx.redraw_all();
            }
            if let Event::Draw(event) = event {
                let (_, window_pass, list, show, drawn, ready_frames) = state.as_mut().unwrap();
                // every pipeline in: a draw whose pipeline still builds is left out of its frame
                let ready = drawn.iter().all(|d| d.far.ready(cx) && d.whole.ready(cx)) && show.draw_vars.draw_shader_id.is_some_and(|id| cx.draw_shader_ready(id, true)) && cx.pipelines_pending() == 0;
                let mut draw = CxDraw::new(cx, event);
                let mut cx = Cx2d::new(&mut draw);
                cx.begin_pass(window_pass, None);
                list.begin_always(&mut cx);
                cx.begin_root_turtle(dvec2(WINDOW.0 as f64, WINDOW.1 as f64), Layout::flow_overlay());
                for d in drawn.iter_mut() {
                    d.far.paint(&mut cx, &d.frame, &d.light, &d.camera, false);
                    d.whole.paint(&mut cx, &d.frame, &d.light, &d.camera, true);
                }
                let dpi = cx.current_dpi_factor();
                for (i, d) in drawn.iter().enumerate() {
                    for (t, at) in [&d.whole, &d.far].into_iter().zip(places(i)) {
                        show.draw_vars.set_texture(0, &t.aux);
                        show.new_draw_call(&mut cx);
                        show.draw_abs(&mut cx, Rect { pos: dvec2(at.0 as f64 / dpi, at.1 as f64 / dpi), size: dvec2(t.size.0 as f64 / dpi, t.size.1 as f64 / dpi) });
                    }
                }
                cx.end_pass_sized_turtle();
                list.end(&mut cx);
                cx.end_pass(window_pass);
                *ready_frames = if ready { *ready_frames + 1 } else { 0 };
                done_in_draw.set(*ready_frames >= 3);
                frames_in_draw.set((frames_in_draw.get().0 + 1, *ready_frames));
                *far_drawn_in_draw.borrow_mut() = drawn.iter().map(|d| d.far.far_ranges).collect();
                if !done_in_draw.get() {
                    let _ = cx.new_next_frame();
                }
            }
        }))));
        cx.borrow_mut().init_cx_os();
        let (w, rgba) = capture(&cx, &done, &frames);
        let px = |x: usize, y: usize| -> [u8; 3] {
            let o = (y * w + x) * 4;
            [rgba[o], rgba[o + 1], rgba[o + 2]]
        };
        let decode = |b: u8| (b as f64 / 255.0).powf(2.2);
        let encode = |v: f64| v.powf(1.0 / 2.2) * 255.0;
        let far_drawn = far_drawn.borrow().clone();
        for (i, case) in CASES.iter().enumerate() {
            assert!(far_drawn[i] > 0, "{}: the patch's run draws far", case.name);
            let [(wx, wy), (fx, fy)] = places(i);
            // the patch in the far picture, three pixels in from its edges
            let (mut lo, mut hi) = ((FAR.0, FAR.1), (0, 0));
            for y in 0..FAR.1 {
                for x in 0..FAR.0 {
                    if px(fx + x, fy + y) != UNDRAWN {
                        lo = (lo.0.min(x), lo.1.min(y));
                        hi = (hi.0.max(x), hi.1.max(y));
                    }
                }
            }
            let (mut n, mut far_sum, mut whole_sum) = (0usize, 0.0, 0.0);
            for y in lo.1 + 3..hi.1.saturating_sub(2) {
                for x in lo.0 + 3..hi.0.saturating_sub(2) {
                    let far = px(fx + x, fy + y);
                    let block: Vec<[u8; 3]> = (0..FINE * FINE).map(|k| px(wx + x * FINE + k % FINE, wy + y * FINE + k / FINE)).collect();
                    if far == UNDRAWN || block.contains(&UNDRAWN) {
                        continue;
                    }
                    // the patch is grey: its green stands for all three
                    far_sum += far[1] as f64;
                    whole_sum += encode(block.iter().map(|p| decode(p[1])).sum::<f64>() / block.len() as f64);
                    n += 1;
                }
            }
            assert!(n > 100, "{}: {n} pixels of the patch compared", case.name);
            let (far, whole) = (far_sum / n as f64, whole_sum / n as f64);
            println!("far tier: {}: {n} pixels, far {far:.1}, whole {whole:.1} (8-bit), far - whole {:+.2} levels", case.name, far - whole);
            assert!((far - whole).abs() <= case.bar, "{}: the far beads read {:+.2} levels from the beads drawn whole (bar {})", case.name, far - whole, case.bar);
        }
    }

    #[test]
    fn four_virtual_cameras_reuse_their_pictures() {
        use fab::ui::camera::{CameraTurns, FabPrinterCamera, VirtualView};
        std::env::set_var("MAKEPAD_HAL_BACKEND", "offscreen");
        let preview = slice_bench::with_demo_slice(|sliced, _| sliced.preview.clone());
        let _gpu = crate::live_slice::HUB_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let _cx = crate::cx_owned();
        let machine = Machine::dual_nozzle_350();
        let plate = fab::PlateInfo { bed_min: machine.bed.min, bed_max: machine.bed.max, max_height: machine.max_height, ..Default::default() };

        // Pick two instants *inside* an extrusion in the final third, and the next bead start.
        let (first, within, next) = {
            let mut late = preview.beads.each_in_time().skip_while(|b| (bead::t0(b) as f64) < preview.total * 0.7).peekable();
            loop {
                let b = late.next().expect("the demo has late extrusions");
                let start = bead::t0(b) as f64;
                let next = bead::t0(late.peek().expect("another bead follows")) as f64;
                let length = next.min(bead::t1(b) as f64) - start;
                if length > 0.0 { break (start + length * 0.25, start + length * 0.75, next) }
            }
        };
        let mut times = [first, preview.total * 0.8, preview.total * 0.85, preview.total * 0.9];
        let starts = [next, times[1], times[2], times[3]].map(|mut after| {
            let mut beads = preview.beads.each_in_time().map(|b| bead::t0(b) as f64);
            std::array::from_fn::<_, 6, _>(|_| {
                after = beads.find(|&t| t > after).expect("the demo has six more distinct bead starts");
                after
            })
        });
        let done = Rc::new(Cell::new(false));
        let frames = Rc::new(Cell::new((0, 0)));
        let (finished, drawn) = (done.clone(), frames.clone());
        let mut state: Option<(WindowHandle, DrawPass, DrawList2d, [(FabPrinterCamera, VirtualView); 4])> = None;
        let mut viewport: Option<Target> = None;
        let mut turns = CameraTurns::default();
        let mut phase = 0;
        let mut step = 0;
        let mut recorded_at = [0; 4];
        let mut refused_in_a_row = 0;
        let size = dvec2(640.0, 360.0);
        let cx = Rc::new(RefCell::new(Cx::new(Box::new(move |cx, event| {
            if matches!(event, Event::Startup) {
                cx.with_vm(|vm| { makepad_widgets::script_mod(vm); fab::script_mod(vm); });
                let mut window = WindowHandle::new(cx);
                window.configure_window(cx, size, dvec2(0.0, 0.0), false, "Printer cameras".into());
                let pass = DrawPass::new(cx);
                window.set_pass(cx, &pass);
                let cameras = std::array::from_fn(|_| {
                    let camera = cx.with_vm(|vm| FabPrinterCamera::script_new_with_default(vm));
                    let view = VirtualView::new(cx, vec4(0.06, 0.07, 0.08, 1.0));
                    view.pass().set_gpu_timing_enabled(cx, true);
                    (camera, view)
                });
                state = Some((window, pass, DrawList2d::new(cx), cameras));
                cx.redraw_all();
            }
            if matches!(event, Event::NextFrame(_)) { cx.redraw_all(); }
            if let Event::Draw(event) = event {
                let (_, pass, list, cameras) = state.as_mut().unwrap();
                // Shader compilation or a GPU upload can request a repaint independently of
                // the widget. Measure the cached frame once all four first pictures are complete.
                let steady = matches!(phase, 1 | 4 | 6) && cameras.iter().zip(recorded_at)
                    .all(|((_, v), since)| v.pass().painted_whole_since(cx, since));
                if phase == 2 { times[0] = within; }
                if phase == 3 { times[0] = next; }
                if phase == 5 { times = starts.map(|s| s[0]); }
                if phase == 7 {
                    times = starts.map(|s| s[1]);
                    times[0] = starts[0][1 + step];
                }
                let before = cameras.each_ref().map(|(_, v)| v.recordings());
                let dirty_before = cameras.each_ref().map(|(_, v)| cx.passes[v.pass().draw_pass_id()].paint_dirty);
                let mut draw = CxDraw::new(cx, event);
                let mut cx = Cx2d::new(&mut draw);
                cx.begin_pass(pass, None);
                list.begin_always(&mut cx);
                cx.begin_root_turtle(size, Layout::flow_overlay());
                if phase == 4 {
                    // Let the viewport's first-use uploads and pipelines settle before the burst.
                    cx.make_child_pass(&viewport.as_ref().unwrap().pass);
                }
                let start = std::time::Instant::now();
                for (i, (host, view)) in cameras.iter_mut().enumerate() {
                    let card = size * 0.5;
                    let rect = Rect { pos: dvec2((i % 2) as f64 * card.x, (i / 2) as f64 * card.y), size: card };
                    assert!(view.draw(&mut cx, &mut turns, host, rect, &preview, times[i], fab_plate::ColourMode::Model, &plate),
                        "camera {i} presents its demo slice");
                }
                let elapsed = start.elapsed();
                let counts = cameras.each_ref().map(|(_, v)| v.recordings());
                let dirty = cameras.each_ref().map(|(_, v)| cx.passes[v.pass().draw_pass_id()].paint_dirty);
                let records = (0..4).filter(|&i| counts[i] != before[i]).count();
                assert!(records <= 1, "at most one virtual camera records per drawing: {before:?} -> {counts:?}");
                let items = &cx.draw_lists[list.draw_list_id()].draw_items;
                for (i, (_, view)) in cameras.iter().enumerate() {
                    let texture = &cx.passes[view.pass().draw_pass_id()].color_textures[0].texture;
                    let presents = (0..items.len()).any(|j| items[j].kind.draw_call()
                        .is_some_and(|call| call.texture_slots[0].as_ref() == Some(texture)));
                    assert_eq!(presents, counts[i] != 0, "camera {i} presents its held picture and leaves its first wait blank");
                    if counts[i] != before[i] {
                        recorded_at[i] = cx.repaint_id();
                        view.pass().set_gpu_time_tag(&cx, phase as u64);
                    }
                }
                if phase == 3 {
                    // Record the viewport after the timed paints. Fit the same deposited cut to
                    // a small target so each stream has beads in the viewport's far tier.
                    let target = viewport.insert(Target::new(&mut cx, FAR));
                    let [min, max] = preview.bounds.map(|p| vec3(p[0], p[1], p[2]) * 0.001);
                    let mut eye = fab::Camera::default();
                    eye.frame_bounds(&fab::api::Aabb { min, max }, FAR.0 as f32 / FAR.1 as f32);
                    let frame = PrintFrame::camera(preview.clone(), next, fab_plate::ColourMode::Model, None).unwrap();
                    target.paint(&mut cx, &frame, &TileLight::default(), &eye, false);

                    // Both far shaders use the same quad mesh. Identify each stream by the
                    // retained bead buffer and count the instance ranges its draw submitted.
                    let quad = target.far.draw_vars.geometry_id.expect("the far tier has a quad mesh");
                    let far_counts = |pass: &DrawPass| {
                        let id = cx.passes[pass.draw_pass_id()].main_draw_list_id.unwrap();
                        let items = &cx.draw_lists[id].draw_items;
                        BeadStream::ALL.map(|stream| {
                            let buffer = preview.beads.of(stream).id();
                            (0..items.len()).map(|i| &items[i]).filter(|item| {
                                item.kind.draw_call().and_then(|c| c.geometry_id) == Some(quad)
                                    && item.retained_instances.as_ref().is_some_and(|r| r.id() == buffer)
                            }).map(|item| {
                                if item.instance_ranges.is_empty() { item.retained_instance_count }
                                else { item.instance_ranges.iter().map(|r| r.len()).sum() }
                            }).sum::<usize>()
                        })
                    };
                    let viewport_far = far_counts(&target.pass);
                    assert!(viewport_far.iter().all(|&n| n > 0), "the viewport draws all three far streams at the camera cut: {viewport_far:?}");
                    let [surface, lining, core] = far_counts(cameras[0].1.pass());
                    assert!(surface > 0, "the virtual camera draws far surface beads");
                    assert_eq!([lining, core], [0, 0], "the virtual camera omits far Lining and Core instances");
                }
                cx.end_pass_sized_turtle();
                list.end(&mut cx);
                cx.end_pass(pass);
                match phase {
                    0 | 5 => {
                        let fill = phase == 0;
                        let baseline = if fill { [0; 4] } else { [2, 1, 1, 1] };
                        let expected = std::array::from_fn(|i| baseline[i] + u64::from(i <= step));
                        assert_eq!(counts, expected, "fill and burst record one camera in oldest-ticket order");
                        assert_eq!(dirty, std::array::from_fn(|i| i == step || (fill && dirty_before[i])),
                            "only the granted camera marks its pass to paint");
                        let label = if fill { "fill" } else { "burst" };
                        log!("camera turns {label} frame {}, UI: {:.3} ms", step + 1, elapsed.as_secs_f64() * 1000.0);
                        step += 1;
                        if step == 4 { phase += 1; step = 0; }
                    }
                    1 | 6 if steady => {
                        let fill = phase == 1;
                        assert_eq!(counts, if fill { [1; 4] } else { [3, 2, 2, 2] }, "unchanged cameras reuse all four recordings");
                        assert_eq!(dirty, [false; 4], "presenting four cached pictures marks no camera pass to paint");
                        log!("four cached cameras, UI: {:.3} ms", elapsed.as_secs_f64() * 1000.0);
                        let gpu = cameras.each_ref().map(|(_, v)| v.pass().take_gpu_time_samples(&cx)
                            .into_iter().rev().find(|(tag, _)| *tag == (phase - 1) as u64).map(|(_, ms)| ms));
                        let label = if fill { "fill" } else { "burst" };
                        log!("camera turns {label}, last completed GPU paint per frame (ms): {gpu:?}");
                        phase += 1;
                    }
                    2 => {
                        assert_eq!(counts, [1; 4], "advancing inside one bead reuses the recording");
                        assert_eq!(dirty, [false; 4], "an intra-bead report requests no paint");
                        phase += 1;
                    }
                    3 => {
                        assert_eq!(counts, [2, 1, 1, 1], "crossing a bead start records only the advancing printer");
                        assert_eq!(dirty, [true, false, false, false], "only the advancing camera requests a paint");
                        phase += 1;
                    }
                    4 if steady && viewport.as_ref().unwrap().pass.painted_whole_since(&cx, 0) => { phase += 1; }
                    7 => {
                        if counts[0] == before[0] { refused_in_a_row += 1; } else { refused_in_a_row = 0; }
                        assert!(refused_in_a_row <= 3, "the continuously advancing camera waits at most three drawings");
                        step += 1;
                        if step == 4 {
                            assert_eq!(&counts[1..], &[3; 3], "a continuously advancing camera cannot starve the other three beyond four drawings");
                        }
                        if step == 5 { finished.set(true); }
                    }
                    _ => {}
                }
                drawn.set((drawn.get().0 + 1, phase));
                if !finished.get() { let _ = cx.new_next_frame(); }
            }
        }))));
        cx.borrow_mut().init_cx_os();
        capture(&cx, &done, &frames);
    }

    /// The window's pixels once `done` holds and the frame that drew it finished: its width and
    /// RGBA rows, top first. Only a hang fails it: five minutes from the first frame drawn (a
    /// cold or loaded box's shader builds come before it).
    fn capture(cx: &Rc<RefCell<Cx>>, done: &Cell<bool>, frames: &Cell<(u32, u32)>) -> (usize, Vec<u8>) {
        let hal = Hal::global();
        let mut req = OpenReq::from_env(0, open_flags::OFFSCREEN_WINDOWS, 1.0);
        req.flags |= open_flags::OFFSCREEN_WINDOWS | open_flags::RETAIN_WINDOWS;
        let info = hal.open(&req);
        assert_ne!(info.backend, backend_id::HEADLESS, "pixel comparisons require a GPU");
        cx.borrow_mut().hal_attach_session(info);
        let mut deadline = None;
        let pixels = loop {
            if frames.get().0 > 0 {
                let at = *deadline.get_or_insert_with(|| std::time::Instant::now() + std::time::Duration::from_secs(300));
                let (drawn, ready) = frames.get();
                assert!(std::time::Instant::now() < at, "the passes never painted whole: {drawn} frames drawn, the last {ready} with every pipeline in, {} pipelines pending", cx.borrow().pipelines_pending());
            }
            let mut cx = cx.borrow_mut();
            assert!(cx.hal_session_turn(1_000_000, true));
            let submitted = cx.frame_submission_serial();
            if done.get() && submitted != 0 && cx.frame_completion_serial() == submitted && cx.pipelines_pending() == 0 {
                if let Some(pixels) = hal.window_pixels(info.session, None) {
                    break pixels;
                }
            }
        };
        hal.close(info.session);
        (pixels.w as usize, pixels.rgba)
    }
}
