//! Anthropic's Messages API as a [`Brain`]: native tool use, streamed.
//!
//! - One long-lived worker thread per brain posts each request and streams
//!   the server-sent events back over a channel; the UI side only drains
//!   the channel and assembles the reply.
//! - Adaptive thinking (Claude Opus 5.5 always thinks; effort is the
//!   control), `display: "summarized"` so the panel shows the reasoning.
//! - The system text (the authoring guide) and the tool table are a stable
//!   prefix with a cache breakpoint, and the request's automatic cache point
//!   follows the growing conversation, so each round re-reads the history
//!   from the cache.
//! - The history is append-only: every assistant message goes back exactly
//!   as it came, thinking blocks and their signatures included.
//! - Tool inputs stream eagerly (a Motion document arrives as it is
//!   written); each input is parsed strictly, and a reply cut off at
//!   `max_tokens` never runs its tools.
//! - Rendered pictures go back as image blocks after the tool results.
//!
//! The credential is an API key from the environment (`ANTHROPIC_API_KEY`),
//! only ever sent to api.anthropic.com. A Claude subscription login is never
//! sent to the API: it is used only through the `claude` CLI.

use crate::brain::{Brain, BrainEvent, BrainImage, CallResult};
use crate::tools::{Args, ToolDef};
use makepad_network::blocking_http::{request_to_writer_no_redirect, CancelToken, Limits, Request};
use makepad_strict_json::{self as json, Value};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

pub const MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
pub const API_VERSION: &str = "2023-06-01";
/// Output room per reply: thinking counts toward it.
pub const MAX_TOKENS: u32 = 64_000;
/// How hard the model thinks: this is design work judged by eye.
pub const EFFORT: &str = "high";

/// The environment's credential: the API key.
pub fn credential() -> Option<String> {
    std::env::var("ANTHROPIC_API_KEY").ok().map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
}

/// What a person sees when there is no credential.
pub const NO_CREDENTIAL: &str = "no ANTHROPIC_API_KEY is set";

/// The request body for one round: pure, so the exact wire shape is
/// tested.
pub fn request_body(model: &str, system: &str, tools: &[ToolDef], messages: &[Value]) -> Value {
    let tools = tools
        .iter()
        .map(|tool| {
            json::obj(vec![
                ("name", json::s(tool.name.clone())),
                ("description", json::s(tool.description.clone())),
                ("input_schema", json::parse_depth(tool.parameters.as_bytes(), 32).unwrap_or(Value::Obj(Vec::new()))),
                ("eager_input_streaming", Value::Bool(true)),
            ])
        })
        .collect();
    json::obj(vec![
        ("model", json::s(model)),
        ("max_tokens", Value::Int(MAX_TOKENS as i64)),
        ("stream", Value::Bool(true)),
        ("thinking", json::obj(vec![("type", json::s("adaptive")), ("display", json::s("summarized"))])),
        ("output_config", json::obj(vec![("effort", json::s(EFFORT))])),
        // The guide and tools are the cached prefix; the automatic cache
        // point moves with the conversation.
        ("system", Value::Arr(vec![json::obj(vec![("type", json::s("text")), ("text", json::s(system)), ("cache_control", json::obj(vec![("type", json::s("ephemeral"))]))])])),
        ("tools", Value::Arr(tools)),
        ("cache_control", json::obj(vec![("type", json::s("ephemeral"))])),
        ("messages", Value::Arr(messages.to_vec())),
    ])
}

fn image_blocks(images: &[BrainImage]) -> Vec<Value> {
    let mut blocks = Vec::new();
    for image in images {
        blocks.push(json::obj(vec![("type", json::s("text")), ("text", json::s(format!("[picture: {}]", image.label)))]));
        let data = String::from_utf8(makepad_base64::base64_encode(&image.png, &makepad_base64::BASE64_STANDARD)).unwrap_or_default();
        blocks.push(json::obj(vec![
            ("type", json::s("image")),
            ("source", json::obj(vec![("type", json::s("base64")), ("media_type", json::s("image/png")), ("data", json::s(data))])),
        ]));
    }
    blocks
}

/// The user message for a line from the person.
pub fn user_message(text: &str, images: &[BrainImage]) -> Value {
    let mut content = vec![json::obj(vec![("type", json::s("text")), ("text", json::s(text))])];
    content.extend(image_blocks(images));
    json::obj(vec![("role", json::s("user")), ("content", Value::Arr(content))])
}

