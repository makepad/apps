//! Event spread, capture features and the precision of the lane's unchanged answer strings.
use fab_elec_bench::measure_index::RecordIndex;
use fab_elec_view::instruments::{
    axis::si_label,
    scope::{answer_digits, answer_stats_text, expect_mark, lane_answers, Domain, LaneExpect, LaneSource, Measurement, ScopeAction, ScopeSettings, SignalLane},
    trend::TrendQuantity,
    scrub::measurement_rows,
    simfeed::{lane_features, FeatureKind},
};
use makepad_widgets_core::*;
use std::sync::Arc;

const PERIOD: f64 = 100e-6;
const EDGE: f64 = 1e-6;
const STEP: f64 = 50e-9;
const DELAY: f64 = 10e-6;
const CYCLES: usize = 20;

fn pwm(jitter: f64) -> (Vec<f64>, Vec<f32>, Vec<f64>) {
    let mut starts = vec![DELAY];
    for cycle in 0..CYCLES {
        starts.push(starts[cycle] + PERIOD + if cycle % 2 == 0 { jitter } else { -jitter });
    }
    let end = starts[CYCLES] + PERIOD * 0.5;
    let t: Vec<_> = (0..=(end / STEP).ceil() as usize).map(|i| i as f64 * STEP).collect();
    let y = t.iter().map(|&t| {
        let after = starts.partition_point(|&start| start <= t);
        if after == 0 { return 0.0; }
        let cycle = after - 1;
        let period = starts.get(cycle + 1).map_or(PERIOD, |&next| next - starts[cycle]);
        let phase = t - starts[cycle];
        let level = if phase < EDGE { phase / EDGE }
            else if phase < period * 0.3 { 1.0 }
            else { (1.0 - (phase - period * 0.3) / EDGE).max(0.0) };
        (4.0 * level) as f32
    }).collect();
    (t, y, starts)
}

fn row<'a>(rows: &'a [Measurement], name: &str) -> &'a Measurement {
    rows.iter().find(|row| row.name == name).expect("requested PWM measurement exists")
}

