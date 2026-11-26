//! The timeline's edits as graph edits: each takes the cut's
//! source and returns its next source, written through the patchers in
//! `patch.rs`, and the loaded document that proves it. Pure; evaluates
//! Splash, so it runs on the cut worker.
//!
//! The picture of a cut made on this page is one `Stack` (the output's
//! frame) whose layers are the video blocks, bottom first; audio blocks are
//! entries of the output's `audio`. An effect wraps the chain it is dropped
//! on; a transition takes the two chains that meet in the stack and stands
//! in the first one's place.

use super::patch::{NewNode, Patcher, Value};
use makepad_flow::graph::edges_of;
use makepad_flow::{Graph, Literal, NodeInputValue};
use makepad_flow_ui::graph_edit;
use makepad_story_cut::{AssetRef, Duck, CutDoc, CutNode, CutNodeKind, EffectRef, MediaRef, SourceTime, Span, VoiceLine, VoiceSpec};

/// What a library row adds to the timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Video,
    Image,
    Audio,
    /// An app run's pinned render, played as a clip.
    AppRun,
    /// Another cut, nested as a compound clip (`Clip{src: cut(…)}`).
    Cut,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Media {
    pub kind: MediaKind,
    pub src: MediaRef,
    /// Its own length, when the library knows it.
    pub seconds: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CutOp {
    /// A new block at `start`; on `track` when that is free there, else on
    /// the first free track of its lane.
    Add { media: Media, start: f64, track: Option<u32> },
    Move { node: String, start: f64, track: u32 },
    /// New span; moving the start moves the source `in` with it.
    Trim { node: String, start: f64, length: f64 },
    /// Wrap the block's chain in an `Fx`.
    Effect { node: String, effect: String },
    /// A transition over the overlap of two blocks.
    Transition { a: String, b: String, effect: String },
    /// Blocks' spans (and `in`, when given) set together, as one cut: the
    /// clips round a transition when it is made longer or shorter.
    Respan { spans: Vec<Respan> },
    /// A transition on the cut point between two adjacent clips on one
    /// track (Final Cut's way): the spans first (the clips extended into
    /// their media handles, or the later clips rippled left when there are
    /// none), then the transition over the overlap they make.
    CutTransition { a: String, b: String, effect: String, spans: Vec<Respan> },
    Delete { node: String },
    /// Blade: cut the block in two at `t`; the second half is a new node
    /// on the same track.
    Split { node: String, t: f64 },
    Param { node: String, key: String, value: Value },
    Label(String),
    /// A header value of the cut: `output`, `duration`, `bpm`.
    Header { key: String, value: Value },
    /// A voiceover with one line at `start`.
    AddVoiceOver { start: f64, voice: String },
    /// A voiceover's lines, as a whole.
    SetLines { node: String, lines: Vec<VoiceLine> },
    /// Captions of a voiceover, on top of the picture.
    AddSubtitles { voice: String },
    /// A text title at `start` on the first free video track.
    AddTitle { start: f64, text: String },
    /// An audio source's fades, in seconds.
    Fade { node: String, fade_in: f64, fade_out: f64 },
    /// Duck this source under `under` (voice node ids), or stop ducking.
    Duck { node: String, under: Option<Vec<String>>, depth_db: Option<f64> },
    /// A live app of a presentation's slide as a layer on top of the
    /// slide's picture: an `AppView` for the app `app` of the slide's `apps`,
    /// shown the whole edit.
    AppLayer { app: String },
    /// The whole source, as typed in the Source view (it loads: checked).
    Source { text: String },
}

/// A block's new span, and its new source `in` when that moves too.
#[derive(Clone, Debug, PartialEq)]
pub struct Respan {
    pub node: String,
    pub start: f64,
    pub length: f64,
    pub source_in: Option<f64>,
}

fn respan(patcher: &Patcher, source: &str, doc: &CutDoc, spans: &[Respan]) -> Result<String, String> {
    let end = spans.iter().map(|span| span.start + span.length).fold(0.0, f64::max);
    let mut written = extend(patcher, source, doc, end)?;
    for span in spans {
        let mut values = vec![("span".to_string(), Value::span(span.start.max(0.0), span.length.max(0.001)))];
        if let Some(source_in) = span.source_in {
            values.push(("in".into(), Value::num(source_in.max(0.0))));
        }
        written = patcher.set_params(&written, Some(&span.node), &values)?;
    }
    Ok(written)
}

/// Timeline lanes: video tracks, then audio tracks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lane {
    Video,
    Audio,
    /// Subtitles, above the picture: drawn only, never dropped on.
    Captions,
    /// Voiceovers, under the sound.
    Voice,
}

pub fn lane_of(node: &CutNode) -> Option<Lane> {
    match node.kind {
        CutNodeKind::Clip { .. } | CutNodeKind::Still { .. } | CutNodeKind::Title { .. } => Some(Lane::Video),
        CutNodeKind::AudioClip { .. } => Some(Lane::Audio),
        CutNodeKind::VoiceOver { .. } => Some(Lane::Voice),
        _ => None,
    }
}

/// Seconds a new block of `kind` lasts when its media's length is unknown.
fn default_seconds(kind: MediaKind) -> f64 {
    match kind {
        MediaKind::Video | MediaKind::AppRun | MediaKind::Cut => 5.0,
        MediaKind::Image => 4.0,
        MediaKind::Audio => 10.0,
    }
}

/// The first track of `lane` with nothing on `[start, end)`, `preferred`
/// when that one is free. Effects and transitions sit over blocks and do
/// not occupy tracks.
pub fn free_track(doc: &CutDoc, lane: Lane, start: f64, end: f64, preferred: Option<u32>, except: Option<&str>) -> u32 {
    let busy = |track: u32| doc.nodes.iter().any(|node| {
        Some(node.id.as_str()) != except && lane_of(node) == Some(lane) && node.track == track
            && node.span.map_or(true, |span| span.start < end && start < span.end())
    });
    if let Some(track) = preferred.filter(|track| !busy(*track)) {
        return track;
    }
    (0..).find(|track| !busy(*track)).unwrap()
}

/// A nested cut revision, `edit("id", "rev")`.
pub fn cut_value(edit: &AssetRef) -> Value {
    let lit = |text: &str| Literal::Str(text.to_string());
    Value {
        text: format!("cut(\"{}\", \"{}\")", edit.id, edit.rev),
        literal: Literal::Obj(vec![("ref".into(), Literal::Id("edit".into())), ("id".into(), lit(&edit.id)), ("rev".into(), lit(&edit.rev))]),
    }
}

pub fn media_value(media: &MediaRef) -> Value {
    let lit = |text: &str| Literal::Str(text.to_string());
    match media {
        MediaRef::MissingOptional(path) => Value { text: makepad_widgets::makepad_script::syntax::write::quote(path), literal: lit(path) },
        MediaRef::Asset(asset) => Value {
            text: format!("asset(\"{}\", \"{}\")", asset.id, asset.rev),
            literal: Literal::Obj(vec![("ref".into(), Literal::Id("asset".into())), ("id".into(), lit(&asset.id)), ("rev".into(), lit(&asset.rev))]),
        },
        MediaRef::Run(run) => Value {
            text: format!("run(\"{}\", \"{}\", \"{}\", \"{}\")", run.id, run.rev, run.render_id, run.render),
            literal: Literal::Obj(vec![
                ("ref".into(), Literal::Id("run".into())), ("id".into(), lit(&run.id)), ("rev".into(), lit(&run.rev)),
                ("render_id".into(), lit(&run.render_id)), ("render".into(), lit(&run.render)),
            ]),
        },
    }
}

pub fn media_param(value: &Value) -> Option<MediaRef> {
    let Literal::Obj(fields) = &value.literal else { return None; };
    let field = |name| fields.iter().find(|(key, _)| key == name).map(|(_, value)| value);
    let text = |name| match field(name) { Some(Literal::Str(text)) => Some(text.clone()), _ => None };
    match field("ref") {
        Some(Literal::Id(kind)) if kind == "asset" || kind == "cut" => Some(MediaRef::Asset(AssetRef { id: text("id")?, rev: text("rev")? })),
        Some(Literal::Id(kind)) if kind == "run" => Some(MediaRef::Run(makepad_story_cut::RunRef { id: text("id")?, rev: text("rev")?, render_id: text("render_id")?, render: text("render")? })),
        _ => None,
    }
}

fn marker_value(name: &str, offset: f64) -> Value {
    let offset = Value::num(offset);
    Value {
        text: format!("marker(@{name}, {})", offset.text),
        literal: Literal::Obj(vec![("marker".into(), Literal::Id(name.into())), ("offset".into(), offset.literal)]),
    }
}

