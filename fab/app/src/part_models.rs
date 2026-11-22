//! Missing part models: one pool job owns imports, file reads, hashes and mesh parsing.
//! The UI queues distinct numbers and drains results. The worker owns retries and library I/O.

use fab_elec_import::{
    cache::MeshCache,
    easyeda::Lcsc,
    library::{Entry, Fetch, ImportError, Library, Source},
};
use fab::model_sources::ModelSource;
use makepad_network::blocking_http::{request_no_redirect, CancelToken, Limits, Request, Response};
use makepad_widgets::jobs::{JobHandle, JobSpec};
use makepad_widgets::log;
use makepad_widgets::makepad_platform::thread::{Class, TaskPool, ToUIReceiver, ToUISender};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Only the two service URLs built from a validated part number or model uuid can be fetched.
/// Comparing the builder's bytes also excludes extra queries, fragments, ports and path segments.
pub fn allowed(url: &str) -> bool {
    if let Some(rest) = url.strip_prefix("https://easyeda.com/api/products/") {
        if let Some(number) = rest.strip_suffix("/components?version=6.4.19.5") {
            if let Some(digits) = number.strip_prefix('C') {
                return (1..=12).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())
                    && url == Lcsc::component_url(number);
            }
        }
    }
    if let Some(uuid) = url.strip_prefix("https://modules.easyeda.com/3dmodel/") {
        return (1..=64).contains(&uuid.len()) && uuid.bytes().all(|b| b.is_ascii_hexdigit())
            && url == Lcsc::model_url(uuid);
    }
    false
}

type Get = Box<dyn FnMut(&str) -> Result<Response, String> + Send>;
pub type Fetcher = Arc<dyn Fn(CancelToken) -> Get + Send + Sync>;

fn part_request(cancel: &CancelToken, url: &str) -> Result<Response, String> {
    if !allowed(url) {
        return Err(format!("GET {url} is not an allowed part-service URL; check the part number"));
    }
    let limits = Limits { max_body_bytes: usize::MAX, ..Limits::default() };
    // blocking_http supplies the generic makepad-network/1.0 agent and reserves that header.
    let request = Request::get(url).limits(limits).cancel_token(cancel.clone());
    request_no_redirect(request).map_err(|e| e.to_string())
}

#[derive(Clone, Copy)]
struct Pause {
    until: u64,
    status: u16,
}

struct Net {
    request: Get,
    pause: Option<Pause>,
}

impl Fetch for Net {
    fn get(&mut self, url: &str) -> Result<(u16, Vec<u8>), String> {
        if let Some(pause) = self.pause {
            return Err(format!("the part service paused requests until {}", pause.until));
        }
        let response = (self.request)(url)?;
        if matches!(response.status, 429 | 503) {
            let seconds = response.header("retry-after").map(str::trim)
                .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .map_or(600, |s| s.parse::<u64>().unwrap_or(u64::MAX));
            self.pause = Some(Pause { until: unix_now().saturating_add(seconds), status: response.status });
        }
        Ok((response.status, response.body))
    }
}

fn file_error(number: &str, path: &Path, operation: &str, message: &str) -> String {
    format!("{number}: the part library at {} could not be {operation}: {message}; \
        check the folder's permissions and available space, then try again", path.display())
}

fn import_error(number: &str, error: ImportError) -> String {
    match error {
        ImportError::Number(_) => format!(
            "{number:?} is not a valid part number; use C followed by 1 to 12 digits, as listed by the part service"
        ),
        ImportError::Request { number, message, .. } => format!(
            "{number}: the part service did not answer ({message}); try again later"
        ),
        ImportError::Response { number, issue, .. } => format!(
            "{number}: the part service did not provide a usable part ({issue}); check the number"
        ),
        ImportError::Io { path, message } => file_error(number, &path, "read or written", &message),
    }
}

const DAY: u64 = 86_400;

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

struct Backoff {
    path: PathBuf,
    entries: BTreeMap<String, (u64, u32)>,
    logged_write_error: bool,
}

