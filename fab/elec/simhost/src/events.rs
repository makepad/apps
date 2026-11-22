//! The firmware event store: what a co-simulated device reports in simulated time (the firmware
//! image's own watch stream, and the emulator's trace: handler entries and exits, statements when
//! enabled), kept for the scope's firmware lane, the code view's stepping and `elec.fw.watch`.
//!
//! - The device offers one batch per commit to its [`EventFeed`], a bounded channel of batches
//!   (`try_send`). A full channel refuses the batch: the device keeps it, in order, and offers it
//!   again first, holding its time until it is taken, so nothing is dropped; the feed's `full`
//!   flag says so, and the host names it as the run's limit. Each offer wakes the UI, so the host
//!   drains ([`EventLog::drain`], `try_recv`, never waiting) on that signal, with or without a
//!   redraw.
//! - Nothing a reader has not read is dropped. Each registered reader ([`EventStore::register`])
//!   has a cursor (the sequence number of the next event it reads); events live in chunks, and a
//!   chunk is freed only once every reader has passed it and all of it is older than the visible
//!   length of simulated time (the scope's span, what [`EventStore::visible`] shows). A stalled
//!   reader makes the store grow; past [`LARGE`] events that is logged once, and the data stays.
//! - Growth appends a chunk and never moves an event; freeing pops whole chunks from the front.
//! - A new run ([`EventStore::restart`], its time from zero again) starts the visible span afresh;
//!   the earlier run's events stay for the readers that have not read them.
//! - A live run that goes back (an edit behind its front) hands the device's sink to an earlier
//!   copy, which sends the events from its place again; its first batch says how many of the
//!   newest events are void ([`EventFeed::offer_voiding`]), in the stream, and the store drops
//!   them ([`EventStore::rewind`]). A reader that had read past them reads on from the new end,
//!   and [`EventStore::rewinds`] counts these, so a reader keeping state from its reads can tell.
//! - The frame type `F` is the device's own; the store reads only each event's time and, through
//!   the log's `source`, where it came from. The host holds the log type-erased ([`EventPump`]).

use makepad_widgets_core::log;
use std::any::Any;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;

/// Events per chunk.
pub const CHUNK: usize = 1024;

/// Held events past which a stalled reader's growth is logged (once per episode).
pub const LARGE: usize = 1 << 20;

/// Batches a feed holds before it refuses one: a device commits a batch per step it commits, so
/// this is a few frames of a fast device. A full feed slows the device; it never drops events.
pub const BATCHES: usize = 4096;

/// Where a firmware event comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventSource {
    /// The firmware image's watch stream, as a board would send it.
    Image,
    /// The emulator's trace (handler entry and exit, statements when enabled).
    Trace,
}

/// One event at a simulated time.
#[derive(Clone, Debug, PartialEq)]
pub struct FwEvent<F> {
    pub t: f64,
    pub source: EventSource,
    pub frame: F,
}

/// A registered reader of a store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ReaderId(u32);

#[derive(Clone, Debug)]
struct Chunk<F> {
    events: Vec<FwEvent<F>>,
    /// The newest event time in the chunk.
    newest: f64,
}

/// A device's events in arrival order, held while the visible span or a reader needs them.
#[derive(Clone, Debug)]
pub struct EventStore<F> {
    chunks: VecDeque<Chunk<F>>,
    /// The sequence number of the first held event.
    base: u64,
    held: usize,
    length: f64,
    /// The newest event time of the current run (sources may interleave out of time order).
    newest: f64,
    /// The sequence number of the current run's first event.
    run: u64,
    /// The next event opens a chunk (the first of a run).
    split: bool,
    /// Each registered reader and the sequence number of the next event it reads.
    readers: Vec<(ReaderId, u64)>,
    next_reader: u32,
    large: bool,
    /// The rewinds taken ([`EventStore::rewind`]).
    rewinds: u64,
}

