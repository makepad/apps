//! Symbols: what a part looks like on a sheet. Pins sit on the 1.27 mm connection grid (`SchPt`,
//! E4 item 3); graphics are free (`P`, nm). Y up.

use fab_elec_core::{canon_enum, canon_struct, canon_tags, Angle, Nm, PinIx, SchPt, P};

/// A symbol: one or more units (the gates of a quad op-amp), each in one or more body styles.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Symbol {
    pub units: Vec<SymbolUnit>,
}
canon_struct!(Symbol { units });

#[derive(Clone, PartialEq, Debug, Default)]
pub struct SymbolUnit {
    pub name: String,
    /// 0 the normal body, 1 the alternate (De Morgan) body.
    pub body_style: u8,
    pub graphics: Vec<SymGraphic>,
    pub pins: Vec<SymPin>,
}
canon_struct!(SymbolUnit { name, body_style, graphics, pins });

/// A pin as drawn: its connection point is on the grid; the pin line runs `len` from there
/// towards the body in `dir`.
#[derive(Clone, PartialEq, Debug)]
pub struct SymPin {
    pub pin: PinIx,
    pub at: SchPt,
    pub dir: PinDir,
    pub len: Nm,
    pub shape: PinShape,
    pub name_shown: bool,
    pub number_shown: bool,
}
canon_struct!(SymPin { pin, at, dir, len, shape, name_shown, number_shown });

/// The direction from a pin's connection point towards the symbol body.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum PinDir {
    Right,
    Left,
    Up,
    Down,
}
canon_tags!(PinDir);

/// The graphic style of a pin's line end.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum PinShape {
    #[default]
    Line,
    Inverted,
    Clock,
    InvertedClock,
    InputLow,
    ClockLow,
    OutputLow,
    FallingEdgeClock,
    NonLogic,
}
canon_tags!(PinShape);

/// Fill of a closed symbol graphic.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum SymFill {
    #[default]
    None,
    /// Filled with the outline colour.
    Outline,
    /// Filled with the body background colour.
    Body,
}
canon_tags!(SymFill);

/// Symbol body graphics, in nm.
#[derive(Clone, PartialEq, Debug)]
pub enum SymGraphic {
    Poly { pts: Vec<P>, closed: bool, w: Nm, fill: SymFill },
    Arc { a: P, mid: P, b: P, w: Nm },
    Circle { c: P, r: Nm, w: Nm, fill: SymFill },
    Rect { min: P, max: P, w: Nm, fill: SymFill },
    Text { at: P, text: String, size: Nm, rot: Angle },
}
canon_enum!(SymGraphic {
    0 => Poly { pts, closed, w, fill }, 1 => Arc { a, mid, b, w }, 2 => Circle { c, r, w, fill },
    3 => Rect { min, max, w, fill }, 4 => Text { at, text, size, rot },
});