impl Backoff {
    fn load(root: &Path) -> Self {
        let path = root.join("backoff.tsv");
        let mut entries = BTreeMap::new();
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                for line in text.lines() {
                    let mut fields = line.split('\t');
                    if let (Some(number), Some(next), Some(count), None) =
                        (fields.next(), fields.next(), fields.next(), fields.next())
                    {
                        if let (Ok(next), Ok(count)) = (next.parse(), count.parse()) {
                            entries.insert(number.to_owned(), (next, count));
                        }
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => log!("part models: could not read {}: {error}", path.display()),
        }
        Self { path, entries, logged_write_error: false }
    }

    fn record_import(&mut self, number: &str, failed: bool) {
        if failed {
            let (next, failures) = self.entries.entry(number.to_owned()).or_default();
            *failures = failures.saturating_add(1);
            let days = match *failures { 1 => 1, 2 => 7, _ => 30 };
            *next = unix_now().saturating_add(days * DAY);
        } else if self.entries.remove(number).is_none() {
            return;
        }
        let mut text = String::new();
        for (number, (next, count)) in &self.entries {
            writeln!(text, "{number}\t{next}\t{count}").expect("writing to a String");
        }
        if let Err(error) = fab_model::project::write_atomic(&self.path, text.as_bytes()) {
            if !self.logged_write_error {
                self.logged_write_error = true;
                log!("part models: could not write backoff: {error}");
            }
        }
    }
}

/// Canonical `parts/<source.folder()>/<N>.glb` paths, once each in path order.
/// Other paths are returned unchanged, in their original order.
pub fn fetchable(source: &dyn Source, missing: &[String]) -> (Vec<(String, String)>, Vec<String>) {
    let prefix = format!("parts/{}/", source.folder());
    let mut paths = BTreeMap::new();
    let mut other = Vec::new();
    for path in missing {
        let number = path.strip_prefix(&prefix).and_then(|p| p.strip_suffix(".glb"));
        match number {
            Some(number) if !number.contains('/')
                && source.number(number).is_ok_and(|canonical| canonical == number) => {
                paths.insert(path.clone(), number.to_string());
            }
            _ => other.push(path.clone()),
        }
    }
    (paths.into_iter().map(|(path, number)| (number, path)).collect(), other)
}

/// A library model ready for the design, already parsed into the shared mesh cache.
#[derive(Debug)]
pub struct Model {
    #[cfg(test)]
    pub number: String,
    /// The project path the design names (`parts/lcsc/C12624.glb`).
    pub path: String,
    /// The mesh file in the part library, the same key a freshly opened board uses.
    pub key: String,
    #[cfg(test)]
    pub origin: fab_elec_import::library::Origin,
}

/// How a run ended. Unattempted numbers after cancellation are not failures.
#[derive(Debug)]
pub struct Finished {
    pub models: Vec<Model>,
    /// Each failure names its number, the cause, and what to do next.
    pub failed: Vec<String>,
    pub cancelled: bool,
    /// Empty on success. Otherwise the failure, cancellation or service pause for the log.
    pub line: String,
}

fn model_of(source: &dyn Source, entry: Entry, path: String) -> Result<Model, String> {
    let number = &entry.number;
    if let Some(reason) = &entry.refused {
        return Err(format!(
            "{number}: this Fab cannot convert the part yet (converter {}, {reason}); its download is kept, \
             so use a newer Fab to convert it without downloading again", source.version()
        ));
    }
    let mesh = entry.mesh.as_ref().ok_or_else(|| format!(
        "{number}: has no 3D model in the part service; choose another part model or keep its plain body"
    ))?;
    let key = mesh.to_str().ok_or_else(|| format!("{number}: the model needs a part library path that is valid UTF-8"))?.to_owned();
    MeshCache::shared().get(mesh).map_err(|issue| format!(
        "{number}: the part library's 3D model could not be loaded ({issue}); use a newer Fab to convert its stored download"
    ))?;
    Ok(Model {
        path, key,
        #[cfg(test)]
        number: entry.number,
        #[cfg(test)]
        origin: entry.origin,
    })
}

enum Update {
    Starting { number: String, index: usize, in_library: bool },
    Model(Result<Model, String>),
    End { next: usize, cancelled: bool, pause: Option<Pause>, deadlines: Vec<(usize, u64)> },
}

struct Run {
    cancel: CancelToken,
    /// The originating board closed; only a subsequent board's wants remain pending.
    board_closed: bool,
    job: JobHandle,
    batch: Arc<Vec<(String, String)>>,
    models: Vec<Model>,
    failed: Vec<String>,
}

fn import_models(
    library: &Library, source: &dyn Source, mut net: Net, batch: &[(String, String)],
    cancel: &CancelToken, tx: &ToUISender<Update>,
) -> Update {
    let mut backoff = Backoff::load(library.root());
    let mut skipped = 0usize;
    let mut soonest = u64::MAX;
    let mut next = 0;
    while let Some((number, path)) = batch.get(next) {
        if cancel.is_cancelled() { break; }
        if let Some(&(until, _)) = backoff.entries.get(number) {
            if until > unix_now() {
                skipped += 1;
                soonest = soonest.min(until);
                next += 1;
                continue;
            }
        }
        let in_library = library.entry_dir(source, number).is_dir();
        let _ = tx.send(Update::Starting { number: number.clone(), index: next, in_library });
        if cancel.is_cancelled() { break; }
        let entry = library.import(source, number, &mut net);
        if net.pause.is_some() || cancel.is_cancelled() { break; }
        // A stored entry (including an unsupported conversion) already has its download.
        if !matches!(entry, Err(ImportError::Number(_))) {
            backoff.record_import(number, entry.is_err());
        }
        let result = entry.map_err(|error| import_error(number, error))
            .and_then(|entry| model_of(source, entry, path.clone()));
        if cancel.is_cancelled() { break; }
        let _ = tx.send(Update::Model(result));
        next += 1;
    }
    if skipped > 0 {
        log!("part models: skipped {skipped} in backoff; soonest next try {soonest}");
    }
    let deadlines = batch[..next].iter().enumerate().filter_map(|(index, (number, _))| {
        backoff.entries.get(number).map(|&(until, _)| (index, until))
    }).collect();
    Update::End { next, cancelled: cancel.is_cancelled(), pause: net.pause, deadlines }
}

/// The enabled 3D model sources (`makepad_fab_shell::model_sources::ModelSources::bits`), for the
/// readers off the UI thread: the design's evaluation lists library models only of sources that are
/// on. The app sets it at start and on every change. Before that: the defaults (KiCad on, EasyEDA
/// off), so nothing reaches EasyEDA before the person's choice is read. The model board keeps the
/// one copy, which the part cards and the actions read too.
pub fn set_sources(bits: u8) {
    fab_elec_parts::model_board::set_sources(bits);
}

/// Whether `source` is on. One atomic load.
pub fn source_on(source: ModelSource) -> bool {
    fab_elec_parts::model_board::sources() & source.bit() != 0
}

/// Where an EasyEDA model in the part library came from, for its part's card and
/// `elec.part.model`: `path` is its project path (`parts/lcsc/C2040.glb`).
pub fn easyeda_note(path: &str) -> fab_elec_parts::model_board::ModelNote {
    let number = path.rsplit('/').next().and_then(|name| name.strip_suffix(".glb")).unwrap_or(path);
    fab_elec_parts::model_board::ModelNote {
        path: path.to_string(), family: fab_elec_parts::model_board::Family::EasyEda,
        line: "EasyEDA part service".into(), detail: format!("part {number}, in your part library"), here: true,
    }
}

/// The note for parts whose models come only from EasyEDA while it is off: they keep their plain
/// bodies, and where to turn it on. None when no such part is open.
pub fn easyeda_off_note(missing: &[String]) -> Option<String> {
    let (lcsc, _) = fetchable(&Lcsc, missing);
    (!lcsc.is_empty()).then(|| format!(
        "3D models: {} part model{} come from EasyEDA, which is off; those parts keep their plain bodies (Edit > 3D Model Sources…)",
        lcsc.len(), if lcsc.len() == 1 { "" } else { "s" }))
}

/// The person's part library, below their home: where the queue keeps models, and where
/// a plate's electronics build reads the models the plate names but holds no file of (plate file,
/// then this library, then the placeholder). A unit test's build reads none: a test that wants
/// one names its own folder where it builds. No I/O.
pub fn library() -> Option<PathBuf> {
    (!cfg!(test)).then(|| makepad_fab_settings::home::user_home().join(".makepad/fab/parts"))
}

/// The UI's completion queue and run state. All library work belongs to the pool job.
pub struct PartModels {
    library: Arc<Library>,
    source: Arc<dyn Source + Send + Sync>,
    fetcher: Fetcher,
    rx: ToUIReceiver<Update>,
    pending: Vec<(String, String)>,
    // All queued, running or finished numbers. A deadline admits a later want once the
    // library's backoff expires, including failures recorded during this session.
    known: HashMap<String, Option<u64>>,
    resume_at: Option<u64>,
    run: Option<Run>,
}

impl PartModels {
    /// Prepare the person's library path and guarded fetcher, without doing any I/O.
    pub fn new() -> Self {
        Self::with(
            makepad_fab_settings::home::user_home().join(".makepad/fab/parts"),
            Arc::new(Lcsc),
            Arc::new(|cancel| Box::new(move |url: &str| part_request(&cancel, url))),
        )
    }