impl<F> EventStore<F> {
    /// An empty store showing `length` simulated seconds.
    pub fn new(length: f64) -> EventStore<F> {
        EventStore {
            chunks: VecDeque::new(),
            base: 0,
            held: 0,
            length,
            newest: f64::NEG_INFINITY,
            run: 0,
            split: false,
            readers: Vec::new(),
            next_reader: 0,
            large: false,
            rewinds: 0,
        }
    }

    /// A new run starts (its time from zero again): the visible span and time queries cover its
    /// events only, and the earlier run's are freed once every reader has passed them. O(1).
    pub fn restart(&mut self) {
        self.run = self.next();
        self.newest = f64::NEG_INFINITY;
        self.split = true;
        self.release();
    }

    /// The sequence number of the current run's first event: a reader whose events cross it has
    /// seen a new run begin.
    pub fn run_start(&self) -> u64 {
        self.run
    }

    pub fn length(&self) -> f64 {
        self.length
    }

    /// A new visible length (the scope's span); what no reader needs and the span no longer shows
    /// is freed.
    pub fn set_length(&mut self, length: f64) {
        self.length = length;
        self.release();
    }

    /// Appends an event. O(1), and O(chunks freed).
    pub fn push(&mut self, event: FwEvent<F>) {
        self.append(event);
        self.release();
    }

    /// Takes the batches the device sent without waiting, each event with its source, a batch's
    /// void events dropped first ([`EventStore::rewind`]); returns how many events arrived.
    /// O(events), and O(chunks freed).
    pub fn drain(&mut self, rx: &Receiver<(u64, Vec<(f64, F)>)>, source: impl Fn(&F) -> EventSource) -> usize {
        self.drain_batches(rx, source).1
    }

    /// [`drain`](EventStore::drain), with the batches taken (a batch may void events and bring
    /// none): `(batches, events)`.
    fn drain_batches(&mut self, rx: &Receiver<(u64, Vec<(f64, F)>)>, source: impl Fn(&F) -> EventSource) -> (usize, usize) {
        let (mut batches, mut n) = (0, 0);
        while let Ok((void, batch)) = rx.try_recv() {
            batches += 1;
            if void > 0 {
                self.rewind(void);
            }
            n += batch.len();
            for (t, frame) in batch {
                let source = source(&frame);
                self.append(FwEvent { t, source, frame });
            }
        }
        if n > 0 {
            self.release();
        }
        (batches, n)
    }

    /// The device's `void` newest events are void: the live run went back to an earlier copy of
    /// it, which sends them again. Drops them, never before the current run's start; a reader past
    /// them reads on from the new end. O(void + the current run's chunks).
    pub fn rewind(&mut self, void: u64) {
        let end = self.next();
        let keep = end.saturating_sub(void).max(self.run).max(self.base);
        let mut drop = (end - keep) as usize;
        if drop == 0 {
            return;
        }
        self.rewinds += 1;
        while drop > 0 {
            let Some(chunk) = self.chunks.back_mut() else { break };
            let k = drop.min(chunk.events.len());
            chunk.events.truncate(chunk.events.len() - k);
            chunk.newest = chunk.events.iter().map(|e| e.t).fold(f64::NEG_INFINITY, f64::max);
            if chunk.events.is_empty() {
                self.chunks.pop_back();
            }
            self.held -= k;
            drop -= k;
        }
        // the current run's chunks: from the back, down to the run's start
        let mut start = self.next();
        self.newest = f64::NEG_INFINITY;
        for chunk in self.chunks.iter().rev() {
            if start <= self.run {
                break;
            }
            self.newest = self.newest.max(chunk.newest);
            start -= chunk.events.len() as u64;
        }
        // a run whose events all went opens a chunk again with its next
        self.split |= keep == self.run;
        for (_, cursor) in &mut self.readers {
            *cursor = (*cursor).min(keep);
        }
    }

    /// The rewinds this store took ([`EventStore::rewind`]): a reader that keeps state from its
    /// reads starts over when it changes.
    pub fn rewinds(&self) -> u64 {
        self.rewinds
    }

