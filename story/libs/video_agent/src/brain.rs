//! The model behind the agent, whichever answers: one small poll-driven
//! seam ([`Brain`]) over
//!
//! - **Claude Code** — the person's logged-in `claude` CLI, run chat-only
//!   with a pinned model (`claude-opus-5-5` by default). Calls use the text
//!   protocol of [`crate::calls`]; rendered pictures go along as image
//!   blocks, so the model sees its contact sheets.
//! - **the Claude API** — [`crate::claude_api`], native tools, prompt
//!   caching of the guide, pictures in tool results. Needs a key in the
//!   environment (`ANTHROPIC_API_KEY`).
//! - **the AI Hub** (feature `hub`) — the LAN's fleet chat nodes through
//!   the hub's chat election, tools in the model's own template. Text only.
//!
//! No backend is chosen silently: the person picks one, and one that
//! cannot answer says why.

use crate::calls::{parse_reply, protocol, Written};
use crate::tools::{Args, ToolDef};
use makepad_ai_hub::chat_wire::{ChatMessage, ChatRole};
use makepad_ai_hub::providers::claude::ClaudeCodeChatProvider;
use makepad_ai_hub::providers::provider::{ChatProvider, ProviderEvent, ToolImage, TurnInput};
use std::sync::Arc;

/// The Claude model the Generate panel asks for unless told otherwise.
pub const DEFAULT_CLAUDE_MODEL: &str = "claude-opus-5-5";

/// Which model answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Backend {
    /// The `claude` CLI with this model id.
    ClaudeCode { model: String },
    /// Anthropic's Messages API with this model id.
    ClaudeApi { model: String },
    /// The AI Hub's fleet chat on the LAN; `None` lets the hub elect.
    Hub { model: Option<String> },
}

impl Backend {
    pub fn label(&self) -> String {
        match self {
            Backend::ClaudeCode { model } => format!("{} · Claude Code", model_label(model)),
            Backend::ClaudeApi { model } => format!("{} · Claude API", model_label(model)),
            Backend::Hub { model: Some(model) } => format!("{model} · AI Hub"),
            Backend::Hub { model: None } => "AI Hub (LAN)".into(),
        }
    }

    /// Whether it can be started here, and why not.
    pub fn unavailable(&self) -> Option<String> {
        match self {
            Backend::ClaudeCode { .. } => makepad_ai_hub::providers::claude::find_cli().is_none().then(|| "the Claude Code CLI (`claude`) is not installed here".into()),
            Backend::ClaudeApi { .. } => crate::claude_api::credential().is_none().then(|| crate::claude_api::NO_CREDENTIAL.into()),
            #[cfg(feature = "hub")]
            Backend::Hub { .. } => None,
            #[cfg(not(feature = "hub"))]
            Backend::Hub { .. } => Some("this build has no AI Hub chat".into()),
        }
    }

    /// Whether the model gets the compact reference instead of the full
    /// guide: the AI Hub's local models have small contexts (16k tokens on
    /// the LAN's nodes; the full guide alone is about 29k).
    pub fn compact_guide(&self) -> bool {
        matches!(self, Backend::Hub { .. })
    }

    /// Whether the model looks at the rendered pictures.
    pub fn vision(&self) -> bool {
        !matches!(self, Backend::Hub { .. })
    }

    pub fn start(&self, system: &str, tools: &[ToolDef]) -> Result<Box<dyn Brain>, String> {
        if let Some(reason) = self.unavailable() {
            return Err(reason);
        }
        Ok(match self {
            // One CLI process for the whole conversation: a turn sends only
            // what is new, and pays no process start.
            Backend::ClaudeCode { model } => Box::new(TextBrain::new(ClaudeCodeChatProvider::new(Some(model.clone())).live(), self.label(), true, system, tools)),
            Backend::ClaudeApi { model } => Box::new(crate::claude_api::ApiBrain::new(model.clone(), self.label(), system, tools)?),
            #[cfg(feature = "hub")]
            Backend::Hub { model } => Box::new(hub::HubBrain::new(model.clone(), self.label(), system, tools)),
            #[cfg(not(feature = "hub"))]
            Backend::Hub { .. } => unreachable!(),
        })
    }
}

