use std::sync::{Arc, Mutex};
use fab_elec_bench::{limited::{Limited, LimitedIntake, LimitedSlot}, source::*, trigger::Slope};
use fab_elec_instr::{Limits, Signal, Synth, SynthChannel, SynthConfig, synth::Synth as Pwm};

struct Captured {
    index: u64, t0: f64, dt: f64, trigger: Option<f64>, values: Vec<Vec<f32>>, clipped: Vec<Clipped>,
}
#[derive(Default)]
struct Calls {
    blocks: Vec<Captured>, gaps: Vec<(f64, f64, Gap)>, order: Vec<&'static str>,
    begins: Vec<Layout>, marks: Vec<(f64, Mark)>, idles: usize, steady: Vec<f64>, pending: bool,
}
struct Recording(Arc<Mutex<Calls>>);
impl Intake for Recording {
    fn begin(&mut self, layout: &Layout) { self.0.lock().unwrap().begins.push(layout.clone()); }
    fn point(&mut self, _: f64, _: &[f32]) { panic!("limited intake must deliver ADC blocks"); }
    fn block(&mut self, block: &Block) {
        let mut calls = self.0.lock().unwrap();
        calls.blocks.push(Captured { index: block.index, t0: block.t0, dt: block.dt, trigger: block.trigger,
            values: block.chans.iter().map(|c| (0..c.len()).map(|i| c.value(i)).collect()).collect(), clipped: block.clipped.to_vec() });
        calls.order.push("block");
    }
    fn mark(&mut self, at: f64, kind: Mark) { self.0.lock().unwrap().marks.push((at, kind)); }
    fn gap(&mut self, from: f64, to: f64, why: Gap) {
        let mut calls = self.0.lock().unwrap(); calls.gaps.push((from, to, why)); calls.order.push("gap");
    }
    fn idle(&mut self) { self.0.lock().unwrap().idles += 1; }
    fn steady(&mut self, at: f64) { self.0.lock().unwrap().steady.push(at); }
    fn pending(&self) -> bool { self.0.lock().unwrap().pending }
}

fn setup(mode: ModeWant, rate: f64, depth: u64, pre: f64, run: TriggerRun) -> (Layout, Caps, Setup) {
    let source = Synth::new(SynthConfig { side: Side::Sim, channels: (0..2).map(|i| SynthChannel {
        name: format!("CH{}", i + 1), signal: Signal::Synth(Pwm::pwm()),
    }).collect(), seed: 1, member_spread: None, pace: None, limits: Some(Limits::bench_scope()) }).unwrap();
    let mut caps = source.caps();
    if mode == ModeWant::Stream { caps.modes.stream = true; }
    if let ModeWant::Segmented(n) = mode { caps.modes.segmented = Some(n); }
    let layout = Layout { generation: 1, clock: ClockKind::Simulated, channels: caps.channels.iter().map(|c| ChannelInfo {
        id: c.id, name: c.name.clone(), quantity: c.quantity,
    }).collect() };
    // Resolved setup order deliberately differs from layout/ADC-bits order.
    let want = Setup { channels: (0..2).rev().map(|i| ChannelSetup { id: ChanId(i), on: i == 0,
        window: (-0.5, 0.5), coupling: Coupling::Dc, probe: Probe::default(), bw_limit: None, invert: false }).collect(),
        time: TimeSetup { span: depth as f64 / rate, pre, rate: RateWant::Exact(rate), depth: DepthWant::Samples(depth), plot_px: 800 },
        trigger: TriggerSpec { kind: TriggerKind::Edge, channel: ChanId(0), level: 0.013, hyst: 0.1,
            slope: Slope::Rising, holdoff: 0.0, run }, mode, drives: Vec::new() };
    (layout, caps, want)
}

fn intake(caps: &Caps, want: &Setup) -> (LimitedIntake<Recording>, Arc<Resolved>, Arc<Mutex<Calls>>) {
    let resolved = Arc::new(resolve(caps, want, None));
    let calls = Arc::new(Mutex::new(Calls::default()));
    let intake = LimitedIntake::new(Recording(calls.clone()), caps.channels.iter().map(|c| c.bits).collect(),
        resolved.clone(), want.trigger.clone(), want.time.pre, caps.modes.block_period);
    (intake, resolved, calls)
}

