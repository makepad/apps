use fab_elec_bench::source::*;
use fab_elec_bench::trigger::Slope;
use fab_elec_instr::{prbs::Prbs, synth::{Synth as Pwm, Spread}, Record, Replay, Signal, Synth, SynthChannel, SynthConfig, MEMBER_CYCLE};

#[derive(Default)]
struct Capture { layouts: Vec<Layout>, blocks: Vec<(u64, f64, f64, usize)>, points: Vec<(f64, Vec<f32>)>, marks: Vec<(f64, Mark)>, gaps: Vec<(f64, f64, Gap)> }
impl BlockOut for Capture {
    fn layout(&mut self, layout: &Layout) { self.layouts.push(layout.clone()); }
    fn block(&mut self, block: &Block) {
        let n = block.chans[0].len();
        self.blocks.push((block.index, block.t0, block.dt, n));
        assert!(block.trigger.is_none(), "synthetic blocks leave software trigger search enabled");
        for i in 0..n { self.points.push((block.t0 + i as f64 * block.dt, block.chans.iter().map(|c| c.value(i)).collect())); }
    }
    fn point(&mut self, t: f64, values: &[f32]) { self.points.push((t, values.to_vec())); }
    fn mark(&mut self, t: f64, kind: Mark) { self.marks.push((t, kind)); }
    fn gap(&mut self, from: f64, to: f64, why: Gap) { self.gaps.push((from, to, why)); }
}
fn setup(caps: &Caps, rate: f64, span: f64) -> Setup {
    Setup { channels: caps.channels.iter().map(|c| ChannelSetup { id: c.id, on: true, window: (-1.0, 4.0), coupling: Coupling::Dc,
        probe: Probe::default(), bw_limit: None, invert: false }).collect(),
        time: TimeSetup { span, pre: 0.2, rate: RateWant::Exact(rate), depth: DepthWant::Auto, plot_px: 100 },
        trigger: TriggerSpec { kind: TriggerKind::Edge, channel: caps.channels[0].id, level: 1.65, hyst: 0.1,
            slope: Slope::Rising, holdoff: 0.0, run: TriggerRun::Normal }, mode: ModeWant::Auto, drives: Vec::new() }
}

#[test]
fn uniform_pwm_and_prbs_preserve_clock_and_bits() {
    let pwm = Pwm { freq: 1000.0, noise: 0.0, ..Pwm::pwm() };
    let prbs = Prbs { bit_rate: 1000.0, wn: 40.0 * std::f64::consts::TAU * 1000.0, zeta: 0.9, jitter: 0.0, noise: 0.0, ..Prbs::gigabit() };
    let config = SynthConfig { side: Side::Sim, channels: vec![SynthChannel { signal: Signal::Synth(pwm.clone()), name: "PWM".into() },
        SynthChannel { signal: Signal::Prbs(prbs.clone()), name: "data".into() }], seed: 0, limits: None, member_spread: None, pace: None };
    let mut real = config.clone(); real.side = Side::Real;
    assert_eq!(Synth::new(real).unwrap().info().side, Side::Real, "a twin hardware stand-in announces its configured side");
    let mut driver = Synth::new(config).unwrap();
    assert_eq!(driver.info().side, Side::Sim, "the ordinary synthetic driver announces the simulator side");
    let mut want = setup(&driver.caps(), 8000.0, 1.0);
    let resolved = driver.apply(&want).unwrap();
    let mut out = Capture::default();
    for _ in 0..16 { assert_eq!(driver.pump(&mut out), Pump::Data, "unpaced synth produces a bounded block per pump"); }
    assert_eq!(out.layouts.len(), 1, "one layout starts a continuous acquisition");
    assert_eq!(out.layouts[0].channels.len(), 2, "layout includes both signals in configured order");
    assert_eq!(out.layouts[0].channels[1].name, "data", "channel names survive the source seam");
    assert_eq!(resolved.rate, 8000.0, "synth uses the resolved rate");
    let mut next = 0;
    let mut at = 0;
    for &(index, t0, dt, n) in &out.blocks {
        assert_eq!(index, next, "uniform sample indices have no holes");
        assert_eq!(t0, index as f64 * dt, "block starts use sample index and resolved interval");
        assert_eq!(dt, 1.0 / resolved.rate, "uniform sample interval is exact");
        assert!(n <= 1024, "each pump bounds its sample count");
        for i in 0..n {
            let (time, values) = &out.points[at + i];
            assert_eq!(*time, t0 + i as f64 * dt, "sample times use the Block contract arithmetic");
            assert_eq!(values[0], pwm.value(*time) as f32, "PWM samples match the unchanged signal evaluator");
            if (index + i as u64) % 8 == 4 {
                assert_eq!(values[1] > 0.4, prbs.bit((index + i as u64) / 8), "PRBS bit-centre samples match Prbs::bit");
            }
        }
        next += n as u64; at += n;
    }
    assert!(out.gaps.is_empty(), "continuous synthetic data has no gaps");
    want.trigger.level = 2.0;
    assert!(!driver.apply(&want).unwrap().restart, "driver compares retunes with the answer it actually applied");
    driver.stop();
    assert!(matches!(driver.pump(&mut out), Pump::Idle(_)), "stopped synth produces no new block");
}

