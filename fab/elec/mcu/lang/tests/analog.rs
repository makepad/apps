//! Analog declarations, source refusals and samples at their port sites.
use fab_elec_mcu::{compile, Compiled, Profile};
use fab_elec_mcu_rt as rt;
use rt::{analog, module::{self, LoadError}, ports, sched::{Device, Fault, Hal, Role}};

const P: Profile = Profile {
    id: 3, pins: 22, max_entry_ops: 50_000, call_ops: None, ram_words: 2048, buffer_bytes: 0, eeprom: 0, eeprom_write_ns: 0,
    f64: false, op_ns: 62, clock_hz: 16_000_000,
    pwm: &rt::pwm::ATMEGA328P, analog: &analog::ATMEGA328P, serial: &rt::serial::NONE, motion: None, touch: false, audio: None, display: None,
};
const PINS: &[(&str, u32)] = &[
    ("PC0", 14), ("PC1", 15), ("PC2", 16), ("PC3", 17), ("PC4", 18), ("PC5", 19),
    ("ADC6", 20), ("ADC7", 21), ("PB5", 13), ("PD3", 3),
];
const MS: u64 = 1_000_000;

fn compiled(src: &str) -> Compiled {
    compile(src, &P, PINS, &[]).unwrap_or_else(|e| panic!("compile {src:?}: {e:?}"))
}

struct Scripted {
    codes: std::vec::IntoIter<u16>,
    roles: Vec<(u64, u32, Role)>,
    reads: Vec<(u64, u32, u32)>,
    writes: Vec<(u64, u32, bool, u32)>,
    faults: Vec<(u64, Fault)>,
}

impl Scripted {
    fn new(codes: Vec<u16>) -> Self {
        Self { codes: codes.into_iter(), roles: Vec::new(), reads: Vec::new(), writes: Vec::new(), faults: Vec::new() }
    }
}

impl Hal for Scripted {
    fn eeprom_read(&mut self, _: u64, _: u16, _: u32) -> u8 { unreachable!("this profile has no EEPROM") }
    fn eeprom_write(&mut self, _: u64, _: u16, _: u8, _: u32) { unreachable!("this profile has no EEPROM") }
    fn shift(&mut self, _: u64, _: u32, _: u32, _: u32, _: u8, _: u32) { unreachable!("this fixture shifts nothing") }
    fn configure(&mut self, t: u64, pin: u32, role: Role) { self.roles.push((t, pin, role)); }
    fn serial(&mut self, _: u64, _: u32, _: rt::serial::SerialSetting) { unreachable!("this fixture declares no serial link") }
    fn write(&mut self, t: u64, pin: u32, high: bool, site: u32) { self.writes.push((t, pin, high, site)); }
    fn duty(&mut self, _: u64, _: u32, _: u8, _: u32) { unreachable!("this fixture declares no PWM output") }
    fn read(&mut self, _: u64, _: u32, _: u32) -> bool { unreachable!("this fixture makes no valid digital read") }
    fn adc(&mut self, t: u64, pin: u32, site: u32) -> u16 {
        self.reads.push((t, pin, site));
        self.codes.next().expect("one scripted code per valid ADC call")
    }
    fn eth_send(&mut self, _: u64, _: &[u8], _: u32) -> bool { false }
    fn eth_mac(&mut self, _: u64, _: u32) -> [u8; 6] { [2, 0, 0, 0, 0, 1] }
    fn clock(&mut self, t: u64, _: u32) -> u64 { t }
    fn fault(&mut self, t: u64, fault: Fault) { self.faults.push((t, fault)); }
}

fn check_site(c: &Compiled, src: &str, site: u32, call: &str) {
    let start = src.find(call).expect("call in source");
    let s = c.sites[site as usize];
    assert_eq!((s.start as usize, s.end as usize), (start, start + call.len()), "{call}: exact user call span");
    let line = src[..start].bytes().filter(|b| *b == b'\n').count() + 1;
    let col = start - src[..start].rfind('\n').map_or(0, |n| n + 1) + 1;
    assert_eq!((s.line as usize, s.col as usize), (line, col), "{call}: user line and column");
}

fn value(c: &Compiled, dev: &Device<'_>, name: &str) -> u32 {
    let v = c.lowered.state_vars.iter().find(|v| v.name == name).expect("state variable");
    dev.state()[v.offset as usize]
}

