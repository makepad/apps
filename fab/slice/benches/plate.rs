//! The plate bench: the live engine's CPU path on the demo plate at 1, 2, 4, 8 and all threads
//! (`BENCH_THREADS=1,10` or `-- threads=1,10` picks counts). Per scenario: when the settled revision's toolpaths were
//! sent (the end of the edit path: a live edit asks no settle), settled wall, CPU, cores used, the settle's
//! phases (from its progress stages), CPU seconds per stage (from the plate's task spans and the
//! stage timers), trace ms per layer per part, the G-code hash (identical at every count), and
//! the GPU-EXEC §11.2 report of the window from the input to the settle (T, G, C/N, E, the idle
//! gaps, the critical path: [`makepad_fab_slice::bench_report`]).
//! Scenarios: the cold plate settle, a 10-step vase drag plus release and settle, a wavy cube
//! resize 10.3 -> 11.3 mm, a cold Voronoi vase (the heaviest kernel field), one parameter step per demo part timed to its viewed layer ([`viewed`]),
//! a sculpt stroke on the vase timed to its band ([`brush`]), and a mesh vase's top raised
//! ([`mesh_edit`]).
//! Every field is cut on the cores. `-- trace` instead counts the field tracer's work per layer
//! against what the contours need ([`trace_work`]); `-- guards=N` runs only the edit-path guards,
//! `N` times, for their medians ([`guards`]).
//!
//! `cargo bench -p makepad-fab-slice --bench plate` (timed work: on a quiet machine).
use fab_model::eval::Evaluated;
use fab_model::FabModel;
use fab_plate::machine::Machine;
use makepad_fab_settings::profiles::{JobSetup, Preset};
use makepad_fab_slice::bench_report;
use makepad_fab_slice::live::{Frame, LiveEngine, LiveEvent, LiveInput, PartField, Spawn, Urgency};
use makepad_fab_toolpath::{par, prof};
use std::collections::BTreeMap;
use std::sync::{atomic::{AtomicU64, Ordering}, mpsc, Arc, Mutex};
use std::time::Instant;

fn setup() -> JobSetup {
    let mut process = Preset::default();
    for (k, v) in [("wall_generator", "classic"), ("layer_height", "0.2"), ("sparse_infill_density", "15%"), ("sparse_infill_pattern", "gyroid"), ("wall_loops", "2"), ("support_type", "tree(auto)")] {
        process.values.set_str(k, v);
    }
    // The demo plate's two colours, one per nozzle (as the app's default materials).
    JobSetup { machine: Preset::default(), process, filaments: vec![Preset::default(); 2], filament_map: vec![1, 2], nozzle_volume_type: vec!["Standard".into(); 2], bed_type: "Textured PEI Plate".into(), filament_map_mode: "Manual".into(), filament_colours: vec![None; 2] }
}

