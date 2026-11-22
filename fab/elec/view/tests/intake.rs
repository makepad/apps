use fab_elec_bench::source::{Block, ChanData, ChanId, ChannelInfo, ClockKind, Gap, Intake, Layout, Mark, Quantity};
use fab_elec_bench::trigger::{EdgeTrigger, Slope, Window};
use fab_elec_sim::{Sink, stamp::{UnknownInfo, UnkKind}};
use fab_elec_view::instruments::acquire::{AcquireIntake, Acquisition, LaneFrame, LaneSetup, NetProbe, Published, SimChannel, SimIntake, SimLayouts};
use fab_elec_view::instruments::feed::Merger;
use fab_elec_view::instruments::phosphor::{Hits, PhosphorMap, Raster};
use std::sync::{Arc, Mutex, atomic::AtomicBool, mpsc::sync_channel};
use std::time::Duration;

fn setup(k: usize) -> (EdgeTrigger, Window, Vec<LaneSetup>, Layout) {
    let trigger = EdgeTrigger { level: 0.5, hyst: 0.1, slope: Slope::Rising, holdoff: 0.0 };
    let window = Window { pre: 2.0, post: 2.0 };
    let lanes = (0..k).map(|c| LaneSetup::time(PhosphorMap::screen(16, 6, 2.0, 4.0, -0.25, if c == 2 { 2.5 } else { 1.25 }))).collect();
    let layout = Layout { generation: 7, clock: ClockKind::Device, channels: (0..k).map(|c| ChannelInfo { id: ChanId(c as u32), name: format!("lane {c}"), quantity: Quantity::Volt }).collect() };
    (trigger, window, lanes, layout)
}

fn pictures(mode: usize, height: u32) -> LaneFrame {
    let (trigger, window, mut lanes, layout) = setup(3);
    lanes[2].map = PhosphorMap::screen(16, 6, 2.0, 4.0, -0.5, 2.5);
    // Binary column weights make exact regrouping independent of f32 addition order.
    for lane in &mut lanes { let span = lane.map.v_px * lane.map.h as f64; lane.map.h = height; lane.map.v_px = span / height as f64; }
    let raw: Vec<f32> = (0..96).map(|i| if i % 8 < 4 { 0.0 } else { 1.0 }).collect();
    let scaled: Vec<f32> = raw.iter().map(|v| v * 2.0).collect();
    let mut total = LaneFrame::new(&lanes);
    total.reset(&lanes, layout.generation);
    let mut intake = AcquireIntake::new(Acquisition::new(trigger, 0, window, lanes.clone()), Duration::MAX, |frame, _| {
        total.add(&frame);
        Published::Taken(frame)
    });
    intake.begin(&layout);
    if mode == 1 {
        let mut i = 0;
        let mut seed = 31_u32;
        while i < raw.len() {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let end = (i + 1 + (seed % 13) as usize).min(raw.len());
            let chans = [ChanData::F32(&raw[i..end]), ChanData::F32(&raw[i..end]), ChanData::F32(&scaled[i..end])];
            intake.block(&Block { index: i as u64, t0: i as f64, dt: 1.0, chans: &chans, clipped: &[], trigger: None });
            i = end;
        }
    } else {
        for (i, &v) in raw.iter().enumerate() {
            intake.point(i as f64, &[v, v, 2.0 * v]);
            if mode == 2 && i % 11 == 0 { intake.idle(); }
        }
    }
    drop(intake);
    total
}

/// The summed publications' windows as whole ones. A window open at a publication sweeps its settled
/// columns into it and the rest into a later one (`Hits::partial`); summed, each such window must
/// have reached every column once, which is what a whole window brings.
fn as_whole(mut frame: LaneFrame) -> LaneFrame {
    for lane in &mut frame.lanes {
        let done = lane.partial_done as f32;
        assert!(lane.partial.iter().all(|&n| n == done), "every swept window reached every column once: {:?} of {done}", lane.partial);
        lane.partial.clear();
        lane.partial_done = 0;
    }
    frame
}

