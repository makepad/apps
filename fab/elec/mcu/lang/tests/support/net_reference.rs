//! Shared reference client runner for the language and whole-image network tests.
use fab_elec_mcu::{compile, Profile};
use fab_elec_mcu_rt as rt;
use rt::module;
use rt::net::{self, code, Arrival, Cursor, Dir, Host, NetMsg, Network, Request, UpstreamLog, World};
use rt::sched::{Device, Fault, Hal, NetIn, Role};

pub(super) const P: Profile = Profile {
    id: 0, pins: 0, max_entry_ops: u32::MAX as u64, call_ops: None, ram_words: 1 << 16, buffer_bytes: 1 << 16,
    eeprom: 0, eeprom_write_ns: 0, f64: false, op_ns: 5, clock_hz: 240_000_000,
    analog: &rt::analog::NONE, pwm: &rt::pwm::NONE, serial: &rt::serial::ATMEGA328P, motion: None, touch: false, audio: None, display: None,
};

#[derive(Default)]
pub(super) struct Wire {
    pub(super) out: Vec<(u64, NetMsg)>,
    pub(super) probes: Vec<(u64, u8, u16)>,
    pub(super) faults: Vec<Fault>,
}

impl Hal for Wire {
    fn configure(&mut self, _: u64, _: u32, _: Role) { panic!("the client declares no pins") }
    fn serial(&mut self, _: u64, _: u32, _: rt::serial::SerialSetting) {}
    fn write(&mut self, _: u64, _: u32, _: bool, _: u32) { panic!("the client writes no pins") }
    fn shift(&mut self, _: u64, _: u32, _: u32, _: u32, _: u8, _: u32) { panic!("the client shifts nothing") }
    fn eeprom_read(&mut self, _: u64, _: u16, _: u32) -> u8 { panic!("no EEPROM") }
    fn eeprom_write(&mut self, _: u64, _: u16, _: u8, _: u32) { panic!("no EEPROM") }
    fn duty(&mut self, _: u64, _: u32, _: u8, _: u32) { panic!("no PWM") }
    fn read(&mut self, _: u64, _: u32, _: u32) -> bool { panic!("the client reads no pins") }
    fn adc(&mut self, _: u64, _: u32, _: u32) -> u16 { panic!("no ADC") }
    fn eth_send(&mut self, _: u64, _: &[u8], _: u32) -> bool { panic!("no eth") }
    fn eth_mac(&mut self, _: u64, _: u32) -> [u8; 6] { panic!("no eth") }
    fn clock(&mut self, t: u64, _: u32) -> u64 { t }
    fn fault(&mut self, _: u64, fault: Fault) { self.faults.push(fault) }
    fn probe(&mut self, t: u64, id: u8, value: u16) { self.probes.push((t, id, value)) }
    fn net(&mut self, t: u64, msg: &NetMsg) { self.out.push((t, msg.clone())) }
}

/// Through the codec and back, as the tether would carry it.
fn coded(msg: &NetMsg, dir: Dir) -> NetMsg {
    let mut payload = Vec::new();
    msg.encode(&mut payload);
    let back = NetMsg::decode(msg.kind(), dir, &payload).unwrap_or_else(|| panic!("{msg:?} decodes"));
    assert_eq!(&back, msg, "the codec carries {msg:?} unchanged");
    back
}

#[derive(Debug, Default, PartialEq)]
pub(super) struct Run {
    pub(super) probes: Vec<(u64, u8, u16)>,
    /// Every message in the order it was taken: (time, up, message).
    pub(super) wire: Vec<(u64, bool, NetMsg)>,
    /// Host messages the device ignored as crossed in flight.
    pub(super) stale: usize,
    pub(super) host_stale: u64,
    pub(super) faults: Vec<Fault>,
}

/// Runs `source` against `world` until `until` ns: the host first on a tie, so an arrival at t is
/// seen by a handler that starts at t.
pub(super) fn run(source: &str, world: World, until: u64) -> Run { run_on(source, world, until, None) }

/// The real network's stand-in for the logged runs: an echo server that answers at once (so its
/// answers are stamped just after the time the run had reached), taking each part as it comes and
/// echoing each whole message. What it was asked, in order.
#[derive(Default)]
pub(super) struct EchoNet {
    pub(super) asked: Vec<Request>,
    pub(super) open: Vec<(u8, u32)>,
    pub(super) partial: Vec<u8>,
    pub(super) answers: Vec<Arrival>,
    pub(super) closed_all: usize,
}

