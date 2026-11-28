//! The browser safety gate: a film that could take the browser down is not
//! published.
//!
//! A WebGL page that hangs the GPU, runs it out of memory or loses its
//! context can crash the browser's GPU process, and with it every tab of
//! the browser. The export therefore plays what it wrote, before it hands
//! the folder over, and fails on the real signals of that, never on a
//! guessed limit:
//!
//! 1. a shader loop that is not provably bounded ([`crate::shader_loops`],
//!    checked statically over the film's shader pack before any browser
//!    runs it: a loop that never ends is what hangs the GPU);
//! 2. the browser's GPU process restarting or dying, the page crashing,
//!    detaching or not answering, the WebGL context lost;
//! 3. the driver's `GL_OUT_OF_MEMORY` (long frames, the runtime watchdog's
//!    `hung_frames`, are reported with the worst frame time, not failed);
//! 4. growth across a replay: the film is played twice, and at each plate
//!    the second pass may hold at most [`REPLAY_GROWTH`] times the first
//!    pass's live GPU bytes, textures, buffers and wasm memory (a leak grows
//!    with time; a film's legitimate memory is the same on every pass);
//! 5. the runtime's own warnings: a second of play with no draw, the
//!    console lines in [`CONSOLE_FAILURES`] (refused uploads, draws that
//!    wait or draw nothing, out of memory, textures re-created every
//!    frame), and `WebAssembly.Memory.grow` after the first plate.
//!
//! The absolute numbers (peak GPU and wasm memory, the worst frame time,
//! what holds the memory at the peak, the watchdog's held frames per
//! plate, the costliest shaders by static per-pixel cost) are
//! reported for a person to judge; none of them fails the film.
//!
//! The play: headless Chrome with the GPU on (Metal through ANGLE on macOS),
//! muted, at [`VIEWPORT`], a throwaway profile under the build folder
//! (removed after), the folder served on 127.0.0.1
//! ([`makepad_web_pack::serve`]). The film is played from start to end
//! twice, then [`SEEKS`] random seeks with [`SEEK_PLAY_S`] seconds of play
//! after each. Chrome is driven over the DevTools protocol on a pipe
//! (`--remote-debugging-pipe`: NUL-separated JSON on fds 3 and 4), with no
//! client library. The runtime's `makepad_gpu_stats()` and
//! `makepad_gpu_inventory()` are the primary sources; a probe installed
//! before the page's scripts (`safety_page.js`) counts the same from the
//! WebGL and WebAssembly calls for runtimes without them, and draws, grows,
//! context losses and frame times for all.
//!
//! What it cannot see: a crash only a real display, another GPU or a long
//! session brings out (headless Chrome renders offscreen on this machine's
//! GPU). It is a gate against films that crash a browser here, not a proof
//! that no browser anywhere crashes.

use crate::shader_loops::{self, Cost, PackLoops};
use crate::{Phase, Progress};
use makepad_strict_json::{obj, s, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// At a plate, the second pass may hold this many times the first pass's
/// live GPU bytes, textures, buffers and wasm memory (plus
/// [`REPLAY_SLACK_BYTES`] / [`REPLAY_SLACK_OBJECTS`], so that a few objects
/// more on a tiny plate are not a leak).
pub const REPLAY_GROWTH: f64 = 1.1;
pub const REPLAY_SLACK_BYTES: u64 = 4 << 20;
pub const REPLAY_SLACK_OBJECTS: u64 = 8;
/// Console lines (any level) that fail the film: the runtime's own reports
/// of a draw or upload it refused or could not make, of a plate that drew
/// nothing, of the driver out of memory and of textures re-created every
/// frame. (Not every "refused": a single-threaded page's task pool says
/// its background jobs are refused, which is how it runs.)
pub const CONSOLE_FAILURES: [&str; 9] = [
    "refused by the upload budget",
    "draws wait",
    "has drawn nothing",
    "GL_OUT_OF_MEMORY",
    "textures allocated in",
    "WebGL texture upload rejected",
    "WebGL vertex submission rejected",
    "texture allocation threw",
    "texture upload failed",
];
/// Not a limit on the film: the play stops when the page holds this much
/// GPU memory, to keep the machine running the export usable. The gate then
/// did not finish, which fails the film (it cannot be shown safe).
pub const HOST_GPU_STOP_BYTES: u64 = 4 << 30;
/// Random seeks after the two passes.
pub const SEEKS: usize = 20;
/// Seconds of play after each seek.
pub const SEEK_PLAY_S: f64 = 3.0;
/// The page's size in CSS pixels and its device pixel ratio: a large
/// desktop display.
pub const VIEWPORT: (u32, u32, f64) = (1920, 1080, 2.0);
/// The film's first frame, from the page's request.
const START_TIMEOUT: Duration = Duration::from_secs(120);
/// A page that does not answer an evaluation within this is unresponsive.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

/// Chrome where it usually is, per platform.
const CHROME_PATHS: [&str; 4] = [
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/usr/bin/google-chrome",
    "/usr/bin/google-chrome-stable",
    "/usr/bin/chromium",
];

#[derive(Clone, Debug)]
pub struct SafetyOptions {
    /// Run the gate (default). Off, the export says it did not.
    pub enabled: bool,
    /// The Chrome executable; else `MAKEPAD_CHROME`, else where it usually is.
    pub chrome: Option<PathBuf>,
    /// Full passes over the film (2: the second is compared with the first).
    pub passes: usize,
    pub seeks: usize,
    pub seek_play_s: f64,
    /// CSS width, height and device pixel ratio.
    pub viewport: (u32, u32, f64),
    /// The seeks' random seed (fixed: a rerun seeks to the same times).
    pub seed: u64,
    /// The runtime's console reports fail the film without ending the play
    /// (a coverage run must see all of it).
    pub keep_playing: bool,
}

impl Default for SafetyOptions {
    fn default() -> Self {
        SafetyOptions { enabled: true, chrome: None, passes: 2, seeks: SEEKS, seek_play_s: SEEK_PLAY_S, viewport: VIEWPORT, seed: 0x5afe_f11e, keep_playing: false }
    }
}

impl SafetyOptions {
    /// The Chrome to run, if there is one.
    pub fn chrome_path(&self) -> Option<PathBuf> {
        let from_env = std::env::var_os("MAKEPAD_CHROME").map(PathBuf::from);
        match self.chrome.clone().or(from_env) {
            Some(p) => p.is_file().then_some(p),
            None => CHROME_PATHS.iter().map(PathBuf::from).find(|p| p.is_file()),
        }
    }
}

/// The film the gate plays.
pub struct Film<'a> {
    /// The export's folder (served as it is).
    pub dir: &'a Path,
    /// Seconds.
    pub duration: f64,
    /// The plates' start times (seconds, ascending).
    pub plates: Vec<f64>,
    /// The song is beside the page: its media element is the film's clock.
    pub has_music: bool,
    /// The film's shader pack, for the loop check and the cost list.
    pub shaders: Option<&'a [u8]>,
}

