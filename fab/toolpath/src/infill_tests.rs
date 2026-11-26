//! Per-pattern checks on a square region with a hole: length per area
//! against the density convention, nothing outside the region, coverage
//! (no point of the region far from a line), determinism, and a budget of
//! points per layer.

use crate::infill::{self, Aux, SparseIn, SparseOpts, Stroke};
use crate::pattern::{Sparse, Surface};
use crate::patterns::Ctx;
use crate::plate_tests::{input, job};
use crate::{check, par, plan_one, ObjectInput, ObjectPlan, Role};
use makepad_fab_settings::machine::Job;
use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::sdf::SdfSource;

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
        adaptive: None,
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
        Sparse::Gyroid | Sparse::TpmsD | Sparse::TpmsFk => (1.0, 0.15, 1.0),
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

/// Mark vertices covered by each target segment, searching only its x interval.
fn gyroid_vertices_match(from: &crate::clipper::Pieces, to: &crate::clipper::Pieces) -> bool {
    let mut vertices: Vec<_> = from.pts.iter().map(|&pt| (pt, false)).collect();
    vertices.sort_unstable_by_key(|v| v.0.x);
    let tol = scaled(0.02);
    for i in 0..to.len() {
        for w in to.piece(i).windows(2) {
            let (a, b) = (w[0], w[1]);
            let lo = vertices.partition_point(|v| v.0.x < a.x.min(b.x) - tol);
            let hi = vertices.partition_point(|v| v.0.x <= a.x.max(b.x) + tol);
            for (p, hit) in &mut vertices[lo..hi] {
                if !*hit && p.y >= a.y.min(b.y) - tol && p.y <= a.y.max(b.y) + tol {
                    *hit = dist_point_segment(*p, a, b) <= tol as f64;
                }
            }
        }
    }
    vertices.iter().all(|v| v.1)
}

#[test]
fn tpms_sections_follow_the_field_and_meet_the_walls() {
    use crate::clipper::{Clipper, Pieces, FREE};
    use crate::patterns::{gyroid, GYROID_PERIOD};
    use crate::tpms::{Tpms, DIAMOND_PERIOD, FISCHER_KOCH_PERIOD};
    use std::f64::consts::TAU;

    let r = region(38.0);
    let mut clip = Clipper::new(&r);
    let pivot = Pt::new(0, 0);
    let base = S / D;
    let mut closed_fk = false;
    for li in [0, 6, 11] {
        let z = 0.2 * (li + 1) as f64;
        for (surface, period, pattern) in [
            (Tpms::Gyroid, 2.0 * GYROID_PERIOD * base, None),
            (Tpms::Diamond, DIAMOND_PERIOD * base, Some(Sparse::TpmsD)),
            (Tpms::FischerKoch, FISCHER_KOCH_PERIOD * base, Some(Sparse::TpmsFk)),
        ] {
            let height = if pattern.is_none() { z - period / 4.0 } else { z };
            let (pieces, loops) = surface.contours(&mut clip, pivot, height, period);
            assert!(!pieces.is_empty(), "{surface:?} L{li}: no wall-to-wall contours");
            if let Some(pat) = pattern {
                assert!(pieces.ea.iter().chain(&pieces.eb).all(|&e| e != FREE), "{pat:?} L{li}: clipped contour has a free end");
                for stroke in run(pat, &r, &Aux::None, li) {
                    if stroke.closed {
                        let ring = Polygon::new(stroke.pts);
                        assert!(ring.perimeter() >= 4.0 * stroke.width * SCALE, "{pat:?} L{li}: printed a dot-sized loop");
                        closed_fk |= pat == Sparse::TpmsFk;
                    } else {
                        for p in [stroke.pts[0], *stroke.pts.last().unwrap()] {
                            assert!(clip.boundary_within(p, scaled(0.01)) <= scaled(0.005) as f64, "{pat:?} L{li}: stroke end {p:?} inside the region");
                        }
                    }
                }
            } else {
                let mut analytic = Pieces::default();
                gyroid(&mut clip, z, base, pivot, &mut analytic);
                assert!(loops.is_empty(), "gyroid L{li}: found a closed loop between analytic waves");
                assert!(gyroid_vertices_match(&pieces, &analytic), "gyroid L{li}: contour vertex over 0.02 mm from analytic lines");
                assert!(gyroid_vertices_match(&analytic, &pieces), "gyroid L{li}: analytic vertex over 0.02 mm from contour lines");
                let reference = analytic.length_mm();
                assert!((pieces.length_mm() - reference).abs() <= 0.01 * reference, "gyroid L{li}: raw lengths differ by over 1%");
            }

            // Independent textbook field and its in-plane gradient per mm. Include the
            // vertices clipping creates on the outer wall and the wall of the hole.
            let k = TAU / period;
            let (sz, cz) = (k * height).sin_cos();
            let field = |x: f64, y: f64| {
                let (x, y) = (k * x, k * y);
                let (sx, cx) = x.sin_cos();
                let (sy, cy) = y.sin_cos();
                match surface {
                    Tpms::Gyroid => sx * cy + sy * cz + sz * cx,
                    Tpms::Diamond => sx * sy * sz + sx * cy * cz + cx * sy * cz + cx * cy * sz,
                    Tpms::FischerKoch => (2.0 * x).cos() * sy * cz + cx * (2.0 * y).cos() * sz + sx * cy * (2.0 * k * height).cos(),
                }
            };
            let delta = 1e-5;
            for &pt in pieces.pts.iter().chain(loops.iter().flatten()) {
                let p = pt.sub(pivot);
                let (x, y) = (p.x_mm(), p.y_mm());
                let dx = (field(x + delta, y) - field(x - delta, y)) / (2.0 * delta);
                let dy = (field(x, y + delta) - field(x, y - delta)) / (2.0 * delta);
                let error = field(x, y).abs() / dx.hypot(dy);
                assert!(error < 0.005, "{surface:?} L{li}: contour vertex {pt:?} is {error:.6} mm off the zero set");
            }
        }
    }
    assert!(closed_fk, "Fischer-Koch S: no interior loop survived as a closed stroke");
}

