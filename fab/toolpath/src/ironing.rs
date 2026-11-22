//! Ironing (EXPSLICE §2.9): after a top surface is printed, the nozzle runs
//! over it again at the same height on close lines with very little flow,
//! melting the ridges flat. The passes print last in their island (the
//! writer orders `Role::Ironing` after every other fill).

use crate::exp::Ironing;
use crate::fill;
use crate::{ExtrusionPath, Role};
use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::offset::{offset_ex, DEFAULT_MITER};

/// Ironing passes over `region` (a top surface of one island) on a layer of height `h`.
pub fn paths(region: &[ExPolygon], ir: &Ironing, h: f64, w_top: f64, center: Pt) -> Vec<ExtrusionPath> {
    // Stay off the outer wall's edge: half a top line in, plus the user's inset.
    let inset = 0.5 * w_top + ir.inset;
    let area_in = offset_ex(region, -inset * SCALE, DEFAULT_MITER);
    if area_in.is_empty() {
        return Vec::new();
    }
    let lines = fill::lines(&area_in, ir.angle, ir.spacing, center, fill::Align::Grid, fill::Connect::Boundary { max: 3.0 * ir.spacing.max(0.1), anchor: 0.0 });
    // The flow of a bead the ironing spacing wide at the layer height, scaled by the ironing flow.
    let area = ir.flow * ir.spacing * h;
    lines
        .into_iter()
        .filter(|pl| pl.pts.len() >= 2)
        .map(|pl| ExtrusionPath { role: Role::Ironing, pts: pl.pts, width: ir.spacing.max(0.1), height: h, area, speed: ir.speed, mode: crate::exp::mode::IRONING, ..Default::default() })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_square_top_gets_close_low_flow_lines() {
        let sq = ExPolygon::new(Polygon::new(vec![Pt::from_mm(0.0, 0.0), Pt::from_mm(20.0, 0.0), Pt::from_mm(20.0, 20.0), Pt::from_mm(0.0, 20.0)]));
        let ir = Ironing { only_topmost: false, solid: false, flow: 0.1, spacing: 0.1, speed: 20.0, inset: 0.0, angle: 45f64.to_radians() };
        let ps = paths(&[sq], &ir, 0.2, 0.42, Pt::from_mm(10.0, 10.0));
        assert!(!ps.is_empty());
        let len: f64 = ps.iter().map(|p| p.length_mm()).sum();
        // About (20 - 0.42)² / 0.1 mm of line.
        let want = (20.0f64 - 0.42).powi(2) / 0.1;
        assert!((len - want).abs() / want < 0.15, "length {len} vs {want}");
        assert!(ps.iter().all(|p| p.role == Role::Ironing && p.area < 0.003 && p.speed == 20.0));
        // Inside the inset square.
        for p in &ps {
            for q in &p.pts {
                assert!(q.x_mm() > 0.2 && q.x_mm() < 19.8 && q.y_mm() > 0.2 && q.y_mm() < 19.8);
            }
        }
    }
}
