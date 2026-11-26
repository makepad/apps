//! `Character{…}` layers (lane CH1): 2D cut-out character rigs for
//! character-driven cartoons, evaluated as a pure function of time like
//! every other layer.
//!
//! A character is a [`Rig`]: named parts (hips, torso, head, upper and
//! lower arms, hands, legs, feet, and the face slots eyes, brows and
//! mouth), each with a pivot, a parent and a draw order. It comes from one
//! model: a built-in kit character with its look ([`kit`]), or an SVG whose
//! groups carry the part ids ([`svg`]). Every shot places the same model, so
//! the character looks the same in every shot.
//!
//! Each frame the layer's cues are solved into joint angles ([`pose`]: pose
//! presets blended with eases, a walk locked to the beat, nods and bobs,
//! secondary motion), face variants ([`face`]: expressions, seeded blinks)
//! and a mouth shape ([`lipsync`]: phones to the nine visemes, opened by
//! the voice's loudness, one frame ahead of the audio). The solved rig
//! lowers to ordinary `Rect`/`Ellipse`/`Path` items: distance-field quads
//! for the limbs and heads, a few paths for the face, so the drawing side
//! needs nothing of its own.

pub mod emote;
pub mod face;
pub mod kit;
pub mod lipsync;
pub mod pose;
pub mod svg;
#[cfg(test)]
mod tests;

use crate::features::VoiceFeatures;
use crate::frame::{Affine, Item, ItemKind, Local};
use crate::geometry::VectorPath;
use crate::model::{AssetRef, Blend, BlendForm, Cap, Color, Join, Prop, Time};
use lipsync::Viseme;
use pose::{Joint, Pose};
use std::sync::Arc;

/// A shape's outline in the model's coordinates.
#[derive(Clone, Debug, PartialEq)]
pub enum Geom {
    /// Absolute path geometry, shared by the rig and its frames.
    Path(Arc<VectorPath>),
    Ellipse { center: [f64; 2], radius: [f64; 2] },
    /// An axis-aligned rounded rectangle from `pos`.
    Rect { pos: [f64; 2], size: [f64; 2], radius: f64 },
    /// A limb: a capsule from `a` to `b`, `width` across (a rounded
    /// rectangle turned along a→b).
    Capsule { a: [f64; 2], b: [f64; 2], width: f64 },
}

/// One drawn shape of a part.
#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    pub geom: Geom,
    pub fill: Option<Color>,
    pub stroke: Option<Color>,
    pub width: f64,
    pub cap: Cap,
    pub join: Join,
}

impl Shape {
    pub fn fill(geom: Geom, fill: Color, stroke: Option<Color>, width: f64) -> Self {
        Self { geom, fill: Some(fill), stroke, width, cap: Cap::Round, join: Join::Round }
    }

    pub fn line(path: VectorPath, stroke: Color, width: f64) -> Self {
        Self { geom: Geom::Path(path.into()), fill: None, stroke: Some(stroke), width, cap: Cap::Round, join: Join::Round }
    }
}

/// Which slot a variant part belongs to: `mouth_AHA`, `eyes_closed`,
/// `brows_angry`, `costume_crown`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Slot {
    Mouth,
    Eyes,
    Brows,
    /// Hats, masks, bows: worn by cue, several at once (`@crown+mask`).
    Costume,
}

pub const SLOTS: [Slot; 4] = [Slot::Mouth, Slot::Eyes, Slot::Brows, Slot::Costume];

impl Slot {
    pub fn name(self) -> &'static str {
        match self {
            Slot::Mouth => "mouth",
            Slot::Eyes => "eyes",
            Slot::Brows => "brows",
            Slot::Costume => "costume",
        }
    }

    /// `mouth_AHA` → (Mouth, "AHA").
    pub fn of_part(name: &str) -> Option<(Slot, &str)> {
        for slot in SLOTS {
            if let Some(variant) = name.strip_prefix(slot.name()).and_then(|rest| rest.strip_prefix('_')) {
                if !variant.is_empty() {
                    return Some((slot, variant));
                }
            }
        }
        None
    }
}

/// A named piece of the character, turning about `pivot` (model
/// coordinates, rest pose) and carried by its parent.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub name: String,
    pub parent: Option<usize>,
    pub pivot: [f64; 2],
    /// Draw order: higher draws later (in front).
    pub z: f64,
    pub shapes: Vec<Shape>,
    /// A face variant (`mouth_AHA`): shown only when its slot picks it.
    pub variant: Option<(Slot, String)>,
    /// The joint that turns it, when it is one of the skeleton's.
    pub joint: Option<Joint>,
    /// Secondary motion: hair, cloth, pendants and antennas sway on a spring.
    pub sways: bool,
}

