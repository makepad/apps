//! Ethernet ports agree with compute AIR; arrivals determine handler readiness.
use std::collections::VecDeque;
use fab_elec_mcu::{compile, encode, Compiled, Profile, FORMS, PORTS};
use fab_elec_mcu_rt as rt;
use makepad_script_compute::{ir::{self, RawBuf}, lower::event::{DeclArg, DeclHandle}};
use rt::{format::*, module::{self, LoadError}, ports, sched::{Device, EthIn, Fault, Hal, Ran, Role}, watch};

const SOURCE: &str = include_str!("support/eth.splash");
const MAC: [u8; 6] = [2, 0x13, 0x80, 0xff, 0, 0x42];
const P: Profile = Profile {
    id: 0, pins: 0, max_entry_ops: 50_000, call_ops: None, ram_words: 4096, buffer_bytes: 4096, eeprom: 0, eeprom_write_ns: 0,
    f64: false, op_ns: 0, clock_hz: 16_000_000,
    analog: &rt::analog::NONE, pwm: &rt::pwm::NONE, serial: &rt::serial::ATMEGA328P, motion: None, touch: false, audio: None, display: None,
};

fn compiled(source: &str) -> Compiled {
    compile(source, &P, &[], &[]).unwrap_or_else(|e| panic!("eth fixture compiles: {e:?}"))
}

#[derive(Default)]
struct Recording {
    full: bool,
    sends: Vec<(u32, Vec<u8>)>,
    macs: Vec<u32>,
    faults: Vec<Fault>,
    times: Vec<u64>,
}

impl Hal for Recording {
    fn eeprom_read(&mut self, _: u64, _: u16, _: u32) -> u8 { unreachable!("this profile has no EEPROM") }
    fn eeprom_write(&mut self, _: u64, _: u16, _: u8, _: u32) { unreachable!("this profile has no EEPROM") }
    fn shift(&mut self, _: u64, _: u32, _: u32, _: u32, _: u8, _: u32) { unreachable!("this fixture shifts nothing") }
    fn configure(&mut self, _: u64, _: u32, _: Role) { panic!("eth fixture declares no pins"); }
    fn serial(&mut self, _: u64, _: u32, _: rt::serial::SerialSetting) {}
    fn write(&mut self, _: u64, _: u32, _: bool, _: u32) { panic!("eth fixture writes no pins"); }
    fn duty(&mut self, _: u64, _: u32, _: u8, _: u32) { panic!("eth fixture has no PWM"); }
    fn read(&mut self, _: u64, _: u32, _: u32) -> bool { panic!("eth fixture reads no pins"); }
    fn adc(&mut self, _: u64, _: u32, _: u32) -> u16 { panic!("eth fixture has no ADC"); }
    fn clock(&mut self, t: u64, _: u32) -> u64 { t }
    fn eth_send(&mut self, t: u64, frame: &[u8], site: u32) -> bool {
        self.times.push(t);
        self.sends.push((site, frame.to_vec()));
        !self.full
    }
    fn eth_mac(&mut self, t: u64, site: u32) -> [u8; 6] {
        self.times.push(t);
        self.macs.push(site);
        MAC
    }
    fn fault(&mut self, t: u64, fault: Fault) { self.times.push(t); self.faults.push(fault); }
}

struct Core {
    state: Vec<u32>,
    buffers: Vec<Vec<u32>>,
    pending: VecDeque<Vec<u8>>,
    hal: Recording,
}

impl Core {
    fn new(c: &Compiled) -> Self {
        Self {
            state: c.lowered.state_init.clone(),
            buffers: c.lowered.buffers.iter().map(|b| vec![0; (b.len as usize * b.elem.bytes()).div_ceil(4)]).collect(),
            pending: VecDeque::new(), hal: Recording::default(),
        }
    }

