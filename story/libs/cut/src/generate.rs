//! Generative segments: a part of the timeline piped
//! through an AI Hub generation flow and played as a clip.
//!
//! ```splash
//! let dream = Generate{input: demo.out(@frame) span: [4.0, 6.0]
//!     flow: preset("bundled:image-to-video") prompt: "hands become birds" negative: "" seed: 7
//!     params: {frames: 124} fit: @fill
//!     takes: [take(asset("ast_…", "arev_…"), 7, "gk1-…", 5.17, "minimax-h3-q4-24g")] take: 0}
//! ```
//!
//! - **Input**: the picture wired into `input` over the span
//!   (video-to-video), or, with `frame_at: <edit seconds>`, that one frame
//!   of it (image-to-video).
//! - **Takes** are `Video` assets the cut pins, each with the seed it was
//!   made with, the [`generate_key`] it was made under, its own length and
//!   the model that made it. The node plays `takes[take]` stretched over its
//!   span; with no take it plays its input (the host badges it).
//! - **Staleness** is a pure function of the source: a take whose key is
//!   not the key the node would make now (input, prompt, negative, params,
//!   preset, model) or a `seed` no take was made with means "needs
//!   generation". Nothing regenerates by itself.
//! - Outside its span a Generate shows its input, so it can wrap a whole
//!   picture (a selection of the timeline) as well as one clip.
//!
//! Generate nodes live in [`crate::CutDoc::generates`], beside the other
//! nodes (`CutDoc::nodes`), so consumers that match every
//! [`crate::CutNodeKind`] keep building; the frame plan, the dependencies
//! and the graph see them.
//!
//! Also here, all pure: the key, the state, chunking a long span for a
//! model with a maximum length, and the two helper edits the host renders:
//! the input alone at the model's size and rate ([`input_edit_source`]) and
//! the chunks blended at their seams ([`blend_edit_source`]).

use crate::eval::{DrawKind, CutEval, FramePlan, TICKS_PER_SECOND};
use crate::model::*;
use makepad_flow::{Literal, Loc};
use std::collections::HashSet;
use std::fmt::Write;

/// The version every [`generate_key`] starts with; a change to what the
/// key covers bumps it, so old takes read as needing generation.
pub const KEY_VERSION: &str = "gk1";

/// One generated result the cut pins: `take(asset(id, rev), seed, key,
/// seconds, model)`.
#[derive(Clone, Debug, PartialEq)]
pub struct GenTake {
    /// The `Video` asset revision.
    pub video: AssetRef,
    pub seed: u64,
    /// The [`generate_key`] it was made under.
    pub key: Option<String>,
    /// Its own length; the node stretches it over its span. `None`: played
    /// at its own rate from the span's start.
    pub seconds: Option<f64>,
    /// The model that made it (a hub model id).
    pub model: String,
}

/// `Generate{…}`.
#[derive(Clone, Debug, PartialEq)]
pub struct GenerateNode {
    pub id: String,
    pub span: Span,
    pub track: u32,
    /// 1-based, like the other nodes.
    pub loc: Loc,
    pub input: Option<Wire>,
    /// Image-to-video: the cut second whose input frame starts the clip.
    pub frame_at: Option<f64>,
    /// The FLOW preset key (`bundled:image-to-video`).
    pub preset: String,
    pub prompt: String,
    pub negative: String,
    /// The seed the next take is made with (further takes count up).
    pub seed: u64,
    /// Generator parameters, sorted by name.
    pub params: Vec<(String, Literal)>,
    pub fit: Fit,
    pub takes: Vec<GenTake>,
    /// The index of the take the clip plays.
    pub take: usize,
}

impl GenerateNode {
    /// The take the clip plays.
    pub fn chosen(&self) -> Option<&GenTake> {
        self.takes.get(self.take)
    }

    pub fn active(&self, t: f64) -> bool {
        t.is_finite() && self.span.contains(t)
    }

    pub fn param(&self, name: &str) -> Option<&Literal> {
        self.params.iter().find(|(key, _)| key == name).map(|(_, value)| value)
    }

    /// A numeric parameter.
    pub fn number(&self, name: &str) -> Option<f64> {
        match self.param(name)? {
            Literal::Num(value) => Some(*value),
            _ => None,
        }
    }

