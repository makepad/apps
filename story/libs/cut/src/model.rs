//! The typed edit model: what `CutDoc::load` extracts from an evaluated
//! `kind: @cut` flow. Every value here is a plain copy of the Splash source;
//! the source stays the document's truth.

use makepad_flow::{Graph, Loc};

/// A problem in a cut's source, with the place it was written.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub kind: DiagnosticKind,
    /// 1-based line and column in the cut's own source.
    pub line: u32,
    pub col: u32,
    /// The node the problem belongs to, when it belongs to one.
    pub node: Option<String>,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticKind {
    /// The flow did not evaluate (syntax, types, undeclared fields, cycles
    /// the flow evaluator finds itself).
    Eval,
    /// The `Flow{}` header: kind, output, duration, bpm.
    Header,
    /// A media reference is required and absent.
    MissingRef,
    /// A media reference is not an exact `asset()`/`run()` reference.
    InvalidRef,
    /// A span lies outside the cut, or is not `[start, length]`.
    SpanOutOfRange,
    /// Key times do not strictly increase.
    KeysNotSorted,
    /// A key names an ease that does not exist.
    UnknownEase,
    /// A parameter value has the wrong shape or range.
    InvalidParam,
    /// A transition does not have both of its inputs, or has no timing.
    TransitionInputs,
    /// More clips are active at one time than the decoders allow.
    TooManyClips,
    /// The wiring loops back on itself, or a cut nests itself (directly
    /// or through other edits).
    Cycle,
    /// Edits nest deeper than [`crate::MAX_NEST_DEPTH`].
    NestDepth,
    /// A node that is not part of the cut vocabulary, or a wiring problem.
    Graph,
}

impl Diagnostic {
    pub(crate) fn new(kind: DiagnosticKind, loc: Loc, node: Option<&str>, message: impl Into<String>) -> Self {
        Self { kind, line: loc.line, col: loc.col, node: node.map(str::to_string), message: message.into() }
    }
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.message)
    }
}

/// An exact library revision: `asset(id, rev)`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AssetRef {
    pub id: String,
    pub rev: String,
}

/// An app run script revision and the render (a `Video` asset and revision)
/// the cut pins: `run(id, rev, render_id, render_rev)`. Re-running never
/// moves a cut.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RunRef {
    pub id: String,
    pub rev: String,
    /// The render's `Video` asset id.
    pub render_id: String,
    /// The render's revision.
    pub render: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MediaRef {
    Asset(AssetRef),
    Run(RunRef),
    /// A declared tree entry without content. Retains its node and timing.
    MissingOptional(String),
}

/// A clip's `in:`: seconds, or a marker of the run it plays.
#[derive(Clone, Debug, PartialEq)]
pub enum SourceTime {
    Seconds(f64),
    Marker { name: String, offset: f64 },
}

/// `span: [start, length]` in cut seconds; active on `[start, start + length)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub start: f64,
    pub length: f64,
}

impl Span {
    pub fn end(&self) -> f64 {
        self.start + self.length
    }
    pub fn contains(&self, t: f64) -> bool {
        t >= self.start && t < self.end()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Ease {
    #[default]
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
    /// Keep the previous key's value until this key's time, then jump.
    Hold,
}

impl Ease {
    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "linear" => Self::Linear,
            "ease_in" => Self::EaseIn,
            "ease_out" => Self::EaseOut,
            "ease_in_out" => Self::EaseInOut,
            "hold" => Self::Hold,
            _ => return None,
        })
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::EaseIn => "ease_in",
            Self::EaseOut => "ease_out",
            Self::EaseInOut => "ease_in_out",
            Self::Hold => "hold",
        }
    }

    /// Map the segment fraction `u` (0..1) through the ease: quadratic in,
    /// out and in-out; hold stays at 0 until the key.
    pub fn apply(self, u: f64) -> f64 {
        let u = u.clamp(0.0, 1.0);
        match self {
            Self::Linear => u,
            Self::EaseIn => u * u,
            Self::EaseOut => 1.0 - (1.0 - u) * (1.0 - u),
            Self::EaseInOut => if u < 0.5 { 2.0 * u * u } else { 1.0 - 2.0 * (1.0 - u) * (1.0 - u) },
            Self::Hold => if u >= 1.0 { 1.0 } else { 0.0 },
        }
    }
}

