//! Live stepping, command ordering, publications and lossless trace delivery.

use fab_elec_sim::cosim::{Bus, CoRun, CoSim, CoSimDevice, CoSimSlot, CoState, StepTo, Watch};
use fab_elec_sim::input::{InputSource, Inputs, UNPACED_LEAD_PERIODS};
use fab_elec_sim::live::{Latest, LiveCmd, LiveValues, Terminal, ValuePlan};
use fab_elec_sim::livesim::{run_live, LiveSim, LiveSpec, SinkCmd, SinkSet, FAST_RELTOL};
use fab_elec_sim::listen::{AudioTap, Player, SampleRing, TapMix, TapSpec, TapVoice};
use fab_elec_sim::stamp::UnknownInfo;
use fab_elec_sim::trace::{TraceDelta, TraceStore, CHUNK};
use fab_elec_sim::{run, Analysis, Control, HoldKind, Options, Record, SimCircuit, SimDiag, Sink, Source, TranSpec, Wave};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

fn pin(device: &str, terminal: u8) -> Vec<Terminal> {
    vec![Terminal { device: device.into(), terminal }]
}

fn rc(source: Source, resistance: f64, shunt: bool) -> (SimCircuit, ValuePlan) {
    let mut circuit = SimCircuit::new();
    circuit.v("v1", "in", "0", source);
    circuit.r("r1", "in", "out", resistance);
    circuit.c("c1", "out", "0", 1e-6);
    let mut plan = ValuePlan {
        nets: vec!["0".into(), "in".into(), "out".into()],
        pins: vec![pin("r1", 0), pin("r1", 1), pin("c1", 0), pin("c1", 1), pin("v1", 0), pin("v1", 1)],
        parts: vec![vec!["r1".into()], vec!["c1".into()], vec!["v1".into()]],
        pin_net: vec![Some(1), Some(2), Some(2), Some(0), Some(1), Some(0)],
        part_pins: vec![(0, 2), (2, 2), (4, 2)],
    };
    if shunt {
        circuit.r("r2", "out", "0", 1e3);
        plan.pins.extend([pin("r2", 0), pin("r2", 1)]);
        plan.parts.push(vec!["r2".into()]);
        plan.pin_net.extend([Some(2), Some(0)]);
        plan.part_pins.push((6, 2));
    }
    (circuit, plan)
}

fn pwm() -> Source {
    Source::wave(Wave::Pulse {
        v1: 0.0, v2: 1.0, td: 0.0, tr: 1e-7, tf: 1e-7,
        pw: 5e-5 - 1e-7, per: 1e-4,
    })
}

fn resistance(generation: u64, device: &str, value: f64) -> LiveCmd {
    LiveCmd::SetParam { generation, device: device.into(), key: "value".into(), value }
}

// A new simulation or a parameter change requests a publication after the next point;
// acknowledge it, then keep advancing until the requested Step has actually paused.
/// Runs a Step to its end, publishing after each advance: each publication's interval starts at
/// the previous one's time, and it says whether the run goes on.
fn finish_step(sim: &mut LiveSim, sink: &mut dyn Sink) {
    let mut published: Option<f64> = None;
    while sim.running() {
        let before = sim.t();
        sim.advance(f64::INFINITY, None, sink)
            .unwrap_or_else(|diag| panic!("live Step converges: {diag}"));
        assert!(sim.t() > before, "live Step makes simulated-time progress");
        let v = sim.values(1.0);
        if let Some(t) = published {
            assert_eq!(v.since, t, "a publication's interval starts at the previous publication's time");
        }
        assert_eq!(v.running, sim.running(), "a publication says whether the run goes on");
        published = Some(v.t);
    }
}

fn curve(record: &Record, first: usize, start: (f64, f64), response: (f64, f64), property: &str) {
    let (t0, v0) = start;
    let (end, tau) = response;
    let out = record.index("out");
    assert!(record.at.len() > first, "{property}: new points were recorded");
    assert!(record.at[first] > t0, "{property}: time continues after the preceding point");
    let want = |t: f64| end + (v0 - end) * (-(t - t0) / tau).exp();
    let physical_change = (want(record.at[first]) - v0).abs();
    assert!(
        (record.x[first][out] - v0).abs() < physical_change + 2e-4,
        "{property}: voltage is continuous across the first step"
    );
    for (&t, x) in record.at[first..].iter().zip(&record.x[first..]) {
        assert!(
            (x[out] - want(t)).abs() < 2e-4,
            "{property}: closed-form voltage at {t:e}: {} vs {}", x[out], want(t)
        );
    }
}

#[test]
fn live_set_param_mid_run_and_replace() {
    let (mut circuit, plan) = rc(Source::dc(1.0), 1e3, false);
    // LiveSim solves an operating point with node initial conditions held.
    circuit.set_ic("out", 0.0);
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, reltol: Some(1e-4), max_step: 2e-5, ..LiveSpec::default() };
    let mut sim = LiveSim::new(circuit, plan, 1, spec).expect("RC live initial conditions converge");
    let mut record = Record::default();
    record.begin(&sim.solver().unknowns);

    assert!(sim.apply(LiveCmd::Step(2e-3)), "initial Step keeps the worker alive");
    finish_step(&mut sim, &mut record);
    assert!(!sim.running() && sim.t() >= 2e-3, "initial Step reaches its target and pauses");
    curve(&record, 0, (0.0, 0.0), (1.0, 1e-3), "1 kohm RC charging");

    let (t1, v1, first) = (sim.t(), record.last("out"), record.at.len());
    let generation = sim.generation();
    assert!(sim.apply(resistance(generation, "r1", 500.0)), "paused resistor change keeps the worker alive");
    assert_eq!(sim.t(), t1, "resistor change does not restart time");
    assert!(!sim.running(), "resistor change preserves the paused state");
    let before = sim.values(1.0);
    assert_eq!(before.net_v[2], v1 as f32, "paused resistor change preserves capacitor voltage");
    assert!(sim.apply(resistance(generation + 1, "r1", 1.0)), "wrong-generation command keeps the worker alive");
    assert!(sim.apply(resistance(generation, "missing", 1.0)), "unknown-device command keeps the worker alive");
    let after = sim.values(1.0);
    assert_eq!(sim.stats().stale, 1, "wrong-generation parameter is counted as stale");
    assert_eq!(sim.stats().unknown, 1, "missing device is counted as unknown");
    assert_eq!(after.t, before.t, "rejected commands preserve time");
    assert_eq!(after.net_v, before.net_v, "rejected commands preserve net voltages");
    assert_eq!(after.branch_i, before.branch_i, "rejected commands preserve terminal currents");
    assert!(sim.apply(LiveCmd::Step(2e-3)), "changed-resistor Step keeps the worker alive");
    finish_step(&mut sim, &mut record);
    assert!(!sim.running() && sim.t() >= t1 + 2e-3, "changed-resistor Step reaches its target and pauses");
    curve(&record, first, (t1, v1), (1.0, 0.5e-3), "500 ohm RC after paused SetParam");

    let (t1, v1, first) = (sim.t(), record.last("out"), record.at.len());
    let before = sim.values(1.0);
    assert!(sim.apply(LiveCmd::Step(1e-3)), "ordered Step keeps the worker alive");
    assert!(sim.apply(resistance(generation, "r1", 2e3)), "SetParam queues behind Step");
    assert_eq!(sim.values(1.0).branch_i, before.branch_i, "held SetParam has no immediate electrical effect");
    finish_step(&mut sim, &mut record);
    assert!(!sim.running() && sim.t() >= t1 + 1e-3, "ordered Step pauses before releasing SetParam");
    curve(&record, first, (t1, v1), (1.0, 0.5e-3), "held SetParam leaves the entire Step at 500 ohm");
    let changed = sim.values(1.0);
    let expected_i = (1.0 - record.last("out")) / 2e3;
    assert!(
        (f64::from(changed.branch_i[0]) - expected_i).abs() <= 1e-12 + 1e-6 * expected_i.abs(),
        "held SetParam takes effect at the pause: resistor current is the 2 kohm current"
    );

    let (t1, v1, first) = (sim.t(), record.last("out"), record.at.len());
    assert!(sim.apply(LiveCmd::Step(1e-3)), "post-command Step keeps the worker alive");
    finish_step(&mut sim, &mut record);
    assert!(!sim.running() && sim.t() >= t1 + 1e-3, "post-command Step reaches its target and pauses");
    curve(&record, first, (t1, v1), (1.0, 2e-3), "2 kohm RC after held SetParam");

    let (t1, v1, first) = (sim.t(), record.last("out"), record.at.len());
    let (circuit, plan) = rc(Source::dc(1.0), 2e3, true);
    assert!(sim.apply(LiveCmd::Replace { generation: 2, circuit: Box::new(circuit), plan: Box::new(plan) }), "Replace keeps the worker alive");
    assert_eq!(sim.generation(), 2, "Replace installs the new generation");
    assert_eq!(sim.t(), t1, "Replace preserves simulated time");
    assert!(!sim.running(), "Replace preserves the paused state");
    let replaced = sim.values(1.0);
    assert_eq!(replaced.generation, 2, "the next values publication belongs to generation 2");
    assert_eq!(replaced.since, t1, "a replacement's first interval starts at the replacement");
    assert_eq!(replaced.net_v[2], v1 as f32, "Replace carries the capacitor voltage continuously");
    record.begin(&sim.solver().unknowns);
    assert!(sim.apply(LiveCmd::Step(2e-3)), "replacement Step keeps the worker alive");
    finish_step(&mut sim, &mut record);
    assert!(!sim.running() && sim.t() >= t1 + 2e-3, "replacement Step reaches its target and pauses");
    let thevenin_v = 1e3 / (2e3 + 1e3);
    let thevenin_r = 2e3 * 1e3 / (2e3 + 1e3);
    curve(&record, first, (t1, v1), (thevenin_v, thevenin_r * 1e-6), "replacement RC Thevenin response");
    let delta = sim.take_delta().expect("replacement records a new trace delta");
    assert_eq!(delta.generation, 2, "replacement trace delta has generation 2");
    assert_eq!(delta.from, 0, "replacement trace starts at sample zero");
    assert!(delta.time.iter().all(|&t| t > t1), "replacement delta contains only new-topology points");
}

