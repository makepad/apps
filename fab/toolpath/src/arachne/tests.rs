use super::*;
use crate::walls::{bead_paths, SeamMode, WallOrder};
use crate::Role;

/// Golden widths (0.42 outer, 0.45 inner at 0.2 mm) and the default settings at a 0.4 nozzle.
fn cfg(loops: usize) -> Config {
    let s = Settings { min_bead_width: 0.34, min_feature_size: 0.10, transition_length: 0.4, transition_filter_deviation: 0.1, transition_angle: 10f64.to_radians(), distribution_count: 1 };
    s.config(loops, 0.42, 0.45, 0.2, false)
}

#[test]
fn thick_square_corner_layouts_equal_reach_anchors() {
    let c = cfg(2);
    let st = beading::Strategy::new(&c).unwrap();
    // The fixed nominal layout is the cap+1 state, including the fill marker.
    let r_cap = st.transition_thickness(st.cap) * 0.5;
    let need = r_cap + c.transition_length + c.transition_filter_deviation;
    let mut beading = beading::Beading::default();
    st.compute_into(2.0 * need, st.cap + 1, &mut beading);
    let expected: Vec<_> = beading.locations.iter().zip(&beading.widths).take(c.loops).map(|(&l, &w)| (l, w)).collect();
    let marker = beading.locations[c.loops];
    let prepared = prepare(&rect(20.0, 20.0), &c);
    assert_eq!(prepared.len(), 1);
    let mut sk = skeleton::build(&prepared[0], c.transition_angle, st.transition_thickness(st.cap) + c.s_inner);
    let (halves, owners) = beads::layouts(&mut sk, &c, &st);
    let mut corners = [0usize; 4];
    let mut worst = 0.0f64;
    for (i, node) in sk.nodes.iter().enumerate() {
        if node.r / SCALE > need { continue; }
        let x = node.pos.x / SCALE;
        let y = node.pos.y / SCALE;
        if x.min(20.0 - x) > need || y.min(20.0 - y) > need { continue; }
        let half = &halves[owners[i] as usize];
        assert_eq!(half.side.len(), expected.len(), "corner ({x},{y}) count agrees with cap anchor");
        assert_eq!(half.mid, 0.0, "corner has no middle bead");
        for (&(l, w), &(el, ew)) in half.side.iter().zip(&expected) { worst = worst.max((l - el).abs()).max((w - ew).abs()); }
        assert!(half.marker.is_finite(), "corner inherits the cap fill marker");
        worst = worst.max((half.marker - marker).abs());
        corners[usize::from(x > 10.0) + 2 * usize::from(y > 10.0)] += 1;
    }
    assert!(corners.iter().all(|&n| n != 0), "all four corners sampled: {corners:?}");
    println!("W1b 20mm-square CPU corner anchors: R_cap={r_cap:.9} R_need={need:.9} nodes={corners:?} max_layout_error={worst:.9}");
    assert!(worst <= 0.001, "CPU propagated corner layouts equal fixed cap anchors");
}

fn poly(pts: &[(f64, f64)]) -> Polygon {
    Polygon::new(pts.iter().map(|&(x, y)| Pt::from_mm(x, y)).collect())
}

fn rect(x1: f64, y1: f64) -> ExPolygon {
    ExPolygon::new(poly(&[(0.0, 0.0), (x1, 0.0), (x1, y1), (0.0, y1)]))
}

fn circle(cx: f64, cy: f64, r: f64, ccw: bool) -> Polygon {
    let mut p = poly(&(0..64).map(|k| {
        let a = k as f64 * std::f64::consts::TAU / 64.0;
        (cx + r * a.cos(), cy + r * a.sin())
    }).collect::<Vec<_>>());
    if !ccw {
        p.reverse();
    }
    p
}

fn taper() -> ExPolygon {
    ExPolygon::new(poly(&[(0.0, 0.0), (10.0, 0.6), (10.0, 1.0), (0.0, 1.6)]))
}

/// The beads a cut across x = `x` meets: (bead width, middle bead) per crossing.
fn cut(w: &Walls, x: f64) -> Vec<(f64, bool)> {
    let mut out = Vec::new();
    for l in &w.lines {
        let n = l.pts.len();
        for i in 0..if l.closed { n } else { n - 1 } {
            let (a, b) = (l.pts[i], l.pts[(i + 1) % n]);
            let (xa, xb) = (a.x_mm(), b.x_mm());
            if (xa < x) != (xb < x) {
                let t = (x - xa) / (xb - xa);
                out.push((l.widths[i] + (l.widths[(i + 1) % n] - l.widths[i]) * t, l.odd));
            }
        }
    }
    out
}

