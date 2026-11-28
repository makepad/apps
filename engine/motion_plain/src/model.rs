use crate::{Ease, LiveId};
use crate::timing::{Length, TimeExpr};
use std::sync::Arc;

/// 1-based line and column in the document's own source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, PartialOrd, Ord, Hash)]
pub struct Loc {
    pub line: u32,
    pub col: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Error,
    /// The document loads and draws; something is probably not what the
    /// author meant (an unknown font, a key past the duration).
    Warning,
}

/// A problem in a Motion document: where it is, which layer and property it
/// is about, what is wrong, and how to fix it. Written for people and for
/// the agents that author these documents, so every message names the
/// property and every `hint` is a concrete fix.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub severity: Severity,
    /// The file `loc` is in, where it is not the document's own text: an
    /// inline document's is the game's script.
    pub file: Option<Arc<str>>,
    pub loc: Loc,
    /// `layers[2].opacity`, `inputs.accent`, `duration`; empty for the
    /// document as a whole.
    pub path: String,
    pub message: String,
    pub hint: Option<String>,
}

impl Diagnostic {
    pub fn error(loc: Loc, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self { severity: Severity::Error, file: None, loc, path: path.into(), message: message.into(), hint: None }
    }

    pub fn warning(loc: Loc, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self { severity: Severity::Warning, file: None, loc, path: path.into(), message: message.into(), hint: None }
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let severity = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        if let Some(file) = &self.file {
            write!(f, "{file}:")?;
        }
        write!(f, "{}:{}: {severity}: ", self.loc.line, self.loc.col)?;
        if !self.path.is_empty() {
            write!(f, "{}: ", self.path)?;
        }
        write!(f, "{}", self.message)?;
        if let Some(hint) = &self.hint {
            write!(f, "\n    hint: {hint}")?;
        }
        Ok(())
    }
}

/// An exact library revision: `asset(id, rev)`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AssetRef {
    pub id: String,
    pub rev: String,
}

/// A colour, straight (not premultiplied) RGBA in 0..1.
pub type Color = [f64; 4];

/// A time as written and as resolved. `value` is seconds on the clock of
/// the layer it belongs to; musical and spoken references are resolved into
/// it by the host.
#[derive(Clone, Debug, PartialEq)]
pub struct Time {
    pub expr: TimeExpr,
    pub value: f64,
}

impl Time {
    pub fn seconds(value: f64) -> Self {
        Self { expr: TimeExpr::Seconds(value), value }
    }
}

/// A length as written and as resolved (`beats(x)` depends on the tempo).
#[derive(Clone, Debug, PartialEq)]
pub struct Dur {
    pub expr: Length,
    pub value: f64,
}

impl Dur {
    pub fn seconds(value: f64) -> Self {
        Self { expr: Length::Seconds(value), value }
    }
}

/// One key: at `t` the value is `v`; `ease` shapes the segment arriving at
/// it from the previous key, or `bez` makes that segment a bezier curve
/// through both keys' handles.
#[derive(Clone, Debug, PartialEq)]
pub struct Key<const N: usize> {
    pub t: Time,
    pub v: [f64; N],
    pub ease: Ease,
    /// `[t, v, @bezier]`, `[t, v, {ease: @bezier handle: @auto}]`,
    /// `[t, v, {in: [-0.3, 5] out: [0.25, -4]}]`: the segment arriving here
    /// is a cubic bezier, and this key's handles are these.
    pub bez: Option<KeyHandles>,
}

/// How a key's handles are made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum HandleMode {
    /// Smooth through the key (the neighbours' slope).
    Auto,
    /// Smooth, never overshooting the neighbours (flat at a peak). The default.
    #[default]
    Clamped,
    /// Written, in and out on one line.
    Aligned,
    /// Written, each on its own.
    Free,
    /// Pointing at the neighbouring keys (a corner).
    Vector,
}

impl HandleMode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Clamped => "clamped",
            Self::Aligned => "aligned",
            Self::Free => "free",
            Self::Vector => "vector",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "auto" => Self::Auto,
            "clamped" => Self::Clamped,
            "aligned" => Self::Aligned,
            "free" => Self::Free,
            "vector" => Self::Vector,
            _ => return None,
        })
    }

    /// Its handles are written (else computed from the neighbours).
    pub fn written(self) -> bool {
        matches!(self, Self::Aligned | Self::Free)
    }
}

