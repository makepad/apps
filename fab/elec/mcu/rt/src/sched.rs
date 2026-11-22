//! The scheduler: one logical CPU, cooperative. Events (timer expiries and
//! received bytes) run in device-time order; ties put timers first by number,
//! then bytes by link and arrival order, then Ethernet. Each handler runs to completion.
//! Device time is integer nanoseconds from boot.
//!
//! - `setup` runs at boot (t = 0), after pin roles and serial settings.
//! - `every(period, f)`: `f` runs at `k * period` (k = 1, 2, ..) on a grid
//!   from its start, never drifting with handler time.
//! - `after(delay, f)`: `f` runs once at `delay`; `start(timer)` arms it again.
//! - A handler starting at `t` makes its port calls at `t + ops * op_ns`
//!   (the profile's nominal op time; hardware-timed things are exact) and
//!   holds the CPU until `t + ops * op_ns`: an event due meanwhile runs late,
//!   at that end. A timer that came due more than once before it could run
//!   runs once and reports the missed firings as a fault (nothing is dropped
//!   silently).
//! - Each serial link holds bytes whose handlers have not started, up to its
//!   verified FIFO depth. Overflow drops the newest byte, preserving held
//!   bytes in order. Lost bytes are counted (saturating) and reported at the
//!   next handler's start on that link, then the count resets.
//! - Pin ports write outputs, read inputs/outputs, or set a PWM output's
//!   duty level at the call's instant; the HAL owns waveform updates.
//!   `adc_read` samples a declared analog input, clamped to the chip's range.
//!   `shift_out` sends 1..=32 bits MSB first on two declared outputs.
//! - A port call that names an undeclared pin, a pin of another role or an
//!   unknown timer does nothing and is reported as a fault with its site
//!   (the verifier refuses one whose pin is a constant), as is a loop that
//!   reached its cap. `byte(link)` returns this receive event's byte; outside
//!   that link's handler it returns -1 and faults at the call's site.
//! - Draws reach the panel in call order; the program cannot observe when.
//!   Refused draws fault at their call sites and are skipped; the handler continues.
//! - Writable buffers belong to the device and start zero at every boot;
//!   read-only image buffers retain the module's pixels.
//!   Copy/fill clamp bad ranges and report each site only once per boot.
//! - Ethernet arrivals coalesce. The handler is due again when a frame has
//!   arrived since its last run started: it may have read the queue before a
//!   frame that came during the run. Unread frames alone do not rearm it.
//! - Each `net` connection has its handler, due on any change of the link, its state, its data
//!   or its send credit (coalesced as Ethernet's is), after Ethernet in a tie. The ports layer
//!   (`crate::net` has the vocabulary) keeps the receive slots, the declared send queue, the epoch
//!   and the reopen backoff; it speaks `net::NetMsg` with the host end through `Hal::net` and
//!   `Device::net_in`.
//! - Motion and touch append repeating timers, in that order, after ordinary
//!   timers. Touch only invokes its handler when the latched report changes;
//!   a release keeps the previous coordinates. Ports only read these latches.
use crate::display::Display;
use crate::exec::{self, Exit, Observe, PortCall, Ports};
use crate::module::{buffer_dimensions, Buffer, Decl, DeclArg, Module};
use crate::net::{self, NetMsg};
use crate::ports::{self, FORM_AFTER, FORM_ANALOG, FORM_EVERY, FORM_INPUT, FORM_OUTPUT};
use crate::{pwm, serial, watch, Profile};
use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;

/// A pin's role, from its declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Unused,
    Output,
    /// `pull`: [`ports::PULL_NONE`], [`ports::PULL_UP`] or [`ports::PULL_DOWN`].
    Input { pull: i32 },
    /// The pin's level is 0 (low) from boot until the first `duty`.
    Pwm(pwm::PwmSetting),
    Analog,
}

/// Why an ordered draw was refused before any pixel was written.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawRefusal {
    /// The clipped destination has an odd start coordinate or size.
    OddWindow = 0,
    /// A negative size or source rectangle outside the source surface.
    SourceOutside = 1,
    /// An undeclared display or a display handle other than zero.
    Handle = 2,
}

/// What a firmware run reports beside its pin traffic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// A port call on a pin that is not declared for it.
    Pin { port: u16, pin: i32, site: u32 },
    /// A timer port call with no such timer.
    Timer { port: u16, timer: i32, site: u32 },
    /// A byte port outside its link's receive handler, or an undeclared RX link.
    Serial { port: u16, link: i32, site: u32 },
    /// Bytes lost since this link's previous report, with a saturating count.
    /// `at_least` marks hardware overrun uncertainty; the model never overruns
    /// and reports only exact FIFO drops, with `at_least: false`.
    SerialLost { link: u32, count: u32, at_least: bool },
    /// Timer `timer` came due `missed` more times before its handler could run.
    Overrun { timer: u32, missed: u32 },
    /// The loop at `site` reached its run-time cap and stopped.
    LoopCap { site: u32 },
    /// A kernel access was refused without changing memory or interrupt state.
    Kernel { kind: crate::kernel::KernelFault, site: u32 },
    /// A non-fatal kernel report; the export continues after reporting it.
    KernelReport { code: u8, site: u32, arg: i32 },
    /// The draw at `site` was refused and skipped; the handler continues.
    Draw { port: u16, site: u32, refusal: DrawRefusal },
    /// A copy/fill range was clamped (once per site per boot), or a motion
    /// axis or audio argument was out of range (reported on each invalid call).
    Range { site: u32, n: i32 },
    /// The Ethernet send length is outside ETH_MIN..=ETH_MAX; nothing is sent.
    EthLength { site: u32, n: i32 },
    /// The Ethernet buffer range is invalid; no I/O occurs and RX stays queued.
    EthRange { port: u16, site: u32, off: i32 },
    /// The shift bit count is outside 1..=32; nothing is driven. Reports every call.
    ShiftBits { site: u32, bits: i32 },
    /// The EEPROM address is outside the profile; no I/O occurs and a read returns 0.
    /// Reports every call as `watch::FAULT_EEPROM_ADDR`, with arg = addr.
    EepromAddr { site: u32, addr: i32 },
    /// A networking port named no declared connection; it did nothing and returned 0.
    NetHandle { port: u16, site: u32, c: i32 },
    /// A `net_send`/`net_recv` count was negative or its range left the buffer; nothing moved.
    NetRange { port: u16, site: u32, off: i32 },
    /// The HAL refused an audio port. Its count is zero and the reason names the missing path.
    AudioUnavailable { port: u16, site: u32, reason: &'static str },
}

/// Why an arriving Ethernet frame was refused without changing the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EthIn {
    /// The program has no eth declaration.
    Undeclared,
    /// The frame length is outside ETH_MIN..=ETH_MAX.
    Length,
    /// All declared receive buffers hold frames; the host exceeded its credits.
    Full,
}

/// Why the ports layer refused a message from the host end. Every one but `Stale` is a host bug.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetIn {
    /// No such connection is declared.
    Unknown,
    /// A message only the device sends.
    Direction,
    /// Meant for an attempt the device has already left (it crossed a close or an end in flight);
    /// ignored.
    Stale,
    /// A part with no free receive slot: the host exceeded its credits.
    Full,
    /// A part out of message order, empty, oversized, or with unknown flags.
    Order,
}