#[test]
fn live_retune_takes_the_next_period() {
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, reltol: Some(1e-4), max_step: 1e-6, ..LiveSpec::default() };
    let (circuit, plan) = rc(pwm(), 1e3, false);
    let mut sim = LiveSim::new(circuit, plan, 1, spec.clone()).expect("retuned PWM RC initial conditions converge");
    let mut record = Record::default();
    record.begin(&sim.solver().unknowns);
    let value_at = |record: &Record, t: f64| {
        let i = record.at.partition_point(|&at| at < t);
        assert!(i < record.at.len(), "the source record reaches the requested sample at {t:e}");
        record.x[i][record.index("in")]
    };

    assert!(sim.apply(LiveCmd::Step(2.2e-4)), "initial PWM Step keeps the worker alive");
    finish_step(&mut sim, &mut record);
    let before = sim.t();
    assert!(!sim.running() && before >= 2.2e-4 && before < 2.3e-4, "initial Step pauses inside period 2's old high phase");
    let generation = sim.generation();
    for width in [2e-5 - 1e-7, 7e-5 - 1e-7] {
        let mut source = pwm();
        if let Wave::Pulse { pw, .. } = &mut source.wave {
            *pw = width;
        }
        assert!(sim.apply(LiveCmd::Retune { generation, device: "v1".into(), source }), "PWM retune keeps the worker alive");
        assert_eq!(sim.t(), before, "a pending PWM retune preserves simulated time");
        assert!(!sim.running(), "a pending PWM retune preserves the paused state");
    }
    assert!(sim.apply(LiveCmd::Step(6e-4 - sim.t())), "retuned PWM Step keeps the worker alive");
    finish_step(&mut sim, &mut record);
    assert_eq!(sim.stats().replaced, 0, "source retunes do not replace the circuit");
    assert_eq!(sim.stats().retuned, 2, "both PWM retunes are counted");
    assert_eq!(sim.generation(), generation, "source retunes preserve the lowering generation");
    assert!(!sim.running() && sim.t() >= 6e-4 && sim.t() > before, "retuned simulation keeps advancing and pauses at the target");
    assert!(record.at.windows(2).all(|t| t[0] < t[1]), "recorded source times keep advancing across retunes");
    assert_eq!(sim.store().len(), record.at.len(), "retunes retain the trace and deliver every point to the recording sink");

    assert!(value_at(&record, 2e-4 + 4.5e-5) > 0.5, "period 2 keeps its old high phase before the old falling edge");
    assert!(value_at(&record, 2e-4 + 5.5e-5) < 0.5, "period 2 keeps its old falling edge despite the pending longer duty");
    for period in [3, 4] {
        let start = f64::from(period) * 1e-4;
        assert!(value_at(&record, start + 6.5e-5) > 0.5, "period {period} uses the second duty, never the replaced first duty");
        assert!(value_at(&record, start + 7.5e-5) < 0.5, "period {period} falls at the second retune's requested duty");
    }
    assert!(value_at(&record, 3e-4) < 0.5, "period 3 starts at the old waveform's left value");
    assert!(value_at(&record, 3e-4 + 2e-7) > 0.5, "period 3 rises just after 3e-4 with its origin at the switch");

    let (circuit, plan) = rc(Source::dc(1.0), 1e3, false);
    let mut dc = LiveSim::new(circuit, plan, 1, spec).expect("retuned DC RC initial conditions converge");
    let mut dc_record = Record::default();
    dc_record.begin(&dc.solver().unknowns);
    assert!(dc.apply(LiveCmd::Step(1e-4)), "initial DC Step keeps the worker alive");
    finish_step(&mut dc, &mut dc_record);
    let before = dc.t();
    let first = dc_record.at.len();
    assert!((value_at(&dc_record, before) - 1.0).abs() < 1e-9, "DC source starts at one volt before retuning");
    assert!(
        dc.apply(LiveCmd::Retune { generation: dc.generation(), device: "v1".into(), source: Source::dc(2.0) }),
        "DC retune keeps the worker alive"
    );
    assert_eq!(dc.t(), before, "an immediate DC retune preserves simulated time");
    assert!(dc.apply(LiveCmd::Step(1e-5)), "retuned DC Step keeps the worker alive");
    finish_step(&mut dc, &mut dc_record);
    assert!(dc_record.at.len() > first && dc_record.at[first] > before, "DC retune records a first point after the command");
    assert!(
        (value_at(&dc_record, dc_record.at[first]) - 2.0).abs() < 1e-9,
        "DC retune takes effect at the first recorded point after the command"
    );
    assert_eq!(dc.stats().retuned, 1, "the immediate DC retune is counted");
    assert_eq!(dc.stats().replaced, 0, "the immediate DC retune does not replace the circuit");
}

