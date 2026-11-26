//! Poses and actions: the skeleton's joints, named pose presets blended
//! with eases, a walk locked to the beat (one step per beat), nods and bobs
//! on the beat, and secondary motion on a spring.
//!
//! Angles are degrees, clockwise on screen (y down), relative to the rest
//! pose the model is drawn in (arms and legs hanging straight down). The
//! model faces screen-right; `_l` and `_r` are the screen-left and
//! screen-right limbs as drawn (the left ones are the far side).

use super::Cue;

/// Apply hand/foot reaches to FK maps already in Character layer units.
/// Grounding happens before this step so a foot target remains exact.
/// Descendant geometry follows each rigid segment; the triangle solve is
/// shared with Bones, including its unreachable-target behavior.
pub fn reach(rig: &super::Rig, world: &mut [Option<crate::Affine>], targets: &[(Joint, [f64; 2])]) -> Vec<Joint> {
    use crate::rig::{angle, turn, two_bone, Bend};
    use crate::Affine;
    let mut unreachable = Vec::new();
    for &(end, target) in targets {
        let Some([a, b, c]) = rig.reach_chain(end) else { continue };
        let point = |i: usize, world: &[Option<Affine>]| world[i].unwrap().apply(rig.parts[i].pivot);
        let (base, elbow, tip) = (point(a, world), point(b, world), point(c, world));
        let v = [elbow[0] - base[0], elbow[1] - base[1]];
        let w = [tip[0] - elbow[0], tip[1] - elbow[1]];
        let cross = v[0] * w[1] - v[1] * w[0];
        let bend = if cross > 1e-9 { Bend::Cw } else if cross < -1e-9 { Bend::Ccw }
            else if matches!(end, Joint::HandL | Joint::FootR) { Bend::Cw } else { Bend::Ccw };
        let solved = two_bone(base, [v[0].hypot(v[1]), w[0].hypot(w[1])], target, bend);
        if solved.unreachable { unreachable.push(end); }
        let move_branch = |root: usize, map: Affine, world: &mut [Option<Affine>]| {
            for i in 0..world.len() {
                let mut ancestor = Some(i);
                while let Some(j) = ancestor {
                    if j == root { world[i] = Some(map.then(&world[i].unwrap())); break; }
                    ancestor = rig.parts[j].parent;
                }
            }
        };
        move_branch(a, Affine::about(base, &Affine::rotate(turn(angle(base, elbow), angle(base, solved.joint)))), world);
        let elbow = point(b, world);
        let tip = point(c, world);
        move_branch(b, Affine::about(elbow, &Affine::rotate(turn(angle(elbow, tip), angle(solved.joint, solved.end)))), world);
    }
    unreachable
}

/// The skeleton's joints. Each turns the part of the same name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Joint {
    Hips,
    Torso,
    Head,
    ArmLU,
    ArmLL,
    HandL,
    ArmRU,
    ArmRL,
    HandR,
    LegLU,
    LegLL,
    FootL,
    LegRU,
    LegRL,
    FootR,
}

pub const JOINTS: usize = 15;

impl Joint {
    pub const ALL: [Joint; JOINTS] = [
        Joint::Hips, Joint::Torso, Joint::Head, Joint::ArmLU, Joint::ArmLL, Joint::HandL, Joint::ArmRU, Joint::ArmRL, Joint::HandR,
        Joint::LegLU, Joint::LegLL, Joint::FootL, Joint::LegRU, Joint::LegRL, Joint::FootR,
    ];

