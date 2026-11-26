//! Field parts slice on the cores through the live engine, the way the app slices them: a vase
//! (engine nodes), a lattice (a Splash-kernel field) and a Links part each settle to the G-code
//! the plain slice of the same frames prints ([`slice_frames`], the engine's reference), byte for
//! byte with its settings. Each part is the smallest of its kind and the layers are thick: the
//! path, not the detail, is under test.

use fab_model::eval::Evaluated;
use fab_model::FabModel;
use fab_plate::machine::Machine;
use makepad_fab_settings::profiles::{JobSetup, Preset};
use makepad_fab_slice::live::{slice_frames, Frame, LiveEngine, LiveEvent, LiveInput, Next, PartField, Spawn, Urgency};
use std::sync::{atomic::{AtomicU64, Ordering}, mpsc, Arc, Mutex};

fn setup() -> JobSetup {
    let mut process = Preset::default();
    for (k, v) in [("wall_generator", "classic"), ("layer_height", "0.4"), ("sparse_infill_density", "15%"), ("sparse_infill_pattern", "gyroid"), ("wall_loops", "2"), ("support_type", "tree(auto)")] {
        process.values.set_str(k, v);
    }
    JobSetup { machine: Preset::default(), process, filaments: vec![Preset::default()], filament_map: vec![1], nozzle_volume_type: vec!["Standard".into()], bed_type: "Textured PEI Plate".into(), filament_map_mode: "Manual".into(), filament_colours: vec![None] }
}

fn evaluate(project: &str, script: (&str, &str)) -> Evaluated {
    let scripts = [(script.0.to_string(), script.1.to_string())].into_iter().collect();
    let m = FabModel::load("cores", project, scripts).unwrap_or_else(|f| panic!("{f}"));
    let mut e = fab_model::eval::Evaluator::new(Machine::dual_nozzle_350());
    m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}")).1
}

/// The engine's input as the app makes it: each part's frame carries its placed field.
fn input(ev: &Evaluated) -> LiveInput {
    let frames: Vec<Frame> = ev.frames.iter().enumerate().map(|(i, f)| {
        let field = ev.fields.get(i).and_then(Option::as_ref).and_then(|pf| PartField::placed(pf.shape.clone(), pf.to_part).map(|f| f.with_bounds(pf.bounds.map(|b| (b.min, b.max)))));
        Frame { local: f.local.clone(), offset: f.offset, field }
    }).collect();
    assert!(frames.iter().all(|f| f.field.is_some()), "every part is a field part");
    let mut i = LiveInput::ready(1, 1, setup(), Arc::new(ev.plate.printable()), Arc::new(Vec::new()));
    i.frames = Some(Arc::new(frames));
    i.machine = ev.machine.clone();
    i
}

/// An engine on std threads whose continuations skip a stale input's work, as the app's pool
/// tickets do, with no GPU attached: every field is cut on the cores.
fn engine() -> (LiveEngine, mpsc::Receiver<LiveEvent>) {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let spawn: Spawn = Arc::new(|_, job| { std::thread::spawn(job); });
    let version = Arc::new(AtomicU64::new(0));
    let next: Next = Arc::new(move || {
        let mine = version.fetch_add(1, Ordering::AcqRel) + 1;
        let version = version.clone();
        Arc::new(move || version.load(Ordering::Acquire) != mine)
    });
    let engine = LiveEngine::new(spawn.clone(), Arc::new(move |e| { let _ = tx.lock().unwrap().send(e); }), next);
    engine.set_resume(Arc::new(move |checkpoint| {
        let spawn = spawn.clone();
        Arc::new(move |urgency, job| {
            let checkpoint = checkpoint.clone();
            spawn(urgency, Box::new(move || { if urgency == Urgency::Viewed || !checkpoint() { job(); } }));
        })
    }));
    (engine, rx)
}

/// Settles the plate on the cores: its layers and its G-code.
fn settle_gcode(ev: &Evaluated) -> (usize, String) {
    makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map_or(4, |n| n.get())).min(8));
    let (e, rx) = engine();
    e.settle(1, input(ev));
    loop {
        match rx.recv().expect("the plate settles") {
            LiveEvent::Settled { tag: 1, result, .. } => {
                let s = result.unwrap_or_else(|e| panic!("the settle failed: {e}"));
                return (s.stats.layers, s.gcode.text().expect("the settled G-code").to_string());
            }
            LiveEvent::Failed { error, .. } => panic!("the slice failed: {error}"),
            _ => {}
        }
    }
}

