//! New video: one ask ("a lyric video about night trains,
//! synthwave, 75 s") makes a complete new cut, step by step, each step's
//! output shown and replaceable (`makepad-story-production`):
//!
//! 1. **Lyrics**: the model writes the sheet, the music brief and the
//!    title (the person's own lyrics are kept as they are).
//! 2. **Music**: the `song` preset sings the sheet on the AI Hub's music
//!    model; the song lands in the library.
//! 3. **Timing**: the song's beats and sections and every sung word's time
//!    (the known sheet forced against the song, `audio_lyrics::sung`),
//!    written as the song timing module, a Motion module in the library.
//! 4. **Storyboard**: the model's style guide and one scene plan per
//!    section; a new cut opens with the board (bible, sections, lyric
//!    lines, a card per section).
//! 5. **Scenes**: one scene author per section writes its Motion document
//!    against the style guide and the timing module (the Generate panel's
//!    crew, so each author's work is visible there); the scenes go on the
//!    timeline and each gets its editor panel.
//! 6. **Edit**: transitions on the section cuts, the cut's panel, saved.
//!
//! The production is saved after every change (`production.json` in its
//! folder, with each step's output as a file beside it): it resumes at its
//! first unfinished step, and any step can be made again (it and the steps
//! after it), one scene alone too. The steps run through Stage's control
//! API, so every change is an ordinary undoable edit; the same flow is the
//! `new_video`, `new_video_status`, `new_video_redo` and `new_video_cancel` tools.
//!
//! There is no panel: the chat asks (`new_video` card), and each step's
//! start and finish is a line in the chat transcript.
//!
//! Main-loop cost: the pump drains one answer or one call per step; the
//! timing runs on its own worker thread.

use super::VideoCut;
use makepad_story_production::{self as production, AssetRef, Production, Scene, Status, Step};
use makepad_story_video_agent::{Ask, Backend, DEFAULT_CLAUDE_MODEL};
use makepad_strict_json::{self as json, Value as Json};
use makepad_widgets::*;
use std::path::PathBuf;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TryRecvError};

/// Who the control API's change log says made a change.
const ORIGIN: &str = "new_video";
/// How long a scene's panel change may take to reach the library before
/// the next scene's panel is made anyway (the desk saves after a pause).
const SETTLE_SECS: f64 = 30.0;
/// How long to wait before asking a music node that refused the song again.
const REFUSED_WAIT_SECS: f64 = 15.0;

/// What the running step waits for.
enum Run {
    Idle,
    /// The model's answer (lyrics, storyboard).
    Ask(Ask),
    /// One control-API call, and what it is for.
    Call(u64, Want),
    /// The timing worker's answer.
    Timing,
    /// The scene authors (the chat's crew).
    Authors,
    /// The song again at this time: the node was not admitting work (its
    /// own use of the GPU, or between jobs); how many times it said so.
    SongAgain(f64, u32),
    /// The graphic desk publishing a panel's change, before the next scene:
    /// the scene's index and since when.
    Settle(usize, f64),
}

/// What a call's answer goes to.
#[derive(Clone, Debug, PartialEq)]
enum Want {
    /// The song's `gen_run`, after this many refusals.
    Song(u32),
    SongJob,
    Module,
    NewEdit,
    Board,
    Place,
    Open(usize),
    Panel(usize),
    CutPanel,
    CutPanelStatus,
    Save,
}

pub struct NewVideo {
    production: Option<Production>,
    run: Run,
    /// When the running step started (`Cx::time_now`).
    started: f64,
    /// Stop after the running step (`new_video_cancel`).
    stopping: bool,
    backends: Vec<Backend>,
    backend: usize,
    generation: u64,
    timer: Timer,
    timing: Option<TimingWorker>,
    /// The per-line timing report, for the status tool and the pane.
    timing_lines: String,
    /// Scenes the Scenes step makes this time (all, or the one remade).
    authoring: Vec<String>,
    /// Restored from disk once.
    restored: bool,
}

impl Default for NewVideo {
    fn default() -> Self {
        NewVideo {
            production: None,
            run: Run::Idle,
            started: 0.0,
            stopping: false,
            backends: vec![
                Backend::ClaudeCode { model: format!("{DEFAULT_CLAUDE_MODEL}[1m]") },
                Backend::ClaudeApi { model: DEFAULT_CLAUDE_MODEL.into() },
                Backend::Hub { model: None },
            ],
            backend: 0,
            generation: 0,
            timer: Timer::default(),
            timing: None,
            timing_lines: String::new(),
            authoring: Vec::new(),
            restored: false,
        }
    }
}

impl NewVideo {
    pub fn busy(&self) -> bool {
        !matches!(self.run, Run::Idle)
    }

    /// Ask the running step to be the last: nothing starts after it until
    /// `new_video_redo` goes on. False when no step runs.
    pub fn stop(&mut self) -> bool {
        if !self.busy() {
            return false;
        }
        self.stopping = true;
        self.generation += 1;
        true
    }

    fn backend(&self) -> &Backend {
        &self.backends[self.backend.min(self.backends.len() - 1)]
    }
}

/// Where productions are kept: `$STAGE_PRODUCTIONS`, else
/// `<makepad home>/stage-product/productions` (a hidden instance: its own
/// temp folder).
fn root() -> PathBuf {
    makepad_app_shell::user_state::dir(Some("STAGE_PRODUCTIONS"), "productions")
}

fn folder(production: &Production) -> PathBuf {
    root().join(&production.id)
}

/// Write the record and each step's output beside it. Small files, once
/// per change of a step.
fn save(production: &Production, timing_lines: &str) {
    let dir = folder(production);
    if let Err(error) = std::fs::create_dir_all(&dir) {
        log!("new video: {}: {error}", dir.display());
        return;
    }
    let write = |name: &str, text: &str| {
        let tmp = dir.join(format!(".{name}.tmp"));
        if std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, dir.join(name))).is_err() {
            log!("new video: could not write {name}");
        }
    };
    write("production.json", &production.to_json());
    if !production.sheet.is_empty() {
        write("lyrics.txt", &format!("TITLE: {}\nMUSIC: {}\n\n{}\n", production.title, production.caption, production.sheet));
    }
    if !production.style.is_empty() {
        write("style.md", &production.style);
    }
    if !timing_lines.is_empty() {
        write("timing.txt", timing_lines);
    }
    let _ = std::fs::write(root().join("last"), &production.id);
}