#[test]
fn stream_grid_codes_nan_intervals_and_publication() {
    let (layout, caps, want) = setup(ModeWant::Stream, 1e3, 100, 0.0, TriggerRun::Normal);
    let (mut limited, resolved, calls) = intake(&caps, &want);
    assert_eq!(resolved.mode, Mode::Stream, "stream fixture must resolve to streaming");
    let dt = 1.0 / resolved.rate;
    let channel = resolved.channels.iter().find(|c| c.id == ChanId(0)).unwrap();
    let origin = 0.00037;
    limited.begin(&layout);
    for (time, value) in [(0.0, -0.3), (0.6, -0.2), (2.4, 0.25), (3.2, 1.5), (4.8, 1.5),
        (5.2, 0.2), (6.3, f32::NAN), (8.4, 0.3), (10.2, 0.4)] {
        limited.point(origin + time * dt, &[value, 99.0]);
    }
    limited.idle();
    {
        let calls = calls.lock().unwrap();
        assert_eq!(calls.blocks.len(), 1, "idle publishes a short stream batch");
        let block = &calls.blocks[0];
        assert_eq!(block.values[0].len(), 11, "irregular input emits every grid sample including the anchor");
        assert_eq!(block.clipped[0], Clipped { low: false, high: true }, "overrange input reports high saturation");
        for i in 0..11 {
            assert_eq!(block.values[0][i].is_nan(), (6..=8).contains(&i), "NaN affects only the two adjacent input intervals, sample {i}");
        }
        assert!((block.values[0][1] as f64 + 0.1).abs() < 1e-6, "first interior grid sample is linearly interpolated");
        assert!((block.values[0][2] as f64 - 0.15).abs() < 1e-6, "several grid samples interpolate the same irregular interval");
        assert!((block.values[0][4] as f64 - (channel.range.1 - channel.step)).abs() < 1e-6, "overload reads the top code, one LSB below full scale");
    }
    // One source interval spans two full batches; idle/steady/drop also publish short tails.
    limited.point(origin + 8202.1 * dt, &[0.4, 99.0]);
    limited.point(origin + 8203.2 * dt, &[0.4, 99.0]);
    limited.mark(origin + 8203.2 * dt, Mark::Solver);
    limited.steady(origin + 8203.2 * dt);
    limited.idle();
    calls.lock().unwrap().pending = true;
    assert!(limited.pending(), "backpressure is delegated to the inner intake");
    calls.lock().unwrap().pending = false;
    assert!(!limited.pending(), "cleared inner backpressure is visible immediately");
    limited.point(origin + 8204.2 * dt, &[0.4, 99.0]);
    drop(limited);
    let calls = calls.lock().unwrap();
    assert_eq!(calls.blocks.iter().map(|b| b.values[0].len()).collect::<Vec<_>>(), [11, 4096, 4096, 1, 1], "stream chunks are bounded and drop publishes the last tail");
    let mut index = 0;
    for (n, block) in calls.blocks.iter().enumerate() {
        assert_eq!(block.index, index, "stream block indices count delivered samples");
        assert_eq!(block.dt, dt, "stream sample interval is resolved");
        assert_eq!(block.trigger, None, "stream batches carry no hardware trigger");
        assert!(block.values[1].iter().all(|v| v.is_nan()), "disabled layout channel always reads NaN");
        assert_eq!(block.clipped[1], Clipped::default(), "disabled channel does not clip");
        if n > 0 { assert_eq!(block.clipped[0], Clipped::default(), "clipping does not leak into later batches"); }
        for (i, &value) in block.values[0].iter().enumerate() {
            let at = block.t0 + i as f64 * block.dt;
            assert!((at - (origin + (index + i as u64) as f64 * dt)).abs() < 1e-12, "all samples remain on the first point's ADC grid");
            if value.is_finite() {
                let code = (value as f64 - channel.range.0) / channel.step;
                assert!((code - code.round()).abs() < 1e-4, "stream values lie on the ADC code grid");
                assert!((-0.0001..=255.0001).contains(&code), "stream codes stay within eight-bit rails");
            }
        }
        index += block.values[0].len() as u64;
    }
    assert!(calls.marks.is_empty(), "solver breakpoints are invisible to a bench scope");
    assert!(calls.gaps.is_empty(), "stream batching does not invent acquisition gaps");
    assert_eq!(calls.begins, [layout], "layout is forwarded unchanged");
    assert_eq!(calls.idles, 2, "idle is forwarded");
    assert_eq!(calls.steady, [origin + 8203.2 * dt], "steady is forwarded without advancing source time");
}

