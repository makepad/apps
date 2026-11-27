//! A lowered circuit's IMU drives an LED through compiled firmware and the S3 machine, from a
//! held stimulus and from a motion track the live run holds for; and the twin's network ports
//! reach the bench's fake servers (a folder of pictures among them) and the network its run owns.

#[allow(dead_code)]
#[path = "../../mcu/emit_s3/tests/support/island_fixture.rs"]
mod island_fixture;

use fab_elec_bench::bench::Supply;
use fab_elec_bench::{lower_circuit, BenchSpec, Firmware, ModelLibs};
use fab_elec_circuit::{Circuit, Component, Net, NetKind, QtyUnit, Quantity, Value};
use fab_elec_core::{BlockId, BoardId, ClassId, CompId, NetId, NetRef, PinIx};
use fab_elec_bench::net::{Fake, NetEndpoint, Real, Server};
use fab_elec_fw::{compile, maker, Built, Chip, FwInput, OpenNetwork, TrackInput};
use fab_elec_mcu_cosim::{watch, FwFrame, FwState, FwValue, NetConn, Upstream};
use fab_elec_mcu_rt::net::{code, Arrival, Network, Request, OFFLINE, PART_BINARY, PART_FIRST, PART_LAST, READY};
use fab_elec_mcu_island::Archive;
use fab_elec_mcu_emit_s3::wiring;
use fab_elec_mcu_parts::{MotionSample, MotionTrack, PartKind as BusKind, Stimulus, TrackRef};
use fab_elec_parts::{Fields, GpioCard, PartDef, PartKind, PinDef, PinKind, Symbol};
use fab_elec_sim::cosim::{Bus, CoRun, CoSim, CoSimDevice, CoState, StepTo, Watch};
use fab_elec_sim::input::{InputSource, Inputs};
use fab_elec_sim::live::{DeviceInput, LiveCmd};
use fab_elec_sim::livesim::{LiveSim, LiveSpec};
use fab_elec_sim::trace::LogicEdge;
use fab_elec_sim::{PinMode, SimDiag, Sink};
use fab_elec_simhost::events::{EventLog, EventSource};
use std::any::Any;
use std::sync::{atomic::{AtomicBool, AtomicU64, Ordering}, Arc, Mutex};
use std::time::{Duration, Instant};

// motion.rs's motion declaration and runtime.rs's output on GPIO33. Numeric pins are the
// document entry point's pin vocabulary; D33 is an alias supplied only by runtime.rs's helper.
const SOURCE: &str = r#"
let led = output(33)
let imu = motion(10000, fn() {
    if accel(2) > ACCEL_LSB_PER_G / 2 { write(led, high) }
})
"#;

fn component(id: u32, path: &str, sim: &str, pins: &[(&str, u32)]) -> Component {
    Component {
        id: CompId(id), path: path.into(), refdes: path.into(), ref_kind: Default::default(),
        part: Arc::new(PartDef {
            name: path.into(), kind: PartKind::Ic, symbol: Symbol::default(), footprint: None,
            body: None, emitter: None, gpio: None, lifecycle: None, approved: false, fields: Fields(vec![("sim".into(), sim.into())]),
            pins: pins.iter().map(|&(name, _)| PinDef {
                name: name.into(), pads: Vec::new(), unit: 0, limits: None, alternates: Vec::new(), delay: None,
                kind: if ["VDD", "GND"].contains(&name) { PinKind::PowerIn } else { PinKind::Bidi },
            }).collect(),
        }),
        value: None, conns: pins.iter().enumerate().map(|(i, &(_, n))| (PinIx(i as u16), NetId(n))).collect(),
        nc: Vec::new(), block: BlockId(0), dnp: false, fields: Fields::default(),
    }
}

fn value(comp: &mut Component, si: f64, unit: QtyUnit) {
    comp.value = Some(Value { text: si.to_string(), qty: Some(Quantity { si, unit }), tol: None });
}

fn card() -> GpioCard {
    GpioCard { r_high: 25.0, r_low: 25.0, pull_up: 30e3, vih: 0.7, vil: 0.3, leak: 1e-6, edge: None, c_pin: Some(5e-12) }
}

fn schematic(second_imu: bool) -> (Circuit, BenchSpec) {
    let mut mcu = component(0, "mcu", "mcu", &[
        ("GPIO15", 2), ("GPIO14", 3), ("GPIO33", 8),
        ("VDD", 1), ("GND", 0), ("XTAL1", 6), ("XTAL2", 7),
    ]);
    let def = Arc::make_mut(&mut mcu.part);
    def.kind = PartKind::Mcu;
    def.fields.0.push(("model".into(), "ESP32-S3".into()));
    def.gpio = Some(card());
    let mut parts = vec![mcu];
    for (path, addr, int1, int2) in [("imu", "107", 4, 5), ("imu2", "106", 10, 11)].into_iter().take(if second_imu { 2 } else { 1 }) {
        let mut imu = component(parts.len() as u32, path, "bus", &[
            ("SDA", 2), ("SCL", 3), ("INT1", int1), ("INT2", int2), ("VDD", 1), ("GND", 0),
        ]);
        let def = Arc::make_mut(&mut imu.part);
        def.fields.0.extend([("model".into(), "QMI8658C".into()), ("bus.addr".into(), addr.into())]);
        def.gpio = Some(card());
        parts.push(imu);
    }
    for (path, a, b, ohms) in [("pull_sda", 2, 1, 10e3), ("pull_scl", 3, 1, 10e3), ("led_r", 8, 9, 330.0)] {
        let mut r = component(parts.len() as u32, path, "resistor", &[("1", a), ("2", b)]);
        Arc::make_mut(&mut r.part).kind = PartKind::Resistor;
        value(&mut r, ohms, QtyUnit::Ohm);
        parts.push(r);
    }
    let mut led = component(parts.len() as u32, "led", "led", &[("A", 9), ("K", 0)]);
    Arc::make_mut(&mut led.part).kind = PartKind::Led;
    led.value = Some(Value { text: "red".into(), qty: None, tol: None });
    parts.push(led);
    let mut crystal = component(parts.len() as u32, "crystal", "passive", &[("1", 6), ("2", 7)]);
    Arc::make_mut(&mut crystal.part).kind = PartKind::Crystal;
    value(&mut crystal, 40e6, QtyUnit::Hertz);
    parts.push(crystal);
    let mut names = vec!["gnd", "vdd", "sda", "scl", "int1", "int2", "xtal1", "xtal2", "led_drive", "led_anode"];
    if second_imu { names.extend(["imu2_int1", "imu2_int2"]); }
    let circuit = Circuit {
        unit: BoardId(0), name: "motion_light".into(), comps: parts,
        nets: names.into_iter().enumerate().map(|(i, name)| Net {
            id: NetId(i as u32), name: name.into(), class: ClassId(0), volts: None, scope: None,
            kind: if i == 0 { NetKind::Ground } else if i == 1 { NetKind::Power } else { NetKind::Signal },
        }).collect(),
        ..Circuit::default()
    };
    let bench = BenchSpec {
        supplies: vec![Supply { net: NetRef { board: BoardId(0), net: NetId(1) }, v: 3.3, limit: None, ramp: None }],
        firmware: vec![Firmware { path: "mcu".into(), doc: "motion.fw.splash".into(), params: Vec::new() }],
        ..BenchSpec::default()
    };
    (circuit, bench)
}

