//! The board is the input, the cut its output. The board records which
//! of its revisions built the cut and which edit nodes each card made
//! (`Built`), so when the board changes the cut is updated, not remade:
//!
//! - **Mechanical** changes apply without the AI: a card moved or retimed
//!   moves and scales its blocks (from where they are now, so a hand trim
//!   stays a trim); a changed voiceover note re-words only its line (that
//!   line goes stale until it is spoken again); a deleted card deletes its
//!   blocks and, if the person chooses, closes the gap; a reference added
//!   with role `@use` goes on the timeline; one with `@generate` becomes a
//!   generative segment through the same tools as the
//!   Generate page's; a new voiceover note becomes a line.
//! - **Judgement** (a card's text, mood or look changed, a new scene card)
//!   becomes a small scoped task for the AI with only that scene's context
//!   and the node ids it may touch.
//! - Everything else in the cut is untouched, byte for byte; blocks the
//!   board did not make (hand-added) are never moved, except by "close the
//!   gap", which moves what comes after a deleted card.

use crate::diff::{content, name_of, signature};
use crate::model::*;
use crate::splash::number;

/// A block of the cut as the planner sees it: its node id, its kind
/// (`clip`, `still`, `graphic`, `title`, `voice`, `audio`, `generate`,
/// `effect`, …) and span. A voiceover line is `node#index` with kind
/// `voice_line` and its text.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub id: String,
    pub kind: String,
    pub start: f64,
    pub length: f64,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Gap {
    /// A deleted card leaves a hole.
    #[default]
    Keep,
    /// What comes after a deleted card moves left by its length.
    Close,
}

/// One mechanical change, in terms of the cut tools that make it.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// `move_block`.
    Move { node: String, start: f64 },
    /// `trim_block`.
    Trim { node: String, start: f64, length: f64 },
    /// `delete_block`.
    Delete { node: String },
    /// One voiceover line's words and/or start (`set_voice_lines` on its
    /// node with the other lines as they are).
    VoiceLine { node: String, line: usize, text: Option<String>, at: Option<f64> },
    /// Drop one voiceover line (its note was deleted).
    DeleteVoiceLine { node: String, line: usize },
    /// A new line on the cut's voiceover (`set_voice_lines`), or a new
    /// voiceover when the cut has none (`add_voiceover`).
    AddVoiceLine { node: Option<String>, text: String, at: f64 },
    /// `add_clip`: library media over the item's span.
    Place { asset: AssetRef, kind: &'static str, start: f64, length: f64 },
    /// `generate_add` over the block the previous `Place` of the same item
    /// made (or `clip` when known): image-to-video from its first frame
    /// when `frame`.
    Generate { clip: Option<String>, start: f64, length: f64, preset: String, prompt: String, seed: Option<i64>, frame: bool },
    /// `set_header`.
    Duration { seconds: f64 },
    Tempo { bpm: f64 },
}

impl Action {
    pub fn describe(&self) -> String {
        let s = |t: f64| format!("{} s", number(t));
        match self {
            Action::Move { node, start } => format!("move `{node}` to {}", s(*start)),
            Action::Trim { node, start, length } => format!("set `{node}` to {}–{}", s(*start), s(start + length)),
            Action::Delete { node } => format!("delete `{node}`"),
            Action::VoiceLine { node, line, text, at } => match (text, at) {
                (Some(text), _) => format!("re-word voiceover `{node}` line {} to \"{text}\" (re-take)", line + 1),
                (None, Some(at)) => format!("move voiceover `{node}` line {} to {}", line + 1, s(*at)),
                _ => format!("voiceover `{node}` line {}", line + 1),
            },
            Action::DeleteVoiceLine { node, line } => format!("drop voiceover `{node}` line {}", line + 1),
            Action::AddVoiceLine { text, at, .. } => format!("add voiceover line \"{text}\" at {}", s(*at)),
            Action::Place { kind, start, length, .. } => format!("place the {kind} at {} for {}", s(*start), s(*length)),
            Action::Generate { preset, .. } => format!("make a generative segment ({})", if preset.is_empty() { "the default preset" } else { preset }),
            Action::Duration { seconds } => format!("set the cut's length to {}", s(*seconds)),
            Action::Tempo { bpm } => format!("set the cut's tempo to {bpm}"),
        }
    }
}

