//! The score's tools (`score_*`) as the chat's model calls them: reading and
//! changing the score and preparing outputs answer through the frame pump. `score_guide`, `score_check`,
//! `score_listen` and `instrument_search` are the Score library's own
//! (`makepad_score_live::tools`); this shell gives them the open score.

use super::Desk;
use crate::library::{self, Document};
use makepad_score_live::{source, templates, tools, PrepareControl, NoSamples, RenderOpts};
use makepad_strict_json::{self as json, Value as Json};
use makepad_widgets::*;
use makepad_widgets::makepad_platform::thread::{SubmitError, TaskError};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A tool's answer: now, or when job `id` is done.
pub enum Answer {
    Done(Result<Json, String>),
    Later(u64),
}

fn ok(value: Json) -> Answer {
    Answer::Done(Ok(value))
}

fn refused(message: impl Into<String>) -> Answer {
    Answer::Done(Err(message.into()))
}

fn num(value: f64) -> Json {
    Json::F64((value * 1000.0).round() / 1000.0)
}

fn text(value: impl Into<String>) -> Json {
    json::s(value)
}

fn f64_arg(args: &Json, key: &str) -> Option<f64> {
    match args.get(key) {
        Some(Json::F64(n)) => Some(*n),
        Some(Json::Int(n)) => Some(*n as f64),
        _ => None,
    }
}

type JobResult = Result<Json, String>;

/// A retained frame job. The worker and its owner share the score request's word.
pub(super) struct RequestJob<T> {
    frame: FrameJob<T>,
    control: Option<PrepareControl>,
}

impl<T> RequestJob<T> {
    pub(super) fn submit(cx: &Cx, control: PrepareControl,
        work: impl FnOnce(&PrepareControl) -> T + Send + 'static,
    ) -> Result<Self, SubmitError> where T: Send + 'static {
        let worker = control.clone();
        let frame = cx.task_pool().submit_frame_job(Class::Visible, move || work(&worker))?;
        Ok(Self { frame, control: Some(control) })
    }

    pub(super) fn try_take(&mut self) -> Option<Result<T, TaskError>> {
        let result = self.frame.try_take()?;
        self.control = None;
        Some(result)
    }

    pub(super) fn word(&self) -> Option<&PrepareControl> {
        self.control.as_ref()
    }

    fn cancel_score(&mut self) {
        if let Some(control) = self.control.take() { control.cancel(); }
    }

    pub(super) fn cancel(&mut self) {
        self.cancel_score();
        self.frame.cancel();
    }
}

impl<T> Drop for RequestJob<T> {
    fn drop(&mut self) { self.cancel_score(); }
}

/// Observed only while this reply is pending in chat.
pub(crate) struct ReplyEvaluation {
    word: PrepareControl,
    since: Option<f64>,
}

impl ReplyEvaluation {
    pub(crate) fn seconds(&mut self, now: f64) -> Option<u64> {
        // A reply owns a fresh word and never renews it, so both counts belong to this call.
        if !self.word.script_running() && !self.word.executing() {
            self.since = None;
            return None;
        }
        let elapsed = now - *self.since.get_or_insert(now);
        (elapsed >= super::INIT_STATUS_DELAY).then_some(elapsed as u64)
    }
}

/// The worker returns data. Only the frame pump can install an accepted edit.
pub(crate) enum PreparedTool {
    Reply(Json),
    Open(Document, String),
    Edit { source: String, title: Option<String>, fields: Vec<(String, Json)> },
}

struct PendingTool {
    kind: &'static str,
    id: u64,
    revision: Option<u64>,
    job: RequestJob<Result<PreparedTool, String>>,
}

fn validate(source: &str, control: &makepad_score_live::PrepareControl) -> Result<(), String> {
    let problems = check(source, control)?;
    if problems.is_empty() { Ok(()) } else { Err(format!("the score does not load:\n{}", problems.join("\n"))) }
}

struct FileRequest {
    id: u64,
    document: u64,
    revision: u64,
    source: Arc<str>,
    // A render of a still-loading score resolves its default name on the pool.
    path: Option<PathBuf>,
    name_job: Option<RequestJob<Result<PathBuf, makepad_score_live::LoadError>>>,
    save: bool,
    default_save: bool,
    remote: bool,
    state: FileState,
}

enum FileState {
    Preparing(RequestJob<Result<(Vec<u8>, Json), String>>),
    Writing(FrameJob<JobResult>),
}

fn output_path(path: &Path) -> PathBuf {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    path.canonicalize().unwrap_or_else(|_| match (path.parent(), path.file_name()) {
        (Some(dir), Some(file)) => dir.canonicalize().unwrap_or_else(|_| dir.to_owned()).join(file),
        _ => path,
    })
}

fn wav_path(label: &str) -> PathBuf {
    let label = if label.trim().is_empty() { "score" } else { label };
    let stem: String = label.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect();
    output_path(&crate::paths::renders_dir().join(format!("{stem}.wav")))
}

/// Background work: renders and measurements.
#[derive(Default)]
pub struct Jobs {
    next: u64,
    pending: Vec<(u64, RequestJob<JobResult>)>,
    running: Vec<u64>,
    tools: Vec<PendingTool>,
    files: Vec<FileRequest>,
    document: u64,
    adopted: u64,
    /// Reads that wait for a pending revision: each is answered from the applied one.
    reads: Vec<(u64, bool)>,
    // A default filename may resolve after a newer request for that file has completed.
    resolved: HashMap<PathBuf, u64>,
    writers: std::collections::HashSet<PathBuf>,
    done: HashMap<u64, JobResult>,
    /// What the header says when a job ends (a render written).
    message: Option<String>,
    /// Samples and SoundFonts loaded since the last look.
    loaded: usize,
}

impl Desk {
    pub(crate) fn reply_evaluation(&self, id: u64) -> Option<ReplyEvaluation> {
        let word = self.jobs.word(id).or_else(|| self.vary.reply_word(id)).or_else(|| self.panel.reply_word(id))?;
        Some(ReplyEvaluation { word: word.clone(), since: None })
    }

    pub(crate) fn cancel_reply(&mut self, id: u64, reason: &str) {
        if self.jobs.stop_job(id) || self.vary.stop_reply(id) || self.panel.stop_reply(id) {
            self.jobs.panel_answer(id, Err(reason.into()));
        }
    }
}

impl Jobs {
    pub(super) fn cancel_score_requests(&mut self) {
        for (_, job) in &mut self.pending { job.cancel_score(); }
        for tool in &mut self.tools { tool.job.cancel_score(); }
        for request in &mut self.files {
            if let Some(job) = &mut request.name_job { job.cancel_score(); }
            if let FileState::Preparing(job) = &mut request.state { job.cancel_score(); }
        }
    }

