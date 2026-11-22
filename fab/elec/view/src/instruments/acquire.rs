//! Rasterise acquisition windows as accepted solver samples arrive.
//! An open spectrum lens adds one more raster stroke per capture and one more
//! dirty-rectangle upload per frame per open cell. A closed lens costs nothing.

use super::{events::{EventList, EventLog}, phosphor::{Hits, PhosphorMap, Stroke}, scope::FftSettings, spectrum::{SpectrumResult, SpectrumStroke}};
use super::span::SpanRequest;
use super::trend::{TrendBound, TrendQuantity, TrendRecorder, TrendSeries, Violation, ViolationList};
use fab_elec_bench::source::{Block, ChanId, ChannelInfo, ClockKind, Gap, Intake, Layout, Mark, Quantity};
use fab_elec_bench::trigger::{Edge, EdgeDetector, EdgeTrigger, Qualifier, Slope, Window};
use fab_elec_sim::stamp::{UnkKind, UnknownInfo};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Read `(x[pos] - x[neg]) * scale` from a run's unknowns; index zero is ground.
pub struct Probe {
    pub pos: usize,
    pub neg: usize,
    pub scale: f64,
}

/// A probe by net names, resolved against each lowering; no negative net means ground.
#[derive(Clone, Debug)]
pub struct NetProbe {
    pub pos: String,
    pub neg: Option<String>,
    pub scale: f64,
}

/// Per-channel axes and optional spectrum, all sharing the trigger's time window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LaneSetup { pub map: PhosphorMap, pub spectrum: Option<(FftSettings, (u32, u32))>, pub drawn: bool }

impl LaneSetup {
    pub fn time(map: PhosphorMap) -> Self { Self { map, spectrum: None, drawn: true } }
}

/// A box in trigger-relative seconds and lane units on one acquisition lane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Zone { pub lane: usize, pub t0: f64, pub t1: f64, pub v0: f64, pub v1: f64, pub must: bool }

impl Zone {
    pub fn valid(self) -> bool {
        [self.t0, self.t1, self.v0, self.v1].into_iter().all(f64::is_finite) && self.t0 < self.t1 && self.v0 < self.v1
    }

    fn clipped(self, window: Window, lanes: usize) -> Option<Self> {
        let zone = Self { t0: self.t0.max(-window.pre), t1: self.t1.min(window.post), ..self };
        (self.valid() && self.lane < lanes && zone.t0 < zone.t1).then_some(zone)
    }

    fn intersects(self, event: f64, a: (f64, f32), b: (f64, f32)) -> bool {
        if !a.1.is_finite() || !b.1.is_finite() || b.0 <= a.0 { return false; }
        let lo = a.0.max(event + self.t0);
        let hi = b.0.min(event + self.t1);
        if lo > hi { return false; }
        let value = |t| a.1 as f64 + (b.1 as f64 - a.1 as f64) * ((t - a.0) / (b.0 - a.0));
        let (a, b) = (value(lo), value(hi));
        a.min(b) <= self.v1 && a.max(b) >= self.v0
    }
}

#[derive(Clone, Copy)]
struct WaitingZone { time: f64, side: usize, zone: Zone, replay: bool }

/// All columns share one circular index and capacity; trimming is O(1), independent of K.
struct TimeRing { data: Vec<f64>, head: usize, len: usize }

impl TimeRing {
    fn len(&self) -> usize { self.len }
    fn get(&self, i: usize) -> Option<&f64> { if i < self.len { Some(&self.data[(self.head + i) & (self.data.len() - 1)]) } else { None } }
    fn front(&self) -> Option<&f64> { self.get(0) }
    fn back(&self) -> Option<&f64> { self.len.checked_sub(1).and_then(|i| self.get(i)) }
    fn iter(&self) -> impl DoubleEndedIterator<Item = &f64> + ExactSizeIterator { (0..self.len).map(|i| &self[i]) }
    fn partition_point(&self, mut before: impl FnMut(&f64) -> bool) -> usize {
        let (mut lo, mut hi) = (0, self.len);
        while lo < hi { let mid = lo + (hi - lo) / 2; if before(&self[mid]) { lo = mid + 1; } else { hi = mid; } }
        lo
    }
}

impl std::ops::Index<usize> for TimeRing {
    type Output = f64;
    fn index(&self, i: usize) -> &f64 { &self.data[(self.head + i) & (self.data.len() - 1)] }
}

struct SampleRing { times: TimeRing, values: Vec<Vec<f32>> }

impl SampleRing {
    fn new(lanes: usize) -> Self { Self { times: TimeRing { data: Vec::new(), head: 0, len: 0 }, values: (0..lanes).map(|_| Vec::new()).collect() } }
    fn clear(&mut self) { self.times.head = 0; self.times.len = 0; }
    fn value(&self, lane: usize, i: usize) -> f32 { self.values[lane][(self.times.head + i) & (self.times.data.len() - 1)] }
    fn push<const SINGLE: bool>(&mut self, t: f64, y: &[f32]) {
        if self.times.len == self.times.data.len() { self.grow(); }
        let i = (self.times.head + self.times.len) & (self.times.data.len() - 1);
        self.times.data[i] = t;
        if SINGLE { self.values[0][i] = y[0]; }
        else { for (column, &v) in self.values.iter_mut().zip(y) { column[i] = v; } }
        self.times.len += 1;
    }
    #[cold]
    fn grow(&mut self) {
        let capacity = (self.times.data.len() * 2).max(4);
        self.times.data.rotate_left(self.times.head);
        self.times.data.resize(capacity, 0.0);
        for column in &mut self.values { column.rotate_left(self.times.head); column.resize(capacity, 0.0); }
        self.times.head = 0;
    }
    fn pop_front(&mut self) { self.times.head = (self.times.head + 1) & (self.times.data.len() - 1); self.times.len -= 1; }
}

/// A lane's samples over one drawing block: the first, last, lowest and highest, and whether all
/// are finite (segments skip a non-finite sample, so a block holding one draws sample by sample).
#[derive(Clone, Copy, Debug)]
struct Fold { first: f32, last: f32, lo: f32, hi: f32, finite: bool }

impl Fold {
    fn of(y: f32) -> Self { Self { first: y, last: y, lo: y, hi: y, finite: y.is_finite() } }
    fn add(&mut self, y: f32) {
        self.last = y;
        self.lo = self.lo.min(y);
        self.hi = self.hi.max(y);
        self.finite &= y.is_finite();
    }
}

/// The time rasters' unit of drawing: every lane's samples over at most `width` seconds of
/// absolute time. A block reaches each open window once, when it closes, so drawing costs blocks,
/// not samples, however many windows are open. Inside one pixel column of a window, or outside the
/// window, a block's segments add exactly its fold's span there (`Stroke::fold`); across a column
/// edge the window draws the block's samples one by one. Either way a window gets the bits a
/// sample-by-sample draw gives. Triggers, zones, spectra and captures still read every sample.
/// Closed blocks keep their folds for a new window's replay as long as the ring keeps their samples.
struct Blocks {
    /// Seconds per block: an eighth of the finest drawn lane's pixel column (`fold_width`), so
    /// about one block in eight crosses a column edge; 0 makes a block of every sample.
    width: f64,
    /// The open block: its index (`floor(t / width)`), first and last sample time; per lane its fold
    /// (`open`), and its samples (`times`, and `values` sample-major) for windows it crosses.
    at: Option<(f64, f64, f64)>,
    open: Vec<Fold>,
    times: Vec<f64>,
    values: Vec<f32>,
    /// Closed blocks: first and last sample time, and per lane its fold.
    closed: VecDeque<(f64, f64)>,
    folds: Vec<VecDeque<Fold>>,
}

impl Blocks {
    fn new(lanes: &[LaneSetup]) -> Self {
        Self {
            width: fold_width(lanes), at: None, open: Vec::with_capacity(lanes.len()), times: Vec::new(), values: Vec::new(),
            closed: VecDeque::new(), folds: (0..lanes.len()).map(|_| VecDeque::new()).collect(),
        }
    }
    fn index(&self, t: f64) -> f64 { if self.width > 0.0 { (t / self.width).floor() } else { t } }
    fn clear(&mut self) {
        self.at = None;
        self.closed.clear();
        for folds in &mut self.folds { folds.clear(); }
    }
    /// Adds a sample to the open block (opening one when none is). O(lanes).
    fn push(&mut self, t: f64, y: &[f32]) {
        match &mut self.at {
            Some((_, _, last)) => {
                *last = t;
                for (fold, &v) in self.open.iter_mut().zip(y) { fold.add(v); }
            }
            None => {
                self.at = Some((self.index(t), t, t));
                self.open.clear();
                self.open.extend(y.iter().map(|&v| Fold::of(v)));
                self.times.clear();
                self.values.clear();
            }
        }
        self.times.push(t);
        self.values.extend_from_slice(y);
    }
    /// Lane `c`'s last sample before closed block `i` (its previous block's last), if kept.
    fn before(&self, c: usize, i: usize) -> Option<(f64, f32)> {
        let p = i.checked_sub(1)?;
        Some((self.closed[p].1, self.folds[c][p].last))
    }
}

/// Blocks an eighth of the finest drawn time lane's pixel column wide; 0 (a block per sample)
/// without one.
fn fold_width(lanes: &[LaneSetup]) -> f64 {
    let finest = lanes.iter().filter(|lane| lane.drawn && lane.map.w > 0).map(|lane| lane.map.t_px).fold(f64::INFINITY, f64::min);
    if finest.is_finite() && finest > 0.0 { finest / 8.0 } else { 0.0 }
}

struct LaneRaster { time: Stroke, spectrum: Option<SpectrumStroke>, drawn: bool }

