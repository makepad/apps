//! Read models for the workspace. Draws reuse these across frames.

use makepad_town_core::key::PublicKey;

use crate::ai_trace::{Output, Phase, Trace, Traces};
use std::collections::{BTreeMap, HashMap, HashSet};
use crate::link_worker::{ConnectView, DeviceLink};
pub use crate::ui::chat::ClipPlayback;
use crate::model::{peer_label, task_column, Model, PeerUpdate, SeatSummary};
use makepad_town_core::blob::{self, lane::Status};
use makepad_town_core::entity::{field, kind, Entity};
use makepad_town_core::event::{Actor, Id, Key, TownId, Service, SessionId, SessionMode, Value};
use makepad_town_core::fold::{addresses_my_ai, project_entities, ChatMsg, Entry, Outcome, Seen, SessionSummary};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const SEAT_SLOTS: usize = 8;

/// Theme palette slot index (`0..SEAT_SLOTS`) for a peer seat.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SeatTone(pub u8);

/// Deterministic preferred slot before resolving collisions with earlier seats.
pub fn preferred_seat_slot(seat: Id, profile_colour: u32) -> u8 {
    if profile_colour == 0 {
        let hash = seat.iter().fold(0x811c9dc5u32, |h, &b| (h ^ u32::from(b)).wrapping_mul(0x01000193));
        return (hash % SEAT_SLOTS as u32) as u8;
    }
    if profile_colour <= SEAT_SLOTS as u32 { return (profile_colour - 1) as u8; }

    let [_, r, g, b] = profile_colour.to_be_bytes();
    if r.max(g).max(b) - r.min(g).min(b) < 16 { return (profile_colour % SEAT_SLOTS as u32) as u8; }
    let hue = makepad_widgets::Vec4f::from_u32((profile_colour << 8) | 0xff).to_hsva().x * 360.;
    let hues = [28., 170., 269., 45., 335., 124., 232., 2.];
    let distance = |angle: f32| { let gap = (hue - angle).abs(); gap.min(360. - gap) };
    (0..SEAT_SLOTS).min_by(|&a, &b| distance(hues[a]).total_cmp(&distance(hues[b]))).unwrap() as u8
}

pub fn seat_tones(model: &Model) -> BTreeMap<Id, SeatTone> {
    tones_in_order(model, &model.seats_in_order())
}

fn tones_in_order(model: &Model, seats: &[SeatSummary]) -> BTreeMap<Id, SeatTone> {
    let mut tones = BTreeMap::new();
    let mut used = [0u16; SEAT_SLOTS];
    for seat in seats.iter().filter(|s| !s.is_mine) {
        let colour = seat.devices.iter().filter_map(|key| model.state.profiles.get(key))
            .filter(|p| p.colour != 0).max_by_key(|p| model.state.order_of(&p.writer)).map_or(0, |p| p.colour);
        let preferred = preferred_seat_slot(seat.seat, colour) as usize;
        let slot = (0..SEAT_SLOTS).map(|offset| (preferred + offset) % SEAT_SLOTS)
            .min_by_key(|&slot| used[slot]).unwrap();
        // When even the least-used slot is full, subtract the same count from all slots.
        if used[slot] == u16::MAX { used.fill(0); }
        used[slot] += 1;
        tones.insert(seat.seat, SeatTone(slot as u8));
    }
    tones
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Seat {
    You,
    YourAi,
    Peer,
    PeerAi,
}

impl Seat {
    fn from_author(mine: bool, actor: Actor) -> Self {
        match (mine, actor) {
            (true, Actor::Human) => Self::You,
            (true, Actor::Ai) => Self::YourAi,
            (false, Actor::Human) => Self::Peer,
            (false, Actor::Ai) => Self::PeerAi,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::You => "You",
            Self::YourAi => "Your AI",
            Self::Peer => "Peer",
            Self::PeerAi => "Peer’s AI",
        }
    }
}

pub struct Card {
    pub id: Id,
    /// Proposed by a hub service; a person accepts or dismisses it.
    pub proposed: bool,
    pub author_name: Arc<str>,
    pub number: String,
    pub title: String,
    pub seat: Seat,
    pub tone: Option<SeatTone>,
}

pub struct DocumentRow {
    pub id: Id,
    pub title: String,
}

pub struct Project {
    pub id: Id,
    pub name: String,
    pub goal: String,
    pub columns: [Vec<Card>; 4],
    /// The proposed tasks, in board order.
    pub proposed: Vec<Id>,
    pub authors: Vec<Seat>,
    pub docs: Vec<DocumentRow>,
}

#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq)]
pub struct TraceFold {
    pub line: Id,
    pub trace: usize,
    /// Finishing work collapses its history even if earlier live output was expanded.
    pub history: bool,
}

pub struct TraceRow {
    pub phase: Phase,
    pub headline: String,
    pub output: Vec<Arc<Output>>,
    pub fold_count: usize,
    pub expanded: bool,
    pub spin_until: Option<u64>,
}

impl TraceRow {
    pub fn read(trace: &Trace, now: u64, clock: &LocalClock, expanded: bool) -> Self {
        let phase = trace.phase(now);
        let total = trace.output.len();
        let fold_count = if phase.finished() { total } else { total.saturating_sub(2) };
        let start = if expanded { 0 } else { fold_count };
        Self { phase, headline: trace.headline(now, clock), output: trace.output[start..].to_vec(),
            fold_count, expanded: expanded && fold_count != 0, spin_until: trace.spin_until(now) }
    }

    pub fn fold_label(&self) -> String {
        if self.phase.finished() { format!("what it did ({})", self.fold_count) }
        else { format!("{} earlier", self.fold_count) }
    }
}

fn message_name(model: &Model, m: &ChatMsg, author: impl FnOnce() -> String) -> String {
    if m.actor == Actor::Ai {
        return m.author_agent.map_or_else(author, |id| model.agent_display_label(id))
    }
    // A verified recipient names its owner's signed addition, also after the AI leaves.
    let owner = m.to_agent.and_then(|id| model.event(&id)).map(|e| e.header.author)
        .or_else(|| (m.actor == Actor::Human && m.chat.to_ai).then_some(m.author));
    owner.map_or_else(author, |owner| model.ai_request_label_for(&m.author, m.to_agent, &owner))
}

pub struct Quote {
    pub id: Id,
    pub name: String,
    pub text: String,
}

impl Quote {
    pub fn read(model: &Model, id: Id) -> Self {
        if let Some(m) = model.state.message(&id).filter(|_| !model.state.message_retracted(&id)) {
            let mut line = m.chat.text.chars().take_while(|c| !matches!(c, '\r' | '\n' | '\u{2028}' | '\u{2029}'))
                .skip_while(|c| c.is_whitespace());
            let mut text: String = line.by_ref().take(80).collect();
            text.truncate(text.trim_end().len());
            if line.any(|c| !c.is_whitespace()) { text.push('…'); }
            Self { id, name: message_name(model, m, || model.author_label(&m.author, m.actor)), text }
        } else {
            Self { id, name: String::new(), text: "Removed message".into() }
        }
    }
}

impl std::fmt::Display for Quote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !self.name.is_empty() { write!(f, "{}: ", self.name)?; }
        f.write_str(&self.text)
    }
}

