//! How each board item looks: its surface, the shadow it casts, where its text sits and how
//! large it may grow, and an ink that reads on its fill. Pure rules; the canvas draws them.
use makepad_canvas::CanvasShape;
use makepad_town_core::entity::kind;
use makepad_town_core::event::Key;
use makepad_widgets::{dvec2, vec4, DVec2, Rect, Vec4f};

/// The board's colours, read from the canvas's script properties once a draw. They carry the
/// names of Town's skin tokens, so a skin sets them.
#[derive(Clone, Copy)]
pub(super) struct Look {
    pub sticky: Vec4f,
    pub shape: Vec4f,
    pub shape_line: Vec4f,
    /// A section's fill and border when it has no colour of its own.
    pub section: Vec4f,
    pub section_line: Vec4f,
    pub ink: Vec4f,
    /// The board ground that translucent fills and text are read against.
    pub ground: Vec4f,
    pub frame: Vec4f,
    pub frame_line: Vec4f,
    pub caption: Vec4f,
    pub shadow: Vec4f,
    /// Scales every shadow's alpha: a light skin casts lighter shadows.
    pub shadow_strength: f32,
    /// 1 for soft shadows, 0 for hard ones with a crisp edge.
    pub shadow_softness: f32,
}

/// What an item draws as.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Body {
    Note,
    Shape(CanvasShape),
    Text,
    Section,
    Frame,
    /// A kind this build has no look for: a plain card that names it.
    Card,
}

impl Body {
    pub fn of(kind: &Key, shape: &str) -> Self {
        match kind {
            Key::Number(kind::STICKY) => Self::Note,
            Key::Number(kind::SHAPE) => Self::Shape(match shape {
                "ellipse" => CanvasShape::Ellipse,
                "circle" => CanvasShape::Circle,
                "triangle" => CanvasShape::Triangle,
                "diamond" => CanvasShape::Diamond,
                _ => CanvasShape::Box,
            }),
            Key::Number(kind::TEXT_BLOCK) => Self::Text,
            Key::Number(kind::SECTION) => Self::Section,
            Key::Number(kind::DESIGN) => Self::Frame,
            _ => Self::Card,
        }
    }

    /// Whether its text sits above its top-left corner, out of its content.
    pub fn caption_above(self) -> bool {
        self == Self::Frame
    }
}

/// A card's surface: everything [`makepad_canvas::DrawCanvasCard`] takes besides the rect.
#[derive(Clone, Copy, Debug)]
pub(super) struct Surface {
    pub shape: CanvasShape,
    pub radius: f32,
    pub shade: f32,
    pub rim: f32,
    pub border: f32,
    pub border_color: Vec4f,
}

impl Surface {
    pub const FLAT: Surface =
        Surface { shape: CanvasShape::Box, radius: 0., shade: 0., rim: 0., border: 0., border_color: vec4(0., 0., 0., 0.) };
}

/// A shadow, in world units: a blurred ambient one moved down by `offset`, and a tight one at the edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Lift {
    pub blur: f32,
    pub offset: f32,
    pub alpha: f32,
    pub contact: f32,
    pub contact_blur: f32,
}

impl Lift {
    /// The farthest any shadow reaches past its item, lifted or not.
    pub const REACH: f64 = 18. * 2.2 + 10. * 2.4 + 2.;

    /// The same shadow raised by `k`: wider, further and a touch darker.
    pub fn raised(self, blur: f32, offset: f32) -> Self {
        Self { blur: self.blur * blur, offset: self.offset * offset, alpha: (self.alpha * 1.1).min(1.), ..self }
    }
}

/// The corner radius of a frame for a design preset.
fn frame_radius(preset: &str) -> f32 {
    match preset {
        "phone" => 36.,
        "tablet" => 22.,
        "custom" => 12.,
        _ => 10.,
    }
}

