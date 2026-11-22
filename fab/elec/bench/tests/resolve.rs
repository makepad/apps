use fab_elec_bench::source::*;
use fab_elec_bench::trigger::Slope;
use std::sync::Arc;

fn fixture() -> (Caps, Setup) {
    let caps = Caps { generation: 0, channels: vec![ChannelCaps {
        id: ChanId(0), name: "A".into(), kind: ChanKind::Analog, quantity: Quantity::Volt,
        ranges: Ranges::Steps(vec![(0.0, 1.0), (0.0, 2.0), (0.0, 10.0)]), bits: 8,
        coupling: vec![Coupling::Dc, Coupling::Ac], attenuation: Atten::Free, bandwidth: None, bw_limits: Vec::new(),
    }], rate: RateCaps::Any, depth: Depth::Unbounded,
        modes: Modes { stream: true, block: true, segmented: Some(8), block_period: Some(4.0) }, stream_rate: None,
        trigger: TriggerCaps { hardware: vec![TriggerKind::Edge], ..TriggerCaps::default() },
        powers: Powers::default(), outputs: Vec::new() };
    let want = Setup { channels: vec![ChannelSetup { id: ChanId(0), on: true, window: (0.0, 1.0), coupling: Coupling::Dc,
        probe: Probe::default(), bw_limit: None, invert: false }],
        time: TimeSetup { span: 1.0, pre: 0.2, rate: RateWant::Exact(100.0), depth: DepthWant::Auto, plot_px: 100 },
        trigger: TriggerSpec { kind: TriggerKind::Edge, channel: ChanId(0), level: 0.5, hyst: 0.1, slope: Slope::Rising, holdoff: 0.0, run: TriggerRun::Normal },
        mode: ModeWant::Auto, drives: Vec::new() };
    (caps, want)
}

#[test]
fn full_scale_step_edges_are_inclusive() {
    let (caps, mut want) = fixture();
    for (window, range) in [((0.0, 1.0), (0.0, 1.0)), ((0.0, 1.00001), (0.0, 2.0)), ((0.0, 2.0), (0.0, 2.0)),
        ((0.0, 2.00001), (0.0, 10.0)), ((0.0, 10.0), (0.0, 10.0))] {
        want.channels[0].window = window;
        let result = resolve(&caps, &want, None);
        assert_eq!(result.channels[0].range, range, "smallest containing range at {window:?}");
        assert!(!result.channels[0].clipped, "inclusive step boundary is not clipping at {window:?}");
        assert_eq!(result.channels[0].step, (range.1 - range.0) / 256.0, "LSB follows chosen range at {window:?}");
    }
}

#[test]
fn clipping_names_the_missing_end() {
    let (caps, mut want) = fixture();
    for (window, text) in [((0.0, 12.0), "clipped above 10 V"), ((-1.0, 1.0), "clipped below 0 V"), ((-1.0, 12.0), "clipped below 0 V and above 10 V")] {
        want.channels[0].window = window;
        let result = resolve(&caps, &want, None);
        assert_eq!(result.channels[0].range, (0.0, 10.0), "uncontained view uses largest range");
        assert!(result.channels[0].clipped, "uncontained view reports clipping");
        assert!(result.notes.iter().any(|n| n.what == NoteKind::Range(ChanId(0)) && n.text.contains(text)), "clipping note identifies channel and lost end");
    }
}

#[test]
fn probes_and_exact_ranges_use_lane_units() {
    let (mut caps, mut want) = fixture();
    caps.channels[0].attenuation = Atten::Sensed(10.0);
    caps.channels[0].coupling = vec![Coupling::Ac, Coupling::Dc];
    want.channels[0].coupling = Coupling::Gnd;
    want.channels[0].probe.transducer = 2.0;
    want.channels[0].window = (0.0, 40.0);
    let sensed = resolve(&caps, &want, None);
    assert_eq!(sensed.channels[0].range, (0.0, 2.0), "view divides by sensed attenuation and transducer");
    assert_eq!(sensed.channels[0].step, 40.0 / 256.0, "input LSB converts back to lane units");
    assert_eq!(sensed.channels[0].coupling, Coupling::Dc, "unsupported coupling prefers DC even when AC is listed first");
    assert!(sensed.notes.iter().any(|n| n.text.contains("sensed")), "sensed attenuation override is explained");
    caps.channels[0].attenuation = Atten::Fixed(vec![10.0, 1.0]);
    want.channels[0].probe.attenuation = 8.0;
    let fixed = resolve(&caps, &want, None);
    assert_eq!(fixed.channels[0].probe.attenuation, 10.0, "unsupported attenuation picks nearest offered value");
    caps.channels[0].ranges = Ranges::Any;
    caps.channels[0].bits = 0;
    want.channels[0].window = (-7.0, 42.0);
    let exact = resolve(&caps, &want, None);
    assert_eq!(exact.channels[0].range, (-0.35, 2.1), "Any preserves the exact input window");
    assert_eq!(exact.channels[0].step, 0.0, "zero bits denotes exact samples");
    assert!(!exact.channels[0].clipped, "Any never clips");
}

