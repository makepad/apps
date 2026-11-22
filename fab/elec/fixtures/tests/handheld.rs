//! The stock handheld app on its lowered board, from Sand and speech to pictures on the glass.

use fab_elec_bench::{lower, FwBinding, ModelLibs, SimCmd};
use fab_elec_bench::bus::BusConn;
use fab_elec_core::{NetRef, Severity};
use fab_elec_fw::{compile, maker, Built, Chip, FwInput};
use fab_elec_fw::pictures::{Checker, Pictures};
use fab_elec_lang::{evaluate, std_libs, Sources, UnitState};
use fab_elec_mcu_cosim::{watch, FwDebug, FwFrame};
use fab_elec_mcu_emit_s3::{wiring, AmpEnable, Audio, Wiring, VOICEBOX, VOICEBOX_I2C, VOICEBOX_MOTION, VOICEBOX_PANEL, VOICEBOX_TOUCH};
use fab_elec_mcu_parts::screen::ScreenFrame;
use fab_elec_sim::live::LiveCmd;
use fab_elec_sim::livesim::{LiveSim, LiveSpec};
use fab_elec_sim::Sink;
use fab_elec_simhost::events::{EventLog, EventSource};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use fab_elec_mcu_cosim::{CoMachine, Config};
use fab_elec_mcu_emu::{board::Audio as SpeakerOutput, Lockstep, Machine, PcSet, Ran, Tier, Until};
use fab_elec_mcu_parts::{Netlist, Part, PartKind, SoundTrack, Stimulus, TrackRef};
use fab_elec_mcu_rt::net::{self, Host, NetMsg, Service};
use fab_elec_mcu_s3::S3Machine;
use fab_elec_sim::cosim::CoSimDevice;

#[path = "../../mcu/emit_s3/tests/support/net_link.rs"]
mod net_link;
use net_link::NetLink;
#[path = "../../fw/tests/support/png.rs"]
mod png;

const GLASS: [usize; 4] = [16, 0, 368, 448];
// Firmware RGB values after RGB565 quantization and the screen's bit replication.
const SAND: [u32; 3] = [0xffff_cf6b, 0xffe7_a642, 0xffc6_7d29];
const END: f64 = 2.0;

struct Discard;
impl Sink for Discard {}

fn firmware_input(sources: &Sources, binding: &FwBinding, pins: Wiring) -> FwInput {
    let doc = sources.find(&binding.doc).and_then(|id| sources.doc(id)).expect("the bench's firmware document");
    FwInput {
        part: binding.part.clone(), doc: binding.doc.clone(), rev: 1, text: doc.text.clone(),
        f_clk: binding.f_clk.expect("the main crystal supplies the firmware clock"),
        params: binding.params.clone(), chip: binding.chip.as_deref().and_then(Chip::from_model).expect("the bench's MCU"),
        wiring: Some(pins), island: None, files: Vec::new(), net_files: Vec::new(),
    }
}

fn glass_rows(frame: &ScreenFrame) -> impl Iterator<Item = &[u32]> {
    let [x, y, w, h] = GLASS;
    frame.bgra.chunks_exact(frame.width).skip(y).take(h).map(move |row| &row[x..x + w])
}

fn sand_pixels(frame: &ScreenFrame, first_row: usize) -> usize {
    if frame.generation == 0 || !frame.on || frame.brightness == 0 { return 0; }
    let [x, y, w, h] = GLASS;
    let mut count = 0;
    for row in y + first_row..y + h {
        for pixel in &frame.bgra[row * frame.width + x..row * frame.width + x + w] {
            if SAND.contains(pixel) { count += 1; }
            // Power-on panel memory is junk: a few coincidental palette pixels
            // must not pass before setup has cleared the whole glass.
            else if *pixel != 0xff00_0000 { return 0; }
        }
    }
    count
}

fn advance(sim: &mut LiveSim, log: &mut EventLog<FwFrame>, until: f64) {
    log.drain();
    sim.advance((sim.t() + 1e-3).min(until), Some(Duration::from_millis(50)), &mut Discard)
        .unwrap_or_else(|d| panic!("the handheld live circuit advances: {d}"));
    // values() synchronizes the machine and publishes committed screen memory.
    sim.values(0.0);
    log.drain();
}