/// The chip side of the ports: pins at exact device times (nanoseconds).
pub trait Hal {
    /// Pin `pin` takes its declared role (at boot, before `setup`).
    fn configure(&mut self, t: u64, pin: u32, role: Role);
    /// Link `link` takes its baud setting after pin roles, before `setup`.
    fn serial(&mut self, t: u64, link: u32, setting: serial::SerialSetting);
    /// Drives output pin `pin` to `high` at `t`; `site` names the call.
    fn write(&mut self, t: u64, pin: u32, high: bool, site: u32);
    /// Shifts the low `bits` (1..=32) of `value` out on the declared outputs `data` and `clock`,
    /// as `ports::SHIFT_OUT` defines. A HAL that models time places the edges from `t` on.
    fn shift(&mut self, t: u64, data: u32, clock: u32, value: u32, bits: u8, site: u32);
    /// Reads a valid EEPROM address at `t`, as `ports::EEPROM_READ` defines.
    fn eeprom_read(&mut self, t: u64, addr: u16, site: u32) -> u8;
    /// Stores a byte at a valid EEPROM address at `t`, as `ports::EEPROM_WRITE` defines.
    fn eeprom_write(&mut self, t: u64, addr: u16, value: u8, site: u32);
    /// The PWM level from `t` on; `site` names the call. A HAL that models
    /// the waveform applies it at the setting's next update point (Fast:
    /// BOTTOM, PhaseCorrect: TOP; every PWM period's origin is t = 0).
    fn duty(&mut self, t: u64, pin: u32, level: u8, site: u32);
    /// The level of pin `pin` at `t`.
    fn read(&mut self, t: u64, pin: u32, site: u32) -> bool;
    /// The analog input `pin` sampled at `t`: 0..=max of the profile's analog table.
    fn adc(&mut self, t: u64, pin: u32, site: u32) -> u16;
    /// Queue one complete Ethernet frame at `t`; false means the TX side is full.
    fn eth_send(&mut self, t: u64, frame: &[u8], site: u32) -> bool;
    /// The board's six-byte MAC address, read at the call's instant.
    fn eth_mac(&mut self, t: u64, site: u32) -> [u8; 6];
    /// The device clock (ns since boot) at the instant `t`: a simulator
    /// answers `t`, a chip reads its own timer.
    fn clock(&mut self, t: u64, site: u32) -> u64;
    fn fault(&mut self, t: u64, fault: Fault);
    /// The program's `probe(id, value)` at `t`, as `ports::PROBE` defines; a HAL with no host to
    /// tell keeps it to itself.
    fn probe(&mut self, _t: u64, _id: u8, _value: u16) {}
    /// A networking message from the ports layer to the host end at `t`, as `crate::net` defines.
    /// A HAL with no host end answers nothing: the link stays none and every open connecting.
    fn net(&mut self, _t: u64, _msg: &NetMsg) {}
    /// Queue up to n samples without waiting. The verified range starts at sample off in
    /// packed I16 storage (even samples in the low 16 bits). Return the count taken.
    fn play(&mut self, _t: u64, _words: &[u32], _off: usize, _n: usize, _site: u32) -> Result<u32, &'static str> {
        Err("no speaker on this chip")
    }
    fn speaker_free(&mut self, _t: u64, _site: u32) -> Result<u32, &'static str> { Err("no speaker on this chip") }
    fn speaker_queued(&mut self, _t: u64, _site: u32) -> Result<u32, &'static str> { Err("no speaker on this chip") }
    fn speaker_on(&mut self, _t: u64, _on: bool, _site: u32) -> Result<(), &'static str> { Err("no speaker on this chip") }
    /// Copy up to n unread samples starting at off, packed as for play. Preserve the
    /// neighboring half-word on an odd start or end. Return the count copied.
    fn record(&mut self, _t: u64, _words: &mut [u32], _off: usize, _n: usize, _site: u32) -> Result<u32, &'static str> {
        Err("no mic on this chip")
    }
    fn mic_ready(&mut self, _t: u64, _site: u32) -> Result<u32, &'static str> { Err("no mic on this chip") }
    fn mic_rewind(&mut self, _t: u64, _n: u32, _site: u32) -> Result<u32, &'static str> { Err("no mic on this chip") }
}

#[derive(Clone, Copy, Debug)]
struct Timer {
    entry: u16,
    /// The period (`every`) or delay (`after`), ns.
    period: u64,
    repeat: bool,
    /// The next due time; None: stopped.
    due: Option<u64>,
}

struct Receiver {
    entry: u16,
    limit: u32,
    pending: VecDeque<(u64, u8)>,
    lost: u32,
}

struct Ethernet {
    entry: u16,
    limit: usize,
    pending: VecDeque<Vec<u8>>,
    /// First accepted arrival since the handler last returned. Keeping the
    /// edge directly is equivalent to received != seen, without counter wrap.
    due: Option<u64>,
    /// The start of the handler's last run. An arrival at or after it may
    /// have come after the handler read the queue, so it rearms the handler.
    seen_until: u64,
}

/// One message a connection's program is sending, as its send queue holds it.
struct OutMsg {
    binary: bool,
    /// Its first part has gone.
    started: bool,
    /// Its last send has been taken; until then its last queued byte stays back, so the part that
    /// ends it is never empty.
    ended: bool,
    bytes: VecDeque<u8>,
}

/// One declared connection's ports layer.
struct NetConn {
    entry: u16,
    slots: usize,
    send_cap: usize,
    /// The program opened it and has not closed it: the layer keeps reopening it.
    wanted: bool,
    state: u8,
    /// An OPEN went to the host and its one offline has not come back.
    live: bool,
    /// A CLOSE went and the offline answering it has not come back; a reopen waits for it.
    closing: bool,
    epoch: u32,
    error: u8,
    detail: u32,
    /// Received parts, each in a slot of its own, with their flags.
    recv: VecDeque<(u8, Vec<u8>)>,
    /// Bytes of the front part already read.
    read: usize,
    /// The kind of the message being read, between reading its first part and its last.
    reading: Option<bool>,
    /// A received message is between its first and last parts.
    rx_open: bool,
    send: VecDeque<OutMsg>,
    queued: usize,
    grant: u64,
    backoff: u64,
    retry: Option<u64>,
    due: Option<u64>,
    seen_until: u64,
}

impl NetConn {
    fn clear(&mut self) {
        self.recv.clear();
        self.read = 0;
        self.reading = None;
        self.rx_open = false;
        self.send.clear();
        self.queued = 0;
        self.grant = 0;
    }

    fn reset(&mut self) {
        self.clear();
        (self.wanted, self.state, self.live, self.closing) = (false, net::OFFLINE, false, false);
        (self.epoch, self.error, self.detail) = (0, net::code::NONE, 0);
        (self.backoff, self.retry, self.due, self.seen_until) = (net::RETRY_FIRST_NS, None, None, 0);
    }

    /// A change at `t`: the handler is due, unless it already is or ran after the change.
    fn mark(&mut self, t: u64) {
        if t >= self.seen_until && self.due.is_none() { self.due = Some(t); }
    }

