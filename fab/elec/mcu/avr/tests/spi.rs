//! DS40002061B §19: exact SPI master edges, register side effects and circuit drives.
use fab_elec_mcu_avr::{asm::Asm, insn::Insn::*, machine::AvrMachine, sim::{Cpu, Event, Stop}};
use fab_elec_mcu_emu::{ChipState, Drive, DriveEdge, Machine, PcSet, Pending, PinUse, Pull, Ran, Until};

const SPCR: u16 = 0x4c;
const SPSR: u16 = 0x4d;
const SPDR: u16 = 0x4e;

fn store(a: &mut Asm, addr: u16, value: u8) {
    a.emit(Ldi(16, value)); a.emit(Sts(addr, 16));
}

fn poll(a: &mut Asm) {
    a.emit(In(18, 0x2d)); a.emit(Sbrs(18, 7)); a.emit(Rjmp(-3));
}

// Word zero is a NOP for single-clock probes; the rest sends two bytes using real polling.
fn configured(control: u8, double: u8, byte: u8) -> (Cpu, u32) {
    let mut a = Asm::new();
    a.emit(Nop);
    for (addr, value) in [(0x24, 0x2c), (SPCR, control), (SPSR, double), (SPDR, byte)] {
        store(&mut a, addr, value);
    }
    poll(&mut a);
    // The first status read arms clearing; a write both acknowledges and starts the next byte.
    a.emit(Ldi(16, 0x3c));
    let second = a.here(); a.emit(Out(0x2e, 16));
    poll(&mut a); a.emit(In(19, 0x2e)); a.emit(Break);
    let mut cpu = Cpu::new(&a.finish().unwrap());
    cpu.pc = 1;
    for _ in 0..8 { cpu.step().unwrap(); }
    assert_eq!(cpu.cycles, 12, "SPDR STS takes effect after four LDI/STS pairs");
    cpu.events.retain(|e| matches!(e, Event::Pin { cycle: 12, .. }));
    (cpu, second)
}

fn to(cpu: &mut Cpu, tick: u64) {
    assert!(cpu.cycles <= tick, "single-clock deadline is ahead");
    while cpu.cycles < tick { cpu.pc = 0; cpu.step().expect("NOP reaches the next clock"); }
}

fn pins(cpu: &Cpu) -> Vec<(u64, u8, bool)> {
    cpu.events.iter().filter_map(|e| match *e {
        Event::Pin { cycle, port: 'B', bit: bit @ (3 | 5), high } => Some((cycle, bit, high)),
        _ => None,
    }).collect()
}

// Datasheet waveform: list MOSI changes at the shift instants and every SCK half-period.
fn waveform(start: u64, div: u64, control: u8, byte: u8, mut mosi: bool) -> Vec<(u64, u8, bool)> {
    let cpol = control & 8 != 0;
    let cpha = control & 4 != 0;
    let mut edges = Vec::new();
    for k in 0..8 {
        let high = byte & (1 << if control & 0x20 != 0 { k } else { 7 - k }) != 0;
        if high != mosi {
            edges.push((start + k * div + if cpha { div / 2 } else { 0 }, 3, high));
            mosi = high;
        }
        edges.push((start + k * div + div / 2, 5, !cpol));
        edges.push((start + (k + 1) * div, 5, cpol));
    }
    edges.sort_by_key(|&(tick, bit, _)| (tick, bit));
    edges
}

#[test]
fn mode_zero_exact_edges_polling_and_back_to_back_bytes() {
    for (double, spr, div) in [(1, 0, 2), (0, 1, 16)] {
        let (mut cpu, second) = configured(0x50 | spr, double, 0xa5);
        let start = cpu.cycles;
        let mut probe = cpu.clone();
        for bit in 0..8 {
            let leading = start + bit * div + div / 2;
            to(&mut probe, leading);
            assert_eq!(probe.read(0x23) & 0x28, 0x20 | if 0xa5 & (0x80 >> bit) != 0 { 8 } else { 0 },
                "div {div}, bit {bit}: next MSB is present on each leading edge");
        }
        to(&mut probe, start + 8 * div - 1);
        assert_eq!(probe.io(SPSR as u8) & 0x80, 0, "div {div}: no early SPIF");
        to(&mut probe, start + 8 * div);
        assert_eq!(probe.io(SPSR as u8) & 0x80, 0x80, "div {div}: SPIF at eighth trailing edge");
        assert_eq!(probe.read(0x23) & 0x28, 8, "div {div}: idle SCK low and last MOSI bit held");
        to(&mut probe, start + 8 * div + 4);
        assert_eq!(pins(&probe), waveform(start, div, 0x50, 0xa5, false), "div {div}: every change and idle hold");

        let first_poll = cpu.pc;
        let mut seen = None;
        let mut second_start = None;
        while cpu.cycles < start + 16 * div + 40 {
            let pc = cpu.pc;
            let result = cpu.step();
            if pc == first_poll && cpu.r[18] & 0x80 != 0 { seen = Some(cpu.cycles); }
            if pc == second {
                second_start = Some(cpu.cycles);
                assert_eq!(cpu.io(SPSR as u8) & 0xc0, 0, "div {div}: status-read/data-write clears flags");
            }
            if result == Err(Stop::Break) { break; }
            result.unwrap();
        }
        assert_eq!(seen, Some(start + 8 * div + 1), "div {div}: first polling IN after completion sees SPIF");
        let second_start = second_start.expect("polling starts the second byte");
        assert_eq!(second_start, start + 8 * div + 5, "div {div}: next byte follows the successful poll directly");
        let mut expected = waveform(start, div, 0x50, 0xa5, false);
        expected.extend(waveform(second_start, div, 0x50, 0x3c, true));
        assert_eq!(pins(&cpu), expected, "div {div}: both transmitted bytes preserve every edge");
        assert_eq!(cpu.io(SPSR as u8) & 0xc0, 0, "div {div}: status-read/data-read clears completion");
        assert_eq!(cpu.read(0x23) & 0x28, 0, "div {div}: second byte holds its zero last bit");
    }
}