#[test]
fn rates_snap_up_then_depth_snaps_down() {
    let (mut caps, mut want) = fixture();
    caps.rate = RateCaps::List(vec![1000.0, 100.0, 400.0]);
    want.time.rate = RateWant::Exact(101.0);
    let up = resolve(&caps, &want, None);
    assert_eq!(up.rate, 400.0, "unsorted rate list snaps upward");
    assert_eq!(up.depth, 400, "depth uses the snapped rate");
    want.time.rate = RateWant::Exact(399.0);
    assert!(!resolve(&caps, &want, Some(&up)).restart, "different asks that snap to the same rate and depth retune live");
    assert!(up.notes.iter().any(|n| n.what == NoteKind::Rate), "upward rate snap is explained");
    want.time.rate = RateWant::Exact(1001.0);
    let ceiling = resolve(&caps, &want, None);
    assert_eq!(ceiling.rate, 1000.0, "rate above all offers uses the largest");
    caps.depth = Depth::Samples(650);
    let down = resolve(&caps, &want, None);
    assert_eq!(down.rate, 400.0, "depth snaps downward to an offered rate");
    assert_eq!(down.depth, 650, "depth keeps the hardware maximum after lowering rate");
    assert!(down.notes.iter().any(|n| n.what == NoteKind::Depth && n.text.contains("1000") && n.text.contains("650") && n.text.contains("400")), "depth note names needed depth, maximum and lowered rate");
    want.time.depth = DepthWant::Samples(100);
    let explicit = resolve(&caps, &want, None);
    assert_eq!((explicit.rate, explicit.depth), (100.0, 100), "explicit sample budget also lowers rate");
}

#[test]
fn auto_rate_range_and_unbounded_depth() {
    let (mut caps, mut want) = fixture();
    want.time.rate = RateWant::Auto { per_px: 2.5 };
    want.time.span = 0.5;
    let any = resolve(&caps, &want, None);
    assert_eq!((any.rate, any.depth), (500.0, 250), "auto rate uses plot pixels and span");
    caps.rate = RateCaps::Range { lo: 600.0, hi: 800.0 };
    let low = resolve(&caps, &want, None);
    assert_eq!(low.rate, 600.0, "rate range clamps its lower edge");
    want.time.rate = RateWant::Exact(900.0);
    let high = resolve(&caps, &want, None);
    assert_eq!(high.rate, 800.0, "rate range clamps its upper edge");
    caps.depth = Depth::Samples(350);
    let limited = resolve(&caps, &want, None);
    assert_eq!((limited.rate, limited.depth), (700.0, 350), "continuous rate falls to depth divided by span");
}

#[test]
fn hardware_trigger_requires_fine_enough_levels() {
    let (mut caps, mut want) = fixture();
    want.trigger.hyst = 4.0 / 256.0;
    let edge = resolve(&caps, &want, None);
    assert_eq!(edge.trigger, TrigResolved::Hardware, "quarter-hysteresis equality permits hardware");
    want.trigger.hyst *= 0.99;
    let coarse = resolve(&caps, &want, None);
    assert_eq!(coarse.trigger, TrigResolved::Software { over: Over::Stream }, "coarser level steps use software");
    want.trigger.hyst = 0.0;
    assert_eq!(resolve(&caps, &want, None).trigger, TrigResolved::Software { over: Over::Stream }, "zero hysteresis uses one percent of view as its band");
    caps.channels[0].bits = 10;
    assert_eq!(resolve(&caps, &want, None).trigger, TrigResolved::Hardware, "fine hardware steps satisfy zero-hysteresis fallback");
    want.trigger.kind = TriggerKind::Zone;
    assert_eq!(resolve(&caps, &want, None).trigger, TrigResolved::Software { over: Over::Stream }, "unlisted hardware kind uses software");
    want.trigger.kind = TriggerKind::Pattern;
    assert!(matches!(resolve(&caps, &want, None).trigger, TrigResolved::Unavailable(_)), "pattern without logic has no trigger path");
}

