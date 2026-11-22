//! Classic gap fill: thin bars and slots get lines, and no place is left hollow.
use crate::*;

fn params(loops: &str) -> Params {
    let mut cfg = makepad_fab_settings::config::Config::new();
    for key in ["sparse_infill_pattern", "top_surface_pattern", "bottom_surface_pattern", "internal_solid_infill_pattern"] {
        cfg.set_str(key, "zig-zag");
    }
    for (key, value) in [("nozzle_diameter", "0.4"), ("layer_height", "0.2"), ("initial_layer_print_height", "0.2"), ("wall_loops", loops), ("wall_generator", "classic")] {
        cfg.set_str(key, value);
    }
    Params::from_job(&Job::new(cfg), 0)
}

fn bar(length: f64, width: f64) -> ExPolygon {
    let ring = [(0.0, 0.0), (length, 0.0), (length, width), (0.0, width)];
    ExPolygon::new(Polygon::new(ring.iter().map(|&(x, y)| Pt::from_mm(x, y)).collect()))
}

/// Every bead of the walls as (a, b, half width in mm).
fn beads(w: &IslandWalls, half_wall: f64) -> Vec<([f64; 2], [f64; 2], f64)> {
    let mm = |p: Pt| [p.x_mm(), p.y_mm()];
    let mut out = Vec::new();
    if let WallShape::Rings(rings) = &w.shape {
        for ring in rings.iter().flatten() {
            for (i, &a) in ring.pts.iter().enumerate() {
                out.push((mm(a), mm(ring.pts[(i + 1) % ring.pts.len()]), half_wall));
            }
        }
    }
    for l in w.gaps.iter().chain(&w.thin) {
        let n = l.pts.len();
        for i in 0..if l.closed { n } else { n - 1 } {
            let j = (i + 1) % n;
            out.push((mm(l.pts[i]), mm(l.pts[j]), 0.5 * flow::width_for_spacing(l.widths[i].max(l.widths[j]), 0.2)));
        }
    }
    out
}

/// The share of a width x length bar, away from its ends, that some bead covers.
fn covered(w: &IslandWalls, length: f64, width: f64, half_wall: f64) -> f64 {
    let beads = beads(w, half_wall);
    let (mut hit, mut all) = (0, 0);
    let mut y = 0.0125;
    while y < width {
        let mut x = length / 2.0 - 1.0;
        while x < length / 2.0 + 1.0 {
            all += 1;
            hit += beads.iter().any(|&(a, b, half)| {
                let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                let t = (((x - a[0]) * dx + (y - a[1]) * dy) / (dx * dx + dy * dy).max(1e-12)).clamp(0.0, 1.0);
                (x - a[0] - t * dx).hypot(y - a[1] - t * dy) <= half
            }) as usize;
            x += 0.05;
        }
        y += 0.025;
    }
    hit as f64 / all as f64
}

/// A bar too narrow for the second wall has no slot left down its middle, at any width from one
/// wall to two, on the first layer's wider beads too.
#[test]
fn no_slot_down_a_thin_bar() {
    let p = params("2");
    for &(first, half_wall) in &[(false, 0.21), (true, 0.25)] {
        let mut width = 0.4;
        while width < 1.85 {
            let island = bar(20.0, width);
            let w = island_walls(&p, std::slice::from_ref(&island), 2, first, 0.2);
            let share = covered(&w, 20.0, width, half_wall);
            assert!(share > 0.93, "first {first}: {width:.2} mm bar {:.1}% covered, {} gap lines, {} thin", share * 100.0, w.gaps.len(), w.thin.len());
            width += 0.05;
        }
    }
}

/// A gap's bead is as wide as the gap: nothing runs into the walls on either side.
#[test]
fn a_bead_between_walls_is_as_wide_as_the_gap() {
    let p = params("2");
    let w = island_walls(&p, std::slice::from_ref(&bar(20.0, 1.2)), 2, false, 0.2);
    assert_eq!(w.gaps.len(), 1, "{:?}", w.gaps);
    assert!(w.fill.is_empty(), "a bar too narrow for the second wall has no fill");
    let widest = w.gaps[0].widths.iter().copied().fold(0.0, f64::max);
    assert!((0.3..0.45).contains(&widest), "{widest}");
}

