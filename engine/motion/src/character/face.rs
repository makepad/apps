//! The face: expressions swapped by cue, blinks on a seeded schedule, and
//! which variant of each slot (eyes, brows, mouth) a moment shows.
//!
//! A slot's variants are parts named `<slot>_<variant>`: `eyes_open`,
//! `eyes_closed`, `eyes_surprised`, `brows_angry`, `mouth_smile`,
//! `mouth_AHA`. An expression picks `eyes_<expression>`,
//! `brows_<expression>` and (while the mouth rests) `mouth_<expression>`,
//! falling back to `eyes_open`, `brows_neutral` and `mouth_REST`.

use super::lipsync::Viseme;
use super::pose::{Joint, Pose};
use super::{Cue, Rig, Slot};

/// The expressions the kit characters have (a custom model may add its own).
pub const EXPRESSIONS: &[(&str, &str)] = &[
    ("neutral", "eyes open, mouth at rest"),
    ("deadpan", "half-lidded, flat mouth: dry, unimpressed"),
    ("smile", "a warm smile"),
    ("surprised", "wide eyes, raised brows, small O mouth"),
    ("won", "eyes shut in joy, big grin"),
    ("angry", "brows down, frown"),
    ("sly", "one brow up, side glance, smirk"),
];

/// The expression the cues give at `t` (swaps snap, like a cut-out's).
pub fn expression_at(cues: &[Cue], t: f64) -> String {
    let mut best: Option<&Cue> = None;
    for cue in cues {
        if cue.t.value <= t && best.is_none_or(|b| cue.t.value >= b.t.value) {
            best = Some(cue);
        }
    }
    best.or_else(|| cues.iter().min_by(|a, b| a.t.value.partial_cmp(&b.t.value).unwrap_or(std::cmp::Ordering::Equal)))
        .map_or_else(|| "neutral".to_string(), |cue| cue.name.clone())
}

fn hash01(seed: u32, k: u64, salt: u64) -> f64 {
    let mut h = (seed as u64 ^ 0xC2B2_AE3D_27D4_EB4F).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ k.wrapping_mul(0xBF58_476D_1CE4_E5B9) ^ salt.wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^= h >> 31;
    h = h.wrapping_mul(0xD6E8_FEB8_6659_FD93);
    h ^= h >> 32;
    h as f64 / u64::MAX as f64
}

/// How long the eyes stay shut in a blink, seconds.
pub const BLINK_SECONDS: f64 = 0.13;

/// The blinks' start times up to `until`: about every `interval` seconds
/// (0.55× to 1.45× of it), sometimes a double blink, the same for the same
/// seed.
pub fn blinks(until: f64, interval: f64, seed: u32) -> Vec<f64> {
    let mut out = Vec::new();
    if !(interval > 0.0) {
        return out;
    }
    let mut at = interval * (0.25 + 0.6 * hash01(seed, 0, 1));
    let mut k = 0u64;
    while at <= until {
        out.push(at);
        if hash01(seed, k, 2) > 0.82 {
            out.push(at + 0.26);
        }
        k += 1;
        at += interval * (0.55 + 0.9 * hash01(seed, k, 3));
    }
    out
}

pub fn blinking(t: f64, interval: f64, seed: u32) -> bool {
    blinks(t, interval, seed).iter().any(|&b| t >= b && t < b + BLINK_SECONDS)
}

/// The variant a slot shows.
fn pick<'a>(variants: &[&'a str], wanted: &[&str]) -> Option<&'a str> {
    for want in wanted {
        if let Some(v) = variants.iter().find(|v| v.eq_ignore_ascii_case(want)) {
            return Some(v);
        }
    }
    variants.first().copied()
}

/// Which parts show at this moment: the slots' chosen variants, the mic
/// and prop when the pose holds them, and nothing under a hidden part.
/// `costume` is the worn costume variants joined by `+` (`crown+mask`).
pub fn shown(rig: &Rig, expression: &str, costume: &str, blinking: bool, viseme: Viseme, pose: &Pose) -> Vec<bool> {
    let mouths = rig.variants(Slot::Mouth);
    let eyes = rig.variants(Slot::Eyes);
    let brows = rig.variants(Slot::Brows);
    let mouth = if viseme == Viseme::Rest {
        pick(&mouths, &[expression, "REST"])
    } else {
        let chain: Vec<&str> = viseme.fallbacks().iter().map(|v| v.name()).collect();
        pick(&mouths, &chain)
    };
    let eye = if blinking { pick(&eyes, &["closed", expression, "open"]) } else { pick(&eyes, &[expression, "open"]) };
    let brow = pick(&brows, &[expression, "neutral"]);
    let own: Vec<bool> = rig.parts.iter().map(|part| match &part.variant {
        Some((Slot::Mouth, v)) => Some(v.as_str()) == mouth,
        Some((Slot::Eyes, v)) => Some(v.as_str()) == eye,
        Some((Slot::Brows, v)) => Some(v.as_str()) == brow,
        Some((Slot::Costume, v)) => costume.split('+').any(|c| c.trim() == v),
        None if part.name == "mic" => pose.mic > 0.5,
        None if part.name == "prop" || part.name.starts_with("prop_") => pose.prop > 0.5,
        None => true,
    }).collect();
    (0..rig.parts.len()).map(|i| {
        let mut at = Some(i);
        while let Some(p) = at {
            if !own[p] {
                return false;
            }
            at = rig.parts[p].parent;
        }
        true
    }).collect()
}

/// A part's draw order under a pose: the right arm (and what it holds)
/// comes in front of the head when the pose brings it there.
pub fn z_of(rig: &Rig, index: usize, pose: &Pose) -> f64 {
    let part = &rig.parts[index];
    if pose.front_arm > 0.5 {
        let mut at = Some(index);
        while let Some(p) = at {
            if rig.parts[p].joint == Some(Joint::ArmRU) {
                return part.z + 30.0;
            }
            at = rig.parts[p].parent;
        }
    }
    part.z
}
