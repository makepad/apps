//! The printability gate on the CPU's own walls (they pass), and each check against a
//! deliberately broken candidate (it fails, on one line, naming the check).
use crate::printcheck::{self, Check, Failure, Island, Join, Level, Tol};
use crate::*;
use makepad_fab_slicer::offset::offset_polygons;
use makepad_fab_slicer::source::BeadLine;

fn job(generator: &str) -> Job {
    let mut cfg = makepad_fab_settings::config::Config::new();
    for key in ["sparse_infill_pattern", "top_surface_pattern", "bottom_surface_pattern", "internal_solid_infill_pattern"] {
        cfg.set_str(key, "zig-zag");
    }
    for (key, value) in [("nozzle_diameter", "0.4"), ("layer_height", "0.2"), ("initial_layer_print_height", "0.2"), ("wall_loops", "3"), ("wall_generator", generator)] {
        cfg.set_str(key, value);
    }
    Job::new(cfg)
}

fn ring(pts: &[(f64, f64)]) -> Polygon {
    Polygon::new(pts.iter().map(|&(x, y)| Pt::from_mm(x, y)).collect())
}

fn circle(cx: f64, cy: f64, r: f64, n: usize, ccw: bool) -> Polygon {
    let mut p: Vec<Pt> = (0..n).map(|i| {
        let a = i as f64 / n as f64 * std::f64::consts::TAU;
        Pt::from_mm(cx + r * a.cos(), cy + r * a.sin())
    }).collect();
    if !ccw {
        p.reverse();
    }
    Polygon::new(p)
}

/// A 20 x 14 block with a round hole, an L, a dumbbell whose neck cuts the inner walls, and a
/// strip too narrow for more than one wall.
fn shapes() -> Vec<(&'static str, ExPolygon)> {
    let mut block = ExPolygon::new(ring(&[(90.0, 93.0), (110.0, 93.0), (110.0, 107.0), (90.0, 107.0)]));
    block.holes.push(circle(97.0, 100.0, 2.5, 64, false));
    let l = ExPolygon::new(ring(&[(0.0, 0.0), (12.0, 0.0), (12.0, 4.0), (4.0, 4.0), (4.0, 10.0), (0.0, 10.0)]));
    let neck = ExPolygon::new(ring(&[(0.0, 0.0), (6.0, 0.0), (6.0, 2.4), (9.0, 2.4), (9.0, 0.0), (15.0, 0.0), (15.0, 6.0), (9.0, 6.0), (9.0, 3.6), (6.0, 3.6), (6.0, 6.0), (0.0, 6.0)]));
    let strip = ExPolygon::new(ring(&[(0.0, 0.0), (10.0, 0.0), (10.0, 0.9), (0.0, 0.9)]));
    vec![("block", block), ("l", l), ("neck", neck), ("strip", strip)]
}

/// The job's wall levels and join, and an island's walls as the planner makes them.
fn levels(p: &Params) -> (Vec<Level>, Join) {
    let h = p.layer_height;
    match &p.arachne {
        Some(a) => (a.config(p.wall_loops, p.w_outer, p.w_inner, h, p.precise_outer).nominal_wall_levels().2.map(|(inset, spacing, _)| Level { inset, spacing }).collect(), Join::Round),
        None => (walls::levels(p.wall_loops, p.w_outer, p.w_inner, h, p.precise_outer).map(|(inset, spacing, _)| Level { inset, spacing }).collect(), Join::Miter(3.0)),
    }
}

fn planned(p: &Params, outline: &ExPolygon) -> IslandWalls {
    island_walls(p, std::slice::from_ref(outline), p.wall_loops, false, p.layer_height)
}

fn check(p: &Params, outline: &ExPolygon, candidate: &IslandWalls, reference: &IslandWalls) -> Vec<Failure> {
    let (levels, join) = levels(p);
    printcheck::walls(7, &levels, join, &[Island { outline, walls: candidate }], &[Island { outline, walls: reference }], &Tol::default())
}

#[test]
fn the_cpus_own_walls_pass() {
    for generator in ["classic", "arachne"] {
        let p = Params::from_job(&job(generator), 0);
        for (name, outline) in shapes() {
            let w = planned(&p, &outline);
            let f = check(&p, &outline, &w, &w);
            assert!(f.is_empty(), "{generator} {name}: the CPU's walls fail its own gate:\n{}", printcheck::lines(&f));
            // Classic walls away from necks hold the f64 oracle and their spacing with nothing
            // excused. Where an opening cuts a wall (the neck, the strip) its caps leave gaps for
            // gap fill, and Arachne's beads are simplified at 0.0125 mm: those are held to the
            // reference by `walls`.
            let (levels, join) = levels(&p);
            let own = printcheck::own_walls(7, &levels, join, &[Island { outline: &outline, walls: &w }], &Tol::default());
            if generator == "classic" && (name == "block" || name == "l") {
                assert!(own.is_empty(), "classic {name}: the CPU's walls against the oracle alone:\n{}", printcheck::lines(&own));
            } else {
                eprintln!("{generator} {name}: {} deviations of its own", own.len());
            }
        }
    }
}