/// A character model: its parts in their rest pose, feet on the ground at
/// y = `ground` (y grows downward), `height` from the ground to the top.
#[derive(Clone, Debug, PartialEq)]
pub struct Rig {
    pub name: String,
    pub parts: Vec<Part>,
    pub ground: f64,
    /// The x the character stands on.
    pub center: f64,
    pub height: f64,
    /// Where each foot's sole touches the ground at rest: the solver keeps
    /// the lowest one on the ground (a walk bobs, a sit drops the hips).
    pub soles: Vec<(usize, [f64; 2])>,
    /// The outline colour (emotes are drawn with it).
    pub ink: Color,
}

impl Rig {
    pub fn find(&self, name: &str) -> Option<usize> {
        self.parts.iter().position(|part| part.name == name)
    }

    /// The part a joint turns.
    pub fn joint_part(&self, joint: Joint) -> Option<usize> {
        self.parts.iter().position(|part| part.joint == Some(joint))
    }

    /// The two limb segments and end pivot, connected in the model's
    /// actual hierarchy (custom parts may sit between the named joints).
    pub(crate) fn reach_chain(&self, end: Joint) -> Option<[usize; 3]> {
        let lower = end.parent()?;
        let upper = lower.parent()?;
        let chain = [self.joint_part(upper)?, self.joint_part(lower)?, self.joint_part(end)?];
        for pair in chain.windows(2) {
            let mut parent = self.parts[pair[1]].parent;
            while parent.is_some() && parent != Some(pair[0]) { parent = self.parts[parent.unwrap()].parent; }
            if parent.is_none() { return None; }
        }
        Some(chain)
    }

    /// The variants a slot has (`AHA`, `MBP`, …).
    pub fn variants(&self, slot: Slot) -> Vec<&str> {
        self.parts.iter().filter_map(|part| part.variant.as_ref().filter(|(s, _)| *s == slot).map(|(_, v)| v.as_str())).collect()
    }

    /// The distance between two joints' pivots (a limb's length), or `fallback`.
    pub fn span(&self, from: Joint, to: Joint, fallback: f64) -> f64 {
        match (self.joint_part(from), self.joint_part(to)) {
            (Some(a), Some(b)) => {
                let (a, b) = (self.parts[a].pivot, self.parts[b].pivot);
                ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt().max(1e-6)
            }
            _ => fallback,
        }
    }

    /// Fill in what is derived: joints from names, parents from the
    /// skeleton where the source gave none, slot variants, sway flags and
    /// soles. Parts must be in the order they were written.
    pub fn finish(mut self) -> Rig {
        for part in &mut self.parts {
            part.joint = Joint::from_name(&part.name);
            part.variant = Slot::of_part(&part.name).map(|(slot, variant)| (slot, variant.to_string()));
            part.sways = pose::sways(&part.name);
        }
        let names: Vec<String> = self.parts.iter().map(|p| p.name.clone()).collect();
        for index in 0..self.parts.len() {
            if self.parts[index].parent.is_some() {
                continue;
            }
            let name = names[index].as_str();
            let wanted: Option<String> = match (&self.parts[index].variant, self.parts[index].joint) {
                (Some((slot, _)), _) => Some(slot.name().to_string()),
                (None, Some(joint)) => joint.parent().map(|p| p.name().to_string()),
                (None, None) => Some(pose::default_parent(name).to_string()),
            };
            // The wanted parent, else its own parent in turn (a rig with no
            // `hips` hangs its legs off the torso).
            let mut wanted = wanted;
            while let Some(want) = wanted {
                if want == name {
                    break;
                }
                if let Some(at) = names.iter().position(|n| *n == want) {
                    self.parts[index].parent = Some(at);
                    break;
                }
                wanted = match Joint::from_name(&want) {
                    Some(joint) => joint.parent().map(|p| p.name().to_string()),
                    // A slot's variants without the slot's group hang off the head.
                    None if SLOTS.iter().any(|s| s.name() == want) => Some("head".into()),
                    None => None,
                };
            }
        }
        // A cycle (a part its own ancestor) is cut where it closes.
        for index in 0..self.parts.len() {
            let mut seen = vec![index];
            let mut at = self.parts[index].parent;
            while let Some(p) = at {
                if seen.contains(&p) {
                    self.parts[index].parent = None;
                    break;
                }
                seen.push(p);
                at = self.parts[p].parent;
            }
        }
        if self.soles.is_empty() {
            for joint in [Joint::FootL, Joint::FootR] {
                if let Some(at) = self.joint_part(joint) {
                    if let Some(b) = shapes_bounds(&self.parts[at].shapes) {
                        self.soles.push((at, [(b[0] + b[2]) * 0.5, b[3]]));
                    }
                }
            }
        }
        self
    }
}

