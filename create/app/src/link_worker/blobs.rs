//! One blob lane and store port per worker. Each authenticated attempt supplies its own
//! authorization view. Fetches and verified store gaps belong to the town and file.

use crate::store_worker::{LinkResult, LinkWork, StorePort, StoreWorker};
use makepad_town_core::blob::{self, lane::{Lane, Received, TownView, Status, StoreRequest}, Body, Hash, Served};
use makepad_town_core::event::{Id, TownId};
use makepad_town_core::hub::{Attempt, Carrier};
use makepad_town_core::key::PublicKey;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::{mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError}, Arc};

pub(crate) type Blob = (TownId, Id);
type Changes = BTreeMap<Blob, Option<Status>>;
type Towns = BTreeMap<TownId, Arc<Selection>>;

pub(crate) struct Selection {
    pub refs: HashMap<Id, (u64, Hash)>,
    pub authored: HashSet<Id>,
    pub small: BTreeSet<Id>,
}

#[derive(Default)]
pub(crate) struct View {
    pub towns: Towns,
    pub closed: BTreeSet<TownId>,
}

pub(crate) struct Update {
    pub views: BTreeMap<Attempt, View>,
    pub explicit: Arc<BTreeSet<Blob>>,
    pub store: Option<StorePort>,
}

pub struct Endpoint {
    pub(crate) to_link: SyncSender<Update>,
    pub(crate) statuses: Receiver<Changes>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.debug_struct("BlobEndpoint").finish_non_exhaustive() }
}

impl Endpoint {
    pub(crate) fn attach(&self, worker: &mut StoreWorker) -> StorePort {
        let wake = self.wake.clone();
        worker.attach(move || wake())
    }

    pub(crate) fn wake(&self) { (self.wake)(); }
}

pub(super) struct BlobPump {
    lane: Lane,
    control: Receiver<Update>,
    statuses: SyncSender<Changes>,
    changes: Changes,
    next_status: u64,
    store: Option<StorePort>,
    views: BTreeMap<Attempt, Towns>,
    explicit: Arc<BTreeSet<Blob>>,
    repair: BTreeSet<Blob>,
    active: BTreeSet<Blob>,
    auto: Option<Blob>,
    // A new fetch samples disk once. READY on the selected attempt can refresh its gaps.
    begin: BTreeMap<Blob, Option<Attempt>>,
    beginning: Option<Blob>,
    pending: Option<(Option<Attempt>, LinkWork)>,
    // Store replies are FIFO. Holdings are unsolicited and consume no entry.
    dispatched: VecDeque<Option<Attempt>>,
}

impl BlobPump {
    pub(super) fn new(me: PublicKey, chunk: usize, wake: impl Fn() + Send + Sync + 'static) -> (Self, Endpoint) {
        let (to_link, control) = mpsc::sync_channel(1);
        let (statuses, from_link) = mpsc::sync_channel(1);
        let end = Endpoint { to_link, statuses: from_link, wake: Arc::new(wake) };
        (Self { lane: Lane::new(me, chunk), control, statuses, changes: Changes::new(), next_status: 0,
            store: None, views: BTreeMap::new(), explicit: Arc::default(), repair: BTreeSet::new(),
            active: BTreeSet::new(), auto: None, begin: BTreeMap::new(), beginning: None,
            pending: None, dispatched: VecDeque::new() }, end)
    }

    fn attempt(&self, peer: PublicKey) -> Option<Attempt> {
        self.views.range(Attempt { carrier: Carrier::Peer(peer), generation: 0 }..)
            .next().map(|(&at, _)| at).filter(|at| at.carrier == Carrier::Peer(peer))
    }

    pub(super) fn attach(&mut self, at: Attempt) {
        let Carrier::Peer(peer) = at.carrier else { return };
        if let Some(old) = self.attempt(peer) {
            if old == at { return; }
            self.detach(old);
        }
        self.views.insert(at, Towns::new());
    }

    pub(super) fn detach(&mut self, at: Attempt) {
        if self.views.remove(&at).is_none() { return; }
        if let Carrier::Peer(peer) = at.carrier { self.lane.drop_carrier(peer); }
        self.schedule();
    }

