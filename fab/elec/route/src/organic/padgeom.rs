// Ported from TiNRS Psychedelic Router 0.1.0: src/core/PadGeometry.h.
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in this folder.

//! Exact pad copper and drill geometry in millimetres, with board Y down.
//! Local to board: centre + (lx * c + ly * s, -lx * s + ly * c).

use super::geom::{std_clamp, std_max, std_min, Vec2};
use super::model::{Pad, PadShape};

pub const K_DEG_D: f64 = 3.14159265358979323846 / 180.0;

/// Rounded box frame; polygon coordinates are local coordinates plus (ox, oy).
pub struct Frame<'a> {
    pub cx: f64,
    pub cy: f64,
    pub c: f64,
    pub s: f64,
    pub hx: f64,
    pub hy: f64,
    pub r: f64,
    /// Borrows the pad's ring; (cx, cy) is its bounding box centre.
    pub poly: Option<&'a [Vec2]>,
    pub ox: f64,
    pub oy: f64,
}

impl Default for Frame<'_> {
    fn default() -> Self {
        Self { cx: 0.0, cy: 0.0, c: 1.0, s: 0.0, hx: 0.0, hy: 0.0, r: 0.0, poly: None, ox: 0.0, oy: 0.0 }
    }
}

pub fn has_local(p: &Pad) -> bool {
    p.local_size.x > 1e-4_f32 && p.local_size.y > 1e-4_f32
}

pub fn frame(p: &Pad) -> Frame<'_> {
    let mut f = Frame::default();
    f.cx = p.center.x as f64;
    f.cy = p.center.y as f64;
    let loc = has_local(p);
    f.hx = (if loc { p.local_size.x } else { p.size.x }) as f64 * 0.5;
    f.hy = (if loc { p.local_size.y } else { p.size.y }) as f64 * 0.5;
    if loc && (p.rotation as f64).abs() % 360.0 > 1e-6 {
        let a = p.rotation as f64 * K_DEG_D;
        f.c = a.cos();
        f.s = a.sin();
        // Snap multiples of 90 degrees to keep axis-aligned pads exact.
        if f.c.abs() < 1e-9 { f.c = 0.0; }
        if f.s.abs() < 1e-9 { f.s = 0.0; }
    }
    match p.shape {
        PadShape::Circle => {
            f.hy = std_max(f.hx, f.hy);
            f.hx = f.hy;
            f.r = f.hx;
        }
        PadShape::Rect => f.r = 0.0,
        PadShape::RoundRect => {
            f.r = std_clamp(p.round_rect_ratio as f64, 0.0, 0.5) * 2.0 * std_min(f.hx, f.hy);
        }
        PadShape::Polygon => {
            if p.polygon.len() >= 3 {
                let (mut x0, mut y0, mut x1, mut y1) = (1e300, 1e300, -1e300, -1e300);
                for v in &p.polygon {
                    x0 = std_min(x0, v.x as f64);
                    x1 = std_max(x1, v.x as f64);
                    y0 = std_min(y0, v.y as f64);
                    y1 = std_max(y1, v.y as f64);
                }
                f.poly = Some(&p.polygon);
                f.r = 0.0;
                f.hx = 0.5 * (x1 - x0);
                f.hy = 0.5 * (y1 - y0);
                f.ox = 0.5 * (x0 + x1);
                f.oy = 0.5 * (y0 + y1);
                // Box centre in the board frame, as in to_board.
                f.cx = p.center.x as f64 + f.ox * f.c + f.oy * f.s;
                f.cy = p.center.y as f64 - f.ox * f.s + f.oy * f.c;
            } else {
                f.r = 0.0; // No ring: use the bounding rect.
            }
        }
        PadShape::Oval => f.r = std_min(f.hx, f.hy),
    }
    f
}

/// Board to local.
pub fn to_local(f: &Frame<'_>, x: f64, y: f64) -> (f64, f64) {
    let dx = x - f.cx;
    let dy = y - f.cy;
    (dx * f.c - dy * f.s, dx * f.s + dy * f.c)
}

/// Local to board.
pub fn to_board(f: &Frame<'_>, lx: f64, ly: f64) -> Vec2 {
    Vec2::new((f.cx + lx * f.c + ly * f.s) as f32, (f.cy - lx * f.s + ly * f.c) as f32)
}

pub fn point_seg_d(px: f64, py: f64, ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    let dx = bx - ax;
    let dy = by - ay;
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 1e-18 { std_clamp(((px - ax) * dx + (py - ay) * dy) / l2, 0.0, 1.0) } else { 0.0 };
    let ex = ax + dx * t - px;
    let ey = ay + dy * t - py;
    (ex * ex + ey * ey).sqrt()
}

