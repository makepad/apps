//! DS40002061B §24, Figure 24-3 and Table 24-1. Peripheral edges are exact CPU clocks;
//! instruction register writes take effect at completion, external writes at the current cycle.
use fab_elec_mcu_avr::{asm::Asm, chip, insn::Insn::*, machine::AvrMachine, sim::{Cpu, Stop}};
use fab_elec_mcu_emu::{ChipState, Drive, HaltKind, Machine, PcSet, Pending, PinUse, Pull, Ran, SampleAt, Status, Until};

const ADCL: u16 = 0x78;
const ADCH: u16 = 0x79;
const ADCSRA: u16 = 0x7a;
const ADMUX: u16 = 0x7c;
const HZ: u64 = 16_000_000;

fn store(a: &mut Asm, addr: u16, value: u8) {
    a.emit(Ldi(16, value));
    a.emit(Sts(addr, 16));
}

fn nop_cpu() -> Cpu {
    let mut a = Asm::new();
    a.emit(Nop);
    Cpu::new(&a.finish().unwrap())
}

// One-clock NOPs, as in avr_io.rs, let assertions straddle each hardware event.
fn to(cpu: &mut Cpu, tick: u64) {
    assert!(cpu.cycles <= tick, "test deadline is ahead");
    while cpu.cycles < tick {
        cpu.pc = 0;
        cpu.step().expect("NOP reaches next clock");
    }
}

fn code(cpu: &Cpu) -> u16 {
    u16::from(cpu.read(ADCL)) | (u16::from(cpu.read(ADCH)) << 8)
}

fn configured(adps: u8, mux: u8, free: bool) -> (Cpu, u64) {
    let mut a = Asm::new();
    // Leave word zero a NOP for subsequent individual-clock assertions.
    a.emit(Nop);
    store(&mut a, ADMUX, mux);
    store(&mut a, ADCSRA, 0xc0 | adps | if free { 0x20 } else { 0 });
    let mut cpu = Cpu::new(&a.finish().unwrap());
    cpu.pc = 1;
    for _ in 0..4 { cpu.step().unwrap(); }
    assert_eq!(cpu.cycles, 6, "two LDI/STS pairs complete at cycle 6");
    (cpu, 6)
}

#[test]
fn first_and_normal_conversions_sample_and_finish_on_the_prescaler_phase() {
    for (adps, div) in [(7, 128), (2, 4)] {
        let (mut cpu, e) = configured(adps, 0x40, false);
        let first_sample = e + 14 * div;
        let first_end = e + div / 2 + 25 * div;
        cpu.set_analog(0, 0.25);
        to(&mut cpu, first_sample - 1);
        cpu.set_analog(0, 1.25);
        to(&mut cpu, first_sample);
        cpu.set_analog(0, 4.0);
        to(&mut cpu, first_end - 1);
        assert_eq!(cpu.read(ADCSRA) & 0x50, 0x40, "div {div}: first conversion still busy one clock before end");
        to(&mut cpu, first_end);
        assert_eq!(cpu.read(ADCSRA) & 0x50, 0x10, "div {div}: completion clears ADSC and sets ADIF together");
        assert_eq!(code(&cpu), 256, "div {div}: only the voltage at the 13.5-clock sample is taken");

        // A write between edges, then one exactly on an edge: both select a strictly later edge.
        for gap in [17, 0] {
            let w = cpu.cycles + gap;
            to(&mut cpu, w);
            cpu.write(ADCSRA, 0xd0 | adps); // W1C ADIF, start again, retain ADEN.
            let edge = e + div / 2;
            let start = edge + ((w - edge) / div + 1) * div;
            let sample = start + 3 * div / 2;
            let end = start + 13 * div;
            cpu.set_analog(0, 0.0);
            to(&mut cpu, sample - 1);
            cpu.set_analog(0, 2.5);
            to(&mut cpu, sample);
            cpu.set_analog(0, 5.0);
            // Neither a zero ADSC write nor another one restarts the active conversion.
            cpu.write(ADCSRA, 0x80 | adps);
            cpu.write(ADCSRA, 0xc0 | adps);
            to(&mut cpu, end - 1);
            assert_eq!(cpu.read(ADCSRA) & 0x50, 0x40, "div {div}: normal conversion is still busy");
            to(&mut cpu, end);
            assert_eq!(cpu.read(ADCSRA) & 0x50, 0x10, "div {div}: normal completion follows the original phase");
            assert_eq!(code(&cpu), 512, "div {div}: normal sample is at 1.5 clocks, not at completion");
        }
        cpu.write(ADCSRA, 0xc0 | adps);
        cpu.write(ADCSRA, 0x10); // A whole-register disable may also reset ADPS.
        let end = cpu.cycles + 26 * div;
        to(&mut cpu, end);
        assert_eq!(cpu.read(ADCSRA), 0, "div {div}: ADEN=0 aborts even when the same write clears ADPS");
    }
}

