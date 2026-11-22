use fab_elec_sim::fft::Window;
use fab_elec_view::instruments::{
    phosphor::Hits,
    scope::{lane_answers, Domain, FftSettings},
    spectrum::{fft_size, SpectrumMeasurements, SpectrumStroke},
};
use std::f64::consts::TAU;

fn measure(window: Window, sample: impl Fn(usize, usize) -> f32) -> SpectrumMeasurements {
    let plot = (512, 64);
    let n = fft_size(plot.0);
    let mut stroke = SpectrumStroke::default();
    let mut hits = Hits::new(plot.0, plot.1);
    stroke.begin(0.0, 1.0, plot, FftSettings { window, ..FftSettings::default() });
    stroke.point(0.0, sample(0, n));
    for i in 1..=n {
        stroke.segment((i - 1) as f64 / n as f64, sample(i - 1, n), i as f64 / n as f64, sample(i, n));
    }
    stroke.finish(&mut hits);
    stroke.measurements().expect("a complete finite record publishes spectrum measurements")
}

#[test]
fn square_harmonics_keep_window_gain_frequency_and_answer_order() {
    for window in [Window::Rect, Window::Hann, Window::Hamming, Window::BlackmanHarris, Window::FlatTop] {
        // Sixteen exact periods, with an equal number of positive and negative samples.
        let spectrum = measure(window, |i, n| if i % (n / 16) < n / 32 { 1.0 } else { -1.0 });
        let df = spectrum.rate / spectrum.n as f64;
        let h3 = spectrum.harmonics[1].expect("a square wave resolves H3");
        let h5 = spectrum.harmonics[3].expect("a square wave resolves H5");
        assert!((h3.1 + 9.54).abs() < 0.3, "{window:?}: square H3 is -9.54 dBc, got {}", h3.1);
        assert!((h5.1 + 13.98).abs() < 0.3, "{window:?}: square H5 is -13.98 dBc, got {}", h5.1);
        assert!((h3.0 - 3.0 * spectrum.peak_f).abs() <= df, "{window:?}: H3 frequency is three times the fundamental within one bin");
        for i in [0, 2] {
            assert!(spectrum.harmonics[i].is_none_or(|h| h.1 < -60.0), "{window:?}: square H{} is absent or below -60 dBc", i + 2);
        }
        let rows = spectrum.rows();
        for (name, value) in [("H3", h3.1), ("H5", h5.1)] {
            let row = rows.iter().find(|row| row.name == name).expect("every resolved harmonic is in the catalog");
            assert_eq!((row.value, &*row.unit), (value, "dBc"), "{window:?}: {name} keeps its measured relative level");
        }
        let answers = lane_answers(&rows, &[], 0, Domain::Fft, false, false, false);
        assert_eq!(answers.items.iter().take(3).map(|a| a.unit.as_str()).collect::<Vec<_>>(), ["Hz", "dBV", "dBc"], "{window:?}: FFT answers start with frequency, level, then harmonic");
        assert_eq!((&*answers.items[2].name, answers.items[2].value.as_str()), ("H3 ", format!("{:.1}", h3.1).as_str()), "{window:?}: the harmonic answer names H3 and uses one decimal");
        assert!(rows.iter().any(|r| r.name == "2nd f") && answers.items.iter().any(|a| a.name == "2nd "), "{window:?}: secondary peak rows remain available to the popover");
        let mut stronger_fifth = spectrum;
        stronger_fifth.harmonics[3] = Some((h5.0, -3.0));
        let rows = stronger_fifth.rows();
        let answers = lane_answers(&rows, &[], 0, Domain::Fft, false, false, false);
        assert_eq!((&*answers.items[2].name, &*answers.items[2].value), ("H5 ", "-3.0"), "{window:?}: automatic answers still choose the largest harmonic by level");
    }
}

