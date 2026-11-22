use fab_elec_bench::{source::*, trigger::Slope};
use fab_elec_instr::{Limits, Signal, Synth, SynthChannel, SynthConfig};
use fab_elec_view::instruments::{feed::FeedStats, source_bar::*};
use std::sync::Arc;

fn bench() -> SourceChip {
    let driver = Synth::new(SynthConfig { side: Side::Sim, channels: vec![SynthChannel { name: "CH1".into(), signal: Signal::Synth(fab_elec_instr::synth::Synth::pwm()) }],
        seed: 1, member_spread: None, pace: None, limits: Some(Limits::bench_scope()) }).unwrap();
    let caps = driver.caps();
    let resolved = resolve(&caps, &Setup {
        channels: vec![ChannelSetup { id: ChanId(0), on: true, window: (-3.0, 3.0), coupling: Coupling::Dc,
            probe: Probe::default(), bw_limit: None, invert: false }],
        time: TimeSetup { span: 80e-6, pre: 16e-6, rate: RateWant::Exact(25e6), depth: DepthWant::Auto, plot_px: 1000 },
        trigger: TriggerSpec { kind: TriggerKind::Zone, channel: ChanId(0), level: 0.0, hyst: 0.1, slope: Slope::Rising, holdoff: 0.0, run: TriggerRun::Normal },
        mode: ModeWant::Auto, drives: Vec::new(),
    }, None);
    SourceChip { info: driver.info(), state: SourceState::Live, caps: Arc::new(caps), resolved: Some(Arc::new(resolved)), trigger_kind: TriggerKind::Zone, note: None }
}
fn simulation() -> SourceChip {
    SourceChip { info: SourceInfo { id: SourceId(0), side: Side::Sim, name: "Simulation".into(), serial: None, clock: ClockKind::Simulated },
        state: SourceState::Live, caps: Arc::new(simulation_caps(vec![ChannelInfo { id: ChanId(0), name: "out".into(), quantity: Quantity::Volt }])),
        resolved: None, trigger_kind: TriggerKind::Edge, note: None }
}

#[test]
fn three_width_forms_keep_source_identity_and_whole_answers() {
    for (chip, answers, full, short) in [
        (simulation(), Answers { coverage: Some(Coverage::All { triggers: 9870.0 }), sim_ratio: Some(1.0 / 53.0) }, "all 9 870 triggers/s drawn", "0.0189×"),
        (bench(), Answers { coverage: Some(Coverage::Blocks { captures: 42.0, blind: 0.996 }), sim_ratio: None }, "42 captures/s, blind 99.6 %", "blind 99.6 %"),
    ] {
        for (width, form) in [(2000.0, BarForm::Full), (250.0, BarForm::Short), (100.0, BarForm::Chips)] {
            let placed = source_bar_layout(width, std::slice::from_ref(&chip), &answers, |s| s.chars().count() as f64 * 6.0);
            assert!(placed.iter().all(|p| p.form == form), "{form:?} width selects the expected presentation for {}", chip.info.name);
            assert_eq!(placed[0].chip, Some(0), "each presentation starts with its clickable source chip");
            assert_eq!(placed[0].text, chip.info.name, "each presentation preserves the source name");
            assert!(placed.windows(2).all(|p| p[0].x + p[0].width <= p[1].x), "placed chips and answers do not overlap");
            assert!(placed.iter().all(|p| p.x + p.width <= width), "every item fits the selected test width");
            match form {
                BarForm::Full => assert!(placed.iter().any(|p| p.text == full), "full width states the complete coverage answer"),
                BarForm::Short => assert!(placed.iter().any(|p| p.text == short), "medium width states the compact answer"),
                BarForm::Chips => assert_eq!(placed.len(), 1, "narrow width shows only source identity"),
            }
        }
    }
    let mut paused = simulation(); paused.state = SourceState::Paused;
    let wide = source_bar_layout(1000.0, &[paused.clone()], &Answers::default(), |s| s.len() as f64 * 6.0);
    assert_eq!(wide[0].text, "Simulation paused", "non-live state word accompanies a source with room");
    let narrow = source_bar_layout(100.0, &[paused], &Answers::default(), |s| s.len() as f64 * 6.0);
    assert_eq!(narrow[0].text, "Simulation", "narrow state remains available in the sheet while chip keeps the name");
    let mut compiling = simulation(); compiling.state = SourceState::Compiling;
    let wide = source_bar_layout(1000.0, &[compiling], &Answers::default(), |s| s.len() as f64 * 6.0);
    assert_eq!(wide[0].text, "Simulation compiling firmware…", "a run waiting for its first firmware image says so");

    // The rate reads as a factor of real time: a run at 0.7x no longer rounds to "1 s of circuit
    // takes 1 s", and a slow one is plainly slow. Three significant digits, as the transport, and
    // two decimals from a tenth to one, so the rate keeps its width either side of real time.
    let rate = |ratio: f64| Answers { coverage: None, sim_ratio: Some(ratio) };
    for (ratio, full, short) in [(0.7, "0.70× real time", "0.70×"), (0.45, "0.45× real time", "0.45×"), (1.0, "1.00× real time", "1.00×"),
        (2.5, "2.50× real time", "2.50×"), (9.996, "10.0× real time", "10.0×"), (1234.4, "1\u{2009}230× real time", "1\u{2009}230×")] {
        assert_eq!((rate(ratio).text(false), rate(ratio).text(true)), (vec![full.to_string()], vec![short.to_string()]), "rate {ratio}");
    }
    assert!(rate(f64::INFINITY).text(false).is_empty() && rate(0.0).text(false).is_empty(), "no rate without a finite, moving one");
}