/// A change that needs the AI: one scene's worth, with only its context.
#[derive(Clone, Debug, PartialEq)]
pub struct Ask {
    pub item: String,
    /// The scene number (1-based, time order) when it is a card.
    pub scene: Option<usize>,
    /// One sentence: what to do.
    pub task: String,
    /// The card's fields now, what changed, its voiceover and references.
    pub context: String,
    /// The cut nodes this task may change (the card's parts).
    pub nodes: Vec<String>,
    /// `[start, length]` the scene now occupies.
    pub span: (f64, f64),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    /// `(item, action)` in order.
    pub actions: Vec<(String, Action)>,
    pub asks: Vec<Ask>,
    /// Items whose parts need nothing.
    pub untouched: Vec<String>,
    /// Nodes the build recorded that the cut no longer has (deleted by
    /// hand): left alone.
    pub missing: Vec<String>,
    /// Plain lines for the "Update edit" sheet.
    pub summary: Vec<String>,
    /// Gaps closed: `(end of the removed card, its length)`; the board's
    /// later items move left the same way (`ripple`).
    pub ripple: Vec<(f64, f64)>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty() && self.asks.is_empty()
    }

    /// The scoped tasks as one message for the AI.
    pub fn ask_text(&self) -> String {
        let mut out = String::new();
        for (index, ask) in self.asks.iter().enumerate() {
            out.push_str(&format!(
                "Task {}: {}\nSpan: {}–{} s. Nodes you may change: {}.\n{}\n\n",
                index + 1,
                ask.task,
                number(ask.span.0),
                number(ask.span.0 + ask.span.1),
                if ask.nodes.is_empty() { "none yet (add new ones)".to_string() } else { ask.nodes.join(", ") },
                ask.context
            ));
        }
        out.push_str("Change only those nodes (or add new ones in that span); every other part of the cut stays as it is. When a task is done, call storyboard_mark_built with the item and the node ids it now has.");
        out
    }
}

fn find<'a>(blocks: &'a [Block], id: &str) -> Option<&'a Block> {
    blocks.iter().find(|block| block.id == id)
}

/// `node#3` → (`node`, 3).
pub fn voice_line(id: &str) -> Option<(&str, usize)> {
    let (node, line) = id.split_once('#')?;
    Some((node, line.parse().ok()?))
}

/// Map `t` from the span `from` to `to` (scaled when the length changed).
fn remap(t: f64, from: (f64, f64), to: (f64, f64)) -> f64 {
    let scale = if from.1 > 1e-9 { to.1 / from.1 } else { 1.0 };
    to.0 + (t - from.0) * scale
}

/// The context an Ask carries for one item.
fn context(board: &Storyboard, item: &Item, changed: &[(String, String, String)]) -> String {
    let mut out = format!("{}:\n", name_of(board, &item.id));
    for (key, value) in content(item) {
        out.push_str(&format!("  {key}: {value}\n"));
    }
    if !changed.is_empty() {
        out.push_str("Changed since the cut was built:\n");
        for (field, from, to) in changed {
            out.push_str(&format!("  {field}: \"{from}\" → \"{to}\"\n"));
        }
    }
    // What is said and referenced over it.
    for other in &board.items {
        if other.id == item.id || other.end() <= item.start || other.start >= item.end() {
            continue;
        }
        let linked = board.links.iter().any(|link| (link.from == item.id && link.to == other.id) || (link.to == item.id && link.from == other.id));
        let row = board.row(&other.row).map(|row| row.kind);
        match (&other.body, row) {
            (Body::Note { text }, Some(RowKind::Voice)) => out.push_str(&format!("Voiceover over it ({}–{} s): \"{text}\"\n", number(other.start), number(other.end()))),
            (Body::Ref(reference), _) if linked || row == Some(RowKind::Refs) => {
                let src = reference.src.as_ref().map(|src| format!(" asset {} {}", src.asset_id, src.revision)).unwrap_or_default();
                out.push_str(&format!("Reference `{}` ({}, role {}){src}: {}\n", other.id, reference.media.map_or("?", Media::tag), reference.role.tag(), other.headline()));
            }
            _ => {}
        }
    }
    out
}

fn preset_of(flow: &str, frame: bool) -> String {
    if flow.is_empty() {
        return if frame { "bundled:image-to-video".into() } else { String::new() };
    }
    flow.to_string()
}

