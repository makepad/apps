//! Mini traces use a fourth row texture, uniform `live_mini`, a [`LiveRows`] with
//! `VecRGBAf32` and 256 texels per texture row. Strips are addressed by subscribed slots,
//! never net indices. A slot belongs to a [`NetRef`] while subscribed, never relocates,
//! and is written empty when freed. Slot s owns [`MINI_SLOT`] texels starting at s * 64,
//! so four slots fit in a texture row.
//!
//! Texel zero is the header (scale_lo, scale_hi, head, frac). The scale bounds are the volts
//! spanned by the strip's height; scale_lo > scale_hi means no data. Head is the open column's
//! ring position, an exact small integer as f32 in 0..MINI_RING. Frac, in 0..1, is how far the
//! shown time has advanced into that column.
//!
//! Texels 1..=MINI_RING are a ring of columns. Absolute column k = floor(t / dt) lives at
//! position k mod 63, texel 1 + position. A column is (lo, hi, mean, x): lo and hi bound the
//! piecewise-linear signal over its interval, including interpolated boundary values so
//! neighbouring columns meet without joins. Mean is the time-weighted trapezoid mean of the
//! covered part; x = volt_x(mean, look) tints the strip like its wire. Empty columns are
//! [`MINI_EMPTY`], (1, -1, 0, -1). Non-finite samples break only their signal's polyline;
//! neither adjoining segment contributes. The CPU retains the mean and computes x on write.
//!
//! A strip shows [`MINI_COLS`] column widths, a window of 48 * dt simulated seconds ending at
//! the shown time. The longer ring keeps the oldest partly visible column separate from the
//! open column. Columns align to absolute multiples of dt, so closed columns never change
//! and do not shimmer. [`MINI_WALL`] makes the rolling window span two wall seconds of
//! simulated time. Columns before the record's first kept point are empty, never clamped
//! to the first sample.
//!
//! Full fills use [`MiniRing::fill`] to share the published trace store with [`MiniFill::run`]
//! on a worker. The UI adopts the columns while retaining its scale hysteresis.
//!
//! A column the trace left empty (a stretch its net was not written, a strip added mid-run in
//! Realtime) takes its net's coarse envelope there ([`MiniRing::fill_gaps`]): the extremes and
//! time-weighted mean of the envelope's slots over it. The first point of the column's own
//! replaces them, so a strip shows its net at full rate from then on and coarse before.

use super::segs::TreeOwner;
use super::sim::EnvSlot;
use super::{volt_x, LiveLook, LiveRows};
use crate::trace::store::time_index;
use fab_elec_core::ids::NetRef;
use fab_elec_sim::trace::TraceStore;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Texels owned by one subscribed strip, including its header.
pub const MINI_SLOT: usize = 64;
/// Columns retained in a strip's ring.
pub const MINI_RING: usize = 63;
/// Column widths in the visible time window.
pub const MINI_COLS: usize = 48;
/// Wall seconds of simulated time shown in a rolling strip.
pub const MINI_WALL: f64 = 2.0;
/// An empty header or column (lo > hi).
pub const MINI_EMPTY: [f32; 4] = [1.0, -1.0, 0.0, -1.0];

/// Rounds a requested window up to a 1-2-5 step when `previous` is None. Otherwise retains
/// the last returned step until `wanted` exceeds it by 20% or falls 20% below the next lower
/// step. Keep `previous` before any record-length cap. Invalid or too-small windows return None.
pub fn mini_window_step(wanted: f64, previous: Option<f64>) -> Option<f64> {
    if !wanted.is_finite() || wanted / MINI_COLS as f64 <= 0.0 {
        return None;
    }
    if let Some(step) = previous.filter(|step| step.is_finite() && *step > 0.0) {
        let decade = 10.0_f64.powf(step.log10().floor());
        // Rounding the mantissa also handles a decade boundary represented just below ten.
        let lower = decade * match (step / decade).round() as u32 {
            1 => 0.5,
            2 => 1.0,
            5 => 2.0,
            _ => 5.0,
        };
        if wanted <= 1.2 * step && wanted >= 0.8 * lower {
            return Some(step);
        }
    }
    let decade = 10.0_f64.powf(wanted.log10().floor());
    [1.0, 2.0, 5.0, 10.0].into_iter().map(|step| step * decade).find(|&step| step.is_finite() && step >= wanted)
}

/// The nets views show strips for, by geometry owner (the same owners as the segment rows), each with a stable slot.
#[derive(Default)]
pub struct MiniSubs {
    owners: BTreeMap<TreeOwner, BTreeSet<NetRef>>,
    nets: BTreeMap<NetRef, (u32, usize)>,
    free: BTreeSet<u32>,
    freed: Vec<u32>,
    count: usize,
    generation: u64,
}

