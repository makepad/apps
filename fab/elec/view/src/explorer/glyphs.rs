//! Schematic glyph geometry in grid steps, with stroke widths in screen points.
//! Conversion is linear in the symbol's graphics and pins; callers can retain the result.

use fab_elec_core::{SchPt, P};
use fab_elec_parts::{PinDir, PinShape, SymFill, SymGraphic, SymbolUnit};
use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI, TAU};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Prim {
    /// A segment with width `w` in points; `butt` selects square rather than round ends.
    /// `pressed`: where its two ends go while the part is pressed (a button's actuator), else None.
    Line { a: [f32; 2], b: [f32; 2], w: f32, butt: bool, pressed: Option<[[f32; 2]; 2]> },
    /// A rounded box in world units. A zero stroke width omits the stroke.
    Box { c: [f32; 2], half: [f32; 2], corner: f32, w: f32, fill: Fill },
    /// An arc, CCW from +x with a signed sweep. Only full circles support fill.
    /// A sweep of magnitude at least two pi is full; a zero width omits the stroke.
    Arc { c: [f32; 2], r: f32, start: f32, sweep: f32, w: f32, fill: Fill },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fill {
    None,
    Body,
    Ink,
}

/// Symbol text with its vertical centre at `at` and its size in world units.
#[derive(Clone, Debug, PartialEq)]
pub struct PinText {
    pub text: String,
    pub at: [f32; 2],
    pub align: Align,
    pub size: f32,
    pub kind: TextKind,
    /// The pin's inner end (at the body), its direction into the body and its length, in
    /// symbol space: `pose_pin_texts` places the texts from them once the pose is known.
    pub end: [f32; 2],
    pub dir: [f32; 2],
    pub len: f32,
}

/// Horizontal alignment about a text anchor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Align {
    Left,
    Right,
    Center,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TextKind {
    PinName,
    PinNumber,
}

/// Mirror x, then rotate `turn` quarter turns CCW, then translate by `at`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Xf {
    pub at: [f32; 2],
    pub turn: u8,
    pub mirror: bool,
}

impl Xf {
    pub fn point(&self, p: [f32; 2]) -> [f32; 2] {
        let x = if self.mirror { -p[0] } else { p[0] };
        let y = p[1];
        let p = match self.turn % 4 {
            0 => [x, y],
            1 => [-y, x],
            2 => [-x, -y],
            _ => [y, -x],
        };
        [p[0] + self.at[0], p[1] + self.at[1]]
    }

    pub fn angle(&self, a: f32) -> f32 {
        let a = if self.mirror { PI - a } else { a };
        a + (self.turn % 4) as f32 * FRAC_PI_2
    }

    pub fn prim(&self, p: &Prim) -> Prim {
        match *p {
            Prim::Line { a, b, w, butt, pressed } => Prim::Line {
                a: self.point(a),
                b: self.point(b),
                w,
                butt,
                pressed: pressed.map(|ends| ends.map(|p| self.point(p))),
            },
            Prim::Box { c, half, corner, w, fill } => Prim::Box {
                c: self.point(c),
                half: if self.turn % 2 == 0 { half } else { [half[1], half[0]] },
                corner,
                w,
                fill,
            },
            Prim::Arc { c, r, start, sweep, w, fill } => Prim::Arc {
                c: self.point(c),
                r,
                start: self.angle(start),
                sweep: if self.mirror { -sweep } else { sweep },
                w,
                fill,
            },
        }
    }
}

pub const SYMBOL_W: f32 = 1.5;
pub const BUS_W: f32 = 3.0;
pub const WIRE_W: f32 = 1.5;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Family {
    Resistor,
    Capacitor,
    Inductor,
    Diode,
    Led,
    Switch,
    Crystal,
    Flag,
}