    /// Take seconds per edit second: the take is stretched over the span.
    pub fn rate(&self, take: &GenTake) -> f64 {
        match take.seconds {
            Some(seconds) if seconds > 0.0 && self.span.length > 0.0 => seconds / self.span.length,
            _ => 1.0,
        }
    }

    /// The take's time shown at edit time `t`.
    pub fn take_time(&self, take: &GenTake, t: f64) -> f64 {
        ((t - self.span.start) * self.rate(take)).max(0.0)
    }
}

/// Why a Generate segment needs generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Staleness {
    /// The picture it reads changed (sources, their revisions or times,
    /// effects, their keys…), or the frame it starts from.
    Input,
    /// The preset, prompt, negative prompt or params changed.
    Settings,
    /// `seed` is one no take was made with.
    Seed,
    /// The chosen take has no key (written by hand).
    Unkeyed,
}

/// What a Generate segment is now (the render bar's states).
#[derive(Clone, Debug, PartialEq)]
pub enum GenState {
    /// No take yet: plays its input.
    Missing,
    /// The chosen take is what the node would make now.
    Generated,
    /// The chosen take plays, but a new one is wanted; why.
    Stale(Vec<Staleness>),
}

impl GenState {
    pub fn needs_generation(&self) -> bool {
        !matches!(self, GenState::Generated)
    }

    pub fn label(&self) -> &'static str {
        match self {
            GenState::Missing => "needs generation",
            GenState::Generated => "generated",
            GenState::Stale(_) => "changed · needs generation",
        }
    }
}