#[test]
fn shared_trigger_preserves_lane_pictures_column_weights_and_chunking() {
    let points = pictures(0, 6);
    assert_eq!(points.lanes[0], points.lanes[1], "copy lane has bit-identical pixels and counts");
    let doubled = &points.lanes[2];
    assert_eq!(points.lanes[0], Hits { map: points.lanes[0].map, ..doubled.clone() }, "doubling both signal and value map preserves all pixels");
    assert_eq!(doubled.map.map(|m| m.v_px), points.lanes[0].map.map(|m| 2.0 * m.v_px), "the doubled lane records its own value map");
    assert_eq!(points, pictures(1, 6), "random block lengths preserve sample times, crossings and all lane hits");
    assert_eq!(points, as_whole(pictures(2, 6)), "idle frame splits preserve accumulated lane hits");
    for (c, lane) in points.lanes.iter().enumerate() {
        assert_eq!(lane.waveforms, 12, "every lane completes all twelve shared windows");
        for col in 0..lane.w {
            let weight: f32 = (0..lane.h).map(|row| lane.at(col, row)).sum();
            assert!((weight - lane.waveforms as f32).abs() < 1e-5, "lane {c}, column {col} contributes one count per complete waveform");
        }
    }
    let fractional = pictures(0, 12);
    let split = pictures(2, 12);
    for (a, b) in fractional.lanes.iter().zip(&split.lanes) {
        assert_eq!(a.waveforms, b.waveforms, "fractional column weights preserve window counts across idle");
        assert!(a.data.iter().zip(&b.data).all(|(a, b)| (a - b).abs() < 1e-6), "fractional weights differ only by f32 summation order across idle");
    }
    let (mut merger, ready) = Merger::with_lanes(&setup(3).2);
    let frame = merger.add(points.clone());
    merger.add(frame.clone());
    assert!(merger.publish(), "multi-lane sum reaches the mailbox");
    let sum = ready.try_recv().unwrap();
    for (lane, single) in sum.lanes.iter().zip(&points.lanes) {
        assert_eq!(lane.waveforms, 2 * single.waveforms, "merger adds every lane's waveform count");
        assert!(lane.data.iter().zip(&single.data).all(|(&a, &b)| a == 2.0 * b), "merger adds each lane's pixels independently");
    }
    merger.recycle(sum);
    merger.add(frame);
    let mut newer = points.clone();
    newer.generation += 1;
    for lane in &mut newer.lanes { lane.generation = newer.generation; }
    merger.add(newer.clone());
    assert!(merger.publish(), "new layout publishes after replacing an older pending sum");
    let published = ready.try_recv().unwrap();
    newer.published = published.published;
    assert_eq!(published, newer, "different layout generations never merge");
    assert_eq!(merger.dropped(), 1, "discarded pending generation is counted once");
}

#[test]
fn hardware_trigger_and_gap_bypass_detection_and_break_all_lanes() {
    let (trigger, window, lanes, layout) = setup(2);
    let raw: Vec<i16> = (0..16).map(|i| if i % 4 < 2 { -2 } else { 2 }).collect();
    let mut frames = Vec::new();
    let mut drops = 0;
    let mut intake = AcquireIntake::new(Acquisition::new(trigger, 0, window, lanes.clone()), Duration::MAX, |frame, stats| {
        drops += stats.dropped;
        frames.push(frame.clone());
        Published::Taken(frame)
    });
    intake.begin(&layout);
    let chans = [ChanData::I16 { raw: &raw, scale: 0.25, offset: 0.5 }, ChanData::I16 { raw: &raw, scale: 0.25, offset: 0.5 }];
    intake.block(&Block { index: 9, t0: 0.0, dt: 1.0, chans: &chans, clipped: &[], trigger: Some(5.25) });
    assert_eq!(intake.acquisition().events(), 0, "hardware block runs no detector search despite repeated crossings");
    assert!(intake.marks().is_empty(), "hardware event is not a software accepted-edge mark");
    intake.idle();
    intake.point(16.0, &[0.0, 0.0]);
    intake.point(17.0, &[1.0, 1.0]);
    assert_eq!(intake.acquisition().open_strokes(), 1, "software edge opens the next shared window");
    intake.gap(17.0, 18.0, Gap::Overrun);
    assert_eq!(intake.acquisition().open_strokes(), 0, "gap closes the interrupted accepted window");
    assert_eq!(intake.acquisition().ring_len(), 0, "gap clears all pre-history");
    intake.point(18.0, &[1.0, 0.0]);
    intake.point(20.0, &[1.0, 1.0]);
    drop(intake);
    assert_eq!(drops, 1, "one interrupted shared window counts as one drop");
    assert_eq!(frames.iter().map(LaneFrame::waveforms).sum::<u64>(), 1, "gap cannot complete a stroke across missing time");
    let mut reference = LaneFrame::new(&lanes);
    let t: Vec<_> = (0..16).map(|i| i as f64 - 5.25).collect();
    let y: Vec<_> = raw.iter().map(|&v| v as f32 * 0.25 + 0.5).collect();
    Raster::new().add(&lanes[0].map, &t, &y, &mut reference.lanes[0]);
    assert_eq!(frames[0].lanes[0].data, reference.lanes[0].data, "hardware window is rasterised at exactly 5.25, not an interpolated edge");
    assert_eq!(frames[0].lanes[0], frames[0].lanes[1], "hardware window uses the same span on every lane");
    assert_eq!(ChanData::U8 { raw: &[4], scale: 0.5, offset: -1.0 }.value(0), 1.0, "unsigned block conversion applies scale and offset");
    assert_eq!(ChanData::I8 { raw: &[-4], scale: 0.5, offset: 3.0 }.value(0), 1.0, "signed byte conversion applies scale and offset");
}