/// Bounds `[x0, y0, x1, y1]` of shapes in model coordinates.
pub fn shapes_bounds(shapes: &[Shape]) -> Option<[f64; 4]> {
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    let mut add = |x: f64, y: f64| {
        b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
    };
    for shape in shapes {
        match &shape.geom {
            Geom::Path(path) => {
                if let Some(p) = crate::geometry::bounds(path) {
                    add(p[0], p[1]);
                    add(p[2], p[3]);
                }
            }
            Geom::Ellipse { center, radius } => {
                add(center[0] - radius[0], center[1] - radius[1]);
                add(center[0] + radius[0], center[1] + radius[1]);
            }
            Geom::Rect { pos, size, .. } => {
                add(pos[0], pos[1]);
                add(pos[0] + size[0], pos[1] + size[1]);
            }
            Geom::Capsule { a, b: e, width } => {
                let r = width * 0.5;
                add(a[0].min(e[0]) - r, a[1].min(e[1]) - r);
                add(a[0].max(e[0]) + r, a[1].max(e[1]) + r);
            }
        }
    }
    (b[0] <= b[2]).then_some(b)
}

/// Where a Character's model comes from.
#[derive(Clone, Debug, PartialEq)]
pub enum Model {
    /// Built from a kit character and its look, or from inline SVG, at load.
    Rig(Arc<Rig>),
    /// A library SVG not yet given by the host
    /// ([`crate::MotionDoc::resolve_characters`]): draws nothing.
    Asset(AssetRef),
    Empty,
}

/// A cue: from `t` on, `name` (a pose or an expression), reached over
/// `blend` seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct Cue {
    pub t: Time,
    pub name: String,
    pub blend: f64,
}

/// A walk: from `start`, `steps` steps of `stride` document units toward
/// `dir` (+1 right, -1 left), `per_beat` steps a beat (1: one step per beat).
#[derive(Clone, Debug, PartialEq)]
pub struct Walk {
    pub start: Time,
    pub steps: f64,
    pub stride: f64,
    pub dir: f64,
    pub per_beat: f64,
}

/// A float: from `start` (to `end`, when given) the character rises
/// `height` document units into the float pose and bobs `bob` up and down
/// once every `period` beats.
#[derive(Clone, Debug, PartialEq)]
pub struct Float {
    pub start: Time,
    pub end: Option<Time>,
    pub height: f64,
    pub bob: f64,
    pub period: f64,
}

/// An emote shown from `t` for `length` seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct EmoteCue {
    pub t: Time,
    pub name: String,
    pub length: f64,
    pub path: Arc<VectorPath>,
}

/// One `parts` entry: offsets added to a part on top of its pose.
#[derive(Clone, Debug, PartialEq)]
pub struct PartKeys {
    pub rotate: Prop<1>,
    pub x: Prop<1>,
    pub y: Prop<1>,
    pub scale: Prop<2>,
    pub opacity: Prop<1>,
}

/// A `Character{…}` layer as loaded.
#[derive(Clone, Debug, PartialEq)]
pub struct CharacterLayer {
    pub model: Model,
    /// Head-to-feet height in document units.
    pub height: f64,
    /// +1 facing right (as the model is drawn), -1 mirrored; `None`: the
    /// direction of the last walk begun, else right.
    pub facing: Option<f64>,
    pub poses: Vec<Cue>,
    /// A value-driven pose in place of the cue timeline.
    pub pose_blend: Option<BlendForm<Pose>>,
    pub expressions: Vec<Cue>,
    /// Costume cues: `@crown`, `@crown+mask`, `@none`.
    pub costumes: Vec<Cue>,
    /// Mouth shapes by hand (`mouth: [[1.2, @OH], [1.4, @MBP]]`), when no
    /// voice drives it.
    pub mouths: Vec<Cue>,
    pub walks: Vec<Walk>,
    pub floats: Vec<Float>,
    pub emotes: Vec<EmoteCue>,
    /// Seeded variety between the copies of a `Repeat` (a crowd of the
    /// character): colours, proportions, groove timing, blinks; 0..1.
    pub vary: f64,
    /// Head nod on every beat, degrees.
    pub nod: f64,
    /// Knee bob on every beat, degrees.
    pub bob: f64,
    /// Mean seconds between blinks (0: never).
    pub blink: f64,
    pub seed: u32,
    /// Secondary motion's strength (0: none).
    pub sway: f64,
    /// The voice (`voice(@vo)`) the mouth follows.
    pub lip_sync: Option<String>,
    /// Frames the mouth leads the audio by.
    pub lead: f64,
    pub parts: Vec<(String, PartKeys)>,
    /// End joint -> keyable target, Character layer units relative to its feet.
    pub reach: Vec<(Joint, Prop<2>)>,
}

