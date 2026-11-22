use super::*;
use super::model::{Model, PROFILE, ECHO_SOURCE, RX_BYTES};
use fab_elec_mcu_rt::{module, sched::{Device, Fault}};

pub(super) use super::model::SETTING;
const BYTE: u64 = 8320;
fn echo_bound(image: &Image) -> u32 {
    image.bounds.serial_byte + image.bounds.write + image.bounds.duty + 3 * 4 + 29 + 4
}

fn receiver(entry: usize, fifo: u32) -> SerialDef { SerialDef { entry, fifo, setting: SETTING } }

fn read_byte(a: &mut Asm, s: &Stubs, site: u16) {
    for r in 16..20 { a.emit(Ldi(r, 0)); }
    a.emit(Ldi(24, site as u8)); a.emit(Ldi(25, (site >> 8) as u8));
    a.jump(Target::Call, s.serial_byte);
}

fn echo(a: &mut Asm, s: &Stubs, sites: [u16; 3]) {
    read_byte(a, s, sites[0]);
    a.emit(Sts(s.state, 16));
    a.emit(Mov(20, 16));
    for r in 21..24 { a.emit(Ldi(r, 0)); }
    a.emit(Ldi(16, 13)); a.emit(Ldi(24, sites[1] as u8)); a.emit(Ldi(25, (sites[1] >> 8) as u8));
    for r in 17..20 { a.emit(Ldi(r, 0)); }
    a.jump(Target::Call, s.write);
    a.emit(Lds(20, s.state));
    for r in 21..24 { a.emit(Ldi(r, 0)); }
    a.emit(Ldi(16, 9)); a.emit(Ldi(24, sites[2] as u8)); a.emit(Ldi(25, (sites[2] >> 8) as u8));
    for r in 17..20 { a.emit(Ldi(r, 0)); }
    a.jump(Target::Call, s.duty);
}

fn image(entry: &dyn Fn(&mut Asm, &Stubs), fifo: u32, watch_tx: bool) -> Image {
    build(&Program { state_init: vec![0], serial: Some(receiver(0, fifo)), watch: if watch_tx { watch::Level::Ports } else { watch::Level::Off },
        pins: vec![PinDef { pin: 13, role: Role::Output },
            PinDef { pin: 9, role: Role::Pwm(pwm_setting(1, 255, 64)) }],
        entries: vec![entry], ..Program::default() }).unwrap()
}

// Finish a short, gapless burst with interrupts masked. The fourth start
// loses the waiting third frame; a fifth start loses the fourth as well.
// Restore the normal link speed before any watch transmission can begin.
fn overrun(cpu: &mut Cpu, image: &Image, last: u8) {
    assert_eq!(cpu.read(0xc0) & 0x80, 0, "overrun fixture starts with an empty hardware FIFO");
    cpu.sreg = 0;
    cpu.write(0xc4, 0); cpu.write(0xc5, 0); cpu.write(0xc0, 2);
    for byte in 1..=last { cpu.rx(byte); }
    let end = cpu.cycles + u64::from(last) * 80;
    pwm_idle_to(cpu, image, end);
    cpu.write(0xc4, SETTING.ubrr as u8); cpu.write(0xc5, (SETTING.ubrr >> 8) as u8);
    cpu.write(0xc0, u8::from(SETTING.u2x) << 1);
}

fn assert_uart(cpu: &Cpu, watch_tx: bool) {
    assert_eq!(cpu.read(0xc1) & 0x98, if watch_tx { 0x98 } else { 0x90 },
        "RXEN/RXCIE stay enabled; only an observed image enables TXEN");
    if !watch_tx { assert_eq!(cpu.read(0xc1) & 0x20, 0, "watch off must never enable UDRIE"); }
}

fn traced_until(cpu: &mut Cpu, image: &Image, t0: u64, trace: &mut Trace,
    watch_tx: bool, done: impl Fn(&Cpu, &Trace) -> bool) {
    let end = cpu.cycles + 24 * BYTE;
    while !done(cpu, trace) {
        assert!(cpu.cycles < end, "receive fixture exceeded its bounded work");
        trace.step(cpu, image, t0); assert_uart(cpu, watch_tx);
    }
}

// Measure the reception sample in the same image instead of assuming the
// save/status prefix stays forty clocks. No fixture state escapes this copy.
fn rx_sample_offset(cpu: &Cpu, image: &Image) -> u64 {
    let mut probe = cpu.clone(); probe.sreg = 0;
    let start = probe.cycles;
    invoke(&mut probe, image, image.symbols.rx).samples[0] - start
}

fn take_sample(trace: &Trace, take: usize) -> u64 {
    *trace.now_samples.iter().find(|&&at| at > trace.rx_takes[take]).expect("take must sample now48")
}

#[test]
fn rx_near_delay_and_reti_order() {
    let receive = |_: &mut Asm, _: &Stubs| {};
    let image = build(&Program { state_init: vec![0],
        pins: vec![PinDef { pin: 13, role: Role::Output }],
        serial: Some(receiver(1, 3)), entries: vec![&blink_entry, &receive],
        timers: vec![TimerDef { entry: 0, period_ticks: 2200, repeat: false }],
        ..Program::default() }).unwrap();
    // Fresh hardware per phase, five short near passes. Fast wire is
    // used solely to put a completion inside a chosen instruction window.
    // Placement: 0 inside delay, 1 RX alone at CALL, 2 RX and TOV at CALL.
    for (due, placement, count) in [(2200, 0, 1), (2295, 0, 1), (2296, 2, 1), (2200, 0, 2), (2200, 1, 1)] {
        let (mut cpu, t0) = booted(&image);
        let c = 8 * due - 2 * OVERFLOW - 64;
        clock_at(&mut cpu, &image, t0, c);
        store(&mut cpu, image.layout.timers, due, 6);
        cpu.write(0xc4, 0); cpu.write(0xc5, 0); cpu.write(0xc0, 2);
        cpu.pc = image.symbols.pass;
        let mut trace = Trace::default();
        if placement != 0 {
            // 80 wire clocks: finish five clocks before nominal CALL completion,
            // inside its final mask. For D=2296, TOV is pending there too.
            // At most two clocks of instruction-boundary rounding still fit.
            traced_until(&mut cpu, &image, t0, &mut trace, true,
                |cpu, _| cpu.cycles >= t0 + 8 * due + u64::from(DISPATCH) - 85);
        } else {
            traced_until(&mut cpu, &image, t0, &mut trace, true,
                |cpu, _| cpu.pc == image.symbols.delay);
        }
        let injected = cpu.cycles;
        let mut plain = cpu.clone();
        let mut reference = trace.clone();
        traced_until(&mut plain, &image, t0, &mut reference, true, |_, t| !t.effects.is_empty());
        for _ in 0..count { cpu.rx(0xa5); }
        traced_until(&mut cpu, &image, t0, &mut trace, true, |_, t| !t.effects.is_empty());
        let base = entry_start(t0, due);
        let start = trace.entries.iter().find(|e| e.0 == 0).unwrap().1;
        let rx: Vec<_> = trace.services.iter().filter(|s| s.vector == 36).collect();
        assert_eq!(rx.len(), count, "each wire byte must interrupt this prefix once");
        if placement == 0 {
            assert!(rx.iter().all(|s| s.interrupted.in_window(image.symbols.delay..image.symbols.delay_end)),
                "completions must interrupt the near delay");
            let mut expected = vec![1; count]; expected.push(0);
            assert_eq!(trace.entries.iter().map(|e| e.0).collect::<Vec<_>>(), expected,
                "earlier delay bytes must all dispatch before the committed timer");
            assert!(trace.rx_checks[0].1 != 0, "A must observe the delay arrival");
            assert!(trace.rx_defers.is_empty(), "A must defer without running B's rollback");
        } else if placement == 2 {
            assert_eq!(start, base, "pending RX after TOV must not delay the first handler instruction");
            let tov = trace.services.iter().rev().find(|s| s.vector == 32 && s.at < start).unwrap();
            assert_eq!(tov.interrupted.pc, image.symbols.entries[0], "TOV must interrupt at the entry boundary");
            let (first, words) = decode(&image.words[image.symbols.entries[0] as usize..]).unwrap();
            assert_eq!((rx[0].at, rx[0].interrupted.pc),
                (start + u64::from(cycles(&first).0), image.symbols.entries[0] + words as u32),
                "RETI must allow exactly the first handler instruction before RX");
        } else {
            assert_eq!(rx[0].interrupted.pc, image.symbols.entries[0], "RX alone at CALL is served before entry");
            let baseline = reference.entries[0].1;
            assert_eq!(start, baseline + service_clocks(&trace, injected, start)
                - service_clocks(&reference, injected, baseline), "entry shifts by observed services through RETI");
            assert_eq!(start - base, rx.iter().map(|s| u64::from(s.clocks)).sum::<u64>(),
                "this CALL phase crosses no extra overflow");
        }
        if placement != 0 {
            let effect = trace.effects[0];
            let baseline = reference.effects[0];
            assert_eq!(effect, baseline + service_clocks(&trace, injected, effect)
                - service_clocks(&reference, injected, baseline), "effect shifts by observed services through RETI");
        }
        assert_eq!(pins(&cpu), [(trace.effects[0], true)], "pin log agrees with the observed write_effect");
    }
}

