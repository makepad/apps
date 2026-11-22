use fab_elec_mcu_avr::{
    asm::{Asm, Target},
    insn::Insn::{self, *},
    sim::{Cpu, Event, Stop},
};

fn program(insns: &[Insn]) -> Vec<u16> {
    let mut a = Asm::new();
    for &insn in insns { a.emit(insn); }
    a.finish().unwrap()
}

// One-clock instructions let assertions straddle individual peripheral edges.
fn clocks(cpu: &mut Cpu, count: u64) {
    for _ in 0..count { cpu.pc = 0; cpu.step().unwrap(); }
}

fn write16(cpu: &mut Cpu, addr: u16, value: u16) {
    cpu.write(addr + 1, (value >> 8) as u8);
    cpu.write(addr, value as u8);
}

fn edges(cpu: &Cpu, port: char, bit: u8) -> Vec<(u64, bool)> {
    cpu.events.iter().filter_map(|e| match *e {
        Event::Pin { cycle, port: p, bit: b, high } if p == port && b == bit => Some((cycle, high)),
        _ => None,
    }).collect()
}

fn receiver(cpu: &mut Cpu) {
    cpu.write(0xc0, 2);
    cpu.write(0xc1, 0x10);
    // Reset UBRR=0 and UCSR0C=8N1: ten bits * eight clocks = 80.
}

#[test]
fn rx_19200_baud_echo_interrupt_arrives_at_each_frame_end() {
    let mut a = Asm::new();
    let isr = a.label();
    a.emit(Sleep); a.emit(Rjmp(-2));
    a.org(36); a.jump(Target::Rjmp, isr);
    a.org(64); a.bind(isr);
    a.emit(Lds(16, 0xc6)); a.emit(Sts(0xc6, 16)); a.emit(Reti);
    let image = a.finish().unwrap();
    // 16 MHz / (8*104) and / (16*52) are both 19230.77 baud, the
    // integer UBRR settings for requested 19200. 8N1 frames take 8320 clocks.
    for (u2x, ubrr) in [(2, 103), (0, 51)] {
        let mut cpu = Cpu::new(&image);
        cpu.write(0xc0, u2x); cpu.write(0xc4, ubrr); cpu.write(0xc1, 0x98);
        cpu.sreg = 128;
        for byte in [b'A', b'B', b'C'] { cpu.rx(byte); }
        for n in 1..=3 {
            let end = n * 8320;
            let remaining = end - 1 - cpu.cycles;
            assert_eq!(cpu.run(remaining), Stop::Cycles);
            assert_eq!(cpu.cycles, end - 1);
            assert_eq!(cpu.read(0xc0) & 0x80, 0, "not complete before stop-bit end");
            assert_eq!(cpu.run(1), Stop::Cycles);
            assert_eq!(cpu.cycles, end);
            assert_eq!(cpu.read(0xc0) & 0x80, 0x80, "RXC at frame end");
            cpu.step().unwrap();
            assert_eq!((cpu.pc, cpu.cycles), (36, end + 8), "idle wake + RX vector entry");
            assert_ne!(cpu.read(0xc0) & 0x80, 0, "entry does not acknowledge RXC");
            cpu.step().unwrap(); cpu.step().unwrap();
            assert_eq!(cpu.cycles, end + 12, "vector RJMP + UDR LDS");
            assert_eq!(cpu.read(0xc0) & 0x80, 0);
            cpu.step().unwrap();
            assert_eq!(cpu.cycles, end + 14, "echo STS");
        }
        let tx: Vec<_> = cpu.events.iter().filter_map(|e| match *e {
            Event::Tx { cycle, byte } => Some((cycle, byte)), _ => None,
        }).collect();
        assert_eq!(tx, [(8334, b'A'), (16654, b'B'), (24974, b'C')]);
        assert_eq!(cpu.rx_lost, 0);
    }
}

#[test]
fn rx_two_fifo_entries_and_waiting_shifter_lose_only_at_next_start() {
    let mut cpu = Cpu::new(&program(&[Nop])); receiver(&mut cpu);
    for byte in 1..=3 { cpu.rx(byte); }
    clocks(&mut cpu, 240);
    assert_eq!((cpu.rx_lost, cpu.read(0xc0) & 0x88), (0, 0x80), "third completed frame waits");
    // A read frees a FIFO slot and transfers the waiting shifter immediately.
    assert_eq!([cpu.read(0xc6), cpu.read(0xc6), cpu.read(0xc6)], [1, 2, 3]);
    assert_eq!(cpu.read(0xc0) & 0x88, 0);

    // Transfer the tagged frame both from waiting on a pop and directly
    // on completion with space. Five frames coalesce two losses into DOR0.
    for (last, read_early) in [(4, false), (5, false), (4, true)] {
        let mut cpu = Cpu::new(&program(&[Nop])); receiver(&mut cpu);
        for byte in 1..=last { cpu.rx(byte); }
        clocks(&mut cpu, 239);
        assert_eq!(cpu.rx_lost, 0, "third frame still receiving");
        clocks(&mut cpu, 1);
        assert_eq!((cpu.rx_lost, cpu.read(0xc0) & 0x88), (1, 0x80),
            "fourth start loses the third frame, but the first frame has no DOR0");
        if !read_early { clocks(&mut cpu, u64::from(last - 3) * 80); }
        let mut kept = Vec::new();
        for byte in [1, 2] {
            for _ in 0..2 {
                assert_eq!(cpu.read(0xc0) & 0x88, 0x80,
                    "repeated status reads retain the untagged first two frames");
            }
            kept.push(cpu.read(0xc6));
            assert_eq!(*kept.last().unwrap(), byte, "oldest buffered frames survive in order");
        }
        if read_early {
            assert_eq!(cpu.read(0xc0) & 0x88, 0, "pending DOR0 is invisible while the FIFO is empty");
            assert_eq!(cpu.read(0xc6), 0, "an empty UDR read must not consume pending DOR0");
            clocks(&mut cpu, 80);
        }
        for _ in 0..2 {
            assert_eq!(cpu.read(0xc0) & 0x88, 0x88, "only the final kept frame carries DOR0; status reads retain it");
        }
        cpu.write(0xc0, 0x8a);
        assert_eq!(cpu.read(0xc0) & 0x88, 0x88, "status writes cannot clear RXC/DOR");
        kept.push(cpu.read(0xc6));
        assert_eq!(kept, [1, 2, last], "four frames keep 1,2,4; five keep 1,2,5");
        assert_eq!(cpu.rx_lost, u64::from(last - 3), "the oracle counts every lost frame, even when DOR0 coalesces");
        assert_eq!(cpu.read(0xc0) & 0x88, 0, "reading the tagged frame removes its flags");
        cpu.rx(last + 1); clocks(&mut cpu, 80);
        assert_eq!(cpu.read(0xc0) & 0x88, 0x80, "successful transfer clears pending DOR0 for the next frame");
        assert_eq!(cpu.read(0xc6), last + 1);
    }
}

#[test]
fn rx_interrupt_is_a_level_and_disabling_rx_flushes_every_held_byte() {
    let mut cpu = Cpu::new(&program(&[Nop])); receiver(&mut cpu);
    cpu.rx(1); cpu.rx(2); clocks(&mut cpu, 160);
    cpu.write(0xc1, 0x90);
    for remaining in [2, 1] {
        cpu.sreg = 128; let before = cpu.cycles; cpu.step().unwrap();
        assert_eq!((cpu.pc, cpu.cycles), (36, before + 4));
        cpu.sreg = 128; cpu.step().unwrap();
        assert_eq!(cpu.pc, 36, "same FIFO byte still requests a second entry");
        assert_eq!(cpu.read(0xc6), 3 - remaining);
    }
    cpu.sreg = 128; cpu.pc = 0; cpu.step().unwrap();
    assert_eq!(cpu.pc, 1, "empty FIFO removes level request");

    let mut cpu = Cpu::new(&program(&[Nop])); receiver(&mut cpu);
    for byte in 1..=3 { cpu.rx(byte); }
    clocks(&mut cpu, 240);
    cpu.write(0xc1, 0);
    assert_eq!(cpu.read(0xc0) & 0x88, 0, "disable flushes FIFO and waiting shifter");
    cpu.write(0xc1, 0x10);
    cpu.rx(4); clocks(&mut cpu, 80);
    assert_eq!(cpu.read(0xc6), 4, "no stale waiting byte survives disable");
    assert_eq!(cpu.read(0xc0) & 0x80, 0);

    cpu.rx(5); cpu.rx(6); clocks(&mut cpu, 40);
    cpu.write(0xc1, 0); cpu.write(0xc1, 0x10);
    clocks(&mut cpu, 40);
    assert_eq!(cpu.read(0xc0) & 0x80, 0, "disable also aborts in-flight frame");
    clocks(&mut cpu, 80);
    assert_eq!(cpu.read(0xc6), 6, "external queued sender still starts at next boundary");
    cpu.write(0xc1, 0); cpu.rx(7); clocks(&mut cpu, 80);
    assert_eq!(cpu.read(0xc0) & 0x80, 0, "completion with RXEN clear is dropped");
    assert_eq!(cpu.rx_lost, 0, "disabled reception is not a data overrun");
}

