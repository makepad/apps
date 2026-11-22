//! Model evaluation benchmark: a cold build of a project (`.fab.splash`) or of one object
//! script (`.cad.splash`), then a list of edits applied one after another on the same
//! evaluator (what the app's model worker does), each timed from the batch to the
//! evaluated plate.
//!
//! `cargo run --release -p fab-model --example eval_bench -- <project.fab.splash | object.cad.splash> [edits.tsv] [--runs N]`
//!
//! The edits file has one batch per line: `label<TAB>site=value; site=value ...`,
//! each a value site of the first script (a param's name or `solid.field[.i]`) set to
//! Splash text (`80mm`).

use fab_model::eval::{Evaluated, Evaluator, PartChange};
use fab_model::program::ScriptEnv;
use fab_model::{project, Change, Edit, FabModel};
use fab_plate::machine::Machine;
use std::time::Instant;

fn summary(ev: &Evaluated) -> String {
    let phases: Vec<String> = ev.phases.iter().map(|(n, ms)| format!("{n} {ms:.1}")).collect();
    let changes: Vec<String> = ev
        .changes
        .iter()
        .zip(&ev.plate.parts)
        .map(|(c, p)| match c {
            PartChange::Reshaped(r) => format!(
                "{}: z {:.2}..{:.2} xy {:.1}x{:.1}",
                p.object,
                r.z.0,
                r.z.1,
                r.xy.max[0] - r.xy.min[0],
                r.xy.max[1] - r.xy.min[1]
            ),
            other => format!("{}: {other:?}", p.object),
        })
        .collect();
    let parts = ev.plate
        .parts
        .iter()
        .map(|p| format!("{}: {} tris, {:.3} mm3, open {}", p.object, p.mesh.triangles.len(), p.mesh.volume(), p.mesh.open_edges()))
        .collect::<Vec<_>>()
        .join("; ");
    format!("{parts}\n    phases ms: {}\n    changed: {}", phases.join(", "), changes.join("; "))
}

/// `FAB_BENCH_DIFF=1`: how many triangles differ between the two local
/// meshes of each part, with their z spread (checks the changed region).
fn diff_report(old: &Evaluated, new: &Evaluated) {
    use std::collections::HashSet;
    for (part, frame) in new.plate.parts.iter().zip(&new.frames) {
        let Some(i) = old.plate.parts.iter().position(|p| p.object == part.object) else { continue };
        let key = |m: &fab_plate::PartMesh, t: [u32; 3]| -> Vec<u64> {
            let mut v: Vec<[u64; 3]> = t.iter().map(|&i| m.positions[i as usize].map(f64::to_bits)).collect();
            v.sort();
            v.concat()
        };
        let a: HashSet<Vec<u64>> = old.frames[i].local.triangles.iter().map(|&t| key(&old.frames[i].local, t)).collect();
        let b: HashSet<Vec<u64>> = frame.local.triangles.iter().map(|&t| key(&frame.local, t)).collect();
        let mut zs: Vec<f64> = a.symmetric_difference(&b).map(|k| (0..3).map(|c| f64::from_bits(k[c * 3 + 2])).fold(f64::MIN, f64::max)).collect();
        zs.sort_by(|x, y| x.partial_cmp(y).unwrap());
        let n = zs.len();
        if n > 0 {
            println!("    diff {}: {} of {}/{} tris, top z quantiles {:.2} {:.2} {:.2} {:.2} {:.2}", part.object, n, a.len(), b.len(), zs[0], zs[n / 4], zs[n / 2], zs[3 * n / 4], zs[n - 1]);
        }
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut runs = 1usize;
    let mut paths = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--runs" {
            runs = args[i + 1].parse().expect("--runs N");
            i += 2;
        } else {
            paths.push(args[i].clone());
            i += 1;
        }
    }
    let program = paths.first().expect("usage: eval_bench <project.fab.splash | object.cad.splash> [edits.tsv] [--runs N]");
    let path = std::path::Path::new(program);
    let open = || -> FabModel {
        if project::is_script(path) {
            let text = std::fs::read_to_string(path).expect("script");
            FabModel::with_script("bench", program, &text)
        } else {
            project::load(path, &ScriptEnv::default()).unwrap_or_else(|e| panic!("{e}"))
        }
    };
    let file = open().state().scripts.keys().next().cloned().expect("a script");
    let edits: Vec<(String, Vec<Change>)> = match paths.get(1) {
        None => Vec::new(),
        Some(p) => std::fs::read_to_string(p)
            .expect("edits")
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                let (label, sets) = l.split_once('\t').expect("label<TAB>site=value; ...");
                let batch = sets
                    .split(';')
                    .filter(|x| !x.trim().is_empty())
                    .map(|x| {
                        let (site, value) = x.split_once('=').expect("site=value");
                        Change::Script { file: file.clone(), edit: Edit::Set { site: site.trim().to_string(), value: value.trim().to_string() } }
                    })
                    .collect();
                (label.to_string(), batch)
            })
            .collect(),
    };

    let mut cold = Vec::new();
    let mut edit_ms: Vec<Vec<f64>> = vec![Vec::new(); edits.len()];
    for run in 0..runs {
        let mut model = open();
        let mut eval = Evaluator::new(Machine::dual_nozzle_350());
        let t = Instant::now();
        let (c, ev) = model.evaluate(&mut eval, None).unwrap_or_else(|f| panic!("{f}"));
        let mut last = ev.clone();
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        cold.push(ms);
        if run == 0 {
            println!("cold build: {ms:.1} ms, {} solids; {}", c.instances.iter().map(|i| i.script.program.solids.len()).sum::<usize>(), summary(&ev));
        }
        for (k, (label, ops)) in edits.iter().enumerate() {
            let head = model.head();
            let t = Instant::now();
            let (_, ev) = model.apply(&mut eval, &format!("bench-{run}-{k}"), head, ops, None).unwrap_or_else(|e| panic!("{label}: {e}"));
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            edit_ms[k].push(ms);
            let (_, ev) = ev.unwrap();
            if run == 0 {
                println!("edit '{label}': {ms:.1} ms; {}", summary(&ev));
                if std::env::var_os("FAB_BENCH_DIFF").is_some() {
                    diff_report(&last, &ev);
                }
            }
            last = ev;
        }
    }
    if runs > 1 {
        println!("median of {runs}: cold {:.1} ms", median(cold));
        for (k, (label, _)) in edits.iter().enumerate() {
            println!("median of {runs}: edit '{label}' {:.1} ms", median(edit_ms[k].clone()));
        }
    }
}