#[test]
fn complete_pwm_events_spread_and_every_capture_feature() {
    let (t, y, starts) = pwm(0.0);
    let rows = measurement_rows(&t, &y, 0.1);
    let frequency = row(&rows, "freq");
    assert!((frequency.value / 10_000.0 - 1.0).abs() < 1e-6, "clean frequency matches 10 kHz within one part per million");
    assert_eq!(frequency.n, CYCLES as u64, "frequency counts every complete rising-to-rising cycle");
    assert!(frequency.sigma < 1e-3, "clean frequency has negligible population deviation");
    let duty = row(&rows, "duty");
    assert!((duty.value - 30.0).abs() < 1e-6, "duty is thirty percent, with no second percent conversion");
    assert_eq!(duty.n, CYCLES as u64, "duty counts the same complete cycles as frequency");
    assert!(duty.sigma < 1e-5, "clean duty has negligible population deviation in percent");
    let width = row(&rows, "width");
    assert!((width.value - PERIOD * 0.3).abs() <= STEP, "the pinned width answer measures the high pulse");
    assert_eq!(width.n, (CYCLES + 1) as u64, "width includes the last complete high pulse before its period closes");
    let rise = row(&rows, "rise");
    assert!((rise.value / (0.8 * EDGE) - 1.0).abs() < 1e-6, "rise duration spans ten to ninety percent");
    assert_eq!(rise.n, (CYCLES + 1) as u64, "rise counts transitions including the last incomplete cycle");
    for name in ["top", "base", "Vpp", "max", "min", "mean"] {
        let amplitude = row(&rows, name);
        assert_eq!(amplitude.n, y.len() as u64, "amplitude sample count: {name}");
        assert_eq!(amplitude.sigma, 0.0, "amplitude deviation stays zero: {name}");
        assert_eq!(amplitude.step, 0.0, "unresolved sample resolution stays exact: {name}");
    }
    let index = RecordIndex::new(Arc::from(t.as_slice()), Arc::from(y.as_slice()));
    let trigger = DELAY + EDGE * 0.5;
    let features = lane_features(&t, &y, &index, trigger);
    assert!(features.windows(2).all(|p| p[0].t <= p[1].t), "capture features remain ordered within a lane");
    for (kind, offset) in [(FeatureKind::Rise, 0.0), (FeatureKind::Fall, PERIOD * 0.3)] {
        let edges: Vec<_> = features.iter().filter(|f| f.kind == kind).collect();
        assert_eq!(edges.len(), CYCLES + 1, "feature list retains every {kind:?} without a count cap");
        for (feature, start) in edges.into_iter().zip(&starts) {
            let expected = start + offset + EDGE * 0.5 - trigger;
            assert!((feature.t - expected).abs() <= STEP, "{kind:?} feature uses the closed-form fifty percent time relative to the trigger");
            assert!((feature.v - 2.0).abs() < 1e-6, "{kind:?} feature uses the mid-swing level");
            let [ten, fifty, ninety] = feature.crossings.expect("transition feature includes all three crossings");
            assert_eq!(feature.t, fifty, "{kind:?} feature time equals its fifty percent crossing");
            assert!(if kind == FeatureKind::Rise { ten < fifty && fifty < ninety } else { ninety < fifty && fifty < ten }, "{kind:?} crossings retain ten/fifty/ninety amplitude order");
            assert!(((ninety - ten).abs() - 0.8 * EDGE).abs() <= STEP, "{kind:?} feature crossing span agrees with the indexed transition");
        }
    }
    for (kind, count, level) in [(FeatureKind::Top, CYCLES + 1, 4.0), (FeatureKind::Base, CYCLES + 2, 0.0)] {
        let levels: Vec<_> = features.iter().filter(|f| f.kind == kind).collect();
        assert_eq!(levels.len(), count, "{kind:?} features include every stretch and partial boundary stretches");
        assert!(levels.iter().all(|f| f.crossings.is_none() && (f.v - level).abs() < 0.01), "{kind:?} features use stretch means with no crossings");
    }
    for (top, start) in features.iter().filter(|f| f.kind == FeatureKind::Top).zip(&starts) {
        assert!((top.t - (start + EDGE * 0.5 + PERIOD * 0.15 - trigger)).abs() <= STEP, "top feature sits at the middle of its high stretch");
        let mean = 4.0 - 0.04 * EDGE / (PERIOD * 0.3 - EDGE * 0.8);
        assert!((top.v - mean).abs() < 1e-6, "top feature integrates the two partial ramps in its mean level");
    }
    let jitter = 2e-6;
    let (t, y, _) = pwm(jitter);
    let rows = measurement_rows(&t, &y, 0.1);
    let frequency = row(&rows, "freq");
    assert_eq!(frequency.n, CYCLES as u64, "jittered frequency still counts complete cycles");
    let measured_period_sigma = frequency.sigma / frequency.value.powi(2);
    assert!((measured_period_sigma / jitter - 1.0).abs() < 0.1, "frequency population spread recovers the injected period deviation within ten percent");
}

#[test]
fn answer_precision_preserves_text_and_dims_only_uncertain_digits() {
    for (value, unit) in [(1.234, "Hz"), (-3.299, "V"), (0.0, "V"), (999.96, "Hz"), (0.125, "%")] {
        let m = Measurement::exact(0, "value", value, unit);
        let label = si_label(value, unit);
        let number = label.split_once(' ').expect("SI label includes its unit").0;
        assert_eq!(answer_digits(&m), (number.into(), String::new()), "exact values keep every formatted digit bright: {value} {unit}");
    }
    let frequency = Measurement { sigma: 0.1, n: 100, ..Measurement::exact(0, "freq", 1.234, "Hz") };
    assert_eq!(answer_digits(&frequency), ("1.23".into(), "4".into()), "standard error of one hundredth Hz keeps hundredths bright");
    let voltage = Measurement { step: 0.03125, ..Measurement::exact(0, "mean", 3.299, "V") };
    assert_eq!(answer_digits(&voltage), ("3.29".into(), "9".into()), "31.25 mV steps dim digits finer than the roughly 16 mV uncertainty");
    let prefixed = Measurement { step: 0.00003125, ..Measurement::exact(0, "mean", -0.003299, "V") };
    assert_eq!(answer_digits(&prefixed), ("−3.29".into(), "9".into()), "precision follows SI prefix scaling and preserves the typographic sign");
    let answers = lane_answers(&[voltage], &[], 0, Domain::Time, true, false, false);
    assert_eq!(answers.items[0].value, "3.299", "dimming preserves the complete layout string");
    assert_eq!(answers.items[0].dim, vec![4..5], "answer stores the uncertain digit range for drawing");
    assert_eq!(answers.text, "3.299\u{2009}V", "accessible answer retains the full formatted number");
}