#[test]
fn rx_frame_lengths_mask_unused_bits_and_sample_baud_at_each_start() {
    for size in 0..=3 {
        for stops in 1..=2 {
            let mut cpu = Cpu::new(&program(&[Nop])); receiver(&mut cpu);
            cpu.write(0xc2, (size << 1) | ((stops - 1) << 3));
            cpu.rx(0xff);
            let frame = (1 + 5 + size as u64 + stops as u64) * 8;
            clocks(&mut cpu, frame - 1);
            assert_eq!(cpu.read(0xc0) & 0x80, 0);
            clocks(&mut cpu, 1);
            assert_eq!(cpu.read(0xc0) & 0x80, 0x80);
            assert_eq!(cpu.read(0xc6), (0xffu16 >> (3 - size)) as u8);
        }
    }
    let mut cpu = Cpu::new(&program(&[Nop])); receiver(&mut cpu);
    clocks(&mut cpu, 7); cpu.rx(1); cpu.rx(2);
    cpu.write(0xc4, 1);
    clocks(&mut cpu, 79); assert_eq!(cpu.read(0xc0) & 0x80, 0);
    clocks(&mut cpu, 1); assert_eq!(cpu.read(0xc6), 1); // 7 + 80
    clocks(&mut cpu, 159); assert_eq!(cpu.read(0xc0) & 0x80, 0);
    clocks(&mut cpu, 1); assert_eq!(cpu.read(0xc6), 2); // 87 + 160
    assert_eq!(cpu.cycles, 247);
}

#[test]
fn timer0_fast_pwm_exact_high_time_including_bottom_and_top() {
    for duty in [0, 1, 128, 255] {
        let mut cpu = Cpu::new(&program(&[Nop]));
        cpu.write(0x2a, 0x60);
        cpu.write(0x47, duty); cpu.write(0x48, duty);
        cpu.write(0x44, 0xb3); // non-inverting A, inverting B, fast PWM
        cpu.write(0x45, 1);
        clocks(&mut cpu, 256); // first BOTTOM establishes the full waveform
        let mut high_a = 0; let mut high_b = 0;
        for phase in 0..256 {
            let level = cpu.read(0x29);
            let high = duty == 255 || phase <= duty as u16;
            assert_eq!(level & 0x40 != 0, high, "PD6 duty={duty}, phase={phase}");
            assert_eq!(level & 0x20 != 0, !high, "PD5 inverted waveform");
            high_a += u16::from(level & 0x40 != 0);
            high_b += u16::from(level & 0x20 != 0);
            clocks(&mut cpu, 1);
        }
        let expected = if duty == 255 { 256 } else { duty as u16 + 1 };
        assert_eq!((high_a, high_b), (expected, 256 - expected));
        let expected_edges = if duty == 255 { vec![(256, true)] }
            else { vec![(256, true), (257 + duty as u64, false), (512, true)] };
        assert_eq!(edges(&cpu, 'D', 6), expected_edges, "exact one-tick BOTTOM spike and TOP constant");
    }
}

#[test]
fn fast_pwm_ocr_transfers_only_at_bottom_and_edges_inside_call_are_exact() {
    for (a, b, ocr, pin) in [(0x44, 0x45, 0x47, ('D', 6)), (0xb0, 0xb1, 0xb3, ('B', 3))] {
        let mut cpu = Cpu::new(&program(&[Nop, Call(0)]));
        cpu.write(0x24, 8); cpu.write(0x2a, 0x40);
        cpu.write(ocr, 128); cpu.write(a, 0x83); cpu.write(b, 1);
        clocks(&mut cpu, 300);
        cpu.write(ocr, 0);
        assert_eq!(cpu.read(ocr), 0, "CPU reads buffer before transfer");
        clocks(&mut cpu, 211); // old pulse falls at 385, next BOTTOM at 512
        cpu.pc = 1; cpu.step().unwrap(); // CALL spans 511..515
        assert_eq!(cpu.cycles, 515);
        assert_eq!(edges(&cpu, pin.0, pin.1), [(256, true), (385, false), (512, true), (513, false)]);
    }
}

#[test]
fn timer1_phase_correct_both_pins_and_ocr_write_waits_for_top() {
    let mut cpu = Cpu::new(&program(&[Nop]));
    cpu.write(0x24, 6);
    write16(&mut cpu, 0x88, 32); write16(&mut cpu, 0x8a, 64);
    cpu.write(0x80, 0xa1); cpu.write(0x81, 1); // Arduino's 8-bit phase-correct mode
    clocks(&mut cpu, 530);
    assert_eq!(edges(&cpu, 'B', 1), [(479, true)]);
    assert_eq!(edges(&cpu, 'B', 2), [(447, true)]);
    write16(&mut cpu, 0x88, 128);
    assert_eq!(cpu.read(0x88), 128);
    clocks(&mut cpu, 235); // TCNT=TOP; still old comparator on this clock
    assert_eq!((cpu.read(0x84), cpu.read(0x85)), (255, 0));
    assert_eq!(edges(&cpu, 'B', 1), [(479, true), (543, false)], "old duty remains on upward slope");
    clocks(&mut cpu, 384);
    assert_eq!(cpu.cycles, 1149);
    assert_eq!(edges(&cpu, 'B', 1), [(479, true), (543, false), (893, true), (1149, false)]);
    assert_eq!(edges(&cpu, 'B', 2), [(447, true), (575, false), (957, true), (1085, false)]);
    assert_eq!(cpu.read(0x23) & 6, 0, "PINB returns OC-driven levels");
    // First A pulse: 543-479=64=2*32; after TOP update: 1149-893=256=2*128.
}

#[test]
fn phase_correct_extremes_are_constant_and_top_updates_restore_symmetry() {
    for (a, b, oa, ob, pa, pb, wide) in [
        (0x44, 0x45, 0x47, 0x48, ('D', 6), ('D', 5), false),
        (0x80, 0x81, 0x88, 0x8a, ('B', 1), ('B', 2), true),
        (0xb0, 0xb1, 0xb3, 0xb4, ('B', 3), ('D', 3), false),
    ] {
        for duty in [0, 255] {
            let mut cpu = Cpu::new(&program(&[Nop]));
            cpu.write(0x24, 0x0e); cpu.write(0x2a, 0x68);
            if wide { write16(&mut cpu, oa, duty); write16(&mut cpu, ob, duty); }
            else { cpu.write(oa, duty as u8); cpu.write(ob, duty as u8); }
            cpu.write(a, 0xb1); cpu.write(b, 1);
            clocks(&mut cpu, 510);
            let a_edges = edges(&cpu, pa.0, pa.1).len();
            let b_edges = edges(&cpu, pb.0, pb.1).len();
            clocks(&mut cpu, 510);
            assert_eq!(edges(&cpu, pa.0, pa.1).len(), a_edges, "extreme A has no spikes");
            assert_eq!(edges(&cpu, pb.0, pb.1).len(), b_edges, "inverted extreme B has no spikes");
            let pin_addr = |port| if port == 'B' { 0x23 } else { 0x29 };
            assert_eq!(cpu.read(pin_addr(pa.0)) & (1 << pa.1) != 0, duty == 255);
            assert_eq!(cpu.read(pin_addr(pb.0)) & (1 << pb.1) != 0, duty == 0);
            if duty == 255 {
                if wide { write16(&mut cpu, oa, 64); } else { cpu.write(oa, 64); }
                clocks(&mut cpu, 256); // next TOP transfer at 1276
                assert_eq!(edges(&cpu, pa.0, pa.1).last(), Some(&(1276, false)), "MAX-to-lower write restores symmetry at TOP");
            }
        }
    }
}

