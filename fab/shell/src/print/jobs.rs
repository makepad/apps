//! Print commits a job. Pressing it saves what goes to the printer under
//! `~/.makepad/fab/jobs/<id>/`, unchangeable, and Monitor shows these jobs, never the plate's slice:
//! nothing in Design can reach one. One folder holds
//!
//! - `package.gcode.3mf`: the bytes the print safety check passed. The job's preview is made from
//!   this G-code on a worker when it is wanted, so what Monitor draws is what the printer has;
//! - `job.txt`: what the slice planned (time, layers, filament) and what the package doesn't say;
//! - `plate/`: the plate's scripts as they were;
//! - `state.txt`: where the job is now, the only file that changes.
//!
//! A job appears whole when its temporary folder is renamed. Startup discards interrupted writes.
//! Committed jobs stay in the print history until the person deletes them. A queued job starts
//! only when the person confirms the plate has been cleared.
//!
//! Main-loop cost: none on disk (workers read and write it), a list scan per printer report.

use super::*;
use fab_printer_link::model::TrayId;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const FORMAT: &str = "1";
const PACKAGE: &str = "package.gcode.3mf";
const META: &str = "job.txt";
const STATE: &str = "state.txt";
const PLATE: &str = "plate";
const UNFINISHED: &str = ".writing-";

/// Wall-clock seconds, what outlives a run of the app.
pub fn wall_now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

/// Where the jobs live.
pub fn jobs_dir() -> PathBuf {
    makepad_fab_settings::home::user_home().join(".makepad").join("fab").join("jobs")
}

/// What a job is and what its slice planned; never changes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JobMeta {
    /// Its folder's name.
    pub id: String,
    pub name: String,
    /// The confirmed destination. Jobs saved before the printer picker have none.
    pub printer: Option<String>,
    /// Whether the confirmed destination uses a virtual printer link.
    pub virtual_printer: bool,
    /// The serial suffix shown by the confirmed link, when available.
    pub serial_suffix: Option<String>,
    /// Wall-clock seconds of the press of Print.
    pub created: f64,
    /// Print time and printer routines inside it (s).
    pub planned: f64,
    pub prepare: f64,
    pub layers: u32,
    pub max_z: f64,
    pub tool_changes: u32,
    /// Per filament slot.
    pub filament_mm: Vec<f64>,
    pub filament_g: Vec<f64>,
    /// The filaments it prints with (0-based), and the tray that feeds each slot.
    pub used: Vec<usize>,
    pub trays: Vec<Option<TrayId>>,
    /// Print-side colours, including the print colour's fallback for an unknown tray.
    pub colours: Vec<[f32; 4]>,
    /// Tools whose Material preview shows red in place of their print-side colour.
    pub unknown_spool_tools: Vec<usize>,
    /// Empty for a job saved before model palettes.
    pub model_colours: Vec<[f32; 4]>,
    pub shared_colour_tools: Vec<usize>,
    /// The plate's object label ids, by part: the G-code names parts by these.
    pub labels: Vec<u32>,
    pub flow_calibration: bool,
    pub timelapse: bool,
    /// The toolhead: X of the right nozzle from the left one, the idle nozzle's lift (mm), and
    /// whether it has two nozzles.
    pub head_offset_x: f32,
    pub head_lift: f32,
    pub dual: bool,
    /// The printer it was sliced for, and the installed presets a safety check reads again when a
    /// queued job starts.
    pub printer_model: String,
    pub machine_preset: String,
    pub filament_presets: Vec<String>,
    /// The printability errors the person printed it anyway with (the confirm card's tick).
    pub accepted: Vec<Accepted>,
    /// Where it waits for items to be placed into it.
    pub pauses: Vec<super::PreviewPause>,
    /// The heat-set inserts to melt in after it.
    pub melts: Vec<super::PreviewMelt>,
}

/// Printability errors of one kind a job was started with: the first layer they are on
/// (0-based) and how many there were.
#[derive(Clone, Debug, PartialEq)]
pub struct Accepted {
    pub kind: String,
    pub layer: Option<u32>,
    pub count: u32,
}

impl Accepted {
    fn text(&self) -> String {
        format!("{}:{}:{}", self.kind, self.layer.map_or("-".to_string(), |l| l.to_string()), self.count)
    }

    fn parse(s: &str) -> Option<Accepted> {
        let mut it = s.split(':');
        let kind = it.next().filter(|k| !k.is_empty())?.to_string();
        let layer = match it.next()? { "-" => None, l => Some(l.parse().ok()?) };
        Some(Accepted { kind, layer, count: it.next()?.parse().ok()? })
    }
}

