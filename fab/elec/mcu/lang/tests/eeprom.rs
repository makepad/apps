//! EEPROM byte storage, address faults and call-site refusals on the reference device.
use fab_elec_mcu::{compile, Compiled, Profile};
use fab_elec_mcu_rt as rt;
use rt::{module::{self, LoadError}, ports, sched::{Device, Fault, Hal, Role}, watch::{self, Event}};

const P: Profile = Profile {
    id: 3, pins: 22, max_entry_ops: 1_000_000, call_ops: None, ram_words: 2048, buffer_bytes: 0, eeprom: 1024, eeprom_write_ns: 3_400_000,
    f64: false, op_ns: 62, clock_hz: 16_000_000,
    pwm: &rt::pwm::ATMEGA328P, analog: &rt::analog::ATMEGA328P, serial: &rt::serial::ATMEGA328P, display: None, motion: None, touch: false, audio: None,
};

struct Recording {
    bytes: [u8; 1024],
    calls: Vec<(u64, Event)>,
}

impl Hal for Recording {
    fn eeprom_read(&mut self, t: u64, addr: u16, site: u32) -> u8 {
        let value = self.bytes[addr as usize];
        self.calls.push((t, Event::EepromRead { addr, value, site: site as u16 }));
        value
    }
    fn eeprom_write(&mut self, t: u64, addr: u16, value: u8, site: u32) {
        self.bytes[addr as usize] = value;
        self.calls.push((t, Event::EepromWrite { addr, value, site: site as u16 }));
    }
    fn configure(&mut self, _: u64, _: u32, _: Role) { unreachable!("this fixture has no pins") }
    fn serial(&mut self, _: u64, _: u32, _: rt::serial::SerialSetting) { unreachable!("this fixture has no serial link") }
    fn shift(&mut self, _: u64, _: u32, _: u32, _: u32, _: u8, _: u32) { unreachable!("this fixture shifts nothing") }
    fn write(&mut self, _: u64, _: u32, _: bool, _: u32) { unreachable!("this fixture has no outputs") }
    fn duty(&mut self, _: u64, _: u32, _: u8, _: u32) { unreachable!("this fixture has no PWM") }
    fn read(&mut self, _: u64, _: u32, _: u32) -> bool { unreachable!("this fixture has no pins") }
    fn adc(&mut self, _: u64, _: u32, _: u32) -> u16 { unreachable!("this fixture has no analog inputs") }
    fn eth_send(&mut self, _: u64, _: &[u8], _: u32) -> bool { unreachable!("this fixture has no Ethernet") }
    fn eth_mac(&mut self, _: u64, _: u32) -> [u8; 6] { unreachable!("this fixture has no Ethernet") }
    fn clock(&mut self, t: u64, _: u32) -> u64 { t }
    fn fault(&mut self, t: u64, fault: Fault) {
        let Fault::EepromAddr { site, addr } = fault else { panic!("unexpected fault: {fault:?}") };
        self.calls.push((t, Event::Fault { code: watch::FAULT_EEPROM_ADDR, site: site as u16, arg: addr }));
    }
}

fn site(c: &Compiled, src: &str, call: &str) -> u16 {
    let start = src.find(call).expect("call in source");
    let (index, s) = c.sites.iter().enumerate().find(|(_, s)| s.start as usize == start).expect("call has a site");
    assert_eq!(s.end as usize, start + call.len(), "{call}: exact source span");
    let line = src[..start].bytes().filter(|b| *b == b'\n').count() + 1;
    let col = start - src[..start].rfind('\n').map_or(0, |n| n + 1) + 1;
    assert_eq!((s.line as usize, s.col as usize), (line, col), "{call}: source line and column");
    index.try_into().expect("fixture sites fit the watch protocol")
}

