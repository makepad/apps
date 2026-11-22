use std::sync::{Arc, atomic::AtomicBool, mpsc::channel};
use fab_elec_bench::source::*;
use fab_elec_bench::trigger::Slope;
use fab_elec_bench::expect::{Bound, Expect, Expectations, Measure, Probe as ExpectProbe, Sample, When};
use fab_elec_bench::edges::Threshold;
use fab_elec_bench::ValueIndex;
use fab_elec_core::ids::{BoardId, NetId, NetRef};
use fab_elec_view::instruments::tools::capture::{Capture, CaptureIntake, CaptureSpec, CaptureStore};

fn record(points: &[(f64, f32)], quantity: Quantity) -> Arc<Capture> {
    let spec = CaptureSpec { source: "sim".into(), name: "closed form".into(), span: (0.0, points.last().unwrap().0),
        trigger: "none".into(), qualifier: fab_elec_bench::trigger::Qualifier::None, channel: 0, on: "test".into(), level: 0.5, slope: Slope::Rising, holdoff: 0.0 };
    let resolved = Resolved { rate: 1.0 / (points[1].0 - points[0].0), depth: points.len() as u64, mode: Mode::Stream,
        channels: Vec::new(), trigger: TrigResolved::Software { over: Over::Stream }, drives: Vec::new(), notes: Vec::new(), restart: true };
    let (complete, completed) = channel();
    let mut intake = CaptureIntake::new("cap1".into(), spec, Side::Sim, resolved, None, complete, Arc::new(AtomicBool::new(false)))
        .with_labels(vec!["test lane".into()]);
    intake.begin(&Layout { generation: 1, clock: ClockKind::Simulated,
        channels: vec![ChannelInfo { id: ChanId(0), name: "test".into(), quantity }] });
    for &(t, y) in points { intake.point(t, &[y]); }
    intake.mark(points.last().unwrap().0, Mark::Solver);
    intake.idle();
    completed.try_recv().expect("idle publishes a full capture, including the final mark")
}
fn close(got: f64, want: f64, tolerance: f64, property: &str) {
    assert!((got - want).abs() <= tolerance, "{property}: got {got}, expected {want} within {tolerance}");
}
fn pwm(frequency: f64, high: usize, periods: usize) -> Vec<(f64, f32)> {
    (0..=periods * 1000).map(|i| (i as f64 / (frequency * 1000.0), if i % 1000 < high { 1.0 } else { 0.0 })).collect()
}