// LiveSim owns the running device. Time only its fork, invoked by a final snapshot, excluding
// the analog snapshot and the capability probe performed when the device is attached.
struct TimedFork {
    inner: Box<dyn CoSim>,
    measure: Arc<AtomicBool>,
}

impl CoSim for TimedFork {
    fn next(&self) -> f64 { self.inner.next() }
    fn fork(&self) -> Option<Box<dyn CoSim>> {
        if !self.measure.swap(false, Ordering::Relaxed) { return self.inner.fork(); }
        let start = Instant::now();
        let fork = self.inner.fork();
        eprintln!("running S3 CoSim::fork wall time: {:?}", start.elapsed());
        fork
    }
    fn run(&mut self, bus: &mut dyn Bus) -> Result<CoRun, SimDiag> { self.inner.run(bus) }
    fn watches(&self) -> &[Watch] { self.inner.watches() }
    fn watch_gen(&self) -> u32 { self.inner.watch_gen() }
    fn arm(&mut self, t: f64, to: Option<StepTo>) -> bool { self.inner.arm(t, to) }
    fn state(&mut self, t: f64) -> Option<CoState> { self.inner.state(t) }
    fn logic_nets(&self) -> &[String] { self.inner.logic_nets() }
    fn take_logic(&mut self, t: f64, sync: bool, out: &mut Vec<(u32, LogicEdge)>) { self.inner.take_logic(t, sync, out); }
    fn restart_logic(&mut self, t: f64, out: &mut Vec<(u32, LogicEdge)>) { self.inner.restart_logic(t, out); }
    fn take_outputs(&mut self) -> Option<Box<dyn Any + Send>> { self.inner.take_outputs() }
    fn put_outputs(&mut self, outputs: Box<dyn Any + Send>) { self.inner.put_outputs(outputs); }
}

struct Discard;
impl Sink for Discard {}

#[test]
fn motion_on_the_lowered_bus_lights_the_led_and_a_second_imu_is_refused() {
    let libs = ModelLibs::generic();
    let (circuit, bench) = schematic(false);
    let lowered = lower_circuit(&circuit, &bench, 1, &libs);
    let binding = &lowered.firmware[0];
    assert_eq!(binding.bus.parts.len(), 1, "the IMU reaches the bus: {:?}", lowered.diags);
    let input = FwInput {
        part: binding.part.clone(), doc: binding.doc.clone(), rev: 1, text: SOURCE.into(),
        f_clk: binding.f_clk.expect("the crystal supplies the firmware clock"), params: binding.params.clone(),
        chip: binding.chip.as_deref().and_then(Chip::from_model).expect("the model selects S3"),
        wiring: Some(wiring(&binding.bus.board(0)).expect("the circuit's IMU wiring resolves")),
        island: None,
        files: Vec::new(), net_files: Vec::new(),
    };
    let built = compile(&input).expect("the lowered firmware builds");
    let (mut log, feed) = EventLog::<FwFrame>::new(3.0, |frame| {
        if frame.is_image() { EventSource::Image } else { EventSource::Trace }
    }, Arc::new(|| {}));
    let (make, _) = maker(&input, built, feed, false, Arc::new(|_| {}), None);
    let device = make(&lowered, 0).expect("the S3 and circuit bus form a device");
    let measure = Arc::new(AtomicBool::new(false));
    let device = CoSimDevice {
        dev: Box::new(TimedFork { inner: device.dev, measure: measure.clone() }), pins: device.pins,
    };
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, max_step: 1e-3, res: 0.0, trace: false, ..LiveSpec::default() };
    let mut sim = LiveSim::new(lowered.circuit.clone(), lowered.plan.clone(), lowered.index.generation, spec)
        .expect("the lowered circuit's operating point converges");
    sim.attach(Some(device));
    let anode = sim.solver().unknown("led_anode").expect("the LED anode has an analog node") as usize;
    let drive = sim.solver().unknown("led_drive").expect("the LED drive has an analog node") as usize;
    let lit = |sim: &LiveSim| sim.solver().x[anode] >= 1.0
        && (sim.solver().x[drive] - sim.solver().x[anode]) / 330.0 > 1e-3;
    assert!(sim.solver().x[anode] < 0.5, "the LED begins off");
    // The part model starts after 1.75 s; include sensor settling and the first 10 ms sample.
    const END: f64 = 2.0;
    let start = Instant::now();
    while sim.t() < END && !lit(&sim) {
        // One loop top per call lets the event log drain even while firmware waits at one
        // analog instant. The test's bound is END in simulated time, never wall time.
        sim.advance(END, Some(Duration::ZERO), &mut Discard).expect("the live circuit advances");
        log.drain();
    }
    let wall = start.elapsed();
    eprintln!("S3 simulated time reached: {:.6} ms; wall time per simulated millisecond: {:.6} ms",
        sim.t() * 1000.0, wall.as_secs_f64() / sim.t());
    measure.store(true, Ordering::Relaxed);
    let _snapshot = sim.snapshot().expect("the running S3 device forks");
    assert!(sim.solver().x[anode] >= 1.0, "gravity must light the LED within {END} simulated seconds");
    assert!((sim.solver().x[drive] - sim.solver().x[anode]) / 330.0 > 1e-3,
        "the high anode carries LED current through its series resistor");

    let (circuit, bench) = schematic(true);
    let lowered = lower_circuit(&circuit, &bench, 2, &libs);
    let binding = &lowered.firmware[0];
    assert_eq!(binding.bus.parts.len(), 2, "both IMUs reach wiring validation: {:?}", lowered.diags);
    assert_eq!(wiring(&binding.bus.board(0)).expect_err("a second IMU is refused"),
        "imu2: a second 6-axis IMU (one is supported)");
}

