//! One run's visual frames, by simulated time. Segments keep at most 256 slots, each a column
//! of means and extremes; a channel constant across a segment holds just one value. Past the
//! budget, older slots merge in pairs, and neighbouring segments join while their slots fit one
//! (each slot knows its first frame), so a long run keeps few segments. Resolution falls, never
//! the span kept for review.
//!
//! A segment's grid bounds stay fixed when an edit renders through it again. The slot's first
//! frame replaces its statistics; subsequent frames merge into them. Slots ahead stay as they
//! were until the render reaches them, as the recording's sound does.
//!
//! Costs: an ordinary push amortized O(channels), with O(slots) once per channel that turns from flat to
//! full. A rewind locates its segment in O(log segments); inserting in a gap also moves segment
//! headers. Means cost O(log segments + slots in the interval x full channels), plus the span
//! weights and one read per flat channel per segment. One net's envelope costs
//! O(log segments + slots returned). A fold costs O(slots x channels) for one segment,
//! about one fold per segment added once over budget. Choosing it scans segment headers;
//! thinning device states also moves the later states in their time-ordered vector.

use fab_elec_sim::cosim::CoState;
use fab_elec_sim::live::GridFrame;
pub use fab_elec_view::live::sim::EnvSlot;
use makepad_widgets_core::log;

const SLOTS: usize = 256;

/// The bytes a channel costs before its values: all a flat channel costs.
pub const CHANNEL_BYTES: usize = std::mem::size_of::<Channel>();

/// The bytes a slot costs apart from its values: its statistics' span and its first grid frame.
pub const SLOT_BYTES: usize = 24;

/// Nominal bytes charged for one device state. Its `Arc` payload is opaque, so its actual
/// allocation size cannot be measured here.
pub const CO_STATE_BYTES: usize = 4096;

/// Interval means by net, pin and part, as the rows take them.
#[derive(Default)]
pub struct Means {
    /// Mean voltage by net.
    pub net: Vec<f32>,
    /// Mean current by pin.
    pub pin: Vec<f32>,
    /// Mean dissipation by part.
    pub part: Vec<f32>,
}

/// The grid frames and device states of one numbering, kept by simulated time.
pub struct Take {
    generation: Option<u64>,
    grid: f64,
    // net mean, net minimum, net maximum, pin mean, part mean
    shape: [usize; 5],
    segments: Vec<Segment>,
    co: Vec<(f64, CoState)>,
    budget: usize,
    bytes: usize,
    revision: u64,
    coarsest: i64,
    warned: bool,
}

#[derive(Clone, Copy)]
struct Span {
    since: f64,
    t: f64,
}

impl Span {
    fn weight(self) -> f64 {
        (self.t - self.since).max(0.0)
    }
}

enum Channel {
    Flat(f32),
    Full(Vec<f32>),
}

impl Channel {
    fn at(&self, i: usize) -> f32 {
        match self {
            Self::Flat(v) => *v,
            Self::Full(v) => v[i],
        }
    }

    fn bytes(&self) -> usize {
        match self {
            Self::Flat(_) => CHANNEL_BYTES,
            Self::Full(v) => CHANNEL_BYTES + 4 * v.len(),
        }
    }

    /// Its values over `slots` slots.
    fn expand(self, slots: usize) -> Vec<f32> {
        match self {
            Self::Flat(v) => vec![v; slots],
            Self::Full(v) => v,
        }
    }

    // Return the additional storage, so an ordinary push never scans older segments.
    fn push(&mut self, value: f32, slots: usize) -> usize {
        match self {
            Self::Flat(v) if *v == value => 0,
            Self::Flat(v) => {
                let mut full = vec![*v; slots];
                full.push(value);
                *self = Self::Full(full);
                4 * (slots + 1)
            }
            Self::Full(v) => {
                v.push(value);
                4
            }
        }
    }

    fn set(&mut self, i: usize, value: f32, slots: usize) -> usize {
        match self {
            Self::Flat(v) if *v == value || slots == 1 => {
                *v = value;
                0
            }
            Self::Flat(v) => {
                let mut full = vec![*v; slots];
                full[i] = value;
                *self = Self::Full(full);
                4 * slots
            }
            Self::Full(v) => {
                v[i] = value;
                0
            }
        }
    }