#[test]
fn missing_values_leave_other_lanes_unchanged_and_do_not_trigger() {
    let (trigger, window, mut lanes, layout) = setup(3);
    lanes[2] = lanes[0];
    let run = |missing: bool| {
        let mut sum = LaneFrame::new(&lanes);
        sum.reset(&lanes, layout.generation);
        let mut intake = AcquireIntake::new(Acquisition::new(trigger, 0, window, lanes.clone()), Duration::MAX, |frame, _| { sum.add(&frame); Published::Taken(frame) });
        intake.begin(&layout);
        for i in 0..9 {
            let v = if i < 4 { 0.0 } else { 1.0 };
            intake.point(i as f64, &[v, if missing && (3..=5).contains(&i) { f32::NAN } else { v }, v]);
        }
        drop(intake);
        sum
    };
    let clean = run(false);
    let missing = run(true);
    assert_eq!(missing.lanes[0], clean.lanes[0], "NaN on lane 1 preserves the trigger lane picture");
    assert_eq!(missing.lanes[2], clean.lanes[2], "NaN on lane 1 preserves independent lane 2");
    for col in 3..14 { assert!((0..missing.lanes[1].h).all(|row| missing.lanes[1].at(col, row) == 0.0), "segments touching NaN draw no lane 1 pixels in column {col}"); }
    let mut acquisition = Acquisition::new(trigger, 0, window, lanes);
    let mut frame = clean;
    for (t, v) in [(0.0, 0.0), (1.0, f32::NAN), (2.0, 1.0)] { acquisition.push(t, &[v, 0.0, 0.0], &mut frame.lanes); }
    assert_eq!(acquisition.events(), 0, "trigger NaN breaks adjacency and cannot invent a crossing");
}

#[test]
fn simulator_layout_identity_marks_capture_and_drop_are_preserved() {
    let (trigger, window, lanes, _) = setup(2);
    let unknowns = vec![UnknownInfo { name: "0".into(), kind: UnkKind::Voltage }, UnknownInfo { name: "v".into(), kind: UnkKind::Voltage }];
    let channels = (0..2).map(|c| SimChannel::voltage(ChanId(c), NetProbe { pos: "v".into(), neg: None, scale: 1.0 })).collect();
    let request = Arc::new(AtomicBool::new(true));
    let (send, captures) = sync_channel(2);
    let mut frames = Vec::new();
    let mut core = AcquireIntake::new(Acquisition::new(trigger, 0, window, lanes), Duration::ZERO, |frame, _| { frames.push(frame.clone()); Published::Taken(frame) });
    core.capture(request, send, 42);
    let mut sink = SimIntake::new(channels, core);
    sink.begin(&unknowns);
    for i in 0..=60 { sink.point(i as f64 * 0.05, None, &[0.0, 0.0]); }
    sink.mark(3.0);
    sink.point(4.0, None, &[0.0, 1.0]);
    assert_eq!(sink.intake().acquisition().open_strokes(), 1, "first edge leaves one open window");
    sink.begin(&unknowns);
    assert_eq!(sink.intake().generation(), 1, "identical unknown names and kinds keep generation");
    assert_eq!(sink.intake().acquisition().open_strokes(), 1, "identical begin keeps the open window");
    sink.point(5.0, None, &[0.0, 1.0]);
    sink.mark(5.0);
    sink.point(5.5, None, &[0.0, 1.0]);
    sink.mark(5.5);
    sink.idle();
    let captured = captures.try_recv().expect("complete capture is published on idle");
    assert_eq!(captured.samples.lanes[0], captured.samples.lanes[1], "capture retains a sample column for every lane");
    assert_eq!(captured.samples.times.first(), Some(&1.5), "capture interpolates the common pre boundary");
    assert_eq!(captured.samples.times.last(), Some(&5.5), "capture interpolates the common post boundary");
    assert_eq!(captured.samples.marks, [(3.0, Mark::Solver), (5.0, Mark::Solver), (5.5, Mark::Solver)], "capture retains pre-history and the mark after a 64th-point clock flush");
    assert_eq!(sink.marks(), &[3.5], "marks() remains accepted detector crossings only");
    sink.point(6.0, None, &[0.0, 0.0]);
    sink.point(7.0, None, &[0.0, 1.0]);
    let mut changed = unknowns.clone();
    changed[0].name = "ground".into();
    sink.begin(&changed);
    assert_eq!(sink.intake().generation(), 2, "changed names bump the source generation");
    assert_eq!(sink.intake().acquisition().open_strokes(), 0, "changed layout discards the unfinished window");
    for i in 8..=13 { sink.point(i as f64, None, &[0.0, if i < 11 { 0.0 } else { 1.0 }]); }
    drop(sink);
    assert_eq!(frames.last().unwrap().generation, 2, "Drop publishes the current source generation without idle");
    assert_eq!(frames.last().unwrap().waveforms(), 1, "Drop publishes the final buffered complete window");
}

