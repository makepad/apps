//! The instruments' visual language: light is data. Channel hue is identity, density is lightness
//! within that hue, and the chrome is the canvas's own colour with hairlines; nothing else glows.
//! The scope reads these tokens over the canvas's (`canvas::looks::Tokens`), so it sits beside the
//! schematic and the board as one more projection of the circuit.

use crate::canvas::looks::{Rgba, Tokens};

const fn hex(rgb: u32) -> Rgba {
    [((rgb >> 16) & 0xff) as f32 / 255.0, ((rgb >> 8) & 0xff) as f32 / 255.0, (rgb & 0xff) as f32 / 255.0, 1.0]
}

/// Eight channel hues at one OKLab lightness (0.78), chroma 0.13 (OKLCH hues 195, 51, 252, 12,
/// 318, 141, 96 and 345 degrees). The first four stay apart under normal vision and under protanopia
/// and deuteranopia; all eight stay apart under normal vision. A channel's name always rides with
/// its hue, so identity never rests on colour alone.
/// How many channel hues there are; past them the hues repeat.
pub const CHANNEL_HUES: usize = 8;

pub const CHANNELS_DARK: [Rgba; CHANNEL_HUES] = [
    hex(0x1ad1d1),
    hex(0xf99e67),
    hex(0x80bcff),
    hex(0xfe93a1),
    hex(0xda9ced),
    hex(0x87cc7c),
    hex(0xd0b74a),
    hex(0xf294ca),
];

/// The same hues at OKLab lightness 0.52 for the light canvas (chroma up to 0.13, less where sRGB
/// cannot hold it), dark enough to read as text on it.
pub const CHANNELS_LIGHT: [Rgba; 8] = [
    hex(0x067878),
    hex(0xa24e0d),
    hex(0x256bb1),
    hex(0xa64355),
    hex(0x874e99),
    hex(0x387b2e),
    hex(0x7b6806),
    hex(0x9b467a),
];

/// The OKLab lightness every channel hue sits at, dark and light.
pub const CHANNEL_LIGHTNESS: (f32, f32) = (0.78, 0.52);

/// The look of one theme: text levels, rules and channel hues over the canvas tokens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InstrumentLook {
    /// The canvas tokens underneath: canvas colour, state colours (ok, violation, stale, ai).
    pub canvas: &'static Tokens,
    /// The Scope ruler cell, menu, hover, lane field and focused lane grounds.
    pub panel: Rgba,
    pub popover: Rgba,
    pub hover_face: Rgba,
    /// A selected audio or instrument control.
    pub accent_face: Rgba,
    pub lane_control: Rgba,
    pub lane_focus: Rgba,
    /// Names and values.
    pub text: Rgba,
    /// Units and secondary words.
    pub text_secondary: Rgba,
    /// Axis labels and digits finer than a measurement's own spread.
    pub text_tertiary: Rgba,
    /// A grid or axis tick rule: the text colour at 6 %.
    pub hairline: Rgba,
    /// The zero line and a lane's separating rule: the text colour at 14 %.
    pub zero_line: Rgba,
    pub channels: &'static [Rgba; 8],
}

const fn alpha(c: Rgba, a: f32) -> Rgba {
    [c[0], c[1], c[2], a]
}

pub const DARK: InstrumentLook = InstrumentLook {
    canvas: &crate::canvas::looks::DARK,
    panel: hex(0x28282a),
    popover: hex(0x39393b),
    hover_face: hex(0x4c4c4f),
    accent_face: hex(0x0a84ff),
    lane_control: hex(0x2d3138),
    lane_focus: hex(0x27303f),
    text: crate::canvas::looks::DARK.ink,
    text_secondary: crate::canvas::looks::DARK.ink_dim,
    text_tertiary: hex(0x6c7380),
    hairline: alpha(crate::canvas::looks::DARK.ink, 0.06),
    zero_line: alpha(crate::canvas::looks::DARK.ink, 0.14),
    channels: &CHANNELS_DARK,
};

pub const LIGHT: InstrumentLook = InstrumentLook {
    canvas: &crate::canvas::looks::LIGHT,
    panel: hex(0xededee),
    popover: hex(0xfafafa),
    hover_face: hex(0xdedee1),
    accent_face: hex(0x007aff),
    lane_control: hex(0xe3e5e8),
    lane_focus: hex(0xdce6f3),
    text: crate::canvas::looks::LIGHT.ink,
    text_secondary: crate::canvas::looks::LIGHT.ink_dim,
    text_tertiary: hex(0x858c96),
    hairline: alpha(crate::canvas::looks::LIGHT.ink, 0.06),
    zero_line: alpha(crate::canvas::looks::LIGHT.ink, 0.14),
    channels: &CHANNELS_LIGHT,
};

impl InstrumentLook {
    /// The same look for text on a popover's panel, the theme's container fill: a mid grey in
    /// the dark theme, far lighter than the canvas. Secondary words take the ink and the finest
    /// level sits 60 % of the way from the dim ink to the ink, about 3:1 against that fill.
    pub fn on_panel(&self) -> InstrumentLook {
        let (dim, ink) = (self.text_secondary, self.text);
        let tertiary = [0, 1, 2, 3].map(|i| dim[i] + (ink[i] - dim[i]) * 0.6);
        InstrumentLook { text_secondary: ink, text_tertiary: tertiary, ..*self }
    }

    pub fn for_theme(dark: bool) -> &'static InstrumentLook {
        if dark {
            &DARK
        } else {
            &LIGHT
        }
    }

    /// The hue of channel `index`; past eight the hues repeat (names keep channels apart).
    pub fn channel(&self, index: usize) -> Rgba {
        self.channels[index % self.channels.len()]
    }
}
