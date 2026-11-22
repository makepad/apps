//! Frame ownership for a scope stack. Channel indices stay independent of display order.

use super::{
    acquire::{LaneFrame, LaneSetup},
    feed::{FeedStats, ScopeFeed},
    phosphor::Hits,
    phosphor_view::{upload_rect, Pushed},
    scope::{FftSettings, ScopeAction, SignalLane, ScopeSettings},
    simfeed::{LimitedSpec, SimScopeFeed},
};
use fab_elec_bench::trigger::{EdgeTrigger, Qualifier, Window};
use makepad_widgets_core::{Cx, SpawnError};
use std::time::Instant;

/// UI transport totals since the last reset; pending publication stamps survive a reset.
#[derive(Default, Clone)]
pub struct TransportCounters {
    pub takes: u64,
    pub pushes: u64,
    pub busy: u64,
    pub hidden: u64,
    pub upload_bytes: u64,
    pub packed_bytes: u64,
    pub rects: u64,
    pub presented: Vec<u64>,
    pub latency_us: Vec<u32>,
    pub draws: u64,
}

struct Publication {
    at: Instant,
    pictures: Vec<(usize, bool)>,
}

pub fn volts(settings: &ScopeSettings, plot: (u32, u32)) -> (f64, f64) {
    let map = settings.map(plot);
    (map.v_top - map.v_px * f64::from(plot.1.max(1)), map.v_top)
}

/// One envelope and one pending bit per picture. Busy pictures gate the next mailbox take;
/// accepted, hidden or wrong-size pictures clear their bits. The worker accumulates new frames
/// meanwhile, and spent buffers travel back in the preceding envelope.
#[derive(Default)]
pub struct LaneDelivery {
    held: Option<LaneFrame>,
    published: Vec<Publication>,
    pending: Vec<bool>,
    spectrum_pending: Vec<bool>,
}

impl LaneDelivery {
    pub fn receive(&mut self, mut frame: LaneFrame) -> Option<LaneFrame> {
        debug_assert!(!self.waiting(), "pending pictures must finish before receiving another frame");
        self.published.clear();
        self.pending.resize(frame.lanes.len(), true);
        self.pending.fill(true);
        self.spectrum_pending.resize(frame.spectra.len(), false);
        for (pending, hits) in self.spectrum_pending.iter_mut().zip(&frame.spectra) { *pending = hits.is_some(); }
        if let Some(at) = frame.published.take() {
            let pictures = (0..frame.lanes.len()).map(|i| (i, false))
                .chain(frame.spectra.iter().enumerate().filter_map(|(i, hits)| hits.as_ref().map(|_| (i, true)))).collect();
            self.published.push(Publication { at, pictures });
        }
        self.held.replace(frame)
    }

    pub fn present(&mut self, mut push: impl FnMut(usize, bool, Hits) -> Pushed) {
        self.present_counted(&mut push, &mut Vec::new());
    }

    fn finish(published: &mut Vec<Publication>, channel: usize, spectrum: bool, taken: bool, latency_us: &mut Vec<u32>) {
        if published.is_empty() { return; }
        let now = taken.then(Instant::now);
        published.retain_mut(|stamp| {
            if !stamp.pictures.contains(&(channel, spectrum)) { return true; }
            if let Some(now) = now {
                latency_us.push(now.duration_since(stamp.at).as_micros().min(u32::MAX as u128) as u32);
                return false;
            }
            stamp.pictures.retain(|&picture| picture != (channel, spectrum));
            !stamp.pictures.is_empty()
        });
    }