#[test]
fn timer1_all_fixed_pwm_resolutions_mask_ocr_and_set_overflow_at_mode_boundary() {
    for (phase_mode, top) in [(1, 255u16), (2, 511), (3, 1023)] {
        for fast in [false, true] {
            let mode = phase_mode + if fast { 4 } else { 0 };
            let mut cpu = Cpu::new(&program(&[Nop]));
            cpu.write(0x80, mode & 3); cpu.write(0x81, (mode & 12) << 1);
            write16(&mut cpu, 0x88, 0xffff); write16(&mut cpu, 0x8a, 0xffff);
            assert_eq!(u16::from_le_bytes([cpu.read(0x88), cpu.read(0x89)]), top);
            assert_eq!(u16::from_le_bytes([cpu.read(0x8a), cpu.read(0x8b)]), top);
            cpu.write(0x81, ((mode & 12) << 1) | 1);
            let period = if fast { top as u64 + 1 } else { 2 * top as u64 };
            clocks(&mut cpu, period - 1);
            assert_eq!(cpu.read(0x36) & 1, 0, "no early TOV mode={mode}");
            clocks(&mut cpu, 1);
            assert_eq!(cpu.read(0x36) & 1, 1, "TOV boundary mode={mode}");
            assert_eq!(u16::from_le_bytes([cpu.read(0x84), cpu.read(0x85)]), 0);
            cpu.write(0x36, 1);
            clocks(&mut cpu, period - 1); assert_eq!(cpu.read(0x36) & 1, 0);
            clocks(&mut cpu, 1); assert_eq!(cpu.read(0x36) & 1, 1);
        }
    }
}

#[test]
fn timer1_fixed_pwm_resolutions_drive_exact_noninverted_and_inverted_pulses() {
    for (phase_mode, top) in [(1, 255u16), (2, 511), (3, 1023)] {
        for fast in [false, true] {
            let mut cpu = Cpu::new(&program(&[Nop]));
            let duty = top / 2;
            write16(&mut cpu, 0x88, duty); write16(&mut cpu, 0x8a, duty);
            cpu.write(0x24, 6);
            let mode = phase_mode + if fast { 4 } else { 0 };
            cpu.write(0x80, 0xb0 | (mode & 3)); cpu.write(0x81, ((mode & 12) << 1) | 1);
            let period = if fast { top as u64 + 1 } else { 2 * top as u64 };
            let rise = if fast { period } else { period - duty as u64 + 1 };
            let fall = period + duty as u64 + 1;
            clocks(&mut cpu, fall);
            assert_eq!(edges(&cpu, 'B', 1), [(rise, true), (fall, false)], "mode {mode} noninverting A");
            assert_eq!(edges(&cpu, 'B', 2), [(duty as u64 + 1, true), (rise, false), (fall, true)],
                "mode {mode} inverted B");
        }
    }
}

#[test]
fn timer1_timerone_sequence_runs_25_khz_phase_frequency_correct() {
    let mut a = Asm::new();
    a.emit(Nop); // also the one-clock instruction used by clocks after setup
    for (addr, value) in [
        (0x24, 6),
        (0x80, 1), (0x81, 3), // Arduino init: WGM 1, clk/64
        (0x81, 0x10),         // stopped WGM 9
        (0x80, 0),            // stopped WGM 8
        (0x87, 1), (0x86, 64), // ICR1=320, high first
        (0x81, 0x11),         // WGM 8, clk/1, completes at 25
        (0x89, 0), (0x88, 160), (0x8b, 0), (0x8a, 1),
        (0x80, 0xa0),         // connect both non-inverting outputs
    ] { a.emit(Ldi(16, value)); a.emit(Sts(addr, 16)); }
    a.emit(Lds(18, 0x86)); a.emit(Lds(19, 0x87)); a.emit(Break);
    let mut cpu = Cpu::new(&a.finish().unwrap());
    assert_eq!(cpu.run(100), Stop::Break, "all six TimerOne steps execute");
    assert_eq!(cpu.cycles, 45, "NOP + thirteen LDI/STS pairs + two LDS + BREAK");
    assert_eq!((cpu.r[18], cpu.r[19]), (64, 1), "ICR1 low-first reads latch 320 through TEMP");

    // No /64 edge before stopping Arduino's timer. Restart at 25 gives
    // TOP equality at 25+320=345, ICF at 346, BOTTOM/TOV/transfer at 665.
    // Each steady period is 2*320=640 clocks, or 25 kHz at 16 MHz.
    clocks(&mut cpu, 300);
    assert_eq!((cpu.cycles, cpu.read(0x84), cpu.read(0x85)), (345, 64, 1), "first TOP equality");
    assert_eq!(cpu.read(0x36) & 0x21, 0, "neither ICF nor TOV before TOP action");
    clocks(&mut cpu, 1);
    assert_eq!(cpu.read(0x36) & 0x21, 0x20, "ICF on the post-equality TOP clock");
    cpu.write(0x36, 0x20);
    assert_eq!(cpu.read(0x36) & 0x20, 0, "ICF is write-one-to-clear");
    clocks(&mut cpu, 318);
    assert_eq!(cpu.read(0x36) & 1, 0, "no TOV one clock before BOTTOM");
    clocks(&mut cpu, 1);
    assert_eq!((cpu.cycles, cpu.read(0x36) & 1), (665, 1), "TOV on reaching BOTTOM");
    clocks(&mut cpu, 2081);
    // Initial OCR buffers transfer at 665. Down-match rise is
    // 25+2*640-duty+1; next up-match fall is 25+2*640+duty+1.
    assert_eq!(edges(&cpu, 'B', 1), [
        (1146, true), (1466, false), (1786, true), (2106, false), (2426, true), (2746, false),
    ], "PB1 repeats 640-clock periods with exactly 320 high clocks");
    assert_eq!(edges(&cpu, 'B', 2), [
        (1305, true), (1307, false), (1945, true), (1947, false), (2585, true), (2587, false),
    ], "OCR1B=1 repeats two-clock high pulses");

    write16(&mut cpu, 0x88, 80); write16(&mut cpu, 0x8a, 319);
    assert_eq!(cpu.read(0x88), 80, "CPU reads the pending OCR buffer");
    cpu.events.clear();
    clocks(&mut cpu, 1439);
    // Write at 2746 (TCNT=161): TOP action 2906 retains 160, so rise
    // is 3225-160+1=3066. BOTTOM 3225 transfers 80: fall 3225+80+1=3306.
    assert_eq!(edges(&cpu, 'B', 1), [
        (3066, true), (3306, false), (3786, true), (3946, false),
    ], "WGM 8 transfers at BOTTOM, never the intervening TOP");
    assert_eq!(edges(&cpu, 'B', 2), [
        (3225, true), (3545, false), (3547, true), (4185, false),
    ], "OCR1B=319 gives 638 high clocks after the transfer period");

    write16(&mut cpu, 0x88, 0); cpu.events.clear();
    clocks(&mut cpu, 320);
    assert_eq!((cpu.cycles, cpu.read(0x23) & 2), (4505, 0), "new zero duty clears at BOTTOM transfer");
    clocks(&mut cpu, 640);
    assert_eq!(edges(&cpu, 'B', 1), [(4426, true), (4505, false)], "zero has no high cycle after transfer");
    write16(&mut cpu, 0x88, 320); cpu.events.clear();
    clocks(&mut cpu, 639);
    assert_eq!(cpu.read(0x23) & 2, 0, "TOP duty is still buffered before BOTTOM 5785");
    clocks(&mut cpu, 1);
    assert_eq!(cpu.read(0x23) & 2, 2, "TOP duty becomes high at the transfer itself");
    clocks(&mut cpu, 640);
    assert_eq!(edges(&cpu, 'B', 1), [(5785, true)], "TOP is constant through the first full period");

    // Inverted B has the opposite constants, including the transfer clock.
    cpu.write(0x80, 0xb0); write16(&mut cpu, 0x8a, 0);
    clocks(&mut cpu, 640); cpu.events.clear();
    assert_eq!((cpu.cycles, cpu.read(0x23) & 4), (7065, 4), "inverted zero is high at BOTTOM");
    clocks(&mut cpu, 640);
    assert!(edges(&cpu, 'B', 2).is_empty(), "inverted zero has no low spike");
    write16(&mut cpu, 0x8a, 320);
    clocks(&mut cpu, 640);
    assert_eq!((cpu.cycles, cpu.read(0x23) & 4), (8345, 0), "inverted TOP clears at BOTTOM");
    clocks(&mut cpu, 640);
    assert_eq!(edges(&cpu, 'B', 2), [(8345, false)], "inverted TOP has no high spike");
}

