use fab_elec_mcu_avr::{
    asm::Asm,
    chip::by_name,
    insn::Insn::*,
    machine::AvrMachine,
    sim::{Cpu, Event, Stop},
};
use fab_elec_mcu_emu::{ChipState, Drive, Halt, HaltKind, Machine, PcSet, Pending, PinUse, Pull, Ran, Status, TimerEvent, Until};

const HZ: u64 = 16_000_000;
const OC0A: usize = 18; // PD6 in the package GPIO list.

fn store(a: &mut Asm, addr: u16, value: u8) {
    a.emit(Ldi(16, value));
    a.emit(Sts(addr, 16));
}

// Reset jumps over the vectors; Timer0 overflow counts interrupts and USART0
// reception echoes three bytes. Configuration is firmware, never a mutable CPU escape.
fn pwm_program(sleep: bool, irq: bool) -> (Vec<u16>, u32) {
    let mut a = Asm::new();
    a.emit(Jmp(64));
    a.org(32); a.emit(Jmp(192));
    a.org(36); a.emit(Jmp(208));
    a.org(64);
    for (addr, value) in [(0x2a, 0x40), (0x47, 10), (0x44, 0x83),
        (0xc0, 2), (0xc1, if irq { 0x98 } else { 0x18 }), (0x6e, u8::from(irq)), (0x45, 1)] {
        store(&mut a, addr, value);
    }
    if irq { a.emit(Bset(7)); }
    let ready = a.here();
    if sleep { a.emit(Sleep); a.emit(Rjmp(-2)); }
    else { a.emit(Jmp(ready)); } // Three clocks; first PWM edge falls inside it.
    a.org(192); a.emit(Inc(17)); a.emit(Sts(0x100, 17)); a.emit(Reti);
    a.org(208); a.emit(Lds(18, 0xc6)); a.emit(Sts(0xc6, 18));
    a.emit(Sts(0x101, 18)); a.emit(In(19, 6)); a.emit(Sts(0x102, 19)); a.emit(Reti);
    (a.finish().expect("PWM fixture assembles"), ready)
}

fn ready(machine: &mut AvrMachine, pc: u32) {
    let mut marks = PcSet::new(0, 256);
    marks.insert(pc);
    assert_eq!(machine.run(&Until { tick: 100, marks: &marks, stops: &[pc],
        stops_after: 0, on_drive: false }), Ran::Pc, "firmware reaches its configured entry point");
}

fn run_to(machine: &mut AvrMachine, tick: u64, marks: &PcSet) -> Ran {
    machine.run(&Until { tick, marks, stops: &[], stops_after: 0, on_drive: false })
}