    fn present_counted(&mut self, mut push: impl FnMut(usize, bool, Hits) -> Pushed, latency_us: &mut Vec<u32>) {
        let Some(frame) = &mut self.held else { return };
        for (i, lane) in frame.lanes.iter_mut().enumerate() {
            if !self.pending[i] { continue; }
            let hits = std::mem::replace(lane, Hits::new(0, 0));
            match push(i, false, hits) {
                Pushed::Taken(old) => { Self::finish(&mut self.published, i, false, true, latency_us); if let Some(old) = old { *lane = old; } self.pending[i] = false; }
                Pushed::Busy(hits) => *lane = hits,
                Pushed::WrongSize(hits) | Pushed::Hidden(hits) => { Self::finish(&mut self.published, i, false, false, latency_us); *lane = hits; self.pending[i] = false; }
            }
        }
        for (i, lane) in frame.spectra.iter_mut().enumerate() {
            if !self.spectrum_pending[i] { continue; }
            let Some(hits) = lane.take() else { continue };
            match push(i, true, hits) {
                Pushed::Taken(old) => { Self::finish(&mut self.published, i, true, true, latency_us); *lane = Some(old.unwrap_or_else(|| Hits::new(0, 0))); self.spectrum_pending[i] = false; }
                Pushed::Busy(hits) => *lane = Some(hits),
                Pushed::WrongSize(hits) | Pushed::Hidden(hits) => { Self::finish(&mut self.published, i, true, false, latency_us); *lane = Some(hits); self.spectrum_pending[i] = false; }
            }
        }
    }

    pub fn take(&mut self) -> Option<LaneFrame> {
        self.pending.clear();
        self.spectrum_pending.clear();
        self.published.clear();
        self.held.take()
    }

    /// A busy view still has hits to accept on a later frame.
    pub fn waiting(&self) -> bool { self.pending.iter().chain(&self.spectrum_pending).any(|&pending| pending) }
}

/// Owns the one feed of the bench, or the single feed of a preview/standalone lane.
pub struct ScopeHost<F: ScopeFeed> {
    feed: Option<F>,
    delivery: LaneDelivery,
    plot: Option<(u32, u32)>,
    spectrum: Option<(FftSettings, (u32, u32))>,
    stopped: bool,
    running: bool,
    single_complete: bool,
    counters: TransportCounters,
    drawing_id: Option<u64>,
    /// The drawing in which a view last accepted a picture. It shows it in the next drawing and is busy
    /// until then, so no frame is taken in this one: it would wait a whole frame in the envelope.
    accepted_in: Option<u64>,
    /// A take waited for that drawing: retry after it.
    retry_take: bool,
    latency_enabled: bool,
}

impl<F: ScopeFeed> Default for ScopeHost<F> {
    fn default() -> Self {
        Self { feed: None, delivery: LaneDelivery::default(), plot: None, spectrum: None, stopped: false, running: false, single_complete: false,
            counters: TransportCounters::default(), drawing_id: None, accepted_in: None, retry_take: false, latency_enabled: true }
    }
}

impl ScopeHost<SimScopeFeed> {
    /// Retune the installed simulation's ADC without replacing its feed.
    pub fn set_limited(&mut self, spec: LimitedSpec) {
        if !self.stopped {
            if let Some(feed) = &mut self.feed { feed.set_limited(spec); }
        }
    }
}

impl<F: ScopeFeed> ScopeHost<F> {
    pub fn counters(&self) -> &TransportCounters { &self.counters }

    pub fn reset_counters(&mut self) {
        let presented = std::mem::take(&mut self.counters.presented);
        let latency_us = std::mem::take(&mut self.counters.latency_us);
        self.counters = TransportCounters { presented, latency_us, ..TransportCounters::default() };
        self.counters.presented.fill(0);
        self.counters.latency_us.clear();
    }

    /// The drawing this call belongs to (`cx.drawing_id()`): counted once, and a view that accepts a
    /// picture in it holds the next take until a later drawing.
    pub fn note_draw(&mut self, drawing_id: u64) {
        if self.drawing_id != Some(drawing_id) {
            self.counters.draws += 1;
            self.drawing_id = Some(drawing_id);
        }
    }

    /// Disable timestamp collection when no report consumes the samples; integer totals stay on.
    pub(crate) fn set_latency_enabled(&mut self, enabled: bool) {
        self.latency_enabled = enabled;
        if !enabled { self.delivery.published.clear(); self.counters.latency_us.clear(); }
    }

    /// Replacing a layout returns its envelope and leaves every view's picture in place.
    pub fn install(&mut self, feed: F) {
        self.stop();
        self.feed = Some(feed);
        self.stopped = false;
        self.running = true;
    }