/// A key's handles: its mode, and for a written mode the handles as offsets
/// from the key (seconds, value), one value per component.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct KeyHandles {
    /// The segment arriving at the key is a bezier (else its `ease`).
    pub bezier: bool,
    pub mode: HandleMode,
    pub r#in: Option<[f64; 2]>,
    pub out: Option<[f64; 2]>,
}

/// What a track does before its first key and after its last.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Extrap {
    /// The end key's value. The default.
    #[default]
    Constant,
    /// On along the end key's slope.
    Linear,
    /// The keys again, from the first to the last.
    Cycle,
}

impl Extrap {
    pub fn name(self) -> &'static str {
        match self {
            Self::Constant => "constant",
            Self::Linear => "linear",
            Self::Cycle => "cycle",
        }
    }
}

/// How a looped track repeats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopMode {
    /// Start over at every period.
    Restart,
    /// Play forward, then backward.
    PingPong,
}

/// Keys, optionally looped: `keys(...)`, `tween(...)` and `sequence(...)`
/// all lower to keys; `repeat(...)` adds the loop.
#[derive(Clone, Debug, PartialEq)]
pub struct Track<const N: usize> {
    /// Times strictly increase.
    pub keys: Vec<Key<N>>,
    pub repeat: Option<Repeat>,
    /// Before the first key, after the last (`keys(…, {before: @linear
    /// after: @cycle})`).
    pub before: Extrap,
    pub after: Extrap,
}

