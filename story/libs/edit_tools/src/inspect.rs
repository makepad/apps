//! `inspect`: everything that would make a cut fail to render, or render
//! something the author did not mean, found without drawing it. Every issue
//! names its place (line, column, node, time range) and, where one exists,
//! the fix, because agents write these documents and act on the report.
//!
//! Errors: the cut does not load; media or revisions that cannot be
//! resolved; unknown or non-seekable effects; nested-edit problems; more
//! clips at one time than the decoders allow; a title anchored outside the
//! frame. Warnings: text wider than the frame, text in a 9:16 platform's
//! safe-zone bands, empty or overlapping spans, stretches with no picture,
//! unresolved run markers, silent voice lines, keys past their span.

use crate::input::{DocKind, Document};
use makepad_story_edit_render::engine::tree_nodes;
use makepad_story_edit_render::{MediaKind, MediaResolver};
use makepad_story_cut::reframe::SafeZone;
use makepad_story_cut::subtitle::CAPTION_CHAR_EM;
use makepad_story_cut::*;
use makepad_content_packs::effects as packs;
use makepad_strict_json::{obj, s, Value};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl Severity {
    pub fn id(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
        }
    }
}

/// One finding.
#[derive(Clone, Debug, PartialEq)]
pub struct Issue {
    pub severity: Severity,
    /// A stable id, `area.what` (`media.missing`, `text.overflow`, …).
    pub code: String,
    /// 1-based place in the document; 0 when it has none.
    pub line: u32,
    pub col: u32,
    /// The node, as a path through compound clips (`outer/inner/title`).
    pub node: Option<String>,
    /// The cut time range it concerns, in seconds.
    pub at: Option<(f64, f64)>,
    pub message: String,
    /// How to fix it, when there is a known fix.
    pub hint: Option<String>,
}

impl Issue {
    pub(crate) fn new(severity: Severity, code: &str, message: impl Into<String>) -> Self {
        Self { severity, code: code.into(), line: 0, col: 0, node: None, at: None, message: message.into(), hint: None }
    }

    fn node(mut self, node: &CutNode, path: &str) -> Self {
        self.line = node.loc.line;
        self.col = node.loc.col;
        self.node = Some(path.to_string());
        self
    }

    pub(crate) fn at(mut self, t0: f64, t1: f64) -> Self {
        self.at = Some((t0, t1));
        self
    }

    pub(crate) fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn to_json(&self) -> Value {
        let mut pairs = vec![("severity", s(self.severity.id())), ("code", s(self.code.clone()))];
        if self.line > 0 {
            pairs.push(("line", Value::Int(self.line as i64)));
            pairs.push(("col", Value::Int(self.col as i64)));
        }
        if let Some(node) = &self.node {
            pairs.push(("node", s(node.clone())));
        }
        if let Some((t0, t1)) = self.at {
            pairs.push(("at", Value::Arr(vec![num(t0), num(t1)])));
        }
        pairs.push(("message", s(self.message.clone())));
        if let Some(hint) = &self.hint {
            pairs.push(("hint", s(hint.clone())));
        }
        obj(pairs)
    }
}

/// A number for JSON, to 0.1 ms / 4 decimals: reports stay readable.
pub fn num(value: f64) -> Value {
    Value::F64((value * 10_000.0).round() / 10_000.0)
}

#[derive(Clone, Debug)]
pub struct InspectOptions {
    /// The platform whose safe zone a portrait (taller than wide) output is
    /// checked against; `None` checks `@reels`, the strictest bands.
    pub safe: Option<SafeZone>,
    /// Times per second the time-based checks sample (besides every span
    /// start and end).
    pub sample_fps: f64,
    /// Motion text's minimum em height, in pixels at the document's own
    /// resolution, after fit and transforms. Zero disables the size check.
    pub min_text_px: f64,
    /// The marks of the app runs the cut plays (a run's `run.json`).
    pub marks: HashMap<RunRef, HashMap<String, f64>>,
}

impl Default for InspectOptions {
    fn default() -> Self {
        Self { safe: None, sample_fps: 10.0, min_text_px: makepad_motion::inspect::DEFAULT_MIN_TEXT_PX, marks: HashMap::new() }
    }
}

/// What `inspect` found.
#[derive(Clone, Debug)]
pub struct InspectReport {
    pub file: String,
    pub kind: DocKind,
    /// `label`, `WxH`, fps, duration, node count, once the cut loads.
    pub summary: Option<(String, (u32, u32), f64, f64, usize)>,
    /// Errors first, then warnings, then information; each by place.
    pub issues: Vec<Issue>,
}

impl InspectReport {
    pub fn count(&self, severity: Severity) -> usize {
        self.issues.iter().filter(|issue| issue.severity == severity).count()
    }

    /// No errors (warnings allowed).
    pub fn ok(&self) -> bool {
        self.count(Severity::Error) == 0
    }

    pub fn to_json(&self) -> Value {
        let mut pairs = vec![
            ("file", s(self.file.clone())),
            ("kind", s(self.kind.id())),
            ("ok", Value::Bool(self.ok())),
            ("errors", Value::Int(self.count(Severity::Error) as i64)),
            ("warnings", Value::Int(self.count(Severity::Warning) as i64)),
        ];
        if let Some((label, (w, h), fps, duration, nodes)) = &self.summary {
            pairs.push((
                "cut",
                obj(vec![
                    ("label", s(label.clone())),
                    ("width", Value::Int(*w as i64)),
                    ("height", Value::Int(*h as i64)),
                    ("fps", num(*fps)),
                    ("duration", num(*duration)),
                    ("nodes", Value::Int(*nodes as i64)),
                ]),
            ));
        }
        pairs.push(("issues", Value::Arr(self.issues.iter().map(Issue::to_json).collect())));
        obj(pairs)
    }

    /// `file:line:col: error[code] node: message` lines, hints under them,
    /// and a closing count.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for issue in &self.issues {
            let place = if issue.line > 0 { format!("{}:{}:{}", self.file, issue.line, issue.col) } else { self.file.clone() };
            let node = issue.node.as_ref().map(|node| format!(" `{node}`")).unwrap_or_default();
            let at = issue.at.map(|(t0, t1)| if (t1 - t0).abs() < 1e-9 { format!(" at {t0:.2}s") } else { format!(" at {t0:.2}–{t1:.2}s") }).unwrap_or_default();
            out.push_str(&format!("{place}: {}[{}]{node}{at}: {}\n", issue.severity.id(), issue.code, issue.message));
            if let Some(hint) = &issue.hint {
                out.push_str(&format!("    fix: {hint}\n"));
            }
        }
        let what = match &self.summary {
            Some((label, (w, h), fps, duration, nodes)) => format!("`{label}` {w}x{h} at {fps} fps, {duration:.2} s, {nodes} nodes: "),
            None => String::new(),
        };
        out.push_str(&format!(
            "{}: {what}{} errors, {} warnings, {} notes\n",
            self.file, self.count(Severity::Error), self.count(Severity::Warning), self.count(Severity::Info)
        ));
        out
    }
}

