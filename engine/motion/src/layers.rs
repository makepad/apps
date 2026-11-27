//! The Motion component library: every layer type, and the document root,
//! is a Splash component registered in a document VM's `mod.motion`. The VM
//! itself checks the fields a document writes (a name the type does not
//! have is an error at the place it is written, with the nearest names),
//! and the loader reads the objects by the type they were made from.
//!
//! The fields are still read by the loader (`crate::load`); each holds the
//! value as written ([`Field`]).

use crate::value::doc_host;
use makepad_script::*;

/// A field's value as the document wrote it (a number, a colour, an
/// animation, a function, …), which the loader checks, and what kind of
/// value the field takes (`K`, a [`Kind`]): what an editor offers for it
/// ([`field_specs`]).
pub struct Field<K: Kind = kind::Any>(pub ScriptValue, std::marker::PhantomData<K>);

impl<K: Kind> Default for Field<K> {
    fn default() -> Self {
        Field(NIL, std::marker::PhantomData)
    }
}

impl<K: Kind> ScriptHook for Field<K> {}

impl<K: Kind> ScriptNew for Field<K> {
    fn script_new(_vm: &mut ScriptVm) -> Self {
        Self::default()
    }
    fn script_type_check(_heap: &ScriptHeap, _value: ScriptValue) -> bool {
        true
    }
    fn script_default(_vm: &mut ScriptVm) -> ScriptValue {
        NIL
    }
    fn script_proto_build(_vm: &mut ScriptVm, _props: &mut ScriptTypeProps) -> ScriptValue {
        NIL
    }
}

impl<K: Kind> ScriptApply for Field<K> {
    fn script_apply(&mut self, _vm: &mut ScriptVm, _apply: &Apply, _scope: &mut Scope, value: ScriptValue) {
        self.0 = value;
    }
    fn script_to_value(&self, _vm: &mut ScriptVm) -> ScriptValue {
        self.0
    }
}

/// What kind of value a field takes, for its editor.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldType {
    /// A number (`dims` 1), a `vec2`/`vec3` (`dims` 2/3); animatable.
    Number { dims: usize },
    Color,
    /// Words; `multiline` for a text or a caption.
    Text { multiline: bool },
    Font,
    /// `@name`, one of `choices`.
    Enum { choices: &'static [&'static str] },
    Ease,
    /// A moment: seconds or `beat(n)`, `word(@vo, i)`, ….
    Time,
    /// A length: seconds or `beats(x)`.
    Length,
    Bool,
    /// An asset, `in(@name)` of one, or markup.
    Media,
    /// A list or table of values (a chart's `data`).
    Data,
    /// One layer.
    Layer,
    /// A list of layers.
    Layers,
    /// A function (`fn(t) {…}`, a shader's `pixel`).
    Code,
    /// Anything else (a record of settings, a value read its own way).
    Any,
}

/// A field kind: what [`Field`] holds, as a type.
pub trait Kind: 'static {
    fn ty() -> FieldType;
}