#[test]
fn handheld_documents_compile_and_sand_falls_on_the_live_screen() {
    let sources = fab_elec_fixtures::docs("handheld").expect("handheld is a registered stock project");
    let evaluated = evaluate(&sources, &std_libs())
        .unwrap_or_else(|diags| panic!("handheld documents evaluate: {diags:?}"));
    let errors: Vec<_> = evaluated.notes.iter().filter(|d| d.severity == Severity::Error).collect();
    assert!(errors.is_empty(), "handheld evaluates without errors: {errors:?}");
    let unit = evaluated.units.iter().find(|u| u.name == "handheld").expect("handheld has its circuit unit");
    assert_eq!(unit.state, UnitState::Built, "handheld circuit builds: {:?}", unit.state.diags());
    let unit = evaluated.design.units.iter().find(|u| u.name == "handheld").expect("handheld lowers its unit");
    let pin_net = |path: &str, pin: &str| {
        let comp = unit.circuit.comps.iter().find(|c| c.path == path)
            .unwrap_or_else(|| panic!("the circuit has {path}"));
        let ix = comp.part.pins.iter().position(|p| p.name == pin)
            .unwrap_or_else(|| panic!("{path} has pin {pin}"));
        comp.conns.iter().find(|(p, _)| p.0 as usize == ix).map(|(_, net)| *net)
            .unwrap_or_else(|| panic!("{path}.{pin} is connected"))
    };
    assert_eq!(pin_net("mcu", "GPIO20"), pin_net("usb", "DP"), "USB D+ reaches GPIO20 (USB_D+)");
    assert_eq!(pin_net("mcu", "GPIO19"), pin_net("usb", "DN"), "USB D- reaches GPIO19 (USB_D-)");
    assert_eq!(pin_net("boot_button", "A"), pin_net("mcu", "GPIO0"), "BOOT pulls the GPIO0 strap");
    assert_eq!(pin_net("reset_button", "A"), pin_net("mcu", "CHIP_PU"), "RESET pulls CHIP_PU");
    assert_eq!(pin_net("ldo", "VIN"), pin_net("usb", "VBUS"), "the regulator runs from USB VBUS");
    assert_eq!(pin_net("ldo", "VOUT"), pin_net("mcu", "VDD3P3"), "the regulator feeds the 3V3 rail");
    let board = unit.board.as_ref().expect("handheld has its board");
    assert_eq!(board.placed.len(), unit.circuit.comps.len(), "every part of the circuit is placed");
    let results = fab_elec_design::engines::full_with(&evaluated.design, &fab_elec_board::Factories::new());
    let drc = &results.boards.iter().find(|b| b.unit == unit.id).expect("the board's results").drc;
    assert!(drc.is_empty(), "the routed board has no finding at all, an unrouted net included: {drc:#?}");
    assert_eq!(fab_elec_lang::write(board, &unit.circuit), include_str!("../handheld.pcb.splash"),
        "the board document is the canonical writer's fixed point");
    let bench = evaluated.benches.iter().find(|b| b.name == "sand").expect("handheld has the sand bench");
    assert_eq!(bench.state, UnitState::Built, "sand bench builds: {:?}", bench.state.diags());
    let lowered = lower(&evaluated.design, bench.scope_id(), &bench.spec, &ModelLibs::generic());
    assert_eq!(lowered.firmware.len(), 1, "handheld lowers one firmware binding: {:?}", lowered.diags);
    let binding = &lowered.firmware[0];
    assert_eq!(binding.chip.as_deref(), Some(Chip::S3.name()),
        "the MCU binding carries the S3 chip model key");
    assert_eq!(binding.bus.parts.len(), 6, "panel, touch, IMU, expander, codec and amplifier all lower: {:?}", lowered.diags);
    assert_eq!(binding.doc, "handheld.fw.splash", "the sand bench binds the handheld app");
    let net = |path: &str, pin: &str| {
        let part = binding.bus.parts.iter().find(|p| p.path == path)
            .unwrap_or_else(|| panic!("the bus retains part {path}"));
        let (_, conn) = part.pins.iter().find(|(name, _)| name == pin)
            .unwrap_or_else(|| panic!("{path} retains model pin {pin}"));
        let BusConn::Net(net) = conn else { panic!("{path}.{pin} connects to a signal net") };
        *net
    };
    assert_eq!(net("touch", "RST"), net("expander", "P2"), "touch reset reaches expander P2");
    assert_eq!(net("imu", "INT1"), net("expander", "P6"), "IMU interrupt reaches expander P6");
    assert_eq!(binding.bus.nets[net("touch", "INT") as usize].chip, ["GPIO21"],
        "touch interrupt reaches GPIO21");
    for (pin, gpio) in [("SIO1", "GPIO5"), ("SIO2", "GPIO6"), ("SIO3", "GPIO7")] {
        assert_eq!(binding.bus.nets[net("panel", pin) as usize].chip, [gpio],
            "panel {pin} reaches {gpio}");
    }
    let pins = wiring(&binding.bus.board(0)).expect("handheld bus wiring resolves");
    assert_eq!(pins.i2c, Some(VOICEBOX_I2C), "shared I2C uses the board's pins and frequency");
    assert_eq!(pins.panel, Some(VOICEBOX_PANEL), "panel geometry, SPI pins and expander reset match the board");
    assert_eq!(pins.touch, Some(VOICEBOX_TOUCH), "touch address matches the board");
    assert_eq!(pins.motion, Some(VOICEBOX_MOTION), "motion address matches the board");
    assert_eq!(pins.audio, Some(Audio {
        addr: 0x18, mclk: 16, sclk: 9, lrck: 45, dsdin: 8, asdout: Some(10),
        amps: vec![AmpEnable { gpio: 46, enable_high: true }],
    }), "the codec's CE strap, I2S pins and the amplifier enable match the board");
    let input = firmware_input(&sources, binding, pins);
    assert_eq!(input.f_clk, 40_000_000.0, "the board's crystal is 40 MHz");
    let built = compile(&input).unwrap_or_else(|diags| panic!("Sand compiles through the firmware build: {diags:?}"));
    let Built::S3(image) = &built else { panic!("Sand builds an S3 image") };
    for note in &image.notes {
        eprintln!("Sand note: {note}");
        // This is the compiler's static device-time bound, not host wall time.
        // No long-handler note means step stays within one 20 ms frame.
        if let Some(bound) = note.strip_prefix("the handler `step` can run for up to ") {
            let ms = bound.split_once(" ms;")
                .and_then(|(ms, _)| ms.parse::<f64>().ok())
                .unwrap_or_else(|| panic!("step's static-bound note contains milliseconds: {note}"));
            assert!(ms <= 60.0, "step's static device-time bound stays within three 20 ms frames: {note}");
        }
    }
    let (mut log, feed) = EventLog::<FwFrame>::new(END, |frame| {
        if frame.is_image() { EventSource::Image } else { EventSource::Trace }
    }, Arc::new(|| {}));
    let readers = Arc::new(Mutex::new(Vec::new()));
    let received = readers.clone();
    let (make, _) = maker(&input, built, feed, false, Arc::new(move |set| {
        *received.lock().expect("screen sink lock is healthy") = set;
    }), None);
    let device = make(&lowered, 0).expect("maker builds member 0 from the lowered circuit");
    let mut readers = readers.lock().expect("screen reader lock is healthy");
    assert_eq!(readers.len(), 1, "member 0 hands the host exactly one screen reader");
    let (_, mut reader) = readers.pop().expect("the host retains the maker's screen reader");
    drop(readers);
    assert_eq!(reader.glass(), GLASS, "the reader's glass is the fixture's 368 x 448 window");
    let spec = LiveSpec { rate: f64::INFINITY, max_step: 1e-3, res: 0.0, trace: false, ..LiveSpec::default() };
    let mut sim = LiveSim::new(lowered.circuit.clone(), lowered.plan.clone(), lowered.index.generation, spec)
        .expect("the handheld circuit's operating point converges");
    sim.attach(Some(device));
    let started = Instant::now();
    while sim.t() < END && sand_pixels(reader.latest(), 0) == 0 {
        advance(&mut sim, &mut log, END);
    }
    let first = reader.latest().clone();
    assert!(sand_pixels(&first, 0) > 0, "the first drawn frame contains sand colours inside the glass");
    eprintln!("Sand first drawn frame: {:.6} simulated seconds", first.t);
    // Exclude setup's heap painting from the steady interval. Its 81 cells contain
    // 1296 pixels; the motion step preserves grains, and this test sends no inputs.
    while sim.t() < END && sand_pixels(reader.latest(), 0) < 81 * 16 {
        advance(&mut sim, &mut log, END);
    }
    assert!(sand_pixels(reader.latest(), 0) >= 81 * 16, "setup draws the complete starting heap");
    let step_start = sim.t();
    let wall_start = Instant::now();
    let later = step_start + 0.3;
    assert!(later <= END, "panel startup leaves 300 ms for falling sand: {step_start:.6} s");
    while sim.t() < later {
        advance(&mut sim, &mut log, later);
    }
    let wall = wall_start.elapsed().as_secs_f64();
    eprintln!("Sand step live run: {:.3} wall s/sim s ({wall:.3} wall s / {:.6} sim s)",
        wall / (sim.t() - step_start), sim.t() - step_start);
    eprintln!("Sand live run: {:.3} wall s for {:.6} sim s", started.elapsed().as_secs_f64(), sim.t());
    let stats = sim.stats();
    eprintln!("Sand live split: analog steps {:?}, device {:?}, sinks {:?}", stats.step_wall, stats.co_wall, stats.sink_wall);
    let values = sim.values(0.0);
    let volts = |name: &str| {
        let net = NetRef { board: unit.id, net: unit.circuit.net(name).unwrap_or_else(|| panic!("handheld has net {name}")).id };
        values.net_v[lowered.index.net(net).unwrap_or_else(|| panic!("{name} is simulated"))]
    };
    assert!((volts("VBUS") - 5.0).abs() < 0.05, "the bench holds VBUS at 5 V: {} V", volts("VBUS"));
    assert!((volts("3V3") - 3.3).abs() < 0.1, "the regulator holds 3V3 while sand falls: {} V", volts("3V3"));
    let second = reader.latest();
    assert!(second.generation > first.generation, "falling sand publishes a newer frame");
    assert_ne!(second.bgra, first.bgra, "the frame changes a few hundred simulated milliseconds later");
    assert!(sand_pixels(second, 25 * 4) > 0,
        "grains fall below the initial heap, so setup painting alone cannot satisfy the frame change");

    let button = unit.circuit.comp("boot_button").expect("the circuit has a BOOT switch");
    let boot = *lowered.index.momentary().iter().find(|r| r.board == unit.id && r.comp == button.id)
        .expect("BOOT is a momentary control in the lowered bench");
    const POLL: f64 = 0.020;
    let press = |sim: &mut LiveSim| {
        let down = sim.t();
        for (at, value) in [(down, 1.0), (down + 2.0 * POLL, 0.0)] {
            let cmds = lowered.index.command(&SimCmd::SetParam { comp: boot, value });
            assert!(!cmds.is_empty(), "BOOT has a simulated switch command");
            for cmd in cmds {
                assert!(sim.apply(LiveCmd::At(at, Box::new(cmd))), "the BOOT edge reaches the timeline");
            }
        }
    };
    // Step moves grains and never makes or loses one: the heap is 81 cells of 16 pixels.
    let grains = 81 * 16;
    const TICK: u32 = 0xffce_cbce;
    const TRACK: u32 = 0xff21_2021;
    const READY_GREEN: u32 = 0xff4a_cb73;
    let pixel = |f: &ScreenFrame, x: usize, y: usize| f.bgra[(GLASS[1] + y) * f.width + GLASS[0] + x];
    let meter_drawn = |f: &ScreenFrame| {
        f.on && f.brightness > 0 && pixel(f, 102, 254) == TICK && pixel(f, 343, 247) == TRACK
    };
    for (page, talk) in [("Voice Toy", false), ("Talk", true)] {
        press(&mut sim);
        // One poll and five paints. Talk's next 20 ms mic callback paints its connection state.
        let deadline = sim.t() + if talk { 7.0 * POLL } else { 6.0 * POLL };
        while sim.t() < deadline {
            let frame = reader.latest();
            if meter_drawn(frame) && (!talk || pixel(frame, 39, 183) == READY_GREEN) { break; }
            advance(&mut sim, &mut log, deadline);
        }
        let frame = reader.latest();
        assert!(meter_drawn(frame), "BOOT displays the {page} tick and dark track");
        assert!(glass_rows(frame).flatten().all(|p| !SAND.contains(p)), "no sand remains on {page}");
        for (y, row) in glass_rows(frame).enumerate() {
            if (168..184).contains(&y) {
                if talk {
                    assert_eq!(&row[24..40], &[READY_GREEN; 16], "Talk shows the ready echo connection on dot row {y}");
                } else {
                    assert_eq!(&row[24..40], &[0xff00_0000; 16], "Voice Toy has no connection dot on row {y}");
                }
            }
            if (200..248).contains(&y) {
                assert!(row[24..344].iter().all(|&p| p == TRACK), "the whole {page} idle meter track is dark on row {y}");
            }
            if (254..264).contains(&y) {
                assert_eq!(&row[102..104], &[TICK; 2], "the {page} threshold tick is visible on row {y}");
            }
        }
    }

    press(&mut sim);
    // Pictures: five paints clear the glass, and then the page opens connection 0. The bench's
    // folder is empty, so the server's count is 0 and the glass stays black. The first paint may
    // already clear the Talk rows, so the run goes on past the press and all five paints.
    let deadline = sim.t() + 7.0 * POLL;
    while sim.t() < deadline {
        advance(&mut sim, &mut log, deadline);
    }
    let black = |f: &ScreenFrame| f.on && f.brightness > 0 && glass_rows(f).flatten().all(|&p| p == 0xff00_0000);
    assert!(black(reader.latest()), "BOOT shows Pictures: the meter, dot and track are cleared and no picture is drawn");

    press(&mut sim);
    // Back on Sand, its step resumes from the grid it left (the grains go on falling) while five
    // paints restore every row: all the grains show again and the meter is gone.
    let deadline = sim.t() + 7.0 * POLL;
    let restored = |f: &ScreenFrame| f.on && f.brightness > 0 && sand_pixels(f, 0) == grains
        && glass_rows(f).flatten().all(|p| *p != TICK && *p != TRACK);
    while sim.t() < deadline && !restored(reader.latest()) {
        advance(&mut sim, &mut log, deadline);
    }
    let faults: Vec<_> = log.store.span(0.0, sim.t()).filter_map(|e| match &e.frame {
        FwFrame::Image(frame) if matches!(frame.event, watch::Event::Fault { .. }) => Some((e.t, frame.event)),
        _ => None,
    }).collect();
    assert!(faults.is_empty(), "the app sends no fault from boot through all four page switches: {faults:?}");
    assert!(restored(reader.latest()), "the fourth BOOT press shows all {grains} sand pixels again and clears the meter and dot");
    assert!(sand_pixels(reader.latest(), 25 * 4) > 0, "Sand resumes from the fallen grains, not from its starting heap");
}

