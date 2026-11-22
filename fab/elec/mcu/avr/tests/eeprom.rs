//! DS40002061B §8.4: EEPROM bus timing, modes, guards, interrupts and snapshots.
use fab_elec_mcu_avr::{asm::Asm, insn::Insn::*, machine::AvrMachine, sim::{Cpu, Stop, EEPROM}};
use fab_elec_mcu_emu::{ChipState, Machine, PcSet, Pending, Ran, Until, Wake};

const EECR: u16 = 0x3f;
const EEDR: u16 = 0x40;
const EEARL: u16 = 0x41;
const EEARH: u16 = 0x42;
const EEIO: u8 = (EECR - 0x20) as u8;

fn store(a: &mut Asm, addr: u16, value: u8) {
    a.emit(Ldi(16, value)); a.emit(Sts(addr, 16));
}

// Word zero is a NOP for probes. The real write sequence starts at word one.
fn writer() -> Cpu {
    let mut a = Asm::new();
    a.emit(Nop); a.emit(Sbi(EEIO, 2)); a.emit(Sbi(EEIO, 1)); a.emit(Inc(17));
    let mut cpu = Cpu::new(&a.finish().unwrap());
    cpu.pc = 1;
    cpu
}

fn to(cpu: &mut Cpu, tick: u64) {
    assert!(cpu.cycles <= tick, "single-clock deadline is ahead");
    while cpu.cycles < tick { cpu.pc = 0; cpu.step().expect("NOP reaches the next clock"); }
}

#[test]
fn erased_and_loaded_reads_halt_the_cpu_but_not_peripherals() {
    let mut a = Asm::new();
    a.emit(Sbi(EEIO, 0)); a.emit(Inc(17));
    let mut cpu = Cpu::new(&a.finish().unwrap());
    assert_eq!(cpu.eeprom(), &[0xff; EEPROM], "a new chip is erased throughout");
    cpu.write(0x45, 1); // Timer0 at the CPU clock, to observe the halt clocks.
    let fresh = cpu.clone();
    cpu.step().unwrap();
    assert_eq!(cpu.read(EEDR), 0xff, "EERE reads the erased byte");
    assert_eq!((cpu.cycles, cpu.pc, cpu.r[17]), (6, 1, 0), "SBI plus four halted clocks precede the next instruction");
    assert_eq!(cpu.read(0x46), 6, "Timer0 advances throughout the read halt");
    assert_eq!(cpu.read(EECR), 0, "EERE is a self-clearing strobe");
    cpu.step().unwrap();
    assert_eq!((cpu.cycles, cpu.r[17]), (7, 1), "the next instruction follows the read halt");

    let mut bytes = [0xff; EEPROM]; bytes[1023] = 0xa5;
    cpu.copy_from(&fresh);
    cpu.set_eeprom(&bytes);
    assert_eq!(cpu.eeprom(), &bytes, "host-loaded committed bytes are returned unchanged");
    cpu.write(EEARH, 0xff); cpu.write(EEARL, 0xff);
    assert_eq!(cpu.read(EEARH), 3, "only EEAR9:8 are implemented in EEARH");
    cpu.pc = 0; cpu.step().unwrap();
    assert_eq!(cpu.read(EEDR), 0xa5, "all ten address bits select the host-loaded byte");
    assert_eq!(cpu.cycles, 6, "a loaded read has the same four-clock halt");
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cpu.set_eeprom(&[0; 1]))).is_err(),
        "host images with the wrong length are rejected");
    assert_eq!(cpu.eeprom(), &bytes, "a rejected image leaves committed bytes intact");
}