#[test]
fn timer1_phase_correct_icr1_updates_at_top_and_ocr1a_top_modes_buffer_top() {
    // Identical TCNT=161 rewrites in modes 8 and 10. With origin zero,
    // BOTTOMs are 640/1280 and TOP actions 321/961. Updating at TOP changes
    // the descending match to 1280-80+1=1201; BOTTOM update retains the
    // 1280-160+1=1121 match. Both next up-matches are 1280+80+1=1361.
    for (mode, expected) in [
        (8, vec![(1121, true), (1361, false)]),
        (10, vec![(1201, true), (1361, false)]),
    ] {
        let mut cpu = Cpu::new(&program(&[Nop]));
        cpu.write(0x24, 6); write16(&mut cpu, 0x88, 160);
        cpu.write(0x80, 0x80 | (mode & 3)); cpu.write(0x81, 0x10);
        write16(&mut cpu, 0x86, 320); cpu.write(0x81, 0x11);
        clocks(&mut cpu, 801); write16(&mut cpu, 0x88, 80); cpu.events.clear();
        clocks(&mut cpu, 159);
        cpu.write(0x36, 0x20);
        assert_eq!(cpu.read(0x36) & 0x20, 0, "ICF cleared just before TOP action");
        clocks(&mut cpu, 1);
        assert_eq!((cpu.cycles, cpu.read(0x36) & 0x20), (961, 0x20), "ICR TOP flag in both dual-slope modes");
        clocks(&mut cpu, 400);
        assert_eq!(edges(&cpu, 'B', 1), expected, "mode {mode} selects TOP versus BOTTOM OCR transfer");
        cpu.write(0x80, 0x50 | (mode & 3)); cpu.write(0x25, 6); cpu.events.clear();
        clocks(&mut cpu, 640);
        assert!(cpu.events.is_empty(), "COM=01 disconnects both outputs in WGM {mode}");
        assert_eq!(cpu.read(0x23) & 6, 6, "normal port operation in non-toggle PWM modes");
    }

    for mode in [9, 11] {
        let mut cpu = Cpu::new(&program(&[Nop]));
        cpu.write(0x24, 6); write16(&mut cpu, 0x88, 7); write16(&mut cpu, 0x8a, 2);
        cpu.write(0x80, 0x60 | (mode & 3)); cpu.write(0x81, 0x11);
        clocks(&mut cpu, 5);
        write16(&mut cpu, 0x88, 11); write16(&mut cpu, 0x8a, 3);
        assert_eq!(cpu.read(0x88), 11, "TOP buffer reads ahead of active TOP");
        clocks(&mut cpu, 2);
        assert_eq!(cpu.read(0x36) & 0x23, 0, "old TOP 7 reached without early flags");
        clocks(&mut cpu, 1);
        assert_eq!(cpu.read(0x36) & 0x23, 2, "COMPA acts at old TOP on clock 8, without ICF");
        clocks(&mut cpu, 32);
        // Old TOP action=8, old BOTTOM=14. New TOP=11 gives next TOP
        // action 14+11+1=26. A toggles once per PWM period (f/2).
        assert_eq!(edges(&cpu, 'B', 1), [(8, true), (26, false)], "active OCR1A controls TOP and toggle frequency");
        let expected_b = if mode == 9 { vec![(13, true), (18, false), (34, true), (40, false)] }
            else { vec![(12, true), (18, false), (34, true), (40, false)] };
        assert_eq!(edges(&cpu, 'B', 2), expected_b, "B down-match uses the mode's OCR update point");
        assert_eq!(cpu.read(0x36) & 0x20, 0, "OCR1A TOP never sets ICF");
        if mode == 9 {
            // BOTTOMs after the new TOP settles: 36,58,80,102,124,146,168.
            write16(&mut cpu, 0x8a, 0); cpu.events.clear();
            clocks(&mut cpu, 18);
            assert_eq!(edges(&cpu, 'B', 2), [(56, true), (58, false)], "WGM 9 zero overrides the old descending pulse at transfer");
            write16(&mut cpu, 0x8a, 11);
            clocks(&mut cpu, 22);
            assert_eq!((cpu.cycles, cpu.read(0x23) & 4), (80, 4), "WGM 9 TOP becomes high at BOTTOM");
            cpu.events.clear(); clocks(&mut cpu, 22);
            assert!(edges(&cpu, 'B', 2).is_empty(), "WGM 9 TOP has no spike in its first period");
            cpu.write(0x80, 0x71); write16(&mut cpu, 0x8a, 0);
            clocks(&mut cpu, 22); cpu.events.clear();
            assert_eq!((cpu.cycles, cpu.read(0x23) & 4), (124, 4), "WGM 9 inverted zero transfers high");
            clocks(&mut cpu, 22);
            assert!(edges(&cpu, 'B', 2).is_empty(), "WGM 9 inverted zero stays high");
            write16(&mut cpu, 0x8a, 11);
            clocks(&mut cpu, 22);
            assert_eq!((cpu.cycles, cpu.read(0x23) & 4), (168, 0), "WGM 9 inverted TOP transfers low");
            clocks(&mut cpu, 22);
            assert_eq!(edges(&cpu, 'B', 2), [(168, false)], "WGM 9 inverted TOP stays low");
        }
    }
}