fn scripts() -> std::collections::BTreeMap<String, String> {
    [
        ("bracket.cad.splash", include_str!("../../app/resources/bracket.cad.splash")),
        ("vase.cad.splash", include_str!("../../app/resources/vase.cad.splash")),
        ("lattice_cube.cad.splash", include_str!("../../app/resources/lattice_cube.cad.splash")),
        ("voronoi_vase.cad.splash", include_str!("../../app/resources/voronoi_vase.cad.splash")),
    ].into_iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

pub fn evaluate(project: &str) -> Evaluated {
    let m = FabModel::load("bench", project, scripts()).expect("the project loads");
    let mut e = fab_model::eval::Evaluator::new(Machine::dual_nozzle_350());
    m.evaluate(&mut e, None).expect("the plate builds").1
}

/// The engine's input as the app makes it, with every part's frame shifted by `dx[i]` mm.
pub fn input(id: u64, ev: &Evaluated, dx: &[f64]) -> LiveInput {
    let mut plate = ev.plate.printable();
    let frames: Vec<Frame> = ev.frames.iter().enumerate().map(|(i, f)| {
        let field = ev.fields.get(i).and_then(Option::as_ref).and_then(|pf| PartField::placed(pf.shape.clone(), pf.to_part).map(|f| f.with_bounds(pf.bounds.map(|b| (b.min, b.max)))));
        let d = dx.get(i).copied().unwrap_or(0.0);
        Frame { local: f.local.clone(), offset: [f.offset[0] + d, f.offset[1], f.offset[2]], field }
    }).collect();
    for (i, p) in plate.parts.iter_mut().enumerate() {
        let d = dx.get(i).copied().unwrap_or(0.0);
        if d != 0.0 { p.mesh.translate([d, 0.0, 0.0]); }
    }
    let mut i = LiveInput::ready(id, 1, setup(), Arc::new(plate), Arc::new(Vec::new()));
    i.frames = Some(Arc::new(frames));
    i.machine = ev.machine.clone();
    i
}

/// What the scenarios run on: the engines, their events and the devices' spans.
pub trait Host {
    /// A new engine, set up for this host; its events come from [`Host::next`] from now on.
    fn engine(&mut self) -> LiveEngine;
    /// The newest engine's next event and when it came.
    fn next(&mut self) -> (Instant, LiveEvent);
    /// Adds to the pool's `spans` the spans of the work off the pool since the last call (the
    /// GPU, its encoder, its readbacks: none on the cores), on their clock, and makes the pool's
    /// spans that waited on that work name it.
    fn device_spans(&mut self, _spans: &mut Vec<par::Span>) {}
}

type Events = mpsc::Receiver<(Instant, LiveEvent)>;

/// The CPU-only plate: engines on std threads, every field on the cores.
#[derive(Default)]
pub struct Cores {
    events: Option<Events>,
}

impl Host for Cores {
    fn engine(&mut self) -> LiveEngine {
        let (e, rx) = engine();
        self.events = Some(rx);
        e
    }
    /// Blocking: nothing else needs this thread.
    fn next(&mut self) -> (Instant, LiveEvent) {
        self.events.as_ref().expect("an engine").recv().expect("an engine event")
    }
}

fn engine() -> (LiveEngine, Events) {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let spawn: Spawn = Arc::new(|_, job| { std::thread::spawn(job); });
    let version = Arc::new(AtomicU64::new(0));
    let next: makepad_fab_slice::live::Next = Arc::new(move || {
        let mine = version.fetch_add(1, Ordering::AcqRel) + 1;
        let version = version.clone();
        Arc::new(move || version.load(Ordering::Acquire) != mine)
    });
    let engine = LiveEngine::new(spawn.clone(), Arc::new(move |e| { let _ = tx.lock().unwrap().send((Instant::now(), e)); }), next);
    // Continuations skip a stale input's work, as the app's pool tickets do.
    engine.set_resume(Arc::new(move |checkpoint| {
        let spawn = spawn.clone();
        Arc::new(move |urgency, job| {
            let checkpoint = checkpoint.clone();
            spawn(urgency, Box::new(move || { if urgency == Urgency::Viewed || !checkpoint() { job(); } }));
        })
    }));
    (engine, rx)
}

pub struct Run {
    layers: usize,
    gcode: u64,
    bytes: usize,
    /// (seconds from the start, what began): the settle's progress stages and the plate events.
    marks: Vec<(f64, String)>,
    /// When the settled revision's toolpaths were sent.
    toolpaths: f64,
    /// When the sliced printability finished streaming (after the settle).
    issues_done: f64,
}

fn hash(s: &str) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in s.bytes() { h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3); }
    h
}

/// Waits for settle `want`, then for its sliced printability (so nothing of it runs into the next scenario).
pub fn settled(host: &mut dyn Host, t0: Instant, want: u64) -> Run {
    let mut marks: Vec<(f64, String)> = Vec::new();
    let mut mark = |at: Instant, what: String| {
        if marks.last().is_none_or(|m| m.1 != what) { marks.push(((at - t0).as_secs_f64(), what)); }
    };
    let mut sent: Vec<(u64, f64)> = Vec::new();
    let (layers, gcode, bytes, revision) = loop {
        let (at, e) = host.next();
        match e {
            LiveEvent::Progress { tag, stage, .. } if tag == want => mark(at, format!("stage {stage}")),
            LiveEvent::Contours { .. } => mark(at, "contours".into()),
            LiveEvent::Toolpaths { revision, .. } => {
                sent.push((revision, (at - t0).as_secs_f64()));
                mark(at, "toolpaths".into());
            }
            LiveEvent::Settled { tag, result, revision, .. } if tag == want => {
                mark(at, "settled".into());
                let s = result.expect("sliced");
                break (s.stats.layers, hash(&s.gcode.text().expect("the settled G-code")), s.gcode.len(), revision);
            }
            LiveEvent::Failed { error, .. } => panic!("failed: {error}"),
            _ => {}
        }
    };
    let issues_done = loop {
        let (at, e) = host.next();
        if let LiveEvent::Issues { revision: r, done, total, .. } = e {
            if r == revision && done == total && total > 0 { break (at - t0).as_secs_f64(); }
        }
    };
    let toolpaths = sent.iter().rev().find(|s| s.0 == revision).expect("the settled revision's toolpaths").1;
    Run { layers, gcode, bytes, marks, toolpaths, issues_done }
}

/// The process's CPU seconds (user + system): `proc_usage` on Linux and Windows, `ps` elsewhere.
fn cpu_s() -> f64 {
    if let (_, Some(c)) = makepad_fab_slice::bench::proc_usage() { return c; }
    let out = std::process::Command::new("ps").args(["-o", "time=", "-p", &std::process::id().to_string()]).output().expect("ps");
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    t.split(':').fold(0.0, |acc, part| acc * 60.0 + part.parse::<f64>().unwrap_or(0.0))
}