#[test]
fn sbi_write_sequence_commits_at_the_nominal_oscillator_deadline() {
    for (clock, duration) in [(16_000_000, 54_400), (8_000_000, 27_200), (1_000_001, 3401)] {
        let mut cpu = writer();
        if clock != 16_000_000 { cpu.set_clock_hz(clock); } // Also proves the standalone default.
        cpu.write(EEARL, 7); cpu.write(EEDR, 0x5a); cpu.write(0x45, 1);
        cpu.step().unwrap();
        assert_eq!((cpu.cycles, cpu.read(EECR)), (2, 4), "{clock}: SBI EEMPE opens the window at completion");
        cpu.step().unwrap();
        let start = 4;
        assert_eq!((cpu.cycles, cpu.pc, cpu.r[17]), (start + 2, 3, 0), "{clock}: EEPE store adds two halt clocks");
        assert_eq!(cpu.read(0x46), 6, "{clock}: peripherals advance during the write halt");
        assert_eq!(cpu.read(EECR), 2, "{clock}: busy is set, and SBI EEPE did not renew EEMPE");
        cpu.step().unwrap();
        assert_eq!((cpu.cycles, cpu.r[17]), (7, 1), "{clock}: the next instruction follows the write halt");
        to(&mut cpu, start + duration - 1);
        assert_eq!(cpu.read(EECR) & 2, 2, "{clock}: EEPE remains set one clock before completion");
        assert_eq!(cpu.eeprom()[7], 0xff, "{clock}: committed bytes do not change early");
        to(&mut cpu, start + duration);
        assert_eq!(cpu.read(EECR) & 2, 0, "{clock}: EEPE clears at the exact write deadline");
        assert_eq!(cpu.eeprom()[7], 0x5a, "{clock}: erase-and-write commits at that same deadline");
    }
}

#[test]
fn programming_modes_erase_write_and_refuse_reserved_mode() {
    let base = writer();
    for (mode, expected) in [(1, 0xff), (2, 0x24), (3, 0xa5)] {
        let mut cpu = base.clone();
        let mut bytes = [0xff; EEPROM]; bytes[0] = 0xa5;
        cpu.set_eeprom(&bytes); cpu.write(EEDR, 0x3c); cpu.write(EECR, mode << 4);
        cpu.step().unwrap();
        let result = cpu.step();
        if mode == 3 {
            assert_eq!(result, Err(Stop::Unmodeled { pc: 2, what: "EEPROM reserved programming mode (EEPM=11)" }),
                "reserved mode reports why it did not write");
            assert_eq!(cpu.cycles, 4, "reserved mode adds no write halt");
            assert_eq!(cpu.read(EECR) & 2, 0, "reserved mode never becomes busy");
        } else {
            result.unwrap();
            to(&mut cpu, 4 + 28_800 - 1);
            assert_eq!((cpu.read(EECR) & 2, cpu.eeprom()[0]), (2, 0xa5), "mode {mode}: no early commit");
            to(&mut cpu, 4 + 28_800);
            assert_eq!(cpu.read(EECR) & 2, 0, "mode {mode}: 1.8 ms write has finished");
        }
        assert_eq!(cpu.eeprom()[0], expected, "mode {mode}: erase, old AND data, or no write");
    }
}

