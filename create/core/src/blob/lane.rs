//! The pulled blob lane. The caller supplies monotonic milliseconds and polls a carrier's
//! output only when its connection is writable. Store results are delivered on this same lane.

use super::{Body, Hash, Message, Ranges, CHUNK};
pub use super::Served as ReadResult;
use crate::event::{Id, TownId};
use crate::key::PublicKey;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::sync::Arc;

pub const MAX_WINDOW: usize = 64;
const SOURCE_WAIT_MS: u64 = 4_000;
type Blob = (TownId, Id);

#[derive(Default)]
pub struct TownView {
    pub open: bool,
    pub referenced: HashMap<Id, u64>,
    pub authored: HashSet<Id>,
}

#[derive(Debug)]
pub enum StoreRequest {
    Read { carrier: PublicKey, town: TownId, id: Id, index: u64 },
    Chunk { carrier: PublicKey, town: TownId, id: Id, index: u64, proof: Arc<[Hash]>, bytes: Arc<[u8]> },
}

#[derive(Debug, PartialEq, Eq)]
pub enum Status {
    Waiting { town: TownId, id: Id },
    Progress { town: TownId, id: Id, have: u64, size: u64, from: PublicKey },
    Done { town: TownId, id: Id },
    Bad { carrier: PublicKey, town: TownId, id: Id },
}

#[derive(Debug, PartialEq, Eq)]
pub enum Received {
    Dropped,
    Control,
    Ready,
    /// The lane copied an admitted CHUNK. poll_store hands it to the worker exactly once.
    Store,
}

enum Answer {
    Missing { referenced: bool, before_ready: bool },
    Ask(VecDeque<Range<u64>>),
}

struct Slot {
    blob: Blob,
    weight: usize,
    answer: Answer,
}

struct Read {
    blob: Blob,
    index: u64,
    // Keep the outstanding operation until its callback, even across cancel and re-ask.
    valid: bool,
}

struct Chunk {
    blob: Blob,
    index: u64,
    proof: Vec<Hash>,
    bytes: Vec<u8>,
}

struct Delivery {
    blob: Blob,
    index: u64,
    proof: Arc<[Hash]>,
    bytes: Arc<[u8]>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Availability { Missing, RetryAfterReady, Have }

#[derive(Default)]
struct Carrier {
    views: HashMap<TownId, TownView>,
    announce: BTreeSet<TownId>,
    slots: VecDeque<Slot>,
    reading: Option<Read>,
    ready: Option<Chunk>,
    awaiting_have: HashSet<Blob>,
    have: BTreeSet<Blob>,

    bad: HashSet<Blob>,
    availability: HashMap<Blob, Availability>,
    // Reserve each WANT's weight until a replacement or an ordered cancel. During a
    // refill this also covers crossing replies that READY may need to request again.
    advertised: BTreeMap<Blob, usize>,
    cancel: BTreeSet<Blob>,
    deliveries: VecDeque<Delivery>,
    // One dispatched write per carrier makes a bad(carrier,town,id) callback unambiguous.
    writing: Option<(Blob, u64)>,
    recent_writes: VecDeque<u64>,
    fetches: BTreeSet<Blob>,
    last_want: Option<Blob>,
}

impl Carrier {
    fn size(&self, (town, id): Blob) -> Option<u64> {
        self.views.get(&town).filter(|v| v.open)?.referenced.get(&id).copied()
    }

    fn eligible(&self, blob: Blob, size: u64) -> bool {
        self.size(blob) == Some(size) && !self.bad.contains(&blob)
            && self.availability.get(&blob) != Some(&Availability::Missing)
    }

    fn invalidate_read(&mut self, blob: Blob) {
        if let Some(read) = &mut self.reading {
            if read.blob == blob { read.valid = false; }
        }
        if self.ready.as_ref().is_some_and(|c| c.blob == blob) { self.ready = None; }
    }

    fn remove_slot(&mut self, blob: Blob) {
        self.invalidate_read(blob);
        self.slots.retain(|s| s.blob != blob);
    }

    fn want(&mut self, blob: Blob, ranges: Ranges<'_>, complete: &HashSet<Blob>, chunk: usize) -> Received {
        if !self.views.get(&blob.0).is_some_and(|v| v.open) { return Received::Dropped; }
        let others: usize = self.slots.iter().filter(|s| s.blob != blob).map(|s| s.weight).sum();
        let available = (MAX_WINDOW - others) as u64;
        let mut count = 0u64;
        let mut end = 0;
        for r in ranges.iter() {
            if r.start < end || r.start >= r.end || r.end - r.start > available - count {
                return Received::Dropped;
            }
            count += r.end - r.start;
            end = r.end;
        }
        if count.max(1) > available { return Received::Dropped; }
        self.remove_slot(blob);
        if count == 0 { return Received::Control; }

        let referenced = self.size(blob);
        let answer = match (referenced, complete.contains(&blob)) {
            (Some(size), true) => {
                let n = size.div_ceil(chunk as u64).max(1);
                let clipped: VecDeque<_> = ranges.iter().take_while(|r| r.start < n)
                    .map(|r| r.start..r.end.min(n)).collect();
                if clipped.is_empty() { Answer::Missing { referenced: false, before_ready: false } }
                else { Answer::Ask(clipped) }
            }
            _ => Answer::Missing { referenced: referenced.is_some(), before_ready: false },
        };
        self.slots.push_back(Slot { blob, weight: count as usize, answer });
        Received::Control
    }

    fn next_read(&mut self, carrier: PublicKey) -> Option<StoreRequest> {
        if self.reading.is_some() || self.ready.is_some() { return None; }
        let pos = self.slots.iter().position(|s| matches!(s.answer, Answer::Ask(_)))?;
        let slot = self.slots.remove(pos)?;
        let Answer::Ask(ranges) = &slot.answer else { return None };
        let index = ranges.front()?.start;
        let (town, id) = slot.blob;
        self.reading = Some(Read { blob: slot.blob, index, valid: true });
        self.slots.push_back(slot);
        Some(StoreRequest::Read { carrier, town, id, index })
    }

    fn send_chunk(&mut self) -> Option<Vec<u8>> {
        let c = self.ready.take()?;
        let pos = self.slots.iter().position(|s| s.blob == c.blob)?;
        let slot = self.slots.get_mut(pos)?;
        let Answer::Ask(ranges) = &mut slot.answer else { return None };
        let range = ranges.front_mut()?;
        if range.start != c.index { return None; }
        let wire = encode(c.blob, Body::Chunk { index: c.index, proof: &c.proof, bytes: &c.bytes })?;
        range.start += 1;
        if range.is_empty() { ranges.pop_front(); }
        slot.weight -= 1;
        if ranges.is_empty() { self.slots.remove(pos); }
        Some(wire)
    }

