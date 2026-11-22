//! Flat-memory differential against rt::kernel. No ESP32 machine, Wi-Fi
//! library, host clock or external fixtures are involved.
use std::collections::BTreeMap;
use fab_elec_mcu::{compile, compile_kernel};
use fab_elec_mcu_emit_s3::{build_handlers, build_kernel, chip, radio::{fault::*, kernel::{RTC_STORE1, SYSCON_WIFI_CLK_EN}, REPORT_RING_ENTRIES},
    EntryKind, KernelCode, KernelOptions, KernelRegion, Options, RADIO_KERNEL_LINES};
use fab_elec_mcu_emu::bus::{Bus, Kind, Memory, Perm, Width};
use fab_elec_mcu_rt::{self as rt, kernel::{call_export, AccessClass, CallError, KernelFault, KernelMemory, KernelPorts, RegionKind},
    module::{self, Module}, ports::*, sched::{Fault, Hal, Role}, watch::Event};
use fab_elec_mcu_xtensa::{insn::{decode, Op}, sim::{Config, Cpu, IntType, Stop, Unaligned, Vectors}};

const BASE: u32 = 0x1000;
const RETURN: u32 = 0x100;
const STATE: u32 = 0x18000;
const SHARED: u32 = 0x19000;
const T0: u32 = 0x1a000;
const REPORTS: u32 = 0x1b000;
const STOP_WORD: u32 = 0x1c000;
const STACK: u32 = 0x20000;
const TIMER: u32 = 0x6002_3000;
const SNAPSHOT: u64 = 0xfff5_4321_0000_0010;
const BOOT_TIME: u64 = 0x123_0000_0020; // exercises the low-word borrow
const MAX: u32 = 8;
const PACK_BUFFER: u32 = 0x70000;

fn options() -> KernelOptions {
    KernelOptions { base: BASE, state: STATE, t0: T0, reports: Some(REPORTS), stop_pending: Some(STOP_WORD), regions: vec![
        KernelRegion { base: 0x30000, len: 32, writable: true, kind: RegionKind::Kernel },
        KernelRegion { base: 0x40000, len: 32, writable: false, kind: RegionKind::Kernel },
        KernelRegion { base: 0x50000, len: 64, writable: true, kind: RegionKind::Kernel },
        KernelRegion { base: PACK_BUFFER, len: 8, writable: true, kind: RegionKind::Pack },
        // The last byte is word-aligned, so a word load there tests containment after alignment.
        KernelRegion { base: PACK_BUFFER + 8, len: 5, writable: false, kind: RegionKind::Pack },
        KernelRegion { base: SYSCON_WIFI_CLK_EN, len: 8, writable: true, kind: RegionKind::Device },
        KernelRegion { base: RTC_STORE1, len: 4, writable: false, kind: RegionKind::Device },
        KernelRegion { base: 0xffff_fff0, len: 16, writable: true, kind: RegionKind::Kernel },
    ] }
}

#[derive(Default)]
struct Host { faults: Vec<(u64, Fault)> }
impl Hal for Host {
    fn configure(&mut self, _: u64, _: u32, _: Role) { unreachable!() }
    fn serial(&mut self, _: u64, _: u32, _: rt::serial::SerialSetting) { unreachable!() }
    fn write(&mut self, _: u64, _: u32, _: bool, _: u32) { unreachable!() }
    fn shift(&mut self, _: u64, _: u32, _: u32, _: u32, _: u8, _: u32) { unreachable!() }
    fn duty(&mut self, _: u64, _: u32, _: u8, _: u32) { unreachable!() }
    fn read(&mut self, _: u64, _: u32, _: u32) -> bool { unreachable!() }
    fn adc(&mut self, _: u64, _: u32, _: u32) -> u16 { unreachable!() }
    fn eth_send(&mut self, _: u64, _: &[u8], _: u32) -> bool { unreachable!() }
    fn eeprom_read(&mut self, _: u64, _: u16, _: u32) -> u8 { unreachable!() }
    fn eeprom_write(&mut self, _: u64, _: u16, _: u8, _: u32) { unreachable!() }
    fn eth_mac(&mut self, _: u64, _: u32) -> [u8; 6] { unreachable!() }
    fn clock(&mut self, t: u64, _: u32) -> u64 { t }
    fn fault(&mut self, t: u64, fault: Fault) { self.faults.push((t, fault)); }
}

/// The only device action: UPDATE latches the preset snapshot and raises
/// VALID immediately. Plain RAM would erase VALID when UPDATE is written.
struct Flat { memory: Memory, polls: u32, updates: u32 }
impl Bus for Flat {
    fn read(&mut self, addr: u32, width: Width, kind: Kind, now: u64) -> Result<u32, ()> {
        if addr == TIMER + 4 && kind == Kind::Load { self.polls += 1; }
        self.memory.read(addr, width, kind, now)
    }
    fn write(&mut self, addr: u32, width: Width, mut value: u32, now: u64) -> Result<(), ()> {
        if addr == TIMER + 4 {
            assert_eq!((width, value), (Width::W, 1 << 30), "unit-0 UPDATE protocol");
            self.updates += 1; value |= 1 << 29;
        }
        self.memory.write(addr, width, value, now)
    }
}

fn put(memory: &mut Memory, store: usize, offset: u32, words: &[u32]) {
    for (n, word) in words.iter().enumerate() {
        let at = offset as usize + n * 4;
        memory.bytes_mut(store)[at..at + 4].copy_from_slice(&word.to_le_bytes());
    }
}

fn word(memory: &Memory, store: usize, offset: u32) -> u32 {
    u32::from_le_bytes(memory.bytes(store)[offset as usize..offset as usize + 4].try_into().unwrap())
}