#[test]
fn hardware_square_captures_pretrigger_holdoff_and_blind_time() {
    let (layout, caps, mut want) = setup(ModeWant::Block, 1e5, 128, 0.000123, TriggerRun::Normal);
    want.trigger.holdoff = 0.035;
    let (mut limited, resolved, calls) = intake(&caps, &want);
    assert_eq!(resolved.trigger, TrigResolved::Hardware, "square-wave fixture uses the hardware edge detector");
    let dt = 1.0 / resolved.rate;
    let pre_count = (want.time.pre / dt).round() as usize;
    let channel = resolved.channels.iter().find(|c| c.id == ChanId(0)).unwrap();
    let level = (want.trigger.level / channel.step).round() * channel.step;
    assert_ne!(level, want.trigger.level, "fixture exercises a quantised trigger level");
    limited.begin(&layout);
    let mut t = 0.0;
    let mut i = 0;
    while t <= 0.12 {
        let value = if i == 0 { 2.0 } // This overload must expire from the pre-trigger ring.
            else if (0.00043..0.00046).contains(&t) { -2.0 } // This one belongs to the first capture.
            else if (t * 1000.0_f64).fract() < 0.5 { -0.3 } else { 0.3 };
        limited.point(t, &[value, 99.0]);
        t += dt * [0.37, 0.81, 0.53, 0.69][i % 4];
        i += 1;
    }
    limited.idle();
    let calls = calls.lock().unwrap();
    assert!(calls.blocks.len() >= 3, "normal mode repeatedly captures rising square-wave edges");
    assert_eq!(calls.gaps.len(), calls.blocks.len() - 1, "every later capture has a blind interval");
    for (i, block) in calls.blocks.iter().enumerate() {
        assert_eq!(block.values[0].len() as u64, resolved.depth, "hardware captures have the resolved depth");
        assert_eq!(block.dt, dt, "hardware captures use the resolved ADC clock");
        assert_eq!(block.index, i as u64 * resolved.depth, "capture indices count delivered samples");
        let trigger = block.trigger.expect("hardware capture needs its accepted crossing time");
        let crossing = 0.0005 + ((trigger - 0.0005) / 0.001).round() * 0.001;
        assert!((trigger - crossing).abs() <= dt, "quantised edge timestamp stays within one sample of the true 1 kHz crossing");
        assert!(block.values[0].windows(2).enumerate().any(|(j, pair)| {
            let (a, b) = (pair[0] as f64, pair[1] as f64);
            a < level && b >= level && (trigger - (block.t0 + j as f64 * dt + dt * (level - a) / (b - a))).abs() < 1e-12
        }), "hardware timestamp interpolates digitised values at the quantised trigger level");
        assert!((trigger - (block.t0 + pre_count as f64 * dt)).abs() <= dt, "capture starts at the oldest rounded pre-trigger sample");
        assert!(trigger >= block.t0 && trigger < block.t0 + resolved.depth as f64 * dt, "hardware trigger lies inside its capture");
        assert!((block.t0 / dt - (block.t0 / dt).round()).abs() < 1e-8, "pre-trigger history retains the original ADC grid");
        assert_eq!(block.clipped[0], Clipped { low: i == 0, high: false }, "capture clipping includes retained history and excludes expired or earlier samples");
        assert!(block.values[1].iter().all(|v| v.is_nan()), "disabled channels remain absent in hardware captures");
        assert_eq!(calls.order[2 * i], "block", "blind gaps and captures remain ordered");
        if i > 0 {
            let previous = &calls.blocks[i - 1];
            assert!(block.t0 - previous.t0 >= caps.modes.block_period.unwrap() - 1e-12, "block starts respect the source-time readout period");
            assert!(trigger - previous.trigger.unwrap() >= want.trigger.holdoff - 1e-12, "holdoff persists across captures and blind intervals");
            let (from, to, why) = calls.gaps[i - 1];
            assert_eq!(why, Gap::Blind, "readout gaps have Blind provenance");
            assert_eq!(from, previous.t0 + resolved.depth as f64 * dt, "blind gap starts just after the previous record");
            assert_eq!(to, block.t0, "blind gap ends at the oldest sample of the next record");
            assert!(to > from, "short captures have positive blind time");
            assert_eq!(calls.order[2 * i - 1], "gap", "each later capture is preceded by its blind gap");
        }
    }
}

