use fab_elec_mcu::{compile, compile_kernel, Compiled, Profile};
use fab_elec_mcu_rt as rt;
use rt::{format::*, kernel::{call_export, CallError, KernelFault, KernelMemory, KernelPorts, RegionKind}, module::{self, LoadError, Module}, ports, sched::{Fault, Hal, Role}};

const P: Profile = Profile { id: 3, pins: 20, max_entry_ops: 50_000, call_ops: None, ram_words: 2048, buffer_bytes: 0, eeprom: 0, eeprom_write_ns: 0,
    f64: false, op_ns: 62, clock_hz: 16_000_000, analog: &rt::analog::NONE, pwm: &rt::pwm::NONE, serial: &rt::serial::NONE, motion: None, touch: false, audio: None, display: None };
// Test policy only; the language supplies no chip's interrupt-line policy.
const KERNEL_LINES: rt::KernelLines = rt::KernelLines { protected: (1 << 1) | (1 << 2) | (1 << 7), raise: 1 << 7 };

#[derive(Default)]
struct Host { faults: Vec<(u64, Fault)> }
impl Hal for Host {
    fn configure(&mut self, _: u64, _: u32, _: Role) { unreachable!("kernel has no pin declarations") }
    fn serial(&mut self, _: u64, _: u32, _: rt::serial::SerialSetting) { unreachable!("kernel has no serial declarations") }
    fn write(&mut self, _: u64, _: u32, _: bool, _: u32) { unreachable!("kernel has no pin ports") }
    fn shift(&mut self, _: u64, _: u32, _: u32, _: u32, _: u8, _: u32) { unreachable!("kernel has no shift port") }
    fn duty(&mut self, _: u64, _: u32, _: u8, _: u32) { unreachable!("kernel has no PWM ports") }
    fn read(&mut self, _: u64, _: u32, _: u32) -> bool { unreachable!("kernel has no pin ports") }
    fn adc(&mut self, _: u64, _: u32, _: u32) -> u16 { unreachable!("kernel has no ADC ports") }
    fn eth_send(&mut self, _: u64, _: &[u8], _: u32) -> bool { unreachable!("kernel has no eth ports") }
    fn eeprom_read(&mut self, _: u64, _: u16, _: u32) -> u8 { unreachable!("kernel has no EEPROM ports") }
    fn eeprom_write(&mut self, _: u64, _: u16, _: u8, _: u32) { unreachable!("kernel has no EEPROM ports") }
    fn eth_mac(&mut self, _: u64, _: u32) -> [u8; 6] { unreachable!("kernel has no eth ports") }
    fn clock(&mut self, t: u64, _: u32) -> u64 { t }
    fn fault(&mut self, t: u64, fault: Fault) { self.faults.push((t, fault)); }
}

struct Run<'a> { module: Module<'a>, state: Vec<u32>, scratch: Vec<u32> }
impl<'a> Run<'a> {
    fn new(c: &'a Compiled) -> Self {
        let module = module::load_kernel(&c.module, &P, &KERNEL_LINES).expect("compiled kernel verifies");
        let state = module.state_init().to_vec();
        let scratch = vec![0; (0..module.entries().len()).map(|k| module.scratch_words(k)).max().unwrap_or(0)];
        Self { module, state, scratch }
    }
    fn call(&mut self, name: &str, args: &[i32], ports: &mut KernelPorts<'_>) -> i32 {
        self.try_call(name, args, ports)
            .unwrap_or_else(|e| panic!("export {name}: {e:?}"))
    }
    fn try_call(&mut self, name: &str, args: &[i32], ports: &mut KernelPorts<'_>) -> Result<i32, CallError> {
        ports.fault = None;
        ports.stop = None;
        call_export(&self.module, name, args, &mut self.state, &mut self.scratch, ports)
    }
}

fn compiled(source: &str) -> Compiled {
    compile_kernel(source, &P, &KERNEL_LINES, &[]).unwrap_or_else(|e| panic!("kernel source: {e:?}"))
}