fn diag_code(kind: DiagnosticKind) -> (&'static str, &'static str) {
    match kind {
        DiagnosticKind::Eval => ("load.eval", "fix the Splash at this place: a typo, an unclosed `{`/`[`, or a field the node does not declare"),
        DiagnosticKind::Header => (
            "load.header",
            "end the cut with `Flow{kind: @cut label: \"…\" output: {width: 1080 height: 1920 fps: 60} duration: 17.0 bpm: 120 <every node id>}`",
        ),
        DiagnosticKind::MissingRef => ("load.missing_ref", "give the node its media: `src: asset(\"<asset id>\", \"<revision>\")`"),
        DiagnosticKind::InvalidRef => ("load.invalid_ref", "media is only `asset(id, rev)`, `run(id, rev, render_id, render_rev)` or, for a Clip, `cut(id, rev)`"),
        DiagnosticKind::SpanOutOfRange => ("load.span", "`span: [start, length]` in seconds, with start >= 0, length > 0 and start + length <= the cut's duration"),
        DiagnosticKind::KeysNotSorted => ("load.keys_order", "write keys in increasing time: `keys([0.0, a], [1.0, b])`"),
        DiagnosticKind::UnknownEase => ("load.ease", "eases are @linear, @ease_in, @ease_out, @ease_in_out and @hold"),
        DiagnosticKind::InvalidParam => ("load.param", "see the message for the value's shape"),
        DiagnosticKind::TransitionInputs => ("load.transition", "wire both sides and time it: `Transition{a: x.out(@frame) b: y.out(@frame) effect: \"vjfx/101_trans_dissolve\" span: [s, l]}`"),
        DiagnosticKind::TooManyClips => ("clips.limit", "stagger the clips' spans, or render part of the cut to a video and use that"),
        DiagnosticKind::Cycle => ("load.cycle", "a node's input must not lead back to itself; a cut must not nest itself"),
        DiagnosticKind::NestDepth => ("nest.depth", "flatten one level: render the innermost edit to a video and use that"),
        DiagnosticKind::Graph => ("load.graph", "only mod.cut nodes, each input wired from a node's `out(@frame)` or `out(@audio)`"),
    }
}

fn from_diagnostic(diag: &Diagnostic) -> Issue {
    let (code, hint) = diag_code(diag.kind);
    Issue {
        severity: Severity::Error,
        code: code.into(),
        line: diag.line,
        col: diag.col,
        node: diag.node.clone(),
        at: None,
        message: diag.message.clone(),
        hint: Some(hint.into()),
    }
}

/// Check `doc`. With a `resolver`, every reference is resolved (media,
/// nested cuts, library effects, takes, transcripts); that reads files or
/// the store, so call it on a worker. Without one those checks are skipped
/// and one note says so.
pub fn inspect(doc: &Document, mut resolver: Option<&mut dyn MediaResolver>, options: &InspectOptions) -> InspectReport {
    if doc.kind == DocKind::Motion {
        return crate::motion::inspect_motion(&doc.source, &doc.name, resolver, options);
    }
    let mut report = InspectReport { file: doc.name.clone(), kind: doc.kind, summary: None, issues: Vec::new() };
    let edit = match doc.load() {
        Ok(edit) => edit,
        Err(diags) => {
            report.issues.extend(diags.iter().map(from_diagnostic));
            finish(&mut report);
            return report;
        }
    };
    report.summary = Some((edit.label.clone(), (edit.output.width, edit.output.height), edit.output.fps, edit.duration, edit.nodes.len() + edit.graphics.len() + edit.generates.len()));
    let mut issues = Vec::new();

    // The tree of nested cuts, when they can be read.
    let nested = !edit.nested_edits().is_empty();
    let tree = match (&mut resolver, nested) {
        (Some(resolver), true) => match CutTree::from_doc(edit.clone(), None, &mut |asset| resolver.cut_source(asset)) {
            Ok(tree) => tree,
            Err(diags) => {
                issues.extend(diags.iter().map(from_diagnostic));
                CutTree { root: edit.clone(), edits: HashMap::new() }
            }
        },
        _ => CutTree { root: edit.clone(), edits: HashMap::new() },
    };
    if nested && resolver.is_none() {
        issues.push(
            Issue::new(Severity::Info, "nest.unchecked", "nested cuts were not loaded (no media table), so what they contain, black stretches and the clip limit are not checked")
                .hint("pass the nested cuts' sources: `--media edit:<id>=<file.splash>` or a media.txt beside the cut"),
        );
    }
    let nodes: Vec<(String, CutNode)> = tree_nodes(&tree).into_iter().map(|(path, node)| (path, node.clone())).collect();

    // Takes and transcripts, so voice lines and captions are real.
    let mut eval_takes: Vec<(AssetRef, TakeInfo)> = Vec::new();
    let mut eval_transcripts: Vec<(AssetRef, Vec<SpokenLine>)> = Vec::new();
    match &mut resolver {
        Some(resolver) => check_media(&nodes, &mut **resolver, &mut issues, &mut eval_takes, &mut eval_transcripts),
        None => issues.push(
            Issue::new(Severity::Info, "media.unchecked", "media references were not resolved (no media table), so missing files and revisions are not checked")
                .hint("pass `--media <asset id>=<file>` for each reference, or put the same lines in a media.txt beside the cut"),
        ),
    }
    check_effects(&nodes, &mut resolver, &mut issues);
    check_spans(&edit, &nodes, &mut issues);
    check_text(&edit, &nodes, options, &mut issues);
    check_voice(&nodes, &mut issues);
    check_graphics(&edit, &mut resolver, &mut issues);

    let mut eval = tree.eval();
    for (run, marks) in &options.marks {
        eval.set_marks(run.clone(), marks.clone());
    }
    for (take, info) in eval_takes {
        eval.set_take(take, info);
    }
    for (transcript, lines) in eval_transcripts {
        eval.set_transcript(transcript, lines);
    }
    check_markers(&edit, options, &mut issues);
    // Without the nested cuts the picture of a compound clip is unknown.
    if !nested || !tree.edits.is_empty() {
        check_over_time(&edit, &nodes, &eval, options, &mut issues);
    }
    check_captions(&edit, &nodes, &eval, options, &mut issues);

    report.issues = issues;
    finish(&mut report);
    report
}