/// Plan how the cut follows the board. `blocks` is the cut now.
pub fn plan(board: &Storyboard, built: &Built, blocks: &[Block], gap: Gap) -> Plan {
    let mut plan = Plan::default();
    let voice_node = blocks.iter().find(|block| block.kind == "voice").map(|block| block.id.clone());
    // Removed items first (their gaps move what follows).
    let mut shifts: Vec<(f64, f64)> = Vec::new();
    for part in &built.parts {
        if board.item(&part.item).is_some() {
            continue;
        }
        plan.summary.push(format!("`{}` was removed: delete {}", part.item, if part.nodes.is_empty() { "nothing".to_string() } else { part.nodes.join(", ") }));
        for node in &part.nodes {
            if let Some((voice, line)) = voice_line(node) {
                if find(blocks, node).is_some() {
                    plan.actions.push((part.item.clone(), Action::DeleteVoiceLine { node: voice.to_string(), line }));
                } else {
                    plan.missing.push(node.clone());
                }
            } else if find(blocks, node).is_some() {
                plan.actions.push((part.item.clone(), Action::Delete { node: node.clone() }));
            } else {
                plan.missing.push(node.clone());
            }
        }
        if gap == Gap::Close {
            shifts.push((part.start + part.length, part.length));
        }
    }
    if !shifts.is_empty() {
        // Close the gaps (a ripple delete): every block after a removed
        // card moves left by the removed lengths before it. The host
        // ripples the board's items the same way (`ripple`), so the cards
        // after it still match their blocks.
        for block in blocks {
            if voice_line(&block.id).is_some() || block.kind == "voice" || built.parts.iter().any(|part| board.item(&part.item).is_none() && part.nodes.contains(&block.id)) {
                continue;
            }
            let shift: f64 = shifts.iter().filter(|(end, _)| block.start >= end - 1e-6).map(|(_, length)| length).sum();
            if shift > 0.0 {
                plan.actions.push((String::new(), Action::Move { node: block.id.clone(), start: (block.start - shift).max(0.0) }));
            }
        }
        for block in blocks.iter().filter(|block| block.kind == "voice_line") {
            let shift: f64 = shifts.iter().filter(|(end, _)| block.start >= end - 1e-6).map(|(_, length)| length).sum();
            if let (true, Some((node, line))) = (shift > 0.0, voice_line(&block.id)) {
                if !built.parts.iter().any(|part| board.item(&part.item).is_none() && part.nodes.contains(&block.id)) {
                    plan.actions.push((String::new(), Action::VoiceLine { node: node.to_string(), line, text: None, at: Some((block.start - shift).max(0.0)) }));
                }
            }
        }
        plan.summary.push(format!("close the gap: what follows moves left by {} s", number(shifts.iter().map(|(_, l)| l).sum())));
        plan.ripple = shifts.clone();
    }
    for item in &board.items {
        let row = board.row(&item.row).map(|row| row.kind);
        let Some(part) = built.parts.iter().find(|part| part.item == item.id) else {
            // New since the build.
            match (&item.body, row) {
                (Body::Scene(_) | Body::Chapter(_) | Body::Lyric(_), _) => {
                    let scene = board.scene_number(&item.id).or_else(|| board.chapter_number(&item.id)).or_else(|| board.line_number(&item.id));
                    plan.asks.push(Ask {
                        item: item.id.clone(),
                        scene,
                        task: format!("Build new {} (`{}`) in the cut from its card.", unit_name(board, item), item.id),
                        context: context(board, item, &[]),
                        nodes: Vec::new(),
                        span: (item.start, item.length),
                    });
                    plan.summary.push(format!("new {}: the AI builds it", name_of(board, &item.id)));
                }
                (Body::Note { text }, Some(RowKind::Voice)) if !text.trim().is_empty() => {
                    plan.actions.push((item.id.clone(), Action::AddVoiceLine { node: voice_node.clone(), text: text.clone(), at: item.start }));
                    plan.summary.push(format!("new {}: add the voiceover line", name_of(board, &item.id)));
                }
                (Body::Ref(reference), _) if reference.role != Role::Reference => {
                    place_ref(&mut plan, board, item, reference);
                }
                _ => plan.untouched.push(item.id.clone()),
            }
            continue;
        };
        let moved = (part.start - item.start).abs() > 1e-6 || (part.length - item.length).abs() > 1e-6;
        let same = part.sig == signature(item);
        if !moved && same {
            plan.untouched.push(item.id.clone());
            continue;
        }
        if moved {
            let from = (part.start, part.length);
            let to = (item.start, item.length);
            plan.summary.push(format!("{} {} {}–{} s → {}–{} s: its blocks follow", name_of(board, &item.id), if (from.1 - to.1).abs() > 1e-6 { "retimed" } else { "moved" }, number(from.0), number(from.0 + from.1), number(to.0), number(to.0 + to.1)));
            for node in &part.nodes {
                let Some(block) = find(blocks, node) else {
                    plan.missing.push(node.clone());
                    continue;
                };
                // From where the block is now, so a hand trim stays.
                let start = remap(block.start, from, to).max(0.0);
                if let Some((voice, line)) = voice_line(node) {
                    plan.actions.push((item.id.clone(), Action::VoiceLine { node: voice.to_string(), line, text: None, at: Some(start) }));
                } else if (from.1 - to.1).abs() > 1e-6 {
                    let length = (block.length * if from.1 > 1e-9 { to.1 / from.1 } else { 1.0 }).max(0.01);
                    plan.actions.push((item.id.clone(), Action::Trim { node: node.clone(), start, length }));
                } else {
                    plan.actions.push((item.id.clone(), Action::Move { node: node.clone(), start }));
                }
            }
        }
        if same {
            continue;
        }
        // The content changed.
        match (&item.body, row) {
            (Body::Note { text }, Some(RowKind::Voice)) => {
                let lines: Vec<&String> = part.nodes.iter().filter(|node| voice_line(node).is_some()).collect();
                if lines.is_empty() {
                    plan.asks.push(ask_for(board, item, part, "Its words changed; update the text the cut shows or says for it."));
                } else {
                    for node in lines {
                        let (voice, line) = voice_line(node).unwrap();
                        if find(blocks, node).is_none() {
                            plan.missing.push(node.clone());
                            continue;
                        }
                        // Merge with a move planned above.
                        let at = plan.actions.iter().rev().find_map(|(_, action)| match action {
                            Action::VoiceLine { node: n, line: l, at, .. } if n == voice && *l == line => *at,
                            _ => None,
                        });
                        plan.actions.retain(|(_, action)| !matches!(action, Action::VoiceLine { node: n, line: l, text: None, .. } if n == voice && *l == line));
                        plan.actions.push((item.id.clone(), Action::VoiceLine { node: voice.to_string(), line, text: Some(text.clone()), at }));
                    }
                    plan.summary.push(format!("{}: re-word its voiceover line (re-take)", name_of(board, &item.id)));
                }
            }
            (Body::Ref(reference), _) if reference.role != Role::Reference => {
                // New source or generation settings: replace its parts.
                for node in &part.nodes {
                    if find(blocks, node).is_some() {
                        plan.actions.push((item.id.clone(), Action::Delete { node: node.clone() }));
                    }
                }
                place_ref(&mut plan, board, item, reference);
            }
            (Body::Ref(_), _) => {
                // A style reference changed: the scenes it is linked to
                // are worth a look.
                for link in board.links.iter().filter(|link| link.from == item.id || link.to == item.id) {
                    let other = if link.from == item.id { &link.to } else { &link.from };
                    if let (Some(scene), Some(scene_part)) = (board.item(other), built.parts.iter().find(|part| part.item == *other)) {
                        if matches!(scene.body, Body::Scene(_) | Body::Chapter(_) | Body::Lyric(_)) && !plan.asks.iter().any(|ask| ask.item == *other) {
                            plan.asks.push(ask_for(board, scene, scene_part, &format!("Its reference `{}` changed; check the scene still follows it.", item.id)));
                        }
                    }
                }
                plan.summary.push(format!("{} changed (reference only)", name_of(board, &item.id)));
            }
            _ => {
                plan.asks.push(ask_for(board, item, part, "The card changed; update the scene to follow it."));
                plan.summary.push(format!("{} changed: the AI updates that scene only", name_of(board, &item.id)));
            }
        }
    }
    plan
}