#[test]
fn unlock_window_and_busy_guards_preserve_the_latched_write() {
    let mut a = Asm::new();
    a.emit(Nop);
    a.emit(Sbi(EEIO, 2)); a.emit(Nop); a.emit(Sbi(EEIO, 1));
    a.emit(Sbi(EEIO, 2)); a.emit(Nop); a.emit(Nop); a.emit(Sbi(EEIO, 1));
    a.emit(Sbi(EEIO, 2)); a.emit(Nop); a.emit(Nop); a.emit(Nop); a.emit(Sbi(EEIO, 1));
    let base = Cpu::new(&a.finish().unwrap());
    let mut cpu = base.clone();
    cpu.write(EEDR, 0x3c); cpu.write(EECR, 0xc2);
    assert_eq!((cpu.cycles, cpu.read(EECR)), (0, 0), "EEPE alone does nothing; reserved bits read zero");
    cpu.write(EECR, 6);
    assert_eq!((cpu.cycles, cpu.read(EECR)), (0, 4), "one store cannot both unlock and start a write");
    to(&mut cpu, 3);
    assert_eq!(cpu.read(EECR), 4, "EEMPE stays set until its fourth clock");
    to(&mut cpu, 4);
    assert_eq!(cpu.read(EECR), 0, "hardware clears EEMPE at c + 4");
    for (pc, gap) in [(1, 3), (4, 4), (8, 5)] {
        cpu.copy_from(&base); cpu.pc = pc;
        for _ in 0..gap { cpu.step().unwrap(); }
        assert_eq!(cpu.read(EECR) & 2, if gap == 3 { 2 } else { 0 },
            "EEPE store {gap} clocks after EEMPE must be strictly inside the window");
        assert_eq!(cpu.eeprom()[0], 0xff, "unlock probes never commit early");
    }

    cpu = writer(); cpu.write(EEARH, 2); cpu.write(EEARL, 7); cpu.write(EEDR, 0x3c);
    cpu.step().unwrap(); cpu.step().unwrap();
    cpu.write(EEARH, 1); cpu.write(EEARL, 8); cpu.write(EEDR, 0x96);
    let before = cpu.cycles;
    cpu.write(EECR, 0xff); // Re-arm EEMPE too, so a second EEPE is genuinely unlocked.
    cpu.write(EECR, 0x3b);
    assert_eq!(cpu.cycles, before, "busy EERE and EEPE add no CPU halt");
    assert_eq!((cpu.read(EEARH), cpu.read(EEARL)), (2, 7), "busy EEAR writes are ignored");
    assert_eq!(cpu.read(EEDR), 0x96, "EEDR remains writable and busy EERE cannot overwrite it");
    assert_eq!(cpu.read(EECR), 0x0a, "busy EEPM is locked, EEPE remains set, and EERIE is writable");
    to(&mut cpu, 4 + 54_400);
    assert_eq!(cpu.read(EECR), 8, "a second EEPE did not extend completion");
    assert_eq!(cpu.eeprom()[0x207], 0x3c, "completion uses the original address, value and mode");
    assert_eq!(cpu.eeprom()[0x108], 0xff, "attempted busy address never receives a write");
}

#[test]
fn ready_interrupt_wakes_at_completion_and_remains_level_triggered() {
    let mut a = Asm::new();
    a.emit(Sleep); a.emit(Inc(18)); a.emit(Sleep);
    a.org(44); a.emit(Inc(17)); a.emit(Cpi(17, 2)); a.emit(Brbc(1, 1));
    a.emit(Cbi(EEIO, 3)); a.emit(Reti);
    let mut cpu = Cpu::new(&a.finish().unwrap());
    cpu.write(EEDR, 0x5a); cpu.write(EECR, 0x0c); cpu.write(EECR, 0x0a);
    cpu.sreg = 0x80;
    cpu.step().unwrap();
    assert_eq!(cpu.wake_at(), Some(54_400), "ready wake projects the write end without entry latency");
    assert_eq!(cpu.step_until(54_399), Err(Stop::Cycles), "sleep stops before completion without running code");
    assert_eq!((cpu.pc, cpu.r[17], cpu.read(EECR) & 2), (1, 0, 2), "CPU stays asleep throughout the write");
    cpu.step().unwrap();
    assert_eq!((cpu.pc, cpu.cycles), (44, 54_408), "EE READY enters vector 44 after four wake and four entry clocks");
    assert_eq!(cpu.eeprom()[0], 0x5a, "the write commits before interrupt entry");
    for _ in 0..4 { cpu.step().unwrap(); }
    assert_eq!((cpu.pc, cpu.r[17]), (1, 1), "first handler returns without acknowledging the level");
    cpu.step().unwrap();
    assert_eq!(cpu.r[18], 1, "RETI permits one main instruction before the pending level re-enters");
    cpu.step().unwrap();
    assert_eq!(cpu.pc, 44, "EE READY is still pending after the first entry");
    for _ in 0..5 { cpu.step().unwrap(); }
    assert_eq!((cpu.pc, cpu.r[17], cpu.read(EECR)), (2, 2, 0), "second handler clears EERIE and returns");
    assert_eq!(cpu.step(), Err(Stop::Sleep), "clearing EERIE stops repeated interrupts");
    assert_eq!(cpu.wake_at(), None, "no EEPROM wake remains when EERIE is clear");
}