#[test]
fn block_blind_fraction_and_mode_fallbacks() {
    let (mut caps, mut want) = fixture();
    caps.trigger.hardware.clear();
    caps.stream_rate = Some(50.0);
    let block = resolve(&caps, &want, None);
    assert_eq!(block.mode, Mode::Block, "auto mode avoids an unachievable stream rate");
    assert_eq!(block.depth, 200, "software blocks hold twice the view's samples to find a trigger");
    assert_eq!(block.trigger, TrigResolved::Software { over: Over::Blocks { blind: 0.5 } }, "blind fraction uses the longer record and start-to-start block period");
    assert!(block.notes.iter().any(|n| n.what == NoteKind::Depth && n.text.contains("200") && n.text.contains("software trigger")), "longer search record is explained with its sample count");
    caps.depth = Depth::Samples(150);
    let capped = resolve(&caps, &want, None);
    assert_eq!((capped.rate, capped.depth), (100.0, 150), "search headroom clips at the depth cap without shortening the view or lowering rate");
    assert_eq!(capped.trigger, TrigResolved::Software { over: Over::Blocks { blind: 0.625 } }, "blind fraction uses the capped search record");
    caps.depth = Depth::Unbounded;
    want.mode = ModeWant::Stream;
    let fallback = resolve(&caps, &want, None);
    assert_eq!(fallback.mode, Mode::Block, "explicit unavailable stream uses block");
    assert!(fallback.notes.iter().any(|n| n.what == NoteKind::Mode), "explicit mode fallback is explained");
    caps.modes.block_period = Some(0.5);
    assert_eq!(resolve(&caps, &want, None).trigger, TrigResolved::Software { over: Over::Blocks { blind: 0.0 } }, "blind fraction is clamped at zero");
    caps.modes.block_period = None;
    let unknown = resolve(&caps, &want, None);
    assert_eq!(unknown.trigger, TrigResolved::Software { over: Over::Blocks { blind: 0.0 } }, "unknown blind time has zero numeric fraction");
    assert!(unknown.notes.iter().any(|n| n.what == NoteKind::Trigger && n.text.contains("not known")), "unknown blind time is disclosed");
    want.mode = ModeWant::Segmented(20);
    assert_eq!(resolve(&caps, &want, None).mode, Mode::Segmented(8), "segments are offered only on explicit ask and clamp to capacity");
    caps.modes.block = false; caps.modes.segmented = None; caps.stream_rate = None; want.mode = ModeWant::Block;
    assert_eq!(resolve(&caps, &want, None).mode, Mode::Stream, "unavailable block falls back to stream");
}

#[test]
fn drives_clamp_every_level_and_rate_and_refuse_shapes() {
    let (mut caps, mut want) = fixture();
    caps.outputs.push(OutputCaps { id: OutputId(0), name: "generator".into(), kind: OutputKind::Analog,
        shapes: vec![DriveKind::Dc, DriveKind::Square, DriveKind::Record], rate: (1.0, 1000.0), level: (-2.0, 3.0), load: Some(50.0) });
    want.drives = vec![DriveSetup { id: OutputId(0), on: true, drive: Drive::Square { frequency: 2000.0, duty: 0.5, lo: -5.0, hi: 6.0 } }];
    let square = resolve(&caps, &want, None);
    assert_eq!(square.drives[0].drive, Drive::Square { frequency: 1000.0, duty: 0.5, lo: -2.0, hi: 3.0 }, "generator uses real levels and frequency");
    assert!(square.drives[0].clamped && square.drives[0].on, "supported clamped output stays enabled");
    assert_eq!(square.notes.iter().filter(|n| n.what == NoteKind::Drive(OutputId(0))).count(), 3, "every square clamp gets a note");
    want.drives[0].drive = Drive::Record { rate: 0.5, values: Arc::from([-4.0_f32, 0.0, 4.0]) };
    let record = resolve(&caps, &want, None);
    assert_eq!(record.drives[0].drive, Drive::Record { rate: 1.0, values: Arc::from([-2.0_f32, 0.0, 3.0]) }, "record samples and sample rate clamp");
    assert_eq!(record.notes.iter().filter(|n| n.what == NoteKind::Drive(OutputId(0))).count(), 2, "the rate clamp and the level clamps get one note each, whatever the record length");
    assert!(record.notes.iter().any(|n| n.text.starts_with("2 record levels clamped")), "the level note counts the clamped samples");
    want.drives[0].drive = Drive::Pulse { frequency: 10.0, duty: 0.2, lo: 0.0, hi: 1.0 };
    let refused = resolve(&caps, &want, None);
    assert!(!refused.drives[0].on && refused.drives[0].refused.is_some(), "unsupported shape is refused and disabled");
    assert!(refused.notes.iter().any(|n| n.text.contains("Pulse")), "refusal names the shape");
}

