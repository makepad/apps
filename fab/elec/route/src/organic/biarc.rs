// Ported from TiNRS Psychedelic Router 0.1.0: src/core/Biarc.h (11-205).
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in this folder.

//! Arc segments, a pair of arcs and the reference's sampled biarc spline, in f32 mm.
//! C++ default arguments are explicit here: sample uses 6, fit_biarc_spline uses 5.

use super::geom::{Segment2, Vec2, K_PI};

#[derive(Copy, Clone, Debug, Default)]
pub struct ArcSegment {
    pub center: Vec2,
    pub radius: f32,
    /// Radians.
    pub start_angle: f32,
    /// Signed radians.
    pub sweep_angle: f32,
    pub p0: Vec2,
    pub p1: Vec2,
}

impl ArcSegment {
    pub fn point_at(&self, t: f32) -> Vec2 {
        let a = self.start_angle + self.sweep_angle * t;
        self.center + Vec2::new(a.cos(), a.sin()) * self.radius
    }

    pub fn arc_length(&self) -> f32 {
        self.sweep_angle.abs() * self.radius
    }
}

#[derive(Copy, Clone, Debug, Default)]
pub struct Biarc {
    pub arc1: ArcSegment,
    pub arc2: ArcSegment,
    pub junction: Vec2,
    pub is_linear: bool,
    pub line_seg: Segment2,
}

impl Biarc {
    pub fn total_length(&self) -> f32 {
        if self.is_linear { self.line_seg.length() } else { self.arc1.arc_length() + self.arc2.arc_length() }
    }

    /// The reference's default points_per_arc is 6.
    pub fn sample(&self, points_per_arc: i32) -> Vec<Vec2> {
        let mut pts = Vec::new();
        if self.is_linear {
            pts.push(self.line_seg.p0);
            pts.push(self.line_seg.p1);
            return pts;
        }
        for i in 0..=points_per_arc {
            let t = i as f32 / points_per_arc as f32;
            pts.push(self.arc1.point_at(t));
        }
        for i in 1..=points_per_arc {
            let t = i as f32 / points_per_arc as f32;
            pts.push(self.arc2.point_at(t));
        }
        pts
    }
}

pub struct BiarcFitter;

impl BiarcFitter {
    /// Fits a single G1-continuous biarc between (p0, t0) and (p1, t1).
    pub fn fit(p0: Vec2, t0_in: Vec2, p1: Vec2, t1_in: Vec2, out: &mut Biarc) -> bool {
        let chord = p1 - p0;
        let d = chord.length();
        if d < 1e-4f32 { return false; }

        let t0 = t0_in.normalized();
        let t1 = t1_in.normalized();
        if t0.cross(chord.normalized()).abs() < 0.05f32
            && t1.cross(chord.normalized()).abs() < 0.05f32
            && t0.dot(t1) > 0.95f32
        {
            out.is_linear = true;
            out.line_seg = Segment2::new(p0, p1);
            return true;
        }

        // Junction construction: the reference's symmetric parameterization.
        let v = t0 + t1;
        let v_len = v.length();
        let t_mid = if v_len > 1e-4f32 {
            (chord * 2.0f32 / d - v / v_len).normalized()
        } else {
            chord.normalized().perpendicular()
        };
        let denom = 2.0f32 * (1.0f32 + t0.dot(t_mid));
        // Kept even though unused, as in the reference.
        let _d1 = if denom > 1e-4f32 { chord.length() * 0.5f32 } else { chord.length() * 0.5f32 };

        let junction = (p0 + p1) * 0.5f32 + t_mid * (d * 0.1f32);
        out.junction = junction;
        out.is_linear = false;
        Self::build_arc(p0, t0, junction, &mut out.arc1);
        let mut t_junction = (junction - out.arc1.center).perpendicular().normalized();
        if t_junction.dot(p1 - junction) < 0.0f32 { t_junction = t_junction * -1.0f32; }
        Self::build_arc(junction, t_junction, p1, &mut out.arc2);
        true
    }

    /// Convert a polyline to sampled biarcs. The reference's default points_per_arc is 5.
    pub fn fit_biarc_spline(points: &[Vec2], points_per_arc: i32) -> Vec<Vec2> {
        if points.len() < 2 { return points.to_vec(); }
        if points.len() == 2 { return points.to_vec(); }

        let mut tangents = vec![Vec2::default(); points.len()];
        tangents[0] = (points[1] - points[0]).normalized();
        tangents[points.len() - 1] = (points[points.len() - 1] - points[points.len() - 2]).normalized();
        for i in 1..points.len() - 1 {
            tangents[i] = ((points[i + 1] - points[i]).normalized()
                + (points[i] - points[i - 1]).normalized()).normalized();
        }

        let mut result = Vec::new();
        for i in 0..points.len() - 1 {
            let mut b = Biarc::default();
            if Self::fit(points[i], tangents[i], points[i + 1], tangents[i + 1], &mut b) {
                let sampled = b.sample(points_per_arc);
                if result.is_empty() {
                    result.extend_from_slice(&sampled);
                } else {
                    result.extend_from_slice(&sampled[1..]);
                }
            } else {
                if result.is_empty() { result.push(points[i]); }
                result.push(points[i + 1]);
            }
        }
        result
    }

    fn build_arc(p0: Vec2, t0: Vec2, p1: Vec2, arc: &mut ArcSegment) {
        arc.p0 = p0;
        arc.p1 = p1;
        let chord = p1 - p0;
        let normal = t0.perpendicular();
        let chord_mid = (p0 + p1) * 0.5f32;
        let chord_normal = chord.perpendicular();
        let _ray_n = Segment2::new(p0, p0 + normal * 1000.0f32);
        let _bisector = Segment2::new(chord_mid, chord_mid + chord_normal * 1000.0f32);

        // Normal to t0 at p0 intersected with the perpendicular chord bisector.
        let rxs = normal.cross(chord_normal);
        if rxs.abs() > 1e-5f32 {
            let t = (chord_mid - p0).cross(chord_normal) / rxs;
            let center = p0 + normal * t;
            arc.center = center;
            arc.radius = (p0 - center).length();
            let a0 = (p0.y - center.y).atan2(p0.x - center.x);
            let a1 = (p1.y - center.y).atan2(p1.x - center.x);
            let mut diff = a1 - a0;
            while diff > K_PI { diff -= 2.0f32 * K_PI; }
            while diff < -K_PI { diff += 2.0f32 * K_PI; }
            arc.start_angle = a0;
            arc.sweep_angle = diff;
        } else {
            arc.center = chord_mid;
            arc.radius = chord.length() * 0.5f32;
            arc.start_angle = (p0.y - arc.center.y).atan2(p0.x - arc.center.x);
            arc.sweep_angle = K_PI;
        }
    }
}