/// The production last worked on.
fn restore() -> Option<Production> {
    let id = std::fs::read_to_string(root().join("last")).ok()?;
    let bytes = std::fs::read(root().join(id.trim()).join("production.json")).ok()?;
    Production::from_json(&bytes).map_err(|error| log!("new video: {error}")).ok()
}

fn new_id() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    format!("nv{secs}")
}

fn seed() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64 & 0x7fff_ffff)
}

// ------------------------------------------------------------------ timing

/// A song to time: its library revision and the sheet sung in it.
struct TimingJob {
    song: AssetRef,
    sheet: String,
    title: String,
    /// The music brief (its tempo decides the beat's octave).
    caption: String,
}

/// The tempo a music brief asks for (`… 96 BPM …`).
fn brief_bpm(caption: &str) -> Option<f64> {
    let words: Vec<&str> = caption.split(|c: char| c.is_whitespace() || c == ',').filter(|w| !w.is_empty()).collect();
    words.windows(2).find(|pair| pair[1].eq_ignore_ascii_case("bpm")).and_then(|pair| pair[0].parse::<f64>().ok()).filter(|bpm| (40.0..=240.0).contains(bpm))
}

/// The timing's results: the module's source and what the steps after it
/// need.
struct TimingDone {
    module: String,
    duration: f64,
    sections: Vec<production::Section>,
    report: String,
    lines: String,
}

struct TimingWorker {
    jobs: SyncSender<TimingJob>,
    done: Receiver<Result<TimingDone, String>>,
}

impl TimingWorker {
    fn new(cx: &mut Cx) -> Result<TimingWorker, String> {
        let (jobs, job_rx) = sync_channel::<TimingJob>(1);
        let (done_tx, done) = sync_channel::<Result<TimingDone, String>>(1);
        let options = ThreadOptions { name: Some(std::sync::Arc::from("new-video-timing")), stack_size: Some(8 * 1024 * 1024), priority: CxThreadPriority::UserInitiated };
        let handle = cx
            .thread_spawner()
            .spawn_worker(options, move || {
                while let Ok(job) = job_rx.recv() {
                    if done_tx.send(time_song(&job)).is_err() {
                        break;
                    }
                    SignalToUI::set_ui_signal();
                }
            })
            .map_err(|error| format!("the timing worker: {error:?}"))?;
        handle.detach();
        Ok(TimingWorker { jobs, done })
    }
}

/// The song's samples, mono, and their rate.
fn song_pcm(song: &AssetRef) -> Result<(Vec<f32>, f64), String> {
    use makepad_story_edit_render::MediaResolver;
    let client = makepad_asset_connection::connect(&makepad_asset_connection::origin())?;
    let mut store = makepad_story_edit_render::StoreMedia::new(client.lane_clone());
    let reference = makepad_story_cut::MediaRef::Asset(makepad_story_cut::AssetRef { id: song.asset_id.clone(), rev: song.revision.clone() });
    let file = store.resolve(&reference).map_err(|error| format!("the song does not resolve: {error}"))?;
    let bytes = std::fs::read(&file.path).map_err(|error| format!("{}: {error}", file.path.display()))?;
    if bytes.starts_with(b"RIFF") {
        let pcm = makepad_stage_audio_media::decode::parse_wav(&bytes, usize::MAX).map_err(|error| format!("the song does not decode: {error}"))?;
        let mono = pcm.frames.iter().map(|[l, r]| (*l as f32 + *r as f32) * (0.5 / 32768.0)).collect();
        return Ok((mono, pcm.sample_rate as f64));
    }
    let audio = makepad_audio_decode::decode_any(&bytes).map_err(|error| format!("the song does not decode: {error:?}"))?;
    let left = audio.channel(0);
    let mono = if audio.channels > 1 { left.iter().zip(&audio.channel(1)).map(|(l, r)| (l + r) * 0.5).collect() } else { left };
    Ok((mono, audio.rate as f64))
}

/// The whisper checkpoint the song timing forces the sheet with:
/// `MAKEPAD_VOICE_MODEL`, else the checkout's `local/` tree and the AI hub's
/// weights folder (`<makepad home>/weights/stt`), each probed file of the
/// pinned size.
fn whisper_path() -> Option<PathBuf> {
    const FILE: &str = "ggml-large-v3-turbo.bin";
    const BYTES: u64 = 1_624_555_275;
    if let Some(path) = std::env::var("MAKEPAD_VOICE_MODEL").ok().map(|path| PathBuf::from(path.trim())).filter(|path| path.is_file()) {
        return Some(path);
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    [root.join("local").join(FILE), root.join("local/models").join(FILE), makepad_ai_hub::home::makepad_home().join("weights/stt").join(FILE)]
        .into_iter()
        .find(|path| std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() == BYTES))
}