    pub(super) fn open_document(&mut self) { self.document += 1; }

    fn remember_path(&mut self, path: &Path, id: u64) {
        let latest = self.resolved.entry(path.to_path_buf()).or_default();
        *latest = (*latest).max(id);
    }

    fn supersede_file(&mut self, index: usize) {
        let request = self.files.remove(index);
        if let FileState::Preparing(mut job) = request.state { job.cancel(); }
        if let Some(mut job) = request.name_job { job.cancel(); }
        if request.remote { self.panel_answer(request.id, Err("superseded".into())); }
        else { self.running.retain(|id| *id != request.id); }
    }

    fn finish_file(&mut self, request: &FileRequest, result: JobResult) {
        if request.remote { self.panel_answer(request.id, result); }
        else {
            self.running.retain(|id| *id != request.id);
            if let Err(error) = result { self.message = Some(error); }
        }
    }

    /// The same reply ID owns this read until it is answered or cancelled.
    fn submit_score_read(&mut self, cx: &Cx, kind: &'static str, revision: Option<u64>,
        read: impl FnOnce(&PrepareControl) -> Result<PreparedTool, String> + Send + 'static,
    ) -> Answer {
        let control = PrepareControl::default();
        self.submit_tool(cx, kind, revision, Some(control.clone()), move || read(&control))
    }

    fn word(&self, id: u64) -> Option<&PrepareControl> {
        if let Some(tool) = self.tools.iter().find(|tool| tool.id == id) { return tool.job.word(); }
        if let Some((_, job)) = self.pending.iter().find(|(request, _)| *request == id) { return job.word(); }
        match &self.files.iter().find(|out| out.id == id)?.state {
            FileState::Preparing(job) => job.word(),
            FileState::Writing(_) => None,
        }
    }

    fn stop_job(&mut self, id: u64) -> bool {
        if let Some(i) = self.tools.iter().position(|tool| tool.id == id) {
            self.tools.remove(i).job.cancel();
        } else if let Some(i) = self.pending.iter().position(|(request, job)| *request == id && job.word().is_some()) {
            self.pending.swap_remove(i).1.cancel();
        } else if let Some(i) = self.files.iter().position(|out| out.id == id && matches!(out.state, FileState::Preparing(_))) {
            let out = self.files.remove(i);
            if let Some(mut job) = out.name_job { job.cancel(); }
            if let FileState::Preparing(mut job) = out.state { job.cancel(); }
        } else if let Some(i) = self.reads.iter().position(|(request, _)| *request == id) {
            self.reads.swap_remove(i);
        } else {
            return false;
        }
        true
    }

    pub(crate) fn submit_tool(&mut self, cx: &Cx, kind: &'static str, revision: Option<u64>, control: Option<PrepareControl>,
        work: impl FnOnce() -> Result<PreparedTool, String> + Send + 'static,
    ) -> Answer {
        if let Some(index) = self.tools.iter().position(|pending| pending.kind == kind) {
            let mut old = self.tools.remove(index);
            old.job.cancel();
            self.panel_answer(old.id, Err("superseded".into()));
        }
        let job = match cx.task_pool().submit_frame_job(Class::Visible, work) {
            Ok(frame) => RequestJob { frame, control },
            Err(error) => return refused(error.to_string()),
        };
        let id = self.panel_request();
        self.tools.push(PendingTool { kind, id, revision, job });
        Answer::Later(id)
    }

    fn next_tool(&mut self, revision: u64, typing: bool) -> Option<(u64, Result<PreparedTool, String>)> {
        for (i, pending) in self.tools.iter_mut().enumerate() {
            let result = if pending.revision.is_some_and(|r| typing || r != revision) {
                pending.job.cancel();
                Err("superseded: the score changed while the request was being checked; read it again".into())
            } else if let Some(done) = pending.job.try_take() {
                done.map_err(|error| error.to_string()).and_then(|result| result)
            } else {
                continue;
            };
            return Some((self.tools.remove(i).id, result));
        }
        None
    }

    pub(super) fn panel_request(&mut self) -> u64 {
        self.next += 1;
        self.running.push(self.next);
        self.next
    }

    pub(super) fn panel_answer(&mut self, id: u64, result: Result<Json, String>) {
        self.running.retain(|r| *r != id);
        self.done.insert(id, result);
    }


    pub fn spawn(&mut self, cx: &Cx, class: Class, control: Option<PrepareControl>, work: impl FnOnce() -> JobResult + Send + 'static) -> u64 {
        let id = self.panel_request();
        match cx.task_pool().submit_frame_job(class, work) {
            Ok(frame) => self.pending.push((id, RequestJob { frame, control })),
            Err(error) => self.finish_job(id, Err(error.to_string())),
        }
        id
    }

    pub fn poll(&mut self) {
        let mut i = 0;
        while i < self.pending.len() {
            if let Some(done) = self.pending[i].1.try_take() {
                let (id, _) = self.pending.swap_remove(i);
                self.finish_job(id, done.map_err(|error| error.to_string()).and_then(|result| result));
            } else {
                i += 1;
            }
        }
    }

    fn finish_job(&mut self, id: u64, result: JobResult) {
        self.running.retain(|r| *r != id);
        match &result {
            Ok(value) => {
                if let Some(path) = value.get("path").and_then(Json::as_str) {
                    self.message = Some(format!("rendered to {path}"));
                }
                if let Some(n) = value.get("loaded").and_then(Json::as_u64) {
                    self.loaded += n as usize;
                }
                if let Some(Json::Arr(failed)) = value.get("failed") {
                    if let Some(first) = failed.first().and_then(Json::as_str) {
                        self.message = Some(first.to_string());
                    }
                }
            }
            Err(error) => self.message = Some(error.clone()),
        }
        self.done.insert(id, result);
    }

    pub fn busy(&self) -> bool {
        !self.running.is_empty()
    }

    /// How many samples and SoundFonts arrived since the last call.
    pub fn take_loaded(&mut self) -> usize {
        std::mem::take(&mut self.loaded)
    }

    pub fn finished_message(&mut self) -> Option<String> {
        self.message.take()
    }

    pub fn take(&mut self, id: u64) -> Option<JobResult> {
        self.poll();
        self.done.remove(&id)
    }
}