/// `claude-opus-5-5[1m]` → `Claude Opus 5.5 (1M)`.
pub fn model_label(model: &str) -> String {
    let (base, long) = match model.strip_suffix("[1m]") {
        Some(base) => (base, " (1M)"),
        None => (model, ""),
    };
    let Some(rest) = base.strip_prefix("claude-") else { return model.to_string() };
    let mut words = rest.split('-');
    let family = words.next().unwrap_or("");
    let version: Vec<&str> = words.collect();
    let mut family_title = family.to_string();
    if let Some(first) = family_title.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    format!("Claude {family_title} {}{long}", version.join("."))
}

/// One question and its plain answer (no tools): a step that only needs
/// text from the model (a lyric sheet, a storyboard). Passive like the
/// agent: [`Ask::poll`] drains the brain without waiting.
pub struct Ask {
    brain: Box<dyn Brain>,
    text: String,
    done: Option<Result<String, String>>,
}

impl Ask {
    pub fn start(backend: &Backend, system: &str, question: String) -> Result<Ask, String> {
        let mut brain = backend.start(system, &[])?;
        brain.send_user(question, Vec::new());
        Ok(Ask { brain, text: String::new(), done: None })
    }

    /// The answer once the reply is over.
    pub fn poll(&mut self) -> Option<Result<String, String>> {
        if self.done.is_none() {
            for event in self.brain.poll() {
                match event {
                    BrainEvent::Delta(text) => self.text.push_str(&text),
                    BrainEvent::TurnDone => self.done = Some(Ok(std::mem::take(&mut self.text))),
                    BrainEvent::Failed(why) => self.done = Some(Err(why)),
                    _ => {}
                }
            }
        }
        self.done.clone()
    }

    pub fn cancel(&mut self) {
        self.brain.cancel();
    }
}

/// A picture for the model: a strip, a frame, an onion trail. PNG, at
/// most 1024 px a side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrainImage {
    pub label: String,
    pub png: Arc<[u8]>,
}

/// One tool's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallResult {
    pub call_id: String,
    pub name: String,
    pub text: String,
    pub is_error: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BrainEvent {
    /// Where the model is: loading, listening for the fleet, which node.
    Status(String),
    /// Its reasoning, when the backend shows it.
    Thinking(String),
    /// Visible text of its reply, in order.
    Delta(String),
    /// It is writing a tool call now: the tool's name and how many bytes
    /// of it have arrived (a long document takes a while).
    Writing { name: String, bytes: usize },
    /// A tool call. Every call is owed exactly one result; a call that
    /// could not be read carries why, and is answered with that.
    Call { call_id: String, name: String, args: Result<Args, String> },
    /// The reply is over: results are owed for its calls, if it made any.
    TurnDone,
    /// The turn failed; the brain is idle and may be sent to again.
    Failed(String),
}

pub trait Brain {
    fn label(&self) -> String;
    fn vision(&self) -> bool;
    /// A line from the person (or from the agent on their behalf).
    fn send_user(&mut self, text: String, images: Vec<BrainImage>);
    /// Every result of the last reply's calls, in call order.
    fn send_results(&mut self, results: Vec<CallResult>, images: Vec<BrainImage>);
    fn poll(&mut self) -> Vec<BrainEvent>;
    fn cancel(&mut self);
}

/// How a pictured result is labelled in the text beside it.
pub fn image_note(images: &[BrainImage]) -> String {
    if images.is_empty() {
        return String::new();
    }
    let labels: Vec<&str> = images.iter().map(|image| image.label.as_str()).collect();
    format!("\n[attached pictures, in order: {}]", labels.join("; "))
}

// ------------------------------------------------------------- text brain

/// A chat provider driven with the text protocol: the history lives here,
/// replies are split into text and calls, results go back as one message.
pub struct TextBrain<P: ChatProvider> {
    provider: P,
    label: String,
    vision: bool,
    system: String,
    history: Vec<ChatMessage>,
    in_turn: bool,
    /// This reply as written, reasoning removed.
    raw: String,
    /// How much of `raw` went out as `Delta`s.
    shown: usize,
    thinking: bool,
    /// The tail of the last delta that may start a marker.
    carry: String,
    /// The call being written (its offset in `raw`) and the size last reported.
    writing: (usize, usize),
    next_call: u64,
    queued: Vec<BrainEvent>,
}

