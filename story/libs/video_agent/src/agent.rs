//! The Generate loop: plan → write → inspect → look → critique → fix, until
//! the work is clean and the model finishes; then it waits for the
//! person's next instruction ("make scene 2 punchier") and goes round
//! again on the same documents.
//!
//! The agent is passive: [`Agent::poll`] hands the host the calls to run
//! (the host executes them through Stage's control API, as undoable edits),
//! and [`Agent::result`] takes each answer back. Nothing here touches a
//! document; everything is recorded in the [`Entry`] transcript the panel
//! draws, including every document change as a diff and every hand edit
//! the person made in between, which the model is told about before it
//! goes on.
//!
//! Bounds: a round budget (a round is one reply and its calls). Near its
//! end the model is told to wrap up; at the end the loop stops and says
//! so. Every step already made stays in the cut, undoable.

use crate::brain::{Backend, Brain, BrainEvent, BrainImage, CallResult};
use crate::diff::{diff, Diff};
use crate::tools::{own_tools, parse_own, Args, Critique, Own, Storyboard, ToolDef};
use makepad_strict_json::Value;
use std::collections::HashMap;

/// What the model makes: a video (a cut) or a deck.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    Cut,
    Deck,
}

impl Kind {
    /// The key of the document it works on.
    pub fn key(self) -> &'static str {
        match self {
            Kind::Cut => "cut",
            Kind::Deck => "deck",
        }
    }
}

/// What the person asked for.
#[derive(Clone, Debug, PartialEq)]
pub struct Brief {
    /// The kind of document the work lands in.
    pub kind: Kind,
    pub prompt: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Seconds.
    pub length: f64,
    /// The look, in the person's words (may be empty).
    pub style: String,
    /// A voice id for the voiceover, or empty for none.
    pub voice: String,
    /// Music: `generate`, `none`, or what to find in the library.
    pub music: String,
    /// Whether to generate B-roll and backgrounds on the AI Hub.
    pub generate_media: bool,
    /// What the host's building blocks are called (style kits, animators,
    /// 3D presets…), compact, for the model to pick from.
    pub menus: String,
    /// A person is watching and may be asked to choose (the panel); false
    /// for a headless run, which decides everything itself.
    pub interactive: bool,
}

impl Brief {
    pub fn aspect(&self) -> &'static str {
        match (self.width, self.height) {
            (w, h) if w * 16 == h * 9 => "9:16",
            (w, h) if w * 9 == h * 16 => "16:9",
            (w, h) if w == h => "1:1",
            (w, h) if w * 5 == h * 4 => "4:5",
            _ => "custom",
        }
    }
}

/// A document the host holds, as it is now: the cut, a Motion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Doc {
    /// A stable key: `cut`, `motion:<asset id>`.
    pub key: String,
    /// What the model and the person call it.
    pub label: String,
    pub source: String,
}

/// A change a call made to a document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocChange {
    pub key: String,
    pub label: String,
    pub before: String,
    pub after: String,
}

/// What an inspect found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InspectFacts {
    /// What it inspected: `cut`, or a Motion document's name.
    pub target: String,
    pub errors: usize,
    pub warnings: usize,
    /// Warnings that must be fixed too: text or layers outside the safe area
    /// or the frame, text that overflows or is clipped.
    pub blocking: usize,
}

/// A call's answer from the host.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outcome {
    pub text: String,
    pub is_error: bool,
    pub images: Vec<BrainImage>,
    pub inspect: Option<InspectFacts>,
    pub changes: Vec<DocChange>,
}

impl Outcome {
    pub fn ok(text: impl Into<String>) -> Self {
        Outcome { text: text.into(), ..Outcome::default() }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Outcome { text: text.into(), is_error: true, ..Outcome::default() }
    }
}

/// A call for the host to run. `args` is the JSON object the tool's schema
/// describes.
#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub call_id: String,
    pub name: String,
    pub args: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallState {
    Running,
    Done(String),
    Failed(String),
}

/// Who changed a document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Author {
    Model,
    Person,
}

/// One change of one document, for the panel and the Source view.
#[derive(Clone, Debug, PartialEq)]
pub struct Revision {
    pub round: u32,
    pub key: String,
    pub label: String,
    pub by: Author,
    pub before: String,
    pub after: String,
    pub diff: Diff,
}

/// The transcript, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    User(String),
    Status(String),
    Thinking(String),
    Say(String),
    Storyboard(Storyboard),
    Call { call_id: String, name: String, summary: String, state: CallState, images: Vec<BrainImage> },
    /// Index into [`Agent::revisions`].
    Change(usize),
    Critique(Critique),
    /// A question the model stopped to ask, with its options.
    Ask { question: String, options: Vec<String> },
    /// The loop's own notes: a refused finish, a nudge, the round budget.
    Note(String),
    Done(String),
    Stopped(String),
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// Waiting for the model's reply.
    Thinking,
    /// Waiting for the host's results.
    Working,
    /// Finished; takes a new instruction.
    Done,
    /// Ended without finishing (budget, cancel, a model that stopped);
    /// takes a new instruction.
    Stopped(String),
    /// The model failed; takes a new instruction (which retries).
    Failed(String),
}

impl State {
    pub fn busy(&self) -> bool {
        matches!(self, State::Thinking | State::Working)
    }
}

/// What the loop is doing, for the panel's header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Plan,
    Write,
    Inspect,
    Look,
    Critique,
    Fix,
    Finish,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Plan => "Planning",
            Phase::Write => "Writing",
            Phase::Inspect => "Inspecting",
            Phase::Look => "Looking",
            Phase::Critique => "Critiquing",
            Phase::Fix => "Fixing",
            Phase::Finish => "Finishing",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Replies (with their calls) per instruction.
    pub rounds: u32,
    /// Replies without a call before the loop stops (it is nudged first).
    pub idle_replies: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { rounds: 120, idle_replies: 3 }
    }
}