#[test]
fn answer_statistics_show_spread_count_exactness_and_source_resolution() {
    let frequency = Measurement { sigma: 12.345, n: 1_482, ..Measurement::exact(0, "freq", 10_000.0, "Hz") };
    let simulator = Measurement { n: 1_482, ..Measurement::exact(0, "mean", 3.3, "V") };
    let voltage = Measurement { sigma: 0.012345, step: 0.03125, ..simulator.clone() };
    assert_eq!(answer_stats_text(&frequency), "sigma 12 Hz   n 1\u{2009}482", "frequency reports population sigma to two significant digits and groups 1482 with a thin space");
    assert_eq!(answer_stats_text(&simulator), "exact", "zero spread and zero source step remain exact across many events");
    assert_eq!(answer_stats_text(&voltage), "sigma 12 mV   n 1\u{2009}482   step 31.25 mV", "stepped voltage reports SI-scaled spread and preserves all source resolution digits");
    assert_eq!(answer_stats_text(&Measurement { n: 1, ..frequency.clone() }), "exact", "a single event is exact without a population spread report");
    assert_eq!(answer_stats_text(&Measurement { n: 1, ..voltage.clone() }), "exact   step 31.25 mV", "single-event exactness still discloses the source step");
    assert_eq!(answer_stats_text(&Measurement { sigma: 0.0, ..voltage }), "sigma 0 V   n 1\u{2009}482   step 31.25 mV", "a stepped source with zero measured spread is not presented as exact");
    assert_eq!(answer_stats_text(&Measurement { sigma: 999.6, n: 1_482_345, ..frequency }), "sigma 1 kHz   n 1\u{2009}482\u{2009}345", "sigma rounding carries into the SI prefix and event counts group every three digits");
}

#[test]
fn expectation_bounds_are_inclusive_and_never_judge_missing_numbers() {
    let rise = Measurement::exact(0, "rise", 221e-12, "s");
    let voltage = Measurement::exact(0, "mean", 2.9, "V");
    let frequency = Measurement::exact(0, "freq", 10_000.0, "Hz");
    assert_eq!(expect_mark(rise.value, &rise.unit, None, Some(1e-9)), Some((true, "ok (< 1 ns)".into())), "rise below its upper bound passes with an SI time bound");
    assert_eq!(expect_mark(voltage.value, &voltage.unit, Some(3.0), None), Some((false, "fails (> 3 V)".into())), "voltage below its lower bound fails with a voltage bound");
    assert_eq!(expect_mark(frequency.value, &frequency.unit, Some(9900.0), Some(10100.0)), Some((true, "ok (9.9 kHz to 10.1 kHz)".into())), "frequency inside both bounds passes with each bound SI formatted");
    for edge in [9900.0, 10100.0] {
        assert_eq!(expect_mark(edge, "Hz", Some(9900.0), Some(10100.0)).map(|mark| mark.0), Some(true), "both ends of a two-sided bound are inclusive: {edge}");
    }
    assert_eq!(expect_mark(f64::NAN, "V", Some(3.0), None), None, "NaN is never judged as passing or failing");
    assert_eq!(expect_mark(f64::INFINITY, "V", Some(3.0), None), None, "infinity is never judged as a measurement");
    assert_eq!(expect_mark(voltage.value, &voltage.unit, None, None), None, "an answer with no bound has no verdict");
    assert_eq!(expect_mark(voltage.value, &voltage.unit, Some(f64::NAN), None), None, "an invalid bound never produces a misleading verdict");
}

fn signal_lane(cx: &mut Cx) -> SignalLane {
    cx.with_vm(|vm| {
        makepad_platform::script::script_mod(vm);
        makepad_draw::script_mod(vm);
        makepad_widgets_core::script_mod_with(vm, Default::default(), &[makepad_widgets_pickers::pickers_mod]);
        fab_elec_view::live::script_mod(vm);
        fab_elec_view::trace::script_mod(vm);
        fab_elec_view::instruments::script_mod(vm);
        let value = vm.eval(script! {
            use mod.prelude.widgets_internal.*
            use mod.widgets.*
            SignalLane {}
        });
        SignalLane::script_from_value(vm, value)
    })
}

