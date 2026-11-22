//! A run's audio as a recording: the render-then-play flow for a run below real time.
//!
//! The tap's recorder (on the worker) writes every voice's sample before its level (after its DC
//! block, at its full scale) into chunks of [`CHUNK`] frames, numbered from its take's start, and
//! sends them to the player through a bounded queue ([`Delivery`]). A take is made at its run's
//! recording rate ([`RunContent::recording_rate`]; the device's for a run that has none), each
//! sample the run's read at its instant (`listen::Band`), through a band-limited rate change where
//! the take's rate is not the run's (a run with no sample period: its mean over the period), and
//! whatever the device plays at: a device change makes no new take, and a kept take replays on
//! any device.
//! Window `w` of a take is its chunks `[w S, (w+1) S)`, `S` chunks a window at the take's rate
//! ([`window_frames`]). The player keeps two windows' slot tables ([`Shape`]): the window it loops
//! and the one rendering after it, and moves to the newer at the seam once it can play it to the
//! end. It plays a take at the device's rate through the read's rate change ([`rate_change`]:
//! past a window's end from the window after, or its start where it loops), and mixes the
//! voices at their levels, the master, the mute and the soft limiter as it plays, so a level
//! change is heard at once and never re-renders. The worker records at most one window past the
//! one heard, then holds the run (`Sink::holds`).
//!
//! Every chunk also goes to the take's [`Tape`], a file the UI reads, so a recording can be saved
//! whole ([`write_wav`]) however long it runs, without growing memory. The process's tape writer
//! writes it, never the worker: the worker queues each chunk and each cut and goes on.
//! A take that starts at its run's start carries what the run simulates ([`RunContent`]): a key
//! of everything that decides its samples, and the run time up to which nothing else changed it.
//! Such takes are kept ([`TapeShelf::start`]): a later take of the same content replays the
//! longest one's clean stretch at once, ahead of its render, which goes on behind it (to extend
//! it, and to check it: a rendered chunk that differs from the kept one is counted).
//!
//! Nothing is dropped on the callback: a chunk the player replaces, and a shape it lets go, go
//! back through a bounded queue ([`Spent`]) for the worker or the UI to drop ([`SpentBin`]). Threads: the worker sends with
//! `try_send`, keeping what does not fit in order; the callback takes with `try_recv` and returns
//! with `try_send` (no lock, wait or allocation); the UI makes shapes; the worker and the UI drop
//! what comes back.

use crate::listen::{rate_change, rate_kernel, soft_limit, TapMix, METER_EVERY, RATE_HALF};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};

/// Frames a chunk holds (each frame one sample of every voice).
pub const CHUNK: usize = 512;

/// The fastest device rate a shape's tables are made for: a faster one hears shorter windows.
pub const MAX_RATE: f64 = 192_000.0;

/// A chunk: [`CHUNK`] frames of every voice, interleaved, before levels.
pub type Chunk = Arc<[f32]>;

/// A recording: the tap that records it and its take (a new take each time the run starts over).
/// Newer takes order after older ones.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Take {
    pub tap: u64,
    pub take: u32,
}

/// A window of a take, in time order.
pub type WindowKey = (Take, u64);

/// A chunk on its way to the player: its take, its index from the take's start, the take's
/// sample rate, Hz, and the run's time at the start of its first sample, seconds.
pub struct Delivery {
    pub take: Take,
    pub index: u64,
    pub chunk: Chunk,
    pub rate: f64,
    pub t: f64,
}

/// A slot of a window's table: the window its chunk belongs to, and the chunk.
pub type Slot = Option<(WindowKey, Chunk)>;

/// What a tap's recordings are made of, from the UI when the tap goes in: the window's length,
/// the voices, their mix controls and meters, and two empty tables (made for [`MAX_RATE`]), so
/// the player takes them without allocating.
pub struct Shape {
    pub tap: u64,
    pub window_s: f64,
    pub voices: usize,
    pub mix: Arc<TapMix>,
    pub tables: [Box<[Slot]>; 2],
    /// Per slot of each table: when its chunk came (the player's count of chunks placed). A slot
    /// whose chunk came before the one before it holds an older render: the newer one's front.
    stamps: [Box<[u64]>; 2],
    /// Per slot: the chunk a newer render replaced, kept while the slot is the newer render's
    /// front, so the play head goes on from it into the older render after it.
    priors: [Box<[Option<Chunk>]>; 2],
    /// The player's meter state, per voice and the mix: the mean square and the peak.
    pub meters: Box<[(f64, f64)]>,
    /// A device sample's voices, as the player gathers them from the take's frames.
    pub acc: Box<[f64]>,
}

impl Shape {
    /// A shape for `voices` voices and windows of `window_s` seconds. O(slots).
    pub fn new(tap: u64, window_s: f64, voices: usize, mix: Arc<TapMix>) -> Shape {
        // the rate change's kernel ([`rate_change`]) is made here, on the UI: the callback only reads it
        rate_kernel();
        let slots = window_frames(window_s, MAX_RATE) / CHUNK;
        let table = || (0..slots).map(|_| None).collect::<Box<[Slot]>>();
        let stamps = || vec![0; slots].into_boxed_slice();
        let priors = || (0..slots).map(|_| None).collect::<Box<[Option<Chunk>]>>();
        let (meters, acc) = (vec![(0.0, 0.0); voices + 1].into_boxed_slice(), vec![0.0; voices].into_boxed_slice());
        Shape { tap, window_s, voices, mix, tables: [table(), table()], stamps: [stamps(), stamps()], priors: [priors(), priors()], meters, acc }
    }

    /// The chunks a window of a take at `rate` Hz holds: its length in whole chunks, at most what
    /// the tables hold (a take faster than [`MAX_RATE`] has shorter windows). 0 for no rate.
    pub fn slots(&self, rate: f64) -> usize {
        if !(rate > 0.0 && rate.is_finite()) {
            return 0;
        }
        window_frames(self.window_s, rate).min(self.tables[0].len() * CHUNK) / CHUNK
    }
}

/// What the player lets go, for the UI to drop.
pub enum Spent {
    Chunk(Chunk),
    Shape(Box<Shape>),
}

/// The frames a window holds at `rate` Hz: `window_s` in whole chunks, at least one.
pub fn window_frames(window_s: f64, rate: f64) -> usize {
    ((window_s * rate / CHUNK as f64).round().max(1.0) as usize) * CHUNK
}


