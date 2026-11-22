//! The desktop handle vocabulary and palette, also used by the headset view.
//! Colours are straight RGBA; renderers premultiply only at the final blend.

pub type Color = [f32; 4];
pub const COL_LINEAR: Color = [1.0, 0.62, 0.20, 1.0];
pub const COL_RADIUS: Color = [0.30, 0.72, 1.0, 1.0];
pub const COL_ANGLE: Color = [0.72, 0.52, 1.0, 1.0];
pub const COL_POINT: Color = [0.40, 0.90, 0.55, 1.0];
/// A cage's wireframe: its points' colour, faint, under their handles.
pub const COL_WIRE: Color = [0.40, 0.90, 0.55, 0.45];
pub const COL_COUNT: Color = [0.95, 0.95, 0.98, 1.0];
pub const COL_HOT: Color = [1.0; 4];
pub const COL_BOX: Color = [1.0, 0.80, 0.30, 0.25];
pub const COL_SNAP: Color = [1.0, 0.92, 0.30, 1.0];
pub const COUNT_BG: Color = [0.12, 0.13, 0.15, 0.92];
pub const COUNT_INK_HOT: Color = [0.05, 0.05, 0.05, 1.0];
pub const LABEL_BG: Color = [0.05, 0.06, 0.07, 0.88];
pub const OUTLINE_ALPHA: f32 = 0.6;
pub const OUTLINE_PX: f32 = 1.0;
pub const DISABLED_ALPHA: f32 = 0.35;
pub const OCCLUDED_ALPHA: f32 = 0.25;
pub const HOVER_SCALE: f32 = 1.2;

pub const AXIS_X: Color = [0.93, 0.27, 0.32, 1.0];
pub const AXIS_Y: Color = [0.45, 0.80, 0.22, 1.0];
pub const AXIS_Z: Color = [0.25, 0.50, 1.0, 1.0];
pub const RING: Color = [0.30, 0.62, 1.0, 1.0];
pub const SCALE: Color = [1.0, 0.78, 0.25, 1.0];
pub const MOVE_FREE: Color = [0.95, 0.95, 0.98, 0.9];
pub const GRAB_HALO: Color = [1.0, 0.62, 0.2, 0.45];
pub const GRAB_BALL: Color = [1.0, 0.82, 0.45, 1.0];
pub const PIN_HALO: Color = [0.3, 0.7, 1.0, 0.4];
pub const PIN_STEM: Color = [0.65, 0.88, 1.0, 1.0];
pub const PIN_BALL: Color = [0.3, 0.7, 1.0, 1.0];

/// The primitives of DrawHandlePrim (its existing shader discriminants).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum HandlePrim { Segment, Disc, RoundedSquare, Triangle }

/// Compositions: every shape has an interaction point, including each profile vertex.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandleShape { Length, Radius, Angle, Point, Profile, Count }

impl HandleShape {
    pub const fn color(self) -> Color {
        match self {
            Self::Length => COL_LINEAR,
            Self::Radius => COL_RADIUS,
            Self::Angle => COL_ANGLE,
            Self::Point | Self::Profile => COL_POINT,
            Self::Count => COL_COUNT,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandleState { Idle, Hover, Active, Disabled }

impl HandleState {
    pub fn color(self, base: Color) -> Color {
        match self {
            Self::Idle => base,
            // The desktop uses the same white ink for hover and capture. XR adds a
            // pulsing outline to distinguish capture without introducing another palette.
            Self::Hover | Self::Active => COL_HOT,
            Self::Disabled => [base[0], base[1], base[2], base[3] * DISABLED_ALPHA],
        }
    }

    pub const fn scale(self) -> f32 {
        match self { Self::Hover | Self::Active => HOVER_SCALE, _ => 1.0 }
    }
}