/// 64-bit FNV-1a, stable across builds (keys are written into sources).
#[derive(Clone, Copy)]
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    fn text(&mut self, text: &str) {
        for byte in text.bytes() {
            self.0 ^= byte as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
        // A separator, so ("ab", "c") and ("a", "bc") differ.
        self.0 ^= 0xff;
        self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

/// The times the input is sampled at for its key: every output frame of
/// the span, or the one frame of an image-to-video.
pub fn key_times(gen: &GenerateNode, fps: f64) -> Vec<f64> {
    if let Some(at) = gen.frame_at {
        return vec![at];
    }
    let fps = if fps > 0.0 { fps } else { 30.0 };
    let frames = ((gen.span.length * fps) - 1e-6).ceil().max(1.0) as u64;
    (0..frames).map(|n| gen.span.start + n as f64 / fps).collect()
}

/// The input's part of the key: its evaluated sub-graph over the span, as
/// the frame plans of its input node (exact source revisions and times,
/// effect params and keys, titles, transitions, nested cuts), without node
/// names or pixel rectangles that follow from source sizes.
pub fn input_digest(eval: &CutEval<'_>, gen: &GenerateNode) -> u64 {
    let mut hash = Fnv::new();
    let Some(input) = &gen.input else {
        hash.text("no input");
        return hash.0;
    };
    let mut text = String::new();
    for t in key_times(gen, eval.doc().output.fps) {
        text.clear();
        canonical_plan(&eval.node_plan(&input.node, t), &mut text);
        hash.text(&text);
    }
    hash.0
}

/// The settings' part of the key: preset, model, prompt, negative, seed,
/// params and the image-to-video frame time.
pub fn settings_digest(gen: &GenerateNode, seed: u64, model: &str) -> u64 {
    let mut hash = Fnv::new();
    hash.text(KEY_VERSION);
    hash.text(&gen.preset);
    hash.text(model);
    hash.text(&gen.prompt);
    hash.text(&gen.negative);
    hash.text(&seed.to_string());
    hash.text(&gen.frame_at.map_or("v2v".to_string(), |at| format!("i2v@{at:?}")));
    let mut text = String::new();
    for (key, value) in &gen.params {
        text.clear();
        text.push_str(key);
        text.push('=');
        canonical_literal(value, &mut text);
        hash.text(&text);
    }
    hash.0
}

/// The key of a take of `gen` made with `seed` by `model` (its cache
/// key): `gk1-<input><settings>`, two 64-bit halves, so a changed
/// key tells whether the input or the settings moved.
pub fn generate_key(eval: &CutEval<'_>, gen: &GenerateNode, seed: u64, model: &str) -> String {
    format!("{KEY_VERSION}-{:016x}{:016x}", input_digest(eval, gen), settings_digest(gen, seed, model))
}

/// The two halves of a key.
fn key_parts(key: &str) -> Option<(&str, &str)> {
    let hex = key.strip_prefix(KEY_VERSION)?.strip_prefix('-')?;
    (hex.len() == 32).then(|| hex.split_at(16))
}

/// What `gen` is now: its chosen take against the key it would be made
/// under now (with that take's seed and model).
pub fn generate_state(eval: &CutEval<'_>, gen: &GenerateNode) -> GenState {
    let Some(take) = gen.chosen() else { return GenState::Missing };
    let mut why = Vec::new();
    match take.key.as_deref().and_then(key_parts) {
        None => why.push(Staleness::Unkeyed),
        Some((input, settings)) => {
            if input != format!("{:016x}", input_digest(eval, gen)) {
                why.push(Staleness::Input);
            }
            if settings != format!("{:016x}", settings_digest(gen, take.seed, &take.model)) {
                why.push(Staleness::Settings);
            }
        }
    }
    if !gen.takes.iter().any(|take| take.seed == gen.seed) {
        why.push(Staleness::Seed);
    }
    if why.is_empty() { GenState::Generated } else { GenState::Stale(why) }
}

/// The seeds of `count` new takes: from the node's `seed` up, skipping
/// seeds a take already has.
pub fn next_seeds(gen: &GenerateNode, count: usize) -> Vec<u64> {
    let used: HashSet<u64> = gen.takes.iter().map(|take| take.seed).collect();
    let mut seeds = Vec::with_capacity(count);
    let mut seed = gen.seed;
    while seeds.len() < count {
        if !used.contains(&seed) {
            seeds.push(seed);
        }
        seed = seed.wrapping_add(1);
    }
    seeds
}

fn canonical_plan(plan: &FramePlan, out: &mut String) {
    let _ = write!(out, "r{:?}|", plan.result);
    for op in &plan.ops {
        let _ = write!(out, "{}x{}:", op.size.0, op.size.1);
        match &op.kind {
            DrawKind::Source { media, still, source_ticks, fit, opacity, .. } => {
                let media = match media {
                    MediaRef::MissingOptional(path) => format!("missing:{path}"),
                    MediaRef::Asset(asset) => format!("{}:{}", asset.id, asset.rev),
                    MediaRef::Run(run) => format!("{}:{}:{}:{}", run.id, run.rev, run.render_id, run.render),
                };
                let _ = write!(out, "S{media},{still},{source_ticks:?},{fit:?},{opacity:?}");
            }
            DrawKind::Text { text, position, font_size, style, local_time, karaoke } => {
                let _ = write!(out, "T{text:?},{position:?},{font_size:?},{style:?},{local_time:?}");
                if let Some(karaoke) = karaoke {
                    let _ = write!(out, ",{:?},{:?},{:?}", karaoke.words, karaoke.active, karaoke.highlight);
                }
            }
            DrawKind::Effect { effect, input, local_time, beat, params } => {
                let _ = write!(out, "E{},{input:?},{local_time:?},{beat:?},{params:?}", effect_text(effect));
            }
            DrawKind::Transition { effect, a, b, mix, local_time, beat, params, effect_asset } => {
                let _ = write!(out, "X{effect:?},{a:?},{b:?},{mix:?},{local_time:?},{beat:?},{params:?}");
                // Written only when set, so keys of preset transitions are unchanged.
                if let Some(asset) = effect_asset {
                    let _ = write!(out, ",{}:{}", asset.id, asset.rev);
                }
            }
            DrawKind::Stack { layers } => {
                let _ = write!(out, "K{layers:?}");
            }
            DrawKind::Reframe { input, crop, dst_rect } => {
                let _ = write!(out, "R{input},{:?},{:?}", crop.to_array(), dst_rect.to_array());
            }
            // A Motion graphic (lane M1): everything its pixels depend on.
            DrawKind::Graphic { src, params, local_time, offset, bpm, fit, opacity } => {
                let _ = write!(out, "G{src:?},{params:?},{local_time:?},{offset:?},{bpm:?},{fit:?},{opacity:?}");
            }
            // The app's picture is the host's, not the plan's.
            DrawKind::App { app, fit, rect } => {
                let _ = write!(out, "A{app},{fit:?},{rect:?}");
            }
            // The camera's picture is the host's, not the plan's.
            // The code picture is the host's, not the plan's.
            DrawKind::Code { label, fit, rect } => {
                let _ = write!(out, "K{label},{fit:?},{rect:?}");
            }
            DrawKind::Camera { spec } => {
                let _ = write!(out, "C{spec:?}");
            }
        }
        out.push(';');
    }
    if !plan.unresolved.is_empty() {
        let _ = write!(out, "u{}", plan.unresolved.len());
    }
}

fn effect_text(effect: &EffectRef) -> String {
    match effect {
        EffectRef::Preset(name) => format!("p:{name}"),
        EffectRef::Asset(asset) => format!("a:{}:{}", asset.id, asset.rev),
    }
}

fn canonical_literal(value: &Literal, out: &mut String) {
    match value {
        Literal::Null => out.push_str("nil"),
        Literal::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
        Literal::Num(value) => {
            let _ = write!(out, "{value:?}");
        }
        Literal::Str(text) => {
            let _ = write!(out, "{text:?}");
        }
        Literal::Id(id) => {
            let _ = write!(out, "@{id}");
        }
        Literal::Arr(items) => {
            out.push('[');
            for item in items {
                canonical_literal(item, out);
                out.push(',');
            }
            out.push(']');
        }
        Literal::Obj(fields) => {
            let mut fields = fields.iter().collect::<Vec<_>>();
            fields.sort_by(|a, b| a.0.cmp(&b.0));
            out.push('{');
            for (key, value) in fields {
                out.push_str(key);
                out.push(':');
                canonical_literal(value, out);
                out.push(',');
            }
            out.push('}');
        }
    }
}

/// One piece of a long span, in seconds from the span's start.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Chunk {
    pub start: f64,
    pub length: f64,
}

impl Chunk {
    pub fn end(&self) -> f64 {
        self.start + self.length
    }
}

/// Cut `length` seconds into chunks a model with a maximum of `max`
/// seconds per job can make (a long span is made in chunks). Consecutive chunks
/// overlap by at least `overlap` (clamped to half a chunk) so the seams can
/// be blended; all chunks have one length, spread evenly. A span no longer
/// than `max` is one chunk.
pub fn chunks(length: f64, max: f64, overlap: f64) -> Vec<Chunk> {
    if !(length > 0.0 && length.is_finite()) {
        return Vec::new();
    }
    if !(max > 0.0 && max.is_finite()) || length <= max + 1e-9 {
        return vec![Chunk { start: 0.0, length }];
    }
    let overlap = overlap.clamp(0.0, max * 0.5);
    let step = max - overlap;
    // Enough chunks that each advances at most `step`.
    let count = ((length - max) / step - 1e-9).ceil().max(0.0) as usize + 1;
    let advance = (length - max) / (count - 1).max(1) as f64;
    (0..count).map(|index| Chunk { start: index as f64 * advance, length: max }).collect()
}

/// Cut `length` seconds into equal chunks of at most `max` seconds that
/// follow each other without overlap: for a model that continues each
/// chunk from the last frame of the one before (image-to-video).
pub fn chunks_in_sequence(length: f64, max: f64) -> Vec<Chunk> {
    if !(length > 0.0 && length.is_finite()) {
        return Vec::new();
    }
    let count = if max > 0.0 && max.is_finite() { (length / max - 1e-9).ceil().max(1.0) as usize } else { 1 };
    let each = length / count as f64;
    (0..count).map(|index| Chunk { start: index as f64 * each, length: each }).collect()
}

/// The overlaps where consecutive chunks are blended: `(start, length)` in
/// seconds from the span's start.
pub fn seams(chunks: &[Chunk]) -> Vec<(f64, f64)> {
    chunks.windows(2).map(|pair| (pair[1].start, pair[0].end() - pair[1].start)).filter(|(_, length)| *length > 0.0).collect()
}

/// A decimal Splash number (no exponent).
fn number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{value:.1}")
    } else {
        let text = format!("{value:.9}");
        text.trim_end_matches('0').to_string()
    }
}