/// One key: at `t` (seconds from the node's span start) the value is `v`;
/// `ease` shapes the segment that arrives at this key from the previous one.
/// `pinned` (`@pin`) marks a key the author placed; solvers keep it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KeyPoint<const N: usize> {
    pub t: f64,
    pub v: [f64; N],
    pub ease: Ease,
    pub pinned: bool,
}

impl<const N: usize> KeyPoint<N> {
    /// An unpinned key.
    pub fn new(t: f64, v: [f64; N], ease: Ease) -> Self {
        Self { t, v, ease, pinned: false }
    }
}

/// `keys([t, v], [t, v, @ease], [t, v, @ease, @pin], [t, v, @pin], …)`,
/// times strictly increasing.
#[derive(Clone, Debug, PartialEq)]
pub struct Keys<const N: usize> {
    pub points: Vec<KeyPoint<N>>,
}

impl<const N: usize> Keys<N> {
    /// The value at `t`: the first key's value before it, the last key's
    /// after it, and the eased interpolation in between.
    pub fn sample(&self, t: f64) -> [f64; N] {
        let points = &self.points;
        let Some(first) = points.first() else { return [0.0; N] };
        // `!(t > first)` also sends NaN to the first key.
        if !(t > first.t) {
            return first.v;
        }
        // Times strictly increase, so the segment is found by bisection.
        let next = points.partition_point(|point| point.t <= t);
        if next >= points.len() {
            return points[points.len() - 1].v;
        }
        let (a, b) = (&points[next - 1], &points[next]);
        let u = b.ease.apply((t - a.t) / (b.t - a.t));
        std::array::from_fn(|i| a.v[i] + (b.v[i] - a.v[i]) * u)
    }
}

/// An animatable parameter: a constant or keys.
#[derive(Clone, Debug, PartialEq)]
pub enum Animated<const N: usize> {
    Const([f64; N]),
    Keys(Keys<N>),
}

impl<const N: usize> Animated<N> {
    /// The value at `local` seconds from the node's span start.
    pub fn at(&self, local: f64) -> [f64; N] {
        match self {
            Self::Const(value) => *value,
            Self::Keys(keys) => keys.sample(local),
        }
    }
    pub fn is_keyed(&self) -> bool {
        matches!(self, Self::Keys(_))
    }
}

impl Animated<1> {
    pub fn scalar(value: f64) -> Self {
        Self::Const([value])
    }
    pub fn at1(&self, local: f64) -> f64 {
        self.at(local)[0]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Fit {
    /// Crop to cover the output.
    #[default]
    Fill,
    /// Letterbox inside the output.
    Fit,
    /// 1:1, centred.
    None,
}

impl Fit {
    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "fill" => Self::Fill,
            "fit" => Self::Fit,
            "none" => Self::None,
            _ => return None,
        })
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Fill => "fill",
            Self::Fit => "fit",
            Self::None => "none",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Blend {
    #[default]
    Normal,
    Add,
    Screen,
    Multiply,
}

impl Blend {
    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "normal" => Self::Normal,
            "add" => Self::Add,
            "screen" => Self::Screen,
            "multiply" => Self::Multiply,
            _ => return None,
        })
    }
}

/// An effect: a bundled preset by name, or a library `VjEffect` revision.
#[derive(Clone, Debug, PartialEq)]
pub enum EffectRef {
    Preset(String),
    Asset(AssetRef),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ReframeMode {
    /// The solver writes `crop` from the focus track.
    #[default]
    Auto,
    /// `crop` is authored; the solver leaves it alone.
    Keys,
}

pub use crate::reframe::SafeZone;

/// An upstream node output wired into an input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wire {
    pub node: String,
    pub port: String,
}