#[test]
fn timer1_fast_and_ctc_icr1_modes() {
    let mut cpu = Cpu::new(&program(&[Nop]));
    cpu.write(0x24, 6); write16(&mut cpu, 0x88, 24); write16(&mut cpu, 0x8a, 74);
    cpu.write(0x80, 0xa2); cpu.write(0x81, 0x18);
    write16(&mut cpu, 0x86, 99); cpu.write(0x81, 0x1a);
    // WGM 14 /8: TOP equality at 99*8=792, wrap/ICF/TOV at 100*8=800.
    // Non-inverting high times are (OCR+1)*8: A=200 and B=600 clocks.
    clocks(&mut cpu, 792);
    assert_eq!(cpu.read(0x36) & 0x21, 0, "fast TOP flags wait for post-equality clock");
    clocks(&mut cpu, 8);
    assert_eq!((cpu.cycles, cpu.read(0x36) & 0x21), (800, 0x21), "ICF and TOV together at TOP-to-zero");
    cpu.write(0x36, 0x20);
    assert_eq!(cpu.read(0x36) & 0x21, 1, "ICF W1C preserves TOV");
    clocks(&mut cpu, 1600);
    assert_eq!(edges(&cpu, 'B', 1), [(800, true), (1000, false), (1600, true), (1800, false), (2400, true)],
        "WGM 14 A period 800, high time 200");
    assert_eq!(edges(&cpu, 'B', 2), [(800, true), (1400, false), (1600, true), (2200, false), (2400, true)],
        "WGM 14 B period 800, high time 600");
    cpu.write(0x80, 0x52); cpu.write(0x25, 4); cpu.events.clear();
    clocks(&mut cpu, 1000);
    assert_eq!(edges(&cpu, 'B', 1), [(2600, false), (3400, true)], "WGM 14 COM1A=01 toggles at compare");
    assert!(edges(&cpu, 'B', 2).is_empty(), "COM1B=01 leaves the port in control throughout PWM");
    assert_eq!(cpu.read(0x23) & 4, 4, "PORTB drives disconnected OC1B");

    // At 3400 TCNT=25. Immediate ICR=49 gives TOP action at
    // 3200+(49+1)*8=3600, before the old TOP's 4000 boundary.
    write16(&mut cpu, 0x86, 49); cpu.write(0x36, 0x27);
    clocks(&mut cpu, 192);
    assert_eq!((cpu.cycles, cpu.read(0x84), cpu.read(0x36) & 0x21), (3592, 49, 0), "ICR changes the current fast period");
    clocks(&mut cpu, 8);
    assert_eq!((cpu.cycles, cpu.read(0x84), cpu.read(0x36) & 0x21), (3600, 0, 0x21), "ICR TOP is not double buffered");
    write16(&mut cpu, 0x86, 65535); write16(&mut cpu, 0x84, 65534);
    write16(&mut cpu, 0x86, 9); cpu.write(0x36, 0x27);
    clocks(&mut cpu, 8);
    assert_eq!((cpu.read(0x84), cpu.read(0x85)), (255, 255), "fast missed TOP reaches MAX");
    clocks(&mut cpu, 8);
    assert_eq!((cpu.cycles, cpu.read(0x84), cpu.read(0x85), cpu.read(0x36) & 0x21), (3616, 0, 0, 0),
        "fast missed TOP wraps at MAX without inventing a TOP flag");
    clocks(&mut cpu, 80);
    assert_eq!((cpu.cycles, cpu.read(0x84), cpu.read(0x36) & 0x21), (3696, 0, 0x21), "fast counting resumes against lowered ICR TOP");

    let mut cpu = Cpu::new(&program(&[Nop]));
    cpu.write(0x24, 6); write16(&mut cpu, 0x88, 99); write16(&mut cpu, 0x8a, 24);
    cpu.write(0x80, 0x63); cpu.write(0x81, 0x19); // WGM 15 /1
    clocks(&mut cpu, 40); write16(&mut cpu, 0x88, 49); write16(&mut cpu, 0x8a, 9);
    clocks(&mut cpu, 59);
    assert_eq!(cpu.read(0x36) & 0x23, 0, "new OCR1A TOP waits for old TOP-to-zero");
    clocks(&mut cpu, 1);
    assert_eq!(cpu.read(0x36) & 0x23, 3, "old TOP 99 acts at 100 without ICF");
    clocks(&mut cpu, 110);
    // First TOP action is 100; new TOP 49 repeats every 50 clocks, B high=10.
    assert_eq!(edges(&cpu, 'B', 1), [(100, true), (150, false), (200, true)], "WGM 15 toggles against buffered active TOP");
    assert_eq!(edges(&cpu, 'B', 2), [(100, true), (110, false), (150, true), (160, false), (200, true), (210, false)],
        "WGM 15 transfers B duty at BOTTOM with TOP");

    let mut cpu = Cpu::new(&program(&[Nop, Sleep]));
    cpu.write(0x81, 0x18); write16(&mut cpu, 0x86, 9);
    write16(&mut cpu, 0x88, 9); write16(&mut cpu, 0x8a, 3);
    cpu.write(0x81, 0x19); // WGM 12 /1
    clocks(&mut cpu, 3);
    assert_eq!(cpu.read(0x36) & 4, 0, "CTC B equality precedes its flag");
    clocks(&mut cpu, 1);
    assert_eq!(cpu.read(0x36) & 4, 4, "CTC COMPB at its own match on clock 4");
    write16(&mut cpu, 0x88, 6);
    clocks(&mut cpu, 2);
    assert_eq!(cpu.read(0x36) & 2, 0, "CTC immediate A equality precedes its flag");
    clocks(&mut cpu, 1);
    assert_eq!((cpu.cycles, cpu.read(0x84), cpu.read(0x36) & 2), (7, 7, 2), "OCF1A on clock 7 does not clear an ICR CTC counter");
    write16(&mut cpu, 0x88, 9); cpu.write(0x36, 0x27);
    cpu.write(0x6f, 0x20); cpu.sreg = 128;
    cpu.write(0xb2, 253); cpu.write(0xb1, 1); // disabled Timer2 OVF also accumulates at 10
    cpu.pc = 1; cpu.step().unwrap(); // SLEEP completes at 8; only ICIE1 can wake
    assert_eq!(cpu.run(1), Stop::Cycles, "sleep projects the ICF wake source");
    assert_eq!((cpu.cycles, cpu.read(0x84), cpu.read(0x36) & 0x22), (9, 9, 0), "no ICF or COMPA at equality alone");
    assert_eq!(cpu.run(1), Stop::Cycles, "sleep reaches ICR TOP exactly");
    assert_eq!((cpu.cycles, cpu.read(0x84), cpu.read(0x36) & 0x23), (10, 0, 0x22), "ICF and COMPA become pending on the same CTC clock");
    cpu.write(0x81, 0x18); cpu.write(0xb1, 0);
    cpu.write(0x6f, 0x22); cpu.write(0x70, 1);
    // TOP at 10 + idle wake 4 + entry 4 = 18. Further entries take four.
    cpu.step().unwrap();
    assert_eq!((cpu.pc, cpu.cycles), (18, 18), "Timer2 OVF has priority over CAPT");
    assert_eq!(cpu.read(0x36) & 0x22, 0x22, "Timer2 entry preserves both Timer1 requests");
    cpu.sreg = 128; cpu.step().unwrap();
    assert_eq!((cpu.pc, cpu.cycles), (20, 22), "CAPT has priority over simultaneous COMPA");
    assert_eq!(cpu.read(0x36) & 0x22, 2, "CAPT entry clears only ICF");
    cpu.sreg = 128; cpu.step().unwrap();
    assert_eq!((cpu.pc, cpu.cycles, cpu.read(0x36) & 0x22), (22, 26, 0), "COMPA follows CAPT and clears only its flag");

    cpu.write(0x6f, 0); cpu.write(0x70, 0);
    write16(&mut cpu, 0x86, 20); cpu.write(0x81, 0x19);
    clocks(&mut cpu, 8); write16(&mut cpu, 0x86, 3); cpu.write(0x36, 0x27);
    // At 34 TCNT=8; lowered TOP=3 is missed. MAX at 34+(65535-8)
    // =65561, wrap/TOV at 65562, next TOP action four clocks later.
    clocks(&mut cpu, 65527);
    assert_eq!((cpu.cycles, cpu.read(0x84), cpu.read(0x85), cpu.read(0x36) & 0x21), (65561, 255, 255, 0),
        "CTC missed ICR TOP runs all the way to MAX");
    clocks(&mut cpu, 1);
    assert_eq!((cpu.cycles, cpu.read(0x84), cpu.read(0x85), cpu.read(0x36) & 0x21), (65562, 0, 0, 1),
        "CTC TOV is at MAX wrap, not ICR TOP");
    clocks(&mut cpu, 3);
    assert_eq!(cpu.read(0x36) & 0x20, 0, "lowered TOP equality still precedes ICF");
    clocks(&mut cpu, 1);
    assert_eq!((cpu.cycles, cpu.read(0x84), cpu.read(0x36) & 0x21), (65566, 0, 0x21), "lowered CTC TOP resumes after MAX wrap");
}

#[test]
fn icr1_writes_only_in_icr1_top_modes() {
    let mut cpu = Cpu::new(&program(&[Nop]));
    for (mode, value) in [(8, 0x4567), (10, 0x89ab), (12, 0xcdef), (14, 0x1234)] {
        cpu.write(0x81, 0); cpu.write(0x80, mode & 3); cpu.write(0x81, (mode & 12) << 1);
        write16(&mut cpu, 0x86, value);
        assert_eq!(u16::from_le_bytes([cpu.read(0x86), cpu.read(0x87)]), value, "ICR1 write accepted in TOP mode {mode}");
    }
    for mode in [0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13, 15] {
        cpu.write(0x81, 0); cpu.write(0x80, mode & 3); cpu.write(0x81, (mode & 12) << 1);
        cpu.write(0x87, 0xab);
        assert_eq!(cpu.read(0x87), 0xab, "ICR1H writes TEMP even outside TOP modes");
        cpu.write(0x86, 0xcd);
        assert_eq!((cpu.read(0x86), cpu.read(0x87)), (0x34, 0x12), "ICR1L write ignored in mode {mode}");
    }
    cpu.write(0x81, 0); cpu.write(0x80, 0);
    cpu.write(0x87, 0xab); cpu.write(0x88, 0x56); cpu.write(0x84, 0x78);
    assert_eq!((cpu.read(0x88), cpu.read(0x89)), (0x56, 0xab), "ICR1H supplies OCR1A's shared TEMP high byte");
    cpu.write(0x87, 0xcd);
    assert_eq!((cpu.read(0x88), cpu.read(0x89), cpu.read(0x87)), (0x56, 0xab, 0xcd), "OCR reads bypass TEMP");
    assert_eq!((cpu.read(0x84), cpu.read(0x87)), (0x78, 0xab), "TCNT1 low read latches into ICR1H's TEMP");
    assert_eq!(cpu.read(0x86), 0x34, "ICR1 low read leaves its stored value intact");
    cpu.write(0x8a, 0xef);
    assert_eq!((cpu.read(0x8a), cpu.read(0x8b)), (0xef, 0x12), "ICR1 low read supplies OCR1B's shared TEMP");
    cpu.write(0x85, 0x56); cpu.write(0x81, 0x18); cpu.write(0x86, 0x78);
    assert_eq!((cpu.read(0x86), cpu.read(0x87)), (0x78, 0x56), "TCNT1 high write supplies an ICR1 low write");
    clocks(&mut cpu, 1);
    assert_eq!(cpu.cycles, 1, "all stopped configurations and ignored writes remain executable");
}