impl CharacterLayer {
    /// Every time the layer holds, for resolution.
    pub fn times_mut(&mut self) -> Vec<&mut Time> {
        let mut out: Vec<&mut Time> = Vec::new();
        for cue in self.poses.iter_mut().chain(self.expressions.iter_mut()).chain(self.costumes.iter_mut()).chain(self.mouths.iter_mut()) {
            out.push(&mut cue.t);
        }
        for walk in &mut self.walks {
            out.push(&mut walk.start);
        }
        for float in &mut self.floats {
            out.push(&mut float.start);
            if let Some(end) = &mut float.end {
                out.push(end);
            }
        }
        for emote in &mut self.emotes {
            out.push(&mut emote.t);
        }
        out
    }

    pub fn rig(&self) -> Option<&Rig> {
        match &self.model {
            Model::Rig(rig) => Some(rig),
            _ => None,
        }
    }
}

/// A `parts` entry sampled at the moment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PartOffset {
    pub rotate: f64,
    pub offset: [f64; 2],
    pub scale: [f64; 2],
    pub opacity: f64,
}

impl Default for PartOffset {
    fn default() -> Self {
        Self { rotate: 0.0, offset: [0.0, 0.0], scale: [1.0, 1.0], opacity: 1.0 }
    }
}

/// What the solver reads besides the layer.
pub struct Moment<'a> {
    /// The layer's clock.
    pub t: f64,
    /// The beat number (fractional) at a moment of the layer's clock.
    pub beat: &'a dyn Fn(f64) -> f64,
    /// The lip-synced voice, on the host's clock.
    pub voice: Option<&'a VoiceFeatures>,
    /// Host time of the layer's 0.
    pub host: f64,
    pub fps: f64,
    /// `parts` sampled, in the layer's order.
    pub parts: &'a [PartOffset],
    pub reach: &'a [(Joint, [f64; 2])],
    /// The value-driven pose, sampled on the layer clock by the evaluator.
    pub pose: Option<Pose>,
    /// The copy of a `Repeat` (or the crowd member) this is: varies it when
    /// the layer `vary`s.
    pub instance: usize,
    /// A host's reactions on top of the cues (a crowd member's): raised
    /// arms (0..1 of `@arms_up`), a hop (document units up) and a nod
    /// (head degrees).
    pub arms: f64,
    pub hop: f64,
    pub nod: f64,
}

/// How one copy of a varied character differs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Variation {
    /// Hue turn of the coloured parts, degrees.
    pub hue: f64,
    pub light: f64,
    pub scale: [f64; 2],
    /// Beats the groove runs early or late.
    pub phase: f64,
}

impl Variation {
    pub const NONE: Variation = Variation { hue: 0.0, light: 1.0, scale: [1.0, 1.0], phase: 0.0 };

    /// Copy `instance`'s variation at strength `vary` (0..1).
    pub fn of(vary: f64, seed: u32, instance: usize) -> Variation {
        if vary <= 0.0 {
            return Variation::NONE;
        }
        let r = |k: u64| {
            let mut h = (seed as u64 ^ 0x5851_F42D_4C95_7F2D).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (instance as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9) ^ k.wrapping_mul(0x94D0_49BB_1331_11EB);
            h ^= h >> 31;
            h = h.wrapping_mul(0xD6E8_FEB8_6659_FD93);
            h ^= h >> 32;
            h as f64 / u64::MAX as f64 * 2.0 - 1.0
        };
        let v = vary.min(1.0);
        Variation {
            hue: 180.0 * v * r(1),
            light: 1.0 + 0.25 * v * r(2),
            scale: [1.0 + 0.14 * v * r(3), 1.0 + 0.09 * v * r(4)],
            phase: 0.18 * v * r(5),
        }
    }

