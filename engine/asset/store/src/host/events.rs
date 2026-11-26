//! Bounded catalog event journal + long-poll wakeup hub.
//!
//! One in-memory, monotonic journal of committed catalog changes: asset
//! publish/quarantine, alias set/clear, annotation set/clear, and the game
//! equivalents. Routes append AFTER the core mutation has committed — and
//! only from inside the state-thread closure that performed it, so journal
//! order equals commit order by construction (the state thread serializes
//! every mutation).
//!
//! Consumers (games, the VJ app, the asset store) long-poll
//! `GET /v1/events` with a resume cursor. Everything is bounded:
//!
//! - the journal is a ring of at most `cap` events; readers whose cursor
//!   points before the retained window get `gap: true` and must resync
//!   their catalog view (never a silent hole),
//! - the cursor carries a per-process random epoch, so a restarted server
//!   (fresh, empty journal) can never alias a stale cursor onto new
//!   sequence numbers — an epoch mismatch is a `gap` too,
//! - waiting is condvar-sliced (250 ms) against a wall-clock deadline and a
//!   shutdown flag, so shutdown never blocks behind a parked long-poll,
//! - concurrent waiters are capped; over the cap a poll degrades to an
//!   immediate empty response (the client just polls again) instead of
//!   parking one more thread.
//!
//! The journal mutex protects accounting, preview selection and appends. It
//! is never held across I/O or a core catalog transaction's wait path; the
//! condvar releases it while a subscriber waits.

use super::draft_spool::DraftFile;
use super::api::principal_str;
use super::config::{PreviewBudget, ServerConfig};
use crate::PrincipalId;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Closed event-kind vocabulary. Clients refuse unknown kinds rather than
/// misrender them, so an addition is a compatibility event: a subscriber
/// asks for the newer vocabulary explicitly (`GET /v1/events?ev=2`), and
/// subscribers that do not are served the v1 spelling of the same fact —
/// retirement renders as `asset_quarantined`, which every existing client
/// already handles by dropping the asset. Nobody is broken and nobody is
/// lied to (see [`downgrade_kind`]).
pub const KIND_ASSET_PUBLISHED: &str = "asset_published";
pub const KIND_ASSET_QUARANTINED: &str = "asset_quarantined";
/// The whole asset was deleted (every revision retired, aliases gone,
/// search rows removed). v2 vocabulary.
pub const KIND_ASSET_RETIRED: &str = "asset_retired";
/// One revision was retired (superseded or explicitly deleted); the asset
/// itself may still be live. v2 vocabulary.
pub const KIND_REVISION_RETIRED: &str = "revision_retired";
pub const KIND_ALIAS_SET: &str = "alias_set";
pub const KIND_ALIAS_CLEARED: &str = "alias_cleared";
pub const KIND_ANNOTATION_SET: &str = "annotation_set";
pub const KIND_ANNOTATION_CLEARED: &str = "annotation_cleared";
pub const KIND_GAME_PUBLISHED: &str = "game_published";
pub const KIND_GAME_QUARANTINED: &str = "game_quarantined";
pub const KIND_GAME_ALIAS_SET: &str = "game_alias_set";
pub const KIND_GAME_ALIAS_CLEARED: &str = "game_alias_cleared";
/// An in-progress LocalGen part delta. It names an alias but creates no
/// revision; subscribers keep it only in memory. v4 vocabulary.
pub const KIND_MODEL_PREVIEW: &str = "model_preview";
/// Drop an in-memory preview session and restore the alias's durable head.
/// v4 vocabulary.
pub const KIND_MODEL_PREVIEW_CLEAR: &str = "model_preview_clear";
/// A whole PIPELINE reached a terminal state: every stage is done, or one
/// failed, or it was cancelled. The one event a grid or a library should
/// listen to for "the thing I asked for is finished" — publishes are
/// per-asset and coincidental, and a pipeline that fails publishes nothing
/// at all yet still has to stop looking busy. v5 vocabulary.
pub const KIND_PIPELINE_FINISHED: &str = "pipeline.finished";

pub const PREVIEW_NOT_OWNER: &str = "model preview belongs to another principal";

pub fn preview_audience_ok(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'-'))
}

fn preview_visible(event: &CatalogEvent, audiences: &BTreeSet<String>) -> bool {
    event.model_preview.as_ref().and_then(|preview| preview.audience.as_ref())
        .is_none_or(|audience| audiences.contains(audience))
}

pub const PREVIEW_META_FULL: &str = "model preview metadata space full";
static DISK_UNAVAILABLE_LOGGED: AtomicBool = AtomicBool::new(false);
static RAM_UNAVAILABLE_LOGGED: AtomicBool = AtomicBool::new(false);
/// Refresh through a valid upload or metadata heartbeat at least every 30s.
pub const MODEL_PREVIEW_LEASE: Duration = Duration::from_secs(120);

/// Condvar wait slice; mirrors the connection loop's idle-slice stop checks.
const WAIT_SLICE_MS: u64 = 250;

/// The newest event vocabulary a subscriber can ask for. A request without
/// `ev` gets vocabulary 1.
pub const EVENT_VOCABULARY: u32 = 5;

#[derive(Clone, Debug)]
pub struct ModelPreviewPart {
    pub name: String,
    /// Content token used only by the process-lifetime preview fetch route.
    pub token: String,
    /// Journal and snapshot parts hold None so they cannot prolong draft storage.
    pub file: Option<Arc<DraftFile>>,
    pub len: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelPreviewRename {
    pub from: String,
    pub to: String,
}

#[derive(Clone, Debug)]
pub struct ModelPreviewEvent {
    pub session: String,
    pub audience: Option<String>,
    /// True only for the one session announcement. Late-join snapshots are
    /// rendered as announcements too, with the cumulative current part set.
    pub open: bool,
    pub program: Option<String>,
    /// Only changed parts for ordinary events; all current parts for a
    /// synthetic late-join announcement.
    pub parts: Vec<ModelPreviewPart>,
    pub removed: Vec<String>,
    pub renamed: Vec<ModelPreviewRename>,
}

/// Render an event kind for a subscriber that asked for vocabulary
/// `vocabulary`. Kinds introduced later are downgraded to the nearest v1
/// kind with the SAME operational meaning; there is no kind whose v1
/// spelling would mislead a client into keeping deleted content.
pub fn downgrade_kind(kind: &'static str, vocabulary: u32) -> &'static str {
    if vocabulary >= 2 {
        return kind;
    }
    match kind {
        KIND_ASSET_RETIRED | KIND_REVISION_RETIRED => KIND_ASSET_QUARANTINED,
        other => other,
    }
}

/// One committed catalog change. Identity fields are pre-rendered display
/// spellings (`ast_…`, `arev_…`, `gam_…`, `grev_…`), which are exactly what
/// travels on the wire.
#[derive(Clone, Debug)]
pub struct CatalogEvent {
    pub seq: u64,
    pub kind: &'static str,
    pub namespace: String,
    pub asset_id: Option<String>,
    pub revision: Option<String>,
    pub game_id: Option<String>,
    pub game_revision: Option<String>,
    pub alias: Option<String>,
    pub model_preview: Option<ModelPreviewEvent>,
    /// Display id of the pipeline this event is about (`pipe_…`).
    pub pipeline: Option<String>,
    /// The pipeline's DERIVED state at announcement: `succeeded`, `failed`
    /// or `cancelled`.
    pub pipeline_state: Option<&'static str>,
    /// Lowercase content kind (`video`, `audio`, …) when the transport knew
    /// it at emit time (from the asset's annotation); `None` otherwise.
    pub content_kind: Option<&'static str>,
    pub ts_ms: u64,
}

/// Everything but the sequence number; the journal assigns `seq` at append.
#[derive(Clone, Debug)]
pub struct EventBody {
    pub kind: &'static str,
    pub namespace: String,
    pub asset_id: Option<String>,
    pub revision: Option<String>,
    pub game_id: Option<String>,
    pub game_revision: Option<String>,
    pub alias: Option<String>,
    pub model_preview: Option<ModelPreviewEvent>,
    pub pipeline: Option<String>,
    pub pipeline_state: Option<&'static str>,
    pub content_kind: Option<&'static str>,
    pub ts_ms: u64,
}

impl EventBody {
    pub fn asset(kind: &'static str, ns: &str, asset_id: String, ts_ms: u64) -> EventBody {
        EventBody {
            kind,
            namespace: ns.to_string(),
            asset_id: Some(asset_id),
            revision: None,
            game_id: None,
            game_revision: None,
            alias: None,
            model_preview: None,
            pipeline: None,
            pipeline_state: None,
            content_kind: None,
            ts_ms,
        }
    }

    pub fn game(kind: &'static str, ns: &str, game_id: String, ts_ms: u64) -> EventBody {
        EventBody {
            kind,
            namespace: ns.to_string(),
            asset_id: None,
            revision: None,
            game_id: Some(game_id),
            game_revision: None,
            alias: None,
            model_preview: None,
            pipeline: None,
            pipeline_state: None,
            content_kind: None,
            ts_ms,
        }
    }

    pub fn with_revision(mut self, rev: String) -> EventBody {
        self.revision = Some(rev);
        self
    }

    pub fn with_game_revision(mut self, rev: String) -> EventBody {
        self.game_revision = Some(rev);
        self
    }

    pub fn with_alias(mut self, alias: String) -> EventBody {
        self.alias = Some(alias);
        self
    }

    pub fn with_content_kind(mut self, kind: Option<&'static str>) -> EventBody {
        self.content_kind = kind;
        self
    }

}

/// A parsed resume cursor: journal epoch + highest sequence already seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventCursor {
    pub epoch: [u8; 8],
    pub seq: u64,
}

impl EventCursor {
    pub fn render(&self) -> String {
        format!("{}-{}", super::util::to_hex(&self.epoch), self.seq)
    }

