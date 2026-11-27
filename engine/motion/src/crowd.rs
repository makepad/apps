//! `Crowd{…}`: rows of seeded silhouettes that react to the music, drawn
//! as plain Rect / Ellipse items (distance-field quads, batched into one
//! draw), so hundreds of members cost a few thousand instanced quads and
//! no tessellation. Every member is a pure function of its index, the
//! seed and the time: locked-time renders are exact.

use crate::plain::PrimitiveFrame;
use crate::frame::{Affine, Item, ItemKind, Local, TextUnit};
use crate::model::{Align, Blend, Cap, Color, Crowd, TextBy, VAlign};

/// A member's seeded number in 0..1 (`k` picks which).
pub fn hash01(seed: u32, i: usize, k: u32) -> f64 {
    let mut h = (seed as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (i as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9) ^ (k as u64).wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^= h >> 31;
    h = h.wrapping_mul(0xD6E8_FEB8_6659_FD93);
    h ^= h >> 32;
    h as f64 / u64::MAX as f64
}

/// What a crowd shows at one moment (its props sampled by the caller).
pub struct Moment<'a> {
    pub crowd: &'a Crowd,
    pub color: Color,
    pub color2: Option<Color>,
    pub outline: Option<Color>,
    pub bob: f64,
    pub hop: f64,
    pub nod: f64,
    /// The `react` feature's level now (0..1), or `None` for @beat.
    pub level: Option<f64>,
    /// `react: @none`.
    pub still: bool,
    /// The `cheer` feature's level now (0..1).
    pub cheer: f64,
    /// Sample `(arms, sign_up)` at a member's delay and its Character
    /// reaches at layer time, plus its value-driven pose. The caller reuses the reach buffer.
    pub late: &'a mut dyn FnMut(f64, usize, &mut Vec<(crate::character::pose::Joint, [f64; 2])>) -> (f64, f64, Option<crate::character::pose::Pose>),
    /// Member index and unreachable end joint, reported by frame evaluation.
    pub unreachable: &'a mut Vec<(usize, crate::character::pose::Joint)>,
    /// Beats since the start (fractional), and beats per second.
    pub beat: f64,
    pub beats_per_second: f64,
    /// The layer's time and `start` (the crowd pops up from below then).
    pub t: f64,
    pub start: f64,
}

fn shape(path: String, world: Affine, rotate: f64, anchor: [f64; 2], opacity: f64, kind: ItemKind) -> Item {
    Item {
        path,
        world,
        local: Local { anchor, scale: [1.0, 1.0], rotate },
        opacity,
        blur: 0.0,
        feather: 0.0,
        feather_side: Default::default(),
        effects: Vec::new(),
        blend: Blend::Normal,
        is_static: false,
        kind,
    }
}

fn rect(size: [f64; 2], radius: f64, fill: Color, stroke: Option<Color>, width: f64) -> ItemKind {
    ItemKind::Primitive(PrimitiveFrame::Rect { size, radius: radius.min(size[0].min(size[1]) * 0.5), fill: Some(fill), stroke, width, trim: [0.0, 1.0], cap: Cap::Butt })
}

fn ellipse(size: [f64; 2], fill: Color, stroke: Option<Color>, width: f64) -> ItemKind {
    ItemKind::Primitive(PrimitiveFrame::Ellipse { size, fill: Some(fill), stroke, width, trim: [0.0, 1.0], cap: Cap::Butt })
}

