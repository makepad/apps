//! The lightening engine on known shapes: the distance transform against brute force, the
//! largest pocket and a row of holes in a strip, the searches on known curves, and the demo
//! shelf bracket (analytic, as printed in PLA) under its study: where it may be cut, the cuts
//! keeping off the fixtures and loads, and a solved variant.

use super::cuts::Cut;
use super::fit::{self, Mask};
use super::plan::{analyse, Ground, Rules};
use super::search::{infill_floor, tune, Goal, Score};
use crate::bc::Kind;
use crate::bench::{bracket, bracket_loads, BRACKET};
use crate::material::{Materials, Print, PLA};
use crate::solve::Options;
use crate::study::{Load, Spot};
use crate::surface::Pick;

/// The exact distance transform agrees with brute force on a mask with holes and a notch; the
/// largest rectangle of a known shape is found; a 40 x 10 mm strip takes a row of 6 mm holes a
/// web apart along its length.
#[test]
fn masks_measure_clearance_and_fit_rows_and_pockets() {
    let mut m = Mask::new(9, 7);
    for j in 0..7 {
        for i in 0..9 {
            m.set(i, j, !((i == 4 && j == 3) || (i == 0 && j < 3)));
        }
    }
    let d2 = fit::distance2(&m);
    for j in 0..7 {
        for i in 0..9 {
            let mut best = f64::INFINITY;
            // the border outside counts as off: cells at -1 and at w, h
            for y in -1..=7i32 {
                for x in -1..=9i32 {
                    let off = x < 0 || y < 0 || x >= 9 || y >= 7 || !m.at(x as usize, y as usize);
                    if off {
                        best = best.min(((x - i as i32).pow(2) + (y - j as i32).pow(2)) as f64);
                    }
                }
            }
            let want = if m.at(i, j) { best } else { 0.0 };
            assert_eq!(d2[i + 9 * j], want, "squared distance at ({i}, {j})");
        }
    }
    // an L: the 6 x 3 foot is the largest rectangle
    let mut l = Mask::new(6, 6);
    for j in 0..6 {
        for i in 0..6 {
            l.set(i, j, j < 3 || i < 2);
        }
    }
    let open: Vec<f64> = l.on.iter().map(|on| if *on { 0.0 } else { -1.0 }).collect();
    assert_eq!(fit::largest_rect(&l, &open), Some(([0, 0], [6, 3])));
    // a 40 x 10 strip at 1 mm cells: 6 mm holes with a 1.5 mm web fit 2 mm from the ends and the sides
    let mut strip = Mask::new(40, 10);
    strip.on.iter_mut().for_each(|v| *v = true);
    let clear = fit::clearance(&strip, 1.0, 1.5);
    let band = fit::bands(&strip, &clear, 1.0, 1.5).remove(0);
    assert!(band.dir[0].abs() > 0.99, "the band runs along the strip: {:?}", band.dir);
    let row = fit::row_of(&strip, &clear, 1.0, &band, 3.0, 1.5).expect("a row fits");
    // 5 x 6 mm + 4 webs between + 2 at the ends = 39 mm of the 40
    assert_eq!(row.count, 5, "5 holes of 6 mm a 1.5 mm web apart and from the ends: {row:?}");
    let pitch = (row.to[0] - row.from[0]) / 4.0;
    assert!(pitch >= 7.5 - 1e-9, "a web between holes: pitch {pitch}");
    assert!(row.from[0] - 3.0 >= 1.5 - 1e-9 && 40.0 - row.to[0] - 3.0 >= 1.5 - 1e-9, "a web at each end: {row:?}");
}

