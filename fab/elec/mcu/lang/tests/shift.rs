//! Shift calls, pin roles and bit counts against the reference device.
use fab_elec_mcu::{compile, Compiled, Profile};
use fab_elec_mcu_rt as rt;
use rt::{module, ports, sched::{Device, Fault, Hal, Role}};

const P: Profile = Profile {
    id: 3, pins: 20, max_entry_ops: 50_000, call_ops: None, ram_words: 2048, buffer_bytes: 0, eeprom: 0, eeprom_write_ns: 0,
    f64: false, op_ns: 62, clock_hz: 16_000_000,
    pwm: &rt::pwm::NONE, analog: &rt::analog::NONE, serial: &rt::serial::NONE, motion: None, touch: false, audio: None, display: None,
};
const PINS: &[(&str, u32)] = &[("PB2", 10), ("PB3", 11), ("PB5", 13), ("PD2", 2)];

#[derive(Debug, PartialEq, Eq)]
enum Call {
    Shift { data: u32, clock: u32, value: u32, bits: u8, site: u32 },
    Write { pin: u32, high: bool, site: u32 },
    Fault(Fault),
}

#[derive(Default)]
struct Scripted {
    roles: Vec<(u64, u32, Role)>,
    calls: Vec<(u64, Call)>,
}

impl Hal for Scripted {
    fn eeprom_read(&mut self, _: u64, _: u16, _: u32) -> u8 { unreachable!("this profile has no EEPROM") }
    fn eeprom_write(&mut self, _: u64, _: u16, _: u8, _: u32) { unreachable!("this profile has no EEPROM") }
    fn configure(&mut self, t: u64, pin: u32, role: Role) { self.roles.push((t, pin, role)); }
    fn serial(&mut self, _: u64, _: u32, _: rt::serial::SerialSetting) { unreachable!("this fixture declares no serial link") }
    fn shift(&mut self, t: u64, data: u32, clock: u32, value: u32, bits: u8, site: u32) {
        self.calls.push((t, Call::Shift { data, clock, value, bits, site }));
    }
    fn write(&mut self, t: u64, pin: u32, high: bool, site: u32) { self.calls.push((t, Call::Write { pin, high, site })); }
    fn duty(&mut self, _: u64, _: u32, _: u8, _: u32) { unreachable!("this fixture declares no PWM output") }
    fn read(&mut self, _: u64, _: u32, _: u32) -> bool { unreachable!("this fixture reads nothing") }
    fn adc(&mut self, _: u64, _: u32, _: u32) -> u16 { unreachable!("this fixture declares no analog input") }
    fn eth_send(&mut self, _: u64, _: &[u8], _: u32) -> bool { unreachable!("this fixture sends nothing") }
    fn eth_mac(&mut self, _: u64, _: u32) -> [u8; 6] { unreachable!("this fixture uses no Ethernet") }
    fn clock(&mut self, t: u64, _: u32) -> u64 { t }
    fn fault(&mut self, t: u64, fault: Fault) { self.calls.push((t, Call::Fault(fault))); }
}

fn compiled(src: &str) -> Compiled {
    compile(src, &P, PINS, &[]).unwrap_or_else(|e| panic!("compile {src:?}: {e:?}"))
}

fn site(c: &Compiled, src: &str, call: &str) -> u32 {
    let start = src.find(call).expect("call in source");
    let (index, s) = c.sites.iter().enumerate().find(|(_, s)| s.start as usize == start).expect("call has a source site");
    assert_eq!(s.end as usize, start + call.len(), "{call}: exact call span");
    let line = src[..start].bytes().filter(|b| *b == b'\n').count() + 1;
    let col = start - src[..start].rfind('\n').map_or(0, |n| n + 1) + 1;
    assert_eq!((s.line as usize, s.col as usize), (line, col), "{call}: source line and column");
    index as u32
}