/// The surface `body` draws with in `colour`, for an item of `size`.
pub(super) fn surface(body: Body, colour: Vec4f, size: DVec2, preset: &str, look: &Look) -> Surface {
    let short = size.x.min(size.y) as f32;
    match body {
        Body::Note => Surface { radius: 6f32.min(short * 0.12), shade: 0.045, rim: 0.22, ..Surface::FLAT },
        Body::Shape(shape) => {
            let radius: f32 = if shape == CanvasShape::Box { 10. } else { 4. };
            let ink = ink_on(colour, look.ground);
            Surface { shape, radius: radius.min(short * 0.2), shade: 0.075, rim: 0.18, border: 1.5,
                border_color: mix(opaque(colour), ink, 0.3) }
        }
        Body::Section => Surface { radius: 14f32.min(short * 0.1), border: 1.5,
            border_color: vec4(colour.x, colour.y, colour.z, 0.4), ..Surface::FLAT },
        Body::Frame => Surface { radius: frame_radius(preset).min(short * 0.2), border: 1., border_color: look.frame_line,
            ..Surface::FLAT },
        Body::Card => Surface { radius: 8f32.min(short * 0.15), shade: 0.04, rim: 0.15, ..Surface::FLAT },
        Body::Text => Surface::FLAT,
    }
}

/// The shadow `body` casts, if any.
pub(super) fn lift(body: Body) -> Option<Lift> {
    match body {
        Body::Note => Some(Lift { blur: 9., offset: 5., alpha: 0.2, contact: 0.3, contact_blur: 1.5 }),
        Body::Shape(_) | Body::Card => Some(Lift { blur: 6., offset: 3., alpha: 0.16, contact: 0.25, contact_blur: 1. }),
        Body::Frame => Some(Lift { blur: 18., offset: 10., alpha: 0.3, contact: 0.25, contact_blur: 1.5 }),
        Body::Text | Body::Section => None,
    }
}

/// Font sizes, in points, that text fits to. Neighbouring items of one size share a size.
pub(super) const STEPS: [f32; 19] =
    [9., 10.5, 12., 13.5, 15., 16.5, 18., 21., 24., 27., 30., 36., 42., 48., 54., 66., 78., 96., 120.];

/// Points to local pixels.
pub(super) const PX_PER_PT: f64 = 96. / 72.;

/// The largest step at most `pt`, or the smallest step.
pub(super) fn step_at_most(pt: f64) -> usize {
    STEPS.iter().take_while(|s| **s as f64 <= pt).count().max(1) - 1
}

/// Where an item's text goes and how large it may be.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Place {
    /// The box the text is laid out in and clipped to, in the item's own units.
    pub inner: Rect,
    /// The largest size, in points, the text may take.
    pub cap: f64,
    /// The text takes exactly the cap's step rather than fitting the box.
    pub fixed: bool,
    pub centre_x: bool,
    pub centre_y: bool,
    pub bold: bool,
}

/// A section title chip's inset from the section's corner, and its padding, in ems.
pub(super) const CHIP_INSET: f64 = 12.;
pub(super) const CHIP_PAD: (f64, f64) = (0.55, 0.3);

/// The size, in local pixels, of a frame's caption.
pub(super) fn caption_px(width: f64) -> f64 {
    (width * 0.035).clamp(12., 28.)
}

/// How far a frame's caption stands above it.
pub(super) fn caption_reach(width: f64) -> f64 {
    caption_px(width) * 1.5 + 6.
}

fn inset(r: Rect, x: f64, y: f64) -> Rect {
    Rect { pos: r.pos + dvec2(x, y), size: dvec2((r.size.x - 2. * x).max(0.), (r.size.y - 2. * y).max(0.)) }
}

fn centred(r: Rect, w: f64, h: f64) -> Rect {
    Rect { pos: r.pos + (r.size - dvec2(w, h)) * 0.5, size: dvec2(w, h) }
}

