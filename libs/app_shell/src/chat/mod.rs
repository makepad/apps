//! The window's one chat: the aichat panel (`makepad_app_aichat::AiChatPanel`),
//! docked beside the pages, with the product's tools as its services.
//!
//! The shell owns the plumbing every product window shares; the product
//! says what is its own through [`ChatHost`]:
//!
//! - **Services.** The product hands over one service manifest per target
//!   ([`ChatHost::chat_services`]), each a subset of its tools, so a turn
//!   never sees all of them. A call the model makes (or a card the person
//!   confirms) goes to [`ChatHost::chat_turn_call`] with the person's turn it
//!   belongs to (by default [`ChatHost::chat_call`]) and its answer goes back
//!   when [`ChatHost::chat_answer`] has it; a long call keeps the chat's
//!   deadline alive with [`ChatHost::chat_progress`]. Each line the person
//!   sends starts their next turn when the model gets it: at once, or when the turn it waits
//!   behind ends ([`ChatHost::chat_turn_started`]). The engine's line log says so; a line a stop
//!   drops starts none.
//! - **Turn actions.** A product puts buttons under a finished turn's answer
//!   ([`ChatDock::set_turn_actions`]). The first call the turn made locates the
//!   answer in the transcript. A press comes back to [`ChatHost::chat_turn_action`].
//! - **Target.** What the person looks at ([`ChatHost::chat_target`]) is
//!   the panel's target chip: it scopes the turn's tools to that target's
//!   service and tells the model what is open.
//! - **Suggestions.** The target's chips open a settings card with no model
//!   turn.
//! - **Pictures.** While the person types, the picture the product names
//!   ([`ChatHost::chat_look`], [`ChatHost::chat_picture`]) is read back once
//!   and goes to a model that takes images with the line ([`picture`]).
//! - **Marks.** A product that lets the person mark its page for the AI
//!   ([`ChatHost::chat_marks`]) gets the Annotate button and a chip with the marks' count. A line
//!   sent with marks waits until the product has them resolved; the shell then reads the page's
//!   picture back, the product paints the marks in it and reads their text for it
//!   ([`ChatHost::chat_marks_picture`], [`ChatHost::chat_marks_text`]), and the line goes with
//!   that picture (it is a picture of the chat like the others) and the text in its context.
//! - **Notes.** Pages say what happened through a [`ChatOutbox`]: lines in
//!   the transcript, a card to open, a line as if typed. A line sent with a receipt
//!   ([`ChatOutbox::say_tracked`]) is sent exactly as written, and the product hears what became of
//!   it ([`ChatHost::chat_said`]).
//!
//! The window's template names the panel `ai_chat` and the splitter that
//! docks it `chat_split`. F10, the bridge's `/ai` routes and a toolbar
//! button reach the one panel ([`take_requests`]); the window's own F10
//! overlay is never used, so there is one engine.
//!
//! Main-loop cost: every pump drains the services' channels (empty most of
//! the time), reads each call's progress and task card once, and compares the
//! length of the engine's line log with what it read (one compare when no line
//! started or dropped). Task cards keep
//! a quarter-second timer; changed readings are sent at most four times a second.
//! The target, the suggestions and the picture are looked at ten times a second
//! at most, and only while the chat is showing.
//! Turn actions add one flag check per pump. When a turn starts, the shell
//! walks the transcript's entries once per kept turn to find its first call.

pub mod picture;

use makepad_ai_services::engine::{ModelImage, ServiceRegistry};
use makepad_ai_services::port::{AiServicePort, PortEvent};
use makepad_ai_services::state::{EngineState, Entry, LineFate};
use makepad_ai_services::wire::{CallOrigin, ServiceManifest, TaskCard, ToolResult};
use makepad_ai_services::{Suggestion, Target};
use makepad_app_aichat::attach::Source;
use makepad_app_aichat::{AiChatPanel, AiChatPanelAction, MarksUi, TurnActions, TurnButton, VoiceUi};
use makepad_strict_json::Value;
use makepad_widgets::ai_slot::AiSlotRequests;
use makepad_widgets::*;
use picture::{Pixels, Waiting};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// How often the target, the suggestions and the picture are looked at.
const LOOK_EVERY: f64 = 0.1;
/// A call in flight tells the chat it is alive this often (the chat gives
/// up on a silent call after a minute).
const ALIVE_EVERY: f64 = 5.0;
/// The shortest interval between changed progress readings.
const PROGRESS_EVERY: f64 = 0.25;

/// A call the chat made that the product is still running.
struct Inflight {
    port: usize,
    call_id: String,
    /// The product's id for it.
    id: u64,
    last_alive: f64,
    last_sent: Option<(String, u16, Option<TaskCard>)>,
}

/// What a page asks of the chat.
#[derive(Clone, Debug, PartialEq)]
pub enum ChatOut {
    /// A line in the transcript.
    Note(String),
    /// A settings card of a tool of a service (`app`), opened with no
    /// model turn.
    Propose { app: String, tool: String, prefill: String },
    /// A line as if the person had typed it.
    Say(String),
    /// A line sent exactly as written, whose fate goes to its receipt ([`ChatHost::chat_said`]).
    SayTracked { receipt: u64, words: String },
}

/// What became of a line sent with a receipt ([`ChatOutbox::say_tracked`]). Every receipt hears
/// `Queued` at most once, then either ends in `Cancelled` or `Refused`, or reaches `Started(turn)`
/// (after which it may still hear `Stopped` or `Failed` if the turn is cut short before finishing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatSaid {
    /// It waits behind the turn that runs.
    Queued,
    /// The model got it: it is the person's turn `turn` ([`ChatHost::chat_turn_started`] came
    /// first).
    Started(u64),
    /// A stop dropped it before the model got it.
    Cancelled,
    /// The person stopped the running turn (Stop, Escape, New chat, or model swap) after it started.
    Stopped,
    /// The running turn ended on a model error after it started.
    Failed,
    /// It was not sent, and why.
    Refused(&'static str),
}

/// Receipts are unique in the process: every page's outbox and the dock's own draw from one
/// counter.
static NEXT_RECEIPT: AtomicU64 = AtomicU64::new(1);

/// What a page asks of the chat since the window last looked: the page
/// keeps one, the window hands it to [`ChatDock::post`] each pump.
#[derive(Default)]
pub struct ChatOutbox(Vec<ChatOut>);

impl ChatOutbox {
    /// A line in the transcript (a render finished, an export started).
    pub fn note(&mut self, line: String) {
        self.0.push(ChatOut::Note(line));
    }

