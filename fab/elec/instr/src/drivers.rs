use crate::{Signal, synth::Spread};
use fab_elec_bench::source::*;
use std::sync::Arc;
use std::time::{Duration, Instant};

const BATCH: usize = 1024;
/// A repeating ensemble of independent draws, one member per source period.
pub const MEMBER_CYCLE: u64 = 64;

#[derive(Clone, Debug)]
pub struct SynthChannel { pub signal: Signal, pub name: String }
#[derive(Clone, Debug)]
pub struct SynthConfig {
    /// Origin presented by this source (Real is useful for a hardware stand-in).
    pub side: Side,
    pub channels: Vec<SynthChannel>,
    /// XORed with each signal seed before drawing members.
    pub seed: u64, pub member_spread: Option<Spread>,
    /// Source seconds per wall second; None runs without a wall clock limit.
    pub pace: Option<f64>,
    /// Optional device restrictions; None preserves the exact synthetic source capabilities.
    pub limits: Option<Limits>,
}

/// Acquisition limits shared by every input channel of a synthetic bench instrument.
#[derive(Clone, Debug, PartialEq)]
pub struct Limits {
    /// Full-scale input volts (eight divisions), smallest first.
    pub ranges: Vec<f64>,
    pub bits: u8,
    pub rates: Vec<f64>,
    pub depth: u64,
    pub modes: Modes,
    pub hardware: Vec<TriggerKind>,
    /// Hardware trigger level granularity in ADC codes.
    pub level_step: u32,
    pub couplings: Vec<Coupling>,
    pub members: bool,
    pub current: bool,
}
impl Limits {
    pub fn caps(&self, channels: Vec<ChannelInfo>) -> Caps {
        let mut caps = exact_caps(&Layout { generation: 0, clock: ClockKind::Simulated, channels });
        for channel in &mut caps.channels {
            channel.ranges = Ranges::Steps(self.ranges.iter().map(|v| (-v * 0.5, v * 0.5)).collect());
            channel.bits = self.bits;
            channel.coupling = self.couplings.clone();
        }
        caps.rate = RateCaps::List(self.rates.clone());
        caps.depth = Depth::Samples(self.depth);
        caps.modes = self.modes;
        caps.trigger.hardware = self.hardware.clone();
        caps.powers = Powers { members: self.members, current: self.current, ..Powers::default() };
        caps
    }

    /// An eight-bit, 10 ksample block scope with 20 ms start-to-start readout cadence.
    pub fn bench_scope() -> Self {
        Self { ranges: vec![0.04, 0.08, 0.16, 0.4, 0.8, 1.6, 4.0, 8.0, 16.0, 40.0], bits: 8,
            rates: vec![1e3, 2e3, 5e3, 1e4, 2e4, 5e4, 1e5, 2e5, 5e5, 1e6, 2e6, 5e6, 1e7, 2e7, 25e6],
            depth: 10_000, modes: Modes { block: true, block_period: Some(0.02), ..Modes::default() },
            hardware: vec![TriggerKind::Edge], level_step: 1, couplings: vec![Coupling::Dc, Coupling::Ac], members: false, current: false }
    }
}

struct Pacer { pace: Option<f64>, start: Instant }
impl Pacer {
    fn new(pace: Option<f64>) -> Result<Self, DriverError> {
        if pace.is_some_and(|p| !p.is_finite() || p <= 0.0) { return Err(DriverError::Refused("pace must be positive and finite".into())); }
        Ok(Self { pace, start: Instant::now() })
    }
    fn reset(&mut self) { self.start = Instant::now(); }
    fn wait(&self, elapsed: f64) -> Option<Duration> {
        let left = elapsed / self.pace? - self.start.elapsed().as_secs_f64();
        (left > 0.0).then(|| Duration::from_secs_f64(left.min(3600.0)))
    }
}

