//! KiCad 3D models of the open design (EKMODELS): the parts of a KiCad board name models of
//! KiCad's library (`parts/kicad/3d/<major>/<Lib>.3dshapes/<Name>.glb`). One pool job finds each
//! in a KiCad installed on this computer, else downloads its single `.wrl` from KiCad's repository
//! at its board's library tag (only the two raw-file URL shapes `kicad3d::allowed` takes, no
//! redirects), converts it into the person's part library and parses it into the shared mesh
//! cache; the UI queues paths and drains results without waiting. Which sources it may use is the
//! person's choice (`part_models::source_on`), read per model, as is this computer's choice for
//! the model (`model_board::choice`: either, this computer only, or download only), kept in
//! `choices.tsv` beside the cache. Each outcome goes on the model board, where the part's card and
//! `elec.part.model` read where its model came from. A host that asks to wait pauses the queue
//! until then; the rest of the batch waits with it.

use crate::part_models::source_on;
use fab::model_sources::ModelSource;
use fab_elec_import::cache::MeshCache;
use fab_elec_import::kicad3d::{self, find_local, install_candidates, roots, Cache, DownloadError, Got, LibModel, Local, Origin, Root};
use fab_elec_parts::model_board::{self, Choice, Family, ModelNote};
use makepad_network::blocking_http::{request_no_redirect, CancelToken, Limits, Request};
use makepad_widgets::jobs::{JobHandle, JobSpec};
use makepad_widgets::log;
use makepad_widgets::makepad_platform::thread::{Class, TaskPool, ToUIReceiver, ToUISender};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

type Get = Box<dyn FnMut(&str) -> Result<Got, String> + Send>;
/// Makes a run's GET; the run's cancel token stops a request in flight.
pub type Fetcher = Arc<dyn Fn(CancelToken) -> Get + Send + Sync>;

/// One GET of a KiCad library file: only a URL [`kicad3d::allowed`] takes, no redirect followed.
fn kicad_request(cancel: &CancelToken, url: &str) -> Result<Got, String> {
    if !kicad3d::allowed(url) {
        return Err(format!("GET {url} is not a KiCad library file Fab may fetch"));
    }
    let limits = Limits { max_body_bytes: usize::MAX, ..Limits::default() };
    let response = request_no_redirect(Request::get(url).limits(limits).cancel_token(cancel.clone())).map_err(|e| e.to_string())?;
    let retry_after = response.header("retry-after").map(str::trim)
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .map(|s| s.parse::<u64>().unwrap_or(u64::MAX));
    Ok(Got { status: response.status, body: response.body, retry_after })
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

/// A model the design lacks: its project path, the library model and KiCad's scale.
#[derive(Clone, Debug)]
struct Want {
    path: String,
    model: LibModel,
    scale: [f64; 3],
}

/// A KiCad model ready for the design, parsed into the shared mesh cache.
#[derive(Debug)]
pub struct Model {
    /// The project path the design names.
    pub path: String,
    /// The converted mesh in the part library: the key the design resolves the path to.
    pub key: String,
    pub origin: Origin,
}

/// How a run ended.
#[derive(Debug)]
pub struct Finished {
    pub models: Vec<Model>,
    /// Each names the model, why it has no mesh, and what the part does instead.
    pub failed: Vec<String>,
    /// Models asked for again after a change of this computer's choice that now have none: the
    /// design stops using the mesh it had (its parts show their plain bodies).
    pub drop: Vec<String>,
    pub cancelled: bool,
}

/// The KiCad 3D library folders on this computer, found on the worker.
pub type Roots = Arc<dyn Fn() -> Vec<Root> + Send + Sync>;

enum Update {
    Starting { index: usize, name: String, download: bool },
    /// A model, or its project path and why it has none.
    Model(Result<Model, (String, String)>),
    /// `next`: the first model not done; `pause`: a host asked to wait until then (Unix seconds).
    End { cancelled: bool, next: usize, pause: Option<(u64, u16)> },
}

struct Run {
    cancel: CancelToken,
    job: JobHandle,
    batch: Arc<Vec<Want>>,
    models: Vec<Model>,
    failed: Vec<String>,
    drop: Vec<String>,
}

/// What became of one model.
enum Outcome {
    Ready(Model),
    Failed(String),
    Paused(u64, u16),
}

/// The person's KiCad model cache, beside the EasyEDA part library.
pub fn cache_root() -> PathBuf {
    makepad_fab_settings::home::user_home().join(".makepad/fab/parts/kicad")
}

/// The roots of this computer: the person's folder (3D Model Sources), the environment's
/// variables, then KiCad's standard install folders.
pub fn system_roots(setting: Option<PathBuf>) -> Roots {
    Arc::new(move || {
        let home = makepad_fab_settings::home::user_home();
        let program_files = std::env::var_os("ProgramFiles").map(PathBuf::from);
        let installs = install_candidates(&home, program_files.as_deref());
        roots(setting.as_deref(), &|var| std::env::var(var).ok(), &installs)
    })
}

/// Why the local libraries give no model, or None when one does (then its root and file).
fn local_file(want: &Want, found: &[Root]) -> Result<(Root, PathBuf), String> {
    let shown = format!("{}/{}", want.model.lib, want.model.name);
    match find_local(&want.model, found) {
        Local::Found { root, file } => Ok((root, file)),
        Local::StepOnly { file } => Err(format!(
            "{shown}: no mesh: STEP only ({} has only {}, and Fab reads KiCad's .wrl models)",
            file.parent().map_or(String::new(), |d| d.display().to_string()),
            file.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned()))),
        Local::Missing { looked } if looked.is_empty() => Err(format!("{shown}: Fab found no KiCad 3D library on this computer")),
        Local::Missing { looked } => Err(format!("{shown}: not in the KiCad 3D {} on this computer ({})",
            if looked.len() == 1 { "library" } else { "libraries" },
            looked.iter().map(|d| d.display().to_string()).collect::<Vec<_>>().join(", "))),
    }
}