    fn run(&mut self, c: &Compiled, ran: Ran, dev: &Device<'_>, hal: &Recording) {
        let bufs: Vec<_> = c.lowered.buffers.iter().zip(&mut self.buffers).map(|(b, words)| RawBuf {
            ptr: words.as_mut_ptr(), len: b.len as usize, writable: b.writable, elem: b.elem,
        }).collect();
        let entry = &c.lowered.entries[ran.entry as usize];
        let mut scratch = vec![0; entry.program.scratch_words()];
        let mut mem = ir::Mem { ctx: &mut [], state: &mut self.state, shared: ir::Shared::Read(&c.lowered.shared_init), bufs: &bufs };
        let mut io = ir::Io::default();
        let mut ports = CorePorts { bufs: &bufs, pending: &mut self.pending, hal: &mut self.hal };
        ir::run_ports(&entry.program, &mut scratch, &mut mem, &mut io, 1, &mut ports);
        assert_eq!(dev.state(), self.state, "{}: all state words match the language interpreter", entry.name);
        assert_eq!(dev.buffers(), self.buffers, "{}: buffer bytes and padding match the language interpreter", entry.name);
        assert_eq!(hal.sends, self.hal.sends, "{}: sent frames and sites match the language interpreter", entry.name);
        assert_eq!(hal.macs, self.hal.macs, "{}: MAC reads and sites match the language interpreter", entry.name);
        assert_eq!(hal.faults, self.hal.faults, "{}: named faults match the language interpreter", entry.name);
        assert!(ran.ops <= entry.cost, "{}: execution stays within the verified cost", entry.name);
    }
}

struct CorePorts<'a> {
    bufs: &'a [RawBuf],
    pending: &'a mut VecDeque<Vec<u8>>,
    hal: &'a mut Recording,
}

impl ir::Ports for CorePorts<'_> {
    fn call(&mut self, port: u16, site: u32, args: &[u32], rets: &mut [u32]) {
        let buf = self.bufs[args[0] as usize];
        if port == ports::FILL {
            for i in args[1]..args[1] + args[2] { buf.store_word(i as usize, args[3]); }
            return;
        }
        if let Some(r) = rets.first_mut() { *r = 0; }
        let off = args[1] as i32;
        let n = match port {
            ports::ETH_SEND => args[2] as i32,
            ports::ETH_RECV => watch::ETH_MAX as i32,
            ports::ETH_MAC => 6,
            _ => panic!("eth oracle received unexpected port {port}"),
        };
        if port == ports::ETH_SEND && (n < watch::ETH_MIN as i32 || n > watch::ETH_MAX as i32) {
            self.hal.faults.push(Fault::EthLength { site, n });
        } else if off < 0 || off as u64 + n as u64 > buf.len as u64 {
            self.hal.faults.push(Fault::EthRange { port, site, off });
        } else {
            let start = off as usize;
            match port {
                ports::ETH_SEND => {
                    let bytes = (start..start + n as usize).map(|i| buf.load_word(i) as u8).collect();
                    self.hal.sends.push((site, bytes));
                    rets[0] = u32::from(!self.hal.full);
                }
                ports::ETH_RECV => if let Some(frame) = self.pending.pop_front() {
                    for (i, &b) in frame.iter().enumerate() { buf.store_word(start + i, u32::from(b)); }
                    rets[0] = frame.len() as u32;
                },
                _ => {
                    self.hal.macs.push(site);
                    for (i, b) in MAC.into_iter().enumerate() { buf.store_word(start + i, u32::from(b)); }
                }
            }
        }
    }
}

fn value(c: &Compiled, dev: &Device<'_>, name: &str) -> u32 {
    let v = c.lowered.state_vars.iter().find(|v| v.name == name).expect("named state exists");
    dev.state()[v.offset as usize]
}

fn frame(n: usize, seed: u8) -> Vec<u8> { (0..n).map(|i| (i as u8).wrapping_add(seed)).collect() }