#[test]
fn rx_configuration_isr_and_stub_contracts() {
    let empty = |_: &mut Asm, _: &Stubs| {};
    let base = image(&empty, 1, true);
    let available = 0x900 - u32::from(base.layout.rx_fifo + base.layout.stack_bytes) - 32;
    let capacity = available / 7;
    let max = image(&empty, capacity, true);
    assert!(capacity > 255, "this minimal image must exercise a two-byte FIFO count");
    assert_eq!(u32::from(max.layout.rx_depth), capacity, "FIFO must use the RAM-derived capacity, including indices above 255");
    let (mut large_cpu, large_t0) = booted(&max);
    store(&mut large_cpu, max.layout.rx_count, u64::from(capacity - 1), 2);
    let last = max.layout.rx_fifo + 7 * (max.layout.rx_depth - 1);
    store(&mut large_cpu, max.layout.rx_head, u64::from(last), 2);
    for full in [false, true] {
        large_cpu.sreg = 0; large_cpu.rx(0x91);
        let end = large_cpu.cycles + BYTE;
        pwm_idle_to(&mut large_cpu, &max, end);
        clock_at(&mut large_cpu, &max, large_t0, 100); large_cpu.sreg = 0;
        let call = invoke(&mut large_cpu, &max, max.symbols.rx);
        within("rx_int two-byte capacity", call.clocks + 7, max.bounds.rx_int);
        assert_eq!(load(&large_cpu, max.layout.rx_count, 2), u64::from(capacity), "count must neither truncate nor exceed capacity");
        assert_eq!(load(&large_cpu, max.layout.rx_head, 2), u64::from(max.layout.rx_fifo), "large FIFO pointer wraps exactly once");
        assert_eq!(load(&large_cpu, max.layout.rx_lost, 4), u64::from(full), "only the byte after capacity is lost");
        assert_eq!(large_cpu.read(last), 0x91, "last slot receives the accepted byte");
    }
    let error = build(&Program { serial: Some(receiver(0, capacity + 1)),
        state_init: vec![0], pins: vec![PinDef { pin: 13, role: Role::Output },
            PinDef { pin: 9, role: Role::Pwm(pwm_setting(1, 255, 64)) }],
        entries: vec![&empty], ..Program::default() }).err().unwrap();
    assert!(error.contains(&((capacity + 1) * 7).to_string()) && error.contains(&available.to_string()),
        "RAM refusal must report required and available bytes: {error}");
    for (ubrr, u2x) in [(Some(16), None), (None, Some(true))] {
        let error = build(&Program { serial: Some(receiver(0, 1)), entries: vec![&empty], ubrr, u2x,
            ..Program::default() }).err().unwrap();
        assert!(error.contains("conflicts") && error.contains("51"), "conflicting watch baud: {error}");
    }
    let mut cpu = Cpu::new(&base.words);
    let mut enabled = false;
    while cpu.pc != base.symbols.t0 {
        assert!(cpu.cycles < 30_000, "reset must terminate");
        let uart_write = matches!(decode(&cpu.flash[cpu.pc as usize..]), Some((Sts(0xc1, _), _)));
        cpu.step().unwrap();
        enabled |= cpu.read(0xc1) != 0;
        if enabled || uart_write { assert_uart(&cpu, true); }
    }
    let t0 = cpu.cycles;
    assert_eq!((cpu.read(0xc4), cpu.read(0xc5), cpu.read(0xc0) & 2), (51, 0, 0), "serial setting owns both directions");
    assert_eq!(SETTING.byte_cycles(10), BYTE, "19200's actual wire byte time");
    // Isolated ISR measurements retain real wire reception, then establish
    // an exact clock phase. RXEN stays set, so clock_at must not flush UDR.
    for (phase, full, dor, lost, prior_overrun) in [(100, false, false, 0, false),
        (2030, false, false, 0, false), (2047, false, false, 0, false),
        (100, true, false, 0, false), (100, true, false, u32::MAX, false),
        ((1u64 << 32) * 8 - 16, false, false, 0, false), (100, false, false, u32::MAX, false),
        (100, false, true, 0, false), (100, true, true, 0, false), (100, false, true, u32::MAX, false),
        (100, true, true, 0x00ff_fffe, false), (100, true, true, u32::MAX - 2, false),
        (100, true, true, u32::MAX - 1, false), (100, true, true, u32::MAX, false),
        (100, false, false, 1, true), (100, true, false, 1, true)] {
        if dor {
            let before = cpu.rx_lost;
            overrun(&mut cpu, &base, 4);
            assert_eq!(cpu.rx_lost - before, 1, "four-frame fixture loses exactly one hardware byte");
            assert_eq!([cpu.read(0xc6), cpu.read(0xc6)], [1, 2], "leave the tagged fourth frame for the ISR");
            assert_eq!(cpu.read(0xc0) & 8, 8, "DOR0 must belong to the byte the ISR will read");
        } else {
            cpu.sreg = 0; cpu.rx(0xa7);
            let end = t0.max(cpu.cycles) + BYTE;
            pwm_idle_to(&mut cpu, &base, end);
        }
        clock_at(&mut cpu, &base, t0, phase); cpu.sreg = 0;
        store(&mut cpu, base.layout.rx_count, u64::from(full), 2);
        store(&mut cpu, base.layout.rx_lost, u64::from(lost), 4);
        cpu.write(base.layout.rx_overrun, u8::from(prior_overrun));
        store(&mut cpu, base.layout.rx_head, u64::from(base.layout.rx_fifo), 2);
        let payload: Vec<_> = (0..7).map(|i| cpu.read(base.layout.rx_fifo + i)).collect();
        let tail = load(&cpu, base.layout.rx_tail, 2);
        let regs = cpu.r; let status = cpu.sreg;
        let call = invoke(&mut cpu, &base, base.symbols.rx);
        within(&format!("rx_int phase={phase} full={full} dor={dor} lost={lost}"), call.clocks + 7, base.bounds.rx_int);
        assert_eq!(cpu.r, regs, "RX must preserve every handler/delay register");
        assert_eq!(cpu.sreg, status | 128, "RX restores flags and RETI enables interrupts");
        assert_eq!(load(&cpu, base.layout.rx_lost, 4), u64::from(lost.saturating_add(u32::from(dor) + u32::from(full))),
            "count DOR0 and FIFO drop once each, saturating their sum: full={full}, dor={dor}, lost={lost}");
        assert_eq!(cpu.read(base.layout.rx_overrun), u8::from(prior_overrun || dor),
            "DOR0 sets the flag even at saturation; later untagged bytes cannot clear it");
        assert_eq!(load(&cpu, base.layout.rx_count, 2), 1, "accept fills the slot and drop leaves it full");
        assert_eq!(load(&cpu, base.layout.rx_tail, 2), tail, "RX never moves the consumer's tail");
        assert_eq!(load(&cpu, base.layout.rx_head, 2), u64::from(base.layout.rx_fifo), "accept wraps and drop leaves the head alone");
        if full {
            assert_eq!((0..7).map(|i| cpu.read(base.layout.rx_fifo + i)).collect::<Vec<_>>(), payload,
                "drop must preserve the oldest payload and timestamp, including DOR0 + full");
        } else {
            assert_eq!(cpu.read(base.layout.rx_fifo), if dor { 4 } else { 0xa7 }, "DOR0 must not discard the valid UDR byte");
            assert_eq!(load(&cpu, base.layout.rx_fifo + 1, 6), (call.samples[0] - t0) / 8,
                "RX timestamp must use the corrected reception sample, including high carries");
        }
    }
    for active in [false, true] {
        for link in [0u32, 1, 256, u32::MAX] {
            clock_at(&mut cpu, &base, t0, 100); cpu.write(0x6e, 0);
            cpu.write(base.layout.rx_active, u8::from(active)); cpu.write(base.layout.rx_byte, 255);
            cpu.r[16..20].copy_from_slice(&link.to_le_bytes()); cpu.r[24] = 0x34; cpu.r[25] = 0x12;
            cpu.write(base.layout.head, 0); cpu.write(base.layout.tail, 0); store(&mut cpu, base.layout.lost, 0, 4);
            let kept = preserved(&cpu);
            let call = invoke(&mut cpu, &base, base.symbols.serial_byte);
            within("serial_byte", call.clocks, base.bounds.serial_byte);
            assert_eq!(preserved(&cpu), kept, "serial_byte preserves reserved registers");
            let got = u32::from_le_bytes(cpu.r[16..20].try_into().unwrap());
            assert_eq!(got, if active && link == 0 { 255 } else { u32::MAX }, "serial_byte context/link contract");
            if !active || link != 0 {
                assert_eq!(cpu.read(base.layout.ring + 5), watch::FAULT_SERIAL, "invalid context must fault");
                assert_eq!(load(&cpu, base.layout.ring + 6, 2), 0x1234, "serial fault site");
                assert_eq!(load(&cpu, base.layout.ring + 9, 4), u64::from(link), "serial fault retains the whole link argument");
            } else { assert_eq!(cpu.read(base.layout.head), 0, "valid serial_byte must not queue a fault"); }
        }
    }
    let absent = build(&Program::default()).unwrap();
    let (mut cpu, t0) = booted(&absent);
    clock_at(&mut cpu, &absent, t0, 100); cpu.write(0x6e, 0);
    cpu.r[16..20].fill(0);
    let call = invoke(&mut cpu, &absent, absent.symbols.serial_byte);
    within("serial_byte undeclared link", call.clocks, absent.bounds.serial_byte);
    assert_eq!(&cpu.r[16..20], &[255; 4], "undeclared serial returns -1");
    assert_eq!(cpu.read(absent.layout.ring + 5), watch::FAULT_SERIAL, "undeclared serial faults");
}

