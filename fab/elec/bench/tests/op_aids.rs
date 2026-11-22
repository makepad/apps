//! The operating point's aid list on a current-limited supply (ESIM-Q4 A). On this bench the whole
//! gmin ladder creeps along a wall at 3 to 10 mA for 3500 to 6900 iterations (it fails at 3 mA and
//! crosses at 5 to 10 mA), where source stepping solves the circuit in a few hundred: the default
//! list gives way at the wall it has confirmed, reaches the same solution through source stepping,
//! and a ladder that yields and resumes is the full ladder stage for stage. At 15 to 30 mA the
//! ladder's first stage fails and source stepping solves it alone.
//!
//! The guard bounds what converges and counts what fails (ESIM-Q8). A failing Newton solve ends
//! when its iterate repeats a checkpoint (taken at 8, 16, 32 and 64 iterations) or at `itl1`, so
//! its length is 32 or 64 plus the orbit's period, or 100: quantised by the cycle detector, not a
//! property of the model, and no total of iterations is bounded. The bounds are the larger of two
//! regulator models' traces plus a tenth (one smoothing width for all three current knees, and
//! widths of each knee's own scale): converged source stages at most 63 iterations (bound 69),
//! converged gmin stages before the yield at most 37 (bound 41). The failed-stage counts are the
//! same on both.
//!
//! The LED cards' reverse breakdown (ELANG 11-16) adds the breakdown knee's limiting to every LED's
//! Newton step, measured on ledctl: converged source stages at most 79 iterations (73 at 15 and 20
//! mA, 79 at 30 mA, from 56 and 63), bound 87, the measure plus a tenth. The failed source-stage
//! counts are unchanged.

use fab_elec_bench::bench::Supply;
use fab_elec_bench::fixture::ledctl_bench;
use fab_elec_bench::{lower, ModelLibs};
use fab_elec_core::ids::{BoardId, NetRef};
use fab_elec_sim::livesim::FAST_RELTOL;
use fab_elec_sim::stamp::{EvalCx, UnkKind};
use fab_elec_sim::{operating_point_aids, GminLadder, OpAid, OpTry, Solver, OP_AIDS};