    fn fold(&mut self, spans: &[Span], kind: Kind) {
        let Self::Full(values) = self else { return };
        let mut folded = Vec::with_capacity((values.len() + 1) / 2);
        for i in (0..values.len()).step_by(2) {
            folded.push(if i + 1 < values.len() {
                kind.merge(values[i], values[i + 1], spans[i].weight(), spans[i + 1].weight())
            } else {
                values[i]
            });
        }
        *self = if folded.iter().all(|v| *v == folded[0]) {
            Self::Flat(folded[0])
        } else {
            Self::Full(folded)
        };
    }

    fn weighted(&self, from: usize, weights: &[f64], weight: f64) -> f32 {
        match self {
            Self::Flat(v) => (*v as f64 * weight) as f32,
            Self::Full(v) => v[from..from + weights.len()].iter().zip(weights)
                .filter(|(_, w)| **w > 0.0)
                .map(|(v, w)| *v as f64 * *w).sum::<f64>() as f32,
        }
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Mean,
    Min,
    Max,
}

impl Kind {
    fn at(c: usize, shape: [usize; 5]) -> Kind {
        if c >= shape[0] && c < shape[0] + shape[1] {
            Self::Min
        } else if c >= shape[0] + shape[1] && c < shape[0] + shape[1] + shape[2] {
            Self::Max
        } else {
            Self::Mean
        }
    }

    fn merge(self, a: f32, b: f32, wa: f64, wb: f64) -> f32 {
        match self {
            Self::Min => a.min(b),
            Self::Max => a.max(b),
            Self::Mean if a == b => a,
            Self::Mean if wa <= 0.0 => b,
            Self::Mean if wb <= 0.0 => a,
            Self::Mean => ((a as f64 * wa + b as f64 * wb) / (wa + wb)) as f32,
        }
    }
}

struct Segment {
    // Inclusive grid bounds, independent of the statistics of a partly re-rendered slot.
    first: i64,
    last: i64,
    /// The most frames one of its slots holds: 1 while none was folded.
    widest: i64,
    spans: Vec<Span>,
    /// Each slot's first grid frame: a slot holds the frames up to the next one's first.
    starts: Vec<i64>,
    channels: Vec<Channel>,
    bytes: usize,
}

fn shape(frame: &GridFrame) -> [usize; 5] {
    [frame.net_mean.len(), frame.net_min.len(), frame.net_max.len(), frame.pin_mean.len(), frame.part_mean.len()]
}

fn values(frame: &GridFrame) -> impl Iterator<Item = f32> + '_ {
    frame.net_mean.iter().chain(&frame.net_min).chain(&frame.net_max)
        .chain(&frame.pin_mean).chain(&frame.part_mean).copied()
}

impl Segment {
    fn new(frame: &GridFrame) -> Segment {
        let channels: Vec<_> = values(frame).map(Channel::Flat).collect();
        Segment {
            first: frame.k,
            last: frame.k,
            widest: 1,
            spans: vec![Span { since: frame.since, t: frame.t }],
            starts: vec![frame.k],
            bytes: SLOT_BYTES + CHANNEL_BYTES * channels.len(),
            channels,
        }
    }

    fn end(&self) -> f64 {
        self.spans[self.spans.len() - 1].t
    }

    /// Whether `next` can join this one: from the frame after this one's last, at most a
    /// segment's slots together.
    fn joins(&self, next: &Segment) -> bool {
        self.last.checked_add(1) == Some(next.first) && self.spans.len() + next.spans.len() <= SLOTS
    }

    /// Takes `next`'s slots after its own (`joins`).
    fn join(&mut self, next: Segment) {
        let (n, m) = (self.spans.len(), next.spans.len());
        for (a, b) in self.channels.iter_mut().zip(next.channels) {
            let joined = match (std::mem::replace(a, Channel::Flat(0.0)), b) {
                (Channel::Flat(x), Channel::Flat(y)) if x == y => Channel::Flat(x),
                (x, y) => {
                    let mut v = x.expand(n);
                    v.extend(y.expand(m));
                    Channel::Full(v)
                }
            };
            *a = joined;
        }
        self.spans.extend(next.spans);
        self.starts.extend(next.starts);
        self.last = next.last;
        self.widest = self.widest.max(next.widest);
        self.bytes = SLOT_BYTES * self.spans.len() + self.channels.iter().map(Channel::bytes).sum::<usize>();
    }

