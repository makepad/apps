//! A Motion document at one moment: [`MotionFrame`], the resolved draw
//! list [`crate::MotionDoc::eval`] returns. Every value is final (keys
//! sampled, inputs filled, time references resolved, staggers applied);
//! only what needs a font or a decoded file (text layout, an image's own
//! size) is left to the drawing side.

use crate::model::*;
use std::sync::Arc;

/// A 2D affine map: `x' = a x + c y + e`, `y' = b x + d y + f`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Default for Affine {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Affine {
    pub const IDENTITY: Affine = Affine { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 };

    pub fn translate(x: f64, y: f64) -> Affine {
        Affine { e: x, f: y, ..Self::IDENTITY }
    }

    pub fn scale(x: f64, y: f64) -> Affine {
        Affine { a: x, d: y, ..Self::IDENTITY }
    }

    /// Clockwise by `degrees` (y points down).
    pub fn rotate(degrees: f64) -> Affine {
        let (s, c) = degrees.to_radians().sin_cos();
        Affine { a: c, b: s, c: -s, d: c, e: 0.0, f: 0.0 }
    }

    /// `self` after `other`: a point goes through `other` first.
    pub fn then(&self, other: &Affine) -> Affine {
        // self * other
        Affine {
            a: self.a * other.a + self.c * other.b,
            b: self.b * other.a + self.d * other.b,
            c: self.a * other.c + self.c * other.d,
            d: self.b * other.c + self.d * other.d,
            e: self.a * other.e + self.c * other.f + self.e,
            f: self.b * other.e + self.d * other.f + self.f,
        }
    }

    pub fn apply(&self, p: [f64; 2]) -> [f64; 2] {
        [self.a * p[0] + self.c * p[1] + self.e, self.b * p[0] + self.d * p[1] + self.f]
    }

    /// Whether the map only translates and scales (axis-aligned).
    pub fn is_axis_aligned(&self) -> bool {
        self.b.abs() < 1e-12 && self.c.abs() < 1e-12
    }

    /// How much the map scales lengths, on average.
    pub fn mean_scale(&self) -> f64 {
        ((self.a * self.d - self.b * self.c).abs()).sqrt()
    }

    /// The map about a pivot: translate to it, `local`, and back.
    pub fn about(pivot: [f64; 2], local: &Affine) -> Affine {
        Affine::translate(pivot[0], pivot[1]).then(local).then(&Affine::translate(-pivot[0], -pivot[1]))
    }
}

/// A layer's own scale and rotation, about `anchor` (a fraction of its box).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Local {
    pub anchor: [f64; 2],
    pub scale: [f64; 2],
    /// Degrees, clockwise.
    pub rotate: f64,
}

impl Local {
    pub fn is_identity(&self) -> bool {
        self.scale == [1.0, 1.0] && self.rotate == 0.0
    }

    pub fn affine(&self, box_size: [f64; 2]) -> Affine {
        if self.is_identity() {
            return Affine::IDENTITY;
        }
        let pivot = [self.anchor[0] * box_size[0], self.anchor[1] * box_size[1]];
        Affine::about(pivot, &Affine::rotate(self.rotate).then(&Affine::scale(self.scale[0], self.scale[1])))
    }
}

/// One text unit (a letter, word, line or syllable, or the whole text) as
/// the frame shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct TextUnit {
    /// Byte range of the unit in the item's text.
    pub range: std::ops::Range<usize>,
    /// Offset from where layout puts it, in document units.
    pub offset: [f64; 2],
    /// Scale and rotation about the unit's own centre.
    pub scale: [f64; 2],
    pub rotate: f64,
    pub opacity: f64,
    pub color: Color,
    /// Blur in document units.
    pub blur: f64,
    /// This unit's stepped font axes; empty inherits the item's axes.
    pub axes: Vec<(String, f64)>,
}

/// A Text's plate at this moment.
#[derive(Clone, Debug, PartialEq)]
pub struct PlateFrame {
    pub color: Color,
    pub pad: [f64; 2],
    pub radius: f64,
    pub stroke: Option<Color>,
    pub width: f64,
    pub reveal: f64,
    pub each: bool,
    pub underline: bool,
}

/// A KineticText's words at this moment: the document's own text, or the
/// voiceover's line being said, the next one, and how far it is sung.
#[derive(Clone, Debug, PartialEq)]
pub struct KineticFrame {
    pub text: String,
    pub next: String,
    /// The sung fraction of `text`, 0..1 (0 for a document's own text).
    pub progress: f64,
    /// Whether the words come from a voiceover (the lyric channels).
    pub voice: bool,
    /// Seconds since `text` arrived: the layer's own time for its own
    /// text, the time into the line for a voiceover's (so a text change
    /// is a function of time and seeking lands on the same frame).
    pub since: f64,
}