#[test]
fn frames_match_the_interpreter_and_coalesce() {
    let c = compiled(SOURCE);
    let mut dev = Device::new(module::load(&c.module, &P).expect("eth module loads"), &P);
    let mut hal = Recording::default();
    let mut core = Core::new(&c);
    let ran = dev.boot(&mut hal).expect("setup reads the MAC");
    core.run(&c, ran, &dev, &hal);
    for (i, byte) in MAC.into_iter().enumerate() {
        assert_eq!(value(&c, &dev, &format!("mac{i}")), u32::from(byte), "MAC byte {i} is recorded unsigned in state");
    }
    let frames = [frame(watch::ETH_MIN, 0), frame(65, 128), frame(watch::ETH_MAX, 255)];
    dev.run_until(100, &mut hal);
    for bytes in &frames {
        assert_eq!(dev.eth_in(100, bytes), Ok(()), "one batch occupies the three declared RX buffers");
        core.pending.push_back(bytes.clone());
    }
    assert_eq!(dev.next_due(), Some(100), "the first unseen arrival supplies the coalesced due time");
    let ran = dev.step(&mut hal).expect("one eth firing");
    assert_eq!((ran.due, ran.start, ran.rx, ran.timer), (100, 100, None, None), "eth uses its arrival time without acquiring a timer or serial identity");
    core.run(&c, ran, &dev, &hal);
    assert_eq!(hal.sends.iter().map(|(_, b)| b).collect::<Vec<_>>(), frames.iter().collect::<Vec<_>>(), "minimum, middle and maximum frames echo byte-exact in FIFO order");
    for (name, expected) in [("firings", 1), ("frames", 3), ("first", 14), ("last", 1514), ("total", 1593), ("queued", 3)] {
        assert_eq!(value(&c, &dev, name), expected, "{name}: coalesced echo records the expected state");
    }
    let bytes: Vec<_> = dev.buffers()[0].iter().flat_map(|w| w.to_le_bytes()).collect();
    assert_eq!(&bytes[..3], &[165; 3], "receive preserves the prefix before off");
    assert_eq!(&bytes[1517..1520], &[165; 3], "receive preserves bytes after the maximum frame");
    assert_eq!(dev.step(&mut hal), None, "three arrivals coalesce into one handler");

    hal.full = true;
    core.hal.full = true;
    dev.run_until(200, &mut hal);
    assert_eq!(dev.eth_in(200, &frames[0]), Ok(()), "recv freed capacity for another arrival");
    core.pending.push_back(frames[0].clone());
    let ran = dev.step(&mut hal).expect("a later arrival rearms eth");
    core.run(&c, ran, &dev, &hal);
    assert_eq!((value(&c, &dev, "frames"), value(&c, &dev, "queued")), (4, 3), "a full HAL returns zero without losing the receive result");
    assert_eq!(dev.step(&mut hal), None, "empty receive returns zero without scheduling a new firing");
    assert!(hal.faults.is_empty(), "valid frames and a full transmitter never fault");
}

#[test]
fn unread_frames_wait_for_an_arrival_and_refusals_change_nothing() {
    let c = compiled(&SOURCE.replace("var take: i32 = 3", "var take: i32 = 1"));
    let mut dev = Device::new(module::load(&c.module, &P).expect("one-read fixture loads"), &P);
    let mut hal = Recording::default();
    let mut core = Core::new(&c);
    core.run(&c, dev.boot(&mut hal).expect("setup"), &dev, &hal);
    dev.run_until(10, &mut hal);
    for n in [14, 15, 16] {
        let bytes = frame(n, n as u8);
        assert_eq!(dev.eth_in(10, &bytes), Ok(()), "all declared slots can hold a frame");
        core.pending.push_back(bytes);
    }
    assert_eq!(dev.eth_in(10, &[0; 14]), Err(EthIn::Full), "a host without a credit gets Full");
    assert_eq!(dev.eth_in(10, &[0; 13]), Err(EthIn::Length), "length validation also applies when full");
    assert_eq!(dev.next_due(), Some(10), "refusals preserve the first arrival's due time");
    core.run(&c, dev.step(&mut hal).expect("first arrival batch"), &dev, &hal);
    assert_eq!(dev.next_due(), None, "unread frames do not refire a returned handler");
    dev.run_until(20, &mut hal);
    for n in [0, watch::ETH_MIN - 1, watch::ETH_MAX + 1] {
        assert_eq!(dev.eth_in(20, &vec![0; n]), Err(EthIn::Length), "length {n} is refused");
        assert_eq!(dev.next_due(), None, "a refused length cannot rearm the handler");
    }
    dev.run_until(30, &mut hal);
    let newest = frame(17, 17);
    assert_eq!(dev.eth_in(30, &newest), Ok(()), "one read freed one slot");
    core.pending.push_back(newest);
    assert_eq!(dev.next_due(), Some(30), "the new frame, not an unread old frame, supplies readiness");
    core.run(&c, dev.step(&mut hal).expect("new arrival"), &dev, &hal);
    assert_eq!(value(&c, &dev, "last"), 15, "the oldest unread frame survives Full and Length refusals");
    assert_eq!(dev.step(&mut hal), None, "remaining old frames stay quiet after the second return");
    dev.boot(&mut hal);
    assert_eq!(dev.next_due(), None, "boot clears Ethernet readiness");
    for _ in 0..3 { assert_eq!(dev.eth_in(40, &[0; 14]), Ok(()), "boot releases every RX slot"); }
    let plain = compiled("fn setup() {}");
    let mut plain = Device::new(module::load(&plain.module, &P).expect("plain module loads"), &P);
    assert_eq!(plain.eth_in(0, &[0; 14]), Err(EthIn::Undeclared), "ingress without eth is named Undeclared");
    assert_eq!(plain.next_due(), None, "undeclared ingress creates no readiness");
}