#[test]
fn closed_forms_have_shared_exact_measurements_and_activity() {
    let dc: Vec<_> = (0..=100).map(|i| (i as f64 * 1e-5, 3.25)).collect();
    let square = pwm(1000.0, 500, 12);
    let pwm = pwm(10000.0, 300, 12);
    let sine: Vec<_> = (0..=2000).map(|i| {
        let t = i as f64 / 100000.0;
        (t, (std::f64::consts::TAU * 1000.0 * t).sin() as f32)
    }).collect();
    let burst: Vec<_> = (0..=4400).map(|i| {
        let bit = i / 8;
        let active = (100..120).contains(&bit) || (300..320).contains(&bit);
        (i as f64 / (9600.0 * 8.0), if active && bit % 2 == 0 { 1.0 } else { 0.0 })
    }).collect();
    for (points, class, mean, rms, min, max) in [
        (&dc, "dc", 3.25, 3.25, 3.25, 3.25),
        (&square, "square", 0.5, 0.5_f64.sqrt(), 0.0, 1.0),
        (&pwm, "pwm", 0.3, 0.3_f64.sqrt(), 0.0, 1.0),
        (&sine, "sine", 0.0, 0.5_f64.sqrt(), -1.0, 1.0),
    ] {
        let capture = record(points, Quantity::Volt);
        let feature = &capture.features[0]; let index = &feature.index;
        let (from, to) = capture.span;
        assert_eq!(feature.class, class, "closed form gets its expected class");
        close(index.reading("min", from, to).unwrap().value, min, 1e-6, "minimum");
        close(index.reading("max", from, to).unwrap().value, max, 1e-6, "maximum");
        close(index.reading("mean", from, to).unwrap().value, mean, 1e-6, "time-weighted mean");
        close(index.reading("rms", from, to).unwrap().value, rms, 0.002, "piecewise-linear RMS within sample step");
        close(index.base, min, 0.02, "lower histogram mode");
        close(index.top, max, 0.02, "upper histogram mode");
        assert_eq!(capture.trace.len(), points.len(), "every point is recorded once");
        assert_eq!(capture.marks, vec![(to, Mark::Solver)], "mark at the window end is retained");
        assert_eq!(feature.segments.len(), 1, "uninterrupted signals have one segment");
        if class == "square" || class == "pwm" {
            let frequency = if class == "square" { 1000.0 } else { 10000.0 };
            close(index.reading("frequency", from, to).unwrap().value, frequency, frequency * 1e-6, "frequency to one part per million");
            close(index.reading("duty", from, to).unwrap().value, mean * 100.0, 1e-6, "complete-cycle duty");
            assert_eq!(index.edges.items.len(), 24, "twelve periods contain twenty-four edges");
        }
        let (min_pyr, max_pyr) = capture.minmax(0, 1, points.len() - 1).unwrap();
        close(min_pyr, min, 1e-6, "pyramid lower bound"); close(max_pyr, max, 1e-6, "pyramid upper bound");
    }
    let capture = record(&burst, Quantity::Volt);
    assert_eq!(capture.features[0].class, "bursts", "idle stretches split serial activity into bursts");
    let index = &capture.features[0].index;
    let (from, to) = capture.span;
    close(index.reading("min", from, to).unwrap().value, 0.0, 1e-12, "burst minimum");
    close(index.reading("max", from, to).unwrap().value, 1.0, 1e-12, "burst maximum");
    close(index.reading("mean", from, to).unwrap().value, 20.0 / 550.0, 1e-12, "burst time-weighted mean includes idle stretches");
    close(index.reading("rms", from, to).unwrap().value, ((20.0 - 20.0 / 24.0) / 550.0_f64).sqrt(), 1e-12, "burst RMS integrates the sampled edge ramps");
    close(index.reading("frequency", from, to).unwrap().value, 19.0 * 9600.0 / 218.0, 1e-6, "burst frequency includes complete cycles across idle stretches");
    close(index.reading("duty", from, to).unwrap().value, 100.0 * 19.0 / 218.0, 1e-6, "burst duty is the shared ratio of complete-cycle sums");
    assert_eq!(index.edges.items.len(), 40, "two ten-pulse bursts contain forty edges");
    close(index.base, 0.0, 1e-12, "burst histogram base"); close(index.top, 1.0, 1e-12, "burst histogram top");
    let segments = &capture.features[0].segments;
    assert_eq!(segments.len(), 5, "two bursts have initial, intervening and final idle segments");
    assert_eq!(segments[0].what, "idle low", "initial idle polarity");
    assert!(segments[1].what.starts_with("burst ") && segments[1].what.ends_with(" baud"), "burst activity names its bit rate");
    let baud = segments[1].what.split_whitespace().nth(1).unwrap().parse::<f64>().unwrap();
    close(baud, 9600.0, 9600.0 * 1e-6, "shortest edge intervals recover the burst bit rate");
    assert_eq!(segments[4].what, "idle low", "final idle polarity");
    assert_eq!(record(&dc, Quantity::Logic).features[0].class, "idle", "logic without edges is idle");
}

#[test]
fn capture_measurements_match_expectations_and_store_ids_never_recur() {
    let points = pwm(1000.0, 300, 10);
    let capture = record(&points, Quantity::Volt);
    let net = NetRef { board: BoardId(0), net: NetId(0) };
    let mut values = ValueIndex::new(1); values.add_net(net);
    let threshold = Threshold { level: 0.5, hysteresis: 0.1 };
    let measures = [("frequency", Measure::Frequency(threshold)), ("duty", Measure::Duty(threshold)),
        ("width", Measure::Width { threshold, high: true }), ("rise", Measure::Rise { base: 0.0, top: 1.0 }), ("mean", Measure::Mean)];
    let checks = measures.iter().map(|(name, measure)| Expect { name: (*name).into(), probe: ExpectProbe::V(net), measure: measure.clone(),
        when: When::Span { from: 0.0, to: points.last().unwrap().0 }, bound: Bound::Range { min: -1e9, max: 1e9 } }).collect();
    let (mut expects, diags) = Expectations::new(checks, &values);
    assert!(diags.is_empty(), "fixed test probe resolves in the expectation machine");
    for &(t, y) in &points { expects.step(&Sample { t, net_v: &[y as f64], pin_i: &[], part_p: &[] }); }
    for ((name, _), state) in measures.iter().zip(expects.states()) {
        let got = capture.features[0].index.reading(name, capture.span.0, capture.span.1).unwrap().value;
        assert_eq!(got, state.value, "{name}: the capture index and Expect use the same accumulation order");
    }
    let mut store = CaptureStore::default();
    assert_eq!(store.reserve(), "cap1", "first id is reserved before acquisition");
    store.insert(capture.clone());
    assert!(store.drop_capture("cap1"), "drop releases the store's capture");
    assert!(store.get("cap1").is_none(), "dropped capture is unknown");
    assert_eq!(store.reserve(), "cap2", "dropped ids are never reused");
    let bytes = capture.bytes();
    assert_eq!(&bytes[..8], b"FABTRACE", "save format has a distinct magic");
    assert_eq!(&bytes[8..12], &2_u32.to_le_bytes(), "save format is explicitly versioned");
    // read back: the same channels (nets and display names), times and samples; a cut file
    // fails, never reads short; another version is refused, never guessed at
    let back = fab_elec_view::instruments::tools::capture::read_trace(&bytes, 7).expect("a saved capture reads back");
    assert_eq!(back.layout, capture.layout, "the layout reads back");
    assert_eq!((back.labels.as_slice(), capture.labels.as_slice()), (&["test lane".to_string()][..], &["test lane".to_string()][..]), "each channel's display name reads back");
    let mut older = bytes.clone();
    older[8..12].copy_from_slice(&1_u32.to_le_bytes());
    assert!(fab_elec_view::instruments::tools::capture::read_trace(&older, 7).is_err_and(|why| why.contains("version 1")), "another version is refused, saying which");
    assert!(back.store.generation == 7 && back.store.len() == capture.trace.len(), "the store holds every sample at the given generation");
    assert!((0..back.store.len()).all(|i| back.store.time.at(i) == capture.trace.time.at(i)
        && back.store.signals[0].at(i).to_bits() == capture.trace.signals[0].at(i).to_bits()), "times and samples read back exactly");
    assert_eq!((back.t_trigger, back.gaps.len(), back.marks.len()), (capture.t_trigger, capture.gaps.len(), capture.marks.len()), "trigger, gaps and marks read back");
    assert!(fab_elec_view::instruments::tools::capture::read_trace(&bytes[..bytes.len() - 1], 7).is_err(), "a cut trace fails");
}