    pub fn sync_plot(&mut self, cx: &mut Cx, lane: &SignalLane,
        create: impl FnOnce(&mut Cx, (u32, u32), &ScopeSettings) -> Result<F, SpawnError>,
    ) -> Result<(), SpawnError> {
        let Some(plot) = lane.plot_px().filter(|&(w, h)| w > 0 && h > 0) else { return Ok(()) };
        if self.feed.is_none() || self.stopped {
            self.install(create(cx, plot, lane.settings())?);
            self.plot = Some(plot);
            self.configure(plot, lane.settings(), lane.spectrum_setup());
        } else if self.plot != Some(plot) || self.spectrum != lane.spectrum_setup() {
            self.configure(plot, lane.settings(), lane.spectrum_setup());
        }
        self.running = lane.settings().running;
        Ok(())
    }

    fn configure(&mut self, plot: (u32, u32), settings: &ScopeSettings, spectrum: Option<(FftSettings, (u32, u32))>) {
        if !self.stopped {
            if let Some(feed) = &mut self.feed {
                feed.reconfigure(settings.trigger(), settings.trigger.qualify, 0, settings.window(), vec![LaneSetup { map: settings.map(plot), spectrum, drawn: true }], settings.auto_after(), settings.trigger.zone);
                self.plot = Some(plot);
                self.spectrum = spectrum;
            }
        }
    }

    pub fn configure_lanes(&mut self, trigger: EdgeTrigger, qualifier: Qualifier, channel: usize, window: Window, lanes: Vec<LaneSetup>, auto: Option<f64>, zone: Option<super::acquire::Zone>) {
        if !self.stopped {
            if let Some(feed) = &mut self.feed { feed.reconfigure(trigger, qualifier, channel, window, lanes, auto, zone); }
        }
    }

    /// Retry held pictures before taking at most one mailbox frame, only when none still wait.
    /// True when a frame arrived or a view accepted hits.
    /// The callback maps a channel index to its lane; true selects its spectrum view.
    pub fn present_lanes(&mut self, mut push: impl FnMut(usize, bool, Hits) -> Pushed) -> bool {
        if !self.running() { return false; }
        let Some(feed) = &mut self.feed else { return false };
        if self.delivery.waiting() && self.delivery.held.as_ref().is_some_and(|frame| !feed.generation_is_current(frame.generation)) {
            if let Some(frame) = self.delivery.take() { feed.give_back(frame); }
        }
        let mut changed = false;
        let accepted = std::cell::Cell::new(false);
        let counters = &mut self.counters;
        let mut push_counted = |channel, spectrum, hits: Hits| {
            let rect = upload_rect(hits.dirty);
            let bytes = rect.size.width as u64 * rect.size.height as u64 * 4;
            let packed = hits.dirty.r0 != 0 || hits.dirty.r1 != hits.h;
            let waveforms = hits.waveforms;
            let pushed = push(channel, spectrum, hits);
            match &pushed {
                Pushed::Taken(_) => {
                    counters.pushes += 1;
                    counters.upload_bytes += bytes;
                    if packed { counters.packed_bytes += bytes; }
                    counters.rects += u64::from(bytes != 0);
                    if !spectrum {
                        if counters.presented.len() <= channel { counters.presented.resize(channel + 1, 0); }
                        counters.presented[channel] += waveforms;
                    }
                }
                Pushed::Busy(_) => counters.busy += 1,
                Pushed::Hidden(_) => counters.hidden += 1,
                Pushed::WrongSize(_) => {}
            }
            changed |= matches!(pushed, Pushed::Taken(_));
            accepted.set(accepted.get() | matches!(pushed, Pushed::Taken(_)));
            pushed
        };
        self.delivery.present_counted(&mut push_counted, &mut counters.latency_us);
        if accepted.get() { self.accepted_in = self.drawing_id; }
        let mut received = false;
        // A host told no drawing (note_draw) takes as soon as its pictures are delivered.
        let drawn = self.accepted_in.is_none_or(|at| self.drawing_id.is_some_and(|now| now > at));
        self.retry_take = !self.delivery.waiting() && !drawn;
        if !self.delivery.waiting() && drawn {
            if let Some(mut frame) = feed.take() {
                counters.takes += 1;
                if !self.latency_enabled { frame.published = None; }
                if let Some(old) = self.delivery.receive(frame) { feed.give_back(old); }
                accepted.set(false);
                self.delivery.present_counted(&mut push_counted, &mut counters.latency_us);
                if accepted.get() { self.accepted_in = self.drawing_id; }
                received = true;
            }
        }
        changed || received
    }