#[test]
fn rx_hardware_overrun_and_fifo_loss_report_at_next_handler() {
    let receive = |a: &mut Asm, s: &Stubs| { read_byte(a, s, 1); a.emit(Sts(s.state, 16)); };
    let images = [image(&receive, 3, true), image(&receive, 2, true)];
    for (last, index) in [(4, 0), (4, 1), (5, 0)] {
        let image = &images[index];
        let (mut cpu, t0) = booted(image);
        clock_at(&mut cpu, image, t0, 100);
        overrun(&mut cpu, image, last);
        assert_eq!(cpu.rx_lost, u64::from(last - 3), "four frames lose one; five frames lose two");
        let full = image.layout.rx_depth == 2;
        let reported = 1 + u64::from(full);
        // Run each ISR in isolation before starting any byte handler. The
        // first pop transfers the waiting last frame, with DOR0, behind 2.
        for (n, byte) in [1, 2, last].into_iter().enumerate() {
            let dor = n == 2;
            assert_eq!(cpu.read(0xc0) & 8, if dor { 8 } else { 0 }, "only the last kept hardware frame carries DOR0");
            cpu.sreg = 0;
            let call = invoke(&mut cpu, image, image.symbols.rx);
            within("rx_int buffered overrun", call.clocks + 7, image.bounds.rx_int);
            assert_eq!(load(&cpu, image.layout.rx_lost, 4), if dor { reported } else { 0 },
                "loss first appears when the tagged frame is read, plus its independent FIFO drop");
            assert_eq!(cpu.read(image.layout.rx_overrun), u8::from(dor), "DOR0 marks the count as a lower bound");
            if !dor || !full {
                assert_eq!(cpu.read(image.layout.rx_fifo + n as u16 * 7), byte,
                    "ISR keeps hardware bytes 1,2,last in order, unless the software FIFO is full");
            }
        }
        assert_eq!(cpu.read(0xc0) & 0x88, 0, "all three surviving hardware frames were consumed");
        assert_eq!(cpu.rx_lost, u64::from(last - 3), "reading buffered frames cannot add hardware losses");
        let kept = if full { vec![1, 2] } else { vec![1, 2, last] };
        assert_eq!(load(&cpu, image.layout.rx_count, 2), kept.len() as u64, "only the full FIFO drops a surviving frame");
        cpu.pc = image.symbols.pass;
        let mut trace = Trace::default();
        traced_until(&mut cpu, image, t0, &mut trace, true,
            |cpu, t| t.entries.len() == 1 && cpu.pc == image.symbols.pass);
        assert_eq!((load(&cpu, image.layout.rx_report, 4), cpu.read(image.layout.rx_report_overrun)), (reported, 1),
            "the first handler captures count and overrun together");
        assert_eq!((load(&cpu, image.layout.rx_lost, 4), cpu.read(image.layout.rx_overrun)), (0, 0),
            "the same take resets both live loss fields");
        traced_until(&mut cpu, image, t0, &mut trace, true,
            |cpu, t| t.entries.len() == kept.len() && cpu.pc == image.symbols.pass);
        assert_eq!(load(&cpu, image.layout.rx_lost, 4), 0, "the next byte handler clears the reported loss");
        assert_eq!(load(&cpu, image.layout.rx_count, 2), 0, "all kept bytes reach a handler");
        assert_eq!(cpu.read(image.layout.state), *kept.last().unwrap(), "the final handler reads the last kept byte");
        drain(&mut cpu, image);
        let got = frames(&cpu);
        let received: Vec<_> = got.iter().filter_map(|(f, _)| match f.event {
            watch::Event::Rx { byte } => Some(byte), _ => None,
        }).collect();
        assert_eq!(received, kept, "watch reports exactly the surviving software FIFO bytes in order");
        let faults: Vec<_> = got.iter().filter_map(|(f, _)| match f.event {
            watch::Event::Fault { code, site, arg }
                if matches!(code, watch::FAULT_SERIAL_LOST | watch::FAULT_SERIAL_LOST_AT_LEAST) => Some((code, site, arg as u32)),
            _ => None,
        }).collect();
        assert_eq!(faults, [(watch::FAULT_SERIAL_LOST_AT_LEAST, 0, reported as u32)],
            "report the lower bound once: one DOR0 indication plus FIFO drops, even when two hardware losses coalesce");
        let loss_at = got.iter().position(|(f, _)| matches!(f.event,
            watch::Event::Fault { code: watch::FAULT_SERIAL_LOST_AT_LEAST, .. })).unwrap();
        assert!(matches!(got[loss_at - 1].0.event, watch::Event::Rx { byte: 1 }),
            "SerialLost belongs to the first handler starting after the tagged frame's ISR");
        assert_eq!(got[loss_at].1, (take_sample(&trace, 0) - t0) / 8,
            "SerialLost uses that handler's atomic start sample");
        if last == 5 {
            // Same CPU after the at-least report: service each new wire byte
            // before sending the next, while delaying handlers to fill the
            // software FIFO. drain left the link at 80 clocks per frame.
            for n in 0..=image.layout.rx_depth {
                cpu.sreg = 0; cpu.rx(10 + n as u8);
                let end = cpu.cycles + 80;
                pwm_idle_to(&mut cpu, image, end);
                assert_eq!(cpu.read(0xc0) & 8, 0, "later overflow is software-only");
                invoke(&mut cpu, image, image.symbols.rx);
            }
            assert_eq!(cpu.rx_lost, 2, "later bytes must not add hardware losses");
            assert_eq!((load(&cpu, image.layout.rx_lost, 4), cpu.read(image.layout.rx_overrun)), (1, 0),
                "one later FIFO drop must not inherit the earlier hardware flag");
            cpu.pc = image.symbols.pass;
            let mut exact = Trace { deferred: true, ..Trace::default() }; // Last isolated RETI.
            traced_until(&mut cpu, image, t0, &mut exact, true,
                |cpu, t| t.entries.len() == usize::from(image.layout.rx_depth) && cpu.pc == image.symbols.pass);
            drain(&mut cpu, image);
            let got = frames(&cpu);
            let faults: Vec<_> = got.iter().filter_map(|(f, _)| match f.event {
                watch::Event::Fault { code, site, arg }
                    if matches!(code, watch::FAULT_SERIAL_LOST | watch::FAULT_SERIAL_LOST_AT_LEAST) => Some((code, site, arg as u32)),
                _ => None,
            }).collect();
            assert_eq!(faults, [(watch::FAULT_SERIAL_LOST_AT_LEAST, 0, 1), (watch::FAULT_SERIAL_LOST, 0, 1)],
                "reset the overrun flag: a later FIFO-only report must say exact");
        }
    }
}