/// [`inspect`] of Splash text that is not a file (an agent's draft):
/// `name` is what the report calls it.
pub fn inspect_text(source: &str, name: &str, resolver: Option<&mut dyn MediaResolver>, options: &InspectOptions) -> InspectReport {
    match Document::from_source(source, name) {
        Ok(doc) => inspect(&doc, resolver, options),
        Err(error) => InspectReport {
            file: name.to_string(),
            kind: crate::input::detect_kind(source),
            summary: None,
            issues: vec![Issue::new(Severity::Error, "doc.unsupported", error)],
        },
    }
}

fn finish(report: &mut InspectReport) {
    report.issues.sort_by(|a, b| {
        (a.severity, a.line, a.col, a.at.map(|at| at.0).unwrap_or(0.0).to_bits())
            .cmp(&(b.severity, b.line, b.col, b.at.map(|at| at.0).unwrap_or(0.0).to_bits()))
    });
    report.issues.dedup();
}

fn media_name(media: &MediaRef) -> String {
    match media {
        MediaRef::MissingOptional(path) => format!("media not present: {path} (plays silent)"),
        MediaRef::Asset(asset) => format!("asset(\"{}\", \"{}\")", asset.id, asset.rev),
        MediaRef::Run(run) => format!("the render {} of run {}", run.render_id, run.id),
    }
}

fn media_key(media: &MediaRef) -> &str {
    match media {
        MediaRef::Asset(asset) => &asset.id,
        MediaRef::Run(run) => &run.render_id,
        MediaRef::MissingOptional(path) => path,
    }
}

fn check_media(
    nodes: &[(String, CutNode)],
    resolver: &mut dyn MediaResolver,
    issues: &mut Vec<Issue>,
    takes: &mut Vec<(AssetRef, TakeInfo)>,
    transcripts: &mut Vec<(AssetRef, Vec<SpokenLine>)>,
) {
    let mut resolved: HashMap<MediaRef, Result<MediaKind, String>> = HashMap::new();
    let missing_hint = |media: &MediaRef| {
        format!("add `{}=<file>` to the media table (media.txt or --media), or fix the id and revision to one the library has", media_key(media))
    };
    for (path, node) in nodes {
        let (src, want) = match &node.kind {
            CutNodeKind::Clip { nested: true, .. } => continue,
            CutNodeKind::Clip { src, .. } => (src, "Clip"),
            CutNodeKind::Still { src, .. } => (src, "Still"),
            CutNodeKind::AudioClip { src, .. } => (src, "AudioClip"),
            CutNodeKind::VoiceOver { lines, .. } => {
                for (index, take) in lines.iter().enumerate().filter_map(|(i, line)| line.take.as_ref().map(|take| (i, take))) {
                    let media = MediaRef::Asset(take.clone());
                    if let Err(error) = resolved.entry(media.clone()).or_insert_with(|| resolver.resolve(&media).map(|r| r.kind)) {
                        issues.push(Issue::new(Severity::Error, "media.missing", format!("the take of line {index} cannot be resolved: {error}")).node(node, path).hint(missing_hint(&media)));
                    }
                    match resolver.take_info(take) {
                        Ok(Some(info)) => takes.push((take.clone(), info)),
                        Ok(None) => {}
                        Err(error) => issues.push(Issue::new(Severity::Warning, "voice.timings", format!("the word times of line {index}'s take cannot be read: {error}")).node(node, path)),
                    }
                }
                continue;
            }
            CutNodeKind::Subtitles { transcript: Some(transcript), .. } => {
                match resolver.transcript(transcript) {
                    Ok(Some(lines)) => transcripts.push((transcript.clone(), lines)),
                    Ok(None) => issues.push(
                        Issue::new(Severity::Warning, "media.missing", format!("the transcript asset(\"{}\", \"{}\") has no word times here, so its captions are empty", transcript.id, transcript.rev))
                            .node(node, path)
                            .hint(format!("add `lyrics:{}=<transcript.json>` to the media table", transcript.id)),
                    ),
                    Err(error) => issues.push(Issue::new(Severity::Error, "media.missing", format!("the transcript cannot be read: {error}")).node(node, path)),
                }
                continue;
            }
            _ => continue,
        };
        if let MediaRef::MissingOptional(missing) = src {
            issues.push(Issue::new(Severity::Info, "media.optional", format!("media not present: {missing} (plays silent)")).node(node, path));
            continue;
        }
        let kind = resolved.entry(src.clone()).or_insert_with(|| resolver.resolve(src).map(|r| r.kind)).clone();
        match kind {
            Err(error) => issues.push(
                Issue::new(Severity::Error, "media.missing", format!("{} cannot be resolved: {error}", media_name(src))).node(node, path).hint(missing_hint(src)),
            ),
            Ok(kind) => {
                let wrong = match (want, kind) {
                    ("Clip", MediaKind::Image) => Some("an image; a Clip plays video. Use `Still{src: …}` for an image"),
                    ("Clip", MediaKind::Audio) => Some("sound only; a Clip plays video. Use `AudioClip{src: …}` for sound"),
                    ("Still", MediaKind::Video) => Some("a video; a Still shows an image. Use `Clip{src: …}` for a video"),
                    ("Still", MediaKind::Audio) => Some("sound; a Still shows an image"),
                    ("AudioClip", MediaKind::Image) => Some("an image; an AudioClip plays sound"),
                    _ => None,
                };
                if let Some(wrong) = wrong {
                    issues.push(Issue::new(Severity::Error, "media.kind", format!("{} is {wrong}", media_name(src))).node(node, path));
                }
            }
        }
    }
}

