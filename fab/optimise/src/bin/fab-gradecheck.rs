//! `fab-gradecheck [coarse_mm fine_mm [fillet_mm]]`: a Voronoi web graded by stress against a uniform web
//! of the same mass, both solved for real. A cantilever bar (60 x 12 x 12 mm, held at one end,
//! 30 N down at the other) as `m.voronoi` with a foam core and a solid grip at each end, so the
//! hold and the load spread over a whole face and no strut end alone takes them; the graded
//! web's struts go from 1.6 mm to 3.2 mm by the solid bar's stress, the uniform web's strut is
//! bisected until its plastic equals the graded web's. Each web is solved at two cell sizes
//! (default 0.5 and 0.35 mm), and its safety factor away from the fixture is read with the two
//! sizes' verdict. It passes when both verdicts are converged and the graded web's safety factor
//! is at least the uniform web's; a safety factor the cell sizes disagree on is not a pass.
//! `fillet_mm` (default 0.5) is the blend where the web meets the grips.

use fab_force::bc::Kind;
use fab_force::material::{Materials, Print, FILAMENTS};
use fab_force::results::Verdict;
use fab_force::study::{Load, Model, Spot};
use fab_force::surface::Pick;
use fab_force::V3;
use fab_optimise::grade;
use makepad_model_script::lang::csg_graph::Op;
use makepad_model_script::lang::csg_sdf::{self, SdfGrid, SdfNode};
use makepad_model_script::lang::{evaluate, Assets, Options, RootFn};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

const ROOTS: &[RootFn] = &[RootFn { name: "object", args: &["solid", "name"], model: false }];
const BAR: [f64; 3] = [60.0, 12.0, 12.0];
/// How long the solid grips at the bar's ends are (mm), inside its length.
const GRIP: f64 = 3.0;

/// The web `m.voronoi(bar, {opts})`'s field (model mm), and its graded part's ask.
fn web(opts: &str, fillet: f64, stress: HashMap<String, Arc<SdfGrid>>) -> (SdfNode, Option<makepad_model_script::lang::StressRequest>) {
    let grip = |x: f64| format!("m.box({{size: [{GRIP}mm {}mm {}mm] at: [{x}mm, 0, 0]}})", BAR[1], BAR[2]);
    let end = 0.5 * (BAR[0] - GRIP);
    let src = format!("use mod.fab.*\nlet m = mod.model\nlet bar = m.box({{size: [{}mm {}mm {}mm]}})\nlet v = m.voronoi(bar, {{size: 6mm core: @foam {opts}}})\nlet part = m.blend([v {} {}], {fillet}mm)\nobject(part)\n", BAR[0], BAR[1], BAR[2], grip(-end), grip(end));
    let assets = Assets::new();
    let p = evaluate(&src, &Options { generated: &Default::default(), assets: &assets, images: &Default::default(), extension: "fab", libraries: &[], inputs: &makepad_model_script::lang::NO_INPUTS, unit: 1000.0, roots: ROOTS, overrides: HashMap::new(), weave: None, solved: stress }).unwrap_or_else(|e| panic!("{e:?}"));
    let Op::Field { shape, .. } = &p.graph.node(p.solid("part").unwrap().node).op else { panic!("the web is a field") };
    (shape.root.clone(), p.stress_requests.first().cloned())
}

fn loads() -> Vec<Load> {
    let x = 0.5 * BAR[0];
    vec![
        Load::new(Kind::Fixed, Spot::Seed { pick: Pick::Face, at: [-x, 0.0, 0.0] }),
        Load::new(Kind::Force([0.0, -30.0, 0.0]), Spot::Seed { pick: Pick::Face, at: [x, 0.0, 0.0] }),
    ]
}

fn model(root: &SdfNode, h: f64, m: &Materials) -> Model {
    let lo: V3 = std::array::from_fn(|a| -0.5 * BAR[a] - h);
    let hi: V3 = std::array::from_fn(|a| 0.5 * BAR[a] + h);
    let mut distance = |ps: &[V3], out: &mut [f64]| {
        for (p, o) in ps.iter().zip(out.iter_mut()) {
            *o = csg_sdf::distance(root, makepad_csg_math::Vec3d::new(p[0], p[1], p[2]));
        }
    };
    Model::new(lo, hi, h, &mut distance, m)
}

