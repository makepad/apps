use fab_elec_bench::trigger::{EdgeTrigger, Slope, Window};
use fab_elec_sim::live::{LiveCmd, ValuePlan};
use fab_elec_sim::livesim::{LiveSim, LiveSpec, SinkCmd, SinkSet};
use fab_elec_sim::stamp::{UnkKind, UnknownInfo};
use fab_elec_sim::{GpioParams, PinMode, SimCircuit, Sink, Source, Wave};
use fab_elec_view::instruments::acquire::{AcquireIntake, LaneFrame, LaneSetup, SimIntake, AcquireStats, Acquisition, NetProbe, Published};
use fab_elec_view::instruments::phosphor::PhosphorMap;
use fab_elec_view::instruments::simfeed::{SimScopeConfig, SimScopeFeed};
use makepad_widgets_core::Cx;
use std::sync::{Arc, Mutex};
use std::time::Duration;

type Publisher = Box<dyn FnMut(LaneFrame, AcquireStats) -> Published + Send>;

#[derive(Default)]
struct Report {
    frames: Vec<LaneFrame>,
    samples: u64,
    accepted: Vec<f64>,
    source_edges: Vec<f64>,
    points: Vec<(f64, Vec<f64>)>,
    unresolved: bool,
    restarted: Option<f64>,
}

impl Report {
    fn waveforms(&self) -> u64 {
        self.frames.iter().map(|frame| frame.waveforms()).sum()
    }
}

struct Observed {
    sink: SimIntake<AcquireIntake<Publisher>>,
    report: Arc<Mutex<Report>>,
    unknowns: Vec<UnknownInfo>,
    restart_at: Option<f64>,
}

impl Sink for Observed {
    fn begin(&mut self, unknowns: &[UnknownInfo]) {
        self.unknowns = unknowns.to_vec();
        self.sink.begin(unknowns);
        self.report.lock().unwrap().unresolved = self.sink.unresolved();
    }

    fn point(&mut self, at: f64, outer: Option<f64>, x: &[f64]) {
        if self.restart_at.is_some_and(|restart| at >= restart) {
            self.sink.begin(&self.unknowns);
            self.restart_at = None;
            self.report.lock().unwrap().restarted = Some(at);
        }
        self.sink.point(at, outer, x);
        let mut report = self.report.lock().unwrap();
        report.accepted.append(self.sink.marks());
        report.points.push((at, x.to_vec()));
    }

    fn mark(&mut self, at: f64) {
        self.sink.mark(at);
        self.report.lock().unwrap().source_edges.push(at);
    }

    fn below_res(&mut self, at: f64) {
        self.sink.below_res(at);
    }

    fn idle(&mut self) {
        self.sink.idle();
    }
}

fn observe(name: &str, trigger: EdgeTrigger, window: Window, map: PhosphorMap, restart_at: Option<f64>) -> (Observed, Arc<Mutex<Report>>) {
    let report = Arc::new(Mutex::new(Report::default()));
    let published = report.clone();
    let callback: Publisher = Box::new(move |frame, stats| {
        let mut report = published.lock().unwrap();
        report.samples += stats.samples;
        report.frames.push(frame);
        Published::Taken(LaneFrame::single(map.w, map.h))
    });
    let sink = SimIntake::by_name(
        NetProbe { pos: name.into(), neg: None, scale: 1.0 },
        Acquisition::new(trigger, 0, window, vec![LaneSetup::time(map)]),
        Duration::MAX,
        callback,
    );
    (Observed { sink, report: report.clone(), unknowns: Vec::new(), restart_at }, report)
}