#[test]
fn gmin_yields_at_a_confirmed_wall() {
    let design = fab_elec_fixtures::ledctl();
    let libs = ModelLibs::generic();
    let scope = BoardId(0);
    let circuit = &design.unit(scope).unwrap().circuit;
    let vbus = NetRef { board: scope, net: circuit.net("VBUS").unwrap().id };
    // the order before the aid list: the whole ladder, then source stepping
    let ladder_first = [OpTry::Newton, OpTry::Gmin(GminLadder::Full), OpTry::Source, OpTry::PseudoTransient];
    let full = [OpTry::Newton, OpTry::Gmin(GminLadder::Full)];
    let resumed = [OpTry::Newton, OpTry::Gmin(GminLadder::Yield), OpTry::Gmin(GminLadder::Resume)];

    let mut failures = Vec::new();
    for limit_ma in [3, 5, 7, 10, 15, 20, 30] {
        let mut bench = ledctl_bench(1e3, [0.3; 8]);
        bench.supplies.push(Supply { net: vbus, v: 5.0, limit: Some(f64::from(limit_ma) * 1e-3), ramp: None });
        let lowered = lower(&design, scope, &bench, &libs);
        assert!(lowered.diags.is_empty(), "limit={limit_ma} mA: fixture lowers without diagnostics: {:?}", lowered.diags);
        let mut circuit = lowered.circuit;
        circuit.opts.reltol = FAST_RELTOL;
        let solve = |aids: &[OpTry], name: &str| {
            let mut s = Solver::new(&circuit).unwrap_or_else(|diag| panic!("limit={limit_ma} mA: {name} solver setup: {diag}"));
            s.op_trace = Some(Vec::new());
            let aid = operating_point_aids(&mut s, true, None, aids).ok();
            (s, aid)
        };
        let by_aid = |s: &Solver, aid: OpAid| {
            let trace = s.op_trace.as_ref().unwrap();
            let stages: Vec<_> = trace.iter().filter(|t| t.aid == aid).collect();
            let converged = stages.iter().filter(|t| t.ok).map(|t| t.iterations).sum::<u64>();
            (stages.len(), stages.iter().filter(|t| !t.ok).count(), converged)
        };
        let gmin = |s: &Solver| by_aid(s, OpAid::GminStepping);

        let (s, aid) = solve(&OP_AIDS, "default aids");
        assert_eq!(aid, Some(OpAid::SourceStepping), "limit={limit_ma} mA: source stepping wins after the ladder gives way");
        let (s_stages, s_failed, s_converged) = by_aid(&s, OpAid::SourceStepping);
        println!("limit={limit_ma} mA source stages/failed/converged iterations {s_stages}/{s_failed}/{s_converged}, total {}", s.stats.iterations);
        if s_converged > 87 {
            failures.push(format!("limit={limit_ma} mA: source stepping's converged stages within 87 iterations: {s_converged} ({s_stages} stages)"));
        }
        let most_failed = if limit_ma <= 10 { 1 } else if limit_ma < 30 { 2 } else { 3 };
        if s_failed > most_failed {
            failures.push(format!("limit={limit_ma} mA: source stepping fails at most {most_failed} stages: {s_failed} of {s_stages}"));
        }
        // After a failed stage source stepping restores the last converged point and reseats the
        // devices there: the next stage starts where the failed one did, its limiters measuring the
        // first step from that point (the state a fresh solver reseated there holds), not from the
        // failed stage's last iterate.
        // (entries that differ bit for bit, the first of them)
        let differ = |a: &[f64], b: &[f64]| {
            let at: Vec<_> = (0..a.len().max(b.len())).filter(|&i| a.get(i).map(|x| x.to_bits()) != b.get(i).map(|x| x.to_bits())).collect();
            (at.len(), at.first().map(|&i| (i, a.get(i).copied(), b.get(i).copied())))
        };
        let mut seat = Solver::new(&circuit).unwrap();
        let trace = s.op_trace.as_ref().unwrap();
        let mut retried = 0;
        for pair in trace.windows(2).filter(|p| p[0].aid == OpAid::SourceStepping && !p[0].ok && p[1].aid == OpAid::SourceStepping) {
            let (failed, next) = (&pair[0], &pair[1]);
            retried += 1;
            let (n, first) = differ(&next.start_x, &failed.start_x);
            assert!(n == 0, "limit={limit_ma} mA: after the failed source stage at {}, the stage at {} starts from the point the failed one started at: {n} unknowns differ, first {first:?}",
                failed.param, next.param);
            seat.x.copy_from_slice(&next.start_x);
            seat.reseat(&EvalCx { src_scale: next.param, ..seat.base_cx() });
            let mut reseated = Vec::new();
            for b in &seat.banks {
                b.iter_state(&mut reseated);
            }
            let (n, first) = differ(&next.start_state, &reseated);
            assert!(n == 0, "limit={limit_ma} mA: after the failed source stage at {}, the stage at {} starts with the devices reseated at that point: {n} of {} state entries differ, first (entry, held, reseated) {first:?}",
                failed.param, next.param, reseated.len());
        }
        assert_eq!(retried, s_failed, "limit={limit_ma} mA: every failed source stage is retried from the restored point");
        if limit_ma > 10 {
            continue;
        }
        let (before, before_aid) = solve(&ladder_first, "ladder first");
        assert!(before_aid.is_some(), "limit={limit_ma} mA: with the whole ladder first, the operating point converges");
        for (u, (&a, &b)) in s.x.iter().zip(&before.x).enumerate() {
            let absolute = if s.unknowns[u].kind == UnkKind::Voltage { s.opts.vntol } else { s.opts.abstol };
            let tolerance = s.opts.reltol * a.abs().max(b.abs()) + absolute;
            assert!((a - b).abs() <= tolerance,
                "limit={limit_ma} mA: the yielded and the whole-ladder solves agree within Newton's tolerance at {}: {a:e} vs {b:e}", s.unknowns[u].name);
        }
        let (stages, failed, converged) = gmin(&s);
        let (b_stages, b_failed, b_converged) = gmin(&before);
        println!("limit={limit_ma} mA gmin stages/failed/converged iterations: yielding {stages}/{failed}/{converged}, whole ladder {b_stages}/{b_failed}/{b_converged} ({before_aid:?}); total {} vs {}",
            s.stats.iterations, before.stats.iterations);
        if converged > 41 {
            failures.push(format!("limit={limit_ma} mA: gmin stepping's converged stages before the yield within 41 iterations: {converged} ({stages} stages)"));
        }
        if failed > 5 {
            failures.push(format!("limit={limit_ma} mA: gmin stepping fails at most 5 stages before it yields: {failed} of {stages}"));
        }
        if 2 * s.stats.iterations > before.stats.iterations {
            failures.push(format!("limit={limit_ma} mA: the yield at least halves the cold operating point: {} against {} with the whole ladder first",
                s.stats.iterations, before.stats.iterations));
        }

        if limit_ma <= 5 {
            // nothing between the yield and the resume: the resumed ladder must be the whole one,
            // where it fails (3 mA) and where it converges (5 mA)
            let (r, r_aid) = solve(&resumed, "yield then resume");
            let (f, f_aid) = solve(&full, "whole ladder");
            assert_eq!(r_aid, f_aid, "limit={limit_ma} mA: yield then resume ends as the whole ladder does");
            let trace = |s: &Solver| s.op_trace.as_ref().unwrap().iter().map(|t| (t.aid, t.param, t.ok)).collect::<Vec<_>>();
            assert_eq!(trace(&r), trace(&f), "limit={limit_ma} mA: yield then resume runs every stage of the whole ladder (aid, param, ok)");
            for (u, (a, b)) in r.x.iter().zip(&f.x).enumerate() {
                assert_eq!(a.to_bits(), b.to_bits(), "limit={limit_ma} mA: yield then resume ends at the whole ladder's state bit for bit at {}", r.unknowns[u].name);
            }
        }
    }
    assert!(failures.is_empty(), "the yield on a current-limited supply: {failures:#?}");
}
