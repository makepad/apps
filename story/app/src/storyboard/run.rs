//! The storyboard's side of the AI control API (the `storyboard_*` tools)
//! and the update run that makes the cut follow the board.
//!
//! The pure tools (read, add, update, move, delete, rows, set, link,
//! write) run on the board at once (`makepad_story_storyboard::tools`).
//! The others need the cut: `storyboard_plan_update` plans with
//! [`build::plan`] on the open cut's blocks; `storyboard_apply_changes`
//! (and the bar's **Update edit**) starts a [`Run`] that makes each
//! mechanical change through the control API's own edit tools (`move_block`,
//! `trim_block`, `delete_block`, `set_voice_lines`, `add_clip`,
//! `generate_add`, …), so each is one undoable step in the CUT page,
//! logged in the change feed, and hand edits elsewhere stay as they are.
//! The run is driven by the app's pump (`pump`), outside the control API's
//! own dispatch; a tool that waits for it answers through `tool_reply`.

use super::{Desk, VideoCut};
use super::super::super::StoryApp;
use makepad_story_storyboard::build::{self, Action, Block, Gap, Plan};
use makepad_story_storyboard::edit::Op;
use makepad_story_storyboard::{tools as board_tools, AssetRef, Body, Built, RowKind, Storyboard};
use makepad_studio_api::{CallResult, ErrorKind, Image, ToolError, ToolOutput};
use makepad_story_cut::{CutNodeKind, SourceTime};
use makepad_strict_json::{self as json, Value as Json};
use makepad_widgets::*;
use std::collections::VecDeque;

/// The storyboard tools this Stage implements (all of the family).
pub const TOOL_NAMES: &[&str] = &[
    "storyboard_read", "storyboard_new", "storyboard_add", "storyboard_update", "storyboard_move", "storyboard_delete", "storyboard_row",
    "storyboard_set", "storyboard_link", "storyboard_write", "storyboard_changes", "storyboard_plan_update", "storyboard_apply_changes",
    "storyboard_mark_built", "storyboard_look",
];

/// How long `storyboard_look` waits for thumbnails it asked for.
const LOOK_WAIT: f64 = 6.0;

/// A tool's answer: now, or later through [`tool_reply`].
pub enum Answer {
    Done(CallResult),
    Later(u64),
}

/// A call the run makes on the control API, and what to do with its answer.
#[derive(Clone, Debug)]
struct Step {
    item: String,
    action: Action,
}

pub struct Run {
    /// The API call to answer when done (`None`: the bar's button).
    request: Option<u64>,
    origin: String,
    steps: VecDeque<Step>,
    /// The step in flight: its control API call id.
    current: Option<(Step, u64)>,
    /// Nodes the run made per item.
    made: Vec<(String, Vec<String>)>,
    /// The node the item's last `add_clip` made (for its `generate_add`).
    placed: Vec<(String, String)>,
    applied: Vec<String>,
    errors: Vec<String>,
    /// Items a step of failed: their part stays as it was (out of date).
    failed: Vec<String>,
    plan: Plan,
    built: Built,
    edit: AssetRef,
}

/// `storyboard_look` waiting for pictures.
struct Look {
    request: u64,
    ids: Vec<String>,
    since: f64,
}