    /// The part id in a model.
    pub fn name(self) -> &'static str {
        match self {
            Joint::Hips => "hips",
            Joint::Torso => "torso",
            Joint::Head => "head",
            Joint::ArmLU => "arm_l_upper",
            Joint::ArmLL => "arm_l_lower",
            Joint::HandL => "hand_l",
            Joint::ArmRU => "arm_r_upper",
            Joint::ArmRL => "arm_r_lower",
            Joint::HandR => "hand_r",
            Joint::LegLU => "leg_l_upper",
            Joint::LegLL => "leg_l_lower",
            Joint::FootL => "foot_l",
            Joint::LegRU => "leg_r_upper",
            Joint::LegRL => "leg_r_lower",
            Joint::FootR => "foot_r",
        }
    }

    pub fn from_name(name: &str) -> Option<Joint> {
        Joint::ALL.iter().copied().find(|j| j.name() == name)
    }

    pub fn parent(self) -> Option<Joint> {
        Some(match self {
            Joint::Hips => return None,
            Joint::Torso => Joint::Hips,
            Joint::Head => Joint::Torso,
            Joint::ArmLU | Joint::ArmRU => Joint::Torso,
            Joint::ArmLL => Joint::ArmLU,
            Joint::HandL => Joint::ArmLL,
            Joint::ArmRL => Joint::ArmRU,
            Joint::HandR => Joint::ArmRL,
            Joint::LegLU | Joint::LegRU => Joint::Hips,
            Joint::LegLL => Joint::LegLU,
            Joint::FootL => Joint::LegLL,
            Joint::LegRL => Joint::LegRU,
            Joint::FootR => Joint::LegRL,
        })
    }
}

/// The parent of a part that is not a joint, by its name, when the model
/// does not nest it: props in the right hand, headwear and face on the
/// head, pendants and cloth on the torso.
pub fn default_parent(name: &str) -> &'static str {
    const HEAD: &[&str] = &["costume", "hair", "hat", "cap", "beanie", "glasses", "ear", "antenna", "headphones", "headwear", "nose", "face", "mouth", "eyes", "brows", "cheek"];
    const TORSO: &[&str] = &["medallion", "chain", "cloth", "hood", "scarf", "tie", "necklace"];
    if name == "mic" || name == "prop" || name.starts_with("prop_") {
        "hand_r"
    } else if HEAD.iter().any(|p| name.starts_with(p)) {
        "head"
    } else if TORSO.iter().any(|p| name.starts_with(p)) {
        "torso"
    } else if name.starts_with("tail") {
        "hips"
    } else {
        "torso"
    }
}

/// Parts that sway on the spring (secondary motion), by name.
pub fn sways(name: &str) -> bool {
    ["hair", "cloth", "scarf", "tail", "antenna", "medallion", "chain", "pendant", "necklace", "sway"].iter().any(|p| name.starts_with(p))
}

/// Joint angles plus what a pose shows: the mic, the prop, and whether
/// the right arm comes in front of the head.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub angles: [f64; JOINTS],
    pub mic: f64,
    pub prop: f64,
    pub front_arm: f64,
}

impl Pose {
    fn of(pairs: &[(Joint, f64)]) -> Pose {
        let mut angles = [0.0; JOINTS];
        for (joint, angle) in pairs {
            angles[*joint as usize] = *angle;
        }
        Pose { angles, mic: 0.0, prop: 0.0, front_arm: 0.0 }
    }

    /// `self` blended `w` of the way to `other`.
    pub fn mix(&self, other: &Pose, w: f64) -> Pose {
        let w = w.clamp(0.0, 1.0);
        let mut out = *self;
        for k in 0..JOINTS {
            out.angles[k] = self.angles[k] + (other.angles[k] - self.angles[k]) * w;
        }
        out.mic = self.mic + (other.mic - self.mic) * w;
        out.prop = self.prop + (other.prop - self.prop) * w;
        out.front_arm = self.front_arm + (other.front_arm - self.front_arm) * w;
        out
    }

    pub fn angle(&self, joint: Joint) -> f64 {
        self.angles[joint as usize]
    }
}

/// The pose presets, as the model sheet shows them.
pub const POSES: &[(&str, &str)] = &[
    ("idle", "standing easy, arms loose"),
    ("hero", "hands on hips, chin up, feet apart"),
    ("walk", "the walk's base (a `walk` adds the steps)"),
    ("on_mic", "right hand holds the mic at the mouth"),
    ("point", "right arm points ahead"),
    ("arms_up", "both arms raised (a win, a cheer)"),
    ("cheer", "arms raised, hands turned out, knees bent in celebration"),
    ("sit", "seated (feet stay on the ground, the hips drop)"),
    ("hold_prop", "right hand holds the `prop` out"),
    ("shrug", "palms out, shoulders up"),
    ("float", "hovering: arms out, legs dangling (a `float` blends it in)"),
    ("fall", "falling: arms flailing up, legs apart"),
    ("wave", "right arm up, hand high (animate `parts: {arm_r_lower: {rotate: …}}` to wave)"),
];