    /// Open the settings card of `tool` of the service `app`, prefilled
    /// (an object), and show the chat.
    pub fn propose(&mut self, app: &str, tool: &str, prefill: &Value) {
        self.0.push(ChatOut::Propose { app: app.to_string(), tool: tool.to_string(), prefill: prefill.to_json() });
    }

    /// A line to the chat as if the person had typed it.
    pub fn say(&mut self, words: String) {
        self.0.push(ChatOut::Say(words));
    }

    /// Exactly `words` as the person's line (the composer's text, pictures and marks stay as
    /// they are), and the receipt its fate comes back under ([`ChatHost::chat_said`]).
    pub fn say_tracked(&mut self, words: String) -> u64 {
        let receipt = NEXT_RECEIPT.fetch_add(1, Ordering::Relaxed);
        self.0.push(ChatOut::SayTracked { receipt, words });
        receipt
    }

    pub fn take(&mut self) -> Vec<ChatOut> {
        std::mem::take(&mut self.0)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The picture that goes with the person's next line.
#[derive(Default)]
struct Look {
    /// The readback under way, and what it is a picture of.
    grab: Option<(Waiting, String, String)>,
    /// What the model was last given a picture of.
    attached: Option<String>,
    /// The picture the model could not take (it does not see images).
    refused: Option<String>,
    /// The person's lines the transcript had when `attached` was set.
    lines: usize,
}

/// A line's marks on their way: the page's picture, then the marks' text for it.
#[derive(Default)]
enum MarksFlow {
    #[default]
    Idle,
    /// The page's picture is being read back.
    Reading(Waiting),
    /// The picture is painted and the product is reading the marks' text for it.
    Text(Option<Pixels>),
}

/// The chat's state in the window.
#[derive(Default)]
pub struct ChatDock {
    marks: MarksFlow,
    /// The panel and its composer, found once.
    panel: Option<WidgetRef>,
    input: Option<TextInputRef>,
    linked: bool,
    /// The registry was told the product's services are on their way ([`link`]).
    promised: bool,
    /// A frame was drawn, and whether the event being handled is that draw: the product's
    /// services register once the first frame is on screen ([`link`]).
    drawn: bool,
    in_draw: bool,
    /// The panel was told to keep the product's conversation.
    kept: bool,
    ports: Vec<AiServicePort>,
    inflight: Vec<Inflight>,
    timer: Timer,
    timer_due: f64,
    outbox: ChatOutbox,
    /// The suggestions the panel was last given.
    suggestions: Vec<Suggestion>,
    checked: f64,
    /// When the picture was last looked at.
    looked: f64,
    /// The splitter shows the chat.
    open: bool,
    look: Look,
    /// The person's turn the conversation's calls belong to now, from 1 ([`ChatDock::read_log`]).
    turn: u64,
    /// Each turn's first call id, in turn order.
    turn_calls: Vec<(u64, String)>,
    /// The buttons the product set, in turn order.
    turn_actions: Vec<(u64, Vec<TurnButton>)>,
    /// The panel needs the current turns and buttons.
    turn_actions_changed: bool,
    /// The engine's line log entries read so far.
    log_read: usize,
    /// Lines sent with a receipt that have not started or been dropped yet.
    tracked: Vec<Tracked>,
    /// The tracked line that is running, `(line id, receipt)`, until it stops or fails or the next
    /// turn starts.
    active_tracked: Option<(u64, u64)>,
}

/// A line sent with a receipt, until it starts or is dropped.
struct Tracked {
    line: u64,
    receipt: u64,
    /// It heard it waits.
    queued: bool,
}

/// What the line log says, for the product, in order.
enum Heard {
    Turn(u64),
    Said(u64, ChatSaid),
}

impl ChatDock {
    /// Whether the chat shows.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Look at the target and the suggestions again on the next pump (the
    /// page changed what the chat should say).
    pub fn refresh(&mut self) {
        self.checked = 0.0;
    }

    /// Set the buttons under a turn's answer. An empty list removes them.
    pub fn set_turn_actions(&mut self, turn: u64, actions: Vec<TurnButton>) {
        match self.turn_actions.binary_search_by_key(&turn, |(turn, _)| *turn) {
            Ok(at) if self.turn_actions[at].1 == actions => return,
            Ok(at) if actions.is_empty() => { self.turn_actions.remove(at); }
            Ok(at) => self.turn_actions[at].1 = actions,
            Err(_) if actions.is_empty() => return,
            Err(at) => self.turn_actions.insert(at, (turn, actions)),
        }
        self.turn_actions_changed = true;
    }

    /// Whether the shell keeps the turn's first call so the transcript can show its buttons.
    pub fn has_turn(&self, turn: u64) -> bool {
        self.turn_calls.binary_search_by_key(&turn, |(turn, _)| *turn).is_ok()
    }

    /// What pages asked of the chat; shown on the next pump.
    pub fn post(&mut self, outs: Vec<ChatOut>) {
        self.outbox.0.extend(outs);
    }

    /// The chat's own outbox (for the window's own notes).
    pub fn outbox(&mut self) -> &mut ChatOutbox {
        &mut self.outbox
    }

    /// The progress and keep-alive timer, and the first frame ([`link`] waits for it).
    pub fn handle_event(&mut self, event: &Event) {
        if self.timer.is_event(event).is_some() {
            self.timer = Timer::default();
        }
        self.in_draw = matches!(event, Event::Draw(_));
        self.drawn |= self.in_draw;
    }

    /// The line log since the last read, as the person's turns and the receipts' news, in order.
    /// The turn moves only when a line really starts. A tracked line still waiting after the read
    /// hears so once. The first calls of turns gone from the transcript go, with their buttons.
    fn read_log(&mut self, state: &EngineState) -> Vec<Heard> {
        let log = &state.line_log;
        let mut heard = Vec::new();
        let mut started = false;
        for fate in &log[self.log_read..] {
            match *fate {
                LineFate::Started(line) => {
                    self.turn += 1;
                    started = true;
                    self.active_tracked = None;
                    heard.push(Heard::Turn(self.turn));
                    if let Some(at) = self.tracked.iter().position(|t| t.line == line) {
                        let receipt = self.tracked.remove(at).receipt;
                        self.active_tracked = Some((line, receipt));
                        heard.push(Heard::Said(receipt, ChatSaid::Started(self.turn)));
                    }
                }
                LineFate::Dropped(line) => {
                    if let Some(at) = self.tracked.iter().position(|t| t.line == line) {
                        heard.push(Heard::Said(self.tracked.remove(at).receipt, ChatSaid::Cancelled));
                    }
                }
                LineFate::Stopped(line) => {
                    if let Some((_, receipt)) = self.active_tracked.take_if(|(active, _)| *active == line) {
                        heard.push(Heard::Said(receipt, ChatSaid::Stopped));
                    }
                }
                LineFate::Failed(line) => {
                    if let Some((_, receipt)) = self.active_tracked.take_if(|(active, _)| *active == line) {
                        heard.push(Heard::Said(receipt, ChatSaid::Failed));
                    }
                }
            }
        }
        self.log_read = log.len();
        for tracked in self.tracked.iter_mut().filter(|t| !t.queued) {
            tracked.queued = true;
            heard.push(Heard::Said(tracked.receipt, ChatSaid::Queued));
        }
        if started {
            let (turn_calls, turn_actions) = (&mut self.turn_calls, &mut self.turn_actions);
            let mut changed = false;
            turn_calls.retain(|(turn, call_id)| {
                if state.tool(call_id).is_some() {
                    return true;
                }
                if let Ok(at) = turn_actions.binary_search_by_key(turn, |(turn, _)| *turn) {
                    turn_actions.remove(at);
                }
                changed = true;
                false
            });
            self.turn_actions_changed |= changed;
        }
        heard
    }

    /// The panel, once the window has it.
    pub fn panel(&mut self, cx: &mut Cx, window: &WidgetRef) -> Option<WidgetRef> {
        if self.panel.is_none() {
            // By its splitter: every `Window` also holds an `ai_chat` slot.
            let panel = window.widget(cx, ids!(chat_split.ai_chat));
            if panel.is_empty() {
                return None;
            }
            self.input = Some(panel.text_input(cx, ids!(input)));
            self.panel = Some(panel);
        }
        self.panel.clone()
    }
}

/// A picture for the person's next line: what it is of (taken once per
/// key), the texture showing it and the label the model reads.
pub struct ChatPicture {
    pub texture: Option<Texture>,
    pub label: String,
}

/// The conversation turn making a tool call, or a caller outside the conversation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChatCaller {
    Turn(u64),
    #[default]
    Outside,
}

impl ChatCaller {
    pub fn from_origin(origin: CallOrigin, turn: u64) -> Self {
        match origin {
            CallOrigin::Conversation => Self::Turn(turn),
            CallOrigin::Remote => Self::Outside,
        }
    }