    pub fn present(&mut self, cx: &mut Cx, lane: &mut SignalLane) -> Option<usize> { self.present_source(cx, lane, 0) }

    /// Deliver one independently running source to the lane's current main/ghost selection.
    /// The source index stays fixed when the user swaps the chips.
    pub fn present_source(&mut self, cx: &mut Cx, lane: &mut SignalLane, source: usize) -> Option<usize> {
        self.note_draw(cx.drawing_id());
        self.running = !self.stopped && self.feed.is_some() && lane.settings().running;
        if source == lane.primary_source() { if let Some(feed) = &self.feed { lane.set_trigger_state(feed.trigger_state()); } }
        let mut uploaded = None;
        let mut complete = false;
        self.present_lanes(|_, spectrum, hits| {
            let single = lane.settings().single && hits.waveforms > 0;
            let clipped = hits.clipped;
            let pushed = if spectrum { lane.push_spectrum_frame(cx, source, hits) } else { lane.push_source_frame(cx, source, hits) };
            if !spectrum && matches!(pushed, Pushed::Taken(_)) && source == lane.primary_source() { lane.set_clipped(cx, clipped); }
            if matches!(pushed, Pushed::Taken(_)) {
                let primary = source == lane.primary_source();
                let view = if spectrum { lane.spectrum_view() } else { lane.phosphor() };
                uploaded = Some(uploaded.unwrap_or(0) + if primary { view.uploaded_bytes() } else { view.ghost_uploaded_bytes() }); complete |= single;
            }
            pushed
        });
        self.single_complete = lane.settings().single && (self.single_complete || complete);
        if self.single_complete && !self.delivery.waiting() {
            let mut settings = lane.settings().clone();
            settings.running = false;
            settings.single = false;
            lane.set_settings(cx, settings);
            self.running = false;
            self.single_complete = false;
        }
        uploaded
    }

    pub fn handle_action(&mut self, cx: &mut Cx, lane: &mut SignalLane, action: ScopeAction, light: bool) {
        match action {
            ScopeAction::Clear => { lane.phosphor().clear(cx); lane.spectrum_view().clear(cx); self.return_held(); }
            ScopeAction::Reconfigure(settings) => {
                if let Some(plot) = lane.plot_px() { self.configure(plot, &settings, lane.spectrum_setup()); }
                lane.phosphor().set_look(cx, settings.look(light));
                self.running = settings.running && !self.stopped && self.feed.is_some();
            }
            _ => {}
        }
    }

    /// A feed is installed, not stopped, and its lane is running.
    pub fn running(&self) -> bool { self.running }
    /// Only a busy view's pending hits need another UI frame.
    pub fn wants_frame(&self) -> bool { self.running() && (self.delivery.waiting() || self.retry_take) }
    pub fn plot(&self) -> Option<(u32, u32)> { self.plot }
    pub fn feed(&self) -> Option<&F> { self.feed.as_ref() }
    pub fn stats(&self) -> FeedStats { self.feed.as_ref().map_or_else(FeedStats::default, ScopeFeed::stats) }
    fn return_held(&mut self) {
        self.single_complete = false;
        if let (Some(feed), Some(held)) = (&mut self.feed, self.delivery.take()) { feed.give_back(held); }
    }
    pub fn stop(&mut self) {
        self.return_held();
        if !self.stopped { if let Some(feed) = &mut self.feed { feed.stop(); } }
        self.stopped = true;
        self.running = false;
    }
}

impl<F: ScopeFeed> Drop for ScopeHost<F> {
    fn drop(&mut self) { self.stop(); }
}