/// The knob search finds the most removal that holds the floor (a curve that is unsafe at its
/// top), and backs off to the goal when the top saves more than asked; the infill floor is
/// found to a percent.
#[test]
fn searches_meet_the_goal_with_the_fewest_solves() {
    let base = Score { plastic: 100.0, sf: 4.0 };
    let goal = Goal { cut: 0.3, min_sf: 2.0 };
    // removal k saves k / 2 and costs safety 3 k: safe up to k = 2/3 (saves a third)
    let mut eval = |k: f64| Ok(Score { plastic: 100.0 * (1.0 - 0.5 * k), sf: 4.0 - 3.0 * k });
    let (best, trace) = tune(&mut eval, &base, &goal, 0.0, 1.0, 6).unwrap();
    let best = best.expect("a safe variant");
    assert!(goal.safe(&best.score) && best.knob > 0.6 && best.knob <= 2.0 / 3.0, "the most removal that holds SF 2: {best:?}");
    assert!(trace.len() <= 6, "{} solves", trace.len());
    // removal saves k and costs little: the top saves 100 %, so it backs off to save about 30 %
    let mut eval = |k: f64| Ok(Score { plastic: 100.0 * (1.0 - k), sf: 4.0 - k });
    let (best, _) = tune(&mut eval, &base, &goal, 0.0, 1.0, 8).unwrap();
    let saved = Goal::saved(&base, &best.unwrap().score);
    assert!((0.3..0.32).contains(&saved), "it saves just the 30 % asked, keeping the most safety: {saved}");
    // nothing holds: none
    let mut eval = |_: f64| Ok(Score { plastic: 90.0, sf: 1.0 });
    assert!(tune(&mut eval, &base, &goal, 0.0, 1.0, 4).unwrap().0.is_none());
    let mut sf = |d: f64| Ok(1.0 + 10.0 * d);
    let (floor, tried) = infill_floor(&mut sf, 0.15, 2.0, 0.01).unwrap();
    let floor = floor.unwrap();
    assert!((0.1..0.11).contains(&floor), "SF 1 + 10 d reaches 2 at 10 %: {floor}");
    assert!(tried.len() <= 6, "{} solves", tried.len());
}

/// Cell size of the bracket tests: 3 cells through a 6 mm plate.
const H: f64 = 2.0;

fn printed() -> (Materials, Print) {
    let print = Print::default();
    (Materials::new(PLA, &print, true), print)
}

/// The demo bracket screwed flat to a wall (its back face held) under 100 N on its shelf: it
/// is two plates. The wall may only be pocketed from the front (its back is held) and the shelf
/// only from below (its top is loaded): no hole goes through either. The wall's pocket gets a
/// roof (its axis is level), the shelf's opens down so it splits into slots a bridge wide. Every
/// cut keeps its distance from the held and loaded faces, and the pocketed variant, solved, is
/// lighter as printed and still holds.
#[test]
fn the_bracket_held_by_its_back_is_pocketed_from_the_free_faces() {
    let (m, _) = printed();
    let (lo, hi) = BRACKET;
    let opts = Options::default();
    let loads = bracket_loads();
    let (model, base) = super::solve_field(lo, hi, H, &bracket, &loads, &m, &opts).unwrap();
    let conditions = model.conditions(&loads);
    let rules = Rules::default();
    let g = Ground { model: &model, results: &base.results, conditions: &conditions, materials: &m };
    let a = analyse(&g, &rules, 0.3);
    let axes: Vec<usize> = a.plates.iter().map(|p| p.plate.axis).collect();
    assert!(axes.contains(&1) && axes.contains(&2), "a wall (thin along y) and a shelf (thin along z): {axes:?}");
    for p in &a.plates {
        assert_eq!(p.through.count(), 0, "plate along {}: nothing goes through a held or loaded plate", p.plate.axis);
    }
    let wall = a.plates.iter().find(|p| p.plate.axis == 1).unwrap();
    let shelf = a.plates.iter().find(|p| p.plate.axis == 2).unwrap();
    assert!(wall.from_hi.count() > 0 && wall.from_lo.count() == 0, "the wall: from the front only ({} front, {} back)", wall.from_hi.count(), wall.from_lo.count());
    assert!(shelf.from_lo.count() > 0 && shelf.from_hi.count() == 0, "the shelf: from below only ({} below, {} above)", shelf.from_lo.count(), shelf.from_hi.count());
    assert!(a.holes(1.0).is_empty(), "no through holes");
    let pockets = a.pockets(1.0);
    let wall_pockets: Vec<&Cut> = pockets.iter().filter(|c| matches!(c, Cut::Pocket { axis, .. } if axis[1] < -0.5)).collect();
    let slots: Vec<&Cut> = pockets.iter().filter(|c| matches!(c, Cut::Pocket { axis, .. } if axis[2] > 0.5)).collect();
    assert!(matches!(wall_pockets[..], [Cut::Pocket { roof: true, .. }]), "one roofed pocket in the wall's front: {pockets:?}");
    assert!(!slots.is_empty(), "slots under the shelf: {pockets:?}");
    for s in &slots {
        let Cut::Pocket { size, roof, .. } = s else { unreachable!() };
        assert!(size[0].min(size[1]) <= rules.bridge + 1e-9 && !roof, "a slot that opens down is a bridge wide: {s:?}");
    }
    // every cut keeps off the held and loaded faces
    let s = &model.surface;
    for c in &conditions {
        for &q in &c.region.faces {
            let p = s.quad_center[q as usize].map(|v| v as f64);
            for cut in &pockets {
                let d = cut.distance(p);
                assert!(d > 0.5 * a.web, "{cut:?} reaches {d:.2} mm from a {:?} point {p:?}", c.kind);
            }
        }
    }
    let field = super::cuts::cut_field(&bracket, &pockets);
    let (vmodel, variant) = super::solve_field(lo, hi, H, &field, &loads, &m, &opts).unwrap();
    let before = super::mass::plastic(&model.grid, &m);
    let after = super::mass::plastic(&vmodel.grid, &m);
    eprintln!("bracket pockets: plastic {before:.0} -> {after:.0} mm3, SF {:.2} -> {:.2}", base.results.min_sf.0, variant.results.min_sf.0);
    // (measured 14859 -> 14679 mm3: the pockets take sparse infill, and their own walls print)
    assert!(after < before, "the pockets save plastic as printed: {before:.0} -> {after:.0} mm3");
    assert!(variant.results.min_sf.0.is_finite() && variant.results.min_sf.0 > 1.0, "and it holds: SF {}", variant.results.min_sf.0);
}