    fn window(&self, now: u64) -> usize {
        let rate = self.recent_writes.iter().rev().take_while(|&&t| now.saturating_sub(t) < 250).count();
        (2 * rate).clamp(4, MAX_WINDOW)
    }
}

fn encode((town, id): Blob, body: Body<'_>) -> Option<Vec<u8>> {
    // Wire credit changes only after successful encoding.
    Message { town, id, body }.encode().ok()
}

fn encode_want(blob: Blob, indices: &BTreeSet<u64>) -> Option<Vec<u8>> {
    let mut ranges: Vec<Range<u64>> = Vec::new();
    for &i in indices {
        if let Some(r) = ranges.last_mut().filter(|r| r.end == i) { r.end += 1; }
        else { ranges.push(i..i + 1); }
    }
    encode(blob, Body::Want(Ranges::Local(&ranges)))
}

struct Fetch {
    size: u64,
    // Store's verified gaps, in chunks. Taking one index splits at most one range.
    absent: BTreeMap<u64, u64>,
    have: u64,
    source: Option<PublicKey>,
    silent: BTreeSet<PublicKey>,
    deadline: Option<u64>,
    // Wire credits move from here to the carrier's delivery queue, then to writing.
    asked: BTreeSet<u64>,
    // Fresh indices in the replacement WANT, while replies to its predecessor can cross.
    replacement: BTreeSet<u64>,
    reissue: bool,
    batch: usize,
    written_in_batch: usize,
    waiting: bool,
    progress_from: Option<PublicKey>,
    last_progress: Option<u64>,
}

impl Fetch {
    fn new(size: u64, chunk: usize, mut absent: Vec<Range<u64>>) -> Self {
        let n = size.div_ceil(chunk as u64).max(1);
        absent.sort_unstable_by_key(|r| r.start);
        let mut gaps = BTreeMap::new();
        let mut have = size;
        let mut end = 0;
        for r in absent {
            let start = r.start.max(end);
            end = end.max(r.end.min(n));
            if start < end {
                gaps.insert(start, end);
                let offset = |i: u64| i.saturating_mul(chunk as u64).min(size);
                have -= offset(end) - offset(start);
            }
        }
        Self { size, absent: gaps, have, source: None, silent: BTreeSet::new(), deadline: None, asked: BTreeSet::new(), replacement: BTreeSet::new(), reissue: false, batch: 0,
            written_in_batch: 0, waiting: false, progress_from: None, last_progress: None }
    }

    fn accept_chunk(&mut self, index: u64) -> bool {
        if !self.asked.remove(&index) { return false; }
        if self.replacement.remove(&index) {
            // Blob messages arrive in order. The first fresh reply proves the server has
            // retired the previous WANT. Its remaining indices are still gaps on disk.
            self.batch -= self.asked.len() - self.replacement.len();
            self.asked = std::mem::take(&mut self.replacement);
        }
        true
    }

    fn mark_written(&mut self, index: u64, chunk: usize) -> bool {
        let Some((&start, &end)) = self.absent.range(..=index).next_back() else { return false };
        if index >= end { return false; }
        self.absent.remove(&start);
        if start < index { self.absent.insert(start, index); }
        if index + 1 < end { self.absent.insert(index + 1, end); }
        self.have += (self.size - index * chunk as u64).min(chunk as u64);
        true
    }
}

pub struct Lane {
    me: PublicKey,
    chunk: usize,
    now: u64,
    carriers: BTreeMap<PublicKey, Carrier>,
    complete: HashSet<Blob>,
    fetches: BTreeMap<Blob, Fetch>,
    pending: HashMap<(Blob, u64), PublicKey>,
    progress: BTreeSet<Blob>,
    status: VecDeque<Status>,
    store_turn: Option<PublicKey>,
}

impl Lane {
    /// chunk must match the town store. Only the local application selects it.
    pub fn new(me: PublicKey, chunk: usize) -> Self {
        assert!((1..=CHUNK).contains(&chunk));
        Self { me, chunk, now: 0, carriers: BTreeMap::new(), complete: HashSet::new(),
            fetches: BTreeMap::new(), pending: HashMap::new(), progress: BTreeSet::new(),
            status: VecDeque::new(), store_turn: None }
    }

    /// Advance the clock before delivering store results or polling for delayed progress.
    pub fn tick(&mut self, now: u64) {
        self.now = self.now.max(now);
        for (&blob, f) in &mut self.fetches {
            if f.deadline.is_some_and(|due| due <= self.now) {
                f.deadline = None;
                if let Some(peer) = f.source { f.silent.insert(peer); }
                Self::select(&self.me, &mut self.carriers, blob, f, &mut self.status);
            }
        }
    }

    pub fn next_wake(&self) -> Option<u64> {
        self.fetches.values().filter_map(|f| f.deadline).min()
    }

    pub fn source(&self, blob: Blob) -> Option<PublicKey> {
        self.fetches.get(&blob)?.source
    }

    pub fn drop_carrier(&mut self, carrier: PublicKey) {
        let Some(peer) = self.carriers.remove(&carrier) else { return };
        for d in peer.deliveries { self.pending.remove(&(d.blob, d.index)); }
        // A dispatched write still owns its index until the old attempt's result arrives.
        for (&blob, f) in &mut self.fetches {
            f.silent.remove(&carrier);
            Self::select(&self.me, &mut self.carriers, blob, f, &mut self.status);
        }
    }


    pub fn set_view(&mut self, carrier: PublicKey, town: TownId, view: TownView) {
        if !view.open { self.drop_view(carrier, town); return; }
        let p = self.carriers.entry(carrier).or_default();
        // A larger set necessarily gained an id. A replacement can also gain one while
        // shrinking or keeping its length. Only compare the peer's two visibility maps.
        let announce = p.views.get(&town).is_none_or(|old|
            view.referenced.len() > old.referenced.len()
                || view.referenced.keys().any(|id| !old.referenced.contains_key(id)));
        if announce { p.announce.insert(town); }
        p.views.insert(town, view);
        self.view_changed(carrier, town, announce);
    }

    pub fn drop_view(&mut self, carrier: PublicKey, town: TownId) {
        if let Some(peer) = self.carriers.get_mut(&carrier) {
            peer.views.remove(&town);
            peer.announce.remove(&town);
            self.view_changed(carrier, town, false);
        }
    }

    fn view_changed(&mut self, carrier: PublicKey, town: TownId, announce: bool) {
        let Some(peer) = self.carriers.get_mut(&carrier) else { return };
        let view = peer.views.get(&town);
        let allows = |blob: &Blob| blob.0 != town || view.is_some_and(|v| v.referenced.contains_key(&blob.1));
        peer.awaiting_have.retain(&allows);
        peer.have.retain(&allows);
        peer.availability.retain(|blob, _| allows(blob));
        peer.slots.retain_mut(|s| {
            if s.blob.0 != town { return true; }
            if view.is_none() { return false; }
            match &mut s.answer {
                Answer::Ask(_) => allows(&s.blob),
                Answer::Missing { referenced, before_ready } => {
                    *referenced &= allows(&s.blob);
                    *before_ready |= announce;
                    true
                }
            }
        });
        if let Some(r) = &mut peer.reading { r.valid &= allows(&r.blob); }
        if peer.ready.as_ref().is_some_and(|c| !allows(&c.blob)) { peer.ready = None; }
        for (&blob, fetch) in self.fetches.range_mut((town, [0;32])..=(town, [255;32])) {
            Self::select(&self.me, &mut self.carriers, blob, fetch, &mut self.status);
        }
    }

