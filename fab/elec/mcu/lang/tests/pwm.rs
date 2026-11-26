//! PWM settings, source refusals and duty calls against the AIR reference.
use fab_elec_mcu::{compile, Compiled, Profile, FORMS};
use fab_elec_mcu_rt as rt;
use makepad_script_compute::{ir, lower::event::DeclHandle};
use rt::ports::{self, FORM_AFTER, FORM_EVERY, FORM_INPUT, FORM_OUTPUT, FORM_PWM, FORM_SERIAL, PIN_READ, PIN_WRITE, PWM_DUTY};
use rt::pwm::{self, PwmMode, PwmOut, PwmSetting};
use rt::sched::{Device, Fault, Hal, Ran, Role};

const P: Profile = Profile { id: 3, pins: 20, max_entry_ops: 50_000, call_ops: None, ram_words: 2048, buffer_bytes: 0, eeprom: 0, eeprom_write_ns: 0, f64: false, op_ns: 62, clock_hz: 16_000_000, analog: &rt::analog::NONE, pwm: &pwm::ATMEGA328P, serial: &rt::serial::NONE, motion: None, touch: false, audio: None, display: None };
const PINS: &[(&str, u32)] = &[("D3", 3), ("D4", 4), ("D5", 5), ("D6", 6), ("D9", 9), ("D10", 10), ("D11", 11), ("D13", 13)];
const MS: u64 = 1_000_000;
const T0: PwmSetting = PwmSetting { timer: 0, mode: PwmMode::Fast, prescale: 8, top: 255 };
const T1: PwmSetting = PwmSetting { timer: 1, mode: PwmMode::PhaseCorrect, prescale: 64, top: 255 };
const T2: PwmSetting = PwmSetting { timer: 2, mode: PwmMode::PhaseCorrect, prescale: 32, top: 255 };

fn compiled(src: &str) -> Compiled {
    compile(src, &P, PINS, &[]).unwrap_or_else(|e| panic!("compile {src:?}: {e:?}"))
}

fn line_of(src: &str, needle: &str) -> u32 {
    let at = src.find(needle).unwrap_or_else(|| panic!("{needle:?} is in the source"));
    src[..at].matches('\n').count() as u32 + 1
}

fn refusal(src: &str, needle: &str, message: &str) {
    match compile(src, &P, PINS, &[]) {
        Ok(_) => panic!("compiled, but must refuse {needle}:\n{src}"),
        Err(es) => {
            assert_eq!(es.len(), 1, "one refusal for {needle}");
            assert_eq!(es[0].message, message, "refusal text for {needle}");
            assert_eq!(es[0].line_col(src).0 as u32, line_of(src, needle), "refusal line for {needle}");
        }
    }
}

