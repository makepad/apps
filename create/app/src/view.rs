//! Read models for the workspace. Draws reuse these across frames.

use makepad_town_core::key::PublicKey;

use crate::ai_trace::{Output, Phase, Trace, Traces};
use std::collections::HashSet;
use crate::link_worker::{ConnectView, DeviceLink};
pub use crate::ui::chat::ClipPlayback;
use crate::model::{peer_label, task_column, Model, PeerUpdate};
use makepad_town_core::blob::{self, lane::Status};
use makepad_town_core::entity::{field, kind, Entity};
use makepad_town_core::event::{Actor, Id, Key, TownId, Service, SessionId, SessionMode, Value};
use makepad_town_core::fold::{addresses_my_ai, project_entities, ChatMsg, Entry, Outcome, Seen, SessionSummary};
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    // A verified recipient names its owner's signed addition, also after the AI leaves.
    let owner = m.to_agent.and_then(|id| model.event(&id)).map(|e| e.header.author)
        .or_else(|| (m.actor == Actor::Human && m.chat.to_ai).then_some(m.author));
    owner.map_or_else(author, |owner| model.ai_request_label(&m.author, &owner))
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
    pub name: String,
    pub time: String,
    pub text: String,
    /// My own line to my AI: drawn with "sent to your AI".
    pub addressed: bool,
    /// My own line to my AI while no AI of mine is here: drawn with "No AI here yet · Add AI".
    pub unheard: bool,
    /// Any person's line to their own AI: the "@ai" marker keeps its ink on every computer.
    pub mention: bool,
    /// This person may delete the line now (their own, in a town they still write to).
    pub deletable: bool,
}

