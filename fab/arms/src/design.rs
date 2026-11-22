//! The part to build: its signed distance in the part frame (mm, z up from the plate top), and
//! its layer regions as the slicer's polygons.

use fab_robot::math::*;
use makepad_fab_slicer::geom::{ExPolygon, Polygon, Pt};
use makepad_fab_slicer::offset::offset_mm;
use makepad_fab_slicer::sdf::{contour_segments, SdfParams};
use makepad_fab_slicer::slice::{segments_to_regions, SliceParams};
use std::sync::Arc;

pub type Sdf = Arc<dyn Fn(V3) -> f64 + Send + Sync>;

#[derive(Clone)]
pub struct Design {
    pub sdf: Sdf,
    /// Its box (part frame).
    pub lo: V3,
    pub hi: V3,
}

impl std::fmt::Debug for Design {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Design({:?}..{:?})", self.lo, self.hi)
    }
}

impl Design {
    pub fn new(sdf: Sdf, lo: V3, hi: V3) -> Design {
        Design { sdf, lo, hi }
    }

    pub fn distance(&self, p: V3) -> f64 {
        (self.sdf)(p)
    }

    /// The outward normal at `p` (the distance's gradient).
    pub fn normal(&self, p: V3) -> V3 {
        let h = 1e-3;
        let d = |k: usize| {
            let (mut a, mut b) = (p, p);
            a[k] += h;
            b[k] -= h;
            self.distance(a) - self.distance(b)
        };
        norm([d(0), d(1), d(2)])
    }

    /// A ring of height `height` from the plate, radii `inner`..`outer`, centred at `c` (x, y).
    pub fn ring(c: [f64; 2], inner: f64, outer: f64, height: f64) -> Design {
        let mid = 0.5 * (inner + outer);
        let half = 0.5 * (outer - inner);
        let sdf: Sdf = Arc::new(move |p: V3| {
            let r = ((p[0] - c[0]).powi(2) + (p[1] - c[1]).powi(2)).sqrt();
            let across = (r - mid).abs() - half;
            let up = (p[2] - 0.5 * height).abs() - 0.5 * height;
            let o = across.max(0.0).hypot(up.max(0.0));
            o + across.max(up).min(0.0)
        });
        Design::new(sdf, [c[0] - outer, c[1] - outer, 0.0], [c[0] + outer, c[1] + outer, height])
    }

    /// The section at height `z`: its regions with their outer contours grown by `margin` and
    /// their holes kept where they are (only an outer face can be cut back tangentially).
    pub fn layer(&self, z: f64, margin: f64) -> Vec<ExPolygon> {
        let sdf = self.sdf.clone();
        let f = move |x: f64, y: f64| sdf([x, y, z]);
        let pad = margin + 2.0;
        let segs = contour_segments(&f, [self.lo[0] - pad, self.lo[1] - pad], [self.hi[0] + pad, self.hi[1] + pad], &SdfParams { coarse: 1.0, fine: 0.1, ..SdfParams::default() });
        let regions = segments_to_regions(segs, &SliceParams::default());
        if margin <= 0.0 {
            return regions;
        }
        regions
            .iter()
            .flat_map(|r| {
                let grown = offset_mm(&[ExPolygon::new(r.contour.clone())], margin);
                grown.into_iter().map(|mut g| {
                    g.holes.extend(r.holes.iter().cloned());
                    g
                })
            })
            .collect()
    }
}

/// The bead centre lines that fill `region`: rings `spacing` apart from half a bead in; where
/// the region is thinner than a bead, its centre ring at the region's width. Each ring with
/// the width its bead must have.
pub fn tracks(region: &ExPolygon, width: f64, spacing: f64) -> Vec<(Vec<[f64; 2]>, f64)> {
    let src = std::slice::from_ref(region);
    let mut out = Vec::new();
    let mut d = 0.5 * width;
    loop {
        let rings = offset_mm(src, -d);
        if rings.is_empty() {
            break;
        }
        for r in &rings {
            out.push((points(&r.contour), width));
        }
        d += spacing;
    }
    if out.is_empty() {
        // Thinner than a bead: the deepest offset still there is the centre line.
        let (mut lo, mut hi) = (0.0, 0.5 * width);
        for _ in 0..14 {
            let m = 0.5 * (lo + hi);
            if offset_mm(src, -m).is_empty() {
                hi = m;
            } else {
                lo = m;
            }
        }
        let d = (lo - 0.01).max(0.0);
        for r in offset_mm(src, -d) {
            out.push((points(&r.contour), 2.0 * lo));
        }
    }
    out
}

fn points(p: &Polygon) -> Vec<[f64; 2]> {
    p.pts.iter().map(|q: &Pt| [q.x_mm(), q.y_mm()]).collect()
}