    /// Strict parse of `<16 lowercase hex>-<decimal seq>`; anything else is
    /// `None` (routes map that to a 400).
    pub fn parse(text: &str) -> Option<EventCursor> {
        let (epoch_hex, seq_text) = text.split_once('-')?;
        let epoch = super::util::from_hex_exact::<8>(epoch_hex)?;
        if seq_text.is_empty()
            || seq_text.len() > 20
            || !seq_text.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let seq: u64 = seq_text.parse().ok()?;
        Some(EventCursor { epoch, seq })
    }
}

/// What one poll of the journal produced.
#[derive(Clone, Debug)]
pub struct EventsPoll {
    pub events: Vec<CatalogEvent>,
    /// Resume cursor covering everything scanned (matching or not), so a
    /// kind-filtered subscriber still advances past foreign events.
    pub cursor: EventCursor,
    /// The caller's cursor fell outside the retained window (or belonged to
    /// another journal epoch/process life): events were lost and the client
    /// must resync its catalog view from scratch.
    pub gap: bool,
}

struct Journal {
    /// Retained events; `seq` values are contiguous.
    buf: VecDeque<Arc<CatalogEvent>>,
    /// Sequence the NEXT event will get; first event of a journal gets 1.
    next_seq: u64,
    previews: Vec<ModelPreviewSession>,
    owners: HashMap<PrincipalId, PreviewUsage>,
    disk: u128,
    meta: u128,
}

#[derive(Clone)]
struct ModelPreviewSession {
    owner: PrincipalId,
    audience: Option<String>,
    namespace: String,
    alias: String,
    session: String,
    program: String,
    parts: Vec<ModelPreviewPart>,
    id: u64,
    disk: u128,
    meta: u128,
    logged: BTreeSet<PreviewStep>,
    last_activity: Instant,
    last_seq: u64,
    ts_ms: u64,
}

#[derive(Default)]
struct PreviewUsage { sessions: usize, disk: u128, meta: u128 }

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum PreviewStep { Withheld, Superseded, Evicted, Wait, Metadata }

#[derive(Debug, PartialEq, Eq)]
pub enum PreviewAdmission {
    Withheld { len: u64, budget: u64 },
    Wait { len: u64, used: u64, budget: u64, share: u64 },
    Refused(&'static str),
}

/// Holds declared bytes even if its session lapses while the body arrives.
pub struct PreviewReservation<'a> {
    hub: &'a EventHub,
    owner: PrincipalId,
    session: String,
    id: u64,
    part: String,
    supersedes: Vec<String>,
    len: u64,
    active: bool,
}

impl Drop for PreviewReservation<'_> {
    fn drop(&mut self) {
        if self.active {
            let mut j = self.hub.journal.lock().unwrap();
            j.release(self.owner, self.len);
        }
    }
}

#[derive(Default)]
struct PreviewChanges {
    sessions: Vec<ModelPreviewSession>,
    parts: Vec<ModelPreviewPart>,
    logs: Vec<String>,
}

impl PreviewChanges {
    fn log(&mut self, preview: &mut ModelPreviewSession, step: PreviewStep, text: String) {
        if preview.logged.insert(step) { self.logs.push(text); }
    }

    fn finish(self, log: bool) {
        if log { for line in &self.logs { super::util::log(true, &format!("model preview: {line}")); } }
        // Closing an anonymous draft can do I/O; callers have released the journal.
        drop(self);
    }
}

impl ModelPreviewSession {
    fn metadata_charge(&self) -> u128 {
        (std::mem::size_of::<Self>() + self.program.len() + self.alias.len()
            + self.session.len() + self.namespace.len() + self.audience.as_ref().map_or(0, String::len)
            + self.parts.iter().map(|part| std::mem::size_of::<ModelPreviewPart>()
                + part.name.len() + part.token.len()).sum::<usize>()) as u128
    }
}

fn preview_bytes(bytes: u128) -> String {
    if bytes >= 1024 * 1024 * 1024 { format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0)) }
    else { format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0)) }
}

impl Journal {
    fn share(&self, budget: u64, owner: PrincipalId) -> u64 {
        let count = self.owners.values().filter(|usage| usage.sessions != 0).count()
            + usize::from(self.owners.get(&owner).is_none_or(|usage| usage.sessions == 0));
        budget / count as u64
    }

    fn add_charges(&mut self, preview: &ModelPreviewSession) {
        let usage = self.owners.entry(preview.owner).or_default();
        usage.sessions += 1;
        usage.disk += preview.disk;
        usage.meta += preview.meta;
        self.disk += preview.disk;
        self.meta += preview.meta;
    }

    fn remove_charges(&mut self, preview: &ModelPreviewSession) {
        self.remove_session_charges(preview.owner, preview.disk, preview.meta);
    }

    fn remove_session_charges(&mut self, owner: PrincipalId, disk: u128, meta: u128) {
        let usage = self.owners.get_mut(&owner).unwrap();
        usage.sessions -= 1;
        usage.disk -= disk;
        usage.meta -= meta;
        self.disk -= disk;
        self.meta -= meta;
        self.prune_owner(owner);
    }

    fn prune_owner(&mut self, owner: PrincipalId) {
        if self.owners.get(&owner).is_some_and(|usage| usage.sessions == 0 && usage.disk == 0) {
            self.owners.remove(&owner);
        }
    }

    fn release(&mut self, owner: PrincipalId, len: u64) {
        self.disk -= len as u128;
        // A reservation keeps its owner's row alive (`prune_owner` needs zero disk).
        if let Some(usage) = self.owners.get_mut(&owner) { usage.disk -= len as u128; }
        self.prune_owner(owner);
    }

    fn candidates(&self, owner: PrincipalId, session: &str) -> Vec<usize> {
        let mut candidates: Vec<_> = self.previews.iter().enumerate().filter(|(_, p)| p.session != session)
            .map(|(index, p)| (p.owner != owner, p.last_activity, p.id, index)).collect();
        candidates.sort_unstable();
        candidates.into_iter().map(|(_, _, _, index)| index).collect()
    }

    /// Sequence of the oldest retained event; `next_seq` when empty.
    fn first_seq(&self) -> u64 {
        self.next_seq - self.buf.len() as u64
    }
}

pub struct EventHub {
    epoch: [u8; 8],
    cap: usize,
    preview_meta: Option<u64>,
    log: bool,
    max_waiters: usize,
    journal: Mutex<Journal>,
    wake: Condvar,
    stopped: AtomicBool,
    waiters: AtomicUsize,
}

impl EventHub {
    pub fn new(epoch: [u8; 8], cap: usize, max_waiters: usize, metadata: PreviewBudget, log: bool) -> EventHub {
        let preview_meta = match metadata {
            PreviewBudget::Bytes(bytes) => Some(bytes),
            PreviewBudget::Share(share) => match makepad_platform::system_info::physical_memory_bytes() {
                Some(bytes) => Some((share * bytes as f64) as u64),
                None => {
                    if log && !RAM_UNAVAILABLE_LOGGED.swap(true, Ordering::Relaxed) {
                        super::util::log(true, "model preview: physical RAM unavailable; metadata admitted without a budget");
                    }
                    None
                }
            },
        };
        EventHub {
            epoch,
            preview_meta,
            log,
            cap: cap.max(1),
            max_waiters: max_waiters.max(1),
            journal: Mutex::new(Journal {
                buf: VecDeque::new(),
                next_seq: 1,
                previews: Vec::new(),
                owners: HashMap::new(),
                disk: 0,
                meta: 0,
            }),
            wake: Condvar::new(),
            stopped: AtomicBool::new(false),
            waiters: AtomicUsize::new(0),
        }
    }

    /// Append one committed event and wake every parked long-poll. Called
    /// from state-thread closures AFTER the core mutation succeeded; the
    /// lock is held for O(1).
    pub fn publish(&self, body: EventBody) {
        {
            let mut j = self.journal.lock().unwrap();
            self.append_locked(&mut j, body);
        }
        self.wake.notify_all();
    }

    fn append_locked(&self, j: &mut Journal, body: EventBody) -> u64 {
        let seq = j.next_seq;
        j.next_seq += 1;
        j.buf.push_back(Arc::new(CatalogEvent {
            seq,
            kind: body.kind,
            namespace: body.namespace,
            asset_id: body.asset_id,
            revision: body.revision,
            game_id: body.game_id,
            game_revision: body.game_revision,
            alias: body.alias,
            model_preview: body.model_preview,
            pipeline: body.pipeline,
            pipeline_state: body.pipeline_state,
            content_kind: body.content_kind,
            ts_ms: body.ts_ms,
        }));
        while j.buf.len() > self.cap {
            j.buf.pop_front();
        }
        seq
    }