    /// A reader from the oldest event held on: it reads every event from there, in order, and the
    /// store keeps each one until it has.
    pub fn register(&mut self) -> ReaderId {
        let id = ReaderId(self.next_reader);
        self.next_reader += 1;
        self.readers.push((id, self.base));
        id
    }

    /// The reader is done; what only it held is freed.
    pub fn unregister(&mut self, id: ReaderId) {
        self.readers.retain(|(r, _)| *r != id);
        self.release();
    }

    /// The events the reader has not read, in arrival order; it has read them after this. Empty for
    /// an unknown reader. What it passed is freed at the next push, drain or length change.
    pub fn read(&mut self, id: ReaderId) -> impl Iterator<Item = &FwEvent<F>> {
        let next = self.next();
        let from = match self.readers.iter_mut().find(|(r, _)| *r == id) {
            Some((_, cursor)) => std::mem::replace(cursor, next),
            None => next,
        };
        self.from(from)
    }

    /// The events within the visible length of the newest one (the lane's display).
    pub fn visible(&self) -> impl Iterator<Item = &FwEvent<F>> {
        self.span(self.newest - self.length, self.newest)
    }

    /// The held events whose time is in `[t0, t1]`.
    pub fn span(&self, t0: f64, t1: f64) -> impl Iterator<Item = &FwEvent<F>> {
        self.from(self.run).filter(move |e| e.t >= t0 && e.t <= t1)
    }

    /// The sequence number the next event gets.
    pub fn next(&self) -> u64 {
        self.base + self.held as u64
    }

    /// The events held.
    pub fn len(&self) -> usize {
        self.held
    }

    pub fn is_empty(&self) -> bool {
        self.held == 0
    }

    /// The held events from sequence number `seq` on. Chunks differ in length (a run opens one, so
    /// the one an earlier run ended in stays part full, and a rewind shortens the newest), so the
    /// chunk is found by walking back from the newest, where readers read. O(chunks walked).
    fn from(&self, seq: u64) -> impl Iterator<Item = &FwEvent<F>> {
        let skip = seq.saturating_sub(self.base).min(self.held as u64);
        let (mut first, mut start) = (self.chunks.len(), self.held as u64);
        while first > 0 && start > skip {
            first -= 1;
            start -= self.chunks[first].events.len() as u64;
        }
        self.chunks.iter().skip(first).flat_map(|c| c.events.iter()).skip((skip - start) as usize)
    }

    fn append(&mut self, event: FwEvent<F>) {
        self.newest = self.newest.max(event.t);
        if self.split || self.chunks.back().is_none_or(|c| c.events.len() == CHUNK) {
            self.split = false;
            self.chunks.push_back(Chunk { events: Vec::with_capacity(CHUNK), newest: f64::NEG_INFINITY });
        }
        if let Some(chunk) = self.chunks.back_mut() {
            chunk.newest = chunk.newest.max(event.t);
            chunk.events.push(event);
        }
        self.held += 1;
        if self.held > LARGE && !self.large {
            self.large = true;
            log!("elec sim: the firmware event store holds {} events for a reader that has not read them; it keeps them all", self.held);
        }
    }

    /// Frees front chunks every reader has passed that lie wholly outside the visible length.
    /// O(readers + chunks freed).
    fn release(&mut self) {
        let read = self.readers.iter().map(|&(_, cursor)| cursor).min().unwrap_or(u64::MAX);
        let oldest = self.newest - self.length;
        while let Some(chunk) = self.chunks.front() {
            let end = self.base + chunk.events.len() as u64;
            // the chunk still filling is never freed; a full one, or one an earlier run ended, is
            // once every reader passed it and none of it is in the current run's visible span
            let open = self.chunks.len() == 1 && chunk.events.len() < CHUNK;
            if open || end > read || (end > self.run && chunk.newest >= oldest) {
                break;
            }
            self.base = end;
            self.held -= chunk.events.len();
            self.chunks.pop_front();
        }
        if self.large && self.held <= LARGE / 2 {
            self.large = false;
        }
    }
}

