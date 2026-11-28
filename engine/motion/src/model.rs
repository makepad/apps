//! The typed Motion document: what [`MotionDoc::load`] reads out of a
//! `Motion{...}` Splash value. Every value here is a plain copy of the
//! source's; the source stays the document's truth (EDITS.md §1).

use crate::plain::{EllipseProps, GroupProps, PathProps, RectProps, RepeatProps, TextProps};
use crate::{Ease, LoadedLayers};
use crate::features::AudioFeatures;
use crate::signals::{LiveSignals, SignalSource};
use crate::timing::Timing;
use makepad_script::{LiveId, ScriptFnImage};
use std::sync::Arc;

pub use makepad_motion_plain::model::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub enum Fit {
    /// Crop to cover the box.
    Fill,
    /// Letterbox inside the box.
    #[default]
    Fit,
    /// Stretch to the box.
    Stretch,
    /// Natural size, centred in the box.
    None,
}

impl Fit {
    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "fill" => Self::Fill,
            "fit" => Self::Fit,
            "stretch" => Self::Stretch,
            "none" => Self::None,
            _ => return None,
        })
    }
}

/// A range's axis target and the font's default when Text does not set it.
#[derive(Clone, Debug, PartialEq)]
pub struct TextRangeAxis {
    pub tag: String,
    pub value: Prop<1>,
    pub default: f64,
}

/// A window over text units. Every property uses the layer's clock, after
/// the units' staggered animation. Coarser selections share a weight among
/// all units in the containing word or explicit line.
#[derive(Clone, Debug, PartialEq)]
pub struct TextRange {
    pub loc: Loc,
    pub path: String,
    pub by: TextBy,
    pub from: Prop<1>,
    pub to: Prop<1>,
    pub slide: Prop<1>,
    pub soft: Prop<1>,
    pub ease: Ease,
    pub invert: bool,
    pub x: Option<Prop<1>>,
    pub y: Option<Prop<1>>,
    pub rotate: Option<Prop<1>>,
    pub blur: Option<Prop<1>>,
    pub scale: Option<Prop<2>>,
    pub opacity: Option<Prop<1>>,
    pub color: Option<Prop<4>>,
    pub axes: Vec<TextRangeAxis>,
}

impl TextRange {
    pub fn is_animated(&self) -> bool {
        [&self.from, &self.to, &self.slide, &self.soft].into_iter().any(Prop::is_animated)
            || [&self.x, &self.y, &self.rotate, &self.blur, &self.opacity].into_iter().any(|p| p.as_ref().is_some_and(Prop::is_animated))
            || self.scale.as_ref().is_some_and(Prop::is_animated)
            || self.color.as_ref().is_some_and(Prop::is_animated)
            || self.axes.iter().any(|a| a.value.is_animated())
    }
}

/// Text's source: its own string, or a voiceover's words.
#[derive(Clone, Debug, PartialEq)]
pub enum TextSource {
    Text(String),
    /// `source: voice(@vo)`: the voiceover's words as the host's audio
    /// features give them.
    Voice(String),
    /// `number: tween(0, 1234, 0, 2)`: a number counting, formatted with
    /// `decimals`, `grouping` (a thousands separator), `prefix` and `suffix`.
    Number { value: Prop<1>, decimals: u32, grouping: Option<String>, prefix: String, suffix: String, tabular: bool, clock: Option<Clock>, format: TimeFormat, log: bool, wall: bool },
}

/// `number: @time | @doc | @edit`: the number is a clock (added to
/// `number`'s value): the layer's own seconds, the document's, or the
/// host edit's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Clock {
    Layer,
    Doc,
    Edit,
}

/// How a number reads as a time (`format:`); `Plain` is a number.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum TimeFormat {
    #[default]
    Plain,
    /// `00:00:00:12` (hours, minutes, seconds, frames at the document's fps).
    Timecode,
    /// `45:10` (minutes and seconds).
    MmSs,
    /// `01:45:10`.
    HhMmSs,
    /// `3.5 / 15.0 s` with `total`, else `3.5 s` (`decimals` digits).
    Seconds { total: Option<f64> },
    /// A number that outgrows its notation: grouped (1,234,567), then named
    /// (12.4 million … 1 decillion), googol, then scientific (1.0e42), ∞, NaN.
    Escalate,
}

/// `plate: #111 plate_pad: vec2(24, 12) plate_radius: 4 …`: a plate behind
/// a Text's lines (one per line, hugging it; at least as wide as it is
/// tall), or behind every unit with `plate_each`.
#[derive(Clone, Debug, PartialEq)]
pub struct Plate {
    pub color: Prop<4>,
    pub pad: [f64; 2],
    pub radius: f64,
    pub stroke: Option<Prop<4>>,
    pub width: f64,
    /// 0..1: how much of the plate shows, wiping open left to right.
    pub reveal: Prop<1>,
    pub each: bool,
    /// `plate_style: @underline`: a band under each line instead of a box
    /// behind it.
    pub underline: bool,
}