#[test]
fn cannot_and_sheet_describe_real_limits_without_inventing_simulator_limits() {
    let device = bench();
    let sentences = cannot(&device.caps);
    for expected in [
        "It cannot measure current.",
        "It keeps at most 10 000 samples per capture, so it cannot rewind before a capture began.",
        "It cannot run ensemble members.",
        "It triggers on edges in hardware; other trigger kinds run in software and miss what happens between captures.",
    ] { assert!(sentences.iter().any(|s| s == expected), "capability limitation is explicit: {expected}"); }
    let sheet = source_sheet(&device, &Answers::default());
    assert!(sheet.contains("25 MS/s"), "source sheet names the resolved sample rate");
    let depth = device.resolved.as_ref().unwrap().depth;
    assert!(sheet.replace(' ', "").contains(&format!("{depth}samplespercapture")), "source sheet names the resolved capture depth");
    assert!(sheet.contains("31.25 mV steps"), "source sheet names the input code size");
    assert!(sheet.contains("Zone trigger: sw, blind 99.2 %"), "source sheet explains software trigger blindness (a record twice the view: 4 000 of 10 000 samples, 160 us of every 20 ms block)");
    for note in &device.resolved.as_ref().unwrap().notes { assert!(sheet.contains(&note.text), "source sheet retains every resolver note"); }
    let sim = simulation();
    assert!(cannot(&sim.caps).is_empty(), "circuit simulation's current, members, rewind and zero-load powers are not listed as missing");
    let sheet = source_sheet(&sim, &Answers::default());
    assert!(sheet.contains("Exact values.") && sheet.contains("Unbounded record."), "simulator sheet states exact values and unbounded record");
}

#[test]
fn coverage_uses_interval_deltas_and_cannot_drop_keeps_a_reason() {
    let previous = FeedStats { waveforms: 100, trigger_events: 100, frames: 100, dropped: 100, ..FeedStats::default() };
    let now = FeedStats { waveforms: 9970, trigger_events: 9970, frames: 163, dropped: 137, ..FeedStats::default() };
    let answers = Answers::from_stats(now, previous, 1.0, None, None);
    assert_eq!(answers.text(false), ["blind 37 %"], "display blindness uses last-second deltas, not lifetime totals");
    let all = Answers::from_stats(FeedStats { dropped: 100, ..now }, previous, 1.0, None, None);
    assert_eq!(all.text(false), ["all 9 870 triggers/s drawn"], "coverage rate uses last-second trigger delta");
    let chip = bench();
    let item = fab_elec_view::live::probe::current_probe_drag_item();
    assert_eq!(fab_elec_view::live::probe::source_refusal(&chip.caps, std::slice::from_ref(&item)), Some("It cannot measure current."), "unsupported current drop carries the reason sentence");
    assert_eq!(fab_elec_view::live::probe::source_refusal(&simulation().caps, &[item]), None, "simulator current drops are not refused");
}