#[test]
fn rx_fifo_drop_newest_loss_reset_and_timer_tie() {
    let busy = |a: &mut Asm, s: &Stubs| {
        read_byte(a, s, 1);
        a.emit(Sts(s.state, 16));
        let done = a.label();
        a.emit(Cpi(16, 10)); a.jump(Target::Brbc(1), done);
        // Only the first handler stalls: all six wire frames complete before
        // it returns, leaving two pending bytes and exactly three drops.
        let q = (5 * BYTE / 4) as u16;
        a.emit(Ldi(24, q as u8)); a.emit(Ldi(25, (q >> 8) as u8));
        let again = a.label(); a.bind(again);
        a.emit(Sbiw(24, 1)); a.jump(Target::Brbc(1), again);
        a.bind(done);
    };
    let image = image(&busy, 2, true);
    let (mut cpu, t0) = booted(&image);
    for byte in [10, 20, 30, 40, 50, 60] { cpu.rx(byte); }
    let mut trace = Trace::default();
    traced_until(&mut cpu, &image, t0, &mut trace, true,
        |cpu, t| t.entries.len() == 3 && cpu.pc == image.symbols.pass);
    assert_eq!(trace.rx_samples.len(), 6, "every wire byte must reach the RX ISR, including drops");
    assert_eq!(cpu.rx_lost, 0, "this is runtime FIFO overflow, not hardware overrun");
    assert_eq!(cpu.read(image.layout.state), 30, "oldest queued bytes must survive");
    assert_eq!(load(&cpu, image.layout.rx_count, 2), 0, "handlers remove their byte at dispatch");
    assert_eq!(load(&cpu, image.layout.rx_lost, 4), 0, "next byte handler clears the reported count");
    cpu.rx(70);
    traced_until(&mut cpu, &image, t0, &mut trace, true,
        |cpu, t| t.entries.len() == 4 && cpu.pc == image.symbols.pass);
    drain(&mut cpu, &image); assert_uart(&cpu, true);
    let got = frames(&cpu);
    let received: Vec<_> = got.iter().filter_map(|(f, tick)| match f.event {
        watch::Event::Rx { byte } => Some((byte, *tick)), _ => None,
    }).collect();
    assert_eq!(received, [(10, trace.rx_samples[0]), (20, trace.rx_samples[1]),
        (30, trace.rx_samples[2]), (70, trace.rx_samples[6])], "FIFO order and original reception stamps");
    let faults: Vec<_> = got.iter().filter_map(|(f, _)| match f.event {
        watch::Event::Fault { code: watch::FAULT_SERIAL_LOST, site, arg } => Some((site, arg as u32)), _ => None,
    }).collect();
    assert_eq!(faults, [(0, 3)], "three newest bytes dropped, reported once at the next handler");
    let loss_at = got.iter().position(|(f, _)| matches!(f.event,
        watch::Event::Fault { code: watch::FAULT_SERIAL_LOST, .. })).unwrap();
    assert!(matches!(got[loss_at - 1].0.event, watch::Event::Rx { byte: 20 }), "loss belongs to the next started byte handler");
    assert_eq!(got[loss_at].1, (take_sample(&trace, 1) - t0) / 8,
        "SerialLost must use the next dispatch's atomic start sample, not reception time");

    let receive = |a: &mut Asm, s: &Stubs| { read_byte(a, s, 7); a.emit(Sts(s.state, 16)); };
    let tie = build(&Program { state_init: vec![0],
        pins: vec![PinDef { pin: 13, role: Role::Output }],
        serial: Some(receiver(1, 2)), entries: vec![&blink_entry, &receive],
        timers: vec![TimerDef { entry: 0, period_ticks: 50_000, repeat: false }],
        ..Program::default() }).unwrap();
    let (mut cpu, t0) = booted(&tie);
    // Receive in an idle instruction loop before allowing the scheduler to
    // scan. Give the timer the exact six-byte tick stored by the real ISR.
    clock_at(&mut cpu, &tie, t0, 100);
    let idle = scratch(&mut cpu, &tie);
    cpu.flash[idle as usize] = encode(&Rjmp(-1)).unwrap().0[0]; cpu.pc = idle;
    cpu.rx(0x7e);
    let mut trace = Trace::default();
    traced_until(&mut cpu, &tie, t0, &mut trace, true,
        |cpu, t| !t.rx_samples.is_empty() && cpu.sp == tie.layout.stack_top);
    store(&mut cpu, tie.layout.timers, trace.rx_samples[0], 6);
    cpu.pc = tie.symbols.pass;
    traced_until(&mut cpu, &tie, t0, &mut trace, true,
        |cpu, t| t.entries.len() == 2 && cpu.pc == tie.symbols.pass);
    assert_eq!(trace.entries.iter().map(|e| e.0).collect::<Vec<_>>(), [0, 1], "timer wins an equal-tick byte");
    assert_eq!(cpu.read(tie.layout.state), 0x7e, "timer must not consume the tied byte");
    drain(&mut cpu, &tie);
    assert!(frames(&cpu).iter().any(|(f, t)| matches!(f.event, watch::Event::Rx { byte: 0x7e })
        && *t == trace.rx_samples[0]), "dispatch must not replace the reception tick");
}

/// The fixture has at most `queued` earlier byte handlers, and `arrivals`
/// total RX services. Arrival may miss a scan: include the remainder of an
/// empty/far pass, its suffix and longest idle recheck, one overflow wake
/// (plus four wake clocks), one foreground encode and its next idle check.
/// Count the encode's empty pump and six short pumps around far's SEI/SLEEP,
/// the RX fast check, ready's SEI/JMP and pass entry before UDRIE is disabled.
/// Each byte turn includes its redispatch bound, its entire bounded body, and
/// the three-clock jump back. Solve the conservative overflow insertion
/// bound using ceil(length/2048)+1 for any starting phase. This bounds all
/// outputs through the last body port, including queued predecessors.
fn replay_bound(image: &Image, queued: u64, arrivals: u64, body: u64) -> u64 {
    let b = &image.bounds;
    let base = u64::from(b.pass_max() + b.rx_scan + b.far + 2 + 2 * b.idle_check + b.encode + 7 * b.udre) + OVERFLOW + 4
        + (queued + 1) * (u64::from(b.rx_redispatch()) + body + 3)
        + arrivals * u64::from(b.rx_int);
    let mut bound = base;
    loop {
        let next = base + (bound.div_ceil(OVERFLOW) + 1) * u64::from(TOV_INT);
        if next == bound { return bound; }
        assert!(next > bound, "replay bound iteration must increase until stable");
        bound = next;
    }
}

#[test]
fn rx_wire_and_device_replay() {
    let source = ECHO_SOURCE;
    let profile = PROFILE;
    let compiled = fab_elec_mcu::compile(source, &profile, &[("D13", 13), ("D9", 9)], &[]).unwrap();
    let site = |line| compiled.sites.iter().position(|s| s.line == line).unwrap() as u16;
    let sites = [site(4), site(5), site(6)];
    let entry = |a: &mut Asm, s: &Stubs| echo(a, s, sites);
    let image = build(&Program { state_init: vec![0], serial: Some(SerialDef {
        entry: 0, setting: compiled.serial[0].setting, fifo: compiled.serial[0].fifo }),
        pins: vec![PinDef { pin: 13, role: Role::Output }, PinDef { pin: 9, role: Role::Pwm(compiled.pwm[0].setting) }],
        entries: vec![&entry], ..Program::default() }).unwrap();
    let (mut cpu, t0) = booted(&image);
    let bytes = RX_BYTES;
    // Cpu::rx queues a gapless 8N1 wire: starts at t0, then every 8320 cycles.
    for byte in bytes { cpu.rx(byte); }
    let mut trace = Trace::default();
    traced_until(&mut cpu, &image, t0, &mut trace, true,
        |cpu, t| t.entries.len() == bytes.len() && cpu.pc == image.symbols.pass);
    assert_eq!(trace.rx_samples.len(), bytes.len(), "one sample per wire byte");
    assert_eq!(cpu.rx_lost, 0, "19200 baud must not overrun the hardware receiver");
    for (k, tick) in trace.rx_samples.iter().enumerate() {
        assert!(*tick * 8 >= (k as u64 + 1) * BYTE, "RX cannot precede wire completion");
        assert!(*tick * 8 < (k as u64 + 1) * BYTE + u64::from(image.bounds.udre + TX_PUBLISH + LOCK + image.bounds.rx_int + TOV_INT),
            "reception sample must stay within bounded masking/priority latency");
    }
    drain(&mut cpu, &image); assert_uart(&cpu, true);
    let got = frames(&cpu);
    let received: Vec<_> = got.iter().filter_map(|(f, tick)| match f.event {
        watch::Event::Rx { byte } => Some((*tick, byte)), _ => None,
    }).collect();
    assert_eq!(received, trace.rx_samples.iter().copied().zip(bytes).collect::<Vec<_>>(),
        "wire frames must retain bytes, order and the ISR's reception ticks");
    let mut device = Device::new(module::load(&compiled.module, &profile).unwrap(), &profile);
    let mut model = Model::default(); device.boot(&mut model);
    for &(tick, byte) in &received {
        device.run_until(tick * 500, &mut model);
        device.rx(tick * 500, 0, byte, &mut model);
    }
    device.run_until(u64::MAX, &mut model);
    assert!(model.faults.is_empty(), "replayed serial program must not fault: {:?}", model.faults);
    let outputs: Vec<_> = got.iter().filter_map(|(f, tick)|
        matches!(f.event, watch::Event::Write { .. } | watch::Event::Duty { .. }).then_some((*tick, f.event))).collect();
    assert_eq!(outputs.len(), model.outputs.len(), "same number of output operations");
    // The watch-state grid independently checks the complete body bound.
    let bound = replay_bound(&image, u64::from(image.layout.rx_depth), bytes.len() as u64, u64::from(echo_bound(&image)));
    for ((tick, actual), (ns, expected)) in outputs.iter().zip(&model.outputs) {
        assert_eq!(actual, expected, "replay must preserve Write/Duty values, order and sites");
        let chip_ns = *tick * 500;
        assert!(chip_ns >= *ns && (chip_ns - ns) * 2 <= bound * 125,
            "dispatch bound {bound} cycles: chip={chip_ns}ns model={ns}ns event={actual:?}");
    }
    println!("RX replay: {} bytes, {} output events, {bound} cycle bound", bytes.len(), outputs.len());
}