thread_local! {
    static LOOKS: std::cell::RefCell<Vec<Look>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn now() -> f64 {
    super::now()
}

fn text(value: &str) -> Json {
    json::s(value)
}

fn ok(value: Json) -> Answer {
    Answer::Done(Ok(ToolOutput::json(value)))
}

fn refused(message: impl Into<String>) -> Answer {
    Answer::Done(Err(ToolError::refused(message)))
}

/// The open cut as blocks for the planner: every spanned node (graphics
/// and generative segments included), and each voiceover line as
/// `node#index` from its start to the next line's.
pub fn blocks_of(page: &VideoCut) -> Vec<Block> {
    let Some(session) = page.session.as_ref() else { return Vec::new() };
    let doc = &session.doc;
    let mut out = Vec::new();
    for node in &doc.nodes {
        let kind = match &node.kind {
            CutNodeKind::Clip { .. } => "clip",
            CutNodeKind::Still { .. } => "still",
            CutNodeKind::Title { .. } => "title",
            CutNodeKind::Fx { .. } => "effect",
            CutNodeKind::Transition { .. } => "transition",
            CutNodeKind::Reframe { .. } => "reframe",
            CutNodeKind::AudioClip { .. } => "audio",
            CutNodeKind::VoiceOver { .. } => "voice",
            CutNodeKind::Subtitles { .. } => "caption",
            _ => continue,
        };
        if let CutNodeKind::VoiceOver { lines, .. } = &node.kind {
            let starts: Vec<Option<f64>> = lines.iter().map(|line| match &line.at {
                SourceTime::Seconds(at) => Some(*at),
                SourceTime::Marker { .. } => None,
            }).collect();
            for (index, line) in lines.iter().enumerate() {
                let Some(at) = starts[index] else { continue };
                let next = starts.iter().skip(index + 1).flatten().next().copied().unwrap_or(at + 3.0);
                out.push(Block { id: format!("{}#{index}", node.id), kind: "voice_line".into(), start: at, length: (next - at).max(0.1), text: line.text.clone() });
            }
            let span = node.span.map_or((starts.iter().flatten().next().copied().unwrap_or(0.0), doc.duration), |span| (span.start, span.length));
            out.push(Block { id: node.id.clone(), kind: kind.into(), start: span.0, length: span.1, text: String::new() });
            continue;
        }
        let Some(span) = node.span else { continue };
        out.push(Block { id: node.id.clone(), kind: kind.into(), start: span.start, length: span.length, text: String::new() });
    }
    for graphic in &doc.graphics {
        if let Some(span) = graphic.span {
            out.push(Block { id: graphic.id.clone(), kind: "graphic".into(), start: span.start, length: span.length, text: String::new() });
        }
    }
    for generate in &doc.generates {
        out.push(Block { id: generate.id.clone(), kind: "generate".into(), start: generate.span.start, length: generate.span.length, text: String::new() });
    }
    out
}

/// The open cut as the board names it: its library revision, or its
/// draft while it has none.
fn cut_ref(page: &VideoCut) -> AssetRef {
    match page.session.as_ref().and_then(|session| session.head) {
        Some(head) => AssetRef { asset_id: head.asset_id.to_string(), revision: head.revision.to_string() },
        None => AssetRef { asset_id: page.autosave.id.clone().unwrap_or_else(|| "draft".into()), revision: "draft".into() },
    }
}

/// Run a storyboard tool. `origin`: who called (`mcp`, `cli`, `generate`).
pub fn tool(app: &mut StoryApp, cx: &mut Cx, name: &str, args: &Json, origin: &str) -> Answer {
    let mut desk = super::take();
    let answer = tool_on(&mut desk, app, cx, name, args, origin);
    super::put(desk);
    answer
}

fn tool_on(desk: &mut Desk, app: &mut StoryApp, cx: &mut Cx, name: &str, args: &Json, origin: &str) -> Answer {
    let page = &mut app.video_cut;
    if page.session.is_none() {
        return refused("no cut is open: the storyboard belongs to the cut in the CUT page (new_cut or open_cut first)");
    }
    // Tools that start a board.
    match name {
        "storyboard_new" => {
            if desk.source.is_some() {
                return refused("the open cut has a storyboard: read it with storyboard_read, or replace it with storyboard_write");
            }
            let number = |key: &str| args.get(key).and_then(|value| match value {
                Json::F64(n) => Some(*n),
                Json::Int(n) => Some(*n as f64),
                _ => None,
            });
            let get = |key: &str| args.get(key).and_then(Json::as_str).unwrap_or("").to_string();
            let title = if get("title").is_empty() { page.session.as_ref().map(|session| session.doc.label.clone()).unwrap_or_default() } else { get("title") };
            let source = board_tools::new_board(&title, number("duration").unwrap_or(30.0), number("bpm"), &get("brief"), &get("style"), &get("kit"));
            return match desk.change(source, true) {
                Ok(()) => ok(changed(desk, None, Vec::new())),
                Err(error) => refused(error),
            };
        }
        "storyboard_write" if desk.source.is_none() => {
            let text = args.get("source").and_then(Json::as_str).unwrap_or("");
            return match Storyboard::load(text) {
                Ok(_) => match desk.change(text.to_string(), true) {
                    Ok(()) => ok(changed(desk, None, Vec::new())),
                    Err(error) => refused(error),
                },
                Err(diags) => refused(format!("the source does not load, nothing was changed:\n{}", diags.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))),
            };
        }
        _ => {}
    }
    let Some(source) = desk.source.clone() else {
        return refused("the open cut has no storyboard yet: start one with storyboard_new (or storyboard_write a whole board)");
    };
    let answer = match name {
        "storyboard_changes" => {
            let seen = desk.ai_seen.as_deref().and_then(|seen| Storyboard::load(seen).ok()).map(|(board, _)| board);
            let board = desk.board.clone().unwrap_or_default();
            let lines: Vec<Json> = match &seen {
                Some(seen) => {
                    let changes = makepad_story_storyboard::diff::diff(seen, &board);
                    let described = makepad_story_storyboard::diff::describe(seen, &board, &changes);
                    described.lines().filter(|line| !line.is_empty()).map(|line| text(line.trim_start_matches("- "))).collect()
                }
                None => Vec::new(),
            };
            desk.ai_seen = Some(source.clone());
            return ok(json::obj(vec![("changes", Json::Arr(lines)), ("revision", text(&board.revision()))]));
        }
        "storyboard_plan_update" => {
            let board = desk.board.clone().unwrap_or_default();
            let Some(built) = board.built.clone() else {
                return refused("the cut was not made from this board yet: build it (Make video), then record its parts with storyboard_mark_built");
            };
            let gap = if args.get("close_gap").and_then(Json::as_bool) == Some(true) { Gap::Close } else { Gap::Keep };
            let plan = build::plan(&board, &built, &blocks_of(page), gap);
            return ok(json::obj(vec![
                ("actions", Json::Arr(plan.actions.iter().map(|(item, action)| text(&described(item, action))).collect())),
                ("tasks", text(&if plan.asks.is_empty() { String::new() } else { plan.ask_text() })),
                ("summary", Json::Arr(plan.summary.iter().map(|line| text(line)).collect())),
                ("untouched", Json::Arr(plan.untouched.iter().map(|id| text(id)).collect())),
            ]));
        }
        "storyboard_apply_changes" => {
            let gap = args.get("close_gap").and_then(Json::as_bool) == Some(true);
            desk.next_request += 1;
            let request = desk.next_request;
            return match update(desk, page, gap, Some((request, origin.to_string()))) {
                Ok(()) => Answer::Later(request),
                Err(error) => refused(error),
            };
        }
        "storyboard_mark_built" => {
            let board = desk.board.clone().unwrap_or_default();
            let edit = cut_ref(page);
            let built = if args.get("infer").and_then(Json::as_bool) == Some(true) {
                Ok(build::infer(&board, &blocks_of(page), edit))
            } else {
                let parts: Vec<(String, Vec<String>)> = args.get("parts").and_then(Json::as_arr).unwrap_or(&[]).iter().filter_map(|part| {
                    let item = part.get("item").and_then(Json::as_str)?.to_string();
                    let nodes = part.get("nodes").and_then(Json::as_arr).unwrap_or(&[]).iter().filter_map(|n| n.as_str().map(str::to_string)).collect();
                    Some((item, nodes))
                }).collect();
                if parts.is_empty() {
                    return refused("give `parts` (item ids with the cut nodes each made) or `infer: true`");
                }
                let blocks = blocks_of(page);
                if let Some(unknown) = parts.iter().flat_map(|(_, nodes)| nodes).find(|node| !blocks.iter().any(|block| block.id == **node)) {
                    return refused(format!("the cut has no node `{unknown}` (timeline lists them; a voiceover line is `node#index`)"));
                }
                build::mark(&board, board.built.as_ref(), edit, &parts)
            };
            match built.and_then(|built| board_tools::op(&source, Op::Built(Some(built)))) {
                Ok(applied) => {
                    let before = desk.board.clone();
                    match desk.change(applied.source, true) {
                        Ok(()) => changed(desk, before.as_ref(), applied.changed),
                        Err(error) => return refused(error),
                    }
                }
                Err(error) => return refused(error),
            }
        }
        "storyboard_look" => {
            let board = desk.board.clone().unwrap_or_default();
            let wanted: Vec<String> = match args.get("ids").and_then(Json::as_arr) {
                Some(ids) => ids.iter().filter_map(|id| id.as_str().map(str::to_string)).collect(),
                None => board.items.iter().filter(|item| matches!(&item.body, Body::Ref(reference) if reference.media.is_some_and(makepad_story_storyboard::Media::visual) && reference.src.is_some())).map(|item| item.id.clone()).collect(),
            };
            if wanted.is_empty() {
                return refused("the board has no picture references (images, videos, frames in the library) to look at");
            }
            for id in &wanted {
                if board.item(id).is_none() {
                    return refused(format!("there is no item `{id}`"));
                }
            }
            desk.next_request += 1;
            let request = desk.next_request;
            // Thumbnails come through the page's store; the answer waits
            // for them (`tool_reply`).
            let mut asks = Vec::new();
            for id in &wanted {
                if let Some(Body::Ref(reference)) = board.item(id).map(|item| &item.body) {
                    if let Some(src) = &reference.src {
                        if !page.lib.thumbs.contains_key(&src.asset_id) && page.requested.insert(src.asset_id.clone()) {
                            asks.extend(super::exact(src));
                        }
                    }
                }
            }
            if !asks.is_empty() {
                page.send(super::StoreJob::Thumbs { refs: asks });
            }
            LOOKS.with(|looks| looks.borrow_mut().push(Look { request, ids: wanted, since: now() }));
            let _ = cx;
            return Answer::Later(request);
        }
        _ => match board_tools::call(name, args, &source) {
            None => return Answer::Done(Err(ToolError::new(ErrorKind::UnknownTool, format!("`{name}` is not a storyboard tool")))),
            Some(Err(error)) => return refused(error),
            Some(Ok(called)) => {
                if let Some(new) = called.source {
                    if let Err(error) = desk.change(new, true) {
                        return refused(error);
                    }
                } else {
                    // A read: the AI has seen the board as it is.
                    desk.ai_seen = Some(source.clone());
                }
                desk.repaint = true;
                called.json
            }
        },
    };
    ok(answer)
}