#[test]
fn lane_expectations_follow_selected_answers_and_preserve_not_judged() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut lane = signal_lane(&mut cx);
    let mut rows = vec![
        Measurement::exact(0, "freq", 10_000.0, "Hz"),
        Measurement::exact(0, "duty", 50.0, "%"),
        Measurement::exact(0, "Vpp", 2.9, "V"),
        Measurement::exact(0, "rise", 221e-12, "s"),
        Measurement::exact(0, "mean", 3.3, "V"),
    ];
    let expects = vec![
        LaneExpect { name: "edge".into(), net: "CLK".into(), answer: "rise".into(), min: None, max: Some(1e-9) },
        LaneExpect { name: "level".into(), net: "CLK".into(), answer: "Vpp".into(), min: Some(3.0), max: None },
        LaneExpect { name: "average".into(), net: "CLK".into(), answer: "mean".into(), min: Some(3.0), max: None },
        LaneExpect { name: "absent".into(), net: "CLK".into(), answer: "fall".into(), min: None, max: Some(1e-9) },
    ];
    lane.set_expects(&mut cx, &expects);
    lane.set_lane_measurements(&mut cx, &rows, 0);
    let answers = lane.source_answers();
    assert_eq!(answers.items[3].marks, vec![(true, "ok (< 1 ns)".into())], "rise's mark belongs to the selected rise answer");
    assert_eq!(answers.items[2].marks, vec![(false, "fails (> 3 V)".into())], "a failing bound stays attached to its displayed voltage");
    assert!(answers.text.contains("rise 221\u{2009}ps ok, under 1 ns"), "accessible speech includes the verdict and the bound in words");
    assert_eq!(answers.items[4].marks, vec![(true, "ok (> 3 V)".into())], "an expectation appends and judges its measurement even outside the automatic set");
    assert!(answers.text.contains("fall not judged"), "an absent measurement remains not judged");
    assert_eq!(answers.items.len(), 5, "the expected mean joins the headline; unknown measurements remain not judged");
    lane.freeze_ghost(&mut cx);
    rows[3].value = 2e-9;
    lane.set_lane_measurements(&mut cx, &rows, 0);
    assert_eq!(lane.source_answers().items[3].marks, vec![(false, "fails (< 1 ns)".into())], "judging follows the current rise, never its passing ghost");
    lane.set_collapsed(&mut cx, true);
    let meter = lane.source_answers();
    assert_eq!(meter.items[0].marks, vec![(true, "ok (> 3 V)".into())], "the meter judges mean once that measurement is selected");
    assert_eq!(meter.items[2].marks, vec![(false, "fails (< 1 ns)".into())], "collapse keeps the expected rise after the automatic meter choices");
    lane.set_expects(&mut cx, &[]);
    assert_eq!(lane.source_answers(), lane_answers(&rows, &[], 0, Domain::Time, true, false, false), "removing expectations clears both marks and not-judged rows without changing measurements");
}