/// How an audio source enters the mix besides gain and pan:
/// linear fades at its ends and ducking under other sources.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct AudioShape {
    /// Seconds the source takes to rise from silence at its start (a
    /// VoiceOver: at each line's start).
    pub fade_in: f64,
    /// Seconds the source takes to fall to silence at its end.
    pub fade_out: f64,
    pub duck: Option<Duck>,
}

/// `duck: {under: [@voice…], depth_db: -12, attack: 0.08, release: 0.4}`:
/// this source drops by `depth_db` while any node in `under` speaks. The
/// drop starts `attack` seconds before the speech so it is complete when
/// the first word lands, and recovers over `release` after it ends.
#[derive(Clone, Debug, PartialEq)]
pub struct Duck {
    /// Node ids of the sources this one ducks under.
    pub under: Vec<String>,
    /// Gain change while ducked, in dB (negative).
    pub depth_db: f64,
    pub attack: f64,
    pub release: f64,
}

impl Default for Duck {
    fn default() -> Self {
        Self { under: Vec::new(), depth_db: -12.0, attack: 0.08, release: 0.4 }
    }
}

/// The TTS engine of a voiceover.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum VoiceEngine {
    /// Kokoro-82M (Apache-2.0, 24 kHz, English voices). The default.
    #[default]
    Kokoro,
    /// IndexTTS: voice cloning from a reference asset. Opt-in; its weights
    /// carry a restricted licence.
    IndexTts,
}

impl VoiceEngine {
    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "kokoro" => Self::Kokoro,
            "indextts" => Self::IndexTts,
            _ => return None,
        })
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Kokoro => "kokoro",
            Self::IndexTts => "indextts",
        }
    }
}

/// `voice: {engine: @kokoro id: "bm_daniel" speed: 1.0}`.
#[derive(Clone, Debug, PartialEq)]
pub struct VoiceSpec {
    pub engine: VoiceEngine,
    /// The engine's voice name (a Kokoro voice pack such as `bm_daniel`).
    pub id: String,
    /// Speaking rate; 1 is the voice's own.
    pub speed: f64,
    /// IndexTTS: the reference recording the voice is cloned from.
    pub reference: Option<AssetRef>,
}

impl Default for VoiceSpec {
    fn default() -> Self {
        Self { engine: VoiceEngine::Kokoro, id: "bm_daniel".into(), speed: 1.0, reference: None }
    }
}

impl VoiceSpec {
    /// The language the voice speaks (a whisper code): Kokoro names its
    /// voices by language, `a`/`b` English, `e` Spanish, `f` French, `h`
    /// Hindi, `i` Italian, `j` Japanese, `p` Portuguese, `z` Chinese.
    /// Anything else, and IndexTTS, is English.
    pub fn language(&self) -> &'static str {
        match (self.engine, self.id.as_bytes().first()) {
            (VoiceEngine::Kokoro, Some(b'e')) => "es",
            (VoiceEngine::Kokoro, Some(b'f')) => "fr",
            (VoiceEngine::Kokoro, Some(b'h')) => "hi",
            (VoiceEngine::Kokoro, Some(b'i')) => "it",
            (VoiceEngine::Kokoro, Some(b'j')) => "ja",
            (VoiceEngine::Kokoro, Some(b'p')) => "pt",
            (VoiceEngine::Kokoro, Some(b'z')) => "zh",
            _ => "en",
        }
    }
}

/// The key of a take of `text` in `voice`: equal keys mean the same
/// speech, so a line whose take was made under another key is stale.
/// Whitespace runs count as one space; a stable 64-bit FNV-1a, so
/// keys written into sources stay valid across builds.
pub fn take_key(voice: &VoiceSpec, text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let reference = voice.reference.as_ref().map_or(String::new(), |r| format!("{}:{}", r.id, r.rev));
    let canonical = format!("v1\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}", voice.engine.id(), voice.id, voice.speed, reference, text);
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in canonical.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("tk1-{hash:016x}")
}