#[test]
fn rx_cut_publication_and_comparison() {
    let empty = |_: &mut Asm, _: &Stubs| {};
    let image = image(&empty, 1, true);
    let (mut cpu, t0) = booted(&image);
    // Compare across each byte carry, including both bytes above 32 bits.
    // Accepted/dropped arms, equal stamps and sticky flags all fit the bound.
    // A rejected byte must not set the early flag.
    for bit in [8, 16, 24, 32, 40] {
        let stamp = 1u64 << bit;
        for (delta, full, prior) in [(-1i64, false, 0), (0, false, 0), (1, false, 0),
            (1, true, 0), (-1, false, 255), (1, false, 255)] {
            cpu.sreg = 0;
            cpu.write(0xc4, 0); cpu.write(0xc5, 0); cpu.write(0xc0, 2);
            cpu.rx(7);
            let end = cpu.cycles + 80;
            pwm_idle_to(&mut cpu, &image, end);
            let sample_offset = rx_sample_offset(&cpu, &image);
            clock_at(&mut cpu, &image, t0, 8 * stamp - sample_offset); cpu.sreg = 0;
            store(&mut cpu, image.layout.rx_cut, (stamp as i64 + delta) as u64, 6);
            cpu.write(image.layout.rx_early, prior);
            store(&mut cpu, image.layout.rx_count, u64::from(full), 2);
            store(&mut cpu, image.layout.rx_head, u64::from(image.layout.rx_fifo), 2);
            let call = invoke(&mut cpu, &image, image.symbols.rx);
            within("rx_int comparison/full/sticky", call.clocks + 7, image.bounds.rx_int);
            assert_eq!(cpu.read(image.layout.rx_early), if prior != 0 || (!full && delta > 0) { 255 } else { 0 },
                "strict 48-bit comparison, accepted bytes only, sticky until scan: bit={bit} delta={delta} full={full}");
            if !full { assert_eq!(load(&cpu, image.layout.rx_fifo + 1, 6), stamp, "comparison preserves the timestamp"); }
        }
    }

    let (mut cpu, t0) = booted(&image);
    clock_at(&mut cpu, &image, t0, 4000);
    cpu.write(0xc4, 0); cpu.write(0xc5, 0); cpu.write(0xc0, 2);
    for r in 6..12 { cpu.r[r] = (1000u64 >> (8 * (r - 6))) as u8; }
    cpu.r[12] = 0;
    let mut probe = cpu.clone(); probe.pc = image.symbols.rx_scan; probe.sreg = 128;
    let start = probe.cycles;
    while !matches!(decode(&probe.flash[probe.pc as usize..]), Some((Sts(addr, _), _)) if addr == image.layout.rx_cut) {
        assert!(probe.cycles - start < u64::from(image.bounds.rx_scan_mask), "scan must reach cut publication");
        probe.step().unwrap();
    }
    let to_cut = probe.cycles - start;
    assert!(to_cut < 80, "fast wire must be long enough to place a completion at the cut");
    cpu.rx(9);
    let end = cpu.cycles + 80 - to_cut;
    pwm_idle_to(&mut cpu, &image, end);
    // Complete at the first cut store, after the FIFO snapshot. A stale cut
    // would miss it if service could split snapshot and publication.
    cpu.write(image.layout.rx_early, 255);
    for pass in 0..2 {
        for r in 6..12 { cpu.r[r] = (1000u64 >> (8 * (r - 6))) as u8; }
        cpu.r[12] = 0; cpu.pc = image.symbols.rx_scan; cpu.sreg = 128;
        let start = cpu.cycles;
        cpu.step().unwrap(); // CLI, before the first frame is complete.
        while cpu.sreg & 128 == 0 {
            assert!(cpu.cycles - start < u64::from(image.bounds.rx_scan_mask),
                "rx_scan_mask: measured {} cycles must finish within bound {}", cpu.cycles - start, image.bounds.rx_scan_mask);
            cpu.step().unwrap();
        }
        within("rx_scan_mask", cpu.cycles - start, image.bounds.rx_scan_mask);
        within("rx_scan including active reset", cpu.cycles - start + 2, image.bounds.rx_scan);
        assert_eq!(cpu.read(image.layout.rx_early), 0, "each scan clears the old flag before enabling interrupts");
        if pass == 0 {
            assert_eq!(load(&cpu, image.layout.rx_cut, 6), 1000, "empty FIFO publishes the timer's due");
            assert_eq!(cpu.read(0xc0) & 0x80, 0x80, "wire completion must be pending inside the publication mask");
            let mut trace = Trace { deferred: true, ..Trace::default() }; // Just executed SEI.
            traced_until(&mut cpu, &image, t0, &mut trace, true,
                |cpu, t| !t.rx_samples.is_empty() && cpu.sp == image.layout.stack_top);
            assert_eq!(cpu.read(image.layout.rx_early), 255, "deferred RX service must see the new cut");
        } else {
            assert_eq!(load(&cpu, image.layout.rx_cut, 6), load(&cpu, image.layout.rx_fifo + 1, 6),
                "the next scan publishes the queued byte's own stamp");
            assert_eq!(cpu.r[12], 255, "the earlier byte wins the rescan");
        }
    }
}

fn timer_output_bound(image: &Image) -> u32 { image.bounds.write + image.bounds.duty + 2 * (10 + 4) + 4 }

fn timer_output(a: &mut Asm, s: &Stubs, sites: [u16; 2]) {
    for r in 20..24 { a.emit(Ldi(r, 0)); }
    a.emit(Ldi(16, 13)); a.emit(Ldi(24, sites[0] as u8)); a.emit(Ldi(25, (sites[0] >> 8) as u8));
    for r in 17..20 { a.emit(Ldi(r, 0)); }
    a.jump(Target::Call, s.write);
    duty_call(a, s, 9, 77, sites[1]);
}

// From the earlier of byte stamp/timer due through both complete bodies:
// at most one remaining near pass/delay and defer, one byte redispatch,
// one timer redispatch (including an overrun), both RET/JMPs and one RX.
// These fixtures never enable UDRE until drain. Bound all overflow insertions
// independently of phase with ceil(length/2048)+1, as in replay_bound.
fn ordered_replay_bound(image: &Image) -> u64 {
    let b = &image.bounds;
    let base = 8 * u64::from(image.layout.w)
        + u64::from(b.pass_max() + b.rx_scan + RX_DEFER + b.rx_redispatch()
            + echo_bound(image) + 3 + b.redispatch() + b.rx_scan + b.overrun
            + timer_output_bound(image) + 3 + b.rx_int + 8);
    let mut bound = base;
    loop {
        let next = base + (bound.div_ceil(OVERFLOW) + 1) * u64::from(TOV_INT);
        if next == bound { return bound; }
        assert!(next > bound, "ordered replay bound must converge monotonically");
        bound = next;
    }
}

