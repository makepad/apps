use fab_elec_bench::fixture::ledctl_bench;
use fab_elec_bench::{lower, ModelLibs};
use fab_elec_core::ids::BoardId;
use fab_elec_sim::{run, Analysis, Control, OpAid, Record, TranSpec};

/// The stepper's work on ledctl at 10 kHz PWM over 20 periods, per period: accepted steps,
/// rejected steps and Newton iterations (the operating point's own iterations taken out); and the
/// operating point settles by plain Newton (no gmin, source or pseudo-transient stepping).
#[test]
fn ledctl_step_counts() -> Result<(), String> {
    let design = fab_elec_fixtures::ledctl();
    let bench = ledctl_bench(10e3, [0.3; 8]);
    let lowered = lower(&design, BoardId(0), &bench, &ModelLibs::generic());
    assert!(lowered.diags.is_empty(), "LED controller lowers without diagnostics: {:?}", lowered.diags);
    let periods = 20.0;
    let analysis = Analysis::Tran(TranSpec { tstop: periods / 10e3, max_step: None, uic: false });

    let mut failures: Vec<String> = Vec::new();
    // Bounds: the stepper's work with the MCU pins as GPIO devices, a pad capacitance on every pin
    // and the rail-current cap (ESIM-Q9, 11-15): 52.45 (live) and 80.2 (run) accepted steps a
    // period; 0.05 rejected at 1e-2 and 6.9 % at 1e-3 (4.95 on error and 0.95 provisional a
    // period); Newton 116.9 (live) and 198.8 (run) iterations a period. Before the cap: 40.3 and
    // 49.2 accepted, 96.5 and 136.7 Newton. The added steps are the ones that put the supply
    // current inside its tolerance: after each LED edge the cap decides VBUS's steps through the
    // rail's decay (a 100 ns cable mode and a 294 ns output mode of 20.8 mA). At 1e-3, per decay
    // from 30 ns to 2.4 us: 14.5 steps at the error estimate's own optimum, 19 accepted with the
    // controller at 0.77 of it, and one rejection where the supply current's inflection zeroes the
    // third-derivative estimate; then two or three steps more than before to grow out of the
    // decay's short steps: about 16 attempts more each half period (ESIM-Q10). At 1e-2 VBUS decides
    // 13 to 14 attempts a decay: its steps there are about one time constant of the 294 ns mode (z =
    // h / tau near 1), where the third difference reads (e^z - 1)^3, not z^3, so ten times the
    // tolerance lengthens them 1.8 times, not 10^(1/3) = 2.15. The cube-root law holds for z under
    // about 0.3 (ESIM-Q11).
    // Junction charge at the GPIO edges (the LED cards' capacitance, transit time and breakdown,
    // ELANG 11-16), measured on ledctl: 70.3 (live) and 109.3 (run) accepted steps a period, 6.7 %
    // and 8.3 % rejected, Newton 174.6 and 272.4 iterations a period (before the cards: 52.5 and
    // 80.2, 0.1 % and 6.9 %, 116.9 and 198.8). Each pin edge charges or discharges the eight LEDs'
    // junctions through their loop (about 117 ohm), and the ported rate and level terms follow
    // that charge to or from zero (ESIM-Q12):
    // - turn-off (the pad rises): the junction's 200 pC decays with tau = Cj x 117 ohm, about 6 ns.
    //   On a decaying exponential the rate term gives steps of 0.7 tau (measured 4.1 to 4.7 ns),
    //   decided by the rate, then the level, then the charge floor, until the charge is under it.
    //   The steps scale with tau, so a tenth or ten times the capacitance costs about the same; a
    //   hundredth starts near the floor and costs nothing;
    // - turn-on (the pad falls): the junction charges from rest, where the level term is nothing
    //   and the rate term grows as h^2 like the error, so the first step starts on the charge floor
    //   (about 0.13 ns).
    // The 5 rejections a period at 1e-2 are the doubled confirms at both ramp ends, the provisional
    // step from rest and the confirm after its retry, and one order-2 step on the turn-off tail.
    // The bounds are those numbers plus a tenth.
    for (reltol, mode, max_iterations, max_accepted, max_rejected_pct) in
        [(1e-2, "live", 193.0, 78.0, 7.4), (1e-3, "run", 300.0, 121.0, 9.2)]
    {
        let mut circuit = lowered.circuit.clone();
        circuit.opts.reltol = reltol;
        let op = run(&circuit, &Analysis::Op, &mut Record::default(), &Control::default())
            .map_err(|diag| format!("reltol={reltol:e} ({mode}): operating point failed: {diag}"))?;
        if op.aid != Some(OpAid::Newton) {
            failures.push(format!("reltol={reltol:e} ({mode}): operating point needed {:?} after {} iterations, not plain Newton",
                op.aid, op.stats.iterations));
        }
        let summary = run(&circuit, &analysis, &mut Record::default(), &Control::default())
            .map_err(|diag| format!("reltol={reltol:e} ({mode}): transient failed: {diag}"))?;
        let st = summary.stats;
        let accepted = st.steps.checked_sub(st.rejected)
            .ok_or_else(|| format!("reltol={reltol:e}: rejected {} exceeds steps {}", st.rejected, st.steps))?;
        let iterations = st.iterations.saturating_sub(op.stats.iterations);
        let by_cause = st.rejected_error + st.rejected_provisional + st.rejected_newton + st.rejected_crossing;
        assert_eq!(
            by_cause, st.rejected,
            "reltol={reltol:e}: rejected {} is the sum of error {} + provisional {} + newton {} + crossing {}",
            st.rejected, st.rejected_error, st.rejected_provisional, st.rejected_newton, st.rejected_crossing,
        );
        let per = |n: u64| n as f64 / periods;
        let rejected_pct = 100.0 * st.rejected as f64 / st.steps.max(1) as f64;
        println!(
            "reltol={reltol:e} ({mode}) per period: accepted {:.2}, rejected {:.2} ({rejected_pct:.1} %: newton {:.2}, crossing {:.2}, provisional {:.2}, error {:.2}), newton iterations {:.1} ({:.2} per accepted step); op iterations {}",
            per(accepted), per(st.rejected), per(st.rejected_newton), per(st.rejected_crossing),
            per(st.rejected_provisional), per(st.rejected_error), per(iterations),
            iterations as f64 / accepted.max(1) as f64, op.stats.iterations,
        );
        for (what, value, bound) in [
            ("accepted steps a period", per(accepted), max_accepted),
            ("rejected %", rejected_pct, max_rejected_pct),
            ("newton iterations a period", per(iterations), max_iterations),
        ] {
            if value > bound {
                failures.push(format!("reltol={reltol:e} ({mode}): {what} {value:.2} above {bound}"));
            }
        }
    }
    assert!(failures.is_empty(), "ledctl step counts: {failures:#?}");
    Ok(())
}
