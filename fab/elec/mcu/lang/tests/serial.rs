//! Receive timing, FIFO losses and declaration refusals against the AIR reference.
use fab_elec_mcu::{compile, Compiled, Profile};
use fab_elec_mcu_rt as rt;
use makepad_script_compute::{ir, lower::event::DeclArg};
use rt::module::LoadError;
use rt::ports::{PWM_DUTY, SERIAL_BYTE};
use rt::sched::{Device, Fault, Hal, Ran, Role};
use rt::serial::{self, SerialSetting};

const P: Profile = Profile { id: 3, pins: 20, max_entry_ops: 50_000, call_ops: None, ram_words: 2048, buffer_bytes: 0, eeprom: 0, eeprom_write_ns: 0, f64: false, op_ns: 62, clock_hz: 16_000_000, analog: &rt::analog::NONE, pwm: &rt::pwm::ATMEGA328P, serial: &serial::ATMEGA328P, motion: None, touch: false, audio: None, display: None };
const PINS: &[(&str, u32)] = &[("D9", 9)];
const BYTE_NS: u64 = 520_000;
const SETTING: SerialSetting = SerialSetting { link: 0, asked: 19200, ubrr: 51, u2x: false };

const BYTES: &str = "\
var count = 0
var last = 0
let m = pwm(D9, hz(490))
fn setup() { duty(m, 0) }
let link = serial(19200, fn() {
    last = byte(link)
    count = count + 1
    duty(m, byte(link))
})
";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ev {
    Configure(u64, u32, Role),
    Serial(u64, u32, SerialSetting),
    Duty { t: u64, pin: u32, level: u8, site: u32 },
    Fault(u64, Fault),
}

#[derive(Default)]
struct Scripted { evs: Vec<Ev> }

impl Hal for Scripted {
    fn eeprom_read(&mut self, _: u64, _: u16, _: u32) -> u8 { unreachable!("this profile has no EEPROM") }
    fn eeprom_write(&mut self, _: u64, _: u16, _: u8, _: u32) { unreachable!("this profile has no EEPROM") }
    fn shift(&mut self, _: u64, _: u32, _: u32, _: u32, _: u8, _: u32) { unreachable!("this fixture shifts nothing") }
    fn configure(&mut self, t: u64, pin: u32, role: Role) { self.evs.push(Ev::Configure(t, pin, role)); }
    fn serial(&mut self, t: u64, link: u32, setting: SerialSetting) { self.evs.push(Ev::Serial(t, link, setting)); }
    fn duty(&mut self, t: u64, pin: u32, level: u8, site: u32) { self.evs.push(Ev::Duty { t, pin, level, site }); }
    fn write(&mut self, _: u64, _: u32, _: bool, _: u32) { panic!("receive programs never write an ordinary pin"); }
    fn read(&mut self, _: u64, _: u32, _: u32) -> bool { panic!("receive programs never read a pin"); }
    fn adc(&mut self, _: u64, _: u32, _: u32) -> u16 { unreachable!("this fixture declares no analog input") }
    fn eth_send(&mut self, _: u64, _: &[u8], _: u32) -> bool { false }
    fn eth_mac(&mut self, _: u64, _: u32) -> [u8; 6] { [2, 0, 0, 0, 0, 1] }
    fn clock(&mut self, t: u64, _: u32) -> u64 { t }
    fn fault(&mut self, t: u64, fault: Fault) { self.evs.push(Ev::Fault(t, fault)); }
}

fn compiled(src: &str) -> Compiled {
    compile(src, &P, PINS, &[]).unwrap_or_else(|e| panic!("compile {src:?}: {e:?}"))
}

fn line_of(src: &str, needle: &str) -> u32 {
    let at = src.find(needle).unwrap_or_else(|| panic!("{needle:?} is in the source"));
    src[..at].matches('\n').count() as u32 + 1
}

fn var(c: &Compiled, state: &[u32], name: &str) -> u32 {
    let v = c.lowered.state_vars.iter().find(|v| v.name == name).unwrap_or_else(|| panic!("var {name}"));
    state[v.offset as usize]
}