#[test]
fn shift_then_latch_and_bit_count_boundaries() {
    let src = "\
let latch = output(PB2)
let data = output(PB3)
let clock = output(PB5)
var leds: i32 = -1985229329
let tick = every(ms(10), fn() {
    shift_out(data, clock, leds, 24)
    write(latch, high)
    write(latch, low)
})
";
    for bits in [1, 24, 32] {
        let src = src.replace(", 24)", &format!(", {bits})"));
        let c = compiled(&src);
        let m = module::load(&c.module, &P).expect("shift module verifies");
        let mut dev = Device::new(m, &P);
        let mut hal = Scripted::default();
        assert_eq!(dev.boot(&mut hal), None, "only the timer runs the shift");
        assert_eq!(hal.roles, [(0, 10, Role::Output), (0, 11, Role::Output), (0, 13, Role::Output)], "three declared outputs configure at boot");
        let ran = dev.step(&mut hal).expect("timer fires");
        assert_eq!(hal.calls.iter().map(|(_, call)| call).collect::<Vec<_>>(), [
            &Call::Shift { data: 11, clock: 13, value: 0x89ab_cdef, bits,
                site: site(&c, &src, &format!("shift_out(data, clock, leds, {bits})")) },
            &Call::Write { pin: 10, high: true, site: site(&c, &src, "write(latch, high)") },
            &Call::Write { pin: 10, high: false, site: site(&c, &src, "write(latch, low)") },
        ], "{bits} bits: shift arguments preserve the value's bits and precede both latch writes without faults");
        assert!(hal.calls.windows(2).all(|w| w[0].0 < w[1].0), "shift and latch retain call-time order");
        assert!(hal.calls.iter().all(|(t, _)| *t >= ran.start && *t < ran.start + ran.ops * u64::from(P.op_ns)), "HAL calls fall within the handler");
    }
}

#[test]
fn dynamic_faults_repeat_without_driving_and_check_pins_first() {
    let src = "\
let data = output(PB3)
let clock = output(PB5)
let button = input(PD2, pull_up)
var zero: i32 = 0
var large: i32 = 33
var missing: i32 = 7
var input_pin: i32 = 2
let tick = every(ms(10), fn() {
    shift_out(data, clock, 1, zero)
    shift_out(data, clock, 1, large)
    shift_out(missing, input_pin, 1, zero)
    shift_out(data, input_pin, 1, large)
    shift_out(data, missing, 1, 24)
})
";
    let c = compiled(src);
    let mut dev = Device::new(module::load(&c.module, &P).expect("computed arguments verify"), &P);
    let mut hal = Scripted::default();
    dev.boot(&mut hal);
    let expected = [
        Fault::ShiftBits { site: site(&c, src, "shift_out(data, clock, 1, zero)"), bits: 0 },
        Fault::ShiftBits { site: site(&c, src, "shift_out(data, clock, 1, large)"), bits: 33 },
        Fault::Pin { port: ports::SHIFT_OUT, pin: 7, site: site(&c, src, "shift_out(missing, input_pin, 1, zero)") },
        Fault::Pin { port: ports::SHIFT_OUT, pin: 2, site: site(&c, src, "shift_out(data, input_pin, 1, large)") },
        Fault::Pin { port: ports::SHIFT_OUT, pin: 7, site: site(&c, src, "shift_out(data, missing, 1, 24)") },
    ];
    for tick in 0..2 {
        dev.step(&mut hal).expect("timer fires again");
        assert_eq!(hal.calls.len(), expected.len(), "tick {tick}: exactly one fault per call and no shift/write reaches the HAL");
        for ((_, call), fault) in hal.calls.iter().zip(expected) {
            assert_eq!(call, &Call::Fault(fault), "tick {tick}: faults repeat at their sites, checking data then clock then bits");
        }
        hal.calls.clear();
    }
}

#[test]
fn constant_pin_and_bit_refusals_name_the_call() {
    for (decls, call, message) in [
        ("let clock = output(PB5)", "shift_out(PB3, clock, 1, 24)", "`shift_out` drives pin 11 (PB3) as its data pin, which is not declared as an output: declare it with `let name = output(pin)`"),
        ("let data = output(PB3)", "shift_out(data, PB5, 1, 24)", "`shift_out` drives pin 13 (PB5) as its clock pin, which is not declared as an output: declare it with `let name = output(pin)`"),
        ("let data = output(PB3)\nlet button = input(PD2, pull_up)", "shift_out(data, button, 1, 24)", "`shift_out` drives pin 2 (PD2) as its clock pin, which is not declared as an output: declare it with `let name = output(pin)`"),
        ("let data = output(PB3)\nlet clock = output(PB5)", "shift_out(data, clock, 1, 0)", "`shift_out` sends 1 to 32 bits; this call sends 0"),
        ("let data = output(PB3)\nlet clock = output(PB5)", "shift_out(data, clock, 1, 33)", "`shift_out` sends 1 to 32 bits; this call sends 33"),
    ] {
        let src = format!("{decls}\nfn setup() {{\n    {call}\n}}\n");
        let errors = compile(&src, &P, PINS, &[]).err().expect("constant misuse is refused");
        assert_eq!(errors.len(), 1, "{call}: one refusal");
        assert_eq!(errors[0].message, message, "{call}: actionable refusal names the pin role or bit count");
        let start = src.find(call).expect("refused call in source");
        assert_eq!((errors[0].start, errors[0].end), (start, start + call.len()), "{call}: exact refusal span");
    }
}