impl<const N: usize> Track<N> {
    /// Sampling uses the first and last keys as endpoints. Segment lookup and
    /// endpoint slopes require finite, strictly increasing times in seconds.
    /// Interpolation uses finite values and handle offsets in every component.
    /// A repeating track divides elapsed time by a finite, positive period.
    /// Musical and spoken keys, implicit repeat periods, and SVG morphs use the legacy loader path,
    /// where sampling handles empty keys and guards zero spans and repeat periods.
    pub fn new(keys: Vec<Key<N>>, repeat: Option<Repeat>, before: Extrap, after: Extrap) -> Result<Self, String> {
        if keys.is_empty() {
            return Err("a track needs at least one key".into());
        }
        for (index, key) in keys.iter().enumerate() {
            if !key.t.value.is_finite() {
                return Err(format!("key {index}'s time must be finite"));
            }
            if index > 0 && key.t.value <= keys[index - 1].t.value {
                return Err(format!("key times must increase: key {index} must follow the previous key"));
            }
            if key.v.iter().any(|v| !v.is_finite()) {
                return Err(format!("key {index}'s value must be finite in every component"));
            }
            if let Some(handles) = key.bez {
                for offset in [handles.r#in, handles.out].into_iter().flatten() {
                    if offset.iter().any(|v| !v.is_finite()) {
                        return Err(format!("key {index}'s handle offsets must be finite"));
                    }
                }
            }
        }
        if let Some(repeat) = &repeat {
            if !repeat.period.value.is_finite() || repeat.period.value <= 0.0 {
                return Err("a repeat period must be finite and positive".into());
            }
        }
        Ok(Self { keys, repeat, before, after })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Repeat {
    /// The loop's length; the track's own keys cover `[first key, first key + period)`.
    pub period: Dur,
    pub mode: LoopMode,
    /// How many times it plays; `None` forever.
    pub count: Option<u32>,
}

/// A property function: `fn(t) {...}` (or `fn(t, i)` on per-unit text
/// properties), called at every frame in the document's own VM with the
/// layer's local time. It may read inputs and audio features
/// (`voice(@vo).amplitude(t)`, `music(@m).energy(t)`).
#[derive(Clone, Debug, PartialEq)]
pub struct FnProp {
    /// The function's index in the document's evaluation (stable: the
    /// document evaluates the same way every time).
    pub index: usize,
    pub loc: Loc,
}

/// One blend space for keyable values and named character poses. Values are
/// row-major; a 1D blend has no y driver and one row at 0.
/// [`BlendForm::new`] checks the grid before it is sampled.
#[derive(Clone, Debug, PartialEq)]
pub struct BlendForm<T> {
    pub x: Prop<1>,
    pub y: Option<Prop<1>>,
    pub cols: Vec<f64>,
    pub rows: Vec<f64>,
    pub values: Vec<T>,
    /// Number of components before field shorthand (None for only
    /// functions, whose results must fit the destination field).
    pub value_kind: Option<usize>,
    pub ease: crate::Ease,
}

impl<T> BlendForm<T> {
    /// Segment lookup indexes the first stop of each axis and interpolates
    /// between finite, strictly increasing stops. A 1D blend uses one row at
    /// zero. Row-major lookup requires exactly one value per row and column pair.
    pub fn new(x: Prop<1>, y: Option<Prop<1>>, cols: Vec<f64>, rows: Vec<f64>, values: Vec<T>, value_kind: Option<usize>, ease: Ease) -> Result<Self, String> {
        for (name, axis) in [("cols", &cols), ("rows", &rows)] {
            if axis.is_empty() {
                return Err(format!("blend {name} needs at least one stop"));
            }
            if axis.iter().any(|v| !v.is_finite()) || axis.windows(2).any(|pair| pair[0] >= pair[1]) {
                return Err(format!("blend {name} stops must be finite and strictly increasing"));
            }
        }
        if y.is_none() && rows.as_slice() != [0.0] {
            return Err("a 1D blend has one row at zero".into());
        }
        if rows.len().checked_mul(cols.len()) != Some(values.len()) {
            return Err("a blend needs one value per grid cell".into());
        }
        Ok(Self { x, y, cols, rows, values, value_kind, ease })
    }

    /// Sample at most four values and mix across x, then y. Clamping and
    /// exact stops avoid sampling unused values (including their functions).
    pub fn sample<V>(&self, x: f64, y: f64, mut value: impl FnMut(&T) -> V, mix: impl Fn(V, V, f64) -> V) -> V {
        let (a, b, u) = blend_segment(&self.cols, x);
        let (c, d, v) = blend_segment(&self.rows, y);
        let u = if a == b { 0.0 } else { self.ease.apply(u, 1.0) };
        let v = if c == d { 0.0 } else { self.ease.apply(v, 1.0) };
        let mut row = |r| {
            let first = value(&self.values[r * self.cols.len() + a]);
            if a == b { first } else { mix(first, value(&self.values[r * self.cols.len() + b]), u) }
        };
        let first = row(c);
        if c == d { first } else { mix(first, row(d), v) }
    }
}

/// Adjacent stops and linear weight. Axes are nonempty, finite and strictly
/// increasing (checked by [`BlendForm::new`]). Exact stops and clamped ends select one
/// value, including a singleton axis. NaN follows the first-stop rule used
/// by key tracks; it never indexes outside the grid.
fn blend_segment(stops: &[f64], x: f64) -> (usize, usize, f64) {
    if !(x > stops[0]) { return (0, 0, 0.0); }
    let next = stops.partition_point(|stop| *stop <= x);
    let a = next - 1;
    if next == stops.len() || x == stops[a] { return (a, a, 0.0); }
    (a, next, (x - stops[a]) / (stops[next] - stops[a]))
}

/// An animatable property: a constant, keys, a function or a blend of
/// properties, all sampled on the same layer clock.
#[derive(Clone, Debug, PartialEq)]
pub enum Prop<const N: usize> {
    Const([f64; N]),
    Track(Track<N>),
    Fn(FnProp),
    Blend(Box<BlendForm<Prop<N>>>),
}

impl<const N: usize> Prop<N> {
    /// Sampling returns a constant's components directly. Each must be finite.
    pub fn finite(value: [f64; N]) -> Result<Self, String> {
        if value.iter().any(|v| !v.is_finite()) {
            return Err("a constant property must be finite in every component".into());
        }
        Ok(Self::Const(value))
    }

    pub fn is_animated(&self) -> bool {
        match self {
            Prop::Const(_) => false,
            Prop::Blend(blend) => blend.x.is_animated() || blend.y.as_ref().is_some_and(Prop::is_animated) || blend.values.iter().any(Prop::is_animated),
            _ => true,
        }
    }

    pub fn constant(&self) -> Option<[f64; N]> {
        match self {
            Prop::Const(value) => Some(*value),
            Prop::Blend(blend) => {
                let x = blend.x.constant()?[0];
                let y = match &blend.y { Some(y) => y.constant()?[0], None => 0.0 };
                blend.sample(x, y, Prop::constant, |a, b, w| {
                    let (a, b) = (a?, b?);
                    Some(std::array::from_fn(|i| a[i] + (b[i] - a[i]) * w))
                })
            }
            _ => None,
        }
    }
}

impl Prop<1> {
    pub fn scalar(value: f64) -> Self {
        Prop::Const([value])
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
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

/// Which side of a layer's edges receives the feather transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub enum FeatherSide {
    #[default]
    Both,
    Inner,
    Outer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub enum Cap {
    #[default]
    Round,
    Butt,
    Square,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub enum Join {
    #[default]
    Round,
    Miter,
    Bevel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

/// Where a Text's `at.y` is: the top of its box, its middle or its bottom.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub enum VAlign {
    #[default]
    Top,
    Center,
    Bottom,
}

/// What a Text's animated properties apply to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub enum TextBy {
    /// The text as one piece.
    #[default]
    Whole,
    Word,
    Letter,
    Line,
    /// A voiceover's syllables (`source: voice(@vo)` only).
    Syllable,
}

/// In which order units (letters, words, group children) start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub enum StaggerOrder {
    #[default]
    Forward,
    Reverse,
    /// From the middle outwards.
    Center,
    /// From both ends inwards.
    Edges,
    /// A fixed shuffle from `seed`: the same every frame and every run.
    Random(u32),
}

/// `stagger: 0.03`, `stagger: beats(1/8)` or `stagger(step, @center)`:
/// unit `i` (in `order`) runs its animations `i * step` later.
#[derive(Clone, Debug, PartialEq)]
pub struct Stagger {
    pub step: Dur,
    pub order: StaggerOrder,
}

impl Stagger {
    pub fn none() -> Self {
        Self { step: Dur::seconds(0.0), order: StaggerOrder::Forward }
    }

    /// Unit `index` of `count`'s delay: the tween library's stagger (its
    /// `each` the step, its `from` the order).
    pub fn delay(&self, index: usize, count: usize) -> f64 {
        use makepad_tween::stagger::{Stagger as Spread, StaggerFrom};
        let from = match self.order {
            StaggerOrder::Forward => StaggerFrom::Start,
            StaggerOrder::Reverse => StaggerFrom::End,
            StaggerOrder::Center => StaggerFrom::Center,
            StaggerOrder::Edges => StaggerFrom::Edges,
            StaggerOrder::Random(seed) => StaggerFrom::Random(seed as u64),
        };
        Spread::each(self.step.value).from(from).delay(index as u32, count.max(1) as u32)
    }
}

/// A layer's placement. `at` places the layer's origin (top-left of its
/// box); `x`/`y` add to it (the usual things to animate); `scale` and
/// `rotate` (degrees, clockwise) turn about `anchor`, a fraction of the
/// layer's box (0.5, 0.5 is its centre).
#[derive(Clone, Debug, PartialEq)]
pub struct Transform {
    pub at: Prop<2>,
    pub x: Prop<1>,
    pub y: Prop<1>,
    pub scale: Prop<2>,
    pub rotate: Prop<1>,
    pub anchor: [f64; 2],
    /// `at: along(points_or_svg, progress)`: the origin rides a path, at
    /// `progress` (0..1) of its length; `x`/`y` still add.
    pub along: Option<Along>,
}

/// A path the origin moves along.
#[derive(Clone, Debug, PartialEq)]
pub struct Along {
    /// SVG path data (points become lines).
    pub path: Arc<str>,
    pub progress: Prop<1>,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            at: Prop::Const([0.0, 0.0]),
            x: Prop::scalar(0.0),
            y: Prop::scalar(0.0),
            scale: Prop::Const([1.0, 1.0]),
            rotate: Prop::scalar(0.0),
            anchor: [0.5, 0.5],
            along: None,
        }
    }
}

/// A Path's outline: fixed, or morphing between keyed outlines that share
/// their commands.
#[derive(Clone, Debug, PartialEq)]
pub enum PathShape {
    Static(Arc<makepad_svg::VectorPath>),
    /// Compiled SVG commands with number slots and keyed number lists.
    Morph { template: Arc<crate::geometry::PathTemplate>, track: Track<1>, shapes: Arc<[Vec<f64>]> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub enum MaskMode {
    /// Show the group where the mask is opaque.
    #[default]
    Alpha,
    /// Show it where the mask is transparent.
    Inverse,
    /// Show it by the mask's brightness (a luma matte: white shows).
    Luma,
    /// Show it where the mask is dark.
    LumaInverse,
}

/// Where a `Repeat`'s copies go.
#[derive(Clone, Debug, PartialEq)]
pub enum RepeatLayout {
    /// Rows of `columns`, `spacing` apart, filled left to right.
    Grid { columns: usize, spacing: [f64; 2] },
    /// Round a circle of `radius` from `start` degrees over `sweep`
    /// degrees (clockwise from 12 o'clock); `face` turns each copy along.
    Radial { radius: f64, start: f64, sweep: f64, face: bool },
    /// Along path data, evenly by length; `face` turns each copy along.
    Along { path: Arc<str>, face: bool },
    /// Only the `step` offsets (a line when `step` moves them).
    Line,
}

/// Per-index offsets of a `Repeat` (each times the index), or seeded
/// random ones of up to this much (`jitter`).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct RepeatStep {
    pub x: f64,
    pub y: f64,
    pub rotate: f64,
    pub scale: f64,
    pub opacity: f64,
}

/// `span: [start, length]`: the layer shows on `[start, start + length)` of
/// its parent's clock, and its own clock starts at `start`.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub start: Time,
    pub length: Dur,
}

/// The type of a declared input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InputType {
    Text,
    Number,
    Color,
    Vec2,
    Bool,
    Image,
    Svg,
    Video,
    Model,
    /// A list of values (text, numbers, colours, vec2s, bools, assets or
    /// records): what a `Repeat{source: in(@rows)}` makes one copy per item of.
    List,
}

