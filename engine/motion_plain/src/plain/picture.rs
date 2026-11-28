use crate::frame::{EffectFrame, PlateFrame};
use crate::{Affine, Align, Blend, Cap, Color, Diagnostic, FeatherSide, Join, Local, MaskMode, TextBy, TextUnit, VAlign};
use std::{fmt, sync::Arc};

/// The shared, resolved shapes and text carried by a frame.
#[derive(Clone, Debug, PartialEq)]
pub enum PrimitiveFrame {
    Rect { size: [f64; 2], radius: f64, fill: Option<Color>, stroke: Option<Color>, width: f64, trim: [f64; 2], cap: Cap },
    Ellipse { size: [f64; 2], fill: Option<Color>, stroke: Option<Color>, width: f64, trim: [f64; 2], cap: Cap },
    /// Absolute path geometry. `trim` is the drawn part of the stroke's length, 0..1.
    Path { path: Arc<makepad_svg::VectorPath>, fill: Option<Color>, stroke: Option<Color>, width: f64, trim: [f64; 2], cap: Cap, join: Join },
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
}

#[derive(Clone, PartialEq)]
pub enum ItemKind {
    Primitive(PrimitiveFrame),
    Group { children: Vec<Item>, mask: Option<Box<Item>>, mask_mode: MaskMode, size: Option<[f64; 2]> },
}

impl fmt::Debug for ItemKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Primitive(shape) => fmt::Debug::fmt(shape, f),
            Self::Group { children, mask, mask_mode, size } => f.debug_struct("Group")
                .field("children", children).field("mask", mask).field("mask_mode", mask_mode).field("size", size).finish(),
        }
    }
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

#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub t: f64,
    pub size: [f64; 2],
    pub background: Option<Color>,
    pub items: Vec<Item>,
    pub fps: f64,
    pub errors: Vec<Diagnostic>,
}

impl Frame {
    pub fn hash(&self) -> u64 {
        crate::frame::picture_hash(self.size, self.background, &self.items)
    }
}

/// A frame produced by the data sampler.
pub struct PlainFrame(pub(super) Frame);

impl PlainFrame {
    pub fn frame(&self) -> &Frame { &self.0 }
    pub fn into_frame(self) -> Frame { self.0 }
}