#[test]
fn spread_uses_successive_members_and_repeats_its_cycle() {
    let pwm = Pwm { freq: 1024.0, noise: 0.0, ..Pwm::pwm() };
    let spread = Spread { wn: 0.03, zeta: 0.1, height: 0.1 };
    let seed = 3;
    let mut driver = Synth::new(SynthConfig { side: Side::Sim, channels: vec![SynthChannel { signal: Signal::Synth(pwm.clone()), name: "ensemble".into() }],
        seed, limits: None, member_spread: Some(spread), pace: None }).unwrap();
    let want = setup(&driver.caps(), 16384.0, 0.1);
    driver.apply(&want).unwrap();
    let mut out = Capture::default();
    while out.points.len() < (MEMBER_CYCLE as usize + 2) * 16 { driver.pump(&mut out); }
    let seeded = Pwm { seed: pwm.seed ^ seed, ..pwm };
    for member in 0..MEMBER_CYCLE as usize + 2 {
        let draw = seeded.member(spread, member as u64 % MEMBER_CYCLE);
        assert_eq!(out.points[member * 16 + 4].1[0], draw.value(4.0 / 16384.0) as f32, "each period samples its deterministic member draw");
    }
    assert_ne!(out.points[4].1, out.points[20].1, "successive periods expose the ensemble's spread");
    assert_eq!(out.points[4].1, out.points[MEMBER_CYCLE as usize * 16 + 4].1, "member cycle repeats its first draw");
    assert_eq!(out.points[20].1, out.points[(MEMBER_CYCLE as usize + 1) * 16 + 4].1, "member cycle repeats its second draw");
}

#[test]
fn replay_preserves_nonuniform_samples_marks_and_loop_gap() {
    let record = Record { layout: Layout { generation: 7, clock: ClockKind::Device,
        channels: vec![ChannelInfo { id: ChanId(3), name: "saved".into(), quantity: Quantity::Volt }] },
        times: vec![2.0, 2.125, 2.5], lanes: vec![vec![1.0, 4.0, -2.0]], marks: vec![(2.125, Mark::HardwareTrigger), (2.5, Mark::Segment)] };
    let mut driver = Replay::new(record.clone(), None).unwrap();
    let caps = driver.caps();
    let want = setup(&caps, 4.0, 0.5);
    let answer = driver.apply(&want).unwrap();
    let mut out = Capture::default();
    assert_eq!(driver.pump(&mut out), Pump::Data, "first record pass is one bounded step");
    assert!(out.gaps.is_empty(), "first record pass has no loop seam");
    assert_eq!(driver.pump(&mut out), Pump::Data, "second record pass is available without wall pacing");
    let points: Vec<_> = record.times.iter().zip(&record.lanes[0]).map(|(&t, &v)| (t, vec![v])).collect();
    assert_eq!(&out.points[..3], points.as_slice(), "first pass preserves exact nonuniform samples");
    assert_eq!(&out.points[3..], points.as_slice(), "loop preserves recorded timestamps and values");
    assert_eq!(out.marks, record.marks.repeat(2), "marks retain their exact times and kinds on both passes");
    assert_eq!(out.gaps, vec![(2.5, 2.5, Gap::Restart)], "one restart gap separates two passes");
    assert_eq!(out.layouts, vec![record.layout], "record layout is announced once");
    assert_eq!(caps.depth, Depth::Samples(3), "replay depth is the record length");
    assert_eq!(answer.mode, Mode::Stream, "replay is stream-only");
    assert_eq!(driver.info().side, Side::Replay, "replayed data is labelled on the replay side");
}

#[test]
fn pacing_returns_idle_without_sleeping() {
    let mut synth = Synth::new(SynthConfig { side: Side::Sim, channels: vec![SynthChannel { signal: Signal::Synth(Pwm::pwm()), name: "PWM".into() }],
        seed: 0, limits: None, member_spread: None, pace: Some(1e-9) }).unwrap();
    let want = setup(&synth.caps(), 1000.0, 1.0);
    synth.apply(&want).unwrap();
    let mut out = Capture::default();
    assert_eq!(synth.pump(&mut out), Pump::Data, "first paced block starts immediately");
    let before = out.points.len();
    assert!(matches!(synth.pump(&mut out), Pump::Idle(wait) if !wait.is_zero()), "pacing reports a positive delay to the worker");
    assert_eq!(out.points.len(), before, "paced idle does not write samples");
    let mut replay = Replay::new(Record { layout: out.layouts[0].clone(), times: vec![0.0, 1.0], lanes: vec![vec![0.0, 1.0]], marks: Vec::new() }, Some(1e-9)).unwrap();
    replay.apply(&want).unwrap();
    let mut saved = Capture::default();
    assert_eq!(replay.pump(&mut saved), Pump::Data, "paced replay emits the first due point");
    assert!(matches!(replay.pump(&mut saved), Pump::Idle(wait) if !wait.is_zero()), "replay returns idle before its next point is due");
    assert_eq!(saved.points.len(), 1, "replay pacing does not emit future samples early");
}
