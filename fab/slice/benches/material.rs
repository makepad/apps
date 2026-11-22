//! The material bench: the demo parts whose material is a field of their script (the Voronoi
//! lattice cube, the dithered gradient vase, the marble block), each sliced as painted (its
//! inside in large cells), painted with an exact inside (`fab_paint_inside_exact`) and in one
//! colour (the same script without its `Mix` fill), at 0.2 mm layers on two filaments.
//! Per part and kind: the slice's wall time (the best of `runs`, default 3), the print time,
//! head swaps per layer (mean and most) and pieces per layer, in all and of the skin (walls and
//! the fills that show). `-- runs=N` sets the count.
//!
//! `cargo bench -p makepad-fab-slice --bench material` (timed work: on a quiet machine).
use fab_model::eval::{Evaluated, Evaluator};
use fab_model::FabModel;
use fab_plate::machine::Machine;
use makepad_fab_settings::profiles::{JobSetup, Preset};
use makepad_fab_slice::live::{slice_frames, Frame, PartField};
use std::time::Instant;

const VASE: &str = include_str!("../../app/resources/vase.cad.splash");
const LATTICE: &str = include_str!("../../app/resources/lattice_cube.cad.splash");
const MARBLE: &str = include_str!("../../app/resources/demos/marble_block/marble_block.cad.splash");
const VORONOI_PAINT: &str = include_str!("../../app/resources/voronoi_paint.splash");
const GRADIENT_PAINT: &str = include_str!("../../app/resources/gradient_paint.splash");

fn setup(exact: bool) -> JobSetup {
    let mut process = Preset::default();
    for (k, v) in [("layer_height", "0.2"), ("sparse_infill_density", "15%"), ("wall_loops", "2"), ("fab_paint_inside_exact", if exact { "1" } else { "0" })] {
        process.values.set_str(k, v);
    }
    JobSetup { machine: Preset::default(), process, filaments: vec![Preset::default(); 2], filament_map: vec![1, 2], nozzle_volume_type: vec!["Standard".into(); 2], bed_type: "Textured PEI Plate".into(), filament_map_mode: "Manual".into(), filament_colours: vec![None; 2] }
}

/// The script built, its first colour on tool 1 and the second on tool 2.
fn build(source: &str, second: &str) -> Evaluated {
    let m = FabModel::with_script("bench", "bench.cad.splash", source);
    let mut e = Evaluator::new(Machine::dual_nozzle_350());
    let mut ev = m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}")).1;
    let colors = ev.part_colors.clone();
    ev.plate.map_colors(&colors, &|name| Some(if name == second { (2, 1) } else { (1, 0) }));
    ev
}

fn frames(ev: &Evaluated) -> Vec<Frame> {
    let fr = &ev.frames[0];
    let field = ev.fields[0].as_ref().map(|pf| {
        let mut to = pf.to_printer;
        for (r, o) in fr.offset.iter().enumerate() {
            to[r][3] -= o;
        }
        PartField::new(pf.shape.clone(), to)
    });
    vec![Frame { local: fr.local.clone(), offset: fr.offset, field }]
}

/// Tool changes per layer from the G-code (`T<n>` lines after each layer change).
fn tool_changes(g: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for l in g.lines() {
        if l.starts_with("; CHANGE_LAYER") {
            out.push(0);
        } else if let Some(last) = out.last_mut() {
            let t = l.trim();
            if t.len() >= 2 && t.starts_with('T') && t[1..].chars().all(|c| c.is_ascii_digit()) {
                *last += 1;
            }
        }
    }
    out
}

fn main() {
    let runs: usize = std::env::args().find_map(|a| a.strip_prefix("runs=").and_then(|v| v.parse().ok())).unwrap_or(3);
    makepad_fab_toolpath::par::set_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4));
    let parts = [
        ("voronoi lattice cube", format!("{LATTICE}\n{VORONOI_PAINT}"), LATTICE.to_string(), "Yellow"),
        ("gradient vase", format!("{VASE}\n{GRADIENT_PAINT}"), VASE.to_string(), "Yellow"),
        ("marble block", MARBLE.to_string(), MARBLE.lines().filter(|l| !l.starts_with("m.fill(")).collect::<Vec<_>>().join("\n"), "White"),
    ];
    println!("part | kind | slice s (best of {runs}) | print s | swaps a layer (mean, most) | pieces a layer (skin)");
    for (name, painted, plain, second) in &parts {
        for (kind, source, exact) in [("painted", painted, false), ("painted, exact inside", painted, true), ("one colour", plain, false)] {
            let ev = build(source, second);
            let fr = frames(&ev);
            let mut best = f64::INFINITY;
            let mut result = None;
            for _ in 0..runs {
                let t = Instant::now();
                let res = slice_frames(&setup(exact), &ev.plate, &fr, &[], None).expect("slices");
                best = best.min(t.elapsed().as_secs_f64());
                result = Some(res);
            }
            let res = result.expect("one run");
            let tc = tool_changes(&res.gcode);
            let layers = tc.len().max(1) as f64;
            let islands = || res.layers().iter().flat_map(|l| &l.objects).flat_map(|o| &o.islands);
            let pieces: usize = islands().map(|i| i.walls.len() + i.fills.len()).sum();
            let skin: usize = islands().map(|i| i.walls.len() + i.fills.iter().filter(|f| f.role.shows()).count()).sum();
            println!(
                "{name} | {kind} | {best:.2} | {:.0} | {:.2}, {} | {:.0} ({:.0})",
                res.stats.total_time,
                tc.iter().sum::<usize>() as f64 / layers,
                tc.iter().max().copied().unwrap_or(0),
                pieces as f64 / layers,
                skin as f64 / layers
            );
        }
    }
}
