//! Host board commands reach firmware and replay through an edit behind the live front.

use fab_elec_bench::bench::Supply;
use fab_elec_bench::{lower_circuit, BenchSpec, BoardInput, Firmware, ModelLibs, SimCmd, Touches};
use fab_elec_circuit::{Circuit, Component, Net, NetKind, QtyUnit, Quantity, Value};
use fab_elec_core::ids::BENCH_SCOPE_BASE;
use fab_elec_core::{BlockId, BoardId, ClassId, CompId, CompRef, NetId, NetRef, PinIx};
use fab_elec_fw::{compile, maker, sound_track, Chip, FwInput};
use fab_elec_mcu_cosim::FwFrame;
use fab_elec_mcu_emit_s3::wiring;
use fab_elec_mcu_parts::screen::{ScreenFrame, ScreenReader};
use fab_elec_parts::{Fields, GpioCard, PartDef, PartKind, PinDef, PinKind, Symbol};
use fab_elec_sim::live::LiveCmd;
use fab_elec_sim::livesim::{LiveSim, LiveSpec, Snapshots};
use fab_elec_sim::Sink;
use fab_elec_design::Design;
use fab_elec_lang::BenchScope;
use fab_elec_simhost::events::{EventLog, EventSource};
use fab_elec_simhost::{SimHost, Spawn};
use fab_elec_view::live::sim::SimLive;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[path = "../../simhost/tests/support/ears.rs"]
mod ears;
use ears::{ready, Ears, EAR_GAIN, SIDE, WINDOW};

const GLASS: [usize; 4] = [16, 12, 8, 8];
const RED: u32 = 0xffff_0000;
const BLUE: u32 = 0xff00_00ff;
const END: f64 = 3.0;

#[test]
fn wav_samples_fill_a_closed_track_at_the_declared_rate_and_level() {
    let rate = 16_000_u32;
    let samples = [i16::MIN, -2000, 0, 1000, 32766];
    let data_bytes = samples.len() as u32 * 2;
    let mut wav = Vec::from(*b"RIFF");
    wav.extend((36 + data_bytes).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16_u32.to_le_bytes()); // PCM format chunk length
    wav.extend(1_u16.to_le_bytes()); // PCM
    wav.extend(1_u16.to_le_bytes()); // mono
    wav.extend(rate.to_le_bytes());
    wav.extend((rate * 2).to_le_bytes());
    wav.extend(2_u16.to_le_bytes()); // bytes per frame
    wav.extend(16_u16.to_le_bytes()); // bits per sample
    wav.extend(b"data");
    wav.extend(data_bytes.to_le_bytes());
    for sample in samples { wav.extend(sample.to_le_bytes()); }

    let origin_ps = 125_000_000_000;
    for (db, expected) in [
        (0.0, samples),
        (-6.0206, [-16384, -1000, 0, 500, 16383]),
        (6.0206, [i16::MIN, -4000, 0, 2000, i16::MAX]),
    ] {
        let track = sound_track(&wav, rate, db, origin_ps).unwrap();
        assert!(track.is_closed());
        assert_eq!(track.len(), expected.len() as u64);
        assert_eq!(track.period_s(), 1.0 / f64::from(rate));
        let last_s = origin_ps as f64 * 1e-12 + (expected.len() - 1) as f64 / f64::from(rate);
        assert!((track.horizon_s() - last_s).abs() < 1e-12);
        for (i, sample) in expected.into_iter().enumerate() {
            assert_eq!(track.sample(i as u64), Some(sample), "sample {i} at {db} dB");
        }
    }

    let resampled = sound_track(&wav, 48_000, 0.0, origin_ps).unwrap();
    assert_eq!(resampled.period_s(), 1.0 / 48_000.0);
    assert_eq!(resampled.len(), 3 * samples.len() as u64);
    assert_eq!(resampled.sample(3), Some(samples[1]));
}