fn wcet(c: &Compiled) -> u64 {
    c.lowered.entries.iter().map(|e| e.cost).max().expect("an entry") * P.op_ns as u64
}

fn faults(hal: &Scripted) -> Vec<(u64, Fault)> {
    hal.evs.iter().filter_map(|e| if let Ev::Fault(t, f) = *e { Some((t, f)) } else { None }).collect()
}

struct Replay<'a> {
    rx: Option<(u32, u8)>,
    evs: std::iter::Filter<std::slice::Iter<'a, Ev>, fn(&&Ev) -> bool>,
}

impl ir::Ports for Replay<'_> {
    fn call(&mut self, port: u16, site: u32, args: &[u32], rets: &mut [u32]) {
        match port {
            SERIAL_BYTE => {
                let (link, byte) = self.rx.expect("byte called in the reference receive handler");
                assert_eq!(args, [link], "AIR receive uses the event's link");
                assert_eq!(rets.len(), 1, "AIR receive has one result");
                rets[0] = byte as u32;
            }
            PWM_DUTY => {
                let ev = self.evs.next().copied();
                let Some(Ev::Duty { pin, level, site: s, .. }) = ev else { panic!("AIR duty at {site}, device did {ev:?}") };
                assert_eq!((pin, level, s), (args[0], args[1] as u8, site), "AIR duty keeps the received byte and source site");
                assert!(rets.is_empty(), "AIR duty has no result");
            }
            _ => panic!("unexpected AIR port {port} at {site}"),
        }
    }
}

fn reference(c: &Compiled, ran: Ran, state: &mut [u32], actual: &[u32], events: &[Ev]) {
    let e = &c.lowered.entries[ran.entry as usize];
    let pinned: fn(&&Ev) -> bool = |e| matches!(e, Ev::Duty { .. });
    let mut replay = Replay { rx: ran.rx, evs: events.iter().filter(pinned) };
    let mut scratch = vec![0; e.program.scratch_words()];
    let mut mem = ir::Mem { ctx: &mut [], state, shared: ir::Shared::Read(&c.lowered.shared_init), bufs: &[] };
    let mut io = ir::Io { ins: [&[], &[]], outs: [&mut [], &mut []] };
    ir::run_ports(&e.program, &mut scratch, &mut mem, &mut io, 1, &mut replay);
    assert_eq!(&*mem.state, actual, "runtime state diverges from AIR after {} at {}", e.name, ran.start);
    assert_eq!(replay.evs.next(), None, "AIR omitted a device duty");
    assert!(ran.ops <= e.cost, "receive handler exceeded its declared cost");
}

