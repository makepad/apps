use fab_elec_mcu_avr::{
    asm::{Asm, Target},
    chip,
    insn::{encode, Insn::*, Ptr},
    machine::AvrMachine,
    sim::{Cpu, Event, Stop, FLASH_WORDS},
};
use fab_elec_mcu_emu::{Drive, DriveEdge, LinkOut, Machine, PcSet, Pull, Ran, Until};

const END: u64 = 200_000;

fn store(a: &mut Asm, addr: u16, value: u8) {
    a.emit(Ldi(16, value));
    a.emit(Sts(addr, 16));
}

fn pointer(a: &mut Asm, lo: u8, addr: u16) {
    a.emit(Ldi(lo, addr as u8));
    a.emit(Ldi(lo + 1, (addr >> 8) as u8));
}

fn timer0() -> Vec<u16> {
    let mut a = Asm::new();
    a.emit(Jmp(64));
    a.org(32); a.emit(Jmp(192));
    a.org(64);
    for (addr, value) in [(0x2a, 0x60), (0x47, 61), (0x48, 173), (0x44, 0xb3),
        (0xc0, 2), (0xc1, 0x18), (0x6e, 1), (0x45, 2)] {
        store(&mut a, addr, value);
    }
    a.emit(Bset(7)); a.emit(Sleep); a.emit(Rjmp(-2));
    a.org(192);
    a.emit(Inc(17)); a.emit(Sts(0x100, 17)); a.emit(Sts(0xc6, 17)); a.emit(Reti);
    a.finish().unwrap()
}

fn timer1() -> Vec<u16> {
    let mut a = Asm::new();
    a.emit(Jmp(64));
    a.org(22); a.emit(Jmp(256));
    a.org(64);
    for (addr, value) in [(0x24, 0x20), (0x89, 1), (0x88, 49), (0x6f, 2), (0x81, 9)] {
        store(&mut a, addr, value);
    }
    a.emit(Bset(7)); a.emit(Ldi(19, 0x20)); a.emit(Ldi(24, 31));
    let again = a.label(); a.bind(again);
    a.emit(Lds(20, 0x84)); a.emit(Lds(21, 0x85)); // TCNT1 low latches TEMP.
    a.emit(Sts(0x100, 20)); a.emit(Sts(0x101, 21));
    a.emit(Inc(24)); a.emit(Andi(24, 31)); a.emit(Ori(24, 16));
    store(&mut a, 0x89, 1); a.emit(Sts(0x88, 24));
    a.emit(Sbi(5, 5)); a.emit(Cbi(5, 5)); a.emit(Out(3, 19));
    a.jump(Target::Rjmp, again);
    a.org(256); a.emit(Inc(22)); a.emit(Reti);
    a.finish().unwrap()
}

fn timer2() -> Vec<u16> {
    let mut a = Asm::new();
    for (addr, value) in [(0x24, 8), (0x2a, 8), (0xb3, 63), (0xb4, 127),
        (0xb0, 0xa1), (0xb1, 1), (0x45, 2), (0x81, 2)] {
        store(&mut a, addr, value);
    }
    let again = a.label(); let delay = a.label(); a.bind(again);
    a.emit(Ldi(18, 255)); a.bind(delay); a.emit(Dec(18)); a.jump(Target::Brbc(1), delay);
    a.emit(Inc(17)); a.emit(Sts(0xb2, 17)); // Block compare on the next timer tick.
    store(&mut a, 0x43, 0x83); // Hold and reset both prescaler phases.
    a.emit(Nop); store(&mut a, 0x43, 0);
    a.jump(Target::Rjmp, again);
    a.finish().unwrap()
}

fn usart() -> Vec<u16> {
    let mut a = Asm::new();
    a.emit(Jmp(64));
    a.org(36); a.emit(Jmp(192));
    a.org(64);
    for (addr, value) in [(0xc0, 2), (0xc4, 3), (0xc1, 0x98)] { store(&mut a, addr, value); }
    a.emit(Bset(7));
    let again = a.label(); a.bind(again);
    a.emit(Lds(18, 0xc0)); a.emit(Sbrs(18, 5)); a.jump(Target::Rjmp, again);
    a.emit(Inc(17)); a.emit(Sts(0xc6, 17)); a.jump(Target::Rjmp, again);
    a.org(192); a.emit(Lds(19, 0xc6)); a.emit(Sts(0x100, 19)); a.emit(Inc(20)); a.emit(Reti);
    a.finish().unwrap()
}