#[test]
fn live_scope_acquires_named_nets_and_restarts_without_losing_frames() {
    let period = 1.0 / 10_000.0;
    let rise = period / 1000.0;
    let delay = period / 4.0;
    let stop = 20.0 * period;
    let trigger = EdgeTrigger { level: 1.65, hyst: 0.1, slope: Slope::Rising, holdoff: 0.0 };
    let window = Window { pre: period / 10.0, post: period / 10.0 };
    let map = PhosphorMap::screen(32, 24, window.pre, window.pre + window.post, -0.2, 3.5);
    let mut circuit = SimCircuit::new();
    circuit.v("drive", "pwm", "0", Source::wave(Wave::Pulse { v1: 0.0, v2: 3.3, td: delay, tr: rise, tf: rise, pw: period * 0.3 - rise, per: period }));
    circuit.r("series", "pwm", "out", 1000.0);
    circuit.c("shunt", "out", "0", 1e-8);
    let plan = ValuePlan { nets: vec!["0".into(), "pwm".into(), "out".into()], ..ValuePlan::default() };
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, max_step: period / 100.0, trace: false, ..LiveSpec::default() };
    let mut sim = LiveSim::new(circuit, plan, 1, spec).expect("pulse RC initial conditions converge");
    let unknowns = sim.solver().unknowns.clone();
    let (commands, mut sinks) = SinkSet::new();
    let restart_at = delay + 10.0 * period + rise * 0.5 + window.post * 0.5;
    let (nominal, normal) = observe("pwm", trigger, window, map, None);
    let (missing, absent) = observe("missing", trigger, window, map, None);
    let (restarted, lowered) = observe("pwm", trigger, window, map, Some(restart_at));
    for (id, sink) in [(1, nominal), (2, missing), (3, restarted)] {
        assert!(commands.try_send(SinkCmd::Add(id, Box::new(sink))).is_ok(), "acquisition installation fits the sink channel");
    }
    sinks.begin(&unknowns);
    sinks.point(0.0, None, &sim.solver().x);
    sim.values(1.0);
    assert!(sim.apply(LiveCmd::Step(stop)), "twenty-period Step keeps the solver alive");
    while sim.running() {
        sim.advance(f64::INFINITY, None, &mut sinks).expect("pulse RC transient converges");
        sim.values(1.0);
    }
    assert!(normal.lock().unwrap().frames.is_empty(), "partial frames wait for idle when the wall interval has not elapsed");
    sinks.idle();

    let normal = normal.lock().unwrap();
    let absent = absent.lock().unwrap();
    let lowered = lowered.lock().unwrap();
    let expected: Vec<_> =
        (0..20).map(|index| delay + index as f64 * period + rise * 0.5).filter(|&time| time - window.pre >= 0.0 && time + window.post <= stop).collect();
    assert_eq!(normal.waveforms(), expected.len() as u64, "idle publishes exactly the rising edges with complete acquisition windows");
    assert_eq!(normal.frames.len(), 1, "idle publishes the partial frame immediately");
    assert_eq!(normal.samples, sim.stats().points + 1, "frame sample totals include every accepted point and the initial point");
    assert!(!normal.unresolved, "the named voltage net resolves on lowering");
    assert_eq!(normal.accepted.len(), expected.len(), "each rising edge produces one accepted trigger");
    assert!(
        normal.accepted.iter().zip(&expected).all(|(actual, expected)| (actual - expected).abs() < rise * 1e-5),
        "accepted triggers interpolate the pulse's mid-level crossings"
    );
    // Source marks are ramp corners; the mid-level trigger is half a rise before its upper corner.
    for time in &expected {
        let corner = time + rise * 0.5;
        let marked = normal.source_edges.iter().copied().find(|at| (at - corner).abs() < rise * 1e-5).expect("each rising source edge has a solver mark");
        assert!(
            normal.accepted.iter().any(|accepted| (accepted - (marked - rise * 0.5)).abs() < rise * 1e-5),
            "each marked rising edge has its mid-level time among the accepted triggers"
        );
    }
    assert!(absent.unresolved, "an unknown net reports unresolved");
    assert_eq!(absent.waveforms(), 0, "an unknown net acquires no waveforms");
    assert!(absent.accepted.is_empty(), "an unknown net accepts no triggers");
    let boundary = lowered.restarted.expect("a second lowering occurs mid-run");
    assert_eq!(lowered.waveforms(), expected.len() as u64, "identical unknown names and kinds preserve the open window across begin");
    assert!(lowered.accepted.iter().any(|&time| time > boundary), "triggers continue after a second begin");

    let mut deliveries = Vec::new();
    let mut attempts = 0;
    let mut retained = None;
    let mut replay = SimIntake::by_name(
        NetProbe { pos: "pwm".into(), neg: Some("0".into()), scale: 1.0 },
        Acquisition::new(trigger, 0, window, vec![LaneSetup::time(map)]),
        Duration::MAX,
        |frame, stats| {
            attempts += 1;
            if attempts == 1 {
                retained = Some(frame.clone());
                Published::Kept(frame)
            } else {
                if attempts == 2 {
                    assert_eq!(retained.as_ref(), Some(&frame), "a full queue retains every pixel and waveform for the next idle retry");
                }
                deliveries.push((frame.waveforms(), stats.samples));
                Published::Taken(frame)
            }
        },
    );
    replay.begin(&unknowns);
    for (at, x) in &normal.points {
        replay.point(*at, None, x);
    }
    replay.idle();
    replay.idle();
    // The first replay ends with an open window, then time moves back to zero.
    let interrupted = expected[1] + window.post * 0.5;
    let first_pass: Vec<_> = normal.points.iter().take_while(|(at, _)| *at <= interrupted).collect();
    for (at, x) in first_pass.iter().copied() {
        replay.point(*at, None, x);
    }
    for (at, x) in &normal.points {
        replay.point(*at, None, x);
    }
    replay.idle();
    drop(replay);
    assert_eq!(deliveries[0], (expected.len() as u64, normal.samples), "retained frame retries do not duplicate sample totals");
    assert_eq!(deliveries[1].0, expected.len() as u64 + 1, "backward time drops unfinished strokes and preserves completed waveforms");
    assert_eq!(deliveries[1].1, normal.samples + first_pass.len() as u64, "backward time retains unpublished local sample counts");

    let mut recovered = SimIntake::by_name(
        NetProbe { pos: "later".into(), neg: None, scale: 1.0 },
        Acquisition::new(trigger, 0, window, vec![LaneSetup::time(map)]),
        Duration::MAX,
        |frame, _| Published::Taken(frame),
    );
    let mut renamed = unknowns;
    let pwm = renamed.iter().position(|unknown| unknown.name == "pwm").expect("solver exposes the pulse net");
    renamed[pwm].name = "later".into();
    renamed[pwm].kind = UnkKind::Current;
    recovered.begin(&renamed);
    assert!(recovered.unresolved(), "a same-name current unknown is not a voltage net");
    renamed[pwm].kind = UnkKind::Voltage;
    recovered.begin(&renamed);
    assert!(!recovered.unresolved(), "a later lowering resolves a previously missing voltage net");
}