pub fn seg_cross(ax: f64, ay: f64, bx: f64, by: f64, cx: f64, cy: f64, dx: f64, dy: f64) -> bool {
    let o = |px: f64, py: f64, qx: f64, qy: f64, rx: f64, ry: f64| {
        (qx - px) * (ry - py) - (qy - py) * (rx - px)
    };
    let d1 = o(ax, ay, bx, by, cx, cy);
    let d2 = o(ax, ay, bx, by, dx, dy);
    let d3 = o(cx, cy, dx, dy, ax, ay);
    let d4 = o(cx, cy, dx, dy, bx, by);
    ((d1 > 0.0) != (d2 > 0.0)) && ((d3 > 0.0) != (d4 > 0.0)) && d1 != 0.0 && d2 != 0.0 && d3 != 0.0 && d4 != 0.0
}

pub fn seg_seg_d(ax: f64, ay: f64, bx: f64, by: f64, cx: f64, cy: f64, dx: f64, dy: f64) -> f64 {
    if seg_cross(ax, ay, bx, by, cx, cy, dx, dy) { return 0.0; }
    std_min(std_min(std_min(
        point_seg_d(ax, ay, cx, cy, dx, dy),
        point_seg_d(bx, by, cx, cy, dx, dy)),
        point_seg_d(cx, cy, ax, ay, bx, by)),
        point_seg_d(dx, dy, ax, ay, bx, by))
}

/// Local point to the box [-ix, ix] × [-iy, iy], zero inside.
pub fn point_box_local(lx: f64, ly: f64, ix: f64, iy: f64) -> f64 {
    let dx = std_max(0.0, lx.abs() - ix);
    let dy = std_max(0.0, ly.abs() - iy);
    (dx * dx + dy * dy).sqrt()
}

pub fn seg_box_local(ax: f64, ay: f64, bx: f64, by: f64, ix: f64, iy: f64) -> f64 {
    let mut d = std_min(point_box_local(ax, ay, ix, iy), point_box_local(bx, by, ix, iy));
    if d <= 0.0 { return 0.0; }
    if ix <= 0.0 && iy <= 0.0 { return point_seg_d(0.0, 0.0, ax, ay, bx, by); }
    let q = [[-ix, -iy], [ix, -iy], [ix, iy], [-ix, iy]];
    for i in 0..4 {
        let u = q[i];
        let v = q[(i + 1) % 4];
        d = std_min(d, seg_seg_d(ax, ay, bx, by, u[0], u[1], v[0], v[1]));
    }
    d
}

/// Even-odd point in a pad-local ring.
pub fn poly_contains(p: &[Vec2], x: f64, y: f64) -> bool {
    let mut inside = false;
    let n = p.len();
    let mut j = n.wrapping_sub(1);
    for i in 0..n {
        let yi = p[i].y as f64;
        let yj = p[j].y as f64;
        if (yi > y) != (yj > y) {
            let xi = p[i].x as f64 + (y - yi) * (p[j].x as f64 - p[i].x as f64) / (yj - yi);
            if x < xi { inside = !inside; }
        }
        j = i;
    }
    inside
}

/// Point to ring boundary distance.
pub fn poly_edge_distance(p: &[Vec2], x: f64, y: f64) -> f64 {
    let mut d = 1e300;
    let n = p.len();
    let mut j = n.wrapping_sub(1);
    for i in 0..n {
        d = std_min(d, point_seg_d(x, y, p[j].x as f64, p[j].y as f64, p[i].x as f64, p[i].y as f64));
        j = i;
    }
    d
}

/// Exact signed distance to the ring, negative inside.
pub fn poly_signed(p: &[Vec2], x: f64, y: f64) -> f64 {
    let d = poly_edge_distance(p, x, y);
    if poly_contains(p, x, y) { -d } else { d }
}

/// Segment to ring: negative endpoint depth inside, zero on crossing, else the gap.
pub fn poly_segment(p: &[Vec2], ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    let ia = poly_contains(p, ax, ay);
    let ib = poly_contains(p, bx, by);
    if ia || ib {
        return -std_max(if ia { poly_edge_distance(p, ax, ay) } else { 0.0 }, if ib { poly_edge_distance(p, bx, by) } else { 0.0 });
    }
    let mut d = 1e300;
    let n = p.len();
    let mut j = n.wrapping_sub(1);
    let mut i = 0;
    while i < n && d > 0.0 {
        d = std_min(d, seg_seg_d(ax, ay, bx, by, p[j].x as f64, p[j].y as f64, p[i].x as f64, p[i].y as f64));
        j = i;
        i += 1;
    }
    d
}

