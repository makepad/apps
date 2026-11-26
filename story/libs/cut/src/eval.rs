//! `CutEval`: a cut as a pure function of time. It reads
//! nothing and draws nothing: `frame_plan(t)` says which source frame to
//! draw where, which effect at which local time, and in which order;
//! `audio_plan(t0, t1)` says which audio to mix at which gains. The offline
//! renderer and the live preview execute the plans.

use relax_attr::relax;
use crate::audio::{duck_amount, duck_gain, fade_gain, merge_intervals};
use crate::model::*;
use crate::reframe::{CropKey, Rect};
use crate::subtitle::{captions_with, estimate_words, place_caption, Caption, ESTIMATED_SECONDS_PER_WORD, MAX_CAPTION_CHARS, MAX_CAPTION_CHARS_VERTICAL};
use std::collections::HashMap;

/// A texture a plan's op writes: the index of that op in `FramePlan::ops`.
pub type Slot = usize;

/// Media timestamps are 10 MHz ticks (frame n is at `pts = n * 10_000_000 /
/// fps`).
pub const TICKS_PER_SECOND: f64 = 10_000_000.0;

#[derive(Clone, Debug, PartialEq)]
pub struct FramePlan {
    pub t: f64,
    /// `t * bpm / 60`.
    pub beat: f64,
    /// Output size in pixels.
    pub size: (u32, u32),
    /// Ordered draw list: every op reads only slots of earlier ops.
    pub ops: Vec<DrawOp>,
    /// The slot the output shows; `None` is a transparent frame.
    pub result: Option<Slot>,
    /// Nodes left out of this plan because a size they need is unknown
    /// (a Reframe's input without `set_source_size`). Empty when the plan is
    /// complete.
    pub unresolved: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DrawOp {
    pub node: String,
    /// The size of the texture this op writes.
    pub size: (u32, u32),
    pub kind: DrawKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DrawKind {
    /// Draw `src_rect` of the source (source pixels) into `dst_rect` of the
    /// texture (texture pixels). `source_time` is the frame at or before
    /// that time; `None` for a still, or for a clip whose marker is not
    /// resolved (then the op draws nothing). `source_ticks` is the same time
    /// rounded to 10 MHz ticks: pick the frame by comparing it with the
    /// source's integer pts, never `source_time` in floating point, which
    /// lands a hair before a frame boundary about as often as after it.
    /// `src_rect`/`dst_rect` are `None` while the source's pixel size is
    /// unknown.
    Source {
        media: MediaRef,
        still: bool,
        source_time: Option<f64>,
        source_ticks: Option<i64>,
        fit: Fit,
        src_rect: Option<Rect>,
        dst_rect: Option<Rect>,
        opacity: f64,
    },
    /// A `Title`, or the caption a `Subtitles` shows (then `karaoke` says
    /// which words are spoken); `position` is its anchor in texture pixels.
    Text { text: String, position: [f64; 2], font_size: f64, style: String, local_time: f64, karaoke: Option<Karaoke> },
    /// `params` are p1..p4; below 0 leaves the effect document's value.
    Effect { effect: EffectRef, input: Option<Slot>, local_time: f64, beat: f64, params: [f64; 4] },
    /// `params[3]` is `mix`, as the live renderer maps it. `effect` is a
    /// preset name (empty: a dissolve) unless `effect_asset` names a library
    /// VjEffect revision, which then wins.
    Transition { effect: String, a: Option<Slot>, b: Option<Slot>, mix: f64, local_time: f64, beat: f64, params: [f64; 4], effect_asset: Option<AssetRef> },
    /// Bottom first.
    Stack { layers: Vec<(Slot, Blend)> },
    /// Draw `crop` of the input texture (whose pixels are the source's)
    /// into `dst_rect` of this texture, letterboxed at the crop's own
    /// aspect.
    Reframe { input: Slot, crop: Rect, dst_rect: Rect },
    /// A Motion document at `local_time` of its own clock (a `Graphic`, or
    /// a `Title` lowered to the built-in "title" document), drawn into this
    /// texture by `fit` at `opacity`. `params` fill its inputs; `offset` is
    /// the cut time of its clock's 0 and `bpm` the cut's tempo, so its
    /// musical time and signals follow the cut's.
    Graphic { src: crate::graphic::GraphicSrc, params: Vec<(String, crate::graphic::InputValue)>, local_time: f64, offset: f64, bpm: f64, fit: Fit, opacity: f64 },
    /// An `AppView`: the picture the host holds for this node (a presented
    /// app window), placed in this texture by `fit` at its own size; a
    /// placeholder naming `app` when the host holds none.
    App { app: String, fit: Fit, rect: Option<[f64; 4]> },
    /// A `Camera`: the picture the host holds for this node (the camera's
    /// newest frame), cropped, turned, mirrored and adjusted on the GPU and
    /// placed by `spec.fit`; a placeholder naming the device when the host
    /// holds none.
    Camera { spec: CameraSpec },
    /// A `Code` node: the picture the host holds for this node (the file
    /// drawn with the code editor), placed by `fit`; a placeholder naming
    /// the file when the host holds none.
    Code { label: String, fit: Fit, rect: Option<[f64; 4]> },
}

/// The word-by-word state of a burned-in caption.
#[derive(Clone, Debug, PartialEq)]
pub struct Karaoke {
    /// The caption's index in `CutEval::captions` of its node.
    pub caption: usize,
    /// The caption's words; `text` is them joined by spaces.
    pub words: Vec<String>,
    /// The word being spoken (the last one started).
    pub active: Option<usize>,
    /// Words spoken so far: whole words count 1, the one being spoken its
    /// elapsed fraction.
    pub highlight: f64,
    /// The safe region `[x, y, w, h]` in fractions of the frame the
    /// caption is drawn into: wrap its lines to this width and keep
    /// the whole block inside it. The anchor and font size already fit it
    /// by estimate; a renderer that measures the text keeps it exact.
    pub safe: [f64; 4],
    /// Lines the caption wraps into, by the same estimate.
    pub lines: usize,
}

/// One `Subtitles` node's caption at a time (`CutEval::subtitle_plan`).
#[derive(Clone, Debug, PartialEq)]
pub struct SubtitleFrame {
    pub node: String,
    pub karaoke: Karaoke,
    pub text: String,
    /// Where the caption is centred, in output fractions: the node's
    /// anchor moved inside its safe zone.
    pub anchor: [f64; 2],
    pub font_size: f64,
    pub style: String,
    /// The caption's words are the current text but not (yet) what a take
    /// says: see [`PlacedLine::provisional`].
    pub provisional: bool,
}

/// What a take is, once measured: its length and, when known, its word
/// times in seconds from the take's start.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct TakeInfo {
    pub duration: f64,
    pub words: Vec<Word>,
    /// Its timed phonemes (IPA in `text`), when the engine gave them: what
    /// a character's lip sync reads.
    pub phones: Vec<Word>,
}

/// A voiceover line placed on the cut's timeline.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedLine {
    /// The line's index in the node's `lines`.
    pub index: usize,
    /// When it starts speaking, in cut seconds (its marker resolved).
    pub start: f64,
    /// When the speech ends: the take's end, or an estimate while the take
    /// is missing or not measured (`set_take`).
    pub end: f64,
    /// Until when the take plays: its end, or up to the next line (or the
    /// edit's end) while its length is unknown.
    pub sound_end: f64,
    pub take: Option<AssetRef>,
    pub text: String,
    /// Word times in cut seconds.
    pub words: Vec<Word>,
    /// Whether `words` take their times from the take (rather than an
    /// estimate).
    pub timed: bool,
    /// Whether `words` are not what the take says: the line is stale (its
    /// text or voice changed since the take) or its times are estimated.
    /// The words are always the line's current text (the text is the
    /// truth); captions of such a line are provisional until the take is
    /// regenerated.
    pub provisional: bool,
    /// Whether the next line starts before this take ends: the take is cut
    /// there, with a [`DECLICK`] fade (lines keep their times).
    pub cut: bool,
    /// The take's timed phonemes in cut seconds (IPA in `text`), when it
    /// has them and says the line's current words; empty otherwise.
    pub phones: Vec<Word>,
}