#[test]
fn pure_tone_and_out_of_span_harmonics_are_absent() {
    for window in [Window::Rect, Window::Hann, Window::Hamming, Window::BlackmanHarris, Window::FlatTop] {
        let tone = measure(window, |i, n| (TAU * 16.0 * i as f64 / n as f64).sin() as f32);
        assert!(tone.harmonics.iter().all(Option::is_none), "{window:?}: a pure tone has no harmonics above the fallback noise floor");
        assert!(!tone.rows().iter().any(|r| r.unit == "dBc"), "{window:?}: rows omit harmonics when all are absent");
    }
    // The fourth target is just outside the span, but its search lobe would include a
    // deliberately populated Nyquist bin if an out-of-span target were merely clamped.
    let high = measure(Window::FlatTop, |i, n| (TAU * 129.0 * i as f64 / n as f64).sin() as f32 + if i % 2 == 0 { 0.1 } else { -0.1 });
    assert!(4.0 * high.peak_f > high.span, "high tone's fourth harmonic lies beyond the last spectrum bin");
    assert!(high.harmonics[2].is_none(), "H4 beyond the last bin is absent");
    assert!(high.harmonics[3].is_none(), "H5 beyond the last bin is absent");
    let silent = measure(Window::Hann, |_, _| 0.0);
    assert!(silent.harmonics.iter().all(Option::is_none), "the numerical amplitude floor does not turn silence into harmonics");
}