#[test]
fn timer2_phase_correct_exact_edges_on_pb3_and_pd3() {
    let mut cpu = Cpu::new(&program(&[Nop]));
    cpu.write(0x24, 8); cpu.write(0x2a, 8);
    cpu.write(0xb3, 64); cpu.write(0xb4, 128);
    cpu.write(0xb0, 0xa1); cpu.write(0xb1, 1);
    clocks(&mut cpu, 509); assert_eq!(cpu.read(0x37) & 1, 0);
    clocks(&mut cpu, 1); assert_eq!(cpu.read(0x37) & 1, 1);
    clocks(&mut cpu, 511);
    assert_eq!(edges(&cpu, 'B', 3), [(447, true), (575, false), (957, true)]);
    assert_eq!(edges(&cpu, 'D', 3), [(383, true), (639, false), (893, true)]);
}

#[test]
fn eight_bit_ocr_top_pwm_toggle_a_and_buffered_top_changes() {
    for (a, b, oa, ob, flag, pa, pb) in [
        (0x44, 0x45, 0x47, 0x48, 0x35, ('D', 6), ('D', 5)),
        (0xb0, 0xb1, 0xb3, 0xb4, 0x37, ('B', 3), ('D', 3)),
    ] {
        for fast in [false, true] {
            let mut cpu = Cpu::new(&program(&[Nop]));
            cpu.write(0x24, 8); cpu.write(0x2a, 0x68);
            cpu.write(oa, 7); cpu.write(ob, 2);
            cpu.write(a, if fast { 0x63 } else { 0x61 }); cpu.write(b, 9);
            clocks(&mut cpu, 5); cpu.write(oa, 11); cpu.write(ob, 3);
            clocks(&mut cpu, 2);
            assert_eq!(cpu.read(flag) & 3, 0, "new TOP must not take effect mid-slope");
            clocks(&mut cpu, 1);
            assert_eq!(cpu.read(flag) & 3, if fast { 3 } else { 2 }, "first TOP equality acts at clock 8");
            clocks(&mut cpu, 32);
            let expected_a = if fast { vec![(8, true), (20, false), (32, true)] }
                else { vec![(8, true), (26, false)] };
            assert_eq!(edges(&cpu, pa.0, pa.1), expected_a, "OCnA toggles with buffered OCRnA TOP");
            let expected_b = if fast { vec![(8, true), (12, false), (20, true), (24, false), (32, true), (36, false)] }
                else { vec![(12, true), (18, false), (34, true), (40, false)] };
            assert_eq!(edges(&cpu, pb.0, pb.1), expected_b, "OCnB duty follows buffered compare");
        }
    }
}

fn interrupt_counter_image(vector: u32) -> Vec<u16> {
    let mut a = Asm::new(); let isr = a.label();
    a.emit(Sleep); a.emit(Rjmp(-2));
    a.org(vector); a.jump(Target::Rjmp, isr);
    a.org(64); a.bind(isr); a.emit(Inc(17)); a.emit(Sbi(3, 5)); a.emit(Reti);
    a.finish().unwrap()
}

#[test]
fn timer2_ctc_interrupts_every_320_clocks_and_timer0_millis_every_16384() {
    for (vector, period, fast) in [(14, 320u64, false), (32, 16384, true)] {
        let mut cpu = Cpu::new(&interrupt_counter_image(vector));
        cpu.write(0x24, 0x20); cpu.sreg = 128;
        if fast {
            cpu.write(0x44, 3); cpu.write(0x45, 3); cpu.write(0x6e, 1); // /64
        } else {
            cpu.write(0xb3, 9); cpu.write(0xb0, 2); cpu.write(0xb1, 3); cpu.write(0x70, 2); // /32
        }
        assert_eq!(cpu.run(4 * period + 32), Stop::Cycles);
        assert_eq!(cpu.cycles, 4 * period + 32);
        assert_eq!(cpu.r[17], 4, "four timer interrupts over fixed span");
        assert_eq!(cpu.sp, 0x8ff);
        let expected: Vec<_> = (1..=4).map(|n| (n * period + 13, n % 2 == 1)).collect();
        // wake 4 + entry 4 + RJMP 2 + INC 1 + SBI 2 = 13.
        assert_eq!(edges(&cpu, 'B', 5), expected);
    }
}

#[test]
fn prescalers_share_timer01_phase_but_timer2_reset_and_hold_are_independent() {
    let mut cpu = Cpu::new(&program(&[Nop]));
    clocks(&mut cpu, 5);
    cpu.write(0x45, 2); cpu.write(0x81, 2); cpu.write(0xb1, 2);
    cpu.write(0x43, 2); // only Timer2 reset at clock 5
    clocks(&mut cpu, 3);
    assert_eq!((cpu.read(0x46), cpu.read(0x84), cpu.read(0xb2)), (1, 1, 0));
    clocks(&mut cpu, 5);
    assert_eq!(cpu.read(0xb2), 1, "Timer2 first tick at 13, not shared tick 8");
    cpu.write(0x43, 0x81); // hold sync only; Timer2 keeps running
    clocks(&mut cpu, 8);
    assert_eq!((cpu.read(0x46), cpu.read(0x84), cpu.read(0xb2)), (1, 1, 2));
    cpu.write(0x43, 0x83);
    clocks(&mut cpu, 17);
    assert_eq!((cpu.read(0x46), cpu.read(0x84), cpu.read(0xb2)), (1, 1, 2));
    cpu.write(0x43, 0);
    clocks(&mut cpu, 7);
    assert_eq!((cpu.read(0x46), cpu.read(0x84), cpu.read(0xb2)), (1, 1, 2));
    clocks(&mut cpu, 1);
    assert_eq!((cpu.read(0x46), cpu.read(0x84), cpu.read(0xb2)), (2, 2, 3));
    assert_eq!(cpu.cycles, 46);
    for (cs, div) in [(1, 1), (2, 8), (3, 32), (4, 64), (5, 128), (6, 256), (7, 1024)] {
        let mut cpu = Cpu::new(&program(&[Nop]));
        cpu.write(0xb1, cs);
        clocks(&mut cpu, div - 1); assert_eq!(cpu.read(0xb2), 0);
        clocks(&mut cpu, 1); assert_eq!(cpu.read(0xb2), 1, "Timer2 divisor {div}");
    }
}

#[test]
fn all_interrupt_vectors_follow_datasheet_priority_and_only_selected_flags_clear() {
    let mut cpu = Cpu::new(&program(&[Nop])); receiver(&mut cpu);
    cpu.write(0xc1, 0xf8); cpu.rx(0x55); cpu.write(0xc6, 0x66);
    // With I clear, accumulate all three flags on every timer and RX/TX/UDRE.
    cpu.write(0x46, 254); write16(&mut cpu, 0x84, 65534); cpu.write(0xb2, 254);
    cpu.write(0x45, 1); cpu.write(0x81, 1); cpu.write(0xb1, 1);
    clocks(&mut cpu, 3);
    cpu.write(0x45, 0); cpu.write(0x81, 0); cpu.write(0xb1, 0);
    clocks(&mut cpu, 77);
    for addr in [0x35, 0x36, 0x37] { assert_eq!(cpu.read(addr), 7); }
    for addr in [0x6e, 0x6f, 0x70] { cpu.write(addr, 7); }
    for vector in [14, 16, 18, 22, 24, 26, 28, 30, 32, 36, 38, 40] {
        cpu.sreg = 128; let before = cpu.cycles; cpu.step().unwrap();
        assert_eq!((cpu.pc, cpu.cycles), (vector, before + 4));
        if vector == 36 { assert_eq!(cpu.read(0xc6), 0x55); }
        if vector == 38 { cpu.write(0xc1, 0xd8); }
    }
    for addr in [0x35, 0x36, 0x37] { assert_eq!(cpu.read(addr), 0); }
    assert_eq!(cpu.read(0xc0) & 0xc0, 0);
}