fn described(item: &str, action: &Action) -> String {
    if item.is_empty() { action.describe() } else { format!("{item}: {}", action.describe()) }
}

/// A change's answer.
fn changed(desk: &Desk, before: Option<&Storyboard>, ids: Vec<String>) -> Json {
    let board = desk.board.clone().unwrap_or_default();
    let (stale, _) = build::stale(&board);
    let changes = before.map(|before| {
        let changes = makepad_story_storyboard::diff::diff(before, &board);
        makepad_story_storyboard::diff::describe(before, &board, &changes)
    }).unwrap_or_default();
    json::obj(vec![
        ("revision", text(&board.revision())),
        ("changed", Json::Arr(ids.iter().map(|id| text(id)).collect())),
        ("changes", text(&changes)),
        ("stale", Json::Arr(stale.iter().map(|id| text(id)).collect())),
    ])
}

/// Start updating the cut to the board (the bar's button; the API's
/// `storyboard_apply_changes` with its request).
pub fn update(desk: &mut Desk, page: &VideoCut, close_gap: bool, request: Option<(u64, String)>) -> Result<(), String> {
    if desk.run.is_some() {
        return Err("an update is under way".into());
    }
    let board = desk.board.clone().ok_or("the cut has no storyboard yet")?;
    let Some(built) = board.built.clone() else {
        return Err("the cut was not made from this board yet: Make video first (the AI records what each card made)".into());
    };
    let plan = build::plan(&board, &built, &blocks_of(page), if close_gap { Gap::Close } else { Gap::Keep });
    let mut steps: VecDeque<Step> = VecDeque::new();
    // Voiceover lines are dropped last, highest first, so the others keep
    // their places while the run works.
    let (mut drops, rest): (Vec<_>, Vec<_>) = plan.actions.iter().cloned().partition(|(_, action)| matches!(action, Action::DeleteVoiceLine { .. }));
    drops.sort_by(|a, b| match (&b.1, &a.1) {
        (Action::DeleteVoiceLine { line: x, .. }, Action::DeleteVoiceLine { line: y, .. }) => x.cmp(y),
        _ => std::cmp::Ordering::Equal,
    });
    for (item, action) in rest.into_iter().chain(drops) {
        steps.push_back(Step { item, action });
    }
    let (request, origin) = match request {
        Some((request, origin)) => (Some(request), origin),
        None => (None, "storyboard".to_string()),
    };
    desk.status = if plan.is_empty() { "The cut already follows the board".into() } else { format!("Updating the cut: {} changes{}", steps.len(), if plan.asks.is_empty() { String::new() } else { format!(", {} for the AI", plan.asks.len()) }) };
    desk.repaint = true;
    desk.run = Some(Run { request, origin, steps, current: None, made: Vec::new(), placed: Vec::new(), applied: Vec::new(), errors: Vec::new(), failed: Vec::new(), plan, built, edit: cut_ref(page) });
    Ok(())
}