/// The one line Monitor shows for a job started with accepted printability errors (empty: none).
pub fn accepted_line(accepted: &[Accepted]) -> String {
    if accepted.is_empty() {
        return String::new();
    }
    let total: u32 = accepted.iter().map(|a| a.count).sum();
    let kinds: Vec<String> = accepted.iter().map(|a| {
        let kind = a.kind.replace('_', " ");
        match (a.count, a.layer) {
            (1, Some(l)) => format!("{kind} on layer {}", l + 1),
            (n, Some(l)) => format!("{kind} ×{n} from layer {}", l + 1),
            (1, None) => kind,
            (n, None) => format!("{kind} ×{n}"),
        }
    }).collect();
    let problems = if total == 1 { "1 accepted problem".to_string() } else { format!("{total} accepted problems") };
    format!("Started with {problems}: {}", kinds.join(", "))
}

fn numbers<T: ToString>(v: &[T]) -> String {
    v.iter().map(T::to_string).collect::<Vec<_>>().join(",")
}

fn parse_numbers<T: std::str::FromStr>(s: &str) -> Option<Vec<T>> {
    s.split(',').filter(|n| !n.is_empty()).map(|n| n.parse().ok()).collect()
}

/// A value on one line.
fn one_line(s: &str) -> String {
    s.replace(['\n', '\r'], " ")
}

/// Whether the configured model names the model in the job's slicer profile.
pub fn same_printer(configured: &str, printer_model: &str) -> bool {
    makepad_fab_settings::family::of_model(printer_model.trim()).is_some_and(|f| f.same_model(configured, printer_model))
}

impl JobMeta {
    /// Jobs saved without a destination could only have gone to a configured printer.
    pub fn for_printer(&self, choice: &PrinterChoice) -> bool {
        match (self.printer.as_deref(), choice) {
            (None, PrinterChoice::Real(_)) => true,
            (Some(id), PrinterChoice::Real(name)) => id.strip_prefix("printer:") == Some(name.as_str()),
            (Some(id), PrinterChoice::Virtual(name)) => id.strip_prefix("virtual:") == Some(name.as_str()),
            _ => false,
        }
    }

    pub(super) fn for_report(&self, printer: &PrinterChoice, report: &fab_printer_link::Snapshot) -> bool {
        self.virtual_printer == report.virtual_printer && self.for_printer(printer)
            && self.serial_suffix.as_ref().zip(report.serial_suffix.as_ref()).is_none_or(|(saved, reported)| saved == reported)
    }

    /// A new job's folder name: ordered by time, readable, unique per press.
    pub fn new_id(name: &str, created: f64) -> String {
        static LAST: AtomicU64 = AtomicU64::new(0);
        let now_ms = (created * 1000.0) as u64;
        let mut last = LAST.load(Ordering::Relaxed);
        let stamp = loop {
            let next = now_ms.max(last + 1);
            match LAST.compare_exchange_weak(last, next, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => break next,
                Err(actual) => last = actual,
            }
        };
        let safe: String = name.chars().take(48).map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
        format!("{stamp:014}-{safe}")
    }

    fn text(&self) -> String {
        let palette = |v: &[[f32; 4]]| v.iter().map(|c| numbers(c)).collect::<Vec<_>>().join(";");
        let colours = palette(&self.colours);
        let model_colours = palette(&self.model_colours);
        let shared_colour_tools = numbers(&self.shared_colour_tools);
        let trays = self.trays.iter().map(|t| t.map_or("-".to_string(), |t| format!("{}:{}", t.ams, t.slot))).collect::<Vec<_>>().join(",");
        let mut out = format!(
            "format={FORMAT}\nname={}\ncreated={}\nplanned={}\nprepare={}\nlayers={}\nmax_z={}\ntool_changes={}\nfilament_mm={}\nfilament_g={}\nused={}\ntrays={trays}\ncolours={colours}\nmodel_colours={model_colours}\nshared_colour_tools={shared_colour_tools}\nlabels={}\nflow_calibration={}\ntimelapse={}\nhead_offset_x={}\nhead_lift={}\ndual={}\nprinter_model={}\nmachine_preset={}\n",
            one_line(&self.name), self.created, self.planned, self.prepare, self.layers, self.max_z, self.tool_changes,
            numbers(&self.filament_mm), numbers(&self.filament_g), numbers(&self.used), numbers(&self.labels),
            self.flow_calibration, self.timelapse, self.head_offset_x, self.head_lift, self.dual,
            one_line(&self.printer_model), one_line(&self.machine_preset),
        );
        if !self.unknown_spool_tools.is_empty() {
            out.push_str(&format!("unknown_spool_tools={}\n", numbers(&self.unknown_spool_tools)));
        }
        if let Some(id) = &self.printer {
            out.push_str(&format!("printer={}\n", one_line(id)));
        }
        out.push_str(&format!("virtual={}\n", self.virtual_printer));
        if let Some(suffix) = &self.serial_suffix {
            out.push_str(&format!("serial={}\n", one_line(suffix)));
        }
        for preset in &self.filament_presets {
            out.push_str(&format!("filament_preset={}\n", one_line(preset)));
        }
        for a in &self.accepted {
            out.push_str(&format!("accepted={}\n", one_line(&a.text())));
        }
        for p in &self.pauses {
            out.push_str(&format!("pause={}:{}:{}\n", p.layer, p.z, one_line(&p.items.join("\t"))));
        }
        for m in &self.melts {
            out.push_str(&format!("melt={}:{}:{}:{}\n", m.count, m.iron, one_line(&m.size).replace(':', " "), one_line(&m.into)));
        }
        out
    }