const SOURCE: &str = r#"
let screen = display()
let led = output(33)
let finger = touch(10000, fn() {
    if touch_down() != 0 {
        fill_rect(screen, 0, 0, display_w, display_h, rgb(0, 0, 255))
    }
})
let imu = motion(10000, fn() {
    if accel(0) > ACCEL_LSB_PER_G / 2 { write(led, high) }
})
fn setup() {
    write(led, low)
    fill_rect(screen, 0, 0, display_w, display_h, rgb(255, 0, 0))
}
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

fn schematic() -> (Circuit, BenchSpec) {
    let mut mcu = component(0, "mcu", "mcu", &[
        ("GPIO15", 2), ("GPIO14", 3), ("GPIO12", 4), ("GPIO11", 5),
        ("GPIO4", 6), ("GPIO5", 7), ("GPIO6", 8), ("GPIO7", 9),
        ("GPIO21", 13), ("GPIO33", 16),
        ("VDD", 1), ("GND", 0), ("XTAL1", 11), ("XTAL2", 12),
    ]);
    let def = Arc::make_mut(&mut mcu.part);
    def.kind = PartKind::Mcu;
    def.fields.0.push(("model".into(), "ESP32-S3".into()));
    def.gpio = Some(card());
    let mut panel = component(1, "panel", "bus", &[
        ("CSX", 4), ("SCLK", 5), ("SIO0", 6), ("SIO1", 7), ("SIO2", 8), ("SIO3", 9),
        ("RESX", 10), ("VDD", 1), ("GND", 0),
    ]);
    let def = Arc::make_mut(&mut panel.part);
    def.fields.0.push(("model".into(), "CO5300".into()));
    for (key, n) in ["bus.glass_x", "bus.glass_y", "bus.glass_w", "bus.glass_h"].into_iter().zip(GLASS) {
        def.fields.0.push((key.into(), n.to_string()));
    }
    def.gpio = Some(card());
    let mut touch = component(2, "touch", "bus", &[
        ("SDA", 2), ("SCL", 3), ("INT", 13), ("RST", 1), ("VDD", 1), ("GND", 0),
    ]);
    let def = Arc::make_mut(&mut touch.part);
    def.fields.0.extend([("model".into(), "CST820".into()), ("bus.addr".into(), "21".into())]);
    def.gpio = Some(card());
    let mut imu = component(3, "imu", "bus", &[
        ("SDA", 2), ("SCL", 3), ("INT1", 14), ("INT2", 15), ("VDD", 1), ("GND", 0),
    ]);
    let def = Arc::make_mut(&mut imu.part);
    def.fields.0.extend([("model".into(), "QMI8658C".into()), ("bus.addr".into(), "107".into())]);
    def.gpio = Some(card());
    let mut expander = component(4, "expander", "bus", &[
        ("SDA", 2), ("SCL", 3), ("P0", 10),
        ("P1", 1), ("P2", 1), ("P3", 1), ("P4", 1), ("P5", 1), ("P6", 1), ("P7", 1),
        ("INT", 1), ("VDD", 1), ("GND", 0),
    ]);
    let def = Arc::make_mut(&mut expander.part);
    def.fields.0.extend([("model".into(), "TCA9554".into()), ("bus.addr".into(), "32".into())]);
    def.gpio = Some(card());
    let mut parts = vec![mcu, panel, touch, imu, expander];
    for (path, a, b, ohms) in [
        ("pull_sda", 2, 1, 10e3), ("pull_scl", 3, 1, 10e3), ("pull_cs", 4, 1, 10e3),
        ("pull_touch", 13, 1, 10e3), ("led_r", 16, 17, 330.0),
    ] {
        let mut r = component(parts.len() as u32, path, "resistor", &[("1", a), ("2", b)]);
        Arc::make_mut(&mut r.part).kind = PartKind::Resistor;
        value(&mut r, ohms, QtyUnit::Ohm);
        parts.push(r);
    }
    let mut led = component(parts.len() as u32, "led", "led", &[("A", 17), ("K", 0)]);
    Arc::make_mut(&mut led.part).kind = PartKind::Led;
    led.value = Some(Value { text: "red".into(), qty: None, tol: None });
    parts.push(led);
    let mut crystal = component(parts.len() as u32, "crystal", "passive", &[("1", 11), ("2", 12)]);
    Arc::make_mut(&mut crystal.part).kind = PartKind::Crystal;
    value(&mut crystal, 40e6, QtyUnit::Hertz);
    parts.push(crystal);
    let circuit = Circuit {
        unit: BoardId(0), name: "board_inputs".into(), comps: parts,
        nets: ["gnd", "vdd", "sda", "scl", "panel_cs", "panel_clock", "panel_data0", "panel_data1",
            "panel_data2", "panel_data3", "panel_reset", "xtal1", "xtal2", "touch_irq",
            "imu_irq1", "imu_irq2", "led_drive", "led_anode"]
            .into_iter().enumerate().map(|(i, name)| Net {
                id: NetId(i as u32), name: name.into(), class: ClassId(0), volts: None, scope: None,
                kind: if i == 0 { NetKind::Ground } else if i == 1 { NetKind::Power } else { NetKind::Signal },
            }).collect(),
        ..Circuit::default()
    };
    let bench = BenchSpec {
        supplies: vec![Supply { net: NetRef { board: BoardId(0), net: NetId(1) }, v: 3.3, limit: None, ramp: None }],
        firmware: vec![Firmware { path: "mcu".into(), doc: "board.fw.splash".into(), params: Vec::new() }],
        ..BenchSpec::default()
    };
    (circuit, bench)
}