/// CPU seconds per reported stage: the plate's task spans by stage, with the overhang split
/// (S3, in the paths tasks) and the narrow solid and concentric fills (S1b, in the infill tasks)
/// from the stage timers.
fn stage_table(spans: &[par::Span], stages: &[(prof::Stage, f64)]) -> Vec<(&'static str, f64)> {
    let mut by: BTreeMap<&str, f64> = BTreeMap::new();
    for s in spans {
        *by.entry(s.stage).or_default() += (s.end_ns - s.start_ns) as f64 * 1e-9;
    }
    let timer = |st: prof::Stage| stages.iter().find(|s| s.0 == st).map_or(0.0, |s| s.1);
    let sum = |names: &[&str]| names.iter().map(|n| by.get(n).copied().unwrap_or(0.0)).sum::<f64>();
    let (s3, s1b) = (timer(prof::Stage::Overhangs), timer(prof::Stage::InfillConcentric));
    let checks: f64 = [prof::Stage::CheckLower, prof::Stage::CheckThin, prof::Stage::CheckIslands, prof::Stage::CheckBridges, prof::Stage::CheckSupportArea, prof::Stage::CheckMoves].into_iter().map(timer).sum();
    vec![
        ("trace", sum(&["trace"])),
        ("classes", sum(&["faces", "skin", "shells", "bridges", "classes"])),
        ("walls", sum(&["walls"])),
        ("planner", sum(&["outline", "regions", "paths", "aux", "layer", "vase", "supports", "finish"]) - s3),
        ("S3", s3),
        ("S1b", s1b),
        ("infill", sum(&["infill"]) - s1b),
        ("merge", sum(&["merge"])),
        ("checks", checks),
        ("gcode", sum(&["gcode"])),
        ("package", sum(&["package", "layer timeline"])),
    ]
}

/// Runs one scenario (`run` from its start) and prints its BENCH lines and its §11.2 report.
pub fn timed(what: &str, threads: usize, names: &[String], host: &mut dyn Host, run: impl FnOnce(&mut dyn Host, Instant) -> Run) {
    prof::enable(true);
    let _ = prof::take();
    par::record_spans(true);
    let _ = par::take_spans();
    host.device_spans(&mut Vec::new());
    let (t, start_ns, c) = (Instant::now(), makepad_fab_slicer::par::now_ns(), cpu_s());
    let r = run(&mut *host, t);
    let wall = r.marks.iter().find(|m| m.1 == "settled").map_or(0.0, |m| m.0);
    let cpu = cpu_s() - c;
    let stages = prof::take();
    let spans = par::take_spans();
    par::record_spans(false);
    prof::enable(false);
    println!("BENCH {what} threads={threads} toolpaths_s={:.3} settled_s={wall:.3} issues_done_s={:.3} cpu_s={cpu:.2} cores={:.2} layers={} gcode={:016x} gcode_mb={:.1}", r.toolpaths, r.issues_done, cpu / r.issues_done.max(1e-9), r.layers, r.gcode, r.bytes as f64 / 1e6);
    let phases: Vec<String> = r.marks.windows(2).map(|w| format!("{}={:.3}", w[0].1, w[1].0 - w[0].0)).collect();
    println!("BENCH   phases {what} threads={threads} {}", phases.join(" | "));
    let table: Vec<String> = stage_table(&spans, &stages).into_iter().map(|(n, v)| format!("{n}={v:.3}")).collect();
    println!("BENCH   stage_cpu_s {what} threads={threads} {}", table.join(" | "));
    // Trace per part: ms per layer cut on the cores.
    let mut trace: BTreeMap<u32, (f64, usize)> = BTreeMap::new();
    for s in spans.iter().filter(|s| s.stage == "trace") {
        let e = trace.entry(s.part).or_default();
        e.0 += (s.end_ns - s.start_ns) as f64 * 1e-6;
        e.1 += 1;
    }
    let per: Vec<String> = trace.iter().map(|(p, (ms, n))| format!("{}={:.3} ms/layer ({n} layers)", names.get(*p as usize).map_or("?", String::as_str), ms / *n as f64)).collect();
    println!("BENCH   trace_per_part {what} threads={threads} {}", per.join(" | "));
    // The planner's tasks per part: CPU ms per planned layer (walls and infill run per island,
    // aux per block of layers; the class stages together).
    let stage_of = |st: &str| match st {
        "faces" | "skin" | "shells" | "bridges" | "classes" => Some("classes"),
        "walls" | "paths" | "aux" | "infill" | "layer" => Some(match st { "walls" => "walls", "paths" => "paths", "aux" => "aux", "infill" => "infill", _ => "layer" }),
        _ => None,
    };
    let mut tasks: BTreeMap<(u32, &str), f64> = BTreeMap::new();
    let mut planned: BTreeMap<u32, usize> = BTreeMap::new();
    for s in &spans {
        let Some(st) = stage_of(s.stage) else { continue };
        *tasks.entry((s.part, st)).or_default() += (s.end_ns - s.start_ns) as f64 * 1e-6;
        if st == "layer" {
            *planned.entry(s.part).or_default() += 1;
        }
    }
    let per: Vec<String> = tasks.iter().filter_map(|((p, st), ms)| {
        let n = *planned.get(p)?;
        Some(format!("{}.{st}={:.3}", names.get(*p as usize).map_or("?", String::as_str), ms / n as f64))
    }).collect();
    println!("BENCH   task_ms_per_layer {what} threads={threads} {}", per.join(" | "));
    let st: Vec<String> = stages.iter().filter(|s| s.1 > 0.0005).map(|(s, v)| format!("{}={:.3}", s.name().trim(), v)).collect();
    println!("BENCH   timers_s {what} threads={threads} {}", st.join(" | "));
    let mut all = spans;
    host.device_spans(&mut all);
    let settled_ns = start_ns + (wall * 1e9) as u64;
    print!("{}", bench_report::report_window(&all, threads, start_ns, settled_ns).named(&format!("{what} threads={threads}")));
}