    pub fn with(root: impl Into<PathBuf>, source: Arc<dyn Source + Send + Sync>, fetcher: Fetcher) -> Self {
        Self {
            library: Arc::new(Library::new(root)), source, fetcher,
            rx: ToUIReceiver::default(), pending: Vec::new(), known: HashMap::new(),
            resume_at: None, run: None,
        }
    }

    pub fn running(&self) -> bool {
        self.run.is_some()
    }

    #[cfg(test)]
    fn job(&self) -> Option<makepad_widgets::jobs::JobId> {
        self.run.as_ref().map(|run| run.job.id())
    }

    /// Queue new numbers in order, including while another batch is running or paused.
    pub fn want(&mut self, pool: &TaskPool, models: Vec<(String, String)>) {
        let now = unix_now();
        for (number, path) in models {
            match self.known.get(&number) {
                Some(None) => continue,
                Some(Some(until)) if *until > now => continue,
                _ => {}
            }
            self.known.insert(number.clone(), None);
            self.pending.push((number, path));
        }
        self.resume(pool);
    }

    /// The service's Unix-seconds deadline, available once poll delivers the paused run.
    pub fn resume_at(&self) -> Option<u64> {
        self.resume_at
    }

    /// Stop asking for parts of the previous board. The running worker still drains through
    /// poll, so new wants stay serial and a service pause applies across the switch.
    pub fn close_board(&mut self) {
        self.cancel();
        for (number, _) in self.pending.drain(..) {
            self.known.remove(&number);
        }
        if let Some(run) = &mut self.run {
            run.board_closed = true;
            for (number, _) in run.batch.iter() {
                self.known.remove(number);
            }
        }
    }

    /// Start the pending batch when idle and the service's pause (if any) has elapsed.
    pub fn resume(&mut self, pool: &TaskPool) {
        if self.running() { return; }
        if self.resume_at().is_some_and(|until| unix_now() < until) { return; }
        let was_paused = self.resume_at.take().is_some();
        if self.pending.is_empty() { return; }
        let batch = Arc::new(std::mem::take(&mut self.pending));
        let cancel = CancelToken::new();
        let hook = cancel.clone();
        let job = JobSpec::new("Getting 3D models").stage("starting")
            .on_cancel(move || hook.cancel()).start();
        let (library, source, fetcher) = (self.library.clone(), self.source.clone(), self.fetcher.clone());
        let (tx, worker_cancel, inputs) = (self.rx.sender(), cancel.clone(), batch.clone());
        match pool.submit_named(Class::Background, "part models", move || {
            let net = Net { request: fetcher(worker_cancel.clone()), pause: None };
            let end = import_models(&library, source.as_ref(), net, &inputs, &worker_cancel, &tx);
            drop(inputs);
            let _ = tx.send(end);
        }) {
            Ok(task) => {
                task.detach();
                self.run = Some(Run { cancel, board_closed: false, job, batch, models: Vec::new(), failed: Vec::new() });
                if was_paused { log!("part models: resuming requests to the part service"); }
            }
            Err(error) => {
                self.pending = Arc::unwrap_or_clone(batch);
                let message = format!("3D models could not start ({error}); try again when Fab is ready");
                log!("part models: {message}");
                job.fail(message, true);
            }
        }
    }

    /// Drain queued progress and results without waiting. Completion is returned only once.
    pub fn poll(&mut self, pool: &TaskPool) -> Option<Finished> {
        while let Ok(update) = self.rx.try_recv() {
            let run = self.run.as_mut()?;
            match update {
                Update::Starting { number, index, in_library } => {
                    let from = if in_library { "your part library" } else { "the part service" };
                    let total = run.batch.len();
                    run.job.stage(format!("{number} from {from}"),
                        Some(((index + 1) as u32, total as u32)),
                        Some(index as f32 / total as f32));
                }
                Update::Model(Ok(model)) => run.models.push(model),
                Update::Model(Err(error)) => run.failed.push(error),
                Update::End { next, cancelled, pause, deadlines } => {
                    let run = self.run.take()?;
                    if !run.board_closed {
                        for (index, until) in deadlines {
                            if let Some(deadline) = self.known.get_mut(&run.batch[index].0) {
                                *deadline = Some(until);
                            }
                        }
                    }
                    let mut notices = Vec::new();
                    if cancelled {
                        notices.push(format!("cancelled after {next} of {} parts", run.batch.len()));
                    }
                    if let Some(pause) = pause {
                        self.resume_at = Some(pause.until);
                        let reason = format!("the part service paused requests until {} (HTTP {})", pause.until, pause.status);
                        log!("part models: {reason}");
                        notices.push(reason);
                        if !run.board_closed {
                            let mut pending = Arc::unwrap_or_clone(run.batch).split_off(next);
                            pending.append(&mut self.pending);
                            self.pending = pending;
                        }
                    } else if !run.board_closed {
                        // Cancelled, unattempted numbers may be wanted again.
                        for (number, _) in &run.batch[next..] { self.known.remove(number); }
                    }
                    if !run.failed.is_empty() {
                        notices.push(format!("{} failed ({})", run.failed.len(), run.failed.join("; ")));
                    }
                    let line = if notices.is_empty() { String::new() } else { format!("3D models: {}", notices.join("; ")) };
                    // A part without a model keeps its box and is logged by the app; the run itself
                    // did its work, so the jobs board shows no failure for it.
                    if cancelled {
                        run.job.cancelled();
                    } else {
                        run.job.done();
                    }
                    if pause.is_none() { self.resume(pool); }
                    return Some(Finished { models: run.models, failed: run.failed, cancelled, line });
                }
            }
        }
        None
    }