/// The bracket held at its wall's two screw holes' walls (rigid: no bolt's clamp on the wall's
/// faces), 100 N on its shelf: the wall's back is free now, so lightening holes go through the
/// wall as teardrops (their axis is level), clear of the held holes. At 40 % infill: at 15 % a
/// hole's own walls print about what a hole under 3.7 mm across saves.
#[test]
fn the_bracket_bolted_through_its_holes_takes_teardrops_through_its_wall() {
    let print = Print { density: 0.4, ..Print::default() };
    let m = Materials::new(PLA, &print, true);
    let (lo, hi) = BRACKET;
    let opts = Options::default();
    let x = 0.27 * 64.0;
    let wall = |at: crate::V3| Load { rigid: true, ..Load::new(Kind::Fixed, Spot::Seed { pick: Pick::Hole, at }) };
    let loads = [wall([-x, -20.0, 35.35 + 2.3]), wall([x, -20.0, 35.35 + 2.3]), bracket_loads()[1].clone()];
    let (model, base) = super::solve_field(lo, hi, H, &bracket, &loads, &m, &opts).unwrap();
    let conditions = model.conditions(&loads);
    let rules = Rules::default();
    let g = Ground { model: &model, results: &base.results, conditions: &conditions, materials: &m };
    let a = analyse(&g, &rules, 0.3);
    let holes = a.holes(1.0);
    let wall_rows: Vec<&Cut> = holes.iter().filter(|c| matches!(c, Cut::Holes { axis, .. } if axis[1].abs() > 0.5)).collect();
    assert!(!wall_rows.is_empty(), "rows of holes through the wall: {holes:?}");
    for c in &wall_rows {
        let Cut::Holes { teardrop: true, diameter, .. } = c else { panic!("a level hole is a teardrop: {c:?}") };
        // the tip, r sqrt 2 over each centre, stays a web inside the wall
        for o in c.centres() {
            let tip = [o[0], o[1], o[2] + 0.5 * diameter * std::f64::consts::SQRT_2];
            assert!(bracket(tip) < -0.5 * a.web, "{c:?}: its tip {tip:?} is {:.2} mm from the surface", bracket(tip));
        }
    }
    let s = &model.surface;
    for c in conditions.iter().filter(|c| c.kind == Kind::Fixed) {
        for &q in &c.region.faces {
            let p = s.quad_center[q as usize].map(|v| v as f64);
            for cut in &holes {
                assert!(cut.distance(p) > 0.5 * a.web, "{cut:?} reaches a bolted hole at {p:?}");
            }
        }
    }
    let field = super::cuts::cut_field(&bracket, &holes);
    let (vmodel, variant) = super::solve_field(lo, hi, H, &field, &loads, &m, &opts).unwrap();
    let (before, after) = (super::score(&model, &base, &m, 0), super::score(&vmodel, &variant, &m, 0));
    eprintln!("bracket teardrops: {} rows, plastic {:.0} -> {:.0} mm3, SF {:.2} -> {:.2}", holes.len(), before.plastic, after.plastic, before.sf, after.sf);
    assert!(after.plastic < before.plastic && after.sf > 1.0, "lighter as printed, and it holds: {before:?} -> {after:?}");
}