impl LaneRaster {
    /// The time raster's segment into a block's first sample from the sample before it, or the
    /// lone first sample without one.
    #[inline]
    fn join(&mut self, previous: Option<(f64, f32)>, t: f64, y: f32) {
        if !self.drawn { return; }
        match previous {
            Some((t0, y0)) => if y0.is_finite() && y.is_finite() { self.time.segment(t0, y0, t, y); },
            None => if y.is_finite() { self.time.point(t, y); },
        }
    }
    /// The time raster's segments inside a block after its first sample: its fold where that is
    /// exact and no sample reaches `stop`, else its `samples` one by one (`walk`).
    #[inline]
    fn body<I: Iterator<Item = (f64, f32)>>(&mut self, t0: f64, t1: f64, fold: &Fold, stop: f64, samples: impl FnOnce() -> I) {
        if !self.drawn { return; }
        if fold.finite && t1 < stop && self.time.fold(t0, t1, fold.lo, fold.hi, fold.last) { return; }
        self.walk(samples(), stop);
    }
    /// The time raster's segments between consecutive `samples` (the first drawn already), through
    /// the first at or past `stop`, where a window completes.
    fn walk(&mut self, mut samples: impl Iterator<Item = (f64, f32)>, stop: f64) {
        if !self.drawn { return; }
        let Some(mut previous) = samples.next() else { return };
        if previous.0 >= stop { return; }
        for (t, y) in samples {
            if previous.1.is_finite() && y.is_finite() { self.time.segment(previous.0, previous.1, t, y); }
            if t >= stop { return; }
            previous = (t, y);
        }
    }
    #[inline]
    fn segment(&mut self, t0: f64, y0: f32, t1: f64, y1: f32) {
        if self.drawn && y0.is_finite() && y1.is_finite() { self.time.segment(t0, y0, t1, y1); }
        if let Some(s) = &mut self.spectrum { s.segment(t0, y0, t1, y1); }
    }
    fn point(&mut self, t: f64, y: f32) {
        if self.drawn && y.is_finite() { self.time.point(t, y); }
        if let Some(s) = &mut self.spectrum { s.point(t, y); }
    }
    fn finish(&mut self, hits: &mut Hits, spectrum: Option<&mut Hits>) -> SpectrumResult {
        if self.drawn { self.time.finish(hits); }
        if let Some(s) = &mut self.spectrum { s.finish_optional(if self.drawn { Some(spectrum.expect("open lens needs spectrum storage")) } else { None }); s.result() }
        else { SpectrumResult::default() }
    }
}

struct OpenStroke {
    rasters: Vec<LaneRaster>,
    t_trigger: f64,
    start: f64,
    end: f64,
    /// Drawing ends with the first sample at or past this time: the end, or the sample the window
    /// opened at when that is later (its replay draws up to there).
    stop: f64,
    complete: bool,
    undecided: bool,
    /// The acquisition's `settings` when the window opened: a window under older settings does not
    /// hold the picture's re-arm.
    settings: u64,
}

impl OpenStroke {
    fn extend(&mut self, ring: &SampleRing, t: f64, y: &[f32]) {
        if !self.complete {
            if let Some(&previous) = ring.times.back() {
                for (c, (raster, &value)) in self.rasters.iter_mut().zip(y).enumerate() {
                    if !raster.drawn && raster.spectrum.is_none() { continue; }
                    raster.segment(previous, ring.value(c, ring.times.len() - 1), t, value);
                }
            } else {
                for (raster, &value) in self.rasters.iter_mut().zip(y) { raster.point(t, value); }
            }
            self.complete = t >= self.end;
        }
    }
}

/// One trigger, a SoA history, and pooled sets of rasters for unfinished windows.
/// Steady-state pushes allocate nothing: O(K * open windows) segments, plus the columns
/// crossed by those segments. Opening replays pre-history; completion visits covered pixels.
///
/// The picture re-arms after its window, as a real scope's does: a trigger inside the window still
/// drawing counts (its event, capture and answers) but opens no second window over it, so one
/// window draws at a time under one set of settings (`drawing_over`). Every sample still reaches
/// the picture while triggers come faster than the pre-trigger time, since the next window replays
/// it. A zone trigger decides by drawing its window, and a device's records carry their own dead
/// time, so neither path changes.
pub struct Acquisition {
    trigger: EdgeTrigger,
    qualifier: Qualifier,
    zone: Option<Zone>,
    waiting: Option<WaitingZone>,
    last_zone_event: Option<f64>,
    events: u64,
    zone_held_off: u64,
    zone_rejected: u64,
    zone_busy: u64,
    trigger_channel: usize,
    detector: EdgeDetector,
    window: Window,
    lanes: Vec<LaneSetup>,
    ring: SampleRing,
    candidates: [Option<OpenStroke>; 2],
    open: Vec<OpenStroke>,
    pool: Vec<Vec<LaneRaster>>,
    last_spectrum: Vec<SpectrumResult>,
    spectrum_updated: bool,
    span: Option<(f64, f64)>,
    auto: Option<f64>,
    last_event: Option<f64>,
    opened: Option<f64>,
    hardware_opened: Option<f64>,
    last_sweep: Option<(f64, bool)>,
    /// A steady source's values are held (repeated, not observed) after this time.
    held_from: Option<f64>,
    /// The steps below the time resolution (`Mark::BelowRes`) within the last window span, as
    /// (from, to) in source time, oldest first.
    below: VecDeque<(f64, f64)>,
    /// The time rasters' blocks (`Blocks`).
    blocks: Blocks,
    /// The earliest end of an open window still drawing: a sample there closes its block at once.
    next_end: f64,
    /// A lane has a spectrum lens, which reads every sample.
    spectra: bool,
    /// Counts changes of the window, a lane's map or the trigger: a window opened under older
    /// settings finishes as it was, and does not hold a window under the new ones from opening.
    settings: u64,
}

impl Acquisition {
    pub fn new(trigger: EdgeTrigger, trigger_channel: usize, window: Window, lanes: Vec<LaneSetup>) -> Self {
        validate(window);
        assert!(trigger_channel < lanes.len(), "trigger channel must name a lane");
        let (blocks, spectra) = (Blocks::new(&lanes), lanes.iter().any(|lane| lane.spectrum.is_some()));
        Self {
            trigger, qualifier: Qualifier::None, zone: None, waiting: None, last_zone_event: None, events: 0, zone_held_off: 0, zone_rejected: 0, zone_busy: 0, trigger_channel, detector: EdgeDetector::new(trigger), window,
            ring: SampleRing::new(lanes.len()), last_spectrum: vec![SpectrumResult::default(); lanes.len()], spectrum_updated: false, span: None, lanes,
            candidates: [None, None], open: Vec::new(), pool: Vec::new(),
            auto: None, last_event: None, opened: None, hardware_opened: None, last_sweep: None, held_from: None,
            below: VecDeque::new(), blocks, next_end: f64::INFINITY, spectra, settings: 0,
        }
    }

    pub fn with_qualifier(mut self, qualifier: Qualifier) -> Self {
        self.set_qualifier(qualifier);
        self
    }

    pub fn set_qualifier(&mut self, qualifier: Qualifier) {
        self.detector.set_qualifier(qualifier);
        if self.qualifier != qualifier { self.last_sweep = None; self.clear_waiting(); }
        self.qualifier = qualifier;
    }

    pub fn set_zone(&mut self, zone: Option<Zone>) {
        let zone = zone.filter(|zone| zone.valid());
        if self.zone == zone { return; }
        self.clear_waiting();
        self.clear_candidates();
        self.zone = zone;
        self.detector.set(self.detector_trigger());
        self.last_sweep = None;
    }

    fn detector_trigger(&self) -> EdgeTrigger {
        EdgeTrigger { holdoff: if self.active_zone().is_some() { 0.0 } else { self.trigger.holdoff }, ..self.trigger }
    }

    fn active_zone(&self) -> Option<Zone> { self.zone.and_then(|zone| zone.clipped(self.window, self.lanes.len())) }

    /// Keep provisional trend spans and capture copies until the box has decided.
    pub fn pending(&self) -> [Option<f64>; 2] {
        if let Some(waiting) = self.waiting {
            let mut pending = [None; 2];
            pending[waiting.side] = Some(waiting.time);
            pending
        } else { self.detector.pending() }
    }

    fn slope_side(&self, value: f32) -> usize {
        match self.trigger.slope {
            Slope::Rising => 0, Slope::Falling => 1,
            Slope::Either => usize::from(self.ring.times.back().is_some_and(|_| self.ring.value(self.trigger_channel, self.ring.times.len() - 1) > value)),
        }
    }

    fn clear_waiting(&mut self) {
        self.waiting = None;
        let mut i = 0;
        while i < self.open.len() {
            if self.open[i].undecided { self.pool.push(self.open.remove(i).rasters); }
            else { i += 1; }
        }
    }

    fn break_stream(&mut self) {
        self.detector.break_stream();
        self.clear_waiting();
        self.clear_candidates();
    }

    fn decide_zone(&mut self, t: f64, y: &[f32], replay: bool) -> Edge {
        if self.waiting.unwrap().replay {
            let side = self.slope_side(y[self.trigger_channel]);
            self.waiting.as_mut().unwrap().side = side;
        }
        let waiting = self.waiting.unwrap();
        self.waiting.as_mut().unwrap().replay = false;
        let zone = waiting.zone;
        let n = self.ring.times.len();
        let mut crossed = false;
        if replay {
            let first = self.ring.times.partition_point(|&t| t < waiting.time + zone.t0).saturating_sub(1);
            for i in first + 1..n {
                if self.ring.times[i - 1] > waiting.time + zone.t1 { break; }
                if zone.intersects(waiting.time, (self.ring.times[i - 1], self.ring.value(zone.lane, i - 1)), (self.ring.times[i], self.ring.value(zone.lane, i))) {
                    crossed = true;
                    break;
                }
            }
        }
        if n > 0 { crossed |= zone.intersects(waiting.time, (self.ring.times[n - 1], self.ring.value(zone.lane, n - 1)), (t, y[zone.lane])); }
        let accept = if crossed { Some(zone.must) } else if t >= waiting.time + zone.t1 { Some(!zone.must) } else { None };
        let Some(accept) = accept else { return Edge::Pending(waiting.time) };
        self.waiting = None;
        let index = self.open.iter().position(|stroke| stroke.undecided).expect("waiting zone owns a stroke");
        if accept {
            self.open[index].undecided = false;
            self.last_zone_event = Some(waiting.time);
            Edge::Accepted(waiting.time)
        } else {
            self.pool.push(self.open.remove(index).rasters);
            self.zone_rejected += 1;
            Edge::Dropped
        }
    }

    #[inline]
    pub fn push(&mut self, t: f64, y: &[f32], lanes: &mut [Hits]) -> Edge { self.push_sample(t, y, lanes, &mut [], true) }

    /// Both pictures from one sample pass; `push` is the time-only convenience path.
    pub fn push_frame(&mut self, t: f64, y: &[f32], frame: &mut LaneFrame) -> Edge { self.push_sample(t, y, &mut frame.lanes, &mut frame.spectra, true) }

    #[inline]
    fn push_sample(&mut self, t: f64, y: &[f32], lanes: &mut [Hits], spectra: &mut [Option<Hits>], search: bool) -> Edge {
        if y.len() == 1 { self.push_lanes::<true>(t, y, lanes, spectra, search) }
        else { self.push_lanes::<false>(t, y, lanes, spectra, search) }
    }