/// A voiceover node's lines as the tools write them, from the page.
fn lines_json(page: &VideoCut, node: &str) -> Option<Vec<Json>> {
    let doc = &page.session.as_ref()?.doc;
    let CutNodeKind::VoiceOver { lines, .. } = &doc.node(node)?.kind else { return None };
    Some(lines.iter().map(|line| match &line.at {
        SourceTime::Seconds(at) => json::obj(vec![("text", text(&line.text)), ("at", Json::F64(*at))]),
        SourceTime::Marker { name, offset } => json::obj(vec![("text", text(&line.text)), ("marker", text(name)), ("offset", Json::F64(*offset))]),
    }).collect())
}

/// The control API call that makes one step, or why it cannot.
fn call_of(run: &Run, page: &VideoCut, step: &Step) -> Result<Option<(&'static str, Json)>, String> {
    let num = |n: f64| Json::F64((n * 1000.0).round() / 1000.0);
    Ok(Some(match &step.action {
        Action::Move { node, start } => ("move_block", json::obj(vec![("node", text(node)), ("start", num(*start))])),
        Action::Trim { node, start, length } => ("trim_block", json::obj(vec![("node", text(node)), ("start", num(*start)), ("length", num(*length))])),
        Action::Delete { node } => ("delete_block", json::obj(vec![("node", text(node))])),
        Action::VoiceLine { node, line, text: words, at } => {
            let mut lines = lines_json(page, node).ok_or_else(|| format!("the cut has no voiceover `{node}`"))?;
            let entry = lines.get_mut(*line).ok_or_else(|| format!("voiceover `{node}` has no line {}", line + 1))?;
            if let Json::Obj(fields) = entry {
                if let Some(words) = words {
                    fields.retain(|(key, _)| key != "text");
                    fields.push(("text".into(), text(words)));
                }
                if let Some(at) = at {
                    fields.retain(|(key, _)| key != "at" && key != "marker" && key != "offset");
                    fields.push(("at".into(), num(*at)));
                }
            }
            ("set_voice_lines", json::obj(vec![("node", text(node)), ("lines", Json::Arr(lines))]))
        }
        Action::DeleteVoiceLine { node, line } => {
            let mut lines = lines_json(page, node).ok_or_else(|| format!("the cut has no voiceover `{node}`"))?;
            if *line >= lines.len() {
                return Ok(None);
            }
            lines.remove(*line);
            if lines.is_empty() {
                ("delete_block", json::obj(vec![("node", text(node))]))
            } else {
                ("set_voice_lines", json::obj(vec![("node", text(node)), ("lines", Json::Arr(lines))]))
            }
        }
        Action::AddVoiceLine { node, text: words, at } => {
            let existing = node.as_ref().and_then(|node| lines_json(page, node).map(|lines| (node.clone(), lines)));
            match existing {
                Some((node, mut lines)) => {
                    let index = lines.iter().position(|line| line.get("at").and_then(|at| match at { Json::F64(n) => Some(*n), Json::Int(n) => Some(*n as f64), _ => None }).is_some_and(|line_at| line_at > *at)).unwrap_or(lines.len());
                    lines.insert(index, json::obj(vec![("text", text(words)), ("at", num(*at))]));
                    ("set_voice_lines", json::obj(vec![("node", text(&node)), ("lines", Json::Arr(lines))]))
                }
                None => ("add_voiceover", json::obj(vec![("lines", Json::Arr(vec![json::obj(vec![("text", text(words)), ("at", num(*at))])]))])),
            }
        }
        Action::Place { asset, kind, start, length } => {
            let mut fields = vec![
                ("media", json::obj(vec![("asset_id", text(&asset.asset_id)), ("revision", text(&asset.revision))])),
                ("kind", text(kind)),
                ("start", num(*start)),
            ];
            fields.push(("length", num(*length)));
            ("add_clip", json::obj(fields))
        }
        Action::Generate { clip, start, length, preset, prompt, seed, frame } => {
            let clip = clip.clone().or_else(|| run.placed.iter().rev().find(|(item, _)| *item == step.item).map(|(_, node)| node.clone()));
            let mut fields = vec![("start", num(*start)), ("length", num(length.max(0.1)))];
            if let Some(clip) = clip {
                fields.push(("clip", text(&clip)));
            }
            if !preset.is_empty() {
                fields.push(("preset", text(preset)));
            }
            if !prompt.is_empty() {
                fields.push(("prompt", text(prompt)));
            }
            if let Some(seed) = seed {
                fields.push(("seed", Json::Int((*seed).max(0))));
            }
            if *frame {
                fields.push(("frame_at", num(*start)));
            }
            ("generate_add", json::obj(fields))
        }
        Action::Duration { seconds } => ("set_header", json::obj(vec![("duration", num(*seconds))])),
        Action::Tempo { bpm } => ("set_header", json::obj(vec![("bpm", num(*bpm))])),
    }))
}