#[derive(Default)]
struct Recorded { layouts: Vec<Layout>, values: Vec<Vec<f32>> }
impl Intake for Recorded {
    fn begin(&mut self, layout: &Layout) { self.layouts.push(layout.clone()); }
    fn point(&mut self, _: f64, y: &[f32]) { self.values.push(y.to_vec()); }
    fn block(&mut self, _: &Block) { unreachable!() }
    fn mark(&mut self, _: f64, _: Mark) {}
    fn gap(&mut self, _: f64, _: f64, _: Gap) {}
    fn idle(&mut self) {}
}

#[test]
fn simulated_ramp_is_exact_or_adc_blocks_with_blind_time() {
    use fab_elec_bench::{limited::LimitedIntake, source::*};
    use fab_elec_instr::Limits;
    use fab_elec_view::instruments::simfeed::LimitedSpec;

    #[derive(Default)]
    struct Captured {
        points: Vec<(f64, f32)>, blocks: Vec<(f64, f64, Vec<f32>, Clipped, Option<f64>)>,
        gaps: Vec<(f64, f64, Gap)>, order: Vec<&'static str>,
    }
    struct Record<'a>(&'a mut Captured);
    impl Intake for Record<'_> {
        fn begin(&mut self, _: &Layout) {}
        fn point(&mut self, t: f64, y: &[f32]) { self.0.points.push((t, y[0])); }
        fn block(&mut self, block: &Block) {
            self.0.blocks.push((block.t0, block.dt, (0..block.chans[0].len()).map(|i| block.chans[0].value(i)).collect(), block.clipped[0], block.trigger));
            self.0.order.push("block");
        }
        fn gap(&mut self, from: f64, to: f64, why: Gap) { self.0.gaps.push((from, to, why)); self.0.order.push("gap"); }
        fn mark(&mut self, _: f64, _: Mark) {}
        fn idle(&mut self) {}
    }

    let channels = vec![SimChannel::voltage(ChanId(0), NetProbe { pos: "ramp".into(), neg: None, scale: 1.0 })];
    let caps = Limits::bench_scope().caps(channels.iter().map(|c| c.info.clone()).collect());
    let want = Setup { channels: vec![ChannelSetup { id: ChanId(0), on: true, window: (-0.015, 0.015), coupling: Coupling::Dc,
        probe: Probe::default(), bw_limit: None, invert: false }],
        time: TimeSetup { span: 0.002, pre: 0.0, rate: RateWant::Exact(1e5), depth: DepthWant::Auto, plot_px: 100 },
        trigger: TriggerSpec { kind: TriggerKind::Width, channel: ChanId(0), level: 0.0, hyst: 0.001, slope: Slope::Rising, holdoff: 0.0, run: TriggerRun::Normal },
        mode: ModeWant::Block, drives: Vec::new() };
    let resolved = Arc::new(resolve(&caps, &want, None));
    assert_eq!(resolved.trigger, TrigResolved::Software { over: Over::Blocks { blind: 0.8 } }, "Width stays a software trigger over blind blocks with a record twice the view");
    let spec = LimitedSpec { bits: caps.channels.iter().map(|c| c.bits).collect(), resolved: resolved.clone(), trigger: want.trigger,
        pre: want.time.pre, block_period: caps.modes.block_period };
    let times = [0.0, 0.00037, 0.00183, 0.0041, 0.013, 0.0207, 0.025];
    let ramp = |t: f64| -0.01 + 2.0 * t;
    let record = |limited: Option<LimitedSpec>| {
        let mut captured = Captured::default();
        let out = Record(&mut captured);
        let intake: Box<dyn Intake + '_> = match limited {
            Some(spec) => Box::new(LimitedIntake::new(out, spec.bits, spec.resolved, spec.trigger, spec.pre, spec.block_period)),
            None => Box::new(out),
        };
        let mut sink = SimIntake::new(channels.clone(), intake);
        sink.begin(&[UnknownInfo { name: "0".into(), kind: UnkKind::Voltage }, UnknownInfo { name: "ramp".into(), kind: UnkKind::Voltage }]);
        for t in times { sink.point(t, None, &[0.0, ramp(t)]); }
        sink.idle();
        drop(sink);
        captured
    };
    let exact = record(None);
    let limited = record(Some(spec));
    assert_eq!(exact.points, times.into_iter().map(|t| (t, ramp(t) as f32)).collect::<Vec<_>>(), "exact keeps every simulator value and adaptive timestamp");
    assert!(exact.blocks.is_empty() && exact.gaps.is_empty(), "exact introduces neither blocks nor blind time");
    assert!(limited.points.is_empty(), "the limited simulator delivers ADC blocks");
    assert_eq!(limited.order, ["block", "gap", "block"], "a blind gap separates the two captures");
    assert_eq!(limited.gaps.len(), 1, "one blind interval separates the captures");
    let (from, to, why) = limited.gaps[0];
    assert_eq!(why, Gap::Blind, "the interval has blind-time provenance");
    assert!((from - resolved.depth as f64 / resolved.rate).abs() < 1e-12 && (to - 0.02).abs() < 1e-12,
        "blind time runs from capture end to the next 20 ms block");
    let channel = &resolved.channels[0];
    for (n, (t0, dt, values, clipped, trigger)) in limited.blocks.iter().enumerate() {
        assert_eq!(*t0, n as f64 * 0.02, "blocks use circuit time and the advertised cadence");
        assert_eq!(*dt, 1.0 / resolved.rate, "ADC uses the resolved finite sample rate");
        assert_eq!(values.len() as u64, resolved.depth, "each block has the resolved depth");
        assert_eq!(*trigger, None, "software trigger blocks never claim a hardware timestamp");
        assert_eq!(*clipped, Clipped { low: false, high: n == 1 }, "only the over-range second block clips");
        for (i, &value) in values.iter().enumerate() {
            let code = (value as f64 - channel.range.0) / channel.step;
            assert!((code - code.round()).abs() < 1e-4 && (-0.0001..=255.0001).contains(&code), "limited values lie on the eight-bit code grid");
            let expected = ((ramp(t0 + i as f64 * dt) - channel.range.0) / channel.step).round().clamp(0.0, 255.0);
            assert!((code - expected).abs() < 1e-4, "ADC samples the closed-form ramp, then rounds and saturates");
        }
    }
    assert!(limited.blocks[0].2.windows(2).any(|v| v[0] == v[1]), "the small ramp has visible quantisation steps");
    assert!(exact.points.iter().any(|&(_, value)| value as f64 > channel.range.1), "exact preserves values above the bench scope's range");
}