/// Beats and sections from the music, every sung word from the sheet
/// forced against the song, as the song timing module. On the worker.
fn time_song(job: &TimingJob) -> Result<TimingDone, String> {
    use makepad_audio_lyrics::sung::{How, Sheet};
    use production::timing::{self, Line, SongTiming, Word};
    let started = std::time::Instant::now();
    let (mono, rate) = song_pcm(&job.song)?;
    let duration = mono.len() as f64 / rate.max(1.0);
    log!("new video: timing: {duration:.1} s of song read in {:.1} s", started.elapsed().as_secs_f64());
    let features = makepad_story_audio_features::analyze_music_with(&mono, rate, brief_bpm(&job.caption));
    log!("new video: timing: beats and sections in {:.1} s", started.elapsed().as_secs_f64());
    let model = whisper_path().ok_or_else(|| format!("the word timing needs whisper ({}) in the checkout's local/models", "ggml-large-v3-turbo.bin"))?;
    let mut baker = makepad_audio_lyrics::bake::LyricsBaker::open(&model)?;
    log!("new video: timing: whisper loaded in {:.1} s", started.elapsed().as_secs_f64());
    let sheet = Sheet::parse(&job.sheet);
    let sung = baker.align_sheet(&mono, rate, duration, &sheet, "en");
    log!("new video: timing: the sheet timed in {:.1} s", started.elapsed().as_secs_f64());
    let lines: Vec<Line> = sung
        .lines
        .iter()
        .map(|line| Line { block: line.block, words: line.words.iter().map(|word| Word { w: word.w.clone(), start: word.start, end: word.end, syl: word.syl.clone() }).collect() })
        .collect();
    let tags: Vec<String> = sheet.blocks.iter().map(|block| block.tag.clone()).collect();
    let sections = timing::sections(&tags, &lines, &features.downbeats, duration);
    // Hits: the low bands' rises (kick), the top band's (hat), the vocal's attacks.
    let hop = 1.0 / makepad_story_audio_features::RATE;
    let low: Vec<f32> = features.bands.iter().map(|b| 0.6 * b[0] + 0.4 * b[1]).collect();
    let high: Vec<f32> = features.bands.iter().map(|b| b[5]).collect();
    let vocal: Vec<(f64, f32)> = sung.vocal_onsets.iter().copied().filter(|(_, strength)| *strength >= 0.25).collect();
    let song = SongTiming {
        duration,
        beats: features.beats.clone(),
        downbeats: features.downbeats.clone(),
        sections: sections.clone(),
        onsets: vec![("kick".into(), timing::rises(&low, hop, 0.18)), ("hat".into(), timing::rises(&high, hop, 0.09)), ("vocal".into(), vocal)],
        lines,
    };
    let module = song.module_source(&format!("\"{}\": its analysis and its lyric sheet, timed by Stage", job.title.replace('"', "'")));
    // The honest numbers: per line, how it was timed and how far its
    // words sit from the heard ones.
    let forced = sung.lines.iter().filter(|line| line.forced).count();
    let mut agreements: Vec<f64> = sung.lines.iter().filter_map(|line| line.agreement).collect();
    agreements.sort_by(f64::total_cmp);
    let median = agreements.get(agreements.len() / 2).copied();
    let spread = sung.lines.iter().flat_map(|line| &line.words).filter(|word| word.how == How::Spread).count();
    let report = format!(
        "{} lines, {} words · {} forced from the sheet, {} from the heard words · median agreement {} · {} of {} sheet words heard · {} unsung line(s) · {} spread word(s) · {:.0} BPM",
        sung.lines.len(),
        sung.lines.iter().map(|line| line.words.len()).sum::<usize>(),
        forced,
        sung.lines.len() - forced,
        median.map_or("-".to_string(), |m| format!("{:.0} ms", m * 1000.0)),
        sung.matched_words,
        sung.sheet_words,
        sung.unsung.len(),
        spread,
        production::edit::bpm(&features.beats),
    );
    let mut lines_text = format!("{report}\nheard: {}\n\n", sung.heard_text);
    for line in &sung.lines {
        lines_text.push_str(&format!(
            "{:7.2}-{:7.2} {} match {:2}/{:2} agree {}  {}\n",
            line.start(),
            line.end(),
            if line.forced { "forced" } else { "heard " },
            line.matched,
            line.words.len(),
            line.agreement.map_or("   -  ".to_string(), |a| format!("{:4.0}ms", a * 1000.0)),
            line.text()
        ));
    }
    for (_, text) in &sung.unsung {
        lines_text.push_str(&format!("  unsung: {text}\n"));
    }
    log!("new video: timing: {report}");
    Ok(TimingDone { module, duration, sections, report, lines: lines_text })
}

pub(super) fn handle_event(page: &mut VideoCut, _cx: &mut Cx, event: &Event) {
    if page.new_video.timer.is_event(event).is_some() {
        page.new_video.timer = Timer::default();
    }
}

// ------------------------------------------------------------- the flow

/// A new production of `ask`; it starts on the next pump.
pub(super) fn start(page: &mut VideoCut, ask: &str, seconds: f64, lyrics: Option<String>, model: Option<&str>) -> Result<(), String> {
    if page.new_video.busy() {
        return Err("a video is being made: wait for it, or stop it".into());
    }
    if ask.trim().is_empty() {
        return Err("Say what to make first".into());
    }
    if let Some(model) = model.filter(|model| !model.is_empty()) {
        let backend = super::generate::backend_named(model);
        page.new_video.backends.insert(0, backend);
        page.new_video.backend = 0;
    }
    if let Some(reason) = page.new_video.backend().unavailable() {
        return Err(format!("{}: {reason}", page.new_video.backend().label()));
    }
    let production = Production::new(new_id(), ask.trim(), seconds, lyrics.filter(|l| !l.trim().is_empty()), seed());
    save(&production, "");
    log!("new video: {} “{}”, {:.0} s", production.id, production.ask, production.seconds);
    page.new_video.production = Some(production);
    page.new_video.timing_lines.clear();
    page.new_video.stopping = false;
    page.new_video.generation += 1;
    Ok(())
}

/// Go on from the first unfinished step (a failed one runs again).
pub(super) fn resume(page: &mut VideoCut) {
    if let Some(production) = page.new_video.production.as_mut() {
        if let Some(step) = production.next() {
            if matches!(production.record(step).status, Status::Failed(_)) {
                production.record_mut(step).status = Status::Waiting;
            }
        }
    }
    page.new_video.stopping = false;
    page.new_video.generation += 1;
}

/// Make `step` again (and the steps after it); with `section` on the
/// Scenes step, only that section's scene.
pub(super) fn redo(page: &mut VideoCut, step: Step, section: Option<&str>) -> Result<(), String> {
    if page.new_video.busy() {
        return Err("a step is running: stop it first".into());
    }
    let production = page.new_video.production.as_mut().ok_or("no video is being made")?;
    match section {
        Some(section) if step == Step::Scenes => {
            if !production.redo_scene(section) {
                return Err(format!("no scene for section `{section}`"));
            }
        }
        _ => production.redo(step),
    }
    if step.index() <= Step::Timing.index() {
        page.new_video.timing_lines.clear();
    }
    let production = page.new_video.production.as_ref().expect("production");
    save(production, &page.new_video.timing_lines);
    page.new_video.stopping = false;
    page.new_video.generation += 1;
    Ok(())
}

/// The production's state for the status tool.
pub(super) fn status(page: &VideoCut) -> Json {
    match &page.new_video.production {
        Some(production) => {
            let mut value = production.status_value();
            if let Json::Obj(pairs) = &mut value {
                pairs.push(("running".into(), Json::Bool(page.new_video.busy())));
                pairs.push(("folder".into(), json::s(folder(production).display().to_string())));
                if !page.new_video.timing_lines.is_empty() {
                    pairs.push(("timing_lines".into(), json::s(page.new_video.timing_lines.clone())));
                }
            }
            value
        }
        None => json::obj(vec![("production", Json::Null)]),
    }
}