    fn select(me: &PublicKey, peers: &mut BTreeMap<PublicKey, Carrier>, blob: Blob, f: &mut Fetch, status: &mut VecDeque<Status>) {
        let current = f.source.filter(|key| peers.get(key).is_some_and(|p| p.eligible(blob, f.size)));
        if f.batch != 0 && current.is_some_and(|key| !f.silent.contains(&key)) { return; }
        let next = peers.iter().filter(|(_, p)| p.eligible(blob, f.size))
            .min_by_key(|(key, p)| {
                let known = p.availability.get(&blob) == Some(&Availability::Have);
                let author = p.views.get(&blob.0).is_some_and(|v| v.authored.contains(&blob.1));
                // Exhausted sources keep their place until progress or new availability.
                let rank = if known { 0 } else if author { 1 } else { 2 };
                let silent = f.silent.contains(*key);
                (silent, silent && current != Some(**key), rank,
                    super::digest(&[me, &blob.0, &blob.1, *key]))
            }).map(|(&key, _)| key);
        if next.is_some() && next == f.source { return; }
        if let Some(old) = f.source.and_then(|key| peers.get_mut(&key)) {
            old.fetches.remove(&blob);
            if old.advertised.contains_key(&blob) { old.cancel.insert(blob); }
        }
        f.asked.clear();
        f.replacement.clear();
        f.reissue = false;
        f.batch = 0;
        f.written_in_batch = 0;
        f.deadline = None;
        f.source = next;
        // HAVE can arrive before a retired WANT's cancel was pulled. The next WANT replaces
        // that same slot and its weight directly.
        if let Some(p) = f.source.and_then(|k| peers.get_mut(&k)) {
            p.fetches.insert(blob);
            p.cancel.remove(&blob);
        }
        let waiting = f.source.is_none();
        if f.waiting != waiting {
            let (town, id) = blob;
            if waiting { status.push_back(Status::Waiting { town, id }); }
            else { status.retain(|s| !matches!(s, Status::Waiting { town: r, id: i } if (*r, *i) == blob)); }
            f.waiting = waiting;
        }
    }

    pub fn is_complete(&self, town: TownId, id: Id) -> bool { self.complete.contains(&(town, id)) }

    pub fn can_fetch(&self, carrier: PublicKey, blob: Blob, size: u64) -> bool {
        self.carriers.get(&carrier).is_some_and(|p| p.eligible(blob, size))
    }

    pub fn set_complete(&mut self, town: TownId, id: Id, complete: bool) {
        let blob = (town, id);
        if complete {
            self.complete.insert(blob);
            for p in self.carriers.values_mut() {
                if p.awaiting_have.remove(&blob) && p.size(blob).is_some() { p.have.insert(blob); }
            }
        } else {
            self.complete.remove(&blob);
            for p in self.carriers.values_mut() {
                if p.have.remove(&blob) { p.awaiting_have.insert(blob); }
                p.invalidate_read(blob);
                if let Some(s) = p.slots.iter_mut().find(|s| s.blob == blob) {
                    if matches!(s.answer, Answer::Ask(_)) {
                        s.answer = Answer::Missing { referenced: true, before_ready: false };
                    }
                }
            }
        }
    }

    /// Dispatch a read for a particular carrier. poll_store can dispatch the same work
    /// across all carriers; either poll consumes the operation until read_done.
    pub fn poll_read(&mut self, carrier: PublicKey) -> Option<StoreRequest> {
        self.carriers.get_mut(&carrier)?.next_read(carrier)
    }

    pub fn read_done(&mut self, carrier: PublicKey, town: TownId, id: Id, index: u64, result: ReadResult) {
        let blob = (town, id);
        let Some(peer) = self.carriers.get_mut(&carrier) else { return };
        if !peer.reading.as_ref().is_some_and(|r| (r.blob, r.index) == (blob, index)) { return; }
        if !peer.reading.take().is_some_and(|r| r.valid) { return; }
        match result {
            ReadResult::Missing => self.set_complete(town, id, false),
            ReadResult::Chunk { proof, bytes } => peer.ready = Some(Chunk { blob, index, proof, bytes }),
        }
    }

    pub fn fetch(&mut self, town: TownId, id: Id, size: u64, absent: Vec<Range<u64>>) {
        let blob = (town, id);
        let mut f = Fetch::new(size, self.chunk, absent);
        self.progress.remove(&blob);
        if let Some(old) = self.fetches.remove(&blob) {
            f.last_progress = old.last_progress;
            if let Some(peer) = old.source.and_then(|p| self.carriers.get_mut(&p)) {
                peer.fetches.remove(&blob);
                if peer.advertised.contains_key(&blob) { peer.cancel.insert(blob); }
            }
        }
        self.status.retain(|s| !matches!(s, Status::Waiting {town:r,id:i} | Status::Done {town:r,id:i} if (*r,*i) == blob));
        if f.absent.is_empty() {
            self.done(blob);
        } else {
            Self::select(&self.me, &mut self.carriers, blob, &mut f, &mut self.status);
            self.fetches.insert(blob, f);
        }
    }

    /// Replace verified gaps after READY. An index already delivered to the store remains
    /// charged there. Unanswered indices keep their original reservation until re-advertised.
    pub fn refreshed(&mut self, carrier: PublicKey, town: TownId, id: Id, size: u64, absent: Vec<Range<u64>>) {
        let key = (town, id);
        let Some(f) = self.fetches.get_mut(&key) else { self.fetch(town, id, size, absent); return };
        if f.source != Some(carrier) { return; }
        let verified = Fetch::new(size, self.chunk, absent);
        f.absent = verified.absent;
        f.have = verified.have;
        if f.absent.is_empty() { self.done(key); return; }
        let outstanding = f.asked.len();
        f.asked.retain(|i| f.absent.range(..=*i).next_back().is_some_and(|(_, end)| i < end));
        f.batch -= outstanding - f.asked.len();
        f.replacement.clear(); // READY reissues all unanswered gaps as one WANT.
        f.reissue = !f.asked.is_empty();
        if f.asked.is_empty() { f.deadline = None; }
        Self::select(&self.me, &mut self.carriers, key, f, &mut self.status);
    }

    pub fn cancel(&mut self, town: TownId, id: Id) {
        let key = (town, id);
        self.retire(key);
        for p in self.carriers.values_mut() {
            p.deliveries.retain(|d| {
                if d.blob != key { return true; }
                self.pending.remove(&(key, d.index));
                false
            });
        }
    }

    fn done(&mut self, blob: Blob) {
        self.retire(blob);
        self.status.push_back(Status::Done { town: blob.0, id: blob.1 });
    }

    fn retire(&mut self, blob: Blob) {
        self.fetches.remove(&blob);
        self.progress.remove(&blob);
        for p in self.carriers.values_mut() {
            p.fetches.remove(&blob);
            if p.advertised.contains_key(&blob) { p.cancel.insert(blob); }
        }
        self.status.retain(|s| !matches!(s, Status::Waiting {town:r,id:i} | Status::Done {town:r,id:i} if (*r,*i) == blob));
    }