#[test]
fn serial_bytes_at_their_times() {
    let c = compiled(BYTES);
    let module = rt::module::load(&c.module, &P).expect("verified");
    assert_eq!(module.serial(), c.serial, "module discarded the resolved receive settings");
    assert_eq!(c.serial.len(), 1, "one declared link");
    assert_eq!(c.serial[0].setting, SETTING, "U2X chosen on a tie");
    assert_eq!((SETTING.divisor(), SETTING.real_milli_baud(P.clock_hz), SETTING.byte_cycles(10), SETTING.byte_ns(P.clock_hz, 10)), (832, 19_230_769, 8320, BYTE_NS), "baud and byte time must come from clock cycles");
    assert_eq!(c.serial[0].fifo, 3, "ceil(nonzero short handler / 520000) + 2 is three bytes");
    assert_eq!(c.notes.len(), 2, "one PWM note and one serial note");
    assert!(c.notes[0].starts_with("pin D9"), "serial notes must follow PWM notes");
    assert_eq!(c.notes[1], format!("serial link 0: 19230.8 baud (asked 19200), UBRR 51, U2X 0; the watch stream runs at this rate too; FIFO 3 bytes; longest handler {} ns against a byte time of 520000 ns", wcet(&c)), "serial note lost the actual baud, watch rate, FIFO or whole-program WCET");

    let mut dev = Device::new(module, &P);
    let mut hal = Scripted::default();
    let mut state = c.lowered.state_init.clone();
    let setup = dev.boot(&mut hal).expect("setup");
    assert_eq!((setup.rx, setup.timer, setup.start), (None, None, 0), "setup acquired an event identity");
    assert_eq!(hal.evs[..2], [Ev::Configure(0, 9, Role::Pwm(c.pwm[0].setting)), Ev::Serial(0, 0, SETTING)], "serial configured before pin roles or after setup");
    assert!(matches!(hal.evs.get(2), Some(Ev::Duty { level: 0, .. })), "setup must follow serial configuration");
    reference(&c, setup, &mut state, dev.state(), &hal.evs);

    for (k, byte) in [0x01, 0x80, 0xFF, 0x00].into_iter().enumerate() {
        let t = (k as u64 + 1) * BYTE_NS;
        dev.run_until(t, &mut hal);
        let before = hal.evs.len();
        dev.rx(t, 0, byte, &mut hal);
        assert_eq!((dev.next_due(), dev.next_start()), (Some(t), Some(t)), "idle receive delayed past its completion time");
        let ran = dev.step(&mut hal).expect("one byte event");
        assert_eq!((ran.rx, ran.timer, ran.due, ran.start), (Some((0, byte)), None, t, t), "byte event lost its identity or completion time");
        assert_eq!(var(&c, dev.state(), "last"), (byte as f32).to_bits(), "byte() sign-extended or retained an older byte");
        assert_eq!(var(&c, dev.state(), "count"), ((k + 1) as f32).to_bits(), "one event per byte was coalesced or duplicated");
        let body = &c.lowered.entries[ran.entry as usize].program.body;
        assert!(!body.iter().any(|s| matches!(s, ir::Stmt::If(..) | ir::Stmt::Loop { .. })), "timing proof requires the straight-line receive body");
        let duty_ops = ir::unmarked(body).position(|s| matches!(s, ir::Stmt::Port { port: PWM_DUTY, .. })).expect("duty statement") as u64;
        let duty_at = BYTES.find("duty(m, byte(link))").expect("duty source call");
        let site = c.sites.iter().position(|s| s.start as usize == duty_at).expect("duty source site") as u32;
        assert_eq!(c.sites[site as usize].line, line_of(BYTES, "duty(m, byte(link))"), "duty lost its source line");
        assert_eq!(&hal.evs[before..], &[Ev::Duty { t: t + duty_ops * P.op_ns as u64, pin: 9, level: byte, site }], "duty lost its source line or ran off the operation clock");
        reference(&c, ran, &mut state, dev.state(), &hal.evs[before..]);
        assert_eq!(dev.step(&mut hal), None, "receive event ran twice");
    }
    assert!(faults(&hal).is_empty(), "valid serial traffic faulted");
}

// This timer's constant-range loop is capped at 2081 iterations. Its
// 2080 useful iterations take about 2.5 byte times; the conservative cost
// is just under three byte times, so ceil(WCET / byte time) + 2 = 5.
const BUSY_TIMER: &str = "\
var acc: i32 = 0
var last: i32 = 0
var count: i32 = 0
let busy = after(us(0), fn() {
    for i in 0..2080 { acc = (acc + i) % 1000003 }
})
let first = after(us(130), fn() { count = count + 1 })
let second = after(us(130), fn() { count = count + 1 })
let link = serial(19200, fn() {
    last = byte(link)
    count = count + 1
})
";

// 2500 iterations take about three byte times. The declared bound counts
// both loop overheads and the exit branch at every iteration: between
// three and four byte times, hence ceil(WCET / byte time) + 2 = 6.
const BUSY_BYTE: &str = "\
var acc: i32 = 0
var last: i32 = 0
var count: i32 = 0
let link = serial(19200, fn() {
    last = byte(link)
    count = count + 1
    for i in 0..2500 { acc = (acc + i) % 1000003 }
})
";