/// What the player let go, waiting to be dropped by whichever of the worker's recorder (a chunk
/// at a time as it records) and the UI (at a change and a status read) comes first: the queue
/// drains while the Scope is not drawn, and is never dropped on the callback.
#[derive(Clone)]
pub struct SpentBin(Arc<Mutex<Receiver<Spent>>>);

impl SpentBin {
    /// Drops what waits, unless the other side is at it. O(items); never waits.
    pub fn drain(&self) {
        if let Ok(spent) = self.0.try_lock() {
            while spent.try_recv().is_ok() {}
        }
    }
}

/// The UI's ends: the chunk queue's sender (a tap's recorder takes a clone), shapes out, and
/// what the player lets go (a tap's recorder takes a clone too).
pub struct UiLinks {
    pub chunks: SyncSender<Delivery>,
    pub shapes: SyncSender<Box<Shape>>,
    pub spent: SpentBin,
}

/// The player's ends.
pub struct PlayerLinks {
    pub(crate) chunks: Receiver<Delivery>,
    pub(crate) shapes: Receiver<Box<Shape>>,
    pub(crate) spent: SyncSender<Spent>,
}

/// Chunks the queue holds before the worker keeps them itself.
const CHUNK_QUEUE: usize = 1024;

/// What the player may hand back before the UI drains it: a chunk per chunk taken, so the player
/// stops taking when it is full.
const SPENT_QUEUE: usize = 4096;

/// The three queues between the worker, the player and the UI. Allocates their fixed storage.
pub fn links() -> (UiLinks, PlayerLinks) {
    let (chunks_tx, chunks_rx) = sync_channel(CHUNK_QUEUE);
    let (shapes_tx, shapes_rx) = sync_channel(4);
    let (spent_tx, spent_rx) = sync_channel(SPENT_QUEUE);
    let spent = SpentBin(Arc::new(Mutex::new(spent_rx)));
    (UiLinks { chunks: chunks_tx, shapes: shapes_tx, spent }, PlayerLinks { chunks: chunks_rx, shapes: shapes_rx, spent: spent_tx })
}

/// A table's window, how many of its slots, from the first, hold that window's chunks, its
/// take's rate (Hz) and the chunks its window holds at that rate.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Tab {
    pub key: Option<WindowKey>,
    pub ready: usize,
    pub rate: f64,
    pub slots: usize,
    /// The run's time at the take's frame 0, from the newest chunk placed (its `t` less its
    /// frames from the take's start).
    pub origin: f64,
}

/// The player's recording side: the shape it plays, its two tables' state, the one it loops and
/// where (in the take's frames), and what the full spent queue refused. Every operation is O(work
/// done), with no allocation, lock or wait.
///
/// A take's chunks come in order, and again from the first a return reaches when its run went
/// back (an edit behind the front; the recorder's rewind): a chunk at or before the last of its
/// take. The newer render replaces the older slot by slot. A slot the play head reads that changes
/// under it is a swap: the change it makes in the next sample (`swapped`) is the player's to decay.
/// Ahead of the newer render's front the older render stays and plays on; the slot at the front
/// keeps the chunk it replaced (its prior) until the next slot is replaced too, and the play head
/// that reaches the slot after it goes on into the older render from that prior (`cross`), the
/// difference there decayed by the player too. A window counts as ready only as far as the newest
/// render has replaced it, so the player moves on to it once that can play.
#[derive(Default)]
pub(crate) struct Windows {
    pub shape: Option<Box<Shape>>,
    pub tabs: [Tab; 2],
    pub heard: Option<usize>,
    pub pos: f64,
    /// The heard table's step through its frames a device sample (its take's rate over the
    /// device's), set by the player each callback.
    pub step: f64,
    /// The slot of the heard table at a render's front that the play head reads from its prior:
    /// the head went on into the older render after it.
    pub seam: Option<usize>,
    /// What swaps of chunks the play head reads changed in its next sample: the voices' sum at
    /// their levels, old less new.
    pub swapped: f64,
    /// Chunks placed so far (each slot's stamp), the take and index of the last, and the newest
    /// return: its take, the first index it reached and its stamp.
    placed: u64,
    last: Option<(Take, u64)>,
    rewound: Option<(Take, u64, u64)>,
    held: [Option<Spent>; 4],
}

/// Chunks the player takes from the queue a callback, so one callback's work stays bounded.
const TAKE_PER_FILL: usize = 64;

impl Windows {
    /// Hands back what the queue refused before, then takes the newest shape and the chunks that
    /// came, while the spent queue has room.
    pub fn take(&mut self, links: &PlayerLinks) {
        for held in &mut self.held {
            if let Some(item) = held.take() {
                if let Err(TrySendError::Full(item) | TrySendError::Disconnected(item)) = links.spent.try_send(item) {
                    *held = Some(item);
                }
            }
        }
        if self.holding() {
            return;
        }
        while let Ok(shape) = links.shapes.try_recv() {
            if let Some(old) = self.shape.replace(shape) {
                self.give(links, Spent::Shape(old));
            }
            (self.tabs, self.heard, self.pos, self.seam, self.swapped) = (Default::default(), None, 0.0, None, 0.0);
            if self.holding() {
                return;
            }
        }
        for _ in 0..TAKE_PER_FILL {
            let Ok(d) = links.chunks.try_recv() else { break };
            self.place(links, d);
            if self.holding() {
                return;
            }
        }
    }

    fn holding(&self) -> bool {
        self.held.iter().any(Option::is_some)
    }

    /// Hands `item` back. What the full queue refuses waits here, a few at most (a chunk placed
    /// gives back three at most, and none is placed while any waits): dropped with the player,
    /// never on its callback.
    fn give(&mut self, links: &PlayerLinks, item: Spent) {
        if let Err(TrySendError::Full(item) | TrySendError::Disconnected(item)) = links.spent.try_send(item) {
            if let Some(held) = self.held.iter_mut().find(|h| h.is_none()) {
                *held = Some(item);
            }
        }
    }

    /// Whether slot `j` of table `i` (holding its window's chunk, come at `stamp`) is of the
    /// newest render: not past the newest return of its take, or come since.
    fn fresh(&self, i: usize, j: usize, stamp: u64) -> bool {
        let Some((take, w)) = self.tabs[i].key else { return false };
        let n = w * self.tabs[i].slots as u64 + j as u64;
        !matches!(self.rewound, Some((t, from, since)) if t == take && n >= from && stamp < since)
    }