/// A family symbol in its unrotated frame, including its leads. The flag's sole pin is its foot.
pub fn family_prims(f: Family) -> Vec<Prim> {
    let leads = |end: f32| vec![
        line([-2.0, 0.0], [-end, 0.0], SYMBOL_W, false),
        line([end, 0.0], [2.0, 0.0], SYMBOL_W, false),
    ];
    let mut prims = match f {
        Family::Resistor => leads(1.0),
        Family::Capacitor => leads(0.25),
        Family::Inductor => leads(1.2),
        Family::Diode | Family::Led => leads(0.5),
        Family::Switch => leads(0.8),
        Family::Crystal => leads(0.55),
        Family::Flag => Vec::new(),
    };
    match f {
        Family::Flag => {
            let points = [[0.0, 0.0], [0.0, 0.85], [-0.6, 1.2], [0.0, 1.55], [0.6, 1.2], [0.0, 0.85]];
            prims.extend(points.windows(2).map(|p| line(p[0], p[1], SYMBOL_W, false)));
        }
        Family::Resistor => prims.push(Prim::Box {
            c: [0.0, 0.0],
            half: [1.0, 0.4],
            corner: 0.05,
            w: SYMBOL_W,
            fill: Fill::Body,
        }),
        Family::Capacitor => {
            for x in [-0.25, 0.25] {
                prims.push(line([x, -0.8], [x, 0.8], 2.0, true));
            }
        }
        Family::Inductor => {
            for x in [-0.9, -0.3, 0.3, 0.9] {
                prims.push(Prim::Arc {
                    c: [x, 0.0],
                    r: 0.3,
                    start: 0.0,
                    sweep: PI,
                    w: SYMBOL_W,
                    fill: Fill::None,
                });
            }
        }
        Family::Diode | Family::Led => {
            prims.extend([
                line([-0.5, -0.55], [-0.5, 0.55], SYMBOL_W, false),
                line([-0.5, 0.55], [0.5, 0.0], SYMBOL_W, false),
                line([0.5, 0.0], [-0.5, -0.55], SYMBOL_W, false),
                line([0.5, -0.55], [0.5, 0.55], 2.0, true),
            ]);
            if f == Family::Led {
                // Inside the body box (y <= 1), so labels placed clear of the body clear the light too.
                for (a, b) in [([-0.05, 0.62], [0.3, 0.97]), ([0.35, 0.62], [0.7, 0.97])] {
                    prims.push(line(a, b, 1.0, false));
                    for angle in [FRAC_PI_4 - 35.0_f32.to_radians(), FRAC_PI_4 + 35.0_f32.to_radians()] {
                        let back = [b[0] - 0.18 * angle.cos(), b[1] - 0.18 * angle.sin()];
                        prims.push(line(b, back, 1.0, false));
                    }
                }
            }
        }
        Family::Switch => {
            // Pressed, the lever lies on the far contact and the plunger rides down with it.
            let down = SWITCH_DROP;
            prims.extend([
                circle([-0.8, 0.0], 0.15, SYMBOL_W, Fill::Ink),
                circle([0.8, 0.0], 0.15, SYMBOL_W, Fill::Ink),
                pressed(line([-0.8, 0.0], [0.7, 0.55], SYMBOL_W, false), [[-0.8, 0.0], [0.8, 0.0]]),
                pressed(line([0.0, 0.35], [0.0, 0.95], 1.0, false), [[0.0, 0.35 - down], [0.0, 0.95 - down]]),
                pressed(line([-0.4, 0.95], [0.4, 0.95], 1.0, false), [[-0.4, 0.95 - down], [0.4, 0.95 - down]]),
            ]);
        }
        Family::Crystal => {
            for x in [-0.55, 0.55] {
                prims.push(line([x, -0.7], [x, 0.7], SYMBOL_W, false));
            }
            prims.push(Prim::Box {
                c: [0.0, 0.0],
                half: [0.3, 0.8],
                corner: 0.05,
                w: SYMBOL_W,
                fill: Fill::Body,
            });
        }
    }
    prims
}

/// Convert a symbol unit to its unrotated geometry and pin texts.
/// `pin_text` supplies (name, number) by the part's pin index, not the unit's pin order.
pub fn symbol_prims(unit: &SymbolUnit, pin_text: impl Fn(usize) -> (String, String)) -> (Vec<Prim>, Vec<PinText>) {
    let mut prims = Vec::new();
    let mut texts = Vec::new();
    for graphic in &unit.graphics {
        match graphic {
            SymGraphic::Poly { pts, closed, .. } => {
                for pair in pts.windows(2) {
                    prims.push(line(grid_point(pair[0]), grid_point(pair[1]), SYMBOL_W, false));
                }
                if *closed && !pts.is_empty() {
                    prims.push(line(grid_point(pts[pts.len() - 1]), grid_point(pts[0]), SYMBOL_W, false));
                }
            }
            SymGraphic::Rect { min, max, fill, .. } => {
                let min = grid_point(*min);
                let max = grid_point(*max);
                prims.push(Prim::Box {
                    c: [(min[0] + max[0]) * 0.5, (min[1] + max[1]) * 0.5],
                    half: [(max[0] - min[0]).abs() * 0.5, (max[1] - min[1]).abs() * 0.5],
                    corner: 0.15,
                    w: SYMBOL_W,
                    fill: symbol_fill(*fill),
                });
            }
            SymGraphic::Circle { c, r, fill, .. } => {
                prims.push(circle(grid_point(*c), *r as f32 / SchPt::GRID as f32, SYMBOL_W, symbol_fill(*fill)));
            }
            SymGraphic::Arc { a, mid, b, .. } => prims.push(graphic_arc(*a, *mid, *b)),
            SymGraphic::Text { .. } => {}
        }
    }

    for pin in &unit.pins {
        let at = [pin.at.x as f32, pin.at.y as f32];
        let dir = match pin.dir {
            PinDir::Right => [1.0, 0.0],
            PinDir::Left => [-1.0, 0.0],
            PinDir::Up => [0.0, 1.0],
            PinDir::Down => [0.0, -1.0],
        };
        let perp = [-dir[1], dir[0]];
        let end = along(at, dir, pin.len as f32 / SchPt::GRID as f32);
        let inverted = matches!(pin.shape, PinShape::Inverted | PinShape::InvertedClock | PinShape::OutputLow | PinShape::InputLow);
        let line_end = if inverted { along(end, dir, -0.5) } else { end };
        prims.push(line(at, line_end, SYMBOL_W, false));
        if inverted {
            prims.push(circle(along(end, dir, -0.25), 0.25, SYMBOL_W, Fill::None));
        }
        if matches!(pin.shape, PinShape::Clock | PinShape::ClockLow | PinShape::FallingEdgeClock | PinShape::InvertedClock) {
            let tip = along(end, dir, 0.5);
            prims.push(line(along(end, perp, 0.35), tip, SYMBOL_W, false));
            prims.push(line(along(end, perp, -0.35), tip, SYMBOL_W, false));
        }
        if pin.shape == PinShape::NonLogic {
            prims.extend(cross(at, 0.25, SYMBOL_W));
        }

        let (name, number) = pin_text(pin.pin.ix());
        if pin.name_shown && !name.is_empty() && name != "~" {
            let (distance, align) = match pin.dir {
                PinDir::Right => (0.4, Align::Left),
                PinDir::Left => (0.4, Align::Right),
                PinDir::Up | PinDir::Down => (1.0, Align::Center),
            };
            texts.push(PinText {
                text: name,
                at: along(end, dir, distance),
                align,
                size: NAME_SIZES[0],
                kind: TextKind::PinName,
                end,
                dir,
                len: pin.len as f32 / SchPt::GRID as f32,
            });
        }
        if pin.number_shown {
            let middle = [(at[0] + line_end[0]) * 0.5, (at[1] + line_end[1]) * 0.5];
            // Numbers stay above horizontal pins and left of vertical pins in both directions.
            let offset = if dir[0] != 0.0 { [0.0, 1.0] } else { [-1.0, 0.0] };
            texts.push(PinText {
                text: number,
                at: along(middle, offset, 0.45),
                align: Align::Center,
                size: 0.6,
                kind: TextKind::PinNumber,
                end,
                dir,
                len: pin.len as f32 / SchPt::GRID as f32,
            });
        }
    }
    (prims, texts)
}