impl MiniSubs {
    /// Replaces the owner's nets and returns slots in input order, including duplicates.
    /// Owners share a net's slot; new nets take the lowest free slot. The last owner to
    /// release a net puts its slot on the freed list, even if it is reused before the drain.
    pub fn set(&mut self, owner: TreeOwner, nets: &[NetRef]) -> Vec<u32> {
        let next: BTreeSet<_> = nets.iter().copied().collect();
        let mut previous = self.owners.remove(&owner).unwrap_or_default();
        for &net in previous.difference(&next) {
            self.release(net);
        }
        // Allocate in input order, with one reference per owner, not per occurrence.
        for &net in nets {
            if !previous.insert(net) {
                continue;
            }
            if let Some((_, refs)) = self.nets.get_mut(&net) {
                *refs += 1;
            } else {
                let slot = self.free.pop_first().unwrap_or_else(|| {
                    let slot = self.count as u32;
                    self.count += 1;
                    slot
                });
                self.nets.insert(net, (slot, 1));
                self.generation = self.generation.wrapping_add(1);
            }
        }
        if !next.is_empty() {
            self.owners.insert(owner, next);
        }
        nets.iter().map(|net| self.nets[net].0).collect()
    }

    /// Drops the owner, releasing its nets' slots when no other owner holds them.
    pub fn remove(&mut self, owner: TreeOwner) {
        if let Some(nets) = self.owners.remove(&owner) {
            for net in nets {
                self.release(net);
            }
        }
    }

    /// The slot of a subscribed net.
    pub fn slot(&self, net: NetRef) -> Option<u32> {
        self.nets.get(&net).map(|&(slot, _)| slot)
    }

    /// Every subscribed (slot, net), ordered by slot.
    pub fn slots(&self) -> Vec<(u32, NetRef)> {
        let mut slots: Vec<_> = self.nets.iter().map(|(&net, &(slot, _))| (slot, net)).collect();
        slots.sort_unstable_by_key(|&(slot, _)| slot);
        slots
    }

    /// One past the highest slot ever handed out; the texture holds this many slots.
    pub fn len(&self) -> usize {
        self.count
    }

    /// Slots released since the last call, including slots already taken again.
    pub fn take_freed(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.freed)
    }

    /// Moves whenever a slot is taken or released.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn release(&mut self, net: NetRef) {
        let Some((slot, refs)) = self.nets.get_mut(&net) else { return };
        *refs -= 1;
        if *refs == 0 {
            let slot = *slot;
            self.nets.remove(&net);
            self.free.insert(slot);
            self.freed.push(slot);
            self.generation = self.generation.wrapping_add(1);
        }
    }
}

#[derive(Clone, Copy)]
struct Column {
    lo: f32,
    hi: f32,
    sum: f64,
    dur: f64,
    /// Filled from the net's coarse envelope (`MiniRing::fill_gaps`): its own points replace it.
    coarse: bool,
}

impl Column {
    const EMPTY: Self = Self { lo: 1.0, hi: -1.0, sum: 0.0, dur: 0.0, coarse: false };

    fn add(&mut self, a: f32, b: f32, dur: f64) {
        if self.coarse {
            *self = Self::EMPTY;
        }
        if self.lo > self.hi {
            self.lo = a.min(b);
            self.hi = a.max(b);
        } else {
            self.lo = self.lo.min(a).min(b);
            self.hi = self.hi.max(a).max(b);
        }
        self.sum += (f64::from(a) + f64::from(b)) * 0.5 * dur;
        self.dur += dur;
    }

    fn texel(self, look: &LiveLook) -> [f32; 4] {
        if self.lo > self.hi {
            return MINI_EMPTY;
        }
        let mean = if self.dur > 0.0 { (self.sum / self.dur) as f32 } else { self.lo };
        [self.lo, self.hi, mean, volt_x(mean, look)]
    }
}

/// The columns of the subscribed slots on the CPU: extended from new trace points, or rebuilt from a store.
/// Owned plain data, so a worker can send a filled ring back to the UI.
pub struct MiniRing {
    window: f64,
    slots: Vec<(u32, Option<u32>)>,
    columns: Vec<Column>,
    previous: Vec<f32>,
    scales: Vec<[f32; 2]>,
    releasing: Vec<bool>,
    head: Option<i64>,
    last_t: Option<f64>,
    last_now: Option<f64>,
}

/// A sendable fill job sharing its trace store, owning its window, slots and requested end time.
/// Create it on the UI thread with [`MiniRing::fill`] and run it on a worker.
pub struct MiniFill {
    snapshot: Arc<TraceStore>,
    window: f64,
    slots: Vec<(u32, Option<u32>)>,
    t_end: f64,
}

impl MiniFill {
    /// One past the snapshot's last absolute sample index, including its store base.
    pub fn end(&self) -> usize {
        self.snapshot.base + self.snapshot.len()
    }

