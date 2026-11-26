//! Playback of one slide's demo steps, like a video: play, pause, step,
//! speed and seek, over any app_run [`Driver`] on the real clock.
//!
//! A [`Playback`] is the state machine behind the DECK transport. It owns
//! one app_run [`Runner`], the step the demo is at and why it is stopped.
//! `drive` runs it as far as it goes now; nothing blocks or sleeps.
//!
//! Seeking to step `n` means: the app is as it is when step `n` is about to
//! begin (steps `0..n` have run). The app cannot run backwards, so a seek
//! back is a relaunch of the app into a clean home (the caller does that,
//! see [`Playback::needs_relaunch`]) followed by a fast replay of steps
//! `0..n`; a seek forward replays only the steps in between. A fast replay
//! has no holds, no cursor travel and no typing delay (the calls run
//! [`Call::at_speed`] with `f64::INFINITY`), and every wait stays a wait on
//! the app's widget tree, so a replay never runs ahead of the app. Snapshot
//! points are not kept: they would need the app to save and restore its own
//! state, and a replay of a short script costs about as much as the app's
//! start.
//!
//! The presenter using the app while a replay runs does not cut the replay
//! short (half a replay is a state nobody asked for): the replay finishes,
//! then the demo is paused, "paused (you)".

use crate::doc::{DemoStep, Step};
use makepad_story_app_run::call::Call;
use makepad_story_app_run::doc::RunOutput;
use makepad_story_app_run::driver::Driver;
use makepad_story_app_run::runner::{Clock, Poll, RunLimits, Runner};
use std::path::PathBuf;

/// How long a step waits for its widget to be there, visible and enabled.
pub const TARGET_WAIT: f64 = 10.0;
/// The runner's frame rate (its waits count frames of this clock).
pub const FPS: u32 = 60;
/// A step that has waited this long shows what it waits for.
pub const WAIT_NOTE_AFTER: f64 = 0.6;
/// Where a demo is.
#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    /// Not running (not started, or stopped by leaving the slide).
    Idle,
    Running,
    /// Stopped at a step boundary; see [`Playback::paused_by`].
    Paused,
    Done,
    /// The step failed or the app is gone; playing tries the step again.
    Failed(String),
}

/// Who stopped a paused demo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PausedBy {
    /// A `pause()` step in the script.
    Script,
    /// The presenter used the app.
    You,
    /// The presenter pressed pause, or stepped, or sought.
    Transport,
}

/// A fast replay on its way to the start of step `to`.
#[derive(Clone, Copy, Debug)]
struct Seek {
    to: usize,
    /// Play on from there; else pause there, by `by`.
    then_run: bool,
    by: PausedBy,
}

pub struct Playback {
    /// The step in flight (or next to begin).
    pub step: usize,
    pub phase: Phase,
    /// The app launch the progress belongs to.
    pub generation: u64,
    /// The step has been begun on the runner.
    pub begun: bool,
    /// When the step began, for its waiting note.
    pub begun_at: f64,
    /// The step in flight is a `hold`: only time, so a seek cuts it short.
    hold_in_flight: bool,
    paused_by: PausedBy,
    /// Pause at the next step boundary, by whom.
    pause_at_boundary: Option<PausedBy>,
    runner: Option<Runner>,
    seek: Option<Seek>,
}

impl Playback {
    pub fn fresh(generation: u64) -> Playback {
        Playback {
            step: 0,
            phase: Phase::Idle,
            generation,
            begun: false,
            begun_at: 0.0,
            hold_in_flight: false,
            paused_by: PausedBy::Transport,
            pause_at_boundary: None,
            runner: None,
            seek: None,
        }
    }

    pub fn paused_by(&self) -> PausedBy {
        self.paused_by
    }

    /// The step a fast replay is on its way to.
    pub fn seeking(&self) -> Option<usize> {
        self.seek.map(|seek| seek.to)
    }