/// What becomes of one model: the local library's file converted when that source is on and has
/// it, else its download when that source is on, else why there is none.
fn resolve(cache: &Cache, found: &[Root], want: &Want, get: &mut dyn FnMut(&str) -> Result<Got, String>) -> Outcome {
    let shown = format!("{}/{}", want.model.lib, want.model.name);
    let choice = model_board::choice(&want.path);
    let (local_on, download_on) = (source_on(ModelSource::KicadLocal), source_on(ModelSource::KicadDownload));
    let (local, download) = (local_on && choice != Choice::Download, download_on && choice != Choice::Local);
    let local_why = if local {
        match local_file(want, found) {
            Ok((root, file)) => match cache.local(&want.path, &want.model, want.scale, &root, &file) {
                Ok(ready) => return finish(&shown, ready),
                Err(issue) => format!("{shown}: {issue}"),
            },
            Err(why) => why,
        }
    } else if local_on {
        format!("{shown}: this computer gets this model only by download (the part's card)")
    } else {
        format!("{shown}: KiCad's library on this computer is off in 3D Model Sources")
    };
    if !download {
        let hint = if download_on {
            "; the part keeps its plain body (the part's card gets this model from this computer only)"
        } else if local_on {
            "; the part keeps its plain body (Edit > 3D Model Sources… turns on KiCad library downloads)"
        } else {
            "; the part keeps its plain body"
        };
        return Outcome::Failed(format!("{local_why}{hint}"));
    }
    match cache.download(&want.path, &want.model, want.scale, unix_now(), get) {
        Ok(ready) => finish(&shown, ready),
        Err(DownloadError::Paused { until, status }) => Outcome::Paused(until, status),
        // Both reasons: what this computer lacks, then what the download said.
        Err(DownloadError::NotFound(why) | DownloadError::Failed(why)) if local_on => {
            let prefix = format!("{shown}: ");
            Outcome::Failed(format!("{local_why}; download: {}", why.strip_prefix(&prefix).unwrap_or(&why)))
        }
        Err(DownloadError::NotFound(why) | DownloadError::Failed(why)) => Outcome::Failed(why),
    }
}

