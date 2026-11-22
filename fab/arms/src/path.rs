//! A tool path and its timing: waypoints in the part frame, each with the tool's axes, the
//! speed to reach it at and whether the process runs on the way; the timing jerk-limited
//! speed changes over the path length (an S-curve; a trapezoid at infinite jerk), slowing for
//! corners by how far the tool may round them.

use fab_robot::math::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Waypoint {
    /// Where the TCP goes (part frame).
    pub p: V3,
    /// The tool axis there (unit, pointing into the work).
    pub z: V3,
    /// The tool's x there, where its roll matters (a cut's offcut side).
    pub x: Option<V3>,
    /// Speed toward it (mm/s).
    pub speed: f64,
    /// The process runs on the way here.
    pub on: bool,
}

/// How a path may move: the TCP's acceleration (mm/s²) and jerk (mm/s³; infinite for a
/// trapezoid), how far a corner may be rounded (mm: the speed a corner allows is the arc of
/// that deviation taken at `accel`), how fast the tool may turn along it (rad/s: a tangent
/// beam round a small radius turns fast), the tool's length from the flange to its TCP (mm:
/// where the tool turns, the flange swings that much further per radian, under the same
/// acceleration and jerk), and whether the tool comes to rest where a process starts (the
/// live loop may hold it there).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Motion {
    pub accel: f64,
    pub jerk: f64,
    pub deviation: f64,
    pub turn: f64,
    pub lever: f64,
    pub rest: bool,
}

/// The path's timing: arc length per waypoint and the speed at each.
#[derive(Clone, Debug)]
pub struct Profile {
    pub s: Vec<f64>,
    v: Vec<f64>,
    /// Each segment's peak speed: its waypoint's, or less where the tool turns faster per mm
    /// than its turn rate allows, or where the segment is too short to reach it.
    vp: Vec<f64>,
    /// Time at each waypoint.
    pub t: Vec<f64>,
    /// Each segment's acceleration and jerk along it: the motion's, less where the tool
    /// turns (the flange swings further than the TCP moves).
    accel: Vec<f64>,
    jerk: Vec<f64>,
}

/// Where the path is at a time: position, axes, speed along it, and the segment.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub p: V3,
    pub z: V3,
    pub x: Option<V3>,
    pub speed: f64,
    pub seg: usize,
    /// How far along the segment (0..1).
    pub u: f64,
    pub on: bool,
}

/// A positioner turning while a path runs: its turn at each waypoint (rad, about the part
/// frame's z), the turn axis' fastest speed and acceleration (rad/s, rad/s²).
#[derive(Clone, Copy, Debug)]
pub struct Turning<'a> {
    pub turns: &'a [f64],
    pub vmax: f64,
    pub accel: f64,
}

impl Profile {
    /// Time `path` under `m`: each segment speeds up to its top speed, cruises and slows down
    /// with the acceleration and jerk limits, corners at the speed their rounding allows.
    pub fn new(path: &[Waypoint], m: &Motion) -> Profile {
        Profile::build(path, m, None)
    }

    /// The same with the part turning under the tool as `turning` says: the TCP keeps its
    /// speeds along the part, the tool turns only as much as the part does not turn it (the
    /// lever's swing from that), and the turn axis keeps within its own speed and acceleration.
    pub fn turning(path: &[Waypoint], m: &Motion, turning: Turning) -> Profile {
        Profile::build(path, m, Some(turning))
    }