/// The one failure line of `check` among `f`, which must name `layer 7 island 0 [check]` and say `says`.
fn caught(f: &[Failure], check: Check, says: &str, what: &str) {
    let hit = f.iter().find(|x| x.check == check && x.detail.contains(says));
    let line = hit.unwrap_or_else(|| panic!("{what}: no [{check}] failure saying {says:?}:\n{}", printcheck::lines(f))).to_string();
    assert!(line.starts_with(&format!("layer 7 island 0 [{check}] ")) && !line.contains('\n'), "{what}: one line naming layer, island and check: {line}");
    eprintln!("{what}: {line}");
}

fn rings_mut(w: &mut IslandWalls) -> &mut Vec<Vec<Polygon>> {
    match &mut w.shape {
        WallShape::Rings(r) => r,
        WallShape::Beads(_) => unreachable!("classic walls"),
    }
}

fn moved(r: &Polygon, by: f64) -> Polygon {
    let mut out = offset_polygons(std::slice::from_ref(r), by * SCALE, makepad_fab_slicer::offset::DEFAULT_MITER);
    assert_eq!(out.len(), 1);
    out.pop().unwrap().contour
}


#[test]
fn each_check_catches_its_break() {
    let p = Params::from_job(&job("classic"), 0);
    let (_, block) = shapes().into_iter().next().unwrap();
    let good = planned(&p, &block);
    let rings = match &good.shape { WallShape::Rings(r) => r.clone(), _ => unreachable!() };
    assert_eq!(rings.len(), 3, "three walls");
    assert_eq!(rings[1].len(), 2, "wall 1: the block's contour and the hole's loop");
    let broken = |change: &dyn Fn(&mut IslandWalls)| {
        let mut w = good.clone();
        change(&mut w);
        check(&p, &block, &w, &good)
    };
    caught(&broken(&|w| { rings_mut(w)[1].remove(1); }), Check::Loops, "missing hole loop", "a dropped loop");
    caught(&broken(&|w| { let extra = rings_mut(w)[2][0].clone(); let mut e = extra; for q in &mut e.pts { q.x += scaled(0.6); } rings_mut(w)[2].push(e); }), Check::Loops, "extra contour loop", "an extra loop");
    caught(&broken(&|w| { rings_mut(w).truncate(2); }), Check::Walls, "2 walls, CPU 3", "a dropped wall");
    caught(&broken(&|w| { rings_mut(w)[0][1].reverse(); }), Check::Nesting, "winds as a contour loop, the CPU's as a hole loop", "a hole wound as a contour");
    caught(&broken(&|w| {
        // The block's outer loop as a bow tie: two corners swapped.
        let r = &mut rings_mut(w)[0][0];
        let n = r.pts.len();
        assert!(n >= 4);
        r.pts.swap(1, 2);
    }), Check::Crossing, "crosses itself", "a self-intersection");
    caught(&broken(&|w| { let r = moved(&rings_mut(w)[1][0], -0.1); rings_mut(w)[1][0] = r; }), Check::Gap, "further from wall 0", "a wall moved 0.1 mm in");
    caught(&broken(&|w| { let r = moved(&rings_mut(w)[1][0], -0.1); rings_mut(w)[1][0] = r; }), Check::Oracle, "deeper than its level", "a wall moved 0.1 mm in, against the oracle");
    caught(&broken(&|w| { let r = moved(&rings_mut(w)[1][0], 0.1); rings_mut(w)[1][0] = r; }), Check::Spacing, "closer than their beads allow", "a wall moved 0.1 mm out");
    caught(&broken(&|w| { let r = moved(&rings_mut(w)[1][0], 0.1); rings_mut(w)[1][0] = r; }), Check::Oracle, "shallower than its level", "a wall moved 0.1 mm out, against the oracle");
    caught(&broken(&|w| { let r = moved(&rings_mut(w)[0][0], 0.015); rings_mut(w)[0][0] = r; }), Check::Oracle, "shallower than its level", "the outer wall 0.015 mm out");
    caught(&broken(&|w| { w.fill = makepad_fab_slicer::offset::offset_ex(&w.fill, -0.05 * SCALE, makepad_fab_slicer::offset::DEFAULT_MITER); }), Check::Fill, "mm², CPU", "the fill 0.05 mm smaller");
    // Small enough to pass: the outer wall 0.005 mm out, the fill 0.002 mm smaller.
    let near = broken(&|w| {
        let r = moved(&rings_mut(w)[0][0], 0.005);
        rings_mut(w)[0][0] = r;
        w.fill = makepad_fab_slicer::offset::offset_ex(&w.fill, -0.002 * SCALE, makepad_fab_slicer::offset::DEFAULT_MITER);
    });
    assert!(near.is_empty(), "within tolerance:\n{}", printcheck::lines(&near));
    // Islands: an extra one, and one missing.
    let (levels, join) = levels(&p);
    let mut far = block.clone();
    far.translate(Pt::from_mm(40.0, 0.0));
    let far_walls = planned(&p, &far);
    let two = [Island { outline: &block, walls: &good }, Island { outline: &far, walls: &far_walls }];
    let f = printcheck::walls(7, &levels, join, &two, &two[..1], &Tol::default());
    assert!(f.iter().any(|x| x.check == Check::Islands && x.to_string().starts_with("layer 7 island 1 [islands] extra island")), "{}", printcheck::lines(&f));
    let f = printcheck::walls(7, &levels, join, &two[..1], &two, &Tol::default());
    assert!(f.iter().any(|x| x.check == Check::Islands && x.to_string().starts_with("layer 7 [islands] missing island")), "{}", printcheck::lines(&f));
    // An outline moved 0.03 mm with walls true to it: the outline check sees it.
    let mut shifted = block.clone();
    shifted.translate(Pt::from_mm(0.03, 0.0));
    let shifted_walls = planned(&p, &shifted);
    let f = printcheck::walls(7, &levels, join, &[Island { outline: &shifted, walls: &shifted_walls }], &two[..1], &Tol::default());
    caught(&f, Check::Outline, "the outline is 0.0300 mm from the CPU's", "the outline moved 0.03 mm");
    // An open bead where the CPU closes every line.
    let pa = Params::from_job(&job("arachne"), 0);
    let beads = planned(&pa, &block);
    let mut open = beads.clone();
    let WallShape::Beads(lines) = &mut open.shape else { unreachable!() };
    let first: &mut BeadLine = lines.iter_mut().find(|l| l.inset == 1 && l.closed).unwrap();
    first.closed = false;
    caught(&check(&pa, &block, &open, &beads), Check::Closed, "open line", "an open bead");
}

