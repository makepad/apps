//! Per-pattern checks on a square region with a hole: length per area
//! against the density convention, nothing outside the region, coverage
//! (no point of the region far from a line), determinism, and a budget of
//! points per layer.

use crate::infill::{self, Aux, SparseIn, SparseOpts, Stroke};
use crate::pattern::{Sparse, Surface};
use crate::patterns::Ctx;
use makepad_fab_slicer::geom::*;

const S: f64 = 0.407; // 0.45 bead at 0.2 mm
const D: f64 = 0.15;

fn region(size: f64) -> Vec<ExPolygon> {
    let h = size / 2.0;
    let mut e = ExPolygon::new(Polygon::new(vec![Pt::from_mm(-h, -h), Pt::from_mm(h, -h), Pt::from_mm(h, h), Pt::from_mm(-h, h)]));
    let mut hole = Polygon::new(vec![Pt::from_mm(-h / 4.0, -h / 4.0), Pt::from_mm(h / 4.0, -h / 4.0), Pt::from_mm(0.0, h / 4.0)]);
    if hole.is_ccw() {
        hole.reverse();
    }
    e.holes.push(hole);
    vec![e]
}

fn opts() -> SparseOpts {
    SparseOpts {
        shift_step: 0.4,
        rotate_step: 0.0,
        lattice: [(-45f64).to_radians(), 45f64.to_radians()],
        skin: Sparse::CrossZag,
        skeleton: Sparse::ZigZag,
        skin_density: D,
        skeleton_density: D,
        skin_width: 0.45,
        skeleton_width: 0.45,
        skin_depth: 2.0,
        lock_depth: 1.0,
    }
}

/// An object of `n` layers of the region: sparse below, solid on top.
fn object(r: &[ExPolygon], n: usize) -> (Vec<Vec<ExPolygon>>, Vec<Vec<ExPolygon>>, Vec<(f64, f64)>) {
    let fill = vec![r.to_vec(); n];
    let mut sparse = fill.clone();
    sparse[n - 1].clear();
    let heights = (0..n).map(|i| (0.2 * (i + 1) as f64, 0.2)).collect();
    (sparse, fill, heights)
}

fn run(pat: Sparse, r: &[ExPolygon], aux: &Aux, li: usize) -> Vec<Stroke> {
    let inp = SparseIn {
        region: r,
        width: 0.45,
        spacing: S,
        density: D,
        ctx: Ctx { li, z: 0.2 * (li + 1) as f64, pivot: Pt::new(0, 0), dir: 45f64.to_radians() },
        anchor: 1.628,
        max: 20.0,
        reach: 3.6,
        bridge_loop: false,
        height: 0.2,
    };
    infill::sparse(pat, &opts(), aux, &inp)
}

fn length(st: &[Stroke]) -> f64 {
    st.iter().map(|s| {
        let mut l: f64 = s.pts.windows(2).map(|w| w[0].dist(w[1])).sum();
        if s.closed {
            l += s.pts[0].dist(*s.pts.last().unwrap());
        }
        l / SCALE
    }).sum()
}

fn segments(st: &[Stroke]) -> Vec<(Pt, Pt)> {
    let mut v = Vec::new();
    for s in st {
        v.extend(s.pts.windows(2).map(|w| (w[0], w[1])));
        if s.closed {
            v.push((*s.pts.last().unwrap(), s.pts[0]));
        }
    }
    v
}

/// Every segment's midpoint and ends lie in the region (1 µm slack).
fn inside(st: &[Stroke], r: &[ExPolygon]) -> bool {
    let grown = makepad_fab_slicer::offset::offset_ex(r, 0.001 * SCALE, makepad_fab_slicer::offset::DEFAULT_MITER);
    segments(st).iter().all(|(a, b)| [*a, *b, lerp(*a, *b, 0.5)].iter().all(|p| grown.iter().any(|e| e.contains(*p))))
}

