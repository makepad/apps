//! A grown pattern's live run in the app (`Turing{}` in a mix, `fab_paint_field::pattern`): the
//! plate is built with each pattern's latest state and its simulation steps on in the task pool,
//! one job at a time, below the shape's own work. Each state it publishes rings the UI, which
//! puts it in the parts' paint views (the viewport bakes the newest once the one before is
//! baked), so the part shows the pattern growing and the UI never waits. Once every pattern on
//! the plate has grown to the steps it prints at, the model takes those states and the slice
//! that waited for them goes.

use fab_paint_field::pattern::live;
use fab_model::eval::Evaluated;
use makepad_widgets::makepad_platform::thread::{checkpoint, worker_wanted, Class, Ticket};
use makepad_widgets::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often the step counter in the status moves.
const STATUS_EVERY: Duration = Duration::from_millis(250);

#[derive(Default)]
pub struct PatternLive {
    serial: u64,
    /// A job steps the simulations now.
    working: Arc<AtomicBool>,
    /// A job ended (it may have left work for the next).
    ended: Arc<AtomicBool>,
    /// The plate (its revision) whose grown states were asked of the model.
    asked: Option<u64>,
    /// The plate (its revision) whose paint views were last brought up to their simulations.
    seen: Option<u64>,
    status_at: Option<Instant>,
}

/// What a signal moved.
#[derive(Default)]
pub struct Moved {
    /// A part's paint view shows a newer state.
    pub views: bool,
    /// Every pattern on the plate has grown: the model takes their states.
    pub grown: bool,
    /// The step counter is due.
    pub status: bool,
}

fn wake() {
    SignalToUI::set_ui_signal();
}

impl PatternLive {
    pub fn start() -> PatternLive {
        live::install_wake(wake);
        PatternLive::default()
    }

    /// A plate came: one that grows patterns is looked at on the next signal.
    pub fn plate_in(&self, plate: &Evaluated) {
        if plate.patterns_pending() {
            wake();
        }
    }

    /// On a signal: the newest states into the paint views, the simulations kept stepping, and
    /// whether the plate's patterns have all grown. A plate that came since is looked at too: it
    /// may hold an older state of a simulation that has grown on (to its end, even) meanwhile.
    pub fn signal(&mut self, cx: &mut Cx, state: &mut fab::AppState, plate: Option<&Arc<Evaluated>>, revision: u64) -> Moved {
        let serial = live::serial();
        let mut moved = Moved::default();
        let ended = self.ended.swap(false, Ordering::AcqRel);
        if serial == self.serial && !ended && self.seen == Some(revision) {
            return moved;
        }
        self.serial = serial;
        self.seen = Some(revision);
        for p in &mut state.plate.parts {
            let Some(view) = &mut p.paint_view else { continue };
            if !view.program.patterns().any(|g| g.sim.is_some()) {
                continue;
            }
            let mut key = view.key;
            let program = view.program.resolve_patterns(&mut |g| {
                let shown = live::shown(g);
                let (was, now) = (g.grid.as_ref().map(|x| x.key), shown.grid.as_ref().map(|x| x.key));
                if was != now {
                    key = (key ^ now.unwrap_or(0)).wrapping_mul(0x100_0000_01b3) | 1;
                }
                shown
            });
            if key != view.key {
                view.program = Arc::new(program);
                view.key = key;
                moved.views = true;
            }
        }
        if live::has_work() && !self.working.swap(true, Ordering::AcqRel) {
            let (working, ended) = (self.working.clone(), self.ended.clone());
            let submitted = cx.task_pool().submit_at(Class::Visible, "fab pattern growth", Ticket::none(), move || {
                // a step is whole on every worker (`par::fan` runs every index); a job of a
                // higher class takes the workers back between steps
                let stop = || checkpoint().is_err() || worker_wanted();
                live::work(&|n, f| makepad_fab_slicer::par::fan(n, f), &stop);
                working.store(false, Ordering::Release);
                ended.store(true, Ordering::Release);
                wake();
            });
            if submitted.is_err() {
                self.working.store(false, Ordering::Release);
            }
        }
        if let Some(ev) = plate.filter(|ev| ev.patterns_pending()) {
            if ev.patterns_ready() && self.asked != Some(revision) {
                self.asked = Some(revision);
                moved.grown = true;
            }
            if self.status_at.is_none_or(|t| t.elapsed() >= STATUS_EVERY) {
                self.status_at = Some(Instant::now());
                moved.status = true;
            }
        }
        moved
    }
}