#[test]
fn retune_applies_at_the_next_block_without_a_restart() {
    let (layout, caps, mut want) = setup(ModeWant::Block, 1e3, 16, 0.002, TriggerRun::Normal);
    let (mut limited, old, calls) = intake(&caps, &want);
    let slot = Arc::new(LimitedSlot::default());
    let spec = |resolved: Arc<Resolved>, want: &Setup| Limited { bits: caps.channels.iter().map(|c| c.bits).collect(),
        resolved, trigger: want.trigger.clone(), pre: want.time.pre, block_period: caps.modes.block_period };
    slot.set(spec(old.clone(), &want));
    limited.follow(slot.clone());
    limited.begin(&layout);
    let dt = 1.0 / old.rate;
    limited.point(0.0, &[-0.333, 99.0]);
    limited.point(dt, &[-0.333, 99.0]);
    limited.point(2.0 * dt, &[0.333, 99.0]);
    limited.point(3.0 * dt, &[0.333, 99.0]);
    assert!(calls.lock().unwrap().blocks.is_empty(), "the first hardware capture is still in progress at the edit");

    want.channels.iter_mut().find(|c| c.id == ChanId(0)).unwrap().window = (-2.0, 2.0);
    want.time.span = 0.008;
    want.time.depth = DepthWant::Samples(8);
    want.time.pre = 0.004;
    let next = Arc::new(resolve(&caps, &want, Some(&old)));
    slot.set(spec(next.clone(), &want));
    limited.point(16.25 * dt, &[0.333, 99.0]);
    {
        let calls = calls.lock().unwrap();
        assert_eq!(calls.blocks.len(), 1, "editing during capture does not discard or prematurely emit it");
        assert_eq!(calls.blocks[0].values[0].len() as u64, old.depth, "the capture in progress keeps its old depth");
        assert!(calls.blocks[0].trigger.is_some(), "the capture in progress keeps its hardware trigger");
    }

    let origin = 0.03037;
    for i in 0..4 { limited.point(origin + i as f64 * dt, &[-0.333, 99.0]); }
    limited.point(origin + 4.0 * dt, &[0.333, 99.0]);
    limited.point(origin + 8.25 * dt, &[0.333, 99.0]);
    assert_eq!(calls.lock().unwrap().blocks.len(), 2, "the next capture completes at the retuned depth");

    // Finish blind time on a falling input, so the rising detector is armed without capturing.
    limited.point(0.06037, &[-0.333, 99.0]);
    assert_eq!(calls.lock().unwrap().blocks.len(), 2, "the second edit is made while armed, with no open capture");
    want.channels.iter_mut().find(|c| c.id == ChanId(0)).unwrap().window = (-0.25, 0.25);
    want.time.rate = RateWant::Exact(2e3);
    want.time.span = 0.006;
    want.time.depth = DepthWant::Samples(12);
    want.time.pre = 0.001;
    want.trigger.slope = Slope::Falling;
    let armed = Arc::new(resolve(&caps, &want, Some(&next)));
    slot.set(spec(armed.clone(), &want));
    let armed_dt = 1.0 / armed.rate;
    let armed_origin = 0.07037;
    limited.point(armed_origin, &[0.333, 99.0]);
    limited.point(armed_origin + armed_dt, &[0.333, 99.0]);
    limited.point(armed_origin + 2.0 * armed_dt, &[-0.333, 99.0]);
    limited.point(armed_origin + 12.25 * armed_dt, &[-0.333, 99.0]);
    limited.idle();

    let calls = calls.lock().unwrap();
    assert_eq!(calls.blocks.len(), 3, "an armed retune applies to the very next hardware capture");
    assert_ne!(old.depth, next.depth, "the in-capture edit exercises a depth change");
    assert_ne!(next.depth, armed.depth, "the armed edit exercises another depth change");
    let on_grid = |value: f32, channel: &ChanResolved| {
        let code = (value as f64 - channel.range.0) / channel.step;
        (code - code.round()).abs() < 1e-4
    };
    let old_channel = old.channels.iter().find(|c| c.id == ChanId(0)).unwrap();
    let next_channel = next.channels.iter().find(|c| c.id == ChanId(0)).unwrap();
    assert_ne!(old_channel.step, next_channel.step, "the in-capture edit exercises a different ADC step");
    assert!(calls.blocks[0].values[0].iter().any(|&v| !on_grid(v, next_channel)), "old codes distinguish the old range from the pending range");
    assert!(calls.blocks[1].values[0].iter().any(|&v| !on_grid(v, old_channel)), "new codes distinguish the retuned range from the old range");
    let mut index = 0;
    for (i, (block, resolved)) in calls.blocks.iter().zip([&old, &next, &armed]).enumerate() {
        assert_eq!(resolved.mode, Mode::Block, "retune {i} fixture uses block acquisition");
        assert_eq!(resolved.trigger, TrigResolved::Hardware, "retune {i} fixture uses hardware triggering");
        assert_eq!(block.values[0].len() as u64, resolved.depth, "capture {i} uses its own resolved depth");
        assert_eq!(block.dt, 1.0 / resolved.rate, "capture {i} uses its own resolved sample clock");
        assert_eq!(block.index, index, "capture {i} continues the delivered sample index across retunes");
        assert!(block.trigger.is_some(), "capture {i} uses the rebuilt hardware detector");
        let channel = resolved.channels.iter().find(|c| c.id == ChanId(0)).unwrap();
        assert!(block.values[0].iter().all(|&v| on_grid(v, channel)), "capture {i} keeps one quantisation step throughout");
        assert!(block.values[1].iter().all(|v| v.is_nan()), "retune {i} preserves disabled channels in layout order");
        index += resolved.depth;
    }
    assert_eq!(calls.blocks[1].t0, origin, "the new clock and larger pre-trigger ring start at the next point");
    assert_eq!(calls.blocks[2].t0, armed_origin, "the armed retune replaces history and anchors the new clock immediately");
    assert!(!calls.gaps.iter().any(|&(_, _, why)| why == Gap::Restart), "retuning never emits a restart gap");
    assert_eq!(calls.gaps.len(), 2, "both retunes preserve the blind interval from the previous capture");
    for (i, &(from, to, why)) in calls.gaps.iter().enumerate() {
        let previous = &calls.blocks[i];
        assert_eq!(why, Gap::Blind, "retune {i} reports blind-time provenance");
        assert_eq!(from, previous.t0 + previous.values[0].len() as f64 * previous.dt, "retune {i} preserves the previous capture end");
        assert_eq!(to, calls.blocks[i + 1].t0, "retune {i} ends blind time at the next capture");
    }
    assert_eq!(calls.begins, [layout], "retunes do not begin a new layout");
}