/// A converted model into the shared mesh cache, its conversion's warnings logged.
fn finish(shown: &str, ready: kicad3d::Ready) -> Outcome {
    for warning in &ready.warnings {
        log!("part models: {shown}: {warning}");
    }
    let Some(key) = ready.glb.to_str().map(str::to_owned) else {
        return Outcome::Failed(format!("{shown}: the part library's path is not valid UTF-8"));
    };
    if let Err(issue) = MeshCache::shared().get(Path::new(&key)) {
        return Outcome::Failed(format!("{shown}: the converted model could not be loaded ({issue})"));
    }
    Outcome::Ready(Model { path: ready.path, key, origin: ready.origin })
}

/// Where a model came from, for its part's card and `elec.part.model`.
fn ready_note(model: &Model) -> ModelNote {
    let (line, detail) = match &model.origin {
        Origin::Local { root, major: Some(major), .. } => (format!("KiCad {major} library on this computer"), root.display().to_string()),
        Origin::Local { root, major: None, .. } => ("KiCad library on this computer".to_string(), root.display().to_string()),
        // short enough for the part card's one line; the licences page and the record keep the URL
        Origin::Download { tag, host, date, .. } => {
            let site = match host.as_str() { kicad3d::GITLAB_HOST => "GitLab", kicad3d::GITHUB_HOST => "GitHub", other => other };
            (format!("KiCad library {tag}"), format!("from {site} on {date}"))
        }
    };
    ModelNote { path: model.path.clone(), family: Family::Kicad, line, detail, here: true }
}

/// Why a model is not here, for its part's card and `elec.part.model`.
fn failed_note(want: &Want, why: &str) -> ModelNote {
    let shown = format!("{}/{}: ", want.model.lib, want.model.name);
    ModelNote { path: want.path.clone(), family: Family::Kicad, line: "KiCad model not here".into(),
        detail: why.strip_prefix(&shown).unwrap_or(why).to_string(), here: false }
}

fn resolve_all(cache: &Cache, roots: &Roots, mut get: Get, batch: &[Want], cancel: &CancelToken, tx: &ToUISender<Update>) -> Update {
    let found = roots();
    for (index, want) in batch.iter().enumerate() {
        if cancel.is_cancelled() {
            return Update::End { cancelled: true, next: index, pause: None };
        }
        let download = source_on(ModelSource::KicadDownload);
        let _ = tx.send(Update::Starting { index, name: want.model.name.clone(), download });
        match resolve(cache, &found, want, &mut get) {
            Outcome::Ready(model) => {
                model_board::note(ready_note(&model), Some(&model.key));
                let _ = tx.send(Update::Model(Ok(model)));
            }
            Outcome::Failed(why) => {
                model_board::note(failed_note(want, &why), None);
                let _ = tx.send(Update::Model(Err((want.path.clone(), why))));
            }
            Outcome::Paused(until, status) => return Update::End { cancelled: false, next: index, pause: Some((until, status)) },
        }
    }
    Update::End { cancelled: cancel.is_cancelled(), next: batch.len(), pause: None }
}

/// The UI's queue of KiCad models and its run.
pub struct KicadModels {
    cache: Arc<Cache>,
    roots: Roots,
    fetcher: Fetcher,
    rx: ToUIReceiver<Update>,
    pending: Vec<Want>,
    /// Every path queued, running or finished: asked for once per session.
    known: HashSet<String>,
    /// A host asked to wait until then (Unix seconds): nothing more is asked before.
    resume_at: Option<u64>,
    /// Asked for again after a change of this computer's choice: one that now fails is dropped.
    rechosen: HashSet<String>,
    run: Option<Run>,
}

impl KicadModels {
    pub fn new(setting: Option<PathBuf>) -> Self {
        Self::with(cache_root(), system_roots(setting), Arc::new(|cancel| Box::new(move |url: &str| kicad_request(&cancel, url))))
    }