fn config() -> Config {
    Config { mul32_high: true, div32: true, unaligned: Unaligned::Hardware, excm_level: 3,
        mac16: true, s32c1i: true, threadptr: true, prid: None, vecbase_reset: 0,
        vectors: Vectors { kernel: 0x100, user: 0x180, window: [0, 0x40, 0x80, 0xc0, 0x100, 0x140] },
        int_type: [IntType::Software; 32], int_level: [1; 32] }
}

fn decoded_fault(cpu: &Cpu, code: &KernelCode) -> RadioFault {
    let mut words = vec![0; FAULT_WORDS as usize];
    for (at, value) in [(FAULT_LENGTH, FAULT_WORDS), (FAULT_CAUSE, 0), (FAULT_PC, cpu.pc),
        (FAULT_PS, cpu.ps), (FAULT_WINDOWBASE, cpu.window_base), (FAULT_WINDOWSTART, cpu.window_start)] {
        words[at as usize] = value;
    }
    // Exactly the recorder's rotation, including a nonzero WINDOWBASE.
    let physical = cpu.physical_ar();
    for r in 0..64 { words[FAULT_AR as usize + r] = physical[(4 * cpu.window_base as usize + r) % 64]; }
    let (_, address, size) = code.symbols.iter().find(|s| s.0 == "kernel_fault").unwrap();
    RadioFault::decode(&words).unwrap().with_map(&format!("0x{address:08x} {size} TEXT/FUNC kernel_fault kernel\n"))
}

struct Observation { calls: Vec<(u16, u32, u64)>, cpu: Cpu, fault: Option<Fault> }

#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction { Read, Write }

fn expected_access(regions: &[KernelRegion], class: AccessClass, direction: Direction, width: u32, addr: u32) -> Option<KernelFault> {
    // now64 writes eight bytes on a four-byte boundary. Scalar and single-byte raw accesses align to their width.
    if addr % width.min(4) != 0 { return Some(KernelFault::Misaligned); }
    let end = u64::from(addr) + u64::from(width);
    let allowed = regions.iter().any(|r| {
        let reachable = match r.kind {
            RegionKind::Kernel => matches!(class, AccessClass::Kernel | AccessClass::Pack),
            RegionKind::Pack => class == AccessClass::Pack,
            RegionKind::Device => class == AccessClass::Device,
        };
        reachable && (direction == Direction::Read || r.writable)
            && r.base <= addr && end <= u64::from(r.base) + u64::from(r.len)
    });
    (!allowed).then_some(KernelFault::OutOfRegion)
}

impl Observation {
    fn assert_access(&self, operand: &str, region: Option<&KernelRegion>, addr: u32, expected: Option<KernelFault>) {
        let port = KERNEL_PORTS[(self.calls[0].0 - KERNEL_BASE) as usize].name;
        let actual = self.fault.map(|fault| match fault {
            Fault::Kernel { kind, .. } => kind,
            other => panic!("{port} {operand} at {addr:#010x}, region {region:?}: unexpected {other:?}"),
        });
        assert_eq!(actual, expected, "{port} {operand} at {addr:#010x}, region {region:?}");
    }
}

fn compare(m: &Module<'_>, code: &KernelCode, o: &KernelOptions, name: &str, args: &[i32], enabled: u32, boot: u64) -> Observation {
    // Start near wrap so a burst crosses both the last slot and u32::MAX.
    compare_with_reports(m, code, o, name, args, enabled, boot, [u32::MAX - 1, u32::MAX - 1, 0])
}