// R11a: with the radio pack installed, Fab's own path (compile, maker, the board machine on its
// native tier, the live run) starts the voicebox station from the image's flash and the stand-in.
#[test]
fn an_installed_radio_pack_starts_the_station_in_the_live_run() {
    let libs = ModelLibs::generic();
    let (circuit, bench) = schematic(false);
    let lowered = lower_circuit(&circuit, &bench, 1, &libs);
    let binding = &lowered.firmware[0];
    let names: Vec<_> = fab_elec_mcu_emit_s3::radio::stand_in::row_names().collect();
    let (_, archive) = island_fixture::station_archive_with_virtual_roots(vec![0; 4], 4, &names);
    let island = fab_elec_mcu_emit_s3::radio::link_pack(&[Archive { name: "station.a", data: &archive }])
        .expect("the fixture station pack links");
    let input = FwInput {
        part: binding.part.clone(), doc: binding.doc.clone(), rev: 1, text: SOURCE.into(),
        f_clk: binding.f_clk.expect("the crystal supplies the firmware clock"), params: binding.params.clone(),
        chip: Chip::S3, wiring: Some(wiring(&binding.bus.board(0)).expect("the circuit's IMU wiring resolves")),
        island: Some(Arc::new(island)),
        files: Vec::new(), net_files: Vec::new(),
    };
    let built = compile(&input).expect("the firmware builds with the radio");
    assert!(matches!(&built, Built::S3(fw) if fw.radio.is_some()), "an installed pack gives the image its radio");
    let (mut log, feed) = EventLog::<FwFrame>::new(3.0, |frame| {
        if frame.is_image() { EventSource::Image } else { EventSource::Trace }
    }, Arc::new(|| {}));
    let (make, _) = maker(&input, built, feed, false, Arc::new(|_| {}), None);
    let device = make(&lowered, 0).expect("the S3 with its island and stand-in forms a device");
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, max_step: 1e-3, res: 0.0, trace: false, ..LiveSpec::default() };
    let mut sim = LiveSim::new(lowered.circuit.clone(), lowered.plan.clone(), lowered.index.generation, spec)
        .expect("the lowered circuit's operating point converges");
    sim.attach(Some(device));
    // The device's first frames (its Hello, then the start) follow the boot's image CRC over the
    // radio-on slot: they reached the log at 99 ms of this run at 160 MHz. The station starts
    // about 160 us after the Hello.
    const END: f64 = 0.2;
    while sim.t() < END {
        sim.advance(END, Some(Duration::ZERO), &mut Discard).expect("the live circuit advances");
        log.drain();
    }
    let faults: Vec<_> = log.store.span(0.0, sim.t()).filter_map(|e| match &e.frame {
        FwFrame::Image(frame) => match frame.event { watch::Event::Fault { code, arg, .. } => Some((code, arg)), _ => None },
        _ => None,
    }).collect();
    assert_eq!(faults, [(watch::FAULT_RADIO_STARTED, 1), (watch::FAULT_RADIO_EVENT, 2)],
        "the station starts once and posts STA_START, with no other report, by {END} s");
}

// Q11 test 4, the seam: real firmware on the exact S3 reads the IMU at every 125 Hz sample of a
// tilting motion track, and the live run holds for the track (`fab_elec_sim::input`). The LED
// shows accel X's sign, so every reading the firmware takes reaches the analog points.
const TILT: &str = r#"
let led = output(33)
let imu = motion(10000, fn() {
    if accel(0) > 0 { write(led, high) } else { write(led, low) }
})
"#;

/// A Mechanics world's step rate.
const TRACK_HZ: u32 = 240;
/// Where the arms part: the part starts after 1.75 s, so the run up to here reads no motion and
/// every arm shares it.
const TILT_FORK: f64 = 1.7;
/// Where every arm ends: the firmware reads the part from about 1.85 s, so the LED follows several
/// half periods of the tilt.
const TILT_END: f64 = 2.0;

/// Sample `k` of the tilt: accel X a 25 Hz sine of half a g, +1 g on Z, no rotation.
fn tilt(k: u64) -> MotionSample {
    let phase = 2.0 * std::f64::consts::PI * 25.0 * k as f64 / f64::from(TRACK_HZ);
    MotionSample { accel_ug: [(500_000.0 * phase.sin()).round() as i32, 0, 1_000_000], gyro_mdps: [0; 3] }
}

/// Every point, its time and values as bits, in order.
struct Points(Vec<Vec<u64>>);

impl Sink for Points {
    fn point(&mut self, at: f64, _: Option<f64>, x: &[f64]) {
        self.0.push(std::iter::once(at.to_bits()).chain(x.iter().map(|v| v.to_bits())).collect());
    }
}

/// The run of test 4: the live run, its device's event log, and the track its IMU reads.
struct Tilt {
    sim: LiveSim,
    log: EventLog<FwFrame>,
    track: Arc<MotionTrack>,
}

impl Tilt {
    /// One advance of at most `budget` toward `end`; true when the run holds for its track.
    fn advance(&mut self, end: f64, budget: Duration, points: &mut dyn Sink) -> bool {
        self.sim.advance(end, Some(budget), points).expect("the live circuit advances");
        self.log.drain();
        self.sim.input_starved()
    }

    /// The track's next sample, as a Mechanics world writes it.
    fn feed(&mut self) {
        self.track.push(tilt(self.track.len()));
    }
}