/// Advance the update run and the pictures `storyboard_look` waits for.
pub fn pump(desk: &mut Desk, app: &mut StoryApp, cx: &mut Cx) {
    pump_looks(desk, app);
    pump_grabs(desk, app);
    let Some(mut run) = desk.run.take() else { return };
    // The call in flight.
    if let Some((step, id)) = run.current.take() {
        match app.studio_result(id) {
            None => {
                run.current = Some((step, id));
                desk.run = Some(run);
                return;
            }
            Some(Ok(output)) => {
                // Only the blocks the call made count (not the stack or the
                // output it was wired into).
                let blocks = blocks_of(&app.video_cut);
                let nodes: Vec<String> = output.json.get("changed").and_then(Json::as_arr).unwrap_or(&[]).iter().filter_map(|n| n.as_str().map(str::to_string))
                    .chain(output.json.get("node").and_then(Json::as_str).map(str::to_string))
                    .filter(|node| blocks.iter().any(|block| block.id == *node && !matches!(block.kind.as_str(), "voice" | "voice_line")))
                    .collect();
                if matches!(step.action, Action::Place { .. } | Action::Generate { .. } | Action::AddVoiceLine { .. }) && !step.item.is_empty() {
                    if let Action::Place { .. } = step.action {
                        if let Some(node) = nodes.first() {
                            run.placed.push((step.item.clone(), node.clone()));
                        }
                    }
                    match run.made.iter_mut().find(|(item, _)| *item == step.item) {
                        Some((_, made)) => made.extend(nodes.iter().filter(|n| !n.contains('#')).cloned()),
                        None => run.made.push((step.item.clone(), nodes.iter().filter(|n| !n.contains('#')).cloned().collect())),
                    }
                }
                run.applied.push(described(&step.item, &step.action));
            }
            Some(Err(error)) => {
                run.errors.push(format!("{}: {}", described(&step.item, &step.action), error.message));
                run.failed.push(step.item.clone());
            }
        }
    }
    // The next step.
    while let Some(step) = run.steps.pop_front() {
        match call_of(&run, &app.video_cut, &step) {
            Ok(Some((tool, args))) => {
                let id = app.studio_call(cx, tool, args, &run.origin);
                run.current = Some((step, id));
                desk.run = Some(run);
                return;
            }
            Ok(None) => {}
            Err(error) => {
                run.errors.push(format!("{}: {error}", described(&step.item, &step.action)));
                run.failed.push(step.item.clone());
            }
        }
    }
    finish(desk, app, run);
}

