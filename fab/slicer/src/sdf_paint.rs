//! Colour from a distance field's material slots: the paint split of a
//! field source (`SdfSource::material`), through the same per-layer split as
//! a painted mesh ([`crate::paint::split_cuts`]).
//!
//! - The cut segments of each layer are tagged with the state along them. A
//!   segment whose two ends differ is split where the state changes
//!   (bisection along it to 1 µm), so colour never bleeds past a boundary.
//! - Painted up and down facing flats (the mesh path's coloured faces) are the
//!   layer's exposed area sampled on a scanline grid ([`FLAT_STEP`]) at the
//!   layer's plane.

use std::sync::Arc;

use crate::clip::{difference_ex, union_ex};
use crate::geom::{ExPolygon, Polygon, Pt};
use crate::paint::{split_cuts, Flat, LayerPaint, PaintParams};
use crate::sdf::SdfSource;

/// The state at a point in printer mm (0: the object's own filament, N:
/// filament N).
pub type MaterialFn = Arc<dyn Fn(f64, f64, f64) -> u32 + Send + Sync>;

/// The flats' sample spacing (mm).
pub const FLAT_STEP: f64 = 0.2;

/// Layer `z`'s segments with the state along each.
pub fn tag_segments(segs: &[(Pt, Pt)], z: f64, m: &MaterialFn) -> Vec<(Pt, Pt, u32)> {
    let mut out = Vec::with_capacity(segs.len());
    for &(a, b) in segs {
        let (pa, pb) = ((a.x_mm(), a.y_mm()), (b.x_mm(), b.y_mm()));
        let (sa, sb) = (m(pa.0, pa.1, z), m(pb.0, pb.1, z));
        split(a, b, pa, pb, sa, sb, z, m, 0, &mut out);
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn split(a: Pt, b: Pt, pa: (f64, f64), pb: (f64, f64), sa: u32, sb: u32, z: f64, m: &MaterialFn, depth: u32, out: &mut Vec<(Pt, Pt, u32)>) {
    if sa == sb {
        push(out, a, b, sa);
        return;
    }
    let pm = (0.5 * (pa.0 + pb.0), 0.5 * (pa.1 + pb.1));
    let c = Pt::from_mm(pm.0, pm.1);
    let len = (pb.0 - pa.0).hypot(pb.1 - pa.1);
    if len < 1e-3 || depth > 24 || c == a || c == b {
        if c != a {
            push(out, a, c, sa);
        }
        if c != b {
            push(out, c, b, sb);
        }
        return;
    }
    let sm = m(pm.0, pm.1, z);
    split(a, c, pa, pm, sa, sm, z, m, depth + 1, out);
    split(c, b, pm, pb, sm, sb, z, m, depth + 1, out);
}

/// Appends a piece, joined to the previous one when it continues it.
fn push(out: &mut Vec<(Pt, Pt, u32)>, a: Pt, b: Pt, st: u32) {
    match out.last_mut() {
        Some(l) if l.1 == a && l.2 == st => l.1 = b,
        _ => out.push((a, b, st)),
    }
}

/// The painted parts of `exposed` sampled at plane `z`, as flats at height `fz`.
pub fn sample_flats(exposed: &[ExPolygon], z: f64, fz: f64, m: &MaterialFn) -> Vec<Flat> {
    let h = FLAT_STEP;
    let mut by_state: Vec<(u32, Vec<ExPolygon>)> = Vec::new();
    for ex in exposed {
        let edges: Vec<((f64, f64), (f64, f64))> = ex
            .rings()
            .flat_map(|r| (0..r.pts.len()).map(move |i| (r.pts[i], r.pts[(i + 1) % r.pts.len()])))
            .map(|(p, q)| ((p.x_mm(), p.y_mm()), (q.x_mm(), q.y_mm())))
            .collect();
        let bb = ex.bbox();
        let (y0, y1) = (bb.min.y_mm(), bb.max.y_mm());
        let mut y = y0 + 0.5 * h;
        while y < y1 {
            // The row's spans inside the region (even-odd over every ring).
            let mut xs: Vec<f64> = edges
                .iter()
                .filter(|(p, q)| (p.1 <= y) != (q.1 <= y))
                .map(|(p, q)| p.0 + (y - p.1) / (q.1 - p.1) * (q.0 - p.0))
                .collect();
            xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
            for s in xs.chunks_exact(2) {
                let mut run: Option<(u32, f64)> = None;
                let mut x = s[0] + 0.5 * h;
                let mut flush = |run: Option<(u32, f64)>, end: f64| {
                    if let Some((st, start)) = run.filter(|r| r.0 != 0) {
                        let r = ExPolygon::new(Polygon::rect(Pt::from_mm(start - 0.5 * h, y - 0.5 * h), Pt::from_mm(end + 0.5 * h, y + 0.5 * h)));
                        match by_state.iter_mut().find(|b| b.0 == st) {
                            Some(b) => b.1.push(r),
                            None => by_state.push((st, vec![r])),
                        }
                    }
                };
                let mut last = x;
                while x < s[1] {
                    let st = m(x, y, z);
                    match run {
                        Some((r, _)) if r == st => {}
                        _ => {
                            flush(run, last);
                            run = Some((st, x));
                        }
                    }
                    last = x;
                    x += h;
                }
                flush(run, last);
            }
            y += h;
        }
    }
    let mut out = Vec::new();
    for (st, rects) in by_state {
        for e in union_ex(&rects) {
            out.push((fz, st, e));
        }
    }
    out
}

fn split_with_flats(cuts: Vec<Vec<(Pt, Pt, u32)>>, layer_m: &(dyn Fn(usize) -> MaterialFn + Sync), zs: &[f64], slices: &[Vec<ExPolygon>], p: &PaintParams) -> Vec<LayerPaint> {
    let n = zs.len();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| zs[a].partial_cmp(&zs[b]).unwrap());
    let flats = |up: bool, layers: usize| -> Vec<Flat> {
        if layers == 0 {
            return Vec::new();
        }
        crate::par::map(n, |k| {
            let li = order[k];
            let (other, fz) = if up {
                match order.get(k + 1) {
                    Some(&j) => (slices[j].as_slice(), 0.5 * (zs[li] + zs[j])),
                    None => (&[][..], zs[li] + 1e-6),
                }
            } else {
                (if k > 0 { slices[order[k - 1]].as_slice() } else { &[][..] }, zs[li])
            };
            let exposed = difference_ex(&slices[li], other);
            if exposed.is_empty() {
                return Vec::new();
            }
            sample_flats(&exposed, zs[li], fz, &layer_m(li))
        })
        .into_iter()
        .flatten()
        .collect()
    };
    let (up, down) = (flats(true, p.top_layers), flats(false, p.bottom_layers));
    if up.is_empty() && down.is_empty() && cuts.iter().all(|c| c.iter().all(|s| s.2 == 0)) {
        return slices.iter().map(|s| vec![(0, s.clone())]).collect();
    }
    split_cuts(&cuts, [up, down], zs, slices, p)
}

pub fn regions(src: &SdfSource, m: &MaterialFn, zs: &[f64], slices: &[Vec<ExPolygon>], p: &PaintParams) -> Vec<LayerPaint> {
    let n = zs.len();
    // Each layer's segments and states from its slab (the same reduced tree).
    let cuts: Vec<Vec<(Pt, Pt, u32)>> = crate::par::map(n, |li| {
        let (segs, lm) = src.segments_with_material(zs[li]);
        tag_segments(&segs, zs[li], lm.as_ref().unwrap_or(m))
    });
    split_with_flats(cuts, &|li| src.at(zs[li]).material.unwrap_or_else(|| m.clone()), zs, slices, p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::ContourSource;

    /// A 20 mm cube (0..20) whose x > 10 half is filament 2.
    fn cube() -> SdfSource {
        let f = |x: f64, y: f64, z: f64| {
            let d = [(x - 10.0).abs() - 10.0, (y - 10.0).abs() - 10.0, (z - 10.0).abs() - 10.0];
            let o = (d[0].max(0.0).powi(2) + d[1].max(0.0).powi(2) + d[2].max(0.0).powi(2)).sqrt();
            o + d[0].max(d[1]).max(d[2]).min(0.0)
        };
        let m: MaterialFn = Arc::new(|x: f64, _y: f64, _z: f64| if x > 10.0 { 2 } else { 0 });
        SdfSource::new(Arc::new(f), ([0.0; 3], [20.0; 3])).with_material(m)
    }

    #[test]
    fn segments_split_where_the_state_changes() {
        let m: MaterialFn = Arc::new(|x: f64, _y: f64, _z: f64| if x > 10.0 { 2 } else { 0 });
        let t = tag_segments(&[(Pt::from_mm(0.0, 0.0), Pt::from_mm(20.0, 0.0))], 1.0, &m);
        assert_eq!(t.len(), 2);
        assert_eq!((t[0].2, t[1].2), (0, 2));
        assert!((t[0].1.x_mm() - 10.0).abs() < 1e-3, "{}", t[0].1.x_mm());
    }

    #[test]
    fn a_two_material_field_splits_its_layers() {
        let src = cube();
        assert!(src.painted());
        let zs: Vec<f64> = (0..10).map(|i| 0.2 + 2.0 * i as f64).collect();
        let slices = src.contours(&zs);
        let p = PaintParams { top_layers: 0, bottom_layers: 0, shrink: 0.0, max_width: 0.0, interlocking_depth: 0.0 };
        let r = src.paint_regions(&zs, &slices, &p);
        // Layer 1 is the base filament; above it both halves.
        assert_eq!(r[0].len(), 1);
        for l in &r[1..] {
            let a = |st: u32| l.iter().filter(|x| x.0 == st).flat_map(|x| x.1.iter()).map(|e| e.area_mm2()).sum::<f64>();
            assert!((a(0) - 200.0).abs() < 2.0 && (a(2) - 200.0).abs() < 2.0, "{} {}", a(0), a(2));
        }
    }

    #[test]
    fn painted_top_reaches_down_its_layers() {
        // The top half of the cube (z > 10) painted: its top face colours
        // `top_layers` layers below it across the whole section.
        let f = |x: f64, y: f64, z: f64| {
            let d = [(x - 10.0).abs() - 10.0, (y - 10.0).abs() - 10.0, (z - 10.0).abs() - 10.0];
            let o = (d[0].max(0.0).powi(2) + d[1].max(0.0).powi(2) + d[2].max(0.0).powi(2)).sqrt();
            o + d[0].max(d[1]).max(d[2]).min(0.0)
        };
        let m: MaterialFn = Arc::new(|_x: f64, _y: f64, z: f64| if z > 18.0 { 1 } else { 0 });
        let src = SdfSource::new(Arc::new(f), ([0.0; 3], [20.0; 3])).with_material(m);
        let zs: Vec<f64> = (0..40).map(|i| 0.5 * (i + 1) as f64 - 0.25).collect();
        let slices = src.contours(&zs);
        let p = PaintParams { top_layers: 3, bottom_layers: 0, shrink: 0.0, max_width: 0.0, interlocking_depth: 0.0 };
        let r = src.paint_regions(&zs, &slices, &p);
        let top = r.last().unwrap();
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].0, 1);
        assert!((top[0].1.iter().map(|e| e.area_mm2()).sum::<f64>() - 400.0).abs() < 2.0);
    }
}