pub struct Synth {
    config: SynthConfig, caps: Caps, layout: Layout, resolved: Option<Arc<Resolved>>,
    pacer: Pacer, index: u64, origin: f64, time: f64, announce: bool, stopped: bool,
    trigger: Option<TriggerSpec>, pre: f64, last_end: Option<f64>, last_trigger: Option<f64>, clipped: Vec<Clipped>,
    values: Vec<Vec<f32>>, members: Vec<Vec<Signal>>,
}
impl Synth {
    pub fn new(mut config: SynthConfig) -> Result<Self, DriverError> {
        let pacer = Pacer::new(config.pace)?;
        if config.channels.is_empty() { return Err(DriverError::Refused("synth needs channels".into())); }
        if let Some(limits) = &config.limits {
            if limits.bits == 0 || limits.bits > 24 || limits.depth == 0 || limits.level_step != 1
                || limits.ranges.is_empty() || limits.ranges.iter().any(|v| !v.is_finite() || *v <= 0.0)
                || limits.rates.is_empty() || limits.rates.iter().any(|v| !v.is_finite() || *v <= 0.0)
                || limits.modes.block_period.is_some_and(|v| !v.is_finite() || v <= 0.0)
            { return Err(DriverError::Refused("invalid synth limits (level steps must be one code)".into())); }
            if !limits.members { config.member_spread = None; }
        }
        let spread = config.member_spread.unwrap_or_default();
        for channel in &mut config.channels {
            if !channel.signal.valid(spread) { return Err(DriverError::Refused(format!("invalid signal {}", channel.name))); }
            // Members add up to MEMBER_CYCLE to the seed.
            let seed = (channel.signal.seed() ^ config.seed) % (u64::MAX - MEMBER_CYCLE);
            match &mut channel.signal { Signal::Synth(s) => s.seed = seed, Signal::Prbs(s) => s.seed = seed }
        }
        let layout = Layout { generation: 0, clock: ClockKind::Simulated, channels: config.channels.iter().enumerate()
            .map(|(i, c)| ChannelInfo { id: ChanId(i as u32), name: c.name.clone(), quantity: Quantity::Volt }).collect() };
        let caps = if let Some(limits) = &config.limits { limits.caps(layout.channels.clone()) }
        else {
            let mut caps = exact_caps(&layout);
            caps.rate = RateCaps::Range { lo: 1.0, hi: 1e9 };
            caps.modes.block = true;
            caps.powers.members = true;
            caps
        };
        let clipped = vec![Clipped::default(); config.channels.len()];
        let members = config.channels.iter().map(|c| if let Some(spread) = config.member_spread {
            (0..MEMBER_CYCLE).map(|k| c.signal.member(spread, k)).collect()
        } else { vec![c.signal.clone()] }).collect();
        let values = config.channels.iter().map(|_| Vec::with_capacity(BATCH)).collect();
        Ok(Self { config, caps, layout, resolved: None, pacer, index: 0, origin: 0.0, time: 0.0, announce: true, stopped: false, trigger: None, pre: 0.0, last_end: None, last_trigger: None, clipped, values, members })
    }
}
impl Driver for Synth {
    fn info(&self) -> SourceInfo { SourceInfo { id: SourceId(self.config.seed), side: self.config.side, name: if self.config.limits.is_some() { "Bench scope".into() } else if self.config.side == Side::Real { "Stand-in device".into() } else { "Synthetic".into() }, serial: None, clock: self.layout.clock } }
    fn caps(&self) -> Caps { self.caps.clone() }
    fn apply(&mut self, want: &Setup) -> Result<Resolved, DriverError> {
        valid_time(want)?;
        let result = resolve(&self.caps, want, self.resolved.as_deref());
        if result.restart {
            self.origin = self.time;
            self.index = 0;
            self.layout.generation += 1;
            self.announce = true;
        }
        if self.resolved.is_none() { self.pacer.reset(); }
        self.trigger = Some(want.trigger.clone());
        self.pre = want.time.pre;
        if result.restart { self.last_end = None; self.last_trigger = None; }
        self.resolved = Some(Arc::new(result.clone()));
        self.stopped = false;
        Ok(result)
    }
    fn pump(&mut self, out: &mut dyn BlockOut) -> Pump {
        let Some(resolved) = &self.resolved else { return Pump::Idle(Duration::from_millis(10)) };
        if self.stopped { return Pump::Idle(Duration::from_millis(10)); }
        if let Some(wait) = self.pacer.wait(self.time) { return Pump::Idle(wait); }
        if self.announce { out.layout(&self.layout); self.announce = false; }
        let dt = 1.0 / resolved.rate;
        let block = self.config.limits.is_some() && resolved.mode != Mode::Stream;
        let mut t0 = if block { self.time } else { self.origin + self.index as f64 * dt };
        let count = if block { resolved.depth.max(1) as usize } else { BATCH.min((resolved.rate * 0.01).ceil().max(1.0) as usize) };
        let mut trigger = None;
        if block && resolved.trigger == TrigResolved::Hardware {
            let spec = self.trigger.as_ref().unwrap();
            let channel = resolved.channels.iter().find(|c| c.id == spec.channel).unwrap();
            let signal = &self.members[spec.channel.0 as usize][0];
            let level = (spec.level / channel.step).round() * channel.step;
            let mut detector = fab_elec_bench::trigger::EdgeDetector::new(fab_elec_bench::trigger::EdgeTrigger {
                level, hyst: spec.hyst, slope: spec.slope, holdoff: spec.holdoff });
            let pre = self.pre.min((count - 1) as f64 * dt);
            // A bounded search per pump; waiting advances the source clock without inventing an edge.
            for i in 0..count {
                let t = t0 + pre + i as f64 * dt;
                let (value, _) = digitise(input_sample(signal, t, self.index + i as u64, channel.coupling), channel, self.caps.channels[spec.channel.0 as usize].bits);
                if let fab_elec_bench::trigger::Edge::Accepted(at) = detector.push(t, value) {
                    if self.last_trigger.is_none_or(|last| at - last >= spec.holdoff) { trigger = Some(at); break; }
                }
            }
            if let Some(at) = trigger { t0 = at - pre; self.last_trigger = Some(at); }
            else if matches!(spec.run, TriggerRun::Auto(_)) { trigger = Some(t0 + pre); }
            else { self.time += count as f64 * dt; return Pump::Data; }
        }
        if block {
            if let Some(end) = self.last_end { if t0 > end { out.gap(end, t0, Gap::Blind); } }
        }
        self.clipped.fill(Clipped::default());
        for (c, values) in self.values.iter_mut().enumerate() {
            values.clear();
            let period = self.config.channels[c].signal.period();
            let channel = resolved.channels.iter().find(|r| r.id == ChanId(c as u32) && r.on);
            for i in 0..count {
                let t = t0 + i as f64 * dt;
                let (signal, time) = if self.config.member_spread.is_some() {
                    let mut cycle = (t / period).floor() as u64;
                    if (cycle + 1) as f64 * period <= t { cycle += 1; }
                    else if cycle as f64 * period > t { cycle = cycle.saturating_sub(1); }
                    (&self.members[c][(cycle % MEMBER_CYCLE) as usize], t - cycle as f64 * period)
                } else { (&self.members[c][0], t) };
                let noise_index = if self.config.member_spread.is_some() { (time / dt).round() as u64 } else { self.index + i as u64 };
                values.push(if let Some(channel) = channel {
                    if self.config.limits.is_some() {
                        let value = input_sample(signal, time, noise_index, channel.coupling);
                        let (value, clipped) = digitise(value, channel, self.caps.channels[c].bits);
                        self.clipped[c].include(clipped);
                        value
                    } else { (sample(signal, time, noise_index) as f64 * channel.probe.attenuation * channel.probe.transducer * if channel.invert { -1.0 } else { 1.0 }) as f32 }
                } else { f32::NAN });
            }
        }
        let chans: Vec<_> = self.values.iter().map(|v| ChanData::F32(v)).collect();
        out.block(&Block { index: self.index, t0, dt, chans: &chans, trigger, clipped: &self.clipped });
        self.index += count as u64;
        self.time = if block {
            self.last_end = Some(t0 + count as f64 * dt);
            t0 + (count as f64 * dt).max(self.caps.modes.block_period.unwrap_or(0.0))
        } else { self.origin + self.index as f64 * dt };
        Pump::Data
    }
    fn stop(&mut self) { self.stopped = true; }
}