#[test]
fn auto_timeout_on_flat_input_and_bounded_pretrigger_history() {
    let timeout = 0.003;
    let (layout, caps, want) = setup(ModeWant::Block, 1e5, 128, 0.01, TriggerRun::Auto(timeout));
    let (mut limited, resolved, calls) = intake(&caps, &want);
    assert_eq!(resolved.trigger, TrigResolved::Hardware, "auto fixture waits for a hardware edge");
    let dt = 1.0 / resolved.rate;
    limited.begin(&layout);
    limited.point(0.0, &[-0.3, 99.0]);
    limited.point(timeout - 0.25 * dt, &[-0.3, 99.0]);
    limited.idle();
    assert!(calls.lock().unwrap().blocks.is_empty(), "flat input produces no capture before the auto timeout");
    limited.point(timeout + 2.25 * dt, &[-0.3, 99.0]);
    let calls = calls.lock().unwrap();
    assert_eq!(calls.blocks.len(), 1, "auto forces a flat-line capture after source-time timeout");
    let block = &calls.blocks[0];
    assert_eq!(block.values[0].len() as u64, resolved.depth, "forced capture has full depth");
    let trigger = block.trigger.expect("auto capture carries its forced trigger timestamp");
    assert!(trigger >= timeout - 1e-12 && trigger <= timeout + dt, "auto timeout is measured in source seconds while armed");
    assert!((trigger - (block.t0 + (resolved.depth - 1) as f64 * dt)).abs() < 1e-12, "pre-trigger history is capped at depth minus one");
    assert!(block.values[0].iter().all(|v| (*v as f64 + 0.3).abs() < 1e-6), "forced capture contains the retained flat input");
    assert_eq!(block.clipped[0], Clipped::default(), "auto does not invent clipping");
    assert!(calls.gaps.is_empty(), "the first auto capture has no blind predecessor");
}

