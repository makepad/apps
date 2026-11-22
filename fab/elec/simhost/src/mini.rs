//! The host's fourth texture, filled from the published trace store the host takes and guarded by
//! the numbering's generation. Live columns keep advancing during scrubs; a separate ring holds the cursor.
//! Full fills run on pool jobs over the published store; the UI adopts them and catches up live
//! samples.

use crate::take::{EnvSlot, Take};
use crate::Spawn;
use fab_elec_bench::ValueIndex;
use fab_elec_sim::trace::TraceStore;
use fab_elec_view::live::mini::{mini_window_step, MiniRing, MiniSubs, MINI_EMPTY, MINI_SLOT, MINI_WALL};
use fab_elec_view::live::{LiveLook, LiveRows};
use makepad_widgets_core::{log, Cx, Texture};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Instant;

/// The ring (None is live, Some is the scrub cursor), configuration and snapshot bounds of a fill.
#[derive(Clone, Copy, PartialEq)]
struct FillKey {
    scrub: Option<f64>,
    window: f64,
    mapping: (u64, u64, usize),
    epoch: Epoch,
    base: usize,
    /// Live: the snapshot's absolute end, where catch-up starts. Scrub: the end of the samples
    /// the cursor's window reads.
    end: usize,
}

/// A store's generation and the rewinds it took (an edit sent its run back): points of another
/// epoch at the same index are other points.
type Epoch = (u64, u64);

fn epoch(store: &TraceStore) -> Epoch {
    (store.generation, store.rewinds)
}

/// One outstanding job, retaining its snapshot cost and invalidation across store resets.
struct PendingFill {
    key: FillKey,
    ui_seconds: f64,
    valid: bool,
}

/// The mini-trace texture: the subscribed nets' strip columns, from the host's own trace store.
pub struct MiniWriter {
    live: MiniRing,
    scrub: MiniRing,
    rows: LiveRows,
    // Epoch, absolute end, signal count of the store last followed.
    end: Option<(Epoch, usize, usize)>,
    // Subscription generation, numbering generation, number of available store signals.
    mapping: Option<(u64, u64, usize)>,
    slots: Vec<(u32, Option<u32>)>,
    done_tx: Sender<(FillKey, MiniRing, f64)>,
    done_rx: Receiver<(FillKey, MiniRing, f64)>,
    live_job: Option<PendingFill>,
    scrub_job: Option<PendingFill>,
    last_fill: Option<(f64, f64)>,
    // Cursor time, store epoch and the end of the samples the cursor's window reads.
    scrub_fill: Option<(f64, Epoch, usize)>,
    // The strips' coarse envelopes over the shown ring, by slot, and the take's revision and end
    // they were read at (`MiniRing::fill_gaps`): a write within the take changes the one, a frame
    // at its tail the other.
    envs: Vec<Vec<EnvSlot>>,
    gap_rev: Option<(u64, u64)>,
    shown_scrub: Option<f64>,
    // Last quantized step, before capping the displayed window to the record length.
    window_step: Option<f64>,
    // The look used for the last write, including the voltage range that determines x.
    look: Option<LiveLook>,
    rebuild: bool,
    dirty: bool,
}

impl MiniWriter {
    /// No subscriptions or samples yet; every texel is MINI_EMPTY.
    pub fn new() -> MiniWriter {
        let (done_tx, done_rx) = mpsc::channel();
        Self {
            live: MiniRing::new(),
            scrub: MiniRing::new(),
            rows: LiveRows::with_off(MINI_EMPTY),
            end: None,
            mapping: None,
            slots: Vec::new(),
            done_tx,
            done_rx,
            live_job: None,
            scrub_job: None,
            last_fill: None,
            scrub_fill: None,
            envs: Vec::new(),
            gap_rev: None,
            shown_scrub: None,
            window_step: None,
            look: None,
            rebuild: true,
            dirty: true,
        }
    }

