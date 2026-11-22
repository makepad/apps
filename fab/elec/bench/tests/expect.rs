use fab_elec_bench::expect::*;
use fab_elec_bench::ValueIndex;
use fab_elec_core::diag::Severity;
use fab_elec_core::ids::{BoardId, CompId, CompRef, NetId, NetRef, PinIx, PinRef};
use fab_elec_core::sel::Sel;

#[test]
fn expectations_judge_windows_exactly() {
    let board = BoardId(0);
    let n0 = NetRef { board, net: NetId(0) };
    let n1 = NetRef { board, net: NetId(1) };
    let comp = CompRef { board, comp: CompId(0) };
    let pin = PinRef { board, comp: comp.comp, pin: PinIx(0) };
    let missing = NetRef { board: BoardId(9), net: NetId(0) };
    let mut index = ValueIndex::new(1);
    assert_eq!(index.add_net(n0), 0, "n0 sample numbering");
    assert_eq!(index.add_net(n1), 1, "n1 sample numbering");
    assert_eq!(index.add_comp(comp, 2, None), (0, 0), "component and pin numbering");
    assert_eq!(index.counts(), (2, 2, 1), "sample slice lengths");
    assert_eq!(Probe::V(n0).unit(), "V", "net voltage unit");
    assert_eq!(Probe::VDiff(n0, n1).unit(), "V", "differential voltage unit");
    assert_eq!(Probe::I(pin).unit(), "A", "current into the pin is in amperes");
    assert_eq!(Probe::P(comp).unit(), "W", "dissipated power is in watts");

    let window = 1e-3;
    let pwm_mean = (3e-5 + 1e-7) / 1e-4;
    let sine_rms = 2.0 / 2.0_f64.sqrt();
    let check = |name: &str, probe, measure, min, max| Expect {
        name: name.into(), probe, measure, when: When::Trailing { window, after: 0.0 }, bound: Bound::Range { min, max },
    };
    let mut delayed = check("settled rail", Probe::V(n0), Measure::Mean, pwm_mean - 1e-12, pwm_mean + 1e-12);
    delayed.when = When::Trailing { window, after: 5e-4 };
    let list = vec![
        check("PWM mean", Probe::V(n0), Measure::Mean, pwm_mean - 1e-12, pwm_mean + 1e-12),
        check("sine RMS", Probe::V(n1), Measure::Rms, sine_rms - 1e-4, sine_rms + 1e-4),
        check("sine min", Probe::V(n1), Measure::Min, -2.0 - 1e-3, -2.0 + 1e-3),
        check("sine max", Probe::V(n1), Measure::Max, 2.0 - 1e-3, 2.0 + 1e-3),
        check("sine peak to peak", Probe::V(n1), Measure::PeakToPeak, 4.0 - 1e-3, 4.0 + 1e-3),
        check("sine now", Probe::V(n1), Measure::Now, -1.0, 1.0),
        check("difference", Probe::VDiff(n1, n0), Measure::Now, -3.0, 2.0),
        check("LED current", Probe::I(pin), Measure::Mean, 0.005 - 1e-15, 0.005 + 1e-15),
        check("power", Probe::P(comp), Measure::Mean, 0.01 - 1e-15, 0.01 + 1e-15),
        check("missing", Probe::V(missing), Measure::Now, -1.0, 1.0),
        delayed,
    ];
    let (mut expectations, diags) = Expectations::new(list.clone(), &index);
    assert_eq!(expectations.expects(), list.as_slice(), "expectation definitions are preserved");
    assert_eq!(diags.len(), 1, "one diagnostic for an unresolved probe");
    assert_eq!(diags[0].severity, Severity::Error, "unresolved probe is an error");
    assert_eq!(diags[0].message, format!("missing: {:?} is not in the simulated scope", Probe::V(missing)), "diagnostic names the check and probe");
    assert_eq!(diags[0].sel, vec![Sel::Net(missing)], "diagnostic selects the missing net");
    let states = expectations.states();
    assert_eq!(states[0].sel, vec![Sel::Net(n0)], "voltage selection");
    assert_eq!(states[6].sel, vec![Sel::Net(n1), Sel::Net(n0)], "differential selection order");
    assert_eq!(states[7].sel, vec![Sel::Pin(pin), Sel::Comp(comp)], "current selects pin and component");
    assert_eq!(states[8].sel, vec![Sel::Comp(comp)], "power selection");
    assert!(states.iter().all(|s| s.verdict == Verdict::Pending && s.value.is_nan() && s.at.is_nan() && s.failed.is_none() && s.worst == 0.0), "all checks start pending with empty history");
    assert!(states.iter().all(|s| !s.done && s.outcome() == Outcome::NotJudged), "all checks start unfinished and NotJudged");

    // Integer 100 ns ticks place every PWM corner exactly; intermediate samples are 2.3 us
    // apart within each segment. A period is 100 us, including a 30 us high plateau.
    let mut ticks = Vec::new();
    for period in 0..40 {
        let start = period * 1000;
        let corners = [start, start + 1, start + 301, start + 302, start + 1000];
        for segment in corners.windows(2) {
            ticks.push(segment[0]);
            let mut tick = segment[0] + 23;
            while tick < segment[1] {
                ticks.push(tick);
                tick += 23;
            }
        }
    }
    ticks.push(40_000);
    let samples: Vec<_> = ticks.into_iter().map(|tick| {
        let t = f64::from(tick) * 1e-7;
        let phase = tick % 1000;
        let v0 = if (1..=301).contains(&phase) { 1.0 } else { 0.0 };
        (t, [v0, 2.0 * (std::f64::consts::TAU * 1e3 * t).sin()])
    }).collect();
    assert!(samples.windows(2).all(|s| s[0].0 < s[1].0), "synthetic times strictly increase");
    let first_failure = samples.iter().find(|(_, v)| v[1].abs() > 1.0)
        .map(|(t, v)| (*t, v[1])).expect("sine must cross the Now limits");
    let pin_i = [0.005, -0.005];
    let part_p = [0.01];
    let mut passed_again = false;
    for &(t, net_v) in &samples {
        expectations.step(&Sample { t, net_v: &net_v, pin_i: &pin_i, part_p: &part_p });
        let states = expectations.states();
        assert!(states.iter().all(|s| !s.done), "trailing checks never become final at {t}");
        let now = &states[5];
        assert_eq!(now.verdict, if net_v[1].abs() <= 1.0 { Verdict::Pass } else { Verdict::Fail }, "Now uses inclusive limits at {t}");
        assert_eq!(now.value, net_v[1], "Now reads the current sample at {t}");
        assert_eq!(now.at, t, "Now records its judgment time");
        assert_eq!(now.failed, (t >= first_failure.0).then_some(first_failure), "first failure persists at {t}");
        passed_again |= now.failed.is_some() && now.verdict == Verdict::Pass;
        assert_eq!(states[6].value, net_v[1] - net_v[0], "VDiff subtracts in probe order at {t}");
        assert_eq!(states[9].verdict, Verdict::Pending, "unresolved probe stays pending at {t}");
        assert!(states[9].value.is_nan() && states[9].failed.is_none(), "unresolved probe never has a judged value");
        if t >= window {
            for (i, expected, tolerance) in [
                (0, pwm_mean, 1e-12), (1, sine_rms, 1e-4), (2, -2.0, 1e-3),
                (3, 2.0, 1e-3), (4, 4.0, 1e-3), (7, 0.005, 1e-15), (8, 0.01, 1e-15),
            ] {
                let state = &states[i];
                assert!((state.value - expected).abs() <= tolerance, "{} is exact within tolerance at {t}: {} vs {expected}", list[i].name, state.value);
                assert_eq!(state.verdict, Verdict::Pass, "{} passes every complete window at {t}", list[i].name);
                assert_eq!(state.at, t, "{} is judged at each accepted time", list[i].name);
                assert!(state.failed.is_none() && state.worst == 0.0, "{} never has a spurious failure", list[i].name);
            }
        } else {
            for i in [0, 1, 2, 3, 4, 7, 8] {
                assert_eq!(states[i].verdict, Verdict::Pending, "{} waits for a complete window", list[i].name);
                assert!(states[i].value.is_nan(), "{} has no value while pending", list[i].name);
            }
        }
        if t - window >= 5e-4 {
            assert_eq!(states[10].verdict, Verdict::Pass, "after gate judges at and beyond 1.5 ms");
            assert!((states[10].value - pwm_mean).abs() <= 1e-12, "settled mean uses the full trailing window");
        } else {
            assert_eq!(states[10].verdict, Verdict::Pending, "after gate waits until the whole window follows settling");
        }
    }
    assert!(passed_again, "Now passes again after its first failure");
    assert_eq!(expectations.states()[5].verdict, Verdict::Pass, "Now ends passing");
    assert_eq!(expectations.states()[5].outcome(), Outcome::Fail, "Now's first failure determines its run outcome even after it passes again");
    assert_eq!(expectations.states()[9].outcome(), Outcome::NotJudged, "an unresolved probe never passes by default");
    for i in [0, 1, 2, 3, 4, 6, 7, 8, 10] {
        assert_eq!(expectations.states()[i].outcome(), Outcome::Pass, "{} passes the run", list[i].name);
    }
    assert!((expectations.states()[5].worst - 1.0).abs() < 1e-3, "worst records the largest excursion");
    let &(last_t, last_v) = samples.last().expect("waveform has a last sample");
    assert_eq!(expectations.states()[6].value, last_v[1] - last_v[0], "VDiff resolves and reads the last sample");
    let before = expectations.states()[..9].to_vec();
    for t in [last_t, last_t - 1e-6] {
        expectations.step(&Sample { t, net_v: &[99.0, 99.0], pin_i: &[99.0, 99.0], part_p: &[99.0] });
        assert_eq!(&expectations.states()[..9], before.as_slice(), "duplicate and older times leave state unchanged");
    }

    let selections: Vec<_> = expectations.states().iter().map(|s| s.sel.clone()).collect();
    expectations.reset();
    for (i, state) in expectations.states().iter().enumerate() {
        assert!(state.verdict == Verdict::Pending && state.value.is_nan() && state.at.is_nan() && state.failed.is_none() && state.worst == 0.0, "reset clears state for {}", list[i].name);
        assert!(!state.done && state.outcome() == Outcome::NotJudged, "reset clears completion and outcome for {}", list[i].name);
        assert_eq!(state.sel, selections[i], "reset preserves selection for {}", list[i].name);
    }
    assert_eq!(expectations.expects(), list.as_slice(), "reset preserves definitions");
    for &(t, net_v) in &samples {
        expectations.step(&Sample { t, net_v: &net_v, pin_i: &pin_i, part_p: &part_p });
        if t == 0.0 {
            assert_eq!(expectations.states()[5].verdict, Verdict::Pass, "reset accepts a fresh time zero");
            assert_eq!(expectations.states()[0].verdict, Verdict::Pending, "reset discards the old window");
        }
    }
    assert_eq!(&expectations.states()[..9], before.as_slice(), "fresh feed reproduces judgments and failure history");
    assert_eq!(expectations.states()[9].verdict, Verdict::Pending, "reset keeps unresolved probes unresolved");
    assert_eq!(expectations.states()[10].verdict, Verdict::Pass, "settling check judges again after reset");

    // A single segment straddles the left edge: [0, 2 ms] maps to [0, 2], so the
    // trailing millisecond is exactly the ramp from 1 to 2, including its extrema.
    expectations.reset();
    for (t, v) in [(0.0, 0.0), (2e-3, 2.0)] {
        expectations.step(&Sample { t, net_v: &[v, v], pin_i: &pin_i, part_p: &part_p });
    }
    for (i, expected) in [(0, 1.5), (1, (7.0_f64 / 3.0).sqrt()), (2, 1.0), (3, 2.0), (4, 1.0)] {
        assert!((expectations.states()[i].value - expected).abs() < 1e-14, "{} includes the interpolated left edge", list[i].name);
    }

    expectations.reset();
    for (t, v) in [(0.0, f64::NAN), (1e-3, f64::INFINITY), (2e-3, f64::NEG_INFINITY)] {
        expectations.step(&Sample { t, net_v: &[v, v], pin_i: &pin_i, part_p: &part_p });
        let now = &expectations.states()[5];
        assert_eq!(now.verdict, Verdict::Fail, "Now fails nonfinite values");
        assert!(now.value == v || (now.value.is_nan() && v.is_nan()), "Now preserves the nonfinite judged value");
        assert_eq!(now.worst, f64::INFINITY, "NaN is infinitely far outside the range");
    }
    for t in [3e-3, 4e-3, 5e-3] {
        expectations.step(&Sample { t, net_v: &[0.0, 0.0], pin_i: &pin_i, part_p: &part_p });
    }
    let now = &expectations.states()[5];
    assert_eq!(now.verdict, Verdict::Pass, "Now recovers after nonfinite samples");
    assert!(now.failed.is_some_and(|(t, v)| t == 0.0 && v.is_nan()), "first NaN failure remains recorded");
    for i in [0, 1, 2, 3, 4] {
        assert_eq!(expectations.states()[i].value, 0.0, "{} recovers after nonfinite samples leave the window", list[i].name);
    }

    let (mut long, diags) = Expectations::new(vec![check("long constant", Probe::V(n0), Measure::Mean, 1.0 - 1e-12, 1.0 + 1e-12)], &index);
    assert!(diags.is_empty(), "long-run probe resolves");
    for i in 0..200_000 {
        long.step(&Sample { t: 1e6 + f64::from(i) * 1e-6, net_v: &[1.0, 0.0], pin_i: &pin_i, part_p: &part_p });
    }
    let state = &long.states()[0];
    assert!((state.value - 1.0).abs() <= 1e-12, "rebasing preserves the constant mean at large simulated times: {}", state.value);
    assert_eq!(state.verdict, Verdict::Pass, "long-run mean passes");
    assert!(!state.done && state.outcome() == Outcome::Pass, "passing trailing windows remain live");
    assert!(state.failed.is_none() && state.worst == 0.0, "rebasing avoids transient constant-signal failures");
}