fn compare_with_reports(m: &Module<'_>, code: &KernelCode, o: &KernelOptions, name: &str, args: &[i32], enabled: u32, boot: u64, header: [u32; 3]) -> Observation {
    let (entry, report) = code.entries.iter().enumerate().find(|(_, e)| e.name == name).expect("named export report");
    assert_eq!(code.map.entries[entry], (code.base + report.offset, code.base + report.offset + report.code_bytes), "{name}: source map uses the blob's absolute addresses");
    let EntryKind::Export { params, result } = &report.kind else { panic!("export has Handler report"); };
    assert_eq!(params.len(), args.len(), "{name}: reported argument count");
    let slot = |suffix: &str| {
        let v = m.vars().iter().find(|v| m.string(v.name) == format!("{name}#{suffix}")).unwrap();
        o.state + 4 * v.offset
    };
    for (p, &address) in params.iter().enumerate() { assert_eq!(address, slot(&format!("p{p}")), "{name}: param {p} address"); }
    assert_eq!(*result, slot("r"), "{name}: result address");

    let mut memory = Memory::new(); let ram = memory.store(STACK as usize + 16);
    memory.map(0, ram, 0, STACK as usize + 16, Perm { read: true, write: true, exec: true });
    memory.bytes_mut(ram)[BASE as usize..BASE as usize + code.bytes.len()].copy_from_slice(&code.bytes);
    memory.bytes_mut(ram)[RETURN as usize..RETURN as usize + 3].copy_from_slice(&0x004000u32.to_le_bytes()[..3]);
    put(&mut memory, ram, STATE, m.state_init()); put(&mut memory, ram, SHARED, m.shared());
    put(&mut memory, ram, T0, &[boot as u32, (boot >> 32) as u32]);
    let mut expected_ring = o.reports.map(|base| {
        let mut ring = vec![0xa5a5_a5a5; 3 + REPORT_RING_ENTRIES as usize * 3];
        ring[..3].copy_from_slice(&header);
        put(&mut memory, ram, base, &ring);
        ring
    });
    for (&address, &arg) in params.iter().zip(args) { put(&mut memory, ram, address, &[arg as u32]); }
    let timer = memory.store(0x100);
    memory.map(TIMER, timer, 0, 0x100, Perm { read: true, write: true, exec: false });
    put(&mut memory, timer, 4, &[1 << 29]);
    put(&mut memory, timer, 0x40, &[(SNAPSHOT >> 32) as u32, SNAPSHOT as u32]);
    let mut reference_memory = KernelMemory::new(); let mut stores = Vec::new();
    for (n, r) in o.regions.iter().enumerate() {
        let store = memory.store(r.len as usize);
        memory.map(r.base, store, 0, r.len as usize, Perm { read: true, write: r.writable, exec: false });
        for (i, b) in memory.bytes_mut(store).iter_mut().enumerate() { *b = (17 * n + 3 * i) as u8; }
        reference_memory.add_region(r.base, memory.bytes(store).to_vec(), r.writable, r.kind).unwrap();
        stores.push(store);
    }
    let mut bus = Flat { memory, polls: 0, updates: 0 };
    let mut cpu = Cpu::new(config(), code.base + report.offset);
    cpu.ps = 15; cpu.window_base = 9; cpu.window_start = 1 << 9;
    cpu.ar[0] = RETURN; cpu.ar[1] = STACK; cpu.ar[2] = STATE; cpu.ar[3] = SHARED;
    cpu.ar[12..16].copy_from_slice(&[0x1212, 0x1313, 0x1414, 0x1515]);
    let preserved = cpu.ar[12..16].to_vec();
    cpu.intenable = enabled; cpu.cpenable = 1;
    let mut calls = Vec::new(); let mut active = None;
    let mut raised = Vec::new(); let mut fault = None; let mut stop = None; let mut finished = false;
    for _ in 0..report.max_insns + 1 {
        let bits = bus.memory.read(cpu.pc, Width::W, Kind::Fetch, cpu.instret).unwrap();
        let (insn, _) = decode(bits).expect("generated instruction decodes");
        if insn.op == Op::Callx0 {
            assert!(active.is_none(), "kernel ports are leaf CALL0 routines");
            let target = cpu.ar[insn.s as usize];
            let symbol = code.symbols.iter().find(|s| s.1 == target).expect("port target symbol");
            let port = KERNEL_BASE + KERNEL_PORTS.iter().position(|p| p.name == symbol.0).unwrap() as u16;
            let max = if matches!(port, COPY_RAW | FILL_RAW | PACK_COPY | PACK_FILL) { cpu.ar[5] } else { 0 };
            active = Some((port, max, cpu.instret, cpu.pc + 3, cpu.ar[1], cpu.ar[12..16].to_vec()));
        }
        let set = (insn.op == Op::Wsr && insn.imm == 226).then(|| cpu.ar[insn.t as usize]);
        match cpu.step(&mut bus) {
            Ok(()) => {
                if let Some(mask) = set { for line in 0..32 { if mask & (1 << line) != 0 { raised.push(line); } } }
                if active.as_ref().is_some_and(|a| cpu.pc == a.3) {
                    let (port, max, start, _, sp, saved) = active.take().unwrap();
                    assert_eq!(cpu.ar[1], sp, "{name}: port preserves a1");
                    assert_eq!(&cpu.ar[12..16], saved.as_slice(), "{name}: port preserves a12..a15");
                    calls.push((port, max, cpu.instret - start));
                }
            }
            Err(Stop::Break { pc: RETURN }) => { finished = true; break; }
            Err(Stop::Exception { cause: 0, pc, .. }) => {
                assert_eq!(pc, code.symbols.iter().find(|s| s.0 == "kernel_fault").unwrap().1, "{name}: named shared fault stub");
                let (port, max, start, return_pc, sp, saved) = active.take().expect("fault originated in port");
                if port == RADIO_STOP {
                    let call = return_pc - 3;
                    let site = code.map.ops.iter().find(|op| (op.start..op.end).contains(&call)).unwrap().site.unwrap();
                    assert_eq!((cpu.ar[2], cpu.ar[3]), (107, site), "stop cause and call site at the ILL");
                    assert_eq!(word(&bus.memory, ram, STOP_WORD), cpu.ar[2], "pending cause stored before the ILL");
                    stop = Some(cpu.ar[2] as u8);
                } else {
                    fault = Event::Fault { code: cpu.ar[2].try_into().unwrap(), site: cpu.ar[3].try_into().unwrap(), arg: 0 }.kernel_fault();
                    assert!(fault.is_some(), "{name}: named fault code");
                }
                assert_eq!(cpu.ar[1], sp, "{name}: fault path preserves a1");
                assert_eq!(&cpu.ar[12..16], saved.as_slice(), "{name}: fault path preserves a12..a15");
                // The contract counts through ILL. The model stops before
                // retiring a faulting instruction, so charge that ILL here.
                calls.push((port, max, cpu.instret - start + 1));
                finished = true; break;
            }
            Err(stop) => panic!("{name}{args:?}: unexpected {stop:?}"),
        }
    }
    assert!(finished, "{name}{args:?}: exceeded the export instruction bound");
    let suspended = fault.is_some() || stop.is_some();
    assert!(cpu.instret + u64::from(suspended) <= report.max_insns, "{name}{args:?}: max_insns covers the whole export");
    assert_eq!(cpu.ps, 15, "{name}: export leaves the thunk's mask intact");
    assert_eq!(bus.polls, bus.updates, "{name}: one VALID poll per snapshot");
    for &(port, max, count) in &calls {
        let bound = code.port_cost(port, max).expect("used port has a derived cost");
        assert!(count <= bound, "{name}: port {port} cost {count} exceeds derived bound {bound}");
    }
    if !suspended {
        assert_eq!(cpu.ar[1], STACK, "{name}: export restores stack");
        assert_eq!(&cpu.ar[12..16], preserved.as_slice(), "{name}: export restores callee-saved registers");
    }

    let mut host = Host::default();
    let lines = m.kernel_lines().unwrap();
    let mut io = KernelPorts::new(reference_memory, lines, &mut host);
    let clock = (SNAPSHOT & ((1u64 << 52) - 1)).wrapping_sub(boot);
    io.clock = clock; io.intenable = enabled;
    let mut state = m.state_init().to_vec();
    let mut scratch = vec![0; (0..m.entries().len()).map(|k| m.scratch_words(k)).max().unwrap_or(0)];
    let expected = call_export(m, name, args, &mut state, &mut scratch, &mut io);
    let actual = if suspended { Err(CallError::Faulted) } else { Ok(word(&bus.memory, ram, *result) as i32) };
    assert_eq!(actual, expected, "{name}{args:?}: result or stopped export");
    assert_eq!(fault, io.fault, "{name}{args:?}: named fault and AIR site");
    assert_eq!(stop, io.stop, "{name}{args:?}: stop cause");
    for (n, (r, &store)) in o.regions.iter().zip(&stores).enumerate() {
        assert_eq!(bus.memory.bytes(store), io.memory.bytes(r.base, r.len as usize).unwrap(), "{name}{args:?}: region {n}");
    }
    for (i, &value) in state.iter().enumerate() { assert_eq!(word(&bus.memory, ram, STATE + 4 * i as u32), value, "{name}: state slot {i}"); }
    assert_eq!(cpu.intenable, io.intenable, "{name}: INTENABLE");
    assert_eq!(cpu.intenable & lines.protected, enabled & lines.protected, "{name}: protected enables never change");
    assert_eq!(raised, io.raised, "{name}: raised line sequence");
    assert_eq!(cpu.interrupt, raised.iter().fold(0, |mask, line| mask | (1 << line)), "{name}: raised lines stay latched at INTLEVEL 15");
    drop(io);
    let mut fatal = Vec::new();
    for (tick, reported) in host.faults {
        if let Fault::KernelReport { code, site, arg } = reported {
            assert_eq!(tick, clock, "{name}: report uses the reference clock");
            let ring = expected_ring.as_mut().expect("report test supplies writable ring RAM");
            if ring[1].wrapping_sub(ring[0]) >= REPORT_RING_ENTRIES {
                ring[2] = ring[2].saturating_add(1);
            } else {
                let slot = 3 + (ring[1] & (REPORT_RING_ENTRIES - 1)) as usize * 3;
                ring[slot..slot + 3].copy_from_slice(&[u32::from(code), site, arg as u32]);
                ring[1] = ring[1].wrapping_add(1);
            }
        } else { fatal.push((tick, reported)); }
    }
    assert_eq!(fatal, fault.into_iter().map(|f| (clock, f)).collect::<Vec<_>>(), "{name}: shared fatal Hal::fault channel");
    if let Some(ring) = expected_ring {
        let base = o.reports.unwrap();
        let actual: Vec<_> = (0..ring.len()).map(|i| word(&bus.memory, ram, base + i as u32 * 4)).collect();
        assert_eq!(actual, ring, "{name}: S3 ring equals Hal::fault reports; overflow changes only saturating lost");
    }
    Observation { calls, cpu, fault }
}