#[test]
fn pwm_settings_and_refusals() {
    assert_eq!(FORMS.len(), ports::FORMS as usize, "declaration table lengths");
    assert_eq!([FORM_OUTPUT, FORM_INPUT, FORM_EVERY, FORM_AFTER, FORM_PWM, FORM_SERIAL], [0, 1, 2, 3, 4, 5], "stable form numbers");
    for (index, (name, number, handle, class)) in [
        ("output", FORM_OUTPUT, DeclHandle::Arg(0), 0),
        ("input", FORM_INPUT, DeclHandle::Arg(0), 0),
        ("every", FORM_EVERY, DeclHandle::Ordinal, 1),
        ("after", FORM_AFTER, DeclHandle::Ordinal, 1),
        ("pwm", FORM_PWM, DeclHandle::Arg(0), 0),
        ("serial", FORM_SERIAL, DeclHandle::Ordinal, 2),
    ].into_iter().enumerate() {
        assert_eq!(index as u32, number, "{name} form index");
        assert_eq!((FORMS[index].name, FORMS[index].handle, FORMS[index].class), (name, handle, class), "{name} form and handle");
    }
    assert_eq!(FORMS[FORM_PWM as usize].name, "pwm", "PWM form");
    assert_eq!(PWM_DUTY, 8, "PWM duty port index");

    let c = compiled("\
let a = pwm(D5, hz(7812))
let b = pwm(D6, hz(7812))
let c = pwm(D9, hz(490))
let d = pwm(D10, hz(490))
let e = pwm(D11, hz(980))
let f = pwm(D3, hz(980))
fn setup() {}
");
    assert_eq!(c.module[rt::format::H_PORT_VERSION], ports::PORT_TABLE_VERSION, "module carries the current port table version");
    let expected = [
        PwmOut { decl: 0, pin: 5, asked_mhz: 7_812_000, real_mhz: 7_812_500, setting: T0 },
        PwmOut { decl: 1, pin: 6, asked_mhz: 7_812_000, real_mhz: 7_812_500, setting: T0 },
        PwmOut { decl: 2, pin: 9, asked_mhz: 490_000, real_mhz: 490_196, setting: T1 },
        PwmOut { decl: 3, pin: 10, asked_mhz: 490_000, real_mhz: 490_196, setting: T1 },
        PwmOut { decl: 4, pin: 11, asked_mhz: 980_000, real_mhz: 980_392, setting: T2 },
        PwmOut { decl: 5, pin: 3, asked_mhz: 980_000, real_mhz: 980_392, setting: T2 },
    ];
    assert_eq!(c.pwm, expected, "six PWM pins keep declaration order and their timer settings");
    assert_eq!(rt::module::load(&c.module, &P).expect("verified").pwm(), expected, "the module owns the same resolved settings");
    assert_eq!([T0, T1, T2].map(|s| s.period_cycles()), [2048, 32640, 16320], "Fast (TOP+1)N and phase-correct 2 TOP N periods");
    assert_eq!(c.notes.len(), 6, "one note per PWM declaration");
    assert_eq!(c.notes[0], "pin D5 (OC0B, Timer0): 7812.5 Hz (asked 7812 Hz), fast, clk/8, TOP 255", "Fast clock note");
    assert_eq!(c.notes[2], "pin D9 (OC1A, Timer1): 490.2 Hz (asked 490 Hz), phase-correct, clk/64, TOP 255", "phase-correct note and tenths rounding");
    assert_eq!(c.notes[5], "pin D3 (OC2B, Timer2): 980.4 Hz (asked 980 Hz), phase-correct, clk/32, TOP 255", "notes follow declarations, not pin order");

    assert_eq!([0, 1, 128, 254, 255].map(|level| T0.high_cycles(level)), [0, 16, 1032, 2040, 2048], "Fast true-low, OCR+1, and true-high endpoints");
    assert_eq!([0, 1, 128, 255].map(|level| T1.high_cycles(level)), [0, 128, 16384, 32640], "phase-correct high cycles and both endpoints");
    assert_eq!([0, 1, 128, 255].map(|level| T1.ocr(level)), [0, 1, 128, 255], "8-bit TOP preserves the level");

    let c = compiled("let a = pwm(D9, hz(25000))\nlet b = pwm(D10, hz(25500))\nfn setup() {}\n");
    let free = PwmSetting { timer: 1, mode: PwmMode::PhaseCorrect, prescale: 1, top: 320 };
    assert_eq!(c.pwm, [
        PwmOut { decl: 0, pin: 9, asked_mhz: 25_000_000, real_mhz: 25_000_000, setting: free },
        PwmOut { decl: 1, pin: 10, asked_mhz: 25_500_000, real_mhz: 25_000_000, setting: free },
    ], "the first declaration fixes the free TOP for both outputs");
    assert_eq!(free.period_cycles(), 640, "25 kHz has 16 million / 25000 cycles");
    assert_eq!([0, 128, 255].map(|level| free.ocr(level)), [0, 161, 320], "free TOP scales OCR with nearest-integer rounding");
    assert_eq!([0, 128, 255].map(|level| free.high_cycles(level)), [0, 322, 640], "free TOP high cycles use the scaled OCR");
    assert_eq!(c.notes[0], "pin D9 (OC1A, Timer1): 25000 Hz (asked 25000 Hz), phase-correct, clk/1, TOP 320 (ICR1)", "free TOP note names its register and drops .0");
    assert_eq!(c.notes[1], "pin D10 (OC1B, Timer1): 25000 Hz (asked 25500 Hz), phase-correct, clk/1, TOP 320 (ICR1)", "shared pin reports actual and requested frequencies separately");

    let c = compiled("let a = pwm(D9, 100050)\nfn setup() {}\n");
    assert_eq!(c.pwm[0].setting, PwmSetting { timer: 1, mode: PwmMode::PhaseCorrect, prescale: 8, top: 9995 }, "free TOP skips a prescaler whose TOP exceeds 65535");
    assert_eq!(c.pwm[0].real_mhz, 100_050, "free TOP real frequency rounds to millihertz");
    assert_eq!(c.notes[0], "pin D9 (OC1A, Timer1): 100.1 Hz (asked 100.1 Hz), phase-correct, clk/8, TOP 9995 (ICR1)", "Hz tenths round a half upward");

    // Close synthetic table frequencies exercise a tie that the chip's
    // widely spaced prescalers cannot put inside the tolerance window.
    const TIE_TABLE: pwm::PwmTable = pwm::PwmTable {
        pins: &[pwm::PwmPin { pin: 9, timer: 0, output: "OC0A" }],
        timers: &[pwm::PwmTimer { name: "Timer0", mode: PwmMode::PhaseCorrect, top: 800, prescalers: &[100, 101], free_top: 0, free_top_reg: "", clock: false }],
    };
    let profile = Profile { pwm: &TIE_TABLE, ..P };
    let c = compile("let a = pwm(D9, 99505)\nfn setup() {}\n", &profile, PINS, &[]).unwrap_or_else(|e| panic!("table tie: {e:?}"));
    assert_eq!(c.pwm[0].setting, PwmSetting { timer: 0, mode: PwmMode::PhaseCorrect, prescale: 100, top: 800 }, "equidistant table frequencies choose the smaller prescaler");
    assert_eq!(c.pwm[0].real_mhz, 100_000, "100000 and 99010 mHz are both 495 mHz from 99505");

    // The chip's only Fast timer is fixed; a synthetic free timer covers
    // Fast's rounded count minus one, including a half-way request.
    const FAST_TABLE: pwm::PwmTable = pwm::PwmTable {
        pins: &[pwm::PwmPin { pin: 9, timer: 0, output: "OC0A" }],
        timers: &[pwm::PwmTimer { name: "Timer0", mode: PwmMode::Fast, top: 255, prescalers: &[1, 8], free_top: 65535, free_top_reg: "TOP", clock: false }],
    };
    let profile = Profile { clock_hz: 1000, pwm: &FAST_TABLE, ..P };
    let c = compile("let a = pwm(D9, 3200)\nfn setup() {}\n", &profile, PINS, &[]).unwrap_or_else(|e| panic!("Fast free TOP: {e:?}"));
    let fast = c.pwm[0].setting;
    assert_eq!(fast, PwmSetting { timer: 0, mode: PwmMode::Fast, prescale: 1, top: 312 }, "312.5 Fast counts round to 313, then TOP is one less");
    assert_eq!((fast.period_cycles(), c.pwm[0].real_mhz), (313, 3195), "Fast free TOP uses TOP+1 and rounds actual millihertz upward");
    assert_eq!([0, 128, 255].map(|level| fast.high_cycles(level)), [0, 158, 313], "Fast free TOP scales OCR and never exceeds a full period");

    let c = compiled("let a = pwm(D11, hz(505))\nfn setup() {}\n");
    assert_eq!(c.pwm[0].real_mhz, 490_196, "505 Hz is inside the 3 percent tolerance");
    assert!(pwm::matches(970, 1000) && pwm::matches(1030, 1000), "both exact 3 percent endpoints are included");
    assert!(!pwm::matches(969, 1000) && !pwm::matches(1031, 1000), "both sides outside 3 percent are excluded");
    assert!(pwm::matches(u64::MAX, u64::MAX) && !pwm::matches(0, u64::MAX), "tolerance arithmetic does not wrap at u64 limits");

    for (src, needle, message) in [
        ("let a = pwm(D4, hz(490))\nfn setup() {}\n", "pwm(D4", "pin D4 has no PWM output on this chip (D3, D5, D6, D9, D10, D11 have)"),
        ("let a = pwm(D5, hz(1000))\nfn setup() {}\n", "pwm(D5", "pin D5 (OC0B) is on Timer0, the device clock: it runs at 7812.5 Hz only; D3, D9, D10, D11 take other frequencies"),
        ("let a = pwm(D9, hz(490))\nlet b = pwm(D10, hz(25000))\nfn setup() {}\n", "pwm(D10", "pin D10 (OC1B) asks 25000 Hz but pin D9 (OC1A), on the same Timer1, runs at 490.2 Hz"),
        ("let a = pwm(D9, hz(25000))\nlet b = pwm(D10, hz(26000))\nfn setup() {}\n", "pwm(D10", "pin D10 (OC1B) asks 26000 Hz but pin D9 (OC1A), on the same Timer1, runs at 25000 Hz"),
        ("let a = pwm(D11, hz(600))\nfn setup() {}\n", "pwm(D11", "Timer2 has no setting near 600 Hz; it has 31372.5, 3921.6, 980.4, 490.2, 245.1, 122.5, 30.6"),
        ("let a = pwm(D11, hz(506))\nfn setup() {}\n", "pwm(D11", "Timer2 has no setting near 506 Hz; it has 31372.5, 3921.6, 980.4, 490.2, 245.1, 122.5, 30.6"),
        ("let a = pwm(D9, 0)\nfn setup() {}\n", "pwm(D9", "a PWM frequency must be above 0 Hz"),
        ("let a = pwm(D9, -1)\nfn setup() {}\n", "pwm(D9", "a PWM frequency must be above 0 Hz"),
        ("let a = pwm(D9, 1)\nfn setup() {}\n", "pwm(D9", "Timer1 has no setting near 0 Hz; it has 31372.5, 3921.6, 490.2, 122.5, 30.6, and through ICR1 0.1 to 2666666.7 Hz"),
        ("let a = pwm(D9, 2147483647)\nfn setup() {}\n", "pwm(D9", "Timer1 has no setting near 2147483.6 Hz; it has 31372.5, 3921.6, 490.2, 122.5, 30.6, and through ICR1 0.1 to 2666666.7 Hz"),
        ("let a = pwm(D9, hz(490))\nlet b = output(D9)\nfn setup() {}\n", "output(D9)", "this does not fit the chip: pin 9 is declared twice"),
        ("let a = output(D9)\nlet b = pwm(D9, hz(490))\nfn setup() {}\n", "pwm(D9", "this does not fit the chip: pin 9 is declared twice"),
        ("let a = pwm(D4, 0)\nfn setup() {}\n", "pwm(D4", "pin D4 has no PWM output on this chip (D3, D5, D6, D9, D10, D11 have)"),
        ("let a = pwm(D5, hz(7812))\nlet b = pwm(D6, hz(1000))\nfn setup() {}\n", "pwm(D6", "pin D6 (OC0A) is on Timer0, the device clock: it runs at 7812.5 Hz only; D3, D9, D10, D11 take other frequencies"),
        ("let m0 = pwm(D9, hz(490))\nfn setup() {\n    write(m0, high)\n}\n", "write(m0", "`write` drives pin 9 (D9), a PWM output: set its level with `duty(..)`"),
        ("let m0 = pwm(D9, hz(490))\nfn setup() {\n    read(m0)\n}\n", "read(m0", "`read` reads pin 9 (D9), a PWM output: only `input(..)` and `output(..)` pins can be read"),
        ("let led = output(D13)\nfn setup() {\n    duty(led, 10)\n}\n", "duty(led", "`duty` drives pin 13 (D13), which is not declared as PWM: declare it with `let name = pwm(pin, hz(..))`"),
    ] {
        refusal(src, needle, message);
    }

    let c = compile("let a = pwm(9, hz(490))\nfn setup() {}\n", &P, &[("first", 9), ("second", 9)], &[]).unwrap_or_else(|e| panic!("aliases: {e:?}"));
    assert!(c.notes[0].starts_with("pin first (OC1A, Timer1):"), "the first name for a pin wins");
    let c = compile("let a = pwm(9, hz(490))\nfn setup() {}\n", &P, &[], &[]).unwrap_or_else(|e| panic!("numeric pin: {e:?}"));
    assert!(c.notes[0].starts_with("pin 9 (OC1A, Timer1):"), "unnamed pins use their number");
}

const RAMP: &str = "\
let m0 = pwm(D9, hz(490))
let m1 = pwm(D10, hz(490))
let led = output(D13)
var x = 0
var p = 13
fn setup() {
    duty(m0, 0)
}
let ramp = every(ms(10), fn() {
    x = x + 100
    duty(m0, x)
    duty(m1, 255 - x)
    if x >= 300 {
        duty(p, 5)
        stop(ramp)
    }
})
";

#[derive(Clone, Copy, Debug, PartialEq)]
enum Ev {
    Configure(u64, u32, Role),
    Serial(u64, u32, rt::serial::SerialSetting),
    Duty { t: u64, pin: u32, level: u8, site: u32 },
    Write { t: u64, pin: u32, high: bool, site: u32 },
    Read { t: u64, pin: u32, site: u32, high: bool },
    Clock { t: u64, site: u32 },
    Fault(u64, Fault),
}

struct Scripted { evs: Vec<Ev> }

impl Hal for Scripted {
    fn eeprom_read(&mut self, _: u64, _: u16, _: u32) -> u8 { unreachable!("this profile has no EEPROM") }
    fn eeprom_write(&mut self, _: u64, _: u16, _: u8, _: u32) { unreachable!("this profile has no EEPROM") }
    fn shift(&mut self, _: u64, _: u32, _: u32, _: u32, _: u8, _: u32) { unreachable!("this fixture shifts nothing") }
    fn serial(&mut self, t: u64, link: u32, setting: rt::serial::SerialSetting) { self.evs.push(Ev::Serial(t, link, setting)); }
    fn configure(&mut self, t: u64, pin: u32, role: Role) { self.evs.push(Ev::Configure(t, pin, role)); }
    fn duty(&mut self, t: u64, pin: u32, level: u8, site: u32) { self.evs.push(Ev::Duty { t, pin, level, site }); }
    fn write(&mut self, t: u64, pin: u32, high: bool, site: u32) { self.evs.push(Ev::Write { t, pin, high, site }); }
    fn read(&mut self, t: u64, pin: u32, site: u32) -> bool {
        self.evs.push(Ev::Read { t, pin, site, high: true });
        true
    }
    fn adc(&mut self, _: u64, _: u32, _: u32) -> u16 { unreachable!("this fixture declares no analog input") }
    fn eth_send(&mut self, _: u64, _: &[u8], _: u32) -> bool { false }
    fn eth_mac(&mut self, _: u64, _: u32) -> [u8; 6] { [2, 0, 0, 0, 0, 1] }
    fn clock(&mut self, t: u64, site: u32) -> u64 {
        self.evs.push(Ev::Clock { t, site });
        t
    }
    fn fault(&mut self, t: u64, fault: Fault) { self.evs.push(Ev::Fault(t, fault)); }
}

/// Duty returns nothing. The AIR reference must make the same calls, with
/// the same pins and sites, and the HAL must see saturated levels.
struct Replay<'a> { evs: std::iter::Filter<std::slice::Iter<'a, Ev>, fn(&&Ev) -> bool> }

