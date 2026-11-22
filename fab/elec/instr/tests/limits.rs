use fab_elec_bench::{source::*, trigger::Slope};
use fab_elec_instr::{Limits, Signal, Synth, SynthChannel, SynthConfig, synth::Synth as Pwm};

fn driver(lo: f64, hi: f64) -> Synth {
    Synth::new(SynthConfig { side: Side::Sim, channels: vec![SynthChannel { name: "CH1".into(), signal: Signal::Synth(Pwm {
        freq: 1000.0, duty: 0.5, lo, hi, noise: 0.0, zeta: 0.9, ..Pwm::pwm()
    }) }], seed: 1, member_spread: None, pace: None, limits: Some(Limits::bench_scope()) }).unwrap()
}
fn setup() -> Setup {
    Setup { channels: vec![ChannelSetup { id: ChanId(0), on: true, window: (-0.5, 0.5), coupling: Coupling::Dc,
        probe: Probe::default(), bw_limit: None, invert: false }],
        time: TimeSetup { span: 0.002, pre: 0.0005, rate: RateWant::Exact(1e6), depth: DepthWant::Auto, plot_px: 800 },
        trigger: TriggerSpec { kind: TriggerKind::Edge, channel: ChanId(0), level: 0.0, hyst: 0.1, slope: Slope::Rising, holdoff: 0.0, run: TriggerRun::Normal },
        mode: ModeWant::Auto, drives: Vec::new() }
}
#[derive(Default)]
struct Capture { blocks: Vec<(f64, f64, Option<f64>, Vec<f32>, Clipped)>, gaps: Vec<(f64, f64, Gap)>, order: Vec<&'static str> }
impl BlockOut for Capture {
    fn layout(&mut self, _: &Layout) {}
    fn block(&mut self, block: &Block) {
        self.blocks.push((block.t0, block.dt, block.trigger, (0..block.chans[0].len()).map(|i| block.chans[0].value(i)).collect(), block.clipped[0]));
        self.order.push("block");
    }
    fn point(&mut self, _: f64, _: &[f32]) { panic!("block-only limits never emit points"); }
    fn mark(&mut self, _: f64, _: Mark) {}
    fn gap(&mut self, from: f64, to: f64, why: Gap) { self.gaps.push((from, to, why)); self.order.push("gap"); }
}

#[test]
fn bench_scope_caps_and_range_rate_resolution() {
    let driver = driver(-0.3, 0.3);
    let caps = driver.caps();
    assert_eq!(Limits::bench_scope().caps(vec![ChannelInfo { id: ChanId(0), name: "CH1".into(), quantity: Quantity::Volt }]), caps,
        "shared bench limits report exactly the same capabilities as Synth");
    assert_eq!(caps.channels[0].bits, 8, "bench ADC resolution is eight bits");
    assert_eq!(caps.depth, Depth::Samples(10_000), "bench maximum record is ten thousand samples");
    assert_eq!(caps.modes, Modes { block: true, block_period: Some(0.02), ..Modes::default() }, "bench offers only 20 ms blocks");
    assert_eq!(caps.channels[0].coupling, vec![Coupling::Dc, Coupling::Ac], "bench offers DC and AC coupling");
    assert_eq!(caps.trigger.hardware, vec![TriggerKind::Edge], "bench hardware triggers only on edges");
    assert!(!caps.powers.members && !caps.powers.current, "bench has neither ensemble members nor current");
    assert_eq!(Limits::bench_scope().level_step, 1, "bench hardware level steps are one ADC code");
    assert_eq!(caps.channels[0].ranges, Ranges::Steps(vec![(-0.02, 0.02), (-0.04, 0.04), (-0.08, 0.08), (-0.2, 0.2), (-0.4, 0.4), (-0.8, 0.8), (-2.0, 2.0), (-4.0, 4.0), (-8.0, 8.0), (-20.0, 20.0)]), "full scales are eight divisions of the 1-2-5 menu");
    let mut want = setup();
    want.time.rate = RateWant::Auto { per_px: 2.0 };
    let resolved = resolve(&caps, &want, None);
    assert_eq!(resolved.channels[0].range, (-0.8, 0.8), "view gets the smallest containing input range");
    assert!(resolved.notes.iter().any(|n| n.what == NoteKind::Range(ChanId(0))), "non-clipping range snap has a Range note");
    assert_eq!(resolved.rate, 1e6, "plot density snaps upward from 800 kS/s to the offered 1 MS/s");
    assert!(resolved.notes.iter().any(|n| n.what == NoteKind::Rate), "auto-rate snap has a Rate note");
    assert_eq!(resolved.trigger, TrigResolved::Hardware, "one-code levels fine enough for hysteresis use hardware");
    want.trigger.hyst = 0.001;
    let software = resolve(&caps, &want, None);
    assert_eq!(software.trigger, TrigResolved::Software { over: Over::Blocks { blind: 0.8 } }, "coarse levels fall back to software with 80 percent blindness");
    want.trigger.kind = TriggerKind::Zone;
    assert_eq!(resolve(&caps, &want, None).trigger, software.trigger, "Zone uses software inside free-running blocks");
    let mut stream_caps = caps.clone(); stream_caps.modes.stream = true;
    assert_eq!(resolve(&stream_caps, &want, None).trigger, TrigResolved::Software { over: Over::Stream }, "software stream precedes block fallback when streaming is available");
}

#[test]
fn adc_codes_clipping_and_block_dead_time_follow_resolved() {
    for (lo, hi, clip) in [(-0.3, 0.3, false), (-2.0, 2.0, true)] {
        let mut source = driver(lo, hi);
        let resolved = source.apply(&setup()).unwrap();
        let range = resolved.channels[0].range;
        let step = resolved.channels[0].step;
        let mut capture = Capture::default();
        for _ in 0..3 { assert_eq!(source.pump(&mut capture), Pump::Data, "unpaced hardware acquisition produces one capture per pump"); }
        assert_eq!(capture.blocks.len(), 3, "three pumps deliver three complete blocks");
        assert_eq!(capture.order, ["block", "gap", "block", "gap", "block"], "each blind interval is ordered between its captures");
        for (t0, dt, trigger, values, clipped) in &capture.blocks {
            assert_eq!(*dt, 1.0 / resolved.rate, "sample interval is the resolved rate");
            assert_eq!(values.len() as u64, resolved.depth, "one capture contains the resolved depth");
            assert!(trigger.is_some_and(|at| at >= *t0 && at <= *t0 + (values.len() - 1) as f64 * dt), "hardware trigger lies within its capture");
            assert_eq!(*clipped, Clipped { low: clip, high: clip }, "actual saturation reports both edges independently of view clipping");
            for &value in values {
                let code = (value as f64 - range.0) / step;
                assert!((code - code.round()).abs() < 1e-4, "every sample lies on the resolved eight-bit code grid");
                assert!((-0.0001..=255.0001).contains(&code), "eight-bit samples remain between the lower and upper saturation codes");
            }
            if clip {
                assert!(values.iter().any(|&v| (v as f64 - range.0).abs() < 1e-6), "negative overload saturates at the lower code");
                assert!(values.iter().any(|&v| (v as f64 - (range.1 - step)).abs() < 1e-6), "positive overload saturates at the upper code");
            }
        }
        for (i, &(from, to, why)) in capture.gaps.iter().enumerate() {
            assert_eq!(why, Gap::Blind, "readout dead time has Blind provenance");
            assert_eq!(from, capture.blocks[i].0 + resolved.depth as f64 / resolved.rate, "blind interval begins at capture end");
            assert_eq!(to, capture.blocks[i + 1].0, "blind interval ends at the next capture start");
            assert!(to - capture.blocks[i].0 >= 0.02 - 1e-12, "capture starts respect the minimum 20 ms cadence");
            assert!(to > from, "short captures leave positive dead time");
        }
    }
}

#[test]
fn software_blocks_leave_trigger_search_enabled() {
    let mut source = driver(-0.3, 0.3);
    let mut want = setup(); want.trigger.kind = TriggerKind::Zone;
    let resolved = source.apply(&want).unwrap();
    let mut capture = Capture::default();
    for _ in 0..2 { source.pump(&mut capture); }
    assert!(capture.blocks.iter().all(|b| b.2.is_none()), "software blocks carry no hardware timestamp to suppress the software search");
    assert_eq!(capture.blocks[1].0 - capture.blocks[0].0, 0.02, "free-running captures use the advertised start-to-start period");
    assert_eq!(resolved.trigger, TrigResolved::Software { over: Over::Blocks { blind: 0.8 } }, "free-running Zone fallback states its blind fraction");
}