    /// Counts table `i`'s ready slots: its window's chunks of the newest render, from its start.
    fn count_ready(&mut self, i: usize) {
        let Some(shape) = &self.shape else { return };
        let (key, slots) = (self.tabs[i].key, self.tabs[i].slots);
        let mut ready = self.tabs[i].ready;
        while ready < slots && shape.tables[i][ready].as_ref().is_some_and(|(k, _)| Some(*k) == key) && self.fresh(i, ready, shape.stamps[i][ready]) {
            ready += 1;
        }
        self.tabs[i].ready = ready;
    }

    /// The slots of table `i`'s window the device sample at `pos` reads ([`rate_change`]: its
    /// kernel's reach each way).
    fn read_slots(&self, i: usize, pos: f64, step: f64) -> (usize, usize) {
        let last = (self.window(i).max(1) - 1) as f64;
        let reach = RATE_HALF as f64 * step.max(1.0);
        let lo = (pos - reach).ceil().clamp(0.0, last) as usize;
        let hi = (pos + reach).floor().clamp(0.0, last) as usize;
        (lo / CHUNK, hi / CHUNK)
    }

    /// Puts a chunk in the table of its window (windows at its take's rate): the heard one's, the
    /// other's, or the other retargeted to a newer window. A chunk of no window it keeps (another
    /// tap's, or older than the one heard) goes back. A chunk at or before the last of its take is
    /// a return: its take's slots from there on are of an older render until replaced.
    fn place(&mut self, links: &PlayerLinks, d: Delivery) {
        self.placed += 1;
        let stamp = self.placed;
        let back = self.last.is_some_and(|(t, n)| t == d.take && d.index <= n);
        if self.last.is_none_or(|(t, _)| t <= d.take) {
            self.last = Some((d.take, d.index));
        }
        if back {
            self.rewound = Some((d.take, d.index, stamp));
            for i in 0..2 {
                if self.tabs[i].key.is_some_and(|(t, _)| t == d.take) {
                    self.tabs[i].ready = 0;
                    self.count_ready(i);
                }
            }
        }
        let Some(shape) = &self.shape else {
            self.give(links, Spent::Chunk(d.chunk));
            return;
        };
        let slots = shape.slots(d.rate);
        if d.take.tap != shape.tap || slots == 0 || d.chunk.len() != CHUNK * shape.voices {
            self.give(links, Spent::Chunk(d.chunk));
            return;
        }
        let key = (d.take, d.index / slots as u64);
        let at = match self.tabs.iter().position(|t| t.key == Some(key)) {
            Some(i) => Some(i),
            None => {
                // the table that is not heard, retargeted when this window is newer than its own
                let free = (0..2).filter(|&i| Some(i) != self.heard).min_by_key(|&i| self.tabs[i].key);
                let newer_than_heard = self.heard.is_none_or(|h| self.tabs[h].key < Some(key));
                free.filter(|&i| newer_than_heard && self.tabs[i].key < Some(key))
            }
        };
        let Some(i) = at else {
            self.give(links, Spent::Chunk(d.chunk));
            return;
        };
        if self.tabs[i].key != Some(key) {
            self.tabs[i] = Tab { key: Some(key), ready: 0, rate: d.rate, slots, origin: f64::NAN };
        }
        if d.t.is_finite() {
            self.tabs[i].origin = d.t - (d.index * CHUNK as u64) as f64 / d.rate;
        }
        let j = (d.index % slots as u64) as usize;
        // a chunk the play head reads changes under it: its next sample, before and after
        let reads = self.heard == Some(i) && {
            let (lo, hi) = self.read_slots(i, self.pos, self.step);
            (lo..=hi).contains(&j)
        };
        let before = if reads { self.sum_at(i, self.pos, self.step) } else { None };
        let Some(shape) = &mut self.shape else {
            return self.give(links, Spent::Chunk(d.chunk));
        };
        let old = shape.tables[i][j].replace((key, d.chunk));
        shape.stamps[i][j] = stamp;
        let mut back = [None, None, None];
        match old {
            // a newer render of the slot: the older is kept while this slot is the newer's front
            // (the next slot holds the older render); one kept already is older still, and stays
            Some((k, chunk)) if k == key => {
                let front = shape.tables[i].get(j + 1).is_some_and(|s| s.as_ref().is_some_and(|(k, _)| *k == key));
                if front && shape.priors[i][j].is_none() {
                    shape.priors[i][j] = Some(chunk);
                } else {
                    back[0] = Some(chunk);
                }
            }
            Some((_, chunk)) => {
                back[0] = Some(chunk);
                back[1] = shape.priors[i][j].take();
            }
            None => back[1] = shape.priors[i][j].take(),
        }
        // the slot before is no longer a front: this one is as new
        if j > 0 {
            back[2] = shape.priors[i][j - 1].take();
            if back[2].is_some() && self.heard == Some(i) && self.seam == Some(j - 1) {
                self.seam = None;
            }
        }
        if let (Some(before), Some(after)) = (before, if reads { self.sum_at(i, self.pos, self.step) } else { None }) {
            self.swapped += before - after;
        }
        for chunk in back.into_iter().flatten() {
            self.give(links, Spent::Chunk(chunk));
        }
        self.count_ready(i);
    }

    /// The frames of table `i` ready from its start, in its take's frames.
    pub fn ready_frames(&self, i: usize) -> usize {
        self.tabs[i].ready * CHUNK
    }

    /// The frames of table `i`'s window, in its take's frames.
    pub fn window(&self, i: usize) -> usize {
        self.tabs[i].slots * CHUNK
    }

    /// Whether a table holds a window whole, ready from its start to its end. O(1).
    pub fn whole(&self) -> bool {
        (0..self.tabs.len()).any(|i| self.whole_at(i))
    }

    /// Whether this window is ready from start to end.
    pub fn whole_at(&self, i: usize) -> bool {
        self.tabs[i].key.is_some() && self.tabs[i].slots > 0 && self.ready_frames(i) >= self.window(i)
    }

    /// The run's time at `pos` of table `i`'s window (its take's frames from the window's start),
    /// or None for a table with no window: a frame is the run's read at its instant, so this is
    /// the instant of the device sample read at `pos` ([`rate_change`]). O(1).
    pub fn time_at(&self, i: usize, pos: f64) -> Option<f64> {
        let tab = &self.tabs[i];
        let (_, w) = tab.key?;
        let t = tab.origin + ((w * tab.slots as u64 * CHUNK as u64) as f64 + pos) / tab.rate;
        t.is_finite().then_some(t)
    }