/// What is too thin for a loop prints as its outer wall: one bead as wide as it is. Narrower than
/// the narrowest bead the nozzle lays, nothing prints: widened, the bead would print past the
/// model.
#[test]
fn thin_islands_print_or_are_too_thin() {
    let p = params("2");
    let least = flow::spacing(p.gap_fill.min_bead_width, 0.2);
    for &(width, lines) in &[(0.75, 1), (0.5, 1), (0.36, 1), (0.3, 0), (0.05, 0)] {
        let w = island_walls(&p, std::slice::from_ref(&bar(10.0, width)), 2, false, 0.2);
        assert!(w.gaps.is_empty(), "{width} mm: no gap fill on a part's outside, {:?}", w.gaps);
        assert_eq!(w.thin.len(), lines, "{width} mm: {:?}", w.thin);
        for l in &w.thin {
            assert!(l.widths.iter().all(|&b| b >= least && b <= width + 0.01), "{width} mm: {:?}", l.widths);
        }
    }
}

/// Thick walls leave nothing: their beads touch and the fill takes the middle.
#[test]
fn thick_walls_have_no_gap_fill() {
    let p = params("2");
    for width in [3.0, 5.0] {
        let w = island_walls(&p, std::slice::from_ref(&bar(20.0, width)), 2, false, 0.2);
        assert!(w.gaps.is_empty(), "{width} mm: {:?}", w.gaps);
        assert!(!w.fill.is_empty());
    }
}

/// Gap lines leave the planner with their role, height, area and a capped speed.
#[test]
fn gap_paths_are_gap_fill() {
    let p = params("2");
    let w = island_walls(&p, std::slice::from_ref(&bar(20.0, 1.2)), 2, false, 0.2);
    let paths = walls::gap_paths(&w.gaps, 0.2, Pt::from_mm(21.0, 0.0));
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].role, Role::GapFill);
    assert!(paths[0].pts[0].x_mm() > 10.0, "starts at the end nearest the head");
}

/// Elephant-foot compensation shrinks what it can and keeps what it would wipe out: a fin from
/// the second layer up with nothing under it on the plate cannot print.
#[test]
fn the_foot_never_wipes_out_a_thin_fin() {
    let mut p = params("2");
    p.elephant_foot = 0.2;
    let fin = [bar(10.0, 0.35)];
    let base = foot_compensated(&p, &fin);
    assert_eq!(base.len(), 1);
    assert!((base[0].area_mm2() - fin[0].area_mm2()).abs() < 1e-6, "a fin thinner than the two shrinks stays as it is");
    let w = island_walls(&p, &base, 2, true, 0.2);
    assert_eq!(w.thin.len(), 1, "{:?}", w.thin);
    let block = [bar(10.0, 5.0)];
    let base = foot_compensated(&p, &block);
    assert_eq!(base.len(), 1);
    assert!((base[0].area_mm2() - 9.6 * 4.6).abs() < 0.01, "a block's outline just shrinks");
}

/// A place that tapers gets one line down its length, as wide as it is where it runs: a wedge
/// steeper than the walls' own transition angle is no reason to break it up. A square's corners
/// stay no place for one.
#[test]
fn a_taper_gets_one_line_down_it() {
    let p = params("2");
    let h = 0.2;
    let ring = |pts: &[(f64, f64)]| ExPolygon::new(Polygon::new(pts.iter().map(|&(x, y)| Pt::from_mm(x, y)).collect()));
    let taper = ring(&[(0.0, -0.33), (1.5, -0.18), (1.5, 0.18), (0.0, 0.33)]);
    let lines = gap_lines(&p, std::slice::from_ref(&taper), p.w_solid, h);
    assert_eq!(lines.len(), 1, "one line, {} of them", lines.len());
    let length: f64 = lines[0].pts.windows(2).map(|s| s[0].dist(s[1])).sum::<f64>() / SCALE;
    assert!(!lines[0].closed && length > 1.0, "down its length, {length:.2} mm");
    let (lo, hi) = lines[0].widths.iter().fold((f64::MAX, 0f64), |m, &w| (m.0.min(w), m.1.max(w)));
    assert!(hi > lo + 0.05 && hi < 0.62, "as wide as it is where it runs, {lo:.3}..{hi:.3}");
    let lines = gap_lines(&p, std::slice::from_ref(&bar(0.3, 0.3)), p.w_solid, h);
    assert!(lines.iter().all(|l| l.pts.windows(2).map(|s| s[0].dist(s[1])).sum::<f64>() / SCALE < 0.1), "no lines into a square's corners");
}