fn spi() -> Vec<u16> {
    let mut a = Asm::new();
    a.emit(Jmp(64));
    a.org(34); a.emit(Jmp(192));
    a.org(64);
    store(&mut a, 0x24, 0x2c); store(&mut a, 0x4d, 1);
    let again = a.label(); let poll = a.label(); a.bind(again);
    store(&mut a, 0x4c, 0x50); // First byte polls; the second wakes idle through SPIE.
    a.emit(Inc(17)); a.emit(Out(0x2e, 17));
    a.bind(poll); a.emit(In(18, 0x2d)); a.emit(Sbrs(18, 7)); a.jump(Target::Rjmp, poll);
    a.emit(In(19, 0x2e)); a.emit(Ldi(21, 1));
    store(&mut a, 0x4c, 0xd0); a.emit(Bset(7));
    a.emit(Inc(17)); a.emit(Out(0x2e, 17)); a.emit(Sleep);
    a.emit(Bclr(7)); a.jump(Target::Rjmp, again);
    a.org(192); a.emit(Ldi(20, 1)); a.emit(In(19, 0x2e)); a.emit(Reti);
    a.finish().unwrap()
}

fn eeprom() -> Vec<u16> {
    let mut a = Asm::new();
    a.emit(Jmp(64));
    a.org(44); a.emit(Jmp(192));
    a.org(64);
    for (address, value) in [(0, 0xa5), (1, 0x5a)] {
        store(&mut a, 0x41, address); store(&mut a, 0x40, value);
        a.emit(Sbi(0x1f, 2)); a.emit(Sbi(0x1f, 1));
        let poll = a.label(); a.bind(poll);
        a.emit(Sbic(0x1f, 1)); a.jump(Target::Rjmp, poll);
        a.emit(Sbi(0x1f, 0)); a.emit(In(18 + address, 0x20));
    }
    store(&mut a, 0x41, 2); store(&mut a, 0x40, 0x3c);
    a.emit(Sbi(0x1f, 2)); a.emit(Sbi(0x1f, 1));
    a.emit(Sbi(0x1f, 3)); a.emit(Bset(7));
    // No bus accesses in this wait: completion must bound the deferred horizon
    // and refresh its cached IRQ before the next instruction executes.
    let ready = a.label(); a.bind(ready);
    a.emit(Cpi(21, 0)); a.jump(Target::Brbs(1), ready);
    a.emit(Sbi(0x1f, 0)); a.emit(In(20, 0x20));
    // Finish with ordinary instructions so the fixture uses the full lockstep budget.
    a.emit(Rjmp(-1));
    a.org(192); a.emit(Inc(21)); a.emit(Cbi(0x1f, 3)); a.emit(Reti);
    a.finish().unwrap()
}