impl Limits {
    /// The budget for a video `seconds` long: a longer film has more sets to
    /// build and look at (a 60 s lyric film of 15 lines spent 120 rounds).
    /// Two more rounds per second above 30 s, at most 400.
    pub fn for_length(seconds: f64) -> Limits {
        let extra = ((seconds - 30.0).max(0.0) * 2.0).round() as u32;
        Limits { rounds: (120 + extra).min(400), ..Limits::default() }
    }
}

/// Failed picture calls in a row after which `finish` stops requiring a
/// look (the loop must not spin on a broken renderer).
pub const LOOK_FAILURES: u32 = 3;

/// Refusals of `finish` for blocking warnings alone, after which the loop
/// accepts them (a check the model cannot satisfy must not trap it).
pub const WARNING_REFUSALS: u32 = 2;
/// Past [`WARNING_REFUSALS`], finish is refused again while each refusal
/// finds fewer blocking warnings than the one before (the fixes are
/// working), up to this many refusals in all.
pub const WARNING_REFUSALS_MAX: u32 = 6;

/// The largest side a picture may have (the backends' own limit).
pub const MAX_EDGE: u32 = 1024;

/// Whether a PNG's header says it fits within [`MAX_EDGE`].
fn fits(png: &[u8]) -> bool {
    let side = |at: usize| png.get(at..at + 4).map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    !png.starts_with(b"\x89PNG") || (side(16) <= MAX_EDGE && side(20) <= MAX_EDGE)
}

/// Pictures one message may carry (the backends' own limit).
pub const MAX_IMAGES: usize = 4;

struct Pending {
    call_id: String,
    name: String,
    /// The transcript entry to update.
    entry: usize,
    result: Option<(Outcome, bool)>,
}

pub struct Agent {
    brain: Box<dyn Brain>,
    tools: Vec<ToolDef>,
    limits: Limits,
    state: State,
    phase: Phase,
    entries: Vec<Entry>,
    revisions: Vec<Revision>,
    generation: u64,
    storyboard: Option<Storyboard>,
    /// Rounds of the current instruction.
    round: u32,
    /// Fix cycles: a change after the model looked.
    iterations: u32,
    pending: Vec<Pending>,
    turn_over: bool,
    actions: Vec<Action>,
    /// Documents as the model last knew them, and as the host has them.
    seen: HashMap<String, String>,
    docs: HashMap<String, (String, String)>,
    /// Lines the person sent while the model was busy.
    queued: Vec<String>,
    /// Facts behind `finish`.
    changed_since_inspect: bool,
    edit_errors: Option<usize>,
    /// Blocking warnings in the last inspect of the gate, and how many times
    /// `finish` was refused for them alone.
    edit_blocking: usize,
    /// Blocking warnings per inspected document (the cut's own inspect
    /// does not look inside each Motion document).
    blocking_by_target: HashMap<String, usize>,
    warning_refusals: u32,
    /// The blocking warnings when finish was last refused for them.
    refused_blocking: Option<usize>,
    /// What `finish` needs inspected clean: `cut`, or (a scene author)
    /// its own Motion document's name.
    gate: String,
    looked_since_change: bool,
    /// Picture tools that failed in a row: when looking keeps failing (a
    /// renderer problem, not the model's), finish no longer waits for it.
    look_failures: u32,
    finished: Option<String>,
    /// The model asked the person something: it waits after this reply.
    asked: bool,
    idle_replies: u32,
}

impl Agent {
    /// Start a generation: the model gets the system text (the authoring
    /// guide and the process), the host's tools plus the agent's own, and
    /// the brief with the documents as they are.
    pub fn start(backend: &Backend, host_tools: Vec<ToolDef>, brief: &Brief, docs: Vec<Doc>, limits: Limits) -> Result<Agent, String> {
        Agent::start_seeing(backend, host_tools, brief, docs, limits, Vec::new())
    }

    /// [`Agent::start`] with pictures that go with the first message.
    pub fn start_seeing(backend: &Backend, host_tools: Vec<ToolDef>, brief: &Brief, docs: Vec<Doc>, limits: Limits, images: Vec<crate::brain::BrainImage>) -> Result<Agent, String> {
        let mut tools: Vec<ToolDef> = own_tools().into_iter().filter(|tool| brief.interactive || tool.name != crate::tools::ASK).collect();
        tools.extend(host_tools.into_iter().filter(|tool| parse_own(&tool.name, &Args::default()).is_none()));
        let system = if backend.compact_guide() { crate::guide::compact_system_prompt(brief, backend.vision()) } else { crate::guide::system_prompt(brief, backend.vision()) };
        let brain = backend.start(&system, &tools)?;
        let mut agent = Agent::with_brain(brain, tools, limits);
        agent.gate = brief.kind.key().to_string();
        agent.observe(docs);
        agent.seen = agent.docs.iter().map(|(key, (_, source))| (key.clone(), source.clone())).collect();
        let first = crate::guide::first_message(brief, &agent.documents());
        agent.push(Entry::User(brief.prompt.clone()));
        agent.state = State::Thinking;
        agent.brain.send_user(first, images);
        Ok(agent)
    }

    /// The director of a team (see [`crate::team`]): the guide with the
    /// team process, the host's and the team's tools.
    pub fn start_team(backend: &Backend, tools: Vec<ToolDef>, brief: &Brief, docs: Vec<Doc>, limits: Limits) -> Result<Agent, String> {
        let mut all = own_tools();
        all.extend(tools.into_iter().filter(|tool| parse_own(&tool.name, &Args::default()).is_none()));
        let system = crate::guide::director_system(backend.vision());
        let brain = backend.start(&system, &all)?;
        let mut agent = Agent::with_brain(brain, all, limits);
        agent.observe(docs);
        agent.seen = agent.docs.iter().map(|(key, (_, source))| (key.clone(), source.clone())).collect();
        let first = crate::guide::first_message(brief, &agent.documents());
        agent.push(Entry::User(brief.prompt.clone()));
        agent.state = State::Thinking;
        agent.brain.send_user(first, Vec::new());
        Ok(agent)
    }

