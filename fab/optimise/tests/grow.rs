//! Grow on this machine's GPU (Linux Vulkan is the gate) against its f64 twin with exact solves.

use fab_force::bc::Kind;
use fab_force::study::{Load, Spot};
use fab_force::surface::Pick;
use fab_optimise::grow::{answer, cantilever, config, no_room, oracle, program, run, settled, shape, Edit, Job, Level};
use fab_optimise::grown::{body_to_object, Posed};
use makepad_compute_program::gpu::GpuLink;
use makepad_compute_program::{Session, Updates};
use makepad_csg_math::Vec3d;
use makepad_model_script::lang::csg_graph::Op;
use makepad_model_script::lang::csg_sdf::{self, SdfGrid, SdfNode};
use makepad_model_script::lang::{evaluate, Assets, Options, Program};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn gpu() -> GpuLink {
    static GPU: OnceLock<GpuLink> = OnceLock::new();
    GPU.get_or_init(|| makepad_compute_program_gpu::spawn().expect("a GPU device")).clone()
}

/// A 24 x 12 x 12 mm box at 4 mm, then at 2 mm.
fn levels() -> Vec<Level> {
    vec![
        cantilever([6, 3, 3], 4.0, 8, 6.0, [1.0, 2.0], 100.0),
        cantilever([12, 6, 6], 2.0, 6, 3.0, [2.0, 4.0], 100.0),
    ]
}