/// A voice-bound Text's behaviours (§10.3).
#[derive(Clone, Debug, PartialEq)]
pub struct VoiceStyle {
    /// Which words show: `@all`, `@spoken` (as they are said), `@active`
    /// (only the one being said), `@line` (the words of the line being said).
    pub show: VoiceShow,
    /// `pop: @on_word`: a word jumps in scale when it is said.
    pub pop: bool,
    /// `highlight: @active` / `@spoken`: words take `highlight_color`.
    pub highlight: Option<VoiceShow>,
    pub highlight_color: Color,
    /// `stretch: @vowels`: long syllables stretch their word horizontally.
    pub stretch: bool,
    /// `wave: @pitch`: words rise and fall with the voice's pitch.
    pub wave: bool,
    /// `ghost: #ffffff40`: words not yet said show in this colour (the
    /// ghost → current (highlight) → said (the text's colour) states).
    pub ghost: Option<Color>,
    /// `line: 3`: only that line of the voice (a set per lyric line).
    pub line: Option<u32>,
    /// `sweep: true`: the current word fills letter by letter over its
    /// sung time (with `by: @letter`), instead of all at once.
    pub sweep: bool,
    /// `karaoke: {sung cooled unsung anticipate cool by dir}`: every letter
    /// (with `by: @letter`) or word goes unsung → sung → cooled at its own
    /// time within its word, never before the word starts.
    pub karaoke: Option<Karaoke>,
}

/// Karaoke state colours (PDOOM-PARITY F6).
#[derive(Clone, Debug, PartialEq)]
pub struct Karaoke {
    /// Unsung colour; `None`: the text's own.
    pub unsung: Option<Color>,
    pub sung: Color,
    pub cooled: Color,
    /// Seconds of lead-in during which an unsung letter warms toward `sung`.
    pub anticipate: f64,
    /// Seconds from sung to cooled.
    pub cool: f64,
    /// `by: @syllable`: letters fill by the word's syllable times.
    pub syllables: bool,
    /// `dir: @rtl`: the fill runs from a word's last letter to its first.
    pub rtl: bool,
}

impl Default for VoiceStyle {
    fn default() -> Self {
        Self { show: VoiceShow::All, pop: false, highlight: None, highlight_color: [1.0, 0.82, 0.25, 1.0], stretch: false, wave: false, ghost: None, line: None, sweep: false, karaoke: None }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VoiceShow {
    All,
    Spoken,
    Active,
    Line,
}

/// The moment a `Freeze` shows: one (`16.2`, `beat(32)`), or one per frame
/// (`fn(t) {…}`, keys: seconds of the document, read on the layer's clock).
#[derive(Clone, Debug, PartialEq)]
pub enum FreezeTime {
    Moment(Time),
    Moving(Prop<1>),
}

/// Where an image, SVG, video or model comes from: an exact library
/// revision, an input the host fills (`in(@logo)`), or (SVG only) markup
/// written in the document.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Media {
    /// `src: @backdrop`: what is under the layer in the document (an
    /// Image magnifies it with `zoom` about `focus`).
    Backdrop,
    Asset(AssetRef),
    Inline(Arc<str>),
    /// An input of an asset type left empty: draws nothing.
    Empty,
}

/// A Shader layer's program.
/// An inline Shader layer's `data: [f, f, …]`: a flat list of floats the
/// pixel function reads from a texture (`self.data_float(i)`,
/// `self.data_vec4(i)`), for tables too large to write as shader
/// constants. Shared, never copied: the load makes it once, every frame's
/// item holds the same list, and the drawing side uploads it once per list.
/// Equal when it is the same list or holds the same values; printed by its
/// length.
#[derive(Clone)]
pub struct ShaderData(pub Arc<[f32]>);

impl PartialEq for ShaderData {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0[..] == other.0[..]
    }
}

impl std::fmt::Debug for ShaderData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ShaderData({} floats)", self.0.len())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ShaderProgram {
    /// A bundled vj_fx preset (`effect: "vjfx/grain"`), run on locked time.
    Preset(String),
    /// A library `VjEffect` revision.
    Effect(AssetRef),
    /// `pixel: fn() {...}` in Makepad's shader DSL. `source` is the document's
    /// function (moved to the GPU's VM by its tokens), drawn over the layer's box with
    /// `self.pos` (0..1 in the box), uniforms `time` (layer seconds), `beat`,
    /// `size` (pixels) and one per `params` entry, and textures `below` (the
    /// frame under the layer) when `input: @below`, and `data` (the layer's
    /// `data` list, see [`ShaderData`]). `helpers` are further
    /// functions it calls as `self.<name>(…)`, each its name and function.
    /// `origin` is where `source` begins (its `fn`) in an editor's document:
    /// the file its literal sites name (the document or a module) and the
    /// place (`None` when the document is not being edited).
    Inline { source: makepad_render_graph::pass::PassFn, helpers: Arc<[(String, makepad_render_graph::pass::PassFn)]>, loc: Loc, origin: Option<(String, Loc)> },
}