/// Seconds of fade-out a take gets where the next line cuts it short.
pub const DECLICK: f64 = 0.01;

#[derive(Clone, Debug, PartialEq)]
pub struct AudioPlan {
    pub t0: f64,
    pub t1: f64,
    /// In `CutOutput.audio` order.
    pub segments: Vec<AudioSegment>,
}

/// One source's audio inside `[t0, t1)` of an audio plan.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioSegment {
    pub node: String,
    /// Position in `CutOutput.audio`.
    pub entry: usize,
    pub media: MediaRef,
    /// The part of the plan's window this node is active in, edit seconds.
    pub t0: f64,
    pub t1: f64,
    /// Source time at `t0`; `None` while the clip's marker is unresolved.
    pub source_t0: Option<f64>,
    /// `source_t0` in 10 MHz ticks, rounded, as `DrawKind::Source`'s
    /// `source_ticks`: a mix that starts a source at a frame boundary picks
    /// its first sample from the integer pts, never from floating point.
    pub source_ticks: Option<i64>,
    /// Source seconds per edit second.
    pub speed: f64,
    /// Gain and pan at `t0` and `t1`; keyed ones (keys, fades, ducking)
    /// are exact per sample through `CutEval::gain` and `CutEval::pan`.
    pub gain: (f64, f64),
    pub pan: (f64, f64),
    pub keyed: bool,
    /// A VoiceOver's line index: `media` is that line's take and
    /// `source_t0` is seconds into it.
    pub line: Option<usize>,
}

/// A revision a cut depends on (listed in its manifest's
/// `dependencies`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Dep {
    /// A library asset revision: media, image, VjEffect.
    Asset(AssetRef),
    /// An app run script revision.
    AppRun(AssetRef),
    /// The `Video` render (asset and revision) a `run()` pins, with the run
    /// script revision it came from.
    Render { run: AssetRef, render: AssetRef },
    /// A cut revision a compound clip nests. Only this cut's own
    /// references are listed; the nested cut's manifest lists its own.
    Cut(AssetRef),
}

/// Every revision the cut names, in source order, each once.
pub fn dependencies(doc: &CutDoc) -> Vec<Dep> {
    let mut deps = Vec::new();
    let mut push = |dep: Dep| if !deps.contains(&dep) { deps.push(dep) };
    let media = |media: &MediaRef, push: &mut dyn FnMut(Dep)| match media {
        MediaRef::MissingOptional(_) => {},
        MediaRef::Asset(asset) => push(Dep::Asset(asset.clone())),
        MediaRef::Run(run) => {
            let script = AssetRef { id: run.id.clone(), rev: run.rev.clone() };
            push(Dep::AppRun(script.clone()));
            push(Dep::Render { run: script, render: AssetRef { id: run.render_id.clone(), rev: run.render.clone() } });
        }
    };
    for node in &doc.nodes {
        if let Some(edit) = node.kind.nested_edit() {
            push(Dep::Cut(edit.clone()));
            continue;
        }
        match &node.kind {
            CutNodeKind::Clip { src, .. } | CutNodeKind::Still { src, .. } | CutNodeKind::AudioClip { src, .. } => media(src, &mut push),
            CutNodeKind::Fx { effect: EffectRef::Asset(asset), .. } | CutNodeKind::Transition { effect_asset: Some(asset), .. } => push(Dep::Asset(asset.clone())),
            CutNodeKind::Reframe { focus: Some(focus), .. } => media(focus, &mut push),
            CutNodeKind::VoiceOver { voice, lines, run, .. } => {
                if let Some(reference) = &voice.reference {
                    push(Dep::Asset(reference.clone()));
                }
                if let Some(run) = run {
                    media(&MediaRef::Run(run.clone()), &mut push);
                }
                for take in lines.iter().filter_map(|line| line.take.as_ref()) {
                    push(Dep::Asset(take.clone()));
                }
            }
            CutNodeKind::Subtitles { transcript: Some(transcript), .. } => push(Dep::Asset(transcript.clone())),
            _ => {}
        }
    }
    // A graphic's document and its asset params.
    for graphic in &doc.graphics {
        for asset in graphic.assets() {
            push(Dep::Asset(asset));
        }
    }
    // Every take of a Generate is pinned, chosen or not.
    for gen in &doc.generates {
        for take in &gen.takes {
            push(Dep::Asset(take.video.clone()));
        }
    }
    deps
}

pub struct CutEval<'a> {
    doc: &'a CutDoc,
    index: HashMap<&'a str, usize>,
    marks: HashMap<RunRef, HashMap<String, f64>>,
    sizes: HashMap<MediaRef, (u32, u32)>,
    takes: HashMap<AssetRef, TakeInfo>,
    transcripts: HashMap<AssetRef, Vec<SpokenLine>>,
    /// Derived from the above by `refresh`, so per-sample and per-frame
    /// reads stay cheap: placed voice lines, the merged speech each ducked
    /// source ducks under, and every Subtitles node's captions.
    placed: HashMap<String, Vec<PlacedLine>>,
    /// The evaluators of the cuts compound clips nest, by revision.
    nested: HashMap<AssetRef, CutEval<'a>>,
    ducks: HashMap<String, Vec<(f64, f64)>>,
    captions: HashMap<String, Vec<Caption>>,
}

impl<'a> CutEval<'a> {
    pub fn new(doc: &'a CutDoc) -> Self {
        let index = doc.nodes.iter().enumerate().map(|(index, node)| (node.id.as_str(), index)).collect();
        let mut eval = Self {
            doc, index, marks: HashMap::new(), sizes: HashMap::new(), takes: HashMap::new(), transcripts: HashMap::new(),
            placed: HashMap::new(), ducks: HashMap::new(), captions: HashMap::new(), nested: HashMap::new(),
        };
        eval.refresh();
        eval
    }