#[test]
fn selection_latches_at_start_and_free_running_keeps_the_successor() {
    let (mut cpu, e) = configured(2, 0x40, true);
    cpu.set_analog(0, 0.5);
    cpu.set_analog(1, 1.25);
    cpu.set_analog(2, 3.75);
    cpu.write(ADMUX, 0x41); // Between ADSC and its start edge: still updates this conversion.
    let first_end = e + 2 + 25 * 4;
    to(&mut cpu, e + 2);
    cpu.write(ADMUX, 0xc0); // Locked for the first; next starts with internal 1.1 V reference.
    to(&mut cpu, first_end);
    assert_eq!(code(&cpu), 256, "MUX and REFS latch at start, not ADSC or sample-and-hold");
    assert_eq!(cpu.read(ADCSRA) & 0x50, 0x50, "free-running completion leaves ADSC set");
    cpu.write(ADMUX, 0x42); // The successor has already latched channel 0 / internal reference.
    for (n, expected) in [(1, (0.5_f64 / 1.1 * 1024.0).floor() as u16), (2, 768), (3, 768)] {
        cpu.write(ADCSRA, 0xb2); // Clear ADIF, write ADSC=0: conversion must keep going.
        to(&mut cpu, first_end + n * 52 - 1);
        assert_eq!(cpu.read(ADCSRA) & 0x50, 0x40, "free-running period {n}: no early completion");
        to(&mut cpu, first_end + n * 52);
        assert_eq!(cpu.read(ADCSRA) & 0x50, 0x50, "free-running period {n}: exactly 13 clocks");
        assert_eq!(code(&cpu), expected, "a post-completion MUX/REFS write appears two results later");
    }
}

#[test]
fn data_protection_alignment_flags_inputs_and_references() {
    let mut cpu = nop_cpu();
    cpu.set_analog(0, 513.5 * 5.0 / 1024.0);
    cpu.write(ADMUX, 0x40);
    cpu.write(ADCSRA, 0xc0);
    to(&mut cpu, 51);
    assert_eq!(code(&cpu), 513, "ten-bit result rounds down");
    cpu.write(ADMUX, 0x60);
    assert_eq!((cpu.read(ADCL), cpu.read(ADCH)), (0x40, 0x80), "ADLAR presents the existing code immediately");
    cpu.write(ADMUX, 0x40);
    assert_eq!(code(&cpu), 513, "clearing ADLAR immediately restores right alignment");
    cpu.write(ADCSRA, 0x90);
    assert_eq!(cpu.read(ADCSRA) & 0x10, 0, "ADIF clears by writing one");
    let low = cpu.read(ADCL);
    cpu.set_analog(0, 4.0);
    cpu.write(ADCSRA, 0xc0);
    to(&mut cpu, 79); // ADSC at edge 51 starts at 53, completes at 79.
    assert_eq!(cpu.read(ADCSRA) & 0x50, 0, "locked completion loses both result and ADIF but clears ADSC");
    assert_eq!((low, cpu.io(ADCL as u8), cpu.read(ADCH)), (1, 1, 2), "ADCL lock retains both old data bytes");
    cpu.write(ADCSRA, 0xc0);
    to(&mut cpu, 107);
    assert_eq!(code(&cpu), 819, "reading ADCH releases the lock for the next conversion");
    assert_eq!(cpu.read(ADCSRA) & 0x10, 0x10, "unlocked completion sets ADIF again");

    cpu.write(ADCSRA, 0xc0);
    to(&mut cpu, 110);
    cpu.write(ADCSRA, 0x10); // Disable in flight and clear the previous flag.
    to(&mut cpu, 140);
    assert_eq!(cpu.read(ADCSRA) & 0x50, 0, "clearing ADEN aborts with no completion");
    assert_eq!(code(&cpu), 819, "aborting preserves the old result");
    cpu.write(ADCSRA, 0xc0);
    to(&mut cpu, 190);
    assert_eq!(cpu.read(ADCSRA) & 0x40, 0x40, "re-enabling resets the phase and first-conversion delay");
    to(&mut cpu, 191);
    assert_eq!(cpu.read(ADCSRA) & 0x50, 0x10, "the fresh first conversion takes 25 clocks again");

    for (mux, volts, supply, expected) in [
        (0xc0, 0.55, 5.0, 512), (0x4e, 0.0, 5.0, 225), (0xce, 0.0, 5.0, 1023),
        (0x4f, 5.0, 5.0, 0), (0x40, -1.0, 5.0, 0), (0x40, 6.0, 5.0, 1023),
        (0x40, f64::NAN, 5.0, 0), (0x40, 1.25, f64::NAN, 256), (0x40, 1.25, 2.5, 512),
    ] {
        let mut cpu = nop_cpu();
        cpu.set_analog(0, volts);
        cpu.set_avcc(supply);
        cpu.write(ADMUX, mux);
        cpu.write(ADCSRA, 0xc1); // ADPS 1, like 0, divides by two.
        to(&mut cpu, 51);
        assert_eq!(code(&cpu), expected, "MUX/REFS {mux:#x}, input {volts}, supply {supply}");
    }
    cpu.write(0x27, 0x02); // DDRC: PC1 output, PC0 input.
    cpu.write(0x28, 0x03);
    cpu.set_pin('C', 0, true);
    assert_eq!(cpu.read(0x26) & 3, 3, "PINC observes the pad before DIDR0");
    cpu.write(0x7e, 0xff);
    assert_eq!(cpu.read(0x26) & 3, 0, "DIDR0 gates input and output digital reads");
    assert_eq!((cpu.read(0x27), cpu.read(0x28), cpu.read(0x7e)), (2, 3, 0x3f), "DIDR0 preserves DDR/PORT and masks reserved bits");
    assert_eq!(cpu.pin_use(1, 0), (PinUse::Peripheral, 0.0), "disabled digital buffer reports peripheral use");
}