/// An island's planned layer: its wall paths in print order, as the planner orders them.
fn layer_of(p: &Params, islands: &[ExPolygon]) -> ObjectLayer {
    let mut out = ObjectLayer::default();
    let mut target = Pt::from_mm(0.0, 0.0);
    for e in islands {
        let w = planned(p, e);
        let (paths, seam) = walls::shape_paths(&w.shape, p.w_outer, p.w_inner, p.layer_height, 0.0, target, walls::WallOrder::InnerOuter, SeamMode::Aligned, 0.4, 0.3);
        target = seam.unwrap_or(target);
        out.islands.push(std::sync::Arc::new(crate::Island { walls: paths.into_iter().map(|p| p.0).collect(), fills: Vec::new() }));
    }
    out
}

#[test]
fn order_and_seams_catch_their_breaks() {
    let p = Params::from_job(&job("classic"), 0);
    let shapes = shapes();
    let islands: Vec<ExPolygon> = shapes.iter().take(2).map(|s| s.1.clone()).collect();
    let good = layer_of(&p, &islands);
    let tol = Tol::default();
    let f = printcheck::order(7, &good, &good, &tol);
    assert!(f.is_empty(), "{}", printcheck::lines(&f));
    let broken = |change: &dyn Fn(&mut ObjectLayer)| {
        let mut l = good.clone();
        change(&mut l);
        printcheck::order(7, &l, &good, &tol)
    };
    let f = broken(&|l| std::sync::Arc::make_mut(&mut l.islands[0]).walls.swap(0, 1));
    assert!(f.iter().any(|x| x.check == Check::Order && x.to_string().starts_with("layer 7 island 0 [order] wall path")), "swapped loops:\n{}", printcheck::lines(&f));
    let f = broken(&|l| l.islands.swap(0, 1));
    assert!(f.iter().any(|x| x.to_string().starts_with("layer 7 [order] islands print as the CPU's [1, 0]")), "swapped islands:\n{}", printcheck::lines(&f));
    let f = broken(&|l| { std::sync::Arc::make_mut(&mut l.islands[1]).walls.pop(); });
    assert!(f.iter().any(|x| x.to_string().starts_with("layer 7 island 1 [order] missing outer wall path")), "a dropped path:\n{}", printcheck::lines(&f));
    let f = broken(&|l| {
        let w = std::sync::Arc::make_mut(&mut l.islands[0]).walls.iter_mut().find(|w| w.role == Role::OuterWall).unwrap();
        let n = w.loop_pts.len() - 1;
        let k = n / 2;
        let mut ring: Vec<Pt> = w.loop_pts[..n].to_vec();
        ring.rotate_left(k);
        ring.push(ring[0]);
        w.loop_pts = ring.clone();
        w.pts = ring;
    });
    assert!(f.iter().any(|x| x.check == Check::Seam && x.to_string().starts_with("layer 7 island 0 [seam] outer wall at")), "a moved seam:\n{}", printcheck::lines(&f));
    for x in &f {
        eprintln!("moved seam: {x}");
    }
}