macro_rules! kinds {
    ($($(#[$doc:meta])* $name:ident => $ty:expr;)*) => {
        /// The kinds a [`Field`] takes.
        pub mod kind {
            use super::*;
            $( $(#[$doc])* pub struct $name; impl Kind for $name { fn ty() -> FieldType { $ty } } )*
        }
        /// Every kind, by the type id of its field.
        fn kind_types() -> Vec<(ScriptTypeId, FieldType)> {
            vec![$((<Field<kind::$name> as ScriptNew>::script_type_id_static(), kind::$name::ty())),*]
        }
    };
}

kinds! {
    /// A number.
    Num => FieldType::Number { dims: 1 };
    /// `vec2(x, y)`.
    Vec2 => FieldType::Number { dims: 2 };
    /// `vec3(x, y, z)`.
    Vec3 => FieldType::Number { dims: 3 };
    Color => FieldType::Color;
    /// A short string (a name, a label).
    Words => FieldType::Text { multiline: false };
    /// A text that may run over lines.
    Prose => FieldType::Text { multiline: true };
    Font => FieldType::Font;
    Ease => FieldType::Ease;
    Time => FieldType::Time;
    Length => FieldType::Length;
    Bool => FieldType::Bool;
    Media => FieldType::Media;
    Data => FieldType::Data;
    Layer => FieldType::Layer;
    Layers => FieldType::Layers;
    Code => FieldType::Code;
    Any => FieldType::Any;
    Cap => FieldType::Enum { choices: CAP };
    Join => FieldType::Enum { choices: JOIN };
    FeatherSide => FieldType::Enum { choices: FEATHER_SIDE };
    Blend => FieldType::Enum { choices: BLEND };
    Fit => FieldType::Enum { choices: FIT };
    Align => FieldType::Enum { choices: ALIGN };
    VAlign => FieldType::Enum { choices: VALIGN };
    TextBy => FieldType::Enum { choices: TEXT_BY };
    MaskMode => FieldType::Enum { choices: MASK_MODE };
    RepeatLayout => FieldType::Enum { choices: REPEAT_LAYOUT };
    StaggerOrder => FieldType::Enum { choices: STAGGER_ORDER };
    Exit => FieldType::Enum { choices: EXIT };
    VoiceShow => FieldType::Enum { choices: VOICE_SHOW };
    TimeFormat => FieldType::Enum { choices: TIME_FORMAT };
}

/// The choices of each `@name` field kind, as the loader takes them.
pub const CAP: &[&str] = &["round", "butt", "square"];
pub const JOIN: &[&str] = &["round", "miter", "bevel"];
pub const FEATHER_SIDE: &[&str] = &["both", "inner", "outer"];
pub const BLEND: &[&str] = &["normal", "add", "screen", "multiply"];
pub const FIT: &[&str] = &["fill", "fit", "stretch", "none"];
pub const ALIGN: &[&str] = &["left", "center", "right"];
pub const VALIGN: &[&str] = &["top", "center", "bottom"];
pub const TEXT_BY: &[&str] = &["whole", "word", "letter", "line", "syllable"];
pub const MASK_MODE: &[&str] = &["alpha", "inverse", "luma", "luma_inverse"];
pub const REPEAT_LAYOUT: &[&str] = &["grid", "radial", "along", "line"];
pub const STAGGER_ORDER: &[&str] = &["forward", "reverse", "center", "edges", "random"];
pub const EXIT: &[&str] = &["defocus", "collapse", "fill", "iris", "curtain", "speed_lines"];
pub const VOICE_SHOW: &[&str] = &["all", "spoken", "active", "line"];
pub const TIME_FORMAT: &[&str] = &["timecode", "mmss", "hhmmss", "seconds"];

/// One field of a layer type, for editors.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldSpec {
    pub name: String,
    pub ty: FieldType,
}

/// The fields of layer type `kind` with the kind of value each takes, the
/// common ones first (empty for an unknown type).
pub fn field_specs(kind: &str) -> Vec<FieldSpec> {
    let Some(layer) = LAYERS.iter().find(|layer| layer.name == kind) else { return Vec::new() };
    let kinds = kind_types();
    typed_props(layer.make, layer.type_id).into_iter().map(|(name, ty)| FieldSpec {
        // These items give a few Common fields a data-coordinate meaning.
        ty: match (kind, name.as_str()) {
            ("Plot", "x" | "y") => FieldType::Data,
            ("VectorField", "scale") => FieldType::Number { dims: 1 },
            _ => kinds.iter().find(|(id, _)| *id == ty).map_or(FieldType::Any, |(_, t)| t.clone()),
        },
        name,
    }).collect()
}

/// The document's own fields (`Motion{…}`) with the kind of value each
/// takes.
pub fn document_specs() -> Vec<FieldSpec> {
    let kinds = kind_types();
    typed_props(<Motion as ScriptNew>::script_component, <Motion as ScriptNew>::script_type_id_static)
        .into_iter()
        .map(|(name, ty)| FieldSpec { ty: kinds.iter().find(|(id, _)| *id == ty).map_or(FieldType::Any, |(_, t)| t.clone()), name })
        .collect()
}

/// A Motion document: its frame, timing, inputs, layers and post.
#[derive(Script, ScriptHook, Default)]
pub struct Motion {
    #[live] pub label: Field<kind::Words>,
    #[live] pub brief: Field<kind::Prose>,
    #[live] pub tags: Field<kind::Any>,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub duration: Field<kind::Num>,
    #[live] pub stops: Field<kind::Any>,
    #[live] pub fps: Field<kind::Num>,
    #[live] pub bpm: Field<kind::Num>,
    #[live] pub background: Field<kind::Color>,
    #[live] pub style: Field<kind::Any>,
    #[live] pub inputs: Field<kind::Any>,
    #[live] pub layers: Field<kind::Layers>,
    #[live] pub camera: Field<kind::Any>,
    #[live] pub color: Field<kind::Any>,
    #[live] pub post: Field<kind::Any>,
    #[live] pub motion_blur: Field<kind::Any>,
}

/// The fields every layer has: name, place, transform, opacity, blend,
/// span, blur, edge feather, effects, exits and modifiers.
#[derive(Script, ScriptHook, Default)]
pub struct Common {
    #[live] pub data_layer: Field<kind::Any>,
    #[live] pub name: Field<kind::Words>,
    #[live] pub at: Field<kind::Vec2>,
    #[live] pub x: Field<kind::Num>,
    #[live] pub y: Field<kind::Num>,
    #[live] pub scale: Field<kind::Vec2>,
    #[live] pub rotate: Field<kind::Num>,
    #[live] pub anchor: Field<kind::Vec2>,
    #[live] pub opacity: Field<kind::Num>,
    #[live] pub blend: Field<kind::Blend>,
    #[live] pub span: Field<kind::Any>,
    #[live] pub blur: Field<kind::Num>,
    #[live] pub feather: Field<kind::Num>,
    #[live] pub feather_side: Field<kind::FeatherSide>,
    #[live] pub effects: Field<kind::Any>,
    #[live] pub visible: Field<kind::Bool>,
    #[live] pub locked: Field<kind::Bool>,
    /// Cuts in and out by intent: no `layer.pop_in`/`pop_out` for it.
    #[live] pub hard_cut: Field<kind::Bool>,
    #[live] pub exit: Field<kind::Exit>,
    #[live] pub exit_at: Field<kind::Time>,
    #[live] pub exit_duration: Field<kind::Length>,
    #[live] pub exit_to: Field<kind::Any>,
    #[live] pub onion: Field<kind::Any>,
    #[live] pub slices: Field<kind::Any>,
    #[live] pub tear: Field<kind::Any>,
    #[live] pub droste: Field<kind::Any>,
    /// Bind this child of Bones to one named bone.
    #[live] pub rigid: Field<kind::Words>,
}

/// A rectangle: `size`, corner `radius`, `fill` and/or `stroke` of `width`.
#[derive(Script, ScriptHook)]
pub struct Rect {
    #[script_splat]
    pub common: Common,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub radius: Field<kind::Num>,
    #[live] pub fill: Field<kind::Color>,
    #[live] pub stroke: Field<kind::Color>,
    #[live] pub width: Field<kind::Num>,
    #[live] pub trim_start: Field<kind::Num>,
    #[live] pub trim_end: Field<kind::Num>,
    #[live] pub cap: Field<kind::Cap>,
}

/// An ellipse in its `size` box, filled and/or stroked.
#[derive(Script, ScriptHook)]
pub struct Ellipse {
    #[script_splat]
    pub common: Common,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub fill: Field<kind::Color>,
    #[live] pub stroke: Field<kind::Color>,
    #[live] pub width: Field<kind::Num>,
    #[live] pub trim_start: Field<kind::Num>,
    #[live] pub trim_end: Field<kind::Num>,
    #[live] pub cap: Field<kind::Cap>,
}

/// An SVG path (`svg`) or `points`, filled and/or stroked, drawn on with `trim_start`/`trim_end`.
#[derive(Script, ScriptHook)]
pub struct Path {
    #[script_splat]
    pub common: Common,
    #[live] pub svg: Field<kind::Any>,
    #[live] pub points: Field<kind::Data>,
    #[live] pub closed: Field<kind::Bool>,
    #[live] pub smooth: Field<kind::Bool>,
    #[live] pub fill: Field<kind::Color>,
    #[live] pub stroke: Field<kind::Color>,
    #[live] pub width: Field<kind::Num>,
    #[live] pub trim_start: Field<kind::Num>,
    #[live] pub trim_end: Field<kind::Num>,
    #[live] pub cap: Field<kind::Cap>,
    #[live] pub join: Field<kind::Join>,
}

/// An SVG image (`src` asset or inline `svg` markup) in its `size` box.
#[derive(Script, ScriptHook)]
pub struct Svg {
    #[script_splat]
    pub common: Common,
    #[live] pub src: Field<kind::Media>,
    #[live] pub svg: Field<kind::Prose>,
    #[live] pub size: Field<kind::Vec2>,
}

/// An SVG as editable parts: per-part transforms, a draw-on, recolouring.
#[derive(Script, ScriptHook)]
pub struct Vector {
    #[script_splat]
    pub common: Common,
    #[live] pub src: Field<kind::Media>,
    #[live] pub svg: Field<kind::Prose>,
    #[live] pub icon: Field<kind::Words>,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub parts: Field<kind::Any>,
    #[live] pub draw_on: Field<kind::Num>,
    #[live] pub stagger: Field<kind::Num>,
    #[live] pub trace: Field<kind::Num>,
    #[live] pub fill: Field<kind::Color>,
    #[live] pub stroke: Field<kind::Color>,
}

/// Text set in a bundled or installed font, by letter, word or line, with its animators.
#[derive(Script, ScriptHook)]
pub struct Text {
    #[script_splat]
    pub common: Common,
    #[live] pub text: Field<kind::Prose>,
    #[live] pub source: Field<kind::Any>,
    #[live] pub font: Field<kind::Font>,
    #[live] pub size: Field<kind::Num>,
    #[live] pub color: Field<kind::Color>,
    #[live] pub align: Field<kind::Align>,
    #[live] pub valign: Field<kind::VAlign>,
    #[live] pub by: Field<kind::TextBy>,
    #[live] pub stagger: Field<kind::Length>,
    #[live] pub range: Field<kind::Any>,
    #[live] pub ranges: Field<kind::Data>,
    #[live] pub line_height: Field<kind::Num>,
    #[live] pub tracking: Field<kind::Num>,
    #[live] pub wrap: Field<kind::Any>,
    #[live] pub fit: Field<kind::Any>,
    #[live] pub show: Field<kind::VoiceShow>,
    #[live] pub pop: Field<kind::Any>,
    #[live] pub highlight: Field<kind::Any>,
    #[live] pub highlight_color: Field<kind::Color>,
    #[live] pub karaoke: Field<kind::Any>,
    #[live] pub stretch: Field<kind::Any>,
    #[live] pub wdth: Field<kind::Num>,
    #[live] pub wght: Field<kind::Num>,
    #[live] pub slnt: Field<kind::Num>,
    #[live] pub axes: Field<kind::Any>,
    #[live] pub wave: Field<kind::Any>,
    #[live] pub ghost: Field<kind::Color>,
    #[live] pub line: Field<kind::Num>,
    #[live] pub sweep: Field<kind::Bool>,
    #[live] pub number: Field<kind::Any>,
    #[live] pub decimals: Field<kind::Num>,
    #[live] pub grouping: Field<kind::Any>,
    #[live] pub prefix: Field<kind::Words>,
    #[live] pub suffix: Field<kind::Words>,
    #[live] pub tabular: Field<kind::Bool>,
    #[live] pub format: Field<kind::TimeFormat>,
    #[live] pub total: Field<kind::Num>,
    #[live] pub log: Field<kind::Bool>,
    #[live] pub overflow: Field<kind::Any>,
    #[live] pub plate: Field<kind::Color>,
    #[live] pub plate_pad: Field<kind::Vec2>,
    #[live] pub plate_radius: Field<kind::Num>,
    #[live] pub plate_stroke: Field<kind::Color>,
    #[live] pub plate_width: Field<kind::Num>,
    #[live] pub plate_reveal: Field<kind::Num>,
    #[live] pub plate_each: Field<kind::Bool>,
    #[live] pub plate_style: Field<kind::Any>,
    #[live] pub path: Field<kind::Any>,
    #[live] pub path_offset: Field<kind::Num>,
    #[live] pub animate: Field<kind::Any>,
    #[live] pub start: Field<kind::Time>,
    #[live] pub speed: Field<kind::Num>,
    #[live] pub words: Field<kind::Prose>,
    #[live] pub hold: Field<kind::Length>,
    #[live] pub layout: Field<kind::Any>,
    #[live] pub inside: Field<kind::Any>,
    #[live] pub stroke: Field<kind::Color>,
    #[live] pub width: Field<kind::Num>,
    #[live] pub reveal: Field<kind::Num>,
    #[live] pub caret: Field<kind::Num>,
    #[live] pub caret_color: Field<kind::Color>,
    #[live] pub caret_style: Field<kind::Any>,
    #[live] pub cps: Field<kind::Num>,
    #[live] pub flicker: Field<kind::Num>,
    #[live] pub fail: Field<kind::Any>,
    #[live] pub role: Field<kind::Any>,
    #[live] pub only: Field<kind::Any>,
    #[live] pub window: Field<kind::Any>,
    #[live] pub morph: Field<kind::Any>,
    #[live] pub mirror: Field<kind::Any>,
    #[live] pub collapse: Field<kind::Any>,
    #[live] pub rows: Field<kind::Any>,
    #[live] pub glyphs: Field<kind::Any>,
}

/// An image asset fitted into its `size` box.
#[derive(Script, ScriptHook)]
pub struct Image {
    #[script_splat]
    pub common: Common,
    #[live] pub src: Field<kind::Media>,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub fit: Field<kind::Fit>,
    #[live] pub zoom: Field<kind::Num>,
    #[live] pub focus: Field<kind::Vec2>,
    #[live] pub mesh: Field<kind::Vec2>,
}

/// A deforming container: FK, follow, IK, then skin its children.
#[derive(Script, ScriptHook)]
pub struct Bones {
    #[script_splat]
    pub common: Common,
    #[live] pub bones: Field<kind::Any>,
    #[live] pub ik: Field<kind::Any>,
    #[live] pub follow: Field<kind::Any>,
    #[live] pub layers: Field<kind::Layers>,
}

#[derive(Script, ScriptHook)]
pub struct Bone {
    #[live] pub name: Field<kind::Words>,
    #[live] pub parent: Field<kind::Any>,
    #[live] pub length: Field<kind::Num>,
    #[live] pub rotate: Field<kind::Num>,
}

#[derive(Script, ScriptHook)]
pub struct IK {
    #[live] pub end: Field<kind::Words>,
    #[live] pub target: Field<kind::Vec2>,
    #[live] pub bend: Field<kind::Any>,
    #[live] pub mix: Field<kind::Num>,
}

#[derive(Script, ScriptHook)]
pub struct Follow {
    #[live] pub bone: Field<kind::Words>,
    #[live] pub path: Field<kind::Prose>,
    #[live] pub along: Field<kind::Num>,
    #[live] pub orient: Field<kind::Bool>,
}

/// A video asset fitted into its `size` box, from its `in` second at its `speed`.
#[derive(Script, ScriptHook)]
pub struct Video {
    #[script_splat]
    pub common: Common,
    #[live] pub src: Field<kind::Media>,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub fit: Field<kind::Fit>,
    #[live] pub r#in: Field<kind::Time>,
    #[live] pub speed: Field<kind::Num>,
}

/// Layers drawn together, optionally masked, with child staggers.
#[derive(Script, ScriptHook)]
pub struct Group {
    #[script_splat]
    pub common: Common,
    #[live] pub layers: Field<kind::Layers>,
    #[live] pub mask: Field<kind::Layer>,
    #[live] pub mask_mode: Field<kind::MaskMode>,
    #[live] pub mask_feather: Field<kind::Num>,
    #[live] pub stagger: Field<kind::Length>,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub post: Field<kind::Any>,
    /// `glyphs: true`: the children are letters drawn as paths (a Math
    /// formula, outlined lettering); layout checks treat them as text.
    #[live] pub glyphs: Field<kind::Bool>,
}

/// Copies of one `layer` in a layout, one per `source` item or `count`.
#[derive(Script, ScriptHook)]
pub struct Repeat {
    #[script_splat]
    pub common: Common,
    #[live] pub layer: Field<kind::Layer>,
    #[live] pub source: Field<kind::Any>,
    #[live] pub count: Field<kind::Num>,
    #[live] pub layout: Field<kind::RepeatLayout>,
    #[live] pub columns: Field<kind::Num>,
    #[live] pub spacing: Field<kind::Vec2>,
    #[live] pub radius: Field<kind::Num>,
    #[live] pub start: Field<kind::Num>,
    #[live] pub sweep: Field<kind::Num>,
    #[live] pub face: Field<kind::Any>,
    #[live] pub path: Field<kind::Any>,
    #[live] pub delay: Field<kind::Length>,
    #[live] pub step: Field<kind::Any>,
    #[live] pub jitter: Field<kind::Any>,
    #[live] pub seed: Field<kind::Num>,
}

/// A procedural pattern (stripes, dots, checks, …) filling its box.
#[derive(Script, ScriptHook)]
pub struct Pattern {
    #[script_splat]
    pub common: Common,
    #[live] pub kind: Field<kind::Words>,
    #[live] pub color: Field<kind::Color>,
    #[live] pub color2: Field<kind::Color>,
    #[live] pub cell: Field<kind::Num>,
    #[live] pub angle: Field<kind::Num>,
    #[live] pub weight: Field<kind::Num>,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub speed: Field<kind::Num>,
    #[live] pub seed: Field<kind::Num>,
}

/// A data chart (`type`) built from `data`, drawn as ordinary layers.
#[derive(Script, ScriptHook)]
pub struct Chart {
    #[script_splat]
    pub common: Common,
    #[live] pub r#type: Field<kind::Words>,
    #[live] pub data: Field<kind::Data>,
    #[live] pub csv: Field<kind::Media>,
    #[live] pub json: Field<kind::Media>,
    #[live] pub split: Field<kind::Any>,
    #[live] pub next: Field<kind::Any>,
    #[live] pub switch: Field<kind::Any>,
    #[live] pub every: Field<kind::Length>,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub theme: Field<kind::Any>,
    #[live] pub colors: Field<kind::Any>,
    #[live] pub color: Field<kind::Color>,
    #[live] pub accent: Field<kind::Color>,
    #[live] pub label_color: Field<kind::Color>,
    #[live] pub font: Field<kind::Font>,
    #[live] pub title_font: Field<kind::Font>,
    #[live] pub label_size: Field<kind::Num>,
    #[live] pub format: Field<kind::Any>,
    #[live] pub decimals: Field<kind::Num>,
    #[live] pub prefix: Field<kind::Words>,
    #[live] pub suffix: Field<kind::Words>,
    #[live] pub currency: Field<kind::Words>,
    #[live] pub start: Field<kind::Time>,
    #[live] pub stagger: Field<kind::Length>,
    #[live] pub grow: Field<kind::Any>,
    #[live] pub ease: Field<kind::Ease>,
    #[live] pub land: Field<kind::Any>,
    #[live] pub axes: Field<kind::Any>,
    #[live] pub grid: Field<kind::Any>,
    #[live] pub values: Field<kind::Data>,
    #[live] pub labels: Field<kind::Any>,
    #[live] pub legend: Field<kind::Any>,
    #[live] pub title: Field<kind::Words>,
    #[live] pub subtitle: Field<kind::Words>,
    #[live] pub source: Field<kind::Words>,
    #[live] pub annotate: Field<kind::Any>,
    #[live] pub highlight: Field<kind::Any>,
    #[live] pub links: Field<kind::Data>,
    #[live] pub layout: Field<kind::Any>,
    #[live] pub thickness: Field<kind::Num>,
    #[live] pub max: Field<kind::Num>,
    #[live] pub min: Field<kind::Num>,
    #[live] pub orient: Field<kind::Any>,
    #[live] pub mode: Field<kind::Any>,
    #[live] pub sort: Field<kind::Any>,
    #[live] pub top: Field<kind::Num>,
    #[live] pub smooth: Field<kind::Any>,
    #[live] pub center: Field<kind::Any>,
    #[live] pub x_title: Field<kind::Words>,
    #[live] pub y_title: Field<kind::Words>,
    #[live] pub root: Field<kind::Any>,
    #[live] pub payoff: Field<kind::Any>,
    #[live] pub dots: Field<kind::Any>,
    #[live] pub series: Field<kind::Data>,
    #[live] pub regions: Field<kind::Any>,
    #[live] pub face: Field<kind::Any>,
    #[live] pub shape: Field<kind::Any>,
    #[live] pub callout: Field<kind::Any>,
    #[live] pub style: Field<kind::Any>,
    #[live] pub clock: Field<kind::Any>,
    #[live] pub total: Field<kind::Any>,
    #[live] pub rider: Field<kind::Any>,
    #[live] pub rider_rotate: Field<kind::Any>,
    #[live] pub rider_offset: Field<kind::Any>,
    #[live] pub review: Field<kind::Any>,
    #[live] pub y_scale: Field<kind::Any>,
    #[live] pub x_scale: Field<kind::Any>,
    #[live] pub window: Field<kind::Any>,
    #[live] pub activate: Field<kind::Any>,
    #[live] pub header: Field<kind::Any>,
    #[live] pub top_k: Field<kind::Num>,
    #[live] pub overflow: Field<kind::Any>,
}

/// A shader effect or an inline `pixel` function over its box.
#[derive(Script, ScriptHook)]
pub struct Shader {
    #[script_splat]
    pub common: Common,
    #[live] pub effect: Field<kind::Any>,
    #[live] pub pixel: Field<kind::Code>,
    #[live] pub helpers: Field<kind::Code>,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub params: Field<kind::Any>,
    #[live] pub p1: Field<kind::Num>,
    #[live] pub p2: Field<kind::Num>,
    #[live] pub p3: Field<kind::Num>,
    #[live] pub p4: Field<kind::Num>,
    #[live] pub speed: Field<kind::Num>,
    #[live] pub input: Field<kind::Any>,
    #[live] pub data: Field<kind::Any>,
}

/// Text animated by a kinetic kit (`preset`).
#[derive(Script, ScriptHook)]
pub struct KineticText {
    #[script_splat]
    pub common: Common,
    #[live] pub preset: Field<kind::Words>,
    #[live] pub text: Field<kind::Prose>,
    #[live] pub source: Field<kind::Any>,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub p1: Field<kind::Num>,
    #[live] pub p2: Field<kind::Num>,
    #[live] pub p3: Field<kind::Num>,
    #[live] pub p4: Field<kind::Num>,
    #[live] pub speed: Field<kind::Num>,
    #[live] pub fit: Field<kind::Any>,
    #[live] pub wdth: Field<kind::Num>,
    #[live] pub wght: Field<kind::Num>,
    #[live] pub slnt: Field<kind::Num>,
    #[live] pub backdrop: Field<kind::Bool>,
    #[live] pub font: Field<kind::Font>,
}

/// Extruded 3D letters with a preset motion.
#[derive(Script, ScriptHook)]
pub struct Text3D {
    #[script_splat]
    pub common: Common,
    #[live] pub text: Field<kind::Prose>,
    #[live] pub preset: Field<kind::Words>,
    #[live] pub font: Field<kind::Font>,
    #[live] pub material: Field<kind::Any>,
    #[live] pub color: Field<kind::Color>,
    #[live] pub accent: Field<kind::Color>,
    #[live] pub background: Field<kind::Any>,
    #[live] pub depth: Field<kind::Num>,
    #[live] pub bevel: Field<kind::Num>,
    #[live] pub bevel_type: Field<kind::Any>,
    #[live] pub tracking: Field<kind::Num>,
    #[live] pub line_height: Field<kind::Num>,
    #[live] pub align: Field<kind::Align>,
    #[live] pub by: Field<kind::TextBy>,
    #[live] pub order: Field<kind::StaggerOrder>,
    #[live] pub stagger: Field<kind::Num>,
    #[live] pub duration: Field<kind::Num>,
    #[live] pub word_times: Field<kind::Data>,
    #[live] pub words: Field<kind::Any>,
    #[live] pub line: Field<kind::Num>,
    #[live] pub ghost: Field<kind::Color>,
    #[live] pub highlight_color: Field<kind::Color>,
    #[live] pub pulse: Field<kind::Num>,
    #[live] pub music: Field<kind::Any>,
    #[live] pub fit: Field<kind::Any>,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub environment: Field<kind::Any>,
    #[live] pub floor: Field<kind::Bool>,
    #[live] pub motion_blur: Field<kind::Bool>,
    #[live] pub seed: Field<kind::Num>,
}

/// A ready-made 3D sequence (`template`) with text, media and colours.
#[derive(Script, ScriptHook)]
pub struct Seq3D {
    #[script_splat]
    pub common: Common,
    #[live] pub template: Field<kind::Words>,
    #[live] pub text: Field<kind::Prose>,
    #[live] pub subtitle: Field<kind::Words>,
    #[live] pub color: Field<kind::Color>,
    #[live] pub accent: Field<kind::Color>,
    #[live] pub background: Field<kind::Any>,
    #[live] pub logo: Field<kind::Media>,
    #[live] pub image: Field<kind::Media>,
    #[live] pub video: Field<kind::Media>,
    #[live] pub product: Field<kind::Media>,
    #[live] pub font: Field<kind::Font>,
    #[live] pub bpm: Field<kind::Num>,
    #[live] pub duration: Field<kind::Num>,
    #[live] pub seed: Field<kind::Num>,
    #[live] pub from: Field<kind::Num>,
    #[live] pub to: Field<kind::Num>,
    #[live] pub points: Field<kind::Data>,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub phone_image: Field<kind::Media>,
    #[live] pub phone_video: Field<kind::Media>,
    #[live] pub fit: Field<kind::Any>,
    #[live] pub crop: Field<kind::Any>,
    #[live] pub phone_crop: Field<kind::Any>,
    #[live] pub phone_aspect: Field<kind::Num>,
}

/// A 3D scene: a glTF `model` or a Splash scene, with camera, lights and materials.
#[derive(Script, ScriptHook)]
pub struct Scene3D {
    #[script_splat]
    pub common: Common,
    #[live] pub model: Field<kind::Media>,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub camera: Field<kind::Any>,
    #[live] pub lights: Field<kind::Any>,
    #[live] pub nodes: Field<kind::Any>,
    #[live] pub materials: Field<kind::Any>,
    #[live] pub background: Field<kind::Color>,
    #[live] pub exposure: Field<kind::Num>,
    #[live] pub textures: Field<kind::Any>,
    #[live] pub videos: Field<kind::Any>,
    #[live] pub layers: Field<kind::Any>,
}

/// A Makepad widget tree (`ui`) driven by animated `props`.
#[derive(Script, ScriptHook)]
pub struct Widget {
    #[script_splat]
    pub common: Common,
    #[live] pub ui: Field<kind::Code>,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub props: Field<kind::Any>,
}

/// A crowd of simple characters that react to the music.
#[derive(Script, ScriptHook)]
pub struct Crowd {
    #[script_splat]
    pub common: Common,
    #[live] pub count: Field<kind::Num>,
    #[live] pub rows: Field<kind::Num>,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub seed: Field<kind::Num>,
    #[live] pub variants: Field<kind::Num>,
    #[live] pub shape: Field<kind::Any>,
    #[live] pub color: Field<kind::Color>,
    #[live] pub color2: Field<kind::Color>,
    #[live] pub outline: Field<kind::Color>,
    #[live] pub eyes: Field<kind::Color>,
    #[live] pub eyes2: Field<kind::Color>,
    #[live] pub bob: Field<kind::Num>,
    #[live] pub stagger: Field<kind::Num>,
    #[live] pub react: Field<kind::Any>,
    #[live] pub hop: Field<kind::Num>,
    #[live] pub nod: Field<kind::Num>,
    #[live] pub arms: Field<kind::Num>,
    #[live] pub cheer: Field<kind::Any>,
    #[live] pub signs: Field<kind::Num>,
    #[live] pub sign: Field<kind::Words>,
    #[live] pub sign_up: Field<kind::Num>,
    #[live] pub sign_color: Field<kind::Color>,
    #[live] pub sign_ink: Field<kind::Color>,
    #[live] pub start: Field<kind::Time>,
    #[live] pub member: Field<kind::Any>,
}

/// The layers before it frozen at `time`, shown in its box.
#[derive(Script, ScriptHook)]
pub struct Freeze {
    #[script_splat]
    pub common: Common,
    #[live] pub time: Field<kind::Time>,
    #[live] pub size: Field<kind::Vec2>,
}

/// Comic panels with captions, bursts and halftone.
#[derive(Script, ScriptHook)]
pub struct Comic {
    #[script_splat]
    pub common: Common,
    #[live] pub panels: Field<kind::Data>,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub layout: Field<kind::Any>,
    #[live] pub columns: Field<kind::Num>,
    #[live] pub gutter: Field<kind::Num>,
    #[live] pub border: Field<kind::Num>,
    #[live] pub ink: Field<kind::Color>,
    #[live] pub paper: Field<kind::Color>,
    #[live] pub halftone: Field<kind::Bool>,
    #[live] pub seed: Field<kind::Num>,
}

/// A photo card that flashes in and develops.
#[derive(Script, ScriptHook)]
pub struct Polaroid {
    #[script_splat]
    pub common: Common,
    #[live] pub time: Field<kind::Time>,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub caption: Field<kind::Words>,
    #[live] pub caption_time: Field<kind::Time>,
    #[live] pub tilt: Field<kind::Num>,
    #[live] pub flash: Field<kind::Bool>,
    #[live] pub mono: Field<kind::Bool>,
    #[live] pub burst: Field<kind::Bool>,
    #[live] pub ink: Field<kind::Color>,
    #[live] pub card: Field<kind::Color>,
}

/// A 2D character rig: poses, expressions, walks and lip sync.
#[derive(Script, ScriptHook)]
pub struct Character {
    #[script_splat]
    pub common: Common,
    #[live] pub model: Field<kind::Any>,
    #[live] pub look: Field<kind::Any>,
    #[live] pub svg: Field<kind::Media>,
    #[live] pub src: Field<kind::Media>,
    #[live] pub height: Field<kind::Num>,
    #[live] pub facing: Field<kind::Any>,
    #[live] pub pose: Field<kind::Any>,
    #[live] pub expression: Field<kind::Any>,
    #[live] pub costume: Field<kind::Any>,
    #[live] pub mouth: Field<kind::Any>,
    #[live] pub walk: Field<kind::Any>,
    #[live] pub float: Field<kind::Any>,
    #[live] pub emote: Field<kind::Any>,
    #[live] pub nod: Field<kind::Num>,
    #[live] pub bob: Field<kind::Num>,
    #[live] pub blink: Field<kind::Num>,
    #[live] pub seed: Field<kind::Num>,
    #[live] pub sway: Field<kind::Num>,
    #[live] pub vary: Field<kind::Num>,
    #[live] pub lip_sync: Field<kind::Any>,
    #[live] pub lead: Field<kind::Num>,
    #[live] pub parts: Field<kind::Any>,
    #[live] pub reach: Field<kind::Any>,
}

/// A heads-up overlay: title, chapters, timecode and brackets.
#[derive(Script, ScriptHook)]
pub struct Hud {
    #[script_splat]
    pub common: Common,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub title: Field<kind::Words>,
    #[live] pub tag: Field<kind::Words>,
    #[live] pub chapters: Field<kind::Data>,
    #[live] pub bpm: Field<kind::Num>,
    #[live] pub timecode: Field<kind::Bool>,
    #[live] pub clock: Field<kind::Any>,
    #[live] pub ink: Field<kind::Color>,
    #[live] pub inset: Field<kind::Num>,
    #[live] pub brackets: Field<kind::Bool>,
    #[live] pub label_size: Field<kind::Num>,
}

/// Words punched in one at a time on the beat.
#[derive(Script, ScriptHook)]
pub struct Words {
    #[script_splat]
    pub common: Common,
    #[live] pub words: Field<kind::Prose>,
    #[live] pub every: Field<kind::Length>,
    #[live] pub start: Field<kind::Time>,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub font: Field<kind::Font>,
    #[live] pub text_size: Field<kind::Num>,
    #[live] pub color: Field<kind::Color>,
    #[live] pub bg: Field<kind::Color>,
    #[live] pub look: Field<kind::Any>,
    #[live] pub hold: Field<kind::Length>,
}

/// A camera flight into one letter of its layers.
#[derive(Script, ScriptHook)]
pub struct FlyThrough {
    #[script_splat]
    pub common: Common,
    #[live] pub layers: Field<kind::Layers>,
    #[live] pub into: Field<kind::Any>,
    #[live] pub letter: Field<kind::Num>,
    #[live] pub time: Field<kind::Time>,
    #[live] pub duration: Field<kind::Num>,
    #[live] pub zoom: Field<kind::Num>,
}

/// A vertical short-form layout: tag, headline, inset clip and meta.
#[derive(Script, ScriptHook)]
pub struct Shorts {
    #[script_splat]
    pub common: Common,
    #[live] pub tag: Field<kind::Words>,
    #[live] pub headline: Field<kind::Words>,
    #[live] pub accent: Field<kind::Color>,
    #[live] pub inset: Field<kind::Any>,
    #[live] pub inset_size: Field<kind::Vec2>,
    #[live] pub meta: Field<kind::Words>,
    #[live] pub clip: Field<kind::Any>,
    #[live] pub total: Field<kind::Any>,
    #[live] pub floor: Field<kind::Bool>,
    #[live] pub ink: Field<kind::Color>,
    #[live] pub accent_color: Field<kind::Color>,
    #[live] pub background: Field<kind::Color>,
}

/// Tokens streamed out of a prompt, with candidates.
#[derive(Script, ScriptHook)]
pub struct TokenStream {
    #[script_splat]
    pub common: Common,
    #[live] pub prompt: Field<kind::Words>,
    #[live] pub tokens: Field<kind::Data>,
    #[live] pub times: Field<kind::Data>,
    #[live] pub every: Field<kind::Length>,
    #[live] pub start: Field<kind::Time>,
    #[live] pub words: Field<kind::Prose>,
    #[live] pub line: Field<kind::Any>,
    #[live] pub header: Field<kind::Any>,
    #[live] pub candidates: Field<kind::Data>,
    #[live] pub decoys: Field<kind::Data>,
    #[live] pub top_k: Field<kind::Num>,
    #[live] pub accent: Field<kind::Color>,
    #[live] pub ink: Field<kind::Color>,
    #[live] pub panel: Field<kind::Color>,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub enter: Field<kind::Bool>,
}

/// A form filled in field by field, stamped.
#[derive(Script, ScriptHook)]
pub struct Form {
    #[script_splat]
    pub common: Common,
    #[live] pub title: Field<kind::Words>,
    #[live] pub subtitle: Field<kind::Words>,
    #[live] pub fields: Field<kind::Data>,
    #[live] pub checks: Field<kind::Data>,
    #[live] pub signature: Field<kind::Words>,
    #[live] pub stamp: Field<kind::Words>,
    #[live] pub stamp2: Field<kind::Words>,
    #[live] pub start: Field<kind::Time>,
    #[live] pub every: Field<kind::Length>,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub ink: Field<kind::Color>,
    #[live] pub paper: Field<kind::Color>,
    #[live] pub stamp_color: Field<kind::Color>,
}

/// Code typed in beside the layers it draws.
#[derive(Script, ScriptHook)]
pub struct CodeDraw {
    #[script_splat]
    pub common: Common,
    #[live] pub code: Field<kind::Code>,
    #[live] pub layers: Field<kind::Layers>,
    #[live] pub every: Field<kind::Length>,
    #[live] pub start: Field<kind::Time>,
    #[live] pub split: Field<kind::Any>,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub ink: Field<kind::Color>,
    #[live] pub muted: Field<kind::Color>,
    #[live] pub panel: Field<kind::Color>,
}

/// A UI window typing out its lines.
#[derive(Script, ScriptHook)]
pub struct UiWindow {
    #[script_splat]
    pub common: Common,
    #[live] pub title: Field<kind::Words>,
    #[live] pub lines: Field<kind::Data>,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub start: Field<kind::Time>,
    #[live] pub cps: Field<kind::Num>,
    #[live] pub accent: Field<kind::Color>,
    #[live] pub ink: Field<kind::Color>,
    #[live] pub panel: Field<kind::Color>,
    #[live] pub button: Field<kind::Words>,
}

/// A UI button clicked by a cursor.
#[derive(Script, ScriptHook)]
pub struct UiButton {
    #[script_splat]
    pub common: Common,
    #[live] pub label: Field<kind::Words>,
    #[live] pub click: Field<kind::Time>,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub accent: Field<kind::Color>,
    #[live] pub ink: Field<kind::Color>,
    #[live] pub cursor: Field<kind::Bool>,
}

/// A typeset formula: one Path per glyph and rule, addressable by `\id` name, path or character.
#[derive(Script, ScriptHook)]
pub struct Math {
    #[script_splat]
    pub common: Common,
    #[live] pub tex: Field<kind::Prose>,
    #[live] pub size: Field<kind::Num>,
    #[live] pub color: Field<kind::Color>,
    #[live] pub display: Field<kind::Bool>,
    #[live] pub align: Field<kind::Align>,
    #[live] pub font: Field<kind::Font>,
    #[live] pub parts: Field<kind::Any>,
    #[live] pub paths: Field<kind::Any>,
    #[live] pub chars: Field<kind::Any>,
}

/// A data-coordinate box: `x` and `y` are [min, max, optional tick step].
/// `at` is its top-left in pixels; items in `layers` use data coordinates.
#[derive(Script, ScriptHook)]
pub struct Plot {
    #[script_splat]
    pub common: Common,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub layers: Field<kind::Layers>,
    #[live] pub grid: Field<kind::Any>,
    #[live] pub labels: Field<kind::Bool>,
    #[live] pub x_label: Field<kind::Words>,
    #[live] pub y_label: Field<kind::Words>,
    #[live] pub color: Field<kind::Color>,
    #[live] pub grid_color: Field<kind::Color>,
    #[live] pub label_size: Field<kind::Num>,
    #[live] pub width: Field<kind::Num>,
    #[live] pub reveal: Field<kind::Data>,
    /// `map: [[a, b], [c, d]]` or `map: fn(p) { vec2(…) }`: every data point moved before drawing.
    #[live] pub map: Field<kind::Any>,
    /// How far the map is applied, 0 (identity) to 1 (default); animatable.
    #[live] pub map_at: Field<kind::Num>,
    /// Samples per second of a baked map animation (30).
    #[live] pub fps: Field<kind::Num>,
}

/// Rect, Ellipse, Path and Text outlines combined by `op` (`@union`,
/// `@intersection`, `@difference`, `@xor`) into one filled Path.
#[derive(Script, ScriptHook)]
pub struct Shape {
    #[script_splat]
    pub common: Common,
    #[live] pub op: Field<kind::Any>,
    #[live] pub layers: Field<kind::Layers>,
    #[live] pub fill: Field<kind::Color>,
    #[live] pub stroke: Field<kind::Color>,
    #[live] pub width: Field<kind::Num>,
    #[live] pub trim_start: Field<kind::Num>,
    #[live] pub trim_end: Field<kind::Num>,
    #[live] pub cap: Field<kind::Cap>,
    #[live] pub join: Field<kind::Join>,
}

/// Shared Path look and Common fields for items inside a Plot. `color` is
/// shorthand for stroke (fill on Dot); `fps` sets load-time geometry sampling.
#[derive(Script, ScriptHook, Default)]
pub struct PlotItem {
    #[script_splat]
    pub common: Common,
    #[live] pub color: Field<kind::Color>,
    #[live] pub stroke: Field<kind::Color>,
    #[live] pub fill: Field<kind::Color>,
    #[live] pub width: Field<kind::Num>,
    #[live] pub trim_start: Field<kind::Num>,
    #[live] pub trim_end: Field<kind::Num>,
    #[live] pub cap: Field<kind::Cap>,
    #[live] pub join: Field<kind::Join>,
    #[live] pub fps: Field<kind::Num>,
    /// Points of a moving curve, drawn on a fixed grid (400).
    #[live] pub samples: Field<kind::Num>,
}

/// `y = f(x)` in a Plot, adaptively sampled, split at poles and clipped.
/// `f(x, t)` and animated `from`/`to` become one Path with SVG keys.
#[derive(Script, ScriptHook)]
pub struct Graph {
    #[script_splat] pub look: PlotItem,
    #[live] pub f: Field<kind::Code>,
    #[live] pub from: Field<kind::Num>,
    #[live] pub to: Field<kind::Num>,
}

/// A Plot curve `f(s)` returning vec2(x, y), optionally `f(s, t)`.
#[derive(Script, ScriptHook)]
pub struct Parametric {
    #[script_splat] pub look: PlotItem,
    #[live] pub f: Field<kind::Code>,
    #[live] pub from: Field<kind::Num>,
    #[live] pub to: Field<kind::Num>,
}

/// The zero set of `f(x, y)` in a Plot, joined into open or closed contours.
/// `cells` is the number of square data cells across the longer side.
#[derive(Script, ScriptHook)]
pub struct Implicit {
    #[script_splat] pub look: PlotItem,
    #[live] pub f: Field<kind::Code>,
    #[live] pub cells: Field<kind::Num>,
}

/// One Path of arrows `f(x, y) -> vec2`, spaced by `step` data units.
/// Common's `scale` here is the vector length multiplier in data units.
#[derive(Script, ScriptHook)]
pub struct VectorField {
    #[script_splat] pub look: PlotItem,
    #[live] pub f: Field<kind::Code>,
    #[live] pub step: Field<kind::Num>,
}

/// `n` rectangles between y=0 and f(x) in a Plot; side is @left/@right/@mid.
#[derive(Script, ScriptHook)]
pub struct Riemann {
    #[script_splat] pub look: PlotItem,
    #[live] pub f: Field<kind::Code>,
    #[live] pub from: Field<kind::Num>,
    #[live] pub to: Field<kind::Num>,
    #[live] pub n: Field<kind::Num>,
    #[live] pub side: Field<kind::Any>,
}

/// A filled Plot region between `f(x)` and `g(x)` (default y=0).
#[derive(Script, ScriptHook)]
pub struct Area {
    #[script_splat] pub look: PlotItem,
    #[live] pub f: Field<kind::Code>,
    #[live] pub g: Field<kind::Code>,
    #[live] pub from: Field<kind::Num>,
    #[live] pub to: Field<kind::Num>,
}

/// A Plot tangent to f at Common's `x`, with central-difference slope.
/// `length` is in data x units; `dot: true` adds a filled 7 pixel circle.
#[derive(Script, ScriptHook)]
pub struct Tangent {
    #[script_splat] pub look: PlotItem,
    #[live] pub f: Field<kind::Code>,
    #[live] pub length: Field<kind::Num>,
    #[live] pub dot: Field<kind::Bool>,
}

/// A filled circle at Common's `at` in Plot data coordinates (animatable).
/// `radius` is in pixels; `label`/`tex` adds Text/Math up-right of the point.
#[derive(Script, ScriptHook)]
pub struct Dot {
    #[script_splat] pub look: PlotItem,
    #[live] pub radius: Field<kind::Num>,
    #[live] pub label: Field<kind::Words>,
    #[live] pub tex: Field<kind::Words>,
    #[live] pub label_size: Field<kind::Num>,
}

/// One layout turning into another by flying the pieces they share (a word,
/// a character, an `\id` part of a formula) and fading the rest.
#[derive(Script, ScriptHook)]
pub struct MatchMorph {
    #[script_splat]
    pub common: Common,
    #[live] pub steps: Field<kind::Any>,
    #[live] pub times: Field<kind::Any>,
    #[live] pub durations: Field<kind::Any>,
    #[live] pub by: Field<kind::Any>,
    #[live] pub math: Field<kind::Bool>,
    #[live] pub display: Field<kind::Bool>,
    #[live] pub font: Field<kind::Font>,
    #[live] pub size: Field<kind::Num>,
    #[live] pub tracking: Field<kind::Num>,
    #[live] pub color: Field<kind::Color>,
    #[live] pub colors: Field<kind::Any>,
    #[live] pub enter_color: Field<kind::Color>,
    #[live] pub settle: Field<kind::Num>,
    #[live] pub parts: Field<kind::Any>,
    #[live] pub paths: Field<kind::Any>,
    #[live] pub chars: Field<kind::Any>,
    #[live] pub align: Field<kind::Align>,
    #[live] pub duration: Field<kind::Num>,
    #[live] pub ease: Field<kind::Any>,
}

/// An easing curve drawn with its moving dot.
#[derive(Script, ScriptHook)]
pub struct EaseGraph {
    #[script_splat]
    pub common: Common,
    #[live] pub curve: Field<kind::Any>,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub color: Field<kind::Color>,
    #[live] pub accent: Field<kind::Color>,
    #[live] pub ink: Field<kind::Color>,
    #[live] pub start: Field<kind::Time>,
    #[live] pub duration: Field<kind::Length>,
    #[live] pub label: Field<kind::Words>,
    #[live] pub dots: Field<kind::Any>,
    #[live] pub font: Field<kind::Font>,
    #[live] pub fill_at: Field<kind::Any>,
}

/// Particles that gather into text and release.
#[derive(Script, ScriptHook)]
pub struct ParticleText {
    #[script_splat]
    pub common: Common,
    #[live] pub text: Field<kind::Words>,
    #[live] pub stages: Field<kind::Data>,
    #[live] pub hold: Field<kind::Length>,
    #[live] pub r#move: Field<kind::Num>,
    #[live] pub font: Field<kind::Font>,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub r#box: Field<kind::Vec2>,
    #[live] pub count: Field<kind::Num>,
    #[live] pub radius: Field<kind::Num>,
    #[live] pub seed: Field<kind::Num>,
    #[live] pub start: Field<kind::Time>,
    #[live] pub converge: Field<kind::Num>,
    #[live] pub release: Field<kind::Num>,
    #[live] pub wind: Field<kind::Vec2>,
    #[live] pub swirl: Field<kind::Num>,
    #[live] pub colors: Field<kind::Data>,
    #[live] pub glow: Field<kind::Num>,
}

/// A ball bouncing with squash and labels.
#[derive(Script, ScriptHook)]
pub struct Bounce {
    #[script_splat]
    pub common: Common,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub color: Field<kind::Color>,
    #[live] pub radius: Field<kind::Num>,
    #[live] pub height: Field<kind::Num>,
    #[live] pub bounces: Field<kind::Num>,
    #[live] pub restitution: Field<kind::Num>,
    #[live] pub squash: Field<kind::Num>,
    #[live] pub start: Field<kind::Time>,
    #[live] pub duration: Field<kind::Length>,
    #[live] pub baseline: Field<kind::Any>,
    #[live] pub labels: Field<kind::Bool>,
    #[live] pub ink: Field<kind::Color>,
    #[live] pub font: Field<kind::Font>,
}

/// A scrolling perspective grid floor.
#[derive(Script, ScriptHook)]
pub struct GridFloor {
    #[script_splat]
    pub common: Common,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub color: Field<kind::Color>,
    #[live] pub horizon: Field<kind::Num>,
    #[live] pub cells: Field<kind::Num>,
    #[live] pub speed: Field<kind::Num>,
    #[live] pub glow: Field<kind::Num>,
    #[live] pub fade: Field<kind::Num>,
    #[live] pub width: Field<kind::Num>,
    #[live] pub sky: Field<kind::Num>,
}

/// Blobs that merge around a centre.
#[derive(Script, ScriptHook)]
pub struct Metaballs {
    #[script_splat]
    pub common: Common,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub color: Field<kind::Color>,
    #[live] pub colors: Field<kind::Data>,
    #[live] pub satellites: Field<kind::Num>,
    #[live] pub radius: Field<kind::Num>,
    #[live] pub orbit: Field<kind::Num>,
    #[live] pub k: Field<kind::Num>,
    #[live] pub speed: Field<kind::Num>,
    #[live] pub wobble: Field<kind::Num>,
    #[live] pub background: Field<kind::Color>,
    #[live] pub caption: Field<kind::Words>,
    #[live] pub ink: Field<kind::Color>,
    #[live] pub font: Field<kind::Font>,
}

/// Light rays bursting from a centre.
#[derive(Script, ScriptHook)]
pub struct Rays {
    #[script_splat]
    pub common: Common,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub color: Field<kind::Color>,
    #[live] pub center: Field<kind::Vec2>,
    #[live] pub count: Field<kind::Num>,
    #[live] pub spread: Field<kind::Num>,
    #[live] pub speed: Field<kind::Num>,
    #[live] pub reach: Field<kind::Num>,
    #[live] pub burst: Field<kind::Bool>,
    #[live] pub intensity: Field<kind::Num>,
}

/// A grid warped by a mass.
#[derive(Script, ScriptHook)]
pub struct GridWarp {
    #[script_splat]
    pub common: Common,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub color: Field<kind::Color>,
    #[live] pub center: Field<kind::Vec2>,
    #[live] pub cells: Field<kind::Num>,
    #[live] pub mass: Field<kind::Num>,
    #[live] pub mode: Field<kind::Any>,
    #[live] pub ring: Field<kind::Bool>,
    #[live] pub speed: Field<kind::Num>,
    #[live] pub width: Field<kind::Num>,
}

/// A branching tree growing.
#[derive(Script, ScriptHook)]
pub struct Branches {
    #[script_splat]
    pub common: Common,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub style: Field<kind::Any>,
    #[live] pub from: Field<kind::Any>,
    #[live] pub angle: Field<kind::Num>,
    #[live] pub length: Field<kind::Num>,
    #[live] pub depth: Field<kind::Num>,
    #[live] pub spread: Field<kind::Num>,
    #[live] pub shrink: Field<kind::Num>,
    #[live] pub seed: Field<kind::Num>,
    #[live] pub color: Field<kind::Color>,
    #[live] pub width: Field<kind::Num>,
    #[live] pub start: Field<kind::Time>,
    #[live] pub grow: Field<kind::Num>,
    #[live] pub labels: Field<kind::Bool>,
    #[live] pub ink: Field<kind::Color>,
    #[live] pub font: Field<kind::Font>,
}

/// A layer turning into another: `from` until `time`, then morphing into
/// `to` over `duration` seconds (what `Text{morph: {…}}` makes).
#[derive(Script, ScriptHook)]
pub struct Morph {
    #[script_splat]
    pub common: Common,
    #[live] pub from: Field<kind::Layer>,
    #[live] pub to: Field<kind::Layer>,
    #[live] pub time: Field<kind::Time>,
    #[live] pub duration: Field<kind::Num>,
}

/// A world-space 2D camera, lowered to origin-pivoted Groups. `center` maps
/// to `screen` (the document centre by default); zoom scales document units.
/// Positive roll is clockwise camera rotation, so the world turns counter-clockwise.
#[derive(Script, ScriptHook)]
pub struct Camera {
    #[script_splat] pub common: Common,
    #[live] pub center: Field<kind::Vec2>,
    #[live] pub zoom: Field<kind::Num>,
    #[live] pub roll: Field<kind::Num>,
    #[live] pub screen: Field<kind::Vec2>,
    #[live] pub layers: Field<kind::Layers>,
}

/// A clipped, uniformly scaled copy of a named layer's document-space region.
/// `at`/`size` place the inset box. Use static `region: [x, y, w, h]`, or
/// animatable `region_at`/`region_size`, sampled into ordinary keys at 30 fps.
#[derive(Script, ScriptHook)]
pub struct Zoomed {
    #[script_splat] pub common: Common,
    #[live] pub of: Field<kind::Words>,
    #[live] pub region: Field<kind::Data>,
    #[live] pub region_at: Field<kind::Vec2>,
    #[live] pub region_size: Field<kind::Vec2>,
    #[live] pub size: Field<kind::Vec2>,
    #[live] pub frame: Field<kind::Color>,
    #[live] pub width: Field<kind::Num>,
    #[live] pub connect: Field<kind::Bool>,
    #[live] pub background: Field<kind::Color>,
    #[live] pub radius: Field<kind::Num>,
}

/// An immediate-mode 2D layer: `draw: fn(c, t, edit) {…}` records into `c` every frame.
#[derive(Script, ScriptHook)]
pub struct Draw2D {
    #[script_splat]
    pub common: Common,
    #[live] pub draw: Field<kind::Code>,
    #[live] pub size: Field<kind::Vec2>,
}


// ---- values ------------------------------------------------------------
//
// What the helpers of `mod.motion` make (`tween(…)`, `beat(…)`, `in(…)`,
// …): components the loader reads by their type.

/// A weighted window over a Text's units, applied after their stagger.
#[derive(Script, ScriptHook, Default)]
pub struct Range {
    #[live] pub by: Field<kind::TextBy>,
    #[live] pub from: Field<kind::Num>,
    #[live] pub to: Field<kind::Num>,
    #[live] pub slide: Field<kind::Num>,
    #[live] pub soft: Field<kind::Num>,
    #[live] pub ease: Field<kind::Ease>,
    #[live] pub invert: Field<kind::Bool>,
    #[live] pub x: Field<kind::Num>,
    #[live] pub y: Field<kind::Num>,
    #[live] pub rotate: Field<kind::Num>,
    #[live] pub blur: Field<kind::Num>,
    #[live] pub scale: Field<kind::Vec2>,
    #[live] pub opacity: Field<kind::Num>,
    #[live] pub color: Field<kind::Color>,
    #[live] pub wght: Field<kind::Num>,
    #[live] pub wdth: Field<kind::Num>,
    #[live] pub slnt: Field<kind::Num>,
    #[live] pub axes: Field<kind::Data>,
}

/// `tween(from, to, start, end, ease)`: from `from` at `start`, eased to `to` at `end`.
#[derive(Script, ScriptHook, Default)]
pub struct Tween {
    #[live] pub from: Field<kind::Any>,
    #[live] pub to: Field<kind::Any>,
    #[live] pub start: Field<kind::Time>,
    #[live] pub end: Field<kind::Time>,
    #[live] pub ease: Field<kind::Ease>,
}

/// `keys([t, v], [t, v, @ease], …)`: keys at increasing times.
#[derive(Script, ScriptHook, Default)]
pub struct Keys {
    #[live] pub points: Field<kind::Data>,
}

/// `sequence([[v, seconds, @ease], …], start)`: each step's value after its duration.
#[derive(Script, ScriptHook, Default)]
pub struct Sequence {
    #[live] pub steps: Field<kind::Data>,
    #[live] pub start: Field<kind::Time>,
}

/// `blend(x, stops, ease)`: a value selected between adjacent stops.
#[derive(Script, ScriptHook, Default)]
pub struct Blend {
    #[live] pub x: Field<kind::Num>,
    #[live] pub stops: Field<kind::Data>,
    #[live] pub ease: Field<kind::Ease>,
}

/// `blend2(x, y, grid, ease)`: bilinear values on a rectangular grid.
#[derive(Script, ScriptHook, Default)]
pub struct Blend2 {
    #[live] pub x: Field<kind::Num>,
    #[live] pub y: Field<kind::Num>,
    #[live] pub grid: Field<kind::Data>,
    #[live] pub ease: Field<kind::Ease>,
}

/// `repeat(anim, period, @restart | @pingpong, count)`: an animation looped.
#[derive(Script, ScriptHook, Default)]
pub struct Loop {
    #[live] pub inner: Field<kind::Any>,
    #[live] pub period: Field<kind::Length>,
    #[live] pub mode: Field<kind::Any>,
    #[live] pub count: Field<kind::Num>,
}

/// `spring(k, d)`: a damped spring ease.
#[derive(Script, ScriptHook, Default)]
pub struct Spring {
    #[live] pub k: Field<kind::Num>,
    #[live] pub d: Field<kind::Num>,
}

/// `bezier(x1, y1, x2, y2)`: a CSS cubic-bezier ease.
#[derive(Script, ScriptHook, Default)]
pub struct Bezier {
    #[live] pub x1: Field<kind::Num>,
    #[live] pub y1: Field<kind::Num>,
    #[live] pub x2: Field<kind::Num>,
    #[live] pub y2: Field<kind::Num>,
}

/// A rate shaper over an ease (`wiggle(@ease_out, 3)`, `squish(@linear, 0.2, 0.8)`):
/// `shape` its name, `of` the ease it reshapes, `a` / `b` its numbers (the
/// shaper's defaults when nil).
#[derive(Script, ScriptHook, Default)]
pub struct Rate {
    #[live] pub shape: Field<kind::Any>,
    #[live] pub of: Field<kind::Any>,
    #[live] pub a: Field<kind::Num>,
    #[live] pub b: Field<kind::Num>,
}

/// `beat(n, offset)`: the n-th beat (from 0).
#[derive(Script, ScriptHook, Default)]
pub struct Beat {
    #[live] pub n: Field<kind::Num>,
    #[live] pub offset: Field<kind::Num>,
}

/// `bar(n, beat, offset)`: a beat of the n-th bar.
#[derive(Script, ScriptHook, Default)]
pub struct Bar {
    #[live] pub n: Field<kind::Num>,
    #[live] pub beat: Field<kind::Num>,
    #[live] pub offset: Field<kind::Num>,
}

/// `downbeat(n, offset)`: the n-th bar's first beat.
#[derive(Script, ScriptHook, Default)]
pub struct Downbeat {
    #[live] pub n: Field<kind::Num>,
    #[live] pub offset: Field<kind::Num>,
}

/// `onset(n, offset)`: the n-th onset, or a named one.
#[derive(Script, ScriptHook, Default)]
pub struct Onset {
    #[live] pub n: Field<kind::Num>,
    #[live] pub offset: Field<kind::Num>,
}

/// `section(@name, offset, nth)`: where a section of the song starts.
#[derive(Script, ScriptHook, Default)]
pub struct Section {
    #[live] pub name: Field<kind::Words>,
    #[live] pub offset: Field<kind::Num>,
    #[live] pub nth: Field<kind::Num>,
}

/// `word(@voice, index, offset, nth, after)`: when a voice says a word.
#[derive(Script, ScriptHook, Default)]
pub struct Word {
    #[live] pub voice: Field<kind::Any>,
    #[live] pub index: Field<kind::Num>,
    #[live] pub offset: Field<kind::Num>,
    #[live] pub nth: Field<kind::Num>,
    #[live] pub after: Field<kind::Any>,
}

/// `line(@voice, index, offset)`: when a voice starts a line.
#[derive(Script, ScriptHook, Default)]
pub struct Line {
    #[live] pub voice: Field<kind::Any>,
    #[live] pub index: Field<kind::Num>,
    #[live] pub offset: Field<kind::Num>,
}

/// `edit(s, offset)`: second `s` of the host edit.
#[derive(Script, ScriptHook, Default)]
pub struct Edit {
    #[live] pub s: Field<kind::Any>,
    #[live] pub offset: Field<kind::Num>,
}

/// `beats(n)`: a length in beats.
#[derive(Script, ScriptHook, Default)]
pub struct Beats {
    #[live] pub n: Field<kind::Num>,
}

/// `in(@name)`: a declared input's value, where the loader places it.
#[derive(Script, ScriptHook, Default)]
pub struct In {
    #[live] pub name: Field<kind::Words>,
}

/// `item(@field)`: a Repeat copy's item of its `source` list.
#[derive(Script, ScriptHook, Default)]
pub struct Item {
    #[live] pub field: Field<kind::Words>,
}

/// `input(@type, default, "doc")`: a typed input declaration.
#[derive(Script, ScriptHook, Default)]
pub struct Input {
    #[live] pub ty: Field<kind::Any>,
    #[live] pub default: Field<kind::Any>,
    #[live] pub doc: Field<kind::Prose>,
}

/// `asset(id, rev)`: an exact library revision.
#[derive(Script, ScriptHook, Default)]
pub struct Asset {
    #[live] pub id: Field<kind::Words>,
    #[live] pub rev: Field<kind::Num>,
}

/// A declared optional tree media entry without content.
#[derive(Script, ScriptHook, Default)]
pub struct MissingMedia {
    #[live] pub path: Field<kind::Words>,
}

/// `along(path, progress)`: an origin riding a path.
#[derive(Script, ScriptHook, Default)]
pub struct Along {
    #[live] pub path: Field<kind::Any>,
    #[live] pub progress: Field<kind::Num>,
}

/// `stagger(step, @order, seed)`: units' delays.
#[derive(Script, ScriptHook, Default)]
pub struct Stagger {
    #[live] pub step: Field<kind::Num>,
    #[live] pub order: Field<kind::StaggerOrder>,
    #[live] pub seed: Field<kind::Num>,
}

/// `playhead("name")`: a named layer's own seconds.
#[derive(Script, ScriptHook, Default)]
pub struct Playhead {
    #[live] pub name: Field<kind::Words>,
}

/// `progress("name", from, to)`: a named layer's progress over its span.
#[derive(Script, ScriptHook, Default)]
pub struct Progress {
    #[live] pub name: Field<kind::Words>,
    #[live] pub from: Field<kind::Num>,
    #[live] pub to: Field<kind::Num>,
}

/// `layer("name", series)`: another layer's path for a Text to ride.
#[derive(Script, ScriptHook, Default)]
pub struct LayerPath {
    #[live] pub name: Field<kind::Words>,
    #[live] pub series: Field<kind::Any>,
}

/// `anchor("scene", "node")`: a 3D node a 2D layer follows.
#[derive(Script, ScriptHook, Default)]
pub struct Anchor {
    #[live] pub scene: Field<kind::Any>,
    #[live] pub node: Field<kind::Any>,
}

/// `Font{family: "Roboto Flex" weight: 700 axes: {wdth: 100}}`: a font face as a
/// value (a document font's family, its weight, italic and variation axes), what
/// every text call and layer takes as `font`, so a face is named once and an
/// editor shows a font picker for it.
#[derive(Script, ScriptHook, Default)]
pub struct Font {
    #[live] pub family: Field<kind::Font>,
    #[live] pub weight: Field<kind::Num>,
    #[live] pub italic: Field<kind::Bool>,
    #[live] pub axes: Field<kind::Data>,
}

/// A value component: its name and what makes it.
struct ValueType {
    name: &'static str,
    make: fn(&mut ScriptVm) -> ScriptValue,
}

macro_rules! value_type {
    ($ty:ident) => {
        ValueType { name: stringify!($ty), make: <$ty as ScriptNew>::script_component }
    };
}

/// The value components.
const VALUES: &[ValueType] = &[
    value_type!(Bone),
    value_type!(IK),
    value_type!(Follow),
    value_type!(Range),
    value_type!(Tween),
    value_type!(Keys),
    value_type!(Sequence),
    value_type!(Blend),
    value_type!(Blend2),
    value_type!(Loop),
    value_type!(Spring),
    value_type!(Bezier),
    value_type!(Rate),
    value_type!(Beat),
    value_type!(Bar),
    value_type!(Downbeat),
    value_type!(Onset),
    value_type!(Section),
    value_type!(Word),
    value_type!(Line),
    value_type!(Edit),
    value_type!(Beats),
    value_type!(In),
    value_type!(Item),
    value_type!(Input),
    value_type!(Asset),
    value_type!(MissingMedia),
    value_type!(Along),
    value_type!(Stagger),
    value_type!(Playhead),
    value_type!(Progress),
    value_type!(LayerPath),
    value_type!(Anchor),
    value_type!(Font),
];

/// A layer type: its name in documents and its component.
pub struct LayerType {
    pub name: &'static str,
    make: fn(&mut ScriptVm) -> ScriptValue,
    type_id: fn() -> ScriptTypeId,
}

macro_rules! layer_type {
    ($ty:ident) => {
        LayerType { name: stringify!($ty), make: <$ty as ScriptNew>::script_component, type_id: <$ty as ScriptNew>::script_type_id_static }
    };
}

/// The layer types, in the order the guide lists them.
pub const LAYERS: &[LayerType] = &[
    layer_type!(Rect),
    layer_type!(Ellipse),
    layer_type!(Path),
    layer_type!(Svg),
    layer_type!(Vector),
    layer_type!(Text),
    layer_type!(Image),
    layer_type!(Video),
    layer_type!(Group),
    layer_type!(Bones),
    layer_type!(Repeat),
    layer_type!(Pattern),
    layer_type!(Chart),
    layer_type!(Shader),
    layer_type!(KineticText),
    layer_type!(Text3D),
    layer_type!(Seq3D),
    layer_type!(Scene3D),
    layer_type!(Widget),
    layer_type!(Crowd),
    layer_type!(Freeze),
    layer_type!(Comic),
    layer_type!(Polaroid),
    layer_type!(Character),
    layer_type!(Hud),
    layer_type!(Words),
    layer_type!(FlyThrough),
    layer_type!(Shorts),
    layer_type!(TokenStream),
    layer_type!(Form),
    layer_type!(CodeDraw),
    layer_type!(UiWindow),
    layer_type!(UiButton),
    layer_type!(EaseGraph),
    layer_type!(ParticleText),
    layer_type!(Bounce),
    layer_type!(GridFloor),
    layer_type!(Metaballs),
    layer_type!(Rays),
    layer_type!(GridWarp),
    layer_type!(Branches),
    layer_type!(Math),
    layer_type!(Shape),
    layer_type!(MatchMorph),
    layer_type!(Plot),
    layer_type!(Graph),
    layer_type!(Parametric),
    layer_type!(Implicit),
    layer_type!(VectorField),
    layer_type!(Riemann),
    layer_type!(Area),
    layer_type!(Tangent),
    layer_type!(Dot),
    layer_type!(Draw2D),
    layer_type!(Morph),
    layer_type!(Camera),
    layer_type!(Zoomed),
];

/// The layer types' names.
pub fn layer_types() -> impl Iterator<Item = &'static str> {
    LAYERS.iter().map(|layer| layer.name)
}

/// Objects documents make but whose fields the loader checks itself (an
/// `Effect`'s are the render kit's, a post kit's the render graph's): plain
/// prototypes, known by type like the components.
/// A `voice()` or `music()` object carries its data and methods (and its
/// `id`).
const PLAIN: &[&str] = &["Effect", "Voice", "Music"];

/// Layer types a 3D scene uses too, with its own fields: a `Scene3D{..}`
/// is a layer and the scene motion3d reads from the same object, a `Text3D`
/// a layer or a node of a scene, a `Seq3D` a layer or a scene on its own.
/// Their prototypes are plain objects (the VM allows any field); Motion's
/// loader checks the fields of the layer it reads (`unknown_fields`), the
/// scene reader those of the scene.
// Camera is also the comp/scene camera record; only its layer use lowers to 2D.
pub(crate) const OPEN: &[&str] = &["Scene3D", "Text3D", "Seq3D", "Camera"];

/// Register the component library in a new document VM's `mod.motion`:
/// the document root, every layer type, and the plain prototypes (with the
/// post vocabulary, `post`). Each is noted in the VM's host state, by which
/// the loader knows an object's type.
pub(crate) fn register(vm: &mut ScriptVm, module: ScriptObject, forms: ScriptObject) {
    // A distinct root-parent value: omitted parent continues the chain.
    put(vm, module, "none", id_lut!(none).into());
    let motion = Motion::script_component(vm);
    put(vm, module, "Motion", motion);
    for layer in LAYERS {
        // An open type's fields are not Motion's alone: the VM checks none
        // (the loader checks the layer's own, see `OPEN`).
        let proto = if OPEN.contains(&layer.name) { vm.bx.heap_mut().new_object().into() } else { (layer.make)(vm) };
        put(vm, module, layer.name, proto);
    }
    for value in VALUES {
        let proto = (value.make)(vm);
        // The animation forms' records are `mod.forms`' (motion and scene both carry them).
        put(vm, if crate::forms::TYPES.contains(&value.name) { forms } else { module }, value.name, proto);
    }
    for name in PLAIN {
        let proto = vm.bx.heap_mut().new_object();
        put(vm, module, name, proto.into());
    }
}

/// Name the types `names` already in `module` (the post types the render
/// graph registers), so a value's kind reads as its type.
pub(crate) fn name_types(vm: &mut ScriptVm, module: ScriptObject, names: &[&'static str]) {
    for name in names {
        let key = LiveId::from_str_with_lut(name).unwrap_or_else(|_| LiveId::from_str(name));
        if let Some(obj) = vm.bx.heap_ref().value(module, key.into(), NoTrap).as_object() {
            doc_host(vm).types.push((obj, name));
        }
    }
}

fn put(vm: &mut ScriptVm, module: ScriptObject, name: &'static str, proto: ScriptValue) {
    let key = LiveId::from_str_with_lut(name).unwrap_or_else(|_| LiveId::from_str(name));
    vm.bx.heap_mut().set_value_def(module, key.into(), proto);
    if let Some(obj) = proto.as_object() {
        doc_host(vm).types.push((obj, name));
    }
}

/// A new object of registered type `name` with `fields` (what a native
/// makes, `in(@name)`'s placeholder, an `asset()` input, a voice).
pub(crate) fn make(vm: &mut ScriptVm, name: &str, fields: &[(LiveId, ScriptValue)]) -> ScriptObject {
    let proto = doc_host(vm).types.iter().find(|(_, n)| *n == name).map(|(o, _)| *o);
    let object = match proto {
        Some(proto) => vm.bx.heap_mut().new_with_proto(proto.into()),
        None => vm.bx.heap_mut().new_object(),
    };
    for (key, value) in fields {
        vm.bx.heap_mut().set_value_def(object, (*key).into(), *value);
    }
    object
}

/// Whether `name` is a component (a layer, a value type or the document),
/// whose fields the VM checks; the plain prototypes keep their fields in
/// the order written.
pub(crate) fn is_component(name: &str) -> bool {
    name == "Motion" || LAYERS.iter().any(|l| l.name == name) || VALUES.iter().any(|v| v.name == name)
}

/// The type `obj` was made from: the first object of its prototype chain
/// that is a registered type (a document's own `let Card = Rect{…}` is a
/// Rect).
pub(crate) fn type_of(vm: &ScriptVm, obj: ScriptObject) -> Option<&'static str> {
    let host = vm.host.get::<crate::value::DocHost>()?;
    let mut at = Some(obj);
    let mut depth = 0;
    while let Some(o) = at {
        if let Some((_, name)) = host.types.iter().find(|(t, _)| *t == o) {
            return Some(name);
        }
        depth += 1;
        if depth > 64 {
            return None;
        }
        at = vm.bx.heap_ref().proto(o).as_object();
    }
    None
}

/// The fields of layer type `kind`, the common ones first, as its
/// component declares them (empty for an unknown type).
pub fn fields(kind: &str) -> Vec<String> {
    let Some(layer) = LAYERS.iter().find(|layer| layer.name == kind) else { return Vec::new() };
    props(layer.make, layer.type_id)
}

/// The document's own fields (`Motion{…}`), as its component declares them.
pub fn document_fields() -> Vec<String> {
    props(<Motion as ScriptNew>::script_component, <Motion as ScriptNew>::script_type_id_static)
}

/// Whether `field` is one every layer has ([`Common`]).
pub fn is_common(field: &str) -> bool {
    static COMMON: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    COMMON.get_or_init(|| props(Common::script_proto, <Common as ScriptNew>::script_type_id_static)).iter().any(|f| f == field)
}

/// A component's fields in declaration order, read from its registration in
/// a VM of its own.
fn props(make: fn(&mut ScriptVm) -> ScriptValue, type_id: fn() -> ScriptTypeId) -> Vec<String> {
    typed_props(make, type_id).into_iter().map(|(name, _)| name).collect()
}

/// [`props`] with the type of each field.
fn typed_props(make: fn(&mut ScriptVm) -> ScriptValue, type_id: fn() -> ScriptTypeId) -> Vec<(String, ScriptTypeId)> {
    let mut host = ScriptVmHost::new(crate::value::DocHost::default(), ());
    let mut vm = ScriptVm {
        host: host.host_context(),
        bx: Box::new(ScriptVmBase::new()),
    };
    make(&mut vm);
    let Some(ty) = vm.bx.heap_ref().registered_type(type_id()) else { return Vec::new() };
    ty.props().iter_ordered().filter_map(|(id, ty)| id.as_string(|s| s.map(|n| (n.to_string(), ty)))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every field of every layer type has a kind an editor knows, and the
    /// `@name` fields offer the loader's choices.
    #[test]
    fn field_specs_type_every_field() {
        let kinds = kind_types();
        for layer in LAYERS {
            let specs = field_specs(layer.name);
            assert_eq!(specs.len(), fields(layer.name).len(), "{}", layer.name);
            for (name, ty) in typed_props(layer.make, layer.type_id) {
                assert!(kinds.iter().any(|(id, _)| *id == ty), "{}.{name} is not a Field", layer.name);
            }
        }
        let text = field_specs("Text");
        let ty = |name: &str| text.iter().find(|s| s.name == name).map(|s| s.ty.clone());
        assert_eq!(ty("align"), Some(FieldType::Enum { choices: ALIGN }));
        assert_eq!(ty("color"), Some(FieldType::Color));
        assert_eq!(ty("font"), Some(FieldType::Font));
        assert_eq!(ty("x"), Some(FieldType::Number { dims: 1 }));
        assert!(field_specs("NoSuchLayer").is_empty());
    }
}