    pub fn turn(self) -> Option<u64> {
        match self {
            Self::Turn(turn) => Some(turn),
            Self::Outside => None,
        }
    }
}

/// What the product tells the chat.
pub trait ChatHost {
    /// The chat's state, kept by the product.
    fn chat_dock(&mut self) -> &mut ChatDock;
    /// The window (its template has `ai_chat` and `chat_split`).
    fn chat_window(&self) -> WidgetRef;
    /// The services the chat links once: one manifest per target.
    fn chat_services(&mut self) -> Vec<ServiceManifest>;
    /// Run a tool for the chat; the id its answer comes back under.
    fn chat_call(&mut self, cx: &mut Cx, tool: &str, args: Value) -> u64;
    /// Run a tool for its caller. The person's first line the model gets starts turn 1, and each
    /// one after it the next turn ([`ChatHost::chat_turn_started`]); a call belongs to the turn
    /// running when the model made it. Remote calls arrive as [`ChatCaller::Outside`]. A product
    /// can use the turn to group work for undo. By default the call runs as
    /// [`ChatHost::chat_call`].
    fn chat_turn_call(&mut self, cx: &mut Cx, tool: &str, args: Value, _caller: ChatCaller) -> u64 {
        self.chat_call(cx, tool, args)
    }
    /// A call refused before dispatch. The argument text is private and may be malformed.
    fn chat_refused(&mut self, _cx: &mut Cx, _tool: &str, _call_id: &str, _raw_args: &str, _reason: &str, _caller: ChatCaller) {}
    /// The model got the person's line: their turn `turn` starts (at once, or when the turn the
    /// line waited behind ended). A line a stop drops before that starts no turn, and a restored
    /// conversation starts none. A product ends here what showed for their last turn.
    fn chat_turn_started(&mut self, _cx: &mut Cx, _turn: u64) {}
    /// What became of the line sent with `receipt` ([`ChatOutbox::say_tracked`]).
    fn chat_said(&mut self, _cx: &mut Cx, _receipt: u64, _said: ChatSaid) {}
    /// The person pressed the button the product put under turn `turn`'s answer, with id `_id`.
    fn chat_turn_action(&mut self, _cx: &mut Cx, _turn: u64, _id: &str) {}
    /// The answer of call `id` (made for the chat's `call_id`) once it is
    /// in, with the pictures that go to the model.
    fn chat_answer(&mut self, call_id: &str, id: u64) -> Option<ToolResult>;
    /// How far a call still running is: a stage and permille.
    fn chat_progress(&self, id: u64) -> Option<(String, u16)>;
    /// The task card of call `id` while it runs: drawn in place of its progress note.
    fn chat_task(&self, _id: u64) -> Option<TaskCard> {
        None
    }
    /// A finished task card's button: the action id the product put on the card.
    fn chat_task_action(&mut self, _cx: &mut Cx, _action: &str) {}
    /// The target the chat is scoped to now (its label is the context line
    /// over the composer), and what it suggests.
    fn chat_target(&mut self, cx: &mut Cx) -> (Target, Vec<Suggestion>);
    /// What a picture of the view now would be of (none: no picture).
    fn chat_look(&mut self, _cx: &mut Cx) -> Option<String> {
        None
    }
    /// The picture of the view now (asked only after `chat_look` named a
    /// new one while the person types).
    fn chat_picture(&mut self, _cx: &mut Cx) -> ChatPicture {
        ChatPicture { texture: None, label: String::new() }
    }
    /// The marks on the page: None when the product offers no marker.
    fn chat_marks(&mut self, _cx: &mut Cx) -> Option<MarksUi> {
        None
    }
    /// The person switched the marker on or off.
    fn chat_marker(&mut self, _cx: &mut Cx, _on: bool) {}
    /// The person cleared the marks.
    fn chat_marks_clear(&mut self, _cx: &mut Cx) {}
    /// The texture that shows the page, to read back for the marks' picture.
    fn chat_marks_capture(&mut self, _cx: &mut Cx) -> Option<Texture> {
        None
    }
    /// The page's picture is back (as the model will get it, at most [`picture::EDGE`] a side;
    /// none: it could not be read): paint the marks in it, and start reading their text for it.
    fn chat_marks_picture(&mut self, _cx: &mut Cx, _pixels: Option<&mut Pixels>) {}
    /// The marks' text for that picture once it is read (Err: why not); the product keeps it
    /// in the target's context. None: still being read.
    fn chat_marks_text(&mut self, _cx: &mut Cx) -> Option<Result<(), String>> {
        Some(Ok(()))
    }
    /// The marks went with the person's line number `line` (1 is the first).
    fn chat_marks_sent(&mut self, _cx: &mut Cx, _line: usize) {}
    /// The chat was shown or hidden: the window lays itself out again.
    fn chat_shown(&mut self, _cx: &mut Cx) {}
    /// The person removed attachment `id` (a pick) from the chat's composer: drop it on the page.
    fn chat_attachment_removed(&mut self, _cx: &mut Cx, _id: &str) {}
    /// The pointer is over attachment `id` in the composer, or left them (None): light it up.
    fn chat_attachment_hovered(&mut self, _cx: &mut Cx, _id: Option<&str>) {}
    /// The person stopped call `id` in the chat: stop its work (its answer is no longer
    /// wanted).
    fn chat_cancel(&mut self, _cx: &mut Cx, _id: u64) {}
    /// The composer's microphone button: hidden unless the product has a microphone to offer.
    fn chat_voice(&mut self, _cx: &mut Cx) -> VoiceUi {
        VoiceUi::Hidden
    }
    /// The microphone button was pressed.
    fn chat_voice_toggled(&mut self, _cx: &mut Cx) {}
    /// What the microphone has given since the last look: text for the composer, or a line the
    /// person must read.
    fn chat_voice_says(&mut self, _cx: &mut Cx) -> Vec<VoiceSays> {
        Vec::new()
    }
}

/// What a product's microphone gives the chat.
pub enum VoiceSays {
    /// What was said: written into the composer.
    Text(String),
    /// A line to read (why the microphone cannot be used).
    Notice(String),
}

/// Show or hide the chat.
pub fn set_open<H: ChatHost>(host: &mut H, cx: &mut Cx, open: bool) {
    let window = host.chat_window();
    let split = window.splitter(cx, ids!(chat_split));
    split.set_collapse(cx, if open { SplitterCollapse::None } else { SplitterCollapse::B });
    let dock = host.chat_dock();
    dock.open = open;
    cx.global::<AiSlotRequests>().is_open = open;
    if open {
        dock.checked = 0.0;
        dock.panel(cx, &window);
        if let Some(input) = &dock.input {
            input.set_key_focus(cx);
        }
    }
    host.chat_shown(cx);
    cx.redraw_all();
}

pub fn toggle<H: ChatHost>(host: &mut H, cx: &mut Cx) {
    let open = !host.chat_dock().open;
    set_open(host, cx, open);
}

/// F10, the bridge's `/ai` routes and the toolbar button reach the one
/// panel: the window's own overlay never gets the request.
pub fn take_requests<H: ChatHost>(host: &mut H, cx: &mut Cx) {
    let requests = cx.global::<AiSlotRequests>();
    let open = requests.open.take();
    let say = std::mem::take(&mut requests.say);
    let answers = std::mem::take(&mut requests.confirm);
    if let Some(open) = open {
        set_open(host, cx, open);
    }
    if !answers.is_empty() {
        let window = host.chat_window();
        if let Some(panel) = host.chat_dock().panel(cx, &window) {
            if let Some(mut panel) = panel.borrow_mut::<AiChatPanel>() {
                for how in &answers {
                    panel.answer_cards(cx, how);
                }
            }
        }
    }
    if !say.is_empty() {
        set_open(host, cx, true);
        let window = host.chat_window();
        if let Some(panel) = host.chat_dock().panel(cx, &window) {
            if let Some(mut panel) = panel.borrow_mut::<AiChatPanel>() {
                for line in say {
                    panel.say(cx, line);
                }
            }
        }
    }
}

/// Escape in the chat hides it; removing an attachment from the composer drops it in the
/// product, and hovering one lets the product light it up.
pub fn handle_actions<H: ChatHost>(host: &mut H, cx: &mut Cx, actions: &Actions) {
    for action in actions {
        if let Some(widget_action) = action.as_widget_action() {
            match widget_action.cast() {
                AiChatPanelAction::Close => set_open(host, cx, false),
                AiChatPanelAction::AttachmentRemoved(id) => {
                    host.chat_attachment_removed(cx, &id);
                    host.chat_dock().checked = 0.0;
                    cx.redraw_all();
                }
                AiChatPanelAction::AttachmentHovered(id) => host.chat_attachment_hovered(cx, id.as_deref()),
                AiChatPanelAction::TaskAction { action, .. } => host.chat_task_action(cx, &action),
                AiChatPanelAction::TurnAction { turn, id } => host.chat_turn_action(cx, turn, &id),
                AiChatPanelAction::VoiceToggled => host.chat_voice_toggled(cx),
                AiChatPanelAction::Annotate(on) => host.chat_marker(cx, on),
                AiChatPanelAction::MarksCleared => host.chat_marks_clear(cx),
                AiChatPanelAction::MarksWanted => {
                    let texture = host.chat_marks_capture(cx);
                    host.chat_dock().marks = MarksFlow::Reading(Waiting::new(cx, texture));
                    cx.new_next_frame();
                }
                _ => {}
            }
        }
    }
}

/// Scope the chat to the target now and give the panel its suggestions (a
/// product that drives the chat itself, then says a line).
pub fn retarget<H: ChatHost>(host: &mut H, cx: &mut Cx) -> Option<WidgetRef> {
    let window = host.chat_window();
    let panel = host.chat_dock().panel(cx, &window)?;
    let (target, _) = host.chat_target(cx);
    if let Some(mut chat) = panel.borrow_mut::<AiChatPanel>() {
        chat.set_target(cx, Some(target));
    }
    host.chat_dock().checked = 0.0;
    Some(panel)
}

/// The chat's turn in the window's pump.
pub fn pump<H: ChatHost>(host: &mut H, cx: &mut Cx) {
    link(host, cx);
    let window = host.chat_window();
    let Some(panel) = host.chat_dock().panel(cx, &window) else {
        // no chat column yet: a caller from outside (`makepad-remote app <pid> call`) is served
        run_calls(host, cx);
        return;
    };
    // The product's conversation survives a restart (a crash, a live-patch
    // relaunch) until the person starts a new chat: keyed before anything
    // starts the engine, so the saved chat is what it starts with.
    if !host.chat_dock().kept {
        host.chat_dock().kept = true;
        let (target, _) = host.chat_target(cx);
        if let Some(mut chat) = panel.borrow_mut::<AiChatPanel>() {
            chat.keep_chat(&target.app);
        }
    }
    take_requests(host, cx);
    follow_log(host, cx, &panel);
    let voice = host.chat_voice(cx);
    let says = host.chat_voice_says(cx);
    if let Some(mut chat) = panel.borrow_mut::<AiChatPanel>() {
        chat.set_voice(cx, voice);
        for say in says {
            match say {
                VoiceSays::Text(text) => chat.dictate(cx, &text),
                VoiceSays::Notice(text) => chat.notify(cx, text),
            }
        }
    }
    run_calls(host, cx);
    let dock = host.chat_dock();
    if dock.turn_actions_changed {
        if let Some(mut chat) = panel.borrow_mut::<AiChatPanel>() {
            let actions = dock.turn_actions.iter().filter_map(|(turn, actions)| {
                let at = dock.turn_calls.binary_search_by_key(turn, |(turn, _)| *turn).ok()?;
                Some(TurnActions { turn: *turn, call_id: dock.turn_calls[at].1.clone(), actions: actions.clone() })
            }).collect();
            chat.set_turn_actions(cx, actions);
            dock.turn_actions_changed = false;
        }
    }
    drain_outbox(host, cx, &panel);
    // The splitter is the truth about whether the chat shows (the person
    // drags and folds it too).
    let shown = window.splitter(cx, ids!(chat_split)).collapse() == SplitterCollapse::None;
    if shown != host.chat_dock().open {
        let dock = host.chat_dock();
        dock.open = shown;
        dock.checked = 0.0;
        cx.global::<AiSlotRequests>().is_open = shown;
        host.chat_shown(cx);
    }
    if !shown {
        return;
    }
    let now = cx.seconds_since_app_start();
    if now - host.chat_dock().checked >= LOOK_EVERY {
        host.chat_dock().checked = now;
        let (target, suggestions) = host.chat_target(cx);
        if let Some(mut chat) = panel.borrow_mut::<AiChatPanel>() {
            if chat.state().and_then(|state| state.target.as_ref()) != Some(&target) {
                chat.set_target(cx, Some(target));
            }
            let dock = host.chat_dock();
            if dock.suggestions != suggestions {
                chat.set_suggestions(cx, suggestions.clone());
                dock.suggestions = suggestions;
            }
        }
    }
    pump_marks(host, cx, &panel);
    let dock = host.chat_dock();
    if dock.look.grab.is_some() || now - dock.looked >= LOOK_EVERY {
        dock.looked = now;
        pump_look(host, cx, &panel);
    }
}

/// The product's marks to the panel, and a held line's marks on their way.
fn pump_marks<H: ChatHost>(host: &mut H, cx: &mut Cx, panel: &WidgetRef) {
    if let Some(ui) = host.chat_marks(cx) {
        if let Some(mut chat) = panel.borrow_mut::<AiChatPanel>() {
            chat.set_marks(cx, ui);
        }
    }
    match std::mem::take(&mut host.chat_dock().marks) {
        MarksFlow::Idle => {}
        MarksFlow::Reading(mut waiting) => match waiting.poll(cx) {
            None => host.chat_dock().marks = MarksFlow::Reading(waiting),
            Some(result) => {
                let mut pixels = match result {
                    Ok(pixels) => Some(pixels.fit(picture::EDGE)),
                    Err(why) => {
                        log!("chat: no picture for the marks: {why}");
                        None
                    }
                };
                host.chat_marks_picture(cx, pixels.as_mut());
                host.chat_dock().marks = MarksFlow::Text(pixels);
                cx.new_next_frame();
            }
        },
        MarksFlow::Text(pixels) => match host.chat_marks_text(cx) {
            None => {
                host.chat_dock().marks = MarksFlow::Text(pixels);
                cx.new_next_frame();
            }
            Some(result) => {
                if let Err(why) = result {
                    log!("chat: the marks' text: {why}");
                }
                // the target's context now has the marks: the line is the next one
                let lines = panel.borrow::<AiChatPanel>().and_then(|chat| chat.state().map(|state| state.entries.iter().filter(|entry| matches!(entry, Entry::User { .. })).count())).unwrap_or(0);
                retarget(host, cx);
                host.chat_marks_sent(cx, lines + 1);
                let source = pixels.map(|p| Source::Rgba { name: "marks.png".into(), width: p.width, height: p.height, rgba: Arc::new(p.rgba) });
                if let Some(mut chat) = panel.borrow_mut::<AiChatPanel>() {
                    chat.release_marks(cx, source);
                }
            }
        },
    }
}

/// Plug the product's services into the process's registry, once, at the first pump after the
/// first frame is on screen: that frame does not wait for the manifests, and a remote `call`
/// reaches them before the chat column was ever opened. Until then the registry knows they are
/// on their way ([`ServiceRegistry::expect`]), so a call from outside that comes earlier waits
/// that frame rather than finding none. The panel shows that same registry (`AiChatPanel`'s is
/// [`ServiceRegistry::process`]), so this is the one registration.
fn link<H: ChatHost>(host: &mut H, cx: &mut Cx) {
    let dock = host.chat_dock();
    if dock.linked {
        return;
    }
    let registry = ServiceRegistry::process();
    if !dock.promised {
        dock.promised = true;
        registry.expect();
    }
    if !dock.drawn || dock.in_draw {
        // the first frame is drawing now: the pump after it links
        if dock.in_draw {
            let _ = cx.new_next_frame();
        }
        return;
    }
    dock.linked = true;
    let manifests = host.chat_services();
    for manifest in manifests {
        let id = manifest.id.clone();
        match AiServicePort::in_process(manifest) {
            Ok((port, link)) => match registry.register(link, "this window", None) {
                Ok(_) => host.chat_dock().ports.push(port),
                Err(error) => error!("chat: the registry would not take {id}: {error}"),
            },
            Err(error) => error!("chat: the {id} service: {error}"),
        }
    }
    registry.expected();
}

/// Calls the model (or a confirmed card) made, to the product; the answers
/// of the ones that finished, back.
fn run_calls<H: ChatHost>(host: &mut H, cx: &mut Cx) {
    let now = cx.seconds_since_app_start();
    let turn = host.chat_dock().turn.max(1);
    for index in 0..host.chat_dock().ports.len() {
        let events = host.chat_dock().ports[index].handle_event(cx, &Event::Signal);
        for event in events {
            let call = match event {
                PortEvent::Call(call) => call,
                PortEvent::Cancel { call_id } => {
                    let flights = &mut host.chat_dock().inflight;
                    if let Some(i) = flights.iter().position(|f| f.port == index && f.call_id == call_id) {
                        let id = flights.remove(i).id;
                        host.chat_cancel(cx, id);
                    }
                    continue;
                }
                _ => continue,
            };
            let caller = ChatCaller::from_origin(call.origin, turn);
            match makepad_strict_json::parse(call.args.as_bytes()) {
                Ok(args) => {
                    let dock = host.chat_dock();
                    if caller == ChatCaller::Turn(turn) && !dock.has_turn(turn) {
                        dock.turn_calls.push((turn, call.call_id.clone()));
                        if dock.turn_actions.binary_search_by_key(&turn, |(turn, _)| *turn).is_ok() {
                            dock.turn_actions_changed = true;
                        }
                    }
                    let id = host.chat_turn_call(cx, &call.tool, args, caller);
                    host.chat_dock().inflight.push(Inflight { port: index, call_id: call.call_id, id, last_alive: now, last_sent: None });
                }
                Err(error) => {
                    let reason = format!("the arguments do not parse: {error}");
                    host.chat_refused(cx, &call.tool, &call.call_id, &call.args, &reason, caller);
                    host.chat_dock().ports[index].reply(ToolResult::refused(&call.call_id, reason));
                }
            }
        }
    }
    if host.chat_dock().inflight.is_empty() {
        let dock = host.chat_dock();
        cx.stop_timer(dock.timer);
        dock.timer = Timer::default();
        return;
    }
    let mut waiting = Vec::new();
    let mut has_task = false;
    for mut flight in std::mem::take(&mut host.chat_dock().inflight) {
        match host.chat_answer(&flight.call_id, flight.id) {
            // Its pictures ride in the result: the engine that made the call hands them to
            // its model.
            Some(reply) => host.chat_dock().ports[flight.port].reply(reply),
            None => {
                let (note, permille) = host.chat_progress(flight.id).unwrap_or_else(|| ("running".to_string(), 0));
                let task = host.chat_task(flight.id);
                has_task |= task.is_some();
                let reading = (note, permille, task);
                if now - flight.last_alive >= ALIVE_EVERY
                    || (flight.last_sent.as_ref() != Some(&reading)
                        && (flight.last_sent.is_none() || now - flight.last_alive >= PROGRESS_EVERY))
                {
                    flight.last_alive = now;
                    host.chat_dock().ports[flight.port].progress_task(&flight.call_id, &reading.0, reading.1, reading.2.clone());
                    flight.last_sent = Some(reading);
                }
                waiting.push(flight);
            }
        }
    }
    let dock = host.chat_dock();
    if waiting.is_empty() {
        cx.stop_timer(dock.timer);
        dock.timer = Timer::default();
    } else {
        let interval = if has_task { PROGRESS_EVERY } else { ALIVE_EVERY / 2.0 };
        let due = now + interval;
        if dock.timer.is_empty() || dock.timer_due > due {
            cx.stop_timer(dock.timer);
            dock.timer = cx.start_timeout(interval);
            dock.timer_due = due;
        }
    }
    dock.inflight = waiting;
}

/// The engine's line log to the product: each line the model gets starts the person's next turn
/// ([`ChatHost::chat_turn_started`]), and each tracked line's receipt hears its fate
/// ([`ChatHost::chat_said`]). One compare per pump when no line started or dropped.
fn follow_log<H: ChatHost>(host: &mut H, cx: &mut Cx, panel: &WidgetRef) {
    let heard = {
        let Some(chat) = panel.borrow::<AiChatPanel>() else { return };
        let Some(state) = chat.state() else { return };
        host.chat_dock().read_log(state)
    };
    tell(host, cx, heard);
}

/// What the line log said, to the product, in order.
fn tell<H: ChatHost>(host: &mut H, cx: &mut Cx, heard: Vec<Heard>) {
    for heard in heard {
        match heard {
            Heard::Turn(turn) => host.chat_turn_started(cx, turn),
            Heard::Said(receipt, said) => host.chat_said(cx, receipt, said),
        }
    }
}

/// What the pages asked of the chat since the last pump.
fn drain_outbox<H: ChatHost>(host: &mut H, cx: &mut Cx, panel: &WidgetRef) {
    if host.chat_dock().outbox.is_empty() {
        return;
    }
    for out in host.chat_dock().outbox.take() {
        match out {
            ChatOut::Note(line) => {
                if let Some(mut chat) = panel.borrow_mut::<AiChatPanel>() {
                    chat.announce(cx, &line);
                }
            }
            ChatOut::Propose { app, tool, prefill } => {
                set_open(host, cx, true);
                if let Some(mut chat) = panel.borrow_mut::<AiChatPanel>() {
                    chat.propose(cx, &app, &tool, &prefill);
                }
            }
            ChatOut::Say(line) => {
                set_open(host, cx, true);
                if let Some(mut chat) = panel.borrow_mut::<AiChatPanel>() {
                    chat.say(cx, line);
                }
            }
            ChatOut::SayTracked { receipt, words } => {
                set_open(host, cx, true);
                let sent = panel.borrow_mut::<AiChatPanel>().map(|mut chat| chat.send_exact(cx, &words));
                match sent.unwrap_or(Err("No AI model is connected")) {
                    Ok(line) => host.chat_dock().tracked.push(Tracked { line, receipt, queued: false }),
                    Err(why) => host.chat_said(cx, receipt, ChatSaid::Refused(why)),
                }
            }
        }
    }
    // The lines just sent: started at once, or waiting.
    follow_log(host, cx, panel);
}

/// The picture that goes with a line the person is writing: read back once
/// per look, while there is text in the composer.
fn pump_look<H: ChatHost>(host: &mut H, cx: &mut Cx, panel: &WidgetRef) {
    let key = host.chat_look(cx);
    // A line sent: the next one gets a fresh picture.
    let lines = panel.borrow::<AiChatPanel>().and_then(|chat| chat.state().map(|state| state.entries.iter().filter(|entry| matches!(entry, Entry::User { .. })).count())).unwrap_or(0);
    let dock = host.chat_dock();
    let line_sent = lines != dock.look.lines;
    // A late readback for a line already sent must not become the next line's picture.
    if line_sent || key.is_none() {
        if let Some(ticket) = dock.look.grab.take().and_then(|(waiting, _, _)| waiting.ticket) {
            cx.cancel_texture_readback(ticket);
        }
    }
    if !line_sent && dock.look.attached.is_some() && key.is_none() {
        if let Some(mut chat) = panel.borrow_mut::<AiChatPanel>() {
            let _ = chat.set_next_line_picture(None);
        }
        dock.look.attached = None;
    }
    if line_sent {
        dock.look.lines = lines;
        dock.look.attached = None;
    }
    // The readback under way.
    if let Some((waiting, look, label)) = dock.look.grab.as_mut() {
        let Some(frame) = waiting.poll(cx) else { return };
        let (look, label) = (look.clone(), label.clone());
        dock.look.grab = None;
        let image = frame.and_then(|pixels: Pixels| pixels.fit(picture::EDGE).png());
        match image {
            Ok(png) => {
                let sent = panel.borrow_mut::<AiChatPanel>().map(|mut chat| chat.set_next_line_picture(Some(ModelImage { label, png: Arc::from(png) })));
                if matches!(sent, Some(Err(_))) {
                    dock.look.refused = Some(look.clone());
                }
                dock.look.attached = Some(look);
            }
            Err(why) => {
                log!("chat: no picture for the model: {why}");
                dock.look.refused = Some(look);
            }
        }
        return;
    }
    let Some(key) = key else { return };
    let typing = dock.input.as_ref().is_some_and(|input| !input.text().trim().is_empty());
    if !typing || dock.look.attached.as_ref() == Some(&key) || dock.look.refused.as_ref() == Some(&key) {
        return;
    }
    let picture = host.chat_picture(cx);
    host.chat_dock().look.grab = Some((Waiting::new(cx, picture.texture), key, picture.label));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancel_in_the_same_drain_reaches_the_admitted_call() {
        use makepad_ai_services::wire::{Risk, ServiceCall, ServiceDown, ToolDef};
        use makepad_ai_services::ServiceRegistry;

        #[derive(Default)]
        struct Host {
            dock: ChatDock,
            events: Vec<(&'static str, u64)>,
        }
        impl ChatHost for Host {
            fn chat_dock(&mut self) -> &mut ChatDock { &mut self.dock }
            fn chat_call(&mut self, _: &mut Cx, _: &str, _: Value) -> u64 {
                let id = 17;
                self.events.push(("admit", id));
                id
            }
            fn chat_cancel(&mut self, _: &mut Cx, id: u64) { self.events.push(("cancel", id)); }
            fn chat_answer(&mut self, _: &str, _: u64) -> Option<ToolResult> { panic!("the cancelled call is still being polled") }
            fn chat_progress(&self, _: u64) -> Option<(String, u16)> { unreachable!() }
            fn chat_target(&mut self, _: &mut Cx) -> (Target, Vec<Suggestion>) { unreachable!() }
            fn chat_services(&mut self) -> Vec<ServiceManifest> { unreachable!() }
            fn chat_window(&self) -> WidgetRef { unreachable!() }
        }

        let mut host = Host::default();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let registry = ServiceRegistry::new();
        let manifest = ServiceManifest::new("files", "Files", "Read a file.")
            .with_tool(ToolDef::new("read", "Read a file.", r#"{"type":"object","properties":{}}"#, Risk::Read));
        let (port, link) = AiServicePort::in_process(manifest).unwrap();
        let endpoint = registry.register(link, "this window", None).unwrap();
        host.dock.ports.push(port);
        registry.pump();
        let call = ServiceCall { call_id: "first".into(), tool: "read".into(), args: "{}".into(), caller: None, origin: CallOrigin::Conversation };
        assert!(registry.send(&endpoint, ServiceDown::Call(call)));
        assert!(registry.send(&endpoint, ServiceDown::Cancel { call_id: "first".into() }));

        run_calls(&mut host, &mut cx);
        assert_eq!(host.events, [("admit", 17), ("cancel", 17)], "the same drain cancels the ID the host just admitted");
        assert!(host.dock.inflight.is_empty(), "the cancelled call is gone before reply polling");
    }

    /// The person's turns follow the lines the model really gets, read from a real engine's line
    /// log: the same line typed again after a stop is a new turn; a line typed during a turn starts
    /// its own when that one ends, and the calls made meanwhile stay the running turn's; a tracked
    /// line hears that it waits, then how it ended.
    #[test]
    fn turns_and_receipts_follow_the_lines_the_model_gets() {
        use makepad_ai_services::engine::{EngineCore, Model, ModelEvent, ToolDefinition};
        use makepad_ai_services::wire::{Risk, ToolDef};
        use std::sync::Mutex;

        /// Answers each tool result by ending its turn; the test ends the others.
        struct Scripted(Arc<Mutex<Vec<ModelEvent>>>);
        impl Model for Scripted {
            fn label(&self) -> String { "Scripted".into() }
            fn configure(&mut self, _: &str, _: &[ToolDefinition]) -> Result<(), String> { Ok(()) }
            fn send_user(&mut self, _: &str, _: &str) {}
            fn send_tool_result(&mut self, _: &str, _: &str, _: bool) {
                self.0.lock().unwrap().push(ModelEvent::TurnDone { tool_calls: 0 });
            }
            fn cancel(&mut self) {}
            fn reset(&mut self) {}
            fn poll(&mut self) -> Vec<ModelEvent> { std::mem::take(&mut self.0.lock().unwrap()) }
        }

        #[derive(Default)]
        struct Host {
            dock: ChatDock,
            calls: Vec<ChatCaller>,
            turns: Vec<u64>,
            said: Vec<(u64, ChatSaid)>,
        }
        impl ChatHost for Host {
            fn chat_dock(&mut self) -> &mut ChatDock { &mut self.dock }
            fn chat_turn_call(&mut self, _: &mut Cx, _: &str, _: Value, caller: ChatCaller) -> u64 {
                self.calls.push(caller);
                self.calls.len() as u64
            }
            fn chat_turn_started(&mut self, _: &mut Cx, turn: u64) { self.turns.push(turn); }
            fn chat_said(&mut self, _: &mut Cx, receipt: u64, said: ChatSaid) { self.said.push((receipt, said)); }
            fn chat_answer(&mut self, call_id: &str, _: u64) -> Option<ToolResult> { Some(ToolResult::ok(call_id, "read", "read")) }
            fn chat_call(&mut self, _: &mut Cx, _: &str, _: Value) -> u64 { unreachable!() }
            fn chat_progress(&self, _: u64) -> Option<(String, u16)> { unreachable!() }
            fn chat_target(&mut self, _: &mut Cx) -> (Target, Vec<Suggestion>) { unreachable!() }
            fn chat_services(&mut self) -> Vec<ServiceManifest> { unreachable!() }
            fn chat_window(&self) -> WidgetRef { unreachable!() }
        }

        let mut host = Host::default();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let registry = ServiceRegistry::new();
        let manifest = ServiceManifest::new("files", "Files", "Read a file.")
            .with_tool(ToolDef::new("read", "Read a file.", r#"{"type":"object","properties":{}}"#, Risk::Read));
        let (port, link) = AiServicePort::in_process(manifest).unwrap();
        registry.register(link, "this window", None).unwrap();
        host.dock.ports.push(port);
        registry.pump();
        let out = Arc::new(Mutex::new(Vec::new()));
        let mut core = EngineCore::new(registry, Box::new(Scripted(out.clone())), None, 1);
        // One window pump: the engine, then the dock reads its log and runs the calls.
        let pump = |host: &mut Host, core: &mut EngineCore, cx: &mut Cx, now: f64| {
            core.pump(now);
            let heard = host.dock.read_log(core.state());
            tell(host, cx, heard);
            run_calls(host, cx);
        };

        core.send("again", 0.0);
        pump(&mut host, &mut core, &mut cx, 0.1);
        core.cancel(0.2);
        core.send("again", 0.3);
        pump(&mut host, &mut core, &mut cx, 0.4);
        assert_eq!(host.turns, [1, 2], "the same line sent again after a stop is a new turn");

        core.send("meanwhile", 0.5);
        pump(&mut host, &mut core, &mut cx, 0.6);
        assert_eq!(host.turns, [1, 2], "a line typed during a turn has not started");
        out.lock().unwrap().extend([
            ModelEvent::ToolCall { call_id: "m1".into(), name: "files.read".into(), args: "{}".into() },
            ModelEvent::TurnDone { tool_calls: 1 },
        ]);
        pump(&mut host, &mut core, &mut cx, 0.7);
        assert_eq!(host.calls, [ChatCaller::Turn(2)], "a call the running turn makes after a line was typed is still that turn's");
        pump(&mut host, &mut core, &mut cx, 0.8);
        assert_eq!(host.turns, [1, 2, 3], "the typed line starts its turn when the running one ends");

        let line = core.send_exact("draft while busy");
        host.dock.tracked.push(Tracked { line, receipt: 41, queued: false });
        pump(&mut host, &mut core, &mut cx, 0.9);
        assert_eq!(host.said, [(41, ChatSaid::Queued)], "a tracked line sent during a turn waits");
        out.lock().unwrap().push(ModelEvent::TurnDone { tool_calls: 0 });
        pump(&mut host, &mut core, &mut cx, 1.0);
        assert_eq!(host.said, [(41, ChatSaid::Queued), (41, ChatSaid::Started(4))], "it starts with the turn after the running one");
        assert_eq!(host.turns, [1, 2, 3, 4], "its turn started before its receipt heard so");

        let line = core.send_exact("draft to drop");
        host.dock.tracked.push(Tracked { line, receipt: 42, queued: false });
        pump(&mut host, &mut core, &mut cx, 1.1);
        core.cancel(1.15);
        pump(&mut host, &mut core, &mut cx, 1.2);
        let line = core.send_exact("draft while idle");
        host.dock.tracked.push(Tracked { line, receipt: 43, queued: false });
        pump(&mut host, &mut core, &mut cx, 1.3);
        assert_eq!(host.said[2..], [(42, ChatSaid::Queued), (41, ChatSaid::Stopped), (42, ChatSaid::Cancelled), (43, ChatSaid::Started(5))],
            "a stop ends the running tracked line, cancels the waiting one; one sent while idle starts at once");
        assert!(host.dock.tracked.is_empty(), "every receipt ended");

        core.clear(1.4);
        pump(&mut host, &mut core, &mut cx, 1.5);
        assert_eq!(host.said[6..], [(43, ChatSaid::Stopped)], "New chat with no tool in flight stops the running tracked line");
        assert_eq!(host.dock.active_tracked, None, "no tracked line runs after the clear");

        let line = core.send_exact("draft over a swap");
        host.dock.tracked.push(Tracked { line, receipt: 44, queued: false });
        pump(&mut host, &mut core, &mut cx, 1.6);
        core.set_model(Box::new(Scripted(out.clone())));
        pump(&mut host, &mut core, &mut cx, 1.7);
        assert_eq!(host.said[7..], [(44, ChatSaid::Started(6)), (44, ChatSaid::Stopped)], "swapping the model stops the running tracked line");

        let line = core.send_exact("draft that fails");
        host.dock.tracked.push(Tracked { line, receipt: 45, queued: false });
        pump(&mut host, &mut core, &mut cx, 1.8);
        let line = core.send_exact("draft behind the failure");
        host.dock.tracked.push(Tracked { line, receipt: 46, queued: false });
        pump(&mut host, &mut core, &mut cx, 1.9);
        out.lock().unwrap().push(ModelEvent::Error("overflow".into()));
        pump(&mut host, &mut core, &mut cx, 2.0);
        assert_eq!(host.said[9..], [(45, ChatSaid::Started(7)), (46, ChatSaid::Queued), (45, ChatSaid::Failed), (46, ChatSaid::Started(8))],
            "a failure ends the running tracked line, then the queued one starts");
        assert_eq!(host.turns, [1, 2, 3, 4, 5, 6, 7, 8], "the next turn starts after the failure");
        out.lock().unwrap().push(ModelEvent::TurnDone { tool_calls: 0 });
        pump(&mut host, &mut core, &mut cx, 2.1);
        let heard = host.said.len();
        core.cancel(2.2);
        pump(&mut host, &mut core, &mut cx, 2.3);
        assert_eq!(host.said.len(), heard, "a stop while idle tells no receipt anything");
    }

    /// What a page asks goes to the dock in order, and the page's outbox is
    /// empty after.
    #[test]
    fn a_page_outbox_reaches_the_dock_in_order() {
        let mut page = ChatOutbox::default();
        page.note("Rendered".into());
        page.propose("cut", "render_cut", &makepad_strict_json::obj(vec![("fps", Value::Int(30))]));
        page.say("make it shorter".into());
        let mut dock = ChatDock::default();
        dock.post(page.take());
        assert!(page.is_empty());
        assert_eq!(dock.outbox().take(), vec![
            ChatOut::Note("Rendered".into()),
            ChatOut::Propose { app: "cut".into(), tool: "render_cut".into(), prefill: "{\"fps\":30}".into() },
            ChatOut::Say("make it shorter".into()),
        ]);
    }
}