#[test]
fn adc_interrupt_acknowledges_and_wakes_idle_sleep() {
    for asleep in [false, true] {
        let mut a = Asm::new();
        a.emit(if asleep { Sleep } else { Nop });
        a.org(42); a.emit(Inc(17));
        let mut cpu = Cpu::new(&a.finish().unwrap());
        cpu.sreg = 0x80;
        cpu.write(ADMUX, 0x4f);
        cpu.write(ADCSRA, 0xc8);
        if asleep {
            cpu.step().unwrap();
            assert_eq!(cpu.wake_at(), Some(51), "ADC completion projects the idle wake tick");
            assert_eq!(cpu.step_until(50), Err(Stop::Cycles), "sleep clips just before completion");
            cpu.step().unwrap();
        } else {
            to(&mut cpu, 51);
            assert_eq!(cpu.read(ADCSRA) & 0x10, 0x10, "completion requests the interrupt before entry");
            cpu.step().unwrap();
        }
        assert_eq!((cpu.pc, cpu.cycles), (42, if asleep { 59 } else { 55 }), "ADC vector with four entry clocks and four idle-wake clocks");
        assert_eq!(cpu.read(ADCSRA) & 0x10, 0, "ADC vector entry acknowledges ADIF");
        cpu.step().unwrap();
        assert_eq!(cpu.r[17], 1, "the ISR at word address 0x2a executes");
    }
    let mut cpu = nop_cpu();
    cpu.write(0xc0, 2); cpu.write(0xc1, 0x48); cpu.write(0xc6, 0x55); // TX complete at 80.
    to(&mut cpu, 29);
    cpu.write(ADMUX, 0x4f); cpu.write(ADCSRA, 0xc8); // First ADC completion also at 80.
    cpu.sreg = 0x80;
    to(&mut cpu, 80);
    cpu.step().unwrap();
    assert_eq!(cpu.pc, 40, "USART_TX wins a simultaneous completion before ADC");
    assert_eq!(cpu.read(ADCSRA) & 0x10, 0x10, "USART entry preserves the ADC flag");
    cpu.sreg = 0x80;
    cpu.step().unwrap();
    assert_eq!(cpu.pc, 42, "ADC is next in priority");
}