    fn descriptor(&self, (town,id): Blob) -> Option<(u64, Hash)> {
        self.views.values().find_map(|towns| towns.get(&town)?.refs.get(&id).copied())
    }

    fn update(&mut self, update: Update) {
        if let Some(port) = update.store { self.store = Some(port); }
        for (at, view) in update.views {
            let Some(towns) = self.views.get_mut(&at) else { continue };
            let Carrier::Peer(peer) = at.carrier else { continue };
            for &town in towns.keys() {
                if view.closed.contains(&town) || !view.towns.contains_key(&town) { self.lane.drop_view(peer, town); }
            }
            for (&town, new) in &view.towns {
                if view.closed.contains(&town) || !towns.get(&town).is_some_and(|old| Arc::ptr_eq(old, new)) {
                    self.lane.set_view(peer, town, TownView { open: true, authored: new.authored.clone(),
                        referenced: new.refs.iter().map(|(&id, &(size, _))| (id, size)).collect() });
                }
            }
            *towns = view.towns;
        }
        self.explicit = update.explicit;
        self.schedule();
    }

    // One automatic fetch across all carriers. A missing small file yields to the next
    // one; its carrier still remembers MISSING, and a qualified HAVE schedules it again.
    fn schedule(&mut self) {
        let mut wanted = BTreeSet::new();
        wanted.extend(self.explicit.iter().chain(&self.repair).copied().filter(|&key|
            !self.lane.is_complete(key.0, key.1) && self.descriptor(key).is_some()));
        let mut eligible = self.views.iter().flat_map(|(at, towns)| towns.iter().flat_map(move |(&town, selection)|
            selection.small.iter().map(move |&id| (at.carrier, (town,id), selection))))
            .filter_map(|(carrier, key, selection)| {
                let Carrier::Peer(peer) = carrier else { return None };
                let &(size, _) = selection.refs.get(&key.1)?;
                (!self.lane.is_complete(key.0,key.1) && self.lane.can_fetch(peer,key,size)).then_some(key)
            });
        self.auto = self.auto.filter(|key| eligible.clone().any(|k| k == *key)).or_else(|| eligible.next());
        wanted.extend(self.auto);
        for &key in self.active.difference(&wanted) {
            self.lane.cancel(key.0,key.1);
            self.begin.remove(&key);
            if self.lane.is_complete(key.0,key.1) {
                self.changes.insert(key, Some(Status::Done { town: key.0, id: key.1 }));
            } else if self.views.values().any(|towns| towns.get(&key.0).is_some_and(|s| s.small.contains(&key.1))) {
                self.changes.insert(key, Some(Status::Waiting { town: key.0, id: key.1 }));
            } else {
                self.changes.insert(key, None);
            }
        }
        self.begin.extend(wanted.difference(&self.active).map(|&key| (key,None)));
        self.active = wanted;
    }

    pub(super) fn receive(&mut self, at: Attempt, bytes: &[u8], now: u64) {
        if !self.views.contains_key(&at) { return; }
        let Carrier::Peer(peer) = at.carrier else { return };
        let Ok(msg) = blob::Message::decode(bytes) else { return };
        match self.lane.receive(peer, &msg, now) {
            Received::Ready => {
                self.schedule();
                for &key in self.active.range((msg.town, [0; 32])..=(msg.town, [255; 32])) {
                    if self.lane.source(key) == Some(peer) { self.begin.insert(key, Some(at)); }
                }
            }
            Received::Control if matches!(msg.body, Body::Missing | Body::Have) => self.schedule(),
            _ => {}
        }
    }

    fn holding(&mut self, key: Blob, complete: bool) {
        if self.lane.is_complete(key.0, key.1) != complete {
            self.lane.set_complete(key.0, key.1, complete);
            if complete { self.repair.remove(&key); }
        }
    }