fn text_of<'a>(value: &'a Json, key: &str) -> &'a str {
    value.get(key).and_then(Json::as_str).unwrap_or("")
}

fn asset_of(value: Option<&Json>) -> AssetRef {
    let Some(value) = value else { return AssetRef::default() };
    AssetRef::new(text_of(value, "asset_id"), text_of(value, "revision"))
}

/// Each `let <node> = Graphic{src: asset("<id>", "<rev>") …` of a cut's
/// source: the revision each scene's clip plays now.
fn graphic_srcs(source: &str) -> Vec<(String, AssetRef)> {
    let mut out = Vec::new();
    for line in source.lines() {
        let Some(rest) = line.trim().strip_prefix("let ") else { continue };
        let Some((node, rest)) = rest.split_once('=') else { continue };
        let Some(rest) = rest.trim().strip_prefix("Graphic{src: asset(\"") else { continue };
        let mut parts = rest.splitn(3, '"');
        let (Some(id), Some(_), Some(tail)) = (parts.next(), parts.next(), parts.next()) else { continue };
        let Some(rev) = tail.split('"').next() else { continue };
        out.push((node.trim().to_string(), AssetRef::new(id, rev)));
    }
    out
}

impl crate::StoryApp {
    /// The New video flow's turn in the page pump: one answer or one call
    /// per step at a time, through the control API.
    pub(crate) fn pump_new_video(&mut self, cx: &mut Cx) {
        if !self.video_cut.new_video.restored {
            self.video_cut.new_video.restored = true;
            if self.video_cut.new_video.production.is_none() {
                self.video_cut.new_video.production = restore();
            }
        }
        if self.video_cut.new_video.production.is_none() {
            return;
        }
        let before = self.video_cut.new_video.generation;
        self.advance(cx);
        let now = Cx::time_now();
        let nv = &mut self.video_cut.new_video;
        let busy = nv.busy();
        if let (Some(production), true) = (nv.production.as_mut(), busy) {
            if let Some(step) = production.next() {
                let secs = now - nv.started;
                let record = production.record_mut(step);
                if (secs - record.secs).abs() >= 1.0 {
                    record.secs = secs.floor();
                    nv.generation += 1;
                }
            }
        }
        if nv.busy() {
            nv.timer = cx.start_timeout(0.25);
        }
        if nv.generation != before {
            self.video_cut.repaint = true;
        }
    }

    fn nv_fail(&mut self, why: String) {
        let nv = &mut self.video_cut.new_video;
        nv.run = Run::Idle;
        let mut line = None;
        if let Some(production) = nv.production.as_mut() {
            if let Some(step) = production.next() {
                log!("new video: {} failed: {why}", step.id());
                line = Some(format!("New video, {}: failed: {why}", step.label()));
                production.record_mut(step).status = Status::Failed(why);
                save(production, &nv.timing_lines);
            }
        }
        nv.generation += 1;
        if let Some(line) = line {
            super::chat::note(&mut self.video_cut, line);
        }
    }

    /// The running step is done: its time, its note, saved.
    fn nv_done(&mut self, note: String) {
        let now = Cx::time_now();
        let nv = &mut self.video_cut.new_video;
        nv.run = Run::Idle;
        let mut line = None;
        if let Some(production) = nv.production.as_mut() {
            if let Some(step) = production.next() {
                let record = production.record_mut(step);
                record.status = Status::Done;
                record.secs = ((now - nv.started) * 10.0).round() / 10.0;
                record.note = note;
                log!("new video: {} done in {:.1} s: {}", step.id(), record.secs, record.note);
                line = Some(format!("New video, {}: done in {:.0} s. {}", step.label(), record.secs, record.note));
                save(production, &nv.timing_lines);
            }
        }
        nv.generation += 1;
        if let Some(line) = line {
            super::chat::note(&mut self.video_cut, line);
        }
    }

    fn nv_note(&mut self, note: String) {
        let nv = &mut self.video_cut.new_video;
        if let Some(production) = nv.production.as_mut() {
            if let Some(step) = production.next() {
                production.record_mut(step).note = note;
            }
        }
        nv.generation += 1;
    }

    fn nv_call(&mut self, cx: &mut Cx, tool: &str, args: Json, want: Want) {
        let id = self.studio_call(cx, tool, args, ORIGIN);
        self.video_cut.new_video.run = Run::Call(id, want);
    }