    #[inline]
    fn push_lanes<const SINGLE: bool>(&mut self, t: f64, y: &[f32], lanes: &mut [Hits], spectra: &mut [Option<Hits>], search: bool) -> Edge {
        assert_eq!(y.len(), self.lanes.len(), "point must contain every lane");
        debug_assert_eq!(lanes.len(), y.len());
        self.opened = self.hardware_opened.take();
        let valid = t.is_finite() && self.ring.times.back().is_none_or(|&previous| t > previous);
        debug_assert!(valid, "sample times must be finite and strictly increasing");
        if !valid { return Edge::None; }
        let mut retain_from = t;
        self.last_event.get_or_insert(t);
        if !y[self.trigger_channel].is_finite() || self.waiting.is_some_and(|waiting| !y[waiting.zone.lane].is_finite()) { self.break_stream(); }
        let mut verdict = if search { self.detector.push(t, y[self.trigger_channel]) } else { Edge::None };
        self.draw_sample(t, y);
        if self.waiting.is_some() {
            if self.waiting.unwrap().replay { retain_from = retain_from.min(self.waiting.unwrap().time); }
            if matches!(verdict, Edge::Accepted(_)) { self.zone_busy += 1; }
            verdict = self.decide_zone(t, y, self.waiting.unwrap().replay);
        } else if let Edge::Accepted(time) = verdict {
            if self.active_zone().is_some() && self.last_zone_event.is_some_and(|last| time - last < self.trigger.holdoff) {
                self.zone_held_off += 1;
                verdict = Edge::Dropped;
            } else if self.active_zone().is_none() && self.drawing_over(time) {
                // The picture re-arms after its window: this trigger counts, but draws nothing over
                // the window still drawing. A qualifier's provisional stroke for it goes back.
                if let Some(stroke) = self.candidates.iter_mut().find(|c| c.as_ref().is_some_and(|s| s.t_trigger == time)).and_then(Option::take) {
                    self.pool.push(stroke.rasters);
                }
            } else {
                let candidate = self.candidates.iter().position(|stroke| stroke.as_ref().is_some_and(|stroke| stroke.t_trigger == time));
                let mut stroke = candidate.and_then(|side| self.candidates[side].take()).unwrap_or_else(|| self.start(time, Some((t, y))));
                if let Some(zone) = self.active_zone() {
                    stroke.undecided = true;
                    let side = candidate.unwrap_or_else(|| self.slope_side(y[self.trigger_channel]));
                    self.waiting = Some(WaitingZone { time, side, zone, replay: false });
                    retain_from = retain_from.min(time);
                    self.open.push(stroke);
                    verdict = self.decide_zone(t, y, true);
                } else { self.open.push(stroke); }
            }
        }
        if let Edge::Accepted(time) = verdict {
            self.events += 1;
            self.last_event = Some(time);
            self.last_sweep = Some((time, false));
            self.opened = Some(time);
            retain_from = time;
        } else if search && self.auto.zip(self.last_event).is_some_and(|(after, last)| t - last >= after)
            && self.open.is_empty()
            && (self.window.pre == 0.0 || self.ring.times.front().is_some_and(|&first| t - first >= self.window.pre))
        {
            let stroke = self.start(t, Some((t, y)));
            self.open.push(stroke);
            self.last_event = Some(t);
            self.last_sweep = Some((t, true));
            self.opened = Some(t);
        }
        for (side, crossing) in self.pending().into_iter().enumerate() {
            // The waiting stroke is already in `open`; do not rasterise a second copy.
            let crossing = crossing.filter(|&time| self.waiting.is_none_or(|waiting| waiting.time != time));
            if self.active_zone().is_some() { if let Some(time) = crossing { retain_from = retain_from.min(time); } }
            if self.candidates[side].as_ref().map(|stroke| stroke.t_trigger) != crossing {
                if let Some(stroke) = self.candidates[side].take() { self.pool.push(stroke.rasters); }
                if let Some(time) = crossing {
                    // a crossing inside the window still drawing would draw nothing if accepted
                    if self.active_zone().is_some() || !self.drawing_over(time) {
                        let stroke = self.start(time, Some((t, y)));
                        self.candidates[side] = Some(stroke);
                    }
                    retain_from = retain_from.min(time);
                }
            }
        }
        if t >= self.next_end { self.close_block(); }
        self.finish_strokes(lanes, spectra, |_, _| {});
        self.ring.push::<SINGLE>(t, y);
        self.trim_ring(retain_from);
        verdict
    }

    /// The open windows' share of a sample: a spectrum lens reads it at once; the time rasters
    /// read its block when the block closes (O(lanes) here). A window that this sample completes
    /// closes the block at the end of the push (`next_end`).
    #[inline]
    fn draw_sample(&mut self, t: f64, y: &[f32]) {
        if self.spectra {
            let previous = self.ring.times.back().copied();
            let last = self.ring.times.len().wrapping_sub(1);
            for stroke in self.open.iter_mut().chain(self.candidates.iter_mut().flatten()) {
                if stroke.complete { continue; }
                for (c, raster) in stroke.rasters.iter_mut().enumerate() {
                    let Some(s) = &mut raster.spectrum else { continue };
                    match previous { Some(p) => s.segment(p, self.ring.value(c, last), t, y[c]), None => s.point(t, y[c]) }
                }
            }
        }
        if self.blocks.at.is_some_and(|(index, ..)| index != self.blocks.index(t)) { self.close_block(); }
        self.blocks.push(t, y);
    }

    /// Closes the open block: each open window's time rasters draw it once, O(windows x lanes) plus
    /// the samples of the windows whose column edge it crosses, and it joins the closed blocks for
    /// later windows' replay.
    fn close_block(&mut self) {
        let Some((_, t0, t1)) = self.blocks.at.take() else { return };
        let Acquisition { open, candidates, blocks, .. } = self;
        let lanes = blocks.open.len();
        let previous = blocks.closed.back().map(|&(_, t)| t);
        for stroke in open.iter_mut().chain(candidates.iter_mut().flatten()) {
            if stroke.complete { continue; }
            for (c, raster) in stroke.rasters.iter_mut().enumerate() {
                let fold = &blocks.open[c];
                raster.join(previous.and_then(|t| Some((t, blocks.folds[c].back()?.last))), t0, fold.first);
                let (times, values) = (&blocks.times, &blocks.values);
                raster.body(t0, t1, fold, stroke.stop, || times.iter().enumerate().map(move |(k, &t)| (t, values[k * lanes + c])));
            }
            stroke.complete = t1 >= stroke.stop;
        }
        blocks.closed.push_back((t0, t1));
        for (folds, &fold) in blocks.folds.iter_mut().zip(&blocks.open) { folds.push_back(fold); }
    }

    #[inline]
    fn finish_strokes(&mut self, lanes: &mut [Hits], spectra: &mut [Option<Hits>], mut close: impl FnMut(&mut OpenStroke, &SampleRing)) {
        let mut index = 0;
        let mut next_end = f64::INFINITY;
        while index < self.open.len() {
            close(&mut self.open[index], &self.ring);
            if self.open[index].complete && !self.open[index].undecided {
                let mut stroke = self.open.remove(index);
                for (i, ((raster, hits), last)) in stroke.rasters.iter_mut().zip(lanes.iter_mut()).zip(&mut self.last_spectrum).enumerate() {
                    *last = raster.finish(hits, spectra.get_mut(i).and_then(Option::as_mut));
                    self.spectrum_updated |= raster.spectrum.is_some();
                }
                let held = self.held_from.filter(|&from| from < stroke.end).map(|from| from - stroke.t_trigger);
                let below = self.below.iter().filter(|&&(from, to)| to > stroke.start && from < stroke.end)
                    .fold(None, |span: Option<(f64, f64)>, &(from, to)| Some(span.map_or((from, to), |(a, b)| (a.min(from), b.max(to)))))
                    .map(|(from, to)| (from.max(stroke.start) - stroke.t_trigger, to.min(stroke.end) - stroke.t_trigger));
                for hits in lanes.iter_mut() { hits.held = held; hits.below = below; }
                self.pool.push(stroke.rasters);
            } else {
                if !self.open[index].complete { next_end = next_end.min(self.open[index].stop); }
                index += 1;
            }
        }
        for stroke in self.candidates.iter().flatten() { if !stroke.complete { next_end = next_end.min(stroke.stop); } }
        self.next_end = next_end;
    }

    /// Open at the hardware's exact timestamp; replay retained pre-history without detection.
    /// The caller then feeds the block with software search disabled.
    pub fn push_triggered(&mut self, t_trigger: f64) {
        assert!(t_trigger.is_finite(), "hardware trigger time must be finite");
        if let Some(zone) = self.active_zone() {
            if self.waiting.is_some() { self.zone_busy += 1; return; }
            if self.last_zone_event.is_some_and(|last| t_trigger - last < self.trigger.holdoff) { self.zone_held_off += 1; return; }
            self.break_stream();
            let mut stroke = self.start(t_trigger, None);
            stroke.undecided = true;
            self.open.push(stroke);
            self.waiting = Some(WaitingZone { time: t_trigger, side: usize::from(self.trigger.slope == Slope::Falling), zone, replay: true });
        } else {
            self.break_stream();
            let stroke = self.start(t_trigger, None);
            self.open.push(stroke);
            self.last_event = Some(t_trigger);
            self.last_sweep = Some((t_trigger, false));
            self.hardware_opened = Some(t_trigger);
        }
    }

    pub fn set_auto(&mut self, after: Option<f64>) {
        assert!(after.is_none_or(|after| after.is_finite() && after >= 0.0), "auto timeout must be finite and nonnegative");
        self.auto = after;
    }

    pub fn trigger_state(&self) -> TriggerState {
        match self.last_sweep {
            Some((_, true)) => TriggerState::Automatic,
            Some((_, false)) if !self.open.is_empty() || self.opened.is_some() => TriggerState::Fired,
            _ => TriggerState::Waiting,
        }
    }

    pub(super) fn opened_at(&self) -> Option<f64> { self.opened }

    /// Hands each drawing window's settled columns to its lanes' frames (`Stroke::sweep`), so a
    /// window longer than a frame sweeps across the picture as it draws, as a scope's trace does.
    /// A window a zone has not decided keeps its columns until it is. O(open windows x lanes) plus
    /// the columns settled since the last sweep.
    pub fn sweep(&mut self, lanes: &mut [Hits]) {
        for stroke in self.open.iter_mut().filter(|stroke| !stroke.undecided) {
            for (raster, hits) in stroke.rasters.iter_mut().zip(lanes.iter_mut()) {
                if raster.drawn { raster.time.sweep(hits); }
            }
        }
    }

    /// Whether a trigger at `time` falls inside a window the picture is still drawing under the
    /// current settings (before its end). O(open windows), one or two here.
    fn drawing_over(&self, time: f64) -> bool {
        self.open.iter().any(|stroke| stroke.settings == self.settings && !stroke.undecided && time < stroke.end)
    }
    pub(super) fn spectrum_result(&self, lane: usize) -> SpectrumResult { self.last_spectrum.get(lane).copied().unwrap_or_default() }