    pub fn with(cache: impl Into<PathBuf>, roots: Roots, fetcher: Fetcher) -> Self {
        Self {
            cache: Arc::new(Cache::new(cache)), roots, fetcher, rx: ToUIReceiver::default(),
            pending: Vec::new(), known: HashSet::new(), resume_at: None, rechosen: HashSet::new(), run: None,
        }
    }

    /// When a paused queue may ask again (Unix seconds), known once the paused run has been polled.
    pub fn resume_at(&self) -> Option<u64> {
        self.resume_at
    }

    pub fn running(&self) -> bool {
        self.run.is_some()
    }

    /// Queue the KiCad models among the design's missing meshes (others are left alone), each
    /// once, in path order. Cost: a parse of each path.
    pub fn want(&mut self, pool: &TaskPool, missing: &[String]) {
        for path in missing {
            let Some((model, scale)) = LibModel::of_project_path(path) else { continue };
            if self.known.insert(path.clone()) {
                self.pending.push(Want { path: path.clone(), model, scale });
            }
        }
        self.resume(pool);
    }

    /// Ask for the model at `path` again, after this computer's choice for it changed: it is
    /// resolved by the new choice, and dropped from the design if that gives none.
    pub fn again(&mut self, pool: &TaskPool, path: &str) {
        let Some((model, scale)) = LibModel::of_project_path(path) else { return };
        self.known.insert(path.to_string());
        self.rechosen.insert(path.to_string());
        self.pending.retain(|want| want.path != path);
        self.pending.push(Want { path: path.to_string(), model, scale });
        self.resume(pool);
    }

    /// Start the pending models when idle and no host's pause is running.
    pub fn resume(&mut self, pool: &TaskPool) {
        if self.running() || self.pending.is_empty() || self.resume_at.is_some_and(|until| unix_now() < until) {
            return;
        }
        if self.resume_at.take().is_some() {
            log!("part models: asking KiCad's library hosts again");
        }
        let batch = Arc::new(std::mem::take(&mut self.pending));
        let cancel = CancelToken::new();
        let hook = cancel.clone();
        let job = JobSpec::new("Getting KiCad 3D models").stage("starting").on_cancel(move || hook.cancel()).start();
        let (cache, roots, fetcher, tx, token, inputs) = (self.cache.clone(), self.roots.clone(), self.fetcher.clone(), self.rx.sender(), cancel.clone(), batch.clone());
        match pool.submit_named(Class::Background, "kicad models", move || {
            let end = resolve_all(&cache, &roots, fetcher(token.clone()), &inputs, &token, &tx);
            let _ = tx.send(end);
        }) {
            Ok(task) => {
                task.detach();
                self.run = Some(Run { cancel, job, batch, models: Vec::new(), failed: Vec::new(), drop: Vec::new() });
            }
            Err(error) => {
                for want in batch.iter() {
                    self.known.remove(&want.path);
                }
                let message = format!("KiCad 3D models could not start ({error}); they are asked for again with the next change");
                log!("part models: {message}");
                job.fail(message, true);
            }
        }
    }