#[test]
fn a_tilting_track_reaches_the_firmware_the_same_however_it_is_paced() {
    let libs = ModelLibs::generic();
    let (circuit, bench) = schematic(false);
    let lowered = lower_circuit(&circuit, &bench, 1, &libs);
    let binding = &lowered.firmware[0];
    let input = FwInput {
        part: binding.part.clone(), doc: binding.doc.clone(), rev: 1, text: TILT.into(),
        f_clk: binding.f_clk.expect("the crystal supplies the firmware clock"), params: binding.params.clone(),
        chip: binding.chip.as_deref().and_then(Chip::from_model).expect("the model selects S3"),
        wiring: Some(wiring(&binding.bus.board(0)).expect("the circuit's IMU wiring resolves")),
        island: None,
        files: Vec::new(), net_files: Vec::new(),
    };
    let built = compile(&input).expect("the tilt firmware builds");
    let (mut log, feed) = EventLog::<FwFrame>::new(3.0, |frame| {
        if frame.is_image() { EventSource::Image } else { EventSource::Trace }
    }, Arc::new(|| {}));
    let (make, _) = maker(&input, built, feed, false, Arc::new(|_| {}), None);
    let device = make(&lowered, 0).expect("the S3 and its IMU form a device");
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, max_step: 1e-3, res: 0.0, trace: false, ..LiveSpec::default() };
    let mut sim = LiveSim::new(lowered.circuit.clone(), lowered.plan.clone(), lowered.index.generation, spec)
        .expect("the lowered circuit's operating point converges");
    sim.attach(Some(device));
    let anode = 1 + sim.solver().unknown("led_anode").expect("the LED anode has an analog node") as usize;

    // The part every arm shares, with nothing to hold for: the IMU publishes no sample yet.
    while sim.t() < TILT_FORK {
        sim.advance(TILT_FORK, Some(Duration::ZERO), &mut Discard).expect("the live circuit advances");
        log.drain();
    }
    // As the link does: a track from the run's front, its source in the run's inputs, written as
    // far as the run asks it, then handed to the IMU there as a timed input.
    let track = Arc::new(MotionTrack::new((sim.t() * 1e12) as u64, (TRACK_HZ, 1)));
    let mut run = Tilt { sim, log, track };
    let source = Arc::new(TrackInput::new(run.track.clone(), "imu".into()));
    let allowed = source.allowed();
    assert!(source.first_want().is_none(), "a track the run never asked has no first want");
    assert!(run.sim.apply(LiveCmd::Inputs(Inputs { sources: vec![source.clone() as Arc<dyn InputSource>], want: Arc::new(AtomicU64::new(0)) })), "the inputs keep the run");
    while run.sim.input_starved() {
        let asked = allowed.load(Ordering::Acquire);
        assert!(asked > run.track.len(), "a held run asks its track for more than it has ({asked} samples asked)");
        while run.track.len() < asked {
            run.feed();
        }
    }
    assert!(source.first_want().is_some_and(|want| want > run.sim.t()), "the run's first ask is past where it stands");
    let imu = binding.bus.parts.iter().position(|part| matches!(part.kind, BusKind::Qmi8658c { .. }))
        .expect("the circuit has its IMU on the bus") as u16;
    let attach = Stimulus::Track { part: imu, track: TrackRef(run.track.clone()) };
    assert!(run.sim.apply(LiveCmd::Input { generation: lowered.index.generation, key: "track imu".into(), input: DeviceInput(Arc::new(attach)), replaceable: false }),
        "the track's attach keeps the run");
    let fork = run.sim.snapshot().expect("the S3 run forks");
    let written = run.track.len();

    // Paced: one sample ahead of each hold, as a Mechanics world slower than the electronics writes
    // it, with the normal slice (the case Q12 found).
    let slice = LiveSpec::default().slice;
    let mut paced = Points(Vec::new());
    let mut kept = None;
    while run.sim.t() < TILT_END {
        if run.advance(TILT_END, slice, &mut paced) {
            if kept.is_none() && run.sim.t() > 1.9 {
                // held, the run neither steps nor publishes a point, and a snapshot taken there
                // goes on as the run does
                let (t, n) = (run.sim.t(), paced.0.len());
                assert!(run.advance(TILT_END, slice, &mut paced), "a held run stays held until its track grows");
                assert_eq!((run.sim.t(), paced.0.len()), (t, n), "a held run neither steps nor publishes a point");
                kept = Some((run.sim.snapshot().expect("a held S3 run forks"), n));
            }
            run.feed();
        }
    }
    // The track covers where the run ends, so the runs sent back below never wait for it.
    while run.sim.input_starved() {
        run.feed();
    }
    let held = run.track.len() - written;
    assert!(held > 60, "the run held for each sample past the fork ({held} samples written while it held)");
    assert_eq!(run.track.short_reads(), 0, "no read passed the paced track");

    // The reference: the same run from the fork with the whole track written, one loop top an
    // advance, so it never holds.
    run.sim.restore(&fork);
    let mut reference = Points(Vec::new());
    while run.sim.t() < TILT_END {
        assert!(!run.advance(TILT_END, Duration::ZERO, &mut reference), "a run whose track covers its reach never holds, at {} s", run.sim.t());
    }
    let changes = reference.0.windows(2)
        .filter(|w| (f64::from_bits(w[0][anode]) >= 1.0) != (f64::from_bits(w[1][anode]) >= 1.0)).count();
    assert!(changes >= 4, "the firmware reads the tilt: the LED follows accel X's sign ({changes} changes)");
    let first = paced.0.iter().zip(&reference.0).position(|(a, b)| a != b);
    assert!(paced.0 == reference.0, "the paced run's points are the reference's, bit for bit ({} and {} points, first difference at {first:?})",
        paced.0.len(), reference.0.len());

    let (snapshot, n) = kept.expect("the run held after 1.9 s");
    run.sim.restore(&snapshot);
    let mut tail = Points(Vec::new());
    while run.sim.t() < TILT_END {
        assert!(!run.advance(TILT_END, slice, &mut tail), "a run sent back reads behind its track and never holds");
    }
    assert!(tail.0[..] == paced.0[n..], "the run from the snapshot taken while held goes on as the run did");

    // A closed track holds the run no more, and its readers keep its last sample.
    run.track.close();
    let past = TILT_END + 0.02;
    while run.sim.t() < past {
        assert!(!run.advance(past, slice, &mut Discard), "a closed track holds the run no more");
    }
    assert_eq!(run.track.short_reads(), 0, "a track is never read short");
}

// N2e: the twin's host end. The image's connection 0 reaches the bench's fake echo; connection 1
// its real server, which the test plays inside the process as the network the nominal run owns.
// Every 20 ms the client sends "ping" on each ready connection and keeps, per connection, how many
// messages came back and the last one's first byte plus 256 times its length. On connection 1 it
// also counts the bytes of a long reply of 7s (`long_b`), read 64 bytes at a time.
const NET_CLIENT: &str = r#"
let rx = bytes(64)
let tx = bytes(4)
var got_a: i32 = 0
var last_a: i32 = 0
var got_b: i32 = 0
var last_b: i32 = 0
var long_b: i32 = 0

fn setup() {
    tx[0] = 112
    tx[1] = 105
    tx[2] = 110
    tx[3] = 103
    net_open(0)
    net_open(1)
}

fn ping(c: i32) {
    if net_state(c) == net_ready {
        net_send(c, tx, 0, 4, net_binary)
    }
}

let a = net(net_websocket, 2, 64, fn() {
    for k in 0..4 {
        if net_left(0) == 0 {
            break
        }
        let n = net_recv(0, rx, 0, 64)
        got_a = got_a + 1
        last_a = rx[0] + 256 * n
    }
})

let b = net(net_websocket, 2, 64, fn() {
    for k in 0..64 {
        let n = net_recv(1, rx, 0, 64)
        if n == 0 {
            break
        }
        if rx[0] == 7 {
            long_b = long_b + n
        } else {
            got_b = got_b + 1
            last_b = rx[0] + 256 * n
        }
    }
})

let tick = every(ms(20), fn() {
    ping(0)
    ping(1)
})
"#;

/// A reply the played network sends as a connection opens: six parts, three times the client's two
/// slots, so it reaches the device only as the device reads and frees them.
const LONG: usize = 5 * fab_elec_mcu_rt::net::NET_PART + 100;

/// The real network as the test plays it, in the process: it takes every open and every part, and
/// answers each whole message with its bytes reversed; an open it answers with [`LONG`] 7s too.
/// What it was asked goes to `asked`.
#[derive(Default)]
struct Played {
    asked: Arc<Mutex<Vec<Request>>>,
    answers: Vec<Arrival>,
    partial: Vec<u8>,
}

impl Network for Played {
    fn send(&mut self, request: &Request) {
        self.asked.lock().unwrap().push(request.clone());
        match request {
            &Request::Open { c, gen } => {
                self.answers.push(Arrival::Opened { c, gen });
                self.answers.push(Arrival::Message { c, gen, binary: true, bytes: vec![7; LONG] });
            }
            Request::Send { c, gen, flags, bytes } => {
                self.answers.push(Arrival::Taken { c: *c, gen: *gen, bytes: bytes.len() as u32 });
                if flags & PART_FIRST != 0 { self.partial.clear(); }
                self.partial.extend_from_slice(bytes);
                if flags & PART_LAST != 0 {
                    let mut bytes = std::mem::take(&mut self.partial);
                    bytes.reverse();
                    self.answers.push(Arrival::Message { c: *c, gen: *gen, binary: flags & PART_BINARY != 0, bytes });
                }
            }
            Request::Close { .. } => {}
        }
    }
    fn close_all(&mut self) { self.answers.clear(); }
    fn answers(&mut self, out: &mut Vec<Arrival>) { out.append(&mut self.answers); }
}

/// The analog for a device whose pins the test leaves out of the circuit: its clock only.
struct Clock(f64);