/// Bundled preset names close to `name`: sharing its words or a prefix.
pub(crate) fn suggest(name: &str, transition: bool) -> Vec<&'static str> {
    let bare = name.trim_start_matches(|c: char| c.is_ascii_digit() || c == '_').to_ascii_lowercase();
    let words: Vec<&str> = bare.split(|c: char| c == '_' || c == ' ' || c == '-').filter(|w| w.len() >= 3).collect();
    let mut scored: Vec<(usize, &'static str)> = packs::presets()
        .into_iter()
        .map(|(preset, _)| preset)
        .filter(|preset| !transition || packs::is_transition(preset))
        .filter_map(|preset| {
            let score = words.iter().filter(|word| preset.contains(**word)).count();
            (score > 0).then_some((score, preset))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
    scored.into_iter().take(5).map(|(_, preset)| preset).collect()
}

fn check_effects(nodes: &[(String, CutNode)], resolver: &mut Option<&mut dyn MediaResolver>, issues: &mut Vec<Issue>) {
    for (path, node) in nodes {
        let (name, transition) = match &node.kind {
            CutNodeKind::Fx { effect: EffectRef::Preset(name), .. } => (name.clone(), false),
            CutNodeKind::Transition { effect, .. } if !effect.is_empty() => (effect.clone(), true),
            CutNodeKind::Fx { effect: EffectRef::Asset(asset), .. } => {
                let Some(resolver) = resolver.as_deref_mut() else { continue };
                match resolver.effect_source(asset) {
                    Ok(source) => {
                        if let Some(why) = effect_refusal(&source, false) {
                            issues.push(not_seekable(node, path, &format!("the library effect {}", asset.id), &why));
                        }
                    }
                    Err(error) => issues.push(
                        Issue::new(Severity::Error, "media.missing", format!("the library effect asset(\"{}\", \"{}\") cannot be read: {error}", asset.id, asset.rev))
                            .node(node, path)
                            .hint(format!("add `effect:{}=<effect.splash>` to the media table", asset.id)),
                    ),
                }
                continue;
            }
            _ => continue,
        };
        let bare = name.strip_prefix("vjfx/").unwrap_or(&name);
        let Some((_, _, source)) = packs::resolve(bare) else {
            let near = suggest(bare, transition);
            let hint = if makepad_content_packs::effect_packs().is_empty() {
                packs::missing("a vjfx/ preset")
            } else if near.is_empty() {
                "name a bundled preset as \"vjfx/<name>\" (the VJ effect library), or a library effect as asset(id, rev)".to_string()
            } else {
                format!("did you mean {}?", near.iter().map(|n| format!("\"vjfx/{n}\"")).collect::<Vec<_>>().join(", "))
            };
            issues.push(Issue::new(Severity::Error, "effect.unknown", format!("there is no bundled effect `{name}`")).node(node, path).hint(hint));
            continue;
        };
        if transition && !packs::is_transition(bare) {
            let near = suggest(bare, true);
            issues.push(
                Issue::new(Severity::Error, "effect.not_transition", format!("`{name}` is not a transition preset"))
                    .node(node, path)
                    .hint(if near.is_empty() { "use a transition preset, e.g. \"vjfx/101_trans_dissolve\" or \"vjfx/102_trans_wipe\"".into() } else { format!("transition presets like {}", near.join(", ")) }),
            );
            continue;
        }
        if let Some(why) = effect_refusal(source, false) {
            issues.push(not_seekable(node, path, &format!("`{name}`"), &why));
        }
    }
}

/// Why an effect document cannot be drawn as a function of time, read from
/// its text by the installed effect pack (with none installed: that it
/// needs one). `literal_text`: the layer gives its own words.
pub(crate) fn effect_refusal(source: &str, literal_text: bool) -> Option<String> {
    match packs::library() {
        Some(pack) => pack.seek_refusal(source, literal_text),
        None => Some(packs::missing("the effect")),
    }
}

fn not_seekable(node: &CutNode, path: &str, what: &str, why: &str) -> Issue {
    Issue::new(Severity::Error, "effect.not_seekable", format!("{what}: {why}; a cut renders every frame as a function of its time, so it refuses this effect"))
        .node(node, path)
        .hint("pick an effect whose look is a function of time and beat alone (no feedback, hold, simulation or per-frame script)")
}

/// The timeline lane a node is drawn in (the CUT page's rows).
fn lane(node: &CutNode) -> Option<&'static str> {
    match node.kind {
        CutNodeKind::Clip { .. } | CutNodeKind::Still { .. } | CutNodeKind::Title { .. } => Some("video"),
        CutNodeKind::AudioClip { .. } => Some("audio"),
        CutNodeKind::VoiceOver { .. } => Some("voice"),
        CutNodeKind::Subtitles { .. } => Some("captions"),
        _ => None,
    }
}

/// The wires a node reads its picture from.
fn frame_inputs(kind: &CutNodeKind) -> Vec<&Wire> {
    match kind {
        CutNodeKind::Fx { input, .. } | CutNodeKind::Reframe { input, .. } => input.iter().collect(),
        CutNodeKind::Transition { a, b, .. } => a.iter().chain(b.iter()).collect(),
        CutNodeKind::Stack { layers } => layers.iter().map(|(wire, _)| wire).collect(),
        CutNodeKind::Output { frame, .. } => frame.iter().collect(),
        _ => Vec::new(),
    }
}

/// Every node `wire` reads through, itself included.
fn upstream<'a>(doc: &'a CutDoc, wire: &Wire, out: &mut HashSet<&'a str>) {
    let Some(node) = doc.node(&wire.node) else { return };
    if !out.insert(node.id.as_str()) {
        return;
    }
    for input in frame_inputs(&node.kind) {
        upstream(doc, input, out);
    }
}

