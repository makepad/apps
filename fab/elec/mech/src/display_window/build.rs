//! Display window polygons, solids, measurements and manufacturing checks.

use super::params::*;
use crate::csg::{Csg, Prim, Ring, V2};
use std::f64::consts::PI;

const TAU: f64 = 2.0 * PI;

fn deg(d: f64) -> f64 {
    d * PI / 180.0
}

fn clampf(v: f64, lo: f64, hi: f64) -> f64 {
    lo.max(hi.min(v))
}

fn circle(n: i32, r: f64) -> Poly {
    (0..n)
        .map(|i| {
            [
                r * (TAU * i as f64 / n as f64).cos(),
                r * (TAU * i as f64 / n as f64).sin(),
            ]
        })
        .collect()
}

// Equal point counts keep loft rings matched: k per corner and eight per side.
fn rounded_rect(w: f64, h: f64, r: f64, k: i32) -> Poly {
    let r = clampf(r, 0.02, w.min(h) * 0.5 - 0.001);
    let cx = w * 0.5 - r;
    let cy = h * 0.5 - r;
    let ox = [cx, -cx, -cx, cx];
    let oy = [cy, cy, -cy, -cy];
    let side = 8;
    let mut q = Poly::new();
    for c in 0..4 {
        for i in 0..k {
            let a = PI * 0.5 * (c as f64 + i as f64 / (k - 1) as f64);
            q.push([ox[c] + r * a.cos(), oy[c] + r * a.sin()]);
        }
        let n = (c + 1) % 4;
        let a1 = PI * 0.5 * (c + 1) as f64;
        let p0 = *q.last().unwrap();
        let p1 = [ox[n] + r * a1.cos(), oy[n] + r * a1.sin()];
        for i in 1..=side {
            let t = i as f64 / (side + 1) as f64;
            q.push([p0[0] + (p1[0] - p0[0]) * t, p0[1] + (p1[1] - p0[1]) * t]);
        }
    }
    q
}

fn rotated(q: &Poly, a: f64) -> Poly {
    let c = a.cos();
    let s = a.sin();
    q.iter().map(|p| [p[0] * c - p[1] * s, p[0] * s + p[1] * c]).collect()
}

// Clock degrees from +y, clockwise, to the rotation that takes +x there.
fn clock_to_rot(clock_deg: f64) -> f64 {
    PI * 0.5 - deg(clock_deg)
}

fn arc(q: &mut Poly, r: f64, a0: f64, a1: f64, n: i32) {
    let k = 2.max(((a1 - a0) / TAU * n as f64).ceil() as i32 + 1);
    for i in 0..k {
        let a = a0 + (a1 - a0) * i as f64 / (k - 1) as f64;
        q.push([r * a.cos(), r * a.sin()]);
    }
}

fn hole_poly(p: &WindowParams, n: i32) -> Poly {
    let r = 0.3_f64.max(p.hole_d * 0.5);
    let t = p.hole();
    if t == HoleType::Round {
        return circle(n, r);
    }
    if t == HoleType::DFlat {
        let f = clampf(p.flat_dist, -r * 0.9, r * 0.95);
        let a = (f / r).acos();
        let mut q = Poly::new();
        arc(&mut q, r, a, TAU - a, n);
        return rotated(&q, clock_to_rot(p.notch_angle));
    }
    let w = clampf(p.notch_width * 0.5, 0.05, r * 0.9);
    let tip = (p.notch_span * 0.5).max(r + 0.05);
    let ai = (w / r).asin();
    let xs = (r * r - w * w).sqrt();
    let nn = if t == HoleType::Notched2 { 2 } else { 1 };
    let mut q = Poly::new();
    for k in 0..nn {
        let a0 = TAU * k as f64 / nn as f64;
        let a1 = TAU * (k + 1) as f64 / nn as f64;
        let c = a0.cos();
        let s = a0.sin();
        let mut put = |x, y| q.push([x * c - y * s, x * s + y * c]);
        put(xs, -w);
        if p.notch_end() == NotchEnd::Round {
            let c0 = (tip - w).max(xs);
            let kk = 6.max(n / 16);
            for i in 0..kk {
                let a = -PI * 0.5 + PI * i as f64 / (kk - 1) as f64;
                put(c0 + w * a.cos(), w * a.sin());
            }
        } else {
            put(tip, -w);
            put(tip, w);
        }
        let mut a = Poly::new();
        arc(&mut a, r, a0 + ai, a1 - ai, n);
        q.extend_from_slice(&a[..a.len() - 1]);
    }
    rotated(&q, clock_to_rot(p.notch_angle))
}