/// Sand on the live circuit with its S3 in lockstep: the decoded tier and the reference run side by
/// side through the co-simulation's own runs, reruns and rewinds, and after each one the whole
/// machine must be the same (CPU, SRAM, peripherals, the panel's frame memory, the touch, IMU and
/// expander models, the trace and every output stream). Setup, the panel's power-up and the first
/// drawn frames run through SPI, GDMA, I2C, the timers and interrupts.
#[test]
fn sand_runs_the_same_in_the_native_tier_as_in_the_reference() {
    const UNTIL: f64 = 0.55;
    let sources = fab_elec_fixtures::docs("handheld").expect("handheld is a registered stock project");
    let evaluated = evaluate(&sources, &std_libs()).unwrap_or_else(|diags| panic!("handheld documents evaluate: {diags:?}"));
    let bench = evaluated.benches.iter().find(|b| b.name == "sand").expect("handheld has the sand bench");
    let lowered = lower(&evaluated.design, bench.scope_id(), &bench.spec, &ModelLibs::generic());
    let binding = &lowered.firmware[0];
    let input = firmware_input(&sources, binding, wiring(&binding.bus.board(0)).expect("handheld bus wiring resolves"));
    let Built::S3(image) = compile(&input).unwrap_or_else(|d| panic!("Sand compiles: {d:?}")) else { panic!("Sand builds an S3 image") };
    let netlist = Netlist::new(binding.bus.board(0)).expect("the handheld board builds");
    // The machine as the app's co-simulation makes it (`s3_maker`).
    let machine = fab_elec_fw::s3_machine(Arc::new(image.flash.clone()), 0, netlist).expect("the board binds");
    let pins = lowered.cosim_pins(&binding.part);
    let config = Config { stretch: 96_000, debug: Some(Arc::new(image.debug.clone())) };
    // Native page units, compiled at once, and the decoded lane elsewhere.
    fab_elec_mcu_emu::jit::set_inline_compile(true);
    let mut machine = Lockstep::new(machine);
    machine.set_tier(Tier::Native);
    let device = CoMachine::new(machine, config, &pins).expect("the lockstep device builds");
    let spec = LiveSpec { rate: f64::INFINITY, max_step: 1e-3, res: 0.0, trace: false, ..LiveSpec::default() };
    let mut sim = LiveSim::new(lowered.circuit.clone(), lowered.plan.clone(), lowered.index.generation, spec)
        .expect("the handheld circuit's operating point converges");
    sim.attach(Some(CoSimDevice { dev: Box::new(device), pins }));
    while sim.t() < UNTIL {
        sim.advance((sim.t() + 1e-3).min(UNTIL), Some(Duration::from_millis(50)), &mut Discard)
            .unwrap_or_else(|d| panic!("the live circuit advances in lockstep: {d}"));
        sim.values(0.0);
    }
    assert!(sim.stats().co_runs > 1000, "the device ran in many co-simulation runs: {}", sim.stats().co_runs);
}

// The bench build keeps scalar debug addresses. For samples the image itself carries the
// descriptor used by copy and play (emit_s3/src/image.rs): address, length, element type,
// element bytes, width and three reserved words. Only take is a 64,000-element I16 buffer.
fn take_address(flash: &[u8]) -> u32 {
    let regions = fab_elec_mcu_emit_s3::flash_regions(flash);
    let (_, slot) = regions.iter().find(|(offset, _)| *offset == VOICEBOX.slots[0]).expect("the linked app slot");
    let shape = [64_000_u32, 3, 2, 0, 0, 0, 0].map(u32::to_le_bytes);
    let mut rows = slot.as_chunks::<4>().0.windows(8).filter(|row| row[1..] == shape);
    let row = rows.next().expect("the linked slot describes the take buffer");
    assert!(rows.next().is_none(), "the take buffer has a unique descriptor");
    u32::from_le_bytes(row[0])
}

const RATE: usize = 16_000;
const BLOCK: usize = RATE / 50;
const ONSET: usize = RATE; // allows panel startup and at least 200 ms of quiet mic history
const VOICE: usize = 3 * RATE / 10;
// The end of the take, followed by the length of its playback, in samples from reset.
const PLAYBACK_END: usize = ONSET + VOICE + 2 * RATE + 10 * BLOCK + VOICE + 2 * RATE;
const CAPTURE_BY_MS: u64 = ((ONSET + VOICE + 2 * RATE + 3 * BLOCK) * 1000 / RATE) as u64;

// The bench echo has no delay, jitter or rate limit. Its only timed event in the loss
// tests is Disconnect at network time 1 ns. The device and its USB polls keep real time.
const DISCONNECT_NS: u64 = 1;

#[derive(Clone, Copy)]
enum EchoPoll { BoardTime, BeforeLoss, CollectUpload }

struct Reopen {
    // Accepted records end at the loss. Frames still on USB can cross the offline reply.
    before_loss: usize,
    second_open: usize,
    // Board tick at the first net_epoch return that tells Talk its connection ended.
    epoch_tick: u64,
}

fn sample(i: usize) -> i16 {
    if !(ONSET..ONSET + VOICE).contains(&i) { return 0; }
    let k = i - ONSET;
    let magnitude = 4000 + k as i16;
    if k / 20 % 2 == 0 { magnitude } else { -magnitude }
}

struct VoiceBench {
    machine: S3Machine<Netlist>,
    codec: u16,
    debug: FwDebug,
    take_addr: u32,
    link: NetLink,
    ms: u64,
    poll: EchoPoll,
}

impl VoiceBench {
    fn new(name: &str, poll: EchoPoll) -> Self {
        Self::build(name, poll, Vec::new(), |binding, bench_world| {
            let world = match poll {
                EchoPoll::BoardTime => bench_world,
                _ => {
                    let endpoint = binding.net.iter().find(|e| e.c == 1).expect("Talk declares connection one");
                    assert!(matches!(endpoint.fake, fab_elec_bench::net::Fake::Echo), "Talk's server is the bench echo");
                    assert_eq!((endpoint.latency, endpoint.jitter, endpoint.rate), (0.0, 0.0, 0.0),
                        "the bench echo needs no elapsed network time to answer");
                    // Connection 0 is Pictures, which Talk never opens.
                    net::World::new(0, vec![net::Endpoint { responder: net::Responder::Sink }, net::Endpoint { responder: net::Responder::Echo }],
                        vec![(DISCONNECT_NS, net::Condition::Disconnect { c: 1, code: net::code::CLOSED, detail: 1001 })])
                }
            };
            (world, ())
        }).0
    }