    fn open(&mut self, c: u8, t: u64, hal: &mut dyn Hal) -> u32 {
        if self.wanted { return 0; }
        self.wanted = true;
        self.backoff = net::RETRY_FIRST_NS;
        self.retry = None;
        self.state = net::CONNECTING;
        if !self.closing {
            self.live = true;
            hal.net(t, &NetMsg::Open { c });
        }
        1
    }

    fn close(&mut self, c: u8, t: u64, hal: &mut dyn Hal) -> u32 {
        if !self.wanted { return 0; }
        self.wanted = false;
        self.retry = None;
        if self.state == net::READY { self.epoch = self.epoch.wrapping_add(1); }
        if self.live {
            self.live = false;
            self.closing = true;
            hal.net(t, &NetMsg::Close { c });
        }
        (self.state, self.error, self.detail) = (net::OFFLINE, net::code::NONE, 0);
        self.clear();
        1
    }

    /// The reopen after a backoff, when nothing has changed the connection meanwhile.
    fn reopen(&mut self, c: u8, t: u64, hal: &mut dyn Hal) {
        self.retry = None;
        if self.wanted && self.state == net::OFFLINE && !self.live && !self.closing {
            self.state = net::CONNECTING;
            self.live = true;
            hal.net(t, &NetMsg::Open { c });
            self.mark(t);
        }
    }

    /// Sends what the grant allows, part by part, in message order.
    fn drain(&mut self, c: u8, t: u64, hal: &mut dyn Hal) {
        while self.grant > 0 {
            let Some(m) = self.send.front_mut() else { break };
            let avail = m.bytes.len();
            let sendable = if m.ended { avail } else { avail.saturating_sub(1) };
            let n = sendable.min(net::NET_PART).min(self.grant.min(usize::MAX as u64) as usize);
            if n == 0 { break; }
            let last = m.ended && n == avail;
            let flags = u8::from(m.binary) * net::PART_BINARY
                | if m.started { 0 } else { net::PART_FIRST }
                | if last { net::PART_LAST } else { 0 };
            let bytes = m.bytes.drain(..n).collect();
            m.started = true;
            self.queued -= n;
            self.grant -= n as u64;
            if last { self.send.pop_front(); }
            hal.net(t, &NetMsg::Data { c, flags, bytes });
        }
    }

    /// `net_send`'s taking: see `ports::NET_SEND`.
    fn send(&mut self, c: u8, t: u64, hal: &mut dyn Hal, data: &[u8], flags: i32) -> u32 {
        if self.state != net::READY { return 0; }
        let more = flags & net::SEND_MORE != 0;
        let open = self.send.back().is_some_and(|m| !m.ended);
        if data.is_empty() {
            if !more && open {
                if let Some(m) = self.send.back_mut() { m.ended = true; }
                self.drain(c, t, hal);
            }
            return 0;
        }
        let take = data.len().min(self.send_cap - self.queued);
        if take == 0 { return 0; }
        if !open {
            self.send.push_back(OutMsg { binary: flags & net::SEND_BINARY != 0, started: false, ended: false, bytes: VecDeque::new() });
        }
        let Some(m) = self.send.back_mut() else { return 0 };
        m.bytes.extend(&data[..take]);
        m.ended = take == data.len() && !more;
        self.queued += take;
        self.drain(c, t, hal);
        take as u32
    }

    /// `net_recv`'s reading into `out`: see `ports::NET_RECV`.
    fn recv(&mut self, c: u8, t: u64, hal: &mut dyn Hal, out: &mut Vec<u8>, cap: usize) {
        let mut freed = 0u16;
        while out.len() < cap {
            let Some((flags, bytes)) = self.recv.front() else { break };
            let flags = *flags;
            self.reading.get_or_insert(flags & net::PART_BINARY != 0);
            let n = (cap - out.len()).min(bytes.len() - self.read);
            out.extend_from_slice(&bytes[self.read..self.read + n]);
            self.read += n;
            if self.read < bytes.len() { break; }
            self.recv.pop_front();
            self.read = 0;
            freed += 1;
            if flags & net::PART_LAST != 0 {
                self.reading = None;
                break;
            }
        }
        if freed > 0 { hal.net(t, &NetMsg::Freed { c, slots: freed }); }
    }

    fn left(&self) -> u32 {
        if self.recv.is_empty() && self.reading.is_none() { return 0; }
        let mut left = 0usize;
        for (k, (flags, bytes)) in self.recv.iter().enumerate() {
            left += bytes.len() - if k == 0 { self.read } else { 0 };
            if flags & net::PART_LAST != 0 { return left as u32; }
        }
        left as u32 + 1
    }

    fn kind(&self) -> u32 {
        let binary = self.reading.or_else(|| self.recv.front().map(|(flags, _)| flags & net::PART_BINARY != 0));
        u32::from(binary == Some(true))
    }
}

struct Net {
    link: (u8, u8, u32),
    conns: Vec<NetConn>,
}

#[derive(Clone, Copy)]
enum Event { Timer(u32), Rx(u32), Eth, Net(u32), NetRetry(u32) }

/// One processed event, as [`Device::step`] reports it. An unchanged touch
/// has zero ops and does not execute its handler, nor does a connection's
/// reopen after its backoff (the ports layer's own step).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ran {
    pub entry: u16,
    /// The timer whose event it was (None: a byte, Ethernet or `setup`).
    pub timer: Option<u32>,
    /// The receive event's link and byte (None: a timer, Ethernet or `setup`).
    pub rx: Option<(u32, u8)>,
    /// When it was due and when it started (later when the CPU was busy).
    pub due: u64,
    pub start: u64,
    pub ops: u64,
}

/// A loaded module running on a chip (real or simulated).
pub struct Device<'m, O: Observe = ()> {
    observer: O,
    module: Module<'m>,
    op_ns: u64,
    state: Vec<u32>,
    scratch: Vec<u32>,
    buffers: Vec<Vec<u32>>,
    range_faults: Vec<u32>,
    roles: Vec<Role>,
    analog_max: i32,
    eeprom: u32,
    timers: Vec<Timer>,
    motion_timer: Option<usize>,
    motion_input: [i16; 6],
    motion_sample: [i16; 6],
    touch_timer: Option<usize>,
    touch_input: TouchReport,
    touch_report: TouchReport,
    receivers: Vec<Receiver>,
    eth: Option<Ethernet>,
    net: Option<Net>,
    display: Option<Display>,
    /// The CPU is busy until then (the end of the last handler).
    busy_until: u64,
}

const NS_PER_US: u64 = 1000;

/// A single-finger report in visible-glass pixels, origin top-left, with axes
/// aligned with the display. A release retains the last delivered coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TouchReport {
    pub down: bool,
    pub x: u16,
    pub y: u16,
}