#[test]
fn timer_flags_are_w1c_and_counter_write_blocks_both_compare_channels() {
    for (count, control, flag, io) in [(0x46, 0x45, 0x35, 0x15), (0xb2, 0xb1, 0x37, 0x17)] {
        let mut cpu = Cpu::new(&program(&[Nop, Cbi(io, 1), Sbi(io, 1)]));
        cpu.write(control, 1); cpu.write(count, 0);
        clocks(&mut cpu, 1);
        assert_eq!(cpu.read(flag), 0, "TCNT write inhibits A and B equality");
        clocks(&mut cpu, 255); assert_eq!(cpu.read(flag), 1);
        clocks(&mut cpu, 1); cpu.write(control, 0);
        assert_eq!(cpu.read(flag), 7);
        cpu.write(flag, 0); assert_eq!(cpu.read(flag), 7);
        cpu.pc = 1; cpu.step().unwrap(); assert_eq!(cpu.read(flag), 7);
        cpu.pc = 2; cpu.step().unwrap(); assert_eq!(cpu.read(flag), 5, "SBI acknowledges A only");
        cpu.write(flag, 4); assert_eq!(cpu.read(flag), 1);
        cpu.write(flag, 1); assert_eq!(cpu.read(flag), 0);
    }
}

#[test]
fn eight_bit_ctc_ocr_writes_are_immediate_and_missed_top_wraps_at_max() {
    for (a, b, count, oa, ob, flag) in [
        (0x44, 0x45, 0x46, 0x47, 0x48, 0x35),
        (0xb0, 0xb1, 0xb2, 0xb3, 0xb4, 0x37),
    ] {
        let mut cpu = Cpu::new(&program(&[Nop]));
        cpu.write(oa, 20); cpu.write(ob, 20); cpu.write(count, 10);
        cpu.write(a, 2); cpu.write(b, 1);
        clocks(&mut cpu, 1);
        cpu.write(oa, 12); cpu.write(ob, 12);
        clocks(&mut cpu, 1); assert_eq!(cpu.read(flag), 0);
        clocks(&mut cpu, 1);
        assert_eq!((cpu.cycles, cpu.read(count), cpu.read(flag)), (3, 0, 6), "both CTC compares update immediately");
        cpu.write(flag, 7); cpu.write(count, 254); cpu.write(oa, 2);
        clocks(&mut cpu, 2);
        assert_eq!((cpu.cycles, cpu.read(count), cpu.read(flag)), (5, 0, 1), "missed TOP wraps at MAX and overflows");
        clocks(&mut cpu, 3);
        assert_eq!((cpu.cycles, cpu.read(count), cpu.read(flag)), (8, 0, 3), "next TOP produces CTC clear and COMPA");
    }
}

#[test]
fn ocr1b_uses_shared_temp_on_writes_and_bypasses_temp_on_reads() {
    let mut cpu = Cpu::new(&program(&[Nop]));
    write16(&mut cpu, 0x8a, 0x1234);
    cpu.write(0x8b, 0xab);
    assert_eq!((cpu.read(0x8a), cpu.read(0x8b)), (0x34, 0x12));
    cpu.write(0x88, 0x56);
    assert_eq!((cpu.read(0x88), cpu.read(0x89)), (0x56, 0xab), "OCR1B reads leave shared TEMP untouched");
    write16(&mut cpu, 0x84, 0x5678);
    cpu.write(0x8b, 0xcd); cpu.read(0x84); cpu.write(0x8a, 0xef);
    assert_eq!((cpu.read(0x8a), cpu.read(0x8b)), (0xef, 0x56), "TCNT low read overwrites shared TEMP");
    write16(&mut cpu, 0x84, 3); write16(&mut cpu, 0x8a, 4);
    cpu.write(0x81, 1); clocks(&mut cpu, 1);
    assert_eq!(cpu.read(0x36) & 4, 0);
    clocks(&mut cpu, 1); assert_eq!(cpu.read(0x36) & 4, 4, "COMPB flag after equality");
    cpu.write(0x6f, 4); cpu.sreg = 128; cpu.step().unwrap(); assert_eq!(cpu.pc, 24);
}

#[test]
fn compare_outputs_toggle_clear_set_and_ddr_port_mux_is_live() {
    for (a, b, oa, ob, pa, pb, wide) in [
        (0x44, 0x45, 0x47, 0x48, ('D', 6), ('D', 5), false),
        (0x80, 0x81, 0x88, 0x8a, ('B', 1), ('B', 2), true),
        (0xb0, 0xb1, 0xb3, 0xb4, ('B', 3), ('D', 3), false),
    ] {
        let mut cpu = Cpu::new(&program(&[Nop]));
        if wide { write16(&mut cpu, oa, 3); write16(&mut cpu, ob, 1); }
        else { cpu.write(oa, 3); cpu.write(ob, 1); }
        cpu.write(a, if wide { 0x50 } else { 0x52 }); // toggle A/B, CTC
        cpu.write(b, if wide { 9 } else { 1 });
        clocks(&mut cpu, 4);
        assert!(cpu.events.is_empty(), "DDR inputs hide comparator output");
        let base = |port| if port == 'B' { 0x23 } else { 0x29 };
        cpu.write(base(pa.0) + 1, cpu.read(base(pa.0) + 1) | (1 << pa.1));
        cpu.write(base(pb.0) + 1, cpu.read(base(pb.0) + 1) | (1 << pb.1));
        assert_eq!(edges(&cpu, pa.0, pa.1), [(4, true)], "enabling DDR reveals running OC latch");
        assert_eq!(edges(&cpu, pb.0, pb.1), [(4, true)]);
        clocks(&mut cpu, 4);
        assert_eq!(edges(&cpu, pa.0, pa.1), [(4, true), (8, false)]);
        assert_eq!(edges(&cpu, pb.0, pb.1), [(4, true), (6, false)]);
        cpu.write(a, if wide { 0xe0 } else { 0xe2 }); // set A, clear B
        clocks(&mut cpu, 4);
        assert_eq!(edges(&cpu, pa.0, pa.1).last(), Some(&(12, true)));
        cpu.write(base(pa.0) + 2, 0);
        assert_ne!(cpu.read(base(pa.0)) & (1 << pa.1), 0, "PORT cannot override connected OC");
        cpu.write(a, if wide { 0 } else { 2 });
        assert_eq!(edges(&cpu, pa.0, pa.1).last(), Some(&(12, false)), "disconnect restores PORT immediately");
        cpu.write(a, if wide { 0x90 } else { 0x92 }); // clear A, toggle B
        let force_addr = if wide { 0x82 } else { b };
        let flag = if wide { 0x36 } else if a == 0x44 { 0x35 } else { 0x37 };
        cpu.write(flag, 7);
        cpu.write(force_addr, 0xc0 | if wide { 0 } else { 1 });
        assert_eq!(cpu.read(force_addr) & 0xc0, 0, "FOC strobes read zero");
        assert_ne!(cpu.read(base(pb.0)) & (1 << pb.1), 0, "FOCB forces toggle without waiting");
        assert_eq!(cpu.read(flag), 0, "forced matches do not set interrupt flags");
        cpu.write(a, if wide { 0xb0 } else { 0xb2 }); // force set B, then force clear B
        cpu.write(force_addr, 0x40 | if wide { 0 } else { 1 });
        cpu.write(a, if wide { 0xa0 } else { 0xa2 });
        cpu.write(force_addr, 0x40 | if wide { 0 } else { 1 });
        assert_eq!(cpu.read(base(pb.0)) & (1 << pb.1), 0, "clear changes an actually high OC latch");
        cpu.write(a, 0x51); // fixed phase PWM: COM=1 disconnects both outputs
        if wide { cpu.write(b, 1); }
        assert_eq!(cpu.read(base(pa.0)) & (1 << pa.1), 0);
        assert_eq!(cpu.read(base(pb.0)) & (1 << pb.1), 0);
    }
}