    /// Replays the snapshot into a fresh ring, ready for the UI to adopt.
    pub fn run(self) -> MiniRing {
        let mut ring = MiniRing::new();
        ring.set_window(self.window);
        ring.set_slots(&self.slots);
        ring.rebuild(&self.snapshot, self.t_end);
        ring
    }
}

impl MiniRing {
    /// An empty ring with a two-second window and no subscribed slots.
    pub fn new() -> MiniRing {
        Self {
            window: MINI_WALL,
            slots: Vec::new(),
            columns: Vec::new(),
            previous: Vec::new(),
            scales: Vec::new(),
            releasing: Vec::new(),
            head: None,
            last_t: None,
            last_now: None,
        }
    }

    /// A worker fill of `store` (an immutable published store, shared): the caller's thread copies
    /// only the slot list; the worker reads the subscribed signals from the shared store.
    pub fn fill(window: f64, slots: &[(u32, Option<u32>)], store: &Arc<TraceStore>, t_end: f64) -> MiniFill {
        MiniFill { snapshot: store.clone(), window, slots: slots.to_vec(), t_end }
    }

    /// Takes a filled ring's columns and previous points only when its window and slots match.
    /// Retains this ring's scales, release flags and wall time so UI hysteresis continues.
    /// Returns false without changing this ring when the fill no longer matches.
    pub fn adopt(&mut self, filled: MiniRing) -> bool {
        if self.window != filled.window || self.slots != filled.slots {
            return false;
        }
        self.columns = filled.columns;
        self.previous = filled.previous;
        self.head = filled.head;
        self.last_t = filled.last_t;
        true
    }

    /// Sets a finite positive window in simulated seconds (dt = window / MINI_COLS).
    /// Returns true when it changed; the caller then rebuilds the columns.
    pub fn set_window(&mut self, window: f64) -> bool {
        if !window.is_finite() || window / MINI_COLS as f64 <= 0.0 || window == self.window {
            return false;
        }
        self.window = window;
        true
    }

    /// The window in simulated seconds.
    pub fn window(&self) -> f64 {
        self.window
    }

    /// Sets (slot, signal index) pairs; None leaves a slot empty. Returns true when changed,
    /// in which case the caller rebuilds. Storage is flat and allocated only here.
    pub fn set_slots(&mut self, slots: &[(u32, Option<u32>)]) -> bool {
        if self.slots == slots {
            return false;
        }
        self.slots.clear();
        self.slots.extend_from_slice(slots);
        self.columns.resize(slots.len() * MINI_RING, Column::EMPTY);
        self.previous.resize(slots.len(), f32::NAN);
        self.scales.resize(slots.len(), [1.0, -1.0]);
        self.scales.fill([1.0, -1.0]);
        self.releasing.resize(slots.len(), false);
        self.releasing.fill(false);
        self.clear();
        self.last_now = None;
        true
    }

    /// Forgets every column and the previous point, retaining the scale's hysteresis.
    pub fn clear(&mut self) {
        self.columns.fill(Column::EMPTY);
        self.previous.fill(f32::NAN);
        self.head = None;
        self.last_t = None;
    }

    /// Accepts one later point without allocating. Splits its segment at absolute column
    /// boundaries, interpolates both ends, and integrates trapezoids. The first point only
    /// starts the polyline; an equal or earlier time is ignored. Missing or non-finite values
    /// contribute nothing and break only their signal's line; its next finite value starts anew.
    pub fn push(&mut self, t: f64, v: impl Fn(u32) -> f32) {
        let dt = self.window / MINI_COLS as f64;
        let Some(head) = column_at(t, dt) else { return };
        if self.last_t.is_some_and(|previous| t <= previous) {
            return;
        }
        let first = self.head.unwrap_or(head).max(head.saturating_sub(MINI_RING as i64 - 1));
        for (i, &(_, signal)) in self.slots.iter().enumerate() {
            let columns = &mut self.columns[i * MINI_RING..(i + 1) * MINI_RING];
            if let Some(old) = self.head {
                for k in old.saturating_add(1).max(head.saturating_sub(MINI_RING as i64 - 1))..=head {
                    columns[position(k)] = Column::EMPTY;
                }
            }
            let next = signal.map_or(f32::NAN, &v);
            let previous = self.previous[i];
            if next.is_finite() {
                if let Some(t0) = self.last_t.filter(|_| previous.is_finite()) {
                    for k in first..=head {
                        let a = t0.max(k as f64 * dt);
                        let b = t.min((k as f64 + 1.0) * dt);
                        if a <= b {
                            columns[position(k)].add(interpolate(t0, previous, t, next, a), interpolate(t0, previous, t, next, b), b - a);
                        }
                    }
                } else {
                    columns[position(head)].add(next, next, 0.0);
                }
            }
            self.previous[i] = next;
        }
        self.head = Some(head);
        self.last_t = Some(t);
    }