    fn build(path: &[Waypoint], m: &Motion, turning: Option<Turning>) -> Profile {
        let n = path.len();
        let mut s = vec![0.0; n];
        let mut vc = vec![0.0; n];
        let (mut accel, mut jerk) = (vec![m.accel; n], vec![m.jerk; n]);
        // The tool's axes as the arm holds them: the part's turn taken off.
        let held = |i: usize, v: V3| match turning {
            Some(tr) => mv(&rot_z(tr.turns[i]), v),
            None => v,
        };
        for i in 1..n {
            let l = dist(path[i - 1].p, path[i].p);
            s[i] = s[i - 1] + l;
            let (a, b) = (&path[i - 1], &path[i]);
            let mut angle = dot(held(i - 1, a.z), held(i, b.z)).clamp(-1.0, 1.0).acos();
            if let (Some(xa), Some(xb)) = (a.x, b.x) {
                angle = angle.max(dot(held(i - 1, xa), held(i, xb)).clamp(-1.0, 1.0).acos());
            }
            vc[i] = if angle > 1e-9 { b.speed.min(m.turn * l / angle) } else { b.speed };
            // The flange moves the TCP's way plus the lever's swing.
            if angle > 1e-9 && l > 1e-12 {
                let k = 1.0 + m.lever * angle / l;
                accel[i] /= k;
                jerk[i] /= k;
            }
            // The turn axis: its speed and acceleration per mm of the path.
            if let Some(tr) = turning {
                let turn = (tr.turns[i] - tr.turns[i - 1]).abs();
                if turn > 1e-9 && l > 1e-12 {
                    vc[i] = vc[i].min(tr.vmax * l / turn);
                    let a = accel[i].min(tr.accel * l / turn);
                    jerk[i] *= a / accel[i];
                    accel[i] = a;
                }
            }
        }
        // Corner speed limits.
        let mut v = vec![0.0; n];
        for i in 1..n.saturating_sub(1) {
            let a = norm(sub(path[i].p, path[i - 1].p));
            let b = norm(sub(path[i + 1].p, path[i].p));
            // The junction's half angle (between the way in, reversed, and the way out):
            // straight on is a right half angle, a full reversal none.
            let c = dot(a, b).clamp(-1.0, 1.0);
            let sh = ((1.0 + c) * 0.5).sqrt();
            let lim = if sh > 1.0 - 1e-9 { f64::MAX } else { (accel[i].min(accel[i + 1]) * m.deviation * sh / (1.0 - sh)).sqrt() };
            v[i] = vc[i].min(vc[i + 1]).min(lim);
            if m.rest && !path[i].on && path[i + 1].on {
                v[i] = 0.0;
            }
        }
        // Forward and backward passes: each junction's speed reachable from the last one's
        // within the segment between.
        for i in 1..n.saturating_sub(1) {
            v[i] = v[i].min(reach(v[i - 1], s[i] - s[i - 1], accel[i], jerk[i]));
        }
        for i in (0..n.saturating_sub(1)).rev() {
            v[i] = v[i].min(reach(v[i + 1], s[i + 1] - s[i], accel[i + 1], jerk[i + 1]));
        }
        // Each segment: speed up, cruise at its peak, slow down.
        let mut t = vec![0.0; n];
        let mut vp = vec![0.0; n];
        for i in 1..n {
            let l = s[i] - s[i - 1];
            vp[i] = peak(l, v[i - 1], v[i], vc[i], accel[i], jerk[i]);
            t[i] = t[i - 1] + seg_time(l, v[i - 1], v[i], vp[i], accel[i], jerk[i]);
        }
        Profile { s, v, vp, t, accel, jerk }
    }

    pub fn duration(&self) -> f64 {
        self.t.last().copied().unwrap_or(0.0)
    }

    pub fn length(&self) -> f64 {
        self.s.last().copied().unwrap_or(0.0)
    }

    /// The turn at time `time` of a positioner turning through `turns` (one per waypoint).
    pub fn turn_at(&self, path: &[Waypoint], turns: &[f64], time: f64) -> f64 {
        let smp = self.at(path, time);
        if smp.seg == 0 {
            return turns[0];
        }
        turns[smp.seg - 1] + (turns[smp.seg] - turns[smp.seg - 1]) * smp.u
    }

    /// The path at time `time` (clamped to its ends).
    pub fn at(&self, path: &[Waypoint], time: f64) -> Sample {
        let n = path.len();
        if n == 1 || time <= 0.0 {
            return Sample { p: path[0].p, z: path[0].z, x: path[0].x, speed: 0.0, seg: 0, u: 0.0, on: false };
        }
        let time = time.min(self.duration());
        let i = match self.t.binary_search_by(|x| x.total_cmp(&time)) {
            Ok(k) => k.max(1),
            Err(k) => k.clamp(1, n - 1),
        };
        let l = self.s[i] - self.s[i - 1];
        let (d, speed) = seg_at(l, self.v[i - 1], self.v[i], self.vp[i], self.accel[i], self.jerk[i], time - self.t[i - 1]);
        let u = if l > 1e-12 { (d / l).clamp(0.0, 1.0) } else { 1.0 };
        let (a, b) = (&path[i - 1], &path[i]);
        let x = match (a.x, b.x) {
            (Some(xa), Some(xb)) => Some(norm(lerp(xa, xb, u))),
            (_, x) => x,
        };
        Sample { p: lerp(a.p, b.p, u), z: norm(lerp(a.z, b.z, u)), x, speed, seg: i, u, on: b.on }
    }
}

/// A speed change from `v0` to `v1` under `accel` and `jerk`: the acceleration ramps up at
/// the jerk, holds, ramps down (no hold when the change is too small to reach `accel`). Its
/// peak acceleration and duration; the speed runs point-symmetric about the middle, so the
/// distance is the mean speed times the duration.
fn ramp(v0: f64, v1: f64, accel: f64, jerk: f64) -> (f64, f64) {
    let dv = (v1 - v0).abs();
    if dv <= 0.0 {
        return (accel, 0.0);
    }
    let ap = accel.min((dv * jerk).sqrt());
    (ap, dv / ap + ap / jerk)
}