fn check_spans(edit: &CutDoc, nodes: &[(String, CutNode)], issues: &mut Vec<Issue>) {
    let frame = 1.0 / edit.output.fps.max(1.0);
    for (path, node) in nodes.iter().filter(|(path, _)| !path.contains('/')) {
        if let Some(span) = node.span {
            if span.length < frame - 1e-9 {
                issues.push(
                    Issue::new(Severity::Warning, "span.empty", format!("the span is {:.3} s, shorter than one frame ({frame:.3} s), so it never shows", span.length))
                        .node(node, path)
                        .at(span.start, span.end())
                        .hint("give it a length of at least one frame, or remove the node"),
                );
            }
        }
    }
    // Blocks on one row of the timeline that overlap, unless a transition
    // joins them (that overlap is the transition's).
    let mut joined: Vec<(HashSet<&str>, HashSet<&str>)> = Vec::new();
    for node in &edit.nodes {
        if let CutNodeKind::Transition { a: Some(a), b: Some(b), .. } = &node.kind {
            let (mut sa, mut sb) = (HashSet::new(), HashSet::new());
            upstream(edit, a, &mut sa);
            upstream(edit, b, &mut sb);
            joined.push((sa, sb));
        }
    }
    // (lane, id, track, span, line, col) of every block, graphics included.
    let mut blocks: Vec<(&str, &str, u32, Span, u32, u32)> =
        edit.nodes.iter().filter_map(|node| Some((lane(node)?, node.id.as_str(), node.track, node.span?, node.loc.line, node.loc.col))).collect();
    blocks.extend(edit.graphics.iter().filter_map(|g| Some(("video", g.id.as_str(), g.track, g.span?, g.loc.line, g.loc.col))));
    let overlaps = |a: Span, b: Span| a.start < b.end() - 1e-9 && b.start < a.end() - 1e-9;
    let mut order: Vec<usize> = (0..blocks.len()).collect();
    order.sort_by(|&x, &y| blocks[x].3.start.total_cmp(&blocks[y].3.start).then(x.cmp(&y)));
    for (at, &i) in order.iter().enumerate() {
        let (lane, b, track, sb, line, col) = blocks[i];
        let under: Vec<(&str, Span)> = order[..at]
            .iter()
            .map(|&j| blocks[j])
            .filter(|(l, _, t, sa, ..)| *l == lane && *t == track && overlaps(*sa, sb))
            .filter(|(_, a, ..)| !joined.iter().any(|(x, y)| (x.contains(a) && y.contains(b)) || (x.contains(b) && y.contains(a))))
            .map(|(_, a, _, sa, ..)| (a, sa))
            .collect();
        if under.is_empty() {
            continue;
        }
        // The first row of the lane where nothing else plays then (the
        // CUT page's own rule for a new block).
        let free = (0..)
            .find(|row| !blocks.iter().any(|(l, n, t, sn, ..)| *l == lane && *n != b && t == row && overlaps(*sn, sb)))
            .unwrap_or(track + 1);
        let (t0, t1) = under.iter().fold((f64::MAX, f64::MIN), |(t0, t1), (_, sa)| (t0.min(sa.start.max(sb.start)), t1.max(sa.end().min(sb.end()))));
        let names = under.iter().map(|(a, _)| format!("`{a}`")).collect::<Vec<_>>().join(", ");
        let mut issue = Issue::new(Severity::Warning, "span.overlap", format!("`{b}` overlaps {names} on {lane} track {track}"))
            .at(t0, t1)
            .hint(format!("give `{b}` `track: {free}` (the timeline shows one block per row at a time), or join them with a Transition"));
        (issue.line, issue.col, issue.node) = (line, col, Some(b.to_string()));
        issues.push(issue);
    }
    // Keys written past the end of their span never play.
    for (path, node) in nodes.iter().filter(|(path, _)| !path.contains('/')) {
        let Some(span) = node.span else { continue };
        for (field, last) in key_ends(&node.kind) {
            if last > span.length + 1e-9 {
                issues.push(
                    Issue::new(Severity::Warning, "keys.past_span", format!("`{field}` has a key at {last:.2} s, past the span's length {:.2} s; key times count from the span's start", span.length))
                        .node(node, path)
                        .hint("write key times relative to the span's start, from 0 to its length"),
                );
            }
        }
    }
}

/// `(field, last key time)` of every keyed parameter.
fn key_ends(kind: &CutNodeKind) -> Vec<(&'static str, f64)> {
    fn last1(field: &'static str, value: &Animated<1>, out: &mut Vec<(&'static str, f64)>) {
        if let Animated::Keys(keys) = value {
            if let Some(point) = keys.points.last() {
                out.push((field, point.t));
            }
        }
    }
    let mut out = Vec::new();
    match kind {
        CutNodeKind::Clip { opacity, gain, pan, .. } => {
            last1("opacity", opacity, &mut out);
            last1("gain", gain, &mut out);
            last1("pan", pan, &mut out);
        }
        CutNodeKind::Still { opacity, .. } => last1("opacity", opacity, &mut out),
        CutNodeKind::Title { font_size, .. } => last1("font_size", font_size, &mut out),
        CutNodeKind::Fx { params, .. } => {
            for (field, value) in ["p1", "p2", "p3", "p4"].into_iter().zip(params) {
                last1(field, value, &mut out);
            }
        }
        CutNodeKind::Transition { params, mix, .. } => {
            for (field, value) in ["p1", "p2", "p3"].into_iter().zip(params) {
                last1(field, value, &mut out);
            }
            if let Some(mix) = mix {
                last1("mix", mix, &mut out);
            }
        }
        CutNodeKind::AudioClip { gain, pan, .. } | CutNodeKind::VoiceOver { gain, pan, .. } => {
            last1("gain", gain, &mut out);
            last1("pan", pan, &mut out);
        }
        CutNodeKind::Reframe { crop: Some(Animated::Keys(keys)), .. } => {
            if let Some(point) = keys.points.last() {
                out.push(("crop", point.t));
            }
        }
        _ => {}
    }
    out
}

/// The largest value an animated scalar takes (its constant, or its
/// largest key).
fn max_of(value: &Animated<1>) -> f64 {
    match value {
        Animated::Const([v]) => *v,
        Animated::Keys(keys) => keys.points.iter().map(|p| p.v[0]).fold(f64::MIN, f64::max),
    }
}

/// A portrait output (taller than 4:5) is checked against a safe zone.
fn portrait(edit: &CutDoc) -> bool {
    edit.output.height as f64 > edit.output.width as f64 * 1.25
}

/// A title's estimated box, in output fractions: its text at 0.55 em per
/// character (the loader's caption estimate), one line of 1.3 em.
fn title_box(text: &str, anchor: [f64; 2], font: f64, size: (u32, u32)) -> [f64; 4] {
    let (w, h) = (size.0.max(1) as f64, size.1.max(1) as f64);
    let longest = text.lines().map(|line| line.chars().count()).max().unwrap_or(0) as f64;
    let rows = text.lines().count().max(1) as f64;
    let (bw, bh) = (longest * font * 0.55 / w, rows * font * 1.3 / h);
    [anchor[0] - bw / 2.0, anchor[1] - bh / 2.0, bw, bh]
}