    /// Clears and replays the store through the shown time, starting with the last sample
    /// before the window's first column. Adds an interpolated endpoint between samples.
    /// A time before the first kept sample (or no index at all) leaves every column empty.
    pub fn rebuild(&mut self, store: &TraceStore, t_end: f64) {
        self.clear();
        let dt = self.window / MINI_COLS as f64;
        let Some(head) = column_at(t_end, dt) else { return };
        let Some(end) = store.index_at(t_end).map(|i| i - store.base) else { return };
        let first_time = head.saturating_sub(MINI_COLS as i64) as f64 * dt;
        let first = time_index(&store.time, first_time).saturating_sub(1).min(end);
        for i in first..=end {
            self.push(store.time.at(i), |signal| store.signals.get(signal as usize).map_or(f32::NAN, |s| s.at(i)));
        }
        let left = store.time.at(end);
        if left < t_end && end + 1 < store.len() {
            let right = store.time.at(end + 1);
            self.push(t_end, |signal| {
                store.signals.get(signal as usize).map_or(f32::NAN, |s| interpolate(left, s.at(end), right, s.at(end + 1), t_end))
            });
        }
    }

    /// The newest pushed time, if any.
    pub fn last_t(&self) -> Option<f64> {
        self.last_t
    }

    /// Whether a column of a strip with a signal is empty or coarse in the ring after `from`
    /// (the start of what a coarse envelope covers), up to the newest point: what
    /// [`Self::fill_gaps`] could fill. O(slots × ring).
    pub fn gaps(&self, from: f64) -> bool {
        let (Some(head), Some(last)) = (self.head, self.last_t) else { return false };
        let dt = self.window / MINI_COLS as f64;
        let ks = (head.saturating_sub(MINI_RING as i64 - 1)..=head).filter(|&k| (k as f64 + 1.0) * dt > from && k as f64 * dt < last);
        self.slots.iter().enumerate().filter(|(_, (_, signal))| signal.is_some()).any(|(i, _)| {
            ks.clone().any(|k| {
                let c = self.columns[i * MINI_RING + position(k)];
                c.lo > c.hi || c.coarse
            })
        })
    }

    /// Fills the ring's empty and coarse columns from `envelopes`, by slot in `set_slots` order
    /// (each in time order, a net's coarse statistics: the take's): over each column's span up to
    /// the newest point, the extremes of the slots in it and their time-weighted mean. A column
    /// with a point of its own keeps it; one no slot reaches stays as it was. Returns whether a
    /// column changed. O(slots × (ring + envelope slots)).
    pub fn fill_gaps(&mut self, envelopes: &[Vec<EnvSlot>]) -> bool {
        let (Some(head), Some(last)) = (self.head, self.last_t) else { return false };
        let dt = self.window / MINI_COLS as f64;
        let mut changed = false;
        for (i, slots) in envelopes.iter().enumerate().take(self.slots.len()) {
            let columns = &mut self.columns[i * MINI_RING..(i + 1) * MINI_RING];
            let mut j = 0;
            for k in head.saturating_sub(MINI_RING as i64 - 1)..=head {
                let c = &mut columns[position(k)];
                if c.lo <= c.hi && !c.coarse {
                    continue;
                }
                let (a, b) = (k as f64 * dt, ((k as f64 + 1.0) * dt).min(last));
                while slots.get(j).is_some_and(|slot| slot.t1 <= a) {
                    j += 1;
                }
                let mut fill = Column { coarse: true, ..Column::EMPTY };
                for slot in slots[j..].iter().take_while(|slot| slot.t0 < b) {
                    let (l, r) = (slot.t0.max(a), slot.t1.min(b));
                    if r > l && slot.min.is_finite() && slot.max.is_finite() && slot.mean.is_finite() {
                        fill.lo = if fill.dur > 0.0 { fill.lo.min(slot.min) } else { slot.min };
                        fill.hi = if fill.dur > 0.0 { fill.hi.max(slot.max) } else { slot.max };
                        fill.sum += f64::from(slot.mean) * (r - l);
                        fill.dur += r - l;
                    }
                }
                if fill.dur > 0.0 && (fill.lo, fill.hi, fill.sum.to_bits(), fill.dur.to_bits()) != (c.lo, c.hi, c.sum.to_bits(), c.dur.to_bits()) {
                    *c = fill;
                    changed = true;
                }
            }
        }
        changed
    }