/// What a KineticText shows: its own text, or a voiceover's words (the
/// line being said as the preset's lyric channels, with its sung progress).
#[derive(Clone, Debug, PartialEq)]
pub enum KineticSource {
    Text(String),
    Voice(String),
}

/// One render-kit effect as written: its name and its parameters, each a
/// keyable value (the kit's catalog says what each takes).
#[derive(Clone, Debug, PartialEq)]
pub struct EffectSpec {
    pub name: String,
    pub params: Vec<(String, EffectArg)>,
    pub loc: Loc,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EffectArg {
    Num(Prop<1>),
    Vec2(Prop<2>),
    Vec3(Prop<3>),
    Color(Prop<4>),
    Bool(bool),
    /// An `@id`: one of the parameter's choices, or another layer's `name`.
    Choice(String),
    Text(String),
    Points(Vec<[f64; 2]>),
}

impl EffectArg {
    pub fn is_animated(&self) -> bool {
        match self {
            EffectArg::Num(p) => p.is_animated(),
            EffectArg::Vec2(p) => p.is_animated(),
            EffectArg::Vec3(p) => p.is_animated(),
            EffectArg::Color(p) => p.is_animated(),
            _ => false,
        }
    }
}

/// A light of a Scene3D.
#[derive(Clone, Debug, PartialEq)]
pub struct Light3D {
    /// `@directional` (from `direction`) or `@point` (at `position`).
    pub point: bool,
    pub direction: Prop<3>,
    pub position: Prop<3>,
    pub color: Prop<4>,
    pub intensity: Prop<1>,
}

/// A node of a Scene3D's model, by its glTF node name.
#[derive(Clone, Debug, PartialEq)]
pub struct Node3D {
    pub name: String,
    pub translate: Prop<3>,
    /// Degrees about x, y, z (applied in that order).
    pub rotate: Prop<3>,
    pub scale: Prop<3>,
    pub visible: Prop<1>,
}

/// A material override of a Scene3D's model, by its glTF material name.
#[derive(Clone, Debug, PartialEq)]
pub struct Material3D {
    pub name: String,
    pub base_color: Option<Prop<4>>,
    pub metallic: Option<Prop<1>>,
    pub roughness: Option<Prop<1>>,
    pub emissive: Option<Prop<4>>,
}

/// A Scene3D's camera: looking from `position` at `target`, or orbiting
/// `target` at `distance` with `yaw`/`pitch` in degrees when `orbit` is set.
#[derive(Clone, Debug, PartialEq)]
pub struct Camera3D {
    pub position: Prop<3>,
    pub target: Prop<3>,
    pub orbit: bool,
    pub yaw: Prop<1>,
    pub pitch: Prop<1>,
    pub distance: Prop<1>,
    /// Vertical field of view, degrees.
    pub fov: Prop<1>,
}

/// Where a 3D scene is. A `Scene3D{..}` the document wrote is an object of
/// its VM, which motion3d reads there; a Text3D or Seq3D lowers to a scene
/// template, a source of its own with the inputs it reads.
#[derive(Clone, Debug, PartialEq)]
pub enum SceneSource {
    /// The scene object in the document's VM (compared by identity).
    #[cfg(feature = "scene3d")]
    Object(makepad_stage_motion3d::doc::SceneObject),
    /// A template's scene text, evaluated in a VM of its own with `inputs`.
    Template { source: Arc<str>, inputs: Vec<(String, InputValue)> },
}

#[derive(Clone, Debug, PartialEq)]
pub enum LayerKind {
    /// `trim_start`/`trim_end` draw part of the stroke, clockwise from the
    /// top-left corner.
    Rect(RectProps),
    /// Trims run clockwise from 12 o'clock.
    Ellipse(EllipseProps),
    /// `data` is SVG path data in the layer's own coordinates; its box is
    /// the path's bounds from (0, 0). `trim_start`/`trim_end` (0..1) draw
    /// that part of the stroke's length (a path "drawing itself").
    Path(PathProps),
    /// A whole SVG, fitted into `size` (its own size when none).
    Svg { src: Media, size: Option<Prop<2>> },
    Text {
        source: TextSource,
        props: TextProps,
        ranges: Vec<TextRange>,
        voice: VoiceStyle,
        plate: Option<Plate>,
        /// A caret after the last shown character: its opacity and colour.
        /// `caret_style: @block` draws a block instead of a bar.
        caret: Option<(Prop<1>, Prop<4>, bool)>,
        /// Variable-font axes, keyable: `wdth` (`stretch` is its old name,
        /// percent, 100 normal; Roboto Flex runs 25..151), `wght`, `slnt`
        /// and any other `axes: {GRAD: 50}`, by four-letter tag. The font
        /// is read at these values (a font without an axis ignores it).
        axes: Vec<(String, Prop<1>)>,
        /// `path: "M…"`: the letters ride this path (in the layer's own
        /// coordinates) from `path_offset` (0..1 of its length).
        path: Option<(Arc<makepad_svg::VectorPath>, Prop<1>)>,
        /// `flicker`: neon-tube flicker of each letter (0 steady, 1 wild),
        /// and the letters (character indices) that keep failing.
        flicker: Option<(Prop<1>, Vec<usize>)>,
        /// `only: [3]`: only these letters (character indices) show.
        only: Option<Vec<usize>>,
        /// `stroke: #fff width: 3`: the letters' outlines, stroked (`width`
        /// in document units) over their fill; `color: #0000` leaves them
        /// hollow.
        stroke: Option<(Prop<4>, Prop<1>)>,
    },
    /// `src: @backdrop` magnifies what is under the layer `zoom` times
    /// about `focus` (document units; the box's centre when none).
    Image { src: Media, size: Option<Prop<2>>, fit: Fit, zoom: Prop<1>, focus: Option<Prop<2>>, mesh: Option<Arc<crate::rig::MeshGrid>> },
    Video { src: Media, size: Option<Prop<2>>, fit: Fit, source_in: Time, speed: f64 },
    /// `post: [...]`: passes over the group's own picture (STRUCTURE.md),
    /// as the document's post is over the frame.
    Group { layers: Vec<Layer>, mask: Option<Box<Layer>>, props: GroupProps, post: GroupPost, glyphs: bool },
    Bones(Box<crate::rig::Bones>),
    /// `Crowd{…}`: rows of seeded silhouettes reacting to the music.
    Crowd(Box<Crowd>),
    /// `Character{…}`: a 2D character rig placed and performed (lane CH1).
    Character(Box<crate::character::CharacterLayer>),
    /// `Freeze{time: t size: …}`: the document's layers under this one
    /// (the root layers before the one holding it) as they are at `time`,
    /// covering `size` and clipped to it: a still of the video's own
    /// picture, a pure function of `time`. In an edit, the edit's picture
    /// at that time comes under them. `time` may move (`fn(t) {…}`, keys):
    /// the picture of each frame is then the one at that frame's `time`.
    Freeze { time: FreezeTime, size: [f64; 2] },
    /// `FlyThrough{into: "title" letter: 3 time: 2.0 duration: 0.6 zoom: 40
    /// layers: […]}`: its layers, with the camera flying into letter
    /// `letter` of the Text named `into` (its left stem, measured on the
    /// real glyphs) from `time`, `zoom` times by the end.
    FlyThrough { layers: Vec<Layer>, target: String, letter: usize, time: Time, duration: f64, zoom: f64 },
    /// `at: anchor("scene", "eye_3")`: the layer follows a named node of
    /// a Scene3D layer (its projected point each frame); its own `x`/`y`
    /// offset it from there. `hide_behind`: hidden while the node is
    /// behind the camera.
    Anchored { layer: Box<Layer>, scene: String, node: String },
    /// A shader over `size` (the whole frame when none). `params` are
    /// p1..p4 for a vj_fx effect (below 0 leaves the effect's own value) or
    /// the named uniforms of an inline shader.
    /// `text` is set for a `KineticText` layer: the text a vj_fx kinetic
    /// type preset shows (a Shader of that preset with its words given).
    /// `font`: a KineticText's document font in place of its kit's own.
    /// `data`: an inline shader's `data: [f, …]` list (see [`ShaderData`]).
    Shader { program: ShaderProgram, size: Option<Prop<2>>, params: Vec<(String, Prop<1>)>, speed: f64, below: bool, data: Option<ShaderData>, text: Option<KineticSource>, font: Option<String> },
    /// A glTF/GLB model drawn with Makepad's PBR renderer into `size`.
    Scene3D { model: Media, size: Option<Prop<2>>, camera: Camera3D, lights: Vec<Light3D>, nodes: Vec<Node3D>, materials: Vec<Material3D>, background: Option<Prop<4>>, exposure: Prop<1> },
    /// A Makepad UI subtree in the Splash widget DSL, laid out at `size`.
    /// `ui` is the field as written; `props` are keyed values the UI reads
    /// as `props.<name>`.
    Widget { ui: Option<makepad_render_graph::pass::PassFn>, ui_loc: Loc, size: Prop<2>, props: Vec<(String, WidgetProp)> },
    /// A full 3D scene (lane R3's `makepad_stage_motion3d`): a
    /// Scene3D with fields beyond the ones above, an object of the
    /// document's own VM that motion3d reads from it; or the scene a Text3D
    /// or Seq3D lowers to (a source of its own). `loc` is where it is
    /// written; `assets` the library files it names.
    /// `textures: {name: asset(…)}` (images) and `videos: {name: asset(…)}`
    /// (video and app-run renders, a frame at the layer's time) are shown by
    /// materials with `texture: "name"`.
    /// `layers: {intro: Group{…}}`: 2D Motion layers drawn at the scene's
    /// time into textures of those names (3D transitions between 2D scenes).
    /// `written`: the scene is an editor's document's own code (not a
    /// template a Text3D or Seq3D fills in).
    Scene3dDoc { scene: SceneSource, loc: Loc, written: bool, size: Option<Prop<2>>, assets: Vec<AssetRef>, textures: Vec<(String, Media)>, videos: Vec<(String, Media)>, layers: Vec<(String, Layer)> },
    /// An SVG as editable parts: every element drawn, and the ones with an
    /// `id` animatable through `parts`; `draw_on` draws the whole thing on
    /// (strokes traced, fills fading in) element by element.
    Vector {
        src: Media,
        size: Option<Prop<2>>,
        parts: Vec<(String, VectorPart)>,
        draw_on: Option<Prop<1>>,
        /// 0..1: how much of the draw-on the elements spread over (0: all
        /// at once, 0.9: one after another).
        stagger: f64,
        /// Width of the traced outline of fill-only shapes during a draw-on,
        /// in the SVG's own units.
        trace: f64,
        fill: Option<Prop<4>>,
        stroke: Option<Prop<4>>,
    },
    /// `count` copies of a layer laid out on a grid, a circle, a line or
    /// along a path, each `delay` seconds after the one before and moved
    /// by `step` times its index (plus a seeded `jitter`). Property
    /// functions in the copy get the copy's index as `i`. With a `source`
    /// list, `items` holds one copy per item (the template with its
    /// `item()` fields filled) and `count` is their number.
    Repeat { layer: Box<Layer>, items: Vec<Layer>, props: RepeatProps },
    /// `Draw2D{draw: fn(c, t, edit) {…}}`: an immediate-mode 2D layer.
    Draw2d { draw: FnProp, size: Option<Prop<2>> },
}

/// A value a Widget's UI reads.
#[derive(Clone, Debug, PartialEq)]
pub enum WidgetProp {
    Num(Prop<1>),
    Color(Prop<4>),
    Text(String),
}

impl LayerKind {
    pub fn type_name(&self) -> &'static str {
        match self {
            LayerKind::Rect(_) => "Rect",
            LayerKind::Ellipse(_) => "Ellipse",
            LayerKind::Path(_) => "Path",
            LayerKind::Svg { .. } => "Svg",
            LayerKind::Text { .. } => "Text",
            LayerKind::Image { .. } => "Image",
            LayerKind::Video { .. } => "Video",
            LayerKind::Group { .. } => "Group",
            LayerKind::Bones(_) => "Bones",
            LayerKind::Shader { text: Some(_), .. } => "KineticText",
            LayerKind::Shader { .. } => "Shader",
            LayerKind::Scene3D { .. } | LayerKind::Scene3dDoc { .. } => "Scene3D",
            LayerKind::Widget { .. } => "Widget",
            LayerKind::Draw2d { .. } => "Draw2D",
            LayerKind::Vector { .. } => "Vector",
            LayerKind::Repeat { .. } => "Repeat",
            LayerKind::Crowd(_) => "Crowd",
            LayerKind::Character(_) => "Character",
            LayerKind::Freeze { .. } => "Freeze",
            LayerKind::FlyThrough { .. } => "FlyThrough",
            LayerKind::Anchored { layer, .. } => layer.kind.type_name(),
        }
    }
}