/// `line(text, at, take, key)`: one spoken line of a voiceover.
#[derive(Clone, Debug, PartialEq)]
pub struct VoiceLine {
    /// The script: the source of truth for the take and the subtitles.
    pub text: String,
    /// Edit seconds, or a marker of the voiceover's run.
    pub at: SourceTime,
    /// The generated take (an `Audio` asset), once there is one.
    pub take: Option<AssetRef>,
    /// The [`take_key`] the take was generated under.
    pub key: Option<String>,
}

impl VoiceLine {
    /// Whether the line needs (re)generating in `voice`: no take, or a take
    /// made for other text or another voice.
    pub fn stale(&self, voice: &VoiceSpec) -> bool {
        self.take.is_none() || self.key.as_deref() != Some(take_key(voice, &self.text).as_str())
    }
}

/// A word and when it is spoken, in the seconds of whatever it is timed
/// against (a take, a source, or the cut).
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

/// A line of timed words: a voiceover line or a transcript line.
#[derive(Clone, Debug, PartialEq)]
pub struct SpokenLine {
    pub start: f64,
    pub end: f64,
    pub words: Vec<Word>,
}

impl SpokenLine {
    pub fn text(&self) -> String {
        self.words.iter().map(|word| word.text.as_str()).collect::<Vec<_>>().join(" ")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum CutNodeKind {
    /// `audio`: whether the video's soundtrack is mixed (`Clip{audio: …}`;
    /// on by default for library videos, off for app runs). `nested`: `src`
    /// is `cut(id, rev)`, a compound clip whose picture and sound are
    /// that cut's, evaluated at the clip's source time; `src` is then
    /// `MediaRef::Asset` of the cut revision (see
    /// [`CutNodeKind::nested_edit`]). `subtitles`: a compound clip shows its
    /// edit's own captions (off by default), placed by this cut.
    Clip { src: MediaRef, source_in: SourceTime, speed: f64, fit: Fit, opacity: Animated<1>, audio: bool, gain: Animated<1>, pan: Animated<1>, shape: AudioShape, nested: bool, subtitles: bool },
    Still { src: MediaRef, fit: Fit, opacity: Animated<1> },
    /// `anchor` is in output fractions (0..1).
    Title { text: String, anchor: [f64; 2], font_size: Animated<1>, style: String },
    /// `params` are p1..p4; below 0 leaves the effect document's value, as
    /// in the live renderer.
    Fx { input: Option<Wire>, effect: EffectRef, params: [Animated<1>; 4], speed: f64 },
    /// `mix` is `None` when it ramps 0→1 over the span.
    /// `effect` is a bundled transition preset by name (empty: a
    /// dissolve), or empty with `effect_asset` set when the source wrote
    /// `effect: asset(id, rev)` of a library VjEffect.
    Transition { a: Option<Wire>, b: Option<Wire>, effect: String, params: [Animated<1>; 3], speed: f64, mix: Option<Animated<1>>, effect_asset: Option<AssetRef> },
    /// Layers bottom first, each with its blend.
    Stack { layers: Vec<(Wire, Blend)> },
    /// `aspect` is `None` for the output's aspect. `crop` is `[x, y, w, h]`
    /// in source pixels of the input. An `@auto` Reframe without `focus`
    /// (and before any solve) shows the largest centred window.
    Reframe { input: Option<Wire>, aspect: Option<[f64; 2]>, crop: Option<Animated<4>>, mode: ReframeMode, focus: Option<MediaRef>, safe: SafeZone },
    AudioClip { src: MediaRef, source_in: SourceTime, gain: Animated<1>, pan: Animated<1>, shape: AudioShape },
    /// Spoken lines, each with its own take placed at the line's start.
    /// `run` is the app run whose markers the lines' `at` use.
    VoiceOver { voice: VoiceSpec, lines: Vec<VoiceLine>, run: Option<RunRef>, gain: Animated<1>, pan: Animated<1>, shape: AudioShape },
    /// Captions of `source`: a VoiceOver's own script and timings,
    /// else `transcript` (a text asset of the source's words). `anchor` is
    /// in output fractions and is kept inside `safe`.
    Subtitles { source: Option<Wire>, style: String, anchor: [f64; 2], safe: SafeZone, max_words: u32, transcript: Option<AssetRef>, language: String, font_size: Animated<1> },
    /// A live app a deck hosts (`apps` of the slide, by name),
    /// placed in the frame by `fit`. An edit never launches anything: the
    /// renderer draws the picture its host hands it for this node, or a
    /// placeholder naming the app.
    AppView { app: String, fit: Fit, rect: Option<[f64; 4]> },
    /// A live camera a deck hosts (a document camera, a phone under it) as a
    /// layer. The host opens the device only while the slide is prepared and
    /// streams it only while the slide shows; an edit never opens a device.
    /// `crop`, `rotate`, `mirror` and `adjust` are done on the GPU as the
    /// picture is placed by `fit`.
    Camera(CameraSpec),
    /// A source file of a live app, drawn with the code editor (syntax
    /// colours, a caret and a soft highlight while a demo edit types). DECK
    /// draws it; elsewhere it is a placeholder naming the file.
    Code(CodeSpec),
    Output { frame: Option<Wire>, audio: Vec<Wire> },
}

/// A `Code` node's fields.
#[derive(Clone, Debug, PartialEq)]
pub struct CodeSpec {
    /// The name of the slide's `apps` entry the file belongs to.
    pub app: String,
    /// The file, relative to the workspace root.
    pub file: String,
    /// The first and last line shown (1-based, inclusive); None: the whole
    /// file.
    pub lines: Option<(u32, u32)>,
    /// The picture's size in pixels, which `fit` places in the output.
    pub size: (u32, u32),
    /// The text size in pixels of that picture.
    pub font_size: f64,
    pub fit: Fit,
    /// Where in the output the picture goes, `[x, y, w, h]` in fractions of
    /// it (`fit` then places it in that rectangle); None: the whole output.
    pub rect: Option<[f64; 4]>,
}

/// Which camera a `Camera` node shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum CameraDevice {
    /// The first camera the system lists.
    #[default]
    First,
    /// The camera at this position in the system's list.
    Index(u32),
    /// The first camera whose name contains this text (any case); the first
    /// camera when none does.
    Name(String),
}

impl CameraDevice {
    /// The camera among `names` (the system's list, in order) this picks.
    pub fn pick<'n>(&self, names: impl IntoIterator<Item = &'n str>) -> Option<usize> {
        let mut names = names.into_iter();
        match self {
            Self::First => names.next().map(|_| 0),
            Self::Index(at) => names.nth(*at as usize).map(|_| *at as usize),
            Self::Name(wanted) => {
                let wanted = wanted.to_lowercase();
                let names: Vec<&str> = names.collect();
                names.iter().position(|name| name.to_lowercase().contains(&wanted)).or_else(|| (!names.is_empty()).then_some(0))
            }
        }
    }