impl InputType {
    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "text" | "string" => Self::Text,
            "number" | "num" => Self::Number,
            "color" | "colour" => Self::Color,
            "vec2" => Self::Vec2,
            "bool" => Self::Bool,
            "image" => Self::Image,
            "svg" => Self::Svg,
            "video" => Self::Video,
            "model" => Self::Model,
            "list" => Self::List,
            _ => return None,
        })
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Number => "number",
            Self::Color => "color",
            Self::Vec2 => "vec2",
            Self::Bool => "bool",
            Self::Image => "image",
            Self::Svg => "svg",
            Self::Video => "video",
            Self::Model => "model",
            Self::List => "list",
        }
    }
}

/// A value an input holds: its default, or what the host (an edit's
/// `Graphic{params: …}`) gives it.
#[derive(Clone, Debug, PartialEq)]
pub enum InputValue {
    Text(String),
    Number(f64),
    Color(Color),
    Vec2([f64; 2]),
    Bool(bool),
    Asset(AssetRef),
    /// SVG markup given inline.
    Markup(String),
    /// An asset input with no default.
    Empty,
    /// A list input's items.
    List(Vec<InputValue>),
    /// One item of a list given as fields (`{name: "Ada" score: 12}`), read
    /// in a Repeat's copy as `item(@name)`.
    Record(Vec<(String, InputValue)>),
}