    /// Signal cancellation without joining the worker or discarding results already queued.
    pub fn cancel(&mut self) {
        if let Some(run) = &self.run {
            run.cancel.cancel();
            if !makepad_widgets::jobs::board().cancel(run.job.id()) {
                run.job.cancel_flag().store(true, std::sync::atomic::Ordering::Release);
            }
        }
    }
}

impl Drop for PartModels {
    fn drop(&mut self) {
        self.cancel();
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_import::library::{Converted, Origin, Raw};
    use fab_elec_import::Issue;
    use makepad_widgets::Cx;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Mutex};
    use std::time::{Duration, Instant};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let time = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock follows the epoch").as_nanos();
            let next = NEXT.fetch_add(1, Ordering::Relaxed);
            Self(std::env::temp_dir().join(format!("fab-part-import-{}-{time}-{next}", std::process::id())))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    /// Every request is synthetic; a one-shot gate holds the first request until the test releases it.
    struct Fake {
        jobs: AtomicUsize,
        requests: AtomicUsize,
        in_flight: AtomicUsize,
        peak: AtomicUsize,
        reply: Box<dyn Fn(&str) -> Response + Send + Sync>,
        gate: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
    }

    impl Fake {
        fn new(reply: impl Fn(&str) -> Response + Send + Sync + 'static) -> Arc<Self> {
            Arc::new(Self {
                jobs: AtomicUsize::new(0), requests: AtomicUsize::new(0),
                in_flight: AtomicUsize::new(0), peak: AtomicUsize::new(0),
                reply: Box::new(reply), gate: Mutex::new(None),
            })
        }
        fn fetcher(self: &Arc<Self>) -> Fetcher {
            let fake = self.clone();
            Arc::new(move |cancel| {
                fake.jobs.fetch_add(1, Ordering::SeqCst);
                let fake = fake.clone();
                Box::new(move |url: &str| fake.get(&cancel, url))
            })
        }
        fn get(&self, cancel: &CancelToken, url: &str) -> Result<Response, String> {
            self.requests.fetch_add(1, Ordering::SeqCst);
            let active = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(active, Ordering::SeqCst);
            let gate = self.gate.lock().expect("gate lock").take();
            if let Some((started, open)) = gate {
                started.send(()).expect("test waits for first request");
                open.recv_timeout(Duration::from_secs(60)).expect("test opens first request's gate");
            }
            let result = if cancel.is_cancelled() { Err("cancelled".into()) } else { Ok((self.reply)(url)) };
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            result
        }
        fn hold(&self) -> (mpsc::Receiver<()>, Release) {
            let (started_tx, started_rx) = mpsc::channel();
            let (open_tx, open_rx) = mpsc::channel();
            *self.gate.lock().expect("gate lock") = Some((started_tx, open_rx));
            (started_rx, Release(Some(open_tx)))
        }
    }

    // Release on an assertion failure too, so the test cannot leave a worker stuck on its gate.
    struct Release(Option<mpsc::Sender<()>>);
    impl Drop for Release {
        fn drop(&mut self) {
            if let Some(open) = self.0.take() { let _ = open.send(()); }
        }
    }

    #[test]
    fn the_url_guard_allows_only_the_two_part_service_shapes() {
        for number in ["C1", "C123456789012"] {
            assert!(allowed(&Lcsc::component_url(number)), "builder URL for {number} is allowed");
        }
        for uuid in ["a".to_string(), "0123456789abcdefABCDEF".into(), "a".repeat(64)] {
            assert!(allowed(&Lcsc::model_url(&uuid)), "builder URL for {uuid} is allowed");
        }
        let component = Lcsc::component_url("C2040");
        let model = Lcsc::model_url("aBc123");
        let refused = vec![
            component.replacen("https:", "http:", 1), model.replacen("https:", "http:", 1),
            component.replace("easyeda.com", "easyeda.com.example"),
            model.replace("modules.easyeda.com", "modules.easyeda.com.example"),
            component.replace("easyeda.com", "easyeda.com:443"),
            model.replace("easyeda.com", "easyeda.com:443"),
            component.replace("/components?", "/extra/components?"), format!("{model}/extra"),
            Lcsc::model_url(".."), Lcsc::model_url("../x"), Lcsc::model_url(""),
            Lcsc::component_url("C"), Lcsc::component_url("C1234567890123"), Lcsc::component_url("C12x"),
            Lcsc::component_url("c1"), Lcsc::component_url("C١"),
            Lcsc::model_url("xyz"), Lcsc::model_url(&"a".repeat(65)),
            format!("{component}&extra=1"), format!("{model}?extra=1"),
            component.replace("6.4.19.5", "6.4.19.6"), format!("{component}#fragment"), format!("{model}#fragment"),
        ];
        let cancel = CancelToken::new();
        for url in refused {
            assert!(!allowed(&url), "URL outside the two exact service shapes must be refused: {url}");
            let error = part_request(&cancel, &url).expect_err("refused URLs must return before any network request");
            assert!(error.contains(&url) && error.contains("not an allowed"), "guard names the refused URL: {error}");
        }
    }

    struct Example;

    impl Source for Example {
        fn folder(&self) -> &'static str { "lcsc" }
        fn version(&self) -> u32 { 7 }
        fn number(&self, text: &str) -> Result<String, Issue> { Lcsc.number(text) }
        fn fetch(&self, number: &str, get: &mut dyn FnMut(&str) -> Result<Vec<u8>, ImportError>, raw: &mut Raw)
            -> Result<(), ImportError>
        {
            raw.put("part.txt", get(&format!("https://parts.example/part/{number}"))?);
            Ok(())
        }
        fn convert(&self, number: &str, raw: &Raw) -> Result<Converted, Issue> {
            use fab_elec_import::{glb::write_glb, Material, MeshBuilder, Normals};
            match raw.get("part.txt").expect("synthetic response is stored") {
                b"no mesh" => return Ok(Converted { part: format!("// {number}\n"), mesh: None, warnings: vec![] }),
                b"unsupported" => return Err(Issue::new(1, "shape", "an unsupported shape")),
                b"invalid mesh" => return Ok(Converted { part: format!("// {number}\n"), mesh: Some(b"not a glb".to_vec()), warnings: vec![] }),
                _ => {}
            }
            let mut builder = MeshBuilder::new();
            let grey = builder.add_material(Material::named("body"));
            let base = [[-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [1.0, 1.0, 0.0], [-1.0, 1.0, 0.0]];
            let normals = Normals::Crease { domain: 0, angle: 0.5 };
            assert!(builder.polygon(grey, &[base[3], base[2], base[1], base[0]], normals), "the base is a polygon");
            for i in 0..4 {
                assert!(builder.polygon(grey, &[base[i], base[(i + 1) % 4], [0.0, 0.0, 2.0]], normals),
                    "pyramid side {i} is a triangle");
            }
            Ok(Converted {
                part: format!("// {number}\nbody: {{mesh: \"{number}.glb\"}}\n"),
                mesh: Some(write_glb(&builder.finish())), warnings: vec![],
            })
        }
    }

