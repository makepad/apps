//! F3/F4 through real boot, emitted ports, the OUT byte pipe and the watch reader.
//! Deadlines below are simulated USB service events, never wall-clock caps.
use std::sync::Arc;

use fab_elec_mcu::Compiled;
use fab_elec_mcu_emit32::xtensa::{ccz, Asm};
use fab_elec_mcu_emit_s3::{build_slot, chip, runtime, Image, SlotOptions, SLOT_BASE};
use fab_elec_mcu_emu::{Machine, PcSet, Ran, Status, Until};
use fab_elec_mcu_rt::{module, sched::{Device, Fault, Hal, Role}, serial, watch};
use fab_elec_mcu_s3::S3Machine;

#[path = "support/image_fixture.rs"]
mod image_fixture;

#[path = "support/link_image.rs"]
mod link_image;
use link_image::{compiled, profile, MAC};

fn read(m: &S3Machine, address: u32, n: usize) -> Vec<u8> {
    let mut bytes = vec![0; n]; assert!(m.read_data(address, &mut bytes)); bytes
}

fn word(m: &S3Machine, address: u32) -> u32 {
    u32::from_le_bytes(read(m, address, 4).try_into().unwrap())
}

fn pair(m: &S3Machine, address: u32) -> u64 {
    u64::from_le_bytes(read(m, address, 8).try_into().unwrap())
}

#[derive(Debug)]
enum Trace { Arrival(u64, Vec<u8>), Start(u32) }

struct Rig {
    image: Image,
    machine: S3Machine,
    reader: watch::Reader,
    frames: Vec<watch::Frame>,
    echoes: Vec<Vec<u8>>,
    trace: Vec<Trace>,
    received: u32,
    epoch: u32,
    timer_origin: u64,
    stops: Vec<u32>,
}

impl Rig {
    fn new(c: &Compiled) -> Self {
        let p = profile(); let image = link_image::image(c);
        let s = &image.slot.symbols;
        let mut stops = vec![s.waiti, s.idle_waiti, s.handler_return];
        if s.timer_start != 0 { stops.push(s.timer_start); }
        if s.setup_call != 0 { stops.push(s.setup_call + 3); }
        stops.extend(image.slot.handlers.iter().map(|h| h.address));
        stops.push(image.slot.map.runtime.iter().find(|r| r.name == "eth_rx").unwrap().end - 2);
        let machine = S3Machine::new(Arc::new(image.flash.clone()), &p);
        Self { image, machine, reader: watch::Reader::default(), frames: Vec::new(), echoes: Vec::new(),
            trace: Vec::new(), received: 0, epoch: 0, timer_origin: 0, stops }
    }

    fn take(&mut self) {
        let mut out = Vec::new(); self.machine.take_out(&mut out);
        for byte in out {
            assert_eq!(byte.link, 0);
            if let Some(frame) = self.reader.push(byte.byte) {
                let frame = frame.expect("all device frames have valid CRC and framing");
                if matches!(frame.event, watch::Event::Eth { .. }) { self.echoes.push(self.reader.eth_body().to_vec()); }
                self.frames.push(frame);
            }
        }
        self.machine.mark();
        assert!(self.machine.last_reset().is_none(), "every RX/copy/codec stretch keeps the watchdog fed");
    }

    fn next(&mut self, deadline: u64, extra: &[u32]) -> Ran {
        let mut stops = self.stops.clone(); stops.extend(extra); stops.sort_unstable(); stops.dedup();
        let mut marks = PcSet::new(SLOT_BASE, self.image.slot.bytes.len() as u32);
        for &pc in &stops { marks.insert(pc); }
        let result = self.machine.run(&Until { tick: deadline, marks: &marks, stops: &stops,
            stops_after: self.machine.now(), on_drive: false });
        assert!(matches!(result, Ran::Pc | Ran::Tick | Ran::Barrier), "{result:?}: {:?}", self.machine.status());
        self.take();
        if result == Ran::Pc {
            if self.machine.pc() == self.image.slot.symbols.timer_start {
                let cpu = self.machine.cpu();
                self.timer_origin = u64::from(cpu.ar[2]) | (u64::from(cpu.ar[3]) << 32);
            }
            let l = &self.image.slot.layout; let e = l.eth.as_ref().unwrap();
            let epoch = word(&self.machine, l.link.as_ref().unwrap().epoch);
            if epoch != self.epoch { self.epoch = epoch; self.received = 0; }
            let received = word(&self.machine, e.received);
            while self.received != received {
                let at = e.slots + self.received % e.count * runtime::RX_SLOT_BYTES;
                let n = u16::from_le_bytes(read(&self.machine, at, 2).try_into().unwrap()) as usize;
                let due = u64::from(word(&self.machine, e.due)) | (u64::from(word(&self.machine, e.due + 4)) << 32);
                self.trace.push(Trace::Arrival(due, read(&self.machine, at + 2, n)));
                self.received = self.received.wrapping_add(1);
            }
            if let Some(h) = self.image.slot.handlers.iter().find(|h| h.address == self.machine.pc()) {
                self.trace.push(Trace::Start(h.id));
                if h.id as usize == e.entry { assert_eq!(word(&self.machine, e.seen), received, "seen is captured at handler START"); }
            }
            if [self.image.slot.symbols.waiti, self.image.slot.symbols.idle_waiti].contains(&self.machine.pc()) {
                assert_eq!(self.machine.cpu().intenable, 6, "eth keeps CPU line 2 enabled at every WAITI");
            }
        }
        result
    }