/// The user message answering a reply's tool calls: every result first,
/// then the pictures.
pub fn results_message(results: &[CallResult], images: &[BrainImage]) -> Value {
    let mut content: Vec<Value> = results
        .iter()
        .map(|result| {
            json::obj(vec![
                ("type", json::s("tool_result")),
                ("tool_use_id", json::s(result.call_id.clone())),
                ("content", json::s(if result.text.is_empty() { "(no output)".to_string() } else { result.text.clone() })),
                ("is_error", Value::Bool(result.is_error)),
            ])
        })
        .collect();
    content.extend(image_blocks(images));
    json::obj(vec![("role", json::s("user")), ("content", Value::Arr(content))])
}

/// One content block as it streams in.
#[derive(Debug)]
enum Block {
    Text(String),
    Thinking { thinking: String, signature: String },
    ToolUse { id: String, name: String, input: String },
    /// Anything else (redacted thinking, server blocks): kept verbatim.
    Other(Value),
}

impl Block {
    fn value(&self) -> Value {
        match self {
            Block::Text(text) => json::obj(vec![("type", json::s("text")), ("text", json::s(text.clone()))]),
            Block::Thinking { thinking, signature } => json::obj(vec![("type", json::s("thinking")), ("thinking", json::s(thinking.clone())), ("signature", json::s(signature.clone()))]),
            Block::ToolUse { id, name, input } => json::obj(vec![
                ("type", json::s("tool_use")),
                ("id", json::s(id.clone())),
                ("name", json::s(name.clone())),
                ("input", json::parse_depth(input.trim().as_bytes(), 64).ok().filter(|v| matches!(v, Value::Obj(_))).unwrap_or(Value::Obj(Vec::new()))),
            ]),
            Block::Other(value) => value.clone(),
        }
    }
}

/// A reply being assembled from its events.
#[derive(Debug, Default)]
pub struct Assembly {
    blocks: Vec<Block>,
    stop_reason: String,
    refusal: Option<String>,
}

/// What an assembled event means to the brain.
#[derive(Debug, PartialEq)]
pub enum Step {
    Events(Vec<BrainEvent>),
    /// The message is complete.
    Stop,
    Error(String),
}

impl Assembly {
    /// Take one SSE `data` object.
    pub fn take(&mut self, event: &Value) -> Step {
        let mut out = Vec::new();
        match event.get("type").and_then(Value::as_str) {
            Some("content_block_start") => {
                let block = event.get("content_block").cloned().unwrap_or(Value::Null);
                let text = |key: &str| block.get(key).and_then(Value::as_str).unwrap_or("").to_string();
                self.blocks.push(match block.get("type").and_then(Value::as_str) {
                    Some("text") => Block::Text(text("text")),
                    Some("thinking") => Block::Thinking { thinking: text("thinking"), signature: text("signature") },
                    Some("tool_use") => {
                        out.push(BrainEvent::Writing { name: text("name"), bytes: 0 });
                        Block::ToolUse { id: text("id"), name: text("name"), input: String::new() }
                    }
                    _ => Block::Other(block.clone()),
                });
            }
            Some("content_block_delta") => {
                let delta = event.get("delta").cloned().unwrap_or(Value::Null);
                let text = |key: &str| delta.get(key).and_then(Value::as_str).unwrap_or("").to_string();
                match (self.blocks.last_mut(), delta.get("type").and_then(Value::as_str)) {
                    (Some(Block::Text(body)), Some("text_delta")) => {
                        let piece = text("text");
                        body.push_str(&piece);
                        out.push(BrainEvent::Delta(piece));
                    }
                    (Some(Block::Thinking { thinking, .. }), Some("thinking_delta")) => {
                        let piece = text("thinking");
                        thinking.push_str(&piece);
                        if !piece.is_empty() {
                            out.push(BrainEvent::Thinking(piece));
                        }
                    }
                    (Some(Block::Thinking { signature, .. }), Some("signature_delta")) => signature.push_str(&text("signature")),
                    (Some(Block::ToolUse { name, input, .. }), Some("input_json_delta")) => {
                        input.push_str(&text("partial_json"));
                        out.push(BrainEvent::Writing { name: name.clone(), bytes: input.len() });
                    }
                    _ => {}
                }
            }
            Some("message_delta") => {
                if let Some(reason) = event.get("delta").and_then(|d| d.get("stop_reason")).and_then(Value::as_str) {
                    self.stop_reason = reason.to_string();
                }
                if self.stop_reason == "refusal" {
                    let category = event.get("delta").and_then(|d| d.get("stop_details")).and_then(|d| d.get("category")).and_then(Value::as_str).unwrap_or("unspecified");
                    self.refusal = Some(category.to_string());
                }
            }
            Some("message_stop") => return Step::Stop,
            Some("error") => {
                let message = event.get("error").and_then(|e| e.get("message")).and_then(Value::as_str).unwrap_or("the API reported an error");
                return Step::Error(message.chars().take(300).collect());
            }
            _ => {}
        }
        Step::Events(out)
    }