/// Apply `op` to `source`: the next source and its loaded document.
pub fn apply(patcher: &Patcher, source: &str, op: &CutOp) -> Result<(String, CutDoc), String> {
    let doc = patcher.load(source)?;
    let written = match op {
        CutOp::Add { media, start, track } => add(patcher, source, &doc, media, *start, *track)?,
        CutOp::Move { node, start, track } if doc.graphic(node).is_some() => {
            // A motion graphic lives in `doc.graphics`.
            let length = doc.graphic(node).and_then(|graphic| graphic.span).map_or(3.0, |span| span.length);
            let start = start.max(0.0);
            let source = extend(patcher, source, &doc, start + length)?;
            patcher.set_params(&source, Some(node), &[("span".into(), Value::span(start, length)), ("track".into(), Value::num(*track as f64))])?
        }
        CutOp::Trim { node, start, length } if doc.graphic(node).is_some() => {
            let (start, length) = (start.max(0.0), length.max(0.1));
            let source = extend(patcher, source, &doc, start + length)?;
            patcher.set_params(&source, Some(node), &[("span".into(), Value::span(start, length))])?
        }
        CutOp::Delete { node } if doc.generate(node).is_some() => delete_generate(patcher, source, node)?,
        CutOp::Move { node, start, track } => {
            // An effect's band moves its clip: an effect has no time of its own.
            let node = &wrapped_block(&doc, node);
            let block = block(&doc, node)?;
            let span0 = block.span.ok_or_else(|| format!("`{node}` has no span to move"))?;
            let start = start.max(0.0);
            // A transition whose clips no longer meet goes with the move (one
            // step, undone together), the cut under it restored.
            let moved = Span { start, length: span0.length };
            let mut source = source.to_string();
            for (id, other) in joined(&doc, node) {
                let current = patcher.load(&source)?;
                let meets = current.node(&other).and_then(|other| other.span).is_some_and(|other| overlap(moved, other).is_some());
                if !meets && current.node(&id).is_some() {
                    source = delete_transition(patcher, &source, &current, &id, true)?;
                }
            }
            let current = patcher.load(&source)?;
            // The restore may have moved the clip's own edge: keep the drag's
            // offset, not its absolute start.
            let span = block_span(&current, node)?;
            let start = (span.start + (start - span0.start)).max(0.0);
            let source = extend(patcher, &source, &current, start + span.length)?;
            patcher.set_params(&source, Some(node), &[("span".into(), Value::span(start, span.length)), ("track".into(), Value::num(*track as f64))])?
        }
        CutOp::Respan { spans } => respan(patcher, source, &doc, spans)?,
        CutOp::CutTransition { a, b, effect, spans } => {
            let spanned = respan(patcher, source, &doc, spans)?;
            let current = patcher.load(&spanned)?;
            transition(patcher, &spanned, &current, a, b, effect)?
        }
        CutOp::Trim { node, start, length } => trim(patcher, source, &doc, &wrapped_block(&doc, node), *start, *length)?,
        CutOp::Effect { node, effect } => wrap_effect(patcher, source, &doc, node, effect)?,
        CutOp::Transition { a, b, effect } => transition(patcher, source, &doc, a, b, effect)?,
        CutOp::Delete { node } => delete(patcher, source, &doc, node)?,
        CutOp::Source { text } => text.clone(),
        CutOp::AppLayer { app } => add_app_layer(patcher, source, &doc, app)?,
        CutOp::Split { node, t } => split(patcher, source, &doc, node, *t)?,
        CutOp::Param { node, key, value } => patcher.set_params(source, Some(node), &[(key.clone(), media_param(value).map_or_else(|| value.clone(), |media| patcher.media_value(&media)))])?,
        CutOp::Label(label) => patcher.set_params(source, None, &[("label".into(), Value::string(label))])?,
        CutOp::Header { key, value } => patcher.set_params(source, None, &[(key.clone(), value.clone())])?,
        CutOp::AddVoiceOver { start, voice } => add_voiceover(patcher, source, &doc, *start, voice)?,
        CutOp::SetLines { node, lines } => patcher.set_params(source, Some(node), &[("lines".into(), lines_value(patcher, lines))])?,
        CutOp::AddSubtitles { voice } => add_subtitles(patcher, source, &doc, voice)?,
        CutOp::AddTitle { start, text } => add_title(patcher, source, &doc, *start, text)?,
        CutOp::Fade { node, fade_in, fade_out } => {
            patcher.set_params(source, Some(node), &[("fade_in".into(), Value::num(fade_in.max(0.0))), ("fade_out".into(), Value::num(fade_out.max(0.0)))])?
        }
        CutOp::Duck { node, under, depth_db } => {
            let value = match under {
                Some(under) => {
                    let mut duck = Duck { under: under.clone(), ..Duck::default() };
                    if let Some(depth_db) = depth_db {
                        duck.depth_db = depth_db.min(0.0);
                    }
                    Value::object(vec![
                        ("under", Value::list(duck.under.iter().map(|id| Value::id(id)).collect())),
                        ("depth_db", Value::num(duck.depth_db)),
                        ("attack", Value::num(duck.attack)),
                        ("release", Value::num(duck.release)),
                    ])
                }
                None => Value::nil(),
            };
            patcher.set_params(source, Some(node), &[("duck".into(), value)])?
        }
    };
    let written = normalize(patcher, &written)?;
    let doc = patcher.load(&written)?;
    #[cfg(any(test, debug_assertions))]
    {
        let broken = super::invariants::check(&doc);
        if !broken.is_empty() {
            let message = format!("{op:?} broke the cut: {}", broken.join("; "));
            if cfg!(test) {
                panic!("{message}\n{written}");
            }
            makepad_widgets::log!("video_cut: {message}");
        }
    }
    Ok((written, doc))
}

/// The cut's `output` object with `fps` and `loudness`, its size kept.
pub fn output_value(doc: &CutDoc, fps: f64, loudness: Option<f64>) -> Value {
    let mut output = vec![("width", Value::num(doc.output.width as f64)), ("height", Value::num(doc.output.height as f64)), ("fps", Value::num(super::rates::written(fps)))];
    if let Some(lufs) = loudness {
        output.push(("loudness", Value::num(lufs)));
    }
    Value::object(output)
}

/// A clip, still or title on the timeline: what effects wrap and
/// transitions join.
pub fn timed_block(doc: &CutDoc, id: &str) -> Option<CutNode> {
    doc.node(id).filter(|node| node.span.is_some() && matches!(node.kind, CutNodeKind::Clip { .. } | CutNodeKind::Still { .. } | CutNodeKind::Title { .. })).cloned()
        .or_else(|| doc.graphic(id).and_then(graphic_block))
}

/// A Graphic as the video block it is on the timeline: a Motion
/// document drawn over its span, like a title (which lowers to one), so
/// everything that places, joins and extends blocks takes it the same way,
/// with a title's unlimited handles. `None` without a span.
pub fn graphic_block(graphic: &makepad_story_cut::GraphicNode) -> Option<CutNode> {
    graphic.span?;
    Some(CutNode {
        id: graphic.id.clone(),
        span: graphic.span,
        track: graphic.track,
        loc: graphic.loc.clone(),
        kind: CutNodeKind::Title { text: String::new(), anchor: [0.5, 0.5], font_size: makepad_story_cut::Animated::scalar(0.0), style: String::new() },
    })
}

/// Every video block of `doc` with a span: clips, stills, titles and graphics.
pub fn video_blocks(doc: &CutDoc) -> Vec<CutNode> {
    doc.nodes.iter().filter_map(|node| timed_block(doc, &node.id)).chain(doc.graphics.iter().filter_map(graphic_block)).collect()
}

/// `id`, or the block an effect `id` wraps (its band stands for the clip).
fn wrapped_block(doc: &CutDoc, id: &str) -> String {
    if doc.generate(id).is_some() {
        let base = chain_base(doc, id);
        return if timed_block(doc, &base).is_some() { base } else { id.to_string() };
    }
    match doc.node(id).map(|node| &node.kind) {
        Some(CutNodeKind::Fx { .. }) => {
            let base = chain_base(doc, id);
            if timed_block(doc, &base).is_some() { base } else { id.to_string() }
        }
        _ => id.to_string(),
    }
}

fn block_span(doc: &CutDoc, id: &str) -> Result<Span, String> {
    block(doc, id)?.span.ok_or_else(|| format!("`{id}` has no span"))
}

/// After every op (the cut model's invariants): an effect on a clip has the
/// clip's span and track, a transition lies inside the overlap of the two
/// clips it joins, and one whose clips no longer meet is gone. One op, so
/// one undo step.
fn normalize(patcher: &Patcher, source: &str) -> Result<String, String> {
    let mut source = source.to_string();
    for _ in 0..64 {
        let doc = patcher.load(&source)?;
        // Transitions first: dropping one rewires chains.
        let mut dropped = None;
        let mut changes: Vec<(String, Vec<(String, Value)>)> = Vec::new();
        for tr in &doc.nodes {
            let CutNodeKind::Transition { a: Some(a), b: Some(b), .. } = &tr.kind else { continue };
            let (base_a, base_b) = (side_base(&doc, &a.node, true), side_base(&doc, &b.node, false));
            let (Some(block_a), Some(block_b)) = (timed_block(&doc, &base_a), timed_block(&doc, &base_b)) else { continue };
            let Some(over) = overlap(block_a.span.unwrap(), block_b.span.unwrap()) else {
                dropped = Some(tr.id.clone());
                break;
            };
            let span = tr.span.unwrap_or(over);
            if span.start < over.start - 1e-6 || span.end() > over.end() + 1e-6 {
                let fit = overlap(span, over).unwrap_or(over);
                changes.push((tr.id.clone(), vec![("span".into(), Value::span(fit.start, fit.length))]));
            }
        }
        if let Some(id) = dropped {
            source = delete_transition(patcher, &source, &doc, &id, false)?;
            continue;
        }
        // A Generate over a clip follows it (its take goes stale when the
        // input changes, which a respan does).
        for gen in &doc.generates {
            let Some(base) = timed_block(&doc, &chain_base(&doc, &gen.id)) else { continue };
            let span = base.span.unwrap();
            if (gen.span.start - span.start).abs() > 1e-9 || (gen.span.length - span.length).abs() > 1e-9 || gen.track != base.track {
                changes.push((gen.id.clone(), vec![("span".into(), Value::span(span.start, span.length)), ("track".into(), Value::num(base.track as f64))]));
            }
        }
        for fx in doc.nodes.iter().filter(|node| matches!(node.kind, CutNodeKind::Fx { .. })) {
            let Some(base) = timed_block(&doc, &chain_base(&doc, &fx.id)) else { continue };
            let span = base.span.unwrap();
            let same = fx.span.is_some_and(|own| (own.start - span.start).abs() < 1e-9 && (own.length - span.length).abs() < 1e-9);
            if !same || fx.track != base.track {
                changes.push((fx.id.clone(), vec![("span".into(), Value::span(span.start, span.length)), ("track".into(), Value::num(base.track as f64))]));
            }
        }
        if changes.is_empty() {
            return Ok(source);
        }
        for (id, values) in changes {
            source = patcher.set_params(&source, Some(&id), &values)?;
        }
    }
    Err("the cut did not settle".into())
}