impl Film<'_> {
    fn plate_at(&self, t: f64) -> usize {
        self.plates.iter().rposition(|s| *s <= t + 1e-6).unwrap_or(0)
    }
}

/// What one plate held on one pass.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlateUse {
    pub seen: bool,
    /// At the end of the plate's visit: live GPU bytes, textures, buffers,
    /// wasm bytes.
    pub gpu_bytes: u64,
    pub textures: u64,
    pub buffers: u64,
    pub wasm_bytes: u64,
    /// The highest live GPU bytes during the visit.
    pub peak_bytes: u64,
    /// The watchdog's frames and held frames during the visit.
    pub frames: u64,
    pub held_frames: u64,
}

/// What the gate saw.
#[derive(Clone, Debug, Default)]
pub struct SafetyReport {
    /// Why the browser part did not run (no Chrome, turned off): not a pass.
    pub skipped: Option<String>,
    pub loops: PackLoops,
    /// The costliest fragment shaders by static per-pixel cost.
    pub costliest: Vec<(String, Cost)>,
    /// Seconds of film played in the browser (passes and seeks).
    pub played_s: f64,
    pub passes: usize,
    pub seeks: usize,
    /// Whether the numbers come from the runtime's `makepad_gpu_stats()`
    /// (else the gate's own probe).
    pub runtime_stats: bool,
    pub peak_wasm_bytes: u64,
    pub peak_gpu_bytes: u64,
    /// What held GPU memory at the peak: (what, count, bytes), and whose
    /// list it is (`page`: the runtime's inventory with owners; `probe`).
    pub breakdown: Vec<(String, u64, u64)>,
    pub breakdown_source: String,
    /// The longest frame GPU time (the runtime's watchdog, else the timer
    /// query results).
    pub max_gpu_frame_ms: Option<f64>,
    pub max_frame_interval_ms: f64,
    pub frames: u64,
    pub held_frames: u64,
    pub hung_frames: u64,
    pub gpu_restarts: u32,
    pub context_losses: u32,
    pub page_crashes: u32,
    pub draws: u64,
    /// Per pass, per plate.
    pub plates: Vec<Vec<PlateUse>>,
    /// Each a reason the film is not published.
    pub failures: Vec<String>,
    pub notes: Vec<String>,
}

impl SafetyReport {
    pub fn passed(&self) -> bool {
        self.failures.is_empty() && self.skipped.is_none()
    }

    /// One line with the numbers.
    pub fn summary(&self) -> String {
        let mb = |b: u64| b as f64 / 1048576.0;
        let gpu_ms = match self.max_gpu_frame_ms {
            Some(ms) => format!("worst GPU frame {ms:.1} ms"),
            None => "no GPU frame times".to_string(),
        };
        let per_plate: Vec<String> = self
            .plates
            .iter()
            .enumerate()
            .map(|(pass, plates)| {
                let cells: Vec<String> = plates.iter().enumerate().filter(|(_, p)| p.seen).map(|(i, p)| format!("#{i} {:.0}/{:.0} MB {}/{} held", mb(p.gpu_bytes), mb(p.peak_bytes), p.held_frames, p.frames)).collect();
                format!("pass {}: {}", pass + 1, cells.join(", "))
            })
            .collect();
        let held = if self.frames > 0 { format!("{}/{} frames held", self.held_frames, self.frames) } else { "no watchdog".into() };
        format!(
            "{} shaders ({} bounded loops, {} guarded); {:.0} s played ({} passes, {} seeks); peak GPU {:.0} MB, peak wasm {:.0} MB; {gpu_ms}, max frame interval {:.0} ms, {held}, {} hung; GPU process restarts {}, context losses {}, page crashes {}; {} numbers; per graphic(end/peak MB, held/frames): {}",
            self.loops.shaders,
            self.loops.bounded,
            self.loops.guarded,
            self.played_s,
            self.passes,
            self.seeks,
            mb(self.peak_gpu_bytes),
            mb(self.peak_wasm_bytes),
            self.max_frame_interval_ms,
            self.hung_frames,
            self.gpu_restarts,
            self.context_losses,
            self.page_crashes,
            if self.runtime_stats { "runtime ledger" } else { "probe" },
            per_plate.join("; ")
        )
    }

    /// What held GPU memory at the peak, and the costliest shaders.
    pub fn details(&self) -> String {
        let mut out = String::new();
        if !self.breakdown.is_empty() {
            out.push_str(&format!("GPU memory at the peak ({}):", self.breakdown_source));
            for (what, count, bytes) in &self.breakdown {
                out.push_str(&format!("\n  {:>7.1} MB  {count:>4}x {what}", *bytes as f64 / 1048576.0));
            }
            out.push('\n');
        }
        if !self.costliest.is_empty() {
            out.push_str("costliest fragment shaders by literal loops (static per-pixel estimate: ops + 8 x fetches; * = also has data-dependent loops, counted once, so it costs more):");
            for (name, c) in &self.costliest {
                out.push_str(&format!("\n  {:>9.0}{} {name} ({:.0} ops, {:.0} fetches)", c.total(), if c.data_dependent { "*" } else { " " }, c.ops, c.fetches));
            }
            out.push('\n');
        }
        out
    }
}