    fn parse(id: &str, text: &str) -> Option<JobMeta> {
        let mut map = BTreeMap::new();
        let (mut filament_presets, mut accepted, mut pauses, mut melts) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for (key, value) in text.lines().filter_map(|l| l.split_once('=')) {
            match key {
                "filament_preset" => filament_presets.push(value.to_string()),
                "accepted" => accepted.push(Accepted::parse(value)?),
                "pause" => {
                    let mut it = value.splitn(3, ':');
                    let (layer, z, items) = (it.next()?.parse().ok()?, it.next()?.parse().ok()?, it.next()?);
                    pauses.push(super::PreviewPause { layer, z, items: items.split('\t').map(str::to_string).collect() });
                }
                "melt" => {
                    let mut it = value.splitn(4, ':');
                    let (count, iron, size, into) = (it.next()?.parse().ok()?, it.next()?.parse().ok()?, it.next()?, it.next()?);
                    melts.push(super::PreviewMelt { size: size.to_string(), count, into: into.to_string(), iron });
                }
                _ => { map.insert(key, value); }
            }
        }
        if map.get("format").copied() != Some(FORMAT) { return None; }
        let get = |key: &str| map.get(key).copied();
        let num = |key: &str| get(key).and_then(|v| v.parse::<f64>().ok());
        let trays = get("trays")?.split(',').filter(|t| !t.is_empty()).map(|t| match t {
            "-" => Some(None),
            t => t.split_once(':').and_then(|(a, s)| Some(Some(TrayId { ams: a.parse().ok()?, slot: s.parse().ok()? }))),
        }).collect::<Option<Vec<_>>>()?;
        let palette = |text: &str| text.split(';').filter(|c| !c.is_empty()).map(|c| {
            parse_numbers::<f32>(c).and_then(|c| <[f32; 4]>::try_from(c).ok())
        }).collect::<Option<Vec<_>>>();
        Some(JobMeta {
            id: id.to_string(),
            name: get("name")?.to_string(),
            printer: get("printer").map(str::to_string),
            virtual_printer: match get("virtual") {
                Some(flag) => flag.parse().ok()?,
                None => get("printer").is_some_and(|id| id.starts_with("virtual:")),
            },
            serial_suffix: get("serial").map(str::to_string),
            created: num("created")?,
            planned: num("planned")?,
            prepare: num("prepare")?,
            layers: get("layers")?.parse().ok()?,
            max_z: num("max_z")?,
            tool_changes: get("tool_changes")?.parse().ok()?,
            filament_mm: parse_numbers(get("filament_mm")?)?,
            filament_g: parse_numbers(get("filament_g")?)?,
            used: parse_numbers(get("used")?)?,
            trays,
            colours: palette(get("colours")?)?,
            unknown_spool_tools: parse_numbers(get("unknown_spool_tools").unwrap_or(""))?,
            model_colours: palette(get("model_colours").unwrap_or(""))?,
            shared_colour_tools: parse_numbers(get("shared_colour_tools").unwrap_or(""))?,
            labels: parse_numbers(get("labels")?)?,
            flow_calibration: get("flow_calibration")? == "true",
            timelapse: get("timelapse")? == "true",
            head_offset_x: num("head_offset_x")? as f32,
            head_lift: num("head_lift")? as f32,
            dual: get("dual")? == "true",
            printer_model: get("printer_model")?.to_string(),
            machine_preset: get("machine_preset")?.to_string(),
            filament_presets,
            accepted,
            pauses,
            melts,
        })
    }
}

/// Where a job is.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum JobState {
    /// Its package is being made; nothing is on disk yet.
    #[default]
    Preparing,
    /// Saved, waiting for a person to start it.
    Queued,
    /// On its way to the printer or on it.
    Sent,
    Finished,
    Failed(String),
    Stopped,
    /// It was on the printer when Fab closed and is not any more; how it ended isn't known.
    EndedUnseen,
}

