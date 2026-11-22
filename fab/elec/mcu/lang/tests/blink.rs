//! Firmware on a scripted HAL: a program's pin events land at exact device
//! times and name their source lines, the runtime's state equals the compute
//! core's reference interpreter after every handler, and what a chip cannot
//! run is refused before it runs.
use fab_elec_mcu::{compile, Compiled, Profile, FORMS, PORTS};
use fab_elec_mcu_rt as rt;
use makepad_script_compute::ir;
use rt::ports::{CLOCK_MS, CLOCK_US, PIN_READ, PIN_WRITE, PWM_DUTY};
use rt::sched::{Device, Fault, Hal, Ran, Role};

const P: Profile = Profile { id: 3, pins: 20, max_entry_ops: 50_000, call_ops: None, ram_words: 2048, buffer_bytes: 0, eeprom: 0, eeprom_write_ns: 0, f64: false, op_ns: 62, clock_hz: 16_000_000, analog: &rt::analog::NONE, pwm: &rt::pwm::NONE, serial: &rt::serial::NONE, motion: None, touch: false, audio: None, display: None };
const PINS: &[(&str, u32)] = &[("PD2", 2), ("PB0", 8), ("PB5", 13)];
const MS: u64 = 1_000_000;

#[path = "support/blink.rs"]
mod source;
use source::BLINK;

/// A handler that outlasts its period.
const BUSY: &str = "\
var n = 0
var acc = 0
let busy = every(us(2), fn() {
    n = n + 1
    for i in 0..40 {
        acc = acc + i * n
    }
})
";

#[derive(Clone, Copy, Debug, PartialEq)]
enum Ev {
    Configure(u64, u32, Role),
    Serial(u64, u32, rt::serial::SerialSetting),
    Write { t: u64, pin: u32, high: bool, site: u32 },
    Duty { t: u64, pin: u32, level: u8, site: u32 },
    Read { t: u64, pin: u32, site: u32, high: bool },
    Clock { t: u64, site: u32 },
    Fault(u64, Fault),
}

/// Pins as a script: the button (PD2) reads low (pressed) in `low`.
struct Scripted {
    evs: Vec<Ev>,
    low: std::ops::Range<u64>,
}

impl Hal for Scripted {
    fn eeprom_read(&mut self, _: u64, _: u16, _: u32) -> u8 { unreachable!("this profile has no EEPROM") }
    fn eeprom_write(&mut self, _: u64, _: u16, _: u8, _: u32) { unreachable!("this profile has no EEPROM") }
    fn shift(&mut self, _: u64, _: u32, _: u32, _: u32, _: u8, _: u32) { unreachable!("this fixture shifts nothing") }
    fn serial(&mut self, t: u64, link: u32, setting: rt::serial::SerialSetting) {
        self.evs.push(Ev::Serial(t, link, setting));
    }
    fn configure(&mut self, t: u64, pin: u32, role: Role) {
        self.evs.push(Ev::Configure(t, pin, role));
    }
    fn write(&mut self, t: u64, pin: u32, high: bool, site: u32) {
        self.evs.push(Ev::Write { t, pin, high, site });
    }
    fn duty(&mut self, t: u64, pin: u32, level: u8, site: u32) {
        self.evs.push(Ev::Duty { t, pin, level, site });
    }
    fn read(&mut self, t: u64, pin: u32, site: u32) -> bool {
        let high = !(pin == 2 && self.low.contains(&t));
        self.evs.push(Ev::Read { t, pin, site, high });
        high
    }
    fn adc(&mut self, _: u64, _: u32, _: u32) -> u16 { unreachable!("this fixture declares no analog input") }
    fn eth_send(&mut self, _: u64, _: &[u8], _: u32) -> bool { false }
    fn eth_mac(&mut self, _: u64, _: u32) -> [u8; 6] { [2, 0, 0, 0, 0, 1] }
    fn clock(&mut self, t: u64, site: u32) -> u64 {
        self.evs.push(Ev::Clock { t, site });
        t
    }
    fn fault(&mut self, t: u64, fault: Fault) {
        self.evs.push(Ev::Fault(t, fault));
    }
}