    /// Drain progress and results without waiting; a run's end is returned once.
    pub fn poll(&mut self, pool: &TaskPool) -> Option<Finished> {
        while let Ok(update) = self.rx.try_recv() {
            let run = self.run.as_mut()?;
            match update {
                Update::Starting { index, name, download } => {
                    let total = run.batch.len();
                    let stage = if download { format!("{name} (from this computer, else KiCad's repository)") } else { name };
                    run.job.stage(stage, Some(((index + 1) as u32, total as u32)), Some(index as f32 / total as f32));
                }
                Update::Model(Ok(model)) => {
                    self.rechosen.remove(&model.path);
                    run.models.push(model);
                }
                Update::Model(Err((path, error))) => {
                    if self.rechosen.remove(&path) {
                        run.drop.push(path);
                    }
                    run.failed.push(error);
                }
                Update::End { cancelled, next, pause } => {
                    let run = self.run.take()?;
                    let rest = run.batch.get(next..).unwrap_or_default();
                    if let Some((until, status)) = pause {
                        // The model the host paused and the rest wait for the pause, before later wants.
                        log!("part models: KiCad's library hosts asked to wait until {} (HTTP {status}); {} model(s) wait",
                            kicad3d::date(until), rest.len());
                        let mut pending = rest.to_vec();
                        pending.append(&mut self.pending);
                        self.pending = pending;
                        self.resume_at = Some(until);
                    } else if cancelled {
                        // Not attempted: asked for again with a later want.
                        for want in rest {
                            self.known.remove(&want.path);
                        }
                    }
                    if cancelled {
                        run.job.cancelled();
                    } else {
                        run.job.done();
                    }
                    self.resume(pool);
                    return Some(Finished { models: run.models, failed: run.failed, drop: run.drop, cancelled });
                }
            }
        }
        None
    }

    pub fn cancel(&mut self) {
        if let Some(run) = &self.run {
            run.cancel.cancel();
        }
    }
}

impl Drop for KicadModels {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// This computer's choices for KiCad models (`model_board::choices`): `choices.tsv` beside the
/// cache, one `<project path>\t<auto|local|download>` per line. A lane's test instance
/// (`MAKEPAD_TEST_LANE`) reads and keeps none.
fn choices_file() -> Option<PathBuf> {
    if std::env::var_os("MAKEPAD_TEST_LANE").is_some() { return None }
    Some(cache_root().join("choices.tsv"))
}

/// The choices as the file states them; a line it cannot read is skipped and logged.
fn parse_choices(text: &str) -> Vec<(String, Choice)> {
    text.lines().filter(|line| !line.trim().is_empty()).filter_map(|line| {
        let parsed = line.split_once('\t').and_then(|(path, choice)| Some((path.to_string(), Choice::of_key(choice)?)))
            .filter(|(path, _)| Family::of_path(path) == Some(Family::Kicad));
        if parsed.is_none() { log!("part models: choices.tsv: skipped the line {line:?}"); }
        parsed
    }).collect()
}

/// Read this computer's choices at start (none: every model takes either source).
pub fn load_choices() -> BTreeMap<String, Choice> {
    let Some(file) = choices_file() else { return BTreeMap::new() };
    let choices = std::fs::read_to_string(&file).map(|text| parse_choices(&text)).unwrap_or_default();
    model_board::load_choices(choices.clone());
    choices.into_iter().collect()
}

/// Keep this computer's choices; a failure is logged and the choices hold for this session.
pub fn save_choices(choices: &BTreeMap<String, Choice>) {
    let Some(file) = choices_file() else { return };
    let text: String = choices.iter().map(|(path, choice)| format!("{path}\t{}\n", choice.key())).collect();
    let temp = file.with_extension(format!("tsv.new-{}", std::process::id()));
    let written = file.parent().map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&temp, text)).and_then(|()| std::fs::rename(&temp, &file));
    if let Err(error) = written {
        log!("part models: could not keep this computer's model choices in {}: {error}", file.display());
    }
}