impl ir::Ports for Replay<'_> {
    fn call(&mut self, port: u16, site: u32, args: &[u32], rets: &mut [u32]) {
        if matches!(port, ports::TIMER_STOP | ports::TIMER_START | ports::TIMER_PERIOD) { return; }
        let ev = self.evs.next().copied();
        match (port, ev) {
            (PWM_DUTY, Some(Ev::Duty { pin, level, site: s, .. })) => {
                assert!(rets.is_empty(), "duty has no result");
                assert_eq!((pin, level, s), (args[0], (args[1] as i32).clamp(0, 255) as u8, site), "AIR duty pin, saturated level and source site");
            }
            (PIN_WRITE, Some(Ev::Write { pin, high, site: s, .. })) => assert_eq!((pin, high, s), (args[0], args[1] != 0, site), "AIR write"),
            (PIN_READ, Some(Ev::Read { pin, site: s, high, .. })) => {
                assert_eq!((pin, s), (args[0], site), "AIR read");
                rets[0] = high as u32;
            }
            (ports::CLOCK_US | ports::CLOCK_MS, Some(Ev::Clock { t, site: s })) => {
                assert_eq!(s, site, "AIR clock site");
                rets[0] = (t / if port == ports::CLOCK_US { 1000 } else { MS }) as u32;
            }
            (PWM_DUTY | PIN_WRITE | PIN_READ, Some(Ev::Fault(_, Fault::Pin { port: p, pin, site: s }))) => {
                assert_eq!((p, pin, s), (port, args[0] as i32, site), "AIR call that faults keeps its port, pin and site");
                rets.fill(0);
            }
            _ => panic!("the reference called port {port} at site {site}; the device did {ev:?}"),
        }
    }
}

