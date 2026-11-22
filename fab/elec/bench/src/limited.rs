//! A point stream seen through a resolved bench scope's ADC and acquisition limits.

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use crate::source::*;
use crate::trigger::{Edge, EdgeDetector, EdgeTrigger};

const STREAM_BATCH: usize = 4096;

/// The simulated input seen through a resolved instrument's acquisition limits.
#[derive(Clone)]
pub struct Limited {
    pub bits: Vec<u8>, pub resolved: Arc<Resolved>, pub trigger: TriggerSpec,
    pub pre: f64, pub block_period: Option<f64>,
}

/// Latest acquisition limits for running intakes. Readers clone only the small settings and
/// an Arc under the lock; validation, storage changes and acquisition happen after releasing it.
#[derive(Default)]
pub struct LimitedSlot {
    generation: AtomicU64,
    value: Mutex<Option<Limited>>,
}

impl LimitedSlot {
    /// One UI producer publishes the latest settings to any number of intakes.
    pub fn set(&self, limited: Limited) {
        let old = {
            let mut value = self.value.lock().unwrap();
            let old = value.replace(limited);
            self.generation.fetch_add(1, Ordering::Release);
            old
        };
        drop(old);
    }

    pub fn generation(&self) -> u64 { self.generation.load(Ordering::Relaxed) }

    /// Read the current settings and their generation together, including for a later intake.
    pub fn read(&self) -> Option<(u64, Limited)> {
        let value = self.value.lock().unwrap();
        value.as_ref().map(|limited| (self.generation(), limited.clone()))
    }
}

struct Sampling { dt: f64, depth: usize, pre: f64, history_capacity: usize, block_period: f64 }

impl Limited {
    fn sampling(&self) -> Sampling {
        assert!(self.resolved.rate.is_finite() && self.resolved.rate > 0.0, "limited intake needs a positive finite rate");
        let dt = 1.0 / self.resolved.rate;
        assert!(dt.is_finite() && dt > 0.0, "limited intake needs a finite sample interval");
        let depth = usize::try_from(self.resolved.depth).expect("capture depth must fit in memory");
        assert!(depth > 0, "limited intake needs a nonempty capture");
        assert!(self.pre.is_finite() && self.pre >= 0.0, "pre-trigger time must be finite and nonnegative");
        assert!(self.block_period.is_none_or(|period| period.is_finite() && period >= 0.0), "block period must be finite and nonnegative");
        let history_capacity = if self.resolved.mode == Mode::Stream { 0 } else { ((self.pre / dt).round() as usize).min(depth - 1) };
        Sampling { dt, depth, pre: self.pre.min((depth - 1) as f64 * dt), history_capacity, block_period: self.block_period.unwrap_or(0.0) }
    }
}

#[derive(Clone, Copy)]
enum Phase { Armed, Capturing, Blind(f64) }

/// Samples input volts on a fixed clock, then delivers digitised captures in layout order.
/// Sample storage is allocated at `begin` and at a retune that changes its size, never per sample.
pub struct LimitedIntake<I: Intake> {
    inner: I, bits: Vec<u8>, resolved: Arc<Resolved>, trigger: TriggerSpec,
    dt: f64, depth: usize, pre: f64, history_capacity: usize, block_period: f64,
    limited_slot: Option<(Arc<LimitedSlot>, u64)>, layout: Vec<ChanId>,
    channels: Vec<Option<usize>>, trigger_channel: Option<usize>, detector: Option<EdgeDetector>,
    previous: Vec<f32>, previous_time: Option<f64>, anchor: f64, next_sample: u64,
    sample: Vec<f32>, sample_clipped: Vec<Clipped>, input: Vec<f32>,
    values: Vec<Vec<f32>>, clipped: Vec<Clipped>,
    history: Vec<Vec<f32>>, history_clipped: Vec<Vec<Clipped>>, history_times: Vec<f64>,
    history_head: usize, history_len: usize,
    phase: Phase, armed_at: Option<f64>, single_done: bool,
    count: usize, start: f64, capture_trigger: Option<f64>, index: u64, last_end: Option<f64>,
}