/// Render the WAV and its timing in memory before the writer receives them.
fn prepare_wav(src: &str, seconds: Option<f64>, seed: Option<u64>, control: &makepad_score_live::PrepareControl) -> Result<(Vec<u8>, Json), String> {
    let rendered = makepad_score_live::render(src, &RenderOpts { rate: 48_000.0, seconds, seed, params: Vec::new(), start: None, held: Vec::new() }, &NoSamples, control)
        .map_err(|e| e.describe(|diags| diags.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("; ")))?;
    let wav = makepad_audio_synth::wav::stereo_wav_16(&rendered.left, &rendered.right, 48_000);
    Ok((wav, json::obj(vec![
        ("seconds", num(rendered.seconds())),
        ("bpm", num(rendered.bpm)),
        ("beats_per_bar", num(rendered.beats_per_bar)),
        ("sections", Json::Arr(rendered.sections.iter().enumerate().map(|(i, (beat, name))| {
            let end = rendered.sections.get(i + 1).map_or(rendered.seconds(), |(next, _)| rendered.seconds_at(*next));
            json::obj(vec![("beat", num(*beat)), ("seconds", num(rendered.seconds_at(*beat))), ("end_seconds", num(end)), ("name", text(name.clone()))])
        }).collect())),
    ])))
}

fn write_file(path: &Path, bytes: Vec<u8>, mut reply: Json) -> JobResult {
    std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    if let Json::Obj(fields) = &mut reply { fields.push(("path".into(), text(path.to_string_lossy()))); }
    Ok(reply)
}

/// The longest sample decoded: a minute at 48 kHz.
const MAX_SAMPLE_FRAMES: usize = 48_000 * 60;
/// The largest SoundFont read.
const MAX_FONT_BYTES: u64 = 256 << 20;

/// The samples and SoundFonts a score names, read from the samples folder
/// (`asset("<file>", "file")`), decoded and registered under their names.
pub fn load_samples(assets: Vec<makepad_score_live::AssetRef>) -> JobResult {
    load_from(&crate::paths::samples_dir(), assets)
}

/// [`load_samples`] from the folder `dir`.
fn load_from(dir: &std::path::Path, assets: Vec<makepad_score_live::AssetRef>) -> JobResult {
    let (mut loaded, mut failed) = (0u64, Vec::new());
    for asset in assets {
        let got = (|| -> Result<(), String> {
            // A sample of a chip or tracker song opened before.
            if let Some(sample) = makepad_midi_library::chip::import::stored(&asset) {
                makepad_score_live::render::register_sample(&asset, sample);
                return Ok(());
            }
            if asset.rev != library::FILE_REV {
                return Err(format!("`asset(\"{}\", \"{}\")`: a sample here is a file of the samples folder, asset(\"<file>\", \"{}\")", asset.id, asset.rev, library::FILE_REV));
            }
            let path = dir.join(&asset.id);
            let len = std::fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?.len();
            let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"sfbk") {
                if len > MAX_FONT_BYTES {
                    return Err(format!("{}: the SoundFont is {len} bytes", asset.id));
                }
                makepad_score_live::render::register_file(&asset, bytes.into());
                return Ok(());
            }
            let audio = makepad_audio_decode::decode_any(&bytes).map_err(|e| format!("{}: {e:?}", asset.id))?;
            let channels = audio.channels.max(1) as usize;
            let frames = audio.frames().min(MAX_SAMPLE_FRAMES);
            let pcm = &audio.pcm_interleaved_f32;
            let left: Vec<f32> = (0..frames).map(|i| pcm[i * channels]).collect();
            let right: Option<Vec<f32>> = (channels > 1).then(|| (0..frames).map(|i| pcm[i * channels + 1]).collect());
            let sample = makepad_score_player::Sample::new(left.into(), right.map(Into::into), audio.rate.max(1) as f32);
            makepad_score_live::render::register_sample(&asset, std::sync::Arc::new(sample));
            Ok(())
        })();
        match got {
            Ok(()) => loaded += 1,
            Err(error) => failed.push(text(error)),
        }
    }
    Ok(json::obj(vec![("loaded", Json::Int(loaded as i64)), ("failed", Json::Arr(failed))]))
}

/// What changed from `before` to `after`, as lines: the changed span between
/// the lines both keep at the start and the end, `context` lines around it,
/// `-` for a line that went and `+` for one that came (at most 80 lines).
pub fn changed_lines(before: &str, after: &str, context: usize) -> String {
    let (a, b): (Vec<&str>, Vec<&str>) = (before.lines().collect(), after.lines().collect());
    let head = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let tail = a[head..].iter().rev().zip(b[head..].iter().rev()).take_while(|(x, y)| x == y).count();
    if head == a.len() && head == b.len() {
        return String::new();
    }
    let from = head.saturating_sub(context);
    let mut out = vec![format!("@@ line {} @@", from + 1)];
    out.extend(a[from..head].iter().map(|l| format!(" {l}")));
    out.extend(a[head..a.len() - tail].iter().map(|l| format!("-{l}")));
    out.extend(b[head..b.len() - tail].iter().map(|l| format!("+{l}")));
    out.extend(b[b.len() - tail..].iter().take(context).map(|l| format!(" {l}")));
    if out.len() > 80 {
        let more = out.len() - 80;
        out.truncate(80);
        out.push(format!("… {more} more lines"));
    }
    out.join("\n")
}