fn total(c: &[(f64, bool)]) -> f64 {
    c.iter().map(|b| b.0).sum()
}

/// R6B §5.18: walls standing alone, two loops, true part widths. The beads
/// across fill the outline less the half overlap at each side (0.043).
#[test]
fn strips_get_the_expected_beads() {
    let c = cfg(2);
    let near = |v: f64, want: f64| (v - want).abs() < 0.006;
    assert!(walls(&rect(3.0, 0.08), &c).lines.is_empty());
    for (t, beads) in [(0.30, &[0.34][..]), (0.60, &[0.557]), (0.80, &[0.379, 0.379]), (1.20, &[0.377, 0.403, 0.377]), (1.80, &[0.377, 0.50, 0.50, 0.377]), (2.50, &[0.377, 0.407, 0.407, 0.377])] {
        let w = walls(&rect(3.0, t), &c);
        let mut across: Vec<f64> = cut(&w, 1.5).iter().map(|b| b.0).collect();
        across.sort_by(f64::total_cmp);
        let mut want = beads.to_vec();
        want.sort_by(f64::total_cmp);
        assert_eq!(across.len(), want.len(), "strip {t}: beads {across:?}");
        assert!(across.iter().zip(&want).all(|(a, b)| near(*a, *b)), "strip {t}: beads {across:?}, want {want:?}");
        // One loop round the strip from 0.8 up; a middle bead is one open line.
        let loops = w.lines.iter().filter(|l| l.closed && !l.odd).count();
        assert_eq!(loops, beads.len() / 2, "strip {t}");
        assert!(w.lines.iter().filter(|l| l.odd).all(|l| !l.closed && l.inset == beads.len() / 2), "strip {t}: middle bead inset");
        assert_eq!(w.inner.is_empty(), t < 2.0, "strip {t}: infill {:?}", w.inner.iter().map(|e| e.area_mm2()).collect::<Vec<_>>());
    }
    // The infill strip of the 2.5 mm wall: inside two loops of 0.377 + 0.407.
    let w = walls(&rect(6.0, 2.5), &c);
    let inner: f64 = w.inner.iter().map(|e| e.area_mm2()).sum();
    let band: f64 = 0.0215 + 0.377 + 0.407;
    assert!((inner - (6.0 - 2.0 * band) * (2.5 - 2.0 * band)).abs() < 0.05, "infill {inner}");
}

/// A wall narrowing from 1.6 to 0.4 mm over 10 mm (a 6.9° taper, under the
/// transition angle): four beads become three, two, one, and across it the
/// beads fill the width to 5 µm, and to 30 µm within a transition.
#[test]
fn a_taper_steps_down_and_stays_filled() {
    let w = walls(&taper(), &cfg(2));
    let slope = (0.06f64).atan().cos();
    let (mut counts, mut off) = (Vec::new(), Vec::new());
    // Cuts clear of the end caps (the inner loop turns 0.6 mm in).
    for k in 2..19 {
        let x = 0.5 * k as f64;
        let c = cut(&w, x);
        let want = (1.6 - 0.12 * x) * slope - 0.043;
        off.push(((total(&c) - want) * 1000.0).round() / 1000.0);
        // Within a transition the ending beads ramp down beside the new
        // ones; count the beads past the ramp's thin ends.
        counts.push(c.iter().filter(|b| b.0 >= 0.15).count());
    }
    // Within a transition's ramp the sum strays a little (two cuts land in one).
    assert!(off.iter().all(|d| d.abs() < 0.03) && off.iter().filter(|d| d.abs() > 0.005).count() <= 2, "bead sum less the width per cut: {off:?}");
    // Counts only fall along the taper, from four to one.
    assert_eq!((counts[0], *counts.last().unwrap()), (4, 1), "{counts:?}");
    assert!(counts.windows(2).all(|p| p[1] <= p[0]), "{counts:?}");
    // The single bead at the thin end is the outer wall.
    assert!(w.lines.iter().any(|l| l.odd && l.inset == 0 && l.pts.iter().any(|p| p.x_mm() > 9.0)));
}