impl Bus for Clock {
    fn t(&self) -> f64 { self.0 }
    fn v(&self, _: u32) -> f64 { f64::NAN }
    fn supply(&self, _: u32) -> f64 { f64::NAN }
    fn advance(&mut self, t: f64) -> Result<(), SimDiag> { self.0 = self.0.max(t); Ok(()) }
    fn write(&mut self, _: u32, _: PinMode) -> bool { true }
    fn hits(&self) -> &[(u32, f64)] { &[] }
}

/// Runs `dev` at its instants up to `end`, draining the event log; its state there.
fn play(dev: &mut dyn CoSim, clock: &mut Clock, end: f64, log: &mut EventLog<FwFrame>) -> FwState {
    loop {
        let next = dev.next();
        if next > end { break; }
        clock.0 = clock.0.max(next);
        dev.run(clock).expect("the device runs");
        log.drain();
    }
    clock.0 = end;
    let state = dev.state(end).expect("an attached device has a state");
    state.get::<FwState>().expect("the firmware's state").clone()
}

/// The client's counters: (got_a, last_a, got_b, last_b, long_b).
fn counters(state: &FwState) -> [i32; 5] {
    let debug = state.debug.as_ref().expect("the image's table");
    ["got_a", "last_a", "got_b", "last_b", "long_b"].map(|name| {
        let k = debug.vars.iter().position(|v| v.name == name).unwrap_or_else(|| panic!("the client's `{name}`"));
        match state.vars[k] { FwValue::I32(v) => v, ref other => panic!("`{name}` reads {other:?}") }
    })
}

fn conn(state: &FwState, c: u8) -> Option<NetConn> {
    state.net.as_ref().expect("the device has a network host end").conns.iter().find(|k| k.c == c).copied()
}

#[test]
fn the_twin_reaches_its_bench_fakes_and_the_network_its_run_owns_and_copies_replay_it() {
    let libs = ModelLibs::generic();
    let (circuit, mut bench) = schematic(false);
    let endpoint = |c, name: Option<&str>, url: Option<&str>| NetEndpoint {
        path: "mcu".into(), c, name: name.map(Into::into), real: url.map(|u| Real::Server(Server::parse(u).expect("a server address"))),
        fake: Fake::Echo, latency: 0.002, jitter: 0.0, rate: 0.0,
    };
    bench.nets = vec![endpoint(0, None, None), endpoint(1, Some("svc"), Some("wss://svc.example/v1"))];
    let lowered = lower_circuit(&circuit, &bench, 1, &libs);
    let binding = &lowered.firmware[0];
    assert_eq!(binding.net.len(), 2, "the bench's connections reach the binding: {:?}", lowered.diags);
    let input = FwInput {
        part: binding.part.clone(), doc: binding.doc.clone(), rev: 1, text: NET_CLIENT.into(),
        f_clk: binding.f_clk.expect("the crystal supplies the firmware clock"), params: binding.params.clone(),
        chip: Chip::S3, wiring: Some(wiring(&binding.bus.board(0)).expect("the circuit's wiring resolves")),
        island: None, files: Vec::new(), net_files: Vec::new(),
    };
    let built = compile(&input).expect("the network client builds");
    let asked = Arc::new(Mutex::new(Vec::new()));
    let opened = Arc::new(AtomicU64::new(0));
    let open: OpenNetwork = {
        let (asked, opened) = (asked.clone(), opened.clone());
        Arc::new(move |endpoints: &[NetEndpoint]| {
            opened.fetch_add(1, Ordering::Relaxed);
            let real = endpoints.iter().filter(|e| e.real.is_some()).map(|e| e.c).collect();
            Some((real, Box::new(Played { asked: asked.clone(), ..Played::default() }) as Upstream))
        })
    };
    let (mut log, feed) = EventLog::<FwFrame>::new(3.0, |frame| {
        if frame.is_image() { EventSource::Image } else { EventSource::Trace }
    }, Arc::new(|| {}));
    let (make, _) = maker(&input, built, feed, false, Arc::new(|_| {}), Some(open));
    assert!(make(&lowered, 1).is_some(), "an ensemble member forms a device");
    assert_eq!(opened.load(Ordering::Relaxed), 0, "an ensemble member plays the fakes and opens no network");
    let mut owner = make(&lowered, 0).expect("the S3 with network ports forms a device").dev;
    assert_eq!(opened.load(Ordering::Relaxed), 1, "the nominal run opens the network once, from its reset");

    // Two copies of the run from its first millisecond: one without the network, one that takes it
    // over once the owner is done.
    let mut clock = Clock(0.0);
    let early = play(&mut *owner, &mut clock, 1e-3, &mut log);
    assert!(early.net.as_ref().is_some_and(|n| n.owner && n.up == 0), "the owner holds the network before any traffic: {:?}", early.net);
    let (mut copy, mut heir) = (owner.fork().expect("the S3 forks"), owner.fork().expect("the S3 forks"));
    const END: f64 = 0.25;
    const ON: f64 = END + 0.05;
    let start = Instant::now();
    let end = play(&mut *owner, &mut clock, END, &mut log);
    eprintln!("the owner's run to {END} s took {:?} of wall time", start.elapsed());
    let net = end.net.clone().expect("the host end's account");
    let [got_a, last_a, got_b, last_b, long_b] = counters(&end);
    assert!(got_a >= 2 && got_b >= 2, "both connections answered more than once: {:?}", counters(&end));
    assert_eq!((last_a, last_b), (112 + 256 * 4, 103 + 256 * 4),
        "the bench's fake echoes \"ping\"; the run's network answers \"gnip\", through the same host end");
    assert_eq!(long_b as usize, LONG, "a reply three times the connection's slots streams to the device as it reads");
    assert!(net.bytes_down >= LONG as u64, "the host end sent it all down: {net:?}");
    assert_eq!((conn(&end, 0).map(|k| k.state), conn(&end, 1).map(|k| k.state)), (Some(READY), Some(READY)), "both connections are ready");
    assert_eq!((net.owner, net.refused, net.cuts, net.link.map(|l| l.0)), (true, 0, 0, Some(fab_elec_mcu_rt::net::LINK_UP)),
        "the owner's host end refused nothing and never cut its log");
    let reached = asked.lock().unwrap().clone();
    assert!(matches!(reached.first(), Some(Request::Open { c: 1, .. })), "the network is opened at the firmware's request: {reached:?}");
    assert!(reached[1..].iter().all(|r| matches!(r, Request::Send { c: 1, flags, bytes, .. }
        if *flags == PART_BINARY | PART_FIRST | PART_LAST && bytes == b"ping")), "then only connection 1's pings reach it: {reached:?}");
    assert!(reached.len() > got_b as usize, "every answer the client read was asked for");

    // A copy without the network replays what it answered, exactly, and reaches it never.
    let replayed = play(&mut *copy, &mut clock_at(1e-3), END, &mut log);
    assert_eq!(counters(&replayed), counters(&end), "the copy's run is the owner's, its answers replayed");
    assert_eq!(conn(&replayed, 1), conn(&end, 1), "the replayed connection stands as the owner's");
    assert!(!replayed.net.as_ref().unwrap().owner, "a fork holds no network");
    let past = play(&mut *copy, &mut clock_at(END), ON, &mut log);
    assert_eq!(conn(&past, 1).map(|k| (k.state, k.code)), Some((OFFLINE, code::RESET)),
        "past the owner's time the copy's real connection reads as reset");
    assert!(counters(&past)[0] > got_a && conn(&past, 0).map(|k| k.state) == Some(READY), "its fake connection plays on");
    assert_eq!(asked.lock().unwrap().len(), reached.len(), "the copy asked the network nothing");

    // A copy that takes the run's outputs over holds the network: it replays the owner's past, then
    // is live from the end of it.
    heir.put_outputs(owner.take_outputs().expect("the owner hands over its outputs"));
    let caught_up = play(&mut *heir, &mut clock_at(1e-3), END, &mut log);
    assert_eq!(counters(&caught_up), counters(&end), "the heir replays the owner's run");
    assert_eq!(asked.lock().unwrap().len(), reached.len(), "and asks the network nothing while it replays");
    let live = play(&mut *heir, &mut clock_at(END), ON, &mut log);
    let later = asked.lock().unwrap()[reached.len()..].to_vec();
    assert!(!later.is_empty() && later.iter().all(|r| matches!(r, Request::Send { c: 1, .. })),
        "past the owner's end its pings go out on the open connection, with no new open: {later:?}");
    assert!(counters(&live)[2] > got_b && conn(&live, 1).map(|k| k.state) == Some(READY), "and their answers come back");
    assert_eq!(live.net.as_ref().map(|n| (n.owner, n.cuts)), Some((true, 0)), "the heir owns the network and never ran differently");
}