/// Pin name sizes, largest first: a name shrinks only when no row holds it at full size.
const NAME_SIZES: [f32; 4] = [0.8, 0.7, 0.6, 0.5];
/// Pin number sizes, largest first.
const NUMBER_SIZES: [f32; 3] = [0.6, 0.5, 0.4];
/// A centred name's first row depth inside the body, and the step to each further row.
const NAME_DEPTH: f32 = 1.0;
const NAME_ROW: f32 = 0.95;
/// A side name's distance from its pin end.
const NAME_SIDE: f32 = 0.4;
/// A centred name slides along its side in these steps while it still covers its pin by
/// this much.
const NAME_SLIDE: f32 = 0.25;
const NAME_COVER: f32 = 0.2;
/// A number's distance from its stub: beside a vertical stub, or short of the body edge
/// above a horizontal one.
const NUMBER_SIDE: f32 = 0.25;
const NUMBER_RISE: f32 = 0.45;
/// Clearance between two pin texts.
const TEXT_GAP: f32 = 0.1;

/// An upright text's box: `at` is its vertical centre on its aligned edge.
pub fn pin_text_box(t: &PinText) -> [f32; 4] {
    text_box((t.at, t.align, t.size), &t.text)
}

/// A candidate place for a pin text: anchor, alignment and size.
type Place = ([f32; 2], Align, f32);

/// Widths come from the drawn font's advances (`fab_elec_explore::text`).
fn text_box((at, align, size): Place, text: &str) -> [f32; 4] {
    let w = text_width(text, size);
    let x0 = match align {
        Align::Left => at[0],
        Align::Right => at[0] - w,
        Align::Center => at[0] - w * 0.5,
    };
    [x0, at[1] - size * 0.5, x0 + w, at[1] + size * 0.5]
}

fn text_width(text: &str, size: f32) -> f32 {
    fab_elec_explore::text::width(text, size as f64) as f32
}

/// Whether two text boxes come closer than the text gap.
pub fn pin_texts_clash(a: [f32; 4], b: [f32; 4]) -> bool {
    a[0] < b[2] + TEXT_GAP && b[0] < a[2] + TEXT_GAP && a[1] < b[3] + TEXT_GAP && b[1] < a[3] + TEXT_GAP
}

/// The symbol's body: the union of its rectangles, in grid steps.
pub fn symbol_body(unit: &SymbolUnit) -> Option<[f32; 4]> {
    unit.graphics.iter()
        .filter_map(|g| match g {
            SymGraphic::Rect { min, max, .. } => {
                let (a, b) = (grid_point(*min), grid_point(*max));
                Some([a[0].min(b[0]), a[1].min(b[1]), a[0].max(b[0]), a[1].max(b[1])])
            }
            _ => None,
        })
        .reduce(|a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])])
}