#[test]
fn bracket_gates_tones_resolution_and_empty_answers() {
    use fab_elec_bench::{source::{Block, ChanData, Intake}, trigger::{EdgeTrigger, Slope, Window as TimeWindow}};
    use fab_elec_sim::trace::TraceStore;
    use fab_elec_view::instruments::{
        acquire::{AcquireIntake, Acquisition, LaneFrame, LaneSetup, Published},
        axis::si_label,
        scope::ScopeSettings,
        scrub::{draw_spectrum_still, Still, StillScratch},
        span::SpanRequest,
    };
    use std::{sync::{Arc, Mutex}, time::Duration};

    let plot = (512, 64);
    let n = fft_size(plot.0);
    let record = 2e-3;
    let trigger = record * 0.5;
    let samples = n * 4;
    let dt = record / samples as f64;
    let t: Vec<_> = (0..=samples + 1).map(|i| i as f64 * dt).collect();
    // The extra tone starts exactly at the excluded endpoint of the first-half gate.
    let y: Vec<_> = t.iter().enumerate().map(|(i, &t)| ((TAU * 10e3 * t).sin()
        + if i >= samples / 2 { 0.25 * (TAU * 30e3 * t).cos() } else { 0.0 }) as f32).collect();
    let settings = ScopeSettings { domain: Domain::Fft, time_per_div: record / 10.0, position: 0.5,
        fft: FftSettings { window: Window::Hann, ..FftSettings::default() }, ..ScopeSettings::default() };
    let mut store = TraceStore::new(1, &[false]);
    let mut closed = Vec::new();
    for (&t, &y) in t.iter().zip(&y) { store.push(t, &[f64::from(y)], &mut closed); }
    let mut scratch = StillScratch::default();
    let mut stroke = SpectrumStroke::default();
    let mut hits = Hits::new(plot.0, plot.1);
    let setup = LaneSetup { map: settings.map(plot), spectrum: Some((settings.fft, plot)), drawn: true };
    let acquisition = Acquisition::new(EdgeTrigger { level: 0.0, hyst: 0.0, slope: Slope::Rising, holdoff: 0.0 }, 0,
        TimeWindow { pre: trigger, post: record - trigger }, vec![setup]);
    let published = Arc::new(Mutex::new(Vec::new()));
    let sink = published.clone();
    let mut intake = AcquireIntake::new(acquisition, Duration::MAX, move |frame: LaneFrame, stats| {
        assert_eq!(frame.lanes[0].waveforms, 1, "gating never removes the completed time picture");
        sink.lock().unwrap().push((stats, frame.spectra[0].as_ref().unwrap().waveforms));
        // Retain accumulation to prove that changing/clearing the gate discards it.
        Published::Kept(frame)
    });
    let request = Arc::new(SpanRequest::default());
    intake.follow_span(request.clone());
    let first_half = Some((-trigger, 0.0));
    let short = Some((-trigger, -trigger + 2.0 * dt));
    let gates = [None, first_half, short, Some((record, 2.0 * record)), Some((-trigger - dt, 0.0)), None];
    for (case, gate) in gates.into_iter().enumerate() {
        hits.reset(plot.0, plot.1);
        stroke.begin_gated(-trigger, record, trigger, gate, plot, settings.fft);
        stroke.point(t[0], y[0]);
        for i in 1..t.len() { stroke.segment(t[i - 1], y[i - 1], t[i], y[i]); }
        stroke.finish(&mut hits);
        let result = stroke.result();
        let valid = case == 0 || case == 1 || case == 5;
        assert_eq!(result.gate_too_short, !valid, "case {case}: two samples and uncovered gates are rejected");
        assert_eq!(hits.waveforms, u64::from(valid), "case {case}: only valid transforms draw a waveform");
        let rows = result.rows();
        let mut answers = lane_answers(&rows, &[], 0, Domain::Fft, false, true, false);
        if valid {
            let spectrum = result.measurements.expect("valid gate publishes spectral measurements");
            let length = gate.map_or(record, |(t0, t1)| t1 - t0);
            let df = spectrum.rate / spectrum.n as f64;
            assert!((spectrum.peak_f - 10e3).abs() < 1e-6, "case {case}: the fundamental remains at 10 kHz");
            assert!((df * length - 1.0).abs() < 1e-12, "case {case}: bin spacing follows gated record length");
            assert!((spectrum.rbw * length - 1.5).abs() < 1e-12, "case {case}: Hann RBW equals ENBW over gated length");
            assert!((spectrum.span - n as f64 / (2.0 * length)).abs() < 1e-6, "case {case}: frequency span follows gated length");
            let third = stroke.bins()[(30e3 / df).round() as usize];
            if gate.is_some() {
                assert!(third < -60.0, "first-half gate: 30 kHz is below -60 dBV, got {third}");
                assert!(spectrum.harmonics[1].is_none(), "first-half gate: H3 is absent below the -60 dBc answer floor");
            } else {
                assert!(third > -25.0, "case {case}: ungated 30 kHz is above -25 dBV, got {third}");
                let h3 = spectrum.harmonics[1].expect("ungated second-half tone resolves as H3");
                assert!((h3.0 - 30e3).abs() <= df, "case {case}: the harmonic marker stays at 30 kHz");
            }
        } else {
            assert!(result.measurements.is_none() && stroke.bins().is_empty(), "case {case}: rejected gate has no spectrum or stale bins");
            assert_eq!(answers.text, "gate too short", "case {case}: rejected gate replaces the waiting-for-trigger note");
        }
        if let Some((t0, t1)) = gate {
            answers.append_gate(t1 - t0);
            assert_eq!(answers.items.last().map(|a| a.name.as_str()), Some("gated "), "case {case}: gate duration is the last answer item");
            assert!(answers.text.ends_with(&format!("gated {}", si_label(t1 - t0, "s"))), "case {case}: gate duration uses si_label");
            if !valid { assert_eq!(answers.items[0].name, "gate too short", "case {case}: the empty-spectrum note remains drawable beside the gate"); }
        }

        request.set(Some((record, 2.0 * record)));
        let generation = request.set(gate);
        let chans = [ChanData::F32(&y)];
        intake.block(&Block { index: 0, t0: 0.0, dt, chans: &chans, clipped: &[], trigger: Some(trigger) });
        intake.flush();
        let (stats, waveforms) = published.lock().unwrap().pop().expect("worker publishes this completed capture");
        assert_eq!(stats.spectrum_span, generation, "case {case}: acquisition takes the latest span request");
        assert_eq!(stats.spectrum[0], result, "case {case}: worker and standalone stroke use the same trigger-relative gate");
        assert_eq!(waveforms, u64::from(valid), "case {case}: a bracket edit or clear discards old accumulation");

        // The cell uses the time cursor as trigger with or without a bracket.
        let at = trigger;
        let (still, still_rows) = draw_spectrum_still(&store, 0, at, &settings, gate, plot, &mut scratch, &mut hits);
        assert!(matches!(still, Still::Drawn), "case {case}: the still reports a completed draw, including an empty gate");
        assert_eq!(still_rows, rows, "case {case}: scrub stills share the worker's spectrum and rejection note");
        assert_eq!(hits.waveforms, u64::from(valid), "case {case}: scrub stills leave rejected gates empty");
    }
}