    /// Same-size time rasters retain accepted windows with their original axes and end.
    /// Lane count, dimensions or spectrum changes discard unfinished pictures.
    pub fn set_lanes(&mut self, trigger_channel: usize, window: Window, lanes: Vec<LaneSetup>) {
        validate(window);
        assert!(trigger_channel < lanes.len(), "trigger channel must name a lane");
        let incompatible = lanes.len() != self.lanes.len() || lanes.iter().zip(&self.lanes).any(|(a, b)| a.spectrum != b.spectrum || (a.map.w, a.map.h) != (b.map.w, b.map.h));
        if incompatible {
            self.restart();
            self.pool.clear();
            if lanes.len() != self.lanes.len() { self.ring = SampleRing::new(lanes.len()); }
            self.last_spectrum.resize(lanes.len(), SpectrumResult::default());
        } else {
            // Hiding discards this lane's unfinished picture only. Revealing starts with the next
            // complete capture; the other lanes, detector, samples and spectral answers continue.
            for stroke in self.open.iter_mut().chain(self.candidates.iter_mut().flatten()) {
                for (raster, lane) in stroke.rasters.iter_mut().zip(&lanes) { raster.drawn &= lane.drawn; }
            }
        }
        if self.trigger_channel != trigger_channel {
            self.break_stream();
            if let Some(&t) = self.ring.times.back() { self.detector.push(t, self.ring.value(trigger_channel, self.ring.times.len() - 1)); }
        }
        let before = self.active_zone();
        if window != self.window || lanes.len() != self.lanes.len() || lanes.iter().zip(&self.lanes).any(|(a, b)| a.map != b.map) {
            self.settings += 1;
        }
        self.trigger_channel = trigger_channel;
        self.window = window;
        self.lanes = lanes;
        // Kept blocks stay valid folds at any width; a new width applies from the next block.
        if self.blocks.folds.len() != self.lanes.len() { self.blocks = Blocks::new(&self.lanes); }
        else { self.blocks.width = fold_width(&self.lanes); }
        self.spectra = self.lanes.iter().any(|lane| lane.spectrum.is_some());
        if before != self.active_zone() {
            self.clear_waiting();
            self.clear_candidates();
            self.detector.set(self.detector_trigger());
        }
        if let Some(&t) = self.ring.times.back() { self.trim_ring(t); }
    }

    pub fn set_trigger(&mut self, trigger: EdgeTrigger) {
        if self.trigger != trigger { self.last_sweep = None; self.settings += 1; }
        self.trigger = trigger;
        self.detector.set(self.detector_trigger());
        self.clear_waiting();
        self.clear_candidates();
    }

    fn clear_candidates(&mut self) {
        for candidate in &mut self.candidates { if let Some(stroke) = candidate.take() { self.pool.push(stroke.rasters); } }
    }

    pub fn restart(&mut self) {
        self.detector = EdgeDetector::with_qualifier(self.detector_trigger(), self.qualifier);
        self.waiting = None;
        self.last_zone_event = None;
        self.events = 0;
        self.zone_held_off = 0;
        self.zone_rejected = 0;
        self.zone_busy = 0;
        self.ring.clear();
        self.last_event = None;
        self.last_sweep = None;
        self.opened = None;
        self.hardware_opened = None;
        self.last_spectrum.fill(SpectrumResult::default());
        self.spectrum_updated = false;
        self.pool.extend(self.open.drain(..).map(|stroke| stroke.rasters));
        self.clear_candidates();
        self.held_from = None;
        self.below.clear();
        self.blocks.clear();
        self.next_end = f64::INFINITY;
    }

    /// Every lane's last pushed value.
    fn last_values(&self) -> Option<Vec<f32>> {
        let last = self.ring.times.len().checked_sub(1)?;
        Some((0..self.lanes.len()).map(|c| self.ring.value(c, last)).collect())
    }

    /// When held values pushed after `from` (a steady source's last point) complete what is open:
    /// every open window's end, then with auto on one sweep (its opening, then its end). Empty when
    /// The step to `t` (the newest sample) from the sample before it is below the simulation's
    /// time resolution: kept while a window can still cover it, adjoining steps joined. O(1)
    /// amortised per mark.
    fn below_res(&mut self, t: f64) {
        let from = self.ring.times.len().checked_sub(2).and_then(|i| self.ring.times.get(i)).copied().unwrap_or(t);
        let span = self.window.pre + self.window.post;
        while self.below.front().is_some_and(|&(_, to)| to < t - span) { self.below.pop_front(); }
        match self.below.back_mut() {
            Some(last) if last.1 >= from => last.1 = last.1.max(t),
            _ => self.below.push_back((from, t)),
        }
    }

    /// nothing would complete, as with a normal trigger on a level that never crosses.
    fn hold_times(&self, from: f64) -> Vec<f64> {
        let mut times = Vec::new();
        let mut t = from;
        if let Some(end) = self.open.iter().map(|stroke| stroke.end).reduce(f64::max) {
            t = end.max(t.next_up());
            times.push(t);
        }
        if let Some(after) = self.auto {
            // The ring keeps nothing newer than `from` before these pushes, so `from + pre` covers
            // the sweep's pre-history check whatever the ring trims meanwhile.
            let open = (self.last_event.unwrap_or(from) + after).max(from + self.window.pre).max(t.next_up());
            times.push(open);
            times.push((open + self.window.post).max(open.next_up()));
        }
        times
    }

    /// Discard interrupted accepted windows; tentative crossings never count as drops.
    fn gap(&mut self, from: f64, to: f64) -> u64 {
        let mut dropped = 0;
        let mut i = 0;
        while i < self.open.len() {
            if self.open[i].start <= to && self.open[i].end >= from {
                let stroke = self.open.remove(i);
                self.pool.push(stroke.rasters);
                dropped += u64::from(!stroke.undecided);
            } else { i += 1; }
        }
        self.break_stream();
        self.ring.clear();
        self.blocks.clear();
        self.opened = None;
        self.hardware_opened = None;
        self.last_event = None;
        self.last_sweep = None;
        dropped
    }
    pub fn ring_len(&self) -> usize { self.ring.times.len() }
    pub fn open_strokes(&self) -> usize { self.open.len() + self.candidates.iter().filter(|stroke| stroke.is_some()).count() }
    pub fn events(&self) -> u64 { self.events }
    pub fn held_off(&self) -> u64 { self.detector.held_off() + self.zone_held_off }
    pub fn unqualified(&self) -> u64 { self.detector.unqualified() }
    pub fn zone_rejected(&self) -> u64 { self.zone_rejected }
    pub fn zone_busy(&self) -> u64 { self.zone_busy }

    fn start(&mut self, time: f64, sample: Option<(f64, &[f32])>) -> OpenStroke {
        let mut rasters = self.pool.pop().unwrap_or_else(|| self.lanes.iter().map(|lane| LaneRaster { time: Stroke::default(), spectrum: lane.spectrum.map(|_| SpectrumStroke::default()), drawn: lane.drawn }).collect());
        let left = time - self.window.pre;
        let start = self.ring.times.iter().rposition(|&t| t < left).unwrap_or(0);
        let from = self.ring.times.get(start).copied();
        // The time rasters replay the closed blocks from the one holding the ring's sample `start`
        // (the last before the window), that one from `start` on; the open block (holding this
        // sample) reaches them when it closes. Spectra replay every sample.
        let first_block = from.map_or(self.blocks.closed.len(), |from| self.blocks.closed.partition_point(|&(_, t1)| t1 < from));
        for (c, (raster, lane)) in rasters.iter_mut().zip(&self.lanes).enumerate() {
            raster.drawn = lane.drawn;
            if !raster.drawn && raster.spectrum.is_none() { continue; }
            if raster.drawn {
                raster.time.begin(&lane.map, time);
                let ring = &self.ring;
                let in_ring = |from: f64, to: f64| {
                    (ring.times.partition_point(|&t| t < from)..ring.times.len()).map(move |k| (ring.times[k], ring.value(c, k))).take_while(move |&(t, _)| t <= to)
                };
                for i in first_block..self.blocks.closed.len() {
                    let (t0, t1) = self.blocks.closed[i];
                    if i == first_block {
                        raster.walk(in_ring(from.unwrap(), t1), f64::INFINITY);
                    } else {
                        let fold = &self.blocks.folds[c][i];
                        raster.join(self.blocks.before(c, i), t0, fold.first);
                        raster.body(t0, t1, fold, f64::INFINITY, || in_ring(t0, t1));
                    }
                }
            }
            if let Some(stroke) = &mut raster.spectrum {
                let (fft, pixels) = lane.spectrum.unwrap();
                stroke.begin_gated(-self.window.pre, self.window.pre + self.window.post, time, self.span, pixels, fft);
                if let Some(&first) = self.ring.times.get(start) {
                    let mut previous = (first, self.ring.value(c, start));
                    for i in start + 1..self.ring.times.len() {
                        let (t, y) = (self.ring.times[i], self.ring.value(c, i));
                        stroke.segment(previous.0, previous.1, t, y);
                        previous = (t, y);
                    }
                    if let Some((t, y)) = sample { stroke.segment(previous.0, previous.1, t, y[c]); }
                } else if let Some((t, y)) = sample { stroke.point(t, y[c]); }
            }
        }
        let end = time + self.window.post;
        let opened = sample.map(|(t, _)| t).or_else(|| self.ring.times.back().copied());
        let stop = opened.map_or(end, |t| t.max(end));
        self.next_end = self.next_end.min(stop);
        // With an open block the window completes when that block reaches it.
        let complete = self.blocks.at.is_none() && opened.is_some_and(|t| t >= end);
        OpenStroke { rasters, t_trigger: time, start: time - self.window.pre, end, stop, undecided: false, complete, settings: self.settings }
    }

    fn trim_ring(&mut self, newest: f64) {
        let newest = if self.active_zone().is_some() { self.pending().into_iter().flatten().fold(newest, f64::min) } else { newest };
        let start = newest - self.window.pre;
        while self.ring.times.get(1).is_some_and(|&t| t < start) { self.ring.pop_front(); }
        while self.blocks.closed.get(1).is_some_and(|&(_, t1)| t1 < start) {
            self.blocks.closed.pop_front();
            for folds in &mut self.blocks.folds { folds.pop_front(); }
        }
    }
}

/// Pictures of one shared trigger, from exactly one source layout generation.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LaneFrame { pub generation: u64, pub published: Option<std::time::Instant>, pub lanes: Vec<Hits>, pub spectra: Vec<Option<Hits>> }