/// Runs the gate on `film`, with `work_dir` for the browser's throwaway
/// profile. Failures are in the report.
pub fn gate(film: &Film, opts: &SafetyOptions, work_dir: &Path, progress: &mut dyn Progress) -> SafetyReport {
    let mut report = SafetyReport::default();
    // 1. The loops, before any browser runs them.
    if let Some(pack) = film.shaders {
        match shader_loops::check_pack(pack) {
            Ok(loops) => report.loops = loops,
            Err(e) => report.failures.push(format!("the shader pack could not be read: {e}")),
        }
        report.costliest = shader_loops::costliest_of_pack(pack, 5).unwrap_or_default();
        for u in &report.loops.unbounded {
            report.failures.push(format!("shader {} ({}, line {}): a loop that may not end ({}): {}", u.shader, u.stage, u.line, u.reason, u.head));
        }
        progress.phase(Phase::Safety, 0.02, &format!("{} shaders: {} bounded loops, {} guarded, {} unbounded", report.loops.shaders, report.loops.bounded, report.loops.guarded, report.loops.unbounded.len()));
        if !report.failures.is_empty() {
            report.notes.push("not played in a browser: an unbounded shader loop can hang the GPU".into());
            return report;
        }
    }
    if !opts.enabled {
        report.skipped = Some("the browser safety gate is turned off".into());
        return report;
    }
    let Some(chrome) = opts.chrome_path() else {
        report.skipped = Some("no Chrome found (set MAKEPAD_CHROME or the export's Chrome path): the film was not played in a browser".into());
        return report;
    };
    if !cfg!(unix) {
        report.skipped = Some("the browser safety gate drives Chrome over a pipe, which this build does on macOS and Linux".into());
        return report;
    }
    let server = match makepad_web_pack::serve::serve(film.dir, false, 0) {
        Ok(s) => s,
        Err(e) => {
            report.failures.push(format!("could not serve the film: {e}"));
            return report;
        }
    };
    let url = format!("http://127.0.0.1:{}/index.html", server.addr.port());
    let profile = work_dir.join("safety-profile");
    let _ = std::fs::remove_dir_all(&profile);
    let mut browser = match Browser::launch(&chrome, &profile, (opts.viewport.0, opts.viewport.1)) {
        Ok(b) => b,
        Err(e) => {
            report.failures.push(format!("could not start Chrome ({}): {e}", chrome.display()));
            return report;
        }
    };
    let mut play = Play {
        film,
        opts,
        report: &mut report,
        browser: &mut browser,
        session: String::new(),
        gpu_pid: None,
        gpu_missing: 0,
        last: Sample::default(),
        started: Instant::now(),
        pass: None,
        playing: false,
        clock_start: None,
        last_draws: None,
        grows_after_first_plate: None,
        stats_prev: (0, 0),
        breakdown_at: 0,
        process_types: String::new(),
    };
    if let Err(e) = play.run(&url, progress) {
        play.report.failures.push(e);
    }
    play.read_coverage(work_dir);
    play.breakdown_now();
    play.finish();
    browser.close();
    let _ = std::fs::remove_dir_all(&profile);
    report
}

