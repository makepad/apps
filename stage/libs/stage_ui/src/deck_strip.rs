//! One deck strip: labels, transport, speed, cue, and loop.
//!
//! The strip only draws and reports edits. A Rust app binds
//! [`DeckStripAction`] to its flow deck.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    mod.widgets.DeckStripBase = #(DeckStrip::register_widget(vm))
    mod.widgets.DeckStrip = set_type_default() do mod.widgets.DeckStripBase{
        width: Fill
        height: Fit
        flow: Down
        spacing: 6
        padding: Inset{left: 10 right: 10 top: 8 bottom: 8}
        show_bg: true
        draw_bg: mod.widgets.RoundedView.draw_bg{
            color: theme.color_surface_container
            border_color: theme.color_outline_variant
            border_size: 1.0
            border_radius: 8.0
        }
        title := Label{
            width: Fill height: 16 padding: 0 text: "Deck"
            draw_text +: {color: theme.color_on_surface text_style: theme.font_bold{font_size: 10}}
        }
        active_title := Label{
            width: Fill height: 18 padding: 0 text: "No track"
            draw_text +: {color: theme.color_on_surface}
        }
        status := Label{
            width: Fill height: 18 padding: 0 visible: false
            draw_text +: {color: theme.color_on_surface_variant}
        }
        waveform := Waveform{width: Fill height: 56 marker_strip: 0 show_names: false}
        View{
            width: Fill height: Fit flow: Right spacing: 8 align: Align{y: 0.5}
            play := CheckBox{width: Fit text: "Play"}
            cue := ButtonFlatter{width: Fit height: 24 text: "Cue"}
            eject := ButtonFlatter{width: Fit height: 24 text: "Eject"}
            position := Label{
                width: Fit height: Fit padding: 0 text: "0:00 / 0:00"
                draw_text +: {color: theme.color_on_surface}
            }
        }
        View{width: Fill height: Fit flow: Right spacing: 8
            speed := Slider{width: Fill height: 36 margin: 0 min: 0.25 max: 4 default: 1 text: "Speed"}
            cue_at := Slider{width: Fill height: 36 margin: 0 min: 0 max: 36000 default: 0 text: "Cue"}
        }
        loop_enabled := CheckBox{width: Fit text: "Loop"}
        View{width: Fill height: Fit flow: Right spacing: 8
            loop_in := Slider{width: Fill height: 36 margin: 0 min: 0 max: 36000 default: 0 text: "In"}
            loop_out := Slider{width: Fill height: 36 margin: 0 min: 0 max: 36000 default: 0 text: "Out"}
        }
    }
}

/// Edits from the strip. Applying [`DeckStrip::set_state`] does not emit these.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum DeckStripAction {
    Play(bool),
    Cue,
    Eject,
    Speed(f64),
    CueAt(f64),
    LoopEnabled(bool),
    LoopIn(f64),
    LoopOut(f64),
    Seek(f64),
    /// A hand on the record at this many seconds (the first one takes hold).
    Scratch(f64),
    /// The hand let go: the record plays on (or stops) from where it is.
    ScratchEnd,
    /// Swap what the two decks play, each keeping its place and transport.
    SwapDecks,
    #[default]
    None,
}

/// The accepted identity of a paged (streamed) deck source: an opaque
/// source id unique within the application session, and the seek serial the
/// deck last accepted. A cue on the same source is settled when `serial`
/// grows; a different `source` is a replacement. Growth of the source's
/// duration is neither.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PagedTransportStamp {
    pub source: u64,
    pub serial: u64,
}

/// A deck's saved beat grid: one straight tempo line, in source seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeckBeat {
    pub beat_secs: f64,
    /// A downbeat, so bars rule where the music's bars are.
    pub downbeat_secs: f64,
}

/// What the strip shows. `position` and `duration` are seconds.
/// Cue and loop sliders use that duration as their max when a track is ready.
#[derive(Clone, Debug, PartialEq)]
pub struct DeckStripState {
    pub title: String,
    pub active_title: String,
    pub status: String,
    pub ready: bool,
    pub has_track: bool,
    pub playing: bool,
    pub speed: f64,
    pub cue_at: f64,
    pub loop_enabled: bool,
    pub loop_in: f64,
    pub loop_out: f64,
    pub position: f64,
    pub duration: f64,
    /// Set for a paged source: `position` is then the accepted observation,
    /// shown as is (never extrapolated from arrival time).
    pub paged: Option<PagedTransportStamp>,
    /// The saved beat grid of the loaded track, when it has one.
    pub beat: Option<DeckBeat>,
}

#[derive(Script, ScriptHook, Widget)]
pub struct DeckStrip {
    #[deref]
    view: View,
    #[rust]
    state: Option<DeckStripState>,
    #[rust]
    waveform_dragging: bool,
    /// Last peaks handed to the lane. Identity is `Arc::ptr_eq`; the vec is cloned once per change.
    #[rust]
    peaks: Option<std::sync::Arc<Vec<(f32, f32)>>>,
    /// False after script reapply so the same `Arc` is written into the lane again.
    #[rust]
    peaks_applied: bool,
}