impl<'m> Device<'m> {
    /// The device for a verified module (state at its initial values;
    /// nothing runs before [`Device::boot`]).
    pub fn new(module: Module<'m>, profile: &Profile) -> Device<'m> {
        Device::with_observer(module, profile, ())
    }
}

impl<'m, O: Observe> Device<'m, O> {
    /// Constructs a device whose executed definitions and writes go to `observer`.
    pub fn with_observer(module: Module<'m>, profile: &Profile, observer: O) -> Self {
        let scratch = (0..module.entries().len()).map(|k| module.scratch_words(k)).max().unwrap_or(0);
        let mut roles = vec![Role::Unused; profile.pins as usize];
        let mut timers = Vec::new();
        let mut eth = None;
        for d in module.decls() {
            let int = |k: usize| match d.args.get(k) {
                Some(DeclArg::Int(x)) => *x,
                _ => 0,
            };
            let entry = |k: usize| match d.args.get(k) {
                Some(DeclArg::Entry(e)) => *e,
                _ => 0,
            };
            match d.form {
                ports::FORM_ETH => eth = Some(Ethernet {
                    entry: entry(1), limit: int(0) as usize, pending: VecDeque::new(),
                    due: None, seen_until: 0,
                }),
                FORM_OUTPUT | FORM_INPUT | FORM_ANALOG => {
                    let role = match d.form {
                        FORM_OUTPUT => Role::Output,
                        FORM_ANALOG => Role::Analog,
                        _ => Role::Input { pull: int(1) },
                    };
                    if let Some(r) = roles.get_mut(int(0) as usize) {
                        *r = role;
                    }
                }
                FORM_EVERY | FORM_AFTER => {
                    let period = (int(0).max(0) as u64) * NS_PER_US;
                    timers.push(Timer { entry: entry(1), period, repeat: d.form == FORM_EVERY, due: None });
                }
                _ => {}
            }
        }
        for output in module.pwm() {
            if let Some(role) = roles.get_mut(output.pin as usize) {
                *role = Role::Pwm(output.setting);
            }
        }
        // Append motion after ordinary timers so their public handles stay
        // unchanged. Its scheduling and coalescing use the ordinary path.
        let motion_timer = module.decls().iter().find(|d| d.form == ports::FORM_MOTION).map(|d| {
            let [DeclArg::Int(us), DeclArg::Entry(entry)] = d.args.as_slice() else { unreachable!("verified motion") };
            let index = timers.len();
            timers.push(Timer { entry: *entry, period: *us as u64 * NS_PER_US, repeat: true, due: None });
            index
        });
        let touch_timer = module.decls().iter().find(|d| d.form == ports::FORM_TOUCH).map(|d| {
            let [DeclArg::Int(us), DeclArg::Entry(entry)] = d.args.as_slice() else { unreachable!("verified touch") };
            let index = timers.len();
            timers.push(Timer { entry: *entry, period: *us as u64 * NS_PER_US, repeat: true, due: None });
            index
        });
        let receivers = module.serial().iter().map(|s| Receiver {
            entry: s.entry, limit: s.fifo, pending: VecDeque::with_capacity(s.fifo as usize), lost: 0,
        }).collect();
        let display = module.display().map(Display::new);
        let conns: Vec<NetConn> = module.net().iter().map(|d| {
            let mut conn = NetConn {
                entry: d.entry, slots: d.slots as usize, send_cap: d.send_bytes as usize,
                wanted: false, state: net::OFFLINE, live: false, closing: false, epoch: 0, error: 0, detail: 0,
                recv: VecDeque::new(), read: 0, reading: None, rx_open: false, send: VecDeque::new(), queued: 0,
                grant: 0, backoff: net::RETRY_FIRST_NS, retry: None, due: None, seen_until: 0,
            };
            conn.reset();
            conn
        }).collect();
        let net = (!conns.is_empty()).then(|| Net { link: (net::LINK_NONE, net::code::NONE, 0), conns });
        let buffers = module.buffers().iter().map(|b| {
            let mut words = vec![0; b.words()];
            let decl = &module.decls()[b.decl as usize];
            if let (ports::FORM_IMAGE, [DeclArg::Int(index), ..]) = (decl.form, decl.args.as_slice()) {
                let image = &module.images()[*index as usize];
                for k in 0..b.len {
                    let pixel = image.pixel(k % image.width, k / image.width).expect("verified image extent");
                    words[k as usize / 2] |= u32::from(pixel) << (16 * (k % 2));
                }
            }
            words
        }).collect();
        let range_faults = vec![0; if module.buffers().is_empty() { 0 } else { module.n_sites().div_ceil(32) as usize }];
        Device { observer, state: module.state_init().to_vec(), scratch: vec![0; scratch], buffers, range_faults, module, op_ns: profile.op_ns as u64, roles, analog_max: profile.analog.max(), eeprom: profile.eeprom, timers, motion_timer, motion_input: [0; 6], motion_sample: [0; 6], touch_timer, touch_input: TouchReport::default(), touch_report: TouchReport::default(), receivers, eth, net, display, busy_until: 0 }
    }

    pub fn observer(&self) -> &O { &self.observer }

    pub fn module(&self) -> &Module<'m> {
        &self.module
    }

    /// The state words (the program's `var`s, laid out as the module's VARS
    /// table says).
    pub fn state(&self) -> &[u32] {
        &self.state
    }

    /// Whole-word buffer allocations in buffer-number order. Narrow elements
    /// are packed little endian; padding after the last element stays zero.
    pub fn buffers(&self) -> &[Vec<u32>] { &self.buffers }

    /// Reference panel pixels in row-major RGB565 order; None without a
    /// display declaration. Firmware cannot read the framebuffer or draw timing.
    pub fn framebuffer(&self) -> Option<&[u16]> {
        self.display.as_ref().map(Display::pixels)
    }

    pub fn role(&self, pin: u32) -> Role {
        self.roles.get(pin as usize).copied().unwrap_or(Role::Unused)
    }

    /// Boot at t = 0: pins take their roles, serial links take their settings,
    /// writable buffers and range-fault bits clear, timers start, then `setup` runs
    /// (when the program has one).
    pub fn boot(&mut self, hal: &mut dyn Hal) -> Option<Ran> {
        for (buffer, row) in self.buffers.iter_mut().zip(self.module.buffers()) {
            if row.writable { buffer.fill(0); }
        }
        self.range_faults.fill(0);
        self.motion_input = [0; 6];
        self.motion_sample = [0; 6];
        self.touch_input = TouchReport::default();
        self.touch_report = TouchReport::default();
        if let Some(eth) = &mut self.eth {
            eth.pending.clear();
            eth.due = None;
            eth.seen_until = 0;
        }
        if let Some(net) = &mut self.net {
            net.link = (net::LINK_NONE, net::code::NONE, 0);
            net.conns.iter_mut().for_each(NetConn::reset);
        }
        for (pin, role) in self.roles.iter().enumerate() {
            if *role != Role::Unused {
                hal.configure(0, pin as u32, *role);
            }
        }
        for output in self.module.serial() {
            hal.serial(0, output.link, output.setting);
        }
        for t in &mut self.timers {
            t.due = Some(t.period);
        }
        let setup = self.module.entry(crate::SETUP)?;
        Some(self.run(setup as u16, None, None, 0, 0, hal))
    }