fn block<'a>(doc: &'a CutDoc, id: &str) -> Result<&'a CutNode, String> {
    doc.node(id).ok_or_else(|| format!("the cut has no `{id}`"))
}

/// Grow the cut to cover `end`, in whole seconds.
fn extend(patcher: &Patcher, source: &str, doc: &CutDoc, end: f64) -> Result<String, String> {
    if end <= doc.duration + 1e-9 {
        return Ok(source.to_string());
    }
    patcher.set_params(source, None, &[("duration".into(), Value::num(end.ceil()))])
}

/// The stack the output's picture is, which new video blocks join.
fn picture_stack(doc: &CutDoc) -> Result<String, String> {
    let out = block(doc, &doc.output_node)?;
    let CutNodeKind::Output { frame, .. } = &out.kind else { return Err("the cut has no output".into()) };
    let frame = frame.as_ref().ok_or("the cut's output shows nothing; wire a Stack into it")?;
    match doc.node(&frame.node).map(|node| &node.kind) {
        Some(CutNodeKind::Stack { .. }) => Ok(frame.node.clone()),
        _ => Err("the cut's picture is not a Stack; add blocks in the flow canvas".into()),
    }
}

fn add(patcher: &Patcher, source: &str, doc: &CutDoc, media: &Media, start: f64, track: Option<u32>) -> Result<String, String> {
    let start = start.max(0.0);
    let length = media.seconds.filter(|seconds| *seconds > 0.05).unwrap_or(default_seconds(media.kind));
    let (type_name, stem, lane) = match media.kind {
        MediaKind::Video | MediaKind::AppRun | MediaKind::Cut => ("Clip", "clip", Lane::Video),
        MediaKind::Image => ("Still", "still", Lane::Video),
        MediaKind::Audio => ("AudioClip", "audio", Lane::Audio),
    };
    let track = free_track(doc, lane, start, start + length, track, None);
    let source = extend(patcher, source, doc, start + length)?;
    let id = patcher.fresh_id(&source, stem);
    let src = match (&media.kind, &media.src) {
        (MediaKind::Cut, MediaRef::Asset(edit)) if patcher.tree.is_none() => cut_value(edit),
        _ => patcher.media_value(&media.src),
    };
    let mut params = vec![
        ("src".to_string(), src),
        ("span".to_string(), Value::span(start, length)),
        ("track".to_string(), Value::num(track as f64)),
    ];
    if lane == Lane::Video {
        params.push(("fit".to_string(), Value::id("fill")));
    }
    let node = NewNode { id: id.clone(), type_name: type_name.into(), at: (start * 40.0 + 300.0, track as f64 * 90.0 + if lane == Lane::Audio { 400.0 } else { 0.0 }), params, wires: Vec::new() };
    let added = patcher.add_nodes(&source, &[node])?;
    match lane {
        Lane::Video => {
            let stack = picture_stack(doc)?;
            patcher.rewire(&added, |graph| graph_edit::connect(graph, &id, "frame", &stack, "layers"))
        }
        Lane::Audio | Lane::Captions | Lane::Voice => {
            let out = doc.output_node.clone();
            patcher.rewire(&added, |graph| graph_edit::connect(graph, &id, "audio", &out, "audio"))
        }
    }
}

fn trim(patcher: &Patcher, source: &str, doc: &CutDoc, id: &str, start: f64, length: f64) -> Result<String, String> {
    let graphic = doc.graphic(id).and_then(graphic_block);
    let node = match &graphic {
        Some(node) => node,
        None => block(doc, id)?,
    };
    let span = node.span.ok_or_else(|| format!("`{id}` has no span to trim"))?;
    let (start, length) = (start.max(0.0), length.max(0.1));
    let source = extend(patcher, source, doc, start + length)?;
    let mut values = vec![("span".to_string(), Value::span(start, length))];
    // The picture under a left edge stays where it was: `in` moves with it.
    let delta = start - span.start;
    if delta.abs() > 1e-9 {
        let (source_in, speed) = match &node.kind {
            CutNodeKind::Clip { source_in, speed, .. } => (Some(source_in), *speed),
            CutNodeKind::AudioClip { source_in, .. } => (Some(source_in), 1.0),
            _ => (None, 1.0),
        };
        match source_in {
            Some(SourceTime::Seconds(seconds)) => values.push(("in".into(), Value::num((seconds + delta * speed).max(0.0)))),
            Some(SourceTime::Marker { name, offset }) => values.push(("in".into(), marker_value(name, offset + delta * speed))),
            None => {}
        }
    }
    patcher.set_params(&source, Some(id), &values)
}

/// Every input that reads `node`'s frame: `(consumer, port, entry id)`.
fn frame_readers(graph: &Graph, node: &str) -> Vec<(String, String, Option<u64>)> {
    let mut readers = Vec::new();
    for consumer in &graph.nodes {
        for input in &consumer.inputs {
            match &input.value {
                NodeInputValue::Edge(edge) if edge.from_node == node && edge.from_port == "frame" => {
                    readers.push((consumer.id.clone(), input.port.clone(), None));
                }
                NodeInputValue::Edges(entries) => {
                    for entry in entries.iter().filter(|entry| entry.from.from_node == node && entry.from.from_port == "frame") {
                        readers.push((consumer.id.clone(), input.port.clone(), Some(entry.id)));
                    }
                }
                _ => {}
            }
        }
    }
    readers
}

/// Point every reader of `from`'s frame (but `except`) at `to`'s frame,
/// keeping ordered entries' ids and places.
fn redirect(graph: &Graph, from: &str, to: &str, except: &str) -> Graph {
    let mut next = graph.clone();
    for consumer in next.nodes.iter_mut().filter(|node| node.id != except) {
        for input in &mut consumer.inputs {
            match &mut input.value {
                NodeInputValue::Edge(edge) if edge.from_node == from && edge.from_port == "frame" => edge.from_node = to.to_string(),
                NodeInputValue::Edges(entries) => {
                    for entry in entries.iter_mut().filter(|entry| entry.from.from_node == from && entry.from.from_port == "frame") {
                        entry.from.from_node = to.to_string();
                    }
                }
                _ => {}
            }
        }
    }
    next.edges = edges_of(&next.nodes);
    next
}

/// The node standing for `id` in the picture stack: `id`, or the effect or
/// transition chain that wraps it, followed up to the stack.
fn chain_top(doc: &CutDoc, id: &str) -> Result<String, String> {
    let stack = picture_stack(doc)?;
    let mut current = id.to_string();
    for _ in 0..=doc.nodes.len() + doc.generates.len() {
        let readers: Vec<&str> = doc.wiring().filter(|(from, port, _, _)| *from == current && *port == "frame").map(|(_, _, to, _)| to).collect();
        if readers.contains(&stack.as_str()) {
            return Ok(current);
        }
        match readers.as_slice() {
            [single] if is_wrapper(doc, single) || matches!(doc.node(single).map(|node| &node.kind), Some(CutNodeKind::Transition { .. })) => current = single.to_string(),
            _ => return Err(format!("`{id}` is not a layer of the cut's picture; wire it in the flow canvas")),
        }
    }
    Err(format!("`{id}`'s chain loops"))
}

fn wrap_effect(patcher: &Patcher, source: &str, doc: &CutDoc, id: &str, effect: &str) -> Result<String, String> {
    wrap_effect_with(patcher, source, doc, id, Value::string(effect), Vec::new())
}

fn wrap_effect_with(patcher: &Patcher, source: &str, doc: &CutDoc, id: &str, effect: Value, extra: Vec<(String, Value)>) -> Result<String, String> {
    let node = block(doc, id)?;
    if !node.kind_has_frame() || matches!(node.kind, CutNodeKind::Transition { .. }) {
        return Err(format!("`{id}` has no picture to put an effect on"));
    }
    // The effect takes its clip's time (an effect on an effect: the clip's).
    let node = timed_block(doc, &chain_base(doc, id)).unwrap_or_else(|| node.clone());
    let fx = patcher.fresh_id(source, "fx");
    let mut params = vec![("effect".to_string(), effect), ("track".to_string(), Value::num(node.track as f64))];
    params.extend(extra);
    if let Some(span) = node.span {
        params.push(("span".into(), Value::span(span.start, span.length)));
    }
    let at = (node.span.map_or(0.0, |span| span.start) * 40.0 + 340.0, node.track as f64 * 90.0 + 40.0);
    let new = NewNode { id: fx.clone(), type_name: "Fx".into(), at, params, wires: vec![("input".into(), id.to_string(), "frame".into())] };
    let added = patcher.add_nodes(source, &[new])?;
    patcher.rewire(&added, |graph| Ok(redirect(graph, id, &fx, &fx)))
}