/// One read-out of the page.
#[derive(Clone, Debug, Default)]
struct Sample {
    wasm: u64,
    gpu_live: u64,
    gpu_peak: u64,
    textures: u64,
    buffers: u64,
    draws: u64,
    grows: u64,
    lost: u32,
    frames: u64,
    interval_max: f64,
    gpu_ms_max: f64,
    gpu_samples: u64,
    context_lost_now: bool,
    loader: u64,
    media: Option<Media>,
    /// `makepad_gpu_stats()`: frames, held, hung, worst ms.
    stats: Option<(u64, u64, u64, f64)>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Media {
    t: f64,
    paused: bool,
    ended: bool,
}

fn num(v: &Value, key: &str) -> f64 {
    match v.get(key) {
        Some(Value::Int(i)) => *i as f64,
        Some(Value::F64(f)) => *f,
        _ => 0.0,
    }
}

impl Sample {
    fn parse(v: &Value) -> Sample {
        let mut s = Sample {
            wasm: num(v, "wasm") as u64,
            gpu_live: num(v, "gpu_live") as u64,
            gpu_peak: num(v, "gpu_peak") as u64,
            textures: v.get("live").map_or(0.0, |l| num(l, "textures")) as u64,
            buffers: v.get("live").map_or(0.0, |l| num(l, "buffers")) as u64,
            draws: num(v, "draws") as u64,
            grows: num(v, "grows") as u64,
            lost: num(v, "lost") as u32,
            frames: num(v, "frames") as u64,
            interval_max: num(v, "interval_max"),
            gpu_ms_max: num(v, "gpu_ms_max"),
            gpu_samples: num(v, "gpu_samples") as u64,
            context_lost_now: v.get("context_lost_now").and_then(|b| b.as_bool()).unwrap_or(false),
            loader: num(v, "loader") as u64,
            media: v.get("media").filter(|m| !m.is_null()).map(|m| Media {
                t: num(m, "t"),
                paused: m.get("paused").and_then(|b| b.as_bool()).unwrap_or(true),
                ended: m.get("ended").and_then(|b| b.as_bool()).unwrap_or(false),
            }),
            stats: None,
        };
        // The runtime's own numbers, where it publishes them.
        if let Some(st) = v.get("stats").filter(|x| !x.is_null()) {
            s.stats = Some((num(st, "frames") as u64, num(st, "held_frames") as u64, num(st, "hung_frames") as u64, num(st, "worst_frame_ms")));
            if let Some(l) = st.get("ledger").filter(|x| !x.is_null()) {
                s.gpu_live = num(l, "live_bytes") as u64;
                s.gpu_peak = num(l, "peak_bytes") as u64;
                s.textures = num(l, "live_textures") as u64;
            }
        }
        s
    }
}

struct Play<'a, 'b> {
    film: &'a Film<'a>,
    opts: &'a SafetyOptions,
    report: &'b mut SafetyReport,
    browser: &'b mut Browser,
    session: String,
    /// The GPU process the browser started with.
    gpu_pid: Option<i64>,
    gpu_missing: u32,
    last: Sample,
    started: Instant,
    /// The full pass being played (per-plate numbers go to it).
    pass: Option<usize>,
    /// The film is playing (a second without a draw then fails).
    playing: bool,
    /// Wall clock of the pass's start (the film's clock without a song).
    clock_start: Option<Instant>,
    last_draws: Option<(u64, Instant)>,
    /// Memory.grow count once the first plate was over.
    grows_after_first_plate: Option<u64>,
    /// The watchdog's (frames, held) at the last sample.
    stats_prev: (u64, u64),
    /// Live GPU bytes at the last breakdown.
    breakdown_at: u64,
    /// The browser's process types, for the report when none is the GPU's.
    process_types: String,
}

impl Play<'_, '_> {
    fn run(&mut self, url: &str, progress: &mut dyn Progress) -> Result<(), String> {
        let b = &mut *self.browser;
        b.call(None, "Target.setDiscoverTargets", obj(vec![("discover", Value::Bool(true))]))?;
        let target = b.call(None, "Target.createTarget", obj(vec![("url", s("about:blank"))]))?;
        let target_id = target.get("targetId").and_then(|v| v.as_str()).ok_or("Chrome made no page")?.to_string();
        let attached = b.call(None, "Target.attachToTarget", obj(vec![("targetId", s(target_id)), ("flatten", Value::Bool(true))]))?;
        self.session = attached.get("sessionId").and_then(|v| v.as_str()).ok_or("Chrome did not attach to the page")?.to_string();
        let sid = Some(self.session.as_str());
        for method in ["Page.enable", "Runtime.enable", "Inspector.enable"] {
            b.call(sid, method, obj(vec![]))?;
        }
        let (w, h, dpr) = self.opts.viewport;
        b.call(sid, "Emulation.setDeviceMetricsOverride", obj(vec![("width", Value::Int(w as i64)), ("height", Value::Int(h as i64)), ("deviceScaleFactor", Value::F64(dpr)), ("mobile", Value::Bool(false))]))?;
        b.call(sid, "Page.addScriptToEvaluateOnNewDocument", obj(vec![("source", s(PROBE))]))?;
        b.call(sid, "Page.navigate", obj(vec![("url", s(url))]))?;
        self.started = Instant::now();
        progress.phase(Phase::Safety, 0.05, "loading the film in headless Chrome");

        // The film draws.
        loop {
            self.wait(0.5, progress)?;
            if self.last.frames >= 30 && self.last.loader == 0 {
                break;
            }
            if self.started.elapsed() > START_TIMEOUT {
                return Err(format!("the film did not start drawing within {} s", START_TIMEOUT.as_secs()));
            }
        }
        let plates = self.film.plates.len().max(1);
        self.report.plates = vec![vec![PlateUse::default(); plates]; self.opts.passes];
        let total = self.film.duration * self.opts.passes as f64 + self.opts.seeks as f64 * self.opts.seek_play_s;
        let fraction = |played: f64| 0.1 + 0.85 * (played / total.max(1.0)).min(1.0);

        // The whole film, `passes` times (a click on the picture plays it
        // from the start once it has ended).
        for pass in 0..self.opts.passes {
            self.start_playing(progress)?;
            if pass == 0 {
                self.sample(true)?;
            }
            self.pass = Some(pass);
            let play_start = Instant::now();
            self.clock_start = Some(play_start);
            let limit = self.film.duration * 1.5 + 30.0;
            loop {
                self.wait(1.0, progress)?;
                let wall = play_start.elapsed().as_secs_f64();
                let at = self.clock();
                self.report.passes = pass + 1;
                self.report.played_s = pass as f64 * self.film.duration + wall.min(self.film.duration);
                let ended = match self.last.media {
                    Some(m) if self.film.has_music => m.ended || m.t >= self.film.duration - 0.5,
                    _ => wall >= self.film.duration + 1.0,
                };
                progress.phase(Phase::Safety, fraction(pass as f64 * self.film.duration + at), &format!("pass {} of {}: {:.0} / {:.0} s", pass + 1, self.opts.passes, at, self.film.duration));
                if ended {
                    break;
                }
                if wall > limit {
                    self.report.notes.push(format!("pass {}: the film's clock reached {at:.0} of {:.0} s in {wall:.0} s of play: stopped there", pass + 1, self.film.duration));
                    break;
                }
            }
            self.report.played_s = pass as f64 * self.film.duration + play_start.elapsed().as_secs_f64().min(self.film.duration);
            self.pass = None;
            self.playing = false;
            if pass + 1 < self.opts.passes {
                // Let the end settle before the next pass's click.
                self.wait(1.0, progress)?;
            }
        }

        // Random seeks, a few seconds of play after each.
        let mut rng = self.opts.seed | 1;
        for i in 0..self.opts.seeks {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let at = 0.02 + 0.93 * ((rng >> 11) as f64 / (1u64 << 53) as f64);
            if !self.playing || (self.film.has_music && self.last.media.is_some_and(|m| m.paused || m.ended)) {
                self.start_playing(progress)?;
            }
            self.seek(at)?;
            let seek_start = Instant::now();
            while seek_start.elapsed().as_secs_f64() < self.opts.seek_play_s {
                self.wait(1.0_f64.min(self.opts.seek_play_s), progress)?;
            }
            self.report.played_s += seek_start.elapsed().as_secs_f64();
            self.report.seeks = i + 1;
            progress.phase(Phase::Safety, fraction(self.film.duration * self.opts.passes as f64 + (i + 1) as f64 * self.opts.seek_play_s), &format!("seek {} / {} to {:.0} s", i + 1, self.opts.seeks, at * self.film.duration));
        }
        Ok(())
    }

    /// The film's time: the song's position, else the wall clock since the
    /// pass began.
    fn clock(&self) -> f64 {
        match self.last.media {
            Some(m) if self.film.has_music => m.t,
            _ => self.clock_start.map_or(0.0, |s| s.elapsed().as_secs_f64()),
        }
    }

    /// Clicks the picture until the song plays (a film with a song), or
    /// once after a moment to load (one without: nothing shows it plays).
    fn start_playing(&mut self, progress: &mut dyn Progress) -> Result<(), String> {
        let (w, h, _) = self.opts.viewport;
        if !self.film.has_music {
            self.wait(if self.report.passes == 0 && !self.playing { 5.0 } else { 0.5 }, progress)?;
            self.click(w as f64 * 0.5, h as f64 * 0.4)?;
            self.playing = true;
            self.last_draws = None;
            return Ok(());
        }
        let since = Instant::now();
        loop {
            self.click(w as f64 * 0.5, h as f64 * 0.4)?;
            self.wait(1.5, progress)?;
            if self.last.media.is_some_and(|m| !m.paused && !m.ended) {
                self.playing = true;
                self.last_draws = None;
                return Ok(());
            }
            if since.elapsed() > Duration::from_secs(90) {
                return Err("the film did not start playing within 90 s of drawing (its song never played)".into());
            }
        }
    }