fn check_text(edit: &CutDoc, nodes: &[(String, CutNode)], options: &InspectOptions, issues: &mut Vec<Issue>) {
    let size = (edit.output.width, edit.output.height);
    let safe = options.safe.unwrap_or(SafeZone::Reels);
    for (path, node) in nodes.iter().filter(|(path, _)| !path.contains('/')) {
        let span = node.span.map(|s| (s.start, s.end())).unwrap_or((0.0, edit.duration));
        match &node.kind {
            CutNodeKind::Title { text, anchor, font_size, .. } => {
                if !(0.0..=1.0).contains(&anchor[0]) || !(0.0..=1.0).contains(&anchor[1]) {
                    issues.push(
                        Issue::new(Severity::Error, "layer.outside_frame", format!("the title is anchored at vec2({}, {}), outside the frame", anchor[0], anchor[1]))
                            .node(node, path)
                            .at(span.0, span.1)
                            .hint("`anchor` is in output fractions: vec2(0.5, 0.5) is the centre, both from 0 to 1"),
                    );
                    continue;
                }
                let font = max_of(font_size);
                let [x, y, bw, bh] = title_box(text, *anchor, font, size);
                if bw > 1.0 || x < 0.0 || x + bw > 1.0 || y < 0.0 || y + bh > 1.0 {
                    let chars = text.lines().map(|line| line.chars().count()).max().unwrap_or(1).max(1) as f64;
                    let fits = (0.9 * size.0 as f64 / (chars * 0.55)).floor();
                    let over = ((bw - 1.0).max(0.0) * size.0 as f64).round();
                    let message = if bw > 1.0 {
                        format!("the title (\"{}\") is about {over} px wider than the frame at font_size {font}", clip_text(text))
                    } else {
                        format!("the title (\"{}\") runs past the frame's edge at anchor vec2({}, {})", clip_text(text), anchor[0], anchor[1])
                    };
                    issues.push(
                        Issue::new(Severity::Warning, "text.overflow", message)
                            .node(node, path)
                            .at(span.0, span.1)
                            .hint(format!("font_size {fits} or less fits 90% of the width; or shorten the text, or move the anchor towards the centre")),
                    );
                    continue;
                }
                if portrait(edit) && safe != SafeZone::None {
                    let bands = safe.bands();
                    let mut hit = Vec::new();
                    if y < bands.top {
                        hit.push("top");
                    }
                    if y + bh > 1.0 - bands.bottom {
                        hit.push("bottom");
                    }
                    if x < bands.left {
                        hit.push("left");
                    }
                    if x + bw > 1.0 - bands.right {
                        hit.push("right");
                    }
                    if !hit.is_empty() {
                        let (lo, hi) = (bands.top + bh / 2.0, 1.0 - bands.bottom - bh / 2.0);
                        issues.push(
                            Issue::new(
                                Severity::Warning,
                                "safe_zone",
                                format!("the title reaches into the {} band that @{} covers with its own buttons and caption", hit.join(" and "), safe.id()),
                            )
                            .node(node, path)
                            .at(span.0, span.1)
                            .hint(format!(
                                "keep the anchor's y between {lo:.2} and {hi:.2}{}",
                                if x < bands.left || x + bw > 1.0 - bands.right { " and the text narrower (smaller font_size or fewer characters)" } else { "" }
                            )),
                        );
                    }
                }
            }
            CutNodeKind::Subtitles { safe: SafeZone::None, .. } if portrait(edit) => {
                issues.push(
                    Issue::new(Severity::Warning, "safe_zone", "captions in a portrait edit without a safe zone can sit under the platform's own buttons and caption")
                        .node(node, path)
                        .hint(format!("add `safe: @{}` (or @tiktok, @shorts)", safe.id())),
                );
            }
            CutNodeKind::Reframe { safe: SafeZone::None, mode, .. } if portrait(edit) && *mode == ReframeMode::Auto => {
                issues.push(
                    Issue::new(Severity::Info, "safe_zone", "the automatic camera keeps no platform bands clear")
                        .node(node, path)
                        .hint(format!("add `safe: @{}` so the action stays out of the covered bands", safe.id())),
                );
            }
            _ => {}
        }
    }
}

fn clip_text(text: &str) -> String {
    let text: String = text.chars().take(40).collect();
    text.replace('\n', " ")
}

/// Each `Graphic`'s Motion document: resolvable, and loading with the
/// graphic's params (M1's diagnostics, placed at the graphic).
fn check_graphics(edit: &CutDoc, resolver: &mut Option<&mut dyn MediaResolver>, issues: &mut Vec<Issue>) {
    for graphic in &edit.graphics {
        let at = |mut issue: Issue| {
            (issue.line, issue.col, issue.node) = (graphic.loc.line, graphic.loc.col, Some(graphic.id.clone()));
            issue
        };
        let source = match &graphic.src {
            GraphicSrc::Tree(tree) => match tree.resolve(&tree.document).and_then(|file| file.text().map(str::to_string)) {
                Ok(source) => source,
                Err(error) => { issues.push(at(Issue::new(Severity::Error, "graphic.path", error))); continue; }
            },
            GraphicSrc::Builtin(name) => match makepad_motion::builtin::source(name) {
                Some(source) => source.to_string(),
                None => {
                    issues.push(at(Issue::new(Severity::Error, "graphic.builtin", format!("there is no built-in graphic \"{name}\""))));
                    continue;
                }
            },
            GraphicSrc::Asset(asset) => {
                let Some(resolver) = resolver.as_deref_mut() else { continue };
                match resolver.motion_source(asset) {
                    Ok(source) => source,
                    Err(error) => {
                        issues.push(at(Issue::new(Severity::Error, "media.missing", format!("the Motion document asset(\"{}\", \"{}\") cannot be read: {error}", asset.id, asset.rev))
                            .hint(format!("add `{}=<document.splash>` to the media table", asset.id))));
                        continue;
                    }
                }
            }
        };
        let context = makepad_motion::MotionContext {
            modules: match &graphic.src {
                GraphicSrc::Tree(tree) => makepad_motion::modules::Modules::from_tree(tree.clone()),
                _ => Default::default(),
            },
            ..Default::default()
        };
        let diags = match makepad_motion::MotionDoc::load_with(&source, &makepad_story_cut::graphic::motion_inputs(&graphic.params), context) {
            Ok(doc) => doc.warnings,
            Err(diags) => diags,
        };
        for diag in diags {
            let error = diag.severity == makepad_motion::Severity::Error;
            let place = if diag.path.is_empty() { String::new() } else { format!(" {}", diag.path) };
            let mut issue = at(Issue::new(
                if error { Severity::Error } else { Severity::Warning },
                if error { "graphic.motion" } else { "graphic.warning" },
                format!("its Motion document, line {}{place}: {}", diag.loc.line, diag.message),
            ));
            issue.hint = diag.hint.clone();
            issues.push(issue);
        }
    }
}

fn check_voice(nodes: &[(String, CutNode)], issues: &mut Vec<Issue>) {
    for (path, node) in nodes.iter().filter(|(path, _)| !path.contains('/')) {
        let CutNodeKind::VoiceOver { voice, lines, .. } = &node.kind else { continue };
        for (index, line) in lines.iter().enumerate() {
            if line.take.is_none() {
                issues.push(
                    Issue::new(Severity::Warning, "voice.no_take", format!("line {index} (\"{}\") has no generated take, so it is silent", clip_text(&line.text)))
                        .node(node, path)
                        .hint("generate the voice (CUT page: Generate), which pins each line's take as `take: asset(…)`"),
                );
            } else if line.stale(voice) {
                issues.push(
                    Issue::new(Severity::Warning, "voice.stale", format!("line {index}'s take was made for other text or another voice"))
                        .node(node, path)
                        .hint("Update voice regenerates the stale lines"),
                );
            }
        }
    }
}

fn check_markers(edit: &CutDoc, options: &InspectOptions, issues: &mut Vec<Issue>) {
    for marker in edit.markers() {
        let known = options.marks.get(&marker.run).is_some_and(|marks| marks.contains_key(&marker.name));
        if !known {
            let Some(node) = edit.node(&marker.node) else { continue };
            issues.push(
                Issue::new(Severity::Warning, "marker.unresolved", format!("marker(@{}) of run {} is not known here, so what it times draws or sounds nothing", marker.name, marker.run.id))
                    .node(node, &node.id)
                    .hint("the marks come from the run's run.json (Stage passes them); check the name against the run's `app.mark(@…)` calls"),
            );
        }
    }
}

