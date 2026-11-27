//! The UI owns the fold and its blob references. Only a changed town, fold, or eligibility
//! rebuilds the carrier's visibility map. Page requests reuse that map.

use super::*;
use crate::link_worker::blobs::{Blob, Endpoint, Selection, Update, View};
use crate::store_worker::StorePort;
use makepad_town_core::hub::{Attempt, Carrier};
use makepad_town_core::blob::lane::Status;
use std::sync::mpsc::TrySendError;

const AT_ONCE: u64 = 1024 * 1024;

#[derive(Default)]
pub(super) struct BlobViews {
    attachments: BTreeMap<PublicKey, Attachment>,
    worker: Option<Publisher>,
    withdrawn: BTreeSet<Attempt>,
    status: BTreeMap<Blob, Status>,
    explicit: Arc<BTreeSet<Blob>>,
    changed_status: BTreeSet<Blob>,
    clip_wants: BTreeSet<Blob>,
}

struct Publisher {
    endpoint: Endpoint,
    port: Option<StorePort>,
    explicit: Option<Arc<BTreeSet<Blob>>>,
    full: bool,
}

struct Attachment {
    at: Attempt,
    eligibility: Option<(u64, bool)>,
    towns: BTreeMap<TownId, Arc<Selection>>,
    revisions: BTreeMap<TownId, usize>,
    closed: BTreeSet<TownId>,
    dirty: bool,
}

impl Model {
    pub fn attach_blobs(&mut self, endpoint: Endpoint) {
        let Some(worker) = &mut self.files.worker else { return };
        let port = endpoint.attach(worker);
        worker.dispatch();
        self.bind_blobs(endpoint, port);
    }

    pub(crate) fn bind_blobs(&mut self, endpoint: Endpoint, port: StorePort) {
        let views = &mut self.files.views;
        views.attachments.clear();
        views.withdrawn.clear();
        views.changed_status.extend(std::mem::take(&mut views.status).into_keys());
        views.worker = Some(Publisher { endpoint, port: Some(port), explicit: None, full: false });
    }

    pub fn attach_blob_view(&mut self, at: Attempt) {
        let Carrier::Peer(peer) = at.carrier else { return };
        if self.files.views.worker.is_none() || !self.sessions.contains_key(&peer) { return; }
        self.detach_peer_blobs(peer);
        self.files.views.attachments.insert(peer, Attachment { at, eligibility: None, towns: BTreeMap::new(),
            revisions: BTreeMap::new(), closed: BTreeSet::new(), dirty: true });
    }

    pub(super) fn detach_peer_blobs(&mut self, peer: PublicKey) {
        let views = &mut self.files.views;
        if let Some(a) = views.attachments.remove(&peer) { views.withdrawn.insert(a.at); }
    }

    /// Keep this reference wanted while its page is open, across temporary disconnections.
    pub fn want_file(&mut self, town: TownId, id: Id) -> Result<(), String> {
        self.in_town(town, |r, key| {
            if r.state.active_member(&key.public()) && r.references.visible(&id, &r.state, None) { Ok(()) }
            else { Err("This file is unavailable in this town.".into()) }
        })?;
        Arc::make_mut(&mut self.files.views.explicit).insert((town,id));
        Ok(())
    }

    pub fn release_file(&mut self, town: TownId, id: Id) {
        Arc::make_mut(&mut self.files.views.explicit).remove(&(town,id));
    }

