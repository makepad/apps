//! Real image, I2C controller and QMI8658C; no simulated firmware path.
use std::sync::Arc;

use fab_elec_mcu::{compile, Compiled};
use fab_elec_mcu_emit_s3::{build_image, build_slot, chip, Board, BoardTime, Image,
    SlotOptions, SLOT_BASE, VOICEBOX, VOICEBOX_I2C, VOICEBOX_MOTION};
use fab_elec_mcu_emu::{board::{BusEvent, I2cLine, I2cTiming, I2cWhat}, HaltKind, Lockstep, Machine, PcSet, Pull, Ran, Status, Until};
use fab_elec_mcu_parts::{BoardDesc, NetDesc, Netlist, PartDesc, PartKind, PinRef, Stimulus};
use fab_elec_mcu_rt::{module, sched::{Device, Fault, Hal, Role}, serial, watch};
use fab_elec_mcu_s3::{CpuClock, PinsProfile, S3Machine, S3Profile, TimeProfile};

fn profile() -> S3Profile {
    S3Profile {
        mac: [2, 0, 0, 0, 0, 1], pvt: fab_elec_mcu_s3::NO_PVT, flash_bytes: VOICEBOX.flash_bytes as usize,
        handoff_cpu: CpuClock::Xtal { div: 2 }, handoff_mapped_pages: 64,
        entry: 0x4200_0408, seed: 0x8658,
        time: TimeProfile {
            systimer_first_long: true, rc_slow_hz: (136_000, 1), wdt_delay_sel: 0,
            swd_timeout_ticks: 432_000_000, swd_request_lead_ticks: 48_000_000,
            wdt_sync_cycles: 2, mwdt_rephase_on_feed: false, mwdt_rephase_on_config: true,
            wdt_restart_on_enable: true,
        },
        pins: PinsProfile {
            power_on_gpio0_high: true, usb_service_ticks: 480_000,
            usb_frame_phase_ticks: 0, input_sync_delay_ticks: 0,
            handoff_digital_pads_unheld: true, handoff_out: [0; 2], handoff_enable: [0; 2],
            handoff_pin: [0; 45], handoff_io_mux: [1 << 12; 45], handoff_out_sel: [256; 45],
            handoff_usb_tx: Vec::new(), handoff_usb_tx_flushed: false,
            handoff_usb_rx: Vec::new(), handoff_usb_int_raw: 8, handoff_usb_int_ena: 0,
        },
    }
}

fn netlist(addr: u8, rise_ps: Option<u64>) -> Netlist {
    Netlist::new(BoardDesc {
        seed: 0x8658,
        parts: vec![PartDesc { refdes: "U5".into(), kind: PartKind::Qmi8658c { addr } }],
        nets: vec![
            NetDesc { rise_ps, name: "SDA".into(), pull: Pull::Up, pins: vec![PinRef::Chip("GPIO15".into()), PinRef::Part { part: 0, pin: "SDA".into() }] },
            NetDesc { rise_ps, name: "SCL".into(), pull: Pull::Up, pins: vec![PinRef::Chip("GPIO14".into()), PinRef::Part { part: 0, pin: "SCL".into() }] },
            NetDesc { rise_ps: None, name: "INT1".into(), pull: Pull::None, pins: vec![PinRef::Part { part: 0, pin: "INT1".into() }] },
            NetDesc { rise_ps: None, name: "INT2".into(), pull: Pull::None, pins: vec![PinRef::Part { part: 0, pin: "INT2".into() }] },
        ],
    }).expect("shared-bus IMU wiring")
}