    pub fn receive(&mut self, carrier: PublicKey, message: &Message<'_>, now: u64) -> Received {
        self.now = self.now.max(now);
        let blob = (message.town, message.id);
        let Some(p) = self.carriers.get_mut(&carrier) else { return Received::Dropped };
        match message.body {
            Body::Ready => {
                if !p.views.contains_key(&message.town) { return Received::Dropped; }
                for (&(town, _), state) in &mut p.availability {
                    if town == message.town && *state == Availability::Missing { *state = Availability::RetryAfterReady; }
                }
                for (&blob, f) in self.fetches.range_mut((message.town, [0;32])..=(message.town, [255;32])) {
                    Self::select(&self.me, &mut self.carriers, blob, f, &mut self.status);
                }
                Received::Ready
            }
            Body::Want(ranges) => p.want(blob, ranges, &self.complete, self.chunk),
            Body::Chunk { index, proof, bytes } => {
                if bytes.len() > CHUNK || proof.len() > u64::BITS as usize { return Received::Dropped; }
                let Some(f) = self.fetches.get_mut(&blob) else { return Received::Dropped };
                if f.source != Some(carrier) || !f.accept_chunk(index) { return Received::Dropped; }
                f.silent.clear();
                f.deadline = (!f.asked.is_empty()).then(|| self.now.saturating_add(SOURCE_WAIT_MS));
                self.pending.insert((blob, index), carrier);
                p.deliveries.push_back(Delivery { blob, index, proof: Arc::from(proof), bytes: Arc::from(bytes) });
                Received::Store
            }
            Body::Missing | Body::Have => {
                if matches!(message.body, Body::Missing) {
                    if self.fetches.get(&blob).and_then(|f| f.source) != Some(carrier) || !p.advertised.contains_key(&blob) {
                        return Received::Dropped;
                    }
                    p.availability.insert(blob, Availability::Missing);
                } else {
                    let Some(state) = p.availability.get_mut(&blob) else { return Received::Dropped };
                    if *state == Availability::Have { return Received::Dropped; }
                    *state = Availability::Have;
                }
                if let Some(f) = self.fetches.get_mut(&blob) {
                    f.silent.remove(&carrier);
                    Self::select(&self.me, &mut self.carriers, blob, f, &mut self.status);
                }
                Received::Control
            }
        }
    }

    pub fn bad(&mut self, carrier: PublicKey, town: TownId, id: Id) {
        let blob = (town, id);
        let Some(p) = self.carriers.get_mut(&carrier) else { return };
        if let Some(job) = p.writing.filter(|(key, _)| *key == blob) {
            self.pending.remove(&job);
            p.writing = None;
        }
        p.deliveries.retain(|d| {
            if d.blob != blob { return true; }
            self.pending.remove(&(blob, d.index));
            false
        });
        if !p.bad.insert(blob) { return; }
        if p.advertised.contains_key(&blob) { p.cancel.insert(blob); }
        self.status.push_back(Status::Bad { carrier, town, id });
        if let Some(f) = self.fetches.get_mut(&blob) {
            Self::select(&self.me, &mut self.carriers, blob, f, &mut self.status);
        }
    }

    /// An operation already at the store outlived its authenticated attempt. Its disk
    /// result belongs to the file; it cannot return credit or assign blame to a new slot.
    pub fn retired_write(&mut self, town: TownId, id: Id, index: u64, verified: bool) {
        let blob = (town, id);
        self.pending.remove(&(blob, index));
        if let Some(f) = self.fetches.get_mut(&blob) {
            if verified && f.mark_written(index, self.chunk) && f.absent.is_empty() { self.done(blob); }
        }
    }

    pub fn written(&mut self, town: TownId, id: Id, index: u64) {
        let blob = (town, id);
        let Some(&carrier) = self.pending.get(&(blob, index)) else { return };
        let Some(p) = self.carriers.get_mut(&carrier) else { return };
        if p.writing != Some((blob, index)) { return; }
        p.writing = None;
        self.pending.remove(&(blob, index));
        let Some(f) = self.fetches.get_mut(&blob) else { return };
        // A refreshed absent list can already include this write in its verified total.
        // The outstanding store credit still returns to the advertised batch.
        if f.source == Some(carrier) { f.written_in_batch = f.written_in_batch.saturating_add(1); }
        if !f.mark_written(index, self.chunk) { return; }
        p.recent_writes.push_back(self.now);
        if p.recent_writes.len() > MAX_WINDOW / 2 { p.recent_writes.pop_front(); }
        f.progress_from = Some(carrier);
        self.progress.insert(blob);
        if f.absent.is_empty() { self.done(blob); }
    }

    pub fn poll_status(&mut self) -> Option<Status> {
        if let Some(s) = self.status.pop_front() { return Some(s); }
        let &blob = self.progress.iter().find(|key| self.fetches.get(*key).is_some_and(|f|
            !f.waiting && f.last_progress.is_none_or(|t| self.now.saturating_sub(t) >= 100)))?;
        self.progress.remove(&blob);
        let f = self.fetches.get_mut(&blob)?;
        f.last_progress = Some(self.now);
        Some(Status::Progress { town: blob.0, id: blob.1, have: f.have, size: f.size, from: f.progress_from.take()? })
    }

    /// Round-robin across carriers. A write remains charged while it is in the caller's
    /// bounded channel, or while the caller is retaining it for retry on a full channel.
    pub fn poll_store(&mut self) -> Option<StoreRequest> {
        use std::ops::Bound::{Excluded, Unbounded};
        let turn = self.store_turn;
        let peers = self.carriers.range((turn.map_or(Unbounded, Excluded), Unbounded))
            .chain(self.carriers.iter().take_while(|(key, _)| turn.is_some_and(|last| **key <= last)));
        let key = peers.filter(|(_, p)| (p.writing.is_none() && !p.deliveries.is_empty())
            || (p.ready.is_none() && p.reading.is_none() && p.slots.iter().any(|s| matches!(s.answer, Answer::Ask(_)))))
            .map(|(&k, _)| k).next()?;
        let p = self.carriers.get_mut(&key)?;
        self.store_turn = Some(key);
        if p.writing.is_none() {
            if let Some(d) = p.deliveries.pop_front() {
                p.writing = Some((d.blob, d.index));
                return Some(StoreRequest::Chunk { carrier: key, town: d.blob.0, id: d.blob.1,
                    index: d.index, proof: d.proof, bytes: d.bytes });
            }
        }
        p.next_read(key)
    }