/// Pin texts posed upright in world space (input order, names all kept) so that no two run
/// together. Each text takes the first of its candidate places that clears every text placed
/// before it, largest size first:
/// - side names first: a name whose pin is horizontal in the world aligns away from its pin;
/// - then centred names (vertical pins): in the first row of their side (rows step deeper but
///   stay in the side's half of the body) where they are clear, sliding along the side while
///   they still cover their pin; failing that, sliding further inside the body;
/// - then numbers, outside the body: beside a vertical stub (its middle, nearer the body or
///   nearer the pin end), or above a horizontal stub short of the body edge. A number with no
///   clear place is left out: numbers yield to names.
///
/// A name with no clear place even at its smallest size keeps that size in its first place.
pub fn pose_pin_texts(texts: &[PinText], body: Option<[f32; 4]>, xf: &Xf) -> Vec<PinText> {
    let origin = xf.point([0.0, 0.0]);
    let turn = |d: [f32; 2]| {
        let p = xf.point(d);
        [(p[0] - origin[0]).round(), (p[1] - origin[1]).round()]
    };
    let body = body.map(|b| {
        let (p, q) = (xf.point([b[0], b[1]]), xf.point([b[2], b[3]]));
        [p[0].min(q[0]), p[1].min(q[1]), p[0].max(q[0]), p[1].max(q[1])]
    });
    // (class: side name, centred name, number; world direction into the body; world pin end)
    let mut order: Vec<(u8, [f32; 2], [f32; 2], usize)> = texts.iter().enumerate()
        .map(|(i, t)| {
            let dir = turn(t.dir);
            let class = match t.kind {
                TextKind::PinName if dir[0] != 0.0 => 0,
                TextKind::PinName => 1,
                TextKind::PinNumber => 2,
            };
            (class, dir, xf.point(t.end), i)
        })
        .collect();
    order.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(a.1[1].total_cmp(&b.1[1]))
            .then(a.2[1].total_cmp(&b.2[1]))
            .then(a.2[0].total_cmp(&b.2[0]))
            .then(a.3.cmp(&b.3))
    });
    let mut out: Vec<Option<PinText>> = vec![None; texts.len()];
    let mut boxes: Vec<[f32; 4]> = Vec::with_capacity(texts.len());
    let mut places: Vec<Place> = Vec::new();
    for &(class, dir, end, i) in &order {
        let t = &texts[i];
        places.clear();
        match class {
            0 => {
                let align = if dir[0] > 0.0 { Align::Left } else { Align::Right };
                for &size in &NAME_SIZES {
                    places.push((along(end, dir, NAME_SIDE), align, size));
                }
            }
            1 => {
                // The side's half of the body; without a body, a row per name of the side.
                let deepest = match body {
                    Some(b) => (b[3] - b[1]) * 0.5,
                    None => {
                        let side = order.iter().filter(|o| o.0 == 1 && o.1 == dir && o.2[1] == end[1]).count();
                        NAME_DEPTH + side as f32 * NAME_ROW
                    }
                };
                // Inside the body (when there is one) by the text gap.
                let inside = |x: f32, half: f32| body.is_none_or(|b| x - half >= b[0] + TEXT_GAP && x + half <= b[2] - TEXT_GAP);
                for &size in &NAME_SIZES {
                    let half = text_width(&t.text, size) * 0.5;
                    let mut rows = Vec::new();
                    for row in 0.. {
                        let depth = NAME_DEPTH + row as f32 * NAME_ROW;
                        if row > 0 && depth + size * 0.5 > deepest {
                            break;
                        }
                        rows.push(along(end, dir, depth));
                    }
                    let covering = ((half - NAME_COVER) / NAME_SLIDE).max(0.0) as i32;
                    let reach = body.map_or(covering, |b| ((b[2] - b[0]) / NAME_SLIDE) as i32);
                    for ks in [0..=2 * covering, 2 * covering + 1..=2 * reach] {
                        for &at in &rows {
                            for k in ks.clone() {
                                let x = at[0] + slide(k);
                                if inside(x, half) {
                                    places.push(([x, at[1]], Align::Center, size));
                                }
                            }
                        }
                    }
                }
            }
            _ => {
                for &size in &NUMBER_SIZES {
                    if dir[0] != 0.0 {
                        let align = if dir[0] > 0.0 { Align::Right } else { Align::Left };
                        let at = [end[0] - dir[0] * NUMBER_SIDE, end[1] + NUMBER_RISE];
                        places.push((at, align, size));
                    } else {
                        for along_stub in [0.5, 0.25, 0.75] {
                            let at = along(end, dir, -t.len * along_stub);
                            places.push(([at[0] - NUMBER_SIDE, at[1]], Align::Right, size));
                        }
                    }
                }
            }
        }
        let clear = places.iter().copied().find(|&p| {
            let b = text_box(p, &t.text);
            !boxes.iter().any(|&o| pin_texts_clash(b, o))
        });
        let (at, align, size) = match (clear, class) {
            (Some(p), _) => p,
            (None, 2) => continue,
            (None, 0) => places[places.len() - 1],
            (None, _) => (along(end, dir, NAME_DEPTH), Align::Center, NAME_SIZES[NAME_SIZES.len() - 1]),
        };
        boxes.push(text_box((at, align, size), &t.text));
        out[i] = Some(PinText { at, align, size, ..t.clone() });
    }
    out.into_iter().flatten().collect()
}