/// The reference side of the ports: replays what the HAL answered, and
/// checks each call against what the HAL saw.
struct Replay<'a> {
    evs: std::iter::Filter<std::slice::Iter<'a, Ev>, fn(&&Ev) -> bool>,
}

impl ir::Ports for Replay<'_> {
    fn call(&mut self, port: u16, site: u32, args: &[u32], rets: &mut [u32]) {
        if !matches!(port, PIN_WRITE | PIN_READ | CLOCK_US | CLOCK_MS | PWM_DUTY) {
            return;
        }
        let ev = self.evs.next().copied();
        match (port, ev) {
            (PIN_WRITE, Some(Ev::Write { pin, high, site: s, .. })) => assert_eq!((pin, high, s), (args[0], args[1] != 0, site), "a write"),
            (PWM_DUTY, Some(Ev::Duty { pin, level, site: s, .. })) => assert_eq!((pin, level, s), (args[0], (args[1] as i32).clamp(0, 255) as u8, site), "a duty"),
            (PIN_READ, Some(Ev::Read { pin, site: s, high, .. })) => {
                assert_eq!((pin, s), (args[0], site), "a read");
                rets[0] = high as u32;
            }
            (CLOCK_US | CLOCK_MS, Some(Ev::Clock { t, site: s })) => {
                assert_eq!(s, site, "a clock read");
                rets[0] = (t / if port == CLOCK_US { 1000 } else { MS }) as u32;
            }
            _ => panic!("the reference called port {} at site {}; the device did {:?}", port, site, ev),
        }
    }
}

/// A run: its handlers, the state words after each, the HAL's log and the
/// next event's due time.
struct Trace {
    rans: Vec<Ran>,
    states: Vec<Vec<u32>>,
    evs: Vec<Ev>,
    next: Option<u64>,
}

/// Boots `c`, runs it to `until`, and checks the runtime's state after every
/// handler against `ir::run_ports` on the entry's AIR, fed the same HAL
/// answers.
fn run(c: &Compiled, until: u64, low: std::ops::Range<u64>) -> Trace {
    let module = rt::module::load(&c.module, &P).expect("verified");
    let mut dev = Device::new(module, &P);
    let mut hal = Scripted { evs: Vec::new(), low };
    let mut rans = Vec::new();
    let mut states = Vec::new();
    if let Some(r) = dev.boot(&mut hal) {
        rans.push(r);
        states.push(dev.state().to_vec());
    }
    while dev.next_start().is_some_and(|s| s <= until) {
        rans.push(dev.step(&mut hal).expect("an event"));
        states.push(dev.state().to_vec());
    }
    let pinned: fn(&&Ev) -> bool = |e| matches!(e, Ev::Write { .. } | Ev::Duty { .. } | Ev::Read { .. } | Ev::Clock { .. });
    let mut replay = Replay { evs: hal.evs.iter().filter(pinned) };
    let mut state = c.lowered.state_init.clone();
    for (ran, want) in rans.iter().zip(&states) {
        let e = &c.lowered.entries[ran.entry as usize];
        let mut scratch = vec![0; e.program.scratch_words()];
        let mut mem = ir::Mem { ctx: &mut [], state: &mut state, shared: ir::Shared::Read(&c.lowered.shared_init), bufs: &[] };
        let mut io = ir::Io { ins: [&[], &[]], outs: [&mut [], &mut []] };
        ir::run_ports(&e.program, &mut scratch, &mut mem, &mut io, 1, &mut replay);
        assert_eq!(&state, want, "state words after `{}` at {} ns", e.name, ran.start);
        assert!(ran.ops <= e.cost, "`{}` ran {} ops; its bound is {}", e.name, ran.ops, e.cost);
    }
    assert_eq!(replay.evs.next(), None, "the reference made every port call the device made");
    Trace { rans, states, evs: hal.evs, next: dev.next_due() }
}