#[test]
fn live_scope_auto_displays_dc_and_measures_the_displayed_pulse() {
    let period = 100e-6;
    let rise = period / 1000.0;
    let duty = 0.3;
    let mut circuit = SimCircuit::new();
    circuit.v("supply", "dc", "0", Source::dc(3.3));
    circuit.v("drive", "pwm", "0", Source::wave(Wave::Pulse { v1: 0.0, v2: 3.3, td: period / 4.0, tr: rise, tf: rise, pw: period * duty - rise, per: period }));
    circuit.r("series", "pwm", "out", 1000.0);
    circuit.c("shunt", "out", "0", 1e-8);
    // A 3.3 V rail with 2 mV of ripple: far below a tenth of a division on this screen.
    circuit.v("rail", "rail", "0", Source::wave(Wave::Pulse { v1: 3.3, v2: 3.302, td: period / 4.0, tr: rise, tf: rise, pw: period * duty - rise, per: period }));
    let plan = ValuePlan { nets: vec!["0".into(), "dc".into(), "pwm".into(), "out".into(), "rail".into()], ..ValuePlan::default() };
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, max_step: period / 100.0, trace: false, ..LiveSpec::default() };
    let mut sim = LiveSim::new(circuit, plan, 1, spec).expect("pulse RC and DC source initial conditions converge");
    let config = SimScopeConfig {
        plot: (32, 24),
        volts: (-0.2, 3.5),
        trigger: EdgeTrigger { level: 1.65, hyst: 0.1, slope: Slope::Rising, holdoff: 0.0 },
        qualifier: fab_elec_bench::trigger::Qualifier::None,
        zone: None,
        window: Window { pre: period / 10.0, post: 4.0 * period },
        auto: Some(1e-3),
        frame_every: Duration::from_millis(1),
    };
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut feeds = Vec::new();
    let (commands, mut sinks) = SinkSet::new();
    for (index, (net, auto)) in [("dc", Some(1e-3)), ("dc", None), ("pwm", Some(1e-3)), ("rail", Some(1e-3))].into_iter().enumerate() {
        let feed = SimScopeFeed::show_with_capture_interval(
            &mut cx,
            NetProbe { pos: net.into(), neg: None, scale: 1.0 },
            SimScopeConfig { auto, ..config },
            Duration::ZERO,
        )
        .expect("the simulation scope merger starts");
        assert!(feed.measurements().is_none(), "no snapshot exists before a complete displayed window");
        assert!(commands.try_send(SinkCmd::Add(index as u64, feed.sinks().make())).is_ok(), "each feed installs a simulation acquisition");
        feeds.push(feed);
    }
    sinks.begin(&sim.solver().unknowns);
    sinks.point(0.0, None, &sim.solver().x);
    sim.values(1.0);
    assert!(sim.apply(LiveCmd::Step(3e-3)), "the measurement run keeps the solver alive");
    while sim.running() {
        sim.advance(f64::INFINITY, None, &mut sinks).expect("pulse RC transient converges with scope capture");
        sim.values(1.0);
    }
    sinks.idle();
    // The run is over: every window it completed was captured and every frame handed over on this
    // thread, or held where a full channel refused it, offered again as a waiting run does. The
    // merge workers publish them whenever they are scheduled, so the wait is for that event, with
    // no wall-clock bound.
    let mut waveforms = [0; 4];
    let mut frames = [0; 4];
    loop {
        if sinks.pending() {
            sinks.idle();
        }
        for (index, feed) in feeds.iter_mut().enumerate() {
            if let Some(hits) = feed.take() {
                waveforms[index] += hits.waveforms();
                frames[index] += 1;
                feed.give_back(hits);
            }
        }
        if frames.iter().all(|&count| count > 0) && waveforms[0] > 0 && feeds[0].measurements().is_some() && feeds[2].measurements().is_some() && feeds[3].measurements().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(waveforms[0] > 0, "auto acquisition displays waveforms on the DC source node");
    assert_eq!(waveforms[1], 0, "normal acquisition displays no waveforms on the same DC node");
    assert!(feeds[1].measurements().is_none(), "normal DC has no displayed window to measure");
    let (generation, pulse) = feeds[2].measurements().expect("pulse capture reaches the UI snapshot");
    assert_ne!(generation, 0, "the first measurement snapshot has a nonzero generation");
    let value = |name: &str, unit: &str| {
        let row = pulse.iter().find(|row| row.name == name).expect("a complete pulse window defines this measurement");
        assert_eq!(row.lane, 0, "live measurements belong to the scope's first channel");
        assert_eq!(row.unit, unit, "live measurements use the card's SI or percent units");
        row.value
    };
    assert!((value("freq", "Hz") / 10_000.0 - 1.0).abs() < 0.001, "pulse frequency converges within 0.1 percent of 10 kHz");
    assert!((value("duty", "%") / (100.0 * duty) - 1.0).abs() < 0.01, "pulse duty converges within 1 percent of its high fraction");
    assert!((value("Vpp", "V") / 3.3 - 1.0).abs() < 0.01, "pulse peak-to-peak converges within 1 percent of its swing");
    assert!((value("rise", "s") / (0.8 * rise) - 1.0).abs() < 0.01, "rise time uses seconds and the 10 to 90 percent thresholds");
    assert!(value("overshoot", "%").abs() < 0.01, "the ideal pulse has no measured overshoot");
    let screen_mean = 3.3 * (4.0 * duty * period + 0.125 * rise) / (config.window.pre + config.window.post);
    assert!((value("mean", "V") - screen_mean).abs() < 1e-6, "mean measures the displayed pre/post window, including its interpolated endpoints");
    let (_, dc) = feeds[0].measurements().expect("a forced DC window is measured");
    assert_eq!(dc.len(), 2, "DC omits undefined edge measurements");
    assert!(dc.iter().any(|row| row.name == "mean" && row.unit == "V" && (row.value - 3.3).abs() < 1e-6), "the forced window measures the 3.3 V rail mean");
    assert!(dc.iter().any(|row| row.name == "Vpp" && row.value == 0.0), "the forced DC window has zero peak-to-peak swing");
    let (_, rail) = feeds[3].measurements().expect("the rippled rail is measured");
    let mut names: Vec<&str> = rail.iter().map(|row| row.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["Vpp", "mean"], "ripple below a tenth of a division reads as a DC rail: mean and Vpp only");
    assert!(rail.iter().any(|row| row.name == "Vpp" && (row.value - 0.002).abs() < 2e-4), "the rail's Vpp still reads its 2 mV ripple");
    for feed in &mut feeds {
        feed.stop();
    }
}