/// "scene 3" or "chapter 02/08".
fn unit_name(board: &Storyboard, item: &Item) -> String {
    match (&item.body, board.scene_number(&item.id), board.chapter_number(&item.id)) {
        (Body::Chapter(_), _, Some(n)) => format!("chapter {n:02}/{:02}", board.chapters().len()),
        (Body::Lyric(_), _, _) => format!("line {}", board.line_number(&item.id).unwrap_or(0)),
        (_, Some(n), _) => format!("scene {n}"),
        _ => "item".into(),
    }
}

fn ask_for(board: &Storyboard, item: &Item, part: &Part, task: &str) -> Ask {
    let scene = board.scene_number(&item.id).or_else(|| board.chapter_number(&item.id)).or_else(|| board.line_number(&item.id));
    let head = match (&item.body, scene) {
        (Body::Scene(_) | Body::Chapter(_) | Body::Lyric(_), Some(_)) => {
            let mut name = unit_name(board, item);
            name[..1].make_ascii_uppercase();
            format!("{name} (`{}`): {task}", item.id)
        }
        _ => format!("`{}`: {task}", item.id),
    };
    // The fields that changed are not known from the signature alone: the
    // context shows the card as it is now, and the nodes show what was made.
    Ask { item: item.id.clone(), scene, task: head, context: context(board, item, &[]), nodes: part.nodes.clone(), span: (item.start, item.length) }
}

