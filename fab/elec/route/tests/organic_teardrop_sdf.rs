use fab_elec_route::organic::{
    geom::Vec2,
    model::{Pad, PadShape},
    padgeom,
    teardrop_sdf::{build, Input, Result, Scale},
};

#[test]
fn teardrop_sdf_geometry() {
    fn orient(a: Vec2, b: Vec2, c: Vec2) -> f64 {
        (b.x as f64 - a.x as f64) * (c.y as f64 - a.y as f64)
            - (b.y as f64 - a.y as f64) * (c.x as f64 - a.x as f64)
    }
    fn on_segment(a: Vec2, b: Vec2, p: Vec2) -> bool {
        let eps = 1e-12;
        orient(a, b, p).abs() <= eps
            && p.x as f64 >= (a.x as f64).min(b.x as f64) - eps
            && p.x as f64 <= (a.x as f64).max(b.x as f64) + eps
            && p.y as f64 >= (a.y as f64).min(b.y as f64) - eps
            && p.y as f64 <= (a.y as f64).max(b.y as f64) + eps
    }
    fn check_ring(poly: &[Vec2], name: &str) {
        assert!(poly.len() >= 3, "{name}: a closed ring has at least three vertices");
        // TeardropSdf.cpp:1247-1252 (multi:1135-1142) removes a repeated first vertex.
        // Closure is implicit: include the last-to-first edge in every check below.
        assert!(
            poly[0].distance_squared_to(*poly.last().unwrap()) >= 1e-7_f32,
            "{name}: implicit closure has no duplicate end vertex"
        );
        let n = poly.len();
        let mut area2 = 0.0;
        for i in 0..n {
            let a = poly[i];
            let b = poly[(i + 1) % n];
            assert!(a.x.is_finite() && a.y.is_finite(), "{name}: vertex {i} is finite");
            assert!(a.distance_squared_to(b) > 0.0, "{name}: edge {i}, including closure, is nonzero");
            area2 += a.x as f64 * b.y as f64 - a.y as f64 * b.x as f64;
            for j in i + 1..n {
                if j == i + 1 || (i == 0 && j == n - 1) { continue; }
                let c = poly[j];
                let d = poly[(j + 1) % n];
                let ab_c = orient(a, b, c);
                let ab_d = orient(a, b, d);
                let cd_a = orient(c, d, a);
                let cd_b = orient(c, d, b);
                let crossing = ((ab_c > 0.0 && ab_d < 0.0) || (ab_c < 0.0 && ab_d > 0.0))
                    && ((cd_a > 0.0 && cd_b < 0.0) || (cd_a < 0.0 && cd_b > 0.0));
                let touching = on_segment(a, b, c) || on_segment(a, b, d)
                    || on_segment(c, d, a) || on_segment(c, d, b);
                assert!(!crossing && !touching, "{name}: non-adjacent edges {i} and {j} do not intersect");
            }
        }
        assert!(area2.abs() > 1e-6, "{name}: the closed ring encloses nonzero area");
    }

    let radius = 0.3;
    let track_width = 0.25_f32;
    let half_track = track_width as f64 / 2.0;
    let length = 0.5;
    let path = [Vec2::new(0.0, 0.0), Vec2::new(1.2, 0.0)];
    let widths = [track_width; 2];
    let input = Input {
        via_radius: radius,
        path: Some(&path),
        width: Some(&widths),
        s_x: radius,
        s_root: 0.0,
        length,
        width2: 2.0 * radius,
        ..Input::default()
    };
    let full = Scale::default();
    let mut via = Result::default();
    assert!(build(&input, &full, &full, &mut via), "round via: build succeeds");
    check_ring(&via.poly, "round via");
    let tol = 2e-5;
    let tip_station = input.s_x + input.length;
    assert!(via.flare > half_track, "round via: a nonzero flare extends beyond the track");
    assert!(via.flare <= input.width2 / 2.0 + tol, "round via: flare is bounded by the half width");
    // TeardropSdf.cpp:385-394 lets a full flank take the next outline sample, so width2
    // is not a universal hard cap. Here it is the via diameter (and below the pad's
    // transverse extent), which geometrically bounds every possible landing sample.
    for (i, &p) in via.poly.iter().enumerate() {
        let x = p.x as f64;
        let y = p.y as f64;
        let in_via = x * x + y * y <= (radius + tol) * (radius + tol);
        let in_region = x >= -tol && x <= tip_station + tol && y.abs() <= input.width2 / 2.0 + tol;
        assert!(in_via || in_region, "round via: vertex {i} lies in the disc or teardrop bounding region");
        assert!(x <= tip_station + tol, "round via: vertex {i} does not pass the tip station");
        assert!(y.abs() <= input.width2 / 2.0 + tol, "round via: vertex {i} stays within the half width");
        assert!(
            via.poly.iter().any(|q| (q.x as f64 - x).abs() <= tol && (q.y as f64 + y).abs() <= tol),
            "round via: vertex {i} has a reflection across the track axis"
        );
    }
    for side in [-1.0, 1.0] {
        assert!(
            via.poly.iter().any(|p| (p.x as f64 - tip_station).abs() <= tol
                && (p.y as f64 - side * half_track).abs() <= tol),
            "round via: side {side} reaches the geometrically computed tip on the track edge"
        );
    }

    let mut repeated = Result::default();
    assert!(build(&input, &full, &full, &mut repeated), "determinism: repeated build succeeds");
    assert_eq!(via.poly.len(), repeated.poly.len(), "determinism: identical polygon sizes");
    for (i, (a, b)) in via.poly.iter().zip(&repeated.poly).enumerate() {
        assert_eq!(a.x.to_bits(), b.x.to_bits(), "determinism: vertex {i} x bits");
        assert_eq!(a.y.to_bits(), b.y.to_bits(), "determinism: vertex {i} y bits");
    }
    assert_eq!(via.flare.to_bits(), repeated.flare.to_bits(), "determinism: identical flare bits");

    let pad = Pad {
        shape: PadShape::Rect,
        size: Vec2::new(0.6, 0.8),
        local_size: Vec2::new(0.6, 0.8),
        is_smd: true,
        drill_diameter: 0.0,
        ..Pad::default()
    };
    let frame = padgeom::frame(&pad);
    let smd_input = Input {
        pad: Some(&frame), path: Some(&path), width: Some(&widths), s_x: frame.hx,
        length, width2: 2.0 * frame.hy, ..Input::default()
    };
    let mut smd = Result::default();
    assert!(build(&smd_input, &full, &full, &mut smd), "rectangular SMD: build succeeds");
    check_ring(&smd.poly, "rectangular SMD");
    assert!(smd.flare > half_track, "rectangular SMD: a nonzero flare extends beyond the track");
    assert!(smd.flare <= smd_input.width2 / 2.0 + tol, "rectangular SMD: flare is bounded by the half width");

    let flat = Scale { height: 0.0, length: 1.0 };
    let mut one_side = Result::default();
    assert!(build(&input, &flat, &full, &mut one_side), "flat left: build succeeds");
    check_ring(&one_side.poly, "flat left");
    // TeardropSdf.cpp:1219-1226 samples T.at without a normal offset: height zero follows
    // the centreline, not the track edge. Check that actual reference behaviour explicitly.
    let centre_tip = one_side.poly.iter().position(|p| (p.x as f64 - tip_station).abs() <= tol && (p.y as f64).abs() <= tol);
    assert!(centre_tip.is_some(), "flat left: the centreline reaches the tip station");
    let centre_tip = centre_tip.unwrap();
    assert!(one_side.poly.len() - centre_tip >= 2, "flat left: the centreline has more than its tip");
    for (i, p) in one_side.poly.iter().enumerate() {
        assert!(p.y as f64 <= tol, "flat left: vertex {i} does not flare on the disabled side");
        if i >= centre_tip || i == 0 {
            assert!((p.y as f64).abs() <= tol, "flat left: disabled flank vertex {i} lies on the centreline");
        }
    }
    assert!(one_side.poly.iter().any(|p| (p.y as f64) < -half_track - tol), "flat left: the right flank still flares");

    // A circular clearance obstacle outside the via, above the outgoing track. Its signed
    // distance is 1-Lipschitz, as required by the reference's foreign-field cache.
    let obstacle = Vec2::new(0.42, 0.28);
    let obstacle_radius = 0.13;
    let foreign = |v: Vec2| {
        let dx = v.x as f64 - obstacle.x as f64;
        let dy = v.y as f64 - obstacle.y as f64;
        (dx * dx + dy * dy).sqrt() - obstacle_radius
    };
    assert!(foreign(obstacle) < 0.0, "foreign field: obstacle interior is forbidden");
    assert!(via.poly.iter().any(|&v| foreign(v) < -tol), "foreign field: the obstacle cuts the unconstrained flank");
    let foreign_input = Input { foreign: Some(&foreign), ..input };
    let mut constrained = Result::default();
    assert!(build(&foreign_input, &full, &full, &mut constrained), "foreign field: build succeeds");
    check_ring(&constrained.poly, "foreign field");
    assert!(constrained.poly.iter().any(|p| p.y as f64 > half_track + tol), "foreign field: a non-flat upper flank remains");
    for (i, &v) in constrained.poly.iter().enumerate() {
        assert!(foreign(v) >= -tol, "foreign field: vertex {i} stays outside the forbidden region");
    }
}
