use fab_elec_bench::expect::{Bound, Expect, Measure, Outcome, Probe, Verdict, When};
use fab_elec_bench::fixture::ledctl_bench;
use fab_elec_bench::{bench_run, lower, ModelLibs};
use fab_elec_core::ids::{BoardId, NetRef, PinRef};
use fab_elec_core::sel::Sel;
use std::sync::atomic::AtomicBool;

#[test]
fn bench_run_counts_expectations() -> Result<(), &'static str> {
    let design = fab_elec_fixtures::ledctl();
    let board = BoardId(0);
    let circuit = &design.unit(board).ok_or("ledctl design has board 0")?.circuit;
    let rail = NetRef { board, net: circuit.net("3V3").ok_or("fixture has the 3V3 net")?.id };
    let anode_net = circuit.net("lamps[0].d.A").ok_or("fixture has LED 0's anode net")?;
    let led = circuit.comp("lamps[0].d").ok_or("fixture has LED 0")?;
    let anode = PinRef { board, comp: led.id, pin: led.part.pin("A").ok_or("LED 0 has an anode pin")? };
    assert_eq!(circuit.net_of(led.id, anode.pin), Some(anode_net.id), "LED 0's A pin is on its named anode net");
    let missing = NetRef { board: BoardId(7), ..rail };
    let lowered = lower(&design, board, &ledctl_bench(10e3, [0.3; 8]), &ModelLibs::generic());
    assert!(lowered.diags.is_empty(), "PWM fixture lowers without diagnostics: {:?}", lowered.diags);

    let check = |name: &str, probe, measure, min, max| Expect {
        name: name.into(), probe, measure, when: When::Trailing { window: 1e-3, after: 1e-3 }, bound: Bound::Range { min, max },
    };
    // The on-current is the plateau of one on-phase (3.03 to 3.1 ms: the pin is high for the first
    // 30 us of each period): its edges carry the junction's charging spikes, a few times the
    // on-current for nanoseconds, which a Max over whole periods would read instead.
    let peak = Expect {
        when: When::Span { from: 3.04e-3, to: 3.09e-3 },
        ..check("LED on current", Probe::I(anode), Measure::Max, 0.0, f64::INFINITY)
    };
    // Calibrate the absolute limits from this lowering, then judge them at every point.
    let reached = std::cell::Cell::new(0.0);
    let calibration = bench_run(lowered.clone(), vec![peak.clone()], 4e-3, None, &AtomicBool::new(false), &|t| reached.set(t));
    assert!(calibration.error.is_none(), "on-current calibration completes: {:?}", calibration.error);
    assert!(reached.get() >= 4e-3 * (1.0 - 1e-9), "the run's progress reaches its stop: {}", reached.get());
    let on_current = calibration.states.first().ok_or("calibration returns its Max expectation")?.value;
    assert!(on_current.is_finite() && on_current > 0.0, "LED on-current Max is finite and positive: {on_current}");

    let report = bench_run(lowered, vec![
        check("settled rail", Probe::V(rail), Measure::Mean, 3.25, 3.35),
        check("LED duty", Probe::I(anode), Measure::Mean, 0.6 * on_current, 0.8 * on_current),
        peak,
        check("rail too low", Probe::V(rail), Measure::Max, 0.0, 3.0),
        check("missing net", Probe::V(missing), Measure::Mean, 0.0, 3.0),
        Expect {
            when: When::Span { from: 2e-3, to: 3e-3 },
            ..check("rail span", Probe::V(rail), Measure::Mean, 3.25, 3.35)
        },
        Expect {
            when: When::Span { from: 2e-3, to: 5e-3 },
            ..check("late rail span", Probe::V(rail), Measure::Mean, 3.25, 3.35)
        },
    ], 4e-3, None, &AtomicBool::new(false), &|_| {});
    assert!(report.error.is_none(), "counted transient completes without a solver failure: {:?}", report.error);
    assert!(report.points > 100, "counted run judges more than 100 accepted points: {}", report.points);
    assert!(report.t >= 4e-3, "counted run reaches at least 4 ms: {}", report.t);
    assert_eq!(report.states.len(), 7, "report keeps one state per expectation");
    let [rail_mean, led_mean, led_max, rail_max, unresolved, rail_span, late_span] = report.states.as_slice() else {
        return Err("report keeps all seven expectation states in order");
    };
    assert_eq!(rail_mean.verdict, Verdict::Pass, "settled 3V3 mean passes its limits");
    assert_eq!(rail_mean.failed, None, "settled 3V3 mean never fails");
    assert_eq!(led_max.verdict, Verdict::Pass, "LED on-current Max passes");
    assert!(led_max.value > 0.0 && led_max.done, "checked run's LED on-current Max is positive and final at its span's end: {}", led_max.value);
    assert_eq!(led_mean.verdict, Verdict::Pass, "the LED mean at 70 % on-time (the pin sinks it while low, 30 % high) passes its calibrated limits");
    assert_eq!(led_mean.failed, None, "the LED mean passes every complete settled window");
    assert!(
        (0.6 * led_max.value..=0.8 * led_max.value).contains(&led_mean.value),
        "LED mean is 60 % to 80 % of the same run's on-current Max: {} vs {}", led_mean.value, led_max.value
    );
    assert_eq!(rail_max.verdict, Verdict::Fail, "3V3 Max fails the deliberately low 3.0 V limit");
    assert!(rail_max.failed.is_some_and(|(t, _)| t >= 2e-3), "rail failure is recorded only after settling plus a full window");
    assert_eq!(report.diags.len(), 1, "only the unresolved board 7 probe produces a diagnostic");
    assert!(report.diags.first().is_some_and(|diag| diag.sel == vec![Sel::Net(missing)]), "diagnostic selects the board 7 net");
    assert_eq!(unresolved.verdict, Verdict::Pending, "unlowered board 7 net stays Pending");
    assert!(unresolved.failed.is_none() && unresolved.value.is_nan(), "unresolved probe has no judgment or failure");
    for state in [rail_mean, led_mean, led_max, rail_span] {
        assert_eq!(state.outcome(), Outcome::Pass, "passing checks pass the counted run");
    }
    assert_eq!(rail_max.outcome(), Outcome::Fail, "rail too low fails the counted run");
    assert_eq!(unresolved.outcome(), Outcome::NotJudged, "unresolved board 7 net is NotJudged");
    assert_eq!(rail_span.verdict, Verdict::Pass, "3V3 mean over 2 to 3 ms passes");
    assert!(rail_span.done, "completed rail span is final");
    assert_eq!(rail_span.at, 3e-3, "rail span is judged at its end");
    assert_eq!(late_span.outcome(), Outcome::NotJudged, "span ending after the stop is NotJudged");
    assert_eq!(late_span.verdict, Verdict::Pending, "late span has no judgment");
    assert!(!late_span.done && late_span.value.is_nan() && late_span.at.is_nan() && late_span.failed.is_none(), "late span is unfinished at the 4 ms stop");
    assert!(
        [rail_mean, led_mean, rail_max].iter().all(|state| state.at == report.t),
        "every resolved trailing expectation is judged through the final accepted point"
    );
    Ok(())
}