/// The largest distance from a grid of points in the region to the lines.
fn worst_gap(st: &[Stroke], r: &[ExPolygon], step: f64) -> f64 {
    let segs = segments(st);
    let bb = bbox_of(r);
    let mut worst = 0.0f64;
    let mut y = bb.min.y as f64 + step * SCALE / 2.0;
    while y < bb.max.y as f64 {
        let mut x = bb.min.x as f64 + step * SCALE / 2.0;
        while x < bb.max.x as f64 {
            let p = Pt::new(x as i64, y as i64);
            if r.iter().any(|e| e.contains(p)) {
                let d = segs.iter().map(|(a, b)| dist_point_segment(p, *a, *b)).fold(f64::INFINITY, f64::min);
                worst = worst.max(d / SCALE);
            }
            x += step * SCALE;
        }
        y += step * SCALE;
    }
    worst
}

/// Expected length per area relative to d/s, the allowed band, and the
/// largest gap (in units of base = s/d) for each uniform pattern.
fn expectation(p: Sparse) -> Option<(f64, f64, f64)> {
    Some(match p {
        Sparse::Honeycomb => (4.0 / 3.0, 0.12, 1.0),
        Sparse::Honeycomb3d => (1.0, 0.15, 1.2),
        Sparse::Grid => (1.0, 0.1, 1.0),
        Sparse::Triangles | Sparse::TriHexagon | Sparse::Cubic => (1.0, 0.1, 2.0),
        Sparse::Lattice2d => (1.0, 0.1, 2.0),
        Sparse::Gyroid => (1.0, 0.15, 1.0),
        // Folded transition layers run up to √2 longer.
        Sparse::CrossHatch => (1.15, 0.3, 1.0),
        Sparse::Hilbert | Sparse::ArchimedeanChords | Sparse::OctagramSpiral => (1.0, 0.12, 1.0),
        Sparse::Concentric => (1.0, 0.1, 1.0),
        Sparse::Rectilinear | Sparse::AlignedRectilinear | Sparse::ZigZag | Sparse::CrossZag | Sparse::Line => (1.0, 0.1, 0.75),
        // Skin and skeleton overlap by the lock depth and the skin's short
        // lines join along a narrow band (the reference: 1.28 of d/s).
        Sparse::LockedZag => (1.35, 0.25, 1.0),
        Sparse::Lightning | Sparse::AdaptiveCubic | Sparse::SupportCubic => return None,
    })
}

#[test]
fn every_sparse_pattern_density_containment_coverage() {
    let r = region(38.0);
    let area = total_area_mm2(&r);
    let base = S / D;
    let mut report = String::new();
    for (name, pat) in Sparse::ALL {
        let (sparse, fill, heights) = object(&r, 60);
        let aux = infill::prepare(pat, &sparse, &fill, &heights, S, D, Pt::new(0, 0));
        for li in [10, 11, 12, 40, 41, 57] {
            let st = run(pat, &r, &aux, li);
            assert!(inside(&st, &r), "{name} layer {li}: a path leaves the region");
            let per = length(&st) / area / (D / S);
            report += &format!("{name:>18} L{li}: {:.3} of d/s\n", per);
            if let Some((want, band, gap)) = expectation(pat) {
                // Joins and anchors along the walls add length.
                assert!(per >= want * (1.0 - band) && per <= want * (1.0 + band) + 0.1, "{name} L{li}: length {per:.3} of d/s, want {want:.3}");
                let g = worst_gap(&st, &r, 0.7);
                assert!(g <= gap * base + 1.0, "{name} L{li}: a point {g:.2} mm from any line (base {base:.2})");
            }
        }
    }
    eprintln!("{report}");
}