    /// One control message or one verified store result for a writable connection.
    pub fn poll_send(&mut self, carrier: PublicKey) -> Option<Vec<u8>> {
        let p = self.carriers.get_mut(&carrier)?;
        let announce = p.announce.first().copied();
        // Denials already owed under this town's old view precede its READY. New WANTs
        // cannot keep that READY behind a stream of fresh denials.
        if let Some(pos) = p.slots.iter().position(|s| matches!(s.answer, Answer::Missing { before_ready, .. }
            if announce.is_none() || (before_ready && announce == Some(s.blob.0)))) {
            let blob = p.slots.get(pos)?.blob;
            let wire = encode(blob, Body::Missing)?;
            if matches!(p.slots.remove(pos)?.answer, Answer::Missing { referenced: true, .. }) {
                if self.complete.contains(&blob) { p.have.insert(blob); }
                else { p.awaiting_have.insert(blob); }
            }
            return Some(wire);
        }
        if let Some(town) = announce {
            let wire = encode((town, [0;32]), Body::Ready)?;
            p.announce.remove(&town);
            return Some(wire);
        }
        if let Some(&blob) = p.have.first() {
            let wire = encode(blob, Body::Have)?;
            p.have.remove(&blob);
            return Some(wire);
        }
        if let Some(&blob) = p.cancel.first() {
            let wire = encode(blob, Body::Want(Ranges::Local(&[])))?;
            p.advertised.remove(&blob);
            p.cancel.remove(&blob);
            return Some(wire);
        }

        for blob in &p.fetches {
            let Some(f) = self.fetches.get_mut(blob) else { continue };
            if std::mem::take(&mut f.reissue) && !f.asked.is_empty() {
                let wire = encode_want(*blob, &f.asked)?;
                p.advertised.insert(*blob, f.asked.len());
                return Some(wire);
            }
        }
        let window = p.window(self.now);
        let held = p.deliveries.len() + usize::from(p.writing.is_some());
        let mut charged = held;
        let mut fresh = 0usize;
        for f in p.fetches.iter().filter_map(|blob| self.fetches.get(blob)) {
            charged += f.asked.len();
            if f.batch == 0 { fresh += 1; }
        }
        let available = window.saturating_sub(charged);
        let reserved: usize = p.advertised.values().sum();
        let mut needs_wire_credit = false;
        if available > 0 {
            use std::ops::Bound::{Excluded, Unbounded};
            let start = p.last_want.map_or(Unbounded, Excluded);
            let candidates = p.fetches.range((start, Unbounded)).chain(p.fetches.iter()
                .take_while(|key| p.last_want.is_some_and(|last| **key <= last)));
            let mut next = None;
            for &blob in candidates {
                let Some(f) = self.fetches.get(&blob) else { continue };
                if !f.replacement.is_empty() || f.written_in_batch < f.batch.div_ceil(2) { continue; }
                let others = reserved - p.advertised.get(&blob).copied().unwrap_or(0);
                let town = window.saturating_sub(others + f.asked.len());
                if town == 0 { needs_wire_credit = true; continue; }
                let share = if f.batch == 0 { available.div_ceil(fresh.max(1)) } else { available };
                let limit = town.min(share);
                // Stop after at most one window of new indices. A huge signed size stays a range.
                let indices: BTreeSet<_> = f.absent.iter().flat_map(|(&start, &end)| start..end)
                    .filter(|i| !f.asked.contains(i) && !self.pending.contains_key(&(blob, *i)))
                    .take(limit).collect();
                if indices.is_empty() { continue; }
                next = Some((blob, indices));
                break;
            }
            if let Some((blob, indices)) = next {
                let wire = encode_want(blob, &indices)?;
                let f = self.fetches.get_mut(&blob)?;
                if !f.asked.is_empty() { f.replacement.clone_from(&indices); }
                f.asked.extend(indices);
                if f.deadline.is_none() && !f.silent.contains(&carrier) {
                    f.deadline = Some(self.now.saturating_add(SOURCE_WAIT_MS));
                }
                f.batch = f.asked.len() + p.deliveries.iter().filter(|d| d.blob == blob).count()
                    + usize::from(p.writing.is_some_and(|(key, _)| key == blob));
                f.written_in_batch = 0;
                p.advertised.insert(blob, f.asked.len());
                p.last_want = Some(blob);
                return Some(wire);
            }
        }
        // All responses to this id arrived, but its last replacement may still be travelling
        // toward the server. Release that reservation in stream order. Store writes keep their
        // own credit. A later WANT can use the slot once the half-window is written.
        if let Some(&blob) = p.advertised.keys().find(|key| needs_wire_credit
            && self.fetches.get(*key).is_none_or(|f| f.asked.is_empty() || f.source != Some(carrier))) {
            let wire = encode(blob, Body::Want(Ranges::Local(&[])))?;
            p.advertised.remove(&blob);
            return Some(wire);
        }
        p.send_chunk()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: PublicKey = [1; 65];
    const C: PublicKey = [2; 65];
    const R: TownId = [3; 32];
    const S: TownId = [4; 32];
    const X: Id = [5; 32];
    const Y: Id = [6; 32];
    const Z: Id = [7; 32];

    fn install(lane: &mut Lane, peer: PublicKey, town: TownId, view: TownView) {
        lane.set_view(peer, town, view);
        assert_eq!(lane.poll_send(peer), encode((town,[0;32]), Body::Ready));
    }

    fn view(entries: &[(Id, u64)]) -> TownView {
        TownView { open: true, referenced: entries.iter().copied().collect(), authored: HashSet::new() }
    }

    fn input(lane: &mut Lane, peer: PublicKey, (town, id): Blob, body: Body<'_>) -> Received {
        lane.receive(peer, &Message { town, id, body }, lane.now)
    }

    fn want(lane: &mut Lane, peer: PublicKey, blob: Blob, ranges: &[Range<u64>]) -> Received {
        input(lane, peer, blob, Body::Want(Ranges::Local(ranges)))
    }

    fn wanted(wire: &[u8]) -> (Blob, Vec<Range<u64>>) {
        let m = Message::decode(wire).unwrap();
        let Body::Want(r) = m.body else { panic!("expected WANT") };
        ((m.town, m.id), r.iter().collect())
    }

    fn answer(lane: &mut Lane, peer: PublicKey, (town, id): Blob, index: u64) {
        assert!(matches!(lane.poll_read(peer), Some(StoreRequest::Read { carrier, town:r, id:i, index:n })
            if (carrier,r,i,n) == (peer,town,id,index)));
        lane.read_done(peer, town, id, index, ReadResult::Chunk { proof: vec![], bytes: vec![1] });
    }

    #[test]
    fn ready_crossing_a_chunk_reissues_only_unanswered_credit() {
        let mut lane = Lane::new([0; 65], 1);
        install(&mut lane, A, R, view(&[(X, 6)]));
        lane.fetch(R, X, 6, vec![0..6]);
        assert_eq!(wanted(&lane.poll_send(A).unwrap()).1, [0..4]);
        let reply = Body::Chunk { index: 0, proof: &[], bytes: &[1] };
        assert_eq!(input(&mut lane, A, (R, [0; 32]), Body::Ready), Received::Ready);
        // The store sampled absent before this response crossed the READY.
        assert_eq!(input(&mut lane, A, (R, X), reply), Received::Store);
        assert!(matches!(lane.poll_store(), Some(StoreRequest::Chunk { index: 0, .. })));
        lane.refreshed(A, R, X, 6, vec![0..6]);
        assert_eq!(wanted(&lane.poll_send(A).unwrap()).1, [1..4]);
        assert_eq!(lane.carriers[&A].advertised[&(R,X)] + lane.pending.len(), 4);
        assert_eq!(input(&mut lane, A, (R, X), reply), Received::Dropped);
        lane.written(R, X, 0);
        assert_eq!(input(&mut lane, A, (R, X), reply), Received::Dropped);
        assert!(lane.poll_store().is_none() && lane.poll_send(A).is_none());
        // Index 1 was already on the wire when the replacement WANT went out. Even if the
        // server answers both WANTs, exactly one response reaches the write port.
        let crossing = Body::Chunk { index: 1, proof: &[], bytes: &[2] };
        assert_eq!(input(&mut lane, A, (R, X), crossing), Received::Store);
        assert!(matches!(lane.poll_store(), Some(StoreRequest::Chunk { index: 1, .. })));
        lane.written(R, X, 1);
        assert_eq!(input(&mut lane, A, (R, X), crossing), Received::Dropped);
        assert!(lane.poll_store().is_none());
        assert_eq!(wanted(&lane.poll_send(A).unwrap()).1, [4..6], "a normal refill grants only fresh indices");
        lane.set_view(A, R, view(&[(X, 6)]));
        assert!(lane.poll_send(A).is_none(), "an unchanged visibility set does not announce READY");

        // Chunk 2 crossed the refill. Only a reply to the new WANT proves that
        // chunk 3, still unanswered, needs to be requested again.
        let write = |lane: &mut Lane, index| {
            let chunk = Body::Chunk { index, proof: &[], bytes: &[7] };
            assert_eq!(input(lane, A, (R, X), chunk), Received::Store);
            assert!(matches!(lane.poll_store(), Some(StoreRequest::Chunk { index: i, .. }) if i == index));
            lane.written(R, X, index);
        };
        write(&mut lane, 2);
        assert!(lane.poll_send(A).is_none(), "a crossing reply cannot release the old gaps");
        write(&mut lane, 4);
        assert_eq!(wanted(&lane.poll_send(A).unwrap()).1, [3..4], "only the unanswered gap is granted again");
        write(&mut lane, 5);
        write(&mut lane, 3);
        assert_eq!(lane.poll_status(), Some(Status::Done { town: R, id: X }));
    }

    #[test]
    fn reference_burst_coalesces_per_peer_and_orders_only_older_denials_first() {
        let mut lane = Lane::new([0; 65], 1);
        let entries: Vec<_> = (0..200u8).map(|i| ([i; 32], 1)).collect();
        let peers: Vec<_> = (0..50u8).map(|i| [i; 65]).collect();
        let early = entries[0].0;
        let late = [254; 32];
        for &peer in &peers {
            install(&mut lane, peer, R, view(&[]));
            want(&mut lane, peer, (R, early), &[0..1]);
            lane.set_view(peer, R, view(&entries[..100]));
            lane.set_view(peer, R, view(&entries));
            want(&mut lane, peer, (R, late), &[0..1]);
        }
        for &peer in &peers {
            assert_eq!(lane.poll_send(peer), encode((R, early), Body::Missing));
            assert_eq!(lane.poll_send(peer), encode((R, [0; 32]), Body::Ready));
            assert_eq!(lane.poll_send(peer), encode((R, late), Body::Missing));
            assert!(lane.poll_send(peer).is_none(), "one READY for the entire burst, even with no holdings");
        }
        assert!(lane.poll_store().is_none());

        // Replacement can gain an id while losing more. A pure withdrawal is silent.
        install(&mut lane, A, R, view(&[(late, 1)]));
        lane.set_view(A, R, view(&[]));
        assert!(lane.poll_send(A).is_none());
        lane.set_view(A, R, view(&[(early, 1)]));
        lane.drop_view(A, R);
        assert!(lane.poll_send(A).is_none(), "closing also withdraws the pending READY");
    }

    #[test]
    fn town_holdings_and_closed_views_disclose_nothing() {
        let mut lane = Lane::new([0; 65], 1);
        install(&mut lane, A, R, view(&[]));
        install(&mut lane, A, S, view(&[(X, 1)]));
        lane.set_complete(S, X, true);
        want(&mut lane, A, (R,X), &[0..1]);
        let other_town = lane.poll_send(A).unwrap();
        lane.set_complete(S, X, false);
        want(&mut lane, A, (R,X), &[0..1]);
        assert_eq!(lane.poll_send(A).unwrap(), other_town, "identical query after removing the other holding");
        for id in [X,Y] {
            want(&mut lane, A, (R,id), &[0..1]);
            assert_eq!(lane.poll_send(A).unwrap(), encode((R,id), Body::Missing).unwrap(), "only the echoed id varies");
            lane.set_complete(R, id, true);
        }
        assert!(lane.poll_store().is_none() && lane.poll_send(A).is_none());
        assert!(lane.carriers[&A].awaiting_have.is_empty());
        want(&mut lane, A, (R,X), &[1..4]);
        let unknown = lane.poll_send(A).unwrap();
        install(&mut lane, A, R, view(&[(X, 1)]));
        want(&mut lane, A, (R,X), &[1..4]);
        assert_eq!(lane.carriers[&A].slots.front().unwrap().weight, 3);
        assert!(lane.poll_read(A).is_none());
        assert_eq!(lane.poll_send(A).unwrap(), unknown, "past-end and unknown answer the same WANT identically");
        lane.set_complete(R, X, false);
        lane.set_complete(R, X, true);
        assert!(lane.poll_send(A).is_none(), "an out-of-range ask earns no HAVE");
        // All three denial reasons arrive from the fold as the same closed view.
        for _ in ["not active", "paused", "left"] {
            lane.set_view(A, R, TownView { open: false, ..view(&[(X, 1)]) });
            assert_eq!(want(&mut lane, A, (R,X), &[0..1]), Received::Dropped);
            assert!(lane.poll_send(A).is_none() && lane.poll_store().is_none());
            assert!(lane.carriers[&A].slots.is_empty() && lane.carriers[&A].awaiting_have.is_empty());
        }
        assert_eq!(want(&mut lane, C, (R,X), &[0..1]), Received::Dropped);
        assert!(!lane.carriers.contains_key(&C), "unsolicited carrier created no state");
    }

    #[test]
    fn replacement_rotation_cancel_and_revocation_cover_reads_in_flight() {
        let mut lane = Lane::new([0; 65], 2);
        install(&mut lane, A, R, view(&[(X,5), (Y,2)]));
        for id in [X,Y] { lane.set_complete(R, id, true); }
        for _ in 0..100 { want(&mut lane, A, (R,X), &[1..7]); }
        assert_eq!(lane.carriers[&A].slots.len(), 1);
        assert_eq!(lane.carriers[&A].slots[0].weight, 6, "admission used the unclipped range");
        want(&mut lane, A, (R,Y), &[0..1]);
        answer(&mut lane, A, (R,X), 1);
        assert!(lane.poll_read(A).is_none(), "one buffered chunk prevents read-ahead");
        let wire = lane.poll_send(A).unwrap();
        assert!(matches!(Message::decode(&wire).unwrap().body, Body::Chunk { index: 1,.. }));
        assert_eq!(lane.carriers[&A].slots.iter().find(|s| s.blob == (R,X)).unwrap().weight, 5);
        answer(&mut lane, A, (R,Y), 0);
        // Control always precedes an already buffered chunk.
        want(&mut lane, A, (R,Z), &[0..1]);
        assert_eq!(lane.poll_send(A).unwrap(), encode((R,Z), Body::Missing).unwrap());
        assert_eq!(Message::decode(&lane.poll_send(A).unwrap()).unwrap().id, Y);
        answer(&mut lane, A, (R,X), 2);
        lane.poll_send(A).unwrap();
        assert!(lane.carriers[&A].slots.is_empty(), "last clipped chunk freed the unused weight");

        want(&mut lane, A, (R,X), &[0..1]);
        assert!(lane.poll_read(A).is_some());
        lane.read_done(A, R, X, 0, ReadResult::Chunk { proof: vec![[0;32]; 65], bytes: vec![1] });
        assert!(lane.poll_send(A).is_none(), "unencodable result is discarded");
        answer(&mut lane, A, (R,X), 0);
        assert!(lane.poll_send(A).is_some(), "the slot can read and send that chunk again");

        for close in [false,true] {
            lane.set_view(A, R, view(&[(X,5)]));
            want(&mut lane, A, (R,X), &[0..1]);
            assert!(matches!(lane.poll_store(), Some(StoreRequest::Read { index:0, .. })));
            if close { lane.drop_view(A,R); } else { install(&mut lane, A,R,view(&[(Y,2)])); }
            assert!(lane.poll_send(A).is_none());
            install(&mut lane, A,R,view(&[(X,5)]));
            want(&mut lane, A, (R,X), &[0..1]);
            assert!(lane.poll_store().is_none(), "stale outstanding read still owns the worker slot");
            lane.read_done(A,R,X,0,ReadResult::Chunk { proof: vec![], bytes: vec![99] });
            assert!(lane.poll_send(A).is_none(), "reopened view cannot revive a stale callback");
            answer(&mut lane, A, (R,X), 0);
            if close { lane.drop_view(A,R); } else { want(&mut lane, A, (R,X), &[]); }
            assert!(lane.poll_send(A).is_none() && lane.poll_store().is_none(), "also revoke buffered bytes");
        }
    }

    #[test]
    fn missing_have_and_bad_drive_failover_without_a_timer() {
        let mut asker = Lane::new([0; 65], 1);
        let mut server = Lane::new([0; 65], 1);
        for lane in [&mut asker, &mut server] { install(lane, A,R,view(&[(X,12)])); }
        asker.fetch(R,X,12,vec![0..12]);
        let request = asker.poll_send(A).unwrap();
        assert_eq!(wanted(&request), ((R,X),vec![0..4]));
        server.receive(A,&Message::decode(&request).unwrap(),0);
        let missing = server.poll_send(A).unwrap();
        assert_eq!(missing, encode((R,X),Body::Missing).unwrap());
        asker.receive(A,&Message::decode(&missing).unwrap(),0);
        assert_eq!(asker.poll_status(),Some(Status::Waiting {town:R,id:X}));
        assert!(server.poll_store().is_none());
        server.set_complete(S,X,true);
        assert!(server.poll_send(A).is_none(), "no cross-town HAVE");
        server.set_complete(R,X,true);
        let have = server.poll_send(A).unwrap();
        assert_eq!(have, encode((R,X),Body::Have).unwrap());
        asker.receive(A,&Message::decode(&have).unwrap(),0);
        assert_eq!(asker.poll_send(A).unwrap(), request, "immediate re-ask on HAVE");

        install(&mut asker, C,R,view(&[(X,12)]));
        for index in [0,1] {
            assert_eq!(input(&mut asker,A,(R,X),Body::Chunk {index,proof:&[],bytes:&[9]}), Received::Store);
        }
        assert!(matches!(asker.poll_store(), Some(StoreRequest::Chunk {carrier:A,index:0,..})));
        asker.bad(A,R,X);
        assert_eq!(asker.poll_status(),Some(Status::Bad {carrier:A,town:R,id:X}));
        assert_eq!(wanted(&asker.poll_send(A).unwrap()), ((R,X),vec![]));
        assert_eq!(wanted(&asker.poll_send(C).unwrap()), ((R,X),vec![0..4]), "all unwritten indices move to C");
        assert!(asker.poll_store().is_none(), "bad carrier's queued deliveries retired");
        assert_eq!(input(&mut asker,A,(R,X),Body::Chunk {index:1,proof:&[],bytes:&[9]}),Received::Dropped);
        input(&mut asker,A,(R,X),Body::Have);
        asker.drop_view(A,R);
        install(&mut asker, A,R,view(&[(X,12)]));
        asker.fetch(R,X,12,vec![0..12]);
        assert!(asker.poll_send(A).is_none(), "refresh and HAVE cannot rehabilitate this carrier");
        // Clear rule 4 for all serving views on a failed read, remembering the qualified denial.
        server.receive(A,&Message::decode(&request).unwrap(),0);
        assert!(matches!(server.poll_read(A),Some(StoreRequest::Read {..})));
        server.read_done(A,R,X,0,ReadResult::Missing);
        assert_eq!(server.poll_send(A).unwrap(), missing);
        server.set_view(A,R,view(&[]));
        server.set_complete(R,X,true);
        assert!(server.poll_send(A).is_none(), "retracted reference revoked HAVE memory");
    }

    #[test]
    fn ranked_sources_rotate_on_silence_and_keep_the_last_grant_open() {
        const BEN: PublicKey = [6; 65];
        let town = || view(&[(X,12)]);
        let ranked = |me| {
            let mut lane = Lane::new(me, 1);
            for peer in [A,C,BEN] { install(&mut lane, peer, R, town()); }
            lane.fetch(R,X,12,vec![0..12]);
            lane
        };
        let mut lane = ranked([8;65]);
        assert_ne!(lane.source((R,X)), ranked([12;65]).source((R,X)), "requesters spread across holders");
        assert_eq!(input(&mut lane,A,(R,X),Body::Have), Received::Dropped, "unsolicited HAVE gets no preference");
        let mut author = town();
        author.authored.insert(X);
        lane.set_view(BEN,R,author);
        assert_eq!(lane.source((R,X)), Some(BEN), "the author outranks the hash before the first WANT");
        assert_eq!(wanted(&lane.poll_send(BEN).unwrap()).1, [0..4]);
        input(&mut lane,BEN,(R,X),Body::Missing);
        let other = lane.source((R,X)).unwrap();
        assert_ne!(other, BEN);
        assert_eq!(wanted(&lane.poll_send(other).unwrap()).1, [0..4]);
        let chunk = |index| Body::Chunk { index, proof: &[], bytes: &[7] };
        assert_eq!(input(&mut lane,other,(R,X),chunk(0)), Received::Store);
        input(&mut lane,BEN,(R,X),Body::Have);
        assert_eq!(lane.source((R,X)), Some(other), "HAVE never interrupts progress");
        lane.cancel(R,X);
        lane.set_view(BEN,R,town());
        let mut author = town();
        author.authored.insert(X);
        lane.set_view(other,R,author);
        lane.fetch(R,X,12,vec![0..12]);
        assert_eq!(lane.source((R,X)), Some(BEN), "qualified HAVE outranks the author");

        let mut tried = BTreeSet::new();
        for turn in 1..=3 {
            let peer = lane.source((R,X)).unwrap();
            assert!(tried.insert(peer), "try every eligible holder before repeating one");
            assert_eq!(wanted(&lane.poll_send(peer).unwrap()).1, [0..4]);
            let due = turn * SOURCE_WAIT_MS;
            assert_eq!(lane.next_wake(), Some(due));
            lane.tick(due - 1);
            assert_eq!(lane.source((R,X)), Some(peer));
            lane.tick(due);
            if turn == 3 {
                assert_eq!(lane.source((R,X)), Some(peer), "all silent: keep this request outstanding");
                assert_eq!(lane.next_wake(), None);
                lane.tick(due + 2 * SOURCE_WAIT_MS);
                assert!(lane.poll_send(peer).is_none(), "no repeated WANT while waiting");
                for index in 0..4 {
                    assert_eq!(input(&mut lane,peer,(R,X),chunk(index)), Received::Store);
                }
                assert_eq!(lane.next_wake(), None, "a stalled store cannot time out the holder");
                lane.tick(due + 4 * SOURCE_WAIT_MS);
                assert_eq!(lane.source((R,X)), Some(peer));
            }
        }
    }

    #[test]
    fn unanswered_denials_share_the_same_bounded_window_as_reads() {
        let mut lane = Lane::new([0; 65], 1);
        install(&mut lane, A,R,view(&[(X,1),(Y,1)]));
        lane.set_complete(R,Y,true);
        for i in 0..200u8 {
            let mut id = [0;32];
            id[0] = i;
            let admitted = want(&mut lane,A,(R,id), &[0..1]);
            assert_eq!(admitted == Received::Control, usize::from(i) < MAX_WINDOW);
            assert!(lane.carriers[&A].slots.len() <= MAX_WINDOW);
        }
        for id in [X,Y] {
            assert_eq!(want(&mut lane,A,(R,id), &[0..1]), Received::Dropped);
            assert!(lane.poll_store().is_none());
        }
        let old = [0;32];
        assert_eq!(want(&mut lane,A,(R,old), &[0..2]),Received::Dropped,"oversize replacement kept its old slot");
        assert!(lane.carriers[&A].awaiting_have.is_empty());
        for i in 0..MAX_WINDOW {
            let mut id = [0;32]; id[0] = i as u8;
            assert_eq!(lane.poll_send(A).unwrap(), encode((R,id),Body::Missing).unwrap());
        }
        assert!(lane.poll_send(A).is_none());
        lane.set_complete(R,X,true);
        assert!(lane.poll_send(A).is_none(), "dropped referenced WANT left no HAVE memory");
        assert_eq!(want(&mut lane,A,(R,Y), &[0..u64::MAX]),Received::Dropped, "weight checked before clipping");
        assert_eq!(want(&mut lane,A,(R,Y), &[0..64]),Received::Control);
        assert_eq!(want(&mut lane,A,(R,X), &[0..1]),Received::Dropped);
        answer(&mut lane,A,(R,Y),0);
        lane.poll_send(A).unwrap();
        assert_eq!(want(&mut lane,A,(R,X), &[0..1]),Received::Control, "single clipped answer released the full slot");
    }

    #[test]
    fn only_accepted_written_chunks_return_credit_and_progress_is_coalesced() {
        let mut lane = Lane::new([0; 65], 2);
        install(&mut lane, A,R,view(&[(X,u64::MAX)]));
        let n = u64::MAX.div_ceil(2);
        lane.fetch(R,X,u64::MAX,vec![0..n]);
        assert_eq!(wanted(&lane.poll_send(A).unwrap()), ((R,X),vec![0..4]));
        let chunk = |index| Body::Chunk { index, proof: &[], bytes: &[4,7] };
        assert_eq!(input(&mut lane,A,(R,X),chunk(n)),Received::Dropped);
        assert!(lane.poll_store().is_none(), "unasked index never reached the store");
        for index in 0..4 {
            assert_eq!(input(&mut lane,A,(R,X),chunk(index)),Received::Store);
            assert_eq!(input(&mut lane,A,(R,X),chunk(index)),Received::Dropped, "duplicate before write");
        }
        assert!(lane.poll_send(A).is_none(), "arrival alone cannot open the window");
        for index in 0..4 {
            assert!(matches!(lane.poll_store(),Some(StoreRequest::Chunk { index:i,.. }) if i == index));
            lane.written(R,X,index);
            if index == 0 {
                assert_eq!(lane.poll_status(),Some(Status::Progress {town:R,id:X,have:2,size:u64::MAX,from:A}));
                assert!(lane.poll_send(A).is_none(), "one write is below the half-window");
            }
            if index == 1 {
                assert_eq!(wanted(&lane.poll_send(A).unwrap()), ((R,X),vec![4..6]), "refill half a window");
            }
        }
        lane.tick(99);
        assert!(lane.poll_status().is_none());
        lane.tick(100);
        assert_eq!(lane.poll_status(),Some(Status::Progress {town:R,id:X,have:8,size:u64::MAX,from:A}));
        assert!(lane.poll_status().is_none(), "no repeated snapshot without a write");
        assert_eq!(lane.carriers[&A].window(250),4,"expired writes no longer increase W");
        lane.fetch(R,X,u64::MAX,vec![n-1..n]);
        let request = lane.poll_send(A).unwrap();
        assert_eq!(wanted(&request), ((R,X),vec![n-1..n]));
        input(&mut lane,A,(R,X),Body::Chunk {index:n-1,proof:&[],bytes:&[4]});
        assert!(matches!(lane.poll_store(),Some(StoreRequest::Chunk { .. })));
        lane.written(R,X,n-1);
        assert_eq!(lane.poll_status(),Some(Status::Done {town:R,id:X}));

        let mut refreshed = Lane::new([0; 65], 1);
        install(&mut refreshed, A, R, view(&[(X, 8)]));
        refreshed.fetch(R, X, 8, vec![0..8]);
        refreshed.poll_send(A).unwrap();
        for index in 0..3 {
            input(&mut refreshed, A, (R, X), Body::Chunk { index, proof: &[], bytes: &[0] });
        }
        refreshed.fetch(R, X, 8, vec![4..8]);
        assert_eq!(wanted(&refreshed.poll_send(A).unwrap()).1, [4..5], "three store writes still charged");
        input(&mut refreshed, A, (R, X), Body::Chunk { index: 4, proof: &[], bytes: &[0] });
        for index in [0, 1, 2, 4] {
            assert!(matches!(refreshed.poll_store(), Some(StoreRequest::Chunk {index:i, ..}) if i == index));
            refreshed.written(R, X, index);
        }
        assert_eq!(wanted(&refreshed.poll_send(A).unwrap()).1, [5..8], "refresh preserved credit and progress");
    }

    #[test]
    fn three_honest_fetches_share_credit_across_towns_with_crossing_replies() {
        let mut asker = Lane::new([0; 65], 1);
        let mut server = Lane::new([0; 65], 1);
        let blobs = [(R,X,11),(R,Y,17),(S,Z,193)];
        for lane in [&mut asker,&mut server] {
            install(lane, A,R,view(&[(X,11),(Y,17)]));
            install(lane, A,S,view(&[(Z,193)]));
        }
        for (town,id,size) in blobs {
            server.set_complete(town,id,true);
            asker.fetch(town,id,size,vec![0..size]);
        }
        let mut replies = VecDeque::new();
        let mut written = HashSet::new();
        let mut done = HashSet::new();
        let mut peak = 0;
        let mut crossing = false;
        for step in 0..2000 {
            // Many writes may finish in one millisecond. The server and its output are
            // independent of when the asker consumes those already ordered replies.
            let now = step / 8;
            asker.tick(now);
            while let Some(wire) = asker.poll_send(A) {
                let msg = Message::decode(&wire).unwrap();
                if matches!(msg.body,Body::Want(r) if r.iter().len() != 0)
                    && replies.iter().any(|wire: &Vec<u8>| {
                        let m = Message::decode(wire).unwrap();
                        (m.town,m.id) == (msg.town,msg.id)
                    }) { crossing = true; }
                assert_eq!(server.receive(A,&msg,now),Received::Control,"honest WANT at step {step}");
            }
            let peer = &asker.carriers[&A];
            let charged = peer.deliveries.len() + usize::from(peer.writing.is_some())
                + asker.fetches.values().map(|f| f.asked.len()).sum::<usize>();
            assert!(charged <= peer.window(now), "joint store and wire credit at step {step}");
            peak = peak.max(charged);
            if step == 0 {
                assert_eq!(charged,4);
                assert_eq!(server.carriers[&A].slots.len(),3,"all three share the first four credits");
            }
            if step % 9 >= 3 {
                while let Some(StoreRequest::Read {town,id,index,..}) = server.poll_store() {
                    server.read_done(A,town,id,index,ReadResult::Chunk {proof:vec![],bytes:vec![index as u8]});
                    replies.push_back(server.poll_send(A).unwrap());
                }
            }
            for _ in 0..3 {
                if let Some(wire) = replies.pop_front() {
                    assert_eq!(asker.receive(A,&Message::decode(&wire).unwrap(),now), Received::Store,
                        "every transmitted chunk still owns its store credit at step {step}");
                }
            }
            if step % 7 >= 2 {
                while let Some(StoreRequest::Chunk {town,id,index,bytes,..}) = asker.poll_store() {
                    assert_eq!(&*bytes,&[index as u8]);
                    assert!(written.insert((town,id,index)), "duplicate store delivery at step {step}");
                    asker.written(town,id,index);
                }
            }
            while let Some(s) = asker.poll_status() {
                if let Status::Done {town,id} = s { done.insert((town,id)); }
            }
            if done.len() == 3 { break; }
        }
        assert_eq!(done.len(),3,"every fetch resumed after backpressure");
        assert_eq!(written.len(),11+17+193);
        assert_eq!(peak,MAX_WINDOW,"the adaptive window reached its shared ceiling");
        assert!(crossing,"replacement WANT crossed replies for that same blob");
    }
}