    fn build<T>(name: &str, poll: EchoPoll, folders: Vec<(String, Vec<(String, Arc<[u8]>)>)>,
        network: impl FnOnce(&FwBinding, net::World) -> (net::World, T)) -> (Self, T) {
        let sources = fab_elec_fixtures::docs("handheld").expect("handheld is a stock project");
        let evaluated = evaluate(&sources, &std_libs()).unwrap_or_else(|d| panic!("handheld evaluates: {d:?}"));
        let bench = evaluated.benches.iter().find(|b| b.name == name).unwrap_or_else(|| panic!("handheld has a {name} bench"));
        let lowered = lower(&evaluated.design, bench.scope_id(), &bench.spec, &ModelLibs::generic());
        let [binding] = lowered.firmware.as_slice() else { panic!("{name} binds one MCU: {:?}", lowered.diags) };
        assert_eq!(binding.doc, "handheld.fw.splash", "{name} runs the handheld app");
        let board = binding.bus.board(0);
        let codec = board.parts.iter().position(|p| matches!(p.kind, PartKind::Es8311 { .. }))
            .expect("the lowered handheld has a microphone codec") as u16;
        let mut input = firmware_input(&sources, binding, wiring(&board).expect("the handheld wiring resolves"));
        input.net_files = folders;
        let Built::S3(image) = compile(&input).unwrap_or_else(|d| panic!("the {name} bench compiles: {d:?}"))
            else { panic!("{name} builds an S3 image") };
        let fakes = fab_elec_fw::NetFakes::new(&input.net_files);
        let bench_world = fab_elec_fw::net_world(0, image.net_slots.len(), &binding.net, &[], &fakes).expect("the bench's network fakes build");
        let (world, server) = network(binding, bench_world);
        let link = NetLink::new(Host::new(world, &image.net_slots));
        let take_addr = take_address(&image.flash);
        let netlist = Netlist::new(board).expect("the handheld netlist builds");
        let machine = fab_elec_fw::s3_machine(Arc::new(image.flash), 0, netlist).expect("the handheld S3 binds");
        let mut bench = Self { machine, codec, debug: image.debug, take_addr, link, ms: 0, poll };
        // Each voiced frame has a distinct magnitude, so a repeated or missing sample is visible.
        if name != "pictures" { bench.mic_track((0..ONSET + VOICE).map(sample)); }
        (bench, server)
    }

    fn mic_track(&mut self, samples: impl Iterator<Item = i16>) {
        let origin_ps = u128::from(self.machine.board_time()) * 1_000_000_000_000 / u128::from(self.machine.tick_hz());
        let track = Arc::new(SoundTrack::new(origin_ps as u64, (RATE as u32, 1)));
        for v in samples { track.push(v); }
        track.close();
        assert!(self.machine.host_input(&Stimulus::Sound { part: self.codec, track: TrackRef(track) }), "the S3 accepts the mic track");
    }

    // Stop at the given PC or finish the interval. Observing a port never moves a USB poll.
    fn run_to(&mut self, end_ms: u64, stop: Option<u32>) -> bool {
        let mut marks = PcSet::default();
        if let Some(pc) = stop { marks = PcSet::new(pc, 1); marks.insert(pc); }
        let ticks_per_ms = self.machine.tick_hz() / 1000;
        let mut barrier = None;
        while self.ms < end_ms {
            let until = Until { tick: (self.ms + 1) * ticks_per_ms, marks: &marks, stops: stop.as_slice(),
                stops_after: self.machine.now(), on_drive: false };
            match self.machine.run(&until) {
                Ran::Tick => {
                    self.ms += 1;
                    match self.poll {
                        EchoPoll::BoardTime => self.link.exchange(&mut self.machine, self.ms * 1_000_000),
                        EchoPoll::BeforeLoss => self.link.exchange(&mut self.machine, 0),
                        EchoPoll::CollectUpload => self.link.read_up(&mut self.machine, DISCONNECT_NS),
                    }
                }
                Ran::Pc => return true,
                Ran::Barrier => {
                    let now = self.machine.now();
                    assert_ne!(barrier.replace(now), Some(now), "the panel journal advances between marks");
                    self.machine.mark();
                }
                other => panic!("the handheld stopped while running the app: {other:?}, {:?}", self.machine.status()),
            }
        }
        false
    }

    fn var(&self, name: &str) -> usize {
        let v = self.debug.vars.iter().find(|v| v.name == name).unwrap_or_else(|| panic!("debug address for {name}"));
        let mut word = [0; 4];
        assert!(self.machine.read_data(v.addr, &mut word), "the runtime value of {name} is readable");
        u32::from_le_bytes(word) as usize
    }

    fn port(&self, name: &str) -> u32 {
        self.debug.runtime.iter().find(|r| r.1 == name)
            .unwrap_or_else(|| panic!("the image has a {name} port routine")).0
    }

    fn wait_for(&mut self, end_ms: u64, property: &str, reached: impl Fn(&Self) -> bool) {
        while !reached(self) {
            assert!(self.ms < end_ms, "{property} by {end_ms} ms");
            self.run_to(self.ms + 1, None);
        }
    }

    fn start_upload(&mut self) {
        let send = self.port("net_send");
        loop {
            assert!(self.run_to(CAPTURE_BY_MS, Some(send)), "the completed take starts an upload");
            if self.var("turn") == 1 && self.machine.cpu().ar[6] == net::SEND_BINARY as u32 { break; }
        }
    }

    // A 320-sample block needs eleven USB packets, plus a credit and a short flush.
    // Bound both traversals and playback, which normally overlap.
    fn echo_deadline(&self, taken: usize) -> u64 {
        assert!((1..=64_000).contains(&taken), "Talk captured a take in its sample buffer");
        self.ms + (taken * 1000).div_ceil(RATE) as u64 + 2 * 13 * taken.div_ceil(BLOCK) as u64 + 4 * 20
    }

    fn finish_playback(&mut self, taken: usize) {
        self.wait_for(self.echo_deadline(taken), "the speaker drains the echoed take",
            |b| b.var("sp_r") == taken && b.var("speaking") == 0);
        self.run_to(self.ms + 20, None);
    }

    fn disconnect_and_reopen(&mut self) -> Reopen {
        let loss_ms = self.ms;
        let old = self.var("talk_epoch") as u32;
        let down = self.link.down.len();
        self.link.exchange(&mut self.machine, DISCONNECT_NS);
        self.poll = EchoPoll::BoardTime;
        let before_loss = self.link.up.len();
        assert_eq!(&self.link.down[down..],
            &[NetMsg::State { c: 1, state: net::OFFLINE, code: net::code::CLOSED, detail: 1001 }],
            "the World ends the first connection at the chosen exchange");

        // Four occupied receive slots can precede OFFLINE on USB. Observe the new epoch
        // at the port return, independently of when the app mutes or the output goes quiet.
        let epoch = self.port("net_epoch");
        let observed_by = self.ms + 4 * 11 + 2 * 20;
        loop {
            assert!(self.run_to(observed_by, Some(epoch)), "Talk reads the epoch after the disconnect");
            let returned = self.machine.cpu().ar[0];
            assert!(self.run_to(observed_by, Some(returned)), "net_epoch returns to Talk after the disconnect");
            if self.machine.cpu().ar[2] != old { break; }
        }
        assert_eq!(self.machine.cpu().ar[2], old + 1, "losing one ready connection advances the epoch once");
        let epoch_tick = self.machine.board_time();
        let retry_ms = net::RETRY_FIRST_NS / 1_000_000;
        // A mic handler may straddle the retry deadline. Allow its period and two USB polls.
        self.wait_for(self.ms + retry_ms + 20 + 2, "the host sees the backoff reopen", |b|
            b.link.up[before_loss..].iter().any(|(_, m)| matches!(m, NetMsg::Open { c: 1 })));
        assert!(self.ms >= loss_ms + retry_ms, "the second Open waits the first backoff after loss");
        let second_open = before_loss + self.link.up[before_loss..].iter()
            .position(|(_, m)| matches!(m, NetMsg::Open { c: 1 })).expect("the second Open reached the host");
        assert_eq!(self.link.host.stale, (second_open - before_loss) as u64,
            "the host discarded every old frame that crossed OFFLINE before the second Open");
        let states: Vec<_> = self.link.down.iter().filter_map(|m| match m {
            NetMsg::State { c: 1, state, .. } => Some(*state), _ => None,
        }).collect();
        assert_eq!(states, [net::READY, net::OFFLINE, net::READY], "the reopen starts a second ready period");
        Reopen { before_loss, second_open, epoch_tick }
    }

    fn check_take(&self) -> Vec<i16> {
        assert!(matches!(self.link.hello.map(|f| f.event), Some(watch::Event::Hello { image, .. }) if image == self.debug.image_id),
            "the watch log covers this image from boot");
        assert!(self.link.faults.is_empty(), "the mic, connection and speaker ran without a fault: {:?}", self.link.faults);
        assert_eq!(self.var("takes"), 1, "one stretch of voice makes one take");
        assert_eq!(self.var("rewound"), 16 * BLOCK, "rewind recovers six attack blocks and ten pre-roll blocks");
        let taken = self.var("taken");
        assert!(taken <= 64_000, "the take fits its declared sample buffer");
        let mut pcm = vec![0; 2 * taken];
        assert!(self.machine.read_data(self.take_addr, &mut pcm), "the recorded take is readable");
        let take: Vec<_> = pcm.chunks_exact(2).map(|s| i16::from_le_bytes([s[0], s[1]])).collect();
        let lead = take.iter().position(|&v| v != 0).expect("the take holds voiced samples");
        assert!(lead.abs_diff(10 * BLOCK) <= BLOCK, "the take starts 200 ms before speech within one block: {lead} samples");
        let tail = taken.checked_sub(lead + VOICE).expect("the entire voice fits after its pre-roll");
        assert!(tail.abs_diff(2 * RATE) <= BLOCK, "the take ends two quiet seconds after speech within one block: {tail} samples");
        for (i, &v) in take.iter().enumerate() {
            assert_eq!(v, sample(ONSET - lead + i), "captured sample {i} matches the microphone input");
        }
        take
    }