impl LaneFrame {
    pub fn new(lanes: &[LaneSetup]) -> Self { Self { generation: 0, published: None, lanes: lanes.iter().map(|lane| Hits::new(lane.map.w, lane.map.h)).collect(), spectra: lanes.iter().map(|lane| lane.spectrum.map(|(_, (w, h))| Hits::new(w, h))).collect() } }
    pub fn single(w: u32, h: u32) -> Self { Self { generation: 0, published: None, lanes: vec![Hits::new(w, h)], spectra: vec![None] } }
    /// Largest per-lane accumulation; lane zero may be hidden. Per-lane counts remain authoritative.
    pub fn waveforms(&self) -> u64 { self.lanes.iter().map(|lane| lane.waveforms).max().unwrap_or(0) }
    pub fn reset(&mut self, lanes: &[LaneSetup], generation: u64) {
        self.published = None;
        self.lanes.resize_with(lanes.len(), || Hits::new(0, 0));
        for (hits, lane) in self.lanes.iter_mut().zip(lanes) { hits.reset(lane.map.w, lane.map.h); hits.generation = generation; }
        self.spectra.resize_with(lanes.len(), || None);
        for (hits, lane) in self.spectra.iter_mut().zip(lanes) {
            if let Some((_, (w, h))) = lane.spectrum { let hits = hits.get_or_insert_with(|| Hits::new(w, h)); hits.reset(w, h); hits.generation = generation; }
            else { *hits = None; }
        }
        self.generation = generation;
    }
    pub fn pictures_mut(&mut self) -> impl Iterator<Item = &mut Hits> { self.lanes.iter_mut().chain(self.spectra.iter_mut().flatten()) }
    pub fn clear(&mut self) { self.published = None; let generation = self.generation; for lane in self.pictures_mut() { lane.reset(lane.w, lane.h); lane.generation = generation; } }
    pub fn same_shape(&self, other: &Self) -> bool {
        self.lanes.len() == other.lanes.len() && self.lanes.iter().zip(&other.lanes).all(|(a, b)| (a.w, a.h) == (b.w, b.h))
            && self.spectra.len() == other.spectra.len() && self.spectra.iter().zip(&other.spectra).all(|(a, b)| a.as_ref().map(|h| (h.w, h.h)) == b.as_ref().map(|h| (h.w, h.h)))
    }
    pub fn add(&mut self, frame: &Self) {
        assert_eq!(self.generation, frame.generation, "cannot merge different layouts");
        assert_eq!(self.lanes.len(), frame.lanes.len(), "cannot merge different lane counts");
        for (lane, other) in self.lanes.iter_mut().zip(&frame.lanes) { lane.add(other); }
        assert_eq!(self.spectra.len(), frame.spectra.len(), "cannot merge different spectrum counts");
        for (lane, other) in self.spectra.iter_mut().zip(&frame.spectra) {
            match (lane, other) { (Some(lane), Some(other)) => lane.add(other), (None, None) => {}, _ => panic!("cannot merge different lenses") }
        }
    }
}

/// The publisher either takes the frame or returns it untouched for retry.
pub enum Published { Taken(LaneFrame), Kept(LaneFrame) }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TriggerState { #[default] Waiting, Fired, Automatic }

#[derive(Clone, Debug, Default)]
pub struct AcquireStats {
    pub trigger_state: TriggerState,
    /// Accepted edges since the last successful publication; automatic sweeps are excluded.
    pub trigger_events: u64,
    /// Optional cumulative source-time snapshot from this intake's accepted software edges.
    pub events: Option<EventList>,
    pub columns: Arc<Vec<TrendSeries>>,
    pub violations: ViolationList,
    pub event_epoch: u64,
    pub stopped: Option<(u64, Violation)>,
    pub samples: u64,
    pub unresolved: bool,
    pub dropped: u64,
    pub spectrum: Vec<SpectrumResult>,
    /// At least one spectrum completed since publication, even if its picture was hidden.
    pub spectrum_updated: bool,
    /// Request generation used by every FFT in this frame, including empty gates.
    pub spectrum_span: u64,
}

/// One screen window, transferred without copying between intake and measurement worker.
#[derive(Default)]
pub struct CaptureSamples {
    pub times: Vec<f64>,
    pub lanes: Vec<Vec<f32>>,
    /// Exact solver/hardware/segment annotations, separate from accepted detector marks().
    pub marks: Vec<(f64, Mark)>,
}

impl CaptureSamples {
    fn clear(&mut self) { self.times.clear(); for lane in &mut self.lanes { lane.clear(); } self.marks.clear(); }
    fn push(&mut self, t: f64, y: &[f32], end: f64) {
        if self.times.last().is_some_and(|&time| time >= end) { return; }
        self.lanes.resize_with(y.len(), Vec::new);
        if t > end {
            let Some(&previous) = self.times.last() else { return };
            let fraction = (end - previous) / (t - previous);
            for (lane, &v) in self.lanes.iter_mut().zip(y) {
                let old = *lane.last().unwrap();
                lane.push((f64::from(old) + fraction * (f64::from(v) - f64::from(old))) as f32);
            }
            self.times.push(end);
        } else {
            self.times.push(t);
            for (lane, &v) in self.lanes.iter_mut().zip(y) { lane.push(v); }
        }
    }
    fn copy_ring(&mut self, ring: &SampleRing, start: f64, end: f64, marks: &VecDeque<(f64, Mark)>) {
        self.clear();
        self.lanes.resize_with(ring.values.len(), Vec::new);
        let first = ring.times.partition_point(|&t| t < start);
        for (c, lane) in self.lanes.iter_mut().enumerate() {
            if first > 0 && ring.times.get(first).is_some_and(|&t| t > start) {
                let f = (start - ring.times[first - 1]) / (ring.times[first] - ring.times[first - 1]);
                lane.push((f64::from(ring.value(c, first - 1)) + f * (f64::from(ring.value(c, first)) - f64::from(ring.value(c, first - 1)))) as f32);
            }
            for i in first..ring.times.len() {
                if ring.times[i] > end {
                    if i > 0 {
                        let f = (end - ring.times[i - 1]) / (ring.times[i] - ring.times[i - 1]);
                        lane.push((f64::from(ring.value(c, i - 1)) + f * (f64::from(ring.value(c, i)) - f64::from(ring.value(c, i - 1)))) as f32);
                    }
                    break;
                }
                lane.push(ring.value(c, i));
                if ring.times[i] == end { break; }
            }
        }
        if first > 0 && ring.times.get(first).is_some_and(|&t| t > start) { self.times.push(start); }
        for &t in ring.times.iter().skip(first) { self.times.push(t.min(end)); if t >= end { break; } }
        self.marks.extend(marks.iter().copied().filter(|&(t, _)| t >= start && t <= end));
    }
    fn mark(&mut self, t: f64, kind: Mark, end: f64) {
        if self.times.first().is_some_and(|&first| t >= first) && t <= end { self.marks.push((t, kind)); }
    }
}

pub struct Capture {
    pub configuration: u64,
    /// Accepted trigger (or automatic sweep origin), in source seconds.
    pub trigger: f64,
    pub samples: CaptureSamples,
    pub generation: u64,
    pub returned: SyncSender<CaptureSamples>,
}

#[derive(Default)]
struct PendingCapture { event: Option<f64>, end: f64, samples: CaptureSamples }

struct CaptureSink {
    configuration: u64,
    request: Arc<AtomicBool>,
    send: SyncSender<Capture>,
    generation: u64,
    returned: SyncSender<CaptureSamples>,
    spares: Receiver<CaptureSamples>,
    samples: CaptureSamples,
    trigger: f64,
    end: Option<f64>,
    /// One sample interval while pushing a block; point streams require full pre-history.
    start_slack: f64,
    pending: [PendingCapture; 2],
    marks: VecDeque<(f64, Mark)>,
    /// The newest completed window the measure worker's full channel refused, offered again at
    /// each flush (a newer one replaces it).
    unsent: Option<Capture>,
}

impl CaptureSink {
    fn point(&mut self, acquisition: &Acquisition, t: f64, y: &[f32]) {
        // The prior point's mark has arrived by now, including a mark on the post boundary.
        self.flush();
        for pending in &mut self.pending { if pending.event.is_some() { pending.samples.push(t, y, pending.end); } }
        if let Some(end) = self.end { self.samples.push(t, y, end); }
        else if self.request.load(Ordering::Relaxed) {
            if let Some(event) = acquisition.opened_at() {
                let start = event - acquisition.window.pre;
                let pending = self.pending.iter_mut().find(|pending| pending.event == Some(event));
                let covered = pending.is_some() || acquisition.ring.times.front().is_some_and(|&first| first <= start || first - start < self.start_slack);
                if covered && self.request.swap(false, Ordering::Relaxed) {
                    let end = event + acquisition.window.post;
                    if let Some(pending) = pending {
                        std::mem::swap(&mut self.samples, &mut pending.samples);
                        pending.event = None;
                    } else {
                        if self.samples.times.capacity() == 0 { self.samples = self.spares.try_recv().unwrap_or_default(); }
                        self.samples.copy_ring(&acquisition.ring, start, end, &self.marks);
                    }
                    self.trigger = event;
                    self.end = Some(end);
                }
            }
        }
        let waiting = self.end.is_none() && self.request.load(Ordering::Relaxed);
        for (pending, crossing) in self.pending.iter_mut().zip(acquisition.pending()) {
            let crossing = crossing.filter(|_| waiting);
            if pending.event != crossing {
                pending.event = None;
                pending.samples.clear();
                if let Some(event) = crossing {
                    let start = event - acquisition.window.pre;
                    if acquisition.ring.times.front().is_some_and(|&first| first <= start || first - start < self.start_slack) {
                        pending.end = event + acquisition.window.post;
                        pending.samples.copy_ring(&acquisition.ring, start, pending.end, &self.marks);
                        pending.event = Some(event);
                    }
                }
            }
        }
        if let Some(&first) = acquisition.ring.times.front() { while self.marks.front().is_some_and(|&(t, _)| t < first) { self.marks.pop_front(); } }
    }
    fn mark(&mut self, t: f64, kind: Mark) {
        self.marks.push_back((t, kind));
        if let Some(end) = self.end { self.samples.mark(t, kind, end); }
        for pending in &mut self.pending { if pending.event.is_some() { pending.samples.mark(t, kind, pending.end); } }
    }
    fn flush(&mut self) {
        if let Some(capture) = self.unsent.take() {
            if let Err(TrySendError::Full(capture)) = self.send.try_send(capture) { self.unsent = Some(capture); }
        }
        if self.end.is_none_or(|end| self.samples.times.last().is_none_or(|&t| t < end)) { return; }
        self.end = None;
        let capture = Capture { configuration: self.configuration, trigger: self.trigger, samples: std::mem::take(&mut self.samples), generation: self.generation, returned: self.returned.clone() };
        self.samples = match self.send.try_send(capture) {
            Ok(()) => self.spares.try_recv().unwrap_or_default(),
            // the newest window waits for the worker; the one it replaces lends its storage
            Err(TrySendError::Full(capture)) => match self.unsent.replace(capture) {
                Some(mut older) => { older.samples.clear(); older.samples }
                None => self.spares.try_recv().unwrap_or_default(),
            },
            Err(TrySendError::Disconnected(capture)) => capture.samples,
        };
    }
    fn restart(&mut self) {
        if self.end.take().is_some() { self.request.store(true, Ordering::Relaxed); }
        self.samples.clear(); self.marks.clear();
        for pending in &mut self.pending { pending.event = None; pending.samples.clear(); }
    }
}

/// A trigger and auto timeout that running sinks take up in place, keeping their history and open
/// windows: a trigger edit on an unchanged screen never restarts acquisition.
#[derive(Default)]
pub struct TriggerSlot {
    version: AtomicU64,
    level: AtomicU64,
    hyst: AtomicU64,
    slope: AtomicU64,
    holdoff: AtomicU64,
    auto: AtomicU64,
    qualifier: AtomicU64,
    min: AtomicU64,
    max: AtomicU64,
    zone_present: AtomicBool,
    zone_lane: AtomicU64,
    zone_t0: AtomicU64,
    zone_t1: AtomicU64,
    zone_v0: AtomicU64,
    zone_v1: AtomicU64,
    zone_must: AtomicBool,
}

impl TriggerSlot {
    /// One UI producer publishes a latest value to any number of sinks, without a lock or wait.
    /// Sequential consistency brackets the fields: a reader seeing an in-progress edit defers
    /// until its next 64-sample check, rather than spinning on the producer.
    pub fn set(&self, trigger: EdgeTrigger, qualifier: Qualifier, auto: Option<f64>, zone: Option<Zone>) {
        use fab_elec_bench::trigger::Slope;
        assert!(fab_elec_bench::trigger::validate_qualifier(qualifier), "invalid trigger qualifier bounds");
        let (kind, min, max) = match qualifier {
            Qualifier::None => (0, 0.0f64, f64::INFINITY),
            Qualifier::Width { min, max } => (1, min, max),
            Qualifier::Period { min, max } => (2, min, max),
            Qualifier::Runt { other } => (3, other, f64::INFINITY),
        };
        self.version.fetch_add(1, Ordering::SeqCst);
        self.zone_present.store(zone.is_some(), Ordering::SeqCst);
        if let Some(zone) = zone {
            self.zone_lane.store(zone.lane as u64, Ordering::SeqCst);
            self.zone_t0.store(zone.t0.to_bits(), Ordering::SeqCst);
            self.zone_t1.store(zone.t1.to_bits(), Ordering::SeqCst);
            self.zone_v0.store(zone.v0.to_bits(), Ordering::SeqCst);
            self.zone_v1.store(zone.v1.to_bits(), Ordering::SeqCst);
            self.zone_must.store(zone.must, Ordering::SeqCst);
        }
        self.qualifier.store(kind, Ordering::SeqCst);
        self.min.store(min.to_bits(), Ordering::SeqCst);
        self.max.store(max.to_bits(), Ordering::SeqCst);
        self.level.store(trigger.level.to_bits(), Ordering::SeqCst);
        self.hyst.store(trigger.hyst.to_bits(), Ordering::SeqCst);
        self.slope.store(match trigger.slope { Slope::Rising => 0, Slope::Falling => 1, Slope::Either => 2 }, Ordering::SeqCst);
        self.holdoff.store(trigger.holdoff.to_bits(), Ordering::SeqCst);
        self.auto.store(auto.unwrap_or(f64::INFINITY).to_bits(), Ordering::SeqCst);
        self.version.fetch_add(1, Ordering::SeqCst);
    }

