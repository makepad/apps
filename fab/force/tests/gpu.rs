//! The Force program on a GPU device (an AI hub's: its GPU engine, f32 runs on Splash kernels as
//! the device schedules them, corrected in f64 by the program's CPU kernels) against the f64 CPU
//! solver on the same hierarchy, on this machine's GPU, from rest and from a guess: the
//! cantilever as printed (a clamped face; PLA walls around gyroid infill, so anisotropic
//! materials and the cells across the walls' inner edge integrated point by point) and Kirsch's
//! plate (rollers holding one axis, a hole cutting cells, a level that stops halving across the
//! plate), and a lattice whose walls are a cell or two across. Both stop at the same f64 residual (1e-6 of the load), so they agree to 1e-5 of the
//! largest displacement and of the largest von Mises stress.

use fab_force::bench;
use fab_force::material::{Materials, Print, PLA};
use fab_force::program;
use fab_force::solve::{solve, solve_until, Hierarchy, Options};
use makepad_compute_program::{Session, SessionConfig, Updates, Value};

#[test]
fn the_force_program_on_a_gpu_device_matches_the_f64_solver() {
    let device = makepad_compute_program_gpu::spawn().expect("a GPU device");
    let opts = Options::default();
    let printed = Materials::new(PLA, &Print::default(), true);
    let (grid, surface, conditions) = bench::cantilever(1.0, &printed);
    let (held, force) = fab_force::bc::nodes(&grid, &surface, &conditions);
    let iso = bench::isotropic();
    let cases = [("cantilever", printed, grid, held, force), {
        let (grid, held, force) = bench::kirsch(2.5, 20.0, 10.0, 0.5, 0.5, 10.0, &iso);
        ("kirsch", iso.clone(), grid, held, force)
    }, {
        let (grid, held, force) = bench::lattice(0.5, &iso);
        let posed = fab_force::pose(grid, held, force, Vec::new()).unwrap();
        ("lattice", iso, std::sync::Arc::unwrap_or_clone(posed.grid), posed.held, posed.force)
    }];
    for (name, m, grid, held, force) in cases {
        let h = Hierarchy::new(grid, &held, &m, &opts);
        let before = solve(&h, &force, &opts);
        // 50 N to 80 N with a sideways part, from the first answer
        let after: Vec<[f64; 3]> = force.iter().map(|f| [1.6 * f[0], 1.6 * f[1] + 0.3 * (f[0] + f[2]), 1.6 * f[2]]).collect();
        let guessed = solve_until(&h, &after, Some(&before.u), &opts, &mut |_, _| false);
        // from rest, both loads in one session (a block: the GPU engine set up once); from a
        // guess, the second
        let rested = solve(&h, &after, &opts);
        let rest: Vec<&[[f64; 3]]> = vec![&force, &after];
        let moved: Vec<&[[f64; 3]]> = vec![&after];
        for (how, loads, guess, wanted) in [("from rest", rest, None, vec![&before, &rested]), ("from a guess", moved, Some(&before.u[..]), vec![&guessed])] {
            let (params, inputs) = program::job_block(&h.levels[0], &loads, guess, &opts, program::Solves::ToTolerance).unwrap();
            let session = Session::open(&program::program(), SessionConfig { params, inputs, limits: program::limits(), gpu: Some(device.clone()), ..SessionConfig::default() });
            let mut all = Updates::default();
            let solved = loop {
                all.merge(session.wait());
                if let Some(done) = program::answers(&all) {
                    break done.unwrap_or_else(|e| panic!("{name} {how}: {e}"));
                }
            };
            let Some(Ok(end)) = &all.end else { unreachable!() };
            let runs = end.get("gpu").and_then(|g| g.get("runs")).and_then(Value::as_number).unwrap_or_else(|| panic!("{name} {how}: the program ran on the CPU: {end:?}"));
            assert_eq!(solved.len(), wanted.len(), "{name} {how}: an answer for each load");
            for (k, (s, here)) in solved.iter().zip(wanted).enumerate() {
                let how = format!("{how}, load {k}");
                let len = |v: &[f64; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
                let umax = here.u.iter().map(len).fold(0.0, f64::max);
                let err = s.u.iter().zip(&here.u).map(|(a, b)| len(&[a[0] - b[0], a[1] - b[1], a[2] - b[2]])).fold(0.0, f64::max);
                let fine = &h.levels[0].grid;
                let near = vec![false; fine.cells()];
                let vm = |u: &[[f64; 3]]| fab_force::results::evaluate(fine, &m, u, &near, None).max_vm.0;
                let (vc, vg) = (vm(&here.u), vm(&s.u));
                println!("{name} {how}: CPU {} its, the program {} its in {runs} GPU runs, residual {:.1e}, max |u| {umax:.4e}, max |du| {:.2e} relative, von Mises {vc:.5} / {vg:.5}", here.iterations, s.iterations, s.residual, err / umax);
                assert!(umax > 0.0, "{name} {how}: the load moves the part");
                assert!(s.residual <= opts.tolerance, "{name} {how}: residual {:.1e}", s.residual);
                assert!(err < 1e-5 * umax, "{name} {how}: displacements within 1e-5 of the largest: {err:.2e} of {umax:.2e}");
                assert!((vg - vc).abs() < 1e-5 * vc, "{name} {how}: largest von Mises {vg} against {vc}");
            }
        }
    }
}