#[test]
fn due_order_and_arrivals_during_a_handler() {
    let source = "\
var firings: i32 = 0
var byte_seen: i32 = 0
var work: i32 = 0
fn setup() { for i in 0..200 { work = work + i } }
let first = after(10, fn() {})
let second = after(10, fn() {})
let link = serial(19200, fn() { byte_seen = byte(link) })
let network = eth(3, fn() { firings = firings + 1 })
";
    let c = compiled(source);
    let profile = Profile { op_ns: 10, ..P };
    let mut dev = Device::new(module::load(&c.module, &profile).expect("ordered fixture loads"), &profile);
    let mut hal = Recording::default();
    let setup = dev.boot(&mut hal).expect("setup holds the CPU across the tied due time");
    assert!(setup.ops * u64::from(profile.op_ns) > 10_000, "setup keeps timers pending while same-time ingress is queued");
    dev.run_until(10_000, &mut hal);
    dev.rx(10_000, 0, 42, &mut hal);
    assert_eq!(dev.eth_in(10_000, &[0; 14]), Ok(()), "eth joins the same due-time batch");
    for timer in [0, 1] {
        let ran = dev.step(&mut hal).expect("timer at tied due time");
        assert_eq!((ran.timer, ran.due), (Some(timer), 10_000), "timer {timer} wins ties in existing timer order");
    }
    let ran = dev.step(&mut hal).expect("serial at tied due time");
    assert_eq!((ran.rx, ran.due), (Some((0, 42)), 10_000), "serial wins its tie with eth");
    let ran = dev.step(&mut hal).expect("eth after the tied timer and serial events");
    assert_eq!(ran.due, 10_000, "eth retains its original due time while the CPU is busy");
    let end = ran.start + ran.ops * u64::from(profile.op_ns);
    assert!(end > ran.start + 1, "eth handler occupies a nonzero simulated interval");
    dev.run_until(ran.start + 1, &mut hal);
    assert_eq!(dev.eth_in(ran.start + 1, &[1; 14]), Ok(()), "an arrival during the eth handler consumes a free slot");
    assert_eq!(dev.next_due(), Some(ran.start + 1),
        "an arrival during the handler rearms it: the handler may have read the queue before the frame came");
    let again = dev.step(&mut hal).expect("in-run arrival rearms eth");
    assert_eq!((again.due, again.start), (ran.start + 1, end), "the rearmed handler waits for the CPU");
    assert_eq!(dev.next_due(), None, "unread frames alone do not rearm the handler");
    let end = again.start + again.ops * u64::from(profile.op_ns);
    dev.run_until(end + 1, &mut hal);
    assert_eq!(dev.eth_in(end + 1, &[2; 14]), Ok(()), "arrival after return rearms eth even with unread frames");
    let ran = dev.step(&mut hal).expect("later arrival rearms eth");
    assert_eq!(ran.due, end + 1, "only the first arrival since the last start determines the new due time");
    assert_eq!(value(&c, &dev, "firings"), 3, "each run sees the arrivals since the previous run started");

    // A timer holds the CPU while two differently timed frames arrive. The
    // first frame's due time beats a later timer even though all start late.
    let c = compiled("var x: i32 = 0\nlet busy = after(0, fn() { for i in 0..20 { x = x + i } })\nlet later = after(1, fn() {})\nlet network = eth(2, fn() {})");
    let mut dev = Device::new(module::load(&c.module, &profile).expect("busy fixture loads"), &profile);
    dev.boot(&mut hal);
    let busy = dev.step(&mut hal).expect("busy timer runs first");
    assert!(busy.ops * u64::from(profile.op_ns) > 1000, "busy timer spans both arrivals and the later timer");
    for t in [100, 200] {
        dev.run_until(t, &mut hal);
        assert_eq!(dev.eth_in(t, &[0; 14]), Ok(()), "arrival at {t} queues while the CPU is busy");
    }
    let ran = dev.step(&mut hal).expect("earlier eth beats later timer");
    assert_eq!((ran.due, ran.timer), (100, None), "eth competes by first unseen arrival time rather than dispatch order");
    assert_eq!(dev.step(&mut hal).expect("later timer remains pending").timer, Some(1), "later timer follows eth");
    assert_eq!(dev.next_due(), None, "two differently timed arrivals coalesce into one eth firing");
}

