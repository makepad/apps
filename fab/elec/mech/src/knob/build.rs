//! Knob solids, separate cap and pointer bodies, and manufacturing warnings.

use super::params::*;
use crate::csg::{Csg, Prim, Ring};
use std::f64::consts::PI;

const TAU: f64 = 2.0 * PI;
const SC_BLEND: f64 = 0.2;

fn deg(d: f64) -> f64 { d * PI / 180.0 }
fn clampf(v: f64, lo: f64, hi: f64) -> f64 { lo.max(hi.min(v)) }
fn smooth01(t: f64) -> f64 {
    let t = clampf(t, 0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
// Symmetric remainder, with ties rounded to the even quotient.
fn remainder(x: f64, y: f64) -> f64 {
    let r = x % y;
    let half = y * 0.5;
    if r.abs() > half || (r.abs() == half && (x / y).trunc() % 2.0 != 0.0) {
        r - y.copysign(r)
    } else { r }
}
fn circle(n: usize, r: f64, z: f64) -> Ring {
    Ring { z, pts: (0..n).map(|i| {
        [r * (TAU * i as f64 / n as f64).cos(), r * (TAU * i as f64 / n as f64).sin()]
    }).collect(), ..Ring::default() }
}
fn program(segments: i32) -> Csg { Csg { segments, ..Csg::default() } }

#[derive(Default)]
struct Loft {
    rings: Vec<Ring>,
    crease: Vec<bool>,
}
impl Loft {
    fn add(&mut self, ring: Ring, crease: bool) {
        self.rings.push(ring);
        self.crease.push(crease);
    }
    fn end(&mut self) { *self.crease.last_mut().unwrap() = true; }
    fn prim(self, name: &str) -> Prim { Prim::loft(name, self.rings).crease(self.crease) }
}

struct Gen<'a> {
    p: &'a KnobParams,
    n: usize,
    r: f64,
    h: f64,
    z0: f64,
    cove: f64,
    th: Vec<f64>,
    e_top: f64,
    c_bot: f64,
    zb: f64,
    sc_n: i32,
    sc_w: f64,
    sc_d: f64,
    sc_rc: f64,
    sc_za: f64,
    sc_zb: f64,
    sc_pitch: f64,
    sc_phase: f64,
    sc_skip: i32,
    sc_ball_bottom: bool,
    sc_top: ScallopTop,
    sc_tan_ramp: f64,
    notes: Vec<String>,
}
impl<'a> Gen<'a> {
    fn new(p: &'a KnobParams) -> Self {
        let mut g = Self {
            p, n: p.segments.max(32) as usize, r: p.diameter * 0.5, h: p.height,
            z0: if p.skirt { p.skirt_height.min(p.height * 0.6) } else { 0.0 },
            cove: 0.0, th: Vec::new(), e_top: 0.0, c_bot: 0.0, zb: 0.0,
            sc_n: 0, sc_w: 0.0, sc_d: 0.0, sc_rc: 0.0, sc_za: 0.0, sc_zb: 0.0,
            sc_pitch: 1.0, sc_phase: 0.0, sc_skip: -1, sc_ball_bottom: true,
            sc_top: ScallopTop::Open, sc_tan_ramp: 1.0, notes: Vec::new(),
        };
        if p.skirt {
            let room = g.skirt_apothem(g.z0) - p.skirt_edge.min(g.z0 * 0.45) - p.diameter * 0.5 - 0.2;
            g.cove = 0.0_f64.max(p.skirt_blend.min(room).min((g.h - g.z0) * 0.5));
        }
        g.zb = if p.skirt { 0.0_f64.max(g.z0 - 0.05) } else { 0.0 };
        g.c_bot = if p.skirt { 0.0 } else { p.bottom_edge.min(g.h * 0.3) };
        g.e_top = 0.0_f64.max(p.top_edge.min((g.h - g.zb - g.c_bot - 0.2).min(g.r * p.taper * 0.8)));
        if p.grip_style() == GripStyle::Scallops && p.ridge_depth > 0.0 && p.ridges > 0 { g.setup_scallops(); }
        g.th = (0..g.n).map(|i| TAU * i as f64 / g.n as f64).collect();
        g
    }
    fn setup_scallops(&mut self) {
        let p = self.p;
        self.sc_n = p.ridges;
        self.sc_pitch = TAU / self.sc_n as f64;
        self.sc_w = 0.3_f64.max(p.scallop_width);
        self.sc_d = p.ridge_depth;
        if self.sc_d > self.sc_w * 0.5 {
            self.sc_d = self.sc_w * 0.5;
            self.notes.push("scallop depth limited to half the width (a deeper cut would undercut its own edges)".into());
        }
        self.sc_rc = (self.sc_w * self.sc_w * 0.25 + self.sc_d * self.sc_d) / (2.0 * self.sc_d);
        self.sc_ball_bottom = p.scallop_bottom() == ScallopBottom::Ball;
        self.sc_top = p.scallop_top();
        self.sc_tan_ramp = deg(clampf(p.scallop_ramp, 3.0, 75.0)).tan();
        let span = self.h - self.z0;
        let lo = if p.skirt { self.z0 + self.cove + 0.1 } else { 0.0 };
        let hi = self.h - self.e_top - 0.3;
        let from = lo.max(self.z0 + p.grip_from * span);
        let mut to = if self.sc_top == ScallopTop::Open { self.h + 10.0 }
            else { hi.min(self.z0 + p.grip_to.max(p.grip_from + 0.02) * span) };
        if to < from + 0.2 { to = from + 0.2; }
        self.sc_za = from + if self.sc_ball_bottom { self.sc_w * 0.5 } else { 0.0 };
        self.sc_zb = match self.sc_top {
            ScallopTop::Open => to,
            ScallopTop::Ball => to - self.sc_w * 0.5,
            ScallopTop::Ramp => to - self.sc_d / self.sc_tan_ramp,
        };
        if self.sc_zb < self.sc_za {
            self.sc_zb = 0.5 * (from + to.min(self.h));
            self.sc_za = self.sc_zb;
        }
        self.sc_phase = deg(p.scallop_phase);
        if p.scallop_skip {
            let mut best = 1e9;
            for k in 0..self.sc_n {
                let d = remainder(self.sc_phase + (k as f64 + 0.5) * self.sc_pitch, TAU).abs();
                if d < best { best = d; self.sc_skip = k; }
            }
        }
        let r_max = self.r + 0.0_f64.max(p.bulge);
        let need = (30.0 * TAU * r_max / self.sc_w).ceil() as usize;
        let n0 = self.n;
        if self.n < need { self.n = need.min(1440); }
        let count = self.sc_n as usize;
        self.n = (self.n + count - 1) / count * count;
        if self.n != n0 { self.notes.push(format!("segments raised to {} for the scallops", self.n)); }
        if self.sc_w >= TAU * self.r * p.taper / self.sc_n as f64 {
            self.notes.push("scallops wider than their pitch: they merge".into());
        }
    }
    fn scallops(&self) -> bool { self.sc_n > 0 }
    fn scallop_cut(&self, a: f64, z: f64, r: f64) -> f64 {
        let u = (a - self.sc_phase) / self.sc_pitch - 0.5;
        let k0 = u.floor() as i32;
        let mut cut = 0.0_f64;
        let mut dz = 0.0;
        let mut d = self.sc_d;
        if z < self.sc_za {
            if !self.sc_ball_bottom { return 0.0; }
            dz = self.sc_za - z;
        } else if z > self.sc_zb {
            if self.sc_top == ScallopTop::Ball { dz = z - self.sc_zb; }
            else if self.sc_top == ScallopTop::Ramp {
                d = self.sc_d - (z - self.sc_zb) * self.sc_tan_ramp;
                if d < -SC_BLEND { return 0.0; }
            }
        }
        for dk in 0..=1 {
            let k = (k0 + dk).rem_euclid(self.sc_n);
            if k == self.sc_skip { continue; }
            let lat = r * remainder(a - self.sc_phase - (k as f64 + 0.5) * self.sc_pitch, TAU);
            let sd = (lat * lat + dz * dz).sqrt();
            if sd >= self.sc_rc + SC_BLEND { continue; }
            let raw = if sd < self.sc_rc { (self.sc_rc * self.sc_rc - sd * sd).sqrt() - (self.sc_rc - d) }
                else { -(self.sc_rc - d) - (sd - self.sc_rc) };
            let h = clampf(0.5 + 0.5 * raw / SC_BLEND, 0.0, 1.0);
            cut = cut.max(raw * h + SC_BLEND * h * (1.0 - h));
        }
        cut.max(0.0)
    }
    fn crease_at(&self, z: f64) -> bool {
        self.scallops() && !self.sc_ball_bottom && [self.sc_za - 0.03, self.sc_za].iter().any(|c| (z - c).abs() < 1e-4)
    }
    fn lobe(&self, a: f64) -> f64 {
        let p = self.p;
        let l = p.lobes as f64;
        let s = p.lobe_shape;
        match p.lobe_style() {
            LobeStyle::Sine | LobeStyle::Star => {
                let g = if p.lobe_style() == LobeStyle::Sine { 0.5 + 0.5 * (l * a).cos() }
                    else { let u = (l * a / TAU + 1000.5) % 1.0; 1.0 - (u - 0.5).abs() * 2.0 };
                let e = (s - 0.5) * 2.0;
                if e >= 0.0 { 1.0 - (1.0 - g).powf(1.0 + 4.0 * e) } else { g.powf(1.0 - 4.0 * e) }
            }
            LobeStyle::Crenel => {
                let k = 0.04 + 0.9 * s;
                smooth01(((l * a).cos() + k) / (2.0 * k))
            }
            _ => 1.0,
        }
    }
    fn shape_r(&self, a: f64) -> f64 {
        let p = self.p;
        if p.lobes < 2 { return self.r; }
        let l = p.lobes as f64;
        match p.lobe_style() {
            LobeStyle::Scallop => {
                let w = 0.15 + 0.5 * p.lobe_shape;
                let rc = self.r * w * (PI / l).sin() * 2.0;
                let c = self.r - p.lobe_depth + rc;
                let u = (a / TAU * l + 1000.0) % 1.0;
                let phi = (u - 0.5) * TAU / l;
                let disc = rc * rc - c * c * phi.sin() * phi.sin();
                if disc <= 0.0 { return self.r; }
                clampf(c * phi.cos() - disc.sqrt(), self.r * 0.2, self.r)
            }
            LobeStyle::Polygon => {
                let half = PI / l;
                let rc = 0.01_f64.max(p.lobe_shape * self.r * 0.6);
                let ap = (self.r - rc) * half.cos();
                let u = (a / TAU * l + 1000.0) % 1.0;
                let phi = (u - 0.5) * TAU / l;
                let t = (ap + rc) / phi.cos();
                if (t * phi.sin()).abs() <= ap * half.tan() { return t; }
                let cx = ap;
                let cy = ap * half.tan() * if phi < 0.0 { -1.0 } else { 1.0 };
                let uc = phi.cos() * cx + phi.sin() * cy;
                uc + 0.0_f64.max(uc * uc - (cx * cx + cy * cy) + rc * rc).sqrt()
            }
            _ => self.r - p.lobe_depth * (1.0 - self.lobe(a)),
        }
    }
    fn grip_mask(&self, zf: f64) -> f64 {
        let p = self.p;
        let a = p.grip_from;
        let b = p.grip_to.max(a + 0.02);
        let f = 0.06_f64.min((b - a) * 0.25);
        let open = p.grip_style() == GripStyle::Flutes && p.flute_top == 0;
        smooth01((zf - a) / f) * if open { 1.0 } else { smooth01((b - zf) / f) }
    }
    fn skirt_apothem(&self, z: f64) -> f64 {
        let p = self.p;
        let side_top = self.z0 - p.skirt_edge.min(self.z0 * 0.45);
        p.skirt_diameter * 0.5 - 0.0_f64.max(z.min(side_top)) * deg(p.skirt_taper).tan()
    }
    fn skirt_r(&self, a: f64, apo: f64) -> f64 {
        let p = self.p;
        if p.skirt_facets < 3 { return apo; }
        let k = p.skirt_facets as f64;
        let half = PI / k;
        let rc = p.skirt_facet_round.max(0.0).min(apo * 0.9);
        let ap = apo - rc;
        let u = (a / TAU * k + 1000.5) % 1.0 - 0.5;
        let phi = u * TAU / k;
        let t = apo / phi.cos();
        if (t * phi.sin()).abs() <= ap * half.tan() { return t; }
        let cx = ap;
        let cy = ap * half.tan() * if phi < 0.0 { -1.0 } else { 1.0 };
        let uc = phi.cos() * cx + phi.sin() * cy;
        uc + 0.0_f64.max(uc * uc - (cx * cx + cy * cy) + rc * rc).sqrt()
    }
    fn skirt_ring(&self, z: f64, inset: f64) -> Ring {
        let apo = self.skirt_apothem(z) - inset;
        Ring { z, pts: self.th.iter().map(|&a| {
            let r = 0.2_f64.max(self.skirt_r(a, apo)); [r * a.cos(), r * a.sin()]
        }).collect(), ..Ring::default() }
    }
    fn runout(&self, z: f64) -> f64 {
        if self.cove > 0.0 { smooth01((z - self.z0) / self.cove) } else { 1.0 }
    }
    fn outer_r(&self, a: f64, z: f64) -> f64 {
        let p = self.p;
        let zf = clampf((z - self.z0) / 0.01_f64.max(self.h - self.z0), 0.0, 1.0);
        let tw = deg(p.twist) * zf;
        let ro = self.runout(z);
        let mut r = self.r + (self.shape_r(a - tw) - self.r) * ro;
        r *= 1.0 + (p.taper - 1.0) * zf;
        r += p.bulge * (PI * zf).sin();
        if self.scallops() && ro > 0.0 { r -= ro * self.scallop_cut(a - tw, z, r); }
        let m = if self.scallops() { 0.0 } else { self.grip_mask(zf) * ro };
        if m > 0.0 && p.ridge_depth > 0.0 {
            let k = p.ridges as f64;
            match p.grip_style() {
                GripStyle::Flutes => { r -= p.ridge_depth * m * (0.5 + 0.5 * (k * (a - tw)).cos()).powf(3.0); }
                GripStyle::Knurl => {
                    let lead = deg(p.knurl_angle).tan() * (z - self.z0) / 1.0_f64.max(self.r);
                    let tri = |x: f64| { let u = (x / TAU + 1000.0) % 1.0; (u - 0.5).abs() * 2.0 };
                    r -= p.ridge_depth * m * tri(k * (a + lead)).max(tri(k * (a - lead)));
                }
                GripStyle::Rings => {
                    let span = 0.02_f64.max(p.grip_to - p.grip_from);
                    r -= p.ridge_depth * m * (0.5 - 0.5 * (TAU * k * (zf - p.grip_from) / span).cos()).powf(2.0);
                }
                _ => {}
            }
        }
        if matches!(p.indicator(), Indicator::SideNotch | Indicator::GrooveAndNotch) && a.cos() > 0.0 {
            let lat = (r * a.sin()).abs() / (p.ind_width * 0.5);
            if lat < 1.0 { r -= p.notch_depth * ro * (1.0 - lat * lat).sqrt(); }
        }
        r.max(0.3)
    }
    fn ring(&self, z: f64, off: f64, z_profile: f64) -> Ring {
        let r: Vec<_> = self.th.iter().map(|&a| self.outer_r(a, z_profile)).collect();
        self.offset_ring(&r, z, off)
    }
    fn offset_ring(&self, r: &[f64], z: f64, off: f64) -> Ring {
        let n = self.n;
        let dth = TAU / n as f64;
        Ring { z, pts: (0..n).map(|i| {
            let mut rr = r[i];
            if off != 0.0 {
                let dr = (r[(i + 1) % n] - r[(i + n - 1) % n]) / (2.0 * dth);
                let inv = (rr * rr + dr * dr).sqrt() / rr.max(1e-3);
                rr -= off * inv.min(2.0);
            }
            rr = rr.max(0.2);
            [rr * self.th[i].cos(), rr * self.th[i].sin()]
        }).collect(), ..Ring::default() }
    }
    fn body_zs(&self, zb: f64, zt: f64) -> Vec<f64> {
        let p = self.p;
        let mut m = 1;
        if p.twist.abs() > 0.01 || p.bulge.abs() > 0.001 || p.grip_style() != GripStyle::None {
            m = 24;
            if matches!(p.grip_style(), GripStyle::Knurl | GripStyle::Rings) { m = m.max(160.min(((zt - zb) / 0.12) as i32)); }
            if p.grip_style() == GripStyle::Rings { m = 200.min(m.max(p.ridges * 8)); }
        }
        let mut zs: Vec<_> = (0..=m).map(|k| zb + (zt - zb) * k as f64 / m as f64).collect();
        if self.scallops() {
            let mut put = |z| { if z > zb && z < zt { zs.push(z); } };
            if self.sc_ball_bottom {
                let e = self.sc_w * 0.5 + 0.1;
                let mut z = self.sc_za - e;
                while z <= self.sc_za + 0.06 { put(z); z += 0.03; }
            } else { for z in [self.sc_za - 0.03, self.sc_za] { put(z); } }
            if self.sc_top == ScallopTop::Ball {
                let mut z = self.sc_zb - 0.06;
                while z <= self.sc_zb + self.sc_w * 0.5 + 0.1 { put(z); z += 0.03; }
            } else if self.sc_top == ScallopTop::Ramp {
                let mut z = self.sc_zb - 0.06;
                while z <= self.sc_zb + (self.sc_d + SC_BLEND) / self.sc_tan_ramp + 0.1 { put(z); z += 0.03; }
            }
            unique_heights(&mut zs);
            if !self.sc_ball_bottom {
                for c in [self.sc_za - 0.03, self.sc_za] {
                    for z in &mut zs { if (*z - c).abs() < 0.02 && *z != zt && *z != zb { *z = c; } }
                }
            }
            *zs.last_mut().unwrap() = zt;
        } else if p.grip_style() != GripStyle::None && p.ridge_depth > 0.0 {
            let a = p.grip_from;
            let b = p.grip_to.max(a + 0.02);
            let f = 0.06_f64.min((b - a) * 0.25);
            for (zf0, zf1) in [(a - 0.005, a + f + 0.005), (b - f - 0.005, b + 0.005)] {
                let mut z = self.z0 + zf0 * (self.h - self.z0);
                let z1b = self.z0 + zf1 * (self.h - self.z0);
                while z <= z1b { if z > zb && z < zt { zs.push(z); } z += 0.08; }
            }
            unique_heights(&mut zs);
            *zs.last_mut().unwrap() = zt;
        }
        zs
    }
}
fn unique_heights(zs: &mut Vec<f64>) {
    zs.sort_by(f64::total_cmp);
    zs.dedup_by(|y, x| *y - *x < 0.02);
}

fn body_loft(g: &Gen<'_>, s: f64) -> Loft {
    let p = g.p;
    let h = g.h;
    let e = g.e_top;
    let fillet = p.edge_style() == EdgeStyle::Fillet;
    let zb = g.zb;
    let c = g.c_bot;
    let mut b = Loft::default();
    let add = |b: &mut Loft, z, off, cr| b.add(g.ring(z, off + s, z), cr);
    if s != 0.0 && !p.skirt { b.add(g.ring(-1.0, s, -1.0), true); }
    if c > 0.01 && s == 0.0 { add(&mut b, zb, c, true); }
    let mut z_body = zb + c;
    if g.cove > 0.0 {
        let rb = g.cove;
        let cove_off = |dz| {
            let dz = clampf(dz, 0.0, rb);
            -(rb - 0.0_f64.max(rb * rb - (rb - dz) * (rb - dz)).sqrt())
        };
        add(&mut b, zb, cove_off(0.0), true);
        let kc = 12.max((rb / 0.08).ceil() as i32);
        for k in 1..=kc {
            let dz = rb * k as f64 / kc as f64;
            add(&mut b, g.z0 + dz, cove_off(dz), false);
        }
        z_body = g.z0 + rb;
    }
    let zs = g.body_zs(z_body, h - e);
    let skip = usize::from(g.cove > 0.0);
    if s == 0.0 {
        for &z in &zs[skip..] { add(&mut b, z, 0.0, g.crease_at(z)); }
        b.crease[usize::from(c > 0.01)] = true;
        if e > 0.01 {
            b.end();
            if fillet {
                for k in 1..=8 {
                    let a = 0.5 * PI * k as f64 / 8.0;
                    add(&mut b, h - e + e * a.sin(), e * (1.0 - a.cos()), false);
                }
            } else { add(&mut b, h, e, false); }
        }
        b.end();
    } else {
        let r2 = 2.0_f64.sqrt() - 1.0;
        let mut z_side = h - s;
        if e > 0.01 && fillet && e - s > 0.02 { z_side = h - e; }
        else if e > 0.01 && !fillet { z_side = h - e - s * r2; }
        for &z in &zs[skip..] { if z < z_side - 0.02 { add(&mut b, z, 0.0, g.crease_at(z)); } }
        add(&mut b, z_side, 0.0, true);
        if e > 0.01 && fillet && e - s > 0.02 {
            let re = e - s;
            for k in 1..=8 {
                let a = 0.5 * PI * k as f64 / 8.0;
                let z = h - e + re * a.sin();
                b.add(g.ring(z, e - re * a.cos(), z), false);
            }
        } else if e > 0.01 && !fillet { b.add(g.ring(h - s, e + s * r2, h - s), false); }
        b.end();
    }
    if p.top_style() == TopStyle::Dome {
        let top = b.rings.last().unwrap().clone();
        for k in 1..=10 {
            let rho = (0.5 * PI * k as f64 / (10.0 + 0.6)).cos();
            b.add(Ring {
                z: h - s + p.dome_height * (1.0 - rho * rho),
                pts: top.pts.iter().map(|q| [q[0] * rho, q[1] * rho]).collect(),
                ..Ring::default()
            }, false);
        }
        if !fillet || e <= 0.01 { let i = b.crease.len() - 11; b.crease[i] = true; }
        b.end();
    }
    b
}
fn sphere_dish(n: usize, rs: f64, zc: f64, r_rim: f64, z_collar: f64, label: &str) -> Prim {
    let mut s = Loft::default();
    let kr = 16.max(64.min((r_rim / 0.2).ceil() as i32));
    for k in 0..=kr {
        let rho = r_rim * k.max(1) as f64 / kr as f64 * if k == 0 { 0.05 } else { 1.0 };
        s.add(circle(n, rho, zc - 0.0_f64.max(rs * rs - rho * rho).sqrt()), k == 0 || k == kr);
    }
    s.add(circle(n, r_rim, z_collar), true);
    s.prim(label)
}

/// Build the knob's solids and dimensional report without evaluating a mesh.
/// Mass, volume, area, triangle count and mesh build time remain unset (zero).
pub fn build(p: &KnobParams) -> KnobBuild { build_gen(p, Gen::new(p)) }

fn build_gen(p: &KnobParams, g: Gen<'_>) -> KnobBuild {
    let mut out = KnobBuild::default();
    let csg = &mut out.csg;
    csg.segments = 48.max(256.min(g.n as i32 / 2));
    let mut warn = |s: &str| out.warnings.push(s.to_owned());
    let n = g.n;
    let h = g.h;
    let z0 = g.z0;
    if p.skirt {
        let rs = g.skirt_apothem(z0);
        let e = p.skirt_edge.min(z0 * 0.45);
        let c = p.bottom_edge.min(z0 * 0.45);
        let mut s = Loft::default();
        let mut add = |z, inset, cr| s.add(g.skirt_ring(z, inset), cr);
        if c > 0.01 { add(0.0, c, true); }
        add(if c > 0.01 { c } else { 0.0 }, 0.0, true);
        if e > 0.01 && p.skirt_edge_style() == EdgeStyle::Chamfer {
            let ev = (e * deg(clampf(p.skirt_chamfer_angle, 10.0, 80.0)).tan()).min(z0 * 0.6);
            add(z0 - ev, 0.0, true);
            add(z0, ev / deg(clampf(p.skirt_chamfer_angle, 10.0, 80.0)).tan(), true);
        } else if e > 0.01 {
            for k in 0..=6 {
                let a = 0.5 * PI * k as f64 / 6.0;
                add(z0 - e + e * a.sin(), e * (1.0 - a.cos()), k == 0);
            }
        } else { add(z0, 0.0, true); }
        s.end();
        csg.add(s.prim("skirt"));
        if rs < g.r { warn("skirt is smaller than the body"); }
        if p.skirt_taper < 0.5 { warn("skirt side has no draft: give it a taper of 1 deg or more"); }
    }
    if p.grip_style() == GripStyle::Rings { warn("ring grooves run around the knob: their upper flanks face down (an undercut for a straight pull)"); }
    if p.grip_style() == GripStyle::Knurl { warn("a diamond knurl has down-facing facets (an undercut for a straight pull: side actions or a printed part)"); }
    if p.grip_style() == GripStyle::Flutes && p.flute_top == 1 { warn("faded flute tops close the groove going up (an undercut): use Flute top = Open"); }
    if g.scallops() && p.twist.abs() > 0.5 { warn("twisted scallops are helical: one flank faces down (an unscrewing core or a printed part)"); }
    if g.scallops() && g.sc_top != ScallopTop::Open { warn("closed scallop tops overhang the groove: an undercut for the cavity pulled along +z (use Scallop top = Open)"); }

    let mut top_r = g.r * p.taper;
    let e = g.e_top;
    let top_style = p.top_style();
    let dome_on = top_style == TopStyle::Dome;
    let cap_on = top_style == TopStyle::CapInsert;
    let body_main = body_loft(&g, 0.0);
    let mut r_flat_min = 1e9_f64;
    let top = &body_main.rings[body_main.rings.len() - if dome_on { 11 } else { 1 }];
    for (i, q) in top.pts.iter().enumerate() {
        let r = (q[0] * q[0] + q[1] * q[1]).sqrt();
        if g.scallops() && g.scallop_cut(g.th[i] - deg(p.twist), h - 0.01, r + 1.0) > 0.0 { continue; }
        r_flat_min = r_flat_min.min(r);
    }
    if r_flat_min > 1e8 { r_flat_min = g.r * p.taper - e; }
    csg.add(body_main.prim("body"));
    top_r = 0.5_f64.max(top_r - e);
    let mut top_depth = 0.0_f64;
    let (mut dish_rs, mut dish_zc, mut dish_dep) = (0.0, 0.0, 0.0);
    if top_style == TopStyle::Dish {
        let d = 0.5_f64.max(p.dish_diameter * top_r * 2.0);
        let dep = p.dish_depth.min(h * 0.5);
        let rs = (d * d / 4.0 + dep * dep) / (2.0 * dep);
        csg.sub(sphere_dish(n, rs, h + rs - dep, d * 0.5, h + 1.0, "dish"));
        top_depth = dep;
        dish_rs = rs; dish_zc = h + rs - dep; dish_dep = dep;
    }
    let (mut cap_top_low, mut cap_top_high, mut rim_inner) = (h, h, 0.0);
    let mut rec_r: Box<dyn Fn(f64) -> f64> = Box::new(|_| 0.0);
    let mut cap_base = program(csg.segments);
    let mut cap_plus = program(csg.segments);
    if cap_on {
        let tan_c = deg(p.cap_draft).tan();
        let rim_w = p.rim_width.min(r_flat_min * 0.6);
        let ch = p.rim_chamfer.min(0.0_f64.max(rim_w - 0.1));
        let rr = r_flat_min - rim_w;
        let zc = h - ch;
        rec_r = Box::new(move |z| if z <= zc { rr - (zc - z) * tan_c } else { rr + (z - zc) });
        rim_inner = rec_r(h);
        let cap_top = h - p.cap_recess.min(h * 0.3);
        let mut thick = p.cap_thickness;
        let zf_min = (z0 + 1.0).max(h * 0.35);
        if cap_top - thick < zf_min { thick = 0.3_f64.max(cap_top - zf_min); warn("cap too thick for the knob: thickness limited"); }
        let zf = cap_top - thick;
        if rim_w < p.rim_width - 1e-4 { warn("rim width limited to 60% of the top radius"); }
        if g.scallops() && g.sc_top == ScallopTop::Open && rim_w - g.sc_d - ch < 0.5 { warn("open scallops leave under 0.5 mm of rim beside the cap recess: widen the rim"); }
        if ch < p.rim_chamfer - 1e-4 { warn("rim chamfer limited to the rim width"); }
        if rr < 1.5 { warn("cap recess very small: narrow the rim"); }
        let mut rc = Loft::default();
        rc.add(circle(n, rec_r(zf), zf), true);
        if ch > 0.01 && zc > zf + 0.02 { rc.add(circle(n, rec_r(zc), zc), true); }
        rc.add(circle(n, rec_r(h), h), true);
        rc.add(circle(n, rec_r(h) + if ch > 0.01 { 1.0 } else { 0.0 }, h + 1.0), true);
        csg.sub(rc.prim("cap recess"));
        let f = p.cap_edge.min(thick * 0.5).min(rec_r(cap_top) * 0.3);
        let mut cp = Loft::default();
        cp.add(circle(n, rec_r(zf), zf), true);
        let z_fil = cap_top - if f > 0.01 { f } else { 0.0 };
        if ch > 0.01 && zc > zf + 0.02 && zc < z_fil - 0.02 { cp.add(circle(n, rec_r(zc), zc), true); }
        let r_edge = rec_r(z_fil);
        cp.add(circle(n, r_edge, z_fil), f <= 0.01);
        if f > 0.01 {
            for k in 1..=8 {
                let a = 0.5 * PI * k as f64 / 8.0;
                cp.add(circle(n, r_edge - f * (1.0 - a.cos()), cap_top - f + f * a.sin()), false);
            }
        }
        let rho0 = r_edge - f.max(0.0);
        let crown = clampf(p.cap_crown, -rho0 * 0.4, rho0 * 0.4);
        if crown > 0.01 {
            if f <= 0.01 { cp.end(); }
            for k in 1..=10 {
                let t = (0.5 * PI * k as f64 / (10.0 + 0.6)).cos();
                cp.add(circle(n, rho0 * t, cap_top + crown * (1.0 - t * t)), false);
            }
            cap_top_high = cap_top + crown;
        }
        cp.end();
        // Bury the envelope's cap side in the body, retaining its exposed edge.
        let mut cpp = Loft::default();
        let i_fil = if cp.rings.len() > 1 && (cp.rings[1].z - z_fil).abs() < 1e-6 { 1 } else { 2 };
        for ring in &cp.rings[..i_fil] { cpp.add(circle(n, rec_r(ring.z) + 0.1, ring.z), true); }
        if z_fil - 0.05 > cpp.rings.last().unwrap().z + 0.02 { cpp.add(circle(n, rec_r(z_fil - 0.05) + 0.1, z_fil - 0.05), true); }
        for k in i_fil..cp.rings.len() { cpp.add(cp.rings[k].clone(), k == i_fil || cp.crease[k]); }
        cap_base.add(cp.prim("cap"));
        cap_plus.add(cpp.prim("cap (envelope)"));
        if crown < -0.01 {
            let r_rim = rho0 * 0.97;
            let dep = -crown;
            let rs = (r_rim * r_rim + dep * dep) / (2.0 * dep);
            cap_base.sub(sphere_dish(n, rs, cap_top + rs - dep, r_rim, cap_top + 1.0, "cap dish"));
            cap_plus.sub(sphere_dish(n, rs, cap_top + rs - dep, r_rim, cap_top + 1.0, "cap dish"));
            cap_top_low = cap_top - dep;
        } else { cap_top_low = cap_top; }
        top_depth = h - zf;
    }
    let ind = p.indicator();
    let dome_h = if dome_on { p.dome_height } else { 0.0 };
    let ind_x0 = if cap_on { (p.ind_start * top_r).max(rim_inner + 0.3) } else { p.ind_start * top_r };
    if cap_on && matches!(ind, Indicator::Groove | Indicator::GrooveAndNotch | Indicator::Ridge | Indicator::Dot) {
        warn("indicator kept on the rim around the cap (Pointer line crosses the cap)");
    }
    if matches!(ind, Indicator::Groove | Indicator::GrooveAndNotch) {
        let x0 = ind_x0;
        let x1 = g.r + 1.0;
        let z_top = h + dome_h + 1.0;
        let t = deg(clampf(p.ind_draft, 0.0, 10.0)).tan();
        let hgt = z_top - (h - p.ind_depth);
        let wb = 0.1_f64.max(p.ind_width - 2.0 * p.ind_depth * t);
        let x0b = x0 + p.ind_depth * t;
        csg.sub(Prim::cuboid("indicator groove", [(x0b + x1) / 2.0, 0.0, (h - p.ind_depth + z_top) / 2.0],
            [x1 - x0b, wb, hgt]).taper(2.0 * hgt * t, 2.0 * hgt * t));
        top_depth = top_depth.max(p.ind_depth);
    } else if ind == Indicator::Ridge {
        let x0 = ind_x0;
        let x1 = (x0 + 0.5).max(top_r - 0.2);
        csg.add(Prim::cuboid("indicator line", [(x0 + x1) / 2.0, 0.0, h - 0.05 + p.ind_depth / 2.0],
            [x1 - x0, p.ind_width, p.ind_depth + 0.1]));
        if dome_h > 0.0 { warn("raised line on a dome top sits at the dome's base height"); }
    } else if ind == Indicator::Dot {
        let x = if cap_on { ind_x0.max(p.ind_start.max(0.3) * top_r) } else { p.ind_start.max(0.3) * top_r };
        csg.sub(Prim::cylinder("indicator dot", [x, 0.0, h - p.ind_depth], p.ind_width * 0.5,
            p.ind_depth + dome_h + 1.0, [0.0, 0.0, 1.0]));
        top_depth = top_depth.max(p.ind_depth);
    }

    // Intersect the pointer footprint with the surface shell. Reuse the same
    // groove in each body so their shared faces touch without overlapping.
    if ind == Indicator::Line {
        let mode = p.line_mode();
        let groove = mode != LineMode::Raised;
        let sgn = if groove { 1.0 } else { -1.0 };
        let ws = 0.05_f64.max(p.line_width_start);
        let we = 0.1_f64.max(p.line_width_end);
        let mut d = if groove { p.line_depth } else { p.line_height };
        if groove && cap_on && d > p.cap_thickness - 0.3 {
            d = 0.1_f64.max(p.cap_thickness - 0.3);
            warn("line depth limited to the cap thickness minus 0.3 mm");
        }
        if groove && p.hollow() && d > p.wall - 0.4 { warn("line groove deeper than the wall minus 0.4 mm"); }
        if groove && cap_on && p.cap_edge > d - 0.1 { warn("cap edge fillet deeper than the line: the line thins at the cap edge"); }
        let mut env = program(csg.segments);
        env.steps = csg.steps.clone();
        if cap_on { env.add(Prim::group("cap", cap_plus)); }
        let s_off = if groove { d } else { -d };
        let mut off = program(csg.segments);
        if p.skirt {
            for st in &csg.steps { if st.prim.label == "skirt" { off.add(st.prim.clone()); } }
        }
        off.add(body_loft(&g, s_off).prim(if s_off > 0.0 { "body inset" } else { "body outset" }));
        if top_style == TopStyle::Dish {
            let rs2 = dish_rs + s_off;
            let z_top_cut = h - s_off;
            let rim = 0.01_f64.max(rs2 * rs2 - (dish_zc - z_top_cut) * (dish_zc - z_top_cut)).sqrt();
            off.sub(sphere_dish(n, rs2, dish_zc, rim, h + 1.0 + s_off.abs(), if groove { "dish grown" } else { "dish shrunk" }));
        }
        if cap_on {
            let mut ra = Loft::default();
            let za = (if groove { cap_top_low } else { cap_top_high }) - s_off;
            let mut zz = vec![za];
            let zc = h - p.rim_chamfer.min(0.0_f64.max(p.rim_width.min(r_flat_min * 0.6) - 0.1));
            if zc > za + 0.02 { zz.push(zc); }
            if h > zz[zz.len() - 1] + 0.02 { zz.push(h); }
            zz.push(h.max(zz[zz.len() - 1]) + 1.0 + s_off.abs());
            for z in zz { ra.add(circle(n, 0.3_f64.max(rec_r(z) + s_off), z), true); }
            off.sub(ra.prim(if groove { "recess above cap grown" } else { "recess above cap shrunk" }));
        }
        let tan_l0 = deg(p.line_draft).tan();
        let xe = 1.0_f64.max(g.outer_r(0.0, h) - e);
        let x0 = (p.ind_start * top_r).min(xe - 0.5);
        let x_max = g.r.max(if p.skirt { p.skirt_diameter * 0.5 } else { 0.0 }) + 3.0;
        let z_top_min = if cap_on { cap_top_low } else if top_style == TopStyle::Dish { h - dish_dep } else { h };
        let z_top = (h + dome_h).max(cap_top_high) + if groove { 0.0 } else { d } + 1.0;
        let z_low1 = z_top_min - if groove { d } else { 0.0 } - 0.5;
        let z_lo = if p.skirt { z0 + g.cove * 0.25 } else { 0.0 };
        let z_bot = if p.line_bottom > 0.01 { clampf(p.line_bottom, z_lo, h - e - 0.5) }
            else if p.skirt { if groove { z0 + 0.05 } else { z0 } }
            else if groove { -1.0 } else { 0.0 };
        if !groove && p.line_bottom > 0.01 { warn("raised line ends above the bottom: its bottom end faces down (an undercut); use line_bottom = 0"); }
        let tan_l = if groove {
            tan_l0.min(0.7 * we * 0.5 / 0.1_f64.max(h - z_bot)).min(0.7 * ws * 0.5 / 0.1_f64.max(h - z_low1))
        } else { tan_l0.min(0.7 * ws * 0.5 / 0.1_f64.max(z_top - h)) };
        if tan_l < tan_l0 - 1e-5 {
            warn(&format!("line draft limited to {:.2} deg (the line is too narrow for more)", tan_l.atan() * 180.0 / PI));
        }
        let hw_w = |x, z| ws * 0.5 + (we - ws) * 0.5 * (x - x0) / (xe - x0) + sgn * (z - h) * tan_l;
        let hw_c = |z| we * 0.5 + sgn * (z - h) * tan_l;
        let xs = |z| x0 - sgn * (z - h) * tan_l;
        let mut pts = Vec::new();
        for z in [z_low1, z_top] {
            let x = xs(z);
            let hw = hw_w(x, z);
            pts.push([x, -hw, z]); pts.push([x, hw, z]);
        }
        for z in [z_bot, z_top] {
            for x in [xe, x_max] { pts.push([x, -hw_c(z), z]); pts.push([x, hw_c(z), z]); }
        }
        let footprint = Prim::hull("line footprint", pts);
        if groove {
            let mut groove_csg = program(csg.segments);
            groove_csg.add(footprint);
            groove_csg.sub(Prim::group("envelope inset", off));
            let gp = Prim::group("line groove", groove_csg);
            csg.sub(gp.clone());
            if cap_on { cap_base.sub(gp.clone()); }
            top_depth = top_depth.max(h - z_top_min + d);
            if mode == LineMode::Inlay {
                let mut line = program(csg.segments);
                line.add(gp);
                line.intersect(Prim::group("envelope", env));
                out.bodies.push(KnobBody { name: "line".into(), colour: p.line_colour, csg: line, ..KnobBody::default() });
            }
        } else {
            let mut line = program(csg.segments);
            line.add(footprint);
            line.intersect(Prim::group("envelope outset", off));
            line.sub(Prim::group("envelope", env));
            out.bodies.push(KnobBody { name: "line".into(), colour: p.line_colour, csg: line, ..KnobBody::default() });
        }
        if g.scallops() {
            let half = (we * 0.5 + g.sc_w * 0.5) / 1.0_f64.max(g.r * p.taper);
            for k in 0..g.sc_n {
                if k != g.sc_skip && remainder(g.sc_phase + (k as f64 + 0.5) * g.sc_pitch, TAU).abs() < half {
                    warn("the pointer line runs through a scallop: set Scallop phase or Skip under indicator");
                    break;
                }
            }
            if p.twist.abs() > 0.5 { warn("twisted scallops cross the straight pointer line"); }
        }
    }
    if cap_on {
        out.bodies.insert(0, KnobBody { name: "cap".into(), colour: p.cap_colour, roughness: 0.5, csg: cap_base });
    }

    let rs = p.shaft_diameter * 0.5 + p.clearance;
    let bore_a = deg(p.bore_angle);
    let bore_r = |a| bore_radius(p, a, bore_a, rs);
    if p.bore_type() == BoreType::D && p.shaft_flat >= p.shaft_diameter { warn("D flat is outside the shaft"); }
    let mut bore_max = rs;
    for &a in &g.th { bore_max = bore_max.max(bore_r(a)); }
    let bore_depth = p.bore_depth.min(h - 0.6);
    if p.bore_depth > h - 0.6 { warn("bore deeper than the knob; clamped"); }
    let boss_r = bore_max + p.boss_wall;
    if p.hollow() {
        let ceil = h - top_depth - p.top_thickness;
        let boss_bottom = p.boss_recess.min(ceil - 1.0);
        if ceil < 1.5 { warn("top too thick or knob too short to core out"); }
        else {
            let tan_d = deg(p.draft).tan();
            let zs = g.body_zs(0.0, ceil);
            let rr = cavity_radii(&g, &zs, boss_r, tan_d);
            let min_cav = rr.last().unwrap().iter().copied().fold(1e9_f64, f64::min);
            if min_cav < boss_r + p.wall * 0.5 { warn("little room between boss and wall: consider Construction: Solid (cast) or a smaller boss"); }
            let mut cv = Loft::default();
            cv.add(g.offset_ring(&rr[0], -1.0, 0.0), true);
            for (k, &z) in zs.iter().enumerate() { cv.add(g.offset_ring(&rr[k], z, 0.0), k == 0 || k + 1 == zs.len()); }
            csg.sub(cv.prim("cavity"));
            let b_top = ceil + 0.05;
            csg.add(Prim::loft("boss", vec![circle(n, boss_r, boss_bottom),
                circle(n, boss_r + (b_top - boss_bottom) * tan_d, b_top)]).crease(vec![true, true]));
            let rib_bottom = p.rib_recess.max(boss_bottom).min(ceil - 0.5);
            for k in 0..p.ribs {
                let a = deg(p.rib_angle) + TAU * k as f64 / p.ribs as f64;
                let i0 = (((a / TAU + 1.0) % 1.0 * n as f64).round() as i32).rem_euclid(n as i32);
                let ov = (p.wall * 0.5).min(0.6);
                let outer_at = |z| {
                    let mut k = 0;
                    while k + 1 < zs.len() && zs[k + 1] < z { k += 1; }
                    let k1 = (k + 1).min(zs.len() - 1);
                    let t = if k1 == k { 0.0 } else { clampf((z - zs[k]) / 1e-4_f64.max(zs[k1] - zs[k]), 0.0, 1.0) };
                    let mut w0 = 1e9_f64;
                    let mut w1 = 1e9_f64;
                    for d in -3..=3 {
                        let i = (i0 + d).rem_euclid(n as i32) as usize;
                        w0 = w0.min(rr[k][i]); w1 = w1.min(rr[k1][i]);
                    }
                    w0 + (w1 - w0) * t + ov
                };
                let r0 = bore_max + 0.2;
                let z_top = ceil + 0.05;
                if outer_at(ceil) <= boss_r + 0.2 { continue; }
                let mut cuts = vec![rib_bottom, z_top];
                for _ in 0..4 {
                    let mut nc = vec![cuts[0]];
                    for c in 0..cuts.len() - 1 {
                        let za = cuts[c]; let zb = cuts[c + 1];
                        let la = outer_at(za); let lb = outer_at(zb.min(ceil));
                        let mut ok = true;
                        for q in 1..8 {
                            if !ok { break; }
                            let z = za + (zb - za) * q as f64 / 8.0;
                            let d = la + (lb - la) * q as f64 / 8.0 - outer_at(z);
                            ok = d > 0.05 - ov && d < p.wall - 0.4 - ov;
                        }
                        if !ok { nc.push((za + zb) / 2.0); }
                        nc.push(zb);
                    }
                    if nc.len() == cuts.len() { break; }
                    cuts = nc;
                }
                let bands = cuts.len() - 1;
                for b in 0..bands {
                    let za = cuts[b];
                    let zb = cuts[b + 1] + if b + 1 < bands { 0.02 } else { 0.0 };
                    let la = outer_at(za) - r0; let lb = outer_at(zb.min(ceil)) - r0;
                    let ta = p.rib_thickness + 2.0 * (za - rib_bottom) * tan_d;
                    let tb = p.rib_thickness + 2.0 * (zb - rib_bottom) * tan_d;
                    let rm = r0 + la / 2.0;
                    csg.add(Prim::cuboid("rib", [rm * a.cos(), rm * a.sin(), (za + zb) / 2.0], [la, ta, zb - za])
                        .rot_z(a * 180.0 / PI).taper(lb - la, tb - ta).shift_x((lb - la) / 2.0));
                }
            }
            if p.set_screw {
                let a = bore_a + deg(p.screw_angle);
                let i0 = (((a / TAU + 100.0) % 1.0 * n as f64).round() as i32).rem_euclid(n as i32);
                let mut wall_r = 1e9_f64;
                for d in -4..=4 { wall_r = wall_r.min(rr.last().unwrap()[(i0 + d).rem_euclid(n as i32) as usize]); }
                let r0 = bore_max;
                let r1 = wall_r + (p.wall * 0.5).min(0.6);
                let z_lo = boss_bottom.max(p.screw_height - p.screw_diameter * 0.5 - p.boss_wall);
                if r1 > r0 && z_lo < ceil {
                    let rm = (r0 + r1) / 2.0;
                    let height = ceil + 0.05 - z_lo;
                    csg.add(Prim::cuboid("set screw web", [rm * a.cos(), rm * a.sin(), (z_lo + ceil + 0.05) / 2.0],
                        [r1 - r0, p.screw_diameter + 2.0 * p.boss_wall, height])
                        .rot_z(a * 180.0 / PI).taper(0.0, 2.0 * height * tan_d));
                }
                if p.screw_height + p.screw_diameter * 0.5 > ceil { warn("set screw hole reaches the top skin"); }
            }
            if p.wall < 1.0 { warn("walls under 1 mm are hard to fill in ABS"); }
            if p.rib_thickness > p.wall * 0.7 { warn("ribs thicker than ~60% of the wall show sink marks"); }
        }
        if bore_depth > ceil + p.top_thickness * 0.5 { warn("bore reaches into the top: thin skin above the shaft"); }
    }
    let r: Vec<_> = g.th.iter().map(|&a| bore_r(a)).collect();
    let tan_b = deg(p.bore_draft).tan();
    let rg_at = |z, mut grow| {
        grow += (bore_depth - z) * tan_b;
        Ring { z, pts: (0..n).map(|i| {
            let rr = r[i] + grow; [rr * g.th[i].cos(), rr * g.th[i].sin()]
        }).collect(), ..Ring::default() }
    };
    let zb = if p.hollow() { p.boss_recess.min(h - 1.0) } else { 0.0 };
    let li = p.lead_in.min(bore_depth * 0.3);
    let mut br = Loft::default();
    br.add(rg_at(zb - 1.0, li + 1.0), true);
    if li > 0.01 { br.add(rg_at(zb + li, 0.0), true); }
    br.add(rg_at(bore_depth, 0.0), true);
    csg.sub(br.prim("bore"));
    if p.set_screw {
        let a = bore_a + deg(p.screw_angle);
        let axis = [a.cos(), a.sin(), 0.0];
        let radius = p.screw_diameter * 0.5;
        csg.sub(Prim::cylinder("set screw hole", [rs * 0.5 * axis[0], rs * 0.5 * axis[1], p.screw_height], radius,
            p.diameter.max(if p.skirt { p.skirt_diameter } else { 0.0 }) * 0.5 + 2.0, axis));
        if p.screw_height - radius < zb || p.screw_height + radius > bore_depth { warn("set screw hole misses the bore"); }
    }
    if p.bore_type() == BoreType::SplitKnurled {
        let kd = p.key_depth.min(bore_depth - zb - 0.5);
        csg.add(Prim::cuboid("split shaft key", [0.0, 0.0, bore_depth - kd / 2.0 + 0.025],
            [2.0 * rs + 0.6, p.key_width, kd + 0.05]).rot_z(p.bore_angle));
    }
    if p.hollow() && boss_r >= g.r * p.taper - p.wall { warn("boss is as wide as the knob"); }
    out.report.diameter = p.diameter.max(if p.skirt { p.skirt_diameter } else { 0.0 });
    out.report.height = (h + dome_h).max(cap_top_high) +
        if ind == Indicator::Line && p.line_mode() == LineMode::Raised { p.line_height } else { 0.0 };
    if ind == Indicator::Ridge { out.report.height = out.report.height.max(h + p.ind_depth); }
    out.report.bore = p.shaft_diameter + 2.0 * p.clearance;
    out.notes = g.notes;
    out
}

fn bore_radius(p: &KnobParams, a: f64, bore_a: f64, rs: f64) -> f64 {
    let mut r = rs;
    let fl = p.shaft_flat - p.shaft_diameter * 0.5 + p.clearance;
    match p.bore_type() {
        BoreType::D => { let c = (a - bore_a).cos(); if c > 1e-4 { r = r.min(fl / c); } }
        BoreType::DoubleD => {
            let c = (a - bore_a).cos().abs();
            let f2 = p.shaft_flat * 0.5 + p.clearance;
            if c > 1e-4 { r = r.min(f2 / c); }
        }
        BoreType::Spline | BoreType::SplitKnurled => { r -= 0.25 * (0.5 + 0.5 * (p.spline_teeth as f64 * (a - bore_a)).cos()); }
        BoreType::Hex | BoreType::Square => {
            let k = if p.bore_type() == BoreType::Hex { 6.0 } else { 4.0 };
            let u = ((a - bore_a) / TAU * k + 1000.5) % 1.0 - 0.5;
            r = (p.shaft_flat * 0.5 + p.clearance) / (u * TAU / k).cos();
        }
        BoreType::Round => {}
    }
    if p.crush_ribs > 0 {
        let count = p.crush_ribs as f64;
        let u = ((a - bore_a) / TAU * count + 1000.5) % 1.0 - 0.5;
        let w = 0.5 / r.max(0.5) / TAU * count;
        let t = u.abs() / w.max(1e-3);
        if t < 1.0 { r -= p.crush_rib_height * (1.0 - t * t); }
    }
    r
}

// Erode sampled outlines in three dimensions, smooth only inward, and enforce
// monotone radii so the cavity core can pull out through the bottom.
fn cavity_radii(g: &Gen<'_>, zs: &[f64], boss_r: f64, tan_d: f64) -> Vec<Vec<f64>> {
    let p = g.p;
    let n = g.n;
    let nf = n as f64;
    let h = g.h;
    let outline = |z| {
        let e = p.top_edge;
        let mut off = 0.0;
        if z > h - e && e > 0.0 {
            let t = (z - (h - e)) / e;
            off = if p.edge_style() == EdgeStyle::Fillet { e * (1.0 - 0.0_f64.max(1.0 - t * t).sqrt()) } else { e * t };
        }
        g.th.iter().map(|&a| g.outer_r(a, z) - off).collect::<Vec<_>>()
    };
    let erode = |ro: &[f64], w: f64, out: &mut [f64]| {
        let rmin = ro.iter().copied().fold(1e9_f64, f64::min);
        let hw = (n as i32 / 2).min(((1.0_f64.min(w / rmin.max(0.1))).asin() / (TAU / nf)).ceil() as i32 + 1);
        for i in 0..n {
            let mut m = ro[i] - w;
            for d in -hw..=hw {
                let q = ro[(i as i32 + d).rem_euclid(n as i32) as usize];
                let ph = d as f64 * (TAU / nf);
                let qs = q * ph.sin();
                if qs.abs() < w { m = m.min(q * ph.cos() - (w * w - qs * qs).sqrt()); }
            }
            out[i] = out[i].min(m.max(0.2));
        }
    };
    let np = 6.max((h / 0.25).ceil() as i32);
    let probe: Vec<_> = (0..=np).map(|k| h * k as f64 / np as f64).collect();
    let po: Vec<_> = probe.iter().map(|&z| outline(z)).collect();
    let mut rr = vec![vec![1e9_f64; n]; zs.len()];
    for (k, &z) in zs.iter().enumerate() {
        erode(&outline(z), p.wall, &mut rr[k]);
        for (j, &zp) in probe.iter().enumerate() {
            let dz = zp - z;
            if dz > 1e-4 && dz < p.wall { erode(&po[j], (p.wall * p.wall - dz * dz).sqrt(), &mut rr[k]); }
        }
    }
    let mut pitch = (p.wall * 1.5).max(1.0);
    if p.grip_style() != GripStyle::None && p.ridges > 0 { pitch = pitch.max(TAU * g.r / p.ridges as f64 * 1.2); }
    for row in &mut rr {
        let mut rmean = 0.0_f64;
        for &v in row.iter() { rmean += v / nf; }
        let hw = 1.max((pitch / rmean.max(0.5) / (TAU / nf) * 0.5).ceil() as i32);
        let mut ero = vec![0.0; n];
        for (i, v) in ero.iter_mut().enumerate() {
            let mut m = 1e9_f64;
            for d in -hw..=hw { m = m.min(row[(i as i32 + d).rem_euclid(n as i32) as usize]); }
            *v = m;
        }
        let hs = 1.max(hw / 2);
        for (i, v) in row.iter_mut().enumerate() {
            let mut acc = 0.0;
            for d in -hs..=hs { acc += ero[(i as i32 + d).rem_euclid(n as i32) as usize]; }
            *v = acc / (2 * hs + 1) as f64;
        }
    }
    let dth = TAU / nf;
    for k in 0..zs.len() {
        let base = rr[k].clone();
        for i in 0..n {
            let rb = base[i];
            let dr = (base[(i + 1) % n] - base[(i + n - 1) % n]) / (2.0 * dth);
            let inv = 2.0_f64.min((rb * rb + dr * dr).sqrt() / rb.max(1e-3));
            let mut r = rb - zs[k] * tan_d * inv;
            if k > 0 { r = r.min(rr[k - 1][i]); }
            rr[k][i] = r.max(boss_r + 0.3);
        }
    }
    rr
}

#[cfg(test)]
#[path = "selftest.rs"]
mod selftest;