struct Discard;
impl Sink for Discard {}

fn has_colour(frame: &ScreenFrame, colour: u32) -> bool {
    let [x, y, w, h] = GLASS;
    frame.generation != 0 && frame.on && frame.brightness == 0xff
        && (y..y + h).all(|row| frame.bgra[row * frame.width + x..row * frame.width + x + w]
            .iter().all(|&pixel| pixel == colour))
}

fn advance(sim: &mut LiveSim, log: &mut EventLog<FwFrame>, until: f64) {
    while sim.t() < until {
        // Drain after every loop top, including those at the same time: the nominal maker's
        // event feed must not hold the machine while the test waits for a panel publication.
        sim.advance(until, Some(Duration::ZERO), &mut Discard).expect("the live board circuit advances");
        log.drain();
    }
    sim.values(0.0);
    log.drain();
}

fn until_colour(sim: &mut LiveSim, log: &mut EventLog<FwFrame>, reader: &mut ScreenReader, colour: u32, phase: &str) {
    while sim.t() < END && !has_colour(reader.latest(), colour) {
        let until = (sim.t() + 1e-3).min(END);
        advance(sim, log, until);
    }
    assert!(has_colour(reader.latest(), colour), "{phase}: the complete panel colour {colour:#010x} must appear by {END} s");
    eprintln!("{phase}: panel turns {colour:#010x} at {:.6} simulated seconds", reader.latest().t);
}