#[test]
fn faults_do_no_io_and_receive_keeps_its_frame() {
    let source = "\
let b = bytes(1514)
var bad: i32 = 99
var received: i32 = 0
var sent: i32 = 0
fn setup() {
    bad = eth_send(b, 0, -1)
    bad = bad + eth_send(b, 0, 0)
    bad = bad + eth_send(b, 0, 13)
    bad = bad + eth_send(b, 0, 1515)
    bad = bad + eth_send(b, 0, 2147483647)
    bad = bad + eth_send(b, -1, 14)
    bad = bad + eth_send(b, 2147483647, 14)
    bad = bad + eth_send(b, 1501, 14)
    eth_mac(b, -1)
    eth_mac(b, 2147483647)
    eth_mac(b, 1509)
    eth_mac(b, 1508)
    sent = eth_send(b, 1500, 14)
}
let network = eth(1, fn() {
    for i in 0..2 { bad = bad + eth_recv(b, -1) }
    bad = bad + eth_recv(b, 1)
    bad = bad + eth_recv(b, 2147483647)
    received = eth_recv(b, 0)
    sent = sent + eth_send(b, 0, received)
})
";
    let c = compiled(source);
    let profile = Profile { op_ns: 10, ..P };
    let mut dev = Device::new(module::load(&c.module, &profile).expect("range fixture loads"), &profile);
    let mut hal = Recording::default();
    let mut core = Core::new(&c);
    let ran = dev.boot(&mut hal).expect("faulting setup continues");
    core.run(&c, ran, &dev, &hal);
    assert_eq!(hal.faults.len(), 11, "each bad send length and send/MAC range faults");
    assert_eq!(hal.sends.len(), 1, "invalid sends do not reach the HAL");
    assert_eq!(hal.macs.len(), 1, "invalid MAC ranges do not reach the HAL");
    assert_eq!(&hal.sends[0].1[8..], &MAC, "MAC and send accept ranges ending exactly at buffer end");
    assert_eq!(value(&c, &dev, "bad"), 0, "all refused sends explicitly return zero");
    assert!(hal.times.iter().all(|&t| t < ran.ops * u64::from(profile.op_ns)), "setup HAL calls occur inside the handler");
    assert!(hal.times.windows(2).all(|w| w[0] < w[1]), "fault and HAL times advance in port-call order");
    let t = ran.ops * u64::from(profile.op_ns) + 1;
    dev.run_until(t, &mut hal);
    let bytes = frame(watch::ETH_MAX, 0x80);
    assert_eq!(dev.eth_in(t, &bytes), Ok(()), "one frame queues before the range faults");
    core.pending.push_back(bytes.clone());
    let ran = dev.step(&mut hal).expect("faulting receive handler continues");
    core.run(&c, ran, &dev, &hal);
    assert_eq!(hal.faults.len(), 15, "all four bad receives fault, including repeat calls at one site");
    assert_eq!(hal.faults[11], hal.faults[12], "Ethernet faults are reported on each call, without copy/fill suppression");
    assert_eq!(hal.sends.last().expect("valid echo reaches HAL").1, bytes, "invalid receives retain the exact queued frame");
    assert_eq!((value(&c, &dev, "bad"), value(&c, &dev, "received")), (0, 1514), "bad receives return zero and the following good receive returns the maximum length");
    for fault in &hal.faults {
        let (site, name) = match *fault {
            Fault::EthLength { site, .. } => (site, "eth_send"),
            Fault::EthRange { port, site, .. } => (site, ports::PORTS[port as usize].name),
            other => panic!("Ethernet faults have their own names: {other:?}"),
        };
        let at = c.sites[site as usize];
        assert!(source[at.start as usize..at.end as usize].starts_with(name), "{name}: fault identifies the source call");
    }
    let next = ran.start + ran.ops * u64::from(profile.op_ns) + 1;
    dev.run_until(next, &mut hal);
    assert_eq!(dev.eth_in(next, &[0; 14]), Ok(()), "successful receive frees the slot after range refusals");
}