/// A crowd (the 2D twin of R3's 3D `Crowd` kit, same field names):
/// `count` silhouettes in `rows` filling `size` (back rows smaller;
/// `shape: @arena` bows the rows), each a seeded variant (of `variants`
/// shapes, or all different), bodies in `color` / `color2`, eyes in
/// `eyes` on the left half and `eyes2` on the right; bobbing with a
/// seeded phase (`bob`, spread `stagger` seconds), hopping by `hop` and
/// nodding by `nod` on `react` (@beat, a music feature, or @none),
/// raising arms by `arms` (and by the `cheer` feature), a `signs` share
/// holding up `sign` cards by `sign_up`, popping up from `start`.
#[derive(Clone, Debug, PartialEq)]
pub struct Crowd {
    pub count: usize,
    pub rows: usize,
    pub size: [f64; 2],
    pub seed: u32,
    pub variants: Option<usize>,
    pub arena: bool,
    pub color: Prop<4>,
    pub color2: Option<Prop<4>>,
    pub outline: Option<Prop<4>>,
    pub eyes: [Color; 2],
    pub bob: Prop<1>,
    pub stagger: Option<f64>,
    pub react: CrowdReact,
    pub hop: Prop<1>,
    pub nod: Prop<1>,
    pub arms: Prop<1>,
    pub cheer: Option<String>,
    pub signs: f64,
    pub sign: String,
    pub sign_up: Prop<1>,
    pub sign_color: Color,
    pub sign_ink: Color,
    pub start: Time,
    /// `member: Character{…}`: every member a varied copy of this
    /// character instead of a silhouette (lane CH1).
    pub member: Option<Box<crate::character::CharacterLayer>>,
}