impl JobState {
    pub fn ended(&self) -> bool {
        matches!(self, Self::Finished | Self::Failed(_) | Self::Stopped | Self::EndedUnseen)
    }

    pub fn label(&self) -> String {
        match self {
            Self::Preparing => "Preparing".into(),
            Self::Queued => "Queued".into(),
            Self::Sent => "Printing".into(),
            Self::Finished => "Finished".into(),
            Self::Failed(_) => "Failed".into(),
            Self::Stopped => "Stopped".into(),
            Self::EndedUnseen => "Ended while Fab was closed".into(),
        }
    }

    fn word(&self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Queued => "queued",
            Self::Sent => "sent",
            Self::Finished => "finished",
            Self::Failed(_) => "failed",
            Self::Stopped => "stopped",
            Self::EndedUnseen => "ended_unseen",
        }
    }
}

/// `state.txt`: what changes as the job goes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SavedState {
    pub state: JobState,
    pub stopped_reason: Option<String>,
    /// Wall-clock seconds it was started and ended.
    pub started: Option<f64>,
    pub ended: Option<f64>,
    /// How far it printed (s of its preview).
    pub reached: f64,
}

impl SavedState {
    fn text(&self) -> String {
        let mut out = format!("state={}\nreached={}\n", self.state.word(), self.reached);
        if let Some(t) = self.started { out.push_str(&format!("started={t}\n")); }
        if let Some(t) = self.ended { out.push_str(&format!("ended={t}\n")); }
        if let Some(why) = &self.stopped_reason { out.push_str(&format!("stopped_reason={}\n", one_line(why))); }
        if let JobState::Failed(why) = &self.state { out.push_str(&format!("why={}\n", one_line(why))); }
        out
    }

    fn parse(text: &str) -> Option<SavedState> {
        let map: BTreeMap<&str, &str> = text.lines().filter_map(|l| l.split_once('=')).collect();
        let state = match *map.get("state")? {
            "queued" => JobState::Queued,
            "sent" => JobState::Sent,
            "finished" => JobState::Finished,
            "failed" => JobState::Failed(map.get("why").copied().unwrap_or("").to_string()),
            "stopped" => JobState::Stopped,
            "ended_unseen" => JobState::EndedUnseen,
            _ => return None,
        };
        let time = |key: &str| map.get(key).and_then(|v| v.parse().ok());
        Some(SavedState { stopped_reason: map.get("stopped_reason").map(|s| s.to_string()), state, started: time("started"), ended: time("ended"), reached: time("reached").unwrap_or(0.0) })
    }
}

/// Saves a new job whole. `package` is what goes to the printer; `scripts` the plate's, by file.
pub fn commit(dir: &Path, meta: &JobMeta, state: &SavedState, package: &[u8], scripts: &BTreeMap<String, String>) -> std::io::Result<()> {
    let writing = dir.join(format!("{UNFINISHED}{}", meta.id));
    let _ = std::fs::remove_dir_all(&writing);
    std::fs::create_dir_all(writing.join(PLATE))?;
    std::fs::write(writing.join(PACKAGE), package)?;
    std::fs::write(writing.join(META), meta.text())?;
    std::fs::write(writing.join(STATE), state.text())?;
    for (file, text) in scripts {
        let name: String = file.chars().map(|c| if c == '/' || c == '\\' || c == ':' { '_' } else { c }).collect();
        std::fs::write(writing.join(PLATE).join(name), text)?;
    }
    std::fs::rename(&writing, dir.join(&meta.id))
}

/// Replaces a job's `state.txt` whole.
pub fn save_state(dir: &Path, id: &str, state: &SavedState) -> std::io::Result<()> {
    let folder = dir.join(id);
    let writing = folder.join(format!("{STATE}.new"));
    std::fs::write(&writing, state.text())?;
    std::fs::rename(writing, folder.join(STATE))
}

/// A job's package, the bytes that went to the printer.
pub fn read_package(dir: &Path, id: &str) -> std::io::Result<Vec<u8>> {
    std::fs::read(dir.join(id).join(PACKAGE))
}

pub fn remove(dir: &Path, id: &str) -> std::io::Result<()> {
    std::fs::remove_dir_all(dir.join(id))
}

/// The saved jobs, newest first. What a crash left half written goes; a folder Fab can't read
/// (a newer format) is left alone and not listed.
pub fn read_all(dir: &Path) -> Vec<(JobMeta, SavedState)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with(UNFINISHED) {
            let _ = std::fs::remove_dir_all(entry.path());
            continue;
        }
        let read = |file: &str| std::fs::read_to_string(entry.path().join(file)).ok();
        if let (Some(meta), Some(state)) = (read(META).and_then(|t| JobMeta::parse(&name, &t)), read(STATE).and_then(|t| SavedState::parse(&t))) {
            out.push((meta, state));
        }
    }
    out.sort_by(|a, b| b.0.created.total_cmp(&a.0.created).then_with(|| b.0.id.cmp(&a.0.id)));
    out
}

