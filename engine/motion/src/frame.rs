//! A Motion document at one moment: [`MotionFrame`], the resolved draw
//! list [`crate::MotionDoc::eval`] returns. Every value is final (keys
//! sampled, inputs filled, time references resolved, staggers applied);
//! only what needs a font or a decoded file (text layout, an image's own
//! size) is left to the drawing side.

use crate::plain::PrimitiveFrame;
use crate::model::*;
use std::sync::Arc;

pub use makepad_motion_plain::frame::{Affine, Local, PlateFrame, TextUnit};

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

pub use makepad_motion_plain::frame::{EffectFrame, EffectValue};

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
    Primitive(PrimitiveFrame),
    Svg { src: Media, size: Option<[f64; 2]> },
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
#[derive(Clone, PartialEq)]
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

// The frame hash uses the original variant names, including in nested groups.
impl std::fmt::Debug for Item {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind: &dyn std::fmt::Debug = match &self.kind {
            ItemKind::Primitive(shape) => shape,
            other => other,
        };
        f.debug_struct("Item")
            .field("path", &self.path)
            .field("world", &self.world)
            .field("local", &self.local)
            .field("opacity", &self.opacity)
            .field("blur", &self.blur)
            .field("feather", &self.feather)
            .field("feather_side", &self.feather_side)
            .field("effects", &self.effects)
            .field("blend", &self.blend)
            .field("is_static", &self.is_static)
            .field("kind", kind)
            .finish()
    }
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
            ItemKind::Primitive(PrimitiveFrame::Rect { size, .. }) | ItemKind::Primitive(PrimitiveFrame::Ellipse { size, .. }) => Some(*size),
            ItemKind::Svg { size, .. } | ItemKind::Image { size, .. } | ItemKind::Video { size, .. } => *size,
            ItemKind::Group { size, .. } => Some(size.unwrap_or([0.0, 0.0])),
            ItemKind::Shader { size, .. } | ItemKind::Scene3D { size, .. } | ItemKind::Widget { size, .. } | ItemKind::Draw2d { size, .. } | ItemKind::Scene3dDoc { size, .. } | ItemKind::Vector { size, .. } => Some(*size),
            ItemKind::Primitive(PrimitiveFrame::Path { .. }) | ItemKind::Primitive(PrimitiveFrame::Text { .. }) | ItemKind::Fly { .. } | ItemKind::Anchored { .. } => None,
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
        makepad_motion_plain::frame::picture_hash(self.size, self.background, &self.items)
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
        static_prefix_key(self.size, self.background, &self.items[..n])
    }
}

/// Cache identity of a document's static bottom layers.
pub fn static_prefix_key(size: [f64; 2], background: Option<Color>, items: &[impl std::fmt::Debug]) -> u64 {
    let text = format!("{:?}|{:?}|{:?}", size, background, items);
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
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