    fn until_pc(&mut self, pc: u32) {
        loop { if self.next(u64::MAX, &[pc]) == Ran::Pc && self.machine.pc() == pc { break; } }
        assert_eq!(self.machine.status(), Status::Running);
    }

    fn frame(&mut self) {
        let end = self.machine.now() + self.machine.tick_hz() / 1000;
        loop { if self.next(end, &[]) == Ran::Tick { break; } }
    }

    fn advance_ms(&mut self, ms: u64) {
        let end = self.machine.now() + ms * (self.machine.tick_hz() / 1000);
        loop { if self.next(end, &[]) == Ran::Tick { break; } }
    }

    fn hellos(&self) -> Vec<watch::Frame> {
        self.frames.iter().filter(|f| f.kind == watch::HELLO).copied().collect()
    }

    fn links(&self, counter: u8) -> Vec<u64> {
        self.frames.iter().filter_map(|f| match f.event {
            watch::Event::Link { counter: id, value } if id == counter => Some(value), _ => None,
        }).collect()
    }

    fn credit_counts(&self) -> Vec<u16> {
        self.frames.iter().filter_map(|f| match f.event { watch::Event::EthCredit { count } => Some(count), _ => None }).collect()
    }

    fn boot(&mut self) {
        while self.credit_counts().is_empty() { self.frame(); }
        let hello = self.frames.iter().find(|f| f.kind == watch::HELLO).expect("Hello precedes credit");
        assert_eq!(hello.tick, word(&self.machine, self.image.slot.layout.link.as_ref().unwrap().epoch));
        assert_eq!(self.credit_counts(), [2]);
    }

    fn send_wire(&mut self, bytes: &[u8]) { for &byte in bytes { self.machine.link_in(0, byte); } }

    fn send(&mut self, payload: &[u8], stale: bool) {
        let epoch = word(&self.machine, self.image.slot.layout.link.as_ref().unwrap().epoch);
        let mut bytes = [0; watch::ETH_WIRE];
        let n = watch::encode_eth(53, epoch.wrapping_add(u32::from(stale)), payload, &mut bytes).unwrap();
        self.send_wire(&bytes[..n]);
    }

    fn replay(&self, c: &Compiled) {
        // Native instruction durations are not AIR op durations. Zero AIR op
        // time compares ordering using the device's recorded due stamps only.
        let p = fab_elec_mcu_rt::Profile { op_ns: 0, ..chip::profile() };
        let mut d = Device::new(module::load(&c.module, &p).unwrap(), &p);
        let mut hal = Reference::default(); let setup = d.boot(&mut hal).map(|r| r.entry as u32);
        let mut boot = setup;
        for event in &self.trace {
            match event {
                Trace::Arrival(due, bytes) => {
                    // Device starts its timers at zero. Put arrivals on that same grid.
                    d.eth_in(((due - self.timer_origin) * 125).div_ceil(2), bytes).unwrap();
                }
                Trace::Start(id) if boot == Some(*id) => { boot = None; }
                Trace::Start(id) => assert_eq!(d.step(&mut hal).expect("a reference handler is ready").entry as u32, *id,
                    "timer/eth ordering agrees at the device's recorded due stamps"),
            }
        }
        assert_eq!(self.echoes, hal.sends, "echo payloads agree with Device");
        let actual = read(&self.machine, self.image.slot.layout.state, d.state().len() * 4);
        assert_eq!(actual, d.state().iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<_>>());
    }
}

#[derive(Default)]
struct Reference { sends: Vec<Vec<u8>>, faults: Vec<watch::Event> }

