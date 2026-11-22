use fab_elec_mcu_avr::asm::{Asm, Target};
use fab_elec_mcu_avr::insn::Insn;
use fab_elec_mcu_avr::sim::{Cpu, Stop, SRAM};
use fab_elec_mcu_emit_avr::helpers;
use makepad_script_compute::ir::{eval_bin, Bin};

struct Driver {
    name: &'static str,
    pc: u32,
    op: Option<Bin>,
    cycles: u32,
    stack: u16,
}

fn quad(cpu: &Cpu, low: usize) -> u32 {
    u32::from_le_bytes(cpu.r[low..low + 4].try_into().unwrap())
}

fn check(cpu: &mut Cpu, driver: &Driver, a: u32, b: u32) {
    // Reuse the flash and peripheral allocation for the whole grid. All
    // peripheral interrupts remain disabled; arithmetic runs with I set.
    for (r, value) in cpu.r.iter_mut().enumerate() {
        *value = (r as u8).wrapping_mul(37).wrapping_add((a ^ b) as u8);
    }
    cpu.r[2] = 0;
    cpu.r[16..20].copy_from_slice(&a.to_le_bytes());
    cpu.r[20..24].copy_from_slice(&b.to_le_bytes());
    let before = cpu.r;
    let sreg = (a ^ b.rotate_left(7)) as u8 | 0x80;
    cpu.sreg = sreg;
    cpu.sp = (0x100 + SRAM - 1) as u16;
    let sp = cpu.sp;
    let guard = sp - 2 - driver.stack;
    cpu.write(guard, 0xa5);
    cpu.pc = driver.pc;
    cpu.cycles = 0;

    // AIR has no unsigned division op. Its signed operations and all shifts
    // use eval_bin directly; unsigned quotient/remainder use Rust's u32 rule.
    let reference = match driver.op {
        Some(op) => eval_bin(op, u64::from(a), u64::from(b)) as u32,
        None => if b == 0 { 0 } else { a / b },
    };
    let stop = cpu.run(u64::from(driver.cycles) + 1);
    let result = quad(cpu, 16);
    assert_eq!(stop, Stop::Break,
        "{} a={a:#010x} b={b:#010x}: result={result:#010x}, reference={reference:#010x}, pc={}",
        driver.name, cpu.pc);
    assert_eq!(result, reference,
        "{} a={a:#010x} b={b:#010x}: result={result:#010x}, reference={reference:#010x}",
        driver.name);
    if driver.op.is_none() {
        let remainder = quad(cpu, 24);
        let reference = if b == 0 { a } else { a % b };
        assert_eq!(remainder, reference,
            "{} remainder a={a:#010x} b={b:#010x}: result={remainder:#010x}, reference={reference:#010x}",
            driver.name);
    }
    // BREAK costs one cycle and is outside the published CALL..RET total.
    assert_eq!(cpu.cycles - 1, u64::from(driver.cycles),
        "{} cycles a={a:#010x} b={b:#010x}: result={result:#010x}, reference={reference:#010x}",
        driver.name);
    assert_eq!(&cpu.r[2..16], &before[2..16],
        "{} must preserve r2..r15 for a={a:#010x} b={b:#010x}", driver.name);
    assert_eq!(&cpu.r[28..30], &before[28..30],
        "{} must preserve Y for a={a:#010x} b={b:#010x}", driver.name);
    assert_eq!(cpu.sreg & 0x80, sreg & 0x80,
        "{} must preserve interrupts for a={a:#010x} b={b:#010x}", driver.name);
    assert_eq!(cpu.sp, sp,
        "{} must restore SP for a={a:#010x} b={b:#010x}", driver.name);
    assert_eq!(cpu.read(guard), 0xa5,
        "{} exceeded its stack for a={a:#010x} b={b:#010x}", driver.name);
}

fn xorshift(state: &mut u32) -> u32 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    *state
}

#[test]
fn helpers_match_air_at_constant_cost() {
    let mut asm = Asm::new();
    let h = helpers::emit(&mut asm);
    let drivers = [
        ("mul32", h.mul32, Some(Bin::MulI), helpers::MUL32, 0),
        ("udivmod32", h.udivmod32, None, helpers::UDIVMOD32, 0),
        ("sdiv32", h.sdiv32, Some(Bin::DivI), helpers::SDIV32, helpers::STACK_BYTES as u16),
        ("srem32", h.srem32, Some(Bin::RemI), helpers::SREM32, helpers::STACK_BYTES as u16),
        ("shl32", h.shl32, Some(Bin::ShlI), helpers::SHL32, 0),
        ("shr32", h.shr32, Some(Bin::ShrUI), helpers::SHR32, 0),
        ("sar32", h.sar32, Some(Bin::ShrI), helpers::SAR32, 0),
    ].map(|(name, label, op, cycles, stack)| {
        let pc = asm.here();
        asm.jump(Target::Call, label);
        asm.emit(Insn::Break);
        Driver { name, pc, op, cycles, stack }
    });
    let words = asm.finish().expect("helper branches must fit without relaxation");
    let mut cpu = Cpu::new(&words);
    let mut edge = vec![
        0, 1, u32::MAX, 2, (-2i32) as u32, 3, 7, 10, 255, 256, 65535, 65536,
        i32::MIN as u32, (i32::MIN + 1) as u32, i32::MAX as u32, (i32::MAX - 1) as u32,
        0x5555_5555, 0xaaaa_aaaa,
    ];
    // Pairs use the byte and half-word boundaries; the shifts below take
    // every power of two and its neighbours.
    let mut pairs = edge.clone();
    for bit in 0..32 {
        let value = 1u32 << bit;
        let near = [value, value.wrapping_sub(1), value.wrapping_add(1)];
        edge.extend(near);
        if [0, 1, 7, 8, 15, 16, 23, 24, 30, 31].contains(&bit) { pairs.extend(near); }
    }
    for set in [&mut edge, &mut pairs] {
        set.sort_unstable();
        set.dedup();
    }
    for &a in &pairs {
        for &b in &pairs {
            for driver in &drivers {
                check(&mut cpu, driver, a, b);
            }
        }
    }
    let mut state = 0x6d2b_79f5;
    for _ in 0..384 {
        let a = xorshift(&mut state);
        let b = xorshift(&mut state);
        for driver in &drivers {
            check(&mut cpu, driver, a, b);
        }
    }
    // Includes negative/high-bit A values, amounts past 31, and upper bits
    // of B set while the effective amount is one.
    for &a in &edge {
        for b in (0..=40).chain([0xffff_ff21]) {
            for driver in &drivers[4..] {
                check(&mut cpu, driver, a, b);
            }
        }
    }
}