#[test]
fn span_checks_are_judged_once_at_their_end() {
    let n0 = NetRef { board: BoardId(0), net: NetId(0) };
    let mut index = ValueIndex::new(1);
    assert_eq!(index.add_net(n0), 0, "span signal sample numbering");

    let from = 1.5_f64;
    let to = 4.25_f64;
    // v(t) = 2t up to t=2, then 8-2t through the span's end.
    // Integrate those polynomials analytically, including both clipped segments.
    let end_v = 8.0 - 2.0 * to;
    let area = 2.0_f64.powi(2) - from.powi(2) + 8.0 * (to - 2.0) - (to.powi(2) - 4.0);
    let square = 4.0 / 3.0 * (2.0_f64.powi(3) - from.powi(3))
        + (4.0_f64.powi(3) - end_v.powi(3)) / 6.0;
    let mean = area / (to - from);
    let rms = (square / (to - from)).sqrt();
    let early_mean = (area + from.powi(2) - 1.0) / (to - 1.0);
    let samples = [(1.0, 2.0), (2.0, 4.0), (3.5, 1.0), (5.0, -2.0), (6.25, -99.0), (8.0, 99.0)];
    assert!(samples.windows(2).all(|pair| pair[0].0 < pair[1].0), "span samples strictly increase");
    assert!(samples.iter().all(|&(t, _)| t != from && t != to), "both span ends require interpolation");

    let check = |name: &str, measure, from, to, min, max| Expect {
        name: name.into(), probe: Probe::V(n0), measure, when: When::Span { from, to }, bound: Bound::Range { min, max },
    };
    let list = vec![
        check("span mean", Measure::Mean, from, to, mean * (1.0 - 1e-12), mean * (1.0 + 1e-12)),
        check("span RMS", Measure::Rms, from, to, rms * (1.0 - 1e-12), rms * (1.0 + 1e-12)),
        check("span min", Measure::Min, from, to, end_v, end_v),
        check("span max", Measure::Max, from, to, 4.0, 4.0),
        check("span peak to peak", Measure::PeakToPeak, from, to, 4.0 - end_v, 4.0 - end_v),
        check("span now", Measure::Now, from, to, 0.0, 4.0),
        check("span before sampling", Measure::Mean, 0.0, to, early_mean * (1.0 - 1e-12), early_mean * (1.0 + 1e-12)),
        check("late span", Measure::Mean, from, 10.0, 0.0, 4.0),
        check("reversed span", Measure::Mean, to, from, 0.0, 4.0),
        check("nonfinite span", Measure::Now, f64::NAN, to, 0.0, 4.0),
        check("failing span", Measure::Mean, from, to, 0.0, 1.0),
        check("unsampled span", Measure::Mean, 0.0, 0.5, 0.0, 4.0),
    ];
    let (mut expectations, diags) = Expectations::new(list, &index);
    assert_eq!(diags.len(), 2, "only the two invalid spans produce diagnostics");
    for (diag, name, start, end) in [
        (&diags[0], "reversed span", to, from),
        (&diags[1], "nonfinite span", f64::NAN, to),
    ] {
        assert_eq!(diag.severity, Severity::Error, "invalid span is an error");
        assert_eq!(diag.message, format!("{name}: span from {start} to {end} is empty or not finite"), "diagnostic names the invalid bounds");
        assert_eq!(diag.sel, vec![Sel::Net(n0)], "invalid span selects its resolved probe");
    }

    // Pending states contain NaNs, so compare their bits as well as every other field.
    let same_state = |actual: &CheckState, expected: &CheckState| {
        assert_eq!(actual.verdict, expected.verdict, "final verdict is unchanged");
        assert_eq!(actual.done, expected.done, "completion is unchanged");
        assert_eq!(actual.value.to_bits(), expected.value.to_bits(), "judged value is unchanged");
        assert_eq!(actual.at.to_bits(), expected.at.to_bits(), "judgment time is unchanged");
        assert_eq!(actual.failed, expected.failed, "first failure is unchanged");
        assert_eq!(actual.worst, expected.worst, "worst excursion is unchanged");
        assert_eq!(actual.sel, expected.sel, "selection is unchanged");
        assert_eq!(actual.outcome(), expected.outcome(), "run outcome is unchanged");
    };
    let mut final_states: Option<Vec<CheckState>> = None;
    for pass in 0..2 {
        if pass != 0 {
            expectations.reset();
        }
        for (i, state) in expectations.states().iter().enumerate() {
            assert_eq!(state.done, i == 8 || i == 9, "only invalid spans start done after reset");
            assert_eq!(state.verdict, Verdict::Pending, "all spans start Pending");
            assert_eq!(state.outcome(), Outcome::NotJudged, "all spans start NotJudged");
            assert!(state.value.is_nan() && state.at.is_nan() && state.failed.is_none() && state.worst == 0.0, "span history starts empty");
        }
        for &(t, v) in &samples {
            expectations.step(&Sample { t, net_v: &[v], pin_i: &[], part_p: &[] });
            let states = expectations.states();
            for (i, expected) in [(0, mean), (1, rms), (2, end_v), (3, 4.0), (4, 4.0 - end_v), (6, early_mean), (10, mean)] {
                let state = &states[i];
                if t < to {
                    assert_eq!(state.verdict, Verdict::Pending, "windowed span {i} waits for its end at {t}");
                    assert!(!state.done && state.outcome() == Outcome::NotJudged, "windowed span {i} is not final before its end");
                    assert!(state.value.is_nan() && state.at.is_nan(), "windowed span {i} has no early judgment");
                } else {
                    assert!(state.done, "windowed span {i} is final at the first point past its end");
                    assert!((state.value - expected).abs() <= expected.abs() * 1e-12, "span {i} integrates the clipped signal: {} vs {expected}", state.value);
                    assert_eq!(state.at, to, "windowed span {i} is judged at the span end");
                    if i == 10 {
                        assert_eq!(state.verdict, Verdict::Fail, "excluded span mean fails");
                        assert_eq!(state.outcome(), Outcome::Fail, "failing span fails the run");
                        assert_eq!(state.failed, Some((to, state.value)), "span failure is recorded at its end");
                        assert_eq!(state.worst, state.value - 1.0, "span failure records its distance above the limit");
                    } else {
                        assert_eq!(state.verdict, Verdict::Pass, "windowed span {i} passes");
                        assert_eq!(state.outcome(), Outcome::Pass, "windowed span {i} passes the run");
                        assert!(state.failed.is_none() && state.worst == 0.0, "windowed span {i} never fails");
                    }
                }
            }
            let now = &states[5];
            assert_eq!(now.done, t >= to, "Now becomes final at the first point past its end");
            if t < from {
                assert_eq!(now.verdict, Verdict::Pending, "Now ignores points before its span");
                assert_eq!(now.outcome(), Outcome::NotJudged, "Now starts NotJudged");
            } else {
                let (at, value) = if t <= to { (t, v) } else { (3.5, 1.0) };
                assert_eq!(now.at, at, "Now records only accepted times inside its span");
                assert_eq!(now.value, value, "Now ignores points past its span");
                assert_eq!(now.verdict, Verdict::Pass, "Now passes every point inside its span");
                assert_eq!(now.outcome(), Outcome::Pass, "Now passes on the sampled span values");
            }
            for i in [7, 8, 9, 11] {
                let state = &states[i];
                assert_eq!(state.done, i != 7, "invalid and unsampled spans are done; the late span remains open");
                assert_eq!(state.verdict, Verdict::Pending, "unjudged spans stay Pending");
                assert_eq!(state.outcome(), Outcome::NotJudged, "unjudged spans never pass by default");
                assert!(state.value.is_nan() && state.at.is_nan() && state.failed.is_none() && state.worst == 0.0, "unjudged spans have no judgment history");
            }
            if t >= to {
                if let Some(expected) = &final_states {
                    for (actual, expected) in states.iter().zip(expected) {
                        same_state(actual, expected);
                    }
                } else {
                    final_states = Some(states.to_vec());
                }
            }
        }
    }
}

