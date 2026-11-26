//! One implementation of the call semantics, as a
//! poll-driven state machine over a [`Driver`].
//!
//! `runner.begin(call)` queues a [`Call`]; `runner.poll(driver, now)` runs
//! it as far as it can. Phases (the pointer moving, a press held, typing at
//! cps, a wait for a predicate with a deadline) are state in the runner, not
//! stack, so a call can be resumed across any number of polls.
//!
//! Two clocks, chosen at construction:
//! - [`Clock::Virtual`] (AppRun): time is the app's frame counter. The
//!   runner itself asks the driver for frames (`advance_frames`, in chunks
//!   that diff the layout) whenever a call has to let time pass, so one
//!   `poll` runs a call to completion and returns `Done` or `Failed`, never
//!   `Pending`. Frame stamps, marks, the focus track and the recording are a
//!   pure function of the calls and the frame rate.
//! - [`Clock::Real`] (DECK): the caller passes the wall time as `now`
//!   (seconds, any origin, once per child frame). The runner's frame counter
//!   is `floor((now - first now) * fps)`; a call that has to let time pass
//!   returns `Pending` and is resumed by the next poll. Nothing blocks or
//!   sleeps, and `advance_frames` is never called.

use crate::call::{Call, SelWant, StateWant, Target};
use crate::doc::RunOutput;
use crate::driver::{Driver, Mods, Mouse, Row};
use crate::plan::{control_key, frame_at, frames_for, move_path, same_aspect, spread};
use crate::track::{layout_change, CallRecord, Failure, FocusSource, Input, Press, Rect, SnapWidget, Track};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// What the app's `settled` names that its workers finish in real time, not on its frames: its
/// pool's jobs, asynchronous loads and shader compiles. On the virtual clock a settle wait steps
/// no frame for them, so the frame their result lands on is the same at any machine load.
const REAL_TIME_WORK: [&str; 3] = ["pool", "loads", "shaders"];
/// How long a virtual-clock settle wait waits for that work alone before it steps frames while it
/// runs, as before, and says the run's frames may differ (the platform's own bound for a step's
/// loads).
const REAL_TIME_WAIT: Duration = Duration::from_secs(30);
/// A pointer press lasts this long (at least one frame).
const PRESS_TIME: f64 = 0.05;
/// Key down and up are 3 frames apart at 60 fps: this long, and at
/// least one frame at any rate.
const KEY_TIME: f64 = 3.0 / 60.0;
/// After a release, drop or key up the app gets this long (one frame at
/// 60 fps) before the next call.
const SETTLE_TIME: f64 = 1.0 / 60.0;
/// The platform's multi-click window (`platform/src/event/finger.rs`
/// TAP_COUNT_TIME / TAP_COUNT_DISTANCE, seconds and points).
const TAP_COUNT_TIME: f64 = 0.5;
const TAP_COUNT_DISTANCE: f64 = 5.0;
/// How often a layout change is looked for while frames are stepped.
const LAYOUT_CADENCE: f64 = 0.1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clock {
    Virtual,
    Real,
}

/// The result of one `poll`.
#[derive(Clone, Debug, PartialEq)]
pub enum Poll {
    /// The call needs more time (or the app needs to catch up): poll again
    /// with a later `now`.
    Pending,
    /// The call finished (also when no call is running).
    Done,
    /// The call failed; the message is also in `track.failures`.
    Failed(String),
}

/// Bounds of one run: a cancel flag checked between chunks of frames, and
/// the most clock frames it may take.
#[derive(Clone, Debug)]
pub struct RunLimits {
    pub cancel: Arc<AtomicBool>,
    pub max_frames: u64,
}

impl Default for RunLimits {
    fn default() -> RunLimits {
        RunLimits { cancel: Arc::new(AtomicBool::new(false)), max_frames: u64::MAX }
    }
}

/// A resolved target: its point in window points and rect in pixels.
struct Resolved {
    point: (f64, f64),
    rect: Rect,
    row: Option<Row>,
}

/// The pointer moving: one position per frame.
struct Move {
    path: Vec<(f64, f64)>,
    i: usize,
    start: u64,
    frames: u64,
    px: Vec<(f64, f64)>,
}

/// The wait that keeps separate calls from counting as one multi-click.
#[derive(Default)]
struct Gap {
    planned: bool,
    until: Option<u64>,
    from: u64,
}

#[derive(Clone, Copy, PartialEq)]
enum ClickAt {
    Resolve,
    Moving,
    Arrive,
    Gap,
    Press,
    Pressed,
    Released,
}

struct ClickM {
    target: Target,
    button: u8,
    count: u64,
    move_time: f64,
    from: u64,
    at: ClickAt,
    first: Option<Resolved>,
    mv: Option<Move>,
    path: Vec<(f64, f64)>,
    arrived: Option<Resolved>,
    gap: Gap,
    i: u64,
    until: u64,
}

