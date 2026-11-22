//! The canvas's visual language as data (ARCHITECTURE 6.6, 6.7): the state looks every pane draws
//! the same way, and the colour and weight tokens, dark and light. Shaders read the look as one
//! per-instance code and the tokens as uniforms or resolved instance colours; nothing else in a
//! view picks a colour.

/// One look per state, in every pane (ARCHITECTURE 6.6). Hover is not here: it changes at input
/// rate, so it is a uniform (`hover_item`, faded over 120 ms), never an instance rewrite.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum Look {
    /// Full ink.
    #[default]
    Committed = 0,
    /// The selection accent outline, two device pixels outside the stroke.
    Focus = 1,
    /// A drag in progress or a proposal: full ink with the selection accent outline, one pixel.
    Preview = 2,
    /// The committed state under a preview, or the previous run: a third of the opacity,
    /// desaturated, outline dashed.
    Ghost = 3,
    /// An engine has not caught up: the old result, its outline dotted in the stale colour.
    Stale = 4,
    /// A check fails here: the item's own ink outlined in the violation colour, at any zoom.
    Violation = 5,
    /// Excluded by a filter or disabled: a quarter of the opacity.
    Disabled = 6,
    /// An AI proposal: a preview in the AI accent.
    Proposal = 7,
}

impl Look {
    /// The code the shaders compare against (`look` instance attribute).
    pub fn code(self) -> f32 {
        self as u8 as f32
    }
}

/// An sRGB colour with straight alpha, as the shaders take it.
pub type Rgba = [f32; 4];

const fn hex(rgb: u32) -> Rgba {
    [
        ((rgb >> 16) & 0xff) as f32 / 255.0,
        ((rgb >> 8) & 0xff) as f32 / 255.0,
        (rgb & 0xff) as f32 / 255.0,
        1.0,
    ]
}

/// The colour roles of ARCHITECTURE 6.7 for one theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tokens {
    pub canvas: Rgba,
    pub grid: Rgba,
    pub grid_major: Rgba,
    pub ink: Rgba,
    pub ink_dim: Rgba,
    pub wire_signal: Rgba,
    pub wire_power: Rgba,
    pub wire_ground: Rgba,
    pub wire_bus: Rgba,
    pub block_fill: Rgba,
    pub block_frame: Rgba,
    pub part_fill: Rgba,
    pub hover: Rgba,
    pub selection: Rgba,
    pub ai: Rgba,
    pub violation: Rgba,
    pub warning: Rgba,
    pub ok: Rgba,
    pub stale: Rgba,
    /// The voltage ramp (perceptual, low, middle, high) and the magnitude ramp's high end.
    pub ramp_lo: Rgba,
    pub ramp_mid: Rgba,
    pub ramp_hi: Rgba,
    pub magnitude: Rgba,
}

pub const DARK: Tokens = Tokens {
    canvas: hex(0x14161a),
    grid: hex(0x1d2026),
    grid_major: hex(0x272b33),
    ink: hex(0xd8dde5),
    ink_dim: hex(0x8a92a0),
    wire_signal: hex(0xc4cad3),
    wire_power: hex(0xe3a25b),
    wire_ground: hex(0x7d8593),
    wire_bus: hex(0xeceef2),
    block_fill: hex(0x191c22),
    block_frame: hex(0x3a404b),
    part_fill: hex(0x1e2229),
    hover: hex(0x5cc8e6),
    selection: hex(0x4d8dff),
    ai: hex(0xb48cff),
    violation: hex(0xff5a5f),
    warning: hex(0xffb84d),
    ok: hex(0x4cd38a),
    stale: hex(0x9ea6ff),
    ramp_lo: hex(0x3b6fd8),
    ramp_mid: hex(0x2fbf9f),
    ramp_hi: hex(0xf2c94c),
    magnitude: hex(0xffd166),
};

pub const LIGHT: Tokens = Tokens {
    canvas: hex(0xf7f7f4),
    grid: hex(0xe8e8e3),
    grid_major: hex(0xd8d8d1),
    ink: hex(0x1f2328),
    ink_dim: hex(0x68707b),
    wire_signal: hex(0x30363e),
    wire_power: hex(0xb05f0c),
    wire_ground: hex(0x6b7380),
    wire_bus: hex(0x111418),
    block_fill: hex(0xf0f0ec),
    block_frame: hex(0xc4c6c0),
    part_fill: hex(0xfbfbf9),
    hover: hex(0x0b9ac2),
    selection: hex(0x1f6feb),
    ai: hex(0x7c4dff),
    violation: hex(0xd1242f),
    warning: hex(0xb87500),
    ok: hex(0x1a7f37),
    stale: hex(0x5b5fc7),
    ramp_lo: hex(0x2457c5),
    ramp_mid: hex(0x13917a),
    ramp_hi: hex(0xc99a06),
    magnitude: hex(0xd98e04),
};

impl Tokens {
    pub fn for_theme(dark: bool) -> &'static Tokens {
        if dark {
            &DARK
        } else {
            &LIGHT
        }
    }
}

/// Line weights in screen points (ARCHITECTURE 6.7), per rung band: far rungs, the symbol rung,
/// detail. A stroke is never under one device pixel (the shaders floor it).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Weight {
    pub far: f32,
    pub symbol: f32,
    pub detail: f32,
}

pub const WIRE: Weight = Weight { far: 1.0, symbol: 1.5, detail: 1.5 };
pub const BUS: Weight = Weight { far: 3.0, symbol: 3.0, detail: 3.0 };
pub const SYMBOL: Weight = Weight { far: 1.0, symbol: 1.5, detail: 2.0 };
pub const BLOCK_FRAME: Weight = Weight { far: 1.5, symbol: 1.0, detail: 1.0 };

/// Motion durations (ARCHITECTURE 6.7), seconds: state changes, moves, a reveal.
pub const STATE_SECONDS: f64 = 0.12;
pub const MOVE_SECONDS: f64 = 0.20;
pub const REVEAL_SECONDS: f64 = 0.30;

/// The one easing for moves: ease out (cubic), nothing bounces. The shaders use the same curve.
pub fn ease_out(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t)
}