    fn example_reply(url: &str) -> Response {
        let number = url.strip_prefix("https://parts.example/part/").expect("only synthetic part URLs are fetched");
        let (status, body) = match number {
            "C2" => (500, Vec::new()),
            "C3" => (200, b"no mesh".to_vec()),
            "C5" => (200, b"unsupported".to_vec()),
            "C6" => (200, b"invalid mesh".to_vec()),
            _ => (200, number.as_bytes().to_vec()),
        };
        Response { status, headers: Vec::new(), body }
    }

    fn paths(numbers: &[&str]) -> Vec<(String, String)> {
        numbers.iter().map(|n| (n.to_string(), format!("parts/lcsc/{n}.glb"))).collect()
    }

    fn next_result(models: &mut PartModels, pool: &TaskPool) -> Finished {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(result) = models.poll(pool) {
                return result;
            }
            assert!(Instant::now() < deadline, "model worker did not finish within the deadline");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn done(models: &mut PartModels, pool: &TaskPool) -> Finished {
        let result = next_result(models, pool);
        assert!(!models.running(), "completion leaves no run active");
        assert!(models.job().is_none(), "completion leaves no current job id");
        assert!(models.poll(pool).is_none(), "completion is returned only once");
        result
    }

    #[test]
    fn fetchable_keeps_only_canonical_mesh_paths_once_in_path_order() {
        let other = ["parts/lcsc/c12.glb", "parts/other/C1.glb", "parts/lcsc/C1.obj",
            "parts/lcsc/sub/C1.glb", "x.glb", "x.glb"];
        let mut missing: Vec<String> = other.iter().map(|p| p.to_string()).collect();
        missing.extend(["parts/lcsc/C4.glb", "parts/lcsc/C1.glb", "parts/lcsc/C4.glb"].map(str::to_string));
        let (models, rest) = fetchable(&Example, &missing);
        assert_eq!(models, paths(&["C1", "C4"]), "fetchable models are canonical, sorted by path and unique");
        assert_eq!(rest, other, "other paths keep their original spelling, order and duplicates");
        let defaults = PartModels::new(); // Construction alone does not create a fetcher or make a request.
        assert_eq!(defaults.library.root(), makepad_fab_settings::home::user_home().join(".makepad/fab/parts"),
            "default construction uses the person's part library");
        assert!(!defaults.running() && defaults.job().is_none(), "default construction starts no work");
    }

    #[test]
    fn runs_fetch_once_cache_models_and_reuse_the_library() {
        use makepad_widgets::jobs::{board, JobEnd};
        let root = Scratch::new();
        let _cx = crate::cx_shared();
        let cx = Cx::new(Box::new(|_, _| {}));
        let pool = cx.task_pool();
        let fake = Fake::new(example_reply);
        let mut models = PartModels::with(&root.0, Arc::new(Example), fake.fetcher());
        assert_eq!(models.library.root(), root.0, "injected library root is retained");
        models.want(&pool, paths(&["C4", "C2", "C1"]));
        let job = models.job().expect("running import has a board job");
        let result = done(&mut models, &pool);
        assert!(!result.cancelled, "normal completion is not cancelled");
        assert_eq!(result.models.len(), 2, "two good parts yield two models");
        assert_eq!(result.models.iter().map(|m| m.number.as_str()).collect::<Vec<_>>(), ["C4", "C1"],
            "successful models stay in request order across a failure");
        let files: BTreeMap<_, _> = models.library.meshes(&Example).into_iter().collect();
        for model in &result.models {
            assert_eq!(model.path, format!("parts/lcsc/{}.glb", model.number), "model keeps the requested project path");
            assert_eq!(Path::new(&model.key), files[&model.number], "the worker and a fresh library listing use the same mesh key");
            let cached = MeshCache::shared().get(Path::new(&model.key)).expect("the worker's key resolves to a usable mesh");
            let bytes = std::fs::read(&files[&model.number]).expect("library holds the converted mesh");
            assert_eq!(cached.sha256.as_ref(), makepad_core_util::sha256::sha256_hex(&bytes), "the cache used the exact library bytes");
            assert_eq!(model.origin, Origin::Fetched, "first import is a download");
        }
        assert_eq!(result.failed.len(), 1, "only the failed number is reported as failed");
        assert!(result.failed[0].contains("C2") && result.failed[0].contains("status 500") && result.failed[0].contains("try again"),
            "failure names the number, HTTP status and remedy: {}", result.failed[0]);
        assert_eq!(result.line, format!("3D models: 1 failed ({})", result.failed[0]),
            "a partial run reports its failure without announcing the two downloads");
        assert!(!result.line.contains("successfully") && !result.line.contains('!'), "result wording stays plain");
        // a failed job stays on the board; a plain done one this quick leaves it (the board's fleeting rule)
        assert!(board().get(job).is_none_or(|finished| matches!(finished.end, JobEnd::Done { next: None })),
            "a part without a model is logged and keeps its box: the run ends done, never failed");
        assert_eq!(fake.requests.load(Ordering::SeqCst), 3, "one request per number, including the failure");
        assert_eq!(fake.jobs.load(Ordering::SeqCst), 1, "the whole run uses one fetcher");

        models.want(&pool, paths(&["C1", "C2", "C4"]));
        assert!(!models.running() && models.pending.is_empty(), "a repeat want starts no work for landed or failed numbers");

        let mut models = PartModels::with(&root.0, Arc::new(Example), fake.fetcher());
        models.want(&pool, paths(&["C1", "C4"]));
        let job = models.job().expect("library run has a board job");
        let result = done(&mut models, &pool);
        assert_eq!(result.models.len(), 2, "library run returns both models");
        assert!(result.models.iter().all(|m| m.origin == Origin::Library), "second run reads unchanged library entries");
        assert!(result.failed.is_empty() && !result.cancelled, "library run has no failure or cancellation");
        assert!(result.line.is_empty(), "two library hits produce no success notice");
        assert_eq!(fake.requests.load(Ordering::SeqCst), 3, "library hits make no further request");
        assert_eq!(fake.jobs.load(Ordering::SeqCst), 2, "each run creates just one fetcher");
        assert!(board().get(job).is_none_or(|finished| matches!(finished.end, JobEnd::Done { .. })),
            "all-good run ends as done, or leaves the board as a fleeting success");

        std::fs::write(models.library.entry_dir(&Example, "C1").join("convert.txt"), "converter 6\n")
            .expect("simulate an older converter marker");
        let mut models = PartModels::with(&root.0, Arc::new(Example), fake.fetcher());
        models.want(&pool, paths(&["C1"]));
        let result = done(&mut models, &pool);
        assert_eq!(result.models[0].origin, Origin::Reconverted, "older library entry reconverts from its raw data");
        assert_eq!(result.models.len(), 1, "only the reconverted library model is returned");
        assert!(result.line.is_empty(), "a successful reconversion produces no notice");
        assert_eq!(fake.requests.load(Ordering::SeqCst), 3, "reconversion does not download again");

        let (started, release) = fake.hold();
        models.want(&pool, paths(&["C7", "C8"]));
        let first_job = models.job().expect("first serial batch has a job");
        started.recv_timeout(Duration::from_secs(60)).expect("first serial batch reaches its gate");
        models.want(&pool, paths(&["C8", "C7", "C9", "C9"]));
        assert_eq!(models.job(), Some(first_job), "a second want preserves the running job");
        assert_eq!(models.pending, paths(&["C9"]), "the queue accepts only distinct new numbers");
        drop(release);
        let first = next_result(&mut models, &pool);
        assert_eq!(first.models.iter().map(|m| m.number.as_str()).collect::<Vec<_>>(), ["C7", "C8"],
            "the running batch finishes before queued wants");
        assert!(models.job().is_some_and(|job| job != first_job), "poll starts the pending batch as a new job");
        let second = done(&mut models, &pool);
        assert_eq!(second.models.iter().map(|m| m.number.as_str()).collect::<Vec<_>>(), ["C9"],
            "duplicate wants land each new number only once");
        assert_eq!(fake.requests.load(Ordering::SeqCst), 6, "overlapping wants add one request per distinct new number");
        assert_eq!(fake.peak.load(Ordering::SeqCst), 1, "at most one request is ever in flight across runs");
    }

    #[test]
    fn missing_and_refused_models_are_explained() {
        let root = Scratch::new();
        let _cx = crate::cx_shared();
        let cx = Cx::new(Box::new(|_, _| {}));
        let pool = cx.task_pool();
        let fake = Fake::new(example_reply);
        let mut models = PartModels::with(&root.0, Arc::new(Example), fake.fetcher());
        models.want(&pool, paths(&["C3", "C5", "C6"]));
        let result = done(&mut models, &pool);
        assert!(result.models.is_empty(), "missing, refused and invalid meshes add no model");
        assert_eq!(result.failed.len(), 3, "each unusable model has one failure");
        assert!(result.failed[0].contains("C3") && result.failed[0].contains("has no 3D model in the part service")
            && result.failed[0].contains("keep its plain body"), "no-mesh failure names the number, reason and next step: {}", result.failed[0]);
        assert!(result.failed[1].contains("C5") && result.failed[1].contains("converter 7") && result.failed[1].contains("newer Fab"),
            "refused conversion names the number, converter and next step: {}", result.failed[1]);
        assert!(result.failed[2].contains("C6") && result.failed[2].contains("could not be loaded") && result.failed[2].contains("glb"),
            "mesh-cache failure names the number and parsing issue: {}", result.failed[2]);
        assert_eq!(fake.requests.load(Ordering::SeqCst), 3, "each failed model uses only its synthetic request");
        let mut models = PartModels::with(&root.0, Arc::new(Example), fake.fetcher());
        models.want(&pool, paths(&["C3", "C5", "C6"]));
        assert_eq!(done(&mut models, &pool).failed.len(), 3, "stored unsuccessful conversions remain failures");
        assert_eq!(fake.requests.load(Ordering::SeqCst), 3, "missing and unusable models retain their downloads too");
    }

    #[test]
    fn import_failures_back_off_across_sessions_and_success_clears_the_record() {
        let root = Scratch::new();
        let _cx = crate::cx_shared();
        let cx = Cx::new(Box::new(|_, _| {}));
        let pool = cx.task_pool();
        let fake = Fake::new(example_reply);
        let path = root.0.join("backoff.tsv");
        for (count, days) in [(1, 1), (2, 7), (3, 30), (4, 30)] {
            if count > 1 {
                std::fs::write(&path, format!("C2\t0\t{}\n", count - 1)).expect("expire the previous failure");
            }
            let mut models = PartModels::with(&root.0, Arc::new(Example), fake.fetcher());
            let before = unix_now();
            models.want(&pool, paths(&["C2"]));
            let result = done(&mut models, &pool);
            assert_eq!(result.failed.len(), 1, "attempt {count} reports its failure");
            let saved = std::fs::read_to_string(&path).expect("the failure wrote backoff.tsv");
            let until: u64 = saved.split('\t').nth(1).expect("a next-try field").parse().expect("Unix seconds");
            assert_eq!(models.known.get("C2"), Some(&Some(until)), "the open session honors the same retry deadline as the library");
            assert_eq!(saved, format!("C2\t{until}\t{count}\n"), "each failure updates one TSV record for its number");
            let delay = days * 24 * 60 * 60;
            assert!((before + delay..=unix_now() + delay).contains(&until),
                "failure {count} backs off for {days} days");

            let mut reopened = PartModels::with(&root.0, Arc::new(Example), fake.fetcher());
            reopened.want(&pool, paths(&["C2"]));
            let skipped = done(&mut reopened, &pool);
            assert!(skipped.models.is_empty() && skipped.failed.is_empty() && skipped.line.is_empty(),
                "an active backoff skips the number silently");
            assert_eq!(fake.requests.load(Ordering::SeqCst), count, "a new session honors the deadline without a request");
            assert_eq!(std::fs::read_to_string(&path).expect("read backoff after skip"), saved,
                "skipping a number preserves its deadline and failure count");
        }
        let retained = std::fs::read_to_string(&path).expect("unrelated failure record");
        std::fs::write(&path, format!("{retained}C1\t0\t2\n")).expect("seed an expired retry for a good part");
        let mut models = PartModels::with(&root.0, Arc::new(Example), fake.fetcher());
        models.want(&pool, paths(&["C1", "C2"]));
        let result = done(&mut models, &pool);
        assert_eq!(result.models.iter().map(|m| m.number.as_str()).collect::<Vec<_>>(), ["C1"],
            "the expired retry succeeds alongside an active backoff");
        assert_eq!(std::fs::read_to_string(&path).expect("read backoff after success"), retained,
            "success removes only that number's failure history");
    }

    #[test]
    fn service_pause_returns_the_untouched_tail_and_resumes_it_in_order() {
        let root = Scratch::new();
        let _cx = crate::cx_shared();
        let cx = Cx::new(Box::new(|_, _| {}));
        let pool = cx.task_pool();
        let attempts = AtomicUsize::new(0);
        let fake = Fake::new(move |url| {
            let mut reply = example_reply(url);
            if url.ends_with("/C4") && attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                reply.status = 429;
                reply.headers.push(("retry-after".into(), "120".into()));
            } else if url.ends_with("/C8") {
                reply.status = 503;
                reply.headers.push(("retry-after".into(), "later".into()));
            }
            reply
        });
        let mut models = PartModels::with(&root.0, Arc::new(Example), fake.fetcher());
        let before = unix_now();
        models.want(&pool, paths(&["C1", "C4", "C7"]));
        let result = done(&mut models, &pool);
        let until = models.resume_at().expect("poll exposes the service deadline");
        assert!((before + 120..=unix_now() + 120).contains(&until), "429 honors numeric Retry-After seconds");
        assert!(result.line.contains("429") && result.line.contains(&until.to_string()), "poll reports the pause status and time");
        assert!(result.failed.is_empty() && !result.cancelled, "a service pause is neither a part failure nor a cancellation");
        assert_eq!(result.models.iter().map(|m| m.number.as_str()).collect::<Vec<_>>(), ["C1"],
            "the model completed before the pause is delivered");
        let log = std::fs::read_to_string(root.0.join("requests.log")).expect("read synthetic request log");
        assert_eq!(log.lines().map(|l| l.split_whitespace().nth(1)).collect::<Vec<_>>(), [Some("C1"), Some("C4")],
            "the worker stops at the second number without requesting the third");
        assert_eq!(models.pending, paths(&["C4", "C7"]), "the paused number and its untouched successor stay pending");
        assert!(!root.0.join("backoff.tsv").exists(), "neither the paused nor the unattempted number gets a backoff record");

        models.want(&pool, paths(&["C1", "C4", "C7", "C8"]));
        models.resume(&pool);
        assert!(!models.running() && models.resume_at() == Some(until), "want and early resume respect the whole-queue pause");
        assert_eq!(models.pending, paths(&["C4", "C7", "C8"]), "new distinct wants wait behind the untouched tail");
        assert_eq!(fake.jobs.load(Ordering::SeqCst), 1, "an early resume does not submit another worker");
        assert_eq!(fake.requests.load(Ordering::SeqCst), 2, "an early resume makes no requests");

        models.resume_at = Some(unix_now());
        let before = unix_now();
        models.resume(&pool);
        assert!(models.running() && models.resume_at().is_none(), "a due timer starts the pending job and clears its pause");
        let resumed = done(&mut models, &pool);
        assert_eq!(resumed.models.iter().map(|m| m.number.as_str()).collect::<Vec<_>>(), ["C4", "C7"],
            "resumption retries the paused number first and delivers the untouched successor");
        let until = models.resume_at().expect("503 pauses the queue too");
        assert!((before + 600..=unix_now() + 600).contains(&until), "503 without numeric Retry-After pauses for ten minutes");
        assert_eq!(models.pending, paths(&["C8"]), "the new pause keeps only its outstanding number");
        assert_eq!(fake.requests.load(Ordering::SeqCst), 5, "resumption makes no repeat request for the landed model");

        models.close_board();
        models.want(&pool, paths(&["C9", "C10"]));
        assert_eq!(models.resume_at(), Some(until), "switching boards preserves the service cooldown");
        assert_eq!(models.pending, paths(&["C9", "C10"]), "only the new board's parts wait for the deadline");
        let (started, release) = fake.hold();
        models.resume_at = Some(unix_now());
        models.resume(&pool);
        started.recv_timeout(Duration::from_secs(60)).expect("the new board's first request starts");
        models.want(&pool, paths(&["C11"]));
        models.close_board();
        models.want(&pool, paths(&["C10"]));
        drop(release);
        assert!(next_result(&mut models, &pool).cancelled, "closing a board stops its active batch");
        let last = done(&mut models, &pool);
        assert_eq!(last.models.iter().map(|m| m.number.as_str()).collect::<Vec<_>>(), ["C10"],
            "an overlapping number still lands for the newly opened board");
        assert_eq!(fake.requests.load(Ordering::SeqCst), 7, "only the held request and the open board's number were requested");
        assert_eq!(fake.peak.load(Ordering::SeqCst), 1, "a board switch keeps requests serial");
    }