fn place_ref(plan: &mut Plan, board: &Storyboard, item: &Item, reference: &Reference) {
    let Some(asset) = reference.src.clone() else {
        plan.summary.push(format!("{} is to be used but is not in the library yet", name_of(board, &item.id)));
        return;
    };
    let kind = match reference.media {
        Some(Media::Video) => "video",
        Some(Media::Audio) => "audio",
        Some(Media::Cut) => "cut",
        _ => "image",
    };
    plan.actions.push((item.id.clone(), Action::Place { asset, kind, start: item.start, length: item.length }));
    if reference.role == Role::Generate {
        let frame = kind == "image" || reference.media == Some(Media::Frame);
        plan.actions.push((item.id.clone(), Action::Generate {
            clip: None,
            start: item.start,
            length: item.length,
            preset: preset_of(&reference.gen.flow, frame),
            prompt: reference.gen.prompt.clone(),
            seed: reference.gen.seed,
            frame,
        }));
        plan.summary.push(format!("{}: a generative segment from it ({})", name_of(board, &item.id), if reference.gen.prompt.is_empty() { "no prompt".to_string() } else { format!("\"{}\"", reference.gen.prompt) }));
    } else {
        plan.summary.push(format!("{}: place it on the timeline", name_of(board, &item.id)));
    }
}

/// The board ops that close the same gaps on the board: every item that
/// starts at or after a removed card's end moves left by its length.
pub fn ripple(board: &Storyboard, plan: &Plan) -> Vec<crate::edit::Op> {
    if plan.ripple.is_empty() {
        return Vec::new();
    }
    let places: Vec<(String, f64, f64, Option<String>)> = board.items.iter().filter_map(|item| {
        let shift: f64 = plan.ripple.iter().filter(|(end, _)| item.start >= end - 1e-6).map(|(_, length)| length).sum();
        (shift > 0.0).then(|| (item.id.clone(), (item.start - shift).max(0.0), item.length, None))
    }).collect();
    if places.is_empty() { Vec::new() } else { vec![crate::edit::Op::Place(places)] }
}

/// The build record after an update: parts of items handled mechanically
/// take their new span and signature; parts with an open Ask keep the old
/// ones (they stay "out of date" until the AI marks them built); parts of
/// removed items go. `made` adds nodes the update created, per item.
pub fn rebuilt(board: &Storyboard, built: &Built, plan: &Plan, made: &[(String, Vec<String>)], edit: AssetRef) -> Built {
    let mut parts = Vec::new();
    for item in &board.items {
        let asked = plan.asks.iter().any(|ask| ask.item == item.id);
        let old = built.parts.iter().find(|part| part.item == item.id);
        let mut nodes: Vec<String> = old.map(|part| part.nodes.clone()).unwrap_or_default();
        // Replaced (deleted) nodes go.
        nodes.retain(|node| !plan.actions.iter().any(|(owner, action)| owner == &item.id && matches!(action, Action::Delete { node: n } if n == node)));
        nodes.retain(|node| !plan.missing.contains(node));
        if let Some((_, new)) = made.iter().find(|(id, _)| *id == item.id) {
            nodes.extend(new.iter().cloned());
        }
        match (old, asked) {
            (Some(old), true) => parts.push(Part { nodes, ..old.clone() }),
            (None, true) => {}
            _ if nodes.is_empty() && old.is_none() => {}
            _ => parts.push(Part { item: item.id.clone(), nodes, start: item.start, length: item.length, sig: signature(item) }),
        }
    }
    Built { edit, board: board.revision(), parts }
}