    /// Send the first user line of an agent made with [`Agent::with_brain`].
    pub fn start_turn(&mut self, text: String) {
        self.push(Entry::User(text.clone()));
        self.state = State::Thinking;
        self.brain.send_user(text, Vec::new());
    }

    /// A note in the transcript from the loop (or the team) itself.
    pub fn note(&mut self, text: String) {
        self.push(Entry::Note(text));
    }

    /// An agent with its own system text and first message (a scene author
    /// of a team): `gate` is what `finish` needs inspected clean.
    pub fn start_with(backend: &Backend, tools: Vec<ToolDef>, system: &str, first: String, shown: String, gate: &str, docs: Vec<Doc>, limits: Limits) -> Result<Agent, String> {
        let brain = backend.start(system, &tools)?;
        Ok(Agent::start_on(brain, tools, first, shown, gate, docs, limits))
    }

    /// [`Agent::start_with`] on a brain already made.
    pub fn start_on(brain: Box<dyn Brain>, tools: Vec<ToolDef>, first: String, shown: String, gate: &str, docs: Vec<Doc>, limits: Limits) -> Agent {
        let mut agent = Agent::with_brain(brain, tools, limits);
        agent.gate = gate.to_string();
        agent.observe(docs);
        agent.seen = agent.docs.iter().map(|(key, (_, source))| (key.clone(), source.clone())).collect();
        agent.push(Entry::User(shown));
        agent.state = State::Thinking;
        agent.brain.send_user(first, Vec::new());
        agent
    }

    /// The last summary the agent finished with, if it finished.
    pub fn summary(&self) -> Option<&str> {
        self.entries.iter().rev().find_map(|entry| match entry {
            Entry::Done(summary) => Some(summary.as_str()),
            _ => None,
        })
    }

    /// An agent over any brain (tests, a headless evaluator).
    pub fn with_brain(brain: Box<dyn Brain>, tools: Vec<ToolDef>, limits: Limits) -> Agent {
        Agent {
            brain,
            tools,
            limits,
            state: State::Done,
            phase: Phase::Plan,
            entries: Vec::new(),
            revisions: Vec::new(),
            generation: 1,
            storyboard: None,
            round: 0,
            iterations: 0,
            pending: Vec::new(),
            turn_over: false,
            actions: Vec::new(),
            seen: HashMap::new(),
            docs: HashMap::new(),
            queued: Vec::new(),
            changed_since_inspect: false,
            edit_errors: None,
            edit_blocking: 0,
            blocking_by_target: HashMap::new(),
            warning_refusals: 0,
            refused_blocking: None,
            gate: "cut".into(),
            looked_since_change: false,
            look_failures: 0,
            finished: None,
            asked: false,
            idle_replies: 0,
        }
    }

    pub fn label(&self) -> String {
        self.brain.label()
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn revisions(&self) -> &[Revision] {
        &self.revisions
    }

    pub fn storyboard(&self) -> Option<&Storyboard> {
        self.storyboard.as_ref()
    }

    pub fn round(&self) -> u32 {
        self.round
    }

    pub fn iterations(&self) -> u32 {
        self.iterations
    }

    /// Bumped whenever anything a panel shows changed.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn touch(&mut self) {
        self.generation += 1;
    }

    fn push(&mut self, entry: Entry) -> usize {
        // Streamed text and reasoning grow their last entry.
        match (self.entries.last_mut(), &entry) {
            (Some(Entry::Say(text)), Entry::Say(more)) | (Some(Entry::Thinking(text)), Entry::Thinking(more)) => text.push_str(more),
            (Some(Entry::Status(text)), Entry::Status(more)) => *text = more.clone(),
            _ => self.entries.push(entry),
        }
        self.touch();
        self.entries.len() - 1
    }

    /// The documents as the host has them now. Call whenever they change.
    pub fn observe(&mut self, docs: Vec<Doc>) {
        let docs: HashMap<String, (String, String)> = docs.into_iter().map(|doc| (doc.key, (doc.label, doc.source))).collect();
        if docs != self.docs {
            self.docs = docs;
            self.touch();
        }
    }

    fn documents(&self) -> Vec<Doc> {
        let mut docs: Vec<Doc> = self.docs.iter().map(|(key, (label, source))| Doc { key: key.clone(), label: label.clone(), source: source.clone() }).collect();
        docs.sort_by(|a, b| (a.key != "cut", &a.label).cmp(&(b.key != "cut", &b.label)));
        docs
    }

    /// Documents that differ from what the model last knew: the person's
    /// hand edits. Recorded, and returned as text for the model.
    fn hand_edits(&mut self) -> String {
        let mut text = String::new();
        for doc in self.documents() {
            let before = self.seen.get(&doc.key).cloned().unwrap_or_default();
            if before == doc.source {
                continue;
            }
            let change = diff(&before, &doc.source, 2);
            if change.is_empty() {
                continue;
            }
            text.push_str(&format!("\n[the person edited {} by hand]\n{}", doc.label, change.text()));
            self.revisions.push(Revision { round: self.round, key: doc.key.clone(), label: doc.label.clone(), by: Author::Person, before, after: doc.source.clone(), diff: change });
            let index = self.revisions.len() - 1;
            self.push(Entry::Change(index));
            self.seen.insert(doc.key.clone(), doc.source.clone());
            self.changed_since_inspect = true;
            self.looked_since_change = false;
        }
        text
    }