#[test]
fn receive_samples_miso_at_the_selected_edges_despite_ddr() {
    for mode in [0, 4, 0x20] {
        let (mut cpu, _) = configured(0x50 | mode, 1, 0xa5);
        let start = cpu.cycles;
        cpu.write(0x24, 0x3c); // DDRB4 cannot turn master MISO into an output.
        cpu.write(0x25, 0x10);
        assert_eq!(cpu.io(0x24), 0x3c, "mode {mode}: forced input preserves the DDR register");
        assert_eq!(cpu.drive('B').0 & 0x10, 0, "mode {mode}: circuit-facing MISO stays input");
        assert_eq!(cpu.pin_use(0, 4), (PinUse::PullUp, 0.0), "mode {mode}: MISO pull-up follows PORT");
        assert_eq!(cpu.read(0x23) & 0x10, 0x10, "mode {mode}: undriven MISO reads its pull-up");
        cpu.write(0x55, 0x10);
        assert_eq!(cpu.read(0x23) & 0x10, 0, "mode {mode}: PUD disables the MISO pull-up");
        assert_eq!(cpu.pin_use(0, 4), (PinUse::Input, 0.0), "mode {mode}: PUD reflected in pin use");
        for bit in 0..8 {
            let sample = start + 2 * bit + if mode & 4 != 0 { 2 } else { 1 };
            to(&mut cpu, sample - 1);
            let high = 0x96 & (1 << if mode & 0x20 != 0 { bit } else { 7 - bit }) != 0;
            cpu.set_pin('B', 4, high);
            to(&mut cpu, sample);
            assert_eq!(cpu.read(0x23) & 0x10 != 0, high, "mode {mode}, bit {bit}: sample is PINB's level");
            cpu.set_pin('B', 4, !high); // A change after sampling must not alter that bit.
        }
        to(&mut cpu, start + 16);
        assert_eq!(cpu.read(SPDR), 0x96, "mode {mode}: receive byte assembles sampling-edge levels in DORD order");
        assert_eq!(cpu.io(SPSR as u8) & 0x80, 0x80, "mode {mode}: SPDR alone does not clear SPIF");
    }
}

#[test]
fn collision_flags_and_snapshots_preserve_the_running_transfer() {
    let (mut cpu, _) = configured(0x51, 0, 0xa5);
    let start = cpu.cycles;
    to(&mut cpu, start + 9);
    cpu.write(SPDR, 0x00);
    assert_eq!(cpu.read(SPSR), 0x40, "busy write sets WCOL without SPIF");
    cpu.read(SPDR);
    assert_eq!(cpu.io(SPSR as u8), 0x40, "status read without SPIF does not arm the specified clearing sequence");
    cpu.write(SPSR, 0xff);
    assert_eq!(cpu.io(SPSR as u8), 0x41, "only SPI2X is writable; reserved bits stay zero");
    cpu.write(SPSR, 0);
    assert_eq!(cpu.io(SPSR as u8), 0x40, "writing zero cannot clear WCOL");
    let mark = cpu.clone();
    to(&mut cpu, start + 128);
    assert_eq!(pins(&cpu), waveform(start, 16, 0x50, 0xa5, false), "collision and rate writes do not disturb the active byte");
    assert_eq!(cpu.io(SPSR as u8), 0xc0, "completion sets SPIF alongside WCOL");
    cpu.read(SPDR);
    assert_eq!(cpu.io(SPSR as u8), 0xc0, "reversed clearing order leaves both flags set");
    let done = cpu.clone();
    cpu.copy_from(&mark);
    assert_eq!(cpu.same_state(&mark), None, "copy_from restores active shift state and flags");
    to(&mut cpu, start + 128);
    assert_eq!(cpu.same_state(&done), None, "restored transfer reproduces its bits, receive byte and completion");
    assert_eq!(cpu.read(SPSR), 0xc0, "read status arms both flag clears");
    let armed = cpu.clone();
    cpu.read(SPDR);
    assert_eq!(cpu.io(SPSR as u8), 0, "status/data sequence clears SPIF and WCOL");
    cpu.copy_from(&armed); cpu.read(SPDR);
    assert_eq!(cpu.io(SPSR as u8), 0, "snapshot also restores the armed read sequence");
}