const SOURCE: &str = r#"
let zero = export(fn() { return 17 })
let six = export(fn(a, b, c, d, e, f) { return a + 2*b + 3*c + 4*d + 5*e + 6*f })
let l8 = export(fn(a) { return load8u(a) })
let l16 = export(fn(a) { return load16u(a) })
let l32 = export(fn(a) { return load32(a) })
let s8 = export(fn(a, v) { store8(a, v) })
let s16 = export(fn(a, v) { store16(a, v) })
let s32 = export(fn(a, v) { store32(a, v) })
let pl32 = export(fn(a) { return pack_load32(a) })
let ps32 = export(fn(a, v) { pack_store32(a, v) })
let ps8 = export(fn(a, v) { pack_store8(a, v) })
let dl32 = export(fn(a) { return dev_load32(a) })
let ds32 = export(fn(a, v) { dev_store32(a, v) })
let copy = export(fn(d, s, n) { copy_raw(d, s, n, 8) })
let pcopy = export(fn(d, s, n) { pack_copy(d, s, n, 8) })
let pfill = export(fn(d, b, n) { pack_fill(d, b, n, 8) })
let copy_sixteen = export(fn(d, s, n) { copy_raw(d, s, n, 16) })
let fill = export(fn(d, b, n) { fill_raw(d, b, n, 8) })
let copy_zero = export(fn(n) { copy_raw(0, 0, n, 0) })
let fill_zero = export(fn(n) { fill_raw(0, 0, n, 0) })
let zeros = export(fn(x) { return clz(x) })
let enable = export(fn(mask) { irq_enable(mask) })
let disable = export(fn(mask) { irq_disable(mask) })
let raise = export(fn() { irq_raise(7) })
let time = export(fn(a) { now64(a) })
let cap = export(fn(x) { while x >= 0 { x += 1 }; return x })
let notice = export(fn(n) { report(96, n); return n + 1 })
let halt = export(fn() { radio_stop(107) })
let notices = export(fn(n) { for i in 0..9 { report(127, n + i) }; return n })
let many = export(fn(a, b) {
    store32(a, b); store16(a + 4, b); store8(a + 6, b)
    copy_raw(a + 8, a, 8, 8); fill_raw(a + 4, 0xa5, 2, 8)
    irq_enable(-1); irq_disable(0x100); irq_raise(7); irq_raise(7)
    return load32(a + 8) + load16u(a + 4) + load8u(a + 6) + clz(b)
})
let stop = export(fn(a) { store32(a, 123); irq_raise(7); store8(0x30000, 0) })
let stop_after = export(fn(a) { store8(0x30000, 123); store32(a, 456); irq_raise(7); store8(0x30000, 0) })
let snapshot = export(fn(a) { now64(a); return load32(a) + load32(a + 4) })
let pack_device = export(fn(buf, regs) {
    let value: i32 = dev_load32(regs)
    pack_fill(buf, 0xa5, 8, 8)
    pack_store8(buf, value)
    pack_store32(buf + 4, value)
    pack_copy(0x30000, buf, 8, 8)
    dev_store32(regs + 4, pack_load32(buf + 4))
})
"#;