/// Waits for a worker's progress with no deadline: a hang shows as a stalled test, not as a
/// failure that depends on how busy the box is.
fn wait_for(mut ready: impl FnMut() -> bool) {
    while !ready() {
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn live_loop_never_waits_on_a_stalled_consumer() {
    let (circuit, plan) = rc(pwm(), 1e3, false);
    // Publish and attempt a trace on every iteration. A finite Step bounds the work,
    // while the one-slot trace channel must fill independently of processor speed.
    let spec = LiveSpec {
        rate: f64::INFINITY, playing: true, max_step: 2e-5,
        values_every: Duration::ZERO, trace_every: Duration::ZERO, slice: Duration::ZERO,
        ..LiveSpec::default()
    };
    let mut sim = LiveSim::new(circuit, plan, 1, spec).expect("PWM RC live initial conditions converge");
    let latest = Arc::new(Latest::<LiveValues>::new());
    latest.publish(Arc::new(sim.values(1.0)));
    let start = latest.latest().expect("stall starts with published live values").1;
    let (commands, command_rx) = mpsc::sync_channel(4);
    let (mut traces, trace_rx) = mpsc::sync_channel(1);
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    let stall_target = 5e-4;
    commands.try_send(LiveCmd::Step(stall_target)).expect("stall Step is queued before the worker starts");
    let published = Arc::clone(&latest);
    let worker = thread::spawn(move || {
        let mut sink = Record::default();
        let result = run_live(sim, &command_rx, &published, &mut traces, &mut sink, &|| {});
        let _ = done_tx.send(result);
    });

    thread::sleep(Duration::from_millis(100));
    // A busy build box may not have scheduled the worker yet. Keep the consumer
    // stalled until the requested simulated-time progress: the worker progresses while the
    // trace consumer is stalled.
    wait_for(|| latest.latest().is_some_and(|(_, v)| v.t >= stall_target));
    let end = latest.latest().expect("stall ends with published live values").1;
    assert!(end.t > start.t, "published simulated time rises without consuming any trace delta");

    let mut consumer = TraceStore::default();
    commands.try_send(LiveCmd::Step(2.37e-4)).expect("consumer recovery Step is queued");
    let drain_until = Instant::now() + Duration::from_millis(50);
    while let Some(remaining) = drain_until.checked_duration_since(Instant::now()) {
        match trace_rx.recv_timeout(remaining) {
            Ok(delta) => consumer.apply(&delta),
            Err(mpsc::RecvTimeoutError::Timeout) => break,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    commands.try_send(LiveCmd::Stop).expect("Stop reaches the live worker");
    let result = done_rx.recv().expect("live worker returns after Stop");
    worker.join().expect("live worker joins without a panic");
    let mut sim = result.expect("PWM RC worker completes without a solver diagnostic");
    // Deltas already queued precede the unsent tail returned by the stopped sim.
    for delta in trace_rx.try_iter() {
        consumer.apply(&delta);
    }
    if let Some(delta) = sim.take_delta() {
        consumer.apply(&delta);
    }
    assert!(sim.stats().full > 0, "stalled consumer fills the bounded trace channel");
    assert!(!consumer.is_empty(), "consumer receives the live trace");
    assert_eq!(consumer.base, 0, "consumer retains the first sample through backpressure");
    assert_eq!(consumer.len(), sim.store().len(), "backpressure loses no trace points");
    assert_eq!(consumer.time, sim.store().time, "consumer time axis equals the producer time axis");
    assert_eq!(consumer.signals, sim.store().signals, "consumer values equal all producer values, including closed chunks");

    let last = consumer.time.at(consumer.len() - 1);
    assert!(consumer.marks.windows(2).all(|m| m[0] < m[1]), "PWM event marks remain strictly ordered");
    for k in 0..=(last / 1e-4).floor() as usize {
        for offset in [0.0, 1e-7, 5e-5, 5e-5 + 1e-7] {
            let corner = k as f64 * 1e-4 + offset;
            // t = 0 is the initial condition, not an edge crossed by a live step.
            if corner <= start.t || corner >= last {
                continue;
            }
            let i = consumer.marks.partition_point(|&t| t < corner - 1e-12);
            assert!(
                consumer.marks.get(i).is_some_and(|&t| (t - corner).abs() <= 1e-12),
                "PWM period {k} corner {offset:e} survives the stalled consumer at {corner:e}"
            );
        }
    }
}

/// The test stream's sample period, and how far past the analog the test reader reads.
const STREAM_PERIOD: f64 = 1e-5;
const READ_SPAN: f64 = 5e-5;

/// A stream the test feeds by hand: its horizon in seconds, and whether it closed.
struct Stream {
    horizon: AtomicU64,
    closed: AtomicBool,
}

impl Stream {
    fn new(horizon: f64) -> Arc<Stream> {
        Arc::new(Stream { horizon: AtomicU64::new(horizon.to_bits()), closed: AtomicBool::new(false) })
    }
    fn reach(&self, horizon: f64) {
        self.horizon.store(horizon.to_bits(), Ordering::Release);
    }
}

impl InputSource for Stream {
    fn horizon(&self) -> Option<f64> {
        (!self.closed.load(Ordering::Acquire)).then(|| f64::from_bits(self.horizon.load(Ordering::Acquire)))
    }
    fn period(&self) -> f64 {
        STREAM_PERIOD
    }
    fn name(&self) -> String {
        "the test stream".into()
    }
}

/// Runs every 30 µs and reads the stream `READ_SPAN` past the analog, as a device's private run
/// reads at its farthest: logs each run's time, and counts the reads past the horizon.
struct Reader {
    stream: Arc<Stream>,
    k: u64,
    runs: Arc<Mutex<Vec<f64>>>,
    short: Arc<AtomicUsize>,
}

impl CoSim for Reader {
    fn next(&self) -> f64 {
        self.k as f64 * 3e-5
    }
    fn input_span(&self) -> Option<f64> {
        Some(READ_SPAN)
    }
    fn run(&mut self, bus: &mut dyn Bus) -> Result<CoRun, SimDiag> {
        if self.stream.horizon().is_some_and(|horizon| bus.t() + READ_SPAN > horizon) {
            self.short.fetch_add(1, Ordering::Relaxed);
        }
        self.runs.lock().unwrap().push(bus.t());
        self.k += 1;
        Ok(CoRun::Go)
    }
    fn watches(&self) -> &[Watch] {
        &[]
    }
    fn watch_gen(&self) -> u32 {
        0
    }
    fn arm(&mut self, _t: f64, _to: Option<StepTo>) -> bool {
        false
    }
    fn state(&mut self, _t: f64) -> Option<CoState> {
        None
    }
}

/// Keeps every point's time.
struct Times(Arc<Mutex<Vec<f64>>>);

impl Sink for Times {
    fn point(&mut self, at: f64, _outer: Option<f64>, _x: &[f64]) {
        self.0.lock().unwrap().push(at);
    }
}

/// A live PWM RC run on its worker with a `Reader` on a stream it holds for.
struct Streamed {
    commands: mpsc::SyncSender<LiveCmd>,
    latest: Arc<Latest<LiveValues>>,
    want: Arc<AtomicU64>,
    points: Arc<Mutex<Vec<f64>>>,
    runs: Arc<Mutex<Vec<f64>>>,
    short: Arc<AtomicUsize>,
    worker: thread::JoinHandle<Result<(), SimDiag>>,
    _traces: mpsc::Receiver<TraceDelta>,
    _sinks: mpsc::SyncSender<SinkCmd>,
}

impl Streamed {
    /// The run's step cap, and the run started: playing, or a `Step` of `step` seconds.
    fn start(spec: LiveSpec, stream: &Arc<Stream>, step: Option<f64>) -> (f64, Streamed) {
        let (circuit, plan) = rc(pwm(), 1e3, false);
        let sim = LiveSim::new(circuit, plan, 1, spec).expect("PWM RC live initial conditions converge");
        let cap = sim.step_cap();
        let (points, runs, short) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(Vec::new())), Arc::new(AtomicUsize::new(0)));
        let (sink_tx, mut sinks) = SinkSet::new();
        sink_tx.try_send(SinkCmd::Add(1, Box::new(Times(points.clone())))).expect("the point sink is queued");
        let want = Arc::new(AtomicU64::new(0));
        let (commands, command_rx) = mpsc::sync_channel(4);
        let source: Arc<dyn InputSource> = stream.clone();
        commands.send(LiveCmd::Inputs(Inputs { sources: vec![source], want: want.clone() })).expect("the inputs are queued");
        let reader = Reader { stream: stream.clone(), k: 0, runs: runs.clone(), short: short.clone() };
        commands.send(LiveCmd::CoSim(CoSimSlot::new(Some(CoSimDevice { dev: Box::new(reader), pins: Vec::new() })))).expect("the reader is queued");
        if let Some(step) = step {
            commands.send(LiveCmd::Step(step)).expect("the step is queued");
        }
        let latest = Arc::new(Latest::<LiveValues>::new());
        let (mut traces, trace_rx) = mpsc::sync_channel(64);
        let published = Arc::clone(&latest);
        let worker = thread::spawn(move || run_live(sim, &command_rx, &published, &mut traces, &mut sinks, &|| {}).map(|_| ()));
        (cap, Streamed { commands, latest, want, points, runs, short, worker, _traces: trace_rx, _sinks: sink_tx })
    }

    fn values(&self) -> Arc<LiveValues> {
        self.latest.latest().expect("the run has published").1
    }

    fn want(&self) -> f64 {
        f64::from_bits(self.want.load(Ordering::Acquire))
    }

    /// Stops the run: its points' times, the reader's run times and its short reads.
    fn stop(self) -> (Vec<f64>, Vec<f64>, usize) {
        self.commands.send(LiveCmd::Stop).expect("Stop reaches the live worker");
        let result = self.worker.join().expect("the live worker joins without a panic");
        assert!(result.is_ok(), "a streamed run ends without a diagnostic");
        let (points, runs) = (self.points.lock().unwrap().clone(), self.runs.lock().unwrap().clone());
        (points, runs, self.short.load(Ordering::Relaxed))
    }
}

#[test]
fn a_streamed_input_holds_the_run_short_of_what_it_reads() {
    let normal = LiveSpec::default();
    // one step per advance and a publication every loop, then the normal slice and publication
    // period, where an advance would run far past the horizon unless held inside it
    for (slice, values_every) in [(Duration::ZERO, Duration::ZERO), (normal.slice, normal.values_every)] {
        let spec = LiveSpec { rate: f64::INFINITY, playing: true, max_step: 2e-5, values_every, slice, ..LiveSpec::default() };
        let stream = Stream::new(3e-4);
        let (cap, run) = Streamed::start(spec, &stream, None);
        // the run steps while its reach (the step's end and the reader's span) and one source
        // period stay within the horizon
        let last = |horizon: f64| horizon - READ_SPAN - STREAM_PERIOD;
        let held_near = |horizon: f64| run.latest.latest().is_some_and(|(_, v)| v.held == Some(HoldKind::Input) && v.t > last(horizon) - cap);

        wait_for(|| held_near(3e-4));
        let held = run.values();
        assert!(held.t <= last(3e-4) + 1e-12, "slice {slice:?}: the run holds before its reach passes the horizon, at {}", held.t);
        assert!(held.running, "a held run is still running: the readout names what holds it");
        let wanted = run.want();
        assert!(wanted > 3e-4 && wanted <= held.t + cap + READ_SPAN + (1.0 + UNPACED_LEAD_PERIODS) * STREAM_PERIOD + 1e-12,
            "slice {slice:?}: want asks past the horizon by the reach, a period and the lead: {wanted} at {}", held.t);
        let runs = run.runs.lock().unwrap().len();
        thread::sleep(Duration::from_millis(20));
        assert_eq!(run.values().t, held.t, "a held run does not step on");
        assert_eq!(run.runs.lock().unwrap().len(), runs, "a held run does not run its process again");

        stream.reach(6e-4);
        wait_for(|| held_near(6e-4));
        assert!(run.values().t <= last(6e-4) + 1e-12, "slice {slice:?}: the run steps on to the new horizon and holds short of it");

        stream.closed.store(true, Ordering::Release);
        wait_for(|| run.latest.latest().is_some_and(|(_, v)| v.t > 1e-3 && v.held.is_none()));
        let (_, _, short) = run.stop();
        assert_eq!(short, 0, "slice {slice:?}: no read passed the horizon");
    }
}

#[test]
fn a_held_run_steps_as_one_never_held() {
    let until = 2e-3;
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, max_step: 2e-5, ..LiveSpec::default() };
    let paused_at_end = |run: &Streamed| run.latest.latest().is_some_and(|(_, v)| !v.running && v.t >= until);

    let covered = Stream::new(1.0);
    let (_, reference) = Streamed::start(spec.clone(), &covered, Some(until));
    wait_for(|| paused_at_end(&reference));
    let (points, runs, short) = reference.stop();
    assert_eq!(short, 0, "a covered stream is never read past");

    // the source covers 100 µs more each millisecond of wall time, up to what the run asks: the
    // run holds again and again, at loop tops wherever the wall clock left it
    let fed = Stream::new(0.0);
    let (_, run) = Streamed::start(spec, &fed, Some(until));
    let mut held = false;
    while !paused_at_end(&run) {
        held |= run.latest.latest().is_some_and(|(_, v)| v.held == Some(HoldKind::Input));
        let horizon = fed.horizon().expect("the fed stream is open");
        fed.reach(horizon.max((horizon + 1e-4).min(run.want())));
        thread::sleep(Duration::from_millis(1));
    }
    let (held_points, held_runs, held_short) = run.stop();
    assert!(held, "the fed run held for its stream");
    assert_eq!(held_short, 0, "a held run never reads past its stream");
    assert_eq!(held_runs, runs, "the process runs at the same instants, held or not");
    assert_eq!(held_points, points, "the points are the same, held or not");
}

#[test]
fn interval_means_hold_a_pwm_steady() {
    let r = 1e3;
    let mut circuit = SimCircuit::new();
    circuit.v("v1", "pwm", "0", pwm());
    circuit.r("r1", "pwm", "0", r);
    let plan = ValuePlan {
        nets: vec!["0".into(), "pwm".into()],
        pins: vec![pin("r1", 0), pin("r1", 1), pin("v1", 0), pin("v1", 1)],
        parts: vec![vec!["r1".into()], vec!["v1".into()]],
        pin_net: vec![Some(1), Some(0), Some(1), Some(0)],
        part_pins: vec![(0, 2), (2, 2)],
    };
    let spec = LiveSpec { rate: f64::INFINITY, max_step: 2e-5, ..LiveSpec::default() };
    let mut sim = LiveSim::new(circuit, plan, 1, spec).expect("resistive PWM live initial conditions converge");
    assert!(sim.apply(LiveCmd::Subscribe { generation: 1, pins: vec![0], parts: vec![0] }), "PWM subscription keeps the worker alive");
    let first = sim.values(1.0);
    let mut previous = first.t;
    let mut frames = first.frames;
    let mut record = Record::default();
    for k in 1..=20 {
        let target = k as f64 * 26.37e-4;
        sim.advance(target, None, &mut record).expect("PWM averaging interval converges");
        let mut values = sim.values(1.0);
        frames.append(&mut values.frames);
        assert!(values.t >= target, "PWM interval {k} reaches its simulated-time target");
        assert_eq!(values.since, previous, "PWM interval {k} begins at the preceding publication");
        assert!((values.net_mean[1] - 0.5).abs() <= 0.02 * 0.5, "PWM interval {k} mean voltage stays within 2% of 0.5: {}", values.net_mean[1]);
        let v = values.net_v[1];
        assert!(v.abs().min((v - 1.0).abs()) <= 1e-3, "PWM interval {k} instantaneous voltage is a rail: {v}");
        assert!(values.net_min[1].abs() <= 1e-3, "PWM interval {k} minimum reaches zero");
        assert!((values.net_max[1] - 1.0).abs() <= 1e-3, "PWM interval {k} maximum reaches one");
        let mean_i = 0.5 / r;
        let mean_p = 0.5 * 1.0_f64.powi(2) / r;
        assert!((f64::from(values.pin_mean[0]) - mean_i).abs() <= 0.02 * mean_i, "PWM interval {k} subscribed pin mean is the mean resistor current");
        assert!((f64::from(values.part_mean[0]) - mean_p).abs() <= 0.02 * mean_p, "PWM interval {k} subscribed part mean is the mean resistor power");
        assert_eq!(values.pin_mean[1], 0.0, "PWM interval {k} unsubscribed resistor pin has no mean");
        previous = values.t;
    }
    // the grid's frames, from the same accumulator: on their places in simulated time whatever the
    // publications' (each a 2.637 ms step), tiling the run, each holding whole PWM periods
    let grid = LiveSpec::default().grid;
    let end = previous;
    assert_eq!(frames.len(), (end / grid).floor() as usize, "a frame for every whole grid step of the run ({end} s)");
    for (k, f) in frames.iter().enumerate() {
        assert!(f.k == k as i64 && f.since == if k == 0 { 0.0 } else { frames[k - 1].t } && (f.t - (k + 1) as f64 * grid).abs() < 1e-12,
            "frame {k} covers its grid step: {} [{}, {}]", f.k, f.since, f.t);
        assert!((f.net_mean[1] - 0.5).abs() <= 0.01 * 0.5 && f.net_min[1].abs() <= 1e-3 && (f.net_max[1] - 1.0).abs() <= 1e-3,
            "frame {k} holds the PWM's mean and its rails: {} in [{}, {}]", f.net_mean[1], f.net_min[1], f.net_max[1]);
        assert!((f64::from(f.pin_mean[0]) - 0.5 / r).abs() <= 0.01 * 0.5 / r, "frame {k} has the resistor's mean current: {}", f.pin_mean[0]);
    }
}

/// A sink that holds each point it gets for a while, as a busy scope's acquisition does.
struct Holding(Duration);

impl Sink for Holding {
    fn point(&mut self, _at: f64, _outer: Option<f64>, _x: &[f64]) {
        let start = Instant::now();
        while start.elapsed() < self.0 {
            std::hint::spin_loop();
        }
    }
}

/// The sinks' wall time is measured apart from the solver's: every point a sink holds counts in
/// the session's `sink_wall` and the interval's `sink_share`, which takes most of the stepping time
/// when the sink holds each point 200 us and much less when it returns at once; with no device
/// attached the device's share stays 0.
#[test]
fn the_sinks_time_is_apart_from_the_solvers() {
    let run = |hold: Duration| {
        let (circuit, plan) = rc(pwm(), 1e3, false);
        let spec = LiveSpec { rate: f64::INFINITY, max_step: 2e-5, trace: false, ..LiveSpec::default() };
        let mut sim = LiveSim::new(circuit, plan, 1, spec).expect("the RC's initial conditions converge");
        // the first publication (a new run asks for one at once), then a millisecond in one go
        sim.values(1.0);
        sim.advance(1e-3, None, &mut Holding(hold)).expect("the RC steps");
        (sim.values(1.0), sim.stats().clone())
    };
    let hold = Duration::from_micros(200);
    let (held, stats) = run(hold);
    let (free, _) = run(Duration::ZERO);
    let points = u32::try_from(stats.points).expect("a millisecond of points");
    assert!(points > 50 && stats.sink_wall >= hold * points, "each of {points} points held the sink {hold:?}: {:?} in all", stats.sink_wall);
    assert!(stats.sink_wall <= stats.step_wall, "the sinks' time is part of the stepping time: {:?} of {:?}", stats.sink_wall, stats.step_wall);
    assert!(held.sink_share > 0.5 && held.sink_share <= 1.0, "a sink that holds each point takes most of the interval: {}", held.sink_share);
    assert!(free.sink_share < held.sink_share, "a sink that returns at once takes less: {} against {}", free.sink_share, held.sink_share);
    assert_eq!((held.co_share, free.co_share), (0.0, 0.0), "no device, no device share");
}

struct CountingSink(Arc<AtomicUsize>);

impl Sink for CountingSink {
    fn point(&mut self, _at: f64, _outer: Option<f64>, _x: &[f64]) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn sink_set_installs_and_removes() {
    let (mut circuit, plan) = rc(Source::dc(1.0), 1e3, false);
    circuit.set_ic("out", 0.0);
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, max_step: 2e-5, ..LiveSpec::default() };
    let mut sim = LiveSim::new(circuit, plan, 1, spec).expect("sink-set RC live initial conditions converge");
    let (commands, mut sinks) = SinkSet::new();
    sinks.begin(&sim.solver().unknowns);
    let count = Arc::new(AtomicUsize::new(0));

    assert!(sim.apply(LiveCmd::Step(1e-4)), "Step before sink installation keeps the worker alive");
    finish_step(&mut sim, &mut sinks);
    assert!(sim.stats().points > 0, "simulation records points before sink installation");
    assert_eq!(count.load(Ordering::Relaxed), 0, "uninstalled sink receives no points");
    assert!(sinks.is_empty(), "sink set starts empty");

    assert!(commands.try_send(SinkCmd::Add(1, Box::new(CountingSink(Arc::clone(&count))))).is_ok(), "sink Add fits the command channel");
    let before = sim.stats().points;
    assert!(sim.apply(LiveCmd::Step(1e-4)), "Step with sink installation keeps the worker alive");
    finish_step(&mut sim, &mut sinks);
    let installed = count.load(Ordering::Relaxed);
    assert!(installed > 0, "installed sink receives new points");
    assert_eq!(installed as u64, sim.stats().points - before, "installed sink receives every point and no earlier history");
    assert_eq!(sinks.len(), 1, "Add installs exactly one sink");

    assert!(commands.try_send(SinkCmd::Remove(1)).is_ok(), "sink Remove fits the command channel");
    let before = sim.stats().points;
    assert!(sim.apply(LiveCmd::Step(1e-4)), "Step with sink removal keeps the worker alive");
    finish_step(&mut sim, &mut sinks);
    assert!(sim.stats().points > before, "simulation continues after sink removal");
    assert_eq!(count.load(Ordering::Relaxed), installed, "removed sink receives no subsequent points");
    assert!(sinks.is_empty(), "Remove leaves the sink set empty");
}

#[test]
fn live_trace_carries_pin_currents() {
    for trace_pins in [true, false] {
        let (mut circuit, plan) = rc(Source::dc(1.0), 1e3, false);
        circuit.set_ic("out", 0.0);
        let (nets, pins) = (plan.nets.len(), plan.pins.len());
        let spec = LiveSpec { rate: f64::INFINITY, playing: false, trace_pins, ..LiveSpec::default() };
        let mut sim = LiveSim::new(circuit, plan, 1, spec).expect("traced RC live initial conditions converge");
        let mut record = Record::default();
        record.begin(&sim.solver().unknowns);

        assert!(sim.apply(LiveCmd::Step(1e-3)), "traced RC Step keeps the worker alive");
        finish_step(&mut sim, &mut record);
        assert!(!sim.running() && sim.t() >= 1e-3, "traced RC Step reaches its target and pauses");
        let values = sim.values(1.0);
        let store = sim.store();
        assert_eq!(store.signals.len(), nets + if trace_pins { pins } else { 0 }, "trace signal count follows trace_pins");
        assert_eq!(store.len(), record.at.len(), "trace and Record receive every accepted point");
        assert!(store.len() > 1, "RC trace includes points after the first");
        let last = store.len() - 1;
        assert_eq!(store.time.at(last), values.t, "last trace point and live values share the same instant");
        if !trace_pins {
            continue;
        }

        assert_eq!(values.branch_i.len(), pins, "live values carry every planned pin");
        for (p, &current) in values.branch_i.iter().enumerate() {
            assert_eq!(store.signals[nets + p].at(last), current, "last trace current equals live pin {p} exactly as f32");
        }
        for k in 0..store.len() {
            let r_top = f64::from(store.signals[nets].at(k));
            let r_bottom = f64::from(store.signals[nets + 1].at(k));
            assert!((r_top + r_bottom).abs() <= 1e-9, "resistor pin currents are opposite at point {k}");
            if k > 0 {
                let c_top = f64::from(store.signals[nets + 2].at(k));
                let out = f64::from(store.signals[2].at(k));
                assert!((c_top - (1.0 - out) / 1e3).abs() <= 1e-6, "capacitor current satisfies Kirchhoff at out at point {k}");
            }
        }
    }
}

struct Idle(Arc<AtomicUsize>, Arc<AtomicUsize>);

impl Sink for Idle {
    fn begin(&mut self, _unknowns: &[UnknownInfo]) {
        self.1.fetch_add(1, Ordering::Relaxed);
    }
    fn idle(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

/// Slow motion moves at least once per publication (A8): at 1e-4x no accepted step is longer than
/// `rate * values_every` (1.6 us), so a paced run never jumps a bandwidth step (100 us here) and then
/// waits for the clock, and a `Step` passes its end by less than that. The bound is the commanded
/// rate's, never the clock's: the record is the same however the worker slices its time, and the
/// bound lifts with the rate.
///
/// Its speed reading leaves its waits for the clock out (`Sink::clock_wait`): a paced run with
/// time to spare reads above real time, so a player one stall put on the recording hears it live
/// again, its lead primed. While its listener hears it, its worker is raised through the host's
/// hook (`LiveSim::set_heard_priority`), once, and dropped back when the listener closes. So is a
/// run its player's ring paces (unpaced itself, `LiveSpec::heard`: a Listen tap, as Fab's Record
/// live, whose worker was never raised, 11-22); a listened run nothing paces stays as it is.
#[test]
fn a_paced_run_steps_no_longer_than_its_rate_allows() {
    let spec = LiveSpec { rate: 1e-4, playing: false, ..LiveSpec::default() };
    let cap = spec.rate * spec.values_every.as_secs_f64();
    assert!(cap < spec.max_step / 50.0, "the rate's bound is far below the bandwidth step");
    let run = |slices: Option<f64>| {
        let (circuit, plan) = rc(Source::dc(1.0), 1e3, false);
        let mut sim = LiveSim::new(circuit, plan, 1, spec.clone()).expect("RC live initial conditions converge");
        sim.values(1.0);
        let mut record = Record::default();
        record.begin(&sim.solver().unknowns);
        let step = |sim: &mut LiveSim, record: &mut Record, dt: f64| {
            let end = sim.t() + dt;
            assert!(sim.apply(LiveCmd::Step(dt)), "the Step keeps the worker");
            while sim.running() {
                let target = slices.map_or(f64::INFINITY, |s| sim.t() + s);
                sim.advance(target, slices.map(|_| Duration::ZERO), record).expect("the RC steps");
                sim.values(1.0);
            }
            (end, sim.t())
        };
        let paced = step(&mut sim, &mut record, 200e-6);
        let points = record.at.len();
        assert!(sim.apply(LiveCmd::SetRate(f64::INFINITY)), "the rate change keeps the worker");
        assert_eq!(sim.step_cap(), spec.max_step, "an unbounded rate steps up to the bandwidth step");
        let unbounded = step(&mut sim, &mut record, 2e-3);
        (record, points, paced, unbounded)
    };
    let (record, points, (end, t), _) = run(None);
    let longest = record.at[..points].windows(2).map(|w| w[1] - w[0]).fold(0.0, f64::max);
    assert!(longest <= cap * (1.0 + 1e-9), "at 1e-4x no step is longer than {cap:e} s: {longest:e}");
    assert!(t >= end && t - end <= cap, "a Step passes its end by less than one bounded step: {:e}", t - end);
    let lifted = record.at[points - 1..].windows(2).map(|w| w[1] - w[0]).fold(0.0, f64::max);
    assert!(lifted > 10.0 * cap, "an unbounded rate lifts the bound: the longest step after it is {lifted:e} s");
    let (sliced, sliced_points, ..) = run(Some(0.7e-6));
    assert_eq!(sliced_points, points, "the same paced points however the worker slices its time");
    assert_eq!(sliced.at, record.at, "the same accepted times however the worker slices its time");
    assert_eq!(sliced.x, record.x, "the same solutions however the worker slices its time");

    // A listener's player that one stall put on the recording (it ran dry before any speed
    // reading) hears the run live again once its speed reads above real time: the reading is what
    // the run can do, its waits for the clock left out, so this paced run with time to spare
    // reads it, and the player primes its lead again from the live samples.
    const STALL_AT: f64 = 50e-3;
    struct Stall(bool);
    impl Sink for Stall {
        fn point(&mut self, at: f64, _outer: Option<f64>, _x: &[f64]) {
            if at >= STALL_AT && !self.0 {
                self.0 = true;
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
    const HZ: f64 = 48_000.0;
    let ring = SampleRing::new(1 << 14);
    ring.set_device_rate(HZ as u32);
    ring.set_open(true);
    let voices = vec![TapVoice { net: "out".into(), reference: None, gain: 1.0 }];
    let tap = AudioTap::new(TapSpec { voices, latency: 0.06, dc_block_hz: 0.0, mix: TapMix::new(1) }, ring.clone());
    let (sink_tx, mut sinks) = SinkSet::new();
    sink_tx.try_send(SinkCmd::Add(1, Box::new(tap))).expect("the tap is queued");
    sink_tx.try_send(SinkCmd::Add(2, Box::new(Stall(false)))).expect("the stall is queued");
    let tone = Source::wave(Wave::Sin { vo: 0.0, va: 1.0, freq: 100.0, td: 0.0, theta: 0.0, phase: 0.0 });
    let (circuit, plan) = rc(tone, 1e3, false);
    let mut sim = LiveSim::new(circuit, plan, 1, LiveSpec { rate: 1.0, playing: true, ..LiveSpec::default() }).expect("tone RC live initial conditions converge");
    let raised = Arc::new(Mutex::new(Vec::<bool>::new()));
    let hook = raised.clone();
    sim.set_heard_priority(Some(Box::new(move |heard| hook.lock().expect("the priority calls").push(heard))));
    // the device: its buffers as the wall clock owes them, every 5 ms, at 48 kHz; each callback
    // notes whether the recording played, the lead, the live samples the player took and what the
    // ring held after
    let done = Arc::new(AtomicBool::new(false));
    let heard = Arc::new(Mutex::new(Vec::<(bool, usize, usize, usize)>::new()));
    let device = {
        let (ring, done, heard) = (ring.clone(), done.clone(), heard.clone());
        thread::spawn(move || {
            let mut player = Player::new(None);
            let mut out = vec![0.0f32; 4 * 240];
            let (mut owed, mut at) = (0.0f64, Instant::now());
            while !done.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(5));
                let now = Instant::now();
                owed = (owed + (now - at).as_secs_f64() * HZ).min(out.len() as f64);
                at = now;
                let n = owed as usize;
                owed -= n as f64;
                let before = ring.consumed();
                player.fill(&ring, &mut out[..n], HZ);
                heard.lock().expect("the callbacks").push((ring.plays_recording(), ring.ahead(), ring.consumed() - before, ring.fill()));
            }
        })
    };
    let latest = Arc::new(Latest::<LiveValues>::new());
    let (commands, command_rx) = mpsc::sync_channel(4);
    let (mut traces, _trace_rx) = mpsc::sync_channel(64);
    let published = Arc::clone(&latest);
    let worker = thread::spawn(move || run_live(sim, &command_rx, &published, &mut traces, &mut sinks, &|| {}));
    // until the player, once on the recording, has heard the run live for 0.3 s (another stall of
    // the box's own puts it back, and it comes back again), on the callbacks however fast the box
    // is: a player that never comes back ends 5 s past the stall
    const LIVE_FOR: usize = 60;
    let back = |heard: &[(bool, usize, usize, usize)]| heard.iter().any(|h| h.0) && heard.len() > LIVE_FOR && heard[heard.len() - LIVE_FOR..].iter().all(|h| !h.0);
    wait_for(|| back(&heard.lock().expect("the callbacks")) || latest.latest().is_some_and(|(_, v)| v.t > STALL_AT + 5.0));
    // the device stops first: once the run stops, its ring runs dry
    done.store(true, Ordering::Release);
    device.join().expect("the device joins");
    // the listener closes: the run goes on unheard, 50 ms of it, its worker dropped back
    ring.set_open(false);
    let closed = latest.latest().map_or(0.0, |(_, v)| v.t);
    wait_for(|| latest.latest().is_some_and(|(_, v)| v.t > closed + 0.05));
    commands.try_send(LiveCmd::Stop).expect("Stop reaches the paced worker");
    worker.join().expect("the paced worker joins without a panic").expect("the paced tone RC run ends without a diagnostic");
    let heard = heard.lock().expect("the callbacks");
    let recorded = heard.iter().position(|h| h.0).expect("the stall puts the player on the recording");
    assert!(back(&heard), "the player hears the run live again after the stall: on the recording from callback {recorded} of {}", heard.len());
    // the lead is primed again from the live samples: the player takes the first of them only once
    // the ring holds its lead (what it took and what it left at least the lead, the worker only
    // adding meanwhile), however late the box runs the callbacks
    let last = heard.iter().rposition(|h| h.0).expect("the player was on the recording");
    let first = heard[last + 1..].iter().find(|h| h.2 > 0).expect("the player takes live samples again");
    assert!(first.2 + first.3 >= first.1, "live again, the player primes its lead before it plays: took {}, left {}, the lead {}", first.2, first.3, first.1);
    assert_eq!(*raised.lock().expect("the priority calls"), [true, false], "the heard run raises its worker once, and drops it back when its listener closes");

    // the same listener on a run its player's ring paces (unpaced itself, heard as the host says,
    // `LiveSpec::heard`): raised while heard, dropped back 10 ms after its listener closed (the
    // ring closes before the device stops: a run the ring paces waits on a ring nobody drains)
    let ring = SampleRing::new(1 << 14);
    ring.set_device_rate(HZ as u32);
    ring.set_open(true);
    let voices = vec![TapVoice { net: "out".into(), reference: None, gain: 1.0 }];
    let tap = AudioTap::new(TapSpec { voices, latency: 0.06, dc_block_hz: 0.0, mix: TapMix::new(1) }, ring.clone());
    let (sink_tx, sinks) = SinkSet::new();
    sink_tx.try_send(SinkCmd::Add(1, Box::new(tap))).expect("the tap is queued");
    let tone = || Source::wave(Wave::Sin { vo: 0.0, va: 1.0, freq: 100.0, td: 0.0, theta: 0.0, phase: 0.0 });
    let unpaced = |heard: bool| {
        let (circuit, plan) = rc(tone(), 1e3, false);
        let mut sim = LiveSim::new(circuit, plan, 1, LiveSpec { rate: f64::INFINITY, heard, playing: true, ..LiveSpec::default() }).expect("tone RC live initial conditions converge");
        let raised = Arc::new(Mutex::new(Vec::<bool>::new()));
        let hook = raised.clone();
        sim.set_heard_priority(Some(Box::new(move |heard| hook.lock().expect("the priority calls").push(heard))));
        (sim, raised)
    };
    // runs the worker until `until` says so of the latest values, then stops it; its priority calls
    let run = |sim: LiveSim, raised: Arc<Mutex<Vec<bool>>>, mut sinks: SinkSet, until: &mut dyn FnMut(&Latest<LiveValues>)| {
        let latest = Arc::new(Latest::<LiveValues>::new());
        let (commands, command_rx) = mpsc::sync_channel(4);
        let (mut traces, _trace_rx) = mpsc::sync_channel(64);
        let published = Arc::clone(&latest);
        let worker = thread::spawn(move || run_live(sim, &command_rx, &published, &mut traces, &mut sinks, &|| {}));
        until(&latest);
        commands.try_send(LiveCmd::Stop).expect("Stop reaches the worker");
        worker.join().expect("the worker joins without a panic").expect("the tone RC run ends without a diagnostic");
        let calls = raised.lock().expect("the priority calls").clone();
        calls
    };
    let done = Arc::new(AtomicBool::new(false));
    let device = {
        let (ring, done) = (ring.clone(), done.clone());
        thread::spawn(move || {
            let mut player = Player::new(None);
            let mut out = vec![0.0f32; 240];
            while !done.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(5));
                player.fill(&ring, &mut out, HZ);
            }
        })
    };
    let (sim, raised) = unpaced(true);
    let ring_paced = run(sim, raised, sinks, &mut |latest| {
        wait_for(|| latest.latest().is_some_and(|(_, v)| v.t > 0.05));
        ring.set_open(false);
        let closed = latest.latest().map_or(0.0, |(_, v)| v.t);
        wait_for(|| latest.latest().is_some_and(|(_, v)| v.t > closed + 0.01));
    });
    done.store(true, Ordering::Release);
    device.join().expect("the device joins");
    assert_eq!(ring_paced, [true, false], "a run its player's ring paces raises its worker while heard, and drops it back when its listener closes");
    // a listener on a run nothing paces (a render flat out, heard by no one keeping time with it)
    struct Ear;
    impl Sink for Ear {
        fn listened(&self) -> bool {
            true
        }
    }
    let (sink_tx, sinks) = SinkSet::new();
    sink_tx.try_send(SinkCmd::Add(1, Box::new(Ear))).expect("the ear is queued");
    let (sim, raised) = unpaced(false);
    let flat_out = run(sim, raised, sinks, &mut |latest| wait_for(|| latest.latest().is_some_and(|(_, v)| v.t > 0.05)));
    assert_eq!(flat_out, [], "a listened run nothing paces (a render flat out) stays as it is");
}

/// A time resolution is a command like the rate: given at the same simulated time it gives the
/// same record however the worker slices its time, it steps far fewer points from there on, and the
/// publications carry it.
#[test]
fn a_resolution_command_gives_the_same_record_every_run() {
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, ..LiveSpec::default() };
    let res = 25e-6;
    let run = |slices: Option<f64>| {
        let (circuit, plan) = rc(pwm(), 1e3, false);
        let mut sim = LiveSim::new(circuit, plan, 1, spec.clone()).expect("RC live initial conditions converge");
        assert_eq!(sim.values(1.0).res, 0.0, "a run starts at its spec's resolution: Exact");
        let mut record = Record::default();
        record.begin(&sim.solver().unknowns);
        let step = |sim: &mut LiveSim, record: &mut Record, dt: f64| {
            assert!(sim.apply(LiveCmd::Step(dt)), "the Step keeps the worker");
            while sim.running() {
                let target = slices.map_or(f64::INFINITY, |s| sim.t() + s);
                sim.advance(target, slices.map(|_| Duration::ZERO), record).expect("the PWM RC steps");
                sim.values(1.0);
            }
            record.at.len()
        };
        let exact = step(&mut sim, &mut record, 1e-3);
        assert!(sim.apply(LiveCmd::SetRes(res)), "the resolution keeps the worker");
        assert_eq!(sim.values(1.0).res, res, "the publications carry the resolution");
        let fast = step(&mut sim, &mut record, 1e-3) - exact;
        (record, exact, fast)
    };
    let (record, exact, fast) = run(None);
    eprintln!("10 PWM periods: {exact} points at Exact, then {fast} at {res:e} s");
    assert!(2 * fast < exact, "a resolution of a quarter period steps far fewer points through 10 PWM periods: {fast} against Exact's {exact}");
    let (sliced, ..) = run(Some(0.7e-6));
    assert_eq!(sliced.at, record.at, "the same accepted times however the worker slices its time");
    assert_eq!(sliced.x, record.x, "the same solutions however the worker slices its time");
}

/// What is below the resolution is marked (`Sink::below_res`, `Stats::coarse_steps`): a 173 kHz
/// sine stepped at 25 us is, through its whole run (through 100 ohms, so the capacitor's charge
/// carries it: behind 1k it is a millivolt, under the tolerance after the first steps); a PWM's
/// unresolved edges are not, though their first steps sit at the floor with their error above
/// tolerance, since each edge point carries its jump exactly.
#[test]
fn what_is_below_the_resolution_is_marked_and_an_edge_is_not() {
    struct Below(Vec<f64>);
    impl Sink for Below {
        fn below_res(&mut self, at: f64) {
            self.0.push(at);
        }
    }
    let run = |source: Source, res: f64, resistance: f64| {
        let (circuit, plan) = rc(source, resistance, false);
        let spec = LiveSpec { rate: f64::INFINITY, playing: false, res, ..LiveSpec::default() };
        let mut sim = LiveSim::new(circuit, plan, 1, spec).expect("RC live initial conditions converge");
        let mut below = Below(Vec::new());
        assert!(sim.apply(LiveCmd::Step(1e-3)), "the Step keeps the worker");
        finish_step(&mut sim, &mut below);
        let stats = &sim.solver().stats;
        (stats.floor_steps, stats.coarse_steps, below.0)
    };
    let sine = Source::wave(Wave::Sin { vo: 0.0, va: 1.0, freq: 173e3, td: 0.0, theta: 0.0, phase: 0.0 });
    let (floor, coarse, below) = run(sine.clone(), 25e-6, 100.0);
    eprintln!("173 kHz sine at 25 us: {floor} floor steps, {coarse} coarse, {} marked", below.len());
    assert!(coarse > 10 && below.len() as u64 == coarse, "a sine far faster than the resolution is marked below it, each step once: {coarse} coarse, {} marked", below.len());
    assert!(below.windows(2).all(|w| w[0] < w[1]), "marks go out in time order");
    assert!(below.last().is_some_and(|&t| t > 0.9e-3), "the sine is marked through its whole run, not only its start: last mark at {:?} s of 1 ms", below.last());
    let (floor, coarse, below) = run(sine, 0.0, 100.0);
    assert_eq!((coarse, below.len()), (0, 0), "Exact marks nothing ({floor} floor steps)");
    let (floor, coarse, below) = run(pwm(), 25e-6, 1e3);
    eprintln!("10 kHz PWM at 25 us: {floor} floor steps, {coarse} coarse, {} marked", below.len());
    assert_eq!((coarse, below.len()), (0, 0), "a PWM's unresolved edges are carried by their edge points, not marked ({floor} floor steps)");
}

/// A paused run tells its sinks it is idle: at once, after a value change and at a `Step`'s end.
/// And what its values count of the commands sent (`LiveValues::stepped_with`, which a host's wait
/// for an edit reads): a timed command due where a `Step` ends applies as the next step begins,
/// so the values at that end count only the commands before it; the next `Step`'s count it.
#[test]
fn paused_live_run_tells_its_sinks_it_is_idle() {
    let (circuit, plan) = rc(pwm(), 1e3, false);
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, ..LiveSpec::default() };
    let sim = LiveSim::new(circuit, plan, 1, spec).expect("PWM RC live initial conditions converge");
    let idle = Arc::new(AtomicUsize::new(0));
    let begins = Arc::new(AtomicUsize::new(0));
    let (sink_tx, mut sinks) = SinkSet::new();
    sink_tx.try_send(SinkCmd::Add(1, Box::new(Idle(idle.clone(), begins.clone())))).expect("the scope's sink is queued");
    let latest = Arc::new(Latest::<LiveValues>::new());
    let (commands, command_rx) = mpsc::sync_channel(4);
    let (mut traces, _trace_rx) = mpsc::sync_channel(64);
    let published = Arc::clone(&latest);
    let worker = thread::spawn(move || run_live(sim, &command_rx, &published, &mut traces, &mut sinks, &|| {}).map(|_| ()));

    // a run that starts paused tells its sinks at once
    wait_for(|| idle.load(Ordering::Relaxed) >= 1);
    assert_eq!(begins.load(Ordering::Relaxed), 1, "a sink queued before the run begins once");
    let before = idle.load(Ordering::Relaxed);
    commands.try_send(resistance(1, "r1", 2e3)).expect("a paused value change is queued");
    // a value change while paused is followed by an idle call
    wait_for(|| idle.load(Ordering::Relaxed) > before);
    let before = idle.load(Ordering::Relaxed);
    commands.try_send(LiveCmd::Step(2e-4)).expect("a Step is queued");
    // the end of a Step is an idle call
    wait_for(|| latest.latest().is_some_and(|(_, v)| v.t >= 2e-4) && idle.load(Ordering::Relaxed) > before);
    let (_, ended) = latest.latest().expect("the Step's end is published");
    assert_eq!(ended.stepped_with, 2, "a Step's values count the value change and the Step");
    // a value change at the next Step's end, sent before that Step (commands 3 and 4)
    let end = ended.t + 2e-4;
    let before = idle.load(Ordering::Relaxed);
    commands.try_send(LiveCmd::At(end, Box::new(resistance(1, "r1", 4e3)))).expect("a timed value change is queued");
    commands.try_send(LiveCmd::Step(2e-4)).expect("a Step to its time is queued");
    wait_for(|| latest.latest().is_some_and(|(_, v)| v.t >= end) && idle.load(Ordering::Relaxed) > before);
    let (_, ended) = latest.latest().expect("the second Step's end is published");
    assert_eq!(ended.stepped_with, 2, "values at a timed change's time, stepped before it applied, count only the commands before it");
    let before = idle.load(Ordering::Relaxed);
    commands.try_send(LiveCmd::Step(2e-4)).expect("a third Step is queued");
    wait_for(|| latest.latest().is_some_and(|(_, v)| v.t > ended.t) && idle.load(Ordering::Relaxed) > before);
    let (_, ended) = latest.latest().expect("the third Step's end is published");
    assert_eq!(ended.stepped_with, 5, "the next Step applies the timed change as it begins: its values count every command");
    commands.try_send(LiveCmd::Stop).expect("Stop reaches the live worker");
    let result = worker.join().expect("the live worker joins without a panic");
    assert!(result.is_ok(), "the paused PWM RC run ends without a solver diagnostic");
}

