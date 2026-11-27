use crate::Color;

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

/// The stable hash used by Motion's sampled frames.
pub fn picture_hash(size: [f64; 2], background: Option<Color>, items: &dyn std::fmt::Debug) -> u64 {
    let text = format!("{:?}|{:?}|{:?}", size, background, items);
    // Text without an outline hashes as it did before texts had one.
    let text = text.replace(", outline: None }", " }");
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