    /// The assistant message to keep, exactly as it came.
    pub fn message(&self) -> Value {
        json::obj(vec![("role", json::s("assistant")), ("content", Value::Arr(self.blocks.iter().map(Block::value).collect()))])
    }

    /// The reply's calls, checked: a reply cut off at `max_tokens` runs
    /// none of them, and an input that is not a JSON object is refused.
    pub fn calls(&self) -> Vec<BrainEvent> {
        let cut = self.stop_reason == "max_tokens";
        self.blocks
            .iter()
            .filter_map(|block| match block {
                Block::ToolUse { id, name, input } => {
                    let args = if cut {
                        Err("the reply was cut off at the output limit while writing this call; write it again, smaller (split a long document with patch calls)".to_string())
                    } else if input.trim().is_empty() {
                        Ok(Args::default())
                    } else {
                        Args::from_json(input).map_err(|error| format!("{error}; the input could not be read"))
                    };
                    Some(BrainEvent::Call { call_id: id.clone(), name: name.clone(), args })
                }
                _ => None,
            })
            .collect()
    }
}

/// Server-sent events cut into `data:` payloads.
#[derive(Default)]
struct Sse {
    pending: Vec<u8>,
}

impl Sse {
    fn feed(&mut self, bytes: &[u8]) -> Vec<Value> {
        self.pending.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(end) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            if let Some(data) = line.trim_end().strip_prefix("data:") {
                if let Ok(value) = json::parse_depth(data.trim().as_bytes(), 64) {
                    out.push(value);
                }
            }
        }
        out
    }
}

enum Job {
    Post(Value),
}

enum Out {
    Event(Value),
    /// The HTTP exchange is over: `Err` says why it failed.
    Done(Result<(), String>),
}

/// Writes the streamed body into SSE events on the channel.
struct Stream<'a> {
    sse: Sse,
    to_brain: &'a Sender<Out>,
    /// The body of an error answer, read whole.
    raw: Vec<u8>,
}