    pub fn doc(&self) -> &'a CutDoc {
        self.doc
    }

    /// The marks (`name → seconds` in the render) of one run's render.
    /// Nested edits get them too, as every setter below passes its data
    /// on: marks, takes, transcripts and sizes are keyed by exact revision.
    pub fn set_marks(&mut self, run: RunRef, marks: HashMap<String, f64>) {
        for child in self.nested.values_mut() {
            child.set_marks(run.clone(), marks.clone());
        }
        self.marks.insert(run, marks);
        self.refresh();
    }

    /// The evaluator of the cut revision `cut` that compound clips of
    /// this cut nest ([`crate::CutTree::eval`] attaches them all).
    pub fn set_nested(&mut self, edit: AssetRef, child: CutEval<'a>) {
        self.nested.insert(edit, child);
    }

    pub fn nested(&self, edit: &AssetRef) -> Option<&CutEval<'a>> {
        self.nested.get(edit)
    }

    /// A voice take's length and word times (from its `vj-lyrics` file).
    /// Until a take is set, its line's speech is estimated.
    pub fn set_take(&mut self, take: AssetRef, info: TakeInfo) {
        for child in self.nested.values_mut() {
            child.set_take(take.clone(), info.clone());
        }
        self.takes.insert(take, info);
        self.refresh();
    }

    /// A transcript's lines, in seconds of the source it transcribes.
    pub fn set_transcript(&mut self, transcript: AssetRef, lines: Vec<SpokenLine>) {
        for child in self.nested.values_mut() {
            child.set_transcript(transcript.clone(), lines.clone());
        }
        self.transcripts.insert(transcript, lines);
        self.refresh();
    }

    /// Recompute what the setters change.
    fn refresh(&mut self) {
        let doc = self.doc;
        let mut placed = HashMap::new();
        for node in &doc.nodes {
            if let CutNodeKind::VoiceOver { lines, run, voice, .. } = &node.kind {
                placed.insert(node.id.clone(), self.place_lines(lines, voice, run.as_ref()));
            }
        }
        self.placed = placed;
        // Only lines with a take speak; an ungenerated line is silence.
        let speech = |node: &CutNode| -> Vec<(f64, f64)> {
            match &node.kind {
                CutNodeKind::AudioClip { src: MediaRef::MissingOptional(_), .. }
                | CutNodeKind::Clip { src: MediaRef::MissingOptional(_), .. } => Vec::new(),
                CutNodeKind::VoiceOver { .. } => self.placed.get(&node.id).map_or(Vec::new(), |lines| {
                    lines.iter().filter(|line| line.take.is_some()).map(|line| (line.start, line.end)).collect()
                }),
                CutNodeKind::AudioClip { .. } | CutNodeKind::Clip { audio: true, .. } => vec![self.bounds(node)],
                _ => Vec::new(),
            }
        };
        let mut ducks = HashMap::new();
        for node in &doc.nodes {
            let (CutNodeKind::Clip { shape, .. } | CutNodeKind::AudioClip { shape, .. } | CutNodeKind::VoiceOver { shape, .. }) = &node.kind else { continue };
            let Some(duck) = &shape.duck else { continue };
            let intervals = duck.under.iter().filter_map(|under| self.node(under)).flat_map(speech).collect();
            ducks.insert(node.id.clone(), merge_intervals(intervals));
        }
        self.ducks = ducks;
        let mut all = HashMap::new();
        let (width, height) = (doc.output.width, doc.output.height);
        let max_chars = if height > width { MAX_CAPTION_CHARS_VERTICAL } else { MAX_CAPTION_CHARS };
        for node in &doc.nodes {
            let CutNodeKind::Subtitles { max_words, source, .. } = &node.kind else { continue };
            let mut list = captions_with(&self.spoken_lines(node), *max_words as usize, max_chars);
            // A caption shows only while its Subtitles node does, and never
            // past the cut, burned in or in the sidecars.
            let (from, to) = node.span.map_or((0.0, doc.duration), |span| (span.start.max(0.0), span.end().min(doc.duration)));
            for caption in &mut list {
                caption.start = caption.start.max(from);
                caption.end = caption.end.min(to);
            }
            list.retain(|caption| caption.end > caption.start);
            // Captions of provisional voice lines are provisional.
            if let Some(lines) = source.as_ref().and_then(|source| self.placed.get(&source.node)) {
                for caption in &mut list {
                    caption.provisional = lines.iter().any(|line| line.provisional && line.start < caption.end && caption.start < line.end.max(line.start + 1e-9));
                }
            }
            all.insert(node.id.clone(), list);
        }
        self.captions = all;
    }

    /// [`Self::set_take`] for many takes at once: one recomputation.
    pub fn set_takes(&mut self, takes: impl IntoIterator<Item = (AssetRef, TakeInfo)>) {
        let takes = takes.into_iter().collect::<Vec<_>>();
        for child in self.nested.values_mut() {
            child.set_takes(takes.clone());
        }
        self.takes.extend(takes);
        self.refresh();
    }

    /// Pairs `(earlier, later)` of a VoiceOver's line indices where the later
    /// line starts before the earlier take ends (the earlier take is cut
    /// there), with the seconds cut off. For a badge; the render stays
    /// well-formed.
    pub fn voice_overlaps(&self, node: &str) -> Vec<(usize, usize, f64)> {
        let lines = self.voice_lines(node);
        lines.windows(2).filter(|pair| pair[0].cut).map(|pair| {
            let take_end = pair[0].take.as_ref().and_then(|take| self.takes.get(take)).map_or(pair[0].sound_end, |info| pair[0].start + info.duration);
            (pair[0].index, pair[1].index, take_end - pair[1].start)
        }).collect()
    }

    /// A cut time of a run's render time `at`, through the first clip of
    /// that run whose span shows it (else the first clip of the run).
    fn run_time_to_edit(&self, run: &RunRef, at: f64) -> Option<f64> {
        let mut first = None;
        for node in &self.doc.nodes {
            let CutNodeKind::Clip { src: MediaRef::Run(clip_run), speed, .. } = &node.kind else { continue };
            if clip_run != run {
                continue;
            }
            let Some(source_in) = self.source_in(node) else { continue };
            let t = node.span.map_or(0.0, |span| span.start) + (at - source_in) / speed;
            if node.span.map_or(true, |span| span.contains(t)) {
                return Some(t);
            }
            first.get_or_insert(t);
        }
        first
    }

    /// `lines` placed on the timeline, in time order; a line whose marker
    /// is not resolved yet is left out. Every line starts at its own time;
    /// a take still sounding when the next line starts is cut there
    /// with a [`DECLICK`] fade, so a voiceover's takes never play over each
    /// other ([`Self::voice_overlaps`] lists the cuts).
    fn place_lines(&self, lines: &[VoiceLine], voice: &VoiceSpec, run: Option<&RunRef>) -> Vec<PlacedLine> {
        let mut starts = lines.iter().enumerate().filter_map(|(index, line)| {
            let start = match &line.at {
                SourceTime::Seconds(seconds) => *seconds,
                SourceTime::Marker { name, offset } => {
                    let run = run?;
                    let mark = self.marks.get(run)?.get(name)?;
                    self.run_time_to_edit(run, mark + offset)?
                }
            };
            (start.is_finite() && start < self.doc.duration).then_some((index, start.max(0.0)))
        }).collect::<Vec<_>>();
        starts.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
        let duration = self.doc.duration;
        (0..starts.len()).map(|at| {
            let (index, start) = starts[at];
            let next = starts.get(at + 1).map(|next| next.1);
            let limit = next.unwrap_or(duration).min(duration).max(start);
            let line = &lines[index];
            let stale = line.stale(voice);
            let texts = line.text.split_whitespace().collect::<Vec<_>>();
            let info = line.take.as_ref().and_then(|take| self.takes.get(take)).filter(|info| info.duration > 0.0);
            let (end, sound_end, words, timed, cut) = match info {
                Some(info) => {
                    let take_end = start + info.duration;
                    let sound_end = take_end.min(limit);
                    let cut = next.is_some_and(|next| take_end > next + 1e-9);
                    let same = info.words.len() == texts.len() && !texts.is_empty();
                    let (words, timed) = if same {
                        // The take's times; the words are always the text's,
                        // so a fixed typo shows at once.
                        let words = texts.iter().zip(&info.words).map(|(text, word)| Word {
                            text: text.to_string(),
                            start: (start + word.start).min(sound_end),
                            end: (start + word.end).min(sound_end),
                        }).collect();
                        (words, true)
                    } else {
                        // Another number of words: spread the text over what
                        // the take speaks.
                        let from = info.words.first().map_or(start, |word| start + word.start).min(sound_end);
                        let to = info.words.last().map_or(sound_end, |word| start + word.end).min(sound_end);
                        (estimate_words(&line.text, from, to.max(from)), false)
                    };
                    let end = words.last().map_or(sound_end, |word: &Word| word.end.max(start)).min(sound_end);
                    let end = if info.words.is_empty() { sound_end } else { end };
                    (end, sound_end, words, timed, cut)
                }
                None => {
                    let end = (start + texts.len() as f64 * ESTIMATED_SECONDS_PER_WORD).min(limit);
                    (end, limit, estimate_words(&line.text, start, end), false, false)
                }
            };
            let provisional = stale || !timed;
            let phones = match info {
                Some(info) if !provisional => info.phones.iter().filter(|p| start + p.start < sound_end).map(|p| Word { text: p.text.clone(), start: start + p.start, end: (start + p.end).min(sound_end) }).collect(),
                _ => Vec::new(),
            };
            PlacedLine { index, start, end, sound_end, take: line.take.clone(), text: line.text.clone(), words, timed, provisional, cut, phones }
        }).collect()
    }

    /// A VoiceOver's lines on the timeline (see [`PlacedLine`]).
    pub fn voice_lines(&self, node: &str) -> &[PlacedLine] {
        self.placed.get(node).map_or(&[], Vec::as_slice)
    }

    /// Where an audio node sounds: its span, or the whole edit.
    fn bounds(&self, node: &CutNode) -> (f64, f64) {
        node.span.map_or((0.0, self.doc.duration), |span| (span.start, span.end()))
    }

    /// Fades and ducking at `t`, the part of the gain that is not
    /// the authored `gain`.
    pub fn shape_gain(&self, node: &CutNode, t: f64) -> f64 {
        let (CutNodeKind::Clip { shape, .. } | CutNodeKind::AudioClip { shape, .. } | CutNodeKind::VoiceOver { shape, .. }) = &node.kind else { return 1.0 };
        let mut gain = 1.0;
        if shape.fade_in > 0.0 || shape.fade_out > 0.0 {
            let bounds = match &node.kind {
                CutNodeKind::VoiceOver { .. } => None,
                _ => Some(self.bounds(node)),
            };
            if let Some((start, end)) = bounds {
                gain *= fade_gain(t, start, end, shape.fade_in, shape.fade_out);
            }
        }
        // Lines never overlap, so the line sounding at `t` is the one; its
        // fades are its own, and a cut take fades out over DECLICK.
        if let CutNodeKind::VoiceOver { .. } = &node.kind {
            let lines = self.voice_lines(&node.id);
            let line = lines.iter().rev().find(|line| line.start <= t && t < line.sound_end);
            gain *= line.map_or(0.0, |line| {
                let fade_out = if line.cut { shape.fade_out.max(DECLICK) } else { shape.fade_out };
                fade_gain(t, line.start, line.sound_end, shape.fade_in, fade_out)
            });
        }
        if let (Some(duck), Some(speech)) = (&shape.duck, self.ducks.get(&node.id)) {
            gain *= duck_gain(duck.depth_db, duck_amount(t, speech, duck.attack, duck.release));
        }
        gain
    }

    /// How far the node's `duck` lowers it at `t`, in dB (0 when it is not
    /// ducked there or has no `duck`): the part of [`Self::shape_gain`] the
    /// voices it ducks under take.
    pub fn duck_db(&self, node: &CutNode, t: f64) -> f64 {
        let (CutNodeKind::Clip { shape, .. } | CutNodeKind::AudioClip { shape, .. } | CutNodeKind::VoiceOver { shape, .. }) = &node.kind else { return 0.0 };
        match (&shape.duck, self.ducks.get(&node.id)) {
            (Some(duck), Some(speech)) => duck.depth_db * duck_amount(t, speech, duck.attack, duck.release).clamp(0.0, 1.0),
            _ => 0.0,
        }
    }

    /// The timed lines a Subtitles node captions, in cut seconds: its
    /// VoiceOver's script, else the transcript of its source mapped
    /// through that source's `in` and speed.
    pub fn spoken_lines(&self, node: &CutNode) -> Vec<SpokenLine> {
        let CutNodeKind::Subtitles { source: Some(source), transcript, .. } = &node.kind else { return Vec::new() };
        let Some(source) = self.node(&source.node) else { return Vec::new() };
        match &source.kind {
            CutNodeKind::VoiceOver { .. } => self.voice_lines(&source.id).iter()
                .filter(|line| !line.words.is_empty())
                .map(|line| SpokenLine { start: line.start, end: line.end, words: line.words.clone() })
                .collect(),
            CutNodeKind::AudioClip { .. } | CutNodeKind::Clip { .. } => {
                let (Some(lines), Some(source_in)) = (transcript.as_ref().and_then(|t| self.transcripts.get(t)), self.source_in(source)) else { return Vec::new() };
                let speed = match &source.kind {
                    CutNodeKind::Clip { speed, .. } => *speed,
                    _ => 1.0,
                };
                let start = source.span.map_or(0.0, |span| span.start);
                let (from, to) = self.bounds(source);
                let map = |at: f64| start + (at - source_in) / speed;
                // Words that overlap the source's span, clamped into it: a
                // word cut by the span shows for its audible part.
                lines.iter().filter_map(|line| {
                    let words = line.words.iter()
                        .map(|word| Word { text: word.text.clone(), start: map(word.start).max(from), end: map(word.end).min(to) })
                        .filter(|word| word.start < to && word.end > from && word.end >= word.start)
                        .collect::<Vec<_>>();
                    (!words.is_empty()).then(|| SpokenLine { start: words[0].start, end: words[words.len() - 1].end, words })
                }).collect()
            }
            _ => Vec::new(),
        }
    }

    /// A Subtitles node's captions, in time order.
    pub fn captions(&self, node: &str) -> &[Caption] {
        self.captions.get(node).map_or(&[], Vec::as_slice)
    }

    /// Every caption on screen at `t`, one per Subtitles node active then
    /// that has one.
    /// Captions of nested cuts whose compound clip says `subtitles: 1`
    /// (every clip on the way) are included, named by their path
    /// (`clip/…/subs`).
    pub fn subtitle_plan(&self, t: f64) -> Vec<SubtitleFrame> {
        let mut frames = self.doc.nodes.iter().filter_map(|node| self.subtitle_frame(node, t)).collect::<Vec<_>>();
        for node in &self.doc.nodes {
            frames.extend(self.nested_subtitles(node, t));
        }
        frames
    }

    /// A compound clip's nested captions at `t`, when it shows them: placed
    /// where the clip shows that point of the nested picture, scaled with
    /// it, then kept inside this cut's safe zone (that of its first
    /// Subtitles node; none without one). A caption the clip's crop cuts
    /// away is not shown.
    fn nested_subtitles(&self, node: &CutNode, t: f64) -> Vec<SubtitleFrame> {
        let (Some((child, tc)), CutNodeKind::Clip { fit, subtitles: true, .. }) = (self.nested_at(node, t), &node.kind) else { return Vec::new() };
        let size = (self.doc.output.width, self.doc.output.height);
        let safe = self.doc.nodes.iter().find_map(|node| match &node.kind {
            CutNodeKind::Subtitles { safe, .. } => Some(*safe),
            _ => None,
        }).unwrap_or_default();
        let child_size = (child.doc.output.width, child.doc.output.height);
        let (src, dst) = fit_rects(child_size, size, *fit);
        let scale = dst.w / src.w.max(1e-9);
        child.subtitle_plan(tc).into_iter().filter_map(|mut frame| {
            let (x, y) = (frame.anchor[0] * child_size.0 as f64, frame.anchor[1] * child_size.1 as f64);
            if x < src.x || x > src.right() || y < src.y || y > src.bottom() {
                return None;
            }
            let placed = [(dst.x + (x - src.x) * scale) / size.0 as f64, (dst.y + (y - src.y) * scale) / size.1 as f64];
            let placement = place_caption(placed, safe, frame.font_size * scale, frame.text.chars().count(), size);
            frame.anchor = placement.anchor;
            frame.font_size = placement.font_size;
            frame.karaoke.safe = placement.safe;
            frame.karaoke.lines = placement.lines;
            frame.node = format!("{}/{}", node.id, frame.node);
            Some(frame)
        }).collect()
    }

    /// One Subtitles node's caption at `t`, if it shows one.
    pub fn subtitle_frame(&self, node: &CutNode, t: f64) -> Option<SubtitleFrame> {
        let size = (self.doc.output.width, self.doc.output.height);
        if !node.active(t) {
            return None;
        }
        let CutNodeKind::Subtitles { anchor, safe, style, font_size, .. } = &node.kind else { return None };
        let captions = self.captions(&node.id);
        // Captions do not overlap: the last one started is the one.
        let index = captions.partition_point(|caption| caption.start <= t).checked_sub(1)?;
        let caption = &captions[index];
        if !caption.visible(t) {
            return None;
        }
        let (active, highlight) = caption.highlight(t);
        let text = caption.text();
        // Strictly inside the safe zone: wrapped, shrunk within a floor and
        // moved as `@movie` subtitles are (`place_caption`).
        let placement = place_caption(*anchor, *safe, font_size.at1(node.local(t)).max(1.0), text.chars().count(), size);
        Some(SubtitleFrame {
            node: node.id.clone(),
            anchor: placement.anchor,
            karaoke: Karaoke {
                caption: index, words: caption.words.iter().map(|word| word.text.clone()).collect(), active, highlight,
                safe: placement.safe, lines: placement.lines,
            },
            text,
            font_size: placement.font_size,
            style: style.clone(),
            provisional: caption.provisional,
        })
    }

    /// A source's pixel size, for fit and crop rectangles.
    pub fn set_source_size(&mut self, media: MediaRef, width: u32, height: u32) {
        for child in self.nested.values_mut() {
            child.set_source_size(media.clone(), width, height);
        }
        self.sizes.insert(media, (width, height));
    }

    /// A compound clip's nested evaluator and its time at `t` (the clip's
    /// source time, `in + (t - start) * speed`), while the clip is active.
    fn nested_at(&self, node: &CutNode, t: f64) -> Option<(&CutEval<'a>, f64)> {
        let edit = node.kind.nested_edit()?;
        if !node.active(t) {
            return None;
        }
        Some((self.nested.get(edit)?, self.source_time(node, t)?))
    }

    /// The gain of an audio plan's segment at `t`: the node's gain, and for
    /// a segment of a nested cut (`node` is the path `clip/…/node`) every
    /// compound clip's gain on the way, each at its own edit's time. Use it
    /// for keyed segments; it equals [`Self::gain`] for this cut's own.
    #[relax(O3, always)]
    pub fn segment_gain(&self, segment: &AudioSegment, t: f64) -> f64 {
        self.path_gain(&segment.node, t)
    }

    /// The pan of a segment at `t`: the node's pan plus every compound
    /// clip's on the way, clamped.
    #[relax(O3, always)]
    pub fn segment_pan(&self, segment: &AudioSegment, t: f64) -> f64 {
        self.path_pan(&segment.node, t).clamp(-1.0, 1.0)
    }

    fn path_gain(&self, path: &str, t: f64) -> f64 {
        match path.split_once('/') {
            Some((clip, rest)) => {
                let Some(node) = self.node(clip) else { return 0.0 };
                let Some((child, tc)) = self.nested_at(node, t) else { return 0.0 };
                self.gain(node, t) * child.path_gain(rest, tc)
            }
            None => self.node(path).map_or(0.0, |node| self.gain(node, t)),
        }
    }

    fn path_pan(&self, path: &str, t: f64) -> f64 {
        match path.split_once('/') {
            Some((clip, rest)) => {
                let Some(node) = self.node(clip) else { return 0.0 };
                let Some((child, tc)) = self.nested_at(node, t) else { return 0.0 };
                self.pan(node, t) + child.path_pan(rest, tc)
            }
            None => self.node(path).map_or(0.0, |node| self.pan(node, t)),
        }
    }

    pub fn node(&self, id: &str) -> Option<&'a CutNode> {
        self.index.get(id).map(|&index| &self.doc.nodes[index])
    }

    pub fn beat(&self, t: f64) -> f64 {
        t * self.doc.bpm / 60.0
    }

    /// Every node active at `t`, in source order.
    pub fn active_nodes(&self, t: f64) -> Vec<&'a CutNode> {
        self.doc.nodes.iter().filter(|node| node.active(t)).collect()
    }

    /// A clip's `in` in source seconds: a marker resolves through the run's
    /// marks, `None` while they are not supplied or lack the name.
    pub fn source_in(&self, node: &CutNode) -> Option<f64> {
        let (source_in, src) = match &node.kind {
            CutNodeKind::Clip { source_in, src, .. } | CutNodeKind::AudioClip { source_in, src, .. } => (source_in, src),
            _ => return None,
        };
        match (source_in, src) {
            (SourceTime::Seconds(seconds), _) => Some(*seconds),
            (SourceTime::Marker { name, offset }, MediaRef::Run(run)) => self.marks.get(run)?.get(name).map(|mark| mark + offset),
            _ => None,
        }
    }

    /// `in + (t - start) * speed`.
    pub fn source_time(&self, node: &CutNode, t: f64) -> Option<f64> {
        let speed = match &node.kind {
            CutNodeKind::Clip { speed, .. } => *speed,
            CutNodeKind::AudioClip { .. } => 1.0,
            _ => return None,
        };
        Some(self.source_in(node)? + node.local(t) * speed)
    }

    /// `source_time` in 10 MHz ticks, rounded: at a frame time `n / fps`
    /// this is at or after the source frame's integer pts, so "the frame at
    /// or before" is exact.
    pub fn source_ticks(&self, node: &CutNode, t: f64) -> Option<i64> {
        let time = self.source_time(node, t)?;
        time.is_finite().then(|| (time * TICKS_PER_SECOND).round() as i64)
    }

    /// An effect's or transition's own clock: `(t - start) * speed`.
    pub fn local_time(&self, node: &CutNode, t: f64) -> f64 {
        let speed = match &node.kind {
            CutNodeKind::Fx { speed, .. } | CutNodeKind::Transition { speed, .. } => *speed,
            _ => 1.0,
        };
        node.local(t) * speed
    }

    /// A transition's mix at `t`: its keys, or 0→1 over its span.
    pub fn mix(&self, node: &CutNode, t: f64) -> f64 {
        let CutNodeKind::Transition { mix, .. } = &node.kind else { return 0.0 };
        match (mix, node.span) {
            (Some(mix), _) => mix.at1(node.local(t)),
            (None, Some(span)) => (t - span.start) / span.length,
            (None, None) => 0.0,
        }
        .clamp(0.0, 1.0)
    }

    pub fn opacity(&self, node: &CutNode, t: f64) -> f64 {
        match &node.kind {
            CutNodeKind::Clip { opacity, .. } | CutNodeKind::Still { opacity, .. } => opacity.at1(node.local(t)).clamp(0.0, 1.0),
            _ => 1.0,
        }
    }

    /// An audio node's gain at `t`: its `gain` with its fades and ducking
    /// (0 when inactive, and for a Clip whose sound is off).
    pub fn gain(&self, node: &CutNode, t: f64) -> f64 {
        if !node.active(t) {
            return 0.0;
        }
        let gain = match &node.kind {
            CutNodeKind::AudioClip { src: MediaRef::MissingOptional(_), .. }
            | CutNodeKind::Clip { src: MediaRef::MissingOptional(_), .. } => return 0.0,
            CutNodeKind::AudioClip { gain, .. } | CutNodeKind::VoiceOver { gain, .. } | CutNodeKind::Clip { audio: true, gain, .. } => gain.at1(node.local(t)).max(0.0),
            _ => return 0.0,
        };
        gain * self.shape_gain(node, t)
    }

    pub fn pan(&self, node: &CutNode, t: f64) -> f64 {
        match &node.kind {
            CutNodeKind::AudioClip { pan, .. } | CutNodeKind::VoiceOver { pan, .. } | CutNodeKind::Clip { pan, .. } => pan.at1(node.local(t)).clamp(-1.0, 1.0),
            _ => 0.0,
        }
    }

    /// The crop a Reframe applies at `t`, in its input's pixels: its keys,
    /// or the largest centred window of its aspect.
    pub fn crop(&self, node: &CutNode, t: f64, input_size: (u32, u32)) -> Option<Rect> {
        let CutNodeKind::Reframe { crop, aspect, .. } = &node.kind else { return None };
        if let Some(crop) = crop {
            return Some(Rect::from_array(crop.at(node.local(t))));
        }
        let aspect = aspect.map_or(self.doc.output.width as f64 / self.doc.output.height as f64, |[w, h]| w / h);
        let (sw, sh) = (input_size.0 as f64, input_size.1 as f64);
        let (w, h) = if sw / sh > aspect { (sh * aspect, sh) } else { (sw, sw / aspect) };
        Some(Rect::new((sw - w) * 0.5, (sh - h) * 0.5, w, h))
    }

    /// The render plan of frame time `t`. A non-finite `t` plans nothing.
    pub fn frame_plan(&self, t: f64) -> FramePlan {
        let size = (self.doc.output.width, self.doc.output.height);
        let mut builder = PlanBuilder { eval: self, t, ops: Vec::new(), memo: HashMap::new(), unresolved: Vec::new() };
        let frame = self.node(&self.doc.output_node).and_then(|node| match &node.kind {
            CutNodeKind::Output { frame, .. } => frame.as_ref(),
            _ => None,
        });
        let result = if t.is_finite() { frame.and_then(|wire| builder.build(&wire.node, size)) } else { None };
        FramePlan { t, beat: self.beat(t), size, ops: builder.ops, result, unresolved: builder.unresolved }
    }

    /// The plan of node `id`'s own picture at `t`, at the output size (a
    /// Generate's input): what the output plan would draw for it.
    pub fn node_plan(&self, id: &str, t: f64) -> FramePlan {
        let size = (self.doc.output.width, self.doc.output.height);
        let mut builder = PlanBuilder { eval: self, t, ops: Vec::new(), memo: HashMap::new(), unresolved: Vec::new() };
        let result = if t.is_finite() { builder.build(id, size) } else { None };
        FramePlan { t, beat: self.beat(t), size, ops: builder.ops, result, unresolved: builder.unresolved }
    }

    /// The audio to mix for `[t0, t1)`. A non-finite or empty window mixes
    /// nothing.
    #[relax(O3, always)]
    pub fn audio_plan(&self, t0: f64, t1: f64) -> AudioPlan {
        let mut segments = Vec::new();
        if !(t0.is_finite() && t1.is_finite() && t0 < t1) {
            return AudioPlan { t0, t1, segments };
        }
        let wires = match self.node(&self.doc.output_node).map(|node| &node.kind) {
            Some(CutNodeKind::Output { audio, .. }) => audio.as_slice(),
            _ => &[],
        };
        // Every sound is in the mix: the output's entries in their
        // order, then the audio sources it does not list, in source order.
        let mut sources = wires.iter().filter_map(|wire| self.node(&wire.node)).collect::<Vec<_>>();
        for node in &self.doc.nodes {
            let sounds = matches!(node.kind, CutNodeKind::AudioClip { .. } | CutNodeKind::VoiceOver { .. } | CutNodeKind::Clip { audio: true, .. });
            if sounds && !sources.iter().any(|source| source.id == node.id) {
                sources.push(node);
            }
        }
        for (entry, node) in sources.into_iter().enumerate() {
            // A compound clip mixes its cut's sound, mapped to this cut's
            // time; its own gain, fades and duck multiply in (segment_gain).
            if let (Some(edit), CutNodeKind::Clip { speed, audio: true, gain, pan, shape, .. }) = (node.kind.nested_edit(), &node.kind) {
                let (Some(child), Some(source_in)) = (self.nested.get(edit), self.source_in(node)) else { continue };
                let (start, end) = node.span.map_or((t0, t1), |span| (t0.max(span.start), t1.min(span.end())));
                if start >= end {
                    continue;
                }
                let clip_start = node.span.map_or(0.0, |span| span.start);
                let back = |tc: f64| clip_start + (tc - source_in) / speed;
                let own_keyed = gain.is_keyed() || pan.is_keyed() || shape.fade_in > 0.0 || shape.fade_out > 0.0 || shape.duck.is_some();
                let (c0, c1) = (source_in + (start - clip_start) * speed, source_in + (end - clip_start) * speed);
                for inner in child.audio_plan(c0, c1).segments {
                    let (p0, p1) = (back(inner.t0).max(start), back(inner.t1).min(end));
                    if p0 >= p1 {
                        continue;
                    }
                    let path = format!("{}/{}", node.id, inner.node);
                    let last = p1 - (p1 - p0) * 1e-9;
                    segments.push(AudioSegment {
                        gain: (self.path_gain(&path, p0), self.path_gain(&path, last)),
                        pan: (self.path_pan(&path, p0).clamp(-1.0, 1.0), self.path_pan(&path, last).clamp(-1.0, 1.0)),
                        node: path,
                        entry,
                        t0: p0,
                        t1: p1,
                        speed: inner.speed * speed,
                        keyed: inner.keyed || own_keyed,
                        ..inner
                    });
                }
                continue;
            }
            let (media, speed, gain, pan, shape) = match &node.kind {
                CutNodeKind::AudioClip { src, gain, pan, shape, .. } => (Some(src), 1.0, gain, pan, shape),
                CutNodeKind::Clip { src, speed, audio: true, gain, pan, shape, .. } => (Some(src), *speed, gain, pan, shape),
                CutNodeKind::VoiceOver { gain, pan, shape, .. } => (None, 1.0, gain, pan, shape),
                _ => continue,
            };
            if matches!(media, Some(MediaRef::MissingOptional(_))) { continue; }
            let keyed = gain.is_keyed() || pan.is_keyed() || shape.fade_in > 0.0 || shape.fade_out > 0.0 || shape.duck.is_some();
            let mut push = |media: MediaRef, start: f64, end: f64, source_t0: Option<f64>, line: Option<usize>| {
                if start >= end {
                    return;
                }
                // Gains at the segment's last instant inside it.
                let last = end - (end - start) * 1e-9;
                segments.push(AudioSegment {
                    node: node.id.clone(),
                    entry,
                    media,
                    t0: start,
                    t1: end,
                    source_t0,
                    source_ticks: source_t0.filter(|time| time.is_finite()).map(|time| (time * TICKS_PER_SECOND).round() as i64),
                    speed,
                    gain: (self.gain(node, start), self.gain(node, last)),
                    pan: (self.pan(node, start), self.pan(node, last)),
                    keyed,
                    line,
                });
            };
            match media {
                Some(media) => {
                    let (start, end) = node.span.map_or((t0, t1), |span| (t0.max(span.start), t1.min(span.end())));
                    push(media.clone(), start, end, self.source_time(node, start), None);
                }
                // Each line's take plays from its start.
                None => {
                    for line in self.voice_lines(&node.id) {
                        let Some(take) = &line.take else { continue };
                        let (start, end) = (t0.max(line.start), t1.min(line.sound_end));
                        push(MediaRef::Asset(take.clone()), start, end, Some(start - line.start), Some(line.index));
                    }
                }
            }
        }
        AudioPlan { t0, t1, segments }
    }

    /// Solver crop keys (`Solution::keys`, in seconds of the clip's source)
    /// as the Reframe's `crop` keys (seconds from its span start, or from 0
    /// without a span). The Reframe's input must be one `Clip`; its `in` is
    /// resolved through the marks, so a marker clip needs `set_marks` first.
    /// Keys before the Reframe's start keep only the last one.
    pub fn reframe_keys(&self, reframe: &str, keys: &[CropKey]) -> Result<Keys<4>, String> {
        let (_, clip_start, source_in, speed, local_start) = self.reframe_clip(reframe)?;
        let mapped = keys.iter().map(|key| KeyPoint {
            t: clip_start + (key.t - source_in) / speed - local_start,
            v: key.crop.to_array(),
            ease: key.ease,
            pinned: key.pinned,
        }).collect::<Vec<_>>();
        let first_inside = mapped.iter().position(|key| key.t >= 0.0).unwrap_or(mapped.len());
        Ok(Keys { points: mapped[first_inside.saturating_sub(1)..].to_vec() })
    }

    /// The inverse of `reframe_keys` for the `@pin` keys of the Reframe's
    /// authored `crop`: the source frames (at the clip's `fps`) they pin,
    /// for `reframe::solve`'s `pinned`.
    pub fn pinned_keys(&self, reframe: &str, fps: f64) -> Result<Vec<crate::reframe::PinnedKey>, String> {
        let (_, clip_start, source_in, speed, local_start) = self.reframe_clip(reframe)?;
        let Some(CutNodeKind::Reframe { crop: Some(Animated::Keys(keys)), .. }) = self.node(reframe).map(|node| &node.kind) else {
            return Ok(Vec::new());
        };
        keys.points.iter().enumerate().filter(|(_, point)| point.pinned).map(|(index, point)| {
            let source = source_in + (point.t + local_start - clip_start) * speed;
            let frame = (source * fps).round();
            if !(frame >= 0.0 && frame.is_finite()) {
                return Err(format!("crop key {index} lies before the clip's source"));
            }
            Ok(crate::reframe::PinnedKey { frame: frame as u64, crop: Rect::from_array(point.v) })
        }).collect()
    }

    /// (clip, clip span start, clip in, clip speed, reframe span start).
    fn reframe_clip(&self, reframe: &str) -> Result<(&'a CutNode, f64, f64, f64, f64), String> {
        let node = self.node(reframe).ok_or_else(|| format!("no node `{reframe}`"))?;
        let CutNodeKind::Reframe { input: Some(input), .. } = &node.kind else { return Err(format!("`{reframe}` is not a wired Reframe")) };
        let clip = self.node(&input.node).ok_or_else(|| format!("no node `{}`", input.node))?;
        let CutNodeKind::Clip { speed, .. } = &clip.kind else { return Err(format!("`{reframe}` does not reframe a Clip")) };
        let source_in = self.source_in(clip).ok_or_else(|| format!("the marker of `{}` is not resolved", clip.id))?;
        let clip_start = clip.span.map_or(0.0, |span| span.start);
        Ok((clip, clip_start, source_in, *speed, node.span.map_or(0.0, |span| span.start)))
    }

    pub(crate) fn source_size(&self, media: &MediaRef) -> Option<(u32, u32)> {
        self.sizes.get(media).copied()
    }
}