#[test]
fn removing_answers_preserves_order_unpins_and_refuses_expectations() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut lane = signal_lane(&mut cx);
    let rows: Vec<_> = [("freq", 10_000.0, "Hz"), ("duty", 50.0, "%"), ("Vpp", 3.3, "V"), ("rise", 2e-9, "s")]
        .into_iter().map(|(name, value, unit)| Measurement::exact(0, name, value, unit)).collect();
    lane.set_lane_measurements(&mut cx, &rows, 0);
    let automatic = lane.source_answers();
    assert_eq!(lane.settings().answers, None, "the clock starts with automatic answers");
    let actions = cx.capture_actions(|cx| {
        assert!(lane.remove_answer(cx, "duty"), "an automatic answer can be removed");
    });
    assert_eq!(lane.settings().answers, Some(vec!["freq".into(), "Vpp".into(), "rise".into()]), "removing duty copies the automatic set into an explicit list in order");
    assert_eq!(lane.source_answers().items, vec![automatic.items[0].clone(), automatic.items[2].clone(), automatic.items[3].clone()], "removal preserves the other answers and their order");
    assert_eq!(actions.filter_widget_actions_cast::<ScopeAction>(lane.widget_uid()).collect::<Vec<_>>(), vec![ScopeAction::Reconfigure(lane.settings().clone())], "removal publishes the explicit settings without clearing acquisition");

    lane.set_pins(&mut cx, vec![TrendQuantity::Freq]);
    let actions = cx.capture_actions(|cx| {
        assert!(lane.remove_answer(cx, "freq"), "a pinned answer can be removed");
    });
    let actions = actions.filter_widget_actions_cast::<ScopeAction>(lane.widget_uid()).collect::<Vec<_>>();
    assert_eq!(actions, vec![ScopeAction::Pin { name: "freq", on: false }, ScopeAction::Reconfigure(lane.settings().clone())], "removing pinned freq emits its unpin and settings actions");
    assert_eq!(lane.settings().answers, Some(vec!["Vpp".into(), "rise".into()]), "the pinned answer is removed from the explicit list");
    assert_eq!(lane.source_answers().items, automatic.items[2..].to_vec(), "freq is actually unpinned and cannot reappear as a required answer");

    lane.set_pins(&mut cx, vec![TrendQuantity::Freq]);
    let actions = cx.capture_actions(|cx| {
        assert!(lane.remove_answer(cx, "freq"), "a pin outside the explicit list can also be removed");
    });
    assert_eq!(actions.filter_widget_actions_cast::<ScopeAction>(lane.widget_uid()).collect::<Vec<_>>(), vec![ScopeAction::Pin { name: "freq", on: false }], "removing an unselected pin emits only its unpin");
    assert_eq!(lane.source_answers().items, automatic.items[2..].to_vec(), "removing an unselected pin never toggles it back on");

    lane.set_pins(&mut cx, vec![TrendQuantity::Vpp]);
    lane.set_expects(&mut cx, &[LaneExpect { name: "swing".into(), net: "CLK".into(), answer: "Vpp".into(), min: Some(3.0), max: None }]);
    let before = (lane.settings().clone(), lane.source_answers());
    let actions = cx.capture_actions(|cx| {
        assert!(!lane.remove_answer(cx, "Vpp"), "an expected answer refuses removal even when pinned");
    });
    assert_eq!((lane.settings().clone(), lane.source_answers()), before, "refused removal changes neither settings nor expected answers");
    assert_eq!(actions.filter_widget_actions_cast::<ScopeAction>(lane.widget_uid()).count(), 0, "refused removal emits no unpin or settings action");
    lane.set_expects(&mut cx, &[]);
    let actions = cx.capture_actions(|cx| { lane.remove_answer(cx, "Vpp"); });
    assert!(actions.filter_widget_actions_cast::<ScopeAction>(lane.widget_uid()).any(|action| action == ScopeAction::Pin { name: "Vpp", on: false }), "refused removal preserved the pin until its expectation was removed");
}