pub fn preset(name: &str) -> Option<Pose> {
    use Joint::*;
    Some(match name {
        "idle" => Pose::of(&[(ArmLU, 7.0), (ArmLL, -6.0), (ArmRU, -7.0), (ArmRL, -8.0), (HandL, 4.0), (HandR, -4.0)]),
        "hero" => Pose::of(&[
            (Torso, -2.0), (Head, -5.0), (ArmLU, 38.0), (ArmLL, -100.0), (ArmRU, -38.0), (ArmRL, 100.0), (HandL, -20.0), (HandR, 20.0),
            (LegLU, 8.0), (FootL, -8.0), (LegRU, -8.0), (FootR, 8.0),
        ]),
        "walk" => Pose::of(&[(ArmLU, 3.0), (ArmRU, -3.0)]),
        "on_mic" => {
            let mut p = Pose::of(&[(Head, 4.0), (ArmLU, 14.0), (ArmLL, -32.0), (ArmRU, -22.0), (ArmRL, 150.0), (HandR, 8.0)]);
            p.mic = 1.0;
            p.front_arm = 1.0;
            p
        }
        "point" => Pose::of(&[(Torso, -2.0), (Head, -4.0), (ArmLU, 7.0), (ArmLL, -6.0), (ArmRU, -98.0), (ArmRL, -6.0), (HandR, -4.0)]),
        "arms_up" => Pose::of(&[(Head, -8.0), (ArmLU, 155.0), (ArmLL, 18.0), (ArmRU, -155.0), (ArmRL, -18.0), (LegLU, 5.0), (FootL, -5.0), (LegRU, -5.0), (FootR, 5.0)]),
        "cheer" => Pose::of(&[(Torso, -3.0), (Head, -10.0), (ArmLU, 150.0), (ArmLL, 30.0), (HandL, 15.0), (ArmRU, -150.0), (ArmRL, -30.0), (HandR, -15.0), (LegLU, -10.0), (LegLL, 20.0), (FootL, -10.0), (LegRU, -10.0), (LegRL, 20.0), (FootR, -10.0)]),
        "sit" => Pose::of(&[(Torso, -4.0), (ArmLU, -18.0), (ArmLL, -50.0), (ArmRU, -24.0), (ArmRL, -46.0), (LegLU, -84.0), (LegLL, 84.0), (LegRU, -88.0), (LegRL, 88.0)]),
        "hold_prop" => {
            let mut p = Pose::of(&[(ArmLU, 7.0), (ArmLL, -6.0), (ArmRU, -24.0), (ArmRL, -66.0), (HandR, 0.0)]);
            p.prop = 1.0;
            p
        }
        "shrug" => Pose::of(&[(Head, 6.0), (ArmLU, 12.0), (ArmLL, 100.0), (ArmRU, -12.0), (ArmRL, -100.0), (HandL, 20.0), (HandR, -20.0)]),
        "float" => Pose::of(&[(Head, -5.0), (ArmLU, 38.0), (ArmLL, 20.0), (ArmRU, -38.0), (ArmRL, -20.0), (LegLU, 10.0), (LegLL, 26.0), (FootL, 22.0), (LegRU, -6.0), (LegRL, 32.0), (FootR, 18.0)]),
        "fall" => Pose::of(&[(Torso, -8.0), (Head, -10.0), (ArmLU, 140.0), (ArmLL, 40.0), (ArmRU, -125.0), (ArmRL, -50.0), (LegLU, 25.0), (LegLL, 30.0), (LegRU, -22.0), (LegRL, 40.0)]),
        "wave" => Pose::of(&[(Head, -3.0), (ArmLU, 7.0), (ArmLL, -6.0), (ArmRU, -125.0), (ArmRL, -45.0), (HandR, -10.0)]),
        _ => return None,
    })
}