#[path = "support/pwm.rs"] mod pwm;
#[path = "support/serial.rs"] mod serial;

#[test]
fn timing_spans_windows_and_chunking_share_the_offline_machine() {
    use fab_elec_bench::edges::Threshold;
    use fab_elec_bench::measure::{self, Slope};
    let net = NetRef { board: BoardId(0), net: NetId(0) };
    let delayed = NetRef { net: NetId(1), ..net };
    let missing = NetRef { net: NetId(2), ..net };
    let mut index = ValueIndex::new(0);
    index.add_net(net);
    index.add_net(delayed);
    let threshold = Threshold { level: 0.5, hysteresis: 0.1 };
    let end = 0.008;
    let (t, y) = pwm::record(end, 0.0);
    let (dt, dy) = pwm::record(end, 0.0001);
    assert_eq!(t, dt, "both pins use the same accepted times");
    let timing = measure::timing(&t, &y, threshold);
    let check = |name: &str, measure, when| Expect {
        name: name.into(), probe: Probe::V(net), measure, when, bound: Bound::Range { min: 0.0, max: f64::INFINITY },
    };
    let span = When::Span { from: 0.0, to: end };
    let measures = [Measure::Frequency(threshold), Measure::Duty(threshold),
        Measure::Width { threshold, high: true }, Measure::Width { threshold, high: false }];
    let mut list: Vec<_> = measures.iter().map(|m| check("timing", m.clone(), span)).collect();
    list.extend(measures.iter().map(|m| check("window", m.clone(), When::Trailing { window: 0.003, after: 0.0 })));
    list.push(check("count", Measure::EdgeCount { threshold, slope: Slope::Either }, span));
    let delay = Measure::Delay { threshold, from: Slope::Rising, to: Probe::V(delayed), to_slope: Slope::Rising };
    list.push(check("delay", delay.clone(), span));
    list.push(check("half cycle", Measure::Frequency(threshold), When::Span { from: 0.0002, to: 0.0007 }));
    list.push(check("no target edge", delay, When::Span { from: 0.0002, to: 0.00025 }));
    list.push(check("unresolved target", Measure::Delay { threshold, from: Slope::Either, to: Probe::V(missing), to_slope: Slope::Either }, span));
    list.push(check("short window", Measure::Frequency(threshold), When::Trailing { window: 0.0005, after: 0.0 }));
    let (mut expectations, diags) = Expectations::new(list.clone(), &index);
    assert_eq!(diags.len(), 1, "unresolved target has one diagnostic");
    assert_eq!(diags[0].sel, vec![Sel::Net(missing)], "target diagnostic selects the unresolved net");
    let (mut chunks, _) = Expectations::new(list, &index);
    for ((&t, &y), &dy) in t.iter().zip(&y).zip(&dy) {
        expectations.step(&Sample { t, net_v: &[y as f64, dy as f64], pin_i: &[], part_p: &[] });
        if t >= 0.003 {
            for state in &expectations.states()[4..8] {
                assert_eq!(state.at, t, "timing windows judge every accepted point");
                assert_eq!(state.verdict, Verdict::Pass, "complete trailing cycles pass");
            }
            let states = expectations.states();
            assert!((states[4].value - 1000.0).abs() < 6.0, "window frequency follows PWM");
            assert!((states[5].value - 30.0).abs() < 0.6, "window duty follows PWM");
        }
    }
    for ((t, y), dy) in t.chunks(37).zip(y.chunks(37)).zip(dy.chunks(37)) {
        for ((&t, &y), &dy) in t.iter().zip(y).zip(dy) {
            chunks.step(&Sample { t, net_v: &[y as f64, dy as f64], pin_i: &[], part_p: &[] });
        }
    }
    let states = expectations.states();
    for (i, offline) in [timing.frequency, timing.duty, timing.width_high, timing.width_low].into_iter().enumerate() {
        assert_eq!(states[i].value.to_bits(), offline.unwrap().to_bits(), "measure {i}: offline and streamed span are bit-identical");
    }
    for (a, b) in states.iter().zip(chunks.states()) {
        assert_eq!(a.value.to_bits(), b.value.to_bits(), "chunk boundaries preserve values including Pending NaN");
        assert_eq!(a.verdict, b.verdict, "chunk boundaries preserve verdicts");
        assert_eq!(a.failed, b.failed, "chunk boundaries preserve first failure");
    }
    assert_eq!(states[8].value, measure::edges(&t, &y, threshold).len() as f64, "Either counts every offline edge");
    assert!((states[9].value - 0.0001).abs() < 0.000006, "inter-pin delay reads the known offset");
    for i in [10, 11, 12, 13] {
        assert_eq!(states[i].verdict, Verdict::Pending, "incomplete check {i} stays Pending");
        assert_eq!(states[i].outcome(), Outcome::NotJudged, "incomplete check {i} is NotJudged");
    }
    expectations.reset();
    assert!(expectations.states().iter().all(|s| s.outcome() == Outcome::NotJudged), "reset clears every timing history");
}