    /// Announce an empty in-memory preview immediately, before any mesh upload.
    /// Metadata can describe a construction cage while the first mesh is built.
    pub fn open_model_preview(
        &self,
        owner: PrincipalId,
        namespace: String,
        alias: String,
        session: String,
        program: String,
        audience: Option<String>,
        ts_ms: u64,
    ) -> Result<(), &'static str> {
        self.expire_model_previews();
        let mut changes = PreviewChanges::default();
        let result = (|| {
            let mut j = self.journal.lock().unwrap();
            if j.previews.iter().any(|preview| preview.session == session) {
                return Err("model preview session already exists");
            }
            if j.previews.iter().any(|preview| preview.alias == alias) {
                return Err("model preview alias already has a live session");
            }
            let mut preview = ModelPreviewSession {
                owner, audience, namespace, alias, session, program, parts: Vec::new(),
                id: j.next_seq, disk: 0, meta: 0, logged: BTreeSet::new(),
                last_activity: Instant::now(), last_seq: 0, ts_ms,
            };
            preview.meta = preview.metadata_charge();
            self.admit_metadata(&mut j, owner, &preview.session, 0, preview.meta, ts_ms, &mut changes)?;
            preview.last_seq = self.append_locked(&mut j, Self::preview_body(&preview, true, ts_ms));
            // Metadata lapses append events too, so the open's sequence is its identity.
            preview.id = preview.last_seq;
            j.add_charges(&preview);
            j.previews.push(preview);
            Ok(())
        })();
        changes.finish(self.log);
        self.wake.notify_all();
        result
    }

    fn metadata_part(part: &ModelPreviewPart) -> ModelPreviewPart {
        ModelPreviewPart { name: part.name.clone(), token: part.token.clone(), file: None, len: part.len }
    }

    fn preview_body(preview: &ModelPreviewSession, open: bool, ts_ms: u64) -> EventBody {
        EventBody {
            kind: if open { KIND_MODEL_PREVIEW } else { KIND_MODEL_PREVIEW_CLEAR },
            namespace: preview.namespace.clone(), asset_id: None, revision: None,
            game_id: None, game_revision: None, alias: Some(preview.alias.clone()),
            model_preview: Some(ModelPreviewEvent {
                session: preview.session.clone(), audience: preview.audience.clone(), open,
                program: open.then(|| preview.program.clone()),
                parts: if open { preview.parts.iter().map(Self::metadata_part).collect() } else { Vec::new() },
                removed: Vec::new(), renamed: Vec::new(),
            }),
            pipeline: None, pipeline_state: None, content_kind: Some("model-program"), ts_ms,
        }
    }

    /// Drop abandoned sessions and emit bounded clear tombstones. Only the
    /// monotonic activity clock controls expiry; event timestamps are cosmetic.
    /// The existing server janitor calls this even with no subscribers.
    pub fn expire_model_previews(&self) -> usize {
        let ts_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis().min(u64::MAX as u128) as u64;
        self.expire_model_previews_at(Instant::now(), ts_ms)
    }

    fn expire_model_previews_at(&self, now: Instant, ts_ms: u64) -> usize {
        let mut expired = Vec::new();
        {
            let mut j = self.journal.lock().unwrap();
            let mut index = 0;
            while index < j.previews.len() {
                if now.saturating_duration_since(j.previews[index].last_activity) >= MODEL_PREVIEW_LEASE {
                    let preview = j.previews.remove(index);
                    j.remove_charges(&preview);
                    self.append_locked(&mut j, Self::preview_body(&preview, false, ts_ms));
                    expired.push(preview);
                } else { index += 1; }
            }
        }
        let count = expired.len();
        drop(expired);
        if count != 0 { self.wake.notify_all(); }
        count
    }

    /// Namespace of a live session, used to authorize a byte upload before
    /// consuming its body.
    pub fn model_preview_namespace(&self, session: &str) -> Option<String> {
        self.expire_model_previews();
        self.journal
            .lock()
            .unwrap()
            .previews
            .iter()
            .find(|preview| preview.session == session)
            .map(|preview| preview.namespace.clone())
    }

    pub fn model_preview_owner(&self, session: &str) -> Option<PrincipalId> {
        let j = self.journal.lock().unwrap();
        j.previews.iter().find(|preview| preview.session == session).map(|preview| preview.owner)
    }

    /// One worker may upload for several sessions; progress keeps its owner's
    /// live sessions alive without emitting scene updates or reviving expired ones.
    pub fn refresh_model_preview_owner(&self, owner: PrincipalId, now: Instant) {
        let mut j = self.journal.lock().unwrap();
        for preview in &mut j.previews {
            if preview.owner == owner
                && now.saturating_duration_since(preview.last_activity) < MODEL_PREVIEW_LEASE
            {
                preview.last_activity = now;
            }
        }
    }

    /// Measure the volume outside the journal; drafts already charged remain usable space.
    pub fn preview_disk_budget(&self, cfg: &ServerConfig) -> Option<u64> {
        match cfg.preview_disk {
            PreviewBudget::Bytes(bytes) => Some(bytes),
            PreviewBudget::Share(share) => match makepad_platform::storage::volume_available_bytes(&cfg.root) {
                Ok(available) => {
                    let total = self.journal.lock().unwrap().disk;
                    Some((share * (available as u128 + total) as f64) as u64)
                }
                Err(_) => {
                    if self.log && !DISK_UNAVAILABLE_LOGGED.swap(true, Ordering::Relaxed) {
                        super::util::log(true, "model preview: free disk unavailable; drafts admitted without a budget");
                    }
                    None
                }
            },
        }
    }

    fn admit_metadata(&self, j: &mut Journal, owner: PrincipalId, session: &str,
        old: u128, new: u128, ts_ms: u64, changes: &mut PreviewChanges) -> Result<(), &'static str>
    {
        let Some(budget) = self.preview_meta else { return Ok(()); };
        if new <= old || j.meta - old + new <= budget as u128 { return Ok(()); }
        let mut lapsed = BTreeSet::new();
        for index in j.candidates(owner, session) {
            if j.meta - old + new <= budget as u128 { break; }
            let preview = &j.previews[index];
            let candidate_owner = preview.owner;
            let used = j.owners[&candidate_owner].meta;
            let projected = if candidate_owner == owner { used - old + new } else { used };
            let share = j.share(budget, owner);
            if projected <= share as u128 { continue; }
            let disk = preview.disk;
            let meta = preview.meta;
            let body = Self::preview_body(preview, false, ts_ms);
            j.remove_session_charges(candidate_owner, disk, meta);
            self.append_locked(j, body);
            let line = format!("metadata lapse of session {} (owner {}) for session {session} (owner {}): metadata {}, budget {}, share {}",
                j.previews[index].session, principal_str(&candidate_owner), principal_str(&owner), preview_bytes(j.meta), preview_bytes(budget as u128), preview_bytes(share as u128));
            changes.log(&mut j.previews[index], PreviewStep::Metadata, line);
            lapsed.insert(index);
        }
        // Compact once: pressure across many sessions must not shift the vector per lapse.
        if !lapsed.is_empty() {
            for (index, preview) in std::mem::take(&mut j.previews).into_iter().enumerate() {
                if lapsed.contains(&index) { changes.sessions.push(preview); }
                else { j.previews.push(preview); }
            }
        }
        if j.meta - old + new <= budget as u128 { Ok(()) } else { Err(PREVIEW_META_FULL) }
    }

    fn delta_body(preview: &ModelPreviewSession, parts: Vec<ModelPreviewPart>, removed: Vec<String>, ts_ms: u64) -> EventBody {
        let mut body = Self::preview_body(preview, false, ts_ms);
        body.kind = KIND_MODEL_PREVIEW;
        let delta = body.model_preview.as_mut().unwrap();
        delta.parts = parts;
        delta.removed = removed;
        body
    }

    fn remove_parts(&self, j: &mut Journal, index: usize, names: &[String], ts_ms: u64, changes: &mut PreviewChanges) -> (usize, u128) {
        if names.is_empty() { return (0, 0); }
        let order: BTreeMap<_, _> = names.iter().enumerate().map(|(index, name)| (name.as_str(), index)).collect();
        let preview = &mut j.previews[index];
        let old_meta = preview.meta;
        let mut removed = Vec::new();
        let mut bytes = 0;
        for part in std::mem::take(&mut preview.parts) {
            if let Some(rank) = order.get(part.name.as_str()) {
                removed.push((*rank, part.name.clone()));
                bytes += part.len as u128;
                changes.parts.push(part);
            } else { preview.parts.push(part); }
        }
        let count = removed.len();
        if count == 0 { return (0, 0); }
        removed.sort_unstable_by_key(|(rank, _)| *rank);
        preview.disk -= bytes;
        preview.meta = preview.metadata_charge();
        let owner = preview.owner;
        let new_meta = preview.meta;
        let body = Self::delta_body(preview, Vec::new(), removed.into_iter().map(|(_, name)| name).collect(), ts_ms);
        let usage = j.owners.get_mut(&owner).unwrap();
        usage.disk -= bytes;
        usage.meta = usage.meta - old_meta + new_meta;
        j.disk -= bytes;
        j.meta = j.meta - old_meta + new_meta;
        let seq = self.append_locked(j, body);
        j.previews[index].last_seq = seq;
        j.previews[index].ts_ms = ts_ms;
        // An eviction is not activity by this author and must not renew its lease.
        (count, bytes)
    }

    /// Admission reserves the whole declared body before the route asks for any bytes.
    pub fn admit_model_preview_part(&self, owner: PrincipalId, session: &str, part: &str,
        len: u64, supersedes: &[String], budget: Option<u64>, now: Instant) -> Result<PreviewReservation<'_>, PreviewAdmission>
    {
        let ts_ms = super::util::now_ms();
        self.expire_model_previews_at(now, ts_ms);
        let mut changes = PreviewChanges::default();
        let result = (|| {
            let mut j = self.journal.lock().unwrap();
            let index = j.previews.iter().position(|preview| preview.session == session)
                .ok_or(PreviewAdmission::Refused("model preview session not found"))?;
            if j.previews[index].owner != owner { return Err(PreviewAdmission::Refused(PREVIEW_NOT_OWNER)); }
            if let Some(budget) = budget {
                if len > budget {
                    changes.log(&mut j.previews[index], PreviewStep::Withheld,
                        format!("withheld part {part} of session {session} (owner {}): {}, budget {}", principal_str(&owner), preview_bytes(len as u128), preview_bytes(budget as u128)));
                    return Err(PreviewAdmission::Withheld { len, budget });
                }
                if j.disk + len as u128 > budget as u128 {
                    let (count, bytes) = self.remove_parts(&mut j, index, supersedes, ts_ms, &mut changes);
                    if count != 0 {
                        changes.log(&mut j.previews[index], PreviewStep::Superseded,
                            format!("superseded early {count} parts ({}) of session {session} (owner {}) for part {part}", principal_str(&owner), preview_bytes(bytes)));
                    }
                }
                let share = j.share(budget, owner);
                if j.disk + len as u128 > budget as u128 {
                    for candidate in j.candidates(owner, session) {
                        if j.disk + len as u128 <= budget as u128 { break; }
                        let p = &j.previews[candidate];
                        let candidate_owner = p.owner;
                        let mut usage = j.owners[&candidate_owner].disk + if candidate_owner == owner { len as u128 } else { 0 };
                        if usage <= share as u128 { continue; }
                        let mut parts: Vec<_> = p.parts.iter().map(|part| (part.len, part.name.clone())).collect();
                        parts.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
                        let mut used = j.disk;
                        let mut names = Vec::new();
                        for (bytes, name) in parts {
                            if used + len as u128 <= budget as u128 || usage <= share as u128 { break; }
                            used -= bytes as u128;
                            usage -= bytes as u128;
                            names.push(name);
                        }
                        let (count, bytes) = self.remove_parts(&mut j, candidate, &names, ts_ms, &mut changes);
                        if count != 0 {
                            let line = format!("evicted {count} parts ({}) of session {} (owner {}) for an upload of {session} (owner {}): draft space {} of {}, share {}",
                                preview_bytes(bytes), j.previews[candidate].session, principal_str(&candidate_owner), principal_str(&owner), preview_bytes(j.disk), preview_bytes(budget as u128), preview_bytes(share as u128));
                            changes.log(&mut j.previews[candidate], PreviewStep::Evicted, line);
                        }
                    }
                }
                if j.disk + len as u128 > budget as u128 {
                    let used = j.disk.min(u64::MAX as u128) as u64;
                    changes.log(&mut j.previews[index], PreviewStep::Wait,
                        format!("wait for part {part} of session {session} (owner {}): {}, draft space {} of {}, share {}",
                            principal_str(&owner), preview_bytes(len as u128), preview_bytes(used as u128), preview_bytes(budget as u128), preview_bytes(share as u128)));
                    return Err(PreviewAdmission::Wait { len, used, budget, share });
                }
            }
            j.disk += len as u128;
            j.owners.get_mut(&owner).unwrap().disk += len as u128;
            j.previews[index].last_activity = now;
            Ok(PreviewReservation { hub: self, owner, session: session.into(), id: j.previews[index].id,
                part: part.into(), supersedes: supersedes.to_vec(), len, active: true })
        })();
        changes.finish(self.log);
        self.wake.notify_all();
        result
    }

    /// The new part and its superseded names form one atomic scene delta.
    pub fn update_model_preview_part(&self, mut reservation: PreviewReservation<'_>, file: Arc<DraftFile>, ts_ms: u64) -> Result<(), &'static str> {
        self.expire_model_previews();
        let mut changes = PreviewChanges::default();
        let result = (|| {
            let mut j = self.journal.lock().unwrap();
            if !std::ptr::eq(self, reservation.hub) { return Err("model preview reservation hub"); }
            if file.len != reservation.len { return Err("model preview reservation length"); }
            let index = j.previews.iter().position(|preview| preview.session == reservation.session && preview.id == reservation.id)
                .ok_or("model preview session not found")?;
            if j.previews[index].owner != reservation.owner { return Err(PREVIEW_NOT_OWNER); }
            let mut preview = j.previews[index].clone();
            let part = ModelPreviewPart { name: reservation.part.clone(), token: file.token.clone(), len: file.len, file: Some(file.clone()) };
            let mut removed = Vec::new();
            preview.parts.retain(|old| {
                if old.name == part.name || reservation.supersedes.contains(&old.name) {
                    if reservation.supersedes.contains(&old.name) { removed.push(old.name.clone()); }
                    preview.disk -= old.len as u128;
                    false
                } else { true }
            });
            preview.disk += part.len as u128;
            preview.parts.push(part.clone());
            preview.meta = preview.metadata_charge();
            let old_meta = j.previews[index].meta;
            self.admit_metadata(&mut j, preview.owner, &preview.session, old_meta, preview.meta, ts_ms, &mut changes)?;
            let index = j.previews.iter().position(|p| p.id == reservation.id).unwrap();
            // Release while the session still counts for its owner, then swap the session's charges.
            j.release(reservation.owner, reservation.len);
            reservation.active = false;
            let old = j.previews.remove(index);
            j.remove_charges(&old);
            changes.sessions.push(old);
            preview.last_activity = Instant::now();
            preview.ts_ms = ts_ms;
            preview.last_seq = self.append_locked(&mut j, Self::delta_body(&preview, vec![Self::metadata_part(&part)], removed, ts_ms));
            j.add_charges(&preview);
            j.previews.insert(index, preview);
            Ok(())
        })();
        changes.finish(self.log);
        self.wake.notify_all();
        result
    }

    /// Validate the prospective session before admitting its metadata delta.
    pub fn update_model_preview_metadata(&self, owner: PrincipalId, session: &str,
        program: Option<String>, removed: Vec<String>, renamed: Vec<ModelPreviewRename>, ts_ms: u64) -> Result<(), &'static str>
    {
        self.expire_model_previews();
        let mut changes = PreviewChanges::default();
        let result = (|| {
            let mut j = self.journal.lock().unwrap();
            let index = j.previews.iter().position(|preview| preview.session == session)
                .ok_or("model preview session not found")?;
            if j.previews[index].owner != owner { return Err(PREVIEW_NOT_OWNER); }
            if program.is_none() && removed.is_empty() && renamed.is_empty() {
                j.previews[index].last_activity = Instant::now();
                return Ok(());
            }
            let mut preview = j.previews[index].clone();
            for rename in &renamed {
                if rename.from == rename.to { continue; }
                if preview.parts.iter().any(|part| part.name == rename.to) { return Err("model preview rename target exists"); }
                let part = preview.parts.iter_mut().find(|part| part.name == rename.from)
                    .ok_or("model preview rename source not found")?;
                part.name = rename.to.clone();
            }
            preview.parts.retain(|part| {
                if removed.contains(&part.name) { preview.disk -= part.len as u128; false } else { true }
            });
            if let Some(program) = &program { preview.program = program.clone(); }
            preview.meta = preview.metadata_charge();
            let old_meta = j.previews[index].meta;
            self.admit_metadata(&mut j, owner, session, old_meta, preview.meta, ts_ms, &mut changes)?;
            let index = j.previews.iter().position(|p| p.id == preview.id).unwrap();
            let old = j.previews.remove(index);
            j.remove_charges(&old);
            changes.sessions.push(old);
            let mut body = Self::delta_body(&preview, Vec::new(), removed, ts_ms);
            let delta = body.model_preview.as_mut().unwrap();
            delta.program = program;
            delta.renamed = renamed;
            preview.last_seq = self.append_locked(&mut j, body);
            preview.last_activity = Instant::now();
            preview.ts_ms = ts_ms;
            j.add_charges(&preview);
            j.previews.insert(index, preview);
            Ok(())
        })();
        changes.finish(self.log);
        self.wake.notify_all();
        result
    }

    /// Clear withdraws tokens immediately; only existing readers may retain files.
    pub fn clear_model_preview(&self, owner: PrincipalId, moderate: bool, session: &str, ts_ms: u64) -> Result<(), &'static str> {
        let preview;
        {
            let mut j = self.journal.lock().unwrap();
            let Some(index) = j.previews.iter().position(|preview| preview.session == session)
            else { return Err("model preview session not found"); };
            if j.previews[index].owner != owner && !moderate { return Err(PREVIEW_NOT_OWNER); }
            preview = j.previews.remove(index);
            j.remove_charges(&preview);
            self.append_locked(&mut j, Self::preview_body(&preview, false, ts_ms));
        }
        drop(preview);
        self.wake.notify_all();
        Ok(())
    }

    /// Complete cursor-less bootstrap, captured atomically with its tail.
    /// Every live session is returned, independently of the ordinary delta
    /// page size. Truncating here would lose omitted sessions behind the tail.
    pub fn model_preview_snapshot(&self, content_kind: Option<&str>, audiences: &BTreeSet<String>) -> EventsPoll {
        self.expire_model_previews();
        let j = self.journal.lock().unwrap();
        let mut events = Vec::new();
        if !matches!(content_kind, Some(kind) if kind != "model-program") {
            for preview in &j.previews {
                let body = Self::preview_body(preview, true, preview.ts_ms);
                events.push(CatalogEvent { seq: preview.last_seq, kind: body.kind,
                    namespace: body.namespace, asset_id: None, revision: None,
                    game_id: None, game_revision: None, alias: body.alias,
                    model_preview: body.model_preview, pipeline: None, pipeline_state: None,
                    content_kind: body.content_kind, ts_ms: body.ts_ms });
            }
        }
        let cursor = EventCursor { epoch: self.epoch, seq: j.next_seq - 1 };
        drop(j);
        events.retain(|event| preview_visible(event, audiences));
        events.sort_by_key(|event| event.seq);
        EventsPoll { events, cursor, gap: false }
    }

    /// Only current tokens can acquire a reader; an existing reader may finish
    /// after replacement or withdrawal without keeping the token reachable.
    pub fn model_preview_mesh(&self, token: &str, audience: Option<&str>) -> Option<Arc<DraftFile>> {
        self.expire_model_previews();
        let j = self.journal.lock().unwrap();
        for preview in &j.previews {
            if preview.audience.as_deref().is_some_and(|required| Some(required) != audience) { continue; }
            if let Some(part) = preview.parts.iter().find(|part| part.token == token) {
                return part.file.clone();
            }
        }
        None
    }

    /// The resume cursor a fresh (cursor-less) subscriber starts from: the
    /// current tail. The subscriber then loads its catalog view once and
    /// receives only events committed after this point (also the wake-up
    /// point of store sync's log long-poll).
    pub fn tail_cursor(&self) -> EventCursor {
        let j = self.journal.lock().unwrap();
        EventCursor { epoch: self.epoch, seq: j.next_seq - 1 }
    }

    /// One non-blocking poll: events with `seq > cursor.seq`, kind-filtered,
    /// at most `limit`. See [`EventsPoll::cursor`] for the advance rule.
    pub fn poll_after(
        &self,
        cursor: EventCursor,
        content_kind: Option<&str>,
        limit: usize,
        audiences: &BTreeSet<String>,
    ) -> EventsPoll {
        self.expire_model_previews();
        let j = self.journal.lock().unwrap();
        // Another epoch's cursor, or a sequence this journal never issued:
        // the client's view is unanchored — force a resync at the tail.
        if cursor.epoch != self.epoch || cursor.seq >= j.next_seq {
            return EventsPoll {
                events: Vec::new(),
                cursor: EventCursor { epoch: self.epoch, seq: j.next_seq - 1 },
                gap: true,
            };
        }
        // Events (cursor.seq, first_seq) have been evicted: the client
        // missed changes it can never receive.
        if cursor.seq + 1 < j.first_seq() {
            return EventsPoll {
                events: Vec::new(),
                cursor: EventCursor { epoch: self.epoch, seq: j.next_seq - 1 },
                gap: true,
            };
        }
        // Copy only shared event handles under the lock; audience selection and
        // payload cloning must not extend the writer's journal critical section.
        let candidates: Vec<_> = j.buf.iter().filter(|ev| ev.seq > cursor.seq).cloned().collect();
        drop(j);
        let mut events = Vec::new();
        let mut scanned_to = cursor.seq;
        for ev in candidates {
            if ev.seq <= cursor.seq {
                continue;
            }
            let matches = match (content_kind, ev.content_kind) {
                // No filter: everything matches.
                (None, _) => true,
                // Filtered, event kind known: exact match only.
                (Some(want), Some(have)) => want == have,
                // Filtered, event kind unknown: include. A refresh is
                // idempotent; silently dropping a possibly-matching change
                // is not.
                (Some(_), None) => true,
            };
            if matches && preview_visible(&ev, audiences) {
                if events.len() >= limit {
                    // Batch full: stop BEFORE consuming this event so the
                    // next poll resumes exactly here.
                    break;
                }
                events.push((*ev).clone());
            }
            scanned_to = ev.seq;
        }
        EventsPoll {
            events,
            cursor: EventCursor { epoch: self.epoch, seq: scanned_to },
            gap: false,
        }
    }

    /// Park until an event newer than `after_seq` exists, the deadline
    /// passes, the hub shuts down, or the waiter cap is hit (returns
    /// immediately). Returns true when new events may exist.
    pub fn wait_beyond(&self, after_seq: u64, deadline: Instant) -> bool {
        if self.stopped.load(Ordering::SeqCst) {
            return false;
        }
        // Cap check-and-claim; over the cap the poll degrades to an
        // immediate empty response instead of parking one more thread.
        let mut claimed = self.waiters.load(Ordering::SeqCst);
        loop {
            if claimed >= self.max_waiters {
                return false;
            }
            match self.waiters.compare_exchange(
                claimed,
                claimed + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(cur) => claimed = cur,
            }
        }
        let mut woke = false;
        let mut j = self.journal.lock().unwrap();
        loop {
            if j.next_seq - 1 > after_seq {
                woke = true;
                break;
            }
            if self.stopped.load(Ordering::SeqCst) {
                break;
            }
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let slice = (deadline - now).min(Duration::from_millis(WAIT_SLICE_MS));
            let (guard, _timeout) = self.wake.wait_timeout(j, slice).unwrap();
            j = guard;
        }
        drop(j);
        self.waiters.fetch_sub(1, Ordering::SeqCst);
        woke
    }

    /// Wake every parked poll and refuse future waits. Idempotent; called
    /// before connection threads are joined so shutdown never blocks behind
    /// a long-poll deadline.
    pub fn shutdown(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.wake.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct TestSpool(std::path::PathBuf);

    impl TestSpool {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("preview-spool-{}",
                super::super::util::to_hex(&super::super::util::rand32().unwrap())));
            DraftFile::recover(&root).unwrap();
            Self(root)
        }

        fn part(&self, token: &str, bytes: &[u8]) -> Arc<DraftFile> {
            use std::io::Write;
            let mut file = DraftFile::create(&self.0).unwrap();
            file.write_all(bytes).unwrap();
            Arc::new(DraftFile { file, len: bytes.len() as u64, token: token.into() })
        }

        fn assert_empty(&self) {
            assert_eq!(std::fs::read_dir(self.0.join("previews")).unwrap().count(), 0);
        }
    }

    impl Drop for TestSpool {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn put_preview(hub: &EventHub, owner: PrincipalId, session: &str, name: String, file: Arc<DraftFile>, ts_ms: u64) -> Result<(), &'static str> {
        let reservation = hub.admit_model_preview_part(owner, session, &name, file.len, &[], None, Instant::now()).unwrap();
        hub.update_model_preview_part(reservation, file, ts_ms)
    }

    fn open_preview(hub: &EventHub, owner: u8, session: &str) {
        hub.open_model_preview(PrincipalId([owner; 16]), "gen".into(), format!("gen/drafts/{session}"), session.into(), "cage".into(), None, 0).unwrap();
    }

    fn draft(hub: &EventHub, spool: &TestSpool, owner: u8, session: &str, name: &str, len: usize) {
        put_preview(hub, PrincipalId([owner; 16]), session, name.into(), spool.part(&format!("pmesh_{session}_{name}"), &vec![1; len]), 0).unwrap();
    }

    fn body(kind: &'static str, ns: &str, ck: Option<&'static str>) -> EventBody {
        EventBody::asset(kind, ns, format!("ast_{ns}"), 5).with_content_kind(ck)
    }

    #[test]
    fn cursor_render_parse_roundtrip_and_strictness() {
        let c = EventCursor { epoch: [0xab; 8], seq: 42 };
        let text = c.render();
        assert_eq!(text, format!("{}-42", "ab".repeat(8)));
        assert_eq!(EventCursor::parse(&text), Some(c));
        for bad in [
            "",
            "abab-1",                          // short epoch
            &format!("{}-", "ab".repeat(8)),   // empty seq
            &format!("{}-x1", "ab".repeat(8)), // non-digit seq
            &format!("{}-1", "AB".repeat(8)),  // uppercase epoch
            &"ab".repeat(8),                   // no separator
            &format!("{}-123456789012345678901", "ab".repeat(8)), // seq too long
        ] {
            assert_eq!(EventCursor::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn sequences_are_monotonic_and_retention_gaps_reported() {
        let hub = EventHub::new([1; 8], 4, 4, PreviewBudget::Bytes(u64::MAX), false);
        let start = hub.tail_cursor();
        assert_eq!(start.seq, 0);
        for i in 0..10u64 {
            hub.publish(body(KIND_ASSET_PUBLISHED, &format!("n{i}"), None));
        }
        // Ring cap 4: only seqs 7..=10 retained; the start cursor is out.
        let poll = hub.poll_after(start, None, 100, &Default::default());
        assert!(poll.gap);
        assert_eq!(poll.cursor.seq, 10);
        // Resuming from the gap cursor is clean.
        let poll = hub.poll_after(poll.cursor, None, 100, &Default::default());
        assert!(!poll.gap);
        assert!(poll.events.is_empty());
        // A cursor still inside the window replays exactly the missing tail.
        let mid = EventCursor { epoch: [1; 8], seq: 8 };
        let poll = hub.poll_after(mid, None, 100, &Default::default());
        assert!(!poll.gap);
        assert_eq!(poll.events.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![9, 10]);
    }

    #[test]
    fn epoch_mismatch_and_future_cursor_force_resync() {
        let hub = EventHub::new([2; 8], 8, 4, PreviewBudget::Bytes(u64::MAX), false);
        hub.publish(body(KIND_ALIAS_SET, "ns", None));
        let foreign = EventCursor { epoch: [9; 8], seq: 1 };
        assert!(hub.poll_after(foreign, None, 10, &Default::default()).gap);
        let future = EventCursor { epoch: [2; 8], seq: 99 };
        assert!(hub.poll_after(future, None, 10, &Default::default()).gap);
    }

    #[test]
    fn kind_filter_matches_exact_and_includes_unknown() {
        let hub = EventHub::new([3; 8], 16, 4, PreviewBudget::Bytes(u64::MAX), false);
        hub.publish(body(KIND_ASSET_PUBLISHED, "a", Some("video")));
        hub.publish(body(KIND_ASSET_PUBLISHED, "b", Some("mesh")));
        hub.publish(body(KIND_ASSET_PUBLISHED, "c", None));
        let start = EventCursor { epoch: [3; 8], seq: 0 };
        let poll = hub.poll_after(start, Some("video"), 10, &Default::default());
        let kinds: Vec<_> = poll.events.iter().map(|e| e.namespace.as_str()).collect();
        assert_eq!(kinds, vec!["a", "c"]);
        // Cursor advanced past the filtered-out mesh event too.
        assert_eq!(poll.cursor.seq, 3);
    }

    #[test]
    fn batch_limit_stops_before_dropping() {
        let hub = EventHub::new([4; 8], 16, 4, PreviewBudget::Bytes(u64::MAX), false);
        for i in 0..5u64 {
            hub.publish(body(KIND_ASSET_PUBLISHED, &format!("n{i}"), None));
        }
        let start = EventCursor { epoch: [4; 8], seq: 0 };
        let poll = hub.poll_after(start, None, 2, &Default::default());
        assert_eq!(poll.events.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(poll.cursor.seq, 2);
        let rest = hub.poll_after(poll.cursor, None, 100, &Default::default());
        assert_eq!(rest.events.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![3, 4, 5]);
    }

    #[test]
    fn model_preview_sessions_announce_accumulate_sync_and_cancel() {
        let spool = TestSpool::new();
        let hub = EventHub::new([6; 8], 16, 4, PreviewBudget::Bytes(u64::MAX), false);
        hub.open_model_preview(PrincipalId([1; 16]), "gen".into(), "gen/csg/mug".into(), "session-a".into(), "program-v1".into(), None, 7)
        .unwrap();
        put_preview(&hub, PrincipalId([1; 16]), "session-a", "body".into(), spool.part("pmesh_body", b"body-glb"), 8)
        .unwrap();
        put_preview(&hub, PrincipalId([1; 16]), "session-a", "handle".into(), spool.part("pmesh_handle", b"handle-glb"), 9)
        .unwrap();
        hub.update_model_preview_metadata(PrincipalId([1; 16]), "session-a", Some("program-v2".into()), vec!["handle".into()], vec![ModelPreviewRename { from: "body".into(), to: "shell".into() }], 10)
        .unwrap();

        let poll = hub.poll_after(EventCursor { epoch: [6; 8], seq: 0 }, None, 10, &Default::default());
        assert_eq!(poll.events.len(), 4);
        assert!(poll.events[0].model_preview.as_ref().unwrap().open);
        assert!(poll.events[0].model_preview.as_ref().unwrap().parts.is_empty());
        assert_eq!(poll.events[1].model_preview.as_ref().unwrap().parts[0].name, "body");
        assert!(!poll.events[1].model_preview.as_ref().unwrap().open);
        assert_eq!(poll.events[0].model_preview.as_ref().unwrap().program.as_deref(), Some("program-v1"));
        assert_eq!(poll.events[1].model_preview.as_ref().unwrap().parts.len(), 1);
        assert_eq!(poll.events[3].model_preview.as_ref().unwrap().removed, ["handle"]);
        for event in &poll.events {
            assert_eq!(event.alias.as_deref(), Some("gen/csg/mug"));
            assert_eq!(event.asset_id, None);
            assert_eq!(event.revision, None);
            assert_eq!(event.content_kind, Some("model-program"));
        }
        assert_eq!(hub.model_preview_mesh("pmesh_body", None).unwrap().len, 8);
        assert!(hub.model_preview_mesh("pmesh_handle", None).is_none());
        #[cfg(unix)]
        spool.assert_empty();

        // A cursor-less late joiner gets one cumulative announcement, not
        // the historical deltas: renamed shell present, removed handle gone.
        let sync = hub.model_preview_snapshot(None, &Default::default()).events;
        assert_eq!(sync.len(), 1);
        let preview = sync[0].model_preview.as_ref().unwrap();
        assert!(preview.open);
        assert_eq!(preview.program.as_deref(), Some("program-v2"));
        assert_eq!(preview.parts.iter().map(|part| part.name.as_str()).collect::<Vec<_>>(), ["shell"]);

        let reader = hub.model_preview_mesh("pmesh_body", None).unwrap();
        let old = Arc::downgrade(&reader);
        put_preview(&hub, PrincipalId([1; 16]), "session-a", "shell".into(), spool.part("pmesh_new", b"new body"), 11).unwrap();
        assert!(hub.model_preview_mesh("pmesh_body", None).is_none());
        assert!(old.upgrade().is_some(), "an existing reader owns the replaced file");
        let replacement = Arc::downgrade(&hub.model_preview_mesh("pmesh_new", None).unwrap());
        hub.clear_model_preview(PrincipalId([1; 16]), false, "session-a", 12).unwrap();
        assert!(replacement.upgrade().is_none(), "clear releases the live file");
        {
            use std::io::{Read, Seek};
            let mut file = &reader.file;
            file.rewind().unwrap();
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, b"body-glb", "the existing reader still finishes after clear");
        }
        drop(reader);
        assert!(old.upgrade().is_none(), "the last reader releases the replaced file");
        spool.assert_empty();
        assert!(hub.model_preview_snapshot(None, &Default::default()).events.is_empty());
        assert!(hub.model_preview_namespace("session-a").is_none());
        assert!(hub.model_preview_mesh("pmesh_body", None).is_none());
        let clear = hub.poll_after(EventCursor { epoch: [6; 8], seq: 5 }, None, 10, &Default::default());
        assert_eq!(clear.events.len(), 1);
        assert_eq!(clear.events[0].kind, KIND_MODEL_PREVIEW_CLEAR);
    }

    #[test]
    fn preview_disk_lends_space_and_reclaims_oldest_largest_parts_fairly() {
        let spool = TestSpool::new();
        let hub = EventHub::new([12; 8], 128, 1, PreviewBudget::Bytes(u64::MAX), false);
        for (owner, session) in [(1, "old"), (1, "recent"), (2, "upload"), (3, "other")] { open_preview(&hub, owner, session); }
        draft(&hub, &spool, 1, "old", "small", 15);
        draft(&hub, &spool, 1, "old", "large", 25);
        draft(&hub, &spool, 1, "recent", "part", 30);
        draft(&hub, &spool, 3, "other", "part", 30);
        let now = Instant::now();
        { let mut j = hub.journal.lock().unwrap();
            j.previews.iter_mut().find(|p| p.session == "old").unwrap().last_activity = now - Duration::from_secs(2);
            j.previews.iter_mut().find(|p| p.session == "recent").unwrap().last_activity = now - Duration::from_secs(1);
        }
        let cursor = hub.model_preview_snapshot(None, &Default::default()).cursor;
        let reservation = hub.admit_model_preview_part(PrincipalId([2; 16]), "upload", "part", 30, &[], Some(100), now).unwrap();
        let events = hub.poll_after(cursor, None, 128, &Default::default()).events;
        assert_eq!(events.len(), 1, "a within-share upload reclaims one oldest session");
        let delta = events[0].model_preview.as_ref().unwrap();
        assert_eq!(delta.session, "old", "least recently active flooder is reclaimed first");
        assert_eq!(delta.removed, ["large", "small"], "largest parts are removed first in one delta");
        assert_eq!(hub.journal.lock().unwrap().disk, 90, "the reservation occupies the freed space");
        drop(reservation);
        assert!(hub.model_preview_namespace("old").is_some(), "eviction keeps the cage and lease");

        // The uploader's own sessions precede even an older foreign flooder.
        let hub = EventHub::new([13; 8], 128, 1, PreviewBudget::Bytes(u64::MAX), false);
        for (owner, session) in [(1, "foreign"), (2, "own"), (2, "upload"), (3, "idle")] { open_preview(&hub, owner, session); }
        draft(&hub, &spool, 1, "foreign", "part", 60);
        draft(&hub, &spool, 2, "own", "part", 40);
        { let mut j = hub.journal.lock().unwrap();
            j.previews.iter_mut().find(|p| p.session == "foreign").unwrap().last_activity = now - Duration::from_secs(10);
        }
        let cursor = hub.model_preview_snapshot(None, &Default::default()).cursor;
        let reservation = hub.admit_model_preview_part(PrincipalId([2; 16]), "upload", "part", 30, &[], Some(100), now).unwrap();
        let events = hub.poll_after(cursor, None, 128, &Default::default()).events;
        assert_eq!(events.len(), 1, "only enough space for this upload is reclaimed");
        assert_eq!(events[0].model_preview.as_ref().unwrap().session, "own", "own sessions precede a foreign flooder");
        assert_eq!(hub.journal.lock().unwrap().disk, 90, "foreign borrowed space remains when the upload fits");
        drop(reservation);
    }

    #[test]
    fn preview_wait_withheld_and_reservations_do_not_lose_charges() {
        let spool = TestSpool::new();
        let hub = EventHub::new([14; 8], 128, 1, PreviewBudget::Bytes(u64::MAX), false);
        open_preview(&hub, 1, "upload"); open_preview(&hub, 2, "other");
        draft(&hub, &spool, 2, "other", "part", 50);
        let cursor = hub.model_preview_snapshot(None, &Default::default()).cursor;
        assert!(matches!(hub.admit_model_preview_part(PrincipalId([1; 16]), "upload", "part", 60, &[], Some(100), Instant::now()),
            Err(PreviewAdmission::Wait { len: 60, used: 50, budget: 100, share: 50 })), "over-share uploader waits with exact book numbers");
        assert_eq!(hub.journal.lock().unwrap().disk, 50, "a waiting upload reserves nothing");
        assert!(matches!(hub.admit_model_preview_part(PrincipalId([1; 16]), "upload", "part", 101, &[], Some(100), Instant::now()),
            Err(PreviewAdmission::Withheld { len: 101, budget: 100 })), "only a part beyond the entire budget is withheld");
        assert!(hub.poll_after(cursor, None, 128, &Default::default()).events.is_empty(), "within-share peers survive wait and withheld admissions");
        hub.clear_model_preview(PrincipalId([2; 16]), false, "other", 1).unwrap();
        let reservation = hub.admit_model_preview_part(PrincipalId([1; 16]), "upload", "part", 60, &[], Some(100), Instant::now()).unwrap();
        assert_eq!(hub.journal.lock().unwrap().disk, 60, "clearing another owner makes the same upload fit");
        drop(reservation);
        assert_eq!(hub.journal.lock().unwrap().disk, 0, "aborted upload releases its reservation");
        let reservation = hub.admit_model_preview_part(PrincipalId([1; 16]), "upload", "part", 60, &[], Some(100), Instant::now()).unwrap();
        hub.clear_model_preview(PrincipalId([1; 16]), false, "upload", 2).unwrap();
        assert_eq!(hub.journal.lock().unwrap().disk, 60, "clearing a session cannot hide bytes still being uploaded");
        open_preview(&hub, 1, "upload");
        assert_eq!(hub.update_model_preview_part(reservation, spool.part("pmesh_late", &[1; 60]), 3), Err("model preview session not found"), "a late body cannot attach to a reopened session");
        assert_eq!(hub.journal.lock().unwrap().disk, 0, "failed commit releases its reservation too");
    }

    #[test]
    fn preview_supersedes_swap_atomically_or_release_before_eviction() {
        let spool = TestSpool::new();
        let hub = EventHub::new([15; 8], 128, 1, PreviewBudget::Bytes(u64::MAX), false);
        open_preview(&hub, 1, "upload"); open_preview(&hub, 2, "other");
        draft(&hub, &spool, 1, "upload", "old", 40);
        draft(&hub, &spool, 2, "other", "part", 40);
        let cursor = hub.model_preview_snapshot(None, &Default::default()).cursor;
        let reservation = hub.admit_model_preview_part(PrincipalId([1; 16]), "upload", "new", 40, &["old".into()], Some(100), Instant::now()).unwrap();
        let events = hub.poll_after(cursor, None, 128, &Default::default()).events;
        assert_eq!(events.len(), 1, "superseded parts alone free enough space");
        let delta = events[0].model_preview.as_ref().unwrap();
        assert_eq!((&*delta.session, &delta.removed), ("upload", &vec!["old".to_string()]), "the uploader's superseded part is removed first");
        assert!(delta.parts.is_empty(), "early removal shows the cage while the body arrives");
        hub.update_model_preview_part(reservation, spool.part("pmesh_new", &[1; 40]), 1).unwrap();
        let cursor = hub.model_preview_snapshot(None, &Default::default()).cursor;
        let reservation = hub.admit_model_preview_part(PrincipalId([1; 16]), "upload", "next", 10, &["new".into()], Some(100), Instant::now()).unwrap();
        assert!(hub.poll_after(cursor, None, 128, &Default::default()).events.is_empty(), "without pressure the old mesh stays through the upload");
        hub.update_model_preview_part(reservation, spool.part("pmesh_next", &[2; 10]), 2).unwrap();
        let events = hub.poll_after(cursor, None, 128, &Default::default()).events;
        assert_eq!(events.len(), 1, "success emits one atomic swap event");
        let delta = events[0].model_preview.as_ref().unwrap();
        assert_eq!(delta.parts[0].name, "next", "swap carries the new part");
        assert_eq!(delta.removed, ["new"], "swap removes the still-existing superseded part");
        assert_eq!(hub.journal.lock().unwrap().disk, 50, "success replaces reservation and superseded bytes with the new part length");
    }

    #[test]
    fn preview_metadata_reclaims_the_requesters_oldest_session() {
        let mut hub = EventHub::new([16; 8], 128, 1, PreviewBudget::Bytes(u64::MAX), false);
        for (owner, session) in [(1, "old"), (1, "new"), (2, "peer")] { open_preview(&hub, owner, session); }
        let now = Instant::now();
        let budget = { let mut j = hub.journal.lock().unwrap();
            j.previews.iter_mut().find(|p| p.session == "old").unwrap().last_activity = now - Duration::from_secs(2);
            j.meta as u64
        };
        hub.preview_meta = Some(budget);
        let cursor = hub.model_preview_snapshot(None, &Default::default()).cursor;
        hub.update_model_preview_metadata(PrincipalId([1; 16]), "new", Some("larger cage".into()), vec![], vec![], 1).unwrap();
        let events = hub.poll_after(cursor, None, 128, &Default::default()).events;
        assert_eq!(events.len(), 2, "metadata lapse precedes the admitted program change");
        assert_eq!(events[0].kind, KIND_MODEL_PREVIEW_CLEAR, "metadata pressure uses the ordinary clear event");
        assert_eq!(events[0].model_preview.as_ref().unwrap().session, "old", "the over-share author's oldest session lapses");
        assert!(hub.model_preview_namespace("peer").is_some(), "the peer within its share survives");
        let j = hub.journal.lock().unwrap();
        assert_eq!(j.meta, j.previews.iter().map(ModelPreviewSession::metadata_charge).sum::<u128>(), "metadata running sum matches live sessions after lapse and change");
    }

    #[test]
    fn preview_metadata_refusal_preserves_the_session_and_releases_the_upload() {
        let mut hub = EventHub::new([18; 8], 128, 1, PreviewBudget::Bytes(0), false);
        assert_eq!(hub.open_model_preview(PrincipalId([1; 16]), "gen".into(), "gen/drafts/one".into(), "one".into(), "cage".into(), None, 0),
            Err(PREVIEW_META_FULL), "an open beyond metadata capacity reports space, not a count");
        assert_eq!(hub.journal.lock().unwrap().meta, 0, "a refused open leaves the book empty");
        hub.preview_meta = None;
        open_preview(&hub, 1, "one");
        let charge = hub.journal.lock().unwrap().meta;
        hub.preview_meta = Some(charge as u64);
        let cursor = hub.model_preview_snapshot(None, &Default::default()).cursor;
        let spool = TestSpool::new();
        let reservation = hub.admit_model_preview_part(PrincipalId([1; 16]), "one", "part", 1, &[], Some(10), Instant::now()).unwrap();
        assert_eq!(hub.update_model_preview_part(reservation, spool.part("pmesh_one", &[1]), 1), Err(PREVIEW_META_FULL), "new parts must fit the metadata book before being applied");
        assert_eq!(hub.update_model_preview_metadata(PrincipalId([1; 16]), "one", Some("larger cage".into()), vec![], vec![], 2), Err(PREVIEW_META_FULL), "a program growth cannot bypass metadata admission");
        assert!(hub.poll_after(cursor, None, 128, &Default::default()).events.is_empty(), "a refused edit emits no partial update");
        let j = hub.journal.lock().unwrap();
        assert_eq!((j.disk, j.meta), (0, charge), "metadata refusal releases the disk reservation and preserves metadata charges");
        assert_eq!(j.previews[0].program, "cage", "the original program survives refusal");
        assert!(j.previews[0].parts.is_empty(), "the refused part never becomes visible");
    }

    #[test]
    fn preview_part_admission_has_no_count_cap() {
        let spool = TestSpool::new();
        let hub = EventHub::new([17; 8], 128, 1, PreviewBudget::Bytes(u64::MAX), false);
        open_preview(&hub, 1, "many");
        for n in 0..40 { draft(&hub, &spool, 1, "many", &format!("part-{n}"), 1); }
        let j = hub.journal.lock().unwrap();
        assert_eq!(j.previews[0].parts.len(), 40, "parts are charged by bytes, never refused for a count");
        assert_eq!(j.disk, 40, "all live parts are charged");
        assert_eq!(j.meta, j.previews[0].metadata_charge(), "each part's strings enter the metadata book");
    }

    #[test]
    fn preview_bootstrap_is_complete_and_its_cursor_covers_every_snapshot() {
        let hub = EventHub::new([7; 8], 2, 1, PreviewBudget::Bytes(u64::MAX), false);
        for id in 0..40 {
            hub.open_model_preview(PrincipalId([(id / 4 + 1) as u8; 16]), "gen".into(), format!("gen/drafts/d{id}"), format!("edit-{id}"), "cage".into(), None, 0).unwrap();
        }
        // The journal has evicted most opens; the independent active snapshot
        // still covers every cage and does not require a first mesh upload.
        let snapshot = hub.model_preview_snapshot(None, &Default::default());
        assert_eq!(snapshot.events.len(), 40, "40 sessions from ten owners are admitted");
        for event in &snapshot.events {
            assert!(event.seq <= snapshot.cursor.seq);
            let preview = event.model_preview.as_ref().unwrap();
            assert!(preview.open);
            assert!(preview.parts.is_empty());
            assert_eq!(preview.program.as_deref(), Some("cage"));
        }
        assert!(hub.model_preview_snapshot(Some("video"), &Default::default()).events.is_empty());
        hub.clear_model_preview(PrincipalId([1; 16]), false, "edit-0", 1).unwrap();
        let delta = hub.poll_after(snapshot.cursor, None, 1, &Default::default());
        assert_eq!(delta.events.len(), 1);
        assert_eq!(delta.events[0].kind, KIND_MODEL_PREVIEW_CLEAR);
        assert_eq!(hub.model_preview_snapshot(None, &Default::default()).events.len(), 40 - 1);
    }

    #[test]
    fn preview_lease_uses_monotonic_activity_and_heartbeat_emits_nothing() {
        let hub = EventHub::new([8; 8], 16, 1, PreviewBudget::Bytes(u64::MAX), false);
        hub.open_model_preview(PrincipalId([1; 16]), "gen".into(), "gen/drafts/car".into(), "edit-a".into(), "cage".into(), None, u64::MAX).unwrap();
        let before = hub.model_preview_snapshot(None, &Default::default()).cursor;
        let old = Instant::now() - Duration::from_secs(90);
        hub.journal.lock().unwrap().previews[0].last_activity = old;
        hub.update_model_preview_metadata(PrincipalId([1; 16]), "edit-a", None, vec![], vec![], 0).unwrap();
        assert_eq!(hub.model_preview_snapshot(None, &Default::default()).cursor, before);
        assert_eq!(hub.expire_model_previews_at(old + MODEL_PREVIEW_LEASE, 0), 0);
        let renewed = hub.journal.lock().unwrap().previews[0].last_activity;
        assert_eq!(hub.expire_model_previews_at(renewed + MODEL_PREVIEW_LEASE - Duration::from_nanos(1), 0), 0);
        assert_eq!(hub.expire_model_previews_at(renewed + MODEL_PREVIEW_LEASE, 0), 1);
        assert_eq!(hub.expire_model_previews_at(renewed + MODEL_PREVIEW_LEASE, 0), 0);
        let cleared = hub.poll_after(before, None, 10, &Default::default());
        assert_eq!(cleared.events.len(), 1);
        assert_eq!(cleared.events[0].kind, KIND_MODEL_PREVIEW_CLEAR);
        assert!(hub.model_preview_snapshot(None, &Default::default()).events.is_empty());
        assert!(hub.update_model_preview_metadata(PrincipalId([1; 16]), "edit-a", None, vec![], vec![], 0).is_err());
    }

    #[test]
    fn preview_expiry_releases_meshes_even_with_journal_and_snapshot_alive() {
        let spool = TestSpool::new();
        let audiences = BTreeSet::from(["A".to_string()]);
        let hub = EventHub::new([9; 8], 16, 1, PreviewBudget::Bytes(u64::MAX), false);
        hub.open_model_preview(PrincipalId([1; 16]), "gen".into(), "gen/drafts/car".into(), "edit-a".into(), "cage".into(), Some("A".into()), 0).unwrap();
        let file = spool.part("pmesh_old", b"draft mesh");
        let weak = Arc::downgrade(&file);
        put_preview(&hub, PrincipalId([1; 16]), "edit-a", "draft-1".into(), file, 1).unwrap();
        let snapshot = hub.model_preview_snapshot(None, &audiences);
        assert!(snapshot.events[0].model_preview.as_ref().unwrap().parts[0].file.is_none());
        let old_event = hub.poll_after(EventCursor { epoch: [9; 8], seq: 0 }, None, 10, &audiences);
        assert!(old_event.events[1].model_preview.as_ref().unwrap().parts[0].file.is_none());
        let activity = hub.journal.lock().unwrap().previews[0].last_activity;
        assert_eq!(hub.expire_model_previews_at(activity + MODEL_PREVIEW_LEASE, 2), 1);
        let clear = hub.poll_after(old_event.cursor, None, 10, &audiences);
        assert_eq!(clear.events.len(), 1);
        assert_eq!(clear.events[0].kind, KIND_MODEL_PREVIEW_CLEAR);
        assert_eq!(clear.events[0].model_preview.as_ref().unwrap().audience.as_deref(), Some("A"));
        assert!(hub.poll_after(old_event.cursor, None, 10, &BTreeSet::new()).events.is_empty());
        assert!(weak.upgrade().is_none(), "journal and snapshot must not retain draft files");
        assert!(hub.model_preview_mesh("pmesh_old", None).is_none());
        assert!(hub.model_preview_namespace("edit-a").is_none());
        spool.assert_empty();
    }

    #[test]
    fn upload_progress_keeps_only_its_owners_live_sessions_without_events() {
        let spool = TestSpool::new();
        let hub = EventHub::new([11; 8], 16, 1, PreviewBudget::Bytes(u64::MAX), false);
        let owner = PrincipalId([1; 16]);
        for (session, principal) in [("upload", owner), ("other", owner), ("foreign", PrincipalId([2; 16]))] {
            hub.open_model_preview(principal, "gen".into(), format!("gen/drafts/{session}"), session.into(), "cage".into(), None, 0).unwrap();
        }
        let start = Instant::now();
        for preview in &mut hub.journal.lock().unwrap().previews {
            preview.last_activity = start;
        }
        let cursor = hub.tail_cursor();
        // Drive the upload's refresh directly: waiting through two real leases
        // would test the same clock rule while making this test take minutes.
        for seconds in [60, 120, 180, 240, 300] {
            let now = start + Duration::from_secs(seconds);
            let before_refresh = hub.tail_cursor();
            hub.refresh_model_preview_owner(owner, now);
            assert_eq!(hub.tail_cursor(), before_refresh);
            assert_eq!(hub.expire_model_previews_at(now, seconds), usize::from(seconds == 120));
            assert!(hub.model_preview_namespace("upload").is_some());
            assert!(hub.model_preview_namespace("other").is_some());
        }
        let events = hub.poll_after(cursor, None, 16, &Default::default());
        assert_eq!(events.events.len(), 1, "progress must not publish events");
        assert_eq!(events.events[0].model_preview.as_ref().unwrap().session, "foreign");
        put_preview(&hub, PrincipalId([1; 16]), "upload", "body".into(), spool.part("pmesh_done", b"done"), 301).unwrap();
        assert!(hub.model_preview_mesh("pmesh_done", None).is_some());
        let expired = start + Duration::from_secs(300) + MODEL_PREVIEW_LEASE;
        assert_eq!(hub.expire_model_previews_at(expired, 420), 2);
        hub.refresh_model_preview_owner(owner, expired);
        assert!(hub.model_preview_snapshot(None, &Default::default()).events.is_empty());
        spool.assert_empty();
    }

    #[test]
    fn invalid_preview_metadata_is_atomic_and_does_not_refresh_lease() {
        let spool = TestSpool::new();
        let hub = EventHub::new([10; 8], 16, 1, PreviewBudget::Bytes(u64::MAX), false);
        hub.open_model_preview(PrincipalId([1; 16]), "gen".into(), "gen/csg/car".into(), "edit-a".into(), "original".into(), None, 0).unwrap();
        put_preview(&hub, PrincipalId([1; 16]), "edit-a", "body".into(), spool.part("pmesh_body", b"body"), 1).unwrap();
        let before = hub.model_preview_snapshot(None, &Default::default());
        let activity = hub.journal.lock().unwrap().previews[0].last_activity;
        assert!(hub.update_model_preview_metadata(PrincipalId([1; 16]), "edit-a", Some("changed".into()), vec!["shell".into()], vec![
            ModelPreviewRename { from: "body".into(), to: "shell".into() },
            ModelPreviewRename { from: "missing".into(), to: "other".into() },
        ], 2).is_err());
        let after = hub.model_preview_snapshot(None, &Default::default());
        assert_eq!(before.cursor, after.cursor);
        let preview = after.events[0].model_preview.as_ref().unwrap();
        assert_eq!(preview.program.as_deref(), Some("original"));
        assert_eq!(preview.parts[0].name, "body");
        assert_eq!(hub.journal.lock().unwrap().previews[0].last_activity, activity);
    }

    #[test]
    fn wait_wakes_on_publish_and_respects_shutdown_and_cap() {
        let hub = Arc::new(EventHub::new([5; 8], 16, 1, PreviewBudget::Bytes(u64::MAX), false));
        // Waker thread publishes shortly after the wait parks.
        let hub_pub = hub.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            hub_pub.publish(body(KIND_ALIAS_SET, "ns", None));
        });
        let woke = hub.wait_beyond(0, Instant::now() + Duration::from_secs(5));
        assert!(woke);
        t.join().unwrap();

        // Cap 1: a first parked waiter makes a second degrade immediately.
        let hub_a = hub.clone();
        let parked = std::thread::spawn(move || {
            hub_a.wait_beyond(1, Instant::now() + Duration::from_millis(600))
        });
        std::thread::sleep(Duration::from_millis(100));
        let start = Instant::now();
        let woke = hub.wait_beyond(1, Instant::now() + Duration::from_secs(5));
        assert!(!woke);
        assert!(start.elapsed() < Duration::from_millis(400), "over-cap wait must not park");
        let _ = parked.join().unwrap();

        // Shutdown wakes parked waiters promptly.
        let hub_b = hub.clone();
        let parked = std::thread::spawn(move || {
            hub_b.wait_beyond(1, Instant::now() + Duration::from_secs(30))
        });
        std::thread::sleep(Duration::from_millis(100));
        let start = Instant::now();
        hub.shutdown();
        assert!(!parked.join().unwrap());
        assert!(start.elapsed() < Duration::from_secs(2));
        // After shutdown nothing waits.
        assert!(!hub.wait_beyond(0, Instant::now() + Duration::from_secs(30)));
    }
}