/// A job Fab holds: what is on disk, and its preview while it is wanted.
#[derive(Clone, Debug, Default)]
pub struct CommittedJob {
    pub stopped_reason: Option<String>,
    pub meta: Arc<JobMeta>,
    pub state: JobState,
    /// Its own preview: made from its package, or the slice its package was made from. Immutable,
    /// and nothing but the job holds it for Monitor. None: not wanted, or not there yet.
    pub preview: Option<Arc<PrintPreview>>,
    /// A worker is making `preview`.
    pub loading: bool,
    /// Why there is no preview.
    pub unreadable: Option<String>,
    /// When it was started (`Cx::time_now`, also before this run of the app), and when it ended.
    pub sent_at: f64,
    pub ended_at: Option<f64>,
    /// How far it printed (s of its preview).
    pub reached: f64,
    /// Restored from disk or left on a previous link. Only printer reports can say how it ends.
    pub restored: bool,
    /// Send number on the current link. Zero means no send tracker owns this job.
    pub link_send: u64,
}

impl CommittedJob {
    /// A job read from disk, its clock on this run's (`now`: `Cx::time_now`, `wall`: [`wall_now`]).
    pub fn restored(meta: JobMeta, saved: SavedState, now: f64, wall: f64) -> Self {
        let here = |t: f64| now - (wall - t);
        CommittedJob {
            meta: Arc::new(meta),
            sent_at: saved.started.map_or(now, here),
            ended_at: saved.ended.map(here),
            reached: saved.reached,
            state: saved.state,
            stopped_reason: saved.stopped_reason,
            restored: true,
            ..Default::default()
        }
    }

