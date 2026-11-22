//! E3.Q1: Exact controls accumulated transient error on ringing and decaying responses.

use fab_elec_sim::analysis::exact::EXACT_TARGET;
use fab_elec_sim::*;
use std::f64::consts::PI;

#[test]
fn exact_meets_e3_on_ringing_and_decaying() -> Result<(), SimDiag> {
    let simulate = |c: &SimCircuit, a: &Analysis| -> Result<(Record, Summary), SimDiag> {
        let mut record = Record::default();
        let summary = run(c, a, &mut record, &Control::default())?;
        Ok((record, summary))
    };
    let tran = |tstop| Analysis::Tran(TranSpec { tstop, max_step: None, uic: true });

    let (resistance, inductance, capacitance): (f64, f64, f64) = (10.0, 1e-3, 1e-6);
    let alpha = resistance / (2.0 * inductance);
    let w0 = 1.0 / (inductance * capacitance).sqrt();
    let wd = (w0 * w0 - alpha * alpha).sqrt();
    let exact = |t: f64| 1.0 - (-alpha * t).exp() * ((wd * t).cos() + alpha / wd * (wd * t).sin());
    let fs = 1.0 + (-alpha * PI / wd).exp();
    let mut ringing = SimCircuit::new();
    ringing.v("v1", "in", "0", Source::dc(1.0));
    ringing.r("r1", "in", "a", resistance);
    ringing.l("l1", "a", "b", inductance);
    ringing.c("c1", "b", "0", capacitance);
    ringing.set_ic("b", 0.0);
    ringing.opts.exact = true;
    let (record, summary) = simulate(&ringing, &tran(2e-3))?;
    let worst = record.at.iter().zip(record.v("b"))
        .map(|(&t, v)| (v - exact(t)).abs()).fold(0.0f64, f64::max);
    assert!(worst < 1e-3 * fs, "T4 Exact RLC global error {worst:e} is below 0.1% of full scale {fs}");
    assert!(summary.exact.as_ref().is_some_and(|r| r.reached), "T4 Exact RLC reaches its target: {:?}", summary.exact);
    assert!(summary.exact.as_ref().is_some_and(|r| r.runs >= 2), "T4 Exact RLC compares at least two runs");
    assert!(summary.exact.as_ref().is_some_and(|r| r.reltol < 1e-3), "T4 Exact RLC tightens the default reltol");
    assert!(summary.exact.as_ref().is_some_and(|r| r.error <= EXACT_TARGET), "T4 Exact RLC reports agreement within EXACT_TARGET");
    assert_eq!(summary.points, record.at.len(), "T4 Exact RLC summary counts only replayed points");

    ringing.opts.exact = false;
    let (record, summary) = simulate(&ringing, &tran(2e-3))?;
    let worst = record.at.iter().zip(record.v("b"))
        .map(|(&t, v)| (v - exact(t)).abs()).fold(0.0f64, f64::max);
    assert!(worst > 1e-3 * fs, "T4 Fast RLC global error {worst:e} exceeds 0.1% of full scale {fs}");
    assert!(summary.exact.is_none(), "T4 Fast RLC has no Exact report");

    let mut decaying = SimCircuit::new();
    decaying.opts.exact = true;
    decaying.v("v1", "in", "0", Source::dc(1.0));
    decaying.r("r1", "in", "rc", 1e3);
    decaying.c("c1", "rc", "0", 1e-6);
    decaying.set_ic("rc", 0.0);
    decaying.v("v2", "in2", "0", Source::dc(1.0));
    decaying.r("r2", "in2", "rl", 10.0);
    decaying.l("l1", "rl", "0", 10e-3);
    let (record, summary) = simulate(&decaying, &tran(5e-3))?;
    let mut worst_rc = 0.0f64;
    let mut worst_rl = 0.0f64;
    for ((&t, rc), il) in record.at.iter().zip(record.v("rc")).zip(record.v("l1#branch")) {
        let response = 1.0 - (-t / 1e-3).exp();
        worst_rc = worst_rc.max((rc - response).abs());
        worst_rl = worst_rl.max((il - 0.1 * response).abs());
    }
    assert!(worst_rc < 1e-3, "T2 Exact RC global error {worst_rc:e} is below 0.1% of 1 V full scale");
    assert!(worst_rl < 1e-3 * 0.1, "T3 Exact RL global error {worst_rl:e} is below 0.1% of 0.1 A full scale");
    assert!(summary.exact.as_ref().is_some_and(|r| r.reached), "T2/T3 Exact RC/RL reaches its target: {:?}", summary.exact);

    let (exact_op, exact_summary) = simulate(&decaying, &Analysis::Op)?;
    decaying.opts.exact = false;
    let (fast_op, fast_summary) = simulate(&decaying, &Analysis::Op)?;
    assert!(exact_summary.exact.is_none(), "OP ignores the Exact transient setting");
    assert!(fast_summary.exact.is_none(), "Fast OP has no Exact report");
    assert_eq!(exact_op.names, fast_op.names, "OP unknowns are unchanged by Exact");
    assert_eq!(exact_op.at, fast_op.at, "OP point times are unchanged by Exact");
    assert_eq!(exact_op.x, fast_op.x, "OP solution is unchanged by Exact");
    assert_eq!(exact_summary.aid, fast_summary.aid, "OP convergence aid is unchanged by Exact");
    assert_eq!(exact_summary.stats, fast_summary.stats, "OP solver work is unchanged by Exact");
    Ok(())
}