trait HasFrame {
    fn kind_has_frame(&self) -> bool;
}

impl HasFrame for CutNode {
    fn kind_has_frame(&self) -> bool {
        !matches!(self.kind, CutNodeKind::AudioClip { .. } | CutNodeKind::Output { .. })
    }
}

/// `[start, length]` shared by two spans, when they overlap.
pub fn overlap(a: Span, b: Span) -> Option<Span> {
    let (start, end) = (a.start.max(b.start), a.end().min(b.end()));
    (end - start > 1e-6).then(|| Span { start, length: end - start })
}

fn transition(patcher: &Patcher, source: &str, doc: &CutDoc, a: &str, b: &str, effect: &str) -> Result<String, String> {
    let (block_a, block_b) = (timed_block(doc, a).ok_or_else(|| format!("the cut has no video block `{a}`"))?, timed_block(doc, b).ok_or_else(|| format!("the cut has no video block `{b}`"))?);
    if a == b || lane_of(&block_a) != Some(Lane::Video) || lane_of(&block_b) != Some(Lane::Video) {
        return Err("a transition joins two video blocks".into());
    }
    let (span_a, span_b) = (block_a.span.ok_or("the first block has no span")?, block_b.span.ok_or("the second block has no span")?);
    // The earlier block plays out, the later one plays in.
    let ((first, _), (second, _)) = if (span_a.start, span_a.end()) <= (span_b.start, span_b.end()) { ((a, span_a), (b, span_b)) } else { ((b, span_b), (a, span_a)) };
    let over = overlap(span_a, span_b).ok_or("the blocks do not overlap; drag one over the other first")?;
    let (top_a, top_b) = (chain_top(doc, first)?, chain_top(doc, second)?);
    if top_a == top_b || joined(doc, first).iter().any(|(_, other)| other == second) {
        return Err("these clips are already joined by a transition".into());
    }
    let stack = picture_stack(doc)?;
    let id = patcher.fresh_id(source, "tr");
    let track = block_a.track.min(block_b.track);
    let new = NewNode {
        id: id.clone(),
        type_name: "Transition".into(),
        at: (over.start * 40.0 + 360.0, track as f64 * 90.0 + 60.0),
        params: vec![("effect".into(), Value::string(effect)), ("span".into(), Value::span(over.start, over.length)), ("track".into(), Value::num(track as f64))],
        wires: vec![("a".into(), top_a.clone(), "frame".into()), ("b".into(), top_b.clone(), "frame".into())],
    };
    let added = patcher.add_nodes(source, &[new])?;
    patcher.rewire(&added, |graph| {
        // The transition takes the first chain's layer; the second chain
        // leaves the stack (the transition plays it after its span).
        let mut next = redirect(graph, &top_a, &id, &id);
        for (consumer, port, key) in frame_readers(&next, &top_b) {
            if consumer == stack && key.is_some() {
                next = graph_edit::disconnect(&next, &consumer, &port, key);
            }
        }
        Ok(next)
    })
}

/// The block a chain of effects and transitions starts from.
pub fn chain_base(doc: &CutDoc, id: &str) -> String {
    let mut current = id.to_string();
    for _ in 0..=doc.nodes.len() + doc.generates.len() {
        // A Generate wraps its input like an effect.
        if let Some(input) = doc.generate(&current).and_then(|gen| gen.input.as_ref()) {
            current = input.node.clone();
            continue;
        }
        match doc.node(&current).map(|node| &node.kind) {
            Some(CutNodeKind::Fx { input: Some(input), .. }) => current = input.node.clone(),
            _ => break,
        }
    }
    current
}

/// Whether `id` wraps one input chain: an effect or a Generate.
fn is_wrapper(doc: &CutDoc, id: &str) -> bool {
    doc.generate(id).is_some() || matches!(doc.node(id).map(|node| &node.kind), Some(CutNodeKind::Fx { .. }))
}

/// The block at the cut end of a transition's input chain. The chain that
/// plays out (`a_side`) ends in its latest clip, so through an earlier
/// transition it follows that one's `b`; the chain that plays in starts
/// with its earliest clip, following `a`.
pub fn side_base(doc: &CutDoc, id: &str, a_side: bool) -> String {
    let mut current = id.to_string();
    for _ in 0..=doc.nodes.len() + doc.generates.len() {
        if let Some(input) = doc.generate(&current).and_then(|gen| gen.input.as_ref()) {
            current = input.node.clone();
            continue;
        }
        match doc.node(&current).map(|node| &node.kind) {
            Some(CutNodeKind::Fx { input: Some(input), .. }) => current = input.node.clone(),
            Some(CutNodeKind::Transition { a: Some(a), b: Some(b), .. }) => current = if a_side { b.node.clone() } else { a.node.clone() },
            _ => break,
        }
    }
    current
}

/// The transitions joining `node` to another block: `(transition, other)`.
pub fn joined(doc: &CutDoc, node: &str) -> Vec<(String, String)> {
    doc.nodes.iter().filter_map(|tr| match &tr.kind {
        CutNodeKind::Transition { a: Some(a), b: Some(b), .. } => {
            let (a, b) = (side_base(doc, &a.node, true), side_base(doc, &b.node, false));
            if a == node { Some((tr.id.clone(), b)) } else if b == node { Some((tr.id.clone(), a)) } else { None }
        }
        _ => None,
    }).collect()
}