    /// One turn of the running step, or the start of the next one.
    fn advance(&mut self, cx: &mut Cx) {
        let Some(step) = self.video_cut.new_video.production.as_ref().and_then(Production::next) else {
            self.video_cut.new_video.run = Run::Idle;
            return;
        };
        let status = self.video_cut.new_video.production.as_ref().map(|p| p.record(step).status.clone()).unwrap_or(Status::Waiting);
        match std::mem::replace(&mut self.video_cut.new_video.run, Run::Idle) {
            Run::Idle => {
                if status != Status::Waiting || self.video_cut.new_video.stopping {
                    return;
                }
                self.video_cut.new_video.started = Cx::time_now();
                if let Some(production) = self.video_cut.new_video.production.as_mut() {
                    production.record_mut(step).status = Status::Running;
                }
                self.video_cut.new_video.generation += 1;
                super::chat::note(&mut self.video_cut, format!("New video, {}: started", step.label()));
                if let Err(why) = self.begin_step(cx, step) {
                    self.nv_fail(why);
                }
            }
            Run::Ask(mut ask) => match ask.poll() {
                None => self.video_cut.new_video.run = Run::Ask(ask),
                Some(Err(why)) => self.nv_fail(format!("the model: {why}")),
                Some(Ok(answer)) => {
                    if let Err(why) = self.answered(cx, step, &answer) {
                        self.nv_fail(why);
                    }
                }
            },
            Run::Call(id, want) => match self.studio_result(id) {
                None => self.video_cut.new_video.run = Run::Call(id, want),
                Some(result) => {
                    let result = result.map(|output| output.json).map_err(|error| error.message);
                    if let Err(why) = self.called(cx, want, result) {
                        self.nv_fail(why);
                    }
                }
            },
            Run::Timing => {
                let received = self.video_cut.new_video.timing.as_ref().map(|worker| worker.done.try_recv());
                match received {
                    Some(Err(TryRecvError::Empty)) => self.video_cut.new_video.run = Run::Timing,
                    Some(Ok(Ok(done))) => self.timed(cx, done),
                    Some(Ok(Err(why))) => self.nv_fail(why),
                    Some(Err(TryRecvError::Disconnected)) | None => {
                        self.video_cut.new_video.timing = None;
                        self.nv_fail("the timing worker stopped".into());
                    }
                }
            }
            Run::Authors => {
                if self.video_cut.generate.busy() {
                    self.video_cut.new_video.run = Run::Authors;
                    let working = self.video_cut.generate.crew.as_ref().map_or(0, |crew| crew.agents().iter().filter(|(_, agent)| agent.state().busy()).count());
                    let note = format!("{working} of {} scene authors working", self.video_cut.new_video.authoring.len());
                    if self.video_cut.new_video.production.as_ref().is_some_and(|p| p.record(Step::Scenes).note != note) {
                        self.nv_note(note);
                    }
                } else if let Err(why) = self.authored(cx) {
                    self.nv_fail(why);
                }
            }
            Run::SongAgain(at, tries) => {
                if self.video_cut.new_video.stopping {
                    self.nv_fail("stopped while waiting for the music node".into());
                } else if Cx::time_now() >= at {
                    self.sing(cx, tries);
                } else {
                    self.video_cut.new_video.run = Run::SongAgain(at, tries);
                }
            }
            Run::Settle(index, since) => {
                let settled = super::motion_edit::desk::settled(&self.video_cut) && self.video_cut.queue.is_empty() && self.video_cut.in_flight.is_none();
                if settled || Cx::time_now() - since > SETTLE_SECS {
                    if !settled {
                        log!("new video: scene {index}'s panel change was not published within {SETTLE_SECS} s; going on");
                    }
                    self.next_panel(cx, index + 1);
                } else {
                    self.video_cut.new_video.run = Run::Settle(index, since);
                }
            }
        }
    }

    fn begin_step(&mut self, cx: &mut Cx, step: Step) -> Result<(), String> {
        let backend = self.video_cut.new_video.backend().clone();
        let production = self.video_cut.new_video.production.clone().ok_or("no production")?;
        match step {
            Step::Lyrics => {
                let given = production.given_lyrics.then_some(production.sheet.as_str());
                let ask = Ask::start(&backend, production::lyrics::SYSTEM, production::lyrics::request(&production.ask, production.seconds, given))?;
                self.video_cut.new_video.run = Run::Ask(ask);
                self.nv_note(format!("{} writes the song", backend.label()));
            }
            Step::Music => self.sing(cx, 0),
            Step::Timing => self.start_timing(cx)?,
            Step::Storyboard => {
                let (bpm, _) = self.production_bpm();
                let ask = Ask::start(&backend, production::story::SYSTEM, production::story::request(&production.ask, &production.title, &production.caption, &production.sheet, &production.sections, bpm))?;
                self.video_cut.new_video.run = Run::Ask(ask);
                self.nv_note(format!("{} plans a scene per section", backend.label()));
            }
            Step::Scenes => {
                // The cut the scenes are written for: the production's own.
                if !self.cut_is_ours() {
                    return self.open_production_edit(cx);
                }
                self.start_authors(cx)?;
            }
            Step::Cut => {
                if !self.cut_is_ours() {
                    return self.open_production_edit(cx);
                }
                self.assemble(cx)?;
            }
        }
        Ok(())
    }

    /// The song to the timing worker.
    fn start_timing(&mut self, cx: &mut Cx) -> Result<(), String> {
        let production = self.video_cut.new_video.production.as_ref().ok_or("no production")?;
        let job = TimingJob { song: production.song.clone(), sheet: production.sheet.clone(), title: production.title.clone(), caption: production.caption.clone() };
        if self.video_cut.new_video.timing.is_none() {
            self.video_cut.new_video.timing = Some(TimingWorker::new(cx)?);
        }
        let worker = self.video_cut.new_video.timing.as_ref().expect("timing worker");
        worker.jobs.try_send(job).map_err(|_| "the timing worker is busy".to_string())?;
        self.video_cut.new_video.run = Run::Timing;
        self.nv_note("beats, sections and the sung words".into());
        Ok(())
    }

    /// The song preset on the AI Hub. `tries`: refusals so far.
    fn sing(&mut self, cx: &mut Cx, tries: u32) {
        let Some(production) = self.video_cut.new_video.production.as_ref() else { return };
        let args = json::obj(vec![
            ("preset", json::s("song")),
            ("prompt", json::s(production.caption.clone())),
            ("params", json::obj(vec![("lyrics", json::s(production.sheet.clone())), ("seconds", Json::F64(production.seconds.round()))])),
            ("seed", Json::Int((production.seed & 0x7fff_ffff) as i64)),
            ("wait", Json::Bool(true)),
        ]);
        self.nv_call(cx, "gen_run", args, Want::Song(tries));
        if tries == 0 {
            self.nv_note("the song preset on the AI Hub".into());
        }
    }

    /// Whether the open cut is this production's (its label is the title).
    fn cut_is_ours(&self) -> bool {
        let title = self.video_cut.new_video.production.as_ref().map_or("", |p| p.title.as_str());
        self.video_cut.session.as_ref().is_some_and(|session| session.doc.label == title)
    }

    /// A new cut in the production's format, named after the song.
    fn open_production_edit(&mut self, cx: &mut Cx) -> Result<(), String> {
        let production = self.video_cut.new_video.production.as_ref().ok_or("no production")?;
        let beats_bpm = 120.0;
        let args = json::obj(vec![
            ("width", Json::Int(production.width as i64)),
            ("height", Json::Int(production.height as i64)),
            ("fps", Json::Int(production.fps as i64)),
            ("label", json::s(production.title.clone())),
            ("duration", Json::F64(production.song_seconds.max(1.0))),
            ("bpm", Json::F64(beats_bpm)),
        ]);
        self.nv_call(cx, "new_cut", args, Want::NewEdit);
        Ok(())
    }