impl<P: ChatProvider> TextBrain<P> {
    pub fn new(provider: P, label: String, vision: bool, system: &str, tools: &[ToolDef]) -> Self {
        TextBrain {
            provider,
            label,
            vision,
            // With no tools, no protocol: a plain answer is wanted.
            system: if tools.is_empty() { system.to_string() } else { format!("{system}{}", protocol(tools)) },
            history: Vec::new(),
            in_turn: false,
            raw: String::new(),
            shown: 0,
            thinking: false,
            carry: String::new(),
            writing: (usize::MAX, 0),
            next_call: 0,
            queued: Vec::new(),
        }
    }

    fn begin(&mut self, images: Vec<BrainImage>) {
        self.raw.clear();
        self.shown = 0;
        self.thinking = false;
        self.carry.clear();
        self.writing = (usize::MAX, 0);
        let images = if self.vision { images } else { Vec::new() };
        let images: Vec<ToolImage> = images.into_iter().map(|image| ToolImage { label: image.label, png: image.png }).collect();
        if let Err(error) = self.provider.attach_tool_images(images) {
            self.queued.push(BrainEvent::Failed(format!("{}: {error}", self.label)));
            return;
        }
        let input = TurnInput::new(self.system.clone(), self.history.clone());
        match self.provider.begin_turn(&input) {
            Ok(()) => self.in_turn = true,
            Err(error) => self.queued.push(BrainEvent::Failed(format!("{}: {error}", self.label))),
        }
    }

    /// Reasoning to `Thinking`, the rest into `raw`; visible text is shown
    /// up to the first call, and a call being written is reported. A
    /// `<think>` marker split across deltas is held until it is whole.
    fn take(&mut self, text: &str, out: &mut Vec<BrainEvent>) {
        let joined = format!("{}{text}", std::mem::take(&mut self.carry));
        let mut rest = joined.as_str();
        while !rest.is_empty() {
            let marker = if self.thinking { "</think>" } else { "<think>" };
            let (piece, next) = match rest.find(marker) {
                Some(at) => (&rest[..at], Some(&rest[at + marker.len()..])),
                None => {
                    // Keep back a tail that may be the start of the marker.
                    let keep = (1..marker.len()).rev().find(|&n| rest.len() >= n && rest.is_char_boundary(rest.len() - n) && marker.starts_with(&rest[rest.len() - n..])).unwrap_or(0);
                    self.carry = rest[rest.len() - keep..].to_string();
                    (&rest[..rest.len() - keep], None)
                }
            };
            if !piece.is_empty() {
                if self.thinking {
                    out.push(BrainEvent::Thinking(piece.to_string()));
                } else {
                    self.raw.push_str(piece);
                }
            }
            match next {
                Some(next) => {
                    self.thinking = !self.thinking;
                    rest = if self.thinking { next } else { next.trim_start_matches('\n') };
                }
                None => rest = "",
            }
        }
        let visible_end = match ["<invoke name=", "<tool_call>", "<function_calls>"].iter().filter_map(|marker| self.raw.find(marker)).min() {
            Some(at) => {
                // The call being written now: the last opened one.
                if let Some(open) = self.raw.rfind("<invoke name=\"") {
                    let name_start = open + "<invoke name=\"".len();
                    let bytes = self.raw.len() - open;
                    // Reported every half kilobyte, and when a call starts.
                    if let Some(len) = self.raw[name_start..].find('"').filter(|_| open != self.writing.0 || bytes >= self.writing.1 + 512) {
                        self.writing = (open, bytes);
                        out.push(BrainEvent::Writing { name: self.raw[name_start..name_start + len].to_string(), bytes });
                    }
                }
                at
            }
            // Hold back what may be the start of a marker.
            None => {
                let mut end = self.raw.len();
                if let Some(lt) = self.raw.rfind('<') {
                    if self.raw.len() - lt < "<function_calls>".len() {
                        end = lt;
                    }
                }
                end
            }
        };
        if visible_end > self.shown && self.raw.is_char_boundary(visible_end) {
            out.push(BrainEvent::Delta(self.raw[self.shown..visible_end].to_string()));
            self.shown = visible_end;
        }
    }