#[test]
fn chip_reports_pin_use_and_pwm_duty() {
    let mut a = Asm::new();
    // DS40002061B §14.4, pp. 100-101: PB5/PB1/PB3 and PD6/PD5/PD0 outputs;
    // PD2 input with PORT set. PD0/PD1 deliberately oppose their UART directions.
    for (addr, value) in [(0x24, 0x2a), (0x2a, 0x61), (0x2b, 0x04),
        // Preload active OCRs before selecting PWM (double buffering: §§15.5,
        // 16.7, 18.5, pp. 105, 129, 153). Timer0 WGM3, COMA=2, COMB=3;
        // Timer1 WGM1, COMA=2; Timer2 WGM1, COMA=2 (Tables 15-3/6/8,
        // pp. 113-115; 16-3/4, p. 141; 18-4/8, pp. 163-164).
        (0x47, 63), (0x48, 63), (0x44, 0xb3),
        (0x89, 0), (0x88, 51), (0x80, 0x81),
        (0xb3, 0), (0xb0, 0x81),
        // clk/8, clk/1, clk/1 (Tables 15-9, 16-5, 18-9, pp. 117, 143, 165).
        (0x45, 2), (0x81, 1), (0xb1, 1)] {
        store(&mut a, addr, value);
    }
    let entry = a.here();
    store(&mut a, 0x47, 127); // Buffer changes before Timer0 reaches BOTTOM.
    store(&mut a, 0x89, 0);
    store(&mut a, 0x88, 102); // Buffer changes before Timer1 reaches TOP.
    let buffered = a.here();
    store(&mut a, 0x55, 0x10); // PUD: §14.4.1, p. 100.
    store(&mut a, 0xc1, 0x18); // RXEN0 | TXEN0: §20.11.3, p. 202.
    store(&mut a, 0x45, 0); // Timer0 stopped, OC channels still connected.
    let changed = a.here();
    a.emit(Jmp(changed));
    let mut machine = AvrMachine::new(&a.finish().expect("pin-use fixture assembles"), HZ);
    ready(&mut machine, entry);
    let row = |name| by_name(name).expect("fixture pin has a machine row").0;
    let mut chip = ChipState::default();
    machine.chip(&mut chip);
    assert_eq!(chip.pins.len(), 22, "chip reports one pin use per machine row");
    assert_eq!(chip.pins[row("PB5")], (PinUse::Output, 0.0), "PB5 with DDR set is an output with zero duty");
    assert_eq!(chip.pins[row("PD2")], (PinUse::PullUp, 0.0), "PD2 input with PORT set enables its pull-up");
    assert_eq!(chip.pins[row("PD6")], (PinUse::Pwm, 64.0 / 256.0), "OC0A fast non-inverting PWM includes the BOTTOM tick");
    assert_eq!(chip.pins[row("PD5")], (PinUse::Pwm, 1.0 - 64.0 / 256.0), "OC0B fast inverting PWM complements the duty");
    assert_eq!(chip.pins[row("PB1")], (PinUse::Pwm, 51.0 / 255.0), "OC1A 8-bit phase-correct PWM uses OCR over TOP");
    assert_eq!(chip.pins[row("PB3")], (PinUse::Pwm, 0.0), "OC2A phase-correct PWM at OCR zero is continuously low");

    ready(&mut machine, buffered);
    machine.chip(&mut chip);
    assert_eq!(chip.pins[row("PD6")], (PinUse::Pwm, 64.0 / 256.0), "fast PWM duty uses the active latch until BOTTOM");
    assert_eq!(chip.pins[row("PB1")], (PinUse::Pwm, 51.0 / 255.0), "phase-correct PWM duty uses the active latch until TOP");

    ready(&mut machine, changed);
    chip.pins.push((PinUse::Peripheral, 0.0));
    machine.chip(&mut chip);
    assert_eq!(chip.pins.len(), 22, "chip clears stale pin entries on each snapshot");
    assert_eq!(chip.pins[row("PD2")], (PinUse::Input, 0.0), "PUD disables the PD2 pull-up");
    assert_eq!(chip.pins[row("PD1")], (PinUse::Peripheral, 0.0), "TXEN0 gives USART0 ownership of PD1 despite DDR clear");
    assert_eq!(chip.pins[row("PD0")], (PinUse::Peripheral, 0.0), "RXEN0 gives USART0 ownership of PD0 despite DDR set");
    assert_eq!(chip.pins[row("PD6")], (PinUse::Output, 0.0), "a connected OC0A with Timer0 stopped is an output");
}