    /// The model answered the running step.
    fn answered(&mut self, cx: &mut Cx, step: Step, answer: &str) -> Result<(), String> {
        match step {
            Step::Lyrics => {
                let written = production::lyrics::parse(answer)?;
                let nv = &mut self.video_cut.new_video;
                let production = nv.production.as_mut().ok_or("no production")?;
                production.title = written.title;
                production.caption = written.caption;
                if !production.given_lyrics {
                    production.sheet = written.sheet;
                }
                let lines = production.sheet.lines().filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('[')).count();
                let note = format!("“{}” · {lines} lines", production.title);
                self.nv_done(note);
            }
            Step::Storyboard => {
                let production = self.video_cut.new_video.production.as_mut().ok_or("no production")?;
                let (style, plans) = production::story::parse(answer, &production.sections)?;
                production.style = style;
                production.plans = plans;
                save(production, &self.video_cut.new_video.timing_lines);
                self.open_production_edit(cx)?;
            }
            _ => return Err(format!("{} takes no model answer", step.id())),
        }
        Ok(())
    }

    /// The timing worker's result: the module goes into the library.
    fn timed(&mut self, cx: &mut Cx, done: TimingDone) {
        let nv = &mut self.video_cut.new_video;
        nv.timing_lines = done.lines;
        let Some(production) = nv.production.as_mut() else { return };
        production.song_seconds = done.duration;
        production.sections = done.sections;
        production.timing_report = done.report.clone();
        let name = production::story::motion_name(&production.title, "song_timing");
        let title = format!("{} · song timing", production.title);
        let dir = folder(production);
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("song_timing.splash"), &done.module);
        save(production, &nv.timing_lines);
        nv.generation += 1;
        self.nv_call(cx, "write_motion", json::obj(vec![("name", json::s(name)), ("title", json::s(title)), ("source", json::s(done.module))]), Want::Module);
    }

    /// A control-API call answered.
    fn called(&mut self, cx: &mut Cx, want: Want, result: Result<Json, String>) -> Result<(), String> {
        match want {
            Want::Song(_) | Want::SongJob => {
                let tries = if let Want::Song(tries) = want { tries } else { 0 };
                // A node that is not admitting work now (its owner's use of
                // the GPU, the pause between jobs) is asked again shortly,
                // as long as it takes; Stop ends the wait.
                let refused = |why: &str| why.contains("admission") || why.contains("local-use") || why.contains("busy");
                let out = match result {
                    Ok(out) if text_of(&out, "phase") == "failed" && refused(text_of(&out, "error")) => Err(text_of(&out, "error").to_string()),
                    other => other,
                };
                let out = match out {
                    Err(why) if refused(&why) => {
                        log!("new video: the music node is not admitting work ({why}); again in {REFUSED_WAIT_SECS} s");
                        self.nv_note(format!("waiting for the music node to admit the song ({} refusals)", tries + 1));
                        self.video_cut.new_video.run = Run::SongAgain(Cx::time_now() + REFUSED_WAIT_SECS, tries + 1);
                        return Ok(());
                    }
                    other => other.map_err(|why| format!("the song: {why}"))?,
                };
                match text_of(&out, "phase") {
                    "done" => {
                        let asset = out.get("assets").and_then(Json::as_arr).and_then(|assets| assets.iter().find(|a| text_of(a, "kind") == "audio" || text_of(a, "kind").is_empty()).or_else(|| assets.first())).map(|a| asset_of(Some(a))).unwrap_or_default();
                        if !asset.is_set() {
                            return Err(format!("the song job published nothing: {}", out.to_json()));
                        }
                        let seconds = match out.get("seconds") {
                            Some(Json::F64(s)) => *s,
                            Some(Json::Int(s)) => *s as f64,
                            _ => 0.0,
                        };
                        let production = self.video_cut.new_video.production.as_mut().ok_or("no production")?;
                        production.song = asset;
                        let note = format!("sung in {seconds:.0} s on the AI Hub{}", if out.get("reused").and_then(Json::as_bool) == Some(true) { " (the stored take)" } else { "" });
                        self.nv_done(note);
                    }
                    "running" | "choosing" | "" => {
                        let id = text_of(&out, "id").to_string();
                        let progress = match out.get("progress") {
                            Some(Json::F64(p)) => *p,
                            _ => 0.0,
                        };
                        self.nv_note(format!("{} {:.0}%", if text_of(&out, "stage").is_empty() { "singing" } else { text_of(&out, "stage") }, progress * 100.0));
                        self.nv_call(cx, "gen_job", json::obj(vec![("id", json::s(id)), ("wait", Json::Bool(true))]), Want::SongJob);
                    }
                    phase => return Err(format!("the song job {phase}: {}", text_of(&out, "error"))),
                }
            }
            Want::Module => {
                let out = result.map_err(|why| format!("the timing module: {why}"))?;
                let asset = asset_of(out.get("asset"));
                if !asset.is_set() {
                    return Err(format!("the timing module was not published: {}", out.to_json()));
                }
                let production = self.video_cut.new_video.production.as_mut().ok_or("no production")?;
                production.timing = asset;
                let note = production.timing_report.split(" · ").take(3).collect::<Vec<_>>().join(" · ");
                self.nv_done(note);
            }
            Want::NewEdit => {
                result.map_err(|why| format!("the new cut: {why}"))?;
                // The board goes with the cut (a resumed production's edit
                // is opened again, and its board with it).
                let production = self.video_cut.new_video.production.as_ref().ok_or("no production")?;
                let beats = self.production_bpm();
                let source = production::story::board_source(production, beats.0, beats.1);
                self.nv_call(cx, "storyboard_write", json::obj(vec![("source", json::s(source))]), Want::Board);
            }
            Want::Board => {
                result.map_err(|why| format!("the storyboard: {why}"))?;
                match self.video_cut.new_video.production.as_ref().and_then(Production::next) {
                    Some(Step::Storyboard) => {
                        let n = self.video_cut.new_video.production.as_ref().map_or(0, |p| p.plans.len());
                        self.nv_done(format!("{n} scenes planned · the cut is open with its board"));
                    }
                    Some(Step::Scenes) => self.start_authors(cx)?,
                    Some(Step::Cut) => self.assemble(cx)?,
                    _ => {}
                }
            }
            Want::Place => {
                result.map_err(|why| format!("placing the scenes: {why}"))?;
                match self.video_cut.new_video.production.as_ref().and_then(Production::next) {
                    Some(Step::Scenes) => self.next_panel(cx, 0),
                    _ => self.finish_edit(cx),
                }
            }
            Want::Open(index) => match result {
                Ok(_) => self.nv_call(cx, "editor_make", Json::Obj(Vec::new()), Want::Panel(index)),
                Err(why) => {
                    log!("new video: scene {index} did not open on the desk: {why}");
                    self.next_panel(cx, index + 1);
                }
            },
            Want::Panel(index) => {
                let made = match &result {
                    Ok(out) => out.get("applied").is_some_and(|a| !a.is_null()),
                    Err(_) => false,
                };
                let nv = &mut self.video_cut.new_video;
                if let Some(scene) = nv.production.as_mut().and_then(|p| p.scenes.get_mut(index)) {
                    scene.panel = made;
                    if !made {
                        scene.state = format!("no panel: {}", match &result {
                            Ok(out) => text_of(out, "failed").to_string(),
                            Err(why) => why.clone(),
                        });
                    }
                }
                nv.generation += 1;
                // The desk publishes the panel's change and repoints the clip first.
                self.video_cut.new_video.run = Run::Settle(index, Cx::time_now());
            }
            Want::CutPanel => match result {
                Ok(_) => self.nv_call(cx, "cut_panel_status", Json::Obj(Vec::new()), Want::CutPanelStatus),
                Err(why) => {
                    log!("new video: the cut's panel was not made: {why}");
                    self.nv_call(cx, "save_cut", Json::Obj(Vec::new()), Want::Save);
                }
            },
            Want::CutPanelStatus => {
                let made = result.as_ref().ok().is_some_and(|out| out.get("panel").is_some_and(|p| !p.is_null()) || text_of(out, "making").starts_with("Made"));
                let failed = result.is_err() || result.as_ref().ok().is_some_and(|out| text_of(out, "making").starts_with("Failed"));
                if made || failed {
                    self.nv_call(cx, "save_cut", Json::Obj(Vec::new()), Want::Save);
                } else {
                    self.nv_call(cx, "cut_panel_status", Json::Obj(Vec::new()), Want::CutPanelStatus);
                }
            }
            Want::Save => {
                let out = result.map_err(|why| format!("saving the cut: {why}"))?;
                let asset = out.get("edit").or_else(|| out.get("asset")).map(|a| asset_of(Some(a))).filter(AssetRef::is_set).unwrap_or_else(|| {
                    self.video_cut.session.as_ref().and_then(|s| s.head.as_ref()).map_or(AssetRef::default(), |head| AssetRef::new(head.asset_id.to_string(), head.revision.to_string()))
                });
                let production = self.video_cut.new_video.production.as_mut().ok_or("no production")?;
                production.edit = asset;
                let panels = production.scenes.iter().filter(|scene| scene.panel).count();
                let note = format!("{} scenes ({panels} with panels) over the song, saved", production.scenes.len());
                self.nv_done(note);
            }
        }
        Ok(())
    }

    /// The song's tempo and first bar line, from the timing module's
    /// sections (the module itself holds the beats).
    fn production_bpm(&self) -> (f64, f64) {
        let Some(production) = self.video_cut.new_video.production.as_ref() else { return (120.0, 0.0) };
        let bpm = production.timing_report.split(" · ").find_map(|part| part.strip_suffix(" BPM").and_then(|n| n.trim().parse::<f64>().ok())).unwrap_or(120.0);
        let first = production.sections.iter().find(|section| !section.lines.is_empty()).map_or(0.0, |section| section.start);
        (bpm, first)
    }

    /// One author per scene still to write (all of them, or a remade one).
    fn start_authors(&mut self, cx: &mut Cx) -> Result<(), String> {
        if self.video_cut.generate.busy() {
            return Err("the chat's model is working: stop it, then Resume".into());
        }
        let backend = self.video_cut.new_video.backend().clone();
        let tail = production::edit::transition_secs(self.production_bpm().0);
        let production = self.video_cut.new_video.production.as_mut().ok_or("no production")?;
        if production.scenes.is_empty() {
            production.scenes = production
                .sections
                .iter()
                .map(|section| Scene { section: section.name.clone(), motion: production::story::motion_name(&production.title, &section.name), ..Scene::default() })
                .collect();
        }
        let mut tasks = Vec::new();
        for scene in production.scenes.iter().filter(|scene| !scene.asset.is_set()) {
            let Some(section) = production.sections.iter().find(|section| section.name == scene.section) else { continue };
            let plan = production.plans.iter().find(|plan| plan.section == scene.section).cloned().unwrap_or_default();
            let brief = production::story::scene_brief(section, &plan, &production.timing, &production.title, production.width, production.height, tail);
            tasks.push((scene.section.clone(), scene.motion.clone(), brief));
        }
        let style = production.style.clone();
        let length = production.song_seconds;
        self.video_cut.new_video.authoring = tasks.iter().map(|task| task.0.clone()).collect();
        if tasks.is_empty() {
            return self.place_scenes(cx, true);
        }
        let tools = self.new_video_tools();
        super::generate::start_authors(&mut self.video_cut, &backend, &tools, &style, &tasks, length)?;
        self.video_cut.new_video.run = Run::Authors;
        self.nv_note(format!("{} scene authors writing", tasks.len()));
        Ok(())
    }

    /// The authors finished: each scene's latest revision.
    fn authored(&mut self, cx: &mut Cx) -> Result<(), String> {
        let states: Vec<(String, String, String)> = self.video_cut.generate.crew.as_ref().map_or(Vec::new(), |crew| {
            crew.agents()
                .iter()
                .map(|(id, agent)| {
                    let state = match agent.state() {
                        makepad_story_video_agent::State::Done => agent.summary().unwrap_or("done").to_string(),
                        makepad_story_video_agent::State::Stopped(why) => format!("stopped: {why}"),
                        makepad_story_video_agent::State::Failed(why) => format!("failed: {why}"),
                        _ => "working".into(),
                    };
                    (id.to_string(), state, String::new())
                })
                .collect()
        });
        let mut missing = Vec::new();
        let names: Vec<(usize, String)> = self.video_cut.new_video.production.as_ref().map_or(Vec::new(), |p| p.scenes.iter().enumerate().map(|(i, s)| (i, s.motion.clone())).collect());
        for (index, motion) in names {
            let written = self.studio_motion(&motion);
            let production = self.video_cut.new_video.production.as_mut().ok_or("no production")?;
            let scene = &mut production.scenes[index];
            if let Some((id, rev)) = written {
                scene.asset = AssetRef::new(id, rev);
            }
            if let Some((_, state, _)) = states.iter().find(|(id, _, _)| *id == scene.section) {
                scene.state = state.clone();
            }
            if !scene.asset.is_set() {
                missing.push(scene.section.clone());
            }
        }
        if !missing.is_empty() {
            return Err(format!("no scene was written for {}", missing.join(", ")));
        }
        self.place_scenes(cx, true)
    }

    /// The cut's source from the production: the scenes over the song.
    fn place_scenes(&mut self, cx: &mut Cx, _for_panels: bool) -> Result<(), String> {
        let production = self.video_cut.new_video.production.as_ref().ok_or("no production")?;
        let (bpm, _) = self.production_bpm();
        let placed: Vec<production::edit::Placed> = production
            .sections
            .iter()
            .filter_map(|section| production.scenes.iter().find(|scene| scene.section == section.name && scene.asset.is_set()).map(|scene| production::edit::Placed { section, motion: &scene.asset }))
            .collect();
        let duration = production.sections.last().map_or(production.song_seconds, |section| section.end).max(production.song_seconds);
        let source = production::edit::source(&production.title, &production.song, duration, bpm, &placed, production.width, production.height, production.fps);
        self.nv_note(format!("{} scenes on the timeline", placed.len()));
        self.nv_call(cx, "doc_write", json::obj(vec![("path", json::s("cut.splash")), ("text", json::s(source))]), Want::Place);
        Ok(())
    }

    /// The editor panel of the next scene from `index` that has none:
    /// opened on the graphic desk, made, published; then the step is done.
    fn next_panel(&mut self, cx: &mut Cx, index: usize) {
        let production = self.video_cut.new_video.production.as_ref();
        let next = production.and_then(|p| p.scenes.iter().enumerate().skip(index).find(|(_, scene)| !scene.panel && scene.asset.is_set()).map(|(i, scene)| (i, scene.section.clone())));
        match next {
            Some((i, section)) => {
                self.nv_note(format!("the editor panel of {section}"));
                self.nv_call(cx, "graphic_open", json::obj(vec![("node", json::s(production::edit::node_id(&section)))]), Want::Open(i));
            }
            None => {
                // The clips play the scenes as the desk last published them.
                let srcs = self.video_cut.session.as_ref().map_or(Vec::new(), |session| graphic_srcs(&session.source));
                let production = self.video_cut.new_video.production.as_mut().expect("production");
                for scene in &mut production.scenes {
                    if let Some((_, asset)) = srcs.iter().find(|(node, _)| *node == production::edit::node_id(&scene.section)) {
                        scene.asset = asset.clone();
                    }
                }
                let panels = production.scenes.iter().filter(|scene| scene.panel).count();
                let note = format!("{} scenes written, {panels} with editor panels", production.scenes.len());
                self.nv_done(note);
            }
        }
    }

    /// The cut from the scenes as they are now (their panels included),
    /// with a transition on each cut; then the cut's panel and the save.
    fn assemble(&mut self, cx: &mut Cx) -> Result<(), String> {
        self.place_scenes(cx, false)
    }

    /// After the cut is written: its panel (when this Stage makes edit
    /// panels), then the save.
    fn finish_edit(&mut self, cx: &mut Cx) {
        let cut_panel = self.studio_tools().iter().any(|(spec, available)| spec.name == "cut_panel_make" && *available);
        if cut_panel {
            self.nv_note("the cut's panel".into());
            self.nv_call(cx, "cut_panel_make", Json::Obj(Vec::new()), Want::CutPanel);
        } else {
            self.nv_note("saving the cut".into());
            self.nv_call(cx, "save_cut", Json::Obj(Vec::new()), Want::Save);
        }
    }

    /// The tools a scene author may call, from the control API.
    fn new_video_tools(&self) -> Vec<makepad_story_video_agent::ToolDef> {
        self.studio_tools()
            .into_iter()
            .filter(|(_, available)| *available)
            .map(|(spec, _)| makepad_story_video_agent::ToolDef {
                name: spec.name.to_string(),
                description: spec.description.to_string(),
                parameters: spec.input.to_string(),
                mutates: spec.mutates,
                images: spec.images,
                inspects: spec.name == "inspect",
            })
            .collect()
    }
}