struct PlanBuilder<'e, 'a> {
    eval: &'e CutEval<'a>,
    t: f64,
    ops: Vec<DrawOp>,
    /// (node, texture size) → its slot, so a node wired twice draws once.
    memo: HashMap<(String, (u32, u32)), Option<Slot>>,
    unresolved: Vec<String>,
}

impl PlanBuilder<'_, '_> {
    fn push(&mut self, node: &CutNode, size: (u32, u32), kind: DrawKind) -> Option<Slot> {
        self.ops.push(DrawOp { node: node.id.clone(), size, kind });
        Some(self.ops.len() - 1)
    }

    /// The slot holding `id`'s picture at time `t` in a `size` texture.
    fn build(&mut self, id: &str, size: (u32, u32)) -> Option<Slot> {
        let key = (id.to_string(), size);
        if let Some(slot) = self.memo.get(&key) {
            return *slot;
        }
        // A cycle cannot load; the memo entry also stops re-entry.
        self.memo.insert(key.clone(), None);
        let slot = self.build_node(id, size);
        self.memo.insert(key, slot);
        slot
    }

    /// The pixel size a node's picture has of its own: its source's for a
    /// clip or still, passed through effects, the first known one of a
    /// stack's layers or a transition's inputs. What a Reframe crops in.
    fn native_size(&self, id: &str, depth: usize) -> Option<(u32, u32)> {
        let eval = self.eval;
        if depth > eval.doc.nodes.len() + eval.doc.generates.len() {
            return None;
        }
        if eval.doc.graphic(id).is_some() {
            // A graphic is drawn at whatever size it is asked for.
            return None;
        }
        if let Some(gen) = eval.doc.generate(id) {
            let take = gen.chosen().and_then(|take| eval.source_size(&MediaRef::Asset(take.video.clone())));
            return take.or_else(|| gen.input.as_ref().and_then(|wire| self.native_size(&wire.node, depth + 1)));
        }
        let node = eval.node(id)?;
        let wired = |wire: &Option<Wire>| wire.as_ref().and_then(|wire| self.native_size(&wire.node, depth + 1));
        match &node.kind {
            CutNodeKind::Clip { .. } if node.kind.nested_edit().is_some() => {
                let child = eval.nested.get(node.kind.nested_edit()?)?;
                Some((child.doc.output.width, child.doc.output.height))
            }
            CutNodeKind::Clip { src, .. } | CutNodeKind::Still { src, .. } => eval.source_size(src),
            CutNodeKind::Fx { input, .. } => wired(input),
            CutNodeKind::Transition { a, b, .. } => wired(a).or_else(|| wired(b)),
            CutNodeKind::Stack { layers } => layers.iter().find_map(|(wire, _)| self.native_size(&wire.node, depth + 1)),
            CutNodeKind::Reframe { .. } => Some((eval.doc.output.width, eval.doc.output.height)),
            CutNodeKind::Title { .. } | CutNodeKind::AudioClip { .. } | CutNodeKind::VoiceOver { .. } | CutNodeKind::Subtitles { .. } | CutNodeKind::AppView { .. } | CutNodeKind::Camera(_) | CutNodeKind::Code(_) | CutNodeKind::Output { .. } => None,
        }
    }