    pub fn read(&self, seen: u64) -> Option<(u64, EdgeTrigger, Qualifier, Option<f64>, Option<Zone>)> {
        use fab_elec_bench::trigger::Slope;
        let version = self.version.load(Ordering::SeqCst);
        if version == seen || version % 2 != 0 { return None; }
        let trigger = EdgeTrigger {
            level: f64::from_bits(self.level.load(Ordering::SeqCst)),
            hyst: f64::from_bits(self.hyst.load(Ordering::SeqCst)),
            slope: match self.slope.load(Ordering::SeqCst) { 1 => Slope::Falling, 2 => Slope::Either, _ => Slope::Rising },
            holdoff: f64::from_bits(self.holdoff.load(Ordering::SeqCst)),
        };
        let kind = self.qualifier.load(Ordering::SeqCst);
        let min = f64::from_bits(self.min.load(Ordering::SeqCst));
        let max = f64::from_bits(self.max.load(Ordering::SeqCst));
        let qualifier = match kind { 1 => Qualifier::Width { min, max }, 2 => Qualifier::Period { min, max }, 3 => Qualifier::Runt { other: min }, _ => Qualifier::None };
        let auto = f64::from_bits(self.auto.load(Ordering::SeqCst));
        let zone = self.zone_present.load(Ordering::SeqCst).then(|| Zone {
            lane: self.zone_lane.load(Ordering::SeqCst) as usize,
            t0: f64::from_bits(self.zone_t0.load(Ordering::SeqCst)),
            t1: f64::from_bits(self.zone_t1.load(Ordering::SeqCst)),
            v0: f64::from_bits(self.zone_v0.load(Ordering::SeqCst)),
            v1: f64::from_bits(self.zone_v1.load(Ordering::SeqCst)),
            must: self.zone_must.load(Ordering::SeqCst),
        });
        (self.version.load(Ordering::SeqCst) == version).then_some((version, trigger, qualifier, auto.is_finite().then_some(auto), zone))
    }
}

/// Latest value windows and visibility for an unchanged acquisition layout. Pixel dimensions, spectrum and
/// time axes are immutable here: those changes need a new feed. Readers clone only an Arc under
/// the lock and copy the small setups after releasing it.
pub struct LaneSlot {
    version: AtomicU64,
    value: Mutex<Arc<Vec<LaneSetup>>>,
}

impl LaneSlot {
    pub fn new(lanes: Vec<LaneSetup>) -> Self {
        Self { version: AtomicU64::new(0), value: Mutex::new(Arc::new(lanes)) }
    }

    pub fn compatible(before: &[LaneSetup], after: &[LaneSetup]) -> bool {
        // Visibility changes neither storage shape nor acquisition identity.
        before.len() == after.len() && before.iter().zip(after).all(|(a, b)| {
            a.spectrum == b.spectrum && (a.map.w, a.map.h, a.map.t_left, a.map.t_px) == (b.map.w, b.map.h, b.map.t_left, b.map.t_px)
                && b.map.v_top.is_finite() && b.map.v_px.is_finite() && b.map.v_px > 0.0
        })
    }