#[test]
fn rise_fall_and_window_expiry_use_complete_edges() {
    use fab_elec_bench::edges::Threshold;
    use fab_elec_bench::measure::Slope;
    let net = NetRef { board: BoardId(0), net: NetId(0) };
    let mut index = ValueIndex::new(0);
    index.add_net(net);
    let check = |measure, when| Expect {
        name: "transition".into(), probe: Probe::V(net), measure, when, bound: Bound::Range { min: 0.0, max: 10.0 },
    };
    let span = When::Span { from: 0.0, to: 20.0 };
    let threshold = Threshold { level: 0.5, hysteresis: 0.1 };
    let (mut checks, diags) = Expectations::new(vec![
        check(Measure::Rise { base: 0.0, top: 1.0 }, span),
        check(Measure::Fall { base: 0.0, top: 1.0 }, span),
        check(Measure::Width { threshold, high: true }, When::Trailing { window: 5.0, after: 0.0 }),
        check(Measure::EdgeCount { threshold, slope: Slope::Either }, When::Trailing { window: 5.0, after: 0.0 }),
    ], &index);
    assert!(diags.is_empty(), "transition checks resolve");
    let rc: f64 = 0.7;
    for i in 0..=2000 {
        let t = i as f64 * 0.01;
        let y = if t <= 7.0 { 1.0 - (-t / rc).exp() } else { (-(t - 7.0) / rc).exp() };
        checks.step(&Sample { t, net_v: &[y], pin_i: &[], part_p: &[] });
    }
    for state in &checks.states()[..2] {
        assert!((state.value - rc * 9.0_f64.ln()).abs() < 0.01, "RC 10%-90% time equals RC ln 9 within a step");
        assert_eq!(state.outcome(), Outcome::Pass, "complete RC transition passes");
    }
    assert_eq!(checks.states()[2].verdict, Verdict::Pending, "width becomes Pending when its completed pulse expires");
    assert_eq!(checks.states()[3].value, 0.0, "edge count expires every old edge");
}