#[test]
fn boxed_simulator_current_pretrigger_marks_and_gap_boundaries() {
    use fab_elec_sim::{Sink, stamp::{UnknownInfo, UnkKind}};
    use fab_elec_view::instruments::acquire::{NetProbe, SimChannel, SimIntake};
    let spec = CaptureSpec { source: "sim".into(), name: "branch".into(), span: (-0.0005, 0.0005),
        trigger: "edge".into(), qualifier: fab_elec_bench::trigger::Qualifier::None, channel: 0, on: "V1#branch".into(), level: 0.5, slope: Slope::Rising, holdoff: 0.0 };
    let resolved = Resolved { rate: 1000.0, depth: 2, mode: Mode::Stream, channels: Vec::new(),
        trigger: TrigResolved::Software { over: Over::Stream }, drives: Vec::new(), notes: Vec::new(), restart: true };
    let (complete, completed) = channel();
    let intake: Box<dyn Intake> = Box::new(CaptureIntake::new("cap1".into(), spec.clone(), Side::Sim, resolved.clone(), None, complete, Arc::new(AtomicBool::new(false))));
    let mut sink = SimIntake::new(vec![SimChannel {
        info: ChannelInfo { id: ChanId(0), name: "source current".into(), quantity: Quantity::Amp },
        probe: NetProbe { pos: "V1#branch".into(), neg: None, scale: 1.0 },
    }], intake);
    sink.begin(&[UnknownInfo { name: "0".into(), kind: UnkKind::Voltage },
        UnknownInfo { name: "VOUT".into(), kind: UnkKind::Voltage }, UnknownInfo { name: "V1#branch".into(), kind: UnkKind::Current }]);
    sink.point(0.0, None, &[0.0, 9.0, 0.0]);
    sink.point(0.001, None, &[0.0, 9.0, 1.0]);
    sink.mark(0.001); sink.idle();
    let capture = completed.try_recv().expect("boxed intake completes through the existing solver Sink adapter");
    assert_eq!(capture.side, Side::Sim, "simulator capture preserves its source side");
    assert_eq!(capture.t_trigger, 0.0005, "branch current crosses at the interpolated trigger instant");
    assert_eq!(capture.span, (0.0, 0.001), "pretrigger history and posttrigger samples share one window");
    assert_eq!(capture.trace.signals[0].at(1), 1.0, "branch current is read from the current unknown, never node voltage");
    assert_eq!(capture.marks, vec![(0.001, Mark::Solver)], "solver mark on the final boundary survives");
    let (complete, completed) = channel();
    let mut spec = spec; spec.trigger = "none".into(); spec.span = (0.0, 2.0);
    let mut intake = CaptureIntake::new("cap2".into(), spec, Side::Real, resolved, None, complete, Arc::new(AtomicBool::new(false)));
    intake.begin(&capture.layout);
    intake.point(0.0, &[0.0]); intake.point(0.5, &[0.0]);
    intake.gap(0.5, 1.5, Gap::Overrun);
    intake.point(1.5, &[1.0]); intake.point(2.0, &[1.0]); intake.idle();
    let capture = completed.try_recv().expect("full post-gap window completes");
    assert_eq!(capture.side, Side::Real, "device capture preserves the supplied source side independently of its clock");
    assert_eq!(capture.gaps, vec![(0.5, 1.5, Gap::Overrun)], "missing source intervals retain their reason");
    assert!(capture.features[0].index.edges.items.is_empty(), "gap endpoints do not invent an edge");
    assert!(capture.features[0].index.reading("mean", 0.0, 2.0).is_none(), "mean never interpolates across missing time");
    assert_eq!(capture.features[0].index.reading("mean", 1.5, 2.0).unwrap().value, 1.0, "continuous spans after a gap remain measurable");
}