    /// Finds the frame at this time, preferring the heard window and then the newest.
    pub fn scrub_position(&self, t: f64) -> Option<(usize, f64)> {
        (0..self.tabs.len()).filter_map(|i| {
            let start = self.time_at(i, 0.0)?;
            let end = self.time_at(i, self.window(i) as f64)?;
            (start <= t && t < end).then(|| (i, (t - start) * self.tabs[i].rate))
        }).max_by_key(|&(i, _)| (Some(i) == self.heard, self.tabs[i].key))
    }

    /// Before the play head's sample at `pos` of table `i`: when its reads reach the slot after
    /// a render's front (a slot of an older render after one whose prior is kept), it goes on into
    /// the older render, reading the front slot from its prior until its reads leave that slot.
    /// Returns the difference that makes at the last sample (`played`: the voices' sum heard at
    /// `pos - step`, the older render's there taken from it), for the player to decay. O(voices x
    /// step) at a seam, O(1) else.
    pub fn cross(&mut self, i: usize, pos: f64, step: f64, played: f64) -> Option<f64> {
        let (lo, hi) = self.read_slots(i, pos, step);
        if self.seam.is_some_and(|k| lo > k) {
            self.seam = None;
        }
        if self.seam.is_some() || pos < step {
            return None;
        }
        let shape = self.shape.as_ref()?;
        let key = self.tabs[i].key?;
        let same = |j: usize| shape.tables[i][j].as_ref().is_some_and(|(k, _)| *k == key);
        let k = (lo..hi).find(|&k| shape.priors[i][k].is_some() && same(k) && same(k + 1) && shape.stamps[i][k + 1] < shape.stamps[i][k])?;
        self.seam = Some(k);
        let older = self.sum_at(i, pos - step, step)?;
        Some(played - older)
    }

    /// Where the device sample of table `i` at `pos` (`step`) reads past its window's ends, each
    /// end as (table, frames: where that table's window starts, in frames past this one's end, or
    /// ends before this one's start), or None to read the end frame there. Past its end: the
    /// take's next window in the other table, holding the frames read (the play head goes on into
    /// it); else this window's start, where it loops, unless the other holds a newer take's window
    /// (the player moves there). Before its start: the take's window before in the other table,
    /// holding the frames read. O(1) away from the ends, O(reach / CHUNK) near one.
    fn beyond(&self, i: usize, pos: f64, step: f64) -> (Option<(usize, i64)>, Option<(usize, i64)>) {
        let (Some((take, w)), Some(shape)) = (self.tabs[i].key, self.shape.as_ref()) else { return (None, None) };
        let reach = RATE_HALF as f64 * step.max(1.0);
        let (o, end) = (1 - i, self.window(i) as f64);
        if pos - reach >= 0.0 && pos + reach < end {
            return (None, None);
        }
        // whether table `t` holds its window's frames `[from, to]`
        let holds = |t: usize, from: f64, to: f64| {
            let (a, b) = ((from.max(0.0) as usize) / CHUNK, (to.max(0.0) as usize) / CHUNK);
            b < self.tabs[t].slots && (a..=b).all(|s| shape.tables[t][s].as_ref().is_some_and(|(k, _)| Some(*k) == self.tabs[t].key))
        };
        let other = self.tabs[o].key;
        let n = self.window(o) as f64;
        let before = (pos - reach < 0.0 && w.checked_sub(1).is_some_and(|p| other == Some((take, p))) && holds(o, n + pos - reach, n - 1.0)).then_some((o, n as i64));
        let after = if pos + reach < end {
            None
        } else if other == Some((take, w + 1)) && holds(o, 0.0, pos + reach - end) {
            Some((o, n as i64))
        } else if other.is_some_and(|(t, _)| t > take) || !holds(i, 0.0, pos + reach - end) {
            None
        } else {
            Some((i, end as i64))
        };
        (before, after)
    }

    /// The voices' sum at their levels for the device sample of table `i` at `pos` of its take's
    /// frames (`step`, the take's rate over the device's), through the read's rate change
    /// ([`rate_change`]: each frame a sample at its instant) and past the window's ends as
    /// [`Windows::beyond`] says, each voice's value left in the shape's `acc` (the seam's slot
    /// read from its prior), or None while a frame it reads is not there. Each chunk is looked up
    /// once. O(voices x 32 max(1, step)).
    fn sum_at(&mut self, i: usize, pos: f64, step: f64) -> Option<f64> {
        let end = self.window(i) as i64;
        let keys = [self.tabs[0].key, self.tabs[1].key];
        keys[i]?;
        let seam = self.seam.filter(|_| self.heard == Some(i));
        let (before, after) = self.beyond(i, pos, step);
        let shape: &mut Shape = self.shape.as_mut()?;
        let v = shape.voices;
        let Shape { tables, priors, acc, mix, .. } = shape;
        let (tables, priors) = (&*tables, &*priors);
        acc.fill(0.0);
        let (first, last) = (before.map_or(0, |(_, n)| -n), end - 1 + after.map_or(0, |(_, n)| n));
        let mut at: Option<(usize, usize, &[f32])> = None;
        let sum = rate_change(pos.max(0.0), step, first, last, |j, w| {
            // the table and frame of sample `j`: this window's, else past one of its ends
            let (t, f) = if (0..end).contains(&j) {
                (i, j as usize)
            } else {
                match (j < 0, before, after) {
                    (true, Some((t, n)), _) => (t, (j + n) as usize),
                    (false, _, Some((t, _))) => (t, (j - end) as usize),
                    _ => return false,
                }
            };
            let slot = f / CHUNK;
            let chunk = match at {
                Some((u, s, c)) if s == slot && u == t => c,
                _ => {
                    let c: &[f32] = match (t == i && seam == Some(slot), priors[t].get(slot), tables[t].get(slot)) {
                        (true, Some(Some(prior)), _) => prior,
                        (_, _, Some(Some((k, chunk)))) if Some(*k) == keys[t] => chunk,
                        _ => return false,
                    };
                    at = Some((t, slot, c));
                    c
                }
            };
            for (a, &x) in acc.iter_mut().zip(&chunk[(f % CHUNK) * v..(f % CHUNK + 1) * v]) {
                *a += w * f64::from(x);
            }
            true
        })?;
        acc.iter_mut().for_each(|a| *a /= sum);
        Some(acc.iter().enumerate().map(|(j, &x)| x * f64::from(mix.level(j))).sum())
    }

