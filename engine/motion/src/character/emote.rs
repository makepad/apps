//! Emotes: little cartoon effects attached to a character's head for a
//! while (`emote: [[beat(4), @sweat, 2.0]]`): sweat drops, dizzy stars,
//! sparkles, an anger mark, hearts. They ride the head (a nod carries
//! them) and are drawn over the character.

use crate::frame::{Affine, Item, ItemKind, Local};
use crate::geometry::{parse_path, VectorPath};
use crate::model::{Blend, Cap, Color, Join};
use std::sync::Arc;

pub const EMOTES: &[(&str, &str)] = &[
    ("sweat", "sweat drops sliding off the temple (nervous, busted)"),
    ("dizzy", "stars circling over the head (knocked out, confused)"),
    ("sparkle", "sparkles popping around the head (proud, fresh, won)"),
    ("anger", "a throbbing anger mark on the temple"),
    ("hearts", "hearts floating up (love, delight)"),
];

pub fn is_emote(name: &str) -> bool {
    EMOTES.iter().any(|(n, _)| *n == name)
}

fn hash01(k: u64) -> f64 {
    let mut h = k.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xBF58_476D_1CE4_E5B9;
    h ^= h >> 31;
    h = h.wrapping_mul(0xD6E8_FEB8_6659_FD93);
    h ^= h >> 32;
    h as f64 / u64::MAX as f64
}

/// The emote's shared outline, built when its cue is loaded.
pub fn path(name: &str) -> VectorPath {
    let data = match name {
        "sweat" => "M 0 -1.6 C 0.5 -0.9 1 -0.2 1 0.35 C 1 0.95 0.5 1.3 0 1.3 C -0.5 1.3 -1 0.95 -1 0.35 C -1 -0.2 -0.5 -0.9 0 -1.6 Z",
        "hearts" => "M 0 1 C -0.4 0.65 -1 0.25 -1 -0.25 C -1 -0.7 -0.5 -0.95 0 -0.5 C 0.5 -0.95 1 -0.7 1 -0.25 C 1 0.25 0.4 0.65 0 1 Z",
        "sparkle" => "M 0 -1 Q 0.12 -0.12 1 0 Q 0.12 0.12 0 1 Q -0.12 0.12 -1 0 Q -0.12 -0.12 0 -1 Z",
        "anger" => "M -1 -0.3 Q -0.35 -0.35 -0.3 -1 M 0.3 -1 Q 0.35 -0.35 1 -0.3 M 1 0.3 Q 0.35 0.35 0.3 1 M -0.3 1 Q -0.35 0.35 -1 0.3",
        "dizzy" => return star(),
        _ => "",
    };
    parse_path(data)
}

fn star() -> VectorPath {
    let mut path = VectorPath::new();
    for k in 0..10 {
        let a = (-90.0 + 36.0 * k as f64).to_radians();
        let r = if k % 2 == 0 { 1.0 } else { 0.45 };
        let x = ((a.cos() * r * 1000.0).round() / 1000.0) as f32;
        let y = ((a.sin() * r * 1000.0).round() / 1000.0) as f32;
        if k == 0 { path.move_to(x, y) } else { path.line_to(x, y) }
    }
    path.close();
    path
}

/// An emote's items at `u` seconds into it (of `length`), placed by `head`
/// (the head part's model-to-document map) about the head's rest bounds
/// `hb` (model units), outlined in `ink`.
#[allow(clippy::too_many_arguments)]
pub fn items(name: &str, geometry: &Arc<VectorPath>, u: f64, length: f64, head: &Affine, hb: [f64; 4], ink: Color, path: &str, opacity: f64) -> Vec<Item> {
    let (w, h) = (hb[2] - hb[0], hb[3] - hb[1]);
    let cx = (hb[0] + hb[2]) * 0.5;
    // In over 0.15 s, out over the last 0.25 s.
    let fade = (u / 0.15).clamp(0.0, 1.0) * ((length - u) / 0.25).clamp(0.0, 1.0) * opacity;
    let mut out = Vec::new();
    let mut push = |k: usize, at: [f64; 2], size: f64, turn: f64, fill: Option<Color>, stroke: Option<Color>, width: f64, alpha: f64| {
        if alpha <= 0.0 || size <= 0.0 {
            return;
        }
        let world = head.then(&Affine::translate(at[0], at[1])).then(&Affine::rotate(turn)).then(&Affine::scale(size, size));
        out.push(Item {
            path: format!("{path}.emote_{name}.{k}"),
            world,
            local: Local { anchor: [0.0, 0.0], scale: [1.0, 1.0], rotate: 0.0 },
            opacity: alpha * fade,
            blur: 0.0,
            feather: 0.0,
            feather_side: Default::default(),
            effects: Vec::new(),
            blend: Blend::Normal,
            is_static: false,
            kind: ItemKind::Path { path: geometry.clone(), fill, stroke, width: width / size, trim: [0.0, 1.0], cap: Cap::Round, join: Join::Round },
        });
    };
    let line = 0.018 * w;
    match name {
        "sweat" => {
            for k in 0..2 {
                let p = (u / 0.9 + k as f64 * 0.5).fract();
                let at = [hb[2] - 0.12 * w + 0.03 * w * k as f64, hb[1] + 0.25 * h + p * 0.3 * h];
                push(k, at, 0.085 * w * (0.8 + 0.2 * k as f64), 0.0, Some([0.56, 0.83, 1.0, 1.0]), Some(ink), line, 1.0 - p * p);
            }
        }
        "dizzy" => {
            for k in 0..3 {
                let a = std::f64::consts::TAU * (u / 1.1 + k as f64 / 3.0);
                let at = [cx + a.cos() * 0.46 * w, hb[1] - 0.02 * h + a.sin() * 0.1 * h];
                push(k, at, 0.11 * w * (0.85 + 0.15 * a.sin()), u * 200.0, Some([1.0, 0.88, 0.3, 1.0]), Some(ink), line, 1.0);
            }
        }
        "sparkle" => {
            for k in 0..4 {
                let p = (u / 0.8 + k as f64 * 0.25).fract();
                let cycle = (u / 0.8 + k as f64 * 0.25).floor() as u64;
                let seed = k as u64 * 1000 + cycle;
                let a = std::f64::consts::TAU * hash01(seed);
                let r = 0.55 + 0.2 * hash01(seed + 7);
                let at = [cx + a.cos() * r * w, (hb[1] + hb[3]) * 0.5 + a.sin() * r * h];
                push(k, at, 0.12 * w * (std::f64::consts::PI * p).sin(), 0.0, Some([1.0, 0.97, 0.75, 1.0]), None, 0.0, 1.0);
            }
        }
        "anger" => {
            let pulse = 1.0 + 0.15 * (std::f64::consts::TAU * u * 3.0).sin().max(0.0);
            push(0, [hb[2] - 0.15 * w, hb[1] + 0.12 * h], 0.13 * w * pulse, 10.0, None, Some([0.9, 0.18, 0.16, 1.0]), 0.035 * w, 1.0);
        }
        "hearts" => {
            for k in 0..3 {
                let p = (u / 1.4 + k as f64 / 3.0).fract();
                let at = [cx + (k as f64 - 1.0) * 0.3 * w + 0.05 * w * (p * 9.0).sin(), hb[1] - p * 0.5 * h];
                push(k, at, 0.1 * w * (0.6 + 0.6 * p.min(0.3) / 0.3), 0.0, Some([1.0, 0.36, 0.5, 1.0]), Some(ink), line, 1.0 - p * p);
            }
        }
        _ => {}
    }
    out
}