#[test]
fn unsupported_adc_modes_latch_named_stops() {
    let mut cases = vec![
        (vec![(ADMUX, 0), (ADCSRA, 0xc0)], false, "ADC external reference (AREF)"),
        (vec![(ADMUX, 0x80), (ADCSRA, 0xc0)], false, "ADC reserved reference (REFS=10)"),
        (vec![(ADMUX, 0x48), (ADCSRA, 0xc0)], false, "ADC temperature sensor (MUX=8)"),
        (vec![(0x64, 1), (ADCSRA, 0x80)], false, "ADC power reduction (PRADC with ADEN)"),
        (vec![(ADCSRA, 0x80), (0x64, 1)], false, "ADC power reduction (PRADC with ADEN)"),
        (vec![(ADCSRA, 0x80), (0x53, 3)], true, "ADC Noise Reduction sleep with ADEN"),
        (vec![(ADMUX, 0x40), (ADCSRA, 0xc0), (ADCSRA, 0xc2)], false, "ADC prescaler changed during conversion (ADPS)"),
    ];
    for mux in 9..=13 {
        cases.push((vec![(ADMUX, 0x40 | mux), (ADCSRA, 0xc0)], false, "ADC reserved channel (MUX=9..13)"));
    }
    for trigger in 1..=7 {
        for writes in [vec![(0x7b, trigger), (ADCSRA, 0xa0)], vec![(ADCSRA, 0xa0), (0x7b, trigger)],
            vec![(ADCSRA, 0x80), (0x7b, trigger), (ADCSRA, 0xa0)]] {
            cases.push((writes, false, "ADC auto trigger source (ADTS other than free running)"));
        }
    }
    for (writes, sleep, name) in cases {
        let mut a = Asm::new();
        for (addr, value) in writes { store(&mut a, addr, value); }
        a.emit(if sleep { Sleep } else { Nop });
        a.emit(Break);
        let mut cpu = Cpu::new(&a.finish().unwrap());
        let stop = cpu.run(100);
        assert!(matches!(stop, Stop::Unmodeled { what, .. } if what == name), "{name}: {stop:?}");
        assert_eq!(cpu.step(), Err(stop), "{name}: the first stop stays latched");
    }
}

#[test]
fn cpu_snapshot_restores_sample_completion_lock_and_nan_inputs() {
    let (mut cpu, e) = configured(2, 0x40, false);
    cpu.set_analog(0, f64::NAN);
    cpu.set_avcc(f64::NAN);
    let mark = cpu.clone();
    assert_eq!(cpu.same_state(&mark), None, "NaN held inputs compare by bits");
    let finish = |cpu: &mut Cpu| {
        to(cpu, e + 55);
        cpu.set_analog(0, 1.25);
        to(cpu, e + 56); // first sample at e + 14 * 4
        cpu.set_analog(0, 4.0);
        to(cpu, e + 102);
        assert_eq!(cpu.read(ADCL), 0, "sample is 256; leave its low-byte lock held");
    };
    finish(&mut cpu);
    let done = cpu.clone();
    cpu.copy_from(&mark);
    assert_eq!(cpu.same_state(&mark), None, "restoring the mark restores all ADC state");
    finish(&mut cpu);
    assert_eq!(cpu.same_state(&done), None, "sample, completion and locked registers replay identically");
}

fn machine_image() -> Vec<u16> {
    let mut a = Asm::new();
    store(&mut a, ADMUX, 0x46);
    store(&mut a, ADCSRA, 0xcf); // ADIE without I lets the snapshot inspect the pending source.
    a.emit(Jmp(a.here())); // Three-clock boundaries straddle sample-and-hold.
    a.finish().unwrap()
}

fn run(m: &mut impl Machine, tick: u64, on_drive: bool) -> Ran {
    m.run(&Until { tick, marks: &PcSet::default(), stops: &[], stops_after: 0, on_drive })
}