/// The run is done: the board records what the cut is now made of, the
/// tasks go to the AI, and the API call (if any) is answered.
fn finish(desk: &mut Desk, app: &mut StoryApp, run: Run) {
    let page = &mut app.video_cut;
    let mut board = desk.board.clone().unwrap_or_default();
    // A closed gap moves the board's later items the same way.
    let ripple = build::ripple(&board, &run.plan);
    if !ripple.is_empty() {
        if let Some(source) = desk.source.clone() {
            if let Ok(applied) = makepad_story_storyboard::edit::apply_all(&source, &ripple) {
                let _ = desk.change(applied.source, false);
                board = applied.board;
            }
        }
    }
    let mut built = build::rebuilt(&board, &run.built, &run.plan, &run.made, run.edit.clone());
    // What failed stays out of date: its old part (and what it made, so a
    // retry replaces it).
    for item in run.failed.iter().filter(|item| !item.is_empty()) {
        let made: Vec<String> = run.made.iter().filter(|(id, _)| id == item).flat_map(|(_, nodes)| nodes.clone()).collect();
        built.parts.retain(|part| part.item != *item);
        match run.built.parts.iter().find(|part| part.item == *item) {
            Some(old) => {
                let mut old = old.clone();
                old.nodes.extend(made);
                built.parts.push(old);
            }
            None if !made.is_empty() => {
                // Its span as it is, no signature: the next update redoes it.
                let (start, length) = board.item(item).map_or((0.0, 0.0), |item| (item.start, item.length));
                built.parts.push(makepad_story_storyboard::Part { item: item.clone(), nodes: made, start, length, sig: String::new() });
            }
            None => {}
        }
    }
    // Voiceover lines move when lines are added or dropped: each voice
    // note is its line again, found by its words.
    let blocks = blocks_of(page);
    for item in &board.items {
        let (Body::Note { text: words }, true) = (&item.body, board.row(&item.row).is_some_and(|row| row.kind == RowKind::Voice)) else { continue };
        let Some(line) = blocks.iter().find(|block| block.kind == "voice_line" && block.text.trim() == words.trim()) else { continue };
        let asked = run.plan.asks.iter().any(|ask| ask.item == item.id);
        match built.parts.iter_mut().find(|part| part.item == item.id) {
            Some(part) => part.nodes = vec![line.id.clone()],
            None if !asked => built.parts.push(makepad_story_storyboard::Part { item: item.id.clone(), nodes: vec![line.id.clone()], start: item.start, length: item.length, sig: makepad_story_storyboard::diff::signature(item) }),
            None => {}
        }
    }
    if let Some(source) = desk.source.clone() {
        if let Ok(applied) = board_tools::op(&source, Op::Built(Some(built))) {
            let _ = desk.change(applied.source, run.request.is_some());
        }
    }
    let tasks = if run.plan.asks.is_empty() { String::new() } else { run.plan.ask_text() };
    desk.status = match (run.applied.len(), run.plan.asks.len(), run.errors.len()) {
        (0, 0, 0) => "The cut already follows the board".into(),
        (applied, 0, 0) => format!("Updated the cut: {applied} changes, every other part as it was"),
        (applied, asks, 0) => format!("Updated the cut: {applied} changes; {asks} scenes need the AI"),
        (applied, _, errors) => format!("Updated the cut: {applied} changes, {errors} failed: {}", run.errors.first().cloned().unwrap_or_default()),
    };
    desk.repaint = true;
    match run.request {
        Some(request) => {
            let mut fields = vec![
                ("applied", Json::Arr(run.applied.iter().map(|line| text(line)).collect())),
                ("tasks", text(&tasks)),
            ];
            if !run.errors.is_empty() {
                fields.push(("errors", Json::Arr(run.errors.iter().map(|line| text(line)).collect())));
            }
            desk.replies.insert(request, Ok(ToolOutput::json(json::obj(fields))));
        }
        None if !tasks.is_empty() => {
            // The bar's button: the judgement goes to the AI when one is at
            // work; otherwise the status says what waits.
            if page.generate.crew.is_some() {
                page.generate.pending_say = Some((format!("The person changed the storyboard; the cut was updated where that was mechanical. Do these tasks:\n\n{tasks}"), None));
                desk.status.push_str(" (sent to the AI)");
            } else {
                desk.status.push_str(": ask the chat to do them");
            }
        }
        None => {}
    }
}