    /// A line from the person: a new instruction when the loop is idle
    /// ("make scene 2 punchier"), or a note it reads with its next
    /// results while it works.
    pub fn say(&mut self, text: String) {
        self.say_seeing(text, Vec::new());
    }

    /// [`Agent::say`] with pictures (sent when the loop is idle; a note to a
    /// working loop goes as text).
    pub fn say_seeing(&mut self, text: String, images: Vec<crate::brain::BrainImage>) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.push(Entry::User(text.clone()));
        if self.state.busy() {
            self.queued.push(text);
            return;
        }
        let edits = self.hand_edits();
        self.round = 0;
        self.idle_replies = 0;
        self.finished = None;
        self.state = State::Thinking;
        self.brain.send_user(format!("{text}{edits}\n\nRevise in place: read what you need, change only what the instruction asks for, then inspect, look and finish as before."), images);
    }

    /// Stop now. Calls the host is running still finish there (each is an
    /// ordinary undoable edit); their answers are no longer awaited.
    pub fn cancel(&mut self) {
        if !self.state.busy() {
            return;
        }
        self.brain.cancel();
        self.pending.clear();
        self.actions.clear();
        self.queued.clear();
        self.state = State::Stopped("cancelled".into());
        self.push(Entry::Stopped("Cancelled. Every change made so far is in the cut and can be undone.".into()));
    }

    /// Advance: read the model, and hand back the calls the host should
    /// run now.
    pub fn poll(&mut self) -> Vec<Action> {
        for event in self.brain.poll() {
            self.event(event);
        }
        self.advance();
        std::mem::take(&mut self.actions)
    }

    fn event(&mut self, event: BrainEvent) {
        if !self.state.busy() {
            return;
        }
        match event {
            BrainEvent::Status(text) => {
                self.push(Entry::Status(text));
            }
            BrainEvent::Thinking(text) => {
                self.push(Entry::Thinking(text));
            }
            BrainEvent::Delta(text) => {
                self.push(Entry::Say(text));
            }
            BrainEvent::Writing { name, bytes } => {
                self.push(Entry::Status(format!("writing {name} · {bytes} bytes")));
            }
            BrainEvent::Call { call_id, name, args } => self.call(call_id, name, args),
            BrainEvent::TurnDone => {
                self.turn_over = true;
                self.state = State::Working;
            }
            BrainEvent::Failed(error) => {
                self.pending.clear();
                self.state = State::Failed(error.clone());
                self.push(Entry::Failed(error));
            }
        }
    }

    fn call(&mut self, call_id: String, name: String, args: Result<Args, String>) {
        let entry = self.push(Entry::Call { call_id: call_id.clone(), name: name.clone(), summary: String::new(), state: CallState::Running, images: Vec::new() });
        self.pending.push(Pending { call_id: call_id.clone(), name: name.clone(), entry, result: None });
        let args = match args {
            Ok(args) => args,
            Err(error) => return self.answer(&call_id, Outcome::error(format!("{error}. Write the call again."))),
        };
        if let Some(own) = parse_own(&name, &args) {
            let outcome = match own {
                Ok(own) => self.own(own),
                Err(error) => Outcome::error(error),
            };
            return self.answer(&call_id, outcome);
        }
        let Some(tool) = self.tools.iter().find(|tool| tool.name == name).cloned() else {
            let names: Vec<&str> = self.tools.iter().map(|tool| tool.name.as_str()).collect();
            return self.answer(&call_id, Outcome::error(format!("there is no tool `{name}`; the tools are: {}", names.join(", "))));
        };
        self.phase = if tool.images {
            Phase::Look
        } else if tool.inspects {
            Phase::Inspect
        } else if tool.mutates {
            if self.looked_since_change || self.iterations > 0 { Phase::Fix } else { Phase::Write }
        } else {
            self.phase
        };
        match args.to_json(&tool.parameters) {
            Ok(json) => {
                if let Some(Entry::Call { summary, .. }) = self.entries.get_mut(entry) {
                    *summary = call_summary(&name, &json);
                }
                self.actions.push(Action { call_id, name, args: json });
            }
            Err(error) => self.answer(&call_id, Outcome::error(format!("{error}. Write the call again."))),
        }
    }

    fn own(&mut self, own: Own) -> Outcome {
        match own {
            Own::Storyboard(board) => {
                self.phase = Phase::Plan;
                let text = format!("Storyboard recorded: {} scenes over {:.1} s. It is shown to the person.", board.scenes.len(), board.duration());
                self.storyboard = Some(board.clone());
                self.push(Entry::Storyboard(board));
                Outcome::ok(text)
            }
            Own::Critique(critique) => {
                self.phase = Phase::Critique;
                let text = format!("Critique recorded ({} issues, {}).", critique.issues.len(), critique.verdict);
                self.push(Entry::Critique(critique));
                Outcome::ok(text)
            }
            Own::Ask { question, options } => {
                self.asked = true;
                self.push(Entry::Ask { question, options });
                Outcome::ok("Asked. End your reply now with a short line to the person; their answer comes as the next message.")
            }
            Own::Finish { summary } => {
                self.phase = Phase::Finish;
                let vision = self.brain.vision();
                let gate = self.gate.clone();
                let what = match gate.as_str() {
                    "cut" => "the cut".to_string(),
                    "deck" => "the deck".to_string(),
                    _ => format!("`{gate}`"),
                };
                let refusal = if self.edit_errors.is_none() {
                    Some(format!("inspect {what} first (`inspect` with target `{gate}`)"))
                } else if self.changed_since_inspect {
                    Some(format!("something changed since {what} was last inspected; inspect it again"))
                } else if let Some(errors) = self.edit_errors.filter(|errors| *errors > 0) {
                    Some(format!("the last inspect of {what} found {errors} error(s); fix them and inspect again"))
                } else if let Some(blocking) = Some(self.edit_blocking.max(self.blocking_by_target.values().sum())).filter(|blocking| {
                    *blocking > 0
                        && (self.warning_refusals < WARNING_REFUSALS
                            || (self.warning_refusals < WARNING_REFUSALS_MAX && self.refused_blocking.is_some_and(|before| *blocking < before)))
                }) {
                    self.warning_refusals += 1;
                    self.refused_blocking = Some(blocking);
                    let mut left: Vec<String> = self.blocking_by_target.iter().filter(|(_, n)| **n > 0).map(|(t, n)| format!("`{t}` ({n})")).collect();
                    left.sort();
                    Some(format!("safe-area, overflow or overlap warnings are left in {}: every line and layer must stay inside the safe area and the frame (use `fit: @safe` / `wrap: @safe`, move or re-break it), and no text may overlap other text (stack lines by their real height), then inspect those documents again", left.join(", ")))
                } else if vision && gate != "deck" && !self.looked_since_change && self.look_failures < LOOK_FAILURES {
                    Some(format!("look at {what} finished first (a `strip` with target `{gate}`), and critique it"))
                } else {
                    None
                };
                match refusal {
                    Some(why) => {
                        self.push(Entry::Note(format!("Finish refused: {why}.")));
                        Outcome::error(format!("Not finished: {why}."))
                    }
                    None => {
                        self.finished = Some(summary.clone());
                        Outcome::ok("Finished. Reply with one closing sentence for the person, and call no more tools.")
                    }
                }
            }
        }
    }

    /// The host's answer to a call it was given.
    pub fn result(&mut self, call_id: &str, outcome: Outcome) {
        if !self.pending.iter().any(|pending| pending.call_id == call_id && pending.result.is_none()) {
            return;
        }
        let tool = self.pending.iter().find(|pending| pending.call_id == call_id).and_then(|pending| self.tools.iter().find(|tool| tool.name == pending.name)).cloned();
        if tool.as_ref().is_some_and(|tool| tool.images) {
            if outcome.is_error || outcome.images.is_empty() {
                self.look_failures += 1;
                if self.look_failures == LOOK_FAILURES {
                    self.push(Entry::Note(format!("Pictures failed {LOOK_FAILURES} times in a row; finish no longer waits for a look.")));
                }
            } else {
                self.look_failures = 0;
            }
        }
        if !outcome.is_error {
            if let Some(tool) = &tool {
                if tool.images && !outcome.images.is_empty() {
                    self.looked_since_change = true;
                }
            }
            if let Some(facts) = outcome.inspect.as_ref() {
                // The cut's inspect counts its Motion documents' warnings
                // too: it replaces their older counts.
                if facts.target == "cut" || facts.target == "deck" {
                    self.blocking_by_target.clear();
                }
                self.blocking_by_target.insert(facts.target.clone(), facts.blocking);
            }
            if let Some(facts) = outcome.inspect.as_ref().filter(|facts| facts.target == self.gate) {
                self.edit_errors = Some(facts.errors);
                self.edit_blocking = facts.blocking;
                self.changed_since_inspect = false;
            }
        }
        for change in &outcome.changes {
            if change.before == change.after {
                continue;
            }
            if self.looked_since_change {
                self.iterations += 1;
            }
            self.changed_since_inspect = true;
            self.looked_since_change = false;
            self.finished = None;
            let d = diff(&change.before, &change.after, 2);
            self.revisions.push(Revision { round: self.round, key: change.key.clone(), label: change.label.clone(), by: Author::Model, before: change.before.clone(), after: change.after.clone(), diff: d });
            let index = self.revisions.len() - 1;
            self.push(Entry::Change(index));
            self.seen.insert(change.key.clone(), change.after.clone());
        }
        self.answer(call_id, outcome);
    }

    fn answer(&mut self, call_id: &str, outcome: Outcome) {
        let Some(pending) = self.pending.iter_mut().find(|pending| pending.call_id == call_id) else { return };
        let state = if outcome.is_error { CallState::Failed(first_line(&outcome.text)) } else { CallState::Done(first_line(&outcome.text)) };
        let entry = pending.entry;
        pending.result = Some((outcome.clone(), outcome.is_error));
        if let Some(Entry::Call { state: shown, images, .. }) = self.entries.get_mut(entry) {
            *shown = state;
            *images = outcome.images;
        }
        self.touch();
    }

    /// When the reply is over and every call is answered: send the round
    /// back, or end.
    fn advance(&mut self) {
        if self.state != State::Working || !self.turn_over || self.pending.iter().any(|pending| pending.result.is_none()) {
            return;
        }
        self.turn_over = false;
        let pending = std::mem::take(&mut self.pending);
        if pending.is_empty() {
            // A reply without calls.
            if let Some(summary) = self.finished.clone() {
                self.state = State::Done;
                self.push(Entry::Done(summary));
                return;
            }
            if std::mem::take(&mut self.asked) {
                self.state = State::Stopped("waiting for your choice".into());
                return;
            }
            self.idle_replies += 1;
            if self.idle_replies >= self.limits.idle_replies {
                let why = "the model stopped without finishing".to_string();
                self.state = State::Stopped(why.clone());
                self.push(Entry::Stopped(format!("Stopped: {why}. Say what to do next.")));
                return;
            }
            self.push(Entry::Note("Nudged: the model replied without a tool call.".into()));
            self.send_round(Vec::new(), Vec::new(), "You replied without calling a tool. Keep working through the tools until the video is done, then call `finish`. Do not ask the person questions; decide.".into());
            return;
        }
        self.idle_replies = 0;
        let mut results = Vec::new();
        let mut images = Vec::new();
        let mut left_out = Vec::new();
        for call in pending {
            let (outcome, is_error) = call.result.expect("every call answered");
            let mut text = outcome.text;
            for image in outcome.images {
                // A picture the backends refuse would fail the whole turn:
                // the host scales pictures down; one it could not is left out.
                if !fits(&image.png) {
                    text.push_str(&format!("\n(picture `{}` not attached: larger than {MAX_EDGE} px a side)", image.label));
                    continue;
                }
                if images.len() < MAX_IMAGES {
                    images.push(image);
                } else {
                    left_out.push(image.label);
                }
            }
            if !left_out.is_empty() && images.len() == MAX_IMAGES {
                text.push_str(&format!("\n(pictures not attached, at most {MAX_IMAGES} per message: {})", left_out.join(", ")));
                left_out.clear();
            }
            results.push(CallResult { call_id: call.call_id, name: call.name, text, is_error });
        }
        let note = String::new();
        self.send_round(results, images, note);
    }

    fn send_round(&mut self, results: Vec<CallResult>, images: Vec<BrainImage>, mut note: String) {
        self.round += 1;
        if self.round >= self.limits.rounds {
            let why = format!("the round budget ({}) is spent", self.limits.rounds);
            self.state = State::Stopped(why.clone());
            self.push(Entry::Stopped(format!("Stopped: {why}. Every change is in the cut; say what to do next.")));
            return;
        }
        note.push_str(&self.hand_edits());
        for line in std::mem::take(&mut self.queued) {
            note.push_str(&format!("\n[the person says] {line}"));
        }
        if self.round + 3 == self.limits.rounds {
            note.push_str("\n[round budget] Three rounds left: inspect the cut, look at a strip, and finish.");
            self.push(Entry::Note("Round budget nearly spent: the model was told to wrap up.".into()));
        }
        self.state = State::Thinking;
        if results.is_empty() {
            self.brain.send_user(note, images);
        } else {
            if let Some(last) = results.last() {
                let mut results = results.clone();
                if !note.is_empty() {
                    let mut last = last.clone();
                    last.text.push_str(&format!("\n{}", note.trim_start()));
                    *results.last_mut().unwrap() = last;
                }
                self.brain.send_results(results, images);
            }
        }
    }
}