#[test]
fn multi_lane_measurements_publish_time_current_and_spectrum_rows() {
    use fab_elec_bench::source::{ChanId, ChannelInfo, Quantity};
    use fab_elec_view::instruments::{acquire::SimChannel, scope::FftSettings};
    let config = SimScopeConfig {
        plot: (32, 24), volts: (-1.5, 1.5),
        trigger: EdgeTrigger { level: 0.0, hyst: 0.1, slope: Slope::Rising, holdoff: 0.0 },
        qualifier: fab_elec_bench::trigger::Qualifier::None,
        zone: None,
        window: Window { pre: 0.001, post: 0.003 }, auto: None, frame_every: Duration::MAX,
    };
    let map = PhosphorMap::screen(32, 24, 0.001, 0.004, -1.5, 1.5);
    let channels = vec![
        SimChannel::voltage(ChanId(0), NetProbe { pos: "v".into(), neg: None, scale: 1.0 }),
        SimChannel { info: ChannelInfo { id: ChanId(1), name: "current".into(), quantity: Quantity::Amp }, probe: NetProbe { pos: "V1#branch".into(), neg: None, scale: 2.0 } },
        SimChannel::voltage(ChanId(2), NetProbe { pos: "v".into(), neg: None, scale: 1.0 }),
    ];
    let lanes = vec![LaneSetup::time(map), LaneSetup::time(map), LaneSetup { map, spectrum: Some((FftSettings::default(), (map.w, map.h))), drawn: true }];
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut feed = SimScopeFeed::show_lanes(&mut cx, channels, config, 0, lanes, None).expect("multi-lane merger starts");
    let mut sink = feed.sinks().make();
    sink.begin(&[
        UnknownInfo { name: "0".into(), kind: UnkKind::Voltage },
        UnknownInfo { name: "v".into(), kind: UnkKind::Voltage },
        UnknownInfo { name: "V1#branch".into(), kind: UnkKind::Current },
    ]);
    for i in 0..=1280 {
        let t = i as f64 / 128_000.0;
        sink.point(t, None, &[0.0, (std::f64::consts::TAU * 1000.0 * t).sin(), -0.25]);
    }
    sink.idle();
    // The workers publish whenever they are scheduled: the wait is for that event, with no
    // wall-clock bound.
    let rows = loop {
        if let Some(frame) = feed.take() { feed.give_back(frame); }
        if let Some((_, rows)) = feed.measurements() {
            if rows.iter().any(|r| r.lane == 0) && rows.iter().any(|r| r.lane == 1) && rows.iter().any(|r| r.lane == 2 && r.name == "peak f") && rows.iter().any(|r| r.lane == 2 && r.name == "mean") { break rows; }
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    assert!(rows.iter().any(|r| r.lane == 0 && r.name == "freq" && (r.value - 1000.0).abs() < 0.01), "time measurements belong to lane 0");
    assert!(rows.iter().any(|r| r.lane == 1 && r.name == "mean" && r.unit == "A" && (r.value + 0.5).abs() < 1e-6), "Amp measurements retain their lane and physical units");
    assert!(rows.iter().any(|r| r.lane == 2 && r.name == "peak f" && (r.value - 1000.0).abs() < 0.01), "FFT measurements belong to lane 2 alongside time rows");
    assert!(rows.iter().any(|r| r.lane == 2 && r.name == "mean"), "a lens keeps its time-domain rows beside the spectrum");
    feed.stop();
}

/// A scope marks each waveform's stretch stepped below the time resolution (`Hits::below`),
/// trigger-relative and clipped to its window, from the steps the solver flags (`Sink::below_res`;
/// which steps those are is the sim's own proof): a window over the flagged stretch carries it from
/// the stretch's start to the window's end, a window before or after it carries nothing.
#[test]
fn live_scope_marks_what_is_below_the_time_resolution() {
    let window = Window { pre: 100e-6, post: 100e-6 };
    let trigger = EdgeTrigger { level: 0.5, hyst: 0.1, slope: Slope::Rising, holdoff: 0.0 };
    let map = PhosphorMap::screen(16, 16, window.pre, window.pre + window.post, -1.5, 1.5);
    let (mut sink, report) = observe("in", trigger, window, map, None);
    sink.begin(&[UnknownInfo { name: "0".into(), kind: UnkKind::Voltage }, UnknownInfo { name: "in".into(), kind: UnkKind::Voltage }]);
    // a 2 kHz sine sampled every 5 us, rising through 0.5 at 41.7 us into each 500 us period; the
    // steps ending within (1.0 ms, 1.3 ms] are flagged below the resolution
    let (dt, freq, flagged) = (5e-6, 2e3, (1.0e-3, 1.3e-3));
    let mut k = 0u32;
    let mut seen = 0;
    let mut newest = |sink: &mut Observed, until: f64| {
        while k as f64 * dt <= until {
            let t = k as f64 * dt;
            sink.point(t, None, &[0.0, (std::f64::consts::TAU * freq * t).sin()]);
            if t > flagged.0 + 0.5 * dt && t < flagged.1 + 0.5 * dt {
                sink.below_res(t);
            }
            k += 1;
        }
        sink.idle();
        let report = report.lock().unwrap();
        let frame = report.frames[seen..].iter().rev().find(|frame| frame.lanes[0].waveforms > 0).expect("a window completed since the last look");
        seen = report.frames.len();
        frame.lanes[0].below
    };
    assert_eq!(newest(&mut sink, 0.7e-3), None, "the window at 542 us, before the flagged stretch, is not marked");
    let t_trigger = 1e-3 + 0.5f64.asin() / (std::f64::consts::TAU * freq);
    let (from, to) = newest(&mut sink, 1.2e-3).expect("the window at 1042 us, over the flagged stretch, is marked");
    assert!((from - (flagged.0 - t_trigger)).abs() < dt && (to - window.post).abs() < dt,
        "the mark runs from the stretch's start to the window's end: {from:e}..{to:e} s from the trigger");
    assert_eq!(newest(&mut sink, 1.7e-3), None, "the window at 1542 us, after the flagged stretch, is not marked");
}

/// Under a time resolution a scope draws a short pulse's edges where Exact draws them (the
/// stepper's edge points): per 150 us column (4 res of 37.5 us) the pad's lowest and highest lit
/// rows are Exact's, also in the column that begins 20 us after a 0.5 us pulse ends, where a line
/// over the 37.5 us step after the edge would still be falling.
#[test]
fn live_scope_columns_under_a_time_resolution_are_exacts() {
    let period = 600e-6;
    let params = GpioParams { r_on_h: 25.0, r_on_l: 25.0, r_pull: 40e3, r_leak: 1e12, t_edge: 5e-9, vih: 0.55, vil: 0.45 };
    let columns = |res: f64| {
        let mut circuit = SimCircuit::new();
        circuit.v("supply", "vdd", "0", Source::dc(3.3));
        circuit.gpio("pa0", "pad", "vdd", "0", params, PinMode::Pwm { period, high: 0.5e-6, origin: 100e-6 });
        circuit.c("pin", "pad", "0", 5e-12);
        let plan = ValuePlan { nets: vec!["0".into(), "pad".into()], ..ValuePlan::default() };
        let spec = LiveSpec { rate: f64::INFINITY, playing: false, max_step: 1e-4, trace: false, res, ..LiveSpec::default() };
        let mut sim = LiveSim::new(circuit, plan, 1, spec).expect("the pulsed pad's operating point converges");
        let trigger = EdgeTrigger { level: 1.65, hyst: 0.1, slope: Slope::Rising, holdoff: 0.0 };
        // four 150 us columns from 280 us before each rising edge: the third begins 20 us after
        // the pulse ends
        let window = Window { pre: 280e-6, post: 320e-6 };
        let map = PhosphorMap::screen(4, 64, window.pre, window.pre + window.post, -0.2, 3.5);
        let (sink, report) = observe("pad", trigger, window, map, None);
        let unknowns = sim.solver().unknowns.clone();
        let (commands, mut sinks) = SinkSet::new();
        assert!(commands.try_send(SinkCmd::Add(1, Box::new(sink))).is_ok(), "the acquisition installs");
        sinks.begin(&unknowns);
        sinks.point(0.0, None, &sim.solver().x);
        sim.values(1.0);
        assert!(sim.apply(LiveCmd::Step(6.0 * period)), "six periods");
        while sim.running() {
            sim.advance(f64::INFINITY, None, &mut sinks).expect("the pulsed pad steps");
            sim.values(1.0);
        }
        sinks.idle();
        let report = report.lock().unwrap();
        let lit = |col: u32, row: u32| report.frames.iter().any(|frame| frame.lanes[0].at(col, row) > 0.0);
        let rows: Vec<_> = (0..map.w).map(|col| {
            let on: Vec<u32> = (0..map.h).filter(|&row| lit(col, row)).collect();
            (on.first().copied(), on.last().copied())
        }).collect();
        (report.waveforms(), rows)
    };
    let (exact, fast) = (columns(0.0), columns(37.5e-6));
    assert!(exact.0 >= 5 && fast.0 == exact.0, "the same windows: {} versus {}", fast.0, exact.0);
    let flat = |(lo, hi): (Option<u32>, Option<u32>)| lo.zip(hi).is_some_and(|(lo, hi)| hi - lo <= 1);
    assert!(flat(exact.1[2]) && !flat(exact.1[1]), "Exact's column after the pulse is a flat line (two rows at most), the pulse's is not: {:?}", exact.1);
    assert_eq!(fast.1, exact.1, "each column's lowest and highest rows are Exact's");
}