/// Record that `item`'s part of the cut is now `nodes`, made from the
/// item as it is (the AI's `storyboard_mark_built`).
pub fn mark(board: &Storyboard, built: Option<&Built>, edit: AssetRef, marks: &[(String, Vec<String>)]) -> Result<Built, String> {
    let mut out = built.cloned().unwrap_or(Built { edit: edit.clone(), board: String::new(), parts: Vec::new() });
    out.edit = edit;
    for (id, nodes) in marks {
        let item = board.item(id).ok_or_else(|| format!("there is no item `{id}` on the board"))?;
        out.parts.retain(|part| part.item != *id);
        out.parts.push(Part { item: id.clone(), nodes: nodes.clone(), start: item.start, length: item.length, sig: signature(item) });
    }
    out.parts.sort_by_key(|part| board.items.iter().position(|item| item.id == part.item).unwrap_or(usize::MAX));
    out.board = board.revision();
    Ok(out)
}

/// Items whose part of the cut no longer matches them (the "out of
/// date" badges), and items not built at all.
pub fn stale(board: &Storyboard) -> (Vec<String>, Vec<String>) {
    let Some(built) = &board.built else { return (Vec::new(), Vec::new()) };
    let mut stale = Vec::new();
    let mut unbuilt = Vec::new();
    for item in &board.items {
        match built.parts.iter().find(|part| part.item == item.id) {
            Some(part) => {
                if part.sig != signature(item) || (part.start - item.start).abs() > 1e-6 || (part.length - item.length).abs() > 1e-6 {
                    stale.push(item.id.clone());
                }
            }
            None => {
                let matters = match &item.body {
                    Body::Scene(_) | Body::Chapter(_) | Body::Lyric(_) => true,
                    Body::Device(_) => false,
                    Body::Note { .. } => board.row(&item.row).is_some_and(|row| row.kind == RowKind::Voice),
                    Body::Ref(reference) => reference.role != Role::Reference,
                    // Plans, not parts of the cut.
                    Body::Treatment(_) | Body::Page(_) => false,
                };
                if matters {
                    unbuilt.push(item.id.clone());
                }
            }
        }
    }
    (stale, unbuilt)
}

