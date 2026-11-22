//! A part planned from given pieces, as a GPU gives them, equals the part planned on the cores:
//! its stages' own results, recorded and put back with the traces, plan it byte for byte at any
//! thread count, and every piece is taken instead of made.

use crate::given::{Bead, GivenIsland, GivenLayer, GivenUse};
use crate::plate_tests::{input, job, part, threads_held, PART};
use crate::stages::{PartPlan, Plate, PlateCtl, PlatePart, PlateRun, Stage, Task};
use crate::*;
use makepad_fab_slicer::sdf::SdfSource;

/// The part planned on the cores, and what its stages made that a GPU can give.
fn captured(job: &Job, o: &ObjectInput, heights: &[(f64, f64)]) -> (String, Vec<GivenLayer>) {
    let plan = PartPlan::new(0, o, job, heights.to_vec()).with_capture();
    let plate = Plate::new(vec![PlatePart { plan: &plan, targets: vec![Task::part(Stage::Finish)], urgency: 0, focus: None }]);
    assert_eq!(plate.run(PlateCtl::default()), PlateRun::Finished);
    drop(plate);
    (format!("{:?}", plan.take_finished().layers), plan.captured())
}

/// The part planned from `given`, every trace put with its given pieces before the plate is made,
/// each its outline when the layer is `compensated`.
fn planned(job: &Job, o: &ObjectInput, heights: &[(f64, f64)], given: &[GivenLayer], classes: bool) -> (String, GivenUse) {
    let plan = PartPlan::new(0, o, job, heights.to_vec()).with_given_classes(classes);
    let p = Params::from_job(job, 0);
    let xy = XyComp::of(job, o);
    for (k, &(z, h)) in heights.iter().enumerate() {
        let contours = o.source.contours(&[cut_z(&p, z, h)]).pop().unwrap();
        let contours = if given[k].compensated { xy.apply(&contours) } else { contours };
        plan.put_trace(k, contours, Some(Arc::new(given[k].clone())));
    }
    let plate = Plate::new(vec![PlatePart { plan: &plan, targets: vec![Task::part(Stage::Finish)], urgency: 0, focus: None }]);
    assert_eq!(plate.run(PlateCtl::default()), PlateRun::Finished);
    drop(plate);
    (format!("{:?}", plan.take_finished().layers), plan.given_use())
}

fn islands(given: &[GivenLayer], has: impl Fn(&GivenIsland) -> bool) -> usize {
    given.iter().flat_map(|l| &l.islands).filter(|i| has(i)).count()
}

#[test]
fn a_part_planned_from_its_given_pieces_is_the_part_planned_on_the_cores() {
    let _threads = threads_held();
    let before = par::threads();
    for generator in ["classic", "arachne"] {
        let job = job(generator);
        let o = input(Box::new(SdfSource::new(Arc::new(part), PART)));
        let heights = plan_heights(&o, &job);
        par::set_threads(16);
        let (cores, given) = captured(&job, &o, &heights);
        let beads: usize = given.iter().flat_map(|l| &l.islands).filter_map(|i| i.concentric.as_ref()).map(|c| c.vertical.len() + c.narrow.len()).sum();
        assert!(beads > 0, "{generator}: the part has concentric fills to give");
        assert!(given.iter().flat_map(|l| &l.islands).filter_map(|i| i.concentric.as_ref()).flat_map(|c| c.vertical.iter().chain(&c.narrow)).flatten().any(|b| matches!(b, Bead::Loop { .. })));
        // The walls and their labels alone: the classes are made on the cores.
        let walls: Vec<GivenLayer> = given.iter().map(|l| GivenLayer {
            classes: None,
            islands: l.islands.iter().map(|i| GivenIsland { walls: i.walls.clone(), labels: i.labels.clone(), ..Default::default() }).collect(),
            compensated: false,
        }).collect();
        let every = GivenUse {
            walls: islands(&given, |i| i.walls.is_some()),
            labels: islands(&given, |i| i.labels.is_some()),
            classes: heights.len(),
            island_classes: islands(&given, |i| i.classes.is_some()),
            narrow: islands(&given, |i| i.narrow.is_some()),
            concentric: islands(&given, |i| i.concentric.is_some()),
        };
        assert!(every.walls > 0 && every.labels > 0 && every.island_classes > 0 && every.narrow > 0, "{generator}: {every:?}");
        for threads in [1, 4, 16] {
            par::set_threads(threads);
            let (plan, used) = planned(&job, &o, &heights, &given, true);
            assert!(plan == cores, "{generator} at {threads} threads: the plan from every given piece differs");
            assert_eq!(used, every, "{generator} at {threads} threads: every given piece is taken");
            let (plan, used) = planned(&job, &o, &heights, &walls, false);
            assert!(plan == cores, "{generator} at {threads} threads: the plan from the given walls differs");
            assert_eq!(used, GivenUse { walls: every.walls, labels: every.labels, ..Default::default() }, "{generator} at {threads} threads");
        }
    }
    par::set_threads(before);
}

/// A trace that is the part's outline already (a GPU's, XY-compensated) plans as the cores plan
/// the raw cut.
#[test]
fn a_compensated_trace_plans_as_the_raw_cut() {
    let _threads = threads_held();
    let mut job = job("classic");
    job.cfg.set_str("xy_contour_compensation", "0.05");
    job.cfg.set_str("xy_hole_compensation", "0.03");
    job.cfg.set_str("elefant_foot_compensation", "0");
    let o = input(Box::new(SdfSource::new(Arc::new(part), PART)));
    assert_ne!(XyComp::of(&job, &o), XyComp::default());
    let heights = plan_heights(&o, &job);
    let (cores, mut given) = captured(&job, &o, &heights);
    for l in &mut given {
        l.compensated = true;
    }
    let (plan, used) = planned(&job, &o, &heights, &given, true);
    assert!(plan == cores, "the plan from a compensated trace differs");
    assert_eq!(used.classes, heights.len());
}