#[derive(Clone, Debug)]
pub struct Record { pub layout: Layout, pub times: Vec<f64>, pub lanes: Vec<Vec<f32>>, pub marks: Vec<(f64, Mark)> }
pub struct Replay {
    record: Record, caps: Caps, resolved: Option<Arc<Resolved>>, pacer: Pacer,
    cursor: usize, mark: usize, pass: u64, span: f64, announce: bool, stopped: bool, values: Vec<f32>,
}
impl Replay {
    pub fn new(record: Record, pace: Option<f64>) -> Result<Self, DriverError> {
        let pacer = Pacer::new(pace)?;
        let n = record.times.len();
        if n == 0 || record.lanes.is_empty() || record.layout.channels.len() != record.lanes.len()
            || record.lanes.iter().any(|v| v.len() != n) || record.times.iter().any(|t| !t.is_finite())
            || record.times.windows(2).any(|t| t[0] >= t[1])
            || record.marks.windows(2).any(|m| m[0].0 > m[1].0)
            || record.marks.iter().any(|(t, _)| !t.is_finite() || record.times.binary_search_by(|v| v.total_cmp(t)).is_err())
        { return Err(DriverError::Refused("record needs ordered finite times, matching lanes, and marks at sample times".into())); }
        let mut caps = exact_caps(&record.layout);
        let dt = if n > 1 { (record.times[n - 1] - record.times[0]) / (n - 1) as f64 } else { 1.0 };
        if !dt.is_finite() || dt <= 0.0 || !(1.0 / dt).is_finite() { return Err(DriverError::Refused("record interval is out of range".into())); }
        caps.rate = RateCaps::List(vec![1.0 / dt]);
        caps.depth = Depth::Samples(n as u64);
        caps.powers.rewind = true;
        let span = record.times[n - 1] - record.times[0] + dt;
        if !span.is_finite() { return Err(DriverError::Refused("record duration is out of range".into())); }
        let values = vec![0.0; record.lanes.len()];
        Ok(Self { record, caps, resolved: None, pacer, cursor: 0, mark: 0, pass: 0, span, announce: true, stopped: false, values })
    }
}
impl Driver for Replay {
    fn info(&self) -> SourceInfo { SourceInfo { id: SourceId(self.record.layout.generation), side: Side::Replay, name: "Replay".into(), serial: None, clock: self.record.layout.clock } }
    fn caps(&self) -> Caps { self.caps.clone() }
    fn apply(&mut self, want: &Setup) -> Result<Resolved, DriverError> {
        valid_time(want)?;
        let result = resolve(&self.caps, want, self.resolved.as_deref());
        if self.resolved.is_none() { self.pacer.reset(); }
        self.resolved = Some(Arc::new(result.clone()));
        self.stopped = false;
        Ok(result)
    }
    fn pump(&mut self, out: &mut dyn BlockOut) -> Pump {
        if self.stopped || self.resolved.is_none() { return Pump::Idle(Duration::from_millis(10)); }
        let first = self.record.times[0];
        let due = self.pass as f64 * self.span + self.record.times[self.cursor] - first;
        if let Some(wait) = self.pacer.wait(due) { return Pump::Idle(wait); }
        if self.announce { out.layout(&self.record.layout); self.announce = false; }
        if self.cursor == 0 && self.pass > 0 {
            let last = *self.record.times.last().unwrap();
            out.gap(last, last, Gap::Restart);
        }
        let mut count = 0;
        while count < BATCH {
            let t = self.record.times[self.cursor];
            if self.pacer.wait(self.pass as f64 * self.span + t - first).is_some() && count > 0 { break; }
            for (v, lane) in self.values.iter_mut().zip(&self.record.lanes) { *v = lane[self.cursor]; }
            out.point(t, &self.values);
            while self.mark < self.record.marks.len() && self.record.marks[self.mark].0 == t {
                let (t, kind) = self.record.marks[self.mark]; out.mark(t, kind); self.mark += 1;
            }
            self.cursor += 1;
            count += 1;
            if self.cursor == self.record.times.len() { self.cursor = 0; self.mark = 0; self.pass += 1; break; }
        }
        Pump::Data
    }
    fn stop(&mut self) { self.stopped = true; }
}