impl Network for EchoNet {
    fn send(&mut self, request: &Request) {
        self.asked.push(request.clone());
        match request {
            Request::Open { c, gen } => {
                self.open.push((*c, *gen));
                self.answers.push(Arrival::Opened { c: *c, gen: *gen });
            }
            Request::Send { c, gen, flags, bytes } => if self.open.contains(&(*c, *gen)) {
                self.answers.push(Arrival::Taken { c: *c, gen: *gen, bytes: bytes.len() as u32 });
                if flags & net::PART_FIRST != 0 { self.partial.clear(); }
                self.partial.extend_from_slice(bytes);
                if flags & net::PART_LAST != 0 {
                    let bytes = std::mem::take(&mut self.partial);
                    self.answers.push(Arrival::Message { c: *c, gen: *gen, binary: flags & net::PART_BINARY != 0, bytes });
                }
            },
            Request::Close { c, gen } => self.open.retain(|o| *o != (*c, *gen)),
        }
    }
    fn close_all(&mut self) {
        self.closed_all += 1;
        for (c, gen) in self.open.drain(..) { self.answers.push(Arrival::Ended { c, gen, code: code::RESET, detail: 0 }); }
    }
    fn answers(&mut self, out: &mut Vec<Arrival>) { out.append(&mut self.answers); }
}

/// A run through an upstream log: the owner holds the network, any other copy passes None.
pub(super) struct Logged<'a> {
    pub(super) log: &'a mut UpstreamLog,
    pub(super) network: Option<&'a mut EchoNet>,
}

/// Runs as `run` does, with a world's upstream endpoints reaching `logged`'s network through its
/// log: requests after every host call, logged arrivals before anything else at their time, the
/// network's answers logged after the newest time processed.
pub(super) fn run_on(source: &str, world: World, until: u64, mut logged: Option<Logged>) -> Run {
    let c = compile(source, &P, &[], &[]).unwrap_or_else(|e| panic!("the client compiles: {e:?}"));
    let m = module::load(&c.module, &P).expect("the client loads");
    let slots: Vec<u16> = m.net().iter().map(|d| d.slots as u16).collect();
    let mut dev = Device::new(m, &P);
    let mut hal = Wire::default();
    let mut host = Host::new(world, &slots);
    let mut out = Run::default();
    let mut at = Cursor::default();
    let mut done = 0;
    let exchange = |host: &mut Host, at: &mut Cursor, done: u64, logged: &mut Option<Logged>| if let Some(l) = logged {
        net::exchange(host, l.log, at, done, l.network.as_deref_mut().map(|n| n as &mut dyn Network));
    };
    host.hello(0);
    exchange(&mut host, &mut at, done, &mut logged);
    dev.boot(&mut hal);
    loop {
        for (t, msg) in hal.out.drain(..) {
            let msg = coded(&msg, Dir::Up);
            host.from_device(t, &msg).unwrap_or_else(|e| panic!("the host refused {msg:?} at {t}: {e:?}"));
            done = done.max(t);
            exchange(&mut host, &mut at, done, &mut logged);
            out.wire.push((t, true, msg));
        }
        if let Some(Logged { log, network: Some(network) }) = &mut logged {
            for arrival in network.answers.drain(..) { log.record(done, arrival); }
        }
        let a = logged.as_ref().and_then(|l| l.log.next_arrival(&at));
        let (d, h) = (dev.next_start(), host.next_due());
        let Some(next) = [a, d, h].into_iter().flatten().min().filter(|t| *t <= until) else { break };
        if a == Some(next) {
            let l = logged.as_mut().expect("an arrival comes from a log");
            net::deliver(&mut host, l.log, &mut at, done, next, l.network.as_deref_mut().map(|n| n as &mut dyn Network));
            done = done.max(next);
        } else if h == Some(next) {
            let polled = host.poll(next);
            done = done.max(next);
            exchange(&mut host, &mut at, done, &mut logged);
            let Some((t, msg)) = polled else { continue };
            let msg = coded(&msg, Dir::Down);
            match dev.net_in(t, &msg, &mut hal) {
                Ok(()) => {}
                Err(NetIn::Stale) => out.stale += 1,
                Err(e) => panic!("the device refused {msg:?} at {t}: {e:?}"),
            }
            out.wire.push((t, false, msg));
        } else {
            dev.step(&mut hal);
            done = done.max(next);
            exchange(&mut host, &mut at, done, &mut logged);
        }
    }
    out.probes = hal.probes;
    out.faults = hal.faults;
    out.host_stale = host.stale;
    out
}