/// A plain square's top lines leave only the teeth between their ends and its corners: no gap
/// fill, as in the reference's cubes.
#[test]
fn a_plain_surface_has_no_gap_fill() {
    let p = params("2");
    let (center, aux) = (Pt::from_mm(0.0, 0.0), infill::Aux::None);
    let square = [bar(20.0, 20.0)];
    let own = infill::SparseFill { density: p.density, pattern: p.sparse_pattern };
    let lines = infill(&p, Role::TopSurface, &square, &square, 5, 0.2, std::f64::consts::FRAC_PI_4, center, false, 1.2, None, false, 0, &aux, own, &[]);
    assert!(!lines.is_empty());
    let gaps = surface_gaps(&p, &square, &lines, 0.2, false, center);
    assert!(gaps.is_empty(), "{} gap lines on a plain square", gaps.len());
}

/// A place the surface's lines did not reach gets gap fill; once they cover it, none.
#[test]
fn a_surface_fills_the_place_its_lines_leave() {
    let p = params("2");
    let line = |y: f64| {
        let mut path = gap_path(y);
        path.role = Role::TopSurface;
        path
    };
    let area = [bar(20.0, 3.0)];
    let center = Pt::from_mm(0.0, 0.0);
    let few: Vec<ExtrusionPath> = (0..4).map(|k| line(0.2 + 0.4 * k as f64)).collect();
    let gaps = surface_gaps(&p, &area, &few, 0.2, false, center);
    assert!(!gaps.is_empty(), "the far side is left hollow");
    assert!(gaps.iter().all(|g| g.role == Role::GapFill && g.height == 0.2 && g.area > 0.0));
    assert!(gaps.iter().flat_map(|g| g.pts.iter()).all(|q| q.y_mm() > 1.4 && q.y_mm() < 3.0), "gap fill off the free place");
    let all: Vec<ExtrusionPath> = (0..8).map(|k| line(0.2 + 0.4 * k as f64)).collect();
    assert!(surface_gaps(&p, &area, &all, 0.2, false, center).is_empty());
}

fn gap_path(y: f64) -> ExtrusionPath {
    ExtrusionPath { role: Role::GapFill, pts: vec![Pt::from_mm(0.0, y), Pt::from_mm(20.0, y)], closed: false, loop_pts: Vec::new(), width: 0.42, height: 0.2, area: 0.08, speed: 100.0, dry: Vec::new(), z: Vec::new(), flow: Vec::new(), mode: 0, core: false, overhang: 0.0 }
}

/// A line through a place between beads lays what the place holds: its spacing is the place's
/// width, so its volume is the place's area times the height.
#[test]
fn a_place_line_holds_what_the_place_holds() {
    let p = params("2");
    let h = 0.2;
    let mut t = 0.26;
    while t < 0.75 {
        let lines = place_lines(&p, std::slice::from_ref(&bar(10.0, t)), p.w_solid, h);
        assert!(!lines.is_empty(), "a {t:.2} mm place gets lines");
        // Short of the place's area by no more than the ends a line stops short of.
        let laid: f64 = lines.iter().map(line_area).sum();
        assert!(laid > 0.965 * t * (10.0 - t) && laid < 1.005 * 10.0 * t, "a {t:.2} mm place: lines laying {laid:.3} of {:.3} mm2", 10.0 * t);
        t += 0.01;
    }
}

