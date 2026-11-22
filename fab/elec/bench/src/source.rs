//! Source-neutral acquisition input, capabilities, and view-to-source setup policy.

use std::sync::Arc;
use std::time::Duration;
use crate::trigger::Slope;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ChanId(pub u32);

/// Physical quantity. Derived power needs a future simulator Sink extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quantity { Volt, Amp, Logic }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelInfo { pub id: ChanId, pub name: String, pub quantity: Quantity }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockKind { Simulated, Device }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout { pub generation: u64, pub clock: ClockKind, pub channels: Vec<ChannelInfo> }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Solver, HardwareTrigger, Segment,
    /// From this point on the values are held, not observed: a steady source (a settled simulation)
    /// stopped its clock, and the samples after this mark repeat the last values.
    Held,
    /// The step from the sample before this point to this one is below the simulation's time
    /// resolution (`Sink::below_res`): what happened within it is not resolved.
    BelowRes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gap { Overrun, Rearm, Blind, Restart }

/// Sample i of every channel is at `t0 + i * dt`; channels have equal lengths.
/// A hardware trigger opens exactly that window and suppresses software search for this block.
pub struct Block<'a> { pub index: u64, pub t0: f64, pub dt: f64, pub chans: &'a [ChanData<'a>], pub trigger: Option<f64>,
    /// Measured saturation per channel in lane units; empty means no reported clipping.
    pub clipped: &'a [Clipped],
}

/// The ends actually saturated in a capture, independent of the requested view window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Clipped { pub low: bool, pub high: bool }
impl Clipped {
    pub fn any(self) -> bool { self.low || self.high }
    pub fn include(&mut self, other: Self) { self.low |= other.low; self.high |= other.high; }
}

