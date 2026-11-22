use fab_elec_bench::{lower, ModelLibs, SimCmd};
use fab_elec_fw::{compile, maker, Chip, FwInput};
use fab_elec_lang::{evaluate, std_libs, UnitState};
use fab_elec_mcu_cosim::{watch, FwFrame};
use fab_elec_mcu_emit_s3::{wiring, AmpEnable, Audio, I2cBus};
use fab_elec_mcu_parts::{Netlist, PartKind};
use fab_elec_sim::cosim::CoAudio;
use fab_elec_sim::live::LiveCmd;
use fab_elec_sim::livesim::{LiveSim, LiveSpec};
use fab_elec_sim::Sink;
use fab_elec_simhost::events::{EventLog, EventSource};
use std::sync::Arc;
use std::time::Duration;

#[test]
fn beep_binds_its_firmware_and_resolves_both_audio_legs() {
    let sources = fab_elec_fixtures::docs("beep").expect("beep is a stock project");
    let ev = evaluate(&sources, &std_libs()).unwrap_or_else(|diags| panic!("beep evaluates: {diags:?}"));
    assert!(ev.notes.is_empty(), "beep documents evaluate cleanly: {:?}", ev.notes);
    assert!(ev.system.diags.is_empty(), "beep system resolves: {:?}", ev.system.diags);
    let unit = ev.units.iter().find(|u| u.name == "beep").expect("beep has its circuit");
    assert_eq!(unit.state, UnitState::Built, "beep circuit builds: {:?}", unit.state.diags());
    let bench = ev.benches.iter().find(|b| b.name == "beep").expect("beep has its bench");
    assert_eq!(bench.state, UnitState::Built, "beep bench builds: {:?}", bench.state.diags());
    let design = ev.design.units.iter().find(|u| u.name == "beep").expect("beep lowers its unit");
    for (path, pads) in [("codec", 21), ("amp", 8)] {
        let comp = design.circuit.comps.iter().find(|c| c.path == path).unwrap_or_else(|| panic!("the circuit has {path}"));
        let footprint = comp.part.footprint.as_ref().unwrap_or_else(|| panic!("{path} has its standard package's footprint"));
        assert_eq!(footprint.pads.len(), pads, "{path}'s footprint has a land for every pad");
    }
    let lowered = lower(&ev.design, bench.scope_id(), &bench.spec, &ModelLibs::generic());
    let [binding] = lowered.firmware.as_slice() else { panic!("expected one firmware binding: {:?}", lowered.diags) };
    assert_eq!(binding.doc, "beep.fw.splash", "the bench binds the button firmware");
    assert_eq!(binding.chip.as_deref(), Some("ESP32-S3"), "the firmware runs on the S3");
    assert_eq!(binding.f_clk, Some(40_000_000.0), "the S3 clock comes from the crystal");
    let board = binding.bus.board(0);
    assert_eq!(board.parts.len(), 2, "both audio parts survive lowering: {:?}", lowered.diags);
    for (path, kind) in [("codec", PartKind::Es8311 { addr: 0x18 }),
        ("amp", PartKind::Amp { enable_high: true, gain_db: 4, start_ms: 120 })] {
        assert!(board.parts.iter().any(|p| p.refdes.as_ref() == path && p.kind == kind), "{path} has its circuit parameters");
        for d in lowered.diags.iter().filter(|d| d.path == path) {
            assert!(d.message.ends_with("has no bus model; left to the circuit"), "{path} lowered with a refusal: {}", d.message);
        }
    }
    for (cap, resistor) in [("cin_p", "rin_p"), ("cin_n", "rin_n")] {
        assert_eq!(lowered.diags.iter().filter(|d| d.message.contains("series coupling through")
            && d.message.contains(cap) && d.message.contains(resistor)).count(), 1, "{cap} and {resistor} share one coupling note");
    }
    let pins = wiring(&board).expect("the audio parts resolve to S3 GPIOs");
    assert_eq!(pins.i2c, Some(I2cBus { sda: 15, scl: 14, hz: 400_000 }), "codec control uses the handheld I2C pins");
    assert_eq!(pins.audio, Some(Audio {
        addr: 0x18, mclk: 16, sclk: 9, lrck: 45, dsdin: 8, asdout: Some(10),
        amps: vec![AmpEnable { gpio: 46, enable_high: true }],
    }), "the CE strap, I2S signals and amp enable match the circuit");
    Netlist::new(board).expect("both amplifier inputs reach the corresponding codec outputs");
}

/// The speaker's committed sound, as (run time of each sample, sample).
#[derive(Default)]
struct Heard(Vec<(f64, i16)>);

impl Sink for Heard {
    fn co_audio(&mut self, audio: &CoAudio) {
        for (run, samples) in audio.each() {
            let period = run.rate.1 as f64 / run.rate.0 as f64;
            self.0.extend(samples.iter().enumerate().map(|(k, &x)| (run.t0 + k as f64 * period, x)));
        }
    }
}