#[test]
fn gap_drops_partial_capture_reanchors_and_single_waits_for_begin() {
    let (mut layout, caps, mut want) = setup(ModeWant::Segmented(3), 1e3, 16, 0.005, TriggerRun::Single);
    want.trigger.kind = TriggerKind::Zone;
    let (mut limited, resolved, calls) = intake(&caps, &want);
    assert_eq!(resolved.mode, Mode::Segmented(3), "segmented fixture uses the block acquisition machine");
    assert!(matches!(resolved.trigger, TrigResolved::Software { .. }), "Zone fixture uses free-running software blocks");
    let dt = 1.0 / resolved.rate;
    limited.begin(&layout);
    limited.point(0.0, &[2.0, 99.0]);
    limited.point(0.0046, &[2.0, 99.0]);
    limited.idle();
    assert!(calls.lock().unwrap().blocks.is_empty(), "idle keeps a partial block unfinished");
    limited.gap(0.0046, 0.0317, Gap::Rearm);
    let origin = 0.03237;
    limited.point(origin, &[-0.3, 99.0]);
    limited.point(origin + 7.2 * dt, &[-0.3, 99.0]);
    limited.steady(origin + 7.2 * dt);
    limited.idle();
    assert!(calls.lock().unwrap().blocks.is_empty(), "gap drops the old partial record and steady does not complete the new one");
    limited.point(origin + 18.2 * dt, &[-0.3, 99.0]);
    {
        let calls = calls.lock().unwrap();
        assert_eq!(calls.blocks.len(), 1, "new capture survives idle and completes at depth");
        let block = &calls.blocks[0];
        assert_eq!(block.t0, origin, "ADC grid reanchors at the first point after the gap");
        assert_eq!(block.values[0].len() as u64, resolved.depth, "reanchored capture is complete");
        assert_eq!(block.trigger, None, "software capture leaves trigger search to the consumer");
        assert!(block.values[0].iter().all(|v| (*v as f64 + 0.3).abs() < 1e-6), "no pre-gap samples survive in the new capture");
        assert_eq!(block.clipped[0], Clipped::default(), "clipping from the abandoned capture is discarded");
        assert_eq!(calls.gaps, [(0.0046, 0.0317, Gap::Rearm)], "source gap is forwarded without a blind gap for discarded work");
        assert_eq!(calls.order, ["gap", "block"], "source gap precedes the first complete capture");
        assert_eq!(calls.idles, 2, "idle calls are forwarded during unfinished captures");
        assert_eq!(calls.steady, [origin + 7.2 * dt], "steady time is forwarded during unfinished captures");
    }
    limited.point(0.15, &[-0.3, 99.0]);
    limited.gap(0.15, 0.2, Gap::Restart);
    limited.point(0.2, &[-0.3, 99.0]);
    limited.point(0.3, &[-0.3, 99.0]);
    assert_eq!(calls.lock().unwrap().blocks.len(), 1, "Single stays blind even across a source gap until begin");
    layout.generation += 1;
    limited.begin(&layout);
    // A uniform input block follows the same point path and reuses the input scratch buffer.
    let values = [0.3; 20];
    let off = [99.0; 20];
    limited.block(&Block { index: 99, t0: 0.4, dt, chans: &[ChanData::F32(&values), ChanData::F32(&off)],
        trigger: Some(0.4), clipped: &[] });
    limited.point(0.5, &[0.3, 99.0]);
    let calls = calls.lock().unwrap();
    assert_eq!(calls.blocks.len(), 2, "begin rearms exactly one further Single acquisition");
    let block = &calls.blocks[1];
    assert_eq!(block.index, 0, "begin resets the delivered sample index");
    assert_eq!(block.t0, 0.4, "begin resets the ADC anchor");
    assert_eq!(block.values[0].len() as u64, resolved.depth, "input blocks obey the same capture depth");
    assert_eq!(block.trigger, None, "input block metadata does not bypass the scope trigger");
    assert_eq!(calls.begins.len(), 2, "both layouts are forwarded");
}