/// The k-th shift of a slide: 0, then one step right, one left, two right, two left...
fn slide(k: i32) -> f32 {
    (if k % 2 == 1 { (k + 1) / 2 } else { -(k / 2) }) as f32 * NAME_SLIDE
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TagKind {
    Power,
    Ground,
    Label { text_w: f32 },
    NoConnect,
}

/// A tag pointing away from the pin end along one of the four axis unit directions.
/// The optional text anchor is in world space; its text remains upright.
/// A tag's primitives and its text anchor. `band`: the compact rail that fits a pin's row band, with
/// the stub's outward direction (a supply's name sits on that side of its bar).
/// `side`: an up or down supply name sits beyond its bar (0), to its left (-1) or right (1).
pub fn tag_prims(kind: TagKind, at: [f32; 2], dir: [i32; 2], band: Option<[i32; 2]>, side: i8) -> (Vec<Prim>, Option<([f32; 2], Align)>) {
    let turn = match dir {
        [1, 0] => 0,
        [0, 1] => 1,
        [-1, 0] => 2,
        [0, -1] => 3,
        _ => panic!("tag direction must be an axis unit vector"),
    };
    let xf = Xf { at, turn, mirror: false };
    if let Some(out) = band {
        let (prims, text) = match kind {
            TagKind::Power => {
                let align = if out[0] < 0 { Align::Right } else { Align::Left };
                let d = [dir[0] as f32, dir[1] as f32];
                let anchor = [at[0] + d[0] * 0.35 + out[0] as f32 * 0.7, at[1] + d[1] * 0.35 + out[1] as f32 * 0.7];
                (vec![line([0.0, 0.0], [0.3, 0.0], WIRE_W, false), line([0.3, -0.5], [0.3, 0.5], 2.0, true)], Some((anchor, align)))
            }
            TagKind::Ground => (
                vec![
                    line([0.0, 0.0], [0.25, 0.0], WIRE_W, false),
                    line([0.25, -0.5], [0.25, 0.5], 1.5, true),
                    line([0.5, -0.32], [0.5, 0.32], 1.5, true),
                    line([0.75, -0.14], [0.75, 0.14], 1.5, true),
                ],
                None,
            ),
            _ => return tag_prims(kind, at, dir, None, side),
        };
        return (prims.iter().map(|p| xf.prim(p)).collect(), text);
    }
    let (prims, text) = match kind {
        TagKind::Power => {
            let (distance, align) = match turn {
                0 => (1.35, Align::Left),
                2 => (1.35, Align::Right),
                _ => (1.75, Align::Center),
            };
            (
                vec![line([0.0, 0.0], [1.0, 0.0], WIRE_W, false), line([1.0, -0.6], [1.0, 0.6], 2.0, true)],
                Some(([distance, 0.0], align)),
            )
        }
        TagKind::Ground => (
            vec![
                line([0.0, 0.0], [1.0, 0.0], WIRE_W, false),
                line([1.0, -0.6], [1.0, 0.6], 1.5, true),
                line([1.3, -0.38], [1.3, 0.38], 1.5, true),
                line([1.6, -0.16], [1.6, 0.16], 1.5, true),
            ],
            None,
        ),
        TagKind::Label { text_w } => {
            // v9's arrow tag: a flag from the stub, pointed away from the pin (the net goes on
            // by name). The point stays inside the old box, so the sheet's extents are unchanged.
            let half = fab_elec_explore::sheet::LABEL_HALF as f32;
            let (start, tip) = (0.5, 0.5 + text_w + 0.8);
            let shoulder = tip - half;
            let mid = (start + shoulder) * 0.5;
            let outline = [[start, -half], [shoulder, -half], [tip, 0.0], [shoulder, half], [start, half], [start, -half]];
            // `[along, across]` to sheet units: along the pin's way, across it.
            let vertical = turn == 1 || turn == 3;
            let d = dir[1] as f32;
            let map = |[t, s]: [f32; 2]| if vertical { [at[0] + s, at[1] + d * t] } else { [t, s] };
            let fill = if vertical { [half, (shoulder - start) * 0.5] } else { [(shoulder - start) * 0.5, half] };
            let mut prims = vec![
                line(map([0.0, 0.0]), map([0.5, 0.0]), WIRE_W, false),
                Prim::Box { c: map([mid, 0.0]), half: fill, corner: 0.0, w: 0.0, fill: Fill::Body },
            ];
            prims.extend(outline.windows(2).map(|w| line(map(w[0]), map(w[1]), 1.0, false)));
            let text = Some((map([mid, 0.0]), Align::Center));
            // A vertical tag is placed in sheet units already, its text read upright; a horizontal
            // one goes through the tag's turn below.
            if vertical { return (prims, text); }
            (prims, text)
        }
        TagKind::NoConnect => (cross([0.0, 0.0], 0.3, 1.5).to_vec(), None),
    };
    let text = if kind == TagKind::Power && dir[0] == 0 && side != 0 {
        let anchor = [at[0] + f32::from(side) * 0.9, at[1] + dir[1] as f32];
        Some((anchor, if side > 0 { Align::Left } else { Align::Right }))
    } else {
        text.map(|(p, align)| (xf.point(p), align))
    };
    (prims.iter().map(|p| xf.prim(p)).collect(), text)
}

/// A filled connection disc, radius 0.3 grid steps, without a stroke.
pub fn junction(at: [f32; 2]) -> Prim {
    circle(at, 0.3, 0.0, Fill::Ink)
}

/// A 45-degree entry, using the CCW perpendicular of the axis unit direction `dir`.
pub fn bus_entry(at: [f32; 2], dir: [i32; 2]) -> Prim {
    let d = [dir[0] as f32, dir[1] as f32];
    line(at, [at[0] + 0.6 * (d[0] - d[1]), at[1] + 0.6 * (d[1] + d[0])], WIRE_W, false)
}

fn line(a: [f32; 2], b: [f32; 2], w: f32, butt: bool) -> Prim {
    Prim::Line { a, b, w, butt, pressed: None }
}

/// How far a switch's plunger rides down when pressed: the open lever's height under it.
pub const SWITCH_DROP: f32 = 0.55 * 0.8 / 1.5;

/// `line` with where its ends go while its part is pressed.
fn pressed(line: Prim, ends: [[f32; 2]; 2]) -> Prim {
    match line {
        Prim::Line { a, b, w, butt, .. } => Prim::Line { a, b, w, butt, pressed: Some(ends) },
        other => other,
    }
}

fn circle(c: [f32; 2], r: f32, w: f32, fill: Fill) -> Prim {
    Prim::Arc { c, r, start: 0.0, sweep: TAU, w, fill }
}

fn along(p: [f32; 2], dir: [f32; 2], distance: f32) -> [f32; 2] {
    [p[0] + dir[0] * distance, p[1] + dir[1] * distance]
}

fn cross(at: [f32; 2], half: f32, w: f32) -> [Prim; 2] {
    [
        line([at[0] - half, at[1] - half], [at[0] + half, at[1] + half], w, false),
        line([at[0] - half, at[1] + half], [at[0] + half, at[1] - half], w, false),
    ]
}

fn grid_point(p: P) -> [f32; 2] {
    [p.x as f32 / SchPt::GRID as f32, p.y as f32 / SchPt::GRID as f32]
}

fn symbol_fill(fill: SymFill) -> Fill {
    match fill {
        SymFill::None => Fill::None,
        SymFill::Body => Fill::Body,
        SymFill::Outline => Fill::Ink,
    }
}

fn graphic_arc(a: P, mid: P, b: P) -> Prim {
    // Work relative to a in f64, and detect collinearity exactly in the source nm space.
    let u = [mid.x as i64 - a.x as i64, mid.y as i64 - a.y as i64];
    let v = [b.x as i64 - a.x as i64, b.y as i64 - a.y as i64];
    let cross = u[0] as i128 * v[1] as i128 - u[1] as i128 * v[0] as i128;
    if cross == 0 {
        return line(grid_point(a), grid_point(b), SYMBOL_W, false);
    }
    let grid = SchPt::GRID as f64;
    let u = [u[0] as f64 / grid, u[1] as f64 / grid];
    let v = [v[0] as f64 / grid, v[1] as f64 / grid];
    let determinant = 2.0 * cross as f64 / (grid * grid);
    let u2 = u[0] * u[0] + u[1] * u[1];
    let v2 = v[0] * v[0] + v[1] * v[1];
    let c = [(u2 * v[1] - v2 * u[1]) / determinant, (u[0] * v2 - v[0] * u2) / determinant];
    let start = (-c[1]).atan2(-c[0]);
    let end = (v[1] - c[1]).atan2(v[0] - c[0]);
    let middle = (u[1] - c[1]).atan2(u[0] - c[0]);
    let tau = std::f64::consts::TAU;
    let ccw = (end - start).rem_euclid(tau);
    let sweep = if (middle - start).rem_euclid(tau) <= ccw { ccw } else { ccw - tau };
    Prim::Arc {
        c: [(a.x as f64 / grid + c[0]) as f32, (a.y as f64 / grid + c[1]) as f32],
        r: c[0].hypot(c[1]) as f32,
        start: start as f32,
        sweep: sweep as f32,
        w: SYMBOL_W,
        fill: Fill::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_parts::SymPin;

    #[test]
    fn family_glyphs_reach_their_pins_and_transforms_compose() {
        for family in [Family::Resistor, Family::Capacitor, Family::Inductor, Family::Diode, Family::Led, Family::Switch, Family::Crystal] {
            let prims = family_prims(family);
            for pin in [[-2.0, 0.0], [2.0, 0.0]] {
                assert!(
                    prims.iter().any(|p| matches!(p, Prim::Line { a, b, .. } if *a == pin || *b == pin)),
                    "{family:?} must have a lead ending exactly at pin {pin:?}"
                );
            }
        }
        let rotate = Xf { at: [0.0, 0.0], turn: 1, mirror: false };
        let mirror = Xf { at: [0.0, 0.0], turn: 0, mirror: true };
        assert_eq!(rotate.point([2.0, 0.0]), [0.0, 2.0], "quarter turn must rotate CCW");
        assert_eq!(mirror.point([2.0, 0.0]), [-2.0, 0.0], "mirror must reflect x");
        let combined = Xf { at: [3.0, -4.0], turn: 1, mirror: true };
        let rotated = rotate.point(mirror.point([2.0, 1.0]));
        assert_eq!(
            combined.point([2.0, 1.0]),
            [rotated[0] + 3.0, rotated[1] - 4.0],
            "placement must mirror, rotate, then translate"
        );
        let box_prim = Prim::Box { c: [1.0, 2.0], half: [2.0, 0.5], corner: 0.15, w: SYMBOL_W, fill: Fill::Body };
        assert_eq!(
            rotate.prim(&box_prim),
            Prim::Box { c: [-2.0, 1.0], half: [0.5, 2.0], corner: 0.15, w: SYMBOL_W, fill: Fill::Body },
            "quarter turn must swap box half sizes and rotate its centre"
        );
        let arc = Prim::Arc { c: [1.0, -0.5], r: 1.2, start: 0.3, sweep: 1.7, w: SYMBOL_W, fill: Fill::None };
        let Prim::Arc { c, r, start, sweep, .. } = arc else { unreachable!() };
        let angle = start + sweep * 0.5;
        let expected = mirror.point([c[0] + r * angle.cos(), c[1] + r * angle.sin()]);
        let Prim::Arc { c, r, start, sweep: mirrored_sweep, .. } = mirror.prim(&arc) else {
            panic!("mirroring an arc must preserve its primitive kind");
        };
        assert_eq!(mirrored_sweep, -sweep, "mirror must reverse the arc's sweep");
        let angle = start + mirrored_sweep * 0.5;
        let actual = [c[0] + r * angle.cos(), c[1] + r * angle.sin()];
        assert!(
            (actual[0] - expected[0]).abs() < 1e-6 && (actual[1] - expected[1]).abs() < 1e-6,
            "mirrored arc midpoint must belong to the mirrored point set: {actual:?} versus {expected:?}"
        );
        // A switch draws closed while pressed: its lever keeps its pivot and lies on the far contact,
        // and its plunger rides down by the open lever's height under it. Nothing else moves.
        for family in [Family::Resistor, Family::Capacitor, Family::Inductor, Family::Diode, Family::Led, Family::Switch, Family::Crystal] {
            let moving: Vec<([[f32; 2]; 2], [[f32; 2]; 2])> = family_prims(family).into_iter().filter_map(|p| match p {
                Prim::Line { a, b, pressed: Some(to), .. } => Some(([a, b], to)),
                _ => None,
            }).collect();
            if family != Family::Switch {
                assert!(moving.is_empty(), "{family:?} has nothing that moves when pressed");
                continue;
            }
            assert_eq!(moving.len(), 3, "the switch's lever and its plunger's stem and cap move when pressed");
            let (lever, closed) = moving[0];
            assert_eq!(closed, [lever[0], [0.8, 0.0]], "the pressed lever keeps its pivot and lies on the far contact");
            let under = lever[0][1] + (lever[1][1] - lever[0][1]) * (0.0 - lever[0][0]) / (lever[1][0] - lever[0][0]);
            assert!((under - SWITCH_DROP).abs() < 1e-6, "the plunger drops by the open lever's height under it ({under})");
            for (open, pressed) in &moving[1..] {
                assert_eq!(*pressed, open.map(|p| [p[0], p[1] - SWITCH_DROP]), "the plunger rides straight down");
            }
            let line = Prim::Line { a: lever[0], b: lever[1], w: SYMBOL_W, butt: false, pressed: Some(closed) };
            let Prim::Line { pressed: Some(posed), .. } = combined.prim(&line) else { panic!("posing keeps a line's pressed ends") };
            assert_eq!(posed, closed.map(|p| combined.point(p)), "posing places the pressed ends with the line");
        }
    }

    #[test]
    fn pin_texts_never_run_together_on_any_fixture_part_in_any_pose() {
        let mut seen = std::collections::HashSet::new();
        let designs = [fab_elec_fixtures::ledctl(), fab_elec_fixtures::ledctl_pair()];
        let mut checked = 0;
        let mut fitted_parts = 0;
        for comp in designs.iter().flat_map(|d| d.units.iter()).flat_map(|u| u.circuit.comps.iter()) {
            let Some(drawn) = comp.part.symbol.units.first() else { continue };
            if !seen.insert(comp.part.name.clone()) {
                continue;
            }
            fitted_parts += fab_elec_explore::adapt::sheet_unit(&comp.part, false).is_some() as usize;
            for turn in 0..4 {
                // The sheet draws a box symbol in the fit for its pose's turn parity.
                let fitted = fab_elec_explore::adapt::sheet_unit(&comp.part, turn & 1 == 1);
                let unit = fitted.as_ref().unwrap_or(drawn);
                if let Some(f) = &fitted {
                    // The fit keeps each pin's edge, length and order along its edge.
                    let along = |p: &SymPin| if matches!(p.dir, PinDir::Left | PinDir::Right) { -p.at.y } else { p.at.x };
                    for (i, (a, fa)) in drawn.pins.iter().zip(&f.pins).enumerate() {
                        assert_eq!((a.pin, a.dir, a.len), (fa.pin, fa.dir, fa.len), "{}: the fit keeps each pin's edge and length", comp.part.name);
                        for (b, fb) in drawn.pins[i + 1..].iter().zip(&f.pins[i + 1..]).filter(|(b, _)| b.dir == a.dir) {
                            assert_eq!(along(a).cmp(&along(b)), along(fa).cmp(&along(fb)), "{}: the fit keeps the pin order along an edge", comp.part.name);
                        }
                    }
                }
                let (_, texts) = symbol_prims(unit, |i| (comp.part.pins[i].name.clone(), comp.part.pins[i].pads.join(",")));
                for mirror in [false, true] {
                    let xf = Xf { at: [3.0, -2.0], turn, mirror };
                    let posed = pose_pin_texts(&texts, symbol_body(unit), &xf);
                    let names = |t: &[PinText]| t.iter().filter(|t| t.kind == TextKind::PinName).count();
                    assert_eq!(names(&posed), names(&texts), "{}: every pin name is posed", comp.part.name);
                    for (i, a) in posed.iter().enumerate() {
                        for b in &posed[i + 1..] {
                            assert!(!pin_texts_clash(pin_text_box(a), pin_text_box(b)),
                                "{} turn {turn} mirror {mirror}: pin texts {:?} and {:?} run together", comp.part.name, a.text, b.text);
                        }
                    }
                    if fitted.is_some() {
                        // A fitted body leaves every name its first place: full size, at its own pin.
                        for t in posed.iter().filter(|t| t.kind == TextKind::PinName) {
                            let end = xf.point(t.end);
                            let d = ((t.at[0] - end[0]).powi(2) + (t.at[1] - end[1]).powi(2)).sqrt();
                            assert!(t.size == NAME_SIZES[0] && [NAME_SIDE, NAME_DEPTH].iter().any(|&f| (d - f).abs() < 1e-4),
                                "{} turn {turn} mirror {mirror}: pin name {:?} sits full size at its pin in a fitted body (size {}, {d} from its pin end)",
                                comp.part.name, t.text, t.size);
                        }
                    }
                    checked += 1;
                }
            }
        }
        assert!(checked >= 8 * 3, "the fixture's symbol parts (regulator, MCU, connector, header...) were checked: {checked}");
        assert!(fitted_parts >= 3, "the regulator, MCU and connector are box symbols the sheet fits: {fitted_parts}");
    }

    #[test]
    fn tags_point_outward() {
        let at = [3.0, -2.0];
        let text_w = 1.5;
        for dir in [[1, 0], [-1, 0], [0, 1], [0, -1]] {
            let projection = |p: [f32; 2]| (p[0] - at[0]) * dir[0] as f32 + (p[1] - at[1]) * dir[1] as f32;
            for kind in [TagKind::Power, TagKind::Ground, TagKind::Label { text_w }] {
                let (prims, text) = tag_prims(kind, at, dir, None, 0);
                assert!(!prims.is_empty(), "{kind:?} tag in {dir:?} must have geometry");
                let reach = prims.iter().filter_map(|prim| match prim {
                    Prim::Line { a, b, .. } => Some(projection(*a).max(projection(*b))),
                    _ => None,
                }).fold(0.0f32, f32::max);
                if matches!(kind, TagKind::Label { .. }) {
                    assert!((reach - (1.3 + text_w)).abs() < 1e-5,
                        "a name flag's point is 1.3 + text_w out with its stub, where the sheet's extents end ({dir:?})");
                }
                // Segment endpoints and box corners bound every point of these tags.
                for prim in prims {
                    match prim {
                        Prim::Line { a, b, .. } => {
                            assert!(projection(a) >= -1e-6, "{kind:?} tag start {a:?} must point outward in {dir:?}");
                            assert!(projection(b) >= -1e-6, "{kind:?} tag end {b:?} must point outward in {dir:?}");
                        }
                        Prim::Box { c, half, .. } if matches!(kind, TagKind::Label { .. }) => {
                            for x in [-half[0], half[0]] {
                                for y in [-half[1], half[1]] {
                                    let p = [c[0] + x, c[1] + y];
                                    assert!(projection(p) >= -1e-6, "{kind:?} tag corner {p:?} must point outward in {dir:?}");
                                }
                            }
                            if dir[1] != 0 {
                                assert!(half[1] > half[0], "a vertical name flag lies along its pin");
                            }
                            let (across, half_w) = if dir[1] == 0 { (half[1], half[0]) } else { (half[0], half[1]) };
                            assert!((across - fab_elec_explore::sheet::LABEL_HALF as f32).abs() < 1e-6,
                                "a name flag is 2 * LABEL_HALF across its pin");
                            assert!(0.5 + 2.0 * half_w < 1.3 + text_w,
                                "a name flag's body ends before its point");
                        }
                        _ => panic!("{kind:?} tag must consist of stub and bar segments or a name box"),
                    }
                }
                if kind == TagKind::Power {
                    let (anchor, align) = text.expect("power tag must supply a text anchor");
                    assert!(projection(anchor) > 1.0, "power text must lie beyond its bar in {dir:?}");
                    let expected_align = match dir {
                        [1, 0] => Align::Left,
                        [-1, 0] => Align::Right,
                        _ => Align::Center,
                    };
                    assert_eq!(align, expected_align, "power text must align away from the pin in {dir:?}");
                    if dir[1] != 0 {
                        assert_eq!(anchor, [at[0], at[1] + dir[1] as f32 * 1.75], "centred power text keeps its anchor in {dir:?}");
                        for side in [-1, 1] {
                            let (_, text) = tag_prims(kind, at, dir, None, side);
                            let (anchor, align) = text.expect("power tag must supply a side text anchor");
                            assert_eq!(anchor, [at[0] + f32::from(side) * 0.9, at[1] + dir[1] as f32],
                                "side {side} power text sits beside the bar at its height in {dir:?}");
                            assert_eq!(align, if side > 0 { Align::Left } else { Align::Right },
                                "side {side} power text aligns away from the bar in {dir:?}");
                        }
                    }
                } else if matches!(kind, TagKind::Label { .. }) {
                    let (anchor, align) = text.expect("name flag must supply a text anchor");
                    assert!(projection(anchor) > 0.5, "name flag text must lie beyond its stub in {dir:?}");
                    assert_eq!(align, Align::Center, "name flag text must be centred in its box in {dir:?}");
                } else {
                    assert!(text.is_none(), "ground tag must not supply a text anchor");
                }
            }
        }
    }
}