    /// A press and release on the player's timeline at `at` (0..1): it
    /// keeps playing from there. The timeline's place is the player's
    /// control strip (story/apps/player/src/player.rs `draw_controls`):
    /// from x = 168 to 16 from the right, 28 above the bottom.
    fn seek(&mut self, at: f64) -> Result<(), String> {
        let (w, h) = (self.opts.viewport.0 as f64, self.opts.viewport.1 as f64);
        self.click(168.0 + (w - 184.0) * at, h - 28.0)
    }

    fn click(&mut self, x: f64, y: f64) -> Result<(), String> {
        let sid = Some(self.session.clone());
        for kind in ["mouseMoved", "mousePressed", "mouseReleased"] {
            let mut p = vec![("type", s(kind)), ("x", Value::F64(x)), ("y", Value::F64(y))];
            if kind != "mouseMoved" {
                p.push(("button", s("left")));
                p.push(("clickCount", Value::Int(1)));
            }
            self.browser.call(sid.as_deref(), "Input.dispatchMouseEvent", obj(p))?;
        }
        Ok(())
    }

    /// Waits `secs`, watching: the page, the browser's processes, events.
    fn wait(&mut self, secs: f64, progress: &mut dyn Progress) -> Result<(), String> {
        let until = Instant::now() + Duration::from_secs_f64(secs);
        while Instant::now() < until {
            if progress.cancelled() {
                return Err("cancelled".into());
            }
            std::thread::sleep(until.saturating_duration_since(Instant::now()).min(Duration::from_millis(250)));
            self.watch()?;
        }
        self.sample(false)?;
        self.watch_gpu_process()?;
        Ok(())
    }

    /// Browser events so far: crashes, detaches and the runtime's console
    /// warnings end the play.
    fn watch(&mut self) -> Result<(), String> {
        self.browser.pump();
        let events = std::mem::take(&mut self.browser.events);
        let mut fatal = None;
        for e in events {
            let method = e.get("method").and_then(|m| m.as_str()).unwrap_or("");
            let params = e.get("params");
            match method {
                "Inspector.targetCrashed" | "Target.targetCrashed" => {
                    self.report.page_crashes += 1;
                    fatal = Some(format!("the page crashed ({method})"));
                }
                "Target.detachedFromTarget" if params.and_then(|p| p.get("sessionId")).and_then(|v| v.as_str()) == Some(self.session.as_str()) => {
                    fatal = Some("the browser detached the page".to_string());
                }
                "Runtime.consoleAPICalled" => {
                    let text: Vec<String> = params
                        .and_then(|p| p.get("args"))
                        .and_then(|a| a.as_arr())
                        .unwrap_or(&[])
                        .iter()
                        .filter_map(|a| a.get("value").and_then(|v| v.as_str()).or_else(|| a.get("description").and_then(|v| v.as_str())))
                        .map(|s| s.to_string())
                        .collect();
                    let line = text.join(" ");
                    if let Some(p) = CONSOLE_FAILURES.iter().find(|p| line.contains(*p)) {
                        let failure = format!("the page reported \"{p}\": {}", line.chars().take(300).collect::<String>());
                        if !self.opts.keep_playing {
                            fatal.get_or_insert(failure);
                        } else if self.report.failures.len() < 40 {
                            self.report.failures.push(failure);
                        }
                    } else if self.opts.keep_playing && line.contains("not found") && self.report.notes.len() < 40 {
                        // A stripped build's startup misses (the script's
                        // `not found` errors), for the keep list.
                        self.report.notes.push(format!("console: {}", line.chars().take(300).collect::<String>()));
                    }
                }
                "Runtime.exceptionThrown" if self.report.notes.len() < 40 => {
                    let text = params.and_then(|p| p.get("exceptionDetails")).and_then(|d| d.get("exception").and_then(|x| x.get("description")).or_else(|| d.get("text"))).and_then(|t| t.as_str()).unwrap_or("");
                    self.report.notes.push(format!("page exception: {}", text.lines().next().unwrap_or("")));
                }
                _ => {}
            }
        }
        let gpu_log = self.browser.gpu_log.load(Ordering::Relaxed);
        if gpu_log > 0 {
            self.report.gpu_restarts = self.report.gpu_restarts.max(gpu_log);
            fatal.get_or_insert_with(|| format!("Chrome's GPU process died: {}", self.browser.gpu_lines.lock().map(|l| l.join(" | ")).unwrap_or_default()));
        }
        if let Some(code) = self.browser.exited() {
            fatal.get_or_insert_with(|| format!("Chrome exited ({code})"));
        }
        fatal.map_or(Ok(()), Err)
    }

    /// The browser's GPU process: the same one throughout.
    fn watch_gpu_process(&mut self) -> Result<(), String> {
        let info = self.browser.call(None, "SystemInfo.getProcessInfo", obj(vec![]))?;
        let procs = info.get("processInfo").and_then(|p| p.as_arr()).unwrap_or(&[]);
        let mut types: Vec<&str> = procs.iter().filter_map(|p| p.get("type").and_then(|t| t.as_str())).collect();
        types.sort();
        types.dedup();
        self.process_types = types.join(", ");
        let gpu: Vec<i64> = info
            .get("processInfo")
            .and_then(|p| p.as_arr())
            .unwrap_or(&[])
            .iter()
            .filter(|p| p.get("type").and_then(|t| t.as_str()).is_some_and(|t| t.eq_ignore_ascii_case("gpu")))
            .filter_map(|p| p.get("id").and_then(|i| i.as_i64()))
            .collect();
        match (self.gpu_pid, gpu.first()) {
            (None, Some(&pid)) => self.gpu_pid = Some(pid),
            (Some(pid), Some(&now)) if now != pid => {
                self.report.gpu_restarts += 1;
                self.gpu_pid = Some(now);
                return Err(format!("Chrome's GPU process restarted (pid {pid} -> {now})"));
            }
            (Some(pid), None) => {
                self.gpu_missing += 1;
                if self.gpu_missing >= 2 {
                    self.report.gpu_restarts += 1;
                    return Err(format!("Chrome's GPU process (pid {pid}) is gone"));
                }
            }
            _ => self.gpu_missing = 0,
        }
        Ok(())
    }