#[test]
fn a_settled_bench_waits_for_a_command_and_a_pwm_never_settles() {
    let (circuit, plan) = rc(Source::dc(1.0), 1e3, false);
    let spec = LiveSpec { rate: f64::INFINITY, ..LiveSpec::default() };
    let sim = LiveSim::new(circuit, plan, 1, spec.clone()).expect("DC RC live initial conditions converge");
    let generation = sim.generation();
    let tolerance = spec.reltol_at(spec.res) * 1.0 + SimCircuit::new().opts.vntol;
    let latest = Arc::new(Latest::<LiveValues>::new());
    let wakes = Arc::new(AtomicU64::new(0));
    let (commands, command_rx) = mpsc::sync_channel(4);
    let (mut traces, _trace_rx) = mpsc::sync_channel(64);
    let published = Arc::clone(&latest);
    let wake_count = Arc::clone(&wakes);
    let worker = thread::spawn(move || {
        let mut sink = Record::default();
        run_live(sim, &command_rx, &published, &mut traces, &mut sink, &|| {
            wake_count.fetch_add(1, Ordering::Relaxed);
        })
    });

    // The reader can see a publication just before the worker calls its wake.
    wait_for(|| latest.latest().is_some_and(|(_, v)| v.steady) && wakes.load(Ordering::Relaxed) > 0);
    let (n1, first) = latest.latest().expect("the settled DC bench publishes its values");
    assert!((f64::from(first.net_v[2]) - 1.0).abs() <= tolerance, "the settled DC output is within Newton's tolerance of 1 V");
    assert!(wakes.load(Ordering::Relaxed) > 0, "a live publication wakes the host");
    let t1 = first.t;
    commands.try_send(LiveCmd::Retune { generation, device: "v1".into(), source: Source::dc(2.0) })
        .expect("a DC retune reaches the settled worker");
    wait_for(|| latest.latest().is_some_and(|(_, v)| v.steady && v.t > t1));
    let (n2, second) = latest.latest().expect("the retuned DC bench publishes its settled values");
    assert!((f64::from(second.net_v[2]) - 2.0).abs() <= tolerance, "the retuned settled DC output is within tolerance of 2 V");
    assert!(n2 > n1, "a retune wakes the settled bench and publishes new values");
    let t2 = second.t;
    // a loop that kept stepping would publish again within a few publication periods
    thread::sleep(spec.values_every * 4);
    assert_eq!(latest.latest().expect("the settled values remain available").0, n2, "no publication after settling");
    // what changes only what is published or how leaves the settled point standing: a sink set
    // (a Listen joining), the pace, the record, the detail, the resolution, a subscription and
    // the streamed inputs are published again at the settled time, with no step
    for cmd in [LiveCmd::Sinks, LiveCmd::SetRate(1.0), LiveCmd::SetWhole(Arc::from([true])), LiveCmd::LogicDetail(Vec::new()), LiveCmd::SetRes(1e-6),
        LiveCmd::Subscribe { generation, pins: Vec::new(), parts: Vec::new() }, LiveCmd::Inputs(Inputs { sources: Vec::new(), want: Arc::new(AtomicU64::new(0)) })] {
        commands.send(cmd).expect("a publication command reaches the settled worker");
    }
    wait_for(|| latest.latest().is_some_and(|(n, _)| n > n2));
    thread::sleep(spec.values_every * 4);
    commands.try_send(LiveCmd::Stop).expect("Stop reaches the settled worker");
    let sim = worker.join().expect("the settled worker joins without a panic")
        .expect("the settled DC run ends without a solver diagnostic");
    assert_eq!(sim.t(), t2, "a settled bench steps no further, for a sink, the pace, the record, the detail, the resolution, a subscription or the inputs either");
    assert!(latest.latest().is_some_and(|(_, v)| v.t == t2 && v.steady), "what was published again is the settled point");

    let source = pwm();
    let period = match &source.wave {
        Wave::Pulse { per, .. } => *per,
        _ => panic!("the PWM helper supplies a periodic pulse"),
    };
    let (circuit, plan) = rc(source, 1e3, false);
    let sim = LiveSim::new(circuit, plan, 1, spec).expect("PWM RC live initial conditions converge");
    let latest = Arc::new(Latest::<LiveValues>::new());
    let (commands, command_rx) = mpsc::sync_channel(4);
    let (mut traces, _trace_rx) = mpsc::sync_channel(64);
    let published = Arc::clone(&latest);
    let worker = thread::spawn(move || {
        let mut sink = Record::default();
        run_live(sim, &command_rx, &published, &mut traces, &mut sink, &|| {})
    });
    wait_for(|| latest.latest().is_some_and(|(_, v)| {
        assert!(!v.steady, "a PWM keeps publishing and never settles");
        v.t > 3.0 * period
    }));
    commands.try_send(LiveCmd::Stop).expect("Stop reaches the PWM worker");
    worker.join().expect("the PWM worker joins without a panic")
        .expect("the PWM run ends without a solver diagnostic");
}