    /// The device sample of table `i` at `pos` of its take's frames ([`Windows::sum_at`]) mixed:
    /// each voice at its level, summed, `resid` added (the player's decaying difference at a swap
    /// or a seam), at the master level and soft limited (silence while muted), with the sum before
    /// the master; or None while a frame it reads is not there.
    /// Keeps the meters (`ms_a`: the mean square's smoothing a device sample) and writes them to
    /// the mix every [`METER_EVERY`] samples. O(voices x 32 max(1, step)).
    pub fn mix_at(&mut self, i: usize, pos: f64, step: f64, resid: f64, ms_a: f32, metered: &mut u32) -> Option<(f32, f64)> {
        let sum = self.sum_at(i, pos, step)? + resid;
        let shape: &mut Shape = self.shape.as_mut()?;
        let (mix, ms_a, v) = (&shape.mix, f64::from(ms_a), shape.voices);
        for (j, &x) in shape.acc.iter().enumerate() {
            let y = x * f64::from(mix.level(j));
            let m = &mut shape.meters[j];
            (m.0, m.1) = (m.0 + ms_a * (y * y - m.0), m.1.max(y.abs()));
        }
        let out = if mix.muted() { 0.0 } else { soft_limit(sum * f64::from(mix.master())) };
        let m = &mut shape.meters[v];
        (m.0, m.1) = (m.0 + ms_a * (out * out - m.0), m.1.max(out.abs()));
        *metered += 1;
        if *metered >= METER_EVERY {
            *metered = 0;
            for (j, m) in shape.meters.iter_mut().enumerate() {
                mix.note(j, m.1 as f32, m.0.sqrt() as f32);
                m.1 = 0.0;
            }
        }
        Some((out as f32, sum))
    }

    /// Silence on the meters while nothing of the recording plays (the first window renders, or
    /// the play head waits at the render). O(voices).
    pub fn quiet_meters(&mut self) {
        let Some(shape) = self.shape.as_mut() else { return };
        for (j, m) in shape.meters.iter_mut().enumerate() {
            *m = (0.0, 0.0);
            shape.mix.note(j, 0.0, 0.0);
        }
    }

    /// Whether table `n` holds a window of a newer take than table `h`'s.
    pub fn newer_take(&self, h: usize, n: usize) -> bool {
        matches!((self.tabs[h].key, self.tabs[n].key), (Some(a), Some(b)) if b.0 > a.0)
    }

    /// The table not heard whose window is newer than the heard one's, if any.
    pub fn newer(&self) -> Option<usize> {
        let heard = self.heard.and_then(|h| self.tabs[h].key);
        (0..2).filter(|&i| Some(i) != self.heard && self.tabs[i].key.is_some() && self.tabs[i].key > heard).max_by_key(|&i| self.tabs[i].key)
    }
}

/// A take on disk: every chunk the recorder made, in order, as little-endian f32 frames of every
/// voice before its level. The recorder queues whole chunks at their frames (`write_later`), and
/// cuts the take back when its run goes back (`cut_later`; what follows is written again over
/// it); the tape writer does both in the order queued, so a cut never races a write. Any thread
/// reads what is written. An anonymous temporary file, made by the writer at the first write: it
/// goes when the last holder lets go, or the app ends.
pub struct Tape {
    file: OnceLock<File>,
    path: PathBuf,
    /// The first error making or writing the file: the tape takes no more.
    error: OnceLock<String>,
    pub take: Take,
    pub voices: usize,
    /// The take's recording rate, Hz.
    pub rate: f64,
    /// What the take's run simulates, when the take starts at the run's start (None for a take
    /// that joined a run already going, or a run nobody keyed).
    pub content: Option<Arc<RunContent>>,
    /// The tap's own settings that shape its samples (its voices, gains, DC block), as a key.
    pub source: u64,
    /// Each voice's own settings as a key, in the tape's column order (`listen::voice_key`): a
    /// column is bit for bit what a tap of that one voice records from the same run, so a tap
    /// whose voices are all here can play its take from these columns.
    pub keys: Arc<[u64]>,
    /// The run's time of the take's first point.
    pub t0: f64,
    frames: AtomicU64,
    /// The cuts queued and not yet done, and the frames the tape has once what is queued is done.
    cuts: AtomicU64,
    promised: AtomicU64,
}