    fn push(&mut self, frame: &GridFrame) {
        let n = self.spans.len();
        for (channel, value) in self.channels.iter_mut().zip(values(frame)) {
            self.bytes += channel.push(value, n);
        }
        self.spans.push(Span { since: frame.since, t: frame.t });
        self.starts.push(frame.k);
        self.bytes += SLOT_BYTES;
        self.last = frame.k;
    }

    fn put(&mut self, frame: &GridFrame, shape: [usize; 5]) {
        let i = self.starts.partition_point(|&k| k <= frame.k).saturating_sub(1);
        let replace = frame.k == self.starts[i];
        let old = self.spans[i];
        let new = Span { since: frame.since, t: frame.t };
        for (c, (channel, value)) in self.channels.iter_mut().zip(values(frame)).enumerate() {
            let value = if replace { value } else {
                Kind::at(c, shape).merge(channel.at(i), value, old.weight(), new.weight())
            };
            self.bytes += channel.set(i, value, self.spans.len());
        }
        self.spans[i] = Span { since: if replace { new.since } else { old.since }, t: new.t };
    }

    fn fold(&mut self, shape: [usize; 5]) {
        for (c, channel) in self.channels.iter_mut().enumerate() {
            channel.fold(&self.spans, Kind::at(c, shape));
        }
        self.spans = self.spans.chunks(2).map(|pair| Span {
            since: pair[0].since,
            t: pair[pair.len() - 1].t,
        }).collect();
        self.starts = self.starts.chunks(2).map(|pair| pair[0]).collect();
        let ends = self.starts.iter().skip(1).copied().chain(std::iter::once(self.last + 1));
        self.widest = self.starts.iter().zip(ends).map(|(a, b)| b - a).max().unwrap_or(1);
        self.bytes = SLOT_BYTES * self.spans.len() + self.channels.iter().map(Channel::bytes).sum::<usize>();
    }

    fn means(&self, shape: [usize; 5]) -> impl Iterator<Item = &Channel> {
        self.channels[..shape[0]].iter().chain(&self.channels[shape[0] + shape[1] + shape[2]..])
    }
}

impl Take {
    /// An empty take with a byte budget. The host uses `fab_elec_sim::budget::TAKE_BYTES`.
    pub fn new(budget: usize) -> Take {
        Take {
            generation: None,
            grid: 0.0,
            shape: [0; 5],
            segments: Vec::new(),
            co: Vec::new(),
            budget,
            bytes: 0,
            revision: 0,
            coarsest: 1,
            warned: false,
        }
    }

    /// Forget the run, including its device states, keeping the budget and invalidating readers.
    pub fn clear(&mut self) {
        self.generation = None;
        self.grid = 0.0;
        self.shape = [0; 5];
        self.segments.clear();
        self.co.clear();
        self.bytes = 0;
        self.revision = self.revision.wrapping_add(1);
        self.coarsest = 1;
        self.warned = false;
    }

    /// Whether no visual frames are kept.
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// The lowering's numbering, or None before the first frame.
    pub fn generation(&self) -> Option<u64> {
        self.generation
    }

    /// Keep a frame, replacing or rebuilding a kept slot when the run goes back. Another
    /// generation, grid or set of row counts starts the take over. Gaps get separate segments.
    pub fn push(&mut self, generation: u64, grid: f64, frame: &GridFrame) {
        let shape = shape(frame);
        if self.generation != Some(generation) || self.grid != grid || self.shape != shape {
            self.clear();
            self.generation = Some(generation);
            self.grid = grid;
            self.shape = shape;
        }
        let n = self.segments.len();
        let p = if self.segments.last().is_none_or(|s| frame.k > s.last) {
            n
        } else {
            self.segments.partition_point(|s| s.last < frame.k)
        };
        // A write before the tail can also fill a gap between kept slots.
        if p < n {
            self.revision = self.revision.wrapping_add(1);
        }
        if p < n && frame.k >= self.segments[p].first {
            let segment = &mut self.segments[p];
            let before = segment.bytes;
            segment.put(frame, shape);
            self.bytes += segment.bytes - before;
        } else if p > 0 && self.segments[p - 1].widest == 1
            && self.segments[p - 1].spans.len() < SLOTS
            && self.segments[p - 1].last.checked_add(1) == Some(frame.k)
        {
            let segment = &mut self.segments[p - 1];
            let before = segment.bytes;
            segment.push(frame);
            self.bytes += segment.bytes - before;
        } else {
            let segment = Segment::new(frame);
            self.bytes += segment.bytes;
            self.segments.insert(p, segment);
        }
        self.keep_budget();
    }