/// The scenarios' plates: the demo plate, the wavy cube before and after its resize, and the
/// demo plate's parameter steps ([`viewed`]).
pub struct Plates {
    pub demo: Evaluated,
    wavy: (Evaluated, Evaluated),
    voronoi: Evaluated,
    vase: usize,
    names: Vec<String>,
    wavy_names: Vec<String>,
    voronoi_names: Vec<String>,
    /// One parameter step per part, each on top of the ones before: (the part, the plate).
    edits: Vec<(usize, Evaluated)>,
}

pub fn plates() -> Plates {
    let source = include_str!("../../app/resources/demo.fab.splash");
    let demo = evaluate(source);
    let wavy = |size: f64| evaluate(&format!("use mod.fab.*\nscript(\"lattice_cube.cad.splash\", params: {{lattice_size: {size}mm lattice_period: 5mm}})\nplace(\"lattice_cube\", object: \"lattice\", at: [80mm 165mm], name: \"Lattice cube\")\n"));
    let wavy = (wavy(10.3), wavy(11.3));
    let voronoi = evaluate("use mod.fab.*\nscript(\"voronoi_vase.cad.splash\")\nplace(\"voronoi_vase\", object: \"web\", at: [175mm 160mm])\n");
    let vase = demo.plate.parts.iter().position(|p| p.object.starts_with("vase")).expect("the vase");
    let names: Vec<String> = demo.plate.printable().parts.iter().map(|p| p.object.clone()).collect();
    let wavy_names = wavy.0.plate.printable().parts.iter().map(|p| p.object.clone()).collect();
    let voronoi_names = voronoi.plate.printable().parts.iter().map(|p| p.object.clone()).collect();
    let mut stepped = source.to_string();
    let edits = [("bracket", "bracket.cad.splash", "thickness: 6.5mm"), ("vase", "vase.cad.splash", "vase_belly: 22mm"), ("lattice", "lattice_cube.cad.splash", "lattice_period: 8.5mm")].into_iter().map(|(object, script, param)| {
        let call = format!("script(\"{script}\")");
        assert!(stepped.contains(&call), "the demo plate loads {script}");
        stepped = stepped.replace(&call, &format!("script(\"{script}\", params: {{{param}}})"));
        (names.iter().position(|n| n.starts_with(object)).expect("the edited part"), evaluate(&stepped))
    }).collect();
    Plates { demo, wavy, voronoi, vase, names, wavy_names, voronoi_names, edits }
}

/// The height the viewer looks at in [`viewed`]: one every demo part reaches.
const VIEW_Z: f64 = 10.0;

/// The edit path's first answer, the number editing feels: per part, one parameter step on the
/// settled demo plate with the viewer at [`VIEW_Z`], the milliseconds from the new input to that
/// part's viewed layer drawn ([`LiveEvent::Viewed`]: its focus band traced and planned), then to
/// the plate's toolpaths; the plate settles before the next step. The engine's part only: each
/// step's model is evaluated before it. Returns the viewed milliseconds per step.
pub fn viewed(host: &mut dyn Host, p: &Plates, threads: usize, suffix: &str) -> Vec<f64> {
    let e = host.engine();
    e.set_focus_z(Some(VIEW_Z));
    let t = Instant::now();
    e.settle(20, input(20, &p.demo, &[]));
    settled(host, t, 20);
    let (mut steps, mut ms) = (Vec::new(), Vec::new());
    for (n, (part, stepped)) in p.edits.iter().enumerate() {
        let tag = 21 + n as u64;
        let t = Instant::now();
        e.settle(tag, input(tag, stepped, &[]));
        let (at, layer, field_ms, slice_ms, plan_ms) = loop {
            match host.next() {
                (at, LiveEvent::Viewed { part: i, layer, field_ms, slice_ms, plan_ms, .. }) if i == *part => break (at, layer, field_ms, slice_ms, plan_ms),
                (_, LiveEvent::Failed { error, .. }) => panic!("failed: {error}"),
                _ => {}
            }
        };
        let r = settled(host, t, tag);
        ms.push((at - t).as_secs_f64() * 1e3);
        steps.push(format!("{}={:.1} ms (layer {layer}: field {field_ms:.1}, trace {slice_ms:.1}, plan {plan_ms:.1}; toolpaths {:.3} s)", p.names[*part], (at - t).as_secs_f64() * 1e3, r.toolpaths));
    }
    e.set_focus_z(None);
    println!("BENCH viewed{suffix} threads={threads} z={VIEW_Z} {}", steps.join(" | "));
    ms
}