impl InputValue {
    pub fn describe(&self) -> &'static str {
        match self {
            Self::Text(_) => "text",
            Self::Number(_) => "a number",
            Self::Color(_) => "a colour",
            Self::Vec2(_) => "a vec2",
            Self::Bool(_) => "a bool",
            Self::Asset(_) => "an asset",
            Self::Markup(_) => "SVG markup",
            Self::Empty => "nothing",
            Self::List(_) => "a list",
            Self::Record(_) => "a record",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct InputDecl {
    /// The input as hosts name it: `in(@name)`'s identifier.
    pub id: LiveId,
    /// Its written name (messages and scene templates).
    pub name: String,
    pub ty: InputType,
    pub default: InputValue,
    pub doc: String,
    pub loc: Loc,
}

/// The identifier of the input named `name`, for a host that holds names (an
/// edit's text, a command line). A host with identifiers uses them.
pub fn input_id(name: &str) -> LiveId {
    LiveId::from_str_with_lut(name).unwrap_or_else(|_| LiveId::from_str(name))
}

pub fn unpack(c: u32) -> Color {
    [((c >> 24) & 0xff) as f64 / 255.0, ((c >> 16) & 0xff) as f64 / 255.0, ((c >> 8) & 0xff) as f64 / 255.0, (c & 0xff) as f64 / 255.0]
}

impl Transform {
    pub fn is_animated(&self) -> bool {
        self.at.is_animated() || self.x.is_animated() || self.y.is_animated()
            || self.scale.is_animated() || self.rotate.is_animated()
            || self.along.as_ref().is_some_and(|along| along.progress.is_animated())
    }
}

impl PathShape {
    pub fn at(&self, t: f64) -> Arc<makepad_svg::VectorPath> {
        match self {
            PathShape::Static(path) => path.clone(),
            PathShape::Morph { template, track, shapes } => {
                // The keyed index, eased: between two outlines.
                let x = crate::sample_track(track, t)[0].clamp(0.0, (shapes.len() - 1) as f64);
                let i = (x.floor() as usize).min(shapes.len().saturating_sub(2));
                let u = x - i as f64;
                let (a, b) = (&shapes[i], &shapes[(i + 1).min(shapes.len() - 1)]);
                template.interpolate(a, b, u).into()
            }
        }
    }
}