/// The times the time-based checks look at: a grid, and each span's first
/// and last frame.
fn sample_times(edit: &CutDoc, nodes: &[(String, CutNode)], sample_fps: f64) -> Vec<f64> {
    let frame = 1.0 / edit.output.fps.max(1.0);
    let step = 1.0 / sample_fps.max(0.1);
    let mut times: Vec<f64> = (0..).map(|k| k as f64 * step).take_while(|t| *t < edit.duration).collect();
    for (_, node) in nodes {
        if let Some(span) = node.span {
            times.push(span.start);
            times.push((span.end() - frame).max(span.start));
        }
    }
    times.retain(|t| *t >= 0.0 && *t < edit.duration);
    times.sort_by(f64::total_cmp);
    times.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    times
}

/// Contiguous runs of sample times where `hit` holds, as `[first, end)`
/// where `end` is the next sample that does not hit (or the cut's end).
fn runs(times: &[f64], hits: &[bool], end: f64) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, (&t, &hit)) in times.iter().zip(hits).enumerate() {
        match (hit, start) {
            (true, None) => start = Some(t),
            (false, Some(s)) => {
                out.push((s, t));
                start = None;
            }
            _ => {}
        }
        if i + 1 == times.len() {
            if let Some(s) = start {
                out.push((s, end));
            }
        }
    }
    out
}

fn check_over_time(edit: &CutDoc, nodes: &[(String, CutNode)], eval: &CutEval<'_>, options: &InspectOptions, issues: &mut Vec<Issue>) {
    let times = sample_times(edit, nodes, options.sample_fps);
    let mut empty = Vec::with_capacity(times.len());
    let mut crowded = Vec::with_capacity(times.len());
    let mut most = (0usize, 0.0);
    for &t in &times {
        let plan = eval.frame_plan(t);
        empty.push(plan.result.is_none());
        let clips = plan.ops.iter().filter(|op| matches!(op.kind, DrawKind::Source { still: false, .. })).count();
        if clips > most.0 {
            most = (clips, t);
        }
        crowded.push(clips > MAX_ACTIVE_CLIPS);
    }
    for (t0, t1) in runs(&times, &empty, edit.duration) {
        issues.push(
            Issue::new(Severity::Warning, "frame.gap", format!("nothing is drawn from {t0:.2} s to {t1:.2} s: those frames are black"))
                .at(t0, t1)
                .hint("extend a clip's span over the gap, add a Still or Title there, or shorten the cut's duration"),
        );
    }
    for (t0, t1) in runs(&times, &crowded, edit.duration) {
        issues.push(
            Issue::new(Severity::Error, "clips.limit", format!("more than {MAX_ACTIVE_CLIPS} video clips play at once (counting nested cuts; {} at {:.2} s)", most.0, most.1))
                .at(t0, t1)
                .hint("stagger the clips' spans, or render a nested cut to a video and use that"),
        );
    }
}