    /// Keep a changed device state, at most one per grid interval `[k * grid, (k + 1) * grid)`.
    /// A time before the newest state first removes states after it, even if this state is then
    /// deduplicated. With no positive grid yet, only equal times and equal `Arc`s deduplicate.
    pub fn push_co(&mut self, t: f64, co: &CoState) {
        if self.co.last().is_some_and(|(last, _)| t < *last) {
            let n = self.co.partition_point(|(time, _)| *time <= t);
            self.bytes -= (self.co.len() - n) * CO_STATE_BYTES;
            self.co.truncate(n);
        }
        if self.co.last().is_some_and(|(last, state)| state == co || *last == t
            || (self.grid > 0.0 && (*last / self.grid).floor() == (t / self.grid).floor()))
        {
            return;
        }
        self.co.push((t, co.clone()));
        self.bytes += CO_STATE_BYTES;
        self.keep_budget();
    }

    /// The simulated span kept, `[first since, last t]`.
    pub fn span(&self) -> Option<(f64, f64)> {
        Some((self.segments.first()?.spans[0].since, self.segments.last()?.end()))
    }

    /// Logical channel storage, 16 bytes per slot's span, and nominal device-state bytes.
    /// Vector capacities, allocation headers and segment metadata are not charged.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Changes on a write within the kept history, on a fold, and on clear. Tail appends alone
    /// leave it unchanged. Readers can use it to invalidate values from an earlier render.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The take's original grid interval in simulated seconds.
    pub(crate) fn grid(&self) -> f64 {
        self.grid
    }

    /// Start of the newest stretch of full-resolution segments; None if even the tail is folded.
    pub fn finest_from(&self) -> Option<f64> {
        self.segments.iter().rev().take_while(|s| s.widest == 1).last().map(|s| s.spans[0].since)
    }

    fn at(&self, t: f64) -> Option<(&Segment, usize)> {
        if self.is_empty() { return None; }
        let p = self.segments.partition_point(|s| s.end() < t).min(self.segments.len() - 1);
        let segment = &self.segments[p];
        let i = segment.spans.partition_point(|s| s.t < t).min(segment.spans.len() - 1);
        Some((segment, i))
    }

    /// Means over `(a, b]`, weighted by each slot's overlapping statistics span. With no overlap
    /// or `a >= b`, read the slot holding `b`: the first ending at or after it, clamped at the
    /// ends of the take. Returns that slot's `t`, or None when empty (leaving `out` alone).
    pub fn means(&self, a: f64, b: f64, out: &mut Means) -> Option<f64> {
        let (at, i) = self.at(b)?;
        out.net.clear();
        out.net.resize(self.shape[0], 0.0);
        out.pin.clear();
        out.pin.resize(self.shape[3], 0.0);
        out.part.clear();
        out.part.resize(self.shape[4], 0.0);
        let mut weight = 0.0;
        if a < b {
            let from = self.segments.partition_point(|s| s.end() <= a);
            for segment in self.segments[from..].iter().take_while(|s| s.spans[0].since < b) {
                let start = segment.spans.partition_point(|s| s.t <= a);
                let end = segment.spans.partition_point(|s| s.since < b).max(start);
                let mut weights = [0.0; SLOTS];
                let weights = &mut weights[..end - start];
                for (w, span) in weights.iter_mut().zip(&segment.spans[start..end]) {
                    *w = (span.t.min(b) - span.since.max(a)).max(0.0);
                }
                let w: f64 = weights.iter().sum();
                if !(w > 0.0) { continue; }
                weight += w;
                for (o, channel) in out.net.iter_mut().chain(&mut out.pin).chain(&mut out.part)
                    .zip(segment.means(self.shape))
                {
                    *o += channel.weighted(start, weights, w);
                }
            }
        }
        let output = out.net.iter_mut().chain(&mut out.pin).chain(&mut out.part);
        if weight > 0.0 {
            let k = (1.0 / weight) as f32;
            for o in output { *o *= k; }
        } else {
            for (o, channel) in output.zip(at.means(self.shape)) { *o = channel.at(i); }
        }
        Some(at.spans[i].t)
    }

    /// The newest device state at or before `t`, or the first when `t` precedes them all.
    pub fn co_at(&self, t: f64) -> Option<&CoState> {
        let i = self.co.partition_point(|(time, _)| *time <= t).saturating_sub(1);
        self.co.get(i).map(|(_, co)| co)
    }