// Record the starts before advancing to an arrival, then explicitly run up
// to its time. This retains evidence while meeting rx's caller contract.
fn advance(dev: &mut Device<'_>, t: u64, hal: &mut Scripted, rans: &mut Vec<Ran>) {
    while dev.next_start().is_some_and(|start| start <= t) {
        rans.push(dev.step(hal).expect("pending event"));
    }
    dev.run_until(t, hal);
}

// Independent queue model: a vector of retained arrivals, with service
// duration taken from consecutive starts and checked against executed ops.
struct Model {
    held: Vec<(u64, u8)>,
    starts: Vec<(u64, u64, u8)>,
    reports: Vec<(u64, Fault)>,
    dropped: Vec<u8>,
    busy: u64,
    duration: u64,
    limit: usize,
    lost: u32,
}

impl Model {
    fn until(&mut self, t: u64) {
        while let Some(&(due, byte)) = self.held.first() {
            let start = due.max(self.busy);
            if start > t { break; }
            self.held.remove(0);
            if self.lost != 0 {
                self.reports.push((start, Fault::SerialLost { link: 0, count: self.lost, at_least: false }));
                self.lost = 0;
            }
            self.starts.push((due, start, byte));
            self.busy = start + self.duration;
        }
    }

    fn receive(&mut self, t: u64, byte: u8) {
        self.until(t);
        if self.held.len() == self.limit {
            self.dropped.push(byte);
            self.lost += 1;
        } else {
            self.held.push((t, byte));
        }
    }
}