impl DeckStrip {
    pub fn set_state(&mut self, cx: &mut Cx, state: &DeckStripState) {
        let prev = self.state.clone();
        let transport = state.ready && state.has_track;
        let prev_transport = prev.as_ref().map(|p| p.ready && p.has_track);

        if prev.as_ref().map(|p| p.title.as_str()) != Some(state.title.as_str()) {
            self.label(cx, ids!(title)).set_text(cx, &state.title);
        }
        if prev.as_ref().map(|p| p.active_title.as_str()) != Some(state.active_title.as_str()) {
            self.label(cx, ids!(active_title)).set_text(cx, &state.active_title);
        }
        if prev.as_ref().map(|p| p.status.as_str()) != Some(state.status.as_str()) {
            self.label(cx, ids!(status)).set_text(cx, &state.status);
            self.widget(cx, ids!(status)).set_visible(cx, !state.status.is_empty());
        }
        let position_changed = prev.as_ref().map(|p| clock_secs(p.position) != clock_secs(state.position) || clock_secs(p.duration) != clock_secs(state.duration)).unwrap_or(true);
        if position_changed {
            self.label(cx, ids!(position)).set_text(cx, &clock_pair(state.position, state.duration));
        }
        if prev.as_ref().map(|p| p.playing) != Some(state.playing) {
            self.check_box(cx, ids!(play)).set_active(cx, state.playing, Animate::No);
        }
        let duration_changed = prev.as_ref().map(|p| p.duration) != Some(state.duration);
        if duration_changed {
            self.waveform(cx, ids!(waveform)).set_duration(cx, state.duration);
        }
        if !transport {
            self.waveform_dragging = false;
        }
        // A gesture can change the widget even when the engine keeps the
        // same position (a refused seek, or wrapping into the same loop).
        // The waveform setter already avoids redraws for unchanged values.
        if !self.waveform_dragging {
            self.waveform(cx, ids!(waveform)).set_playhead(cx, state.position);
        }
        if prev_transport != Some(transport) {
            self.widget(cx, ids!(waveform)).set_disabled(cx, !transport);
            self.widget(cx, ids!(play)).set_disabled(cx, !transport);
            self.widget(cx, ids!(cue)).set_disabled(cx, !transport);
            self.widget(cx, ids!(eject)).set_disabled(cx, !transport);
            self.widget(cx, ids!(cue_at)).set_disabled(cx, !transport);
            self.widget(cx, ids!(loop_enabled)).set_disabled(cx, !transport);
            self.widget(cx, ids!(loop_in)).set_disabled(cx, !transport);
            self.widget(cx, ids!(loop_out)).set_disabled(cx, !transport);
        }
        if prev.as_ref().map(|p| p.ready) != Some(state.ready) {
            self.widget(cx, ids!(speed)).set_disabled(cx, !state.ready);
        }
        if prev.as_ref().map(|p| p.speed) != Some(state.speed) {
            self.slider(cx, ids!(speed)).set_value(cx, state.speed.clamp(0.25, 4.0));
        }
        let maximum = seek_max(state);
        let bounds_changed = prev.as_ref().map(seek_max) != Some(maximum);
        if bounds_changed {
            let mut cue_at = self.widget(cx, ids!(cue_at));
            let mut loop_in = self.widget(cx, ids!(loop_in));
            let mut loop_out = self.widget(cx, ids!(loop_out));
            script_apply_eval!(cx, cue_at, { max: #(maximum) });
            script_apply_eval!(cx, loop_in, { max: #(maximum) });
            script_apply_eval!(cx, loop_out, { max: #(maximum) });
        }
        if bounds_changed || prev.as_ref().map(|p| p.cue_at) != Some(state.cue_at) {
            self.slider(cx, ids!(cue_at)).set_value(cx, state.cue_at.clamp(0.0, maximum));
        }
        if prev.as_ref().map(|p| p.loop_enabled) != Some(state.loop_enabled) {
            self.check_box(cx, ids!(loop_enabled)).set_active(cx, state.loop_enabled, Animate::No);
        }
        if bounds_changed || prev.as_ref().map(|p| p.loop_in) != Some(state.loop_in) {
            self.slider(cx, ids!(loop_in)).set_value(cx, state.loop_in.clamp(0.0, maximum));
        }
        if bounds_changed || prev.as_ref().map(|p| p.loop_out) != Some(state.loop_out) {
            self.slider(cx, ids!(loop_out)).set_value(cx, state.loop_out.clamp(0.0, maximum));
        }
        if bounds_changed {
            // The same normalized travel can mean a different time after
            // replacing a track. Refresh even when set_value did not move it.
            for id in [live_id!(cue_at), live_id!(loop_in), live_id!(loop_out)] {
                if let Some(mut slider) = self.slider(cx, &[id]).borrow_mut() {
                    slider.update_text_input(cx);
                }
            }
            self.view.redraw(cx);
        }
        self.state = Some(state.clone());
    }

    pub fn set_waveform(&mut self, cx: &mut Cx, peaks: Option<std::sync::Arc<Vec<(f32, f32)>>>) {
        let same = self.peaks_applied && match (&self.peaks, &peaks) {
            (Some(prev), Some(next)) => std::sync::Arc::ptr_eq(prev, next),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        match &peaks {
            Some(peaks) => self.waveform(cx, ids!(waveform)).set_peak_pairs(cx, (**peaks).clone()),
            None => self.waveform(cx, ids!(waveform)).clear_peaks(cx),
        }
        self.peaks = peaks;
        self.peaks_applied = true;
    }

    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let (ready, transport, max) = match self.state.as_ref() {
            Some(state) => (state.ready, state.ready && state.has_track, seek_max(state)),
            None => (false, false, 36000.0),
        };
        if let Some(playing) = self.check_box(cx, ids!(play)).changed(actions) {
            if transport {
                cx.widget_action(self.widget_uid(), DeckStripAction::Play(playing));
            }
        }
        if self.button(cx, ids!(cue)).clicked(actions) && transport {
            cx.widget_action(self.widget_uid(), DeckStripAction::Cue);
        }
        if self.button(cx, ids!(eject)).clicked(actions) && transport {
            cx.widget_action(self.widget_uid(), DeckStripAction::Eject);
        }
        if let Some(speed) = self.slider(cx, ids!(speed)).slided(actions) {
            if ready {
                cx.widget_action(self.widget_uid(), DeckStripAction::Speed(speed.clamp(0.25, 4.0)));
            }
        }
        if let Some(cue_at) = self.slider(cx, ids!(cue_at)).slided(actions) {
            if transport {
                cx.widget_action(self.widget_uid(), DeckStripAction::CueAt(cue_at.clamp(0.0, max)));
            }
        }
        if let Some(enabled) = self.check_box(cx, ids!(loop_enabled)).changed(actions) {
            if transport {
                cx.widget_action(self.widget_uid(), DeckStripAction::LoopEnabled(enabled));
            }
        }
        if let Some(loop_in) = self.slider(cx, ids!(loop_in)).slided(actions) {
            if transport {
                cx.widget_action(self.widget_uid(), DeckStripAction::LoopIn(loop_in.clamp(0.0, max)));
            }
        }
        if let Some(loop_out) = self.slider(cx, ids!(loop_out)).slided(actions) {
            if transport {
                cx.widget_action(self.widget_uid(), DeckStripAction::LoopOut(loop_out.clamp(0.0, max)));
            }
        }
        if self.waveform(cx, ids!(waveform)).grabbed(actions) == Some(WaveformPart::Playhead) {
            self.waveform_dragging = true;
        }
        if let Some(at) = self.waveform(cx, ids!(waveform)).seeked(actions) {
            self.waveform_dragging = false;
            if transport {
                let duration = self.state.as_ref().map(|state| state.duration).unwrap_or(0.0);
                let at = if duration.is_finite() {
                    at.clamp(0.0, duration.max(0.0))
                } else {
                    0.0
                };
                cx.widget_action(self.widget_uid(), DeckStripAction::Seek(at));
            }
        }
    }
}

impl DeckStripRef {
    /// Restore authoritative values on the next sync after a refused edit.
    pub fn invalidate_state(&self) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.state = None;
            inner.waveform_dragging = false;
        }
    }

    pub fn set_state(&self, cx: &mut Cx, state: &DeckStripState) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_state(cx, state);
        }
    }

    pub fn set_waveform(&self, cx: &mut Cx, peaks: Option<std::sync::Arc<Vec<(f32, f32)>>>) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_waveform(cx, peaks);
        }
    }
}

impl Widget for DeckStrip {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if matches!(event, Event::ScriptReapply) {
            self.state = None;
            self.waveform_dragging = false;
            self.peaks_applied = false;
        }
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            self.handle_actions(cx, actions);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

fn seek_max(state: &DeckStripState) -> f64 {
    if state.ready && state.has_track && state.duration.is_finite() && state.duration > 0.0 {
        state.duration.min(36000.0)
    } else {
        36000.0
    }
}

fn clock_pair(position: f64, duration: f64) -> String {
    format!("{} / {}", clock(position), clock(duration))
}

fn clock_secs(seconds: f64) -> u64 {
    if seconds.is_finite() { seconds.max(0.0).floor() as u64 } else { 0 }
}

fn clock(seconds: f64) -> String {
    let whole = clock_secs(seconds);
    format!("{}:{:02}", whole / 60, whole % 60)
}