struct Trace { rans: Vec<Ran>, states: Vec<Vec<u32>>, evs: Vec<Ev>, next: Option<u64> }

fn run(c: &Compiled, until: u64) -> Trace {
    let module = rt::module::load(&c.module, &P).expect("verified");
    let mut dev = Device::new(module, &P);
    let mut hal = Scripted { evs: Vec::new() };
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
    let pinned: fn(&&Ev) -> bool = |e| !matches!(e, Ev::Configure(..) | Ev::Serial(..));
    let mut replay = Replay { evs: hal.evs.iter().filter(pinned) };
    let mut state = c.lowered.state_init.clone();
    for (ran, want) in rans.iter().zip(&states) {
        let e = &c.lowered.entries[ran.entry as usize];
        let mut scratch = vec![0; e.program.scratch_words()];
        let mut mem = ir::Mem { ctx: &mut [], state: &mut state, shared: ir::Shared::Read(&c.lowered.shared_init), bufs: &[] };
        let mut io = ir::Io::default();
        ir::run_ports(&e.program, &mut scratch, &mut mem, &mut io, 1, &mut replay);
        assert_eq!(&state, want, "state words after `{}` at {} ns", e.name, ran.start);
        assert!(ran.ops <= e.cost, "`{}` stays within its operation bound", e.name);
    }
    assert_eq!(replay.evs.next(), None, "AIR accounts for every device call and fault");
    Trace { rans, states, evs: hal.evs, next: dev.next_due() }
}