#[test]
fn restart_compares_topology_to_the_applied_answer() {
    let (caps, mut want) = fixture();
    let initial = resolve(&caps, &want, None);
    assert!(initial.restart, "first apply starts a record");
    assert!(!resolve(&caps, &want, Some(&initial)).restart, "identical setup keeps the record");
    want.time.rate = RateWant::Exact(200.0);
    assert!(resolve(&caps, &want, Some(&initial)).restart, "rate change restarts");
    want.time.rate = RateWant::Exact(100.0); want.time.span = 0.5;
    assert!(resolve(&caps, &want, Some(&initial)).restart, "depth change at fixed rate restarts");
    want.time.span = 1.0; want.mode = ModeWant::Block;
    assert!(resolve(&caps, &want, Some(&initial)).restart, "mode change restarts even on the same hardware trigger path");
    want.mode = ModeWant::Auto; want.channels[0].on = false;
    assert!(resolve(&caps, &want, Some(&initial)).restart, "channel on/off change restarts");
    want.channels[0].on = true; want.channels[0].window = (0.0, 1.5);
    assert!(resolve(&caps, &want, Some(&initial)).restart, "range change restarts");
    want.channels[0].window = (0.1, 0.9); want.trigger.hyst = 0.0001;
    assert!(resolve(&caps, &want, Some(&initial)).restart, "trigger path change restarts");
    want.trigger.hyst = 0.1; want.trigger.level = 0.7; want.trigger.slope = Slope::Falling; want.trigger.holdoff = 0.3;
    want.time.pre = 0.4;
    assert!(!resolve(&caps, &want, Some(&initial)).restart, "level, slope, holdoff, pre and a view inside the range retune live");
    want.drives.push(DriveSetup { id: OutputId(9), on: true, drive: Drive::Dc(2.0) });
    assert!(!resolve(&caps, &want, Some(&initial)).restart, "drive-only changes do not restart acquisition");
}

#[test]
fn depth_below_the_minimum_rate_does_not_invent_a_device_setting() {
    let (mut caps, mut want) = fixture();
    caps.rate = RateCaps::List(vec![100.0, 200.0]);
    caps.depth = Depth::Samples(50);
    want.time.rate = RateWant::Exact(200.0);
    let result = resolve(&caps, &want, None);
    assert_eq!(result.rate, 100.0, "impossible span keeps the minimum actually offered rate");
    assert_eq!(result.depth, 50, "impossible span still respects the depth cap");
    assert!(result.notes.iter().any(|n| n.what == NoteKind::Depth && n.text.contains("cannot cover")), "impossible span is disclosed explicitly");
}

#[test]
fn empty_caps_lists_constrain_nothing() {
    let (mut caps, mut want) = fixture();
    caps.channels[0].ranges = Ranges::Steps(Vec::new());
    caps.channels[0].coupling = Vec::new();
    caps.channels[0].attenuation = Atten::Fixed(Vec::new());
    caps.rate = RateCaps::List(Vec::new());
    want.channels[0].probe.attenuation = 10.0;
    let result = resolve(&caps, &want, None);
    assert_eq!(result.channels[0].range, (0.0, 0.1), "no ranges listed: the view window at the input is the range");
    assert_eq!(result.channels[0].coupling, Coupling::Dc, "no couplings listed: the asked coupling is kept");
    assert_eq!(result.channels[0].probe.attenuation, 10.0, "no attenuations listed: the asked attenuation is kept");
    assert_eq!(result.rate, 100.0, "no rates listed: the asked rate is kept");
}

#[test]
fn range_snap_without_clipping_and_depth_limited_blind_time_are_explained() {
    let (mut caps, mut want) = fixture();
    want.channels[0].window = (0.1, 0.9);
    let result = resolve(&caps, &want, None);
    assert_eq!(result.channels[0].range, (0.0, 1.0), "smallest containing range remains selected for an inset view");
    assert!(result.notes.iter().any(|n| n.what == NoteKind::Range(ChanId(0))), "a range differing from the ask is noted without clipping");
    assert!(!result.channels[0].clipped, "range snapping alone is not measured clipping");
    caps.modes.stream = false;
    caps.depth = Depth::Samples(50);
    caps.rate = RateCaps::List(vec![100.0]);
    want.trigger.kind = TriggerKind::Zone;
    let result = resolve(&caps, &want, None);
    assert_eq!(result.trigger, TrigResolved::Software { over: Over::Blocks { blind: 0.875 } }, "blind fraction uses the actual half-second capture, not the one-second view");
    assert!(result.notes.iter().any(|n| n.what == NoteKind::Trigger && n.text.contains("87.5 %")), "software block note states the blind fraction");
}