#[test]
fn measure_choices_keep_order_required_answers_and_settings_round_trip() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut lane = signal_lane(&mut cx);
    let rows: Vec<_> = [("freq", 10_000.0, "Hz"), ("duty", 50.0, "%"), ("Vpp", 3.3, "V"),
        ("rise", 2e-9, "s"), ("width", 50e-6, "s"), ("mean", 1.65, "V")]
        .into_iter().map(|(name, value, unit)| Measurement::exact(0, name, value, unit)).collect();
    lane.set_lane_measurements(&mut cx, &rows, 0);
    let automatic = lane.source_answers();
    let names = ["freq", "duty", "Vpp", "rise"];
    let mut settings = ScopeSettings::default();
    assert!(settings.toggle_answer("unknown", &names).is_empty(), "an unknown name does not leave automatic mode");
    assert_eq!(settings.answers, None);
    let actions = settings.toggle_answer("width", &names);
    assert_eq!(settings.answers, Some(vec!["freq".into(), "duty".into(), "Vpp".into(), "rise".into(), "width".into()]), "the first toggle copies the automatic four and appends width");
    assert_eq!(actions, vec![ScopeAction::Reconfigure(settings.clone())], "display choices reconfigure without clearing acquisition");
    let ScopeAction::Reconfigure(snapshot) = &actions[0] else { panic!("selection publishes settings") };
    lane.set_settings(&mut cx, snapshot.clone());
    assert_eq!(lane.settings(), snapshot, "the settings round trip retains the explicit list");
    let expanded = lane.source_answers();
    assert_eq!(&expanded.items[..4], automatic.items.as_slice(), "adding width leaves the automatic four in place");
    assert_eq!(expanded.items[4].name, "width ");
    assert_eq!(expanded.items[4].value, "50");
    let before = settings.clone();
    assert!(settings.toggle_answer("unknown", &names).is_empty());
    assert_eq!(settings, before, "an unknown name leaves an explicit selection unchanged too");
    settings.toggle_answer("duty", &names);
    lane.set_settings(&mut cx, settings.clone());
    assert_eq!(&lane.source_answers().items[..3], &[automatic.items[0].clone(), automatic.items[2].clone(), automatic.items[3].clone()], "removing duty keeps freq, Vpp and rise in order");
    lane.set_collapsed(&mut cx, true);
    assert_eq!(lane.source_answers(), lane_answers(&rows, &[], 0, Domain::Time, true, false, false), "the meter ignores the list and keeps mean and ripple");
    assert_eq!(lane.settings().answers, settings.answers, "collapse retains the expanded selection");
    lane.set_collapsed(&mut cx, false);
    assert_eq!(lane.source_answers().items.last().unwrap().name, "width ", "expanding restores the list");
    settings.set_automatic_answers();
    lane.set_settings(&mut cx, settings.clone());
    assert_eq!(lane.source_answers(), automatic, "automatic restores today's set");
    assert!(settings.set_automatic_answers().is_empty(), "returning to automatic twice is a no-op");

    lane.set_pins(&mut cx, vec![TrendQuantity::Freq, TrendQuantity::Mean]);
    settings.toggle_answer("freq", &names);
    lane.set_settings(&mut cx, settings.clone());
    let pinned = lane.source_answers();
    assert_eq!(&pinned.items[..3], &automatic.items[1..], "unselected pins do not reorder the explicit answers");
    assert_eq!(pinned.items[3], automatic.items[0], "pinned freq remains after being toggled off");
    assert_eq!(pinned.items[4].value, "1.65", "pins outside the former width/freq special case also remain");
    lane.set_expects(&mut cx, &[LaneExpect { name: "pulse".into(), net: "CLK".into(), answer: "width".into(), min: None, max: Some(60e-6) }]);
    let expected = lane.source_answers();
    assert_eq!(expected.items[5].name, "width ", "expected names append after explicit names and pins");
    assert!(expected.items[5].marks[0].0, "the appended expectation is judged from its own measurement");
    lane.set_lane_measurements(&mut cx, &rows[1..], 0);
    assert!(lane.source_answers().items.iter().any(|answer| answer.name == "freq " && answer.value == "-"), "a pin stays visible when its measurement is missing");
    lane.set_pins(&mut cx, Vec::new());
    lane.set_expects(&mut cx, &[]);

    use fab_elec_bench::source::Side;
    lane.set_sources(&[LaneSource { channel: None, side: Side::Real, name: "real".into() }, LaneSource { channel: None, side: Side::Sim, name: "sim".into() }]);
    lane.set_source_measurements(&mut cx, 0, &rows);
    lane.set_source_measurements(&mut cx, 1, &rows);
    settings.answers = Some(vec!["width".into(), "freq".into()]);
    lane.set_settings(&mut cx, settings.clone());
    let twin = lane.source_answers();
    assert_eq!(twin.items.len(), 2, "twins honor the same explicit selection");
    assert_eq!(twin.items[0].name, "width ");
    assert!(twin.items[0].value.contains("real") && twin.items[0].value.ends_with("±0 s"), "selected width compares both time measurements");
    assert!(twin.items[1].value.ends_with("±0 %"), "selected frequency keeps the existing ratio comparison");
    settings.domain = Domain::Fft;
    settings.answers = Some(vec!["H5".into(), "width".into(), "peak f".into(), "H3".into()]);
    lane.set_settings(&mut cx, settings);
    let spectral = [Measurement::exact(0, "peak f", 10_000.0, "Hz"), Measurement::exact(0, "H3", -9.5, "dBc"), Measurement::exact(0, "H5", -14.0, "dBc")];
    lane.set_source_measurements_with_spectrum(&mut cx, 0, &rows, &spectral);
    lane.set_source_measurements(&mut cx, 1, &rows);
    let mixed = lane.source_answers();
    assert_eq!(mixed.items.iter().map(|answer| answer.name.as_str()).collect::<Vec<_>>(), ["H5 ", "width ", "", "H3 "], "interleaved time and spectrum choices preserve the exact list order");
    assert_eq!(mixed.items[0].value, "-14.0", "spectral choices use the primary transform even without a secondary spectrum");
}