/// The crowd's items, back row first, placed by `placed` (the layer's
/// world and local transform).
pub fn items(path: &str, placed: &Affine, m: &mut Moment) -> Vec<Item> {
    let c = m.crowd;
    let rows = c.rows.max(1);
    let [w, h] = c.size;
    let front_h = h * 0.62;
    let mut out = Vec::with_capacity(c.count * 6);
    let mut index = 0usize;
    let mut reach = Vec::new();
    let pulse = |p: f64| (1.0 - p.fract()).powi(3);
    for r in 0..rows {
        // Members in this row: the front rows take the remainder.
        let n = c.count / rows + usize::from(rows - 1 - r < c.count % rows);
        if n == 0 {
            continue;
        }
        let depth = if rows > 1 { r as f64 / (rows - 1) as f64 } else { 1.0 };
        let hh = front_h * (0.6 + 0.4 * depth);
        let base = h * (0.42 + 0.58 * (r + 1) as f64 / rows as f64);
        let spacing = w / n as f64;
        let shift = if r % 2 == 1 { spacing * 0.5 } else { 0.0 };
        let glow = 0.5 + 0.5 * depth;
        for k in 0..n {
            let i = index;
            index += 1;
            let rnd = |q: u32| hash01(c.seed, i, q);
            // A variant's shape: of `variants` shared ones, or its own.
            let key = match c.variants {
                Some(n) => (rnd(20) * n as f64).floor() as usize,
                None => i,
            };
            let shape_rnd = |q: u32| hash01(c.seed, key, q);
            let x = ((k as f64 + 0.5) * spacing + shift + (rnd(1) - 0.5) * spacing * 0.5).rem_euclid(w);
            let s = hh * (0.88 + 0.24 * rnd(2));
            let eye = if x < w * 0.5 { c.eyes[0] } else { c.eyes[1] };
            let body_color = match m.color2 {
                Some(c2) if shape_rnd(21) < 0.5 => c2,
                _ => m.color,
            };
            // Popping up from below at `start`, each a seeded moment late.
            let up = ((m.t - m.start - rnd(22) * 0.5) / 0.35).clamp(0.0, 1.0);
            if up <= 0.0 {
                continue;
            }
            let rise = (1.0 - crate::Ease::Out(crate::Curve::Back).apply(up, 1.0)) * s * 1.2;
            let eye = [eye[0], eye[1], eye[2], eye[3] * glow];
            // The build: head, shoulders, body.
            let head_w = s * (0.26 + 0.1 * shape_rnd(3));
            let head_h = head_w * (0.85 + 0.25 * shape_rnd(4));
            let boxy = shape_rnd(5) < 0.45;
            let body_w = s * (0.5 + 0.22 * shape_rnd(6));
            let body_h = s * 0.58;
            let shoulder = body_w * (0.22 + 0.25 * shape_rnd(7));
            // The beat: a bob with the member's own phase, a nod on the beat.
            // `stagger` seconds of phase spread (a full bob cycle when unset).
            let phase = match c.stagger {
                Some(secs) => rnd(8) * secs * m.beats_per_second * 0.5,
                None => rnd(8),
            };
            let bob = -((m.beat * 0.5 + phase) * std::f64::consts::TAU).sin().abs() * m.bob * s;
            // Hop and nod: on each beat, or with the feature's level.
            let (kick, bounce) = match (m.still, m.level) {
                (true, _) => (0.0, 0.0),
                (false, None) => {
                    let p = pulse(m.beat + (rnd(9) - 0.5) * 0.08);
                    (p, p)
                }
                (false, Some(level)) => (level, level * ((m.beat + rnd(9) * 0.2) * std::f64::consts::PI).sin().abs()),
            };
            let hop = -m.hop * s * bounce;
            let dip = m.nod * s * 0.07 * kick;
            let (arms, signs, pose) = (m.late)(rnd(10) * 0.35, i, &mut reach);
            let arms = arms.max(m.cheer);
            let holds_sign = rnd(11) < c.signs;
            let arena = if c.arena { -((x / w - 0.5) * 2.0).powi(2) * h * 0.12 * (1.0 - depth * 0.5) } else { 0.0 };
            let feet = placed.then(&Affine::translate(x, base + bob + hop + rise + arena));
            // A character crowd (lane CH1): the member is a varied copy of
            // it, at its feet and height, arms and nod from the crowd.
            if let Some(member) = &c.member {
                let (beat, per_second, now) = (m.beat, m.beats_per_second, m.t);
                let beat_at = move |u: f64| beat + (u - now) * per_second;
                let nod = if s > 0.0 { dip / (s * 0.07) * 8.0 } else { 0.0 };
                let (items, unreachable) = crate::character::member_items(member, i, [0.0, 0.0], s * 1.15 / member.height.max(1e-6), m.t, &beat_at, &reach, arms, 0.0, nod, pose, path, &feet);
                m.unreachable.extend(unreachable.into_iter().map(|joint| (i, joint)));
                out.extend(items);
                continue;
            }
            let id = |part: &str| format!("{path}#{i}.{part}");
            let width = s * 0.025;
            // Arms behind the body, raised by `arms`, pumping on the beat.
            if arms > 0.01 && !(holds_sign && signs > 0.01) {
                let aw = body_w * 0.16;
                let len = s * 0.46;
                let up = arms * (158.0 + 10.0 * pulse(m.beat)) - 6.0 * rnd(12);
                for (side, sign) in [(-1.0, 1.0), (1.0, -1.0)] {
                    let sx = side * (body_w * 0.5 - aw * 0.7);
                    let at = feet.then(&Affine::translate(sx - aw * 0.5, -body_h + aw * 0.3));
                    out.push(shape(id(if side < 0.0 { "arm_l" } else { "arm_r" }), at, sign * up, [0.5, 0.0], 1.0, rect([aw, len], aw * 0.5, body_color, m.outline, width)));
                }
            }
            // The sign, on a pole, rising behind the head.
            if holds_sign && signs > 0.01 {
                let e = signs.clamp(0.0, 1.0);
                let (cw, ch) = (s * 0.64, s * 0.27);
                let lift = (1.0 - e) * s * 0.5;
                let top = -body_h - head_h - s * 0.3 - ch + lift - dip;
                let pole = feet.then(&Affine::translate(-s * 0.02, top + ch));
                out.push(shape(id("pole"), pole, 0.0, [0.5, 0.5], e, rect([s * 0.04, -top - ch - body_h * 0.5], 0.0, body_color, m.outline, width)));
                let card = feet.then(&Affine::translate(-cw * 0.5, top));
                let tilt = (rnd(13) - 0.5) * 12.0;
                out.push(shape(id("sign"), card, tilt, [0.5, 1.0], e, rect([cw, ch], s * 0.03, c.sign_color, Some(c.sign_ink), width)));
                let label = feet.then(&Affine::translate(0.0, top + ch * 0.5));
                let size = ch * 0.5;
                out.push(shape(id("sign_text"), label, tilt, [0.5, 0.5], e, ItemKind::Primitive(PrimitiveFrame::Text {
                    text: c.sign.clone(),
                    font: "Anton".into(),
                    weight: 400.0,
                    axes: Vec::new(),
                    italic: false,
                    size,
                    align: Align::Center,
                    valign: VAlign::Center,
                    line_height: 1.0,
                    tracking: 0.0,
                    wrap: None,
                    fit: Some(cw * 0.86),
                    path: None,
                    by: TextBy::Whole,
                    units: vec![TextUnit { range: 0..c.sign.len(), offset: [0.0, 0.0], scale: [1.0, 1.0], rotate: 0.0, opacity: 1.0, color: c.sign_ink, blur: 0.0, axes: Vec::new() }],
                    plate: None,
                    reveal: None,
                    caret: None,
                    outline: None,
                })));
            }
            // Body (running off below the row) and head.
            let body = feet.then(&Affine::translate(-body_w * 0.5, -body_h));
            out.push(shape(id("body"), body, 0.0, [0.5, 0.5], 1.0, rect([body_w, body_h + s * 0.4], shoulder, body_color, m.outline, width)));
            let head_y = -body_h - head_h * 0.9 + dip;
            let head = feet.then(&Affine::translate(-head_w * 0.5, head_y));
            let head_kind = if boxy { rect([head_w, head_h], head_w * 0.18, body_color, m.outline, width) } else { ellipse([head_w, head_h], body_color, m.outline, width) };
            // Antenna or ears, before the head so the head covers the joint.
            if shape_rnd(14) < 0.35 {
                let aw = head_w * 0.07;
                let ah = head_h * 0.45;
                out.push(shape(id("antenna"), feet.then(&Affine::translate(-aw * 0.5, head_y - ah + head_h * 0.1)), 0.0, [0.5, 0.5], 1.0, rect([aw, ah], 0.0, body_color, m.outline, width)));
                let tip = head_w * 0.16;
                out.push(shape(id("tip"), feet.then(&Affine::translate(-tip * 0.5, head_y - ah - tip * 0.3)), 0.0, [0.5, 0.5], 1.0, ellipse([tip, tip], eye, None, 0.0)));
            } else if boxy && shape_rnd(15) < 0.5 {
                let ew = head_w * 0.12;
                for side in [-1.0, 1.0] {
                    let ex = side * (head_w * 0.5 + ew * 0.3) - ew * 0.5;
                    out.push(shape(id("ear"), feet.then(&Affine::translate(ex, head_y + head_h * 0.3)), 0.0, [0.5, 0.5], 1.0, rect([ew, head_h * 0.35], ew * 0.3, body_color, m.outline, width)));
                }
            }
            out.push(shape(id("head"), head, 0.0, [0.5, 0.5], 1.0, head_kind));
            // Eyes: two dots or a visor, in the side's colour.
            let ey = head_y + head_h * (0.42 + 0.1 * rnd(16));
            if shape_rnd(17) < 0.2 {
                let (vw, vh) = (head_w * 0.66, head_w * 0.13);
                out.push(shape(id("visor"), feet.then(&Affine::translate(-vw * 0.5, ey - vh * 0.5)), 0.0, [0.5, 0.5], 1.0, rect([vw, vh], vh * 0.5, eye, None, 0.0)));
            } else {
                let d = head_w * (0.12 + 0.05 * rnd(18));
                let gap = head_w * (0.17 + 0.06 * rnd(19));
                for side in [-1.0, 1.0] {
                    out.push(shape(id("eye"), feet.then(&Affine::translate(side * gap - d * 0.5, ey - d * 0.5)), 0.0, [0.5, 0.5], 1.0, ellipse([d, d], eye, None, 0.0)));
                }
            }
        }
    }
    out
}

/// The brightness of letter `i` of a neon tube at `t` seconds: 1 lit,
/// near 0 out. `amount` 0..1 is how often the tubes sputter; a failing
/// letter (`fails`) keeps dropping out in long, irregular stretches.
pub fn neon(i: usize, t: f64, amount: f64, fails: bool) -> f64 {
    if fails {
        // Stretches of about 0.1–0.5 s, dim more than a third of the time,
        // with rapid stutter at each change.
        let slot = (t * 6.0).floor();
        let h = hash01(0x6e65_6f6e, i, slot as i64 as u32);
        let stutter = hash01(0x7374_7574, i, (t * 40.0).floor() as i64 as u32);
        let edge = (t * 6.0).fract() < 0.18;
        return if h < 0.38 { 0.12 } else if edge && stutter < 0.5 { 0.35 } else { 1.0 };
    }
    if amount <= 0.0 {
        return 1.0;
    }
    // Brief dropouts at 20 Hz slots, more often with `amount`.
    let slot = (t * 20.0).floor();
    let h = hash01(0x666c_6b72, i, slot as i64 as u32);
    if h < amount * 0.06 { 0.25 } else { 1.0 }
}