/// A render-kit effect at one moment.
#[derive(Clone, Debug, PartialEq)]
pub struct EffectFrame {
    pub name: String,
    pub params: Vec<(String, EffectValue)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EffectValue {
    Num(f64),
    Vec2([f64; 2]),
    Vec3([f64; 3]),
    Color(Color),
    Bool(bool),
    Choice(String),
    Text(String),
    Points(Vec<[f64; 2]>),
}

/// A Scene3D's camera at this moment.
#[derive(Clone, Debug, PartialEq)]
pub struct CameraFrame {
    pub eye: [f64; 3],
    pub target: [f64; 3],
    pub fov: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LightFrame {
    pub point: bool,
    pub direction: [f64; 3],
    pub position: [f64; 3],
    pub color: Color,
    pub intensity: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NodeFrame {
    pub name: String,
    pub translate: [f64; 3],
    pub rotate: [f64; 3],
    pub scale: [f64; 3],
    pub visible: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MaterialFrame {
    pub name: String,
    pub base_color: Option<Color>,
    pub metallic: Option<f64>,
    pub roughness: Option<f64>,
    pub emissive: Option<Color>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum WidgetValue {
    Num(f64),
    Color(Color),
    Text(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImageMesh {
    /// Deformed positions in the item's local document units (y down).
    pub positions: Vec<[f64; 2]>,
    /// Undeformed box coordinates, 0..1, top-left origin. Image fitting
    /// maps these to texture UVs once the decoded image size is known.
    pub uvs: Arc<[[f64; 2]]>,
    /// Consecutive triples index both positions and UVs; shared across frames.
    pub indices: Arc<[u32]>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ItemKind {
    Rect { size: [f64; 2], radius: f64, fill: Option<Color>, stroke: Option<Color>, width: f64, trim: [f64; 2], cap: Cap },
    Ellipse { size: [f64; 2], fill: Option<Color>, stroke: Option<Color>, width: f64, trim: [f64; 2], cap: Cap },
    /// Absolute path geometry. `trim` is the drawn part of the stroke's length, 0..1.
    Path { path: Arc<makepad_svg::VectorPath>, fill: Option<Color>, stroke: Option<Color>, width: f64, trim: [f64; 2], cap: Cap, join: Join },
    Svg { src: Media, size: Option<[f64; 2]> },
    /// `units` cover the text in order; a unit's `offset`, `scale`,
    /// `rotate`, `opacity` and `color` are its own on top of the item's.
    Text {
        text: String,
        font: String,
        weight: f64,
        /// Variable-font axis values the font is read at, stepped
        /// (`eval::axis_step`), by tag.
        axes: Vec<(String, f64)>,
        italic: bool,
        size: f64,
        align: Align,
        valign: VAlign,
        line_height: f64,
        tracking: f64,
        wrap: Option<f64>,
        /// See `LayerKind::Text::fit`.
        fit: Option<f64>,
        /// Text on a path: its geometry and where along it (0..1) the text starts.
        path: Option<(Arc<makepad_svg::VectorPath>, f64)>,
        by: TextBy,
        units: Vec<TextUnit>,
        plate: Option<PlateFrame>,
        /// Characters shown (a typewriter); `None`: all.
        reveal: Option<usize>,
        /// The caret after the last shown character: opacity and colour.
        caret: Option<(f64, Color, bool)>,
        /// `stroke`: the letters' outlines, colour and width (document
        /// units).
        outline: Option<(Color, f64)>,
    },
    Image { src: Media, size: Option<[f64; 2]>, fit: Fit, zoom: f64, focus: Option<[f64; 2]>, mesh: Option<Arc<ImageMesh>> },
    Video { src: Media, size: Option<[f64; 2]>, fit: Fit, source_time: f64 },
    Group { children: Vec<Item>, mask: Option<Box<Item>>, mask_mode: MaskMode, size: Option<[f64; 2]> },
    /// `inner` drawn with the camera `zoom` times closer about letter
    /// `letter` of the Text item at `target` (a path inside `inner`).
    Fly { inner: Box<Item>, target: String, letter: usize, zoom: f64 },
    /// `inner` drawn from the projected point of `node` in the Scene3D item
    /// at `scene` (found when the scene is drawn).
    Anchored { inner: Box<Item>, scene: String, node: String },
    /// `audio`: the music's [energy, low, mid, high] at the host's time (the
    /// document's signals), what a vj_fx preset's audio bindings read.
    /// `sound`: which music that is and the host's time it was read at, so
    /// a host holding the song's analysed picture (makepad-audio-reactive's
    /// `AudioTrack`) hands a kit the same moment.
    /// `font`: a KineticText's document font in place of its kit's own.
    /// `data`: the layer's `data` list, shared with the document.
    Shader { program: ShaderProgram, size: [f64; 2], params: Vec<(String, f64)>, local_time: f64, beat: f64, below: bool, data: Option<ShaderData>, text: Option<KineticFrame>, font: Option<String>, audio: [f64; 4], sound: (String, f64) },
    Scene3D {
        model: Media,
        size: [f64; 2],
        camera: CameraFrame,
        lights: Vec<LightFrame>,
        nodes: Vec<NodeFrame>,
        materials: Vec<MaterialFrame>,
        background: Option<Color>,
        exposure: f64,
        local_time: f64,
    },
    Widget { ui: Option<makepad_render_graph::pass::PassFn>, size: [f64; 2], props: Vec<(String, WidgetValue)> },
    /// A motion3d scene at the layer's `local_time`; `edit_offset` is the
    /// host time of the layer's 0 (audio is read on the host's clock).
    /// `bpm` is the document's (motion3d's `beats(n)` counts at it);
    /// `textures`/`videos` name the host pictures its materials show.
    /// `written`: the scene is the document's own code, written from line
    /// `first_line`. A template scene's inputs are the document's, then the
    /// layer's own.
    Scene3dDoc { scene: crate::model::SceneSource, first_line: u32, written: bool, size: [f64; 2], local_time: f64, edit_offset: f64, fps: f64, bpm: f64, signals: SignalsRef, textures: Vec<(String, Media)>, videos: Vec<(String, Media)>, layers: Vec<(String, Vec<Item>)>, modules: crate::modules::Modules },
    /// A `Vector` at this moment: its source, box, the parts' offsets, the
    /// draw-on and the recolouring.
    Vector { src: Media, size: [f64; 2], parts: Vec<(String, PartFrame)>, draw_on: Option<f64>, stagger: f64, trace: f64, fill: Option<Color>, stroke: Option<Color> },
    /// A Draw2D layer's picture at this moment (what its draw function
    /// recorded).
    Draw2d { picture: crate::draw2d::Draw2dFrame, size: [f64; 2] },
}

/// One visible layer at one moment.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// The layer's path in the document (`layers[2].layers[0]`).
    pub path: String,
    /// Where the layer's origin is: every parent's transform and the
    /// layer's own `at` + `x`/`y`, in document units.
    pub world: Affine,
    /// The layer's scale and rotation about its anchor (a Text with
    /// per-unit animation keeps its unit transforms in its units instead).
    pub local: Local,
    pub opacity: f64,
    /// Gaussian blur in document units (0: none).
    pub blur: f64,
    /// Edge softness in document units; Gaussian sigma is half this value.
    pub feather: f64,
    pub feather_side: FeatherSide,
    /// Render-kit effects over the item's picture, their values at `t`.
    pub effects: Vec<EffectFrame>,
    pub blend: Blend,
    /// Whether the layer is the same over its whole span (drawn once and
    /// reused).
    pub is_static: bool,
    pub kind: ItemKind,
}

impl Item {
    /// The full map from the layer's own coordinates (its box from (0, 0))
    /// to document units, given the box's size.
    pub fn transform(&self, box_size: [f64; 2]) -> Affine {
        self.world.then(&self.local.affine(box_size))
    }

    /// The layer's box size when it is known without drawing.
    pub fn box_size(&self) -> Option<[f64; 2]> {
        match &self.kind {
            ItemKind::Rect { size, .. } | ItemKind::Ellipse { size, .. } => Some(*size),
            ItemKind::Svg { size, .. } | ItemKind::Image { size, .. } | ItemKind::Video { size, .. } => *size,
            ItemKind::Group { size, .. } => Some(size.unwrap_or([0.0, 0.0])),
            ItemKind::Shader { size, .. } | ItemKind::Scene3D { size, .. } | ItemKind::Widget { size, .. } | ItemKind::Draw2d { size, .. } | ItemKind::Scene3dDoc { size, .. } | ItemKind::Vector { size, .. } => Some(*size),
            ItemKind::Path { .. } | ItemKind::Text { .. } | ItemKind::Fly { .. } | ItemKind::Anchored { .. } => None,
        }
    }
}

/// A document at one moment: the items bottom first.
#[derive(Clone, Debug, PartialEq)]
pub struct MotionFrame {
    pub t: f64,
    pub size: [f64; 2],
    pub background: Option<Color>,
    pub items: Vec<Item>,
    /// The comp camera's parameters at `t` (2.5D effects).
    pub camera: Option<Vec<(String, EffectValue)>>,
    /// The document's frame rate (motion blur's shutter refers to it).
    pub fps: f64,
    /// A linear half-float frame (`color: @hdr`).
    pub hdr: bool,
    /// The post passes and their uniform values at `t`.
    pub post: Option<(std::sync::Arc<Vec<makepad_render_graph::PassDecl>>, Vec<makepad_render_graph::PassValues>)>,
    /// Groups' own post passes at `t` (`Group{post: [...]}`), by the group
    /// item's path.
    pub group_post: Vec<(String, (std::sync::Arc<Vec<makepad_render_graph::PassDecl>>, Vec<makepad_render_graph::PassValues>))>,
    /// Motion blur at `t`: shutter degrees, the most sub-frames, tolerance.
    pub motion_blur: Option<(f64, u32, f32)>,
    /// Data layers at `t`: each root layer with `data:`, by its name, drawn
    /// into its own texture for post passes (not into the frame).
    pub data_layers: Vec<(String, Vec<Item>)>,
    /// Evaluation diagnostics: failed property functions (which kept their
    /// default) and constraint warnings such as an unreachable IK target.
    pub errors: Vec<Diagnostic>,
}

impl MotionFrame {
    /// A stable hash of everything the frame draws: equal frames hash equal
    /// across runs and builds (a 64-bit FNV-1a of a canonical rendering).
    pub fn hash(&self) -> u64 {
        let text = format!("{:?}|{:?}|{:?}", self.size, self.background, self.items);
        // Text without an outline hashes as it did before texts had one.
        let text = text.replace(", outline: None }", " }");
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in text.bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }

    /// Every item, depth first, with its depth: the frame's, then its data
    /// layers' (drawn for post, they need their media as the frame does).
    pub fn walk(&self) -> Vec<(usize, &Item)> {
        fn go<'a>(items: &'a [Item], depth: usize, out: &mut Vec<(usize, &'a Item)>) {
            for item in items {
                out.push((depth, item));
                if let ItemKind::Group { children, mask, .. } = &item.kind {
                    go(children, depth + 1, out);
                    if let Some(mask) = mask {
                        go(std::slice::from_ref(mask), depth + 1, out);
                    }
                }
                if let ItemKind::Fly { inner, .. } | ItemKind::Anchored { inner, .. } = &item.kind {
                    go(std::slice::from_ref(inner.as_ref()), depth + 1, out);
                }
            }
        }
        let mut out = Vec::new();
        go(&self.items, 0, &mut out);
        for (_, items) in &self.data_layers {
            go(items, 0, &mut out);
        }
        out
    }

    /// The number of bottom items that are static: they draw the same
    /// until one of them leaves or the layers above change, so a renderer
    /// may keep their picture.
    pub fn static_prefix(&self) -> usize {
        self.items.iter().take_while(|item| item.is_static && !matches!(item.kind, ItemKind::Shader { below: true, .. })).count()
    }

    /// A hash of the static prefix's items: while it holds, their picture
    /// can be reused.
    pub fn static_key(&self) -> u64 {
        let n = self.static_prefix();
        let text = format!("{:?}|{:?}|{:?}", self.size, self.background, &self.items[..n]);
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in text.bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affines_compose_in_order() {
        let t = Affine::translate(10.0, 0.0).then(&Affine::scale(2.0, 2.0));
        assert_eq!(t.apply([1.0, 1.0]), [12.0, 2.0]);
        let r = Affine::rotate(90.0);
        let p = r.apply([1.0, 0.0]);
        assert!((p[0]).abs() < 1e-12 && (p[1] - 1.0).abs() < 1e-12, "clockwise with y down: {p:?}");
        let about = Affine::about([5.0, 5.0], &Affine::scale(2.0, 2.0));
        assert_eq!(about.apply([5.0, 5.0]), [5.0, 5.0]);
        assert_eq!(about.apply([6.0, 5.0]), [7.0, 5.0]);
        let local = Local { anchor: [0.5, 0.5], scale: [1.0, 1.0], rotate: 180.0 };
        let q = local.affine([10.0, 4.0]).apply([0.0, 0.0]);
        assert!((q[0] - 10.0).abs() < 1e-9 && (q[1] - 4.0).abs() < 1e-9, "{q:?}");
    }
}

/// The document's signals, carried in a frame for layers that sample audio
/// while they draw (motion3d scenes). Frames compare and hash it as one
/// value: the signals are the document's, the same for every frame.
#[derive(Clone)]
pub struct SignalsRef(pub std::sync::Arc<dyn crate::signals::SignalSource>);

impl std::fmt::Debug for SignalsRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("signals")
    }
}

impl PartialEq for SignalsRef {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

/// A `Vector` part's offsets at one moment.
#[derive(Clone, Debug, PartialEq)]
pub struct PartFrame {
    pub opacity: f64,
    pub offset: [f64; 2],
    pub scale: [f64; 2],
    pub rotate: f64,
    pub fill: Option<Color>,
    pub stroke: Option<Color>,
    pub width: Option<f64>,
    pub draw_on: Option<f64>,
}