fn delete(patcher: &Patcher, source: &str, doc: &CutDoc, id: &str) -> Result<String, String> {
    let node = block(doc, id)?;
    let unwired = match &node.kind {
        CutNodeKind::Stack { .. } | CutNodeKind::Output { .. } => return Err(format!("`{id}` is the cut's structure; edit it in the flow canvas")),
        CutNodeKind::Fx { input, .. } => {
            let input = input.as_ref().map(|wire| wire.node.clone());
            patcher.rewire(source, |graph| {
                let Some(input) = input else {
                    return Ok(frame_readers(graph, id).into_iter().fold(graph.clone(), |graph, (consumer, port, key)| graph_edit::disconnect(&graph, &consumer, &port, key)));
                };
                let next = redirect(graph, id, &input, id);
                Ok(graph_edit::disconnect(&next, id, "input", None))
            })?
        }
        CutNodeKind::Transition { .. } => return delete_transition(patcher, source, doc, id, true),
        _ => {
            // Its effects and transitions go with it (one step).
            let wrapped = doc.generates.iter().any(|gen| chain_base(doc, &gen.id) == id);
            if wrapped || timed_block(doc, id).is_some() && doc.wiring().any(|(from, _, to, _)| from == id && doc.node(to).is_some_and(|node| matches!(node.kind, CutNodeKind::Fx { .. } | CutNodeKind::Transition { .. }))) || !joined(doc, id).is_empty() {
                let mut source = source.to_string();
                for _ in 0..doc.nodes.len() + doc.generates.len() {
                    let current = patcher.load(&source)?;
                    // A Generate over the clip goes too (its takes stay in the library).
                    if let Some(gen) = current.generates.iter().find(|gen| chain_base(&current, &gen.id) == id).map(|gen| gen.id.clone()) {
                        source = delete_generate(patcher, &source, &gen)?;
                        continue;
                    }
                    if let Some((tr, _)) = joined(&current, id).into_iter().next() {
                        source = delete_transition(patcher, &source, &current, &tr, true)?;
                        continue;
                    }
                    let fx = current.wiring().find(|(from, port, to, _)| *from == id && *port == "frame" && current.node(to).is_some_and(|node| matches!(node.kind, CutNodeKind::Fx { .. }))).map(|(_, _, to, _)| to.to_string());
                    match fx {
                        Some(fx) => source = delete(patcher, &source, &current, &fx)?,
                        None => break,
                    }
                }
                let current = patcher.load(&source)?;
                return delete(patcher, &source, &current, id);
            }
            if let Some((_, _, reader, _)) = doc.wiring().find(|(from, _, to, _)| *from == id && doc.node(to).is_some_and(|node| matches!(node.kind, CutNodeKind::Fx { .. } | CutNodeKind::Transition { .. } | CutNodeKind::Subtitles { .. }))) {
                return Err(format!("`{reader}` reads `{id}`; remove it first"));
            }
            let port = if matches!(node.kind, CutNodeKind::AudioClip { .. } | CutNodeKind::VoiceOver { .. }) { "audio" } else { "frame" };
            patcher.rewire(source, |graph| {
                let mut next = graph.clone();
                for consumer in graph.nodes.iter() {
                    for input in &consumer.inputs {
                        match &input.value {
                            NodeInputValue::Edge(edge) if edge.from_node == id && edge.from_port == port => {
                                next = graph_edit::disconnect(&next, &consumer.id, &input.port, None);
                            }
                            NodeInputValue::Edges(entries) => {
                                for entry in entries.iter().filter(|entry| entry.from.from_node == id) {
                                    next = graph_edit::disconnect(&next, &consumer.id, &input.port, Some(entry.id));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Ok(next)
            })?
        }
    };
    patcher.remove_nodes(&unwired, &[id.to_string()])
}

/// Remove a Generate: its readers read its input again.
fn delete_generate(patcher: &Patcher, source: &str, id: &str) -> Result<String, String> {
    let doc = patcher.load(source)?;
    let input = doc.generate(id).and_then(|gen| gen.input.as_ref()).map(|wire| wire.node.clone());
    let unwired = patcher.rewire(source, |graph| {
        let Some(input) = &input else {
            return Ok(frame_readers(graph, id).into_iter().fold(graph.clone(), |graph, (consumer, port, key)| graph_edit::disconnect(&graph, &consumer, &port, key)));
        };
        let next = redirect(graph, id, input, id);
        Ok(graph_edit::disconnect(&next, id, "input", None))
    })?;
    patcher.remove_nodes(&unwired, &[id.to_string()])
}

/// Remove a transition. Its readers take the chain on their side of it (a
/// later transition reading it as `a` takes its `b`), a chain nothing
/// reads any more returns to the stack, and with `restore` two clips on one
/// track are trimmed back to the cut at its centre (Final Cut's delete).
fn delete_transition(patcher: &Patcher, source: &str, doc: &CutDoc, id: &str, restore: bool) -> Result<String, String> {
    let node = block(doc, id)?;
    let CutNodeKind::Transition { a, b, .. } = &node.kind else { return Err(format!("`{id}` is not a transition")) };
    let (a, b) = (a.as_ref().map(|wire| wire.node.clone()), b.as_ref().map(|wire| wire.node.clone()));
    let mut source = source.to_string();
    if let (true, Some(a), Some(b), Some(span)) = (restore, &a, &b, node.span) {
        let (base_a, base_b) = (side_base(doc, a, true), side_base(doc, b, false));
        if let (Some(block_a), Some(block_b)) = (timed_block(doc, &base_a), timed_block(doc, &base_b)) {
            let (sa, sb) = (block_a.span.unwrap(), block_b.span.unwrap());
            let centre = (span.start + span.length * 0.5).clamp(sb.start, sa.end());
            if block_a.track == block_b.track && sa.end() > sb.start && centre > sa.start + 0.05 && centre < sb.end() - 0.05 {
                source = trim(patcher, &source, doc, &base_a, sa.start, centre - sa.start)?;
                let current = patcher.load(&source)?;
                source = trim(patcher, &source, &current, &base_b, centre, sb.end() - centre)?;
            }
        }
    }
    let transitions: Vec<String> = doc.nodes.iter().filter(|node| matches!(node.kind, CutNodeKind::Transition { .. })).map(|node| node.id.clone()).collect();
    let stack = picture_stack(doc).ok();
    let unwired = patcher.rewire(&source, |graph| {
        let mut next = graph.clone();
        for consumer in next.nodes.iter_mut().filter(|node| node.id != id) {
            let is_transition = transitions.contains(&consumer.id);
            for input in &mut consumer.inputs {
                let side = if is_transition && input.port == "a" { b.clone() } else { a.clone() };
                match &mut input.value {
                    NodeInputValue::Edge(edge) if edge.from_node == id && edge.from_port == "frame" => {
                        if let Some(side) = side { edge.from_node = side; }
                    }
                    NodeInputValue::Edges(entries) => {
                        for entry in entries.iter_mut().filter(|entry| entry.from.from_node == id && entry.from.from_port == "frame") {
                            if let Some(side) = &side { entry.from.from_node = side.clone(); }
                        }
                    }
                    _ => {}
                }
            }
        }
        next.edges = edges_of(&next.nodes);
        next = graph_edit::disconnect(&next, id, "a", None);
        next = graph_edit::disconnect(&next, id, "b", None);
        for (consumer, port, key) in frame_readers(&next, id) {
            next = graph_edit::disconnect(&next, &consumer, &port, key);
        }
        // A chain nothing reads any more is its own layer again.
        if let Some(stack) = &stack {
            for side in [&a, &b].into_iter().flatten() {
                if frame_readers(&next, side).is_empty() {
                    next = graph_edit::connect(&next, side, "frame", stack, "layers")?;
                }
            }
        }
        Ok(next)
    })?;
    patcher.remove_nodes(&unwired, &[id.to_string()])
}

/// An effect's params as written values, keys shifted `offset` seconds
/// earlier (a copy on a clip's later half). Unset (0) params stay unset.
fn fx_param_values(params: &[makepad_story_cut::Animated<1>], offset: f64) -> Vec<(String, Value)> {
    use makepad_story_cut::Animated;
    params.iter().enumerate().filter_map(|(index, param)| {
        let value = match param {
            Animated::Const([value]) if *value == 0.0 => return None,
            Animated::Const([value]) => Value::num(*value),
            Animated::Keys(keys) => {
                let first = keys.sample(offset)[0];
                let mut points: Vec<(f64, f64, Option<&str>)> = vec![(0.0, first, None)];
                points.extend(keys.points.iter().filter(|point| point.t - offset > 1e-6).map(|point| (point.t - offset, point.v[0], Some(point.ease.id()).filter(|ease| *ease != "linear"))));
                if points.len() == 1 { Value::num(first) } else { Value::keys(&points) }
            }
        };
        Some((format!("p{}", index + 1), value))
    }).collect()
}

fn split(patcher: &Patcher, source: &str, doc: &CutDoc, id: &str, t: f64) -> Result<String, String> {
    let id = &wrapped_block(doc, id);
    let node = block(doc, id)?;
    for (tr, _) in joined(doc, id) {
        if doc.node(&tr).and_then(|tr| tr.span).is_some_and(|span| t > span.start - 1e-6 && t < span.end() + 1e-6) {
            return Err("cut outside the transition".into());
        }
    }
    let span = node.span.ok_or_else(|| format!("`{id}` has no span to cut"))?;
    if t <= span.start + 0.01 || t >= span.end() - 0.01 {
        return Err("put the playhead inside the block to cut it".into());
    }
    let (first, second) = (t - span.start, span.end() - t);
    let mut params = vec![("span".to_string(), Value::span(t, second)), ("track".to_string(), Value::num(node.track as f64))];
    let (type_name, stem, port) = match &node.kind {
        CutNodeKind::Clip { src, source_in, speed, fit, .. } => {
            params.push(("src".into(), patcher.media_value(src)));
            params.push(("in".into(), match source_in {
                SourceTime::Seconds(seconds) => Value::num(seconds + first * speed),
                SourceTime::Marker { name, offset } => marker_value(name, offset + first * speed),
            }));
            if *speed != 1.0 {
                params.push(("speed".into(), Value::num(*speed)));
            }
            params.push(("fit".into(), Value::id(fit_id(*fit))));
            ("Clip", "clip", "frame")
        }
        CutNodeKind::Still { src, fit, .. } => {
            params.push(("src".into(), patcher.media_value(src)));
            params.push(("fit".into(), Value::id(fit_id(*fit))));
            ("Still", "still", "frame")
        }
        CutNodeKind::AudioClip { src, source_in, .. } => {
            params.push(("src".into(), patcher.media_value(src)));
            params.push(("in".into(), match source_in {
                SourceTime::Seconds(seconds) => Value::num(seconds + first),
                SourceTime::Marker { name, offset } => marker_value(name, offset + first),
            }));
            ("AudioClip", "audio", "audio")
        }
        CutNodeKind::Title { text, .. } => {
            params.push(("text".into(), Value::string(text)));
            ("Title", "title", "frame")
        }
        _ => return Err(format!("`{id}` is not a clip, still, title or audio block")),
    };
    let source = patcher.set_params(source, Some(id), &[("span".into(), Value::span(span.start, first))])?;
    let new = patcher.fresh_id(&source, stem);
    let at = (t * 40.0 + 300.0, node.track as f64 * 90.0 + if port == "audio" { 400.0 } else { 0.0 });
    let added = patcher.add_nodes(&source, &[NewNode { id: new.clone(), type_name: type_name.into(), at, params, wires: Vec::new() }])?;
    let (to, to_port) = if port == "audio" { (doc.output_node.clone(), "audio") } else { (picture_stack(doc)?, "layers") };
    let mut source = patcher.rewire(&added, |graph| graph_edit::connect(graph, &new, port, &to, to_port))?;
    if port == "audio" {
        return Ok(source);
    }
    // The later half keeps the clip's effects, innermost first ...
    let mut chain = Vec::new();
    let mut current = id.to_string();
    for _ in 0..doc.nodes.len() {
        let fx = doc.wiring().find(|(from, port, to, _)| *from == current && *port == "frame" && doc.node(to).is_some_and(|node| matches!(node.kind, CutNodeKind::Fx { .. }))).map(|(_, _, to, _)| to.to_string());
        let Some(fx) = fx else { break };
        chain.push(fx.clone());
        current = fx;
    }
    let mut top = new.clone();
    for fx in chain {
        let Some(CutNodeKind::Fx { effect, params, .. }) = doc.node(&fx).map(|node| &node.kind) else { continue };
        let effect = match effect {
            EffectRef::Preset(name) => Value::string(name),
            EffectRef::Asset(asset) => patcher.media_value(&MediaRef::Asset(asset.clone())),
        };
        let current = patcher.load(&source)?;
        let before: Vec<String> = current.nodes.iter().map(|node| node.id.clone()).collect();
        source = wrap_effect_with(patcher, &source, &current, &top, effect, fx_param_values(params, first))?;
        let after = patcher.load(&source)?;
        top = after.nodes.iter().find(|node| !before.contains(&node.id)).map(|node| node.id.clone()).unwrap_or(top);
    }
    // ... and a transition at its out point (it plays out of the later half).
    let current = patcher.load(&source)?;
    let outgoing = current.nodes.iter().find_map(|tr| match &tr.kind {
        CutNodeKind::Transition { a: Some(a), .. } if side_base(&current, &a.node, true) == *id => Some((tr.id.clone(), a.node.clone())),
        _ => None,
    });
    if let Some((tr, old)) = outgoing {
        let stack = picture_stack(&current)?;
        source = patcher.rewire(&source, |graph| {
            let mut next = graph.clone();
            for (consumer, port, key) in frame_readers(&next, &top) {
                next = graph_edit::disconnect(&next, &consumer, &port, key);
            }
            next = graph_edit::disconnect(&next, &tr, "a", None);
            next = graph_edit::connect(&next, &top, "frame", &tr, "a")?;
            if frame_readers(&next, &old).is_empty() {
                next = graph_edit::connect(&next, &old, "frame", &stack, "layers")?;
            }
            Ok(next)
        })?;
    }
    Ok(source)
}

fn source_time_value(at: &SourceTime) -> Value {
    match at {
        SourceTime::Seconds(seconds) => Value::num(*seconds),
        SourceTime::Marker { name, offset } => marker_value(name, *offset),
    }
}

/// `[line(text, at, take, key), …]`, as the prelude's `line()` builds them.
pub fn lines_value(patcher: &Patcher, lines: &[VoiceLine]) -> Value {
    let items = lines.iter().map(|line| {
        let text = Value::string(&line.text);
        let at = source_time_value(&line.at);
        let take = line.take.as_ref().map_or_else(Value::nil, |take| patcher.media_value(&MediaRef::Asset(take.clone())));
        let key = line.key.as_deref().map_or_else(Value::nil, Value::string);
        let mut args = vec![text.text.clone(), at.text.clone()];
        if line.take.is_some() || line.key.is_some() {
            args.push(take.text.clone());
            args.push(key.text.clone());
        }
        Value {
            text: format!("line({})", args.join(", ")),
            literal: Literal::Obj(vec![("line".into(), text.literal), ("at".into(), at.literal), ("take".into(), take.literal), ("key".into(), key.literal)]),
        }
    }).collect::<Vec<_>>();
    Value { text: format!("[\n        {}\n    ]", items.iter().map(|item| item.text.as_str()).collect::<Vec<_>>().join("\n        ")), literal: Literal::Arr(items.into_iter().map(|item| item.literal).collect()) }
}

pub fn voice_value(voice: &VoiceSpec) -> Value {
    Value::object(vec![("engine", Value::id(voice.engine.id())), ("id", Value::string(&voice.id)), ("speed", Value::num(voice.speed))])
}

fn add_voiceover(patcher: &Patcher, source: &str, doc: &CutDoc, start: f64, voice: &str) -> Result<String, String> {
    let id = patcher.fresh_id(source, "voice");
    let track = doc.nodes.iter().filter(|node| lane_of(node) == Some(Lane::Voice)).map(|node| node.track + 1).max().unwrap_or(0);
    let line = VoiceLine { text: "New line".into(), at: SourceTime::Seconds(start.max(0.0)), take: None, key: None };
    let node = NewNode {
        id: id.clone(),
        type_name: "VoiceOver".into(),
        at: (start * 40.0 + 300.0, 500.0),
        params: vec![("voice".into(), voice_value(&VoiceSpec { id: voice.to_string(), ..VoiceSpec::default() })), ("lines".into(), lines_value(patcher, &[line])), ("track".into(), Value::num(track as f64))],
        wires: Vec::new(),
    };
    let added = patcher.add_nodes(source, &[node])?;
    let out = doc.output_node.clone();
    patcher.rewire(&added, |graph| graph_edit::connect(graph, &id, "audio", &out, "audio"))
}

fn add_subtitles(patcher: &Patcher, source: &str, doc: &CutDoc, voice: &str) -> Result<String, String> {
    if !matches!(block(doc, voice)?.kind, CutNodeKind::VoiceOver { .. }) {
        return Err("subtitles follow a voiceover: select one first".into());
    }
    let stack = picture_stack(doc)?;
    let id = patcher.fresh_id(source, "subs");
    let node = NewNode {
        id: id.clone(),
        type_name: "Subtitles".into(),
        at: (600.0, 600.0),
        // Film subtitles by default: whole lines, no
        // word-by-word highlight.
        params: vec![("style".into(), Value::id("movie")), ("max_words".into(), Value::num(8.0))],
        wires: vec![("source".into(), voice.to_string(), "audio".into())],
    };
    let added = patcher.add_nodes(source, &[node])?;
    patcher.rewire(&added, |graph| graph_edit::connect(graph, &id, "frame", &stack, "layers"))
}

fn add_app_layer(patcher: &Patcher, source: &str, doc: &CutDoc, app: &str) -> Result<String, String> {
    let id = patcher.fresh_id(source, &format!("{app}_view"));
    let node = NewNode {
        id: id.clone(),
        type_name: "AppView".into(),
        at: (300.0, 0.0),
        params: vec![("app".into(), Value::id(app)), ("fit".into(), Value::id("fit")), ("span".into(), Value::span(0.0, doc.duration))],
        wires: Vec::new(),
    };
    let added = patcher.add_nodes(source, &[node])?;
    let stack = picture_stack(doc)?;
    patcher.rewire(&added, |graph| graph_edit::connect(graph, &id, "frame", &stack, "layers"))
}

fn add_title(patcher: &Patcher, source: &str, doc: &CutDoc, start: f64, text: &str) -> Result<String, String> {
    let length = 3.0;
    let track = free_track(doc, Lane::Video, start, start + length, None, None);
    let source = extend(patcher, source, doc, start + length)?;
    let id = patcher.fresh_id(&source, "title");
    let node = NewNode {
        id: id.clone(),
        type_name: "Title".into(),
        at: (start * 40.0 + 300.0, track as f64 * 90.0),
        params: vec![("text".into(), Value::string(text)), ("span".into(), Value::span(start.max(0.0), length)), ("track".into(), Value::num(track as f64))],
        wires: Vec::new(),
    };
    let added = patcher.add_nodes(&source, &[node])?;
    let stack = picture_stack(doc)?;
    patcher.rewire(&added, |graph| graph_edit::connect(graph, &id, "frame", &stack, "layers"))
}

fn fit_id(fit: makepad_story_cut::Fit) -> &'static str {
    match fit {
        makepad_story_cut::Fit::Fill => "fill",
        makepad_story_cut::Fit::Fit => "fit",
        makepad_story_cut::Fit::None => "none",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_story_cut::{CutEval, DrawKind};

    const EMPTY: &str = "use mod.flow.*\nuse mod.cut.*\n\nlet main = Stack{layers: []}\nlet out = CutOutput{frame: main.out(@frame) audio: []}\n\nFlow{\n    kind: @cut\n    label: \"Test\"\n    output: {width: 1080 height: 1920 fps: 60}\n    duration: 10.0\n    bpm: 120\n    main, out\n}\n";

    fn video(n: u8) -> Media {
        Media {
            kind: MediaKind::Video,
            src: MediaRef::Asset(AssetRef {
                id: makepad_asset_data::AssetId::from_bytes([n; 16]).to_string(),
                rev: format!("arev_{}", format!("{n:x}").repeat(64)),
            }),
            seconds: Some(6.0),
        }
    }

    fn run(patcher: &Patcher, source: &str, op: CutOp) -> (String, CutDoc) {
        apply(patcher, source, &op).unwrap_or_else(|error| panic!("{op:?}: {error}\n{source}"))
    }

    /// G1's Generate over a clip wraps it like an effect: it
    /// follows the clip, and goes with it.
    #[test]
    fn a_generate_over_a_clip_follows_it_and_goes_with_it() {
        let patcher = Patcher::new().unwrap();
        let (source, _) = run(&patcher, EMPTY, CutOp::Add { media: video(1), start: 0.0, track: None });
        let (source, _) = run(&patcher, &source, CutOp::Effect { node: "clip1".into(), effect: "15_acid_bloom".into() });
        let doc = CutDoc::load(&source).unwrap();
        let top = chain_top(&doc, "clip1").unwrap();
        let new = NewNode {
            id: "gen1".into(), type_name: "Generate".into(), at: (0.0, 0.0),
            params: vec![("span".into(), Value::span(0.0, 6.0)), ("flow".into(), Value::string("bundled:image-to-video")), ("prompt".into(), Value::string("birds")), ("seed".into(), Value::num(1.0)), ("track".into(), Value::num(0.0))],
            wires: vec![("input".into(), top.clone(), "frame".into())],
        };
        let added = patcher.add_nodes(&source, &[new]).unwrap();
        let source = patcher.rewire(&added, |graph| Ok(redirect(graph, &top, "gen1", "gen1"))).unwrap();
        let doc = CutDoc::load(&source).unwrap();
        assert!(super::super::invariants::check(&doc).is_empty(), "{:?}", super::super::invariants::check(&doc));
        assert_eq!(chain_base(&doc, "gen1"), "clip1");
        // A second effect goes under the Generate; the chain still reaches the picture.
        let (source, _) = run(&patcher, &source, CutOp::Effect { node: "clip1".into(), effect: "31_reel".into() });
        // Moved and trimmed, the clip takes its Generate along (the take goes stale).
        let (source, doc) = run(&patcher, &source, CutOp::Move { node: "clip1".into(), start: 3.0, track: 1 });
        let gen = doc.generate("gen1").unwrap();
        assert_eq!((gen.span.start, gen.track), (3.0, 1));
        let (source, doc) = run(&patcher, &source, CutOp::Trim { node: "clip1".into(), start: 3.0, length: 4.0 });
        assert_eq!(doc.generate("gen1").unwrap().span.length, 4.0);
        // Dragging the Generate's band moves the clip.
        let (source, doc) = run(&patcher, &source, CutOp::Move { node: "gen1".into(), start: 5.0, track: 1 });
        assert_eq!(doc.node("clip1").unwrap().span.unwrap().start, 5.0);
        // Deleting the Generate hands the picture back to its input.
        let (_, doc) = run(&patcher, &source, CutOp::Delete { node: "gen1".into() });
        assert!(doc.generates.is_empty());
        // Deleting the clip takes its effects and Generate with it.
        let (_, doc) = run(&patcher, &source, CutOp::Delete { node: "clip1".into() });
        assert!(doc.generates.is_empty() && doc.node("clip1").is_none() && !doc.nodes.iter().any(|node| matches!(node.kind, CutNodeKind::Fx { .. })));
    }

    /// A render draws the cut with its settings (`CutDoc::render_as`, the
    /// renderer's `--fps`/`--loudness`/`--no-burn`): the cut those settings
    /// written into its source load as, field for field.
    #[test]
    fn a_render_draws_the_cut_with_its_settings() {
        let patcher = Patcher::new().unwrap();
        let (source, _) = run(&patcher, EMPTY, CutOp::Add { media: video(1), start: 0.0, track: None });
        let (source, _) = run(&patcher, &source, CutOp::AddVoiceOver { start: 1.0, voice: "bf_lily".into() });
        let (source, doc) = run(&patcher, &source, CutOp::AddSubtitles { voice: "voice1".into() });
        let stack = picture_stack(&doc).unwrap();
        assert!(frame_readers(&doc.graph, "subs1").iter().any(|(consumer, ..)| *consumer == stack), "the captions are in the picture");
        let written = |fps: Option<f64>, loudness: Option<f64>, burn: bool| {
            let doc = patcher.load(&source).unwrap();
            let mut text = patcher.set_params(&source, None, &[("output".to_string(), output_value(&doc, fps.unwrap_or(doc.output.fps), loudness))]).unwrap();
            if !burn {
                text = patcher.rewire(&text, |graph| Ok(frame_readers(graph, "subs1").into_iter().filter(|(consumer, ..)| *consumer == stack)
                    .fold(graph.clone(), |graph, (consumer, port, key)| graph_edit::disconnect(&graph, &consumer, &port, key)))).unwrap();
            }
            CutDoc::load(&text).unwrap()
        };
        let plain = |doc: &CutDoc| doc.nodes.iter().map(|node| (node.id.clone(), node.span, node.track, node.kind.clone())).collect::<Vec<_>>();
        for (fps, loudness, burn) in [(None, Some(-16.0), true), (None, None, true), (Some(24.0), Some(-14.0), false), (Some(60000.0 / 1001.0), None, false), (Some(120.0), Some(-23.0), true)] {
            let expected = written(fps, loudness, burn);
            let mut doc = CutDoc::load(&source).unwrap();
            doc.render_as(fps, loudness, burn);
            let case = format!("fps {fps:?} loudness {loudness:?} burn {burn}");
            assert_eq!(doc.output, expected.output, "{case}");
            assert_eq!(plain(&doc), plain(&expected), "{case}");
            assert_eq!(doc.graph.edges, expected.graph.edges, "{case}");
            assert_eq!(makepad_story_edit_render::audio::fps_ratio(doc.output.fps), makepad_story_edit_render::audio::fps_ratio(fps.unwrap_or(60.0)), "{case}");
        }
        // A cut that names its loudness renders without it when the render says none.
        let mut doc = written(None, Some(-16.0), true);
        doc.render_as(None, None, true);
        assert_eq!(doc.output.loudness, None);
        // The API's Header{output} sets the cut's own.
        let (_, doc) = run(&patcher, &source, CutOp::Header { key: "output".into(), value: Value::object(vec![("width", Value::num(720.0)), ("height", Value::num(1280.0)), ("fps", Value::num(30.0))]) });
        assert_eq!((doc.output.width, doc.output.height, doc.output.fps), (720, 1280, 30.0));
    }

    #[test]
    fn a_nested_edit_is_a_compound_clip_and_its_captions_opt_in() {
        let patcher = Patcher::new().unwrap();
        let mut media = video(3);
        media.kind = MediaKind::Cut;
        let (source, doc) = run(&patcher, EMPTY, CutOp::Add { media, start: 0.0, track: None });
        assert!(source.contains("cut(\""));
        let node = doc.nodes.iter().find(|node| node.kind.nested_edit().is_some()).unwrap().id.clone();
        let (_, doc) = run(&patcher, &source, CutOp::Param { node: node.clone(), key: "subtitles".into(), value: Value::num(1.0) });
        assert!(matches!(doc.node(&node).unwrap().kind, CutNodeKind::Clip { nested: true, subtitles: true, .. }));
    }

    #[test]
    fn a_transition_on_a_cut_then_the_clips_moved_apart() {
        let patcher = Patcher::new().unwrap();
        let (source, _) = run(&patcher, EMPTY, CutOp::Add { media: video(1), start: 0.0, track: None });
        let (source, doc) = run(&patcher, &source, CutOp::Add { media: video(2), start: 6.0, track: None });
        assert_eq!((doc.node("clip1").unwrap().track, doc.node("clip2").unwrap().track), (0, 0));
        let (source, _) = run(&patcher, &source, CutOp::Effect { node: "clip1".into(), effect: "15_acid_bloom".into() });
        let spans = vec![
            Respan { node: "clip1".into(), start: 0.0, length: 6.5, source_in: None },
            Respan { node: "clip2".into(), start: 5.5, length: 6.5, source_in: Some(0.0) },
        ];
        let (source, doc) = run(&patcher, &source, CutOp::CutTransition { a: "clip1".into(), b: "clip2".into(), effect: "101_trans_dissolve".into(), spans });
        let tr = doc.nodes.iter().find(|node| matches!(node.kind, CutNodeKind::Transition { .. })).unwrap();
        let span = tr.span.unwrap();
        assert!((span.start - 5.5).abs() < 1e-9 && (span.length - 1.0).abs() < 1e-9);
        assert_eq!(joined(&doc, "clip1").len(), 1);
        // The effect on the first clip follows it into the overlap.
        let fx = doc.nodes.iter().find(|node| matches!(node.kind, CutNodeKind::Fx { .. })).unwrap();
        assert!((fx.span.unwrap().end() - 6.5).abs() < 1e-9);
        // Moved apart: the transition goes with the move.
        let (_, doc) = run(&patcher, &source, CutOp::Move { node: "clip2".into(), start: 20.0, track: 0 });
        assert!(!doc.nodes.iter().any(|node| matches!(node.kind, CutNodeKind::Transition { .. })));
        // The cut under the transition is restored (clip2 starts at 6 again,
        // clip1 ends there), and the drag's offset is kept.
        assert_eq!(doc.node("clip2").unwrap().span.unwrap().start, 20.5);
        assert_eq!(doc.node("clip1").unwrap().span.unwrap().end(), 6.0);
    }

    #[test]
    fn two_clips_a_transition_and_back() {
        let patcher = Patcher::new().unwrap();
        let (source, doc) = run(&patcher, EMPTY, CutOp::Add { media: video(1), start: 0.0, track: None });
        assert_eq!(doc.node("clip1").unwrap().track, 0);
        // Overlapping: the second clip lands on the next free track, and
        // the cut grows to hold it.
        let (source, doc) = run(&patcher, &source, CutOp::Add { media: video(2), start: 5.0, track: Some(0) });
        let clip2 = doc.node("clip2").unwrap();
        assert_eq!((clip2.track, doc.duration), (1, 11.0));
        let (source, doc) = run(&patcher, &source, CutOp::Transition { a: "clip2".into(), b: "clip1".into(), effect: "13_pixel_dissolve".into() });
        let tr = doc.node("tr1").unwrap();
        assert_eq!(tr.span, Some(Span { start: 5.0, length: 1.0 }));
        let CutNodeKind::Transition { a, b, .. } = &tr.kind else { panic!() };
        assert_eq!((a.as_ref().unwrap().node.as_str(), b.as_ref().unwrap().node.as_str()), ("clip1", "clip2"));
        let CutNodeKind::Stack { layers } = &doc.node("main").unwrap().kind else { panic!() };
        assert_eq!(layers.iter().map(|(wire, _)| wire.node.as_str()).collect::<Vec<_>>(), vec!["tr1"]);
        // Before, in and after the transition the picture is clip1, both, clip2.
        let eval = CutEval::new(&doc);
        let sources = |t: f64| eval.frame_plan(t).ops.iter().filter_map(|op| matches!(op.kind, DrawKind::Source { .. }).then(|| op.node.clone())).collect::<Vec<_>>();
        assert_eq!(sources(2.0), vec!["clip1"]);
        assert_eq!(sources(5.5), vec!["clip1", "clip2"]);
        assert_eq!(sources(8.0), vec!["clip2"]);
        // Undoing it by delete puts both layers back.
        let (_, doc) = run(&patcher, &source, CutOp::Delete { node: "tr1".into() });
        let CutNodeKind::Stack { layers } = &doc.node("main").unwrap().kind else { panic!() };
        assert_eq!(layers.len(), 2);
    }

    #[test]
    fn effects_wrap_and_unwrap_and_blocks_move_and_trim() {
        let patcher = Patcher::new().unwrap();
        let (source, _) = run(&patcher, EMPTY, CutOp::Add { media: video(1), start: 1.0, track: None });
        let (source, doc) = run(&patcher, &source, CutOp::Effect { node: "clip1".into(), effect: "15_acid_bloom".into() });
        let CutNodeKind::Stack { layers } = &doc.node("main").unwrap().kind else { panic!() };
        assert_eq!(layers[0].0.node, "fx1");
        // Deleting the clip takes its effect with it (one step).
        let (_, gone) = run(&patcher, &source, CutOp::Delete { node: "clip1".into() });
        assert!(gone.node("clip1").is_none() && gone.node("fx1").is_none());
        let (source, doc) = run(&patcher, &source, CutOp::Move { node: "clip1".into(), start: 2.0, track: 2 });
        let clip = doc.node("clip1").unwrap();
        assert_eq!((clip.span.unwrap().start, clip.track), (2.0, 2));
        let (source, doc) = run(&patcher, &source, CutOp::Trim { node: "clip1".into(), start: 3.0, length: 4.0 });
        let CutNodeKind::Clip { source_in: SourceTime::Seconds(source_in), .. } = &doc.node("clip1").unwrap().kind else { panic!() };
        assert_eq!(*source_in, 1.0);
        let (split, doc) = run(&patcher, &source, CutOp::Split { node: "clip1".into(), t: 5.0 });
        assert_eq!(doc.node("clip1").unwrap().span.unwrap().length, 2.0);
        let CutNodeKind::Clip { source_in: SourceTime::Seconds(second_in), .. } = &doc.node("clip2").unwrap().kind else { panic!() };
        assert_eq!((doc.node("clip2").unwrap().span.unwrap().start, *second_in), (5.0, 3.0));
        assert!(split.contains("let clip2 = Clip{"));
        let (source, doc) = run(&patcher, &source, CutOp::Delete { node: "fx1".into() });
        let CutNodeKind::Stack { layers } = &doc.node("main").unwrap().kind else { panic!() };
        assert_eq!(layers[0].0.node, "clip1");
        let (source, doc) = run(&patcher, &source, CutOp::Delete { node: "clip1".into() });
        assert!(doc.node("clip1").is_none());
        let (_, doc) = run(&patcher, &source, CutOp::Label("Renamed".into()));
        assert_eq!(doc.label, "Renamed");
        let (_, doc) = run(&patcher, &source, CutOp::Header { key: "duration".into(), value: Value::num(24.0) });
        assert_eq!(doc.duration, 24.0);
        let (_, doc) = run(&patcher, &source, CutOp::Header { key: "bpm".into(), value: Value::num(96.0) });
        assert_eq!(doc.bpm, 96.0);
    }

    #[test]
    fn voiceovers_lines_subtitles_titles_fades_and_ducking() {
        let patcher = Patcher::new().unwrap();
        let mut media = video(3);
        media.kind = MediaKind::Audio;
        let (source, _) = run(&patcher, EMPTY, CutOp::Add { media, start: 0.0, track: None });
        let (source, doc) = run(&patcher, &source, CutOp::AddVoiceOver { start: 1.0, voice: "bf_lily".into() });
        let CutNodeKind::VoiceOver { lines, .. } = &doc.node("voice1").unwrap().kind else { panic!() };
        assert_eq!(lines[0].text, "New line");
        let mut lines = lines.clone();
        lines[0].text = "Hello there.".into();
        lines.push(VoiceLine { text: "Second.".into(), at: SourceTime::Seconds(4.0), take: None, key: Some("tk1-0000000000000000".into()) });
        let (source, doc) = run(&patcher, &source, CutOp::SetLines { node: "voice1".into(), lines });
        let CutNodeKind::VoiceOver { lines, .. } = &doc.node("voice1").unwrap().kind else { panic!() };
        assert_eq!((lines.len(), lines[1].key.as_deref()), (2, Some("tk1-0000000000000000")));
        assert!(source.contains("line(\"Hello there.\", 1.0)"), "{source}");
        let (source, doc) = run(&patcher, &source, CutOp::AddSubtitles { voice: "voice1".into() });
        assert!(matches!(doc.node("subs1").unwrap().kind, CutNodeKind::Subtitles { .. }));
        let (source, doc) = run(&patcher, &source, CutOp::Fade { node: "audio1".into(), fade_in: 0.5, fade_out: 1.0 });
        let CutNodeKind::AudioClip { shape, .. } = &doc.node("audio1").unwrap().kind else { panic!() };
        assert_eq!((shape.fade_in, shape.fade_out), (0.5, 1.0));
        let (source, doc) = run(&patcher, &source, CutOp::Duck { node: "audio1".into(), under: Some(vec!["voice1".into()]), depth_db: None });
        let CutNodeKind::AudioClip { shape, .. } = &doc.node("audio1").unwrap().kind else { panic!() };
        assert_eq!(shape.duck.as_ref().unwrap().under, vec!["voice1".to_string()]);
        let (source, doc) = run(&patcher, &source, CutOp::Param { node: "audio1".into(), key: "gain".into(), value: Value::keys(&[(0.0, 0.2, None), (2.0, 1.0, Some("ease_out"))]) });
        let CutNodeKind::AudioClip { gain, .. } = &doc.node("audio1").unwrap().kind else { panic!() };
        assert!(gain.is_keyed());
        let (_, doc) = run(&patcher, &source, CutOp::AddTitle { start: 2.0, text: "Hi".into() });
        assert!(matches!(doc.node("title1").unwrap().kind, CutNodeKind::Title { .. }));
    }

    #[test]
    fn audio_joins_the_output_mix() {
        let patcher = Patcher::new().unwrap();
        let mut media = video(3);
        media.kind = MediaKind::Audio;
        media.seconds = None;
        let (_, doc) = run(&patcher, EMPTY, CutOp::Add { media, start: 0.0, track: None });
        let CutNodeKind::Output { audio, .. } = &doc.node("out").unwrap().kind else { panic!() };
        assert_eq!(audio[0].node, "audio1");
        assert_eq!(doc.node("audio1").unwrap().span.unwrap().length, 10.0);
    }

    #[test]
    fn a_transition_joins_two_graphics_and_a_video() {
        use super::super::transitions::cut_plan;
        let patcher = Patcher::new().unwrap();
        // Two graphics back to back on one track (as the overlay path writes them).
        let motion = format!("asset(\"ast_{}\", \"arev_{}\")", "a".repeat(26), "5".repeat(64));
        let source = format!("use mod.flow.*\nuse mod.cut.*\n\nlet graphic1 = Graphic{{src: {motion} span: [0.0, 3.0] track: 0 fit: @fit}}\nlet graphic2 = Graphic{{src: {motion} span: [3.0, 3.0] track: 0 fit: @fit}}\nlet main = Stack{{layers: [mod.flow.conn(1, mod.flow.wire(@graphic1, @frame)), mod.flow.conn(2, mod.flow.wire(@graphic2, @frame))]}}\nlet out = CutOutput{{frame: main.out(@frame) audio: []}}\n\nFlow{{\n    kind: @cut\n    label: \"Test\"\n    output: {{width: 1080 height: 1920 fps: 60}}\n    duration: 10.0\n    bpm: 120\n    graphic1, graphic2, main, out\n}}\n");
        let doc = CutDoc::load(&source).unwrap_or_else(|error| panic!("{error:?}\n{source}"));
        assert_eq!(doc.graphics.len(), 2, "{source}");
        let (ga, gb) = (doc.graphics[0].id.clone(), doc.graphics[1].id.clone());
        // On the cut: the graphics are extended (a title's handles) into an overlap.
        let (found_a, found_b, _) = super::super::transitions::find_cut_in(&doc, doc.graphics[0].track, 3.0, 0.1).expect("a cut between graphics");
        assert_eq!((found_a.as_str(), found_b.as_str()), (ga.as_str(), gb.as_str()));
        let handles = |_: &CutNode| (f64::INFINITY, f64::INFINITY);
        let plan = cut_plan(&doc, "101_trans_dissolve", &ga, &gb, 1.0, &handles, &|id| id.to_string()).expect("a plan");
        assert!((plan.length - 1.0).abs() < 1e-9 && (plan.start - 2.5).abs() < 1e-9, "{} {}", plan.start, plan.length);
        let (source, doc) = run(&patcher, &source, plan.op);
        let tr = doc.nodes.iter().find(|node| matches!(node.kind, CutNodeKind::Transition { .. })).expect("a transition");
        assert_eq!(joined(&doc, &ga).len(), 1);
        let (sa, sb) = (doc.graphic(&ga).unwrap().span.unwrap(), doc.graphic(&gb).unwrap().span.unwrap());
        assert!((sa.end() - 3.5).abs() < 1e-9 && (sb.start - 2.5).abs() < 1e-9);
        let mid = tr.span.unwrap().start + 0.5;
        let plan = CutEval::new(&doc).frame_plan(mid);
        let graphics = plan.ops.iter().filter(|op| matches!(op.kind, DrawKind::Graphic { .. })).count();
        assert_eq!(graphics, 2, "both graphics draw at mid-transition");
        assert!(plan.ops.iter().any(|op| matches!(op.kind, DrawKind::Transition { a: Some(_), b: Some(_), .. })));
        // Removing the transition restores the cut.
        let (_, doc) = run(&patcher, &source, CutOp::Delete { node: tr.id.clone() });
        assert_eq!(doc.graphic(&ga).unwrap().span.unwrap().end(), 3.0);
    }
}