/// What a concentric fill's rounds leave: every neck narrower than a round gets one line, from
/// the walls' gap fill or from the piece's own, with no width between the two rules. A strip that
/// bends gets its whole length, and a plain block's corners get nothing.
#[test]
fn no_width_falls_between_gap_fill_and_the_rounds() {
    let mut p = params("2");
    p.infill_overlap = 0.15;
    let h = 0.2;
    let role = Role::InternalSolidInfill;
    let block = bar(6.0, 6.0);
    let rounds = concentric_solid(&p, role, &block, h, false);
    assert!(left_beads(&p, role, &block, &rounds, &[], h, false).is_empty(), "a block's corners are no place for a line");

    let ring = |pts: &[(f64, f64)]| ExPolygon::new(Polygon::new(pts.iter().map(|&(x, y)| Pt::from_mm(x, y)).collect()));
    let depth = island_walls(&p, std::slice::from_ref(&bar(10.0, 10.0)), 2, false, h).fill[0].bbox().min.x_mm();
    let spacing = flow::spacing(p.w_solid, h);
    let two_reach = 2.0 * fill_reach(&p, h, false);
    let mut neck = 0.16;
    while neck < spacing {
        let (lo, hi) = (2.0 - neck / 2.0, 2.0 + neck / 2.0);
        let fill = ring(&[(0.0, 0.0), (4.0, 0.0), (4.0, lo), (10.0, lo), (10.0, 0.0), (14.0, 0.0), (14.0, 4.0), (10.0, 4.0), (10.0, hi), (4.0, hi), (4.0, 4.0), (0.0, 4.0)]);
        let island = offset_ex(std::slice::from_ref(&fill), depth * SCALE, DEFAULT_MITER);
        let w = island_walls(&p, &island, 2, false, h);
        assert_eq!(w.fill.len(), 1, "{neck:.2} mm neck: the walls leave one fill");
        let gaps = walls::gap_paths(&w.gaps, h, Pt::new(0, 0));
        let rounds = concentric_solid(&p, role, &w.fill[0], h, false);
        let left = left_beads(&p, role, &w.fill[0], &rounds, &gaps, h, false);
        assert_eq!(left.is_empty(), neck < two_reach - 0.01, "{neck:.2} mm neck: the piece fills it from {two_reach:.3} mm up");
        let crossings: usize = gaps.iter().chain(&rounds).chain(&left).map(|b| {
            b.pts.windows(2).filter(|s| (s[0].x_mm() - 7.0) * (s[1].x_mm() - 7.0) <= 0.0 && (s[0].y_mm() - 2.0).abs() < 1.0).count()
        }).sum();
        assert_eq!(crossings, 1, "{neck:.2} mm neck: one line down it");
        for b in &left {
            assert!(b.role == role && flow::spacing(b.width, h) <= neck + 0.02, "{neck:.2} mm neck: a line in the piece's role no wider than the place, {:?} {:.3}", b.role, b.width);
            assert!(b.pts.iter().all(|q| q.x_mm() > 3.0 && q.x_mm() < 11.0), "{neck:.2} mm neck: lines stay in the neck");
        }
        neck += 0.03;
    }

    // From a spacing to two the neck holds no ring: the piece's own lines share its width. From two
    // up, the ring's two sides run down it, a spacing or more apart.
    let across = |beads: &[ExtrusionPath]| -> Vec<(f64, f64)> {
        beads.iter().flat_map(|b| b.pts.windows(2).filter(|s| (s[0].x_mm() - 5.3) * (s[1].x_mm() - 5.3) <= 0.0 && (s[0].y_mm() - 2.0).abs() < 1.0).map(|s| ((s[0].y_mm() + s[1].y_mm()) / 2.0, b.width)).collect::<Vec<_>>()).collect()
    };
    while neck < 2.6 * spacing {
        let (lo, hi) = (2.0 - neck / 2.0, 2.0 + neck / 2.0);
        let fill = ring(&[(0.0, 0.0), (4.0, 0.0), (4.0, lo), (10.0, lo), (10.0, 0.0), (14.0, 0.0), (14.0, 4.0), (10.0, 4.0), (10.0, hi), (4.0, hi), (4.0, 4.0), (0.0, 4.0)]);
        let rounds = concentric_solid(&p, role, &fill, h, false);
        let left = left_beads(&p, role, &fill, &rounds, &[], h, false);
        if neck < 2.0 * spacing - 0.01 {
            assert!(across(&rounds).is_empty(), "{neck:.2} mm neck: no ring down it");
            let lines = across(&left);
            let share: f64 = lines.iter().map(|l| flow::cross_section(l.1, h)).sum();
            assert!(!lines.is_empty() && (share - neck * h).abs() < 0.02 * h, "{neck:.2} mm neck: lines holding what it holds, {share:.4} mm2, {lines:?}");
        } else if neck > 2.0 * spacing + 0.01 {
            let sides = across(&rounds);
            assert!(sides.len() == 2 && (sides[0].0 - sides[1].0).abs() > spacing - 0.001, "{neck:.2} mm neck: the ring's sides a spacing apart, {sides:?}");
        }
        neck += 0.03;
    }

    // A bent strip's extent says nothing of its thickness: the beads lay it once.
    for t in [1.05, 1.3, 1.6, 1.95].map(|f| f * spacing) {
        let strip = ring(&[(0.0, 0.0), (20.0, 0.0), (20.0, t), (t, t), (t, 10.0), (0.0, 10.0)]);
        let rounds = concentric_solid(&p, role, &strip, h, false);
        let beads: Vec<_> = left_beads(&p, role, &strip, &rounds, &[], h, false).into_iter().chain(rounds).collect();
        let laid: f64 = beads.iter().map(|b| flow::spacing(b.width, h) * b.pts.windows(2).enumerate().map(|(i, s)| s[0].dist(s[1]) / SCALE * b.flow.get(i).copied().unwrap_or(1.0)).sum::<f64>()).sum();
        let place = strip.area_mm2();
        assert!(laid > 0.9 * place && laid < 1.05 * place, "a {t:.3} mm strip that bends is laid once: {laid:.2} mm2 of beads on {place:.2}");
    }

    // A block keeps its rounds, and the last pair of a strip closes exactly.
    let first = concentric_beads(&p, &block, h, false);
    assert!(first.len() >= 2);
    let ring_box = |b: &given::Bead| match b {
        given::Bead::Loop { pts, .. } => BBox::of(pts),
        given::Bead::Line { pts, .. } => BBox::of(pts),
    };
    assert!((ring_box(&first[0]).min.x_mm() - spacing / 2.0).abs() < 1e-3 && (ring_box(&first[0]).max.x_mm() - (6.0 - spacing / 2.0)).abs() < 1e-3, "a block's first ring");
    let mut t = 1.55 * spacing;
    while t < 2.45 * spacing {
        let beads = concentric_beads(&p, &bar(10.0, t), h, false);
        assert_eq!(beads.len(), 1, "a {t:.3} mm strip's last pair is one ring");
        let b = ring_box(&beads[0]);
        assert!((b.min.y_mm() - t / 4.0).abs() < 1e-3 && (b.max.y_mm() - 3.0 * t / 4.0).abs() < 1e-3, "a {t:.3} mm strip's last pair shares its width");
        t += 0.03;
    }

    let t = spacing;
    let strip = ring(&[(0.0, 0.0), (20.0, 0.0), (20.0, t), (t, t), (t, 10.0), (0.0, 10.0)]);
    let beads = floating_shell(&p, std::slice::from_ref(&strip), &[], h, false);
    let length: f64 = beads.iter().map(|b| b.pts.windows(2).map(|s| s[0].dist(s[1])).sum::<f64>()).sum::<f64>() / SCALE;
    assert!(length > 0.9 * (30.0 - t) && length < 30.0, "a bent strip's bead runs its length once, {length:.2} mm");
    assert!(beads.iter().all(|b| b.role == Role::FloatingVerticalShell && b.width <= t * std::f64::consts::SQRT_2 + 0.02), "a bent strip keeps its band's role and is no wider than its corner");
}
