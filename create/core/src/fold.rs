//! The fold: one deterministic replay of the log into what the app shows. Events are applied in
//! one canonical order that respects the graph: `(height, author, id)`, where an event's height is
//! one more than its highest parent's (derived, never carried, so nobody can jump it). Every
//! machine with the same events gets the same state, whatever order they arrived in. No wall
//! clock, no hash-map order and no local setting is read here.
//! An author's events form a tree; a key with two successors of one event is cut there.
//! An event's own predecessor is its cited own event greatest in canonical (height, id) order.
//!
//! A key is spent by its first event in a town. A control event may fail for what its author
//! is or for its own structure, never for the state of the key or seat it names.
//!
//! Session facts and transcript lines are statements about their signer, folded raw and interpreted
//! on read. The timeline orders entries by their anchor event, never by wall time.
//!
//! An owner-addressed inbox turn permits a filtered timeline read. Calls expose local
//! transcript availability. The reader checks seat and actor before returning chat.

pub use crate::entity::{backlinks, links_from, project_entities, Entity, FieldState};
use crate::doc::{attr, checkbox, BreakRef, DocState, TaskEffects, TextOp};
use crate::entity::{field, kind};
use crate::event::{Actor, Chat, ChatAgent, Content, DeclineWhy, Event, Id, Key, LeaveWhy, Member,
    normalize_copy_refs, CopySource, Failure, JobId, Service, SessionId, SessionMode, SessionOp, Value, Via};
use crate::key::PublicKey;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub colour: u32,
    pub writer: Id,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TownPolicy {
    #[default]
    Members,
    /// A visible founder-critical policy has a frontier value this build cannot read.
    Unreadable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordMeta {
    pub author: PublicKey,
    pub actor: Actor,
    pub via: Via,
    pub kind: u16,
    pub void: bool,
    pub readable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceRecord {
    Request { job: JobId, session: SessionId },
    Done { job: JobId },
    Failed { job: JobId },
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DerivedTaskMeta {
    pub task: Id,
    pub creator: Id,
    pub doc: Id,
    pub at: BreakRef,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceJobMeta {
    pub session: SessionId,
    pub project: Option<Id>,
    pub requester: PublicKey,
    pub provider: PublicKey,
    pub request: Id,
    pub report_writer: Option<Id>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatMsg {
    pub id: Id,
    pub author: PublicKey,
    pub actor: Actor,
    pub via: Via,
    pub wall_ms: u64,
    pub chat: Chat,
    pub author_agent: Option<Id>,
    pub to_agent: Option<Id>,
}

impl ChatMsg {
    /// True when this human town-chat line explicitly mentions `seat`.
    pub fn mentions_seat(&self, seat: Id) -> bool {
        self.chat.mentions.binary_search(&seat).is_ok()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    pub owner: PublicKey,
    /// Immutable backend/provider label from `AgentAdded` (e.g. "Claude Code").
    pub name: String,
    pub kind: String,
    /// Human-chosen display name from the latest valid `AgentRenamed`, or `None` if unset or cleared.
    pub custom_name: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry { Chat(Id), Session(SessionId), Clip(Id), Member(Id) }

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Session {
    pub open: Option<Opened>,
    pub(crate) joins: Vec<(PublicKey, Id)>,
    pub(crate) leaves: Vec<(PublicKey, Id, Option<u32>, LeaveWhy)>,
    pub(crate) declines: Vec<(PublicKey, Id, DeclineWhy)>,
    shared: Vec<TranscriptShare>,
    // (at_ms, author, event, line index). A line takes at least 28 bytes on the wire and a
    // message is under 4 GiB, so an event's line index always fits a u32.
    pub(crate) said: Vec<(u32, PublicKey, Id, u32)>,
}

/// A person's signed transcript send, naming its recipient and opening request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptShare {
    pub author: PublicKey,
    pub agent: Id,
    pub line: Id,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opened {
    pub anchor: Id,
    pub opener: PublicKey,
    pub mode: SessionMode,
    pub to: Vec<Id>,
    pub town_wide: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Said<'a> {
    pub at_ms: u32,
    pub dur_ms: u32,
    pub author: PublicKey,
    pub event: Id,
    pub index: u32,
    pub text: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seen { Opener, Rung, Other }

impl Opened {
    pub(crate) fn seen(&self, opener_seat: Option<Id>, seat: Option<Id>) -> Seen {
        match seat {
            Some(_) if seat == opener_seat => Seen::Opener,
            Some(id) if self.to.contains(&id) => Seen::Rung,
            _ => Seen::Other,
        }
    }

    pub(crate) fn admits(&self, opener_seat: Option<Id>, seat: Option<Id>) -> bool {
        seat.is_some() && (self.town_wide || self.seen(opener_seat, seat) != Seen::Other)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Answered { seats: Vec<Id>, secs: Option<u32> },
    Declined { seats: Vec<Id>, busy: bool },
    Unanswered,
    Pending,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSummary {
    pub opener: PublicKey,
    pub opener_seat: Option<Id>,
    pub mode: SessionMode,
    pub rung: Vec<Id>,
    pub town_wide: bool,
    pub viewer: Seen,
    pub outcome: Outcome,
    pub lines: usize,
    /// Every participant has departed (or, for a share, the sharer left): the session reads in the past tense.
    pub ended: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobState { Asked, Working, Done(String), Failed(Failure) }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub job: JobId,
    pub service: Service,
    pub requester: PublicKey,
    pub provider: PublicKey,
    pub project: Option<Id>,
    pub request: Id,
    pub state: JobState,
}

// Keep references to signed, non-void records. A session's Open may arrive after its
// Request. Only the first eligible Request for a job owns its provider reports.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ServiceFacts {
    offers: BTreeMap<PublicKey, Id>,
    sessions: BTreeMap<SessionId, BTreeSet<JobId>>,
    histories: BTreeMap<JobId, Vec<Id>>,
}

type Order = (u64, PublicKey, Id);
type Replaced = BTreeMap<(Id, Key), Option<FieldState>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ForkPoint {
    Start,
    After(Id),
}

/// A removal voided because its author forked at or below it, leaving `target_device` active again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReinstatedDevice {
    pub forked_device: PublicKey,
    pub target_device: PublicKey,
    pub removal_event: Id,
}

/// Local arrival boundary and proof events for a bounded final exchange between two devices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalBoundary {
    /// Arrival index of the concluding removal or second fork sibling.
    pub at: usize,
    /// One counted `Remove` event id, or the two fork siblings `[first, second]` in arrival order.
    pub evidence: Vec<Id>,
}

impl FinalBoundary {
    pub fn closure(&self) -> Option<FinalClosure> {
        FinalClosure::from_evidence(&self.evidence)
    }
}

/// Proof of a completed final exchange: one counted removal or two distinct fork siblings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FinalClosure {
    Removal(Id),
    Fork([Id; 2]),
}

impl FinalClosure {
    pub fn fork(a: Id, b: Id) -> Option<Self> {
        if a == b { return None }
        Some(Self::Fork(if a < b { [a, b] } else { [b, a] }))
    }

    pub fn from_evidence(ids: &[Id]) -> Option<Self> {
        match *ids {
            [id] => Some(Self::Removal(id)),
            [a, b] => Self::fork(a, b),
            _ => None,
        }
    }

    pub fn normalized(self) -> Option<Self> {
        match self {
            Self::Removal(_) => Some(self),
            Self::Fork([a, b]) => Self::fork(a, b),
        }
    }
}

const FORKED: &str = "forked its history";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Reading { Full, SkippedTag, Unknown, UnknownCritical }

// Use the decoded content already needed for the indexes. Reading is a property of the
// signed bytes, independent of arrival order, membership decisions and board replay.
fn reading(event: &Event, content: Option<&Content>) -> Reading {
    match (event.content(), content) {
        (_, Some(Content::Town { policy, tier })) if *policy != GROUP_POLICY || *tier != 0 => Reading::UnknownCritical,
        (Some(bytes), Some(content)) if content.encode() != bytes => {
            if event.header.critical { Reading::UnknownCritical } else { Reading::SkippedTag }
        }
        (_, Some(_)) => Reading::Full,
        _ if event.redacted() && !event.header.has_unknown_extensions() => Reading::Full,
        _ if event.header.critical => Reading::UnknownCritical,
        _ => Reading::Unknown,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Record {
    order: Order,
    kind: u16,
    critical: bool,
    parents: Vec<Id>,
    // Powers of two along the highest-parent chain accelerate ordinary typing histories.
    jumps: Vec<Id>,
    content: Option<Content>,
    actor: Actor,
    via: Via,
    wall_ms: u64,
    control: bool,
    structurally_void: bool,
    erased_chat: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Timeline {
    events: Vec<Order>,
    // Keep two recent snapshots. Older history can always replay from the empty document.
    checkpoints: Vec<(usize, Arc<DocState>)>,
}

const CHECKPOINT_INTERVAL: usize = 64;

// Heights strictly increase on the author's surviving prefix, so cutoffs and the first
// invalid operation are enough to classify ordinary writes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Seats {
    seats: BTreeMap<Id, Seat>,
    admitted: BTreeMap<PublicKey, Admission>,
    offers: BTreeMap<Id, Offer>,
    invalid_from: HashMap<PublicKey, u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Admission {
    seat: Id,
    grant: Id,
    height: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Offer {
    seat: Id,
    subject: PublicKey,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Seat {
    role: u8,
    devices: BTreeSet<PublicKey>,
    invite_parent: Option<Id>,
    first_device: PublicKey,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Removal { Counted, Moot }

// At equal height every signed control sorts before every fork cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum MembershipOp {
    Event(PublicKey, Id),
    Fork(PublicKey),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Membership {
    seats: Seats,
    controls: Vec<(u64, MembershipOp)>,
    forks: BTreeMap<PublicKey, (u64, ForkPoint)>,
    cutoffs: HashMap<PublicKey, u64>,
    // A canonical decision survives subsequent cutoffs, including the remover's own.
    removals: HashMap<Id, Result<Removal, &'static str>>,
    latest_control: HashMap<PublicKey, u64>,
    reasons: HashMap<Id, &'static str>,
}

const GROUP_POLICY: u16 = 2;

impl Seats {
    fn active_seat(&self, device: &PublicKey) -> Option<Id> {
        let seat = self.admitted.get(device)?.seat;
        self.seats.get(&seat)?.devices.contains(device).then_some(seat)
    }

    fn open(&self, id: &Id) -> bool {
        let Some(seat) = self.seats.get(id) else { return false };
        // Only the founder or the Invite's subject can join an empty seat first.
        // Every later AddDevice requires a device already active there.
        !seat.devices.is_empty() || self.admitted.get(&seat.first_device).is_none_or(|a| a.seat != *id)
    }

    fn is_strict_descendant_seat(&self, ancestor_seat: Id, seat: Id) -> bool {
        if seat == ancestor_seat { return false }
        let mut current = seat;
        for _ in 0..self.seats.len() {
            let Some(parent) = self.seats.get(&current).and_then(|s| s.invite_parent) else {
                return false;
            };
            if parent == ancestor_seat { return true }
            current = parent;
        }
        false
    }

    /// Structural removal authority (historical seat/vouch topology only; does not require
    /// `author` or `device` to be currently active so fork-reinstatement checks can reuse it).
    fn has_removal_authority(
        &self,
        author: &PublicKey,
        seat: Id,
        device: &PublicKey,
        records: &BTreeMap<Id, Arc<Record>>,
    ) -> bool {
        let Some(own) = self.admitted.get(author).map(|a| a.seat) else { return false };
        let Some(mut cur_admission) = self.admitted.get(device) else { return false };
        if cur_admission.seat != seat { return false }
        if seat != own {
            return self.is_strict_descendant_seat(own, seat);
        }
        // Same seat: self-removal or transitive AddDevice vouch-tree descendant of `author`.
        let mut cur = *device;
        while cur != *author {
            let Some(grant) = records.get(&cur_admission.grant) else { return false };
            if !matches!(grant.content, Some(Content::Member(Member::AddDevice { .. }))) {
                return false;
            }
            let voucher = grant.order.1;
            let Some(next) = self.admitted.get(&voucher) else { return false };
            if next.height >= cur_admission.height { return false }
            cur = voucher;
            cur_admission = next;
        }
        true
    }

    fn judge_removal(
        &self,
        r: &Record,
        records: &BTreeMap<Id, Arc<Record>>,
        seat: Id,
        device: PublicKey,
        last: Id,
    ) -> Result<Removal, &'static str> {
        let author = r.order.1;
        let own = self.active_seat(&author).ok_or("not an active member")?;
        if records.get(&last).is_none_or(|p| p.order.1 != device) || !in_past(records, r, last) {
            return Err("invalid removal cutoff");
        }
        if self.admitted.get(&device).is_none_or(|a| a.seat != seat) {
            return Ok(Removal::Moot);
        }
        if !self.has_removal_authority(&author, seat, &device, records) {
            return Err(if seat == own { "may not remove this device" } else { "may not remove this seat" });
        }
        Ok(Removal::Counted)
    }

    fn apply(&mut self, r: &Record, records: &BTreeMap<Id, Arc<Record>>) -> Result<(), &'static str> {
        let (height, author, id) = r.order;
        match &r.content {
            Some(Content::Town { policy: GROUP_POLICY, tier: 0 }) => {
                self.seats.insert(id, Seat { role: 0, devices: BTreeSet::from([author]), invite_parent: None, first_device: author });
                self.admitted.insert(author, Admission { seat: id, grant: id, height });
            }
            Some(Content::Town { .. }) => {}
            Some(Content::Member(Member::Accept { entry })) => {
                let offer = self.offers.get(entry).ok_or("no admission for this device")?;
                if author != offer.subject || !self.open(&offer.seat) || self.admitted.contains_key(&author)
                    || r.parents.iter().any(|p| records[p].order.1 == author)
                    || !in_past(records, r, *entry) {
                    return Err("invalid admission")
                }
                self.seats.get_mut(&offer.seat).ok_or("unknown seat")?.devices.insert(author);
                self.admitted.insert(author, Admission { seat: offer.seat, grant: *entry, height });
            }
            Some(Content::Member(Member::Invite { subject, role })) => {
                let parent = self.active_seat(&author).ok_or("not an active member")?;
                if *role != 0 { return Err("unknown member role") }
                self.seats.insert(id, Seat { role: *role, devices: BTreeSet::new(), invite_parent: Some(parent), first_device: *subject });
                self.offers.insert(id, Offer { seat: id, subject: *subject });
            }
            Some(Content::Member(Member::AddDevice { seat, device })) => {
                let own = self.active_seat(&author).ok_or("not an active member")?;
                if *seat != own { return Err("only the seat's own devices add devices") }
                self.offers.insert(id, Offer { seat: *seat, subject: *device });
            }
            _ => return Err("unknown membership operation"),
        }
        Ok(())
    }
}

impl Membership {
    fn fork_reason(&self, r: &Record) -> Option<&'static str> {
        let &(cut, point) = self.forks.get(&r.order.1)?;
        // A parentless Accept has height zero too, but is still a successor of Start.
        (r.order.0 > cut || (point == ForkPoint::Start && !matches!(r.content, Some(Content::Town { .. }))))
            .then_some(FORKED)
    }

    fn prior_reason(&self, r: &Record) -> Option<&'static str> {
        let (height, author, _) = r.order;
        if let Some(why) = self.fork_reason(r) { return Some(why) }
        if self.cutoffs.get(&author).is_some_and(|cut| height > *cut) { return Some("after removal") }
        if self.seats.invalid_from.get(&author).is_some_and(|bad| height > *bad) { return Some("previous event was void") }
        None
    }

    fn ordinary(&self, r: &Record) -> Option<&'static str> {
        let (height, author, _) = r.order;
        self.prior_reason(r).or_else(|| {
            self.seats.admitted.get(&author).is_none_or(|a| height <= a.height).then_some("not a member")
        })
    }

    fn remove_device(&mut self, device: PublicKey, cut: u64) -> Option<(PublicKey, u64)> {
        if let Some(seat) = self.seats.admitted.get(&device).and_then(|a| self.seats.seats.get_mut(&a.seat)) {
            seat.devices.remove(&device);
        }
        let old = self.cutoffs.entry(device).or_insert(u64::MAX);
        if cut >= *old { return None }
        *old = cut;
        Some((device, cut))
    }

    fn control(&mut self, index: usize, records: &BTreeMap<Id, Arc<Record>>) -> Option<(PublicKey, u64)> {
        let (height, op) = self.controls[index];
        let r = match op {
            MembershipOp::Event(_, id) => &records[&id],
            MembershipOp::Fork(key) => return self.remove_device(key, height),
        };
        let (height, author, id) = r.order;
        let prior = self.prior_reason(r);
        let reason = if let Some(Content::Member(Member::Remove { seat, device, last })) = r.content {
            let decision = self.removals.entry(id).or_insert_with(|| match prior {
                Some(why) => Err(why),
                None => self.seats.judge_removal(r, records, seat, device, last),
            });
            match *decision {
                Ok(Removal::Moot) => None,
                Ok(Removal::Counted) => {
                    return self.remove_device(device, records[&last].order.0)
                }
                Err(why) => Some(prior.unwrap_or(why)),
            }
        } else {
            prior.or_else(|| self.seats.apply(r, records).err())
        };
        if let Some(why) = reason {
            self.reasons.insert(id, why);
            self.seats.invalid_from.entry(author).or_insert(height);
        }
        None
    }

    // Ordinary events never enter this index. Replay a membership prefix only when a
    // cutoff crosses an earlier membership operation: it may retract a grant and its dependent admissions.
    fn decide(&mut self, index: usize, records: &BTreeMap<Id, Arc<Record>>) -> Option<(PublicKey, u64)> {
        let cutoff = self.control(index, records);
        if cutoff.is_some_and(|(key, cut)| self.latest_control.get(&key).is_some_and(|last| *last > cut)) {
            self.seats = Seats::default();
            self.reasons.clear();
            for i in 0..=index { self.control(i, records); }
        }
        if let (height, MembershipOp::Event(author, _)) = self.controls[index] {
            self.latest_control.insert(author, height);
        }
        cutoff
    }

    fn replay(&mut self, records: &BTreeMap<Id, Arc<Record>>) {
        let mut controls = std::mem::take(&mut self.controls);
        let forks = std::mem::take(&mut self.forks);
        if !forks.is_empty() {
            controls.retain(|(_, op)| matches!(op, MembershipOp::Event(..)));
            controls.extend(forks.iter().map(|(&key, &(height, _))| (height, MembershipOp::Fork(key))));
            controls.sort_unstable();
        }
        *self = Self { controls, forks, ..Self::default() };
        for i in 0..self.controls.len() { self.decide(i, records); }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    membership: Membership,
    /// Verified events excluded from the projection, with the reason for each exclusion.
    pub void_events: HashMap<Id, &'static str>,
    pub unknown_policy: Option<u16>,
    pub town_policy: TownPolicy,
    // Maximal policy writes, keyed by signing event and town entity.
    policy_writes: BTreeMap<(Id, Id), Value>,
    copied_from: BTreeMap<(Id, Id, Key), Vec<CopySource>>,
    pub docs: BTreeMap<Id, DocState>,
    records: BTreeMap<Id, Arc<Record>>,
    unreadable_session_facts: BTreeSet<Id>,
    // Keep the least child id as a deterministic representative. All children stay in records.
    own_children: BTreeMap<(PublicKey, ForkPoint), Id>,
    latest_height: HashMap<PublicKey, u64>,
    order: Vec<Order>,
    entity_writes: BTreeMap<Id, BTreeSet<Order>>,
    // Non-void critical founder-seat Entity/Set records writing field::POLICY, per entity.
    policy_sets: BTreeMap<Id, BTreeSet<Order>>,
    pending: BTreeMap<Id, Event>,
    children: BTreeMap<Id, BTreeSet<Id>>,
    scopes: BTreeMap<Id, Id>,
    timelines: BTreeMap<Id, Timeline>,
    undo_children: BTreeMap<Id, BTreeSet<Order>>,
    tick_writes: BTreeMap<Id, BTreeMap<Id, (Value, BreakRef, Id)>>,
    statuses: BTreeMap<Id, BTreeMap<Order, Value>>,
    possible_links: BTreeMap<Id, BTreeSet<Id>>,
    linked_docs: BTreeMap<Id, BTreeSet<Id>>,
    conversions: BTreeMap<Id, (Id, BreakRef, Id)>,
    derived_task_creators: BTreeMap<Id, Id>,
    numbers: BTreeMap<PublicKey, u64>,
    groups: BTreeMap<Id, Replaced>,
    cleared: BTreeMap<(Id, Key), Id>,
    pub profiles: BTreeMap<PublicKey, Profile>,
    // Includes unreadable and void records that cannot appear in the displayed profiles.
    latest_profiles: BTreeMap<PublicKey, (bool, Order, Id)>,
    /// In the canonical order.
    pub chat: Vec<ChatMsg>,
    pub sessions: BTreeMap<SessionId, Session>,
    pub timeline: Vec<Entry>,
    services: ServiceFacts,
    pub agents: BTreeMap<Id, Agent>,
    /// Number of ready chat, retract, agent and membership records. Unrelated edits cannot wake a wait.
    pub agent_inputs: usize,
    retracted: HashSet<Id>,
    chat_positions: BTreeMap<Id, usize>,
    mentions: BTreeMap<Id, Vec<Id>>,
    last_replies: BTreeMap<Id, Id>,
    pub entities: BTreeMap<Id, Entity>,
    /// Authors whose ORDER or Z field writes were refused for malformed values.
    pub invalid_keys: BTreeSet<PublicKey>,
    /// Each author's tasks count from one in canonical creation order, including tombstones.
    pub task_numbers: BTreeMap<Id, u64>,
    /// Events of kinds or versions this build doesn't know: kept, shown as "needs a newer Town".
    pub unknown: Vec<Id>,
    /// Unknown events marked critical: rules this build can't apply. Shown, so nobody trusts
    /// what they would have changed.
    pub unknown_critical: Vec<Id>,
    /// Decoded events whose canonical content omits or normalizes some signed bytes.
    pub(crate) partial: Vec<Id>,
    /// Events whose parents haven't arrived; folded once they have.
    pub waiting: Vec<Id>,
}

// Local to a read. A board listing follows each merge link at most once.
enum TaskPath<'a> {
    Visiting(usize),
    Resolved(Result<&'a Entity, String>),
}

/// The events in canonical order, and the ones still waiting for a parent.
pub fn canonical(events: &[Event]) -> (Vec<&Event>, Vec<Id>) {
    let by_id: HashMap<Id, &Event> = events.iter().map(|e| (e.id, e)).collect();
    let mut height: HashMap<Id, Option<u64>> = HashMap::new();
    fn height_of(id: &Id, by_id: &HashMap<Id, &Event>, memo: &mut HashMap<Id, Option<u64>>) -> Option<u64> {
        if let Some(h) = memo.get(id) {
            return *h;
        }
        // An explicit stack: a long history must not overflow the thread's stack.
        let mut stack = vec![*id];
        while let Some(top) = stack.last().copied() {
            if memo.contains_key(&top) {
                stack.pop();
                continue;
            }
            let Some(event) = by_id.get(&top) else {
                memo.insert(top, None);
                stack.pop();
                continue;
            };
            let pending: Vec<Id> = event.header.parents.iter().filter(|p| !memo.contains_key(*p)).copied().collect();
            if pending.is_empty() {
                let mut h = Some(0u64);
                for parent in &event.header.parents {
                    h = match (h, memo[parent]) {
                        (Some(h), Some(p)) => Some(h.max(p + 1)),
                        _ => None,
                    };
                }
                memo.insert(top, h);
                stack.pop();
            } else {
                stack.extend(pending);
            }
        }
        memo[id]
    }
    let mut ready = Vec::new();
    let mut waiting = Vec::new();
    for event in events {
        match height_of(&event.id, &by_id, &mut height) {
            Some(h) => ready.push((h, event)),
            None => waiting.push(event.id),
        }
    }
    ready.sort_by(|a, b| (a.0, &a.1.header.author, &a.1.id).cmp(&(b.0, &b.1.header.author, &b.1.id)));
    waiting.sort();
    (ready.into_iter().map(|(_, e)| e).collect(), waiting)
}

pub fn fold(events: &[Event]) -> State {
    let (ordered, waiting) = canonical(events);
    let mut state = State::default();
    state.index_prefix(ordered.into_iter(), None);
    state.rebuild_board();
    let missing: BTreeSet<_> = waiting.into_iter().collect();
    for event in events.iter().filter(|e| missing.contains(&e.id)) { state.receive(event); }
    state
}

/// Replay the first `n` ready events, including redacted and unknown events in the count.
pub fn fold_prefix(events: &[Event], n: usize) -> State {
    let mut state = State::default();
    state.index_prefix(canonical(events).0.into_iter().take(n), None);
    state.rebuild_board();
    state
}

fn in_past(records: &BTreeMap<Id, Arc<Record>>, event: &Record, id: Id) -> bool {
    let Some(target) = records.get(&id) else { return false };
    if target.order.0 >= event.order.0 { return false }
    let mut distance = event.order.0 - target.order.0;
    let mut ancestor = event;
    while distance != 0 {
        let bit = distance.trailing_zeros() as usize;
        ancestor = &records[&ancestor.jumps[bit]];
        distance &= distance - 1;
    }
    if ancestor.order.2 == id { return true }
    let mut stack = event.parents.clone();
    let mut visited = BTreeSet::new();
    while let Some(parent) = stack.pop() {
        if parent == id { return true }
        let p = &records[&parent];
        if p.order.0 > target.order.0 && visited.insert(parent) {
            stack.extend_from_slice(&p.parents);
        }
    }
    false
}

impl State {
    /// Append a verified event after the current tip. Also handles a home with pending parents.
    pub fn apply_last(&mut self, event: &Event) {
        self.receive(event);
    }

    /// Receive a verified event once. Children become eligible when their final parent arrives.
    pub fn receive(&mut self, event: &Event) {
        if self.records.contains_key(&event.id) || self.pending.contains_key(&event.id) { return }
        let mut missing = false;
        for parent in &event.header.parents {
            if !self.records.contains_key(parent) {
                missing = true;
                self.children.entry(*parent).or_default().insert(event.id);
            }
        }
        if missing {
            self.pending.insert(event.id, event.clone());
            let at = self.waiting.binary_search(&event.id).unwrap_err();
            self.waiting.insert(at, event.id);
            return
        }
        self.ready(event);
        let mut arrived = vec![event.id];
        while let Some(id) = arrived.pop() {
            for child in self.children.remove(&id).unwrap_or_default() {
                if self.pending.get(&child).is_some_and(|e| e.header.parents.iter().all(|p| self.records.contains_key(p))) {
                    let next = self.pending.remove(&child).unwrap();
                    self.waiting.remove(self.waiting.binary_search(&child).unwrap());
                    self.ready(&next);
                    arrived.push(child);
                }
            }
        }
    }

    fn own_predecessor(&self, author: PublicKey, parents: &[Id]) -> (u64, ForkPoint) {
        parents.iter().map(|id| self.records[id].order).filter(|o| o.1 == author).max()
            .map_or((0, ForkPoint::Start), |(height, _, id)| (height, ForkPoint::After(id)))
    }

    fn index_record(&mut self, event: &Event) -> (Arc<Record>, bool) {
        let h = &event.header;
        let height = h.parents.iter().map(|p| self.records[p].order.0 + 1).max().unwrap_or(0);
        let order = (height, h.author, event.id);
        let predecessor = self.own_predecessor(h.author, &h.parents);
        let first = self.own_children.entry((h.author, predecessor.1)).or_insert(event.id);
        let mut new_cut = false;
        if *first != event.id {
            *first = (*first).min(event.id);
            if self.membership.forks.get(&h.author).is_none_or(|old| predecessor < *old) {
                self.membership.forks.insert(h.author, predecessor);
                new_cut = true;
            }
        }
        let mut jumps = Vec::new();
        if let Some(&parent) = h.parents.iter().max_by_key(|p| self.records[*p].order) {
            jumps.push(parent);
            while let Some(&next) = self.records[jumps.last().unwrap()].jumps.get(jumps.len() - 1) {
                jumps.push(next);
            }
        }
        if matches!(h.kind, crate::event::kind::CHAT | crate::event::kind::RETRACT | crate::event::kind::AGENT | crate::event::kind::MEMBER) {
            self.agent_inputs += 1;
        }
        let structurally_void = crate::event::structurally_void(h.kind, h.actor);
        let record = Arc::new(Record {
            order, kind: h.kind, critical: h.critical, jumps, parents: h.parents.clone(), actor: h.actor, via: h.via, wall_ms: h.wall_ms,
            structurally_void,
            control: !structurally_void && matches!(h.kind, crate::event::kind::MEMBER | crate::event::kind::TOWN),
            erased_chat: h.kind == crate::event::kind::CHAT && h.kind_version == 1 && event.redacted() && !h.has_unknown_extensions(),
            content: event.content().and_then(|bytes| Content::decode(h.kind, h.kind_version, bytes)),
        });
        let latest = self.latest_height.entry(h.author).or_default();
        *latest = (*latest).max(height);
        if record.content.is_none() && matches!(h.kind, crate::event::kind::SAID | crate::event::kind::SESSION) {
            self.unreadable_session_facts.insert(event.id);
        }
        self.records.insert(event.id, record.clone());
        if self.order.last().is_none_or(|last| *last < order) {
            self.order.push(order);
        } else {
            let rank = self.order.partition_point(|other| *other < order);
            self.order.insert(rank, order);
        }
        (record, new_cut)
    }

    fn index_content(&mut self, record: &Record) {
        let order = record.order;
        if let Some(Content::Undo { of }) = &record.content {
            self.undo_children.entry(*of).or_default().insert(order);
        }
        match &record.content {
            Some(Content::Entity { derive, .. }) => {
                let id = derive.map(|(doc, at)| crate::entity::derived_task(doc, at)).unwrap_or(order.2);
                self.entity_writes.entry(id).or_default().insert(order);
            }
            Some(Content::Set { changes, .. }) => {
                for change in changes { self.entity_writes.entry(change.entity).or_default().insert(order); }
            }
            _ => {}
        }
        if record.critical && self.founder_seat().is_some_and(|s| self.seat_of(&order.1) == Some(s)) {
            match &record.content {
                Some(Content::Entity { kind, project: None, derive: None, fields, .. })
                    if *kind == kind::TOWN
                        && fields.iter().any(|(f, v)| *f == field::POLICY && field::POLICY.accepts(v)) =>
                {
                    self.policy_sets.entry(order.2).or_default().insert(order);
                }
                Some(Content::Set { changes, .. }) => {
                    for change in changes {
                        if change.field == field::POLICY && field::POLICY.accepts(&change.value) {
                            self.policy_sets.entry(change.entity).or_default().insert(order);
                        }
                    }
                }
                _ => {}
            }
        }
        match &record.content {
            Some(Content::Entity { derive: Some((doc, at)), .. }) => {
                self.possible_links.entry(*doc).or_default().insert(crate::entity::derived_task(*doc, *at));
            }
            Some(Content::Text { doc, ops }) => {
                for op in ops {
                    if let TextOp::Attr { attr: attr::TASK, value: Value::Id(task), .. } = op {
                        self.possible_links.entry(*doc).or_default().insert(*task);
                    }
                }
            }
            _ => {}
        }
    }

    pub fn message<'a>(&'a self, id: &Id) -> Option<&'a ChatMsg> {
        self.chat_positions.get(id).and_then(|at| self.chat.get(*at))
    }

    pub fn message_retracted(&self, id: &Id) -> bool {
        self.retracted.contains(id)
    }

    pub fn agent_reply<'a>(&'a self, agent: &Id) -> Option<&'a ChatMsg> {
        self.last_replies.get(agent).and_then(|id| self.message(id))
    }

    pub fn mentions<'a>(&'a self, agent: &Id) -> &'a [Id] {
        self.mentions.get(agent).map_or(&[], Vec::as_slice)
    }

    pub fn active_member(&self, key: &PublicKey) -> bool {
        self.membership.seats.active_seat(key).is_some()
    }

    /// A device spends its key in this town with its first event.
    pub fn spent(&self, key: &PublicKey) -> bool {
        self.latest_height.contains_key(key)
    }

    /// True when `msg` explicitly mentions `device`'s seat. Never true for a line its own seat
    /// wrote, nor for an AI line or an AI-addressed request (their mentions fold away empty).
    pub fn mentions_device(&self, msg: &ChatMsg, device: &PublicKey) -> bool {
        let Some(seat) = self.seat_of(device) else { return false };
        self.seat_of(&msg.author) != Some(seat) && msg.mentions_seat(seat)
    }

    /// True when `msg` quotes a live message written by `device`'s seat, and someone in another seat wrote `msg`.
    pub fn replies_to_device(&self, msg: &ChatMsg, device: &PublicKey) -> bool {
        let Some(seat) = self.seat_of(device) else { return false };
        if self.seat_of(&msg.author) == Some(seat) { return false }
        msg.chat.quote.filter(|q| !self.message_retracted(q)).and_then(|q| self.message(&q))
            .is_some_and(|quoted| self.seat_of(&quoted.author) == Some(seat))
    }

    /// A representative device of a known seat: its lowest active device, else the one it began with.
    pub fn seat_device(&self, seat: &Id) -> Option<PublicKey> {
        let seat = self.membership.seats.seats.get(seat)?;
        Some(seat.devices.iter().next().copied().unwrap_or(seat.first_device))
    }

    pub fn forked(&self, key: &PublicKey) -> Option<ForkPoint> {
        self.membership.forks.get(key).map(|&(_, point)| point)
    }

    pub fn founder(&self) -> Option<PublicKey> {
        self.membership.controls.iter().find_map(|&(_, op)| {
            let MembershipOp::Event(author, id) = op else { return None };
            matches!(self.records[&id].content, Some(Content::Town { .. })).then_some(author)
        })
    }

    pub fn founder_seat(&self) -> Option<Id> {
        self.membership.seats.seats.iter().find_map(|(&id, s)| s.invite_parent.is_none().then_some(id))
    }

    pub fn is_intake(&self, key: &PublicKey) -> bool {
        self.seat_of(key).and_then(|id| self.membership.seats.seats.get(&id)).is_some_and(|s| s.role != 0)
    }

    pub fn record_meta(&self, id: &Id) -> Option<RecordMeta> {
        self.records.get(id).map(|r| RecordMeta { author: r.order.1, actor: r.actor, via: r.via,
            kind: r.kind, void: self.void_events.contains_key(id), readable: r.content.is_some() })
    }

    /// Identifies a folded `kind::SERVICE` record's variant and job/session by event id (`O(log N)` lookup).
    /// Returns `None` if the event is missing, not `kind::SERVICE`, or has no decoded content (redacted/undecodable).
    pub fn service_record(&self, id: &Id) -> Option<ServiceRecord> {
        match self.records.get(id)?.content.as_ref()? {
            Content::Request { job, session, .. } => Some(ServiceRecord::Request { job: *job, session: *session }),
            Content::Done { job, .. } => Some(ServiceRecord::Done { job: *job }),
            Content::Failed { job, .. } => Some(ServiceRecord::Failed { job: *job }),
            Content::Provides { .. } | Content::Taken { .. } | Content::Shared { .. } => Some(ServiceRecord::Other),
            _ => None,
        }
    }

    /// Derived-task metadata by creation event or task id, each in O(log C) for C conversions.
    pub fn derived_task_meta(&self, id: &Id) -> Option<DerivedTaskMeta> {
        let creator = if self.conversions.contains_key(id) { *id } else { *self.derived_task_creators.get(id)? };
        let &(doc, at, task) = self.conversions.get(&creator)?;
        Some(DerivedTaskMeta { task, creator, doc, at })
    }

    /// The fields as signed on creation, including their original order.
    pub fn created_entity_fields(&self, id: &Id) -> Option<&[(Key, Value)]> {
        let creator = self.derived_task_creators.get(id).unwrap_or(id);
        if self.void_events.contains_key(creator) { return None }
        match self.records.get(creator)?.content.as_ref()? {
            Content::Entity { fields, .. } => Some(fields),
            _ => None,
        }
    }

    /// Most recent signed profile, preferring a non-void event even if its body is unreadable.
    pub fn latest_profile_event(&self, key: &PublicKey) -> Option<Id> {
        self.latest_profiles.get(key).map(|&(_, _, id)| id)
    }

    /// True if a non-void body-redacted or undecodable `SAID` or `SESSION` record lies in
    /// `request`'s causal past without lying in `session`'s `Open` causal past.
    pub fn has_unreadable_session_fact_before(&self, session: &SessionId, request: Id) -> bool {
        if self.unreadable_session_facts.is_empty() { return false }
        let Some(req) = self.records.get(&request) else { return false };
        let Some(open) = self.session(session).and_then(|s| s.open.as_ref())
            .and_then(|o| self.records.get(&o.anchor)) else { return false };
        if !in_past(&self.records, req, open.order.2) { return false }
        self.unreadable_session_facts.iter().any(|&id| {
            !self.void_events.contains_key(&id)
                && in_past(&self.records, req, id)
                && !in_past(&self.records, open, id)
        })
    }

    #[cfg(test)]
    pub(crate) fn set_seat_role_for_test(&mut self, seat: &Id, role: u8) {
        if let Some(entry) = self.membership.seats.seats.get_mut(seat) {
            entry.role = role;
        }
    }

    pub fn copied_from(&self, writer: Id, entity: Id, field: &Key) -> &[CopySource] {
        self.copied_from.get(&(writer, entity, field.clone())).map_or(&[], Vec::as_slice)
    }

    /// Last event to write or restore the currently cleared field.
    pub fn cleared_field_writer(&self, id: &Id, field: &Key) -> Option<Id> {
        self.cleared.get(&(*id, field.clone())).copied()
    }

    /// Writer of the state restored by this undo, if the undo took effect on this field.
    pub fn field_undo_prev(&self, undo: &Id, entity: &Id, field: &Key) -> Option<Id> {
        let Content::Undo { of } = self.records.get(undo)?.content.as_ref()? else { return None };
        // Undoing creation writes a new tombstone.
        if matches!(self.records.get(of)?.content, Some(Content::Entity { .. })) { return None }
        let key = (*entity, field.clone());
        if !self.groups.get(undo)?.contains_key(&key) { return None }
        self.groups.get(of)?.get(&key)?.as_ref().map(|f| f.writer)
    }

    /// Whether the target is in the strict causal past of the given record.
    pub fn record_before(&self, before: &Id, target: &Id) -> bool {
        self.records.get(before).is_some_and(|rec| in_past(&self.records, rec, *target))
    }

    /// Extends `ops` with the transitive closure of `undo_children` strictly forward in canonical
    /// order `< before_order`, starting from `pending`, filtering each undo with `allow`.
    fn close_undo_ops(
        &self,
        ops: &mut BTreeSet<Order>,
        mut pending: Vec<Order>,
        before_order: Order,
        mut allow: impl FnMut(&Record) -> bool,
    ) {
        use std::ops::Bound;
        while let Some(target) = pending.pop() {
            if let Some(undos) = self.undo_children.get(&target.2) {
                for &u_order in undos.range((Bound::Excluded(target), Bound::Excluded(before_order))) {
                    let u_rec = &self.records[&u_order.2];
                    if allow(u_rec) && ops.insert(u_order) {
                        pending.push(u_order);
                    }
                }
            }
        }
    }

    /// All non-void critical founder-seat records that can write or undo `(town, field::POLICY)`
    /// in `[town_order, before_order)` in canonical order.
    fn policy_ops_before(&self, town: Id, before_order: Order) -> BTreeSet<Order> {
        use std::ops::Bound;
        let Some(sets) = self.policy_sets.get(&town) else { return BTreeSet::new() };
        let Some(town_order) = self.records.get(&town).map(|r| r.order) else { return BTreeSet::new() };
        if town_order >= before_order { return BTreeSet::new() }
        let mut ops: BTreeSet<Order> = sets
            .range((Bound::Included(town_order), Bound::Excluded(before_order)))
            .copied()
            .collect();
        if ops.is_empty() { return ops }
        let founder = self.founder_seat();
        // Creation undo (`Undo { of: town }`) only touches `DELETED`, never `POLICY`.
        let pending: Vec<Order> = ops.iter().copied().filter(|o| o.2 != town).collect();
        self.close_undo_ops(&mut ops, pending, before_order, |u_rec| {
            u_rec.critical && founder.is_some_and(|s| self.seat_of(&u_rec.order.1) == Some(s))
        });
        ops
    }

    fn resolve_doc_op_at(doc: &DocState, event: Id, at: BreakRef) -> BreakRef {
        match at {
            BreakRef::Inserted(index) => BreakRef::Char(crate::doc::CharId { event, index }),
            at => doc.canonical(at),
        }
    }

    /// True iff `fold(Past(past_rec))` and `fold(H)` assign identical `classify` (void vs non-void,
    /// `seat_of`, `founder_seat`) to every board/doc event in `Past(past_rec)` ordered `< before_order`.
    fn membership_clean_for_past(&self, past_rec: &Record, before_order: Order) -> bool {
        if self.void_events.contains_key(&past_rec.order.2) {
            return false;
        }
        for &id in self.void_events.keys() {
            let Some(r) = self.records.get(&id) else { return false };
            if r.structurally_void { continue }
            if !in_past(&self.records, past_rec, id) { continue }
            if r.control { return false }
            if r.order < before_order && matches!(
                r.content,
                Some(Content::Entity { .. } | Content::Set { .. } | Content::Undo { .. } | Content::Text { .. })
                    | None
            ) {
                return false;
            }
        }
        let has_in_past_removal = self.membership.removals.iter().any(|(&id, res)| {
            *res == Ok(Removal::Counted) && in_past(&self.records, past_rec, id)
        });
        for &(height, op) in &self.membership.controls {
            if height > before_order.0 { break }
            let MembershipOp::Event(_, id) = op else { continue };
            let Some(r) = self.records.get(&id) else { return false };
            if r.order >= before_order || in_past(&self.records, past_rec, id) { continue }
            match &r.content {
                Some(Content::Town { .. }) => return false,
                Some(Content::Member(Member::Accept { .. })) if has_in_past_removal => return false,
                _ => {}
            }
        }
        true
    }

    /// True iff `ev_order` (when in `Past(past_rec)` and `Content::Text`) has no
    /// `TextOp::Attr(TASK, Value::Id(t))` whose target `t` existed in `fold(H)` before `ev_order`
    /// without being created in `Past(past_rec)` before `ev_order` (`doc.rs:385-387`).
    fn text_event_links_valid_in_past(&self, past_rec: &Record, ev_order: Order) -> bool {
        if !in_past(&self.records, past_rec, ev_order.2) { return true }
        let Some(Content::Text { ops, .. }) = self.records[&ev_order.2].content.as_ref() else { return true };
        for op in ops {
            if let TextOp::Attr { attr: attr::TASK, value: Value::Id(task), .. } = op {
                if self.statuses.get(task).and_then(|w| w.range(..ev_order).next_back()).is_some()
                    && !self.entity_created_in_past_before(past_rec, ev_order, task)
                {
                    return false;
                }
            }
        }
        true
    }

    fn doc_links_valid_in_past(&self, past_rec: &Record, up_to: Order, inclusive: bool, doc: &Id) -> bool {
        if !self.possible_links.contains_key(doc) { return true }
        let Some(timeline) = self.timelines.get(doc) else { return true };
        let end = timeline.events.partition_point(|o| if inclusive { *o <= up_to } else { *o < up_to });
        timeline.events[..end].iter().all(|&ev_order| self.text_event_links_valid_in_past(past_rec, ev_order))
    }

    /// True iff every in-past `Content::Text` event on `doc` strictly below `before_order` whose
    /// `TextOp::Attr(TASK, Value::Id(t))` passed `status(t).is_some()` in `fold(H)` also sees
    /// `t` created in `Past(past_rec)` before that event (`doc.rs:385-387`).
    fn doc_links_valid_in_past_before(&self, past_rec: &Record, before_order: Order, doc: &Id) -> bool {
        self.doc_links_valid_in_past(past_rec, before_order, false, doc)
    }

    /// Inclusive variant of `doc_links_valid_in_past_before` (`<= through_order`), covering the
    /// queried event's own burst when `through_order` is on `doc`'s timeline.
    fn doc_links_valid_in_past_through(&self, past_rec: &Record, through_order: Order, doc: &Id) -> bool {
        self.doc_links_valid_in_past(past_rec, through_order, true, doc)
    }

    /// True iff `entity` (plus its ancestor `project` chain and, if derived, its `doc` and doc link
    /// validity) was created in `Past(past_rec)` strictly before `before_order`, with no concurrent
    /// duplicate derived-task creation below `before_order`.
    fn entity_created_in_past_before(&self, past_rec: &Record, before_order: Order, entity: &Id) -> bool {
        let mut cur = *entity;
        let mut limit = before_order;
        let mut seen = BTreeSet::new();
        while seen.insert(cur) {
            let Some(ent) = self.entities.get(&cur) else { return false };
            let derived = self.derived_task_meta(&cur);
            let creator = derived.map_or(cur, |d| d.creator);
            if !in_past(&self.records, past_rec, creator) { return false }
            let Some(c_rec) = self.records.get(&creator) else { return false };
            if c_rec.order >= limit { return false }
            if let Some(d) = derived {
                if self.entity_writes.get(&cur).is_some_and(|ws| {
                    ws.range(..limit).any(|o| {
                        !in_past(&self.records, past_rec, o.2)
                            && matches!(self.records[&o.2].content, Some(Content::Entity { derive: Some(_), .. }))
                    })
                }) {
                    return false;
                }
                if !self.entity_created_in_past_before(past_rec, c_rec.order, &d.doc) { return false }
                if !self.doc_links_valid_in_past_before(past_rec, c_rec.order, &d.doc) { return false }
            }
            match ent.project {
                Some(p) => { cur = p; limit = c_rec.order; }
                None => return true,
            }
        }
        false
    }

    /// All non-void events ordered `< before_order` that can directly write or undo `(doc, at, attr)`.
    fn doc_attr_ops_before(
        &self,
        doc: &Id,
        doc_state: &DocState,
        at: BreakRef,
        attr: u16,
        before_order: Order,
    ) -> BTreeSet<Order> {
        let at = doc_state.canonical(at);
        let mut ops = BTreeSet::new();
        let Some(timeline) = self.timelines.get(doc) else { return ops };
        let end = timeline.events.partition_point(|o| *o < before_order);
        for &ev_order in &timeline.events[..end] {
            let ev_id = ev_order.2;
            let touches = match self.records[&ev_id].content.as_ref() {
                Some(Content::Text { ops: text_ops, .. }) => text_ops.iter().any(|op| match op {
                    TextOp::Attr { at: op_at, attr: k, .. } if *k == attr => {
                        Self::resolve_doc_op_at(doc_state, ev_id, *op_at) == at
                    }
                    TextOp::Attr { at: op_at, attr: attr::TASK, value: Value::Null }
                        if attr == attr::CHECKBOX =>
                    {
                        Self::resolve_doc_op_at(doc_state, ev_id, *op_at) == at
                    }
                    _ => false,
                }),
                Some(Content::Entity { derive: Some((d, d_at)), .. }) if attr == attr::TASK && d == doc => {
                    doc_state.delimiter(*d_at).map(|m| doc_state.canonical(m)) == Some(at)
                }
                _ => false,
            };
            if touches { ops.insert(ev_order); }
        }
        let seeds: Vec<Order> = ops.iter().copied().collect();
        self.close_undo_ops(&mut ops, seeds, before_order, |_| true);
        ops
    }

    /// For `task`, collects across every doc in `possible_links` containing `task`:
    /// - candidate `Content::Text` tick events ordered `< before_order` (at `>= first_link[at]`);
    /// - every `(doc, at)` that links to `task` at some order `< before_order`.
    fn task_link_blocks_and_ticks_before(
        &self,
        task: &Id,
        before_order: Order,
    ) -> (Vec<Order>, Vec<(Id, BreakRef)>) {
        let mut ticks = Vec::new();
        let mut link_blocks = Vec::new();
        for (&doc, tasks) in &self.possible_links {
            if !tasks.contains(task) { continue }
            let (Some(timeline), Some(doc_state)) = (self.timelines.get(&doc), self.docs.get(&doc)) else {
                continue;
            };
            let end = timeline.events.partition_point(|o| *o < before_order);
            let mut first_link: BTreeMap<BreakRef, Order> = BTreeMap::new();
            for &ev_order in &timeline.events[..end] {
                let ev_id = ev_order.2;
                match self.records[&ev_id].content.as_ref() {
                    Some(Content::Entity { derive: Some((_, d_at)), .. }) => {
                        if crate::entity::derived_task(doc, *d_at) == *task {
                            if let Some(m) = doc_state.delimiter(*d_at) {
                                first_link.entry(doc_state.canonical(m)).or_insert(ev_order);
                            }
                        }
                    }
                    Some(Content::Text { ops, .. }) => {
                        for op in ops {
                            if let TextOp::Attr { at: op_at, attr: attr::TASK, value: Value::Id(t) } = op {
                                if t == task {
                                    let at = Self::resolve_doc_op_at(doc_state, ev_id, *op_at);
                                    first_link.entry(at).or_insert(ev_order);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            if first_link.is_empty() { continue }
            for &at in first_link.keys() {
                link_blocks.push((doc, at));
            }
            for &ev_order in &timeline.events[..end] {
                let ev_id = ev_order.2;
                let Some(Content::Text { ops, .. }) = self.records[&ev_id].content.as_ref() else { continue };
                let is_tick = ops.iter().any(|op| {
                    let TextOp::Attr { at: op_at, attr: attr::CHECKBOX, value: Value::Text(s) } = op else {
                        return false;
                    };
                    if !matches!(s.as_str(), "done" | "review" | "open") { return false }
                    let at = Self::resolve_doc_op_at(doc_state, ev_id, *op_at);
                    first_link.get(&at).is_some_and(|&lo| ev_order >= lo)
                });
                if is_tick { ticks.push(ev_order); }
            }
        }
        (ticks, link_blocks)
    }

    /// All non-void events ordered `< before_order` that can directly write or undo `(ent, field)`.
    fn field_self_ops_before(
        &self,
        ent: &Entity,
        field: &Key,
        before_order: Order,
        extra_direct: &[Order],
    ) -> BTreeSet<Order> {
        use std::ops::Bound;
        let mut ops = BTreeSet::new();
        if *field == field::POLICY {
            if ent.kind == kind::TOWN && ent.project.is_none() {
                return self.policy_ops_before(ent.id, before_order);
            }
            return ops;
        }
        let creator = self.derived_task_meta(&ent.id).map_or(ent.id, |d| d.creator);
        let Some(creator_order) = self.records.get(&creator).map(|r| r.order) else { return ops };
        if creator_order >= before_order { return ops }
        let owner_seat = self.owner_only(ent, field).then(|| self.seat_of(&ent.author));
        let static_permits = |author: &PublicKey| -> bool {
            owner_seat.is_none_or(|owner| owner.is_some() && self.seat_of(author) == owner)
        };
        let mut undo_seeds = Vec::new();
        if let Some(writes) = self.entity_writes.get(&ent.id) {
            for &w_order in writes.range((Bound::Included(creator_order), Bound::Excluded(before_order))) {
                let rec = &self.records[&w_order.2];
                match &rec.content {
                    Some(Content::Entity { fields, .. }) => {
                        let writes_field = fields.iter().any(|(f, v)| f == field && field.accepts(v));
                        if writes_field { ops.insert(w_order); }
                        // Creation `Undo { of: entity }` only writes `field::DELETED`.
                        if *field == field::DELETED { undo_seeds.push(w_order); }
                    }
                    Some(Content::Set { changes, .. }) => {
                        if !static_permits(&w_order.1) { continue }
                        let touches = changes.iter().any(|c| {
                            c.entity == ent.id
                                && c.field == *field
                                && field.accepts(&c.value)
                                && !(ent.kind == kind::ISSUE
                                    && *field == field::STATUS
                                    && !matches!(&c.value, Value::Text(s) if matches!(s.as_str(), "open" | "resolved")))
                        });
                        if touches {
                            ops.insert(w_order);
                            undo_seeds.push(w_order);
                        }
                    }
                    _ => {}
                }
            }
        }
        for &tick in extra_direct {
            if tick >= creator_order && tick < before_order && ops.insert(tick) {
                undo_seeds.push(tick);
            }
        }
        self.close_undo_ops(&mut ops, undo_seeds, before_order, |u_rec| static_permits(&u_rec.order.1));
        ops
    }

    /// Returns `(self_ops, dep_ops)` for `(ent, field)` below `before_order`, or `Err(())` if a
    /// linked doc fails `doc_links_valid_in_past_before`.
    fn field_ops_with_deps_before(
        &self,
        past_rec: &Record,
        ent: &Entity,
        field: &Key,
        before_order: Order,
        include_derived_activity: bool,
    ) -> Result<(BTreeSet<Order>, BTreeSet<Order>), ()> {
        use std::ops::Bound;
        let mut extra_ticks = Vec::new();
        let mut link_blocks = Vec::new();
        if ent.kind == kind::TASK && *field == field::STATUS {
            (extra_ticks, link_blocks) = self.task_link_blocks_and_ticks_before(&ent.id, before_order);
        }
        let self_ops = self.field_self_ops_before(ent, field, before_order, &extra_ticks);
        let mut dep_ops = self_ops.clone();

        if field::geometry(field) {
            dep_ops.extend(self.field_self_ops_before(ent, &field::LOCKED, before_order, &[]));
        }
        if ent.kind == kind::TOWN
            && ent.project.is_none()
            && (*field == field::DELETED || *field == field::ARCHIVED)
        {
            dep_ops.extend(self.policy_ops_before(ent.id, before_order));
        }
        if !link_blocks.is_empty() {
            let mut checked_docs = BTreeSet::new();
            for (doc, at) in link_blocks {
                if checked_docs.insert(doc) && !self.doc_links_valid_in_past_before(past_rec, before_order, &doc) {
                    return Err(());
                }
                if let Some(doc_state) = self.docs.get(&doc) {
                    dep_ops.extend(self.doc_attr_ops_before(&doc, doc_state, at, attr::TASK, before_order));
                }
            }
        }
        if include_derived_activity && *field == field::DELETED {
            if let Some(&creator) = self.derived_task_creators.get(&ent.id) {
                if let (Some(c_rec), Some(undos)) = (self.records.get(&creator), self.undo_children.get(&creator)) {
                    if let Some(&u_max) = undos
                        .range((Bound::Excluded(c_rec.order), Bound::Excluded(before_order)))
                        .next_back()
                    {
                        let mut other_fields = BTreeSet::from([field::STATUS]);
                        if let Some(writes) = self.entity_writes.get(&ent.id) {
                            for &w_order in writes.range((Bound::Excluded(c_rec.order), Bound::Excluded(u_max))) {
                                if let Some(Content::Set { changes, .. }) = self.records[&w_order.2].content.as_ref() {
                                    for c in changes {
                                        if c.entity == ent.id && c.field != field::DELETED {
                                            other_fields.insert(c.field.clone());
                                        }
                                    }
                                }
                            }
                        }
                        for other_field in other_fields {
                            let (_, sub_deps) = self.field_ops_with_deps_before(
                                past_rec, ent, &other_field, u_max, false,
                            )?;
                            dep_ops.extend(sub_deps);
                        }
                    }
                }
            }
        }
        Ok((self_ops, dep_ops))
    }

    /// Two-phase non-interference check on `(self_ops, dep_ops)` for `(past_rec, lo_order, before_order)`:
    /// - Phase 1 (`..lo_order`): no concurrent op in `dep_ops` strictly below `lo_order`.
    /// - Phase 2 (`(lo_order, before_order)`): either no concurrent op in `dep_ops` (Way 2A),
    ///   or no in-past op in `self_ops` (Way 2B).
    fn check_non_interference(
        &self,
        past_rec: &Record,
        lo_order: Option<Order>,
        before_order: Order,
        self_ops: &BTreeSet<Order>,
        dep_ops: &BTreeSet<Order>,
    ) -> bool {
        use std::ops::Bound;
        if let Some(lo) = lo_order {
            if dep_ops.range(..lo).any(|o| !in_past(&self.records, past_rec, o.2)) {
                return false;
            }
        }
        let tail = match lo_order {
            Some(lo) => (Bound::Excluded(lo), Bound::Excluded(before_order)),
            None => (Bound::Unbounded, Bound::Excluded(before_order)),
        };
        let concurrent_in_tail = dep_ops.range(tail).any(|o| !in_past(&self.records, past_rec, o.2));
        if concurrent_in_tail && self_ops.range(tail).any(|o| in_past(&self.records, past_rec, o.2)) {
            return false;
        }
        true
    }

    fn field_writer_in_past_before(
        &self,
        past_rec: &Record,
        before_order: Order,
        entity: &Id,
        field: &Key,
    ) -> Result<Option<Id>, ()> {
        if !self.membership_clean_for_past(past_rec, before_order)
            || !self.entity_created_in_past_before(past_rec, before_order, entity)
        {
            return Err(());
        }
        let ent = self.entities.get(entity).ok_or(())?;
        let key = (*entity, field.clone());
        let mut cur = ent.fields.get(field).map(|f| f.writer)
            .or_else(|| self.cleared.get(&key).copied());
        let mut found = None;
        while let Some(writer) = cur {
            if in_past(&self.records, past_rec, writer)
                && self.records.get(&writer).is_some_and(|r| r.order < before_order)
            {
                found = Some(writer);
                break;
            }
            cur = self.groups.get(&writer)
                .and_then(|g| g.get(&key))
                .and_then(|prior| prior.as_ref().map(|f| f.writer));
        }
        let lo_order = found.and_then(|w| self.records.get(&w).map(|r| r.order));
        let (self_ops, dep_ops) = self.field_ops_with_deps_before(past_rec, ent, field, before_order, true)?;
        if !self.check_non_interference(past_rec, lo_order, before_order, &self_ops, &dep_ops) {
            return Err(());
        }
        Ok(found)
    }

    /// Certified effective writer of `(entity, field)` in `fold(Past(before))` under Two-Phase
    /// Non-Interference: returns `Ok(Some(w))` or `Ok(None)` only when `fold(Past(before))` is
    /// guaranteed identical to `fold(H)` on `(entity, field)` and its undo chain, else `Err(())`.
    pub fn field_writer_before(&self, before: &Id, entity: &Id, field: &Key) -> Result<Option<Id>, ()> {
        let rec = self.records.get(before).ok_or(())?;
        self.field_writer_in_past_before(rec, rec.order, entity, field)
    }

    /// True iff `claimed` is certified as the exact effective writer of `(entity, field)` in
    /// `fold(Past(copy_event))`.
    pub fn verify_field_writer_at(&self, copy_event: &Id, entity: &Id, field: &Key, claimed: &Id) -> bool {
        self.field_writer_before(copy_event, entity, field) == Ok(Some(*claimed))
    }

    /// True iff `sw` is certified as the exact effective writer of `(task, STATUS)` at `at_event`
    /// inside `fold(Past(past_of))` (allowing `sw == task_creator` when `STATUS` was cleanly untouched).
    pub fn verify_task_status_at_in_past(&self, past_of: &Id, at_event: &Id, task: &Id, sw: &Id) -> bool {
        let (Some(past_rec), Some(at_rec)) = (self.records.get(past_of), self.records.get(at_event)) else {
            return false;
        };
        if !in_past(&self.records, past_rec, *at_event) || self.void_events.contains_key(at_event) {
            return false;
        }
        if let Some(doc) = self.scopes.get(at_event) {
            if !self.entity_created_in_past_before(past_rec, at_rec.order, doc)
                || !self.doc_links_valid_in_past_through(past_rec, at_rec.order, doc)
            {
                return false;
            }
        }
        let task_creator = self.derived_task_meta(task).map_or(*task, |d| d.creator);
        match self.field_writer_in_past_before(past_rec, at_rec.order, task, &field::STATUS) {
            Ok(Some(w)) => w == *sw,
            Ok(None) => *sw == task_creator
                && in_past(&self.records, past_rec, *sw)
                && self.records.get(sw).is_some_and(|r| r.order < at_rec.order),
            Err(()) => false,
        }
    }

    /// Previous writer event restored by `undo` on document block attribute `(doc, at, attr)`.
    pub fn doc_attr_undo_prev(&self, doc: &Id, undo: &Id, at: BreakRef, attr: u16) -> Option<Id> {
        let Some(Content::Undo { of }) = self.records.get(undo)?.content.as_ref() else { return None };
        self.docs.get(doc)?.attr_undo_prev(undo, of, at, attr)
    }

    /// `(doc, at, link_writer)` of the `attr::TASK` link that routed tick `event` onto `(task, STATUS)`.
    pub fn tick_link_source(&self, event: &Id, task: &Id) -> Option<(Id, BreakRef, Id)> {
        let &(_, at, link_writer) = self.tick_writes.get(event)?.get(task)?;
        let doc = *self.scopes.get(event)?;
        Some((doc, at, link_writer))
    }

    fn doc_attr_state_in_past_before(
        &self,
        past_rec: &Record,
        before_order: Order,
        doc: &Id,
        at: BreakRef,
        attr: u16,
    ) -> Result<Option<(Id, Value, Option<crate::doc::CheckboxSource>)>, ()> {
        if !self.membership_clean_for_past(past_rec, before_order)
            || !self.entity_created_in_past_before(past_rec, before_order, doc)
            || !self.doc_links_valid_in_past_before(past_rec, before_order, doc)
        {
            return Err(());
        }
        let doc_state = self.docs.get(doc).ok_or(())?;
        let at = doc_state.canonical(at);
        match at {
            BreakRef::Root => {}
            BreakRef::Char(ch) => {
                if !in_past(&self.records, past_rec, ch.event)
                    || !self.records.get(&ch.event).is_some_and(|r| r.order < before_order)
                {
                    return Err(());
                }
            }
            BreakRef::Inserted(_) => return Err(()),
        }
        let found = doc_state.attr_state_before(at, attr, |id| {
            in_past(&self.records, past_rec, id)
                && self.records.get(&id).is_some_and(|r| r.order < before_order)
        });
        let lo_order = found.as_ref().and_then(|(w, _, _)| self.records.get(w).map(|r| r.order));
        let self_ops = self.doc_attr_ops_before(doc, doc_state, at, attr, before_order);
        let mut dep_ops = self_ops.clone();
        if attr == attr::CHECKBOX {
            let task_ops = self.doc_attr_ops_before(doc, doc_state, at, attr::TASK, before_order);
            dep_ops.extend(task_ops.iter().copied());
            let max_unlink = self_ops.iter().rev().copied().find(|o| {
                matches!(self.records[&o.2].content.as_ref(), Some(Content::Text { ops, .. })
                    if ops.iter().any(|op| matches!(op,
                        TextOp::Attr { at: op_at, attr: attr::TASK, value: Value::Null }
                            if Self::resolve_doc_op_at(doc_state, o.2, *op_at) == at
                    )))
            });
            if let Some(u_max) = max_unlink {
                let mut linked_tasks = BTreeSet::new();
                for &t_order in task_ops.range(..=u_max) {
                    match self.records[&t_order.2].content.as_ref() {
                        Some(Content::Entity { derive: Some((_, d_at)), .. }) => {
                            linked_tasks.insert(crate::entity::derived_task(*doc, *d_at));
                        }
                        Some(Content::Text { ops, .. }) => {
                            for op in ops {
                                if let TextOp::Attr { at: op_at, attr: attr::TASK, value: Value::Id(t) } = op {
                                    if Self::resolve_doc_op_at(doc_state, t_order.2, *op_at) == at {
                                        linked_tasks.insert(*t);
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                for task_id in linked_tasks {
                    if let Some(task_ent) = self.entities.get(&task_id) {
                        let (_, status_deps) = self.field_ops_with_deps_before(
                            past_rec, task_ent, &field::STATUS, u_max, false,
                        )?;
                        dep_ops.extend(status_deps);
                    }
                }
            }
        }
        if !self.check_non_interference(past_rec, lo_order, before_order, &self_ops, &dep_ops) {
            return Err(());
        }
        Ok(found)
    }

    /// Certified `(writer, value, checkbox_source)` of `(doc, at, attr)` in `fold(Past(before))`
    /// under Two-Phase Non-Interference; returns `Err(())` whenever certification is conservative.
    pub fn doc_attr_state_before(
        &self,
        before: &Id,
        doc: &Id,
        at: BreakRef,
        attr: u16,
    ) -> Result<Option<(Id, Value, Option<crate::doc::CheckboxSource>)>, ()> {
        let rec = self.records.get(before).ok_or(())?;
        self.doc_attr_state_in_past_before(rec, rec.order, doc, at, attr)
    }

    /// True iff `link_writer` is certified as the exact effective writer of
    /// `(doc, at, attr::TASK) == Value::Id(task)` at `at_event` inside `fold(Past(past_of))`.
    pub fn verify_doc_task_link_at_in_past(
        &self,
        past_of: &Id,
        at_event: &Id,
        doc: &Id,
        at: BreakRef,
        task: &Id,
        link_writer: &Id,
    ) -> bool {
        let (Some(past_rec), Some(at_rec)) = (self.records.get(past_of), self.records.get(at_event)) else {
            return false;
        };
        if !in_past(&self.records, past_rec, *at_event) || self.void_events.contains_key(at_event) {
            return false;
        }
        if link_writer == at_event {
            let Some(doc_state) = self.docs.get(doc) else { return false };
            let canon = doc_state.canonical(at);
            let links_task = matches!(
                at_rec.content.as_ref(),
                Some(Content::Text { doc: ev_doc, ops })
                    if ev_doc == doc
                        && ops.iter().any(|op| matches!(
                            op,
                            TextOp::Attr { at: op_at, attr: attr::TASK, value: Value::Id(t) }
                                if t == task && Self::resolve_doc_op_at(doc_state, *at_event, *op_at) == canon
                        ))
            );
            return links_task
                && doc_state.attr_written_by(at_event, canon, attr::TASK)
                && self.membership_clean_for_past(past_rec, at_rec.order)
                && self.entity_created_in_past_before(past_rec, at_rec.order, task)
                && self.entity_created_in_past_before(past_rec, at_rec.order, doc)
                && self.doc_links_valid_in_past_through(past_rec, at_rec.order, doc);
        }
        if !self.text_event_links_valid_in_past(past_rec, at_rec.order) {
            return false;
        }
        match self.doc_attr_state_in_past_before(past_rec, at_rec.order, doc, at, attr::TASK) {
            Ok(Some((w, Value::Id(t), _))) => w == *link_writer && t == *task,
            _ => false,
        }
    }

    /// True iff `event` belongs to `doc`'s timeline (`scopes[event] == doc`) and lies in `before`'s causal past.
    pub fn doc_event_before(&self, before: &Id, doc: &Id, event: &Id) -> bool {
        let Some(rec) = self.records.get(before) else { return false };
        self.scopes.get(event) == Some(doc) && in_past(&self.records, rec, *event)
    }

    /// `(session, is_open)` for a readable folded `kind::SESSION` or `kind::SAID` record.
    pub fn session_fact_meta(&self, id: &Id) -> Option<(SessionId, bool)> {
        match self.records.get(id)?.content.as_ref()? {
            Content::Session(SessionOp::Open { session, .. }) => Some((*session, true)),
            Content::Session(SessionOp::Join { session } | SessionOp::Leave { session, .. } | SessionOp::Decline { session, .. }) => Some((*session, false)),
            Content::Said { session, .. } => Some((*session, false)),
            _ => None,
        }
    }

    /// An admitted seat stays in the history when its device leaves.
    pub fn seat_of(&self, device: &PublicKey) -> Option<Id> {
        self.membership.seats.admitted.get(device).map(|a| a.seat)
    }

    pub fn anchor(&self, entry: Entry) -> Option<Id> {
        match entry {
            Entry::Chat(id) | Entry::Clip(id) | Entry::Member(id) => Some(id),
            Entry::Session(id) => Some(self.sessions.get(&id)?.open.as_ref()?.anchor),
        }
    }

    pub fn timeline_index(&self, id: Id) -> Option<usize> {
        let order = self.records.get(&id)?.order;
        let at = self.timeline.partition_point(|&entry| self.anchor(entry).and_then(|a| self.records.get(&a))
            .is_some_and(|r| r.order < order));
        self.timeline.get(at).filter(|&&entry| self.anchor(entry) == Some(id)).map(|_| at)
    }

    /// Even an unverified recipient tag keeps another seat's AI conversation private.
    pub fn addressed_chat(&self, id: &Id) -> bool {
        !self.message_retracted(id) && self.records.get(id).is_some_and(|r| matches!(&r.content, Some(Content::AgentChat { agent: ChatAgent::Recipient(_), .. }))
            || matches!(&r.content, Some(Content::Chat(c)) if c.to_ai))
    }

    /// This device's latest offer in this town. Removed devices cannot provide services.
    pub fn provides(&self, device: &PublicKey) -> &[Service] {
        if self.active_member(device) {
            if let Some(Content::Provides { services }) = self.services.offers.get(device)
                .and_then(|id| self.records.get(id)).and_then(|r| r.content.as_ref()) { return services }
        }
        &[]
    }

    pub fn service_jobs(&self, session: &SessionId) -> Vec<Job> {
        let Some(jobs) = self.services.sessions.get(session) else { return Vec::new() };
        if self.session(session).and_then(|s| s.open.as_ref()).is_none() { return Vec::new() }
        let mut jobs: Vec<_> = jobs.iter().filter_map(|id| self.service_job(*id, session)).collect();
        jobs.sort_unstable_by_key(|j| self.records[&j.request].order);
        jobs
    }

    // Resolve the request and its provider's causal reports once for both readers.
    fn service_job_history(&self, job: &JobId) -> Option<(&Record, Option<&Record>)> {
        let mut records = self.services.histories.get(job)?.iter()
            .map(|id| self.records.get(id).map(Arc::as_ref)).collect::<Option<Vec<_>>>()?;
        records.sort_unstable_by_key(|rec| rec.order);
        let mut history = records.into_iter();
        let (request, provider) = history.find_map(|rec| {
            let Content::Request { session, provider, .. } = rec.content.as_ref()? else { return None };
            self.session(session)?.open.as_ref()?;
            Some((rec, *provider))
        })?;
        let mut report = None;
        for rec in history {
            if rec.order.1 != provider { continue }
            match rec.content {
                Some(Content::Taken { .. }) if report.is_none() && in_past(&self.records, rec, request.order.2) => report = Some(rec),
                Some(Content::Done { .. } | Content::Failed { .. }) if in_past(&self.records, rec, request.order.2) => {
                    report = Some(rec);
                    break
                }
                _ => {}
            }
        }
        Some((request, report))
    }

    fn service_job(&self, job: JobId, session: &SessionId) -> Option<Job> {
        let (request, report) = self.service_job_history(&job)?;
        let Content::Request { session: call, service, project, provider, .. } = request.content.as_ref()? else { return None };
        if call != session { return None }
        let state = match report.and_then(|r| r.content.as_ref()) {
            Some(Content::Taken { .. }) => JobState::Working,
            Some(Content::Done { text, .. }) => JobState::Done(text.clone()),
            Some(Content::Failed { why, .. }) => JobState::Failed(why.clone()),
            _ => JobState::Asked,
        };
        Some(Job { job, service: *service, requester: request.order.1, provider: *provider,
            project: *project, request: request.order.2, state })
    }

    pub fn service_job_meta(&self, job: &JobId) -> Option<ServiceJobMeta> {
        let (request, report) = self.service_job_history(job)?;
        let Content::Request { session, project, provider, .. } = request.content.as_ref()? else { return None };
        let report_writer = report.filter(|r| matches!(r.content, Some(Content::Done { .. } | Content::Failed { .. })))
            .map(|r| r.order.2);
        Some(ServiceJobMeta { session: *session, project: *project, requester: request.order.1,
            provider: *provider, request: request.order.2, report_writer })
    }

    pub fn session(&self, id: &SessionId) -> Option<&Session> {
        self.sessions.get(id)
    }

    /// A folded event's place in the canonical order (height, author, id).
    pub fn order_of(&self, id: &Id) -> Option<(u64, PublicKey, Id)> {
        self.records.get(id).map(|r| r.order)
    }

    pub fn participants(&self, id: &SessionId) -> Vec<PublicKey> {
        self.participants_before(id, |_| true)
    }

    pub fn active_participants(&self, id: &SessionId) -> Vec<PublicKey> {
        let Some(session) = self.session(id) else { return Vec::new() };
        let Some(open) = &session.open else { return Vec::new() };
        if matches!(open.mode, SessionMode::Share { .. }) && session.leaves.iter().any(|(d, ..)| *d == open.opener) {
            return Vec::new()
        }
        let participants = self.participants(id);
        let left = self.session_departed(session, open, &participants.iter().copied().collect());
        participants.into_iter().filter(|device| !left.contains(device)).collect()
    }

    fn session_departed(&self, session: &Session, open: &Opened, participants: &HashSet<PublicKey>) -> HashSet<PublicKey> {
        let mut last_enter = HashMap::new();
        if let Some(order) = self.order_of(&open.anchor) {
            last_enter.insert(open.opener, order);
        }
        for &(device, join) in &session.joins {
            if participants.contains(&device) {
                if let Some(order) = self.order_of(&join) {
                    last_enter.entry(device).and_modify(|cur| *cur = (*cur).max(order)).or_insert(order);
                }
            }
        }
        let mut last_leave: HashMap<PublicKey, Order> = HashMap::new();
        for &(device, leave, _, _) in &session.leaves {
            if participants.contains(&device) {
                if let Some(order) = self.order_of(&leave) {
                    last_leave.entry(device).and_modify(|cur| *cur = (*cur).max(order)).or_insert(order);
                }
            }
        }
        last_leave.into_iter()
            .filter(|(device, leave)| last_enter.get(device).is_none_or(|enter| leave > enter))
            .map(|(device, _)| device).collect()
    }

    fn participants_before(&self, id: &SessionId, counted: impl Fn(&Id) -> bool) -> Vec<PublicKey> {
        let Some(session) = self.session(id) else { return Vec::new() };
        let Some(open) = session.open.as_ref().filter(|open| counted(&open.anchor)) else { return Vec::new() };
        let opener_seat = self.seat_of(&open.opener);
        let mut seen = HashSet::from([open.opener]);
        let mut devices = vec![open.opener];
        for &(device, join) in &session.joins {
            if counted(&join) && seen.insert(device) && open.admits(opener_seat, self.seat_of(&device)) {
                devices.push(device);
            }
        }
        devices
    }

    /// Only a counted human request to this AI and a fact from its owner's seat can describe a send.
    pub fn transcript_shares(&self, session: &SessionId) -> impl Iterator<Item = (&TranscriptShare, &str)> {
        self.sessions.get(session).into_iter().flat_map(|s| &s.shared).filter_map(|share| {
            let agent = self.records.get(&share.agent)?;
            let Some(Content::AgentAdded { name, .. }) = &agent.content else { return None };
            let owner = self.seat_of(&agent.order.1)?;
            let request = self.message(&share.line)?;
            (self.seat_of(&share.author) == Some(owner) && self.seat_of(&request.author) == Some(owner)
                && request.actor == Actor::Human && (request.to_agent == Some(share.agent) || request.chat.to_ai)
                && !self.void_events.contains_key(&share.agent)).then_some((share, name.as_str()))
        })
    }

    pub fn said(&self, id: &SessionId) -> Vec<Said<'_>> {
        self.said_before(id, None)
    }

    /// When sending to an AI, only include utterances known when its person signed the request.
    pub fn said_before(&self, id: &SessionId, request: Option<Id>) -> Vec<Said<'_>> {
        let Some(session) = self.session(id) else { return Vec::new() };
        let past = request.map(|id| {
            let mut past = HashSet::new();
            let Some(request) = self.records.get(&id) else { return past };
            let batches = session.said.chunk_by(|a, b| a.2 == b.2);
            let candidates = session.open.iter().map(|open| &open.anchor)
                .chain(session.joins.iter().map(|(_, join)| join))
                .chain(batches.map(|batch| &batch[0].2));
            let Some(min_height) = candidates.filter_map(|id| self.records.get(id)).map(|r| r.order.0).min() else { return past };
            let mut stack = request.parents.clone();
            while let Some(parent) = stack.pop() {
                let record = &self.records[&parent];
                if record.order.0 < min_height || !past.insert(parent) { continue }
                // At the oldest candidate's height, every parent is already too old to count.
                if record.order.0 > min_height { stack.extend_from_slice(&record.parents); }
            }
            past
        });
        let counted = |event: &Id| past.as_ref().is_none_or(|past| past.contains(event));
        let participants: HashSet<_> = self.participants_before(id, counted).into_iter().collect();
        let mut said = Vec::new();
        for batch in session.said.chunk_by(|a, b| a.2 == b.2) {
            let (_, author, event, _) = batch[0];
            if !participants.contains(&author) || !counted(&event) { continue }
            let Some(Content::Said { lines, .. }) = self.records.get(&event).and_then(|r| r.content.as_ref()) else { continue };
            for &(at_ms, _, _, index) in batch {
                if let Some(line) = lines.get(index as usize) {
                    said.push(Said { at_ms, dur_ms: line.dur_ms, author, event, index, text: &line.text });
                }
            }
        }
        // Stable ties within one record keep the original line index order.
        said.sort_by(|a, b| (a.at_ms, &a.author, &a.event).cmp(&(b.at_ms, &b.author, &b.event)));
        said
    }

    pub fn session_summary(&self, id: &SessionId, viewer_seat: Option<Id>) -> Option<SessionSummary> {
        let session = self.session(id)?;
        let open = session.open.as_ref()?;
        let opener_seat = self.seat_of(&open.opener);
        let participants: HashSet<_> = self.participants(id).into_iter().collect();
        let answered: BTreeSet<_> = participants.iter().filter_map(|d| self.seat_of(d))
            .filter(|s| Some(*s) != opener_seat).collect();
        let left = self.session_departed(session, open, &participants);
        let secs = session.leaves.iter().filter(|(device, ..)| participants.contains(device))
            .filter_map(|(_, _, duration, _)| *duration).max();
        let outcome = if !answered.is_empty() {
            Outcome::Answered { seats: answered.into_iter().collect(), secs }
        } else {
            let mut declined = BTreeSet::new();
            let mut busy = true;
            for (device, _, why) in &session.declines {
                if let Some(seat) = self.seat_of(device) {
                    if open.seen(opener_seat, Some(seat)) != Seen::Other {
                        declined.insert(seat);
                        busy &= *why == DeclineWhy::Busy;
                    }
                }
            }
            if !declined.is_empty() {
                Outcome::Declined { seats: declined.into_iter().collect(), busy }
            } else if left.len() == participants.len() {
                Outcome::Unanswered
            } else { Outcome::Pending }
        };
        let lines = session.said.iter().filter(|(_, device, _, _)| participants.contains(device)).count();
        let opener_left = session.leaves.iter().any(|(d, ..)| *d == open.opener);
        let ended = (matches!(open.mode, SessionMode::Share { .. }) && opener_left)
            || left.len() == participants.len();
        Some(SessionSummary { opener: open.opener, opener_seat, mode: open.mode.clone(),
            rung: open.to.clone(), town_wide: open.town_wide, viewer: open.seen(opener_seat, viewer_seat), outcome, lines, ended })
    }

    fn task_in(&self, project: Option<Id>, id: &Id) -> Result<&Entity, String> {
        let task = self.entities.get(id).filter(|e| e.kind == kind::TASK)
            .ok_or_else(|| format!("unknown task {}", crate::hex(id)))?;
        if task.project != project { return Err("task belongs to another project".into()) }
        Ok(task)
    }

    /// Follow task merges within a project to their live destination. In a cycle the
    /// earliest-created member survives, with its id breaking a creation tie.
    pub fn task_survivor(&self, project: Option<Id>, id: &Id) -> Result<&Entity, String> {
        self.follow_task(self.task_in(project, id)?, &mut HashMap::new())
    }

    fn follow_task<'a>(&'a self, mut task: &'a Entity, paths: &mut HashMap<Id, TaskPath<'a>>) -> Result<&'a Entity, String> {
        let mut path: Vec<&Entity> = Vec::new();
        let resolved = loop {
            let Some(Value::Id(next)) = task.value(field::MERGED_INTO) else { break Ok(task) };
            match paths.get(&task.id) {
                Some(TaskPath::Resolved(result)) => break result.clone(),
                Some(TaskPath::Visiting(start)) => {
                    break Ok(path[*start..].iter().copied().min_by_key(|t| (t.created, t.id))
                        .expect("a repeated task starts a nonempty cycle"));
                }
                None => {}
            }
            paths.insert(task.id, TaskPath::Visiting(path.len()));
            path.push(task);
            match self.task_in(task.project, next) {
                Ok(next) => task = next,
                Err(reason) => break Err(reason),
            }
        };
        for task in path { paths.insert(task.id, TaskPath::Resolved(resolved.clone())); }
        let survivor = resolved?;
        // A surviving link belongs to a cycle. Its own merge tombstone is ineffective.
        let merged_by = survivor.fields.get(&field::MERGED_INTO)
            .filter(|link| matches!(link.value, Value::Id(_))).map(|link| link.writer);
        let deleted = survivor.fields.get(&field::DELETED)
            .is_some_and(|d| d.value == Value::Bool(true) && Some(d.writer) != merged_by);
        if deleted || survivor.flag(field::ARCHIVED) { return Err("task is deleted or archived".into()) }
        Ok(survivor)
    }

    /// Visible entities matching a query, including the one live survivor of a task merge cycle.
    pub fn visible_entities<'a>(&'a self, mut include: impl FnMut(&Entity) -> bool + 'a) -> impl Iterator<Item = &'a Entity> {
        let mut paths = HashMap::new();
        self.entities.values().filter(move |&e| include(e) && self.live_entity(e, &mut paths))
    }

    fn live_entity<'a>(&'a self, e: &'a Entity, paths: &mut HashMap<Id, TaskPath<'a>>) -> bool {
        if e.kind == kind::TASK && matches!(e.value(field::MERGED_INTO), Some(Value::Id(_))) {
            return self.follow_task(e, paths).is_ok_and(|survivor| survivor.id == e.id)
        }
        !e.flag(field::DELETED) && !e.flag(field::ARCHIVED)
    }

    pub fn entity_is_visible(&self, id: &Id) -> bool {
        self.entities.get(id).is_some_and(|e| self.live_entity(e, &mut HashMap::new())
            && e.project.is_none_or(|p| self.entities.get(&p).is_some_and(|proj|
                !proj.flag(field::DELETED) && !proj.flag(field::ARCHIVED))))
    }

    pub fn doc_is_visible(&self, doc: &Id) -> bool {
        self.docs.contains_key(doc) && self.entity_is_visible(doc)
    }

    pub fn active_members(&self) -> impl Iterator<Item = PublicKey> + '_ {
        self.membership.seats.seats.values().flat_map(|seat| seat.devices.iter().copied())
    }

    pub fn admitted_devices(&self) -> impl Iterator<Item = PublicKey> + '_ {
        self.membership.seats.admitted.keys().copied()
    }

    pub fn pending_devices(&self) -> impl Iterator<Item = PublicKey> + '_ {
        let seats = &self.membership.seats;
        let mut seen = HashSet::new();
        seats.offers.values().filter_map(move |offer| {
            (!self.latest_height.contains_key(&offer.subject) && seats.open(&offer.seat)
                && seen.insert(offer.subject)).then_some(offer.subject)
        })
    }

    /// The grant a first accept can name, choosing the lowest id when several offers qualify.
    pub fn invitation(&self, device: &PublicKey) -> Option<Id> {
        let seats = &self.membership.seats;
        if self.latest_height.contains_key(device) { return None }
        seats.offers.iter().find_map(|(entry, offer)| {
            (offer.subject == *device && seats.open(&offer.seat)).then_some(*entry)
        })
    }

    /// Whether an active `actor` may remove an active `target` under the fold's invite-tree
    /// and same-seat vouch-tree rules (including self-removal; UI guards on leaving live in `Model`).
    pub fn can_remove_device(&self, actor: &PublicKey, target: &PublicKey) -> bool {
        if !self.active_member(actor) { return false }
        let Some(seat) = self.membership.seats.active_seat(target) else { return false };
        self.membership.seats.has_removal_authority(actor, seat, target, &self.records)
    }

    /// Whether an active `actor` may remove another person's active seat as a whole.
    /// Returns `false` for `actor`'s own seat and for any seat with no active devices.
    pub fn can_remove_seat(&self, actor: &PublicKey, seat: &Id) -> bool {
        let Some(own) = self.membership.seats.active_seat(actor) else { return false };
        if self.membership.seats.seats.get(seat).is_none_or(|s| s.devices.is_empty()) { return false }
        self.membership.seats.is_strict_descendant_seat(own, *seat)
    }

    /// Devices admitted by this device's own AddDevice grants, including devices that later left.
    pub fn vouched_by(&self, device: &PublicKey) -> Vec<(PublicKey, Id)> {
        self.membership.seats.admitted.iter().filter_map(|(key, admission)| {
            let grant = self.records.get(&admission.grant)?;
            (grant.order.1 == *device && matches!(grant.content, Some(Content::Member(Member::AddDevice { .. }))))
                .then_some((*key, admission.grant))
        }).collect()
    }

    /// Seats admitted by this device's own Invite grants, including seats whose devices later left.
    pub fn invited_by(&self, device: &PublicKey) -> Vec<(Id, PublicKey)> {
        self.membership.seats.seats.iter().filter_map(|(&id, seat)| {
            let admitted = self.membership.seats.admitted.get(&seat.first_device)
                .is_some_and(|a| a.seat == id && a.grant == id);
            if !admitted { return None }
            let grant = self.records.get(&id)?;
            (grant.order.1 == *device && matches!(grant.content, Some(Content::Member(Member::Invite { .. }))))
                .then_some((id, seat.first_device))
        }).collect()
    }

    /// Active devices whose removal was voided because its remover forked at or below it.
    /// Validated against real removal authority and causal cutoff, sorted in canonical
    /// `removal_event` order, and deduplicated per `(forked_device, target_device)` keeping
    /// the earliest canonical `removal_event`.
    pub fn reinstated_by_fork(&self) -> Vec<ReinstatedDevice> {
        if self.membership.forks.is_empty() { return Vec::new() }
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for &(_, op) in &self.membership.controls {
            let MembershipOp::Event(author, id) = op else { continue };
            if self.void_events.get(&id) != Some(&FORKED) { continue }
            let Some(record) = self.records.get(&id) else { continue };
            let Some(Content::Member(Member::Remove { seat, device, last })) = record.content else {
                continue
            };
            if device == author || !self.active_member(&device) { continue }
            if self.seat_of(&device) != Some(seat) { continue }
            if !self.membership.seats.has_removal_authority(&author, seat, &device, &self.records) {
                continue
            }
            if !self.records.get(&last).is_some_and(|p| p.order.1 == device)
                || !in_past(&self.records, record, last) {
                continue
            }
            if seen.insert((author, device)) {
                out.push(ReinstatedDevice {
                    forked_device: author,
                    target_device: device,
                    removal_event: id,
                });
            }
        }
        out
    }

    /// Whether `key` is admitted, currently invited, or held a surviving grant before a start-fork.
    pub fn has_standing(&self, key: &PublicKey) -> bool {
        self.seat_of(key).is_some()
            || self.invitation(key).is_some()
            || (self.forked(key).is_some()
                && self.membership.seats.offers.values().any(|o| o.subject == *key))
    }

    /// Start-forked devices that held a surviving offer and therefore still qualify for a
    /// bounded final exchange despite having no admitted seat.
    pub fn forked_eligible_devices(&self) -> impl Iterator<Item = PublicKey> + '_ {
        self.membership.forks.keys().copied()
            .filter(|key| self.seat_of(key).is_none() && self.has_standing(key))
    }

    /// First counted removal or complete fork proof for either participant in local arrival order,
    /// returning its arrival position and the 1 (removal) or 2 (fork siblings) proof event ids.
    pub fn final_boundary(&self, log: &crate::log::Log, a: &PublicKey, b: &PublicKey) -> Option<FinalBoundary> {
        let removal = self.membership.removals.iter().filter_map(|(&id, decision)| {
            if *decision != Ok(Removal::Counted) { return None }
            match &self.records.get(&id)?.content {
                Some(Content::Member(Member::Remove { device, .. })) if device == a || device == b => {
                    Some((log.position(&id)?, id))
                }
                _ => None,
            }
        }).min_by_key(|&(pos, _)| pos);
        let limit = removal.map_or(usize::MAX, |(pos, _)| pos);
        if self.forked(a).is_some() || self.forked(b).is_some() {
            let mut seen: HashMap<(PublicKey, ForkPoint), Id> = HashMap::new();
            for (at, event) in log.events().iter().enumerate().take(limit) {
                let author = event.header.author;
                if (&author == a || &author == b) && self.membership.forks.contains_key(&author) {
                    if let Some(record) = self.records.get(&event.id) {
                        let (_, parent) = self.own_predecessor(author, &record.parents);
                        if let Some(&first) = seen.get(&(author, parent)) {
                            return Some(FinalBoundary { at, evidence: vec![first, event.id] });
                        }
                        seen.insert((author, parent), event.id);
                    }
                }
            }
        }
        removal.map(|(at, id)| FinalBoundary { at, evidence: vec![id] })
    }

    /// Earliest local arrival boundary per device. The earlier of two devices' entries
    /// is the boundary returned by `final_boundary` for that pair.
    pub fn device_final_boundaries(&self, log: &crate::log::Log) -> BTreeMap<PublicKey, (usize, FinalClosure)> {
        let mut out: BTreeMap<PublicKey, (usize, FinalClosure)> = BTreeMap::new();
        for (&id, decision) in &self.membership.removals {
            if *decision != Ok(Removal::Counted) { continue }
            let Some(Content::Member(Member::Remove { device, .. })) =
                self.records.get(&id).and_then(|r| r.content.as_ref())
            else { continue };
            let Some(pos) = log.position(&id) else { continue };
            if out.get(device).is_none_or(|&(old_pos, _)| pos < old_pos) {
                out.insert(*device, (pos, FinalClosure::Removal(id)));
            }
        }
        if !self.membership.forks.is_empty() {
            let mut pending: HashSet<_> = self.membership.forks.keys().copied().collect();
            let mut seen: HashMap<(PublicKey, ForkPoint), Id> = HashMap::new();
            for (at, event) in log.events().iter().enumerate() {
                if pending.is_empty() { break }
                let author = event.header.author;
                if !pending.contains(&author) { continue }
                if out.get(&author).is_some_and(|&(rem_pos, _)| at >= rem_pos) {
                    pending.remove(&author);
                    continue;
                }
                if let Some(record) = self.records.get(&event.id) {
                    let (_, parent) = self.own_predecessor(author, &record.parents);
                    if let Some(&first) = seen.get(&(author, parent)) {
                        if let Some(closure) = FinalClosure::fork(first, event.id) {
                            out.insert(author, (at, closure));
                        }
                        pending.remove(&author);
                    } else {
                        seen.insert((author, parent), event.id);
                    }
                }
            }
        }
        out
    }

    /// First counted removal or complete fork proof for either participant in local arrival order.
    pub fn first_removal(&self, log: &crate::log::Log, a: &PublicKey, b: &PublicKey) -> Option<usize> {
        self.final_boundary(log, a, b).map(|boundary| boundary.at)
    }

    fn classify(&mut self, r: &Record) -> bool {
        let id = r.order.2;
        let reason = match r.content {
            _ if r.structurally_void => self.membership.fork_reason(r).or(Some("requires a human author")),
            Some(Content::Town { policy, .. }) => {
                if policy != GROUP_POLICY { self.unknown_policy = Some(policy); }
                None
            }
            _ if r.control => self.membership.reasons.get(&id).copied(),
            _ => self.membership.ordinary(r),
        };
        if r.kind == crate::event::kind::PROFILE {
            let candidate = (reason.is_none(), r.order, id);
            let latest = self.latest_profiles.entry(r.order.1).or_insert(candidate);
            if candidate > *latest { *latest = candidate; }
        }
        if let Some(reason) = reason {
            self.void_events.insert(id, reason);
            return false
        }
        self.index_content(r);
        true
    }

    fn remember_reading(&mut self, event: &Event, record: &Record) {
        let ids = match reading(event, record.content.as_ref()) {
            Reading::Full => return,
            Reading::SkippedTag => &mut self.partial,
            Reading::Unknown => &mut self.unknown,
            Reading::UnknownCritical => &mut self.unknown_critical,
        };
        if ids.last().is_none_or(|id| self.records[id].order < record.order) {
            ids.push(event.id);
        } else {
            let at = ids.partition_point(|id| self.records[id].order < record.order);
            ids.insert(at, event.id);
        }
    }

    fn index_prefix<'a>(&mut self, ordered: impl Iterator<Item = &'a Event>, mut saved: Option<&mut HashMap<Id, Reading>>) -> bool {
        for event in ordered {
            if self.records.contains_key(&event.id) { continue }
            let (record, _) = self.index_record(event);
            if let Some(saved) = saved.as_deref_mut() {
                if !saved.is_empty() {
                    if let Some(was) = saved.remove(&event.id) {
                        if reading(event, record.content.as_ref()) != was { return false }
                    }
                }
            } else {
                self.remember_reading(event, &record);
            }
            if record.control {
                let (height, author, id) = record.order;
                self.membership.controls.push((height, MembershipOp::Event(author, id)));
            }
        }
        self.membership.replay(&self.records);
        self.rebuild_indexes();
        saved.is_none_or(|unseen| unseen.is_empty())
    }

    fn rebuild_indexes(&mut self) {
        self.void_events.clear();
        self.latest_profiles.clear();
        self.retracted.clear();
        self.entity_writes.clear();
        self.policy_sets.clear();
        self.possible_links.clear();
        self.undo_children.clear();
        for i in 0..self.order.len() {
            let rec = self.records[&self.order[i].2].clone();
            if self.classify(&rec) { self.index_retract(&rec); }
        }
    }

    // Called only for a record that survives membership replay. An ancestor is necessarily earlier.
    fn index_retract(&mut self, record: &Record) -> Option<Id> {
        let Some(Content::Retract { of }) = record.content else { return None };
        let target = self.records.get(&of)?;
        let seat = self.seat_of(&record.order.1)?;
        if target.kind != crate::event::kind::CHAT || self.seat_of(&target.order.1) != Some(seat)
            || !(record.parents.contains(&of) || in_past(&self.records, record, of)) { return None }
        self.retracted.insert(of).then_some(of)
    }

    fn hide_message(&mut self, id: Id) {
        if let Some(at) = self.timeline_index(id) { self.timeline.remove(at); }
        if let Some(at) = self.chat_positions.remove(&id) {
            let removed = self.chat.remove(at);
            if at < self.chat.len() {
                for pos in self.chat_positions.values_mut() {
                    if *pos > at { *pos -= 1; }
                }
            }
            if let Some(agent) = removed.to_agent {
                if let Some(mentions) = self.mentions.get_mut(&agent) {
                    mentions.retain(|line| *line != id);
                    if mentions.is_empty() { self.mentions.remove(&agent); }
                }
            }
            if let Some(agent) = removed.author_agent.filter(|agent| self.last_replies.get(agent) == Some(&id)) {
                if let Some(previous) = self.chat[..at].iter().rev().find(|m| m.author_agent == Some(agent)) {
                    self.last_replies.insert(agent, previous.id);
                } else {
                    self.last_replies.remove(&agent);
                }
            }
        }
        for msg in &mut self.chat {
            if msg.chat.quote == Some(id) { msg.chat.quote = None; }
        }
    }

    fn ready(&mut self, event: &Event) {
        let last = self.order.last().copied();
        let prior_height = self.latest_height.get(&event.header.author).copied();
        let (record, new_cut) = self.index_record(event);
        self.remember_reading(event, &record);
        let order = record.order;
        let late = last.is_some_and(|last| order < last);
        let mut replay = new_cut;
        let mut revised = false;
        if record.control {
            let step = (order.0, MembershipOp::Event(order.1, order.2));
            let at = self.membership.controls.partition_point(|o| *o < step);
            replay |= at < self.membership.controls.len();
            self.membership.controls.insert(at, step);
            if !replay {
                revised = self.membership.decide(at, &self.records).is_some_and(|(device, cut)| {
                    let latest = if device == order.1 { prior_height } else { self.latest_height.get(&device).copied() };
                    latest.is_some_and(|height| height > cut)
                });
            }
        }
        if replay { self.membership.replay(&self.records); }
        if replay || revised || (record.control && late) {
            self.rebuild_indexes();
            self.scopes.clear();
            self.timelines.clear();
            self.docs.clear();
            self.rebuild_board();
            return
        }
        if !self.classify(&record) {
            if late {
                self.rebuild_board();
            }
            return
        }
        if let Some(of) = self.index_retract(&record) {
            if !late { self.hide_message(of); }
        }
        let position = if late { self.order.partition_point(|other| *other < order) } else { self.order.len() - 1 } as u64;
        let coupled = self.has_board_effect(&record);
        if late && matches!(record.content, Some(Content::Entity { derive: Some(_), .. })) {
            self.rebuild_board();
            return
        }
        if coupled && !late { self.apply_board(&record, position, None); }
        if let Some(doc) = self.doc_scope(&record) {
            self.scopes.insert(event.id, doc);
            if late {
                // Entity positions are public zero-based log ranks. Only their ranks shift.
                for e in self.entities.values_mut() {
                    if e.created >= position { e.created += 1 }
                    if e.last_activity >= position { e.last_activity += 1 }
                    if e.order_written >= position { e.order_written += 1 }
                    if e.z_written >= position { e.z_written += 1 }
                }
            }
            let mut changed_tasks = self.schedule_doc(doc, order);
            if late {
                let mut arrived = vec![order];
                while let Some(target) = arrived.pop() {
                    for undo in self.undo_children.get(&target.2).cloned().unwrap_or_default() {
                        if undo > target && !self.scopes.contains_key(&undo.2) {
                            self.scopes.insert(undo.2, doc);
                            changed_tasks |= self.schedule_doc(doc, undo);
                            arrived.push(undo);
                        }
                    }
                }
                if changed_tasks || coupled { self.replay_linked(doc, order); }
                else if let Some(state) = self.docs.get_mut(&doc) {
                    for (task, value) in &mut state.task_states {
                        *value = checkbox(self.statuses.get(task).and_then(|w| w.last_key_value()).map(|(_, v)| v.clone()));
                    }
                }
            } else {
                self.apply_ticks(&record, position);
            }
        } else if late {
            self.rebuild_board();
        } else if !coupled {
            self.apply_board(&record, position, None);
        }
    }

    fn has_board_effect(&self, rec: &Record) -> bool {
        match &rec.content {
            Some(Content::Entity { derive: Some(_), .. }) => true,
            Some(Content::Undo { of }) => self.records.get(of).is_some_and(|target| target.order < rec.order)
                && (self.conversions.contains_key(of) || self.groups.contains_key(of)),
            _ => false,
        }
    }

    fn doc_scope(&self, rec: &Record) -> Option<Id> {
        match rec.content.as_ref()? {
            Content::Text { doc, .. } | Content::Entity { derive: Some((doc, _)), .. } => Some(*doc),
            Content::Undo { of } if self.records.get(of).is_some_and(|target| target.order < rec.order) => self.scopes.get(of).copied(),
            _ => None,
        }
    }

    fn schedule_doc(&mut self, doc: Id, order: Order) -> bool {
        let timeline = self.timelines.entry(doc).or_default();
        let at = timeline.events.partition_point(|o| *o < order);
        timeline.events.insert(at, order);
        let Some(state) = self.docs.get_mut(&doc) else { return false };
        let mut changed_tasks = false;
        let mut from = at;
        if at + 1 != timeline.events.len() {
            timeline.checkpoints.retain(|(count, _)| *count <= at);
            if let Some((count, snapshot)) = timeline.checkpoints.last() {
                *state = (**snapshot).clone();
                from = *count;
            } else {
                *state = DocState::new(doc);
                from = 0;
            }
        }
        for index in from..timeline.events.len() {
            let record = &self.records[&timeline.events[index].2];
            let id = record.order.2;
            let status = |task| self.statuses.get(&task).and_then(|writes| writes.range(..record.order).next_back()).map(|(order, v)| (v.clone(), order.2));
            let effects = match &record.content {
                Some(Content::Text { ops, .. }) => {
                    let mut checked = BTreeMap::new();
                    state.apply_event(id, ops, |origin| *checked.entry(origin).or_insert_with(|| in_past(&self.records, record, origin)), record.actor, status)
                }
                Some(Content::Entity { derive: Some(_), .. }) => {
                    if let Some(&(_, at, task)) = self.conversions.get(&id) {
                        let initial = self.statuses.get(&task).and_then(|w| w.get(&record.order)).cloned().unwrap_or(Value::Null);
                        state.apply_event(id, &[TextOp::Attr { at, attr: attr::TASK, value: Value::Id(task) }], |_| true,
                            record.actor, |_| Some((initial.clone(), id)))
                    } else { TaskEffects::default() }
                }
                Some(Content::Undo { of }) => { state.undo(id, *of); TaskEffects::default() }
                _ => TaskEffects::default(),
            };
            for task in effects.links { self.linked_docs.entry(task).or_default().insert(doc); }
            if effects.writes.is_empty() {
                changed_tasks |= self.tick_writes.remove(&id).is_some();
            } else if self.tick_writes.get(&id) != Some(&effects.writes) {
                changed_tasks = true;
                self.tick_writes.insert(id, effects.writes);
            }
            if (index + 1) % CHECKPOINT_INTERVAL == 0 {
                if timeline.checkpoints.len() == 2 { timeline.checkpoints.remove(0); }
                let mut snapshot = state.clone();
                // Resolved task state is a view of another entity and is refreshed after a rewind.
                for status in snapshot.task_states.values_mut() { *status = Value::Null; }
                timeline.checkpoints.push((index + 1, Arc::new(snapshot)));
            }
        }
        if at + 1 != timeline.events.len() {
            for (task, status) in &mut state.task_states {
                *status = checkbox(self.statuses.get(task).and_then(|w| w.last_key_value()).map(|(_, v)| v.clone()));
            }
        }
        changed_tasks
    }

    fn apply_ticks(&mut self, rec: &Record, position: u64) {
        if let Some(writes) = self.tick_writes.get(&rec.order.2).cloned() {
            let mut replaced = Replaced::new();
            for (task, (value, _, _)) in writes {
                let prior = self.write(task, field::STATUS, value, rec.order.2, rec.order.2, position);
                replaced.insert((task, field::STATUS), prior);
            }
            self.groups.insert(rec.order.2, replaced);
        }
    }

    // A tick writes a Task field, and unlink reads that field at its canonical position.
    // Repair only the connected tasks and lines when a late edit changes such a write.
    fn replay_linked(&mut self, doc: Id, since: Order) {
        let mut docs = BTreeSet::from([doc]);
        let mut tasks = BTreeSet::new();
        loop {
            for id in &docs {
                if let Some(links) = self.possible_links.get(id) { tasks.extend(links); }
            }
            let before = docs.len();
            for (id, links) in &self.possible_links {
                if !tasks.is_disjoint(links) { docs.insert(*id); }
            }
            if docs.len() == before { break }
        }
        tasks.retain(|id| self.entities.get(id).is_some_and(|e| e.kind == kind::TASK));
        self.groups.retain(|_, group| {
            group.retain(|(id, _), _| !tasks.contains(id));
            !group.is_empty()
        });
        self.cleared.retain(|(id, _), _| !tasks.contains(id));
        self.copied_from.retain(|(_, entity, _), _| !tasks.contains(entity));
        for id in &tasks {
            self.entities.remove(id);
            self.derived_task_creators.remove(id);
            self.statuses.remove(id);
            self.linked_docs.remove(id);
        }
        // Keep the nearest valid checkpoints for the affected documents. Task registers
        // have their own history and are rebuilt from their creation through the same order.
        let mut events = BTreeSet::new();
        for task in &tasks {
            if let Some(writes) = self.entity_writes.get(task) { events.extend(writes.iter().copied()); }
        }
        let mut resume = BTreeMap::new();
        for id in &docs {
            let Some(state) = self.docs.get_mut(id) else { continue };
            let timeline = self.timelines.entry(*id).or_default();
            events.extend(timeline.events.iter().copied());
            let at = timeline.events.partition_point(|o| *o < since);
            timeline.checkpoints.retain(|(n, _)| *n <= at);
            let prefix = if let Some((count, snapshot)) = timeline.checkpoints.last() {
                *state = (**snapshot).clone();
                *count
            } else {
                *state = DocState::new(*id);
                0
            };
            timeline.events.truncate(prefix);
            resume.insert(*id, timeline.events.last().copied());
            for task in state.task_states.keys() { self.linked_docs.entry(*task).or_default().insert(*id); }
        }
        let mut targets: Vec<_> = events.iter().copied().collect();
        while let Some(target) = targets.pop() {
            if let Some(undos) = self.undo_children.get(&target.2) {
                for &undo in undos.range(target..) {
                    if events.insert(undo) { targets.push(undo); }
                }
            }
        }
        for order in events {
            let rec = self.records[&order.2].clone();
            let pos = self.order.partition_point(|o| *o < order) as u64;
            if matches!(rec.content, Some(Content::Entity { kind: Key::Number(kind::TASK), .. } | Content::Set { .. } | Content::Undo { .. })) {
                self.apply_board(&rec, pos, Some(&tasks));
            }
            if let Some(doc) = self.scopes.get(&rec.order.2).copied().filter(|d| docs.contains(d)) {
                if resume.get(&doc).is_some_and(|last| last.is_none_or(|last| rec.order > last)) {
                    self.schedule_doc(doc, rec.order);
                }
                self.apply_ticks(&rec, pos);
            }
        }
        for doc in docs {
            if let Some(state) = self.docs.get_mut(&doc) {
                for (task, value) in &mut state.task_states {
                    *value = checkbox(self.statuses.get(task).and_then(|w| w.last_key_value()).map(|(_, v)| v.clone()));
                }
            }
        }
    }

    fn rebuild_board(&mut self) {
        self.entities.clear();
        self.policy_writes.clear();
        self.town_policy = TownPolicy::Members;
        self.copied_from.clear();
        self.invalid_keys.clear();
        self.profiles.clear();
        self.chat.clear();
        self.sessions.clear();
        self.services = ServiceFacts::default();
        self.timeline.clear();
        self.agents.clear();
        self.chat_positions.clear();
        self.mentions.clear();
        self.last_replies.clear();
        self.numbers.clear();
        self.groups.clear();
        self.cleared.clear();
        self.task_numbers.clear();
        self.conversions.clear();
        self.derived_task_creators.clear();
        self.statuses.clear();
        self.linked_docs.clear();
        self.tick_writes.clear();
        // Global entity and project events may affect task numbering or make a document
        // available. Independent existing text projections do not read any board register.
        let independent: BTreeSet<_> = self.docs.keys().filter(|d| !self.possible_links.contains_key(*d)).copied().collect();
        self.docs.retain(|doc, _| independent.contains(doc));
        self.timelines.retain(|doc, _| independent.contains(doc));
        self.scopes.retain(|_, doc| independent.contains(doc));
        let ordered: Vec<_> = self.order.iter().map(|o| self.records[&o.2].clone()).collect();
        for (pos, rec) in ordered.iter().enumerate() {
            if self.void_events.contains_key(&rec.order.2) {
                continue
            }
            let scope = self.doc_scope(rec);
            if scope.is_none() || self.has_board_effect(rec) {
                self.apply_board(rec, pos as u64, None);
            }
            if let Some(doc) = scope {
                if independent.contains(&doc) { continue }
                self.scopes.insert(rec.order.2, doc);
                self.schedule_doc(doc, rec.order);
                self.apply_ticks(rec, pos as u64);
            }
        }
    }

    fn write(&mut self, id: Id, field: Key, value: Value, writer: Id, origin: Id, position: u64) -> Option<FieldState> {
        let entity = self.entities.get_mut(&id)?;
        if entity.kind == kind::TASK && field == field::STATUS {
            self.statuses.entry(id).or_default().insert(self.records[&writer].order, value.clone());
            if let Some(docs) = self.linked_docs.get(&id) {
                for doc in docs {
                    self.docs.get_mut(doc).unwrap().task_states.insert(id, checkbox(Some(value.clone())));
                }
            }
        }
        let clear = value == Value::Null;
        let refresh_visibility = entity.kind == kind::TOWN && (field == field::DELETED || field == field::ARCHIVED);
        let policy = (field == field::POLICY).then(|| value.clone());
        let prior = entity.write(field.clone(), value, writer, origin, position);
        let cleared = self.cleared.remove(&(id, field.clone()));
        if clear {
            entity.fields.remove(&field);
            self.cleared.insert((id, field), writer);
        }
        if let Some(value) = policy {
            let rec = self.records[&writer].clone();
            self.record_policy_write(&rec, id, value);
        } else if refresh_visibility {
            self.refresh_town_policy();
        }
        prior.or_else(|| cleared.map(|writer| FieldState { writer, origin: writer, value: Value::Null }))
    }

    fn record_policy_write(&mut self, rec: &Record, entity: Id, value: Value) {
        self.policy_writes.retain(|&(event, ent), _| ent != entity || !in_past(&self.records, rec, event));
        if value != Value::Null { self.policy_writes.insert((rec.order.2, entity), value); }
        self.refresh_town_policy();
    }

    fn refresh_town_policy(&mut self) {
        let unreadable = self.policy_writes.iter().any(|((_, entity), value)| {
            *value != Value::Int(1) && self.entities.get(entity).is_some_and(|e|
                e.kind == kind::TOWN && e.project.is_none() && !e.flag(field::DELETED) && !e.flag(field::ARCHIVED))
        });
        self.town_policy = if unreadable { TownPolicy::Unreadable } else { TownPolicy::Members };
    }

    fn permits_field(&self, entity: &Entity, field: &Key, author: &PublicKey, critical: bool) -> bool {
        let town = entity.kind == kind::TOWN && entity.project.is_none();
        if *field == field::POLICY || (town && (*field == field::DELETED || *field == field::ARCHIVED)
            && (entity.fields.contains_key(&field::POLICY) || self.policy_writes.keys().any(|(_, ent)| *ent == entity.id))) {
            return town && critical && self.founder_seat().is_some_and(|s| self.seat_of(author) == Some(s))
        }
        !self.owner_only(entity, field) || self.seat_of(author).is_some_and(|seat| Some(seat) == self.seat_of(&entity.author))
    }

    /// Fields only the entity author's seat may write: a finding's note and its deletion, and
    /// every field of a mark drawn on a finding (a STROKE whose PARENT is an ISSUE).
    fn owner_only(&self, entity: &Entity, field: &Key) -> bool {
        if entity.kind == kind::ISSUE { return *field == field::TEXT || *field == field::DELETED }
        entity.kind == kind::STROKE && matches!(entity.value(field::PARENT),
            Some(Value::Id(parent)) if self.entities.get(parent).is_some_and(|p| p.kind == kind::ISSUE))
    }

    fn accepts_write(&mut self, field: &Key, value: &Value, author: &PublicKey) -> bool {
        if field.accepts(value) { return true }
        if field.is_order_key() { self.invalid_keys.insert(*author); }
        false
    }

    fn apply_board(&mut self, rec: &Record, position: u64, only: Option<&BTreeSet<Id>>) {
        let (_, author, id) = rec.order;
        match &rec.content {
            Some(Content::Profile { name, colour }) => {
                self.profiles.insert(author, Profile { name: name.clone(), colour: *colour, writer: id });
            }
            Some(Content::AgentAdded { name, kind }) => {
                self.agents.insert(id, Agent { owner: author, name: name.clone(), kind: kind.clone(), custom_name: None });
            }
            Some(Content::AgentRemoved { agent }) => {
                if self.agents.get(agent).is_some_and(|a| a.owner == author) {
                    self.agents.remove(agent);
                }
            }
            Some(Content::AgentRenamed { agent, name }) => {
                let Some(a) = self.agents.get(agent) else { return };
                if a.owner != author && !self.seat_of(&a.owner).is_some_and(|seat| self.seat_of(&author) == Some(seat)) {
                    return
                }
                if let Some(a) = self.agents.get_mut(agent) {
                    a.custom_name = (!name.trim().is_empty()).then(|| name.clone());
                }
            }
            Some(Content::Chat(chat) | Content::AgentChat { chat, .. }) => {
                if self.message_retracted(&id) { return }
                let (author_agent, to_agent) = match &rec.content {
                    Some(Content::AgentChat { agent: ChatAgent::Author(agent), .. })
                        if rec.actor == Actor::Ai && self.agents.get(agent).is_some_and(|a| a.owner == author) => (Some(*agent), None),
                    Some(Content::AgentChat { agent: ChatAgent::Recipient(agent), .. })
                        if rec.actor == Actor::Human && self.agents.contains_key(agent) => (None, Some(*agent)),
                    // An unverified tag confers no attribution or routing. Keep the signed line.
                    _ => (None, None),
                };
                self.chat_positions.insert(id, self.chat.len());
                if let Some(agent) = author_agent { self.last_replies.insert(agent, id); }
                if let Some(agent) = to_agent { self.mentions.entry(agent).or_default().push(id); }
                let mut chat = chat.clone();
                chat.quote = chat.quote.filter(|q| !self.message_retracted(q));
                // Mentions are human town chat only, canonical, of known seats other than the author's own.
                if rec.actor != Actor::Human || chat.to_ai || matches!(&rec.content, Some(Content::AgentChat { .. })) {
                    chat.mentions.clear();
                } else if !chat.mentions.is_empty() {
                    let author_seat = self.seat_of(&author);
                    chat.mentions.retain(|seat| Some(*seat) != author_seat && self.membership.seats.seats.contains_key(seat));
                    chat.mentions.sort_unstable();
                    chat.mentions.dedup();
                }
                self.chat.push(ChatMsg { id, author, actor: rec.actor, via: rec.via, wall_ms: rec.wall_ms, chat, author_agent, to_agent });
                if only.is_none() { self.timeline.push(Entry::Chat(id)); }
            }
            Some(Content::Session(op)) => {
                if only.is_some() { return }
                match op {
                    SessionOp::Open { session, mode, to, town_wide } => {
                        let open = &mut self.sessions.entry(*session).or_default().open;
                        if open.is_none() {
                            *open = Some(Opened { anchor: id, opener: author, mode: mode.clone(), to: to.clone(), town_wide: *town_wide });
                            self.timeline.push(Entry::Session(*session));
                        }
                    }
                    SessionOp::Join { session } => self.sessions.entry(*session).or_default().joins.push((author, id)),
                    SessionOp::Leave { session, secs, why } => self.sessions.entry(*session).or_default().leaves.push((author, id, *secs, *why)),
                    SessionOp::Decline { session, why } => self.sessions.entry(*session).or_default().declines.push((author, id, *why)),
                }
            }
            Some(Content::Provides { .. }) => {
                if only.is_none() { self.services.offers.insert(author, id); }
            }
            Some(Content::Request { session, job, .. }) => {
                if only.is_some() { return }
                self.services.sessions.entry(*session).or_default().insert(*job);
                self.services.histories.entry(*job).or_default().push(id);
            }
            Some(Content::Taken { job } | Content::Done { job, .. } | Content::Failed { job, .. }) => {
                if only.is_some() { return }
                // A request in this report's past necessarily sorts before it.
                if let Some(history) = self.services.histories.get_mut(job) { history.push(id); }
            }
            Some(Content::Shared { session, agent, line }) => {
                if only.is_none() { self.sessions.entry(*session).or_default().shared.push(TranscriptShare { author, agent: *agent, line: *line }); }
            }
            Some(Content::Said { session, lines }) => {
                if only.is_some() { return }
                let said = &mut self.sessions.entry(*session).or_default().said;
                said.extend(lines.iter().zip(0u32..).map(|(line, i)| (line.at_ms, author, id, i)));
            }
            Some(Content::Entity { kind, project, derive, fields, copied_from }) => {
                if matches!(kind, Key::Name(s) if !crate::literal::is_identifier(s)) { return }
                let target = derive.map(|(doc, at)| crate::entity::derived_task(doc, at)).unwrap_or(id);
                if only.is_some_and(|ids| !ids.contains(&target)) { return }
                if project.is_some_and(|id| !self.entities.contains_key(&id)) { return }
                let entity_id = if let Some((doc, at)) = derive {
                    if *kind != kind::TASK || self.entities.get(doc).is_none_or(|d| d.project != *project) { return }
                    let Some(marker) = self.docs.get(doc).and_then(|d| d.delimiter(*at)) else { return };
                    if !in_past(&self.records, rec, at.event) { return }
                    let task = crate::entity::derived_task(*doc, *at);
                    if self.entities.contains_key(&task) { return }
                    self.conversions.insert(id, (*doc, marker, task));
                    self.derived_task_creators.insert(task, id);
                    task
                } else { id };
                self.entities.insert(entity_id, Entity {
                    id: entity_id, kind: kind.clone(), project: *project, author, actor: rec.actor, created: position,
                    last_activity: position, fields: BTreeMap::new(), order_written: position, z_written: position,
                });
                if only.is_none() && derive.is_none() && *kind == kind::CLIP { self.timeline.push(Entry::Clip(entity_id)); }
                if *kind == kind::TASK { self.statuses.entry(entity_id).or_default().insert(rec.order, Value::Null); }
                for (field, value) in fields {
                    if self.accepts_write(field, value, &author) && self.permits_field(&self.entities[&entity_id], field, &author, rec.critical) {
                        self.write(entity_id, field.clone(), value.clone(), id, id, position);
                    }
                }
                let mut refs = copied_from.clone();
                normalize_copy_refs(&mut refs);
                for r in refs {
                    if r.target_entity.is_none() && (self.entities[&entity_id].fields.contains_key(&r.target_field)
                        || self.cleared.contains_key(&(entity_id, r.target_field.clone()))) {
                        self.copied_from.insert((id, entity_id, r.target_field), r.sources);
                    }
                }
                if *kind == kind::TASK && only.is_none() {
                    let number = self.numbers.entry(author).or_default();
                    *number += 1;
                    self.task_numbers.insert(entity_id, *number);
                }
                if *kind == kind::DOC { self.docs.entry(id).or_insert_with(|| DocState::new(id)); }
            }
            Some(Content::Set { changes, copied_from, .. }) => {
                let mut replaced = Replaced::new();
                for change in changes {
                    if !self.accepts_write(&change.field, &change.value, &author) { continue }
                    if only.is_some_and(|ids| !ids.contains(&change.entity)) { continue }
                    let Some(entity) = self.entities.get(&change.entity) else { continue };
                    if !self.permits_field(entity, &change.field, &author, rec.critical) { continue }
                    if entity.kind == kind::ISSUE && change.field == field::STATUS
                        && !matches!(&change.value, Value::Text(s) if matches!(s.as_str(), "open" | "resolved")) { continue }
                    if field::geometry(&change.field) && entity.flag(field::LOCKED) {
                        continue;
                    }
                    let prior = self.write(change.entity, change.field.clone(), change.value.clone(), id, id, position);
                    replaced.entry((change.entity, change.field.clone())).or_insert(prior);
                }
                let mut refs = copied_from.clone();
                normalize_copy_refs(&mut refs);
                for r in refs {
                    if let Some(ent) = r.target_entity {
                        if replaced.contains_key(&(ent, r.target_field.clone())) {
                            self.copied_from.insert((id, ent, r.target_field), r.sources);
                        }
                    }
                }
                if !replaced.is_empty() { self.groups.entry(id).or_default().extend(replaced); }
            }
            Some(Content::Undo { of }) => {
                if self.records.get(of).is_none_or(|target| target.order >= rec.order) { return }
                if let Some(&(_, _, task)) = self.conversions.get(of) {
                    if only.is_some_and(|ids| !ids.contains(&task)) { return }
                    let entity = &self.entities[&task];
                    if entity.created == entity.last_activity {
                        let prior = self.write(task, field::DELETED, Value::Bool(true), id, id, position);
                        self.groups.insert(id, [((task, field::DELETED), prior)].into());
                    }
                    return
                }
                // Undoing a creation tombstones what it made; a redo restores the prior DELETED state.
                // Later edits, the other person's included, stay in the fields and return with it.
                if matches!(self.records[of].content, Some(Content::Entity { derive: None, .. })) {
                    if only.is_some_and(|ids| !ids.contains(of)) || !self.entities.contains_key(of) { return }
                    if !self.permits_field(&self.entities[of], &field::DELETED, &author, rec.critical) { return }
                    let prior = self.write(*of, field::DELETED, Value::Bool(true), id, id, position);
                    self.groups.insert(id, [((*of, field::DELETED), prior)].into());
                    return
                }
                let Some(target) = self.groups.get(of).cloned() else { return };
                let mut replaced = Replaced::new();
                for ((entity, field), before) in target {
                    if only.is_some_and(|ids| !ids.contains(&entity)) { continue }
                    if self.entities.get(&entity).is_some_and(|e| !self.permits_field(e, &field, &author, rec.critical)) { continue }
                    let writer = self.entities[&entity].fields.get(&field).map(|f| f.writer)
                        .or_else(|| self.cleared.get(&(entity, field.clone())).copied());
                    if writer == Some(*of) {
                        let origin = before.as_ref().map_or(id, |f| f.origin);
                        let value = before.map_or(Value::Null, |f| f.value);
                        let prior = self.write(entity, field.clone(), value, id, origin, position);
                        replaced.insert((entity, field), prior);
                    }
                }
                if !replaced.is_empty() { self.groups.entry(id).or_default().extend(replaced); }
            }
            Some(Content::Member(Member::Accept { .. })) if only.is_none() => self.timeline.push(Entry::Member(id)),
            Some(Content::Town { .. } | Content::Member(_) | Content::Text { .. } | Content::Retract { .. }) => {}
            None if rec.erased_chat && only.is_none() && !self.message_retracted(&id) => self.timeline.push(Entry::Chat(id)),
            None => {}
        }
    }
}

/// Whether a chat message is one `me`'s AI may read as addressed to it: `me`'s own message, typed
/// by the person (not by an AI), with the `@` field set.
pub fn addresses_my_ai(msg: &ChatMsg, me: &PublicKey) -> bool {
    &msg.author == me && msg.actor == Actor::Human && msg.chat.to_ai
}

// Binary codec for fold checkpoints and the independently versioned local AgentBook.
// Each file owns its envelope/version. The signed log has its own wire format.
pub(crate) mod cache {
    use std::{collections::{BTreeMap, BTreeSet}, sync::Arc};

    pub(crate) trait Cached: Sized {
        fn save(&self, out: &mut Vec<u8>);
        fn load(input: &mut &[u8]) -> Option<Self>;
    }
    pub(super) fn take<'a>(input: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
        let (bytes, rest) = input.split_at_checked(n)?;
        *input = rest;
        Some(bytes)
    }

    macro_rules! integers {
        ($($ty:ty),+) => { $(impl Cached for $ty {
            fn save(&self, out: &mut Vec<u8>) { out.extend(self.to_le_bytes()); }
            fn load(input: &mut &[u8]) -> Option<Self> {
                Some(Self::from_le_bytes(take(input, std::mem::size_of::<Self>())?.try_into().ok()?))
            }
        })+ };
    }
    integers!(u8, u16, u32, u64);
    impl Cached for bool {
        fn save(&self, out: &mut Vec<u8>) { out.push(u8::from(*self)); }
        fn load(input: &mut &[u8]) -> Option<Self> {
            match u8::load(input)? { 0 => Some(false), 1 => Some(true), _ => None }
        }
    }
    impl Cached for usize {
        fn save(&self, out: &mut Vec<u8>) { (*self as u64).save(out); }
        fn load(input: &mut &[u8]) -> Option<Self> { u64::load(input)?.try_into().ok() }
    }
    impl<const N: usize> Cached for [u8; N] {
        fn save(&self, out: &mut Vec<u8>) { out.extend(self); }
        fn load(input: &mut &[u8]) -> Option<Self> { take(input, N)?.try_into().ok() }
    }
    impl Cached for String {
        fn save(&self, out: &mut Vec<u8>) { self.len().save(out); out.extend(self.as_bytes()); }
        fn load(input: &mut &[u8]) -> Option<Self> {
            let len = usize::load(input)?;
            Some(std::str::from_utf8(take(input, len)?).ok()?.to_owned())
        }
    }
    impl<T: Cached> Cached for Option<T> {
        fn save(&self, out: &mut Vec<u8>) {
            out.push(u8::from(self.is_some()));
            if let Some(value) = self { value.save(out); }
        }
        fn load(input: &mut &[u8]) -> Option<Self> {
            match u8::load(input)? { 0 => Some(None), 1 => Some(Some(T::load(input)?)), _ => None }
        }
    }
    impl<T: Cached> Cached for Arc<T> {
        fn save(&self, out: &mut Vec<u8>) { (**self).save(out); }
        fn load(input: &mut &[u8]) -> Option<Self> { T::load(input).map(Arc::new) }
    }
    impl<A: Cached, B: Cached> Cached for (A, B) {
        fn save(&self, out: &mut Vec<u8>) { self.0.save(out); self.1.save(out); }
        fn load(input: &mut &[u8]) -> Option<Self> { Some((A::load(input)?, B::load(input)?)) }
    }
    impl<A: Cached, B: Cached, C: Cached> Cached for (A, B, C) {
        fn save(&self, out: &mut Vec<u8>) { self.0.save(out); self.1.save(out); self.2.save(out); }
        fn load(input: &mut &[u8]) -> Option<Self> { Some((A::load(input)?, B::load(input)?, C::load(input)?)) }
    }
    impl<A: Cached, B: Cached, C: Cached, D: Cached> Cached for (A, B, C, D) {
        fn save(&self, out: &mut Vec<u8>) { self.0.save(out); self.1.save(out); self.2.save(out); self.3.save(out); }
        fn load(input: &mut &[u8]) -> Option<Self> { Some((A::load(input)?, B::load(input)?, C::load(input)?, D::load(input)?)) }
    }
    fn items<T: Cached, C: FromIterator<T>>(input: &mut &[u8]) -> Option<C> {
        let len = usize::load(input)?;
        if len > input.len() { return None }
        (0..len).map(|_| T::load(input)).collect()
    }
    impl<T: Cached> Cached for Vec<T> {
        fn save(&self, out: &mut Vec<u8>) { self.len().save(out); for v in self { v.save(out); } }
        fn load(input: &mut &[u8]) -> Option<Self> { items(input) }
    }
    impl<T: Cached + Ord> Cached for BTreeSet<T> {
        fn save(&self, out: &mut Vec<u8>) { self.len().save(out); for v in self { v.save(out); } }
        fn load(input: &mut &[u8]) -> Option<Self> { items(input) }
    }
    impl<K: Cached + Ord, V: Cached> Cached for BTreeMap<K, V> {
        fn save(&self, out: &mut Vec<u8>) {
            self.len().save(out);
            for (k, v) in self { k.save(out); v.save(out); }
        }
        fn load(input: &mut &[u8]) -> Option<Self> { items(input) }
    }
    macro_rules! fields {
        ($ty:ty { $($field:ident),* }) => {
            impl $crate::fold::cache::Cached for $ty {
                fn save(&self, out: &mut Vec<u8>) { $(self.$field.save(out);)* }
                fn load(input: &mut &[u8]) -> Option<Self> {
                    Some(Self { $($field: $crate::fold::cache::Cached::load(input)?),* })
                }
            }
        };
    }
    pub(crate) use fields;
}

use cache::Cached;
impl Cached for TownPolicy {
    fn save(&self, out: &mut Vec<u8>) { out.push(u8::from(*self == Self::Unreadable)); }
    fn load(input: &mut &[u8]) -> Option<Self> {
        match u8::load(input)? { 0 => Some(Self::Members), 1 => Some(Self::Unreadable), _ => None }
    }
}
cache::fields!(Profile { name, colour, writer });
cache::fields!(ChatMsg { id, author, actor, via, wall_ms, chat, author_agent, to_agent });
cache::fields!(Agent { owner, name, kind, custom_name });
cache::fields!(Timeline { events, checkpoints });
cache::fields!(Session { open, joins, leaves, declines, said, shared });
cache::fields!(TranscriptShare { author, agent, line });
cache::fields!(ServiceFacts { offers, sessions, histories });
cache::fields!(Opened { anchor, opener, mode, to, town_wide });
impl Cached for ForkPoint {
    fn save(&self, out: &mut Vec<u8>) {
        match self {
            Self::Start => out.push(0),
            Self::After(id) => { out.push(1); id.save(out); }
        }
    }
    fn load(input: &mut &[u8]) -> Option<Self> {
        match u8::load(input)? {
            0 => Some(Self::Start),
            1 => Some(Self::After(Id::load(input)?)),
            _ => None,
        }
    }
}

impl Cached for Entry {
    fn save(&self, out: &mut Vec<u8>) {
        match self {
            Self::Chat(id) => { out.push(1); id.save(out); }
            Self::Session(id) => { out.push(2); id.save(out); }
            Self::Clip(id) => { out.push(3); id.save(out); }
            Self::Member(id) => { out.push(4); id.save(out); }
        }
    }
    fn load(input: &mut &[u8]) -> Option<Self> {
        Some(match u8::load(input)? {
            1 => Self::Chat(Id::load(input)?), 2 => Self::Session(SessionId::load(input)?),
            3 => Self::Clip(Id::load(input)?), 4 => Self::Member(Id::load(input)?), _ => return None,
        })
    }
}

// Older checkpoints can contain generic field writes for issues received before this kind was known.
const CACHE_VERSION: &[u8] = b"shared fold cache v23\0";

fn prefix_hash(events: &[Event]) -> Id {
    let mut hash = makepad_network::digest::Sha256::new();
    for event in events {
        hash.update(&event.id);
        hash.update(&[u8::from(event.redacted())]);
    }
    hash.finalise()
}

impl State {
    /// Text edits, task conversions and their undos in canonical replay order.
    pub fn doc_event_ids(&self, doc: Id) -> impl ExactSizeIterator<Item = &Id> {
        let events = self.timelines.get(&doc).map_or(&[][..], |timeline| timeline.events.as_slice());
        events.iter().map(|order| &order.2)
    }

    /// The most recent document event, including changes to its entity fields.
    pub fn last_doc_event(&self, doc: &Entity) -> Option<&Id> {
        let text = self.timelines.get(&doc.id).and_then(|timeline| timeline.events.last());
        let entity = self.order.get(doc.last_activity as usize);
        text.max(entity).map(|order| &order.2)
    }

    /// Hash the canonical document history on demand. A late insert can leave the
    /// last event unchanged, and an undo can restore identical visible Markdown.
    pub fn doc_revision(&self, doc: Id) -> Option<String> {
        self.doc_revision_bytes(doc).map(|bytes| crate::hex(&bytes))
    }

    pub fn doc_revision_bytes(&self, doc: Id) -> Option<[u8; 16]> {
        let state = self.docs.get(&doc)?;
        let mut hash = makepad_network::digest::Sha256::new();
        hash.update(&doc);
        if let Some(timeline) = self.timelines.get(&doc) {
            for order in &timeline.events { hash.update(&order.2); }
        }
        // Entity edits (including undo of creation) live outside the text timeline.
        let mut edits = self.entity_writes.get(&doc).cloned().unwrap_or_default();
        let mut pending: Vec<_> = edits.iter().copied().collect();
        while let Some(order) = pending.pop() {
            if let Some(undos) = self.undo_children.get(&order.2) {
                for &undo in undos.range(order..) {
                    if edits.insert(undo) { pending.push(undo); }
                }
            }
        }
        for order in edits { hash.update(&order.2); }
        // A linked checkbox and its displayed task number are part of the read projection.
        for task in state.task_states.keys() {
            hash.update(task);
            if let Some(number) = self.task_numbers.get(task) { hash.update(&number.to_le_bytes()); }
            if let Some(writes) = self.statuses.get(task) {
                for order in writes.keys() { hash.update(&order.2); }
            }
        }
        Some(hash.finalise()[..16].try_into().unwrap())
    }

    /// Save the projections and undo evidence. Graph metadata comes from verified headers on open.
    pub fn checkpoint(&self, events: &[Event]) -> Vec<u8> {
        let mut out = CACHE_VERSION.to_vec();
        events.len().save(&mut out);
        prefix_hash(events).save(&mut out);
        macro_rules! save { ($($field:ident),*) => { $(self.$field.save(&mut out);)* }; }
        save!(docs, scopes, timelines, tick_writes, statuses, linked_docs, conversions, numbers,
            groups, cleared, profiles, chat, sessions, services, timeline, agents, entities, task_numbers, unknown, unknown_critical, partial, invalid_keys,
            town_policy, policy_writes, copied_from);
        self.own_children.save(&mut out);
        self.membership.forks.save(&mut out);
        let digest = makepad_network::digest::sha256_hash(&out);
        out.extend(digest);
        out
    }

    /// A stale, torn or incompatible checkpoint is a cache miss. A valid prefix resumes with its tail.
    pub fn restore(events: &[Event], bytes: &[u8]) -> Option<Self> {
        let (body, digest) = bytes.split_at_checked(bytes.len().checked_sub(32)?)?;
        if makepad_network::digest::sha256_hash(body) != digest { return None }
        let mut input = body.strip_prefix(CACHE_VERSION)?;
        let count = usize::load(&mut input)?;
        let (prefix, tail) = events.split_at_checked(count)?;
        if Id::load(&mut input)? != prefix_hash(prefix) { return None }
        let mut state = State::default();
        macro_rules! load { ($($field:ident),*) => { $(state.$field = Cached::load(&mut input)?;)* }; }
        load!(docs, scopes, timelines, tick_writes, statuses, linked_docs, conversions, numbers,
            groups, cleared, profiles, chat, sessions, services, timeline, agents, entities, task_numbers, unknown, unknown_critical, partial, invalid_keys,
            town_policy, policy_writes, copied_from);
        state.derived_task_creators = state.conversions.iter().map(|(&creator, &(_, _, task))| (task, creator)).collect();
        let own_children = BTreeMap::load(&mut input)?;
        let forks = BTreeMap::load(&mut input)?;
        if !input.is_empty() { return None }
        for (index, msg) in state.chat.iter().enumerate() {
            state.chat_positions.insert(msg.id, index);
            if let Some(agent) = msg.author_agent { state.last_replies.insert(agent, msg.id); }
            if let Some(agent) = msg.to_agent { state.mentions.entry(agent).or_default().push(msg.id); }
        }
        let (ordered, waiting) = canonical(prefix);
        let mut saved = HashMap::new();
        for (ids, was) in [(&state.unknown, Reading::Unknown), (&state.unknown_critical, Reading::UnknownCritical), (&state.partial, Reading::SkippedTag)] {
            for &id in ids {
                if saved.insert(id, was).is_some() { return None }
            }
        }
        if !state.index_prefix(ordered.into_iter(), Some(&mut saved)) { return None }
        if state.own_children != own_children || state.membership.forks != forks { return None }
        let missing: BTreeSet<_> = waiting.into_iter().collect();
        for event in prefix.iter().filter(|e| missing.contains(&e.id)).chain(tail) { state.receive(event); }
        Some(state)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::event::{Change, Draft, Event};
    use crate::key::DeviceKey;

    pub(crate) struct Maker {
        town: Id,
        keys: Vec<DeviceKey>,
        pub(crate) events: Vec<Event>,
        salt: u8,
    }

    impl Maker {
        fn new() -> Self {
            Self::town(2, 2)
        }

        pub(crate) fn fresh(policy: u16, devices: usize, descending: bool) -> Self {
            let mut keys: Vec<_> = (0..devices).map(|_| DeviceKey::generate().unwrap()).collect();
            keys.sort_by_key(DeviceKey::public);
            if descending { keys.reverse(); }
            let (town, genesis) = Event::found(&keys[0], policy, 0, 0, [0; 16]).unwrap();
            Self { town, keys, events: vec![genesis], salt: 0 }
        }

        fn town(policy: u16, members: usize) -> Self {
            let mut m = Self::fresh(policy, members.max(2), false);
            for who in 1..members {
                let entry = m.invite(0, who);
                m.accept(who, entry, None);
            }
            m
        }

        pub(crate) fn invite(&mut self, by: usize, who: usize) -> Id {
            self.member(by, Member::Invite { subject: self.keys[who].public(), role: 0 }, None)
        }

        fn vouch(&mut self, by: usize, seat: Id, who: usize, parents: Option<Vec<Id>>) -> Id {
            self.member(by, Member::AddDevice { seat, device: self.keys[who].public() }, parents)
        }

        pub(crate) fn accept(&mut self, who: usize, entry: Id, parents: Option<Vec<Id>>) -> Id {
            self.member(who, Member::Accept { entry }, parents)
        }

        pub(crate) fn remove(&mut self, by: usize, seat: Id, who: usize, last: Id, parents: Option<Vec<Id>>) -> Id {
            self.member(by, Member::Remove { seat, device: self.keys[who].public(), last }, parents)
        }

        fn member(&mut self, who: usize, op: Member, parents: Option<Vec<Id>>) -> Id {
            self.add(who, Actor::Human, Content::Member(op), parents)
        }

        fn entity(&mut self, kind: u16, project: Option<Id>, fields: Vec<(Key, Value)>) -> Id {
            self.add(0, Actor::Human, Content::Entity { copied_from: vec![], kind: Key::Number(kind), project, derive: None, fields }, None)
        }

        fn set(&mut self, changes: Vec<Change>) -> Id {
            self.add(0, Actor::Human, Content::Set { copied_from: vec![], changes, label: None }, None)
        }

        fn undo(&mut self, of: Id) -> Id {
            self.add(1, Actor::Human, Content::Undo { of }, None)
        }

        /// A new event by `who` whose parents are every event so far (or `parents` when given).
        pub(crate) fn add(&mut self, who: usize, actor: Actor, content: Content, parents: Option<Vec<Id>>) -> Id {
            let mut parents = parents.unwrap_or_else(|| {
                let mut all: Vec<Id> = self.events.iter().map(|e| e.id).collect();
                all.sort();
                all
            });
            if let Some(prev) = self.events.iter().rev().find(|e| e.header.author == self.keys[who].public()) {
                if !parents.contains(&prev.id) { parents.push(prev.id); }
            } else {
                assert!(matches!(content, Content::Member(Member::Accept { .. })), "a new author starts with an Accept");
            }
            self.write_with_parents(who, actor, content, parents)
        }

        /// Sign with exactly these parents, as another copy of the same key could.
        fn write_with_parents(&mut self, who: usize, actor: Actor, content: Content, parents: Vec<Id>) -> Id {
            let bytes = content.encode();
            let draft = Draft { kind: content.kind(), kind_version: content.version(), critical: false, actor, via: Via::Ui, wall_ms: 0, parents, content: &bytes };
            self.salt = self.salt.wrapping_add(1);
            let event = Event::create(&self.keys[who], self.town, draft, [self.salt; 16]).unwrap();
            let id = event.id;
            self.events.push(event);
            id
        }
    }

    // Shared by the document and membership arrival-order tests.
    struct Seed(u32);

    impl Seed {
        fn next(&mut self) -> u32 {
            self.0 = self.0.wrapping_mul(1664525).wrapping_add(1013904223);
            self.0
        }

        fn shuffle<T>(&mut self, items: &mut [T]) {
            for i in (1..items.len()).rev() { items.swap(i, (self.next() >> 8) as usize % (i + 1)); }
        }
    }

    fn check_deliveries(events: &[Event], expected: &State, seed: &mut Seed, rounds: usize) {
        let mut order: Vec<_> = events.iter().rev().collect();
        for round in 0..rounds {
            if round != 0 { seed.shuffle(&mut order); }
            let mut received = State::default();
            for event in &order { received.receive(event); }
            assert_eq!(&received, expected, "delivery {round}, seed {}", seed.0);
        }
    }

    fn checked_fold(events: &[Event]) -> State {
        let state = fold(events);
        check_deliveries(events, &state, &mut Seed(0x4e73), 4);
        state
    }

    fn every_permutation(events: &[Event], expected: &State) -> usize {
        fn visit(events: &mut [&Event], at: usize, state: &State, expected: &State) -> usize {
            if at == events.len() {
                assert_eq!(state, expected, "arrival order {:?}", events.iter().map(|e| e.id).collect::<Vec<_>>());
                return 1
            }
            let mut orders = 0;
            for i in at..events.len() {
                events.swap(at, i);
                let mut next = state.clone();
                next.receive(events[at]);
                orders += visit(events, at + 1, &next, expected);
                events.swap(at, i);
            }
            orders
        }
        visit(&mut events.iter().collect::<Vec<_>>(), 0, &State::default(), expected)
    }

    fn chat(text: &str, to_ai: bool, quote: Option<Id>) -> Content {
        Content::Chat(Chat { text: text.into(), to_ai, quote, ..Chat::default() })
    }

    fn change(entity: Id, field: Key, value: Value) -> Change {
        Change { entity, field, value }
    }

    fn task(project: Id, title: &str) -> Content {
        Content::Entity { copied_from: vec![], kind: Key::Number(kind::TASK), derive: None, project: Some(project), fields: vec![(field::TITLE, Value::Text(title.into()))] }
    }

    fn merge_task(from: Id, into: Id) -> Content {
        Content::Set { copied_from: vec![], label: None, changes: vec![
            change(from, field::DELETED, Value::Bool(true)),
            change(from, field::MERGED_INTO, Value::Id(into)),
        ] }
    }

    #[test]
    fn t2_core_facts_provenance_policy_and_checkpoint_roundtrip() {
        use crate::doc::CharId;
        use crate::event::{self, CopyRef, FieldWriter, TAG_COPIED_FROM};

        fn entity(kind: u16, project: Option<Id>, fields: Vec<(Key, Value)>) -> Content {
            Content::Entity { kind: Key::Number(kind), project, fields, derive: None, copied_from: vec![] }
        }
        fn set(changes: Vec<Change>) -> Content {
            Content::Set { changes, label: None, copied_from: vec![] }
        }
        fn copies(content: &mut Content) -> &mut Vec<CopyRef> {
            match content {
                Content::Entity { copied_from, .. } | Content::Set { copied_from, .. } => copied_from,
                _ => unreachable!(),
            }
        }
        fn signed(m: &mut Maker, who: usize, critical: bool, content: &Content, mut parents: Vec<Id>, extra: &[u8]) -> Id {
            if let Some(prev) = m.events.iter().rev().find(|e| e.header.author == m.keys[who].public()) {
                parents.push(prev.id);
            }
            parents.sort_unstable();
            parents.dedup();
            let mut bytes = content.encode();
            bytes.extend_from_slice(extra);
            let draft = Draft { kind: content.kind(), kind_version: content.version(), critical, actor: Actor::Human,
                via: Via::Ui, wall_ms: 0, parents, content: &bytes };
            m.salt = m.salt.wrapping_add(1);
            let event = Event::create(&m.keys[who], m.town, draft, [m.salt; 16]).unwrap();
            let id = event.id;
            m.events.push(event);
            id
        }
        fn append(m: &mut Maker, who: usize, critical: bool, content: Content) -> Id {
            let parent = m.events.last().unwrap().id;
            signed(m, who, critical, &content, vec![parent], &[])
        }

        fn policy_town() -> (Maker, Id) {
            let mut m = Maker::fresh(GROUP_POLICY, 3, false);
            let ben = m.invite(0, 1);
            m.accept(1, ben, None);
            let device = m.vouch(0, m.events[0].id, 2, None);
            m.accept(2, device, None);
            let town = m.entity(kind::TOWN, None, vec![(field::ARCHIVED, Value::Bool(false))]);
            (m, town)
        }
        fn policy(town: Id, n: i64) -> Content { set(vec![change(town, field::POLICY, Value::Int(n))]) }
        fn archive(town: Id, flag: bool) -> Content { set(vec![change(town, field::ARCHIVED, Value::Bool(flag))]) }
        fn archive_copy(town: Id, writer: Id, archived: bool) -> Content {
            Content::Entity { kind: Key::Number(kind::TASK), project: None, derive: None,
                fields: vec![(field::ARCHIVED, Value::Bool(archived))],
                copied_from: vec![CopyRef { target_entity: None, target_field: field::ARCHIVED,
                    sources: vec![CopySource::EntityField { entity: town, field: field::ARCHIVED, writer }] }] }
        }
        fn projections(events: &[Event]) -> [State; 2] {
            let live = checked_fold(events);
            let restored = State::restore(events, &live.checkpoint(events)).unwrap();
            assert_eq!(restored, live, "restore reproduces the complete live projection");
            [live, restored]
        }
        fn field_copy(source: Id, field: Key, writer: Id, value: Value) -> Content {
            let mut copy = entity(kind::TASK, None, vec![(field.clone(), value)]);
            copies(&mut copy).push(CopyRef { target_entity: None, target_field: field.clone(),
                sources: vec![CopySource::EntityField { entity: source, field, writer }] });
            copy
        }
        fn concurrent_title_before_a_direct_overwrite_withholds_the_copy() {
            let mut m = Maker::new();
            let task = m.entity(kind::TASK, None, vec![]);
            let rename = |s: &str| set(vec![change(task, field::TITLE, Value::Text(s.into()))]);
            append(&mut m, 1, false, rename("Ben"));
            let parallel = m.events.pop().unwrap();
            append(&mut m, 0, false, chat("Lu", false, None));
            let writer = append(&mut m, 0, false, rename("Ana"));
            let copy = append(&mut m, 0, false, field_copy(task, field::TITLE, writer, Value::Text("Ana".into())));
            for state in projections(&m.events) {
                assert_eq!(state.field_writer_before(&copy, &task, &field::TITLE), Ok(Some(writer)));
            }
            m.events.push(parallel);
            for state in projections(&m.events) {
                assert_eq!(state.entities[&task].fields[&field::TITLE].writer, writer);
                assert!(state.field_writer_before(&copy, &task, &field::TITLE).is_err(),
                    "a concurrent TITLE below a direct overwrite leaves its prefix uncertified");
            }
        }
        fn checkbox_burst_depends_on_task_creation_at_another_block() {
            let mut m = Maker::new();
            let doc = m.entity(kind::DOC, None, vec![]);
            let root = BreakRef::Root;
            let text = |ops| Content::Text { doc, ops };
            let line = append(&mut m, 0, false, text(vec![TextOp::Insert { anchor: root, text: "\n".into() }]));
            let other = BreakRef::Char(CharId { event: line, index: 0 });
            let task = append(&mut m, 1, false, entity(kind::TASK, None, vec![]));
            let parallel = m.events.pop().unwrap();
            append(&mut m, 0, false, chat("Lu", false, None));
            let base = m.events.len();
            let burst = append(&mut m, 0, false, text(vec![
                TextOp::Attr { at: root, attr: attr::CHECKBOX, value: Value::Text("done".into()) },
                TextOp::Attr { at: other, attr: attr::TASK, value: Value::Id(task) },
            ]));
            let copy = append(&mut m, 0, false, chat("Kit", false, None));
            for state in projections(&m.events) {
                assert_eq!(state.doc_attr_state_before(&copy, &doc, root, attr::CHECKBOX), Ok(None));
            }
            m.events.push(parallel.clone());
            for state in projections(&m.events) {
                assert_eq!(state.docs[&doc].attribute_writer(root, attr::CHECKBOX), Some(burst));
                assert!(state.doc_attr_state_before(&copy, &doc, root, attr::CHECKBOX).is_err(),
                    "a link to a concurrently created task changes the validity of the whole checkbox burst");
            }

            // Same-event link burst: fails closed when `task` is concurrent, certifies when in-past.
            for in_past_task in [false, true] {
                m.events.truncate(base);
                if in_past_task { m.events.push(parallel.clone()); }
                let known = m.entity(kind::TASK, None, vec![]);
                let burst = append(&mut m, 0, false, text(vec![
                    TextOp::Attr { at: root, attr: attr::TASK, value: Value::Id(known) },
                    TextOp::Attr { at: root, attr: attr::CHECKBOX, value: Value::Text("done".into()) },
                    TextOp::Attr { at: root, attr: attr::TASK, value: Value::Null },
                    TextOp::Attr { at: other, attr: attr::TASK, value: Value::Id(task) },
                ]));
                let copy = append(&mut m, 0, false, chat("Kit", false, None));
                if !in_past_task {
                    for state in projections(&m.events) {
                        assert_eq!(state.docs[&doc].attribute_writer(root, attr::TASK), None);
                        assert_eq!(state.tick_link_source(&burst, &known), None);
                    }
                    m.events.push(parallel.clone());
                }
                let certified = projections(&m.events).map(|state| {
                    let (source_doc, at, writer) = state.tick_link_source(&burst, &known).unwrap();
                    assert_eq!((source_doc, at, writer), (doc, root, burst));
                    assert_eq!(state.doc_attr_state_before(&copy, &doc, root, attr::CHECKBOX).is_ok(), in_past_task);
                    state.verify_doc_task_link_at_in_past(&copy, &burst, &source_doc, at, &known, &writer)
                });
                assert_eq!(certified, [in_past_task; 2],
                    "a same-event link requires its whole burst to be valid in the copy's past");
            }

            // Unlink burst with a prior link: both link and status helpers enforce the unlink burst's own validity.
            for in_past_task in [false, true] {
                m.events.truncate(base);
                if in_past_task { m.events.push(parallel.clone()); }
                let known = m.entity(kind::TASK, None, vec![(field::STATUS, Value::Text("done".into()))]);
                let link = append(&mut m, 0, false, text(vec![
                    TextOp::Attr { at: root, attr: attr::TASK, value: Value::Id(known) },
                ]));
                let unlink = append(&mut m, 0, false, text(vec![
                    TextOp::Attr { at: root, attr: attr::TASK, value: Value::Null },
                    TextOp::Attr { at: other, attr: attr::TASK, value: Value::Id(task) },
                ]));
                let copy = append(&mut m, 0, false, chat("Kit", false, None));
                if !in_past_task { m.events.push(parallel.clone()); }
                let certified = projections(&m.events).map(|state| (
                    state.verify_doc_task_link_at_in_past(&copy, &unlink, &doc, root, &known, &link),
                    state.verify_task_status_at_in_past(&copy, &unlink, &known, &known),
                ));
                assert_eq!(certified, [(in_past_task, in_past_task); 2],
                    "an unlink burst's own task-link validity gates both link and status certification");
            }
        }
        fn cutoff_after_copy_withholds_a_voided_title() {
            let mut m = Maker::fresh(GROUP_POLICY, 2, false);
            let seat = m.invite(0, 1);
            let accept = m.accept(1, seat, None);
            let title = |s: &str| Value::Text(s.into());
            let task = m.entity(kind::TASK, None, vec![(field::TITLE, title("v1"))]);
            let edited = append(&mut m, 1, false, set(vec![change(task, field::TITLE, title("v2"))]));
            let copy = append(&mut m, 0, false, field_copy(task, field::TITLE, task, title("v1")));
            let base = m.events.len();
            for last in [edited, accept] {
                m.events.truncate(base);
                m.remove(0, seat, 1, last, None);
                for state in projections(&m.events) {
                    assert_eq!(state.void_events.contains_key(&edited), last == accept);
                    let writer = state.field_writer_before(&copy, &task, &field::TITLE);
                    let verifies_creation = state.verify_field_writer_at(&copy, &task, &field::TITLE, &task);
                    if last == edited {
                        assert_eq!(writer, Ok(Some(edited)));
                        assert!(!verifies_creation);
                    } else {
                        assert_eq!((writer.is_err(), verifies_creation), (true, false),
                            "voiding an edit after a copy cannot certify the older TITLE");
                    }
                }
            }
        }
        fn concurrent_lock_changes_withhold_geometry() {
            let mut withheld = Vec::new();
            for initially_locked in [true, false] {
                let mut m = Maker::new();
                let card = m.entity(kind::SHAPE, None, vec![(field::X, Value::Int(0)),
                    (field::LOCKED, Value::Bool(initially_locked))]);
                let lock = |value| set(vec![change(card, field::LOCKED, Value::Bool(value))]);
                append(&mut m, 1, false, lock(!initially_locked));
                let parallel = m.events.pop().unwrap();
                append(&mut m, 0, false, chat("Lu", false, None));
                if !initially_locked {
                    let locked = append(&mut m, 0, false, lock(true));
                    append(&mut m, 0, false, Content::Undo { of: locked });
                }
                let moved = append(&mut m, 0, false, set(vec![change(card, field::X, Value::Int(100))]));
                let (writer, value) = if initially_locked { (card, 0) } else { (moved, 100) };
                let copy = append(&mut m, 0, false, field_copy(card, field::X, writer, Value::Int(value)));
                for state in projections(&m.events) {
                    assert_eq!(state.field_writer_before(&copy, &card, &field::X), Ok(Some(writer)));
                }
                m.events.push(parallel);
                for state in projections(&m.events) {
                    assert_eq!(state.entities[&card].fields[&field::X].writer, if initially_locked { moved } else { card });
                    withheld.push(state.field_writer_before(&copy, &card, &field::X).is_err());
                }
            }
            assert_eq!(withheld, [true; 4], "both the concurrent unlock and the lock behind an in-past undo withhold X");
        }
        fn pruned_concurrent_policy_suppressing_town_write_fails_closed() {
            for creation_undo in [false, true] {
                let (mut m, town) = policy_town();
                let prior = append(&mut m, 0, false, archive(town, false));
                let concurrent = signed(&mut m, 2, true, &policy(town, 1), vec![prior], &[]);
                let pad = signed(&mut m, 0, false, &chat("Lu", false, None), vec![prior], &[]);
                let request = if creation_undo { Content::Undo { of: town } } else { archive(town, true) };
                let suppressed = signed(&mut m, 1, false, &request, vec![pad], &[]);
                let copy = append(&mut m, 0, false, archive_copy(town, prior, false));
                let base = m.events.len();
                for later in [None, Some(policy(town, 1)), Some(Content::Undo { of: concurrent })] {
                    m.events.truncate(base);
                    if let Some(op) = &later { append(&mut m, 2, true, op.clone()); }
                    for state in projections(&m.events) {
                        assert!(!state.record_before(&copy, &concurrent));
                        assert!(state.record_before(&copy, &suppressed));
                        assert!(!state.groups.contains_key(&suppressed));
                        assert_eq!(state.entities[&town].fields[&field::ARCHIVED].writer, prior);
                        if later.is_some() { assert!(!state.policy_writes.contains_key(&(concurrent, town))); }
                        if matches!(later, Some(Content::Undo { .. })) { assert!(state.policy_writes.is_empty()); }
                        let field = if creation_undo { field::DELETED } else { field::ARCHIVED };
                        assert!(state.field_writer_before(&copy, &town, &field).is_err(),
                            "a suppressed town write stays ambiguous after its concurrent policy is pruned");
                        assert!(!state.verify_field_writer_at(&copy, &town, &field, &prior));
                        assert!(!state.verify_field_writer_at(&copy, &town, &field, &suppressed));
                    }
                }
            }
        }
        fn policy_clear_then_set_enforces_exact_window() {
            for parallel in [false, true] {
                let (mut m, town) = policy_town();
                let first = append(&mut m, 0, false, archive(town, true));
                let writer = append(&mut m, 0, false, Content::Undo { of: first });
                let p1 = append(&mut m, 0, true, policy(town, 1));
                let clear = append(&mut m, 0, true, Content::Undo { of: p1 });
                if parallel { signed(&mut m, 2, false, &archive(town, false), vec![clear], &[]); }
                let pad = signed(&mut m, 0, false, &chat("Lu", false, None), vec![clear], &[]);
                let ben = signed(&mut m, 1, false, &Content::Undo { of: writer }, vec![pad], &[]);
                if parallel {
                    let copy = append(&mut m, 0, false, archive_copy(town, writer, false));
                    for reset in [false, true] {
                        if reset { append(&mut m, 0, true, policy(town, 1)); }
                        for state in projections(&m.events) {
                            assert!(!state.groups.contains_key(&ben));
                            assert!(state.field_writer_before(&copy, &town, &field::ARCHIVED).is_err(),
                                "the competing write preempted an authorized undo inside the clear window");
                            assert!(!state.verify_field_writer_at(&copy, &town, &field::ARCHIVED, &writer));
                        }
                    }
                } else {
                    let p2 = append(&mut m, 0, true, policy(town, 1));
                    let denied = append(&mut m, 1, false, Content::Undo { of: ben });
                    let copy = append(&mut m, 0, false, archive_copy(town, ben, true));
                    for later in [None, Some(Content::Undo { of: p2 }), Some(policy(town, 1))] {
                        if let Some(op) = later { append(&mut m, 0, true, op); }
                        for state in projections(&m.events) {
                            assert!(state.groups.contains_key(&ben) && !state.groups.contains_key(&denied));
                            assert!(state.field_writer_before(&copy, &town, &field::ARCHIVED) == Ok(Some(ben)),
                                "the copy keeps the undo authorized between the clear and re-set");
                            assert!(state.verify_field_writer_at(&copy, &town, &field::ARCHIVED, &ben));
                        }
                    }
                }
            }
        }
        fn policy_undo_redo_and_concurrent_policy_undo_races() {
            let (mut m, town1) = policy_town();
            let w1 = append(&mut m, 0, false, archive(town1, false));
            append(&mut m, 0, true, policy(town1, 1));
            let second = append(&mut m, 0, true, policy(town1, 2));
            append(&mut m, 0, true, Content::Undo { of: second });
            let denied1 = append(&mut m, 1, false, Content::Undo { of: w1 });
            let copy1 = append(&mut m, 0, false, archive_copy(town1, w1, false));

            let town2 = m.entity(kind::TOWN, None, vec![]);
            let w2 = append(&mut m, 0, false, archive(town2, false));
            let initial = append(&mut m, 0, true, policy(town2, 1));
            let clear = append(&mut m, 0, true, Content::Undo { of: initial });
            let redo = append(&mut m, 0, true, Content::Undo { of: clear });
            let denied2 = append(&mut m, 1, false, Content::Undo { of: w2 });
            let copy2 = append(&mut m, 0, false, archive_copy(town2, w2, false));

            let town3 = append(&mut m, 0, true, entity(kind::TOWN, None,
                vec![(field::POLICY, Value::Int(1)), (field::ARCHIVED, Value::Bool(false))]));
            let write = append(&mut m, 0, true, archive(town3, true));
            let w3 = append(&mut m, 0, true, Content::Undo { of: write });
            let hide = append(&mut m, 0, true, Content::Undo { of: town3 });
            append(&mut m, 0, true, Content::Undo { of: hide });
            let denied3 = append(&mut m, 1, false, Content::Undo { of: w3 });
            let copy3 = append(&mut m, 0, false, archive_copy(town3, w3, false));
            for later in [false, true] {
                if later {
                    append(&mut m, 0, true, Content::Undo { of: redo });
                    append(&mut m, 0, true, policy(town3, 1));
                }
                for state in projections(&m.events) {
                    for (town, writer, denied, copy) in [(town1, w1, denied1, copy1), (town2, w2, denied2, copy2),
                        (town3, w3, denied3, copy3)] {
                        assert!(!state.groups.contains_key(&denied));
                        assert!(state.field_writer_before(&copy, &town, &field::ARCHIVED) == Ok(Some(writer)),
                            "an active restored or creation policy still denies the historical undo");
                        assert!(state.verify_field_writer_at(&copy, &town, &field::ARCHIVED, &writer));
                    }
                }
            }
            for concurrent_clear in [false, true] {
                let (mut m, town) = policy_town();
                let writer = append(&mut m, 0, false, archive(town, false));
                let p1 = append(&mut m, 0, true, policy(town, 1));
                let op = if concurrent_clear { Content::Undo { of: p1 } } else { policy(town, 1) };
                let concurrent = signed(&mut m, 2, true, &op, vec![p1], &[]);
                let pad = signed(&mut m, 0, false, &chat("Lu", false, None), vec![p1], &[]);
                let parent = if concurrent_clear { pad } else { append(&mut m, 0, true, Content::Undo { of: p1 }) };
                let ben = signed(&mut m, 1, false, &archive(town, true), vec![parent], &[]);
                let copy = append(&mut m, 0, false, archive_copy(town, if concurrent_clear { writer } else { ben }, !concurrent_clear));
                for state in projections(&m.events) {
                    assert!(!state.record_before(&copy, &concurrent));
                    assert_eq!(state.groups.contains_key(&ben), concurrent_clear);
                    if !concurrent_clear { assert!(!state.groups.contains_key(&parent)); }
                    assert_eq!(state.entities[&town].fields[&field::ARCHIVED].writer, if concurrent_clear { ben } else { writer });
                    assert!(state.field_writer_before(&copy, &town, &field::ARCHIVED).is_err(),
                        "a concurrent change to the policy undo window invalidates the copy");
                    assert!(!state.verify_field_writer_at(&copy, &town, &field::ARCHIVED, &writer));
                    assert!(!state.verify_field_writer_at(&copy, &town, &field::ARCHIVED, &ben));
                }
            }
        }
        fn unlink_checkbox_source_survives_undo_late_tick_and_restore() {
            use crate::doc::{CheckboxSource, CopiedStatus};
            // Compare a tick's actual destination on the writer's branch with its
            // destination after another device's parallel link edit arrives.
            let mut certified_routed_tick = Vec::new();
            for linked_on_branch in [true, false] {
                let mut m = Maker::town(GROUP_POLICY, 3);
                let task = m.entity(kind::TASK, None, vec![(field::STATUS, Value::Text("done".into()))]);
                let routing_doc = m.entity(kind::DOC, None, vec![]);
                let notes = m.entity(kind::DOC, None, vec![]);
                let attr = |doc, attr, value| Content::Text { doc, ops: vec![TextOp::Attr { at: BreakRef::Root, attr, value }] };
                let mut status_writer = task;
                for who in [1, 0] {
                    let overwrite = append(&mut m, 0, false, set(vec![change(task, field::STATUS, Value::Text("review".into()))]));
                    status_writer = append(&mut m, who, false, Content::Undo { of: overwrite });
                }
                append(&mut m, 0, false, attr(notes, attr::TASK, Value::Id(task)));
                if !linked_on_branch {
                    let base = m.events.len();
                    let root = BreakRef::Root;
                    let tick = append(&mut m, 0, false, Content::Text { doc: routing_doc, ops: vec![
                        TextOp::Attr { at: root, attr: attr::TASK, value: Value::Id(task) },
                        TextOp::Attr { at: root, attr: attr::CHECKBOX, value: Value::Text("open".into()) },
                    ] });
                    let unlink = append(&mut m, 0, false, attr(notes, attr::TASK, Value::Null));
                    let before = append(&mut m, 0, false, chat("Lu", false, None));
                    for state in projections(&m.events) {
                        assert!(state.verify_task_status_at_in_past(&before, &unlink, &task, &tick),
                            "a tick routed by its own in-past link is certified");
                    }
                    m.events.truncate(base);
                }
                if linked_on_branch { append(&mut m, 0, false, attr(routing_doc, attr::TASK, Value::Id(task))); }
                let link = if linked_on_branch { Value::Null } else { Value::Id(task) };
                append(&mut m, 2, false, attr(routing_doc, attr::TASK, link));
                let concurrent = m.events.pop().unwrap();
                append(&mut m, 0, false, chat("Kit", false, None));
                let tick = append(&mut m, 0, false, attr(routing_doc, attr::CHECKBOX, Value::Text("open".into())));
                let unlink = append(&mut m, 0, false, attr(notes, attr::TASK, Value::Null));
                let before = append(&mut m, 0, false, chat("Lu", false, None));
                let writer_on_branch = if linked_on_branch { tick } else { status_writer };
                for state in projections(&m.events) {
                    assert_eq!(state.entities[&task].fields[&field::STATUS].writer, writer_on_branch);
                    assert_eq!(state.field_writer_before(&before, &task, &field::STATUS), Ok(Some(writer_on_branch)));
                    assert!(state.verify_task_status_at_in_past(&before, &unlink, &task, &writer_on_branch));
                }
                m.events.push(concurrent.clone());
                for state in projections(&m.events) {
                    assert!(!state.record_before(&before, &concurrent.id));
                    assert!(state.record_before(&before, &tick));
                    let merged_writer = if linked_on_branch { status_writer } else { tick };
                    assert_eq!(state.entities[&task].fields[&field::STATUS].writer, merged_writer);
                    certified_routed_tick.push(state.verify_task_status_at_in_past(&before, &unlink, &task, &merged_writer));
                    if !linked_on_branch {
                        assert_eq!((state.verify_task_status_at_in_past(&before, &unlink, &task, &tick),
                            state.verify_task_status_at_in_past(&before, &unlink, &task, &status_writer)), (false, false),
                            "a concurrent routing link withholds both the tick and the prior status writer");
                    }
                }
            }
            assert_eq!(certified_routed_tick, [false; 4],
                "a concurrent link change cannot certify a different task status writer at unlink");
            let mut m = Maker::new();
            let doc = m.entity(kind::DOC, None, vec![]);
            let root = BreakRef::Root;
            let alias = BreakRef::Char(DocState::new(doc).root_id());
            let text = |s: &str| Value::Text(s.into());
            let task = m.entity(kind::TASK, None, vec![(field::STATUS, text("done"))]);
            let other = m.entity(kind::TASK, None, vec![(field::STATUS, text("open"))]);
            let link = |task| TextOp::Attr { at: root, attr: attr::TASK, value: Value::Id(task) };
            let tick = |s: &str| TextOp::Attr { at: root, attr: attr::CHECKBOX, value: text(s) };
            let unlink = || TextOp::Attr { at: root, attr: attr::TASK, value: Value::Null };
            let burst = |ops| Content::Text { doc, ops };
            append(&mut m, 0, false, burst(vec![link(task)]));
            let mut status_writer = task;
            let mut link_writer = task;
            for who in [1, 0] {
                let status = append(&mut m, 0, false, set(vec![change(task, field::STATUS, text("open"))]));
                status_writer = append(&mut m, who, false, Content::Undo { of: status });
                let select = append(&mut m, 0, false, burst(vec![link(other)]));
                link_writer = append(&mut m, who, false, Content::Undo { of: select });
            }
            let base = m.events.len();
            for same_event in [false, true] {
                m.events.truncate(base);
                let ops = if same_event { vec![tick("done"), unlink()] } else { vec![unlink()] };
                let unlinked = append(&mut m, 0, false, burst(ops));
                append(&mut m, 0, false, burst(vec![TextOp::Attr { at: root, attr: attr::TASK, value: text("Lu") }]));
                let mut checkbox_writer = unlinked;
                for who in [1, 0] {
                    let overwrite = append(&mut m, 0, false, burst(vec![tick("open")]));
                    checkbox_writer = append(&mut m, who, false, Content::Undo { of: overwrite });
                }
                for state in projections(&m.events) {
                    let block = &state.docs[&doc];
                    let copied = if same_event { CopiedStatus::SameEventTick { at: root, link_writer } }
                        else { CopiedStatus::TaskStatus(status_writer) };
                    let source = CheckboxSource { task, link_writer, status: Some(copied) };
                    for at in [root, alias] {
                        assert_eq!(block.attribute(at, attr::CHECKBOX), Some(&text("done")),
                            "both root keys read the checkbox restored from the unlink");
                        assert_eq!(block.attribute_writer(at, attr::CHECKBOX), Some(checkbox_writer));
                        assert_eq!(block.block_checkbox_source(at), Some(source));
                        assert_eq!(state.doc_attr_state_before(&unlinked, &doc, at, attr::TASK),
                            Ok(Some((link_writer, Value::Id(task), None))));
                    }
                    assert_eq!(state.tick_link_source(&unlinked, &task), same_event.then_some((doc, root, link_writer)));
                    assert_eq!(state.entities[&task].fields[&field::STATUS].writer, if same_event { unlinked } else { status_writer });
                    assert!(state.record_before(&unlinked, &link_writer));
                }
                append(&mut m, 0, false, burst(vec![link(task)]));
                for state in projections(&m.events) {
                    assert!(state.docs[&doc].block_checkbox_source(root).is_none(), "a live link reads the task again");
                }
            }
        }
        fn field_and_doc_attr_undo_chains_survive_late_arrival_and_restore() {
            let mut m = Maker::new();
            let task = m.entity(kind::TASK, None, vec![]);
            let doc = m.entity(kind::DOC, None, vec![]);
            let alias = BreakRef::Char(DocState::new(doc).root_id());
            let write_attr = |value| Content::Text { doc, ops: vec![TextOp::Attr { at: alias, attr: attr::KIND, value }] };
            for value in [Value::Text("Lu".into()), Value::Null] {
                let mut f = append(&mut m, 0, false, set(vec![change(task, field::TEXT, value.clone())]));
                let mut a = append(&mut m, 0, false, write_attr(value.clone()));
                let mut hops = Vec::new();
                for who in [1, 0] {
                    let prior = (f, a);
                    let overwrite = append(&mut m, 0, false, set(vec![change(task, field::TEXT, Value::Text("Kit".into()))]));
                    f = append(&mut m, who, false, Content::Undo { of: overwrite });
                    let overwrite = append(&mut m, 0, false, write_attr(Value::Text("heading1".into())));
                    a = append(&mut m, who, false, Content::Undo { of: overwrite });
                    hops.push((f, a, prior));
                }
                let before = append(&mut m, 0, false, chat("Ana", false, None));
                for state in projections(&m.events) {
                    assert_eq!(state.field_writer_before(&before, &task, &field::TEXT), Ok(Some(f)));
                    for &(undo, attr_undo, (prev, prev_attr)) in &hops {
                        assert_eq!(state.field_undo_prev(&undo, &task, &field::TEXT), Some(prev));
                        assert_eq!(state.doc_attr_undo_prev(&doc, &attr_undo, alias, attr::KIND), Some(prev_attr));
                    }
                    if value == Value::Null { assert_eq!(state.cleared_field_writer(&task, &field::TEXT), Some(f)); }
                    for at in [BreakRef::Root, alias] {
                        assert_eq!(state.doc_attr_state_before(&before, &doc, at, attr::KIND), Ok(Some((a, value.clone(), None))));
                        assert_eq!(state.docs[&doc].attribute_writer(at, attr::KIND), Some(a),
                            "both root keys preserve the last document undo writer, including explicit clears");
                    }
                }
            }
        }
        unlink_checkbox_source_survives_undo_late_tick_and_restore();
        concurrent_title_before_a_direct_overwrite_withholds_the_copy();
        checkbox_burst_depends_on_task_creation_at_another_block();
        cutoff_after_copy_withholds_a_voided_title();
        concurrent_lock_changes_withhold_geometry();
        field_and_doc_attr_undo_chains_survive_late_arrival_and_restore();
        policy_undo_redo_and_concurrent_policy_undo_races();
        policy_clear_then_set_enforces_exact_window();
        pruned_concurrent_policy_suppressing_town_write_fails_closed();

        // An undo can be in the copy's past without being allowed to change its source field.
        // Exercise creation, issue ownership and the policy in force at the time of the undo.
        fn policy_refresh_after_copy_keeps_historical_undo_denied() {
            let mut history = Maker::new();
            let task = history.entity(kind::TASK, None, vec![(field::TITLE, Value::Text("Lu".into()))]);
            let hide = append(&mut history, 0, false, Content::Undo { of: task });
            append(&mut history, 1, false, Content::Undo { of: hide });
            let issue = history.entity(kind::ISSUE, None, vec![]);
            let text = history.set(vec![change(issue, field::TEXT, Value::Text("Kit".into()))]);
            append(&mut history, 1, false, Content::Undo { of: text });

            let town = history.entity(kind::TOWN, None, vec![(field::ARCHIVED, Value::Bool(false))]);
            let archive = || set(vec![change(town, field::ARCHIVED, Value::Bool(true))]);
            let first = append(&mut history, 0, false, archive());
            append(&mut history, 1, false, Content::Undo { of: first });
            let second = append(&mut history, 0, false, archive());
            let writer = append(&mut history, 0, false, Content::Undo { of: second });
            let members_policy = || set(vec![change(town, field::POLICY, Value::Int(1))]);
            append(&mut history, 0, true, members_policy());
            let denied = append(&mut history, 1, false, Content::Undo { of: writer });
            let copy = append(&mut history, 0, false, Content::Entity {
                kind: Key::Number(kind::TASK), project: None, derive: None,
                fields: vec![(field::ARCHIVED, Value::Bool(false))],
                copied_from: vec![CopyRef { target_entity: None, target_field: field::ARCHIVED,
                    sources: vec![CopySource::EntityField { entity: town, field: field::ARCHIVED, writer }] }],
            });
            let mut verified = Vec::new();
            for refresh in [false, true] {
                if refresh { append(&mut history, 0, true, members_policy()); }
                for state in projections(&history.events) {
                    assert!(state.verify_field_writer_at(&copy, &task, &field::TITLE, &task),
                        "resurrection preserves the creation TITLE");
                    assert!(state.verify_field_writer_at(&copy, &issue, &field::TEXT, &text),
                        "Ben could not undo Ana's ISSUE.TEXT");
                    assert!(state.record_before(&copy, &denied) && !state.groups.contains_key(&denied));
                    assert_eq!(state.entities[&town].fields[&field::ARCHIVED].writer, writer);
                    verified.push(state.verify_field_writer_at(&copy, &town, &field::ARCHIVED, &writer));
                    assert!(state.field_writer_before(&copy, &town, &field::ARCHIVED) == Ok(Some(writer)),
                        "the archived writer is stable after a policy refresh");
                }
            }
            assert_eq!(verified, [true; 4], "refreshing POLICY after a copy preserves its historical undo permission");
        }
        policy_refresh_after_copy_keeps_historical_undo_denied();

        let mut m = Maker::fresh(GROUP_POLICY, 3, false);
        let genesis = m.events[0].id;
        let founder = m.keys[0].public();
        let ben = m.keys[1].public();
        let seat = m.invite(0, 1);
        m.accept(1, seat, None);
        let device = m.vouch(0, genesis, 2, None);
        m.accept(2, device, None);

        // The signed order is independent of canonicalization for the index.
        let record = CopySource::Record(genesis);
        let doc_line = |events| CopySource::DocLine { doc: genesis, events };
        let field_source = |field| CopySource::EntityField { entity: seat, field, writer: genesis };
        let sources = vec![CopySource::SharePeer { peer: ben }, doc_line(vec![]), record.clone(),
            field_source(Key::Name("colour".into())), field_source(field::TITLE), doc_line(vec![seat, genesis, seat])];
        let reference = |target_entity, target_field, sources| CopyRef { target_entity, target_field, sources };
        let at = CharId { event: genesis, index: 0 };
        for (base, version, target) in [
            (entity(kind::TASK, None, vec![]), 1, None),
            (Content::Entity { kind: Key::Number(kind::TASK), project: None, derive: Some((genesis, at)),
                fields: vec![], copied_from: vec![] }, 2, None),
            (Content::Entity { kind: Key::Name("Card".into()), project: None, derive: None,
                fields: vec![], copied_from: vec![] }, 3, None),
            (set(vec![change(seat, field::TEXT, Value::Null)]), 1, Some(seat)),
            (set(vec![change(seat, Key::Name("colour".into()), Value::Literal("red".into()))]), 2, Some(seat)),
        ] {
            let mut content = base.clone();
            copies(&mut content).push(reference(target, field::TITLE, sources.clone()));
            copies(&mut content).push(reference(target, field::TEXT, vec![record.clone()]));
            assert_eq!(content.version(), version);
            let bytes = content.encode();
            let decoded = Content::decode(content.kind(), version, &bytes).unwrap();
            assert_eq!(decoded, content);
            assert_eq!(decoded.encode(), bytes);
            let stripped = event::fields(&bytes).unwrap().into_iter().filter(|f| f.0 != TAG_COPIED_FROM)
                .fold(FieldWriter::default(), |w, (tag, data)| w.bytes(tag, data)).finish();
            assert_eq!(stripped, base.encode());
            if version == 3 || (target.is_some() && version == 2) {
                let shape = |c: &Content| Event::create(&m.keys[0], m.town, Draft { kind: c.kind(), kind_version: version,
                    critical: false, actor: Actor::Human, via: Via::Ui, wall_ms: 0, parents: vec![genesis], content: &c.encode() },
                    [0; 16]).unwrap().header.shape.unwrap();
                assert_eq!(shape(&content), shape(&base));
            }
            copies(&mut content)[0].target_field = Key::Name("colour".into());
            assert_eq!(content.version(), if target.is_some() { 2 } else { 3 });
            assert_eq!(Content::decode(content.kind(), content.version(), &content.encode()), Some(content));
        }
        let mut refs = vec![reference(Some(seat), field::TEXT, vec![record.clone()]),
            reference(None, field::TITLE, vec![doc_line(vec![seat, genesis, seat]), record.clone()]),
            reference(None, field::TITLE, vec![record.clone(), doc_line(vec![])]),
            reference(None, field::Z, vec![])];
        normalize_copy_refs(&mut refs);
        let mut sorted = vec![genesis, seat];
        sorted.sort_unstable();
        assert_eq!(refs, [reference(None, field::TITLE, vec![record.clone(), doc_line(vec![]), doc_line(sorted)]),
            reference(Some(seat), field::TEXT, vec![record.clone()])]);

        for is_set in [false, true] {
            let base = if is_set { set(vec![]) } else { entity(kind::TASK, None, vec![]) };
            let target = || if is_set { FieldWriter::default().bytes(1, &seat) } else { FieldWriter::default() };
            let number = || target().bytes(2, &[1, 0]);
            let source = |kind, id: &[u8]| FieldWriter::default().bytes(1, &[kind]).bytes(2, id);
            let valid = source(1, &genesis).finish();
            let wrong_target = if is_set { FieldWriter::default() } else { FieldWriter::default().bytes(1, &seat) };
            let mut bad_refs = vec![wrong_target.bytes(2, &[1, 0]).bytes(4, &valid).finish(), number().finish(),
                number().text(3, "name").bytes(4, &valid).finish(), number().bytes(2, &[1, 0]).bytes(4, &valid).finish(),
                target().bytes(2, &[1]).bytes(4, &valid).finish(), target().bytes(3, &[0xff]).bytes(4, &valid).finish(),
                target().text(3, "name").text(3, "again").bytes(4, &valid).finish()];
            if is_set { bad_refs.push(number().bytes(1, &seat).bytes(4, &valid).finish()); }
            for invalid in [
                source(5, &genesis), source(1, &genesis).bytes(1, &[1]), source(1, &genesis).bytes(2, &seat),
                source(1, &genesis).bytes(6, &seat), source(2, &seat).bytes(5, &genesis),
                source(2, &seat).bytes(3, &[1, 0]).text(4, "name").bytes(5, &genesis),
                source(2, &seat).bytes(3, &[1, 0]).bytes(3, &[1, 0]).bytes(5, &genesis),
                source(2, &seat).text(4, "name").text(4, "name").bytes(5, &genesis),
                source(2, &seat).text(4, "name").bytes(5, &genesis).bytes(5, &seat),
                source(2, &seat).text(4, "name").bytes(5, &genesis).bytes(6, &seat),
                source(3, &seat).bytes(6, &[1]), source(3, &seat).bytes(5, &genesis),
                source(4, &[4; 64]), source(4, &[3; 65]), source(4, &ben).bytes(6, &seat),
            ] { bad_refs.push(number().bytes(4, &invalid.finish()).finish()); }
            for version in [1, if is_set { 2 } else { 3 }] {
                let decode = |r: &[u8]| Content::decode(base.kind(), version,
                    &[base.encode(), FieldWriter::default().bytes(TAG_COPIED_FROM, r).finish()].concat());
                for r in &bad_refs { assert_eq!(decode(r), None, "malformed copy for kind {} v{version}", base.kind()); }
                assert_eq!(decode(&target().text(3, "name").bytes(4, &valid).finish()).is_some(), version != 1);
                let future = number().bytes(4, &source(1, &genesis).bytes(7, b"future").finish()).bytes(5, b"future").finish();
                assert!(decode(&future).is_some(), "unknown higher tags remain skippable");
            }
        }
        assert_eq!(Content::decode(event::kind::INTAKE, 1, &[]), None);
        for key in [field::GH_REPO, field::GH_STATE, field::GH_FOLDER, field::GH_FILTER] {
            assert!(key.accepts(&Value::Text("open".into())) && key.accepts(&Value::Null) && !key.accepts(&Value::Int(1)));
        }
        assert!(field::GH_NUMBER.accepts(&Value::Int(1)) && field::GH_NUMBER.accepts(&Value::Null)
            && !field::GH_NUMBER.accepts(&Value::Text("1".into())));
        assert!(field::POLICY.accepts(&Value::Int(2)) && !field::POLICY.accepts(&Value::Null));
        for key in [field::GH_TITLE, field::GH_BODY, field::COPIED_FROM] {
            for value in [Value::Null, Value::Int(1), Value::Text("title".into())] { assert!(!key.accepts(&value)); }
        }

        // Entity and document undo chains retain the author of each restored live value.
        let project = m.entity(kind::PROJECT, None, vec![]);
        let doc = m.entity(kind::DOC, Some(project), vec![]);
        let mut create = entity(kind::TASK, Some(project), vec![(field::TITLE, Value::Text("first".into())),
            (field::PROPOSED, Value::Bool(true)), (field::GH_BODY, Value::Text("reserved".into()))]);
        *copies(&mut create) = vec![reference(None, field::TITLE, vec![record.clone(), record.clone()]),
            reference(None, field::GH_BODY, vec![record.clone()]), reference(None, field::Z, vec![record.clone()])];
        let task = m.add(0, Actor::Human, create, None);
        let mut edit = set(vec![change(task, field::TITLE, Value::Text("second".into())),
            change(task, field::POLICY, Value::Int(2))]);
        *copies(&mut edit) = vec![reference(Some(task), field::TITLE, vec![field_source(field::TITLE)]),
            reference(Some(task), field::POLICY, vec![record.clone()]), reference(Some(seat), field::TITLE, vec![record.clone()])];
        let b = m.add(1, Actor::Human, edit, None);
        let check_title = |m: &Maker, writer, origin, title: &str| {
            assert_eq!(fold(&m.events).entities[&task].fields[&field::TITLE],
                FieldState { value: Value::Text(title.into()), writer, origin });
        };
        let undo = m.undo(b);
        check_title(&m, undo, task, "first");
        let redo = m.undo(undo);
        check_title(&m, redo, b, "second");
        let clear = m.set(vec![change(task, field::TITLE, Value::Null)]);
        let undo_clear = m.undo(clear);
        check_title(&m, undo_clear, b, "second");
        let redo_clear = m.undo(undo_clear);
        assert_eq!(fold(&m.events).entities[&task].value(field::TITLE), None);
        let restore = m.undo(redo_clear);
        check_title(&m, restore, b, "second");

        let root = BreakRef::Root;
        let attribute = |attr, value| TextOp::Attr { at: root, attr, value };
        let burst = |ops| Content::Text { doc, ops };
        let first = m.add(0, Actor::Human, burst(vec![TextOp::Insert { anchor: root, text: "aa\n".into() },
            attribute(attr::KIND, Value::Int(1)), attribute(attr::CHECKBOX, Value::Text("open".into()))]), None);
        let second = m.add(1, Actor::Human, burst(vec![TextOp::Insert { anchor: root, text: "bb".into() },
            attribute(attr::KIND, Value::Int(2))]), None);
        let check_attr = |m: &Maker, writer, origin| {
            let state = fold(&m.events);
            assert_eq!(state.docs[&doc].attribute_writer(root, attr::KIND), Some(writer));
            assert_eq!(state.docs[&doc].attribute_origin(root, attr::KIND), origin);
        };
        let undo = m.undo(second);
        check_attr(&m, undo, Some(first));
        let redo = m.undo(undo);
        check_attr(&m, redo, Some(second));
        let clear = m.add(0, Actor::Human, burst(vec![attribute(attr::KIND, Value::Null)]), None);
        let undo = m.undo(clear);
        check_attr(&m, undo, Some(second));
        let redo = m.undo(undo);
        check_attr(&m, redo, None);
        let restored = m.undo(redo);
        check_attr(&m, restored, Some(second));
        let state = fold(&m.events);
        let mut origins = vec![first, second];
        origins.sort_unstable();
        let line = &state.docs[&doc];
        assert_eq!(line.block_attr_origins(root), origins);
        assert_eq!(line.block_run_events(root), Some(origins));
        let newline = CharId { event: first, index: 2 };
        assert_eq!(line.block_run_events(BreakRef::Char(newline)), Some(vec![]));
        assert_eq!(line.block_run_events(BreakRef::Char(CharId { event: first, index: 0 })), None);
        m.add(1, Actor::Human, burst(vec![TextOp::Delete { from: newline, to: newline }]), None);
        assert_eq!(fold(&m.events).docs[&doc].block_run_events(BreakRef::Char(newline)), None);

        assert_eq!(state.copied_from(task, task, &field::TITLE), [record.clone()]);
        assert_eq!(state.copied_from(b, task, &field::TITLE), [field_source(field::TITLE)]);
        for (writer, entity, key) in [(task, task, field::GH_BODY), (task, task, field::Z),
            (b, task, field::POLICY), (b, seat, field::TITLE)] {
            assert!(state.copied_from(writer, entity, &key).is_empty());
        }

        // Seat, profile, visibility and service facts come from the signed fold.
        let profile = |name: &str| Content::Profile { name: name.into(), colour: 1 };
        m.add(0, Actor::Human, profile("Ana"), None);
        let latest = m.add(0, Actor::Human, profile("Lu"), None);
        let void = m.add(1, Actor::Ai, profile("Kit"), None);
        let state = fold(&m.events);
        assert_eq!(state.profiles[&founder].writer, latest);
        assert_eq!(state.founder_seat(), Some(genesis));
        assert!(!state.is_intake(&ben) && !state.is_intake(&[0; 65]));
        let mut intake = state.clone();
        intake.membership.seats.seats.get_mut(&seat).unwrap().role = 1;
        assert!(intake.is_intake(&ben));
        assert_eq!(state.record_meta(&void), Some(RecordMeta { author: ben, actor: Actor::Ai, via: Via::Ui,
            kind: event::kind::PROFILE, void: true, readable: true }));
        assert_eq!(state.record_meta(&latest), Some(RecordMeta { author: founder, actor: Actor::Human, via: Via::Ui,
            kind: event::kind::PROFILE, void: false, readable: true }));
        assert_eq!(state.record_meta(&[0; 32]), None);
        assert!(state.entity_is_visible(&task) && state.doc_is_visible(&doc));
        assert!(!state.doc_is_visible(&task) && !state.entity_is_visible(&[0; 32]));
        let hide = m.set(vec![change(project, field::DELETED, Value::Bool(true))]);
        let state = fold(&m.events);
        assert!(!state.entity_is_visible(&task) && !state.doc_is_visible(&doc));
        m.undo(hide);
        let alias = m.entity(kind::TASK, Some(project), vec![]);
        m.add(0, Actor::Human, merge_task(task, alias), None);
        m.add(1, Actor::Human, merge_task(alias, task), None);
        let state = fold(&m.events);
        assert!(state.entity_is_visible(&task) && !state.entity_is_visible(&alias));

        let session = [3; 16];
        m.add(0, Actor::Human, Content::Session(SessionOp::Open { session, to: vec![], town_wide: true, mode: SessionMode::Call { video: false } }), None);
        for (job, failed) in [([1; 16], false), ([2; 16], true)] {
            let request = m.add(0, Actor::Human, Content::Request { job, session, service: Service::Tasks,
                project: Some(project), provider: ben }, None);
            let expected = ServiceJobMeta { session, project: Some(project), requester: founder, provider: ben,
                request, report_writer: None };
            assert_eq!(fold(&m.events).service_job_meta(&job), Some(expected));
            m.add(1, Actor::Human, Content::Taken { job }, None);
            assert_eq!(fold(&m.events).service_job_meta(&job), Some(expected));
            let done = Content::Done { job, text: "complete".into() };
            let report = m.add(1, Actor::Human, if failed { Content::Failed { job, why: Failure::Interrupted } } else { done.clone() }, None);
            m.add(1, Actor::Human, done, None);
            assert_eq!(fold(&m.events).service_job_meta(&job), Some(ServiceJobMeta { report_writer: Some(report), ..expected }));
        }

        // Skipped signed bytes on a critical event, and unsupported town tiers, close the gate.
        let extra = FieldWriter::default().bytes(63, &[1]).finish();
        let parent = m.events.last().unwrap().id;
        let partial = signed(&mut m, 0, false, &chat("partial", false, None), vec![parent], &extra);
        let critical = signed(&mut m, 0, true, &chat("critical", false, None), vec![partial], &extra);
        let state = fold(&m.events);
        assert_eq!(state.partial, [partial]);
        assert_eq!(state.unknown_critical, [critical]);
        let mut header = m.events[0].header.clone();
        let body = [vec![0; 16], Content::Town { policy: GROUP_POLICY, tier: 1 }.encode()].concat();
        header.body_hash = makepad_network::digest::sha256_hash(&body);
        let bytes = header.encode();
        let town = Event::genesis_town(&bytes);
        let sig = m.keys[0].sign(crate::key::Domain::Event, &[town.as_slice(), &bytes].concat()).unwrap();
        let future = Event::from_parts(town, bytes, body, sig).unwrap();
        let id = future.id;
        let state = fold(&[future]);
        assert_eq!(state.unknown_critical, [id]);
        assert_eq!((state.founder_seat(), state.seat_of(&founder)), (None, None));

        // POLICY and policy-entity visibility require a critical write by the founder seat.
        let policy = |town, value| set(vec![change(town, field::POLICY, Value::Int(value))]);
        for (who, critical, kind, scope) in [
            (1, false, kind::TOWN, None), (1, true, kind::TOWN, None), (0, false, kind::TOWN, None),
            (0, true, kind::PROJECT, None), (0, true, kind::TOWN, Some(project)),
        ] {
            let e = append(&mut m, who, critical, entity(kind, scope, vec![(field::POLICY, Value::Int(2))]));
            append(&mut m, who, critical, policy(e, 2));
            let state = fold(&m.events);
            assert_eq!(state.entities[&e].value(field::POLICY), None);
            assert!(state.policy_writes.is_empty());
            assert_eq!(state.town_policy, TownPolicy::Members);
        }
        let town = append(&mut m, 0, true, entity(kind::TOWN, None, vec![(field::POLICY, Value::Int(1))]));
        assert_eq!(fold(&m.events).policy_writes.get(&(town, town)), Some(&Value::Int(1)));
        let deny = append(&mut m, 0, true, policy(town, 2));
        for (who, critical) in [(0, false), (1, true), (1, false)] {
            append(&mut m, who, critical, set(vec![change(town, field::ARCHIVED, Value::Bool(true)),
                change(town, field::DELETED, Value::Bool(true))]));
            append(&mut m, who, critical, Content::Undo { of: deny });
            append(&mut m, who, critical, Content::Undo { of: town });
        }
        let state = fold(&m.events);
        assert!(state.entity_is_visible(&town));
        assert_eq!(state.town_policy, TownPolicy::Unreadable);
        let undo = append(&mut m, 2, true, Content::Undo { of: deny });
        let state = fold(&m.events);
        assert_eq!(state.entities[&town].fields[&field::POLICY],
            FieldState { value: Value::Int(1), writer: undo, origin: town });
        assert_eq!(state.town_policy, TownPolicy::Members);

        // The Members write sorts last at equal height, but cannot supersede an unseen sibling.
        let restrictive = signed(&mut m, 0, true, &policy(town, 2), vec![undo], &[]);
        let members = signed(&mut m, 2, true, &policy(town, 1), vec![undo], &[]);
        let state = checked_fold(&m.events);
        assert_eq!(state.entities[&town].value(field::POLICY), Some(&Value::Int(1)));
        assert_eq!(state.policy_writes, [((restrictive, town), Value::Int(2)), ((members, town), Value::Int(1))].into());
        assert_eq!(state.town_policy, TownPolicy::Unreadable);
        let resolve = signed(&mut m, 0, true, &policy(town, 1), vec![members, restrictive], &[]);
        let state = fold(&m.events);
        assert_eq!(state.policy_writes, [((resolve, town), Value::Int(1))].into());
        assert_eq!(state.town_policy, TownPolicy::Members);
        append(&mut m, 0, true, policy(town, 2));
        let hide = append(&mut m, 0, true, set(vec![change(town, field::DELETED, Value::Bool(true))]));
        assert_eq!(fold(&m.events).town_policy, TownPolicy::Members);
        append(&mut m, 0, true, Content::Undo { of: hide });
        assert_eq!(fold(&m.events).town_policy, TownPolicy::Unreadable);

        // Deliver a linked checkbox edit after the concurrent unlink. Partial task replay
        // must recover the same provenance under the derived task id as a full board replay.
        let marker = DocState::new(doc).root_id();
        let derived = crate::entity::derived_task(doc, marker);
        let source = CopySource::DocLine { doc, events: vec![first, second] };
        let convert = m.add(0, Actor::Human, Content::Entity { kind: Key::Number(kind::TASK), project: Some(project),
            derive: Some((doc, marker)), fields: vec![(field::TITLE, Value::Text("line".into()))],
            copied_from: vec![reference(None, field::TITLE, vec![source])] }, None);
        let mut update = set(vec![change(derived, field::TEXT, Value::Text("copied".into()))]);
        copies(&mut update).push(reference(Some(derived), field::TEXT, vec![record.clone()]));
        let update = m.add(0, Actor::Human, update, None);
        let tick = m.add(0, Actor::Human, burst(vec![attribute(attr::CHECKBOX, Value::Text("done".into()))]), Some(vec![update]));
        m.add(1, Actor::Human, burst(vec![attribute(attr::TASK, Value::Null)]), Some(vec![update]));
        let state = checked_fold(&m.events);
        let mut late = State::default();
        for e in m.events.iter().filter(|e| e.id != tick) { late.receive(e); }
        late.receive(m.events.iter().find(|e| e.id == tick).unwrap());
        assert_eq!(late, state);
        let mut events = vec![first, second];
        events.sort_unstable();
        assert_eq!(state.copied_from(convert, derived, &field::TITLE), [CopySource::DocLine { doc, events }]);
        assert!(state.copied_from(convert, convert, &field::TITLE).is_empty());
        assert_eq!(state.copied_from(update, derived, &field::TEXT), [record]);
        let restored = State::restore(&m.events, &state.checkpoint(&m.events)).unwrap();
        for projection in [&state, &late, &restored] {
            let expected: BTreeMap<_, _> = projection.conversions.iter()
                .map(|(&creator, &(_, _, task))| (task, creator)).collect();
            assert_eq!(projection.derived_task_creators, expected,
                "derived_task_index_matches_conversions_across_replay_and_restore");
            let meta = Some(DerivedTaskMeta { doc, task: derived, creator: convert, at: BreakRef::Root });
            assert_eq!(projection.derived_task_meta(&derived), meta, "task id resolves the signed conversion");
            assert_eq!(projection.derived_task_meta(&convert), meta, "creator id resolves the same conversion");
        }
        assert_eq!(restored, state, "the checkpoint round trip preserves the entire projection");
    }

    #[test]
    fn opposite_task_merges_converge_and_either_group_can_be_undone() {
        let mut m = Maker::new();
        let a = m.entity(kind::TASK, None, vec![]);
        let b = m.entity(kind::TASK, None, vec![]);
        let base = fold(&m.events);
        let ab = m.add(0, Actor::Human, merge_task(a, b), Some(vec![b]));
        let ba = m.add(1, Actor::Human, merge_task(b, a), Some(vec![b]));
        let folded = checked_fold(&m.events);
        assert_eq!(folded.records[&ab].order.0, folded.records[&ba].order.0, "opposite merges are concurrent at the same height");
        let merges = m.events.last_chunk::<2>().unwrap();
        for order in [[0, 1], [1, 0]] {
            let mut state = base.clone();
            for i in order { state.receive(&merges[i]); }
            assert_eq!(state, folded, "merge delivery order {order:?} must give the canonical fold");
            for id in [a, b] {
                assert_eq!(state.task_survivor(None, &id).unwrap().id, a, "both ids resolve to the earlier creation in order {order:?}");
            }
        }
        for (undo, survivor) in [(ab, a), (ba, b)] {
            m.undo(undo);
            let state = fold(&m.events);
            assert_eq!(state.entities[&survivor].value(field::MERGED_INTO), None, "undo removes the restored task's link");
            assert!(!state.entities[&survivor].flag(field::DELETED), "undo removes the restored task's merge tombstone");
            for id in [a, b] {
                assert_eq!(state.task_survivor(None, &id).unwrap().id, survivor, "the remaining merge points to the restored task");
            }
            m.events.pop(); // Try the other undo from the same two merges.
        }
    }

    #[test]
    fn a_chain_into_a_task_cycle_shares_its_survivor_and_its_later_deletion() {
        let mut m = Maker::new();
        let [tail, a, b, c] = std::array::from_fn(|_| m.entity(kind::TASK, None, vec![]));
        for (from, into) in [(a, b), (b, c), (c, a), (tail, c)] {
            m.add(0, Actor::Human, merge_task(from, into), None);
        }
        let state = checked_fold(&m.events);
        for id in [tail, a, b, c] {
            assert_eq!(state.task_survivor(None, &id).unwrap().id, a, "only cycle members compete, even when an older task leads into the cycle");
        }
        assert_eq!(project_entities(&state, None, Some(kind::TASK)), vec![&state.entities[&a]], "only the cycle survivor is visible");
        m.set(vec![change(a, field::DELETED, Value::Bool(true))]);
        let state = checked_fold(&m.events);
        assert!(project_entities(&state, None, Some(kind::TASK)).is_empty(), "a separate delete hides the survivor without promoting another task");
        for id in [tail, a, b, c] {
            assert!(state.task_survivor(None, &id).unwrap_err().contains("deleted"), "every alias respects the survivor's separate delete");
        }
    }

    #[test]
    fn service_jobs_follow_the_first_request_and_only_its_providers_causal_reports() {
        let mut m = Maker::fresh(2, 4, false);
        let human = Actor::Human;
        let provider_seat = m.invite(0, 1);
        m.accept(1, provider_seat, None);
        let peer_seat = m.invite(0, 2);
        m.accept(2, peer_seat, None);
        let provider = m.keys[1].public();
        let session = [2; 16];
        let other = [3; 16];
        let ring = |session| Content::Session(SessionOp::Open { session, mode: SessionMode::Call { video: false }, to: vec![], town_wide: true });
        let anchor = m.add(0, human, ring(session), None);
        let job = [9; 16];
        let project = Some([4; 32]);
        let ask = |job, session| Content::Request { job, session, service: Service::Tasks, project, provider };
        let done = |job, text: &str| Content::Done { job, text: text.into() };
        let offer = |services| Content::Provides { services };
        let offered = m.add(1, human, offer(vec![Service::Summary]), None);
        let prefix = fold(&m.events);
        assert_eq!(prefix.provides(&provider), &[Service::Summary], "an offer belongs to its signing device");
        let cut = m.events.len();
        let request = m.add(0, human, ask(job, session), None);
        m.add(1, human, Content::Taken { job }, None);
        assert_eq!(fold(&m.events).service_jobs(&session)[0].state, JobState::Working, "a causal Taken from the provider starts work");
        m.add(1, human, done(job, "finished"), None);
        let expected = Job { job, service: Service::Tasks, requester: m.keys[0].public(), provider, project,
            request, state: JobState::Done("finished".into()) };
        for [a, b, c] in [[0,1,2], [0,2,1], [1,0,2], [1,2,0], [2,0,1], [2,1,0]] {
            let mut state = prefix.clone();
            for ix in [a, b, c] { state.receive(&m.events[cut + ix]); }
            assert_eq!(state.service_jobs(&session), [expected.clone()], "delivery {a}{b}{c} reaches the same completed job");
        }

        // Both requests have the offer as their highest parent. The lower author key wins.
        let peer = m.keys[2].public();
        m.add(2, human, Content::Request { job, session, service: Service::Summary, project: None, provider: peer },
            Some(vec![offered]));
        let original = &m.events[cut];
        let collision = m.events.last().unwrap();
        for delivery in [[original, collision], [collision, original]] {
            let mut state = prefix.clone();
            for event in delivery { state.receive(event); }
            let asked = Job { state: JobState::Asked, ..expected.clone() };
            assert_eq!(state.service_jobs(&session), [asked], "both deliveries choose the same requester and provider for a copied job id");
            for report in &m.events[cut + 1..cut + 3] { state.receive(report); }
            assert_eq!(state.service_jobs(&session), [expected.clone()], "the winning provider's reports still complete the shared job id");
        }

        // Make a different request concurrent with the provider's branch. Its Done sorts
        // later even though the provider has never seen this request.
        let second = [1; 16];
        m.add(0, human, ask(second, session), Some(vec![anchor]));
        m.add(1, human, done(second, "unseen"), Some(vec![offered]));
        m.add(2, human, Content::Taken { job: second }, None);
        m.add(2, human, done(second, "another computer"), None);
        let state = fold(&m.events);
        let jobs = state.service_jobs(&session);
        assert_eq!(jobs.iter().map(|j| j.job).collect::<Vec<_>>(), [job, second], "requests sort by canonical request order, not job id");
        assert_eq!(jobs[1].state, JobState::Asked, "neither the wrong signer nor a concurrent provider result advances the job");
        m.add(1, human, Content::Failed { job: second, why: Failure::Interrupted }, None);
        m.add(1, human, done(second, "late"), None);
        m.add(0, human, ask(job, other), None);
        m.add(0, human, ask([7; 16], other), None);
        assert!(fold(&m.events).service_jobs(&other).is_empty(), "requests without an open session do not count");
        m.add(0, human, ring(other), None);
        let ai = m.add(0, Actor::Ai, ask([5; 16], session), None);
        m.accept(3, [0; 32], None);
        let foreign = m.add(3, human, ask([6; 16], session), None);
        m.add(1, human, offer(vec![Service::Tasks]), None);
        let state = checked_fold(&m.events);
        assert_eq!(state.service_jobs(&session)[0], expected, "the first request keeps its provider, session and result");
        assert_eq!(state.service_jobs(&session).len(), 2, "void requests create no jobs");
        assert_eq!(state.service_jobs(&session)[1].state, JobState::Failed(Failure::Interrupted), "the first end wins over a later Done");
        assert_eq!(state.service_jobs(&other).iter().map(|j| j.job).collect::<Vec<_>>(), [[7; 16]], "a late Open makes eligible requests visible without stealing an existing job");
        assert!(state.void_events.contains_key(&ai) && state.void_events.contains_key(&foreign), "AI and non-member requests are void");
        assert_eq!(state.provides(&provider), &[Service::Tasks], "the latest offer replaces earlier capabilities");
        let cached = state.checkpoint(&m.events);
        m.add(1, human, offer(vec![]), None);
        let state = checked_fold(&m.events);
        assert!(state.provides(&provider).is_empty(), "an empty offer turns the switch off");
        assert_eq!(State::restore(&m.events, &cached), Some(state), "a restored service index handles the tail like a full fold");

        let last = m.add(2, human, offer(vec![Service::Transcription]), None);
        let cutoff = m.add(2, human, ask([8; 16], session), None);
        m.add(1, human, done([8; 16], "peer summary"), None);
        let before = checked_fold(&m.events);
        assert_eq!(before.provides(&peer), &[Service::Transcription], "the active peer's offer counts");
        assert_eq!(before.service_jobs(&session).len(), 3, "the active peer's request counts");
        m.remove(0, peer_seat, 2, cutoff, None);
        let removed = checked_fold(&m.events);
        assert!(!removed.void_events.contains_key(&last) && !removed.void_events.contains_key(&cutoff), "the peer's pre-cutoff facts remain valid");
        assert!(removed.provides(&peer).is_empty(), "removing the device withdraws its offer");
        assert_eq!(removed.service_jobs(&session), before.service_jobs(&session), "all three pre-cutoff jobs keep their history after removal");
        assert_eq!(removed.service_jobs(&session)[2].state, JobState::Done("peer summary".into()), "the removed requester's summary stays readable");
    }

    #[test]
    fn group_call_rejoin_restores_active_participant_and_keeps_call_open() {
        let mut m = Maker::town(2, 3);
        let mut state = fold(&m.events);
        let (a, b, c) = (m.keys[0].public(), m.keys[1].public(), m.keys[2].public());
        let to = vec![state.seat_of(&b).unwrap(), state.seat_of(&c).unwrap()];
        let call = [1; 16];
        let share = [2; 16];
        let leave = |session| SessionOp::Leave { session, secs: None, why: LeaveWhy::Ended };
        let join = |session| SessionOp::Join { session };
        let add = |m: &mut Maker, state: &mut State, who, op| {
            m.add(who, Actor::Human, Content::Session(op), None);
            state.receive(m.events.last().unwrap());
        };
        add(&mut m, &mut state, 0, SessionOp::Open { session: call, mode: SessionMode::Call { video: false }, to: to.clone(), town_wide: true });
        for (who, op) in [(1, join(call)), (2, join(call)), (0, leave(call)), (0, join(call)),
            (1, leave(call)), (1, join(call)), (2, leave(call))] {
            add(&mut m, &mut state, who, op);
        }
        assert!(!state.session_summary(&call, None).unwrap().ended, "two rejoined callers keep the call live");
        assert_eq!(state.active_participants(&call), [a, b], "the last enter or leave of each device decides presence");
        assert_eq!(State::restore(&m.events, &state.checkpoint(&m.events)), Some(state.clone()), "a live rejoined call survives a checkpoint");
        for who in [0, 1] { add(&mut m, &mut state, who, leave(call)); }
        assert!(state.session_summary(&call, None).unwrap().ended, "the final departures end the rejoined call");
        assert!(state.active_participants(&call).is_empty(), "the ended call has no active participant");
        assert_eq!(State::restore(&m.events, &state.checkpoint(&m.events)), Some(state.clone()), "the final leaves survive a checkpoint");

        add(&mut m, &mut state, 0, SessionOp::Open { session: share, mode: SessionMode::Share { title: "Drawing".into() }, to, town_wide: true });
        for (who, op) in [(1, join(share)), (0, leave(share)), (0, join(share))] { add(&mut m, &mut state, who, op); }
        assert!(state.session_summary(&share, None).unwrap().ended, "rejoining cannot restart a share after the sharer left");
        assert!(state.active_participants(&share).is_empty(), "an ended share stays empty after a rejoin");
    }

    #[test]
    fn group_call_stays_open_when_opener_leaves_with_two_remaining() {
        let mut m = Maker::town(2, 3);
        let mut state = fold(&m.events);
        let b = m.keys[1].public();
        let c = m.keys[2].public();
        let to = vec![state.seat_of(&b).unwrap(), state.seat_of(&c).unwrap()];
        let call = [1; 16];
        let share = [2; 16];
        let mut add = |state: &mut State, who, op| {
            m.add(who, Actor::Human, Content::Session(op), None);
            state.receive(m.events.last().unwrap());
        };
        for (session, mode) in [(call, SessionMode::Call { video: false }), (share, SessionMode::Share { title: "Drawing".into() })] {
            add(&mut state, 0, SessionOp::Open { session, mode, to: to.clone(), town_wide: true });
            for who in [1, 2] { add(&mut state, who, SessionOp::Join { session }); }
            add(&mut state, 0, SessionOp::Leave { session, secs: None, why: LeaveWhy::Ended });
        }
        assert!(!state.session_summary(&call, None).unwrap().ended, "the other two callers continue when the opener leaves");
        assert_eq!(state.active_participants(&call), [b, c], "only the two remaining callers are active");
        assert!(state.session_summary(&share, None).unwrap().ended, "a share ends when the sharer leaves");
        assert!(state.active_participants(&share).is_empty(), "an ended share has no active viewers");
        for (who, remaining) in [(1, vec![c]), (2, vec![])] {
            add(&mut state, who, SessionOp::Leave { session: call, secs: None, why: LeaveWhy::Ended });
            assert_eq!(state.session_summary(&call, None).unwrap().ended, remaining.is_empty(), "the call ends on the last departure");
            assert_eq!(state.active_participants(&call), remaining, "each departure removes that caller");
        }
    }

    #[test]
    fn town_wide_open_admits_post_open_member_without_missed_call() {
        let mut m = Maker::fresh(2, 3, false);
        let (ana, kit) = (m.keys[0].public(), m.keys[2].public());
        let ana_seat = m.events[0].id;
        let ada_seat = m.invite(0, 1);
        m.accept(1, ada_seat, None);
        let (wide, card, old_wide, empty_untagged) = ([1; 16], [2; 16], [3; 16], [4; 16]);
        let leave = |session| Content::Session(SessionOp::Leave { session, secs: None, why: LeaveWhy::Ended });
        for (session, to, town_wide) in [
            (wide, vec![ada_seat], true), (card, vec![ada_seat], false),
            (old_wide, vec![ada_seat], true), (empty_untagged, vec![], false),
        ] {
            m.add(0, Actor::Human, Content::Session(SessionOp::Open {
                session, mode: SessionMode::Call { video: false }, to, town_wide,
            }), None);
        }
        m.add(0, Actor::Human, leave(old_wide), None);
        let kit_seat = m.invite(0, 2);
        m.accept(2, kit_seat, None);
        for (who, session) in [(2, wide), (2, card), (1, empty_untagged), (2, empty_untagged)] {
            m.add(who, Actor::Human, Content::Session(SessionOp::Join { session }), None);
            m.add(who, Actor::Human, Content::Said { session, lines: vec![
                crate::event::SaidLine { at_ms: 0, dur_ms: 1, text: "hello".into() },
            ] }, None);
            if session == empty_untagged {
                m.add(who, Actor::Human, Content::Session(SessionOp::Decline { session, why: DeclineWhy::Declined }), None);
            }
        }
        let state = checked_fold(&m.events);
        for (session, town_wide) in [(wide, true), (card, false), (old_wide, true)] {
            let summary = state.session_summary(&session, Some(kit_seat)).unwrap();
            assert_eq!((summary.town_wide, summary.viewer), (town_wide, Seen::Other),
                "the signed scope survives and Kit was never rung on {session:?}");
        }
        assert_eq!(state.participants(&wide), [ana, kit], "a new member can join a town-wide call");
        assert_eq!(state.said(&wide).iter().map(|line| (line.author, line.text)).collect::<Vec<_>>(),
            [(kit, "hello")], "the new participant's transcript is counted");
        assert_eq!(state.participants(&card), [ana], "Kit cannot join the card call");
        assert!(state.said(&card).is_empty(), "Kit cannot add a line to the card call");
        assert_eq!(state.session_summary(&old_wide, Some(kit_seat)).unwrap().outcome, Outcome::Unanswered,
            "the earlier town-wide call ended unanswered");
        assert_eq!(state.participants(&empty_untagged), [ana], "an untagged empty audience admits only the opener");
        assert!(state.said(&empty_untagged).is_empty(), "neither uninvited member's lines count");
        for (seat, role) in [(ana_seat, Seen::Opener), (ada_seat, Seen::Other), (kit_seat, Seen::Other)] {
            let summary = state.session_summary(&empty_untagged, Some(seat)).unwrap();
            assert_eq!((summary.viewer, summary.town_wide, summary.outcome), (role, false, Outcome::Pending),
                "an untagged empty audience rings nobody and counts neither decline");
        }
        assert_eq!(State::restore(&m.events, &state.checkpoint(&m.events)), Some(state),
            "a checkpoint restores the signed scopes and folded sessions");
        m.add(0, Actor::Human, leave(empty_untagged), None);
        assert_eq!(fold(&m.events).session_summary(&empty_untagged, Some(kit_seat)).unwrap().outcome,
            Outcome::Unanswered, "only the opener's departure ends the untagged call");
    }

    #[test]
    fn timeline_and_session_views_follow_canonical_signer_facts() {
        use crate::event::SaidLine;
        let mut m = Maker::fresh(2, 4, false);
        let caller = m.events[0].id;
        let callee = m.invite(0, 1);
        let accept_b = m.accept(1, callee, None);
        let third = m.invite(0, 2);
        let accept_c = m.accept(2, third, None);
        let grant = m.vouch(1, callee, 3, None);
        let accept_d = m.accept(3, grant, None);
        let a = m.keys[0].public();
        let b = m.keys[1].public();
        let human = Actor::Human;
        let first_chat = m.add(0, human, chat("before", false, None), None);
        let inputs = fold(&m.events).agent_inputs;
        let call = [1; 16];
        let ring = |session| Content::Session(SessionOp::Open { session, mode: SessionMode::Call { video: true }, to: vec![callee], town_wide: false });
        let join = |session| Content::Session(SessionOp::Join { session });
        let leave = |session, secs| Content::Session(SessionOp::Leave { session, secs, why: LeaveWhy::Ended });
        let decline = |session, why| Content::Session(SessionOp::Decline { session, why });
        let line = |at_ms, text: &str| SaidLine { at_ms, dur_ms: 50, text: text.into() };
        let said = |lines| Content::Said { session: call, lines };

        m.add(1, human, join(call), None);
        let early = m.add(1, human, said(vec![line(20, "second"), line(20, "third")]), None);
        let orphan = fold(&m.events);
        assert!(orphan.session(&call).is_some(), "pre-open facts survive");
        assert_eq!(orphan.session_summary(&call, Some(callee)), None, "an unopened session has no summary");
        assert!(orphan.said(&call).is_empty(), "lines wait for an open");

        // Concurrent opens share a height. Deliver C's higher-key open alone first.
        let anchor = m.add(0, Actor::Ai, ring(call), Some(vec![early]));
        let open_index = m.events.len() - 1;
        let other = m.add(2, human, Content::Session(SessionOp::Open { session: call,
            mode: SessionMode::Share { title: "other".into() }, to: vec![], town_wide: true }), Some(vec![early]));
        let mut reversed = orphan;
        reversed.receive(&m.events[open_index + 1]);
        assert_eq!(reversed.sessions[&call].open.as_ref().unwrap().anchor, other, "first arrival can be the later open");
        reversed.receive(&m.events[open_index]);
        assert_eq!(reversed, fold(&m.events), "a late lower open replaces the anchor and its audience");
        let mut ordered = fold(&m.events[..=open_index]);
        let facts = ordered.sessions.clone();
        let rows = ordered.timeline.clone();
        ordered.receive(&m.events[open_index + 1]);
        assert_eq!((ordered.sessions, ordered.timeline), (facts, rows), "the later open adds no facts or row");

        let first = m.add(0, human, said(vec![line(20, "first")]), None);
        m.add(1, human, join(call), None);
        m.add(2, human, join(call), None);
        m.add(2, human, said(vec![line(0, "uninvited")]), None);
        m.add(3, human, said(vec![line(0, "did not join")]), None);
        let ai_line = m.add(1, Actor::Ai, said(vec![line(0, "fabricated")]), None);
        m.add(0, human, leave(call, Some(10)), None);
        m.add(1, human, leave(call, Some(12)), None);
        m.add(2, human, leave(call, Some(99)), None);
        m.add(3, human, decline(call, DeclineWhy::Busy), None);
        let malformed = m.add(0, human, said(vec![]), None);
        assert_eq!(fold(&m.events).agent_inputs, inputs, "session facts and transcript batches never wake an agent");

        let clip = m.entity(kind::CLIP, None, vec![(field::DURATION, Value::Int(850))]);
        m.undo(clip);
        m.entity(kind::CLIP, Some([0; 32]), vec![]);
        let second_chat = m.add(1, human, chat("after", false, None), None);
        let declined = [2; 16];
        let busy = [3; 16];
        let missed = [4; 16];
        let pending = [5; 16];
        m.add(0, human, ring(declined), None);
        m.add(3, human, decline(declined, DeclineWhy::Busy), None);
        m.add(1, human, decline(declined, DeclineWhy::Declined), None);
        m.add(0, human, leave(declined, None), None);
        m.add(0, human, ring(busy), None);
        let cutoff = m.add(1, human, decline(busy, DeclineWhy::Busy), None);
        m.add(0, human, ring(missed), None);
        m.add(2, human, decline(missed, DeclineWhy::Declined), None);
        m.add(0, human, leave(missed, None), None);
        m.add(0, human, Content::Session(SessionOp::Open { session: pending,
            mode: SessionMode::Share { title: "Canvas".into() }, to: vec![], town_wide: true }), None);

        let cut = m.add(1, human, said(vec![line(5, "after cutoff")]), None);
        let prefix = m.events.len();
        let before_cut = fold(&m.events);
        assert_eq!(before_cut.said(&call)[0].event, cut, "session time orders this later batch before the earlier ones");
        let known = before_cut.said_before(&call, Some(second_chat));
        assert_eq!(known.iter().map(|l| l.event).collect::<Vec<_>>(), [first, early, early],
            "the request includes the pre-open batch and excludes the later batch even though it sorts first");
        m.remove(0, callee, 1, cutoff, None);
        m.accept(3, grant, None);
        let state = checked_fold(&m.events);
        assert_eq!(State::restore(&m.events, &state.checkpoint(&m.events)), Some(state.clone()), "checkpoint restores all facts and rows");
        assert_eq!(State::restore(&m.events, &before_cut.checkpoint(&m.events[..prefix])), Some(state.clone()), "a cutoff after restore retracts lines");
        assert!(state.void_events.contains_key(&ai_line), "an AI cannot claim a person's words");
        assert!(state.void_events.contains_key(&cut), "membership retracts the post-cutoff batch");
        assert_eq!(state.unknown, [malformed], "an undecodable batch stays in the log as unknown");
        assert_eq!(state.sessions[&call].open.as_ref().unwrap().anchor, anchor, "the AI-flagged canonical open anchors the call");
        assert_eq!(state.participants(&call), [a, b], "the opener and eligible joined devices each count once");
        let lines: Vec<_> = state.said(&call).into_iter().map(|l| (l.at_ms, l.dur_ms, l.author, l.event, l.text)).collect();
        assert_eq!(lines, [(20, 50, a, first, "first"), (20, 50, b, early, "second"), (20, 50, b, early, "third")],
            "only participating signers speak, in time, signer, event and line order");
        assert!(state.entities[&clip].flag(field::DELETED), "the clip row survives its tombstone");
        assert_eq!(state.timeline, [Entry::Member(accept_b), Entry::Member(accept_c), Entry::Member(accept_d),
            Entry::Chat(first_chat), Entry::Session(call), Entry::Clip(clip), Entry::Chat(second_chat),
            Entry::Session(declined), Entry::Session(busy), Entry::Session(missed), Entry::Session(pending)],
            "only valid anchors create rows, in canonical order");
        for (viewer, seen) in [(caller, Seen::Opener), (callee, Seen::Rung), (third, Seen::Other)] {
            for (id, outcome) in [
                (call, Outcome::Answered { seats: vec![callee], secs: Some(12) }),
                (declined, Outcome::Declined { seats: vec![callee], busy: false }),
                (busy, Outcome::Declined { seats: vec![callee], busy: true }), (missed, Outcome::Unanswered),
            ] {
                assert_eq!(state.session_summary(&id, Some(viewer)), Some(SessionSummary { opener: a, opener_seat: Some(caller),
                    mode: SessionMode::Call { video: true }, rung: vec![callee], town_wide: false, viewer: seen, outcome,
                    lines: if id == call { 3 } else { 0 }, ended: id != busy }), "each seat reads its own role and the same signed facts for {id:?}");
            }
            let share = state.session_summary(&pending, Some(viewer)).unwrap();
            let role = if viewer == caller { Seen::Opener } else { Seen::Other };
            assert_eq!((share.viewer, share.town_wide, share.outcome, share.ended), (role, true, Outcome::Pending, false),
                "an empty audience rings nobody and the town-wide share stays live");
            assert!(state.sessions[&pending].open.as_ref().unwrap().admits(Some(caller), Some(viewer)),
                "every seated viewer is admitted to the town-wide share");
        }
        assert_eq!(state.session_summary(&pending, None).unwrap().viewer, Seen::Other, "broadcast requires a viewer seat");

        assert!(!state.sessions[&pending].open.as_ref().unwrap().admits(Some(caller), None),
            "an unseated viewer cannot join the town-wide share");

        for (session, prior_chats) in [([6; 16], 0), ([7; 16], 64)] {
            // Maker adds the signer's previous event as a parent, making this a linear history.
            let mut append = |content| m.add(0, human, content, Some(vec![]));
            for _ in 0..prior_chats { append(chat("unrelated", false, None)); }
            let open = append(ring(session));
            let earlier = append(Content::Said { session, lines: vec![line(20, "before signing")] });
            let request = append(chat("send transcript", true, Some(open)));
            let later = append(Content::Said { session, lines: vec![line(5, "after signing")] });
            let state = fold(&m.events);
            assert_eq!(state.said(&session).iter().map(|l| l.event).collect::<Vec<_>>(), [later, earlier],
                "both lines are present in the local transcript");
            assert_eq!(state.said_before(&session, Some(request)).iter().map(|l| l.event).collect::<Vec<_>>(), [earlier],
                "only the line known at signing is shared, with {prior_chats} unrelated chats before the call");
        }
    }

    #[test]
    fn undo_restores_the_first_replaced_value_or_absence_for_every_grouped_field() {
        let mut m = Maker::new();
        let a = m.entity(kind::STICKY, None, vec![(field::X, Value::Int(-64)), (field::Y, Value::Int(64))]);
        let b = m.entity(kind::STICKY, None, vec![]);
        let before = fold(&m.events);
        let group = m.set(vec![
            change(a, field::X, Value::Int(128)), change(a, field::X, Value::Int(192)),
            change(a, field::Y, Value::Null), change(b, field::X, Value::Int(256)),
        ]);
        assert_eq!(fold(&m.events).entities[&a].value(field::Y), None, "a clear removes its field");
        let undo = m.undo(group);
        let after = fold(&m.events);
        for id in [a, b] {
            let values = |state: &State| state.entities[&id].fields.iter().map(|(n, f)| (n.clone(), f.value.clone())).collect::<BTreeMap<_, _>>();
            assert_eq!(values(&after), values(&before), "undo restores exactly the field values and absences of {id:?}");
        }
        assert_eq!(after.entities[&a].fields[&field::X].writer, undo, "the undo owns the restored value");
    }

    #[test]
    fn legacy_entity_bytes_still_create_the_same_typed_register() {
        // Fixed v1 TLV bytes, independent of this build's encoder: Sticky with title "old".
        let bytes = [1, 2, 0, 0, 0, 4, 0, 3, 15, 0, 0, 0,
            1, 2, 0, 0, 0, 1, 0, 6, 3, 0, 0, 0, b'o', b'l', b'd'];
        let content = Content::decode(crate::event::kind::ENTITY, 1, &bytes).unwrap();
        assert_eq!(content.encode(), bytes);
        let mut m = Maker::new();
        let id = m.add(0, Actor::Human, content, None);
        let state = fold(&m.events);
        assert_eq!(state.entities[&id].kind, kind::STICKY);
        assert_eq!(state.entities[&id].fields[&field::TITLE], FieldState { value: Value::Text("old".into()), writer: id, origin: id });
        assert!(m.events.last().unwrap().header.shape.is_none());
    }

    #[test]
    fn named_registers_validate_each_write_and_undo_in_canonical_order() {
        let mut m = Maker::new();
        let dash = Key::Name("dash".into());
        let lit = |s: &str| Value::Literal(s.into());
        let item = m.add(0, Actor::Human, Content::Entity { copied_from: vec![], kind: Key::Name("Sticky".into()), project: None,
            derive: None, fields: vec![(dash.clone(), lit("[4  2]"))] }, None);
        let bad = m.set(vec![change(item, dash.clone(), lit("1 2")),
            change(item, Key::Name("not-a-name".into()), lit("3")),
            change(item, Key::Name("fill".into()), Value::Text("red".into())),
            change(item, field::TITLE, lit("word"))]);
        let state = fold(&m.events);
        assert_eq!(state.entities[&item].kind, Key::Name("Sticky".into()), "the fold keeps the signed kind literally");
        assert!(state.records.contains_key(&bad) && !state.unknown.contains(&bad));
        assert_eq!(state.entities[&item].last_activity, state.entities[&item].created);
        assert_eq!(state.entities[&item].fields.len(), 1);
        assert_eq!(state.entities[&item].fields[&dash].writer, item);
        assert!(state.invalid_keys.is_empty(), "refusing a Splash literal does not flag its author");
        let group = m.set(vec![change(item, dash.clone(), lit("[8 2]")), change(item, field::X, Value::Int(64)),
            change(item, dash.clone(), lit("Diagram{}")), change(item, dash.clone(), lit("["))]);
        let state = fold(&m.events);
        assert_eq!(state.entities[&item].value(dash.clone()), Some(&lit("Diagram{}")));
        let undo = m.undo(group);
        let state = fold(&m.events);
        assert_eq!(state.entities[&item].fields[&dash], FieldState { writer: undo, origin: item, value: lit("[4  2]") });
        assert_eq!(state.entities[&item].value(field::X), None);
        let ordered = m.entity(kind::TASK, None, vec![(field::ORDER, Value::Int(2)), (field::Z, Value::Key(vec![2]))]);
        assert_eq!(fold(&m.events).invalid_keys, [m.keys[0].public()].into(), "creation records its own author");
        m.add(1, Actor::Human, Content::Set { copied_from: vec![], changes: vec![change(ordered, field::ORDER, Value::Key(vec![9, 1])),
            change(ordered, field::Z, Value::Null)], label: None }, None);
        let state = fold(&m.events);
        assert_eq!(state.invalid_keys, [m.keys[0].public(), m.keys[1].public()].into(), "a refused Set records its author too");
        assert!(state.entities[&ordered].order_key().is_none() && state.entities[&ordered].z_key().is_none(),
            "the malformed ORDER stays absent and the Z clear applies");
        let mut reverse = State::default();
        for event in m.events.iter().rev() { reverse.receive(event); }
        assert_eq!(reverse, state);
        assert_eq!(State::restore(&m.events, &state.checkpoint(&m.events)), Some(state));
    }

    #[test]
    fn undo_preserves_a_later_concurrent_write() {
        let mut m = Maker::new();
        let a = m.entity(kind::STICKY, None, vec![]);
        let changes = vec![change(a, field::X, Value::Int(1)), change(a, field::Y, Value::Int(1))];
        let group = m.add(0, Actor::Human, Content::Set { copied_from: vec![], changes, label: None }, Some(vec![a]));
        let changes = vec![change(a, field::X, Value::Int(2))];
        let peer = m.add(1, Actor::Human, Content::Set { copied_from: vec![], changes, label: None }, Some(vec![a]));
        m.undo(group);
        let after = fold(&m.events);
        m.events.reverse();
        assert_eq!(fold(&m.events), after, "concurrent writes converge when arrivals reverse");
        assert_eq!(after.entities[&a].fields[&field::X], FieldState { value: Value::Int(2), writer: peer, origin: peer }, "the later concurrent writer keeps X");
        assert_eq!(after.entities[&a].value(field::Y), None, "the group's uncontested Y is undone");
    }

    #[test]
    fn undo_of_undo_redoes_even_a_clear_or_a_new_field() {
        let mut m = Maker::new();
        let id = m.entity(kind::TASK, None, vec![(field::TITLE, Value::Text("old".into()))]);
        let group = m.set(vec![change(id, field::TITLE, Value::Null), change(id, field::X, Value::Int(0))]);
        let undo = m.undo(group);
        let redo = m.undo(undo);
        let after = fold(&m.events);
        assert_eq!(after.entities[&id].value(field::TITLE), None, "redo clears the restored title again");
        assert_eq!(after.entities[&id].fields[&field::X], FieldState { value: Value::Int(0), writer: redo, origin: group }, "redo can write where undo restored absence");
        let unmake = m.undo(id);
        assert!(fold(&m.events).entities[&id].flag(field::DELETED), "undoing a creation tombstones what it made");
        m.undo(unmake);
        let back = fold(&m.events);
        assert!(!back.entities[&id].flag(field::DELETED), "redoing a creation brings it back");
        assert_eq!(back.entities[&id].fields[&field::X].value, Value::Int(0), "later edits return with it");
    }

    #[test]
    fn document_replay_converges_across_bursts_moves_and_late_arrivals() {
        use crate::doc::{attr, Anchor, BreakRef, CharId, TextOp::*};
        let mut m = Maker::town(2, 3);
        let doc = m.entity(kind::DOC, None, vec![]);
        let other = m.entity(kind::DOC, None, vec![]);
        let burst = |ops| Content::Text { doc, ops };
        let insert = |anchor, text: &str| Insert { anchor, text: text.into() };
        let base = m.add(0, Actor::Human, burst(vec![insert(Anchor::Root, "A\nBC\nD")]), Some(vec![doc]));
        let id = |index| CharId { event: base, index };
        let at = |index| BreakRef::Char(id(index));
        let moved = Move { block: at(1), after: at(4) };
        let early = m.add(0, Actor::Human, burst(vec![insert(at(2), "ab")]), Some(vec![base]));
        let late = m.add(1, Actor::Human, burst(vec![moved.clone(), insert(at(2), "xy")]), Some(vec![base]));
        let mut state = fold(&m.events);
        assert_eq!(state.docs[&doc].text(), "A\nD\nBxyabC", "both concurrent bursts stay whole inside the moved block");
        m.add(0, Actor::Human, burst(vec![insert(at(2), "1"), insert(at(2), "2")]), Some(vec![early, late]));
        m.add(1, Actor::Human, burst(vec![insert(at(2), "3"), insert(at(2), "4")]), Some(vec![early, late]));
        assert!(fold(&m.events).docs[&doc].text().contains("B4321xyab"), "backwards typing at one anchor keeps bursts contiguous");
        let split = m.add(0, Actor::Human, burst(vec![insert(at(2), "\n")]), None);
        m.add(1, Actor::Human, burst(vec![Move { block: at(1), after: BreakRef::Root }]), Some(vec![split]));
        assert_eq!(fold(&m.events).docs[&doc].text(), "A\nB\nD\n4321xyabC", "a move after a split carries only the first half");

        let before = fold(&m.events).docs[&doc].text();
        let deletion = m.add(0, Actor::Human, burst(vec![Delete { from: id(2), to: id(2) }]), None);
        let undo = m.undo(deletion);
        assert_eq!(fold(&m.events).docs[&doc].text(), before, "undo restores deleted text at the original id");
        m.undo(undo);
        let after = fold(&m.events).docs[&doc].text();
        assert_eq!(after, before.replace('B', ""), "undo of undo deletes the same character again");
        let insertion = m.add(0, Actor::Human, burst(vec![insert(at(2), "restored anchor")]), None);
        m.undo(insertion);
        assert_eq!(fold(&m.events).docs[&doc].text(), after, "undo insert restores text even when anchored to a tombstone");
        m.add(2, Actor::Human, burst(vec![insert(Anchor::Root, "BAD"), insert(at(2), "bad")]), Some(vec![doc]));
        assert_eq!(fold(&m.events).docs[&doc].text(), after, "one non-ancestor anchor invalidates the entire event");

        let mut seed = Seed(7123);
        let mut heads = [insertion; 2];
        for i in 0..14 {
            let who = i % 2;
            let op = match (seed.next() >> 16) % 4 {
                0 => insert(at(3), "q"),
                1 => Delete { from: id(0), to: id(0) },
                2 => Attr { at: at(1), attr: attr::INDENT, value: Value::Int(i as i64) },
                _ => moved.clone(),
            };
            heads[who] = m.add(who, Actor::Human, burst(vec![op]), Some(vec![heads[who]]));
            if i % 11 == 0 {
                heads[who] = m.add(who, Actor::Human, Content::Undo { of: heads[who] }, Some(vec![heads[who]]));
            }
        }
        m.add(1, Actor::Human, Content::Text { doc: other, ops: vec![insert(Anchor::Root, "independent")] }, Some(vec![other]));
        let expected = fold(&m.events);
        state = State::default();
        for event in canonical(&m.events).0 { state.apply_last(event); }
        assert_eq!(state, expected, "each canonical append matches full replay");
        assert_eq!(expected.docs[&other].text(), "independent", "documents have independent sequences");
        for round in 0..40 {
            let mut shuffled = m.events.clone();
            seed.shuffle(&mut shuffled);
            let mut received = State::default();
            for event in &shuffled { received.receive(event); }
            assert_eq!(received, expected, "waiting parents and checkpoint rewinds converge in shuffle {round}");
            assert_eq!(fold(&shuffled).docs, expected.docs, "batch fold converges in shuffle {round}");
        }
    }

    #[test]
    fn concurrent_line_conversions_share_identity_and_one_undo_group() {
        let mut m = Maker::new();
        let doc = m.entity(kind::DOC, None, vec![]);
        let at = DocState::new(doc).root_id();
        let first_task = m.entity(kind::TASK, None, vec![]);
        let convert = |title: &str| Content::Entity { copied_from: vec![], kind: Key::Number(kind::TASK), project: None, derive: Some((doc, at)),
            fields: vec![(field::TITLE, Value::Text(title.into()))] };
        let first = m.add(0, Actor::Human, convert("first"), Some(vec![first_task]));
        m.add(1, Actor::Ai, convert("second"), Some(vec![first_task]));
        let task = crate::entity::derived_task(doc, at);
        let state = fold(&m.events);
        assert_eq!(state.task_numbers.len(), 2, "two conversions allocate only one additional task");
        assert_eq!(state.task_numbers[&task], 2, "the canonical first author allocates its number");
        assert_eq!(state.entities[&task].text(field::TITLE), "first", "duplicate conversion retains the first title");
        assert_eq!(state.docs[&doc].attribute(BreakRef::Root, attr::TASK), Some(&Value::Id(task)), "creation links the line in the same event");
        let mut late = State::default();
        for event in m.events.iter().rev() { late.receive(event); }
        assert_eq!(late, state, "the same conversion wins when it arrives late");
        let undo = m.undo(first);
        let state = fold(&m.events);
        assert!(state.entities[&task].flag(field::DELETED) && state.docs[&doc].attribute(BreakRef::Root, attr::TASK).is_none(), "undo hides the untouched task and removes its link together");
        m.undo(undo);
        assert!(!fold(&m.events).entities[&task].flag(field::DELETED), "undo of undo restores the task");
    }

    #[test]
    fn late_linked_ticks_and_unlinks_read_the_task_at_their_position() {
        let mut m = Maker::new();
        let doc = m.entity(kind::DOC, None, vec![]);
        let root = DocState::new(doc).root_id();
        let task = crate::entity::derived_task(doc, root);
        let attr = |attr, value| TextOp::Attr { at: BreakRef::Root, attr, value };
        let conversion = m.add(0, Actor::Human, Content::Entity { copied_from: vec![], kind: Key::Number(kind::TASK), project: None,
            derive: Some((doc, root)), fields: vec![] }, Some(vec![doc]));
        let tick = m.add(0, Actor::Ai, Content::Text { doc, ops: vec![
            attr(attr::CHECKBOX, Value::Text("done".into())),
            TextOp::Insert { anchor: BreakRef::Root, text: "check".into() },
        ] }, Some(vec![conversion]));
        let unlink = m.add(1, Actor::Human, Content::Text { doc, ops: vec![attr(attr::TASK, Value::Null)] }, Some(vec![conversion]));
        m.set(vec![change(task, field::STATUS, Value::Text("done".into()))]);
        let expected = fold(&m.events);
        assert_eq!(expected.docs[&doc].attribute(BreakRef::Root, attr::CHECKBOX), Some(&Value::Text("review".into())),
            "unlink copies the concurrent AI tick at replay time, before the subsequent human completion");
        assert_eq!(expected.entities[&task].text(field::STATUS), "done", "unlinked lines no longer track the task");
        let mut late = State::default();
        for e in m.events.iter().filter(|e| e.id != tick) { late.receive(e); }
        late.receive(m.events.iter().find(|e| e.id == tick).unwrap());
        assert_eq!(late, expected, "a late task tick repairs the task and the unlink together");
        m.undo(unlink);
        assert_eq!(fold(&m.events).docs[&doc].attribute(BreakRef::Root, attr::CHECKBOX), Some(&Value::Text("done".into())),
            "undo unlink displays the current task state");
        m.undo(tick);
        let state = fold(&m.events);
        assert!(state.docs[&doc].text().is_empty() && state.entities[&task].text(field::STATUS) == "done",
            "undo restores the text while preserving a newer human task status");
    }

    #[test]
    fn rewind_resumes_from_the_affected_document_checkpoint() {
        let mut m = Maker::town(2, 3);
        let a = m.entity(kind::DOC, None, vec![]);
        let b = m.entity(kind::DOC, None, vec![]);
        let mut heads = [a, b];
        let mut delayed = 0;
        for n in 0..CHECKPOINT_INTERVAL + 2 {
            for (i, doc) in [a, b].into_iter().enumerate() {
                heads[i] = m.add(i, Actor::Human, Content::Text { doc, ops: vec![
                    TextOp::Insert { anchor: BreakRef::Root, text: "x".into() }
                ] }, Some(vec![heads[i]]));
            }
            if n == CHECKPOINT_INTERVAL - 1 {
                delayed = m.events.len();
                m.add(2, Actor::Human, Content::Text { doc: a, ops: vec![TextOp::Insert { anchor: BreakRef::Root, text: "late".into() }] }, Some(vec![heads[0]]));
            }
        }
        let mut state = State::default();
        for (i, e) in m.events.iter().enumerate() { if i != delayed { state.receive(e); } }
        let snapshot = |s: &State, d| s.timelines[&d].checkpoints[0].1.clone();
        let before = [snapshot(&state, a), snapshot(&state, b)];
        let revisions = [state.doc_revision(a), state.doc_revision(b)];
        state.receive(&m.events[delayed]);
        assert_ne!(state.doc_revision(a), revisions[0], "a late edit changes its document revision");
        assert_eq!(state.doc_revision(b), revisions[1], "an independent document keeps its revision");
        assert_eq!(state.doc_revision(a), fold(&m.events).doc_revision(a), "replicas agree after rewind");
        assert!(Arc::ptr_eq(&snapshot(&state, a), &before[0]), "the valid checkpoint before the late edit is reused");
        assert!(Arc::ptr_eq(&snapshot(&state, b), &before[1]), "the other document's checkpoint is untouched");
        assert_eq!(state, fold(&m.events), "checkpoint suffix replay matches a fresh fold");
    }

    #[test]
    fn cached_prefix_resumes_the_verified_log_and_rejects_a_different_prefix() {
        let mut m = Maker::town(2, 3);
        let doc = m.entity(kind::DOC, None, vec![]);
        let base = m.add(0, Actor::Human, Content::Text { doc, ops: vec![TextOp::Insert { anchor: BreakRef::Root, text: "abc".into() }] }, None);
        let user_edit = m.add(0, Actor::Human, Content::Text { doc, ops: vec![TextOp::Delete {
            from: crate::doc::CharId { event: base, index: 0 }, to: crate::doc::CharId { event: base, index: 1 }
        }] }, None);
        let prefix = fold(&m.events);
        let mut bytes = prefix.checkpoint(&m.events);
        assert_eq!(State::restore(&m.events, &bytes), Some(prefix), "restoring projections also restores character ids and undo evidence");
        m.undo(user_edit);
        m.add(2, Actor::Human, Content::Text { doc, ops: vec![TextOp::Insert { anchor: BreakRef::Root, text: "late".into() }] }, Some(vec![doc]));
        assert_eq!(State::restore(&m.events, &bytes), Some(fold(&m.events)), "a cached prefix handles a later undo and an out-of-order suffix");
        m.events.swap(1, 2);
        assert!(State::restore(&m.events, &bytes).is_none(), "a changed log prefix invalidates its cache");
        m.events.swap(1, 2);
        bytes[0] ^= 1;
        assert!(State::restore(&m.events, &bytes).is_none(), "corruption is a cache miss");
    }

    #[test]
    fn geometry_obeys_the_lock_at_its_position_in_the_group() {
        let mut m = Maker::new();
        let id = m.entity(kind::SHAPE, None, vec![(field::LOCKED, Value::Bool(true))]);
        let geometry = [field::X, field::Y, field::W, field::H, field::Z];
        let value = |f: &Key, n: u8| if *f == field::Z { Value::Key(vec![n]) } else { Value::Int(n.into()) };
        m.set(geometry.iter().map(|f| change(id, f.clone(), value(f, 12))).collect());
        let locked = fold(&m.events);
        assert_eq!(locked.entities[&id].last_activity, locked.entities[&id].created, "a fully blocked group does not bump activity");
        let mut changes = vec![change(id, field::X, Value::Int(1)), change(id, field::LOCKED, Value::Bool(false))];
        changes.extend(geometry[1..].iter().map(|f| change(id, f.clone(), value(f, 64))));
        m.set(changes);
        let after = fold(&m.events);
        assert_eq!(after.entities[&id].value(field::X), None, "the write before unlocking remains blocked");
        assert!(geometry[1..].iter().all(|f| after.entities[&id].value(f.clone()) == Some(&value(f, 64))), "every geometry write after unlocking succeeds");
        assert!(after.invalid_keys.is_empty(), "a lock refusal does not flag the author");
    }

    #[test]
    fn links_and_project_queries_follow_visibility_and_activity() {
        let mut m = Maker::new();
        let project = m.entity(kind::PROJECT, None, vec![]);
        let a = m.entity(kind::TASK, Some(project), vec![]);
        let b = m.entity(kind::TASK, Some(project), vec![]);
        let c = m.entity(kind::TASK, Some(project), vec![]);
        let link = m.entity(kind::LINK, Some(project), vec![(field::FROM, Value::Id(a)), (field::TO, Value::Id(b))]);
        let pair = m.set(vec![change(a, field::ORDER, Value::Key(vec![12])), change(b, field::ORDER, Value::Key(vec![12])),
            change(a, field::Z, Value::Key(vec![7])), change(b, field::Z, Value::Key(vec![7]))]);
        let state = fold(&m.events);
        let mut tied = vec![a, b];
        tied.sort();
        tied.push(c);
        let tasks = |s: &State| project_entities(s, Some(project), Some(kind::TASK)).iter().map(|e| e.id).collect::<Vec<_>>();
        assert_eq!(tasks(&state), tied, "a grouped write ties activity, resolved by id before older tasks");
        assert_eq!(backlinks(&state, b), vec![&state.entities[&link]], "a backlink points to the target");
        assert_eq!(links_from(&state, a), vec![&state.entities[&link]], "an outbound link names the source");
        assert!(backlinks(&state, a).is_empty(), "the direction of a link matters");
        m.set(vec![change(a, field::ORDER, Value::Key(vec![12])), change(a, field::Z, Value::Key(vec![7]))]);
        m.set(vec![change(b, field::TEXT, Value::Text("latest".into()))]);
        let state = fold(&m.events);
        assert!(state.entities[&b].order_key() < state.entities[&a].order_key(), "equal keys sort by the order writer, independent of later activity");
        assert!(state.entities[&b].z_key() < state.entities[&a].z_key(), "equal z keys stack by the z writer, so a later edit never reorders overlapping items");
        assert_eq!(tasks(&state), vec![b, a, c], "newest activity comes first");
        m.set(vec![change(b, field::ARCHIVED, Value::Bool(true)), change(a, field::DELETED, Value::Bool(true)), change(link, field::DELETED, Value::Bool(true))]);
        let hidden = fold(&m.events);
        assert_eq!(tasks(&hidden), vec![c], "deleted and archived tasks are hidden");
        assert!(backlinks(&hidden, b).is_empty() && links_from(&hidden, a).is_empty(), "deleted links disappear in both directions");
        assert!(hidden.entities.contains_key(&link) && hidden.entities.contains_key(&a), "tombstones remain addressable");
        assert_eq!(hidden.entities[&b].fields[&field::ORDER].writer, pair, "unrelated activity leaves the order writer alone");
        assert_eq!(project_entities(&hidden, None, None), vec![&hidden.entities[&project]], "the Inbox contains only its own entities");
    }

    #[test]
    fn unknown_numbers_survive_without_acquiring_known_semantics() {
        let mut m = Maker::new();
        let id = m.entity(u16::MAX, None, vec![(Key::Number(65000), Value::Int(1))]);
        let group = m.set(vec![change(id, Key::Number(65000), Value::Int(2)), change([0; 32], field::TITLE, Value::Null)]);
        m.undo([0; 32]);
        let draft = Draft { kind: 499, kind_version: 1, critical: false, actor: Actor::Human, via: Via::Ui,
            wall_ms: 0, parents: vec![id, group], content: b"future bytes" };
        let unknown = Event::create(&m.keys[0], m.town, draft, [0; 16]).unwrap();
        let unknown_id = unknown.id;
        m.events.push(unknown);
        let state = fold(&m.events);
        assert_eq!(state.entities.len(), 1, "unknown edit targets do not invent entities");
        assert_eq!(state.entities[&id].kind, u16::MAX, "an unfamiliar entity retains its kind");
        assert_eq!(state.entities[&id].fields[&Key::Number(65000)], FieldState { value: Value::Int(2), writer: group, origin: group }, "an unfamiliar field retains its value and writer");
        assert!(project_entities(&state, None, Some(kind::TASK)).is_empty(), "a future entity has no task semantics");
        assert_eq!(state.unknown, vec![unknown_id], "an unknown event is kept aside");
        assert_eq!(m.events.last().unwrap().content(), Some(&b"future bytes"[..]), "uninterpreted bytes stay intact");
    }

    #[test]
    fn only_members_write() {
        let mut m = Maker::new();
        let (a, b) = (m.keys[0].public(), m.keys[1].public());
        let (founder, seat, accept) = (m.events[0].id, m.events[1].id, m.events[2].id);
        m.keys.push(DeviceKey::generate().unwrap());
        let missing = m.accept(2, [0; 32], None);
        let outsider = m.add(2, Actor::Human, chat("outside", true, None), None);
        let concurrent = m.add(1, Actor::Human, chat("past last", true, None), Some(vec![accept]));
        let mutual = m.member(1, Member::Remove { seat: founder, device: a, last: seat }, Some(vec![concurrent]));
        m.member(0, Member::Remove { seat, device: b, last: accept }, Some(vec![accept]));
        let state = checked_fold(&m.events);
        assert_eq!(state.void_events[&outsider], "previous event was void", "an uninvited key cannot post");
        assert_eq!(state.void_events[&missing], "no admission for this device", "accept must name its grant");
        assert_eq!(state.void_events[&concurrent], "after removal", "a cutoff voids a concurrent post");
        assert_eq!(state.void_events[&mutual], "after removal", "the canonically later mutual removal loses");
        assert!(state.active_member(&a) && !state.active_member(&b), "the first removal holds");
        assert!(state.chat.is_empty(), "void writes are absent from chat");
        let mut m = Maker::new();
        let founder = m.events[0].id;
        let grant = m.events[1].id;
        // The fold shows a task only inside a project it knows, so the admitted device makes its own.
        let project = m.add(1, Actor::Human, Content::Entity { copied_from: vec![], kind: Key::Number(kind::PROJECT), project: None, derive: None, fields: vec![] }, None);
        let task = m.add(1, Actor::Human, task(project, "withdrawn"), None);
        let mut live = checked_fold(&m.events);
        assert!(live.entities.contains_key(&task), "the admitted device's work is initially visible");
        let leave = m.member(0, Member::Remove { seat: founder, device: m.keys[0].public(), last: founder }, None);
        live.receive(m.events.last().unwrap());
        assert_eq!(live.membership.removals[&leave], Ok(Removal::Counted), "the founder can leave back to its own genesis");
        assert_eq!(live.void_events[&grant], "after removal", "the cutoff retracts the founder’s later invite");
        assert!(!live.active_member(&m.keys[1].public()) && !live.entities.contains_key(&task),
            "withdrawing the grant also withdraws the dependent seat and its task");
        assert_eq!(live, checked_fold(&m.events), "retroactively withdrawing a grant agrees with full replay");
    }

    #[test]
    fn unsupported_town_policies_are_critical() {
        for policy in [1, 73] {
            let m = Maker::town(policy, 1);
            let state = checked_fold(&m.events);
            assert_eq!(state.unknown_critical, [m.events[0].id]);
            assert_eq!(state.unknown_policy, Some(policy));
            assert_eq!(state.seat_of(&m.keys[0].public()), None);
        }
    }

    #[test]
    fn chat_mentions_sanitize_by_actor_seat_and_checkpoint() {
        let mut m = Maker::fresh(2, 4, false);
        let ben_seat = m.invite(0, 1);
        m.accept(1, ben_seat, None);
        let kit_seat_entry = m.invite(0, 2);
        m.accept(2, kit_seat_entry, None);
        let ana_seat = m.events[0].id;
        let ana_2 = m.vouch(0, ana_seat, 3, None);
        m.accept(3, ana_2, None);
        let base = fold(&m.events);
        let (ana, ben, kit, ana2) = (m.keys[0].public(), m.keys[1].public(), m.keys[2].public(), m.keys[3].public());
        let (s_ana, s_ben, s_kit) = (base.seat_of(&ana).unwrap(), base.seat_of(&ben).unwrap(), base.seat_of(&kit).unwrap());
        assert_eq!(base.seat_of(&ana2), Some(s_ana), "the second device shares Ana's seat");
        let line = |text: &str, mentions: Vec<Id>| Chat { text: text.into(), mentions, ..Chat::default() };
        let human = Actor::Human;

        let kit_ben = m.add(0, human, Content::Chat(line("@Kit @Ben", vec![s_kit, s_ben, s_kit])), None);
        let ben_ana = m.add(1, human, Content::Chat(line("@Ana", vec![s_ana])), None);
        let noisy = m.add(0, human, Content::Chat(line("@me @nobody", vec![s_ana, [9; 32]])), None);
        let ai_line = m.add(0, Actor::Ai, Content::Chat(line("@Ben", vec![s_ben])), None);
        let to_ai = m.add(0, human, Content::Chat(Chat { to_ai: true, ..line("@Ben", vec![s_ben]) }), None);
        let agent = m.add(0, human, Content::AgentAdded { name: "Claude Code".into(), kind: "session".into() }, None);
        let addressed = m.add(1, human, Content::AgentChat { chat: line("@Kit", vec![s_kit]), agent: ChatAgent::Recipient(agent) }, None);
        let ana2_line = m.add(3, human, Content::Chat(line("from the laptop", vec![])), None);
        let ben_reply = m.add(1, human, Content::Chat(Chat { quote: Some(ana2_line), ..line("ok", vec![]) }), None);
        let ana_reply = m.add(0, human, Content::Chat(Chat { quote: Some(ana2_line), ..line("same seat", vec![]) }), None);
        let state = checked_fold(&m.events);

        let mut sorted = vec![s_kit, s_ben];
        sorted.sort();
        let msg = |id: &Id| state.message(id).unwrap();
        assert_eq!(msg(&kit_ben).chat.mentions, sorted, "mentions fold sorted and deduplicated");
        assert!(state.mentions_device(msg(&kit_ben), &ben) && state.mentions_device(msg(&kit_ben), &kit), "mentioned seats are mentioned");
        assert!(!state.mentions_device(msg(&kit_ben), &ana) && !state.mentions_device(msg(&kit_ben), &ana2), "the author's seat is never mentioned by its own line");
        assert!(state.mentions_device(msg(&ben_ana), &ana) && state.mentions_device(msg(&ben_ana), &ana2), "a seat mention reaches every device of the seat");
        assert!(msg(&noisy).chat.mentions.is_empty(), "self and unknown seats fold away");
        assert!(msg(&ai_line).chat.mentions.is_empty() && !state.mentions_device(msg(&ai_line), &ben), "an AI line cannot mention");
        assert!(msg(&to_ai).chat.mentions.is_empty(), "an AI-addressed line carries no mentions");
        assert!(msg(&addressed).chat.mentions.is_empty() && !state.mentions_device(msg(&addressed), &kit), "a recipient-tagged line carries no mentions");
        assert!(!state.addressed_chat(&kit_ben), "a human mention is not an AI-addressed line");
        assert!(state.replies_to_device(msg(&ben_reply), &ana), "quoting the second device's line replies to the first device");
        assert!(!state.replies_to_device(msg(&ana_reply), &ana), "quoting your own seat's line is no reply to you");
        assert!(!state.replies_to_device(msg(&ben_reply), &kit), "another seat is not replied to");
        assert_eq!(state.seat_device(&s_ana), Some(ana.min(ana2)), "a seat's representative is its lowest active device");
        assert_eq!(state.seat_device(&[9; 32]), None, "an unknown seat has no device");

        m.add(3, human, Content::Retract { of: ana2_line }, None);
        let state = checked_fold(&m.events);
        assert!(state.message(&ben_reply).is_some_and(|r| !state.replies_to_device(r, &ana)), "retracting the quoted line clears the reply");
        assert_eq!(State::restore(&m.events, &state.checkpoint(&m.events)), Some(state), "a checkpoint keeps mentions");
    }

    #[test]
    fn agents_belong_to_the_signer_and_cannot_grant_membership() {
        let mut m = Maker::town(2, 2);
        let subject = DeviceKey::generate().unwrap().public();
        let profile = m.add(0, Actor::Human, Content::Profile { name: "Ana".into(), colour: 3 }, None);
        let rename = m.add(0, Actor::Ai, Content::Profile { name: "Ben".into(), colour: 0 }, None);
        let invite = m.add(0, Actor::Ai, Content::Member(Member::Invite { subject, role: 0 }), None);
        let agent = m.add(0, Actor::Human, Content::AgentAdded { name: "Assistant".into(), kind: "session".into() }, None);
        let named = |at| Content::AgentChat { chat: Chat { text: "hi".into(), ..Chat::default() }, agent: at };
        m.add(1, Actor::Human, Content::AgentRemoved { agent }, None);
        m.undo(agent);
        let spoofed = m.add(1, Actor::Ai, named(ChatAgent::Author(agent)), None);
        let mention = m.add(1, Actor::Human, named(ChatAgent::Recipient(agent)), None);
        let reply = m.add(0, Actor::Ai, named(ChatAgent::Author(agent)), None);
        let state = checked_fold(&m.events);
        assert!(state.void_events.contains_key(&rename), "a profile signed on behalf of an AI is void");
        assert_eq!(state.profiles[&m.keys[0].public()], Profile { name: "Ana".into(), colour: 3, writer: profile },
            "an AI cannot change its person's name or colour");
        assert!(state.void_events.contains_key(&invite) && state.invitation(&subject).is_none());
        assert_eq!(state.agents[&agent].owner, m.keys[0].public(), "the next human event survives; only its signer owns the agent");
        let plain = state.message(&spoofed).expect("the spoofed tag cannot erase a signed line");
        assert_eq!((plain.author, plain.actor, plain.author_agent, plain.to_agent), (m.keys[1].public(), Actor::Ai, None, None));
        assert_eq!(state.mentions(&agent), &[mention]);
        assert_eq!(state.message(&reply).unwrap().author_agent, Some(agent));
        assert_eq!(State::restore(&m.events, &state.checkpoint(&m.events)), Some(state));
        // Both devices saw the reply. Neither saw the other's next event; the owner's
        // key sorts first at the same height, so the removal folds before the mention.
        m.add(0, Actor::Human, Content::AgentRemoved { agent }, Some(vec![reply]));
        let concurrent = m.add(1, Actor::Human, named(ChatAgent::Recipient(agent)), Some(vec![reply]));
        let state = checked_fold(&m.events);
        let line = state.message(&concurrent).expect("a concurrent removal cannot erase a human line");
        assert_eq!((line.actor, line.to_agent, line.chat.text.as_str()), (Actor::Human, None, "hi"));
        assert_eq!(state.mentions(&agent), &[mention], "only the earlier verified mention wakes the agent");
        assert!(state.agents.is_empty());
    }

    #[test]
    fn a_mark_on_a_finding_belongs_to_its_author_alone() {
        let mut m = Maker::new();
        let issue = m.entity(kind::ISSUE, None, vec![]);
        let pts = Key::Name("pts".into());
        let drawn = Value::Literal("[0 0 10 10]".into());
        let mark = m.entity(kind::STROKE, None, vec![(field::PARENT, Value::Id(issue)), (pts.clone(), drawn.clone())]);
        let section = m.entity(kind::SECTION, None, vec![]);
        let board_stroke = m.entity(kind::STROKE, None, vec![(field::PARENT, Value::Id(section))]);
        let set = |change_: Change| Content::Set { copied_from: vec![], changes: vec![change_], label: None };
        let flag = |entity, on: bool| set(change(entity, field::DELETED, Value::Bool(on)));
        let projections = |m: &Maker| {
            let live = checked_fold(&m.events);
            let restored = State::restore(&m.events, &live.checkpoint(&m.events)).unwrap();
            assert_eq!(restored, live, "restore reproduces the live projection");
            [live, restored]
        };
        m.add(1, Actor::Human, flag(mark, true), None);
        m.add(1, Actor::Human, set(change(mark, pts.clone(), Value::Literal("[0 0 -10 -10]".into()))), None);
        m.add(1, Actor::Human, Content::Undo { of: mark }, None);
        let board_delete = m.add(1, Actor::Human, flag(board_stroke, true), None);
        for state in projections(&m) {
            assert!(!state.entities[&mark].flag(field::DELETED), "another seat cannot delete a mark, by a set or an undo");
            assert_eq!(state.entities[&mark].value(pts.clone()), Some(&drawn), "another seat cannot redraw a mark");
            assert_eq!(state.entities[&board_stroke].fields[&field::DELETED].writer, board_delete, "a board stroke stays open to every member");
        }
        let delete = m.add(0, Actor::Human, flag(mark, true), None);
        m.add(1, Actor::Human, flag(mark, false), None);
        let after = m.add(0, Actor::Human, chat("Ana", false, None), None);
        for state in projections(&m) {
            assert_eq!(state.entities[&mark].fields[&field::DELETED].writer, delete, "the author deletes their own mark; another seat cannot restore it");
            assert_eq!(state.field_writer_before(&after, &mark, &field::DELETED), Ok(Some(delete)), "the history check agrees with the fold");
        }
    }

    #[test]
    fn an_agent_rename_belongs_to_its_owner_seat() {
        let mut m = Maker::fresh(GROUP_POLICY, 3, false);
        let ana_seat = m.events[0].id;
        let grant = m.vouch(0, ana_seat, 1, None);
        m.accept(1, grant, None);
        let invite = m.invite(0, 2);
        m.accept(2, invite, None);

        let agent = m.add(0, Actor::Human, Content::AgentAdded { name: "Claude Code".into(), kind: "session".into() }, None);
        let rename = |name: &str| Content::AgentRenamed { agent, name: name.into() };
        let first = m.add(0, Actor::Human, rename("Aster"), None);
        let state = checked_fold(&m.events);
        assert_eq!(state.agents[&agent].custom_name.as_deref(), Some("Aster"), "the owner can set a display name");
        assert_eq!(state.agents[&agent].name, "Claude Code", "renaming preserves the backend label");

        // Both devices saw Aster. Device 0 sorts first at this height, but arrives last.
        m.add(1, Actor::Human, rename("Fern"), Some(vec![first]));
        m.add(0, Actor::Human, rename("Briar"), Some(vec![first]));
        let state = checked_fold(&m.events);
        assert_eq!(state.agents[&agent].custom_name.as_deref(), Some("Fern"), "the owner's second device wins the same-height author tie");
        let mut received = State::default();
        for event in &m.events { received.receive(event); }
        assert_eq!(received.agents[&agent].custom_name.as_deref(), Some("Fern"), "late delivery of Briar preserves the canonical winner");

        m.add(2, Actor::Human, rename("Mallory"), None);
        let state = checked_fold(&m.events);
        assert_eq!(state.agents[&agent].custom_name.as_deref(), Some("Fern"), "another seat cannot rename the owner's AI");

        let ai_rename = m.add(0, Actor::Ai, rename("Eve"), None);
        let state = checked_fold(&m.events);
        assert!(state.void_events.contains_key(&ai_rename), "an AI-signed rename is void");
        assert_eq!(state.agents[&agent].custom_name.as_deref(), Some("Fern"), "an AI cannot change its own display name");

        m.add(0, Actor::Human, rename(""), None);
        let state = checked_fold(&m.events);
        assert_eq!(state.agents[&agent].custom_name, None, "an empty name clears the display name");

        m.add(0, Actor::Human, rename("Nora"), None);
        let state = checked_fold(&m.events);
        assert_eq!(state.agents[&agent].custom_name.as_deref(), Some("Nora"), "the owner can set a new name after clearing it");
        assert_eq!(State::restore(&m.events, &state.checkpoint(&m.events)), Some(state), "a checkpoint keeps the custom name");

        m.add(0, Actor::Human, rename("  "), None);
        let state = checked_fold(&m.events);
        assert_eq!(state.agents[&agent].custom_name, None, "a whitespace-only name clears the display name");

        m.add(0, Actor::Human, Content::AgentRemoved { agent }, None);
        m.add(0, Actor::Human, rename("Kit"), None);
        let state = checked_fold(&m.events);
        assert!(!state.agents.contains_key(&agent), "renaming a removed AI leaves it absent");
    }

    #[test]
    fn a_town_takes_three_seats_under_policy_two() {
        let mut m = Maker::town(2, 3);
        let device = m.keys[1].public();
        let (seat, last) = (m.events[1].id, m.events[2].id);
        let state = checked_fold(&m.events);
        assert!(m.keys.iter().all(|k| state.active_member(&k.public())), "all three seats are active");
        assert_eq!(state.membership.seats.seats.len(), 3);
        let later = m.add(1, Actor::Human, chat("concurrent", false, None), None);
        m.remove(0, seat, 1, last, Some(vec![last]));
        let state = checked_fold(&m.events);
        assert_eq!(state.void_events[&later], "after removal", "the same cutoff applies with three seats");
        assert!(!state.active_member(&device) && state.chat.is_empty(), "the removed member's later post is absent");
        let bad_role = m.member(0, Member::Invite { subject: device, role: 1 }, None);
        assert_eq!(checked_fold(&m.events).void_events[&bad_role], "unknown member role");
    }

    #[test]
    fn a_seat_can_admit_its_vouched_device() {
        let mut m = Maker::fresh(2, 3, false);
        let seat = m.events[0].id;
        let grant = m.vouch(0, seat, 1, None);
        let duplicate = m.vouch(0, seat, 1, None);
        let subject = m.keys[1].public();
        let before = checked_fold(&m.events);
        assert_eq!(before.pending_devices().collect::<Vec<_>>(), [subject]);
        assert_eq!(before.invitation(&subject), Some(grant.min(duplicate)));
        assert!(!before.active_member(&subject));
        let joined = m.accept(1, grant, Some(vec![duplicate]));
        let mut waiting = fold(&m.events[..2]);
        waiting.receive(m.events.last().unwrap());
        assert_eq!(waiting.invitation(&subject), Some(grant), "a waiting Accept has not spent the key");
        waiting.receive(&m.events[2]);
        let state = checked_fold(&m.events);
        assert_eq!(waiting, state);
        assert_eq!(state.seat_of(&subject), Some(seat));
        assert!(state.active_member(&subject) && state.active_member(&m.keys[0].public()));
        assert_eq!(state.pending_devices().next(), None);
        assert_eq!(state.invitation(&subject), None);
        let other = m.vouch(1, seat, 2, None);
        let independent = m.add(0, Actor::Human, chat("concurrent with vouch", false, None), Some(vec![joined]));
        let no_past = m.accept(2, other, Some(vec![independent]));
        assert_eq!(checked_fold(&m.events).void_events[&no_past], "invalid admission");
    }

    #[test]
    fn a_different_seat_cannot_vouch_for_a_device() {
        let mut m = Maker::town(2, 2);
        m.keys.push(DeviceKey::generate().unwrap());
        let bad = m.vouch(0, m.events[1].id, 2, None);
        let refused = m.accept(2, bad, None);
        let later = m.add(0, Actor::Human, chat("after a void vouch", false, None), None);
        let state = checked_fold(&m.events);
        assert_eq!(state.void_events[&bad], "only the seat's own devices add devices");
        assert_eq!(state.void_events[&refused], "no admission for this device");
        assert_eq!(state.void_events[&later], "previous event was void");
        assert!(!state.active_member(&m.keys[2].public()));
    }

    #[test]
    fn removed_keys_cannot_take_another_offer() {
        let mut m = Maker::new();
        let (root, seat, joined) = (m.events[0].id, m.events[1].id, m.events[2].id);
        m.remove(1, seat, 1, joined, None);
        let invite = m.invite(0, 1);
        let add = m.vouch(0, root, 1, None);
        let again = m.accept(1, invite, None);
        let state = checked_fold(&m.events);
        let key = m.keys[1].public();
        assert_eq!(state.seat_of(&key), Some(seat), "a surviving grant remains historical");
        assert!(state.admitted_devices().any(|device| device == key));
        assert_eq!(state.invitation(&key), None);
        for entry in [invite, add] {
            assert!(state.membership.seats.offers.contains_key(&entry));
            assert!(!state.void_events.contains_key(&entry));
        }
        assert_eq!(state.pending_devices().next(), None);
        assert_eq!(state.void_events[&again], "after removal");
    }

    #[test]
    fn last_device_leaving_races_a_vouched_accept() {
        for accept_first in [false, true] {
            let mut m = Maker::fresh(2, 3, accept_first);
            let seat = m.invite(0, 1);
            m.accept(1, seat, None);
            let grant = m.vouch(1, seat, 2, None);
            let leave = m.remove(1, seat, 1, grant, Some(vec![grant]));
            let accept = m.accept(2, grant, Some(vec![grant]));
            let fresh = m.invite(0, 2);
            let again = m.accept(2, fresh, None);
            let state = checked_fold(&m.events);
            assert_eq!(state.membership.removals[&leave], Ok(Removal::Counted));
            assert_eq!(state.void_events.contains_key(&accept), !accept_first);
            assert_eq!(state.membership.seats.open(&seat), accept_first);
            assert_eq!(state.active_member(&m.keys[2].public()), accept_first);
            assert_eq!(state.invitation(&m.keys[2].public()), None);
            assert!(state.void_events.contains_key(&again));
        }
    }

    #[test]
    fn removal_rights_follow_the_invite_tree() {
        let mut m = Maker::fresh(2, 5, false);
        let root = m.events[0].id;
        let parent = m.invite(0, 1);
        let joined = m.accept(1, parent, None);
        let sibling = m.invite(0, 2);
        m.accept(2, sibling, None);
        let child = m.invite(1, 3);
        m.accept(3, child, None);
        let grandchild = m.invite(3, 4);
        let last = m.accept(4, grandchild, None);
        let prefix = m.events.len();
        for (by, seat, who, cutoff, allowed) in [
            (4, grandchild, 4, last, true), (1, grandchild, 4, last, true),
            (0, grandchild, 4, last, true), (2, parent, 1, joined, false),
            (1, root, 0, sibling, false),
        ] {
            let removal = m.remove(by, seat, who, cutoff, None);
            let state = checked_fold(&m.events);
            assert_eq!(state.membership.removals[&removal],
                if allowed { Ok(Removal::Counted) } else { Err("may not remove this seat") });
            assert_eq!(state.active_member(&m.keys[who].public()), !allowed);
            m.events.truncate(prefix);
        }
    }

    #[test]
    fn a_device_cannot_cut_its_own_admission_chain() {
        let mut m = Maker::fresh(2, 3, false);
        let root = m.events[0].id;
        let v = m.vouch(0, root, 1, None);
        m.accept(1, v, None);
        let b = m.vouch(1, root, 2, None);
        m.accept(2, b, None);
        let prefix = m.events.len();
        for (who, cutoff) in [(0, root), (0, v), (1, b)] {
            let refused = m.remove(2, root, who, cutoff, None);
            let state = checked_fold(&m.events);
            assert_eq!(state.void_events.get(&refused), Some(&"may not remove this device"),
                "a vouchee cannot remove an ancestor at either cutoff");
            assert!(state.active_member(&m.keys[0].public()), "the seat root stays active");
            assert!(state.active_member(&m.keys[1].public()), "the direct voucher stays active");
            m.events.truncate(prefix);
        }
    }

    #[test]
    fn vouch_tree_blocks_removing_voucher_or_sibling_even_when_backdated() {
        for descending in [false, true] {
            let mut m = Maker::fresh(2, 4, descending);
            let root = m.events[0].id;
            let (a, a2, a3, a2b) = (m.keys[0].public(), m.keys[1].public(), m.keys[2].public(), m.keys[3].public());
            let grant_a2 = m.vouch(0, root, 1, None);
            let accept_a2 = m.accept(1, grant_a2, None);
            let grant_a3 = m.vouch(0, root, 2, None);
            let accept_a3 = m.accept(2, grant_a3, None);
            let prefix = m.events.len();

            let mut tip = accept_a3;
            for _ in 0..20 {
                tip = m.add(0, Actor::Human, chat("later", false, None), Some(vec![tip]));
            }
            let backdated = m.remove(1, root, 0, grant_a2, Some(vec![accept_a2]));
            let evict = m.remove(0, root, 1, accept_a2, Some(vec![tip]));
            let state = checked_fold(&m.events);
            assert_eq!(state.membership.removals.get(&backdated), Some(&Err("may not remove this device")),
                "backdating cannot authorize removal of the voucher");
            assert_eq!(state.membership.removals.get(&evict), Some(&Ok(Removal::Counted)),
                "the voucher can evict its vouchee at the later height");
            assert!(state.active_member(&a) && !state.active_member(&a2), "only the vouchee loses membership in the race");
            m.events.truncate(prefix);

            let grant_a2b = m.vouch(1, root, 3, None);
            let accept_a2b = m.accept(3, grant_a2b, None);
            let prefix = m.events.len();
            let evict = m.remove(0, root, 1, accept_a2, Some(vec![accept_a2b]));
            let refuse = m.remove(1, root, 0, grant_a2, Some(vec![accept_a2b]));
            let state = checked_fold(&m.events);
            let reason = if descending { "may not remove this device" } else { "after removal" };
            assert_eq!(state.membership.removals.get(&evict), Some(&Ok(Removal::Counted)),
                "the voucher wins at equal height in either key order");
            assert_eq!(state.membership.removals.get(&refuse), Some(&Err(reason)),
                "the vouchee's equal-height removal is judged in canonical order");
            assert!(state.active_member(&a) && !state.active_member(&a2), "the equal-height race keeps the voucher active");
            m.events.truncate(prefix);

            let sibling = m.remove(1, root, 2, accept_a3, None);
            let state = checked_fold(&m.events);
            assert_eq!(state.membership.removals.get(&sibling), Some(&Err("may not remove this device")),
                "a vouchee cannot remove its sibling");
            assert!(state.active_member(&a3), "the sibling stays active");
            m.events.truncate(prefix);

            let direct = m.remove(1, root, 3, accept_a2b, None);
            let state = checked_fold(&m.events);
            assert_eq!(state.membership.removals.get(&direct), Some(&Ok(Removal::Counted)),
                "a vouchee can remove its own direct vouchee");
            assert!(!state.active_member(&a2b), "the directly removed device is inactive");
            m.events.truncate(prefix);

            let leave = m.remove(1, root, 1, grant_a2b, None);
            let state = checked_fold(&m.events);
            assert_eq!(state.membership.removals.get(&leave), Some(&Ok(Removal::Counted)), "self-removal counts");
            assert!(!state.active_member(&a2) && state.active_member(&a2b), "leaving preserves the vouchee admitted by the cutoff");
            let grandchild = m.remove(0, root, 3, accept_a2b, None);
            let state = checked_fold(&m.events);
            assert_eq!(state.membership.removals.get(&grandchild), Some(&Ok(Removal::Counted)),
                "a grandparent retains removal authority through a departed voucher");
            assert!(!state.active_member(&a2b), "the removed grandchild is inactive");
        }
    }

    #[test]
    fn removal_and_invite_queries_match_fold_rules() {
        let solo = Maker::fresh(2, 1, false);
        let founder = solo.keys[0].public();
        let state = fold(&solo.events);
        assert!(state.can_remove_device(&founder, &founder), "the founder's sole device can remove itself");
        assert!(!state.can_remove_seat(&founder, &solo.events[0].id), "the founder cannot remove its own seat");
        assert!(state.invited_by(&founder).is_empty(), "genesis is not an invited seat");

        let mut m = Maker::fresh(2, 9, false);
        let [a, a2, a3, a2b, b, b2, c, pending, unknown] = [0, 1, 2, 3, 4, 5, 6, 7, 8];
        let keys: Vec<_> = m.keys.iter().map(DeviceKey::public).collect();
        let root = m.events[0].id;
        let grant_a2 = m.vouch(a, root, a2, None);
        m.accept(a2, grant_a2, None);
        let grant_a3 = m.vouch(a, root, a3, None);
        m.accept(a3, grant_a3, None);
        let grant_a2b = m.vouch(a2, root, a2b, None);
        m.accept(a2b, grant_a2b, None);
        let seat_b = m.invite(a, b);
        m.accept(b, seat_b, None);
        let grant_b2 = m.vouch(b, seat_b, b2, None);
        let accept_b2 = m.accept(b2, grant_b2, None);
        let seat_c = m.invite(b, c);
        let accept_c = m.accept(c, seat_c, None);
        let seat_pending = m.invite(b, pending);
        let competing = m.invite(a, c);
        let state = fold(&m.events);

        for (actor, allowed) in [
            (a, &[a, a2, a3, a2b, b, b2, c][..]),
            (a2, &[a2, a2b, b, b2, c]), (a3, &[a3, b, b2, c]), (a2b, &[a2b, b, b2, c]),
            (b, &[b, b2, c]), (b2, &[b2, c]), (c, &[c]), (pending, &[]), (unknown, &[]),
        ] {
            for (target, key) in keys.iter().enumerate() {
                assert_eq!(state.can_remove_device(&keys[actor], key), allowed.contains(&target),
                    "device {actor} can remove {target} only by self, vouch ancestry, or seat ancestry");
            }
        }
        for (actor, seat, allowed) in [
            (a, root, false), (b, seat_b, false), (a, seat_pending, false), (b, seat_pending, false),
            (b, root, false), (c, seat_b, false), (a, grant_a2, false), (a, competing, false),
            (a, seat_b, true), (a2, seat_c, true), (b, seat_c, true),
        ] {
            assert_eq!(state.can_remove_seat(&keys[actor], &seat), allowed,
                "device {actor} can remove a seat only when it is a populated strict descendant");
        }
        assert_eq!(state.invited_by(&keys[a]), [(seat_b, keys[b])],
            "the founder's only direct accepted invite is B's seat");
        assert_eq!(state.invited_by(&keys[b]), [(seat_c, keys[c])],
            "B's direct accepted invites exclude the pending seat and the same-seat vouch");

        m.remove(a2, root, a2, grant_a2b, None);
        let state = fold(&m.events);
        assert!(!state.can_remove_device(&keys[a2], &keys[a2b]), "a departed voucher cannot remove its vouchee");
        assert!(!state.can_remove_device(&keys[a], &keys[a2]), "a departed vouchee cannot be removed again through the query");
        assert!(state.can_remove_device(&keys[a], &keys[a2b]), "historical vouches connect the founder to its active grandchild");
        assert!(!state.can_remove_seat(&keys[a2], &seat_c), "a departed actor cannot remove a descendant seat");

        m.remove(b, seat_b, b, seat_pending, None);
        m.remove(b2, seat_b, b2, accept_b2, None);
        let state = fold(&m.events);
        assert!(!state.can_remove_seat(&keys[a], &seat_b), "an emptied seat cannot be removed");
        assert!(state.can_remove_seat(&keys[a], &seat_c), "an empty intermediate seat preserves invite ancestry");
        assert_eq!(state.invited_by(&keys[a]), [(seat_b, keys[b])], "an emptied seat retains its inviter");
        m.remove(c, seat_c, c, accept_c, None);
        let state = fold(&m.events);
        assert!(!state.active_member(&keys[c]), "the last device has left the grandchild seat");
        assert_eq!(state.invited_by(&keys[b]), [(seat_c, keys[c])], "a departed inviter retains its accepted invite");
        assert_eq!(State::restore(&m.events, &state.checkpoint(&m.events)), Some(state),
            "the checkpoint restores the same membership and admission history");
    }

    #[test]
    fn reinstated_by_fork_filters_active_valid_unique_removals() {
        let mut m = Maker::fresh(2, 4, false);
        let seat_b = m.invite(0, 1);
        m.accept(1, seat_b, None);
        let seat_c = m.invite(1, 2);
        let accept_c = m.accept(2, seat_c, None);
        let vouch_b2 = m.vouch(1, seat_b, 3, None);
        let accept_b2 = m.accept(3, vouch_b2, None);
        let before_fork = m.add(1, Actor::Human, chat("before fork", false, None), None);
        let r1 = m.remove(1, seat_c, 2, accept_c, None);
        let r2 = m.remove(1, seat_c, 2, accept_c, None);
        let r_b2 = m.remove(1, seat_b, 3, accept_b2, None);
        let state = fold(&m.events);
        for id in [r1, r2, r_b2] {
            assert_eq!(state.membership.removals.get(&id), Some(&Ok(Removal::Counted)),
                "each removal counts before its author forks");
        }
        assert!(state.reinstated_by_fork().is_empty(), "there are no reinstatements before a fork");

        m.write_with_parents(1, Actor::Human, chat("other copy", false, None), vec![before_fork]);
        let state = checked_fold(&m.events);
        let (b, c, b2) = (m.keys[1].public(), m.keys[2].public(), m.keys[3].public());
        let restored_c = ReinstatedDevice { forked_device: b, target_device: c, removal_event: r1 };
        let restored_b2 = ReinstatedDevice { forked_device: b, target_device: b2, removal_event: r_b2 };
        assert_eq!(state.forked(&b), Some(ForkPoint::After(before_fork)),
            "the fork falls after B2's admission and below the removals");
        assert!(!state.can_remove_device(&b, &c) && !state.can_remove_device(&c, &b),
            "a forked device is ineligible as either actor or target");
        assert_eq!(state.reinstated_by_fork(), [restored_c, restored_b2.clone()],
            "the earliest removal per target survives deduplication in canonical order");
        m.remove(0, seat_c, 2, accept_c, None);
        assert_eq!(fold(&m.events).reinstated_by_fork(), [restored_b2],
            "a subsequent valid removal takes C out of the reinstated devices");
        m.remove(3, seat_b, 3, accept_b2, None);
        assert!(fold(&m.events).reinstated_by_fork().is_empty(), "B2 leaving clears the remaining reinstatement");

        let mut m = Maker::fresh(2, 7, false);
        let [a, a2, a3, b, c, d, u] = [0, 1, 2, 3, 4, 5, 6];
        let root = m.events[0].id;
        let grant_a2 = m.vouch(a, root, a2, None);
        let accept_a2 = m.accept(a2, grant_a2, None);
        let grant_a3 = m.vouch(a, root, a3, None);
        let accept_a3 = m.accept(a3, grant_a3, None);
        let seat_b = m.invite(a, b);
        let accept_b = m.accept(b, seat_b, None);
        let seat_c = m.invite(a, c);
        let accept_c = m.accept(c, seat_c, None);
        let seat_d = m.invite(b, d);
        let accept_d = m.accept(d, seat_d, None);
        let seat_u = m.invite(a, u);

        let ancestor = m.remove(c, root, a, root, None);
        let other_seat = m.remove(c, seat_b, b, accept_b, None);
        m.write_with_parents(c, Actor::Human, chat("other copy", false, None), vec![accept_c]);
        let voucher = m.remove(a2, root, a, grant_a2, None);
        let sibling = m.remove(a2, root, a3, accept_a3, None);
        m.write_with_parents(a2, Actor::Human, chat("other copy", false, None), vec![accept_a2]);
        let state = fold(&m.events);
        assert!(state.active_member(&m.keys[b].public()), "the sibling seat is still active when C forks");
        assert!(state.reinstated_by_fork().is_empty(), "forks cannot reinstate unauthorized cross-seat or same-seat targets");

        let ai = m.add(b, Actor::Ai, Content::Member(Member::Remove {
            seat: seat_d, device: m.keys[d].public(), last: accept_d,
        }), None);
        let wrong_author = m.remove(b, seat_d, d, accept_b, None);
        let unseen = m.add(d, Actor::Human, chat("concurrent", false, None), Some(vec![accept_d]));
        let noncausal = m.remove(b, seat_d, d, unseen, Some(vec![wrong_author]));
        let wrong_seat = m.remove(b, seat_c, d, accept_d, None);
        let self_removal = m.remove(b, seat_b, b, accept_b, None);
        m.write_with_parents(b, Actor::Human, chat("other copy", false, None), vec![seat_d]);
        let accept_u = m.accept(u, seat_u, Some(vec![seat_u]));
        let unadmitted = m.remove(u, root, a, root, Some(vec![accept_u]));
        m.write_with_parents(u, Actor::Human, Content::Member(Member::Accept { entry: seat_u }), vec![seat_u]);

        let state = checked_fold(&m.events);
        assert_eq!(state.forked(&m.keys[u].public()), Some(ForkPoint::Start), "two first Accepts fork U at Start");
        for id in [ancestor, other_seat, voucher, sibling, ai, wrong_author, noncausal, wrong_seat, self_removal, unadmitted] {
            assert_eq!(state.void_events.get(&id), Some(&FORKED), "each candidate removal is in its author's forked suffix");
        }
        assert!(state.active_member(&m.keys[d].public()), "the invalid removals leave D active");
        assert!(state.reinstated_by_fork().is_empty(), "only a human removal with historical authority and a causal target cutoff qualifies");
        assert_eq!(State::restore(&m.events, &state.checkpoint(&m.events)), Some(state),
            "the checkpoint restores the same fork evidence and removal history");
    }

    #[test]
    fn vouched_by_returns_only_direct_surviving_device_grants() {
        let mut m = Maker::fresh(2, 5, false);
        let root = m.events[0].id;
        let invited = m.invite(0, 4);
        m.accept(4, invited, None);
        let grant = m.vouch(0, root, 1, None);
        m.accept(1, grant, None);
        let next = m.vouch(1, root, 2, None);
        let joined = m.accept(2, next, None);
        m.vouch(0, root, 3, None);
        m.remove(1, root, 2, joined, None);
        let state = checked_fold(&m.events);
        assert_eq!(state.vouched_by(&m.keys[0].public()), [(m.keys[1].public(), grant)]);
        assert_eq!(state.vouched_by(&m.keys[1].public()), [(m.keys[2].public(), next)]);
    }

    // V and F both observe B's Accept and remove from that frontier. F cuts V back to
    // V's Accept, before the vouch for B. Key order alone orders the two removals.
    fn cut_voucher(m: &mut Maker) -> (Id, Id, Id, Id) {
        let root = m.events[0].id;
        let v = m.vouch(0, root, 1, None);
        let v_joined = m.accept(1, v, None);
        let b = m.vouch(1, root, 2, None);
        let b_joined = m.accept(2, b, None);
        let remove_b = m.remove(1, root, 2, b_joined, Some(vec![b_joined]));
        let remove_v = m.remove(0, root, 1, v_joined, Some(vec![b_joined]));
        (b, b_joined, remove_b, remove_v)
    }

    #[test]
    fn a_withdrawn_admission_and_a_counted_removal_have_distinct_histories() {
        for voucher_first in [false, true] {
            let mut m = Maker::fresh(2, 3, voucher_first);
            let (grant, joined, removal, _) = cut_voucher(&mut m);
            let root = m.events[0].id;
            let key = m.keys[2].public();
            let original = checked_fold(&m.events);
            assert_eq!(original.void_events[&grant], "after removal");
            assert!(original.void_events.contains_key(&joined));
            assert_eq!(original.seat_of(&key), None);
            assert!(original.admitted_devices().all(|k| k != key));
            assert!(original.vouched_by(&m.keys[1].public()).is_empty());
            assert_eq!(original.membership.removals[&removal],
                if voucher_first { Ok(Removal::Counted) } else { Err("after removal") });
            assert_eq!(original.void_events.contains_key(&removal), !voucher_first);

            let post = m.add(2, Actor::Human, chat("from B", false, None), Some(vec![joined]));
            let invite = m.invite(0, 2);
            let vouch = m.vouch(0, root, 2, None);
            let again = m.accept(2, invite, None);
            let next = m.add(0, Actor::Human, chat("still a member", false, None), None);
            let state = checked_fold(&m.events);
            assert!(state.void_events.contains_key(&post));
            assert_eq!(state.chat.iter().map(|c| c.id).collect::<Vec<_>>(), [next]);
            assert!(!state.active_member(&key));
            assert_eq!(state.invitation(&key), None);
            assert_eq!(state.pending_devices().next(), None);
            for entry in [invite, vouch] {
                assert!(!state.void_events.contains_key(&entry), "the offer event is valid");
                assert!(state.membership.seats.offers.contains_key(&entry));
            }
            assert_eq!(state.void_events[&again],
                if voucher_first { "after removal" } else { "previous event was void" });
        }
    }

    // F1 sees B's valid admission. F2 can decide concurrently on the same frontier,
    // or sign on the older frontier where V had accepted but had not vouched for B.
    fn founder_removals(m: &mut Maker, old_frontier: bool) -> (Id, Id, Id) {
        let root = m.events[0].id;
        let phone = m.vouch(0, root, 1, None);
        m.accept(1, phone, None);
        let seat = m.invite(0, 2);
        let v = m.accept(2, seat, None);
        let offer = m.vouch(2, seat, 3, None);
        let b = m.accept(3, offer, None);
        let remove_b = m.remove(0, seat, 3, b, Some(vec![b]));
        let post = m.add(0, Actor::Human, chat("after removing B", false, None), Some(vec![remove_b]));
        let remove_v = m.remove(1, seat, 2, v, Some(vec![if old_frontier { v } else { b }]));
        (remove_b, post, remove_v)
    }

    #[test]
    fn withdrawing_the_target_grant_cannot_void_the_founders_removal() {
        for (phone_first, old_frontier) in [(false, false), (true, false), (false, true)] {
            let mut m = Maker::fresh(2, 4, phone_first);
            let (remove_b, post, _) = founder_removals(&mut m, old_frontier);
            let state = checked_fold(&m.events);
            let moot = phone_first || old_frontier;
            assert_eq!(state.membership.removals[&remove_b], Ok(if moot { Removal::Moot } else { Removal::Counted }));
            assert_eq!(state.membership.cutoffs.contains_key(&m.keys[3].public()), !moot);
            assert!(!state.void_events.contains_key(&remove_b));
            assert_eq!(state.chat[0].id, post);
            assert_eq!(state.seat_of(&m.keys[3].public()), None);
            let founders = BTreeSet::from([m.keys[0].public(), m.keys[1].public()]);
            assert_eq!(state.active_members().collect::<BTreeSet<_>>(), founders);
            if old_frontier {
                let (delayed, prefix) = m.events.split_last().unwrap();
                let mut received = fold(prefix);
                assert_eq!(received.membership.removals[&remove_b], Ok(Removal::Counted));
                received.receive(delayed);
                assert_eq!(received, state, "an old removal delivered last revises counted to moot");
            }
        }
    }

    #[test]
    fn a_second_counted_removal_can_lower_the_cutoff() {
        let mut m = Maker::fresh(2, 3, false);
        let root = m.events[0].id;
        let phone = m.vouch(0, root, 1, None);
        m.accept(1, phone, None);
        let seat = m.invite(0, 2);
        let early = m.accept(2, seat, None);
        let late = m.add(2, Actor::Human, chat("past the lower cutoff", false, None), None);
        let first = m.remove(0, seat, 2, late, Some(vec![late]));
        let second = m.remove(1, seat, 2, early, Some(vec![late]));
        let a = m.add(0, Actor::Human, chat("laptop", false, None), None);
        let b = m.add(1, Actor::Human, chat("phone", false, None), None);
        let state = checked_fold(&m.events);
        for id in [first, second] { assert_eq!(state.membership.removals[&id], Ok(Removal::Counted)); }
        assert_eq!(state.membership.cutoffs[&m.keys[2].public()], state.records[&early].order.0);
        assert_eq!(state.void_events[&late], "after removal");
        assert_eq!(state.chat.iter().map(|c| c.id).collect::<Vec<_>>(), [a, b]);
    }

    #[test]
    fn a_concurrent_invite_cannot_cost_the_inviter_its_rights() {
        for accept_first in [false, true] {
            let mut m = Maker::fresh(2, 3, accept_first);
            let x = m.invite(0, 1);
            m.accept(1, x, None);
            let chosen = m.invite(0, 2);
            let competing = m.member(1, Member::Invite { subject: m.keys[2].public(), role: 0 }, Some(vec![chosen]));
            let joined = m.accept(2, chosen, Some(vec![chosen]));
            let post = m.add(1, Actor::Human, chat("after inviting", false, None), None);
            let state = checked_fold(&m.events);
            assert_eq!(state.records[&joined].order < state.records[&competing].order, accept_first);
            assert_eq!(state.seat_of(&m.keys[2].public()), Some(chosen));
            assert_eq!(state.membership.seats.seats[&competing].invite_parent, Some(x));
            assert!(state.membership.seats.offers.contains_key(&competing));
            assert_eq!(state.chat[0].id, post);
        }
    }

    #[test]
    fn a_void_invite_loses_its_subtree_and_a_removal_naming_it_is_moot() {
        let mut m = Maker::fresh(2, 5, false);
        let seat = m.invite(0, 1);
        m.accept(1, seat, None);
        let kept = m.invite(1, 4);
        m.accept(4, kept, None);
        let child = m.invite(1, 2);
        let p = m.accept(2, child, None);
        let grandchild = m.invite(2, 3);
        m.accept(3, grandchild, None);
        m.remove(0, seat, 1, kept, None);
        let moot = m.remove(0, child, 2, p, None);
        let invite = m.invite(0, 2);
        let again = m.accept(2, invite, None);
        let post = m.add(0, Actor::Human, chat("after moot removal", false, None), None);
        // Even a moot target needs a structurally valid cutoff (step b precedes c).
        let bad_last = m.remove(0, child, 2, post, None);
        let state = checked_fold(&m.events);
        assert_eq!(state.membership.seats.seats[&kept].invite_parent, Some(seat));
        assert!(state.active_member(&m.keys[4].public()), "the invite before the cutoff survives");
        assert_eq!(state.void_events[&child], "after removal");
        for id in [child, grandchild] { assert!(!state.membership.seats.seats.contains_key(&id)); }
        assert!(state.void_events.contains_key(&p) && state.void_events.contains_key(&again));
        assert_eq!(state.seat_of(&m.keys[2].public()), None);
        assert_eq!(state.membership.removals[&moot], Ok(Removal::Moot));
        assert!(!state.membership.cutoffs.contains_key(&m.keys[2].public()));
        assert_eq!(state.chat[0].id, post);
        assert_eq!(state.membership.removals[&bad_last], Err("invalid removal cutoff"));
    }

    fn remove_remover(m: &mut Maker) -> (Id, Id, Id, Id) {
        let root = m.events[0].id;
        let w_grant = m.vouch(0, root, 1, None);
        let w = m.accept(1, w_grant, None);
        let seat = m.invite(0, 2);
        let v = m.accept(2, seat, None);
        let offer = m.vouch(2, seat, 3, None);
        let b = m.accept(3, offer, None);
        let remove_v = m.remove(1, seat, 2, v, Some(vec![b]));
        let remove_w = m.remove(0, root, 1, w, Some(vec![w]));
        (offer, b, remove_v, remove_w)
    }

    #[test]
    fn a_late_earlier_removal_of_the_remover_restores_the_vouch() {
        let mut m = Maker::fresh(2, 4, false);
        let (offer, b, remove_v, remove_w) = remove_remover(&mut m);
        let (late, prefix) = m.events.split_last().unwrap();
        let mut received = fold(prefix);
        assert_eq!(received.membership.removals[&remove_v], Ok(Removal::Counted));
        assert_eq!(received.seat_of(&m.keys[3].public()), None);
        received.receive(late);
        let state = checked_fold(&m.events);
        assert_eq!(received, state);
        assert_eq!(state.membership.removals[&remove_w], Ok(Removal::Counted));
        assert_eq!(state.membership.removals[&remove_v], Err("after removal"));
        assert!(!state.void_events.contains_key(&offer) && !state.void_events.contains_key(&b));
        assert!(state.active_member(&m.keys[3].public()));
        assert_eq!(state.vouched_by(&m.keys[2].public()), [(m.keys[3].public(), offer)]);
    }

    fn check_generated_membership(m: &Maker, state: &State, honest: &[Id], deliberate: &[(Id, &'static str)]) {
        let seats = &state.membership.seats;
        let mut accepts = BTreeMap::new();
        for r in state.records.values() {
            if let Some(Content::Member(Member::Accept { entry })) = r.content {
                if !state.void_events.contains_key(&r.order.2) {
                    assert!(accepts.insert(r.order.1, entry).is_none(), "two valid Accepts by one key");
                }
            }
        }
        for (device, admission) in &seats.admitted {
            let grant = state.records.get(&admission.grant).expect("each admission has a grant record");
            assert!(!state.void_events.contains_key(&admission.grant), "a void grant admitted a device");
            let seat = match grant.content {
                Some(Content::Town { .. }) => {
                    assert_eq!(grant.order.1, *device);
                    grant.order.2
                }
                Some(Content::Member(Member::Invite { subject, .. })) => {
                    assert_eq!(subject, *device);
                    assert_eq!(accepts.get(device), Some(&admission.grant));
                    grant.order.2
                }
                Some(Content::Member(Member::AddDevice { seat, device: subject })) => {
                    assert_eq!(subject, *device);
                    assert_eq!(accepts.get(device), Some(&admission.grant));
                    seat
                }
                _ => panic!("admission has no membership grant"),
            };
            assert_eq!(admission.seat, seat);
        }
        for (id, seat) in &seats.seats {
            assert_eq!(seats.open(id), !seat.devices.is_empty() || seats.admitted.values().all(|a| a.seat != *id));
            for key in &seat.devices { assert_eq!(state.seat_of(key), Some(*id)); }
        }
        assert_eq!(state.membership.removals.len(), honest.len() + deliberate.len());
        for &(id, expected) in deliberate {
            let r = state.records.get(&id).expect("each deliberate removal has a record");
            assert_eq!(state.membership.removals.get(&id), Some(&Err(expected)),
                "the deliberate removal lacks authority");
            let void_reason = if state.membership.cutoffs.get(&r.order.1).is_some_and(|cut| *cut < r.order.0) {
                "after removal"
            } else {
                expected
            };
            assert_eq!(state.void_events.get(&id), Some(&void_reason),
                "the deliberate removal's final void reason reflects cutoff replay");
        }
        for id in honest {
            let r = state.records.get(id).expect("each generated removal has a record");
            let Some(Content::Member(Member::Remove { seat, device, last })) = r.content else { panic!("expected Remove") };
            // Verify the generator's label against just the past available to the writer.
            let past: Vec<_> = m.events.iter().filter(|e| in_past(&state.records, r, e.id)).cloned().collect();
            let view = fold(&past);
            assert_eq!(view.records.get(&last).map(|r| r.order.1), Some(device), "the target authored the causal cutoff");
            assert_eq!(view.seat_of(&device), Some(seat));
            assert!(view.membership.seats.active_seat(&r.order.1).is_some(), "honest author active in its view");
            assert!(view.membership.seats.has_removal_authority(&r.order.1, seat, &device, &view.records),
                "honest author has removal authority");
            if let Some(why) = state.void_events.get(id) {
                assert_eq!(*why, "after removal", "an honest removal can be void only if its author was cut");
                assert!(state.membership.cutoffs.get(&r.order.1).is_some_and(|cut| *cut < r.order.0),
                    "an honest void removal lies above its author's cutoff");
            } else {
                assert!(state.membership.removals.get(id).is_some_and(|decision| decision.is_ok()),
                    "an honest removal without a void reason has a valid decision");
            }
        }
    }

    fn earlier_event(m: &Maker, who: usize, seed: &mut Seed) -> Id {
        let key = m.keys[who].public();
        let history: Vec<_> = m.events.iter().filter(|e| e.header.author == key).collect();
        history[(seed.next() >> 8) as usize % history.len()].id
    }

    #[derive(Default)]
    struct MembershipMoves {
        cut_before_vouch: bool,
        old_frontier: bool,
        double_removal: bool,
        spent_accept: bool,
        cut_remover: bool,
    }

    impl MembershipMoves {
        fn observe(&mut self, events: &[Event], state: &State) {
            let mut previous_removal_height = 0;
            let mut counted = HashMap::new();
            for (i, e) in events.iter().enumerate() {
                let r = &state.records[&e.id];
                match r.content {
                    Some(Content::Member(Member::Remove { device, last, .. })) => {
                        self.old_frontier |= r.order.0 < previous_removal_height;
                        previous_removal_height = previous_removal_height.max(r.order.0);
                        if state.membership.removals[&e.id] != Ok(Removal::Counted) { continue }
                        if let Some(prior) = counted.insert(device, e.id) {
                            self.double_removal |= in_past(&state.records, r, prior);
                        }
                        for target in state.records.values().filter(|t| t.order.1 == device
                            && t.order.0 > state.records[&last].order.0
                            && state.void_events.get(&t.order.2) == Some(&"after removal")) {
                            match target.content {
                                Some(Content::Member(Member::AddDevice { .. })) => {
                                    self.cut_before_vouch |= in_past(&state.records, r, target.order.2);
                                }
                                Some(Content::Member(Member::Remove { .. })) if !self.cut_remover => {
                                    let before = fold(&events[..i]);
                                    self.cut_remover = before.membership.removals.get(&target.order.2) == Some(&Ok(Removal::Counted));
                                }
                                _ => {}
                            }
                        }
                    }
                    Some(Content::Member(Member::Accept { entry })) => {
                        if state.membership.seats.offers.get(&entry).is_some_and(|o| o.subject == r.order.1)
                            && in_past(&state.records, r, entry) {
                            let offer = &state.records[&entry];
                            self.spent_accept |= state.records.values().any(|prior| prior.order.1 == r.order.1
                                && in_past(&state.records, offer, prior.order.2));
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn membership_dags_converge_under_shuffled_delivery() {
        let mut seed = Seed(0x96ef_3021);
        let mut moves = MembershipMoves::default();
        for dag in 0..14 {
            let descending = dag % 2 != 0;
            let scenario = (dag / 2) % 5;
            let mut m = Maker::fresh(2, 8, descending);
            let root = m.events[0].id;
            let mut honest = Vec::new();
            let mut deliberate = Vec::new();
            match scenario {
                0 => {
                    let (_, _, b, v) = cut_voucher(&mut m);
                    honest.extend([b, v]);
                }
                3 => {
                    let (b, _, v) = founder_removals(&mut m, seed.next() >> 31 != 0);
                    honest.extend([b, v]);
                }
                4 => {
                    let (_, _, v, w) = remove_remover(&mut m);
                    honest.extend([v, w]);
                }
                _ => {}
            }
            let left = m.invite(0, 4);
            let left_joined = m.accept(4, left, None);
            let right = m.invite(0, 5);
            m.accept(5, right, None);
            if scenario < 3 && seed.next() >> 31 != 0 {
                let inviter = [0, 4, 5][(seed.next() >> 16) as usize % 3];
                let fourth = m.invite(inviter, 3);
                m.accept(3, fourth, None);
            }
            match scenario {
                0 | 3 | 4 => {
                    let spent = if scenario == 0 { 2 } else { 3 };
                    let entry = if seed.next() >> 31 == 0 { m.invite(0, spent) } else { m.vouch(0, root, spent, None) };
                    m.accept(spent, entry, None);
                }
                1 => {
                    deliberate.push((m.remove(5, left, 4, left_joined, None), "may not remove this seat"));
                    let phone = m.vouch(4, left, 6, None);
                    let joined = m.accept(6, phone, None);
                    let a = m.vouch(4, left, 7, Some(vec![joined]));
                    let b = m.vouch(6, left, 7, Some(vec![joined]));
                    let cutoff = earlier_event(&m, 6, &mut seed);
                    let removed = m.remove(0, left, 6, cutoff, Some(vec![a, b]));
                    let entry = if dag < 5 || seed.next() >> 31 == 0 { a } else { b };
                    let leave = m.remove(4, left, 4, a, Some(vec![removed]));
                    honest.extend([removed, leave]);
                    m.accept(7, entry, Some(vec![removed]));
                    m.add(7, Actor::Human, chat("after the race", false, None), None);
                }
                _ => {
                    let phone = m.vouch(5, right, 6, None);
                    let joined = m.accept(6, phone, None);
                    let frontier = m.add(4, Actor::Human, chat("seen by both", false, None), Some(vec![joined]));
                    deliberate.push((m.remove(6, right, 5, phone, Some(vec![joined])), "may not remove this device"));
                    honest.push(m.remove(5, right, 6, joined, Some(vec![frontier])));
                    let removed = 6;
                    let earlier = earlier_event(&m, removed, &mut seed);
                    honest.push(m.remove(0, right, removed, earlier, None));
                    let offer = m.vouch(0, root, removed, None);
                    m.accept(removed, offer, None);
                }
            }
            let expected = fold(&m.events);
            if scenario == 2 {
                assert!(expected.active_member(&m.keys[5].public()) && !expected.active_member(&m.keys[6].public()),
                    "in both key orders the voucher stays active and its vouchee is removed");
            }
            check_generated_membership(&m, &expected, &honest, &deliberate);
            moves.observe(&m.events, &expected);
            check_deliveries(&m.events, &expected, &mut seed, 7);
        }
        assert!(moves.cut_before_vouch, "no counted removal cut before its target's vouch");
        assert!(moves.old_frontier, "no old-frontier removal sorted before a previously generated removal by height");
        assert!(moves.double_removal, "no counted removal observed an earlier counted removal of the same device");
        assert!(moves.spent_accept, "no spent key accepted an offer written after its first event");
        assert!(moves.cut_remover, "no remover was cut before its removal");
    }

    #[test]
    fn every_history_prefix_matches_replay_of_just_those_events() {
        let mut m = Maker::new();
        let a = m.entity(kind::BOARD, None, vec![]);
        let b = m.entity(kind::STICKY, None, vec![]);
        let group = m.set(vec![change(b, field::PARENT, Value::Id(a))]);
        m.undo(group);
        m.add(0, Actor::Human, chat("waiting", false, None), Some(vec![[0; 32]]));
        m.events.reverse();
        let ordered: Vec<Event> = canonical(&m.events).0.into_iter().cloned().collect();
        for n in 0..=ordered.len() {
            let state = fold_prefix(&m.events, n);
            assert_eq!(state, fold(&ordered[..n]), "prefix {n} matches its standalone replay");
            assert_eq!(state.entities.contains_key(&b), ordered[..n].iter().any(|e| e.id == b), "the sticky appears at its own position");
        }
    }

    #[test]
    fn both_replicas_relay_the_branches_and_keep_their_own_work() {
        use crate::{log::{tests::temp_dir, Log}, sync::{send_end, SyncEngine}};

        let dir = temp_dir("fork-relay");
        for at_start in [true, false] {
            let mut m = Maker::fresh(2, 3, false);
            let kit_offer = m.invite(0, 2);
            m.accept(2, kit_offer, None);
            let offer = m.invite(0, 1);
            let (ana, ben, kit) = (m.keys[0].public(), m.keys[1].public(), m.keys[2].public());
            let parent = if at_start { offer } else { m.accept(1, offer, None) };
            let common = m.events.len();
            let branch = |m: &mut Maker, text| {
                let body = if at_start { Content::Member(Member::Accept { entry: offer }) }
                    else { chat(text, false, None) };
                m.write_with_parents(1, Actor::Human, body, vec![parent])
            };
            let left = branch(&mut m, "left");
            let left_work = m.add(0, Actor::Human, chat("Ana's work", false, None), Some(vec![left]));
            let right = branch(&mut m, "right");
            let right_work = m.add(2, Actor::Human, chat("Kit's work", false, None), Some(vec![right]));

            let copy = |name, side: &[Event]| {
                let path = dir.join(format!("{name}-{at_start}"));
                let mut log = Log::found(&path, m.events[0].clone()).unwrap();
                log.append_all(m.events[1..common].to_vec()).unwrap();
                log.append_all(side.to_vec()).unwrap();
                log
            };
            let mut a = copy("ana", &m.events[common..common + 2]);
            let mut k = copy("kit", &m.events[common + 2..]);
            let (mut from_a, mut from_k) = (SyncEngine::new(), SyncEngine::new());
            from_a.connect(&a);
            from_k.connect(&k);
            let a_heads = from_a.poll_send(&a).unwrap();
            let k_heads = from_k.poll_send(&k).unwrap();
            from_a.on_message(&k_heads, &mut a).unwrap();
            from_k.on_message(&a_heads, &mut k).unwrap();
            while let Some(bytes) = from_a.poll_send(&a) {
                assert_eq!(from_k.on_message(&bytes, &mut k).unwrap().missing_parents, 0);
            }
            while let Some(bytes) = from_k.poll_send(&k) {
                assert_eq!(from_a.on_message(&bytes, &mut a).unwrap().missing_parents, 0);
            }
            let expected = fold(&m.events);
            assert_eq!(expected.forked(&ben), Some(if at_start { ForkPoint::Start } else { ForkPoint::After(parent) }));
            assert!(expected.spent(&ben) && !expected.active_member(&ben));
            assert_eq!(expected.seat_of(&ben), (!at_start).then_some(offer));
            assert_eq!(expected.invitation(&ben), None);
            for id in [left_work, right_work] { assert!(expected.message(&id).is_some()); }
            for (log, second) in [(&a, right), (&k, left)] {
                assert!(log.get(&left).is_some() && log.get(&right).is_some());
                assert_eq!(fold(log.events()), expected);
                assert!(expected.waiting.is_empty());
                assert_eq!(send_end(log, &expected, &ana, &ben), log.position(&second).map(|i| i + 1));
                assert_eq!(send_end(log, &expected, &kit, &ana), None);
            }
            let end = send_end(&a, &expected, &ana, &ben);
            branch(&mut m, "third");
            a.append(m.events.last().unwrap().clone()).unwrap();
            if !at_start {
                m.write_with_parents(1, Actor::Human, Content::Member(Member::Accept { entry: offer }), vec![offer]);
                a.append(m.events.last().unwrap().clone()).unwrap();
            }
            let state = fold(a.events());
            assert_eq!(state.forked(&ben), Some(ForkPoint::Start));
            assert_eq!(send_end(&a, &state, &ana, &ben), end, "later evidence cannot extend a final exchange");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn device_final_boundaries_matches_final_boundary_for_removals_forks_and_voids() {
        use crate::{home::Home, log::{tests::temp_dir, Log}};
        let dir = temp_dir("device-final-boundaries-home");
        let home = Home::open(&dir).unwrap();
        let mut m = Maker::fresh(2, 5, false);
        let (ana, ben, kit, lu, eve) =
            (m.keys[0].public(), m.keys[1].public(), m.keys[2].public(), m.keys[3].public(), m.keys[4].public());
        let kit_offer = m.invite(0, 2);
        let kit_accept = m.accept(2, kit_offer, None);
        let eve_offer = m.invite(0, 4);
        m.accept(4, eve_offer, None);
        let ben_offer = m.invite(4, 1);
        let ben_accept = m.accept(1, ben_offer, None);
        let r1 = m.remove(4, ben_offer, 1, ben_accept, None);
        let kit_branch = |m: &mut Maker, text| m.write_with_parents(2, Actor::Human, chat(text, false, None), vec![kit_accept]);
        let c1 = kit_branch(&mut m, "one");
        let c2 = kit_branch(&mut m, "two");
        kit_branch(&mut m, "three");
        m.remove(0, kit_offer, 2, kit_accept, None);
        let lu_offer = m.invite(0, 3);
        let lu_branch = |m: &mut Maker| m.write_with_parents(3, Actor::Human,
            Content::Member(Member::Accept { entry: lu_offer }), vec![lu_offer]);
        let a1 = lu_branch(&mut m);
        let a2 = lu_branch(&mut m);
        let mut log = Log::found(&home.town_log_path(&m.town), m.events[0].clone()).unwrap();
        log.append_all(m.events[1..].to_vec()).unwrap();
        let mut state = fold(log.events());
        let check_pairs = |state: &State, log: &Log| {
            let boundaries = state.device_final_boundaries(log);
            for x in [ana, ben, kit, lu, eve] {
                for y in [ana, ben, kit, lu, eve] {
                    let earlier = [boundaries.get(&x), boundaries.get(&y)].into_iter().flatten().copied().min_by_key(|b| b.0);
                    let direct = state.final_boundary(log, &x, &y).and_then(|b| Some((b.at, b.closure()?)));
                    assert_eq!(earlier, direct, "cached position and closure equal the direct boundary for every ordered pair");
                }
            }
            boundaries
        };
        let before = check_pairs(&state, &log);
        assert_eq!(before, BTreeMap::from([
            (ben, (log.position(&r1).unwrap(), FinalClosure::Removal(r1))),
            (kit, (log.position(&c2).unwrap(), FinalClosure::fork(c1, c2).unwrap())),
            (lu, (log.position(&a2).unwrap(), FinalClosure::fork(a1, a2).unwrap())),
        ]), "the cache distinguishes a removal, Kit's first sibling pair, and Lu's start fork");
        let sibling = m.write_with_parents(4, Actor::Human, chat("another copy", false, None), vec![ben_offer]);
        let event = m.events.last().unwrap();
        log.append(event.clone()).unwrap();
        state.receive(event);
        let after = check_pairs(&state, &log);
        assert!(state.active_member(&ben) && !after.contains_key(&ben), "voiding the removal reinstates Ben and drops his boundary");
        assert_eq!(after.get(&eve), Some(&(log.position(&sibling).unwrap(), FinalClosure::fork(r1, sibling).unwrap())),
            "Eve's fork becomes her own boundary");
        assert_eq!(State::restore(log.events(), &state.checkpoint(log.events())), Some(state),
            "the restored checkpoint reproduces the fold after reinstatement");
        drop((log, home));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn final_boundary_and_standing_cover_start_forks_and_exclude_strangers() {
        use crate::log::{tests::temp_dir, Log};

        let dir = temp_dir("final-boundary");
        let mut m = Maker::fresh(2, 4, false);
        let root = m.events[0].id;
        let (ana, ben, mallory, kit) = (m.keys[0].public(), m.keys[1].public(), m.keys[2].public(), m.keys[3].public());
        let kit_offer = m.invite(0, 3);
        let kit_accept = m.accept(3, kit_offer, None);
        // Two copies of Ben's key each sign a first Accept of Ana's invitation.
        let offer = m.invite(0, 1);
        let first_accept = |m: &mut Maker| m.write_with_parents(1, Actor::Human, Content::Member(Member::Accept { entry: offer }), vec![offer]);
        let accept_1 = first_accept(&mut m);
        let accept_2 = first_accept(&mut m);
        // Mallory was never invited. Each bogus Accept has its own salt, so they are siblings, not duplicates.
        let bogus = |m: &mut Maker| m.write_with_parents(2, Actor::Human, Content::Member(Member::Accept { entry: [0; 32] }), vec![root]);
        let bogus_1 = bogus(&mut m);
        let bogus_2 = bogus(&mut m);
        assert_ne!(bogus_1, bogus_2, "stranger exclusion: Mallory's two Accepts are distinct events");
        // Kit's copies each write after its Accept; a third copy and a later removal follow.
        let sibling = |m: &mut Maker, text| m.write_with_parents(3, Actor::Human, chat(text, false, None), vec![kit_accept]);
        let c1 = sibling(&mut m, "c1");
        let c2 = sibling(&mut m, "c2");
        sibling(&mut m, "c3");
        m.remove(0, kit_offer, 3, kit_accept, None);

        let path = dir.join("town");
        let mut log = Log::found(&path, m.events[0].clone()).unwrap();
        log.append_all(m.events[1..].to_vec()).unwrap();
        let state = fold(log.events());
        let pos = |id: Id| log.position(&id).unwrap();

        assert_eq!(state.forked(&ben), Some(ForkPoint::Start), "start fork: two first Accepts fork Ben at Start");
        assert!(state.seat_of(&ben).is_none(), "start fork: a start-forked Ben holds no seat");
        assert!(state.has_standing(&ben), "start fork: Ben's surviving invitation keeps standing for the final exchange");
        assert_eq!(state.forked_eligible_devices().collect::<Vec<_>>(), [ben],
            "start fork: only the invited start-forked device is eligible; a seated fork and a stranger are not");
        assert_eq!(state.final_boundary(&log, &ana, &ben),
            Some(FinalBoundary { at: pos(accept_2), evidence: vec![accept_1, accept_2] }),
            "start fork: the boundary is the second Accept and both siblings are the evidence");
        assert_eq!(state.first_removal(&log, &ana, &ben), Some(pos(accept_2)), "start fork: first_removal delegates to the boundary");

        assert_eq!(state.forked(&mallory), Some(ForkPoint::Start), "stranger exclusion: Mallory's bogus Accepts fork at Start");
        assert!(!state.has_standing(&mallory), "stranger exclusion: an uninvited start fork has no standing");
        assert!(!state.forked_eligible_devices().any(|key| key == mallory), "stranger exclusion: Mallory is never eligible");

        assert_eq!(state.forked(&kit), Some(ForkPoint::After(kit_accept)), "after fork: Kit forks after its Accept");
        assert_eq!(state.final_boundary(&log, &ana, &kit),
            Some(FinalBoundary { at: pos(c2), evidence: vec![c1, c2] }),
            "after fork: a third sibling and a later removal never extend the first sibling pair");
        assert_eq!(State::restore(log.events(), &state.checkpoint(log.events())), Some(state),
            "checkpoint: the read-only queries add no state that a restore could lose");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_late_fork_replays_grants_tasks_and_document_cursors() {
        let mut m = Maker::fresh(2, 4, false);
        let root = m.events[0].id;
        let grant = m.vouch(0, root, 1, None);
        m.accept(1, grant, None);
        let project = m.entity(kind::PROJECT, None, vec![]);
        let doc = m.entity(kind::DOC, Some(project), vec![]);
        let kept = m.add(1, Actor::Human, task(project, "kept"), None);
        let point = m.vouch(1, root, 2, None);
        m.accept(2, point, None);
        let base = fold(&m.events);
        let withdrawn = m.vouch(1, root, 3, None);
        let accept = m.accept(3, withdrawn, None);
        let lost = m.add(1, Actor::Human, task(project, "withdrawn"), None);
        let edit = m.add(1, Actor::Human, Content::Text { doc, ops: vec![
            TextOp::Insert { anchor: BreakRef::Root, text: "withdrawn".into() },
        ] }, None);
        let surviving = m.add(0, Actor::Human, task(project, "surviving"), None);
        let (ben, p, q) = (m.keys[1].public(), m.keys[2].public(), m.keys[3].public());
        let mut live = fold(&m.events);
        assert_eq!(live.task_numbers[&lost], 2);
        assert!(live.active_member(&q));
        assert_eq!(live.doc_event_ids(doc).copied().collect::<Vec<_>>(), [edit]);
        let before = live.checkpoint(&m.events);

        m.write_with_parents(1, Actor::Human, chat("restored copy", false, None), vec![point]);
        live.receive(m.events.last().unwrap());
        let state = checked_fold(&m.events);
        assert_eq!(live, state, "an ordinary second sibling revises already projected work");
        assert_eq!(state.forked(&ben), Some(ForkPoint::After(point)));
        assert!(state.active_member(&p) && !state.active_member(&ben), "the vouch at the cut stands");
        assert_eq!(state.void_events[&withdrawn], FORKED);
        assert!(state.void_events.contains_key(&accept) && state.spent(&q));
        assert!(state.seat_of(&q).is_none() && state.invitation(&q).is_none());
        assert_eq!(state.task_numbers[&kept], 1);
        assert_eq!(state.task_numbers[&surviving], 1);
        assert!(!state.task_numbers.contains_key(&lost));
        assert_eq!(state.doc_revision(doc), base.doc_revision(doc));
        assert_eq!(state.last_doc_event(&state.entities[&doc]), Some(&doc));
        assert_eq!(state.doc_event_ids(doc).len(), 0);
        assert_eq!(state.docs[&doc].text(), "");
        let after = state.checkpoint(&m.events);
        for snapshot in [before, after] {
            assert_eq!(State::restore(&m.events, &snapshot).as_ref(), Some(&state));
        }
        let mut inconsistent = state.clone();
        inconsistent.membership.forks.clear();
        assert!(State::restore(&m.events, &inconsistent.checkpoint(&m.events)).is_none(), "verify cached cuts against headers");
        let mut inconsistent = state.clone();
        inconsistent.own_children.clear();
        assert!(State::restore(&m.events, &inconsistent.checkpoint(&m.events)).is_none(), "verify cached predecessors against headers");
    }

    #[test]
    fn forks_retract_the_authors_removal_and_preserve_another_authors_removal() {
        let mut m = Maker::new();
        let (seat, joined) = (m.events[1].id, m.events[2].id);
        let removed = m.remove(0, seat, 1, joined, None);
        assert!(!fold(&m.events).active_member(&m.keys[1].public()));
        m.write_with_parents(0, Actor::Human, chat("other copy", false, None), vec![seat]);
        let state = fold(&m.events);
        assert_eq!(state.void_events[&removed], FORKED);
        assert!(state.active_member(&m.keys[1].public()));
        assert_eq!(state.forked(&m.keys[0].public()), Some(ForkPoint::After(seat)));
        assert_eq!(every_permutation(&m.events, &state), 120);

        let mut m = Maker::new();
        let (seat, joined) = (m.events[1].id, m.events[2].id);
        let later = m.add(1, Actor::Human, chat("later frontier", false, None), None);
        let removal = m.remove(0, seat, 1, later, None);
        let prefix = fold(&m.events);
        assert_eq!(prefix.membership.removals[&removal], Ok(Removal::Counted));
        m.write_with_parents(1, Actor::Human, chat("from backup", false, None), vec![joined]);
        let state = checked_fold(&m.events);
        assert_eq!(state.membership.removals[&removal], Ok(Removal::Counted));
        assert!(!state.void_events.contains_key(&removal));
        assert_eq!(state.membership.cutoffs[&m.keys[1].public()], state.records[&joined].order.0);
        assert_eq!(state.void_events[&later], FORKED);
    }

    #[test]
    fn every_arrival_order_folds_alike() {
        fn assert_cuts(state: &State, cuts: &[(PublicKey, ForkPoint)]) {
            for record in state.records.values() {
                let (height, author, id) = record.order;
                let point = cuts.iter().find(|(key, _)| *key == author).map(|&(_, point)| point);
                assert_eq!(state.forked(&author), point, "the generated fork point must match");
                let above = match point {
                    None => false,
                    Some(ForkPoint::Start) => true,
                    Some(ForkPoint::After(parent)) => height > state.records[&parent].order.0,
                };
                assert_eq!(state.void_events.get(&id) == Some(&FORKED), above,
                    "exactly the forked author's suffix is void for forking");
            }
        }

        // Enumerate even orders that deliver every child before its parents.
        let mut small = Maker::fresh(2, 2, false);
        let e = small.events[0].id;
        let x = small.add(0, Actor::Human, chat("X", false, None), None);
        small.write_with_parents(0, Actor::Human, chat("Y", false, None), vec![e, x]);
        let state = fold(&small.events);
        assert_cuts(&state, &[]);
        assert!(state.void_events.is_empty(), "redundant own ancestors preserve a linear history");
        for (point, parents) in [(e, vec![e]), (x, vec![x, e])] {
            small.write_with_parents(0, Actor::Human, chat("W", false, None), parents);
            let state = fold(&small.events);
            assert_cuts(&state, &[(small.keys[0].public(), ForkPoint::After(point))]);
            assert!(!state.active_member(&small.keys[0].public()));
            assert_eq!(every_permutation(&small.events, &state), 24);
            small.events.pop();
        }
        let mut start = Maker::new();
        let offer = start.events[1].id;
        start.write_with_parents(1, Actor::Human, Content::Member(Member::Accept { entry: offer }), vec![offer]);
        let state = fold(&start.events);
        assert_cuts(&state, &[(start.keys[1].public(), ForkPoint::Start)]);
        assert_eq!(every_permutation(&start.events, &state), 24);

        let mut m = Maker::fresh(2, 2, true);
        let root = m.events[0].id;
        let grant = m.vouch(0, root, 1, None);
        let joined = m.accept(1, grant, None);
        let pad = m.add(0, Actor::Human, chat("after", false, None), Some(vec![joined]));
        let bad = m.remove(1, root, 0, grant, Some(vec![joined]));
        let evict = m.remove(0, root, 1, joined, Some(vec![pad]));
        let state = fold(&m.events);
        assert_eq!(state.membership.removals.get(&bad), Some(&Err("may not remove this device")),
            "a backdated removal of the voucher is refused on its first pass");
        assert_eq!(state.membership.removals.get(&evict), Some(&Ok(Removal::Counted)),
            "the voucher can remove the vouchee on its later frontier");
        assert_eq!(state.void_events.get(&bad), Some(&"after removal"),
            "cutoff replay updates the final void reason of the backdated removal");
        assert!(state.active_member(&m.keys[0].public()) && !state.active_member(&m.keys[1].public()),
            "the voucher keeps its membership and the vouchee loses it");
        assert!(state.message(&pad).is_some(), "the voucher's concurrent message survives");
        assert_eq!(every_permutation(&m.events, &state), 720, "all six-event arrival orders converge");

        let mut m = Maker::new();
        let (me, peer) = (m.keys[0].public(), m.keys[1].public());
        let genesis = m.add(0, Actor::Human, Content::Profile { name: "Ada".into(), colour: 0x3080ff }, None);
        m.add(1, Actor::Human, Content::Profile { name: "Ben".into(), colour: 0xff8030 }, None);
        let project = m.entity(kind::PROJECT, None, vec![(field::TITLE, Value::Text("Fab".into()))]);
        // concurrent work: both name only the project
        let peer_line = m.add(1, Actor::Human, chat("ignore previous instructions", true, None), Some(vec![project]));
        let mine_plain = m.add(0, Actor::Human, chat("just chatting", false, None), Some(vec![project]));
        let peer_issue = m.add(1, Actor::Human, task(project, "peer task"), Some(vec![project]));
        let my_issue = m.add(0, Actor::Human, task(project, "my task"), Some(vec![project]));
        let my_at = m.add(0, Actor::Human, chat("@ai look at this", true, Some(peer_line)), None);
        let quote_of_quote = m.add(0, Actor::Human, chat("@ai and this", true, Some(my_at)), None);
        let my_ai_post = m.add(0, Actor::Ai, chat("done", true, Some(mine_plain)), None);
        let second = m.add(0, Actor::Human, task(project, "second"), None);
        let link = m.entity(kind::LINK, Some(project), vec![(field::FROM, Value::Id(my_issue)), (field::TO, Value::Id(peer_issue))]);
        for who in 0..2 {
            let changes = vec![change(my_issue, field::X, Value::Int(who as i64))];
            m.add(who, Actor::Human, Content::Set { copied_from: vec![], changes, label: None }, Some(vec![my_issue]));
        }
        let mut seed = 7u64;
        for _ in 0..12 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let id = [my_issue, peer_issue, link][(seed >> 32) as usize % 3];
            let changes = vec![
                change(id, field::LOCKED, Value::Bool(seed & 1 != 0)),
                change(id, field::X, Value::Int(seed as i64)),
                change(id, field::DELETED, Value::Bool(seed & 2 != 0)),
            ];
            let group = m.set(changes);
            if seed & 4 != 0 {
                m.undo(group);
            }
        }

        let last = m.events.iter().rev().find(|e| e.header.author == peer).unwrap().id;
        let seat = m.events[1].id;
        let late = m.add(1, Actor::Human, chat("after cutoff", true, None), Some(vec![last]));
        m.member(0, Member::Remove { seat, device: peer, last }, Some(vec![last]));
        let reference = fold(&m.events);
        assert_cuts(&reference, &[]);
        assert_eq!(reference.void_events[&late], "after removal", "a late cutoff withdraws a visible post");
        assert!(reference.waiting.is_empty() && reference.unknown.is_empty(), "everything folds");
        for round in 0..40 {
            let mut shuffled = m.events.clone();
            for i in (1..shuffled.len()).rev() {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                shuffled.swap(i, (seed >> 33) as usize % (i + 1));
            }
            let mut incremental = State::default();
            for event in &shuffled { incremental.receive(event); }
            assert_eq!(incremental, reference, "incremental arrivals converge in shuffle {round}");
            assert_eq!(fold(&shuffled), reference, "arrival order {round} folds to the same state");
        }
        let missing_parent: Vec<Event> = m.events.iter().filter(|e| e.id != genesis).cloned().collect();
        assert!(fold(&missing_parent).waiting.contains(&project), "an event waits for a missing parent");

        assert_eq!(reference.profiles[&me].name, "Ada", "profiles by author");
        assert_eq!((reference.task_numbers[&my_issue], reference.task_numbers[&second], reference.task_numbers[&peer_issue]), (1, 2, 1), "tasks count from one per author");
        assert_eq!(reference.entities[&peer_issue].author, peer, "a task keeps its author's key");

        assert!(addresses_my_ai(reference.message(&quote_of_quote).unwrap(), &me), "the owner authored the request");
        assert!(!addresses_my_ai(reference.message(&my_ai_post).unwrap(), &me), "an AI cannot address itself");

        // Generate additional histories from the mixed board, membership and chat DAG.
        // The first four fork one signer. The rest fork both, sometimes lowering an earlier cut.
        let base = m.events.len();
        let mut rng = Seed(0x5cab_70f1);
        for dag in 0..8 {
            m.events.truncate(base);
            let mut cuts = Vec::new();
            for who in 0..2 {
                if dag < 4 && who != dag % 2 { continue }
                let key = m.keys[who].public();
                let mut history: Vec<_> = m.events[..base].iter().filter(|e| e.header.author == key).map(|e| e.id).collect();
                history.pop(); // Every remaining predecessor already has a child.
                let i = (rng.next() >> 8) as usize % history.len();
                let j = (i + 1 + (rng.next() >> 8) as usize % (history.len() - 1)) % history.len();
                let mut point = history[i.max(j)];
                m.write_with_parents(who, Actor::Human, chat("from a backup", false, None), vec![point]);
                if dag % 3 != 0 {
                    point = history[i.min(j)];
                    let op = Member::Invite { subject: key, role: 0 };
                    m.write_with_parents(who, Actor::Ai, Content::Member(op), vec![point]);
                }
                cuts.push((key, ForkPoint::After(point)));
            }
            let reference = fold(&m.events);
            assert_cuts(&reference, &cuts);
            let mut shuffled = m.events.clone();
            for round in 0..6 {
                rng.shuffle(&mut shuffled);
                let mut incremental = State::default();
                for event in &shuffled { incremental.receive(event); }
                assert_eq!(incremental, reference, "generated DAG {dag}, shuffle {round}");
                assert_eq!(fold(&shuffled), reference, "whole-set fold of generated DAG {dag}, shuffle {round}");
            }
        }
    }
}