#[test]
fn fifo_order_and_loss() {
    let c = compiled(BUSY_TIMER);
    let byte_ns = c.serial[0].setting.byte_ns(P.clock_hz, P.serial.frame_bits);
    let wcet_ns = wcet(&c);
    assert!(wcet_ns > 2 * byte_ns && wcet_ns < 3 * byte_ns, "timer must bound receive capacity at three byte intervals: {wcet_ns}");
    assert_eq!((c.serial[0].fifo, wcet_ns.div_ceil(byte_ns) + 2), (5, 5), "fifo sized from the byte handler only");
    let module = rt::module::load(&c.module, &P).expect("verified");
    let mut dev = Device::new(module, &P);
    let mut hal = Scripted::default();
    assert_eq!(dev.boot(&mut hal), None, "no implicit setup handler");
    let long = dev.step(&mut hal).expect("busy timer");
    let end = long.start + long.ops * P.op_ns as u64;
    assert_eq!((long.timer, long.rx, long.start), (Some(0), None, 0), "serial consumed a timer handle");
    assert!(end > 23 * byte_ns / 10 && end < 27 * byte_ns / 10, "timer must run about 2.5 byte times: {end}");
    // Inject a burst every quarter byte time to fill all five slots while
    // the timer is busy. rx accepts completion times supplied by its caller.
    for byte in 1..=5 {
        let t = byte as u64 * byte_ns / 4;
        dev.run_until(t, &mut hal);
        dev.rx(t, 0, byte, &mut hal);
        assert_eq!(dev.next_start(), Some(end), "queued events started while the timer still held the CPU");
    }
    let mut rans = vec![long];
    while let Some(ran) = dev.step(&mut hal) { rans.push(ran); }
    assert_eq!(rans.iter().map(|r| (r.timer, r.rx)).collect::<Vec<_>>(), [
        (Some(0), None), (Some(1), None), (Some(2), None),
        (None, Some((0, 1))), (None, Some((0, 2))), (None, Some((0, 3))), (None, Some((0, 4))), (None, Some((0, 5))),
    ], "equal-time bytes overtook timers, timer numbers changed order, or FIFO reordered bytes");
    for pair in rans.windows(2) {
        assert_eq!(pair[1].start, pair[0].start + pair[0].ops * P.op_ns as u64, "queued handlers must run back to back");
    }
    assert_eq!(rans[3..].iter().map(|r| r.due).collect::<Vec<_>>(), (1..=5).map(|k| k * byte_ns / 4).collect::<Vec<_>>(), "late bytes lost their original completion times");
    assert_eq!((var(&c, dev.state(), "last"), var(&c, dev.state(), "count")), (5, 7), "five bytes and two short timers did not all run");
    assert!(faults(&hal).is_empty(), "the five-byte FIFO lost a byte: {:?}", faults(&hal));

    let c = compiled(BUSY_BYTE);
    let wcet_ns = wcet(&c);
    assert!(wcet_ns > 3 * BYTE_NS && wcet_ns < 4 * BYTE_NS, "receive bound must span four byte intervals: {wcet_ns}");
    assert_eq!((c.serial[0].fifo, wcet_ns.div_ceil(BYTE_NS) + 2), (6, 6), "receive FIFO omitted the two spare slots");
    let module = rt::module::load(&c.module, &P).expect("verified");
    let mut dev = Device::new(module, &P);
    let mut hal = Scripted::default();
    assert_eq!(dev.boot(&mut hal), None, "no implicit setup handler");
    let mut rans = Vec::new();
    for byte in 0..40 {
        let t = (byte as u64 + 1) * BYTE_NS;
        advance(&mut dev, t, &mut hal, &mut rans);
        let events = hal.evs.len();
        dev.rx(t, 0, byte, &mut hal);
        assert_eq!(hal.evs.len(), events, "loss reported at the drop instead of the next handler");
    }
    while let Some(ran) = dev.step(&mut hal) { rans.push(ran); }
    let duration = rans[1].start - rans[0].start;
    assert_eq!(duration, rans[0].ops * P.op_ns as u64, "queued handler used WCET instead of executed operations");
    assert!(duration > 28 * BYTE_NS / 10 && duration < 32 * BYTE_NS / 10, "receive handler must run about three byte times: {duration}");
    assert!(rans.iter().all(|r| r.ops == rans[0].ops), "constant-work receive handler changed its service time");
    let mut model = Model { held: Vec::new(), starts: Vec::new(), reports: Vec::new(), dropped: Vec::new(), busy: 0, duration, limit: 6, lost: 0 };
    for byte in 0..40 { model.receive((byte as u64 + 1) * BYTE_NS, byte); }
    model.until(u64::MAX);
    let handled: Vec<_> = rans.iter().map(|r| {
        let (link, byte) = r.rx.expect("receive event");
        assert_eq!((link, r.timer), (0, None), "byte acquired a timer identity");
        (r.due, r.start, byte)
    }).collect();
    assert_eq!(handled, model.starts, "FIFO keeps the newest instead of the oldest, or uses WCET instead of actual handler time");
    let dropped: Vec<_> = (0..40).filter(|b| !handled.iter().any(|(_, _, byte)| byte == b)).collect();
    assert_eq!(dropped, model.dropped, "overflow dropped an older held byte instead of the newest arrival");
    let reports = faults(&hal);
    assert_eq!(reports, model.reports, "loss count was not reset, or was reported away from the next byte handler's start");
    let lost: u32 = reports.iter().map(|(_, f)| match f {
        Fault::SerialLost { link: 0, count, at_least: false } => *count,
        _ => panic!("expected exact model FIFO loss, got {f:?}"),
    }).sum();
    assert!(lost > 0 && reports.len() > 1, "forty arrivals must exercise repeated overflow reports");
    assert_eq!(lost as usize, dropped.len(), "loss reports do not account for the model's discarded arrivals");
    assert_eq!(rans.len() + lost as usize, 40, "handled plus lost is not forty");
    assert_eq!((rans.len(), lost), (20, 20), "six waiting slots and fourteen starts during forty arrivals must retain twenty bytes");
    assert_eq!(var(&c, dev.state(), "count") as usize, rans.len(), "lost bytes ran handlers");
    assert_eq!(var(&c, dev.state(), "last"), handled.last().expect("handled bytes").2 as u32, "final state did not come from the last retained byte");
    assert!(reports.iter().all(|(t, _)| rans.iter().any(|r| r.start == *t)), "loss reported at the drop instead of the next handler");

    // Two synthetic links make the cross-link ordering and summed RAM
    // observable. Setup is now the long entry, so it must size both FIFOs.
    const TWO: serial::SerialTable = serial::SerialTable { links: 2, ..serial::ATMEGA328P };
    let profile = Profile { serial: &TWO, ..P };
    let src = "\
var acc: i32 = 0
var last0: i32 = 0
var last1: i32 = 0
var wrong: i32 = 0
fn setup() { for i in 0..2080 { acc = (acc + i) % 1000003 } }
let link0 = serial(19200, fn() { last0 = byte(link0) })
let link1 = serial(19200, fn() {
    last1 = byte(link1)
    wrong = byte(link0)
})
";
    let c = compile(src, &profile, PINS, &[]).unwrap_or_else(|e| panic!("two links: {e:?}"));
    assert_eq!(c.serial.iter().map(|s| (s.link, s.setting.link, s.fifo)).collect::<Vec<_>>(), [(0, 0, 5), (1, 1, 5)], "setup was excluded from FIFO sizing or links lost declaration order");
    let module = rt::module::load(&c.module, &profile).expect("verified");
    let scratch = (0..module.entries().len()).map(|k| module.scratch_words(k)).max().expect("scratch");
    let small = Profile { ram_words: module.state_words() + scratch as u32 + 5, ..profile };
    assert!(matches!(rt::module::load(&c.module, &small), Err(LoadError::Ram { fifo: 10, .. })), "RAM counted only the largest FIFO instead of their sum");
    let mut dev = Device::new(module, &profile);
    let mut hal = Scripted::default();
    let setup = dev.boot(&mut hal).expect("long setup");
    let end = setup.ops * P.op_ns as u64;
    assert_eq!(hal.evs, [Ev::Serial(0, 0, SETTING), Ev::Serial(0, 1, SerialSetting { link: 1, ..SETTING })], "boot changed link configuration order");
    dev.run_until(130_000, &mut hal);
    for (link, byte) in [(1, 9), (0, 7), (0, 8)] { dev.rx(130_000, link, byte, &mut hal); }
    dev.run_until(260_000, &mut hal);
    dev.rx(260_000, 1, 10, &mut hal);
    assert_eq!((dev.next_due(), dev.next_start()), (Some(130_000), Some(end)), "setup failed to hold received bytes");
    let mut rans = Vec::new();
    while let Some(ran) = dev.step(&mut hal) { rans.push(ran); }
    assert_eq!(rans.iter().map(|r| (r.due, r.rx)).collect::<Vec<_>>(), [(130_000, Some((0, 7))), (130_000, Some((0, 8))), (130_000, Some((1, 9))), (260_000, Some((1, 10)))], "equal-time bytes must sort by link, then arrival, before later times");
    assert_eq!((var(&c, dev.state(), "last0"), var(&c, dev.state(), "last1"), var(&c, dev.state(), "wrong")), (8, 10, u32::MAX), "byte() returned another link's last byte inside a receive handler");
    let site = c.sites.iter().position(|s| s.line == line_of(src, "wrong = byte(link0)")).expect("cross-link site") as u32;
    assert_eq!(faults(&hal).iter().map(|(_, f)| *f).collect::<Vec<_>>(), [Fault::Serial { port: SERIAL_BYTE, link: 0, site }; 2], "cross-link byte() did not fault at its source site");
}