fn program(period: u32) -> Compiled {
    let source = format!(r#"
let data = words(48)
var count: i32 = 0
var outside: i32 = 0
var before: i32 = 1
var bad: i32 = 1
var scale_a: i32 = 0
var scale_g: i32 = 0
let peek = every(100000, fn() {{ outside = accel(0) }})
let imu = motion({period}, fn() {{
    if count < 8 {{
        let at = count * 6
        data[at] = accel(0)
        data[at + 1] = accel(1)
        data[at + 2] = accel(2)
        data[at + 3] = gyro(0)
        data[at + 4] = gyro(1)
        data[at + 5] = gyro(2)
        count += 1
    }}
}})
fn setup() {{
    before = accel(0) | gyro(2)
    bad = gyro(3) | accel(-1)
    scale_a = ACCEL_LSB_PER_G
    scale_g = GYRO_LSB_PER_DPS
}}
"#);
    compile(&source, &VOICEBOX_MOTION.profile(), &[], &[]).expect("motion program compiles")
}

fn words(m: &S3Machine<Netlist>, at: u32, n: usize) -> Vec<u32> {
    let mut bytes = vec![0; n * 4];
    assert!(m.read_data(at, &mut bytes), "runtime words are readable");
    bytes.chunks_exact(4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).collect()
}

struct Rig {
    image: Image,
    // The decoded tier in lockstep with the reference: every run must agree in full.
    machine: Lockstep<S3Machine<Netlist>>,
    bus: Vec<BusEvent>,
    reader: watch::Reader,
    frames: Vec<watch::Frame>,
}

impl Rig {
    fn new(c: &Compiled, addr: u8) -> Self {
        Self::with_rise(c, addr, None)
    }

    fn with_rise(c: &Compiled, addr: u8, rise_ps: Option<u64>) -> Self {
        let p = profile();
        let language = VOICEBOX_MOTION.profile();
        let m = module::load(&c.module, &language).expect("motion module verifies");
        let board = Board { layout: VOICEBOX,
            time: BoardTime { rc_slow_hz: p.time.rc_slow_hz, wdt_delay_sel: p.time.wdt_delay_sel },
            panel: None, i2c: Some(VOICEBOX_I2C), motion: Some(VOICEBOX_MOTION), touch: None, radio: None };
        let image = build_image(&m, &board, &SlotOptions::default()).expect("motion image links");
        let machine = Lockstep::new(S3Machine::with_board(Arc::new(image.flash.clone()), &p, netlist(addr, rise_ps)).expect("IMU board binds"));
        Self { image, machine, bus: Vec::new(), reader: watch::Reader::default(), frames: Vec::new() }
    }

    fn take(&mut self) {
        self.machine.take_bus(&mut self.bus);
        let mut out = Vec::new(); self.machine.take_out(&mut out);
        for byte in out {
            if let Some(frame) = self.reader.push(byte.byte) { self.frames.push(frame.expect("valid watch frame")); }
        }
        self.machine.mark();
        assert!(self.machine.last_reset().is_none(), "I2C polls keep the watchdog fed");
        let status = self.machine.status();
        assert!(!matches!(status, Status::Halted(_)), "no unsupported model access: {status:?}");
    }

    fn run(&mut self, tick: u64, pc: Option<u32>) {
        let stops: Vec<_> = pc.into_iter().collect();
        let mut marks = PcSet::new(SLOT_BASE, self.image.slot.bytes.len() as u32);
        for &at in &stops { marks.insert(at); }
        loop {
            let result = self.machine.run(&Until { tick, marks: &marks, stops: &stops,
                stops_after: self.machine.now(), on_drive: false });
            self.take();
            match result {
                Ran::Barrier => (),
                Ran::Pc if pc == Some(self.machine.pc()) => return,
                Ran::Tick if pc.is_none() => return,
                other => panic!("bounded motion run did not reach its target: {other:?}"),
            }
        }
    }

    fn until_pc(&mut self, pc: u32) {
        self.run(self.machine.now() + 3 * self.machine.tick_hz(), Some(pc));
    }

    fn sample(&mut self) -> u64 {
        self.until_pc(self.image.slot.symbols.motion_start);
        let before = self.machine.cpu().instret;
        self.until_pc(self.image.slot.symbols.motion_end);
        let cost = self.machine.cpu().instret - before;
        self.until_pc(self.image.slot.symbols.handler_return);
        cost
    }

    fn input(&mut self, accel_ug: [i32; 3], gyro_mdps: [i32; 3]) {
        self.machine.both(|m| m.board_in(Stimulus::Motion { part: 0, accel_ug, gyro_mdps }));
    }

    fn var(&self, c: &Compiled, name: &str) -> u32 {
        let offset = c.lowered.state_vars.iter().find(|v| v.name == name).expect("named state").offset;
        words(&self.machine, self.image.slot.layout.state + offset * 4, 1)[0]
    }

    fn data(&self) -> Vec<u32> { words(&self.machine, self.image.slot.layout.buffers[0].address, 48) }

    // One row per STOP, including pointer writes and repeated-start reads.
    fn transactions(&self) -> Vec<Vec<(u64, u8)>> {
        let mut result = Vec::new(); let mut bytes = Vec::new();
        for event in &self.bus {
            match *event {
                BusEvent::I2c { what: I2cWhat::Byte { value, .. }, t, .. } => bytes.push((t, value)),
                BusEvent::I2c { what: I2cWhat::Stop, .. } => result.push(std::mem::take(&mut bytes)),
                _ => (),
            }
        }
        result
    }

    fn enabled_at(&self) -> u64 {
        self.transactions().iter().find(|b| b.iter().map(|b| b.1).eq([0xd6, 8, 0x83]))
            .expect("CTRL7 enables syncSmpl and both sensors")[2].0
    }
}

#[test]
fn motion_pull_up_rise_changes_the_same_images_outcome() {
    let c = program(10_000);
    let mut strong = Rig::with_rise(&c, 0x6b, Some(100_000)); // 100 ns, 48 board ticks.
    strong.input([1_000_000, -500_000, 250_000], [1_000, -2_000, 500]);
    strong.sample();
    assert_eq!(strong.var(&c, "count"), 1, "a strong pull lets the compiled motion handler read a sample");
    assert_eq!(&strong.data()[..6], &[16_384, (-8_192i32) as u32, 4_096, 128, (-256i32) as u32, 64],
        "a strong pull preserves all six motion sample values");
    assert!(!strong.bus.iter().any(|e| matches!(e, BusEvent::Rise { .. })),
        "a 100 ns rise fits both controller windows");
    let first_start = strong.bus.iter().find_map(|e| match *e {
        BusEvent::I2c { t, what: I2cWhat::Start, .. } => Some(t), _ => None,
    }).expect("the strong-pull image starts its initialization list");

    // emit_s3/src/i2c.rs at 400 kHz: DIV_NUM=0, LOW=55, HOLD=19,
    // WAIT_HIGH=11. I12 windows use the same 12-tick SCLK period.
    let timing = I2cTiming { setup: (55 - 19) * 12, allowance: (11 + 1) * 12 };
    assert_eq!((timing.setup, timing.allowance), (432, 144),
        "the 400 kHz service gives 432 setup ticks and 144 rising-allowance ticks");
    let hz = strong.machine.tick_hz();
    assert_eq!((timing.setup * 1_000_000_000 / hz, timing.allowance * 1_000_000_000 / hz), (900, 300),
        "the 400 kHz windows are 900 ns setup and 300 ns allowance at the machine tick rate");
    let mut weak = Lockstep::new(S3Machine::with_board(Arc::new(strong.image.flash.clone()), &profile(), netlist(0x6b, Some(1_000_000)))
        .expect("the identical board with a 1 us rise binds"));
    weak.both(|m| m.board_in(Stimulus::Motion { part: 0, accel_ug: [1_000_000, -500_000, 250_000], gyro_mdps: [1_000, -2_000, 500] }));
    let marks = PcSet::new(SLOT_BASE, strong.image.slot.bytes.len() as u32);
    let mut records = Vec::new();
    loop {
        let result = weak.run(&Until { tick: 3 * hz, marks: &marks, stops: &[], stops_after: 0, on_drive: false });
        weak.take_bus(&mut records);
        match result {
            Ran::Barrier => weak.mark(),
            Ran::Halted => break,
            other => panic!("the weak pull must stop at its first list: {other:?}"),
        }
    }
    let Status::Halted(halt) = weak.status() else { panic!("the weak-pull machine must retain its halt") };
    assert_eq!((halt.kind, halt.what), (HaltKind::Unmodeled,
        "an I2C line rises slower than the controller's window; see the rise record"),
        "the weak pull stops with the specified static reason");
    let rises: Vec<_> = records.iter().copied().filter(|e| matches!(e, BusEvent::Rise { .. })).collect();
    assert_eq!(rises, [BusEvent::Rise {
        t: first_start, net: weak.board().net("SCL").expect("SCL is named"), line: I2cLine::Scl,
        rise_ticks: 480, window_ticks: timing.allowance,
    }], "take_bus after the halt retains exactly one SCL Rise at the first TRANS_START's board step");
    assert!(!records.iter().any(|e| matches!(e, BusEvent::I2c { .. })),
        "the weak pull stops before the first I2C START reaches the target");
    eprintln!("motion 400 kHz rise windows: setup {} ticks / 900 ns, allowance {} ticks / 300 ns", timing.setup, timing.allowance);
}

#[derive(Default)]
struct Reference { faults: Vec<Fault> }
impl Hal for Reference {
    fn eeprom_read(&mut self, _: u64, _: u16, _: u32) -> u8 { unreachable!("this profile has no EEPROM") }
    fn eeprom_write(&mut self, _: u64, _: u16, _: u8, _: u32) { unreachable!("this profile has no EEPROM") }
    fn configure(&mut self, _: u64, _: u32, _: Role) { panic!("no GPIO"); }
    fn serial(&mut self, _: u64, _: u32, _: serial::SerialSetting) { panic!("no serial"); }
    fn write(&mut self, _: u64, _: u32, _: bool, _: u32) { panic!("no GPIO"); }
    fn duty(&mut self, _: u64, _: u32, _: u8, _: u32) { panic!("no PWM"); }
    fn read(&mut self, _: u64, _: u32, _: u32) -> bool { panic!("no GPIO"); }
    fn adc(&mut self, _: u64, _: u32, _: u32) -> u16 { panic!("no ADC"); }
    fn eth_send(&mut self, _: u64, _: &[u8], _: u32) -> bool { false }
    fn eth_mac(&mut self, _: u64, _: u32) -> [u8; 6] { [0; 6] }
    fn shift(&mut self, _: u64, _: u32, _: u32, _: u32, _: u8, _: u32) { unreachable!("this fixture shifts nothing") }
    fn clock(&mut self, t: u64, _: u32) -> u64 { t }
    fn fault(&mut self, _: u64, fault: Fault) { self.faults.push(fault); }
}

#[test]
fn motion_samples_follow_odr_and_match_reference_counts() {
    let c = program(10_000); let mut r = Rig::new(&c, 0x6b);
    let mut costs = vec![r.sample()];
    let hz = r.machine.tick_hz(); let us = hz / 1_000_000;
    let enabled = r.enabled_at();
    let a_first = enabled + 27_000 * us; // 3 ms + 3/125 Hz, lines 2115–2119.
    let g_first = enabled + 84_000 * us; // 60 ms + 3/125 Hz, lines 2110–2114.
    let odr = 8_000 * us;
    // The last gyro update before the second handler's status lock (one
    // 10 ms period after the first): that lock must still see the
    // pre-stimulus gyro generation.
    let first_lock = r.transactions().iter().filter(|b| b.len() == 4 && b[1].1 == 0x2e)
        .map(|b| b[3].0).last().expect("the first handler locks STATUS0");
    let update = g_first + (first_lock + 10_000 * us - 1 - g_first) / odr * odr;
    assert!(update > r.machine.now(), "the chosen gyro update follows the first handler");
    r.run(update + 1, None);
    let stimulus = r.machine.now();
    r.input([1_000_000, -500_000, 250_000], [1_000, -2_000, 500]);
    for _ in 1..8 { costs.push(r.sample()); }
    let tx = r.transactions();
    assert!(tx[0][0].0 >= 1_750_000 * us, "first addressed access follows power-on startup");
    let init: Vec<_> = tx[..6].iter().map(|b| b.iter().map(|b| b.1).collect::<Vec<_>>()).collect();
    assert_eq!(init, [vec![0xd6, 0x60, 0xb0], vec![0xd6, 2, 0x40], vec![0xd6, 0x4d, 0xd7, 0x80],
        vec![0xd6, 3, 6], vec![0xd6, 4, 0x46], vec![0xd6, 8, 0x83]],
        "soft reset, little-endian auto-increment, the reset's 0x80 answer, scales, lowest qualifying 125 Hz ODR and synchronized enable");
    assert!(tx[1][0].0 >= tx[0][2].0 + 16_000 * us, "the first access after the reset waits 16 ms, past its 15.2 ms");
    let statuses: Vec<_> = tx.iter().filter(|b| b.len() == 4 && b[1].1 == 0x2e).map(|b| b[3].0).collect();
    assert_eq!(statuses.len(), 8, "one status lock per handler");
    let samples: Vec<[i16; 6]> = statuses.iter().map(|&at| {
        let a_new = a_first + (at - a_first) / odr * odr > stimulus;
        let g_new = g_first + (at - g_first) / odr * odr > stimulus;
        let a = if a_new { [16_384, -8_192, 4_096] } else { [0, 0, 16_384] };
        let g = if g_new { [128, -256, 64] } else { [0; 3] };
        [a[0], a[1], a[2], g[0], g[1], g[2]]
    }).collect();
    assert_eq!(samples[0], [0, 0, 16_384, 0, 0, 0], "first handler predates the stimulus");
    assert!(statuses[1] > stimulus, "the second status lock follows the stimulus");
    assert_eq!(&samples[1][3..], &[0; 3], "a stimulus just after an ODR update remains stale until the next update");
    assert_eq!(samples[7], [16_384, -8_192, 4_096, 128, -256, 64], "settled stimulus uses documented sensitivities");
    let expected: Vec<_> = samples.iter().flatten().map(|&v| v as i32 as u32).collect();
    assert_eq!(r.data(), expected, "each sensor changes only after its next ODR, never at stimulus arrival");
    let language = VOICEBOX_MOTION.profile();
    let mut reference = Device::new(module::load(&c.module, &language).unwrap(), &language);
    let mut hal = Reference::default(); reference.boot(&mut hal);
    for sample in samples { assert!(reference.motion_in(sample)); reference.step(&mut hal).expect("motion reference event"); }
    assert_eq!(reference.buffers()[0], r.data(), "identical raw samples have bit-identical handler effects");
    assert_eq!(hal.faults.len(), 2, "both bad axes fault in the reference");
    r.run(r.machine.now() + 100_000 * us, None);
    assert_eq!(r.var(&c, "before"), 0, "ports return zero before the first sample");
    assert_eq!(r.var(&c, "bad"), 0, "invalid positive and negative axes return zero");
    assert_eq!(r.var(&c, "outside"), 16_384, "ordinary timer reads the newest latched sample");
    assert_eq!((r.var(&c, "scale_a"), r.var(&c, "scale_g")), (16_384, 128), "generated constants match configured ranges");
    let faults: Vec<_> = r.frames.iter().filter_map(|f| match f.event { watch::Event::Fault { code, arg, .. } => Some((code, arg)), _ => None }).collect();
    assert_eq!(faults, [(watch::FAULT_RANGE, 3), (watch::FAULT_RANGE, -1)], "normal motion has no bus or overrun fault");
    eprintln!("motion 400 kHz: retired instructions per STATUS0 + six-axis sample: {costs:?}");
}

#[test]
fn locked_runtime_burst_crosses_updates_and_overruns_coalesce_without_lost_faults() {
    let c = program(125); let mut r = Rig::new(&c, 0x6b);
    r.input([1_000_061; 3], [1_000; 3]);
    r.until_pc(r.image.slot.symbols.motion_start);
    let us = r.machine.tick_hz() / 1_000_000;
    let begin = r.machine.now(); let retired = r.machine.cpu().instret;
    let timer = r.image.slot.layout.motion.as_ref().unwrap().timer;
    let due = |r: &Rig| { let w = words(&r.machine, timer, 2); u64::from(w[0]) | (u64::from(w[1]) << 32) };
    let mut previous = due(&r);
    // STATUS0 completes at about 100 us; axes occupy the subsequent burst.
    // Explicit bus assertions below ensure the stimulus is inside that lock.
    r.run(begin + 140 * us, None); let stimulus = r.machine.now();
    r.input([500_000; 3], [-1_000; 3]);
    r.until_pc(r.image.slot.symbols.motion_end);
    let cost = r.machine.cpu().instret - retired;
    r.until_pc(r.image.slot.symbols.handler_return);
    let tx = r.transactions();
    let status = tx.iter().find(|b| b.len() == 4 && b[1].1 == 0x2e).unwrap();
    let axes = tx.iter().find(|b| b.len() == 15 && b[1].1 == 0x35).unwrap();
    let g_first = r.enabled_at() + 60_375 * us;
    let update = g_first + ((stimulus - g_first) / (125 * us) + 1) * (125 * us);
    assert!(status[3].0 < stimulus && axes[3].0 < update && update < axes[14].0,
        "an ODR publication of changed motion falls during the actual axis burst");
    assert_eq!(&r.data()[..6], &[16_385, 16_385, 16_385, 128, 128, 128], "the lock preserves one generation across that update");
    let mut missed = Vec::new();
    for _ in 1..8 {
        r.until_pc(r.image.slot.symbols.motion_start);
        let next = due(&r);
        let count = (next - previous) / (125 * 16) - 1;
        assert!(count > 0, "a transfer longer than the period coalesces overdue firings");
        missed.push(count as i32); previous = next;
        r.until_pc(r.image.slot.symbols.handler_return);
    }
    assert_eq!(r.var(&c, "count"), 8, "eight successful samples finish despite overload");
    let new: Vec<u32> = [8_192, 8_192, 8_192, -128i32, -128, -128].into_iter().map(|v| v as u32).collect();
    for sample in r.data()[6..].chunks_exact(6) { assert_eq!(sample, new, "later bursts refresh after the lock is released"); }
    // Keep producers running long enough for complete USB packets containing
    // every fault in the observed prefix to leave. No host-side SRAM mutation.
    r.run(r.machine.now() + 5_000 * us, None);
    let faults: Vec<_> = r.frames.iter().filter_map(|f| match f.event {
        watch::Event::Fault { code: watch::FAULT_OVERRUN, arg, site: 1 } => Some(arg), _ => None,
    }).collect();
    assert!(faults.len() >= missed.len(), "all observed coalescings reach the watch stream");
    assert_eq!(&faults[..missed.len()], missed, "fault counts exactly match timer-grid advances");
    assert_eq!(words(&r.machine, r.image.slot.layout.lost_count, 1), [0], "no fault enqueue was lost");
    assert!(!r.frames.iter().any(|f| matches!(f.event, watch::Event::Lost { .. })), "no hidden loss record");
    assert!(!r.frames.iter().any(|f| matches!(f.event, watch::Event::Fault { code: watch::FAULT_I2C_NACK | watch::FAULT_I2C_TIMEOUT, .. })), "overload is a timer fault, not a bus-rate refusal");
    eprintln!("motion at 125 us: {cost} retired instructions for the first coherent sample; coalesced counts {missed:?}");
}

#[test]
fn motion_refusals_and_missing_sensor_report_named_fault() {
    use fab_elec_mcu_rt::ports;
    let (forms, ports_) = (fab_elec_mcu::FORMS, fab_elec_mcu::PORTS);
    assert_eq!((ports::FORM_MOTION, ports::ACCEL, ports::GYRO, ports::PORT_TABLE_VERSION), (14, 19, 20, 15));
    assert_eq!((forms[ports::FORM_MOTION as usize].name, ports_[ports::ACCEL as usize].name, ports_[ports::GYRO as usize].name),
        ("motion", "accel", "gyro"), "the front end's motion form and ports are the runtime's numbers");
    let language = VOICEBOX_MOTION.profile();
    for source in ["let imu = motion(0, fn() {})", "let a = motion(10000, fn() {})\nlet b = motion(10000, fn() {})", "fn setup() { let v = accel(0) }"] {
        assert!(compile(source, &language, &[], &[]).is_err(), "invalid motion declaration/port must refuse: {source}");
    }
    assert!(compile("let imu = motion(10000, fn() {})", &chip::profile(), &[], &[]).is_err(), "profile without motion refuses at load");
    let c = program(10_000); let m = module::load(&c.module, &language).unwrap();
    let no_sensor = SlotOptions::default();
    assert!(build_slot(&m, &no_sensor).err().expect("absent board IMU refuses").message.contains("no motion input"));
    let fast = program(124); let m = module::load(&fast.module, &language).unwrap();
    let options = SlotOptions { i2c: Some(VOICEBOX_I2C), motion: Some(VOICEBOX_MOTION), ..SlotOptions::default() };
    assert!(build_slot(&m, &options).err().expect("ODR cannot exceed 8000 Hz").message.contains("8000 Hz"));
    let mut r = Rig::new(&c, 0x6a);
    r.run(3 * r.machine.tick_hz(), None);
    let faults: Vec<_> = r.frames.iter().filter_map(|f| match f.event {
        watch::Event::Fault { code: watch::FAULT_I2C_NACK, site, arg } => Some((site, arg)), _ => None,
    }).collect();
    assert_eq!(faults, [(0x6b, 0x60)], "missing IMU reports its address and the reset's NACK once");
    assert_eq!(r.var(&c, "count"), 0, "failed initialization never invokes motion");
    assert_eq!(r.data(), [0; 48], "failed initialization never latches a sample");
}