/// In a live run the charge cap's current scale is the record's, not the session's (ESIM-Q9): every
/// sample outside a corner's first is within 10 x reltol of the full scale of the record span that
/// ends at it (the largest |reference| over `[t - span, t]`), the guarantee a batch run gives
/// against its own peak. The rail's supply switches on at t = 0 (an inrush of amperes, its modes
/// 31 ns and 0.97 us); a 2 to 41 mA load pulse follows one span later, four edges.
///
/// What is promised and what is not (ESIM-Q11): a quantity small next to an earlier transient in
/// the same record is held absolutely against that transient. At td the inrush's slow tail is 35 %
/// low (0.25 mA of 0.71 mA), which is 1.2e-5 of the 20 A the record still shows; its relative
/// accuracy is not promised until the inrush has left the record (about 16.7 us), from where the
/// samples are judged at the load's own scale. A scale kept from the inrush for the session leaves
/// the load's edges uncontrolled and fails there.
#[test]
fn a_live_run_forgets_an_inrush_after_one_record_span() {
    let (span, reltol) = (10e-6, 1e-4);
    // the rail of a_rail_current_follows_a_dense_reference_after_corners, its supply switched on
    // at t = 0 with a 10 ns ramp, then a 2 to 41 mA load pulse half a microsecond after one span
    let (td, tr, pw) = (span + 0.5e-6, 5e-9, 2.5e-6);
    let tstop = td + 4.0 * pw;
    let corners = [0.0, 1.0].map(|n| td + n * 2.0 * pw).map(|rise| [rise, rise + tr, rise + tr + pw, rise + 2.0 * tr + pw]).concat();
    let mut c = SimCircuit::new();
    c.v("vs", "src", "0", Source::wave(Wave::Pulse { v1: 0.0, v2: 5.0, td: 0.0, tr: 10e-9, tf: 10e-9, pw: 1.0, per: 2.0 }));
    c.r("rs", "src", "a", 0.1);
    c.c("ca", "a", "0", 1e-6);
    c.r("rm", "a", "b", 0.05);
    c.c("cb", "b", "0", 6e-6);
    c.i("iload", "b", "0", Source::wave(Wave::Pulse { v1: 2e-3, v2: 41e-3, td, tr, tf: tr, pw, per: 2.0 * pw }));
    let currents = |record: &Record| {
        let at = |name: &str| record.names.iter().position(|n| n == name).unwrap_or_else(|| panic!("no {name} recorded"));
        let (vs, a, b) = (at("vs#branch"), at("a"), at("b"));
        record.x.iter().map(|x| [x[vs], (x[a] - x[b]) / 0.05]).collect::<Vec<[f64; 2]>>()
    };
    let mut dense = c.clone();
    dense.opts.reltol = 1e-8;
    let mut reference = Record::default();
    run(&dense, &Analysis::Tran(TranSpec { tstop, max_step: Some(2e-9), uic: false }), &mut reference, &Control::default())
        .unwrap_or_else(|diag| panic!("the dense reference runs: {diag}"));
    let ri = currents(&reference);
    let inrush = reference.at.iter().zip(&ri).filter(|(&t, _)| t < 1e-6).map(|(_, i)| i[0].abs()).fold(0.0, f64::max);
    assert!(inrush > 1.0, "the supply's inrush is amperes (the scale a session would otherwise keep): {inrush:.3e} A");
    let reference_at = |t: f64, k: usize| {
        let hi = reference.at.partition_point(|&s| s < t).clamp(1, reference.at.len() - 1);
        let f = (t - reference.at[hi - 1]) / (reference.at[hi] - reference.at[hi - 1]);
        ri[hi - 1][k] + f * (ri[hi][k] - ri[hi - 1][k])
    };
    // the full scale of the record span ending at t
    let record_scale = |t: f64, k: usize| {
        let (lo, hi) = (reference.at.partition_point(|&s| s < t - span), reference.at.partition_point(|&s| s <= t));
        ri[lo..hi].iter().map(|i| i[k].abs()).fold(0.0, f64::max)
    };

    let plan = ValuePlan {
        nets: vec!["0".into(), "src".into(), "a".into(), "b".into()],
        pins: ["vs", "rs", "ca", "rm", "cb", "iload"].iter().flat_map(|d| [pin(d, 0), pin(d, 1)]).collect(),
        parts: ["vs", "rs", "ca", "rm", "cb", "iload"].iter().map(|d| vec![d.to_string()]).collect(),
        pin_net: [1, 0, 1, 2, 2, 0, 2, 3, 3, 0, 3, 0].into_iter().map(Some).collect(),
        part_pins: (0..6).map(|p| (2 * p, 2)).collect(),
    };
    let spec = LiveSpec { rate: f64::INFINITY, max_step: span, reltol: Some(reltol), record: span, ..LiveSpec::default() };
    let mut sim = LiveSim::new(c, plan, 1, spec).expect("the rail's live operating point converges");
    let mut live = Record::default();
    live.begin(&sim.solver().unknowns);
    while sim.t() < tstop {
        sim.advance(tstop, None, &mut live).unwrap_or_else(|diag| panic!("the live rail steps: {diag}"));
    }
    let li = currents(&live);
    let first = corners.iter().map(|&corner| live.at.partition_point(|&t| t <= corner)).collect::<Vec<_>>();
    let names = ["the source branch", "the 0.05 ohm resistor"];
    let mut failures = Vec::new();
    for k in 0..2 {
        // the worst error against its record's full scale: from td while the inrush is in the
        // record, and from one span after its tail falls under the load's currents, when only the
        // load is
        let load = reference.at.iter().zip(&ri).filter(|(&t, _)| t >= td).map(|(_, i)| i[k].abs()).fold(0.0, f64::max);
        let tail = reference.at.iter().zip(&ri).filter(|(&t, i)| t < td && i[k].abs() > load).map(|(&t, _)| t).fold(0.0, f64::max);
        let load_from = tail + span;
        let mut worst = [(0.0f64, 0.0f64, 0.0f64); 2];
        for (p, &t) in live.at.iter().enumerate().filter(|&(p, &t)| t >= td && t <= tstop && !first.contains(&p)) {
            let fs = record_scale(t, k);
            let e = (li[p][k] - reference_at(t, k)).abs() / fs;
            let w = &mut worst[usize::from(t >= load_from)];
            if e > w.0 {
                *w = (e, t, fs);
            }
        }
        println!("{}: the inrush leaves the record at {load_from:.4e} s, points {}", names[k], live.at.len());
        for (part, (e, t, fs)) in ["with the inrush in the record", "at the load's scale"].iter().zip(worst) {
            println!("{} {part}: worst {e:.3e} of its record's full scale {fs:.3e} A at {t:.4e} s", names[k]);
            if e > 10.0 * reltol {
                failures.push(format!("{} {part}: within 10 x reltol of its record's full scale {fs:.3e} A: {e:.3e} at {t:.4e} s", names[k]));
            }
        }
    }
    assert!(failures.is_empty(), "the live rail after its inrush: {failures:#?}");
}