#[test]
fn a_grow_holds_to_its_f64_twin() {
    let job = Job::new(levels(), 0.3);
    let exact = oracle(&job, None);
    let grown = run(&job, gpu()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(grown.iterations.len(), exact.len());
    for (g, x) in grown.iterations.iter().zip(&exact) {
        let c = (g.compliance - x.compliance).abs() / x.compliance;
        println!("level {} it {}: {:.5} against {:.5} ({:.3}%), {} CG, volume off by {:.1e}", g.level, g.it, g.compliance, x.compliance, 100.0 * c, g.pcg, (g.volume - g.target).abs() / g.target);
        assert!(c < 0.01, "level {} it {}: compliance {} against {}", g.level, g.it, g.compliance, x.compliance);
        let v = (g.volume - g.target).abs() / g.target;
        assert!(v < 1e-4, "level {} it {}: volume {} of {}", g.level, g.it, g.volume, g.target);
    }
}

#[test]
fn a_grow_gives_the_same_bytes_at_any_job_size() {
    let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<u32>>();
    let mut first = None;
    // from one dispatch a pass to every pass cut (the 2 mm level's node passes are 10 groups)
    for job_size in [1 << 16, 5, 2] {
        let mut job = Job::new(levels(), 0.3);
        job.job = job_size;
        let g = run(&job, gpu()).unwrap_or_else(|e| panic!("{e}"));
        let result = (bits(&g.x), bits(&g.xp), bits(&g.u), bits(&g.e), g.iterations.iter().map(|i| (i.compliance.to_bits(), i.volume.to_bits(), i.pcg)).collect::<Vec<_>>());
        match &first {
            None => first = Some(result),
            Some(f) => assert_eq!(f, &result, "{job_size} groups a dispatch"),
        }
    }
}

/// A long thin solid on one end: most of how it gives is a near rigid swing, so far that the rounding
/// of each cell's stiffness on a rigid move, added up over the cells, would outweigh what holds it.
#[test]
fn a_part_that_swings_far_holds_to_its_twin() {
    let mut job = Job::new(vec![cantilever([96, 4, 4], 1.0, 1, 1.5, [1.0, 1.0], 1.0)], 1.0);
    job.max_its = 2000;
    job.tol = 1e-3;
    let exact = oracle(&job, None);
    let grown = run(&job, gpu()).unwrap_or_else(|e| panic!("{e}"));
    let (c, x) = (grown.iterations[0].compliance, exact[0].compliance);
    println!("{c:.5} against {x:.5}, {} CG", grown.iterations[0].pcg);
    // a stiffness in f32 still takes a little of a swing: 1.25% on a bar this long
    assert!((c - x).abs() < 0.02 * x, "compliance {c} against {x}");
}

/// A level the device has no room for is not started: the grow stands at the one before it and
/// says so. With no room for its first it ends with nothing, and says so.
#[test]
fn a_grow_stops_at_the_level_that_fits() {
    let grown_within = |bytes: u64| {
        let job = Job::new(levels(), 0.3);
        let mut config = config(&job, Some(gpu()));
        config.limits.buffer_bytes = bytes;
        let session = Session::open(&program(), config);
        let mut seen = Updates::default();
        while seen.end.is_none() {
            seen.merge(session.wait());
        }
        seen
    };
    let seen = grown_within(160 << 10);
    assert_eq!(no_room(&seen), Some(1));
    let grown = answer(&seen).expect("it ended").unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(grown.x.len(), 6 * 3 * 3, "the first level's density");
    assert!(grown.iterations.iter().all(|i| i.level == 0));

    let seen = grown_within(8 << 10);
    assert_eq!(no_room(&seen), Some(0));
    assert!(seen.end.as_ref().is_some_and(|e| e.is_ok()), "{:?}", seen.end);
}

/// Reads a held session until it has settled for edit `rev`: its compliance and shape at its last level.
fn settle(session: &Session, seen: &mut Updates, rev: usize, level: usize) -> (f64, Vec<f32>) {
    loop {
        seen.merge(session.wait());
        if let Some(end) = &seen.end {
            panic!("the held grow ended: {end:?}");
        }
        if let Some(c) = settled(seen, rev) {
            return (c, shape(seen, level).expect("its shape").1);
        }
    }
}

/// The share of the solid that a change of shape moves: cells on one side of the iso in only one
/// of the two, over the first's solid.
fn moved(a: &[f32], b: &[f32]) -> f64 {
    let solid = a.iter().filter(|x| **x > 0.5).count();
    let differ = a.iter().zip(b).filter(|(x, y)| (**x > 0.5) != (**y > 0.5)).count();
    differ as f64 / solid as f64
}

/// A cantilever's load turned sideways by 5% of itself.
fn nudged(levels: &[Level]) -> Edit {
    let load = levels.iter().map(|l| l.load.iter().map(|f| [f[0], 0.05 * f[2].abs(), f[2], 0.0]).collect()).collect();
    Edit { volume: 0.3, load }
}

/// A settled grow answers an edit as its f64 twin does, with the same bytes at any job size.
#[test]
fn a_held_grow_answers_an_edit_as_its_twin() {
    let held = |job_size: usize| {
        let mut job = Job::new(levels(), 0.3);
        job.hold = true;
        job.job = job_size;
        let session = Session::open(&program(), config(&job, Some(gpu())));
        let mut seen = Updates::default();
        settle(&session, &mut seen, 0, 1);
        let rev = session.send("edit", nudged(&job.levels).value()) as usize;
        let answer = settle(&session, &mut seen, rev, 1);
        session.cancel();
        answer
    };
    let (compliance, shape) = held(1 << 16);
    let job = Job::new(levels(), 0.3);
    let exact = oracle(&job, Some((1, &nudged(&job.levels)))).last().unwrap().compliance;
    println!("settled at {compliance:.5} against the twin's {exact:.5}");
    assert!((compliance - exact).abs() / exact < 0.01, "{compliance} against the twin's {exact}");
    let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<u32>>();
    assert_eq!(bits(&shape), bits(&held(5).1), "the answer to an edit at another job size");
}

/// On a settled grow at its full sharpness, a 5% load nudge moves under 3% of the solid and ends as
/// stiff as a grow from nothing under that load.
#[test]
fn a_small_load_nudge_makes_a_small_change() {
    let sharp = || vec![cantilever([12, 6, 6], 4.0, 12, 6.0, [1.0, 3.0], 100.0), cantilever([24, 12, 12], 2.0, 24, 3.0, [3.0, 8.0], 100.0)];
    let mut job = Job::new(sharp(), 0.3);
    job.hold = true;
    let session = Session::open(&program(), config(&job, Some(gpu())));
    let mut seen = Updates::default();
    let (_, before) = settle(&session, &mut seen, 0, 1);
    let edit = nudged(&job.levels);
    let rev = session.send("edit", edit.value()) as usize;
    let (compliance, after) = settle(&session, &mut seen, rev, 1);
    session.cancel();
    let share = moved(&before, &after);
    println!("the nudge moved {:.2}% of the solid", 100.0 * share);
    assert!(share < 0.03, "a 5% nudge moved {:.2}% of the solid", 100.0 * share);
    assert!(share > 0.0, "the nudge changed nothing");

    let mut fresh = sharp();
    for (l, load) in fresh.iter_mut().zip(edit.load) {
        l.load = load;
    }
    let cold = run(&Job::new(fresh, 0.3), gpu()).unwrap_or_else(|e| panic!("{e}")).iterations.last().unwrap().compliance;
    println!("{compliance:.5} against {cold:.5} from nothing under that load");
    assert!(compliance < 1.03 * cold, "{compliance} against {cold} from nothing");
}

/// An edit the shape no longer fits (here the load turned square) grows again from nothing: to the
/// bytes of a grow that had that load from the start, in the session it has.
#[test]
fn a_big_edit_grows_from_nothing() {
    let turned = |levels: &[Level]| -> Vec<Vec<[f32; 4]>> { levels.iter().map(|l| l.load.iter().map(|f| [0.0, f[2].abs(), 0.0, 0.0]).collect()).collect() };
    let mut job = Job::new(levels(), 0.3);
    job.hold = true;
    let session = Session::open(&program(), config(&job, Some(gpu())));
    let mut seen = Updates::default();
    settle(&session, &mut seen, 0, 1);
    let rev = session.send("edit", Edit { volume: 0.3, load: turned(&job.levels) }.value()) as usize;
    let (compliance, regrown) = settle(&session, &mut seen, rev, 1);
    session.cancel();

    let load = turned(&job.levels);
    for (l, load) in job.levels.iter_mut().zip(load) {
        l.load = load;
    }
    let session = Session::open(&program(), config(&job, Some(gpu())));
    let (fresh_compliance, fresh) = settle(&session, &mut Updates::default(), 0, 1);
    session.cancel();
    let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<u32>>();
    assert_eq!(bits(&regrown), bits(&fresh), "{compliance} against {fresh_compliance} from nothing");
}

/// Its space off centre in its body's frame, its body turned and moved in the object's, and another
/// solid on it that stays whatever it grows.
const GROWN: &str = "use mod.fab.*\nlet m = mod.model\nlet space = m.box({size: [60mm 20mm 20mm] at: [10mm 0mm 0mm]})\nlet beam = m.grow(space, {mass: 0.3 at: [5mm 0mm 0mm] rotate: [0 0 90deg]})\nlet boss = m.sphere({radius: 3mm at: [-5mm 10mm 0mm]})\nlet joined = m.blend([beam, boss], 0.2mm)\nobject(joined)\n";

/// That body's point in the object's frame.
fn in_model(p: [f64; 3]) -> [f64; 3] {
    [5.0 - p[1], p[0], p[2]]
}

fn eval_grown(solved: HashMap<String, Arc<SdfGrid>>) -> Program {
    let assets = Assets::new();
    evaluate(GROWN, &Options { generated: &Default::default(), assets: &assets, images: &Default::default(), extension: "fab", libraries: &[], inputs: &makepad_model_script::lang::NO_INPUTS, unit: fab_model::program::UNIT, roots: fab_model::program::ROOTS, overrides: HashMap::new(), weave: None, solved }).unwrap_or_else(|d| panic!("{d:?}"))
}

fn root_of(p: &Program, name: &str) -> SdfNode {
    let Op::Field { shape, .. } = &p.graph.node(p.solid(name).unwrap().node).op else { panic!("a field") };
    shape.root.clone()
}

/// A script's `m.grow` from its ask to its shape: it asks and says so, is posed in its body's frame,
/// reads each shape; the shape keeps its mass and joins the load to the fixture; a 5% nudge of the
/// load moves under 3% of it.
#[test]
fn a_grown_part_reads_its_shape() {
    let plain = eval_grown(HashMap::new());
    let request = plain.grow_requests.first().expect("the part asks").clone();
    assert!(!plain.notes.is_empty(), "it says it waits");
    let root = root_of(&plain, "joined");
    let frame = body_to_object(&root, "beam").unwrap_or_else(|e| panic!("{e}"));
    for p in [[0.0; 3], [1.0, 2.0, 3.0]] {
        let got: [f64; 3] = std::array::from_fn(|r| frame[r][0] * p[0] + frame[r][1] * p[1] + frame[r][2] * p[2] + frame[r][3]);
        let want = in_model(p);
        assert!((0..3).all(|a| (got[a] - want[a]).abs() < 1e-9), "{p:?} goes to {got:?}, not {want:?}");
    }
    let held_end = [-20.0, 0.0, 0.0];
    let loaded_end = [40.0, 0.0, 0.0];
    let loads = |sideways: f64| {
        vec![
            Load::new(Kind::Fixed, Spot::Seed { pick: Pick::Face, at: held_end }),
            Load::new(Kind::Force([0.0, sideways * 50.0, -50.0]), Spot::Seed { pick: Pick::Face, at: loaded_end }),
        ]
    };
    let posed = Posed::new(&root, &request, &loads(0.0)).unwrap_or_else(|e| panic!("{e}"));
    let last = posed.job().levels.len() - 1;
    let session = Session::open(&program(), config(posed.job(), Some(gpu())));
    let mut seen = Updates::default();
    // its first look weighs what it should, though its densities are still soft
    let first = loop {
        seen.merge(session.wait());
        if let Some((_, xp)) = shape(&seen, 0) {
            break xp;
        }
    };
    let level = &posed.job().levels[0];
    let solid = posed.grid(0, &first, 0.3).data.iter().filter(|d| **d < 0.0).count() as f64;
    println!("its first look has {solid} solid cells of the {:.0} it should", level.target(0.3));
    assert!((solid - level.target(0.3)).abs() < 0.02 * level.target(0.3), "{solid} solid cells of {}", level.target(0.3));
    let (_, before) = settle(&session, &mut seen, 0, last);

    let grown = eval_grown(HashMap::from([("beam".to_string(), Arc::new(posed.grid(last, &before, 0.3)))]));
    assert!(grown.notes.is_empty(), "no note once it has its shape: {:?}", grown.notes);
    let solid = root_of(&grown, "joined");

    // its solid over its space's cells, a cell's side apart, read in the model's frame
    let (n, h) = ([60usize, 20, 20], 1.0);
    let index = |i: usize, j: usize, k: usize| i + n[0] * (j + n[1] * k);
    let mut inside = vec![false; n[0] * n[1] * n[2]];
    for k in 0..n[2] {
        for j in 0..n[1] {
            for i in 0..n[0] {
                let p = in_model([-20.0 + (i as f64 + 0.5) * h, -10.0 + (j as f64 + 0.5) * h, -10.0 + (k as f64 + 0.5) * h]);
                inside[index(i, j, k)] = csg_sdf::distance(&solid, Vec3d::new(p[0], p[1], p[2])) < 0.0;
            }
        }
    }
    let share = inside.iter().filter(|s| **s).count() as f64 / inside.len() as f64;
    println!("the shape keeps {:.1}% of its space", 100.0 * share);
    // what it asks of the level, kept cells and all, over its space
    let l = posed.job().levels.last().unwrap();
    let asked = l.target(0.3) * l.h.powi(3) / (n[0] * n[1] * n[2]) as f64;
    assert!((share - asked).abs() < 0.1 * asked, "keeps {share} of its space, asked {asked}");

    // its box, from its grid's samples, holds all of that, and found solid close to each side
    let Op::Field { shape: field, .. } = &grown.graph.node(grown.solid("joined").unwrap().node).op else { panic!("a field") };
    let started = std::time::Instant::now();
    let identity = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
    let e = csg_sdf::shape_bounds(field, &identity, 0.01).expect("it holds a solid");
    println!("its box in {:.2} ms: {:?}", started.elapsed().as_secs_f64() * 1e3, e);
    for k in 0..n[2] {
        for j in 0..n[1] {
            for i in 0..n[0] {
                let p = in_model([-20.0 + (i as f64 + 0.5) * h, -10.0 + (j as f64 + 0.5) * h, -10.0 + (k as f64 + 0.5) * h]);
                assert!(!inside[index(i, j, k)] || (0..3).all(|a| p[a] >= e.min[a] && p[a] <= e.max[a]), "{p:?} past {e:?}");
            }
        }
    }
    let (lo, hi) = e.found.expect("solid points");
    let open = (0..3).map(|a| (lo[a] - e.min[a]).max(e.max[a] - hi[a])).fold(0.0f64, f64::max);
    assert!(open <= 0.1, "{open} between its box and the solid found");

    // from the cells at the fixture, through solid, to the cells at the load
    let mut reached = vec![false; inside.len()];
    let mut front: Vec<[usize; 3]> = Vec::new();
    for k in 0..n[2] {
        for j in 0..n[1] {
            if inside[index(0, j, k)] {
                reached[index(0, j, k)] = true;
                front.push([0, j, k]);
            }
        }
    }
    assert!(!front.is_empty(), "solid at the fixture");
    while let Some([i, j, k]) = front.pop() {
        let at = [i, j, k];
        for a in 0..3 {
            for d in [-1isize, 1] {
                let mut q = at;
                let v = at[a] as isize + d;
                if v < 0 || v >= n[a] as isize {
                    continue;
                }
                q[a] = v as usize;
                let c = index(q[0], q[1], q[2]);
                if inside[c] && !reached[c] {
                    reached[c] = true;
                    front.push(q);
                }
            }
        }
    }
    let joined = (0..n[2]).any(|k| (0..n[1]).any(|j| reached[index(n[0] - 1, j, k)]));
    assert!(joined, "solid joins the load to the fixture");

    let edit = posed.edit(&loads(0.05), 0.3).expect("the same nodes hold and load it");
    let rev = session.send("edit", edit.value()) as usize;
    let (_, after) = settle(&session, &mut seen, rev, last);
    session.cancel();
    let share = moved(&before, &after);
    println!("the nudge moved {:.2}% of the solid", 100.0 * share);
    assert!(share < 0.03, "a 5% nudge moved {:.2}% of the solid", 100.0 * share);
    assert!(share > 0.0, "the nudge changed nothing");
    assert!(posed.edit(&[loads(0.0)[0].clone(), Load::new(Kind::Force([0.0, 0.0, -50.0]), Spot::Seed { pick: Pick::Face, at: [10.0, 0.0, 10.0] })], 0.3).is_none(), "a load moved elsewhere grows again");
}