/// Board point to copper, <= 0 inside (bounded by -radius; polygons are exact signed).
pub fn point_distance(f: &Frame<'_>, x: f64, y: f64) -> f64 {
    let (lx, ly) = to_local(f, x, y);
    if let Some(poly) = f.poly { return poly_signed(poly, lx + f.ox, ly + f.oy); }
    point_box_local(lx, ly, std_max(0.0, f.hx - f.r), std_max(0.0, f.hy - f.r)) - f.r
}

pub fn point_distance_pad(p: &Pad, x: f64, y: f64) -> f64 {
    point_distance(&frame(p), x, y)
}

/// Board segment to copper, with the same sign convention as point_distance.
pub fn segment_distance(f: &Frame<'_>, ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    let (lax, lay) = to_local(f, ax, ay);
    let (lbx, lby) = to_local(f, bx, by);
    if let Some(poly) = f.poly { return poly_segment(poly, lax + f.ox, lay + f.oy, lbx + f.ox, lby + f.oy); }
    seg_box_local(lax, lay, lbx, lby, std_max(0.0, f.hx - f.r), std_max(0.0, f.hy - f.r)) - f.r
}

pub fn segment_distance_pad(p: &Pad, ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    segment_distance(&frame(p), ax, ay, bx, by)
}

pub fn contains(p: &Pad, q: Vec2, tol: f32) -> bool {
    point_distance_pad(p, q.x as f64, q.y as f64) <= tol as f64
}

/// Exact signed copper distance, including depth inside; clearance uses point_distance.
pub fn signed_distance(p: &Pad, x: f64, y: f64) -> f64 {
    let f = frame(p);
    let (lx, ly) = to_local(&f, x, y);
    if let Some(poly) = f.poly { return poly_signed(poly, lx + f.ox, ly + f.oy); }
    let ix = std_max(0.0, f.hx - f.r);
    let iy = std_max(0.0, f.hy - f.r);
    let qx = lx.abs() - ix;
    let qy = ly.abs() - iy;
    std_max(qx, 0.0).hypot(std_max(qy, 0.0)) + std_min(std_max(qx, qy), 0.0) - f.r
}

/// Drill capsule axis and radius; coincident endpoints describe a round hole.
#[derive(Copy, Clone, Debug, Default)]
pub struct Hole {
    pub a: Vec2,
    pub b: Vec2,
    pub r: f32,
}

impl Hole {
    pub fn centre(&self) -> Vec2 { (self.a + self.b) * 0.5_f32 }
    pub fn half_length(&self) -> f32 { self.a.distance_to(self.b) * 0.5_f32 }
    pub fn slot(&self) -> bool { self.half_length() > 1e-5_f32 }
}

/// Through-hole drill capsule; None for SMD pads or pads without a drill.
pub fn hole(p: &Pad) -> Option<Hole> {
    if p.is_smd || p.drill_diameter <= 0.0_f32 { return None; }
    let e = p.drill_extent();
    let mut h = Hole { a: p.center, b: p.center, r: 0.5_f32 * std_min(e.x, e.y) };
    if h.r <= 0.0_f32 { return None; }
    let half = 0.5 * (e.x as f64 - e.y as f64).abs();
    if half < 1e-5 { return Some(h); }
    let a = (if has_local(p) { p.rotation } else { 0.0_f32 }) as f64 * K_DEG_D;
    let c = a.cos();
    let s = a.sin();
    // Local x maps to (c, -s); local y maps to (s, c).
    let ux = if e.x > e.y { c } else { s };
    let uy = if e.x > e.y { -s } else { c };
    h.a = Vec2::new((p.center.x as f64 - ux * half) as f32, (p.center.y as f64 - uy * half) as f32);
    h.b = Vec2::new((p.center.x as f64 + ux * half) as f32, (p.center.y as f64 + uy * half) as f32);
    Some(h)
}

/// Point to hole wall distance, negative inside.
pub fn hole_point_distance(h: &Hole, x: f64, y: f64) -> f64 {
    point_seg_d(x, y, h.a.x as f64, h.a.y as f64, h.b.x as f64, h.b.y as f64) - h.r as f64
}

/// Segment centre line to hole wall distance.
pub fn hole_segment_distance(h: &Hole, ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    seg_seg_d(ax, ay, bx, by, h.a.x as f64, h.a.y as f64, h.b.x as f64, h.b.y as f64) - h.r as f64
}

/// Wall-to-wall distance between holes.
pub fn hole_hole_distance(h: &Hole, g: &Hole) -> f64 {
    seg_seg_d(h.a.x as f64, h.a.y as f64, h.b.x as f64, h.b.y as f64, g.a.x as f64, g.a.y as f64, g.b.x as f64, g.b.y as f64) - h.r as f64 - g.r as f64
}
