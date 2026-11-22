//! One machine per program; the real stage, record, MMU, vectors and USB path.
//! Timing values are explicit model inputs, as in boot.rs, not chip measurements.
#[path = "support/island_fixture.rs"]
mod island_fixture;

use std::sync::Arc;

use fab_elec_mcu::{compile_with, Compiled};
use fab_elec_mcu_emit32::xtensa::{assemble, ccz, enc, Asm};
use fab_elec_mcu_emit_s3::{build_image, build_slot, chip, crc32, feed_quarter_per_hold, runtime, wdt_hold, Board, BoardTime, Image, Layout, Record, Slot, SlotOptions, SlotWhere, CPU_HZ, SLOT_BASE, SLOT_DATA_BASE, VOICEBOX};
use fab_elec_mcu_emu::{Machine, PcSet, Ran, Status, Until};
use fab_elec_mcu_rt::{format::*, module::{self, Module}, ports, sched::{Device, Fault, Hal, Role}, serial, watch};
use fab_elec_mcu_s3::{CpuClock, PinsProfile, ResetScope, S3Machine, S3Profile, TimeProfile};
use fab_elec_mcu_xtensa::insn::{decode, target, Op};
use makepad_core_util::sha256::sha256;

fn profile(seed: u64) -> S3Profile {
    S3Profile {
        flash_bytes: VOICEBOX.flash_bytes as usize,
        mac: [2, 0, 0, 0, 0, 1], pvt: fab_elec_mcu_s3::NO_PVT,
        handoff_cpu: CpuClock::Xtal { div: 2 }, handoff_mapped_pages: 64,
        entry: 0x4200_0408, seed,
        time: TimeProfile {
            systimer_first_long: true, rc_slow_hz: (136_000, 1), wdt_delay_sel: 0,
            swd_timeout_ticks: 432_000_000, swd_request_lead_ticks: 48_000_000,
            wdt_sync_cycles: 2, mwdt_rephase_on_feed: false, mwdt_rephase_on_config: true,
            wdt_restart_on_enable: true,
        },
        pins: PinsProfile {
            power_on_gpio0_high: true, usb_service_ticks: 480_000,
            usb_frame_phase_ticks: seed % 480_000,
            input_sync_delay_ticks: 0, handoff_digital_pads_unheld: true,
            handoff_out: [0; 2], handoff_enable: [0; 2], handoff_pin: [0; 45],
            handoff_io_mux: [1 << 12; 45], handoff_out_sel: [256; 45],
            handoff_usb_tx: Vec::new(), handoff_usb_tx_flushed: false,
            handoff_usb_rx: Vec::new(), handoff_usb_int_raw: 8, handoff_usb_int_ena: 0,
        },
    }
}

fn compiled(source: &str) -> Compiled {
    let names: Vec<_> = (0..=21).chain(26..=48).map(|p| (format!("D{p}"), p)).collect();
    let pins: Vec<_> = names.iter().map(|(name, pin)| (name.as_str(), *pin)).collect();
    compile_with(source, &chip::profile(), &pins, &[], &[]).unwrap_or_else(|e| panic!("{e:?}\n{source}"))
}

fn flash(c: &Compiled, m: &Module<'_>, p: &S3Profile) -> Image {
    let board = Board { layout: VOICEBOX, panel: None, i2c: None, motion: None, touch: None, radio: None, time: BoardTime {
        rc_slow_hz: p.time.rc_slow_hz, wdt_delay_sel: p.time.wdt_delay_sel,
    } };
    let image = build_image(m, &board, &SlotOptions { sites: c.sites.iter().map(|s| (s.line, s.col)).collect(),
        marks: c.marks.clone(), ..SlotOptions::default() }).unwrap();
    assert_eq!(&image.flash[0x400..0x408], &[0x1d, 0x04, 0xdb, 0xae, 0x1d, 0x04, 0xdb, 0xae]);
    image
}

fn words(m: &S3Machine, address: u32, count: u32) -> Vec<u32> {
    let mut bytes = vec![0; count as usize * 4];
    assert!(m.read_data(address, &mut bytes));
    bytes.chunks_exact(4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).collect()
}

fn stamp(m: &S3Machine, slot: &Slot) -> u64 {
    let w = words(m, slot.layout.stamp, 2);
    u64::from(w[0]) | (u64::from(w[1]) << 32)
}

#[derive(Default)]
struct Observed {
    reader: watch::Reader,
    frames: Vec<watch::Frame>,
    trace: Vec<(u64, u32)>,
    vector_entries: usize,
    usb_wakes: usize,
    timer_start: Option<u64>,
    enters: Vec<(u32, u64)>,
    returns: Vec<(u64, Vec<u32>)>,
    handler_start: Option<(u32, u32, u64, u64)>, // id, return PC, instret, drain instret
    handler_cost: Vec<(u32, u64)>, // id, retired including its USB drains
    drain_insns: u64,
    stamp_start: Option<u64>,
    stamp_cost: Vec<u64>,
    drain_start: Option<u64>,
    drain_cost: Vec<u64>,
    arm_start: Option<u64>,
    wait_cost: Vec<u64>,
    turn_start: Option<u64>,
    due_cost: Vec<u64>,
}

impl Observed {
    fn capture_timer_start(&mut self, m: &S3Machine) {
        self.timer_start = Some(u64::from(m.cpu().ar[2]) | (u64::from(m.cpu().ar[3]) << 32));
    }

    fn take(&mut self, m: &mut S3Machine) {
        let mut out = Vec::new(); m.take_out(&mut out);
        for byte in out {
            assert_eq!(byte.link, 0);
            if let Some(frame) = self.reader.push(byte.byte) { self.frames.push(frame.expect("valid watch frame")); }
        }
        m.take_trace(&mut self.trace); m.mark();
    }
    fn at(&mut self, m: &S3Machine, slot: &Slot) {
        let pc = m.pc(); let retired = m.cpu().instret;
        if pc == slot.symbols.timer_start { self.capture_timer_start(m); }
        if pc == runtime::KERNEL_VECTOR {
            assert_eq!(m.cpu().exccause, 4, "only level-1 interrupts enter the wake vector");
            assert!([slot.symbols.waiti + 3, slot.symbols.idle_waiti + 3].contains(&m.cpu().epc1),
                "every interrupt resumes after an armed or idle WAITI");
            if m.cpu().interrupt & m.cpu().intenable & (1 << 2) != 0 {
                assert!(slot.layout.eth.is_some() || words(m, slot.layout.tx_head, 1) != words(m, slot.layout.tx_tail, 1),
                    "Interrupt use: USB wakes with queued TX or declared eth");
                self.usb_wakes += 1;
            }
            self.vector_entries += 1;
        }
        if [slot.symbols.waiti, slot.symbols.idle_waiti].contains(&pc) {
            let pending = words(m, slot.layout.tx_head, 1) != words(m, slot.layout.tx_tail, 1);
            // facts-machine.md §2 / TRM Table 9.3-2: dispatcher CPU lines 1,2.
            assert_eq!(m.cpu().intenable, (1 << 1) | if pending || slot.layout.eth.is_some() { 1 << 2 } else { 0 },
                "Interrupt use: line 2 is enabled at WAITI exactly while TX is pending or eth is declared");
        }
        if let Some(h) = slot.handlers.iter().find(|h| h.address == pc) {
            assert_eq!(words(m, slot.layout.ordinal, 1)[0] as usize, self.enters.len() + 1);
            assert_eq!(m.cpu().ar[2], slot.layout.state); assert_eq!(m.cpu().ar[3], slot.shared);
            assert_eq!(m.cpu().ps & 15, 15, "handlers run with interrupts masked");
            assert!(self.handler_start.replace((h.id, m.cpu().ar[0], retired, self.drain_insns)).is_none(),
                "handlers cannot nest");
            self.enters.push((h.id, stamp(m, slot)));
        }
        if self.handler_start.is_some_and(|(_, return_pc, _, _)| pc == return_pc) {
            let (id, _, start, drain_start) = self.handler_start.take().unwrap();
            // A drain writes at most one packet, so the handler bound includes it.
            let instructions = retired - start;
            let drained = self.drain_insns - drain_start;
            let handler = slot.handlers.iter().find(|h| h.id == id).unwrap();
            assert!(instructions <= handler.max_insns,
                "handler {id}: {instructions} instructions ({drained} of them USB drain), stack_bytes {}, max_insns {}",
                handler.stack_bytes, handler.max_insns);
            self.handler_cost.push((id, instructions));
            assert_eq!(m.cpu().ar[1], slot.layout.stack_top, "handler restores a1");
            self.returns.push((stamp(m, slot), words(m, slot.layout.state, slot.layout.state_words)));
        }
        if pc == slot.symbols.stamp { self.stamp_start = Some(retired); }
        if pc == slot.symbols.stamp_end - 2 {
            self.stamp_cost.push(retired - self.stamp_start.take().expect("stamp entry") + 1);
        }
        if pc == slot.symbols.drain { self.drain_start = Some(retired); }
        if pc == slot.symbols.drain_end - 2 {
            let instructions = retired - self.drain_start.take().expect("drain entry") + 1;
            // Reported per drain; handler bounds include drains (no polling).
            self.drain_insns += instructions;
            self.drain_cost.push(instructions);
        }
        if pc == slot.symbols.arm { self.arm_start = Some(retired); }
        if pc == slot.symbols.dispatch {
            if let Some(start) = self.arm_start.take() { self.wait_cost.push(retired - start); }
            self.turn_start = Some(retired);
        }
        if pc == slot.symbols.call_handler {
            // Includes CALL0 and the nine instructions after its return
            // (eight for feed, one J), excludes the handler itself.
            self.due_cost.push(retired - self.turn_start.take().expect("dispatcher turn") + 10);
        }
    }
    fn report(&self) {
        let range = |v: &[u64]| (v.iter().min().copied().unwrap_or(0), v.iter().max().copied().unwrap_or(0));
        println!("instructions: stamp {:?}; due turn incl. feed, excl. handler {:?}; USB drain incl. packet feeds {:?}; arm/WAITI/vector/return {:?}",
            range(&self.stamp_cost), range(&self.due_cost), range(&self.drain_cost), range(&self.wait_cost));
    }
    fn report_handlers(&self, slot: &Slot, m: &Module<'_>) {
        let calls = self.handler_cost.iter().map(|&(id, retired)|
            (slot.handlers.iter().find(|h| h.id == id).unwrap(), retired));
        if let Some((handler, retired)) = calls.max_by(|a, b|
            (u128::from(a.1) * u128::from(b.0.max_insns)).cmp(&(u128::from(b.1) * u128::from(a.0.max_insns)))) {
            println!("largest handler instructions/max_insns: entry {}, {retired}/{} = {:.3}, AIR cost {}, stack_bytes {}, max_insns {} (USB drains included)",
                handler.id, handler.max_insns, retired as f64 / handler.max_insns as f64,
                m.entries()[handler.id as usize].cost, handler.stack_bytes, handler.max_insns);
        }
    }
}

struct Harness {
    machine: S3Machine,
    observed: Observed,
    marks: PcSet,
    stops: Vec<u32>,
    epoch: u64,
}

impl Harness {
    fn new(image: &Image, seed: u64) -> Self {
        Self::with_costs(image, seed, false)
    }
    fn with_costs(image: &Image, seed: u64, costs: bool) -> Self {
        Self::with_profile(image, &profile(seed), costs)
    }
    fn with_profile(image: &Image, p: &S3Profile, costs: bool) -> Self {
        let slot = &image.slot;
        let mut stops = vec![runtime::KERNEL_VECTOR, slot.symbols.t0_ready,
            slot.symbols.handler_return, slot.symbols.drain, slot.symbols.drain_end - 2,
            slot.symbols.waiti, slot.symbols.idle_waiti];
        // Setup returns to init, not the dispatcher's handler_return label.
        if slot.symbols.setup_call != 0 { stops.push(slot.symbols.setup_call + 3); }
        if slot.symbols.timer_start != 0 { stops.push(slot.symbols.timer_start); }
        if costs {
            stops.extend([slot.symbols.stamp, slot.symbols.stamp_end - 2,
                slot.symbols.drain, slot.symbols.drain_end - 2, slot.symbols.arm,
                slot.symbols.dispatch, slot.symbols.call_handler]);
        }
        stops.extend(slot.handlers.iter().map(|h| h.address)); stops.sort_unstable(); stops.dedup();
        let mut marks = PcSet::new(SLOT_BASE, slot.bytes.len() as u32);
        for &pc in &stops { marks.insert(pc); }
        let mut h = Self { machine: S3Machine::new(Arc::new(image.flash.clone()), p),
            observed: Observed::default(), marks, stops, epoch: 0 };
        // Sampled levels belong to the board, not GPIO's output latch or
        // pull configuration. Hold the fixture pins low, matching Model;
        // leave GPIO0 at the profile's explicit BOOT pull-up level.
        for pin in 1..h.machine.pins().len() { h.machine.set_input(pin, false); }
        // The harness measures its cutoffs from t0, before Hello. The image
        // samples the timer origin after device init.
        loop {
            let ran = h.next(slot, 480_000_000);
            if ran == Ran::Pc && h.machine.pc() == slot.symbols.t0_ready { break; }
            assert_ne!(ran, Ran::Tick, "init must reach t0");
        }
        h.epoch = h.machine.now(); h
    }
    fn next(&mut self, slot: &Slot, deadline: u64) -> Ran {
        let ran = self.machine.run(&Until { tick: deadline, marks: &self.marks, stops: &self.stops,
            stops_after: self.machine.now(), on_drive: false });
        match ran {
            Ran::Pc => self.observed.at(&self.machine, slot),
            Ran::Barrier | Ran::Tick => {}
            _ => panic!("unexpected {ran:?}: {:?}", self.machine.status()),
        }
        self.observed.take(&mut self.machine); ran
    }
    fn until(&mut self, slot: &Slot, ns: u64) {
        let elapsed = (u128::from(ns) * u128::from(self.machine.tick_hz()) / 1_000_000_000) as u64;
        let deadline = self.epoch + elapsed;
        while self.next(slot, deadline) != Ran::Tick {}
        assert!(!matches!(self.machine.status(), Status::Halted(_)));
    }
    fn check_frames(&self, slot: &Slot, m: &Module<'_>) {
        self.observed.report_handlers(slot, m);
        for (seq, frame) in self.observed.frames.iter().enumerate() {
            assert_eq!(frame.seq, seq as u8, "no lost/duplicated frame");
            assert!(!matches!(frame.event, watch::Event::Lost { .. }));
        }
        let hello = self.observed.frames.first().expect("Hello");
        assert_eq!(hello.tick, 0);
        assert_eq!(hello.event, watch::Event::Hello { proto: watch::PROTO,
            ports_version: ports::PORT_TABLE_VERSION as u8, image: crc32(&slot.bytes),
            clock_hz: 16_000_000, prescale: 1, ring: 0, tick_hi: 0, mcusr: 1, wdtcsr: 0, level: watch::Level::Ports, generation: 0 });
        assert_eq!(slot.image_id, crc32(&slot.bytes));
        assert_eq!(self.observed.trace.iter().filter(|(_, pc)| *pc == runtime::KERNEL_VECTOR).count(),
            self.observed.vector_entries, "every stopped vector entry was traced on resumption");
    }
}

#[derive(Default)]
struct Model { events: Vec<watch::Event> }
impl Hal for Model {
    fn eeprom_read(&mut self, _: u64, _: u16, _: u32) -> u8 { unreachable!("this profile has no EEPROM") }
    fn eeprom_write(&mut self, _: u64, _: u16, _: u8, _: u32) { unreachable!("this profile has no EEPROM") }
    fn shift(&mut self, _: u64, _: u32, _: u32, _: u32, _: u8, _: u32) { unreachable!("this fixture shifts nothing") }
    fn configure(&mut self, _: u64, _: u32, _: Role) {}
    fn serial(&mut self, _: u64, _: u32, _: serial::SerialSetting) { panic!("no serial in 6b1"); }
    fn write(&mut self, _: u64, pin: u32, high: bool, site: u32) {
        self.events.push(watch::Event::Write { pin: pin as u8, level: high, site: site as u16 });
    }
    fn duty(&mut self, _: u64, _: u32, _: u8, _: u32) { panic!("no PWM in 6b1"); }
    fn read(&mut self, _: u64, _: u32, _: u32) -> bool { false }
    fn adc(&mut self, _: u64, _: u32, _: u32) -> u16 { unreachable!("this fixture declares no analog input") }
    fn eth_send(&mut self, _: u64, _: &[u8], _: u32) -> bool { false }
    fn eth_mac(&mut self, _: u64, _: u32) -> [u8; 6] { [2, 0, 0, 0, 0, 1] }
    fn clock(&mut self, t: u64, _: u32) -> u64 { t }
    fn probe(&mut self, _: u64, id: u8, value: u16) { self.events.push(watch::Event::Probe { id, value }); }
    fn fault(&mut self, _: u64, f: Fault) {
        let (code, site, arg) = match f {
            Fault::Pin { pin, site, .. } => (watch::FAULT_PIN, site, pin),
            Fault::Timer { timer, site, .. } => (watch::FAULT_TIMER, site, timer),
            Fault::Overrun { timer, missed } => (watch::FAULT_OVERRUN, timer, missed as i32),
            Fault::LoopCap { site } => (watch::FAULT_LOOP_CAP, site, 0),
            Fault::Draw { site, refusal, .. } => (watch::FAULT_DRAW, site, refusal as i32),
            Fault::Range { site, n } => (watch::FAULT_RANGE, site, n),
            Fault::ShiftBits { site, bits } => (watch::FAULT_SHIFT_BITS, site, bits),
            Fault::EepromAddr { site, addr } => (watch::FAULT_EEPROM_ADDR, site, addr),
            Fault::EthLength { site, n } => (watch::FAULT_ETH_LENGTH, site, n),
            Fault::EthRange { site, off, .. } => (watch::FAULT_ETH_RANGE, site, off),
            _ => panic!("unsupported fixture fault {f:?}"),
        };
        self.events.push(watch::Event::Fault { code, site: site as u16, arg });
    }
}