    /// Writes headers and rings into caller-sized rows for the window ending at `t`.
    /// `now` is wall time; `hold` uses the window's own scale instead of live hysteresis.
    pub fn write(&mut self, rows: &mut LiveRows, t: f64, now: f64, look: &LiveLook, hold: bool) {
        let dt = self.window / MINI_COLS as f64;
        let shown = column_at(t, dt);
        let elapsed = self.last_now.map_or(0.0, |last| (now - last).max(0.0));
        let alpha = (1.0 - (-elapsed).exp()) as f32;
        self.last_now = Some(now);
        for (i, &(slot, _)) in self.slots.iter().enumerate() {
            let offset = slot as usize * MINI_SLOT;
            let mut target = [f32::INFINITY, f32::NEG_INFINITY];
            if let Some(head) = shown {
                // Forty-eight column widths can intersect forty-nine columns: both end
                // columns may be partial. The point at an exact right boundary is open.
                for k in head.saturating_sub(MINI_COLS as i64)..=head {
                    let c = self.column(i, k);
                    if c.lo <= c.hi {
                        target[0] = target[0].min(c.lo);
                        target[1] = target[1].max(c.hi);
                    }
                }
            }
            if target[0] > target[1] {
                self.scales[i] = [1.0, -1.0];
                self.releasing[i] = false;
                rows.set(offset, MINI_EMPTY);
            } else {
                let floor = 0.02 * (look.v_range.1 - look.v_range.0).max(0.0);
                let pad = (floor - (target[1] - target[0])).max(0.0) * 0.5;
                target[0] -= pad;
                target[1] += pad;
                let scale = &mut self.scales[i];
                if hold || scale[0] > scale[1] {
                    *scale = target;
                    self.releasing[i] = false;
                } else if target[0] < scale[0] || target[1] > scale[1] {
                    scale[0] = scale[0].min(target[0]);
                    scale[1] = scale[1].max(target[1]);
                    self.releasing[i] = false;
                } else {
                    // The dead band starts a release; let that release finish instead of
                    // stopping forever at a scale 1 / 0.7 times the steady target.
                    self.releasing[i] |= target[1] - target[0] < 0.7 * (scale[1] - scale[0]);
                    if self.releasing[i] {
                        scale[0] = (scale[0] + alpha * (target[0] - scale[0])).min(target[0]);
                        scale[1] = (scale[1] + alpha * (target[1] - scale[1])).max(target[1]);
                    }
                }
                let head = shown.expect("a nonempty target has a shown column");
                rows.set(offset, [scale[0], scale[1], position(head) as f32, (t / dt - head as f64).clamp(0.0, 1.0) as f32]);
            }
            for p in 0..MINI_RING {
                let c = shown.map_or(Column::EMPTY, |head| {
                    let k = head.saturating_sub((position(head) + MINI_RING - p) as i64 % MINI_RING as i64);
                    self.column(i, k)
                });
                rows.set(offset + 1 + p, c.texel(look));
            }
        }
    }

    fn column(&self, slot: usize, k: i64) -> Column {
        if self.head.is_some_and(|head| k <= head && k >= head.saturating_sub(MINI_RING as i64 - 1)) {
            self.columns[slot * MINI_RING + position(k)]
        } else {
            Column::EMPTY
        }
    }
}

fn column_at(t: f64, dt: f64) -> Option<i64> {
    let k = (t / dt).floor();
    (k.is_finite() && k >= i64::MIN as f64 && k < i64::MAX as f64).then_some(k as i64)
}

fn position(k: i64) -> usize {
    k.rem_euclid(MINI_RING as i64) as usize
}

fn interpolate(t0: f64, v0: f32, t1: f64, v1: f32, t: f64) -> f32 {
    if t <= t0 {
        v0
    } else if t >= t1 {
        v1
    } else {
        (f64::from(v0) + (f64::from(v1) - f64::from(v0)) * ((t - t0) / (t1 - t0))) as f32
    }
}