fn clock_at(t: f64) -> Clock { Clock(t) }

#[path = "support/png.rs"]
mod png;
use png::png;

#[test]
fn the_files_fake_serves_a_folders_pictures_fitted_from_the_row_asked() {
    use fab_elec_fw::{net_world, pictures::{Checker, Pictures}, NetFakes};
    use fab_elec_mcu_rt::net::Service;
    let files: Vec<(String, Arc<[u8]>)> = vec![
        ("wide.png".into(), png(2, 1, &[[255, 0, 0], [0, 0, 255]]).into()),
        ("dot.png".into(), png(1, 1, &[[0, 255, 0]]).into()),
        ("notes.txt".into(), b"not a picture".to_vec().into()),
    ];
    let pictures = Pictures::new(&files);
    let said = |answer: Vec<(bool, fab_elec_mcu_rt::net::Payload)>| answer.into_iter().map(|(binary, bytes)| (binary, bytes.to_vec())).collect::<Vec<_>>();
    let ask = |service: &Pictures, line: &str| said(service.answer(false, line.as_bytes()));
    let text = |line: &str| vec![(false, line.as_bytes().to_vec())];
    assert_eq!(said(pictures.ready()), text("count 2"), "the folder's pictures, the other files left out");

    let whole = ask(&pictures, "get 3 4 2 0");
    let [(true, whole)] = &whole[..] else { panic!("one binary answer: {whole:?}") };
    let header = |index: u16, first: u16, rows: u16| [&[b'P', 1, 1, 0][..], &[index, 4, 2, first, rows, 0].map(u16::to_le_bytes).concat()[..]].concat();
    assert_eq!(whole[..16], header(1, 0, 2), "index 3 of 2 is the second by name, 4x2 from row 0");
    assert_eq!(whole.len(), 16 + 4 * 2 * 2, "two rows of four RGB565 pixels");
    assert_eq!((&whole[16..18], &whole[16 + 14..]), (&[0xf8, 0][..], &[0, 0x1f][..]),
        "fitted 2x1 into 4x2: red at the top left, blue at the bottom right, high byte first");
    let tail = ask(&pictures, "get 1 4 2 1");
    assert_eq!(tail, [(true, [&header(1, 1, 1)[..], &whole[16 + 8..]].concat())], "from the row asked on");
    assert_eq!(ask(&pictures, "get 1 4 2 9"), [(true, header(1, 2, 0))], "past the last row: no rows");
    assert_eq!(ask(&pictures, "get 1 0 2 0"), text("none 1"), "nothing fits in no width");
    for skipped in ["get x 4 2 0", "get 1 4 2", "put 1 4 2 0", "get 1 4 2 0 7"] {
        assert!(ask(&pictures, skipped).is_empty(), "{skipped:?} is skipped");
    }
    assert!(said(pictures.answer(true, b"get 1 4 2 0")).is_empty(), "a binary message is skipped");
    // The table's checker takes them, and refuses what breaks the table.
    let mut check = Checker::default();
    check.ready();
    for (up, binary, bytes) in [(false, false, &b"count 2"[..]), (true, false, b"get 3 4 2 0"), (false, true, whole), (false, true, &tail[0].1),
        (false, false, b"none 1")] {
        check.check(up, binary, bytes).unwrap_or_else(|why| panic!("the service's answers follow the table: {why}"));
    }
    let past = [&header(1, 1, 2)[..], &whole[16..]].concat();
    let beyond = [&header(2, 0, 2)[..], &whole[16..]].concat();
    for (up, binary, bytes, broken) in [
        (false, true, &whole[..whole.len() - 1], "a length the header does not say"), (false, true, &past[..], "rows past the height"),
        (false, true, &beyond[..], "an index not below the count"), (true, true, &whole[..], "a binary message up"),
        (false, false, b"put 1", "a verb not in the table"), (true, false, b"get 1 4 2", "too few arguments"), (false, false, b"count -1", "a signed count"),
    ] {
        assert!(check.check(up, binary, bytes).is_err(), "the checker refuses {broken}");
    }
    check.ready();
    assert!(check.check(false, true, whole).is_err(), "a picture before this connection's count");
    let empty = Pictures::new(&[]);
    assert_eq!((said(empty.ready()), ask(&empty, "get 0 4 2 0")), (text("count 0"), text("none 0")), "an empty folder");

    // Through the bench's fake: the world serves it on connection 0, and a folder the run was not
    // given serves no pictures on connection 1.
    let endpoint = |c, folder: &str| NetEndpoint {
        path: "mcu".into(), c, name: None, real: None, fake: Fake::Files(folder.into()), latency: 0.001, jitter: 0.0, rate: 0.0,
    };
    let fakes = NetFakes::new(&[("photos".to_string(), files.clone())]);
    let world = net_world(5, 2, &[endpoint(0, "photos"), endpoint(1, "elsewhere")], &[], &fakes).expect("the fakes play");
    let got: Vec<(u8, Vec<u8>)> = serve(world, 2, b"get 3 4 2 0").into_iter().map(|(c, _, bytes)| (c, bytes)).collect();
    assert_eq!(got, [(0, b"count 2".to_vec()), (1, b"count 0".to_vec()), (0, whole.clone())],
        "count at ready on each connection, then the picture asked, whole and the same");
}