// ------------------------------------------------------------------ AI tools

// The tool specs are in the catalogue (studio_api); these are the host's handlers.
impl crate::StoryApp {
    /// Handle one of the catalogue's new_video tools: each answers at once; the steps run on the pump.
    pub fn new_video_tool_call(&mut self, name: &str, args: &Json) -> Result<Json, String> {
        let page = &mut self.video_cut;
        match name {
            "new_video" => {
                let number = match args.get("seconds") {
                    Some(Json::Int(n)) => *n as f64,
                    Some(Json::F64(n)) => *n,
                    _ => 75.0,
                };
                let lyrics = args.get("lyrics").and_then(Json::as_str).map(str::to_string);
                start(page, text_of(args, "ask"), number, lyrics, args.get("model").and_then(Json::as_str))?;
            }
            "new_video_status" => {}
            "new_video_cancel" => {
                if !page.new_video.stop() {
                    return Err("no video step is running".into());
                }
                super::chat::note(page, "New video: stopping after this step; new_video_redo goes on from there".into());
            }
            "new_video_redo" => match args.get("step").and_then(Json::as_str) {
                Some(step) => {
                    if page.new_video.production.is_none() {
                        page.new_video.production = restore();
                    }
                    let step = Step::from_id(step).ok_or_else(|| format!("`{step}` is not a step: lyrics, music, timing, storyboard, scenes or edit"))?;
                    redo(page, step, args.get("section").and_then(Json::as_str))?;
                }
                None => {
                    if page.new_video.busy() {
                        return Err("a step is running".into());
                    }
                    if page.new_video.production.is_none() {
                        page.new_video.production = restore();
                    }
                    if page.new_video.production.is_none() {
                        return Err("no video is being made: start one with new_video".into());
                    }
                    resume(page);
                }
            },
            _ => return Err(format!("unknown tool `{name}`")),
        }
        Ok(status(page))
    }
}