/// Six Schmitt-trigger RC oscillators (two hysteretic switches each, both reading the input
/// against ground; 50 ohm on, 1 Gohm off, the default band), 110k to 390k onto 10 nF, their
/// outputs mixed through 100k into 16k: a drum machine's metal bank, about 7 600 switching edges a
/// second. Live under a resolution of one 96 kHz sample, every switching restart is a jump whose
/// provisional step and confirming step are each a sample long; a confirming step whose Newton
/// failed was retried at that same length and failed again for good, so the worker never returned.
/// Each oscillator keeps its period, R C (ln(3.15 / 2.15) + ln(2.85 / 1.85)), within 1 %. No step's
/// Newton fails at an edge: a step that would carry an input across its switch's band is solved
/// with the switch as it was and cut to the crossing, where it once swung the output across the
/// supply at every iteration and failed.
/// A key pressed between solves (`SetParam` on its press source) closes its switch at once, in
/// Exact and under a resolution. The control steps from 0 to 1 V with no time inside any step, so
/// every step from the press crosses the threshold at the same share of its length; the stepper
/// takes that crossing a sliver after the press instead of cutting to it until the step has no
/// length and Newton fails (drum16's step key, pressed live, stopped its board there).
#[test]
fn a_key_pressed_between_solves_closes_its_switch_at_once() {
    use fab_elec_sim::ModelKind;
    for res in [0.0, 1.0 / 96e3] {
        let mut c = SimCircuit::new();
        c.model("button", ModelKind::Sw, &[("ron", 0.1), ("roff", 1e9), ("vt", 0.5), ("vh", 0.0), ("vband", 0.1)]);
        c.model("dm", ModelKind::Diode, &[("cjo", 1e-12)]);
        c.v("v5", "v5", "0", Source::dc(5.0));
        c.r("rpull", "v5", "row", 10e3);
        c.c("crow", "row", "0", 100e-12);
        c.d("dkey", "row", "k", "dm");
        c.v("press", "ctl", "0", Source::dc(0.0));
        c.sw("key", "k", "0", "ctl", "0", "button", false);
        let spec = LiveSpec { rate: f64::INFINITY, playing: false, max_step: 1.0 / 96e3, res, trace: false, ..LiveSpec::default() };
        let mut sim = LiveSim::new(c, ValuePlan::default(), 1, spec).expect("the operating point converges");
        let mut record = Record::default();
        record.begin(&sim.solver().unknowns);
        let run_to = |sim: &mut LiveSim, record: &mut Record, stop: f64| {
            while sim.t() < stop {
                sim.advance(stop, None, record).unwrap_or_else(|d| panic!("res {res}: the key's circuit steps on: {d}"));
            }
        };
        run_to(&mut sim, &mut record, 1e-3);
        assert!(record.last("row") > 4.9, "res {res}: the row rests high: {} V", record.last("row"));
        let (pressed, failed) = (sim.t(), sim.solver().stats.rejected_newton);
        assert!(sim.apply(LiveCmd::SetParam { generation: sim.generation(), device: "press".into(), key: "dc".into(), value: 1.0 }));
        run_to(&mut sim, &mut record, 2e-3);
        let row = record.v("row");
        let low = (0..record.at.len()).find(|&n| record.at[n] > pressed && row[n] < 1.0).map(|n| record.at[n] - pressed);
        assert_eq!(sim.solver().stats.rejected_newton, failed, "res {res}: the press costs no failed Newton solve");
        assert!(low.is_some_and(|dt| dt < 1.001 / 96e3), "res {res}: the pressed key pulls the row low within a sample: after {low:?} s");
        assert!(record.last("row") < 1.0, "res {res}: the row stays low while the key is held: {} V", record.last("row"));
    }
}