    fn finish(&mut self, text: String, out: &mut Vec<BrainEvent>) {
        self.in_turn = false;
        let carry = std::mem::take(&mut self.carry);
        if self.thinking {
            if !carry.is_empty() {
                out.push(BrainEvent::Thinking(carry));
            }
        } else {
            self.raw.push_str(&carry);
        }
        // A turn that streamed nothing falls back to the provider's final
        // text, but on a resumed Claude Code session an empty turn's final
        // text is the previous reply: taking it again would replay its
        // calls, turn after turn.
        let last_reply = self.history.iter().rev().find(|message| message.role == ChatRole::Assistant).map(|message| message.text.trim());
        if self.raw.trim().is_empty() && !text.trim().is_empty() && last_reply != Some(text.trim()) {
            self.take(&text, out);
        }
        let reply = parse_reply(&self.raw);
        // Visible text the stream held back (a trailing `<`).
        let visible = reply.text.len();
        if visible > self.shown.min(visible) && self.shown < visible && reply.text.is_char_boundary(self.shown) {
            out.push(BrainEvent::Delta(reply.text[self.shown..].to_string()));
        }
        let raw = std::mem::take(&mut self.raw);
        if !raw.trim().is_empty() {
            self.history.push(ChatMessage::new(ChatRole::Assistant, raw.trim().to_string()));
        }
        for written in reply.calls {
            self.next_call += 1;
            let call_id = format!("c{}", self.next_call);
            let (name, args): (String, Result<Args, String>) = match written as Written {
                Ok((name, args)) => (name, Ok(args)),
                Err(error) => ("?".into(), Err(error)),
            };
            out.push(BrainEvent::Call { call_id, name, args });
        }
        out.push(BrainEvent::TurnDone);
    }
}

impl<P: ChatProvider> Brain for TextBrain<P> {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn vision(&self) -> bool {
        self.vision
    }

    fn send_user(&mut self, text: String, images: Vec<BrainImage>) {
        let text = format!("{text}{}", image_note(&images));
        self.history.push(ChatMessage::new(ChatRole::User, text));
        self.begin(images);
    }

    fn send_results(&mut self, results: Vec<CallResult>, images: Vec<BrainImage>) {
        let mut text = String::new();
        for result in &results {
            let status = if result.is_error { " — ERROR" } else { "" };
            text.push_str(&format!("[result of {} ({}){status}]\n{}\n\n", result.name, result.call_id, result.text.trim_end()));
        }
        text.push_str(&image_note(&images));
        self.history.push(ChatMessage::new(ChatRole::Tool, text.trim_end().to_string()));
        self.begin(images);
    }

    fn poll(&mut self) -> Vec<BrainEvent> {
        let mut out = std::mem::take(&mut self.queued);
        if !self.in_turn {
            return out;
        }
        for event in self.provider.poll() {
            match event {
                ProviderEvent::Delta(text) => self.take(&text, &mut out),
                ProviderEvent::Status { note, .. } => out.push(BrainEvent::Status(note)),
                ProviderEvent::Serving(_) => {}
                ProviderEvent::FunctionCall { .. } | ProviderEvent::FunctionCallPartial { .. } => {}
                ProviderEvent::Done { text } => {
                    self.finish(text, &mut out);
                    break;
                }
                ProviderEvent::Error(error) => {
                    self.in_turn = false;
                    self.raw.clear();
                    out.push(BrainEvent::Failed(format!("{}: {error}", self.label)));
                    break;
                }
            }
        }
        out
    }

    fn cancel(&mut self) {
        self.provider.cancel();
        if self.in_turn {
            // The reply so far is not history: the next line starts clean.
            self.in_turn = false;
            self.raw.clear();
        }
    }
}

// --------------------------------------------------------------- hub brain

#[cfg(feature = "hub")]
mod hub {
    use super::*;
    use makepad_ai_hub::hub_chat::{HubChatConfig, HubChatSession};
    use makepad_ai_hub::local_llm::{ChatEvent, LocalLlmConfig, ToolSpec};

    /// The fleet chat on the LAN. The session's election is given no local
    /// weights, so it never loads a model into Stage: it routes to a node
    /// that already serves one, or says there is none.
    pub struct HubBrain {
        session: HubChatSession,
        label: String,
        next_call: u64,
        /// Calls of the reply in flight, to name the results.
        thinking: bool,
    }

    impl HubBrain {
        pub fn new(model: Option<String>, label: String, system: &str, tools: &[ToolDef]) -> Self {
            let tools = tools.iter().map(|tool| ToolSpec::new(tool.name.clone(), tool.description.clone(), tool.parameters.clone())).collect();
            let session = HubChatSession::start(HubChatConfig {
                llm: LocalLlmConfig::new(std::path::PathBuf::new()),
                preferred_model: model,
                // The hub row has no Claude menu: a hub that runs it on its
                // Claude runs Story's default Claude model.
                claude_model: Some(DEFAULT_CLAUDE_MODEL.to_string()),
                max_tokens: Some(16_384),
                thinking: None,
                system_prompt: system.to_string(),
                tools,
                wake: None,
            });
            HubBrain { session, label, next_call: 0, thinking: false }
        }
    }

