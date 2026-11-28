use crate::model::*;

#[derive(Clone, Debug, PartialEq)]
pub struct RectProps {
    pub size: Prop<2>,
    pub radius: Prop<1>,
    pub fill: Option<Prop<4>>,
    pub stroke: Option<Prop<4>>,
    pub width: Prop<1>,
    pub trim_start: Prop<1>,
    pub trim_end: Prop<1>,
    pub cap: Cap,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EllipseProps {
    pub size: Prop<2>,
    pub fill: Option<Prop<4>>,
    pub stroke: Option<Prop<4>>,
    pub width: Prop<1>,
    pub trim_start: Prop<1>,
    pub trim_end: Prop<1>,
    pub cap: Cap,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PathProps {
    pub data: PathShape,
    pub fill: Option<Prop<4>>,
    pub stroke: Option<Prop<4>>,
    pub width: Prop<1>,
    pub trim_start: Prop<1>,
    pub trim_end: Prop<1>,
    pub cap: Cap,
    pub join: Join,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextProps {
    pub font: String,
    pub weight: f64,
    pub italic: bool,
    pub size: Prop<1>,
    pub color: Prop<4>,
    pub align: Align,
    pub valign: VAlign,
    pub by: TextBy,
    pub stagger: Stagger,
    pub line_height: f64,
    pub tracking: Prop<1>,
    pub wrap: Option<f64>,
    pub fit: Option<f64>,
    pub reveal: Option<Prop<1>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GroupProps {
    pub mask_mode: MaskMode,
    pub stagger: Stagger,
    pub size: Option<Prop<2>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RepeatProps {
    pub count: usize,
    pub layout: RepeatLayout,
    pub delay: f64,
    pub step: RepeatStep,
    pub jitter: RepeatStep,
    pub seed: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LayerKind {
    Rect(RectProps),
    Ellipse(EllipseProps),
    Path(PathProps),
    Text { text: String, props: TextProps },
    Group { layers: Vec<Layer>, mask: Option<Box<Layer>>, props: GroupProps },
    Repeat { layer: Box<Layer>, props: RepeatProps },
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
}

impl Layer {
    pub fn is_animated(&self) -> bool {
        self.transform.is_animated() || self.opacity.is_animated() || self.blur.is_animated()
            || self.feather.is_animated() || match &self.kind {
                LayerKind::Rect(p) => p.is_animated(),
                LayerKind::Ellipse(p) => p.is_animated(),
                LayerKind::Path(p) => p.is_animated(),
                LayerKind::Text { props, .. } => props.is_animated(),
                LayerKind::Group { layers, mask, props } => props.size.as_ref().is_some_and(Prop::is_animated)
                    || layers.iter().any(|l| l.animated) || mask.as_ref().is_some_and(|l| l.animated),
                LayerKind::Repeat { layer, props } => layer.animated || (props.delay != 0.0 && layer.span.is_some()),
            }
    }
}

impl TextProps {
    pub fn is_animated(&self) -> bool {
        self.size.is_animated() || self.color.is_animated() || self.tracking.is_animated()
            || self.reveal.as_ref().is_some_and(Prop::is_animated)
    }
}

impl RectProps {
    pub fn is_animated(&self) -> bool {
        self.size.is_animated()
            || self.radius.is_animated()
            || self.width.is_animated()
            || self.trim_start.is_animated()
            || self.trim_end.is_animated()
            || self.fill.as_ref().is_some_and(Prop::is_animated)
            || self.stroke.as_ref().is_some_and(Prop::is_animated)
    }
}

impl EllipseProps {
    pub fn is_animated(&self) -> bool {
        self.size.is_animated()
            || self.width.is_animated()
            || self.trim_start.is_animated()
            || self.trim_end.is_animated()
            || self.fill.as_ref().is_some_and(Prop::is_animated)
            || self.stroke.as_ref().is_some_and(Prop::is_animated)
    }
}

impl PathProps {
    pub fn is_animated(&self) -> bool {
        matches!(self.data, PathShape::Morph { .. })
            || self.width.is_animated()
            || self.trim_start.is_animated()
            || self.trim_end.is_animated()
            || self.fill.as_ref().is_some_and(Prop::is_animated)
            || self.stroke.as_ref().is_some_and(Prop::is_animated)
    }
}