#[test]
fn bench_scope_blocks_draw_and_capture_at_the_bench_setup() {
    use fab_elec_bench::{limited::LimitedIntake, source::*};
    use fab_elec_instr::Limits;
    use fab_elec_view::instruments::acquire::Capture;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct PerBlock<'a, I> { inner: I, waveforms: &'a AtomicU64, counts: &'a mut Vec<u64> }
    impl<I: Intake> Intake for PerBlock<'_, I> {
        fn begin(&mut self, layout: &Layout) { self.inner.begin(layout); }
        fn point(&mut self, t: f64, y: &[f32]) { self.inner.point(t, y); }
        fn block(&mut self, block: &Block) {
            let before = self.waveforms.load(Ordering::Relaxed);
            self.inner.block(block);
            // Publish each record separately so an extra waveform cannot hide a blank block.
            self.inner.idle();
            self.counts.push(self.waveforms.load(Ordering::Relaxed) - before);
        }
        fn mark(&mut self, t: f64, kind: Mark) { self.inner.mark(t, kind); }
        fn gap(&mut self, from: f64, to: f64, why: Gap) { self.inner.gap(from, to, why); }
        fn idle(&mut self) { self.inner.idle(); }
        fn steady(&mut self, at: f64) { self.inner.steady(at); }
        fn pending(&self) -> bool { self.inner.pending() }
    }

    let channels = vec![SimChannel::voltage(ChanId(0), NetProbe { pos: "sq".into(), neg: None, scale: 1.0 })];
    let caps = Limits::bench_scope().caps(channels.iter().map(|c| c.info.clone()).collect());
    let span = 200e-6;
    let pre = 50e-6;
    let mut results = Vec::new();
    for hyst in [0.05, 0.4] {
        let want = Setup { channels: vec![ChannelSetup { id: ChanId(0), on: true, window: (-2.0, 6.0), coupling: Coupling::Dc,
            probe: Probe::default(), bw_limit: None, invert: false }],
            time: TimeSetup { span, pre, rate: RateWant::Auto { per_px: 2.0 }, depth: DepthWant::Auto, plot_px: 1920 },
            trigger: TriggerSpec { kind: TriggerKind::Edge, channel: ChanId(0), level: 1.65, hyst, slope: Slope::Rising, holdoff: 0.0, run: TriggerRun::Normal },
            mode: ModeWant::Block, drives: Vec::new() };
        let resolved = Arc::new(resolve(&caps, &want, None));
        let trigger = EdgeTrigger { level: 1.65, hyst, slope: Slope::Rising, holdoff: 0.0 };
        let window = Window { pre, post: span - pre };
        let lanes = vec![LaneSetup::time(PhosphorMap::screen(1920, 200, pre, span, -2.0, 6.0))];
        let waveforms = AtomicU64::new(0);
        let mut blocks = Vec::new();
        let (send, captures) = sync_channel::<Capture>(64);
        let request = Arc::new(AtomicBool::new(true));
        {
            let mut acquisition = AcquireIntake::new(Acquisition::new(trigger, 0, window, lanes.clone()), Duration::ZERO, |frame: LaneFrame, _| {
                waveforms.fetch_add(frame.waveforms(), Ordering::Relaxed);
                Published::Taken(frame)
            });
            acquisition.capture(request.clone(), send, 1);
            let acquisition = PerBlock { inner: acquisition, waveforms: &waveforms, counts: &mut blocks };
            let limited = LimitedIntake::new(acquisition, caps.channels.iter().map(|c| c.bits).collect(), resolved.clone(), want.trigger.clone(), pre, caps.modes.block_period);
            let mut sink = SimIntake::new(channels.clone(), limited);
            sink.begin(&[UnknownInfo { name: "0".into(), kind: UnkKind::Voltage }, UnknownInfo { name: "sq".into(), kind: UnkKind::Voltage }]);
            // A 10 kHz, 10 % square with 4 ns edges, as breakpoints: 0.2 s of circuit time.
            let mut t = 0.0;
            while t < 0.2 {
                for (dt, v) in [(0.0, 0.0), (4e-9, 3.3), (10e-6, 3.3), (10e-6 + 4e-9, 0.0)] { sink.point(t + dt, None, &[0.0, v]); }
                t += 100e-6;
            }
            sink.idle();
        }
        results.push((hyst, resolved, blocks, captures.try_iter().collect::<Vec<_>>()));
    }
    for (hyst, resolved, blocks, captures) in results {
        assert_eq!(blocks.len(), 10, "hyst {hyst}: 0.2 s must deliver ten bench scope blocks");
        assert_eq!(captures.len(), 1, "hyst {hyst}: the one-shot request must deliver exactly one capture ({:?})", resolved.trigger);
        if hyst == 0.4 {
            assert_eq!(resolved.trigger, TrigResolved::Hardware, "wide hysteresis must exercise hardware blocks");
            for (i, &drawn) in blocks.iter().enumerate() { assert_eq!(drawn, 1, "hardware block {i} must draw exactly one waveform"); }
            let capture = &captures[0];
            let first = *capture.samples.times.first().expect("hardware capture must have samples");
            let last = *capture.samples.times.last().unwrap();
            let start = capture.trigger - pre;
            assert!(first >= start && first - start <= 1.0 / resolved.rate, "hardware capture must start at or within one sample interval after trigger - pre");
            assert!((last - (capture.trigger + span - pre)).abs() < 1e-12, "hardware capture must end exactly at trigger + post");
            let channel = &resolved.channels[0];
            assert_eq!(capture.samples.lanes[0].len(), capture.samples.times.len(), "hardware capture must retain one ADC value per sample time");
            for &value in &capture.samples.lanes[0] {
                let code = (value as f64 - channel.range.0) / channel.step;
                assert!((code - code.round()).abs() < 1e-4, "hardware capture values must stay on the resolved ADC code grid");
            }
        } else {
            assert!(matches!(resolved.trigger, TrigResolved::Software { over: Over::Blocks { .. } }), "narrow hysteresis must exercise software search over blocks");
            for (i, &drawn) in blocks.iter().enumerate() { assert!(drawn >= 1, "software block {i} must draw at least one waveform"); }
        }
    }
}