#[test]
fn rx_committed_timer_replay() {
    let profile = PROFILE;
    for (repeat, due, baud) in [(false, 6000u64, 19200), (true, 6000, 19200), (true, 100, 500000)] {
        let source = format!("let out = output(D13)\nlet motor = pwm(D9, hz(490))\nlet link = serial({baud}, fn() {{\n    let value = byte(link)\n    write(out, if value != 0 {{ high }} else {{ low }})\n    duty(motor, value)\n}})\nlet timer = {}(us({}), fn() {{\n    write(out, low)\n    duty(motor, 77)\n}})\n",
            if repeat { "every" } else { "after" }, due / 2);
        let compiled = fab_elec_mcu::compile(&source, &profile, &[("D13", 13), ("D9", 9)], &[]).unwrap();
        let site = |line| compiled.sites.iter().position(|s| s.line == line).unwrap() as u16;
        let receive = |a: &mut Asm, s: &Stubs| echo(a, s, [site(4), site(5), site(6)]);
        let timer = |a: &mut Asm, s: &Stubs| timer_output(a, s, [site(9), site(10)]);
        let image = build(&Program { state_init: vec![0], serial: Some(SerialDef {
            entry: 0, setting: compiled.serial[0].setting, fifo: compiled.serial[0].fifo }),
            pins: vec![PinDef { pin: 13, role: Role::Output }, PinDef { pin: 9, role: Role::Pwm(compiled.pwm[0].setting) }],
            timers: vec![TimerDef { entry: 1, period_ticks: due, repeat }],
            entries: vec![&receive, &timer], ..Program::default() }).unwrap();
        let bound = ordered_replay_bound(&image);
        let (mut measured, epoch) = booted(&image);
        clock_at(&mut measured, &image, epoch, 100); measured.write(0x6e, 0);
        within("timer output body through RET", invoke(&mut measured, &image, image.symbols.entries[1]).clocks,
            timer_output_bound(&image));
        // 0: near delay (or the short-period boot counterexample),
        // 1: due-1 after A, 2: due exactly, 3: due+1, 4: dispatch window.
        for placement in 0..if due == 100 { 1 } else { 5 } {
            let (mut cpu, t0) = booted(&image);
            let mut trace = Trace::default();
            if due != 100 {
                clock_at(&mut cpu, &image, t0, 8 * due - 2 * OVERFLOW - 64);
                cpu.pc = image.symbols.pass;
                cpu.write(0xc4, 0); cpu.write(0xc5, 0); cpu.write(0xc0, 2);
                if placement == 0 {
                    traced_until(&mut cpu, &image, t0, &mut trace, true, |cpu, _| cpu.pc == image.symbols.delay);
                } else {
                    // Wire completion plus the measured IRQ/vector/sample prefix.
                    // Start one clock into the target tick, leaving six clocks
                    // for instruction-boundary rounding. No TOV edge is near.
                    let sample_offset = rx_sample_offset(&cpu, &image);
                    let at = if placement == 4 { 8 * due + u64::from(DISPATCH) - 85 }
                        else { 8 * (due + placement - 2) + 1 - (80 + 7 + sample_offset) };
                    traced_until(&mut cpu, &image, t0, &mut trace, true, |cpu, _| cpu.cycles >= t0 + at);
                }
            } else {
                assert_eq!((cpu.read(0xc4), cpu.read(0xc0) & 2), (1, 0), "counterexample uses the real 500000-baud setting");
            }
            cpu.rx(7);
            let early = placement <= 1;
            if placement == 1 {
                traced_until(&mut cpu, &image, t0, &mut trace, true,
                    |cpu, t| cpu.pc == image.symbols.rx_defer || !t.entries.is_empty());
                assert_eq!(cpu.pc, image.symbols.rx_defer, "B must defer before entering either handler");
                assert_eq!(trace.rx_checks[0].1, 0, "this arrival must be after A's read");
                assert!(trace.services.iter().find(|s| s.vector == 36).unwrap().at > trace.rx_checks[0].0,
                    "B fixture must start RX service after A");
                assert!(trace.services.iter().find(|s| s.vector == 36).unwrap().interrupted
                    .in_window(image.symbols.bookkeep..image.symbols.dispatch[0]),
                    "B's arrival must interrupt bookkeeping");
                assert_eq!(load(&cpu, image.layout.timers, 6), if repeat { 2 * due } else { DISARMED },
                    "B must see the already-advanced/disarmed timer");
                let start = trace.dispatches.last().unwrap().1;
                let mut enabled_at = None;
                while cpu.pc != image.symbols.pass {
                    assert!(cpu.cycles - start < u64::from(RX_DEFER), "B must return within its counted path");
                    if cpu.sreg & 128 != 0 && enabled_at.is_none() { enabled_at = Some(cpu.cycles); }
                    trace.step(&mut cpu, &image, t0);
                }
                within("rx_defer mask through SEI", enabled_at.unwrap() - start, RX_DEFER_MASK);
                within("rx_defer through pass", cpu.cycles - start, RX_DEFER);
                assert_eq!(cpu.sp, image.layout.stack_top, "B must not leave a handler CALL frame");
                assert_eq!(load(&cpu, image.layout.timers, 6), due, "B must restore the entire published due");
            }
            if early {
                traced_until(&mut cpu, &image, t0, &mut trace, true,
                    |cpu, t| cpu.pc == image.symbols.rx_take || !t.entries.is_empty());
                assert_eq!(cpu.pc, image.symbols.rx_take, "the byte must be taken before any timer body");
                assert_eq!((load(&cpu, image.layout.timers, 6), load(&cpu, image.layout.timers + 6, 6)), (due, due),
                    "defer preserves the timer's original due and period");
                assert_eq!(cpu.read(image.layout.head), 0, "no overrun record or other watch record may precede the byte");
                if placement == 0 {
                    assert!(trace.rx_checks[0].1 != 0 && trace.rx_defers.is_empty(), "A must defer without B's rollback");
                }
            }
            traced_until(&mut cpu, &image, t0, &mut trace, true,
                |cpu, t| t.entries.len() == 2 && cpu.pc == image.symbols.pass);
            assert_eq!(trace.entries.iter().map(|e| e.0).collect::<Vec<_>>(), if early { vec![0, 1] } else { vec![1, 0] },
                "event order, one byte and one timer: repeat={repeat} placement={placement}");
            assert_eq!(trace.rx_samples.len(), 1, "one wire byte must have one timestamp");
            let stamp = trace.rx_samples[0];
            if (1..=3).contains(&placement) {
                assert_eq!(stamp, due + placement - 2, "fixture must hit the exact boundary tick");
            } else if early { assert!(stamp < due, "A fixture must receive before due"); }
            else { assert!(stamp >= due, "dispatch-window service must stamp at due or later"); }
            assert!(trace.dispatches.iter().filter(|e| e.0 == 0).all(|e| e.2 == 0),
                "byte windows must see a cleared flag and cannot defer their own stamp");
            assert_eq!(trace.rx_defers.len(), usize::from(placement == 1), "only the B fixture rolls bookkeeping back");
            assert_eq!(cpu.rx_lost, 0, "no hardware frame is lost");
            assert!(!cpu.events.iter().any(|e| matches!(e, Event::Tx { .. })), "bound excludes UDRE before drain");
            let lock = trace.locks.iter().rev().find(|l| l.selected == 0).unwrap();
            let mut next = due + due; let mut missed = 0u32;
            while next <= lock.tick { next += due; missed += 1; }
            assert_eq!(lock.due, due, "the actual timer dispatch uses the original due");
            assert_eq!(load(&cpu, image.layout.timers, 6), if repeat { next } else { DISARMED },
                "one-shot stays disarmed and repeating timer stays on its original grid");
            if due != 100 { assert_eq!(missed, 0, "long-period replay does not coalesce another firing"); }
            // Timing assertions above use the cycle-counting oracle.
            // Draining below runs no scheduler passes or program handlers.
            drain(&mut cpu, &image);
            let got = frames(&cpu);
            let rx_at = got.iter().position(|(f, _)| matches!(f.event, watch::Event::Rx { byte: 7 })).unwrap();
            assert_eq!(got[rx_at].1, stamp, "Rx frame retains the reception stamp through defer");
            let overruns: Vec<_> = got.iter().enumerate().filter_map(|(i, (f, _))| match f.event {
                watch::Event::Fault { code: watch::FAULT_OVERRUN, site: 0, arg } => Some((i, arg as u32)), _ => None,
            }).collect();
            if due == 100 {
                assert!(missed > 0, "counterexample must reach the late overrun arm");
                assert_eq!(overruns.len(), 1, "exactly one overrun report after deferral");
                assert!(overruns[0].0 > rx_at, "no overrun may be queued before the byte's Rx record");
                assert_eq!(overruns[0].1, missed, "missed count matches sched.rs's grid loop at the observed lock tick");
            } else { assert!(overruns.is_empty(), "near replay must not invent an overrun"); }

            let mut device = Device::new(module::load(&compiled.module, &profile).unwrap(), &profile);
            let mut model = Model { setting: Some(compiled.serial[0].setting), ..Model::default() };
            device.boot(&mut model);
            let mut ran = Vec::new();
            while device.next_start().is_some_and(|t| t < stamp * 500) { ran.push(device.step(&mut model).unwrap()); }
            device.rx(stamp * 500, 0, 7, &mut model);
            while ran.len() < 2 { ran.push(device.step(&mut model).unwrap()); }
            assert!(model.faults.iter().all(|f| matches!(f, Fault::Overrun { .. })),
                "compiled replay has no pin/serial/loop faults; overrun clocks are compared separately");
            assert_eq!(ran.iter().map(|r| r.rx.is_some()).collect::<Vec<_>>(), if early { vec![true, false] } else { vec![false, true] },
                "compiled scheduler agrees on strict tick order and timer ties");
            let outputs: Vec<_> = got.iter().filter_map(|(f, tick)|
                matches!(f.event, watch::Event::Write { .. } | watch::Event::Duty { .. }).then_some((*tick, f.event))).collect();
            assert_eq!(outputs.len(), model.outputs.len(), "no handler output is lost or duplicated");
            for ((tick, actual), (ns, expected)) in outputs.iter().zip(&model.outputs) {
                assert_eq!(actual, expected, "compiled replay preserves Write/Duty values, sites and order");
                let chip_ns = tick * 500;
                assert!(chip_ns >= *ns && (chip_ns - ns) * 2 <= bound * 125,
                    "ordered replay bound {bound} cycles: chip={chip_ns}ns model={ns}ns");
            }
        }
    }
}