    /// How the device is written in a source and shown in a placeholder.
    pub fn label(&self) -> String {
        match self {
            Self::First => "first camera".into(),
            Self::Index(at) => format!("camera {at}"),
            Self::Name(name) => name.clone(),
        }
    }
}

/// The picture corrections of a `Camera` node. The identity is
/// `{brightness: 0 contrast: 1 saturation: 1}`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraAdjust {
    /// Added to every channel, -1..1.
    pub brightness: f64,
    /// Scales around mid grey, 0..4.
    pub contrast: f64,
    /// Scales the distance from the grey of the pixel, 0..4.
    pub saturation: f64,
}

impl Default for CameraAdjust {
    fn default() -> Self {
        CameraAdjust { brightness: 0.0, contrast: 1.0, saturation: 1.0 }
    }
}

/// A `Camera` node's fields.
#[derive(Clone, Debug, PartialEq)]
pub struct CameraSpec {
    pub device: CameraDevice,
    /// The capture size and rate asked for (the nearest the camera offers
    /// is used).
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// The part of the picture shown, `[x, y, w, h]` in fractions of it.
    pub crop: [f64; 4],
    /// Clockwise turn of the (cropped) picture: 0, 90, 180 or 270 degrees.
    pub rotate: u32,
    /// Flip left to right (after the turn).
    pub mirror: bool,
    pub adjust: CameraAdjust,
    pub fit: Fit,
    /// Where the picture goes, `[x, y, w, h]` as fractions of the output; `None`: all of it.
    /// `fit` places the picture inside it, as for an `AppView`.
    pub rect: Option<[f64; 4]>,
}