    /// Whether a pause is asked for, at the next step boundary.
    pub fn pausing(&self) -> bool {
        self.pause_at_boundary.is_some() || self.seek.is_some_and(|seek| !seek.then_run)
    }

    /// The runner's pointer (app points), while it plays.
    pub fn cursor(&self) -> Option<(f64, f64)> {
        if self.phase == Phase::Idle {
            return None;
        }
        self.runner.as_ref().map(Runner::cursor)
    }

    /// Play, pause or restart. Running: pause at the next step boundary
    /// (during a replay: play on, or stop, when it arrives). `restart`:
    /// from step 0, the caller having relaunched the app. The app's launch
    /// is `generation`: a progress made on another launch starts over.
    pub fn toggle(&mut self, steps: usize, generation: u64, restart: bool) {
        if restart || self.generation != generation || self.phase == Phase::Done {
            *self = Playback::fresh(generation);
        }
        if let Some(seek) = &mut self.seek {
            seek.then_run = !seek.then_run;
            seek.by = PausedBy::Transport;
            return;
        }
        match self.phase {
            Phase::Running => self.pause_at_boundary = Some(PausedBy::Transport),
            _ if steps > 0 => {
                self.phase = Phase::Running;
                self.pause_at_boundary = None;
            }
            _ => {}
        }
    }

    /// The stop an arrow goes to. The stops are the start, after every
    /// step and the end: `0..=steps` steps run. Forward is the stop after the
    /// step in flight (or the one a replay is on its way to); back is the one
    /// before -- the start of the step in flight, which a relaunch and a
    /// replay reach, or the stop before the step the demo stands at. None:
    /// there is none that way (the deck's own next / previous slide).
    pub fn arrow_stop(&self, steps: usize, forward: bool) -> Option<usize> {
        let step = self.step.min(steps);
        if forward {
            let here = self.seeking().unwrap_or(step).min(steps);
            (here < steps).then_some(here + 1)
        } else {
            match self.seeking() {
                Some(to) => to.checked_sub(1),
                None if self.begun => Some(step),
                None => step.checked_sub(1),
            }
        }
    }

    /// Whether getting to the start of step `to` takes a relaunch of the app.
    pub fn needs_relaunch(&self, to: usize) -> bool {
        to < self.step || (to == self.step && self.begun)
    }

    /// Go to the start of step `to` (`steps` is the step count; `to` past
    /// it is the end): replay the steps in between fast. After a seek back
    /// the app must have been relaunched (the caller asks
    /// [`needs_relaunch`](Self::needs_relaunch) first) and `generation` is
    /// the new launch's. `then_run`: play on from `to`; else pause there.
    pub fn seek(&mut self, to: usize, steps: usize, generation: u64, then_run: bool) {
        if self.generation != generation || self.needs_relaunch(to) {
            *self = Playback::fresh(generation);
        }
        let to = to.min(steps);
        self.pause_at_boundary = None;
        // A hold in flight is only time: cut it short. Any other step runs out.
        if self.begun {
            if let (Some(runner), true) = (self.runner.as_mut(), self.hold_in_flight) {
                runner.begin(Call::Hold { seconds: 0.0 });
            }
        }
        if self.step >= to && !self.begun {
            self.seek = None;
            self.phase = if to >= steps { Phase::Done } else if then_run { Phase::Running } else { Phase::Paused };
            if self.phase == Phase::Paused {
                self.paused_by = PausedBy::Transport;
            }
            return;
        }
        self.seek = Some(Seek { to, then_run, by: PausedBy::Transport });
        self.phase = Phase::Running;
    }

    /// The presenter used the app: pause at the next step boundary (after
    /// the replay, if one runs).
    pub fn presenter_input(&mut self) {
        if let Some(seek) = &mut self.seek {
            seek.then_run = false;
            seek.by = PausedBy::You;
        } else if self.phase == Phase::Running {
            self.pause_at_boundary = Some(PausedBy::You);
        }
    }