    fn evaluate(&mut self, expr: &str, await_promise: bool) -> Result<Option<Value>, String> {
        let sid = Some(self.session.clone());
        let params = obj(vec![("expression", s(expr)), ("returnByValue", Value::Bool(true)), ("awaitPromise", Value::Bool(await_promise))]);
        let r = match self.browser.call_timeout(sid.as_deref(), "Runtime.evaluate", params, ANSWER_TIMEOUT) {
            Ok(r) => r,
            Err(e) if e.starts_with("timeout") => return Err(format!("the page did not answer for {} s (unresponsive)", ANSWER_TIMEOUT.as_secs())),
            Err(e) => return Err(e),
        };
        let Some(json) = r.get("result").and_then(|r| r.get("value")).and_then(|v| v.as_str()) else {
            // Before the probe's page is up (about:blank, navigation).
            return Ok(None);
        };
        makepad_strict_json::parse_depth(json.as_bytes(), 16).map(Some).map_err(|e| format!("the probe's read-out: {e}"))
    }

    /// Reads the page (with `counting`, frame intervals start counting:
    /// loading frames do not), and checks the real signals.
    fn sample(&mut self, counting: bool) -> Result<(), String> {
        let expr = if counting { "window.__mp_safety_sample(\"counting\")" } else { "window.__mp_safety_sample()" };
        let Some(v) = self.evaluate(expr, false)? else { return Ok(()) };
        let sample = Sample::parse(&v);
        let now = Instant::now();
        let r = &mut *self.report;
        r.runtime_stats |= sample.stats.is_some();
        r.peak_wasm_bytes = r.peak_wasm_bytes.max(sample.wasm);
        let new_peak = sample.gpu_peak.max(sample.gpu_live) > r.peak_gpu_bytes;
        r.peak_gpu_bytes = r.peak_gpu_bytes.max(sample.gpu_peak).max(sample.gpu_live);
        r.context_losses = r.context_losses.max(sample.lost);
        r.max_frame_interval_ms = r.max_frame_interval_ms.max(sample.interval_max);
        r.draws = sample.draws;
        if sample.gpu_samples > 0 {
            r.max_gpu_frame_ms = Some(r.max_gpu_frame_ms.unwrap_or(0.0).max(sample.gpu_ms_max));
        }
        let (mut d_frames, mut d_held) = (0, 0);
        if let Some((frames, held, hung, worst)) = sample.stats {
            r.frames = frames;
            r.held_frames = held;
            r.hung_frames = hung;
            if worst > 0.0 {
                r.max_gpu_frame_ms = Some(r.max_gpu_frame_ms.unwrap_or(0.0).max(worst));
            }
            d_frames = frames.saturating_sub(self.stats_prev.0);
            d_held = held.saturating_sub(self.stats_prev.1);
            self.stats_prev = (frames, held);
        }
        // This plate's numbers on this pass.
        let at = match sample.media {
            Some(m) if self.film.has_music => m.t,
            _ => self.clock_start.map_or(0.0, |s| s.elapsed().as_secs_f64()),
        };
        let plate = self.film.plate_at(at);
        if let Some(pass) = self.pass {
            if let Some(p) = r.plates.get_mut(pass).and_then(|ps| ps.get_mut(plate)) {
                p.seen = true;
                p.gpu_bytes = sample.gpu_live;
                p.textures = sample.textures;
                p.buffers = sample.buffers;
                p.wasm_bytes = sample.wasm;
                p.peak_bytes = p.peak_bytes.max(sample.gpu_live);
                p.frames += d_frames;
                p.held_frames += d_held;
            }
            // Memory.grow: none once the first plate is over.
            if pass == 0 && plate >= 1 && self.grows_after_first_plate.is_none() {
                self.grows_after_first_plate = Some(sample.grows);
            }
        }
        let grows = sample.grows;
        let draws = sample.draws;
        let lost = sample.lost > 0 || sample.context_lost_now;
        self.last = sample;
        if lost {
            return Err("the page lost its WebGL context".into());
        }
        // A long frame (the runtime's watchdog counts frames over its bound:
        // the first GPU frame at load takes seconds) is reported with the
        // worst frame time, not failed: no guessed thresholds. A GPU that
        // really hangs shows as a lost context or a GPU-process restart.
        if let Some(base) = self.grows_after_first_plate {
            if grows > base {
                return Err(format!("wasm memory grew after the first graphic({} grows, now {} MB)", grows - base, self.report.peak_wasm_bytes >> 20));
            }
        }
        if self.playing {
            match self.last_draws {
                Some((d, since)) if draws == d && now.duration_since(since) >= Duration::from_secs(1) => {
                    return Err(format!("the film drew nothing for {:.1} s while it played (at {:.1} s)", now.duration_since(since).as_secs_f64(), at));
                }
                Some((d, _)) if draws == d => {}
                _ => self.last_draws = Some((draws, now)),
            }
        }
        if self.report.peak_gpu_bytes > HOST_GPU_STOP_BYTES {
            self.breakdown_now();
            return Err(format!("the play was stopped at {} MB of live GPU memory to keep this machine usable, before the film was shown safe", self.report.peak_gpu_bytes >> 20));
        }
        if new_peak && self.report.peak_gpu_bytes > self.breakdown_at + self.breakdown_at / 10 + (8 << 20) {
            self.breakdown_now();
        }
        Ok(())
    }

    /// What holds GPU memory now (kept as the peak's).
    fn breakdown_now(&mut self) {
        let Ok(Some(v)) = self.evaluate("window.__mp_safety_breakdown && window.__mp_safety_breakdown()", true) else { return };
        let items: Vec<(String, u64, u64)> = v
            .get("items")
            .and_then(|i| i.as_arr())
            .unwrap_or(&[])
            .iter()
            .map(|i| (i.get("what").and_then(|w| w.as_str()).unwrap_or("?").to_string(), num(i, "count") as u64, num(i, "bytes") as u64))
            .collect();
        let total: u64 = items.iter().map(|i| i.2).sum();
        // Only a larger total replaces the peak's list.
        let held: u64 = self.report.breakdown.iter().map(|i| i.2).sum();
        if total >= held {
            self.report.breakdown = items;
            self.report.breakdown_source = v.get("source").and_then(|s| s.as_str()).unwrap_or("probe").to_string();
            self.breakdown_at = self.report.peak_gpu_bytes;
        }
    }