/// An Optimise run on the bracket held by its back, asked for 30 % lighter at SF 2: the
/// pockets alone fall short (their floors print as solid walls), so that family goes on with
/// the infill run down; the infill alone is a variant too; its plates taken thinner (its
/// `thickness` from 6 mm towards 2) meet the goal and come first. Every variant keeps the floor
/// and none goes under the lowest density. The masses are the real samples' as printed; the
/// safety factor here is a stand-in that falls with the plastic as a plate's bending does
/// (the solves are the bracket tests' above), so the run is the runner's logic alone.
#[test]
fn an_optimise_run_combines_cuts_with_the_infill_to_reach_the_goal() {
    use super::variants::{run, Candidate, Family, Levers, Size};
    let (m, print) = printed();
    let (lo, hi) = BRACKET;
    let loads = bracket_loads();
    let (model, base) = super::solve_field(lo, hi, H, &bracket, &loads, &m, &Options::default()).unwrap();
    let conditions = model.conditions(&loads);
    let rules = Rules::default();
    let g = Ground { model: &model, results: &base.results, conditions: &conditions, materials: &m };
    let base_score = Score { plastic: super::mass::plastic(&model.grid, &m), sf: 6.0 };
    let goal = Goal { cut: 0.3, min_sf: 2.0 };
    let levers = Levers::default();
    let mut solves = 0;
    let mut eval = |c: &Candidate| {
        solves += 1;
        let field = super::cuts::cut_field(&bracket, &c.cuts);
        let mut distance = |ps: &[crate::V3], out: &mut [f64]| ps.iter().zip(out.iter_mut()).for_each(|(p, o)| *o = field(*p));
        // the walls and skins it prints with are the candidate's
        let cm = c.materials(&m);
        let grid = crate::grid::Grid::from_field(lo, hi, H, &mut distance, &cm).0;
        // its plates thinner: the plastic with their thickness
        let t = c.sizes.iter().find(|s| s.0 == "thickness").map_or(6.0, |s| s.1);
        let plastic = super::mass::plastic(&grid, &cm) * t / 6.0;
        Ok(Score { plastic, sf: 6.0 * (plastic / base_score.plastic).powi(2) })
    };
    let sizes = [Size { name: "thickness".into(), value: 6.0, min: 2.0 }];
    let variants = run(&g, &rules, &base_score, &goal, &levers, &sizes, &mut eval).unwrap();
    eprintln!("optimise run: {solves} solves; {:?}", variants.iter().map(|v| (v.families.clone(), (v.saved * 100.0).round(), v.candidate.print.density, v.candidate.sizes.clone())).collect::<Vec<_>>());
    let kinds: Vec<&[Family]> = variants.iter().map(|v| &v.families[..]).collect();
    assert!(kinds.contains(&&[Family::Pockets, Family::Infill][..]) && kinds.contains(&&[Family::Infill][..]), "pockets with less infill, and less infill alone: {kinds:?}");
    for v in &variants {
        assert!(v.score.sf >= goal.min_sf, "{:?} keeps SF 2: {:?}", v.families, v.score);
        assert!(v.candidate.print.density >= levers.min_density - 1e-9 && v.candidate.print.density <= print.density + 1e-9, "{:?}: density {}", v.families, v.candidate.print.density);
    }
    assert!(variants.windows(2).all(|w| w[0].met >= w[1].met), "goals met first: {:?}", variants.iter().map(|v| (v.met, v.saved)).collect::<Vec<_>>());
    let thinner = variants.iter().find(|v| v.families[0] == Family::Thinner).unwrap();
    assert!(variants[0].met, "a variant that meets the goal comes first: {:?}", variants[0]);
    let t = thinner.candidate.sizes.first().map_or(6.0, |s| s.1);
    assert!(thinner.families == [Family::Thinner] && thinner.met && t > 2.0 && t < 6.0, "thinner plates meet the goal: {thinner:?}");
    let pocketed = variants.iter().find(|v| v.families[0] == Family::Pockets).unwrap();
    assert!(!pocketed.candidate.cuts.is_empty() && pocketed.candidate.print.density < print.density, "the pocketed variant cuts and thins the infill: {pocketed:?}");
    assert!(solves <= 5 * super::variants::STEPS + 3, "{solves} solves");
}