/// The outer wall's seam moved `k` points round its loop.
fn seam_moved(l: &mut ObjectLayer, k: usize) {
    let w = std::sync::Arc::make_mut(&mut l.islands[0]).walls.iter_mut().find(|w| w.role == Role::OuterWall).unwrap();
    let n = w.loop_pts.len() - 1;
    let mut ring: Vec<Pt> = w.loop_pts[..n].to_vec();
    ring.rotate_left(k % n);
    ring.push(ring[0]);
    w.loop_pts = ring.clone();
    w.pts = ring;
}

/// The excuse rule: a seam or an open line's deviation is excused within the reference's own
/// noise and no further; a closed loop's never is; `held` holds length and fill classes.
#[test]
fn the_noise_excuses_seams_and_fragments_and_nothing_else() {
    let p = Params::from_job(&job("classic"), 0);
    let islands: Vec<ExPolygon> = shapes().iter().take(2).map(|s| s.1.clone()).collect();
    let good = layer_of(&p, &islands);
    let tol = Tol::default();
    let with = |change: &dyn Fn(&mut ObjectLayer)| {
        let mut l = good.clone();
        change(&mut l);
        l
    };
    let seam_of = |l: &ObjectLayer| l.islands[0].walls.iter().find(|w| w.role == Role::OuterWall).unwrap().loop_pts[0];
    let dist = |l: &ObjectLayer| seam_of(l).dist(seam_of(&good)) / makepad_fab_slicer::geom::SCALE;
    // two seam moves, the nearer within the seam tolerance's failing range
    let n = good.islands[0].walls.iter().find(|w| w.role == Role::OuterWall).unwrap().loop_pts.len() - 1;
    let (near, far) = {
        let mut ks: Vec<(f64, usize)> = (1..n).map(|k| (dist(&with(&|l| seam_moved(l, k))), k)).filter(|(d, _)| *d > tol.seam).collect();
        ks.sort_by(|a, b| a.0.total_cmp(&b.0));
        (ks[0].1, ks[ks.len() - 1].1)
    };
    let (cand, wide, narrow) = (with(&|l| seam_moved(l, near)), with(&|l| seam_moved(l, far)), good.clone());
    let (f, excused) = printcheck::order_within(7, &cand, &good, &[&narrow], &tol);
    assert!(excused.is_empty() && f.iter().any(|x| x.check == Check::Seam), "no noise there, no excuse:\n{}", printcheck::lines(&f));
    let (f, excused) = printcheck::order_within(7, &cand, &good, &[&narrow, &wide], &tol);
    assert!(f.is_empty() && excused.len() == 1 && excused[0].kind() == "seams", "within the noise:\n{}", printcheck::lines(&f));
    let (f, excused) = printcheck::order_within(7, &wide, &good, &[&cand], &tol);
    assert!(excused.is_empty() && f.iter().any(|x| x.check == Check::Seam), "past the noise:\n{}", printcheck::lines(&f));
    // a dropped closed loop is a wall count: the noise dropping it too excuses nothing
    let dropped = with(&|l| {
        std::sync::Arc::make_mut(&mut l.islands[1]).walls.pop();
    });
    let (f, excused) = printcheck::order_within(7, &dropped, &good, &[&dropped], &tol);
    assert!(excused.is_empty() && f.iter().any(|x| x.to_string().starts_with("layer 7 island 1 [order] missing outer wall path")), "{}", printcheck::lines(&f));
    // held: the length a dropped loop takes, and a fill class the reference has not
    let f = printcheck::held(7, &dropped, &good, &tol);
    assert!(f.iter().any(|x| x.check == Check::Length && x.island == Some(1)), "{}", printcheck::lines(&f));
    let classed = with(&|l| {
        let mut fill = l.islands[0].walls[0].clone();
        fill.role = Role::TopSurface;
        std::sync::Arc::make_mut(&mut l.islands[0]).fills.push(fill);
    });
    let f = printcheck::held(7, &classed, &good, &tol);
    assert!(f.iter().any(|x| x.check == Check::Class && x.detail.contains("TopSurface")), "{}", printcheck::lines(&f));
    assert!(printcheck::held(7, &good, &good, &tol).is_empty());
}