#[test]
fn differential_ports_reports_and_derived_costs() {
    use AccessClass::{Device, Kernel, Pack};
    use Direction::{Read, Write};
    let p = chip::profile(); let c = compile_kernel(SOURCE, &p, &RADIO_KERNEL_LINES, &[]).expect("kernel source");
    let m = module::load_kernel(&c.module, &p, &RADIO_KERNEL_LINES).unwrap();
    let o = options(); let code = build_kernel(&m, &o).unwrap();
    assert_eq!(code.entries.len(), m.decls().len(), "one report per export");
    let emitted = KERNEL_PORTS.len();
    assert_eq!(code.symbols.len(), emitted + 1, "one routine per port plus one shared fault stub");
    assert_eq!(code.costs.len(), emitted, "one derived cost per emitted port");
    assert_eq!(code.port_cost(LOOP_CAP, 0), None, "app ports have no kernel routine cost");
    let mut largest = BTreeMap::<(u16, u32), u64>::new();
    let mut check = |name: &str, args: &[i32], enabled| {
        let observation = compare(&m, &code, &o, name, args, enabled, BOOT_TIME);
        for &(port, max, count) in &observation.calls {
            let value = largest.entry((port, max)).or_default(); *value = (*value).max(count);
        }
        observation
    };
    check("zero", &[], 0); check("six", &[1, -2, 3, -4, 5, -6], 0);
    check("halt", &[], 0);
    for (name, class, width, direction) in [
        ("l8", Kernel, 1, Read), ("l16", Kernel, 2, Read), ("l32", Kernel, 4, Read),
        ("s8", Kernel, 1, Write), ("s16", Kernel, 2, Write), ("s32", Kernel, 4, Write),
        ("time", Kernel, 8, Write), ("pl32", Pack, 4, Read), ("ps32", Pack, 4, Write),
        ("ps8", Pack, 1, Write), ("dl32", Device, 4, Read), ("ds32", Device, 4, Write),
    ] {
        let mut probe = |addr: u32, region: Option<&KernelRegion>| {
            let args = [addr as i32, 0x89ab_cdefu32 as i32];
            let arity = if direction == Write && name != "time" { 2 } else { 1 };
            let expected = expected_access(&o.regions, class, direction, width, addr);
            check(name, &args[..arity], 0).assert_access(name, region, addr, expected);
        };
        for r in &o.regions {
            let end = r.base.wrapping_add(r.len);
            let addresses = [r.base.wrapping_sub(1), r.base, r.base + 1, end.wrapping_sub(width), end.wrapping_sub(1), end];
            for (i, &addr) in addresses.iter().enumerate() {
                if !addresses[..i].contains(&addr) { probe(addr, Some(r)); }
            }
        }
        probe(0x60000, None);
        if name == "time" {
            let ram = &o.regions[0];
            probe(ram.base + 4, Some(ram));
            probe(ram.base + ram.len - 4, Some(ram));
        }
    }
    // Keep the other copy operand inside writable Pack memory while probing each side in turn.
    let peer = PACK_BUFFER + 3;
    for r in &o.regions {
        let end = r.base.wrapping_add(r.len);
        for addr in [r.base.wrapping_sub(1), r.base, end.wrapping_sub(1), end] {
            for (operand, dst, src) in [("source", peer, addr), ("destination", addr, peer)] {
                let expected = expected_access(&o.regions, Pack, Read, 1, src)
                    .or_else(|| expected_access(&o.regions, Pack, Write, 1, dst));
                check("pcopy", &[dst as i32, src as i32, 1], 0).assert_access(operand, Some(r), addr, expected);
            }
            let expected = expected_access(&o.regions, Pack, Write, 1, addr);
            check("pfill", &[addr as i32, 0xa5, 1], 0).assert_access("destination", Some(r), addr, expected);
        }
    }
    for n in [0, 1, MAX as i32, MAX as i32 + 1, -1] {
        for (dst, src) in [(0, 0), (0x30000, 0x40000), (0x40000, 0x30000),
            (0x50001, 0x50000), (0x50000, 0x50001), (0xffff_fff1u32, 0xffff_fff0u32),
            (0xffff_fff0, 0xffff_fff1), (0x30000, 0x4001c), (0x5003c, 0x30000), (0x60000, 0x60000)] {
            check("copy", &[dst as i32, src as i32, n], 0);
        }
        for dst in [0, 0x30000, 0x40000, 0x5003c, 0xffff_fff0u32, 0xffff_ffff] {
            check("fill", &[dst as i32, 0x1234_56a5, n], 0);
        }
    }
    for n in [0, 1, -1] { check("copy_zero", &[n], 0); check("fill_zero", &[n], 0); }
    for x in [0, u32::MAX].into_iter().chain((0..32).map(|bit| 1u32 << bit)) { check("zeros", &[x as i32], 0); }
    for enabled in [0, u32::MAX, RADIO_KERNEL_LINES.protected, 1 << 1] {
        for mask in [0, -1, RADIO_KERNEL_LINES.protected as i32, 0x8000_0100u32 as i32] {
            check("enable", &[mask], enabled); check("disable", &[mask], enabled);
        }
    }
    check("raise", &[], 0); check("cap", &[0], 0);
    check("notice", &[-12345], 0); check("notices", &[0x1234_5678], 0);
    check("many", &[0x50000, 0x1234_5678], 0); check("many", &[0x40000, -1], 0);
    check("stop", &[0x40000], 0); check("stop_after", &[0x40000], 0); check("snapshot", &[0x50000], 0);
    check("pack_device", &[PACK_BUFFER as i32, SYSCON_WIFI_CLK_EN as i32], 0);
    check("pack_device", &[PACK_BUFFER as i32, 0x30000], 0);
    check("pack_device", &[SYSCON_WIFI_CLK_EN as i32, SYSCON_WIFI_CLK_EN as i32], 0);
    // At this max, summing copy's alternative loops would exceed the allowed
    // non-loop slack. The entire last region fits, including its wrapping end.
    check("copy_sixteen", &[0xffff_fff0u32 as i32, 0xffff_fff0u32 as i32, 16], 0);
    drop(check);
    // The derivation counts every non-loop branch arm. Its slack over a real
    // path must fit within the routine's own non-loop instruction count.
    for port in KERNEL_BASE..KERNEL_BASE + KERNEL_PORTS.len() as u16 {
        let max = if matches!(port, COPY_RAW | FILL_RAW | PACK_COPY | PACK_FILL) { MAX } else { 0 };
        assert!(largest.contains_key(&(port, max)), "port {port}: differential exercised the routine");
    }
    for (&(port, max), &observed) in &largest {
        let cost = &code.costs.iter().find(|c| c.0 == port).expect("observed port cost").1;
        let derived = code.port_cost(port, max).unwrap();
        let slack = derived.checked_sub(observed).expect("derived cost covers the largest observation");
        assert!(slack <= cost.non_loop, "port {port}, max {max}: slack {slack} exceeds {} non-loop instructions", cost.non_loop);
    }
    let copy = compare(&m, &code, &o, "copy", &[0xffff_fff1u32 as i32, 0xffff_fff0u32 as i32, MAX as i32], 0, BOOT_TIME);
    let fill = compare(&m, &code, &o, "fill", &[0xffff_fff0u32 as i32, -1, MAX as i32], 0, BOOT_TIME);
    for (port, observed) in [(COPY_RAW, copy.calls[0].2), (FILL_RAW, fill.calls[0].2)] {
        let cost = &code.costs.iter().find(|c| c.0 == port).unwrap().1;
        assert!(cost.per_unit > 0, "port {port}: the byte-loop span is recorded");
        assert!(observed >= u64::from(MAX) * cost.per_unit, "port {port}: execution at max includes the byte-loop charge");
        assert_eq!(code.port_cost(port, MAX), Some(cost.fixed + u64::from(MAX) * cost.per_unit), "port {port}: derived byte-loop cost is charged at the call's constant max");
    }
    let poll = &code.costs.iter().find(|c| c.0 == NOW64).unwrap().1;
    assert!(poll.per_unit > 0, "now64 records the VALID poll span");
    assert_eq!(code.port_cost(NOW64, 0), Some(poll.fixed + poll.per_unit), "now64 charges one VALID poll");
    let report = &code.costs.iter().find(|c| c.0 == KERNEL_REPORT).unwrap().1;
    assert_eq!(report.per_unit, 0, "report has no loop group");
    assert_eq!(report.fixed, report.non_loop + 2, "report charges its routine, CALLX0 and the shared ILL stub");
    assert_eq!(code.port_cost(KERNEL_REPORT, u32::MAX), Some(report.fixed), "report has constant cost");
    let notice = compile_kernel("let notice = export(fn(arg) { report(107, arg); return arg + 1 })",
        &p, &RADIO_KERNEL_LINES, &[]).unwrap();
    let notice = module::load_kernel(&notice.module, &p, &RADIO_KERNEL_LINES).unwrap();
    for stop_pending in [None, Some(STOP_WORD)] {
        let kernel = build_kernel(&notice, &KernelOptions { stop_pending, ..o.clone() }).unwrap();
        let export = &kernel.entries[0];
        let EntryKind::Export { params, result } = &export.kind else { panic!("report fixture must be an export"); };
        let charge = &kernel.costs.iter().find(|(p, _)| *p == KERNEL_REPORT).unwrap().1;
        if stop_pending.is_none() {
            assert_eq!(charge.fixed, charge.non_loop + 1, "without a stop word, report returns and never reaches the ILL stub");
        }
        let symbol = |name: &str| kernel.symbols.iter().find(|s| s.0 == name).unwrap().1;
        for full in [false, true] {
            let mut mem = Memory::new(); let ram = mem.store(STACK as usize + 16);
            mem.map(0, ram, 0, STACK as usize + 16, Perm { read: true, write: true, exec: true });
            mem.bytes_mut(ram)[BASE as usize..BASE as usize + kernel.bytes.len()].copy_from_slice(&kernel.bytes);
            put(&mut mem, ram, params[0], &[41]);
            let tail = if full { REPORT_RING_ENTRIES } else { 0 };
            put(&mut mem, ram, REPORTS + 4, &[tail]);
            let mut cpu = Cpu::new(config(), kernel.base + export.offset);
            cpu.ps = 15; cpu.ar[..4].copy_from_slice(&[RETURN, STACK, STATE, SHARED]);
            let mut published = false;
            let mut site = None;
            while cpu.pc != RETURN && cpu.pc != symbol("kernel_fault") {
                assert!(cpu.instret < export.max_insns, "report reaches its terminal PC within the export bound");
                if cpu.pc == symbol("report") { site = Some(cpu.ar[10]); }
                cpu.step(&mut mem).expect("the export runs to its return or shared ILL");
                if word(&mem, ram, STOP_WORD) != 0 {
                    assert!(published, "report publishes its ring entry or loss strictly before storing the stop cause");
                }
                if full { published = word(&mem, ram, REPORTS + 8) == 1; }
                else if word(&mem, ram, REPORTS + 4) == 1 {
                    assert_eq!([12, 16, 20].map(|n| word(&mem, ram, REPORTS + n)), [107, site.unwrap(), 41],
                        "the complete report is visible at tail publication");
                    published = true;
                }
            }
            assert!(published, "both stop-word configurations publish the report or count its loss");
            if stop_pending.is_some() {
                assert_eq!((cpu.pc, word(&mem, ram, STOP_WORD), cpu.ar[2], cpu.ar[3]),
                    (symbol("kernel_fault"), 107, 107, site.unwrap()), "report stops at the shared ILL with the published cause and site");
            } else {
                assert_eq!((cpu.pc, word(&mem, ram, *result), word(&mem, ram, STOP_WORD)), (RETURN, 42, 0),
                    "without a stop word, the reporting export continues and returns its result");
            }
        }
    }
    for lost in [0, u32::MAX - 1, u32::MAX] {
        let head = u32::MAX - 3;
        compare_with_reports(&m, &code, &o, "notice", &[-7], 0, BOOT_TIME,
            [head, head.wrapping_add(REPORT_RING_ENTRIES), lost]);
    }
    compare(&m, &code, &o, "snapshot", &[0x50000], 0, 8); // no low-word borrow
    let custom = rt::KernelLines { protected: (1 << 3) | (1 << 15), raise: RADIO_KERNEL_LINES.raise };
    let custom_m = module::load_kernel(&c.module, &p, &custom).unwrap();
    let custom_code = build_kernel(&custom_m, &o).unwrap();
    compare(&custom_m, &custom_code, &o, "enable", &[-1], 1 << 3, BOOT_TIME);
    compare(&custom_m, &custom_code, &o, "disable", &[-1], u32::MAX, BOOT_TIME);
}