impl Hal for Reference {
    fn eeprom_read(&mut self, _: u64, _: u16, _: u32) -> u8 { unreachable!("this profile has no EEPROM") }
    fn eeprom_write(&mut self, _: u64, _: u16, _: u8, _: u32) { unreachable!("this profile has no EEPROM") }
    fn shift(&mut self, _: u64, _: u32, _: u32, _: u32, _: u8, _: u32) { unreachable!("this fixture shifts nothing") }
    fn configure(&mut self, _: u64, _: u32, _: Role) { panic!("no GPIO"); }
    fn serial(&mut self, _: u64, _: u32, _: serial::SerialSetting) { panic!("no serial"); }
    fn write(&mut self, _: u64, _: u32, _: bool, _: u32) { panic!("no GPIO"); }
    fn duty(&mut self, _: u64, _: u32, _: u8, _: u32) { panic!("no PWM"); }
    fn read(&mut self, _: u64, _: u32, _: u32) -> bool { panic!("no GPIO"); }
    fn adc(&mut self, _: u64, _: u32, _: u32) -> u16 { panic!("no ADC"); }
    fn clock(&mut self, t: u64, _: u32) -> u64 { t }
    fn eth_send(&mut self, _: u64, frame: &[u8], _: u32) -> bool { self.sends.push(frame.to_vec()); true }
    fn eth_mac(&mut self, _: u64, _: u32) -> [u8; 6] { MAC }
    fn fault(&mut self, _: u64, f: Fault) {
        self.faults.push(match f {
            Fault::EthLength { site, n } => watch::Event::Fault { code: watch::FAULT_ETH_LENGTH, site: site as u16, arg: n },
            Fault::EthRange { site, off, .. } => watch::Event::Fault { code: watch::FAULT_ETH_RANGE, site: site as u16, arg: off },
            other => panic!("unexpected fault: {other:?}"),
        });
    }
}

fn raw(kind: u8, epoch: u32, payload: &[u8], corrupt: bool) -> Vec<u8> {
    let mut body = vec![kind, 77]; body.extend(epoch.to_le_bytes()); body.extend(payload);
    body.push(watch::crc8(&body) ^ u8::from(corrupt));
    let mut wire = vec![0; body.len() + body.len() / 254 + 2];
    let n = watch::cobs_encode(&body, &mut wire).unwrap(); wire.truncate(n + 1); wire[n] = 0; wire
}

fn boot_fixture(r: &mut Rig, prepare: impl Fn(&mut Asm, &mut Vec<u32>)) -> Vec<u32> {
    let stops = image_fixture::before_dispatch(&mut r.image, prepare);
    r.machine = S3Machine::new(Arc::new(r.image.flash.clone()), &profile());
    stops
}

/// ESP32-S3 TRM, USB_SERIAL_JTAG_INT_RAW_REG: the runtime's USB_EP1 block + 0x8; bit 8 is the IN token.
const USB_INT_RAW: u32 = 0x6003_8008;

fn fixture_store(a: &mut Asm, address: u32, value: u32) {
    a.mov_reg_i32(11, address); a.mov_reg_i32_optimised(2, value); a.s32i(2, 11, 0);
}