    /// Enqueue a byte whose reception completed at `t` nanoseconds. The
    /// caller has run the device up to `t` (`run_until(t)`) and pushes each
    /// link's bytes in time order. The FIFO counts bytes whose handlers have
    /// not started; when full, the new byte is dropped and counted. A link
    /// without a serial declaration faults at `t`, with no source site.
    pub fn rx(&mut self, t: u64, link: u32, byte: u8, hal: &mut dyn Hal) {
        let Some(receiver) = self.receivers.get_mut(link as usize) else {
            hal.fault(t, Fault::Serial { port: ports::SERIAL_BYTE, link: link as i32, site: u32::MAX });
            return;
        };
        if receiver.pending.len() >= receiver.limit as usize {
            receiver.lost = receiver.lost.saturating_add(1);
        } else {
            receiver.pending.push_back((t, byte));
        }
    }

    /// Enqueue a frame arriving at `t` nanoseconds. As with `rx`, the caller
    /// runs the device up to `t` first and supplies arrivals in time order.
    /// A refusal changes nothing; the caller logs Full as a credit violation.
    pub fn eth_in(&mut self, t: u64, frame: &[u8]) -> Result<(), EthIn> {
        let eth = self.eth.as_mut().ok_or(EthIn::Undeclared)?;
        if !(watch::ETH_MIN..=watch::ETH_MAX).contains(&frame.len()) { return Err(EthIn::Length); }
        if eth.pending.len() == eth.limit { return Err(EthIn::Full); }
        eth.pending.push_back(frame.to_vec());
        if t >= eth.seen_until && eth.due.is_none() { eth.due = Some(t); }
        Ok(())
    }

    /// A networking message from the host end arriving at `t`. As with `rx`, the caller runs the
    /// device up to `t` first and supplies arrivals in time order. A refusal changes nothing; every
    /// refusal but `Stale` is a host bug.
    pub fn net_in(&mut self, t: u64, msg: &NetMsg, hal: &mut dyn Hal) -> Result<(), NetIn> {
        let Some(net) = self.net.as_mut() else { return Err(NetIn::Unknown) };
        if let NetMsg::Link { state, code, detail } = *msg {
            net.link = (state, code, detail);
            net.conns.iter_mut().for_each(|conn| conn.mark(t));
            return Ok(());
        }
        let c = msg.conn().ok_or(NetIn::Direction)?;
        let conn = net.conns.get_mut(c as usize).ok_or(NetIn::Unknown)?;
        match msg {
            NetMsg::State { state, code, detail, .. } => {
                if conn.closing {
                    if *state != net::OFFLINE { return Err(NetIn::Stale); }
                    conn.closing = false;
                    if conn.wanted {
                        conn.live = true;
                        hal.net(t, &NetMsg::Open { c });
                    }
                    return Ok(());
                }
                if !conn.live { return Err(NetIn::Stale); }
                match *state {
                    net::READY => if conn.state != net::READY {
                        conn.clear();
                        conn.state = net::READY;
                        conn.backoff = net::RETRY_FIRST_NS;
                        conn.mark(t);
                    },
                    net::CONNECTING => {}
                    _ => {
                        conn.live = false;
                        if conn.state == net::READY { conn.epoch = conn.epoch.wrapping_add(1); }
                        conn.clear();
                        (conn.state, conn.error, conn.detail) = (net::OFFLINE, *code, *detail);
                        if conn.wanted {
                            conn.retry = Some(t.saturating_add(conn.backoff));
                            conn.backoff = (conn.backoff * 2).min(net::RETRY_MAX_NS);
                        }
                        conn.mark(t);
                    }
                }
            }
            NetMsg::Data { flags, bytes, .. } => {
                if conn.closing || !conn.live || conn.state != net::READY { return Err(NetIn::Stale); }
                let known = net::PART_BINARY | net::PART_FIRST | net::PART_LAST;
                if flags & !known != 0 || !(1..=net::NET_PART).contains(&bytes.len()) || (flags & net::PART_FIRST != 0) == conn.rx_open {
                    return Err(NetIn::Order);
                }
                if conn.recv.len() == conn.slots { return Err(NetIn::Full); }
                conn.rx_open = flags & net::PART_LAST == 0;
                conn.recv.push_back((*flags, bytes.clone()));
                conn.mark(t);
            }
            NetMsg::Granted { bytes, .. } => {
                if conn.closing || !conn.live || conn.state != net::READY { return Err(NetIn::Stale); }
                conn.grant += u64::from(*bytes);
                conn.drain(c, t, hal);
                conn.mark(t);
            }
            NetMsg::Open { .. } | NetMsg::Close { .. } | NetMsg::Freed { .. } | NetMsg::Link { .. } => return Err(NetIn::Direction),
        }
        Ok(())
    }

    /// Supply raw AX/AY/AZ/GX/GY/GZ counts before the next motion event.
    /// The host owns sensor startup, ODR and transfer timing; the reference
    /// latches this value when that event starts. No new input repeats it.
    /// Returns false when the module has no motion declaration.
    pub fn motion_in(&mut self, sample: [i16; 6]) -> bool {
        if self.motion_timer.is_none() { return false; }
        self.motion_input = sample;
        true
    }

    /// Supply the next touch report. The host owns startup, scan and transfer
    /// timing, as for motion_in. An event latches it and invokes the handler
    /// only when down changes, or coordinates change while down. No input
    /// repeats the last report. Returns false without a touch declaration.
    pub fn touch_in(&mut self, report: TouchReport) -> bool {
        if self.touch_timer.is_none() { return false; }
        self.touch_input = report;
        true
    }

    /// When the next event is due (None: nothing is armed).
    pub fn next_due(&self) -> Option<u64> {
        self.next_event().map(|(_, due)| due)
    }

    /// When the next handler would start: its due time, or the end of the
    /// running one when the CPU is still busy then.
    pub fn next_start(&self) -> Option<u64> {
        self.next_due().map(|due| due.max(self.busy_until))
    }

    fn next_event(&self) -> Option<(Event, u64)> {
        let mut best: Option<(Event, u64)> = None;
        for (k, t) in self.timers.iter().enumerate() {
            if let Some(due) = t.due {
                if best.is_none_or(|(_, b)| due < b) {
                    best = Some((Event::Timer(k as u32), due));
                }
            }
        }
        for (link, receiver) in self.receivers.iter().enumerate() {
            if let Some(&(due, _)) = receiver.pending.front() {
                if best.is_none_or(|(_, b)| due < b) {
                    best = Some((Event::Rx(link as u32), due));
                }
            }
        }
        if let Some(due) = self.eth.as_ref().and_then(|eth| eth.due) {
            if best.is_none_or(|(_, b)| due < b) { best = Some((Event::Eth, due)); }
        }
        for (c, conn) in self.net.iter().flat_map(|net| net.conns.iter().enumerate()) {
            if let Some(due) = conn.retry {
                if best.is_none_or(|(_, b)| due < b) { best = Some((Event::NetRetry(c as u32), due)); }
            }
            if let Some(due) = conn.due {
                if best.is_none_or(|(_, b)| due < b) { best = Some((Event::Net(c as u32), due)); }
            }
        }
        best
    }