/// The initial build record the host writes after "Make video" from the
/// edit's blocks: each scene card gets the blocks inside its span on the
/// picture (not voice/audio), each voiceover note the voice line that
/// starts inside it. Used when the AI did not mark its parts itself.
pub fn infer(board: &Storyboard, blocks: &[Block], edit: AssetRef) -> Built {
    let mut parts = Vec::new();
    let mut taken: Vec<&str> = Vec::new();
    for item in &board.items {
        let row = board.row(&item.row).map(|row| row.kind);
        let inside = |block: &Block| block.start >= item.start - 0.05 && block.start < item.end() - 0.05;
        let nodes: Vec<String> = match (&item.body, row) {
            (Body::Scene(_) | Body::Chapter(_) | Body::Lyric(_), _) => blocks.iter().filter(|block| !matches!(block.kind.as_str(), "voice" | "voice_line" | "audio" | "caption") && inside(block) && !taken.contains(&block.id.as_str())).map(|block| block.id.clone()).collect(),
            (Body::Note { .. }, Some(RowKind::Voice)) => blocks.iter().filter(|block| block.kind == "voice_line" && inside(block)).map(|block| block.id.clone()).collect(),
            _ => Vec::new(),
        };
        if nodes.is_empty() {
            continue;
        }
        taken.extend(blocks.iter().filter(|block| nodes.contains(&block.id)).map(|block| block.id.as_str()));
        parts.push(Part { item: item.id.clone(), nodes, start: item.start, length: item.length, sig: signature(item) });
    }
    Built { edit, board: board.revision(), parts }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::{apply, Op, Patch, Value};

    const BOARD: &str = "let story = Row{kind: @story}\nlet voice = Row{kind: @voice}\nlet refs = Row{kind: @refs}\nlet hook = Scene{row: story span: [0.0, 4.0] title: \"Hook\"}\nlet demo = Scene{row: story span: [4.0, 6.0] title: \"Demo\"}\nlet outro = Scene{row: story span: [10.0, 4.0] title: \"Outro\"}\nlet vo1 = Note{row: voice span: [0.5, 3.0] text: \"Ideas move fast.\"}\nlet vo2 = Note{row: voice span: [4.5, 4.0] text: \"Notes keeps up.\"}\nlet photo = Ref{row: refs span: [4.0, 3.0] media: @image src: asset(\"ast_p\", \"arev_p\") label: \"Desk\"}\nlet l1 = Link{from: demo to: photo}\nStoryboard{title: \"T\" duration: 14.0 rows: [story, voice, refs] items: [hook, demo, outro, vo1, vo2, photo] links: [l1]}\n";

    fn block(id: &str, kind: &str, start: f64, length: f64) -> Block {
        Block { id: id.into(), kind: kind.into(), start, length, text: String::new() }
    }

    fn edit_blocks() -> Vec<Block> {
        vec![
            block("g_hook", "graphic", 0.0, 4.0),
            block("t_hook", "title", 1.0, 2.0),
            block("c_demo", "clip", 4.0, 6.0),
            block("g_outro", "graphic", 10.0, 4.0),
            block("hand", "clip", 11.0, 2.0),
            block("vo", "voice", 0.5, 8.0),
            block("vo#0", "voice_line", 0.5, 2.5),
            block("vo#1", "voice_line", 4.5, 3.0),
            block("music", "audio", 0.0, 14.0),
        ]
    }

    fn built() -> (Storyboard, Built) {
        let (board, _) = Storyboard::load(BOARD).unwrap();
        let mut built = infer(&board, &edit_blocks(), AssetRef { asset_id: "ast_e".into(), revision: "arev_1".into() });
        // `hand` was added by hand after the build.
        for part in &mut built.parts {
            part.nodes.retain(|node| node != "hand");
        }
        (board, built)
    }

    #[test]
    fn infer_assigns_blocks_to_cards_and_lines_to_notes() {
        let (_, built) = built();
        let nodes = |item: &str| built.parts.iter().find(|part| part.item == item).map(|part| part.nodes.clone()).unwrap_or_default();
        assert_eq!(nodes("hook"), ["g_hook", "t_hook"]);
        assert_eq!(nodes("demo"), ["c_demo"]);
        assert_eq!(nodes("outro"), ["g_outro"]);
        assert_eq!(nodes("vo1"), ["vo#0"]);
        assert_eq!(nodes("vo2"), ["vo#1"]);
        assert!(nodes("photo").is_empty());
    }

    #[test]
    fn an_unchanged_board_plans_nothing() {
        let (board, built) = built();
        let plan = plan(&board, &built, &edit_blocks(), Gap::Keep);
        assert!(plan.is_empty(), "{plan:?}");
        assert_eq!(plan.untouched.len(), board.items.len());
        assert_eq!(stale(&Storyboard { built: Some(built), ..board }).0, Vec::<String>::new());
    }

    #[test]
    fn moves_retimes_and_rewords_are_mechanical() {
        let (_, built) = built();
        let mut src = apply(BOARD, &Op::Place(vec![("hook".into(), 2.0, 4.0, None), ("demo".into(), 6.0, 3.0, None)])).unwrap().source;
        src = apply(&src, &Op::Update { id: "vo2".into(), patch: Patch { fields: vec![("text".into(), Value::Text("Notes keeps up with you.".into()))] } }).unwrap().source;
        let (board, _) = Storyboard::load(&src).unwrap();
        let plan = plan(&board, &built, &edit_blocks(), Gap::Keep);
        assert!(plan.asks.is_empty(), "{:?}", plan.asks);
        let actions: Vec<&Action> = plan.actions.iter().map(|(_, action)| action).collect();
        assert!(actions.contains(&&Action::Move { node: "g_hook".into(), start: 2.0 }));
        assert!(actions.contains(&&Action::Move { node: "t_hook".into(), start: 3.0 }));
        // demo: 4–10 → 6–9, half as long.
        assert!(actions.contains(&&Action::Trim { node: "c_demo".into(), start: 6.0, length: 3.0 }));
        assert!(actions.contains(&&Action::VoiceLine { node: "vo".into(), line: 1, text: Some("Notes keeps up with you.".into()), at: None }));
        // Nothing else: the outro, vo1, the hand-made clip and the music stay.
        assert_eq!(actions.len(), 4);
        for untouched in ["outro", "vo1"] {
            assert!(plan.untouched.contains(&untouched.to_string()));
        }
        let (stale_items, _) = stale(&Storyboard { built: Some(built.clone()), ..board.clone() });
        assert_eq!(stale_items, ["hook", "demo", "vo2"]);
        let next = rebuilt(&board, &built, &plan, &[], built.edit.clone());
        assert!(stale(&Storyboard { built: Some(next), ..board }).0.is_empty());
    }

    #[test]
    fn content_changes_become_scoped_asks_and_deletes_close_gaps() {
        let (_, built) = built();
        let mut src = apply(BOARD, &Op::Update { id: "hook".into(), patch: Patch { fields: vec![("mood".into(), Value::Text("calm, warm".into()))] } }).unwrap().source;
        src = apply(&src, &Op::Delete(vec!["demo".into()])).unwrap().source;
        let (board, _) = Storyboard::load(&src).unwrap();
        let plan = plan(&board, &built, &edit_blocks(), Gap::Close);
        assert_eq!(plan.asks.len(), 1);
        let ask = &plan.asks[0];
        assert_eq!((ask.item.as_str(), ask.scene, ask.nodes.clone()), ("hook", Some(1), vec!["g_hook".to_string(), "t_hook".to_string()]));
        assert!(ask.context.contains("mood: calm, warm"), "{}", ask.context);
        assert!(ask.context.contains("Voiceover over it (0.5–3.5 s): \"Ideas move fast.\""), "{}", ask.context);
        assert!(plan.ask_text().contains("storyboard_mark_built"));
        let actions: Vec<&Action> = plan.actions.iter().map(|(_, action)| action).collect();
        assert!(actions.contains(&&Action::Delete { node: "c_demo".into() }));
        // The outro's graphic and the hand clip after the gap move left 6 s.
        assert!(actions.contains(&&Action::Move { node: "hand".into(), start: 5.0 }));
        assert!(actions.contains(&&Action::Move { node: "g_outro".into(), start: 4.0 }));
        assert_eq!(ripple(&board, &plan), vec![Op::Place(vec![("outro".into(), 4.0, 4.0, None)])]);
        assert!(!actions.iter().any(|action| matches!(action, Action::Move { node, .. } if node == "music" || node == "g_hook")));
        // The hook keeps its old part until the AI marks it built.
        let next = rebuilt(&board, &built, &plan, &[], built.edit.clone());
        assert!(stale(&Storyboard { built: Some(next.clone()), ..board.clone() }).0.contains(&"hook".to_string()));
        let marked = mark(&board, Some(&next), built.edit.clone(), &[("hook".into(), vec!["g_hook".into(), "g_hook2".into()])]).unwrap();
        assert!(!stale(&Storyboard { built: Some(marked), ..board }).0.contains(&"hook".to_string()));
    }

    #[test]
    fn references_to_use_or_generate_from_become_blocks() {
        let (_, built) = built();
        let src = apply(BOARD, &Op::Update { id: "photo".into(), patch: Patch { fields: vec![("role".into(), Value::Tag("generate".into())), ("prompt".into(), Value::Text("slow push in, golden hour".into())), ("seed".into(), Value::Int(7))] } }).unwrap().source;
        let (board, _) = Storyboard::load(&src).unwrap();
        let plan = plan(&board, &built, &edit_blocks(), Gap::Keep);
        let actions: Vec<&Action> = plan.actions.iter().filter(|(item, _)| item == "photo").map(|(_, action)| action).collect();
        assert_eq!(actions[0], &Action::Place { asset: AssetRef { asset_id: "ast_p".into(), revision: "arev_p".into() }, kind: "image", start: 4.0, length: 3.0 });
        assert_eq!(actions[1], &Action::Generate { clip: None, start: 4.0, length: 3.0, preset: "bundled:image-to-video".into(), prompt: "slow push in, golden hour".into(), seed: Some(7), frame: true });
    }

    #[test]
    fn a_new_voice_note_is_a_new_line_and_a_new_card_an_ask() {
        let (_, built) = built();
        let mut src = apply(BOARD, &Op::Add(Item { id: "vo3".into(), row: "voice".into(), start: 10.5, length: 3.0, color: None, band: None, body: Body::Note { text: "Try it today.".into() } })).unwrap().source;
        src = apply(&src, &Op::Add(Item { id: "cta".into(), row: "story".into(), start: 14.0, length: 2.0, color: None, band: None, body: Body::Scene(Scene { title: "CTA".into(), ..Default::default() }) })).unwrap().source;
        let (board, _) = Storyboard::load(&src).unwrap();
        let plan = plan(&board, &built, &edit_blocks(), Gap::Keep);
        assert!(plan.actions.iter().any(|(_, action)| *action == Action::AddVoiceLine { node: Some("vo".into()), text: "Try it today.".into(), at: 10.5 }));
        assert_eq!(plan.asks.len(), 1);
        assert!(plan.asks[0].task.contains("Build new scene 4 (`cta`)"), "{}", plan.asks[0].task);
    }
}