#[test]
fn machine_requests_exact_adc6_samples_and_rewinds_given_voltages() {
    let mut m = AvrMachine::new(&machine_image(), HZ);
    let at = SampleAt { tick: 1798, pin: 20 };
    let mut state = ChipState::default();
    for pin in chip::ANALOG_ONLY {
        assert!(m.analog_only(pin.row), "ADC6/ADC7 have no digital input");
        assert_eq!(m.drive(pin.row), Drive::Input(Pull::None), "analog-only pins never drive");
        let generation = m.drive_gen();
        let before = m.cpu().clone();
        m.set_input(pin.row, true);
        assert_eq!(m.drive_gen(), generation, "set_input on an analog-only row does nothing");
        assert_eq!(m.cpu().same_state(&before), None, "analog-only digital input changes no CPU state");
    }
    m.chip(&mut state);
    assert_eq!(&state.pins[20..], &[(PinUse::Peripheral, 0.0); 2], "analog-only rows report peripheral use");
    assert_eq!(run(&mut m, 100, true), Ran::Tick, "no ADC request before sample-and-hold");
    m.mark();
    let mark = m.cpu().clone();
    assert_eq!(run(&mut m, 4000, true), Ran::Drive { at: at.tick }, "sample request stops speculative execution");
    assert_eq!(m.now(), 1800, "request returns at the first instruction boundary after its exact tick");
    let mut requests = Vec::new();
    m.take_samples(&mut requests);
    assert_eq!(requests, [at], "ADC6 uses machine row 20 and its exact sample-and-hold tick");
    m.set_sample(at, 1.25, 2.5);
    assert_eq!(run(&mut m, 3270, true), Ran::Tick, "given sample reaches completion");
    assert_eq!(code(m.cpu()), 512, "the request's supply supplies AVCC");
    m.chip(&mut state);
    assert_eq!(state.pending, [Pending::Irq { source: "ADC", vector: 42 }], "ADC source is named in the snapshot");
    let done = m.cpu().clone();
    m.rewind();
    assert_eq!(m.cpu().same_state(&mark), None, "rewind drops requests and the given sample");
    // The caller may replay an answer before its request is made again.
    m.set_sample(at, 1.25, 2.5);
    assert_eq!(run(&mut m, 4000, true), Ran::Drive { at: at.tick });
    requests.clear();
    m.take_samples(&mut requests);
    assert_eq!(requests, [at], "replay still emits the request for adapter deduplication");
    assert_eq!(run(&mut m, 3270, true), Ran::Tick);
    assert_eq!(code(m.cpu()), 512);
    assert_eq!(m.cpu().same_state(&done), None, "replaying an early answer restores the full CPU state");

    m.rewind();
    assert_eq!(run(&mut m, 4000, true), Ran::Drive { at: at.tick });
    m.take_samples(&mut Vec::new());
    m.set_sample(SampleAt { pin: 21, ..at }, 1.25, 2.5);
    m.set_sample(SampleAt { tick: at.tick - 1, ..at }, 1.25, 2.5);
    assert_eq!(run(&mut m, 3270, true), Ran::Halted, "a sample from the abandoned run cannot satisfy this conversion");
    assert!(matches!(m.status(), Status::Halted(h) if h.kind == HaltKind::Unmodeled
        && h.what == "ADC sample not given by the co-simulation"), "only the exact tick and pin satisfy the named missing-sample halt");

    m.rewind();
    m.set_sample(at, f64::NAN, f64::NAN);
    assert_eq!(run(&mut m, 4000, true), Ran::Drive { at: at.tick });
    m.mark(); // A mark with an untaken request must restore that request too.
    requests.clear(); m.take_samples(&mut requests);
    m.rewind();
    let mut again = Vec::new(); m.take_samples(&mut again);
    assert_eq!(again, requests, "mark restores untaken sample requests");
    assert_eq!(run(&mut m, 3270, true), Ran::Tick);
    assert_eq!(code(m.cpu()), 0, "a pad outside the circuit is modeled as ground");
}

#[test]
fn sleeping_machine_cuts_at_the_sample_and_internal_channels_need_no_sample() {
    for (mux, expected, request) in [(0x46, 256, true), (0x4e, 225, false), (0x4f, 0, false)] {
        let mut a = Asm::new();
        store(&mut a, ADMUX, mux); store(&mut a, ADCSRA, 0xc7);
        a.emit(Sleep);
        let mut m = AvrMachine::new(&a.finish().unwrap(), HZ);
        if request {
            assert_eq!(run(&mut m, 4000, true), Ran::Drive { at: 1798 }, "sleep stops at sample-and-hold even without an IRQ");
            assert_eq!(m.now(), 1798, "a sleep wait has no instruction-boundary overshoot");
            let mut samples = Vec::new(); m.take_samples(&mut samples);
            assert_eq!(samples, [SampleAt { tick: 1798, pin: 20 }]);
            m.set_sample(samples[0], 1.25, f64::NAN);
        }
        assert_eq!(run(&mut m, 3270, true), Ran::Tick, "conversion completes while idle without waking it");
        assert_eq!(code(m.cpu()), expected, "external samples use nominal AVCC for NaN supply; VBG/GND need no request");
        assert!(matches!(m.status(), Status::Sleeping { wake: None }), "ADIE=0 leaves no wake source");
        let mut samples = Vec::new(); m.take_samples(&mut samples);
        assert!(samples.is_empty(), "no extra external request for an internal channel");
    }
}