#[test]
fn samples_and_dynamic_pin_faults() {
    let src = "\
let pot = analog(PC0)
let extra = analog(ADC6)
let led = output(PB5)
var a: i32 = 0
var b: i32 = 0
let tick = every(ms(10), fn() {
    a = analog_read(pot)
    b = analog_read(extra)
    write(led, if a >= 512 { high } else { low })
    write(led, if b == 1023 { high } else { low })
})
";
    let c = compiled(src);
    let m = module::load(&c.module, &P).expect("verified analog module");
    let mut dev = Device::new(m, &P);
    let mut hal = Scripted::new(vec![0, 1023, 512, 511, u16::MAX, u16::MAX]);
    assert_eq!(dev.boot(&mut hal), None, "no setup handler");
    assert_eq!(hal.roles, [(0, 13, Role::Output), (0, 14, Role::Analog), (0, 20, Role::Analog)],
        "declared analog and digital pins configure once, in pin order");
    for (tick, (a, b, levels)) in [(0, 1023, [false, true]), (512, 511, [true, false]), (1023, 1023, [true, true])].into_iter().enumerate() {
        let ran = dev.step(&mut hal).expect("timer fires");
        assert_eq!((ran.timer, ran.due, ran.start), (Some(0), (tick as u64 + 1) * 10 * MS, (tick as u64 + 1) * 10 * MS),
            "tick {tick}: analog declarations do not consume timer handles or shift the grid");
        assert_eq!((value(&c, &dev, "a"), value(&c, &dev, "b")), (a, b), "tick {tick}: samples stay integer codes, clamped to 1023");
        assert_eq!((hal.reads.len(), hal.writes.len()), (2 * (tick + 1), 2 * (tick + 1)), "tick {tick}: two samples and two writes");
        for (index, (pin, call)) in [(14, "analog_read(pot)"), (20, "analog_read(extra)")].into_iter().enumerate() {
            let (t, actual_pin, site) = hal.reads[2 * tick + index];
            assert_eq!(actual_pin, pin, "{call}: samples the declared runtime pin");
            check_site(&c, src, site, call);
            assert!(t >= ran.start && t < ran.start + ran.ops * P.op_ns as u64, "{call}: sample instant is inside the handler");
            assert_eq!((t - ran.start) % P.op_ns as u64, 0, "{call}: sample instant follows the operation grid");
            let (wt, wp, high, ws) = hal.writes[2 * tick + index];
            assert_eq!((wp, high), (13, levels[index]), "tick {tick}: output follows the sampled code");
            assert!(wt > hal.reads[2 * tick + 1].0, "tick {tick}: output follows both samples");
            check_site(&c, src, ws, ["write(led, if a >= 512 { high } else { low })", "write(led, if b == 1023 { high } else { low })"][index]);
        }
    }
    assert_eq!(hal.codes.next(), None, "every scripted sample consumed");
    assert!(hal.faults.is_empty(), "valid analog reads do not fault: {:?}", hal.faults);
    assert!(hal.reads.windows(2).all(|w| w[0].0 < w[1].0), "samples retain their individual instants and call order");

    // State-derived pins bypass the constant-pin check; the runtime must
    // enforce the same roles and leave bad reads at zero without calling the HAL.
    let src = "\
let pot = analog(PC0)
let led = output(PB5)
var p: i32 = 20
var led_pin: i32 = 13
var pot_pin: i32 = 14
var missing: i32 = 7
var wrong_role: i32 = 7
var digital: i32 = 7
fn setup() {
    missing = analog_read(p + 1)
    wrong_role = analog_read(led_pin)
    digital = read(pot_pin)
    write(pot_pin, high)
}
";
    let c = compiled(src);
    let mut dev = Device::new(module::load(&c.module, &P).expect("dynamic pins verify"), &P);
    let mut hal = Scripted::new(vec![]);
    let ran = dev.boot(&mut hal).expect("setup runs");
    assert!(hal.reads.is_empty() && hal.writes.is_empty(), "invalid pin calls never reach HAL reads or writes");
    let faults = [(ports::ADC_READ, 21, "analog_read(p + 1)"), (ports::ADC_READ, 13, "analog_read(led_pin)"),
        (ports::PIN_READ, 14, "read(pot_pin)"), (ports::PIN_WRITE, 14, "write(pot_pin, high)")];
    assert_eq!(hal.faults.len(), faults.len(), "one fault per invalid call");
    for ((t, fault), (port, pin, call)) in hal.faults.iter().copied().zip(faults) {
        let Fault::Pin { port: actual_port, pin: actual_pin, site } = fault else { panic!("{call}: expected pin fault, got {fault:?}") };
        assert_eq!((actual_port, actual_pin), (port, pin), "{call}: fault identifies port and pin");
        check_site(&c, src, site, call);
        assert!(t < ran.start + ran.ops * P.op_ns as u64, "{call}: fault occurs inside setup");
    }
    for name in ["missing", "wrong_role", "digital"] {
        assert_eq!(value(&c, &dev, name), 0, "{name}: faulted read returns zero");
    }
}