impl Tape {
    /// A new empty tape in the temporary directory: no file yet (the writer makes it), so no
    /// file I/O here. `keys` are its voices' (`Tape::keys`), `t0` its first point's time.
    pub fn create(take: Take, voices: usize, rate: f64, content: Option<Arc<RunContent>>, source: u64, keys: Arc<[u64]>, t0: f64) -> Tape {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("fab-take-{}-{n}.tape", std::process::id()));
        Tape { file: OnceLock::new(), path, error: OnceLock::new(), take, voices: voices.max(1), rate, content, source, keys, t0, frames: AtomicU64::new(0), cuts: AtomicU64::new(0), promised: AtomicU64::new(0) }
    }

    /// Queues `chunk` (whole frames of every voice) to be written from frame `at`; the tape has
    /// them once the writer is done with what was queued before. Never blocks; O(1).
    pub fn write_later(self: &Arc<Self>, at: u64, chunk: Arc<[f32]>) {
        let us = (chunk.len() / self.voices) as f64 * 1e6 / self.rate;
        self.promised.store(at + (chunk.len() / self.voices) as u64, Ordering::Release);
        tape_op(TapeOp::Write { tape: self.clone(), at, chunk }, us as u64);
    }

    /// Queues a cut back to the first `frames` (its run went back), in order with the writes:
    /// the next write is queued from them. Never blocks; O(1).
    pub fn cut_later(self: &Arc<Self>, frames: u64) {
        self.promised.fetch_min(frames, Ordering::AcqRel);
        self.cuts.fetch_add(1, Ordering::AcqRel);
        tape_op(TapeOp::Cut { tape: self.clone(), frames }, 0);
    }

    /// Why the tape takes no more, once making or writing its file failed.
    pub fn error(&self) -> Option<&str> {
        self.error.get().map(String::as_str)
    }

    /// The frames that follow from the take's content alone: those written, up to its run's
    /// clean stretch; none without content.
    pub fn clean_frames(&self) -> u64 {
        let Some(content) = &self.content else { return 0 };
        let clean = content.clean_until();
        let by_time = if clean.is_finite() { (clean * self.rate).floor().max(0.0) as u64 } else { u64::MAX };
        self.frames().min(by_time)
    }

    /// Whether `other` records the same samples: the same run content, tap settings and rate.
    pub fn same_as(&self, other: &Tape) -> bool {
        match (&self.content, &other.content) {
            (Some(a), Some(b)) => a.key == b.key && self.source == other.source && self.rate == other.rate && self.voices == other.voices,
            _ => false,
        }
    }

    pub fn bytes(&self) -> u64 {
        self.frames() * self.voices as u64 * 4
    }

    /// The writer's write of `chunk` from frame `at`, making the file at the first. `scratch`
    /// holds the bytes between calls.
    fn write(&self, at: u64, chunk: &[f32], scratch: &mut Vec<u8>) {
        if self.error.get().is_some() {
            return;
        }
        let file = match self.file.get() {
            Some(file) => file,
            None => match anonymous(OpenOptions::new().read(true).write(true).create_new(true), &self.path) {
                Ok(file) => self.file.get_or_init(|| file),
                Err(e) => {
                    let _ = self.error.set(format!("no tape for the recording: {e}"));
                    return;
                }
            },
        };
        scratch.clear();
        scratch.extend(chunk.iter().flat_map(|x| x.to_le_bytes()));
        match write_at(file, scratch, at * self.voices as u64 * 4) {
            Ok(()) => self.frames.store(at + (chunk.len() / self.voices) as u64, Ordering::Release),
            Err(e) => {
                let _ = self.error.set(format!("the recording's tape stopped: {e}"));
            }
        }
    }

    /// The take goes back to its first `frames`: a reader that took the frames before the cut
    /// may read the old ones or the new.
    fn cut(&self, frames: u64) {
        self.frames.fetch_min(frames, Ordering::AcqRel);
    }

    /// The frames written so far.
    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Acquire)
    }

    /// The frames the tape has once the writer is done with what was queued: the take's whole
    /// length once its run rests at its end, while the writer may still be at it.
    pub fn promised(&self) -> u64 {
        self.promised.load(Ordering::Acquire)
    }

    /// No cut queued for the tape waits for the writer: what `read` gives below `frames` is what
    /// the recorder taped last. A run that went back cuts the tape and tapes again from there;
    /// while that cut waits, the frames past it are the old take's. A follower of the tape reads
    /// only while this holds, and only what is written.
    pub fn settled(&self) -> bool {
        self.cuts.load(Ordering::Acquire) == 0
    }

    /// Reads whole frames from frame `from` into `out` (interleaved voices); returns the frames
    /// read, fewer at the end of what is written.
    pub fn read(&self, from: u64, out: &mut [f32]) -> io::Result<usize> {
        let frames = ((out.len() / self.voices) as u64).min(self.frames().saturating_sub(from)) as usize;
        let Some(file) = self.file.get().filter(|_| frames > 0) else { return Ok(0) };
        let mut bytes = vec![0u8; frames * self.voices * 4];
        read_at(file, &mut bytes, from * self.voices as u64 * 4)?;
        for (x, b) in out.iter_mut().zip(bytes.chunks_exact(4)) {
            *x = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        }
        Ok(frames)
    }
}

/// What the tape writer does to a tape, in the order queued.
enum TapeOp {
    Write { tape: Arc<Tape>, at: u64, chunk: Arc<[f32]> },
    Cut { tape: Arc<Tape>, frames: u64 },
    /// Everything queued before is done (`settle_tapes`).
    Settle(SyncSender<()>),
    /// The writer waits here until released (`hold_tapes`).
    Hold(Receiver<()>),
}

/// The audio queued for the writer and not yet written, in microseconds at its tapes' rates.
static QUEUED_US: AtomicU64 = AtomicU64::new(0);
/// The queue was said to be behind ([`BEHIND_SECONDS`]); again once it caught up.
static SAID_BEHIND: AtomicBool = AtomicBool::new(false);

/// Audio queued for the tape writer past which a log line says the disk is behind: nothing is
/// dropped, the chunks wait in memory.
pub const BEHIND_SECONDS: f64 = 3.0;

/// The process's tape writer: one long-lived thread, made at the first tape operation, fed by an
/// unbounded channel, so a recorder never waits on the disk and nothing it queues is dropped.
/// None when the thread could not start.
fn tape_writer() -> Option<&'static Sender<TapeOp>> {
    static WRITER: OnceLock<Option<Sender<TapeOp>>> = OnceLock::new();
    WRITER
        .get_or_init(|| {
            let (tx, rx) = channel();
            let spawned = std::thread::Builder::new().name("elec tape writer".into()).spawn(move || write_tapes(rx));
            match spawned {
                Ok(_) => Some(tx),
                Err(e) => {
                    eprintln!("fab-elec listen: the tape writer could not start ({e}); recordings are not taped");
                    None
                }
            }
        })
        .as_ref()
}

/// Queues `op` for the writer (`us`: the audio it writes). Never blocks. A tape whose op cannot
/// be queued (no writer) takes no more.
fn tape_op(op: TapeOp, us: u64) {
    let tape = match &op {
        TapeOp::Write { tape, .. } | TapeOp::Cut { tape, .. } => Some(tape.clone()),
        TapeOp::Settle(_) | TapeOp::Hold(_) => None,
    };
    let cut = matches!(op, TapeOp::Cut { .. });
    let queued = QUEUED_US.fetch_add(us, Ordering::Relaxed) + us;
    if queued as f64 > BEHIND_SECONDS * 1e6 && !SAID_BEHIND.swap(true, Ordering::Relaxed) {
        eprintln!("fab-elec listen: the tape writer is {:.1} s of audio behind (the disk is slow); the chunks wait in memory until it catches up", queued as f64 * 1e-6);
    }
    if tape_writer().is_none_or(|tx| tx.send(op).is_err()) {
        QUEUED_US.fetch_sub(us, Ordering::Relaxed);
        if let Some(tape) = tape {
            if cut {
                tape.cuts.fetch_sub(1, Ordering::AcqRel);
            }
            let _ = tape.error.set("no tape writer".into());
        }
    }
}