// One light segment about its own centre, with its radial axis along +x.
fn seg_poly(p: &WindowParams, n: i32) -> Poly {
    let l = 0.2_f64.max(p.seg_length);
    let w = 0.2_f64.max(p.seg_width);
    let k = 6.max(n / 16);
    match p.seg_shape() {
        SegShape::Dot => circle(24.max(n / 4), w * 0.5),
        SegShape::RoundedRect => rounded_rect(l, w, p.seg_corner.min(l.min(w) * 0.5), k),
        SegShape::Wedge => {
            let rc = (p.seg_ring_d * 0.5).max(l * 0.5 + 0.2);
            let ri = rc - l * 0.5;
            let ro = rc + l * 0.5;
            let b = ((w * 0.5) / rc).min(PI * 0.45);
            let sb = b.sin();
            let rnd = p.seg_round();
            let mut r_in = if matches!(rnd, SegRound::Inner | SegRound::Both) { p.seg_corner } else { 0.0 };
            let mut r_out = if matches!(rnd, SegRound::Outer | SegRound::Both) { p.seg_corner } else { 0.0 };
            r_in = r_in.min(ri * sb / (1.0 - sb) * 0.999);
            r_out = r_out.min(ro * sb / (1.0 + sb) * 0.999);
            let delta = |d: f64, r: f64| 1.0_f64.min(r / d).asin();
            let side_in = |r| {
                let d = ri + r;
                d * delta(d, r).cos()
            };
            let side_out = |r| {
                let d = ro - r;
                d * delta(d, r).cos()
            };
            for _ in 0..30 {
                if !(side_in(r_in) > side_out(r_out) - 0.02) { break; }
                r_in *= 0.9;
                r_out *= 0.9;
            }
            let eps = 0.01;
            let o = [-rc, 0.0];
            let polar = |r: f64, a: f64| [o[0] + r * a.cos(), o[1] + r * a.sin()];
            let mut up = Poly::new();
            let d_o = if r_out > eps { delta(ro - r_out, r_out) } else { 0.0 };
            for i in 0..k {
                up.push(polar(ro, (b - d_o) * i as f64 / (k - 1) as f64));
            }
            if r_out > eps {
                let c = polar(ro - r_out, b - d_o);
                for i in 1..k {
                    let a = (b - d_o) + (PI * 0.5 + d_o) * i as f64 / (k - 1) as f64;
                    up.push([c[0] + r_out * a.cos(), c[1] + r_out * a.sin()]);
                }
            }
            let s0 = if r_out > eps { side_out(r_out) } else { ro };
            let s1 = if r_in > eps { side_in(r_in) } else { ri };
            for i in 1..5 {
                up.push(polar(s0 + (s1 - s0) * i as f64 / 5.0, b));
            }
            let d_i = if r_in > eps { delta(ri + r_in, r_in) } else { 0.0 };
            if r_in > eps {
                let c = polar(ri + r_in, b - d_i);
                for i in 0..k {
                    let a = (b + PI * 0.5) + (PI * 0.5 - d_i) * i as f64 / (k - 1) as f64;
                    up.push([c[0] + r_in * a.cos(), c[1] + r_in * a.sin()]);
                }
            }
            let ka = if b - d_i > 1e-3 { k } else { 1 };
            for i in (if r_in > eps { 1 } else { 0 })..ka {
                up.push(polar(ri, (b - d_i) * (1.0 - if ka > 1 { i as f64 / (ka - 1) as f64 } else { 1.0 })));
            }
            let mut q = up.clone();
            for i in (1..up.len() - 1).rev() {
                q.push([up[i][0], -up[i][1]]);
            }
            let mut out = Poly::new();
            for v in q {
                if out.last().is_none_or(|last| (v[0] - last[0]).hypot(v[1] - last[1]) > 1e-4) {
                    out.push(v);
                }
            }
            while out.len() > 3 && (out[0][0] - out.last().unwrap()[0]).hypot(out[0][1] - out.last().unwrap()[1]) < 1e-4 {
                out.pop();
            }
            out
        }
        SegShape::Slot => {
            let hw = w * 0.5;
            let hl = 0.0_f64.max(l * 0.5 - hw);
            let mut q = Poly::new();
            for i in 0..k {
                let a = -PI * 0.5 + PI * i as f64 / (k - 1) as f64;
                q.push([hl + hw * a.cos(), hw * a.sin()]);
            }
            for i in 0..k {
                let a = PI * 0.5 + PI * i as f64 / (k - 1) as f64;
                q.push([-hl + hw * a.cos(), hw * a.sin()]);
            }
            q
        }
    }
}

fn poly_area(q: &Poly) -> f64 {
    let mut a = 0.0;
    for i in 0..q.len() {
        let u = q[i];
        let v = q[(i + 1) % q.len()];
        a += u[0] * v[1] - v[0] * u[1];
    }
    0.5 * a
}

fn at(pl: &Placed, i: usize) -> V2 {
    [pl.poly[i][0] + pl.cx, pl.poly[i][1] + pl.cy]
}