#[test]
fn board_commands_draw_light_and_replay_the_same_panel_frame() {
    let start = Instant::now();
    let (circuit, bench) = schematic();
    let lowered = lower_circuit(&circuit, &bench, 1, &ModelLibs::generic());
    let binding = lowered.firmware.first().expect("the MCU has a firmware binding");
    assert_eq!(binding.bus.parts.len(), 4, "panel, touch, IMU and reset expander reach the bus: {:?}", lowered.diags);
    let input = FwInput {
        part: binding.part.clone(), doc: binding.doc.clone(), rev: 1, text: SOURCE.into(),
        f_clk: binding.f_clk.expect("the crystal supplies the firmware clock"), params: binding.params.clone(),
        chip: binding.chip.as_deref().and_then(Chip::from_model).expect("the model selects S3"),
        wiring: Some(wiring(&binding.bus.board(0)).expect("the circuit's board wiring resolves")),
        island: None,
        files: Vec::new(), net_files: Vec::new(),
    };
    let built = compile(&input).expect("the board input firmware builds");
    let (mut log, feed) = EventLog::<FwFrame>::new(END, |frame| {
        if frame.is_image() { EventSource::Image } else { EventSource::Trace }
    }, Arc::new(|| {}));
    let readers = Arc::new(Mutex::new(Vec::new()));
    let received = readers.clone();
    let (make, _) = maker(&input, built, feed, false, Arc::new(move |set| received.lock().unwrap().extend(set)), None);
    let device = make(&lowered, 0).expect("the nominal maker builds the board device");
    let panel = CompRef { board: circuit.unit, comp: CompId(1) };
    let imu = CompRef { board: circuit.unit, comp: CompId(3) };
    let (of, mut reader) = {
        let mut readers = readers.lock().unwrap();
        assert_eq!(readers.len(), 1, "the nominal maker hands out exactly one panel reader");
        readers.pop().expect("the panel reader stays outside the device")
    };
    assert_eq!(of, panel, "the maker's reader belongs to the commanded panel");
    assert_eq!(reader.glass(), GLASS, "host touch coordinates use the maker's glass mapping");
    let snapshots = Snapshots { span: END, every: Duration::ZERO, share: f64::INFINITY, ..Snapshots::default() };
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, max_step: 1e-3, res: 0.0, trace: false,
        snapshots, ..LiveSpec::default() };
    let mut sim = LiveSim::new(lowered.circuit.clone(), lowered.plan.clone(), lowered.index.generation, spec)
        .expect("the board circuit's operating point converges");
    sim.attach(Some(device));
    assert!(sim.apply(LiveCmd::Play), "the run plays so it can retain a snapshot");
    let anode = sim.solver().unknown("led_anode").expect("the LED anode has an analog node") as usize;
    let drive = sim.solver().unknown("led_drive").expect("the LED drive has an analog node") as usize;
    let lit = |sim: &LiveSim, ohms| sim.solver().x[anode] >= 1.0
        && (sim.solver().x[drive] - sim.solver().x[anode]) / ohms > 1e-3;
    let running = Instant::now();
    // Both the panel's initialization waits and the IMU's 1.75 s startup remain real.
    until_colour(&mut sim, &mut log, &mut reader, RED, "setup");
    assert!(!lit(&sim, 330.0), "default gravity on Z must not trigger the X-axis tilt handler");
    sim.keep_snapshot(&Discard);
    let saved_at = sim.t();
    assert_eq!(sim.stats().snapshots_kept, 1, "one snapshot is kept before the touch and tilt");

    let touch_at = saved_at + 10e-3;
    let mut touches = Touches::default();
    let touch = touches.command(&lowered.index, panel, 18, 15, true).expect("a down inside the glass becomes a board command");
    assert!(sim.apply(LiveCmd::At(touch_at, Box::new(touch))), "the touch is scheduled on the run's timeline");
    until_colour(&mut sim, &mut log, &mut reader, BLUE, "touch");
    assert_eq!(sim.stats().inputs, 1, "the colour change took exactly one host touch");
    assert!(!lit(&sim, 330.0), "the touch must not light the LED before the tilt");

    let tilt_at = sim.t() + 10e-3;
    let cmds = lowered.index.command(&SimCmd::Board(BoardInput::Motion {
        comp: imu, accel_ug: [1_000_000, 0, 0], gyro_mdps: [0; 3],
    }));
    assert_eq!(cmds.len(), 1, "the IMU pose maps to one host input");
    for cmd in cmds {
        assert!(sim.apply(LiveCmd::At(tilt_at, Box::new(cmd))), "the tilt is scheduled on the run's timeline");
    }
    while sim.t() < END && !lit(&sim, 330.0) {
        let until = (sim.t() + 1e-3).min(END);
        advance(&mut sim, &mut log, until);
    }
    assert!(lit(&sim, 330.0), "the X-axis tilt must light the LED through its resistor by {END} s");
    eprintln!("tilt: LED lights at {:.6} simulated seconds", sim.t());
    assert_eq!(sim.stats().inputs, 2, "both touch and tilt must be accepted by the device");
    // Record after the GPIO edge settles, so the resistor edit can change its analog tail.
    let until = (sim.t() + 20e-3).min(END);
    advance(&mut sim, &mut log, until);
    let t = sim.t();
    let frame = reader.latest().clone();
    assert!(has_colour(&frame, BLUE), "the recorded frame at T retains the touch colour");
    let forward_wall = running.elapsed();

    let resistor = CompRef { board: circuit.unit, comp: circuit.comp("led_r").expect("the LED has a series resistor").id };
    let edit_at = saved_at + 5e-3;
    let edit = lowered.index.command(&SimCmd::SetParam { comp: resistor, value: 680.0 });
    assert_eq!(edit.len(), 1, "the LED resistor has one tunable value");
    assert!(sim.apply(LiveCmd::Edit(edit.into_iter().map(|cmd| (edit_at, cmd)).collect())),
        "the resistor edit behind the front is accepted");
    assert_eq!(sim.stats().rewinds, 1, "the edit must rewind through a retained snapshot");
    assert_eq!(sim.stats().late_edits, 0, "the edit must not be applied late at the front");
    assert_eq!(sim.t(), saved_at, "the edit restores the snapshot before the touch");
    assert_eq!(sim.stats().inputs, 2, "rewinding preserves the session's accepted-input count");
    sim.values(0.0);
    log.drain();
    assert!(has_colour(reader.latest(), RED), "restoring the snapshot restores the panel before the touch");
    let replay = Instant::now();
    advance(&mut sim, &mut log, t);
    let replayed = reader.latest();
    assert_eq!(sim.stats().inputs, 4, "the timeline takes the same touch and tilt again after the edit");
    assert!(replayed.bgra == frame.bgra, "the replayed panel's entire BGRA memory at T must match bit for bit");
    assert_eq!(replayed.on, frame.on, "the replayed panel's on state at T must match");
    assert_eq!(replayed.brightness, frame.brightness, "the replayed panel's brightness at T must match");
    assert!(lit(&sim, 680.0), "the replayed tilt still drives LED current through the edited resistor");
    assert_eq!((sim.stats().stale, sim.stats().unknown), (0, 0), "both runs must accept every command's target and payload");
    eprintln!("board inputs: T = {t:.6} s, snapshot = {saved_at:.6} s; forward wall = {:?}, replay wall = {:?}, total wall = {:?}",
        forward_wall, replay.elapsed(), start.elapsed());
}