    /// Refuse layout edits without publishing them. Sinks hear edits at the next block or within 64 points.
    pub fn set(&self, lanes: Vec<LaneSetup>) -> bool {
        let old = self.value.lock().unwrap().clone();
        if !Self::compatible(&old, &lanes) { return false; }
        let lanes = Arc::new(lanes);
        let old = std::mem::replace(&mut *self.value.lock().unwrap(), lanes);
        drop(old);
        self.version.fetch_add(1, Ordering::Release);
        true
    }
}

/// One latest-wins pin mailbox. Neither producer nor consumer waits for the other.
/// A refused UI write is retried with the bench's current list on its next frame.
pub struct TrendSlot {
    version: AtomicU64,
    value: Mutex<Arc<TrendRequest>>,
}

#[derive(Clone, Default)]
struct TrendRequest { pins: Vec<(usize, TrendQuantity)>, bounds: Vec<TrendBound>, stop: Vec<usize>, arm: u64 }

impl Default for TrendSlot {
    fn default() -> Self { Self { version: AtomicU64::new(1), value: Mutex::new(Arc::new(TrendRequest::default())) } }
}

impl TrendSlot {
    pub fn set(&self, pins: &[(usize, TrendQuantity)]) -> bool {
        let Ok(mut value) = self.value.try_lock() else { return false };
        if value.pins.as_slice() == pins { return true; }
        Arc::make_mut(&mut value).pins = pins.to_vec();
        self.version.fetch_add(1, Ordering::Release);
        true
    }
    pub fn set_bounds(&self, bounds: &[TrendBound]) -> bool {
        let Ok(mut value) = self.value.try_lock() else { return false };
        if value.bounds.as_slice() == bounds { return true; }
        Arc::make_mut(&mut value).bounds = bounds.to_vec();
        self.version.fetch_add(1, Ordering::Release);
        true
    }
    pub fn arm_stop_on_violation(&self, bounds: &[usize]) -> bool {
        let Ok(mut value) = self.value.try_lock() else { return false };
        let value = Arc::make_mut(&mut value);
        value.stop = bounds.to_vec(); value.arm += 1;
        self.version.fetch_add(1, Ordering::Release);
        true
    }
    pub(super) fn armed_request(&self) -> Option<u64> { Some(self.value.try_lock().ok()?.arm) }
}

/// Source-neutral trigger, raster, capture and frame publisher.
pub struct AcquireIntake<F: FnMut(LaneFrame, AcquireStats) -> Published + Send> {
    previous: Option<f64>,
    samples: u64,
    trigger_events: u64,
    dropped: u64,
    unresolved: bool,
    pub(super) acquisition: Acquisition,
    pub(super) frame: LaneFrame,
    frame_every: Duration,
    frame_started: Instant,
    until_clock: u8,
    publish: F,
    marks: Vec<f64>,
    retain_marks: bool,
    events: Option<EventLog>,
    trends: TrendRecorder,
    trend_slot: Option<(Arc<TrendSlot>, u64)>,
    trends_dirty: bool,
    capture: Option<CaptureSink>,
    trigger_slot: Option<(Arc<TriggerSlot>, u64)>,
    lane_slot: Option<(Arc<LaneSlot>, u64)>,
    span_request: Option<Arc<SpanRequest>>,
    pub(super) span_generation: u64,
    block_values: Vec<f32>,
    /// The last publication was refused (`Published::Kept`): `frame` waits for the next flush.
    kept: bool,
}

impl<F: FnMut(LaneFrame, AcquireStats) -> Published + Send> AcquireIntake<F> {
    pub fn new(acquisition: Acquisition, frame_every: Duration, publish: F) -> Self {
        Self {
            frame: LaneFrame::new(&acquisition.lanes), block_values: vec![0.0; acquisition.lanes.len()],
            previous: None, samples: 0, trigger_events: 0, dropped: 0, unresolved: false, acquisition, frame_every,
            frame_started: Instant::now(), until_clock: 64, publish, marks: Vec::new(),
            retain_marks: true, events: None, trends: TrendRecorder::default(), trend_slot: None, trends_dirty: false, capture: None, trigger_slot: None, lane_slot: None, span_request: None, span_generation: 0, kept: false,
        }
    }
    pub fn acquisition(&self) -> &Acquisition { &self.acquisition }
    pub fn generation(&self) -> u64 { self.frame.generation }
    /// Reuse the bracket mailbox. The worker takes its latest value before each sample batch.
    pub fn follow_span(&mut self, request: Arc<SpanRequest>) { self.span_request = Some(request); }
    fn take_span(&mut self) {
        let Some(request) = &self.span_request else { return };
        let (generation, span) = request.get();
        if generation == self.span_generation { return; }
        self.span_generation = generation;
        self.acquisition.span = span;
        if self.acquisition.lanes.iter().any(|lane| lane.spectrum.is_some()) {
            // As with a window-function edit, unfinished transforms and accumulated hits
            // cannot survive a change of transform. Keep the pooled FFT storage.
            self.acquisition.restart();
            self.frame.clear();
            if let Some(capture) = &mut self.capture { capture.restart(); }
            self.previous = None;
        }
    }
    pub fn follow_trigger(&mut self, slot: Arc<TriggerSlot>) {
        self.trigger_slot = Some((slot, 0));
        self.take_trigger();
    }
    pub fn follow_lanes(&mut self, slot: Arc<LaneSlot>) {
        let version = slot.version.load(Ordering::Acquire);
        let lanes = slot.value.lock().unwrap().clone();
        assert!(LaneSlot::compatible(&self.acquisition.lanes, &lanes), "lane slot must match the intake layout");
        self.acquisition.set_lanes(self.acquisition.trigger_channel, self.acquisition.window, (*lanes).clone());
        self.lane_slot = Some((slot, version));
    }
    /// Follow pin requests only on the intake that records the nominal event list.
    pub fn follow_trends(&mut self, slot: Arc<TrendSlot>) {
        assert!(self.events.is_some(), "trends require the nominal event list");
        self.trend_slot = Some((slot, 0));
        self.take_trends();
    }
    fn take_trends(&mut self) {
        let Some((slot, seen)) = &mut self.trend_slot else { return };
        let version = slot.version.load(Ordering::Acquire);
        if version == *seen { return; }
        let request = {
            let Ok(request) = slot.value.try_lock() else { return };
            request.clone()
        };
        *seen = version;
        assert!(request.pins.iter().chain(request.bounds.iter().map(|bound| &bound.pin)).all(|&(lane, _)| lane < self.acquisition.lanes.len()), "trend pin must name a lane");
        let start = self.events.as_ref().map_or(0, EventLog::len);
        self.trends.set(&request.pins, &request.bounds, start);
        self.trends.arm(request.arm, &request.stop, start);
        self.trends_dirty = true;
    }
    fn restart_events(&mut self, continuous: bool) {
        if let Some(events) = &mut self.events { events.restart(); }
        self.trends.restart(continuous);
        self.trends_dirty |= self.events.is_some();
    }
    fn take_lanes(&mut self) {
        let Some((slot, seen)) = &mut self.lane_slot else { return };
        let version = slot.version.load(Ordering::Acquire);
        if version == *seen { return; }
        let lanes = slot.value.lock().unwrap().clone();
        *seen = version;
        for (i, (before, after)) in self.acquisition.lanes.iter().zip(lanes.iter()).enumerate() {
            if before.drawn && !after.drawn {
                let hits = &mut self.frame.lanes[i];
                hits.reset(hits.w, hits.h); hits.generation = self.frame.generation;
                if let Some(hits) = &mut self.frame.spectra[i] { hits.reset(hits.w, hits.h); hits.generation = self.frame.generation; }
            }
        }
        self.acquisition.set_lanes(self.acquisition.trigger_channel, self.acquisition.window, (*lanes).clone());
    }
    fn take_trigger(&mut self) {
        let Some((slot, seen)) = &mut self.trigger_slot else { return };
        if let Some((version, trigger, qualifier, auto, zone)) = slot.read(*seen) {
            *seen = version;
            if self.acquisition.trigger != trigger || self.acquisition.qualifier != qualifier || self.acquisition.zone != zone { self.restart_events(true); }
            if self.acquisition.trigger != trigger { self.acquisition.set_trigger(trigger); }
            self.acquisition.set_qualifier(qualifier);
            self.acquisition.set_zone(zone);
            self.acquisition.set_auto(auto);
        }
    }
    /// Accepted software detector events only, as before; auto sweeps and source annotations
    /// are excluded. Intake::mark annotations instead accompany CaptureSamples::marks.
    pub fn marks(&mut self) -> &mut Vec<f64> { &mut self.marks }
    pub(super) fn discard_marks(&mut self) { self.retain_marks = false; self.marks.clear(); }
    /// Record this source's accepted times for the strip; no detector or sample replay.
    pub fn record_events(&mut self) { self.events = Some(EventLog::default()); self.trends.restart(false); }
    /// Competing members share one request; each has its own vector return path.
    pub fn capture(&mut self, request: Arc<AtomicBool>, send: SyncSender<Capture>, configuration: u64) {
        let (returned, spares) = sync_channel(1);
        self.capture = Some(CaptureSink {
            configuration, request, send, generation: self.frame.generation, returned, spares,
            samples: CaptureSamples::default(), trigger: 0.0, end: None, start_slack: 0.0,
            pending: [PendingCapture::default(), PendingCapture::default()], marks: VecDeque::new(), unsent: None,
        });
    }
    pub fn flush(&mut self) {
        self.take_span();
        self.take_trends();
        if let Some(capture) = &mut self.capture { capture.flush(); }
        self.flush_frame();
    }
    fn flush_frame(&mut self) {
        // The pen moves only with samples, so a frame the sweep adds to is published anyway.
        self.acquisition.sweep(&mut self.frame.lanes);
        if self.samples == 0 && self.dropped == 0 && self.trigger_events == 0 && !self.trends_dirty && !self.acquisition.spectrum_updated && self.frame.waveforms() == 0 { return; }
        let frame = std::mem::take(&mut self.frame);
        let generation = frame.generation;
        self.frame = match (self.publish)(frame, AcquireStats { trigger_state: self.acquisition.trigger_state(), trigger_events: self.trigger_events, events: self.events.as_mut().map(EventLog::snapshot), columns: self.trends.snapshot(), violations: self.trends.violations(), event_epoch: self.trends.epoch(), stopped: self.trends.stopped(), samples: self.samples, unresolved: self.unresolved, dropped: self.dropped, spectrum: self.acquisition.last_spectrum.clone(), spectrum_updated: self.acquisition.spectrum_updated, spectrum_span: self.span_generation }) {
            Published::Taken(mut spare) => {
                spare.reset(&self.acquisition.lanes, generation);
                self.samples = 0; self.trigger_events = 0; self.dropped = 0; self.trends_dirty = false;
                self.acquisition.spectrum_updated = false;
                self.kept = false;
                spare
            }
            Published::Kept(frame) => { self.kept = true; frame }
        };
        self.frame_started = Instant::now();
    }
    pub(super) fn configure(&mut self, trigger: EdgeTrigger, qualifier: Qualifier, channel: usize, window: Window, lanes: Vec<LaneSetup>, auto: Option<f64>, zone: Option<Zone>) {
        if trigger != self.acquisition.trigger || qualifier != self.acquisition.qualifier || zone != self.acquisition.zone || channel != self.acquisition.trigger_channel || window != self.acquisition.window { self.restart_events(true); }
        self.acquisition.set_lanes(channel, window, lanes);
        if self.acquisition.trigger != trigger { self.acquisition.set_trigger(trigger); }
        self.acquisition.set_qualifier(qualifier);
        self.acquisition.set_zone(zone);
        self.acquisition.set_auto(auto);
        self.block_values.resize(self.acquisition.lanes.len(), 0.0);
    }
    /// Observed points after a hold restart the windows: the held stretch was never observed, and the
    /// source's clock resumes from where it stopped.
    fn resume(&mut self) {
        if self.acquisition.held_from.is_some() {
            self.acquisition.restart();
            self.restart_events(false);
            if let Some(capture) = &mut self.capture { capture.restart(); }
            self.previous = None;
        }
    }
    fn push<const BLOCK: bool>(&mut self, t: f64, y: &[f32], search: bool) {
        if self.until_clock == 64 { self.take_span(); }
        self.take_trends();
        if !t.is_finite() { return; }
        if self.previous.is_some_and(|previous| t <= previous) {
            self.acquisition.restart();
            if let Some(capture) = &mut self.capture { capture.restart(); }
            self.restart_events(false);
        }
        self.previous = Some(t);
        self.unresolved |= y.iter().any(|v| v.is_nan());
        let verdict = self.acquisition.push_sample(t, y, &mut self.frame.lanes, &mut self.frame.spectra, search);
        if !self.trends.is_empty() {
            let accepted = if let Edge::Accepted(time) = verdict { Some(time) } else { None };
            self.trends.push(t, y, accepted, self.events.as_ref().map_or(0, EventLog::len), self.acquisition.pending(), self.acquisition.window.post,
                self.acquisition.trigger_channel, self.acquisition.trigger.level);
        }
        if let Edge::Accepted(time) = verdict {
            self.trigger_events += 1;
            if self.retain_marks { self.marks.push(time); }
            if let Some(events) = &mut self.events { events.push(time); }
        }
        if let Some(capture) = &mut self.capture { capture.point(&self.acquisition, t, y); }
        self.samples += 1;
        self.until_clock -= 1;
        if self.until_clock == 0 {
            self.until_clock = 64;
            if !BLOCK { self.take_lanes(); }
            self.take_trigger();
            if Instant::now().duration_since(self.frame_started) >= self.frame_every { self.flush_frame(); }
        }
    }
}

impl<F: FnMut(LaneFrame, AcquireStats) -> Published + Send> Intake for AcquireIntake<F> {
    fn begin(&mut self, layout: &Layout) {
        assert_eq!(layout.channels.len(), self.acquisition.lanes.len(), "layout and acquisition lanes must match");
        self.flush();
        // A retained old generation must never be mixed into the new one.
        self.dropped += u64::from(self.samples > 0);
        self.samples = 0;
        self.trigger_events = 0;
        self.frame.reset(&self.acquisition.lanes, layout.generation);
        self.acquisition.restart();
        if let Some(capture) = &mut self.capture { capture.restart(); capture.generation = layout.generation; }
        self.restart_events(false);
        self.previous = None;
        self.unresolved = false;
    }
    fn point(&mut self, t: f64, y: &[f32]) { self.resume(); self.push::<false>(t, y, true); }
    fn block(&mut self, b: &Block) {
        self.take_lanes();
        self.take_span();
        assert_eq!(b.chans.len(), self.acquisition.lanes.len(), "block must contain every lane");
        self.resume();
        assert!(b.t0.is_finite() && b.dt.is_finite() && b.dt > 0.0, "block clock must increase");
        let n = b.chans[0].len();
        assert!(b.chans.iter().all(|c| c.len() == n), "block columns must have equal lengths");
        let mut values = std::mem::take(&mut self.block_values);
        let mut trigger = b.trigger;
        if let Some(capture) = &mut self.capture { capture.start_slack = b.dt; }
        if trigger.is_some() && self.acquisition.active_zone().is_none() { self.acquisition.break_stream(); }
        for i in 0..n {
            let t = b.t0 + i as f64 * b.dt;
            if let Some(at) = trigger.filter(|&at| t >= at) { self.acquisition.push_triggered(at); self.trigger_events += u64::from(self.acquisition.active_zone().is_none()); trigger = None; }
            for (v, c) in values.iter_mut().zip(b.chans) { *v = c.value(i); }
            // push may publish mid-block, so each resulting frame keeps this capture's flags.
            for (lane, clipped) in self.frame.lanes.iter_mut().zip(b.clipped) { lane.clipped.include(*clipped); }
            self.push::<true>(t, &values, b.trigger.is_none());
        }
        if let Some(at) = trigger { self.acquisition.push_triggered(at); self.trigger_events += u64::from(self.acquisition.active_zone().is_none()); }
        let record_end = b.t0 + n as f64 * b.dt;
        if let Some(capture) = &mut self.capture {
            capture.start_slack = 0.0;
            if n > 0 {
                if let Some(end) = capture.end.filter(|&end| end <= record_end) { capture.samples.push(end, &values, end); capture.flush(); }
            }
        }
        // Block coverage is N * dt: hold the last sample to each accepted window's end in
        // one closing pass per block. The only added per-sample work is the start-slack read.
        if n > 0 {
            self.acquisition.close_block();
            self.acquisition.finish_strokes(&mut self.frame.lanes, &mut self.frame.spectra, |stroke, ring| {
                if !stroke.undecided && stroke.end <= record_end { stroke.extend(ring, stroke.end, &values); }
            });
            // The last sample may have published and recycled the frame before this close.
            if self.frame.waveforms() > 0 { for (lane, clipped) in self.frame.lanes.iter_mut().zip(b.clipped) { lane.clipped.include(*clipped); } }
        }
        self.block_values = values;
    }
    fn mark(&mut self, t: f64, kind: Mark) {
        debug_assert_eq!(self.previous, Some(t), "marks follow the point at exactly their time");
        if kind == Mark::BelowRes {
            self.acquisition.below_res(t);
            return;
        }
        if let Some(capture) = &mut self.capture { capture.mark(t, kind); }
    }
    fn gap(&mut self, from: f64, to: f64, why: Gap) {
        assert!(from.is_finite() && to.is_finite() && to >= from, "gap must be a finite forward interval");
        self.dropped += self.acquisition.gap(from, to);
        if why == Gap::Restart { self.acquisition.restart(); }
        self.restart_events(false);
        if let Some(capture) = &mut self.capture { capture.restart(); }
        self.previous = None;
    }
    fn idle(&mut self) { self.flush(); }
    fn pending(&self) -> bool { self.kept || self.capture.as_ref().is_some_and(|capture| capture.unsent.is_some()) }
    /// Completes the open windows, and one auto sweep, from the held values: a few pushes, each
    /// marked `Mark::Held`, so the picture and the measured window show a steady source at rest.
    fn steady(&mut self, at: f64) {
        let from = self.previous.map_or(at, |previous| previous.max(at));
        if let Some(values) = self.acquisition.last_values() {
            let times = self.acquisition.hold_times(from);
            if !times.is_empty() {
                self.acquisition.held_from = Some(from);
                // The measure worker reads a held window like any other: ask for this one.
                if let Some(capture) = &self.capture { capture.request.store(true, Ordering::Relaxed); }
                for t in times {
                    self.push::<false>(t, &values, true);
                    if let Some(capture) = &mut self.capture { capture.mark(t, Mark::Held); }
                }
            }
        }
        self.flush();
    }
}

impl<F: FnMut(LaneFrame, AcquireStats) -> Published + Send> Drop for AcquireIntake<F> {
    fn drop(&mut self) { self.flush(); }
}

/// A simulator channel resolves either a voltage difference or a named branch current.
#[derive(Clone, Debug)]
pub struct SimChannel { pub info: ChannelInfo, pub probe: NetProbe }

impl SimChannel {
    pub fn voltage(id: ChanId, probe: NetProbe) -> Self {
        Self { info: ChannelInfo { id, name: probe.pos.clone(), quantity: Quantity::Volt }, probe }
    }
}

/// Shared source epochs for ensemble sinks, including members installed after a re-lowering.
/// Only begin uses this registry; the UI and per-point path never take its lock.
#[derive(Default)]
pub struct SimLayouts { newest: u64, layouts: Vec<(Vec<UnknownInfo>, u64)> }

impl SimLayouts {
    fn generation(&mut self, unknowns: &[UnknownInfo], previous: u64) -> u64 {
        let found = self.layouts.iter().position(|(names, _)| names == unknowns);
        if let Some(i) = found {
            if self.layouts[i].1 > previous { return self.layouts[i].1; }
        }
        self.newest += 1;
        if let Some(i) = found { self.layouts[i].1 = self.newest; }
        else { self.layouts.push((unknowns.to_vec(), self.newest)); }
        self.newest
    }
}

/// Simulator-only name resolution. Identical names AND kinds preserve the input generation.
/// Logic derives from voltage downstream; this adapter never computes device current or power.
pub struct SimIntake<I: Intake> {
    intake: I,
    channels: Vec<SimChannel>,
    probes: Vec<Probe>,
    values: Vec<f32>,
    unknowns: Option<Vec<UnknownInfo>>,
    layouts: Option<Arc<Mutex<SimLayouts>>>,
    generation: u64,
    unresolved: Vec<bool>,
}

impl<I: Intake> SimIntake<I> {
    pub fn new(channels: Vec<SimChannel>, intake: I) -> Self {
        let n = channels.len();
        Self { intake, channels, probes: Vec::new(), values: vec![f32::NAN; n], unknowns: None, layouts: None, generation: 0, unresolved: vec![true; n] }
    }
    /// Use one registry per source ensemble so a late member cannot reuse an old layout tag.
    pub fn follow_layouts(&mut self, layouts: Arc<Mutex<SimLayouts>>) {
        assert!(self.unknowns.is_none(), "install the layout registry before begin");
        self.layouts = Some(layouts);
    }
    pub fn unresolved(&self) -> bool { self.unresolved.iter().any(|&v| v) }
    pub fn unresolved_channels(&self) -> &[bool] { &self.unresolved }
    pub fn intake(&self) -> &I { &self.intake }
    pub fn intake_mut(&mut self) -> &mut I { &mut self.intake }
}

impl<F: FnMut(LaneFrame, AcquireStats) -> Published + Send> SimIntake<AcquireIntake<F>> {
    /// Single voltage card convenience; the acquisition engine still receives a full layout.
    pub fn by_name(probe: NetProbe, acquisition: Acquisition, frame_every: Duration, publish: F) -> Self {
        Self::new(vec![SimChannel::voltage(ChanId(0), probe)], AcquireIntake::new(acquisition, frame_every, publish))
    }
    pub fn marks(&mut self) -> &mut Vec<f64> { self.intake.marks() }
    pub fn flush(&mut self) { self.intake.flush(); }
    pub fn follow_trigger(&mut self, slot: Arc<TriggerSlot>) { self.intake.follow_trigger(slot); }
}

impl<I: Intake> fab_elec_sim::Sink for SimIntake<I> {
    fn begin(&mut self, unknowns: &[UnknownInfo]) {
        if self.unknowns.as_deref() == Some(unknowns) { return; }
        self.unknowns = Some(unknowns.to_vec());
        self.generation = self.layouts.as_ref().map_or(self.generation + 1, |layouts| layouts.lock().unwrap().generation(unknowns, self.generation));
        self.probes.clear();
        for (c, channel) in self.channels.iter().enumerate() {
            let kind = match channel.info.quantity { Quantity::Volt => Some(UnkKind::Voltage), Quantity::Amp => Some(UnkKind::Current), Quantity::Logic => None };
            let resolve = |name: &str| unknowns.iter().position(|u| Some(u.kind) == kind && u.name == name).unwrap_or(usize::MAX);
            let pos = if channel.probe.pos == "0" && kind == Some(UnkKind::Voltage) { 0 } else { resolve(&channel.probe.pos) };
            let neg = if channel.info.quantity == Quantity::Amp { 0 } else { channel.probe.neg.as_deref().filter(|&name| name != "0").map_or(0, resolve) };
            self.unresolved[c] = kind.is_none() || pos == usize::MAX || neg == usize::MAX;
            self.probes.push(Probe { pos, neg, scale: channel.probe.scale });
        }
        self.intake.begin(&Layout { generation: self.generation, clock: ClockKind::Simulated, channels: self.channels.iter().map(|c| c.info.clone()).collect() });
    }
    fn point(&mut self, at: f64, _: Option<f64>, x: &[f64]) {
        for ((value, probe), &unresolved) in self.values.iter_mut().zip(&self.probes).zip(&self.unresolved) {
            let get = |i| if i == 0 { Some(0.0) } else { x.get(i).copied() };
            *value = if unresolved { f32::NAN } else { get(probe.pos).zip(get(probe.neg)).map_or(f32::NAN, |(p, n)| ((p - n) * probe.scale) as f32) };
        }
        self.intake.point(at, &self.values);
    }
    fn mark(&mut self, at: f64) { self.intake.mark(at, Mark::Solver); }
    fn below_res(&mut self, at: f64) { self.intake.mark(at, Mark::BelowRes); }
    fn idle(&mut self) { self.intake.idle(); }
    fn steady(&mut self, at: f64, _x: &[f64]) { self.intake.steady(at); }
    fn pending(&self) -> bool { self.intake.pending() }
}

fn validate(window: Window) {
    assert!(window.pre.is_finite() && window.pre >= 0.0, "pre-trigger duration must be finite and nonnegative");
    assert!(window.post.is_finite() && window.post >= 0.0, "post-trigger duration must be finite and nonnegative");
}