#[test]
fn uart_text_bounds_record_byte_times_and_window_membership() {
    use fab_elec_bench::edges::Threshold;
    use fab_elec_bench::uart::{self, Parity, UartFormat};
    let net = NetRef { board: BoardId(0), net: NetId(0) };
    let mut index = ValueIndex::new(0);
    index.add_net(net);
    let format = UartFormat { baud: 115200.0, bits: 8, parity: Parity::None, stop: 1.0, invert: false, threshold: Threshold { level: 0.5, hysteresis: 0.1 } };
    let (t, y, _) = serial::record(format, &[79, 75, 33], None, None);
    let frames = uart::decode(&t, &y, format);
    let end = *t.last().unwrap();
    let span = When::Span { from: 0.0, to: end };
    let check = |measure, bound, when| Expect { name: "serial".into(), probe: Probe::V(net), measure, when, bound };
    let list = vec![
        check(Measure::Uart(format), Bound::TextEquals("OK!".into()), span),
        check(Measure::Uart(format), Bound::TextEquals("OX!".into()), span),
        check(Measure::Uart(format), Bound::TextContains("K!".into()), span),
        check(Measure::Uart(format), Bound::TextContains("missing".into()), span),
        check(Measure::Uart(format), Bound::TextEquals("OK".into()), span),
        check(Measure::Uart(format), Bound::TextEquals("OK!!".into()), span),
        check(Measure::Uart(format), Bound::TextEquals("".into()), When::Span { from: 0.0, to: 1.0 / format.baud }),
        check(Measure::Now, Bound::TextEquals("invalid".into()), span),
        check(Measure::Uart(format), Bound::Range { min: 0.0, max: 10.0 }, span),
        check(Measure::Uart(format), Bound::TextEquals("K!".into()), When::Trailing { window: end - frames[1].t0 + 0.1 / format.baud, after: 0.0 }),
    ];
    let (mut checks, diags) = Expectations::new(list, &index);
    assert_eq!(diags.len(), 2, "both incompatible bound/measure pairs produce errors");
    for (&t, &y) in t.iter().zip(&y) {
        checks.step(&Sample { t, net_v: &[y as f64], pin_i: &[], part_p: &[] });
        if t < end {
            assert_eq!(checks.states()[1].verdict, Verdict::Pending, "text spans are judged only at their end");
            assert_eq!(checks.states()[3].failed, None, "Contains cannot fail before judgment");
        }
        if t >= frames[1].t1 {
            assert_eq!(checks.states()[1].failed, Some((frames[1].t1, 2.0)), "Equals records the first wrong byte at its stop centre");
        }
    }
    let states = checks.states();
    for i in [0, 2] {
        assert_eq!(states[i].outcome(), Outcome::Pass, "matching text check {i} passes");
        assert_eq!(states[i].value, 3.0, "text value is decoded byte count");
    }
    assert_eq!(states[3].failed, Some((end, 3.0)), "Contains fails only at judgment");
    assert_eq!(states[4].failed, Some((frames[2].t1, 3.0)), "Equals reports the first excess byte");
    assert_eq!(states[5].failed, Some((end, 3.0)), "a matching but short prefix fails at judgment");
    for i in [6, 7, 8] {
        assert_eq!(states[i].outcome(), Outcome::NotJudged, "empty/invalid text check {i} is NotJudged");
    }
    assert_eq!(states[9].verdict, Verdict::Pass, "trailing text excludes frames starting before its left edge");
    assert_eq!(states[9].value, 2.0, "trailing count includes only retained frames");
    checks.reset();
    assert!(checks.states().iter().all(|s| s.failed.is_none() && s.outcome() == Outcome::NotJudged), "reset clears UART decoding and matching");
}