#[test]
fn constant_scalar_accesses_are_proved_before_emission() {
    use AccessClass::{Device, Kernel, Pack};
    let p = chip::profile();
    let o = options();
    let device = SYSCON_WIFI_CLK_EN + 4;
    let bindings = [("LAST_DEVICE", device as i32), ("PACK_WORD", PACK_BUFFER as i32)];
    let source = r#"
        let kbyte = export(fn(v) { store8(-1, v); return load8u(-1) })
        let khalf = export(fn(v) { store16(0x3001e, v); return load16u(0x3001e) })
        let kword = export(fn(v) { store32(-4, v); return load32(-4) })
        let pword = export(fn(v) { pack_store32(PACK_WORD, v); return pack_load32(PACK_WORD) })
        let dword = export(fn(v) { dev_store32(LAST_DEVICE, v); return dev_load32(LAST_DEVICE) })
        let choice = export(fn(a, choose) {
            let addr: i32 = 0x3001c
            if choose != 0 { addr = a }
            return load32(addr)
        })
    "#;
    let compiled = compile_kernel(source, &p, &RADIO_KERNEL_LINES, &bindings).unwrap();
    let m = module::load_kernel(&compiled.module, &p, &RADIO_KERNEL_LINES).unwrap();
    let code = build_kernel(&m, &o).unwrap();
    for (name, address, width, class) in [
        ("kbyte", u32::MAX, 1, Kernel), ("khalf", 0x3001e, 2, Kernel),
        ("kword", u32::MAX - 3, 4, Kernel), ("pword", PACK_BUFFER, 4, Pack),
        ("dword", device, 4, Device),
    ] {
        let observed = compare(&m, &code, &o, name, &[0x1234_5678], 0, BOOT_TIME);
        assert!(observed.fault.is_none() && observed.calls.is_empty(), "{name} executes both accesses inline");
        let entry = code.entries.iter().find(|e| e.name == name).unwrap();
        assert!(entry.port_charges.is_empty(), "{name} charges only its inline instructions");
        let accesses: Vec<_> = entry.proven_accesses.iter().map(|s| (s.address, s.width, s.class, s.write)).collect();
        assert_eq!(accesses, [(address, width, class, true), (address, width, class, false)],
            "{name} records the two accesses checked against the reference");
    }
    for (choose, expected) in [(0, None), (1, Some(KernelFault::OutOfRegion))] {
        let observed = compare(&m, &code, &o, "choice", &[PACK_BUFFER as i32, choose], 0, BOOT_TIME);
        observed.assert_access("branch-selected address", None, if choose == 0 { 0x3001c } else { PACK_BUFFER }, expected);
    }

    // The final aligned device word extends one byte beyond this fixture's region.
    let mut short = o.clone();
    short.regions.iter_mut().find(|r| r.base == SYSCON_WIFI_CLK_EN).unwrap().len = 7;
    for (operation, expression, address, width, class, reason) in [
        ("store8", "store8(ADDR, v)", 0x30020, 1, Kernel, "outside permitted regions"),
        ("load8u", "return load8u(ADDR)", 0x2ffff, 1, Kernel, "outside permitted regions"),
        ("dev_load32", "return dev_load32(ADDR)", device, 4, Device, "outside permitted regions"),
        ("dev_store32", "dev_store32(ADDR, v)", device, 4, Device, "outside permitted regions"),
        ("dev_store32", "dev_store32(ADDR, v)", RTC_STORE1, 4, Device, "outside permitted regions"),
        ("load32", "return load32(ADDR)", PACK_BUFFER, 4, Kernel, "outside permitted regions"),
        ("pack_store8", "pack_store8(ADDR, v)", PACK_BUFFER + 8, 1, Pack, "outside permitted regions"),
        ("load16u", "return load16u(ADDR)", 0x30001, 2, Kernel, "misaligned"),
    ] {
        let source = format!("let refused = export(fn(v) {{ {expression} }})");
        let compiled = compile_kernel(&source, &p, &RADIO_KERNEL_LINES, &[("ADDR", address as i32)]).unwrap();
        let m = module::load_kernel(&compiled.module, &p, &RADIO_KERNEL_LINES).unwrap();
        let message = build_kernel(&m, &short).unwrap_err().message;
        assert!(message.contains("entry `refused`") && message.contains(&format!(
            "{operation} at {address:#010x}, width {width}, class {class:?}: {reason}")),
            "{operation} refusal identifies the export and the failed proof: {message}");
    }
}