fn string(text: &str) -> String {
    let mut out = String::from("\"");
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// A take as the source writes it.
pub fn take_text(take: &GenTake) -> String {
    format!(
        "take(asset({}, {}), {}, {}, {}, {})",
        string(&take.video.id), string(&take.video.rev), take.seed,
        take.key.as_deref().map_or("nil".to_string(), string),
        take.seconds.map_or("nil".to_string(), number),
        string(&take.model),
    )
}

/// `[take(…), …]`.
pub fn takes_text(takes: &[GenTake]) -> String {
    format!("[{}]", takes.iter().map(take_text).collect::<Vec<_>>().join(", "))
}

/// The cut that shows only `gen`'s input: the nodes it reads (every node
/// upstream of its input, as written), a new output of the input's
/// picture, and the header at `width`×`height` and `fps`. Same duration
/// and times as the cut, so the host renders or grabs the span's frames
/// on locked time. Everything else of the cut stays in the text but out
/// of the flow.
pub fn input_edit_source(doc: &CutDoc, gen: &GenerateNode, width: u32, height: u32, fps: f64) -> Result<String, String> {
    let input = gen.input.as_ref().ok_or_else(|| format!("`{}` has no input", gen.id))?;
    let mut members: HashSet<&str> = HashSet::from([input.node.as_str()]);
    let mut stack = vec![input.node.as_str()];
    while let Some(node) = stack.pop() {
        for edge in doc.graph.edges.iter().filter(|edge| edge.to_node == node) {
            if members.insert(edge.from_node.as_str()) {
                stack.push(edge.from_node.as_str());
            }
        }
    }
    let ordered = doc.graph.nodes.iter().map(|node| node.id.as_str()).filter(|id| members.contains(id)).collect::<Vec<_>>();
    let (flow_start, flow_end) = crate::load::final_flow(&doc.source).ok_or("a cut ends with one `Flow{...}`")?;
    let mut out_id = "generate_input".to_string();
    while doc.source.contains(&out_id) {
        out_id.push('_');
    }
    let mut source = String::with_capacity(doc.source.len() + 256);
    source.push_str(&doc.source[..flow_start]);
    let _ = writeln!(source, "let {out_id} = CutOutput{{frame: {}.out(@{})}}\n", input.node, input.port);
    let _ = write!(
        source,
        "Flow{{\n    kind: @cut\n    label: {}\n    output: {{width: {width} height: {height} fps: {}}}\n    duration: {}\n    bpm: {}\n    {}, {out_id}\n}}",
        string(&format!("{} · input of {}", doc.label, gen.id)), number(fps), number(doc.duration), number(doc.bpm), ordered.join(", "),
    );
    source.push_str(&doc.source[flow_end + 1..]);
    Ok(source)
}

/// One generated chunk for [`blend_edit_source`].
#[derive(Clone, Debug, PartialEq)]
pub struct BlendPart {
    pub video: AssetRef,
    /// Where the chunk plays, seconds from the span's start.
    pub chunk: Chunk,
    /// The chunk video's own length; it is stretched over the chunk.
    pub seconds: f64,
}

/// The cut that plays `parts` one after another over `length` seconds,
/// each stretched over its chunk, dissolving across every overlap
/// (blended at the seams). Rendered, it is the take.
pub fn blend_edit_source(parts: &[BlendPart], length: f64, width: u32, height: u32, fps: f64, fit: Fit) -> String {
    let fit = match fit {
        Fit::Fill => "fill",
        Fit::Fit => "fit",
        Fit::None => "none",
    };
    let mut source = String::from("use mod.flow.*\nuse mod.cut.*\n\n");
    let mut members = Vec::new();
    let mut chain: Option<String> = None;
    for (index, part) in parts.iter().enumerate() {
        let clip = format!("chunk{index}");
        let speed = if part.seconds > 0.0 && part.chunk.length > 0.0 { part.seconds / part.chunk.length } else { 1.0 };
        let _ = writeln!(
            source,
            "let {clip} = Clip{{src: asset({}, {}) in: 0.0 span: [{}, {}] speed: {} fit: @{fit} audio: false}}",
            string(&part.video.id), string(&part.video.rev), number(part.chunk.start), number(part.chunk.length), number(speed),
        );
        members.push(clip.clone());
        chain = Some(match chain {
            None => clip,
            Some(previous) => {
                let seam = format!("seam{index}");
                let prev_end = parts[index - 1].chunk.end();
                let overlap = (prev_end - part.chunk.start).max(1.0 / fps.max(1.0));
                let _ = writeln!(
                    source,
                    "let {seam} = Transition{{a: {previous}.out(@frame) b: {clip}.out(@frame) effect: \"\" span: [{}, {}]}}",
                    number(part.chunk.start), number(overlap),
                );
                members.push(seam.clone());
                seam
            }
        });
    }
    if let Some(chain) = &chain {
        let _ = writeln!(source, "let out = CutOutput{{frame: {chain}.out(@frame)}}");
    } else {
        source.push_str("let out = CutOutput{}\n");
    }
    members.push("out".into());
    let _ = write!(
        source,
        "\nFlow{{\n    kind: @cut\n    label: \"generated take\"\n    output: {{width: {width} height: {height} fps: {}}}\n    duration: {}\n    {}\n}}\n",
        number(fps), number(length), members.join(", "),
    );
    source
}

/// Every take a document's Generate nodes play, for a renderer's media
/// table (the chosen ones; [`crate::dependencies`] lists all).
pub fn generate_media(doc: &CutDoc) -> Vec<MediaRef> {
    let mut media = Vec::new();
    for gen in &doc.generates {
        if let Some(take) = gen.chosen() {
            let take = MediaRef::Asset(take.video.clone());
            if !media.contains(&take) {
                media.push(take);
            }
        }
    }
    media
}

/// The source ticks of a take's time.
pub(crate) fn ticks(time: f64) -> Option<i64> {
    time.is_finite().then(|| (time * TICKS_PER_SECOND).round() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_spans_are_one_chunk_and_long_ones_overlap_evenly() {
        assert_eq!(chunks(4.0, 5.0, 0.5), vec![Chunk { start: 0.0, length: 4.0 }]);
        assert_eq!(chunks(5.0, 5.0, 0.5).len(), 1);
        let parts = chunks(12.0, 5.0, 0.5);
        assert_eq!(parts.len(), 3);
        assert!(parts.iter().all(|chunk| chunk.length == 5.0));
        assert_eq!(parts[0].start, 0.0);
        assert!((parts[2].end() - 12.0).abs() < 1e-9);
        for (_, overlap) in seams(&parts) {
            assert!(overlap >= 0.5 - 1e-9, "{overlap}");
        }
        // Exactly two steps' worth: no extra chunk.
        assert_eq!(chunks(9.5, 5.0, 0.5).len(), 2);
        assert!(chunks(0.0, 5.0, 0.5).is_empty());
        // An overlap past half a chunk is clamped.
        assert!(chunks(20.0, 4.0, 10.0).len() >= 9);
    }

    #[test]
    fn chunks_in_sequence_follow_each_other() {
        let parts = chunks_in_sequence(8.0, 3.0);
        assert_eq!(parts.len(), 3);
        assert!(seams(&parts).is_empty());
        assert!((parts[2].end() - 8.0).abs() < 1e-9);
        assert_eq!(chunks_in_sequence(2.0, 3.0), vec![Chunk { start: 0.0, length: 2.0 }]);
    }

    #[test]
    fn keys_split_into_input_and_settings() {
        assert_eq!(key_parts("gk1-0123456789abcdef0123456789abcdef"), Some(("0123456789abcdef", "0123456789abcdef")));
        assert_eq!(key_parts("gk2-0123456789abcdef0123456789abcdef"), None);
        assert_eq!(key_parts("gk1-short"), None);
    }

    #[test]
    fn literals_are_canonical() {
        let mut a = String::new();
        canonical_literal(&Literal::Obj(vec![("b".into(), Literal::Num(1.0)), ("a".into(), Literal::Str("x".into()))]), &mut a);
        let mut b = String::new();
        canonical_literal(&Literal::Obj(vec![("a".into(), Literal::Str("x".into())), ("b".into(), Literal::Num(1.0))]), &mut b);
        assert_eq!(a, b);
    }

    #[test]
    fn numbers_are_plain_decimals() {
        assert_eq!(number(24.0), "24.0");
        assert_eq!(number(0.5), "0.5");
        assert_eq!(number(1.0 / 3.0), "0.333333333");
    }
}
