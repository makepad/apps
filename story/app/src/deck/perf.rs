//! Opt-in native instrumentation: MAKEPAD_DECK_PERF=1, including hidden runs.
use makepad_widgets::*;
use std::collections::HashMap;

#[derive(Default)]
pub struct Perf {
    enabled: Option<bool>,
    serials: HashMap<DrawPassId, u64>,
    since: f64,
    size: Vec2d,
    resized: f64,
    frames: u64,
    passes: u64,
    cpu_ms: f64,
    cpu_max: f64,
    gpu_ms: f64,
    gpu_samples: u64,
    samples: Vec<(u64, f64)>,
    events: [u64; 5],
    /// The UI thread's CPU time at the window's start: everything it did
    /// (events, pumps, every page's handling, the draw, the submit).
    thread_since: f64,
}

/// CPU seconds the calling thread has used (0 where unsupported).
fn thread_cpu() -> f64 {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        #[repr(C)]
        struct Timespec { sec: i64, nsec: i64 }
        extern "C" { fn clock_gettime(clock: i32, out: *mut Timespec) -> i32; }
        const THREAD_CPU: i32 = if cfg!(target_os = "macos") { 16 } else { 3 };
        let mut t = Timespec { sec: 0, nsec: 0 };
        // SAFETY: clock_gettime writes one timespec through a valid pointer.
        if unsafe { clock_gettime(THREAD_CPU, &mut t) } == 0 { return t.sec as f64 + t.nsec as f64 * 1e-9; }
    }
    0.0
}

impl Perf {
    pub fn event(&mut self, event: &Event) {
        if self.enabled != Some(true) { return; }
        let slot = match event { Event::NextFrame(_) => 0, Event::Signal => 1, Event::Timer(_) => 2, Event::Draw(_) => 3, _ => 4 };
        self.events[slot] += 1;
    }

    pub fn begin(&mut self) -> Option<f64> {
        (*self.enabled.get_or_insert_with(|| std::env::var("MAKEPAD_DECK_PERF").is_ok_and(|v| v == "1"))).then(Cx::time_now)
    }

    pub fn end(&mut self, cx: &mut Cx, start: Option<f64>, size: Vec2d, idle: bool) {
        let Some(start) = start else { return };
        let now = Cx::time_now();
        if self.since == 0.0 { self.since = now; self.thread_since = thread_cpu(); }
        if self.size != size { self.resized = now; self.size = size; }
        let cpu = (now - start) * 1000.0;
        self.frames += 1;
        self.cpu_ms += cpu;
        self.cpu_max = self.cpu_max.max(cpu);
        let ids: Vec<_> = cx.passes.id_iter().collect();
        for id in ids {
            let pass = &mut cx.passes[id];
            pass.set_gpu_timing_enabled(true);
            if let Some(previous) = self.serials.insert(id, pass.painted_serial) {
                self.passes += u64::from(pass.painted_serial > previous);
            }
            self.samples.clear();
            pass.drain_gpu_time_samples(&mut self.samples);
            self.gpu_samples += self.samples.len() as u64;
            self.gpu_ms += self.samples.iter().map(|(_, ms)| ms).sum::<f64>();
        }
        if now - self.since >= 1.0 {
            let mode = if now - self.resized < 0.4 { "splitter/resize" } else if idle { "idle" } else { "animation" };
            let thread = thread_cpu();
            let thread_ms = (thread - self.thread_since) * 1000.0 / self.frames as f64;
            self.thread_since = thread;
            log!("deck perf: mode={mode} frames={} thread_ms/frame={thread_ms:.3} passes/frame={:.2} cpu_ms/frame={:.3} cpu_max_ms={:.3} gpu_ms/frame={:.3} gpu_samples={} events(next_frame,signal,timer,draw,other)={:?}", self.frames, self.passes as f64 / self.frames as f64, self.cpu_ms / self.frames as f64, self.cpu_max, self.gpu_ms / self.frames as f64, self.gpu_samples, self.events);
            self.events = [0; 5];
            self.since = now; self.frames = 0; self.passes = 0;
            self.cpu_ms = 0.0; self.cpu_max = 0.0; self.gpu_ms = 0.0; self.gpu_samples = 0;
        }
    }
}

/// The open timeline (MAKEPAD_DECK_PERF=1): from the double-click to the
/// first picture, each step's wall time, on whichever thread it runs; the
/// whole deck's arrival after the first picture is logged against the same start.
struct Open { start: std::time::Instant, last: std::time::Instant, ended: bool, installed: bool }

static OPEN: std::sync::Mutex<Option<Open>> = std::sync::Mutex::new(None);

fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("MAKEPAD_DECK_PERF").is_ok_and(|v| v == "1"))
}

fn mark(what: &str, late: bool) {
    let mut open = OPEN.lock().unwrap();
    let Some(open) = open.as_mut().filter(|open| late || !open.ended) else { return };
    let now = std::time::Instant::now();
    log!("deck open: +{:.1} ms {what} (step {:.1} ms)", (now - open.start).as_secs_f64() * 1000.0, (now - open.last).as_secs_f64() * 1000.0);
    open.last = now;
}

/// A second click of a double-click is a step of the same open.
pub(crate) fn open_begin(what: &str) {
    if !enabled() { return; }
    if opening() { return open_mark(what); }
    let now = std::time::Instant::now();
    *OPEN.lock().unwrap() = Some(Open { start: now, last: now, ended: false, installed: false });
    log!("deck open: +0.0 ms {what}");
}

pub(crate) fn open_mark(what: &str) {
    if enabled() { mark(what, false); }
}

/// After the first picture: the rest of the deck arriving.
pub(crate) fn open_late(what: &str) {
    if enabled() { mark(what, true); }
}

/// The new deck is installed in the page: its first picture ends the open.
pub(crate) fn open_installed(title: &str, slides: usize, pending: bool) {
    if !enabled() { return; }
    if let Some(open) = OPEN.lock().unwrap().as_mut() { open.installed = true; }
    open_mark(&format!("open_library done: “{title}”, {slides} slides{}", if pending { ", later graphics pending" } else { "" }));
}

pub(crate) fn open_end(what: &str) {
    if !enabled() || !OPEN.lock().unwrap().as_ref().is_some_and(|open| open.installed && !open.ended) { return; }
    open_mark(what);
    if let Some(open) = OPEN.lock().unwrap().as_mut() { open.ended = true; }
}

pub(crate) fn opening() -> bool {
    enabled() && OPEN.lock().unwrap().as_ref().is_some_and(|open| !open.ended)
}