#[test]
fn builder_refusals_and_app_reports() {
    let p = chip::profile(); let o = options();
    let app = compile("fn setup() {}", &p, &[], &[]).unwrap();
    let m = module::load(&app.module, &p).unwrap();
    assert!(build_kernel(&m, &o).unwrap_err().message.contains("kernel module"), "app cannot enter kernel builder");
    assert!(build_handlers(&m, &Options::default()).unwrap().entries.iter().all(|e| e.kind == EntryKind::Handler), "app reports retain Handler kind");
    let c = compile_kernel("let f = export(fn(a) { return a })", &p, &RADIO_KERNEL_LINES, &[]).unwrap();
    let m = module::load_kernel(&c.module, &p, &RADIO_KERNEL_LINES).unwrap();
    assert!(build_handlers(&m, &Options::default()).unwrap_err().message.contains("domain"), "kernel cannot enter app builder");
    for (bad, message) in [
        (KernelOptions { base: BASE + 1, ..o.clone() }, "code base"),
        (KernelOptions { state: STATE + 2, ..o.clone() }, "state"),
        (KernelOptions { base: 0xffff_fffc, ..o.clone() }, "code address space"),
        (KernelOptions { state: 0xffff_fffc, ..o.clone() }, "state address"),
        (KernelOptions { regions: vec![KernelRegion { base: 4, len: 8, writable: true, kind: RegionKind::Kernel }, KernelRegion { base: 8, len: 4, writable: false, kind: RegionKind::Kernel }], ..o.clone() }, "overlap"),
        (KernelOptions { regions: vec![KernelRegion { base: 0, len: 0, writable: true, kind: RegionKind::Kernel }], ..o.clone() }, "empty or wraps"),
        (KernelOptions { regions: vec![KernelRegion { base: u32::MAX, len: 2, writable: true, kind: RegionKind::Kernel }], ..o.clone() }, "empty or wraps"),
    ] { assert!(build_kernel(&m, &bad).unwrap_err().message.contains(message), "bad options: {message}"); }
    let c = compile_kernel("let limit = export(fn(n) { report(96, n) })", &p, &RADIO_KERNEL_LINES, &[]).unwrap();
    let m = module::load_kernel(&c.module, &p, &RADIO_KERNEL_LINES).unwrap();
    assert_eq!(build_kernel(&m, &KernelOptions { reports: None, ..o.clone() }).unwrap_err().message,
        "kernel port 78 `report` needs a report ring (KernelOptions::reports)",
        "report is refused by name before routine assembly");
    assert!(build_kernel(&m, &o).is_ok(), "a report ring enables port 78");
    let c = compile_kernel("let halt = export(fn() { radio_stop(107) })", &p, &RADIO_KERNEL_LINES, &[]).unwrap();
    let m = module::load_kernel(&c.module, &p, &RADIO_KERNEL_LINES).unwrap();
    assert_eq!(build_kernel(&m, &KernelOptions { stop_pending: None, ..o.clone() }).unwrap_err().message,
        "kernel port 86 `radio_stop` needs a stop word (KernelOptions::stop_pending)");
    assert!(build_kernel(&m, &KernelOptions { stop_pending: Some(STOP_WORD + 1), ..o }).unwrap_err().message.contains("word-aligned"));
}