#[test]
fn marked_snapshot_replays_cpu_pins_links_and_trace() {
    let (image, entry) = pwm_program(true, true);
    let mut machine = AvrMachine::new(&image, HZ);
    ready(&mut machine, entry);
    let mut marks = PcSet::new(0, 256);
    for pc in [entry, 192, 208] { marks.insert(pc); }
    machine.set_input(6, true); // PC0; saved input state is observed by the RX ISR.
    machine.link_in(0, b'A');
    let start = machine.now();
    assert_eq!(run_to(&mut machine, start + 40, &marks), Ran::Tick, "mark is inside the first received frame");
    let mut discard_out = Vec::new();
    let mut discard_trace = Vec::new();
    machine.take_out(&mut discard_out);
    machine.take_trace(&mut discard_trace);
    machine.mark();
    let mark_tick = machine.now();
    let generation = machine.drive_gen();
    let mut at_mark = ChipState::default();
    machine.chip(&mut at_mark);
    assert!(at_mark.pending.contains(&Pending::Rx { link: 0, bytes: 1 }), "in-flight receive byte is inspectable");
    let timer = &at_mark.timers[0];
    assert_eq!((timer.name, timer.mode, timer.prescale, timer.top, timer.count),
        ("Timer0", "fast pwm", 1, 255, 41), "timer snapshot reports the active configuration and counter");
    assert_eq!(timer.compares, [10, 0], "timer snapshot exposes the active compare latches");
    assert_eq!(timer.next, Some((TimerEvent::Overflow, start - 1 + 256)), "timer projection finds the next hardware event");
    let copy = machine.clone();
    assert_eq!(machine.cpu().flash.as_ptr(), copy.cpu().flash.as_ptr(), "machine clones share flash storage");

    let finish = |machine: &mut AvrMachine| {
        machine.set_input(7, true);
        machine.link_in(0, b'B');
        machine.link_in(0, b'C');
        assert_eq!(run_to(machine, start + 900, &marks), Ran::Tick, "replay reaches the same sleep deadline");
        let mut regs = vec![("stale", 999)];
        machine.regs(&mut regs);
        assert_eq!(regs.len(), 35, "register snapshot clears old entries and includes all registers");
        let mut ram = vec![0; 2048];
        assert!(machine.read_data(0x100, &mut ram), "entire SRAM is readable without bus effects");
        let drives: Vec<_> = (0..machine.pins().len()).map(|pin| machine.drive(pin)).collect();
        let mut chip = ChipState::default();
        machine.chip(&mut chip);
        let mut output = Vec::new();
        let mut trace = Vec::new();
        machine.take_out(&mut output);
        machine.take_trace(&mut trace);
        (regs, ram, machine.now(), drives, output, trace, chip, machine.drive_gen())
    };
    let first = finish(&mut machine);
    assert!(first.1[0] >= 3, "Timer0 interrupt executed repeatedly");
    assert_eq!(first.1[1], b'C', "USART ISR stored the final echoed byte");
    assert_eq!(first.1[2] & 3, 3, "saved and replayed external input levels reached firmware");
    assert_eq!(first.4.iter().map(|tx| tx.byte).collect::<Vec<_>>(), [b'A', b'B', b'C'], "three USART frames were transmitted");
    assert!(first.4.iter().all(|tx| tx.link == 0), "all transmit events use link zero");
    assert!(first.5.iter().any(|&(_, pc)| pc == 192), "timer ISR is traced");
    assert!(first.5.iter().any(|&(_, pc)| pc == 208), "receive ISR is traced");
    machine.rewind();
    assert_eq!((machine.now(), machine.drive_gen()), (mark_tick, generation), "rewind restores time and drive generation");
    assert_eq!(machine.cpu().drive('C').2 & 3, 1, "rewind restores external input levels");
    let mut empty_out = Vec::new();
    let mut empty_trace = Vec::new();
    machine.take_out(&mut empty_out);
    machine.take_trace(&mut empty_trace);
    assert!(empty_out.is_empty() && empty_trace.is_empty(), "rewind after take truncates both journals to the empty mark");
    assert_eq!(finish(&mut machine), first, "replay preserves registers, SRAM, time, pins, output, trace, timers and generation");
    machine.rewind();
    let mut restored = ChipState::default();
    machine.chip(&mut restored);
    assert_eq!(restored, at_mark, "rewind restores timer, wake and receive queue state");
    machine.mark(); // Reuse the already allocated snapshot.
    assert_eq!(run_to(&mut machine, mark_tick + 1, &marks), Ran::Tick, "a reused mark remains runnable");
    machine.rewind();
    assert_eq!(machine.now(), mark_tick, "a reused mark restores the saved cycle");
}

#[test]
fn drive_edge_inside_instruction_retains_its_hardware_cycle() {
    let (image, entry) = pwm_program(false, false);
    let mut machine = AvrMachine::new(&image, HZ);
    ready(&mut machine, entry);
    let start = machine.now();
    let mut raw = Cpu::new(&image);
    assert_eq!(raw.run(start + 300), Stop::Cycles, "raw model runs with default timing");
    let expected = raw.events.iter().find_map(|event| match *event {
        Event::Pin { cycle, port: 'D', bit: 6, .. } => Some(cycle), _ => None,
    }).expect("raw model records the first PWM edge");
    let marks = PcSet::default();
    assert_eq!(machine.run(&Until { tick: start + 300, marks: &marks, stops: &[],
        stops_after: 0, on_drive: true }), Ran::Drive { at: expected }, "drive stop uses the raw pin-event cycle");
    assert!(expected < machine.now(), "multi-cycle instruction completes after its internal edge");
    assert_eq!(machine.now(), start + 258, "three-clock instruction remains indivisible");
    assert_eq!(machine.drive(OC0A), Drive::High, "OC override reaches the package pin");
}