fn encoded(c: &Compiled) -> Vec<u32> { encode::encode(&c.lowered, P.id).expect("mutated fixture encodes") }

#[test]
fn declarations_buffers_and_tables() {
    for (form, name) in [(ports::FORM_ETH, "eth"), (ports::FORM_MOTION, "motion"), (ports::FORM_TOUCH, "touch"),
        (ports::FORM_NET, "net"), (ports::FORM_SPEAKER, "speaker"), (ports::FORM_MIC, "mic")] {
        assert_eq!(FORMS[form as usize].name, name, "the front end numbers `{name}` as the runtime does");
    }
    assert_eq!((watch::FAULT_RANGE, watch::FAULT_ETH_LENGTH, watch::FAULT_ETH_RANGE), (10, 11, 12), "Ethernet fault codes follow the existing range fault");
    assert_eq!(FORMS.len(), ports::FORMS as usize, "language and runtime have the same form count");
    let form = &FORMS[ports::FORM_ETH as usize];
    assert_eq!((form.name, form.handle, form.class, form.buffer), ("eth", DeclHandle::Ordinal, 6, None), "eth has its own ordinal class and runtime-owned buffers");
    assert_eq!(PORTS.len(), ports::PORTS.len(), "language and runtime have the same port count");
    for (front, back) in PORTS.iter().zip(ports::PORTS) {
        let ty = |t: &ir::Ty| match t { ir::Ty::F32 => ports::TY_F32, ir::Ty::I32 => ports::TY_I32, ir::Ty::Bool => ports::TY_BOOL, ir::Ty::F64 => u8::MAX };
        assert_eq!(front.name, back.name, "{}: port name matches", front.name);
        assert_eq!(front.params.iter().map(ty).collect::<Vec<_>>(), back.params, "{}: parameter types match", front.name);
        assert_eq!(front.rets.iter().map(ty).collect::<Vec<_>>(), back.rets, "{}: result types match", front.name);
    }
    for (port, number, name, writes) in [(ports::ETH_SEND, 15, "eth_send", false), (ports::ETH_RECV, 16, "eth_recv", true), (ports::ETH_MAC, 17, "eth_mac", true)] {
        assert_eq!(port, number, "{name}: stable port number");
        assert_eq!(PORTS[port as usize].bufs, &[ir::PortBuf { param: 0, elem: Some(ir::Elem::U8), writes }], "{name}: U8 buffer access contract");
        let call = if port == ports::ETH_SEND { "eth_send(b, off, 14)" } else if port == ports::ETH_RECV { "eth_recv(b, off)" } else { "eth_mac(b, off)" };
        let source = format!("let b = bytes(1514)\nvar off: i32 = 0\nfn setup() {{ {call} }}");
        let errors = compile(&source, &P, &[], &[]).err().expect("eth ports require a declaration");
        assert!(errors[0].message.contains(name) && errors[0].message.contains("needs an eth declaration"), "{name}: undeclared compile error names the port and declaration");
        assert_eq!(&source[errors[0].start..errors[0].end], call, "{name}: undeclared error points to the exact call");
        let with_decl = format!("{source}\nlet network = eth(1, fn() {{}})");
        let mut c = compiled(&with_decl);
        let eth = c.lowered.decls.pop().expect("last declaration is eth");
        let words = encoded(&c);
        assert!(matches!(module::load(&words, &P), Err(LoadError::EthUndeclared { port: p, .. }) if p == u32::from(port)), "{name}: the loader independently rejects undeclared calls");
        c.lowered.decls.push(eth);
        c.lowered.decls[0].form = ports::FORM_SAMPLES as u16;
        c.lowered.buffers[0].elem = ir::Elem::I16;
        let words = encoded(&c);
        assert!(matches!(module::load(&words, &P), Err(LoadError::BufferArgument { port: p, param: 0, .. }) if p == u32::from(port)), "{name}: loader rejects a non-U8 buffer");
        let wrong_kind = with_decl.replace("bytes(1514)", "samples(1514)");
        assert!(compile(&wrong_kind, &P, &[], &[]).is_err(), "{name}: compiler rejects a non-U8 buffer");
        let dynamic = with_decl.replace(call, &call.replacen("(b,", "(off,", 1));
        assert!(compile(&dynamic, &P, &[], &[]).is_err(), "{name}: compiler rejects a dynamic buffer handle");
        let mut c = compiled(&with_decl);
        for entry in &mut c.lowered.entries {
            for statement in &mut entry.program.body {
                if let ir::Stmt::Port { port: p, args, .. } = statement {
                    if *p == port { args[0] = args[1]; }
                }
            }
        }
        let words = encoded(&c);
        assert!(matches!(module::load(&words, &P), Err(LoadError::BufferArgument { port: p, param: 0, .. }) if p == u32::from(port)), "{name}: loader rejects a state-derived buffer handle");
    }
    for count in [-1, 0] {
        let source = format!("let network = eth({count}, fn() {{}})");
        let errors = compile(&source, &P, &[], &[]).err().expect("nonpositive eth count is refused");
        assert_eq!(errors[0].message, format!("eth needs at least one receive buffer; count is {count}"), "count {count}: compiler names the count requirement");
        assert_eq!(&source[errors[0].start..errors[0].end], &source[source.find("eth(").expect("eth declaration")..], "count {count}: compiler points to the eth declaration call");
        let mut c = compiled("let network = eth(1, fn() {})");
        c.lowered.decls[0].args[0] = DeclArg::Int(count);
        let words = encoded(&c);
        assert_eq!(module::load(&words, &P).expect_err("bad count fails loading"), LoadError::EthCount { decl: 0, count }, "count {count}: loader names EthCount");
    }
    let source = "let first = eth(1, fn() {})\nlet second = eth(1, fn() {})";
    let errors = compile(source, &P, &[], &[]).err().expect("second eth is refused");
    assert_eq!(errors[0].message, "at most one eth may be declared: the board has one USB link", "duplicate compile error explains the one-link rule");
    assert_eq!(errors[0].line_col(source).0, 2, "duplicate compile error points to the second declaration");
    let mut c = compiled("let network = eth(1, fn() {})");
    c.lowered.decls.push(c.lowered.decls[0].clone());
    let words = encoded(&c);
    assert_eq!(module::load(&words, &P).expect_err("duplicate fails loading"), LoadError::DuplicateEth { decl: 1 }, "loader refuses a duplicate eth before accepting its handle or handler");
    let c = compiled("let network = eth(2147483647, fn() {})");
    let dev = Device::new(module::load(&c.module, &P).expect("large count loads"), &P);
    assert_eq!(dev.next_due(), None, "the reference imposes no arbitrary receive-count cap or eager allocation");
    assert_eq!(dev.module().decls()[0].handle, 0, "eth handle is its ordinal");
    assert_eq!(c.module[H_PORT_VERSION], ports::PORT_TABLE_VERSION, "modules carry the current port table version");
    let mut old = c.module.clone();
    old[H_PORT_VERSION] = ports::PORT_TABLE_VERSION - 1;
    assert_eq!(module::load(&old, &P).expect_err("old port table is refused"), LoadError::PortVersion { found: ports::PORT_TABLE_VERSION - 1 }, "loader rejects the previous port table version");
}