#[test]
fn amp_channels_resolve_branch_unknowns_and_report_missing_names() {
    let mut unknowns = vec![UnknownInfo { name: "0".into(), kind: UnkKind::Voltage }, UnknownInfo { name: "V1#branch".into(), kind: UnkKind::Current }];
    let channels = ["V1#branch", "missing", "i(R4)"].iter().enumerate().map(|(c, name)| SimChannel {
        info: ChannelInfo { id: ChanId(c as u32), name: (*name).into(), quantity: Quantity::Amp },
        probe: NetProbe { pos: (*name).into(), neg: None, scale: 2.0 },
    }).collect();
    let mut sink = SimIntake::new(channels, Recorded::default());
    sink.begin(&unknowns);
    sink.point(0.0, None, &[0.0, -0.125]);
    assert_eq!(sink.intake().values[0][0], -0.25, "branch current is read directly by name and scaled");
    assert!(sink.intake().values[0][1..].iter().all(|v| v.is_nan()), "missing and derived device currents remain NaN");
    assert_eq!(sink.unresolved_channels(), &[false, true, true], "unresolved status identifies individual missing channels");
    assert!(sink.unresolved(), "one missing current reports unresolved to existing callers");
    sink.begin(&unknowns);
    assert_eq!(sink.intake().layouts.len(), 1, "identical begins do not reach Intake again");
    unknowns[1].kind = UnkKind::Voltage;
    sink.begin(&unknowns);
    sink.point(1.0, None, &[0.0, 12.0]);
    assert!(sink.intake().values[1][0].is_nan(), "a same-name voltage is never interpreted as current");
    assert_eq!(sink.intake().layouts.last().unwrap().generation, 2, "kind-only changes bump the layout generation");
    assert_eq!(sink.intake().layouts.last().unwrap().channels.len(), 3, "unresolved channels stay in layout order");
    let registry = Arc::new(Mutex::new(SimLayouts::default()));
    let channel = SimChannel::voltage(ChanId(0), NetProbe { pos: "v".into(), neg: None, scale: 1.0 });
    let mut first = SimIntake::new(vec![channel.clone()], Recorded::default());
    first.follow_layouts(registry.clone());
    first.begin(&unknowns);
    let mut newer = unknowns.clone();
    newer[1].name = "v".into();
    first.begin(&newer);
    let mut late = SimIntake::new(vec![channel], Recorded::default());
    late.follow_layouts(registry);
    late.begin(&newer);
    assert_eq!(late.intake().layouts[0].generation, 2, "a late ensemble member shares the current layout instead of colliding with generation 1");
    first.begin(&unknowns);
    late.begin(&unknowns);
    assert_eq!(first.intake().layouts.last().unwrap().generation, 3, "returning to an earlier layout still bumps the epoch");
    assert_eq!(late.intake().layouts.last().unwrap().generation, 3, "ensemble members agree on the epoch of the return transition");
}