impl ClickM {
    fn new(target: Target, button: u8, count: u64, move_time: f64, from: u64) -> ClickM {
        ClickM { target, button, count, move_time, from, at: ClickAt::Resolve, first: None, mv: None, path: Vec::new(), arrived: None, gap: Gap::default(), i: 0, until: 0 }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum DragAt {
    ResolveA,
    MovingA,
    Gap,
    Pressed,
    ResolveB,
    MovingB,
    Released,
}

struct DragM {
    a: Target,
    b: Target,
    time: f64,
    move_time: f64,
    from: u64,
    at: DragAt,
    start: Option<Resolved>,
    end: Option<Resolved>,
    mv: Option<Move>,
    path: Vec<(f64, f64)>,
    gap: Gap,
    until: u64,
}

#[derive(Clone, Copy, PartialEq)]
enum ScrollAt {
    Resolve,
    Moving,
    Wheel,
}

struct ScrollM {
    target: Target,
    dx: f64,
    dy: f64,
    time: f64,
    move_time: f64,
    from: u64,
    at: ScrollAt,
    resolved: Option<Resolved>,
    mv: Option<Move>,
    path: Vec<(f64, f64)>,
    wheel: Option<Wheel>,
}

struct Wheel {
    steps: Vec<(f64, f64)>,
    i: usize,
    start: u64,
    frames: u64,
}

#[derive(Clone, Copy, PartialEq)]
enum TypeAt {
    Click,
    Char,
    KeyHeld,
    Advance,
    Follow,
}

struct TypeM {
    target: Target,
    text: Vec<char>,
    cps: f64,
    click: ClickM,
    at: TypeAt,
    i: usize,
    rect: Rect,
    rect_from: u64,
    before: u64,
    until: u64,
    key: &'static str,
}

#[derive(Clone, Copy, PartialEq)]
enum DropAt {
    Resolve,
    Moving,
    Settled,
}

struct DropM {
    target: Target,
    file: PathBuf,
    move_time: f64,
    from: u64,
    at: DropAt,
    resolved: Option<Resolved>,
    mv: Option<Move>,
    path: Vec<(f64, f64)>,
    until: u64,
}

struct KeyM {
    code: String,
    mods: Mods,
    held: bool,
    frames: u64,
    until: u64,
}

struct WaitM {
    start: u64,
    last_text: Option<String>,
    last_row: Option<Row>,
}

enum Stage {
    Fresh,
    Click(ClickM),
    Drag(DragM),
    Scroll(ScrollM),
    Type(TypeM),
    Key(KeyM),
    Drop(DropM),
    /// Resize's window and the frame it settles on, or hold's end.
    Rest { until: u64, resize: Option<(f64, f64)> },
    Settled(u64),
    Wait(WaitM),
    Focus,
    Finished,
}

struct Active {
    call: Call,
    started: bool,
    from: u64,
    waited_before: u64,
    stage: Stage,
}

pub struct Runner {
    pub output: RunOutput,
    pub track: Track,
    mode: Clock,
    real_start: Option<f64>,
    /// The run's time in seconds as the calls' durations add up. Frames are
    /// stepped to `frame_at(time)`, so every call boundary is within a
    /// frame of the same time at any frame rate, and a 5 fps validation run
    /// puts its marks where the full run does. (Real clock: reset to the
    /// clock at the start of every call.)
    time: f64,
    /// The pointer, in window points (starts at the window's centre).
    cursor: (f64, f64),
    pub failed: bool,
    /// Every target the script named, with its call (dry runs collect them).
    pub targets: Vec<(String, Target)>,
    /// Pixel offset of the window inside the recording (after a resize to
    /// a smaller window the capture centres it).
    offset: (f64, f64),
    window: (f64, f64),
    /// The last listing (window points) and the clock frame it was taken at.
    layout: Vec<SnapWidget>,
    layout_clock: u64,
    /// Clock frame and point of the last pointer press, to keep separate
    /// calls from counting as one multi-click.
    last_press: Option<(u64, (f64, f64))>,
    limits: RunLimits,
    drop_root: PathBuf,
    /// The SHA-256 of the file the current call dropped, for its record.
    pending_file: Option<String>,
    /// The text the current `type` call typed, for its record.
    pending_text: Option<String>,
    /// Frames spent waiting (on the app in `wait_*`, on the multi-click
    /// window before a press) rather than on the script's schedule.
    waited: u64,
    /// The capture's result, filled in by the executor: hash, per-frame
    /// hashes, frames written, thumbnail.
    pub finished: (String, Vec<String>, u64, Option<Vec<u8>>),
    /// A target step first waits up to this many seconds for its widget to
    /// be there, visible and enabled (None: it must be there at once).
    target_wait: Option<f64>,
    /// What the run's review should know (a settle wait that stepped frames while the app's
    /// workers ran: [`REAL_TIME_WAIT`]).
    pub warnings: Vec<String>,
    sync_deadline: Option<u64>,
    /// The clock frame a wait last looked at the app on (real clock: once
    /// per frame).
    eval_clock: Option<u64>,
    active: Option<Active>,
}

impl Runner {
    pub fn new(output: RunOutput, mode: Clock, limits: RunLimits, drop_root: PathBuf) -> Runner {
        let window = output.logical();
        Runner {
            output,
            track: Track::new(),
            mode,
            real_start: None,
            time: 0.0,
            cursor: (window.0 * 0.5, window.1 * 0.5),
            failed: false,
            targets: Vec::new(),
            offset: (0.0, 0.0),
            window,
            layout: Vec::new(),
            layout_clock: 0,
            last_press: None,
            limits,
            drop_root,
            pending_file: None,
            pending_text: None,
            waited: 0,
            finished: Default::default(),
            target_wait: None,
            warnings: Vec::new(),
            sync_deadline: None,
            eval_clock: None,
            active: None,
        }
    }

    /// Target steps wait up to `max` seconds for their widget to exist, be
    /// visible and enabled (load time is absorbed instead of failing).
    pub fn with_target_wait(mut self, max: f64) -> Runner {
        self.target_wait = Some(max);
        self
    }

    /// Where the pointer starts (and the synthetic cursor is shown).
    pub fn cursor(&self) -> (f64, f64) {
        self.cursor
    }

    pub fn running(&self) -> bool {
        self.active.is_some()
    }

    pub fn fail(&mut self, call: &str, message: String) {
        if !self.failed {
            self.failed = true;
            self.track.failures.push(Failure { call: call.to_string(), frame: self.track.clock, message });
        }
    }

    /// Queue `call`. Its targets are collected now; its argument checks that
    /// need the output (resize) and everything that acts happen in `poll`.
    pub fn begin(&mut self, call: Call) {
        for t in call.targets() {
            self.targets.push((call.name().to_string(), t));
        }
        self.active = Some(Active { call, started: false, from: 0, waited_before: 0, stage: Stage::Fresh });
        self.pending_file = None;
        self.pending_text = None;
        self.eval_clock = None;
        self.sync_deadline = None;
    }

    /// A dry run (no app): collect the targets and check what can be
    /// checked without one. Calls that only touch the track (mark, cut,
    /// resume, focus_rect, wide) take effect.
    pub fn dry(&mut self, call: Call) -> Result<(), String> {
        for t in call.targets() {
            self.targets.push((call.name().to_string(), t));
        }
        self.precheck(&call)?;
        self.instant(None, &call).unwrap_or(Ok(()))
    }

    fn precheck(&self, call: &Call) -> Result<(), String> {
        if let Call::Resize { width, height } = call {
            if !same_aspect(*width, *height, self.output.width, self.output.height) {
                return Err(format!("resize to {width}x{height} refused: the output is {}x{} px; keep its aspect", self.output.width, self.output.height));
            }
        }
        Ok(())
    }

    /// Run the queued call as far as it goes. `now` is the wall time in
    /// seconds (ignored on the virtual clock).
    pub fn poll(&mut self, d: &mut dyn Driver, now: f64) -> Poll {
        let Some(mut a) = self.active.take() else { return Poll::Done };
        let name = a.call.name();
        match self.poll_active(d, now, &mut a) {
            Ok(false) => {
                self.active = Some(a);
                Poll::Pending
            }
            Ok(true) => Poll::Done,
            Err(e) => {
                self.pending_file = None;
                self.pending_text = None;
                self.fail(name, e.clone());
                Poll::Failed(e)
            }
        }
    }

    fn poll_active(&mut self, d: &mut dyn Driver, now: f64, a: &mut Active) -> Result<bool, String> {
        if self.limits.cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        if self.mode == Clock::Real {
            self.sync_real(now);
        }
        if !a.started {
            a.started = true;
            a.from = self.track.clock;
            a.waited_before = self.waited;
            if self.mode == Clock::Real {
                self.time = self.track.clock as f64 / self.output.fps as f64;
            }
            self.precheck(&a.call)?;
        }
        if !self.drive(d, a)? {
            return Ok(false);
        }
        if self.mode == Clock::Virtual && diffs_layout(&a.call) {
            self.layout_diff(d)?;
        }
        let call = &a.call;
        let to = self.track.clock;
        let coordinates = call.targets().iter().any(|t| matches!(t, Target::Point(..)));
        let target = call.record_target().map(|t| match t {
            Target::Path(p) => p,
            t => t.describe(),
        });
        let record = CallRecord {
            call: call.name().to_string(),
            target,
            from: a.from,
            to,
            coordinates,
            file_sha256: self.pending_file.take(),
            waited: self.waited - a.waited_before,
            text: self.pending_text.take(),
        };
        self.track.calls.push(record);
        Ok(true)
    }

    // ---- the clock ----

    fn fps(&self) -> f64 {
        self.output.fps as f64
    }

    fn sync_real(&mut self, now: f64) {
        let start = *self.real_start.get_or_insert(now);
        let frame = ((now - start).max(0.0) * self.fps()).floor() as u64;
        if frame > self.track.clock {
            self.track.advance(frame - self.track.clock);
        }
    }

    /// Step `frames` frames in chunks of at most `LAYOUT_CADENCE`: between
    /// chunks the cancel flag and the length bound are checked, and the
    /// layout is diffed so a change is stamped when it happens. (Virtual.)
    fn step(&mut self, d: &mut dyn Driver, frames: u64) -> Result<(), String> {
        let cadence = ((LAYOUT_CADENCE * self.fps()).ceil() as u64).max(1);
        let mut left = frames;
        while left > 0 {
            if self.limits.cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let chunk = left.min(cadence - (self.track.clock - self.layout_clock).min(cadence - 1));
            if self.track.clock + chunk > self.limits.max_frames {
                return Err(format!(
                    "the run is longer than its max_duration ({} s); raise it in the run file",
                    self.limits.max_frames as f64 / self.fps()
                ));
            }
            d.advance_frames(chunk)?;
            self.track.advance(chunk);
            left -= chunk;
            if self.track.clock - self.layout_clock >= cadence {
                self.layout_diff(d)?;
            }
        }
        Ok(())
    }

    /// Let the clock reach frame `target`: the virtual clock steps there; the
    /// real clock is there when time has passed (false: poll again later).
    fn reach(&mut self, d: &mut dyn Driver, target: u64) -> Result<bool, String> {
        match self.mode {
            Clock::Virtual => {
                self.step(d, target.saturating_sub(self.track.clock))?;
                Ok(true)
            }
            Clock::Real => Ok(self.track.clock >= target),
        }
    }

    /// The frame `seconds` later on the schedule, at least `min_frames`
    /// ahead (a press must show). A frame taken early is given back by the
    /// next advance.
    fn advance_target(&mut self, seconds: f64, min_frames: u64) -> u64 {
        self.time += seconds;
        let frames = frame_at(self.time, self.output.fps).saturating_sub(self.track.clock).max(min_frames);
        self.track.clock + frames
    }

    /// After a wait that stepped frame by frame, the time is the clock's.
    fn sync_time(&mut self) {
        self.time = self.time.max(self.track.clock as f64 / self.fps());
    }

    /// In a wait loop: whether the app is to be looked at now (the real
    /// clock looks once per frame).
    fn fresh(&self) -> bool {
        self.mode == Clock::Virtual || self.eval_clock != Some(self.track.clock)
    }

    /// In a wait loop that did not find what it waits for: let a frame
    /// pass. True: it has, look again now.
    fn next_frame(&mut self, d: &mut dyn Driver) -> Result<bool, String> {
        match self.mode {
            Clock::Virtual => {
                self.step(d, 1)?;
                Ok(true)
            }
            Clock::Real => {
                self.eval_clock = Some(self.track.clock);
                Ok(false)
            }
        }
    }

    fn after(&self, seconds: f64) -> u64 {
        self.track.clock + frames_for(seconds, self.output.fps)
    }

    // ---- geometry and the track ----

    fn px(&self, r: [f64; 4]) -> Rect {
        let s = self.output.dpi_scale;
        Rect::new(self.offset.0 + r[0] * s, self.offset.1 + r[1] * s, r[2] * s, r[3] * s)
    }

    fn px_point(&self, p: (f64, f64)) -> (f64, f64) {
        (self.offset.0 + p.0 * self.output.dpi_scale, self.offset.1 + p.1 * self.output.dpi_scale)
    }

    /// The pointer moved to `p` (window points): the app has it from now.
    fn pointer_to(&mut self, p: (f64, f64)) {
        self.cursor = p;
        let at = self.px_point(p);
        self.track.pointer(self.track.clock, at);
    }

    fn layout_now(&self, d: &mut dyn Driver) -> Result<Vec<SnapWidget>, String> {
        let rows: Vec<Row> = d.snapshot()?.into_iter().filter(|row| row.path.as_deref() != Some("") && row.ty != "Window").collect();
        Ok(layout_keys(rows))
    }

    pub fn snapshot_layout(&mut self, d: &mut dyn Driver) -> Result<(), String> {
        self.layout = self.layout_now(d)?;
        self.layout_clock = self.track.clock;
        let start = self.px_point(self.cursor);
        self.track.pointer(self.track.clock, start);
        self.record_listing();
        Ok(())
    }

    /// The current listing into the track, in source pixels (for bound
    /// overlays: widget rects over time).
    fn record_listing(&mut self) {
        let px: Vec<SnapWidget> = self
            .layout
            .iter()
            .map(|w| SnapWidget { key: w.key.clone(), rect: self.px([w.rect.x, w.rect.y, w.rect.w, w.rect.h]) })
            .collect();
        self.track.listing(self.track.clock, &px);
    }

    /// Record a layout change since the last listing as a focus rect, from
    /// the frame it is first seen.
    fn layout_diff(&mut self, d: &mut dyn Driver) -> Result<(), String> {
        let now = self.layout_now(d)?;
        if let Some(region) = layout_change(&self.layout, &now) {
            let rect = self.px([region.x, region.y, region.w, region.h]);
            let (from, to) = (self.track.clock, self.after(1.0));
            self.track.focus(FocusSource::Layout, rect, 0.6, from, to);
        }
        self.layout = now;
        self.layout_clock = self.track.clock;
        self.record_listing();
        Ok(())
    }

    // ---- finding widgets ----

    /// The one visible widget a path names; zero or several fail and name
    /// the candidates (several here, zero as `None`).
    fn find_one(&self, d: &mut dyn Driver, path: &str) -> Result<Option<Row>, String> {
        let mut rows = d.find(path)?;
        match rows.len() {
            0 => Ok(None),
            1 => Ok(rows.pop()),
            n => Err(format!(
                "@{path} matches {n} widgets: {}; name one by a longer path",
                rows.iter().map(|r| format!("@{}", r.name())).collect::<Vec<_>>().join(", ")
            )),
        }
    }

    fn missing(&self, d: &mut dyn Driver, path: &str) -> String {
        let rows = d.snapshot().unwrap_or_default();
        if !rows.is_empty() && rows.iter().all(|r| r.path.is_none()) {
            return format!("@{path}: {}", crate::driver::NO_PATHS);
        }
        let paths: Vec<String> = rows.into_iter().filter_map(|r| r.path).filter(|p| !p.is_empty()).collect();
        let near = near_paths(path, &paths);
        if near.is_empty() {
            format!("@{path} matches no visible widget (see `makepad-app-run --dump` for the paths)")
        } else {
            format!("@{path} matches no visible widget; candidates: {}", near.iter().map(|p| format!("@{p}")).collect::<Vec<_>>().join(", "))
        }
    }

    /// Resolve a target to a point and rect. `wait`: with a target wait set,
    /// give the widget time to be there, visible and enabled (None: not yet,
    /// poll again).
    fn resolve(&mut self, d: &mut dyn Driver, target: &Target, wait: bool) -> Result<Option<Resolved>, String> {
        let path = match target {
            Target::Point(x, y) => {
                let p = self.px_point((*x, *y));
                return Ok(Some(Resolved { point: (*x, *y), rect: Rect::new(p.0, p.1, 0.0, 0.0), row: None }));
            }
            Target::Path(path) => path,
        };
        let Some(max) = self.target_wait.filter(|_| wait) else {
            return match self.find_one(d, path)? {
                Some(row) => Ok(Some(Resolved { point: row.center(), rect: self.px(row.rect), row: Some(row) })),
                None => Err(self.missing(d, path)),
            };
        };
        loop {
            if !self.fresh() || !d.ready() {
                return Ok(None);
            }
            let found = self.find_one(d, path)?;
            if let Some(row) = &found {
                if row.visible && row.rect[2] > 0.0 && row.rect[3] > 0.0 && row.enabled != Some(false) {
                    self.sync_deadline = None;
                    let row = found.unwrap();
                    return Ok(Some(Resolved { point: row.center(), rect: self.px(row.rect), row: Some(row) }));
                }
            }
            let deadline = *self.sync_deadline.get_or_insert(self.track.clock + frames_for(max, self.output.fps));
            if self.track.clock >= deadline {
                self.sync_deadline = None;
                return Err(match found {
                    Some(row) => format!("@{path} was not visible and enabled within {max} s (it is at {:?}, visible {}, enabled {:?})", row.rect, row.visible, row.enabled),
                    None => format!("{} (waited {max} s)", self.missing(d, path)),
                });
            }
            if !self.next_frame(d)? {
                return Ok(None);
            }
        }
    }

    // ---- sub-machines ----

    fn move_begin(&mut self, to: (f64, f64), move_time: f64) -> Move {
        self.time += move_time;
        let frames = frame_at(self.time, self.output.fps).saturating_sub(self.track.clock);
        Move { path: move_path(self.cursor, to, frames), i: 0, start: self.track.clock, frames, px: vec![self.px_point(self.cursor)] }
    }

    /// One position per frame; a jump when that is no frame.
    fn move_poll(&mut self, d: &mut dyn Driver, m: &mut Move) -> Result<bool, String> {
        while m.i < m.path.len() {
            if m.frames > 0 && !self.reach(d, m.start + m.i as u64)? {
                return Ok(false);
            }
            let p = m.path[m.i];
            d.mouse(Mouse::Move, p.0, p.1)?;
            self.pointer_to(p);
            m.px.push(self.px_point(p));
            m.i += 1;
        }
        if m.frames > 0 && !self.reach(d, m.start + m.frames)? {
            return Ok(false);
        }
        Ok(true)
    }

    /// Before a press at `at`: if the last press was within the platform's
    /// multi-click distance and time, wait until it is not, so separate
    /// calls never become a double click (`{count}` is how to ask for one).
    /// The wait is on the schedule (to 0.5 s + 1/60 s after that press), so
    /// it lasts the same at 5 fps and at 60.
    fn gap_poll(&mut self, d: &mut dyn Driver, g: &mut Gap, at: (f64, f64)) -> Result<bool, String> {
        if !g.planned {
            g.planned = true;
            g.from = self.track.clock;
            if let Some((clock, point)) = self.last_press {
                let near = ((at.0 - point.0).powi(2) + (at.1 - point.1).powi(2)).sqrt() < TAP_COUNT_DISTANCE;
                let fps = self.fps();
                let clear = clock as f64 / fps + TAP_COUNT_TIME + 1.0 / 60.0;
                if near && self.time < clear {
                    self.time = clear;
                    // Never less than the platform's window, whatever the rounding.
                    g.until = Some(frame_at(clear, self.output.fps).max(clock + (TAP_COUNT_TIME * fps).ceil() as u64));
                }
            }
        }
        if let Some(until) = g.until {
            if !self.reach(d, until)? {
                return Ok(false);
            }
            self.waited += self.track.clock - g.from;
        }
        Ok(true)
    }

    fn press(&mut self, d: &mut dyn Driver, at: (f64, f64), button: u8) -> Result<(), String> {
        d.mouse(Mouse::Down(button), at.0, at.1)?;
        self.last_press = Some((self.track.clock, at));
        let px = self.px_point(at);
        self.track.presses.push(Press { down: self.track.clock, up: None, at: px, button });
        Ok(())
    }

    fn release(&mut self, d: &mut dyn Driver, at: (f64, f64), button: u8) -> Result<(), String> {
        d.mouse(Mouse::Up(button), at.0, at.1)?;
        let clock = self.track.clock;
        if let Some(press) = self.track.presses.iter_mut().rev().find(|p| p.up.is_none()) {
            press.up = Some(clock);
        }
        Ok(())
    }

    /// Key down to key up: 3 frames at 60 fps, the same time at other
    /// rates (at least one frame).
    fn key_frames(&self) -> u64 {
        frames_for(KEY_TIME, self.output.fps).max(1)
    }

    fn click_poll(&mut self, d: &mut dyn Driver, m: &mut ClickM) -> Result<bool, String> {
        loop {
            match m.at {
                ClickAt::Resolve => {
                    let Some(first) = self.resolve(d, &m.target, true)? else { return Ok(false) };
                    m.mv = Some(self.move_begin(first.point, m.move_time));
                    m.first = Some(first);
                    m.at = ClickAt::Moving;
                }
                ClickAt::Moving => {
                    let mv = m.mv.as_mut().unwrap();
                    if !self.move_poll(d, mv)? {
                        return Ok(false);
                    }
                    m.path = m.mv.take().unwrap().px;
                    m.at = ClickAt::Arrive;
                }
                ClickAt::Arrive => {
                    // Re-read the target where the pointer arrived: hovering can move it.
                    let first = m.first.take().unwrap();
                    let at = match &first.row {
                        Some(_) => self.resolve(d, &m.target, false)?.unwrap(),
                        None => first,
                    };
                    let (x, y) = at.point;
                    if (x, y) != self.cursor {
                        d.mouse(Mouse::Move, x, y)?;
                        self.pointer_to((x, y));
                    }
                    m.arrived = Some(at);
                    m.at = ClickAt::Gap;
                }
                ClickAt::Gap => {
                    let at = m.arrived.as_ref().unwrap().point;
                    if !self.gap_poll(d, &mut m.gap, at)? {
                        return Ok(false);
                    }
                    m.at = ClickAt::Press;
                }
                ClickAt::Press => {
                    let at = m.arrived.as_ref().unwrap().point;
                    self.press(d, at, m.button)?;
                    m.until = self.advance_target(PRESS_TIME, 1);
                    m.at = ClickAt::Pressed;
                }
                ClickAt::Pressed => {
                    if !self.reach(d, m.until)? {
                        return Ok(false);
                    }
                    let at = m.arrived.as_ref().unwrap().point;
                    self.release(d, at, m.button)?;
                    m.until = if m.i + 1 == m.count { self.advance_target(SETTLE_TIME, 0) } else { self.advance_target(PRESS_TIME, 1) };
                    m.at = ClickAt::Released;
                }
                ClickAt::Released => {
                    if !self.reach(d, m.until)? {
                        return Ok(false);
                    }
                    m.i += 1;
                    if m.i < m.count {
                        m.at = ClickAt::Press;
                        continue;
                    }
                    let arrived = m.arrived.take().unwrap();
                    let rect = Rect::around(&m.path).map_or(arrived.rect, |p| p.union(arrived.rect));
                    let to = self.after(0.5);
                    self.track.focus(FocusSource::Click, rect, 1.0, m.from, to);
                    return Ok(true);
                }
            }
        }
    }

    fn drag_poll(&mut self, d: &mut dyn Driver, m: &mut DragM) -> Result<bool, String> {
        loop {
            match m.at {
                DragAt::ResolveA => {
                    let Some(start) = self.resolve(d, &m.a, true)? else { return Ok(false) };
                    m.mv = Some(self.move_begin(start.point, m.move_time));
                    m.start = Some(start);
                    m.at = DragAt::MovingA;
                }
                DragAt::MovingA => {
                    if !self.move_poll(d, m.mv.as_mut().unwrap())? {
                        return Ok(false);
                    }
                    m.path = m.mv.take().unwrap().px;
                    m.at = DragAt::Gap;
                }
                DragAt::Gap => {
                    let at = self.cursor;
                    if !self.gap_poll(d, &mut m.gap, at)? {
                        return Ok(false);
                    }
                    self.press(d, at, 0)?;
                    m.until = self.advance_target(PRESS_TIME, 1);
                    m.at = DragAt::Pressed;
                }
                DragAt::Pressed => {
                    if !self.reach(d, m.until)? {
                        return Ok(false);
                    }
                    m.at = DragAt::ResolveB;
                }
                DragAt::ResolveB => {
                    let end = self.resolve(d, &m.b, false)?.unwrap();
                    m.mv = Some(self.move_begin(end.point, m.time));
                    m.end = Some(end);
                    m.at = DragAt::MovingB;
                }
                DragAt::MovingB => {
                    if !self.move_poll(d, m.mv.as_mut().unwrap())? {
                        return Ok(false);
                    }
                    let px = m.mv.take().unwrap().px;
                    m.path.extend(px);
                    let at = self.cursor;
                    self.release(d, at, 0)?;
                    m.until = self.advance_target(SETTLE_TIME, 0);
                    m.at = DragAt::Released;
                }
                DragAt::Released => {
                    if !self.reach(d, m.until)? {
                        return Ok(false);
                    }
                    let rect = Rect::around(&m.path).unwrap().union(m.start.as_ref().unwrap().rect).union(m.end.as_ref().unwrap().rect);
                    let to = self.after(0.5);
                    self.track.focus(FocusSource::Drag, rect, 1.0, m.from, to);
                    return Ok(true);
                }
            }
        }
    }

    fn scroll_poll(&mut self, d: &mut dyn Driver, m: &mut ScrollM) -> Result<bool, String> {
        loop {
            match m.at {
                ScrollAt::Resolve => {
                    let Some(at) = self.resolve(d, &m.target, true)? else { return Ok(false) };
                    m.mv = Some(self.move_begin(at.point, m.move_time));
                    m.resolved = Some(at);
                    m.at = ScrollAt::Moving;
                }
                ScrollAt::Moving => {
                    if !self.move_poll(d, m.mv.as_mut().unwrap())? {
                        return Ok(false);
                    }
                    m.path = m.mv.take().unwrap().px;
                    self.time += m.time;
                    let frames = frame_at(self.time, self.output.fps).saturating_sub(self.track.clock);
                    let parts = frames.max(1);
                    let steps = spread(m.dx, parts).into_iter().zip(spread(m.dy, parts)).collect();
                    m.wheel = Some(Wheel { steps, i: 0, start: self.track.clock, frames });
                    m.at = ScrollAt::Wheel;
                }
                ScrollAt::Wheel => {
                    let (x, y) = self.cursor;
                    let w = m.wheel.as_mut().unwrap();
                    while w.i < w.steps.len() {
                        if w.frames > 0 && !self.reach(d, w.start + w.i as u64)? {
                            return Ok(false);
                        }
                        let (dx, dy) = w.steps[w.i];
                        w.i += 1;
                        d.mouse(Mouse::Scroll(dx, dy), x, y)?;
                    }
                    let (start, frames) = (w.start, w.frames);
                    if frames > 0 && !self.reach(d, start + frames)? {
                        return Ok(false);
                    }
                    let rect = Rect::around(&m.path).unwrap().union(m.resolved.as_ref().unwrap().rect);
                    let to = self.after(0.5);
                    self.track.focus(FocusSource::Scroll, rect, 1.0, m.from, to);
                    return Ok(true);
                }
            }
        }
    }

    fn type_poll(&mut self, d: &mut dyn Driver, m: &mut TypeM) -> Result<bool, String> {
        loop {
            match m.at {
                TypeAt::Click => {
                    if !self.click_poll(d, &mut m.click)? {
                        return Ok(false);
                    }
                    self.pending_text = Some(m.text.iter().collect());
                    m.rect = self.resolve(d, &m.target, false)?.unwrap().rect;
                    m.rect_from = self.track.clock;
                    m.at = TypeAt::Char;
                }
                TypeAt::Char => {
                    let Some(&c) = m.text.get(m.i) else {
                        let end = self.track.clock.max(m.rect_from + 1);
                        self.track.focus(FocusSource::Type, m.rect, 1.0, m.rect_from, end);
                        return Ok(true);
                    };
                    // Character i lands at i / cps seconds after the first.
                    m.before = self.track.clock;
                    match control_key(c) {
                        // Down and up a key's frames apart; the schedule takes the
                        // frames back from the characters after it.
                        Some(key) => {
                            d.key(true, key, Mods::default())?;
                            let (down, frames) = (self.track.clock, self.key_frames());
                            self.track.inputs.push((down, Input::Key { code: key.to_string(), mods: Vec::new(), up: down + frames }));
                            m.key = key;
                            m.until = self.track.clock + frames;
                            m.at = TypeAt::KeyHeld;
                        }
                        None => {
                            d.text(&c.to_string())?;
                            self.track.inputs.push((self.track.clock, Input::Text(c.to_string())));
                            m.until = self.advance_target(1.0 / m.cps, 0);
                            m.at = TypeAt::Advance;
                        }
                    }
                }
                TypeAt::KeyHeld => {
                    if !self.reach(d, m.until)? {
                        return Ok(false);
                    }
                    d.key(false, m.key, Mods::default())?;
                    m.until = self.advance_target(1.0 / m.cps, 0);
                    m.at = TypeAt::Advance;
                }
                TypeAt::Advance => {
                    if !self.reach(d, m.until)? {
                        return Ok(false);
                    }
                    m.at = TypeAt::Follow;
                }
                TypeAt::Follow => {
                    // The field grows as the text wraps: follow its rect.
                    if self.track.clock > m.before {
                        if let Target::Path(_) = m.target {
                            let now = self.resolve(d, &m.target, false)?.unwrap().rect;
                            if now != m.rect {
                                self.track.focus(FocusSource::Type, m.rect, 1.0, m.rect_from, self.track.clock);
                                m.rect = now;
                                m.rect_from = self.track.clock;
                            }
                        }
                    }
                    m.i += 1;
                    m.at = TypeAt::Char;
                }
            }
        }
    }

    fn key_poll(&mut self, d: &mut dyn Driver, m: &mut KeyM) -> Result<bool, String> {
        if !m.held {
            d.key(true, &m.code, m.mods)?;
            m.frames = self.key_frames();
            let down = self.track.clock;
            self.track.inputs.push((down, Input::Key { code: m.code.clone(), mods: m.mods.names(), up: down + m.frames }));
            m.until = down + m.frames;
            m.held = true;
        }
        if m.frames > 0 {
            if !self.reach(d, m.until)? {
                return Ok(false);
            }
            self.time += m.frames as f64 / self.fps();
            d.key(false, &m.code, m.mods)?;
            m.until = self.advance_target(SETTLE_TIME, 0);
            m.frames = 0;
        }
        self.reach(d, m.until)
    }

    fn drop_poll(&mut self, d: &mut dyn Driver, m: &mut DropM) -> Result<bool, String> {
        loop {
            match m.at {
                DropAt::Resolve => {
                    let Some(at) = self.resolve(d, &m.target, true)? else { return Ok(false) };
                    m.mv = Some(self.move_begin(at.point, m.move_time));
                    m.resolved = Some(at);
                    m.at = DropAt::Moving;
                }
                DropAt::Moving => {
                    if !self.move_poll(d, m.mv.as_mut().unwrap())? {
                        return Ok(false);
                    }
                    m.path = m.mv.take().unwrap().px;
                    let (x, y) = self.cursor;
                    d.drop_file(&m.file, x, y)?;
                    m.until = self.advance_target(SETTLE_TIME, 0);
                    m.at = DropAt::Settled;
                }
                DropAt::Settled => {
                    if !self.reach(d, m.until)? {
                        return Ok(false);
                    }
                    let rect = Rect::around(&m.path).unwrap().union(m.resolved.as_ref().unwrap().rect);
                    let to = self.after(0.5);
                    self.track.focus(FocusSource::Drop, rect, 1.0, m.from, to);
                    return Ok(true);
                }
            }
        }
    }

    /// A drop's file: relative to the drop root, or absolute inside it,
    /// symlinks followed; anything outside is refused, so a run file cannot
    /// show the host's files in a published video.
    fn drop_path(&self, path: &str) -> Result<PathBuf, String> {
        let root = self.drop_root.canonicalize().map_err(|e| format!("drop root {}: {e}", self.drop_root.display()))?;
        let file = root.join(path).canonicalize().map_err(|e| format!("drop {path}: {e}"))?;
        if !file.starts_with(&root) || !file.is_file() {
            return Err(format!("drop {path}: only files under the run's drop root {} may be dropped", root.display()));
        }
        Ok(file)
    }

    fn resize_apply(&mut self, w: f64, h: f64) {
        let s = self.output.dpi_scale;
        self.window = (w, h);
        // The pointer stays over the window.
        self.cursor = (self.cursor.0.min(w), self.cursor.1.min(h));
        self.offset = ((self.output.width as f64 - w * s) * 0.5, (self.output.height as f64 - h * s) * 0.5);
        let cursor = self.cursor;
        self.pointer_to(cursor);
    }

    /// Wait for the app to be settled, up to `max`.
    fn settled_poll(&mut self, d: &mut dyn Driver, start: u64, max: f64) -> Result<bool, String> {
        let limit = frames_for(max, self.output.fps);
        let mut working: Option<Instant> = None;
        loop {
            if !self.fresh() || !d.ready() {
                return Ok(false);
            }
            let (settled, reasons) = d.settled()?;
            if settled {
                self.waited += self.track.clock - start;
                self.sync_time();
                return Ok(true);
            }
            // Only the app's workers are busy: wait for them with no frame. A frame stepped while
            // they run counts their time in frames, which differ with the machine's load (a
            // 1-core box's second run settled in 33 frames where its first took 1, and every
            // frame after it moved, 11-22).
            if self.mode == Clock::Virtual && !reasons.is_empty() && reasons.iter().all(|r| REAL_TIME_WORK.contains(&r.as_str())) {
                let since = *working.get_or_insert_with(Instant::now);
                if since.elapsed() < REAL_TIME_WAIT {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                // (said once per wait)
                let said = format!("wait_settled from frame {start}:");
                if !self.warnings.iter().any(|w| w.starts_with(&said)) {
                    self.warnings.push(format!("{said} the app's {} ran past {} s, so frames were stepped while it ran: this run's frames may differ from another run's", reasons.join(", "), REAL_TIME_WAIT.as_secs()));
                }
            }
            if self.track.clock - start >= limit {
                return Err(format!("not settled after {max} s (still pending: {})", reasons.join(", ")));
            }
            if !self.next_frame(d)? {
                return Ok(false);
            }
        }
    }

    /// Wait until the widget at `path` satisfies `met`; `fail` words the
    /// timeout from the last row seen.
    fn wait_poll(
        &mut self,
        d: &mut dyn Driver,
        m: &mut WaitM,
        path: &str,
        max: f64,
        met: &dyn Fn(&Row) -> bool,
        fail: &dyn Fn(&Runner, &mut dyn Driver, &WaitM) -> String,
    ) -> Result<bool, String> {
        let limit = frames_for(max, self.output.fps);
        loop {
            if !self.fresh() || !d.ready() {
                return Ok(false);
            }
            if let Some(row) = self.find_one(d, path)? {
                if row.visible && row.rect[2] > 0.0 && row.rect[3] > 0.0 && met(&row) {
                    self.waited += self.track.clock - m.start;
                    self.sync_time();
                    let (from, to) = (self.track.clock, self.after(1.0));
                    let rect = self.px(row.rect);
                    self.track.focus(FocusSource::Wait, rect, 0.8, from, to);
                    return Ok(true);
                }
                m.last_text = row.text.clone();
                m.last_row = Some(row);
            }
            if self.track.clock - m.start >= limit {
                return Err(fail(self, d, m));
            }
            if !self.next_frame(d)? {
                return Ok(false);
            }
        }
    }

    // ---- calls ----

    /// Calls that take effect at once. `d` is None in a dry run.
    fn instant(&mut self, d: Option<&mut dyn Driver>, call: &Call) -> Option<Result<(), String>> {
        Some(match call {
            Call::Mark { name } => {
                if self.track.mark(name) {
                    Ok(())
                } else {
                    Err(format!("mark {name} is already set: edits cut on marks, so each name is set once"))
                }
            }
            Call::Cut => (|| {
                if !self.track.recording() {
                    return Err("cut() while already cut".to_string());
                }
                if let Some(d) = d {
                    d.capture_pause(true)?;
                }
                self.track.cut();
                Ok(())
            })(),
            Call::Resume => (|| {
                if self.track.recording() {
                    return Err("resume() without a cut()".to_string());
                }
                if let Some(d) = d {
                    d.capture_pause(false)?;
                }
                self.track.resume();
                Ok(())
            })(),
            Call::FocusRect { rect, pad, weight, hold } => {
                let px = self.px(*rect);
                self.focus_rect(px, *pad, *weight, *hold);
                Ok(())
            }
            Call::Wide { hold } => {
                let rect = Rect::new(0.0, 0.0, self.output.width as f64, self.output.height as f64);
                let (from, to) = (self.track.clock, self.after(*hold).max(self.track.clock + 1));
                self.track.focus(FocusSource::Wide, rect, 1.0, from, to);
                Ok(())
            }
            _ => return None,
        })
    }

    fn focus_rect(&mut self, rect: Rect, pad: f64, weight: f64, hold: f64) {
        let (from, to) = (self.track.clock, self.after(hold).max(self.track.clock + 1));
        self.track.focus(FocusSource::Focus, rect.grow(pad * self.output.dpi_scale), weight, from, to);
    }

    fn stage_for(&mut self, d: &mut dyn Driver, call: &Call) -> Result<Stage, String> {
        if let Some(done) = self.instant(Some(d), call) {
            return done.map(|()| Stage::Finished);
        }
        let from = self.track.clock;
        Ok(match call {
            Call::Click { target, button, count, move_time } => Stage::Click(ClickM::new(target.clone(), *button, *count, *move_time, from)),
            Call::Drag { from: a, to: b, time, move_time } => Stage::Drag(DragM {
                a: a.clone(),
                b: b.clone(),
                time: *time,
                move_time: *move_time,
                from,
                at: DragAt::ResolveA,
                start: None,
                end: None,
                mv: None,
                path: Vec::new(),
                gap: Gap::default(),
                until: 0,
            }),
            Call::Scroll { target, dx, dy, time, move_time } => Stage::Scroll(ScrollM {
                target: target.clone(),
                dx: *dx,
                dy: *dy,
                time: *time,
                move_time: *move_time,
                from,
                at: ScrollAt::Resolve,
                resolved: None,
                mv: None,
                path: Vec::new(),
                wheel: None,
            }),
            Call::Type { target, text, cps, move_time } => Stage::Type(TypeM {
                target: target.clone(),
                text: text.chars().collect(),
                cps: *cps,
                click: ClickM::new(target.clone(), 0, 1, *move_time, from),
                at: TypeAt::Click,
                i: 0,
                rect: Rect::new(0.0, 0.0, 0.0, 0.0),
                rect_from: from,
                before: from,
                until: 0,
                key: "",
            }),
            Call::Key { code, mods } => Stage::Key(KeyM { code: code.clone(), mods: *mods, held: false, frames: 0, until: 0 }),
            Call::Drop { target, path, move_time } => {
                let file = self.drop_path(path)?;
                self.pending_file = Some(crate::identity::file_sha256(&file)?);
                Stage::Drop(DropM { target: target.clone(), file, move_time: *move_time, from, at: DropAt::Resolve, resolved: None, mv: None, path: Vec::new(), until: 0 })
            }
            Call::Resize { width, height } => {
                let (max_w, max_h) = self.output.logical();
                if *width > max_w + 1e-9 || *height > max_h + 1e-9 {
                    return Err(format!("resize to {width}x{height} refused: larger than the output window {max_w}x{max_h} (the recording would crop it)"));
                }
                d.resize(*width, *height)?;
                let until = self.advance_target(SETTLE_TIME, 1);
                Stage::Rest { until, resize: Some((*width, *height)) }
            }
            Call::Hold { seconds } => {
                let until = self.advance_target(*seconds, 0);
                Stage::Rest { until, resize: None }
            }
            Call::WaitSettled { .. } => Stage::Settled(from),
            Call::WaitText { .. } | Call::WaitVisible { .. } | Call::WaitState { .. } => Stage::Wait(WaitM { start: from, last_text: None, last_row: None }),
            Call::Focus { .. } => Stage::Focus,
            Call::Mark { .. } | Call::Cut | Call::Resume | Call::FocusRect { .. } | Call::Wide { .. } => unreachable!("instant calls finish above"),
        })
    }

    fn drive(&mut self, d: &mut dyn Driver, a: &mut Active) -> Result<bool, String> {
        if let Stage::Fresh = a.stage {
            a.stage = self.stage_for(d, &a.call)?;
        }
        match &mut a.stage {
            Stage::Fresh => unreachable!(),
            Stage::Finished => Ok(true),
            Stage::Click(m) => self.click_poll(d, m),
            Stage::Drag(m) => self.drag_poll(d, m),
            Stage::Scroll(m) => self.scroll_poll(d, m),
            Stage::Type(m) => self.type_poll(d, m),
            Stage::Key(m) => self.key_poll(d, m),
            Stage::Drop(m) => self.drop_poll(d, m),
            Stage::Rest { until, resize } => {
                if !self.reach(d, *until)? {
                    return Ok(false);
                }
                if let Some((w, h)) = resize.take() {
                    self.resize_apply(w, h);
                }
                Ok(true)
            }
            Stage::Settled(start) => {
                let Call::WaitSettled { max } = a.call else { unreachable!() };
                self.settled_poll(d, *start, max)
            }
            Stage::Focus => {
                let Call::Focus { target, pad, weight, hold } = &a.call else { unreachable!() };
                let Some(at) = self.resolve(d, target, true)? else { return Ok(false) };
                self.focus_rect(at.rect, *pad, *weight, *hold);
                Ok(true)
            }
            Stage::Wait(m) => match &a.call {
                Call::WaitVisible { path, max } => {
                    let (path, max) = (path.clone(), *max);
                    self.wait_poll(d, m, &path, max, &|_| true, &|_, _, _| format!("@{path} did not become visible within {max} s"))
                }
                Call::WaitText { path, text, max } => {
                    let (path, max) = (path.clone(), *max);
                    self.wait_poll(
                        d,
                        m,
                        &path,
                        max,
                        &|row| row.text.as_deref().is_some_and(|t| t.contains(text.as_str())),
                        &|r, d, m| match &m.last_text {
                            Some(seen) => format!("@{path} did not show {text:?} within {max} s (it shows {seen:?})"),
                            None => format!("@{path} did not show {text:?} within {max} s ({})", r.missing(d, &path)),
                        },
                    )
                }
                Call::WaitState { path, want, max } => {
                    let (path, max) = (path.clone(), *max);
                    self.wait_poll(d, m, &path, max, &|row| state_met(row, want), &|r, d, m| match &m.last_row {
                        Some(row) => format!("@{path} did not become {} within {max} s (it is {})", describe_want(want), describe_state(row)),
                        None => format!("@{path} did not become {} within {max} s ({})", describe_want(want), r.missing(d, &path)),
                    })
                }
                _ => unreachable!(),
            },
        }
    }
}

/// The calls the executor diffs the layout after (they act on the app).
fn diffs_layout(call: &Call) -> bool {
    !matches!(call, Call::Mark { .. } | Call::Cut | Call::Resume | Call::Focus { .. } | Call::FocusRect { .. } | Call::Wide { .. })
}

/// A row's layout key: its path (or id and type), with `#n` for its n-th
/// repeat, so repeated rows stay apart.
pub fn layout_keys(rows: Vec<Row>) -> Vec<SnapWidget> {
    let mut seen: Vec<String> = Vec::new();
    rows.into_iter()
        .map(|row| {
            let base = row.path.clone().unwrap_or_else(|| format!("{}:{}", row.id, row.ty));
            let n = seen.iter().filter(|k| **k == base).count();
            seen.push(base.clone());
            SnapWidget { key: format!("{base}#{n}"), rect: Rect::new(row.rect[0], row.rect[1], row.rect[2], row.rect[3]) }
        })
        .collect()
}

/// A widget that does not report `enabled` counts as enabled; one that does
/// not report `checked`, `selected` or `value` has none.
fn state_met(row: &Row, want: &StateWant) -> bool {
    want.enabled.map_or(true, |e| row.enabled.unwrap_or(true) == e)
        && want.checked.map_or(true, |c| row.checked == Some(c))
        && want.value.as_ref().map_or(true, |v| row.value.as_ref() == Some(v))
        && want.selected.as_ref().map_or(true, |s| match s {
            SelWant::Item(item) => row.selected.as_ref() == Some(item),
            SelWant::Some => row.selected.as_ref().is_some_and(|s| !s.is_empty()),
            SelWant::Nothing => row.selected.as_ref().map_or(true, |s| s.is_empty()),
        })
}

fn describe_want(want: &StateWant) -> String {
    let mut parts = Vec::new();
    if let Some(e) = want.enabled {
        parts.push(format!("enabled={e}"));
    }
    if let Some(c) = want.checked {
        parts.push(format!("checked={c}"));
    }
    match &want.selected {
        Some(SelWant::Item(s)) => parts.push(format!("selected={s:?}")),
        Some(SelWant::Some) => parts.push("selected=true".into()),
        Some(SelWant::Nothing) => parts.push("selected=false".into()),
        None => {}
    }
    if let Some(v) = &want.value {
        parts.push(format!("value={v:?}"));
    }
    parts.join(", ")
}

fn describe_state(row: &Row) -> String {
    let opt = |name: &str, v: Option<String>| format!("{name}={}", v.unwrap_or_else(|| "unknown".into()));
    [
        opt("enabled", row.enabled.map(|b| b.to_string())),
        opt("checked", row.checked.map(|b| b.to_string())),
        opt("selected", row.selected.as_ref().map(|s| format!("{s:?}"))),
        opt("value", row.value.as_ref().map(|s| format!("{s:?}"))),
    ]
    .join(", ")
}

/// Up to eight visible paths most like `target`: its last segment (or one
/// within two edits of it) counts most, then every segment they share.
pub fn near_paths(target: &str, paths: &[String]) -> Vec<String> {
    let want: Vec<&str> = target.split('.').collect();
    let last = want.last().copied().unwrap_or(target);
    let mut scored: Vec<(usize, &String)> = paths
        .iter()
        .map(|p| {
            let segs: Vec<&str> = p.split('.').collect();
            let tail = segs.last().copied().unwrap_or("");
            let tail_score = if tail == last { 4 } else if crate::doc::edit_distance(tail, last) <= 2 || tail.contains(last) { 3 } else { 0 };
            (tail_score + want.iter().filter(|w| segs.contains(w)).count(), p)
        })
        .filter(|(score, _)| *score > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.len().cmp(&b.1.len())));
    let mut out: Vec<String> = Vec::new();
    for (_, p) in scored {
        if !out.contains(p) {
            out.push(p.clone());
        }
        if out.len() == 8 {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::call::Call;
    use std::path::Path;

    /// An in-memory app: a widget list (some appear later), a log of what it
    /// was asked, stamped with the `now` the test last polled with.
    struct Fake {
        now: f64,
        widgets: Vec<Row>,
        later: Vec<(f64, Row)>,
        log: Vec<(f64, String)>,
        frames: u64,
        settled_at: f64,
        /// Settle checks still answered "pool": work on its workers, done in real time.
        pool_checks: u32,
    }

    impl Fake {
        fn new(widgets: Vec<Row>) -> Fake {
            Fake { now: 0.0, widgets, later: Vec::new(), log: Vec::new(), frames: 0, settled_at: 0.0, pool_checks: 0 }
        }

        fn arrive(&mut self) {
            let now = self.now;
            let (due, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.later).into_iter().partition(|(at, _)| *at <= now);
            self.later = rest;
            self.widgets.extend(due.into_iter().map(|(_, row)| row));
        }

        fn events(&self, prefix: &str) -> Vec<(f64, String)> {
            self.log.iter().filter(|(_, e)| e.starts_with(prefix)).cloned().collect()
        }
    }

    fn row(path: &str, rect: [f64; 4]) -> Row {
        let mut r = Row::new(Some(path.to_string()), path, "Button", rect);
        r.enabled = Some(true);
        r
    }

    impl Driver for Fake {
        fn snapshot(&mut self) -> Result<Vec<Row>, String> {
            self.arrive();
            Ok(self.widgets.clone())
        }
        fn mouse(&mut self, kind: Mouse, x: f64, y: f64) -> Result<(), String> {
            self.log.push((self.now, format!("{kind:?} {x:.1} {y:.1}")));
            Ok(())
        }
        fn key(&mut self, down: bool, code: &str, _: Mods) -> Result<(), String> {
            self.log.push((self.now, format!("key {} {code}", if down { "down" } else { "up" })));
            Ok(())
        }
        fn text(&mut self, text: &str) -> Result<(), String> {
            self.log.push((self.now, format!("text {text}")));
            Ok(())
        }
        fn drop_file(&mut self, _: &Path, _: f64, _: f64) -> Result<(), String> {
            Ok(())
        }
        fn resize(&mut self, _: f64, _: f64) -> Result<(), String> {
            Ok(())
        }
        fn settled(&mut self) -> Result<(bool, Vec<String>), String> {
            if self.pool_checks > 0 {
                self.pool_checks -= 1;
                return Ok((false, vec!["pool".into()]));
            }
            Ok((self.now >= self.settled_at, vec!["animation".into()]))
        }
        fn advance_frames(&mut self, k: u64) -> Result<(), String> {
            self.frames += k;
            self.now = self.frames as f64 / 60.0;
            self.arrive();
            Ok(())
        }
    }

    fn output() -> RunOutput {
        RunOutput { width: 360, height: 640, dpi_scale: 1.0, fps: 60 }
    }

    fn real() -> Runner {
        Runner::new(output(), Clock::Real, RunLimits::default(), PathBuf::new())
    }

    /// Poll every 1/60 s from `start` until the call ends; returns how it
    /// ended, when, and how many polls were Pending.
    fn run(r: &mut Runner, app: &mut Fake, start: f64, limit: f64) -> (Poll, f64, usize) {
        let mut t = start;
        let mut pending = 0;
        loop {
            app.now = t;
            match r.poll(app, t) {
                Poll::Pending => pending += 1,
                done => return (done, t, pending),
            }
            t += 1.0 / 60.0;
            assert!(t < start + limit, "the call did not end within {limit} s");
        }
    }

    #[test]
    fn wait_visible_is_pending_until_the_widget_appears() {
        let mut app = Fake::new(vec![row("main.other", [0.0, 0.0, 50.0, 20.0])]);
        app.later.push((100.5, row("main.go", [10.0, 20.0, 100.0, 40.0])));
        let mut r = real();
        r.begin(Call::WaitVisible { path: "go".into(), max: 5.0 });
        let (end, at, pending) = run(&mut r, &mut app, 100.0, 5.0);
        assert_eq!(end, Poll::Done);
        // The clock starts at the first poll: the widget came 0.5 s later.
        assert!((at - 100.5).abs() < 0.1, "{at}");
        assert!(pending >= 25, "{pending}");
        assert_eq!(r.track.calls.len(), 1);
        assert!(r.track.failures.is_empty());
    }

    #[test]
    fn a_timeout_names_the_nearest_paths() {
        let mut app = Fake::new(vec![row("main.goo", [0.0, 0.0, 50.0, 20.0]), row("main.stop", [0.0, 30.0, 50.0, 20.0])]);
        let mut r = real();
        r.begin(Call::WaitVisible { path: "main.go".into(), max: 1.0 });
        let (end, at, _) = run(&mut r, &mut app, 0.0, 5.0);
        let Poll::Failed(message) = end else { panic!("{end:?}") };
        assert!(message.contains("did not become visible within 1 s"), "{message}");
        assert!(at >= 1.0 && at < 1.1, "{at}");
        assert_eq!(r.track.failures.len(), 1);
        assert!(r.failed);
        // Which paths are like it, when the timeout has no row to show.
        let mut r = real();
        r.begin(Call::WaitText { path: "main.go".into(), text: "x".into(), max: 0.5 });
        let (end, _, _) = run(&mut r, &mut app, 0.0, 5.0);
        let Poll::Failed(message) = end else { panic!("{end:?}") };
        assert!(message.contains("candidates: @main.goo"), "{message}");
    }

    #[test]
    fn a_click_moves_then_presses_then_releases() {
        let mut app = Fake::new(vec![row("main.go", [10.0, 20.0, 100.0, 40.0])]);
        let mut r = real();
        r.begin(Call::Click { target: Target::Path("go".into()), button: 0, count: 1, move_time: 0.2 });
        let (end, at, _) = run(&mut r, &mut app, 0.0, 5.0);
        assert_eq!(end, Poll::Done);
        let kinds: Vec<&str> = app.log.iter().map(|(_, e)| e.split(' ').next().unwrap()).collect();
        let down = kinds.iter().position(|k| *k == "Down(0)").unwrap();
        let up = kinds.iter().position(|k| *k == "Up(0)").unwrap();
        assert!(kinds[..down].iter().all(|k| *k == "Move") && down >= 10, "{kinds:?}");
        assert_eq!(up, down + 1);
        // The pointer arrives on the centre, where it presses.
        assert!(app.log[down - 1].1.ends_with("60.0 40.0") && app.log[down].1.ends_with("60.0 40.0"));
        // Down no earlier than the move time, up a press later, one more settle frame.
        assert!(app.log[down].0 >= 0.2 - 1e-9 && app.log[up].0 - app.log[down].0 >= 0.049, "{:?}", &app.log[down..]);
        assert!(at - app.log[up].0 <= 0.05);
        assert_eq!(r.track.presses.len(), 1);
        assert!(r.track.presses[0].up.is_some());
    }

    #[test]
    fn typing_goes_at_cps() {
        let mut app = Fake::new(vec![row("main.field", [0.0, 0.0, 200.0, 30.0])]);
        let mut r = real();
        r.begin(Call::Type { target: Target::Path("field".into()), text: "abc\n".into(), cps: 10.0, move_time: 0.0 });
        let (end, _, _) = run(&mut r, &mut app, 0.0, 5.0);
        assert_eq!(end, Poll::Done);
        let chars = app.events("text");
        assert_eq!(chars.iter().map(|(_, e)| e.as_str()).collect::<Vec<_>>(), ["text a", "text b", "text c"]);
        for pair in chars.windows(2) {
            let gap = pair[1].0 - pair[0].0;
            assert!((gap - 0.1).abs() < 0.03, "{gap}");
        }
        // The newline is a Return key, down then up a few frames apart.
        let keys = app.events("key");
        assert_eq!(keys.len(), 2);
        assert!(keys[0].1.contains("down ReturnKey") && keys[1].1.contains("up ReturnKey"));
        assert!(keys[1].0 - keys[0].0 >= 0.04, "{keys:?}");
        assert_eq!(r.track.calls[0].text.as_deref(), Some("abc\n"));
    }

    #[test]
    fn a_call_resumes_across_many_polls_and_calls_follow_each_other() {
        let mut app = Fake::new(vec![row("main.go", [10.0, 20.0, 100.0, 40.0])]);
        let mut r = real();
        r.begin(Call::Hold { seconds: 0.5 });
        assert!(r.running());
        let (end, at, pending) = run(&mut r, &mut app, 7.0, 5.0);
        assert_eq!(end, Poll::Done);
        assert!((29..=31).contains(&pending), "{pending}");
        assert!((at - 7.5).abs() < 0.02, "{at}");
        assert!(!r.running());
        // The next call starts from the clock as it is now, so an idle gap
        // between calls does not shorten it.
        r.begin(Call::Hold { seconds: 0.25 });
        let (end, at2, pending) = run(&mut r, &mut app, 20.0, 5.0);
        assert_eq!(end, Poll::Done);
        assert!((at2 - 20.25).abs() < 0.02 && (14..=16).contains(&pending), "{at2} {pending}");
        // Nothing running polls Done.
        assert_eq!(r.poll(&mut app, 30.0), Poll::Done);
        assert_eq!(r.track.calls.len(), 2);
    }

    #[test]
    fn a_target_step_waits_for_its_widget_to_be_ready() {
        let mut app = Fake::new(Vec::new());
        let mut disabled = row("main.go", [10.0, 20.0, 100.0, 40.0]);
        disabled.enabled = Some(false);
        app.later.push((0.3, disabled));
        let mut r = real().with_target_wait(2.0);
        r.begin(Call::Click { target: Target::Path("go".into()), button: 0, count: 1, move_time: 0.0 });
        let mut t = 0.0;
        let mut ended = Poll::Pending;
        while t < 1.0 && ended == Poll::Pending {
            app.now = t;
            ended = r.poll(&mut app, t);
            if t > 0.6 {
                app.widgets[0].enabled = Some(true);
            }
            t += 1.0 / 60.0;
        }
        assert_eq!(ended, Poll::Done);
        let down = app.events("Down(0)");
        assert!(down[0].0 > 0.6, "pressed while the widget was disabled: {down:?}");
        // Never ready: fails at the wait's end, naming the path.
        let mut app = Fake::new(vec![row("main.stop", [0.0, 0.0, 10.0, 10.0])]);
        let mut r = real().with_target_wait(0.5);
        r.begin(Call::Click { target: Target::Path("main.go".into()), button: 0, count: 1, move_time: 0.0 });
        let (end, at, _) = run(&mut r, &mut app, 0.0, 5.0);
        let Poll::Failed(m) = end else { panic!("{end:?}") };
        assert!(m.contains("matches no visible widget") && m.contains("waited 0.5 s"), "{m}");
        assert!(at >= 0.5);
        assert!(app.log.is_empty());
    }

    #[test]
    fn wait_state_watches_the_named_states() {
        let mut app = Fake::new(Vec::new());
        let mut field = row("main.opt", [0.0, 0.0, 40.0, 20.0]);
        field.checked = Some(false);
        field.value = Some("3".into());
        app.widgets.push(field);
        let want = StateWant { checked: Some(true), value: Some("4".into()), ..Default::default() };
        let mut r = real();
        r.begin(Call::WaitState { path: "opt".into(), want: want.clone(), max: 1.0 });
        let (end, _, _) = run(&mut r, &mut app, 0.0, 5.0);
        let Poll::Failed(m) = end else { panic!("{end:?}") };
        assert!(m.contains("checked=true, value=\"4\"") && m.contains("it is enabled=true, checked=false"), "{m}");
        let mut r = real();
        r.begin(Call::WaitState { path: "opt".into(), want, max: 1.0 });
        app.now = 0.0;
        assert_eq!(r.poll(&mut app, 0.0), Poll::Pending);
        app.widgets[0].checked = Some(true);
        app.widgets[0].value = Some("4".into());
        app.now = 0.02;
        assert_eq!(r.poll(&mut app, 0.02), Poll::Done);
    }

    #[test]
    fn a_wait_looks_at_the_app_once_per_frame() {
        struct Counting(Fake, usize);
        impl Driver for Counting {
            fn snapshot(&mut self) -> Result<Vec<Row>, String> {
                self.1 += 1;
                self.0.snapshot()
            }
            fn mouse(&mut self, k: Mouse, x: f64, y: f64) -> Result<(), String> {
                self.0.mouse(k, x, y)
            }
            fn key(&mut self, d: bool, c: &str, m: Mods) -> Result<(), String> {
                self.0.key(d, c, m)
            }
            fn text(&mut self, t: &str) -> Result<(), String> {
                self.0.text(t)
            }
            fn drop_file(&mut self, p: &Path, x: f64, y: f64) -> Result<(), String> {
                self.0.drop_file(p, x, y)
            }
            fn resize(&mut self, w: f64, h: f64) -> Result<(), String> {
                self.0.resize(w, h)
            }
            fn settled(&mut self) -> Result<(bool, Vec<String>), String> {
                self.0.settled()
            }
            fn advance_frames(&mut self, k: u64) -> Result<(), String> {
                self.0.advance_frames(k)
            }
        }
        let mut app = Counting(Fake::new(Vec::new()), 0);
        let mut r = real();
        r.begin(Call::WaitVisible { path: "x".into(), max: 10.0 });
        // 100 polls inside one frame time: one look.
        for i in 0..100 {
            assert_eq!(r.poll(&mut app, 0.001 + i as f64 * 1e-5), Poll::Pending);
        }
        assert_eq!(app.1, 1);
    }

    #[test]
    fn a_pending_answer_is_not_a_missing_widget() {
        /// A driver whose reads are answered late: `ready` is false until `answers_at`.
        struct Slow(Fake, f64);
        impl Driver for Slow {
            fn ready(&mut self) -> bool {
                self.0.now >= self.1
            }
            fn snapshot(&mut self) -> Result<Vec<Row>, String> {
                self.0.snapshot()
            }
            fn mouse(&mut self, k: Mouse, x: f64, y: f64) -> Result<(), String> {
                self.0.mouse(k, x, y)
            }
            fn key(&mut self, d: bool, c: &str, m: Mods) -> Result<(), String> {
                self.0.key(d, c, m)
            }
            fn text(&mut self, t: &str) -> Result<(), String> {
                self.0.text(t)
            }
            fn drop_file(&mut self, p: &Path, x: f64, y: f64) -> Result<(), String> {
                self.0.drop_file(p, x, y)
            }
            fn resize(&mut self, w: f64, h: f64) -> Result<(), String> {
                self.0.resize(w, h)
            }
            fn settled(&mut self) -> Result<(bool, Vec<String>), String> {
                self.0.settled()
            }
            fn advance_frames(&mut self, k: u64) -> Result<(), String> {
                self.0.advance_frames(k)
            }
        }
        // The widget is there from the start; the first answer takes 2 s,
        // far past the 0.5 s the wait is allowed. Waiting for an answer
        // takes nothing from the wait.
        let mut app = Slow(Fake::new(vec![row("main.go", [10.0, 20.0, 100.0, 40.0])]), 2.0);
        let mut r = real().with_target_wait(0.5);
        r.begin(Call::Click { target: Target::Path("go".into()), button: 0, count: 1, move_time: 0.0 });
        let mut t = 0.0;
        let mut ended = Poll::Pending;
        while t < 10.0 && ended == Poll::Pending {
            app.0.now = t;
            ended = r.poll(&mut app, t);
            t += 1.0 / 60.0;
        }
        assert_eq!(ended, Poll::Done);
        assert!(app.0.events("Down").first().is_some_and(|(at, _)| *at >= 2.0), "no press before the first answer: {:?}", app.0.log);
    }

    #[test]
    fn the_virtual_clock_runs_a_call_to_completion_in_one_poll() {
        let mut app = Fake::new(vec![row("main.go", [10.0, 20.0, 100.0, 40.0])]);
        let mut r = Runner::new(output(), Clock::Virtual, RunLimits::default(), PathBuf::new());
        r.snapshot_layout(&mut app).unwrap();
        r.begin(Call::Click { target: Target::Path("go".into()), button: 0, count: 1, move_time: 0.35 });
        assert_eq!(r.poll(&mut app, 0.0), Poll::Done);
        // 0.35 s of moving is 21 frames, a press 3, the settle frame 1.
        assert_eq!(app.frames, 21 + 3 + 1);
        assert_eq!(r.track.clock, app.frames);
        assert_eq!(r.track.presses[0].down, 21);
        assert_eq!(r.track.presses[0].up, Some(24));
        app.later.push((app.now + 0.5, row("main.late", [0.0, 0.0, 5.0, 5.0])));
        r.begin(Call::WaitVisible { path: "late".into(), max: 5.0 });
        assert_eq!(r.poll(&mut app, 0.0), Poll::Done);
        assert_eq!(r.track.calls[1].waited, r.track.calls[1].to - r.track.calls[1].from);
        assert!(r.track.calls[1].waited >= 30);
        // A wait that runs out fails in the same poll, like today.
        r.begin(Call::WaitVisible { path: "never".into(), max: 0.5 });
        assert!(matches!(r.poll(&mut app, 0.0), Poll::Failed(_)));
        // A settle wait steps no frame while only the app's workers are busy (its pool here): they
        // finish in real time, and frames stepped meanwhile would count that time, different at
        // every machine load. An animation still takes its frames.
        let before = app.frames;
        app.pool_checks = 5;
        r.begin(Call::WaitSettled { max: 5.0 });
        assert_eq!(r.poll(&mut app, 0.0), Poll::Done);
        assert_eq!((app.frames, app.pool_checks), (before, 0), "no frame while its pool worked");
        app.settled_at = (app.frames + 15) as f64 / 60.0;
        r.begin(Call::WaitSettled { max: 5.0 });
        assert_eq!(r.poll(&mut app, 0.0), Poll::Done);
        assert_eq!(app.frames, before + 15, "an animation's 0.25 s is its 15 frames");
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn dry_runs_collect_targets_and_check_marks() {
        let mut r = Runner::new(output(), Clock::Virtual, RunLimits::default(), PathBuf::new());
        r.dry(Call::Click { target: Target::Path("a.b".into()), button: 0, count: 1, move_time: 0.1 }).unwrap();
        r.dry(Call::Mark { name: "x".into() }).unwrap();
        assert!(r.dry(Call::Mark { name: "x".into() }).unwrap_err().contains("already set"));
        assert!(r.dry(Call::Resize { width: 100.0, height: 100.0 }).unwrap_err().contains("keep its aspect"));
        assert_eq!(r.targets.len(), 1);
    }

    #[test]
    fn near_paths_rank_the_last_segment_first() {
        let paths: Vec<String> = ["body.increment_button", "body.main_view.counter_label", "caption_bar.caption_label.label", "body.main_view"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(near_paths("increment", &paths)[0], "body.increment_button");
        assert_eq!(near_paths("main.counter_lable", &paths)[0], "body.main_view.counter_label");
        assert!(near_paths("zzz", &paths).is_empty());
    }

    #[test]
    fn a_splash_record_becomes_a_call() {
        use crate::call::call_from_record;
        use crate::doc::eval_run;
        use makepad_script::*;
        let mut host = ScriptVmHost::new((), ());
        let mut vm = ScriptVm { host: host.host_context(), bx: Box::new(ScriptVmBase::new()) };
        let (value, errors) = eval_run(&mut vm, "use mod.apprun.*\nAppRun{call: @click target: @counter.increment count: 3}", "t");
        assert!(errors.is_empty(), "{errors:?}");
        let call = call_from_record(&vm, value.unwrap()).unwrap();
        assert_eq!(call, Call::Click { target: Target::Path("counter.increment".into()), button: 0, count: 3, move_time: crate::call::MOVE_TIME });
    }
}
