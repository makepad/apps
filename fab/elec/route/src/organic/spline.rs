// Ported from TiNRS Psychedelic Router 0.1.0: src/organic/OrganicRelaxer.cpp (centripetalCR, clearancePass, buildCurve including its biarc branch, 6753-6782).
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in this folder.

//! Spline export: guarded biarcs, chord clearance, centripetal Catmull-Rom pieces and straight-stub tangents.

use super::copper::K_BIG;
use super::engine::{finite_vec, Engine};
use super::geom::{std_max, std_min, Segment2, Vec2};
use super::model::{OBoard, Trace};
use super::relax::ops::{segs_touch, straight_segment};
use super::settings::{relax_immutable, OrganicSettings};

/// Centripetal Catmull-Rom (Barry-Goldman) on [p1, p2] at local parameter s in [0, 1].
pub fn centripetal_cr(p0: Vec2, p1: Vec2, p2: Vec2, p3: Vec2, s: f32) -> Vec2 {
    let knot = |a: Vec2, b: Vec2| std_max(1e-4f32, a.distance_to(b).sqrt());
    let t0: f32 = 0.0;
    let t1 = t0 + knot(p0, p1);
    let t2 = t1 + knot(p1, p2);
    let t3 = t2 + knot(p2, p3);
    let t = t1 + (t2 - t1) * s;
    let a1 = p0 * ((t1 - t) / (t1 - t0)) + p1 * ((t - t0) / (t1 - t0));
    let a2 = p1 * ((t2 - t) / (t2 - t1)) + p2 * ((t - t1) / (t2 - t1));
    let a3 = p2 * ((t3 - t) / (t3 - t2)) + p3 * ((t - t2) / (t3 - t2));
    let b1 = a1 * ((t2 - t) / (t2 - t0)) + a2 * ((t - t0) / (t2 - t0));
    let b2 = a2 * ((t3 - t) / (t3 - t1)) + a3 * ((t - t1) / (t3 - t1));
    b1 * ((t2 - t) / (t2 - t1)) + b2 * ((t - t1) / (t2 - t1))
}

/// Chord slack for the spline guard, nominal node widths and optional per-trace copper gaps.
pub fn clearance_pass(
    e: &mut Engine,
    b: &mut OBoard,
    _st: &OrganicSettings,
    mut slack: Option<&mut Vec<Vec<f64>>>,
    mask: Option<&[u8]>,
    mut min_gap: Option<&mut Vec<f64>>,
) {
    if let Some(slack) = slack.as_deref_mut() {
        slack.clear();
        slack.resize(b.traces.len(), Vec::new());
    }
    if let Some(min_gap) = min_gap.as_deref_mut() {
        min_gap.clear();
        min_gap.resize(b.traces.len(), -K_BIG);
    }
    let mut todo = Vec::new();
    for t in 0..b.traces.len() {
        if mask.is_none() || mask.unwrap()[t] != 0 {
            todo.push(t as i32);
        }
    }
    // ADAPTED: parallelFor is serial in trace order, taking and restoring e.x0 as its scratch.
    let mut x = std::mem::take(&mut e.x0);
    for t in todo {
        let tr = &mut b.traces[t as usize];
        let n = tr.points.len() as i32;
        for pt in &mut tr.points {
            pt.width = tr.width;
        }
        if n < 2 || (slack.is_none() && min_gap.is_none()) {
            continue;
        }
        if let Some(slack) = slack.as_deref_mut() {
            slack[t as usize].resize((n - 1) as usize, 0.0);
        }
        let mut mg = K_BIG;
        for k in 0..n - 1 {
            let tr = &b.traces[t as usize];
            let len = tr.points[k as usize].pos.distance_to(tr.points[(k + 1) as usize].pos) as f64;
            let cap = if slack.is_some() { std_min(0.3, 0.25 * len) } else { 0.0 };
            let (sl, cg) = e.segment_clearance(&*b, &mut x, t, k, cap, if min_gap.is_some() { 0.05 } else { 0.0 });
            if let Some(slack) = slack.as_deref_mut() {
                slack[t as usize][k as usize] = sl;
            }
            mg = std_min(mg, cg);
        }
        if let Some(min_gap) = min_gap.as_deref_mut() {
            min_gap[t as usize] = mg;
        }
    }
    e.x0 = x;
}