    fn check_playback(&mut self, take: &[i16], loss_tick: Option<u64>) -> u64 {
        // The NS4150B's 120 ms start fits in the silent pre-roll. At 4 dB the model rounds the
        // DAC word times 10^(4/20) to the nearest I16; this track stays below clipping.
        let mut audio = SpeakerOutput::default();
        self.machine.take_audio(&mut audio);
        let mut next = audio.runs.first().expect("the board clocked speaker frames").t0;
        let frame_ticks = self.machine.tick_hz() / RATE as u64;
        for (run, samples) in audio.each() {
            assert_eq!(run.rate, (RATE as u64, 1), "the speaker clocks the take at its recorded rate");
            assert_eq!(run.t0, next, "the speaker sample clock continues across output runs");
            next += samples.len() as u64 * frame_ticks;
        }
        assert!(next + frame_ticks >= self.machine.board_time(), "the speaker trace reaches the end of the run");
        let first = audio.samples.iter().position(|&v| v != 0).expect("the handheld speaker sounds");
        let lead = take.iter().position(|&v| v != 0).expect("the take has voiced samples");
        let start = first.checked_sub(lead).expect("the speaker output includes the silent pre-roll");
        let start_tick = audio.runs[0].t0 + start as u64 * frame_ticks;
        assert!(start_tick >= (ONSET + take.len() - lead) as u64 * frame_ticks,
            "playback starts after the last captured sample");
        let end = match loss_tick {
            Some(tick) => {
                let end = (tick - audio.runs[0].t0).div_ceil(frame_ticks) as usize;
                assert!(end > first && end < first + VOICE, "the epoch changed in the audible part of the take");
                end
            }
            None => start + take.len(),
        };
        let playback = audio.samples.get(start..end).expect("the speaker output covers the expected playback");
        let gain = 10.0_f64.powf(4.0 / 20.0);
        for (i, (&v, &heard)) in take.iter().zip(playback).enumerate() {
            assert_eq!(heard, (f64::from(v) * gain).round() as i16, "amplified playback sample {i}");
        }
        assert_eq!(audio.samples[end..].iter().position(|&v| v != 0), None,
            "the speaker stays silent from frame {end} through the end of the run");
        start_tick
    }
}

/// F16 through the app bound by the voice bench, from the ES8311 input to the amplified output.
#[test]
fn f16_a_take_keeps_the_pre_roll_and_ends_after_two_quiet_seconds() {
    let mut b = VoiceBench::new("voice", EchoPoll::BoardTime);
    // Four extra blocks cover block alignment, the speaker's 40 ms ring and the final mic poll.
    b.run_to(((PLAYBACK_END + 4 * BLOCK) * 1000 / RATE) as u64, None);
    assert_eq!(b.var("shown"), 1, "the voice bench starts on the Voice Toy page");
    assert_eq!(b.var("played"), b.var("taken"), "the speaker accepted every sample in the take");
    assert_eq!(b.var("playing"), 0, "the playback drained and finished");
    let take = b.check_take();
    b.check_playback(&take, None);
}

/// The spoken question of the twin's real-hub run (a synthesised voice): the mean absolute sample of
/// each 20 ms block at 0 dB, from its first sound to its last.
const QUESTION: [i16; 117] = [
    1, 5, 115, 810, 3693, 5588, 4905, 4754, 3061, 1401, 1271, 2522, 4501, 4283, 3722, 3009,
    2313, 1811, 1266, 415, 111, 233, 1635, 3545, 2383, 1218, 291, 53, 615, 1782, 1253, 1702,
    1838, 3273, 4141, 3514, 3176, 3402, 3223, 3108, 3342, 3168, 2473, 2349, 1920, 1558, 1497,
    301, 16, 364, 258, 1824, 3930, 4307, 4283, 4851, 4330, 3634, 3336, 3385, 3406, 3320, 3103,
    2712, 1279, 540, 1147, 590, 977, 2092, 1938, 1971, 1841, 1814, 1592, 1709, 1830, 2205, 2082,
    2043, 2138, 2473, 2031, 1754, 2334, 3501, 4352, 3999, 3620, 3345, 3023, 2971, 2520, 1816,
    1619, 1214, 987, 1041, 920, 834, 697, 359, 537, 454, 298, 159, 97, 46, 38, 34, 9, 7, 4, 1,
    1, 0, 1,
];

/// The question at `db` (the pane's mic level) on a 400 Hz square, so each block's level is the
/// question's at that level.
fn question(db: f64) -> impl Iterator<Item = i16> {
    let gain = 10f64.powf(db / 20.0);
    QUESTION.into_iter().flat_map(move |level| {
        let a = (f64::from(level) * gain).round() as i16;
        (0..BLOCK).map(move |k| if k / 20 % 2 == 0 { a } else { -a })
    })
}

/// A steady hiss, uniform within ±200 (a mean level of 100, about -50 dBFS), from a fixed generator.
fn hiss(n: usize) -> impl Iterator<Item = i16> {
    let mut state = 0x2545_f491u32;
    (0..n).map(move |_| {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        ((state >> 16) % 401) as i16 - 200
    })
}

#[test]
fn voice_starts_over_the_noise_floor_at_a_quiet_or_a_loud_mic() {
    // A hiss from power-on, then silence: neither is voice, though the hiss is louder than the
    // quiet question's softer blocks, which a fixed threshold low enough to hear it would take.
    let ms = |samples: usize| (samples * 1000 / RATE) as u64;
    let hissed = 6 * RATE / 5;
    let silent = hissed + 2 * RATE / 5;
    let mut quiet = VoiceBench::new("voice", EchoPoll::BoardTime);
    quiet.mic_track(hiss(hissed).chain(std::iter::repeat_n(0, silent - hissed)).chain(question(-24.0)));
    quiet.run_to(ms(hissed), None);
    assert_eq!(quiet.var("shown"), 1, "the voice bench starts on the Voice Toy page");
    let floor = quiet.var("noise_floor");
    assert!((80..=100).contains(&floor), "the floor follows the hiss: {floor}");
    assert_eq!((quiet.var("takes"), quiet.var("taking"), quiet.var("loud")), (0, 0, 0), "a steady hiss is not voice");
    quiet.run_to(ms(silent), None);
    assert_eq!(quiet.var("noise_floor"), 0, "silence brings the floor down at once");
    assert_eq!((quiet.var("takes"), quiet.var("taking")), (0, 0), "silence is not voice");
    // The question's first voiced blocks reach the detector a block or two late; six start the take.
    quiet.wait_for(ms(silent) + 400, "the question at -24 dB starts a take", |b| b.var("taking") == 1);
    assert_eq!(quiet.var("rewound"), 16 * BLOCK, "its attack had no quiet block, so the take reaches 200 ms before it");

    let mut loud = VoiceBench::new("voice", EchoPoll::BoardTime);
    loud.mic_track(std::iter::repeat_n(0, ONSET).chain(question(0.0)));
    loud.wait_for(ms(ONSET) + 400, "the question at 0 dB starts a take", |b| b.var("taking") == 1);
    assert!(loud.link.faults.is_empty() && quiet.link.faults.is_empty(), "the mic ran without a fault");
}

enum Upload { Hello, Interrupted(u16), Complete(u16) }