fn inside(pl: &Placed, pt: V2) -> bool {
    let mut inside = false;
    if pl.poly.is_empty() { return inside; }
    let mut j = pl.poly.len() - 1;
    for i in 0..pl.poly.len() {
        let a = at(pl, i);
        let b = at(pl, j);
        if (a[1] > pt[1]) != (b[1] > pt[1]) && pt[0] < (b[0] - a[0]) * (pt[1] - a[1]) / (b[1] - a[1]) + a[0] {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn seg_dist(p: V2, a: V2, b: V2) -> f64 {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 { clampf(((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / l2, 0.0, 1.0) } else { 0.0 };
    (p[0] - a[0] - t * dx).hypot(p[1] - a[1] - t * dy)
}

// Vertex-to-edge distances both ways approximate the gap between dense outlines.
fn poly_dist(a: &Placed, b: &Placed) -> f64 {
    let mut m = 1e9_f64;
    for (a, b) in [(a, b), (b, a)] {
        for i in 0..a.poly.len() {
            let p = at(a, i);
            for j in 0..b.poly.len() {
                m = m.min(seg_dist(p, at(b, j), at(b, (j + 1) % b.poly.len())));
            }
        }
    }
    m
}

fn within(a: &Placed, outer: &Placed, margin: f64) -> bool {
    for i in 0..a.poly.len() {
        if !inside(outer, at(a, i)) { return false; }
    }
    margin <= 0.0 || poly_dist(a, outer) >= margin
}

fn overlaps(a: &Placed, b: &Placed) -> bool {
    for i in 0..a.poly.len() {
        if inside(b, at(a, i)) { return true; }
    }
    for i in 0..b.poly.len() {
        if inside(a, at(b, i)) { return true; }
    }
    false
}

// Rows follow the viewport inward, staggered by half a step. The edge row
// overlaps the print; sub-minimum dots are dropped and narrow gaps are closed.
fn dither_dots(p: &WindowParams, view: &Placed, n: i32) -> Vec<Placed> {
    let mut out = Vec::new();
    let a = clampf(p.dither_pitch, 0.1, 5.0);
    let m = clampf(p.dither_min, 0.02, a * 0.5);
    let w = p.dither_width.max(a * 0.5);
    let d_max = a * (1.0 + clampf(p.dither_overlap, 0.0, 100.0) / 100.0);
    let ex = clampf(p.dither_curve, -3.0, 3.0).exp2();
    let k = clampf((n / 16) as f64, 10.0, 24.0) as i32;
    let round = p.viewport() == Viewport::Round;
    for i in 0.. {
        let d = a * i as f64;
        let t = d / w;
        if t >= 1.0 { break; }
        let row = if round {
            if p.view_d * 0.5 - d < a * 0.5 { break; }
            circle(64.max(n), p.view_d * 0.5 - d)
        } else {
            if p.view_w.min(p.view_h) - 2.0 * d < a { break; }
            rounded_rect(p.view_w - 2.0 * d, p.view_h - 2.0 * d, p.view_corner - d, 6.max(n / 16))
        };
        let mut diameter = d_max * (1.0 - t).powf(ex).sqrt();
        if diameter < m { break; }
        if diameter < a && a - diameter < m { diameter = a + 0.01; }
        let dot = circle(k, diameter * 0.5);
        let mut len = vec![0.0; row.len() + 1];
        for j in 0..row.len() {
            let u = row[j];
            let v = row[(j + 1) % row.len()];
            len[j + 1] = len[j] + (v[0] - u[0]).hypot(v[1] - u[1]);
        }
        let length = *len.last().unwrap();
        let cnt = 1.max((length / a).round() as i32);
        let mut seg = 0;
        for j in 0..cnt {
            let s = length * (j as f64 + if i & 1 != 0 { 0.5 } else { 0.0 }) / cnt as f64;
            while seg + 1 < row.len() && len[seg + 1] < s { seg += 1; }
            let u = row[seg];
            let v = row[(seg + 1) % row.len()];
            let f = if len[seg + 1] > len[seg] { (s - len[seg]) / (len[seg + 1] - len[seg]) } else { 0.0 };
            out.push(Placed {
                poly: dot.clone(),
                cx: view.cx + u[0] + (v[0] - u[0]) * f,
                cy: view.cy + u[1] + (v[1] - u[1]) * f,
            });
        }
    }
    out
}

fn loft(pl: &Placed, z0: f64, z1: f64, label: &str) -> Prim {
    Prim::loft(label, [z0, z1].into_iter().map(|z| Ring {
        z,
        cx: pl.cx,
        cy: pl.cy,
        pts: pl.poly.clone(),
        ..Ring::default()
    }).collect())
        .crease(vec![true, true])
        .prism()
}

fn program(p: &WindowParams) -> Csg {
    Csg {
        segments: p.segments.clamp(48, 720),
        step_chord_length: true,
        ..Csg::default()
    }
}

/// The window outline shrunk by `inset`, in mm.
pub fn outline_poly(p: &WindowParams, inset: f64) -> Poly {
    let n = p.segments.clamp(48, 720);
    if p.outline() == Outline::Round {
        return circle(n, 0.2_f64.max(p.diameter * 0.5 - inset));
    }
    rounded_rect(0.4_f64.max(p.width - 2.0 * inset), 0.4_f64.max(p.height - 2.0 * inset), p.corner - inset, 6.max(n / 16))
}

/// Lay out the cut outline, holes, print openings, dots and adhesive in 2D.
pub fn layout(p: &WindowParams) -> WindowLayout {
    let n = p.segments.clamp(48, 720);
    let mut l = WindowLayout { outline: outline_poly(p, 0.0), ..WindowLayout::default() };
    if p.hole() != HoleType::None {
        l.holes.push(Placed { poly: hole_poly(p, n), cx: p.hole_x, cy: p.hole_y });
    }
    if p.mount_holes() != MountHoles::None {
        let four = p.mount_holes() == MountHoles::Four;
        let c = circle(24.max(n / 4), p.mount_d * 0.5);
        if p.outline() == Outline::Round {
            let r = p.diameter * 0.5 - p.mount_inset;
            for k in 0..if four { 4 } else { 2 } {
                let a = if four { PI * 0.25 + PI * 0.5 * k as f64 } else { PI * k as f64 };
                l.holes.push(Placed { poly: c.clone(), cx: r * a.cos(), cy: r * a.sin() });
            }
        } else {
            let x = p.width * 0.5 - p.mount_inset;
            let y = p.height * 0.5 - p.mount_inset;
            if four {
                for k in 0..4 {
                    l.holes.push(Placed { poly: c.clone(), cx: if k == 0 || k == 3 { x } else { -x }, cy: if k < 2 { y } else { -y } });
                }
            } else {
                for sx in [-1.0, 1.0] {
                    l.holes.push(Placed { poly: c.clone(), cx: sx * x, cy: 0.0 });
                }
            }
        }
    }
    if p.viewport() == Viewport::Round {
        l.openings.push(Placed { poly: circle(n, p.view_d * 0.5), cx: p.view_x, cy: p.view_y });
    } else if p.viewport() == Viewport::Rect {
        l.openings.push(Placed { poly: rounded_rect(p.view_w, p.view_h, p.view_corner, 6.max(n / 16)), cx: p.view_x, cy: p.view_y });
    }
    if p.seg_count > 0 {
        let s = seg_poly(p, n);
        let rr = p.seg_ring_d * 0.5;
        let full = p.seg_span >= 359.99;
        for i in 0..p.seg_count {
            let clock = p.seg_start + if full {
                360.0 * i as f64 / p.seg_count as f64
            } else if p.seg_count > 1 {
                p.seg_span * i as f64 / (p.seg_count - 1) as f64
            } else { 0.0 };
            let a = clock_to_rot(clock);
            l.segs.push(Placed { poly: rotated(&s, a), cx: p.hole_x + rr * a.cos(), cy: p.hole_y + rr * a.sin() });
        }
        l.openings.extend(l.segs.iter().cloned());
    }
    if p.print && p.dither && p.viewport() != Viewport::None {
        l.dots = dither_dots(p, &l.openings[0], n);
    }
    l.print_outer = outline_poly(p, p.print_inset);
    l.adhesive_outer = outline_poly(p, p.adhesive_inset);
    l.adhesive_inner = outline_poly(p, p.adhesive_inset + p.adhesive_width);
    l
}

/// Build the four CSG bodies and report dimensions and manufacturing checks.
pub fn build(p: &WindowParams) -> WindowBuild {
    let mut out = WindowBuild::default();
    let WindowBuild { window, print, ink, adhesive, report: r, warnings, notes } = &mut out;
    let mut warn = |s: &str| warnings.push(s.to_owned());
    let mut note = |s: &str| notes.push(s.to_owned());
    let l = layout(p);
    let n = p.segments.clamp(48, 720);
    let t = p.thickness;
    let pt = if p.print { p.print_thickness } else { 0.0 };
    let at = if p.adhesive { p.adhesive_thickness } else { 0.0 };
    let outl = Placed { poly: l.outline.clone(), ..Placed::default() };

    *window = program(p);
    let min_half = if p.outline() == Outline::Round { p.diameter * 0.5 } else { p.width.min(p.height) * 0.5 };
    let e = if p.edge() == EdgeStyle::None { 0.0 } else { p.edge_size.min((t * 0.8).min(min_half * 0.3)) };
    if e < p.edge_size - 1e-4 && p.edge() != EdgeStyle::None {
        note(&format!("front edge {e:.2} mm (limited by the thickness)"));
    }
    window.add(loft(&outl, 0.0, t - e, "window"));
    if e > 0.0 {
        let mut rings = Vec::new();
        let mut crease = Vec::new();
        let mut ring = |z, inset| rings.push(Ring { z, pts: outline_poly(p, inset), ..Ring::default() });
        ring(t - e, 0.0);
        crease.push(true);
        if p.edge() == EdgeStyle::Chamfer {
            ring(t, e);
            crease.push(true);
        } else {
            let m = 6;
            for k in 1..=m {
                let a = PI * 0.5 * k as f64 / m as f64;
                ring(t - e + e * a.sin(), e * (1.0 - a.cos()));
                crease.push(k == m);
            }
        }
        let w = if p.outline() == Outline::Rect {
            Prim::hull("front edge", rings.iter().flat_map(|rg| rg.pts.iter().map(move |q| [q[0], q[1], rg.z])).collect())
        } else {
            Prim::loft("front edge", rings).crease(crease)
        };
        window.add(w);
    }
    for h in &l.holes { window.sub(loft(h, -1.0, t + 1.0, "hole")); }

    let print_outer = Placed { poly: l.print_outer.clone(), ..Placed::default() };
    if p.print {
        *print = program(p);
        print.add(loft(&print_outer, -pt, 0.0, "print"));
        for o in &l.openings { print.sub(loft(o, -1.0, 1.0, "opening")); }
        for d in &l.dots { print.add(loft(d, -pt, 0.0, "dot")); }
        for h in &l.holes { print.sub(loft(h, -1.0, 1.0, "hole")); }
        if p.seg_ink() == SegInk::Diffuser && !l.segs.is_empty() {
            *ink = program(p);
            for s in &l.segs { ink.add(loft(s, -pt, 0.0, "diffuser")); }
            for h in &l.holes { ink.sub(loft(h, -1.0, 1.0, "hole")); }
        }
    }

    let inner = Placed { poly: l.adhesive_inner.clone(), ..Placed::default() };
    if p.adhesive {
        *adhesive = program(p);
        adhesive.add(loft(&Placed { poly: l.adhesive_outer.clone(), ..Placed::default() }, -pt - at, -pt, "adhesive"));
        adhesive.sub(loft(&inner, -pt - at - 1.0, 1.0, "adhesive inner"));
        for h in &l.holes { adhesive.sub(loft(h, -pt - at - 1.0, 1.0, "hole")); }
    }

    let (mut x0, mut x1, mut y0, mut y1) = (1e9_f64, -1e9_f64, 1e9_f64, -1e9_f64);
    for q in &l.outline {
        x0 = x0.min(q[0]); x1 = x1.max(q[0]); y0 = y0.min(q[1]); y1 = y1.max(q[1]);
    }
    r.size_x = x1 - x0;
    r.size_y = y1 - y0;
    let mut area = poly_area(&l.outline);
    for h in &l.holes { area -= poly_area(&h.poly); }
    r.area = area;
    if p.viewport() == Viewport::Round {
        r.view_w = p.view_d; r.view_h = p.view_d;
    } else if p.viewport() == Viewport::Rect {
        r.view_w = p.view_w; r.view_h = p.view_h;
    }
    if p.seg_count > 1 {
        let full = p.seg_span >= 359.99;
        r.seg_pitch = p.seg_ring_d * 0.5 * deg(if full { 360.0 / p.seg_count as f64 } else { p.seg_span / (p.seg_count - 1) as f64 });
        r.seg_gap = if l.segs.len() > 1 { poly_dist(&l.segs[0], &l.segs[1]) } else { 0.0 };
    }
    if p.adhesive { r.adhesive_area = poly_area(&l.adhesive_outer) - poly_area(&l.adhesive_inner); }
    r.print_top = 0.0;
    if !l.dots.is_empty() {
        r.dots = l.dots.len() as i32;
        r.dot_min = 1e9;
        for d in &l.dots {
            let dd = 2.0 * d.poly[0][0].hypot(d.poly[0][1]);
            r.dot_min = r.dot_min.min(dd); r.dot_max = r.dot_max.max(dd);
        }
    }
    r.adhesive_bottom = -pt - at;

    r.min_web = 1e9;
    for i in 0..l.holes.len() {
        if !within(&l.holes[i], &outl, 0.0) {
            warn(if i == 0 && p.hole() != HoleType::None { "centre hole runs off the outline" } else { "a mounting hole runs off the outline" });
            continue;
        }
        r.min_web = r.min_web.min(poly_dist(&l.holes[i], &outl));
        for j in i + 1..l.holes.len() { r.min_web = r.min_web.min(poly_dist(&l.holes[i], &l.holes[j])); }
    }
    if r.min_web > 1e8 { r.min_web = 0.0; }
    else {
        let need = 1.0_f64.max(t);
        if r.min_web < need { warn(&format!("only {:.2} mm of material beside a hole (keep {need:.1} mm or the thickness, whichever is more)", r.min_web)); }
    }
    for (i, opening) in l.openings.iter().enumerate() {
        let view = i == 0 && p.viewport() != Viewport::None;
        if !within(opening, &print_outer, 0.4) {
            warn(if view { "viewport reaches within 0.4 mm of the print's edge (or past it)" } else { "a light segment reaches the print's edge" });
            if !view { break; }
        }
    }
    if !l.segs.is_empty() {
        if l.segs.len() > 1 && r.seg_gap < 0.3 {
            if r.seg_gap <= 0.0 { warn("light segments overlap: fewer, narrower or a larger ring"); }
            else { warn(&format!("only {:.2} mm of print between segments (0.3 mm or more prints cleanly)", r.seg_gap)); }
        }
        for h in &l.holes {
            if l.segs.iter().any(|s| overlaps(s, h) || poly_dist(s, h) < 0.3) {
                warn("light segments run into a hole"); break;
            }
        }
        if p.viewport() != Viewport::None && l.segs.iter().any(|s| overlaps(s, &l.openings[0])) {
            note("light segments overlap the viewport");
        }
    }
    if p.dither && p.print && p.viewport() != Viewport::None {
        if l.dots.is_empty() { warn("dithered edge: no dots (the fade is narrower than the pitch, or every dot is under the smallest)"); }
        else { note(&format!("dithered edge: {} dots, {:.2} to {:.2} mm", r.dots, r.dot_min, r.dot_max)); }
        if p.dither_min < 0.1 { note("dots or gaps under 0.1 mm: beyond screen print, use UV inkjet or a photo-etched pad"); }
        if r.dots > 20000 { note("over 20000 dots: a heavy model (a coarser pitch or a narrower fade builds faster)"); }
    }
    if p.adhesive {
        if p.adhesive_inset + p.adhesive_width >= r.size_x.min(r.size_y) * 0.5 { warn("adhesive ring wider than the window: it closes up"); }
        for (i, opening) in l.openings.iter().enumerate() {
            if !within(opening, &inner, 0.0) {
                warn(if i == 0 && p.viewport() != Viewport::None { "adhesive covers part of the viewport" } else { "adhesive covers light segments" });
                break;
            }
        }
        if !p.print { note("no back print: the adhesive shows through the window"); }
    }
    if p.hole() != HoleType::None && p.viewport() != Viewport::None && overlaps(&l.holes[0], &l.openings[0]) {
        note("the centre hole is inside the viewport");
    }
    let sc = p.screen();
    if sc != Screen::Custom && sc != Screen::LedRing25 {
        let d = screen_data(sc);
        if p.viewport() == Viewport::None { warn(&format!("no viewport: the print hides the {} display", d.name)); }
        else {
            let act = Placed {
                poly: if d.round { circle(n, d.active_w * 0.5) } else { rounded_rect(d.active_w, d.active_h, 0.02, 4) },
                cx: d.active_x, cy: d.active_y,
            };
            if !within(&act, &l.openings[0], 0.0) { warn("viewport crops the display's active area"); }
        }
        let gw = d.glass_w;
        let gh = if d.round { d.glass_w } else { d.glass_h };
        if gw > r.size_x + 0.01 || gh > r.size_y + 0.01 { note("the display glass is larger than the window"); }
    }
    let material = p.material.name();
    if material == "pc" { note("PC: CNC-route or die-cut it (a laser chars and yellows the edges)"); }
    else if material == "pmma" {
        note("PMMA: use cast sheet for laser cutting (flame-polished edges)");
        if t < 1.0 { warn("PMMA under 1 mm is brittle (cracks at holes and when flexed): use PC"); }
    }
    if p.edge() != EdgeStyle::None && material == "pmma" { note("a chamfered / rounded edge is CNC work: a laser cuts square"); }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csg::{CsgOp, Shape};
    use fab_materials::MaterialId;

    fn ring() -> WindowParams {
        let mut p = WindowParams { screen: Screen::LedRing25.index(), ..WindowParams::default() };
        apply_screen(&mut p);
        p
    }

    #[test]
    fn presets_and_features_have_expected_warnings() {
        let mut cases = Vec::new();
        for screen in 1..Screen::NAMES.len() as i32 {
            let mut p = WindowParams { screen, ..WindowParams::default() };
            apply_screen(&mut p);
            cases.push((Screen::NAMES[screen as usize], p));
        }
        let mut p = ring();
        p.seg_ink = SegInk::Diffuser.index();
        cases.push(("wedge diffuser", p));
        let mut p = ring();
        p.seg_count = 16;
        p.seg_width = 2.4;
        p.seg_length = 3.0;
        p.seg_round = SegRound::Inner.index();
        p.seg_corner = 5.0;
        p.notch_end = NotchEnd::Round.index();
        cases.push(("fully rounded inner wedge", p.clone()));
        p.notch_end = NotchEnd::Square.index();
        p.seg_round = SegRound::Both.index();
        p.seg_corner = 0.3;
        cases.push(("both wedge ends rounded", p));
        let mut p = ring();
        p.seg_shape = SegShape::Dot.index();
        p.seg_count = 11;
        p.seg_span = 270.0;
        p.seg_start = -135.0;
        p.hole = HoleType::DFlat.index();
        p.flat_dist = 2.6;
        cases.push(("partial dot scale with D flat", p));
        let mut p = ring();
        p.seg_shape = SegShape::Slot.index();
        p.hole = HoleType::Notched1.index();
        p.notch_angle = 90.0;
        p.notch_end = NotchEnd::Round.index();
        cases.push(("slots with one round notch", p));
        let mut p = WindowParams {
            screen: Screen::Oled130.index(), border: 4.0,
            mount_holes: MountHoles::Four.index(), edge: EdgeStyle::Round.index(),
            edge_size: 0.6, material: MaterialId::lookup("pc").expect("PC material exists"),
            ..WindowParams::default()
        };
        apply_screen(&mut p);
        cases.push(("four mounting holes and round edge", p));
        cases.push(("plain sharp rectangle", WindowParams {
            screen: Screen::Custom.index(), outline: Outline::Rect.index(), width: 30.0,
            height: 20.0, corner: 0.0, viewport: Viewport::None.index(), print: false,
            adhesive: false, edge: EdgeStyle::None.index(), ..WindowParams::default()
        }));
        let mut p = WindowParams { dither: true, ..WindowParams::default() };
        cases.push(("round dither", p.clone()));
        p.screen = Screen::Oled130.index();
        apply_screen(&mut p);
        p.dither_width = 1.5;
        p.dither_pitch = 0.4;
        p.dither_min = 0.1;
        p.dither_overlap = 30.0;
        p.dither_curve = -1.0;
        cases.push(("rectangular dither", p));
        for (name, p) in cases {
            let built = build(&p);
            assert!(built.warnings.is_empty(), "{name} builds without warnings: {:?}", built.warnings);
            let l = layout(&p);
            let holes: f64 = l.holes.iter().map(|h| poly_area(&h.poly)).sum();
            assert!((poly_area(&l.outline) - holes - built.report.area).abs() < 0.001,
                "{name} report area is the outline less all hole areas");
            assert!(!built.window.steps.is_empty(), "{name} has a window body");
        }
        let mut bad = ring();
        bad.seg_count = 60;
        bad.hole_d = 21.0;
        bad.notch_span = 23.0;
        let built = build(&bad);
        assert!(built.warnings.len() >= 2, "bad LED ring reports at least two clearance warnings: {:?}", built.warnings);
    }

    #[test]
    fn polygon_areas_and_hole_shapes() {
        let pi = std::f64::consts::PI;
        let radius = 3.5;
        let n = 192;
        let c = circle(n, radius);
        let circle_area = pi * radius.powi(2);
        let circle_error = circle_area * (2.0 * pi / n as f64).powi(2) / 6.0;
        assert!((poly_area(&c) - circle_area).abs() <= circle_error + 1e-5,
            "circle area agrees with pi r squared within its segment error");
        let (width, height, corner, k) = (30.0, 20.0, 2.0, 12);
        let rect = rounded_rect(width, height, corner, k);
        let rect_area = width * height - (4.0 - pi) * corner.powi(2);
        let rect_error = pi * corner.powi(2) * (pi * 0.5 / (k - 1) as f64).powi(2) / 6.0;
        assert!((poly_area(&rect) - rect_area).abs() <= rect_error + 1e-4,
            "rounded rectangle area agrees with its closed form within its corner segment error");

        let mut p = WindowParams { notch_angle: 90.0, ..WindowParams::default() };
        assert!(layout(&p).holes.is_empty(), "no hole type leaves the layout without a centre cut");
        p.hole = HoleType::Round.index();
        let round = hole_poly(&p, n);
        assert_eq!(round.len(), n as usize, "round hole uses the circle resolution");
        assert!(round.iter().all(|q| (q[0].hypot(q[1]) - radius).abs() < 1e-6),
            "round hole points lie on the specified radius");
        for hole in [HoleType::Notched1, HoleType::Notched2] {
            p.hole = hole.index();
            p.notch_end = NotchEnd::Square.index();
            let square = hole_poly(&p, n);
            let tips = square.iter().filter(|q| q[0].hypot(q[1]) > radius + 0.01).count();
            assert_eq!(tips, if hole == HoleType::Notched1 { 2 } else { 4 },
                "{hole:?} adds two outer corner points per square notch");
            assert!(poly_area(&square) > poly_area(&round), "{hole:?} removes more sheet than a round hole");
            p.notch_end = NotchEnd::Round.index();
            let rounded = hole_poly(&p, n);
            assert!(rounded.len() > square.len(), "{hole:?} round notch ends add arc points");
            assert!(poly_area(&rounded) > 0.0, "{hole:?} has counter-clockwise winding");
        }
        p.hole = HoleType::DFlat.index();
        let flat = hole_poly(&p, n);
        let first = flat[0];
        let last = *flat.last().unwrap();
        assert!((first[0] - p.flat_dist).abs() < 1e-5 && (last[0] - p.flat_dist).abs() < 1e-5,
            "D flat closes with a straight run at the specified distance");
        assert!(first[1] > 0.0 && last[1] < 0.0 && (first[1] + last[1]).abs() < 1e-5,
            "D flat straight run joins opposite ends of the circular arc");
        assert!(flat.iter().all(|q| q[0] <= p.flat_dist + 1e-5) && poly_area(&flat) < poly_area(&round),
            "D flat clips the circle on the notch-facing side");
        for outline in [Outline::Round, Outline::Rect] {
            p.outline = outline.index();
            for mount in [MountHoles::Two, MountHoles::Four] {
                p.mount_holes = mount.index();
                let l = layout(&p);
                assert_eq!(l.holes.len(), 1 + if mount == MountHoles::Two { 2 } else { 4 },
                    "{outline:?}/{mount:?} adds the requested mounting holes beside the centre cut");
                let built = build(&p);
                let mut expected = poly_area(&l.outline);
                for h in &l.holes { expected -= poly_area(&h.poly); }
                assert!((built.report.area - expected).abs() < 0.001,
                    "{outline:?}/{mount:?} sheet area subtracts the centre and mounting holes");
            }
        }
    }

    #[test]
    fn dither_bounds_and_report() {
        let mut p = WindowParams {
            screen: Screen::Custom.index(), dither: true, view_x: 0.7, view_y: -0.4,
            dither_pitch: 0.5, dither_min: 0.02, dither_width: 2.0,
            ..WindowParams::default()
        };
        for viewport in [Viewport::Round, Viewport::Rect] {
            p.viewport = viewport.index();
            for overlap in [0.0, 30.0] {
                p.dither_overlap = overlap;
                let l = layout(&p);
                let built = build(&p);
                assert!(!l.dots.is_empty(), "{viewport:?}/{overlap} dither produces dots");
                let view = &l.openings[0];
                let mut min = f64::INFINITY;
                let mut max = 0.0_f64;
                let mut boundary_dots = 0;
                for dot in &l.dots {
                    let centre = [dot.cx, dot.cy];
                    let edge_dist = (0..view.poly.len())
                        .map(|i| seg_dist(centre, at(view, i), at(view, (i + 1) % view.poly.len())))
                        .fold(f64::INFINITY, f64::min);
                    assert!(inside(view, centre) || edge_dist < 1e-4,
                        "{viewport:?}/{overlap} dot centres stay inside or on the viewport boundary");
                    assert!(edge_dist <= p.dither_width + 1e-4,
                        "{viewport:?}/{overlap} dots stay within the inward fade band");
                    if edge_dist < 1e-4 { boundary_dots += 1; }
                    let diameter = 2.0 * dot.poly[0][0].hypot(dot.poly[0][1]);
                    let upper = p.dither_pitch * (1.0 + overlap / 100.0);
                    assert!(diameter >= p.dither_min - 1e-6 && diameter <= upper + 1e-6,
                        "{viewport:?}/{overlap} dot diameters respect the minimum and pitch plus overlap");
                    min = min.min(diameter); max = max.max(diameter);
                }
                assert!(boundary_dots > 0, "{viewport:?}/{overlap} first dither row touches the mask edge");
                assert_eq!(built.report.dots as usize, l.dots.len(), "dither report counts every dot");
                assert_eq!(built.report.dot_min, min, "dither report gives the smallest dot diameter");
                assert_eq!(built.report.dot_max, max, "dither report gives the largest dot diameter");
                assert!(min < max, "dither dots shrink toward the viewport interior");
            }
        }
        p.dither = false;
        assert!(layout(&p).dots.is_empty(), "disabled dither produces no dots");
        p.dither = true;
        p.print = false;
        assert!(layout(&p).dots.is_empty(), "disabled print produces no dither dots");
        p.print = true;
        p.viewport = Viewport::None.index();
        assert!(layout(&p).dots.is_empty(), "absent viewport produces no dither dots");
    }

    #[test]
    fn segment_positions_pitch_and_gap() {
        let mut p = ring();
        p.seg_shape = SegShape::Dot.index();
        p.seg_count = 11;
        p.seg_start = -135.0;
        p.hole_x = 0.4;
        p.hole_y = -0.6;
        for span in [360.0, 270.0] {
            p.seg_span = span;
            let l = layout(&p);
            let built = build(&p);
            assert_eq!(l.segs.len(), p.seg_count as usize, "{span} degree ring places every segment");
            let step = if span == 360.0 { 360.0 / p.seg_count as f64 } else { span / (p.seg_count - 1) as f64 };
            let radius = p.seg_ring_d * 0.5;
            let pitch = radius * deg(step);
            assert!((built.report.seg_pitch - pitch).abs() < 1e-6, "{span} degree ring reports arc-length pitch");
            for (i, seg) in l.segs.iter().enumerate() {
                let clock = deg(p.seg_start + step * i as f64);
                assert!((seg.cx - p.hole_x - radius * clock.sin()).abs() < 1e-5
                    && (seg.cy - p.hole_y - radius * clock.cos()).abs() < 1e-5,
                    "{span} degree ring segment {i} follows the clock angle around the hole");
            }
            let last_clock = if span == 360.0 { p.seg_start + 360.0 - step } else { p.seg_start + span };
            let last = l.segs.last().unwrap();
            assert!((last.cx - p.hole_x - radius * deg(last_clock).sin()).abs() < 1e-5
                && (last.cy - p.hole_y - radius * deg(last_clock).cos()).abs() < 1e-5,
                "{span} degree ring ends at the partial span endpoint or one pitch short of a full turn");
            let chord_gap = 2.0 * radius * (deg(step) * 0.5).sin() - p.seg_width;
            let dot_error = p.seg_width * (1.0 - (PI / (p.segments / 4) as f64).cos());
            assert!((built.report.seg_gap - chord_gap).abs() <= dot_error + 1e-5,
                "{span} degree ring gap agrees with the circular dot gap within segment error");
        }
        p.seg_count = 1;
        let built = build(&p);
        let l = layout(&p);
        assert_eq!(l.segs.len(), 1, "a single segment is placed on a partial arc");
        assert!((l.segs[0].cx - p.hole_x - p.seg_ring_d * 0.5 * deg(p.seg_start).sin()).abs() < 1e-5,
            "a single segment stays at the start angle");
        assert_eq!((built.report.seg_pitch, built.report.seg_gap), (0.0, 0.0),
            "one segment has no neighbouring pitch or gap");
    }

    #[test]
    fn bodies_follow_options_and_stack_order() {
        let mut p = ring();
        p.segments = 48;
        for print in [false, true] {
            p.print = print;
            for adhesive in [false, true] {
                p.adhesive = adhesive;
                for ink in [SegInk::Clear, SegInk::Diffuser] {
                    p.seg_ink = ink.index();
                    for count in [0, 4] {
                        p.seg_count = count;
                        let b = build(&p);
                        assert!(!b.window.steps.is_empty(), "window body is always present");
                        assert_eq!(b.print.steps.is_empty(), !print, "print body is empty exactly when print is off");
                        assert_eq!(b.adhesive.steps.is_empty(), !adhesive, "adhesive body is empty exactly when adhesive is off");
                        assert_eq!(b.ink.steps.is_empty(), !(print && ink == SegInk::Diffuser && count > 0),
                            "diffuser body needs print, diffuser ink and at least one segment");
                        let pt = if print { p.print_thickness } else { 0.0 };
                        let at = if adhesive { p.adhesive_thickness } else { 0.0 };
                        assert_eq!(b.report.print_top, 0.0, "print top touches the window back face");
                        assert_eq!(b.report.adhesive_bottom, -pt - at, "adhesive bottom includes the enabled stack thicknesses");
                        for (body, z0, z1) in [(&b.window, 0.0, p.thickness), (&b.print, -pt, 0.0),
                            (&b.ink, -pt, 0.0), (&b.adhesive, -pt - at, -pt)] {
                            if body.steps.is_empty() { continue; }
                            assert_eq!(body.steps[0].op, CsgOp::Add, "each body starts by adding its solid");
                            assert!(body.step_chord_length, "bodies use chord-length STEP curves");
                            match &body.steps[0].prim.shape {
                                Shape::Loft { rings, prism, crease } => {
                                    assert!(*prism && crease == &[true, true], "straight body walls preserve exact prism corners");
                                    assert_eq!((rings[0].z, rings[1].z), (z0, z1), "body faces occupy the specified stack interval");
                                }
                                _ => panic!("body starts with a straight loft prism"),
                            }
                        }
                    }
                }
            }
        }
        p.seg_count = 0;
        p.hole = HoleType::None.index();
        for outline in [Outline::Round, Outline::Rect] {
            p.outline = outline.index();
            for edge in [EdgeStyle::Chamfer, EdgeStyle::Round] {
                p.edge = edge.index();
                let b = build(&p);
                match &b.window.steps[1].prim.shape {
                    Shape::Hull { pts } => assert!(outline == Outline::Rect && !pts.is_empty(),
                        "rectangular front edge uses a convex hull"),
                    Shape::Loft { rings, crease, prism } => {
                        assert_eq!(outline, Outline::Round, "round front edge uses a loft band");
                        assert!(!prism, "front edge band is not a straight prism");
                        assert_eq!(rings.len(), if edge == EdgeStyle::Chamfer { 2 } else { 7 },
                            "front edge uses one chamfer or six rounded bands");
                        assert_eq!(rings.len(), crease.len(), "front edge rings each carry a crease flag");
                        assert_eq!(rings.last().unwrap().z, p.thickness, "front edge ends at the sheet front face");
                    }
                    _ => panic!("front edge is a hull or loft band"),
                }
            }
        }
    }
}