impl Default for CameraSpec {
    fn default() -> Self {
        CameraSpec { device: CameraDevice::First, width: 1280, height: 720, fps: 30.0, crop: [0.0, 0.0, 1.0, 1.0], rotate: 0, mirror: false, adjust: CameraAdjust::default(), fit: Fit::Fit, rect: None }
    }
}

impl CutNodeKind {
    /// The effect an `Fx` or `Transition` renders: a bundled preset by name
    /// or a library `VjEffect` revision. `None` for other nodes.
    pub fn effect_ref(&self) -> Option<EffectRef> {
        match self {
            Self::Fx { effect, .. } => Some(effect.clone()),
            Self::Transition { effect_asset: Some(asset), .. } => Some(EffectRef::Asset(asset.clone())),
            Self::Transition { effect, .. } => Some(EffectRef::Preset(effect.clone())),
            _ => None,
        }
    }

    /// The cut revision a compound `Clip{src: cut(id, rev)}` nests.
    pub fn nested_edit(&self) -> Option<&AssetRef> {
        match self {
            Self::Clip { src: MediaRef::Asset(edit), nested: true, .. } => Some(edit),
            _ => None,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Clip { .. } => "Clip",
            Self::Still { .. } => "Still",
            Self::Title { .. } => "Title",
            Self::Fx { .. } => "Fx",
            Self::Transition { .. } => "Transition",
            Self::Stack { .. } => "Stack",
            Self::Reframe { .. } => "Reframe",
            Self::AudioClip { .. } => "AudioClip",
            Self::VoiceOver { .. } => "VoiceOver",
            Self::Subtitles { .. } => "Subtitles",
            Self::AppView { .. } => "AppView",
            Self::Camera(_) => "Camera",
            Self::Code(_) => "Code",
            Self::Output { .. } => "CutOutput",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CutNode {
    pub id: String,
    /// `None`: always active.
    pub span: Option<Span>,
    pub track: u32,
    /// 1-based, like diagnostics (the graph's own node locations are the
    /// script's 0-based row and column).
    pub loc: Loc,
    pub kind: CutNodeKind,
}

impl CutNode {
    /// Whether the node is active at `t`; never at a non-finite `t`.
    pub fn active(&self, t: f64) -> bool {
        t.is_finite() && self.span.map_or(true, |span| span.contains(t))
    }
    /// Seconds since the span start (the time keys are written in).
    pub fn local(&self, t: f64) -> f64 {
        t - self.span.map_or(0.0, |span| span.start)
    }
}

/// `output: {width height fps}`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct OutputFormat {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// `loudness: -14`: the integrated loudness (LUFS) the mix is
    /// normalized to; `None` leaves the mix as authored.
    pub loudness: Option<f64>,
    /// `alpha: true`: the picture keeps its transparency (an overlay: a
    /// lower third, a title over other footage) and is written as HEVC
    /// with alpha, where the OS encoder writes it.
    pub alpha: bool,
    /// `matrix: @bt709 | @bt601 | @bt2020` (default BT.709): the colour the
    /// export is written and tagged in. BT.601 is the SD (525 line)
    /// colour, BT.2020 the wide gamut (SDR): the picture's primaries are
    /// converted to it.
    pub matrix: OutputMatrix,
    /// `range: @limited | @full` (default limited, the video range).
    pub range: OutputRange,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutputMatrix {
    #[default]
    Bt709,
    Bt601,
    Bt2020,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutputRange {
    #[default]
    Limited,
    Full,
}

/// One `marker(@name, offset)` a clip's `in:` uses.
#[derive(Clone, Debug, PartialEq)]
pub struct MarkerUse {
    pub node: String,
    pub run: RunRef,
    pub name: String,
    pub offset: f64,
}

/// A loaded edit: header, typed nodes in source order, and the flow graph
/// they came from (the graph canvas and the timeline edit the same one).
#[derive(Clone, Debug)]
pub struct CutDoc {
    pub source: String,
    /// Declared absence in the loaded tree; informational, never a load error.
    pub missing_media: Vec<String>,
    pub label: String,
    pub output: OutputFormat,
    pub duration: f64,
    pub bpm: f64,
    pub nodes: Vec<CutNode>,
    /// The id of the `CutOutput` node.
    pub output_node: String,
    /// The graph FlowVm evaluated, differing from a plain flow load in
    /// three ways: the loader's `CutMetadata` carrier node is removed, node
    /// locations (0-based, as in every flow graph) are in the cut's own
    /// source, and plain `Stack.layers` entries carry the ids the loader gave
    /// them (listed in `synthetic_layers`; the source has no `conn()` there).
    pub graph: Graph,
    /// `(Stack node id, entry id)` of every layer entry the source wrote
    /// without `conn(id, …)`. A graph → source patcher writes those entries
    /// back plain. Synthetic ids lie above every authored id of that Stack.
    pub synthetic_layers: Vec<(String, u64)>,
    /// The `Generate` nodes, in source order. They are
    /// kept apart from `nodes` so every match over [`CutNodeKind`] keeps
    /// compiling; wires name them like any node, and the frame plan,
    /// [`crate::dependencies`] and the graph include them.
    pub generates: Vec<crate::generate::GenerateNode>,
    /// The `Graphic` nodes, in source order, kept apart
    /// from `nodes` like `generates`.
    pub graphics: Vec<crate::graphic::GraphicNode>,
    /// Spans written in musical or spoken time; `with_timing`
    /// re-places them against the music's grid and the voice timings.
    pub time_bindings: Vec<crate::timing::TimeBinding>,
}

impl CutDoc {
    /// The time of the frame shown at `t`: from its end on, a cut shows (holds)
    /// its last frame, so what is on it then is what that frame has.
    pub fn shown_time(&self, t: f64) -> f64 {
        if t < self.duration { t } else { (self.duration - 1.0 / self.output.fps.max(1.0)).max(0.0) }
    }

    pub fn node(&self, id: &str) -> Option<&CutNode> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// The cut as a render of it draws it (the CUT page's render settings):
    /// at `fps` (its own when `None`), its mix normalised to `loudness`
    /// (`None`: the mix as authored, whatever the cut names), and, unless
    /// `burn`, without its Subtitles in the picture (their sidecars stay).
    /// Values land on the cut's number grid, as a written cut holds them.
    pub fn render_as(&mut self, fps: Option<f64>, loudness: Option<f64>, burn: bool) {
        let grid = |value: f64| (value * 1e6).round() / 1e6;
        self.output.fps = grid(fps.unwrap_or(self.output.fps));
        self.output.loudness = loudness.map(grid);
        if burn {
            return;
        }
        // The picture's Stack: what the output shows.
        let Some(CutNodeKind::Output { frame: Some(frame), .. }) = self.node(&self.output_node).map(|node| &node.kind) else { return };
        let stack = frame.node.clone();
        let subtitles: Vec<String> = self.nodes.iter().filter(|node| matches!(node.kind, CutNodeKind::Subtitles { .. })).map(|node| node.id.clone()).collect();
        let shown = |node: &str, port: &str| port == "frame" && subtitles.iter().any(|id| id == node);
        let Some(CutNodeKind::Stack { layers }) = self.nodes.iter_mut().find(|node| node.id == stack).map(|node| &mut node.kind) else { return };
        layers.retain(|(wire, _)| !shown(&wire.node, &wire.port));
        if let Some(node) = self.graph.nodes.iter_mut().find(|node| node.id == stack) {
            for input in &mut node.inputs {
                if let makepad_flow::wire::NodeInputValue::Edges(entries) = &mut input.value {
                    entries.retain(|entry| !shown(&entry.from.from_node, &entry.from.from_port));
                }
            }
        }
        self.graph.edges.retain(|edge| edge.to_node != stack || !shown(&edge.from_node, &edge.from_port));
    }

    /// A `Graphic` node by id.
    pub fn graphic(&self, id: &str) -> Option<&crate::graphic::GraphicNode> {
        self.graphics.iter().find(|graphic| graphic.id == id)
    }

    /// A `Generate` node by id.
    pub fn generate(&self, id: &str) -> Option<&crate::generate::GenerateNode> {
        self.generates.iter().find(|gen| gen.id == id)
    }

    pub fn node_index(&self, id: &str) -> Option<usize> {
        self.nodes.iter().position(|node| node.id == id)
    }

    /// Every `marker()` cut point, in node order: clips' `in` and, with
    /// their voiceover's run, voice lines' `at`.
    pub fn markers(&self) -> Vec<MarkerUse> {
        let mut out = Vec::new();
        for node in &self.nodes {
            match &node.kind {
                CutNodeKind::Clip { src: MediaRef::Run(run), source_in: SourceTime::Marker { name, offset }, .. }
                | CutNodeKind::AudioClip { src: MediaRef::Run(run), source_in: SourceTime::Marker { name, offset }, .. } => {
                    out.push(MarkerUse { node: node.id.clone(), run: run.clone(), name: name.clone(), offset: *offset });
                }
                CutNodeKind::VoiceOver { lines, run: Some(run), .. } => {
                    for line in lines {
                        if let SourceTime::Marker { name, offset } = &line.at {
                            out.push(MarkerUse { node: node.id.clone(), run: run.clone(), name: name.clone(), offset: *offset });
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// Every compound clip: `(node id, the cut revision it pins)`.
    pub fn nested_edits(&self) -> Vec<(String, AssetRef)> {
        self.nodes.iter().filter_map(|node| node.kind.nested_edit().map(|edit| (node.id.clone(), edit.clone()))).collect()
    }

    /// Compound clips whose pinned edit revision is no longer that cut's
    /// head ("edit changed"; the clip moves only on Update):
    /// `(node id, pinned, head revision)`. `head_of` answers a cut asset
    /// id's current head revision, `None` when unknown (not reported).
    pub fn stale_nested(&self, head_of: &dyn Fn(&str) -> Option<String>) -> Vec<(String, AssetRef, String)> {
        self.nested_edits().into_iter().filter_map(|(node, edit)| {
            let head = head_of(&edit.id)?;
            (head != edit.rev).then_some((node, edit, head))
        }).collect()
    }

    /// Every edge of the graph, as `(from node, from port, to node, to port)`.
    pub fn wiring(&self) -> impl Iterator<Item = (&str, &str, &str, &str)> {
        self.graph.edges.iter().map(|edge| (edge.from_node.as_str(), edge.from_port.as_str(), edge.to_node.as_str(), edge.to_port.as_str()))
    }
}