/// The tape writer's loop: each op in the order queued.
fn write_tapes(rx: Receiver<TapeOp>) {
    let mut scratch = Vec::new();
    for op in rx {
        match op {
            TapeOp::Write { tape, at, chunk } => {
                tape.write(at, &chunk, &mut scratch);
                let us = ((chunk.len() / tape.voices) as f64 * 1e6 / tape.rate) as u64;
                let left = QUEUED_US.fetch_sub(us, Ordering::Relaxed).saturating_sub(us);
                if (left as f64) < 0.5 * BEHIND_SECONDS * 1e6 {
                    SAID_BEHIND.store(false, Ordering::Relaxed);
                }
            }
            TapeOp::Cut { tape, frames } => {
                tape.cut(frames);
                tape.cuts.fetch_sub(1, Ordering::AcqRel);
            }
            TapeOp::Settle(done) => {
                let _ = done.send(());
            }
            TapeOp::Hold(release) => {
                let _ = release.recv();
            }
        }
    }
}

/// Waits until the tape writer is done with every write and cut queued before this call (from
/// this thread, and from others before it): for a reader that wants a whole take now (a save,
/// a test). Blocks, so never on the UI thread, the worker or the audio callback.
pub fn settle_tapes() {
    let (tx, rx) = sync_channel(1);
    tape_op(TapeOp::Settle(tx), 0);
    let _ = rx.recv();
}

/// Holds the tape writer, as a disk that stalls, until the hold drops: what is queued meanwhile
/// waits in memory in order, every tape's (the writer is the process's). For proofs that a
/// reader never reads what a queued cut takes back (`Tape::settled`). Never blocks.
pub fn hold_tapes() -> TapeHold {
    let (release, held) = sync_channel(1);
    tape_op(TapeOp::Hold(held), 0);
    TapeHold(release)
}

/// The tape writer held ([`hold_tapes`]) until this drops.
pub struct TapeHold(SyncSender<()>);

impl Drop for TapeHold {
    fn drop(&mut self) {
        let _ = self.0.try_send(());
    }
}

/// Opens `path` so it goes when its last handle closes: removed at once on Unix, delete-on-close
/// on Windows. Nothing is left behind by a session that ends or crashes (the spill uses it too).
#[cfg(unix)]
pub(crate) fn anonymous(options: &mut OpenOptions, path: &std::path::Path) -> io::Result<File> {
    let file = options.open(path)?;
    std::fs::remove_file(path)?;
    Ok(file)
}

#[cfg(windows)]
pub(crate) fn anonymous(options: &mut OpenOptions, path: &std::path::Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
    options.custom_flags(FILE_FLAG_DELETE_ON_CLOSE).open(path)
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn anonymous(_options: &mut OpenOptions, _path: &std::path::Path) -> io::Result<File> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "no temporary files here"))
}

#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], at: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, at)
}

#[cfg(windows)]
fn read_at(file: &File, mut buf: &mut [u8], mut at: u64) -> io::Result<()> {
    while !buf.is_empty() {
        match std::os::windows::fs::FileExt::seek_read(file, buf, at)? {
            0 => return Err(io::ErrorKind::UnexpectedEof.into()),
            n => {
                buf = &mut buf[n..];
                at += n as u64;
            }
        }
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn read_at(_file: &File, _buf: &mut [u8], _at: u64) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "no temporary files here"))
}

#[cfg(unix)]
fn write_at(file: &File, buf: &[u8], at: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, buf, at)
}

#[cfg(windows)]
fn write_at(file: &File, mut buf: &[u8], mut at: u64) -> io::Result<()> {
    while !buf.is_empty() {
        match std::os::windows::fs::FileExt::seek_write(file, buf, at)? {
            0 => return Err(io::ErrorKind::WriteZero.into()),
            n => {
                buf = &buf[n..];
                at += n as u64;
            }
        }
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn write_at(_file: &File, _buf: &[u8], _at: u64) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "no temporary files here"))
}

/// What one run simulates, as the host knows it when the run starts: a key of everything that
/// decides its samples (the circuit with every value, the bench, the knobs, the firmware and the
/// run's settings), the edit that started the run when one did, and the run's time up to which
/// nothing else changed it (an input, a knob or a setting changed mid-run ends the clean stretch
/// there: what follows is not the key's alone). Keys are compared within the process that made
/// them only. Shared by the host (which ends the clean stretch) and the takes that record the run;
/// every operation is O(1) and lock-free.
#[derive(Debug)]
pub struct RunContent {
    pub key: u128,
    pub edit: Option<Arc<str>>,
    /// The rate the run's takes are recorded at, Hz (0: none chosen, the device's).
    pub recording_rate: f64,
    /// f64 bits of the clean stretch's end (non-negative: bits order as the values do).
    clean_until: AtomicU64,
}

impl RunContent {
    /// A run whose samples follow from `key` alone until something changes it, its takes
    /// recorded at `recording_rate` Hz (0: none chosen, the device's).
    pub fn new(key: u128, edit: Option<Arc<str>>, recording_rate: f64) -> RunContent {
        RunContent { key, edit, recording_rate, clean_until: AtomicU64::new(f64::INFINITY.to_bits()) }
    }

    /// The run's time up to which its samples follow from `key` alone (infinite while nothing
    /// changed it).
    pub fn clean_until(&self) -> f64 {
        f64::from_bits(self.clean_until.load(Ordering::Acquire))
    }

    /// Something changed the run at its time `t` (or later): the clean stretch ends at `t` at
    /// the latest.
    pub fn taint(&self, t: f64) {
        let t = if t.is_nan() { 0.0 } else { t.max(0.0) };
        self.clean_until.fetch_min(t.to_bits(), Ordering::AcqRel);
    }
}

/// The run starting now, for the recorder to stamp its next take with: the host puts each run's
/// content up before the run's worker starts, and runs start one after another, so a take that
/// starts at a run's start finds that run's. The lock guards a pointer swap.
#[derive(Debug, Default)]
pub struct RunSlot(Mutex<Option<Arc<RunContent>>>);

impl RunSlot {
    pub fn set(&self, content: Option<Arc<RunContent>>) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = content;
    }

    pub fn current(&self) -> Option<Arc<RunContent>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// The tape of the take each tap records (one slot a tap, by the sink's id: a board's jack