/// What a crowd hops and nods to.
#[derive(Clone, Debug, PartialEq)]
pub enum CrowdReact {
    None,
    Beat,
    /// A music feature: `energy`, `low`, `mid` or `high`.
    Feature(String),
}

/// One `parts` entry of a `Vector`: offsets for the element with that `id`
/// (and everything inside it).
#[derive(Clone, Debug, PartialEq)]
pub struct VectorPart {
    pub opacity: Prop<1>,
    pub x: Prop<1>,
    pub y: Prop<1>,
    pub scale: Prop<2>,
    pub rotate: Prop<1>,
    pub fill: Option<Prop<4>>,
    pub stroke: Option<Prop<4>>,
    pub width: Option<Prop<1>>,
    pub draw_on: Option<Prop<1>>,
}

/// `data_layer: true` (or `data_layer: {mips: true aniso: 8}`): a root layer drawn into
/// a texture of its own (linear values, no colour management) that post
/// passes read by the layer's `name`, instead of into the frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DataLayer {
    pub mips: bool,
    pub aniso: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    /// `name: "logo"`, for messages and tools.
    pub name: Option<String>,
    /// `layers[2]`, `layers[2].layers[0]`, `layers[1].mask`.
    pub path: String,
    pub loc: Loc,
    pub span: Option<Span>,
    pub transform: Transform,
    pub opacity: Prop<1>,
    /// Gaussian blur in document units (per unit on split text).
    pub blur: Prop<1>,
    /// Edge softness in document units; Gaussian sigma is half this value.
    pub feather: Prop<1>,
    pub feather_side: FeatherSide,
    /// `effects: [Effect{name: @glow …}, …]`: R2's render-kit effects over
    /// the layer's picture, in order.
    pub effects: Vec<EffectSpec>,
    pub blend: Blend,
    pub kind: LayerKind,
    /// Whether anything about the layer (or, for a group, its children)
    /// changes over its span. A static layer draws the same pixels for its
    /// whole span, so a renderer draws it once and reuses it.
    pub animated: bool,
    /// `visible: false`: kept in the document, not drawn (an editor's eye).
    pub visible: bool,
    /// `locked: true`: editor-only (not selectable on the canvas); drawn
    /// as usual.
    pub locked: bool,
    /// `hard_cut: true`: the layer cuts in and out by intent (a beat hit,
    /// a flash); inspect's continuity check (`layer.pop_in`/`pop_out`)
    /// leaves it and everything inside it alone.
    pub hard_cut: bool,
    /// A data layer (see [`DataLayer`]).
    pub data: Option<DataLayer>,
}