#[cfg(test)]
#[test]
fn mini_traces() {
    use fab_elec_core::ids::{BoardId, NetId};

    let look = LiveLook::dark();
    let mut ring = MiniRing::new();
    let slots = [(0, Some(0)), (1, Some(1))];
    assert!(ring.set_slots(&slots), "a square fills its columns: two subscribed signals");
    assert!(ring.set_window(48e-3), "a square fills its columns: one millisecond columns");
    let dt = ring.window() / MINI_COLS as f64;
    let sample = 50e-6;
    let mut rows = LiveRows::with_off(MINI_EMPTY);
    rows.resize(3 * MINI_SLOT);
    let mut store = TraceStore::new(0, &[false, false]);
    let mut closed = Vec::new();
    // Each half-period is forty samples; edges are halfway between samples inside a column.
    let square = |i: usize, high: f32| if (i + 29) / 40 % 2 == 0 { high } else { 0.0 };
    for i in 0..=2000 {
        let t = i as f64 * sample;
        let values = [square(i, 3.3), 1.2];
        ring.push(t, |signal| values[signal as usize]);
        store.push(t, &[f64::from(values[0]), f64::from(values[1])], &mut closed);
    }
    let newest = ring.last_t().expect("a square fills its columns: the newest point");
    ring.write(&mut rows, newest, 0.0, &look, false);
    let head = column_at(newest, dt).expect("a square fills its columns: a finite head");
    let (mut flat, mut edges) = (0, 0);
    for k in head - MINI_COLS as i64..head {
        let c = rows.row(1 + position(k));
        if c[0] == c[1] {
            flat += 1;
            assert!((c[0] == 0.0 || c[0] == 3.3) && c[2] == c[0],
                "a square fills its columns: flat levels and their means at {k}: {c:?}");
        } else {
            edges += 1;
            assert!(c[0] == 0.0 && c[1] == 3.3 && c[2] > 0.0 && c[2] < 3.3,
                "a square fills its columns: edge bounds and mean at {k}: {c:?}");
        }
        assert_eq!(rows.row(MINI_SLOT + 1 + position(k)), [1.2, 1.2, 1.2, volt_x(1.2, &look)],
            "a square fills its columns: the constant signal at {k}");
    }
    assert!(flat > 0 && edges > 0, "a square fills its columns: both flat and edge columns were checked");
    let header = rows.row(MINI_SLOT);
    assert!((header[0] - 1.15).abs() < 1e-6 && (header[1] - 1.25).abs() < 1e-6,
        "the header and freed slots: the constant scale has the two percent floor: {header:?}");
    assert!(header[2] == position(head) as f32 && (0.0..=1.0).contains(&header[3]),
        "the header and freed slots: the open column and its fraction: {header:?}");
    assert!((2 * MINI_SLOT..3 * MINI_SLOT).all(|i| rows.row(i) == MINI_EMPTY),
        "the header and freed slots: an unsubscribed slot stays empty");
    let scale = [rows.row(0)[0], rows.row(0)[1]];
    for i in 1..=5 {
        ring.write(&mut rows, newest, i as f64 * 0.1, &look, false);
        assert_eq!([rows.row(0)[0], rows.row(0)[1]], scale, "no pumping: a steady square keeps its scale");
    }

    let end = newest + 5.5e-3;
    let before = ring.previous[0];
    ring.push(end, |_| 0.0);
    store.push(end, &[0.0, 0.0], &mut closed);
    ring.write(&mut rows, end, 0.6, &look, false);
    let end_head = column_at(end, dt).expect("a long step interpolates, no gaps: a finite head");
    let mut boundary = before;
    for k in head..=end_head {
        let c = rows.row(1 + position(k));
        let a = interpolate(newest, before, end, 0.0, (k as f64 * dt).max(newest));
        let b = interpolate(newest, before, end, 0.0, ((k as f64 + 1.0) * dt).min(end));
        assert!(c[0] <= c[1] && c[0] == b && c[1] == a,
            "a long step interpolates, no gaps: boundary values at {k}: {c:?}, {a}..{b}");
        assert_eq!(c[1], boundary, "a long step interpolates, no gaps: neighbours share the boundary at {k}");
        boundary = c[0];
    }
    // An ignored time must not even ask for its signal values.
    ring.push(end, |_| panic!("a long step interpolates, no gaps: duplicate time was read"));
    ring.push(newest, |_| panic!("a long step interpolates, no gaps: earlier time was read"));

    let mut replay = MiniRing::new();
    replay.set_slots(&slots);
    replay.set_window(ring.window());
    let mut replay_rows = LiveRows::with_off(MINI_EMPTY);
    replay_rows.resize(3 * MINI_SLOT);
    replay.rebuild(&store, end);
    replay.write(&mut replay_rows, end, 0.6, &look, true);
    for slot in 0..2 {
        for k in end_head - MINI_COLS as i64..=end_head {
            let i = slot * MINI_SLOT + 1 + position(k);
            assert_eq!(rows.row(i), replay_rows.row(i), "rebuild equals push: slot {slot}, column {k}");
        }
    }
    let between = (store.time.at(1970) + store.time.at(1971)) * 0.5;
    let expected = interpolate(store.time.at(1970), store.signals[0].at(1970), store.time.at(1971), store.signals[0].at(1971), between);
    replay.rebuild(&store, between);
    replay.write(&mut replay_rows, between, 0.7, &look, true);
    let k = column_at(between, dt).expect("rebuild equals push: an interpolated head");
    let c = replay_rows.row(1 + position(k));
    let bound = if expected < store.signals[0].at(1970) { c[0] } else { c[1] };
    assert_eq!(bound, expected, "rebuild equals push: the last column ends at the interpolated value");
    assert_eq!(replay.last_t(), Some(between), "rebuild equals push: the endpoint is at the cursor");

    {
        let shared = Arc::new(store.clone());
        let fill = MiniRing::fill(replay.window(), &slots, &shared, between);
        let _: &dyn Send = &fill;
        assert_eq!(fill.end(), store.base + store.len(), "worker fill: the snapshot remembers its absolute end");
        let mut filled = fill.run();
        let _: &dyn Send = &filled;
        let mut filled_rows = LiveRows::with_off(MINI_EMPTY);
        filled_rows.resize(3 * MINI_SLOT);
        filled.write(&mut filled_rows, between, 0.7, &look, true);
        for slot in 0..2 {
            for column in 1..=MINI_RING {
                let i = slot * MINI_SLOT + column;
                assert_eq!(filled_rows.row(i), replay_rows.row(i),
                    "worker fill equals inline rebuild: slot {slot}, column {column}");
            }
        }
        replay.scales[0] = [-1.0, 4.0];
        replay.releasing[0] = true;
        let hysteresis = (replay.scales.clone(), replay.releasing.clone(), replay.last_now);
        assert!(replay.adopt(filled), "worker adoption: a matching window and slots are accepted");
        assert_eq!((&replay.scales, &replay.releasing, replay.last_now), (&hysteresis.0, &hysteresis.1, hysteresis.2),
            "worker adoption: the UI scales, release flags and wall time survive");
        let other = MiniRing::fill(replay.window() * 2.0, &slots, &shared, end).run();
        assert!(!replay.adopt(other), "worker adoption: another window is rejected");
        assert_eq!((&replay.scales, &replay.releasing, replay.last_now), (&hysteresis.0, &hysteresis.1, hysteresis.2),
            "worker adoption: rejecting another window preserves UI hysteresis");
        replay.write(&mut replay_rows, between, 0.7, &look, true);
        assert!((0..2 * MINI_SLOT).all(|i| replay_rows.row(i) == filled_rows.row(i)),
            "worker adoption: rejecting another window leaves the ring unchanged");
    }

    let early = 10.025e-3;
    replay.rebuild(&store, early);
    replay.write(&mut replay_rows, early, 0.8, &look, true);
    let early_head = column_at(early, dt).expect("before the record is empty: a finite head");
    for slot in 0..2 {
        for k in early_head - MINI_COLS as i64..0 {
            assert_eq!(replay_rows.row(slot * MINI_SLOT + 1 + position(k)), MINI_EMPTY,
                "before the record is empty: slot {slot}, column {k}");
        }
    }
    replay.rebuild(&store, -sample);
    replay.write(&mut replay_rows, -sample, 0.9, &look, true);
    assert!(replay.last_t().is_none() && (0..2 * MINI_SLOT).all(|i| replay_rows.row(i) == MINI_EMPTY),
        "before the record is empty: a cursor before the first point has no strip");

    // Let the lower-amplitude square replace the old window before observing its release.
    for i in 2111..=4000 {
        ring.push(i as f64 * sample, |signal| if signal == 0 { square(i, 1.0) } else { 1.2 });
    }
    let t = ring.last_t().expect("no pumping: the lower-amplitude points arrived");
    let mut high = scale[1];
    for i in 0..=30 {
        ring.write(&mut rows, t, 1.0 + i as f64 * 0.1, &look, false);
        let s = rows.row(0);
        assert!(s[0] <= 0.0 && s[1] >= 1.0 && s[1] <= high,
            "no pumping: release approaches the target from outside: {s:?}");
        high = s[1];
    }
    assert!(rows.row(0)[0] >= -0.1 && rows.row(0)[1] <= 1.1,
        "no pumping: three wall seconds of writes release to within ten percent: {:?}", rows.row(0));
    ring.write(&mut rows, t, 4.1, &look, true);
    assert_eq!([rows.row(0)[0], rows.row(0)[1]], [0.0, 1.0], "no pumping: hold takes the window's own scale");

    ring.clear();
    ring.set_window(MINI_COLS as f64);
    let mut non_finite = TraceStore::new(0, &[false, false]);
    for (t, value) in [
        (0.5, 1.2), (1.5, 1.2), (1.75, f32::NAN), (2.5, f32::INFINITY),
        (3.5, f32::NEG_INFINITY), (4.5, f32::NAN), (5.5, 1.2), (6.5, 1.2),
    ] {
        ring.push(t, |signal| if signal == 0 { 3.3 } else { value });
        non_finite.push(t, &[3.3, f64::from(value)], &mut closed);
    }
    ring.write(&mut rows, 6.5, 4.2, &look, false);
    replay.set_window(ring.window());
    replay.rebuild(&non_finite, 6.5);
    replay.write(&mut replay_rows, 6.5, 4.2, &look, true);
    for k in 0..=6 {
        let expected = if (2..=4).contains(&k) { MINI_EMPTY } else { [1.2, 1.2, 1.2, volt_x(1.2, &look)] };
        for texels in [&rows, &replay_rows] {
            assert_eq!(texels.row(MINI_SLOT + 1 + position(k)), expected,
                "non-finite samples are skipped: push and rebuild preserve finite levels and gaps at {k}");
            assert_eq!(texels.row(1 + position(k)), [3.3, 3.3, 3.3, volt_x(3.3, &look)],
                "non-finite samples are skipped: the other signal stays continuous at {k}");
        }
    }

    let changed_look = LiveLook { v_range: (0.0, 2.4), ..look };
    for (trace, texels, hold) in [(&mut ring, &mut rows, false), (&mut replay, &mut replay_rows, true)] {
        let before = texels.row(MINI_SLOT + 1 + position(5));
        trace.write(texels, 6.5, 4.3, &changed_look, hold);
        let after = texels.row(MINI_SLOT + 1 + position(5));
        assert_eq!(after[..3], before[..3], "a look change rewrites x: bounds and mean stay unchanged, hold={hold}");
        assert_ne!(after[3], before[3], "a look change rewrites x: the range changes the tint, hold={hold}");
        assert_eq!(after[3], volt_x(after[2], &changed_look), "a look change rewrites x: use the new range, hold={hold}");
    }

    for (boundary, lower) in [(1.0, 0.5), (2.0, 1.0), (5.0, 2.0)] {
        assert_eq!(mini_window_step(boundary * 1.2, Some(boundary)), Some(boundary),
            "the window step holds within 20 %: the upper edge is inclusive");
        assert!(mini_window_step(boundary * 1.21, Some(boundary)).is_some_and(|step| step > boundary),
            "the window step holds within 20 %: exceeding the upper band changes the step");
        assert_eq!(mini_window_step(lower * 0.8, Some(boundary)), Some(boundary),
            "the window step holds within 20 %: the lower edge is inclusive");
        assert_eq!(mini_window_step(lower * 0.79, Some(boundary)), Some(lower),
            "the window step holds within 20 %: falling below the lower band changes the step");
        for first in [0.9, 1.1] {
            let initial = mini_window_step(boundary * first, None).expect("a finite initial window step");
            let mut step = initial;
            for factor in [0.9, 1.1, 0.95, 1.05, 0.9, 1.1] {
                step = mini_window_step(boundary * factor, Some(step)).expect("a finite wobbling window step");
                assert_eq!(step, initial, "the window step holds within 20 %: ten percent wobble around {boundary}");
            }
            let mut changes = 0;
            let jumped = boundary * 1.1 * 3.0;
            for _ in 0..3 {
                let next = mini_window_step(jumped, Some(step)).expect("a finite jumped window step");
                changes += usize::from(next != step);
                step = next;
            }
            assert_eq!(changes, 1, "the window step holds within 20 %: a times-three jump changes it once");
        }
    }

    let mut subs = MiniSubs::default();
    let a = TreeOwner::Sheet(BoardId(0), 0);
    let b = TreeOwner::Copper(BoardId(0));
    let nets = [0, 1, 2].map(|net| NetRef { board: BoardId(0), net: NetId(net) });
    assert_eq!(subs.set(a, &[nets[0], nets[1], nets[0]]), [0, 1, 0], "subscriptions: duplicates share their slot");
    assert_eq!(subs.generation(), 2, "subscriptions: generation moves on each take");
    assert_eq!(subs.set(a, &[nets[1], nets[0]]), [1, 0], "subscriptions: slots survive reordering");
    assert_eq!(subs.set(b, &[nets[1]]), [1], "subscriptions: owners share a net's slot");
    assert_eq!(subs.generation(), 2, "subscriptions: unchanged slots keep the generation");
    subs.remove(a);
    assert!(subs.slot(nets[0]).is_none() && subs.slot(nets[1]) == Some(1), "subscriptions: removal keeps the shared net");
    assert_eq!(subs.generation(), 3, "subscriptions: release moves the generation");
    assert_eq!(subs.set(a, &[nets[2], nets[1]]), [0, 1], "subscriptions: the lowest free slot is reused");
    assert_eq!(subs.take_freed(), [0], "subscriptions: reuse does not hide a pending clear");
    assert!(subs.take_freed().is_empty(), "subscriptions: each release is reported once");
    assert_eq!(subs.slots(), [(0, nets[2]), (1, nets[1])], "subscriptions: enumeration is by slot");
    assert_eq!(subs.generation(), 4, "subscriptions: reuse moves the generation");
    subs.remove(a);
    subs.remove(b);
    subs.remove(b);
    assert_eq!(subs.take_freed(), [0, 1], "subscriptions: removing an absent owner is a no-op");
    assert_eq!(subs.generation(), 6, "subscriptions: both final releases move the generation");
    assert_eq!(subs.len(), 2, "subscriptions: the texture size never shrinks");
}