impl<I: Intake> LimitedIntake<I> {
    pub fn new(inner: I, bits: Vec<u8>, resolved: Arc<Resolved>, trigger: TriggerSpec, pre: f64, block_period: Option<f64>) -> Self {
        let limited = Limited { bits, resolved, trigger, pre, block_period };
        let Sampling { dt, depth, pre, history_capacity, block_period } = limited.sampling();
        Self { inner, bits: limited.bits, resolved: limited.resolved, trigger: limited.trigger, dt, depth, pre, history_capacity,
            block_period, limited_slot: None, layout: Vec::new(), channels: Vec::new(), trigger_channel: None, detector: None,
            previous: Vec::new(), previous_time: None, anchor: 0.0, next_sample: 0,
            sample: Vec::new(), sample_clipped: Vec::new(), input: Vec::new(),
            values: Vec::new(), clipped: Vec::new(),
            history: Vec::new(), history_clipped: Vec::new(), history_times: Vec::new(), history_head: 0, history_len: 0,
            phase: Phase::Armed, armed_at: None, single_done: false,
            count: 0, start: 0.0, capture_trigger: None, index: 0, last_end: None }
    }

    /// Take up the latest limits at a block boundary, finishing an open capture first.
    /// Sample storage is allocated at `begin` and at a retune that changes its size, never per sample.
    pub fn follow(&mut self, slot: Arc<LimitedSlot>) {
        self.limited_slot = Some((slot, 0));
        self.take_limited();
    }

    // True defers a pending retune until the open capture has been emitted.
    fn take_limited(&mut self) -> bool {
        let Some((slot, seen)) = &mut self.limited_slot else { return false };
        if slot.generation() == *seen { return false; }
        if matches!(self.phase, Phase::Capturing) { return true; }
        let Some((generation, limited)) = slot.read() else { return false };
        *seen = generation;
        let Sampling { dt, depth, pre, history_capacity, block_period } = limited.sampling();
        self.flush_stream();
        self.bits = limited.bits;
        self.resolved = limited.resolved;
        self.trigger = limited.trigger;
        self.dt = dt;
        self.depth = depth;
        self.pre = pre;
        self.history_capacity = history_capacity;
        self.block_period = block_period;
        if !self.previous.is_empty() {
            self.configure_layout();
            self.reset_window();
        }
        false
    }

    fn configure_layout(&mut self) {
        let n = self.layout.len();
        assert_eq!(self.bits.len(), n, "ADC bits must be in layout order");
        self.channels.resize(n, None);
        for (channel, id) in self.channels.iter_mut().zip(&self.layout) {
            *channel = self.resolved.channels.iter().position(|r| r.id == *id);
        }
        self.trigger_channel = self.layout.iter().position(|&id| id == self.trigger.channel);
        self.detector = if self.resolved.mode != Mode::Stream && self.resolved.trigger == TrigResolved::Hardware {
            let channel = self.resolved.channels.iter().find(|c| c.id == self.trigger.channel).expect("hardware trigger needs a resolved channel");
            let level = if channel.step > 0.0 { (self.trigger.level / channel.step).round() * channel.step } else { self.trigger.level };
            Some(EdgeDetector::new(EdgeTrigger { level, hyst: self.trigger.hyst, slope: self.trigger.slope, holdoff: self.trigger.holdoff }))
        } else { None };
        let capacity = if self.resolved.mode == Mode::Stream { STREAM_BATCH } else { self.depth };
        self.values.resize_with(n, Vec::new);
        self.history.resize_with(n, Vec::new);
        self.history_clipped.resize_with(n, Vec::new);
        for values in &mut self.values { if values.len() != capacity { values.resize(capacity, 0.0); } }
        for history in &mut self.history { if history.len() != self.history_capacity { history.resize(self.history_capacity, 0.0); } }
        for clipped in &mut self.history_clipped { if clipped.len() != self.history_capacity { clipped.resize(self.history_capacity, Clipped::default()); } }
        if self.history_times.len() != self.history_capacity { self.history_times.resize(self.history_capacity, 0.0); }
    }