/// The brush band, the number sculpting in the sliced view feels: one raise stroke on the vase's
/// outer wall at [`VIEW_Z`] on the settled demo plate, the view on the stroke. The milliseconds
/// from the new input to the vase's viewed layer and to the plate's toolpaths, the vase's layers
/// traced and planned again (of all of them: the stroke's band, widened by what reads across
/// layers), and the settled G-code against a fresh engine's slice of the stroked plate. Returns the
/// viewed milliseconds.
pub fn brush(host: &mut dyn Host, p: &Plates, threads: usize, suffix: &str) -> f64 {
    let (stroked, inside) = stroked(p, VIEW_Z);
    let part = p.names.iter().position(|n| n.starts_with("vase")).expect("the vase");
    let e = host.engine();
    e.set_focus_z(Some(VIEW_Z));
    let t = Instant::now();
    e.settle(40, input(40, &p.demo, &[]));
    settled(host, t, 40);
    let work = |e: &LiveEngine| e.part_work().get(part).copied().unwrap_or((0, 0));
    let before = work(&e);
    let t = Instant::now();
    e.settle(41, input(41, &stroked, &[]));
    let (at, layer, field_ms, slice_ms, plan_ms) = loop {
        match host.next() {
            (at, LiveEvent::Viewed { part: i, layer, field_ms, slice_ms, plan_ms, .. }) if i == part => break (at, layer, field_ms, slice_ms, plan_ms),
            (_, LiveEvent::Failed { error, .. }) => panic!("failed: {error}"),
            _ => {}
        }
    };
    let r = settled(host, t, 41);
    let after = work(&e);
    e.set_focus_z(None);
    let fresh = host.engine();
    let t0 = Instant::now();
    fresh.settle(42, input(42, &stroked, &[]));
    let f = settled(host, t0, 42);
    let all = fresh.part_work().get(part).copied().unwrap_or((0, 0));
    println!(
        "BENCH brush{suffix} threads={threads} z={VIEW_Z} vase raise stroke ({}): viewed {:.1} ms (layer {layer}: field {field_ms:.1}, trace {slice_ms:.1}, plan {plan_ms:.1}), toolpaths {:.3} s, issues done {:.3} s; traced {} of {} layers, planned {} of {}; settled {} the fresh slice ({:016x})",
        if inside { "inside the part's box" } else { "past the part's box" },
        (at - t).as_secs_f64() * 1e3, r.toolpaths, r.issues_done, after.0 - before.0, all.0, after.1 - before.1, all.1,
        if r.gcode == f.gcode { "equals" } else { "DIFFERS from" }, r.gcode,
    );
    (at - t).as_secs_f64() * 1e3
}

/// A mesh part for [`mesh_edit`]: a vase turned from r(z) = 20 + 6 sin(z / 9) mm over z 0..60 mm
/// (96 segments, rings every 0.5 mm, both ends capped), at (175, 160) on the bed.
fn turned_vase() -> fab_plate::Part {
    let (s, k) = (96u32, 121u32);
    let mut positions = Vec::new();
    for ring in 0..k {
        let z = ring as f64 * 0.5;
        let r = 20.0 + 6.0 * (z / 9.0).sin();
        for j in 0..s {
            let a = std::f64::consts::TAU * j as f64 / s as f64;
            positions.push([175.0 + r * a.cos(), 160.0 + r * a.sin(), z]);
        }
    }
    positions.push([175.0, 160.0, 0.0]);
    positions.push([175.0, 160.0, (k - 1) as f64 * 0.5]);
    let (bottom, top) = (k * s, k * s + 1);
    let mut triangles = Vec::new();
    for ring in 0..k - 1 {
        for j in 0..s {
            let (a, b) = (ring * s + j, ring * s + (j + 1) % s);
            triangles.push([a, b, b + s]);
            triangles.push([a, b + s, a + s]);
        }
    }
    for j in 0..s {
        triangles.push([bottom, (j + 1) % s, j]);
        triangles.push([top, (k - 1) * s + j, (k - 1) * s + (j + 1) % s]);
    }
    fab_plate::Part { body_of: None, object: "vase".into(), name: "Turned vase".into(), filament: 1, color: [1.0; 4], nozzle: None,
        mesh: fab_plate::PartMesh { positions, triangles }, field_bounds: None, field_floor: None, print: Vec::new(), print_mesh: None, paint: None, ..Default::default() }
}