#[test]
fn phase_polarity_bit_order_and_all_remaining_rates() {
    for (control, double, div) in [(0x54, 0, 4), (0x72, 0, 64), (0x7f, 0, 128),
        (0x59, 1, 8), (0x56, 1, 32), (0x5b, 1, 64)] {
        let (mut cpu, _) = configured(control, double, 0x96);
        let start = cpu.cycles;
        assert_eq!(cpu.read(0x23) & 8 != 0, control & 4 == 0 && control & 0x20 == 0,
            "control {control:#x}: CPHA=0 presents the first bit immediately; CPHA=1 waits");
        to(&mut cpu, start + 8 * div - 1);
        assert_eq!(cpu.io(SPSR as u8) & 0x80, 0, "control {control:#x}: selected divisor has not finished early");
        to(&mut cpu, start + 8 * div);
        assert_eq!(cpu.io(SPSR as u8) & 0x80, 0x80, "control {control:#x}: selected divisor completes at eight periods");
        assert_eq!(pins(&cpu), waveform(start, div, control, 0x96, false),
            "control {control:#x}: selected shift edge, clock polarity, bit order and divisor");
        assert_eq!(cpu.read(0x23) & 8 != 0, control & 0x20 != 0, "control {control:#x}: holds last transmitted bit");
        cpu.events.clear();
        cpu.write(SPCR, control ^ 8);
        assert_eq!(pins(&cpu), [(cpu.cycles, 5, control & 8 == 0)], "control {control:#x}: idle CPOL write moves SCK immediately");
    }
}

#[test]
fn slave_select_disable_and_power_reduction_release_or_stop_the_master() {
    let (mut cpu, _) = configured(0x50, 1, 0xa5);
    cpu.write(0x25, 0x38);
    cpu.write(0x24, 0x38); // SS input, MISO DDR output, MOSI/SCK DDR outputs.
    assert_eq!(cpu.read(SPCR) & 0x10, 0, "low input SS clears MSTR at the DDR write");
    assert_eq!(cpu.read(SPSR) & 0x80, 0x80, "SS demotion sets SPIF");
    assert_eq!(cpu.drive('B').0 & 0x3c, 0x10, "slave directions force SS/MOSI/SCK input, retain MISO DDR");
    assert_eq!(cpu.read(0x23) & 0x38, 0x38, "released outputs read PORT pull-ups and slave MISO follows PORT");
    cpu.read(SPDR); cpu.write(SPDR, 0xff);
    to(&mut cpu, 40);
    assert_eq!(cpu.io(SPSR as u8) & 0x80, 0, "slave writes and the aborted transfer never complete");

    cpu.set_pin('B', 2, true);
    cpu.write(SPCR, 0x50); cpu.write(SPDR, 0x80);
    to(&mut cpu, 41);
    cpu.set_pin('B', 2, false);
    assert_eq!(cpu.read(SPCR) & 0x10, 0, "external low SS also clears MSTR during a transfer");
    assert_eq!(cpu.read(SPSR) & 0x80, 0x80, "external SS mode fault sets SPIF");
    cpu.read(SPDR);
    cpu.write(0x24, 0x3c); cpu.write(SPCR, 0x50);
    assert_eq!(cpu.read(0x23) & 0x28, 0, "new master session starts with MOSI low and idle SCK");
    // SS is driven low externally but now an output, so it must not demote the master.
    for abort in [SPCR, 0x64] {
        cpu.write(SPDR, 0x80);
        let end = cpu.cycles + 16;
        to(&mut cpu, end - 15);
        cpu.write(abort, if abort == SPCR { 0 } else { 4 });
        if abort == SPCR {
            assert_eq!(cpu.drive('B').0, 0x3c, "SPE clear restores every DDR bit immediately");
            assert_eq!(cpu.read(0x23) & 0x38, 0x38, "SPE clear immediately restores PORT output levels");
        } else {
            assert_eq!(cpu.read(0x23) & 0x28, 8, "PRSPI holds MOSI and returns SCK to CPOL");
        }
        cpu.write(SPDR, 0xff);
        to(&mut cpu, end + 16);
        assert_eq!(cpu.io(SPSR as u8) & 0x80, 0, "abort register {abort:#x}: neither abort nor gated write completes");
        cpu.write(0x64, 0); cpu.write(SPCR, 0x50);
    }
    cpu.write(SPDR, 0xa5); cpu.write(SPCR, 0x40);
    let end = cpu.cycles + 16; to(&mut cpu, end);
    assert_eq!(cpu.io(SPSR as u8) & 0x80, 0, "explicit MSTR clear aborts without a mode-fault flag");
    cpu.write(0x24, 4); cpu.write(SPCR, 0x50);
    cpu.events.clear(); cpu.write(SPDR, 0x96);
    let end = cpu.cycles + 16; to(&mut cpu, end);
    assert_eq!(cpu.drive('B').0 & 0x3c, 4, "master MOSI/SCK still require their DDR bits to drive");
    assert!(pins(&cpu).is_empty(), "input MOSI/SCK expose PORT/input levels throughout the internal transfer");
    assert_eq!(cpu.io(SPSR as u8) & 0x80, 0x80, "the internal master clock completes even with output DDR bits clear");
}