    /// A colour of the copy: coloured parts turn their hue, skin tones only
    /// lighten or darken, light neutrals (an ivory hoodie) take a pastel
    /// tint, inks and whites stay.
    pub fn color(&self, c: Color) -> Color {
        if *self == Variation::NONE {
            return c;
        }
        let (r, g, b) = (c[0], c[1], c[2]);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let l = (max + min) * 0.5;
        let d = max - min;
        if l < 0.15 || l > 0.94 {
            return c;
        }
        let (s, h) = if d < 0.12 {
            (0.4 * (self.hue.abs() / 180.0).sqrt(), self.hue.rem_euclid(360.0))
        } else {
            let s = d / (1.0 - (2.0 * l - 1.0).abs()).max(1e-6);
            let h = if max == r { ((g - b) / d).rem_euclid(6.0) } else if max == g { (b - r) / d + 2.0 } else { (r - g) / d + 4.0 } * 60.0;
            // Skin tones keep their hue (only their lightness varies).
            let skin = (10.0..=45.0).contains(&h) && s < 0.75;
            (s, if skin { h } else { (h + self.hue).rem_euclid(360.0) })
        };
        let l = (l * self.light).clamp(0.05, 0.95);
        let c2 = (1.0 - (2.0 * l - 1.0).abs()) * s;
        let x = c2 * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
        let (r1, g1, b1) = match (h / 60.0) as u32 {
            0 => (c2, x, 0.0),
            1 => (x, c2, 0.0),
            2 => (0.0, c2, x),
            3 => (0.0, x, c2),
            4 => (x, 0.0, c2),
            _ => (c2, 0.0, x),
        };
        let m = l - c2 * 0.5;
        [r1 + m, g1 + m, b1 + m, c[3]]
    }
}
/// A character at one moment: each shown part's map from model
/// coordinates to the layer's, in draw order, and the emotes showing.
#[derive(Clone, Debug, PartialEq)]
pub struct Solved {
    pub unreachable: Vec<Joint>,
    pub parts: Vec<(usize, Affine, f64)>,
    pub pose: Pose,
    pub viseme: Viseme,
    /// The mouth's opening (1: as drawn).
    pub open: f64,
    pub expression: String,
    pub costume: String,
    pub blinking: bool,
    /// The walk's and float's offset from `at`, document units.
    pub offset: [f64; 2],
    pub facing: f64,
    /// Emotes showing: name, seconds into it, its length, shared outline.
    pub emotes: Vec<(String, f64, f64, Arc<VectorPath>)>,
    /// The head's model-to-layer map (emotes ride it).
    pub head: Option<Affine>,
    pub variation: Variation,
}

/// How far a float lifts at `t` (document units) and how much of the
/// float pose shows.
pub fn float_at(floats: &[Float], t: f64, beat: &dyn Fn(f64) -> f64) -> (f64, f64) {
    let smooth = |u: f64| {
        let u = u.clamp(0.0, 1.0);
        u * u * (3.0 - 2.0 * u)
    };
    let mut best = (0.0, 0.0);
    for f in floats {
        let rise = smooth((t - f.start.value) / 0.6);
        let fall = f.end.as_ref().map_or(1.0, |end| 1.0 - smooth((t - end.value) / 0.6));
        let w = rise * fall;
        if w <= 0.0 {
            continue;
        }
        let phase = (beat(t) - beat(f.start.value)) / f.period.max(1e-6);
        let lift = w * (f.height + f.bob * (std::f64::consts::TAU * phase).sin());
        if w > best.1 {
            best = (lift, w);
        }
    }
    best
}