/// The mesh edit, the number an edit of a mesh part feels: the turned vase alone on the plate, its
/// top fifth raised 5 mm (its vertices moved, its triangles kept), the view on the raised part.
/// The milliseconds from the new input to the vase's viewed layer and to its toolpaths, its layers
/// cut and planned again, and the settled G-code against a fresh engine's slice of the edited
/// plate. Returns the viewed milliseconds.
pub fn mesh_edit(host: &mut dyn Host, threads: usize, suffix: &str) -> f64 {
    let plate = fab_plate::Plate { parts: vec![turned_vase()], ..Default::default() };
    let (lo, hi) = plate.parts[0].mesh.positions.iter().fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(v[2]), hi.max(v[2])));
    let from = lo + 0.8 * (hi - lo);
    let mut edited = plate.clone();
    for v in &mut edited.parts[0].mesh.positions {
        if v[2] > from {
            v[2] += 5.0;
        }
    }
    let mesh_input = |id: u64, p: &fab_plate::Plate| LiveInput::ready(id, 1, setup(), Arc::new(p.clone()), Arc::new(Vec::new()));
    let e = host.engine();
    e.set_focus_z(Some((from + hi + 5.0) / 2.0));
    let t = Instant::now();
    e.settle(50, mesh_input(50, &plate));
    settled(host, t, 50);
    let (cut0, planned0) = (e.traced_count(), e.layers_computed());
    let t = Instant::now();
    e.settle(51, mesh_input(51, &edited));
    let (at, layer, slice_ms, plan_ms) = loop {
        match host.next() {
            (at, LiveEvent::Viewed { part: 0, layer, slice_ms, plan_ms, .. }) => break (at, layer, slice_ms, plan_ms),
            (_, LiveEvent::Failed { error, .. }) => panic!("failed: {error}"),
            _ => {}
        }
    };
    let r = settled(host, t, 51);
    let (cut, planned) = (e.traced_count() - cut0, e.layers_computed() - planned0);
    e.set_focus_z(None);
    let fresh = host.engine();
    let t0 = Instant::now();
    fresh.settle(52, mesh_input(52, &edited));
    let f = settled(host, t0, 52);
    println!(
        "BENCH mesh_edit{suffix} threads={threads} vase mesh top fifth +5 mm: viewed {:.1} ms (layer {layer}: cut {slice_ms:.1}, plan {plan_ms:.1}), toolpaths {:.3} s, issues done {:.3} s; cut {cut}, planned {planned} of {} layers; settled {} the fresh slice ({:016x})",
        (at - t).as_secs_f64() * 1e3, r.toolpaths, r.issues_done, r.layers,
        if r.gcode == f.gcode { "equals" } else { "DIFFERS from" }, r.gcode,
    );
    (at - t).as_secs_f64() * 1e3
}

/// `-- guards=N`: the edit-path guards ([`viewed`], [`brush`], [`mesh_edit`]) `N` times on the warm
/// plate at the picked thread count (all by default), then each one's median and minimum: one run
/// of a single layer's latency is too noisy to compare two trees by.
pub fn guards(host: &mut dyn Host, p: &Plates, threads: usize, runs: usize) {
    let mut all: Vec<Vec<f64>> = Vec::new();
    for k in 0..runs {
        let mut ms = viewed(host, p, threads, &format!(" #{k}"));
        ms.push(brush(host, p, threads, &format!(" #{k}")));
        ms.push(mesh_edit(host, threads, &format!(" #{k}")));
        all.push(ms);
    }
    let names: Vec<String> = p.edits.iter().map(|(part, _)| p.names[*part].clone()).chain(["brush".to_string(), "mesh".to_string()]).collect();
    let cols: Vec<String> = names.iter().enumerate().map(|(i, name)| {
        let mut v: Vec<f64> = all.iter().map(|ms| ms[i]).collect();
        v.sort_by(f64::total_cmp);
        format!("{name}={:.1} ({:.1})", v[v.len() / 2], v[0])
    }).collect();
    println!("BENCH guards threads={threads} runs={runs} median (min) ms: {}", cols.join(" | "));
}

/// The demo plate with a raise stroke drawn on the vase's outer wall at `z` (plate mm), sculpted as
/// the app sculpts a field part ([`makepad_fab_slice::makepad_csg_sdf::SdfShape::sculpted`]).
/// The layout's box is kept; also returns whether the stroke's tubes stay inside it.
fn stroked(p: &Plates, z: f64) -> (Evaluated, bool) {
    use makepad_fab_slice::makepad_csg_sdf::strokes::{StrokeOp, Strokes};
    let mut ev = p.demo.clone();
    let lift = ev.frames[p.vase].offset[2];
    let pf = ev.fields[p.vase].as_mut().expect("the vase is a field part");
    let m = pf.to_part;
    let b = pf.bounds.as_ref().expect("the vase's box");
    let f = PartField::placed(pf.shape.clone(), m).expect("a rigid placement").field().expect("the vase's field");
    let (zf, cy) = (z - lift, (b.min[1] + b.max[1]) * 0.5);
    // The outer wall: the last inside-to-outside crossing walking out along +x.
    let (step, cx) = (0.01, (b.min[0] + b.max[0]) * 0.5);
    let x = (0..((b.max[0] - cx) / step) as usize).map(|s| cx + s as f64 * step).filter(|&x| f(x, cy, zf) < 0.0 && f(x + step, cy, zf) >= 0.0).last().expect("the vase's wall at z") + 0.5 * step;
    // The part frame into the shape's (a rigid map with a uniform scale).
    let s2 = m[0][0] * m[0][0] + m[1][0] * m[1][0] + m[2][0] * m[2][0];
    let model = |q: [f64; 3]| -> [f64; 3] {
        let d = [q[0] - m[0][3], q[1] - m[1][3], q[2] - m[2][3]];
        std::array::from_fn(|c| (m[0][c] * d[0] + m[1][c] * d[1] + m[2][c] * d[2]) / s2)
    };
    let mut strokes = Strokes::new();
    let points: Vec<[f64; 3]> = (0..5).map(|k| model([x, cy - 1.0 + 0.5 * k as f64, zf])).collect();
    strokes.push(StrokeOp::Raise, 2.0, 0.5, &points);
    let (lo, hi) = strokes.raised_bounds().expect("a raising stroke");
    let inside = (0..8).all(|c| {
        let q = [if c & 1 == 0 { lo[0] } else { hi[0] }, if c & 2 == 0 { lo[1] } else { hi[1] }, if c & 4 == 0 { lo[2] } else { hi[2] }];
        (0..3).all(|r| {
            let v = m[r][0] * q[0] + m[r][1] * q[1] + m[r][2] * q[2] + m[r][3];
            v >= b.min[r] && v <= b.max[r]
        })
    });
    pf.shape = Arc::new((*pf.shape).clone().sculpted(Arc::new(strokes)));
    (ev, inside)
}