    fn result(&mut self, result: LinkResult) {
        let at = match &result {
            LinkResult::Holdings(_) => None,
            _ => self.dispatched.pop_front().expect("one reply per dispatched store operation"),
        };
        let live = at.is_some_and(|a| self.views.contains_key(&a));
        match result {
            LinkResult::Holdings(holdings) => {
                for (key, yes) in holdings {
                    self.holding(key, yes);
                    if !yes { self.repair.insert(key); }
                }
                self.schedule();
            }
            LinkResult::Begun { town, file, absent } => {
                let key = (town,file.id);
                self.beginning = None;
                if !self.active.contains(&key) { return; }
                if absent.is_empty() { self.finished(key); }
                else if self.lane.source(key).is_none() && at.is_none() {
                    self.lane.fetch(town, file.id, file.size, absent);
                } else if let Some(Attempt { carrier: Carrier::Peer(peer), .. }) = at.filter(|_| live) {
                    self.lane.refreshed(peer, town, file.id, file.size, absent);
                }
            }
            LinkResult::Read { carrier, town, id, index, result, complete } => {
                let missing = matches!(result, Served::Missing);
                if live { self.lane.read_done(carrier, town, id, index, result); }
                self.holding((town,id), complete);
                if missing && !complete {
                    self.repair.insert((town,id));
                    self.schedule();
                }
            }
            LinkResult::Written { town, id, index, complete } => {
                if live { self.lane.written(town,id,index); }
                else { self.lane.retired_write(town,id,index,true); }
                if complete { self.finished((town,id)); }
            }
            LinkResult::Bad { carrier, town, id, index } => {
                if live { self.lane.bad(carrier,town,id); }
                else { self.lane.retired_write(town,id,index,false); }
                self.schedule();
            }
        }
    }

    fn finished(&mut self, key: Blob) {
        self.holding(key,true);
        self.schedule();
    }

    pub(super) fn next_wake(&self, now: u64) -> Option<u64> {
        let status = (!self.changes.is_empty() && now < self.next_status).then_some(self.next_status);
        self.lane.next_wake().into_iter().chain(status).min()
    }

    pub(super) fn pump(&mut self, now: u64) -> bool {
        loop {
            match self.control.try_recv() {
                Ok(update) => self.update(update),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    // The model that authorized these views has gone away.
                    while let Some(&at) = self.views.keys().next() { self.detach(at); }
                    self.store = None;
                    return false;
                }
            }
        }
        self.lane.tick(now);
        while let Some(result) = self.store.as_ref().and_then(StorePort::try_recv) { self.result(result); }
        while let Some(store) = &self.store {
            let retrying = self.pending.is_some();
            let (at, work) = if let Some(pending) = self.pending.take() { pending }
            else if let Some(work) = self.lane.poll_store() {
                let carrier = match &work { StoreRequest::Chunk { carrier, .. } | StoreRequest::Read { carrier, .. } => *carrier };
                (self.attempt(carrier), LinkWork::Transfer(work))
            } else if self.beginning.is_none() {
                let Some((key, at)) = self.begin.pop_first() else { break };
                let Some((size,root)) = self.descriptor(key) else { continue };
                self.beginning = Some(key);
                (at, LinkWork::Begin { town: key.0, size, root })
            } else { break };
            match store.try_send(work) {
                Ok(()) => self.dispatched.push_back(at),
                Err(TrySendError::Full(work)) => {
                    if !retrying { eprintln!("town: blob store port full; retaining the operation"); }
                    self.pending = Some((at, work));
                    break;
                }
                Err(TrySendError::Disconnected(_)) => { self.store = None; break; }
            }
        }
        while let Some(status) = self.lane.poll_status() {
            let key = match &status {
                Status::Waiting {town,id} | Status::Progress {town,id,..} | Status::Done {town,id} => (*town,*id),
                Status::Bad { carrier, town, id } => {
                    eprintln!("town: invalid blob proof from {} in town {} for {}", makepad_town_core::hex(carrier),
                        makepad_town_core::hex(town), makepad_town_core::hex(id));
                    (*town,*id)
                }
            };
            self.changes.insert(key,Some(status));
        }
        if now >= self.next_status && !self.changes.is_empty() {
            match self.statuses.try_send(std::mem::take(&mut self.changes)) {
                Ok(()) => { self.next_status = now.saturating_add(100); return true; }
                Err(TrySendError::Full(saved)) => self.changes = saved,
                Err(TrySendError::Disconnected(_)) => {}
            }
        }
        false
    }

    pub(super) fn poll_send(&mut self, at: Attempt) -> Option<Vec<u8>> {
        if !self.views.contains_key(&at) { return None; }
        let Carrier::Peer(peer) = at.carrier else { return None };
        self.lane.poll_send(peer)
    }
}