#[test]
fn each_lane_can_choose_its_own_spectrum_and_missing_fft_samples_stay_blank() {
    use fab_elec_view::instruments::scope::FftSettings;
    let (trigger, window, mut lanes, layout) = setup(3);
    lanes[1].spectrum = Some((FftSettings::default(), (lanes[1].map.w, lanes[1].map.h)));
    lanes[2] = lanes[1];
    let mut frames = Vec::new();
    let mut spectra = Vec::new();
    let mut intake = AcquireIntake::new(Acquisition::new(trigger, 0, window, lanes), Duration::MAX, |frame, stats| {
        spectra = stats.spectrum;
        frames.push(frame.clone());
        Published::Taken(frame)
    });
    intake.begin(&layout);
    let t: Vec<_> = (0..=1024).map(|i| i as f64 / 128.0).collect();
    let y: Vec<_> = t.iter().map(|&t| (std::f64::consts::TAU * t).sin() as f32).collect();
    let mut missing = y.clone();
    missing[400] = f32::NAN;
    let chans = [ChanData::F32(&y), ChanData::F32(&y), ChanData::F32(&missing)];
    intake.block(&Block { index: 0, t0: 0.0, dt: 1.0 / 128.0, chans: &chans, clipped: &[], trigger: Some(4.0) });
    drop(intake);
    assert_eq!(frames[0].lanes[0].waveforms, 1, "time lane completes the hardware window");
    assert_eq!(frames[0].lanes[1], frames[0].lanes[0], "opening a lens keeps the same time picture alongside its spectrum");
    assert_eq!(frames[0].spectra[1].as_ref().unwrap().waveforms, 1, "FFT lane completes the same hardware window");
    assert_eq!(frames[0].spectra[2].as_ref().unwrap().waveforms, 0, "missing FFT input cannot fabricate a complete spectrum");
    assert!(frames[0].spectra[2].as_ref().unwrap().data.iter().all(|&v| v == 0.0), "FFT with missing input draws nothing");
    assert!(spectra[0].measurements.is_none() && spectra[2].measurements.is_none(), "time and missing FFT lanes have no spectrum measurements");
    assert!((spectra[1].measurements.unwrap().peak_f - 1.0).abs() < 1e-12, "FFT measurements use their lane and the shared four-second window");
}

#[test]
fn selecting_a_trigger_lane_preserves_history_without_inventing_a_crossing() {
    use fab_elec_bench::trigger::Edge;
    let (trigger, window, lanes, _) = setup(3);
    let mut acquisition = Acquisition::new(trigger, 1, window, lanes.clone());
    let mut frame = LaneFrame::new(&lanes);
    acquisition.push(0.0, &[0.0, 0.0, 1.0], &mut frame.lanes);
    assert_eq!(acquisition.push(1.0, &[0.0, 1.0, 1.0], &mut frame.lanes), Edge::Accepted(0.5), "chosen lane 1 triggers while lane 0 is constant");
    acquisition.push(2.0, &[0.0, 0.0, 1.0], &mut frame.lanes);
    acquisition.set_lanes(2, window, lanes);
    assert_eq!(acquisition.push(3.0, &[0.0, 1.0, 1.0], &mut frame.lanes), Edge::None, "changing trigger lanes never crosses from the old lane's last value");
    acquisition.push(4.0, &[0.0, 0.0, 0.0], &mut frame.lanes);
    assert_eq!(acquisition.push(5.0, &[0.0, 0.0, 1.0], &mut frame.lanes), Edge::Accepted(4.5), "the new trigger lane arms and interpolates its next real crossing");
    acquisition.push(7.0, &[0.0, 0.0, 1.0], &mut frame.lanes);
    assert_eq!(acquisition.events(), 2, "retuning the trigger lane preserves accepted-event counts");
    assert!(frame.lanes.iter().all(|lane| lane.waveforms == 2), "old accepted windows survive the trigger lane change on every picture");
}