#[test]
fn rx_dispatch_windows_without_serial_are_unchanged() {
    let image = make_image(6000, false, &blink_entry);
    for ((&start, &call), &entry) in image.symbols.dispatch.iter().zip(&image.symbols.dispatch_call).zip(&image.symbols.entries) {
        let mut pc = start;
        let mut insns = Vec::new();
        while pc <= call {
            let (insn, words) = decode(&image.words[pc as usize..]).unwrap();
            insns.push(insn);
            pc += words as u32;
        }
        let [Bclr(7), Insn::Call(pad), Bset(7), Insn::Call(target)] = insns[..] else {
            panic!("nonserial dispatch must retain CLI/CALL sled/SEI/CALL entry: {insns:?}");
        };
        assert!((image.symbols.pad..image.symbols.pad_end).contains(&pad), "padding must call the shared sled");
        assert_eq!(target, entry, "dispatch must call its mapped entry");
        let (mut cpu, _) = booted(&image);
        cpu.write(0x6e, 0); cpu.write(0xc1, 0); cpu.sreg = 128; cpu.pc = start;
        let began = cpu.cycles;
        let mut masked = 0;
        while cpu.pc != entry {
            assert!(cpu.cycles - began < u64::from(L), "dispatch must reach its entry within L");
            assert!(WindowPc::capture(&cpu, &image).in_window(start..pc), "dispatch stays in its window or sled");
            let (before, enabled) = (cpu.cycles, cpu.sreg & 128 != 0);
            cpu.step().unwrap();
            if !enabled { masked += cpu.cycles - before; }
        }
        assert_eq!(cpu.cycles - began, u64::from(L), "CLI through entry CALL stays exactly L");
        assert_eq!(masked, u64::from(L - 5), "I-clear clocks include the sled and SEI, exclude CLI and entry CALL");
        assert_eq!(cpu.sp, image.layout.stack_top - 2, "only the entry return remains on the stack");
    }
}