/// A wedge wider than the transition angle (11.4°) keeps both walls per
/// side up to where they meet; its tip gets no centre bead.
#[test]
fn a_wide_wedge_keeps_its_walls() {
    let wedge = ExPolygon::new(poly(&[(0.0, 0.0), (12.0, -1.2), (12.0, 1.2)]));
    let w = walls(&wedge, &cfg(2));
    assert!(w.lines.iter().all(|l| l.closed && !l.odd), "{} lines", w.lines.len());
    assert_eq!(w.lines.len(), 2);
    let mut c: Vec<f64> = cut(&w, 9.0).iter().map(|b| b.0).collect();
    c.sort_by(f64::total_cmp);
    assert!(c.len() == 4 && (c[0] - 0.377).abs() < 0.002 && (c[3] - 0.407).abs() < 0.002, "{c:?}");
    // The outer loop reaches to where the wedge is two outer beads thick.
    let tip = w.lines.iter().flat_map(|l| l.pts.iter()).map(|p| p.x_mm()).fold(f64::MAX, f64::min);
    assert!((tip - (2.0 * 0.1886 + 0.043) / 0.2).abs() < 0.3, "tip {tip}");
}

/// Loops round a hole run clockwise, round the contour counter-clockwise,
/// and the infill keeps the hole.
#[test]
fn a_hole_gets_its_own_loops() {
    let mut part = rect(12.0, 12.0);
    part.holes.push(circle(6.0, 6.0, 2.0, false));
    let w = walls(&part, &cfg(2));
    let area = |l: &BeadLine| Polygon::new(l.pts.clone()).area_mm2();
    let outer: Vec<f64> = w.lines.iter().filter(|l| l.inset == 0).map(area).collect();
    assert!(outer.len() == 2 && outer.iter().any(|a| *a > 100.0) && outer.iter().any(|a| *a < -15.0), "{outer:?}");
    assert_eq!(w.lines.len(), 4);
    assert_eq!((w.inner.len(), w.inner[0].holes.len()), (1, 1));
    let band: f64 = 0.0215 + 0.377 + 0.407;
    let want = (12.0 - 2.0 * band).powi(2) - std::f64::consts::PI * (2.0 + band).powi(2);
    assert!((w.inner[0].area_mm2() - want).abs() < 0.3, "infill {} want {want}", w.inner[0].area_mm2());
}

/// Bead lines as paths: a constant-width loop prints at its width, a
/// narrowing line carries its width as flow, and a middle bead prints right
/// after the loop round it.
#[test]
fn bead_paths_carry_width_and_order() {
    let c = cfg(2);
    let h = 0.2;
    let plan = |w: &Walls, order| bead_paths(&w.lines, 0.42, h, 0.06, Pt::from_mm(100.0, 100.0), order, SeamMode::Aligned, 0.4, 0.2).0.into_iter().map(|p| p.0).collect::<Vec<_>>();
    let w = walls(&rect(3.0, 2.5), &c);
    let widths: Vec<(Role, f64, bool)> = plan(&w, WallOrder::InnerOuter).iter().map(|p| (p.role, (p.width * 1000.0).round() / 1000.0, p.flow.is_empty())).collect();
    assert_eq!(widths, vec![(Role::InnerWall, 0.45, true), (Role::OuterWall, 0.42, true)]);
    let strip = walls(&rect(3.0, 1.2), &c);
    let roles: Vec<(Role, bool)> = plan(&strip, WallOrder::InnerOuter).iter().map(|p| (p.role, p.closed)).collect();
    assert_eq!(roles, vec![(Role::OuterWall, true), (Role::InnerWall, false)]);
    // The taper: a path's flow takes each bead down to its own width.
    let t = walls(&taper(), &c);
    let mut narrowed = 0;
    for p in plan(&t, WallOrder::OuterInner) {
        if p.flow.is_empty() {
            continue;
        }
        narrowed += 1;
        let s_max = crate::flow::spacing(p.width, h);
        assert!(p.flow.iter().all(|f| *f > 0.0 && *f <= 1.0 + 1e-9));
        let line = t.lines.iter().find(|l| l.pts.contains(&p.pts[0])).expect("the path's line");
        let lo = line.widths.iter().cloned().filter(|w| *w > 0.0).fold(f64::MAX, f64::min);
        assert!(p.flow.iter().any(|f| (f * s_max - lo).abs() < 0.05), "the narrowest bead {lo} prints narrow");
    }
    assert!(narrowed > 0);
}

#[test]
fn settings_without_a_bead_layout_are_refused() {
    assert!(cfg(2).check().is_ok());
    let mut c = cfg(2);
    c.min_feature_size = 0.8;
    assert!(c.check().unwrap_err().contains("min_feature_size"));
    let mut c = cfg(2);
    c.transition_angle = 0.5f64.to_radians();
    assert!(c.check().unwrap_err().contains("wall_transition_angle"));
}