    #[test]
    fn cancellation_and_submission_refusal_leave_no_later_requests() {
        use makepad_widgets::jobs::{board, JobEnd};
        let root = Scratch::new();
        let _cx = crate::cx_shared();
        let cx = Cx::new(Box::new(|_, _| {}));
        let pool = cx.task_pool();
        let fake = Fake::new(example_reply);
        let mut models = PartModels::with(&root.0, Arc::new(Example), fake.fetcher());
        models.want(&pool, Vec::new());
        assert!(models.job().is_none() && models.pending.is_empty(), "an empty want starts and queues nothing");
        let last_job = board().snapshot().last().map(|job| job.id);
        models.want(&TaskPool::closed(), paths(&["C1"]));
        assert!(board().snapshot().iter().any(|job| Some(job.id) > last_job && job.title == "Getting 3D models"
            && matches!(&job.end, JobEnd::Failed { reason, retry: true }
                if reason.contains("could not start") && reason.contains("try again"))),
            "a closed pool reports its submission failure and remedy");
        assert!(!models.running() && models.job().is_none() && models.poll(&pool).is_none(),
            "refused submissions leave no active job or queued completion");
        assert_eq!(models.pending, paths(&["C1"]), "a refused submission keeps the number available for the next run");
        assert_eq!(fake.jobs.load(Ordering::SeqCst), 0, "refused submissions never create a fetcher");

        let (started, release) = fake.hold();
        models.want(&pool, paths(&["C1", "C4"]));
        let job = models.job().expect("running run has its board id");
        started.recv_timeout(Duration::from_secs(60)).expect("first request reaches its gate");
        models.want(&pool, paths(&["C4"]));
        assert_eq!(models.job(), Some(job), "a duplicate want during a run preserves the original job");
        assert!(models.poll(&pool).is_none(), "poll drains starting progress without waiting for the held request");
        let snap = board().get(job).expect("starting job is on the board");
        assert_eq!(snap.title, "Getting 3D models", "board title names the action");
        assert_eq!(snap.stage, "C1 from the part service", "first uncached request names its source in generic terms");
        assert_eq!(snap.step, Some((1, 2)), "starting progress gives one-based step and total");
        assert_eq!(snap.fraction, Some(0.0), "first request begins at zero progress");
        models.cancel();
        assert!(models.run.as_ref().expect("cancelled run awaits completion").cancel.is_cancelled(), "cancel signals the fetch token");
        assert!(board().get(job).expect("cancelling job is on the board").cancelling, "cancel raises the board's cancelling flag");
        assert!(models.run.as_ref().unwrap().job.is_cancelled(), "cancel raises the job's cancel flag");
        drop(release);
        let result = done(&mut models, &pool);
        assert!(result.cancelled, "worker reports cancellation");
        assert!(result.models.is_empty() && result.failed.is_empty(), "cancelled request and unattempted number produce no model or failure");
        assert_eq!(result.line, "3D models: cancelled after 0 of 2 parts", "cancellation reports completed and requested totals");
        assert_eq!(fake.requests.load(Ordering::SeqCst), 1, "cancel prevents the later number's request");
        assert_eq!(fake.jobs.load(Ordering::SeqCst), 1, "overlapping start creates no extra fetcher");
        assert!(matches!(board().get(job).unwrap().end, JobEnd::Cancelled), "cancel ends the board job as cancelled");

        let (started, release) = fake.hold();
        models.want(&pool, paths(&["C1", "C4"]));
        started.recv_timeout(Duration::from_secs(60)).expect("new token reaches a fresh request");
        let job = models.job().expect("new run has its own job id");
        assert!(board().cancel(job), "the job board accepts cancellation");
        assert!(models.run.as_ref().unwrap().cancel.is_cancelled(), "board hook signals the worker's token");
        drop(release);
        assert!(done(&mut models, &pool).cancelled, "board cancellation also reaches completion");
        assert_eq!(fake.requests.load(Ordering::SeqCst), 2, "board cancellation also prevents later requests");

        models.want(&pool, paths(&["C4"]));
        let result = done(&mut models, &pool);
        assert!(!result.cancelled && result.models.len() == 1, "cancelled token is never reused by a later run");
        assert_eq!(fake.requests.load(Ordering::SeqCst), 3, "fresh run makes exactly its requested download");
    }