    /// A Generate: its chosen take stretched over its span, drawn
    /// like a clip's source; its input while it has no take, and outside
    /// its span.
    fn build_generate(&mut self, gen: &crate::generate::GenerateNode, size: (u32, u32)) -> Option<Slot> {
        let eval = self.eval;
        let t = self.t;
        let take = gen.chosen().filter(|_| gen.active(t));
        let Some(take) = take else {
            let input = gen.input.as_ref()?.node.clone();
            return self.build(&input, size);
        };
        let media = MediaRef::Asset(take.video.clone());
        let rects = eval.source_size(&media).map(|source| fit_rects(source, size, gen.fit));
        let time = gen.take_time(take, t);
        let kind = DrawKind::Source {
            media,
            still: false,
            source_time: Some(time),
            source_ticks: crate::generate::ticks(time),
            fit: gen.fit,
            src_rect: rects.map(|rects| rects.0),
            dst_rect: rects.map(|rects| rects.1),
            opacity: 1.0,
        };
        self.ops.push(DrawOp { node: gen.id.clone(), size, kind });
        Some(self.ops.len() - 1)
    }

    fn build_node(&mut self, id: &str, size: (u32, u32)) -> Option<Slot> {
        let eval = self.eval;
        if let Some(gen) = eval.doc.generate(id) {
            return self.build_generate(gen, size);
        }
        if let Some(graphic) = eval.doc.graphic(id) {
            let t = self.t;
            if !graphic.active(t) {
                return None;
            }
            let local = graphic.local(t);
            let kind = DrawKind::Graphic {
                src: graphic.src.clone(),
                params: graphic.params.clone(),
                local_time: local,
                offset: t - local,
                bpm: eval.doc.bpm,
                fit: graphic.fit,
                opacity: graphic.opacity.at1(local).clamp(0.0, 1.0),
            };
            self.ops.push(DrawOp { node: graphic.id.clone(), size, kind });
            return Some(self.ops.len() - 1);
        }
        let node = eval.node(id)?;
        let t = self.t;
        if let (CutNodeKind::Transition { a, b, .. }, Some(span)) = (&node.kind, node.span) {
            // Outside its span a transition is its first input before and
            // its second after, so `Transition{a b span}` over the overlap
            // of two clips plays a, then the transition, then b.
            if t < span.start {
                return self.build(&a.as_ref()?.node, size);
            }
            if t >= span.end() {
                return self.build(&b.as_ref()?.node, size);
            }
        }
        if !node.active(t) {
            return None;
        }
        match &node.kind {
            CutNodeKind::Clip { src: MediaRef::MissingOptional(_), .. } | CutNodeKind::Still { src: MediaRef::MissingOptional(_), .. } => None,
            // A compound clip: the nested cut's plan at the clip's source
            // time, its ops appended (slots shifted, nodes named by path),
            // then its picture placed by `fit` like any source's.
            CutNodeKind::Clip { fit, .. } if node.kind.nested_edit().is_some() => {
                let Some((child, tc)) = eval.nested_at(node, t) else {
                    if !eval.nested.contains_key(node.kind.nested_edit()?) {
                        self.unresolved.push(node.id.clone());
                    }
                    return None;
                };
                let plan = child.frame_plan(tc);
                self.unresolved.extend(plan.unresolved.iter().map(|inner| format!("{}/{inner}", node.id)));
                let result = plan.result?;
                let offset = self.ops.len();
                // The nested cut's captions are not burned into its picture:
                // blanked (an empty Stack keeps the slots), and drawn over
                // the placed picture by this cut when the clip shows them.
                for op in plan.ops {
                    let kind = match op.kind {
                        DrawKind::Text { karaoke: Some(_), .. } => DrawKind::Stack { layers: Vec::new() },
                        kind => shift_slots(kind, offset),
                    };
                    self.ops.push(DrawOp { node: format!("{}/{}", node.id, op.node), size: op.size, kind });
                }
                let (crop, dst_rect) = fit_rects(plan.size, size, *fit);
                let placed = self.push(node, size, DrawKind::Reframe { input: result + offset, crop, dst_rect })?;
                let captions = eval.nested_subtitles(node, t);
                if captions.is_empty() {
                    return Some(placed);
                }
                let mut layers = vec![(placed, Blend::Normal)];
                for frame in captions {
                    self.ops.push(DrawOp {
                        node: frame.node.clone(),
                        size,
                        kind: DrawKind::Text {
                            text: frame.text,
                            position: [frame.anchor[0] * size.0 as f64, frame.anchor[1] * size.1 as f64],
                            font_size: frame.font_size * size.1 as f64 / eval.doc.output.height.max(1) as f64,
                            style: frame.style,
                            local_time: node.local(t),
                            karaoke: Some(frame.karaoke),
                        },
                    });
                    layers.push((self.ops.len() - 1, Blend::Normal));
                }
                self.push(node, size, DrawKind::Stack { layers })
            }
            CutNodeKind::Clip { src, fit, .. } | CutNodeKind::Still { src, fit, .. } => {
                let still = matches!(node.kind, CutNodeKind::Still { .. });
                let rects = eval.source_size(src).map(|source| fit_rects(source, size, *fit));
                let kind = DrawKind::Source {
                    media: src.clone(),
                    still,
                    source_time: if still { None } else { eval.source_time(node, t) },
                    source_ticks: if still { None } else { eval.source_ticks(node, t) },
                    fit: *fit,
                    src_rect: rects.map(|rects| rects.0),
                    dst_rect: rects.map(|rects| rects.1),
                    opacity: eval.opacity(node, t),
                };
                self.push(node, size, kind)
            }
            // A title is the built-in "title" Motion document, at
            // the texture's own size.
            CutNodeKind::Title { text, anchor, font_size, .. } => {
                let local = node.local(t);
                let kind = DrawKind::Graphic {
                    src: crate::graphic::GraphicSrc::Builtin("title".into()),
                    params: crate::graphic::title_params(text, *anchor, font_size.at1(local), size.0, size.1),
                    local_time: local,
                    offset: t - local,
                    bpm: eval.doc.bpm,
                    fit: Fit::None,
                    opacity: 1.0,
                };
                self.push(node, size, kind)
            }
            CutNodeKind::Fx { input, effect, params, .. } => {
                let input = input.as_ref().and_then(|wire| self.build(&wire.node, size));
                let local = node.local(t);
                let kind = DrawKind::Effect {
                    effect: effect.clone(),
                    input,
                    local_time: eval.local_time(node, t),
                    beat: eval.beat(t),
                    params: std::array::from_fn(|i| params[i].at1(local)),
                };
                self.push(node, size, kind)
            }
            CutNodeKind::Transition { a, b, effect, params, effect_asset, .. } => {
                let a = a.as_ref().and_then(|wire| self.build(&wire.node, size));
                let b = b.as_ref().and_then(|wire| self.build(&wire.node, size));
                let local = node.local(t);
                let mix = eval.mix(node, t);
                let kind = DrawKind::Transition {
                    effect: effect.clone(),
                    a,
                    b,
                    mix,
                    local_time: eval.local_time(node, t),
                    beat: eval.beat(t),
                    params: [params[0].at1(local), params[1].at1(local), params[2].at1(local), mix],
                    effect_asset: effect_asset.clone(),
                };
                self.push(node, size, kind)
            }
            CutNodeKind::Stack { layers } => {
                let layers = layers.iter()
                    .filter_map(|(wire, blend)| self.build(&wire.node, size).map(|slot| (slot, *blend)))
                    .collect::<Vec<_>>();
                if layers.is_empty() {
                    return None;
                }
                self.push(node, size, DrawKind::Stack { layers })
            }
            CutNodeKind::Reframe { input, .. } => {
                let input = &input.as_ref()?.node;
                // The input is drawn at its own pixel size so the crop is in
                // source pixels; without that size there is nothing exact
                // to crop, and the plan says so.
                let Some(native) = self.native_size(input, 0) else {
                    self.unresolved.push(node.id.clone());
                    return None;
                };
                let input = self.build(input, native)?;
                let crop = eval.crop(node, t, native)?;
                if !(crop.w > 0.0 && crop.h > 0.0) {
                    return None;
                }
                let (tw, th) = (size.0 as f64, size.1 as f64);
                let aspect = crop.w / crop.h;
                let (w, h) = if aspect > tw / th { (tw, tw / aspect) } else { (th * aspect, th) };
                let dst_rect = Rect::new((tw - w) * 0.5, (th - h) * 0.5, w, h);
                self.push(node, size, DrawKind::Reframe { input, crop, dst_rect })
            }
            CutNodeKind::Subtitles { .. } => {
                let frame = eval.subtitle_frame(node, t)?;
                let kind = DrawKind::Text {
                    text: frame.text,
                    position: [frame.anchor[0] * size.0 as f64, frame.anchor[1] * size.1 as f64],
                    font_size: frame.font_size * size.1 as f64 / eval.doc.output.height.max(1) as f64,
                    style: frame.style,
                    local_time: node.local(t),
                    karaoke: Some(frame.karaoke),
                };
                self.push(node, size, kind)
            }
            CutNodeKind::AppView { app, fit, rect } => self.push(node, size, DrawKind::App { app: app.clone(), fit: *fit, rect: *rect }),
            CutNodeKind::Camera(spec) => self.push(node, size, DrawKind::Camera { spec: spec.clone() }),
            CutNodeKind::Code(spec) => self.push(node, size, DrawKind::Code { label: spec.file.clone(), fit: spec.fit, rect: spec.rect }),
            CutNodeKind::AudioClip { .. } | CutNodeKind::VoiceOver { .. } | CutNodeKind::Output { .. } => None,
        }
    }
}

