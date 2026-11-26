//! The document bar's presenter clock.

use makepad_widgets::*;

/// Wall time is sampled at minute boundaries; the two elapsed readouts
/// refresh once a second while presenting, so an open deck left alone draws
/// nothing. Neither formats text on the frame pump.
#[derive(Default)]
pub(super) struct PresenterClock {
    minute: Timer,
    second: Timer,
    pub target_seconds: f64,
}

impl PresenterClock {
    pub fn start(&mut self, cx: &mut Cx, ui: &WidgetRef) -> bool {
        if !self.minute.is_empty() {
            return false;
        }
        self.wall_time(cx, ui);
        true
    }

    /// The elapsed readouts refresh each second, or not. True when that
    /// changed.
    pub fn set_ticking(&mut self, cx: &mut Cx, ticking: bool) -> bool {
        if ticking == !self.second.is_empty() {
            return false;
        }
        cx.stop_timer(self.second);
        self.second = if ticking { cx.start_interval(1.0) } else { Timer::empty() };
        true
    }

    fn wall_time(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        let now = makepad_civil_time::local_now();
        ui.label(cx, ids!(deck_clock)).set_text(cx, &now.hm());
        self.minute = cx.start_timeout(f64::from(60 - now.second));
    }

    /// True when the deck and slide elapsed times need reading.
    pub fn handle_event(&mut self, cx: &mut Cx, ui: &WidgetRef, event: &Event) -> bool {
        if self.minute.is_event(event).is_some() {
            self.wall_time(cx, ui);
        }
        self.second.is_event(event).is_some()
    }

    /// The bar's readout: the time left in the talk, large (the overrun in
    /// the accent colour past its length), the slide's small beside it.
    /// Returns the talk's line for the presenting corner readout.
    pub fn update(&self, cx: &mut Cx, ui: &WidgetRef, total: f64, slide: f64, target: Option<f64>) -> (String, bool) {
        let palette = makepad_look::palette::get(cx);
        let (line, over) = if self.target_seconds > 0.0 { left(self.target_seconds, total) } else { (elapsed(total), false) };
        let deck = ui.label(cx, ids!(deck_total_time));
        deck.set_text(cx, &line);
        deck.set_text_color(cx, Vec4f::from_u32(if over { palette.accent } else { palette.text_hi }));
        let time = ui.label(cx, ids!(deck_slide_time));
        let (slide_line, slide_over) = match target {
            Some(target) => left(target, slide),
            None => (elapsed(slide), false),
        };
        time.set_text(cx, &format!("slide {slide_line}"));
        time.set_text_color(cx, Vec4f::from_u32(if slide_over { palette.accent } else { palette.muted }));
        (line, over)
    }

    pub fn stop(&mut self, cx: &mut Cx) {
        cx.stop_timer(self.minute);
        cx.stop_timer(self.second);
        self.minute = Timer::empty();
        self.second = Timer::empty();
    }
}

/// Released beats are filled; the pending holds stay open.
pub fn beats(done: usize, count: usize) -> String {
    (0..count).map(|beat| if beat < done { "●" } else { "○" }).collect::<Vec<_>>().join(" ")
}

/// What is left of `target` seconds after `spent`: "12:34 left", or past
/// it "+1:05 over" (true: over). Whole seconds; the last second counts as
/// left until it has passed.
pub fn left(target: f64, spent: f64) -> (String, bool) {
    let remaining = target.max(0.0).round() - spent.max(0.0).floor();
    if remaining > 0.0 {
        (format!("{} left", elapsed(remaining)), false)
    } else if remaining == 0.0 {
        ("0:00 left".into(), false)
    } else {
        (format!("+{} over", elapsed(-remaining)), true)
    }
}

/// The talk's clock and the shown slide's, on the host's clock. The one
/// place the presenter's elapsed times are worked out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TalkClock {
    started_at: f64,
    slide_started_at: f64,
}

impl TalkClock {
    pub fn new(now: f64) -> Self {
        Self { started_at: now, slide_started_at: now }
    }

    /// The talk and the slide start again at 0.
    pub fn reset(&mut self, now: f64) {
        *self = Self::new(now);
    }

    /// Another slide is shown (or the shown one restarts).
    pub fn enter_slide(&mut self, now: f64) {
        self.slide_started_at = now;
    }

    /// Seconds into the talk and into the shown slide.
    pub fn elapsed(&self, now: f64) -> (f64, f64) {
        ((now - self.started_at).max(0.0), (now - self.slide_started_at).max(0.0))
    }
}

pub fn elapsed(seconds: f64) -> String {
    let seconds = seconds.max(0.0) as u64;
    if seconds >= 3600 {
        format!("{}:{:02}:{:02}", seconds / 3600, seconds / 60 % 60, seconds % 60)
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

#[cfg(test)]
mod clock_tests {
    use super::*;

    #[test]
    fn time_left_counts_down_then_shows_the_overrun() {
        assert_eq!(left(754.0, 0.0), ("12:34 left".to_string(), false));
        assert_eq!(left(600.0, 59.9), ("9:01 left".to_string(), false));
        assert_eq!(left(600.0, 600.4), ("0:00 left".to_string(), false));
        assert_eq!(left(600.0, 665.2), ("+1:05 over".to_string(), true));
        assert_eq!(left(3600.0, 0.0), ("1:00:00 left".to_string(), false));
        assert_eq!(left(60.0, 3700.0), ("+1:00:40 over".to_string(), true));
    }

    #[test]
    fn reset_starts_the_talk_and_the_slide_again() {
        let mut clock = TalkClock::new(100.0);
        clock.enter_slide(160.0);
        assert_eq!(clock.elapsed(190.0), (90.0, 30.0));
        clock.reset(200.0);
        assert_eq!(clock.elapsed(200.0), (0.0, 0.0));
        assert_eq!(clock.elapsed(212.5), (12.5, 12.5));
        // Before the clock's start (a clock jump) reads 0, never negative.
        assert_eq!(clock.elapsed(150.0), (0.0, 0.0));
    }
}