    /// Leaving the slide stops the demo; the step it reached is kept (a
    /// replay in progress is dropped: the app is as far as it got).
    pub fn leave(&mut self) {
        if self.phase == Phase::Running {
            self.phase = Phase::Idle;
        }
        self.seek = None;
        self.pause_at_boundary = None;
        self.runner = None;
        self.begun = false;
    }

    pub fn fail(&mut self, why: String) {
        // A replay that failed is over: the failure is what the presenter sees.
        self.seek = None;
        self.phase = Phase::Failed(why);
    }

    /// Run the steps as far as they go now. `steps[self.step]` is driven
    /// over `driver` with `output` as the window, at `speed` times its
    /// written pace (a replay: as fast as it goes).
    pub fn drive(&mut self, steps: &[DemoStep], driver: &mut dyn Driver, output: RunOutput, speed: f64, now: f64) {
        if self.phase != Phase::Running {
            return;
        }
        // Instant steps chain; the bound keeps a long run of them from
        // taking a whole beat.
        for _ in 0..64 {
            if !self.begun {
                if let Some(seek) = self.seek.filter(|seek| self.step >= seek.to.min(steps.len())) {
                    self.seek = None;
                    if !seek.then_run && self.step < steps.len() {
                        self.phase = Phase::Paused;
                        self.paused_by = seek.by;
                        return;
                    }
                }
                if self.step >= steps.len() {
                    self.phase = Phase::Done;
                    self.runner = None;
                    return;
                }
            }
            let fast = self.seek.is_some();
            let call = match &steps[self.step].step {
                Step::Pause if fast => {
                    // A replay does not stop for the script's own stops.
                    self.step += 1;
                    continue;
                }
                Step::Pause => {
                    self.step += 1;
                    self.phase = Phase::Paused;
                    self.paused_by = PausedBy::Script;
                    return;
                }
                Step::Call(call) => call,
            };
            let runner = self.runner.get_or_insert_with(|| Runner::new(output, Clock::Real, RunLimits::default(), PathBuf::new()).with_target_wait(TARGET_WAIT));
            if !self.begun {
                runner.begin(call.at_speed(if fast { f64::INFINITY } else { speed }));
                self.begun = true;
                self.begun_at = now;
                self.hold_in_flight = matches!(call, Call::Hold { .. });
            }
            match runner.poll(driver, now) {
                Poll::Pending => return,
                Poll::Failed(why) => {
                    // The runner stays failed; a retry starts a fresh one.
                    self.runner = None;
                    self.begun = false;
                    self.hold_in_flight = false;
                    return self.fail(why);
                }
                Poll::Done => {
                    self.begun = false;
                    self.hold_in_flight = false;
                    self.step += 1;
                    if let (Some(by), None) = (self.pause_at_boundary.take(), self.seek) {
                        self.phase = if self.step >= steps.len() { Phase::Done } else { Phase::Paused };
                        self.paused_by = by;
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_story_app_run::call::Target;
    use makepad_story_app_run::driver::{Mods, Mouse, Row};
    use std::path::Path;

    /// A counter: `counter.inc` counts presses, `counter.label` shows them.
    /// Widgets in `later` appear when their time has come.
    struct Fake {
        presses: u32,
        downs: Vec<f64>,
        later: Vec<(f64, Row)>,
        now: f64,
    }

    fn row(path: &str, y: f64) -> Row {
        let mut row = Row::new(Some(path.to_string()), path, "Button", [10.0, y, 100.0, 30.0]);
        row.enabled = Some(true);
        row
    }

    impl Fake {
        fn new() -> Fake {
            Fake { presses: 0, downs: Vec::new(), later: Vec::new(), now: 0.0 }
        }

        /// The app is launched again: its state is the initial one.
        fn relaunch(&mut self) {
            self.presses = 0;
        }
    }

    impl Driver for Fake {
        fn snapshot(&mut self) -> Result<Vec<Row>, String> {
            let now = self.now;
            let mut rows = vec![row("main.inc", 10.0), row("main.dec", 60.0)];
            let mut label = row("main.label", 110.0);
            label.text = Some(self.presses.to_string());
            rows.push(label);
            rows.extend(self.later.iter().filter(|(at, _)| *at <= now).map(|(_, row)| row.clone()));
            Ok(rows)
        }
        fn mouse(&mut self, kind: Mouse, _: f64, y: f64) -> Result<(), String> {
            if kind == Mouse::Down(0) && y < 40.0 {
                self.presses += 1;
                self.downs.push(self.now);
            }
            Ok(())
        }
        fn key(&mut self, _: bool, _: &str, _: Mods) -> Result<(), String> {
            Ok(())
        }
        fn text(&mut self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn drop_file(&mut self, _: &Path, _: f64, _: f64) -> Result<(), String> {
            Ok(())
        }
        fn resize(&mut self, _: f64, _: f64) -> Result<(), String> {
            Ok(())
        }
        fn settled(&mut self) -> Result<(bool, Vec<String>), String> {
            Ok((true, Vec::new()))
        }
        fn advance_frames(&mut self, _: u64) -> Result<(), String> {
            Err("real clock".into())
        }
    }

    fn output() -> RunOutput {
        RunOutput { width: 400, height: 300, dpi_scale: 1.0, fps: 60 }
    }

    fn step(call: Call) -> DemoStep {
        DemoStep { step: Step::Call(call), line: 0 }
    }

    fn click(target: &str) -> DemoStep {
        step(Call::Click { target: Target::Path(target.into()), button: 0, count: 1, move_time: 0.3 })
    }

    fn hold(seconds: f64) -> DemoStep {
        step(Call::Hold { seconds })
    }

    /// click, hold 1 s, ... : five clicks on alternating buttons' worth of
    /// time; only `inc` counts.
    fn script() -> Vec<DemoStep> {
        let mut steps = Vec::new();
        for _ in 0..4 {
            steps.push(click("inc"));
            steps.push(hold(1.0));
        }
        steps.push(step(Call::WaitText { path: "label".into(), text: "4".into(), max: 5.0 }));
        steps
    }

    /// Drive every 1/60 s from `*now` until the phase is not Running.
    fn run(playback: &mut Playback, steps: &[DemoStep], app: &mut Fake, now: &mut f64, speed: f64) {
        let start = *now;
        while playback.phase == Phase::Running {
            app.now = *now;
            playback.drive(steps, app, output(), speed, *now);
            *now += 1.0 / 60.0;
            assert!(*now < start + 120.0, "the run did not stop");
        }
    }

    /// Press Right or Left: the arrow's stop, as the player takes it.
    fn arrow(playback: &mut Playback, steps: &[DemoStep], app: &mut Fake, now: &mut f64, forward: bool) -> bool {
        let Some(to) = playback.arrow_stop(steps.len(), forward) else { return false };
        if playback.needs_relaunch(to) {
            app.relaunch();
        }
        playback.seek(to, steps.len(), 1, false);
        run(playback, steps, app, now, 1.0);
        true
    }

    #[test]
    fn the_arrows_walk_the_stops_of_a_demo() {
        let steps = script();
        let (mut app, mut now) = (Fake::new(), 0.0);
        let mut playback = Playback::fresh(1);
        // Left at the start is the deck's (the previous slide's end).
        assert!(!arrow(&mut playback, &steps, &mut app, &mut now, false));
        // Right performs the steps one by one, each stop reached at once.
        for stop in 1..=steps.len() {
            let began = now;
            assert!(arrow(&mut playback, &steps, &mut app, &mut now, true), "stop {stop}");
            assert_eq!(playback.step, stop);
            // Only the platform's multi-click window (0.5 s) holds back a second
            // click on the same spot; nothing is paced.
            assert!(now - began < 0.6, "step {stop} took {}", now - began);
        }
        assert_eq!(app.presses, 4);
        // From the end Right is the deck's (the next slide's start).
        assert!(!arrow(&mut playback, &steps, &mut app, &mut now, true));
        // Left goes back stop by stop, by a relaunch and a fast replay.
        for stop in (0..steps.len()).rev() {
            let began = now;
            assert!(arrow(&mut playback, &steps, &mut app, &mut now, false), "back to {stop}");
            assert_eq!(playback.step, stop);
            // Four clicks on one spot: each waits out the multi-click window, the
            // holds between them and the cursor travel cost nothing.
            assert!(now - began < 0.6 * stop.div_ceil(2) as f64 + 0.2, "back to {stop} took {}", now - began);
        }
        assert_eq!(app.presses, 0, "the start is the app as launched");
        assert!(!arrow(&mut playback, &steps, &mut app, &mut now, false));
    }

    #[test]
    fn right_in_the_middle_of_a_step_finishes_it_and_left_returns_to_its_start() {
        let steps = script();
        let (mut app, mut now) = (Fake::new(), 0.0);
        let mut playback = Playback::fresh(1);
        playback.toggle(steps.len(), 1, false);
        // Into the first hold, at its written pace.
        while playback.step < 1 || !playback.begun {
            app.now = now;
            playback.drive(&steps, &mut app, output(), 1.0, now);
            now += 1.0 / 60.0;
        }
        assert_eq!(playback.step, 1);
        let began = now;
        assert!(arrow(&mut playback, &steps, &mut app, &mut now, true));
        assert_eq!((playback.step, playback.phase.clone()), (2, Phase::Paused));
        assert!(now - began < 0.2, "the hold was cut short: {}", now - began);
        // Left: the stop before step 2 is after step 1.
        assert!(arrow(&mut playback, &steps, &mut app, &mut now, false));
        assert_eq!(playback.step, 1);
    }

    #[test]
    fn a_wait_is_over_as_soon_as_the_app_shows_what_it_waits_for() {
        let steps = vec![
            click("inc"),
            step(Call::WaitVisible { path: "late".into(), max: 5.0 }),
            step(Call::WaitText { path: "label".into(), text: "1".into(), max: 5.0 }),
            hold(3.0),
        ];
        let mut app = Fake::new();
        app.later.push((0.5, row("main.late", 150.0)));
        let (mut now, mut playback) = (0.0, Playback::fresh(1));
        for stop in 1..=steps.len() {
            let began = now;
            assert!(arrow(&mut playback, &steps, &mut app, &mut now, true));
            assert_eq!(playback.step, stop);
            if stop == 2 {
                assert!((now - 0.5).abs() < 0.1, "the wait ended at {now}, the widget came at 0.5");
            } else {
                assert!(now - began < 0.2, "step {stop} took {}", now - began);
            }
        }
    }

    #[test]
    fn plays_the_script_through() {
        let steps = script();
        let (mut app, mut now) = (Fake::new(), 0.0);
        let mut playback = Playback::fresh(1);
        playback.toggle(steps.len(), 1, false);
        run(&mut playback, &steps, &mut app, &mut now, 1.0);
        assert_eq!(playback.phase, Phase::Done);
        assert_eq!(app.presses, 4);
        assert!(now > 4.0, "the holds are real time: {now}");
    }

    #[test]
    fn seek_back_relaunches_and_replays_from_zero_at_full_speed() {
        let steps = script();
        let (mut app, mut now) = (Fake::new(), 0.0);
        let mut playback = Playback::fresh(1);
        playback.toggle(steps.len(), 1, false);
        run(&mut playback, &steps, &mut app, &mut now, 1.0);
        assert_eq!(app.presses, 4);
        // Back to the start of step 5 (the third click): two clicks have run.
        assert!(playback.needs_relaunch(4));
        app.relaunch();
        playback.seek(4, steps.len(), 2, false);
        assert_eq!(playback.seeking(), Some(4));
        let (before, started) = (app.downs.len(), now);
        run(&mut playback, &steps, &mut app, &mut now, 1.0);
        assert_eq!(playback.phase, Phase::Paused);
        assert_eq!(playback.paused_by(), PausedBy::Transport);
        assert_eq!((playback.step, playback.seeking()), (4, None));
        assert_eq!(app.presses, 2, "the app is as it is before step 5");
        assert_eq!(app.downs.len() - before, 2);
        // Two 1 s holds and two 0.3 s cursor moves would take 2.6 s: the
        // replay takes the presses and the 0.5 s guard between clicks on one
        // button, nothing else.
        assert!(now - started < 1.2, "the replay took {}", now - started);
        // Playing on goes at the written pace again.
        playback.toggle(steps.len(), 2, false);
        let resumed = now;
        run(&mut playback, &steps, &mut app, &mut now, 1.0);
        assert_eq!((playback.phase.clone(), app.presses), (Phase::Done, 4));
        assert!(now - resumed > 2.0);
    }

    #[test]
    fn a_forward_skip_replays_only_the_steps_in_between() {
        let steps = script();
        let (mut app, mut now) = (Fake::new(), 0.0);
        let mut playback = Playback::fresh(1);
        // Perform one step, then pause: the first click.
        playback.seek(1, steps.len(), 1, false);
        run(&mut playback, &steps, &mut app, &mut now, 1.0);
        assert_eq!((playback.step, playback.phase.clone(), app.presses), (1, Phase::Paused, 1));
        assert!(!playback.needs_relaunch(5));
        let started = now;
        playback.seek(5, steps.len(), 1, false);
        run(&mut playback, &steps, &mut app, &mut now, 1.0);
        // Steps 1..5 ran: hold, click, hold, click. The first click is not run again.
        assert_eq!((playback.step, app.presses), (5, 3));
        assert_eq!(app.downs.len(), 3);
        assert!(now - started < 1.2, "{}", now - started);
        // To the end.
        playback.seek(steps.len() + 3, steps.len(), 1, false);
        run(&mut playback, &steps, &mut app, &mut now, 1.0);
        assert_eq!((playback.phase.clone(), app.presses), (Phase::Done, 4));
    }

    #[test]
    fn a_seek_on_a_fresh_progress_replays_from_zero_without_a_relaunch() {
        let steps = script();
        let (mut app, mut now) = (Fake::new(), 0.0);
        let mut playback = Playback::fresh(1);
        assert!(!playback.needs_relaunch(3));
        playback.seek(3, steps.len(), 1, true);
        run(&mut playback, &steps, &mut app, &mut now, 1.0);
        // It runs on after the seek (then_run) at the written pace: to the end.
        assert_eq!((playback.phase.clone(), app.presses), (Phase::Done, 4));
    }

    #[test]
    fn speed_scales_holds_and_never_the_waits() {
        let held = |speed: f64| {
            let steps = vec![hold(1.0)];
            let (mut app, mut now) = (Fake::new(), 0.0);
            let mut playback = Playback::fresh(1);
            playback.toggle(1, 1, false);
            run(&mut playback, &steps, &mut app, &mut now, speed);
            now
        };
        let (slow, normal, fast) = (held(0.5), held(1.0), held(2.0));
        assert!((slow - 2.0).abs() < 0.1 && (normal - 1.0).abs() < 0.1 && (fast - 0.5).abs() < 0.1, "{slow} {normal} {fast}");
        // A widget that comes after 1.5 s is waited for at 2x as at 1x ...
        let waited = |speed: f64, max: f64| {
            let steps = vec![step(Call::WaitVisible { path: "late".into(), max })];
            let mut app = Fake::new();
            app.later.push((1.5, row("main.late", 150.0)));
            let (mut now, mut playback) = (0.0, Playback::fresh(1));
            playback.toggle(1, 1, false);
            run(&mut playback, &steps, &mut app, &mut now, speed);
            (playback.phase.clone(), now)
        };
        let (phase, at) = waited(2.0, 5.0);
        assert_eq!(phase, Phase::Done);
        assert!((at - 1.5).abs() < 0.15, "{at}");
        // ... and its timeout is still its written one.
        let (phase, at) = waited(2.0, 1.0);
        assert!(matches!(phase, Phase::Failed(_)));
        assert!((at - 1.0).abs() < 0.15, "{at}");
    }

    #[test]
    fn the_presenter_using_the_app_mid_replay_pauses_after_it() {
        let steps = script();
        let (mut app, mut now) = (Fake::new(), 0.0);
        let mut playback = Playback::fresh(1);
        playback.seek(6, steps.len(), 1, true);
        // A few beats into the replay the presenter clicks in the app.
        for _ in 0..5 {
            app.now = now;
            playback.drive(&steps, &mut app, output(), 1.0, now);
            now += 1.0 / 60.0;
        }
        assert!(playback.seeking().is_some());
        playback.presenter_input();
        run(&mut playback, &steps, &mut app, &mut now, 1.0);
        // The replay was finished, not cut off, and then the demo paused.
        assert_eq!((playback.step, playback.phase.clone(), playback.paused_by()), (6, Phase::Paused, PausedBy::You));
        assert_eq!(app.presses, 3);
        // Play goes on from the next step.
        playback.toggle(steps.len(), 1, false);
        run(&mut playback, &steps, &mut app, &mut now, 1.0);
        assert_eq!((playback.phase.clone(), app.presses), (Phase::Done, 4));
    }

    #[test]
    fn the_presenter_using_the_app_pauses_at_the_next_step_boundary() {
        let steps = script();
        let (mut app, mut now) = (Fake::new(), 0.0);
        let mut playback = Playback::fresh(1);
        playback.toggle(steps.len(), 1, false);
        for _ in 0..10 {
            app.now = now;
            playback.drive(&steps, &mut app, output(), 1.0, now);
            now += 1.0 / 60.0;
        }
        playback.presenter_input();
        run(&mut playback, &steps, &mut app, &mut now, 1.0);
        assert_eq!((playback.phase.clone(), playback.paused_by()), (Phase::Paused, PausedBy::You));
        assert_eq!(playback.step, 1, "the click in flight finished, nothing began after it");
    }

    #[test]
    fn a_hold_in_flight_is_cut_short_by_a_seek_and_other_steps_run_out() {
        let steps = script();
        let (mut app, mut now) = (Fake::new(), 0.0);
        let mut playback = Playback::fresh(1);
        playback.toggle(steps.len(), 1, false);
        // Into the first hold.
        while playback.step < 2 || !playback.begun {
            app.now = now;
            playback.drive(&steps, &mut app, output(), 1.0, now);
            now += 1.0 / 60.0;
        }
        let started = now;
        playback.seek(4, steps.len(), 1, false);
        run(&mut playback, &steps, &mut app, &mut now, 1.0);
        assert_eq!((playback.step, app.presses), (4, 2));
        assert!(now - started < 1.2, "{}", now - started);
    }

    #[test]
    fn script_pauses_stop_a_play_but_not_a_replay() {
        let mut steps = script();
        steps.insert(2, DemoStep { step: Step::Pause, line: 0 });
        let (mut app, mut now) = (Fake::new(), 0.0);
        let mut playback = Playback::fresh(1);
        playback.toggle(steps.len(), 1, false);
        run(&mut playback, &steps, &mut app, &mut now, 1.0);
        assert_eq!((playback.step, playback.phase.clone(), playback.paused_by()), (3, Phase::Paused, PausedBy::Script));
        app.relaunch();
        playback.seek(6, steps.len(), 2, false);
        run(&mut playback, &steps, &mut app, &mut now, 1.0);
        assert_eq!((playback.step, playback.paused_by()), (6, PausedBy::Transport));
    }
}