/// Solve the layer at a moment.
pub fn solve(layer: &CharacterLayer, rig: &Rig, m: &Moment) -> Solved {
    let t = m.t;
    let variation = Variation::of(layer.vary, layer.seed, m.instance);
    let scale = layer.height / rig.height.max(1e-6);
    // Pose: the cue timeline, the walk and float over it, the groove on the beat.
    let mut pose = m.pose.unwrap_or_else(|| pose::cued(&layer.poses, t));
    let walk = pose::walk_state(&layer.walks, t, m.beat);
    let leg = rig.span(Joint::LegLU, Joint::LegLL, rig.height * 0.19) + rig.span(Joint::LegLL, Joint::FootL, rig.height * 0.19);
    if let Some(w) = &walk {
        let swing = pose::swing_for(w.stride / scale.max(1e-6), leg);
        pose = pose.mix(&pose::walking(&pose::preset("walk").unwrap(), w.phase, swing), w.weight);
    }
    let (lift, floating) = float_at(&layer.floats, t, m.beat);
    if floating > 0.0 {
        pose = pose.mix(&pose::preset("float").unwrap(), floating);
    }
    if m.arms > 0.0 {
        pose = pose.mix(&pose::preset("arms_up").unwrap(), m.arms);
    }
    let beat = (m.beat)(t) + variation.phase;
    pose::groove(&mut pose, beat, layer.nod, layer.bob);
    pose.angles[Joint::Head as usize] += m.nod;
    let facing = match (&walk, layer.facing) {
        (Some(w), _) if w.weight > 0.0 => w.dir,
        (_, Some(f)) => f,
        _ => pose::last_dir(&layer.walks, t).unwrap_or(1.0),
    };
    let mut offset = pose::walk_offset(&layer.walks, t, m.beat);
    offset[1] -= lift + m.hop;
    // Secondary motion: the body's sideways acceleration (in model units)
    // and the head's turning drive a spring.
    let (sway, lag) = if layer.sway > 0.0 {
        let s = pose::secondary(t, &|u| {
            let b = (m.beat)(u) + variation.phase;
            let x = pose::walk_offset(&layer.walks, u, m.beat)[0] / scale.max(1e-6) * facing;
            (x, pose::nod_angle(b, layer.nod))
        });
        (s.0 * layer.sway, s.1 * layer.sway)
    } else {
        (0.0, 0.0)
    };
    pose.angles[Joint::Head as usize] += lag;
    // Face and costume.
    let expression = face::expression_at(&layer.expressions, t);
    let costume = if layer.costumes.is_empty() { String::new() } else { face::expression_at(&layer.costumes, t) };
    let blink_seed = layer.seed.wrapping_add((m.instance as u32).wrapping_mul(7919));
    let blinking = layer.blink > 0.0 && face::blinking(t, layer.blink, blink_seed);
    let (viseme, open) = match (m.voice, &layer.lip_sync) {
        (Some(voice), Some(_)) => lipsync::mouth_at(voice, m.host + t + layer.lead / m.fps.max(1.0)),
        _ if !layer.mouths.is_empty() => (Viseme::from_name(&face::expression_at(&layer.mouths, t)).unwrap_or(Viseme::Rest), 1.0),
        _ => (Viseme::Rest, 1.0),
    };
    let shown = face::shown(rig, &expression, &costume, blinking, viseme, &pose);
    // Forward kinematics, parents first.
    let n = rig.parts.len();
    let mut local: Vec<Affine> = Vec::with_capacity(n);
    let mut opacity = vec![1.0; n];
    for (index, part) in rig.parts.iter().enumerate() {
        let mut angle = part.joint.map_or(0.0, |j| pose.angles[j as usize]);
        if part.sways {
            angle += sway;
        }
        let mut sc = [1.0, 1.0];
        let mut off = [0.0, 0.0];
        if part.variant.as_ref().is_some_and(|(slot, _)| *slot == Slot::Mouth) && viseme.is_open() {
            sc[1] = open;
        }
        if let Some(at) = layer.parts.iter().position(|(name, _)| *name == part.name) {
            let p = m.parts.get(at).copied().unwrap_or_default();
            angle += p.rotate;
            off = [p.offset[0] / scale.max(1e-6), p.offset[1] / scale.max(1e-6)];
            sc = [sc[0] * p.scale[0], sc[1] * p.scale[1]];
            opacity[index] = p.opacity.clamp(0.0, 1.0);
        }
        local.push(Affine::translate(off[0], off[1]).then(&Affine::about(part.pivot, &Affine::rotate(angle).then(&Affine::scale(sc[0], sc[1])))));
    }
    let mut world: Vec<Option<Affine>> = vec![None; n];
    fn resolve(i: usize, rig: &Rig, local: &[Affine], world: &mut Vec<Option<Affine>>) -> Affine {
        if let Some(w) = world[i] {
            return w;
        }
        let w = match rig.parts[i].parent {
            Some(p) => resolve(p, rig, local, world).then(&local[i]),
            None => local[i],
        };
        world[i] = Some(w);
        w
    }
    for i in 0..n {
        resolve(i, rig, &local, &mut world);
    }
    // Keep the lowest sole on the ground.
    let lowest = rig.soles.iter().map(|(part, p)| world[*part].unwrap().apply(*p)[1]).fold(f64::MIN, f64::max);
    let drop = if rig.soles.is_empty() { 0.0 } else { rig.ground - lowest };
    let root = Affine::translate(offset[0], offset[1])
        .then(&Affine::scale(scale * facing * variation.scale[0], scale * variation.scale[1]))
        .then(&Affine::translate(-rig.center, -rig.ground + drop));
    let unreachable = if m.reach.is_empty() {
        Vec::new()
    } else {
        for map in &mut world { *map = Some(root.then(&map.unwrap())); }
        pose::reach(rig, &mut world, m.reach)
    };
    let placed_part = |i: usize| if m.reach.is_empty() { root.then(&world[i].unwrap()) } else { world[i].unwrap() };
    let mut order: Vec<usize> = (0..n).filter(|&i| shown[i] && opacity[i] > 0.0).collect();
    let z = |i: usize| face::z_of(rig, i, &pose);
    order.sort_by(|&a, &b| z(a).partial_cmp(&z(b)).unwrap_or(std::cmp::Ordering::Equal).then(a.cmp(&b)));
    let emotes = layer.emotes.iter().filter(|e| t >= e.t.value && t < e.t.value + e.length).map(|e| (e.name.clone(), t - e.t.value, e.length, e.path.clone())).collect();
    let head = rig.joint_part(Joint::Head).map(|i| placed_part(i));
    Solved {
        unreachable,
        parts: order.into_iter().map(|i| (i, placed_part(i), opacity[i])).collect(),
        pose,
        viseme,
        open,
        expression,
        costume,
        blinking,
        offset,
        facing,
        emotes,
        head,
        variation,
    }
}