/// `kind` with every slot it reads moved by `by`.
fn shift_slots(kind: DrawKind, by: usize) -> DrawKind {
    match kind {
        DrawKind::Effect { effect, input, local_time, beat, params } => DrawKind::Effect { effect, input: input.map(|slot| slot + by), local_time, beat, params },
        DrawKind::Transition { effect, a, b, mix, local_time, beat, params, effect_asset } => {
            DrawKind::Transition { effect, a: a.map(|slot| slot + by), b: b.map(|slot| slot + by), mix, local_time, beat, params, effect_asset }
        }
        DrawKind::Stack { layers } => DrawKind::Stack { layers: layers.into_iter().map(|(slot, blend)| (slot + by, blend)).collect() },
        DrawKind::Reframe { input, crop, dst_rect } => DrawKind::Reframe { input: input + by, crop, dst_rect },
        other @ (DrawKind::Source { .. } | DrawKind::Text { .. } | DrawKind::Graphic { .. } | DrawKind::App { .. } | DrawKind::Camera { .. } | DrawKind::Code { .. }) => other,
    }
}

/// Source and destination rectangles placing a `source`-sized picture in a
/// `target`-sized texture (the clip's `fit`). Equal sizes are always 1:1.
/// `fit_rects` into the part `rect` (`[x, y, w, h]`, fractions of `target`)
/// of the target; None is the whole of it.
pub fn place_rects(source: (u32, u32), target: (u32, u32), fit: Fit, rect: Option<[f64; 4]>) -> (Rect, Rect) {
    let Some([x, y, w, h]) = rect else { return fit_rects(source, target, fit) };
    let (tw, th) = (target.0 as f64, target.1 as f64);
    let part = ((tw * w).round().max(1.0) as u32, (th * h).round().max(1.0) as u32);
    let (src, dst) = fit_rects(source, part, fit);
    (src, Rect::new(dst.x + (tw * x).round(), dst.y + (th * y).round(), dst.w, dst.h))
}