/// One line for the log of where a run's models came from: the folders, each once.
pub fn origins_line(models: &[Model]) -> Option<String> {
    let mut folders: Vec<String> = Vec::new();
    for model in models {
        let folder = match &model.origin {
            Origin::Local { root, .. } => format!("this computer's KiCad library ({})", root.display()),
            Origin::Download { tag, host, .. } => format!("KiCad's library {tag} from {host}"),
        };
        if !folders.contains(&folder) {
            folders.push(folder);
        }
    }
    (!models.is_empty()).then(|| format!("KiCad 3D models: {} from {}", models.len(), folders.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_import::kicad3d::RootFrom;
    use makepad_widgets::Cx;
    use std::time::{Duration, Instant};

    /// A box in KiCad's style, in tenths of an inch.
    const BOX: &str = "#VRML V2.0 utf8
Shape { geometry IndexedFaceSet {
  coordIndex [0,3,2,1,-1,4,5,6,7,-1,0,1,5,4,-1,1,2,6,5,-1,2,3,7,6,-1,3,0,4,7,-1]
  coord Coordinate { point [-0.5 -0.5 0, 0.5 -0.5 0, 0.5 0.5 0, -0.5 0.5 0, -0.5 -0.5 0.5, 0.5 -0.5 0.5, 0.5 0.5 0.5, -0.5 0.5 0.5] } }
  appearance Appearance { material Material { diffuseColor 0.15 0.15 0.15 } } }
";

    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    fn done(models: &mut KicadModels, pool: &TaskPool) -> Finished {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(result) = models.poll(pool) {
                assert!(!models.running(), "a returned run is over");
                return result;
            }
            assert!(Instant::now() < deadline, "the KiCad model worker did not finish");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// A fixture laid out like KiCad's `3dmodels` folder: the design's missing KiCad models are
    /// converted from it into the cache, a STEP-only model says so, models of other sources are
    /// left alone, and a repeat asks for nothing.
    #[test]
    fn the_design_gets_its_kicad_models_from_a_local_library() {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let scratch = Scratch(std::env::temp_dir().join(format!("fab-kicad-models-{}-{nanos}", std::process::id())));
        let lib = scratch.0.join("3dmodels").join("Package_SO.3dshapes");
        std::fs::create_dir_all(&lib).expect("a fixture library");
        std::fs::write(lib.join("SOIC-8.wrl"), BOX).expect("a model");
        std::fs::write(lib.join("STEP_ONLY.step"), "ISO-10303-21;").expect("a STEP-only model");
        let dir = scratch.0.join("3dmodels");
        let roots: Roots = Arc::new(move || vec![Root { dir: dir.clone(), major: Some(9), from: RootFrom::Install }]);
        let _cx = crate::cx_shared();
        let cx = Cx::new(Box::new(|_, _| {}));
        let pool = cx.task_pool();
        // A local fake of KiCad's hosts: GitLab has DOWNLOADED; a pause is asked for BUSY.
        let asked = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let log = asked.clone();
        let fetcher: Fetcher = Arc::new(move |_cancel| {
            let log = log.clone();
            Box::new(move |url: &str| {
                assert!(kicad3d::allowed(url), "only allowed URLs are asked for: {url}");
                log.lock().unwrap().push(url.to_string());
                let (status, body, retry_after) = if url.contains("/BUSY.wrl") {
                    (503, "", Some(3600))
                } else if url.contains("gitlab.com") && url.contains("/DOWNLOADED.wrl") {
                    (200, BOX, None)
                } else {
                    (404, "", None)
                };
                Ok(Got { status, body: body.as_bytes().to_vec(), retry_after })
            })
        });
        let mut models = KicadModels::with(scratch.0.join("cache"), roots, fetcher);
        let missing = [
            "parts/kicad/3d/9/Package_SO.3dshapes/SOIC-8.glb",
            "parts/kicad/3d/9/Package_SO.3dshapes/SOIC-8.scale-2_1_1.glb",
            "parts/kicad/3d/9/Package_SO.3dshapes/STEP_ONLY.glb",
            "parts/kicad/3d/9/Package_SO.3dshapes/NONE.glb",
            "parts/kicad/3d/9/Package_SO.3dshapes/DOWNLOADED.glb",
            "parts/lcsc/C1.glb",
        ].map(str::to_string);
        models.want(&pool, &missing);
        let result = done(&mut models, &pool);
        assert!(!result.cancelled);
        assert_eq!(result.models.iter().map(|m| m.path.as_str()).collect::<Vec<_>>(), [&missing[0], &missing[1], &missing[4]],
            "both scales of the installed model, and the downloaded one");
        for model in &result.models {
            let mesh = MeshCache::shared().get(Path::new(&model.key)).expect("the key is a parsed mesh");
            assert!(mesh.sha256.len() == 64 && model.key.starts_with(scratch.0.join("cache").to_str().unwrap()), "kept in the cache: {}", model.key);
        }
        assert!(result.models[..2].iter().all(|m| matches!(&m.origin, Origin::Local { major: Some(9), .. })), "local first");
        assert!(matches!(&result.models[2].origin, Origin::Download { tag, host, .. } if tag == "9.0.0" && host == "gitlab.com"),
            "else downloaded at the board's tag: {:?}", result.models[2].origin);
        assert_eq!(result.failed.len(), 2, "{:?}", result.failed);
        assert!(result.failed[0].contains("STEP_ONLY") && result.failed[0].contains("no mesh: STEP only")
            && result.failed[0].contains("download: not in KiCad's library 9.0.0"), "{}", result.failed[0]);
        assert!(result.failed[1].contains("NONE") && result.failed[1].contains("not in the KiCad 3D library"), "{}", result.failed[1]);
        assert_eq!(asked.lock().unwrap().len(), 2 + 2 + 1, "the local models ask nothing; each missing one asks the canonical host, then the other");
        assert!(origins_line(&result.models).is_some_and(|l| l.starts_with("KiCad 3D models: 3 from this computer's KiCad library")
            && l.contains("KiCad's library 9.0.0 from gitlab.com")), "{:?}", origins_line(&result.models));
        models.want(&pool, &missing);
        assert!(!models.running(), "each path is asked for once");

        // The model board says where each came from, or why it is not here, by path and by key.
        let note = |path: &str| model_board::lookup(path).unwrap_or_else(|| panic!("a note for {path}"));
        assert!(note(&missing[0]).line == "KiCad 9 library on this computer" && note(&result.models[0].key).path == missing[0]);
        assert!(note(&missing[4]).line == "KiCad library 9.0.0" && note(&missing[4]).detail.starts_with("from GitLab on 20"),
            "{:?}", note(&missing[4]));
        assert!(!note(&missing[2]).here && note(&missing[2]).detail.starts_with("no mesh: STEP only"), "{:?}", note(&missing[2]));
        // This computer's choice: SOIC-8 by download only, which lacks it: asked again, then dropped.
        model_board::choose(&missing[0], Choice::Download).expect("downloads are on");
        models.again(&pool, &missing[0]);
        let rechosen = done(&mut models, &pool);
        assert!(rechosen.models.is_empty() && rechosen.drop == [missing[0].clone()], "{rechosen:?}");
        assert!(rechosen.failed[0].contains("only by download") && rechosen.failed[0].contains("not in KiCad's library 9.0.0"),
            "{}", rechosen.failed[0]);
        assert!(!note(&missing[0]).here, "the card says it is not here");
        model_board::choose(&missing[0], Choice::Auto).expect("back to either source");
        assert_eq!(parse_choices("parts/kicad/3d/9/A.3dshapes/B.glb\tlocal\nparts/lcsc/C1.glb\tlocal\nbad\n"),
            [("parts/kicad/3d/9/A.3dshapes/B.glb".to_string(), Choice::Local)], "the choices file keeps KiCad models' choices only");

        // A host that asks to wait pauses the queue: the paused model and the rest wait, in order.
        let busy = ["parts/kicad/3d/9/Package_SO.3dshapes/BUSY.glb", "parts/kicad/3d/9/Package_SO.3dshapes/LATER.glb"].map(str::to_string);
        models.want(&pool, &busy);
        let paused = done(&mut models, &pool);
        assert!(paused.models.is_empty() && paused.failed.is_empty() && !paused.cancelled, "a pause is no failure");
        assert!(models.resume_at().is_some_and(|until| until > unix_now() + 3000), "the host's own wait");
        assert_eq!(models.pending.iter().map(|w| w.path.as_str()).collect::<Vec<_>>(), busy, "both wait for the pause");
        let before = asked.lock().unwrap().len();
        models.resume(&pool);
        assert!(!models.running() && asked.lock().unwrap().len() == before, "nothing is asked before the pause ends");
    }
}