fn exact_caps(layout: &Layout) -> Caps {
    Caps { generation: layout.generation, channels: layout.channels.iter().map(|c| ChannelCaps {
        id: c.id, name: c.name.clone(), kind: if c.quantity == Quantity::Logic { ChanKind::Logic { bits: 1 } } else { ChanKind::Analog },
        quantity: c.quantity, ranges: Ranges::Any, bits: 0, coupling: vec![Coupling::Dc], attenuation: Atten::Free,
        bandwidth: None, bw_limits: Vec::new(),
    }).collect(), rate: RateCaps::Any, depth: Depth::Unbounded,
        modes: Modes { stream: true, ..Modes::default() }, stream_rate: None,
        trigger: TriggerCaps::default(), powers: Powers { zero_load: true, exact_marks: true, ..Powers::default() }, outputs: Vec::new() }
}
fn valid_time(want: &Setup) -> Result<(), DriverError> {
    let rate = match want.time.rate { RateWant::Exact(r) => r, RateWant::Auto { per_px } => per_px * want.time.plot_px as f64 / want.time.span };
    if !want.trigger.level.is_finite() || !want.trigger.hyst.is_finite() || want.trigger.hyst < 0.0
        || !want.trigger.holdoff.is_finite() || want.trigger.holdoff < 0.0
        || !want.time.pre.is_finite() || want.time.pre < 0.0
        || matches!(want.time.depth, DepthWant::Samples(0))
        || !want.time.span.is_finite() || want.time.span <= 0.0 || !rate.is_finite() || rate <= 0.0
        || want.channels.iter().any(|c| !c.probe.attenuation.is_finite() || c.probe.attenuation <= 0.0
            || !c.probe.transducer.is_finite() || c.probe.transducer == 0.0 || !c.window.0.is_finite() || !c.window.1.is_finite() || c.window.0 >= c.window.1)
    {
        Err(DriverError::Refused("time, trigger, probe scaling and channel windows must be finite and valid".into()))
    } else { Ok(()) }
}
fn sample(signal: &Signal, time: f64, index: u64) -> f32 {
    let (value, noise) = match signal { Signal::Synth(s) => (s.value(time), s.noise), Signal::Prbs(s) => (s.value(time), s.noise) };
    if noise == 0.0 { return value as f32; }
    fn unit(mut bits: u64) -> f64 {
        bits = bits.wrapping_add(0x9e37_79b9_7f4a_7c15);
        bits = (bits ^ (bits >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        bits = (bits ^ (bits >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        ((bits ^ (bits >> 31)) >> 12) as f64 / 4_503_599_627_370_496.0 + 0.5 / 4_503_599_627_370_496.0
    }
    let key = signal.seed().wrapping_add(index.wrapping_mul(2));
    let gaussian = (-2.0 * unit(key).ln()).sqrt() * (std::f64::consts::TAU * unit(key.wrapping_add(1))).cos();
    (value + noise * gaussian) as f32
}

// Ideal AC coupling removes the periodic source's DC component before conversion.
fn input_sample(signal: &Signal, time: f64, index: u64, coupling: Coupling) -> f32 {
    let value = sample(signal, time, index);
    match coupling {
        Coupling::Dc => value, Coupling::Gnd => 0.0,
        Coupling::Ac => value - match signal {
            Signal::Synth(s) => (s.lo + (s.hi - s.lo) * s.duty) as f32,
            Signal::Prbs(s) => (s.lo + (s.hi - s.lo) * (1_u64 << (s.order - 1)) as f64 / ((1_u64 << s.order) - 1) as f64) as f32,
        },
    }
}