fn first_line(text: &str) -> String {
    let line = text.trim().lines().next().unwrap_or("").trim();
    if line.chars().count() > 160 {
        format!("{}…", line.chars().take(160).collect::<String>())
    } else {
        line.to_string()
    }
}

/// One line for the transcript: the call and its short arguments, without
/// its documents.
fn call_summary(name: &str, args: &Value) -> String {
    let Value::Obj(fields) = args else { return name.to_string() };
    let mut parts = Vec::new();
    for (key, value) in fields {
        let shown = match value {
            Value::Str(text) if text.contains('\n') => format!("{} lines", text.lines().count()),
            Value::Str(text) if text.chars().count() > 48 => format!("“{}…”", text.chars().take(48).collect::<String>()),
            Value::Str(text) => format!("“{text}”"),
            Value::Arr(items) => format!("[{} items]", items.len()),
            Value::Obj(_) => "{…}".into(),
            other => other.to_json(),
        };
        parts.push(format!("{key} {shown}"));
    }
    if parts.is_empty() {
        name.to_string()
    } else {
        format!("{name} · {}", parts.join(" · "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    /// A brain that plays scripted replies and records what it was sent.
    struct Script {
        replies: Vec<Vec<BrainEvent>>,
        next: Option<Vec<BrainEvent>>,
        sent: Rc<RefCell<Vec<String>>>,
        vision: bool,
    }

    impl Brain for Script {
        fn label(&self) -> String {
            "script".into()
        }
        fn vision(&self) -> bool {
            self.vision
        }
        fn send_user(&mut self, text: String, _images: Vec<BrainImage>) {
            self.sent.borrow_mut().push(format!("USER {text}"));
            self.next = Some(if self.replies.is_empty() { vec![BrainEvent::Delta("ok".into()), BrainEvent::TurnDone] } else { self.replies.remove(0) });
        }
        fn send_results(&mut self, results: Vec<CallResult>, images: Vec<BrainImage>) {
            let text: Vec<String> = results.iter().map(|r| format!("{}:{}:{}", r.name, if r.is_error { "ERR" } else { "ok" }, r.text)).collect();
            self.sent.borrow_mut().push(format!("RESULTS {} images={}", text.join(" | "), images.len()));
            self.next = Some(if self.replies.is_empty() { vec![BrainEvent::TurnDone] } else { self.replies.remove(0) });
        }
        fn poll(&mut self) -> Vec<BrainEvent> {
            self.next.take().unwrap_or_default()
        }
        fn cancel(&mut self) {
            self.next = None;
        }
    }

    fn call(id: &str, name: &str, args: &[(&str, &str)]) -> BrainEvent {
        BrainEvent::Call { call_id: id.into(), name: name.into(), args: Ok(Args(args.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())) }
    }

    fn host_tools() -> Vec<ToolDef> {
        vec![
            ToolDef::new("write_motion", "w", r#"{"type":"object","properties":{"name":{"type":"string"},"source":{"type":"string"}},"required":["name","source"]}"#).mutating(),
            ToolDef::new("inspect", "i", r#"{"type":"object","properties":{"target":{"type":"string"}}}"#).inspecting(),
            ToolDef::new("strip", "s", r#"{"type":"object","properties":{"target":{"type":"string"},"count":{"type":"integer"}}}"#).with_images(),
        ]
    }

    fn agent(replies: Vec<Vec<BrainEvent>>, vision: bool) -> (Agent, Rc<RefCell<Vec<String>>>) {
        let sent = Rc::new(RefCell::new(Vec::new()));
        let brain = Script { replies, next: None, sent: sent.clone(), vision };
        let mut tools = own_tools();
        tools.extend(host_tools());
        let mut agent = Agent::with_brain(Box::new(brain), tools, Limits::default());
        agent.observe(vec![Doc { key: "cut".into(), label: "the cut".into(), source: "Flow{}".into() }]);
        agent.seen.insert("cut".into(), "Flow{}".into());
        agent.state = State::Thinking;
        agent.brain.send_user("go".into(), Vec::new());
        (agent, sent)
    }

    fn png() -> BrainImage {
        BrainImage { label: "strip of the cut".into(), png: Arc::from(&b"png"[..]) }
    }

    #[test]
    fn plan_write_inspect_look_finish() {
        let (mut agent, sent) = agent(
            vec![
                vec![call("1", "storyboard", &[("scenes", r#"[{"id":"hook","start":0,"length":3,"idea":"tap"}]"#)]), call("2", "write_motion", &[("name", "hook"), ("source", "use mod.motion.*; Motion{\n}")]), BrainEvent::TurnDone],
                vec![call("3", "finish", &[("summary", "done")]), BrainEvent::TurnDone],
                vec![call("4", "inspect", &[("target", "cut")]), call("5", "strip", &[("count", "8")]), BrainEvent::TurnDone],
                vec![call("6", "critique", &[("subject", "strip"), ("issues", "[]"), ("verdict", "ship")]), call("7", "finish", &[("summary", "A 3 s hook.")]), BrainEvent::TurnDone],
                vec![BrainEvent::Delta("Here it is.".into()), BrainEvent::TurnDone],
            ],
            true,
        );
        // Round 1: the storyboard is the agent's; write_motion goes to the host.
        let actions = agent.poll();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].name, "write_motion");
        assert_eq!(actions[0].args.get("source").and_then(Value::as_str), Some("use mod.motion.*; Motion{\n}"));
        assert!(agent.storyboard().is_some());
        assert!(agent.poll().is_empty(), "waits for the host");
        agent.result("2", Outcome { text: "published asset(\"a\",\"1\")".into(), changes: vec![DocChange { key: "motion:a".into(), label: "motion hook".into(), before: String::new(), after: "use mod.motion.*; Motion{\n}".into() }], ..Outcome::default() });
        assert_eq!(agent.revisions().len(), 1);
        // Round 2: finish before inspecting is refused.
        assert!(agent.poll().is_empty());
        assert!(agent.poll().is_empty());
        assert!(sent.borrow().last().unwrap().contains("finish:ERR:Not finished: inspect the cut first"));
        // Round 3: inspect and look.
        let actions = agent.poll();
        assert_eq!(actions.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), vec!["inspect", "strip"]);
        assert_eq!(actions[1].args.get("count"), Some(&Value::Int(8)));
        agent.result("5", Outcome { text: "8 frames".into(), images: vec![png()], ..Outcome::default() });
        agent.result("4", Outcome { text: "0 errors".into(), inspect: Some(InspectFacts { target: "cut".into(), errors: 0, warnings: 1, blocking: 0 }), ..Outcome::default() });
        assert_eq!(agent.phase(), Phase::Look);
        // Round 4: critique and finish are accepted; round 5 ends it.
        assert!(agent.poll().is_empty());
        assert!(sent.borrow().last().unwrap().contains("images=1"));
        agent.poll();
        agent.poll();
        assert_eq!(agent.state(), &State::Done);
        assert!(matches!(agent.entries().last(), Some(Entry::Done(summary)) if summary == "A 3 s hook."));
        assert!(agent.entries().iter().any(|e| matches!(e, Entry::Critique(c) if c.verdict == "ship")));
    }

    #[test]
    fn hand_edits_reach_the_model_with_the_next_instruction() {
        let (mut agent, sent) = agent(vec![vec![BrainEvent::Delta("hi".into()), BrainEvent::TurnDone]], false);
        agent.limits.idle_replies = 1;
        agent.poll();
        agent.poll();
        assert!(matches!(agent.state(), State::Stopped(_)));
        agent.observe(vec![Doc { key: "cut".into(), label: "the cut".into(), source: "Flow{\n  duration: 20.0\n}".into() }]);
        agent.say("make scene 2 punchier".into());
        let last = sent.borrow().last().unwrap().clone();
        assert!(last.starts_with("USER make scene 2 punchier") && last.contains("[the person edited the cut by hand]") && last.contains("+   duration: 20.0"), "{last}");
        assert_eq!(agent.revisions().last().unwrap().by, Author::Person);
    }

    #[test]
    fn bad_arguments_unknown_tools_and_the_round_budget() {
        let mut replies = vec![vec![call("1", "strip", &[("count", "many")]), call("2", "paint", &[]), BrainEvent::TurnDone]];
        for i in 0..10 {
            replies.push(vec![call(&format!("x{i}"), "storyboard", &[("scenes", r#"[{"start":0,"length":1}]"#)]), BrainEvent::TurnDone]);
        }
        let (mut agent, sent) = agent(replies, false);
        agent.limits.rounds = 5;
        assert!(agent.poll().is_empty());
        let first = sent.borrow()[1].clone();
        assert!(first.contains("strip:ERR:`count` must be a whole number") && first.contains("paint:ERR:there is no tool `paint`"), "{first}");
        for _ in 0..10 {
            agent.poll();
        }
        assert!(matches!(agent.state(), State::Stopped(why) if why.contains("round budget")));
        assert!(sent.borrow().iter().any(|s| s.contains("Three rounds left")));
    }

    #[test]
    fn finish_does_not_wait_for_pictures_that_keep_failing() {
        let mut replies = vec![vec![call("1", "inspect", &[]), BrainEvent::TurnDone]];
        for i in 0..3 {
            replies.push(vec![call(&format!("s{i}"), "strip", &[]), BrainEvent::TurnDone]);
        }
        replies.push(vec![call("f", "finish", &[("summary", "ok")]), BrainEvent::TurnDone]);
        let (mut agent, sent) = agent(replies, true);
        agent.poll();
        agent.result("1", Outcome { inspect: Some(InspectFacts { target: "cut".into(), errors: 0, warnings: 0, blocking: 0 }), ..Outcome::ok("clean") });
        for i in 0..3 {
            agent.poll();
            let actions = agent.poll();
            assert_eq!(actions.len(), 1, "round {i}");
            agent.result(&actions[0].call_id, Outcome::error("the picture was not drawn"));
        }
        agent.poll();
        agent.poll();
        assert!(sent.borrow().last().unwrap().contains("finish:ok:Finished"), "{:?}", sent.borrow().last());
    }

    #[test]
    fn finish_waits_for_safe_area_warnings_then_gives_way() {
        let mut replies = vec![vec![call("1", "inspect", &[]), BrainEvent::TurnDone]];
        for i in 0..3 {
            replies.push(vec![call(&format!("f{i}"), "finish", &[("summary", "ok")]), BrainEvent::TurnDone]);
        }
        let (mut agent, sent) = agent(replies, false);
        agent.poll();
        agent.result("1", Outcome { inspect: Some(InspectFacts { target: "cut".into(), errors: 0, warnings: 2, blocking: 2 }), ..Outcome::ok("2 warnings") });
        for _ in 0..8 {
            agent.poll();
        }
        let sent = sent.borrow();
        assert_eq!(sent.iter().filter(|s| s.contains("finish:ERR:Not finished: safe-area, overflow or overlap warnings are left in `cut` (2)")).count(), 2, "{sent:?}");
        assert!(sent.last().unwrap().contains("finish:ok:Finished"), "{:?}", sent.last());
    }

    #[test]
    fn finish_keeps_waiting_while_the_warnings_go_down() {
        let mut replies = Vec::new();
        for i in 0..6 {
            replies.push(vec![call(&format!("i{i}"), "inspect", &[]), BrainEvent::TurnDone]);
            replies.push(vec![call(&format!("f{i}"), "finish", &[("summary", "ok")]), BrainEvent::TurnDone]);
        }
        let (mut agent, sent) = agent(replies, false);
        // 9, 8, 7, 7: two refusals, two more while the count drops, then it gives way.
        for (i, blocking) in [9, 8, 7, 7, 7].into_iter().enumerate() {
            agent.poll();
            agent.result(&format!("i{i}"), Outcome { inspect: Some(InspectFacts { target: "cut".into(), errors: 0, warnings: blocking, blocking }), ..Outcome::ok("warnings") });
            for _ in 0..3 {
                agent.poll();
            }
        }
        let sent = sent.borrow();
        let refused = sent.iter().filter(|s| s.contains("finish:ERR:Not finished: safe-area")).count();
        assert_eq!(refused, 3, "{sent:?}");
        assert!(sent.iter().any(|s| s.contains("finish:ok:Finished")), "{sent:?}");
    }

    #[test]
    fn a_clean_edit_inspect_clears_older_document_counts() {
        let replies = vec![
            vec![call("1", "inspect", &[("target", "verse")]), BrainEvent::TurnDone],
            vec![call("2", "inspect", &[]), BrainEvent::TurnDone],
            vec![call("3", "finish", &[("summary", "ok")]), BrainEvent::TurnDone],
        ];
        let (mut agent, sent) = agent(replies, false);
        agent.poll();
        agent.result("1", Outcome { inspect: Some(InspectFacts { target: "verse".into(), errors: 0, warnings: 59, blocking: 59 }), ..Outcome::ok("59 warnings") });
        agent.poll();
        agent.result("2", Outcome { inspect: Some(InspectFacts { target: "cut".into(), errors: 0, warnings: 0, blocking: 0 }), ..Outcome::ok("clean") });
        for _ in 0..4 {
            agent.poll();
        }
        let sent = sent.borrow();
        assert!(!sent.iter().any(|s| s.contains("warnings are left")), "{sent:?}");
    }

    #[test]
    fn oversized_pictures_are_left_out_with_a_note() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&2000u32.to_be_bytes());
        png.extend_from_slice(&500u32.to_be_bytes());
        assert!(!fits(&png));
        png[16..20].copy_from_slice(&1024u32.to_be_bytes());
        assert!(fits(&png));
    }

    #[test]
    fn asking_waits_for_the_person() {
        let (mut agent, sent) = agent(vec![vec![call("1", "ask", &[("question", "Which treatment?"), ("options", r#"["neon", "paper"]"#)]), BrainEvent::TurnDone], vec![BrainEvent::Delta("Pick one.".into()), BrainEvent::TurnDone]], false);
        for _ in 0..4 {
            agent.poll();
        }
        assert!(matches!(agent.state(), State::Stopped(why) if why == "waiting for your choice"));
        assert!(agent.entries().iter().any(|e| matches!(e, Entry::Ask { options, .. } if options.len() == 2)));
        assert!(!sent.borrow().iter().any(|s| s.contains("You replied without calling a tool")));
    }

    #[test]
    fn cancel_stops_and_ignores_late_results() {
        let (mut agent, _) = agent(vec![vec![call("1", "inspect", &[]), BrainEvent::TurnDone]], false);
        assert_eq!(agent.poll().len(), 1);
        agent.cancel();
        agent.result("1", Outcome::ok("late"));
        assert!(matches!(agent.state(), State::Stopped(why) if why == "cancelled"));
        assert!(agent.poll().is_empty());
    }
}