/// Build the exported path, interpolated widths and chord-summed length of one trace.
pub fn build_curve(tr: &mut Trace, settings: &OrganicSettings, sub: i32, chord_slack: Option<&[f64]>) {
    tr.smoothed.clear();
    tr.point_widths.clear();
    tr.length = 0.0f32;
    let n = tr.points.len() as i32;
    if n < 2 {
        return;
    }

    let mut done = false;
    if n == 2 || sub <= 1 || relax_immutable(tr, settings) {
        for tp in &tr.points {
            tr.smoothed.push(tp.pos);
            tr.point_widths.push(tp.width);
        }
        done = true;
    }

    // Fixed straight exit stubs use chords; the next curve leaves along the stub tangent.
    let mut straight = vec![0u8; (n - 1) as usize];
    let mut any_straight = false;
    for k in 0..n - 1 {
        if straight_segment(&tr.points, k) {
            straight[k as usize] = 1;
            any_straight = true;
        }
    }

    // Allowed deviation per chord.
    let mut slack = vec![0.0f64; (n - 1) as usize];
    if !done {
        for k in 0..(n - 1) as usize {
            let len = tr.points[k].pos.distance_to(tr.points[k + 1].pos) as f64;
            let cap = std_min(0.3, 0.25 * len);
            slack[k] = if let Some(chord_slack) = chord_slack { chord_slack[k] } else { cap * 4.0 };
            if straight[k] != 0 {
                slack[k] = 0.0;
            }
        }
    }

    if !done && settings.enable_biarcs && !any_straight {
        let mut raw = Vec::new();
        for tp in &tr.points {
            raw.push(tp.pos);
        }
        let arc = crate::organic::biarc::BiarcFitter::fit_biarc_spline(&raw, settings.curve_subdivisions);
        let mut ok = arc.len() >= 2 && finite_vec(arc[0]);
        let mut widths = Vec::new();
        let mut i = 0;
        while i < arc.len() && ok {
            if !finite_vec(arc[i]) {
                ok = false;
                break;
            }
            let mut best = K_BIG;
            let mut bj = 0;
            for k in 0..(n - 1) as usize {
                let d = Segment2::new(tr.points[k].pos, tr.points[k + 1].pos).distance_to_point(arc[i]) as f64;
                if d < best { best = d; bj = k; }
            }
            if best > slack[bj] + 1e-6 { ok = false; }
            widths.push(std_min(tr.points[bj].width, tr.points[bj + 1].width));
            i += 1;
        }
        let mut i = 0;
        while i + 1 < arc.len() && ok {
            let mut j = i + 2;
            while j + 1 < arc.len() && ok {
                if segs_touch(arc[i], arc[i + 1], arc[j], arc[j + 1]) { ok = false; }
                j += 1;
            }
            i += 1;
        }
        if ok {
            tr.smoothed = arc;
            tr.point_widths = widths;
            done = true;
        }
    }

    if !done {
        let p = &tr.points;
        let piece = |i: usize, max_dev: f64, out_pts: &mut Vec<Vec2>| {
            out_pts.clear();
            let p1 = p[i].pos;
            let p2 = p[i + 1].pos;
            let p0 = if i > 0 { p[i - 1].pos } else { p1 * 2.0f32 - p2 };
            let p3 = if i + 2 < n as usize { p[i + 2].pos } else { p2 * 2.0f32 - p1 };
            let mut raw = vec![Vec2::default(); sub as usize];
            let mut dev = 0.0f64;
            let stub_before = i > 0 && straight[i - 1] != 0;
            let stub_after = i + 2 < n as usize && straight[i + 1] != 0;
            let hermite = straight[i] == 0 && (stub_before || stub_after);
            let mut m1 = Vec2::default();
            let mut m2 = Vec2::default();
            if hermite {
                let l = p1.distance_to(p2);
                m1 = if stub_before { (p1 - p0).normalized() * l } else { (p2 - p0) * 0.5f32 };
                m2 = if stub_after { (p3 - p2).normalized() * l } else { (p3 - p1) * 0.5f32 };
            }
            for s in 0..sub as usize {
                let u = s as f32 / sub as f32;
                let mut q = if s == 0 { p1 } else { centripetal_cr(p0, p1, p2, p3, u) };
                if hermite && s > 0 {
                    let u2 = u * u;
                    let u3 = u2 * u;
                    q = p1 * (2.0 * u3 - 3.0 * u2 + 1.0) + m1 * (u3 - 2.0 * u2 + u)
                        + p2 * (-2.0 * u3 + 3.0 * u2) + m2 * (u3 - u2);
                }
                if !finite_vec(q) {
                    q = Vec2::lerp(p1, p2, u);
                }
                raw[s] = q;
                dev = std_max(dev, q.distance_to(Vec2::lerp(p1, p2, u)) as f64);
            }
            let f = if dev > max_dev && dev > 0.0 { (max_dev / dev) as f32 } else { 1.0f32 };
            for s in 0..sub as usize {
                let u = s as f32 / sub as f32;
                let l = Vec2::lerp(p1, p2, u);
                out_pts.push(l + (raw[s] - l) * f);
            }
        };
        let mut pieces = vec![Vec::new(); (n - 1) as usize];
        let mut allowed = slack.clone();
        for i in 0..(n - 1) as usize {
            piece(i, allowed[i], &mut pieces[i]);
        }
        // Adjacent pieces must not touch each other except at their shared node.
        for _pass in 0..2 {
            let mut fixed_any = false;
            for i in 0..(n - 2) as usize {
                if allowed[i] <= 0.0 && allowed[i + 1] <= 0.0 {
                    continue;
                }
                {
                    // Below a 60-degree turn the bounded pieces cannot meet except at the shared node.
                    let u0 = (p[i + 1].pos - p[i].pos).normalized();
                    let u1 = (p[i + 2].pos - p[i + 1].pos).normalized();
                    if u0.dot(u1) > 0.5f32 {
                        continue;
                    }
                }
                // Polyline of piece i (including node i+1) and piece i+1 (including node i+2).
                let mut a = pieces[i].clone();
                a.push(p[i + 1].pos);
                let mut b_v = pieces[i + 1].clone();
                b_v.push(p[i + 2].pos);
                let mut bad = false;
                for a_idx in 0..a.len() - 1 {
                    if bad {
                        break;
                    }
                    for b_idx in 0..b_v.len() - 1 {
                        if bad {
                            break;
                        }
                        if a_idx + 2 == a.len() && b_idx == 0 {
                            continue;
                        }
                        if segs_touch(a[a_idx], a[a_idx + 1], b_v[b_idx], b_v[b_idx + 1]) {
                            bad = true;
                        }
                    }
                }
                if bad {
                    allowed[i] = 0.0;
                    allowed[i + 1] = 0.0;
                    piece(i, 0.0, &mut pieces[i]);
                    piece(i + 1, 0.0, &mut pieces[i + 1]);
                    fixed_any = true;
                }
            }
            if !fixed_any {
                break;
            }
        }
        for i in 0..(n - 1) as usize {
            for s in 0..sub as usize {
                let u = s as f32 / sub as f32;
                tr.smoothed.push(pieces[i][s]);
                tr.point_widths.push(p[i].width + (p[i + 1].width - p[i].width) * u);
            }
        }
        tr.smoothed.push(p[(n - 1) as usize].pos);
        tr.point_widths.push(p[(n - 1) as usize].width);
    }

    for k in 0..tr.smoothed.len() - 1 {
        tr.length += tr.smoothed[k].distance_to(tr.smoothed[k + 1]);
    }
}