fn mixed() -> Vec<u16> {
    let mut a = Asm::new();
    for (addr, value) in [(0x24, 0x3f), (0x2a, 0x60), (0x47, 83), (0x48, 167),
        (0x44, 0xa3), (0x45, 2)] { store(&mut a, addr, value); }
    let again = a.label(); let sub = a.label(); a.bind(again);
    let mut seed = 0x61c8_8647u32;
    for index in 0..256 {
        seed ^= seed << 13; seed ^= seed >> 17; seed ^= seed << 5;
        let d = 17 + (seed % 8) as u8;
        let r = 17 + ((seed >> 8) % 8) as u8;
        let bit = ((seed >> 16) & 7) as u8;
        // Cycle through every family; the seed chooses operands, addresses and values.
        match index % 16 {
            0 => { a.emit(Ldi(d, seed as u8)); a.emit(Add(d, r)); a.emit(Adc(r, d)); }
            1 => { a.emit(Eor(d, r)); a.emit(Sub(d, r)); a.emit(Ror(d)); a.emit(Mul(d, r)); }
            2 => { a.emit(Cp(d, r)); a.emit(Brbs(1, 1)); a.emit(Inc(d)); a.emit(Brbc(0, 1)); a.emit(Dec(r)); }
            3 => { a.emit(Cpse(d, r)); a.emit(Sts(0x110, d)); }
            4 => { a.emit(Sbrc(d, bit)); a.emit(Nop); a.emit(Sbrs(r, bit)); a.emit(Lds(d, 0x110)); }
            5 => { a.emit(Sbic(0x15, bit % 3)); a.emit(Sts(0x110, d)); a.emit(Sbis(3, bit % 6)); a.emit(Nop); }
            6..=11 => {
                let addr = if index % 16 < 9 { 0x120 + (seed & 31) as u16 }
                    else { [0x25, 0x35, 0x46, 0x47][((seed >> 5) & 3) as usize] };
                // X/Y/Z each visit SRAM and I/O, including predecrement/postincrement
                // and displacement addresses. Only the selected target is accessed.
                match index % 3 {
                    0 => { pointer(&mut a, 26, addr); a.emit(St(Ptr::XInc, d)); a.emit(Ld(r, Ptr::XDec)); }
                    1 => { pointer(&mut a, 28, addr); a.emit(St(Ptr::YInc, d)); a.emit(Ld(r, Ptr::YDec)); }
                    _ => { pointer(&mut a, 30, addr - 1); a.emit(Std(false, 1, d)); a.emit(Ldd(r, false, 1)); }
                }
            }
            12 => { a.emit(Push(d)); a.emit(Push(r)); a.emit(Pop(d)); a.emit(Pop(r)); }
            13 => a.jump(Target::Rcall, sub),
            14 => { a.emit(In(d, 6)); a.emit(Out(5, r)); a.emit(Sbi(5, bit % 6)); a.emit(Cbi(5, bit % 6)); }
            _ => { a.emit(Andi(d, seed as u8)); a.emit(Ori(r, (seed >> 8) as u8)); a.emit(Swap(d)); }
        }
    }
    a.jump(Target::Rjmp, again);
    a.bind(sub); a.emit(Push(16)); a.emit(Ldi(16, 0x20)); a.emit(Out(3, 16)); a.emit(Pop(16)); a.emit(Ret);
    a.finish().unwrap()
}

fn lockstep(name: &str, mut oracle: Cpu, end: u64, feed_rx: bool, cosim: bool) -> Cpu {
    oracle.set_cosim_timing(cosim);
    let mut deferred = oracle.clone();
    // Reuse the clone's allocations; copy_from is exhaustive and shares immutable flash.
    let mut snapshot = deferred.clone();
    let mut next_rx = 101;
    let mut steps = 0;
    while oracle.cycles < end {
        if feed_rx && oracle.cycles >= next_rx {
            deferred.catch_up();
            assert_eq!(deferred.cycles, oracle.cycles, "{name}: input timestamp");
            for byte in [next_rx as u8, (next_rx >> 8) as u8, 0xa5, 0x5a] {
                oracle.rx(byte); deferred.rx(byte);
            }
            next_rx += 2101;
        }
        assert_eq!(oracle.will_execute(), deferred.will_execute_deferred(), "{name}: execution gate");
        let expected = oracle.step_until(end);
        let actual = deferred.step_deferred(end);
        assert_eq!(actual, expected, "{name}: result at cycle {}", oracle.cycles);
        snapshot.copy_from(&deferred);
        snapshot.catch_up();
        assert_eq!(oracle.same_state(&snapshot), None,
            "{name}: state at cycle {}, PC {}", oracle.cycles, oracle.pc);
        if let Err(stop) = expected {
            if stop != Stop::Cycles { return oracle; }
        }
        steps += 1;
        if steps % 256 == 0 {
            // Bound comparison/copy cost, after proving every journal entry. There are
            // still 256 consecutive deferred steps between these external boundaries.
            deferred.catch_up();
            oracle.events.clear(); deferred.events.clear();
            oracle.set_cosim_timing(cosim); deferred.set_cosim_timing(cosim);
        }
    }
    oracle
}

#[test]
fn deferred_matches_every_instruction_boundary() {
    for (name, image, feed_rx) in [
        ("timer0", timer0(), false), ("timer1", timer1(), false),
        ("timer2", timer2(), false), ("usart", usart(), true), ("mixed", mixed(), false),
        ("spi", spi(), false), ("eeprom", eeprom(), false),
    ] {
        for cosim in [false, true] {
            let cpu = Cpu::new(&image);
            let name = format!("{name}/{}", if cosim { "cosim" } else { "oracle" });
            let cpu = lockstep(&name, cpu, END, feed_rx, cosim);
            assert!(cpu.cycles >= END, "{name}: firmware runs through the full budget");
            if name.starts_with("spi/") {
                assert_eq!((cpu.r[21], cpu.r[20]), (1, 1), "{name}: both the polling loop and SPI ISR execute");
            }
            if name.starts_with("eeprom/") {
                assert_eq!(&cpu.eeprom()[..3], &[0xa5, 0x5a, 0x3c], "{name}: all three writes commit");
                assert_eq!(&cpu.r[18..22], &[0xa5, 0x5a, 0x3c, 1], "{name}: polling, reads and ready ISR execute");
            }
        }
    }
}