    fn keep_budget(&mut self) {
        while self.bytes > self.budget {
            let half = (self.segments.len() / 2).max(1).min(self.segments.len());
            let candidate = |from: usize, to: usize| (from..to).filter(|&i| self.segments[i].spans.len() > 1)
                .min_by_key(|&i| (self.segments[i].widest, i));
            // If the older half is already one slot per segment, only the newer half can help.
            let p = candidate(0, half).or_else(|| candidate(half, self.segments.len()));
            let Some(p) = p else {
                if !self.warned {
                    log!("elec sim: the take keeps one slot per segment; its span stays whole over budget ({:.2} MiB kept)", self.bytes as f64 / (1 << 20) as f64);
                    self.warned = true;
                }
                break;
            };
            let segment = &mut self.segments[p];
            let before = segment.bytes;
            segment.fold(self.shape);
            self.bytes -= before - segment.bytes;
            self.revision = self.revision.wrapping_add(1);
            self.thin_co(p);
            let p = self.join_around(p);
            let segment = &self.segments[p];
            if segment.widest > self.coarsest {
                self.coarsest = segment.widest;
                log!("elec sim: the take's oldest stretch, before {:.2} s, keeps 1 frame in {} now ({:.2} MiB kept)",
                    segment.end(), self.coarsest, self.bytes as f64 / (1 << 20) as f64);
            }
        }
    }

    /// Joins segment `p` with its neighbours where they can (`Segment::joins`); returns where it
    /// is now.
    fn join_around(&mut self, mut p: usize) -> usize {
        if p + 1 < self.segments.len() && self.segments[p].joins(&self.segments[p + 1]) {
            let next = self.segments.remove(p + 1);
            let before = self.segments[p].bytes + next.bytes;
            self.segments[p].join(next);
            self.bytes = self.bytes + self.segments[p].bytes - before;
        }
        if p > 0 && self.segments[p - 1].joins(&self.segments[p]) {
            let this = self.segments.remove(p);
            p -= 1;
            let before = self.segments[p].bytes + this.bytes;
            self.segments[p].join(this);
            self.bytes = self.bytes + self.segments[p].bytes - before;
        }
        p
    }

    fn thin_co(&mut self, p: usize) {
        let segment = &self.segments[p];
        let from = self.co.partition_point(|(t, _)| *t < segment.spans[0].since);
        let to = self.co.partition_point(|(t, _)| *t <= segment.end());
        if to - from <= 1 { return; }
        let mut kept = Vec::with_capacity(segment.spans.len().min(to - from));
        let mut slot = 0;
        let mut previous = None;
        for (t, co) in &self.co[from..to] {
            while slot + 1 < segment.spans.len() && segment.spans[slot].t < *t { slot += 1; }
            if previous != Some(slot) {
                kept.push((*t, co.clone()));
                previous = Some(slot);
            }
        }
        self.bytes -= (to - from - kept.len()) * CO_STATE_BYTES;
        drop(self.co.splice(from..to, kept));
    }

    /// The nets' minima and maxima in the slot holding `t` (as `means` finds it): what a folded
    /// stretch keeps of the swings its means smooth over.
    pub fn net_extremes(&self, t: f64) -> Option<(Vec<f32>, Vec<f32>)> {
        let (segment, i) = self.at(t)?;
        let min = self.shape[0];
        let max = min + self.shape[1];
        Some((segment.channels[min..max].iter().map(|c| c.at(i)).collect(),
            segment.channels[max..max + self.shape[2]].iter().map(|c| c.at(i)).collect()))
    }

    /// Append one net's slots in time order, starting with the first ending after `from`.
    pub fn net_envelope(&self, net: usize, from: f64, out: &mut Vec<EnvSlot>) {
        let [means, mins, maxes, _, _] = self.shape;
        if net >= means || net >= mins || net >= maxes || from.is_nan() {
            return;
        }
        let first = self.segments.partition_point(|s| s.end() <= from);
        for segment in &self.segments[first..] {
            let start = segment.spans.partition_point(|s| s.t <= from);
            let mean = &segment.channels[net];
            let min = &segment.channels[means + net];
            let max = &segment.channels[means + mins + net];
            out.extend((start..segment.spans.len()).map(|i| {
                let span = segment.spans[i];
                EnvSlot { t0: span.since, t1: span.t, mean: mean.at(i), min: min.at(i), max: max.at(i) }
            }));
        }
    }
}