/// The device's end of an [`EventLog`]: [`offer`](EventFeed::offer) is the device's event sink.
/// Cloneable; `Send` when `F` is.
pub struct EventFeed<F> {
    tx: SyncSender<(u64, Vec<(f64, F)>)>,
    full: Arc<AtomicBool>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl<F> Clone for EventFeed<F> {
    fn clone(&self) -> Self {
        EventFeed { tx: self.tx.clone(), full: self.full.clone(), wake: self.wake.clone() }
    }
}

impl<F> EventFeed<F> {
    /// Offers one batch without waiting and wakes the UI to drain. Taken: the batch is emptied and
    /// true returned. Full: the batch is left as it was, the log's `full` flag set, and false
    /// returned; the device keeps it and offers it again first. A feed whose log the host dropped
    /// takes the batch and discards it: no reader exists, and the device must not wait forever.
    pub fn offer(&self, batch: &mut Vec<(f64, F)>) -> bool {
        self.offer_voiding(0, batch)
    }

    /// [`offer`](EventFeed::offer), the log dropping its `void` newest events first, in the stream
    /// ([`EventStore::rewind`]): the device's earlier copy took over and sends them again. A batch
    /// refused goes again with its `void`; an empty one goes when `void` is not 0.
    pub fn offer_voiding(&self, void: u64, batch: &mut Vec<(f64, F)>) -> bool {
        if batch.is_empty() && void == 0 {
            return true;
        }
        let taken = match self.tx.try_send((void, std::mem::take(batch))) {
            Ok(()) | Err(TrySendError::Disconnected(_)) => true,
            Err(TrySendError::Full((_, refused))) => {
                *batch = refused;
                self.full.store(true, Ordering::Relaxed);
                false
            }
        };
        (self.wake)();
        taken
    }
}

/// A device's event store with its channel's receiving end, on the UI thread.
pub struct EventLog<F> {
    pub store: EventStore<F>,
    rx: Receiver<(u64, Vec<(f64, F)>)>,
    source: fn(&F) -> EventSource,
    full: Arc<AtomicBool>,
}

impl<F> EventLog<F> {
    /// A log showing `length` simulated seconds, and the feed of [`BATCHES`] batches whose
    /// offers call `wake`; `source` says where each frame came from.
    pub fn new(length: f64, source: fn(&F) -> EventSource, wake: Arc<dyn Fn() + Send + Sync>) -> (EventLog<F>, EventFeed<F>) {
        let (tx, rx) = sync_channel(BATCHES);
        let full = Arc::new(AtomicBool::new(false));
        (EventLog { store: EventStore::new(length), rx, source, full: full.clone() }, EventFeed { tx, full, wake })
    }

    /// Takes what the feed holds without waiting (see [`EventStore::drain`]); a feed it took
    /// batches from (with events or only a void) has room again, so it is no longer full.
    /// Returns the events taken.
    pub fn drain(&mut self) -> usize {
        let (batches, n) = self.store.drain_batches(&self.rx, self.source);
        if batches > 0 {
            self.full.store(false, Ordering::Relaxed);
        }
        n
    }

    /// Whether the feed refused a batch the log has not made room for since: the device waits.
    pub fn full(&self) -> bool {
        self.full.load(Ordering::Relaxed)
    }
}

/// A host's type-erased [`EventLog`]: drained on the UI thread at each step, and read by the code
/// that knows its frame type through [`EventPump::as_any_mut`].
pub trait EventPump: Any + Send {
    /// [`EventLog::drain`].
    fn pump(&mut self) -> usize;
    /// [`EventLog::full`].
    fn full(&self) -> bool;
    /// [`EventStore::restart`].
    fn restart(&mut self);
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

impl<F: Send + 'static> EventPump for EventLog<F> {
    fn pump(&mut self) -> usize {
        self.drain()
    }

    fn full(&self) -> bool {
        EventLog::full(self)
    }

    fn restart(&mut self) {
        self.store.restart();
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