fn smooth(u: f64) -> f64 {
    let u = u.clamp(0.0, 1.0);
    u * u * (3.0 - 2.0 * u)
}

/// The pose the cues give at `t`: the latest cue's, blended in over its
/// `blend` from where the timeline was when it started.
pub fn cued(cues: &[Cue], t: f64) -> Pose {
    let idle = preset("idle").unwrap();
    let mut sorted: Vec<&Cue> = cues.iter().collect();
    sorted.sort_by(|a, b| a.t.value.partial_cmp(&b.t.value).unwrap_or(std::cmp::Ordering::Equal));
    fn at(cues: &[&Cue], t: f64, idle: &Pose) -> Pose {
        let Some(i) = cues.iter().rposition(|c| c.t.value <= t) else {
            return cues.first().and_then(|c| preset(&c.name)).unwrap_or(*idle);
        };
        let target = preset(&cues[i].name).unwrap_or(*idle);
        let blend = cues[i].blend;
        if i == 0 || blend <= 0.0 || t >= cues[i].t.value + blend {
            return target;
        }
        let from = at(&cues[..i], cues[i].t.value, idle);
        from.mix(&target, smooth((t - cues[i].t.value) / blend))
    }
    at(&sorted, t, &idle)
}

/// Where a walk is at a moment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WalkState {
    /// Steps taken (fractional): a foot lands on every whole step.
    pub phase: f64,
    /// How much the walk shows over the other pose (ramps in and out).
    pub weight: f64,
    pub dir: f64,
    pub stride: f64,
}

/// Steps a walk has taken at `t` (unclamped; negative before it starts).
fn steps_done(walk: &super::Walk, t: f64, beat: &dyn Fn(f64) -> f64) -> f64 {
    (beat(t) - beat(walk.start.value)) * walk.per_beat
}

/// The walk under way at `t` (the latest begun that still shows).
pub fn walk_state(walks: &[super::Walk], t: f64, beat: &dyn Fn(f64) -> f64) -> Option<WalkState> {
    let mut out = None;
    for walk in walks {
        if t < walk.start.value || walk.steps <= 0.0 {
            continue;
        }
        let done = steps_done(walk, t, beat);
        // In over the first quarter step, out over the half step after the last.
        let weight = if done < walk.steps { (done / 0.25).min(1.0) } else { 1.0 - (done - walk.steps) / 0.5 };
        if weight > 0.0 {
            out = Some(WalkState { phase: done.min(walk.steps), weight: weight.clamp(0.0, 1.0), dir: walk.dir, stride: walk.stride });
        }
    }
    out
}

/// How far the walks have carried the character at `t`, document units.
pub fn walk_offset(walks: &[super::Walk], t: f64, beat: &dyn Fn(f64) -> f64) -> [f64; 2] {
    let mut x = 0.0;
    for walk in walks {
        if t < walk.start.value {
            continue;
        }
        x += walk.dir * walk.stride * steps_done(walk, t, beat).clamp(0.0, walk.steps.max(0.0));
    }
    [x, 0.0]
}

/// The direction of the last walk begun by `t`.
pub fn last_dir(walks: &[super::Walk], t: f64) -> Option<f64> {
    walks.iter().filter(|w| w.start.value <= t).max_by(|a, b| a.start.value.partial_cmp(&b.start.value).unwrap_or(std::cmp::Ordering::Equal)).map(|w| w.dir)
}

/// The thigh swing (degrees) whose feet land `stride` apart (model units)
/// on legs `leg` long: feet don't slide.
pub fn swing_for(stride: f64, leg: f64) -> f64 {
    let s = (stride / (2.0 * leg.max(1e-6))).clamp(0.0, 1.0);
    s.asin().to_degrees().clamp(6.0, 38.0)
}