#[test]
fn snapshots_share_old_bytes_restore_inflight_writes_and_rewind_the_machine() {
    let mut cpu = writer(); cpu.set_clock_hz(8_000_000);
    cpu.write(EEARL, 7); cpu.write(EEDR, 0x5a);
    let before = cpu.clone();
    cpu.step().unwrap();
    let unlocked = cpu.clone();
    cpu.step().unwrap(); to(&mut cpu, 100);
    let mut restored = Cpu::new(&[]); restored.copy_from(&cpu);
    assert_eq!(restored.same_state(&cpu), None, "copy_from restores clock and in-flight EEPROM state");
    assert!(std::ptr::eq(cpu.eeprom().as_ptr(), before.eeprom().as_ptr()), "uncommitted writes share the snapshot's bytes");
    for side in [&mut cpu, &mut restored] { to(side, 4 + 27_200); }
    assert_eq!(restored.same_state(&cpu), None, "both sides complete with identical state and bytes");
    assert_eq!(restored.eeprom()[7], 0x5a, "restored write commits the latched data");
    assert_eq!(before.eeprom()[7], 0xff, "a pre-write snapshot never sees a later commit");
    let done = cpu.clone(); cpu.copy_from(&unlocked);
    cpu.step().unwrap(); to(&mut cpu, 4 + 27_200);
    assert_eq!(cpu.same_state(&done), None, "restoring EEMPE also reproduces the timed unlock and write");

    let mut a = Asm::new();
    store(&mut a, EEARL, 7); store(&mut a, EEDR, 0x3c);
    a.emit(Sbi(EEIO, 2)); a.emit(Sbi(EEIO, 1));
    a.emit(Sbi(EEIO, 3)); a.emit(Bset(7)); a.emit(Sleep); a.emit(Rjmp(-1));
    a.org(44); a.emit(Cbi(EEIO, 3)); a.emit(Reti);
    let mut machine = AvrMachine::new(&a.finish().unwrap(), 8_000_000);
    let mut bytes = [0xff; EEPROM]; bytes[7] = 0xa5;
    machine.set_eeprom(&bytes);
    assert_eq!(machine.eeprom(), &bytes, "machine host API loads all EEPROM bytes");
    let marks = PcSet::default();
    let until = |tick| Until { tick, marks: &marks, stops: &[], stops_after: 0, on_drive: false };
    assert_eq!(machine.run(&until(16)), Ran::Tick, "machine reaches sleep with a write in flight");
    let mut state = ChipState::default(); machine.chip(&mut state);
    assert_eq!(state.wake, Some(Wake { tick: 27_210, source: "EE_READY", vector: 44 }), "machine clock sets and names the EEPROM wake");
    machine.mark();
    assert_eq!(machine.run(&until(27_210)), Ran::Tick, "machine reaches the exact completion boundary");
    assert_eq!(machine.eeprom()[7], 0x3c, "machine commits at 8 MHz after 27,200 cycles");
    machine.chip(&mut state);
    assert_eq!(state.pending, [Pending::Irq { source: "EE_READY", vector: 44 }], "machine names the ready level");
    let done = machine.cpu().clone();
    machine.rewind();
    assert_eq!(machine.eeprom()[7], 0xa5, "rewind restores the pre-completion bytes");
    assert_eq!(machine.run(&until(27_210)), Ran::Tick, "rewound machine reaches completion again");
    assert_eq!(machine.cpu().same_state(&done), None, "rewind replays the same EEPROM and CPU state");
}