pub enum ChatRow { Message(Message), Call(CallLine), Clip(Arc<ClipLine>), Member(String) }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
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
        let callback = missed && summary.opener_seat.is_none_or(|seat| !connected.contains(&seat));
        if callback || summary.lines > 0 {
            let call_back = callback.then(|| CallBack { peer: opener, enabled: model.synced_with(&summary.opener) });
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

#[derive(Default)]
pub struct Workspace {
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
    pub allow: AgentControl,
    pub deny: AgentControl,
    pub label: String,
}

pub struct AgentRow {
    pub id: Id,
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
            let seat = Seat::from_author(task.author == me, task.actor);
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
                seat,
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
                id, name: author_name(agent.owner, Actor::Ai).to_string(), owner, status, away, live, working,
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
        let my_seat = model.state.seat_of(&me);
        let mut seat_names = std::collections::BTreeMap::new();
        for device in model.state.admitted_devices() {
            if let Some(seat) = model.state.seat_of(&device) {
                seat_names.entry(seat).or_insert_with(|| model.author_label(&device, Actor::Human));
            }
        }
        if let Some(seat) = my_seat { seat_names.insert(seat, "You".into()); }
        let name = |seat: &Id| seat_names.get(seat).cloned().unwrap_or_else(|| "Peer".into());
        let mut rows = Vec::new();
        let mut unknown = model.state.unknown.iter().filter_map(|id| model.state.order_of(id)).peekable();
        // Merge once, at visible row boundaries. The plain timeline row already has
        // the quiet treatment used for membership notices and has no chat actions.
        let mut show_unknown = |rows: &mut Vec<ChatRow>, before: Option<&Id>| {
            if unknown.peek().is_none() { return }
            let limit = before.and_then(|id| model.state.order_of(id));
            let mut count = 0usize;
            while unknown.next_if(|order| limit.is_none_or(|limit| *order < limit)).is_some() {
                count += 1;
            }
            match count {
                0 => {}
                1 => rows.push(ChatRow::Member("A newer Makepad Town wrote something here.".into())),
                n => rows.push(ChatRow::Member(format!("A newer Makepad Town wrote {n} things here."))),
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
                    show_unknown(&mut rows, Some(id));
                    let by = model.author_label(&grant.header.author, Actor::Human);
                    let line = if accept.header.author == me { format!("{by} added your computer") }
                        else { format!("{by} added {}'s computer", model.author_label(&accept.header.author, Actor::Human)) };
                    rows.push(ChatRow::Member(line));
                    last_shown = Some(*id);
                }
                Entry::Clip(id) => {
                    let Some(entity) = model.state.entities.get(id) else { continue };
                    show_unknown(&mut rows, Some(id));
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
                    show_unknown(&mut rows, Some(&anchor.id));
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
                    show_unknown(&mut rows, Some(id));
                    let index = rows.len();
                    last_shown = Some(m.id);
                    let addressed = addresses_my_ai(m, &me);
                    // A person's `@` line goes to their own AI only. Everyone in the town sees it,
                    // marked, and other seats’ AIs cannot read it.
                    let to_own_ai = m.actor == Actor::Human && m.chat.to_ai;
                    if let Some(&agent) = pending.get(&m.id) {
                        let decision = |allow| AgentControl::Decide { town: model.open_id(), agent, line: m.id, allow };
                        connect.requests.insert(index, ApprovalRow {
                            allow: decision(true), deny: decision(false), label: "Let your AI do it".into(),
                        });
                    }
                    let name = message_name(model, m, || author_name(m.author, m.actor).to_string());
                    let receipts = traces.rows(&m.id);
                    if !receipts.is_empty() { chat_index.insert(m.id, index); }
                    rows.push(ChatRow::Message(Message {
                        id: m.id,
                        // The fold drops the quote of a retracted line, so its text never shows here.
                        quote: m.chat.quote.map(|id| Quote::read(model, id)),
                        traces: receipts.iter().enumerate().map(|(i, trace)| {
                            let key = TraceFold { line: m.id, trace: i, history: trace.phase(now).finished() };
                            TraceRow::read(trace, now, clock, folds.contains(&key))
                        }).collect(),
                        seat: Seat::from_author(m.author == me, m.actor),
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
        let row = &mut message.traces[key.trace];
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
    name.chars().next().unwrap_or('P').to_uppercase().collect()
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
pub enum MemberId { You, Computer(PublicKey), YourAi, Agent(Id) }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence { Here, Reaching, Working, Away }

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
pub struct MemberSeat {
    pub id: MemberId,
    pub seat: Seat,
    pub presence: Presence,
    pub name: String,
    pub status: String,
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct TownMembers {
    pub seats: Vec<MemberSeat>,
    pub refresh_after_secs: Option<u64>,
}

impl TownMembers {
    pub fn read(model: &Model, link: &ConnectView, workspace: &Workspace, clock: &LocalClock, now: u64) -> Self {
        Self::read_with_quiet(model, link, &RetryQuiet::default(), workspace, clock, now)
    }

    pub fn read_with_quiet(model: &Model, link: &ConnectView, quiet: &RetryQuiet,
        workspace: &Workspace, clock: &LocalClock, now: u64) -> Self {
        let mut members = Self::default();
        let me = model.me();
        members.seats.push(MemberSeat { id: MemberId::You, seat: Seat::You, presence: Presence::Here, name: model.device_label(&me).into(), status: String::new() });
        let active_here = model.state.active_member(&me);
        for key in model.active_members_in_join_order().into_iter().filter(|key| *key != me) {
            let name = model.device_label(&key);
            let linked = (active_here && model.linked_with(&key)).then(|| model.synced_with(&key));
            let reaching = match link.device_link(&key, 0) {
                DeviceLink::Reaching { since } if active_here && !quiet.is_quiet(&key) => Some(since),
                _ => None,
            };
            let peer_update = active_here.then(|| model.peer_update(&key)).flatten();
            let peer = TablePeer::read(name, linked, peer_update, reaching, model.device_last_seen(&key), clock, now);
            members.refresh_after_secs = members.refresh_after_secs.into_iter().chain(peer.refresh_after_secs).min();
            members.seats.push(MemberSeat {
                id: MemberId::Computer(key), seat: Seat::Peer, presence: peer.status.presence(), name: peer.name, status: peer.status.text(),
            });
        }
        if workspace.listening { members.seats.push(MemberSeat { id: MemberId::YourAi, seat: Seat::YourAi, presence: Presence::Here, name: "Your AI".into(), status: "listening".into() }); }
        for agent in &workspace.connect.rows {
            members.seats.push(MemberSeat {
                id: MemberId::Agent(agent.id), seat: if agent.owner { Seat::YourAi } else { Seat::PeerAi },
                presence: agent_presence(agent), name: agent.name.clone(), status: agent.status.clone(),
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
fn calendar_date(day: i64) -> String {
    let z = day + 719468;
    let era = z.div_euclid(146097);
    let day_of_era = z.rem_euclid(146097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let march_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * march_month + 2) / 5 + 1;
    let month = if march_month < 10 { march_month + 3 } else { march_month - 9 };
    let year = era * 400 + year_of_era + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
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
        let path = match std::env::var_os("TZ") {
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
        self.changes.get(self.transition_index(now)).map_or(midnight, |&(at, _)| midnight.min(at as u64 - now))
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
        ana.act(Action::SessionOpen { town: None, session: answered, mode: mode.clone(), to: to_ben.clone() }, Ui).unwrap();
        ben.act(Action::SessionJoin { town: None, session: answered }, Ui).unwrap();
        ana.act(leave(answered), Ui).unwrap();
        ben.act(leave(answered), Ui).unwrap();
        let spoken = [6; 16];
        ben.act(Action::SessionOpen { town: None, session: spoken, mode: mode.clone(), to: to_ana.clone() }, Ui).unwrap();
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
            (SessionOp::Open { session: missed, mode: mode.clone(), to: to_ben.clone() }, open_ms),
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
        ana.act(Action::SessionOpen { town: None, session: share, mode: SessionMode::Share { title: "Drawing".into() }, to: to_ben.clone() }, Ui).unwrap();
        exchange(&mut ana, &mut ben);

        let clock = LocalClock { initial: Some(3600), changes: Vec::new() };
        let check = |ana: &Model, ben: &Model, pending: bool| {
            for (model, is_ben) in [(ana, false), (ben, true)] {
                let view = Workspace::read(model, false, &clock);
                let [ChatRow::Member(_), ChatRow::Call(transcribed), middle @ .., ChatRow::Message(line)] = view.rows.as_slice()
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
        ben.act(Action::SessionJoin { town: None, session: share }, Ui).unwrap();
        ana.act(Action::Said { session: share, lines: vec![SaidLine { at_ms: 0, dur_ms: 500, text: "Look here.".into() }] }, Ui).unwrap();
        exchange(&mut ana, &mut ben);
        check(&ana, &ben, true);
        ana.act(leave(share), Ui).unwrap();
        ben.act(leave(share), Ui).unwrap();
        let declined = [2; 16];
        ben.act(Action::SessionOpen { town: None, session: declined, mode: mode.clone(), to: to_ana.clone() }, Ui).unwrap();
        ana.act(Action::SessionDecline { town: None, session: declined, why: DeclineWhy::Declined }, Ui).unwrap();
        ben.act(leave(declined), Ui).unwrap();
        exchange(&mut ana, &mut ben);
        check(&ana, &ben, true);

        let connected = [3; 16];
        ben.act(Action::SessionOpen { town: None, session: connected, mode: mode.clone(), to: to_ana }, Ui).unwrap();
        ana.act(Action::SessionJoin { town: None, session: connected }, Ui).unwrap();
        ana.act(leave(connected), Ui).unwrap();
        ben.act(leave(connected), Ui).unwrap();
        exchange(&mut ana, &mut ben);
        check(&ana, &ben, false);
        let newer = [4; 16];
        ana.act(Action::SessionOpen { town: None, session: newer, mode, to: to_ben }, Ui).unwrap();
        ana.act(leave(newer), Ui).unwrap();
        exchange(&mut ana, &mut ben);
        let check_newer = |ana: &Model, ben: &Model, online: bool| {
            let caller = Workspace::read(ana, false, &clock);
            assert!(matches!(caller.rows.as_slice(), [ChatRow::Member(_), ChatRow::Call(call), ChatRow::Message(line)]
                if call.id.1 == spoken && call.call_back.is_none() && line.id == message),
                "the caller keeps the transcript and message after the newer outgoing call");
            let view = Workspace::read(ben, false, &clock);
            let [ChatRow::Member(_), ChatRow::Call(transcribed), ChatRow::Message(line), ChatRow::Call(row)] = view.rows.as_slice()
                else { panic!("the transcript, message and newer missed call survive; the cleared row stays gone") };
            assert_eq!((transcribed.id.1, line.id, row.id.1), (spoken, message, newer));
            assert_eq!(row.call_back.as_ref().map(|c| c.enabled), Some(online), "the newer callback follows the link's availability");
        };
        check_newer(&ana, &ben, true);

        drop((ana, ben));
        check_newer(&open_home("ana"), &open_home("ben"), false);
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
            (declined, Seen::Other, other, "Ana called Kit · declined", false),
            (Outcome::Unanswered, Seen::Opener, ana, "No answer", false),
            (Outcome::Unanswered, Seen::Rung, kit, "Missed call from Ana", true),
            (Outcome::Unanswered, Seen::Other, other, "Ana called Kit · no answer", false),
            (Outcome::Pending, Seen::Opener, ana, "You called Kit", false),
            (Outcome::Pending, Seen::Rung, kit, "Ana called", false),
            (Outcome::Pending, Seen::Other, other, "Ana called Kit", false),
        ] {
            let s = SessionSummary { opener: [0; 65], opener_seat: Some(ana), viewer, outcome,
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
            let row = AgentRow { id: agent, name: String::new(), owner, status: String::new(), away, live, working };
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
        assert_eq!(delivery[0].call_id, "held");
        let page = makepad_strict_json::parse(delivery[0].text.as_bytes()).unwrap();
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
}
