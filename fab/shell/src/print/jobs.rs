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
//! A job appears whole: its folder is written under another name and then renamed. The running and
//! queued jobs always stay; of the ended ones the newest [`KEEP_ENDED`] stay, up to
//! [`KEEP_ENDED_BYTES`]. A queued job starts only on a person's press: the plate has to be cleared.
//!
//! Main-loop cost: none on disk (workers read and write it), a list scan per printer report.

use super::*;
use fab_printer_link::model::TrayId;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The ended jobs kept, newest first.
pub const KEEP_ENDED: usize = 20;
/// What the ended jobs may hold on disk; the newest always stays.
pub const KEEP_ENDED_BYTES: u64 = 2 << 30;

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
    /// The filament colours its preview draws.
    pub colours: Vec<[f32; 4]>,
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

impl JobMeta {
    /// A new job's folder name: ordered by time, readable, unique per press.
    pub fn new_id(name: &str, created: f64) -> String {
        let safe: String = name.chars().take(48).map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
        format!("{:014}-{safe}", (created * 1000.0) as u64)
    }

    fn text(&self) -> String {
        let colours = self.colours.iter().map(|c| numbers(c)).collect::<Vec<_>>().join(";");
        let trays = self.trays.iter().map(|t| t.map_or("-".to_string(), |t| format!("{}:{}", t.ams, t.slot))).collect::<Vec<_>>().join(",");
        let mut out = format!(
            "format={FORMAT}\nname={}\ncreated={}\nplanned={}\nprepare={}\nlayers={}\nmax_z={}\ntool_changes={}\nfilament_mm={}\nfilament_g={}\nused={}\ntrays={trays}\ncolours={colours}\nlabels={}\nflow_calibration={}\ntimelapse={}\nhead_offset_x={}\nhead_lift={}\ndual={}\nprinter_model={}\nmachine_preset={}\n",
            one_line(&self.name), self.created, self.planned, self.prepare, self.layers, self.max_z, self.tool_changes,
            numbers(&self.filament_mm), numbers(&self.filament_g), numbers(&self.used), numbers(&self.labels),
            self.flow_calibration, self.timelapse, self.head_offset_x, self.head_lift, self.dual,
            one_line(&self.printer_model), one_line(&self.machine_preset),
        );
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
        let colours = get("colours")?.split(';').filter(|c| !c.is_empty()).map(|c| {
            parse_numbers::<f32>(c).and_then(|c| <[f32; 4]>::try_from(c).ok())
        }).collect::<Option<Vec<_>>>()?;
        Some(JobMeta {
            id: id.to_string(),
            name: get("name")?.to_string(),
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
            colours,
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
        Some(SavedState { state, started: time("started"), ended: time("ended"), reached: time("reached").unwrap_or(0.0) })
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
    out.sort_by(|a, b| b.0.created.total_cmp(&a.0.created));
    out
}

fn folder_bytes(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else { return 0 };
    entries.flatten().map(|e| match e.metadata() {
        Ok(m) if m.is_dir() => folder_bytes(&e.path()),
        Ok(m) => m.len(),
        Err(_) => 0,
    }).sum()
}

/// Removes the ended jobs past the bounds, oldest first; returns their ids.
pub fn prune(dir: &Path) -> Vec<String> {
    let (mut kept, mut bytes, mut gone) = (0, 0, Vec::new());
    for (meta, _) in read_all(dir).iter().filter(|(_, s)| s.state.ended()) {
        kept += 1;
        bytes += folder_bytes(&dir.join(&meta.id));
        if (kept > KEEP_ENDED || bytes > KEEP_ENDED_BYTES) && kept > 1 && remove(dir, &meta.id).is_ok() {
            gone.push(meta.id.clone());
        }
    }
    gone
}

/// A job Fab holds: what is on disk, and its preview while it is wanted.
#[derive(Clone, Debug, Default)]
pub struct CommittedJob {
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
    /// Read from disk: no link's tracker knows it, the printer's reports alone say how it ends.
    pub restored: bool,
    /// `Snapshot::job_sends` once the link has taken it: what the link's tracker said before
    /// that was about another job.
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
            restored: true,
            ..Default::default()
        }
    }

    /// `state.txt` of this job now.
    pub fn saved(&self, now: f64, wall: f64) -> SavedState {
        let there = |t: f64| wall - (now - t);
        SavedState {
            state: self.state.clone(),
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
            filament_mm: vec![1000.5, 0.0], filament_g: vec![3.25, 0.0], used: vec![0], trays: vec![Some(TrayId { ams: 1, slot: 2 }), None],
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
    fn a_job_reads_back_as_it_was_saved() {
        let dir = folder("round");
        let m = meta("a", 100.5);
        let saved = SavedState { state: JobState::Sent, started: Some(100.5), ..Default::default() };
        commit(&dir, &m, &saved, b"the package", &BTreeMap::from([("dir/plate.fab.splash".to_string(), "text".to_string())])).unwrap();
        let all = read_all(&dir);
        assert_eq!(all, vec![(m.clone(), saved)]);
        assert_eq!(read_package(&dir, "a").unwrap(), b"the package");
        assert_eq!(std::fs::read_to_string(dir.join("a/plate/dir_plate.fab.splash")).unwrap(), "text");
        // Only the state changes, whole.
        let ended = SavedState { state: JobState::Failed("no filament".into()), started: Some(100.5), ended: Some(200.25), reached: 55.5 };
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
    fn only_ended_jobs_past_the_bound_go() {
        let dir = folder("prune");
        let ended = SavedState { state: JobState::Finished, ..Default::default() };
        for i in 0..KEEP_ENDED + 3 {
            commit(&dir, &meta(&format!("old{i}"), 100.0 + i as f64), &ended, b"p", &BTreeMap::new()).unwrap();
        }
        // Older than all of them: what is running or queued always stays.
        commit(&dir, &meta("running", 1.0), &SavedState { state: JobState::Sent, ..Default::default() }, b"p", &BTreeMap::new()).unwrap();
        commit(&dir, &meta("queued", 2.0), &SavedState { state: JobState::Queued, ..Default::default() }, b"p", &BTreeMap::new()).unwrap();
        let mut gone = prune(&dir);
        gone.sort();
        assert_eq!(gone, vec!["old0", "old1", "old2"]);
        let left: Vec<String> = read_all(&dir).into_iter().map(|(m, _)| m.id).collect();
        assert_eq!(left.len(), KEEP_ENDED + 2);
        assert!(left.iter().any(|id| id == "running") && left.iter().any(|id| id == "queued"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_restored_job_keeps_its_times_across_the_app_clock() {
        // Started 600 s before this run's clock reads 1000.
        let job = CommittedJob::restored(meta("a", 5000.0), SavedState { state: JobState::Sent, started: Some(5000.0), ended: None, reached: 12.5 }, 1000.0, 5600.0);
        assert_eq!((job.sent_at, job.ended_at, job.reached, job.restored), (400.0, None, 12.5, true));
        assert_eq!(job.saved(1000.0, 5600.0), SavedState { state: JobState::Sent, started: Some(5000.0), ended: None, reached: 12.5 });
    }
}