#[test]
fn peripheral_stack_addresses_and_stops_match() {
    // Calls can straddle SRAM/I/O; arbitrary stack placement is legal in this oracle.
    // Timer-backed stack bytes need not return a sensible PC: compare any resulting stop.
    for sp in [0x1f, 0x20, 0x25, 0x46, 0x84, 0xff, 0x100, 0x101, 0x8ff] {
        for op in [Push(16), Pop(17), Call(4), Rcall(3), Icall, Ret, Reti] {
            let mut a = Asm::new();
            a.emit(op); a.emit(Nop); a.emit(Break);
            a.org(4); a.emit(Ret);
            let image = a.finish().unwrap();
            for cosim in [false, true] {
                let mut cpu = Cpu::new(&image);
                cpu.sp = sp; cpu.r[16] = 0x80; cpu.r[30] = 4;
                cpu.write(0x45, 2); cpu.write(0x81, 2);
                let name = format!("stack {sp:#x} {op:?}/{}", if cosim { "cosim" } else { "oracle" });
                lockstep(&name, cpu, 40, false, cosim);
            }
        }
    }
}

#[test]
fn patched_flash_rebuilds_the_shared_table() {
    let mut a = Asm::new();
    a.emit(Ldi(16, 1)); a.emit(Nop); a.emit(Nop); a.emit(Rjmp(-4));
    let mut oracle = Cpu::new(&a.finish().unwrap());
    let mut deferred = oracle.clone();
    for _ in 0..8 {
        assert_eq!(deferred.step_deferred(100), oracle.step_until(100));
    }
    deferred.catch_up();
    assert_eq!(deferred.same_state(&oracle), None);
    let mut old = deferred.clone(); // retains the old flash and its already-built table
    let old_oracle = oracle.clone();
    let (load, _) = encode(&Ldi(16, 7)).unwrap();
    let (store, _) = encode(&Sts(0x100, 16)).unwrap();
    for cpu in [&mut oracle, &mut deferred] {
        cpu.flash[0] = load[0];
        cpu.flash[1] = store[0];
        cpu.flash[2] = store[1]; // turns two one-word instructions into a two-word one
    }
    while oracle.cycles < 100 {
        assert_eq!(deferred.step_deferred(100), oracle.step_until(100));
        let mut boundary = deferred.clone(); boundary.catch_up();
        assert_eq!(boundary.same_state(&oracle), None, "patched instruction boundary");
    }
    assert_eq!(oracle.sram[0], 7, "the patched store executed");
    let mut expected = old_oracle;
    assert_eq!(old.step_deferred(100), expected.step_until(100));
    old.catch_up();
    assert_eq!(old.same_state(&expected), None, "the snapshot still runs its old image");
}

#[test]
fn table_preserves_operand_decodes_and_fetch_errors() {
    let last = FLASH_WORDS as u32 - 1;
    let cpse = encode(&Cpse(0, 0)).unwrap().0[0];
    let sbic = encode(&Sbic(5, 0)).unwrap().0[0];
    for (pc, at, words, expected) in [
        (0, 0, vec![0xffff], Err(Stop::BadInsn { pc: 0 })),
        (1, 0, vec![0x940c, 0x9409], Ok(())), // JMP's operand decodes as IJMP
        (0, 0, vec![cpse, 0xffff, 0], Ok(())), // skip does not decode its discarded word
        (last, last, vec![0x940c], Err(Stop::BadAddress { pc: last, addr: last + 1 })),
        (last - 1, last - 1, vec![sbic, 0x940c], Err(Stop::BadAddress { pc: last, addr: last + 1 })),
        (last, last, vec![cpse], Err(Stop::BadAddress { pc: last + 1, addr: last + 1 })),
        (last + 1, 0, vec![], Err(Stop::BadAddress { pc: last + 1, addr: last + 1 })),
    ] {
        let mut oracle = Cpu::new(&[]);
        oracle.pc = pc;
        oracle.flash[at as usize..at as usize + words.len()].copy_from_slice(&words);
        let mut deferred = oracle.clone();
        assert_eq!(oracle.step_until(100), expected, "oracle at {pc}");
        assert_eq!(deferred.step_deferred(100), expected, "table at {pc}");
        deferred.catch_up();
        assert_eq!(deferred.same_state(&oracle), None, "including fault timing at {pc}");
    }
}