/// The heard board's firmware: a touch lights the LED, its runtime polling every 2 ms.
const TOUCH_SOURCE: &str = r#"
let led = output(33)
let finger = touch(2000, fn() {
    if touch_down() != 0 { write(led, high) }
})
fn setup() {
    write(led, low)
}
"#;

/// The test's threads: one std thread per worker and per job.
struct Threads;

impl Spawn for Threads {
    fn worker(&self, _name: &'static str, f: Box<dyn FnOnce() + Send>) {
        std::thread::spawn(f);
    }
    fn job(&self, f: Box<dyn FnOnce() + Send>) {
        std::thread::spawn(f);
    }
}

/// A touch while the recording plays is heard where it was made: the board above run as the app
/// runs it (the host, the board as a bench's own circuit, its firmware built and attached as the
/// app's glue attaches it), below real time (paced as a heavy board runs), the LED's drive heard
/// through the app's Listen tap and player. With the run well ahead of the play head, a touch goes
/// in at the time the listener hears: the run goes back there and renders on, and the recording
/// has the LED dark up to the touch and lit from the touch controller's next scan and the
/// runtime's next poll after it. A touch taken at the run's front would leave it dark there.
#[test]
fn a_touch_while_the_recording_plays_is_heard_where_it_was_made() {
    let (mut circuit, mut bench) = schematic();
    circuit.unit = BoardId(BENCH_SCOPE_BASE);
    bench.supplies[0].net.board = circuit.unit;
    let panel = CompRef { board: circuit.unit, comp: CompId(1) };
    let start = Instant::now();
    let mut host = SimHost::with_spawn(Box::new(Threads));
    host.set_live_spec(LiveSpec { rate: f64::INFINITY, ..LiveSpec::default() });
    host.set_bench(bench);
    host.set_scope(Some(BenchScope::Own { circuit: Arc::new(circuit), generation: 1 }));
    host.commit(&Arc::new(Design::default()));
    let mut live = SimLive::default();
    let mut ears = Ears::new(&mut host, &mut live, "led_drive", WINDOW);
    ears.until(&mut host, &mut live, start, "the board lowered", |h, _| h.lowered().is_some());

    // the firmware for the lowering, attached to the running run as the app's glue attaches it
    // while nothing of the recording plays
    let lowered = host.lowered().cloned().expect("the board is lowered");
    let binding = lowered.firmware.first().expect("the MCU has a firmware binding");
    let input = FwInput {
        part: binding.part.clone(), doc: binding.doc.clone(), rev: 1, text: TOUCH_SOURCE.into(),
        f_clk: binding.f_clk.expect("the crystal supplies the firmware clock"), params: binding.params.clone(),
        chip: binding.chip.as_deref().and_then(Chip::from_model).expect("the model selects S3"),
        wiring: Some(wiring(&binding.bus.board(0)).expect("the circuit's board wiring resolves")),
        island: None,
        files: Vec::new(), net_files: Vec::new(),
    };
    let built = compile(&input).expect("the touch firmware builds");
    let feed = host.set_events::<FwFrame>(|frame| if frame.is_image() { EventSource::Image } else { EventSource::Trace });
    let (make, _) = maker(&input, built, feed, false, Arc::new(|_: Vec<(CompRef, ScreenReader)>| {}), None);
    ears.until(&mut host, &mut live, start, "the run", |h, _| h.values().is_some());
    host.set_cosim(Some((make, 1)));
    let attached = host.values().map_or(0.0, |v| v.t);
    // the touch controller takes no access for its first 100 ms (T18): touches from after that
    let touchable = attached + 0.1;

    let lit = (3.3 * f64::from(EAR_GAIN) / 2.0) as f32;
    ears.until(&mut host, &mut live, start, "the recording plays past the controller's start, the run ahead of it", ready(touchable));
    let speed = ears.bus.ring.tap_speed();
    let rewinds = host.store().rewinds;
    let touched = ears.heard_at().expect("the recording plays");
    assert!(host.command(&SimCmd::Board(BoardInput::Touch { comp: panel, x: 18, y: 15, down: true })), "a touch on the glass is a command");
    let take = ears.around(&mut host, &mut live, start, touched, rewinds, "the run renders past the touch again");
    // sample i's period starts at the run's time (first + i) / rate
    let first = take.samples.iter().position(|&s| s > lit).map(|i| (take.first + i as u64) as f64 / take.rate);
    // the controller sees the finger at its next scan (10 ms), the runtime at its next poll (2 ms)
    let seen = 13e-3;
    assert!(first.is_some_and(|t| t >= touched && t <= touched + seen),
        "the LED lights in the recording from the touch's next scan and poll: the touch heard at {touched:.4} s, the LED lit at {first:?}");
    assert!(take.between(touched + seen, touched + SIDE).iter().all(|&s| s > lit), "the LED stays lit after the touch");
    assert!(host.command(&SimCmd::Stop), "stop the worker");
    eprintln!("heard touch: the run at {speed:.2}x real time; touched at {touched:.4} s, the LED lit at {first:?}; {:?}", start.elapsed());
}