fn line_of(src: &str, needle: &str) -> u32 {
    let at = src.find(needle).unwrap_or_else(|| panic!("{needle:?} is in the source"));
    src[..at].matches('\n').count() as u32 + 1
}

fn var(c: &Compiled, state: &[u32], name: &str) -> u32 {
    let v = c.lowered.state_vars.iter().find(|v| v.name == name).unwrap_or_else(|| panic!("var {name}"));
    state[v.offset as usize]
}

fn refusal(src: &str) -> (u32, String) {
    match compile(src, &P, PINS, &[]) {
        Ok(_) => panic!("compiled, but must be refused:\n{src}"),
        Err(es) => (es[0].line_col(src).0 as u32, es[0].message.clone()),
    }
}

#[test]
fn blink_on_a_scripted_hal() {
    // The front end's port table is the runtime's.
    assert_eq!(PORTS.len(), rt::ports::PORTS.len(), "port tables");
    for (a, b) in PORTS.iter().zip(rt::ports::PORTS) {
        let code = |t: &ir::Ty| match t {
            ir::Ty::F32 => rt::ports::TY_F32,
            ir::Ty::I32 => rt::ports::TY_I32,
            ir::Ty::Bool => rt::ports::TY_BOOL,
            ir::Ty::F64 => u8::MAX,
        };
        assert_eq!(a.name, b.name, "port names");
        assert_eq!(a.params.iter().map(code).collect::<Vec<_>>(), b.params, "`{}` parameters", a.name);
        assert_eq!(a.rets.iter().map(code).collect::<Vec<_>>(), b.rets, "`{}` results", a.name);
    }
    use rt::ports::{FORM_AFTER, FORM_ANALOG, FORM_BYTES, FORM_DISPLAY, FORM_ETH, FORM_EVERY, FORM_IMAGE,
        FORM_INPUT, FORM_MOTION, FORM_OUTPUT, FORM_PWM, FORM_SAMPLES, FORM_SERIAL, FORM_SURFACE, FORM_TOUCH, FORM_WORDS};
    assert_eq!(FORMS.len(), rt::ports::FORMS as usize, "declaration table lengths");
    for (index, (number, name)) in [
        (FORM_OUTPUT, "output"), (FORM_INPUT, "input"), (FORM_EVERY, "every"),
        (FORM_AFTER, "after"), (FORM_PWM, "pwm"), (FORM_SERIAL, "serial"),
        (FORM_DISPLAY, "display"), (FORM_IMAGE, "image"), (FORM_BYTES, "bytes"),
        (FORM_SAMPLES, "samples"), (FORM_WORDS, "words"), (FORM_SURFACE, "surface"),
        (FORM_ANALOG, "analog"), (FORM_ETH, "eth"), (FORM_MOTION, "motion"), (FORM_TOUCH, "touch"),
    ].into_iter().enumerate() {
        assert_eq!(index as u32, number, "{name} form index");
        assert_eq!(FORMS[index].name, name, "{name} form name");
    }
    assert_eq!((FORMS[FORM_ANALOG as usize].handle, FORMS[FORM_ANALOG as usize].class, FORMS[FORM_ANALOG as usize].buffer),
        (makepad_script_compute::lower::event::DeclHandle::Arg(0), 0, None), "analog uses the pin handle and class, without a buffer");

    let c = compile(BLINK, &P, PINS, &[]).unwrap_or_else(|e| panic!("{:?}", e));
    // Programs with ports run in the interpreter: the compiled back ends decline them.
    for e in &c.lowered.entries {
        assert!(ir::has_ports(&e.program), "`{}` has ports", e.name);
        // The prelude's levels are i32: `if on { high } else { low }` selects integers.
        assert!(!e.program.vals.iter().any(|t| matches!(t, ir::Ty::F32 | ir::Ty::F64)), "`{}` has no float value", e.name);
        #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
        assert!(makepad_script_compute::native::compile(&e.program).is_none(), "native declines `{}`", e.name);
        let we = [makepad_script_compute::wasm::Entry { program: &e.program, simd: false }];
        assert!(makepad_script_compute::wasm::module(&we, Default::default()).is_none(), "wasm declines `{}`", e.name);
    }

    let pressed = 1000 * MS..2600 * MS;
    let Trace { rans, states, evs, next } = run(&c, 3000 * MS, pressed.clone());
    let module = rt::module::load(&c.module, &P).expect("verified");
    let setup = module.entry("setup").expect("setup") as u16;
    let names: Vec<&str> = rans.iter().map(|r| c.lowered.entries[r.entry as usize].name.as_str()).collect();
    assert_eq!(names, ["setup", "blink", "blink", "blink", "blink", "blink", "blink", "again", "blink", "blink", "blink"], "handlers in order");
    assert_eq!(rans[0].entry, setup);

    // Roles first (in pin order), then setup at boot; nothing faults.
    assert_eq!(evs[..2], [Ev::Configure(0, 2, Role::Input { pull: rt::ports::PULL_UP }), Ev::Configure(0, 13, Role::Output)], "roles at boot");
    assert!(!evs.iter().any(|e| matches!(e, Ev::Fault(..))), "no faults: {:?}", evs);
    assert!(!evs.iter().any(|e| matches!(e, Ev::Duty { .. })), "ordinary outputs never get a PWM duty");
    assert!(!evs.iter().any(|e| matches!(e, Ev::Serial(..))), "no serial configuration without a declaration");

    // Timers on their grid: blink at k * 250 ms, `again` at 2 s; blink
    // stopped itself after six ticks and `again` restarted it 250 ms from
    // its `start` call.
    for (k, r) in rans[1..7].iter().enumerate() {
        assert_eq!((r.timer, r.due, r.start), (Some(0), (k as u64 + 1) * 250 * MS, (k as u64 + 1) * 250 * MS), "blink tick {}", k + 1);
    }
    assert_eq!((rans[7].timer, rans[7].start), (Some(1), 2000 * MS), "again");
    let restart = rans[8].start - 2250 * MS;
    assert!(restart > 0 && restart <= rans[7].ops * P.op_ns as u64 && restart % P.op_ns as u64 == 0, "blink restarts from `start(blink)`: +{restart} ns");
    assert_eq!((rans[9].start - rans[8].start, rans[10].start - rans[9].start), (250 * MS, 250 * MS), "the restarted grid");
    assert_eq!(next, Some(rans[10].start + 250 * MS), "the next tick");

    // Each write at its handler's start plus its ops, naming its line.
    let writes: Vec<(u64, u32, bool, u32)> = evs.iter().filter_map(|e| if let Ev::Write { t, pin, high, site } = *e { Some((t, pin, high, site)) } else { None }).collect();
    assert_eq!(writes.len(), 10, "one write per setup and tick");
    let line = |site: u32| c.sites[site as usize].line;
    assert_eq!((writes[0].1, writes[0].2, line(writes[0].3)), (13, false, line_of(BLINK, "write(led, low)")), "setup's write");
    assert!(writes[0].0 > 0 && writes[0].0 % P.op_ns as u64 == 0, "setup writes after its ops: {} ns", writes[0].0);
    let blinks: Vec<&Ran> = rans.iter().filter(|r| r.timer == Some(0)).collect();
    let lag = writes[1].0 - blinks[0].start;
    assert!(lag > 0 && lag % P.op_ns as u64 == 0, "a tick writes after its ops: {lag} ns");
    for (k, (w, r)) in writes[1..].iter().zip(&blinks).enumerate() {
        assert_eq!(*w, (r.start + lag, 13, k % 2 == 0, writes[1].3), "tick {}'s write", k + 1);
        assert_eq!(line(w.3), line_of(BLINK, "write(led, if on"), "tick {}'s write names its line", k + 1);
    }

    // The clock and the button as the handlers saw them.
    for (k, (r, s)) in rans.iter().zip(&states).enumerate().filter(|(_, (r, _))| r.timer == Some(0)) {
        assert_eq!(var(&c, s, "stamp_ms") as u64, r.start / MS, "now_ms() at handler {k}");
        assert_eq!(var(&c, s, "pressed"), !pressed.contains(&r.start) as u32, "read(button) at handler {k}");
    }
    assert_eq!(var(&c, &states[states.len() - 1], "ticks"), 9, "ticks");

    // A handler that outlasts its 2 us period: it runs back to back, every
    // firing it could not run is counted (none dropped), the grid holds.
    let b = compile(BUSY, &P, PINS, &[]).unwrap_or_else(|e| panic!("{:?}", e));
    let Trace { rans, evs, next, .. } = run(&b, 100_000, 0..0);
    let period = 2000u64;
    let missed: u64 = evs.iter().map(|e| if let Ev::Fault(_, Fault::Overrun { timer: 0, missed }) = e { *missed as u64 } else { 0 }).sum();
    assert!(missed > 0, "the handler overran");
    for w in rans.windows(2) {
        assert_eq!(w[1].start, w[0].start + w[0].ops * P.op_ns as u64, "back to back");
        assert_eq!(w[1].due % period, 0, "due on the grid");
    }
    assert_eq!(rans.len() as u64 + missed, next.expect("still armed") / period - 1, "every firing ran or was counted");

    // A loop that reaches its run-time cap stops there and says so, at its line.
    let src = "var x: i32 = 0\nfn setup() {\n    while x >= 0 {\n        x = x + 1\n    }\n}\n";
    let w = compile(src, &P, PINS, &[]).unwrap_or_else(|e| panic!("{:?}", e));
    let t = run(&w, 0, 0..0);
    let caps: Vec<u32> = t.evs.iter().filter_map(|e| if let Ev::Fault(_, Fault::LoopCap { site }) = e { Some(w.sites[*site as usize].line) } else { None }).collect();
    assert_eq!(caps, [3], "one cap report, at the loop's line");
    assert_eq!(var(&w, &t.states[0], "x"), 1024, "the loop ran to its cap");

    // Refused before anything runs, at the line that asks.
    let src = "let led = output(PB5)\nfn setup() {\n    write(PB0, high)\n}\n";
    let (l, m) = refusal(src);
    assert!(l == 3 && m.contains("pin 8") && m.contains("output"), "a write on an undeclared pin: line {l}: {m}");
    let src = "let b = input(PD2, pull_up)\nfn setup() {\n    write(b, high)\n}\n";
    let (l, m) = refusal(src);
    assert!(l == 3 && m.contains("pin 2") && m.contains("output"), "a write on an input: line {l}: {m}");
    let src = "let led = output(PB5)\nfn setup() {\n    read(PB0)\n}\n";
    let (l, m) = refusal(src);
    assert!(l == 3 && m.contains("pin 8"), "a read of an undeclared pin: line {l}: {m}");
    let src = "var x = 0\nvar y = 0\nfn setup() {\n    while x < 100000 {\n        while y < 100000 {\n            y = y + 1\n        }\n        x = x + 1\n    }\n}\n";
    let (l, m) = refusal(src);
    assert!(l == 3 && m.contains("operations"), "loops the chip cannot afford: line {l}: {m}");
    let (_, m) = refusal("let led = output(40)\nfn setup() {\n    write(led, high)\n}\n");
    assert!(m.contains("does not fit"), "a pin the chip does not have: {m}");
}