fn check_voice_up(up: &[(u32, NetMsg)], take: &[i16], upload: Upload) -> usize {
    // Host checks the grant and FIRST/LAST ordering. Reassemble the WebSocket messages at
    // those boundaries; a short net_send can split even a single 20 ms block across parts.
    let mut messages = Vec::new();
    let mut partial = None;
    for (_, msg) in up {
        match msg {
            NetMsg::Open { c: 1 } | NetMsg::Freed { c: 1, .. } => {}
            NetMsg::Data { c: 1, flags, bytes } => {
                let binary = flags & net::PART_BINARY != 0;
                if flags & net::PART_FIRST != 0 { partial = Some((binary, Vec::new())); }
                let (kind, data) = partial.as_mut().expect("the upstream message starts with FIRST");
                assert_eq!(*kind, binary, "all parts of the upstream message have the same kind");
                data.extend_from_slice(bytes);
                if flags & net::PART_LAST != 0 { messages.push(partial.take().unwrap()); }
            }
            other => panic!("unexpected upstream traffic in the ready period: {other:?}"),
        }
    }
    assert!(partial.is_none() || matches!(upload, Upload::Interrupted(_)), "only a loss can cut a message short");
    let (hello, rest) = messages.split_first().expect("the connection sent its greeting");
    assert!(!hello.0 && hello.1.as_slice() == b"hello 1 16000", "the first message after ready announces protocol 1 and 16 kHz");
    let turn = match upload {
        Upload::Hello => {
            assert!(rest.is_empty(), "after loss during speech the new connection sends only hello");
            return 0;
        }
        Upload::Interrupted(turn) | Upload::Complete(turn) => turn,
    };
    let (begin, rest) = rest.split_first().expect("the greeting is followed by a take");
    assert!(!begin.0 && begin.1.as_slice() == format!("take {turn}").as_bytes(), "the next message opens turn {turn}");
    let (blocks, end) = match rest.split_last() {
        Some((last, blocks)) if !last.0 => (blocks, Some(last)),
        _ => (rest, None),
    };
    let mut offset = 0;
    for (binary, bytes) in blocks {
        assert!(*binary, "only binary samples follow take before end");
        let (header, pcm) = bytes.split_at_checked(8).expect("each voice block has an eight-byte header");
        assert_eq!(&header[..2], b"M\0", "the voice-up magic and reserved byte at sample {offset}");
        assert_eq!(u16::from_le_bytes(header[2..4].try_into().unwrap()), turn, "each block belongs to turn {turn}");
        assert_eq!(u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize, offset,
            "the next block starts at the next sample, including offset zero");
        let (samples, rest) = pcm.as_chunks::<2>();
        assert!(rest.is_empty(), "the PCM16 payload ends on a whole sample");
        for word in samples {
            assert_eq!(take.get(offset).copied(), Some(i16::from_le_bytes(*word)), "upstream PCM matches take sample {offset}");
            offset += 1;
        }
    }
    if let Some(end) = end {
        assert_eq!(offset, take.len(), "all captured samples reached the host before end");
        assert_eq!(end.1.as_slice(), format!("end {turn} {offset}").as_bytes(), "end names this turn and its actual sample count");
        assert!(partial.is_none(), "no new message starts after end");
    } else {
        assert!(matches!(upload, Upload::Interrupted(_)), "a complete upload ends with its sample count");
    }
    offset
}

#[test]
fn talk_uploads_one_turn_and_plays_its_echo_as_speech() {
    let mut b = VoiceBench::new("talk", EchoPoll::BoardTime);
    let send = b.port("net_send");
    let record = b.port("record");
    b.start_upload();

    // Let the physical TX ring fill. At a CALL0 port's entry a0 is its return PC; at that
    // PC a2 is the actual result. Keep USB blocked until the mic reads another whole block.
    b.machine.set_usb_host_listening(false);
    let blocked_by = b.ms + 4 * 20;
    loop {
        assert!(b.machine.cpu().ar[5] > 0, "the upload requests a nonempty suffix");
        let returned = b.machine.cpu().ar[0];
        assert!(b.run_to(blocked_by, Some(returned)), "net_send returns while the tether is full");
        if b.machine.cpu().ar[2] == 0 { break; }
        assert!(b.run_to(blocked_by, Some(send)), "the take exceeds the physical TX and send rings");
    }
    let mic_by = b.ms + 2 * 20;
    assert!(b.run_to(mic_by, Some(record)), "the mic handler runs again while the upload is blocked");
    let returned = b.machine.cpu().ar[0];
    assert!(b.run_to(mic_by, Some(returned)), "record returns while the upload is blocked");
    assert_eq!(b.machine.cpu().ar[2], BLOCK as u32, "the mic reads a whole block after net_send returned zero");
    b.machine.set_usb_host_listening(true);

    let taken = b.var("taken");
    b.finish_playback(taken);
    assert_eq!(b.var("shown"), 2, "the talk bench runs the Talk page");
    assert_eq!(b.var("turn"), 1, "one take opens one turn");
    assert_eq!(b.var("sent"), taken, "every captured sample went up");
    assert_eq!(b.var("sp_w"), taken, "every echoed sample came back as speech");
    assert_eq!(b.var("lost"), 0, "the speech has no missing block");
    assert_eq!(b.var("repeated"), 0, "the speech has no repeated block");
    assert_eq!(b.var("skipped"), 3, "the device skips the echoed hello, take and end lines");
    assert_eq!(b.var("sp_r"), taken, "the speaker accepted the whole echoed take");
    assert_eq!(b.var("speaking"), 0, "the speaker drained the last echoed sample");
    let take = b.check_take();
    b.check_playback(&take, None);
    let ready = b.link.down.iter().filter(|m| matches!(m, NetMsg::State { c: 1, state: net::READY, .. })).count();
    assert_eq!(ready, 1, "the echo had one ready period on connection 1");
    assert_eq!(b.link.host.stale, 0, "the host received every part while the connection was ready");
    check_voice_up(&b.link.up, &take, Upload::Complete(1));
}

#[test]
fn talk_resends_the_take_from_sample_zero_after_upload_loss() {
    let mut b = VoiceBench::new("talk", EchoPoll::BeforeLoss);
    b.start_upload();
    let taken = b.var("taken");
    // The host accepts two complete blocks at the disconnect's timestamp. Polling the
    // World there applies the condition before its same-time echoes can reach the device.
    b.poll = EchoPoll::CollectUpload;
    b.wait_for(b.ms + 4 * 20, "two complete turn-one blocks reach the host", |b|
        b.link.up.iter().filter(|(_, m)| matches!(m, NetMsg::Data { c: 1, flags, .. }
            if flags & (net::PART_BINARY | net::PART_LAST) == (net::PART_BINARY | net::PART_LAST))).count() >= 2);
    assert!(b.var("sent") < taken, "turn one still has samples to upload at the loss");
    assert_eq!((b.var("sp_w"), b.var("speaking")), (0, 0), "no speech arrived or played before the upload loss");
    let reopen = b.disconnect_and_reopen();
    let reopened_at = b.machine.board_time();
    b.finish_playback(taken);
    let take = b.check_take();
    let prefix = check_voice_up(&b.link.up[..reopen.before_loss], &take, Upload::Interrupted(1));
    assert!((2 * BLOCK..taken).contains(&prefix), "the host accepted a nonempty proper prefix of turn one");
    check_voice_up(&b.link.up[reopen.second_open..], &take, Upload::Complete(2));
    assert_eq!(b.var("turn"), 2, "the same take retries once under a new turn number");
    assert_eq!((b.var("sent"), b.var("sp_w"), b.var("sp_r")), (taken, taken, taken),
        "the retry uploads, receives and plays every sample of the original take");
    assert_eq!((b.var("lost"), b.var("repeated")), (0, 0), "the new epoch has no gaps or repeated speech blocks");
    assert!(b.check_playback(&take, None) >= reopened_at, "only the retried turn reaches the speaker");
}

#[test]
fn talk_stops_speech_on_loss_and_only_greets_after_reopen() {
    let mut b = VoiceBench::new("talk", EchoPoll::BeforeLoss);
    b.start_upload();
    let take = b.check_take();
    let lead = take.iter().position(|&v| v != 0).expect("the take contains speech");
    // sp_r includes the 40 ms speaker queue. Three blocks beyond the pre-roll put the
    // amplifier inside the distinctive ramp, leaving most of the voiced interval to cut.
    b.wait_for(b.echo_deadline(take.len()), "turn one's voice is playing", |b|
        b.var("speaking") == 1 && b.var("sp_r") >= lead + 3 * BLOCK);
    let reopen = b.disconnect_and_reopen();

    // A new voice shorter than the six-block attack must reach the detector without starting
    // another take. The following mic poll also gives the uploader a chance to run again.
    assert_eq!(b.var("loud"), 0, "the detector was quiet before the new voice");
    b.mic_track((ONSET..ONSET + 2 * BLOCK).map(sample));
    b.wait_for(b.ms + 3 * 20, "Talk listens to fresh mic input after the reopen", |b| b.var("loud") > 0);
    b.run_to(b.ms + 20, None);
    assert_eq!(b.var("takes"), 1, "the short voice does not make a second take");
    assert_eq!((b.var("speaking"), b.var("open_turn"), b.var("asking")), (0, 0, 0),
        "the loss ends playback and leaves Talk listening without a pending request");
    assert_eq!(b.var("turn"), 1, "loss during speech does not start another turn");
    assert_eq!(b.var("sp_r"), b.var("sp_w"), "no buffered speech remains to feed the speaker");
    assert!(b.link.faults.is_empty(), "speech loss and reopen produce no fault: {:?}", b.link.faults);
    check_voice_up(&b.link.up[..reopen.before_loss], &take, Upload::Interrupted(1));
    check_voice_up(&b.link.up[reopen.second_open..], &take, Upload::Hello);
    b.check_playback(&take, Some(reopen.epoch_tick));
}