/// A 20 mm cube at the default settings (15 % infill) with a zone of 100 % infill in its middle,
/// an 8 mm box clear of its walls and skins: the optimiser weighs the cube heavier by the core's
/// volume times what its infill adds (1 - 0.15), in plastic and in grams at PLA's 1.24 g/cm³.
/// A candidate at 10 % infill keeps the core: it adds 1 - 0.10 of the core's volume.
#[test]
fn a_solid_core_weighs_in_the_optimisers_mass() {
    use super::variants::Candidate;
    use crate::bench::boxed;
    use crate::material::Zone;
    use std::sync::Arc;
    let (lo, hi) = ([0.0; 3], [20.0; 3]);
    let (core_lo, core_hi) = ([6.0; 3], [14.0; 3]);
    let core = 8f64.powi(3);
    let mut distance = |ps: &[crate::V3], out: &mut [f64]| ps.iter().zip(out.iter_mut()).for_each(|(p, o)| *o = boxed(*p, lo, hi));
    // the cube's plastic (mm³) under `m`, its grid graded with `m`
    let mut plastic = |m: &Materials| super::mass::plastic(&crate::grid::Grid::from_field(lo, hi, 1.0, &mut distance, m).0, m);
    let (part, print) = printed();
    let zone = Zone { at: Arc::new(move |p| boxed(p, core_lo, core_hi)), bounds: Some((core_lo, core_hi)), density: Some(1.0), infill: None, wall_loops: None };
    let cored = part.clone().with_zones(vec![zone]);
    let added = plastic(&cored) - plastic(&part);
    let want = core * (1.0 - print.density);
    let grams = (super::mass::grams(added, 1.24), super::mass::grams(want, 1.24));
    let candidate = Candidate { cuts: Vec::new(), sizes: Vec::new(), print: Print { density: 0.1, ..print.clone() } };
    let thinned = plastic(&candidate.materials(&cored)) - plastic(&candidate.materials(&part));
    eprintln!("solid core: adds {added:.2} mm3 ({:.4} g), want {want:.2} mm3 ({:.4} g); at 10 % it adds {thinned:.2} mm3", grams.0, grams.1);
    assert!((added - want).abs() <= 1e-6 * want, "the core adds {added:.4} mm3 of plastic, its volume times 1 - 0.15: {want:.4}");
    assert!((grams.0 - grams.1).abs() <= 1e-6 * grams.1, "the core adds {:.6} g, {:.6} g at 1.24 g/cm3", grams.0, grams.1);
    assert!((thinned - core * 0.9).abs() <= 1e-6 * core, "a candidate at 10 % keeps the core: it adds {thinned:.4} mm3, want {:.4}", core * 0.9);
}