impl Layer {
    /// `path (Type "name")`, how messages name a layer.
    pub fn describe(&self) -> String {
        match &self.name {
            Some(name) => format!("{} ({} \"{name}\")", self.path, self.kind.type_name()),
            None => format!("{} ({})", self.path, self.kind.type_name()),
        }
    }
}

/// The host's context (EDITS.md §10.3): musical and spoken time, the
/// signals property functions read, and where the document's 0 is on the
/// host's clock. `timing` and `signals` are on the host's clock (an edit's
/// time), so a graphic moved or trimmed in its edit keeps every beat-bound
/// thing on the music's grid.
#[derive(Clone, Debug)]
pub struct MotionContext {
    pub timing: Timing,
    pub signals: Arc<dyn SignalSource>,
    /// Host time of the document's 0 (a Graphic's span start in its edit).
    pub offset: f64,
    /// The host supplies its own picture at a time (an edit's composite
    /// under the graphic): a `Freeze` shows it under the document's layers,
    /// asked for as [`crate::draw::Need::EditFrame`].
    pub edit_frames: bool,
    /// The host's Splash modules (`import(asset(id, rev))` library
    /// revisions); the built-in effects library comes with every document.
    pub modules: crate::modules::Modules,
    /// The language this render of the document is for (`lang()`: a
    /// document writes each text it shows in the languages it is shown in
    /// and picks by it); `None`: as written, `"en"`.
    pub lang: Option<Arc<str>>,
}

impl Default for MotionContext {
    fn default() -> Self {
        Self { timing: Timing::default(), signals: Arc::new(AudioFeatures::default()), offset: 0.0, edit_frames: false, modules: Default::default(), lang: None }
    }
}

impl MotionContext {
    /// Analyzed signals (frame-exact renders): the timing comes from the
    /// features (the grid of `music`, or of the only music source).
    pub fn analyzed(features: AudioFeatures, music: Option<&str>, offset: f64) -> Self {
        Self { timing: features.timing(music), signals: Arc::new(features), offset, edit_frames: false, modules: Default::default(), lang: None }
    }