// The AVR seeds, using both S3 GPIO banks and no PWM or clock reads.
fn generated(seed: u32) -> String {
    let mut rng = seed;
    let mut next = || { rng ^= rng << 13; rng ^= rng >> 17; rng ^= rng << 5; rng };
    let a = next() as i32; let b = next() as i32;
    let op = ["+", "-", "&", "|", "^"][(next() % 5) as usize];
    let limit = 3 + next() % 3; let shift = next() % 32;
    let extra = if next() & 1 == 0 { "var c: i32 = 3\nvar q = true\n" } else { "" };
    let change = if extra.is_empty() { "" } else { "c = c + 1\nq = !q\n" };
    format!(r#"
let out = output(D4)
let aux = output(D33)
let led = output(D13)
var a: i32 = {a}
var b: i32 = {b}
var p = false
var data = [int(0); 4]
{extra}
fn setup() {{ write(led, low) }}
let first = every(ms(5), fn() {{
    let bias = b ^ {seed}
    let i: i32 = 0
    while i < 5 {{
        i = i + 1
        if i == 2 {{ continue }}
        if i == {limit} {{ break }}
        a = (a {op} bias) ^ i
    }}
    p = !p
    if p {{ if b < 0 {{ a = -a }} else {{ a = a >> {shift} }} }}
    else {{ if a == b {{ b = b + 1 }} else {{ b = b ^ a }} }}
    data[a] = b
    b = data[b]
    {change}
    write(out, if p {{ high }} else {{ low }})
}})
let second = every(ms(7), fn() {{
    a = a + 1
    b = (b >>> {shift}) ^ data[a]
    data[b] = a
    write(aux, a)
}})
"#)
}

fn replay(source: &str, until_ns: u64, seed: u64) -> (Vec<watch::Event>, Vec<u32>) {
    let c = compiled(source); let m = module::load(&c.module, &chip::profile()).unwrap();
    let image = flash(&c, &m, &profile(seed)); let slot = &image.slot;
    let mut h = Harness::new(&image, seed); h.until(slot, until_ns); h.check_frames(slot, &m);
    let state = words(&h.machine, slot.layout.state, slot.layout.state_words);
    let events: Vec<_> = h.observed.frames.iter().filter_map(|f| matches!(f.event,
        watch::Event::Write { .. } | watch::Event::Fault { .. } | watch::Event::Probe { .. }).then_some(f.event)).collect();
    let mut device = Device::new(m, &chip::profile()); let mut model = Model::default();
    device.boot(&mut model); device.run_until(until_ns, &mut model);
    assert_eq!(events, model.events, "watch replay\n{source}");
    assert_eq!(state, device.state(), "SRAM replay\n{source}");
    if !device.module().decls().iter().all(|d| d.form != ports::FORM_EVERY) {
        assert!(h.observed.vector_entries > 0, "WAITI proof must observe an interrupt");
    }
    (events, state)
}

/// Runs `s3` for `ticks` with no stops; the instructions it retired.
fn pace(s3: &mut S3Machine, observed: &mut Observed, ticks: u64) -> u64 {
    let (start, retired) = (s3.now(), s3.cpu().instret);
    while s3.now() < start + ticks {
        let ran = s3.run(&Until { tick: start + ticks, marks: &PcSet::default(), stops: &[],
            stops_after: s3.now(), on_drive: false });
        observed.take(s3);
        assert!(matches!(ran, Ran::Tick | Ran::Barrier), "a plain run: {ran:?}, {:?}", s3.status());
    }
    assert_eq!(s3.now(), start + ticks, "instruction boundaries fall on the paced ticks");
    s3.cpu().instret - retired
}

#[test]
fn the_runtime_runs_the_cpu_at_cpu_hz_from_init_and_notes_the_regulator() {
    let c = compiled("var n: i32 = 0\nfn setup() { for i in 0..1000 { n = (n + i) & 65535 } }");
    let m = module::load(&c.module, &chip::profile()).unwrap();
    let p = profile(1);
    let image = flash(&c, &m, &p);
    let setup = image.slot.handlers[m.entry("setup").unwrap()].address;
    let mut stops = vec![runtime::INIT, setup]; stops.sort_unstable();
    let mut marks = PcSet::new(SLOT_BASE, image.slot.bytes.len() as u32);
    for &pc in &stops { marks.insert(pc); }
    let mut s3 = S3Machine::new(Arc::new(image.flash.clone()), &p);
    let mut observed = Observed::default();
    let reach = |s3: &mut S3Machine, observed: &mut Observed, pc: u32| loop {
        let ran = s3.run(&Until { tick: 60 * s3.tick_hz(), marks: &marks, stops: &stops,
            stops_after: s3.now(), on_drive: false });
        observed.take(s3);
        match ran {
            Ran::Pc if s3.pc() == pc => break s3.now(),
            Ran::Pc | Ran::Barrier => {}
            _ => panic!("the image reaches {pc:#x}: {ran:?}, {:?}", s3.status()),
        }
    };
    // The handoff: XTAL/2 is 24 ticks of the 480 MHz grid; init's first ten instructions
    // (VECBASE, CPENABLE, FCR and the saved a2/a3) precede the clock writes.
    let init = reach(&mut s3, &mut observed, runtime::INIT);
    assert_eq!(s3.tick_hz(), 480_000_000, "the model's grid");
    assert_eq!(pace(&mut s3, &mut observed, 240), 10, "the ROM hands over on XTAL/2");
    let at_setup = reach(&mut s3, &mut observed, setup);
    let per = s3.tick_hz() / u64::from(CPU_HZ);
    assert_eq!(pace(&mut s3, &mut observed, 1000 * per), 1000, "setup runs at CPU_HZ, one instruction per cycle");
    let mut notes = Vec::new(); s3.take_notes(&mut notes);
    let regulator: Vec<_> = notes.iter().filter(|n| n.what.starts_with("CPU on the PLL: Assumed")).collect();
    assert_eq!(regulator.len(), 1, "the switch notes the regulator assumption once: {notes:?}");
    assert!((init..at_setup).contains(&regulator[0].tick), "noted at the switch, inside init");
}

#[test]
fn the_analog_check_reports_the_master_the_bbpll_and_the_regulators_init_raised_for_240_mhz() {
    let c = compiled("fn setup() {}");
    let m = module::load(&c.module, &chip::profile()).unwrap();
    let mut p = profile(3);
    let board = Board { layout: VOICEBOX, panel: None, i2c: None, motion: None, touch: None, radio: None,
        time: BoardTime { rc_slow_hz: p.time.rc_slow_hz, wdt_delay_sel: p.time.wdt_delay_sel } };
    let image = build_image(&m, &board, &SlotOptions { sites: c.sites.iter().map(|s| (s.line, s.col)).collect(),
        marks: c.marks.clone(), analog_check: true, ..SlotOptions::default() }).unwrap();
    let setup = image.slot.handlers[m.entry("setup").unwrap()].address;
    let mut marks = PcSet::new(SLOT_BASE, image.slot.bytes.len() as u32); marks.insert(setup);
    // The PVT fields of the board's chip (session 18: block version 1.4, K_RTC_LDO 1, K_DIG_LDO -4,
    // V_RTC_DBIAS20 8, V_DIG_DBIAS20 -4, DIG_DBIAS_HVT 26), by IDF's formula (rtc_init.c:327-409) rtc 31 and
    // dig min(31, 26 + 3) = 29; then a chip without PVT, IDF's 28 and 28.
    for (pvt, settings) in [([0x0400_0000, 0x4440_2000, 0x0000_d420, 0x1], [31u16, 29]), (fab_elec_mcu_s3::NO_PVT, [28, 28])] {
        p.pvt = pvt;
        let mut s3 = S3Machine::new(Arc::new(image.flash.clone()), &p);
        let mut observed = Observed::default();
        loop {
            let ran = s3.run(&Until { tick: 60 * s3.tick_hz(), marks: &marks, stops: &[setup], stops_after: s3.now(), on_drive: false });
            observed.take(&mut s3);
            match ran {
                Ran::Pc if s3.pc() == setup => break,
                Ran::Pc | Ran::Barrier => {}
                _ => panic!("the check image reaches setup: {ran:?}, {:?}", s3.status()),
            }
        }
        // The frames queued before setup leave over USB in the next simulated 50 ms.
        let drained = s3.now() + s3.tick_hz() / 20;
        loop {
            let ran = s3.run(&Until { tick: drained, marks: &marks, stops: &[], stops_after: s3.now(), on_drive: false });
            observed.take(&mut s3);
            match ran {
                Ran::Tick => break,
                Ran::Barrier => {}
                _ => panic!("the idle check image drains its frames: {ran:?}, {:?}", s3.status()),
            }
        }
        let probes: Vec<(u8, u16)> = observed.frames.iter().filter_map(|f| match f.event {
            watch::Event::Probe { id, value } => Some((id, value)), _ => None }).collect();
        println!("analog check probes (id, value): {probes:x?}");
        let reads = runtime::ANALOG_READS.len() as u8;
        let ids: Vec<u8> = (0..8).chain(runtime::ANALOG_READ_PROBES..runtime::ANALOG_READ_PROBES + 2 * reads)
            .chain(runtime::RAISE_PROBES..runtime::RAISE_PROBES + 10).collect();
        assert_eq!(probes.iter().map(|p| p.0).collect::<Vec<_>>(), ids, "each probe once, in order, before setup");
        let word = |id: usize| u32::from(probes[id].1) | u32::from(probes[id + 1].1) << 16;
        assert_eq!([word(0), word(2), word(4)], [0x2100_e408, 0x2d, 0x4], "ANA_CONF0, ANA_CONFIG and ANA_CONFIG2 as the board found them after the ROM");
        assert_eq!(word(6), word(2) & !(1 << 17), "the BBPLL unmask clears ANA_CONFIG bit 17 and nothing else");
        // What the board's ROM read (session 17), with init's raise in DIG_REG 4 and 6 (reads 6 and 8): the setting
        // in [4:0], bits [7:5] as the ROM left them (sessions 19-21 read them back so). Each call costs at least the
        // stand-in's burn for the block.
        let raised = settings.map(|setting| (0x98 & 0xe0) | setting as u8);
        let board = [0x50, 0x08, 0x6b, 0x80, 0x63, 0x92, raised[0], 0x18, raised[1], 0x18, 0x4e];
        for (k, &(slave, register)) in runtime::ANALOG_READS.iter().enumerate() {
            let (answer, cycles) = (probes[8 + 2 * k].1, probes[9 + 2 * k].1);
            assert_eq!(answer >> 8, u16::from(register), "read {k} names its register");
            assert_eq!(answer as u8, board[k], "read {k} of block {slave:#x}, register {register}, as the board's ROM read it");
            let burn = (if slave == 0x66 { fab_elec_mcu_s3::rom::READ_BBPLL_CYCLES } else { fab_elec_mcu_s3::rom::READ_DIG_REG_CYCLES }) as u16;
            assert!((burn..0xffff).contains(&cycles), "read {k}'s call takes its block's burn and returns: {cycles} cycles");
        }
        // The settings init wrote, then the clock check at 240 MHz: 100 us of the systimer cold, warm, then 200 us
        // warm. The board's warm rounds at 240 MHz (sessions 20-21): 24169-24170, 48137-48140 cycles and 236, 471
        // reads. The model has no fetch cost, so its cold round is a warm one; it keeps the reads' time and polls
        // VALID more often than the chip.
        let raise = &probes[8 + 2 * reads as usize..];
        assert_eq!(raise[0].1, settings[0] | settings[1] << 8, "EXT_RTC_DREG and EXT_DIG_DREG from the eFuse by IDF's formula");
        for (round, (cycles_on_board, reads_on_board)) in [(24_170u16, 236u16), (24_170, 236), (48_138, 471)].into_iter().enumerate() {
            let [cycles, reads, polls] = [1, 2, 3].map(|k| raise[3 * round + k].1);
            assert!(cycles.abs_diff(cycles_on_board) <= 240, "round {round}: {cycles} cycles, the board {cycles_on_board} (1 us)");
            assert!(reads.abs_diff(reads_on_board) <= 3, "round {round}: {reads} systimer reads, the board {reads_on_board}");
            assert!(polls >= 3 * reads, "round {round}: {polls} VALID polls for {reads} reads, as many as the board's (3 a read) or more");
        }
        let mut notes = Vec::new(); s3.take_notes(&mut notes);
        assert_eq!(notes.iter().filter(|n| n.what.starts_with("analog I2C master: ")).count(), 0, "the ROM path makes no master Assumption: {notes:?}");
        for stand_in in ["rom_i2c_readReg: ", "rom_i2c_writeReg: "] {
            assert_eq!(notes.iter().filter(|n| n.what.starts_with(stand_in)).count(), 1, "{stand_in} the stand-in's cost is noted once: {notes:?}");
        }
    }
}

#[test]
fn programs_match_device_and_interrupts_only_wake_waiti() {
    for seed in [1, 7, 19, 37, 71, 101, 257, 509, 1021, 65537, 0x55555555, 0xdeadbeef] {
        replay(&generated(seed), 24_000_000, u64::from(seed));
    }
    let mut source = String::from("let out = output(D13)\nvar value: i32 = 256\nfn setup() { write(out, value) }\nlet clear = after(0, fn() {\n");
    // Single-assignment locals retain frame cells after their AIR is removed.
    // This zero-cost one-shot exercises the clear bound; only two calls run.
    for n in 0..64 { source.push_str(&format!("let temporary{n}: i32 = {n}\n")); }
    source.push_str("})\n");
    replay(&source, 5_000_000, 1);
    replay("let out = output(D13)\nvar x: i32 = 300\nvar y: i32 = 269\nvar z: i32 = -243\nfn setup() { write(x, high) write(y, high) write(z, high) }", 6_000_000, 7);
    replay("let out = output(D13)\nvar n: i32 = 0\nvar p = false\nlet tick = every(ms(5), fn() { p = !p let s: i32 = if p { 2.5 } else { -1.5 } n = n + n + s write(out, n) })", 24_000_000, 19);
    // probe sends the id's low byte and the value's low half, the same frames as the reference's.
    let (events, _) = replay("var n: i32 = 0\nlet tick = every(ms(5), fn() { n = n + 70000 probe(n, n) probe(-1, -2) })", 12_000_000, 3);
    assert_eq!(&events[..4], [watch::Event::Probe { id: 112, value: 4464 }, watch::Event::Probe { id: 255, value: 65534 },
        watch::Event::Probe { id: 224, value: 8928 }, watch::Event::Probe { id: 255, value: 65534 }], "probe frames carry the program's words");
    let (events, state) = replay("var x: i32 = 0\nfn setup() { while x >= 0 { x = x + 1 } }", 20_000_000, 37);
    assert_eq!(state, [1024]);
    assert!(matches!(events.as_slice(), [watch::Event::Fault { code: watch::FAULT_LOOP_CAP, arg: 0, .. }]));
    // Equal deadlines retain declaration order, including a one-shot at zero.
    replay("let a = output(D4)\nlet b = output(D33)\nlet once = after(0, fn() { write(a, low) })\nlet first = every(ms(10), fn() { write(a, high) })\nlet second = every(ms(10), fn() { write(b, high) })", 25_000_000, 71);
    replay("let out = output(D4)\nlet data = [int(11), int(22), int(33)]\nvar n: i32 = 0\nlet tick = every(ms(5), fn() { n = data[n] write(out, n) })", 24_000_000, 509);
    replay(r#"
var x: f32 = 1.25
var divisor: f32 = 1.000001
fn setup() {
    let i: i32 = 0
    while i < 32 {
        x = sqrt(x / divisor + 1.0)
        i = i + 1
    }
}
"#, 5_000_000, 1021);
}

#[test]
fn pin_write_only_handlers_cover_time_and_fault_frames() {
    let c = compiled(r#"
let out = output(D33)
var bad: i32 = 49
let tick = every(270000000, fn() {
    write(bad, high)
    write(out, high)
    write(out, low)
})
"#);
    let m = module::load(&c.module, &chip::profile()).unwrap();
    let image = flash(&c, &m, &profile(71)); let slot = &image.slot;
    let mut h = Harness::new(&image, 71);
    // 270 s crosses the 32-bit SYSTIMER low word on each dispatch. WAITI
    // advances model time directly; only two handlers execute.
    h.until(slot, 541_000_000_000); h.check_frames(slot, &m);
    assert_eq!(h.observed.enters.len(), 2);
    assert_eq!(h.observed.handler_cost.len(), 2);
    assert_eq!(h.observed.frames.iter().filter(|f| matches!(f.event, watch::Event::Time { .. })).count(), 2);
    let mut high = 0;
    for frame in h.observed.frames.windows(2) {
        if let watch::Event::Time { tick_hi } = frame[0].event {
            assert_ne!(tick_hi, high, "each handler changes the timestamp high bits");
            high = tick_hi;
            assert!(matches!(frame[1].event, watch::Event::Fault { code: watch::FAULT_PIN, arg: 49, .. }),
                "the first pin_write sends Time plus the largest port event body");
        }
    }
    assert_eq!(h.observed.frames.iter().filter(|f| matches!(f.event, watch::Event::Write { .. })).count(), 4);
    assert_eq!(h.observed.usb_wakes, 0,
        "a turn's Time, Fault and two Write frames (51 bytes) share one packet flushed at idle: no USB wake");
}

#[test]
fn timer_ports_and_pin_roles_match_device() {
    replay(r#"
let out = output(D33)
var n: i32 = 0
let tick = every(ms(10), fn() { n = n + 1 write(out, n) })
let change = after(ms(12), fn() { period(tick, ms(20)) })
let cancel = after(ms(25), fn() { stop(tick) })
let restart = after(ms(32), fn() { start(tick) })
"#, 75_000_000, 101);
    replay(r#"
let input_pin = input(D2, pull_up)
let output_pin = output(D33)
var input_number: i32 = 2
var bad: i32 = -1
var value: i32 = 7
fn setup() {
    write(input_number, high)
    value = read(bad)
    stop(bad)
    start(bad)
    period(bad, ms(5))
    value = value + read(output_pin) + read(input_pin)
}
"#, 10_000_000, 257);
}

#[test]
fn late_timer_coalesces_once_and_reports_missed_grid_points() {
    let c = compiled(r#"
let out = output(D4)
var n: i32 = 0
var acc: i32 = 0
fn setup() {
    let i: i32 = 0
    while i < 128 { write(out, low) i = i + 1 }
    for k in 0..100000 { acc = (acc + k) & 65535 }
}
let tick = every(ms(1), fn() {
    n = n + 1
    if n == 2 { stop(tick) }
    write(out, high)
})
"#);
    let m = module::load(&c.module, &chip::profile()).unwrap();
    let timer_id = m.entry("tick").unwrap() as u32;
    let image = flash(&c, &m, &profile(37)); let slot = &image.slot;
    let mut h = Harness::new(&image, 37);
    h.until(slot, 40_000_000); h.check_frames(slot, &m);
    let starts: Vec<_> = h.observed.enters.iter().filter_map(|(id, stamp)| (*id == timer_id).then_some(*stamp)).collect();
    assert_eq!(starts.len(), 2, "coalesce, do not replay each missed firing");
    const PERIOD: u64 = 16_000;
    let mut due = h.observed.timer_start.unwrap() + PERIOD; let mut faults = Vec::new();
    for start in starts {
        // rt::sched::step: advance due + period until strictly past start;
        // every additional elapsed grid point is a missed firing.
        let missed = (start - due) / PERIOD;
        due += (missed + 1) * PERIOD;
        if missed != 0 { faults.push((start as u32, watch::Event::Fault {
            code: watch::FAULT_OVERRUN, site: 0, arg: missed.min(u64::from(u32::MAX)) as u32 as i32,
        })); }
    }
    assert!(!faults.is_empty(), "setup computation must make this fixture late without waiting for USB");
    let actual: Vec<_> = h.observed.frames.iter().filter_map(|f| matches!(f.event,
        watch::Event::Fault { code: watch::FAULT_OVERRUN, .. }).then_some((f.tick, f.event))).collect();
    assert_eq!(actual, faults);
    let table = words(&h.machine, slot.layout.timers, 8);
    assert_eq!(u64::from(table[0]) | (u64::from(table[1]) << 32), due);
    assert_eq!(table[4], 0, "second handler stopped the timer");
    let acc = ((0..100_000u64).sum::<u64>() % 65_536) as u32;
    assert_eq!(words(&h.machine, slot.layout.state, slot.layout.state_words), [2, acc], "two firings after the whole setup sum");
}

#[test]
fn watch_ring_keeps_timers_running_and_reports_exact_loss_after_reopen() {
    // Sixteen 12-byte Write frames every 5 ms: below the model's 64-byte/ms
    // host capacity, but enough to fill the ring in 215 ms with a closed port.
    // Stop after fifty firings so the final empty-ring IRQ policy is observable.
    let c = compiled(r#"
let out = output(D4)
var n: i32 = 0
let tick = every(ms(5), fn() {
    n = n + 1
    let i: i32 = 0
    while i < 16 { write(out, i & 1) i = i + 1 }
    if n == 50 { stop(tick) }
})
"#);
    let m = module::load(&c.module, &chip::profile()).unwrap();
    let p = profile(37); let image = flash(&c, &m, &p); let slot = &image.slot;
    let mut open = Harness::new(&image, 37);
    let mut closed = Harness::new(&image, 37);
    closed.machine.set_usb_host_listening(false); // Before the first Hello enqueue.
    open.until(slot, 226_000_000); closed.until(slot, 221_000_000);
    let full_head = words(&closed.machine, slot.layout.tx_head, 1);
    let full_seq = words(&closed.machine, slot.layout.seq, 1);
    let full_ring = words(&closed.machine, slot.layout.tx_ring, runtime::TX_RING_BYTES / 4);
    closed.until(slot, 226_000_000);
    assert_eq!(words(&closed.machine, slot.layout.tx_head, 1), full_head,
        "Closed port: rejected frames leave the head unchanged");
    assert_eq!(words(&closed.machine, slot.layout.seq, 1), full_seq,
        "Closed port: rejected frames leave the sequence unchanged");
    assert_eq!(words(&closed.machine, slot.layout.tx_ring, runtime::TX_RING_BYTES / 4), full_ring,
        "Closed port: rejected frames write no partial bytes");
    assert_eq!(closed.observed.enters.len(), 45, "Closed port: every timer deadline ran");
    assert_eq!(closed.observed.enters.len(), open.observed.enters.len(),
        "Closed port: the handler schedule does not depend on a reader");
    // One blocked drain's fixed suffix is below 32 retired instructions;
    // allow that cost and SYSTIMER rounding, not an extra timer period.
    let jitter = (32u64 * 16_000_000).div_ceil(u64::from(CPU_HZ)) + 1;
    for ((open_id, open_tick), (closed_id, closed_tick)) in
        open.observed.enters.iter().zip(&closed.observed.enters)
    {
        assert_eq!(closed_id, open_id, "Closed port: same handler");
        assert!(closed_tick.abs_diff(*open_tick) <= jitter,
            "Closed port: start {closed_tick} differs from open start {open_tick} by more than {jitter} ticks");
    }
    assert!(closed.observed.frames.is_empty(), "Closed port: the host consumed no bytes");
    assert_eq!(closed.observed.usb_wakes, 0, "Interrupt use: a closed port cannot release the FIFO");
    assert_ne!(closed.machine.cpu().intenable & (1 << 2), 0,
        "Interrupt use: pending bytes arm CPU line 2 at WAITI");
    assert!(matches!(closed.machine.status(), Status::Sleeping { .. }),
        "Closed port: idle time is WAITI, not polling");

    // Hello occupies the flushed hardware packet; the software ring admits
    // exactly floor((8192-1)/12) events. No Time frame is due in this epoch.
    let kept = (runtime::TX_RING_BYTES as usize - 1) / 12;
    let emitted = 45 * 16;
    let lost = emitted - kept;
    assert_eq!(words(&closed.machine, slot.layout.lost_count, 1), [lost as u32],
        "Closed port: count exactly the refused events");
    let first_tick = closed.observed.enters[kept / 16].1 as u32;
    let last_tick = closed.observed.enters[44].1 as u32;
    assert_eq!(words(&closed.machine, slot.layout.lost_first, 1), [first_tick],
        "Closed port: preserve the first refused tick");
    assert_eq!(words(&closed.machine, slot.layout.lost_last, 1), [last_tick],
        "Closed port: preserve the last refused tick");

    closed.machine.set_usb_host_listening(true);
    open.until(slot, 440_000_000); closed.until(slot, 440_000_000);
    open.check_frames(slot, &m);
    assert_eq!(closed.observed.enters.len(), 50, "Reopen: all later timer handlers run");
    assert_eq!(words(&closed.machine, slot.layout.state, 1), [50], "Reopen: handler state is intact");
    let frames = &closed.observed.frames;
    assert_eq!(frames.len(), 1 + 50 * 16 - lost + 1, "Reopen: Hello, retained events, one LOST, later events");
    assert_eq!(frames[0], open.observed.frames[0], "Reopen: boot Hello was retained");
    let reports: Vec<_> = frames.iter().enumerate().filter(|(_, f)| matches!(f.event, watch::Event::Lost { .. })).collect();
    assert_eq!(reports.len(), 1, "Reopen: one coalesced LOST report");
    assert_eq!(reports[0].0, 1 + kept, "Reopen: oldest accepted events precede LOST");
    assert_eq!(reports[0].1.event, watch::Event::Lost { count: lost as u32, saturated: false, first_tick },
        "Reopen: exact count, saturation and first tick");
    assert_eq!(reports[0].1.tick, last_tick, "Reopen: LOST header is the last loss, not report time");
    let expected_events: Vec<_> = open.observed.frames.iter().filter(|f| matches!(f.event, watch::Event::Write { .. })).collect();
    for (frame, source) in frames[1..=kept].iter().zip(0..kept)
        .chain(frames[kept + 2..].iter().zip(emitted..50 * 16))
    {
        assert_eq!(frame.event, expected_events[source].event, "Reopen: preserve event order and payload");
        assert_eq!(frame.tick, closed.observed.enters[source / 16].1 as u32,
            "Reopen: preserve the event's dispatch tick");
    }
    let mut bytes = 0usize; let mut wraps = 0;
    for (seq, frame) in frames.iter().enumerate() {
        assert_eq!(frame.seq, seq as u8, "Reopen: refused frames do not advance sequence");
        let mut wire = [0; watch::MAX_WIRE]; let n = watch::encode(frame, &mut wire);
        if bytes % runtime::TX_RING_BYTES as usize + n > runtime::TX_RING_BYTES as usize { wraps += 1; }
        bytes += n;
    }
    assert!(wraps > 0, "Ring wrap: at least one decoded frame straddled the ring boundary");
    let head = words(&closed.machine, slot.layout.tx_head, 1)[0];
    assert_eq!(head, bytes as u32 & (runtime::TX_RING_BYTES - 1), "Ring wrap: byte index wraps by the mask");
    assert_eq!(words(&closed.machine, slot.layout.tx_tail, 1), [head], "Reopen: the ring drained completely");
    assert_eq!(words(&closed.machine, slot.layout.lost_count, 1), [0], "Reopen: acknowledged loss cleared");
    assert!(closed.observed.usb_wakes > 0, "Interrupt use: host reads woke pending TX");
    assert_eq!(closed.machine.cpu().intenable & (1 << 2), 0, "Interrupt use: empty ring disables line 2");
    let wakes = closed.observed.usb_wakes;
    closed.until(slot, 490_000_000);
    assert_eq!(closed.observed.usb_wakes, wakes, "Interrupt use: later IN tokens cannot wake an empty ring");
    assert!(closed.machine.last_reset().is_none(), "No undocumented access: no watchdog recovery masked a stall");
    assert!(matches!(closed.machine.status(), Status::Sleeping { .. }),
        "No undocumented access: EP1/WR_DONE checks passed and firmware is sleeping");
}

#[test]
fn watch_enqueue_matches_eth_codec_at_block_boundaries_and_refuses_whole_frames() {
    // A tiny hardware-executable caller isolates the enqueue block boundaries
    // from the ETH ports, entering from the real boot stage and using its
    // linked CRC table. No simulator-only image behavior or peripheral bypass.
    let c = compiled("fn setup() {}");
    let m = module::load(&c.module, &chip::profile()).unwrap();
    let p = profile(71); let mut image = flash(&c, &m, &p);
    let l = &image.slot.layout;
    let enqueue = image.slot.map.runtime.iter().find(|r| r.name == "watch_enqueue").unwrap().start;
    let body = l.regions[0].start as u32;
    assert!(u64::from(body) + watch::ETH_BODY as u64 <= l.regions[0].end);
    let origin = runtime::TX_RING_BYTES - 2;
    const TICK: u32 = 0x1111_1111;
    let payloads: Vec<_> = [247, 248, watch::ETH_MAX, watch::ETH_MAX, watch::ETH_MAX,
        watch::ETH_MAX, watch::ETH_MAX, watch::ETH_MAX].into_iter().enumerate().map(|(k, n)| {
        let mut payload = vec![0x11; n];
        // The first body+CRC is exactly 254 nonzero bytes, testing the
        // codec's omission of an empty final block. The next splits at 254.
        // The third puts zeros on both sides of a code-byte boundary.
        if k == 2 { payload[247] = 0; payload[248] = 0; }
        let mut raw = vec![watch::ETH, 0x11 + k as u8];
        raw.extend(TICK.to_le_bytes()); raw.extend(&payload);
        if watch::crc8(&raw) == 0 { payload[n - 1] = 0x12; }
        payload
    }).collect();
    let raw: Vec<_> = payloads.iter().map(|payload| {
        let mut body = vec![watch::ETH, 0, 0, 0, 0, 0]; body.extend(payload); body
    }).collect();
    let prefix = (image.slot.bytes.len() as u32 + 3) & !3;
    let mut stops = Vec::new();
    let mut code = assemble(&mut Asm::new(raw.len() * 2, 11), |a| {
        stops.clear(); a.base.reserve(prefix); a.literal_table();
        for (address, value) in [(l.tx_head, origin), (l.tx_tail, origin), (l.tx_packet, 0), (l.seq, 0x11)] {
            a.mov_reg_i32(11, address); a.mov_reg_i32_optimised(2, value); a.s32i(2, 11, 0);
        }
        for (k, bytes) in raw.iter().enumerate() {
            a.mov_reg_i32(2, SLOT_DATA_BASE + a.base.label_offset(raw.len() + k).unwrap_or(0));
            a.mov_reg_i32(3, body); a.mov_reg_i32_optimised(4, bytes.len() as u32);
            a.base.label_assign(k);
            a.l8ui(5, 2, 0); a.s8i(5, 3, 0); a.addi(2, 2, 1); a.addi(3, 3, 1); a.addi(4, 4, -1);
            a.bccz_reg_label(ccz::NE, 4, k);
            a.mov_reg_i32_optimised(2, bytes.len() as u32); a.mov_reg_i32(3, body); a.mov_reg_i32(4, TICK);
            a.mov_reg_i32(11, enqueue); a.callx0(11); stops.push(SLOT_BASE + a.base.offset());
        }
        // INTENABLE is zero from the boot stage; WAITI 15 (ISA p.644) cannot
        // wake while the host reads the first automatic 64-byte packet.
        a.waiti(15); a.j(-4);
        for (k, bytes) in raw.iter().enumerate() {
            a.base.label_assign(raw.len() + k);
            for &byte in bytes { a.base.data(1, u64::from(byte)); }
        }
    });
    code[..image.slot.bytes.len()].copy_from_slice(&image.slot.bytes);
    // J's PC bias is four (emit32::xtensa, ISA Table 201 p.662).
    let jump = enc::j(prefix as i32 - (runtime::INIT - SLOT_BASE) as i32 - 4).to_le_bytes();
    let init = (runtime::INIT - SLOT_BASE) as usize; code[init..init + 3].copy_from_slice(&jump[..3]);
    image.record.length = code.len() as u32;
    image.record.sha256 = sha256(&code);
    image.record.wdt = 0; // This codec fixture sleeps without a dispatcher feed.
    let record = image.record.bytes(); let at = VOICEBOX.records[0] as usize;
    image.flash[at..at + record.len()].copy_from_slice(&record);
    image.flash.truncate(VOICEBOX.slots[0] as usize); image.flash.extend(code);
    let mut machine = S3Machine::new(Arc::new(image.flash), &p);
    machine.set_usb_host_listening(false);
    let mut marks = PcSet::new(SLOT_BASE, image.record.length);
    for &pc in &stops { marks.insert(pc); }
    let mut expected = Vec::new(); let mut before_refusal = None;
    for (k, &pc) in stops.iter().enumerate() {
        loop {
            let ran = machine.run(&Until { tick: u64::MAX, marks: &marks, stops: &stops,
                stops_after: machine.now(), on_drive: false });
            machine.mark();
            match ran {
                Ran::Pc => { assert_eq!(machine.pc(), pc, "each enqueue returns to its caller"); break; }
                Ran::Barrier => (),
                other => panic!("enqueue must return without undocumented access: {other:?}, {:?}", machine.status()),
            }
        }
        if k + 1 == stops.len() {
            assert_eq!(machine.cpu().ar[2], 0, "full ring refuses the entire maximum ETH frame");
            assert_eq!(Some((words(&machine, l.tx_head, 1), words(&machine, l.seq, 1),
                words(&machine, l.tx_ring, runtime::TX_RING_BYTES / 4))), before_refusal,
                "refusal changes neither ring bytes, head nor sequence");
        } else {
            assert_eq!(machine.cpu().ar[2], 1, "whole ETH frame fits");
            let mut wire = [0; watch::ETH_WIRE];
            let n = watch::encode_eth(0x11 + k as u8, TICK, &payloads[k], &mut wire).unwrap();
            expected.extend_from_slice(&wire[..n]);
            if k + 2 == stops.len() {
                before_refusal = Some((words(&machine, l.tx_head, 1), words(&machine, l.seq, 1),
                    words(&machine, l.tx_ring, runtime::TX_RING_BYTES / 4)));
            }
        }
    }
    machine.set_usb_host_listening(true);
    let deadline = machine.now() + machine.tick_hz() / 1000;
    loop {
        let ran = machine.run(&Until { tick: deadline, marks: &PcSet::default(), stops: &[],
            stops_after: 0, on_drive: false });
        machine.mark();
        match ran {
            Ran::Tick => break,
            Ran::Barrier => (),
            other => panic!("host read must preserve the sleeping codec fixture: {other:?}, {:?}", machine.status()),
        }
    }
    let mut out = Vec::new(); machine.take_out(&mut out);
    assert_eq!(out.len(), 64, "only the automatic first packet reached the host");
    let mut actual: Vec<_> = out.iter().map(|b| b.byte).collect();
    let head = words(&machine, l.tx_head, 1)[0]; let tail = words(&machine, l.tx_tail, 1)[0];
    let mut ring = vec![0; runtime::TX_RING_BYTES as usize]; assert!(machine.read_data(l.tx_ring, &mut ring));
    let count = head.wrapping_sub(tail) & (runtime::TX_RING_BYTES - 1);
    actual.extend((0..count).map(|n| ring[((tail + n) & (runtime::TX_RING_BYTES - 1)) as usize]));
    assert_eq!(actual, expected, "CRC, COBS zero/full/final blocks and ring wrap match the host codec byte for byte");
    assert_eq!(head, (origin + expected.len() as u32) & (runtime::TX_RING_BYTES - 1));
}

#[test]
fn frame_grid_and_instruction_costs() {
    let c = compiled("let out = output(D33)\nvar n: i32 = 0\nlet tick = every(ms(10), fn() { n = n + 1 write(out, n) })");
    let m = module::load(&c.module, &chip::profile()).unwrap();
    let image = flash(&c, &m, &profile(509)); let slot = &image.slot;
    let mut h = Harness::with_costs(&image, 509, true); h.until(slot, 26_000_000); h.check_frames(slot, &m);
    const PERIOD: u64 = 10_000 * 16;
    let origin = h.observed.timer_start.unwrap();
    let events: Vec<_> = h.observed.frames.iter().filter(|f| matches!(f.event, watch::Event::Write { .. })).collect();
    assert_eq!(events.len(), 2); assert_eq!(h.observed.enters.len(), 2);
    // Measure lateness at handler entry independently of watch output. This
    // is a bound for this run/profile, not an assumed hardware cycle bound.
    let latency = h.observed.enters.iter().enumerate().map(|(k, (_, stamp))|
        stamp.checked_sub(origin + (k as u64 + 1) * PERIOD).expect("never dispatch early")).max().unwrap();
    assert!(latency < PERIOD, "this fixture must not overrun");
    for (k, (frame, (_, stamp))) in events.iter().zip(&h.observed.enters).enumerate() {
        let due = origin + (k as u64 + 1) * PERIOD;
        assert_eq!(u64::from(frame.tick), *stamp, "port uses the dispatch stamp");
        assert!((due..=due + latency).contains(&u64::from(frame.tick)));
    }
    let table = words(&h.machine, slot.layout.timers, 8);
    assert_eq!(u64::from(table[0]) | (u64::from(table[1]) << 32), origin + 3 * PERIOD, "deadline stays on its original grid");
    assert_eq!(words(&h.machine, slot.layout.state, slot.layout.state_words), [2]);
    assert_eq!(slot.map.find(runtime::KERNEL_VECTOR), SlotWhere::Runtime("kernel_vector"));
    for op in &slot.map.handlers.ops {
        assert!(matches!(slot.map.find(op.start), SlotWhere::Handler(_)));
    }
    println!("measured dispatch latency bound: {latency} SYSTIMER ticks ({} ns, rounded up)", (latency * 125).div_ceil(2));
    h.observed.report();
}

#[test]
fn clocks_use_dispatch_stamp_and_time_frames_extend_it() {
    for (period, until) in [(10_000u32, 26_000_000u64), (1_500_000_000, 4_600_000_000_000)] {
        let source = format!(r#"
let out = output(D4)
let aux = output(D33)
var micro: i32 = 0
var milli: i32 = 0
let tick = every({period}, fn() {{
    micro = now_us()
    write(out, high)
    milli = now_ms()
    write(aux, high)
}})
"#);
        let c = compiled(&source); let m = module::load(&c.module, &chip::profile()).unwrap();
        let micro = m.vars().iter().find(|v| m.string(v.name) == "micro").unwrap().offset as usize;
        let milli = m.vars().iter().find(|v| m.string(v.name) == "milli").unwrap().offset as usize;
        let image = flash(&c, &m, &profile(1021)); let slot = &image.slot;
        let mut h = Harness::new(&image, 1021);
        h.until(slot, until); h.check_frames(slot, &m);
        assert_eq!(h.observed.returns.len(), if period == 10_000 { 2 } else { 3 });
        for (stamp, state) in &h.observed.returns {
            assert_eq!(state[micro], (stamp / 16) as u32, "us truncates after 64-bit conversion");
            assert_eq!(state[milli], (stamp / 16 / 1000) as u32, "ms divides before u32 truncation");
        }
        let mut high = 0u64; let mut reported = Vec::new(); let mut times = 0;
        for frame in &h.observed.frames {
            match frame.event {
                watch::Event::Time { tick_hi } => {
                    assert_ne!(high, u64::from(tick_hi), "Time precedes a new high word");
                    high = u64::from(tick_hi); times += 1;
                }
                watch::Event::Write { .. } => reported.push((high << 32) | u64::from(frame.tick)),
                _ => {}
            }
        }
        let expected: Vec<_> = h.observed.enters.iter().flat_map(|(_, stamp)| [*stamp, *stamp]).collect();
        assert_eq!(reported, expected, "both port calls retain their handler's entry stamp");
        if period > 10_000 {
            assert_eq!(times, 3);
            assert!(h.observed.returns.last().unwrap().0 / 16 > u64::from(u32::MAX));
        }
    }
}

// Independent listing oracle for the two watchdog fixtures below: no counted
// loops, no looping float helpers, and only IFs with an empty else arm. Every
// emitted body instruction can therefore be charged at its enclosing cap(s).
// Decode the final bytes, including EndLoop tails absent from the op spans;
// neither Handler::max_insns nor the production bound walker supplies a count.
/// Every RWDT feed store in a radio-free image's runtime (`s32i a10, a11, 0` with a11 at
/// RWDT_FEED when it runs), without changing the image or adding runtime observation hooks. Init
/// opens with the jump over the runtime's literal table (emit32 `literal_table`): the walk takes it.
/// The ROM shim (literals, then CALL0-to-windowed code that never feeds) is left out, as the vectors.
fn feed_stores(slot: &Slot) -> Vec<u32> {
    slot.map.runtime.iter()
        .filter(|r| r.end > r.start && !r.name.ends_with("vector") && !r.name.ends_with("vectors") && r.name != "rom_shim")
        .flat_map(|r| {
            let mut stores = Vec::new(); let mut pc = r.start;
            while pc < r.end {
                let at = (pc - SLOT_BASE) as usize;
                let mut bytes = [0; 4]; bytes[..3].copy_from_slice(&slot.bytes[at..at + 3]);
                let (insn, len) = decode(u32::from_le_bytes(bytes))
                    .unwrap_or_else(|_| panic!("runtime routine {} decodes at {pc:#x}", r.name));
                if matches!(insn.op, Op::S32i) && (insn.s, insn.t, insn.imm) == (11, 10, 0) { stores.push(pc); }
                pc = match insn.op {
                    Op::J if pc == runtime::INIT => target(&insn, pc).expect("a jump has a target"),
                    _ => pc + len,
                };
            }
            stores
        }).collect()
}

fn listed_watchdog_bound(slot: &Slot, m: &Module<'_>) -> u128 {
    use fab_elec_mcu_emit32::control::{steps, Analysis, Step};

    slot.handlers.iter().map(|h| {
        let entry = h.id as usize;
        let steps = steps(m, entry);
        let analysis = Analysis::new(m, entry, &steps);
        assert!(analysis.counted.iter().all(Option::is_none), "watchdog fixture uses caps");
        for step in steps {
            if let Step::Op(i) = step {
                if i.op == OP_IF {
                    let otherwise = i.at + 3 + m.code()[i.at as usize + 2];
                    assert_eq!(m.code()[otherwise as usize], 0, "watchdog fixture has empty else arms");
                }
                assert!(!(i.op == OP_BIN && i.sub == BIN_DIV_F || i.op == OP_UN && i.sub == UN_SQRT_F),
                    "watchdog fixture has no internally looping arithmetic");
            }
        }
        let ops: Vec<_> = slot.map.handlers.ops.iter().filter(|s| s.entry == entry).collect();
        let loops: Vec<_> = slot.map.handlers.loops.iter().filter(|s| s.entry == entry).collect();
        let (mut pc, end) = slot.map.handlers.entries[entry];
        let mut body = 0u128;
        while pc < end {
            let at = (pc - SLOT_BASE) as usize;
            let bytes = &slot.bytes[at..at + 3];
            let (_, len) = decode(u32::from(bytes[0]) | u32::from(bytes[1]) << 8 | u32::from(bytes[2]) << 16)
                .expect("watchdog fixture instruction decodes");
            let op = ops.iter().find(|s| (s.start..s.end).contains(&pc));
            let active: Vec<_> = loops.iter().filter(|s| (s.start..s.end).contains(&pc)).collect();
            if op.is_some() || !active.is_empty() {
                let visits: u128 = active.iter().map(|s| u128::from(m.code()[s.code as usize + 1])).product();
                body += visits;
                if op.is_some_and(|s| s.start == pc && s.site.is_some()) { body += visits * 2048; }
            }
            pc += len;
        }
        assert_eq!(pc, end);
        let bound = body + 32 + u128::from(h.stack_bytes - 16);
        assert_eq!(u128::from(h.max_insns), bound, "independent emitted bound for {}", h.name);
        bound
    }).max().unwrap_or(0)
}

#[test]
fn measured_watchdog_holds_cover_sleep_and_identify_a_slow_handler() {
    let c = compiled("var n: i32 = 0\nlet tick = after(ms(350), fn() { while n < 1023 { n = n + 1 } })");
    let m = module::load(&c.module, &chip::profile()).unwrap();
    // A handler that outlasts a hold patched below its length, after 2 ms of idle that drains
    // the Hello: the image sets its own CPU clock, so the record's hold is the contract the slow
    // run breaks.
    let stall_c = compiled("var n: i32 = 0\nlet tick = after(ms(2), fn() { for i in 0..200000 { n = (n + i) & 65535 } })");
    let stall_m = module::load(&stall_c.module, &chip::profile()).unwrap();
    let stall_id = stall_m.entry("tick").unwrap() as u32;
    let mut holds = Vec::new();
    for sel in 0..=3 {
        let mut p = profile(1); p.time.wdt_delay_sel = sel;
        let image = flash(&c, &m, &p); let slot = &image.slot;
        assert_eq!(slot.layout.timer_count, 1);
        // Independently count the emitted listing and retain the literal
        // fixed/runtime bounds and 100 ms; do not use wdt_hold here.
        let handler_insns = listed_watchdog_bound(slot, &m);
        let gap_insns = handler_insns + 2048 + 14;
        let gap_ns = (gap_insns * 1_000_000_000 + u128::from(CPU_HZ) - 1) / u128::from(CPU_HZ);
        let ns = 2 * (100_000_000u128 + gap_ns);
        let cycles = (ns * 136_000 + 999_999_999) / 1_000_000_000;
        let scale = 1u128 << (sel + 1);
        let expected = (cycles + scale - 1) / scale;
        assert_eq!(u128::from(image.record.wdt), expected, "independent hold for eFuse {sel}");
        holds.push(image.record.wdt);

        let mut fast = Harness::with_profile(&image, &p, false);
        fast.until(slot, 850_000_000); fast.check_frames(slot, &m);
        assert!(fast.machine.last_reset().is_none(), "computed hold covers armed and idle caps");
        assert_eq!(fast.observed.enters.len(), 1);
        assert_eq!(fast.observed.returns.len(), 1);
        assert_eq!(words(&fast.machine, slot.layout.state, 1), [1023]);
        assert!(fast.observed.vector_entries >= 7, "both armed and idle sleep wake repeatedly");

        // The quarter feed (wdt_hold's doc): init's quarter is the record's hold times the board's
        // quarter per hold, and with feed points at most S + G apart, feeds come at most
        // Q + S + G <= 3/4 of the hold's timeout T apart, so an RC slow clock up to 4/3 fast is covered.
        let per_hold = feed_quarter_per_hold(&BoardTime { rc_slow_hz: p.time.rc_slow_hz, wdt_delay_sel: sel });
        let quarter = u128::from(image.record.wdt) * u128::from(per_hold);
        assert_eq!(words(&fast.machine, slot.layout.rwdt_due + 4, 1), [quarter as u32], "init's quarter for eFuse {sel}");
        let timeout = (u128::from(image.record.wdt) << (sel + 1)) * u128::from(CPU_HZ) / 136_000;
        let stretch = 100_000_000 * u128::from(CPU_HZ) / 1_000_000_000 + gap_insns;
        assert!(4 * (quarter + stretch) <= 3 * timeout,
            "eFuse {sel}: Q {quarter} + S+G {stretch} cycles within 3/4 of T {timeout}");
        // In the model: real feeds no further apart than Q + S + G, and after init's own feeds no
        // more often than a quarter allows (the feed points between them only compare).
        let stores = feed_stores(slot);
        let init = slot.map.runtime.iter().find(|r| r.name == "init").unwrap();
        let mut marks = PcSet::new(SLOT_BASE, slot.bytes.len() as u32);
        for &pc in &stores { marks.insert(pc); }
        let mut run = S3Machine::new(Arc::new(image.flash.clone()), &p);
        let until = run.tick_hz() * 85 / 100;
        let mut fed = Vec::new();
        loop {
            match run.run(&Until { tick: until, marks: &marks, stops: &stores, stops_after: run.now(), on_drive: false }) {
                Ran::Pc => if run.cpu().ar[11] == 0x6000_80ac { fed.push((run.now(), (init.start..init.end).contains(&run.pc()))); },
                Ran::Barrier => {}
                Ran::Tick => break,
                ran => panic!("eFuse {sel}: the run feeds and sleeps: {ran:?}, {:?}", run.status()),
            }
        }
        let cycles = |ticks: u64| u128::from(ticks) * u128::from(CPU_HZ) / u128::from(run.tick_hz());
        let longest = fed.windows(2).map(|f| cycles(f[1].0 - f[0].0)).max().expect("several feeds");
        assert!(longest <= quarter + stretch, "eFuse {sel}: feeds {longest} cycles apart, bound {}", quarter + stretch);
        let gated: Vec<u64> = fed.iter().filter(|f| !f.1).map(|f| f.0).collect();
        let after_init = cycles(until - gated[0]);
        assert!((gated.len() as u128) <= after_init / quarter + 1,
            "eFuse {sel}: {} feeds after init in {after_init} cycles, a quarter is {quarter}", gated.len());
        assert!(run.last_reset().is_none(), "eFuse {sel}: the quarter feed keeps the RWDT fed");

        // Decode the feed immediately before each WAITI, without changing the image or adding
        // runtime observation hooks, and check each runs before unmasking.
        let dispatcher = slot.map.runtime.iter().find(|r| r.name == "dispatcher").unwrap();
        let mut insns = Vec::new(); let mut pc = dispatcher.start;
        while pc < dispatcher.end {
            let at = (pc - SLOT_BASE) as usize;
            let mut bytes = [0; 4]; bytes[..3].copy_from_slice(&slot.bytes[at..at + 3]);
            let (insn, len) = decode(u32::from_le_bytes(bytes)).expect("dispatcher instruction");
            insns.push((pc, insn)); pc += len;
        }
        let mut sleeps: Vec<_> = [slot.symbols.waiti, slot.symbols.idle_waiti].into_iter().map(|waiti| {
            let index = insns.iter().position(|(pc, _)| *pc == waiti).unwrap();
            let (pc, insn) = insns[index - 3];
            assert!(matches!(insn.op, Op::S32i));
            assert_eq!((insn.s, insn.t, insn.imm), (11, 10, 0));
            pc
        }).collect();
        sleeps.sort_unstable();
        let mut marks = PcSet::new(SLOT_BASE, slot.bytes.len() as u32);
        for &pc in &sleeps { marks.insert(pc); }
        let mut idle = S3Machine::new(Arc::new(image.flash.clone()), &p);
        let mut quiet = Observed::default(); let mut fed = [false; 2];
        while fed != [true; 2] {
            let ran = idle.run(&Until { tick: 2 * idle.tick_hz(), marks: &marks, stops: &sleeps,
                stops_after: idle.now(), on_drive: false });
            quiet.take(&mut idle);
            match ran {
                Ran::Barrier => {}
                Ran::Pc => {
                    // facts-machine.md §5 / rtc_cntl_reg.h:2101–2107.
                    assert_eq!(idle.cpu().ar[11], 0x6000_80ac);
                    assert_eq!(idle.cpu().ar[10], 1 << 31);
                    assert_eq!(idle.cpu().ps & 15, 15, "feed runs before unmasking");
                    fed[sleeps.iter().position(|&pc| pc == idle.pc()).unwrap()] = true;
                }
                _ => panic!("armed and idle sleep both feed first: {ran:?}, {:?}", idle.status()),
            }
        }

        let mut stall = flash(&stall_c, &stall_m, &p);
        let feeds = feed_stores(&stall.slot);
        assert!(!feeds.is_empty(), "the runtime feeds the RWDT");
        let handler = stall.slot.handlers[stall_id as usize].address;
        let mut stops = feeds.clone(); stops.extend([runtime::INIT, handler]); stops.sort_unstable();
        let mut marks = PcSet::new(SLOT_BASE, stall.slot.bytes.len() as u32);
        for &pc in &stops { marks.insert(pc); }
        // Deliberately violate the record's hold: about 4 ms of RTC_SLOW, past the 2 ms idle and
        // short of the handler's 1.2 M instructions (5 ms at CPU_HZ). R0 is rewritten with its
        // own checks.
        let hold = 544u32 >> (sel + 1);
        let short = Record { wdt: hold, ..stall.record.clone() };
        let at = VOICEBOX.records[0] as usize;
        stall.flash[at..at + 58].copy_from_slice(&short.bytes());
        let mut slow = S3Machine::new(Arc::new(stall.flash.clone()), &p);
        let mut observed = Observed::default(); let mut last_feed = None;
        let deadline = 60 * slow.tick_hz();
        loop {
            let ran = slow.run(&Until { tick: deadline, marks: &marks, stops: &stops,
                stops_after: slow.now(), on_drive: false });
            observed.take(&mut slow);
            match ran {
                Ran::Barrier => {}
                Ran::Pc if slow.pc() == handler => break,
                Ran::Pc if feeds.contains(&slow.pc()) => {
                    // facts-machine.md §5 / rtc_cntl_reg.h:2101–2107: only feeds store here.
                    if slow.cpu().ar[11] == 0x6000_80ac {
                        assert_eq!(slow.cpu().ar[10], 1 << 31);
                        last_feed = Some(slow.now());
                    }
                }
                Ran::Pc => {}
                _ => panic!("startup and idle must reach the handler within the short hold: {ran:?}, {:?}", slow.status()),
            }
        }
        assert_eq!(slow.cpu().ps & 15, 15, "the long handler is interrupt-masked");
        let last_feed = last_feed.expect("the runtime fed before the long handler");
        let scaled = u128::from(hold) << (sel + 1);
        let period = u128::from(slow.tick_hz()) * u128::from(p.time.rc_slow_hz.1);
        let num = u128::from(p.time.rc_slow_hz.0);
        // The model retains the slow-clock phase on feed (time.rs, hold/
        // schedule; emu/src/clock.rs, edge). Only its edge rounding is allowed.
        let edge = u128::from(last_feed) * num / period;
        let expected_tick = ((edge + scaled) * period).div_ceil(num) as u64;
        let ran = loop {
            let ran = slow.run(&Until { tick: expected_tick + 1, marks: &PcSet::default(), stops: &[],
                stops_after: slow.now(), on_drive: false });
            observed.take(&mut slow);
            if ran != Ran::Barrier { break ran; }
        };
        assert_eq!(ran, Ran::Reset { at: expected_tick }, "RWDT expires at its scaled slow-clock edge");
        let reset = slow.last_reset().unwrap();
        // facts-machine.md §§4–5: Core RWDT reset is cause 9 and retains RTC STORE0.
        assert_eq!((reset.scope, reset.cause), (ResetScope::Core, 9));
        assert_eq!((slow.now(), slow.pc()), (0, p.entry), "reset reenters Direct Boot");
        assert_eq!(observed.frames.len(), 1, "the handler never reaches a port or return feed");
        assert!(matches!(observed.frames[0].event, watch::Event::Hello { .. }));

        observed = Observed::default(); let mut handoff = false;
        while observed.frames.len() < 2 {
            let ran = slow.run(&Until { tick: deadline, marks: &marks, stops: &stops,
                stops_after: slow.now(), on_drive: false });
            observed.take(&mut slow);
            match ran {
                Ran::Barrier => {}
                Ran::Pc if slow.pc() == runtime::INIT => {
                    handoff = true;
                    assert_eq!(slow.cpu().ar[2] & 63, 9);
                    assert_eq!(slow.cpu().ar[3], stall_id, "stage preserves the offending handler id");
                }
                Ran::Pc => {}
                _ => panic!("recovered runtime must report the watchdog: {ran:?}, {:?}", slow.status()),
            }
        }
        assert!(handoff, "the real stage ran again");
        assert_eq!((observed.frames[0].seq, observed.frames[1].seq), (0, 1));
        assert!(matches!(observed.frames[0].event, watch::Event::Hello { mcusr: 9, .. }));
        assert_eq!(observed.frames[1].event, watch::Event::Fault {
            code: watch::FAULT_WATCHDOG, site: stall_id as u16, arg: 9,
        });
    }
    println!("computed holds for WDT_DELAY_SEL 0..=3: {holds:?}");
}

#[test]
fn watchdog_rounding_and_refusals() {
    // A 1 GHz slow-clock fixture makes nanoseconds exactly cycles. The
    // requested 4_294_967_282 cycles fit at sel 0, but round to 2^32 at sel 3.
    let gap_ns = 2_047_483_641;
    let mut time = BoardTime { rc_slow_hz: (1_000_000_000, 1), wdt_delay_sel: 0 };
    assert_eq!(wdt_hold(gap_ns, &time).unwrap(), 2_147_483_641);
    time.wdt_delay_sel = 3;
    let error = wdt_hold(gap_ns, &time).unwrap_err();
    assert!(error.message.contains("hold") && error.message.contains("4294967296")
        && error.message.contains("4294967295"), "{error}");
    time.wdt_delay_sel = 4;
    assert!(wdt_hold(0, &time).unwrap_err().message.contains("WDT_DELAY_SEL"));
    time.wdt_delay_sel = 0;
    for hz in [(0, 1), (1, 0), (0, 0)] {
        time.rc_slow_hz = hz;
        assert!(wdt_hold(0, &time).unwrap_err().message.contains("rc_slow_hz"));
    }
    time.rc_slow_hz = (136_001, 3);
    assert_eq!(wdt_hold(1, &time).unwrap(), 4534, "round both rational conversions upward");
    time.rc_slow_hz = (1, u64::MAX);
    assert_eq!(wdt_hold(0, &time).unwrap(), 1, "never silently disable RWDT");
    time.rc_slow_hz = (u64::MAX, 1);
    let error = wdt_hold(u64::MAX, &time).unwrap_err();
    assert!(error.message.contains("hold") && error.message.contains("4294967295"), "{error}");
}

#[test]
fn image_uses_board_layout_capacity_and_confirmed_record() {
    let c = compiled("let data = [int(11); 16384]\nvar n: i32 = 0\nfn setup() { n = data[n] }");
    let m = module::load(&c.module, &chip::profile()).unwrap();
    let p = profile(1);
    let mut board = Board { layout: Layout {
        flash_bytes: 10 * 65536, records: [0x3000, 0x4000],
        slots: [2 * 65536, 5 * 65536], slot_pages: 2,
        bundles: [8 * 65536, 9 * 65536], bundle_pages: 1,
        island: 4 * 65536, island_pages: 1,
        calibration: [0x7_0000, 0x7_1000], settings: [0x7_2000, 0x7_3000],
    }, panel: None, i2c: None, motion: None, touch: None, radio: None, time: BoardTime { rc_slow_hz: p.time.rc_slow_hz, wdt_delay_sel: p.time.wdt_delay_sel } };
    // The Board supplies capacity even when the caller's slot option disagrees.
    let options = SlotOptions { flash_limit_bytes: 1, ..SlotOptions::default() };
    let image = build_image(&m, &board, &options).unwrap();
    let layout = &board.layout;
    assert!(image.slot.bytes.len() > 65536, "shared data crosses the board's first slot page");
    assert_eq!(image.flash.len(), layout.slots[0] as usize + image.slot.bytes.len());
    assert_eq!(&image.flash[layout.slots[0] as usize..], &image.slot.bytes);
    let stage = fab_elec_mcu_emit_s3::boot_stage(layout);
    assert_eq!(&image.flash[..stage.len()], &stage);
    let at = layout.records[0] as usize;
    assert_eq!(&image.flash[at..at + 58], &image.record.bytes());
    assert!(image.flash[stage.len()..at].iter().all(|&b| b == 0xff));
    assert!(image.flash[at + 58..layout.slots[0] as usize].iter().all(|&b| b == 0xff),
        "R0's unused bytes, R1's whole sector and slot padding stay erased");
    let handler_insns = listed_watchdog_bound(&image.slot, &m);
    let gap_insns = handler_insns + 2048;
    let gap_ns = u64::try_from((gap_insns * 1_000_000_000).div_ceil(u128::from(CPU_HZ))).unwrap();
    assert_eq!(image.record, Record { seq: 1, slot: 0, prev: 0xff,
        wdt: wdt_hold(gap_ns, &board.time).unwrap(), length: image.slot.bytes.len() as u32, bundle_pages: 0,
        sha256: sha256(&image.slot.bytes), tries: 7, confirmed: true });

    board.layout.slot_pages = 1;
    let error = build_image(&m, &board, &SlotOptions::default()).err().expect("board slot capacity");
    assert!(error.message.contains("flash limit is 65536"), "{error}");
    board.layout.records[1] = board.layout.records[0];
    let error = build_image(&m, &board, &options).err().expect("invalid layout is a Refusal");
    assert!(error.message.contains("overlap"), "{error}");
}

// Small format fixtures, adapted from emit_avr/tests/emit.rs. A nonconstant
// local pin argument allows unavailable ports past the module's role checker
// so this tests build_slot's refusal, rather than front-end diagnostics.
fn fixture(body: &[u32], values: &[u8], vars: &[u8], state: Vec<u32>, sites: u32) -> Vec<u32> {
    let mut code = vec![values.len() as u32, vars.len() as u32, 0, 0];
    for types in [values, vars] {
        for bytes in types.chunks(4) {
            let mut word = [0; 4]; word[..bytes.len()].copy_from_slice(bytes);
            code.push(u32::from_le_bytes(word));
        }
    }
    code.push(body.len() as u32); code.extend_from_slice(body);
    let cost = chip::profile().max_entry_ops;
    let sections = [
        vec![state.len() as u32, 0, sites, 1, 1, 0, 0, 0], state, vec![],
        vec![5, u32::from_le_bytes(*b"setu"), b'p' as u32], vec![], vec![],
        vec![0, 0, code.len() as u32, cost as u32, (cost >> 32) as u32], code,
    ];
    let mut words = vec![0; HEADER_WORDS + SECTION_COUNT * DIRECTORY_WORDS];
    words[..HEADER_WORDS].copy_from_slice(&[MAGIC, FORMAT_VERSION, ports::PORT_TABLE_VERSION,
        chip::PROFILE_ID, 0, SECTION_COUNT as u32, 0, 0]);
    for (k, section) in sections.into_iter().enumerate() {
        let at = HEADER_WORDS + k * DIRECTORY_WORDS; let start = words.len() as u32;
        words[at..at + 3].copy_from_slice(&[k as u32 + 1, start, section.len() as u32]); words.extend(section);
    }
    words[H_WORDS] = words.len() as u32; words
}

#[test]
fn refuses_unavailable_ports_and_state_runtime_stack_overflow() {
    let c = fab_elec_mcu::compile("let data = output(11)\nlet clock = output(13)\nfn setup() { shift_out(data, clock, 1, 24) }", &chip::profile(), &[], &[])
        .expect("shift fixture compiles");
    let m = module::load(&c.module, &chip::profile()).expect("shift fixture verifies");
    let error = build_slot(&m, &SlotOptions::default()).err().expect("S3 shift is unavailable");
    assert_eq!(error.message, "`shift_out` is not available on the S3 yet", "S3 shift refusal names the unavailable port");
    let at = error.code as usize;
    assert_eq!(&m.code()[at..at + 2], &[OP_PORT, u32::from(ports::SHIFT_OUT)], "S3 shift refusal retains its code location");
    for port in [ports::PWM_DUTY, ports::SERIAL_BYTE] {
        let mut body = vec![OP_GET, 0, 0];
        if port == ports::PWM_DUTY {
            body.extend([OP_CONST, 1, 1, OP_PORT, u32::from(port), 0, 2, 0, 0, 1]);
        } else { body.extend([OP_PORT, u32::from(port), 0, 1, 1, 0, 1]); }
        let words = fixture(&body, &[TY_I32; 2], &[TY_I32], vec![], 1);
        let m = module::load(&words, &chip::profile()).unwrap();
        let error = build_slot(&m, &SlotOptions::default()).err().expect("unavailable port");
        assert!(error.message.contains(ports::PORTS[port as usize].name), "{error}");
    }
    // State plus runtime (including the TX ring) fits with 12 bytes left,
    // but the handler's minimum 16-byte CALL0 frame does not.
    let empty = fixture(&[], &[], &[], vec![], 0);
    let empty = module::load(&empty, &chip::profile()).unwrap();
    let runtime_words = build_slot(&empty, &SlotOptions::default()).unwrap().layout.runtime_bytes / 4;
    let state = vec![0; (chip::profile().ram_words - runtime_words - 3) as usize];
    let words = fixture(&[], &[], &[], state, 0);
    let m = module::load(&words, &chip::profile()).unwrap();
    let error = build_slot(&m, &SlotOptions::default()).err().expect("SRAM overflow");
    assert!(error.message.contains("largest handler stack") && error.message.contains("SRAM"), "{error}");
}

// R3-6b fixture: GPIO4 is neither the module's GPIO33 nor a boot-stage pin.
// facts-machine.md §2 source 16/+0x040 and §6: INT_TYPE=1 (rising),
// INT_ENA bit 13 (PRO normal), STATUS_W1TC +0x4c. pins.rs and intr.rs
// implement the same level source even though the pin detector is edge-triggered.
const RADIO_GPIO: usize = 4;
const RADIO_FIXTURE_DATA: u32 = 0x3fcc_0000;
const RADIO_FIXTURE_CODE: u32 = 0x4037_8000 + (RADIO_FIXTURE_DATA - 0x3fc8_8000);
const RADIO_FIXTURE_LOG: u32 = RADIO_FIXTURE_DATA + 0x1000;
const RADIO_PERIOD: u64 = 20_000 * 16;
const RADIO_INJECT_INSNS: u64 = 10_000;

struct RuntimeRadioFixture {
    /// Pre-boot data seed; small fixtures also execute through its SRAM alias.
    data_base: u32,
    bytes: Vec<u8>,
    /// Optional separate executable seed, addressed through the data alias.
    code: Option<(u32, Vec<u8>)>,
    entry: u32,
    isr: u32,
    isr_return: u32,
    record: u32,
    fault: Option<u32>,
}

fn runtime_radio_fixture(fault: bool) -> RuntimeRadioFixture {
    use fab_elec_mcu_emit32::xtensa::{assemble, enc, reg::*, Asm};
    use fab_elec_mcu_emit_s3::radio::{BLOCKED, READY, TCB_STATE, TCB_FRAME, CONTEXT_PC, YIELD_LINE};
    const GPIO: u32 = 0x6000_4000;
    const RECORD: usize = 0;
    let mut code = RuntimeRadioFixture { data_base: RADIO_FIXTURE_DATA, bytes: Vec::new(), code: None,
        entry: 0, isr: 0, isr_return: 0, record: 0, fault: None };
    code.bytes = assemble(&mut Asm::new(1, A9), |a| {
        a.literal_table();
        code.entry = RADIO_FIXTURE_CODE + a.base.offset();
        a.op24(enc::entry(A1, 64)); // ISA pp. 426–427; initial_frame supplies CALLINC=2.
        // Args after ENTRY: a2=isrs, a3=isr_count, a4=self, a5=dispatcher,
        // a6=log, a7=ISR. Keep a4..a6 across both inline blocks.
        a.op24(enc::rrr(0, 0, 0, 6, 15, A12));
        a.movi(A8, 1);
        a.s32i(A8, A2, 0); a.s32i(A7, A2, 4); a.s32i(A4, A2, 8);
        a.s32i(A8, A3, 0);
        a.op24(enc::rsr(0, 3, 0, 228, A9));
        a.or(A9, A9, A8); a.wsr(A9, 228); a.rsync();
        // Enable the digital input, clear handoff status before enabling its
        // normal interrupt, then connect the matrix. No SYSTIMER access.
        for (address, value) in [
            (0x6000_9004 + RADIO_GPIO as u32 * 4, (1 << 12) | (1 << 9)),
            (GPIO + 0x4c, 1 << RADIO_GPIO),
            (GPIO + 0x74 + RADIO_GPIO as u32 * 4, (1 << 7) | (1 << 13)),
            (0x600c_2000 + 16 * 4, 0),
        ] {
            a.mov_reg_i32(A8, address); a.mov_reg_i32(A9, value); a.s32i(A9, A8, 0);
        }
        a.wsr(A12, 230); a.rsync();
        a.movi(A8, BLOCKED as i32); a.s32i(A8, A4, TCB_STATE);
        a.movi(A8, (1 << YIELD_LINE) as i32); a.wsr(A8, 226); a.rsync();

        a.base.label_assign(RECORD);
        code.record = RADIO_FIXTURE_CODE + a.base.offset();
        a.l32i(A8, A6, 0); a.addi(A8, A8, 1); a.s32i(A8, A6, 0);
        a.l32i(A8, A5, TCB_FRAME); a.l32i(A8, A8, CONTEXT_PC); a.s32i(A8, A6, 4);
        if fault {
            code.fault = Some(RADIO_FIXTURE_CODE + a.base.offset());
            a.op24(0); // ILL after recording the preempted dispatcher's PC.
        }
        // Disarm the fixture line for the later tickless interval; preserve
        // dispatcher/yield ownership with a level-15 INTENABLE RMW.
        a.op24(enc::rrr(0, 0, 0, 6, 15, A12));
        a.op24(enc::rsr(0, 3, 0, 228, A8));
        a.movi(A9, -2); a.and(A8, A8, A9); a.wsr(A8, 228); a.rsync();
        a.wsr(A12, 230); a.rsync();
        a.movi(A8, BLOCKED as i32); a.s32i(A8, A4, TCB_STATE);
        a.movi(A8, (1 << YIELD_LINE) as i32); a.wsr(A8, 226); a.rsync();
        a.j_label(RECORD); // Any accidental second wake increments the counter.

        code.isr = RADIO_FIXTURE_CODE + a.base.offset();
        a.op24(enc::entry(A1, 64));
        a.mov_reg_i32(A3, GPIO + 0x4c); a.movi(A4, 1 << RADIO_GPIO); a.s32i(A4, A3, 0);
        a.movi(A3, READY as i32); a.s32i(A3, A2, TCB_STATE);
        a.mov_reg_i32(A3, RADIO_FIXTURE_LOG);
        a.l32i(A4, A3, 8); a.addi(A4, A4, 1); a.s32i(A4, A3, 8);
        code.isr_return = RADIO_FIXTURE_CODE + a.base.offset();
        a.op16(enc::retw_n());
    });
    assert!(code.bytes.len() <= 0x1000, "fixture text stays below its log");
    code
}

// Publish test state through real task stores. GPIO4 wakes one phase at a time;
// each phase readies the dispatcher and blocks itself before yielding. The host
// observes the drain but never writes SRAM after execution has started.
fn runtime_reports_fixture(l: &fab_elec_mcu_emit_s3::SramLayout, ring: &[u32], cases: &[(u32, u32, u32)]) -> RuntimeRadioFixture {
    use fab_elec_mcu_emit32::xtensa::reg::*;
    use fab_elec_mcu_emit_s3::radio::{BLOCKED, READY, TCB_STATE, YIELD_LINE};
    const GPIO: u32 = 0x6000_4000;
    const USB: u32 = 0x6003_8000; // USB Serial/JTAG: EP1 data at +0, EP1_CONF at +4 (bit 1: IN FIFO free).
    const ISR: usize = 0;
    const TX_BYTE: usize = 1;
    const TX_FULL: usize = 2;
    const PARK: usize = 3;
    let r = l.radio.as_ref().unwrap();
    let mut initial: Vec<_> = ring.iter().enumerate().map(|(i, &word)| (r.reports + i as u32 * 4, word)).collect();
    initial.push((l.time_sent, 1)); // First EVENT also sends Time through WIRE.
    let mut phases = vec![initial];
    for &(old, count, _) in cases {
        phases.push(vec![(l.timers + 16, 0), // No ordinary event may flush the artificial loss counts.
            (r.reports + 8, count), (l.lost_count, old),
            (l.lost_saturated, u32::from(old > 0x7fff_ffff)),
            (l.lost_first, 0x1234_5678), (l.lost_last, 0xabcd_0123)]);
    }
    phases.push(vec![(r.reports, 0), (r.reports + 4, 1), (r.reports + 8, 2),
        (r.reports + 12, 96), (r.reports + 16, 123), (r.reports + 20, 456),
        (l.lost_count, 0), (l.lost_saturated, 0)]);
    let mut code = RuntimeRadioFixture { data_base: RADIO_FIXTURE_DATA, bytes: Vec::new(), code: None,
        entry: 0, isr: 0, isr_return: 0, record: 0, fault: None };
    code.bytes = assemble(&mut Asm::new(4, A9), |a| {
        a.literal_table();
        code.entry = RADIO_FIXTURE_CODE + a.base.offset();
        a.op24(enc::entry(A1, 64));
        a.op24(enc::rrr(0, 0, 0, 6, 15, A8));
        for (address, value) in [(r.isrs, 1),
            (r.isrs + 4, RADIO_FIXTURE_CODE + a.base.label_offset(ISR).unwrap_or(0)),
            (r.isrs + 8, r.boot_tasks[0]), (r.isr_count, 1),
            (0x6000_9004 + RADIO_GPIO as u32 * 4, (1 << 12) | (1 << 9)),
            (GPIO + 0x4c, 1 << RADIO_GPIO),
            (GPIO + 0x74 + RADIO_GPIO as u32 * 4, (1 << 7) | (1 << 13)),
            (0x600c_2000 + 16 * 4, 0)]
        { a.mov_reg_i32(A8, address); a.mov_reg_i32(A9, value); a.s32i(A9, A8, 0); }
        a.op24(enc::rsr(0, 3, 0, 228, A8)); a.movi(A9, 1); a.or(A8, A8, A9);
        a.wsr(A8, 228); a.rsync();

        for (phase, stores) in phases.iter().enumerate() {
            a.op24(enc::rrr(0, 0, 0, 6, 15, A8));
            if phase == phases.len() - 1 {
                // The host closes USB before this phase. Fill the hardware
                // packet (at most 64 writes) so sleep's initial DRAIN cannot
                // free space in the synthetic full TX ring before EVENT.
                a.mov_reg_i32(A2, USB); a.movi(A4, 64);
                a.base.label_assign(TX_BYTE);
                a.l32i(A3, A2, 4); a.extui(A3, A3, 1, 1); a.bccz_reg_label(ccz::EQ, A3, TX_FULL);
                a.movi(A3, 0); a.s32i(A3, A2, 0); a.addi(A4, A4, -1);
                a.bccz_reg_label(ccz::NE, A4, TX_BYTE);
                a.base.label_assign(TX_FULL);
                a.mov_reg_i32(A2, l.tx_tail); a.l32i(A3, A2, 0); a.addi(A3, A3, -1);
                a.extui(A3, A3, 0, runtime::TX_RING_BYTES.ilog2() as u8);
                a.mov_reg_i32(A2, l.tx_head); a.s32i(A3, A2, 0);
            }
            for &(address, value) in stores {
                a.mov_reg_i32(A8, address); a.mov_reg_i32(A9, value); a.s32i(A9, A8, 0);
            }
            for (address, value) in [(RADIO_FIXTURE_LOG, phase as u32 + 1),
                (r.dispatcher + TCB_STATE, READY), (r.boot_tasks[0] + TCB_STATE, BLOCKED)]
            { a.mov_reg_i32(A8, address); a.mov_reg_i32_optimised(A9, value); a.s32i(A9, A8, 0); }
            a.movi(A8, (1 << YIELD_LINE) as i32); a.wsr(A8, 226);
            a.op24(enc::rrr(0, 0, 0, 6, 0, A8)); a.rsync();
            // Resume here only when GPIO's ISR readies this task again.
        }
        a.base.label_assign(PARK); a.j_label(PARK);

        a.base.label_assign(ISR); code.isr = RADIO_FIXTURE_CODE + a.base.offset();
        a.op24(enc::entry(A1, 64));
        a.mov_reg_i32(A3, GPIO + 0x4c); a.movi(A4, 1 << RADIO_GPIO); a.s32i(A4, A3, 0);
        a.movi(A3, READY as i32); a.s32i(A3, A2, TCB_STATE);
        a.mov_reg_i32(A3, RADIO_FIXTURE_LOG + 4);
        a.l32i(A4, A3, 0); a.addi(A4, A4, 1); a.s32i(A4, A3, 0);
        code.isr_return = RADIO_FIXTURE_CODE + a.base.offset(); a.op16(enc::retw_n());
    });
    assert!(code.bytes.len() <= 0x1000, "report fixture text stays below its log");
    code
}

fn flash_with_radio(c: &Compiled, m: &Module<'_>, p: &S3Profile, radio: &fab_elec_mcu_emit_s3::Radio) -> Image {
    let board = Board { layout: VOICEBOX, panel: None, i2c: None, motion: None, touch: None, radio: Some(radio.clone()),
        time: BoardTime { rc_slow_hz: p.time.rc_slow_hz, wdt_delay_sel: p.time.wdt_delay_sel } };
    build_image(m, &board, &SlotOptions { sites: c.sites.iter().map(|s| (s.line, s.col)).collect(),
        marks: c.marks.clone(), ..SlotOptions::default() }).unwrap()
}

fn runtime_instruction(slot: &Slot, pc: u32) -> (fab_elec_mcu_xtensa::insn::Insn, u32) {
    let mut bytes = [0; 4];
    let at = (pc - SLOT_BASE) as usize;
    bytes[..3].copy_from_slice(&slot.bytes[at..at + 3]);
    decode(u32::from_le_bytes(bytes)).expect("runtime instruction decodes")
}

impl Harness {
    // Reuse the same machine/watch harness, but seed SRAM before its first
    // mark and observe the capture itself for exact hardware deadlines.
    fn for_radio_proof(image: &Image, p: &S3Profile, fixture: Option<&RuntimeRadioFixture>) -> Self {
        let slot = &image.slot;
        let mut capture = slot.symbols.stamp;
        loop {
            assert!(capture < slot.symbols.stamp_end);
            let (insn, len) = runtime_instruction(slot, capture);
            if insn.op == Op::S32i {
                assert_eq!((insn.s, insn.t, insn.imm), (4, 5, 4), "first stamp store requests unit-0 capture");
                break;
            }
            capture += len;
        }
        let mut stops = vec![capture, slot.symbols.t0_ready]; stops.sort_unstable();
        let mut marks = PcSet::new(SLOT_BASE, slot.bytes.len() as u32);
        for &pc in &stops { marks.insert(pc); }
        marks.insert(runtime::KERNEL_VECTOR); marks.insert(runtime::USER_VECTOR);
        let island = if slot.symbols.radio.is_some() { island_fixture::small_island().image.as_slice() } else { &[] };
        let mut h = Self { machine: island_fixture::machine_with_island(&image.flash, island, p),
            observed: Observed::default(), marks, stops, epoch: 0 };
        if let Some(fixture) = fixture {
            let check_free = |base: u32, bytes: usize| {
                let end = u64::from(base) + bytes as u64;
                assert!(slot.layout.regions.iter().any(|r| r.used <= u64::from(base) && end <= r.end),
                    "fixture seed {base:#010x}..{end:#010x} must be in the image's free SRAM");
                assert!(end <= u64::from(slot.layout.stack_top - slot.layout.stack_bytes),
                    "fixture seed must stay below the dispatcher stack");
            };
            check_free(fixture.data_base, fixture.bytes.len());
            let log = fixture.data_base.checked_add(0x1000).unwrap();
            check_free(log, 12);
            if let Some((base, bytes)) = &fixture.code {
                check_free(*base, bytes.len());
                assert!(u64::from(*base) + bytes.len() as u64 <= u64::from(fixture.data_base),
                    "fixture code and data seeds must be disjoint");
                assert!(h.machine.seed_data(*base, bytes));
            }
            assert!(h.machine.seed_data(fixture.data_base, &fixture.bytes));
            assert!(h.machine.seed_data(log, &[0; 12]));
        }
        for pin in 1..h.machine.pins().len() { h.machine.set_input(pin, false); }
        loop {
            let ran = h.next(slot, h.machine.tick_hz());
            assert_ne!(ran, Ran::Tick, "init must reach t0");
            if ran == Ran::Pc && h.machine.pc() == capture { h.epoch = h.machine.now(); }
            if ran == Ran::Pc && h.machine.pc() == slot.symbols.t0_ready { break; }
        }
        assert_ne!(h.epoch, 0, "observed t0's actual capture instruction");
        h
    }
}

// s3/src/periph/time.rs: 40 MHz XTAL /3,/2, alternating 36/24 machine
// ticks. Count starts with handoff junk; a relative deadline depends only
// on the oscillator edge at t0's capture, not that unknown initial count.
fn radio_deadline(capture: u64, relative: u64, p: &S3Profile) -> u64 {
    let first = if p.time.systimer_first_long { 36 } else { 24 };
    let edge = capture / 60 * 2 + u64::from(capture % 60 >= first) + relative;
    edge / 2 * 60 + if edge & 1 != 0 { first } else { 0 }
}

#[derive(Default)]
struct RuntimeRadioRun {
    events: Vec<watch::Event>,
    state: Vec<u32>,
    enters: Vec<u64>,
    returns: Vec<u64>,
    handler_insns: Vec<u64>,
    idle: Vec<(usize, u64)>,
    interrupts: Vec<(u64, u32)>,
    latency: u64,
    injected_pc: Option<u32>,
    fixture_records: usize,
    longest_pending: u64,
    faults: usize,
}

fn run_radio_proof(image: &Image, p: &S3Profile, fixture: Option<&RuntimeRadioFixture>) -> (Harness, RuntimeRadioRun) {
    use fab_elec_mcu_emit_s3::radio::{BLOCKED, GUARD, TCB_STATE, YIELD_LINE};
    let slot = &image.slot;
    assert_eq!(slot.handlers.len(), 1);
    let handler = &slot.handlers[0];
    let (start, end) = slot.map.handlers.entries[0];
    assert_eq!(start, handler.address);
    let mut h = Harness::for_radio_proof(image, p, fixture);
    let mut result = RuntimeRadioRun::default();
    let cycle = h.machine.tick_hz() / u64::from(CPU_HZ);
    let window = 4 * RADIO_PERIOD + 10_000 * 16;
    let mut deadline = radio_deadline(h.epoch, window, p);
    let mut active = false;
    let mut handler_insns = 0;
    let mut since = None;
    let mut steps = 0u32;
    let sender = slot.map.runtime.iter().find(|r| r.name == "radio_record").map(|r| r.start);
    let mut report_call: Option<(u32, u64, u32, bool)> = None;
    let level1 = h.machine.cpu().config.int_level.iter().enumerate()
        .filter(|(_, level)| **level == 1).fold(0u32, |mask, (line, _)| mask | (1 << line));
    while h.machine.now() < deadline {
        let pc = h.machine.pc();
        let retired = h.machine.cpu().instret;
        let executing = h.machine.cpu().will_execute();
        if executing {
            if pc == slot.symbols.timer_start {
                h.observed.capture_timer_start(&h.machine);
                deadline = radio_deadline(h.epoch, h.observed.timer_start.unwrap() + window, p);
            }
            if report_call.is_some_and(|(return_pc, _, _, _)| pc == return_pc) {
                let (_, begin, first, absent) = report_call.take().unwrap();
                let r = slot.layout.radio.as_ref().unwrap();
                let next = words(&h.machine, r.report_next, 1)[0];
                assert!((first..=first + 2).contains(&next), "each idle sender advances by at most two accepted frames");
                if absent {
                    assert_eq!(retired - begin, 9, "an absent record costs nine routine instructions, plus two for the caller");
                    assert_eq!(next, first, "the empty check leaves the report cursor untouched");
                }
            }
            if Some(pc) == sender {
                let r = slot.layout.radio.as_ref().unwrap();
                report_call = Some((h.machine.cpu().ar[0], retired, words(&h.machine, r.report_next, 1)[0],
                    words(&h.machine, r.fault, 1)[0] == 0));
            }
        }
        // Exception entry itself retires nothing. With a one-instruction
        // run, the next observable boundary follows the vector's first WSR.
        let vector = if slot.symbols.radio.is_some() { runtime::USER_VECTOR } else { runtime::KERNEL_VECTOR };
        let entered = pc == vector + 3;
        let pending = h.machine.cpu().interrupt & h.machine.cpu().intenable & level1;
        if entered {
            if h.machine.cpu().exccause == 4 {
                assert_ne!(pending, 0);
                result.interrupts.push((h.machine.now() - cycle, pending));
            } else {
                assert_eq!(h.machine.cpu().exccause, 0, "fixture fault is ILL");
                assert_eq!(Some(h.machine.cpu().epc1), fixture.and_then(|f| f.fault), "only the planned island fault is allowed");
                result.faults += 1;
            }
        }
        if pending == 0 || entered {
            if let Some(begin) = since.take() {
                let until = retired - u64::from(entered);
                result.longest_pending = result.longest_pending.max(until - begin);
            }
        } else if since.is_none() { since = Some(retired); }

        if let Some(s) = &slot.symbols.radio {
            assert!(![s.no_task, s.task_exit, s.stack_fault, s.fatal].contains(&pc), "unexpected radio fault PC {pc:#x}");
            if [s.fault_entry, SLOT_BASE + fab_elec_mcu_emit_s3::radio::USER_UNHANDLED_OFFSET as u32].contains(&pc) {
                assert_eq!(Some(h.machine.cpu().epc1), fixture.and_then(|f| f.fault), "fault entry belongs to the planned island ILL");
            }
            if executing && pc == s.idle {
                let r = slot.layout.radio.as_ref().unwrap();
                assert_eq!(words(&h.machine, r.current, 1), [r.idle]);
                assert_eq!(runtime_instruction(slot, pc).0.op, Op::Waiti);
                if result.returns.len() >= 1 {
                    assert_eq!(h.machine.cpu().intenable & !(runtime::DISPATCHER_LINES | (1 << YIELD_LINE)), 0,
                        "island line is disarmed during tickless sleeps");
                }
                result.idle.push((result.returns.len(), h.machine.now()));
            }
        }
        if executing && pc == handler.address {
            assert!(!active, "dispatcher handlers never nest");
            active = true; handler_insns = 0;
            let due = radio_deadline(h.epoch, h.observed.timer_start.unwrap() + (result.enters.len() as u64 + 1) * RADIO_PERIOD, p);
            result.latency = result.latency.max(h.machine.now().checked_sub(due).expect("no early handler"));
            result.enters.push(h.machine.now());
            assert_eq!(h.machine.cpu().ps & 15, if fixture.is_some() { 0 } else { 15 });
        }
        if executing && active && pc == slot.symbols.handler_return {
            active = false;
            result.handler_insns.push(handler_insns);
            result.returns.push(h.machine.now());
        }
        if let Some(f) = fixture {
            if result.injected_pc.is_none() && active && handler_insns == RADIO_INJECT_INSNS {
                assert!((start..end).contains(&pc), "raise GPIO inside the long loop, before its port call");
                assert!(executing);
                assert_eq!(result.enters.len(), 1);
                let r = slot.layout.radio.as_ref().unwrap();
                assert_eq!(words(&h.machine, r.current, 1), [r.dispatcher]);
                assert_eq!(words(&h.machine, r.boot_tasks[0] + TCB_STATE, 1), [BLOCKED]);
                assert_eq!(words(&h.machine, r.isr_count, 1), [1]);
                assert_eq!(h.machine.cpu().intenable & 1, 1);
                result.injected_pc = Some(pc);
                h.machine.set_input(RADIO_GPIO, true);
                assert_eq!(h.machine.cpu().interrupt & 1, 1, "GPIO4 edge reaches CPU line 0");
                since.get_or_insert(retired);
            }
            if executing && pc == f.isr_return {
                assert_eq!(words(&h.machine, RADIO_FIXTURE_LOG + 8, 1), [1]);
                assert_eq!(h.machine.cpu().interrupt & 1, 0, "ISR W1TC lowered the level source");
                h.machine.set_input(RADIO_GPIO, false);
            }
            if executing && pc == f.record {
                assert_eq!(words(&h.machine, RADIO_FIXTURE_LOG + 8, 1), [1], "task runs after its ISR");
                result.fixture_records += 1;
            }
        }
        // Only instructions actually executed in handler text count toward
        // the long-loop threshold; injecting an IRQ prevents this instruction.
        let in_handler = active && (start..end).contains(&pc) && h.machine.cpu().will_execute();
        let tick = h.machine.now();
        // Skip sleeping time to the next model event, stopping before its
        // first instruction. Running code is still observed one instruction
        // at a time, including masked sections and exception vectors.
        let until = match h.machine.status() {
            Status::Sleeping { wake: Some(wake) } if wake > tick => wake.min(deadline),
            Status::Sleeping { wake: None } => deadline,
            _ => tick + 1,
        };
        let ran = loop {
            let ran = h.machine.run(&Until { tick: until, marks: &h.marks, stops: &[],
                stops_after: tick, on_drive: false });
            match ran {
                Ran::Tick => break ran,
                Ran::Barrier => {
                    h.observed.take(&mut h.machine);
                    if h.machine.cpu().instret != retired { break ran; }
                }
                _ => panic!("radio proof stopped: {ran:?}, {:?}", h.machine.status()),
            }
        };
        if in_handler { handler_insns += h.machine.cpu().instret - retired; }
        steps += 1;
        if ran == Ran::Barrier || steps % 256 == 0 { h.observed.take(&mut h.machine); }
    }
    if let Some(begin) = since { result.longest_pending = result.longest_pending.max(h.machine.cpu().instret - begin); }
    h.observed.take(&mut h.machine);
    assert!(!active, "last long handler finished");
    assert_eq!(result.enters.len(), 4); assert_eq!(result.returns.len(), 4);
    assert!(result.handler_insns.iter().all(|&n| n >= u64::from(CPU_HZ) / 1000),
        "every long handler executes at least 1 ms of instructions: {:?}", result.handler_insns);
    assert!(h.machine.last_reset().is_none());
    assert_eq!(h.observed.trace.iter().filter(|(_, pc)| *pc == runtime::USER_VECTOR || *pc == runtime::KERNEL_VECTOR).count(),
        result.interrupts.len() + result.faults, "every interrupt and fault entry was counted");
    for (seq, frame) in h.observed.frames.iter().enumerate() {
        assert_eq!(frame.seq, seq as u8, "radio proof preserves the watch sequence");
        assert!(!matches!(frame.event, watch::Event::Lost { .. }), "radio reporting never emits Lost");
    }
    // RadioRecord is intentionally excluded: this comparison concerns only
    // program-visible Write/Fault events, with or without a radio fault.
    result.events = h.observed.frames.iter().filter_map(|f| matches!(f.event,
        watch::Event::Write { .. } | watch::Event::Fault { .. }).then_some(f.event)).collect();
    result.state = words(&h.machine, slot.layout.state, slot.layout.state_words);
    if let Some(r) = &slot.layout.radio {
        assert_eq!(words(&h.machine, r.stray, 1), [0]);
        for base in [slot.layout.stack_top - slot.layout.stack_bytes, r.idle_stack.base, r.irq_stack.base]
            .into_iter().chain(r.boot_stacks.iter().map(|s| s.base))
        { assert_eq!(words(&h.machine, base, 1), [GUARD], "guard at {base:#x}"); }
    }
    (h, result)
}

#[test]
fn radio_profile_preempts_a_long_handler_and_idles_tickless() {
    radio_runtime_proof(false);
}

#[test]
fn radio_profile_survives_an_island_fault_and_keeps_idling_tickless() {
    radio_runtime_proof(true);
}

// Continue the real S3/watch path without the long-handler instruction observer.
// Stops are explicit; the radio's USER_VECTOR must not use Observed::at's
// radio-off WAITI assumptions.
fn radio_advance(h: &mut Harness, deadline: u64, stops: &[u32]) -> Ran {
    for &pc in stops { h.marks.insert(pc); }
    loop {
        let ran = h.machine.run(&Until { tick: deadline, marks: &h.marks, stops,
            stops_after: h.machine.now(), on_drive: false });
        h.observed.take(&mut h.machine);
        match ran {
            Ran::Barrier => {}
            Ran::Pc | Ran::Tick => return ran,
            _ => panic!("radio continuation stopped: {ran:?}, {:?}", h.machine.status()),
        }
    }
}

fn collect_runtime_radio_record(h: &Harness, slot: &Slot) -> Vec<u32> {
    use fab_elec_mcu_emit_s3::radio::{RecordCollector, FAULT_FRAMES, FAULT_WORDS};
    let mut collector = RecordCollector::new();
    let mut indices = Vec::new();
    let mut records = Vec::new();
    for (seq, frame) in h.observed.frames.iter().enumerate() {
        assert_eq!(frame.seq, seq as u8, "fault transport preserves the sequence even across retries");
        assert!(!matches!(frame.event, watch::Event::Lost { .. }), "fault transport never emits Lost");
        if let watch::Event::RadioRecord { index, .. } = frame.event { indices.push(index); }
        if let Some(record) = collector.push(frame.event).expect("runtime record frames are consistent") { records.push(record); }
    }
    assert_eq!(indices, (0..FAULT_FRAMES as u16).collect::<Vec<_>>(), "all record frames arrive once, in order, without skipped retries");
    assert_eq!(records.len(), 1, "the collector yields exactly one complete runtime record");
    let record = records.pop().unwrap();
    assert_eq!(record, words(&h.machine, slot.layout.radio.as_ref().unwrap().fault, FAULT_WORDS),
        "the transported record equals SRAM word for word");
    record
}

// Wait for delivery and a subsequent exhausted sender call before measuring
// tickless idle. USB may legitimately wake the dispatcher while a report drains.
fn finish_radio_report(h: &mut Harness, slot: &Slot) {
    use fab_elec_mcu_emit_s3::radio::FAULT_FRAMES;
    let r = slot.layout.radio.as_ref().unwrap();
    let idle = slot.symbols.radio.as_ref().unwrap().idle;
    let deadline = h.machine.now() + h.machine.tick_hz();
    let mut completed_idles = 0;
    while completed_idles < 2 {
        assert_eq!(radio_advance(h, deadline, &[idle]), Ran::Pc, "the bounded report completes within one second of model time");
        let received = h.observed.frames.iter().filter(|f| matches!(f.event, watch::Event::RadioRecord { .. })).count();
        if received == FAULT_FRAMES as usize
            && words(&h.machine, r.report_next, 1) == [FAULT_FRAMES]
            && words(&h.machine, slot.layout.tx_head, 1) == words(&h.machine, slot.layout.tx_tail, 1)
        { completed_idles += 1; }
    }
    assert!(h.machine.last_reset().is_none(), "fault reporting stays inside the watchdog budget");
}

#[test]
fn radio_image_osi_calls_use_its_kernel_and_enter_linked_phy() {
    use fab_elec_mcu_emit32::xtensa::reg::*;
    use fab_elec_mcu_emit_s3::{BootTask, Radio, radio::{self, TaskAbi,
        adapter::{OSI_FIELDS, OSI_MAGIC, OSI_VERSION},
        kernel::gates::EXIT_LEVEL_EXPORTS}};
    let c = compiled("fn setup() {}");
    let m = module::load(&c.module, &chip::profile()).unwrap();
    let p = profile(6382);
    let mut radio = Radio { start: None, restart: fab_elec_mcu_emit_s3::image::RestartPolicy::PROVISIONAL,
        island: island_fixture::small_island().binding(), dispatcher_priority: 2, irq_stack_bytes: 2048,
        task_capacity: 3, isr_capacity: radio::PACK_ISR_LINES, heap_bytes: 4096, timer_reserve: 8,
        burst_insns: 2048, trace_entries: 8,
        boot_tasks: vec![BootTask { entry: RADIO_FIXTURE_CODE, abi: TaskAbi::Windowed,
            priority: 5, stack_bytes: 8192, args: [0; 6] }] };
    for (heap, isrs) in [(47, radio.isr_capacity), (56, radio.isr_capacity),
        (radio.heap_bytes + 1, radio.isr_capacity), (radio.heap_bytes, radio::PACK_ISR_LINES - 1)] {
        let invalid = Radio { heap_bytes: heap, isr_capacity: isrs, ..radio.clone() };
        assert!(build_slot(&m, &SlotOptions { radio: Some(invalid), ..SlotOptions::default() }).is_err(),
            "heap control/alignment and ISR capacity are checked before compilation");
    }
    let first = flash_with_radio(&c, &m, &p, &radio);
    let r = first.slot.layout.radio.as_ref().unwrap();
    let index = |field: &str| OSI_FIELDS.iter().position(|&(name, _)| name == field).unwrap() as u32;
    // Exactly the pack ABI: fetch a function pointer from the image's table
    // and CALLX8. The MAC destination is in this task's checked stack span.
    let call = |a: &mut Asm, field: &str| {
        a.mov_reg_i32(A8, r.osi_table); a.l32i(A8, A8, index(field) * 4); a.callx8(A8);
    };
    let mut fixture = RuntimeRadioFixture { data_base: RADIO_FIXTURE_DATA, bytes: Vec::new(), code: None,
        entry: 0, isr: 0, isr_return: 0, record: 0, fault: None };
    fixture.bytes = assemble(&mut Asm::new(1, A9), |a| {
        a.literal_table(); fixture.entry = RADIO_FIXTURE_CODE + a.base.offset();
        a.entry(A1, 96); a.mov_reg_i32(A2, RADIO_FIXTURE_LOG);
        a.movi(A10, 48); call(a, "_malloc"); a.mov_n(A3, A10); a.s32i(A10, A2, 0);
        a.mov_n(A10, A3); call(a, "_free");
        a.movi(A10, 48); call(a, "_malloc"); a.s32i(A10, A2, 4);
        a.movi(A10, 1); a.movi(A11, 0); call(a, "_semphr_create");
        a.mov_n(A4, A10); a.s32i(A10, A2, 8);
        a.mov_n(A10, A4); call(a, "_semphr_give"); a.s32i(A10, A2, 12);
        a.mov_n(A10, A4); a.movi(A11, 0); call(a, "_semphr_take"); a.s32i(A10, A2, 16);
        a.mov_n(A10, A4); a.movi(A11, 0); call(a, "_semphr_take"); a.s32i(A10, A2, 20);
        a.addi(A10, A1, 32); a.movi(A11, 0); call(a, "_read_mac"); a.s32i(A10, A2, 24);
        a.l32i(A8, A1, 32); a.s32i(A8, A2, 28);
        a.l16ui(A8, A1, 36); a.s32i(A8, A2, 32);
        // The live semaphore follows the 48-byte block and forces this growth to move.
        for i in 0..12 {
            a.movi(A8, 0x451 + i as i32); a.s32i(A8, A3, i * 4);
        }
        a.mov_n(A10, A3); a.movi(A11, 81); call(a, "_wifi_realloc"); a.s32i(A10, A2, 40);
        a.movi(A11, 97); call(a, "_realloc_internal"); a.s32i(A10, A2, 44);
        // Reuse the dirty 48-byte source freed by the moved realloc.
        a.movi(A10, 3); a.movi(A11, 16); call(a, "_wifi_calloc"); a.s32i(A10, A2, 48);
        call(a, "_phy_enable");
        a.retw_n();
    });
    assert!(fixture.bytes.len() < 0x1000, "fixture text stays below results");
    fixture.bytes.resize(0x1000 + 52, 0);
    radio.boot_tasks[0].entry = fixture.entry;
    let image = flash_with_radio(&c, &m, &p, &radio);
    assert_eq!(image.slot.symbols.radio, first.slot.symbols.radio);
    assert_eq!(image.slot.layout.radio, first.slot.layout.radio);
    let r = image.slot.layout.radio.as_ref().unwrap();
    let symbols = image.slot.symbols.radio.as_ref().unwrap();
    let largest = symbols.gate_reports.iter().max_by_key(|g| g.max_insns).unwrap();
    // Heap size changes no non-timer gate's bound: TLSF allocation and release have no loop over the class tables.
    let larger = build_slot(&m, &SlotOptions {
        radio: Some(Radio { heap_bytes: 64 * 1024, ..radio.clone() }), ..SlotOptions::default()
    }).unwrap();
    let larger_gates = &larger.symbols.radio.as_ref().unwrap().gate_reports;
    for name in ["malloc", "calloc_reserve", "realloc_reserve"] {
        let bound = |gates: &[radio::kernel::gates::GateReport]| gates.iter().find(|g| g.name == name).unwrap().max_insns;
        assert_eq!(bound(&symbols.gate_reports), bound(larger_gates), "{name} bound across heap sizes");
    }
    for gate in symbols.gate_reports.iter().filter(|g| !g.name.starts_with("timer_")) {
        let other = larger_gates.iter().find(|g| g.name == gate.name)
            .unwrap_or_else(|| panic!("missing {} gate at 64 KiB", gate.name));
        assert_eq!(gate.max_insns, other.max_insns, "{} bound across heap sizes", gate.name);
    }
    assert_eq!(u64::from(symbols.crit_span_report_cycles), largest.max_insns,
        "critical section threshold is the largest gate actually emitted");
    let charged = symbols.gate_reports.iter().map(|g| g.max_insns + radio::thunk::masked_insns(g.blocks,
            EXIT_LEVEL_EXPORTS.contains(&g.name.as_str()), true)).max().unwrap();
    assert_eq!(symbols.masked_gate_insns, charged);
    let before_charge = u128::from(image.slot.handlers.iter().map(|h| h.max_insns).max().unwrap_or(0))
        + u128::from(runtime::RUNTIME_GAP_INSNS)
        + u128::from(image.slot.layout.timer_count) * u128::from(runtime::TIMER_SCAN_INSNS)
        + u128::from(radio.burst_insns) + 2 * u128::from(runtime::radio_switch_insns(&radio));
    assert_eq!(image.gap_insns - before_charge, u128::from(charged), "RWDT charges exactly one masked gate");
    let gap_ns = u64::try_from((image.gap_insns * 1_000_000_000).div_ceil(u128::from(CPU_HZ))).unwrap();
    assert_eq!(image.record.wdt, wdt_hold(gap_ns, &BoardTime {
        rc_slow_hz: p.time.rc_slow_hz, wdt_delay_sel: p.time.wdt_delay_sel }).unwrap());
    for blob in &symbols.blobs { eprintln!("{}: {} bytes (including alignment)", blob.name, blob.end - blob.start); }
    eprintln!("radio SRAM: state {}, heap {}, phy_cal_data 1904, phy_dig_regs 84, phy_mutex {}, osi_table {}; largest {} = {}, RWDT charge {}",
        r.kernel_state_words * 4, radio.heap_bytes, radio::kernel::MUTEX_BYTES, OSI_FIELDS.len() * 4,
        largest.name, largest.max_insns, charged);
    let mut h = Harness::for_radio_proof(&image, &p, Some(&fixture));
    assert_eq!(words(&h.machine, r.osi_table, 1), [OSI_VERSION]);
    assert_eq!(words(&h.machine, r.osi_table + 119 * 4, 1), [OSI_MAGIC]);
    assert_eq!(words(&h.machine, symbols.crit_span_report, 1), [symbols.crit_span_report_cycles],
        "init copies the measured threshold into the kernel's state");
    assert_eq!(words(&h.machine, symbols.task_exit_link, 1), [radio::windowed_exit_link(symbols.task_exit)],
        "new tasks return through this image's task_exit");
    let phy = words(&h.machine, symbols.phy_init_data, 1)[0];
    let offset = phy.checked_sub(SLOT_DATA_BASE).expect("PHY init data must be in slot DROM") as usize;
    let table = &radio::phy_init_data::PHY_INIT_DATA;
    assert_eq!(image.slot.bytes.get(offset..offset + table.len()), Some(table.as_slice()),
        "the initialized PHY pointer addresses this image's init-data table");
    for (address, bytes) in [(r.heap, radio.heap_bytes), (r.phy_cal_data, 1904), (r.phy_dig_regs, 84),
        (r.phy_mutex, radio::kernel::MUTEX_BYTES)] {
        assert!(address >= r.start && address + bytes <= r.end);
        assert!(words(&h.machine, address, bytes / 4).iter().all(|&word| word == 0), "radio allocation is cleared");
    }
    assert_eq!(r.heap & 7, 0);
    let deadline = h.machine.now() + h.machine.tick_hz();
    let lock = symbols.gate_reports.iter().find(|g| g.name == "phy_lock").unwrap().address;
    assert_eq!(radio_advance(&mut h, deadline, &[lock]), Ran::Pc, "linked PHY enable calls its kernel lock gate");
    let result = words(&h.machine, RADIO_FIXTURE_LOG, 13);
    for &pointer in &result[..3] {
        assert!(pointer >= r.heap && pointer < r.heap + radio.heap_bytes, "pack receives a heap pointer");
    }
    assert_eq!(result[0], result[1], "free makes the same allocation available again");
    assert_eq!(&result[3..7], &[1, 1, 0, 0], "give/take succeed, empty take fails, read_mac returns ESP_OK");
    let mut mac = result[7].to_le_bytes().to_vec(); mac.extend_from_slice(&result[8].to_le_bytes()[..2]);
    assert_eq!(mac, p.mac, "pack sees the model's eFuse MAC in network order");
    assert_ne!(result[10], result[1], "the live neighbour forces realloc to move");
    assert_eq!(result[11], result[10], "realloc grows in place into the free tail");
    assert_eq!(words(&h.machine, result[11], 12), (0x451..0x451 + 12).collect::<Vec<_>>(),
        "both reallocs preserve all 48 original bytes");
    assert_eq!(result[12], result[1], "calloc reuses the dirty source block");
    assert_eq!(words(&h.machine, result[12], 12), [0; 12], "calloc clears the reused block");
    assert_eq!(words(&h.machine, r.osi_table + index("_phy_enable") * 4, 1), [radio.island.glue["phy_enable_glue"]]);
    assert!(h.machine.last_reset().is_none(), "kernel initialization and calls stay within RWDT");
}

#[test]
fn radio_critical_sections_hold_pending_interrupts_at_every_boundary() {
    use fab_elec_mcu_emit32::xtensa::reg::*;
    use fab_elec_mcu_emit_s3::{BootTask, Radio, radio::{TaskAbi, YIELD_LINE}};
    // s3/src/bus.rs: SRAM1's instruction and data aliases share storage.
    const SRAM_DATA: u32 = 0x3fc8_8000;
    const SRAM_CODE: u32 = 0x4037_8000;
    const DATA_BYTES: u32 = 0x2000;
    const GPIO: u32 = 0x6000_4000;
    const ISR: usize = 0;
    const PARK: usize = 1;
    let c = compiled("let out = output(D33)\nlet tick = every(ms(20), fn() { write(out, 1) })");
    let m = module::load(&c.module, &chip::profile()).unwrap();
    let p = profile(6381);
    let mut radio = Radio { start: None, restart: fab_elec_mcu_emit_s3::image::RestartPolicy::PROVISIONAL,
        island: island_fixture::small_island().binding(), dispatcher_priority: 2, irq_stack_bytes: 2048, task_capacity: 3,
        isr_capacity: 1, heap_bytes: 4096, timer_reserve: 8, burst_insns: 2048, trace_entries: 0,
        boot_tasks: vec![BootTask { entry: RADIO_FIXTURE_CODE, abi: TaskAbi::Windowed,
            priority: 5, stack_bytes: 4096, args: [0; 6] }] };
    let first = flash_with_radio(&c, &m, &p, &radio);
    let l = &first.slot.layout;
    let r = l.radio.as_ref().unwrap();
    let symbols = first.slot.symbols.radio.as_ref().unwrap();
    // Reserve data at the top of this image's free SRAM. Only the task and
    // ISR occupy the span below it: all kernel calls use the image. `used` excludes
    // everything boot clears, including the radio tables and task/IRQ stacks;
    // `end` also accounts for the module RAM budget and dispatcher stack.
    let free = l.regions.iter().find(|r| r.name == "internal SRAM").unwrap();
    let code_data = u32::try_from((free.used + 15) & !15).unwrap();
    let data_base = u32::try_from(free.end.checked_sub(u64::from(DATA_BYTES)).unwrap() & !15).unwrap();
    let code_room = data_base.checked_sub(code_data).expect("fixture data must leave room for code");
    assert!(code_data >= SRAM_DATA && data_base + DATA_BYTES <= l.stack_top - l.stack_bytes);
    let code_base = SRAM_CODE + (code_data - SRAM_DATA);
    let log = data_base + 0x1000;
    let thunks = ["osi_wifi_int_disable", "osi_wifi_int_restore"].map(|name|
        symbols.thunks.iter().find(|t| t.0 == name).unwrap().1);
    let task_base = code_base;
    let find_instruction = |entry: u32, op: Op| {
        let mut pc = entry;
        loop {
            let (insn, len) = runtime_instruction(&first.slot, pc);
            if insn.op == op { break (pc, len); }
            pc += len;
        }
    };
    let (mask, mask_len) = find_instruction(thunks[0], Op::Rsil);
    let masked = mask + mask_len;
    let (restore_wsr, _) = find_instruction(thunks[1], Op::Wsr);
    let (mut outer_return, mut yield_raised, mut done) = (0, 0, 0);
    let mut fixture = RuntimeRadioFixture { data_base, bytes: vec![0; DATA_BYTES as usize], code: None,
        entry: 0, isr: 0, isr_return: 0, record: 0, fault: None };
    let task = assemble(&mut Asm::new(2, A9), |a| {
        a.literal_table();
        fixture.entry = task_base + a.base.offset();
        a.entry(A1, 64);
        a.op24(enc::rrr(0, 0, 0, 6, 15, A7));
        for (address, value) in [(r.isrs, 1),
            (r.isrs + 4, task_base + a.base.label_offset(ISR).unwrap_or(0)),
            (r.isrs + 8, 0), (r.isr_count, 1),
            (0x6000_9004 + RADIO_GPIO as u32 * 4, (1 << 12) | (1 << 9)),
            (GPIO + 0x4c, 1 << RADIO_GPIO),
            (GPIO + 0x74 + RADIO_GPIO as u32 * 4, (1 << 7) | (1 << 13)),
            (0x600c_2000 + 16 * 4, 0)] {
            a.mov_reg_i32(A8, address); a.mov_reg_i32(A9, value); a.s32i(A9, A8, 0);
        }
        a.op24(enc::rsr(0, 3, 0, 228, A8)); a.movi(A9, 1 | (1 << YIELD_LINE));
        a.or(A8, A8, A9); a.wsr(A8, 228); a.rsync();
        a.wsr(A7, 230); a.rsync();
        a.movi(A10, 0); a.mov_reg_i32(A8, thunks[0]); a.callx8(A8);
        outer_return = task_base + a.base.offset();
        a.mov_n(A4, A10); // outer token survives the next CALL8
        a.movi(A10, 0); a.mov_reg_i32(A8, thunks[0]); a.callx8(A8);
        a.mov_n(A5, A10);
        a.movi(A8, 1 << YIELD_LINE); a.wsr(A8, 226); a.rsync();
        yield_raised = task_base + a.base.offset();
        a.movi(A10, 0); a.mov_n(A11, A5); a.mov_reg_i32(A8, thunks[1]); a.callx8(A8);
        a.movi(A10, 0); a.mov_n(A11, A4); a.mov_reg_i32(A8, thunks[1]); a.callx8(A8);
        a.mov_reg_i32(A8, log); a.movi(A9, 1); a.s32i(A9, A8, 0);
        done = task_base + a.base.offset();
        a.base.label_assign(PARK); a.j_label(PARK);

        a.base.label_assign(ISR); fixture.isr = task_base + a.base.offset();
        a.entry(A1, 64);
        a.mov_reg_i32(A3, GPIO + 0x4c); a.movi(A4, 1 << RADIO_GPIO); a.s32i(A4, A3, 0);
        a.mov_reg_i32(A3, log + 4);
        a.l32i(A4, A3, 0); a.addi(A4, A4, 1); a.s32i(A4, A3, 0);
        fixture.isr_return = task_base + a.base.offset(); a.retw_n();
    });
    fixture.record = outer_return;
    // Include fetch padding after the last narrow instruction.
    let code_bytes = task.len() + 4;
    assert!(code_bytes as u64 <= u64::from(code_room),
        "fixture code needs {code_bytes} bytes but its checked SRAM span has {code_room}");
    let mut code = vec![0; code_bytes];
    code[..task.len()].copy_from_slice(&task);
    fixture.code = Some((code_data, code));
    radio.boot_tasks[0].entry = fixture.entry;
    let image = flash_with_radio(&c, &m, &p, &radio);
    assert_eq!(image.slot.symbols.radio, first.slot.symbols.radio);
    assert_eq!(format!("{:?}", image.slot.layout), format!("{:?}", first.slot.layout));
    let mut h = Harness::for_radio_proof(&image, &p, Some(&fixture));
    // The proof spans SRAM code and the real flash interrupt vector.
    h.marks = PcSet::new(code_base, SLOT_BASE + image.slot.bytes.len() as u32 - code_base);
    h.marks.insert(runtime::USER_VECTOR);
    let deadline = h.machine.now() + h.machine.tick_hz();
    assert_eq!(radio_advance(&mut h, deadline, &[masked]), Ran::Pc);
    assert_eq!(h.machine.cpu().ps & 15, 15);
    assert_eq!(words(&h.machine, symbols.crit_depth, 1), [0]);
    // The external line becomes pending after RSIL has captured level 0.
    // It remains asserted across both sections, until the real ISR clears it.
    h.machine.set_input(RADIO_GPIO, true);
    assert_ne!(h.machine.cpu().interrupt & h.machine.cpu().intenable & 1, 0);
    let (mut in_section, mut yielded, mut outer_unmasked, mut vector_seen, mut finished) = (false, false, false, false, false);
    let mut boundaries = 0;
    for _ in 0..100_000 {
        let pc = h.machine.pc();
        if pc == outer_return { in_section = true; }
        if pc == yield_raised { yielded = true; }
        if in_section && !outer_unmasked {
            assert!(h.machine.cpu().ps & 15 >= 3, "section opened at {pc:#010x}");
            assert_ne!(h.machine.cpu().interrupt & 1, 0, "GPIO remains pending at every section boundary");
            if yielded { assert_ne!(h.machine.cpu().interrupt & (1 << YIELD_LINE), 0, "yield stays pending inside the section"); }
            boundaries += 1;
        }
        if pc == runtime::USER_VECTOR {
            assert!(outer_unmasked, "interrupt entered before the outer restore's WSR");
            assert_eq!(h.machine.cpu().exccause, 4);
            if !vector_seen {
                assert_eq!(h.machine.cpu().interrupt & (1 | (1 << YIELD_LINE)), 1 | (1 << YIELD_LINE));
                vector_seen = true;
            }
        }
        if pc == done { finished = true; break; }
        let restores_outer = pc == restore_wsr && words(&h.machine, symbols.crit_depth, 1) == [0];
        let retired = h.machine.cpu().instret;
        let now = h.machine.now();
        // One native instruction at a time. Exception entry costs no model
        // ticks, so stop explicitly at USER_VECTOR before its first instruction.
        let stop_after = if pc == runtime::USER_VECTOR { now } else { 0 };
        loop {
            let ran = h.machine.run(&Until { tick: now + 1, marks: &h.marks,
                stops: &[runtime::USER_VECTOR], stops_after: stop_after, on_drive: false });
            h.observed.take(&mut h.machine);
            match ran {
                Ran::Barrier => continue,
                Ran::Pc | Ran::Tick => break,
                _ => panic!("critical single-step: {ran:?}, {:?}", h.machine.status()),
            }
        }
        assert!(h.machine.cpu().instret - retired <= 1, "observe every retired instruction boundary");
        if restores_outer {
            assert!(in_section && yielded && !outer_unmasked);
            assert_eq!(h.machine.cpu().ps & 15, 0);
            outer_unmasked = true;
        }
    }
    assert!(finished && in_section && yielded && outer_unmasked && vector_seen);
    assert!(boundaries > 4, "the assertion covers the nested calls as well as their returns");
    assert_eq!(words(&h.machine, log, 2), [1, 1], "task completes and the pending GPIO invokes its ISR once");
    assert_eq!(h.machine.cpu().interrupt & (1 | (1 << YIELD_LINE)), 0, "both GPIO and deferred yield were serviced");
    assert_eq!(words(&h.machine, symbols.crit_depth, 1), [0]);
    assert_eq!(h.machine.cpu().ps & 15, 0);
    assert!(h.machine.last_reset().is_none());
}

#[test]
fn radio_reports_drain_faults_and_account_for_loss() {
    use fab_elec_mcu_emit_s3::{BootTask, Radio, radio::{TaskAbi, REPORT_RING_ENTRIES}};
    let c = compiled("let out = output(D33)\nlet tick = every(ms(20), fn() { write(out, 1) })");
    let m = module::load(&c.module, &chip::profile()).unwrap();
    let p = profile(6370);
    let entries = [(96u8, 0x1234u16, -7i32), (97, 0xabcd, i32::MIN), (127, u16::MAX, i32::MAX)];
    let cases = [(0u32, 0u32, 0u32), (0, u32::MAX, u32::MAX),
        (0x7fff_fffe, 1, 0x7fff_ffff), (0x7fff_ffff, 1, 0x8000_0000),
        (u32::MAX - 1, 2, u32::MAX), (u32::MAX, 1, u32::MAX), (5, 2, 7)];
    let head = u32::MAX - 1; let tail = head.wrapping_add(entries.len() as u32);
    let mut ring = vec![0; 3 + REPORT_RING_ENTRIES as usize * 3];
    ring[..3].copy_from_slice(&[head, tail, 2]);
    for (n, &(code, site, arg)) in entries.iter().enumerate() {
        let slot = 3 + (head.wrapping_add(n as u32) & (REPORT_RING_ENTRIES - 1)) as usize * 3;
        ring[slot..slot + 3].copy_from_slice(&[u32::from(code), u32::from(site), arg as u32]);
    }
    let mut radio = Radio { start: None, restart: fab_elec_mcu_emit_s3::image::RestartPolicy::PROVISIONAL,
        island: island_fixture::small_island().binding(), dispatcher_priority: 2, irq_stack_bytes: 2048, task_capacity: 3,
        isr_capacity: 1, heap_bytes: 4096, timer_reserve: 8, burst_insns: 2048, trace_entries: 0,
        boot_tasks: vec![BootTask { entry: RADIO_FIXTURE_CODE, abi: TaskAbi::Windowed,
            priority: 5, stack_bytes: 2048, args: [0; 6] }] };
    let first = flash_with_radio(&c, &m, &p, &radio);
    let fixture = runtime_reports_fixture(&first.slot.layout, &ring, &cases);
    radio.boot_tasks[0].entry = fixture.entry;
    let image = flash_with_radio(&c, &m, &p, &radio);
    assert_eq!(format!("{:?}", image.slot.layout), format!("{:?}", first.slot.layout),
        "linking the report fixture leaves its SRAM addresses unchanged");
    let slot = &image.slot; let l = &slot.layout; let r = l.radio.as_ref().unwrap();
    let sender = slot.map.runtime.iter().find(|r| r.name == "radio_reports").unwrap().start;
    let idle = slot.symbols.radio.as_ref().unwrap().idle;
    let mut h = Harness::for_radio_proof(&image, &p, Some(&fixture));
    assert_eq!(words(&h.machine, r.reports, 3 + REPORT_RING_ENTRIES * 3),
        vec![0; 3 + REPORT_RING_ENTRIES as usize * 3], "init clears the entire report ring before the fixture runs");
    let deadline = h.machine.now() + h.machine.tick_hz();
    // Boot drains the empty ring, then appends the first start marker (generation 1, cause 0).
    let started = slot.symbols.radio.as_ref().unwrap().started;
    assert_eq!(radio_advance(&mut h, deadline, &[started]), Ran::Pc, "boot appends the start marker");
    assert_eq!(words(&h.machine, r.reports + 12, 3), [u32::from(watch::FAULT_RADIO_STARTED), u32::from(watch::SITE_RADIO_SCHEDULER), 1],
        "the boot start marker is the ring's first entry");
    assert_eq!(radio_advance(&mut h, deadline, &[sender]), Ran::Pc, "next sleep reaches the drain");
    assert_eq!(words(&h.machine, RADIO_FIXTURE_LOG, 2), [1, 0], "boot task published the first phase without an external wake");
    assert_eq!(words(&h.machine, r.reports, 3 + REPORT_RING_ENTRIES * 3), ring, "task stores produced the input ring");
    assert_eq!(h.machine.cpu().ps & 15, 15, "sleep masks producers before draining");
    let return_pc = h.machine.cpu().ar[0]; let saved = h.machine.cpu().ar[12..16].to_vec();
    let tick = words(&h.machine, l.stamp, 1)[0];
    assert_eq!(words(&h.machine, l.time_sent, 1), [1], "fixture requests the nested Time frame");
    assert_eq!(radio_advance(&mut h, deadline, &[return_pc]), Ran::Pc, "CALL0 drain returns to sleep");
    assert_eq!(h.machine.cpu().ps & 15, 15, "EVENT and WIRE retain the drain's mask");
    assert_eq!(&h.machine.cpu().ar[12..16], saved.as_slice(), "drain preserves dispatcher registers");
    ring[..3].copy_from_slice(&[tail, tail, 0]);
    assert_eq!(words(&h.machine, r.reports, 3 + REPORT_RING_ENTRIES * 3), ring,
        "drain consumes wrapped entries and clears lost without changing payloads");
    assert_eq!(words(&h.machine, l.lost_count, 1), [2], "ring overflow joins runtime LOST after the entries");
    assert_eq!(words(&h.machine, l.lost_saturated, 1), [0]);
    assert_eq!(words(&h.machine, l.lost_first, 1), [tick]);
    assert_eq!(words(&h.machine, l.lost_last, 1), [tick]);
    assert_eq!(radio_advance(&mut h, deadline, &[idle]), Ran::Pc, "dispatcher completes its sleep path");
    // The next ordinary Write flushes the folded LOST; USB delivers all frames.
    let delivered = h.epoch + h.machine.tick_hz() / 20;
    assert_eq!(radio_advance(&mut h, delivered, &[]), Ran::Tick);
    let faults: Vec<_> = h.observed.frames.iter().filter_map(|f|
        matches!(f.event, watch::Event::Fault { .. }).then_some(f.event)).collect();
    assert_eq!(faults, entries.iter().map(|&(code, site, arg)| watch::Event::Fault { code, site, arg }).collect::<Vec<_>>(),
        "wire carries the three kernel reports in ring order with every payload field");
    let losses: Vec<_> = h.observed.frames.iter().filter(|f| matches!(f.event, watch::Event::Lost { .. })).collect();
    assert_eq!(losses.len(), 1, "ring overflow produces one LOST frame");
    assert_eq!(losses[0].event, watch::Event::Lost { count: 2, saturated: false, first_tick: tick });
    assert_eq!(losses[0].tick, tick, "LOST retains the drain's stamp");
    for (seq, frame) in h.observed.frames.iter().enumerate() { assert_eq!(frame.seq, seq as u8); }
    assert_eq!(words(&h.machine, r.reports, 3), [tail, tail, 0]);
    assert_eq!(words(&h.machine, l.lost_count, 1), [0], "accepted LOST clears runtime accounting");

    // Exercise the bulk addition's 31-bit boundary and u32 saturation without
    // generating billions of reports. Wake the fixture only from idle, when
    // dispatcher scratch has no active frame. It stops the ordinary timer and
    // writes each case before readying the dispatcher for its next sleep.
    for (index, &(old, count, expected)) in cases.iter().enumerate() {
        assert_eq!(radio_advance(&mut h, deadline, &[idle]), Ran::Pc);
        assert_eq!(words(&h.machine, r.current, 1), [r.idle]);
        h.machine.set_input(RADIO_GPIO, true);
        assert_eq!(radio_advance(&mut h, deadline, &[sender]), Ran::Pc);
        h.machine.set_input(RADIO_GPIO, false);
        assert_eq!(words(&h.machine, RADIO_FIXTURE_LOG, 2), [index as u32 + 2, index as u32 + 1],
            "one GPIO wake publishes exactly the next case");
        assert_eq!(words(&h.machine, r.reports, 3), [tail, tail, count]);
        assert_eq!(words(&h.machine, l.lost_count, 1), [old], "fixture's loss count survives until drain entry");
        assert_eq!(h.machine.cpu().ps & 15, 15);
        let return_pc = h.machine.cpu().ar[0]; let begin = h.machine.cpu().instret;
        let saved = h.machine.cpu().ar[12..16].to_vec();
        let tick = words(&h.machine, l.stamp, 1)[0];
        assert_eq!(radio_advance(&mut h, deadline, &[return_pc]), Ran::Pc);
        assert_eq!(h.machine.cpu().ps & 15, 15);
        assert_eq!(&h.machine.cpu().ar[12..16], saved.as_slice());
        assert!(h.machine.cpu().instret - begin <= 64, "empty check and bulk LOSS fit the documented suffix allowance");
        assert_eq!(words(&h.machine, l.lost_count, 1), [expected], "saturating {old}+{count}");
        assert_eq!(words(&h.machine, l.lost_saturated, 1), [u32::from(expected > 0x7fff_ffff)]);
        assert_eq!(words(&h.machine, l.lost_first, 1), [if old == 0 && count != 0 { tick } else { 0x1234_5678 }]);
        assert_eq!(words(&h.machine, l.lost_last, 1), [if count != 0 { tick } else { 0xabcd_0123 }]);
        assert_eq!(words(&h.machine, r.reports, 3), [tail, tail, 0]);
    }

    // The final fixture phase fills both the hardware packet and TX ring.
    // Keep the host closed so these synthetic queued bytes never reach it.
    assert_eq!(radio_advance(&mut h, deadline, &[idle]), Ran::Pc);
    h.machine.set_usb_host_listening(false);
    h.machine.set_input(RADIO_GPIO, true);
    assert_eq!(radio_advance(&mut h, deadline, &[sender]), Ran::Pc);
    h.machine.set_input(RADIO_GPIO, false);
    assert_eq!(words(&h.machine, RADIO_FIXTURE_LOG, 2), [cases.len() as u32 + 2, cases.len() as u32 + 1]);
    let return_pc = h.machine.cpu().ar[0];
    let tx_head = words(&h.machine, l.tx_head, 1)[0];
    let tx_tail = words(&h.machine, l.tx_tail, 1)[0];
    assert_eq!(tx_tail.wrapping_sub(tx_head).wrapping_sub(1) & (runtime::TX_RING_BYTES - 1), 0,
        "sleep's initial DRAIN cannot free TX space while the hardware packet is full");
    assert_eq!(words(&h.machine, r.reports, 3), [0, 1, 2]);
    assert_eq!(radio_advance(&mut h, deadline, &[return_pc]), Ran::Pc);
    assert_eq!(words(&h.machine, r.reports, 3), [1, 1, 0], "refused Fault still advances head");
    assert_eq!(words(&h.machine, l.lost_count, 1), [3], "one refused Fault plus two ring losses, counted once each");
    assert!(h.machine.last_reset().is_none(), "report draining keeps feeding the watchdog");
}

#[test]
fn radio_record_retries_a_full_ring_without_loss() {
    use fab_elec_mcu_emit_s3::{BootTask, Radio, radio::{TaskAbi, FAULT_WORDS}};
    // Hello plus 684 twelve-byte Writes fill the closed hardware packet and
    // leave 16 free bytes in the software ring. The 16-byte boot start marker
    // uses the rest, so the first RadioRecord must wait.
    let c = compiled("let out = output(D33)\nfn setup() { for i in 0..684 { write(out, i & 1) } }");
    let m = module::load(&c.module, &chip::profile()).unwrap();
    let p = profile(6369);
    let bytes = assemble(&mut Asm::new(0, 9), |a| { a.op24(enc::entry(1, 64)); a.op24(0); });
    let fixture = RuntimeRadioFixture { data_base: RADIO_FIXTURE_DATA, bytes, code: None,
        entry: RADIO_FIXTURE_CODE, isr: 0, isr_return: 0,
        record: 0, fault: Some(RADIO_FIXTURE_CODE + 3) };
    let radio = Radio { start: None, restart: fab_elec_mcu_emit_s3::image::RestartPolicy { hold_after_fault: true, ..fab_elec_mcu_emit_s3::image::RestartPolicy::PROVISIONAL },
        island: island_fixture::small_island().binding(), dispatcher_priority: 2, irq_stack_bytes: 2048, task_capacity: 3,
        isr_capacity: fab_elec_mcu_emit_s3::radio::PACK_ISR_LINES, heap_bytes: 4096, timer_reserve: 8,
        burst_insns: 2048, trace_entries: 0,
        boot_tasks: vec![BootTask { entry: fixture.entry, abi: TaskAbi::Windowed,
            priority: 5, stack_bytes: 2048, args: [0; 6] }] };
    let image = flash_with_radio(&c, &m, &p, &radio);
    let slot = &image.slot;
    let r = slot.layout.radio.as_ref().unwrap();
    let idle = slot.symbols.radio.as_ref().unwrap().idle;
    let mut h = Harness::for_radio_proof(&image, &p, Some(&fixture));
    h.machine.set_usb_host_listening(false);
    let deadline = h.machine.now() + h.machine.tick_hz();
    assert_eq!(radio_advance(&mut h, deadline, &[idle]), Ran::Pc, "a full TX ring does not prevent radio idle");
    assert_eq!(words(&h.machine, r.fault, 1), [FAULT_WORDS], "retry fixture has published a real fault");
    let head = words(&h.machine, slot.layout.tx_head, 1)[0];
    let tail = words(&h.machine, slot.layout.tx_tail, 1)[0];
    let free = tail.wrapping_sub(head).wrapping_sub(1) & (runtime::TX_RING_BYTES - 1);
    // 685 Writes used to leave 8; one Write fewer adds a 12-byte pin frame and the
    // start marker takes 16. Hello grew by four bytes, giving 8 + 12 - 16 - 4 = 0.
    assert_eq!(free, 0, "the fixture leaves too little room for a 27-byte record frame");
    assert_eq!(words(&h.machine, r.report_next, 1), [0], "the refused first frame does not advance the cursor");
    assert_eq!(words(&h.machine, slot.layout.lost_count, 1), [0], "refusal is not counted as LOST");
    let seq = words(&h.machine, slot.layout.seq, 1);
    assert_eq!(radio_advance(&mut h, deadline, &[idle]), Ran::Pc, "the next capped idle retries while USB is closed");
    assert_eq!(words(&h.machine, r.report_next, 1), [0], "repeated refusal still retains frame zero");
    assert_eq!(words(&h.machine, slot.layout.tx_head, 1), [head], "refused frames commit no ring bytes");
    assert_eq!(words(&h.machine, slot.layout.seq, 1), seq, "refused frames consume no sequence numbers");

    h.machine.set_usb_host_listening(true);
    assert_eq!(radio_advance(&mut h, deadline, &[idle]), Ran::Pc, "host reopening makes TX space and wakes the dispatcher");
    assert_eq!(words(&h.machine, r.report_next, 1), [2], "the first successful idle sends exactly two retried frames");
    while !h.observed.frames.iter().any(|f| matches!(f.event, watch::Event::RadioRecord { .. })) {
        assert_eq!(radio_advance(&mut h, deadline, &[idle]), Ran::Pc, "the retried record reaches the host within the bound");
    }
    let mut expected = 0;
    for (seq, frame) in h.observed.frames.iter().enumerate() {
        assert_eq!(frame.seq, seq as u8, "retry preserves the complete watch sequence");
        assert!(!matches!(frame.event, watch::Event::Lost { .. }), "retry produces no Lost frame");
        if let watch::Event::RadioRecord { index, words: payload } = frame.event {
            assert_eq!(index, expected, "retry never skips or duplicates an index");
            assert_eq!(payload.as_slice(), words(&h.machine, r.fault + u32::from(index) * 16, 4),
                "retried payload matches the original SRAM words");
            expected += 1;
        }
    }
    assert!(expected > 0, "frame zero was delivered after its refusals");
    assert_eq!(words(&h.machine, slot.layout.lost_count, 1), [0], "reopening leaves the loss counter untouched");
    assert!(h.machine.last_reset().is_none(), "retry keeps feeding the watchdog");
}

fn radio_runtime_proof(fault: bool) {
    use fab_elec_mcu_emit_s3::{BootTask, Radio, radio::{TaskAbi, TCB_STATE, BLOCKED}};
    let c = compiled(r#"
let out = output(D33)
var total: i32 = 0
var turns: i32 = 0
var d: i32 = 3
let tick = every(ms(20), fn() {
    // A runtime divisor keeps each iteration about one instruction per AIR op, so
    // the handler stays above 1 ms of instructions at CPU_HZ as loop code gets faster.
    for i in 0..30000 { total = total + i / d }
    turns = turns + 1
    write(out, turns & 1)
})
"#);
    let m = module::load(&c.module, &chip::profile()).unwrap();
    let p = profile(6368);
    let fixture = runtime_radio_fixture(fault);
    let mut radio = Radio { start: None, restart: fab_elec_mcu_emit_s3::image::RestartPolicy { hold_after_fault: true, ..fab_elec_mcu_emit_s3::image::RestartPolicy::PROVISIONAL },
        island: island_fixture::small_island().binding(), dispatcher_priority: 2, irq_stack_bytes: 2048,
        task_capacity: 3, isr_capacity: 1, heap_bytes: 4096, timer_reserve: 8,
        burst_insns: 2048, trace_entries: if fault { 8 } else { 0 },
        boot_tasks: vec![BootTask { entry: fixture.entry, abi: TaskAbi::Windowed,
            priority: 5, stack_bytes: 2048, args: [0; 6] }] };
    if !fault {
        let mut invalid = radio.clone();
        invalid.boot_tasks[0].priority = 0;
        let result = build_slot(&m, &SlotOptions { radio: Some(invalid), ..SlotOptions::default() });
        assert!(matches!(result, Err(ref error) if error.message == "idle alone has priority zero"),
            "Radio::check refuses an island boot task that could tie idle");
    }
    if fault {
        for entries in [1, 3, 1023, 1025, 2048, u32::MAX] {
            let mut invalid = radio.clone(); invalid.trace_entries = entries;
            let result = build_slot(&m, &SlotOptions { radio: Some(invalid), ..SlotOptions::default() });
            assert!(matches!(result, Err(ref error) if error.message.contains("trace_entries")),
                "trace capacity {entries} is refused before allocation");
        }
    }
    let first = flash_with_radio(&c, &m, &p, &radio);
    let r = first.slot.layout.radio.as_ref().unwrap();
    for (address, bytes) in [(r.radio_state, 4), (r.fault, fab_elec_mcu_emit_s3::radio::FAULT_FRAMES * 16),
        (r.trace_head, 4), (r.trace, radio.trace_entries * 32), (r.report_next, 4), (r.report_return, 4),
        (r.reports, 12 + fab_elec_mcu_emit_s3::radio::REPORT_RING_ENTRIES * 12), (r.reports_return, 4)]
    {
        assert_eq!(address & 3, 0, "fault and trace allocations are word aligned");
        assert!(address >= first.slot.layout.t0 && address + bytes <= first.slot.layout.state,
            "fault and trace allocations are inside the cleared runtime span");
    }
    radio.boot_tasks[0].args = [r.isrs, r.isr_count, r.boot_tasks[0], r.dispatcher, RADIO_FIXTURE_LOG, fixture.isr];
    let image = flash_with_radio(&c, &m, &p, &radio);
    assert_eq!(format!("{:?}", image.slot.layout), format!("{:?}", first.slot.layout),
        "linking boot arguments leaves the entire SRAM layout identical");
    let s = image.slot.symbols.radio.as_ref().unwrap();
    assert_eq!(fixture.entry >> 30, s.task_exit >> 30);
    for (pc, name) in [(SLOT_BASE, "window_vectors"), (runtime::USER_VECTOR, "user_vector"),
        (s.irq_entry, "radio_kernel"), (s.restore, "radio_kernel"), (s.stack_fault, "radio_kernel"),
        (s.pick, "radio_sched"), (s.block, "radio_sched"), (s.idle, "radio_sched"), (s.no_task, "radio_sched"),
        (s.shim, "radio_shim"), (s.start, "radio_boot"), (s.task_exit, "radio_kernel"),
        (s.fault_entry, "radio_kernel"), (s.fatal, "radio_kernel"), (s.trace_record, "radio_kernel")]
    { assert_eq!(image.slot.map.find(pc), SlotWhere::Runtime(name)); }
    assert_eq!(s.block_calls, [image.slot.symbols.waiti, image.slot.symbols.idle_waiti]);
    for pc in s.block_calls {
        assert_eq!(image.slot.map.find(pc), SlotWhere::Runtime("dispatcher"));
        assert_eq!(runtime_instruction(&image.slot, pc).0.op, Op::Callx0);
    }
    let plain = flash(&c, &m, &p);
    assert_eq!(&image.slot.bytes[0x300..0x340], &plain.slot.bytes[0x300..0x340], "init's kernel vector stays unchanged");
    let (off_h, off) = run_radio_proof(&plain, &p, None);
    assert!(off_h.observed.frames.iter().all(|f| !matches!(f.event, watch::Event::RadioRecord { .. })),
        "radio-off images never send RadioRecord");
    let (mut h, on) = run_radio_proof(&image, &p, Some(&fixture));
    assert_eq!(on.events.first(), Some(&watch::Event::Fault { code: watch::FAULT_RADIO_STARTED,
        site: watch::SITE_RADIO_SCHEDULER, arg: 1 }), "the radio's boot start marker leads the stream");
    assert_eq!(on.events[1..], off.events, "Write/Fault stream is unchanged apart from timestamps and the start marker");
    assert_eq!(off.events.len(), 4);
    assert_eq!(on.state, off.state, "preempted handler resumes and finishes with identical state");
    assert_eq!(on.handler_insns, off.handler_insns, "all four handlers execute the same instructions");
    let log = words(&h.machine, RADIO_FIXTURE_LOG, 3);
    assert_eq!((log[0], log[2], on.fixture_records), (1, 1, 1), "exactly one task run, after exactly one ISR");
    assert_ne!(words(&h.machine, s.isr_max_span, 1)[0], 0, "the scheduler measured the island ISR");
    assert_eq!(words(&h.machine, s.isr_max_entry, 1), [fixture.isr], "the longest span belongs to the installed ISR");
    assert_eq!(Some(log[1]), on.injected_pc, "saved CONTEXT_PC is the precise injection boundary");
    let (start, end) = image.slot.map.handlers.entries[0];
    assert!((start..end).contains(&log[1]));
    let r = image.slot.layout.radio.as_ref().unwrap();
    assert_eq!(words(&h.machine, r.boot_tasks[0] + TCB_STATE, 1),
        [if fault { fab_elec_mcu_emit_s3::radio::DEAD } else { BLOCKED }]);
    assert_eq!(on.faults, usize::from(fault), "exactly the planned exception");
    if let Some(pc) = fixture.fault {
        use fab_elec_mcu_emit_s3::radio::{RADIO_DEAD, FAULT_WORDS, FAULT_CAUSE, FAULT_PC, FAULT_TASK};
        assert_eq!(words(&h.machine, r.radio_state, 1), [RADIO_DEAD], "radio remains DEAD after all four handlers");
        assert_eq!(words(&h.machine, r.fault, 1), [FAULT_WORDS], "fault record complete");
        for (offset, value) in [(FAULT_CAUSE, 0), (FAULT_PC, pc), (FAULT_TASK, r.boot_tasks[0])] {
            assert_eq!(words(&h.machine, r.fault + offset * 4, 1), [value], "runtime fault record word {offset}");
        }
        // Continue in batches without observing each handler instruction;
        // the first four firings above retain the full preemption proof.
        finish_radio_report(&mut h, &image.slot);
        let record = collect_runtime_radio_record(&h, &image.slot);
        let decoded = fab_elec_mcu_emit_s3::radio::RadioFault::decode(&record).expect("transported fault decodes");
        assert_eq!(decoded.island_guard, fab_elec_mcu_emit_s3::radio::GUARD, "host received the intact boot guard");
        assert_eq!(decoded.island_status, fab_elec_mcu_emit_s3::radio::ISLAND_READY, "host received the island status at the fault");
        assert_eq!((decoded.cause, decoded.pc, decoded.task), (0, pc, r.boot_tasks[0]),
            "host decoding preserves the checked cause, PC and task");
        let text_bytes = fixture.bytes.len();
        let task_bytes = fixture.isr - fixture.entry;
        let map = format!("0x{:08x} {text_bytes} TEXT .text.fixture fixture\n0x{:08x} {task_bytes} TEXT/FUNC fixture_task fixture\n",
            RADIO_FIXTURE_CODE, fixture.entry);
        assert_eq!(fab_elec_mcu_island::symbolize(&map, decoded.backtrace[0].0), Some(("fixture_task", pc - fixture.entry)),
            "a real captured backtrace PC resolves to its fixture function");
        let printed = decoded.with_map(&map).to_string();
        assert!(printed.contains(&format!("fixture_task+0x{:x}", pc - fixture.entry)), "Display symbolizes the captured PC");
        assert!(printed.contains("a return address points after its call"), "Display explains unadjusted return PCs");

        // At this point the report and its USB wakes are finished. The next
        // handler must still be reached by exactly one TARGET0 wake from idle.
        let trace_start = h.observed.trace.len();
        let deadline = h.machine.now() + h.machine.tick_hz() / 10;
        assert_eq!(radio_advance(&mut h, deadline, &[runtime::USER_VECTOR]), Ran::Pc, "reported fault still wakes from idle");
        assert_eq!(h.machine.cpu().exccause, 4, "post-report idle wakes by interrupt");
        assert_eq!(h.machine.cpu().interrupt & h.machine.cpu().intenable, 1 << 1, "only TARGET0 wakes post-report idle");
        assert_eq!(radio_advance(&mut h, deadline, &[image.slot.handlers[0].address]), Ran::Pc, "post-report TARGET0 reaches the handler");
        assert_eq!(h.observed.trace[trace_start..].iter().filter(|(_, pc)| *pc == runtime::USER_VECTOR).count(), 1,
            "one interrupt from post-report idle through handler entry, with no scheduler tick");
        collect_runtime_radio_record(&h, &image.slot);
    } else {
        assert!(h.observed.frames.iter().all(|f| !matches!(f.event, watch::Event::RadioRecord { .. })),
            "a healthy radio never sends RadioRecord");
    }

    // Begin after the dispatcher-to-idle software yield has completed.
    // Between the second and third timer runs, the only interrupt from
    // idle's WAITI through handler entry is the TARGET0 wake: no tick.
    if !fault {
        let idle = on.idle.iter().find(|(completed, _)| *completed == 2).unwrap().1;
        assert!((on.returns[1]..on.enters[2]).contains(&idle), "healthy idle lies between the second and third firings");
        let wakes: Vec<_> = on.interrupts.iter().filter(|(tick, _)| (idle..on.enters[2]).contains(tick)).collect();
        assert_eq!(wakes.len(), 1, "one interrupt during the healthy idle interval");
        assert_eq!(wakes[0].1, 1 << 1, "only TARGET0 wakes healthy idle");
    }
    let cycle = h.machine.tick_hz() / u64::from(CPU_HZ);
    let bound = runtime::radio_switch_insns(&radio);
    println!("TARGET0 wake latency to first handler instruction: radio-off {} ticks, radio {} ticks ({} Hz grid); switch bound {bound} instructions",
        off.latency, on.latency, h.machine.tick_hz());
    assert!(on.latency <= bound * cycle, "radio latency exceeds derived switch allowance");
    let extra = on.latency.saturating_sub(off.latency);
    println!("TARGET0 radio extra wake latency: {extra} ticks; idle wake allowance {} instructions ({cycle} ticks/instruction)",
        runtime::RADIO_IDLE_WAKE_INSNS);
    assert!(extra <= runtime::RADIO_IDLE_WAKE_INSNS * cycle,
        "radio's extra idle wake latency exceeds its vector-through-RFE allowance");
    println!("radio longest enabled level-1 pending stretch: {} retired instructions", on.longest_pending);
    assert!(on.longest_pending > 0);
}