#[test]
fn radio_fault_decodes_the_recorders_window_relative_ars() {
    let p = chip::profile(); let c = compile_kernel("let f = export(fn(a) { return load32(a) })", &p, &RADIO_KERNEL_LINES, &[]).unwrap();
    let m = module::load_kernel(&c.module, &p, &RADIO_KERNEL_LINES).unwrap(); let o = options();
    let code = build_kernel(&m, &o).unwrap();
    let observed = compare(&m, &code, &o, "f", &[0x30001], 0, BOOT_TIME);
    assert_ne!(observed.cpu.window_base, 0, "decoder exercises a nonzero window");
    let fault = decoded_fault(&observed.cpu, &code);
    assert_eq!(fault.kernel_fault(), observed.fault, "recorder rotation is applied exactly once");
    let Some(Fault::Kernel { kind, site }) = observed.fault else { panic!("expected kernel memory fault"); };
    assert!(fault.to_string().starts_with(&format!("kernel fault: {} at site {site}\nradio fault:", kind.name())), "kernel line precedes the ordinary fault display");
    let mut other = fault.clone(); other.cause = 3;
    assert_eq!(other.kernel_fault(), None, "only IllegalInstruction is a kernel fault");
    assert_eq!(fault.clone().with_map("").kernel_fault(), None, "a matching kernel_fault symbol is required");
    let mut other = fault.clone(); other.ars[2] = 250;
    assert_eq!(other.kernel_fault(), None, "unknown watch fault code is not a kernel fault");
    for code in [rt::watch::FAULT_KERNEL_TIMER_REFUSED, 127] {
        other.ars[2] = u32::from(code);
        assert_eq!(other.kernel_fault(), None, "report code {code} cannot identify a fatal ILL record");
        assert!(other.to_string().starts_with("radio fault:"), "report code {code} keeps the ordinary crash display");
    }
    let mut cap = fault; cap.ars[2] = u32::from(rt::watch::FAULT_LOOP_CAP);
    assert_eq!(cap.kernel_fault(), Some(Fault::LoopCap { site }), "shared loop cap code decodes");
    assert!(cap.to_string().starts_with(&format!("kernel fault: loop cap at site {site}\n")), "loop cap display uses the shared name");
}