    /// Live signals (the VJ graph): the transport's grid and live levels.
    pub fn live(live: LiveSignals, offset: f64) -> Self {
        Self { timing: live.timing(None), signals: Arc::new(live), offset, edit_frames: false, modules: Default::default(), lang: None }
    }
}

/// A Group's post passes (compared by what they are written as).
#[derive(Clone, Debug, Default)]
pub struct GroupPost(pub Vec<PostPassSpec>);

impl PartialEq for GroupPost {
    fn eq(&self, other: &Self) -> bool {
        self.0.len() == other.0.len() && format!("{:?}", self.0) == format!("{:?}", other.0)
    }
}

/// One post pass: its program and its keyable uniforms.
#[derive(Clone, Debug)]
pub struct PostPassSpec {
    pub decl: makepad_render_graph::PassDecl,
    pub uniforms: Vec<PostUniform>,
}

#[derive(Clone, Debug)]
pub enum PostUniform {
    One(Prop<1>),
    Two(Prop<2>),
    Three(Prop<3>),
    Four(Prop<4>),
}

/// Motion blur over the whole frame: shutter degrees (keyable), the most
/// sub-frames (4..324), the convergence tolerance in 8-bit levels.
#[derive(Clone, Debug)]
pub struct MotionBlurSpec {
    pub shutter: Prop<1>,
    pub max: u32,
    pub tol: f32,
}

/// Where a document is written: a file's text, or a function of a game's
/// script moved here as its tokens (`ScriptVm::fn_image`). The function is
/// the document: its body evaluates to the `Motion{...}`.
#[derive(Clone, Debug)]
pub enum DocSource {
    /// A path (for messages) and the text.
    File(Arc<str>, Arc<str>),
    Image(Arc<ScriptFnImage>),
}

impl DocSource {
    /// A document of `text` (named [`crate::DOC_FILE`]).
    pub fn file(text: &str) -> Self {
        Self::File(crate::value::DOC_FILE.into(), text.into())
    }

    /// The characters the document is written in.
    pub fn text(&self) -> &str {
        match self {
            Self::File(_, text) => text,
            Self::Image(image) => image.text(),
        }
    }

    /// [`Self::text`] shared (a file's text is not copied).
    pub fn text_arc(&self) -> Arc<str> {
        match self {
            Self::File(_, text) => text.clone(),
            Self::Image(image) => image.text().into(),
        }
    }

    /// The file it is written in (the game's script, for an image).
    pub fn path(&self) -> &str {
        match self {
            Self::File(path, _) => path,
            Self::Image(image) => image.file(),
        }
    }
}

/// A loaded Motion document. Cheap to clone (clones share its VM); `Send`,
/// so it loads on a worker and is drawn on another thread (one thread
/// evaluates it at a time; [`MotionDoc::fork`] gives another its own).
#[derive(Clone, Debug)]
pub struct MotionDoc {
    pub source: DocSource,
    pub label: String,
    /// `brief:` what the document is for, and `tags:` (library metadata).
    pub brief: String,
    pub tags: Vec<String>,
    /// `camera: Camera{…}`: the comp camera of 2.5D effects (R2's kit).
    pub camera: Option<Vec<(String, EffectArg)>>,
    pub plain: LoadedLayers,
    /// `color: @hdr`: the frame is linear half-float light (colours over 1
    /// glow), tone mapped once after `post`; the default is the 8-bit
    /// display frame.
    pub hdr: bool,
    /// `post: [Pass{…} Glow{…} …]`: render-graph passes over the whole frame
    /// (kits already expanded into their passes).
    pub post: Vec<PostPassSpec>,
    /// `motion_blur: MotionBlur{shutter samples tol}`: the whole frame
    /// sampled over its shutter.
    pub motion_blur: Option<MotionBlurSpec>,
    /// Per declared input: whether the document read it while it evaluated
    /// (it decides structure: a layer's text, a count), so a changed value
    /// needs [`MotionDoc::reevaluate`]. The others are read by property
    /// functions at frame time and take effect at the next
    /// [`MotionDoc::eval_with`].
    pub structural: Vec<bool>,
    pub warnings: Vec<Diagnostic>,
    /// Whether any property is a function (the evaluation then runs the
    /// document's VM; see [`crate::eval`]).
    pub has_functions: bool,
    /// The VM the load evaluated the document in, where its property
    /// functions run (shared by its clones).
    pub(crate) vm: Arc<crate::value::DocVm>,
    /// This evaluation of the document in `vm`: its root, and the property
    /// functions the layers' [`FnProp`]s index.
    pub(crate) run: Arc<crate::value::Run>,
    /// The inputs' current values as the VM reads them (shared by the
    /// evaluations that use the document's own values).
    pub(crate) own: Arc<Vec<(LiveId, InputValue)>>,
    /// An editor's document: its place in the literal-site map (a sink
    /// `Cx::patch_literal` reaches), for as long as it lives.
    pub(crate) _literals: Option<Arc<crate::schema::DocLiterals>>,
}