/// The resistors of [`schmitt_bank`]'s six oscillators, slowest first: about 300 to 1100 Hz.
const BANK_R: [f64; 6] = [390e3, 300e3, 240e3, 200e3, 150e3, 110e3];

/// Six Schmitt-trigger RC oscillators (a hex inverter's, each section two switches with
/// hysteresis), their outputs summed into `metal`: about 7 600 edges a second in all.
fn schmitt_bank() -> SimCircuit {
    use fab_elec_sim::ModelKind;
    let mut c = SimCircuit::new();
    c.model("lo", ModelKind::Sw, &[("vt", 2.35), ("vh", 0.5), ("ron", 50.0), ("roff", 1e9)]);
    c.model("hi", ModelKind::Sw, &[("vt", -2.35), ("vh", 0.5), ("ron", 50.0), ("roff", 1e9)]);
    c.v("v5", "v5", "0", Source::wave(Wave::Pulse { v1: 0.0, v2: 5.0, td: 0.0, tr: 10e-6, tf: 10e-6, pw: 1.0, per: 2.0 }));
    for (k, r) in BANK_R.into_iter().enumerate() {
        let (i, o) = (format!("i{k}"), format!("o{k}"));
        c.sw(&format!("slo{k}"), &o, "0", &i, "0", "lo", false);
        c.sw(&format!("shi{k}"), "v5", &o, "0", &i, "hi", true);
        c.r(&format!("rf{k}"), &o, &i, r);
        c.c(&format!("ct{k}"), &i, "0", 10e-9);
        c.c(&format!("co{k}"), &o, "0", 10e-12);
        c.r(&format!("rm{k}"), &o, "metal", 100e3);
    }
    c.r("rmetal", "metal", "0", 16e3);
    c
}

/// Fast keeps a Schmitt bank's pitch: at a 48 kHz sim rate, where the bank's edges restart the
/// stepper's history some 7 600 times a second, each oscillator's period is Exact's at the same
/// rate to a cent (the steps after an edge must not lag the RC charges, or the lags add up to a
/// delay every period: with full backward Euler steps the bank ran 0.6 to 2.5 cents flat, in
/// proportion to the frequency). The periods are read on the timing capacitors, whose charge is
/// smooth, between their mid-band rising crossings.
#[test]
fn fast_keeps_a_schmitt_banks_pitch_at_a_48_khz_sim_rate() {
    let periods = |res: f64| -> Vec<f64> {
        let spec = LiveSpec { rate: f64::INFINITY, playing: false, sample_rate: 48e3, res, trace: false, ..LiveSpec::default() };
        let mut sim = LiveSim::new(schmitt_bank(), ValuePlan::default(), 1, spec).expect("the operating point converges");
        let mut record = Record::default();
        record.begin(&sim.solver().unknowns);
        let stop = 12e-3;
        while sim.t() < stop {
            sim.advance(stop, None, &mut record).unwrap_or_else(|d| panic!("res {res}: the bank steps: {d}"));
        }
        (0..BANK_R.len()).map(|k| {
            let i = record.v(&format!("i{k}"));
            let ups: Vec<f64> = (1..record.at.len())
                .filter(|&n| i[n - 1] < 2.35 && i[n] >= 2.35)
                .map(|n| record.at[n - 1] + (record.at[n] - record.at[n - 1]) * (2.35 - i[n - 1]) / (i[n] - i[n - 1]))
                .collect();
            assert!(ups.len() >= 4, "res {res}: oscillator {k} runs through 12 ms: rising crossings at {ups:?}");
            (ups[ups.len() - 1] - ups[1]) / (ups.len() - 2) as f64
        }).collect()
    };
    let (exact, fast) = (periods(0.0), periods(100e-6));
    let cents: Vec<f64> = (0..BANK_R.len()).map(|k| 1200.0 * (exact[k] / fast[k]).log2()).collect();
    eprintln!("the bank at 48 kHz, Fast against Exact: {cents:+.3?} cents");
    for (k, cents) in cents.into_iter().enumerate() {
        assert!(cents.abs() < 1.0, "oscillator {k} ({:.0} Hz): Fast is {cents:+.3} cents from Exact (periods {:e} and {:e} s)", 1.0 / exact[k], fast[k], exact[k]);
    }
}

