//! Real image, shared I2C controller and CST820; host input uses board_in.
use std::sync::Arc;

use fab_elec_mcu::{compile, Compiled};
use fab_elec_mcu_emit_s3::{build_image, build_slot, chip, Board, BoardTime, Image,
    SlotOptions, Touch, SLOT_BASE, VOICEBOX, VOICEBOX_I2C, VOICEBOX_MOTION, VOICEBOX_TOUCH};
use fab_elec_mcu_emu::{board::{BusEvent, I2cWhat}, Lockstep, Machine, PcSet, Pull, Ran, Status, Tier, Until};
use fab_elec_mcu_parts::{BoardDesc, NetDesc, Netlist, PartDesc, PartKind, PinRef, Stimulus};
use fab_elec_mcu_rt::{module, ports, sched::{Device, Fault, Hal, Role, TouchReport}, serial, watch};
use fab_elec_mcu_s3::{CpuClock, PinsProfile, S3Machine, S3Profile, TimeProfile};

fn profile() -> S3Profile {
    S3Profile {
        mac: [2, 0, 0, 0, 0, 1], pvt: fab_elec_mcu_s3::NO_PVT, flash_bytes: VOICEBOX.flash_bytes as usize,
        handoff_cpu: CpuClock::Xtal { div: 2 }, handoff_mapped_pages: 64,
        entry: 0x4200_0408, seed: 0x820,
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

fn netlist(addr: u8) -> Netlist {
    // R01/R04: SDA15/SCL14 with pull-ups; R02: TP_RESET is left an input.
    // R05: INT has a pull-up. RST pull-up supplies the test's inactive reset.
    // T01/O04: 0x15 and CST816-family behaviour are Assumed for CST820.
    Netlist::new(BoardDesc {
        seed: 0x820,
        parts: vec![PartDesc { refdes: "TP".into(), kind: PartKind::Cst820 { addr } }],
        nets: vec![
            NetDesc { rise_ps: None, name: "SDA".into(), pull: Pull::Up, pins: vec![PinRef::Chip("GPIO15".into()), PinRef::Part { part: 0, pin: "SDA".into() }] },
            NetDesc { rise_ps: None, name: "SCL".into(), pull: Pull::Up, pins: vec![PinRef::Chip("GPIO14".into()), PinRef::Part { part: 0, pin: "SCL".into() }] },
            NetDesc { rise_ps: None, name: "INT".into(), pull: Pull::Up, pins: vec![PinRef::Chip("GPIO21".into()), PinRef::Part { part: 0, pin: "INT".into() }] },
            NetDesc { rise_ps: None, name: "RST".into(), pull: Pull::Up, pins: vec![PinRef::Part { part: 0, pin: "RST".into() }] },
        ],
    }).expect("shared-bus touch wiring")
}

fn program(period: u32) -> Compiled {
    compile(&format!(r#"
let data = words(24)
var count: i32 = 0
var before: i32 = 1
var outside_x: i32 = 0
var outside_y: i32 = 0
var outside_down: i32 = 0
let peek = every(5000, fn() {{
    outside_x = touch_x()
    outside_y = touch_y()
    outside_down = touch_down()
}})
let finger = touch({period}, fn() {{
    if count < 8 {{
        let at = count * 3
        data[at] = touch_x()
        data[at + 1] = touch_y()
        data[at + 2] = touch_down()
        count += 1
    }}
}})
fn setup() {{ before = touch_x() | touch_y() | touch_down() }}
"#), &VOICEBOX_TOUCH.profile(), &[], &[]).expect("touch program compiles")
}

fn words(m: &S3Machine<Netlist>, at: u32, n: usize) -> Vec<u32> {
    let mut bytes = vec![0; n * 4];
    assert!(m.read_data(at, &mut bytes), "runtime words are readable");
    bytes.chunks_exact(4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).collect()
}

struct Rig {
    image: Image,
    // Tiers alternating from run to run, in lockstep with the reference: every run must agree in full.
    machine: Lockstep<S3Machine<Netlist>>,
    bus: Vec<BusEvent>,
    reader: watch::Reader,
    frames: Vec<watch::Frame>,
    reports: Vec<TouchReport>,
    costs: Vec<u64>,
}

impl Rig {
    fn new(c: &Compiled, addr: u8) -> Self {
        let p = profile(); let language = VOICEBOX_TOUCH.profile();
        let m = module::load(&c.module, &language).expect("touch module verifies");
        let board = Board { layout: VOICEBOX,
            time: BoardTime { rc_slow_hz: p.time.rc_slow_hz, wdt_delay_sel: p.time.wdt_delay_sel },
            panel: None, i2c: Some(VOICEBOX_I2C), motion: None, touch: Some(VOICEBOX_TOUCH), radio: None };
        let image = build_image(&m, &board, &SlotOptions::default()).expect("touch image links");
        // Native page units (compiled at once) and the reference, alternating run by run.
        let mut machine = Lockstep::alternating(S3Machine::with_board(Arc::new(image.flash.clone()), &p, netlist(addr)).expect("touch board binds"));
        fab_elec_mcu_emu::jit::set_inline_compile(true);
        machine.set_tier(Tier::Native);
        Self { image, machine, bus: Vec::new(), reader: watch::Reader::default(), frames: Vec::new(), reports: Vec::new(), costs: Vec::new() }
    }

    fn take(&mut self) {
        self.machine.take_bus(&mut self.bus);
        let mut out = Vec::new(); self.machine.take_out(&mut out);
        for byte in out {
            if let Some(frame) = self.reader.push(byte.byte) { self.frames.push(frame.expect("valid watch frame")); }
        }
        self.machine.mark();
        assert!(self.machine.last_reset().is_none(), "touch transfers keep the watchdog fed");
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
                other => panic!("bounded touch run did not reach its target: {other:?}"),
            }
        }
    }

    fn until_pc(&mut self, pc: u32) { self.run(self.machine.now() + self.machine.tick_hz() / 2, Some(pc)); }
    fn us(&self) -> u64 { self.machine.tick_hz() / 1_000_000 }
    fn input(&mut self, x: u16, y: u16, down: bool) { self.machine.both(|m| m.board_in(Stimulus::Touch { part: 0, x, y, down })); }

    fn var(&self, c: &Compiled, name: &str) -> u32 {
        let offset = c.lowered.state_vars.iter().find(|v| v.name == name).expect("named state").offset;
        words(&self.machine, self.image.slot.layout.state + offset * 4, 1)[0]
    }

    fn data(&self) -> Vec<u32> { words(&self.machine, self.image.slot.layout.buffers[0].address, 24) }

    // One row per STOP, including address and register-pointer bytes.
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

    fn poll(&mut self) {
        self.until_pc(self.image.slot.symbols.touch_start);
        let before = self.machine.cpu().instret;
        let old_transactions = self.transactions().len();
        self.until_pc(self.image.slot.symbols.touch_end);
        self.costs.push(self.machine.cpu().instret - before);
        let tx = self.transactions(); let poll = &tx[old_transactions..];
        assert_eq!(poll[0].iter().take(3).map(|b| b.1).collect::<Vec<_>>(), [0x2a, 0x02, 0x2b], "T03: each poll begins with FingerNum alone");
        assert_eq!(poll[0].len(), 4, "only one count byte is read");
        let down = poll[0][3].1 != 0;
        let (x, y) = if down {
            assert_eq!(poll.len(), 2, "down uses a separate coordinate transaction");
            assert_eq!(poll[1].len(), 7, "T04-T07/T15: four coordinate bytes, never register 0x07");
            assert_eq!(poll[1][1].1, 0x03, "coordinate burst starts at XposH");
            let bytes: Vec<_> = poll[1][3..].iter().map(|b| b.1).collect();
            (u16::from_be_bytes([bytes[0], bytes[1]]), u16::from_be_bytes([bytes[2], bytes[3]]))
        } else {
            assert_eq!(poll.len(), 1, "idle and release never read coordinates");
            // Deliberately unrelated release coordinates prove the reference
            // keeps the last delivered x/y rather than this next input's x/y.
            (999, 888)
        };
        self.reports.push(TouchReport { down, x, y });
        self.until_pc(self.image.slot.symbols.dispatch);
    }

    fn change(&mut self, c: &Compiled, count: u32) {
        for _ in 0..4 {
            self.poll();
            if self.var(c, "count") == count { return; }
        }
        panic!("touch change {count} must be delivered within four polls");
    }

    fn compare_reference(&self, c: &Compiled) {
        let language = VOICEBOX_TOUCH.profile();
        let mut device = Device::new(module::load(&c.module, &language).unwrap(), &language);
        let mut hal = Reference::default(); device.boot(&mut hal);
        for &report in &self.reports {
            assert!(device.touch_in(report), "declared reference accepts the next sampled report");
            loop {
                let ran = device.step(&mut hal).expect("reference timers stay armed");
                if ran.timer == Some(1) { break; }
            }
        }
        // The ordinary 5 ms timer runs before the next touch event in these
        // scenarios, exposing the same latch outside its handler.
        assert_eq!(device.step(&mut hal).unwrap().timer, Some(0), "outside read follows the touch event");
        assert_eq!(device.buffers()[0], self.data(), "reference and S3 deliver exactly the same reports");
        for name in ["before", "count", "outside_x", "outside_y", "outside_down"] {
            let at = c.lowered.state_vars.iter().find(|v| v.name == name).unwrap().offset as usize;
            assert_eq!(device.state()[at], self.var(c, name), "reference and S3 agree on {name}");
        }
        assert!(hal.faults.is_empty(), "reference normal-rate polling has no fault");
    }
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
    fn shift(&mut self, _: u64, _: u32, _: u32, _: u32, _: u8, _: u32) { unreachable!("no shift"); }
    fn clock(&mut self, t: u64, _: u32) -> u64 { t }
    fn fault(&mut self, _: u64, fault: Fault) { self.faults.push(fault); }
}

#[test]
fn startup_scans_changes_and_latched_ports_match_reference() {
    let c = program(6_000); let mut r = Rig::new(&c, 0x15);
    r.poll();
    let tx = r.transactions(); let us = r.us();
    assert!(tx[0][0].0 >= 100_000 * us, "T18: first addressed access is at least 100 ms after power-on");
    let init: Vec<_> = tx[..4].iter().map(|b| b.iter().map(|b| b.1).collect::<Vec<_>>()).collect();
    assert_eq!(init, [vec![0x2a, 0xfe, 1], vec![0x2a, 0xee, 1], vec![0x2a, 0xfc, 0], vec![0x2a, 0xfb, 0]],
        "T09/T12/T14: disable autosleep, ceil(6000/10000) scan units, then no long-press or gesture auto-reset");
    assert_eq!(r.var(&c, "count"), 0, "initial idle is unchanged and invokes no handler");
    assert_eq!(r.var(&c, "before"), 0, "all three ports start at zero");
    let origin = tx[1][2].0; let scan = 10_000 * us; // T12, model's documented scan rephase.
    r.run(origin + scan + 1000 * us, None);
    r.input(12, 34, true); r.change(&c, 1);
    let next_scan = origin + ((r.machine.now() - origin) / scan + 1) * scan;
    r.input(40, 50, true);
    r.run(r.machine.now() + 1000 * us, None); r.input(60, 70, true);
    r.run(r.machine.now() + 1000 * us, None); r.input(80, 90, true);
    assert!(r.machine.now() < next_scan, "all intermediate moves fall inside one scan period");
    r.change(&c, 2); r.poll();
    assert_eq!(r.var(&c, "count"), 2, "a repeated down report is suppressed");
    r.input(110, 300, true); r.change(&c, 3);
    r.input(367, 447, false); r.change(&c, 4); r.poll();
    r.run(r.machine.now() + 5000 * us, None);
    let mut expected = vec![12, 34, 1, 80, 90, 1, 110, 300, 1, 110, 300, 0]; expected.resize(24, 0);
    assert_eq!(r.data(), expected, "only down, coalesced move, later move and release are delivered in glass pixels");
    assert_eq!((r.var(&c, "outside_x"), r.var(&c, "outside_y"), r.var(&c, "outside_down")), (110, 300, 0), "outside ports retain released coordinates");
    assert_eq!(r.transactions().len(), 4 + r.reports.iter().map(|s| if s.down { 2 } else { 1 }).sum::<usize>(), "port reads in setup and ordinary timers never start bus traffic");
    r.compare_reference(&c);
    assert!(!r.frames.iter().any(|f| matches!(f.event, watch::Event::Fault { .. })), "normal touch has no bus or timer fault");
    eprintln!("touch 400 kHz, 6000 us polls: retired instructions per count + optional coordinates + latch: {:?}", r.costs);
}

#[test]
fn short_tap_survives_equal_scan_and_poll_periods() {
    let c = program(10_000); let mut r = Rig::new(&c, 0x15);
    r.poll(); let us = r.us();
    let origin = r.transactions()[1][2].0;
    let next_scan = origin + ((r.machine.now() - origin) / (10_000 * us) + 1) * 10_000 * us;
    r.input(260, 400, true);
    r.run(r.machine.now() + 2000 * us, None); r.input(1, 2, false);
    assert!(r.machine.now() < next_scan, "both tap edges precede the next scan");
    r.change(&c, 1); r.change(&c, 2); r.poll();
    r.run(r.machine.now() + 5000 * us, None);
    assert_eq!(r.var(&c, "count"), 2, "a two-millisecond tap delivers both transitions exactly once");
    assert_eq!(&r.data()[..6], &[260, 400, 1, 260, 400, 0], "tap down and up occupy consecutive scans, preserving release coordinates");
    r.compare_reference(&c);
    assert!(!r.frames.iter().any(|f| matches!(f.event, watch::Event::Fault { .. })), "a short tap causes no fault");
    eprintln!("touch 400 kHz, 10000 us tap: retired instructions per poll: {:?}", r.costs);
    // A finger held past LongPressTime's default 10 s (T14) would auto-reset the controller into
    // its 100 ms reinitialization (T18), which the polls cannot see coming: init sets it to zero,
    // which disables it, so the polls run on through the hold (each run asserts no halt).
    r.input(30, 40, true);
    r.run(r.machine.now() + 10_200_000 * us, None);
    r.input(30, 40, false);
    r.run(r.machine.now() + 50_000 * us, None);
    assert_eq!(r.var(&c, "count"), 4, "the long hold's down and up are delivered once each");
    assert_eq!(&r.data()[6..12], &[30, 40, 1, 30, 40, 0], "the long hold reports its point down, then up");
    assert!(!r.frames.iter().any(|f| matches!(f.event, watch::Event::Fault { .. })), "a long hold causes no fault");
}

#[test]
fn touch_refusals_and_missing_controller_name_address_and_register() {
    let language = VOICEBOX_TOUCH.profile();
    let (forms, ports_) = (fab_elec_mcu::FORMS, fab_elec_mcu::PORTS);
    assert_eq!((ports::FORM_TOUCH, ports::TOUCH_X, ports::TOUCH_Y, ports::TOUCH_DOWN, ports::FORMS), (15, 21, 22, 23, 19));
    assert_eq!([forms[ports::FORM_TOUCH as usize].name, ports_[ports::TOUCH_X as usize].name, ports_[ports::TOUCH_Y as usize].name, ports_[ports::TOUCH_DOWN as usize].name],
        ["touch", "touch_x", "touch_y", "touch_down"], "the front end's touch form and ports are the runtime's numbers");
    for period in [0, 300_001] {
        let source = format!("let finger = touch({period}, fn() {{}})");
        let errors = compile(&source, &language, &[], &[]).err().expect("invalid touch period refuses");
        assert_eq!(errors[0].message, format!("touch period must be 1..=300000 us; period is {period}"), "period refusal gives the allowed interval");
    }
    let errors = compile("let a = touch(10000, fn() {})\nlet b = touch(10000, fn() {})", &language, &[], &[]).err().expect("duplicate touch refuses");
    assert_eq!(errors[0].message, "at most one touch may be declared", "duplicate refusal");
    for name in ["touch_x", "touch_y", "touch_down"] {
        let source = format!("var value: i32 = 0\nfn setup() {{ value = {name}() }}");
        let errors = compile(&source, &language, &[], &[]).err().expect("undeclared touch port refuses");
        assert_eq!(errors[0].message, format!("`{name}` needs a touch declaration"), "each undeclared port names the missing declaration");
    }
    let source = "let finger = touch(10000, fn() {})";
    let c = compile(source, &language, &[], &[]).unwrap();
    let no_touch = chip::profile();
    assert_eq!(module::load(&c.module, &no_touch).unwrap_err().to_string(), "touch declaration needs a board with touch input", "profile refuses at load");
    let m = module::load(&c.module, &language).unwrap();
    assert_eq!(m.decls()[0].handle, 0, "the single touch declaration has handle zero");
    assert_eq!(build_slot(&m, &SlotOptions::default()).err().unwrap().message, "the board has no touch input", "image requires board touch wiring");
    let invalid = SlotOptions { i2c: Some(VOICEBOX_I2C), touch: Some(Touch { addr: 0x80 }), ..SlotOptions::default() };
    assert_eq!(build_slot(&m, &invalid).err().unwrap().message, "CST820 address must be seven-bit (T01, O04)", "invalid device address refuses");
    for period in [1, 300_000] {
        let c = program(period); let m = module::load(&c.module, &language).unwrap();
        let options = SlotOptions { i2c: Some(VOICEBOX_I2C), touch: Some(VOICEBOX_TOUCH), ..SlotOptions::default() };
        assert!(build_slot(&m, &options).is_ok(), "both inclusive period bounds link: {period}");
    }
    // Reverse declaration order must not change ordinary/motion/touch timer
    // numbering. Deliberately slow interpreted ops force overdue coalescing.
    let combined = fab_elec_mcu_rt::Profile { motion: VOICEBOX_MOTION.profile().motion,
        op_ns: 100_000_000, ..language };
    let mixed = compile(r#"
var order: i32 = 0
let finger = touch(10000, fn() { order = order * 10 + 3 })
let imu = motion(10000, fn() { order = order * 10 + 2 })
let tick = every(10000, fn() { order = order * 10 + 1 })
"#, &combined, &[], &[]).unwrap();
    let m = module::load(&mixed.module, &combined).unwrap();
    let options = SlotOptions { i2c: Some(VOICEBOX_I2C), motion: Some(VOICEBOX_MOTION),
        touch: Some(VOICEBOX_TOUCH), ..SlotOptions::default() };
    let slot = build_slot(&m, &options).expect("both sensors link on one I2C service");
    assert_eq!(slot.layout.timer_count, 3, "one ordinary timer plus both sensors");
    assert_eq!(slot.layout.motion.as_ref().unwrap().timer, slot.layout.timers + 32, "motion follows ordinary timers");
    assert_eq!(slot.layout.touch.as_ref().unwrap().timer, slot.layout.timers + 64, "touch follows motion");
    assert!(slot.layout.i2c_pending.is_some(), "both sensors share one pending owner");
    let mut device = Device::new(m, &combined); let mut hal = Reference::default();
    device.boot(&mut hal); device.touch_in(TouchReport { down: true, x: 1, y: 2 });
    assert_eq!(device.step(&mut hal).unwrap().timer, Some(0), "ordinary timer wins the first tie");
    assert_eq!(device.step(&mut hal).unwrap().timer, Some(1), "motion wins the sensor tie");
    let ran = device.step(&mut hal).unwrap();
    assert_eq!(ran.timer, Some(2), "touch is still delivered after earlier deadlines");
    assert_eq!(device.state()[0], 123, "handlers run in ordinary/motion/touch order");
    let missed = ((ran.start - ran.due) / 10_000_000) as u32;
    assert!(missed > 0, "the slow handler makes touch overdue");
    assert!(hal.faults.contains(&Fault::Overrun { timer: 2, missed }), "reference touch coalesces exactly the missed grid events");

    let c = program(10_000); let mut r = Rig::new(&c, 0x16);
    r.until_pc(r.image.slot.symbols.t0_ready);
    r.run(r.machine.now() + 150_000 * r.us(), None);
    let faults: Vec<_> = r.frames.iter().filter_map(|f| match f.event {
        watch::Event::Fault { code, site, arg } => Some((code, site, arg)), _ => None,
    }).collect();
    assert_eq!(faults, [(watch::FAULT_I2C_NACK, 0x15, 0xfe)], "missing touch reports address 0x15 and DisAutoSleep register once");
    assert_eq!(r.var(&c, "count"), 0, "failed initialization never invokes touch");
    assert_eq!(r.data(), [0; 24], "failed initialization leaves the report undelivered");

    let c = program(1); let mut r = Rig::new(&c, 0x15);
    r.poll();
    let timer = r.image.slot.layout.touch.as_ref().unwrap().timer;
    let due = |r: &Rig| { let w = words(&r.machine, timer, 2); u64::from(w[0]) | (u64::from(w[1]) << 32) };
    let before = due(&r); r.poll();
    let missed = ((due(&r) - before) / 16 - 1) as i32;
    assert!(missed > 0, "a one-microsecond poll period coalesces during I2C");
    // Under sustained overload the dispatcher never idles, and only the idle
    // point flushes a partial USB packet: frames leave in full packets. The
    // window covers a packet's worth of overrun frames (delayed, never lost).
    r.run(r.machine.now() + 20_000 * r.us(), None);
    let overruns: Vec<_> = r.frames.iter().filter_map(|f| match f.event {
        watch::Event::Fault { code: watch::FAULT_OVERRUN, site, arg } => Some((site, arg)), _ => None,
    }).collect();
    assert!(overruns.contains(&(1, missed)),
        "runtime touch overrun names its appended timer and exact missed count {missed}: overruns {overruns:?}, {} frames, last {:?}",
        r.frames.len(), r.frames.iter().rev().take(4).map(|f| &f.event).collect::<Vec<_>>());
    assert_eq!(r.var(&c, "count"), 0, "idle overload never invokes an unchanged handler");
    eprintln!("touch 400 kHz, 1 us overload: retired instructions per idle poll: {:?}; coalesced {missed}", r.costs);
}