pub enum ChanData<'a> {
    F32(&'a [f32]),
    I16 { raw: &'a [i16], scale: f32, offset: f32 },
    U8 { raw: &'a [u8], scale: f32, offset: f32 },
    I8 { raw: &'a [i8], scale: f32, offset: f32 },
}

impl ChanData<'_> {
    pub fn len(&self) -> usize {
        match self { Self::F32(raw) => raw.len(), Self::I16 { raw, .. } => raw.len(), Self::U8 { raw, .. } => raw.len(), Self::I8 { raw, .. } => raw.len() }
    }

    pub fn is_empty(&self) -> bool { self.len() == 0 }

    /// Decode one value in physical units, applying scale then offset.
    pub fn value(&self, i: usize) -> f32 {
        match self {
            Self::F32(raw) => raw[i],
            Self::I16 { raw, scale, offset } => raw[i] as f32 * scale + offset,
            Self::U8 { raw, scale, offset } => raw[i] as f32 * scale + offset,
            Self::I8 { raw, scale, offset } => raw[i] as f32 * scale + offset,
        }
    }
}

/// One instant's values or a uniform run feed one trigger and all channel rasters.
/// There is no required end-of-stream call: solver workers can drop without a final idle.
/// Device workers send a final idle; buffered implementations must also publish on Drop.
pub trait Intake: Send {
    /// Called before any point and again on a new layout; ends all previous open windows.
    fn begin(&mut self, layout: &Layout);
    /// Values in layout order. Times strictly increase within a layout; a time at or before
    /// the previous time defensively restarts windows. NaN means no value on this channel
    /// at this instant: segments touching it draw nothing, other channels are unaffected.
    fn point(&mut self, t: f64, y: &[f32]);
    fn block(&mut self, b: &Block);
    /// An exact event, immediately after point(t) with the bit-identical t.
    /// Solver marks denote accepted source breakpoints or localised switching instants.
    fn mark(&mut self, t: f64, kind: Mark);
    /// No samples exist from from to to; never connect samples across this interval.
    fn gap(&mut self, from: f64, to: f64, why: Gap);
    /// Nothing follows for now: publish what is held, preserving unfinished windows.
    fn idle(&mut self);
    /// The source is steady from `at` (its last point): every value holds until it runs again,
    /// and its clock stops. An intake may complete its windows from the held values, marking
    /// them `Mark::Held`; the default ignores it. `idle` follows.
    fn steady(&mut self, _at: f64) {}
    /// True while the intake holds output its consumer refused (a full channel). A source that
    /// waits calls `idle` again, backing off, until this is false: nothing is stranded at rest.
    fn pending(&self) -> bool { false }
}

// Hosts can put a dynamically selected intake directly behind SimIntake's generic adapter.
impl<I: Intake + ?Sized> Intake for Box<I> {
    fn begin(&mut self, layout: &Layout) { (**self).begin(layout); }
    fn point(&mut self, t: f64, y: &[f32]) { (**self).point(t, y); }
    fn block(&mut self, block: &Block) { (**self).block(block); }
    fn mark(&mut self, t: f64, kind: Mark) { (**self).mark(t, kind); }
    fn gap(&mut self, from: f64, to: f64, why: Gap) { (**self).gap(from, to, why); }
    fn idle(&mut self) { (**self).idle(); }
    fn steady(&mut self, at: f64) { (**self).steady(at); }
    fn pending(&self) -> bool { (**self).pending() }
}

/// A source for this session (the simulator, a synthetic or replayed source, a device).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SourceId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OutputId(pub u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AttachId(pub u64);
/// Where the samples come from: computed, measured on hardware, or played back from a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side { Sim, Real, Replay }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceInfo { pub id: SourceId, pub side: Side, pub name: String, pub serial: Option<String>, pub clock: ClockKind }

/// What a source can do, read once at open: the input `resolve` answers from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChanKind { Analog, Logic { bits: u8 } }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coupling { Dc, Ac, Gnd }
#[derive(Clone, Debug, PartialEq)]
pub enum Ranges { Any, Steps(Vec<(f64, f64)>) }
#[derive(Clone, Debug, PartialEq)]
pub enum Atten { Fixed(Vec<f64>), Sensed(f64), Free }
#[derive(Clone, Debug, PartialEq)]
pub enum RateCaps { Any, List(Vec<f64>), Range { lo: f64, hi: f64 } }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Depth { Unbounded, Samples(u64) }
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelCaps {
    pub id: ChanId, pub name: String, pub kind: ChanKind, pub quantity: Quantity,
    pub ranges: Ranges, pub bits: u8, pub coupling: Vec<Coupling>, pub attenuation: Atten,
    pub bandwidth: Option<f64>, pub bw_limits: Vec<f64>,
}
/// `block_period` is the shortest time between the starts of two blocks (arming plus readout):
/// the blind share of a software trigger over blocks comes from it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Modes { pub stream: bool, pub block: bool, pub segmented: Option<u32>, pub block_period: Option<f64> }
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Powers {
    pub probe_any_net: bool, pub current: bool, pub power: bool, pub rewind: bool,
    pub zero_load: bool, pub members: bool, pub exact_marks: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerKind { Edge, Width, Runt, Zone, Pattern, Mark, Bound }
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TriggerCaps {
    pub hardware: Vec<TriggerKind>, pub pre: Option<(f64, f64)>,
    pub holdoff: Option<(f64, f64)>, pub external: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputKind { Analog, Digital }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriveKind { Dc, Square, Pulse, Record }
#[derive(Clone, Debug, PartialEq)]
pub struct OutputCaps {
    pub id: OutputId, pub name: String, pub kind: OutputKind, pub shapes: Vec<DriveKind>,
    pub rate: (f64, f64), pub level: (f64, f64),
    /// Rated load in ohms; None means unspecified.
    pub load: Option<f64>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Caps {
    pub generation: u64, pub channels: Vec<ChannelCaps>, pub rate: RateCaps, pub depth: Depth,
    pub modes: Modes,
    /// Maximum streaming rate, or None when the ordinary rate caps also apply to streaming.
    pub stream_rate: Option<f64>,
    pub trigger: TriggerCaps, pub powers: Powers, pub outputs: Vec<OutputCaps>,
}

/// What the view asks for, in view terms: a value window per lane (lane units: input volts times
/// attenuation times transducer) and a time span; never volts per division.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Probe { pub attenuation: f64, pub transducer: f64 }
impl Default for Probe { fn default() -> Self { Self { attenuation: 1.0, transducer: 1.0 } } }
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelSetup {
    pub id: ChanId, pub on: bool, pub window: (f64, f64), pub coupling: Coupling,
    pub probe: Probe, pub bw_limit: Option<f64>, pub invert: bool,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RateWant { Auto { per_px: f64 }, Exact(f64) }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DepthWant { Auto, Samples(u64) }
/// `span` and `pre` are seconds; `pre` is the requested time before the trigger.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimeSetup { pub span: f64, pub pre: f64, pub rate: RateWant, pub depth: DepthWant, pub plot_px: u32 }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeWant { Auto, Stream, Block, Segmented(u32) }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode { Stream, Block, Segmented(u32) }
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TriggerRun { Auto(f64), Normal, Single }
#[derive(Clone, Debug, PartialEq)]
pub struct TriggerSpec {
    pub kind: TriggerKind, pub channel: ChanId, pub level: f64, pub hyst: f64,
    pub slope: Slope, pub holdoff: f64, pub run: TriggerRun,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Drive {
    Dc(f64),
    Square { frequency: f64, duty: f64, lo: f64, hi: f64 },
    Pulse { frequency: f64, duty: f64, lo: f64, hi: f64 },
    Record { rate: f64, values: Arc<[f32]> },
}
impl Drive {
    pub fn kind(&self) -> DriveKind {
        match self { Self::Dc(_) => DriveKind::Dc, Self::Square { .. } => DriveKind::Square, Self::Pulse { .. } => DriveKind::Pulse, Self::Record { .. } => DriveKind::Record }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct DriveSetup { pub id: OutputId, pub on: bool, pub drive: Drive }
#[derive(Clone, Debug, PartialEq)]
pub struct Setup {
    pub channels: Vec<ChannelSetup>, pub time: TimeSetup, pub trigger: TriggerSpec,
    pub mode: ModeWant, pub drives: Vec<DriveSetup>,
}
/// What the source will really do for a `Setup`, with a note for every place it differs from the
/// ask. `restart` is true when the record cannot continue across the change.
#[derive(Clone, Debug, PartialEq)]
pub struct ChanResolved {
    pub id: ChanId, pub on: bool, pub window: (f64, f64),
    /// Full-scale range in input volts; step is in lane units.
    pub range: (f64, f64), pub step: f64, pub clipped: bool,
    pub coupling: Coupling, pub probe: Probe, pub bw_limit: Option<f64>, pub invert: bool,
}

// ADC codes cover [lo, hi): the upper saturation code is hi minus one LSB.
pub fn digitise(value: f32, channel: &ChanResolved, bits: u8) -> (f32, Clipped) {
    let codes = 2_f64.powi(bits as i32);
    let step = (channel.range.1 - channel.range.0) / codes;
    let value = value as f64;
    let mut clipped = Clipped { low: value < channel.range.0, high: value >= channel.range.1 };
    let code = ((value - channel.range.0) / step).round().clamp(0.0, codes - 1.0);
    let scale = channel.probe.attenuation * channel.probe.transducer * if channel.invert { -1.0 } else { 1.0 };
    if scale < 0.0 { std::mem::swap(&mut clipped.low, &mut clipped.high); }
    (((channel.range.0 + code * step) * scale) as f32, clipped)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Over { Stream, Blocks { blind: f32 } }
#[derive(Clone, Debug, PartialEq)]
pub enum TrigResolved { Hardware, Software { over: Over }, Unavailable(String) }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteKind { Rate, Depth, Range(ChanId), Trigger, Drive(OutputId), Mode }
#[derive(Clone, Debug, PartialEq)]
pub struct Note { pub what: NoteKind, pub text: String }
#[derive(Clone, Debug, PartialEq)]
pub struct DriveResolved {
    pub id: OutputId, pub on: bool, pub drive: Drive,
    pub clamped: bool, pub refused: Option<String>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub rate: f64, pub depth: u64, pub mode: Mode, pub channels: Vec<ChanResolved>,
    pub trigger: TrigResolved, pub drives: Vec<DriveResolved>, pub notes: Vec<Note>, pub restart: bool,
}
/// The source's state and rates as the UI reads them each frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State { Opening, Idle, Armed, Waiting, Firing, Streaming, Paused, Gone }
#[derive(Clone, Debug)]
pub struct Status {
    pub state: State, pub resolved: Option<Arc<Resolved>>, pub sample_rate: f64,
    /// Source-reported trigger events per wall second. Intake software events belong to its feed.
    pub trigger_rate: f64,
    /// Fraction of produced sample time retained in the last wall second; 1 if none was dropped.
    pub coverage: f32, pub gaps: u64, pub error: Option<String>,
}

/// Construct an intake on the acquisition worker whenever the source layout changes.
pub trait IntakeFactory: Send { fn make(&mut self, layout: &Layout) -> Box<dyn Intake>; }
/// The UI's handle on a source. Every call returns at once: no method waits for device I/O,
/// a worker or intake processing. `resolve` previews an answer without sending anything.
pub trait Source {
    fn info(&self) -> &SourceInfo;
    fn caps(&self) -> Arc<Caps>;
    fn resolve(&self, want: &Setup) -> Resolved;
    fn request(&mut self, want: Setup);
    fn status(&self) -> Status;
    fn attach(&mut self, intake: Box<dyn IntakeFactory + Send>) -> AttachId;
    fn detach(&mut self, id: AttachId);
}
/// What a driver writes into on its device worker; it never sees the ring or the intake.
pub trait BlockOut {
    fn layout(&mut self, layout: &Layout);
    fn block(&mut self, block: &Block);
    fn point(&mut self, t: f64, values: &[f32]);
    fn mark(&mut self, t: f64, kind: Mark);
    fn gap(&mut self, from: f64, to: f64, why: Gap);
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DriverError { Gone(String), Refused(String), Io(String) }
/// One bounded driver step: data written, nothing for a while (the worker may wait this long on
/// its commands), or the device is gone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pump { Data, Idle(Duration), Gone(String) }
/// Blocking device operations belong exclusively to a device worker.
pub trait Driver: Send {
    /// In-memory metadata only: DeviceSource caches these two accessors at open.
    fn info(&self) -> SourceInfo;
    fn caps(&self) -> Caps;
    /// Configure the device. The worker decides `restart` against the answer in force.
    fn apply(&mut self, want: &Setup) -> Result<Resolved, DriverError> { Ok(resolve(&self.caps(), want, None)) }
    fn pump(&mut self, out: &mut dyn BlockOut) -> Pump;
    fn stop(&mut self);
}

/// Resolve view units into acquisition settings; `restart` compares with `in_force`, the answer
/// the source runs now (None: a first start). The request must have a positive span, rate,
/// attenuation and nonzero transducer. An empty list in `caps` (attenuations, couplings, ranges,
/// rates) constrains nothing: the ask is kept, never a panic.
pub fn resolve(caps: &Caps, want: &Setup, in_force: Option<&Resolved>) -> Resolved {
    let mut notes = Vec::new();
    let mut note = |what, text| notes.push(Note { what, text });
    let mut channels = Vec::with_capacity(want.channels.len());
    for ask in &want.channels {
        let Some(cap) = caps.channels.iter().find(|c| c.id == ask.id) else {
            note(NoteKind::Range(ask.id), "channel is unavailable".into());
            continue;
        };
        let mut probe = ask.probe;
        probe.attenuation = match &cap.attenuation {
            Atten::Free => probe.attenuation,
            Atten::Sensed(value) => *value,
            Atten::Fixed(values) => values.iter().copied().min_by(|a, b| (a - probe.attenuation).abs().total_cmp(&(b - probe.attenuation).abs()).then(a.total_cmp(b))).unwrap_or(probe.attenuation),
        };
        if ask.on && probe.attenuation != ask.probe.attenuation {
            note(NoteKind::Range(ask.id), format!("attenuation {} uses {}{}", ask.probe.attenuation, probe.attenuation, if matches!(cap.attenuation, Atten::Sensed(_)) { " (sensed)" } else { " (offered)" }));
        }
        let coupling = if cap.coupling.contains(&ask.coupling) { ask.coupling } else {
            [Coupling::Dc, Coupling::Ac, Coupling::Gnd].into_iter().find(|c| cap.coupling.contains(c)).unwrap_or(ask.coupling)
        };
        if ask.on && coupling != ask.coupling { note(NoteKind::Range(ask.id), format!("coupling {:?} uses {:?}", ask.coupling, coupling)); }
        let scale = probe.attenuation * probe.transducer;
        let a = ask.window.0 / scale;
        let b = ask.window.1 / scale;
        let input = (a.min(b), a.max(b));
        let range = match &cap.ranges {
            Ranges::Any => input,
            Ranges::Steps(ranges) => ranges.iter().filter(|r| r.0 <= input.0 && r.1 >= input.1)
                .min_by(|a, b| (a.1 - a.0).total_cmp(&(b.1 - b.0)))
                .or_else(|| ranges.iter().max_by(|a, b| (a.1 - a.0).total_cmp(&(b.1 - b.0))))
                .copied().unwrap_or(input),
        };
        let clipped = ask.on && (input.0 < range.0 || input.1 > range.1);
        if clipped {
            let end = if input.0 < range.0 && input.1 > range.1 { format!("below {} V and above {} V", range.0, range.1) }
                else if input.0 < range.0 { format!("below {} V", range.0) } else { format!("above {} V", range.1) };
            note(NoteKind::Range(ask.id), format!("view {} to {} V exceeds the {} V range: clipped {end}", input.0, input.1, range.1 - range.0));
        }
        if ask.on && range != input && !clipped {
            note(NoteKind::Range(ask.id), format!("view {} to {} V uses the {} to {} V input range", input.0, input.1, range.0, range.1));
        }
        channels.push(ChanResolved { id: ask.id, on: ask.on, window: ask.window, range,
            step: if cap.bits == 0 { 0.0 } else { (range.1 - range.0) * scale.abs() / 2_f64.powi(cap.bits as i32) },
            clipped, coupling, probe, bw_limit: ask.bw_limit, invert: ask.invert });
    }
    let wanted = match want.time.rate { RateWant::Exact(r) => r, RateWant::Auto { per_px } => per_px * want.time.plot_px as f64 / want.time.span };
    let mut rate = snap_rate(&caps.rate, wanted, false);
    if rate != wanted { note(NoteKind::Rate, format!("wanted {}; using {}", rate_text(wanted), rate_text(rate))); }
    let limit = match (caps.depth, want.time.depth) {
        (Depth::Samples(a), DepthWant::Samples(b)) => Some(a.min(b)),
        (Depth::Samples(a), _) | (_, DepthWant::Samples(a)) => Some(a),
        _ => None,
    };
    let needed = (rate * want.time.span).ceil() as u64;
    let mut depth = if let Some(max) = limit.filter(|&max| needed > max) {
        let before = rate;
        rate = snap_rate(&caps.rate, max as f64 / want.time.span, true);
        note(NoteKind::Depth, format!("needed {needed} samples; keeping {max} samples and lowering rate from {} to {}", rate_text(before), rate_text(rate)));
        if rate * want.time.span > max as f64 {
            note(NoteKind::Depth, format!("minimum offered rate {} cannot cover the whole view in {max} samples", rate_text(rate)));
        }
        max
    } else { needed };
    let stream = caps.modes.stream && caps.stream_rate.is_none_or(|max| rate <= max);
    let mode = match want.mode {
        ModeWant::Auto if stream => Mode::Stream,
        ModeWant::Auto if caps.modes.block => Mode::Block,
        ModeWant::Stream if stream => Mode::Stream,
        ModeWant::Block if caps.modes.block => Mode::Block,
        ModeWant::Segmented(n) if caps.modes.segmented.is_some() => {
            let count = n.max(1).min(caps.modes.segmented.unwrap());
            if count != n { note(NoteKind::Mode, format!("requested {n} segments; using {count}")); }
            Mode::Segmented(count)
        }
        _ => {
            let mode = if caps.modes.block { Mode::Block } else { Mode::Stream };
            note(NoteKind::Mode, format!("requested {:?} unavailable at {}; using {mode:?}", want.mode, rate_text(rate)));
            mode
        }
    };
    if mode == Mode::Stream && !stream {
        if let Some(max) = caps.stream_rate {
            let before = rate;
            rate = snap_rate(&caps.rate, max, true);
            note(NoteKind::Rate, format!("stream limit lowers {} to {}", rate_text(before), rate_text(rate)));
            depth = depth.min((rate * want.time.span).ceil() as u64);
        }
    }
    let channel = channels.iter().find(|c| c.id == want.trigger.channel && c.on);
    let trigger = if want.trigger.kind == TriggerKind::Pattern && !caps.channels.iter().any(|c| matches!(c.kind, ChanKind::Logic { .. })) {
        TrigResolved::Unavailable("pattern trigger needs logic channels".into())
    } else if channel.is_none() {
        TrigResolved::Unavailable("trigger channel is off or unavailable".into())
    } else {
        let channel = channel.unwrap();
        let tolerance = (if want.trigger.hyst > 0.0 { want.trigger.hyst } else { (channel.window.1 - channel.window.0).abs() * 0.01 }) * 0.25;
        if caps.trigger.hardware.contains(&want.trigger.kind) && channel.step <= tolerance {
            TrigResolved::Hardware
        } else if mode == Mode::Stream {
            TrigResolved::Software { over: Over::Stream }
        } else {
            let needed = (rate * want.time.span).ceil() as u64;
            depth = needed.saturating_mul(2).min(limit.unwrap_or(u64::MAX));
            if depth > needed {
                note(NoteKind::Depth, format!("record extended to {depth} samples beyond the {needed}-sample view to search for a software trigger"));
            } else {
                note(NoteKind::Depth, format!("software trigger search needs a record longer than the {needed}-sample view; depth limit allows {depth} samples"));
            }
            let blind = if let Some(period) = caps.modes.block_period { (1.0 - depth as f64 / rate / period).clamp(0.0, 1.0) as f32 }
                else { note(NoteKind::Trigger, "software trigger over free-running blocks: blind time is not known".into()); 0.0 };
            if caps.modes.block_period.is_some() {
                note(NoteKind::Trigger, format!("software trigger over free-running blocks: blind {:.1} %", blind * 100.0));
            }
            TrigResolved::Software { over: Over::Blocks { blind } }
        }
    };
    let mut drives = Vec::with_capacity(want.drives.len());
    for ask in &want.drives {
        let mut drive = DriveResolved { id: ask.id, on: ask.on, drive: ask.drive.clone(), clamped: false, refused: None };
        if let Some(cap) = caps.outputs.iter().find(|c| c.id == ask.id) {
            if !cap.shapes.contains(&ask.drive.kind()) {
                drive.refused = Some(format!("output {} does not offer {:?}", cap.name, ask.drive.kind()));
            } else {
                let mut clamp = |value: &mut f64, bounds: (f64, f64), field: &str| {
                    let old = *value;
                    *value = (*value).clamp(bounds.0, bounds.1);
                    if *value != old { drive.clamped = true; note(NoteKind::Drive(ask.id), format!("{field} {old} clamped to {}", *value)); }
                };
                match &mut drive.drive {
                    Drive::Dc(level) => clamp(level, cap.level, "level"),
                    Drive::Square { frequency, duty, lo, hi } | Drive::Pulse { frequency, duty, lo, hi } => {
                        clamp(frequency, cap.rate, "frequency"); clamp(duty, (0.0, 1.0), "duty");
                        clamp(lo, cap.level, "low level"); clamp(hi, cap.level, "high level");
                    }
                    Drive::Record { rate, values } => {
                        clamp(rate, cap.rate, "record rate");
                        let outside = values.iter().filter(|&&v| (v as f64) < cap.level.0 || (v as f64) > cap.level.1).count();
                        if outside > 0 {
                            for value in Arc::make_mut(values) { *value = (*value as f64).clamp(cap.level.0, cap.level.1) as f32; }
                            drive.clamped = true;
                            note(NoteKind::Drive(ask.id), format!("{outside} record levels clamped to {} to {}", cap.level.0, cap.level.1));
                        }
                    }
                }
            }
        } else { drive.refused = Some("output is unavailable".into()); }
        if let Some(reason) = &drive.refused { drive.on = false; note(NoteKind::Drive(ask.id), reason.clone()); }
        drives.push(drive);
    }
    let mut result = Resolved { rate, depth, mode, channels, trigger, drives, notes, restart: true };
    result.restart = in_force.is_none_or(|previous| result.needs_restart(previous));
    result
}

impl Resolved {
    /// Compare acquisition topology only; retuning a trigger or a view within its range is live.
    pub fn needs_restart(&self, previous: &Self) -> bool {
        self.rate != previous.rate || self.depth != previous.depth || self.mode != previous.mode
            || !same_trigger_path(&self.trigger, &previous.trigger)
            || self.channels.len() != previous.channels.len()
            || self.channels.iter().any(|c| previous.channels.iter().find(|p| p.id == c.id).is_none_or(|p| p.on != c.on || p.range != c.range))
    }
}
fn same_trigger_path(a: &TrigResolved, b: &TrigResolved) -> bool {
    matches!((a, b), (TrigResolved::Hardware, TrigResolved::Hardware)
        | (TrigResolved::Software { over: Over::Stream }, TrigResolved::Software { over: Over::Stream })
        | (TrigResolved::Software { over: Over::Blocks { .. } }, TrigResolved::Software { over: Over::Blocks { .. } })
        | (TrigResolved::Unavailable(_), TrigResolved::Unavailable(_)))
}
/// A sample rate as a person reads it in a note: `25 MS/s`, `51.68 MS/s`, `400 S/s`.
fn rate_text(rate: f64) -> String {
    let (scale, prefix) = if rate >= 1e9 { (1e9, "G") } else if rate >= 1e6 { (1e6, "M") } else if rate >= 1e3 { (1e3, "k") } else { (1.0, "") };
    let text = format!("{:.2}", rate / scale);
    format!("{} {prefix}S/s", text.trim_end_matches('0').trim_end_matches('.'))
}
fn snap_rate(caps: &RateCaps, want: f64, down: bool) -> f64 {
    match caps {
        RateCaps::Any => want,
        RateCaps::Range { lo, hi } => want.clamp(*lo, *hi),
        RateCaps::List(list) => if down {
            // Never invent a rate the source cannot produce, even when no rate spans the view.
            list.iter().copied().filter(|&r| r <= want).max_by(f64::total_cmp)
                .or_else(|| list.iter().copied().min_by(f64::total_cmp)).unwrap_or(want)
        } else {
            list.iter().copied().filter(|&r| r >= want).min_by(f64::total_cmp)
                .or_else(|| list.iter().copied().max_by(f64::total_cmp)).unwrap_or(want)
        },
    }
}