/// The demo plate settled once, untimed: every code path warm (the JIT's, a GPU's pipelines).
pub fn warm(host: &mut dyn Host, p: &Plates, label: &str) {
    let e = host.engine();
    let t = Instant::now();
    e.settle(100, input(100, &p.demo, &[]));
    settled(host, t, 100);
    println!("BENCH warm demo plate {label} {:.3} s", t.elapsed().as_secs_f64());
}

/// The three scenarios on `host` at `threads`, each named `<scenario><suffix>`.
pub fn scenarios(host: &mut dyn Host, p: &Plates, threads: usize, suffix: &str) {
    let e = host.engine();
    timed(&format!("demo_plate{suffix}"), threads, &p.names, host, |h, t| { e.settle(1, input(1, &p.demo, &[])); settled(h, t, 1) });
    let mut dx = vec![0.0; p.demo.plate.parts.len()];
    timed(&format!("drag_vase_10mm{suffix}"), threads, &p.names, host, |h, t| {
        for k in 1..=10 {
            dx[p.vase] = k as f64;
            e.set_transform(p.vase, [[1.0, 0.0, 0.0, k as f64], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]]);
        }
        e.release();
        e.settle(2, input(2, &p.demo, &dx));
        settled(h, t, 2)
    });
    let e = host.engine();
    let t = Instant::now();
    e.settle(3, input(3, &p.wavy.0, &[]));
    settled(host, t, 3);
    timed(&format!("wavy_resize_10.3_to_11.3{suffix}"), threads, &p.wavy_names, host, |h, t| { e.settle(4, input(4, &p.wavy.1, &[])); settled(h, t, 4) });
    let e = host.engine();
    timed(&format!("voronoi_vase{suffix}"), threads, &p.voronoi_names, host, |h, t| { e.settle(5, input(5, &p.voronoi, &[])); settled(h, t, 5) });
    viewed(host, p, threads, suffix);
    brush(host, p, threads, suffix);
    mesh_edit(host, threads, suffix);
}