    fn reset_window(&mut self) {
        self.previous_time = None;
        self.anchor = 0.0;
        self.next_sample = 0;
        self.count = 0;
        self.clipped.fill(Clipped::default());
        self.history_head = 0;
        self.history_len = 0;
        self.armed_at = None;
        self.capture_trigger = None;
        self.phase = if self.single_done { Phase::Blind(f64::INFINITY) } else { Phase::Armed };
        if let Some(detector) = &mut self.detector { detector.break_stream(); }
    }

    fn remember(&mut self, at: f64) {
        if self.history_capacity == 0 { return; }
        self.history_times[self.history_head] = at;
        for c in 0..self.sample.len() {
            self.history[c][self.history_head] = self.sample[c];
            self.history_clipped[c][self.history_head] = self.sample_clipped[c];
        }
        self.history_head = (self.history_head + 1) % self.history_capacity;
        self.history_len = (self.history_len + 1).min(self.history_capacity);
    }

    fn start_capture(&mut self, at: f64, trigger: Option<f64>, auto: bool) {
        self.start = at;
        for i in 0..self.history_len {
            let slot = (self.history_head + self.history_capacity - self.history_len + i) % self.history_capacity;
            if i == 0 { self.start = self.history_times[slot]; }
            for c in 0..self.sample.len() {
                self.values[c][i] = self.history[c][slot];
                self.clipped[c].include(self.history_clipped[c][slot]);
            }
        }
        self.count = self.history_len;
        self.history_len = 0;
        self.history_head = 0;
        self.capture_trigger = if auto { Some(self.start + self.pre) } else { trigger };
        self.phase = Phase::Capturing;
    }

    fn append(&mut self) {
        for c in 0..self.sample.len() {
            self.values[c][self.count] = self.sample[c];
            self.clipped[c].include(self.sample_clipped[c]);
        }
        self.count += 1;
    }

    fn emit(&mut self) {
        if self.count == 0 { return; }
        if self.resolved.mode != Mode::Stream {
            if let Some(end) = self.last_end { if self.start > end { self.inner.gap(end, self.start, Gap::Blind); } }
        }
        // One small header list per delivered block (never per sample).
        let chans: Vec<ChanData> = self.values.iter().map(|values| ChanData::F32(&values[..self.count])).collect();
        self.inner.block(&Block { index: self.index, t0: self.start, dt: self.dt, chans: &chans,
            trigger: self.capture_trigger, clipped: &self.clipped });
        self.index += self.count as u64;
        self.last_end = Some(self.start + self.count as f64 * self.dt);
        self.count = 0;
        self.clipped.fill(Clipped::default());
    }

    fn flush_stream(&mut self) {
        if self.resolved.mode == Mode::Stream { self.emit(); }
    }

    fn sample_at(&mut self, at: f64) {
        if self.single_done { return; }
        if self.resolved.mode == Mode::Stream {
            if self.count == 0 { self.start = at; }
            self.append();
            if self.count == STREAM_BATCH { self.emit(); }
            return;
        }
        if let Phase::Blind(until) = self.phase {
            if at < until { return; }
            self.phase = Phase::Armed;
            self.armed_at = None;
        }
        if let Phase::Armed = self.phase {
            let armed_at = *self.armed_at.get_or_insert(at);
            if self.resolved.trigger == TrigResolved::Hardware {
                let edge = match (self.detector.as_mut(), self.trigger_channel) {
                    (Some(detector), Some(c)) => detector.push(at, self.sample[c]),
                    _ => Edge::None,
                };
                if let Edge::Accepted(trigger) = edge { self.start_capture(at, Some(trigger), false); }
                else if matches!(self.trigger.run, TriggerRun::Auto(timeout) if at - armed_at >= timeout) {
                    self.start_capture(at, None, true);
                } else { self.remember(at); return; }
            } else { self.start_capture(at, None, false); }
        }
        self.append();
        if self.count == self.depth {
            let until = self.start + (self.depth as f64 * self.dt).max(self.block_period);
            self.emit();
            self.single_done = self.trigger.run == TriggerRun::Single;
            self.phase = Phase::Blind(if self.single_done { f64::INFINITY } else { until });
            if let Some(detector) = &mut self.detector { detector.break_stream(); }
        }
    }