#[test]
fn exports_and_all_kernel_ports() {
    let c = compiled(r#"
let zero = export(fn() { return 0 })
let max_bytes: i32 = 2 + 2
let six = export(fn(a, b, c, d, e, f) { return a + b*2 + c*3 + d*4 + e*5 + f*6 })
let memory = export(fn(a, v) {
    store8(a, v); store16(a + 2, v); store32(a + 4, v)
    return load8u(a) + load16u(a + 2) + load32(a + 4)
})
let move_bytes = export(fn(dst, src, n) { copy_raw(dst, src, n, max_bytes) })
let paint = export(fn(dst, byte, n) { fill_raw(dst, byte, n, 4) })
let zeros = export(fn(x) { return clz(x) })
let enable = export(fn(mask) { irq_enable(mask) })
let disable = export(fn(mask) { irq_disable(mask) })
let raise = export(fn() { irq_raise(7) })
let time = export(fn(addr) { now64(addr) })
let compares = export(fn(a, b) {
    let flags: i32 = 0
    if ltu(a, b) { flags += 1 }; if leu(a, b) { flags += 2 }
    if gtu(a, b) { flags += 4 }; if geu(a, b) { flags += 8 }
    if before(a, b) { flags += 16 }
    return flags
})
"#);
    let mut run = Run::new(&c);
    let mut host = Host::default();
    let mut memory = KernelMemory::new();
    memory.add_region(0x1000, vec![0; 32], true, RegionKind::Kernel).expect("mapped memory");
    memory.add_region(0x2000, vec![0; 4], true, RegionKind::Kernel).expect("second region");
    let mut io = KernelPorts::new(memory, KERNEL_LINES, &mut host);
    assert_eq!(run.call("zero", &[], &mut io), 0, "zero-parameter export");
    assert_eq!(run.call("six", &[1, 2, 3, 4, 5, 6], &mut io), 91, "all six fixed parameter slots");
    let v = 0x1234_abcd_i32;
    assert_eq!(run.call("memory", &[0x1000, v], &mut io), v + 0xabcd + 0xcd, "loads zero-extend at every width");
    assert_eq!(io.memory.bytes(0x1000, 8).unwrap(), &[0xcd, 0, 0xcd, 0xab, 0xcd, 0xab, 0x34, 0x12], "stores truncate and use little endian");
    assert_eq!(run.call("memory", &[0x1000, -1], &mut io), 0xff + 0xffff - 1, "byte/half-word stay unsigned while a full word preserves signed bits");

    for (dst, src, expected) in [(0x1001, 0x1000, [1, 1, 2, 3, 4]), (0x1000, 0x1001, [2, 3, 4, 5, 5])] {
        io.memory.bytes_mut(0x1000, 5).unwrap().copy_from_slice(&[1, 2, 3, 4, 5]);
        run.call("move_bytes", &[dst, src, 0], &mut io);
        assert_eq!(io.memory.bytes(0x1000, 5).unwrap(), &[1, 2, 3, 4, 5], "copy n=0, direction {dst:#x}/{src:#x}");
        run.call("move_bytes", &[dst, src, 4], &mut io);
        assert_eq!(io.memory.bytes(0x1000, 5).unwrap(), &expected, "overlap-safe copy n=max, direction {dst:#x}/{src:#x}");
    }
    run.call("move_bytes", &[0x2000, 0x1000, 4], &mut io);
    assert_eq!(io.memory.bytes(0x2000, 4).unwrap(), &[2, 3, 4, 5], "copy across regions");
    run.call("paint", &[0x2000, 0x1ab, 4], &mut io);
    assert_eq!(io.memory.bytes(0x2000, 4).unwrap(), &[0xab; 4], "fill n=max uses low byte");
    run.call("paint", &[0x2000, 0, 0], &mut io);
    assert_eq!(io.memory.bytes(0x2000, 4).unwrap(), &[0xab; 4], "fill n=0 writes nothing");
    for (x, expected) in [(0, 32), (1, 31), (i32::MIN, 0)] {
        assert_eq!(run.call("zeros", &[x], &mut io), expected, "clz({x})");
    }
    io.intenable = (1 << 1) | (1 << 5); // protected bits 2 and 7 start clear.
    run.call("enable", &[-1], &mut io);
    assert_eq!(io.intenable, !KERNEL_LINES.protected | (1 << 1), "enable preserves both set and clear protected bits");
    run.call("disable", &[-1], &mut io);
    assert_eq!(io.intenable, 1 << 1, "disable preserves the protected set bit");
    run.call("raise", &[], &mut io);
    assert_eq!(io.raised, [7], "only the admitted yield line is raised");
    io.clock = 0xfedc_ba98_7654_3210;
    run.call("time", &[0x1010], &mut io);
    assert_eq!(io.memory.bytes(0x1010, 8).unwrap(), &[0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc, 0xfe], "now64 stores low word then high word from one snapshot");
    for a in [0i32, i32::MAX, i32::MIN, -1] {
        for b in [0i32, i32::MAX, i32::MIN, -1] {
            let (au, bu) = (a as u32, b as u32);
            let expected = i32::from(au < bu) | (i32::from(au <= bu) << 1) | (i32::from(au > bu) << 2)
                | (i32::from(au >= bu) << 3) | (i32::from(a.wrapping_sub(b) < 0) << 4);
            assert_eq!(run.call("compares", &[a, b], &mut io), expected, "unsigned and wrapping-time comparisons at {a}/{b}");
        }
    }
    let before = run.state.clone();
    assert_eq!(call_export(&run.module, "six", &[1], &mut run.state, &mut run.scratch, &mut io),
        Err(CallError::Arguments { expected: 6, found: 1 }), "wrong call arity is refused");
    assert_eq!(run.state, before, "bad call does not touch any slot");
    drop(io);
    assert!(host.faults.is_empty(), "valid ports emit no faults: {:?}", host.faults);
}

#[test]
fn access_ports_check_region_kind_and_writability() {
    let c = compiled(r#"
let check_load8u = export(fn(a) { return load8u(a) })
let check_load16u = export(fn(a) { return load16u(a) })
let check_load32 = export(fn(a) { return load32(a) })
let check_store8 = export(fn(a) { store8(a, 1) })
let check_store16 = export(fn(a) { store16(a, 1) })
let check_store32 = export(fn(a) { store32(a, 1) })
let check_copy_raw = export(fn(dst, src) { copy_raw(dst, src, 1, 1) })
let check_fill_raw = export(fn(a) { fill_raw(a, 1, 1, 1) })
let check_now64 = export(fn(a) { now64(a) })
let check_pack_load32 = export(fn(a) { return pack_load32(a) })
let check_pack_store32 = export(fn(a) { pack_store32(a, 1) })
let check_pack_store8 = export(fn(a) { pack_store8(a, 1) })
let check_pack_copy = export(fn(dst, src) { pack_copy(dst, src, 1, 1) })
let check_pack_fill = export(fn(a) { pack_fill(a, 1, 1, 1) })
let check_dev_load32 = export(fn(a) { return dev_load32(a) })
let check_dev_store32 = export(fn(a) { dev_store32(a, 1) })
"#);
    const KERNEL_RW: i32 = 0x1000;
    let regions = [
        ("Kernel rw", KERNEL_RW, RegionKind::Kernel, true),
        ("Pack rw",   0x2000,    RegionKind::Pack,   true),
        ("Pack ro",   0x3000,    RegionKind::Pack,   false),
        ("Device rw", 0x4000,    RegionKind::Device, true),
        ("Device ro", 0x5000,    RegionKind::Device, false),
    ];
    let mut memory = KernelMemory::new();
    for (_, base, kind, writable) in regions {
        memory.add_region(base as u32, vec![0; 8], writable, kind).unwrap();
    }
    let mut run = Run::new(&c);
    let mut host = Host::default();
    let mut io = KernelPorts::new(memory, KERNEL_LINES, &mut host);

    #[derive(Debug)]
    enum Operand { Address, Source, Destination }
    use Operand::{Address, Source, Destination};
    // Columns follow the five fixture regions above. For copies the other operand is Kernel rw.
    for (port, operand, expected) in [
        ("load8u",       Address,     [true,  false, false, false, false]),
        ("load16u",      Address,     [true,  false, false, false, false]),
        ("load32",       Address,     [true,  false, false, false, false]),
        ("store8",       Address,     [true,  false, false, false, false]),
        ("store16",      Address,     [true,  false, false, false, false]),
        ("store32",      Address,     [true,  false, false, false, false]),
        ("copy_raw",     Source,      [true,  false, false, false, false]),
        ("copy_raw",     Destination, [true,  false, false, false, false]),
        ("fill_raw",     Address,     [true,  false, false, false, false]),
        ("now64",        Address,     [true,  false, false, false, false]),
        ("pack_load32",  Address,     [true,  true,  true,  false, false]),
        ("pack_store32", Address,     [true,  true,  false, false, false]),
        ("pack_store8",  Address,     [true,  true,  false, false, false]),
        ("pack_copy",    Source,      [true,  true,  true,  false, false]),
        ("pack_copy",    Destination, [true,  true,  false, false, false]),
        ("pack_fill",    Address,     [true,  true,  false, false, false]),
        ("dev_load32",   Address,     [false, false, false, true,  true ]),
        ("dev_store32",  Address,     [false, false, false, true,  false]),
    ] {
        let export = format!("check_{port}");
        for ((region, addr, _, _), allowed) in regions.into_iter().zip(expected) {
            let result = match operand {
                Address => run.try_call(&export, &[addr], &mut io),
                Source => run.try_call(&export, &[KERNEL_RW, addr], &mut io),
                Destination => run.try_call(&export, &[addr, KERNEL_RW], &mut io),
            };
            let reached = match (result, io.fault) {
                (Ok(_), None) => true,
                (Err(CallError::Faulted), Some(Fault::Kernel { kind: KernelFault::OutOfRegion, .. })) => false,
                other => panic!("{port} {operand:?} in {region}: unexpected result {other:?}"),
            };
            assert_eq!(reached, allowed, "{port} {operand:?} in {region}");
        }
    }
}

#[test]
fn return_normalization_preserves_ownership_and_zero_defaults() {
    let c = compiled(r#"
fn helper(x: i32) { return x + 2 }
fn zero_helper() { return 0 }
let helper_zero = export(fn() { return zero_helper() })
let literal = export(fn() { return 0x7fff_ffff })
let bits = export(fn() { return 0x8000_0000 })
let branches = export(fn(x) { if x == 0 { return -1 } else { return helper(x) } })
let in_loop = export(fn(x) { for i in 0..3 { if i == x { return i + 10 } }; return 99 })
let empty = export(fn(x) { if x != 0 { return 7 } })
let bare = export(fn(x) { if x != 0 { return 8 }; return })
let nested = export(fn(x) { let local = fn(v: i32) { return v + 3 }; return local(x) })
let blocks = export(fn(x) { { if x != 0 { return 12 } }; return 13 })
let matching = export(fn(x) { match x { 0 => { return 14 }, _ => { return 15 } } })
"#);
    let mut run = Run::new(&c);
    let mut host = Host::default();
    let mut io = KernelPorts::new(KernelMemory::new(), KERNEL_LINES, &mut host);
    assert_eq!(run.call("helper_zero", &[], &mut io), 0, "an unannotated helper's bare return is i32");
    let program = &c.lowered.entries.iter().find(|e| e.name == "helper_zero").expect("helper export exists").program;
    assert!(!program.vars.is_empty() && program.vars.iter().chain(&program.vals).all(|t| *t == makepad_script_compute::ir::Ty::I32), "the helper's return slots and values are all i32");
    assert_eq!(run.call("literal", &[], &mut io), i32::MAX, "literal return avoids f32 rounding");
    assert_eq!(run.call("bits", &[], &mut io), i32::MIN, "hex sign bit lexes and returns as i32");
    for (name, cases) in [
        ("branches", [(0, -1), (1, 3)]), ("in_loop", [(1, 11), (9, 99)]),
        ("empty", [(1, 7), (0, 0)]), ("bare", [(1, 8), (0, 0)]),
        ("nested", [(0, 3), (1, 4)]),
        ("blocks", [(1, 12), (0, 13)]), ("matching", [(0, 14), (1, 15)]),
    ] {
        for (x, expected) in cases { assert_eq!(run.call(name, &[x], &mut io), expected, "{name}({x}): return semantics and ownership"); }
    }
    for (value, at, message) in [
        ("1.5", "1.5", "this program has no float type: write an integer literal"),
        ("float(1)", "return float(1)", "expected i32"),
    ] {
        let src = format!("let bad = export(fn() {{ return {value} }})");
        let errors = match compile_kernel(&src, &P, &KERNEL_LINES, &[]) { Err(e) => e, Ok(_) => panic!("f32 return must fail: {src}") };
        let start = src.find(at).unwrap();
        assert_eq!((errors[0].start, errors[0].end), (start, start + at.len()), "{value}: refusal names the literal or the typed return");
        assert!(errors[0].message.contains(message), "{value}: f32 refusal names the required type: {:?}", errors);
    }
}

#[test]
fn faults_use_hal_and_leave_memory_unchanged() {
    let source = r#"
let read_bad = export(fn(addr) { return load32(addr) })
let write_bad = export(fn(addr) { store16(addr, 0x1234) })
let copy_bad = export(fn(n) { copy_raw(0x1000, 0x1004, n, 4) })
let fill_bad = export(fn(n) { fill_raw(0x1000, 0, n, 4) })
let copy_out = export(fn() { copy_raw(0x1000, 0x1006, 4, 4) })
let time_bad = export(fn(addr) { now64(addr) })
let capped = export(fn(x) { while x >= 0 { x += 1 }; return x })
"#;
    let c = compiled(source);
    let mut run = Run::new(&c);
    let mut host = Host::default();
    let original = [0xa5; 8];
    let mut memory = KernelMemory::new();
    memory.add_region(0x1000, original.to_vec(), true, RegionKind::Kernel).unwrap();
    assert_eq!(memory.bytes(u32::MAX, 4), Err(KernelFault::OutOfRegion), "wrapping access is a refusal, never a panic");
    let mut io = KernelPorts::new(memory, KERNEL_LINES, &mut host);
    io.clock = 123;
    let cases = [
        ("read_bad", 0x1001, KernelFault::Misaligned, "load32(addr)"),
        ("write_bad", 0x1001, KernelFault::Misaligned, "store16(addr, 0x1234)"),
        ("read_bad", 0x2000, KernelFault::OutOfRegion, "load32(addr)"),
        ("write_bad", 0x2000, KernelFault::OutOfRegion, "store16(addr, 0x1234)"),
        ("copy_bad", 5, KernelFault::CopyBound, "copy_raw(0x1000, 0x1004, n, 4)"),
        ("copy_bad", -1, KernelFault::CopyBound, "copy_raw(0x1000, 0x1004, n, 4)"),
        ("fill_bad", 5, KernelFault::FillBound, "fill_raw(0x1000, 0, n, 4)"),
        ("fill_bad", -1, KernelFault::FillBound, "fill_raw(0x1000, 0, n, 4)"),
        ("time_bad", 0x1001, KernelFault::Misaligned, "now64(addr)"),
        ("time_bad", 0x1004, KernelFault::OutOfRegion, "now64(addr)"),
    ];
    for (name, arg, kind, _) in cases {
        assert_eq!(run.try_call(name, &[arg], &mut io), Err(CallError::Faulted), "{name}({arg}): fault stops the export");
        assert!(matches!(io.fault, Some(Fault::Kernel { kind: got, .. }) if got == kind), "{name}({arg}): caller can read the named fault");
        assert_eq!(io.memory.bytes(0x1000, 8).unwrap(), &original, "{name}({arg}): fault writes nothing, including partial snapshots");
    }
    assert_eq!(run.try_call("copy_out", &[], &mut io), Err(CallError::Faulted), "copy source fault stops the export");
    assert_eq!(io.memory.bytes(0x1000, 8).unwrap(), &original, "copy validates both complete ranges before writing");
    assert_eq!(run.try_call("capped", &[0], &mut io), Err(CallError::Faulted), "loop cap stops the export before its return");
    assert!(matches!(io.fault, Some(Fault::LoopCap { .. })), "loop cap uses the shared fault variant");
    assert_eq!(io.intenable, 0, "faults do not alter IRQ enables");
    assert!(io.raised.is_empty(), "faults do not raise IRQs");
    drop(io);
    assert_eq!(host.faults.len(), cases.len() + 2, "one named fault per refused operation, plus loop cap");
    for ((t, fault), (_, _, kind, call)) in host.faults.iter().zip(cases) {
        let Fault::Kernel { kind: got, site } = *fault else { panic!("expected named kernel fault: {fault:?}"); };
        assert_eq!((*t, got), (123, kind), "{call}: shared HAL fault channel and clock");
        let s = c.sites[site as usize];
        assert_eq!(&source[s.start as usize..s.end as usize], call, "{call}: fault retains the user's call site");
    }
    assert!(matches!(host.faults[cases.len()].1, Fault::Kernel { kind: KernelFault::OutOfRegion, .. }), "copy source range gets a named fault");
    let Fault::LoopCap { site } = host.faults.last().unwrap().1 else { panic!("kernel cap must use the existing LoopCap variant"); };
    assert!(source[c.sites[site as usize].start as usize..].starts_with("while"), "cap names the loop's source site");
}

#[test]
fn reports_use_hal_and_continue() {
    let source = "let limit = export(fn(n) { report(96, n); store32(0x1000, n); return n })";
    let c = compiled(source);
    let mut run = Run::new(&c);
    let mut host = Host::default();
    let mut memory = KernelMemory::new();
    memory.add_region(0x1000, vec![0; 4], true, RegionKind::Kernel).unwrap();
    let mut io = KernelPorts::new(memory, KERNEL_LINES, &mut host);
    io.clock = 123;
    assert_eq!(run.call("limit", &[7], &mut io), 7, "a report does not suspend the export");
    assert_eq!(io.memory.bytes(0x1000, 4).unwrap(), &[7, 0, 0, 0], "the store after the report still runs");
    assert_eq!(io.fault, None, "a report does not set the last fatal fault");
    drop(io);
    assert_eq!(host.faults.len(), 1, "one report through the HAL fault channel");
    let (clock, fault) = host.faults[0];
    let Fault::KernelReport { code, site, arg } = fault else { panic!("expected a non-fatal kernel report: {fault:?}"); };
    assert_eq!((clock, code, arg), (123, rt::watch::FAULT_KERNEL_TIMER_REFUSED, 7), "report preserves clock, code and capacity");
    assert_eq!(rt::watch::Event::from_fault(fault), rt::watch::Event::Fault { code, site: site as u16, arg: 7 }, "report uses the Fault watch event");
    let s = c.sites[site as usize];
    assert_eq!(&source[s.start as usize..s.end as usize], "report(96, n)", "report retains the user's call site");
}

#[test]
fn radio_stop_suspends_the_export_without_a_hal_fault() {
    let c = compiled("let halt = export(fn() { store32(0x1000, 1); radio_stop(107); store32(0x1000, 2) })");
    let mut run = Run::new(&c);
    let mut host = Host::default();
    let mut memory = KernelMemory::new();
    memory.add_region(0x1000, vec![0; 4], true, RegionKind::Kernel).unwrap();
    let mut io = KernelPorts::new(memory, KERNEL_LINES, &mut host);
    assert_eq!(run.try_call("halt", &[], &mut io), Err(CallError::Faulted));
    assert_eq!(io.memory.bytes(0x1000, 4).unwrap(), &1u32.to_le_bytes(), "only the store before the stop ran");
    assert_eq!((io.stop, io.fault), (Some(107), None));
    assert!(host.faults.is_empty());
}

#[test]
fn frontend_and_verifier_refusals_name_the_property() {
    for (source, message) in [
        ("let bad = export(fn(n) { copy_raw(0, 0, 0, n) })", "max must be a compile-time constant >= 0"),
        ("let bad = export(fn(n) { fill_raw(0, 0, 0, n) })", "max must be a compile-time constant >= 0"),
        ("let bad = export(fn() { copy_raw(0, 0, 0, -1) })", "max must be a compile-time constant >= 0"),
        ("let bad = export(fn() { fill_raw(0, 0, 0, -1) })", "max must be a compile-time constant >= 0"),
        ("let bad = export(fn(n) { pack_copy(0, 0, 0, n) })", "`pack_copy` max must be a compile-time constant >= 0"),
        ("let bad = export(fn(n) { pack_fill(0, 0, 0, n) })", "`pack_fill` max must be a compile-time constant >= 0"),
        ("let bad = export(fn() { pack_copy(0, 0, 0, -1) })", "`pack_copy` max must be a compile-time constant >= 0"),
        ("let bad = export(fn() { pack_fill(0, 0, 0, -1) })", "`pack_fill` max must be a compile-time constant >= 0"),
        ("let bad = export(fn(n) { irq_raise(n) })", "line must be a compile-time constant in KernelLines::raise"),
        ("let bad = export(fn() { irq_raise(8) })", "line must be a compile-time constant in KernelLines::raise"),
        ("let bad = export(fn() { irq_raise(32) })", "line must be a compile-time constant in KernelLines::raise"),
        ("let bad = export(fn() { irq_raise(-1) })", "line must be a compile-time constant in KernelLines::raise"),
        ("let bad = export(fn(n) { report(n, 7) })", "`report` code must be a compile-time constant in the kernel report block (96..=127)"),
        ("let bad = export(fn() { report(64, 7) })", "`report` code must be a compile-time constant in the kernel report block (96..=127)"),
        ("let bad = export(fn(n) { radio_stop(n) })", "`radio_stop` cause must be a compile-time constant in the kernel report block (96..=127)"),
        ("let bad = export(fn() { radio_stop(64) })", "`radio_stop` cause must be a compile-time constant in the kernel report block (96..=127)"),
        ("let bad = export(fn() { radio_stop(0) })", "`radio_stop` cause must be a compile-time constant in the kernel report block (96..=127)"),
        ("let bad = export(fn(a,b,c,d,e,f,g) {})", "at most 6 i32 parameters"),
        ("let bad = export(fn(a: f32) {})", "parameters must all be i32"),
        ("let bad = export(fn() { pin_write(0, 1) })", "unknown function `pin_write`"),
        ("let bad = every(1, fn() {})", "unknown function `every`"),
        ("let bad = export(fn() {}); let bad = export(fn() {})", "two exports cannot share"),
        ("fn setup() {}; let ok = export(fn() {})", "cannot declare a setup fn"),
        ("mono()\nlet ok = export(fn() {})", "unknown declaration `mono()`"),
        ("let bad = export(fn(a) { a + 1 })", "result is `return e`"),
        ("let bad = export(fn(a) { if a > 0 { 1 } else { return 2 } })", "result is `return e`"),
    ] {
        let errors = match compile_kernel(source, &P, &KERNEL_LINES, &[]) { Err(e) => e, Ok(_) => panic!("must refuse {source}") };
        assert!(errors[0].message.contains(message), "{source}: expected {message:?}, got {:?}", errors);
        if message.contains("compile-time") {
            assert!(source[errors[0].start..errors[0].end].contains('('), "{source}: verifier error maps to a user call span");
        }
        if let Some(start) = source.find("report(").or_else(|| source.find("radio_stop(")) {
            let end = start + source[start..].find(')').unwrap() + 1;
            assert_eq!((errors[0].start, errors[0].end), (start, end), "{source}: refusal names the exact call span");
            assert_eq!(errors[0].message, message, "{source}: refusal names the required constant block");
        }
    }
    // A top-level declaration named like a layout constant is refused at its span.
    let source = "LIMIT()\nlet ok = export(fn() {})";
    let errors = match compile_kernel(source, &P, &KERNEL_LINES, &[("LIMIT", 4)]) { Err(e) => e, Ok(_) => panic!("must refuse {source}") };
    assert_eq!(errors[0].message, "kernel layout constant `LIMIT` cannot be redeclared", "a declaration cannot redeclare a layout constant");
    assert_eq!((errors[0].start, errors[0].end), (0, "LIMIT()".len()), "the refusal names the declaration");
    for (source, name) in [("let bad = export(fn() {})", "export"), ("fn setup() { load8u(0) }", "load8u")] {
        let errors = match compile(source, &P, &[], &[]) { Err(e) => e, Ok(_) => panic!("app cannot name {name}") };
        assert!(errors[0].message.contains(&format!("unknown function `{name}`")), "app {name} stays unknown: {:?}", errors);
    }
}

#[test]
fn valued_call_tails_are_refused_at_the_call_and_unit_tails_remain_valid() {
    for (source, tail) in [
        ("let bad = export(fn(a, b) { ltu(a, b) })", "ltu(a, b)"),
        ("fn h(x: i32) { return x }; let bad = export(fn(a) { h(a) })", "h(a)"),
        ("let bad = export(fn(a) { let f = fn(v: i32) { return v }; f(a) })", "f(a)"),
        ("let bad = export(fn(a) { load32(a) })", "load32(a)"),
        ("fn h(x: i32) { g(x) }; fn g(x: i32) { leaf(x) }; fn leaf(x: i32) { return x }; let bad = export(fn(a) { h(a) })", "h(a)"),
        ("let bad = export(fn(a) { let f = fn(v: i32) { load32(v) }; let g = fn(v: i32) { f(v) }; g(a) })", "g(a)"),
        ("let bad = export(fn(a) { let f = fn(v: i32) { let g = fn(w: i32) { return w }; g(v) }; f(a) })", "f(a)"),
        ("let bad = export(fn(a) { if a > 0 { let f = fn(v: i32) { return v }; f(a) } else { return 0 } })", "f(a)"),
        ("let bad = export(fn(a) { match a { 0 => { return 0 }, _ => { load32(a) } } })", "load32(a)"),
        ("let bad = export(fn(a) { { let f = fn(v: i32) { return v }; f(a) } })", "f(a)"),
        ("fn h(x: i32) { while x > 0 { return x }; return }; let bad = export(fn(a) { h(a) })", "h(a)"),
    ] {
        let errors = match compile_kernel(source, &P, &KERNEL_LINES, &[]) {
            Err(e) => e, Ok(_) => panic!("valued tail must be refused: {source}"),
        };
        assert_eq!(errors[0].message, "an export's result is `return e`; this trailing value would be discarded", "{source}: valued tail diagnostic");
        let start = source.rfind(tail).unwrap();
        assert_eq!((errors[0].start, errors[0].end), (start, start + tail.len()), "{source}: error names the user's valued call");
    }
    compiled(r#"
fn unit(a: i32, b: i32) { store32(a, b); return }
fn local_return(a: i32, b: i32) { let f = fn(v: i32) { return v }; store32(a, b) }
fn ltu(a: i32, b: i32) { store32(a, b) }
let store_tail = export(fn(a, b) { store32(a, b) })
let unit_tail = export(fn(a, b) { unit(a, b) })
let own_returns = export(fn(a, b) { local_return(a, b) })
let prelude_shadow = export(fn(a, b) { ltu(a, b) })
let valued_local = export(fn(a) { let f = fn(v: i32) { return v }; return f(a) })
let unit_local = export(fn(a, b) { let f = fn(v: i32, w: i32) { store32(v, w) }; f(a, b) })
let scoped_local = export(fn(a, b) { if a == 0 { let unit = fn(v: i32) { return v }; return unit(a) }; unit(a, b) })
let scalar_shadow = export(fn(a, b) { let unit = fn(v: i32) { return v }; { let unit: i32 = 0; unit(a, b) } })
"#);
}

// Assemble raw words independently of lang/encode. The tiny valid kernel
// declares gate(p0) and a one-statement loop-cap entry. In app mode it instead
// has an ordinary setup entry and (optionally) an output declaration.
fn raw_module(kernel: bool, form: Option<u32>, port: u16, param_sig: u32) -> Vec<u32> {
    let mut strings = Vec::new();
    for s in [if kernel { "gate" } else { "setup" }, "gate#p0", "gate#r", "i32", "f32"] {
        strings.push(s.len() as u32);
        for chunk in s.as_bytes().chunks(4) { let mut b = [0; 4]; b[..chunk.len()].copy_from_slice(chunk); strings.push(u32::from_le_bytes(b)); }
    }
    let decl = match (kernel, form) {
        (true, Some(form)) => vec![form, 0, 0, 2, ARG_ENTRY, 0, ARG_INT, 1],
        (false, Some(form)) => vec![form, 0, 0, 1, ARG_INT, 0],
        (_, None) => vec![],
    };
    let code = vec![0, 0, 0, 0, 5, OP_PORT, u32::from(port), 0, 0, 0];
    let sections = [
        vec![2, 0, 1, 5, 1, u32::from(form.is_some()), 2, 0], vec![0, 0], vec![], strings,
        vec![1, param_sig, 0, 1, 2, 3, 1, 1], decl, vec![0, 0, code.len() as u32, 1, 0], code,
    ];
    let mut words = vec![0; HEADER_WORDS + SECTION_COUNT * DIRECTORY_WORDS];
    words[H_MAGIC] = MAGIC; words[H_VERSION] = FORMAT_VERSION;
    words[H_PORT_VERSION] = ports::PORT_TABLE_VERSION; words[H_PROFILE] = P.id;
    words[H_SECTIONS] = SECTION_COUNT as u32;
    for (k, section) in sections.into_iter().enumerate() {
        let at = HEADER_WORDS + k * DIRECTORY_WORDS;
        let offset = words.len() as u32;
        words[at..at + 3].copy_from_slice(&[k as u32 + 1, offset, section.len() as u32]);
        words.extend(section);
    }
    words[H_WORDS] = words.len() as u32;
    words
}

fn section_start(words: &[u32], kind: u32) -> usize {
    words[HEADER_WORDS..HEADER_WORDS + words[H_SECTIONS] as usize * DIRECTORY_WORDS]
        .chunks_exact(DIRECTORY_WORDS).find(|d| d[0] == kind).expect("fixture section")[1] as usize
}

#[test]
fn raw_words_cannot_cross_domains_or_forge_export_slots() {
    let good = raw_module(true, Some(64), 77, 3);
    module::load_kernel(&good, &P, &KERNEL_LINES).expect("hand-built control is valid");
    let bad = raw_module(true, Some(64), 77, 4);
    let e = module::load_kernel(&bad, &P, &KERNEL_LINES).unwrap_err();
    assert_eq!(e, LoadError::ExportSlot { decl: 0, param: Some(0) }, "loader independently checks p0's type");
    assert!(e.to_string().contains("one-word i32 state slot NAME#p0"), "slot refusal names the contract");
    let vars = section_start(&good, SEC_VARS);
    for (word, value, property) in [(0, 2, "missing p0"), (3, 0, "empty p0"), (3, 2, "two-word p0"), (2, 1, "aliased p0")] {
        let mut bad = good.clone();
        bad[vars + word] = value;
        assert!(matches!(module::load_kernel(&bad, &P, &KERNEL_LINES), Err(LoadError::ExportSlot { param: Some(0), .. })), "loader rejects {property}");
    }
    let mut bad = good.clone();
    bad[vars + VAR_WORDS + 1] = 4;
    assert!(matches!(module::load_kernel(&bad, &P, &KERNEL_LINES), Err(LoadError::ExportSlot { param: None, .. })), "loader checks the result slot too");
    let decl = section_start(&good, SEC_DECLS);
    for n in [-1i32, 7] {
        let mut bad = good.clone();
        bad[decl + DECL_HEADER_WORDS + 3] = n as u32;
        assert!(matches!(module::load_kernel(&bad, &P, &KERNEL_LINES), Err(LoadError::ExportArgs { .. })), "loader refuses parameter count {n}");
    }
    let unbound = raw_module(true, None, 77, 3);
    assert!(matches!(module::load_kernel(&unbound, &P, &KERNEL_LINES), Err(LoadError::KernelEntry { .. })), "every kernel entry must belong to an export");
    let pair = compiled("let first = export(fn() {}); let second = export(fn() {})");
    let mut duplicate = pair.module.clone();
    let decl = section_start(&duplicate, SEC_DECLS);
    duplicate[decl + DECL_HEADER_WORDS + 4 + 2] = duplicate[decl + 2];
    assert_eq!(module::load_kernel(&duplicate, &P, &KERNEL_LINES).unwrap_err(), LoadError::DuplicateExport { decl: 1 }, "loader independently rejects duplicate export names");
    for (kernel, form, port, expected) in [
        (false, None, 64, "port 64 is not allowed in an app module"),
        (false, Some(64), 7, "form 64 is not allowed in an app module"),
        (true, Some(64), 7, "port 7 is not allowed in a kernel module"),
        (true, Some(0), 77, "form 0 is not allowed in a kernel module"),
    ] {
        let words = raw_module(kernel, form, port, 3);
        let e = if kernel { module::load_kernel(&words, &P, &KERNEL_LINES) } else { module::load(&words, &P) }.unwrap_err();
        assert_eq!(e.to_string(), expected, "domain refusal must identify the number and domain");
    }
    let c = compiled("let yield_line = export(fn() { irq_raise(7) })");
    let restricted = rt::KernelLines { raise: 0, ..KERNEL_LINES };
    assert!(matches!(module::load_kernel(&c.module, &P, &restricted), Err(LoadError::KernelRaise { raise: 0, .. })), "loader uses the supplied image policy");
    let source = "let denied = export(fn() { irq_raise(7) })";
    let errors = match compile_kernel(source, &P, &restricted, &[]) { Err(e) => e, Ok(_) => panic!("compiler must enforce the caller's raise policy") };
    assert!(errors[0].message.contains("KernelLines::raise (0x00000000)"), "compiler passes the supplied policy to the loader");
    assert_eq!(&source[errors[0].start..errors[0].end], "irq_raise(7)", "policy refusal names the user's call");
    let alternate = rt::KernelLines { protected: 1 << 3, raise: 1 << 5 };
    let c = compile_kernel("let allowed = export(fn() { irq_raise(5) })", &P, &alternate, &[]).expect("caller chooses a different line");
    assert_eq!(module::load_kernel(&c.module, &P, &alternate).unwrap().kernel_lines(), Some(alternate), "policy has no language-side default");
}