fn refusal(src: &str, profile: &Profile, call: &str, message: &str) {
    let es = compile(src, profile, PINS, &[]).err().unwrap_or_else(|| panic!("must refuse {call}"));
    assert_eq!(es.len(), 1, "{call}: one refusal");
    assert_eq!(es[0].message, message, "{call}: refusal text");
    let start = src.find(call).expect("refused call in source");
    assert_eq!((es[0].start, es[0].end), (start, start + call.len()), "{call}: exact refusal span");
}

#[test]
fn analog_table_and_source_refusals() {
    assert_eq!((analog::ATMEGA328P.bits, analog::ATMEGA328P.max(), analog::NONE.max()), (10, 1023, 0), "10-bit conversion range; empty table has zero range");
    assert_eq!(analog::ATMEGA328P.pins.len(), 8, "eight analog channels");
    for channel in 0..8 {
        assert_eq!(analog::ATMEGA328P.get(14 + channel), Some(&analog::AnalogPin {
            pin: 14 + channel, channel: channel as u8, digital: channel < 6,
        }), "channel {channel}: runtime pin, mux channel and digital capability");
    }
    assert_eq!(analog::ATMEGA328P.get(3), None, "PD3 has no analog channel");
    assert_eq!(analog::NONE.get(14), None, "empty table has no analog pins");
    for (src, call, message) in [
        ("let pot = analog(PD3)\nfn setup() {}", "analog(PD3)", "pin 3 (PD3) has no analog input on this chip (PC0, PC1, PC2, PC3, PC4, PC5, ADC6, ADC7 have)"),
        ("let led = output(ADC6)\nfn setup() {}", "output(ADC6)", "pin 20 (ADC6) is an analog-only pin: declare it with `analog(..)`"),
        ("let button = input(ADC7, pull_none)\nfn setup() {}", "input(ADC7, pull_none)", "pin 21 (ADC7) is an analog-only pin: declare it with `analog(..)`"),
        ("let motor = pwm(ADC6, hz(490))\nfn setup() {}", "pwm(ADC6, hz(490))", "pin 20 (ADC6) is an analog-only pin: declare it with `analog(..)`"),
        ("let pot = analog(PC0)\nfn setup() { let v = read(pot) }", "read(pot)", "`read` reads pin 14 (PC0), an analog input: read it with `analog_read(..)`"),
        ("let pot = analog(PC0)\nfn setup() { write(pot, high) }", "write(pot, high)", "`write` drives pin 14 (PC0), an analog input: declare it with `output(..)` to drive it"),
        ("let led = output(PB5)\nfn setup() { let v = analog_read(led) }", "analog_read(led)", "`analog_read` reads pin 13 (PB5), which is not declared as an analog input: declare it with `let name = analog(pin)`"),
        ("let pot = analog(PC0)\nlet button = input(PC0, pull_none)\nfn setup() {}", "input(PC0, pull_none)", "this does not fit the chip: pin 14 is declared twice"),
        ("let button = input(PC0, pull_none)\nlet pot = analog(PC0)\nfn setup() {}", "analog(PC0)", "this does not fit the chip: pin 14 is declared twice"),
        ("let pot = analog(22)\nfn setup() {}", "analog(22)", "this does not fit the chip: pin 22 is not on this chip (its pins are 0..22)"),
    ] {
        refusal(src, &P, call, message);
    }
    refusal("let pot = analog(PC0)\nfn setup() {}", &Profile { analog: &analog::NONE, ..P }, "analog(PC0)", "this chip has no analog inputs");
    let c = compiled("let pot = analog(ADC6)\nfn setup() {}");
    assert!(matches!(module::load(&c.module, &Profile { pins: 20, ..P }), Err(LoadError::PinRange { decl: 0, pin: 20, pins: 20 })),
        "an analog-capable profile must also cover the declared runtime pin");
    assert!(matches!(module::load(&c.module, &Profile { analog: &analog::NONE, ..P }), Err(LoadError::AnalogPin { decl: 0, pin: 20 })),
        "the verifier rejects analog declarations when the profile has no analog table");
    compiled("let led = output(PC0)\nlet button = input(PC1, pull_up)\nlet pot = analog(ADC7)\nfn setup() { write(led, read(button)) }");
}