#[test]
fn exact_due_tie_runs_timers_by_number_before_eth() {
    let c = compiled("\
var order: i32 = 0
let first = after(0, fn() { order = order * 10 + 1 })
let second = after(0, fn() { order = order * 10 + 2 })
let network = eth(2, fn() { order = order * 10 + 3 })");
    let mut r = Rig::new(&c); let e = r.image.slot.layout.eth.as_ref().unwrap().clone();
    let timers = r.image.slot.layout.timers;
    boot_fixture(&mut r, |a, _| {
        // Publish at the zero-delay timers' actual first deadline.
        a.mov_reg_i32(11, timers); a.l32i(2, 11, 0); a.l32i(3, 11, 4);
        a.mov_reg_i32(11, e.due); a.s32i(2, 11, 0); a.s32i(3, 11, 4);
        fixture_store(a, e.slots, 14); fixture_store(a, e.head, 1);
        fixture_store(a, e.received, 1);
    });
    r.until_pc(r.image.slot.symbols.idle_waiti);
    assert_eq!(word(&r.machine, r.image.slot.layout.state), 123);
    r.replay(&c);
}

#[test]
fn refused_credit_keeps_owed_then_retries_without_loss() {
    let c = compiled("let network = eth(2, fn() {})"); let mut r = Rig::new(&c);
    let e = r.image.slot.layout.eth.as_ref().unwrap().clone();
    let (head, tail, lost, seq) = (r.image.slot.layout.tx_head, r.image.slot.layout.tx_tail,
        r.image.slot.layout.lost_count, r.image.slot.layout.seq);
    let credit = r.image.slot.map.runtime.iter().find(|f| f.name == "eth_credit").unwrap().start;
    let stops = boot_fixture(&mut r, |a, stops| {
        fixture_store(a, head, runtime::TX_RING_BYTES - 1); fixture_store(a, tail, 0); fixture_store(a, e.owed, 2);
        // The first enqueue sees a full ring. Its nested drain then fills
        // the partial EP1 packet, freeing enough room for the second attempt.
        for _ in 0..2 { a.mov_reg_i32(11, credit); a.callx0(11); stops.push(SLOT_BASE + a.base.offset()); }
    });
    r.machine.set_usb_host_listening(false);
    r.until_pc(stops[0]);
    assert_eq!(word(&r.machine, e.owed), 2, "refused credit retains the complete owed count");
    assert_eq!(word(&r.machine, lost), 0); let before = word(&r.machine, seq);
    r.until_pc(stops[1]);
    assert_eq!(word(&r.machine, e.owed), 0, "the successful retry clears owed");
    assert_eq!(word(&r.machine, seq), before + 1, "only the accepted credit advances seq");
    assert_eq!(word(&r.machine, lost), 0, "neither credit attempt counts as LOST");
}

#[test]
fn echo_credits_maximum_frame_and_rejection_counters() {
    let c = compiled("\
let b = bytes(1514)
let mac = bytes(8)
var calls: i32 = 0
var queued: i32 = 0
fn setup() { eth_mac(mac, 1) }
let first = after(3000, fn() { calls = calls + 100 })
let second = after(3000, fn() { calls = calls + 1000 })
let network = eth(2, fn() {
    calls = calls + 1
    for i in 0..2 { let n = eth_recv(b, 0) if n != 0 { queued = queued + eth_send(b, 0, n) } }
})");
    let mut r = Rig::new(&c); r.boot();
    let e = r.image.slot.layout.eth.as_ref().unwrap().clone();
    let link = r.image.slot.layout.link.as_ref().unwrap().clone();
    let mac = r.image.slot.layout.buffers.iter().find(|b| b.name == "mac").unwrap();
    assert_eq!(read(&r.machine, mac.address, 8), [vec![0], MAC.to_vec(), vec![0]].concat(), "eFuse byte order and bounds");
    let epoch = word(&r.machine, link.epoch);
    let small: Vec<u8> = (0..14).collect();
    // Two frames fit in one physical OUT packet and coalesce into one turn.
    r.send(&small, false); r.send(&small, false);
    while r.credit_counts().len() < 2 { r.frame(); }
    assert_eq!(r.echoes, [small.clone(), small.clone()]);
    assert_eq!(r.credit_counts(), [2, 2], "two freed slots produce one coalesced credit");
    assert_eq!(word(&r.machine, e.received), 2); assert_eq!(word(&r.machine, e.seen), 2);
    assert_eq!(word(&r.machine, e.owed), 0);

    // Stale earns no credit, and validation order puts bad CRC before epoch.
    r.send(&small, true);
    r.send_wire(&raw(watch::ETH, epoch.wrapping_add(1), &small, true));
    r.send_wire(&raw(watch::READ, epoch, &small, false));
    r.send_wire(&raw(watch::ETH, epoch, &[0; 13], false));
    r.send_wire(&raw(watch::ETH, epoch, &vec![0x55; watch::ETH_MAX + 1], false));
    r.send_wire(&[5, 1, 2, 0]); // short COBS block
    // Follow all errors with a valid maximum frame to prove resynchronisation.
    let maximum: Vec<_> = (0..watch::ETH_MAX).map(|i| (i % 251) as u8).collect();
    r.send(&maximum, false);
    while r.credit_counts().len() < 3 { r.frame(); }
    assert_eq!(r.echoes.last(), Some(&maximum), "1514 bytes both ways, including COBS zeros and splits");
    assert_eq!(r.credit_counts(), [2, 2, 1], "invalid frames earn no credit");
    assert_eq!(word(&r.machine, link.eth_stale), 1);
    assert_eq!(word(&r.machine, link.rx_bad), 5);
    assert_eq!(word(&r.machine, link.rx_refused), 0);
    assert_eq!(word(&r.machine, link.tx_dropped), 0);
    assert_eq!(word(&r.machine, e.received), 3);
    assert_eq!(word(&r.machine, e.seen), 3);
    r.replay(&c);
}

#[test]
fn third_frame_without_credit_is_refused_and_unread_frames_do_not_refire() {
    let c = compiled("var calls: i32 = 0\nlet network = eth(2, fn() { calls = calls + 1 })");
    let mut r = Rig::new(&c); r.boot(); let e = r.image.slot.layout.eth.as_ref().unwrap().clone();
    let link = r.image.slot.layout.link.as_ref().unwrap().clone();
    for n in 1..=3 { r.send(&[n; 14], false); }
    while word(&r.machine, link.rx_refused) == 0 { r.frame(); }
    assert_eq!(word(&r.machine, e.received), 2); assert_eq!(word(&r.machine, e.seen), 2);
    assert_eq!(word(&r.machine, link.rx_bad), 0); assert_eq!(word(&r.machine, e.owed), 0);
    assert_eq!(r.credit_counts(), [2]);
    let starts = r.trace.iter().filter(|t| matches!(t, Trace::Start(_))).count();
    r.frame(); r.frame();
    assert_eq!(r.trace.iter().filter(|t| matches!(t, Trace::Start(_))).count(), starts,
        "unread frames alone never retrigger eth");
    r.replay(&c);
}

#[test]
fn arrival_after_last_recv_during_handler_fires_again() {
    // A runtime divisor keeps the loop near one instruction per AIR op, so it
    // outlasts a 1 ms USB frame at CPU_HZ as loop code improves.
    let c = compiled("\
let b = bytes(1514)
var n: i32 = 0
var calls: i32 = 0
var work: i32 = 0
var d: i32 = 3
let network = eth(2, fn() {
    calls = calls + 1
    n = eth_recv(b, 0)
    for i in 0..24000 { work = work + i / d }
})");
    let mut r = Rig::new(&c); r.boot(); let e = r.image.slot.layout.eth.as_ref().unwrap().clone();
    r.send(&[1; 14], false);
    let recv_return = r.image.slot.map.runtime.iter().find(|f| f.name == "eth_recv").unwrap().end - 2;
    r.until_pc(recv_return);
    assert_eq!(word(&r.machine, e.owed), 1, "the first and last recv already freed its slot");
    r.send(&[2; 14], false);
    let arrival = r.machine.now();
    // Stop at the handler's return, before TURN can decode the new packet.
    r.until_pc(r.image.slot.symbols.handler_return);
    assert!(r.machine.now() - arrival > r.machine.tick_hz() / 1000,
        "a complete USB frame passes while the handler is still running after recv");
    assert_eq!(word(&r.machine, e.seen), 1, "return does not acknowledge unseen arrivals");
    while word(&r.machine, e.seen) < 2 { r.frame(); }
    r.until_pc(r.image.slot.symbols.idle_waiti);
    assert_eq!(word(&r.machine, e.received), 2);
    assert_eq!(word(&r.machine, e.owed), 0);
    r.replay(&c);
}

#[test]
fn every_length_and_range_fault_preserves_io_and_the_queued_frame() {
    let c = compiled("\
let b = bytes(1514)
let small = bytes(5)
var result: i32 = 0
let network = eth(2, fn() {
    result = eth_send(b, -1, -1) + eth_send(b, 0, 0) + eth_send(b, 0, 13) + eth_send(b, 0, 1515)
    result = result + eth_send(b, -1, 14) + eth_send(b, 1501, 14) + eth_send(b, 2147483647, 14)
    result = result + eth_recv(b, -1) + eth_recv(b, 1) + eth_recv(b, 2147483647)
    eth_mac(small, 0) eth_mac(b, -1) eth_mac(b, 1509) eth_mac(b, 2147483647)
    result = result + eth_recv(b, 0)
})");
    let mut r = Rig::new(&c); r.boot(); let payload = vec![0xa5; watch::ETH_MAX]; r.send(&payload, false);
    while r.credit_counts().len() < 2 { r.frame(); }
    let p = fab_elec_mcu_rt::Profile { op_ns: 0, ..chip::profile() };
    let mut d = Device::new(module::load(&c.module, &p).unwrap(), &p); let mut h = Reference::default();
    d.boot(&mut h); d.eth_in(1, &payload).unwrap(); d.step(&mut h).unwrap();
    let faults: Vec<_> = r.frames.iter().filter_map(|f| matches!(f.event, watch::Event::Fault { .. }).then_some(f.event)).collect();
    assert_eq!(faults, h.faults, "length precedes range; every fault has the reference code, site and argument");
    assert_eq!(faults.len(), 14);
    assert!(r.echoes.is_empty(), "faulting sends perform no ETH I/O");
    for (b, words) in r.image.slot.layout.buffers.iter().zip(d.buffers()) {
        assert_eq!(read(&r.machine, b.address, b.allocated_bytes as usize), words.iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<_>>(),
            "faulting recv/mac leave buffers intact and valid recv retains its frame");
    }
    assert_eq!(word(&r.machine, r.image.slot.layout.state), 1514, "all faulting returns are zero");
    assert_eq!(r.credit_counts(), [2, 1]);
}

#[test]
fn full_tx_ring_returns_zero_and_counts_drops() {
    let c = compiled("\
let b = bytes(1514)
var sent: i32 = 0
var refused: i32 = 0
var n: i32 = 0
fn setup() {
    fill(b, 0, 1514, 17)
    for i in 0..8 { let queued = eth_send(b, 0, 1514) sent = sent + queued refused = refused + 1 - queued }
}
let network = eth(2, fn() { n = eth_recv(b, 0) })");
    let mut r = Rig::new(&c); r.machine.set_usb_host_listening(false);
    r.until_pc(r.image.slot.symbols.idle_waiti);
    let e = r.image.slot.layout.link.as_ref().unwrap().clone();
    let var = |name: &str| c.lowered.state_vars.iter().find(|v| v.name == name).unwrap().offset;
    let sent = word(&r.machine, r.image.slot.layout.state + var("sent") * 4);
    let refused = word(&r.machine, r.image.slot.layout.state + var("refused") * 4);
    assert!(sent > 0 && refused > 0, "finite physical ring queues some and refuses the rest");
    assert_eq!(sent + refused, 8); assert_eq!(word(&r.machine, e.tx_dropped), refused);
    assert_eq!(word(&r.machine, r.image.slot.layout.lost_count), 0, "ETH refusal is never LOST");
    r.machine.set_usb_host_listening(true);
    while r.echoes.len() < sent as usize { r.frame(); }
    assert_eq!(r.echoes, vec![vec![17; watch::ETH_MAX]; sent as usize], "whole queued frames survive reopening");
    assert_eq!(r.credit_counts(), [2]);
}

#[test]
fn eth_layout_is_optional_and_only_physical_sram_limits_the_slot_count() {
    let plain = compiled("fn setup() {}"); let eth = compiled("let network = eth(2, fn() {})\nfn setup() {}");
    let p = build_slot(&module::load(&plain.module, &chip::profile()).unwrap(), &SlotOptions::default()).unwrap();
    let e = build_slot(&module::load(&eth.module, &chip::profile()).unwrap(), &SlotOptions::default()).unwrap();
    assert!(p.layout.eth.is_none());
    assert!(!p.map.runtime.iter().any(|r| r.name.starts_with("eth_")), "non-eth images emit no link routines");
    assert_eq!(e.layout.runtime_bytes - p.layout.runtime_bytes, 2 * runtime::RX_SLOT_BYTES + 1728,
        "no eth means no receive slots, decoder/counters or TX staging bytes");
    let slots = e.layout.eth.as_ref().unwrap();
    assert_eq!(slots.head - slots.slots, 2 * runtime::RX_SLOT_BYTES);
    assert_eq!(runtime::RX_SLOT_BYTES, 1516);
    let huge = compiled("let network = eth(1000000, fn() {})");
    let error = build_slot(&module::load(&huge.module, &chip::profile()).unwrap(), &SlotOptions::default()).err().unwrap();
    assert!(error.message.contains("SRAM"));
}

#[test]
fn reopen_starts_one_lifetime_and_reports_the_edge_and_stale_epoch() {
    let c = compiled("\
let b = bytes(1514)
let send = after(20000, fn() { eth_send(b, 0, 1514) })
let network = eth(2, fn() {})");
    let mut r = Rig::new(&c); r.boot(); let e = r.image.slot.layout.eth.as_ref().unwrap().clone();
    let link = r.image.slot.layout.link.as_ref().unwrap().clone();
    let first = r.hellos()[0];
    // Leave both slots unread; re-Hello must free them without returning old
    // credits as well as the new full grant.
    r.send(&[1; 14], false); r.send(&[2; 14], false); r.advance_ms(2);
    assert_eq!(word(&r.machine, e.received), 2);
    r.machine.set_usb_host_listening(false);
    r.advance_ms(30);
    assert_ne!(word(&r.machine, r.image.slot.layout.tx_head), word(&r.machine, r.image.slot.layout.tx_tail));
    assert_eq!(word(&r.machine, link.stall_state), 1);
    let due = pair(&r.machine, link.stall_due);
    r.advance_ms(260);
    assert_eq!(word(&r.machine, link.stall_state), 2, "250 ms of no tokens arms the IN wake");
    assert!(pair(&r.machine, r.image.slot.layout.stamp) >= due);
    assert_eq!(pair(&r.machine, link.in_edges), 0);
    assert_eq!(r.hellos().len(), 1, "a closed port alone does not start a new lifetime");
    r.machine.set_usb_host_listening(true);
    // One maximum wire frame plus Hello/credit and LINK fits within 25 USB
    // packets; 30 service frames include the already-flushed packet and phase.
    r.advance_ms(30);
    let hellos = r.hellos(); assert_eq!(hellos.len(), 2);
    assert_eq!(hellos[1].event, first.event, "reset reason, image and clock remain the boot's");
    assert_ne!(hellos[1].tick, first.tick);
    assert_eq!(word(&r.machine, link.epoch), hellos[1].tick);
    assert_eq!(pair(&r.machine, link.in_edges), 1);
    assert_eq!(r.links(0), [1]);
    assert_eq!(r.credit_counts(), [2, 2]);
    for slot in 0..e.count { assert_eq!(read(&r.machine, e.slots + slot * runtime::RX_SLOT_BYTES, 2), [0, 0]); }
    for address in [e.received, e.seen, e.owed, link.rx_mode, link.rx_bytes, link.rx_left, link.rx_zero, link.rx_crc] {
        assert_eq!(word(&r.machine, address), 0, "new lifetime resets RX state");
    }
    assert_eq!(r.echoes, [vec![0; 1514]], "the queued TX frame survives the lifetime change");
    r.send_wire(&raw(watch::ETH, first.tick, &[3; 14], false)); r.advance_ms(2);
    assert_eq!(pair(&r.machine, link.eth_stale), 1); assert_eq!(r.links(1), [1]);
    assert_eq!(r.credit_counts(), [2, 2], "old epochs earn no credit");
    r.advance_ms(260);
    assert_eq!(pair(&r.machine, link.in_edges), 1); assert_eq!(r.hellos().len(), 2);
    let mut notes = Vec::new(); r.machine.take_notes(&mut notes);
    assert!(notes.iter().any(|n| n.what.contains("fc1-seam.md Board check")), "the hardware dependency is visible as a Note");
}

#[test]
fn unanswered_eth_retries_at_doubling_deadlines_until_an_epoch_answer() {
    let c = compiled("\
let b = bytes(1514)
fn setup() { eth_send(b, 0, 14) }
let send = every(1000000, fn() { eth_send(b, 0, 14) })
let network = eth(2, fn() { eth_recv(b, 0) })");
    let mut r = Rig::new(&c); r.boot(); let e = r.image.slot.layout.eth.as_ref().unwrap().clone();
    for (i, seconds) in [2, 4, 8, 16, 30, 30].into_iter().enumerate() {
        let previous = r.hellos()[i];
        r.advance_ms(seconds * 1000 + 2);
        let hellos = r.hellos(); assert_eq!(hellos.len(), i + 2, "one retry after {seconds} s");
        let elapsed = u64::from(hellos[i + 1].tick.wrapping_sub(previous.tick));
        let deadline = seconds * 16_000_000;
        assert!(elapsed >= deadline, "no retry precedes its simulated deadline");
        // The only coincident handler sends 14 bytes. MAX_OP_INSNS covers
        // that port; the dispatch/idle overhead fits RUNTIME_GAP_INSNS.
        let slack = (runtime::MAX_OP_INSNS + runtime::RUNTIME_GAP_INSNS) * 16 / 20;
        assert!(elapsed - deadline <= slack, "link deadline joins sleep, rather than waiting for the 100 ms cap");
        assert_eq!(r.credit_counts(), vec![2; i + 2]);
        assert_eq!(pair(&r.machine, r.image.slot.layout.link.as_ref().unwrap().in_edges), 0, "answer fallback never counts a host-open edge");
    }
    r.send(&[9; 14], false); r.advance_ms(2);
    assert_eq!(word(&r.machine, e.answer_state), 2);
    assert_eq!(word(&r.machine, e.answer_wait), 32_000_000);
    let count = r.hellos().len(); r.advance_ms(30_002);
    assert_eq!(r.hellos().len(), count, "one accepted current-epoch ETH disarms retries despite later sends");
}

#[test]
fn observed_token_restarts_a_stall_window_without_an_edge() {
    let c = compiled("let network = eth(2, fn() {})"); let mut r = Rig::new(&c);
    let l = r.image.slot.layout.clone(); let e = l.link.as_ref().unwrap();
    let stall = r.image.slot.map.runtime.iter().find(|f| f.name == "eth_stall").unwrap().start;
    let drain = r.image.slot.symbols.drain;
    let stops = boot_fixture(&mut r, |a, stops| {
        // Seed a window that has just elapsed, with pending bytes. Wait for real
        // host service to latch RAW bit 8 (the host's IN token; boot can end
        // before the host's first poll). Fill EP1 again so FREE=0 at the check,
        // modelling a slow reader that did poll within the window.
        let token = 0; a.base.label_assign(token);
        a.mov_reg_i32(11, USB_INT_RAW); a.l32i(2, 11, 0); a.extui(2, 2, 8, 1); a.bccz_reg_label(ccz::EQ, 2, token);
        fixture_store(a, l.tx_head, runtime::TX_RING_BYTES - 1); fixture_store(a, l.tx_tail, 64);
        a.mov_reg_i32(11, drain); a.callx0(11);
        // Exercise deadline carry while the elapsed window still uses RAW
        // from real host service. TURN restores the live stamp after the call.
        fixture_store(a, l.stamp, 0xffff_0000); fixture_store(a, l.stamp + 4, 1);
        fixture_store(a, e.stall_state, 1); fixture_store(a, e.stall_due, 0); fixture_store(a, e.stall_due + 4, 0);
        a.mov_reg_i32(11, stall); a.callx0(11); stops.push(SLOT_BASE + a.base.offset());
    });
    r.until_pc(stops[0]);
    assert_eq!(word(&r.machine, e.stall_state), 1, "a token in the elapsed window restarts it");
    assert_eq!(pair(&r.machine, e.stall_due), pair(&r.machine, l.stamp) + 4_000_000);
    assert_eq!(pair(&r.machine, e.in_edges), 0); assert_eq!(word(&r.machine, e.rehello), 0);
    // Drain the zero-delimited fixture bytes, then allow another full window.
    r.advance_ms(400);
    assert_eq!(r.hellos().len(), 1); assert_eq!(pair(&r.machine, e.in_edges), 0);
    assert_eq!(word(&r.machine, e.stall_state), 0, "a drained ring disarms the window and wake");
}

#[test]
fn pending_rehello_is_atomic_and_link_reports_retry_with_64_bit_values() {
    let c = compiled("let network = eth(2, fn() {})"); let mut r = Rig::new(&c);
    let l = r.image.slot.layout.clone(); let e = l.eth.as_ref().unwrap();
    let link = l.link.as_ref().unwrap();
    let hello = r.image.slot.map.runtime.iter().find(|f| f.name == "eth_rehello").unwrap().start;
    let stats = r.image.slot.map.runtime.iter().find(|f| f.name == "eth_link_stats").unwrap().start;
    let drain = r.image.slot.symbols.drain;
    let counters = [link.in_edges, link.eth_stale, link.tx_dropped, link.rx_bad, link.rx_refused];
    let values = [1 << 32, u64::from(u32::MAX), 0x1234_5678_9abc_def0, 1 << 48, 1 << 63];
    let stops = boot_fixture(&mut r, |a, stops| {
        // Exclude the boot Hello's old ring bytes. The remaining bytes are
        // init-cleared COBS delimiters, so the host can consume them normally.
        for offset in (0..64).step_by(4) { fixture_store(a, l.tx_ring + offset, 0); }
        fixture_store(a, l.tx_tail, 64); fixture_store(a, l.tx_head, 63);
        fixture_store(a, link.rehello, 1); fixture_store(a, e.owed, 1);
        fixture_store(a, e.slots, 14); fixture_store(a, link.rx_mode, 1); fixture_store(a, link.rx_bytes, 7);
        for (&counter, &value) in counters.iter().zip(&values) {
            fixture_store(a, counter, value as u32); fixture_store(a, counter + 4, (value >> 32) as u32);
        }
        stops.push(SLOT_BASE + a.base.offset());
        a.mov_reg_i32(11, hello); a.callx0(11); stops.push(SLOT_BASE + a.base.offset());
        // Even with room for Hello alone, both records must wait. At this
        // stop all ring bytes and lifetime state still belong to the old epoch.
        fixture_store(a, l.tx_head, 32); // tail=64 => 31 free bytes
        a.mov_reg_i32(11, hello); a.callx0(11); stops.push(SLOT_BASE + a.base.offset());
        fixture_store(a, l.tx_head, 63);
        // Fill the hardware packet so a refused LINK cannot free ring space.
        a.mov_reg_i32(11, drain); a.callx0(11);
        fixture_store(a, l.tx_head, 63); fixture_store(a, l.tx_tail, 64);
        a.mov_reg_i32(11, stats); a.callx0(11); stops.push(SLOT_BASE + a.base.offset());
    });
    r.machine.set_usb_host_listening(false); r.until_pc(stops[0]);
    let epoch = word(&r.machine, link.epoch); let seq = word(&r.machine, l.seq);
    let bytes = read(&r.machine, l.tx_ring, runtime::TX_RING_BYTES as usize);
    for &pc in &stops[1..3] {
        r.until_pc(pc);
        assert_eq!(word(&r.machine, link.epoch), epoch); assert_eq!(word(&r.machine, l.seq), seq);
        assert_eq!(word(&r.machine, link.rehello), 1); assert_eq!(word(&r.machine, e.owed), 1);
        assert_eq!(read(&r.machine, e.slots, 2), [14, 0]); assert_eq!(word(&r.machine, link.rx_bytes), 7);
        assert_eq!(read(&r.machine, l.tx_ring, runtime::TX_RING_BYTES as usize), bytes, "refusal preserves every TX byte");
    }
    r.until_pc(stops[3]);
    assert_eq!(read(&r.machine, link.reported, 40), [0; 40], "refused LINK snapshots remain unreported");
    assert_eq!(word(&r.machine, l.lost_count), 0);
    r.machine.set_usb_host_listening(true); r.advance_ms(140);
    let hellos = r.hellos(); assert_eq!(hellos.len(), 2);
    assert_ne!(hellos[1].tick, epoch); assert_eq!(r.credit_counts(), [2, 2]);
    assert_eq!(word(&r.machine, link.rehello), 0); assert_eq!(word(&r.machine, e.owed), 0);
    assert_eq!(word(&r.machine, link.rx_bytes), 0); assert_eq!(read(&r.machine, e.slots, 2), [0, 0]);
    for (counter, &value) in values.iter().enumerate() {
        assert_eq!(r.links(counter as u8), [value]);
        assert_eq!(pair(&r.machine, link.reported + counter as u32 * 8), value);
    }
    r.send_wire(&raw(watch::ETH, epoch, &[0; 14], false)); r.advance_ms(2);
    assert_eq!(pair(&r.machine, link.eth_stale), 1 << 32, "increment carries into the high word");
    assert_eq!(r.links(1), [u64::from(u32::MAX), 1 << 32]);
}