/// Pasted web pages wait for web grabs, which Story does not have yet:
/// the queue is dropped.
fn pump_grabs(desk: &mut Desk, _app: &mut StoryApp) {
    desk.grab_queue.clear();
}

/// `storyboard_look`s whose pictures are in (or that waited long enough).
fn pump_looks(desk: &mut Desk, app: &mut StoryApp) {
    let looks = LOOKS.with(|looks| std::mem::take(&mut *looks.borrow_mut()));
    let mut waiting = Vec::new();
    for look in looks {
        let board = desk.board.clone().unwrap_or_default();
        let assets: Vec<(String, String)> = look.ids.iter().filter_map(|id| match board.item(id).map(|item| &item.body) {
            Some(Body::Ref(reference)) => reference.src.as_ref().map(|src| (id.clone(), src.asset_id.clone())),
            _ => None,
        }).collect();
        let ready = assets.iter().all(|(_, asset)| app.video_cut.lib.thumbs.contains_key(asset));
        if !ready && now() - look.since < LOOK_WAIT {
            waiting.push(look);
            continue;
        }
        let pictures: Vec<(String, &makepad_library_browser::lanes::Thumb)> = assets.iter().filter_map(|(id, asset)| app.video_cut.lib.thumbs.get(asset).map(|(thumb, _)| (id.clone(), thumb))).collect();
        let result = if pictures.is_empty() {
            Err(ToolError::failed("the references' pictures did not load from the library"))
        } else {
            sheet(&pictures).map(|(png, width, height)| {
                let shown: Vec<Json> = pictures.iter().map(|(id, _)| text(id)).collect();
                let label = format!("storyboard references: {}", pictures.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>().join(", "));
                ToolOutput::json(json::obj(vec![("width", Json::Int(width as i64)), ("height", Json::Int(height as i64)), ("shown", Json::Arr(shown)), ("layout", text("left to right, top to bottom, in the order of `shown`"))])).with_image(Image { png, label })
            })
        };
        desk.replies.insert(look.request, result);
    }
    LOOKS.with(|looks| looks.borrow_mut().extend(waiting));
}