#[test]
fn interrupt_wake_and_cosim_drive_journal_keep_exact_spi_cycles() {
    for asleep in [false, true] {
        let mut a = Asm::new();
        a.emit(if asleep { Sleep } else { Nop });
        a.org(34); a.emit(Inc(17));
        let mut cpu = Cpu::new(&a.finish().unwrap());
        cpu.write(0x24, 0x2c); cpu.write(SPCR, 0xd0); cpu.write(SPSR, 1); cpu.write(SPDR, 0xa5);
        cpu.write(SPDR, 0); // WCOL must survive vector entry.
        cpu.sreg = 0x80;
        if asleep {
            cpu.step().unwrap();
            assert_eq!(cpu.wake_at(), Some(16), "SPI projects the completion tick for idle wake");
            assert_eq!(cpu.step_until(15), Err(Stop::Cycles), "idle wait can stop before completion");
        } else { to(&mut cpu, 16); }
        cpu.step().unwrap();
        assert_eq!((cpu.pc, cpu.cycles), (34, if asleep { 24 } else { 20 }), "SPI uses vector 34 plus entry/wake clocks");
        assert_eq!(cpu.io(SPSR as u8) & 0xc0, 0x40, "vector entry clears SPIF, preserves WCOL");
        cpu.step().unwrap();
        assert_eq!(cpu.r[17], 1, "SPI ISR executes");
    }

    let mut a = Asm::new();
    for (addr, value) in [(0x24, 0x3c), (0x25, 0x10), (SPCR, 0xd0), (SPSR, 1), (SPDR, 0xa5)] {
        store(&mut a, addr, value);
    }
    a.emit(Jmp(a.here())); // Three-clock instructions straddle multiple fosc/2 edges.
    let image = a.finish().unwrap();
    let mut m = AvrMachine::new(&image, 16_000_000); // Enables set_cosim_timing(true).
    let marks = PcSet::default();
    let until = |tick| Until { tick, marks: &marks, stops: &[], stops_after: 0, on_drive: false };
    assert_eq!(m.run(&until(12)), Ran::Tick, "configuration reaches its exact boundary");
    assert_eq!(m.drive(4), Drive::Input(Pull::Up), "machine drive sees MISO's forced input and pull-up");
    let mut drives = Vec::new(); m.take_drives(&mut drives); drives.clear();
    m.mark();
    assert_eq!(m.run(&until(36)), Ran::Tick, "machine runs beyond completion");
    m.take_drives(&mut drives);
    let expected: Vec<_> = waveform(15, 2, 0x50, 0xa5, false).into_iter()
        .map(|(tick, pin, high)| DriveEdge { tick, pin: u16::from(pin), drive: if high { Drive::High } else { Drive::Low } }).collect();
    assert_eq!(drives, expected, "co-simulation drive log preserves every MOSI/SCK change at its own cycle");
    let mut state = ChipState::default(); m.chip(&mut state);
    assert_eq!(state.pending, [Pending::Irq { source: "SPI", vector: 34 }], "machine snapshot names SPI");
    assert_eq!(m.drive(3), Drive::High, "machine holds the final MOSI bit");
    assert_eq!(m.drive(5), Drive::Low, "machine returns SCK to idle");
    m.rewind();
    assert_eq!(m.run(&until(36)), Ran::Tick);
    drives.clear(); m.take_drives(&mut drives);
    assert_eq!(drives, expected, "rewind replays the exact SPI drive journal");
}