#[test]
fn graded_tpms_cube_sections() {
    use crate::clipper::{Clipper, Pieces, FREE};
    use crate::grade::{graded, Adaptive, Depth};
    use crate::patterns::GYROID_PERIOD;
    use crate::tpms::{DIAMOND_PERIOD, FISCHER_KOCH_PERIOD};
    use crate::warp::Gradient;
    use std::sync::Arc;
    use std::time::Instant;

    let (sparse, interior, h) = (0.2, 0.1, 0.2);
    let base = S / sparse;
    let period = 2.0 * GYROID_PERIOD * base;
    let square = |lo, hi| [ExPolygon::new(Polygon::rect(Pt::from_mm(lo, lo), Pt::from_mm(hi, hi)))];
    let outline = square(0.0, 30.0);
    let layers: Vec<_> = (1..=150).map(|i| (i as f64 * h, h, &outline[..])).collect();
    let build = |i, lobes| Arc::new(graded(&layers, &Adaptive { interior: i, gradient: Gradient::Linear, lobes }, sparse, period));
    let g = build(interior, false);
    let lobes = build(interior, true);
    for z in [0.0, 7.3, 15.0, 29.8] { for y in [0.4, 11.7, 26.0] { for x in [1.1, 15.0, 30.0] {
        let p = [x, y, z];
        let (a, b) = (g.warp.at(p), lobes.warp.at(p));
        assert_eq!(a.map(f64::to_bits), b.map(f64::to_bits), "30 mm cube warp at {p:?}: distance {a:?}, lobes {b:?}");
    }}}
    let aux = Aux::Warp(Arc::clone(&g));
    let zero = Aux::Warp(build(sparse, false));
    // The constant outline occupies one slab. Sample the same voxel-centred t as the builder.
    let depth = Depth::new(&[(0.0, 30.0, &outline)], period);
    let shell = |t: f64| (t >= 0.5) as usize;
    let wall = 0.5;
    let region = square(wall, 30.0 - wall);
    let clip = Clipper::new(&region);
    let pivot = bbox_of(&outline).center();
    let o = opts();
    let as_pieces = |strokes: &[Stroke]| {
        let mut pieces = Pieces::default();
        for s in strokes {
            pieces.push(&s.pts, FREE, FREE);
            if s.closed { pieces.push(&[*s.pts.last().unwrap(), s.pts[0]], FREE, FREE); }
        }
        pieces
    };
    let mut failures = Vec::new();
    let mut report = format!("stretch {:.5}, scales {:?}, build {:.2} ms\n", g.stretch, g.scales, g.ms);
    for (pat, band) in [(Sparse::Gyroid, period), (Sparse::TpmsD, DIAMOND_PERIOD * base), (Sparse::TpmsFk, FISCHER_KOCH_PERIOD * base)] {
        let first = layers.partition_point(|l| l.0 < 15.0 - band / 2.0);
        let end = layers.partition_point(|l| l.0 < 15.0 + band / 2.0);
        // Two shells, outside and inside half the depth: a TPMS section's own density varies at
        // the scale of its period, so shells much smaller than a few periods only measure that.
        let mut area = [0.0; 2];
        let mut asked = [0.0; 2];
        let mut lengths = [0.0; 2];
        let mut plain_lengths = [0.0; 2];
        let mut times = Vec::new();
        for li in first..end {
            let z = layers[li].0;
            // Omit optional wall runs so the measured length comes from the TPMS section.
            let inp = SparseIn { region: &region, width: 0.45, spacing: S, density: sparse,
                ctx: Ctx { li, z, pivot, dir: 0.0 }, anchor: 0.0, max: 0.0, reach: 0.0, bridge_loop: false, height: h };
            let timed = |aux| {
                let start = Instant::now();
                let strokes = infill::sparse(pat, &o, aux, &inp);
                (strokes, start.elapsed().as_secs_f64() * 1000.0)
            };
            let (warped, warped_ms) = timed(&aux);
            let (plain, plain_ms) = timed(&Aux::None);
            times.push((li, warped_ms, plain_ms));
            if warped.is_empty() { failures.push(format!("{pat:?} L{li}: empty warped section")); }
            for s in &warped {
                if s.closed {
                    let mm = length(std::slice::from_ref(s));
                    if mm < 4.0 * s.width { failures.push(format!("{pat:?} L{li}: closed stroke only {mm:.5} mm")); }
                } else {
                    for p in [s.pts[0], *s.pts.last().unwrap()] {
                        let gap = clip.boundary_within(p, scaled(30.0)) / SCALE;
                        if gap > 0.005 { failures.push(format!("{pat:?} L{li}: open end {p:?} is {gap:.5} mm from the wall")); }
                    }
                }
            }
            let identity = as_pieces(&infill::sparse(pat, &o, &zero, &inp));
            let uniform = as_pieces(&plain);
            let exact = identity.pts == uniform.pts && identity.span == uniform.span;
            if !exact && !(gyroid_vertices_match(&identity, &uniform) && gyroid_vertices_match(&uniform, &identity)) {
                failures.push(format!("{pat:?} L{li}: zero warp and plain strokes differ by over 0.02 mm"));
            }

            // Shift the 0.5 mm sample grid across the band so square depth shells do not
            // always round outwards or inwards. Each sample still covers exactly 0.25 mm².
            let phase = ((li - first) as f64 + 0.5) / (end - first) as f64;
            let side = ((30.0 - 2.0 * wall) / 0.5) as usize;
            for j in 0..side { for i in 0..side {
                let t = depth.at([wall + (i as f64 + phase) * 0.5, wall + (j as f64 + phase) * 0.5, z]).0;
                area[shell(t)] += 0.25;
                asked[shell(t)] += 0.25 * Gradient::Linear.density(sparse, interior, t) / sparse;
            }}
            for (strokes, bins) in [(&warped, &mut lengths), (&plain, &mut plain_lengths)] {
                for (a, b) in segments(strokes) {
                    let p = lerp(a, b, 0.5);
                    bins[shell(depth.at([p.x_mm(), p.y_mm(), z]).0)] += a.dist(b) / SCALE;
                }
            }
        }
        let ratio = |i: usize| lengths[i] / plain_lengths[i];
        let want = |i: usize| asked[i] / area[i];
        let total = lengths.iter().sum::<f64>() / plain_lengths.iter().sum::<f64>();
        let want_total = asked.iter().sum::<f64>() / area.iter().sum::<f64>();
        let band = format!("{pat:?} layers {first}..{end}");
        report += &format!("{band}: outer shell {:.4} (asked {:.4}), inner shell {:.4} (asked {:.4}), total {total:.4} (asked {want_total:.4})\n  (layer, warped ms, plain ms) {times:.3?}\n",
            ratio(0), want(0), ratio(1), want(1));
        if !(ratio(1) <= ratio(0) - 0.1) { failures.push(format!("{band}: the inner shell is not sparser than the outer one")); }
        if !((ratio(1) - want(1)).abs() <= 0.12) { failures.push(format!("{band}: the inner shell's density misses the asked one")); }
        if !((total - want_total).abs() <= 0.05) { failures.push(format!("{band}: the layers' plastic misses the asked mean")); }
    }
    assert!(failures.is_empty(), "{}\n{report}", failures.join("\n"));
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

fn cube() -> ObjectInput {
    let field = |x: f64, y: f64, z: f64| (x - 12.0).abs().max((y - 12.0).abs()).max((z - 12.0).abs()) - 12.0;
    input(Box::new(SdfSource::new(std::sync::Arc::new(field), ([0.0; 3], [24.0; 3]))))
}

fn gyroid_job() -> Job {
    let mut j = job("classic");
    j.cfg.set_str("sparse_infill_density", "20%");
    j.cfg.set_str("sparse_infill_pattern", "gyroid");
    j
}

fn plan_cube(cube: &ObjectInput, j: &Job) -> ObjectPlan {
    check(j).expect("the cube's infill settings must pass validation");
    plan_one(par::Ctl::default(), 0, cube, j).expect("the cube must plan to completion")
}

fn assert_same_layers(a: &ObjectPlan, b: &ObjectPlan, case: &str) {
    assert_eq!(a.layers.len(), b.layers.len(), "{case} changed the layer count");
    for (k, (a, b)) in a.layers.iter().zip(&b.layers).enumerate() {
        assert!(format!("{a:?}") == format!("{b:?}"), "{case} changed the plan at layer {k}");
    }
}

#[test]
fn inactive_adaptive_settings_preserve_the_plan() {
    let cube = cube();
    // A 6 mm slab: no body a period deep, so nothing grades.
    let field = |x: f64, y: f64, z: f64| ((x - 12.0).abs() - 12.0).max((y - 12.0).abs() - 12.0).max((z - 3.0).abs() - 3.0);
    let slab = input(Box::new(SdfSource::new(std::sync::Arc::new(field), ([0.0; 3], [24.0, 24.0, 6.0]))));
    let base = gyroid_job();
    for (part, name, pattern, mode) in [(&cube, "cube", "gyroid", "disabled"), (&cube, "cube", "grid", "distance_warp"), (&slab, "slab", "gyroid", "distance_warp")] {
        let mut j = base.clone();
        j.cfg.set_str("sparse_infill_pattern", pattern);
        let plain = plan_cube(part, &j);
        j.cfg.set_str("adaptive_infill_mode", mode);
        j.cfg.set_str("adaptive_infill_interior_density", "5%");
        j.cfg.set_str("adaptive_infill_gradient", "exponential");
        assert_same_layers(&plain, &plan_cube(part, &j), &format!("{mode} on the {name}'s {pattern}"));
    }
}

#[test]
fn distance_warp_reduces_the_cubes_sparse_infill() {
    let cube = cube();
    let mut j = gyroid_job();
    let lengths = |plan: &ObjectPlan| -> Vec<f64> {
        plan.layers.iter().map(|(_, _, layer)| {
            layer.islands.iter().flat_map(|i| &i.fills).filter(|p| p.role == Role::SparseInfill).map(|p| p.length_mm()).sum()
        }).collect()
    };
    let plain = lengths(&plan_cube(&cube, &j));
    j.cfg.set_str("adaptive_infill_mode", "distance_warp");
    j.cfg.set_str("adaptive_infill_interior_density", "10%");
    j.cfg.set_str("adaptive_infill_gradient", "linear");
    let planned = plan_cube(&cube, &j);
    assert_same_layers(&planned, &plan_cube(&cube, &j), "repeating the distance_warp slice");
    j.cfg.set_str("adaptive_infill_mode", "lobes");
    assert_same_layers(&planned, &plan_cube(&cube, &j), "grading a single-lobe cube with lobes");
    let graded = lengths(&planned);
    assert_eq!(graded.len(), plain.len(), "grading changed the cube's layer count");
    let (total, baseline) = (graded.iter().sum::<f64>(), plain.iter().sum::<f64>());
    assert!(total < baseline, "graded sparse infill used {total} mm across the cube, plain used {baseline} mm");
    for k in graded.len() / 3..2 * graded.len() / 3 {
        assert!(graded[k] > 0.0 && graded[k] < plain[k], "middle layer {k} must retain shorter sparse infill, graded {} mm, plain {} mm", graded[k], plain[k]);
    }
}

#[test]
fn adaptive_settings_are_checked_for_tpms() {
    let mut j = gyroid_job();
    j.cfg.set_str("adaptive_infill_mode", "lobes");
    check(&j).expect("gyroid must accept lobes");
    let adaptive = SparseOpts::from_job(&j, 0.4, 0.45).adaptive;
    assert!(adaptive.is_some_and(|a| a.lobes), "gyroid must plan the selected lobes mode, got {adaptive:?}");
    j.cfg.set_str("adaptive_infill_mode", "normal_z");
    let err = check(&j).expect_err("gyroid must refuse the unbuilt normal_z mode");
    assert!(err.contains("adaptive_infill_mode \"normal_z\" is not built in Fab yet (built: disabled, distance_warp, lobes)"), "unbuilt mode diagnostic: {err}");
    j.cfg.set_str("sparse_infill_pattern", "grid");
    check(&j).expect("grid must ignore the normal_z mode");
    j.cfg.set_str("sparse_infill_pattern", "gyroid");
    j.cfg.set_str("adaptive_infill_mode", "distance_warp");
    j.cfg.set_str("adaptive_infill_interior_density", "0%");
    let err = check(&j).expect_err("distance_warp must refuse zero interior density");
    assert!(err.contains("adaptive_infill_interior_density \"0%\""), "zero interior density diagnostic: {err}");
}