    fn convert(&mut self, fraction: f64, y: &[f32]) {
        for c in 0..self.sample.len() {
            let (value, clipped) = if let Some(channel) = self.channels[c].map(|i| &self.resolved.channels[i]).filter(|c| c.on) {
                let a = self.previous[c] as f64;
                let b = y[c] as f64;
                digitise((a + (b - a) * fraction) as f32, channel, self.bits[c])
            } else { (f32::NAN, Clipped::default()) };
            self.sample[c] = value;
            self.sample_clipped[c] = clipped;
        }
    }
}

impl<I: Intake> Intake for LimitedIntake<I> {
    fn begin(&mut self, layout: &Layout) {
        self.flush_stream();
        let n = layout.channels.len();
        self.layout = layout.channels.iter().map(|c| c.id).collect();
        self.configure_layout();
        self.previous = vec![f32::NAN; n];
        self.sample = vec![f32::NAN; n];
        self.sample_clipped = vec![Clipped::default(); n];
        self.input = vec![f32::NAN; n];
        self.clipped = vec![Clipped::default(); n];
        self.index = 0;
        self.last_end = None;
        self.single_done = false;
        self.reset_window();
        self.inner.begin(layout);
    }

    fn point(&mut self, t: f64, y: &[f32]) {
        let retune_pending = self.take_limited();
        assert_eq!(y.len(), self.previous.len(), "point values must be in layout order");
        if !t.is_finite() { return; }
        if let Some(previous) = self.previous_time {
            // Two points at one time are a step (a breakpoint); only going back is a restart.
            if t < previous { self.gap(previous, t, Gap::Restart); }
        }
        if let Some(previous) = self.previous_time {
            loop {
                // Multiplication from the anchor avoids accumulated clock drift.
                let at = self.anchor + self.next_sample as f64 * self.dt;
                if at > t { break; }
                self.convert((at - previous) / (t - previous), y);
                self.sample_at(at);
                self.next_sample += 1;
                // A sparse source interval must not open another capture with the old limits.
                // Leave the new ADC clock to anchor at the next input point.
                if retune_pending && !matches!(self.phase, Phase::Capturing) { break; }
            }
        } else {
            self.anchor = t;
            self.next_sample = 1;
            self.previous.copy_from_slice(y);
            self.convert(0.0, y);
            self.sample_at(t);
        }
        self.previous.copy_from_slice(y);
        self.previous_time = Some(t);
    }

    fn block(&mut self, block: &Block) {
        assert_eq!(block.chans.len(), self.input.len(), "block channels must be in layout order");
        // Blocks are another point source; their trigger and clipping metadata are not ADC input.
        let mut input = std::mem::take(&mut self.input);
        let count = block.chans.first().map_or(0, ChanData::len);
        for i in 0..count {
            for (value, channel) in input.iter_mut().zip(block.chans) { *value = channel.value(i); }
            self.point(block.t0 + i as f64 * block.dt, &input);
        }
        self.input = input;
    }

    fn mark(&mut self, _: f64, _: Mark) {}
    fn gap(&mut self, from: f64, to: f64, why: Gap) {
        self.flush_stream();
        self.last_end = None;
        self.reset_window();
        self.inner.gap(from, to, why);
    }
    fn idle(&mut self) { self.flush_stream(); self.inner.idle(); }
    fn steady(&mut self, at: f64) { self.flush_stream(); self.inner.steady(at); }
    fn pending(&self) -> bool { self.inner.pending() }
}

impl<I: Intake> Drop for LimitedIntake<I> {
    fn drop(&mut self) { self.flush_stream(); }
}