impl std::ops::Deref for MotionDoc {
    type Target = LoadedLayers;

    fn deref(&self) -> &LoadedLayers {
        &self.plain
    }
}

impl std::ops::DerefMut for MotionDoc {
    fn deref_mut(&mut self) -> &mut LoadedLayers {
        &mut self.plain
    }
}

/// One instance's values of a document's declared inputs, in declaration
/// order. Many instances (a HUD's panes) share one document, each with its
/// own: an instance writes by index ([`MotionDoc::write_input`]) and
/// evaluates with them ([`MotionDoc::eval_with`]).
#[derive(Clone, Debug)]
pub struct InstanceInputs {
    pub(crate) slots: Arc<Vec<(LiveId, InputValue)>>,
}

impl InstanceInputs {
    pub fn get(&self, index: usize) -> Option<&InputValue> {
        self.slots.get(index).map(|(_, value)| value)
    }
}

/// What a write of an input did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputWrite {
    /// The input already held the value.
    Unchanged,
    /// Written; read by the document's property functions, so the next
    /// [`MotionDoc::eval_with`] shows it.
    Applied,
    /// Written; the document read this input while it evaluated (it decides
    /// structure), so [`MotionDoc::reevaluate`] must make the document these
    /// inputs go with.
    Reevaluate,
}

impl MotionDoc {
    /// The game's script file an inline document is written in (what its
    /// diagnostics name); `None` for a document of its own text.
    pub fn script_file(&self) -> Option<Arc<str>> {
        match &self.source {
            DocSource::Image(image) => Some(image.file().into()),
            DocSource::File(..) => None,
        }
    }

    /// A new instance's inputs: the document's current values.
    pub fn new_inputs(&self) -> InstanceInputs {
        InstanceInputs { slots: Arc::new((*self.own).clone()) }
    }

    /// Write the input at `index` (from [`LoadedLayers::input_index`]). No
    /// allocation for a value that holds none. The value is coerced to the
    /// input's type as a host's params are; one that does not fit is an error.
    pub fn write_input(&self, inputs: &mut InstanceInputs, index: usize, value: InputValue) -> Result<InputWrite, String> {
        let Some(decl) = self.inputs.get(index) else {
            return Err(format!("the document declares {} inputs, not {}", self.inputs.len(), index + 1));
        };
        let value = crate::load::coerce_param(decl.ty, value);
        if !crate::load::fits(decl.ty, &value) {
            return Err(format!("input `{}` is @{} but the host gives {}", decl.name, decl.ty.id(), value.describe()));
        }
        let slot = &mut Arc::make_mut(&mut inputs.slots)[index].1;
        if *slot == value {
            return Ok(InputWrite::Unchanged);
        }
        *slot = value;
        Ok(if self.structural[index] { InputWrite::Reevaluate } else { InputWrite::Applied })
    }

    /// How many times this document's VM has evaluated a document (the load
    /// is one): a host or a check sees a write that re-evaluated.
    pub fn evaluations(&self) -> u64 {
        self.vm.with(|e| e.evaluations)
    }

    /// The frame at `t` with the pointer over the picture as fractions 0..1
    /// (x left to right, y top to bottom), -1 on both while it is not over
    /// it, written to the declared `pointer_x` / `pointer_y`. A document
    /// that declares neither evaluates as [`MotionDoc::eval_live`].
    pub fn eval_pointer(&self, t: f64, live: f64, pointer: Option<[f64; 2]>) -> crate::frame::MotionFrame {
        if !self.declares_pointer() {
            return self.eval_live(t, live);
        }
        let [x, y] = pointer.unwrap_or([-1.0, -1.0]);
        let mut inputs = self.new_inputs();
        for (name, value) in [("pointer_x", x), ("pointer_y", y)] {
            if let Some(index) = self.input_index(input_id(name)) {
                let _ = self.write_input(&mut inputs, index, InputValue::Number(value));
            }
        }
        self.eval_with_live(t, live, &mut inputs)
    }

    /// Whether every frame of the document is the same picture: no layer
    /// (at any depth) is animated or confined to a span, no data layer, and
    /// the document has no camera, post pass or motion blur. Judged from the
    /// load's own `Layer::animated` facts, so a property function, a clock
    /// text, a video, a shader or an effect that runs on time all count as
    /// moving. Whether it reads `live()` is a fact of an evaluated frame
    /// ([`MotionDoc::reads_live`]), asked separately. Conservative: only a
    /// document proven still says true.
    pub fn is_time_independent(&self) -> bool {
        self.post.is_empty()
            && self.camera.is_none()
            && self.motion_blur.is_none()
            && self.all_layers().iter().all(|layer| !layer.animated && layer.span.is_none() && layer.data.is_none())
    }
}