/// Where `body`'s text goes in its rect `r`.
pub(super) fn place(body: Body, r: Rect) -> Place {
    let (w, h) = (r.size.x, r.size.y);
    let short = w.min(h);
    let fit = |inner: Rect, cap_px: f64| Place { inner, cap: cap_px / PX_PER_PT, fixed: false, centre_x: true,
        centre_y: true, bold: false };
    match body {
        Body::Note => fit(inset(r, short * 0.12, short * 0.12), (h * 0.24).min(w * 0.2)),
        Body::Card | Body::Shape(CanvasShape::Box) => fit(inset(r, short * 0.1, short * 0.1), short * 0.2),
        Body::Shape(CanvasShape::Ellipse) => fit(centred(r, w * 0.707, h * 0.707), short * 0.2),
        Body::Shape(CanvasShape::Circle) => fit(centred(r, short * 0.707, short * 0.707), short * 0.2),
        Body::Shape(CanvasShape::Diamond) => fit(centred(r, w * 0.5, h * 0.5), short * 0.16),
        Body::Shape(CanvasShape::Triangle) => fit(Rect { pos: r.pos + dvec2(w * 0.27, h * 0.5),
            size: dvec2(w * 0.46, h * 0.4) }, short * 0.16),
        Body::Text => Place { centre_x: false, ..fit(r, h * 0.62) },
        Body::Section => {
            let px = (w * 0.045).min(h * 0.07).clamp(13., 40.);
            let town = (w - 2. * CHIP_INSET - 2. * CHIP_PAD.0 * px).max(0.);
            Place { inner: Rect { pos: r.pos + dvec2(CHIP_INSET + CHIP_PAD.0 * px, CHIP_INSET + CHIP_PAD.1 * px),
                size: dvec2(town, (h - 2. * CHIP_INSET).max(0.)) }, cap: px / PX_PER_PT, fixed: true,
                centre_x: false, centre_y: false, bold: true }
        }
        Body::Frame => {
            let px = caption_px(w);
            Place { inner: Rect { pos: r.pos - dvec2(0., caption_reach(w)), size: dvec2(w, px * 1.5) },
                cap: px / PX_PER_PT, fixed: true, centre_x: false, centre_y: false, bold: false }
        }
    }
}

fn linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// WCAG relative luminance of an opaque colour.
pub(super) fn luminance(c: Vec4f) -> f32 {
    0.2126 * linear(c.x) + 0.7152 * linear(c.y) + 0.0722 * linear(c.z)
}

/// WCAG contrast ratio of two opaque colours, 1 to 21.
pub(super) fn contrast(a: Vec4f, b: Vec4f) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

pub(super) fn mix(a: Vec4f, b: Vec4f, t: f32) -> Vec4f {
    a * (1. - t) + b * t
}

pub(super) fn opaque(c: Vec4f) -> Vec4f {
    vec4(c.x, c.y, c.z, 1.)
}

/// `fill` over `ground`, opaque.
fn over(fill: Vec4f, ground: Vec4f) -> Vec4f {
    opaque(mix(opaque(ground), opaque(fill), fill.w.clamp(0., 1.)))
}

const NEAR_BLACK: Vec4f = vec4(0.063, 0.071, 0.086, 1.);
const WHITE: Vec4f = vec4(1., 1., 1., 1.);

/// An ink for text on `fill` (as it shows over `ground`): the fill's own hue taken most of the
/// way to near-black or to white, whichever reads better, and further until it reads at 7:1 or
/// is pure. Pure ink on any colour reads at 4.58:1 at least.
pub(super) fn ink_on(fill: Vec4f, ground: Vec4f) -> Vec4f {
    let base = over(fill, ground);
    let pure = if contrast(base, NEAR_BLACK) >= contrast(base, WHITE) { NEAR_BLACK } else { WHITE };
    let mut t = 0.82;
    loop {
        let ink = mix(base, pure, t);
        if t >= 1. || contrast(base, ink) >= 7. {
            return ink;
        }
        t = (t + 0.06f32).min(1.);
    }
}