// The tether World schedules the disconnect. Its upstream is the actual bench World
// built by net_world with the photos folder. At this boundary the checker sees whole
// messages, even when the link drops after only a prefix reached the glass.
struct PhotoServer {
    files: net::World,
    checker: Checker,
    expected: Pictures,
}

impl PhotoServer {
    fn request(b: &mut VoiceBench, by: u64) -> net::Request {
        loop {
            if let Some((_, request)) = b.link.host.request() { return request; }
            assert!(b.ms < by, "the device reaches the pictures server by {by} ms");
            b.run_to(b.ms + 1, None);
        }
    }

    fn reply(&mut self, b: &mut VoiceBench, gen: u32, expected_binary: bool) -> net::Payload {
        let Some((_, net::WorldEvent::Message { c: 0, binary, bytes })) = self.files.poll(0)
            else { panic!("the files fake has a whole reply on connection zero") };
        assert_eq!(binary, expected_binary, "the files fake sends the expected message kind");
        self.checker.check(false, binary, &bytes).expect("the files fake's whole reply passes the pictures checker");
        assert!(self.files.poll(0).is_none(), "the files fake sends exactly one reply");
        b.link.host.arrive(0, net::Arrival::Message { c: 0, gen, binary, bytes: bytes.to_vec() });
        bytes
    }

    fn ready(&mut self, b: &mut VoiceBench, by: u64) -> u32 {
        let net::Request::Open { c: 0, gen } = Self::request(b, by) else { panic!("Pictures opens connection zero") };
        self.files.open(0, 0);
        assert!(matches!(self.files.poll(0), Some((_, net::WorldEvent::State { c: 0, state: net::READY, .. }))),
            "the bench's files service goes ready");
        self.checker.ready();
        b.link.host.arrive(0, net::Arrival::Opened { c: 0, gen });
        // Give pic_pump two button polls with READY and its grant, withholding only count.
        let present = b.port("present");
        assert!(!b.run_to(b.ms + 40, Some(present)), "the page waits for count before presenting a band");
        assert_eq!((b.var("pic_counted"), b.var("pic_asked")), (0, 0), "the page waits for a fresh count before asking");
        let open = b.link.up.iter().rposition(|(_, m)| matches!(m, NetMsg::Open { c: 0 })).expect("the tether carried Open");
        assert!(!b.link.up[open..].iter().any(|(_, m)| matches!(m, NetMsg::Data { c: 0, .. })),
            "no application data went up on the wire before this connection's count");
        assert!(b.link.host.request().is_none(), "the ready connection stays open while waiting for count");
        assert_eq!(&*self.reply(b, gen, false), b"count 2", "both files are announced at each ready");
        gen
    }

    fn get(&mut self, b: &mut VoiceBench, gen: u32, index: usize, row: usize) -> net::Payload {
        let by = b.ms + 100;
        let mut line = Vec::new();
        loop {
            let net::Request::Send { c: 0, gen: attempt, flags, bytes } = Self::request(b, by)
                else { panic!("the page sends a get on its open pictures connection") };
            assert_eq!(attempt, gen, "the get belongs to the current connection");
            assert_eq!(flags & net::PART_BINARY, 0, "each part of get is text");
            line.extend_from_slice(&bytes);
            self.files.send(0, 0, flags, &bytes);
            b.link.host.arrive(0, net::Arrival::Taken { c: 0, gen, bytes: bytes.len() as u32 });
            if flags & net::PART_LAST != 0 { break; }
        }
        self.checker.check(true, false, &line).expect("the actual upstream get passes the pictures checker");
        let get = format!("get {index} 368 448 {row}");
        assert_eq!(line, get.as_bytes(), "the page asks for the correct picture, glass size and first unpresented row");
        let answers = self.expected.answer(false, get.as_bytes());
        let [(true, expected)] = answers.as_slice() else { panic!("Pictures answers {get} with one RGB565 block") };
        let actual = self.reply(b, gen, true);
        assert!(actual == *expected, "the bench's full reply to {get} equals the fitted picture");
        expected.clone()
    }
}

fn picture_rows(b: &VoiceBench, panel: u16, answer: &[u8], rows: std::ops::Range<usize>) {
    let Some(Part::Co5300(glass)) = b.machine.board().part(panel) else { panic!("the lowered part is the CO5300") };
    assert!(glass.display_on() && glass.brightness() > 0, "the picture is visible on the powered glass");
    let [x, y, width, _] = GLASS;
    let index = u16::from_le_bytes([answer[4], answer[5]]);
    let first = usize::from(u16::from_le_bytes([answer[10], answer[11]]));
    for row in rows {
        // CO5300::frame() is its 480-column RGB565 RAM. The reply carries high byte first.
        let at = (y + row) * 480 + x;
        let pixels = &glass.frame()[at..at + width];
        let offset = 16 + 2 * width * (row - first);
        let expected = &answer[offset..offset + 2 * width];
        for (col, (&pixel, rgb)) in pixels.iter().zip(expected.chunks_exact(2)).enumerate() {
            let visible = if glass.inversion() { !pixel } else { pixel };
            assert_eq!(visible, u16::from_be_bytes([rgb[0], rgb[1]]), "picture {index}'s fitted RGB565 pixel at ({col}, {row})");
        }
    }
}

// At present's CALL0 entry a6..a9 carry width, height, x and y. Check the destination
// even when a duplicate write would leave identical pixels. Read the glass at its return.
fn picture_band(b: &mut VoiceBench, panel: u16, answer: &[u8], row: usize, by: u64) -> bool {
    let present = b.port("present");
    if !b.run_to(by, Some(present)) { return false; }
    let regs = &b.machine.cpu().ar;
    assert_eq!(regs[6..10], [368, 2, 0, row as u32], "row {row} is presented once, as the next two-row band");
    let returned = regs[0];
    assert!(b.run_to(by + 20, Some(returned)), "the band at row {row} finishes on the panel");
    picture_rows(b, panel, answer, row..row + 2);
    true
}

#[test]
fn pictures_resumes_at_the_first_unpresented_row_and_a_tap_changes_the_glass() {
    // Portraits fill the height, with coloured pixels in the very first band. The two
    // palettes stay distinct there even after containment, dithering and RGB565 rounding.
    let warm: Vec<_> = (0..42).map(|i| [240 - 3 * i, 20 + 4 * i, 0]).collect();
    let cool: Vec<_> = (0..42).map(|i| [0, 215 - 3 * i, 240 - i]).collect();
    let photos: Vec<(String, Arc<[u8]>)> = vec![("a.png".into(), png::png(3, 14, &warm).into()),
        ("b.png".into(), png::png(3, 14, &cool).into())];
    let expected = Pictures::new(&photos);
    let (mut b, files) = VoiceBench::build("pictures", EchoPoll::BeforeLoss, vec![("photos".into(), photos)], |_, files| {
        let world = net::World::new(0, vec![net::Endpoint { responder: net::Responder::Upstream }],
            vec![(DISCONNECT_NS, net::Condition::Disconnect { c: 0, code: net::code::CLOSED, detail: 1001 })]);
        (world, files)
    });
    let mut server = PhotoServer { files, checker: Checker::default(), expected };
    let part = |matches: fn(&Part) -> bool| (0..=u16::MAX).map_while(|i| b.machine.board().part(i).map(|p| (i, p)))
        .find(|(_, p)| matches(p)).expect("the handheld has the requested panel or touch part").0;
    let panel = part(|p| matches!(p, Part::Co5300(_)));
    let touch = part(|p| matches!(p, Part::Cst820(_)));
    let first = server.ready(&mut b, 1000);
    assert_eq!(b.var("shown"), 3, "the pictures bench starts on the Pictures page");
    let mut bus = Vec::new();
    b.machine.take_bus(&mut bus);
    bus.clear();
    let whole = server.get(&mut b, first, 0, 0);
    let mut row = 0;
    while row < 32 {
        let by = b.ms + 80; // a 1472-byte band takes 23 USB packets, plus the page's 20 ms poll
        assert!(picture_band(&mut b, panel, &whole, row, by), "picture zero paints row {row} before loss");
        row += 2;
    }

    let loss_ms = b.ms;
    b.link.exchange(&mut b.machine, DISCONNECT_NS);
    b.poll = EchoPoll::BoardTime;
    assert!(matches!(b.link.host.request(), Some((_, net::Request::Close { c: 0, gen })) if gen == first),
        "the timeline disconnect ends the first pictures connection");
    server.files.close(0, 0);
    let retry_ms = net::RETRY_FIRST_NS / 1_000_000;
    // Already transmitted slots can precede OFFLINE on USB. Count every actual present
    // until the reopen, including any complete bands still in those slots.
    while picture_band(&mut b, panel, &whole, row, loss_ms + retry_ms) { row += 2; }
    assert_eq!(b.var("pic_epoch"), 1, "Pictures observed the lost ready period");
    assert_eq!(b.var("pic_row"), row, "the firmware cursor is the first row the panel has not presented");
    let reopen_by = b.ms + 100;
    let second = server.ready(&mut b, reopen_by);
    assert_ne!(second, first, "the resumed request uses a new connection");
    let tail = server.get(&mut b, second, 0, row);
    let resume = row;
    while row < resume + 32 {
        let by = b.ms + 80;
        assert!(picture_band(&mut b, panel, &tail, row, by), "the resumed picture paints row {row}");
        row += 2;
    }
    picture_rows(&b, panel, &whole, 0..row);
    b.machine.take_bus(&mut bus);
    // Each present sends two four-byte window commands, then the RGB565 pixel payload.
    let writes: Vec<_> = bus.iter().filter_map(|e| match e {
        fab_elec_mcu_emu::board::BusEvent::Spi { len, .. } if *len > 4 => Some(*len), _ => None,
    }).collect();
    assert_eq!(writes, vec![2 * 368 * 2; row / 2], "the panel received exactly one pixel transfer per band across both connections");
    assert!(b.link.faults.is_empty(), "boot, loss and resumed painting have no fault: {:?}", b.link.faults);

    assert!(b.machine.host_input(&Stimulus::Touch { part: touch, x: 120, y: 140, down: true }), "the CST820 accepts the press");
    b.wait_for(b.ms + 100, "the tap selects the next picture", |b| b.var("pic_index") == 1);
    assert!(b.machine.host_input(&Stimulus::Touch { part: touch, x: 120, y: 140, down: false }), "the CST820 accepts the release");
    let next = server.get(&mut b, second, 1, 0);
    assert_ne!(&whole[16..16 + 736], &next[16..16 + 736], "the first fitted rows distinguish the two pictures");
    // On an ordered connection the old response still drains first, and the page drops its
    // bands. Stop at the new picture's first band.
    let parts_left = ((448 - row + 2) * 736).div_ceil(net::NET_PART);
    // One full part takes 24 USB packets plus framing, a slot credit and the short flush.
    b.wait_for(b.ms + 28 * parts_left as u64 + 100, "the next picture presents its first two rows",
        |b| b.var("pic_row") >= 2);
    picture_rows(&b, panel, &next, 0..2);
    b.run_to(b.ms + 2, None);
    assert!(b.link.host.request().is_none(), "one tap sends just one get for picture one");
    assert!(b.link.faults.is_empty(), "the full picture run has no fault: {:?}", b.link.faults);
}