    /// Processes the next event. An unchanged touch consumes its timer event
    /// and returns a zero-op Ran without executing the handler.
    pub fn step(&mut self, hal: &mut dyn Hal) -> Option<Ran> {
        let (event, due) = self.next_event()?;
        let start = due.max(self.busy_until);
        let k = match event {
            Event::Eth => {
                let entry = self.eth.as_ref()?.entry;
                let ran = self.run(entry, None, None, due, start, hal);
                let eth = self.eth.as_mut()?;
                eth.due = None;
                eth.seen_until = start;
                return Some(ran);
            }
            Event::Net(c) => {
                let entry = self.net.as_ref()?.conns.get(c as usize)?.entry;
                let ran = self.run(entry, None, None, due, start, hal);
                let conn = self.net.as_mut()?.conns.get_mut(c as usize)?;
                conn.due = None;
                conn.seen_until = start;
                return Some(ran);
            }
            Event::NetRetry(c) => {
                // The ports layer's own step between handlers: no program runs for it (zero ops),
                // and its state change makes the handler due at once.
                let conn = self.net.as_mut()?.conns.get_mut(c as usize)?;
                conn.reopen(c as u8, start, hal);
                self.busy_until = start;
                return Some(Ran { entry: conn.entry, timer: None, rx: None, due, start, ops: 0 });
            }
            Event::Timer(k) => k,
            Event::Rx(link) => {
                let receiver = self.receivers.get_mut(link as usize)?;
                let (_, byte) = receiver.pending.pop_front()?;
                let entry = receiver.entry;
                if receiver.lost > 0 {
                    hal.fault(start, Fault::SerialLost { link, count: receiver.lost, at_least: false });
                    receiver.lost = 0;
                }
                return Some(self.run(entry, None, Some((link, byte)), due, start, hal));
            }
        };
        let t = *self.timers.get(k as usize)?;
        // The next firing: on the grid, past the start; firings the CPU was
        // too busy to run coalesce into this one.
        let mut next = due.saturating_add(t.period);
        let mut missed = 0u32;
        if t.repeat && t.period > 0 {
            while next <= start {
                next = next.saturating_add(t.period);
                missed = missed.saturating_add(1);
            }
        }
        self.timers.get_mut(k as usize)?.due = if t.repeat { Some(next) } else { None };
        if missed > 0 {
            hal.fault(start, Fault::Overrun { timer: k, missed });
        }
        if self.motion_timer == Some(k as usize) { self.motion_sample = self.motion_input; }
        if self.touch_timer == Some(k as usize) {
            let mut report = self.touch_input;
            if !report.down {
                report.x = self.touch_report.x;
                report.y = self.touch_report.y;
            }
            if report == self.touch_report {
                self.busy_until = start;
                return Some(Ran { entry: t.entry, timer: Some(k), rx: None, due, start, ops: 0 });
            }
            self.touch_report = report;
        }
        Some(self.run(t.entry, Some(k), None, due, start, hal))
    }

    /// Runs every event that starts at or before `until`.
    pub fn run_until(&mut self, until: u64, hal: &mut dyn Hal) {
        while self.next_start().is_some_and(|s| s <= until) {
            if self.step(hal).is_none() {
                break;
            }
        }
    }

    fn run(&mut self, entry: u16, timer: Option<u32>, rx: Option<(u32, u8)>, due: u64, start: u64, hal: &mut dyn Hal) -> Ran {
        let timer_count = self.motion_timer.or(self.touch_timer).unwrap_or(self.timers.len());
        let mut io = Io { hal, roles: &self.roles, analog_max: self.analog_max, eeprom: self.eeprom, timers: &mut self.timers[..timer_count], motion_sample: &self.motion_sample, touch_report: &self.touch_report, eth: self.eth.as_mut(), net: self.net.as_mut(), display: self.display.as_mut(), decls: self.module.decls(), buffers: &mut self.buffers, buffer_rows: self.module.buffers(), range_faults: &mut self.range_faults, rx, start, op_ns: self.op_ns };
        let ops = match exec::run_observed(&self.module, entry as usize, &mut self.state, &mut self.scratch, &mut io, &mut self.observer) {
            Exit::Done { ops } | Exit::Suspended { ops } => ops,
        };
        self.busy_until = start.saturating_add(ops.saturating_mul(self.op_ns));
        Ran { entry, timer, rx, due, start, ops }
    }
}

/// The ports of one handler run.
struct Io<'a> {
    hal: &'a mut dyn Hal,
    roles: &'a [Role],
    analog_max: i32,
    eeprom: u32,
    timers: &'a mut [Timer],
    motion_sample: &'a [i16; 6],
    touch_report: &'a TouchReport,
    eth: Option<&'a mut Ethernet>,
    net: Option<&'a mut Net>,
    display: Option<&'a mut Display>,
    decls: &'a [Decl],
    buffers: &'a mut [Vec<u32>],
    buffer_rows: &'a [Buffer],
    range_faults: &'a mut [u32],
    rx: Option<(u32, u8)>,
    start: u64,
    op_ns: u64,
}

/// Clamp independently of the signed range validity check. The addition is
/// widened before it is made, including for i32::MAX offsets and counts.
fn buffer_range(off: i32, n: i32, len: u32) -> (usize, usize, bool) {
    let start = i64::from(off).clamp(0, i64::from(len));
    let count = i64::from(n).clamp(0, i64::from(len) - start);
    let invalid = off < 0 || n < 0 || i64::from(off) + i64::from(n) > i64::from(len);
    (start as usize, count as usize, invalid)
}

impl Io<'_> {
    fn range_fault(&mut self, t: u64, site: u32, n: i32) {
        let bits = &mut self.range_faults[site as usize / 32];
        let mask = 1 << (site % 32);
        if *bits & mask == 0 {
            *bits |= mask;
            self.hal.fault(t, Fault::Range { site, n });
        }
    }
}