/// `colour` as text on `ground`: itself when it reads there at 4.5:1, else moved toward the
/// far end of the ground until it does. It keeps its alpha.
pub(super) fn readable_on(colour: Vec4f, ground: Vec4f) -> Vec4f {
    let c = opaque(colour);
    let ground = opaque(ground);
    let far = if contrast(ground, WHITE) >= contrast(ground, NEAR_BLACK) { WHITE } else { NEAR_BLACK };
    let mut t = 0.;
    loop {
        let m = mix(c, far, t);
        if t >= 1. || contrast(m, ground) >= 4.5 {
            return vec4(m.x, m.y, m.z, colour.w);
        }
        t = (t + 0.1f32).min(1.);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PALETTE: [(&str, u32); 16] = [
        ("butter", 0xF6DC86), ("peach", 0xF8BE94), ("rose", 0xF5AFC2), ("lilac", 0xCDBDF4),
        ("sky", 0xA9D2F3), ("mint", 0xABE3C6), ("lime", 0xD3E59A), ("paper", 0xEDE9E1),
        ("coral", 0xEE8577), ("berry", 0xE36C9B), ("violet", 0x9F84E6), ("ocean", 0x5AA2E0),
        ("teal", 0x3DB39B), ("amber", 0xE7A93B), ("slate", 0x4F5E70), ("ink", 0x2C3440),
    ];

    fn rgb(n: u32) -> Vec4f {
        Vec4f::from_u32((n << 8) | 255)
    }

    #[test]
    fn every_fill_gets_a_readable_ink_and_every_text_colour_reads_on_either_ground() {
        let dark = rgb(0x16181B);
        let light = rgb(0xF4F1EA);
        for ground in [dark, light] {
            for (name, n) in PALETTE {
                let fill = rgb(n);
                let ink = ink_on(fill, ground);
                assert!(contrast(fill, ink) >= 4.5, "{name}: its ink reads at {:.2}:1", contrast(fill, ink));
                let text = readable_on(fill, ground);
                assert!(contrast(text, ground) >= 4.5, "{name} as text reads at {:.2}:1 on the ground",
                    contrast(text, ground));
            }
            // A translucent fill is read as it shows: mostly the ground.
            let wash = vec4(0.95, 0.4, 0.6, 0.14);
            let ink = ink_on(wash, ground);
            assert!(contrast(over(wash, ground), ink) >= 4.5, "an ink on a 14 % wash reads against what shows");
        }
        // The seeded pink ellipse of the before grabs.
        let pink = rgb(0xE86A9E);
        assert!(contrast(pink, ink_on(pink, dark)) >= 4.5, "the pink ellipse's label reads");
        assert!(luminance(ink_on(pink, dark)) < 0.1, "a mid pink takes the dark ink, its better side");
        let lilac = rgb(0xCDBDF4);
        assert!(contrast(lilac, ink_on(lilac, dark)) >= 7., "a pastel's ink reaches body-text contrast");
        assert_eq!(readable_on(lilac, dark), lilac, "a colour that already reads is kept");
    }

    #[test]
    fn text_sits_inside_each_shape_and_steps_stay_in_range() {
        let r = Rect { pos: dvec2(10., 20.), size: dvec2(200., 130.) };
        for body in [Body::Note, Body::Card, Body::Shape(CanvasShape::Box), Body::Shape(CanvasShape::Ellipse),
            Body::Shape(CanvasShape::Circle), Body::Shape(CanvasShape::Diamond), Body::Shape(CanvasShape::Triangle),
            Body::Text, Body::Section]
        {
            let p = place(body, r);
            let (a, b) = (p.inner.pos, p.inner.pos + p.inner.size);
            assert!(a.x >= r.pos.x && a.y >= r.pos.y && b.x <= r.pos.x + r.size.x && b.y <= r.pos.y + r.size.y,
                "{body:?}: its text box {:?} lies inside its rect", p.inner);
            let centre = |x: f64, y: f64| dvec2((x - r.pos.x) / r.size.x * 2. - 1., (y - r.pos.y) / r.size.y * 2. - 1.);
            for corner in [centre(a.x, a.y), centre(b.x, a.y), centre(a.x, b.y), centre(b.x, b.y)] {
                let inside = match body {
                    Body::Shape(CanvasShape::Ellipse) => corner.x * corner.x + corner.y * corner.y <= 1.001,
                    Body::Shape(CanvasShape::Diamond) => corner.x.abs() + corner.y.abs() <= 1.001,
                    Body::Shape(CanvasShape::Triangle) => corner.x.abs() <= (corner.y + 1.) * 0.5 + 0.001,
                    _ => true,
                };
                assert!(inside, "{body:?}: the text box's corner {corner:?} lies within the shape");
            }
        }
        let frame = place(Body::Frame, r);
        assert!(frame.inner.pos.y + frame.inner.size.y <= r.pos.y, "a frame's caption stands above it");
        assert_eq!(STEPS[step_at_most(1.)], STEPS[0], "below the floor, the smallest step");
        assert_eq!(STEPS[step_at_most(28.)], 27., "a cap between steps takes the step below");
    }
}