// Talk's text: 22 cells of 16 x 28 across from x 8, five lines from y 8 above the bar and six
// from y 272 below it, each cell the atlas's glyph for its Latin-1 character (127, the box, for
// anything else).
fn text_top(line: usize) -> usize { if line < 5 { 8 + 28 * line } else { 272 + 28 * (line - 5) } }

fn put_line(grid: &mut [[u16; 22]], line: usize, text: &str) {
    grid[line] = [32; 22];
    for (col, ch) in text.chars().enumerate() { grid[line][col] = if (ch as u32) < 256 { ch as u16 } else { 127 }; }
}

fn text_glass(b: &VoiceBench, panel: u16, atlas: &[u16], grid: &[[u16; 22]], what: &str) {
    let Some(Part::Co5300(glass)) = b.machine.board().part(panel) else { panic!("the lowered part is the CO5300") };
    let (atlas_w, _) = fab_elec_mcu::glyph_atlas_size(16, 28);
    let [x0, y0, _, _] = GLASS;
    for (line, cells) in grid.iter().enumerate() {
        for (col, &c) in cells.iter().enumerate() {
            let (sx, sy) = (usize::from(c - 32) % 16 * 16, usize::from(c - 32) / 16 * 28);
            for y in 0..28 {
                let at = (y0 + text_top(line) + y) * 480 + x0 + 8 + col * 16;
                for x in 0..16 {
                    let pixel = glass.frame()[at + x];
                    let visible = if glass.inversion() { !pixel } else { pixel };
                    assert_eq!(visible, atlas[(sy + y) * atlas_w as usize + sx + x],
                        "{what}: line {line}, column {col} shows {:?} (pixel {x}, {y})", char::from_u32(u32::from(c)));
                }
            }
        }
    }
}

// The test is the voice server: it opens connection one and takes what the page sends.
fn serve_talk(b: &mut VoiceBench, gen: &mut Option<u32>) {
    while let Some((_, request)) = b.link.host.request() {
        match request {
            net::Request::Open { c: 1, gen: g } => {
                *gen = Some(g);
                b.link.host.arrive(0, net::Arrival::Opened { c: 1, gen: g });
            }
            net::Request::Send { c: 1, gen: g, bytes, .. } => b.link.host.arrive(0, net::Arrival::Taken { c: 1, gen: g, bytes: bytes.len() as u32 }),
            _ => panic!("Talk only opens and sends on connection one"),
        }
    }
}

fn run_serving(b: &mut VoiceBench, gen: &mut Option<u32>, for_ms: u64) {
    let until = b.ms + for_ms;
    while b.ms < until {
        b.run_to(b.ms + 1, None);
        serve_talk(b, gen);
    }
}

#[test]
fn talk_shows_the_transcript_and_the_reply_from_the_glyph_atlas() {
    let (mut b, ()) = VoiceBench::build("talk", EchoPoll::BoardTime, Vec::new(), |_, _| {
        (net::World::new(0, vec![net::Endpoint { responder: net::Responder::Sink }, net::Endpoint { responder: net::Responder::Upstream }], Vec::new()), ())
    });
    let panel = (0..=u16::MAX).map_while(|i| b.machine.board().part(i).map(|p| (i, p)))
        .find(|(_, p)| matches!(p, Part::Co5300(_))).expect("the handheld has its panel").0;
    let atlas = fab_elec_mcu::glyph_atlas(16, 28);
    let mut open = None;
    while open.is_none() || b.var("meter_shown") == 0 {
        // The voice in the mic track starts at 1 s and its take ends after 3 s: the text is done by then.
        assert!(b.ms < 900, "Talk opens connection one and shows its bar by 900 ms");
        run_serving(&mut b, &mut open, 1);
    }
    let gen = open.expect("connection one is open");

    // The transcript wraps before the word that runs over and draws a box for the euro sign.
    // The reply is 14 lines of two ten-letter words, so its region scrolls, and its e-acute
    // straddles the end of the device's 256-byte line buffer.
    let words: Vec<String> = (0..28u8).map(|k| {
        let c = (if k < 26 { b'a' + k } else { b'A' + k - 26 }) as char;
        if k == 22 { format!("{}é", c.to_string().repeat(9)) } else { c.to_string().repeat(10) }
    }).collect();
    let say = format!("say {}", words.join(" "));
    assert_eq!(&say.as_bytes()[255..257], "é".as_bytes(), "the reply's e-acute straddles the 256-byte buffer");
    for line in ["heard what time is it in Köln 5 € ok", &say] {
        b.link.host.arrive(0, net::Arrival::Message { c: 1, gen, binary: false, bytes: line.as_bytes().to_vec() });
    }
    let by = b.ms + 200;
    while b.var("said") == 0 {
        assert!(b.ms < by, "both lines come down by {by} ms");
        run_serving(&mut b, &mut open, 1);
    }
    assert_eq!(b.var("heard_texts"), 1, "the transcript line was read whole");
    let mut grid = [[32u16; 22]; 11];
    put_line(&mut grid, 0, "what time is it in");
    put_line(&mut grid, 1, "Köln 5 \u{7f} ok");
    for (row, pair) in words.chunks(2).skip(8).enumerate() { put_line(&mut grid, 5 + row, &format!("{} {}", pair[0], pair[1])); }
    // A full page of changed cells draws within 150 ms, 48 cells per 20 ms.
    run_serving(&mut b, &mut open, 150);
    text_glass(&b, panel, &atlas, &grid, "the transcript and the reply's last six lines");

    b.link.host.arrive(0, net::Arrival::Message { c: 1, gen, binary: false, bytes: b"error 0 model-unlisted: no hub lists the chat model".to_vec() });
    let by = b.ms + 200;
    while b.var("failed") == 0 {
        assert!(b.ms < by, "the error comes down by {by} ms");
        run_serving(&mut b, &mut open, 1);
    }
    run_serving(&mut b, &mut open, 150);
    // The reason starts a line of its own and wraps at the space after a full line.
    grid.copy_within(6..11, 5);
    grid.copy_within(6..11, 5);
    put_line(&mut grid, 9, "model-unlisted: no hub");
    put_line(&mut grid, 10, "lists the chat model");
    text_glass(&b, panel, &atlas, &grid, "an error's code and phrase on lines of their own below the reply");
    assert!(b.link.faults.is_empty(), "drawing the text never faults: {:?}", b.link.faults);
}