#[test]
fn cross_layer_patterns_follow_the_surfaces() {
    let r = region(38.0);
    let (sparse, fill, heights) = object(&r, 60);
    let len = |pat, li| {
        let aux = infill::prepare(pat, &sparse, &fill, &heights, S, D, Pt::new(0, 0));
        let st = run(pat, &r, &aux, li);
        assert!(inside(&st, &r));
        length(&st)
    };
    let cubic = len(Sparse::Cubic, 10);
    // Adaptive and support cubic: full cubic density under the top,
    // sparser far below it, never denser than cubic.
    for pat in [Sparse::AdaptiveCubic, Sparse::SupportCubic] {
        let near = len(pat, 58);
        let far = len(pat, 10);
        assert!(far < 0.7 * cubic, "{pat:?} far {far} vs cubic {cubic}");
        assert!(near > far * 1.3, "{pat:?} near {near} far {far}");
        assert!(near <= cubic * 1.1);
    }
    // Lightning: dense right under the top, thinning downwards.
    let top = len(Sparse::Lightning, 58);
    let mid = len(Sparse::Lightning, 40);
    let low = len(Sparse::Lightning, 5);
    assert!(top > 0.0 && top >= mid && mid >= low, "lightning {top} {mid} {low}");
    assert!(top < cubic);
}

#[test]
fn deterministic() {
    let r = region(38.0);
    let (sparse, fill, heights) = object(&r, 30);
    for (_, pat) in Sparse::ALL {
        let a1 = infill::prepare(pat, &sparse, &fill, &heights, S, D, Pt::new(0, 0));
        let a2 = infill::prepare(pat, &sparse, &fill, &heights, S, D, Pt::new(0, 0));
        let x = run(pat, &r, &a1, 27);
        let y = run(pat, &r, &a2, 27);
        assert_eq!(x.len(), y.len(), "{pat:?}");
        for (p, q) in x.iter().zip(&y) {
            assert_eq!(p.pts, q.pts, "{pat:?}");
        }
    }
}

#[test]
fn surface_patterns_cover_solid() {
    let r = region(30.0);
    let area = total_area_mm2(&r);
    for (name, pat) in Surface::ALL {
        let ctx = Ctx { li: 3, z: 0.8, pivot: Pt::new(0, 0), dir: 45f64.to_radians() };
        let Some(st) = infill::surface(pat, &r, S, 0.45, &ctx) else { continue };
        assert!(inside(&st, &r), "{name}");
        let per = length(&st) * S / area;
        assert!((0.9..1.15).contains(&per), "{name}: solid length {per:.3} of full");
        let g = worst_gap(&st, &r, 0.5);
        assert!(g <= 0.6, "{name}: gap {g:.2}");
    }
}

/// Infill is hot, and what it costs downstream is the points it hands on: a 100 mm square layer of
/// each pattern stays within a budget of them. A limit on time here would measure the box's load.
#[test]
fn per_layer_point_budget() {
    let r = region(100.0);
    let (sparse, fill, heights) = object(&r, 20);
    let points = |st: &[Stroke]| st.iter().map(|s| s.pts.len()).sum::<usize>();
    for (name, pat) in Sparse::ALL {
        let aux = infill::prepare(pat, &sparse, &fill, &heights, S, D, Pt::new(0, 0));
        let worst = (0..10).map(|li| points(&run(pat, &r, &aux, li))).max().unwrap();
        assert!(worst < 16_000, "{name} hands on {worst} points per 100 mm layer");
    }
    for (name, pat) in Surface::ALL {
        let ctx = Ctx { li: 3, z: 0.8, pivot: Pt::new(0, 0), dir: 45f64.to_radians() };
        let n = infill::surface(pat, &r, S, 0.45, &ctx).map_or(0, |st| points(&st));
        assert!(n < 80_000, "surface {name} hands on {n} points per 100 mm layer");
    }
}

#[test]
fn unknown_pattern_is_an_error() {
    assert!(Sparse::parse("cubic").is_some());
    assert!(Sparse::parse("cubbic").is_none());
    assert!(Surface::parse("hilbertcurve").is_some());
    assert!(Surface::parse("grid").is_none());
}