    /// Follows a newly published store as the host takes it. Only a store of the remembered
    /// generation and rewinds and signal count that still holds the remembered end extends the live ring, by
    /// its points after that end; anything else requires replay. While a fill is in flight only
    /// the end advances; adoption catches up from the store. No allocation per point.
    pub fn follow(&mut self, store: &TraceStore) {
        let end = store.base + store.len();
        let from = match self.end {
            Some((at, e, n)) if at == epoch(store) && n == store.signals.len() && store.base <= e && e <= end => Some(e),
            _ => None,
        };
        if from.is_none() {
            self.rebuild = true;
            self.scrub_fill = None;
            for job in [&mut self.live_job, &mut self.scrub_job].into_iter().flatten() {
                job.valid = false;
            }
        }
        if let Some(e) = from.filter(|_| !self.rebuild && self.live_job.is_none()) {
            for i in e - store.base..store.len() {
                self.live.push(store.time.at(i), |signal| store.signals.get(signal as usize).map_or(f32::NAN, |s| s.at(i)));
            }
        }
        self.end = Some((epoch(store), end, store.signals.len()));
        self.dirty |= from != Some(end) || self.rebuild;
    }

    /// Follows subscriptions, numbering, the quantized playback window and the scrub cursor.
    /// Writes changed strips once a frame, returning true when rows were written or cleared.
    /// Keeps the last rows during a fill, and writes nothing while the store and numbering
    /// generations differ or no rate is known.
    /// A changed look rewrites every subscribed slot of whichever ring is shown.
    pub fn step(
        &mut self, spawn: &dyn Spawn, now: f64, minis: &mut MiniSubs, store: &Arc<TraceStore>, take: &Take, index: &ValueIndex,
        rate: f64, ratio: Option<f64>, record: f64, scrub: Option<f64>, look: &LiveLook,
    ) -> bool {
        let rate = if rate.is_finite() && rate > 0.0 { Some(rate) } else { ratio };
        let step = rate.filter(|rate| rate.is_finite() && *rate > 0.0 && !record.is_nan() && record > 0.0)
            .and_then(|rate| mini_window_step((MINI_WALL * rate).min(record), self.window_step));
        let window = step.map(|step| step.min(record));
        let mapping = (minis.generation(), index.generation, store.signals.len());
        let end = store.base + store.len();
        // The window ending at the cursor reads samples through the first one after it: once the
        // store holds that one, newer samples (the bench keeps running) change nothing there. A
        // cursor before the first kept sample reads none (0).
        let wanted_scrub = scrub.map(|t| (t, epoch(store), store.index_at(t).map_or(0, |i| (i + 2).min(end))));
        let mut changed = false;
        // Drain even when the configuration is unavailable, so a stale job cannot block its replacement.
        while let Ok((key, ring, worker_seconds)) = self.done_rx.try_recv() {
            let pending = if key.scrub.is_some() { self.scrub_job.take() } else { self.live_job.take() };
            let current = pending.as_ref().is_some_and(|job| job.valid && job.key == key)
                && store.generation == index.generation && key.epoch == epoch(store)
                && window == Some(key.window) && key.mapping == mapping && self.mapping == Some(mapping);
            let started = Instant::now();
            let adopted = if let Some(t) = key.scrub {
                if current && wanted_scrub == Some((t, key.epoch, key.end)) && self.scrub.adopt(ring) {
                    self.scrub_fill = wanted_scrub;
                    true
                } else {
                    false
                }
            } else if current && !self.rebuild && key.base <= store.base
                && key.end >= store.base && key.end <= end && self.live.adopt(ring)
            {
                for i in key.end - store.base..store.len() {
                    self.live.push(store.time.at(i), |signal| store.signals.get(signal as usize).map_or(f32::NAN, |s| s.at(i)));
                }
                self.end = Some((epoch(store), end, store.signals.len()));
                true
            } else {
                self.rebuild = true;
                false
            };
            if adopted {
                let ui_seconds = started.elapsed().as_secs_f64();
                self.last_fill = Some((worker_seconds, pending.map_or(0.0, |job| job.ui_seconds) + ui_seconds));
                self.dirty = true;
            }
        }
        if store.generation != index.generation {
            self.mapping = None;
            self.rebuild = true;
            self.scrub_fill = None;
            return changed;
        }
        let (Some(step), Some(window)) = (step, window) else { return changed };
        self.window_step = Some(step);
        if self.mapping != Some(mapping) {
            let nets = index.counts().0;
            self.slots = minis.slots().into_iter().map(|(slot, net)| {
                let signal = index.net(net).filter(|&k| k < nets && k < store.signals.len()).map(|k| k as u32);
                (slot, signal)
            }).collect();
            let count = minis.len() * MINI_SLOT;
            changed |= self.rows.nets() != count;
            self.rows.resize(count);
            self.live.set_slots(&self.slots);
            self.scrub.set_slots(&self.slots);
            self.mapping = Some(mapping);
            self.rebuild = true;
            self.scrub_fill = None;
        }
        let freed = minis.take_freed();
        for &slot in &freed {
            for texel in slot as usize * MINI_SLOT..(slot as usize + 1) * MINI_SLOT {
                self.rows.set(texel, MINI_EMPTY);
            }
        }
        self.dirty |= !freed.is_empty();
        changed |= !freed.is_empty();
        if self.live.set_window(window) {
            self.scrub.set_window(window);
            self.rebuild = true;
            self.scrub_fill = None;
            let n = self.slots.len();
            log!("elec sim: mini traces {n} slots, window {window} s");
        }
        if self.rebuild && self.live_job.is_none() {
            let started = Instant::now();
            let t = store.len().checked_sub(1).map_or(0.0, |last| store.time.at(last));
            let fill = MiniRing::fill(window, &self.slots, store, t);
            let key = FillKey { scrub: None, window, mapping, epoch: epoch(store), base: store.base, end: fill.end() };
            let ui_seconds = started.elapsed().as_secs_f64();
            self.live_job = Some(PendingFill { key, ui_seconds, valid: true });
            self.end = Some((epoch(store), end, store.signals.len()));
            self.rebuild = false;
            let tx = self.done_tx.clone();
            spawn.job(Box::new(move || {
                let started = Instant::now();
                let ring = fill.run();
                let _ = tx.send((key, ring, started.elapsed().as_secs_f64()));
            }));
        }
        if let Some((t, _, read_end)) = wanted_scrub {
            if self.scrub_fill != wanted_scrub && self.scrub_job.is_none() {
                let started = Instant::now();
                let fill = MiniRing::fill(window, &self.slots, store, t);
                let key = FillKey { scrub: Some(t), window, mapping, epoch: epoch(store), base: store.base, end: read_end };
                let ui_seconds = started.elapsed().as_secs_f64();
                self.scrub_job = Some(PendingFill { key, ui_seconds, valid: true });
                let tx = self.done_tx.clone();
                spawn.job(Box::new(move || {
                    let started = Instant::now();
                    let ring = fill.run();
                    let _ = tx.send((key, ring, started.elapsed().as_secs_f64()));
                }));
            }
        }
        // the columns the trace left empty (a net Realtime writes no point of, a strip added
        // mid-run) take their nets' coarse envelope from the take: no blank, the full rate from
        // the net's first point on. O(strips × ring) a frame; the envelopes are read again only
        // while a gap is shown and the take or the ring changed.
        if let Some((from, end)) = take.span().filter(|_| take.generation() == Some(index.generation)) {
            // a frame the take gains at its tail can fill a gap with the ring unmoved: Realtime's
            // take comes by the values' publication, which can reach the host after the trace's
            let revision = (take.revision(), end.to_bits());
            let ring = if scrub.is_some() {
                (self.scrub_job.is_none() && self.scrub_fill == wanted_scrub).then_some(&mut self.scrub)
            } else {
                (self.live_job.is_none() && !self.rebuild).then_some(&mut self.live)
            };
            if let Some(ring) = ring.filter(|ring| ring.gaps(from) && (self.dirty || self.gap_rev != Some(revision))) {
                // the ring's 63 columns span less than two windows
                let lo = ring.last_t().unwrap_or(0.0) - 2.0 * window;
                self.envs.resize_with(self.slots.len(), Vec::new);
                for (env, &(_, signal)) in self.envs.iter_mut().zip(&self.slots) {
                    env.clear();
                    if let Some(row) = signal {
                        take.net_envelope(row as usize, lo, env);
                    }
                }
                self.dirty |= ring.fill_gaps(&self.envs[..self.slots.len()]);
                self.gap_rev = Some(revision);
            }
        }
        self.dirty |= self.shown_scrub != scrub || self.look.as_ref() != Some(look);
        if self.dirty {
            let written = if let Some(t) = scrub {
                if self.scrub_job.is_none() && self.scrub_fill == wanted_scrub {
                    self.scrub.write(&mut self.rows, t, now, look, true);
                    true
                } else {
                    false
                }
            } else if self.live_job.is_none() && !self.rebuild {
                self.live.write(&mut self.rows, self.live.last_t().unwrap_or(0.0), now, look, false);
                true
            } else {
                false
            };
            if written {
                changed |= !self.slots.is_empty();
                self.look = Some(*look);
                self.dirty = false;
                self.shown_scrub = scrub;
            }
        }
        changed
    }