/// The solved character as items under `placed` (the layer's transform),
/// named `<path>.<part>.<k>`, its emotes last.
pub fn items(rig: &Rig, solved: &Solved, path: &str, placed: &Affine) -> Vec<Item> {
    let mut out = Vec::new();
    let v = &solved.variation;
    let color = |c: Option<Color>| c.map(|c| v.color(c));
    for (index, map, opacity) in &solved.parts {
        let part = &rig.parts[*index];
        let map = placed.then(map);
        for (k, shape) in part.shapes.iter().enumerate() {
            let (fill, stroke) = (color(shape.fill), color(shape.stroke));
            let (world, kind) = match &shape.geom {
                Geom::Path(path) => (map, ItemKind::Path { path: path.clone(), fill, stroke, width: shape.width, trim: [0.0, 1.0], cap: shape.cap, join: shape.join }),
                Geom::Ellipse { center, radius } => (
                    map.then(&Affine::translate(center[0] - radius[0], center[1] - radius[1])),
                    ItemKind::Ellipse { size: [radius[0] * 2.0, radius[1] * 2.0], fill, stroke, width: shape.width, trim: [0.0, 1.0], cap: shape.cap },
                ),
                Geom::Rect { pos, size, radius } => (
                    map.then(&Affine::translate(pos[0], pos[1])),
                    ItemKind::Rect { size: *size, radius: radius.min(size[0].min(size[1]) * 0.5), fill, stroke, width: shape.width, trim: [0.0, 1.0], cap: shape.cap },
                ),
                Geom::Capsule { a, b, width } => {
                    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                    let length = (dx * dx + dy * dy).sqrt();
                    // Along +x from `a`, turned onto a→b.
                    let angle = dy.atan2(dx).to_degrees();
                    let r = width * 0.5;
                    (
                        map.then(&Affine::translate(a[0], a[1])).then(&Affine::rotate(angle)).then(&Affine::translate(-r, -r)),
                        ItemKind::Rect { size: [length + *width, *width], radius: r, fill, stroke, width: shape.width, trim: [0.0, 1.0], cap: shape.cap },
                    )
                }
            };
            out.push(Item {
                path: format!("{path}.{}.{k}", part.name),
                world,
                local: Local { anchor: [0.0, 0.0], scale: [1.0, 1.0], rotate: 0.0 },
                opacity: *opacity,
                blur: 0.0,
                feather: 0.0,
                feather_side: Default::default(),
                effects: Vec::new(),
                blend: Blend::Normal,
                is_static: false,
                kind,
            });
        }
    }
    if let (Some(head), Some(at)) = (&solved.head, rig.joint_part(Joint::Head)) {
        if let Some(hb) = shapes_bounds(&rig.parts[at].shapes) {
            let head = placed.then(head);
            for (name, u, length, geometry) in &solved.emotes {
                out.extend(emote::items(name, geometry, *u, *length, &head, hb, rig.ink, path, 1.0));
            }
        }
    }
    out
}

impl crate::model::MotionDoc {
    /// The library SVGs of `Character{src: asset(…)}` layers not built yet:
    /// what a host fetches before [`Self::resolve_characters`].
    pub fn character_models(&self) -> Vec<AssetRef> {
        let mut out: Vec<AssetRef> = Vec::new();
        for layer in self.all_layers() {
            if let crate::model::LayerKind::Character(ch) = &layer.kind {
                if let Model::Asset(asset) = &ch.model {
                    if !out.contains(asset) {
                        out.push(asset.clone());
                    }
                }
            }
        }
        out
    }