/// The host end of `world` with connections `0..conns` of four slots each, opened at 0 by a device
/// that reads every part at once and sends each of `says` (time, connection, binary, bytes) whole
/// at its time: every whole message it takes, (time, connection, binary, bytes).
fn exchange(world: fab_elec_mcu_rt::net::World, conns: u8, says: &[(u64, u8, bool, Vec<u8>)]) -> Vec<(u64, u8, bool, Vec<u8>)> {
    conversation(world, conns, says).into_iter().filter(|m| !m.1).map(|(t, _, c, binary, bytes)| (t, c, binary, bytes)).collect()
}

/// [`exchange`]'s messages both ways in the order they crossed: (time, up, connection, binary, bytes).
fn conversation(world: fab_elec_mcu_rt::net::World, conns: u8, says: &[(u64, u8, bool, Vec<u8>)]) -> Vec<(u64, bool, u8, bool, Vec<u8>)> {
    use fab_elec_mcu_rt::net::{Host, NetMsg};
    let mut host = Host::new(world, &vec![4; usize::from(conns)]);
    host.hello(0);
    for c in 0..conns { host.from_device(0, &NetMsg::Open { c }).unwrap(); }
    let mut got: Vec<(u64, bool, u8, bool, Vec<u8>)> = Vec::new();
    let mut says = says.iter().peekable();
    loop {
        let say = says.peek().map(|s| s.0);
        let Some(now) = [host.next_due(), say].into_iter().flatten().min() else { return got };
        while let Some((t, msg)) = host.poll(now) {
            if let NetMsg::Data { c, flags, bytes } = msg {
                host.from_device(t, &NetMsg::Freed { c, slots: 1 }).unwrap();
                if flags & PART_FIRST != 0 { got.push((t, false, c, flags & PART_BINARY != 0, Vec::new())); }
                got.iter_mut().rev().find(|m| !m.1 && m.2 == c).unwrap().4.extend_from_slice(&bytes);
            }
        }
        if say == Some(now) {
            let (_, c, binary, bytes) = says.next().unwrap();
            let flags = PART_FIRST | PART_LAST | if *binary { PART_BINARY } else { 0 };
            host.from_device(now, &NetMsg::Data { c: *c, flags, bytes: bytes.clone() }).unwrap();
            got.push((now, true, *c, *binary, bytes.clone()));
        }
    }
}

/// [`exchange`] with one text, on connection 0 once it is ready: (connection, binary, bytes).
fn serve(world: fab_elec_mcu_rt::net::World, conns: u8, ask: &[u8]) -> Vec<(u8, bool, Vec<u8>)> {
    exchange(world, conns, &[(10_000_000, 0, false, ask.to_vec())]).into_iter().map(|(_, c, binary, bytes)| (c, binary, bytes)).collect()
}

/// A mono 16-bit WAV of `samples` at `rate` Hz.
fn wav(rate: u32, samples: &[i16]) -> Vec<u8> {
    let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let mut b = b"RIFF".to_vec();
    b.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    for (word, size) in [(16u32, 4), (1, 2), (1, 2), (rate, 4), (rate * 2, 4), (2, 2), (16, 2)] { b.extend_from_slice(&word.to_le_bytes()[..size]); }
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(data.len() as u32).to_le_bytes());
    b.extend_from_slice(&data);
    b
}

#[test]
fn a_rules_fake_sends_its_files_and_counts_a_wav_in_samples() {
    use fab_elec_bench::net::{template, Body, Field, RuleSpec, SendSpec, When};
    use fab_elec_fw::{net_world, NetFakes};
    let reply = wav(16_000, &(0..800).map(|i| (i * 37) as i16).collect::<Vec<_>>());
    let folder: Vec<(String, Arc<[u8]>)> = vec![("reply.wav".into(), reply.clone().into()), ("hello.txt".into(), b"hello there".to_vec().into())];
    let fakes = NetFakes::new(&[("voice".to_string(), folder)]);
    let send = |binary, head: Vec<Field>, body: Body, block| SendSpec { after: 0.0, binary, head, body, block, every: 0.02 };
    let rules = |file: &str| Fake::Rules(vec![RuleSpec { when: When::Message { binary: false, bytes: b"end ".to_vec(), whole: false }, stop: false, send: vec![
        send(false, Vec::new(), Body::File(file.into()), 0),
        send(true, vec![Field::Word16(1), Field::Offset32], Body::Wav { file: "voice/reply.wav".into(), rate: 16_000 }, 320),
        send(false, template("done {1} {sent}").unwrap(), Body::None, 0),
    ] }]);
    let endpoint = |fake| NetEndpoint { path: "mcu".into(), c: 0, name: None, real: None, fake, latency: 0.001, jitter: 0.0, rate: 0.0 };
    let got = serve(net_world(1, 1, &[endpoint(rules("voice/hello.txt"))], &[], &fakes).expect("the fakes play"), 1, b"end 7 0");
    let pcm = fab_elec_mcu::convert::sound(&reply, &fab_elec_mcu::convert::SoundOptions { rate: 16_000, codec: fab_elec_mcu::convert::Codec::Pcm16 }).unwrap();
    let block = |k: u32, samples: std::ops::Range<usize>| (0, true, [&7u16.to_le_bytes()[..], &(k * 320).to_le_bytes()[..], &pcm.bytes[samples.start * 2..samples.end * 2]].concat());
    assert_eq!(got, [(0, false, b"hello there".to_vec()), block(0, 0..320), block(1, 320..640), block(2, 640..800), (0, false, b"done 7 800".to_vec())],
        "the text file as it is; the WAV as the build's own 16-bit samples, in blocks of 320 with their offsets; the samples sent, not the text's bytes");
    let missing = net_world(1, 1, &[endpoint(rules("voice/gone.txt"))], &[], &fakes).map(|_| ()).unwrap_err();
    assert_eq!(missing, "connection 0's fake: firmware/voice/gone.txt is not in the project");
}

/// The handheld's two fakes as a bench writes them (pictures on 0, voice on 1), the snippet handed
/// to its firmware's lane.
const HANDHELD_NET: &str = r#"
    b.net(c.u, 0, {endpoint: "pictures" fake: {files: "photos"}})
    b.net(c.u, 1, {endpoint: "voice" fake: {rules: [
        {on: "hello " send: ["status ready"]}
        {on: "take " send: ["status hearing"]}
        {on: "end " send: [
            "status thinking",
            {after: 400ms text: "heard what is the weather like"},
            {after: 600ms text: "say It is sunny and mild today."},
            "status speaking",
            {binary: ["A" 0 "{1:u16}{offset:u32}"] wav: "voice/reply.wav" rate: 16000 block: 320 every: 20ms},
            "done {1} {sent}",
            "status idle",
        ]}
        {on: "cancel " stop: true send: ["done {1} {sent}", "status idle"]}
    ]}})
"#;

