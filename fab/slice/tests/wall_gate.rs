//! The wall gate on the CPU path (GPU-EXEC §10.6): in every case whose CPU contour lies on the
//! zero set (one `#[test]` each, plates.rs) the CPU's own contours as the candidate trace
//! (`trace::`, W1's GPU stage, scored against the zero set) pass every check, and the stage cases (`stage::`, stage_cases.rs) hold
//! every property on the CPU's walls stage; a broken trace or stage output fails.

#[macro_use]
#[path = "gate/plates.rs"]
mod plates;

use makepad_fab_slice::gate::{self, Kind, Report};
use makepad_fab_slice::stage_cases::{self, StageCase, StageLayer};
use makepad_fab_slicer::geom::{ExPolygon, Polygon, Pt};
use makepad_fab_slicer::source::WallShape;
use makepad_fab_toolpath::printcheck::{Check, Tol};

const GENERATORS: [&str; 2] = ["classic", "arachne"];

fn job(generator: &str) -> makepad_fab_settings::machine::Job {
    makepad_fab_slice::prepare_job(&plates::setup(1, generator)).expect("the gate's job")
}

/// One case with the CPU's own contours as the candidate: its report printed, and a failing
/// case named.
fn cpu_case(case: plates::Case) {
    let part = case.part;
    let report = match case.at {
        Some(at) => gate::run_at(part, &Tol::default(), at, gate::cpu_trace(part)),
        None => gate::run(part, &Tol::default(), gate::cpu_trace(part)),
    };
    println!("{report}");
    assert!(report.ok(), "the CPU path fails the gate on {}", report.part);
}

mod trace {
    use super::*;
    on_zero_set_cases!(cpu_case);
}

/// The bracket's mid layer with an island dropped from its trace fails walls and order there,
/// and only there.
#[test]
fn a_dropped_island_on_a_real_plate_fails_walls_and_order() {
    let mid = 100;
    assert!(plates::BRACKET_LAYERS.contains(&(mid, Kind::Mid)));
    let reports: Vec<Report> = std::thread::scope(|s| {
        let runs: Vec<_> = GENERATORS
            .iter()
            .map(|g| {
                s.spawn(move || {
                    let part = plates::demo_case("Shelf bracket", g).part;
                    gate::run_at(part, &Tol::default(), plates::BRACKET_LAYERS, |at| {
                        let mut got = gate::cpu_trace(part)(at)?;
                        got.iter_mut().find(|s| s.index == mid).expect("the middle layer").outline.pop().expect("an island");
                        Ok(got)
                    })
                })
            })
            .collect();
        runs.into_iter().map(|h| h.join().expect("a gate run")).collect()
    });
    for (generator, report) in GENERATORS.iter().zip(reports) {
        println!("{generator}: {report}");
        let at_mid: Vec<_> = report.failures.iter().filter(|f| f.layer == mid).collect();
        assert!(at_mid.iter().any(|f| f.check == Check::Islands && f.detail.starts_with("missing island")), "{generator}: the walls checks miss the dropped island:\n{report}");
        assert!(at_mid.iter().any(|f| f.detail.starts_with("missing planned island")), "{generator}: the order checks miss the dropped island:\n{report}");
        assert!(report.failures.iter().all(|f| f.layer == mid), "{generator}: only the broken layer fails:\n{report}");
    }
}

/// A chosen layer that is not what its case claims fails `case`.
#[test]
fn a_case_on_the_wrong_layer_fails() {
    let part = plates::demo_case("Shelf bracket", "classic").part;
    let report = gate::run_at(part, &Tol::default(), &[(100, Kind::Bridge), (1000, Kind::Mid)], gate::cpu_trace(part));
    assert!(report.failures.iter().any(|f| f.check == Check::Case && f.detail.contains("past the part's 260 layers")), "{report}");
    let report = gate::run_at(part, &Tol::default(), &[(100, Kind::Bridge)], gate::cpu_trace(part));
    assert!(report.failures.iter().any(|f| f.check == Check::Case && f.to_string().starts_with("layer 100 [case] not a bridge layer on the CPU")), "{report}");
}