/// recorder and a heard net's tap tape apart), shared by the worker's recorders (which make a
/// new one at each take) and the UI (which saves it), and why taping stopped, if it did. The
/// kept takes and their budget are shared. The lock guards a few pointer swaps, never work.
#[derive(Default)]
pub struct TapeShelf {
    tape: Mutex<Vec<(u64, Arc<Tape>)>>,
    error: Mutex<Option<String>>,
    /// The takes kept for replay, least recently used first.
    kept: Mutex<Vec<Arc<Tape>>>,
    /// Chunks played from a kept take, rendered chunks that differed from the kept take's
    /// (a determinism monitor: 0 unless the run is not a function of its content; a restart's
    /// operating point seeded from the last run moves the low bits), and kept takes let go for
    /// room.
    pub replayed: AtomicU64,
    pub differed: AtomicU64,
    pub evicted: AtomicU64,
    /// Takes nobody heard that stopped taping at their bound (`RecordSpec::bound`): again each
    /// time a run that went back before it reaches it.
    pub bounded: AtomicU64,
    /// The largest sample difference the replaying take's render found (f32 bits, full scale 1):
    /// how far the differing chunks differ. Starts at 0 with each take that replays.
    pub worst: AtomicU32,
}

use crate::budget::KEEP_BYTES;

impl TapeShelf {
    /// The take `slot`'s tap records now. O(slots).
    pub fn current(&self, slot: u64) -> Option<Arc<Tape>> {
        self.tape.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(s, _)| *s == slot).map(|(_, tape)| tape.clone())
    }

    /// Puts `tape` up as `slot`'s current take (None: the slot is empty); the one it replaces is
    /// returned for the caller to let go of. O(slots).
    pub fn set(&self, slot: u64, tape: Option<Arc<Tape>>) -> Option<Arc<Tape>> {
        let mut slots = self.tape.lock().unwrap_or_else(|e| e.into_inner());
        let at = slots.iter().position(|(s, _)| *s == slot);
        match (at, tape) {
            (Some(at), Some(tape)) => Some(std::mem::replace(&mut slots[at].1, tape)),
            (Some(at), None) => Some(slots.swap_remove(at).1),
            (None, Some(tape)) => {
                slots.push((slot, tape));
                None
            }
            (None, None) => None,
        }
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// A new take's tape goes up in `slot` (`set`; the one it replaces is let go of here), and when it
    /// starts with its run it is kept for later takes of its content. Returns the longest clean
    /// take of the same content kept before it, to replay, with its whole chunks. A kept take a
    /// longer one of its content covers goes (stale by content), and past [`KEEP_BYTES`] the
    /// least recently used. O(kept takes); called once a take, on the worker.
    pub fn start(&self, slot: u64, tape: Arc<Tape>) -> Option<(Arc<Tape>, u64)> {
        drop(self.set(slot, Some(tape.clone())));
        tape.content.as_ref()?;
        let mut kept = self.kept.lock().unwrap_or_else(|e| e.into_inner());
        let best = kept.iter().filter(|t| t.same_as(&tape)).max_by_key(|t| t.clean_frames()).cloned();
        kept.retain(|t| !t.same_as(&tape) || best.as_ref().is_some_and(|b| Arc::ptr_eq(t, b)));
        if let Some(b) = &best {
            kept.retain(|t| !Arc::ptr_eq(t, b));
            kept.push(b.clone());
        }
        kept.push(tape);
        let mut bytes: u64 = kept.iter().map(|t| t.bytes()).sum();
        while bytes > KEEP_BYTES && kept.len() > 2 {
            let gone = kept.remove(0);
            bytes -= gone.bytes();
            self.evicted.fetch_add(1, Ordering::Relaxed);
        }
        let chunks = best.as_ref().map_or(0, |b| b.clean_frames() / CHUNK as u64);
        if chunks > 0 {
            self.worst.store(0, Ordering::Relaxed);
        }
        best.filter(|_| chunks > 0).map(|b| (b, chunks))
    }

    pub fn set_error(&self, error: Option<String>) {
        *self.error.lock().unwrap_or_else(|e| e.into_inner()) = error;
    }
}

/// Writes `tape` from its start to what is written now as a WAV file: the voices at `mix`'s
/// levels and master, summed and soft limited as the player mixes them, as 32-bit float mono at
/// the tape's rate. The mute is a monitoring control and does not silence a save. Streams in
/// blocks; returns the frames written.
pub fn write_wav(tape: &Tape, mix: &TapMix, out: &mut dyn Write) -> io::Result<u64> {
    let frames = tape.frames();
    let data = frames.checked_mul(4).filter(|&n| n <= u64::from(u32::MAX) - 64).ok_or_else(|| io::Error::other("the recording is too long for a WAV file"))?;
    wav_header(out, tape.rate.round() as u32, 1, frames, data as u32)?;
    let (v, master) = (tape.voices, f64::from(mix.master()));
    let mut block = vec![0.0f32; CHUNK * 16 * v];
    let mut bytes = Vec::with_capacity(CHUNK * 16 * 4);
    let mut at = 0;
    while at < frames {
        let n = tape.read(at, &mut block)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        bytes.clear();
        for frame in block[..n * v].chunks_exact(v) {
            let sum: f64 = frame.iter().enumerate().map(|(j, &x)| f64::from(x) * f64::from(mix.level(j))).sum();
            bytes.extend_from_slice(&(soft_limit(sum * master) as f32).to_le_bytes());
        }
        out.write_all(&bytes)?;
        at += n as u64;
    }
    Ok(frames)
}

/// A float WAV's header: RIFF, `fmt ` (IEEE float), `fact` and the `data` chunk's header.
fn wav_header(out: &mut dyn Write, rate: u32, channels: u16, frames: u64, data: u32) -> io::Result<()> {
    let block = 4 * u32::from(channels);
    let mut h = Vec::with_capacity(58);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&(4 + 26 + 12 + 8 + data).to_le_bytes());
    h.extend_from_slice(b"WAVEfmt ");
    h.extend_from_slice(&18u32.to_le_bytes());
    h.extend_from_slice(&3u16.to_le_bytes());
    h.extend_from_slice(&channels.to_le_bytes());
    h.extend_from_slice(&rate.to_le_bytes());
    h.extend_from_slice(&(rate * block).to_le_bytes());
    h.extend_from_slice(&(block as u16).to_le_bytes());
    h.extend_from_slice(&32u16.to_le_bytes());
    h.extend_from_slice(&0u16.to_le_bytes());
    h.extend_from_slice(b"fact");
    h.extend_from_slice(&4u32.to_le_bytes());
    h.extend_from_slice(&(frames.min(u64::from(u32::MAX)) as u32).to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&data.to_le_bytes());
    out.write_all(&h)
}