#[test]
fn a_bench_of_pictures_and_a_voice_turn_follows_the_agreed_tables_the_same_every_run() {
    use fab_elec_fw::{net_world, pictures::Checker, NetFakes};
    use fab_elec_lang::{evaluate, std_libs, Sources};
    const MS: u64 = 1_000_000;
    let mut sources = Sources::new();
    sources.add("radio.ckt.splash", r#"
use mod.elec.*
use mod.std.*
let e = mod.elec
e.define("S3_T", {
    kind: @mcu
    fields: {model: "ESP32-S3"}
    symbol: ic_symbol("S3_T", {left: [["VDD" @power_in] ["GND" @power_in] ["IO1" @bidi]]})
})
let gnd = e.net({name: "GND" kind: @ground})
let vdd = e.net({name: "VDD" kind: @power volts: 3.3V})
let u = e.part("S3_T", {pins: {VDD: vdd GND: gnd}})
"#).expect("the circuit is accepted");
    sources.add("radio.fw.splash", "firmware text is not evaluated by the circuit language").expect("the firmware is accepted");
    sources.add("net.bench.splash", format!("bench(\"net\", {{circuit: \"radio\"}}, fn(b, c) {{\n    b.firmware(c.u, \"radio.fw.splash\", {{}})\n{HANDHELD_NET}}})\n"))
        .expect("the bench is accepted");
    let ev = evaluate(&sources, &std_libs()).expect("the circuit evaluates");
    let bench = ev.benches.iter().find(|b| b.name == "net").expect("the bench has a result");
    assert!(bench.state.diags().is_empty(), "{:?}", bench.state.diags());

    let speech: Vec<i16> = (0..3200).map(|i| ((i * 13) % 2000 - 1000) as i16).collect();
    let reply = wav(16_000, &speech);
    let fakes = NetFakes::new(&[
        ("photos".to_string(), vec![("a.png".to_string(), Arc::<[u8]>::from(png(2, 2, &[[9, 9, 9]; 4]))), ("b.png".into(), png(3, 1, &[[200, 0, 0]; 3]).into())]),
        ("voice".to_string(), vec![("reply.wav".to_string(), Arc::<[u8]>::from(reply.clone()))]),
    ]);
    let take = |turn: u16, offset: u32| [&b"M\0"[..], &turn.to_le_bytes()[..], &offset.to_le_bytes()[..], &[0; 640][..]].concat();
    let text = |t: u64, c: u8, line: &str| (t * MS, c, false, line.as_bytes().to_vec());
    let says = [
        text(10, 0, "get 0 8 4 0"), text(10, 1, "hello 1 16000"), text(20, 0, "get 5 8 4 2"), text(20, 1, "take 7"),
        (30 * MS, 1, true, take(7, 0)), (40 * MS, 1, true, take(7, 320)), text(50, 1, "end 7 640"),
        text(2000, 1, "take 8"), text(2010, 1, "end 8 0"), text(3060, 1, "cancel 8"), text(4000, 1, "cancel 8"),
    ];
    let world = || net_world(3, 2, &bench.spec.nets, &[], &fakes).expect("the fakes play");
    let both_ways = conversation(world(), 2, &says);
    assert_eq!(conversation(world(), 2, &says), both_ways, "a second run gives the same bytes at the same times");
    let got = exchange(world(), 2, &says);

    // pictures: every message both ways, in arrival order, through the table's checker
    let mut both: Vec<(u64, bool, bool, &[u8])> = got.iter().filter(|m| m.1 == 0).map(|m| (m.0, false, m.2, &m.3[..])).collect();
    both.extend(says.iter().filter(|s| s.1 == 0).map(|s| (s.0, true, s.2, &s.3[..])));
    both.sort_by_key(|m| m.0);
    let mut check = Checker::default();
    check.ready();
    for (t, up, binary, bytes) in &both {
        check.check(*up, *binary, bytes).unwrap_or_else(|why| panic!("pictures at {t} ns: {why}"));
    }
    let pictures: Vec<_> = got.iter().filter(|m| m.1 == 0 && m.2).map(|m| (u16::from_le_bytes([m.3[4], m.3[5]]), u16::from_le_bytes([m.3[10], m.3[11]]))).collect();
    assert_eq!(pictures, [(0, 0), (1, 2)], "picture 0 whole, then 5 modulo 2 from row 2");

    // voice: the turn's order, its speech whole in contiguous blocks, `done` with the samples sent
    let voice: Vec<&(u64, u8, bool, Vec<u8>)> = got.iter().filter(|m| m.1 == 1).collect();
    let texts: Vec<String> = voice.iter().filter(|m| !m.2).map(|m| String::from_utf8(m.3.clone()).unwrap()).collect();
    let blocks = |turn: u16| -> Vec<(usize, u32)> {
        voice.iter().enumerate().filter(|(_, m)| m.2 && m.3[..4] == [b'A', 0, turn as u8, (turn >> 8) as u8]).map(|(k, m)| {
            let offset = u32::from_le_bytes(m.3[4..8].try_into().unwrap());
            assert_eq!(m.3.len(), 8 + 640, "a block of 320 samples");
            (k, offset)
        }).collect()
    };
    let (seven, eight) = (blocks(7), blocks(8));
    assert_eq!(seven.iter().map(|b| b.1).collect::<Vec<_>>(), (0..10).map(|k| k * 320).collect::<Vec<_>>(), "the whole reply, offsets in samples");
    let timeline: Vec<(u64, String)> = voice.iter().map(|m| (m.0 / MS, if m.2 { format!("block {}", m.3.len()) } else { String::from_utf8_lossy(&m.3).into_owned() })).collect();
    assert!((1..10).contains(&eight.len()) && eight.iter().enumerate().all(|(k, b)| b.1 == k as u32 * 320), "the cancel came mid-speech: {eight:?} {timeline:?}");
    let pcm = fab_elec_mcu::convert::sound(&reply, &fab_elec_mcu::convert::SoundOptions { rate: 16_000, codec: fab_elec_mcu::convert::Codec::Pcm16 }).unwrap();
    assert!(seven.iter().all(|&(k, offset)| voice[k].3[8..] == pcm.bytes[offset as usize * 2..][..640]), "each block carries its part of the reply");
    let cancelled = format!("done 8 {}", 320 * eight.len());
    let turn = ["status thinking", "heard what is the weather like", "say It is sunny and mild today.", "status speaking"];
    let want: Vec<&str> = ["status ready", "status hearing"].into_iter().chain(turn).chain(["done 7 3200", "status idle", "status hearing"]).chain(turn)
        .chain([cancelled.as_str(), "status idle"]).collect();
    assert_eq!(texts, want, "hello, take, end and cancel as the table orders them; a cancel after the turn ended answers nothing");
    let at = |line: &str| voice.iter().position(|m| m.3 == line.as_bytes()).unwrap();
    assert!(seven.iter().all(|b| b.0 > at("status speaking") && b.0 < at("done 7 3200")), "turn 7's speech lies between speaking and done");
    assert!(eight.iter().all(|b| b.0 < at(&cancelled)), "nothing of turn 8 follows its done");

    // voice: every message both ways, in the order they crossed, through the spoken turn's checker
    // (the hub's, beside its `/talk` route)
    let mut talk = makepad_ai_hub::talk::check::Checker::default();
    talk.ready();
    for (k, (t, up, _, binary, bytes)) in both_ways.iter().filter(|m| m.2 == 1).enumerate() {
        let checked = if *up { talk.up(*binary, bytes) } else { talk.down(*binary, bytes) };
        checked.unwrap_or_else(|why| panic!("voice message {k} at {t} ns ({}): {why}", if *up { "up" } else { "down" }));
    }
}