fn check_captions(edit: &CutDoc, nodes: &[(String, CutNode)], eval: &CutEval<'_>, options: &InspectOptions, issues: &mut Vec<Issue>) {
    let mut shrunk: HashMap<String, (f64, f64, f64)> = HashMap::new();
    for t in sample_times(edit, nodes, options.sample_fps) {
        for frame in eval.subtitle_plan(t) {
            let Some(node) = edit.node(&frame.node) else { continue };
            let CutNodeKind::Subtitles { font_size, .. } = &node.kind else { continue };
            let authored = font_size.at1(node.local(t));
            if frame.font_size < authored * 0.999 {
                let entry = shrunk.entry(frame.node.clone()).or_insert((t, authored, frame.font_size));
                entry.2 = entry.2.min(frame.font_size);
            }
            let width = frame.text.chars().count() as f64 * frame.font_size * CAPTION_CHAR_EM;
            if frame.karaoke.lines > 2 || width > frame.karaoke.safe[2] * edit.output.width as f64 * 2.0 + 1.0 {
                issues.push(
                    Issue::new(Severity::Warning, "text.overflow", format!("the caption \"{}\" needs more than two lines", clip_text(&frame.text)))
                        .node(node, &node.id)
                        .at(t, t)
                        .hint("lower `max_words`, or split the voice line"),
                );
            }
        }
    }
    let mut shrunk: Vec<_> = shrunk.into_iter().collect();
    shrunk.sort_by(|a, b| a.0.cmp(&b.0));
    for (id, (t, authored, smallest)) in shrunk {
        let Some(node) = edit.node(&id) else { continue };
        issues.push(
            Issue::new(Severity::Info, "text.shrunk", format!("captions shrink from {authored:.0} px to as small as {smallest:.0} px to fit two lines (first at {t:.2} s)"))
                .node(node, &id)
                .at(t, t)
                .hint("lower `max_words` or `font_size` for an even caption size"),
        );
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::input::Document;

    pub(crate) fn edit(nodes: &str, list: &str, output: &str, duration: f64) -> Document {
        let source = format!(
            "use mod.flow.*\nuse mod.cut.*\n\n{nodes}\n\nFlow{{\n    kind: @cut\n    label: \"t\"\n    output: {output}\n    duration: {duration}\n    bpm: 120\n    {list}\n}}\n"
        );
        Document::from_source(&source, "t.splash").unwrap()
    }

    fn codes(report: &InspectReport) -> Vec<(&str, Severity)> {
        report.issues.iter().map(|issue| (issue.code.as_str(), issue.severity)).collect()
    }

    pub(crate) fn asset_id(seed: u8) -> String {
        makepad_asset_data::AssetId::from_bytes([seed; 16]).to_string()
    }

    /// `asset(id, rev)` of a made-up asset.
    pub(crate) fn asset_ref(seed: u8) -> String {
        format!("asset(\"{}\", \"arev_{}\")", asset_id(seed), format!("{seed:02x}").repeat(32))
    }

    #[test]
    fn a_load_error_is_reported_at_its_place_with_a_fix() {
        let doc = edit("let t = Title{text: \"hi\" span: [0.0, 1.0] anchr: vec2(0.5, 0.5)}\nlet out = CutOutput{frame: t.out(@frame)}", "t, out", "{width: 640 height: 360 fps: 30}", 1.0);
        let report = inspect(&doc, None, &InspectOptions::default());
        assert!(!report.ok());
        let issue = &report.issues[0];
        assert_eq!(issue.severity, Severity::Error);
        assert!(issue.code.starts_with("load."), "{issue:?}");
        assert!(issue.line >= 4, "{issue:?}");
        assert!(issue.hint.is_some());
        let json = report.to_json().to_json();
        assert!(json.contains("\"ok\":false"), "{json}");
    }

    #[test]
    fn a_clean_edit_has_no_errors_and_notes_what_it_could_not_check() {
        let doc = edit(
            "let t = Title{text: \"Hello\" span: [0.0, 2.0] anchor: vec2(0.5, 0.5) font_size: 40}\nlet out = CutOutput{frame: t.out(@frame)}",
            "t, out",
            "{width: 640 height: 360 fps: 30}",
            2.0,
        );
        let report = inspect(&doc, None, &InspectOptions::default());
        assert!(report.ok(), "{}", report.to_text());
        assert_eq!(report.count(Severity::Warning), 0, "{}", report.to_text());
        assert_eq!(codes(&report), vec![("media.unchecked", Severity::Info)]);
    }

    #[test]
    fn overflow_outside_safe_zones_gaps_and_unknown_effects_are_found() {
        let nodes = "let wide = Title{text: \"This headline is far too long for a phone\" span: [0.0, 2.0] anchor: vec2(0.5, 0.5) font_size: 120}\n\
                     let low = Title{text: \"Buy\" span: [0.0, 2.0] anchor: vec2(0.5, 0.95) font_size: 60 track: 1}\n\
                     let off = Title{text: \"x\" span: [0.0, 2.0] anchor: vec2(1.4, 0.5) track: 2}\n\
                     let s = Stack{layers: [wide.out(@frame), low.out(@frame), off.out(@frame)]}\n\
                     let fx = Fx{input: s.out(@frame) effect: \"vjfx/no_such_bloom\" span: [0.0, 2.0]}\n\
                     let out = CutOutput{frame: fx.out(@frame)}";
        let doc = edit(nodes, "wide, low, off, s, fx, out", "{width: 1080 height: 1920 fps: 30}", 3.0);
        let report = inspect(&doc, None, &InspectOptions::default());
        let found = codes(&report);
        for want in [("layer.outside_frame", Severity::Error), ("effect.unknown", Severity::Error), ("text.overflow", Severity::Warning), ("safe_zone", Severity::Warning), ("frame.gap", Severity::Warning)] {
            assert!(found.contains(&want), "{want:?} missing from {}", report.to_text());
        }
        let gap = report.issues.iter().find(|issue| issue.code == "frame.gap").unwrap();
        assert_eq!(gap.at, Some((2.0, 3.0)));
        let bloom = report.issues.iter().find(|issue| issue.code == "effect.unknown").unwrap();
        assert!(bloom.hint.as_deref().unwrap().contains("needs the VJ effect pack"), "no effect pack is installed: {bloom:?}");
        assert!(!report.ok());
        let text = report.to_text();
        assert!(text.contains("t.splash:"), "{text}");
    }

    #[test]
    fn overlaps_on_one_row_are_warned_unless_a_transition_joins_them() {
        let a_ref = asset_ref(0x0a);
        let nodes = format!(
            "let a = Clip{{src: {a_ref} span: [0.0, 2.0]}}\n\
             let b = Clip{{src: {a_ref} span: [1.5, 1.5]}}\n\
             let c = Clip{{src: {a_ref} span: [3.0, 1.0]}}\n\
             let d = Clip{{src: {a_ref} span: [3.5, 0.02]}}\n\
             let w = Transition{{a: a.out(@frame) b: b.out(@frame) effect: \"\" span: [1.5, 0.5]}}\n\
             let s = Stack{{layers: [w.out(@frame), c.out(@frame), d.out(@frame)]}}\n\
             let out = CutOutput{{frame: s.out(@frame)}}"
        );
        let doc = edit(&nodes, "a, b, c, d, w, s, out", "{width: 640 height: 360 fps: 30}", 4.0);
        let report = inspect(&doc, None, &InspectOptions::default());
        let overlaps: Vec<_> = report.issues.iter().filter(|issue| issue.code == "span.overlap").collect();
        assert_eq!(overlaps.len(), 1, "{}", report.to_text());
        assert_eq!(overlaps[0].node.as_deref(), Some("d"));
        assert!(report.issues.iter().any(|issue| issue.code == "span.empty" && issue.node.as_deref() == Some("d")), "{}", report.to_text());
    }

    #[test]
    fn missing_media_is_found_through_the_resolver() {
        let nodes = format!("let a = Clip{{src: {} span: [0.0, 1.0]}}\nlet out = CutOutput{{frame: a.out(@frame)}}", asset_ref(0x0a));
        let doc = edit(&nodes, "a, out", "{width: 640 height: 360 fps: 30}", 1.0);
        let mut media = makepad_story_edit_render::LocalMedia::new();
        let report = inspect(&doc, Some(&mut media), &InspectOptions::default());
        let missing = report.issues.iter().find(|issue| issue.code == "media.missing").expect("missing media");
        assert_eq!(missing.node.as_deref(), Some("a"));
        assert!(missing.hint.as_deref().unwrap().contains(&format!("{}=<file>", asset_id(0x0a))), "{missing:?}");
    }

    #[test]
    fn a_graphic_s_motion_document_is_checked_with_its_params() {
        let dir = std::env::temp_dir().join(format!("edit_tools_graphic_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let motion = dir.join("bad.motion.splash");
        std::fs::write(&motion, "use mod.motion.*; Motion{\n  size: vec2(640, 360) duration: 1.0\n  inputs: {headline: \"Hi\"}\n  layers: [Rect{sise: vec2(10, 10)}]\n}\n").unwrap();
        let nodes = format!(
            "let g = Graphic{{src: {} span: [0.0, 1.0] params: {{headline: \"Hello\"}}}}\nlet out = CutOutput{{frame: g.out(@frame)}}",
            asset_ref(0x4d)
        );
        let doc = edit(&nodes, "g, out", "{width: 640 height: 360 fps: 30}", 1.0);
        let mut media = makepad_story_edit_render::LocalMedia::new();
        media.insert(asset_id(0x4d), &motion);
        let report = inspect(&doc, Some(&mut media), &InspectOptions::default());
        let issue = report.issues.iter().find(|i| i.code == "graphic.motion").unwrap_or_else(|| panic!("{}", report.to_text()));
        assert_eq!(issue.node.as_deref(), Some("g"));
        assert!(issue.message.contains("line 4"), "{issue:?}");
        // Without the file: missing.
        let report = inspect(&doc, Some(&mut makepad_story_edit_render::LocalMedia::new()), &InspectOptions::default());
        assert!(report.issues.iter().any(|i| i.code == "media.missing" && i.node.as_deref() == Some("g")), "{}", report.to_text());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