#[test]
fn sleeping_drive_edge_cuts_then_resumes_the_same_wake() {
    let (image, entry) = pwm_program(true, true);
    let mut machine = AvrMachine::new(&image, HZ);
    ready(&mut machine, entry);
    let start = machine.now();
    let mut raw = Cpu::new(&image);
    while raw.pc != entry { raw.step().expect("raw setup executes"); }
    raw.step().expect("raw SLEEP has a timer wake source");
    raw.step().expect("raw sleep completes its wake and interrupt entry");
    let edge = raw.events.iter().find_map(|event| match *event {
        Event::Pin { cycle, port: 'D', bit: 6, .. } => Some(cycle), _ => None,
    }).expect("raw sleep records a PWM edge");
    let mut marks = PcSet::new(0, 256);
    marks.insert(entry + 1); // The saved return PC must not count as execution during sleep.
    let until = Until { tick: raw.cycles, marks: &marks, stops: &[entry + 1], stops_after: 0, on_drive: true };
    assert_eq!(machine.run(&until), Ran::Drive { at: edge }, "sleep reports the raw PWM edge before waking");
    assert_eq!(machine.now(), edge, "sleep ends exactly on its drive edge");
    assert_eq!(machine.status(), Status::Sleeping { wake: Some(edge) }, "edge at the wake request leaves the CPU asleep");
    let mut chip = ChipState::default();
    machine.chip(&mut chip);
    assert_eq!(chip.wake.map(|wake| (wake.tick, wake.source, wake.vector)),
        Some((edge, "TIMER0_OVF", 32)), "wake reports the enabled timer source and word vector");
    assert!(chip.pending.contains(&Pending::Irq { source: "TIMER0_OVF", vector: 32 }), "overflow remains pending at the clipped edge");
    assert_eq!(machine.run(&until), Ran::Tick, "next run resumes the same sleep");
    assert_eq!((machine.now(), machine.pc()), (raw.cycles, raw.pc), "resumed sleep matches raw wake timing and vector");
    assert_eq!(raw.cycles, start - 1 + 256 + 8, "wake adds four wake clocks and four entry clocks");
    let mut trace = Vec::new();
    machine.take_trace(&mut trace);
    assert!(trace.is_empty(), "a sleeping return PC is neither stopped at nor traced");
}

#[test]
fn marked_pc_stops_before_execution_and_resumes_once() {
    let mut a = Asm::new();
    for insn in [Ldi(16, 3), Inc(16), Inc(17), Rjmp(-3)] { a.emit(insn); }
    let mut machine = AvrMachine::new(&a.finish().expect("stop fixture assembles"), HZ);
    let mut marks = PcSet::new(0, 4);
    marks.insert(1); marks.insert(2);
    let mut until = Until { tick: 1, marks: &marks, stops: &[1], stops_after: 0, on_drive: false };
    assert_eq!(machine.run(&until), Ran::Pc, "PC stop takes precedence over a simultaneous tick deadline");
    assert_eq!((machine.pc(), machine.cpu().r[16]), (1, 3), "stopped instruction has not changed its register");
    let mut trace = Vec::new();
    machine.take_trace(&mut trace);
    assert!(trace.is_empty(), "stopping before a marked instruction adds no trace entry");
    let stopped = machine.now();
    until.stops_after = stopped;
    until.tick = 3;
    assert_eq!(machine.run(&until), Ran::Tick, "stops_after permits the stopped instruction to execute");
    machine.take_trace(&mut trace);
    assert_eq!(trace, [(stopped, 1), (stopped + 1, 2)], "executed stop and trace-only mark are recorded once");
    assert_eq!((machine.cpu().r[16], machine.cpu().r[17]), (4, 1), "both marked instructions execute on resumption");
}

#[test]
fn sleep_without_a_wake_source_reaches_deadline_and_keeps_pwm() {
    let (image, entry) = pwm_program(true, false);
    let mut machine = AvrMachine::new(&image, HZ);
    ready(&mut machine, entry);
    let start = machine.now();
    let marks = PcSet::default();
    let until = Until { tick: start + 600, marks: &marks, stops: &[], stops_after: 0, on_drive: true };
    for (offset, drive) in [(256, Drive::High), (267, Drive::Low), (512, Drive::High), (523, Drive::Low)] {
        assert_eq!(machine.run(&until), Ran::Drive { at: start + offset }, "unwakeable sleep still reports PWM edges");
        assert_eq!(machine.now(), start + offset, "unwakeable sleep clips exactly at each edge");
        assert_eq!(machine.drive(OC0A), drive, "sleeping PWM drives the alternating level");
        assert_eq!(machine.status(), Status::Sleeping { wake: None }, "disabled interrupts leave no wake source");
    }
    assert_eq!(machine.run(&until), Ran::Tick, "sleep without a wake source returns the deadline");
    assert_eq!(machine.now(), until.tick, "sleep deadline is exact");
    assert_eq!(run_to(&mut machine, start + 900, &marks), Ran::Tick, "on_drive false consumes sleeping edges internally");
    assert_eq!(machine.now(), start + 900, "unobserved PWM also preserves the exact deadline");
    assert_eq!(machine.status(), Status::Sleeping { wake: None }, "deadline returns retain sleep state");
}