/// A contact sheet: up to four tiles a row, each 320 px wide at its aspect
/// (cropped to 16:10 at most), on the board's ground.
fn sheet(pictures: &[(String, &makepad_library_browser::lanes::Thumb)]) -> Result<(Vec<u8>, usize, usize), ToolError> {
    const TILE_W: usize = 320;
    const TILE_H: usize = 200;
    const GAP: usize = 8;
    let columns = pictures.len().min(4);
    let rows = pictures.len().div_ceil(columns);
    let (width, height) = (columns * TILE_W + (columns + 1) * GAP, rows * TILE_H + (rows + 1) * GAP);
    let mut pixels = vec![0xff0a0d12u32; width * height];
    for (index, (_, thumb)) in pictures.iter().enumerate() {
        let (x0, y0) = (GAP + (index % columns) * (TILE_W + GAP), GAP + (index / columns) * (TILE_H + GAP));
        let (tw, th) = (thumb.width.max(1), thumb.height.max(1));
        // Fit inside the tile at the picture's aspect.
        let scale = (TILE_W as f64 / tw as f64).min(TILE_H as f64 / th as f64);
        let (w, h) = (((tw as f64 * scale) as usize).max(1), ((th as f64 * scale) as usize).max(1));
        let (ox, oy) = (x0 + (TILE_W - w) / 2, y0 + (TILE_H - h) / 2);
        for y in 0..h {
            let sy = (y * th / h).min(th - 1);
            for x in 0..w {
                let sx = (x * tw / w).min(tw - 1);
                pixels[(oy + y) * width + ox + x] = thumb.pixels[sy * tw + sx] | 0xff00_0000;
            }
        }
    }
    let rgba: Vec<u8> = pixels.iter().flat_map(|pixel| [(pixel >> 16) as u8, (pixel >> 8) as u8, *pixel as u8, 255]).collect();
    let png = Cx::encode_rgba_as_png(width as u32, height as u32, &rgba).map_err(|error| ToolError::failed(format!("the contact sheet: {error}")))?;
    Ok((png, width, height))
}

/// An answer the control API waits for (`storyboard_apply_changes`,
/// `storyboard_look`).
pub fn tool_reply(request: u64) -> Option<CallResult> {
    super::DESK.with(|cell| cell.borrow_mut().as_mut().and_then(|desk| desk.replies.remove(&request)))
}