fn at_op_time(t: u64, ran: &Ran) {
    assert!(t > ran.start && t < ran.start + ran.ops * P.op_ns as u64, "port at {t} ns belongs inside handler at {} ns", ran.start);
    assert_eq!((t - ran.start) % P.op_ns as u64, 0, "port time uses the handler's operation grid");
}

#[test]
fn duty_events_on_a_scripted_hal() {
    let c = compiled(RAMP);
    let Trace { rans, states, evs, next } = run(&c, 40 * MS);
    assert_eq!(evs[..3], [Ev::Configure(0, 9, Role::Pwm(T1)), Ev::Configure(0, 10, Role::Pwm(T1)), Ev::Configure(0, 13, Role::Output)], "boot roles include full settings, in pin order, before setup");
    assert_eq!(evs.iter().filter(|e| matches!(e, Ev::Configure(..))).count(), 3, "each declared pin configured once");
    assert!(!evs.iter().any(|e| matches!(e, Ev::Serial(..))), "no serial configuration without a declaration");
    assert_eq!(rans.iter().map(|r| c.lowered.entries[r.entry as usize].name.as_str()).collect::<Vec<_>>(), ["setup", "ramp", "ramp", "ramp"], "setup and three ramp handlers");
    assert_eq!(rans.iter().map(|r| (r.timer, r.due, r.start)).collect::<Vec<_>>(), [(None, 0, 0), (Some(0), 10 * MS, 10 * MS), (Some(0), 20 * MS, 20 * MS), (Some(0), 30 * MS, 30 * MS)], "PWM declarations do not consume event timer handles or shift their grid");
    assert_eq!(next, None, "the ramp stops after its third tick, even after a pin fault");
    let x = c.lowered.state_vars.iter().find(|v| v.name == "x").expect("ramp state").offset as usize;
    assert_eq!(states.iter().map(|s| s[x]).collect::<Vec<_>>(), [0.0_f32, 100.0, 200.0, 300.0].map(f32::to_bits), "one state update per tick (unannotated state is f32)");

    let duties: Vec<_> = evs.iter().filter_map(|e| if let Ev::Duty { t, pin, level, site } = *e { Some((t, pin, level, site)) } else { None }).collect();
    let expected = [
        (0, 9, 0, "duty(m0, 0)"),
        (1, 9, 100, "duty(m0, x)"), (1, 10, 155, "duty(m1, 255 - x)"),
        (2, 9, 200, "duty(m0, x)"), (2, 10, 55, "duty(m1, 255 - x)"),
        (3, 9, 255, "duty(m0, x)"), (3, 10, 0, "duty(m1, 255 - x)"),
    ];
    assert_eq!(duties.len(), expected.len(), "setup plus six valid duty calls, no call for the invalid pin");
    for ((t, pin, level, site), (handler, want_pin, want_level, needle)) in duties.iter().copied().zip(expected) {
        assert_eq!((pin, level, c.sites[site as usize].line), (want_pin, want_level, line_of(RAMP, needle)), "{needle} at handler {handler}: pin, saturated level and source line");
        at_op_time(t, &rans[handler]);
    }
    assert!(duties.windows(2).all(|w| w[0].0 < w[1].0), "duty calls keep their individual instants and order");
    for tick in 2..=3 {
        for output in 0..2 {
            assert_eq!(duties[1 + (tick - 1) * 2 + output].0 - rans[tick].start, duties[1 + output].0 - rans[1].start, "tick {tick}, output {output}: identical work before duty has identical elapsed ops");
        }
    }
    let faults: Vec<_> = evs.iter().filter_map(|e| if let Ev::Fault(t, f) = *e { Some((t, f)) } else { None }).collect();
    assert_eq!(faults.len(), 1, "only the duty through variable p faults: {faults:?}");
    let (t, fault) = faults[0];
    let site = c.sites.iter().position(|s| s.line == line_of(RAMP, "duty(p, 5)")).expect("invalid duty's site") as u32;
    assert_eq!(fault, Fault::Pin { port: PWM_DUTY, pin: 13, site }, "a dynamic output pin is not a PWM pin");
    at_op_time(t, &rans[3]);
    assert!(t > duties[6].0, "the fault occurs after the third tick's two valid duties");

    // Dynamic pins bypass the constant-pin verifier; the scheduler must
    // still keep both ordinary pin ports away from PWM outputs.
    let src = "let m = pwm(D9, hz(490))\nvar p = 9\nvar value = 1\nfn setup() {\n    write(p, high)\n    value = read(p)\n}\n";
    let c = compiled(src);
    let trace = run(&c, 0);
    assert_eq!(trace.evs.len(), 3, "one PWM configuration and two faults, with no HAL reads or writes");
    assert_eq!(trace.evs[0], Ev::Configure(0, 9, Role::Pwm(T1)), "dynamic-pin check configures PWM");
    for (ev, (port, needle)) in trace.evs[1..].iter().zip([(PIN_WRITE, "write(p"), (PIN_READ, "read(p")]) {
        let Ev::Fault(t, Fault::Pin { port: p, pin, site }) = *ev else { panic!("{needle} must fault: {ev:?}") };
        assert_eq!((p, pin, c.sites[site as usize].line), (port, 9, line_of(src, needle)), "{needle} cannot use a dynamic PWM pin");
        at_op_time(t, &trace.rans[0]);
    }
    let value = c.lowered.state_vars.iter().find(|v| v.name == "value").expect("read state").offset as usize;
    assert_eq!(trace.states[0][value], 0, "a faulted PWM read returns zero without consulting the HAL");
}