impl std::io::Write for Stream<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.raw.len() < 16 * 1024 {
            self.raw.extend_from_slice(&bytes[..bytes.len().min(16 * 1024 - self.raw.len())]);
        }
        for event in self.sse.feed(bytes) {
            let _ = self.to_brain.send(Out::Event(event));
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn worker(jobs: Receiver<Job>, to_brain: Sender<Out>, secret: String, cancel: CancelToken) {
    while let Ok(Job::Post(body)) = jobs.recv() {
        let body = body.to_json().into_bytes();
        let mut attempt = 0;
        let result = loop {
            attempt += 1;
            let request = (|| {
                let request = Request::post(MESSAGES_URL)
                    .header("anthropic-version", API_VERSION)?
                    .header("content-type", "application/json")?
                    .header("x-api-key", &secret)?
                    .limits(Limits { max_body_bytes: 256 * 1024 * 1024, idle_timeout: Duration::from_secs(120), ..Limits::default() })
                    .cancel_token(cancel.clone())
                    .body(body.clone());
                Ok::<_, makepad_network::blocking_http::Error>(request)
            })();
            let request = match request {
                Ok(request) => request,
                Err(error) => break Err(format!("the request could not be built: {error}")),
            };
            let mut stream = Stream { sse: Sse::default(), to_brain: &to_brain, raw: Vec::new() };
            match request_to_writer_no_redirect(request, &mut stream) {
                Ok(response) if response.status == 200 => break Ok(()),
                Ok(response) => {
                    let message = json::parse_depth(&stream.raw, 16)
                        .ok()
                        .and_then(|v| v.get("error").and_then(|e| e.get("message")).and_then(Value::as_str).map(str::to_string))
                        .unwrap_or_default();
                    let retry = matches!(response.status, 429 | 500 | 502 | 503 | 504 | 529);
                    if retry && attempt < 4 {
                        std::thread::sleep(Duration::from_secs(2u64.pow(attempt)));
                        continue;
                    }
                    break Err(format!("HTTP {}: {}", response.status, message.chars().take(300).collect::<String>()));
                }
                Err(error) => {
                    if cancel.is_cancelled() {
                        break Err("cancelled".into());
                    }
                    if attempt < 3 {
                        std::thread::sleep(Duration::from_secs(2u64.pow(attempt)));
                        continue;
                    }
                    break Err(error.to_string());
                }
            }
        };
        if to_brain.send(Out::Done(result)).is_err() {
            break;
        }
    }
}

pub struct ApiBrain {
    model: String,
    label: String,
    system: String,
    tools: Vec<ToolDef>,
    history: Vec<Value>,
    jobs: Sender<Job>,
    from_worker: Receiver<Out>,
    cancel: CancelToken,
    assembly: Option<Assembly>,
    /// The turn ended (stop or failure) and the HTTP answer is awaited.
    queued: Vec<BrainEvent>,
}

impl ApiBrain {
    pub fn new(model: String, label: String, system: &str, tools: &[ToolDef]) -> Result<ApiBrain, String> {
        let credential = credential().ok_or(NO_CREDENTIAL)?;
        let (jobs, job_rx) = channel();
        let (out_tx, from_worker) = channel();
        let cancel = CancelToken::new();
        let worker_cancel = cancel.clone();
        std::thread::Builder::new()
            .name("stage-generate-claude".into())
            .spawn(move || worker(job_rx, out_tx, credential, worker_cancel))
            .map_err(|error| format!("the Claude API worker could not start: {error}"))?;
        Ok(ApiBrain { model, label, system: system.to_string(), tools: tools.to_vec(), history: Vec::new(), jobs, from_worker, cancel, assembly: None, queued: Vec::new() })
    }

    fn post(&mut self) {
        self.assembly = Some(Assembly::default());
        let body = request_body(&self.model, &self.system, &self.tools, &self.history);
        if self.jobs.send(Job::Post(body)).is_err() {
            self.assembly = None;
            self.queued.push(BrainEvent::Failed(format!("{}: the API worker is gone", self.label)));
        }
    }
}

impl Brain for ApiBrain {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn vision(&self) -> bool {
        true
    }

    fn send_user(&mut self, text: String, images: Vec<BrainImage>) {
        self.history.push(user_message(&text, &images));
        self.post();
    }

    fn send_results(&mut self, results: Vec<CallResult>, images: Vec<BrainImage>) {
        self.history.push(results_message(&results, &images));
        self.post();
    }

    fn poll(&mut self) -> Vec<BrainEvent> {
        let mut out = std::mem::take(&mut self.queued);
        while let Ok(message) = self.from_worker.try_recv() {
            let Some(assembly) = self.assembly.as_mut() else { continue };
            match message {
                Out::Event(event) => match assembly.take(&event) {
                    Step::Events(events) => out.extend(events),
                    Step::Stop => {}
                    Step::Error(error) => out.push(BrainEvent::Failed(format!("{}: {error}", self.label))),
                },
                Out::Done(Err(error)) => {
                    self.assembly = None;
                    out.push(BrainEvent::Failed(format!("{}: {error}", self.label)));
                }
                Out::Done(Ok(())) => {
                    let assembly = self.assembly.take().expect("a reply in flight");
                    if let Some(category) = &assembly.refusal {
                        out.push(BrainEvent::Failed(format!("{}: the model declined this request ({category})", self.label)));
                        continue;
                    }
                    self.history.push(assembly.message());
                    if assembly.stop_reason == "pause_turn" {
                        // A long turn paused by the server: continue it as it is.
                        self.post();
                        continue;
                    }
                    out.extend(assembly.calls());
                    out.push(BrainEvent::TurnDone);
                }
            }
        }
        out
    }

    fn cancel(&mut self) {
        if self.assembly.take().is_some() {
            self.cancel.cancel();
            // The next request gets a fresh token; the worker drops the
            // cancelled one's answer.
            self.cancel = CancelToken::new();
        }
    }
}

impl Drop for ApiBrain {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> ToolDef {
        ToolDef::new("inspect", "Check it.", r#"{"type":"object","properties":{"target":{"type":"string"}}}"#)
    }

    #[test]
    fn the_request_caches_the_guide_and_streams_tool_input() {
        let body = request_body("claude-opus-5-5", "GUIDE", &[tool()], &[user_message("go", &[])]);
        assert_eq!(body.get("model").and_then(Value::as_str), Some("claude-opus-5-5"));
        assert_eq!(body.get("thinking").and_then(|t| t.get("type")).and_then(Value::as_str), Some("adaptive"));
        let system = &body.get("system").and_then(Value::as_arr).unwrap()[0];
        assert!(system.get("cache_control").is_some());
        let tools = body.get("tools").and_then(Value::as_arr).unwrap();
        assert_eq!(tools[0].get("eager_input_streaming"), Some(&Value::Bool(true)));
        assert_eq!(tools[0].get("input_schema").and_then(|s| s.get("type")).and_then(Value::as_str), Some("object"));
        assert!(body.get("cache_control").is_some());
        assert!(body.get("temperature").is_none() && body.get("tool_choice").is_none());
    }

    #[test]
    fn results_come_first_then_pictures() {
        let png: std::sync::Arc<[u8]> = std::sync::Arc::from(&b"\x89PNG"[..]);
        let message = results_message(&[CallResult { call_id: "toolu_1".into(), name: "strip".into(), text: "8 frames".into(), is_error: false }], &[BrainImage { label: "strip".into(), png }]);
        let content = message.get("content").and_then(Value::as_arr).unwrap();
        assert_eq!(content[0].get("type").and_then(Value::as_str), Some("tool_result"));
        assert_eq!(content[0].get("tool_use_id").and_then(Value::as_str), Some("toolu_1"));
        assert_eq!(content[2].get("type").and_then(Value::as_str), Some("image"));
    }

    fn feed(assembly: &mut Assembly, sse: &str) -> Vec<BrainEvent> {
        let mut parser = Sse::default();
        let mut out = Vec::new();
        for event in parser.feed(sse.as_bytes()) {
            if let Step::Events(events) = assembly.take(&event) {
                out.extend(events);
            }
        }
        out
    }

    const STREAM: &str = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\"}}\n\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"\"}}\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"The hook is weak.\"}}\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"SIG\"}}\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Fixing the hook.\"}}\n",
        "data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_9\",\"name\":\"patch_motion\",\"input\":{}}}\n",
        "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"name\\\": \\\"hook\\\", \"}}\n",
        "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"find\\\": \\\"size: 96\\\", \\\"replace\\\": \\\"size: 140\\\"}\"}}\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n",
        "data: {\"type\":\"message_stop\"}\n",
    );

    #[test]
    fn a_streamed_reply_keeps_its_thinking_and_yields_its_calls() {
        let mut assembly = Assembly::default();
        let events = feed(&mut assembly, STREAM);
        assert!(events.contains(&BrainEvent::Thinking("The hook is weak.".into())));
        assert!(events.contains(&BrainEvent::Delta("Fixing the hook.".into())));
        let kept = assembly.message();
        let content = kept.get("content").and_then(Value::as_arr).unwrap();
        assert_eq!(content[0].get("signature").and_then(Value::as_str), Some("SIG"));
        assert_eq!(content[2].get("input").and_then(|i| i.get("replace")).and_then(Value::as_str), Some("size: 140"));
        let calls = assembly.calls();
        let BrainEvent::Call { call_id, name, args } = &calls[0] else { panic!() };
        assert_eq!((call_id.as_str(), name.as_str()), ("toolu_9", "patch_motion"));
        assert_eq!(args.as_ref().unwrap().raw("find"), Some("size: 96"));
    }

    #[test]
    fn a_reply_cut_off_runs_no_tools() {
        let mut assembly = Assembly::default();
        let cut = STREAM.replace("\"stop_reason\":\"tool_use\"", "\"stop_reason\":\"max_tokens\"");
        feed(&mut assembly, &cut);
        let BrainEvent::Call { args, .. } = &assembly.calls()[0] else { panic!() };
        assert!(args.as_ref().unwrap_err().contains("cut off"));
    }
}