// The firmware's tone: eight samples at +2048, eight at -2048, at 16 kHz (beep.fw.splash).
// Through the DAC at 0 dB and the amplifier's 4 dB (beep.ckt.splash), each half wave is one
// level, so a 1 kHz square shows as eight equal samples of each sign in turn.
fn square_cycles(heard: &[(f64, i16)], from: f64, to: f64) -> usize {
    let window: Vec<i16> = heard.iter().filter(|(t, _)| (from..to).contains(t)).map(|&(_, x)| x).collect();
    let high = window.iter().copied().max().unwrap_or(0);
    assert!(high > 2048, "the amplified tone is louder than the DAC word: {high}");
    assert_eq!(window.iter().copied().min(), Some(-high), "the square is symmetric");
    assert!(window.iter().all(|&x| x == high || x == -high), "only the two levels sound");
    let runs: Vec<usize> = window.chunk_by(|a, b| a == b).map(<[i16]>::len).collect();
    assert!(runs[1..runs.len() - 1].iter().all(|&n| n == 8), "every inner half wave is eight samples: {runs:?}");
    runs.len() / 2
}

#[test]
fn holding_the_button_plays_a_1_khz_tone_on_the_twin() {
    let sources = fab_elec_fixtures::docs("beep").expect("beep is a stock project");
    let ev = evaluate(&sources, &std_libs()).unwrap_or_else(|diags| panic!("beep evaluates: {diags:?}"));
    let bench = ev.benches.iter().find(|b| b.name == "beep").expect("beep has its bench");
    let lowered = lower(&ev.design, bench.scope_id(), &bench.spec, &ModelLibs::generic());
    let binding = lowered.firmware.first().expect("the S3 has its firmware");
    let input = FwInput {
        part: binding.part.clone(), doc: binding.doc.clone(), rev: 1, text: include_str!("../beep.fw.splash").into(),
        f_clk: binding.f_clk.expect("the crystal clocks the S3"), params: binding.params.clone(), chip: Chip::S3,
        wiring: Some(wiring(&binding.bus.board(0)).expect("the audio wiring resolves")), island: None, files: Vec::new(), net_files: Vec::new(),
    };
    let built = compile(&input).unwrap_or_else(|diags| panic!("the beep firmware builds for the S3: {diags:?}"));
    let end = 1.0;
    let (mut log, feed) = EventLog::<FwFrame>::new(end, |frame| {
        if frame.is_image() { EventSource::Image } else { EventSource::Trace }
    }, Arc::new(|| {}));
    let (make, _) = maker(&input, built, feed, false, Arc::new(|_| {}), None);
    let device = make(&lowered, 0).expect("the maker builds the beep board");
    let spec = LiveSpec { rate: f64::INFINITY, max_step: 1e-3, res: 0.0, trace: false, ..LiveSpec::default() };
    let mut sim = LiveSim::new(lowered.circuit.clone(), lowered.plan.clone(), lowered.index.generation, spec)
        .expect("the beep circuit's operating point converges");
    sim.attach(Some(device));
    let mut heard = Heard::default();
    let mut run_to = |sim: &mut LiveSim, heard: &mut Heard, until: f64| {
        while sim.t() < until {
            sim.advance((sim.t() + 1e-3).min(until), Some(Duration::from_millis(50)), heard)
                .unwrap_or_else(|d| panic!("the beep circuit advances: {d}"));
            log.drain();
        }
        sim.values(0.0);
        log.drain();
    };
    let [button] = lowered.index.momentary() else { panic!("beep has one button: {:?}", lowered.index.momentary()) };
    let press = |sim: &mut LiveSim, at: f64, value: f64| {
        for cmd in lowered.index.command(&SimCmd::SetParam { comp: *button, value }) {
            assert!(sim.apply(LiveCmd::At(at, Box::new(cmd))), "the button goes on the run's timeline");
        }
    };
    // NS4150B start time is 120 ms (beep.ckt.splash's amp, ns4150b.txt:113).
    let (down, up) = (0.2, 0.6);
    press(&mut sim, down, 1.0);
    press(&mut sim, up, 0.0);
    run_to(&mut sim, &mut heard, end);

    assert!(heard.0.iter().filter(|&&(t, _)| t < down).all(|&(_, x)| x == 0), "silence before the press");
    let first = heard.0.iter().find(|&&(_, x)| x != 0).map(|&(t, _)| t).expect("the press makes sound");
    assert!((down + 0.120..down + 0.140).contains(&first), "the tone starts once the amplifier has started: {first:.4} s");
    // From the first sound to the release, 1 kHz without a gap or a lost refill.
    let cycles = square_cycles(&heard.0, first, up);
    let expected = ((up - first) * 1000.0) as usize;
    assert!(cycles.abs_diff(expected) <= 1, "{cycles} cycles of 1 kHz in {:.4} s", up - first);
    assert!(heard.0.iter().filter(|&&(t, _)| t > up + 0.01).all(|&(_, x)| x == 0), "silence after the release");
    let faults: Vec<_> = log.store.span(0.0, sim.t()).filter_map(|e| match &e.frame {
        FwFrame::Image(frame) if matches!(frame.event, watch::Event::Fault { .. }) => Some((e.t, frame.event)),
        _ => None,
    }).collect();
    assert!(faults.is_empty(), "the beep runs with no fault: {faults:?}");
}