#[test]
fn a_bank_of_schmitt_oscillators_runs_live_under_a_resolution_at_its_periods() {
    const R: [f64; 6] = BANK_R;
    let c = schmitt_bank();
    let sample = 1.0 / 96e3;
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, max_step: sample, res: sample, trace: false, ..LiveSpec::default() };
    let mut sim = LiveSim::new(c, ValuePlan::default(), 1, spec).expect("the operating point converges");
    sim.values(1.0);
    let mut record = Record::default();
    record.begin(&sim.solver().unknowns);
    let stop = 12e-3;
    while sim.t() < stop {
        sim.advance(stop, None, &mut record).unwrap_or_else(|d| panic!("the bank steps: {d}"));
    }
    assert_eq!(sim.solver().stats.rejected_newton, 0, "Newton failures over 12 ms of the bank's edges");
    // an edge the history sees coming is cut to before its step is solved: no trial step
    let (rs, st) = (sim.restarts(), &sim.solver().stats);
    assert!(4 * st.rejected_crossing <= rs.crossing,
        "at most one edge in four costs a trial step: {} crossings ({} cut ahead, {} cuts ahead short), {} crossing rejections", rs.crossing, rs.crossing_ahead, rs.ahead_short, st.rejected_crossing);
    for (k, r) in R.into_iter().enumerate() {
        let o = record.v(&format!("o{k}"));
        let ups: Vec<f64> = (1..record.at.len())
            .filter(|&n| o[n - 1] < 2.5 && o[n] >= 2.5)
            .map(|n| record.at[n - 1] + (record.at[n] - record.at[n - 1]) * (2.5 - o[n - 1]) / (o[n] - o[n - 1]))
            .collect();
        assert!(ups.len() >= 4, "oscillator {k} runs through 12 ms: rising edges at {ups:?}");
        let period = (ups[ups.len() - 1] - ups[1]) / (ups.len() - 2) as f64;
        let want = r * 10e-9 * ((3.15f64 / 2.15).ln() + (2.85f64 / 1.85).ln());
        assert!((period / want - 1.0).abs() < 1e-2, "oscillator {k}: period {period:e} s, R C (ln(3.15/2.15) + ln(2.85/1.85)) {want:e} s");
    }
}

#[test]
fn a_views_probe_keeps_its_signal_over_the_run_and_changes_no_step() {
    let run = |probe: Option<usize>| {
        let (circuit, plan) = rc(pwm(), 1e3, false);
        let spec = LiveSpec { rate: f64::INFINITY, playing: false, ..LiveSpec::default() };
        let mut sim = LiveSim::new(circuit, plan, 1, spec).expect("the PWM RC's initial conditions converge");
        if let Some(k) = probe {
            let mut whole = vec![false; k + 1];
            whole[k] = true;
            assert!(sim.apply(LiveCmd::SetWhole(whole.into())), "the probes ask keeps the worker");
        }
        let mut record = Record::default();
        record.begin(&sim.solver().unknowns);
        assert!(sim.apply(LiveCmd::Step(0.06)), "the Step keeps the worker");
        finish_step(&mut sim, &mut record);
        let store = sim.store().clone();
        (record, store)
    };
    let (record, own) = run(None);
    let steps = record.at.len();
    // signal 2 is the net "out" (the plan's nets: 0, in, out)
    let (probed_record, probed) = run(Some(2));
    let probed_steps = probed_record.at.len();
    let span = |s: &TraceStore| s.time.at(s.len() - 1) - s.time.at(0);
    let front = LiveSpec::default().record;
    eprintln!("60 ms of PWM RC: {steps} steps; the bench's record keeps {} points over {:.4} s, one probe {} over {:.4} s",
        own.len(), span(&own), probed.len(), span(&probed));
    assert!(steps > 3 * CHUNK, "the run closes several positions ({steps} steps)");
    assert!(span(&own) < 2.0 * front, "with no probe the record keeps its front: {} s", span(&own));
    assert!(probed.base == 0 && probed.time.at(0) < 1e-6, "a probe keeps the record from the run's start: its first point at {} s", probed.time.at(0));
    let old = probed.index_at(probed.time.at(probed.len() - 1) - 2.0 * front).expect("a point older than the front") - probed.base;
    assert!(probed.signals[2].at(old).is_finite() && probed.signals[2].at(0).is_finite(), "the probed signal reads over the whole run");
    assert!(probed.signals[1].at(0).is_nan(), "another signal is absent past the front");
    assert!(probed.signals[1].at(probed.len() - 1).is_finite(), "and present within it");
    // the probed signal is the run's own steps, whole: each kept point is the one the run took
    let out = probed_record.v("out");
    let mut j = 0;
    for i in 0..probed.len() {
        let t = probed.time.at(i);
        while j < probed_record.at.len() && probed_record.at[j] < t {
            j += 1;
        }
        assert!(j < probed_record.at.len() && probed_record.at[j] == t, "the record's point at {t} s is one the run took");
        let (kept, took) = (f64::from(probed.signals[2].at(i)), out[j]);
        assert!((kept - took).abs() <= 1e-6 * took.abs().max(1.0), "at {t} s the record keeps {kept} V where the run had {took} V");
    }
    assert_eq!(probed_steps, steps, "the probe changes what is kept, not a single step");
}

/// A host that must keep up names the rows its views show (`LiveCmd::SetFine`): from the next
/// point on the trace writes only those, a gap for the others, in the worker's store and in the
/// deltas a consumer applies alike; a named row keeps every point the run took, one named again
/// is written from then on, and the stepping does not change.
#[test]
fn the_trace_writes_only_the_rows_a_host_names_from_then_on() {
    // the plan's signals: 0, in, out; every one kept whole, so only the mask leaves gaps
    let masks: [Option<&[bool]>; 4] = [None, Some(&[false, false, true]), Some(&[false, true]), None];
    let run = |named: bool| {
        let (circuit, plan) = rc(pwm(), 1e3, false);
        let spec = LiveSpec { rate: f64::INFINITY, playing: false, whole: Arc::from([true, true, true]), ..LiveSpec::default() };
        let mut sim = LiveSim::new(circuit, plan, 1, spec).expect("the PWM RC's initial conditions converge");
        let mut record = Record::default();
        record.begin(&sim.solver().unknowns);
        let mut consumer = TraceStore::default();
        let (mut ends, mut empty_rows, mut partial_rows) = (Vec::new(), 0, 0);
        // the last stage short: it ends in the position it starts in, so the consumer's open tail
        // there holds a row written again after a delta with no point of it
        for (mask, secs) in masks.into_iter().zip([0.02, 0.02, 0.02, 0.002]) {
            let fine = mask.filter(|_| named).map(Arc::from);
            assert!(sim.apply(LiveCmd::SetFine(fine)), "the rows named keep the worker");
            assert!(sim.apply(LiveCmd::Step(secs)), "the Step keeps the worker");
            finish_step(&mut sim, &mut record);
            // the second stage's delta goes out with the third's: a row left out over both has no
            // point there, one written in either a value at each
            while ends.len() != 1 {
                let Some(d) = sim.take_delta() else { break };
                assert!(d.values.iter().all(|v| v.is_empty() || v.len() == d.time.len()), "a delta's row has a value at each of its points or none");
                empty_rows += d.values.iter().filter(|v| v.is_empty() && !d.time.is_empty()).count();
                partial_rows += d.values.iter().filter(|v| v.iter().any(|y| y.is_nan()) && v.iter().any(|y| y.is_finite())).count();
                consumer.apply(&d);
            }
            ends.push(sim.store().len());
        }
        (record, sim.store().clone(), consumer, ends, empty_rows, partial_rows)
    };
    let (all_record, all, _, all_ends, ..) = run(false);
    let (record, named, consumer, ends, empty_rows, partial_rows) = run(true);
    assert!(record.at.len() == all_record.at.len() && record.at.iter().zip(&all_record.at).all(|(a, b)| a.to_bits() == b.to_bits()), "naming rows changes no step");
    assert_eq!(ends, all_ends, "the trace keeps every point either way");
    assert!(ends[1] - ends[0] > CHUNK, "a stage with a row left out covers a whole position ({} points)", ends[1] - ends[0]);
    assert_eq!(ends[2] / CHUNK, ends[3] / CHUNK, "the last stage ends in the position it starts in");
    assert!(empty_rows > 0 && partial_rows > 0, "a delta had a row with no point ({empty_rows}) and one written over a part of it ({partial_rows})");
    let stage = |i: usize| ends.iter().position(|&end| i < end).expect("a point of a stage");
    let written = |i: usize, k: usize| masks[stage(i)].is_none_or(|m| m.get(k).copied().unwrap_or(false));
    for k in 0..3 {
        for i in 0..named.len() {
            let (v, want) = (named.signals[k].at(i), all.signals[k].at(i));
            assert!(if written(i, k) { v.to_bits() == want.to_bits() } else { v.is_nan() }, "row {k} at point {i} (stage {}): {v} where every row has {want}", stage(i));
            assert_eq!(consumer.signals[k].at(i).to_bits(), v.to_bits(), "the consumer reads row {k} at point {i} as the worker");
        }
        assert!(named.signals[k].chunks.iter().zip(&consumer.signals[k].chunks).all(|(a, b)| Arc::ptr_eq(a, b)), "the consumer shares row {k}'s closed chunks");
        for (p, chunk) in named.signals[k].chunks.iter().enumerate() {
            let none = !(p * CHUNK..(p + 1) * CHUNK).any(|i| written(i, k));
            assert_eq!(fab_elec_sim::trace::is_absent(chunk), none, "row {k}'s position {p} is the absent chunk exactly when it has no point");
        }
    }
    assert_eq!(consumer.len(), named.len(), "the consumer has every point");
}

/// Exact steps at the bench's tolerance (`Options::default().reltol`) and Fast at its own
/// (`LiveSpec::reltol_at`): a run's solver takes its mode's at the start and again at each switch
/// (`set_res`), there and back, and the run steps on at it; a spec that names a tolerance steps
/// both modes at that one.
#[test]
fn a_run_steps_each_mode_at_its_own_tolerance() {
    let (circuit, plan) = rc(Source::dc(1.0), 1e3, false);
    let exact = Options::default().reltol;
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, trace: false, ..LiveSpec::default() };
    let mut sim = LiveSim::new(circuit.clone(), plan.clone(), 1, spec.clone()).expect("the RC's operating point converges");
    let mut record = Record::default();
    let mut step = |sim: &mut LiveSim, to: f64| {
        while sim.t() < to {
            sim.advance(to, None, &mut record).unwrap_or_else(|d| panic!("the RC steps: {d}"));
        }
    };
    assert_eq!(sim.solver().opts.reltol, exact, "a run starts in Exact at the bench's tolerance");
    step(&mut sim, 1e-4);
    for (res, want, mode) in [(1e-5, FAST_RELTOL, "Fast"), (0.0, exact, "Exact again"), (1e-5, FAST_RELTOL, "Fast again")] {
        sim.set_res(res);
        assert_eq!(sim.solver().opts.reltol, want, "{mode} steps at its own tolerance");
        let t = sim.t();
        step(&mut sim, t + 1e-4);
        assert_eq!(sim.solver().opts.reltol, want, "{mode}: the run steps on at it");
    }
    let mut sim = LiveSim::new(circuit, plan, 1, LiveSpec { reltol: Some(1e-5), res: 1e-5, ..spec }).expect("the RC's operating point converges");
    assert_eq!(sim.solver().opts.reltol, 1e-5, "a named tolerance in Fast");
    sim.set_res(0.0);
    assert_eq!(sim.solver().opts.reltol, 1e-5, "and in Exact");
}