/// `trace` (`-- trace`): the field tracer's work per layer of each field part, on one thread,
/// against what its contours need. Field samples by step: the coarse search, the kept cells' fine
/// nodes, the crossing refinement (all rounds), the crossings' gradients, and the curved spans'
/// projections with the single points (saddles, creases); the sign-changing fine edges, one per crossing of the
/// contour (what the output needs: about a node and a few refinement samples each); the time
/// outside the field (the tracer's own work). The counts sample the part's whole field (no slab
/// cull, as the engine's): the time in it is not the engine's, the time outside it is. Then the
/// engine's own trace of every layer (slab culled, regions built) on one thread. Layers are
/// 0.2 mm, cut at their middles over the part's box.
pub fn trace_work(demo: &Evaluated) {
    use makepad_fab_slicer::sdf::{Field3, Field3Many, SdfSource};
    par::set_threads(1);
    let i = input(0, demo, &[]);
    let frames = i.frames.as_deref().expect("the plate's frames");
    #[derive(Default)]
    struct Tally {
        batches: Vec<usize>,
        singles: u64,
        eval_ns: u64,
    }
    for (k, frame) in frames.iter().enumerate() {
        let Some(pf) = frame.field.as_ref() else { continue };
        let engine = pf.source().expect("a field source");
        let bounds = engine.bounds();
        let f = pf.field().expect("a rigid field");
        let tally = Arc::new(Mutex::new(Tally::default()));
        let (g, c) = (f.clone(), tally.clone());
        let field: Field3 = Arc::new(move |x, y, z| {
            let t = Instant::now();
            let v = g(x, y, z);
            let mut c = c.lock().unwrap();
            c.singles += 1;
            c.eval_ns += t.elapsed().as_nanos() as u64;
            v
        });
        let (g, c) = (f.clone(), tally.clone());
        let many: Field3Many = Arc::new(move |pts: &[[f64; 3]], reach: f64, out: &mut [f64]| {
            let t = Instant::now();
            for (v, q) in out.iter_mut().zip(pts) {
                *v = g(q[0], q[1], q[2]).clamp(-reach, reach);
            }
            let mut c = c.lock().unwrap();
            c.batches.push(pts.len());
            c.eval_ns += t.elapsed().as_nanos() as u64;
        });
        let mut src = SdfSource::new(field, bounds).with_many(many);
        (src.params.lipschitz, src.params.window) = pf.shape.clearance().at(src.params.reach());
        let zs: Vec<f64> = (0..((bounds.1[2] - bounds.0[2]) / 0.2) as usize).map(|j| bounds.0[2] + 0.2 * (j as f64 + 0.5)).collect();
        // coarse nodes, fine nodes, crossings, refinement samples, gradient samples, projection and single points, refinement rounds
        let mut n = [0u64; 7];
        let (mut eval_ns, mut all_ns) = (0u64, 0u64);
        for &z in &zs {
            *tally.lock().unwrap() = Tally::default();
            let t = Instant::now();
            std::hint::black_box(src.segments(z));
            all_ns += t.elapsed().as_nanos() as u64;
            let c = tally.lock().unwrap();
            let b = &c.batches;
            n[0] += b.first().copied().unwrap_or(0) as u64;
            // The node batches, then the refinement rounds (all crossings first, never growing),
            // then the gradients (4 a crossing), then the batched projections of curved spans.
            let refine = (2..b.len()).find_map(|r| (r + 1..b.len()).find(|&g| b[g] == 4 * b[r] && b[r..g].windows(2).all(|w| w[1] <= w[0])).map(|g| (r, g)));
            let (r, g) = refine.unwrap_or((b.len(), b.len()));
            n[1] += b.get(1..r).map_or(0, |x| x.iter().sum::<usize>()) as u64;
            if g < b.len() {
                n[2] += b[r] as u64;
                n[3] += b[r..g].iter().sum::<usize>() as u64;
                n[4] += b[g] as u64;
                n[6] += (g - r) as u64;
                n[5] += b[g + 1..].iter().sum::<usize>() as u64;
            }
            n[5] += c.singles;
            eval_ns += c.eval_ns;
        }
        let t = Instant::now();
        for &z in &zs {
            std::hint::black_box(engine.contours(&[z]));
        }
        let engine_ms = t.elapsed().as_secs_f64() * 1e3 / zs.len() as f64;
        let per = |v: u64| v as f64 / zs.len() as f64;
        let samples = n[0] + n[1] + n[3] + n[4] + n[5];
        println!("BENCH trace_work {} layers={} lipschitz={:.2} per layer: coarse={:.0} fine_nodes={:.0} crossings={:.0} refine={:.0} ({:.1} rounds, {:.1} per crossing) gradients={:.0} projections+singles={:.0} samples={:.0} | fine nodes per crossing={:.1} | outside the field {:.2} ms/layer of {:.2} | engine trace {engine_ms:.2} ms/layer, {:.0} ns per sample",
            i.plate.parts[k].object, zs.len(), src.params.lipschitz, per(n[0]), per(n[1]), per(n[2]), per(n[3]), per(n[6]), n[3] as f64 / n[2].max(1) as f64, per(n[4]), per(n[5]), per(samples),
            n[1] as f64 / n[2].max(1) as f64, (all_ns - eval_ns) as f64 * 1e-6 / zs.len() as f64, all_ns as f64 * 1e-6 / zs.len() as f64, engine_ms * 1e6 / per(samples).max(1.0));
    }
}

fn main() {
    if std::env::args().any(|a| a == "trace") {
        trace_work(&evaluate(include_str!("../../app/resources/demo.fab.splash")));
        return;
    }
    let all = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let picked = std::env::args().find_map(|a| a.strip_prefix("threads=").map(str::to_string)).or_else(|| std::env::var("BENCH_THREADS").ok());
    let counts: Vec<usize> = picked.map(|s| s.split(',').filter_map(|x| x.trim().parse().ok()).collect())
        .unwrap_or_else(|| [1, 2, 4, 8, all].into_iter().filter(|&t| t <= all).collect::<std::collections::BTreeSet<_>>().into_iter().collect());
    println!("BENCH machine threads={all} counts={counts:?}");
    let p = plates();
    let mut host = Cores::default();
    par::set_threads(all);
    warm(&mut host, &p, "cores");
    if let Some(runs) = std::env::args().find_map(|a| a.strip_prefix("guards=").and_then(|n| n.parse().ok())) {
        let threads = counts.last().copied().unwrap_or(all);
        par::set_threads(threads);
        guards(&mut host, &p, threads, runs);
        return;
    }
    for &threads in &counts {
        par::set_threads(threads);
        scenarios(&mut host, &p, threads, "");
    }
}