#[test]
fn bytes_boundaries_and_computed_address_faults() {
    let src = "\
var fresh: i32 = 0
var stored: i32 = 0
var last: i32 = 0
var past: i32 = 1024
var negative: i32 = -1
var bad_high: i32 = 99
var bad_low: i32 = 99
fn setup() {
    fresh = eeprom_read(0)
    eeprom_write(0, 0x1234)
    stored = eeprom_read(0)
    eeprom_write(1023, 0xAB)
    last = eeprom_read(1023)
    eeprom_write(past, 7)
    bad_high = eeprom_read(past)
    eeprom_write(negative, 8)
    bad_low = eeprom_read(negative)
}
";
    let c = compile(src, &P, &[], &[]).unwrap_or_else(|e| panic!("{e:?}"));
    let mut dev = Device::new(module::load(&c.module, &P).expect("EEPROM module verifies"), &P);
    let mut hal = Recording { bytes: [0xff; 1024], calls: Vec::new() };
    let ran = dev.boot(&mut hal).expect("setup runs");
    let value = |name| {
        let v = c.lowered.state_vars.iter().find(|v| v.name == name).expect("state variable");
        dev.state()[v.offset as usize]
    };
    assert_eq!(value("fresh"), 255, "a new chip reads 0xFF");
    assert_eq!(value("stored"), 0x34, "a write retains the low eight bits for every later read");
    assert_eq!(value("last"), 0xab, "address 1023 is readable and writable");
    assert_eq!((value("bad_high"), value("bad_low")), (0, 0), "both computed invalid reads return zero");
    let mut expected_bytes = [0xff; 1024];
    expected_bytes[0] = 0x34;
    expected_bytes[1023] = 0xab;
    assert_eq!(hal.bytes, expected_bytes, "invalid writes leave all EEPROM bytes untouched");
    let read_site = site(&c, src, "eeprom_read(0)");
    let second_read = c.sites.iter().position(|s| s.start as usize == src.rfind("eeprom_read(0)").unwrap()).unwrap() as u16;
    assert_eq!(hal.calls.iter().map(|(_, e)| *e).collect::<Vec<_>>(), [
        Event::EepromRead { addr: 0, value: 255, site: read_site },
        Event::EepromWrite { addr: 0, value: 0x34, site: site(&c, src, "eeprom_write(0, 0x1234)") },
        Event::EepromRead { addr: 0, value: 0x34, site: second_read },
        Event::EepromWrite { addr: 1023, value: 0xab, site: site(&c, src, "eeprom_write(1023, 0xAB)") },
        Event::EepromRead { addr: 1023, value: 0xab, site: site(&c, src, "eeprom_read(1023)") },
        Event::Fault { code: watch::FAULT_EEPROM_ADDR, site: site(&c, src, "eeprom_write(past, 7)"), arg: 1024 },
        Event::Fault { code: watch::FAULT_EEPROM_ADDR, site: site(&c, src, "eeprom_read(past)"), arg: 1024 },
        Event::Fault { code: watch::FAULT_EEPROM_ADDR, site: site(&c, src, "eeprom_write(negative, 8)"), arg: -1 },
        Event::Fault { code: watch::FAULT_EEPROM_ADDR, site: site(&c, src, "eeprom_read(negative)"), arg: -1 },
    ], "valid calls reach HAL in order; invalid calls only report code, site and signed address");
    assert!(hal.calls.windows(2).all(|w| w[0].0 < w[1].0), "EEPROM calls retain their device-time order");
    assert!(hal.calls.iter().all(|(t, _)| *t >= ran.start && *t < ran.start + ran.ops * u64::from(P.op_ns)), "calls and faults occur inside setup");

    let access_ops = u64::from(P.eeprom_write_ns).div_ceil(u64::from(P.op_ns));
    assert_eq!(access_ops, 54_839, "3.4 ms rounds up to whole 62 ns ops at the 328P's 16 MHz profile");
    let notes = |c: &Compiled| c.notes.iter().filter(|n| n.starts_with("EEPROM:")).cloned().collect::<Vec<_>>();
    let note = notes(&c);
    assert_eq!(note.len(), 1, "a module with waits has one EEPROM note");
    assert!(note[0].contains("each access is counted at 3.4 ms") && note[0].contains("erase-and-write")
        && note[0].contains("single nominal figure") && note[0].contains("DS40002061B section 8.4"), "the note states the nominal datasheet timing used for accounting");
    let writes: String = (0..16).map(|addr| format!("eeprom_write({addr}, {addr})\n")).collect();
    let save = format!("fn setup() {{\n{writes}}}\n");
    let saving = compile(&save, &P, &[], &[]).expect("a 16-byte save fits the 328P profile");
    let loaded = module::load(&saving.module, &P).expect("the loader independently verifies the save cost");
    let entry = &saving.lowered.entries[0];
    assert!(loaded.entries()[0].cost >= 16 * access_ops, "the save includes all sixteen waits");
    assert_eq!(loaded.entries()[0].cost, entry.program.total_cost() + 16 * (access_ops - 1), "each EEPROM call replaces its original one-op charge");
    assert_eq!(entry.cost, loaded.entries()[0].cost, "frontend and verified save costs agree");
    assert_eq!(notes(&saving).len(), 1, "the wait note does not require a serial declaration");
    assert!(matches!(module::load(&saving.module, &Profile { max_entry_ops: 50_000, call_ops: None, ..P }), Err(LoadError::CostLimit { .. })), "the handler limit includes the waits");

    let read = compile("fn setup() { eeprom_read(0) }", &P, &[], &[]).expect("a read-only module compiles");
    let read_module = module::load(&read.module, &P).expect("read-only cost verifies");
    assert_eq!(read_module.entries()[0].cost, read.lowered.entries[0].program.total_cost(), "read-only calls keep the ordinary one-op port cost");
    assert!(read_module.entries()[0].cost < access_ops && notes(&read).is_empty(), "read-only modules carry neither a wait charge nor a wait note");
    let instant = compile(&save, &Profile { eeprom_write_ns: 0, ..P }, &[], &[]).expect("a zero-wait reference profile compiles");
    assert!(notes(&instant).is_empty(), "a write with no wait adds no wait note");
    assert!(matches!(module::load(&instant.module, &P), Err(LoadError::CostUnderstated { .. })), "the loader rejects a module whose header omits wait costs");

    // A write in a later entry affects a read in an earlier one; waits can
    // also change the longest branch and must multiply through capped loops.
    let mixed = "var choose: i32 = 0\nvar value: i32 = 0\nfn setup() { if choose == 0 { value = eeprom_read(0) } else { for i in 0..4 { value += i } } }\nlet save = every(ms(100), fn() { for i in 0..16 { eeprom_write(i, i) } })";
    let timed = compile(mixed, &P, &[], &[]).expect("branch and loop wait costs compile");
    let timed_module = module::load(&timed.module, &P).expect("branch and loop wait costs verify");
    assert!(timed_module.entries()[0].cost >= access_ops, "a write in another handler charges setup's read");
    assert!(timed_module.entries()[1].cost >= 16 * access_ops, "a capped loop charges every EEPROM access");
    let cheap = compile(mixed, &Profile { eeprom_write_ns: 0, ..P }, &[], &[]).expect("base costs compile");
    assert!(matches!(module::load(&cheap.module, &P), Err(LoadError::CostUnderstated { entry: 0, .. })), "the loader discovers later-entry writes before accepting an earlier read's cost");

    // At 16 MHz, 115200 is outside the existing receiver tolerance. An
    // 18.432 MHz clock gives exact 115200 baud with the same USART table.
    // The 328P limit is time (62.5 ms of ops), so it scales with the clock as chip::profile does.
    let serial_profile = Profile { clock_hz: 18_432_000, op_ns: 1_000_000_000 / 18_432_000, max_entry_ops: 18_432_000 / 16, call_ops: None, ..P };
    for (body, waits) in [(writes.as_str(), true), ("eeprom_read(0)", false)] {
        let source = format!("let rx = serial(115200, fn() {{}})\nfn setup() {{\n{body}\n}}\n");
        let c = compile(&source, &serial_profile, &[], &[]).expect("115200 baud EEPROM fixture compiles");
        let m = module::load(&c.module, &serial_profile).expect("FIFO fixture verifies");
        let rx = m.serial()[0];
        let wcet_ns = m.entries().iter().map(|e| e.cost).max().unwrap() * u64::from(serial_profile.op_ns);
        let byte_ns = rx.setting.byte_ns(serial_profile.clock_hz, serial_profile.serial.frame_bits);
        assert_eq!(rx.fifo as u64, wcet_ns.div_ceil(byte_ns) + 2, "waits={waits}: FIFO follows the module's verified cost");
        assert_eq!(rx.fifo, if waits { 629 } else { 3 }, "a 16-byte save needs about 628 entries; one read retains the small FIFO");
        let higher = Profile { max_entry_ops: 2_000_000, call_ops: None, ..serial_profile };
        assert_eq!(module::load(&c.module, &higher).unwrap().serial()[0].fifo, rx.fifo, "raising the profile maximum does not enlarge the FIFO");
        assert_eq!(notes(&c).len(), usize::from(waits), "the EEPROM note appears exactly when costs include waits");
        if waits {
            let serial_note = c.notes.iter().position(|n| n.starts_with("serial link")).unwrap();
            let eeprom_note = c.notes.iter().position(|n| n.starts_with("EEPROM:")).unwrap();
            assert_eq!(eeprom_note, serial_note + 1, "the wait note follows the serial timing note");
        }
    }
}