#[test]
fn a_candidate_that_errs_or_skips_a_layer_fails() {
    let (name, shape) = gate::analytic().remove(0);
    let part = gate::analytic_part(name, shape, gate::ANALYTIC_HEIGHT, &job("classic")).expect("a field part");
    let erred = gate::run(&part, &Tol::default(), |_| Err("device lost".into()));
    assert!(!erred.ok() && erred.to_string().contains("the candidate failed: device lost"), "{erred}");
    let skipped = gate::run(&part, &Tol::default(), |at| {
        let mut got = gate::cpu_trace(&part)(at)?;
        got.retain(|s| s.index != 1);
        Ok(got)
    });
    assert!(!skipped.ok() && skipped.to_string().contains("the candidate has no layers [1]"), "{skipped}");
}

/// A trace's islands are planned by the cores; its outline is still its own and is held to the
/// zero set and to the CPU's: a traced outline 0.03 mm off fails `outline`.
#[test]
fn a_traced_outline_off_by_three_hundredths_fails() {
    let case = plates::analytic_case("square", "arachne");
    let part = case.part;
    let traced = |shift: f64| {
        gate::run(part, &Tol::default(), |at| {
            let mut got = gate::cpu_trace(part)(at)?;
            for o in got.iter_mut().flat_map(|s| s.outline.iter_mut()) {
                o.translate(Pt::from_mm(shift, 0.0));
            }
            Ok(got)
        })
    };
    let fine = traced(0.0);
    assert!(fine.ok() && fine.planned > 0, "{fine}");
    let moved = traced(0.03);
    println!("{moved}");
    assert!(moved.failures.iter().any(|f| f.check == Check::Outline && f.detail.contains("0.0300 mm")), "{moved}");
}

/// The stage cases on the CPU's stage: every property holds.
mod stage {
    use super::*;
    fn stage_case(case: StageCase) {
        let part = &case.part;
        let report = stage_cases::run(&case, &|at, trace| stage_cases::cpu_stage(part, at, trace));
        println!("{report}");
        assert!(report.ok(), "the CPU's stage fails the stage case on {}", report.part);
    }
    stage_cases!(stage_case);
}

/// A stage case on `stage` (None: the CPU's stage) with its output changed by `change`.
fn broken_stage(name: &str, stage: Option<&StageCase>, change: impl Fn(&mut Vec<StageLayer>)) -> Report {
    let case = plates::stage_case(name);
    let part = &stage.unwrap_or(&case).part;
    let report = stage_cases::run(&case, &|at, trace| {
        let mut out = stage_cases::cpu_stage(part, at, trace)?;
        change(&mut out);
        Ok(out)
    });
    println!("{name}: {report}");
    report
}

fn fails_property(report: &Report, prop: &str) {
    assert!(report.failures.iter().any(|f| f.check == Check::Prop && f.detail.starts_with(prop)), "no {prop} failure:\n{report}");
}

/// Layer `k`'s walls moved by `dx` mm (wall `only`, or every wall).
fn walls_moved(out: &mut [StageLayer], k: usize, only: Option<usize>, dx: f64) {
    let s = out.iter_mut().find(|s| s.index == k).expect("the layer");
    let d = Pt::from_mm(dx, 0.0);
    for w in s.walls.iter_mut() {
        match &mut w.shape {
            WallShape::Rings(rings) => {
                for (_, r) in rings.iter_mut().enumerate().filter(|(j, _)| only.map_or(true, |o| o == *j)) {
                    for q in r.iter_mut() {
                        for p in q.pts.iter_mut() {
                            *p = Pt { x: p.x + d.x, y: p.y + d.y };
                        }
                    }
                }
            }
            WallShape::Beads(beads) => {
                for b in beads.iter_mut().filter(|b| only.map_or(true, |o| o == b.inset)) {
                    for p in b.pts.iter_mut() {
                        *p = Pt { x: p.x + d.x, y: p.y + d.y };
                    }
                }
            }
        }
    }
}