/// Plastic (mm3, solid) at cells of `h`.
fn plastic(root: &SdfNode, h: f64, m: &Materials) -> f64 {
    fab_force::opt::mass::plastic(&model(root, h, m).grid, &Materials::new(m.filament, &Print::default(), false))
}

/// The lowest safety factor away from the fixture at each cell size, and their verdict.
fn safety(name: &str, root: &SdfNode, hs: [f64; 2], m: &Materials) -> Verdict {
    let mut sf = Vec::new();
    for h in hs {
        let t = Instant::now();
        let model = model(root, h, m);
        let conditions = model.conditions(&loads());
        let out = model.solve(m, &conditions, &fab_force::solve::Options::default(), &mut |_, _| false).unwrap_or_else(|e| panic!("{name} at {h} mm: {e}"));
        let v = out.results.min_sf.0;
        // where the weakest cell is, for a safety factor the cell sizes disagree on
        let at = model.grid.cell_center(out.results.min_sf.1);
        println!("{name:<8} weakest cell at [{:.1}, {:.1}, {:.1}] mm", at[0], at[1], at[2]);
        println!("{name:<8} h {h:.2} mm  {:>8} cells  SF {v:.3}  max von Mises {:.2} MPa  residual {:.1e}  {:.1} s", model.grid.cells(), out.results.max_vm.0, out.solution.residual, t.elapsed().as_secs_f64());
        sf.push(v);
    }
    Verdict::of(sf[1], Some(sf[0]), None)
}

fn main() {
    let args: Vec<f64> = std::env::args().skip(1).filter_map(|a| a.parse().ok()).collect();
    let hs = if args.len() >= 2 { [args[0], args[1]] } else { [0.5, 0.35] };
    let fillet = args.get(2).copied().unwrap_or(0.5);
    let m = Materials::new(FILAMENTS[0], &Print::default(), false);
    // the graded web: the solid bar's stress, then the web it grades
    let graded_opts = "strut: 1.6mm strut_max: 3.2mm adapt: @stress";
    let (plain, request) = web(graded_opts, fillet, HashMap::new());
    let request = request.expect("the graded web asks for its stress");
    let identity = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
    let solved = grade::stress_grid(&plain, identity, &request, &loads(), fab_force::study::FIRST_CELLS).unwrap();
    println!("stress: reference {:.2} MPa at {:.2} mm cells", solved.reference, solved.h);
    let (graded, _) = web(graded_opts, fillet, HashMap::from([("v".to_string(), Arc::new(solved.grid))]));
    let target = plastic(&graded, hs[1], &m);
    // the uniform strut of the same plastic
    let (mut lo, mut hi) = (1.6f64, 3.2f64);
    for _ in 0..7 {
        let s = 0.5 * (lo + hi);
        if plastic(&web(&format!("strut: {s:.4}mm"), fillet, HashMap::new()).0, hs[1], &m) < target { lo = s } else { hi = s }
    }
    let strut = 0.5 * (lo + hi);
    let uniform = web(&format!("strut: {strut:.4}mm"), fillet, HashMap::new()).0;
    println!("plastic: graded {target:.0} mm3, uniform {:.0} mm3 at a {strut:.3} mm strut", plastic(&uniform, hs[1], &m));
    let g = safety("graded", &graded, hs, &m);
    let u = safety("uniform", &uniform, hs, &m);
    println!("verdicts: graded {g:?}, uniform {u:?}");
    let pass = matches!((g, u), (Verdict::Converged { fine: a, .. }, Verdict::Converged { fine: b, .. }) if a >= b);
    println!("{}", if pass { "PASS: the graded web is at least as safe at the same plastic" } else { "NOT A PASS (a safety factor below the uniform web's, or not converged)" });
    std::process::exit(if pass { 0 } else { 1 });
}