#[test]
fn constant_addresses_and_missing_eeprom_are_refused_at_the_call() {
    for (port, name, tail) in [(ports::EEPROM_READ, "eeprom_read", ""), (ports::EEPROM_WRITE, "eeprom_write", ", 1")] {
        for (addr, size, message) in [
            ("1024", 1024, format!("`{name}` address 1024 is outside 0..1024 (1024 bytes of EEPROM)")),
            ("-1", 1024, format!("`{name}` address -1 is outside 0..1024 (1024 bytes of EEPROM)")),
            ("0", 0, format!("`{name}` is unavailable: this chip has no EEPROM")),
            ("computed", 0, format!("`{name}` is unavailable: this chip has no EEPROM")),
        ] {
            let call = format!("{name}({addr}{tail})");
            let src = format!("var computed: i32 = 0\nfn setup() {{\n    {call}\n}}\n");
            let errors = compile(&src, &Profile { eeprom: size, ..P }, &[], &[]).err().expect("EEPROM misuse is refused");
            assert_eq!(errors.len(), 1, "{call}: one refusal");
            assert_eq!(errors[0].message, message, "{call}: refusal names the port and capacity problem");
            let start = src.find(&call).unwrap();
            assert_eq!((errors[0].start, errors[0].end), (start, start + call.len()), "{call}: refusal spans the exact call");
        }
        let src = format!("fn setup() {{ {name}(1024{tail}) }}");
        let c = compile(&src, &Profile { eeprom: 2048, ..P }, &[], &[]).expect("address fits the larger profile");
        assert!(matches!(module::load(&c.module, &P), Err(LoadError::EepromAddr { port: p, addr: 1024, size: 1024, .. }) if p == u32::from(port)), "{name}: loader independently checks constant addresses");
        assert!(matches!(module::load(&c.module, &Profile { eeprom: 0, eeprom_write_ns: 0, ..P }), Err(LoadError::EepromUnsupported { port: p, .. }) if p == u32::from(port)), "{name}: loader refuses missing EEPROM before checking its address");
    }
}

#[test]
fn shared_helpers_carry_eeprom_waits_into_entry_costs() {
    let source = "\
var n: i32 = 1
fn save(x: i32) -> i32 {
    eeprom_write(0, x)
    return eeprom_read(0)
}
fn nested(x: i32) -> i32 { return save(x) + save(x + 1) }
fn setup() { n = nested(n) }
let tick = every(ms(1), fn() { n = nested(n) })
";
    let profile = Profile { call_ops: Some(0), ram_words: 8192, ..P };
    let slow = compile(source, &profile, &[], &[]).unwrap();
    let fast = compile(source, &Profile { eeprom_write_ns: 0, ..profile }, &[], &[]).unwrap();
    assert!(slow.lowered.funcs.iter().any(|f| f.name == "save"));
    assert_eq!(slow.lowered.entries[0].cost - fast.lowered.entries[0].cost, 4 * (profile.eeprom_access_ops() - 1),
        "both calls of the nested helper carry both EEPROM access waits");
    module::load(&slow.module, &profile).unwrap();
    assert!(matches!(module::load(&fast.module, &profile), Err(LoadError::CostUnderstated { .. })), "loader independently propagates the waits through calls");
}