/// `base` walking: at every whole step both feet are down (the step lands
/// on the beat); between, the leg behind swings forward with its knee bent
/// and the arms swing against the legs.
pub fn walking(base: &Pose, phase: f64, swing: f64) -> Pose {
    use Joint::*;
    let s = (std::f64::consts::PI * phase).cos();
    let q = (std::f64::consts::PI * phase).sin();
    let mut p = *base;
    let a = swing;
    let knee = 1.7 * a;
    let arm = 0.9 * a;
    p.angles[LegLU as usize] += a * s;
    p.angles[LegRU as usize] -= a * s;
    p.angles[LegLL as usize] += knee * q.max(0.0);
    p.angles[LegRL as usize] += knee * (-q).max(0.0);
    p.angles[FootL as usize] -= 0.8 * (a * s + knee * q.max(0.0));
    p.angles[FootR as usize] -= 0.8 * (-a * s + knee * (-q).max(0.0));
    p.angles[ArmLU as usize] -= arm * s;
    p.angles[ArmRU as usize] += arm * s;
    p.angles[ArmLL as usize] -= 14.0;
    p.angles[ArmRL as usize] -= 14.0;
    p.angles[Torso as usize] += 4.0;
    p.angles[Head as usize] -= 2.0;
    p
}

/// The shape of a groove on the beat: 1 on the beat, easing to 0 halfway
/// between.
pub fn beat_pulse(beat: f64) -> f64 {
    let f = beat.rem_euclid(1.0);
    let c = 0.5 + 0.5 * (std::f64::consts::TAU * f).cos();
    c * c
}

pub fn nod_angle(beat: f64, nod: f64) -> f64 {
    nod * beat_pulse(beat)
}

/// A head nod of `nod` degrees and a knee bob of `bob` degrees on every beat.
pub fn groove(pose: &mut Pose, beat: f64, nod: f64, bob: f64) {
    use Joint::*;
    let g = beat_pulse(beat);
    pose.angles[Head as usize] += nod * g;
    if bob != 0.0 {
        let b = bob * g;
        pose.angles[LegLU as usize] -= b;
        pose.angles[LegRU as usize] -= b;
        pose.angles[LegLL as usize] += 2.0 * b;
        pose.angles[LegRL as usize] += 2.0 * b;
        pose.angles[FootL as usize] -= b;
        pose.angles[FootR as usize] -= b;
        pose.angles[Torso as usize] += 0.3 * b;
    }
}

/// Secondary motion at `t`: `(sway, head_lag)` degrees. `sample(u)` gives
/// the body's sideways position (model units) and the head's nod angle at
/// `u`; two damped springs (hair and cloth loose, the head stiffer) are run
/// from rest over the second before `t`, so the result is a pure function
/// of `t`.
pub fn secondary(t: f64, sample: &dyn Fn(f64) -> (f64, f64)) -> (f64, f64) {
    const RATE: f64 = 120.0;
    const WINDOW: f64 = 1.2;
    let n = (WINDOW * RATE) as usize;
    let dt = 1.0 / RATE;
    let values: Vec<(f64, f64)> = (0..=n + 1).map(|i| sample(t - WINDOW + (i as f64 - 1.0) * dt)).collect();
    // Hair: 1.6 Hz, loose; the head: 2.6 Hz, damped.
    let (k1, c1) = ((std::f64::consts::TAU * 1.6).powi(2), 2.0 * 0.22 * std::f64::consts::TAU * 1.6);
    let (k2, c2) = ((std::f64::consts::TAU * 2.6).powi(2), 2.0 * 0.55 * std::f64::consts::TAU * 2.6);
    let (mut a, mut va, mut h, mut vh) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for i in 1..=n {
        let accel = (values[i + 1].0 - 2.0 * values[i].0 + values[i - 1].0) / (dt * dt);
        let turn = (values[i + 1].1 - 2.0 * values[i].1 + values[i - 1].1) / (dt * dt);
        // A body speeding up leaves its loose parts behind.
        let fa = -k1 * a - c1 * va + 0.3 * accel - 0.5 * turn;
        va += fa * dt;
        a += va * dt;
        let fh = -k2 * h - c2 * vh - 0.08 * accel;
        vh += fh * dt;
        h += vh * dt;
    }
    (a.clamp(-25.0, 25.0), h.clamp(-12.0, 12.0))
}