    /// A coverage build's probe memory (`__cov`, see the wasm optimiser's
    /// `units`), written to `<work_dir>/coverage.bin`: which functions the
    /// whole play entered.
    fn read_coverage(&mut self, work_dir: &Path) {
        let Ok(Some(v)) = self.evaluate("JSON.stringify(window.__mp_cov_read ? window.__mp_cov_read() : null)", false) else { return };
        let Some(b64) = v.as_str() else { return };
        let bytes = base64_decode(b64);
        let path = work_dir.join("coverage.bin");
        match std::fs::write(&path, &bytes) {
            Ok(()) => self.report.notes.push(format!("coverage: {} entered of {} probes, written to {}", bytes.iter().filter(|b| **b != 0).count(), bytes.len(), path.display())),
            Err(e) => self.report.failures.push(format!("coverage: {}: {e}", path.display())),
        }
    }

    /// The checks that need the whole play: growth across the replay.
    fn finish(&mut self) {
        let r = &mut *self.report;
        if let Some(leak) = replay_growth(&r.plates) {
            r.failures.push(leak);
        }
        if !r.runtime_stats {
            r.notes.push("the page publishes no makepad_gpu_stats(): GPU memory and hung frames from the gate's own probe (no watchdog)".into());
        }
        if self.gpu_pid.is_none() {
            r.notes.push(format!("Chrome reported no GPU process (process types seen: {}): GPU process restarts are seen only in its log", self.process_types));
        }
    }
}

/// Standard base64 (the page's `btoa`) to bytes; anything else is skipped.
fn base64_decode(text: &str) -> Vec<u8> {
    let value = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for v in text.bytes().filter_map(value) {
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

/// Growth across the replay: at each plate both passes saw, the last pass
/// against the first (live GPU bytes, textures, buffers, wasm memory).
pub fn replay_growth(plates: &[Vec<PlateUse>]) -> Option<String> {
    let (first, last) = (plates.first()?, plates.last()?);
    if plates.len() < 2 {
        return None;
    }
    let grew = |a: u64, b: u64, slack: u64| b as f64 > a as f64 * REPLAY_GROWTH + slack as f64;
    for (i, (a, b)) in first.iter().zip(last.iter()).enumerate() {
        if !(a.seen && b.seen) {
            continue;
        }
        let mb = |x: u64| x as f64 / 1048576.0;
        if grew(a.gpu_bytes, b.gpu_bytes, REPLAY_SLACK_BYTES) {
            return Some(format!("GPU memory grows across a replay: plate {i} holds {:.0} MB on the second pass against {:.0} MB on the first", mb(b.gpu_bytes), mb(a.gpu_bytes)));
        }
        if grew(a.textures, b.textures, REPLAY_SLACK_OBJECTS) {
            return Some(format!("live textures grow across a replay: plate {i} holds {} on the second pass against {} on the first", b.textures, a.textures));
        }
        if grew(a.buffers, b.buffers, REPLAY_SLACK_OBJECTS) {
            return Some(format!("live buffers grow across a replay: plate {i} holds {} on the second pass against {} on the first", b.buffers, a.buffers));
        }
        if grew(a.wasm_bytes, b.wasm_bytes, 0) {
            return Some(format!("wasm memory grows across a replay: plate {i} at {:.0} MB on the second pass against {:.0} MB on the first", mb(b.wasm_bytes), mb(a.wasm_bytes)));
        }
    }
    None
}

/// Chrome on a DevTools pipe.
struct Browser {
    child: Child,
    to: std::io::PipeWriter,
    from: Receiver<Value>,
    next_id: i64,
    /// Events received while waiting for answers.
    events: Vec<Value>,
    /// GPU process deaths in Chrome's log.
    gpu_log: Arc<AtomicU32>,
    gpu_lines: Arc<Mutex<Vec<String>>>,
}

/// Lines of Chrome's log that say its GPU process died (read by the Unix launch's log thread).
#[cfg(unix)]
const GPU_DEATH: [&str; 5] = ["GPU process exited unexpectedly", "GPU process isn't usable", "GPU process crashed", "Lost UI shared context", "GPU process launch failed"];

#[cfg(unix)]
mod fds {
    use std::os::raw::c_int;
    extern "C" {
        pub fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
        pub fn dup2(from: c_int, to: c_int) -> c_int;
        pub fn close(fd: c_int) -> c_int;
    }
    /// `F_DUPFD` (the same on macOS and Linux).
    pub const F_DUPFD: c_int = 0;
}

impl Browser {
    #[cfg(unix)]
    fn launch(chrome: &Path, profile: &Path, size: (u32, u32)) -> Result<Browser, String> {
        use std::io::Read;
        use std::os::fd::AsRawFd;
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        std::fs::create_dir_all(profile).map_err(|e| format!("{}: {e}", profile.display()))?;
        // Chrome reads commands on fd 3 and writes answers on fd 4.
        let (cmd_read, cmd_write) = std::io::pipe().map_err(|e| e.to_string())?;
        let (out_read, out_write) = std::io::pipe().map_err(|e| e.to_string())?;
        let (r, w) = (cmd_read.as_raw_fd(), out_write.as_raw_fd());
        let mut cmd = Command::new(chrome);
        cmd.args([
            "--headless=new",
            "--enable-gpu",
            "--ignore-gpu-blocklist",
            "--mute-audio",
            "--autoplay-policy=no-user-gesture-required",
            "--remote-debugging-pipe",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-extensions",
            "--disable-background-networking",
            "--disable-sync",
            "--disable-background-timer-throttling",
            "--disable-renderer-backgrounding",
            "--disable-backgrounding-occluded-windows",
            &format!("--window-size={},{}", size.0, size.1),
            &format!("--user-data-dir={}", profile.display()),
        ]);
        if cfg!(target_os = "macos") {
            cmd.arg("--use-angle=metal");
        }
        cmd.arg("about:blank").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            cmd.pre_exec(move || {
                // Above 3 and 4 first, so neither dup2 clobbers the other's source.
                let a = fds::fcntl(r, fds::F_DUPFD, 10);
                let b = fds::fcntl(w, fds::F_DUPFD, 10);
                if a < 0 || b < 0 || fds::dup2(a, 3) < 0 || fds::dup2(b, 4) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                fds::close(a);
                fds::close(b);
                Ok(())
            });
        }
        let mut child = cmd.spawn().map_err(|e| e.to_string())?;
        drop(cmd_read);
        drop(out_write);
        // Answers and events, NUL-separated.
        let (tx, rx) = std::sync::mpsc::channel();
        let mut out_read = out_read;
        std::thread::Builder::new()
            .name("safety-cdp".into())
            .spawn(move || {
                let mut buf = Vec::new();
                let mut chunk = vec![0u8; 1 << 16];
                while let Ok(n) = out_read.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    while let Some(end) = buf.iter().position(|b| *b == 0) {
                        if let Ok(v) = makepad_strict_json::parse_depth(&buf[..end], 64) {
                            if tx.send(v).is_err() {
                                return;
                            }
                        }
                        buf.drain(..=end);
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        // The log: GPU process deaths.
        let gpu_log = Arc::new(AtomicU32::new(0));
        let gpu_lines = Arc::new(Mutex::new(Vec::new()));
        if let Some(stderr) = child.stderr.take() {
            let (count, lines) = (gpu_log.clone(), gpu_lines.clone());
            std::thread::Builder::new()
                .name("safety-log".into())
                .spawn(move || {
                    use std::io::BufRead;
                    for line in std::io::BufReader::new(stderr).lines().map_while(Result::ok) {
                        if GPU_DEATH.iter().any(|p| line.contains(p)) {
                            count.fetch_add(1, Ordering::Relaxed);
                            if let Ok(mut l) = lines.lock() {
                                if l.len() < 4 {
                                    l.push(line.chars().take(200).collect());
                                }
                            }
                        }
                    }
                })
                .map_err(|e| e.to_string())?;
        }
        Ok(Browser { child, to: cmd_write, from: rx, next_id: 0, events: Vec::new(), gpu_log, gpu_lines })
    }

    #[cfg(not(unix))]
    fn launch(_chrome: &Path, _profile: &Path, _size: (u32, u32)) -> Result<Browser, String> {
        Err("not on this platform".into())
    }

    fn call(&mut self, session: Option<&str>, method: &str, params: Value) -> Result<Value, String> {
        self.call_timeout(session, method, params, Duration::from_secs(30))
    }

    fn call_timeout(&mut self, session: Option<&str>, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        let mut msg = vec![("id", Value::Int(id)), ("method", s(method)), ("params", params)];
        if let Some(sid) = session {
            msg.push(("sessionId", s(sid)));
        }
        let mut bytes = obj(msg).to_json().into_bytes();
        bytes.push(0);
        self.to.write_all(&bytes).map_err(|e| format!("Chrome's pipe ({method}): {e}"))?;
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.from.recv_timeout(left) {
                Ok(v) if v.get("id").and_then(|i| i.as_i64()) == Some(id) => {
                    if let Some(err) = v.get("error") {
                        return Err(format!("{method}: {}", err.get("message").and_then(|m| m.as_str()).unwrap_or("error")));
                    }
                    return Ok(v.get("result").cloned().unwrap_or(Value::Null));
                }
                Ok(v) => {
                    if v.get("method").is_some() {
                        self.events.push(v);
                    }
                }
                Err(RecvTimeoutError::Timeout) => return Err(format!("timeout: {method}")),
                Err(RecvTimeoutError::Disconnected) => return Err(format!("Chrome closed its pipe ({method}): the browser died")),
            }
        }
    }

    /// Events that arrived since.
    fn pump(&mut self) {
        while let Ok(v) = self.from.try_recv() {
            if v.get("method").is_some() {
                self.events.push(v);
            }
        }
    }

    fn exited(&mut self) -> Option<String> {
        self.child.try_wait().ok().flatten().map(|s| s.to_string())
    }

    /// Closes the browser and waits for it (killing it after 5 s).
    fn close(&mut self) {
        let _ = self.call_timeout(None, "Browser.close", obj(vec![]), Duration::from_secs(5));
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The page probe (see the file's header).
const PROBE: &str = include_str!("safety_page.js");

#[cfg(test)]
mod tests {
    use super::*;

    fn pass(gpu_mb: &[u64], textures: &[u64]) -> Vec<PlateUse> {
        gpu_mb.iter().zip(textures).map(|(g, t)| PlateUse { seen: true, gpu_bytes: g << 20, textures: *t, buffers: 100, wasm_bytes: 200 << 20, ..Default::default() }).collect()
    }

    #[test]
    fn a_replay_that_holds_the_same_is_not_a_leak() {
        assert_eq!(replay_growth(&[pass(&[100, 300, 220], &[40, 90, 70]), pass(&[104, 310, 221], &[41, 92, 70])]), None);
        // One pass: nothing to compare.
        assert_eq!(replay_growth(&[pass(&[100], &[40])]), None);
    }

    #[test]
    fn growth_across_a_replay_is_a_leak() {
        let leak = replay_growth(&[pass(&[100, 300, 700], &[40, 90, 170]), pass(&[760, 1000, 1400], &[180, 260, 340])]).unwrap();
        assert!(leak.contains("plate 0") && leak.contains("GPU memory"), "{leak}");
        let leak = replay_growth(&[pass(&[100, 100], &[40, 40]), pass(&[100, 100], &[40, 90])]).unwrap();
        assert!(leak.contains("textures") && leak.contains("plate 1"), "{leak}");
        let mut second = pass(&[100], &[40]);
        second[0].wasm_bytes = 300 << 20;
        assert!(replay_growth(&[pass(&[100], &[40]), second]).unwrap().contains("wasm"));
    }

    #[test]
    fn plates_by_time() {
        let film = Film { dir: Path::new("."), duration: 30.0, plates: vec![0.0, 10.0, 20.0], has_music: false, shaders: None };
        assert_eq!((film.plate_at(0.0), film.plate_at(9.9), film.plate_at(10.0), film.plate_at(29.0)), (0, 0, 1, 2));
    }
}