    /// Give every `Character{src: asset(…)}` its SVG model: `fetch` returns
    /// the markup of a library revision (the host reads it from the
    /// library). Returns a message per model that could not be built.
    pub fn resolve_characters(&mut self, fetch: &dyn Fn(&AssetRef) -> Option<String>) -> Vec<String> {
        fn walk(layers: &mut [crate::model::Layer], fetch: &dyn Fn(&AssetRef) -> Option<String>, out: &mut Vec<String>) {
            use crate::model::LayerKind;
            for layer in layers {
                match &mut layer.kind {
                    LayerKind::Character(ch) => {
                        if let Model::Asset(asset) = &ch.model {
                            match fetch(asset) {
                                Some(markup) => match svg::from_svg(&markup) {
                                    Ok((rig, _)) => {
                                        for (joint, _) in &ch.reach {
                                            if rig.reach_chain(*joint).is_none() {
                                                out.push(format!("{}.reach.{}: expected connected upper limb, lower limb and end joints in the model", layer.path, joint.name()));
                                            }
                                        }
                                        ch.model = Model::Rig(Arc::new(rig));
                                    }
                                    Err(message) => out.push(format!("{}: {message}", layer.path)),
                                },
                                None => out.push(format!("{}: the library has no {}:{}", layer.path, asset.id, asset.rev)),
                            }
                        }
                    }
                    LayerKind::Group { layers, mask, .. } => {
                        walk(layers, fetch, out);
                        if let Some(mask) = mask {
                            walk(std::slice::from_mut(mask.as_mut()), fetch, out);
                        }
                    }
                    LayerKind::Repeat { layer, items, .. } => {
                        walk(std::slice::from_mut(layer.as_mut()), fetch, out);
                        walk(items, fetch, out);
                    }
                    LayerKind::Bones(rig) => walk(&mut rig.layers, fetch, out),
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&mut self.layers, fetch, &mut out);
        out
    }
}

/// The Character kit in words: what `makepad-motion characters` prints and
/// the motion skill lists.
pub fn catalog() -> String {
    let mut out = String::new();
    let mut section = |title: &str, rows: &[(&str, &str)]| {
        out.push_str(title);
        out.push('\n');
        for (name, what) in rows {
            out.push_str(&format!("  @{name}: {what}\n"));
        }
    };
    section("kit characters (model: @name)", kit::KITS);
    section("poses (pose: @name or [[time, @name, blend]])", pose::POSES);
    section("expressions (expression: …)", face::EXPRESSIONS);
    section("costumes (costume: [[time, @name, @name2]])", kit::COSTUMES);
    section("emotes (emote: [[time, @name, seconds]])", emote::EMOTES);
    let visemes: Vec<(&str, &str)> = lipsync::Viseme::ALL.iter().map(|v| (v.name(), "")).collect();
    out.push_str("visemes (mouth_<name> parts; lip_sync picks them): ");
    out.push_str(&visemes.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(" "));
    out.push('\n');
    out.push_str("look fields (look: {…}):\n");
    for (name, what) in kit::LOOK_FIELDS {
        out.push_str(&format!("  {name}: {what}\n"));
    }
    out
}

/// A kit character with look changes (`top=#3c78d8`, `head=@square`,
/// `accessories=@cap,@glasses`, `legs=1.2`) as a character SVG.
pub fn kit_svg(name: &str, changes: &[String]) -> Result<String, String> {
    let mut look = kit::look(name).ok_or_else(|| format!("no kit character `{name}`; the kit: {}", kit::KITS.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(" ")))?;
    for change in changes {
        let (key, value) = change.split_once('=').ok_or_else(|| format!("`{change}`: write key=value"))?;
        let value = if let Some(hex) = value.strip_prefix('#') {
            crate::load::parse_hex(&format!("#{hex}")).map(kit::LookValue::Color).ok_or_else(|| format!("`{value}` is not a colour"))?
        } else if let Ok(n) = value.parse::<f64>() {
            kit::LookValue::Num(n)
        } else if value.contains(',') {
            kit::LookValue::Ids(value.split(',').map(|v| v.trim().trim_start_matches('@').to_string()).collect())
        } else {
            kit::LookValue::Id(value.trim_start_matches('@').to_string())
        };
        look.set(key, &value)?;
    }
    Ok(svg::to_svg(&kit::build(name, &look)))
}

/// One crowd member drawn as a varied copy of a character (a `Crowd` with
/// `member: Character{…}`): its feet at `at` in the crowd's coordinates,
/// `scale` times the character's own height, copy `index` (varied when the
/// character `vary`s), with the crowd's reactions (`arms` 0..1, `hop` up,
/// `nod` head degrees). Returns the items and any unreachable end joints.
#[allow(clippy::too_many_arguments)]
pub fn member_items(layer: &CharacterLayer, index: usize, at: [f64; 2], scale: f64, t: f64, beat: &dyn Fn(f64) -> f64, reach: &[(Joint, [f64; 2])], arms: f64, hop: f64, nod: f64, pose: Option<Pose>, path: &str, placed: &Affine) -> (Vec<Item>, Vec<Joint>) {
    let Some(rig) = layer.rig() else { return (Vec::new(), Vec::new()) };
    let m = Moment { t, beat, voice: None, host: 0.0, fps: 30.0, parts: &[], reach, pose, instance: index, arms, hop, nod };
    let solved = solve(layer, rig, &m);
    let placed = placed.then(&Affine::translate(at[0], at[1])).then(&Affine::scale(scale, scale));
    (items(rig, &solved, &format!("{path}#{index}"), &placed), solved.unreachable)
}