    pub fn file_transfer<'a>(&'a self, town: TownId, id: Id) -> Option<&'a Status> {
        self.files.views.status.get(&(town,id))
    }

    pub fn file_wanted(&self, town: TownId, id: Id) -> bool {
        self.files.views.explicit.contains(&(town, id))
    }

    pub fn get_clip(&mut self, town: TownId, id: Id) -> Result<(), String> {
        if self.complete_blobs(&town).is_some_and(|held| held.contains(&id)) { return Ok(()) }
        self.want_file(town, id)?;
        let views = &mut self.files.views;
        views.clip_wants.insert((town, id));
        if matches!(views.status.get(&(town,id)), Some(Status::Bad { .. })) {
            views.status.remove(&(town,id));
        }
        Ok(())
    }

    /// Called after a store snapshot, including holdings for background towns.
    pub fn release_complete_clips(&mut self) {
        let done: Vec<_> = self.files.views.clip_wants.iter().copied().filter(|(town, id)|
            self.complete_blobs(town).is_some_and(|held| held.contains(id))).collect();
        for (town, id) in done {
            self.release_file(town, id);
            self.files.views.clip_wants.remove(&(town, id));
        }
    }

    pub fn take_transfer_changes(&mut self) -> BTreeSet<(TownId, Id)> {
        std::mem::take(&mut self.files.views.changed_status)
    }

    /// False when the link command is retained for the next frame. The store can be stalled
    /// independently; neither the authorization map nor this mailbox reads it.
    pub fn publish_blobs(&mut self) -> bool { self.publish_blob_policy(AT_ONCE) }

    pub(crate) fn publish_blob_policy(&mut self, at_once: u64) -> bool {
        let me = self.me();
        let views = &mut self.files.views;
        let Some(worker) = &mut views.worker else { return true };
        if let Ok(updates) = worker.endpoint.statuses.try_recv() {
            for (key, status) in updates {
                if views.status.get(&key) == status.as_ref() { continue; }
                views.changed_status.insert(key);
                if let Some(status) = status { views.status.insert(key,status); } else { views.status.remove(&key); }
            }
        }
        let mut changed = !views.withdrawn.is_empty();
        for (&peer, a) in &mut views.attachments {
            let eligible = match a.eligibility {
                Some((revision, eligible)) if revision == self.carrier_revision => eligible,
                _ => {
                    let eligible = self.peers.as_ref().is_ok_and(|p| p.device(&peer)
                        .is_some_and(|d| d.state == DeviceState::Active && !d.paused));
                    a.eligibility = Some((self.carrier_revision, eligible));
                    eligible
                }
            };
            a.revisions.retain(|&id, _| {
                let town = if self.open.log.town_id() == id { Some(&self.open) } else { self.background.get(&id) };
                if eligible && self.engines.contains_key(&(peer, id)) && town.is_some_and(|r|
                    r.state.active_member(&me) && r.state.active_member(&peer)) { return true }
                a.closed.insert(id);
                a.towns.remove(&id);
                a.dirty = true;
                false
            });
            for (&(_, id), _) in self.engines.range((peer, [0; 32])..=(peer, [255; 32])) {
                let r = if self.open.log.town_id() == id { &self.open } else { &self.background[&id] };
                if !eligible || !r.state.active_member(&me) || !r.state.active_member(&peer) { continue }
                let revision = r.log.events().len();
                if a.revisions.get(&id) == Some(&revision) { continue }
                let end = makepad_town_core::sync::send_end(&r.log, &r.state, &me, &peer);
                let refs: std::collections::HashMap<_, _> = r.references.servable(&r.state, end).into_keys()
                    .filter_map(|id| Some((id, r.references.descriptor(&id)?))).collect();
                let mut small = BTreeSet::new();
                let mut authored = HashSet::new();
                for entity in r.state.entities.values() {
                    for field in entity.fields.values() {
                        if let FieldValue::Blob { size, root } = &field.value {
                            let id = blob::id(*size, root);
                            if !refs.contains_key(&id) { continue; }
                            if entity.author == peer { authored.insert(id); }
                            if *size <= at_once && !entity.flag(field::DELETED) { small.insert(id); }
                        }
                    }
                }
                a.towns.insert(id, Arc::new(Selection { refs, small, authored }));
                a.revisions.insert(id, revision);
                a.dirty = true;
            }
            changed |= a.dirty;
        }
        if !changed && worker.explicit.as_ref().is_some_and(|e| Arc::ptr_eq(e, &views.explicit)) { return true }
        let mut updates: BTreeMap<_,_> = views.withdrawn.iter().map(|&at| (at, View::default())).collect();
        for a in views.attachments.values().filter(|a| a.dirty) {
            updates.insert(a.at, View { towns: a.towns.clone(), closed: a.closed.clone() });
        }
        let update = Update { views: updates, explicit: views.explicit.clone(), store: worker.port.take() };
        match worker.endpoint.to_link.try_send(update) {
            Ok(()) => {
                worker.endpoint.wake();
                worker.full = false;
                worker.explicit = Some(views.explicit.clone());
                views.withdrawn.clear();
                for a in views.attachments.values_mut() { a.closed.clear(); a.dirty = false; }
                true
            }
            Err(TrySendError::Full(update)) => {
                worker.port = update.store;
                if !worker.full { eprintln!("town: blob view queue full; retrying on the next frame"); }
                worker.full = true;
                false
            }
            Err(TrySendError::Disconnected(_)) => {
                views.worker = None;
                views.changed_status.extend(std::mem::take(&mut views.status).into_keys());
                true
            }
        }
    }

    pub fn blob_policy_ready(&self, peer: PublicKey) -> bool {
        self.files.views.worker.is_none() || self.files.views.attachments.get(&peer).is_none_or(|a| !a.dirty)
    }
}