    /// Every numbered part on the open LED demo gets its library model. Model arrival leaves
    /// the project's files and undo history alone.
    #[test]
    fn the_open_ledavr_board_gets_its_eight_library_meshes_without_an_edit() {
        use crate::elec_host::ElecHost;
        use fab_elec_parts::BodySource;
        use makepad_widgets::Event;
        let root = Scratch::new();
        let _cx = crate::cx_shared();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        // the demo plate's electronics documents, as the Electronics workspace loads them
        let demo = crate::demo_part::DEMOS.iter().find(|demo| demo.key == "ledavr").expect("the LED demo");
        let docs: Arc<fab_model::Docs> = Arc::new(demo.files.iter()
            .filter(|(name, _)| fab_elec_lang::DocKind::of_name(name).is_some())
            .map(|(name, text)| (name.to_string(), Arc::<str>::from(*text))).collect());
        let plate = fab_model::FabModel::new("plate");
        let mut host = ElecHost::start(&mut cx, 1, None, docs, Arc::default(), plate.steps().clone(), plate.logged().clone());
        let idle = |host: &mut ElecHost, cx: &mut Cx| {
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                host.event(cx, &Event::Signal);
                if !host.busy() && host.design().is_some() {
                    return;
                }
                assert!(Instant::now() < deadline, "the electronics engine settles");
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        idle(&mut host, &mut cx);
        let missing = host.design().expect("ledavr evaluates").missing_meshes();
        let (wanted, other) = fetchable(&Lcsc, &missing);
        assert_eq!(wanted.len(), 8, "the open board asks for its eight missing models: {missing:?}");
        assert!(other.is_empty(), "every model the demo names has a fetchable part number: {other:?}");

        let pool = cx.task_pool();
        let fake = Fake::new(example_reply);
        let mut models = PartModels::with(&root.0, Arc::new(Example), fake.fetcher());
        models.want(&pool, wanted);
        let result = done(&mut models, &pool);
        assert!(result.failed.is_empty() && !result.cancelled, "every model arrives: {:?}", result.failed);
        assert_eq!(result.models.len(), 8, "the run returns all eight models");
        assert!(result.models.iter().all(|m| m.origin == Origin::Fetched), "all eight models are newly downloaded");
        assert!(result.line.is_empty(), "landing the demo's models produces no success notice");
        let keys: std::collections::BTreeSet<_> = models.library.meshes(&Example).into_iter()
            .map(|(_, file)| file.into_os_string().into_string().expect("test library path")).collect();
        assert_eq!(keys.len(), 8, "the test library holds all eight models");
        assert_eq!(fake.requests.load(Ordering::SeqCst), 8, "one synthetic request per missing number");
        let before = (host.undo_step(), host.redo_step());
        host.library_models(result.models.into_iter().map(|model| (model.path, model.key)).collect());
        idle(&mut host, &mut cx);
        assert_eq!((host.undo_step(), host.redo_step()), before, "the library refresh leaves history unchanged");
        assert!(host.take_docs().is_none(), "the host sends no document or asset changes to the project");

        let design = host.design().expect("the design after the models").clone();
        assert!(design.missing_meshes().is_empty(), "no model is missing: {:?}", design.missing_meshes());
        let mut used = std::collections::BTreeSet::new();
        for comp in design.units.iter().flat_map(|unit| unit.circuit.comps.iter()) {
            let body = comp.part.body.as_ref().unwrap_or_else(|| panic!("{}: every ledavr part has a body", comp.path));
            match &body.source {
                BodySource::Mesh(key) => {
                    assert!(keys.contains(key), "{}: its mesh is one the run fetched: {key}", comp.path);
                    used.insert(key.clone());
                }
                source => panic!("{}: expected its library mesh, got {source:?}", comp.path),
            }
        }
        assert_eq!(used.len(), 8, "the parts use all eight models");

        host.undo(&mut cx);
        idle(&mut host, &mut cx);
        assert!(host.design().expect("the design after undo").missing_meshes().is_empty(), "undo leaves the library models in place");
    }
}