/// The plate settled on the cores prints what its plain slice prints: the whole G-code, its
/// settings included, byte for byte.
fn settles_as_sliced(ev: &Evaluated, what: &str) {
    let (layers, gcode) = settle_gcode(ev);
    let i = input(ev);
    let plain = slice_frames(&setup(), &i.plate, i.frames.as_ref().expect("field frames"), &[], None).unwrap_or_else(|e| panic!("the {what}'s plain slice failed: {e}"));
    assert!(plain.stats.layers > 0 && layers == plain.stats.layers, "the {what} settles {layers} layers, its plain slice {}", plain.stats.layers);
    if gcode != plain.gcode {
        let k = gcode.lines().zip(plain.gcode.lines()).position(|(a, b)| a != b);
        let line = |g: &str| k.and_then(|k| g.lines().nth(k)).unwrap_or("(none: one ends first)").to_string();
        panic!("the {what}'s settled G-code ({} bytes) is not its plain slice's ({} bytes): first at line {k:?}, {:?} against {:?}", gcode.len(), plain.gcode.len(), line(&gcode), line(&plain.gcode));
    }
}

fn vase() -> Evaluated {
    evaluate("use mod.fab.*\nscript(\"vase.cad.splash\", params: {vase_height: 30mm vase_belly: 10mm vase_neck: 5mm})\nplace(\"vase\", at: [150mm 150mm], name: \"Bud vase\")\n",
        ("vase.cad.splash", include_str!("../../app/resources/vase.cad.splash")))
}

#[test]
fn a_vase_settles_on_the_cores() {
    settles_as_sliced(&vase(), "vase");
}

/// The vase sliced under looser boxes (as a weaker extent proof gives them, each holding the
/// solid) cuts the same contours, bit for bit, and prints the same G-code, byte for byte: the box
/// decides which grid cells there are, never where a sample sits, and the planner centres the
/// part's patterns on the shape's own box.
#[test]
fn a_looser_extent_prints_the_same_gcode() {
    let ev = vase();
    let looser = |grow: [f64; 4]| {
        let mut ev = ev.clone();
        for b in ev.fields.iter_mut().flatten().filter_map(|f| f.bounds.as_mut()) {
            (b.min[0], b.min[1], b.max[0], b.max[1]) = (b.min[0] - grow[0], b.min[1] - grow[1], b.max[0] + grow[2], b.max[1] + grow[3]);
        }
        ev
    };
    // Each field's contours at planes through it: per layer, its rings' lengths and points.
    let traced = |ev: &Evaluated| -> Vec<Vec<u64>> {
        ev.fields.iter().flatten().flat_map(|pf| {
            let src = PartField::placed(pf.shape.clone(), pf.to_part).unwrap().with_bounds(pf.bounds.map(|b| (b.min, b.max))).source().unwrap();
            let (lo, hi) = src.bounds();
            let zs: Vec<f64> = (0..).map(|k| lo[2] + 0.4 * (k as f64 + 0.5)).take_while(|z| *z < hi[2]).collect();
            src.contours(&zs).into_iter().map(|layer| layer.iter().flat_map(|e| e.rings().flat_map(|r| std::iter::once(r.pts.len() as u64).chain(r.pts.iter().flat_map(|q| [q.x_mm().to_bits(), q.y_mm().to_bits()]))).collect::<Vec<_>>()).collect()).collect::<Vec<_>>()
        }).collect()
    };
    let want = traced(&ev);
    assert!(want.iter().filter(|l| !l.is_empty()).count() > 50, "the vase is cut");
    let (layers, gcode) = settle_gcode(&ev);
    for grow in [[0.37, 0.81, 0.53, 0.29], [1.6, 0.05, 0.0, 2.25]] {
        let loose = looser(grow);
        let got = traced(&loose);
        let differ = (0..want.len()).filter(|&k| got.get(k) != Some(&want[k])).count();
        assert!(got.len() == want.len() && differ == 0, "the box grown by {grow:?} cuts {differ} of {} layers otherwise", want.len());
        let (l, g) = settle_gcode(&loose);
        assert!(l == layers && g == gcode, "the box grown by {grow:?} prints other G-code ({l} layers against {layers}, {} bytes against {})", g.len(), gcode.len());
    }
}

#[test]
fn a_lattice_settles_on_the_cores() {
    let ev = evaluate("use mod.fab.*\nscript(\"lattice_cube.cad.splash\", params: {lattice_size: 10mm lattice_period: 5mm})\nplace(\"lattice_cube\", object: \"lattice\", at: [150mm 150mm], name: \"Lattice cube\")\n",
        ("lattice_cube.cad.splash", include_str!("../../app/resources/lattice_cube.cad.splash")));
    settles_as_sliced(&ev, "lattice");
}

#[test]
fn a_links_part_settles_on_the_cores() {
    let chain = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet chain = m.links([[0 0 0] [20mm 0 0]], {size: 6mm bar: 0.8mm})\nobject(chain, name: \"Chain\")\n";
    let ev = evaluate("use mod.fab.*\nscript(\"chain.cad.splash\")\nplace(\"chain\", at: [150mm 150mm], name: \"Chain part\")\n", ("chain.cad.splash", chain));
    settles_as_sliced(&ev, "Links part");
}