#[test]
fn delay_orders_threshold_times_before_hysteresis_confirmation() {
    use fab_elec_bench::edges::Threshold;
    use fab_elec_bench::measure::{self, Slope};
    let a = NetRef { board: BoardId(0), net: NetId(0) };
    let b = NetRef { net: NetId(1), ..a };
    let mut index = ValueIndex::new(0);
    index.add_net(a);
    index.add_net(b);
    let threshold = Threshold { level: 0.5, hysteresis: 0.2 };
    // A source confirmed late; a target confirmed late with sources on both
    // sides of it; and several source edges sharing their next target edge.
    for (source, target) in [
        (vec![0.0, 0.55, 0.56, 1.0, 1.0, 1.0, 1.0], vec![0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0]),
        (vec![0.0, 1.0, 0.0, 1.0, 0.0, 0.0, 0.0], vec![0.0, 0.0, 0.55, 0.56, 1.0, 0.0, 1.0]),
        (vec![0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0], vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0]),
    ] {
        let t: Vec<_> = (0..source.len()).map(|i| i as f64).collect();
        let from_edges = measure::edges(&t, &source, threshold);
        let to_edges = measure::edges(&t, &target, threshold);
        let expected = |from| {
            let delays: Vec<_> = from_edges.iter().filter(|edge| edge.dir == Slope::Rising && edge.t >= from)
                .filter_map(|edge| to_edges.iter().find(|to| to.dir == Slope::Rising && to.t >= edge.t).map(|to| to.t - edge.t)).collect();
            if delays.is_empty() { None } else { Some(delays.iter().sum::<f64>() / delays.len() as f64) }
        };
        let checks = [When::Span { from: 0.0, to: 6.0 }, When::Trailing { window: 5.0, after: 0.0 }].map(|when| Expect {
            name: "ordered delay".into(), probe: Probe::V(a), when,
            measure: Measure::Delay { threshold, from: Slope::Rising, to: Probe::V(b), to_slope: Slope::Rising },
            bound: Bound::Range { min: 0.0, max: 10.0 },
        });
        let (mut checks, diags) = Expectations::new(checks.to_vec(), &index);
        assert!(diags.is_empty(), "both delay probes resolve");
        for ((&t, &a), &b) in t.iter().zip(&source).zip(&target) {
            checks.step(&Sample { t, net_v: &[a as f64, b as f64], pin_i: &[], part_p: &[] });
        }
        for (i, from) in [0.0, 1.0].into_iter().enumerate() {
            if let Some(expected) = expected(from) {
                assert!((checks.states()[i].value - expected).abs() < 1e-14, "delays pair by threshold time even when confirmation order differs");
            } else {
                assert_eq!(checks.states()[i].verdict, Verdict::Pending, "expired source edges cannot contribute a delay");
            }
        }
    }
}