fn ramp_len(v0: f64, v1: f64, accel: f64, jerk: f64) -> f64 {
    0.5 * (v0 + v1) * ramp(v0, v1, accel, jerk).1
}

/// The distance and speed `tau` s into the change from `v0` to `v1`.
fn ramp_at(v0: f64, v1: f64, accel: f64, jerk: f64, tau: f64) -> (f64, f64) {
    let dv = (v1 - v0).abs();
    let (ap, dur) = ramp(v0, v1, accel, jerk);
    let tau = tau.clamp(0.0, dur);
    // The jerk phases' length (none at infinite jerk; jerk · t1 = ap).
    let t1 = if jerk.is_finite() { ap / jerk } else { 0.0 };
    // The speed gained and its integral.
    let (gain, area) = if tau < t1 {
        (0.5 * jerk * tau * tau, jerk * tau * tau * tau / 6.0)
    } else if tau <= dur - t1 {
        let u = tau - t1;
        (0.5 * ap * t1 + ap * u, ap * t1 * t1 / 6.0 + 0.5 * ap * t1 * u + 0.5 * ap * u * u)
    } else {
        let w = dur - tau;
        (dv - 0.5 * jerk * w * w, 0.5 * dv * dur - (dv * w - jerk * w * w * w / 6.0))
    };
    let sign = if v1 >= v0 { 1.0 } else { -1.0 };
    (v0 * tau + sign * area, v0 + sign * gain)
}

/// The fastest speed a change from `v0` reaches within `ds` (up or down: the same either way).
fn reach(v0: f64, ds: f64, accel: f64, jerk: f64) -> f64 {
    if ds <= 0.0 {
        return v0;
    }
    // Past the change that just reaches `accel` (dv_c = a²/j, over d_c): the duration is
    // dv/a + a/j, the distance a quadratic in dv.
    let dv_c = accel * accel / jerk;
    let d_c = ramp_len(v0, v0 + dv_c, accel, jerk);
    if ds >= d_c {
        let (a, b, c) = (0.5 / accel, v0 / accel + 0.5 * accel / jerk, v0 * accel / jerk - ds);
        return v0 + (-b + (b * b - 4.0 * a * c).max(0.0).sqrt()) / (2.0 * a);
    }
    // Below it the distance is (2 v0 r + r³)/√j with r = √dv: Newton from above (either term
    // alone overshoots the root), monotone on the convex cubic.
    let sj = jerk.sqrt();
    let mut r = (ds * sj).cbrt().min(ds * sj / (2.0 * v0.max(1e-300)));
    for _ in 0..30 {
        let f = r * r * r + 2.0 * v0 * r - ds * sj;
        let step = f / (3.0 * r * r + 2.0 * v0).max(1e-300);
        r -= step;
        if step.abs() < 1e-12 * r.abs().max(1e-12) {
            break;
        }
    }
    v0 + r.max(0.0) * r.max(0.0)
}

/// The peak speed a segment reaches from `v0` to `v1` over `l`, capped at `vc`.
fn peak(l: f64, v0: f64, v1: f64, vc: f64, accel: f64, jerk: f64) -> f64 {
    let lo = v0.max(v1).min(vc).max(1e-9);
    let fits = |vp: f64| ramp_len(v0, vp, accel, jerk) + ramp_len(vp, v1, accel, jerk) <= l;
    if fits(vc) {
        return vc.max(1e-9);
    }
    let (mut a, mut b) = (lo, vc.max(lo));
    for _ in 0..48 {
        let m = 0.5 * (a + b);
        if fits(m) {
            a = m;
        } else {
            b = m;
        }
    }
    a
}

fn seg_time(l: f64, v0: f64, v1: f64, vp: f64, accel: f64, jerk: f64) -> f64 {
    if l <= 1e-12 {
        return 0.0;
    }
    let (la, lb) = (ramp_len(v0, vp, accel, jerk), ramp_len(vp, v1, accel, jerk));
    ramp(v0, vp, accel, jerk).1 + ramp(vp, v1, accel, jerk).1 + (l - la - lb).max(0.0) / vp
}

/// Distance and speed `t` s into a segment.
fn seg_at(l: f64, v0: f64, v1: f64, vp: f64, accel: f64, jerk: f64, t: f64) -> (f64, f64) {
    if l <= 1e-12 {
        return (l, v1);
    }
    let ta = ramp(v0, vp, accel, jerk).1;
    let (la, lb) = (ramp_len(v0, vp, accel, jerk), ramp_len(vp, v1, accel, jerk));
    let lc = (l - la - lb).max(0.0);
    let tc = lc / vp;
    if t < ta {
        ramp_at(v0, vp, accel, jerk, t)
    } else if t < ta + tc {
        (la + vp * (t - ta), vp)
    } else {
        let (d, v) = ramp_at(vp, v1, accel, jerk, t - ta - tc);
        ((la + lc + d).min(l), v)
    }
}