fn oracle_run(cpu: &mut Cpu, until: &Until, drives: &mut Vec<DriveEdge>, out: &mut Vec<LinkOut>,
    trace: &mut Vec<(u64, u32)>) -> Ran {
    let generation = cpu.drive_gen();
    cpu.take_drive_at();
    loop {
        if until.on_drive && cpu.drive_gen() != generation {
            return Ran::Drive { at: cpu.take_drive_at().unwrap() };
        }
        let marked = until.marks.contains(cpu.pc) && cpu.will_execute();
        if marked && cpu.cycles > until.stops_after && until.stops.binary_search(&cpu.pc).is_ok() {
            return Ran::Pc;
        }
        if cpu.cycles >= until.tick { return Ran::Tick; }
        if marked { trace.push((cpu.cycles, cpu.pc)); }
        let before_ddr = [cpu.io(0x24), cpu.io(0x27), cpu.io(0x2a)];
        let result = cpu.step_until(until.tick);
        assert!(matches!(result, Ok(()) | Err(Stop::Cycles)), "oracle machine fixture: {result:?}");
        // This fixture configures DDR before starting its timers, with PORT low.
        // Enabling a low output changes its drive without creating a PIN event.
        for (pin, desc) in chip::PINS.iter().enumerate() {
            let p = usize::from(desc.port as u8 - b'B');
            let ddr = cpu.io(0x24 + 3 * p as u8);
            let mask = 1 << desc.bit;
            if (before_ddr[p] ^ ddr) & mask != 0 {
                drives.push(DriveEdge { tick: cpu.cycles, pin: pin as u16,
                    drive: if ddr & mask != 0 { Drive::Low } else { Drive::Input(Pull::None) } });
            }
        }
        // All subsequent drive changes are timer edges. Keep their hardware cycles,
        // including edges strictly inside an instruction, rather than its end cycle.
        for event in cpu.events.drain(..) {
            match event {
                Event::Pin { cycle, port, bit, high } => {
                    let pin = chip::PINS.iter().position(|p| p.port == port && p.bit == bit).unwrap();
                    drives.push(DriveEdge { tick: cycle, pin: pin as u16,
                        drive: if high { Drive::High } else { Drive::Low } });
                }
                Event::Tx { cycle, byte } => out.push(LinkOut { link: 0, tick: cycle, byte }),
                Event::TxDone { .. } => {}
            }
        }
    }
}

#[test]
fn machine_slices_match_oracle_drives_links_and_state() {
    let image = timer0();
    let mut machine = AvrMachine::new(&image, 16_000_000);
    let mut oracle = machine.cpu().clone();
    let mut marks = PcSet::new(0, image.len() as u32);
    for pc in [64, 192] { marks.insert(pc); }
    let mut drives = Vec::new(); let mut links = Vec::new(); let mut trace = Vec::new();
    let mut slice = 0;
    while machine.now() < END {
        let until = Until { tick: (machine.now() + [1, 7, 131, 4093][slice % 4]).min(END),
            marks: &marks, stops: &[192], stops_after: machine.now(), on_drive: slice % 3 == 0 };
        let expected = oracle_run(&mut oracle, &until, &mut drives, &mut links, &mut trace);
        assert_eq!(machine.run(&until), expected, "slice {slice}: return reason");
        assert_eq!(machine.cpu().same_state(&oracle), None, "slice {slice}: caught-up state");
        slice += 1;
    }
    let mut actual_drives = Vec::new(); machine.take_drives(&mut actual_drives);
    let mut actual_links = Vec::new(); machine.take_out(&mut actual_links);
    let mut actual_trace = Vec::new(); machine.take_trace(&mut actual_trace);
    assert!(!drives.is_empty() && !links.is_empty() && !trace.is_empty(), "fixture exercises all journals");
    assert_eq!(actual_drives, drives, "drive log preserves every edge and timestamp");
    assert_eq!(actual_links, links, "link bytes and start cycles");
    assert_eq!(actual_trace, trace, "marks execute only on instruction steps");
    // take_drives drained only the machine's private drive journal; clear the already
    // compared oracle journal via the existing timing setter before the final comparison.
    oracle.set_cosim_timing(true);
    assert_eq!(machine.cpu().same_state(&oracle), None, "final state after journal drains");
}