#[test]
fn inspection_preserves_timer_temp_and_receive_fifo() {
    let mut a = Asm::new();
    for (addr, value) in [(0x81, 0x18), (0x87, 0x12), (0x86, 0x34),
        (0x85, 0x56), (0x84, 0x78), (0x89, 0), (0x88, 9), (0x87, 0xab),
        (0xc0, 2), (0xc1, 0x10)] { store(&mut a, addr, value); }
    let entry = a.here();
    a.emit(Jmp(entry));
    let mut machine = AvrMachine::new(&a.finish().expect("inspection fixture assembles"), HZ);
    ready(&mut machine, entry);
    machine.link_in(0, 0x31); machine.link_in(0, 0x32);
    let marks = PcSet::default();
    let end = machine.now() + 162;
    assert_eq!(run_to(&mut machine, end, &marks), Ran::Tick, "both receive frames finish before inspection");
    let mut chip = ChipState::default();
    machine.chip(&mut chip);
    let before = chip.clone();
    chip.pending.push(Pending::Rx { link: 9, bytes: 99 });
    chip.timers[1].compares.push(999);
    machine.chip(&mut chip);
    assert_eq!(chip, before, "chip overwrites stale snapshot fields and retains no duplicates");
    assert_eq!((chip.timers[1].count, chip.timers[1].top, chip.timers[1].prescale),
        (0x5678, 0x1234, 0), "TCNT1 and ICR1 are read from their actual state while stopped");
    assert_eq!(chip.timers[1].compares, [9, 0], "stopped CTC exposes active compare values");
    assert_eq!(chip.timers[1].next, None, "stopped timer has no next event");
    assert_eq!(chip.pending, [Pending::Rx { link: 0, bytes: 2 }], "receive FIFO count excludes disabled interrupts");
    assert_eq!(machine.cpu().read(0x87), 0xab, "chip inspection leaves Timer1 TEMP untouched");
    let mut bytes = [0xa5; 2];
    for addr in [0xc6, 0xff, 0x8ff, 0x900, u32::MAX] {
        assert!(!machine.read_data(addr, &mut bytes), "SRAM inspection rejects invalid range at {addr:#x}");
        assert_eq!(bytes, [0xa5; 2], "failed RAM reads leave the output untouched");
    }
    assert_eq!([machine.cpu().read(0xc6), machine.cpu().read(0xc6)], [0x31, 0x32],
        "chip and RAM inspection do not consume receive bytes");
}

#[test]
fn pull_disable_changes_drive_and_break_latches_a_resumable_halt() {
    let mut a = Asm::new();
    store(&mut a, 0x25, 1); // PB0 input pull-up.
    store(&mut a, 0x55, 0x10); // Global pull-up disable.
    a.emit(Break);
    let mut machine = AvrMachine::new(&a.finish().expect("pull/halt fixture assembles"), HZ);
    let marks = PcSet::default();
    let until = Until { tick: 7, marks: &marks, stops: &[], stops_after: 0, on_drive: true };
    assert_eq!(machine.tick_hz(), HZ, "board crystal sets the machine tick rate");
    assert_eq!(machine.pins().len(), 22, "twenty GPIO and two analog-only package pins are exposed");
    assert_eq!(machine.run(&until), Ran::Drive { at: 3 }, "PORT write reports its completion cycle");
    assert_eq!(machine.drive(0), Drive::Input(Pull::Up), "PORT bit enables an input pull-up");
    let generation = machine.drive_gen();
    machine.set_input(0, false);
    assert_eq!(machine.drive_gen(), generation, "input sampling alone leaves drive generation unchanged");
    assert_eq!(machine.run(&until), Ran::Drive { at: 6 }, "PUD write reports its completion cycle");
    assert_eq!(machine.drive(0), Drive::Input(Pull::None), "PUD removes the input pull-up");
    machine.mark();
    assert_eq!(machine.run(&until), Ran::Halted, "BREAK takes precedence over the deadline");
    assert_eq!(machine.status(), Status::Halted(Halt { tick: 7, pc: 6, kind: HaltKind::Break, what: "BREAK" }),
        "halt reports the offending BREAK word address and completion cycle");
    assert_eq!(machine.run(&until), Ran::Halted, "a halted run is resumable without executing further instructions");
    machine.rewind();
    assert_eq!(machine.status(), Status::Running, "rewind removes a halt absent at the mark");
    assert_eq!(machine.run(&until), Ran::Halted, "replaying BREAK reproduces the halt");
}