    /// `state.txt` of this job now.
    pub fn saved(&self, now: f64, wall: f64) -> SavedState {
        let there = |t: f64| wall - (now - t);
        SavedState {
            state: self.state.clone(),
            stopped_reason: self.stopped_reason.clone(),
            started: (!matches!(self.state, JobState::Queued | JobState::Preparing)).then(|| there(self.sent_at)),
            ended: self.ended_at.map(there),
            reached: self.reached,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(id: &str, created: f64) -> JobMeta {
        JobMeta {
            id: id.into(), name: "lattice cube".into(), created, planned: 7225.5, prepare: 321.25, layers: 174, max_z: 34.8, tool_changes: 3,
            printer: Some("printer:bench".into()), virtual_printer: false, serial_suffix: Some("0001".into()),
            filament_mm: vec![1000.5, 0.0], filament_g: vec![3.25, 0.0], used: vec![0], trays: vec![Some(TrayId { ams: 1, slot: 2 }), None],
            unknown_spool_tools: vec![1], model_colours: vec![[1.0, 0.0, 0.0, 1.0], [0.0, 0.2, 0.0, 1.0]], shared_colour_tools: vec![0],
            colours: vec![[0.5, 0.25, 1.0, 1.0], [0.1, 0.2, 0.3, 1.0]], labels: vec![12, 345], flow_calibration: true, timelapse: false,
            head_offset_x: 26.5, head_lift: 2.25, dual: true, printer_model: "H2D".into(), machine_preset: "a machine".into(), filament_presets: vec!["one, with a comma".into(), "two".into()],
            accepted: vec![Accepted { kind: "floating_island".into(), layer: Some(58), count: 5 }, Accepted { kind: "out_of_reach".into(), layer: None, count: 1 }],
            pauses: vec![super::super::PreviewPause { layer: 35, z: 7.2, items: vec!["M3 nut: left".into(), "Board".into()] }, super::super::PreviewPause { layer: 60, z: 12.2, items: vec!["Magnet".into()] }],
            melts: vec![super::super::PreviewMelt { size: "M2.5".into(), count: 4, into: "Base: left".into(), iron: 215 }],
        }
    }

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fab-jobs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn same_millisecond_job_ids_increase() {
        let first = JobMeta::new_id("cube", 100.5);
        let second = JobMeta::new_id("cube", 100.5);
        assert!(second > first, "successive presses in one millisecond need increasing folder ids: {first}, {second}");
    }

    #[test]
    fn printer_reports_only_reconcile_their_own_jobs() {
        use fab_printer_link::{JobStage, JobStatus, Snapshot, model::PrinterState};

        // A config and a virtual printer can share both a name and the file they print.
        let a = PrinterChoice::Real("bench".into());
        let b = PrinterChoice::Virtual("bench".into());
        let spare = PrinterChoice::Real("spare".into());
        let job = JobMeta { id: "a".into(), name: "cube".into(), layers: 4, printer_model: "Bambu Lab H2D".into(), printer: a.id(), ..Default::default() };
        let text = job.text();
        assert_eq!(text.lines().filter(|l| l.starts_with("printer=")).collect::<Vec<_>>(), ["printer=printer:bench"], "one configured destination line");
        assert_eq!(JobMeta::parse("a", &text).unwrap(), job, "the destination round-trips with the job");
        let old = JobMeta::parse("old", &text.replace("printer=printer:bench\n", "").replace("virtual=false\n", "")).unwrap();
        assert!(!old.virtual_printer, "a job saved without a destination used a real link");
        assert_eq!(old.printer, None, "an earlier record has no destination");
        let other = JobMeta { id: "b".into(), printer: b.id(), virtual_printer: true, ..job.clone() };
        assert!(text.lines().any(|l| l == "virtual=false") && other.text().lines().any(|l| l == "virtual=true"));
        assert_eq!(JobMeta::parse("b", &other.text()).unwrap(), other, "the virtual link survives a restart");
        assert_eq!(other.printer.as_deref(), Some("virtual:bench"), "the virtual destination has its own namespace");
        let legacy_virtual = JobMeta::parse("legacy-virtual", &other.text().replace("virtual=true\n", "")).unwrap();
        assert!(legacy_virtual.virtual_printer, "the virtual: destination identifies a job saved before the virtual flag");

        let sent = SavedState { state: JobState::Sent, ..Default::default() };
        let mut p = PrintState::default();
        p.set_configured_printers(&["bench".into(), "spare".into()]);
        let newer = JobMeta { id: "new".into(), ..job.clone() };
        for meta in [job, other] {
            p.follow.jobs.push(CommittedJob { meta: Arc::new(meta), state: JobState::Sent, link_send: 1, ..Default::default() });
        }
        let publish = |p: &mut PrintState, choice: &PrinterChoice, gcode: &str, model: &str, name: Option<&str>, layers: Option<u32>, tracker: Option<JobStage>| {
            p.links.picked = choice.clone();
            let state = PrinterState { gcode_state: Some(gcode.into()), job_name: name.map(str::to_string), total_layers: layers, ..Default::default() };
            let job = tracker.map(|stage| JobStatus { name: "cube".into(), file: String::new(), stage });
            p.printers.set(choice, Arc::new(Snapshot { state: Arc::new(state), model: model.into(), job, job_sends: 1, virtual_printer: matches!(choice, PrinterChoice::Virtual(_)), ..Default::default() }));
            follow::printer_changed(p, 10.0);
        };
        let report = |p: &mut PrintState, choice: &PrinterChoice, gcode: &str, tracker| publish(p, choice, gcode, "H2D", Some("cube"), Some(4), tracker);
        let running = |p: &PrintState| p.follow.job.as_ref().and_then(|j| j.id.clone());
        report(&mut p, &b, "RUNNING", None);
        assert_eq!(running(&p).as_deref(), Some("b"), "B follows its own same-named copy");
        let rows = job_list::rows(&p, 10.0);
        assert_eq!(rows[0].state, "Printing", "A's row keeps its saved state while B reports layers");
        assert!(rows[0].detail.starts_with("bench · "), "A's row names its destination");
        report(&mut p, &b, "FINISH", Some(JobStage::Finished));
        assert_eq!(p.follow.jobs[0].state, JobState::Sent, "B's terminal tracker leaves A printing");
        assert_eq!(p.follow.jobs[1].state, JobState::Finished, "B's terminal tracker finishes B's copy");
        assert_eq!(p.follow.shown().unwrap().meta.id, "b", "B's finished card shows B's saved job");
        p.follow.jobs.push(CommittedJob::restored(legacy_virtual, sent.clone(), 10.0, 10.0));
        report(&mut p, &b, "FINISH", None);
        assert_eq!(p.follow.job_of("legacy-virtual").unwrap().state, JobState::Finished, "a virtual FINISH ends its restored legacy job");

        report(&mut p, &a, "RUNNING", None);
        let before = p.follow.jobs[0].saved(10.0, 10.0);
        follow::unlink_printer(&mut p, None, 10.0);
        assert_eq!(p.follow.jobs[0].saved(10.0, 10.0), before, "switching away preserves A's saved state");
        p.follow.jobs.push(CommittedJob { meta: Arc::new(newer), state: JobState::Sent, link_send: 1, ..Default::default() });
        report(&mut p, &a, "FINISH", Some(JobStage::Finished));
        assert_eq!(p.follow.jobs[0].state, JobState::Sent, "a same-printer relink's reused send number must leave the old job printing");
        assert_eq!(p.follow.job_of("new").unwrap().state, JobState::Finished, "the new link finishes its own same-named send");
        assert_eq!(p.follow.jobs[0].link_send, 0, "unlink clears the old send number");
        report(&mut p, &a, "FINISH", Some(JobStage::Finished));
        assert_eq!(p.follow.jobs[0].state, JobState::Sent, "a repeated terminal report still belongs to the new send");
        follow::unlink_printer(&mut p, None, 10.0);

        let absent = JobMeta { id: "absent".into(), name: "bracket".into(), ..old.clone() };
        p.follow.jobs.push(CommittedJob::restored(old, sent.clone(), 10.0, 10.0));
        p.follow.jobs.push(CommittedJob::restored(absent, sent, 10.0, 10.0));
        report(&mut p, &b, "IDLE", None);
        assert_eq!(p.follow.jobs[0].state, JobState::Sent, "B's IDLE must leave A's job printing");
        assert_eq!(p.follow.job_of("old").unwrap().state, JobState::Sent, "a virtual IDLE leaves the old record alone");
        report(&mut p, &b, "FINISH", None);
        assert_eq!(p.follow.jobs[0].state, JobState::Sent, "B's same-named FINISH leaves A's detached job printing");
        assert_eq!(p.follow.job_of("old").unwrap().state, JobState::Sent, "a virtual FINISH leaves the old record alone");
        report(&mut p, &spare, "IDLE", None);
        assert_eq!(p.follow.job_of("old").unwrap().state, JobState::Sent, "a second configured printer's IDLE must leave the legacy job printing");
        assert!(p.follow.shown().is_none(), "an idle printer cannot claim a legacy job for its card");
        for (model, name, layers) in [("A1", Some("cube"), Some(4)), ("H2D", None, Some(4)), ("H2D", Some("cube"), None), ("H2D", Some("cube"), Some(3)), ("H2D", Some("other"), Some(4))] {
            publish(&mut p, &spare, "RUNNING", model, name, layers, None);
            assert_eq!(running(&p), None, "legacy running match needs its model, name and layers: {model}, {name:?}, {layers:?}");
            publish(&mut p, &spare, "FINISH", model, name, layers, None);
            assert_eq!(p.follow.job_of("old").unwrap().state, JobState::Sent, "an incomplete or mismatching FINISH leaves the legacy job printing");
        }
        report(&mut p, &spare, "RUNNING", None);
        assert_eq!(running(&p).as_deref(), Some("old"), "the matching model, name and layers pick up the legacy job");
        report(&mut p, &spare, "FINISH", None);
        assert_eq!(p.follow.job_of("old").unwrap().state, JobState::Finished, "positive evidence finishes the legacy job with two printers configured");
        assert_eq!(p.follow.job_of("absent").unwrap().state, JobState::Sent, "that finish says nothing about the other legacy job");
        p.set_configured_printers(&["spare".into()]);
        publish(&mut p, &spare, "IDLE", "A1", None, None, None);
        assert_eq!(p.follow.job_of("absent").unwrap().state, JobState::Sent, "even a sole printer of another model cannot end the legacy job");
        report(&mut p, &spare, "IDLE", None);
        assert_eq!(p.follow.job_of("absent").unwrap().state, JobState::EndedUnseen, "the sole configured printer of its model can end a legacy job by absence");
        report(&mut p, &a, "RUNNING", None);
        assert_eq!(running(&p).as_deref(), Some("a"), "A picks up its own running job on return");
        report(&mut p, &a, "FINISH", None);
        assert_eq!(p.follow.jobs[0].state, JobState::Finished, "A's new link can finish its job without the old tracker");
    }

    #[test]
    fn a_job_reads_back_as_it_was_saved() {
        let dir = folder("round");
        let m = meta("a", 100.5);
        let saved = SavedState { state: JobState::Sent, started: Some(100.5), ..Default::default() };
        commit(&dir, &m, &saved, b"the package", &BTreeMap::from([("dir/plate.fab.splash".to_string(), "text".to_string())])).unwrap();
        assert!(m.text().lines().any(|l| l == "serial=0001"), "only the panel suffix is written");
        let old = m.text().lines().filter(|l| !l.starts_with("serial=") && !l.starts_with("model_colours=") && !l.starts_with("shared_colour_tools=") && !l.starts_with("unknown_spool_tools=")).collect::<Vec<_>>().join("\n");
        let old = JobMeta::parse("old", &old).expect("legacy record");
        assert_eq!(old.serial_suffix, None, "records without a suffix still load");
        assert!(old.model_colours.is_empty() && old.unknown_spool_tools.is_empty());
        assert_eq!(old.colours, m.colours);
        let mut state = crate::AppState::default();
        state.print.follow.monitor = true;
        state.print.play.color = super::super::ColorBy::Filament;
        state.print.follow.jobs.push(CommittedJob { meta: Arc::new(old), ..Default::default() });
        state.print.follow.picked = Some("old".into());
        assert_eq!(state.colour_notice().as_deref(), Some("This job was saved before model colours were kept: it shows its spool colours"));
        state.print.follow.jobs.push(CommittedJob { meta: Arc::new(m.clone()), ..Default::default() });
        state.print.follow.picked = Some(m.id.clone());
        assert_eq!(state.colour_notice().as_deref(), Some("1 tool prints several model colours; each shows its first"));
        state.view.colours = fab_plate::ColourMode::Material;
        assert!(state.colour_notice().is_none(), "the held job has its own spool colours");
        let all = read_all(&dir);
        assert_eq!(all, vec![(m.clone(), saved)]);
        assert_eq!(read_package(&dir, "a").unwrap(), b"the package");
        assert_eq!(std::fs::read_to_string(dir.join("a/plate/dir_plate.fab.splash")).unwrap(), "text");
        // Only the state changes, whole.
        let ended = SavedState { state: JobState::Failed("no filament".into()), started: Some(100.5), ended: Some(200.25), reached: 55.5, ..Default::default() };
        save_state(&dir, "a", &ended).unwrap();
        assert_eq!(read_all(&dir), vec![(m, ended)]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn half_a_job_is_no_job_and_a_newer_format_is_left_alone() {
        let dir = folder("half");
        let unfinished = dir.join(format!("{UNFINISHED}b"));
        std::fs::create_dir_all(&unfinished).unwrap();
        std::fs::write(unfinished.join(META), meta("b", 1.0).text()).unwrap();
        let newer = dir.join("c");
        std::fs::create_dir_all(&newer).unwrap();
        std::fs::write(newer.join(META), "format=999\nname=c\n").unwrap();
        std::fs::write(newer.join(STATE), "state=queued\nreached=0\n").unwrap();
        assert!(read_all(&dir).is_empty());
        assert!(!unfinished.exists() && newer.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn startup_keeps_every_job_newest_first() {
        let dir = folder("history");
        let mut expected = Vec::new();
        for i in 0..26 {
            // Two presses per millisecond also exercise the tie-break when reloading a folder.
            let created = 100.0 + (i / 2) as f64;
            let id = JobMeta::new_id("cube", created);
            let printer = if i % 2 == 0 { "printer:bench" } else { "printer:spare" };
            let job = JobMeta { printer: Some(printer.into()), ..meta(&id, created) };
            let state = match i { 0 => JobState::Sent, 1 => JobState::Queued, _ => JobState::Finished };
            let saved = SavedState { state, ..Default::default() };
            commit(&dir, &job, &saved, b"package", &BTreeMap::new()).unwrap();
            expected.push((job, saved));
        }
        expected.reverse();
        let interrupted = dir.join(".writing-interrupted");
        std::fs::create_dir(&interrupted).unwrap();
        let all = read_all(&dir);
        assert_eq!(all.len(), expected.len(), "startup keeps the full history and both live jobs");
        assert_eq!(all, expected, "both printers' jobs reload newest first with their saved states");
        assert!(!interrupted.exists(), "startup removes the interrupted write");
        let mut state = PrintState::default();
        for (meta, saved) in all {
            assert_eq!(read_package(&dir, &meta.id).unwrap(), b"package", "the package for {} survives startup", meta.id);
            state.follow.jobs.push(CommittedJob::restored(meta, saved, 0.0, 0.0));
        }
        let listed = job_list::rows(&state, 0.0);
        let (history, queued_and_running) = expected.split_at(24);
        let display_order = queued_and_running.iter().rev().chain(history);
        assert!(listed.iter().map(|row| &row.id).eq(display_order.map(|(meta, _)| &meta.id)),
            "the list shows the running job, the queued job and all 24 finished jobs newest first");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_restored_job_keeps_its_times_across_the_app_clock() {
        // Started 600 s before this run's clock reads 1000.
        let job = CommittedJob::restored(meta("a", 5000.0), SavedState { state: JobState::Sent, started: Some(5000.0), ended: None, reached: 12.5, ..Default::default() }, 1000.0, 5600.0);
        assert_eq!((job.sent_at, job.ended_at, job.reached, job.restored), (400.0, None, 12.5, true));
        assert_eq!(job.saved(1000.0, 5600.0), SavedState { state: JobState::Sent, started: Some(5000.0), ended: None, reached: 12.5, ..Default::default() });
    }
}