impl Ports for Io<'_> {
    fn buffer(&mut self, index: u32) -> &mut [u32] { &mut self.buffers[index as usize] }

    fn call(&mut self, port: u16, site: u32, ops: u64, args: &[u32], rets: &mut [u32]) -> PortCall {
        let t = self.start.saturating_add(ops.saturating_mul(self.op_ns));
        let arg = |k: usize| args.get(k).copied().unwrap_or(0) as i32;
        let role = |pin: i32| if pin < 0 { Role::Unused } else { self.roles.get(pin as usize).copied().unwrap_or(Role::Unused) };
        match port {
            ports::TOUCH_X => rets[0] = u32::from(self.touch_report.x),
            ports::TOUCH_Y => rets[0] = u32::from(self.touch_report.y),
            ports::TOUCH_DOWN => rets[0] = u32::from(self.touch_report.down),
            ports::ACCEL | ports::GYRO => {
                let axis = arg(0);
                rets[0] = if (0..3).contains(&axis) {
                    self.motion_sample[axis as usize + if port == ports::GYRO { 3 } else { 0 }] as i32 as u32
                } else {
                    self.hal.fault(t, Fault::Range { site, n: axis });
                    0
                };
            }
            ports::PLAY..=ports::MIC_REWIND => {
                let result = match port {
                    ports::PLAY | ports::RECORD => {
                        let index = args[0] as usize;
                        let (off, n, bad) = buffer_range(arg(1), arg(2), self.buffer_rows[index].len);
                        if bad {
                            self.hal.fault(t, Fault::Range { site, n: arg(2) });
                            Ok(0)
                        } else if port == ports::PLAY {
                            self.hal.play(t, &self.buffers[index], off, n, site)
                        } else {
                            self.hal.record(t, &mut self.buffers[index], off, n, site)
                        }
                    }
                    ports::SPEAKER_FREE => self.hal.speaker_free(t, site),
                    ports::SPEAKER_QUEUED => self.hal.speaker_queued(t, site),
                    ports::SPEAKER_ON if matches!(arg(0), 0 | 1) => self.hal.speaker_on(t, arg(0) == 1, site).map(|()| 0),
                    ports::MIC_READY => self.hal.mic_ready(t, site),
                    ports::MIC_REWIND if arg(0) >= 0 => self.hal.mic_rewind(t, args[0], site),
                    _ => {
                        self.hal.fault(t, Fault::Range { site, n: arg(0) });
                        Ok(0)
                    }
                };
                let count = result.unwrap_or_else(|reason| {
                    self.hal.fault(t, Fault::AudioUnavailable { port, site, reason });
                    0
                });
                if let Some(ret) = rets.first_mut() { *ret = count; }
            }
            ports::ETH_SEND | ports::ETH_RECV | ports::ETH_MAC => {
                if let Some(r) = rets.first_mut() { *r = 0; }
                let n = match port {
                    ports::ETH_SEND => arg(2),
                    ports::ETH_RECV => watch::ETH_MAX as i32,
                    _ => 6,
                };
                if port == ports::ETH_SEND && !(watch::ETH_MIN as i32..=watch::ETH_MAX as i32).contains(&n) {
                    self.hal.fault(t, Fault::EthLength { site, n });
                    return PortCall::Done;
                }
                let index = arg(0) as usize;
                let buffer = self.buffer_rows[index];
                let (off, count, bad) = buffer_range(arg(1), n, buffer.len);
                if bad {
                    self.hal.fault(t, Fault::EthRange { port, site, off: arg(1) });
                    return PortCall::Done;
                }
                match port {
                    ports::ETH_SEND => {
                        let mut frame = [0; watch::ETH_MAX];
                        for (i, byte) in frame[..count].iter_mut().enumerate() {
                            *byte = buffer.load(&self.buffers[index], off + i) as u8;
                        }
                        rets[0] = u32::from(self.hal.eth_send(t, &frame[..count], site));
                    }
                    ports::ETH_RECV => {
                        if let Some(frame) = self.eth.as_mut().and_then(|eth| eth.pending.pop_front()) {
                            for (i, &byte) in frame.iter().enumerate() {
                                buffer.store(&mut self.buffers[index], off + i, u32::from(byte));
                            }
                            rets[0] = frame.len() as u32;
                        }
                    }
                    _ => {
                        for (i, byte) in self.hal.eth_mac(t, site).into_iter().enumerate() {
                            buffer.store(&mut self.buffers[index], off + i, u32::from(byte));
                        }
                    }
                }
            }
            ports::NET_LINK..=ports::NET_KIND => {
                if let Some(r) = rets.first_mut() { *r = 0; }
                let Some(net) = self.net.as_mut() else { return PortCall::Done };
                let (link, code, detail) = net.link;
                match port {
                    ports::NET_LINK => rets[0] = u32::from(link),
                    ports::NET_LINK_ERROR => rets[0] = u32::from(code),
                    ports::NET_LINK_DETAIL => rets[0] = detail,
                    _ => {
                        let c = arg(0);
                        let Some(conn) = (c >= 0).then(|| net.conns.get_mut(c as usize)).flatten() else {
                            self.hal.fault(t, Fault::NetHandle { port, site, c });
                            return PortCall::Done;
                        };
                        let c8 = c as u8;
                        rets[0] = match port {
                            ports::NET_OPEN => conn.open(c8, t, self.hal),
                            ports::NET_CLOSE => conn.close(c8, t, self.hal),
                            ports::NET_STATE => u32::from(conn.state),
                            ports::NET_EPOCH => conn.epoch,
                            ports::NET_ERROR => u32::from(conn.error),
                            ports::NET_DETAIL => conn.detail,
                            ports::NET_LEFT => conn.left(),
                            ports::NET_KIND => conn.kind(),
                            _ => {
                                // NET_SEND (c, buf, off, n, flags) or NET_RECV (c, buf, off, cap).
                                let index = arg(1) as usize;
                                let buffer = self.buffer_rows[index];
                                let (off, count, bad) = buffer_range(arg(2), arg(3), buffer.len);
                                if bad {
                                    self.hal.fault(t, Fault::NetRange { port, site, off: arg(2) });
                                    return PortCall::Done;
                                }
                                if port == ports::NET_SEND {
                                    let data: Vec<u8> = (0..count).map(|i| buffer.load(&self.buffers[index], off + i) as u8).collect();
                                    conn.send(c8, t, self.hal, &data, arg(4))
                                } else {
                                    let mut data = Vec::new();
                                    conn.recv(c8, t, self.hal, &mut data, count);
                                    for (i, &byte) in data.iter().enumerate() {
                                        buffer.store(&mut self.buffers[index], off + i, u32::from(byte));
                                    }
                                    data.len() as u32
                                }
                            }
                        };
                    }
                }
            }
            ports::COPY => {
                let (dst, src) = (arg(0) as usize, arg(2) as usize);
                let (d, s) = (self.buffer_rows[dst], self.buffer_rows[src]);
                let (doff, dn, dbad) = buffer_range(arg(1), arg(4), d.len);
                let (soff, sn, sbad) = buffer_range(arg(3), arg(4), s.len);
                let count = dn.min(sn);
                if dbad || sbad { self.range_fault(t, site, arg(4)); }
                // Reverse a rightward move within one allocation; every other
                // case is safe forwards. No temporary allocation per call.
                for i in 0..count {
                    let i = if dst == src && doff > soff { count - i - 1 } else { i };
                    let value = s.load(&self.buffers[src], soff + i);
                    d.store(&mut self.buffers[dst], doff + i, value);
                }
            }
            ports::FILL => {
                let dst = arg(0) as usize;
                let buffer = self.buffer_rows[dst];
                let (off, count, bad) = buffer_range(arg(1), arg(2), buffer.len);
                if bad { self.range_fault(t, site, arg(2)); }
                for i in off..off + count { buffer.store(&mut self.buffers[dst], i, arg(3) as u32); }
            }
            ports::EEPROM_READ | ports::EEPROM_WRITE => {
                let addr = arg(0);
                if addr < 0 || addr as u32 >= self.eeprom {
                    self.hal.fault(t, Fault::EepromAddr { site, addr });
                    if let Some(r) = rets.first_mut() { *r = 0; }
                } else if port == ports::EEPROM_READ {
                    rets[0] = u32::from(self.hal.eeprom_read(t, addr as u16, site));
                } else {
                    self.hal.eeprom_write(t, addr as u16, arg(1) as u8, site);
                }
            }
            ports::SHIFT_OUT => {
                if role(arg(0)) != Role::Output {
                    self.hal.fault(t, Fault::Pin { port, pin: arg(0), site });
                } else if role(arg(1)) != Role::Output {
                    self.hal.fault(t, Fault::Pin { port, pin: arg(1), site });
                } else if !(1..=32).contains(&arg(3)) {
                    self.hal.fault(t, Fault::ShiftBits { site, bits: arg(3) });
                } else {
                    self.hal.shift(t, arg(0) as u32, arg(1) as u32, arg(2) as u32, arg(3) as u8, site);
                }
            }
            ports::PIN_WRITE => match role(arg(0)) {
                Role::Output => self.hal.write(t, arg(0) as u32, arg(1) != 0, site),
                _ => self.hal.fault(t, Fault::Pin { port, pin: arg(0), site }),
            },
            ports::PIN_READ => match role(arg(0)) {
                Role::Unused | Role::Pwm(_) | Role::Analog => self.hal.fault(t, Fault::Pin { port, pin: arg(0), site }),
                Role::Output | Role::Input { .. } => {
                    if let Some(r) = rets.get_mut(0) {
                        *r = self.hal.read(t, arg(0) as u32, site) as u32;
                    }
                }
            },
            ports::PWM_DUTY => match role(arg(0)) {
                Role::Pwm(_) => self.hal.duty(t, arg(0) as u32, arg(1).clamp(0, 255) as u8, site),
                _ => self.hal.fault(t, Fault::Pin { port, pin: arg(0), site }),
            },
            ports::ADC_READ => match role(arg(0)) {
                Role::Analog => {
                    if let Some(r) = rets.get_mut(0) {
                        *r = (self.hal.adc(t, arg(0) as u32, site) as i32).min(self.analog_max) as u32;
                    }
                }
                _ => self.hal.fault(t, Fault::Pin { port, pin: arg(0), site }),
            },
            ports::SERIAL_BYTE => {
                let link = arg(0);
                let value = match self.rx {
                    Some((current, byte)) if link >= 0 && current == link as u32 => byte as i32,
                    _ => {
                        self.hal.fault(t, Fault::Serial { port, link, site });
                        -1
                    }
                };
                if let Some(r) = rets.get_mut(0) { *r = value as u32; }
            }
            ports::CLOCK_US | ports::CLOCK_MS => {
                let unit = if port == ports::CLOCK_US { NS_PER_US } else { NS_PER_US * 1000 };
                let now = self.hal.clock(t, site);
                if let Some(r) = rets.get_mut(0) {
                    *r = (now / unit) as u32;
                }
            }
            ports::TIMER_STOP | ports::TIMER_START | ports::TIMER_PERIOD => {
                let Some(timer) = (arg(0) >= 0).then(|| self.timers.get_mut(arg(0) as usize)).flatten() else {
                    self.hal.fault(t, Fault::Timer { port, timer: arg(0), site });
                    return PortCall::Done;
                };
                match port {
                    ports::TIMER_STOP => timer.due = None,
                    ports::TIMER_START => timer.due = Some(t.saturating_add(timer.period)),
                    _ => timer.period = (arg(1).max(0) as u64) * NS_PER_US,
                }
            }
            ports::LOOP_CAP => self.hal.fault(t, Fault::LoopCap { site }),
            ports::PROBE => self.hal.probe(t, arg(0) as u8, arg(1) as u16),
            ports::PRESENT | ports::FILL_RECT => {
                let result = match self.display.as_mut() {
                    Some(display) if port == ports::PRESENT => {
                        let src = arg(1) as usize;
                        let buffer = self.buffer_rows[src];
                        let dimensions = buffer_dimensions(&self.decls[buffer.decl as usize]).expect("verified U16 buffer");
                        display.present(buffer, &self.buffers[src], dimensions, core::array::from_fn(arg))
                    }
                    Some(display) => display.fill_rect(core::array::from_fn(arg)),
                    None => Err(DrawRefusal::Handle),
                };
                if let Err(refusal) = result { self.hal.fault(t, Fault::Draw { port, site, refusal }); }
            }
            _ => {}
        }
        PortCall::Done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Input(u16);
    impl Hal for Input {
        fn configure(&mut self, _: u64, _: u32, _: Role) {}
        fn serial(&mut self, _: u64, _: u32, _: serial::SerialSetting) {}
        fn write(&mut self, _: u64, _: u32, _: bool, _: u32) {}
        fn shift(&mut self, _: u64, _: u32, _: u32, _: u32, _: u8, _: u32) {}
        fn eeprom_read(&mut self, _: u64, _: u16, _: u32) -> u8 { self.0 as u8 }
        fn eeprom_write(&mut self, _: u64, _: u16, _: u8, _: u32) {}
        fn duty(&mut self, _: u64, _: u32, _: u8, _: u32) {}
        fn read(&mut self, _: u64, _: u32, _: u32) -> bool { self.0 & 1 != 0 }
        fn adc(&mut self, _: u64, _: u32, _: u32) -> u16 { self.0 }
        fn eth_send(&mut self, _: u64, _: &[u8], _: u32) -> bool { false }
        fn eth_mac(&mut self, _: u64, _: u32) -> [u8; 6] { [0; 6] }
        fn clock(&mut self, t: u64, _: u32) -> u64 { t }
        fn fault(&mut self, _: u64, _: Fault) {}
    }

    #[test]
    fn io_results_stay_inside_the_port_intervals() {
        for max in [0, 1, 127, 255, 1023, 4095, 65535] {
            for input in [0, 1, 127, 128, 255, 256, 1023, 1024, 4095, 4096, u16::MAX] {
                let mut hal = Input(input);
                let mut io = Io {
                    hal: &mut hal, roles: &[Role::Input { pull: 0 }, Role::Analog, Role::Output],
                    analog_max: max, eeprom: 1024, timers: &mut [], motion_sample: &[0; 6],
                    touch_report: &TouchReport::default(), eth: None, net: None, display: None,
                    decls: &[], buffers: &mut [], buffer_rows: &[], range_faults: &mut [],
                    rx: None, start: 0, op_ns: 1,
                };
                for port in [ports::PIN_READ, ports::ADC_READ, ports::EEPROM_READ] {
                    for arg in [-1i32, 0, 1, 2, 3, 1023, 1024, i32::MIN, i32::MAX] {
                        let mut rets = [0]; // The VM's result-buffer contract, including faults.
                        assert_eq!(io.call(port, 0, 0, &[arg as u32], &mut rets), PortCall::Done);
                        let (lo, hi) = ports::ret_interval(port, 0, max);
                        assert!((lo..=hi).contains(&(rets[0] as i32)),
                            "port {port}, argument {arg}, input {input}, max {max}: {} outside {lo}..={hi}", rets[0]);
                        if port == ports::PIN_READ && arg == 0 { assert_eq!(rets[0], u32::from(input & 1 != 0)); }
                        if port == ports::ADC_READ && arg == 1 { assert_eq!(rets[0], u32::from(input).min(max as u32)); }
                        if port == ports::EEPROM_READ && arg == 0 { assert_eq!(rets[0], u32::from(input as u8)); }
                    }
                }
            }
        }
        for (port, def) in ports::PORTS.iter().enumerate() {
            for ret in 0..=def.rets.len() {
                let expected = match (port as u16, ret) {
                    (ports::PIN_READ, 0) => (0, 1),
                    (ports::ADC_READ, 0) => (0, 1023),
                    (ports::EEPROM_READ, 0) => (0, 255),
                    _ => (i32::MIN, i32::MAX),
                };
                assert_eq!(ports::ret_interval(port as u16, ret, 1023), expected);
            }
        }
        assert_eq!(ports::ret_interval(u16::MAX, 0, 1023), (i32::MIN, i32::MAX));
    }
}