#[test]
fn arduino_startup_plain_stores_and_each_unmodeled_setting_has_a_precise_stop() {
    let mut a = Asm::new();
    for (addr, value) in [(0x7a, 0x87), (0x44, 3), (0x45, 3), (0x6e, 1),
        (0x80, 1), (0x81, 3), (0xb0, 1), (0xb1, 4), (0xc1, 0)] {
        a.emit(Ldi(16, value)); a.emit(Sts(addr, 16));
    }
    a.emit(Break);
    let mut cpu = Cpu::new(&a.finish().unwrap());
    assert_eq!(cpu.run(100), Stop::Break);
    assert_eq!(cpu.cycles, 28, "nine LDI/STS pairs plus BREAK");
    assert_eq!(cpu.read(0x7a), 0x87, "plain storage, no invented ADC conversion");

    let mut cases: Vec<(Vec<(u16, u8)>, u16, u8, &'static str)> = Vec::new();
    for cs in [6, 7] { for b in [0x45, 0x81] {
        cases.push((vec![], b, cs, "external Timer0/1 clock"));
    } }
    cases.push((vec![(0x80, 1), (0x81, 0x18)], 0x81, 0x19, "Timer1 WGM 13 (reserved)"));
    cases.push((vec![(0x81, 0x19)], 0x80, 1, "Timer1 WGM 13 (reserved)"));
    cases.push((vec![(0x80, 1), (0x81, 9)], 0x81, 0x19, "Timer1 WGM 13 (reserved)"));
    for (a, b) in [(0x44, 0x45), (0xb0, 0xb1)] { for low in [0, 2] {
        cases.push((vec![(a, low), (b, 8)], b, 9, "reserved 8-bit timer WGM"));
        cases.push((vec![(a, low), (b, 1)], b, 9, "reserved 8-bit timer WGM"));
        cases.push((vec![(a, 1), (b, 9)], a, low, "reserved 8-bit timer WGM"));
    } }
    // Either store can be second: ICIE1 in clocked WGM 1, or a clock
    // selection while ICIE1 is already set. WGM writes must recheck it too.
    cases.push((vec![(0x80, 1), (0x81, 1)], 0x6f, 0x20, "Timer1 input capture"));
    cases.push((vec![(0x80, 1), (0x6f, 0x20)], 0x81, 1, "Timer1 input capture"));
    cases.push((vec![(0x80, 2), (0x81, 0x11), (0x6f, 0x20)], 0x80, 1, "Timer1 input capture"));
    cases.push((vec![(0x81, 0x11), (0x6f, 0x20)], 0x81, 1, "Timer1 input capture"));
    cases.push((vec![], 0xb6, 0x20, "Timer2 asynchronous clock (AS2)"));
    for parity in [0x10, 0x20, 0x30] {
        cases.push((vec![(0xc1, 0x10)], 0xc2, 6 | parity, "USART0 receive parity"));
        cases.push((vec![(0xc2, 6 | parity)], 0xc1, 0x10, "USART0 receive parity"));
    }
    for size in 0..=3 {
        cases.push((vec![(0xc2, size << 1)], 0xc1, 0x14, "USART0 receive 9-bit/reserved character size"));
    }
    for (setup, addr, value, what) in cases {
        let mut cpu = Cpu::new(&program(&[Nop, Sts(addr, 16)]));
        for (addr, value) in setup { cpu.write(addr, value); }
        cpu.r[16] = value; cpu.step().unwrap();
        assert_eq!(cpu.step(), Err(Stop::Unmodeled { pc: 1, what }), "precise refusal for {what}");
        assert_eq!(cpu.cycles, 3, "NOP + STS: refusal at offending store completion");
    }
    // Stopped reserved modes, including ICIE1 outside ICR-TOP, may persist
    // across instructions. Each program exits that configuration before the
    // clock starts. Every LDI/STS pair is three clocks, BREAK is one.
    let mut stopped = vec![
        vec![(0x80, 1), (0x81, 0x18), (0x80, 2), (0x81, 0x19)],
        vec![(0x80, 1), (0x6f, 0x20), (0x81, 0), (0x6f, 0), (0x81, 1)],
        vec![(0x80, 1), (0x6f, 0x20), (0x81, 0x18), (0x80, 2), (0x81, 0x19)],
    ];
    for (a, b) in [(0x44, 0x45), (0xb0, 0xb1)] { for low in [0, 2] {
        stopped.push(vec![(a, low), (b, 8), (a, 1), (b, 9)]);
    } }
    for stores in stopped {
        let expected_cycles = if stores.len() == 4 { 13 } else { 16 };
        let mut a = Asm::new();
        for (addr, value) in stores { a.emit(Ldi(16, value)); a.emit(Sts(addr, 16)); }
        a.emit(Break);
        let mut cpu = Cpu::new(&a.finish().unwrap());
        assert_eq!(cpu.run(100), Stop::Break, "stopped unsupported configuration may precede a modeled running mode");
        assert_eq!(cpu.cycles, expected_cycles, "every stopped configuration store executes");
    }
    let mut external = Cpu::new(&program(&[Nop]));
    // AS2 also exercises the public bus's deferred fault contract.
    external.write(0xb6, 0x20);
    assert_eq!(external.run(1), Stop::Unmodeled { pc: 0, what: "Timer2 asynchronous clock (AS2)" });
    assert_eq!(external.cycles, 0);
}

/// A snapshot is a clone: re-running it with the same inputs repeats the registers, SRAM, I/O,
/// timers, USART and journal exactly, from a point inside a received frame and a PWM period.
/// The flash image is shared by the snapshot, not copied.
#[test]
fn snapshot_rerun_repeats_the_run_and_shares_flash() {
    let mut a = Asm::new();
    let isr = a.label();
    a.emit(Sleep); a.emit(Rjmp(-2));
    a.org(36); a.jump(Target::Rjmp, isr);
    a.org(64); a.bind(isr);
    a.emit(Lds(16, 0xc6)); a.emit(Sts(0xc6, 16)); a.emit(Sts(0x100, 16)); a.emit(Reti);
    let mut cpu = Cpu::new(&a.finish().unwrap());
    cpu.write(0xc0, 2); cpu.write(0xc4, 103); cpu.write(0xc1, 0x98);
    cpu.write(0x2a, 0x60); cpu.write(0x47, 100); cpu.write(0x48, 50);
    cpu.write(0x44, 0xb3); cpu.write(0x45, 2);
    cpu.sreg = 128;
    cpu.rx(b'A');
    assert_eq!(cpu.run(5000), Stop::Cycles);
    let snapshot = cpu.clone();
    assert!(std::ptr::eq(cpu.flash.as_ptr(), snapshot.flash.as_ptr()), "the snapshot shares the flash image");

    // Both echo B and then sleep with nothing left to wake them: `run` stops there.
    let finish = |mut cpu: Cpu| { cpu.rx(b'B'); let stop = cpu.run(30_000); (cpu, stop) };
    let ((run, stop), (rerun, restop)) = (finish(cpu), finish(snapshot.clone()));
    assert_eq!((stop, restop), (Stop::Sleep, Stop::Sleep), "both runs end asleep");
    let tx = run.events.iter().filter(|e| matches!(e, Event::Tx { .. })).count();
    assert_eq!(tx, 2, "the run echoed both bytes: the frame in flight at the snapshot completed");
    assert!(!edges(&run, 'D', 6).is_empty(), "the run drove PWM edges");
    assert_eq!((rerun.pc, rerun.cycles, rerun.sreg, rerun.sp), (run.pc, run.cycles, run.sreg, run.sp), "core state");
    assert_eq!(rerun.r, run.r, "registers");
    assert_eq!(rerun.sram, run.sram, "SRAM");
    assert_eq!(rerun.events, run.events, "journal: pin edges and USART frames at the same cycles");
    assert_eq!(rerun.rx_lost, run.rx_lost, "receiver losses");
    for addr in [0x29, 0x2a, 0x2b, 0x35, 0x44, 0x45, 0x46, 0x47, 0x48, 0xc0, 0xc1] {
        assert_eq!(rerun.read(addr), run.read(addr), "I/O register {addr:#x}");
    }
    assert_eq!(snapshot.cycles, 5000, "running the copies left the snapshot as it was");
}

/// `drive_gen` changes when a pin's drive may change (DDR, PORT, a timer's output) and not when
/// only an input level changes; `drive` reports DDR, PORT and the pin levels.
#[test]
fn drive_generation_follows_drive_not_inputs() {
    let mut cpu = Cpu::new(&program(&[Nop]));
    let g0 = cpu.drive_gen();
    cpu.set_pin('D', 2, true);
    assert_eq!(cpu.drive_gen(), g0, "an input level change is not a drive change");
    cpu.write(0x2a, 0x40);
    assert_ne!(cpu.drive_gen(), g0, "a DDR write is a drive change");
    assert_eq!(cpu.drive('D'), (0x40, 0x00, 0x04), "DDRD, PORTD and the levels (PD2 input high)");
    let g1 = cpu.drive_gen();
    cpu.write(0x47, 10); cpu.write(0x44, 0x83); cpu.write(0x45, 1);
    let g2 = cpu.drive_gen();
    assert_ne!(g2, g1, "a timer control write is a drive change");
    clocks(&mut cpu, 300);
    assert_ne!(cpu.drive_gen(), g2, "the timer's output edges on PD6 are drive changes");
}