    impl Brain for HubBrain {
        fn label(&self) -> String {
            self.label.clone()
        }

        fn vision(&self) -> bool {
            false
        }

        fn send_user(&mut self, text: String, _images: Vec<BrainImage>) {
            self.session.send_user_turn(text);
        }

        fn send_results(&mut self, results: Vec<CallResult>, _images: Vec<BrainImage>) {
            self.session.send_tool_results(results.into_iter().map(|result| (result.text, result.is_error)).collect());
        }

        fn poll(&mut self) -> Vec<BrainEvent> {
            let mut out = Vec::new();
            for event in self.session.poll() {
                match event {
                    ChatEvent::Loading { phase, fraction } => out.push(BrainEvent::Status(format!("{phase} {:.0}%", fraction * 100.0))),
                    ChatEvent::Ready { .. } => out.push(BrainEvent::Status("ready".into())),
                    ChatEvent::Failed(error) => out.push(BrainEvent::Failed(format!("{}: {error}", self.label))),
                    ChatEvent::Delta(text) => {
                        // Qwen writes its reasoning inside <think>…</think>.
                        let mut rest = text.as_str();
                        while !rest.is_empty() {
                            let marker = if self.thinking { "</think>" } else { "<think>" };
                            let (piece, next) = match rest.find(marker) {
                                Some(at) => (&rest[..at], Some(&rest[at + marker.len()..])),
                                None => (rest, None),
                            };
                            if !piece.is_empty() {
                                out.push(if self.thinking { BrainEvent::Thinking(piece.to_string()) } else { BrainEvent::Delta(piece.to_string()) });
                            }
                            match next {
                                Some(next) => {
                                    self.thinking = !self.thinking;
                                    rest = next;
                                }
                                None => rest = "",
                            }
                        }
                    }
                    ChatEvent::ToolCall { name, args } => {
                        self.next_call += 1;
                        out.push(BrainEvent::Call { call_id: format!("h{}", self.next_call), name, args: Ok(Args(args)) });
                    }
                    ChatEvent::TurnDone { .. } => out.push(BrainEvent::TurnDone),
                    ChatEvent::Working(note) => out.push(BrainEvent::Status(note)),
                    ChatEvent::ContextFull => out.push(BrainEvent::Failed(format!("{}: the model's context is full; start a new generation", self.label))),
                }
            }
            if let Some(route) = self.session.route() {
                let label = format!("{} · {}", self.label, route.label());
                if label != self.label {
                    self.label = label;
                    out.insert(0, BrainEvent::Status(format!("answering: {}", route.label())));
                }
            }
            out
        }