pub struct Message {
    pub id: Id,
    pub quote: Option<Quote>,
    pub traces: Vec<TraceRow>,
    pub seat: Seat,
    pub tone: Option<SeatTone>,
    pub name: String,
    pub time: String,
    pub text: String,
    /// My own line to my AI: drawn with "sent to your AI".
    pub addressed: bool,
    /// My own line to my AI while no AI of mine is here: drawn with "No AI here yet · Add AI".
    pub unheard: bool,
    /// Any person's line to their own AI: the "@ai" marker keeps its ink on every computer.
    pub mention: bool,
    /// Another seat's line that `@`-mentions your seat, replies to your seat, or awaits your AI's approval.
    pub mentions_you: bool,
    /// This person may delete the line now (their own, in a town they still write to).
    pub deletable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NoticeKey {
    /// Timeline admission event id (`Entry::Member(id)`).
    Member(Id),
    /// First unknown event id in a coalesced unknown-event run (`show_unknown`).
    Unknown(Id),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberNotice {
    pub key: NoticeKey,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DayDivider {
    pub day: i64,
    /// Zero unless peer clock skew puts this day on the timeline more than once.
    pub occurrence: u32,
    pub label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChatRowKey {
    Day(i64, u32),
    Message(Id),
    Call(TranscriptId),
    Clip(ClipId),
    Notice(NoticeKey),
}

pub enum ChatRow {
    Day(DayDivider),
    Message(Message),
    Call(CallLine),
    Clip(Arc<ClipLine>),
    Member(MemberNotice),
}

impl ChatRow {
    pub fn key(&self) -> ChatRowKey {
        match self {
            Self::Day(d) => ChatRowKey::Day(d.day, d.occurrence),
            Self::Message(m) => ChatRowKey::Message(m.id),
            Self::Call(c) => ChatRowKey::Call(c.id),
            Self::Clip(c) => ChatRowKey::Clip(c.id),
            Self::Member(n) => ChatRowKey::Notice(n.key),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ClipId(pub TownId, pub Id);

#[derive(PartialEq)]
pub struct ClipLine {
    pub id: ClipId,
    pub title: String,
    pub media: Option<ClipMedia>,
}

#[derive(PartialEq)]
pub struct ClipMedia {
    pub is_picture: bool,
    pub blob: Id,
    pub length: String,
    pub size: String,
    pub poster: Option<PathBuf>,
    pub height: f64,
    pub state: String,
    pub get: bool,
    pub playable: Option<PathBuf>,
}

fn clip_length(ms: u64) -> String {
    let s = ms / 1000;
    if s < 3600 { format!("{}:{:02}", s / 60, s % 60) }
    else { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) }
}

fn clip_height(w: u64, h: u64) -> f64 {
    match (u32::try_from(w), u32::try_from(h)) {
        // A peer's dimensions only shape the box: keep its aspect between 4:1 and 1:2.
        (Ok(w), Ok(h)) if w > 0 && h > 0 => 240.0 * (f64::from(h) / f64::from(w)).clamp(0.25, 2.0),
        _ => 135.0,
    }
}

fn file_size(bytes: u64) -> (String, &'static str) {
    if bytes < 1_000_000 { (format!("{:.0}", bytes as f64 / 1000.0), "KB") }
    else if bytes < 10_000_000 { (format!("{:.1}", bytes as f64 / 1_000_000.0), "MB") }
    else { (format!("{:.0}", bytes as f64 / 1_000_000.0), "MB") }
}

fn clip_status(here: bool, wanted: bool, transfer: Option<&Status>, size: &str, owner: &PublicKey,
    name: impl Fn(&PublicKey, bool) -> String) -> (String, bool) {
    if here { return ("On this computer".into(), false) }
    match transfer {
        Some(Status::Bad { carrier, .. }) => (format!("The copy from {} was damaged", name(carrier, true)), true),
        Some(Status::Progress { have, size, from, .. }) => {
            let (have, unit) = file_size(*have);
            let (total, total_unit) = file_size(*size);
            let have = if unit == total_unit { have } else { format!("{have} {unit}") };
            (format!("{have} of {total} {total_unit} from {}", name(from, false)), false)
        }
        _ if wanted || transfer.is_some() => (format!("Waiting for {} · {size}", name(owner, true)), false),
        _ => (format!("On {}", name(owner, true)), true),
    }
}

impl ClipLine {
    fn read(model: &Model, entity: &Entity, mut watch: impl FnMut(Id, bool)) -> Self {
        let town = model.open_id();
        let id = ClipId(town, entity.id);
        if entity.flag(field::DELETED) {
            let by = model.event(&entity.fields[&field::DELETED].writer)
                .map(|e| model.author_label(&e.header.author, e.header.actor)).unwrap_or_else(|| "Peer".into());
            return Self { id, title: format!("{by} removed a clip"), media: None }
        }
        let title = entity.text(field::TITLE).to_owned();
        let Some(Value::Blob { size, root }) = entity.value(field::BLOB) else { return Self { id, title, media: None } };
        let blob = blob::id(*size, root);
        let (amount, unit) = file_size(*size);
        let size = format!("{amount} {unit}");
        let number = |key: Key| match entity.value(key) { Some(Value::Int(n)) => u64::try_from(*n).unwrap_or(0), _ => 0 };
        let height = clip_height(number(field::W), number(field::H));
        let held = model.complete_blobs(&town);
        let dir = model.home.blobs_dir().join(makepad_town_core::hex(&town));
        let mut path = |id: Id| {
            let here = held.is_some_and(|set| set.contains(&id));
            watch(id, here);
            here.then(|| blob::data_path(&dir, &id))
        };
        let playable = path(blob);
        let poster = match entity.value(field::POSTER) {
            Some(Value::Blob { size, root }) => path(blob::id(*size, root)), _ => None,
        };
        let (state, get) = clip_status(playable.is_some(), model.file_wanted(town, blob), model.file_transfer(town, blob),
            &size, &entity.author, |device, computer| {
                if model.state.seat_of(device).is_some_and(|s| Some(s) == model.state.seat_of(&model.me())) {
                    return "your other computer".into()
                }
                let name = model.author_label(device, Actor::Human);
                if computer { format!("{name}'s computer") } else { name }
            });
        Self { id, title, media: Some(ClipMedia { blob, is_picture: entity.text(field::MEDIA).starts_with("image/"), length: clip_length(number(field::DURATION)), size,
            poster, height, state, get, playable }) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TranscriptId(pub TownId, pub SessionId);

pub struct CallLine {
    pub id: TranscriptId,
    pub words: String,
    pub time: String,
    pub call_back: Option<CallBack>,
    pub sent: String,
    pub jobs: Vec<JobLine>,
    pub count: usize,
    pub transcript: Option<String>,
}

pub struct JobLine {
    pub id: makepad_town_core::event::JobId,
    pub words: String,
}

impl JobLine {
    fn read(job: &makepad_town_core::fold::Job, requester: &str, provider: &str, present: bool, ahead: usize) -> Self {
        use makepad_town_core::{event::Failure, fold::JobState};
        let tasks = job.service == Service::Tasks;
        let words = match &job.state {
            JobState::Asked | JobState::Working if !present => format!("{provider} is off; it continues when it's back"),
            JobState::Asked => {
                let queue = if ahead > 0 { format!(", {ahead} ahead") } else { String::new() };
                let what = if tasks { "to update tasks" } else { "for a summary" };
                format!("{requester} asked {what} · {provider}{queue}")
            }
            JobState::Working if tasks => format!("{provider} is updating tasks…"),
            JobState::Working => format!("{provider} is summarising…"),
            JobState::Done(text) if tasks => format!("Tasks by {provider}: {text}"),
            JobState::Done(text) => format!("Summary by {provider}\n{text}"),
            JobState::Failed(Failure::ProjectGone) => "The project is gone; nothing was proposed.".to_owned(),
            JobState::Failed(why) => {
                let reason = match why {
                    Failure::NotOffered => "not offered any more",
                    Failure::NoTranscript => "no transcript or chat lines",
                    Failure::NoModel(reason) => reason,
                    Failure::Interrupted => "stopped before it finished",
                    Failure::ProjectGone => "the project is gone",
                };
                format!("{provider} couldn't {}: {reason}", if tasks { "update tasks" } else { "summarise" })
            }
        };
        Self { id: job.job, words }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Summarise(pub TranscriptId);

/// The press on a proposed task's Accept or Dismiss, or on the bar's Accept all or Dismiss all
/// (`task` is `None`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DecideProposed { pub project: Id, pub task: Option<Id>, pub accept: bool }

/// The press on a call row's Update tasks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UpdateTasks(pub TranscriptId);

pub struct CallBack {
    pub seat: Id,
    pub peer: String,
    pub enabled: bool,
}

fn call_words(summary: &SessionSummary, me: Option<Id>, opener: &str, name: impl Fn(&Id) -> String) -> (String, bool) {
    let targets = |ids: &[Id]| {
        let names = ids.iter().map(&name).collect::<Vec<_>>().join(", ");
        if names.is_empty() { opener.to_owned() + " called" } else { format!("{opener} called {names}") }
    };
    let duration = |secs: Option<u32>| secs.map(|s| format!(" · {}", crate::call::length(s.into()))).unwrap_or_default();
    let missed = || (format!("Missed call from {opener}"), true);
    let words = match &summary.outcome {
        Outcome::Answered { seats, secs } => {
            if summary.viewer == Seen::Rung && me.is_none_or(|s| !seats.contains(&s)) { return missed() }
            let call = if summary.viewer == Seen::Other { targets(seats) } else { format!("{opener} called") };
            call + &duration(*secs)
        }
        Outcome::Declined { seats, busy } => {
            if summary.viewer == Seen::Opener {
                let who = seats.iter().map(&name).collect::<Vec<_>>().join(", ");
                let verb = if *busy && seats.len() > 1 { "were busy" } else if *busy { "was busy" } else { "declined" };
                format!("{who} {verb}")
            } else if me.is_some_and(|s| seats.contains(&s)) { "You declined".into() }
            else if summary.viewer == Seen::Rung { return missed() }
            else { format!("{} · declined", targets(seats)) }
        }
        Outcome::Unanswered => match summary.viewer {
            Seen::Opener => "No answer".into(), Seen::Rung => return missed(),
            Seen::Other => format!("{} · no answer", targets(&summary.rung)),
        },
        Outcome::Pending if summary.viewer == Seen::Rung => format!("{opener} called"),
        Outcome::Pending => targets(&summary.rung),
    };
    (words, false)
}

fn chat_calls(model: &Model, my_seat: Option<Id>, name: impl Fn(&Id) -> String)
    -> std::collections::HashMap<SessionId, (usize, String, Option<CallBack>)> {
    let mut calls = std::collections::HashMap::new();
    if model.state.sessions.is_empty() { return calls }
    // Walk backwards so a later connection resolves older callbacks before chat rows get indices.
    let mut connected = HashSet::new();
    for entry in model.state.timeline.iter().rev() {
        let Entry::Session(id) = entry else { continue };
        let Some(summary) = model.state.session_summary(id, my_seat) else { continue };
        if !matches!(summary.mode, SessionMode::Call { .. }) { continue }
        if let Outcome::Answered { seats, .. } = &summary.outcome {
            // Open counts as a join. Only seats that participated together clear their callbacks.
            if summary.viewer == Seen::Opener || my_seat.is_some_and(|me| seats.contains(&me)) {
                connected.extend(summary.opener_seat.iter().chain(seats).copied());
            }
        }
        let opener = summary.opener_seat.as_ref().map(&name).unwrap_or_else(|| model.author_label(&summary.opener, Actor::Human));
        let (words, missed) = call_words(&summary, my_seat, &opener, &name);
        let callback = summary.opener_seat.filter(|seat| missed && !connected.contains(seat));
        if callback.is_some() || summary.lines > 0 {
            let call_back = callback.map(|seat| CallBack { seat, peer: opener, enabled: model.state.active_members()
                .any(|key| model.state.seat_of(&key) == Some(seat) && model.synced_with(&key)) });
            calls.insert(*id, (summary.lines, words, call_back));
        }
    }
    calls
}

pub struct RemovedProject {
    pub project: Id,
    pub event: Id,
    pub title: String,
}

struct ClipBlob {
    here: bool,
    rows: Vec<usize>,
}

fn next_rows_rev() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let rev = NEXT.fetch_add(1, Ordering::Relaxed);
    if rev == 0 { NEXT.fetch_add(1, Ordering::Relaxed) } else { rev }
}

#[derive(Default)]
pub struct Workspace {
    pub(crate) rows_rev: u64,
    pub(crate) day_today: Option<i64>,
    index: std::collections::HashMap<Id, usize>,
    chat_index: std::collections::HashMap<Id, usize>,
    pub writable: bool,
    pub removable: Option<Id>,
    pub removed: Option<RemovedProject>,
    pub projects: Vec<Project>,
    pub current: Option<Id>,
    pub rows: Vec<ChatRow>,
    pub summarisable: HashSet<SessionId>,
    /// Calls whose row offers Update tasks: a project is open and a Tasks provider exists.
    pub updatable: HashSet<SessionId>,
    clip_blobs: std::collections::HashMap<Id, ClipBlob>,
    last_shown: Option<Id>,
    pub listening: bool,
    pub connect: AiCommands,
}

#[derive(Default)]
pub struct AiCommands {
    pub rows: Vec<AgentRow>,
    pub requests: std::collections::BTreeMap<usize, ApprovalRow>,
}

pub struct ApprovalRow {
    pub cue: String,
    pub allow: AgentControl,
    pub deny: AgentControl,
    pub label: String,
}

impl ApprovalRow {
    /// Message id of the chat line awaiting this approval.
    pub fn line(&self) -> Id {
        let AgentControl::Decide { line, .. } = self.allow;
        line
    }
}

impl AiCommands {
    /// Earliest pending AI approval in timeline order: `(row_index, message_id, row)`.
    pub fn first_pending(&self) -> Option<(usize, Id, &ApprovalRow)> {
        self.requests.first_key_value().map(|(&row, req)| (row, req.line(), req))
    }
}

pub struct AgentRow {
    pub id: Id,
    pub tone: Option<SeatTone>,
    pub name: String,
    pub owner: bool,
    pub status: String,
    pub away: bool,
    pub live: bool,
    pub working: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AgentControl {
    Decide { town: Id, agent: Id, line: Id, allow: bool },
}

impl AgentControl {
    pub fn clicked(action: &makepad_widgets::WidgetAction) -> Option<Self> {
        use makepad_widgets::ButtonAction;
        if !matches!(action.action.downcast_ref::<ButtonAction>(), Some(ButtonAction::Clicked(_))) { return None }
        action.data.as_ref()?.downcast_ref::<Self>().copied()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RemoteBinary {
    Helper(PathBuf),
    Town(PathBuf),
}

pub fn remote_binary() -> Option<RemoteBinary> {
    find_remote_binary(std::env::current_exe().ok(), std::env::var_os("RELAX_LOADER").map(Into::into),
        &std::env::var_os("PATH").unwrap_or_default(), installed_executable)
}

fn installed_executable(path: &Path) -> Option<PathBuf> {
    let file = std::fs::metadata(path).ok()?;
    if !file.is_file() { return None }
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt; if file.permissions().mode() & 0o111 == 0 { return None } }
    std::path::absolute(path).ok()
}

fn find_remote_binary(work: Option<PathBuf>, loader: Option<PathBuf>, path: &std::ffi::OsStr,
    resolve: impl Fn(&Path) -> Option<PathBuf>) -> Option<RemoteBinary> {
    let name = format!("makepad-remote{}", std::env::consts::EXE_SUFFIX);
    // A JIT run-link and its siblings relaunch live programs. Only the loader's
    // installation and PATH can provide a shell client for that run.
    let sibling = loader.as_ref().or(work.as_ref()).filter(|p| p.is_absolute()).map(|p| p.with_file_name(&name));
    if let Some(helper) = sibling.into_iter().chain(std::env::split_paths(path).map(|p| p.join(&name)))
        .find_map(|p| resolve(&p)) {
        return Some(RemoteBinary::Helper(helper))
    }
    if loader.is_some() { None } else { work.map(RemoteBinary::Town) }
}

/// Quote local paths and JSON as shell arguments. A town title or a peer label never enters a command.
pub fn tool_command(binary: &RemoteBinary, endpoint: &Path, reconnect: bool, tool: &str,
    arguments: makepad_strict_json::Value) -> Result<String, String> {
    fn quote(s: &str) -> String { format!("'{}'", s.replace('\'', "'\\''")) }
    let path = |p: &std::path::Path| p.to_str().filter(|_| p.is_absolute()).map(quote)
        .ok_or_else(|| "Town needs absolute, UTF-8 paths for the copy command.".to_owned());
    let mode = if reconnect { " --reconnect" } else { "" };
    let client = match binary {
        RemoteBinary::Helper(exe) => path(exe)?,
        RemoteBinary::Town(exe) => format!("{} remote", path(exe)?),
    };
    Ok(format!("{client} app {}{mode} call {tool} {}", path(endpoint)?, quote(&arguments.to_json())))
}

impl Workspace {
    /// Refresh only the recordings affected by a link update or by a new store snapshot.
    pub fn refresh_clips(&mut self, model: &Model, transfers: impl IntoIterator<Item = (TownId, Id)>, check_holdings: bool) -> bool {
        let open = model.open_id();
        let mut changed = std::collections::BTreeSet::<usize>::new();
        for (town, id) in transfers {
            if town == open {
                if let Some(blob) = self.clip_blobs.get(&id) { changed.extend(&blob.rows); }
            }
        }
        if check_holdings {
            let held = model.complete_blobs(&open);
            for (id, blob) in &mut self.clip_blobs {
                let here = held.is_some_and(|ids| ids.contains(id));
                if blob.here != here {
                    blob.here = here;
                    changed.extend(&blob.rows);
                }
            }
        }
        let mut redraw = false;
        for index in changed {
            if let ChatRow::Clip(row) = &mut self.rows[index] {
                if let Some(entity) = model.state.entities.get(&row.id.1) {
                    let next = ClipLine::read(model, entity, |_, _| {});
                    if **row != next {
                        *row = Arc::new(next);
                        redraw = true;
                    }
                }
            }
        }
        redraw
    }

    pub(crate) fn still_playable(&self, clip: &ClipLine) -> bool {
        let Some(media) = &clip.media else { return false };
        self.clip_blobs.get(&media.blob).is_some_and(|blob| blob.here && blob.rows.iter().any(|&i|
            matches!(&self.rows[i], ChatRow::Clip(row) if row.id == clip.id && row.media.as_ref()
                .is_some_and(|now| !now.is_picture && now.blob == media.blob && now.playable.is_some()))))
    }

    pub fn read(model: &Model, listening: bool, clock: &LocalClock) -> Self {
        Self::read_with_waits(model, listening, clock, &crate::tools::AgentWaits::default())
    }

    pub fn read_with_waits(model: &Model, listening: bool, clock: &LocalClock, waits: &crate::tools::AgentWaits) -> Self {
        Self::read_chat(model, listening, clock, waits, &Default::default())
    }

    pub fn read_chat(model: &Model, listening: bool, clock: &LocalClock, waits: &crate::tools::AgentWaits,
        expanded: &std::collections::BTreeSet<TranscriptId>) -> Self {
        Self::read_chat_at(model, listening, clock, waits, expanded, crate::model::wall_ms(), &HashSet::new())
    }

    pub fn read_chat_at(model: &Model, listening: bool, clock: &LocalClock, waits: &crate::tools::AgentWaits,
        expanded: &std::collections::BTreeSet<TranscriptId>, now: u64, folds: &HashSet<TraceFold>) -> Self {
        let traces = &waits.traces;
        let me = model.me();
        let my_seat = model.state.seat_of(&me);
        let tones = seat_tones(model);
        let author_badge = |author: &PublicKey, actor| {
            let id = model.state.seat_of(author);
            let mine = *author == me || my_seat.is_some_and(|s| Some(s) == id);
            let tone = id.filter(|_| !mine).map(|s| tones.get(&s).copied()
                .unwrap_or_else(|| SeatTone(preferred_seat_slot(s, 0))));
            (Seat::from_author(mine, actor), tone)
        };
        let mut projects = Vec::new();
        let mut index = std::collections::HashMap::new();
        for entity in project_entities(&model.state, None, Some(kind::PROJECT)) {
            index.insert(entity.id, projects.len());
            projects.push(Project {
                id: entity.id,
                name: entity.text(field::TITLE).to_owned(),
                goal: entity.text(field::TEXT).to_owned(),
                columns: Default::default(),
                proposed: Vec::new(),
                authors: Vec::new(),
                docs: Vec::new(),
            });
        }
        for doc in model.state.entities.values().filter(|e| e.kind == kind::DOC && !e.flag(field::DELETED) && !e.flag(field::ARCHIVED)) {
            if let Some(&i) = doc.project.as_ref().and_then(|id| index.get(id)) {
                projects[i].docs.push(DocumentRow { id: doc.id, title: doc.text(field::TITLE).into() });
            }
        }
        for project in &mut projects {
            project.docs.sort_by(|a, b| a.title.cmp(&b.title).then(a.id.cmp(&b.id)));
        }
        let mut names = std::collections::BTreeMap::new();
        let mut author_name = |author, actor| {
            Arc::<str>::clone(names.entry((author, actor)).or_insert_with(|| model.author_label(&author, actor).into()))
        };
        for task in model.ordered_tasks(None) {
            let Some(project) = task.project.and_then(|id| index.get(&id)).map(|&i| &mut projects[i]) else {
                continue;
            };
            let (seat, tone) = author_badge(&task.author, task.actor);
            if !project.authors.contains(&seat) {
                project.authors.push(seat);
            }
            if task.flag(field::PROPOSED) { project.proposed.push(task.id); }
            project.columns[task_column(task)].push(Card {
                id: task.id,
                proposed: task.flag(field::PROPOSED),
                author_name: author_name(task.author, task.actor),
                number: format!("#{}", model.state.task_numbers[&task.id]),
                title: task.text(field::TITLE).to_owned(),
                seat, tone,
            });
        }
        let mut connect = AiCommands::default();
        let mut pending = std::collections::BTreeMap::new();
        for id in model.active_agents_in_join_order() {
            let Some(agent) = model.state.agents.get(&id) else { continue };
            let owner = agent.owner == me;
            let activity = model.agent_activity(id);
            let live = activity.as_ref().is_some_and(|a| waits.listening(a.binding));
            let current = activity.as_ref().and_then(|a| traces.current(a.binding));
            // A trace tells the seat only while the durable state agrees the AI is here or answering.
            let active_trace = matches!((current, &activity), (Some(_), Some(a)) if live || a.answering);
            let working = active_trace && current.is_some_and(|t| t.spin_until(now).is_some());
            let (away, status) = match (current, activity) {
                (Some(trace), Some(a)) if live || a.answering => (false, trace_seat(trace, now, clock)),
                (_, Some(_)) if live => (false, "listening".into()),
                (_, Some(a)) if a.answering => (false, "answering…".into()),
                (_, Some(a)) => (true, format!("away, last heard {}", clock.hm(a.last_heard))),
                (_, None) if owner => (true, "away".into()),
                (_, None) => (false, peer_ai_status(model, id, &agent.owner, clock)),
            };
            connect.rows.push(AgentRow {
                tone: author_badge(&agent.owner, Actor::Ai).1,
                id, name: model.agent_display_label(id), owner, status, away, live, working,
            });
            if owner {
                match model.agent_requests(id) {
                    Ok(lines) => pending.extend(lines.into_iter().map(|line| (line, id))),
                    Err(why) => makepad_widgets::log!("town: reading agent requests: {why}"),
                }
            }
        }
        // A bare `@` line waits for an AI that joins later; until one does, its row says so.
        let unheard_from = if !listening && model.my_agent(None).is_none() {
            makepad_town_core::queue::unanswered_from(&model.state, &me)
        } else { usize::MAX };
        let mut seat_names = std::collections::BTreeMap::new();
        for device in model.state.admitted_devices() {
            if let Some(seat) = model.state.seat_of(&device) {
                seat_names.entry(seat).or_insert_with(|| model.peer_seat_name(seat, Some(&device)));
            }
        }
        if let Some(seat) = my_seat { seat_names.insert(seat, "You".into()); }
        let name = |seat: &Id| seat_names.get(seat).cloned().unwrap_or_else(|| "Peer".into());
        let mut rows = Vec::new();
        let mut unknown = model.state.unknown.iter()
            .filter_map(|&id| model.state.order_of(&id).map(|order| (id, order)))
            .peekable();
        // Merge once, at visible row boundaries. The plain timeline row already has
        // the quiet treatment used for membership notices and has no chat actions.
        let mut show_unknown = |rows: &mut Vec<ChatRow>, before: Option<&Id>| {
            if unknown.peek().is_none() { return }
            let limit = before.and_then(|id| model.state.order_of(id));
            let mut first = None;
            let mut count = 0usize;
            while let Some((id, _)) = unknown.next_if(|(_, order)| limit.is_none_or(|limit| *order < limit)) {
                first.get_or_insert(id);
                count += 1;
            }
            if let Some(id) = first {
                let text = match count {
                    1 => "A newer Makepad Town wrote something here.".into(),
                    n => format!("A newer Makepad Town wrote {n} things here."),
                };
                rows.push(ChatRow::Member(MemberNotice { key: NoticeKey::Unknown(id), text }));
            }
        };
        let today = clock.local_day_time(now / 1000).0;
        let mut last_day = None;
        let mut day_counts = HashMap::new();
        let mut before_row = |rows: &mut Vec<ChatRow>, id: &Id, wall_ms: u64| {
            show_unknown(rows, Some(id));
            if wall_ms == 0 { return }
            let day = clock.local_day_time(wall_ms / 1000).0;
            if last_day != Some(day) {
                let occurrence = *day_counts.entry(day).and_modify(|n| *n += 1).or_insert(0);
                rows.push(ChatRow::Day(DayDivider { day, occurrence, label: day_label(day, today) }));
                last_day = Some(day);
            }
        };
        let mut clip_blobs = std::collections::HashMap::new();
        let provider = model.service_provider(Service::Summary);
        let tasks_provider = model.service_provider(Service::Tasks).filter(|_| model.current_project().is_some());
        let mut summarisable = HashSet::new();
        let mut updatable = HashSet::new();
        let mut calls = chat_calls(model, my_seat, &name);
        let mut last_shown = None;
        let mut message_number = 0;
        let mut chat_index = std::collections::HashMap::new();
        for entry in &model.state.timeline {
            match entry {
                Entry::Member(id) => {
                    let Some(accept) = model.event(id) else { continue };
                    let Some(grant) = model.accepted_grant(accept) else { continue };
                    before_row(&mut rows, id, accept.header.wall_ms);
                    let by = model.author_label(&grant.header.author, Actor::Human);
                    let line = if accept.header.author == me { format!("{by} added your computer") }
                        else { format!("{by} added {}'s computer", model.author_label(&accept.header.author, Actor::Human)) };
                    rows.push(ChatRow::Member(MemberNotice { key: NoticeKey::Member(*id), text: line }));
                    last_shown = Some(*id);
                }
                Entry::Clip(id) => {
                    let Some(entity) = model.state.entities.get(id) else { continue };
                    before_row(&mut rows, id, model.event(id).map_or(0, |e| e.header.wall_ms));
                    let index = rows.len();
                    let clip = ClipLine::read(model, entity, |id, here| {
                        let blob = clip_blobs.entry(id).or_insert_with(|| ClipBlob { here, rows: Vec::new() });
                        if blob.rows.last() != Some(&index) { blob.rows.push(index); }
                    });
                    rows.push(ChatRow::Clip(Arc::new(clip)));
                    last_shown = Some(*id);
                }
                Entry::Session(id) => {
                    let Some((count, words, call_back)) = calls.remove(id) else { continue };
                    let Some(anchor) = model.state.anchor(*entry).and_then(|id| model.event(&id)) else { continue };
                    before_row(&mut rows, &anchor.id, anchor.header.wall_ms);
                    let key = TranscriptId(model.open_id(), *id);
                    let transcript = (count > 0 && expanded.contains(&key)).then(|| {
                        model.state.said(id).iter().map(|line| {
                            let speaker = model.state.seat_of(&line.author).as_ref().map(&name).unwrap_or_else(|| model.author_label(&line.author, Actor::Human));
                            let secs = line.at_ms / 1000;
                            format!("{speaker} · {}:{:02} · {}", secs / 60, secs % 60, line.text)
                        }).collect::<Vec<_>>().join("\n")
                    });
                    let sent = model.state.transcript_shares(id).map(|(share, ai)| {
                        // The service's own name says where the words went; the sender is named like every author.
                        let by = if share.author == me { "you".to_owned() } else { crate::model::clean_name(&model.author_label(&share.author, Actor::Human)) };
                        let ai = crate::model::clean_name(ai);
                        let ai = if ai.is_empty() { "Claude" } else { &ai };
                        format!("Sent to {ai} by {by}")
                    }).collect::<std::collections::BTreeSet<_>>().into_iter().collect::<Vec<_>>().join("\n");
                    if provider.is_some() && model.can_request(Service::Summary, *id, count) { summarisable.insert(*id); }
                    if tasks_provider.is_some() && model.can_request(Service::Tasks, *id, count) { updatable.insert(*id); }
                    let jobs = model.summary_jobs(*id).map(|(job, ahead)| {
                        let local = job.provider == me;
                        let provider = if local { "This computer".into() } else { format!("{}'s computer", model.device_label(&job.provider)) };
                        JobLine::read(job, &model.author_label(&job.requester, Actor::Human), &provider,
                            local || model.synced_with(&job.provider), ahead)
                    }).collect();
                    last_shown = Some(anchor.id);
                    rows.push(ChatRow::Call(CallLine { id: key, words, time: clock.hm(anchor.header.wall_ms),
                        call_back, sent, jobs, count, transcript }));
                }
                Entry::Chat(id) => {
                    let Some(m) = model.state.message(id) else { continue };
                    before_row(&mut rows, id, m.wall_ms);
                    let index = rows.len();
                    last_shown = Some(m.id);
                    let addressed = addresses_my_ai(m, &me);
                    // A person's `@` line goes to their own AI only. Everyone in the town sees it,
                    // marked, and other seats’ AIs cannot read it.
                    let to_own_ai = m.actor == Actor::Human && m.chat.to_ai;
                    let is_pending = if let Some(&agent) = pending.get(&m.id) {
                        let decision = |allow| AgentControl::Decide { town: model.open_id(), agent, line: m.id, allow };
                        connect.requests.insert(index, ApprovalRow {
                            cue: model.ai_request_label_for(&m.author, Some(agent), &me),
                            allow: decision(true), deny: decision(false),
                            label: model.agent_custom_name(agent).map_or_else(|| "Let your AI do it".into(), |c| format!("Let {c} do it")),
                        });
                        true
                    } else {
                        false
                    };
                    let mentions_you = is_pending
                        || model.state.mentions_device(m, &me)
                        || model.state.replies_to_device(m, &me);
                    let name = message_name(model, m, || author_name(m.author, m.actor).to_string());
                    let (seat, tone) = author_badge(&m.author, m.actor);
                    let receipts = traces.rows(&m.id);
                    chat_index.insert(m.id, index);
                    rows.push(ChatRow::Message(Message {
                        id: m.id,
                        // The fold drops the quote of a retracted line, so its text never shows here.
                        quote: m.chat.quote.map(|id| Quote::read(model, id)),
                        traces: receipts.iter().enumerate().map(|(i, trace)| {
                            let key = TraceFold { line: m.id, trace: i, history: trace.phase(now).finished() };
                            TraceRow::read(trace, now, clock, folds.contains(&key))
                        }).collect(),
                        seat, tone,
                        name,
                        time: clock.hm(m.wall_ms),
                        text: if to_own_ai && receipts.is_empty() {
                            format!("ai {}", m.chat.text)
                        } else {
                            m.chat.text.clone()
                        },
                        addressed,
                        unheard: receipts.is_empty() && addressed && message_number >= unheard_from,
                        mention: to_own_ai && receipts.is_empty(),
                        mentions_you,
                        deletable: model.can_delete_message(&m.id),
                    }));
                    message_number += 1;
                }
            }
        }
        show_unknown(&mut rows, None);
        let current = model.current_project().map(|p| p.id);
        Self {
            index,
            chat_index,
            rows_rev: next_rows_rev(),
            day_today: last_day.map(|_| today),
            writable: model.state.active_member(&me),
            removable: current.filter(|id| model.can_remove_project(*id)),
            removed: None,
            current,
            projects,
            rows,
            summarisable,
            updatable,
            clip_blobs,
            last_shown,
            listening,
            connect,
        }
    }

    /// Relabel dividers in place. Their keys and the row revision stay stable across midnight.
    pub fn refresh_day_labels(&mut self, clock: &LocalClock, now_ms: u64) -> bool {
        let Some(previous) = self.day_today else { return false };
        let today = clock.local_day_time(now_ms / 1000).0;
        if previous == today { return false }
        for row in &mut self.rows {
            if let ChatRow::Day(divider) = row {
                divider.label = day_label(divider.day, today);
            }
        }
        self.day_today = Some(today);
        true
    }

    pub fn day_refresh_after_secs(&self, clock: &LocalClock, now_secs: u64) -> Option<u64> {
        self.day_today.map(|_| clock.next_day_check_at(now_secs))
    }

    /// Row index in the displayed timeline for a live chat message.
    pub fn chat_row(&self, id: &Id) -> Option<usize> {
        self.chat_index.get(id).copied()
    }

    #[cfg(test)]
    pub(crate) fn index_chat_rows(&mut self) {
        self.chat_index.clear();
        for (index, row) in self.rows.iter().enumerate() {
            if let ChatRow::Message(message) = row {
                self.chat_index.insert(message.id, index);
            }
        }
    }

    pub fn refresh_trace(&mut self, traces: &Traces, binding: makepad_town_core::queue::Credential, now: u64,
        clock: &LocalClock, folds: &HashSet<TraceFold>, tick: bool) {
        for line in traces.open_lines(binding) {
            let Some(&index) = self.chat_index.get(&line) else { continue };
            let ChatRow::Message(message) = &mut self.rows[index] else { continue };
            for (i, (row, trace)) in message.traces.iter_mut().zip(traces.rows(&line)).enumerate() {
                let phase = trace.phase(now);
                if tick {
                    if !phase.ticking() { continue }
                    if row.phase == phase {
                        row.headline = trace.headline(now, clock);
                        continue
                    }
                }
                let key = TraceFold { line, trace: i, history: phase.finished() };
                *row = TraceRow::read(trace, now, clock, folds.contains(&key));
            }
        }
        if let Some(current) = traces.current(binding).filter(|r| !tick || r.phase(now).ticking()) {
            if let Some(seat) = self.connect.rows.iter_mut().find(|seat| seat.id == current.agent && (seat.live || !seat.away)) {
                seat.status = trace_seat(current, now, clock);
                seat.working = current.spin_until(now).is_some();
            }
        }
    }

    pub fn toggle_trace(&mut self, key: TraceFold, traces: &Traces, folds: &mut HashSet<TraceFold>, now: u64, clock: &LocalClock) {
        let Some(&index) = self.chat_index.get(&key.line) else { return };
        let Some(trace) = traces.rows(&key.line).get(key.trace) else { return };
        if trace.phase(now).finished() != key.history { return }
        let ChatRow::Message(message) = &mut self.rows[index] else { return };
        let Some(row) = message.traces.get_mut(key.trace) else { return };
        if row.fold_count == 0 { return }
        if !folds.remove(&key) { folds.insert(key); }
        *row = TraceRow::read(trace, now, clock, folds.contains(&key));
    }
    /// Anchor of the final readable timeline row in this displayed snapshot.
    pub fn last_shown(&self) -> Option<Id> { self.last_shown }

    /// Does the window hold a button whose press needs the person's consent?
    pub fn consent_buttons(&self) -> bool {
        !self.summarisable.is_empty() || !self.updatable.is_empty() || self.selected().is_some_and(|p| !p.proposed.is_empty())
    }

    pub fn selected(&self) -> Option<&Project> {
        self.index.get(&self.current?).map(|&i| &self.projects[i])
    }
}

fn trace_seat(trace: &Trace, now: u64, clock: &LocalClock) -> String {
    if trace.phase(now) == Phase::GotIt { "answering…".into() } else { trace.headline(now, clock) }
}

fn peer_ai_status(model: &Model, id: Id, owner: &PublicKey, clock: &LocalClock) -> String {
    if let Some(msg) = model.state.agent_reply(&id) {
        format!("replied {}", clock.hm(msg.wall_ms))
    } else {
        format!("on {}'s computer", model.author_label(owner, Actor::Human))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceStatus {
    NotConnected,
    Offline(String),
    Reaching(u64),
    Synced,
    NotSyncing,
    NeedsUpdate(bool),
}

impl DeviceStatus {
    pub fn text(&self) -> String {
        match self {
            Self::NotConnected => "away".into(),
            Self::Offline(when) => format!("away · last seen {when}"),
            Self::Reaching(secs) if *secs < 60 => "reaching…".into(),
            Self::Reaching(secs) => format!("reaching for {} min", secs / 60),
            Self::Synced => "here".into(),
            Self::NotSyncing => "not syncing this town".into(),
            Self::NeedsUpdate(true) => "needs a newer Town".into(),
            Self::NeedsUpdate(false) => "your Town is older".into(),
        }
    }

    fn presence(&self) -> Presence {
        match self {
            Self::Synced => Presence::Here,
            Self::Reaching(_) => Presence::Reaching,
            Self::NotConnected | Self::Offline(_) | Self::NotSyncing | Self::NeedsUpdate(_) => Presence::Away,
        }
    }
}

/// The device for which today's link worker and known-device store expose a presence signal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TablePeer {
    pub name: String,
    pub status: DeviceStatus,
    /// When the reaching time or local date on this seat can change.
    pub refresh_after_secs: Option<u64>,
}

pub(crate) fn peer_initial(name: &str) -> String {
    seat_initial(name, 'P')
}

pub(crate) fn seat_initial(name: &str, fallback: char) -> String {
    name.trim().chars().next().unwrap_or(fallback).to_uppercase().collect()
}

impl TablePeer {
    /// Read one town member's device from the link and sync signals for that key.
    pub fn read(name: &str, linked: Option<bool>, peer_update: Option<PeerUpdate>, reaching: Option<u64>,
        last: Option<u64>, clock: &LocalClock, now: u64) -> Self {
        let (status, refresh_after_secs) = if let Some(update) = peer_update {
            (DeviceStatus::NeedsUpdate(matches!(update, PeerUpdate::TheyAreOlder)), None)
        } else if let Some(synced) = linked {
            (if synced { DeviceStatus::Synced } else { DeviceStatus::NotSyncing }, None)
        } else if let Some(since) = reaching {
            let secs = now.saturating_sub(since);
            (DeviceStatus::Reaching(secs), Some(60 - secs % 60))
        } else if let Some(last) = last {
            let (day, time) = clock.local_day_time(last);
            let (today, now_time) = clock.local_day_time(now);
            let when = if day == today { hours_minutes(time) } else { calendar_date(day) };
            let refresh = (day >= today).then(|| clock.next_day_check(now, now_time));
            (DeviceStatus::Offline(when), refresh)
        } else {
            (DeviceStatus::NotConnected, None)
        };
        Self {
            name: peer_label(name).to_owned(),
            status,
            refresh_after_secs,
        }
    }

    pub fn initial(&self) -> String {
        peer_initial(&self.name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberId { You, Seat(Id), YourAi, Agent(Id) }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence { Here, Reaching, Working, Away }

impl Presence {
    pub(crate) fn rank(self) -> u8 {
        match self { Self::Away => 0, Self::Reaching => 1, Self::Here => 2, Self::Working => 3 }
    }
}


#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct RetryQuiet {
    quiet: std::collections::BTreeSet<PublicKey>,
}

impl RetryQuiet {
    pub fn on_worker_event(&mut self, peer: PublicKey, event: &crate::link_worker::FromWorker) {
        use crate::link_worker::{FromWorker, Status as LinkStatus};
        match event {
            FromWorker::Phase(LinkStatus::Backoff { .. }) => { self.quiet.insert(peer); }
            FromWorker::Phase(LinkStatus::Securing | LinkStatus::Proving | LinkStatus::Linked | LinkStatus::Stopped)
                | FromWorker::Linked { .. } | FromWorker::KeyChanged | FromWorker::Failed(_) => self.clear(&peer),
            _ => {}
        }
    }

    pub fn clear(&mut self, peer: &PublicKey) {
        self.quiet.remove(peer);
    }

    pub fn is_quiet(&self, peer: &PublicKey) -> bool {
        self.quiet.contains(peer)
    }
}

fn agent_presence(agent: &AgentRow) -> Presence {
    if !agent.owner { Presence::Away }
    else if agent.working { Presence::Working }
    else if agent.live || !agent.away { Presence::Here }
    else { Presence::Away }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeatDevice {
    pub key: PublicKey,
    pub name: String,
    pub presence: Presence,
    pub status: String,
    pub is_me: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveActivity {
    Calling,
    Ringing,
    InCall,
    Sharing { session: SessionId, title: String },
}

impl LiveActivity {
    pub fn rank(&self) -> u8 {
        match self {
            Self::Calling => 1,
            Self::Ringing => 2,
            Self::InCall => 3,
            Self::Sharing { .. } => 4,
        }
    }
}

/// Call and share activity shown on the open town's member chips and cards.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LiveRoster {
    pub town: Option<TownId>,
    pub by_device: BTreeMap<PublicKey, Vec<LiveActivity>>,
}

impl LiveRoster {
    pub fn from_call(open_town: TownId, me: PublicKey, call: &crate::call::CallShown,
        calling_peers: &[PublicKey], ringing_from: Option<PublicKey>) -> Self {
        use crate::call::{Phase, ShareRole};
        let mut by_device = BTreeMap::new();
        let mut push = |key, activity| {
            let list: &mut Vec<_> = by_device.entry(key).or_default();
            if !list.contains(&activity) { list.push(activity); }
        };
        if call.view.town == Some(open_town) {
            match call.view.phase {
                Phase::Idle => {}
                Phase::Calling => {
                    push(me, LiveActivity::Calling);
                    for &peer in calling_peers {
                        if peer != me { push(peer, LiveActivity::Ringing); }
                    }
                }
                Phase::Ringing => {
                    if let Some(from) = ringing_from { push(from, LiveActivity::Calling); }
                }
                Phase::Live => {
                    push(me, LiveActivity::InCall);
                    for p in &call.view.participants { push(p.peer, LiveActivity::InCall); }
                }
            }
        }
        for offer in call.share.offers.iter().filter(|o| o.town == open_town) {
            let owner = match offer.role {
                ShareRole::Sharer { .. } => me,
                ShareRole::Viewer { sharer } => sharer,
            };
            push(owner, LiveActivity::Sharing { session: offer.session, title: offer.title.clone() });
        }
        Self { town: (!by_device.is_empty()).then_some(open_town), by_device }
    }

    fn activities(&self, key: &PublicKey) -> &[LiveActivity] {
        self.by_device.get(key).map_or(&[], Vec::as_slice)
    }

    fn best<'a>(activities: impl Iterator<Item = &'a LiveActivity>) -> Option<&'a LiveActivity> {
        let mut best: Option<&LiveActivity> = None;
        for activity in activities {
            if best.is_none_or(|prev| activity.rank() > prev.rank()) { best = Some(activity); }
        }
        best
    }

    pub fn primary(&self, key: &PublicKey) -> Option<&LiveActivity> {
        Self::best(self.activities(key).iter())
    }

    pub fn primary_for_devices(&self, devices: &[SeatDevice]) -> Option<&LiveActivity> {
        Self::best(devices.iter().filter_map(|device| self.primary(&device.key)))
    }

    fn summary<'a>(activities: impl Iterator<Item = &'a LiveActivity>) -> Option<String> {
        let mut call_rank = 0;
        let mut call = None;
        let mut share = None;
        for activity in activities {
            let phrase = match activity {
                LiveActivity::Calling => "calling…",
                LiveActivity::Ringing => "ringing…",
                LiveActivity::InCall => "in call",
                LiveActivity::Sharing { title, .. } => {
                    share.get_or_insert(title);
                    continue
                }
            };
            if activity.rank() > call_rank {
                call_rank = activity.rank();
                call = Some(phrase);
            }
        }
        match (call, share) {
            (Some(call), Some(title)) => Some(format!("{call} · sharing {title}")),
            (None, Some(title)) => Some(format!("sharing {title}")),
            (call, None) => call.map(str::to_owned),
        }
    }

    pub fn summary_for(&self, key: &PublicKey) -> Option<String> {
        Self::summary(self.activities(key).iter())
    }

    pub fn summary_for_devices(&self, devices: &[SeatDevice]) -> Option<String> {
        Self::summary(devices.iter().flat_map(|device| self.activities(&device.key)))
    }

    pub fn share_for(&self, key: &PublicKey) -> Option<(SessionId, &str)> {
        self.activities(key).iter().find_map(|activity| match activity {
            LiveActivity::Sharing { session, title } => Some((*session, title.as_str())),
            _ => None,
        })
    }

    pub fn share_for_devices(&self, devices: &[SeatDevice]) -> Option<(PublicKey, SessionId, &str)> {
        devices.iter().find_map(|d| self.share_for(&d.key).map(|(session, title)| (d.key, session, title)))
    }

    pub fn in_call(&self, key: &PublicKey) -> bool {
        self.activities(key).contains(&LiveActivity::InCall)
    }

    pub fn has_call_for_devices(&self, devices: &[SeatDevice]) -> bool {
        devices.iter().any(|d| self.activities(&d.key).iter().any(|a|
            matches!(a, LiveActivity::Calling | LiveActivity::Ringing | LiveActivity::InCall)))
    }
}

/// A peer seat's display name when only its ordered device keys are available.
pub fn seat_label(model: &Model, devices: &[PublicKey]) -> String {
    model.peer_seat_name_for_devices(devices)
}

pub fn cascade_question(target_name: &str, cascade_names: &[String]) -> (String, String, String) {
    let who = match cascade_names {
        [] => String::new(),
        [a] => a.clone(),
        [a, b] => format!("{a} and {b}"),
        [a, b, c] => format!("{a}, {b} and {c}"),
        [a, b, rest @ ..] => format!("{a}, {b} and {} others", rest.len()),
    };
    let total = 1 + cascade_names.len();
    let all = if total == 2 { "Remove both".into() } else { format!("Remove all {total}") };
    (format!("Also remove {who}, who joined through {target_name}?"), all, format!("Only {target_name}"))
}

pub fn removing_seat_line(target_name: &str, device_count: usize, cascade_names: &[String]) -> String {
    let who = match cascade_names {
        [] if device_count <= 1 => return format!("Removing {target_name} — that device can't rejoin this town."),
        [] => format!("{target_name} ({device_count} computers)"),
        [a] => format!("{target_name} and {a}"),
        [a, b] => format!("{target_name}, {a} and {b}"),
        many => format!("{target_name} and {} others", many.len()),
    };
    format!("Removing {who} — those devices can't rejoin this town.")
}

fn best_presence(devices: &[SeatDevice]) -> Presence {
    devices.iter().map(|d| d.presence).max_by_key(|p| p.rank()).unwrap_or(Presence::Away)
}

fn seat_summary_status(devices: &[SeatDevice], statuses: &[DeviceStatus], best: Presence) -> String {
    use std::borrow::Cow;
    if devices.len() <= 1 { return devices.first().map_or_else(String::new, |d| d.status.clone()) }
    let lead = match best {
        Presence::Here => Cow::Borrowed("here"),
        Presence::Working => Cow::Borrowed("working"),
        Presence::Reaching => statuses.iter()
            .find(|s| matches!(s, DeviceStatus::Reaching(_)))
            .map_or(Cow::Borrowed("reaching…"), |s| Cow::Owned(s.text())),
        Presence::Away if statuses.contains(&DeviceStatus::NeedsUpdate(false)) => Cow::Borrowed("your Town is older"),
        Presence::Away if statuses.contains(&DeviceStatus::NeedsUpdate(true)) => Cow::Borrowed("needs a newer Town"),
        Presence::Away if statuses.contains(&DeviceStatus::NotSyncing) => Cow::Borrowed("not syncing this town"),
        Presence::Away => Cow::Borrowed("away"),
    };
    format!("{lead} · {} computers", devices.len())
}

fn append_live(base: String, live: Option<String>) -> String {
    match live {
        Some(live) if base.is_empty() => live,
        Some(live) => format!("{base} · {live}"),
        None => base,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberSeat {
    pub id: MemberId,
    pub seat: Seat,
    pub tone: Option<SeatTone>,
    pub presence: Presence,
    pub live: Option<LiveActivity>,
    pub name: String,
    pub status: String,
    pub devices: Vec<SeatDevice>,
}

impl MemberSeat {
    pub fn has_device(&self, key: &PublicKey) -> bool {
        self.devices.iter().any(|d| d.key == *key)
    }
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct TownMembers {
    pub seats: Vec<MemberSeat>,
    pub refresh_after_secs: Option<u64>,
}

impl TownMembers {
    pub fn seat_tone_for_device(&self, key: &PublicKey) -> Option<SeatTone> {
        self.seats.iter().find(|s| s.seat == Seat::Peer && s.has_device(key)).and_then(|s| s.tone)
    }

    pub fn read(model: &Model, link: &ConnectView, workspace: &Workspace, clock: &LocalClock, now: u64) -> Self {
        Self::read_with_quiet(model, link, &RetryQuiet::default(), workspace, clock, now)
    }

    pub fn read_with_quiet(model: &Model, link: &ConnectView, quiet: &RetryQuiet,
        workspace: &Workspace, clock: &LocalClock, now: u64) -> Self {
        Self::read_with_live(model, link, quiet, &LiveRoster::default(), workspace, clock, now)
    }

    pub fn read_with_live(model: &Model, link: &ConnectView, quiet: &RetryQuiet, roster: &LiveRoster,
        workspace: &Workspace, clock: &LocalClock, now: u64) -> Self {
        let empty = LiveRoster::default();
        let roster = if roster.town.is_none_or(|t| t == model.open_id()) { roster } else { &empty };
        let mut members = Self::default();
        let me = model.me();
        let active_here = model.state.active_member(&me);
        let summaries = model.seats_in_order();
        let tones = tones_in_order(model, &summaries);
        let mut read_device = |key| {
            if key == me {
                return (SeatDevice { key, name: model.device_label(&me).into(), presence: Presence::Here,
                    status: append_live("this computer".into(), roster.summary_for(&me)), is_me: true }, DeviceStatus::Synced)
            }
            let name = model.device_label(&key);
            let linked = (active_here && model.linked_with(&key)).then(|| model.synced_with(&key));
            let reaching = match link.device_link(&key, 0) {
                DeviceLink::Reaching { since } if active_here && !quiet.is_quiet(&key) => Some(since),
                _ => None,
            };
            let peer_update = active_here.then(|| model.peer_update(&key)).flatten();
            let peer = TablePeer::read(name, linked, peer_update, reaching, model.device_last_seen(&key), clock, now);
            members.refresh_after_secs = members.refresh_after_secs.into_iter().chain(peer.refresh_after_secs).min();
            (SeatDevice { key, name: peer.name, presence: peer.status.presence(),
                status: append_live(peer.status.text(), roster.summary_for(&key)), is_me: false }, peer.status)
        };
        let mut my_devices: Vec<_> = summaries.iter().find(|s| s.is_mine)
            .map(|s| s.devices.iter().map(|&key| read_device(key).0).collect()).unwrap_or_default();
        if let Some(pos) = my_devices.iter().position(|d| d.is_me) {
            my_devices[..=pos].rotate_right(1);
        }
        let my_status = match (active_here, my_devices.len()) {
            (true, n) if n > 1 => format!("{n} computers"),
            (true, _) => String::new(),
            (false, 0) => "not in this town".into(),
            (false, 1) => "away · 1 other computer".into(),
            (false, n) => format!("away · {n} other computers"),
        };
        members.seats.push(MemberSeat { id: MemberId::You, seat: Seat::You, tone: None, presence: best_presence(&my_devices),
            name: model.device_label(&me).into(), status: append_live(my_status, roster.summary_for_devices(&my_devices)),
            live: roster.primary_for_devices(&my_devices).cloned(), devices: my_devices });
        for summary in summaries.into_iter().filter(|s| !s.is_mine) {
            let (devices, statuses): (Vec<_>, Vec<_>) = summary.devices.into_iter().map(&mut read_device).unzip();
            let presence = best_presence(&devices);
            let base_status = seat_summary_status(&devices, &statuses, presence);
            let status = if devices.len() > 1 { append_live(base_status, roster.summary_for_devices(&devices)) } else { base_status };
            members.seats.push(MemberSeat {
                id: MemberId::Seat(summary.seat), seat: Seat::Peer, tone: tones.get(&summary.seat).copied(),
                presence, name: model.peer_seat_name(summary.seat, devices.first().map(|d| &d.key)),
                status, live: roster.primary_for_devices(&devices).cloned(), devices,
            });
        }
        if workspace.listening { members.seats.push(MemberSeat { id: MemberId::YourAi, seat: Seat::YourAi, tone: None, presence: Presence::Here, name: "Your AI".into(), status: "listening".into(), live: None, devices: Vec::new() }); }
        let my_seat = model.state.seat_of(&me);
        for agent in &workspace.connect.rows {
            let mine = agent.owner || model.state.agents.get(&agent.id).is_some_and(|a|
                my_seat.is_some_and(|s| model.state.seat_of(&a.owner) == Some(s)));
            members.seats.push(MemberSeat {
                id: MemberId::Agent(agent.id), seat: if mine { Seat::YourAi } else { Seat::PeerAi },
                tone: agent.tone, presence: agent_presence(agent), name: agent.name.clone(), status: agent.status.clone(), live: None, devices: Vec::new(),
            });
        }
        members
    }

    pub fn sync_agents(&mut self, rows: &[AgentRow]) -> bool {
        let mut changed = false;
        for seat in &mut self.seats {
            let MemberId::Agent(id) = seat.id else { continue };
            let Some(agent) = rows.iter().find(|row| row.id == id) else { continue };
            let presence = agent_presence(agent);
            if seat.presence != presence || seat.status != agent.status {
                seat.presence = presence;
                seat.status.clone_from(&agent.status);
                changed = true;
            }
        }
        changed
    }
}

fn hours_minutes(time: i64) -> String {
    format!("{:02}:{:02}", time / 3600, time / 60 % 60)
}

// Howard Hinnant's civil_from_days, as in platform's script clock. Day zero is 1970-01-01.
fn civil_ymd(day: i64) -> (i64, u32, u32) {
    let z = day + 719468;
    let era = z.div_euclid(146097);
    let day_of_era = z.rem_euclid(146097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let march_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * march_month + 2) / 5 + 1;
    let month = if march_month < 10 { march_month + 3 } else { march_month - 9 };
    let year = era * 400 + year_of_era + i64::from(month <= 2);
    (year, month as u32, day as u32)
}

fn calendar_date(day: i64) -> String {
    let (year, month, day) = civil_ymd(day);
    format!("{year:04}-{month:02}-{day:02}")
}

pub(crate) fn day_label(day: i64, today: i64) -> String {
    if day == today { return "Today".into() }
    if day == today - 1 { return "Yesterday".into() }
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let (year, month, day) = civil_ymd(day);
    let month = MONTHS[(month - 1) as usize];
    if year == civil_ymd(today).0 { format!("{day} {month}") } else { format!("{day} {month} {year}") }
}

// The host offset is the fallback on hosts without a system zone file. Unix zone files also
// retain historical offset changes, so yesterday's chat keeps its own daylight-saving time.
#[derive(Default)]
pub struct LocalClock {
    initial: Option<i32>,
    changes: Vec<(i64, i32)>,
}

impl LocalClock {
    pub fn load() -> Self {
        Self::load_tz(std::env::var_os("TZ").as_deref())
    }

    pub(crate) fn load_tz(zone: Option<&std::ffi::OsStr>) -> Self {
        let path = match zone {
            Some(zone) => {
                let zone = zone.to_string_lossy();
                std::path::Path::new("/usr/share/zoneinfo").join(zone.trim_start_matches(':'))
            }
            None => std::path::PathBuf::from("/etc/localtime"),
        };
        std::fs::read(path)
            .ok()
            .and_then(|bytes| Self::from_zone(&bytes))
            .unwrap_or_default()
    }

    pub(crate) fn hm(&self, ms: u64) -> String {
        hours_minutes(self.local_day_time(ms / 1000).1)
    }

    pub(crate) fn hms(&self, ms: u64) -> String {
        let time = self.local_day_time(ms / 1000).1;
        format!("{}:{:02}", hours_minutes(time), time % 60)
    }

    fn transition_index(&self, secs: u64) -> usize {
        let instant = i64::try_from(secs).unwrap_or(i64::MAX);
        self.changes.partition_point(|&(at, _)| at <= instant)
    }

    fn local_day_time(&self, secs: u64) -> (i64, i64) {
        let index = self.transition_index(secs);
        let offset = index.checked_sub(1).map(|i| self.changes[i].1).or(self.initial);
        let offset = offset
            .map(i64::from)
            .unwrap_or_else(makepad_widgets::makepad_platform::script::timer::script_local_utc_offset_secs);
        let time = (secs % 86_400) as i64 + offset;
        ((secs / 86_400) as i64 + time.div_euclid(86_400), time.rem_euclid(86_400))
    }

    fn next_day_check(&self, now: u64, time: i64) -> u64 {
        let midnight = (86_400 - time) as u64;
        self.changes.get(self.transition_index(now)).map_or(midnight, |&(at, _)| midnight.min((at as u64).saturating_sub(now).max(1)))
    }

    pub(crate) fn next_day_check_at(&self, now_secs: u64) -> u64 {
        self.next_day_check(now_secs, self.local_day_time(now_secs).1)
    }

    fn from_zone(bytes: &[u8]) -> Option<Self> {
        // TZif v2 and later lead with a legacy 32-bit block before the 64-bit table.
        let mut data = bytes;
        for width in [4, 8] {
            if data.get(..4)? != b"TZif" {
                return None;
            }
            let mut fields = data
                .get(20..44)?
                .chunks_exact(4)
                .map(|v| u32::from_be_bytes(v.try_into().unwrap()) as usize);
            let (ut, std, leaps, times, types, chars) = (
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
            );
            let mut block = data.get(44..)?;
            let mut take = |n: usize| {
                let (prefix, rest) = block.split_at_checked(n)?;
                block = rest;
                Some(prefix)
            };
            let instants = take(times.checked_mul(width)?)?;
            let indices = take(times)?;
            let offsets = take(types.checked_mul(6)?)?;
            if width == 4 && data[4] != 0 {
                take(chars)?;
                take(leaps.checked_mul(width + 4)?)?;
                take(std)?;
                take(ut)?;
                data = block;
                continue;
            }
            let offset = |kind: usize| -> Option<i32> {
                let i = kind * 6;
                Some(i32::from_be_bytes(offsets.get(i..i + 4)?.try_into().ok()?))
            };
            let mut changes = Vec::with_capacity(times);
            for (instant, &kind) in instants.chunks_exact(width).zip(indices) {
                let at = if width == 4 {
                    i64::from(i32::from_be_bytes(instant.try_into().ok()?))
                } else {
                    i64::from_be_bytes(instant.try_into().ok()?)
                };
                changes.push((at, offset(usize::from(kind))?));
            }
            return Some(Self {
                initial: offset(0),
                changes,
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mentions_you_and_first_pending_follow_fold_and_approvals() {
        use crate::model::tests::{exchange, shared_town};
        use makepad_town_core::queue::Credential;

        fn message(ws: &Workspace, id: Id) -> &Message {
            ws.rows.iter().find_map(|r| match r {
                ChatRow::Message(m) if m.id == id => Some(m),
                _ => None,
            }).expect("the chat line is in the workspace")
        }
        fn add_ai(model: &mut Model) -> Id {
            let code = Credential::parse(&model.agent_code(None).unwrap()).unwrap();
            model.agent_join_with_id(code, true).unwrap().1
        }
        let (_homes, mut ada, mut bob) = shared_town("mentions-you");
        let town = ada.open_id();
        bob.open_town(town).unwrap();
        ada.set_person_name("Ada").unwrap();
        bob.set_person_name("Bob").unwrap();
        let agent = add_ai(&mut bob);
        exchange(&mut ada, &mut bob);
        let post = |model: &mut Model, text: &str, quote| model.post_chat_draft(text, None, None, quote).unwrap();
        let read = |model: &Model| Workspace::read(model, false, &LocalClock::default());
        let mine = post(&mut bob, "My note", None);
        let own_mention = post(&mut bob, "@Bob self", None);
        let own_reply = post(&mut bob, "My reply", Some(mine));
        exchange(&mut ada, &mut bob);
        let plain = post(&mut ada, "Plain hello", None);
        let near = post(&mut ada, "@Bobx not you", None);
        let mention = post(&mut ada, "@Bob please check", None);
        let reply = post(&mut ada, "Reply to your note", Some(mine));
        exchange(&mut ada, &mut bob);
        let ws = read(&bob);
        for id in [mine, own_mention, own_reply, plain, near] {
            let m = message(&ws, id);
            assert!(!m.mentions_you, "ordinary or self-authored line was marked: {}", m.text);
        }
        assert!(message(&ws, mention).mentions_you, "an explicit seat mention is marked");
        assert!(message(&ws, reply).mentions_you, "a reply to this seat is marked");
        bob.delete_message(mine).unwrap();
        exchange(&mut ada, &mut bob);
        assert!(!message(&read(&bob), reply).mentions_you, "a retracted quote no longer marks its reply");

        let first = ada.mention_agent(agent, "first ask".into(), None).unwrap();
        let second = ada.mention_agent(agent, "second ask".into(), None).unwrap();
        exchange(&mut ada, &mut bob);
        let ws = read(&bob);
        for id in [first, second] {
            let m = message(&ws, id);
            assert!(m.mentions_you && m.name == "Ada asked your AI", "a waiting request names its author and is marked");
        }
        let (index, line, request) = ws.connect.first_pending().unwrap();
        assert_eq!((line, request.line(), ws.rows[index].key()), (first, first, ChatRowKey::Message(first)));
        let AgentControl::Decide { agent, .. } = request.deny;
        bob.decide_agent_request(town, agent, first, false).unwrap();
        let ws = read(&bob);
        assert!(!message(&ws, first).mentions_you, "declining clears the first mark");
        let (index, line, request) = ws.connect.first_pending().unwrap();
        assert_eq!((line, request.line(), ws.rows[index].key()), (second, second, ChatRowKey::Message(second)));
        bob.decide_agent_request(town, agent, second, true).unwrap();
        let ws = read(&bob);
        assert!(!message(&ws, second).mentions_you && ws.connect.first_pending().is_none(), "approval clears the last mark and request");
        add_ai(&mut ada);
        exchange(&mut ada, &mut bob);
        let unbound = post(&mut ada, "@Claude Code who?", None);
        exchange(&mut ada, &mut bob);
        let ws = read(&bob);
        assert!(!message(&ws, unbound).mentions_you && ws.connect.first_pending().is_none(), "an unbound AI name marks no request");
    }

    #[test]
    fn clip_words_distinguish_verified_bytes_a_want_and_a_damaged_copy() {
        let ana = [1; 65];
        let ben = [2; 65];
        let (town, id) = ([3; 32], [4; 32]);
        let words = |here, wanted, status: Option<Status>| clip_status(here, wanted, status.as_ref(), "12 MB", &ana,
            |key, computer| {
                let name = if key == &ana { "Ana" } else { "Ben" };
                if computer { format!("{name}'s computer") } else { name.into() }
            });
        assert_eq!(words(true, true, None), ("On this computer".into(), false), "local holdings supersede a want");
        assert_eq!(words(false, false, None), ("On Ana's computer".into(), true), "a remote recording offers Get");
        assert_eq!(words(false, true, None), ("Waiting for Ana's computer · 12 MB".into(), false), "Get also works while offline");
        assert_eq!(words(false, true, Some(Status::Waiting { town, id })), ("Waiting for Ana's computer · 12 MB".into(), false),
            "a waiting transfer names the source and total size");
        assert_eq!(words(false, true, Some(Status::Progress { town, id, have: 4_200_000, size: 12_000_000, from: ben })),
            ("4.2 of 12 MB from Ben".into(), false), "progress uses verified bytes and the actual carrier");
        assert_eq!(words(false, false, Some(Status::Bad { town, id, carrier: ben })),
            ("The copy from Ben's computer was damaged".into(), true), "a bad proof names its carrier and permits another Get");
        assert_eq!([0, 84_999, 3_605_000].map(clip_length), ["0:00", "1:24", "1:00:05"], "durations count complete seconds");
        assert_eq!([84_000, 1_200_000, 10_000_000].map(file_size), [("84".into(), "KB"), ("1.2".into(), "MB"), ("10".into(), "MB")],
            "size changes units at one MB and precision at ten");
        assert_eq!((clip_height(0, 720), clip_height(1920, 0), clip_height(640, 480)), (135., 135., 180.),
            "unknown dimensions reserve 16:9 and known dimensions keep their aspect");
        assert_eq!((clip_height(1, 100_000), clip_height(100_000, 1)), (480., 60.),
            "a peer's extreme dimensions cannot make a row taller than 1:2 or flatter than 4:1");
    }

    #[test]
    fn missed_call_keeps_its_anchor_time_and_offers_a_callback_until_the_seats_connect() {
        use crate::model::{tests::{exchange, join, ShareHomes}, Action, Origin::Ui};
        use makepad_town_core::{event::{Content, DeclineWhy, Draft, Event, LeaveWhy, SaidLine, SessionOp, Via}, key::DeviceKey, log::Log};

        fn timeline_rows(view: &Workspace) -> Vec<&ChatRow> {
            view.rows.iter().filter(|r| !matches!(r, ChatRow::Day(_))).collect()
        }
        let homes = ShareHomes::new("call-back");
        let open_home = |name| Model::open(&homes.0.join(name)).unwrap().0;
        let mut ana = open_home("ana");
        let mut ben = open_home("ben");
        ana.pin_linked(&ben.me(), 3, "Ben").unwrap();
        ben.pin_linked(&ana.me(), 3, "Ana").unwrap();
        ana.sync_start(&ben.me());
        ben.sync_start(&ana.me());
        let town = join(&mut ana, &mut ben, false);
        let to_ana = vec![ben.call_seat(&ana.me()).unwrap()];
        let to_ben = vec![ana.call_seat(&ben.me()).unwrap()];
        let mode = SessionMode::Call { video: false };
        let missed = [1; 16];
        let leave = |session| Action::SessionLeave { session, secs: None, why: LeaveWhy::Ended };
        let answered = [5; 16];
        ana.act(Action::SessionOpen { town_wide: false, town: None, session: answered, mode: mode.clone(), to: to_ben.clone() }, Ui).unwrap();
        ben.act(Action::SessionJoin { peer: None, town: None, session: answered }, Ui).unwrap();
        ana.act(leave(answered), Ui).unwrap();
        ben.act(leave(answered), Ui).unwrap();
        let spoken = [6; 16];
        ben.act(Action::SessionOpen { town_wide: false, town: None, session: spoken, mode: mode.clone(), to: to_ana.clone() }, Ui).unwrap();
        ben.act(Action::Said { session: spoken, lines: vec![SaidLine { at_ms: 0, dur_ms: 500, text: "See you soon.".into() }] }, Ui).unwrap();
        ben.act(leave(spoken), Ui).unwrap();
        exchange(&mut ana, &mut ben);

        // Seed signed facts with distinct times. The opener's clock runs ahead of the callback.
        let key = DeviceKey::load_or_create(&ana.home.key_path()).unwrap().0;
        let path = ana.home.town_log_path(&town);
        drop(ana);
        let mut log = Log::open(&path).unwrap().0;
        let open_ms = 4_102_478_400_000;
        for (op, wall_ms) in [
            (SessionOp::Open { town_wide: false, session: missed, mode: mode.clone(), to: to_ben.clone() }, open_ms),
            (SessionOp::Leave { session: missed, secs: None, why: LeaveWhy::Ended }, open_ms + 60_000),
        ] {
            let content = Content::Session(op);
            let draft = Draft { kind: content.kind(), kind_version: content.version(), critical: false, actor: Actor::Human,
                via: Via::Ui, wall_ms, parents: log.parents_for(&key.public()), content: &content.encode() };
            log.append(Event::create(&key, town, draft, [0; 16]).unwrap()).unwrap();
        }
        drop(log);
        let mut ana = open_home("ana");
        ana.sync_start(&ben.me());
        ben.sync_start(&ana.me());
        let message = ana.act(Action::PostChat { text: "Ready when you are.".into(), to_ai: false, reply_to: None }, Ui).unwrap();
        let share = [7; 16];
        ana.act(Action::SessionOpen { town_wide: false, town: None, session: share, mode: SessionMode::Share { title: "Drawing".into() }, to: to_ben.clone() }, Ui).unwrap();
        exchange(&mut ana, &mut ben);

        let clock = LocalClock { initial: Some(3600), changes: Vec::new() };
        let check = |ana: &Model, ben: &Model, pending: bool| {
            for (model, is_ben) in [(ana, false), (ben, true)] {
                let view = Workspace::read(model, false, &clock);
                let rows = timeline_rows(&view);
                let [ChatRow::Member(_), ChatRow::Call(transcribed), middle @ .., ChatRow::Message(line)] = rows.as_slice()
                    else { panic!("the transcribed call follows admission and the written message comes last") };
                assert_eq!((transcribed.id, transcribed.count), (TranscriptId(town, spoken), 1), "the transcript keeps its row");
                assert_eq!(transcribed.call_back.is_some(), !is_ben && pending, "reconnecting clears even a transcribed call's callback");
                assert_eq!(line.id, message, "the message keeps its place after the calls");
                assert_eq!(view.last_shown(), Some(message), "the composer refers to the last visible row");
                if is_ben && pending {
                    let [ChatRow::Call(row)] = middle else { panic!("only Ben's outstanding missed call belongs between the transcript and the message") };
                    assert_eq!(row.id, TranscriptId(town, missed));
                    assert_eq!(row.words, "Missed call from Ana");
                    assert_eq!(row.time, clock.hm(open_ms), "the missed call displays its Open's local time");
                    assert_eq!(row.call_back.as_ref().map(|c| (c.peer.as_str(), c.enabled)), Some(("Ana", true)));
                } else {
                    assert!(middle.is_empty(), "a call without a transcript or outstanding callback has no chat row");
                }
            }
        };
        check(&ana, &ben, true);
        ben.act(Action::SessionJoin { peer: None, town: None, session: share }, Ui).unwrap();
        ana.act(Action::Said { session: share, lines: vec![SaidLine { at_ms: 0, dur_ms: 500, text: "Look here.".into() }] }, Ui).unwrap();
        exchange(&mut ana, &mut ben);
        check(&ana, &ben, true);
        ana.act(leave(share), Ui).unwrap();
        ben.act(leave(share), Ui).unwrap();
        let declined = [2; 16];
        ben.act(Action::SessionOpen { town_wide: false, town: None, session: declined, mode: mode.clone(), to: to_ana.clone() }, Ui).unwrap();
        ana.act(Action::SessionDecline { peer: None, town: None, session: declined, why: DeclineWhy::Declined }, Ui).unwrap();
        ben.act(leave(declined), Ui).unwrap();
        exchange(&mut ana, &mut ben);
        check(&ana, &ben, true);

        let connected = [3; 16];
        ben.act(Action::SessionOpen { town_wide: false, town: None, session: connected, mode: mode.clone(), to: to_ana }, Ui).unwrap();
        ana.act(Action::SessionJoin { peer: None, town: None, session: connected }, Ui).unwrap();
        ana.act(leave(connected), Ui).unwrap();
        ben.act(leave(connected), Ui).unwrap();
        exchange(&mut ana, &mut ben);
        check(&ana, &ben, false);
        let newer = [4; 16];
        ana.act(Action::SessionOpen { town_wide: false, town: None, session: newer, mode, to: to_ben }, Ui).unwrap();
        ana.act(leave(newer), Ui).unwrap();
        exchange(&mut ana, &mut ben);
        let check_newer = |ana: &Model, ben: &Model, online: bool| {
            let caller = Workspace::read(ana, false, &clock);
            assert!(matches!(timeline_rows(&caller).as_slice(), [ChatRow::Member(_), ChatRow::Call(call), ChatRow::Message(line)]
                if call.id.1 == spoken && call.call_back.is_none() && line.id == message),
                "the caller keeps the transcript and message after the newer outgoing call");
            let view = Workspace::read(ben, false, &clock);
            let rows = timeline_rows(&view);
            let [ChatRow::Member(_), ChatRow::Call(transcribed), ChatRow::Message(line), ChatRow::Call(row)] = rows.as_slice()
                else { panic!("the transcript, message and newer missed call survive; the cleared row stays gone") };
            assert_eq!((transcribed.id.1, line.id, row.id.1), (spoken, message, newer));
            assert_eq!(row.call_back.as_ref().map(|c| c.enabled), Some(online), "the newer callback follows the link's availability");
        };
        check_newer(&ana, &ben, true);

        drop((ana, ben));
        check_newer(&open_home("ana"), &open_home("ben"), false);
    }

    #[test]
    fn non_callee_never_reads_missed_call_and_callback_targets_opener_seat() {
        use crate::model::tests::{call_history_models, record_call_event};
        use makepad_town_core::event::{Content, LeaveWhy, SaidLine, SessionOp};
        let (_homes, mut models) = call_history_models("targeted-chat-call", 3, 0);
        let (ana, ada, sibling) = (models[0].me(), models[1].me(), models[3].me());
        let seat = models[0].state.seat_of(&ana).unwrap();
        let target = models[0].state.seat_of(&ada).unwrap();
        let session = [7; 16];
        let open = |session| Content::Session(SessionOp::Open { town_wide: false, session, mode: SessionMode::Call { video: false }, to: vec![target] });
        let leave = |session| Content::Session(SessionOp::Leave { session, secs: Some(10), why: LeaveWhy::Ended });
        models[1].sync_start(&ana);
        record_call_event(&mut models, 0, open(session));
        record_call_event(&mut models, 0, leave(session));
        let rows = |m: &Model| Workspace::read(m, false, &LocalClock::default()).rows.into_iter()
            .filter_map(|r| if let ChatRow::Call(c) = r { Some(c) } else { None }).collect::<Vec<_>>();
        let check_callback = |m: &Model, enabled| {
            let calls = rows(m);
            let [row] = calls.as_slice() else { panic!("Ada has exactly one unanswered card call") };
            assert_eq!(row.words, "Missed call from Ana", "only the rung seat gets the missed-call label");
            assert_eq!(row.call_back.as_ref().map(|c| (c.seat, c.peer.as_str(), c.enabled)),
                Some((seat, "Ana", enabled)), "the target is Ana's seat, reachable through any synced computer on it");
        };
        check_callback(&models[1], true);
        for i in [2, 3] { assert!(rows(&models[i]).is_empty(), "a bystander and the caller's sibling get no missed row"); }
        models[1].end_carrier(ana, false);
        models[1].sync_start(&sibling);
        check_callback(&models[1], true);
        models[1].end_carrier(sibling, false);
        check_callback(&models[1], false);

        let spoken = [8; 16];
        record_call_event(&mut models, 0, open(spoken));
        record_call_event(&mut models, 1, Content::Session(SessionOp::Join { session: spoken }));
        record_call_event(&mut models, 1, Content::Said { session: spoken, lines: vec![SaidLine {
            at_ms: 0, dur_ms: 10, text: "On my way".into(),
        }] });
        for who in [0, 1] { record_call_event(&mut models, who, leave(spoken)); }
        let bystander_rows = rows(&models[2]);
        let [row] = bystander_rows.as_slice() else { panic!("only the transcribed call is visible to Ben") };
        assert!(row.words.starts_with("Ana called Ada · ") && !row.words.contains("Missed call") && row.call_back.is_none(),
            "Ben sees who talked, without a missed-call label or a callback");
    }

    #[test]
    fn call_words_follow_the_viewers_seat() {
        use makepad_town_core::{event::{Failure, Service}, fold::{Job, JobState}};
        let job = Job { job: [1; 16], service: Service::Summary, requester: [1; 65], provider: [2; 65],
            project: None, request: [1; 32], state: JobState::Asked };
        for (state, present, ahead, expected) in [
            (JobState::Asked, true, 2, "Kit asked for a summary · Ana's computer, 2 ahead"),
            (JobState::Asked, true, 0, "Kit asked for a summary · Ana's computer"),
            (JobState::Working, true, 0, "Ana's computer is summarising…"),
            (JobState::Asked, false, 0, "Ana's computer is off; it continues when it's back"),
            (JobState::Working, false, 0, "Ana's computer is off; it continues when it's back"),
            (JobState::Done("<plain>".into()), false, 0, "Summary by Ana's computer\n<plain>"),
            (JobState::Failed(Failure::Interrupted), true, 0, "Ana's computer couldn't summarise: stopped before it finished"),
            (JobState::Failed(Failure::NotOffered), true, 0, "Ana's computer couldn't summarise: not offered any more"),
            (JobState::Failed(Failure::NoTranscript), true, 0, "Ana's computer couldn't summarise: no transcript or chat lines"),
            (JobState::Failed(Failure::NoModel("Install a model".into())), true, 0, "Ana's computer couldn't summarise: Install a model"),
            (JobState::Failed(Failure::ProjectGone), true, 0, "The project is gone; nothing was proposed."),
        ] {
            let job = Job { state, ..job.clone() };
            assert_eq!(JobLine::read(&job, "Kit", "Ana's computer", present, ahead).words, expected,
                "the summary row reflects {:?}, presence {present}, and {ahead} earlier requests", job.state);
        }
        let update = Job { service: Service::Tasks, ..job.clone() };
        for (state, present, ahead, expected) in [
            (JobState::Asked, true, 2, "Kit asked to update tasks · Ana's computer, 2 ahead"),
            (JobState::Working, true, 0, "Ana's computer is updating tasks…"),
            (JobState::Working, false, 0, "Ana's computer is off; it continues when it's back"),
            (JobState::Done("Proposed 2 tasks".into()), true, 0, "Tasks by Ana's computer: Proposed 2 tasks"),
            (JobState::Failed(Failure::NoModel("Install a model".into())), true, 0, "Ana's computer couldn't update tasks: Install a model"),
            (JobState::Failed(Failure::ProjectGone), true, 0, "The project is gone; nothing was proposed."),
        ] {
            let job = Job { state, ..update.clone() };
            assert_eq!(JobLine::read(&job, "Kit", "Ana's computer", present, ahead).words, expected,
                "the task update row reflects {:?}, presence {present}, and {ahead} earlier requests", job.state);
        }
        let (ana, kit, other) = ([1; 32], [2; 32], [3; 32]);
        let answered = Outcome::Answered { seats: vec![kit], secs: Some(720) };
        let declined = Outcome::Declined { seats: vec![kit], busy: false };
        for (outcome, viewer, me, expected, missed) in [
            (answered.clone(), Seen::Opener, ana, "You called · 12 min", false),
            (answered.clone(), Seen::Rung, kit, "Ana called · 12 min", false),
            (answered.clone(), Seen::Rung, other, "Missed call from Ana", true),
            (answered, Seen::Other, other, "Ana called Kit · 12 min", false),
            (declined.clone(), Seen::Opener, ana, "Kit declined", false),
            (Outcome::Declined { seats: vec![kit], busy: true }, Seen::Opener, ana, "Kit was busy", false),
            (declined.clone(), Seen::Rung, kit, "You declined", false),
            (declined.clone(), Seen::Rung, other, "Missed call from Ana", true),
            (Outcome::Declined { seats: vec![kit], busy: true }, Seen::Other, other, "Ana called Kit · declined", false),
            (declined, Seen::Other, other, "Ana called Kit · declined", false),
            (Outcome::Unanswered, Seen::Opener, ana, "No answer", false),
            (Outcome::Unanswered, Seen::Rung, kit, "Missed call from Ana", true),
            (Outcome::Unanswered, Seen::Other, other, "Ana called Kit · no answer", false),
            (Outcome::Pending, Seen::Opener, ana, "You called Kit", false),
            (Outcome::Pending, Seen::Rung, kit, "Ana called", false),
            (Outcome::Pending, Seen::Other, other, "Ana called Kit", false),
        ] {
            let s = SessionSummary { town_wide: false, opener: [0; 65], opener_seat: Some(ana), viewer, outcome,
                mode: SessionMode::Call { video: false }, rung: vec![kit], lines: 1, ended: false };
            let words = call_words(&s, Some(me), if me == ana { "You" } else { "Ana" }, |_| "Kit".into());
            assert_eq!(words, (expected.into(), missed), "{viewer:?} reads {:?}", s.outcome);
        }
    }

    #[cfg(unix)]
    #[test]
    fn trace_rows_project_receipts_and_fold_real_output() {
        use crate::ai_trace::Ink;
        use crate::model::tests::{exchange, shared_town};
        use crate::tools::AgentWaits;
        use makepad_town_core::queue::Credential;
        let clock = LocalClock { initial: Some(0), changes: vec![] };
        let binding = Credential { town: [1; 32], check: [2; 32] };
        let (agent, line) = ([3; 32], [4; 32]);
        let mut traces = Traces::default();
        let read = |traces: &Traces, now, expanded| TraceRow::read(&traces.rows(&line)[0], now, &clock, expanded);
        traces.offer(binding, agent, "Your AI", line, 0, 0);
        let sent = read(&traces, 1_999, false);
        assert_eq!((sent.phase, sent.headline.as_str(), sent.output.len()), (Phase::Sent, "sent", 0), "sending supplies only the dim mark");
        let pending = read(&traces, 40_000, false);
        assert_eq!((pending.phase, pending.headline.as_str(), pending.spin_until), (Phase::NotPickedUp, "Your AI hasn't picked this up · 40 s", None), "an uncollected line warns without motion");
        traces.pickup(binding, agent, "Your AI", &[line], 41_000);
        let got = read(&traces, 41_000, false);
        assert_eq!((got.phase, got.headline.as_str(), got.fold_count), (Phase::GotIt, "Your AI got it · 00:00:41", 0), "pickup shows the named receipt to the second");
        for (text, ink) in [("read board", Ink::Action), ("added step", Ink::Action), ("drafting", Ink::Status), ("item locked", Ink::Warning)] {
            traces.output(binding, text.into(), ink, 42_000);
        }
        let work = read(&traces, 42_010, false);
        assert_eq!((work.phase, work.headline.as_str(), work.spin_until), (Phase::Working, "working", Some(50_000)), "a fresh tool signal gives the spinner a real deadline");
        assert_eq!(work.output.iter().map(|l| (l.text.as_str(), l.ink)).collect::<Vec<_>>(),
            [("drafting", Ink::Status), ("item locked", Ink::Warning)], "two newest lines keep their order and ink");
        assert_eq!((work.fold_count, work.fold_label()), (2, "2 earlier".into()), "the live disclosure counts only the hidden lines");
        let opened = read(&traces, 42_100, true);
        assert!(opened.expanded && opened.output.len() == 4 && opened.output[0].ink == Ink::Action, "opening reveals the earlier actions in order");
        let quiet = read(&traces, 50_000, false);
        assert_eq!((quiet.phase, quiet.spin_until, quiet.headline.as_str()), (Phase::Quiet, None, "working · 9 s since it got it · last: item locked, 8 s ago"), "quiet stops motion and reports both elapsed times");
        let silent = read(&traces, 162_000, false);
        assert_eq!((silent.phase, silent.spin_until, silent.headline.as_str()), (Phase::NoSign, None, "No sign from Your AI for 2 min"), "prolonged silence has its own headline");
        traces.finish(binding, Phase::Done);
        let done = read(&traces, 163_000, false);
        assert_eq!((done.phase, done.headline.as_str(), done.output.len(), done.fold_label()), (Phase::Done, "done", 0, "what it did (4)".into()), "completion folds all output into its history");
        assert_eq!(read(&traces, 163_000, true).output.len(), 4, "completed history can be reopened");
        let other = [5; 32];
        traces.pickup(binding, agent, "Your AI", &[other], 170_000);
        traces.output(binding, "read board".into(), Ink::Action, 170_001);
        traces.finish(binding, Phase::Away);
        let away = TraceRow::read(&traces.rows(&other)[0], 170_002, &clock, false);
        assert_eq!((away.phase, away.headline.as_str(), away.output.len(), away.fold_label()), (Phase::Away, "Your AI left before answering", 0, "what it did (1)".into()), "departure preserves the folded activity");

        for (owner, live, away, working, expected) in [
            (true, true, false, false, Presence::Here),
            (true, false, false, false, Presence::Here),
            (true, false, false, true, Presence::Working),
            (true, false, true, false, Presence::Away),
            (false, false, false, false, Presence::Away),
        ] {
            let row = AgentRow { id: agent, tone: None, name: String::new(), owner, status: String::new(), away, live, working };
            assert_eq!(agent_presence(&row), expected, "owner={owner}, live={live}, away={away}, working={working}");
        }

        let (homes, mut model, mut peer) = shared_town("agent-presence");
        let town = model.open_id();
        peer.open_town(town).unwrap();
        model.set_person_name("Ada").unwrap();
        let code = Credential::parse(&model.agent_code(None).unwrap()).unwrap();
        let (token, agent) = model.agent_join_with_id(code, true).unwrap();
        let binding = Credential::parse(&token).unwrap();
        let mut waits = AgentWaits::default();
        waits.joined(&mut model);
        let snapshot = |model: &Model, waits: &AgentWaits, now| {
            let workspace = Workspace::read_chat_at(model, false, &clock, waits, &Default::default(), now, &Default::default());
            let members = TownMembers::read(model, &ConnectView::default(), &workspace, &clock, now / 1000);
            (workspace, members)
        };
        let assert_seat = |members: &TownMembers, presence, status: &str| {
            let seat = members.seats.iter().find(|s| s.id == MemberId::Agent(agent)).unwrap();
            assert_eq!((seat.presence, seat.status.as_str()), (presence, status), "the AI chip and card share the projected status");
        };
        assert!(waits.wait(&mut model, "held", binding, None).is_empty());
        assert_seat(&snapshot(&model, &waits, crate::model::wall_ms()).1, Presence::Here, "listening");
        exchange(&mut model, &mut peer);
        assert_seat(&snapshot(&peer, &AgentWaits::default(), crate::model::wall_ms()).1, Presence::Away, "on Ada's computer");

        let line = model.mention_agent(agent, "read board".into(), None).unwrap();
        waits.offered(&model, line);
        let delivery = waits.deliver(&mut model, None);
        assert_eq!(delivery[0].call_id(), "held");
        let page = makepad_strict_json::parse(delivery[0].text_for_test().as_bytes()).unwrap();
        let cursor = makepad_town_core::parse_id(page.get("last").unwrap().as_str().unwrap()).unwrap();
        let pickup = waits.traces.current(binding).unwrap().pickup.unwrap();
        let (mut workspace, mut members) = snapshot(&model, &waits, pickup);
        assert_seat(&members, Presence::Here, "answering…");
        assert!(!workspace.connect.rows[0].working && !workspace.connect.rows[0].away, "pickup alone is still");

        waits.status(binding, "reading".into(), None, None).unwrap();
        let signal = waits.traces.current(binding).unwrap().output.last().unwrap().at;
        workspace.refresh_trace(&waits.traces, binding, signal, &clock, &Default::default(), false);
        assert!(members.sync_agents(&workspace.connect.rows), "fresh output changes the member row");
        assert_seat(&members, Presence::Working, "working");
        assert_seat(&snapshot(&model, &waits, signal).1, Presence::Working, "working");
        let quiet = signal + 8_000;
        workspace.refresh_trace(&waits.traces, binding, quiet, &clock, &Default::default(), true);
        assert!(members.sync_agents(&workspace.connect.rows), "the quiet deadline stops the working dot");
        assert!(!workspace.connect.rows[0].working);
        assert_seat(&members, Presence::Here, &waits.traces.current(binding).unwrap().headline(quiet, &clock));
        assert!(!members.sync_agents(&workspace.connect.rows), "repeating the same projection needs no redraw");

        let reply = waits.say(&mut model, binding, "done".into(), Some(line)).unwrap();
        let replied = clock.hm(model.state.message(&reply).unwrap().wall_ms);
        let (workspace, members) = snapshot(&model, &waits, quiet);
        assert!(workspace.connect.rows[0].away && !workspace.connect.rows[0].working);
        assert_seat(&members, Presence::Away, &format!("away, last heard {replied}"));
        assert_eq!(waits.traces.next_change(town, quiet), None, "the finished reply leaves no trace wake");
        assert!(waits.wait(&mut model, "again", binding, Some(cursor)).is_empty(), "acknowledging the delivered page holds the next wait");
        assert_seat(&snapshot(&model, &waits, quiet).1, Presence::Here, "listening");
        exchange(&mut model, &mut peer);
        assert_seat(&snapshot(&peer, &AgentWaits::default(), quiet).1, Presence::Away, &format!("replied {replied}"));

        // A transfer for the same blob in a different town must leave the displayed clip alone.
        let (signal, wake) = std::sync::mpsc::channel();
        model.start_store(crate::store_worker::test_pool(), move || { let _ = signal.send(()); }).unwrap();
        let path = homes.0.join("clip.gif");
        std::fs::write(&path, b"GIF89a\x01\0\x01\0").unwrap();
        model.post_dropped(town, path).unwrap();
        while model.clips_pending() {
            model.drain_store();
            if model.clips_pending() { wake.recv().unwrap(); }
        }
        model.stop_store();
        while !model.store_stopped() {
            model.drain_store();
            if !model.store_stopped() { wake.recv().unwrap(); }
        }
        exchange(&mut model, &mut peer);
        let mut workspace = Workspace::read(&peer, false, &clock);
        let (index, clip) = workspace.rows.iter().enumerate().find_map(|(index, row)| match row {
            ChatRow::Clip(clip) => Some((index, clip.clone())), _ => None,
        }).expect("the peer sees the posted clip");
        let blob = clip.media.as_ref().unwrap().blob;
        peer.get_clip(town, blob).unwrap();
        let other = peer.towns().iter().find(|t| t.id != town).unwrap().id;
        assert!(!workspace.refresh_clips(&peer, [(other, blob)], false), "another town's transfer cannot refresh this clip");
        assert!(matches!(&workspace.rows[index], ChatRow::Clip(row) if Arc::ptr_eq(row, &clip)), "the displayed row is untouched");
        assert!(workspace.refresh_clips(&peer, [(town, blob)], false), "the open town's transfer shows the pending request");
    }

    #[cfg(unix)]
    #[test]
    fn a_copy_command_uses_an_installed_helper_across_jit_runs() {
        use std::{fs, os::unix::fs::{symlink, PermissionsExt}};
        let mut nonce = [0; 8];
        makepad_network::tls::os_random(&mut nonce).unwrap();
        let root = std::env::temp_dir().join(format!("work-command-{}", makepad_town_core::hex(&nonce)));
        let (run, install, path) = (root.join("run"), root.join("install"), root.join("person's bin"));
        for dir in [&run, &install, &path] { fs::create_dir_all(dir).unwrap(); }
        let fake = run.join("makepad-remote");
        fs::write(&fake, []).unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).unwrap();
        let loader = install.join("loader");
        fs::write(&loader, []).unwrap();
        let town = path.join("Town");
        let native = find_remote_binary(Some(town.clone()), None, install.as_os_str(), |_| None);
        assert_eq!(native, Some(RemoteBinary::Town(town)), "an installation without a helper uses its own executable");
        let cmd = tool_command(&native.unwrap(), &root.join("endpoint"), false, "town.join",
            makepad_strict_json::obj(vec![])).unwrap();
        assert!(cmd.starts_with('\'') && cmd.contains(r"/person'\''s bin/Town' remote app '"),
            "the invitation quotes Town's executable as one shell word before remote app");
        let resolve = || find_remote_binary(Some(run.join("work")), Some(loader.clone()), path.as_os_str(), installed_executable);
        assert!(resolve().is_none(), "an earlier CLI run left a launcher, not an installed helper");
        let installed = path.join("makepad-remote");
        symlink(&fake, &installed).unwrap();
        assert_eq!(resolve(), Some(RemoteBinary::Helper(installed.clone())), "PATH preserves the executable's entry name");
        let native = find_remote_binary(Some(install.join("Town")), None, path.as_os_str(), installed_executable);
        assert_eq!(native, Some(RemoteBinary::Helper(installed.clone())), "a helper on PATH wins over the installed Town");
        let beside = install.join("makepad-remote");
        symlink(&fake, &beside).unwrap();
        assert_eq!(resolve(), Some(RemoteBinary::Helper(beside)), "a helper beside the installation takes precedence");
        let cmd = tool_command(&RemoteBinary::Helper(installed), &root.join("endpoint"), true, "town.wait",
            makepad_strict_json::obj(vec![("binding", makepad_strict_json::s("example"))])).unwrap();
        assert!(cmd.contains(r"person'\''s bin") && cmd.contains(" --reconnect call town.wait "));
        fs::remove_dir_all(root).unwrap();
    }

    fn append_as(log: &mut makepad_town_core::log::Log, key: &makepad_town_core::key::DeviceKey,
        actor: Actor, content: makepad_town_core::event::Content) -> Id {
        use makepad_town_core::event::{Content, Draft, Event, Via};
        let event = Event::create(key, log.town_id(), Draft {
            parents: log.parents_for(&key.public()), content: &content.encode(), kind: content.kind(),
            kind_version: content.version(), critical: matches!(content, Content::Member(_)),
            actor, via: if actor == Actor::Ai { Via::Tool } else { Via::Ui }, wall_ms: crate::model::wall_ms(),
        }, [0; 16]).unwrap();
        let id = event.id;
        log.append(event).unwrap();
        id
    }

    fn add_seat_siblings(ana: Model, ben: &mut Model,
        ana2: &makepad_town_core::key::DeviceKey, ben2: &makepad_town_core::key::DeviceKey) -> Model {
        use makepad_town_core::{event::{Content, Member}, link::VERSION, log::Log};
        let town = ana.open_id();
        let home = ana.home.dir.clone();
        let founder_seat = ana.state.seat_of(&ana.me()).unwrap();
        let seat_b = ana.state.seat_of(&ben.me()).unwrap();
        let path = ana.home.town_log_path(&town);
        let ana_key = ana.device_key();
        let ben_key = ben.device_key();
        ana.home.set_open_town(&town).unwrap();
        drop(ana);

        let mut log = Log::open(&path).unwrap().0;
        let mut add = |key, content| append_as(&mut log, key, Actor::Human, content);
        for (seat, signer, device, name) in [(founder_seat, &ana_key, ana2, "Ana2"), (seat_b, &ben_key, ben2, "Ben2")] {
            let entry = add(signer, Content::Member(Member::AddDevice { seat, device: device.public() }));
            add(device, Content::Member(Member::Accept { entry }));
            add(device, Content::Profile { name: name.into(), colour: 0 });
        }
        drop(log);
        let mut ana = Model::open(&home).unwrap().0;
        ana.sync_start(&ben.me());
        ben.sync_start(&ana.me());
        crate::model::tests::exchange(&mut ana, ben);
        ana.pin_linked(&ben2.public(), VERSION, "Ben2").unwrap();
        ana
    }

    #[test]
    fn seat_tones_are_per_seat_collision_free_and_theme_contrast_safe() {
        use crate::model::tests::ShareHomes;
        use makepad_town_core::{event::{Chat, Content, Member}, key::DeviceKey, log::Log};
        let homes = ShareHomes::new("seat-tones");
        let open = || Model::open(&homes.0).unwrap().0;
        let model = open();
        let founder = model.device_key();
        let own = model.state.seat_of(&model.me()).unwrap();
        let project = model.project().id;
        model.home.set_open_town(&model.open_id()).unwrap();
        let path = model.home.town_log_path(&model.open_id());
        drop(model);

        let key = || DeviceKey::generate().unwrap();
        let (first, second, sibling, my_sibling) = (key(), key(), key(), key());
        let mut log = Log::open(&path).unwrap().0;
        let admit = |log: &mut Log, device: &DeviceKey| {
            let entry = append_as(log, &founder, Actor::Human, Content::Member(Member::Invite { subject: device.public(), role: 0 }));
            append_as(log, device, Actor::Human, Content::Member(Member::Accept { entry }));
            entry
        };
        let a = admit(&mut log, &first);
        let b = admit(&mut log, &second);
        for (seat, owner, device) in [(a, &first, &sibling), (own, founder.as_ref(), &my_sibling)] {
            let entry = append_as(&mut log, owner, Actor::Human, Content::Member(Member::AddDevice { seat, device: device.public() }));
            append_as(&mut log, device, Actor::Human, Content::Member(Member::Accept { entry }));
        }
        let profile = |colour| Content::Profile { name: "Member".into(), colour };
        append_as(&mut log, &first, Actor::Human, profile(1));
        append_as(&mut log, &sibling, Actor::Human, profile(3));
        append_as(&mut log, &second, Actor::Human, profile(3));
        let agent = append_as(&mut log, &sibling, Actor::Human, Content::AgentAdded { name: "Helper".into(), kind: "session".into() });
        let mut authors = Vec::new();
        for (device, actor, seat) in [(&first, Actor::Human, Seat::Peer), (&sibling, Actor::Human, Seat::Peer),
            (&sibling, Actor::Ai, Seat::PeerAi), (&my_sibling, Actor::Human, Seat::You)] {
            let chat = append_as(&mut log, device, actor, Content::Chat(Chat { text: "hello".into(), ..Chat::default() }));
            let task = append_as(&mut log, device, actor, Content::Entity { kind: Key::Number(kind::TASK), project: Some(project),
                derive: None, fields: vec![(field::TITLE, Value::Text("Review".into()))], copied_from: vec![] });
            authors.push((seat, chat, task));
        }
        drop(log);
        let model = open();
        let pair = seat_tones(&model);
        assert_eq!(pair, BTreeMap::from([(a, SeatTone(2)), (b, SeatTone(3))]), "the newest nonzero profile wins and the second seat steps forward");
        let clock = LocalClock::default();
        let ws = Workspace::read(&model, false, &clock);
        let members = TownMembers::read(&model, &ConnectView::default(), &ws, &clock, 0);
        for device in [&first, &sibling] { assert_eq!(members.seat_tone_for_device(&device.public()), Some(pair[&a])); }
        assert_eq!(members.seat_tone_for_device(&my_sibling.public()), None);
        let ai = members.seats.iter().find(|s| s.id == MemberId::Agent(agent)).unwrap();
        assert_eq!((ai.seat, ai.tone), (Seat::PeerAi, Some(pair[&a])));
        for (seat, chat, task) in authors {
            let tone = (seat != Seat::You).then_some(pair[&a]);
            let message = ws.rows.iter().find_map(|r| match r { ChatRow::Message(m) if m.id == chat => Some(m), _ => None }).unwrap();
            let card = ws.selected().unwrap().columns.iter().flatten().find(|c| c.id == task).unwrap();
            assert_eq!((message.seat, message.tone), (seat, tone), "chat author");
            assert_eq!((card.seat, card.tone), (seat, tone), "task author");
        }
        drop(model);
        let mut log = Log::open(&path).unwrap().0;
        for _ in 2..SEAT_SLOTS { admit(&mut log, &key()); }
        drop(log);
        let expanded = seat_tones(&open());
        assert_eq!((expanded[&a], expanded[&b]), (pair[&a], pair[&b]), "joining peers preserve earlier colours");
        let mut log = Log::open(&path).unwrap().0;
        for device in [&first, &sibling, &second] { append_as(&mut log, device, Actor::Human, profile(0)); }
        drop(log);
        let mut slots: Vec<_> = seat_tones(&open()).values().map(|t| t.0).collect();
        slots.sort_unstable();
        assert_eq!(slots, (0..SEAT_SLOTS as u8).collect::<Vec<_>>(), "eight default peers use every slot");
        for (colour, slot) in [(0x3080ff, 6), (0xff8030, 0), (0x303033, 3)] {
            assert_eq!(preferred_seat_slot(a, colour), slot, "legacy RGB {colour:#x}");
        }
        crate::theme::tests::check_seat_contrast();
    }

    fn check_two_device_agent_names(profiles: [Option<&str>; 2], before: &str,
        change: impl FnOnce(&mut Model, [PublicKey; 2]), after: &str) {
        use crate::{lists::mention_hint_text, model::{tests::ShareHomes, MentionTarget}};
        use makepad_town_core::{event::{Chat, ChatAgent, Content, LeaveWhy, Member, SessionOp}, key::DeviceKey, log::Log};
        let homes = ShareHomes::new("agent-seat-name");
        let open = || Model::open(&homes.0).unwrap().0;
        let mut ana = open();
        ana.set_person_name("Ana").unwrap();
        let founder = ana.device_key();
        let ana_seat = ana.state.seat_of(&ana.me()).unwrap();
        ana.home.set_open_town(&ana.open_id()).unwrap();
        let path = ana.home.town_log_path(&ana.open_id());
        drop(ana);
        let mut keys = [DeviceKey::generate().unwrap(), DeviceKey::generate().unwrap()];
        keys.sort_by_key(|key| key.public());
        let [ben2, ben1] = keys;
        let mut log = Log::open(&path).unwrap().0;
        let seat = append_as(&mut log, &founder, Actor::Human, Content::Member(Member::Invite { subject: ben1.public(), role: 0 }));
        append_as(&mut log, &ben1, Actor::Human, Content::Member(Member::Accept { entry: seat }));
        let entry = append_as(&mut log, &ben1, Actor::Human, Content::Member(Member::AddDevice { seat, device: ben2.public() }));
        append_as(&mut log, &ben2, Actor::Human, Content::Member(Member::Accept { entry }));
        for (device, name) in [&ben1, &ben2].into_iter().zip(profiles) {
            if let Some(name) = name {
                append_as(&mut log, device, Actor::Human, Content::Profile { name: name.into(), colour: 0 });
            }
        }
        let agent = append_as(&mut log, &ben2, Actor::Human, Content::AgentAdded { name: "Fab Bot".into(), kind: "session".into() });
        let reply = append_as(&mut log, &ben2, Actor::Ai, Content::AgentChat { agent: ChatAgent::Author(agent),
            chat: Chat { text: "The draft is ready".into(), ..Default::default() } });
        let session = [8; 16];
        append_as(&mut log, &ben2, Actor::Human, Content::Session(SessionOp::Open { town_wide: false, session, mode: SessionMode::Call { video: false }, to: vec![ana_seat] }));
        append_as(&mut log, &ben2, Actor::Human, Content::Session(SessionOp::Leave { session, secs: None, why: LeaveWhy::Ended }));
        drop(log);
        let mut ana = open();
        let quote = ana.post_chat_draft("Thanks", None, None, Some(reply)).unwrap();
        let check = |ana: &Model, owner: &str| {
            let short = format!("{owner}'s AI");
            let full = format!("{short} · Fab Bot");
            assert_eq!(ana.agent_short_label(agent), short);
            assert_eq!(ana.agent_display_label(agent), full);
            let clock = LocalClock::default();
            let ws = Workspace::read(ana, false, &clock);
            let members = TownMembers::read(ana, &Default::default(), &ws, &clock, 0);
            let chip = |id| &members.seats.iter().find(|s| s.id == id).unwrap().name;
            assert_eq!(chip(MemberId::Seat(seat)), owner);
            assert_eq!(chip(MemberId::Agent(agent)), &full);
            let devices: Vec<_> = [ben1.public(), ben2.public()].into_iter().filter(|k| ana.state.active_member(k)).collect();
            assert_eq!(seat_label(ana, &devices), owner);
            let message = |id| ws.rows.iter().find_map(|r| match r { ChatRow::Message(m) if m.id == id => Some(m), _ => None }).unwrap();
            assert_eq!(message(reply).name, full);
            assert_eq!(message(quote).quote.as_ref().unwrap().name, full);
            use crate::model::{BoundMention, ComposerTarget};
            assert_eq!(ana.mention_target("@Fab Bot check"), MentionTarget::UnboundAi);
            let bound = BoundMention { range: 0..8, token: "@Fab Bot".into(), target: ComposerTarget::Agent(agent) };
            let target = ana.mention_target_bound("@Fab Bot check", &[bound]);
            assert_eq!(target, MentionTarget::PeerAi { agent, label: full.clone(), owner_name: owner.into() });
            assert_eq!(mention_hint_text(&target), Some(format!("Goes to {full} ({owner} approves first)")));
            let call = ws.rows.iter().find_map(|r| match r { ChatRow::Call(c) if c.id.1 == session => Some(c), _ => None }).unwrap();
            assert_eq!(call.words, format!("Missed call from {owner}"));
            assert_eq!(call.call_back.as_ref().unwrap().peer, owner);
        };
        check(&ana, before);
        change(&mut ana, [ben1.public(), ben2.public()]);
        check(&ana, after);
    }

    #[test]
    fn agent_display_labels_use_canonical_seat_name_across_two_device_seat() {
        check_two_device_agent_names([Some("Ben"), Some("Ben Laptop")], "Ben",
            |ana, [first, _]| ana.remove_device(first, false).unwrap(), "Ben Laptop");
    }

    #[test]
    fn agent_display_labels_agree_with_seat_chip_on_no_profile_fallback() {
        check_two_device_agent_names([Some("   "), None], "Peer",
            |ana, [_, second]| ana.pin_linked(&second, makepad_town_core::link::VERSION, "Ben").unwrap(), "Ben");
    }

    #[test]
    fn seats_collapse_multiple_devices_and_pick_best_presence() {
        use crate::link_worker::{FromWorker, Status as LinkStatus};
        use crate::model::tests::{join, ShareHomes};
        use makepad_town_core::{hub::{Attempt, Carrier}, key::DeviceKey, link::VERSION};
        let homes = ShareHomes::new("seat-collapse");
        let mut ana = Model::open(&homes.0.join("ana")).unwrap().0;
        let mut ben = Model::open(&homes.0.join("ben1")).unwrap().0;
        ana.pin_linked(&ben.me(), VERSION, "Ben").unwrap();
        ben.pin_linked(&ana.me(), VERSION, "Ana").unwrap();
        ana.sync_start(&ben.me());
        ben.sync_start(&ana.me());
        ana.set_person_name("Ana").unwrap();
        ben.set_person_name("Ben").unwrap();
        join(&mut ana, &mut ben, false);
        let founder_seat = ana.state.seat_of(&ana.me()).unwrap();
        let seat_b = ana.state.seat_of(&ben.me()).unwrap();
        let ana2 = DeviceKey::generate().unwrap();
        let ben2 = DeviceKey::generate().unwrap();
        let mut ana = add_seat_siblings(ana, &mut ben, &ana2, &ben2);
        let now = crate::model::wall_ms() / 1000 + 60;
        let read = |model: &Model, link: &ConnectView| TownMembers::read_with_quiet(model, link,
            &RetryQuiet::default(), &Workspace::default(), &LocalClock::default(), now);
        let mut link = ConnectView::default();
        let members = read(&ana, &link);
        let ids = |m: &TownMembers| m.seats.iter().map(|s| s.id).collect::<Vec<_>>();
        assert_eq!(ids(&members), [MemberId::You, MemberId::Seat(seat_b)]);
        let you = &members.seats[0];
        assert_eq!((you.presence, you.status.as_str()), (Presence::Here, "2 computers"));
        let [local, sibling] = you.devices.as_slice() else { panic!("both founder devices belong under You") };
        assert!(local.is_me && local.key == ana.me() && local.status == "this computer");
        assert_eq!(sibling.key, ana2.public());
        let peer = &members.seats[1];
        assert_eq!((peer.name.as_str(), peer.presence, peer.status.as_str()), ("Ben", Presence::Here, "here · 2 computers"));
        let [first, second] = peer.devices.as_slice() else { panic!("both devices belong under Ben") };
        assert_eq!((first.key, first.presence, first.status.as_str()), (ben.me(), Presence::Here, "here"));
        assert_eq!((second.key, second.presence), (ben2.public(), Presence::Away));
        assert!(second.status.starts_with("away · last seen "));

        assert_eq!(seat_label(&ana, &[first.key, second.key]), "Ben");
        assert_eq!(seat_label(&ana, &[]), "Peer");
        let names = ["Kit", "Lu", "Mira", "Rik", "Sora"].map(str::to_owned);
        for (count, who, button) in [
            (1, "Kit", "Remove both"), (2, "Kit and Lu", "Remove all 3"),
            (3, "Kit, Lu and Mira", "Remove all 4"), (5, "Kit, Lu and 3 others", "Remove all 6"),
        ] {
            assert_eq!(cascade_question("Ben", &names[..count]), (
                format!("Also remove {who}, who joined through Ben?"), button.into(), "Only Ben".into()));
        }
        for (computers, cascade_count, expected) in [
            (1, 0, "Removing Ben — that device can't rejoin this town."),
            (2, 0, "Removing Ben (2 computers) — those devices can't rejoin this town."),
            (2, 1, "Removing Ben and Kit — those devices can't rejoin this town."),
            (1, 2, "Removing Ben, Kit and Lu — those devices can't rejoin this town."),
            (2, 3, "Removing Ben and 3 others — those devices can't rejoin this town."),
        ] {
            assert_eq!(removing_seat_line("Ben", computers, &names[..cascade_count]), expected);
        }

        ana.end_carrier(ben.me(), true);
        let at = Some(Attempt { carrier: Carrier::Peer(ben2.public()), generation: 1 });
        link.apply_attempt(at, &FromWorker::Reconnecting { device: ben2.public(), since: now - 20 }, true);
        let peer = &read(&ana, &link).seats[1];
        assert_eq!((peer.presence, peer.status.as_str()), (Presence::Reaching, "reaching… · 2 computers"));
        link.apply_attempt(at, &FromWorker::Phase(LinkStatus::Backoff { retry_at: now + 17 }), true);
        let peer = &read(&ana, &link).seats[1];
        assert_eq!((peer.presence, peer.status.as_str()), (Presence::Away, "away · 2 computers"));
        ana.peer_version(Some(ben2.public()), VERSION + 1);
        let peer = &read(&ana, &link).seats[1];
        assert_eq!((peer.presence, peer.status.as_str()), (Presence::Away, "your Town is older · 2 computers"));
        assert_eq!(peer.devices[1].status, "your Town is older");

        ana.leave_town().unwrap();
        let first = &ana.seats_in_order()[0];
        assert!(first.is_mine && first.seat == founder_seat);
        let members = read(&ana, &link);
        assert_eq!(ids(&members), [MemberId::You, MemberId::Seat(seat_b)]);
        let you = &members.seats[0];
        assert_eq!((you.presence, you.status.as_str()), (Presence::Away, "away · 1 other computer"));
        let [sibling] = you.devices.as_slice() else { panic!("only the other founder device remains") };
        assert!(sibling.key == ana2.public() && !sibling.is_me && sibling.presence == Presence::Away);
    }

    #[test]
    fn live_roster_maps_multi_peer_calls_shares_and_multi_device_seats() {
        use crate::call::{CallController, CallEnv, CallShown, Message, ParticipantShown, Phase, ShareOfferExt, ShareRole};
        use makepad_town_core::share::hello::MediaCaps;
        use crate::link_worker::FromWorker;
        use makepad_town_core::{hub::{Attempt, Carrier}, key::DeviceKey};

        let (homes, ana, mut ben) = crate::model::tests::shared_town("live-roster");
        let (town, me, ben1, kit1) = (ana.open_id(), ana.me(), ben.me(), [7; 65]);
        let now = crate::model::wall_ms() / 1000 + 125;
        let mut link = ConnectView::default();
        let read = |model: &Model, link: &ConnectView, roster: &LiveRoster| TownMembers::read_with_live(
            model, link, &RetryQuiet::default(), roster, &Workspace::default(), &LocalClock::default(), now);
        let project = |call: &CallShown, peers: &[PublicKey], from| {
            LiveRoster::from_call(town, me, call, peers, from)
        };
        let mut call = CallShown::default();
        Arc::make_mut(&mut call.view).town = Some(town);
        assert_eq!(project(&call, &[], None), LiveRoster::default(), "idle towns share the empty roster");

        Arc::make_mut(&mut call.view).phase = Phase::Calling;
        let dialing = project(&call, &[ben1, kit1, ben1, me], None);
        assert_eq!(dialing.town, Some(town));
        assert_eq!(dialing.by_device[&me], [LiveActivity::Calling]);
        assert_eq!(dialing.summary_for(&me).as_deref(), Some("calling…"));
        for key in [ben1, kit1] {
            assert_eq!(dialing.by_device[&key], [LiveActivity::Ringing]);
            assert_eq!(dialing.summary_for(&key).as_deref(), Some("ringing…"));
        }
        Arc::make_mut(&mut call.view).phase = Phase::Ringing;
        let incoming = project(&call, &[], Some(ben1));
        assert_eq!(incoming.primary(&ben1), Some(&LiveActivity::Calling));
        assert_eq!(incoming.summary_for(&ben1).as_deref(), Some("calling…"));
        assert_eq!(incoming.primary(&me), None, "the unanswered receiver has no live mark");

        let view = Arc::make_mut(&mut call.view);
        view.phase = Phase::Live;
        view.participants = [ben1, kit1].into_iter().map(|peer| ParticipantShown {
            peer, name: String::new(), muted: false, transcribing: false,
        }).collect();
        let voice = project(&call, &[], None);
        for key in [me, ben1, kit1] {
            assert!(voice.in_call(&key));
            assert_eq!(voice.summary_for(&key).as_deref(), Some("in call"));
        }
        let single = read(&ana, &link, &voice);
        assert_eq!(single.seats[0].status, "in call", "live text can stand alone on You");
        assert_eq!(single.seats[1].status, "here · in call", "a single computer contributes its suffix once");
        let view = Arc::make_mut(&mut call.view);
        view.duration_secs = 42;
        view.muted = true;
        view.peer_muted = true;
        assert_eq!(project(&call, &[], None), voice, "a call tick or mute leaves the member projection equal");
        let other_town = [8; 32];
        Arc::make_mut(&mut call.view).town = Some(other_town);
        assert_eq!(project(&call, &[], None), LiveRoster::default(), "a background call leaves Home idle");
        let foreign = LiveRoster { town: Some(other_town), ..voice.clone() };
        assert_eq!(read(&ana, &link, &foreign), read(&ana, &link, &LiveRoster::default()));
        Arc::make_mut(&mut call.view).town = Some(town);

        let sibling_home = homes.0.join("ana2");
        let sibling = Model::open(&sibling_home).unwrap().0;
        let second_ben_key = DeviceKey::generate().unwrap();
        let ben2 = second_ben_key.public();
        let mut ana = add_seat_siblings(ana, &mut ben, &sibling.device_key(), &second_ben_key);
        let members = read(&ana, &link, &voice);
        let ben_seat = &members.seats[1];
        assert_eq!(ben_seat.status, "here · 2 computers · in call");
        assert_eq!(ben_seat.devices[0].status, "here · in call");
        let idle = read(&ana, &link, &LiveRoster::default());
        assert!(ben_seat.devices[1].status.starts_with("away · last seen "));
        assert_eq!(ben_seat.devices[1], idle.seats[1].devices[1], "idle sibling keeps its link status");

        let mut ctl = CallController::default();
        ctl.configure(CallEnv { files: Default::default(), devices_path: homes.0.join("devices"),
            voice: makepad_call::driver::VoiceSource::Off, signal: Arc::new(|| {}),
            spawn: Arc::new(|_, _| panic!("roster projection needs no media worker")),
            jobs: Arc::new(|_, _| panic!("roster projection needs no audio job")) });
        let mut cx = makepad_widgets::Cx::new(Box::new(|_, _| {}));
        let port = ctl.link_up(&mut cx, me, ben2, "Ben2").unwrap();
        // A viewer shows only a town-tagged offer from a town member whose build shares to a town.
        ctl.set_open_town(Some((town, vec![(ben2, "Ben2".into())])));
        ctl.set_peer_caps(ben2, MediaCaps(MediaCaps::MULTIPARTY_V1 | MediaCaps::MULTIPARTY_SHARE_V1));
        let (fab, sibling_session) = ([9; 16], [10; 16]);
        for (session, title) in [(fab, "Fab"), (sibling_session, "Sibling")] {
            port.messages_in.try_send(Message::Share { session, title: title.into(),
                w: 20., h: 20., ext: ShareOfferExt { town: Some(town), ..Default::default() } }.encode()).unwrap();
        }
        ctl.poll(&mut cx);
        call.share = ctl.shown().share;
        let watching = project(&call, &[], None);
        let fab_live = LiveActivity::Sharing { session: fab, title: "Fab".into() };
        assert_eq!(watching.primary_for_devices(&ben_seat.devices), Some(&fab_live));
        assert_eq!(watching.summary_for_devices(&ben_seat.devices).as_deref(), Some("in call · sharing Fab"));
        assert_eq!(watching.share_for_devices(&ben_seat.devices), Some((ben2, fab, "Fab")));
        assert!(watching.has_call_for_devices(&ben_seat.devices));
        assert!(!watching.in_call(&ben2), "the viewing offer belongs to a different computer from the call");

        Arc::make_mut(&mut call.view).phase = Phase::Idle;
        let offers = Arc::make_mut(&mut call.share.offers);
        Arc::make_mut(&mut offers[0]).role = ShareRole::Sharer {
            link: crate::app_link::LinkId::TEST, window: crate::app_link::WindowId(1),
        };
        let sharing = project(&call, &[], None);
        let you = &read(&ana, &link, &sharing).seats[0];
        assert_eq!(you.devices[0].status, "this computer · sharing Fab");
        assert_eq!(you.status, "2 computers · sharing Fab");
        assert!(!sharing.has_call_for_devices(&you.devices));
        assert_eq!(LiveRoster::from_call(other_town, me, &call, &[], None), LiveRoster::default(), "shares in another town leave Home idle");

        ana.end_carrier(ben1, true);
        let attempt = Some(Attempt { carrier: Carrier::Peer(ben2), generation: 1 });
        link.apply_attempt(attempt, &FromWorker::Reconnecting { device: ben2, since: now - 125 }, true);
        let mut dialing = CallShown::default();
        Arc::make_mut(&mut dialing.view).town = Some(town);
        Arc::make_mut(&mut dialing.view).phase = Phase::Calling;
        let ringing = project(&dialing, &[ben2], None);
        let ben_seat = &read(&ana, &link, &ringing).seats[1];
        assert_eq!(ben_seat.devices[1].status, "reaching for 2 min · ringing…");
        assert_eq!(ben_seat.status, "reaching for 2 min · 2 computers · ringing…");

        // Open the same signed membership log as the later-joined computer.
        ana.settle_writer();
        std::fs::copy(ana.home.town_log_path(&town), sibling.home.town_log_path(&town)).unwrap();
        sibling.home.set_open_town(&town).unwrap();
        drop(sibling);
        let sibling = Model::open(&sibling_home).unwrap().0;
        assert_eq!(sibling.seats_in_order()[0].devices, [me, sibling.me()], "the model keeps canonical join order");
        Arc::make_mut(&mut Arc::make_mut(&mut call.share.offers)[1]).role = ShareRole::Viewer { sharer: me };
        let both = LiveRoster::from_call(town, sibling.me(), &call, &[], None);
        let you = &read(&sibling, &link, &both).seats[0];
        let [local, other] = &you.devices[..] else { panic!("expected both computers on You") };
        assert!(local.is_me && local.key == sibling.me());
        assert_eq!(local.status, "this computer · sharing Fab");
        assert!(other.key == me && other.status.ends_with("sharing Sibling"));
        assert_eq!(you.status, "2 computers · sharing Fab");
        assert_eq!(you.live.as_ref(), Some(&fab_live));
        assert_eq!(both.share_for_devices(&you.devices), Some((sibling.me(), fab, "Fab")));
    }

    #[test]
    fn the_table_follows_the_link_and_the_local_day() {
        let clock = LocalClock { initial: Some(3600), changes: Vec::new() };
        let last = Some(86_100);
        let table = |name, linked, update, reaching, last, now| TablePeer::read(name, linked, update, reaching, last, &clock, now);
        let live = table("Rik", Some(true), None, None, last, 86_500);
        assert_eq!(live.status, DeviceStatus::Synced);
        assert_eq!((live.status.text().as_str(), live.status.presence()), ("here", Presence::Here));
        assert_eq!(live.initial(), "R");
        let not_syncing = table(" yOu ", Some(false), None, None, last, 86_500);
        assert_eq!(not_syncing.name, "Peer");
        assert_eq!(not_syncing.status, DeviceStatus::NotSyncing);
        assert_eq!((not_syncing.status.text().as_str(), not_syncing.status.presence()), ("not syncing this town", Presence::Away));
        assert_eq!(not_syncing.refresh_after_secs, None);
        for (linked, update, reaching, expected, text) in [
            (Some(false), PeerUpdate::TheyAreOlder, None, DeviceStatus::NeedsUpdate(true), "needs a newer Town"),
            (Some(false), PeerUpdate::WeAreOlder, None, DeviceStatus::NeedsUpdate(false), "your Town is older"),
            (Some(true), PeerUpdate::TheyAreOlder, None, DeviceStatus::NeedsUpdate(true), "needs a newer Town"),
            (None, PeerUpdate::WeAreOlder, None, DeviceStatus::NeedsUpdate(false), "your Town is older"),
            (None, PeerUpdate::WeAreOlder, Some(86_480), DeviceStatus::NeedsUpdate(false), "your Town is older"),
        ] {
            let peer = table("Kit", linked, Some(update), reaching, last, 86_500);
            assert_eq!(peer.status, expected);
            assert_eq!((peer.status.text().as_str(), peer.status.presence()), (text, Presence::Away));
            assert_eq!(peer.refresh_after_secs, None);
        }
        let known = table("Kit", None, None, None, last, 86_500);
        assert_eq!(known.name, "Kit");
        assert_eq!(known.status.presence(), Presence::Away);
        assert_eq!(known.status.text(), "away · last seen 00:55", "links on the same local day show the chat time across UTC midnight");
        assert_eq!(known.refresh_after_secs, Some(169_200 - 86_500), "the time changes to a date at local midnight");
        let yesterday = table("Kit", None, None, None, last, 169_200);
        assert_eq!(yesterday.status.text(), "away · last seen 1970-01-02", "an older link shows its local calendar date");
        assert!(yesterday.refresh_after_secs.is_none(), "an older date needs no midnight refresh");

        for (secs, text, refresh) in [(20, "reaching…", 40), (60, "reaching for 1 min", 60), (125, "reaching for 2 min", 55)] {
            let reaching = table("Rik", None, None, Some(86_500 - secs), last, 86_500);
            assert_eq!(reaching.status, DeviceStatus::Reaching(secs));
            assert_eq!((reaching.status.text().as_str(), reaching.status.presence()), (text, Presence::Reaching));
            assert_eq!(reaching.refresh_after_secs, Some(refresh));
        }
        let unknown = table("", None, None, None, None, 86_500);
        assert_eq!(unknown.status, DeviceStatus::NotConnected);
        assert_eq!((unknown.status.text().as_str(), unknown.status.presence()), ("away", Presence::Away));
        assert_eq!(unknown.initial(), "P");
        for (name, initial) in [("Kit", "K"), ("rik", "R"), ("", "P")] {
            assert_eq!(peer_initial(name), initial);
        }

        use crate::link_worker::{FromWorker, Status as LinkStatus};
        let key = [7; 65];
        let mut quiet = RetryQuiet::default();
        let backoff = FromWorker::Phase(LinkStatus::Backoff { retry_at: 100 });
        assert!(!quiet.is_quiet(&key));
        quiet.on_worker_event(key, &backoff);
        assert!(quiet.is_quiet(&key));
        for event in [
            FromWorker::Phase(LinkStatus::Backoff { retry_at: 117 }),
            FromWorker::Reconnecting { device: key, since: 117 },
            FromWorker::Phase(LinkStatus::Reconnecting), FromWorker::Phase(LinkStatus::Reaching { secs: 1 }),
            FromWorker::Lost, FromWorker::Refused("refused".into()),
        ] {
            quiet.on_worker_event(key, &event);
            assert!(quiet.is_quiet(&key), "{event:?} keeps retries quiet");
        }
        for event in [
            FromWorker::Phase(LinkStatus::Securing), FromWorker::Phase(LinkStatus::Proving), FromWorker::Phase(LinkStatus::Linked),
            FromWorker::Linked { peer_device: key, version: 1, peer_name: "Kit".into(), contact: None },
            FromWorker::Phase(LinkStatus::Stopped), FromWorker::KeyChanged, FromWorker::Failed("stopped".into()),
        ] {
            quiet.on_worker_event(key, &backoff);
            assert!(quiet.is_quiet(&key));
            quiet.on_worker_event(key, &event);
            assert!(!quiet.is_quiet(&key), "{event:?} clears quiet");
        }
        quiet.on_worker_event(key, &backoff);
        quiet.clear(&key);
        assert!(!quiet.is_quiet(&key));
    }

    #[test]
    fn day_separators_follow_local_tz_midnight_and_skewed_clocks() {
        use crate::model::tests::ShareHomes;
        use ChatRowKey::{Day, Message};
        let today = 20_785; // 28 Nov 2026
        let start = today as u64 * 86_400_000;
        let hour = 3_600_000;
        assert_eq!([today, today - 1, today - 2, 20_418, 20_819].map(|d| day_label(d, today)),
            ["Today", "Yesterday", "26 Nov", "26 Nov 2025", "1 Jan 2027"]);

        let homes = ShareHomes::new("chat-days");
        let mut model = Model::open(&homes.0).unwrap().0;
        let fixed = |hours| LocalClock { initial: Some(hours * 3600), changes: Vec::new() };
        let read = |model: &Model, clock: &LocalClock| Workspace::read_chat_at(model, false, clock, &Default::default(),
            &Default::default(), start + 12 * hour, &Default::default());
        let utc = fixed(0);
        let empty = read(&model, &utc);
        assert!(empty.rows.is_empty() && empty.day_refresh_after_secs(&utc, start / 1000).is_none());
        let [a, b, c, d] = ["Before LA midnight", "LA midnight", "Before Tokyo midnight", "Tokyo midnight"]
            .map(|text| model.post_chat_draft(text, None, None, None).unwrap());
        for (line, ms) in model.state.chat.iter_mut().zip([8 * hour - 1, 8 * hour, 15 * hour - 1, 15 * hour]) {
            line.wall_ms = start + ms;
        }
        let keys = |ws: &Workspace| ws.rows.iter().map(ChatRow::key).collect::<Vec<_>>();
        let check_zones = |la: LocalClock, tokyo: LocalClock| {
            assert_eq!(keys(&read(&model, &la)), [Day(today - 1, 0), Message(a), Day(today, 0), Message(b), Message(c), Message(d)]);
            assert_eq!(keys(&read(&model, &tokyo)), [Day(today, 0), Message(a), Message(b), Message(c), Day(today + 1, 0), Message(d)]);
        };
        check_zones(fixed(-8), fixed(9));
        #[cfg(unix)]
        check_zones(LocalClock::load_tz(Some("America/Los_Angeles".as_ref())), LocalClock::load_tz(Some("Asia/Tokyo".as_ref())));

        for (line, ms) in model.state.chat.iter_mut().zip([start, start - 1, start, 0]) {
            line.wall_ms = ms;
        }
        let mut ws = read(&model, &utc);
        let before = keys(&ws);
        assert_eq!(before, [Day(today, 0), Message(a), Day(today - 1, 0), Message(b), Day(today, 1), Message(c), Message(d)]);
        let rev = ws.rows_rev;
        let midnight = start + 24 * hour;
        assert!(!ws.refresh_day_labels(&utc, midnight - 1));
        assert_eq!(ws.day_refresh_after_secs(&utc, (start + 9 * hour) / 1000), Some(54_000));
        assert_eq!(ws.day_refresh_after_secs(&utc, (midnight - 1) / 1000), Some(1));
        assert!(ws.refresh_day_labels(&utc, midnight));
        let labels: Vec<_> = ws.rows.iter().filter_map(|r| match r { ChatRow::Day(d) => Some(d.label.as_str()), _ => None }).collect();
        assert_eq!(labels, ["Yesterday", "27 Nov", "Yesterday"]);
        assert_eq!(ws.rows_rev, rev);
        assert_eq!(keys(&ws), before);
        assert_eq!(ws.day_refresh_after_secs(&utc, midnight / 1000), Some(86_400));
        assert!(!ws.refresh_day_labels(&utc, midnight));
    }

    #[test]
    fn chat_times_follow_the_offset_at_the_message() {
        let mut zone = b"TZif\0".to_vec();
        zone.resize(20, 0);
        for count in [0u32, 0, 0, 1, 2, 8] {
            zone.extend(count.to_be_bytes());
        }
        zone.extend(3600i32.to_be_bytes());
        zone.push(1);
        for (offset, flags) in [(3600i32, [0, 0]), (7200, [1, 4])] {
            zone.extend(offset.to_be_bytes());
            zone.extend(flags);
        }
        zone.extend(b"STD\0DST\0");
        let clock = LocalClock::from_zone(&zone).unwrap();
        assert_eq!(clock.hm(3_599_000), "01:59");
        assert_eq!(clock.hm(3_600_000), "03:00");
        assert!(LocalClock::from_zone(&zone[..48]).is_none(), "a truncated transition is unreadable");
    }

    #[test]
    fn member_and_unknown_notice_keys_stay_unique_and_rename_invariant() {
        use makepad_town_core::{event::{Actor, Draft, Event, Via}, log::Log};
        use crate::model::tests::{exchange, shared_town};
        let (homes, mut founder, mut model) = shared_town("notice-keys");
        let dir = homes.0.join("b");
        let town = founder.open_id();
        model.open_town(town).unwrap();
        let path = model.home.town_log_path(&town);
        let read = |model: &Model| {
            let rows = Workspace::read(model, false, &LocalClock::default()).rows;
            assert_eq!(rows.iter().map(ChatRow::key).collect::<HashSet<_>>().len(), rows.len(),
                "each timeline row has its own key");
            rows.into_iter().filter_map(|row| match row { ChatRow::Member(n) => Some(n), _ => None }).collect::<Vec<_>>()
        };
        founder.set_person_name("Ana").unwrap();
        exchange(&mut founder, &mut model);
        let before = read(&model);
        let accept = model.state.timeline.iter().find_map(|e| match e { Entry::Member(id) => Some(*id), _ => None }).unwrap();
        assert_eq!(before, [MemberNotice { key: NoticeKey::Member(accept), text: "Ana added your computer".into() }]);
        founder.set_person_name("Zoe").unwrap();
        exchange(&mut founder, &mut model);
        let renamed = read(&model);
        assert_eq!(renamed, [MemberNotice { key: before[0].key, text: "Zoe added your computer".into() }]);

        let append_unknown = |model: Model, bytes: &[u8]| {
            let key = model.device_key();
            drop(model);
            let mut log = Log::open(&path).unwrap().0;
            let draft = Draft { kind: 499, kind_version: 1, critical: false, actor: Actor::Human, via: Via::Ui,
                wall_ms: 1, parents: log.parents_for(&key.public()), content: bytes };
            let event = Event::create(&key, town, draft, [0; 16]).unwrap();
            let id = event.id;
            log.append(event).unwrap();
            drop(log);
            (Model::open(&dir).unwrap().0, id)
        };
        let (model, first) = append_unknown(model, b"first");
        let one = read(&model);
        assert_eq!(one.last().unwrap().key, NoticeKey::Unknown(first));
        assert_eq!(one.last().unwrap().text, "A newer Makepad Town wrote something here.");
        let (mut model, _) = append_unknown(model, b"second");
        let two = read(&model);
        assert_eq!(two.len(), one.len());
        assert_eq!(two.last().unwrap().key, NoticeKey::Unknown(first));
        assert_eq!(two.last().unwrap().text, "A newer Makepad Town wrote 2 things here.");
        model.post_chat_draft("Between runs", None, None, None).unwrap();
        let (model, third) = append_unknown(model, b"third");
        let notices = read(&model);
        assert_eq!(notices.len(), two.len() + 1);
        assert_eq!(notices[notices.len() - 2].key, NoticeKey::Unknown(first));
        assert_eq!(notices.last().unwrap().key, NoticeKey::Unknown(third));
    }
}
