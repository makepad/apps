//! What a plate switch costs the main thread. From the ask (a plate or demo opened, a new plate,
//! the AI's switch) until a second and a half after the plate shows, each app event's handling
//! is timed; the first event after that logs one line (`fab switch: ...`): how long the plate
//! took to show, how busy the app kept the main thread and its longest event, with the steps
//! marked in it ([`mark`]). The platform's own work between app events (presenting, uploads) is
//! not in it.

use makepad_widgets::*;
use std::cell::RefCell;

/// How long after the plate shows its events are still counted (s).
const AFTER: f64 = 1.5;

struct Watch {
    asked: f64,
    shown: Option<f64>,
    events: u32,
    draws: u32,
    busy_ms: f64,
    longest: (f64, &'static str),
    /// The longest event's marked steps: (step, ms since the one before).
    longest_steps: Vec<(&'static str, f64)>,
}

impl Watch {
    fn new() -> Watch {
        Watch { asked: Cx::time_now(), shown: None, events: 0, draws: 0, busy_ms: 0.0, longest: (0.0, ""), longest_steps: Vec::new() }
    }
}

thread_local! {
    static WATCH: RefCell<Option<Watch>> = const { RefCell::new(None) };
    /// The steps marked in the event being timed: (step, when).
    static STEPS: RefCell<Vec<(&'static str, f64)>> = const { RefCell::new(Vec::new()) };
}

/// A step of the event being timed ends here (nothing unless a switch is watched).
pub fn mark(step: &'static str) {
    if WATCH.with(|w| w.borrow().is_some()) {
        STEPS.with(|s| s.borrow_mut().push((step, Cx::time_now())));
    }
}

/// Another document was asked for the bed (a later ask starts the watch again).
pub fn asked() {
    WATCH.with(|w| *w.borrow_mut() = Some(Watch::new()));
}

/// The switched plate is on the bed: its scene installs in this event.
pub fn shown() {
    WATCH.with(|w| {
        let mut w = w.borrow_mut();
        let w = w.get_or_insert_with(Watch::new);
        w.shown.get_or_insert_with(Cx::time_now);
    });
}

/// One app event timed while a switch is watched; it counts when dropped.
pub struct Timed(Option<(&'static str, f64)>);

pub fn event(event: &Event) -> Timed {
    if !WATCH.with(|w| w.borrow().is_some()) {
        return Timed(None);
    }
    let kind = match event {
        Event::Draw(_) => "draw",
        Event::NextFrame(_) => "next frame",
        Event::Signal => "signal",
        Event::Timer(_) => "timer",
        Event::Actions(_) => "actions",
        Event::MouseMove(_) | Event::MouseDown(_) | Event::MouseUp(_) => "mouse",
        _ => "other",
    };
    STEPS.with(|s| s.borrow_mut().clear());
    Timed(Some((kind, Cx::time_now())))
}

impl Drop for Timed {
    fn drop(&mut self) {
        let Some((kind, at)) = self.0 else { return };
        let end = Cx::time_now();
        WATCH.with(|slot| {
            let mut slot = slot.borrow_mut();
            let Some(w) = slot.as_mut() else { return };
            let ms = (end - at) * 1e3;
            w.events += 1;
            w.draws += (kind == "draw") as u32;
            w.busy_ms += ms;
            if ms > w.longest.0 {
                w.longest = (ms, kind);
                let mut last = at;
                w.longest_steps = STEPS.with(|s| s.borrow().iter().map(|&(step, t)| (step, (t - std::mem::replace(&mut last, t)) * 1e3)).collect());
                w.longest_steps.push(("rest", (end - last) * 1e3));
            }
            let Some(shown) = w.shown.filter(|s| end - s > AFTER) else { return };
            let steps: Vec<String> = w.longest_steps.iter().filter(|(_, ms)| *ms >= 1.0).map(|(step, ms)| format!("{step} {ms:.0}")).collect();
            log!(
                "fab switch: shown {:.0} ms after the ask; {} events ({} draws) in the {:.1} s after it, the app busy {:.0} ms, its longest event {} {:.1} ms ({})",
                (shown - w.asked) * 1e3,
                w.events,
                w.draws,
                end - w.asked,
                w.busy_ms,
                w.longest.1,
                w.longest.0,
                steps.join(", ")
            );
            *slot = None;
        });
    }
}