/// Through the planner: a 0.5 mm fin beside a block. Classic walls and
/// arachne both print it as one outer-wall bead along its length.
#[test]
fn a_thin_fin_is_printed() {
    use makepad_fab_slicer::sdf::SdfSource;
    let field = |x: f64, y: f64, z: f64| {
        let block = ((x - 100.0).abs() - 3.0).max((y - 100.0).abs() - 3.0);
        let fin = ((x - 108.0).abs() - 5.0).max((y - 100.0).abs() - 0.25);
        block.min(fin).max(-z).max(z - 0.6)
    };
    let job = |generator: &str| {
        let mut cfg = makepad_fab_settings::config::Config::new();
        for key in ["sparse_infill_pattern", "top_surface_pattern", "bottom_surface_pattern", "internal_solid_infill_pattern"] {
            cfg.set_str(key, "zig-zag");
        }
        for (key, value) in [("nozzle_diameter", "0.4"), ("layer_height", "0.2"), ("initial_layer_print_height", "0.2"), ("wall_loops", "2"), ("sparse_infill_density", "15%"), ("wall_generator", generator)] {
            cfg.set_str(key, value);
        }
        cfg.set_vec("filament_map", vec!["1".into()]);
        Job::new(cfg)
    };
    let o = crate::ObjectInput { overrides: Vec::new(), name: "fin".into(), source: Box::new(SdfSource::new(std::sync::Arc::new(field), ([96.0, 96.0, 0.0], [114.0, 104.0, 0.6]))), filament: 0 };
    let fin = |generator: &str| -> Vec<(Role, bool, f64)> {
        let plan = crate::plan_one(crate::par::Ctl::default(), 0, &o, &job(generator)).unwrap();
        plan.layers[1].2.islands.iter().flat_map(|i| &i.walls).filter(|p| p.pts.iter().any(|q| q.x_mm() > 105.0)).map(|p| (p.role, p.closed, p.length_mm())).collect()
    };
    for generator in ["classic", "arachne"] {
        let walls = fin(generator);
        let bead: f64 = walls.iter().filter(|p| p.0 == Role::OuterWall && !p.1).map(|p| p.2).sum();
        assert!(bead > 7.0, "{generator}: the fin's bead: {walls:?}");
    }
}

/// Points a triangulation meets: two parallel rows of samples, whose quads are cocircular up to
/// rounding, at the far corner of the widest box allowed.
fn strip_points(far: i64) -> Vec<(i64, i64)> {
    let mut pts = Vec::new();
    for k in 0..400i64 {
        pts.push((far + k * 10_007, far));
        pts.push((far + k * 10_007, far + 100_003 + k % 3));
    }
    pts.push((far - (1 << 29), far - (1 << 29)));
    pts.push((far + (1 << 29) - 1, far - (1 << 29) + 11));
    pts
}

#[test]
fn every_triangle_is_proper_and_no_point_lies_inside_a_neighbours_circle() {
    let pts = strip_points(1 << 29);
    let tri = delaunay::triangulate(&pts).unwrap();
    assert!(tri.tris.len() / 3 >= pts.len() - 2);
    let exact = |e: usize, far: u32| {
        let at = |v: u32| pts[v as usize];
        delaunay::in_circle_exact(at(tri.tris[e]), at(tri.tris[delaunay::next(e)]), at(tri.tris[delaunay::prev(e)]), at(far))
    };
    for e in 0..tri.tris.len() {
        let (a, b, c) = (pts[tri.tris[e] as usize], pts[tri.tris[delaunay::next(e)] as usize], pts[tri.tris[delaunay::prev(e)] as usize]);
        assert!(delaunay::orient(a, b, c) > 0, "triangle {} has no area", e / 3);
        let t = tri.halfedges[e];
        if t != delaunay::NONE {
            assert!(!exact(e, tri.tris[delaunay::prev(t as usize)]), "edge {e} should have flipped");
        }
    }
}

#[test]
fn the_circle_test_agrees_with_exact_integers_next_to_cocircular() {
    // A square at every size up to the widest box, with its fourth corner moved by nothing and by
    // one unit each way: the determinant is zero or as near as whole units let it be.
    for size in (8..30).map(|b| (1i64 << b) - 1) {
        for (dx, dy) in [(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (-1, -1)] {
            let (a, b, c, d) = ((0, 0), (size, 0), (size, size), (dx, size + dy));
            assert_eq!(delaunay::inside_circle(a, b, c, d), delaunay::in_circle_exact(a, b, c, d), "size {size} moved {dx},{dy}");
        }
    }
}