pub fn fit_rects(source: (u32, u32), target: (u32, u32), fit: Fit) -> (Rect, Rect) {
    let (sw, sh) = (source.0 as f64, source.1 as f64);
    let (tw, th) = (target.0 as f64, target.1 as f64);
    let full_source = Rect::new(0.0, 0.0, sw, sh);
    let full_target = Rect::new(0.0, 0.0, tw, th);
    if source == target {
        return (full_source, full_target);
    }
    match fit {
        Fit::Fill => {
            let scale = (tw / sw).max(th / sh);
            let (w, h) = (tw / scale, th / scale);
            (Rect::new((sw - w) * 0.5, (sh - h) * 0.5, w, h), full_target)
        }
        Fit::Fit => {
            let scale = (tw / sw).min(th / sh);
            let (w, h) = (sw * scale, sh * scale);
            (full_source, Rect::new((tw - w) * 0.5, (th - h) * 0.5, w, h))
        }
        Fit::None => {
            // 1:1 centred; whatever overhangs the texture is cropped away.
            let (w, h) = (sw.min(tw), sh.min(th));
            (Rect::new((sw - w) * 0.5, (sh - h) * 0.5, w, h), Rect::new((tw - w) * 0.5, (th - h) * 0.5, w, h))
        }
    }
}