/// The problems that keep `source_text` from being written: the library's
/// `score_check` (its loops run for 8 bars).
pub fn check(source_text: &str, control: &makepad_score_live::PrepareControl) -> Result<Vec<String>, String> {
    Ok(tools::check_diags(source_text, control).map_err(|e| e.to_string())?.iter().map(|d| d.to_string()).collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn replacing_and_dropping_jobs_cancels_their_score_words() {
        use super::*;
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut jobs = Jobs::default();
        let old = PrepareControl::default();
        let current = PrepareControl::default();
        let listen = PrepareControl::default();
        let reply = || Ok(PreparedTool::Reply(Json::Null));

        let Answer::Later(old_id) = jobs.submit_tool(&cx, "check", None, Some(old.clone()), reply) else {
            panic!("the first check should start")
        };
        jobs.spawn(&cx, Class::Visible, Some(listen.clone()), || Ok(Json::Null));
        assert!(matches!(jobs.submit_tool(&cx, "check", None, Some(current.clone()), reply), Answer::Later(_)));
        assert!(old.is_cancelled(), "replacing the check stops its score work");
        assert!(jobs.done.remove(&old_id).unwrap().unwrap_err().contains("superseded"), "the replaced check answers its caller");
        assert!(!current.is_cancelled(), "the replacement owns a fresh word");
        assert!(!listen.is_cancelled(), "replacing a check leaves the listen running");
        drop(jobs);
        assert!(current.is_cancelled(), "dropping the jobs stops the retained check");
        assert!(listen.is_cancelled(), "dropping the jobs also stops the independent listen");

        // Replies get jobs of their own: `take` polls, and a poll would collect the listen once the pool finished it.
        let mut replies = Desk::default();
        let other = PrepareControl::default();
        assert!(matches!(replies.jobs.submit_tool(&cx, "check", None, Some(other.clone()), reply), Answer::Later(_)));
        let Answer::Later(id) = replies.jobs.submit_score_read(&cx, "score_text", None, |_| Ok(PreparedTool::Reply(Json::Null))) else {
            panic!("the transcription should start")
        };
        let word = replies.jobs.tools.iter().find(|tool| tool.id == id).unwrap().job.control.clone().unwrap();
        replies.cancel_reply(id, "the transcription was cancelled");
        assert!(word.is_cancelled(), "the reply ID reaches the transcription's VM word");
        assert_eq!(replies.jobs.take(id).unwrap().unwrap_err(), "the transcription was cancelled");
        assert!(replies.jobs.take(id).is_none(), "the stopped call answers exactly once");
        assert!(!other.is_cancelled(), "stopping one reply leaves the other reply running");
        drop(replies);
        let mut desk = Desk::default();
        assert!(matches!(desk.file_request(&mut cx, None, false, false, None, None, true), Answer::Later(_)));
        let file = &desk.jobs.files[0];
        let name = file.name_job.as_ref().expect("an unapplied source needs a name job").control.as_ref().unwrap();
        let FileState::Preparing(data) = &file.state else { panic!("the file is still preparing") };
        let render = data.control.clone().unwrap();
        let id = file.id;
        name.cancel();
        assert!(render.is_cancelled(), "the name and WAV data run under the same request's word");
        desk.cancel_reply(id, "render cancelled");
        assert!(desk.jobs.files.is_empty(), "a stopped preparation can never hand off to the WAV writer");
        assert_eq!(desk.jobs.take(id).unwrap().unwrap_err(), "render cancelled");
        super::super::tracker_desk::check_replaced_tracker_compile(&cx);
    }

    #[test]
    fn changed_lines_show_the_changed_span_with_context() {
        let diff = super::changed_lines("a\nb\nc\nd\ne", "a\nb\nX\nd\ne", 1);
        assert_eq!(diff, "@@ line 2 @@\n b\n-c\n+X\n d");
        assert_eq!(super::changed_lines("a", "a", 2), "");
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("synth-test-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Export: a template rendered to a WAV file, with its timing.
    #[test]
    fn a_score_renders_to_a_wav_file() {
        let dir = temp_dir("render");
        let path = dir.join("techno.wav");
        let source = makepad_score_live::templates::template("techno").unwrap();
        let (wav, out) = super::prepare_wav(source, Some(2.0), None, &makepad_score_live::PrepareControl::default()).unwrap();
        let out = super::write_file(&path, wav, out).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WAVE");
        // 2 s of 48 kHz stereo 16-bit, give or take the tail.
        assert!(bytes.len() > 48_000 * 4, "{} bytes", bytes.len());
        assert!(out.get("seconds").and_then(|v| match v { super::Json::F64(f) => Some(*f), _ => None }).is_some_and(|s| s >= 2.0));
        std::fs::remove_file(&path).unwrap();
    }

    /// A sample named by file is decoded from the samples folder and
    /// registered: the score no longer misses it.
    #[test]
    fn a_sample_file_is_loaded_by_name() {
        let dir = temp_dir("samples");
        let tone: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        std::fs::write(dir.join("synth_test_tone.wav"), makepad_audio_synth::wav::stereo_wav_16(&tone, &tone, 48_000)).unwrap();
        let asset = makepad_score_live::AssetRef { id: "synth_test_tone.wav".into(), rev: crate::library::FILE_REV.into() };
        let out = super::load_from(&dir, vec![asset.clone()]).unwrap();
        assert_eq!(out.get("loaded").and_then(super::Json::as_u64), Some(1), "{}", out.to_json());
        let wrong = makepad_score_live::AssetRef { id: "x".into(), rev: "7".into() };
        let out = super::load_from(&dir, vec![wrong]).unwrap();
        assert_eq!(out.get("loaded").and_then(super::Json::as_u64), Some(0));
        std::fs::remove_file(dir.join("synth_test_tone.wav")).unwrap();
    }
}

impl Desk {
    /// The open score for the model: its shape, its problems and (asked)
    /// its source.
    pub fn read_json(&self, with_source: bool) -> Json {
        let revision = self.loads.applied.as_ref().map_or(0, |request| request.revision);
        let mut out = vec![("revision", Json::Int(revision as i64)), ("title", text(self.applied_title.clone()))];
        if let Some(doc) = self.doc.as_ref().filter(|_| self.loads.diags.is_empty()) {
            out.push(("bpm", num(doc.clock.bpm)));
            out.push(("sig", text(format!("{}/{}", doc.clock.sig.0, doc.clock.sig.1))));
            out.push(("seed", Json::Int(doc.clock.seed as i64)));
            out.push(("params", Json::Obj(doc.clock.params.iter().map(|(n, v)| (n.clone(), num(*v as f64))).collect())));
            out.push(("nodes", Json::Arr(doc.order.iter().map(|(n, t)| text(format!("{n}: {t}"))).collect())));
            out.push(("loops", Json::Arr(doc.loops.iter().map(|l| json::obj(vec![
                ("name", text(l.name.clone())),
                ("play", text(l.play.clone().unwrap_or_default())),
                ("beats", l.beats.map_or(Json::Null, num)),
                ("amp", num(l.amp as f64)),
                ("pan", num(l.pan as f64)),
                ("mute", Json::Bool(l.mute)),
                ("solo", Json::Bool(l.solo)),
                ("kind", text(if l.stinger.is_some() { "stinger" } else if l.code.is_some() { "code" } else { "pattern" })),
            ])).collect())));
            out.push(("sections", Json::Arr(doc.sections.iter().map(|s| text(format!("{}: {} bars [{}]", s.name, s.bars, s.loops.join(" ")))).collect())));
            out.push(("arrange", Json::Arr(doc.arrange.iter().map(|a| text(a.clone())).collect())));
        }
        out.push(("problems", Json::Arr(self.problems().into_iter().map(text).collect())));
        if with_source {
            out.push(("source", text(self.loads.applied.as_ref().map_or("", |r| &r.source).to_string())));
        }
        json::obj(out)
    }

    fn problems(&self) -> Vec<String> {
        self.status.document_problems(&self.loads)
    }

    pub(crate) fn midi_tool(&mut self, cx: &mut Cx, name: &str, args: &Json) -> Answer {
        let Some(name) = makepad_midi_library::tools::TOOLS.iter().find(|tool| tool.name == name).map(|t| t.name) else { return refused("no such music tool"); };
        let args = args.clone();
        self.frame = cx.new_next_frame();
        if name == "score_text" {
            let open = self.source.clone();
            let programs = self.loads.shaders.clone().filter(|_| self.loads.diags.is_empty() && self.loads.applied.as_ref().is_some_and(|r| r.revision == self.revision));
            return self.jobs.submit_score_read(cx, name, None, move |control| {
                makepad_midi_library::tools::score_text_controlled(&args, Some(&open), programs.as_deref(), control).map(PreparedTool::Reply)
            });
        }
        self.jobs.submit_tool(cx, name, None, None, move || {
            makepad_midi_library::tools::call_library(name, &args).map(PreparedTool::Reply)
        })
    }

    pub(super) fn save_file(&mut self, cx: &mut Cx, name: Option<&str>, path: Option<PathBuf>, remote: bool) -> Answer {
        let default_save = name.is_none() && path.is_none();
        let path = path.or_else(|| default_save.then(|| self.document.path.clone()).flatten())
            .or_else(|| default_save.then(|| self.jobs.files.iter().find(|r| r.default_save && r.document == self.jobs.document)?.path.clone()).flatten())
            .unwrap_or_else(|| library::free_path(name.unwrap_or(&self.document.title), |p| self.jobs.files.iter().any(|r| r.path.as_deref() == Some(p))));
        self.file_request(cx, Some(path), true, default_save, None, None, remote)
    }

    pub(super) fn file_request(&mut self, cx: &mut Cx, path: Option<PathBuf>, save: bool, default_save: bool,
        seconds: Option<f64>, seed: Option<u64>, remote: bool,
    ) -> Answer {
        let source: Arc<str> = Arc::from(self.source.as_str());
        let mut path = path.map(|p| output_path(&p));
        let control = PrepareControl::default();
        let mut name_job = None;
        if path.is_none() {
            if self.loads.applied.as_ref().is_some_and(|r| r.revision == self.revision) {
                path = Some(wav_path(self.doc.as_ref().map_or("", |d| d.label.as_str())));
            } else {
                let text = source.clone();
                name_job = match RequestJob::submit(cx, control.clone(), move |control| {
                    match makepad_score_live::model::evaluated_label_controlled(&text, control) {
                        Ok(label) => Ok(wav_path(label.as_deref().unwrap_or(""))),
                        Err(makepad_score_live::LoadError::Problems(_)) => Ok(wav_path("")),
                        Err(cancelled) => Err(cancelled),
                    }
                }) {
                    Ok(job) => Some(job), Err(error) => return refused(error.to_string()),
                };
            }
        }
        if let Some(path) = &path {
            if let Some(i) = self.jobs.files.iter().position(|r| r.path.as_ref() == Some(path) && matches!(r.state, FileState::Preparing(_))) {
                self.jobs.supersede_file(i);
            }
        }
        let src = source.clone();
        let job = match RequestJob::submit(cx, control, move |control| {
            if save {
                // Saving a draft also preserves scores with diagnostics.
                Ok((src.as_bytes().to_vec(), Json::Obj(Vec::new())))
            } else {
                validate(&src, control)?;
                prepare_wav(&src, seconds, seed, control)
            }
        }) {
            Ok(job) => job,
            Err(error) => return refused(error.to_string()),
        };
        let id = self.jobs.panel_request();
        if let Some(path) = &path {
            self.jobs.remember_path(path, id);
        }
        self.jobs.files.push(FileRequest { id, source, revision: self.revision, document: self.jobs.document,
            path, name_job, save, default_save, remote, state: FileState::Preparing(job) });
        self.frame = cx.new_next_frame();
        Answer::Later(id)
    }

    fn pump_files(&mut self, cx: &mut Cx) {
        let jobs = &mut self.jobs;
        let mut i = 0;
        while i < jobs.files.len() {
            let file = &mut jobs.files[i];
            if let Some(answer) = file.name_job.as_mut().and_then(RequestJob::try_take) {
                file.name_job = None;
                match answer.map_err(|e| e.to_string()).and_then(|result| result.map_err(|e| e.to_string())) {
                    Ok(path) => {
                        let id = file.id;
                        file.path = Some(path.clone());
                        jobs.remember_path(&path, id);
                    }
                    Err(error) => {
                        let request = jobs.files.remove(i);
                        jobs.finish_file(&request, Err(error));
                        continue;
                    }
                }
            }
            i += 1;
        }
        let mut i = 0;
        while i < jobs.files.len() {
            let Some(path) = jobs.files[i].path.clone() else { i += 1; continue };
            let newer = jobs.resolved.get(&path).is_some_and(|id| *id > jobs.files[i].id);
            let writing = jobs.writers.contains(&path);
            let file = &mut jobs.files[i];
            let answer = match &mut file.state {
                FileState::Preparing(_) if newer => {
                    jobs.supersede_file(i);
                    continue;
                }
                FileState::Preparing(job) if !writing => match job.try_take() {
                    Some(Ok(Ok((bytes, reply)))) => {
                        let destination = path.clone();
                        match cx.task_pool().submit_frame_job(Class::Visible, move || write_file(&destination, bytes, reply)) {
                            Ok(writer) => {
                                jobs.writers.insert(path.clone());
                                file.state = FileState::Writing(writer);
                                None
                            }
                            Err(e) => Some(Err(e.to_string())),
                        }
                    }
                    Some(Ok(Err(error))) => Some(Err(error)),
                    Some(Err(error)) => Some(Err(error.to_string())),
                    None => None,
                },
                FileState::Writing(job) => job.try_take().map(|r| r.map_err(|e| e.to_string()).and_then(|v| v)),
                _ => None,
            };
            if let Some(mut answer) = answer {
                let request = jobs.files.remove(i);
                if matches!(request.state, FileState::Writing(_)) { jobs.writers.remove(&path); }
                if let Ok(Json::Obj(fields)) = &mut answer {
                    fields.push(("revision".into(), Json::Int(request.revision as i64)));
                    if request.save {
                        self.lib.dirty = true;
                        if request.document == jobs.document && request.id >= jobs.adopted {
                            jobs.adopted = request.id;
                            self.document.saved_to(request.source.to_string(), request.path.clone().unwrap(), &self.source);
                        }
                    }
                }
                jobs.finish_file(&request, answer);
            } else { i += 1; }
        }
        if jobs.files.is_empty() { jobs.resolved.clear(); }
        else if jobs.resolved.len() > jobs.files.len() && jobs.files.iter().all(|r| r.path.is_some()) {
            jobs.resolved.clear();
            for r in &jobs.files {
                if let Some(path) = &r.path { jobs.resolved.insert(path.clone(), r.id); }
            }
        }
        if !jobs.files.is_empty() { self.frame = cx.new_next_frame(); }
    }

    pub(super) fn pump_tools(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        self.pump_files(cx);
        if !self.typed && !self.loads.pending() {
            for (id, with_source) in std::mem::take(&mut self.jobs.reads) {
                self.jobs.panel_answer(id, Ok(self.read_json(with_source)));
            }
        }
        while let Some((id, result)) = self.jobs.next_tool(self.revision, self.typed) {
            let result = result.map(|done| match done {
                PreparedTool::Reply(value) => value,
                PreparedTool::Open(document, source) => {
                    self.open_document(cx, ui, document, source);
                    json::obj(vec![("title", text(self.document.title.clone())), ("revision", Json::Int(self.revision as i64))])
                }
                PreparedTool::Edit { source, title, mut fields } => {
                    if let Some(title) = title {
                        self.open_document(cx, ui, Document::new(&title), source);
                    } else {
                        let diff = changed_lines(&self.source, &source, 2);
                        self.set_source(cx, ui, source);
                        fields.push(("diff".into(), text(diff)));
                    }
                    fields.push(("revision".into(), Json::Int(self.revision as i64)));
                    Json::Obj(fields)
                }
            });
            self.jobs.panel_answer(id, result);
        }
        if !self.jobs.tools.is_empty() { self.frame = cx.new_next_frame(); }
    }

    fn write(&mut self, cx: &mut Cx, next: String, base: Option<i64>, title: Option<String>) -> Answer {
        if let Some(base) = base {
            if base != self.revision as i64 {
                return refused(format!("the score changed since revision {base} (it is {} now): read it again", self.revision));
            }
        }
        self.frame = cx.new_next_frame();
        let control = makepad_score_live::PrepareControl::default();
        self.jobs.submit_tool(cx, "edit", Some(self.revision), Some(control.clone()), move || {
            validate(&next, &control)?;
            Ok(PreparedTool::Edit { source: next, title, fields: Vec::new() })
        })
    }

    /// A tool of the score, run.
    pub fn tool(&mut self, cx: &mut Cx, ui: &WidgetRef, name: &str, args: &Json) -> Answer {
        let base = args.get("base_revision").and_then(Json::as_i64);
        match name {
            "score_check" => {
                let open = self.source.clone();
                let args = args.clone();
                self.frame = cx.new_next_frame();
                let control = makepad_score_live::PrepareControl::default();
                self.jobs.submit_tool(cx, "check", None, Some(control.clone()), move || tools::call("score_check", &args, Some(&open), &control).map(PreparedTool::Reply))
            }
            "score_guide" | "instrument_search" | "morph_params" | "tandem_params" | "tandem_patches" | "preset_search" => match tools::metadata_only(name, args) {
                Ok(value) => ok(value),
                Err(message) => refused(message),
            },
            "score_panel_guide" => ok(json::obj(vec![("guide", text(super::panel::guide()))])),
            "score_read" => {
                let with_source = args.get("with_source").and_then(Json::as_bool) != Some(false);
                if !self.typed && !self.loads.pending() { return ok(self.read_json(with_source)); }
                let id = self.jobs.panel_request();
                self.jobs.reads.push((id, with_source));
                self.frame = cx.new_next_frame();
                Answer::Later(id)
            }
            "score_write" => match args.get("source").and_then(Json::as_str) {
                Some(next) => self.write(cx, next.to_string(), base, None),
                None => refused("`source`: the whole score"),
            },
            "score_patch" => {
                if let Some(base) = base {
                    if base != self.revision as i64 {
                        return refused(format!("the score changed since revision {base} (it is {} now): read it again", self.revision));
                    }
                }
                let Some(Json::Arr(edits)) = args.get("edits") else { return refused("`edits`: [{old, new}]") };
                let edits = edits.clone();
                let mut next = self.source.clone();
                self.frame = cx.new_next_frame();
                let control = makepad_score_live::PrepareControl::default();
                self.jobs.submit_tool(cx, "edit", Some(self.revision), Some(control.clone()), move || {
                    for (i, edit) in edits.iter().enumerate() {
                        let (Some(old), Some(new)) = (edit.get("old").and_then(Json::as_str), edit.get("new").and_then(Json::as_str)) else {
                            return Err(format!("edit {}: `old` and `new`", i + 1));
                        };
                        match next.matches(old).count() {
                            1 => next = next.replacen(old, new, 1),
                            0 => return Err(format!("edit {}: `old` is not in the score (read it again)", i + 1)),
                            n => return Err(format!("edit {}: `old` is in the score {n} times: give more of it", i + 1)),
                        }
                    }
                    validate(&next, &control)?;
                    Ok(PreparedTool::Edit { source: next, title: None, fields: Vec::new() })
                })
            }
            "score_new" => {
                let template = args.get("template").and_then(Json::as_str).unwrap_or("blank");
                let Some(mut next) = templates::template(template).map(str::to_string) else { return refused(format!("no template `{template}`")) };
                if let Some(bpm) = f64_arg(args, "bpm") {
                    if let Ok(s) = source::set_field(&next, "clock", "bpm", &makepad_edit_values::number::fmt_num(bpm, 1)) {
                        next = match makepad_edit_values::apply_splices(&next, 0, &[s]) {
                            Ok(next) => next,
                            Err(error) => return refused(error),
                        };
                    }
                }
                if let Some(label) = args.get("label").and_then(Json::as_str) {
                    if let Ok(s) = source::set_field(&next, "", "label", &makepad_editor_panel::fmt_text(label)) {
                        next = match makepad_edit_values::apply_splices(&next, 0, &[s]) {
                            Ok(next) => next,
                            Err(error) => return refused(error),
                        };
                    }
                }
                let title = args.get("label").and_then(Json::as_str).filter(|label| !label.trim().is_empty()).unwrap_or("Untitled");
                self.write(cx, next, None, Some(title.to_owned()))
            }
            "score_play" => {
                let play = args.get("play").and_then(Json::as_bool).unwrap_or(true);
                if let Some(section) = args.get("section").and_then(Json::as_str) {
                    if let Some(live) = &self.live {
                        live.goto_section(section, 0.0);
                    }
                } else if let Some(bar) = f64_arg(args, "bar") {
                    let per_bar = self.doc.as_ref().map_or(4.0, |d| d.beats_per_bar());
                    self.seek(bar.max(0.0) * per_bar, 0.0);
                }
                self.transport(play);
                self.frame = cx.new_next_frame();
                ok(json::obj(vec![("playing", Json::Bool(play))]))
            }
            "score_rewind" => {
                self.rewind();
                self.frame = cx.new_next_frame();
                ok(json::obj(vec![("playing", Json::Bool(self.engine.playing())), ("beat", Json::Int(0))]))
            }
            "score_status" => {
                use makepad_score_player::Meters;
                let status = &self.status;
                let meters = self.engine.meters.clone();
                let db = |x: f32| num(if x > 1e-6 { 20.0 * x.log10() as f64 } else { -90.0 });
                let bar = self.doc.as_ref().map_or(4.0, |d| d.beats_per_bar());
                let levels: Vec<(String, Json)> = status.loops.iter().map(|l| {
                    let (peak, rms) = meters.level(l.key).unwrap_or((0.0, 0.0));
                    (l.name.clone(), json::obj(vec![("peak_db", db(peak)), ("rms_db", db(rms))]))
                }).collect();
                let master = meters.level(crate::audio::MASTER_KEY).unwrap_or((0.0, 0.0));
                ok(json::obj(vec![
                    ("playing", Json::Bool(status.playing)),
                    ("bar", Json::Int((status.beat / bar).floor() as i64)),
                    ("beat_in_bar", num(status.beat - (status.beat / bar).floor() * bar)),
                    ("bpm", num(status.bpm)),
                    ("section", status.section.clone().map_or(Json::Null, text)),
                    ("loops", Json::Arr(status.loops.iter().map(|l| json::obj(vec![
                        ("name", text(l.name.clone())), ("cycle", Json::Int(l.cycle as i64)), ("beats", num(l.len)), ("resting", Json::Bool(l.resting)),
                        ("error", l.error.as_ref().map_or(Json::Null, |e| text(e.to_string()))),
                        ("log", Json::Arr(l.logs.iter().map(|line| text(line.clone())).collect())),
                    ])).collect())),
                    ("levels", Json::Obj(levels)),
                    ("master", json::obj(vec![("peak_db", db(master.0)), ("rms_db", db(master.1))])),
                    ("params", Json::Obj(status.params.iter().map(|(n, v)| (n.clone(), num(*v as f64))).collect())),
                    ("dropped_events", Json::Int(status.dropped as i64)),
                    ("audio_device", Json::Bool(self.engine.device_rate().is_some())),
                    ("problems", Json::Arr(self.problems().into_iter().map(text).collect())),
                ]))
            }
            "score_listen" => {
                let args = args.clone();
                let open = self.source.clone();
                self.frame = cx.new_next_frame();
                let control = makepad_score_live::PrepareControl::default();
                Answer::Later(self.jobs.spawn(cx, Class::Visible, Some(control.clone()), move || tools::listen_args(&args, Some(&open), &control).map(|(value, _)| value)))
            }
            "score_set" => {
                let Some(live) = &self.live else { return refused("the score is not loaded") };
                let mut done = Vec::new();
                if let (Some(param), Some(value)) = (args.get("param").and_then(Json::as_str), f64_arg(args, "value")) {
                    live.set_param(param, value as f32);
                    done.push(format!("{param} = {value}"));
                }
                if let Some(section) = args.get("section").and_then(Json::as_str) {
                    live.goto_section(section, -1.0);
                    done.push(format!("to section {section} on the next bar"));
                }
                if let Some(stinger) = args.get("trigger").and_then(Json::as_str) {
                    live.trigger(stinger);
                    done.push(format!("stinger {stinger}"));
                }
                ok(json::obj(vec![("done", Json::Arr(done.into_iter().map(text).collect()))]))
            }
            "score_render" => {
                let seconds = f64_arg(args, "seconds");
                let seed = args.get("seed").and_then(Json::as_u64);
                let path = args.get("path").and_then(Json::as_str).filter(|p| !p.trim().is_empty()).map(std::path::PathBuf::from);
                self.file_request(cx, path, false, false, seconds, seed, true)
            }
            "score_save" => {
                let name = args.get("name").and_then(Json::as_str).filter(|n| !n.trim().is_empty());
                self.save_file(cx, name, None, true)
            }
            "score_library" => {
                self.frame = cx.new_next_frame();
                self.jobs.submit_score_read(cx, "library", None, |control| Ok(PreparedTool::Reply(json::obj(vec![
                    ("scores", Json::Arr(library::saved_scores(control).map_err(|e| e.to_string())?.into_iter().map(|(title, path)| {
                        json::obj(vec![("title", text(title)), ("name", text(path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()))])
                    }).collect()))
                ]))))
            }
            "score_open" => {
                let Some(name) = args.get("name").and_then(Json::as_str) else { return refused("`name`: a score of score_library") };
                let name = name.to_owned();
                self.frame = cx.new_next_frame();
                self.jobs.submit_score_read(cx, "edit", Some(self.revision), move |control| {
                    let path = library::find(&name, control).map_err(|e| e.to_string())?.ok_or_else(|| format!("no score `{name}` in the library"))?;
                    let (doc, source) = Document::open_file(&path)?;
                    Ok(PreparedTool::Open(doc, source))
                })
            }
            "score_editor_make" => match self.make_panel(cx) {
                Ok(()) => ok(json::obj(vec![("asked", Json::Bool(true))])),
                Err(why) => refused(why),
            },
            "score_editor_write" => {
                let Some(code) = args.get("code").and_then(Json::as_str) else { return refused("`code`: the whole `let editor_panel = fn() { View{…} }`") };
                match super::panel::with_panel(&self.source, code) {
                    Ok(next) => self.write(cx, next, None, None),
                    Err(error) => refused(error),
                }
            }
            "score_editor_paths" => ok(json::obj(vec![("paths", text(super::panel::score_listing(&self.source, self.doc.as_ref())))])),
            "score_panel_drive" => {
                let Some(path) = args.get("path").and_then(Json::as_str) else { return refused("`path`") };
                let values: Vec<f64> = match args.get("values") {
                    Some(Json::Arr(items)) => items.iter().filter_map(|v| match v {
                        Json::Int(n) => Some(*n as f64),
                        Json::F64(n) => Some(*n),
                        _ => None,
                    }).collect(),
                    _ => Vec::new(),
                };
                if values.is_empty() {
                    return refused("`values`: at least one number");
                }
                let hold = args.get("hold").and_then(Json::as_bool) == Some(true);
                let n = values.len();
                self.panel.drive(path, values, hold);
                self.frame = cx.new_next_frame();
                ok(json::obj(vec![("queued", Json::Int(n as i64))]))
            }
            "score_panel_status" => ok(self.panel_status_json()),
            "score_vary" => {
                use makepad_synth_panel::host::VaryAction;
                let Some(node) = args.get("voice").and_then(Json::as_str).map(str::to_string) else { return refused("`voice`: a loop of the score") };
                let mut region = None;
                let card = || f64_arg(args, "card").map(|c| c.max(0.0) as usize);
                let action = match args.get("action").and_then(Json::as_str).unwrap_or("make") {
                    "make" => {
                        let (kind, ai) = match (args.get("kind").and_then(Json::as_str), args.get("ask").and_then(Json::as_str)) {
                            (_, Some(ask)) => (ask.to_string(), true),
                            (Some(kind), None) => (kind.to_string(), false),
                            (None, None) => ("variations".to_string(), false),
                        };
                        if let Some(bars) = args.get("bars").and_then(Json::as_arr) {
                            let bar = self.doc.as_ref().map_or(4.0, |d| d.beats_per_bar());
                            let n = |k: usize| bars.get(k).and_then(|v| match v { Json::Int(i) => Some(*i as f64), Json::F64(f) => Some(*f), _ => None });
                            if let (Some(a), Some(b)) = (n(0), n(1)) {
                                region = Some(((a - 1.0).max(0.0) * bar, b * bar));
                            }
                        }
                        VaryAction::Make { kind, ai, n: f64_arg(args, "n").map_or(4, |n| n.clamp(1.0, 8.0) as usize) }
                    }
                    "audition" => VaryAction::Audition(card()),
                    "keep" => VaryAction::Keep(card().unwrap_or(0)),
                    "apply" => VaryAction::Apply(card().unwrap_or(0)),
                    "merge" => VaryAction::Merge(match args.get("picks") { Some(Json::Arr(items)) => items.iter().filter_map(|v| match v { Json::Int(i) => Some(*i as usize), Json::F64(f) => Some(*f as usize), _ => None }).collect(), _ => Vec::new() }),
                    "clear" => VaryAction::Clear,
                    other => return refused(format!("no action `{other}`: make, audition, keep, apply, merge, clear")),
                };
                self.frame = cx.new_next_frame();
                let id = self.jobs.panel_request();
                match self.vary_action(cx, &node, Some(action), region, Some(id)) {
                    Ok(()) => Answer::Later(id),
                    Err(error) => { self.jobs.panel_answer(id, Err(error)); Answer::Later(id) }
                }
            }
            "score_vary_status" => {
                let Some(node) = args.get("voice").and_then(Json::as_str) else { return refused("`voice`") };
                let view = self.vary.view(node);
                let voice = self.vary.voices.get(node);
                let parts: Vec<_> = voice.map(|voice| voice.original.iter().cloned().chain(voice.cards.iter().map(|c| c.1.clone())).collect()).unwrap_or_default();
                // What the composer plays of this voice now, and whether it
                // is the card playing.
                let Some(applied) = &self.loads.applied else { return refused("no applied score") };
                let revision = applied.revision;
                let Some(programs) = self.variant_shaders.clone().or_else(|| self.loads.shaders.clone()) else { return refused("no prepared shaders") };
                let played = self.variant_live.clone().unwrap_or_else(|| applied.source.to_string());
                let audition = self.variant_live.is_some();
                let with_source = args.get("with_source").and_then(Json::as_bool) == Some(true);
                let ai_made = self.vary_ai_made;
                let valid = self.loads.diags.is_empty();
                let node = node.to_string();
                let playing_part = view.playing.and_then(|i| parts.get(i).cloned());
                self.frame = cx.new_next_frame();
                self.jobs.submit_score_read(cx, "vary_status", None, move |control| {
                    let cards: Vec<Json> = view.cards.iter().zip(&parts).enumerate().map(|(i, (card, part))| {
                        json::obj(vec![("card", Json::Int(i as i64)), ("label", text(card.label.clone())),
                            ("kept", Json::Bool(card.kept)), ("beats", num(part.beats)),
                            ("first_cycle", Json::Int(part.first as i64)), ("notes", text(part.text()))])
                    }).collect();
                    let live_part = if valid {
                        match programs.evaluate_controlled(&played, control) {
                            Ok(loaded) => makepad_score_live::vary::part_from(loaded, &node).ok(),
                            Err(makepad_score_live::LoadError::Problems(_)) => None,
                            Err(cancelled) => return Err(cancelled.to_string()),
                        }
                    } else { None };
                    Ok(PreparedTool::Reply(json::obj(vec![
                        ("revision", Json::Int(revision as i64)),
                        ("cards", Json::Arr(cards)),
                        ("busy", view.busy.map_or(Json::Null, text)),
                        ("region_beats", view.region.map_or(Json::Null, |(a, b)| Json::Arr(vec![num(a), num(b)]))),
                        ("playing", view.playing.map_or(Json::Null, |p| Json::Int(p as i64))),
                        ("in_place", Json::Bool(audition)),
                        ("played_source", if with_source { text(played.clone()) } else { Json::Null }),
                        ("live_notes", live_part.as_ref().map_or(Json::Null, |p| text(p.text()))),
                        ("live_beats", live_part.as_ref().map_or(Json::Null, |p| num(p.beats))),
                        ("live_is_card", match (&live_part, &playing_part) {
                            (Some(a), Some(b)) => Json::Bool(a.notes.len() == b.notes.len() && a.notes.iter().zip(&b.notes).all(|(x, y)| (x.beat - y.beat).abs() < 1e-3 && x.note == y.note && x.hit == y.hit)),
                            _ => Json::Null,
                        }),
                        ("ai_made", ai_made.map_or(Json::Null, |(n, s)| json::obj(vec![("variants", Json::Int(n as i64)), ("seconds", num(s))]))),
                    ])))
                })
            }
            "score_generate" => {
                let (Some(node), Some(gen)) = (args.get("node").and_then(Json::as_str), args.get("generator").and_then(Json::as_str)) else { return refused("`node` and `generator`") };
                let choices: Vec<(String, String)> = match args.get("choices") {
                    Some(Json::Obj(pairs)) => pairs.iter().map(|(k, v)| (k.clone(), match v {
                        Json::Str(s) => s.clone(),
                        Json::Int(n) => n.to_string(),
                        Json::F64(n) => n.to_string(),
                        other => other.to_json(),
                    })).collect(),
                    _ => Vec::new(),
                };
                let write = args.get("write").and_then(Json::as_bool) == Some(true);
                let id = self.jobs.panel_request();
                match self.generate(cx, node.trim(), gen.trim(), &choices, write, Default::default(), Some(id)) {
                    Ok(()) => Answer::Later(id),
                    Err(error) => { self.jobs.panel_answer(id, Err(error)); Answer::Later(id) }
                }
            }
            "score_batch" | "score_batch_status" | "score_rate" | "score_learn" | "score_batch_stop" => Answer::Done(self.batch_tool_call(cx, ui, name, args)),
            other => refused(format!("no score tool `{other}`")),
        }
    }
}