#[test]
fn watch_tx_off_and_bounds() {
    let receive = |a: &mut Asm, s: &Stubs| echo(a, s, [1, 2, 3]);
    let timer = |a: &mut Asm, s: &Stubs| {
        blink_entry(a, s); duty_call(a, s, 9, 77, 4); duty_call(a, s, 12, 77, 5);
    };
    for watch in [watch::Level::Off, watch::Level::Faults, watch::Level::Ports] {
        let watch_tx = watch != watch::Level::Off;
        let image = build(&Program { watch, serial: Some(receiver(0, 3)), state_init: vec![0],
            pins: vec![PinDef { pin: 13, role: Role::Output },
                PinDef { pin: 9, role: Role::Pwm(pwm_setting(1, 255, 64)) }],
            timers: vec![TimerDef { entry: 1, period_ticks: 5000, repeat: true }],
            entries: vec![&receive, &timer], ..Program::default() }).unwrap();
        let mut cpu = Cpu::new(&image.words);
        let mut enabled = false;
        while cpu.pc != image.symbols.t0 {
            assert!(cpu.cycles < 30_000, "reset must complete");
            if !watch_tx { assert_unobserved_uart(&cpu, true); }
            let uart_write = matches!(decode(&cpu.flash[cpu.pc as usize..]), Some((Sts(0xc1, _), _)));
            cpu.step().unwrap();
            enabled |= cpu.read(0xc1) != 0;
            if enabled || uart_write { assert_uart(&cpu, watch_tx); }
            if !watch_tx { assert!(cpu.events.iter().all(|e| !matches!(e, Event::Tx { .. })), "no boot TX"); }
        }
        let t0 = cpu.cycles;
        if watch_tx { assert_eq!(cpu.read(image.layout.hello_pending), 1, "watch enables Hello"); }
        else { assert_unobserved_image(&image); }
        for byte in [0, 100, 255] { cpu.rx(byte); }
        let mut trace = Trace::default();
        traced_until(&mut cpu, &image, t0, &mut trace, watch_tx,
            |cpu, t| t.entries.len() == 4 && cpu.pc == image.symbols.pass);
        assert_eq!(trace.rx_samples.len(), 3, "watch off must still receive");
        assert_eq!(load(&cpu, image.layout.rx_count, 2), 0, "all received handlers must finish");
        assert!(!pins(&cpu).is_empty(), "outputs must work without watch");
        // Exercise the empty/far suffix as well as reset and near dispatch.
        traced_until(&mut cpu, &image, t0, &mut trace, watch_tx,
            |cpu, _| cpu.pc == image.symbols.sleep);
        assert_uart(&cpu, watch_tx);
        if watch_tx {
            drain(&mut cpu, &image); assert_uart(&cpu, true);
            let got = frames(&cpu);
            assert!(got.iter().any(|(f, _)| matches!(f.event, watch::Event::Rx { .. })), "RX frames");
            assert_eq!(got.iter().any(|(f, _)| matches!(f.event, watch::Event::Duty { .. })),
                watch == watch::Level::Ports, "only ports records Duty frames");
            assert!(got.iter().any(|(f, _)| matches!(f.event, watch::Event::Fault { code: watch::FAULT_PIN, .. })), "fault path exercised");
        } else {
            assert!(cpu.events.iter().all(|e| !matches!(e, Event::Tx { .. })), "watch off must produce no TX event at all");
            assert_unobserved_image(&image);
            assert_eq!(fault_cell(&cpu, &image, watch::FAULT_PIN), (1, 5), "the timer's bad duty counts at its site");
        }
        // The same hardware actions may see different drain interference.
        // Check uninterrupted stub/pass bounds with peripheral IRQs off.
        let b = &image.bounds;
        println!("RX {watch:?}: int={}, scan={}, scan_mask={}, take={}, bookkeep={}, W={} (one timer)",
            b.rx_int, b.rx_scan, b.rx_scan_mask, b.rx_take, b.rx_bookkeep, image.layout.w);
        for invalid in [false, true] {
            for (name, target, bound) in [("write", image.symbols.write, b.write), ("read", image.symbols.read, b.read),
                ("duty", image.symbols.duty, b.duty), ("clock_us", image.symbols.clock_us, b.clock_us),
                ("clock_ms", image.symbols.clock_ms, b.clock_ms), ("timer_stop", image.symbols.timer_stop, b.timer_stop),
                ("timer_start", image.symbols.timer_start, b.timer_start), ("timer_period", image.symbols.timer_period, b.timer_period),
                ("serial_byte", image.symbols.serial_byte, b.serial_byte), ("push", image.symbols.push, b.push)] {
                if target == 0 { assert_eq!(bound, 0, "unlinked {name}"); continue; }
                clock_at(&mut cpu, &image, t0, 100); cpu.write(0x6e, 0);
                cpu.r[16..20].fill(0);
                cpu.r[16] = if invalid { 255 } else if target == image.symbols.duty { 9 }
                    else if target == image.symbols.read || target == image.symbols.write { 13 } else { 0 };
                cpu.r[20..24].copy_from_slice(&100u32.to_le_bytes()); cpu.r[24] = 4; cpu.r[25] = 0;
                cpu.write(image.layout.rx_active, 1); cpu.write(image.layout.rx_byte, 99);
                if watch_tx {
                    cpu.write(image.layout.head, 0); cpu.write(image.layout.tail, 0);
                    store(&mut cpu, image.layout.lost, 0, 4);
                    cpu.write(image.layout.record_count, 1); cpu.write(image.layout.record, watch::RX);
                }
                let call = invoke(&mut cpu, &image, target);
                within(&format!("{name} invalid={invalid} watch_tx={watch_tx}"), call.clocks, bound);
                assert_uart(&cpu, watch_tx);
            }
        }
        for wrap in [false, true] {
        for (byte_event, overrun, lost, at_least) in [(false, false, 0, false), (true, false, 0, false),
            (true, false, 3, false), (true, false, 3, true),
            (true, false, u64::from(u32::MAX), false), (true, false, u64::from(u32::MAX), true),
            (false, true, 0, false)] {
            if wrap && !byte_event { continue; }
            clock_at(&mut cpu, &image, t0, 100); cpu.write(0x6e, 0);
            if watch_tx {
                cpu.write(image.layout.head, 0); cpu.write(image.layout.tail, 0);
                cpu.write(image.layout.tx_len, 0); cpu.write(image.layout.tx_pos, 0);
                cpu.write(image.layout.hello_pending, 0); store(&mut cpu, image.layout.lost, 0, 4);
            }
            for cell in &image.layout.fault_cells { store(&mut cpu, cell.addr as u16, 0, 4); }
            store(&mut cpu, image.layout.rx_count, u64::from(byte_event), 2);
            store(&mut cpu, image.layout.rx_lost, lost, 4);
            cpu.write(image.layout.rx_overrun, u8::from(at_least));
            let tail = image.layout.rx_fifo + if wrap { 7 * (image.layout.rx_depth - 1) } else { 0 };
            let head = if wrap { image.layout.rx_fifo } else { tail + 7 };
            store(&mut cpu, image.layout.rx_tail, u64::from(tail), 2);
            store(&mut cpu, image.layout.rx_head, u64::from(head), 2);
            cpu.write(tail, 99); store(&mut cpu, tail + 1, 0, 6);
            store(&mut cpu, image.layout.timers, if byte_event { DISARMED } else { 0 }, 6);
            store(&mut cpu, image.layout.timers + 6, if overrun { 1 } else { 5000 }, 6);
            cpu.pc = image.symbols.pass;
            let start = cpu.cycles;
            if byte_event {
                to_symbol(&mut cpu, image.symbols.bookkeep, 20_000);
                let bookkeeping = cpu.cycles;
                let seed = cpu.clone();
                to_symbol(&mut cpu, image.symbols.rx_take, 20_000);
                let take = cpu.cycles;
                cpu.step().unwrap(); // Execute the take's CLI.
                while cpu.sreg & 128 == 0 {
                    assert!(cpu.cycles - take < u64::from(b.rx_take),
                        "rx_take: measured {} cycles must re-enable interrupts within bound {}", cpu.cycles - take, b.rx_take);
                    cpu.step().unwrap();
                }
                within(&format!("rx_take watch={watch:?} wrap={wrap} lost={lost} at_least={at_least}"),
                    cpu.cycles - take, b.rx_take);
                assert_eq!((load(&cpu, image.layout.rx_report, 4), cpu.read(image.layout.rx_report_overrun)),
                    (lost, u8::from(at_least)), "the take captures both fields before enabling interrupts");
                assert_eq!((load(&cpu, image.layout.rx_lost, 4), cpu.read(image.layout.rx_overrun)), (0, 0),
                    "the take resets both fields before enabling interrupts");
                assert_eq!(load(&cpu, image.layout.rx_tail, 2), u64::from(head), "take advances or wraps the tail");
                to_symbol(&mut cpu, image.symbols.dispatch[0], 20_000);
                within(&format!("rx_bookkeep watch={watch:?} wrap={wrap} lost={lost} at_least={at_least}"),
                    cpu.cycles - bookkeeping, b.rx_bookkeep);
                if watch_tx {
                    // Reuse this gate state for wrapped Lost appends, refusal
                    // at saturation, and the longer increment-to-saturation arm.
                    for ring in 0..3 {
                        let mut probe = seed.clone();
                        probe.write(image.layout.head, image.layout.records - 1);
                        probe.write(image.layout.tail, if ring == 0 { image.layout.records - 1 } else { 0 });
                        store(&mut probe, image.layout.lost, if ring == 0 { 7 }
                            else if ring == 1 { u64::from(u32::MAX) } else { 0x7fff_ffff }, 4);
                        to_symbol(&mut probe, image.symbols.dispatch[0], 20_000);
                        within(&format!("rx_bookkeep watch={watch:?} wrap={wrap} lost={lost} at_least={at_least} ring={ring}"),
                            probe.cycles - bookkeeping, b.rx_bookkeep);
                    }
                }
            }
            to_symbol(&mut cpu, image.symbols.entries[usize::from(!byte_event)], 20_000);
            let bound = if byte_event { b.rx_redispatch() }
                else { b.redispatch() + b.rx_scan + if overrun { b.overrun } else { 0 } };
            within(&format!("late pass byte={byte_event} overrun={overrun} watch_tx={watch_tx}"), cpu.cycles - start, bound);
            if watch_tx && byte_event && lost != 0 {
                assert_eq!(cpu.read(image.layout.ring + 8 + 5),
                    if at_least { watch::FAULT_SERIAL_LOST_AT_LEAST } else { watch::FAULT_SERIAL_LOST },
                    "the queued fault uses the captured flag");
            }
            if !watch_tx {
                for cell in &image.layout.fault_cells {
                    let code = if byte_event && lost != 0 {
                        Some(if at_least { watch::FAULT_SERIAL_LOST_AT_LEAST } else { watch::FAULT_SERIAL_LOST })
                    } else if overrun { Some(watch::FAULT_OVERRUN) } else { None };
                    assert_eq!(fault_cell(&cpu, &image, cell.code),
                        (u16::from(Some(cell.code) == code), 0), "only the selected fault counts; timer/link zero is its site");
                }
                if overrun {
                    let now = load(&cpu, image.layout.stamp, 6);
                    assert_eq!(load(&cpu, image.layout.timers, 6), now + 1,
                        "Off still divides and advances a one-tick repeat past now");
                }
            }
            // CALL has entered the body, but its first instruction has not
            // run. Remove that call frame before the next isolated fixture.
            cpu.sp = image.layout.stack_top;
        }
        }
        clock_at(&mut cpu, &image, t0, 100); cpu.write(0x6e, 0);
        store(&mut cpu, image.layout.timers, DISARMED, 6);
        store(&mut cpu, image.layout.rx_count, 0, 2);
        if watch_tx {
            store(&mut cpu, image.layout.lost, 0, 4);
            for addr in [image.layout.head, image.layout.tail, image.layout.hello_pending,
                image.layout.tx_pos, image.layout.tx_len] { cpu.write(addr, 0); }
        }
        cpu.pc = image.symbols.pass;
        let start = cpu.cycles;
        to_symbol(&mut cpu, image.symbols.sleep, 20_000);
        within("empty/far pass through SEI", cpu.cycles - start, b.pass_prefix + b.rx_scan + 2 + b.far - 1);
        assert_uart(&cpu, watch_tx);
        clock_at(&mut cpu, &image, t0, 100); cpu.write(0x6e, 0);
        cpu.write(image.layout.rx_active, 1); cpu.write(image.layout.rx_byte, 5);
        let body = invoke(&mut cpu, &image, image.symbols.entries[0]);
        within("echo body through RET", body.clocks, echo_bound(&image));
        if !watch_tx {
            assert!(cpu.events.iter().all(|e| !matches!(e, Event::Tx { .. })), "isolated stubs must not transmit either");
            assert_unobserved_image(&image);
            assert_eq!(load(&cpu, image.layout.state, 1), 5, "byte() still returns the active receiver byte");
        }
    }
}
