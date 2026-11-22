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
//!   sends starts their next turn ([`ChatHost::chat_turn_started`]).
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
//!   the transcript, a card to open, a line as if typed.
//!
//! The window's template names the panel `ai_chat` and the splitter that
//! docks it `chat_split`. F10, the bridge's `/ai` routes and a toolbar
//! button reach the one panel ([`take_requests`]); the window's own F10
//! overlay is never used, so there is one engine.
//!
//! Main-loop cost: every pump drains the services' channels (empty most of
//! the time), reads each call's progress and task card once, and compares the
//! conversation's generation; when it changed, it walks back over the entries
//! after the person's newest line. Task cards keep
//! a quarter-second timer; changed readings are sent at most four times a second.
//! The target, the suggestions and the picture are looked at ten times a second
//! at most, and only while the chat is showing.
//! Turn actions add one flag check per pump. When a turn starts, the shell
//! walks the transcript's entries once per kept turn to find its first call.

pub mod picture;

use makepad_ai_services::engine::{ModelImage, ServiceRegistry};
use makepad_ai_services::port::{AiServicePort, PortEvent};
use makepad_ai_services::state::Entry;
use makepad_ai_services::wire::{CallOrigin, ServiceManifest, TaskCard, ToolResult};
use makepad_ai_services::{Suggestion, Target};
use makepad_app_aichat::attach::Source;
use makepad_app_aichat::{AiChatPanel, AiChatPanelAction, MarksUi, TurnActions, TurnButton, VoiceUi};
use makepad_strict_json::Value;
use makepad_widgets::ai_slot::AiSlotRequests;
use makepad_widgets::*;
use picture::{Pixels, Waiting};
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
}

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
    /// The person's turn the conversation's calls belong to now, from 1 ([`person_turn`]).
    turn: u64,
    /// Each turn's first call id, in turn order.
    turn_calls: Vec<(u64, String)>,
    /// The buttons the product set, in turn order.
    turn_actions: Vec<(u64, Vec<TurnButton>)>,
    /// The panel needs the current turns and buttons.
    turn_actions_changed: bool,
    /// The conversation's generation the pump last looked at.
    seen_generation: Option<u64>,
    /// The person's newest line then, and how many entries followed it.
    newest_line: Option<(String, usize)>,
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

    /// The panel, once the window has it.
    pub fn panel(&mut self, cx: &mut Cx, window: &WidgetRef) -> Option<WidgetRef> {
        if self.panel.is_none() {
            let panel = window.widget(cx, ids!(ai_chat));
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
    /// Run a tool for the chat. The person's first line starts turn 1 and each new line starts
    /// the next turn. Calls with [`CallOrigin::Remote`] have turn 0. A product that groups a
    /// turn's work for undo keeps it here. By default the call runs as [`ChatHost::chat_call`].
    fn chat_turn_call(&mut self, cx: &mut Cx, tool: &str, args: Value, _turn: u64) -> u64 {
        self.chat_call(cx, tool, args)
    }
    /// The person sent a line: their turn `turn` starts (whether the model takes it now or at the
    /// end of the turn it is in). A product ends here what showed for their last turn.
    fn chat_turn_started(&mut self, _cx: &mut Cx, _turn: u64) {}
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
    person_turn(host, cx, &panel);
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
    let mut calls = Vec::new();
    let mut cancelled = Vec::new();
    let dock = host.chat_dock();
    for (index, port) in dock.ports.iter_mut().enumerate() {
        for event in port.handle_event(cx, &Event::Signal) {
            match event {
                PortEvent::Call(call) => calls.push((index, call)),
                PortEvent::Cancel { call_id } => dock.inflight.retain(|flight| {
                    if flight.call_id == call_id {
                        cancelled.push(flight.id);
                    }
                    flight.call_id != call_id
                }),
                _ => {}
            }
        }
    }
    for id in cancelled {
        host.chat_cancel(cx, id);
    }
    let turn = host.chat_dock().turn.max(1);
    for (index, call) in calls {
        match makepad_strict_json::parse(call.args.as_bytes()) {
            Ok(args) => {
                // Calls from outside the conversation have no turn, with or without a token.
                let turn = if call.origin == CallOrigin::Remote { 0 } else { turn };
                let dock = host.chat_dock();
                if turn > 0 && !dock.has_turn(turn) {
                    dock.turn_calls.push((turn, call.call_id.clone()));
                    if dock.turn_actions.binary_search_by_key(&turn, |(turn, _)| *turn).is_ok() {
                        dock.turn_actions_changed = true;
                    }
                }
                let id = host.chat_turn_call(cx, &call.tool, args, turn);
                host.chat_dock().inflight.push(Inflight { port: index, call_id: call.call_id, id, last_alive: now, last_sent: None });
            }
            Err(error) => host.chat_dock().ports[index].reply(ToolResult::refused(&call.call_id, format!("the arguments do not parse: {error}"))),
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

/// The person's next turn: one more each time they send a line, seen as their newest line's text
/// changing or fewer entries following it (the same line again, or the transcript dropping its
/// oldest entries, leaves the entries after it alone). Calls the product's
/// [`ChatHost::chat_turn_started`]. One compare per pump; when the conversation changed, a walk
/// back over the entries after the person's newest line.
fn person_turn<H: ChatHost>(host: &mut H, cx: &mut Cx, panel: &WidgetRef) {
    let started = {
        let Some(chat) = panel.borrow::<AiChatPanel>() else { return };
        let Some(state) = chat.state() else { return };
        let dock = host.chat_dock();
        if dock.seen_generation == Some(state.generation) {
            return;
        }
        dock.seen_generation = Some(state.generation);
        let newest = state.entries.iter().rev().enumerate().find_map(|(after, entry)| match entry {
            Entry::User { text, .. } => Some((text.as_str(), after)),
            _ => None,
        });
        let started = match (newest, &dock.newest_line) {
            (Some((text, after)), Some((was, was_after))) => text != was || after < *was_after,
            (Some(_), None) => true,
            (None, _) => false,
        };
        match (newest, &mut dock.newest_line) {
            (Some((_, after)), Some(line)) if !started => line.1 = after,
            (newest, line) => *line = newest.map(|(text, after)| (text.to_string(), after)),
        }
        if started {
            dock.turn += 1;
            dock.turn_calls.retain(|(turn, call_id)| {
                if state.tool(call_id).is_some() {
                    return true;
                }
                if let Ok(at) = dock.turn_actions.binary_search_by_key(turn, |(turn, _)| *turn) {
                    dock.turn_actions.remove(at);
                }
                dock.turn_actions_changed = true;
                false
            });
        }
        started.then_some(dock.turn)
    };
    if let Some(turn) = started {
        host.chat_turn_started(cx, turn);
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
        }
    }
}

/// The picture that goes with a line the person is writing: read back once
/// per look, while there is text in the composer.
fn pump_look<H: ChatHost>(host: &mut H, cx: &mut Cx, panel: &WidgetRef) {
    let key = host.chat_look(cx);
    // A line sent: the next one gets a fresh picture.
    let lines = panel.borrow::<AiChatPanel>().and_then(|chat| chat.state().map(|state| state.entries.iter().filter(|entry| matches!(entry, Entry::User { .. })).count())).unwrap_or(0);
    let dock = host.chat_dock();
    if lines != dock.look.lines {
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
                let sent = panel.borrow_mut::<AiChatPanel>().map(|mut chat| chat.attach_for_model(vec![ModelImage { label, png: Arc::from(png) }]));
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