fn refusal(src: &str, profile: &Profile, needle: &str, message: &str) {
    match compile(src, profile, PINS, &[]) {
        Ok(_) => panic!("compiled, but must refuse {needle}: {src}"),
        Err(es) => {
            assert_eq!(es.len(), 1, "one declaration must produce one refusal");
            assert_eq!(es[0].message, message, "wrong serial refusal for {needle}");
            assert_eq!(es[0].start, src.find(needle).expect("declaration in source"), "serial refusal escaped its declaration span");
            assert_eq!(es[0].line_col(src).0 as u32, line_of(src, needle), "serial refusal escaped its source line");
        }
    }
}

#[test]
fn serial_refusals_and_misuse() {
    assert_eq!(SERIAL_BYTE, 9, "serial byte port was renumbered");
    for (baud, ubrr, milli, ns) in [(19200, 51, 19_230_769, BYTE_NS), (250000, 3, 250_000_000, 40_000)] {
        let c = compiled(&format!("let link = serial({baud}, fn() {{}})\n"));
        let s = c.serial[0].setting;
        assert_eq!(s, SerialSetting { link: 0, asked: baud, ubrr, u2x: false }, "U2X chosen on a tie");
        assert_eq!((s.real_milli_baud(P.clock_hz), s.byte_ns(P.clock_hz, 10)), (milli, ns), "serial rate or frame time used the requested baud instead of the divisor");
    }
    assert_eq!(serial::resolve(P.serial, P.clock_hz, 0, 115200), Err(117_647_059), "receiver tolerance accepted 115200 or chose the farther normal-speed rate");
    assert_eq!(serial::resolve(P.serial, P.clock_hz, 0, 1), Err(244_141), "UBRR did not clamp to 4095 for a baud below the available range");
    assert_eq!(serial::resolve(P.serial, P.clock_hz, 0, u32::MAX), Err(2_000_000_000), "UBRR did not clamp to zero for a baud above the available range");
    assert_eq!(serial::resolve(P.serial, P.clock_hz, 0, 0), Err(0), "zero baud must not divide by zero");
    assert_eq!(serial::resolve(P.serial, P.clock_hz, 0, 57600), Ok(SerialSetting { link: 0, asked: 57600, ubrr: 34, u2x: true }), "double speed's closer valid rate was ignored");
    assert_eq!(serial::resolve(P.serial, 16_320, 0, 1000), Ok(SerialSetting { link: 0, asked: 1000, ubrr: 0, u2x: false }), "normal speed's inclusive 2.0 percent limit was replaced with double speed's 1.5 percent limit");
    assert_eq!(serial::resolve(P.serial, 16_321, 0, 1000), Err(1_020_063), "baud just beyond the receiver limit was accepted");
    refusal("let link = serial(115200, fn() {})\n", &P, "serial(115200", "the chip's UART cannot run 115200 baud closely enough: the nearest is 117647.1 baud (2.1 % off; a receiver allows 1.5 %)");
    for baud in [0, -1] {
        refusal(&format!("let link = serial({baud}, fn() {{}})\n"), &P, "serial(", "a baud rate must be above 0");
    }
    refusal("let link = serial(250000, fn() {})\nlet link2 = serial(19200, fn() {})\n", &P, "serial(19200", "this chip has 1 serial link: `link2` would be the second");

    let c = compiled(BYTES);
    let module = rt::module::load(&c.module, &P).expect("verified");
    let scratch = (0..module.entries().len()).map(|k| module.scratch_words(k)).max().expect("scratch");
    let state = module.state_words();
    let fifo = c.serial[0].fifo;
    let limit = state + scratch as u32 + 1;
    let small = Profile { ram_words: limit, ..P };
    // Each queued byte consumes a whole four-byte word: three bytes need
    // three words (12 bytes), not one packed word. Leave just one word free.
    assert_eq!(fifo, 3, "RAM proof requires a three-byte FIFO");
    assert!(matches!(rt::module::load(&c.module, &small), Err(LoadError::Ram { state: s, scratch: w, fifo: 3, limit: l }) if s == state && w == scratch && l == limit), "receive bytes were packed into one word or omitted from RAM");
    refusal(BYTES, &small, "serial(19200", &format!("this does not fit the chip: the program needs {} words of RAM ({state} of state, {scratch} for the largest handler, {fifo} for serial receive buffers); this chip has {limit}", state + scratch as u32 + fifo));

    let src = "\
var last: i32 = 0
var wrong: i32 = 0
let link = serial(19200, fn() { last = byte(link) })
let tick = every(us(1000), fn() {
    wrong = byte(link)
    stop(tick)
})
";
    let c = compiled(src);
    let module = rt::module::load(&c.module, &P).expect("verified");
    let mut dev = Device::new(module, &P);
    let mut hal = Scripted::default();
    dev.boot(&mut hal);
    dev.run_until(BYTE_NS, &mut hal);
    dev.rx(BYTE_NS, 0, 0x80, &mut hal);
    let rx = dev.step(&mut hal).expect("valid receive");
    assert_eq!(rx.rx, Some((0, 0x80)), "valid byte must precede misuse");
    let timer = dev.step(&mut hal).expect("timer misuse");
    assert_eq!((timer.timer, timer.rx, timer.start), (Some(0), None, 1_000_000), "serial and timer handles share an ordinal class");
    assert_eq!((var(&c, dev.state(), "last"), var(&c, dev.state(), "wrong")), (0x80, u32::MAX), "byte() returns the last byte outside its handler");
    let site = c.sites.iter().position(|s| s.line == line_of(src, "wrong = byte(link)")).expect("misuse site") as u32;
    let report = faults(&hal);
    assert_eq!(report.len(), 1, "misuse must fault exactly once");
    assert_eq!(report[0].1, Fault::Serial { port: SERIAL_BYTE, link: 0, site }, "byte misuse lost its link or source site");
    assert!(report[0].0 >= timer.start && report[0].0 < timer.start + timer.ops * P.op_ns as u64 && (report[0].0 - timer.start) % P.op_ns as u64 == 0, "misuse fault missed the port's operation time");
    let before = dev.state().to_vec();
    dev.run_until(2_000_000, &mut hal);
    dev.rx(2_000_000, 1, 0xFF, &mut hal);
    assert_eq!(hal.evs.last(), Some(&Ev::Fault(2_000_000, Fault::Serial { port: SERIAL_BYTE, link: 1, site: u32::MAX })), "unknown RX link needs a fault at reception time with no source site");
    assert_eq!((dev.next_due(), dev.next_start(), dev.step(&mut hal)), (None, None, None), "unknown RX link queued a handler");
    assert_eq!(dev.state(), before, "unknown RX link changed program state");

    let c = compiled("fn setup() {}\n");
    let module = rt::module::load(&c.module, &P).expect("verified without serial");
    let mut dev = Device::new(module, &P);
    let mut hal = Scripted::default();
    dev.boot(&mut hal);
    dev.run_until(BYTE_NS, &mut hal);
    dev.rx(BYTE_NS, 0, 1, &mut hal);
    assert_eq!(hal.evs, [Ev::Fault(BYTE_NS, Fault::Serial { port: SERIAL_BYTE, link: 0, site: u32::MAX })], "an available but undeclared link accepted a byte or was configured at boot");
    assert_eq!(dev.step(&mut hal), None, "undeclared receive link queued an event");

    let mut c = compiled("let link = serial(19200, fn() {})\nlet timer = after(us(1), fn() {})\n");
    c.lowered.decls[0].handle = 1;
    let bad = fab_elec_mcu::encode::encode(&c.lowered, P.id).expect("encode malformed handle");
    assert!(matches!(rt::module::load(&bad, &P), Err(LoadError::DeclHandle { decl: 0, found: 1, expected: 0 })), "module trusted a serial handle instead of its ordinal");
    c.lowered.decls[0].handle = 0;
    let entry = c.lowered.decls[1].args[1];
    assert!(matches!(entry, DeclArg::Entry(_)), "timer entry argument");
    c.lowered.decls[0].args[1] = entry;
    let bad = fab_elec_mcu::encode::encode(&c.lowered, P.id).expect("encode duplicate entry");
    assert!(matches!(rt::module::load(&bad, &P), Err(LoadError::DuplicateEntry { decl: 1, .. })), "timer and serial handlers were allowed to bind the same entry");
    c.lowered.decls.swap(0, 1);
    let bad = fab_elec_mcu::encode::encode(&c.lowered, P.id).expect("encode duplicate serial entry");
    assert!(matches!(rt::module::load(&bad, &P), Err(LoadError::DuplicateEntry { decl: 1, .. })), "serial accepted an entry already bound to a timer");
}