    /// The store's rows the subscribed strips read, under the numbering they were mapped at.
    pub fn signals(&self) -> impl Iterator<Item = usize> + '_ {
        self.slots.iter().filter_map(|&(_, signal)| signal.map(|row| row as usize))
    }

    /// Takes the latest adopted fill's worker seconds and UI snapshot/adoption/catch-up seconds.
    pub fn last_fill(&mut self) -> Option<(f64, f64)> {
        self.last_fill.take()
    }

    /// The CPU texels, including headers and empty freed slots.
    pub fn rows(&self) -> &LiveRows {
        &self.rows
    }

    /// Uploads changed texture rows once a frame and returns the texture bound as live_mini.
    pub fn upload(&mut self, cx: &mut Cx) -> Texture {
        self.rows.texture(cx).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_core::ids::{BoardId, NetId, NetRef};
    use fab_elec_sim::live::GridFrame;
    use fab_elec_view::live::mini::{MINI_COLS, MINI_RING};
    use fab_elec_view::live::segs::TreeOwner;

    /// Runs each job where it is posted.
    struct Inline;

    impl Spawn for Inline {
        fn worker(&self, _name: &'static str, f: Box<dyn FnOnce() + Send>) {
            f()
        }
        fn job(&self, f: Box<dyn FnOnce() + Send>) {
            f()
        }
    }

    /// A strip's net the trace left empty (Realtime writes no point of a net no view showed)
    /// takes the take's coarse envelope there, and a frame the take gains at its tail fills what it
    /// covers at the next step though the store, and so the strip's ring, has not moved: Realtime's
    /// trace and its take reach the host by two publications, the take's can come after the
    /// trace's, and a run at rest publishes neither again. A 20 ms window over a store to 20 ms,
    /// its net written from 10 ms on; the take to 8 ms, then to 12 ms.
    #[test]
    fn a_strips_gap_takes_the_frames_its_take_gains_at_its_tail() {
        const GRID: f64 = 4e-3;
        let net = NetRef { board: BoardId(0), net: NetId(0) };
        let mut index = ValueIndex::new(1);
        index.add_net(net);
        let mut store = TraceStore::new(1, &[false]);
        let mut closed = Vec::new();
        for i in 0..=200 {
            let t = f64::from(i) * 1e-4;
            store.push(t, &[if t < 10e-3 { f64::NAN } else { 1.0 }], &mut closed);
        }
        let store = Arc::new(store);
        let frame = |k: i64| GridFrame {
            k, since: k as f64 * GRID, t: (k + 1) as f64 * GRID,
            net_mean: vec![0.5], net_min: vec![0.0], net_max: vec![1.0], pin_mean: Vec::new(), part_mean: Vec::new(),
        };
        let mut take = Take::new(usize::MAX);
        take.push(1, GRID, &frame(0));
        take.push(1, GRID, &frame(1));
        let mut minis = MiniSubs::default();
        let slot = minis.set(TreeOwner::Sheet(BoardId(0), 0), &[net])[0] as usize * MINI_SLOT;
        let mut mini = MiniWriter::new();
        let look = LiveLook::dark();
        let blank = |mini: &MiniWriter| -> Vec<usize> {
            let head = mini.rows().row(slot)[2] as usize;
            (0..MINI_COLS).filter(|&d| {
                let c = mini.rows().row(slot + 1 + (head + MINI_RING - d) % MINI_RING);
                c[0] > c[1]
            }).collect()
        };
        // the ring's fill, then its adoption with the take to 8 ms: from there to the net's first
        // point the strip is blank
        for _ in 0..2 {
            mini.step(&Inline, 0.0, &mut minis, &store, &take, &index, 1.0, None, 20e-3, None, &look);
        }
        let short = blank(&mini);
        assert!(!short.is_empty() && short.len() < MINI_COLS / 4, "a take short of the net's first point leaves the columns between blank: {short:?}");
        // the take gains 8 to 12 ms at its tail; the store stays as it was
        take.push(1, GRID, &frame(2));
        mini.step(&Inline, 0.0, &mut minis, &store, &take, &index, 1.0, None, 20e-3, None, &look);
        let after = blank(&mini);
        assert!(after.is_empty(), "the take's new frames fill the strip's gap at the next step: {} of {MINI_COLS} columns still blank, {after:?} back from its head", after.len());
    }
}
