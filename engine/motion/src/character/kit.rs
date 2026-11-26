//! The starter kit: three stylised cartoon characters built from one
//! parameterised generator, each with every viseme and expression, so the
//! AI can cast a character without drawing one.
//!
//! - `spark`: a clay-orange sun-burst head, ivory hoodie, gold medallion.
//! - `mainframe`: an amber CRT head on a chunky boombox body, antenna.
//! - `pip`: a lanky human with a hair puff, bomber jacket and headphones.
//!
//! A [`Look`] changes any of them: palette, proportions, head shape, hair,
//! body and accessories. The same model and look give the same character
//! in every shot. Model units: about 1000 tall, feet on y = 0, facing
//! screen-right in a slight three-quarter view.

use super::{Geom, Part, Rig, Shape};
use crate::model::{Cap, Color, Join};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeadShape {
    Round,
    Square,
    Tall,
    Bean,
    /// A CRT: a case with a screen, the face drawn in light on it.
    Screen,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hair {
    None,
    /// A sun-burst of clay rays behind the head.
    Rays,
    Puff,
    Bob,
    Mohawk,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Body {
    Hoodie,
    Jacket,
    /// A boombox: a box with two speakers.
    Box,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Accessory {
    Medallion,
    Headphones,
    Cap,
    Beanie,
    Glasses,
    Antenna,
}

/// Everything that makes a kit character look the way it does.
#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    pub skin: Color,
    pub hair: Color,
    pub top: Color,
    pub bottom: Color,
    pub shoes: Color,
    pub accent: Color,
    pub ink: Color,
    pub eye: Color,
    pub pupil: Color,
    /// The inside of the mouth.
    pub mouth: Color,
    /// Lines drawn on the face (a CRT's glow; the ink otherwise).
    pub face: Color,
    pub head: HeadShape,
    pub hair_style: Hair,
    pub body: Body,
    pub accessories: Vec<Accessory>,
    pub head_size: f64,
    pub build: f64,
    pub legs: f64,
    pub arms: f64,
    /// How far the face turns toward screen-right (0: straight on).
    pub turn: f64,
    /// Ink line width, model units.
    pub outline: f64,
}

fn hex(v: u32) -> Color {
    [((v >> 16) & 255) as f64 / 255.0, ((v >> 8) & 255) as f64 / 255.0, (v & 255) as f64 / 255.0, 1.0]
}

fn shade(c: Color, f: f64) -> Color {
    [(c[0] * f).min(1.0), (c[1] * f).min(1.0), (c[2] * f).min(1.0), c[3]]
}

/// The kit's characters: name, what it is.
pub const KITS: &[(&str, &str)] = &[
    ("spark", "clay-orange sun-burst head, ivory hoodie, gold medallion; deadpan rapper"),
    ("mainframe", "amber CRT head on a boombox body, antenna; the face glows on the screen"),
    ("pip", "lanky human, hair puff, blue bomber jacket, headphones"),
];

/// A kit character's own look.
pub fn look(name: &str) -> Option<Look> {
    Some(match name {
        "spark" => Look {
            skin: hex(0xDE6F3A),
            hair: hex(0xC4562A),
            top: hex(0xEDE6D3),
            bottom: hex(0x2A2A30),
            shoes: hex(0xF4F4F0),
            accent: hex(0xE8B53C),
            ink: hex(0x17130F),
            eye: hex(0xFFFDF5),
            pupil: hex(0x17130F),
            mouth: hex(0x3A1512),
            face: hex(0x17130F),
            head: HeadShape::Round,
            hair_style: Hair::Rays,
            body: Body::Hoodie,
            accessories: vec![Accessory::Medallion],
            head_size: 1.0,
            build: 1.0,
            legs: 1.0,
            arms: 1.0,
            turn: 0.18,
            outline: 7.0,
        },
        "mainframe" => Look {
            skin: hex(0x1D2016),
            hair: hex(0x3B3D44),
            top: hex(0x5B6374),
            bottom: hex(0x2C2F36),
            shoes: hex(0x1E1F24),
            accent: hex(0xFFB23F),
            ink: hex(0x111216),
            eye: hex(0xFFB23F),
            pupil: hex(0x1D2016),
            mouth: hex(0x5A3A10),
            face: hex(0xFFB23F),
            head: HeadShape::Screen,
            hair_style: Hair::None,
            body: Body::Box,
            accessories: vec![Accessory::Antenna],
            head_size: 1.0,
            build: 1.3,
            legs: 0.8,
            arms: 0.95,
            turn: 0.12,
            outline: 7.0,
        },
        "pip" => Look {
            skin: hex(0x8D5A3C),
            hair: hex(0x1A1716),
            top: hex(0x3C78D8),
            bottom: hex(0xC9B28B),
            shoes: hex(0xD8402E),
            accent: hex(0xF2F2F2),
            ink: hex(0x141414),
            eye: hex(0xFFFFFF),
            pupil: hex(0x141414),
            mouth: hex(0x3B1414),
            face: hex(0x141414),
            head: HeadShape::Bean,
            hair_style: Hair::Puff,
            body: Body::Jacket,
            accessories: vec![Accessory::Headphones],
            head_size: 0.9,
            build: 0.85,
            legs: 1.12,
            arms: 1.08,
            turn: 0.2,
            outline: 7.0,
        },
        _ => return None,
    })
}

/// A look field's value as written.
#[derive(Clone, Debug, PartialEq)]
pub enum LookValue {
    Num(f64),
    Color(Color),
    Id(String),
    Ids(Vec<String>),
}

/// The fields a `look: {…}` takes, with what each is.
pub const LOOK_FIELDS: &[(&str, &str)] = &[
    ("skin", "colour of the head and hands (a CRT's screen)"),
    ("hair", "colour of the hair (a CRT's case)"),
    ("top", "hoodie / jacket / box colour"),
    ("bottom", "trousers"),
    ("shoes", "shoes"),
    ("accent", "medallion, headwear, headphones, antenna tip"),
    ("ink", "outlines"),
    ("eye", "eye whites (a CRT's glow)"),
    ("pupil", "pupils"),
    ("mouth", "inside of the mouth"),
    ("face", "lines on the face: brows, mouth outline"),
    ("head", "@round @square @tall @bean @screen"),
    ("hair_style", "@none @rays @puff @bob @mohawk"),
    ("body", "@hoodie @jacket @box"),
    ("accessories", "[@medallion @headphones @cap @beanie @glasses @antenna] (or @none)"),
    ("head_size", "head scale, 0.6..1.6"),
    ("build", "body width, 0.6..1.6"),
    ("legs", "leg length, 0.6..1.5"),
    ("arms", "arm length, 0.6..1.5"),
    ("turn", "three-quarter turn of the face toward screen-right, 0..0.4"),
    ("outline", "ink line width, model units (about 1000 tall)"),
];

impl Look {
    /// Set one field (`look: {skin: #c85 head: @square}`).
    pub fn set(&mut self, key: &str, value: &LookValue) -> Result<(), String> {
        let color = |v: &LookValue| match v {
            LookValue::Color(c) => Ok(*c),
            _ => Err(format!("`{key}` is a colour (#rrggbb)")),
        };
        let num = |v: &LookValue, lo: f64, hi: f64| match v {
            LookValue::Num(n) if *n >= lo && *n <= hi => Ok(*n),
            _ => Err(format!("`{key}` is a number from {lo} to {hi}")),
        };
        let id = |v: &LookValue| match v {
            LookValue::Id(s) => Ok(s.clone()),
            _ => Err(format!("`{key}` is an @id")),
        };
        match key {
            "skin" => self.skin = color(value)?,
            "hair" => self.hair = color(value)?,
            "top" => self.top = color(value)?,
            "bottom" => self.bottom = color(value)?,
            "shoes" => self.shoes = color(value)?,
            "accent" => self.accent = color(value)?,
            "ink" => self.ink = color(value)?,
            "eye" => self.eye = color(value)?,
            "pupil" => self.pupil = color(value)?,
            "mouth" => self.mouth = color(value)?,
            "face" => self.face = color(value)?,
            "head" => {
                self.head = match id(value)?.as_str() {
                    "round" => HeadShape::Round,
                    "square" => HeadShape::Square,
                    "tall" => HeadShape::Tall,
                    "bean" => HeadShape::Bean,
                    "screen" | "crt" => HeadShape::Screen,
                    other => return Err(format!("no head shape @{other}; @round @square @tall @bean @screen")),
                }
            }
            "hair_style" => {
                self.hair_style = match id(value)?.as_str() {
                    "none" => Hair::None,
                    "rays" => Hair::Rays,
                    "puff" => Hair::Puff,
                    "bob" => Hair::Bob,
                    "mohawk" => Hair::Mohawk,
                    other => return Err(format!("no hair style @{other}; @none @rays @puff @bob @mohawk")),
                }
            }
            "body" => {
                self.body = match id(value)?.as_str() {
                    "hoodie" => Body::Hoodie,
                    "jacket" => Body::Jacket,
                    "box" => Body::Box,
                    other => return Err(format!("no body @{other}; @hoodie @jacket @box")),
                }
            }
            "accessories" => {
                let ids = match value {
                    LookValue::Ids(ids) => ids.clone(),
                    LookValue::Id(id) => vec![id.clone()],
                    _ => return Err("`accessories` is a list of @ids".into()),
                };
                self.accessories.clear();
                for id in ids {
                    self.accessories.push(match id.as_str() {
                        "none" => continue,
                        "medallion" => Accessory::Medallion,
                        "headphones" => Accessory::Headphones,
                        "cap" => Accessory::Cap,
                        "beanie" => Accessory::Beanie,
                        "glasses" => Accessory::Glasses,
                        "antenna" => Accessory::Antenna,
                        other => return Err(format!("no accessory @{other}; @medallion @headphones @cap @beanie @glasses @antenna")),
                    });
                }
            }
            "head_size" => self.head_size = num(value, 0.6, 1.6)?,
            "build" => self.build = num(value, 0.6, 1.6)?,
            "legs" => self.legs = num(value, 0.6, 1.5)?,
            "arms" => self.arms = num(value, 0.6, 1.5)?,
            "turn" => self.turn = num(value, 0.0, 0.4)?,
            "outline" => self.outline = num(value, 0.0, 30.0)?,
            other => {
                let names: Vec<&str> = LOOK_FIELDS.iter().map(|(n, _)| *n).collect();
                return Err(format!("`{other}` is not a look field; fields: {}", names.join(", ")));
            }
        }
        Ok(())
    }
}

/// Geometry from commands and points, retaining the kit's hundredth-unit grid.
fn path(cmds: &[(char, &[f64])]) -> crate::geometry::VectorPath {
    let mut path = crate::geometry::VectorPath::new();
    let mut builder = makepad_svg::path_data::PathDataBuilder::default();
    let mut values = [0.0f32; 7];
    for (c, pts) in cmds {
        for (value, p) in values.iter_mut().zip(*pts) {
            *value = ((p * 100.0).round() / 100.0) as f32;
        }
        builder.command(*c as u8, &values[..pts.len()], &mut path);
    }
    path
}

struct Builder {
    parts: Vec<Part>,
    ink: Color,
    w: f64,
}

impl Builder {
    fn part(&mut self, name: &str, pivot: [f64; 2], z: f64, shapes: Vec<Shape>) {
        self.parts.push(Part { name: name.into(), parent: None, pivot, z, shapes, variant: None, joint: None, sways: false });
    }

    fn filled(&self, geom: Geom, fill: Color) -> Shape {
        Shape::fill(geom, fill, Some(self.ink), self.w)
    }

    fn flat(&self, geom: Geom, fill: Color) -> Shape {
        Shape::fill(geom, fill, None, 0.0)
    }

    fn pathf(&self, data: crate::geometry::VectorPath, fill: Color) -> Shape {
        self.filled(Geom::Path(data.into()), fill)
    }

    fn stroke(&self, data: crate::geometry::VectorPath, color: Color, width: f64) -> Shape {
        Shape::line(data, color, width)
    }
}

/// The colours a face is drawn in.
struct FaceInk {
    line: Color,
    dark: Color,
    teeth: Color,
    tongue: Color,
    lid: Color,
    white: Color,
    pupil: Color,
    screen: bool,
}

/// Build a kit character with a look.
pub fn build(name: &str, look: &Look) -> Rig {
    let l = look;
    let ink = l.ink;
    let w = l.outline;
    let mut b = Builder { parts: Vec::new(), ink, w };
    let (hs, bw, lg, am) = (l.head_size, l.build, l.legs, l.arms);

    // Legs, from the ground up.
    let fh = 44.0;
    let ankle = -38.0;
    let knee = ankle - 175.0 * lg;
    let hip = knee - 170.0 * lg;
    let lw = 64.0 * bw.sqrt();
    let sole = shade(l.shoes, 0.78);
    for (side, x, z) in [("l", -46.0 * bw, 10.0), ("r", 46.0 * bw, 14.0)] {
        let shade_back = if side == "l" { 0.86 } else { 1.0 };
        b.part(&format!("leg_{side}_upper"), [x, hip], z, vec![b.filled(Geom::Capsule { a: [x, hip + 8.0], b: [x, knee], width: lw }, shade(l.bottom, shade_back))]);
        b.part(&format!("leg_{side}_lower"), [x, knee], z + 1.0, vec![b.filled(Geom::Capsule { a: [x, knee], b: [x, ankle - 6.0], width: lw * 0.9 }, shade(l.bottom, shade_back))]);
        b.part(&format!("foot_{side}"), [x, ankle], z + 2.0, vec![
            b.filled(Geom::Rect { pos: [x - 42.0, -fh], size: [124.0, fh], radius: 21.0 }, shade(l.shoes, shade_back)),
            b.flat(Geom::Rect { pos: [x - 38.0, -13.0], size: [116.0, 9.0], radius: 4.0 }, sole),
        ]);
    }
    // Hips and torso.
    b.part("hips", [0.0, hip], 20.0, vec![b.filled(Geom::Rect { pos: [-80.0 * bw, hip - 46.0], size: [160.0 * bw, 88.0], radius: 34.0 }, l.bottom)]);
    let waist = hip - 18.0;
    let shoulder = waist - 238.0;
    let (wb, wt) = (86.0 * bw, 100.0 * bw);
    let mut torso = Vec::new();
    match l.body {
        Body::Hoodie | Body::Jacket => {
            torso.push(b.pathf(path(&[
                ('M', &[-wb, waist + 30.0]),
                ('L', &[wb, waist + 30.0]),
                ('C', &[wb + 8.0, waist - 60.0, wt + 4.0, shoulder + 70.0, wt - 8.0, shoulder + 22.0]),
                ('Q', &[wt - 30.0, shoulder - 8.0, 0.0, shoulder - 8.0]),
                ('Q', &[-wt + 30.0, shoulder - 8.0, -wt + 8.0, shoulder + 22.0]),
                ('C', &[-wt - 4.0, shoulder + 70.0, -wb - 8.0, waist - 60.0, -wb, waist + 30.0]),
                ('Z', &[]),
            ]), l.top));
            torso.push(b.filled(Geom::Rect { pos: [-wb - 3.0, waist + 4.0], size: [2.0 * wb + 6.0, 30.0], radius: 12.0 }, shade(l.top, 0.84)));
            if l.body == Body::Hoodie {
                torso.push(b.stroke(path(&[('M', &[-0.55 * wb, waist - 4.0]), ('L', &[-0.42 * wb, waist - 74.0]), ('L', &[0.42 * wb, waist - 74.0]), ('L', &[0.55 * wb, waist - 4.0])]), shade(l.top, 0.62), w * 0.7));
                torso.push(b.filled(Geom::Ellipse { center: [0.0, shoulder + 8.0], radius: [64.0 * bw, 26.0] }, shade(l.top, 0.86)));
            } else {
                torso.push(b.stroke(path(&[('M', &[8.0, shoulder + 10.0]), ('L', &[8.0, waist + 30.0])]), shade(l.top, 0.55), w * 0.7));
                torso.push(b.pathf(path(&[('M', &[-50.0 * bw, shoulder + 2.0]), ('L', &[8.0, shoulder + 64.0]), ('L', &[58.0 * bw, shoulder + 2.0]), ('Q', &[0.0, shoulder - 14.0, -50.0 * bw, shoulder + 2.0]), ('Z', &[])]), shade(l.top, 0.8)));
            }
        }
        Body::Box => {
            let (x0, y0, x1, y1) = (-wt - 6.0, shoulder - 6.0, wt + 6.0, waist + 30.0);
            torso.push(b.filled(Geom::Rect { pos: [x0, y0], size: [x1 - x0, y1 - y0], radius: 26.0 }, l.top));
            torso.push(b.filled(Geom::Rect { pos: [x0 + 22.0, y0 + 20.0], size: [x1 - x0 - 44.0, 34.0], radius: 10.0 }, shade(l.top, 0.7)));
            for cx in [-0.5 * wt, 0.5 * wt] {
                torso.push(b.filled(Geom::Ellipse { center: [cx, waist - 70.0], radius: [0.36 * wt, 0.36 * wt] }, shade(l.top, 0.55)));
                torso.push(b.filled(Geom::Ellipse { center: [cx, waist - 70.0], radius: [0.14 * wt, 0.14 * wt] }, shade(l.top, 0.35)));
            }
            torso.push(b.flat(Geom::Rect { pos: [-18.0, y0 + 30.0], size: [36.0, 14.0], radius: 6.0 }, l.accent));
        }
    }
    b.part("torso", [0.0, waist], 30.0, torso);
    if l.accessories.contains(&Accessory::Medallion) {
        let top = shoulder + 14.0;
        b.part("medallion", [0.0, top], 35.0, vec![
            b.stroke(path(&[('M', &[-0.4 * wt, top]), ('Q', &[0.0, top + 150.0, 0.4 * wt, top])]), l.accent, w * 1.1),
            b.filled(Geom::Ellipse { center: [0.0, top + 96.0], radius: [36.0, 36.0] }, l.accent),
            Shape { geom: Geom::Ellipse { center: [0.0, top + 96.0], radius: [22.0, 22.0] }, fill: None, stroke: Some(shade(l.accent, 0.62)), width: w * 0.7, cap: Cap::Round, join: Join::Round },
        ]);
    }

    // Arms: sleeves, then the hands.
    let ua = 150.0 * am;
    let la = 140.0 * am;
    let aw = 56.0 * bw.sqrt();
    let hands = if l.head == HeadShape::Screen { shade(l.hair, 1.2) } else { l.skin };
    let neck = shoulder + 4.0;
    for (side, sx, z) in [("l", -(wt - 26.0), 5.0), ("r", wt - 26.0, 40.0)] {
        let back = if side == "l" { 0.86 } else { 1.0 };
        let top = shoulder + 34.0;
        let elbow = top + ua;
        let wrist = elbow + la;
        b.part(&format!("arm_{side}_upper"), [sx, top], z, vec![b.filled(Geom::Capsule { a: [sx, top], b: [sx, elbow], width: aw }, shade(l.top, back))]);
        b.part(&format!("arm_{side}_lower"), [sx, elbow], z + 1.0, vec![
            b.filled(Geom::Capsule { a: [sx, elbow], b: [sx, wrist - 4.0], width: aw * 0.9 }, shade(l.top, back)),
            b.filled(Geom::Rect { pos: [sx - aw * 0.5, wrist - 26.0], size: [aw, 22.0], radius: 9.0 }, shade(l.top, 0.8 * back)),
        ]);
        let hand = [sx, wrist + 22.0];
        b.part(&format!("hand_{side}"), [sx, wrist], z + 2.0, vec![b.filled(Geom::Ellipse { center: hand, radius: [30.0, 33.0] }, shade(hands, back))]);
        if side == "r" {
            // The mic points from the hand to where the mouth is in the
            // on-mic pose; the prop stands up when the forearm is level.
            let d = [-0.93, 0.36];
            let at = |k: f64| [hand[0] + d[0] * k, hand[1] + d[1] * k];
            b.part("mic", [sx, wrist], z + 1.5, vec![
                b.filled(Geom::Capsule { a: at(-18.0), b: at(40.0), width: 24.0 }, hex(0x2A2A2E)),
                b.filled(Geom::Ellipse { center: at(62.0), radius: [28.0, 28.0] }, hex(0x9C9CA6)),
                b.stroke(path(&[('M', &{ let p = at(50.0); [p[0] - 12.0, p[1] - 20.0] }), ('L', &{ let p = at(74.0); [p[0] + 12.0, p[1] + 20.0] })]), shade(hex(0x9C9CA6), 0.6), w * 0.5),
            ]);
            b.part("prop", [sx, wrist], z + 2.5, vec![
                b.filled(Geom::Rect { pos: [hand[0] + 6.0, hand[1] - 70.0], size: [190.0, 140.0], radius: 12.0 }, hex(0xF4F1E6)),
                b.flat(Geom::Rect { pos: [hand[0] + 30.0, hand[1] - 40.0], size: [120.0, 16.0], radius: 6.0 }, l.accent),
                b.flat(Geom::Rect { pos: [hand[0] + 30.0, hand[1] - 6.0], size: [90.0, 12.0], radius: 5.0 }, shade(l.ink, 1.0)),
            ]);
        }
    }

    // Head.
    let hh = 310.0 * hs;
    let hw = 300.0 * hs;
    let hc = neck - hh * 0.5 + 26.0;
    let top = hc - hh * 0.5;
    let fx = l.turn * hw * 0.5;
    let mut head = Vec::new();
    let screen = l.head == HeadShape::Screen;
    if matches!(l.head, HeadShape::Round | HeadShape::Bean | HeadShape::Tall) {
        head.push(b.filled(Geom::Ellipse { center: [-0.47 * hw, hc + 0.05 * hh], radius: [0.08 * hw, 0.11 * hh] }, l.skin));
    }
    match l.head {
        HeadShape::Round => head.push(b.filled(Geom::Ellipse { center: [0.0, hc], radius: [hw * 0.5, hh * 0.5] }, l.skin)),
        HeadShape::Square => head.push(b.filled(Geom::Rect { pos: [-hw * 0.5, top], size: [hw, hh], radius: 44.0 }, l.skin)),
        HeadShape::Tall => head.push(b.filled(Geom::Rect { pos: [-hw * 0.45, hc - hh * 0.54], size: [hw * 0.9, hh * 1.08], radius: hw * 0.4 }, l.skin)),
        HeadShape::Bean => {
            let (t, bt) = (top, hc + hh * 0.5);
            let (rt, rb) = (0.44 * hw, 0.5 * hw);
            let k = 0.5523;
            head.push(b.pathf(path(&[
                ('M', &[0.0, t]),
                ('C', &[rt * k, t, rt, hc - hh * 0.5 * k, rt * 1.02, hc - 0.02 * hh]),
                ('C', &[rb, hc + hh * 0.5 * k, rb * k, bt, 0.0, bt]),
                ('C', &[-rb * k, bt, -rb, hc + hh * 0.5 * k, -rb * 0.98, hc]),
                ('C', &[-rt, hc - hh * 0.5 * k, -rt * k, t, 0.0, t]),
                ('Z', &[]),
            ]), l.skin));
        }
        HeadShape::Screen => {
            head.push(b.filled(Geom::Rect { pos: [-hw * 0.5 - 12.0, top], size: [hw + 24.0, hh], radius: 30.0 }, l.hair));
            head.push(b.filled(Geom::Rect { pos: [-hw * 0.5 + 16.0, top + 26.0], size: [hw - 32.0, hh - 64.0], radius: 36.0 }, l.skin));
            head.push(b.flat(Geom::Rect { pos: [hw * 0.5 - 70.0, hc + hh * 0.5 - 30.0], size: [44.0, 12.0], radius: 5.0 }, l.accent));
        }
    }
    if !screen {
        head.push(b.stroke(path(&[('M', &[fx + 0.07 * hw, hc + 0.03 * hh]), ('Q', &[fx + 0.14 * hw, hc + 0.11 * hh, fx + 0.05 * hw, hc + 0.12 * hh])]), l.face, w * 0.8));
    }
    b.part("head", [0.0, neck], 50.0, head);

    // Hair and headwear.
    let hair_top = [0.0, top + 20.0];
    match l.hair_style {
        Hair::None => {}
        Hair::Rays => {
            let mut s = crate::geometry::VectorPath::new();
            let rays = 13;
            for i in 0..rays {
                let a = (-205.0 + 230.0 * i as f64 / (rays - 1) as f64).to_radians();
                let half = 0.12;
                let (rx, ry) = (0.47 * hw, 0.47 * hh);
                let p = |a: f64, r: f64| [a.cos() * rx * r, hc + a.sin() * ry * r];
                let (p0, p1, tip) = (p(a - half, 0.9), p(a + half, 0.9), p(a, 1.36 + 0.06 * ((i % 2) as f64)));
                s.cmds.extend(path(&[('M', &p0), ('L', &tip), ('L', &p1), ('Z', &[])]).cmds);
            }
            b.part("hair", [0.0, hc], 45.0, vec![b.pathf(s, l.hair)]);
        }
        Hair::Puff => {
            let mut shapes = Vec::new();
            for i in 0..7 {
                let a = (-165.0 + 150.0 * i as f64 / 6.0).to_radians();
                let r = 0.2 * hw * if i % 2 == 0 { 1.0 } else { 0.9 };
                shapes.push(b.filled(Geom::Ellipse { center: [a.cos() * 0.4 * hw, hc - 0.05 * hh + a.sin() * 0.44 * hh], radius: [r, r] }, l.hair));
            }
            shapes.push(b.flat(Geom::Ellipse { center: [0.0, hc - 0.28 * hh], radius: [0.36 * hw, 0.2 * hh] }, l.hair));
            b.part("hair", hair_top, 45.0, shapes);
        }
        Hair::Bob => {
            b.part("hair", hair_top, 59.0, vec![b.pathf(path(&[
                ('M', &[-0.53 * hw, hc + 0.16 * hh]),
                ('C', &[-0.62 * hw, hc - 0.62 * hh, 0.62 * hw, hc - 0.62 * hh, 0.53 * hw, hc + 0.06 * hh]),
                ('L', &[0.4 * hw, hc - 0.14 * hh]),
                ('Q', &[0.0, hc - 0.3 * hh, -0.38 * hw, hc - 0.12 * hh]),
                ('Z', &[]),
            ]), l.hair)]);
        }
        Hair::Mohawk => {
            let mut s = crate::geometry::VectorPath::new();
            for i in 0..5 {
                let x = -0.2 * hw + 0.1 * hw * i as f64;
                s.cmds.extend(path(&[('M', &[x - 0.06 * hw, top + 14.0]), ('L', &[x + 0.02 * hw, top - 0.2 * hh - 10.0 * (i % 2) as f64]), ('L', &[x + 0.07 * hw, top + 14.0]), ('Z', &[])]).cmds);
            }
            b.part("hair", hair_top, 45.0, vec![b.pathf(s, l.hair)]);
        }
    }
    for acc in &l.accessories {
        match acc {
            Accessory::Medallion => {}
            Accessory::Headphones => b.part("headphones", [0.0, hc], 60.0, vec![
                b.stroke(path(&[('M', &[-0.5 * hw, hc - 0.02 * hh]), ('C', &[-0.5 * hw, hc - 0.72 * hh, 0.5 * hw, hc - 0.72 * hh, 0.5 * hw, hc - 0.02 * hh])]), ink, 26.0),
                b.stroke(path(&[('M', &[-0.5 * hw, hc - 0.02 * hh]), ('C', &[-0.5 * hw, hc - 0.72 * hh, 0.5 * hw, hc - 0.72 * hh, 0.5 * hw, hc - 0.02 * hh])]), shade(l.accent, 0.8), 12.0),
                b.filled(Geom::Rect { pos: [-0.6 * hw, hc - 0.1 * hh], size: [0.18 * hw, 0.28 * hh], radius: 18.0 }, l.accent),
                b.filled(Geom::Rect { pos: [0.44 * hw, hc - 0.08 * hh], size: [0.12 * hw, 0.26 * hh], radius: 16.0 }, shade(l.accent, 0.85)),
            ]),
            Accessory::Cap => b.part("cap", [0.0, top], 60.0, vec![
                b.pathf(path(&[('M', &[-0.5 * hw, hc - 0.16 * hh]), ('C', &[-0.52 * hw, hc - 0.66 * hh, 0.52 * hw, hc - 0.66 * hh, 0.5 * hw, hc - 0.16 * hh]), ('Z', &[])]), l.accent),
                b.pathf(path(&[('M', &[0.18 * hw, hc - 0.2 * hh]), ('Q', &[0.72 * hw, hc - 0.26 * hh, 0.8 * hw, hc - 0.13 * hh]), ('Q', &[0.5 * hw, hc - 0.1 * hh, 0.18 * hw, hc - 0.13 * hh]), ('Z', &[])]), shade(l.accent, 0.8)),
            ]),
            Accessory::Beanie => b.part("beanie", [0.0, top], 60.0, vec![
                b.pathf(path(&[('M', &[-0.5 * hw, hc - 0.2 * hh]), ('C', &[-0.5 * hw, hc - 0.74 * hh, 0.5 * hw, hc - 0.74 * hh, 0.5 * hw, hc - 0.2 * hh]), ('Z', &[])]), l.accent),
                b.filled(Geom::Rect { pos: [-0.53 * hw, hc - 0.28 * hh], size: [1.06 * hw, 0.12 * hh], radius: 16.0 }, shade(l.accent, 0.8)),
                b.filled(Geom::Ellipse { center: [0.0, hc - 0.62 * hh], radius: [0.08 * hw, 0.08 * hw] }, shade(l.accent, 0.9)),
            ]),
            Accessory::Antenna => b.part("antenna", [0.12 * hw, top], 44.0, vec![
                b.stroke(path(&[('M', &[0.12 * hw, top + 10.0]), ('L', &[0.18 * hw, top - 120.0])]), ink, 10.0),
                b.filled(Geom::Ellipse { center: [0.18 * hw, top - 128.0], radius: [20.0, 20.0] }, l.accent),
            ]),
            Accessory::Glasses => {
                let ye = hc - 0.03 * hh;
                let mut shapes = Vec::new();
                for ex in [fx - 0.19 * hw, fx + 0.17 * hw] {
                    shapes.push(Shape { geom: Geom::Rect { pos: [ex - 0.13 * hw, ye - 0.12 * hh], size: [0.26 * hw, 0.24 * hh], radius: 18.0 }, fill: Some([1.0, 1.0, 1.0, 0.12]), stroke: Some(ink), width: w * 1.1, cap: Cap::Round, join: Join::Round });
                }
                shapes.push(b.stroke(path(&[('M', &[fx - 0.06 * hw, ye - 0.02 * hh]), ('L', &[fx + 0.04 * hw, ye - 0.02 * hh])]), ink, w));
                b.part("glasses", [fx, ye], 58.0, shapes);
            }
        }
    }

    // Costumes: worn by cue (`costume: [[beat(8), @crown]]`).
    b.part("costume", [0.0, top], 61.0, Vec::new());
    for (name, _) in COSTUMES {
        let (z, shapes) = costume_shapes(&b, l, name, top, hc, hw, hh, fx);
        b.part(&format!("costume_{name}"), [0.0, top], z, shapes);
    }

    // The face slots.
    let face = FaceInk {
        line: l.face,
        dark: if screen { shade(l.face, 0.3) } else { l.mouth },
        teeth: if screen { l.face } else { hex(0xFBF8EE) },
        tongue: if screen { shade(l.face, 0.6) } else { hex(0xD8505A) },
        lid: l.skin,
        white: l.eye,
        pupil: l.pupil,
        screen,
    };
    let ye = hc - 0.03 * hh;
    let eyes = [(fx - 0.19 * hw, 0.92), (fx + 0.17 * hw, 1.0)];
    let (rx, ry) = (0.085 * hw, 0.105 * hh);
    b.part("eyes", [fx, ye], 55.0, Vec::new());
    for variant in ["open", "closed", "deadpan", "surprised", "won", "angry", "sly"] {
        let shapes = eye_shapes(&b, &face, variant, &eyes, ye, rx, ry, w);
        b.part(&format!("eyes_{variant}"), [fx, ye], 55.0, shapes);
    }
    let yb = ye - 0.2 * hh;
    b.part("brows", [fx, yb], 57.0, Vec::new());
    for variant in ["neutral", "deadpan", "smile", "surprised", "won", "angry", "sly"] {
        let shapes = brow_shapes(&b, &face, variant, &eyes, yb, 0.09 * hw, hh, w);
        b.part(&format!("brows_{variant}"), [fx, yb], 57.0, shapes);
    }
    let mc = [fx + 0.03 * hw, hc + 0.26 * hh];
    let mw = 0.4 * hw;
    b.part("mouth", mc, 56.0, Vec::new());
    for variant in ["REST", "MBP", "TDSKN", "EEIY", "AHA", "OH", "OOWR", "FV", "LTH", "deadpan", "smile", "surprised", "won", "angry", "sly"] {
        let shapes = mouth_shapes(&b, &face, variant, mc, mw, w);
        b.part(&format!("mouth_{variant}"), mc, 56.0, shapes);
    }

    let mut rig = Rig { name: name.into(), parts: b.parts, ground: 0.0, center: 0.0, height: 1.0, soles: Vec::new(), ink }.finish();
    let bounds = super::shapes_bounds(&rig.parts.iter().filter(|p| p.variant.is_none() && p.name != "mic" && p.name != "prop").flat_map(|p| p.shapes.iter().cloned()).collect::<Vec<_>>());
    rig.height = bounds.map_or(1000.0, |b| -b[1]);
    rig
}

/// The costumes every kit character can wear.
pub const COSTUMES: &[(&str, &str)] = &[
    ("crown", "a gold crown"),
    ("party_hat", "a pink party cone"),
    ("fedora", "a dark fedora"),
    ("wizard", "a tall wizard hat"),
    ("top_hat", "a black top hat"),
    ("halo", "a golden halo"),
    ("cat_ears", "cat ears"),
    ("bow", "a big pink bow"),
    ("mask", "a domino mask"),
    ("shades", "dark sunglasses"),
];

#[allow(clippy::too_many_arguments)]
fn costume_shapes(b: &Builder, l: &Look, name: &str, top: f64, hc: f64, hw: f64, hh: f64, fx: f64) -> (f64, Vec<Shape>) {
    let w = b.w;
    let ye = hc - 0.03 * hh;
    match name {
        "crown" => {
            let base = top + 0.1 * hh;
            (61.0, vec![
                b.pathf(path(&[('M', &[-0.26 * hw, base]), ('L', &[-0.28 * hw, base - 0.22 * hh]), ('L', &[-0.13 * hw, base - 0.1 * hh]), ('L', &[0.0, base - 0.3 * hh]), ('L', &[0.13 * hw, base - 0.1 * hh]), ('L', &[0.28 * hw, base - 0.22 * hh]), ('L', &[0.26 * hw, base]), ('Z', &[])]), hex(0xF2C230)),
                b.flat(Geom::Ellipse { center: [0.0, base - 0.08 * hh], radius: [0.035 * hw, 0.035 * hw] }, hex(0xD8323C)),
                b.flat(Geom::Ellipse { center: [-0.16 * hw, base - 0.05 * hh], radius: [0.025 * hw, 0.025 * hw] }, hex(0x3C8AD8)),
                b.flat(Geom::Ellipse { center: [0.16 * hw, base - 0.05 * hh], radius: [0.025 * hw, 0.025 * hw] }, hex(0x3C8AD8)),
            ])
        }
        "party_hat" => (61.0, vec![
            b.pathf(path(&[('M', &[-0.2 * hw, top + 0.12 * hh]), ('L', &[0.04 * hw, top - 0.42 * hh]), ('L', &[0.22 * hw, top + 0.1 * hh]), ('Q', &[0.0, top + 0.17 * hh, -0.2 * hw, top + 0.12 * hh]), ('Z', &[])]), hex(0xFF5AA5)),
            b.stroke(path(&[('M', &[-0.1 * hw, top - 0.06 * hh]), ('L', &[0.14 * hw, top - 0.12 * hh])]), hex(0xFFE14D), w * 1.6),
            b.filled(Geom::Ellipse { center: [0.04 * hw, top - 0.44 * hh], radius: [0.05 * hw, 0.05 * hw] }, hex(0xFFE14D)),
        ]),
        "fedora" => (61.0, vec![
            b.filled(Geom::Ellipse { center: [0.0, top + 0.13 * hh], radius: [0.62 * hw, 0.07 * hh] }, hex(0x3A3530)),
            b.pathf(path(&[('M', &[-0.34 * hw, top + 0.12 * hh]), ('C', &[-0.37 * hw, top - 0.3 * hh, 0.37 * hw, top - 0.3 * hh, 0.34 * hw, top + 0.12 * hh]), ('Z', &[])]), hex(0x3A3530)),
            b.flat(Geom::Rect { pos: [-0.34 * hw, top + 0.02 * hh], size: [0.68 * hw, 0.07 * hh], radius: 4.0 }, hex(0xB03A2E)),
        ]),
        "wizard" => (61.0, vec![
            b.filled(Geom::Ellipse { center: [0.0, top + 0.14 * hh], radius: [0.58 * hw, 0.08 * hh] }, hex(0x4B3BA8)),
            b.pathf(path(&[('M', &[-0.36 * hw, top + 0.14 * hh]), ('L', &[0.02 * hw, top - 0.72 * hh]), ('Q', &[0.22 * hw, top - 0.84 * hh, 0.3 * hw, top - 0.62 * hh]), ('L', &[0.36 * hw, top + 0.14 * hh]), ('Z', &[])]), hex(0x4B3BA8)),
            b.flat(Geom::Ellipse { center: [-0.05 * hw, top - 0.2 * hh], radius: [0.04 * hw, 0.04 * hw] }, hex(0xFFE14D)),
            b.flat(Geom::Ellipse { center: [0.12 * hw, top - 0.42 * hh], radius: [0.03 * hw, 0.03 * hw] }, hex(0xFFE14D)),
        ]),
        "top_hat" => (61.0, vec![
            b.filled(Geom::Rect { pos: [-0.26 * hw, top - 0.52 * hh], size: [0.52 * hw, 0.6 * hh], radius: 8.0 }, hex(0x1E1E22)),
            b.flat(Geom::Rect { pos: [-0.26 * hw, top - 0.06 * hh], size: [0.52 * hw, 0.07 * hh], radius: 3.0 }, hex(0xB03A2E)),
            b.filled(Geom::Rect { pos: [-0.44 * hw, top + 0.04 * hh], size: [0.88 * hw, 0.07 * hh], radius: 10.0 }, hex(0x1E1E22)),
        ]),
        "halo" => (61.0, vec![Shape { geom: Geom::Ellipse { center: [0.0, top - 0.14 * hh], radius: [0.3 * hw, 0.07 * hh] }, fill: None, stroke: Some(hex(0xFFD84A)), width: 16.0, cap: Cap::Round, join: Join::Round }]),
        "cat_ears" => {
            let ear = |s: f64| {
                let x = s * 0.28 * hw;
                vec![
                    b.pathf(path(&[('M', &[x - 0.13 * hw, top + 0.12 * hh]), ('L', &[x + s * 0.06 * hw, top - 0.2 * hh]), ('L', &[x + 0.13 * hw, top + 0.1 * hh]), ('Z', &[])]), l.hair),
                    b.flat(Geom::Path(path(&[('M', &[x - 0.07 * hw, top + 0.08 * hh]), ('L', &[x + s * 0.05 * hw, top - 0.1 * hh]), ('L', &[x + 0.07 * hw, top + 0.07 * hh]), ('Z', &[])]).into()), hex(0xF4A0B8)),
                ]
            };
            let mut shapes = ear(-1.0);
            shapes.extend(ear(1.0));
            (44.0, shapes)
        }
        "bow" => {
            let c = [0.3 * hw, top + 0.1 * hh];
            (61.0, vec![
                b.pathf(path(&[('M', &c), ('C', &[c[0] - 0.22 * hw, c[1] - 0.2 * hh, c[0] - 0.24 * hw, c[1] + 0.14 * hh, c[0], c[1]]), ('Z', &[])]), hex(0xFF6FA8)),
                b.pathf(path(&[('M', &c), ('C', &[c[0] + 0.22 * hw, c[1] - 0.2 * hh, c[0] + 0.24 * hw, c[1] + 0.14 * hh, c[0], c[1]]), ('Z', &[])]), hex(0xFF6FA8)),
                b.filled(Geom::Ellipse { center: c, radius: [0.045 * hw, 0.045 * hw] }, hex(0xE0508A)),
            ])
        }
        // Under the eyes (they show through it).
        "mask" => (54.0, vec![b.pathf(path(&[
            ('M', &[fx - 0.36 * hw, ye - 0.02 * hh]),
            ('Q', &[fx - 0.3 * hw, ye - 0.2 * hh, fx - 0.02 * hw, ye - 0.12 * hh]),
            ('Q', &[fx + 0.3 * hw, ye - 0.2 * hh, fx + 0.34 * hw, ye - 0.02 * hh]),
            ('Q', &[fx + 0.3 * hw, ye + 0.16 * hh, fx - 0.01 * hw, ye + 0.08 * hh]),
            ('Q', &[fx - 0.32 * hw, ye + 0.16 * hh, fx - 0.36 * hw, ye - 0.02 * hh]),
            ('Z', &[]),
        ]), hex(0x1C1C22))]),
        "shades" => {
            let mut shapes = Vec::new();
            for ex in [fx - 0.19 * hw, fx + 0.17 * hw] {
                shapes.push(b.filled(Geom::Rect { pos: [ex - 0.13 * hw, ye - 0.09 * hh], size: [0.26 * hw, 0.18 * hh], radius: 14.0 }, hex(0x15151A)));
                shapes.push(b.flat(Geom::Rect { pos: [ex - 0.08 * hw, ye - 0.06 * hh], size: [0.08 * hw, 0.03 * hh], radius: 4.0 }, [1.0, 1.0, 1.0, 0.35]));
            }
            shapes.push(b.stroke(path(&[('M', &[fx - 0.06 * hw, ye - 0.04 * hh]), ('L', &[fx + 0.04 * hw, ye - 0.04 * hh])]), hex(0x15151A), w * 1.4));
            (58.5, shapes)
        }
        _ => (61.0, Vec::new()),
    }
}

/// A kit character with its own look, or with changes: `look(name)` then
/// [`Look::set`] for each field.
pub fn character(name: &str) -> Option<Rig> {
    Some(build(name, &look(name)?))
}

#[allow(clippy::too_many_arguments)]
fn eye_shapes(b: &Builder, f: &FaceInk, variant: &str, eyes: &[(f64, f64); 2], ye: f64, rx: f64, ry: f64, w: f64) -> Vec<Shape> {
    let mut out = Vec::new();
    for (k, (ex, sx)) in eyes.iter().enumerate() {
        let (ex, rx) = (*ex, rx * sx);
        let white = |out: &mut Vec<Shape>, scale: f64| {
            if f.screen {
                out.push(Shape::fill(Geom::Rect { pos: [ex - rx * 0.62 * scale, ye - ry * scale], size: [rx * 1.24 * scale, ry * 2.0 * scale], radius: rx * 0.3 }, f.white, None, 0.0));
            } else {
                out.push(Shape::fill(Geom::Ellipse { center: [ex, ye], radius: [rx * scale, ry * scale] }, f.white, Some(b.ink), w * 0.8));
            }
        };
        let pupil = |out: &mut Vec<Shape>, dx: f64, dy: f64, r: f64| {
            if !f.screen {
                out.push(Shape::fill(Geom::Ellipse { center: [ex + dx * rx, ye + dy * ry], radius: [r * rx, r * rx] }, f.pupil, None, 0.0));
                out.push(Shape::fill(Geom::Ellipse { center: [ex + dx * rx - 0.16 * rx, ye + dy * ry - 0.18 * rx], radius: [0.13 * rx, 0.13 * rx] }, [1.0, 1.0, 1.0, 0.9], None, 0.0));
            }
        };
        // An upper lid down to `low` (a fraction of ry below the centre),
        // tilted by `tilt` (inner corner lower when positive).
        let lid = |out: &mut Vec<Shape>, low: f64, tilt: f64| {
            let inner = if k == 0 { 1.0 } else { -1.0 };
            let (yl, yr) = (ye + (low - tilt * inner) * ry, ye + (low + tilt * inner) * ry);
            let (x0, x1) = (ex - rx * 1.12, ex + rx * 1.12);
            let top = ye - ry * 1.35;
            let lid_color = if f.screen { b.ink } else { f.lid };
            let lid_color = if f.screen { shade(lid_color, 1.0) } else { lid_color };
            out.push(Shape::fill(Geom::Path(path(&[('M', &[x0, yl]), ('L', &[x0, top]), ('L', &[x1, top]), ('L', &[x1, yr]), ('Z', &[])]).into()), if f.screen { [0.0, 0.0, 0.0, 0.0] } else { lid_color }, None, 0.0));
            out.push(Shape::line(path(&[('M', &[x0 + rx * 0.1, yl]), ('L', &[x1 - rx * 0.1, yr])]), f.line, w));
        };
        match variant {
            "closed" => out.push(Shape::line(path(&[('M', &[ex - rx, ye + 0.1 * ry]), ('Q', &[ex, ye + 0.6 * ry, ex + rx, ye + 0.1 * ry])]), f.line, w)),
            "won" => out.push(Shape::line(path(&[('M', &[ex - rx, ye + 0.3 * ry]), ('Q', &[ex, ye - 0.8 * ry, ex + rx, ye + 0.3 * ry])]), f.line, w * 1.2)),
            "surprised" => {
                white(&mut out, 1.2);
                pupil(&mut out, 0.1, 0.0, 0.28);
            }
            "deadpan" => {
                if f.screen {
                    out.push(Shape::fill(Geom::Rect { pos: [ex - rx * 0.62, ye - 0.05 * ry], size: [rx * 1.24, ry * 0.9], radius: rx * 0.25 }, f.white, None, 0.0));
                } else {
                    white(&mut out, 1.0);
                    pupil(&mut out, 0.15, 0.35, 0.42);
                    lid(&mut out, -0.05, 0.0);
                }
            }
            "sly" => {
                if f.screen {
                    out.push(Shape::fill(Geom::Rect { pos: [ex - rx * 0.2, ye - 0.05 * ry], size: [rx * 0.9, ry * 0.9], radius: rx * 0.25 }, f.white, None, 0.0));
                } else {
                    white(&mut out, 1.0);
                    pupil(&mut out, 0.5, 0.3, 0.4);
                    lid(&mut out, 0.0, 0.0);
                }
            }
            "angry" => {
                if f.screen {
                    let inner = if k == 0 { 1.0 } else { -1.0 };
                    let inner: f64 = inner;
                    let (yl, yr) = (ye - ry * (1.0 - 0.45 * inner.max(0.0)), ye - ry * (1.0 - 0.45 * (-inner).max(0.0)));
                    out.push(Shape::fill(Geom::Path(path(&[('M', &[ex - rx * 0.62, yl]), ('L', &[ex + rx * 0.62, yr]), ('L', &[ex + rx * 0.62, ye + ry]), ('L', &[ex - rx * 0.62, ye + ry]), ('Z', &[])]).into()), f.white, None, 0.0));
                } else {
                    white(&mut out, 1.0);
                    pupil(&mut out, 0.1, 0.2, 0.42);
                    lid(&mut out, -0.4, 0.35);
                }
            }
            _ => {
                white(&mut out, 1.0);
                pupil(&mut out, 0.25, 0.1, 0.45);
            }
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn brow_shapes(_b: &Builder, f: &FaceInk, variant: &str, eyes: &[(f64, f64); 2], yb: f64, bl: f64, hh: f64, w: f64) -> Vec<Shape> {
    let mut out = Vec::new();
    for (k, (ex, sx)) in eyes.iter().enumerate() {
        let (ex, bl) = (*ex, bl * sx);
        let inner = if k == 0 { 1.0 } else { -1.0 };
        let arc = |lift: f64, bend: f64| path(&[('M', &[ex - bl, yb - lift * hh]), ('Q', &[ex, yb - (lift + bend) * hh, ex + bl, yb - lift * hh])]);
        let d = match variant {
            "deadpan" => path(&[('M', &[ex - bl, yb + 0.03 * hh]), ('L', &[ex + bl, yb + 0.035 * hh])]),
            "smile" | "won" => arc(0.025, 0.05),
            "surprised" => arc(0.07, 0.07),
            "angry" => {
                let (yl, yr) = (yb + (0.005 - 0.045 * inner) * hh, yb + (0.005 + 0.045 * inner) * hh);
                path(&[('M', &[ex - bl, yl]), ('L', &[ex + bl, yr])])
            }
            "sly" => if k == 0 { path(&[('M', &[ex - bl, yb + 0.03 * hh]), ('L', &[ex + bl, yb + 0.03 * hh])]) } else { arc(0.05, 0.06) },
            _ => arc(0.0, 0.03),
        };
        out.push(Shape::line(d, f.line, w * 1.9));
    }
    out
}

fn mouth_shapes(b: &Builder, f: &FaceInk, variant: &str, c: [f64; 2], mw: f64, w: f64) -> Vec<Shape> {
    // Unit coordinates: x across the mouth's width, y down, both × mw.
    let p = |pts: &[(char, &[f64])]| -> crate::geometry::VectorPath {
        let mapped: Vec<(char, Vec<f64>)> = pts.iter().map(|(cmd, xs)| (*cmd, xs.chunks(2).flat_map(|xy| [c[0] + xy[0] * mw, c[1] + xy[1] * mw]).collect())).collect();
        let refs: Vec<(char, &[f64])> = mapped.iter().map(|(cmd, xs)| (*cmd, xs.as_slice())).collect();
        path(&refs)
    };
    let e = |cx: f64, cy: f64, rx: f64, ry: f64| Geom::Ellipse { center: [c[0] + cx * mw, c[1] + cy * mw], radius: [rx * mw, ry * mw] };
    let open = |d: crate::geometry::VectorPath| Shape::fill(Geom::Path(d.into()), f.dark, Some(f.line), w * 0.9);
    let solid = |d: crate::geometry::VectorPath, color: Color| Shape::fill(Geom::Path(d.into()), color, None, 0.0);
    let line = |d: crate::geometry::VectorPath, width: f64| Shape::line(d, f.line, width);
    let top_teeth = |y: f64, half: f64| solid(p(&[('M', &[-half, y]), ('Q', &[0.0, y - 0.07, half, y]), ('L', &[half - 0.02, y + 0.05]), ('Q', &[0.0, y - 0.01, -half + 0.02, y + 0.05]), ('Z', &[])]), f.teeth);
    let _ = b;
    match variant {
        "MBP" => vec![line(p(&[('M', &[-0.34, 0.0]), ('Q', &[0.0, -0.03, 0.34, 0.0])]), w * 1.6), line(p(&[('M', &[-0.16, 0.08]), ('Q', &[0.0, 0.13, 0.16, 0.08])]), w * 0.7)],
        "TDSKN" => vec![open(p(&[('M', &[-0.36, 0.0]), ('Q', &[0.0, -0.13, 0.36, 0.0]), ('Q', &[0.0, 0.17, -0.36, 0.0]), ('Z', &[])])), top_teeth(-0.04, 0.25)],
        "EEIY" => vec![
            open(p(&[('M', &[-0.46, -0.02]), ('Q', &[0.0, -0.13, 0.46, -0.02]), ('Q', &[0.0, 0.2, -0.46, -0.02]), ('Z', &[])])),
            top_teeth(-0.06, 0.34),
            solid(p(&[('M', &[-0.22, 0.07]), ('Q', &[0.0, 0.1, 0.22, 0.07]), ('L', &[0.18, 0.04]), ('Q', &[0.0, 0.06, -0.18, 0.04]), ('Z', &[])]), f.teeth),
        ],
        "AHA" => vec![
            open(p(&[('M', &[-0.38, -0.06]), ('C', &[-0.3, -0.2, 0.3, -0.2, 0.38, -0.06]), ('C', &[0.36, 0.36, -0.36, 0.36, -0.38, -0.06]), ('Z', &[])])),
            Shape::fill(e(0.0, 0.17, 0.2, 0.08), f.tongue, None, 0.0),
            top_teeth(-0.12, 0.28),
        ],
        "OH" => vec![Shape::fill(e(0.0, 0.04, 0.22, 0.2), f.dark, Some(f.line), w * 0.9), Shape::fill(e(0.0, 0.14, 0.11, 0.05), f.tongue, None, 0.0)],
        "OOWR" => vec![Shape::fill(e(0.0, 0.03, 0.12, 0.12), f.dark, Some(f.line), w * 1.7)],
        "FV" => vec![
            open(p(&[('M', &[-0.34, 0.0]), ('Q', &[0.0, -0.11, 0.34, 0.0]), ('Q', &[0.0, 0.08, -0.34, 0.0]), ('Z', &[])])),
            top_teeth(-0.045, 0.24),
            line(p(&[('M', &[-0.28, 0.06]), ('Q', &[0.0, 0.12, 0.28, 0.06])]), w),
        ],
        "LTH" => vec![
            open(p(&[('M', &[-0.34, -0.03]), ('Q', &[0.0, -0.17, 0.34, -0.03]), ('Q', &[0.0, 0.25, -0.34, -0.03]), ('Z', &[])])),
            top_teeth(-0.08, 0.26),
            Shape::fill(e(0.0, -0.02, 0.12, 0.06), f.tongue, None, 0.0),
        ],
        "deadpan" => vec![line(p(&[('M', &[-0.32, 0.0]), ('Q', &[0.0, -0.01, 0.32, 0.03])]), w * 1.1)],
        "smile" => vec![line(p(&[('M', &[-0.4, -0.06]), ('Q', &[0.0, 0.26, 0.4, -0.06])]), w * 1.15)],
        "surprised" => vec![Shape::fill(e(0.0, 0.04, 0.13, 0.15), f.dark, Some(f.line), w * 0.9)],
        "won" => vec![
            open(p(&[('M', &[-0.44, -0.08]), ('Q', &[0.0, -0.02, 0.44, -0.08]), ('Q', &[0.34, 0.36, 0.0, 0.36]), ('Q', &[-0.34, 0.36, -0.44, -0.08]), ('Z', &[])])),
            Shape::fill(e(0.0, 0.25, 0.16, 0.07), f.tongue, None, 0.0),
            top_teeth(-0.05, 0.36),
        ],
        "angry" => vec![line(p(&[('M', &[-0.32, 0.08]), ('Q', &[0.0, -0.12, 0.32, 0.08])]), w * 1.2)],
        "sly" => vec![line(p(&[('M', &[-0.3, 0.02]), ('Q', &[0.1, 0.08, 0.38, -0.1])]), w * 1.15)],
        _ => vec![line(p(&[('M', &[-0.36, 0.0]), ('Q', &[0.0, 0.04, 0.36, 0.0])]), w)],
    }
}