        fn cancel(&mut self) {
            self.session.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_ai_hub::chat_wire::{ProviderAvailability, ProviderKind};

    /// A provider that answers each turn with the next scripted reply,
    /// streamed in small pieces.
    struct Scripted {
        replies: Vec<String>,
        turns: Vec<TurnInput>,
        pending: Vec<ProviderEvent>,
        images: Vec<usize>,
    }

    impl ChatProvider for Scripted {
        fn kind(&self) -> ProviderKind {
            ProviderKind::ClaudeCli
        }
        fn availability(&mut self) -> ProviderAvailability {
            ProviderAvailability::Available { model: "scripted".into(), detail: String::new() }
        }
        fn begin_turn(&mut self, input: &TurnInput) -> Result<(), String> {
            self.turns.push(input.clone());
            let reply = self.replies.remove(0);
            // `SILENT:<text>`: a turn that streams nothing and ends with
            // `<text>` as the final text (what a resumed Claude Code CLI
            // reports for an empty turn: its previous reply).
            if let Some(text) = reply.strip_prefix("SILENT:") {
                self.pending.push(ProviderEvent::Done { text: text.to_string() });
                return Ok(());
            }
            let chars: Vec<char> = reply.chars().collect();
            for chunk in chars.chunks(7) {
                self.pending.push(ProviderEvent::Delta(chunk.iter().collect()));
            }
            self.pending.push(ProviderEvent::Done { text: reply });
            Ok(())
        }
        fn poll(&mut self) -> Vec<ProviderEvent> {
            std::mem::take(&mut self.pending)
        }
        fn cancel(&mut self) {}
        fn attach_tool_images(&mut self, images: Vec<ToolImage>) -> Result<(), String> {
            self.images.push(images.len());
            Ok(())
        }
    }

    #[test]
    fn replies_split_into_text_thinking_and_calls_and_results_go_back() {
        let provider = Scripted {
            replies: vec![
                "<think>plan first</think>Planning the cut.\n<invoke name=\"timeline\">\n</invoke>\n<invoke name=\"strip\">\n<parameter name=\"count\">8</parameter>\n</invoke>".into(),
                "Looks right.".into(),
            ],
            turns: Vec::new(),
            pending: Vec::new(),
            images: Vec::new(),
        };
        let mut brain = TextBrain::new(provider, "test".into(), true, "SYSTEM", &crate::tools::own_tools());
        brain.send_user("make a video".into(), Vec::new());
        let events = brain.poll();
        let thinking: String = events.iter().filter_map(|e| if let BrainEvent::Thinking(t) = e { Some(t.as_str()) } else { None }).collect();
        let text: String = events.iter().filter_map(|e| if let BrainEvent::Delta(t) = e { Some(t.as_str()) } else { None }).collect();
        assert_eq!(thinking, "plan first");
        assert_eq!(text.trim(), "Planning the cut.");
        let calls: Vec<_> = events.iter().filter_map(|e| if let BrainEvent::Call { name, args, .. } = e { Some((name.clone(), args.clone().unwrap())) } else { None }).collect();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].1 .0, vec![("count".to_string(), "8".to_string())]);
        assert!(events.iter().any(|e| matches!(e, BrainEvent::Writing { name, .. } if name == "strip")));
        assert_eq!(events.last(), Some(&BrainEvent::TurnDone));

        let png: Arc<[u8]> = Arc::from(&b"png"[..]);
        brain.send_results(
            vec![
                CallResult { call_id: "c1".into(), name: "timeline".into(), text: "0–4 s hook".into(), is_error: false },
                CallResult { call_id: "c2".into(), name: "strip".into(), text: "8 frames".into(), is_error: false },
            ],
            vec![BrainImage { label: "strip of the cut".into(), png }],
        );
        let events = brain.poll();
        assert!(events.contains(&BrainEvent::TurnDone));
        let sent = &brain.provider.turns[1];
        let last = sent.messages.last().unwrap();
        assert_eq!(last.role, ChatRole::Tool);
        assert!(last.text.contains("[result of strip (c2)]") && last.text.contains("attached pictures, in order: strip of the cut"));
        assert!(sent.system.starts_with("SYSTEM") && sent.system.contains("## critique"));
        assert_eq!(brain.provider.images, vec![0, 1]);
    }

    #[test]
    fn an_empty_turn_does_not_replay_the_previous_calls() {
        let first = "Checking.\n<invoke name=\"timeline\">\n</invoke>";
        let provider = Scripted { replies: vec![first.into(), format!("SILENT:{first}"), "SILENT:A new line.".into()], turns: Vec::new(), pending: Vec::new(), images: Vec::new() };
        let mut brain = TextBrain::new(provider, "test".into(), true, "SYSTEM", &crate::tools::own_tools());
        let calls = |events: &[BrainEvent]| events.iter().filter(|e| matches!(e, BrainEvent::Call { .. })).count();
        brain.send_user("make a video".into(), Vec::new());
        assert_eq!(calls(&brain.poll()), 1);
        brain.send_results(vec![CallResult { call_id: "c1".into(), name: "timeline".into(), text: "0–4 s hook".into(), is_error: false }], Vec::new());
        let events = brain.poll();
        assert_eq!(calls(&events), 0, "{events:?}");
        assert!(events.contains(&BrainEvent::TurnDone));
        // A silent turn whose final text is new is still taken.
        brain.send_user("go on".into(), Vec::new());
        let events = brain.poll();
        assert!(events.iter().any(|e| matches!(e, BrainEvent::Delta(t) if t.contains("A new line."))), "{events:?}");
    }

    #[test]
    fn model_labels() {
        assert_eq!(model_label("claude-opus-5-5"), "Claude Opus 5.5");
        assert_eq!(model_label("claude-opus-5-5[1m]"), "Claude Opus 5.5 (1M)");
        assert_eq!(model_label("qwen3.8-27b"), "qwen3.8-27b");
    }
}