#[test]
fn a_steady_source_completes_one_held_sweep_then_resumes_observed() {
    let (trigger, window, lanes, _) = setup(2);
    let unknowns = vec![UnknownInfo { name: "0".into(), kind: UnkKind::Voltage }, UnknownInfo { name: "v".into(), kind: UnkKind::Voltage }];
    let channels = || (0..2).map(|c| SimChannel::voltage(ChanId(c), NetProbe { pos: "v".into(), neg: None, scale: 1.0 })).collect::<Vec<_>>();
    let request = Arc::new(AtomicBool::new(false));
    let (send, captures) = sync_channel(2);
    let (published, frames) = std::sync::mpsc::channel::<LaneFrame>();
    let mut acquisition = Acquisition::new(trigger, 0, window, lanes.clone());
    acquisition.set_auto(Some(3.0));
    let mut core = AcquireIntake::new(acquisition, Duration::ZERO, move |frame, _| { published.send(frame.clone()).unwrap(); Published::Taken(frame) });
    core.capture(request.clone(), send, 7);
    let mut sink = SimIntake::new(channels(), core);
    sink.begin(&unknowns);
    // A rail settled at 0.25 (under the level: no edge) by t = 1, where the source stops its clock.
    for i in 0..=10 { sink.point(i as f64 * 0.1, None, &[0.0, 0.25]); }
    sink.steady(1.0, &[0.0, 0.25]);
    sink.idle();
    let shown: Vec<LaneFrame> = frames.try_iter().filter(|f| f.waveforms() > 0).collect();
    assert_eq!(shown.iter().map(|f| f.waveforms()).sum::<u64>(), 1, "the steady hold completes exactly one auto sweep");
    for (c, lane) in shown[0].lanes.iter().enumerate() {
        assert!(lane.held.is_some_and(|from| from <= -window.pre), "lane {c}: the whole sweep is marked held");
        for col in 0..lane.w {
            let weight: f32 = (0..lane.h).map(|row| lane.at(col, row)).sum();
            assert!((weight - 1.0).abs() < 1e-5, "lane {c}, column {col}: the held level draws across the whole window");
        }
    }
    let captured = captures.try_recv().expect("the held sweep is measured like any window");
    assert!(captured.samples.marks.iter().any(|&(_, kind)| kind == Mark::Held), "the measured window carries held marks");
    assert!(captured.samples.lanes.iter().all(|lane| lane.iter().all(|&v| v == 0.25)), "held samples repeat the last values");
    // The source runs again from where its clock stopped: observed points restart the windows.
    for i in 1..=60 { sink.point(1.0 + i as f64 * 0.1, None, &[0.0, if i < 10 { 0.25 } else { 1.0 }]); }
    sink.idle();
    let observed: Vec<LaneFrame> = frames.try_iter().filter(|f| f.waveforms() > 0).collect();
    assert!(!observed.is_empty(), "observed points after a hold complete new windows");
    assert!(observed.iter().all(|f| f.lanes.iter().all(|lane| lane.held.is_none())), "observed windows are never marked held");
    // A normal trigger on a level that never crosses has nothing to complete: no held picture.
    let (published, frames) = std::sync::mpsc::channel::<LaneFrame>();
    let core = AcquireIntake::new(Acquisition::new(trigger, 0, window, lanes), Duration::ZERO, move |frame, _| { published.send(frame.clone()).unwrap(); Published::Taken(frame) });
    let mut sink = SimIntake::new(channels(), core);
    sink.begin(&unknowns);
    for i in 0..=10 { sink.point(i as f64 * 0.1, None, &[0.0, 0.25]); }
    sink.steady(1.0, &[0.0, 0.25]);
    sink.idle();
    assert!(frames.try_iter().all(|f| f.waveforms() == 0), "a normal trigger waits: a steady source invents no sweep");
}

#[test]
fn source_clipping_follows_frames_and_clears_on_recycle() {
    use fab_elec_bench::source::Clipped;
    let (trigger, window, lanes, layout) = setup(2);
    let mut frames = Vec::new();
    let mut intake = AcquireIntake::new(Acquisition::new(trigger, 0, window, lanes), Duration::MAX, |frame: LaneFrame, _| {
        frames.push(frame.clone()); Published::Taken(frame)
    });
    intake.begin(&layout);
    let values = [0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0, 0.0];
    let chans = [ChanData::F32(&values), ChanData::F32(&values)];
    let flags = [Clipped { low: true, high: false }, Clipped { low: false, high: true }];
    intake.block(&Block { index: 0, t0: 0.0, dt: 1.0, chans: &chans, trigger: Some(3.0), clipped: &flags });
    intake.idle();
    intake.gap(8.0, 20.0, Gap::Blind);
    intake.block(&Block { index: 8, t0: 20.0, dt: 1.0, chans: &chans, trigger: Some(23.0), clipped: &[] });
    intake.idle();
    drop(intake);
    assert_eq!(frames.len(), 2, "two explicit publications yield two lane frames");
    assert_eq!(frames[0].lanes[0].clipped, flags[0], "lower clipping belongs to its source lane");
    assert_eq!(frames[0].lanes[1].clipped, flags[1], "upper clipping belongs to its source lane");
    assert!(frames[1].lanes.iter().all(|lane| !lane.clipped.any()), "recycled frames do not retain an earlier capture's saturation");
    let mut merged = frames[0].clone(); merged.add(&frames[1]);
    assert_eq!(merged.lanes[0].clipped, flags[0], "frame merging retains measured clipping");
}