/// Each property fails on a stage output broken its way, naming the property.
#[test]
fn each_stage_property_catches_its_break() {
    // an island dropped; a pinched outline taken for one island
    fails_property(&broken_stage("islands_split", None, |out| {
        let s = out.iter_mut().find(|s| s.index == 1).unwrap();
        s.outline.truncate(1);
        s.walls.truncate(1);
    }), "islands");
    fails_property(&broken_stage("islands_split", None, |out| {
        let s = out.iter_mut().find(|s| s.index == 1).unwrap();
        let ring = [(-2.0, -2.0), (0.0, -2.0), (0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0), (0.0, 0.0), (-2.0, 0.0)];
        s.outline = vec![ExPolygon { contour: Polygon::new(ring.iter().map(|&(x, y)| Pt::from_mm(x, y)).collect()), holes: Vec::new() }];
        s.walls.truncate(1);
    }), "pinches");
    // a middle layer printed with one wall
    fails_property(&broken_stage("count_rules", None, |out| {
        let s = out.iter_mut().find(|s| s.index == 10).unwrap();
        for w in s.walls.iter_mut() {
            if let WallShape::Rings(r) = &mut w.shape {
                r.truncate(1);
            }
        }
    }), "wall count");
    // walls 5 µm off their level (the 0.12 mm middle layer, which has all three walls), a fill
    // 5 µm off its own
    fails_property(&broken_stage("adaptive_levels", None, |out| walls_moved(out, 1, Some(1), 0.005)), "exact levels");
    fails_property(&broken_stage("adaptive_levels", None, |out| {
        let s = out.iter_mut().find(|s| s.index == 1).unwrap();
        for w in s.walls.iter_mut() {
            for e in w.fill.iter_mut() {
                e.translate(Pt::from_mm(0.005, 0.0));
            }
        }
    }), "fill level");
    // corners and radii 3 µm and 5 µm off
    fails_property(&broken_stage("box_corners_classic", None, |out| walls_moved(out, 1, None, 0.003)), "corners");
    fails_property(&broken_stage("disk_radius_classic", None, |out| walls_moved(out, 1, Some(0), 0.005)), "radius");
    // a miter where the join is round
    let classic = plates::stage_case("l_corner_classic");
    fails_property(&broken_stage("l_corner_arachne", Some(&classic), |_| {}), "round join");
    // the first layer's base left whole
    fails_property(&broken_stage("foot_split", None, |out| {
        let s = out.iter_mut().find(|s| s.index == 0).unwrap();
        for w in s.walls.iter_mut() {
            if let WallShape::Rings(r) = &mut w.shape {
                r[0].truncate(1);
            }
        }
    }), "foot split");
    // the hole compensated with the wrong sign
    let grown = plates::stage_case("compensation_grow");
    fails_property(&broken_stage("compensation_mixed", Some(&grown), |_| {}), "compensation");
    // a second run that differs
    let runs = std::sync::atomic::AtomicUsize::new(0);
    fails_property(&broken_stage("wedge_turns_classic", None, |out| {
        if runs.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 1 {
            walls_moved(out, 1, Some(0), 1e-6);
        }
    }), "deterministic");
    // a thick island's bead 0.02 mm wide of its spacing
    fails_property(&broken_stage("arachne_beads", None, |out| {
        let s = out.iter_mut().find(|s| s.index == 1).unwrap();
        for w in s.walls.iter_mut() {
            if let WallShape::Beads(b) = &mut w.shape {
                if b.iter().all(|l| l.closed && !l.odd) {
                    for x in b[0].widths.iter_mut() {
                        *x += 0.02;
                    }
                }
            }
        }
    }), "nominal beads");
}
