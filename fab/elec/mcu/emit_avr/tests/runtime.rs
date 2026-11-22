use std::collections::{BTreeMap, BTreeSet};

use fab_elec_mcu_avr::asm::{Asm, Target};
use fab_elec_mcu_avr::insn::{cycles, decode, encode, Insn};
use fab_elec_mcu_avr::sim::{Cpu, Event, FLASH_WORDS};
use fab_elec_mcu_emit_avr::image::{blink_entry, DISARMED};
use fab_elec_mcu_emit_avr::runtime::*;
use fab_elec_mcu_emit_avr::{build, Image, PinDef, Program, Role, SerialDef, Stubs, TimerDef};
use fab_elec_mcu_rt::{pwm::{self, PwmMode, PwmSetting}, watch};
use Insn::*;

#[path = "support/oracle.rs"]
mod oracle;
use oracle::*;
use oracle::Call;

#[path = "support/model.rs"]
mod model;

#[path = "support/rx.rs"]
mod rx;

const OVERFLOW: u64 = 2048;

#[test]
fn adc_conversion_sample_boundary_and_bounded_pin_stub() {
    use fab_elec_mcu_rt::ports;
    for (f_clk, div) in [(100_000, 2), (400_001, 4), (800_001, 8), (1_600_001, 16),
        (3_200_001, 32), (6_400_001, 64), (16_000_000, 128)] {
        let image = build(&Program { f_clk, ports: 1 << ports::ADC_READ, watch: watch::Level::Off,
            pins: vec![PinDef { pin: 14, role: Role::Analog }, PinDef { pin: 16, role: Role::Analog },
                PinDef { pin: 20, role: Role::Analog }, PinDef { pin: 21, role: Role::Analog },
                PinDef { pin: 13, role: Role::Output }], ..Program::default() }).unwrap();
        let (boot, _) = booted(&image);
        assert_eq!(boot.read(0x7e), 0b000101, "DIDR0 disables only declared PC0/PC2 digital buffers");
        assert_eq!(boot.read(0x7a), 0, "the ADC stays off at boot");
        assert_eq!(boot.read(0x64) & 1, 0, "boot leaves PRADC clear");
        let mut cpu = boot.clone(); cpu.set_avcc(5.0); cpu.set_analog(0, 1.25);
        let preserved = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 21, 22, 26, 27, 28, 29];
        for &r in &preserved { if r != 2 { cpu.r[r] = 0xa0 + r as u8; } }
        let saved = cpu.r;
        cpu.r[20] = 0x40;
        let call = invoke(&mut cpu, &image, image.symbols.adc_convert);
        assert_eq!(call.clocks, u64::from(image.bounds.adc_convert), "clk/{div}: converter trace matches its exact total");
        assert_eq!(image.bounds.adc_convert, 21 + div / 2 + 25 * div, "clk/{div}: smallest eligible divisor and first-conversion hand count");
        assert_eq!(image.bounds.adc_sample, 7 + 14 * div, "clk/{div}: first-conversion sample offset");
        assert_eq!(&cpu.r[16..20], &[0, 1, 0, 0], "clk/{div}: ADCL/ADCH return a zero-extended ten-bit value");
        assert_eq!(cpu.read(0x7a), 0, "clk/{div}: conversion clears ADIF and turns the ADC off");
        for &r in &preserved {
            assert_eq!(cpu.r[r], saved[r], "clk/{div}: converter preserves r{r} outside its clobber set");
        }
        for pin in [14i32, 16, 20, 21, 0, 13, 19, 22, 270, -1, i32::MIN] {
            let mut cpu = boot.clone(); cpu.set_avcc(5.0);
            for channel in [0, 2, 6, 7] { cpu.set_analog(channel, 3.75); }
            cpu.r[16..20].copy_from_slice(&pin.to_le_bytes());
            cpu.r[24..26].copy_from_slice(&0x1234u16.to_le_bytes());
            let saved = cpu.r[2..16].to_vec();
            let call = invoke(&mut cpu, &image, image.symbols.adc);
            within(&format!("adc clk/{div}, pin {pin}"), call.clocks, image.bounds.adc);
            let expected = if [14, 16, 20, 21].contains(&pin) { 768u32 } else { 0 };
            assert_eq!(&cpu.r[16..20], &expected.to_le_bytes(), "clk/{div}, pin {pin}: declared analog pins alone convert");
            assert_eq!(&cpu.r[2..16], saved, "clk/{div}, pin {pin}: stub preserves runtime registers");
            assert_eq!(cpu.read(0x7a), 0, "clk/{div}, pin {pin}: stub leaves the ADC off");
        }
        if f_clk != 16_000_000 { continue; }
        let mut cpu = boot.clone(); cpu.sreg = 128;
        cpu.set_avcc(5.0); cpu.set_analog(0, 1.25); cpu.r[20] = 0x40;
        let call = invoke(&mut cpu, &image, image.symbols.adc_convert);
        assert!(call.services > 0, "Timer0 interrupts the conversion wait");
        assert_eq!(call.clocks, u64::from(image.bounds.adc_convert) + call.services as u64 * u64::from(TOV_INT),
            "interrupt service only lengthens the uninterrupted conversion budget");
        assert_eq!(&cpu.r[16..20], &[0, 1, 0, 0], "interrupted conversion still reads the completed ADC code");
        assert_eq!(cpu.read(0x7a), 0, "interrupted conversion also shuts the ADC down");
        for delta in [-1i64, 0, 1] {
            let mut cpu = boot.clone(); cpu.set_avcc(5.0); cpu.set_analog(0, 1.25);
            let nop = scratch(&mut cpu, &image);
            let end = nop + 1;
            return_to(&mut cpu, end); cpu.pc = image.symbols.adc_convert; cpu.r[20] = 0x40;
            let start = cpu.cycles;
            let change = (start as i64 + i64::from(image.bounds.adc_sample) + delta) as u64;
            let mut inserted = 0;
            while cpu.cycles < change {
                let (insn, _) = decode(&cpu.flash[cpu.pc as usize..]).unwrap();
                let (base, extra) = cycles(&insn);
                if cpu.cycles + u64::from(base + extra) > change {
                    // Include the taken loop branch (delay's skips cover single words too).
                    // The wait's multi-cycle instructions can straddle the requested instant.
                    // A one-cycle fixture NOP reaches it without shifting the running ADC phase.
                    let pc = cpu.pc; cpu.pc = nop; cpu.step().unwrap(); cpu.pc = pc;
                    inserted += 1;
                } else { cpu.step().unwrap(); }
            }
            assert_eq!(cpu.cycles, change, "sample {delta:+}: voltage changes at the requested clock");
            cpu.set_analog(0, 3.75);
            to_symbol(&mut cpu, end, u64::from(image.bounds.adc_convert));
            assert_eq!(cpu.cycles - start, u64::from(image.bounds.adc_convert) + inserted,
                "sample {delta:+}: trace counts the routine plus only the fixture's exact-clock NOPs");
            let expected = if delta < 0 { 768u32 } else { 256 };
            assert_eq!(&cpu.r[16..20], &expected.to_le_bytes(), "sample {delta:+}: only changes before sample-and-hold affect the code");
            assert_eq!(cpu.read(0x7a), 0, "sample {delta:+}: ADC is disabled after the read");
        }
    }
}

/// Reach an awake foreground boundary without waiting for the next due/epoch.
/// A far wake may return straight to SLEEP instead of entering another pass.
fn to_idle_boundary(cpu: &mut Cpu, image: &Image, limit: u64) {
    let end = cpu.cycles + limit;
    loop {
        if cpu.will_execute() && (cpu.pc == image.symbols.pass
            || matches!(decode(&cpu.flash[cpu.pc as usize..]), Some((Sleep, _)))) { return; }
        assert!(cpu.cycles < end, "idle boundary must be reached within the fixture budget");
        cpu.step().expect("the idle check must not fault");
    }
}

/// Drains through the firmware's own far passes (the timers disarmed), unlike
/// `drain`, which runs the encoder and pump without passes: each pass's agent
/// moves a pending Lost count into the ring once two records are free, and
/// the foreground encodes it. Ends with the ring, count and shifter idle.
fn drain_passes(cpu: &mut Cpu, image: &Image) {
    let l = &image.layout;
    cpu.write(0xc4, 0); cpu.write(0xc5, 0); cpu.write(0xc0, 0x42);
    let frame = u64::from(image.bounds.encode + image.bounds.udre) + watch::MAX_WIRE as u64 * (u64::from(image.bounds.udre) + 80);
    let end = cpu.cycles + (u64::from(l.records) + 2) * (frame + 2 * OVERFLOW);
    loop {
        // Judge only at an awake foreground boundary: mid-encode the ring can
        // read empty while its frame is not yet in the shifter. An empty
        // drain now resleeps until the next epoch instead of running a pass.
        run(cpu, 256);
        to_idle_boundary(cpu, image, 4 * OVERFLOW + u64::from(image.bounds.pass_max()));
        let shifting = matches!(cpu.events.iter().rev().find(|e| matches!(e, Event::Tx { .. } | Event::TxDone { .. })),
            Some(Event::Tx { .. }));
        if cpu.read(l.head) == cpu.read(l.tail) && load(cpu, l.lost, 4) == 0 && cpu.read(l.hello_pending) == 0
            && cpu.read(l.tx_pos) == cpu.read(l.tx_len) && !shifting { break; }
        assert!(cpu.cycles < end, "the passes must send every queued record and the Lost count");
    }
}

fn make_image(period: u64, repeat: bool, entry: &dyn Fn(&mut Asm, &Stubs)) -> Image {
    build(&Program {
        image_id: 0x1234_5678, state_init: vec![0],
        pins: vec![PinDef { pin: 13, role: Role::Output }],
        timers: vec![TimerDef { entry: 0, period_ticks: period, repeat }],
        entries: vec![entry], ..Program::default()
    }).expect("fixture image must fit")
}

fn pins(cpu: &Cpu) -> Vec<(u64, bool)> {
    cpu.events.iter().filter_map(|event| match *event {
        Event::Pin { cycle, port: 'B', bit: 5, high } => Some((cycle, high)), _ => None,
    }).collect()
}

/// Place the bus at an exact clock phase for isolated routine measurements.
/// This is fixture initialization, not firmware code and not elapsed execution.
fn clock_at(cpu: &mut Cpu, image: &Image, t0: u64, elapsed: u64) {
    let tick = elapsed / 8;
    cpu.sreg = 0;
    if image.layout.records != 0 || image.layout.rx_depth != 0 {
        cpu.write(0xc1, if image.layout.records == 0 { 0x90 } else if image.layout.rx_depth == 0 { 8 } else { 0x98 });
    }
    cpu.write(0x43, 0x83);
    cpu.write(0x44, 3); cpu.write(0x45, 2); cpu.write(0x46, tick as u8);
    cpu.write(0x35, 7); cpu.write(0x6e, 1);
    cpu.r[2] = 0;
    for r in 13..16 { cpu.r[r] = (tick >> (8 * (r - 12))) as u8; }
    store(cpu, image.layout.clock_hi, tick >> 32, 2);
    cpu.write(image.layout.clock_b31, cpu.r[15] & 0x80);
    cpu.cycles = t0 + elapsed / 8 * 8;
    cpu.write(0x43, 0);
    let pc = scratch(cpu, image);
    for _ in 0..elapsed % 8 { cpu.pc = pc; cpu.step().unwrap(); }
    cpu.pc = pc; cpu.sreg = 128;
}

#[derive(Clone, Default)]
struct Trace {
    deferred: bool,
    entries: Vec<(usize, u64)>,
    services: Vec<Service>,
    service: Option<usize>,
    rx_samples: Vec<u64>,
    rx_takes: Vec<u64>,
    now_samples: Vec<u64>,
    rx_checks: Vec<(u64, u8)>,
    rx_defers: Vec<u64>,
    dispatches: Vec<(usize, u64, u8)>,
    samples: Vec<u64>,
    effects: Vec<u64>,
    owed: Vec<u8>,
    locks: Vec<LockSample>,
    lock_starts: Vec<u64>,
    pass: u64,
    edge_pcs: BTreeSet<WindowPc>,
    write_sp: Option<u16>,
    /// Each push of one or more records: its first record's kind and tick.
    pushes: Vec<(u8, u32)>,
}

#[derive(Clone, Copy, Debug)]
struct Service { at: u64, interrupted: WindowPc, vector: u32, clocks: u32 }

/// Capture before an instruction or IRQ changes the stack. A sled PC belongs to its
/// caller's window; keep both addresses so edge coverage distinguishes shared callers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct WindowPc { pc: u32, caller: Option<u32> }

impl WindowPc {
    fn capture(cpu: &Cpu, image: &Image) -> Self {
        let caller = if (image.symbols.pad..image.symbols.pad_end).contains(&cpu.pc) {
            assert!(cpu.sp + 2 <= image.layout.stack_top, "sled must have a return address");
            Some(u32::from(cpu.read(cpu.sp + 1)) << 8 | u32::from(cpu.read(cpu.sp + 2)))
        } else { None };
        Self { pc: cpu.pc, caller }
    }

    fn in_window(self, window: std::ops::Range<u32>) -> bool {
        window.contains(&self.pc) || self.caller.is_some_and(|pc| window.contains(&pc))
    }
}

#[derive(Clone, Copy, Debug)]
struct LockSample {
    due: u64,
    tick: u64,
    selected: u8,
    owed: u8,
    delta: i64,
    raw: i64,
    burn: u32,
    pass: u64,
}

/// Bulk execution never crosses an observed PC, an I-bit change, or a timer
/// edge. The oracle still executes every instruction. Only redundant observer
/// calls are omitted; the edge and its eventual service are single-stepped.
struct TracePlan {
    straight: Vec<u64>,
    delay_loop: u32,
}

impl TracePlan {
    fn new(image: &Image) -> Self {
        let mut stops = BTreeSet::from([image.symbols.pass, image.symbols.now48,
            image.symbols.lock, image.symbols.lock_end, image.symbols.write,
            image.symbols.write_effect, image.symbols.push]);
        stops.extend(&image.symbols.entries);
        let mut decoded = Vec::new();
        let mut pc = 0;
        let mut delay_loop = None;
        while pc < image.words.len() {
            if let Some((insn, words)) = decode(&image.words[pc..]) {
                if (image.symbols.delay..image.symbols.delay_end).contains(&(pc as u32))
                    && matches!(insn, Sbiw(24, 1)) { delay_loop = Some(pc as u32); }
                decoded.push((pc, insn, words)); pc += words;
            } else { pc += 1; }
        }
        let mut straight = vec![0; FLASH_WORDS];
        for (pc, insn, words) in decoded.into_iter().rev() {
            let ordinary = !matches!(insn, Brbs(..) | Brbc(..) | Rjmp(_) | Jmp(_)
                | Rcall(_) | Call(_) | Ijmp | Icall | Ret | Reti | Sleep | Break
                | Sbrc(..) | Sbrs(..) | Sbic(..) | Sbis(..) | Cpse(..)
                | Bset(7) | Bclr(7) | Out(0x3f, _)) || matches!(insn, Rjmp(0));
            if ordinary && !stops.contains(&(pc as u32)) {
                straight[pc] = u64::from(cycles(&insn).0) + straight[pc + words];
            }
        }
        Self { straight, delay_loop: delay_loop.expect("delay must have its four-clock loop") }
    }
}

impl Trace {
    fn advance(&mut self, cpu: &mut Cpu, image: &Image, t0: u64, plan: &TracePlan) {
        // RX completion can occur between any pair of observed PCs. Existing
        // no-RX sweeps retain their bulk observer and speed.
        if image.layout.rx_depth != 0 { self.step(cpu, image, t0); return; }
        if !self.deferred && (cpu.sreg & 128 == 0 ||
            (cpu.read(0x35) & 1 == 0 && cpu.read(0xc0) & cpu.read(0xc1) & 0x20 == 0)) {
            let mut available = plan.straight[cpu.pc as usize];
            if cpu.pc == plan.delay_loop {
                let q = u16::from_le_bytes([cpu.r[24], cpu.r[25]]);
                // Leave the final iteration and RET for the observer.
                available = 4 * u64::from(q.saturating_sub(1));
            }
            let edge = OVERFLOW - (cpu.cycles - t0) % OVERFLOW;
            let budget = available.min(edge.saturating_sub(4));
            if budget >= 16 {
                let start = cpu.cycles;
                run(cpu, budget);
                assert!(cpu.cycles - start < edge, "bulk observer must stop before the timer edge");
                return;
            }
        }
        self.step(cpu, image, t0);
    }

    fn step(&mut self, cpu: &mut Cpu, image: &Image, t0: u64) {
        if image.layout.records == 0 { assert_unobserved_uart(cpu, image.layout.rx_depth != 0); }
        // Both the far suffix and the idle recheck have an atomic SEI/SLEEP.
        if cpu.will_execute() && matches!(decode(&cpu.flash[cpu.pc as usize..]), Some((Sleep, _))) {
            cpu.step().unwrap();
            let interrupted = WindowPc::capture(cpu, image);
            cpu.step().unwrap();
            assert!(matches!(cpu.pc, 32 | 36 | 38), "idle wake must enter an enabled ISR");
            // Idle adds four wake clocks before entry; only the last four
            // clocks have I clear. No instruction executes during the wait.
            self.service = Some(self.services.len());
            self.services.push(Service { at: cpu.cycles - 4, interrupted, vector: cpu.pc, clocks: 0 });
            self.deferred = false;
            return;
        }
        let pc = cpu.pc;
        let location = WindowPc::capture(cpu, image);
        let start = cpu.cycles;
        let enabled = cpu.sreg & 128 != 0;
        let tov = enabled && !self.deferred && cpu.read(0x35) & cpu.read(0x6e) & 1 != 0;
        let rx = enabled && !self.deferred && !tov && cpu.read(0xc0) & cpu.read(0xc1) & 0x80 != 0;
        let uart = enabled && !self.deferred && !tov && !rx && cpu.read(0xc0) & cpu.read(0xc1) & 0x20 != 0;
        let (insn, _) = decode(&cpu.flash[pc as usize..]).expect("trace PC must decode");
        if !tov && !rx && !uart {
            if pc == image.symbols.pass { self.pass = start; }
            if pc == image.symbols.lock { self.lock_starts.push(start); }
            if pc == image.symbols.rx_take && image.layout.rx_depth != 0 { self.rx_takes.push(start); }
            if image.layout.rx_depth != 0 {
                if pc == image.symbols.rx_check { self.rx_checks.push((start, cpu.read(image.layout.rx_early))); }
                if pc == image.symbols.rx_defer { self.rx_defers.push(start); }
                if let Some(k) = image.symbols.dispatch.iter().position(|&entry| entry == pc) {
                    self.dispatches.push((k, start, cpu.read(image.layout.rx_early)));
                }
            }
            if image.symbols.entries.first().is_some_and(|&first| pc >= first) {
                if let Some(k) = image.symbols.entries.iter().position(|&entry| entry == pc) {
                    self.entries.push((k, start));
                }
            }
            if pc == image.symbols.write { self.write_sp = Some(cpu.sp); }
            if pc == image.symbols.push {
                // Producers run in passes with UDRIE off. The foreground
                // encoder is the sole ring consumer; the ISR only pumps TX.
                assert_eq!(cpu.read(0xc1) & 0x20, 0, "a push must run with the drain masked");
                if cpu.read(image.layout.record_count) != 0 {
                    self.pushes.push((cpu.read(image.layout.record), load(cpu, image.layout.record + 1, 4) as u32));
                }
            }
            if pc == image.symbols.lock_end {
                let due = load(cpu, image.layout.due, 6);
                let tick = load(cpu, image.layout.stamp, 6);
                let difference = due.wrapping_sub(tick) & DISARMED;
                let delta = if difference & (1 << 47) == 0 { difference as i64 }
                    else { difference as i64 - (1i64 << 48) };
                // r19:r18 retains the full twelve-bit owed product at this
                // boundary, before SEI's deferred CALL can service a request.
                let product = u16::from_le_bytes([cpu.r[18], cpu.r[19]]);
                let owed = (product >> 4) as u8;
                assert_eq!(product, u16::from(owed) * 16, "owed product must retain all twelve bits");
                assert_eq!(owed, (((due + u64::from(E)) >> 8) as u8).wrapping_sub(cpu.r[13]),
                    "lock must retain the full served-count difference");
                let raw = 8 * delta - i64::from(cpu.read(image.layout.phase)) - i64::from(C)
                    - i64::from(TOV_INT) * i64::from(owed);
                let burn = u32::from(cpu.r[24]) | u32::from(cpu.r[25]) << 8 | u32::from(cpu.r[23]) << 16;
                assert_eq!(i64::from(burn), if delta < 0 { i64::from(N_MIN) } else { raw.max(i64::from(N_MIN)) },
                    "signed 24-bit burn must clamp without wrapping");
                self.owed.push(owed);
                self.locks.push(LockSample { due, tick, selected: cpu.read(image.layout.selected),
                    owed, delta, raw, burn, pass: self.pass });
            }
        }
        cpu.step().expect("traced instruction must execute");
        if tov {
            assert_eq!((cpu.pc, cpu.cycles), (32, start + 4), "pending TOV must enter at the next eligible boundary");
        } else if rx {
            assert_eq!((cpu.pc, cpu.cycles), (36, start + 4), "RX must enter after TOV and before UDRE");
        } else if uart {
            assert_eq!((cpu.pc, cpu.cycles), (38, start + 4), "pending drain must enter after higher priority TOV");
        } else {
            // Instruction clocks are (start,end]: timer edges run before the
            // completed load/store. Masked edges belong to this instruction,
            // even when their ISR is deferred to a later eligible boundary.
            if self.entries.is_empty() && (start - t0) / OVERFLOW < (cpu.cycles - t0) / OVERFLOW {
                self.edge_pcs.insert(location);
            }
            if pc == image.symbols.now48 {
                self.now_samples.push(cpu.cycles);
                if self.write_sp.is_some() { self.samples.push(cpu.cycles); }
            }
            if pc == image.symbols.rx_sample { self.rx_samples.push((cpu.cycles - t0) / 8); }
            if pc == image.symbols.write_effect { self.effects.push(cpu.cycles); }
            if matches!(insn, Ret) && self.write_sp.is_some_and(|sp| cpu.sp == sp + 2) { self.write_sp = None; }
            if matches!(insn, Reti) {
                let service = &mut self.services[self.service.take().expect("RETI must finish an observed service")];
                service.clocks = (cpu.cycles - service.at).try_into().unwrap();
            }
            self.deferred = matches!(insn, Bset(7) | Reti);
        }
        if tov || rx || uart {
            assert!(self.service.is_none(), "interrupt services must not nest");
            self.service = Some(self.services.len());
            self.services.push(Service { at: start, interrupted: location, vector: cpu.pc, clocks: 0 });
        }
    }
}

fn entry_start(t0: u64, due: u64) -> u64 {
    let s = t0 + 8 * due + u64::from(DISPATCH);
    let first = (s - u64::from(L) - t0).div_ceil(OVERFLOW).max(1);
    let edge = t0 + first * OVERFLOW;
    s + if edge < s { u64::from(TOV_INT) } else { 0 }
}

fn service_clocks(trace: &Trace, from: u64, to: u64) -> u64 {
    trace.services.iter().filter(|s| (from..to).contains(&s.at)).map(|s| {
        assert!(s.clocks != 0 && s.at + u64::from(s.clocks) <= to, "interval must contain complete services");
        u64::from(s.clocks)
    }).sum()
}

fn effect_cycle(trace: &Trace, start: u64) -> (u64, usize) {
    let effect = *trace.effects.iter().find(|&&at| at >= start).expect("entry must reach write_effect");
    let count = trace.services.iter().filter(|s| (start..effect).contains(&s.at)).count();
    (effect, count)
}

#[test]
fn boot() {
    let program = Program { image_id: 0x1234_5678, ..Program::default() };
    let image = build(&program).unwrap();
    let mut cpu = Cpu::new(&image.words);
    let initial_wdtcsr = cpu.read(0x60);
    to_symbol(&mut cpu, image.symbols.t0, 30_000);
    let t0 = cpu.cycles;
    assert_eq!(cpu.read(0x54), 0, "reset must acknowledge MCUSR");
    assert_eq!(cpu.read(0x60) & 8, 0, "reset must disable WDE inside the timed window");
    assert_eq!(cpu.read(0x44), 3, "clock mode must remain fast PWM without COM outputs");
    assert_eq!(cpu.read(0x46), 0, "GTCCR release completion is tick zero");
    run(&mut cpu, 300_000);
    let got = frames(&cpu);
    assert!(matches!(got.first(), Some((watch::Frame { event: watch::Event::Hello {
        proto: watch::PROTO, ports_version: PORTS_VERSION, image: 0x1234_5678,
        clock_hz, prescale: PRESCALE, ring, tick_hi: 0, mcusr: 2, wdtcsr, level: watch::Level::Ports, generation: 0,
    }, .. }, 0)) if *clock_hz == program.f_clk && *ring == u16::from(image.layout.records) && *wdtcsr == initial_wdtcsr),
        "first frame must describe the reset image");
    to_idle_boundary(&mut cpu, &image, 20_000);
    for elapsed in [2040, 2048, 2055, 1 << 24, (1u64 << 32) * 8 + 16] {
        clock_at(&mut cpu, &image, t0, elapsed); cpu.sreg = 0;
        let call = invoke(&mut cpu, &image, image.symbols.now48);
        let now: u64 = (20..26).map(|r| u64::from(cpu.r[r]) << (8 * (r - 20))).sum();
        assert_eq!(now, (call.samples[0] - t0) / 8, "now48 must use the completed TCNT0 sample");
        assert_eq!(call.clocks, u64::from(NOW48), "now48 path must have its declared length");
    }
}

fn pwm_setting(timer: u8, top: u16, prescale: u16) -> PwmSetting {
    PwmSetting { timer, top, prescale, mode: pwm::ATMEGA328P.timers[timer as usize].mode }
}

fn duty_call(a: &mut Asm, s: &Stubs, pin: u8, level: i32, site: u16) {
    a.emit(Ldi(16, pin));
    for r in 17..20 { a.emit(Ldi(r, 0)); }
    for (r, byte) in (20..24).zip(level.to_le_bytes()) { a.emit(Ldi(r, byte)); }
    a.emit(Ldi(24, site as u8)); a.emit(Ldi(25, (site >> 8) as u8));
    a.jump(Target::Call, s.duty);
}

fn pwm_edges(cpu: &Cpu, pin: u8) -> Vec<(u64, bool)> {
    let (port, bit) = if pin < 8 { ('D', pin) } else { ('B', pin - 8) };
    cpu.events.iter().filter_map(|event| match *event {
        Event::Pin { cycle, port: p, bit: b, high } if (p, b) == (port, bit) => Some((cycle, high)),
        _ => None,
    }).collect()
}

// Waveform fixtures suppress interrupt service, leaving all timers running.
// The two-clock idle loop runs in bulk; a final NOP reaches an odd endpoint.
fn pwm_idle_to(cpu: &mut Cpu, image: &Image, at: u64) {
    assert!(cpu.cycles <= at, "PWM fixture cannot travel backward");
    cpu.sreg = 0;
    let pc = scratch(cpu, image);
    cpu.flash[pc as usize] = encode(&Rjmp(-1)).unwrap().0[0]; cpu.pc = pc;
    let clocks = (at - cpu.cycles) & !1;
    if clocks != 0 { run(cpu, clocks); }
    if cpu.cycles < at { cpu.pc = scratch(cpu, image); cpu.step().unwrap(); }
    assert_eq!(cpu.cycles, at, "PWM fixture must reach the chosen cycle exactly");
}

fn duty_at(cpu: &mut Cpu, image: &Image, at: u64, pin: u8, level: i32) -> Call {
    pwm_idle_to(cpu, image, at);
    cpu.r[16..20].copy_from_slice(&i32::from(pin).to_le_bytes());
    cpu.r[20..24].copy_from_slice(&level.to_le_bytes());
    cpu.r[24] = 0xcd; cpu.r[25] = 0xab;
    let call = invoke(cpu, image, image.symbols.duty);
    within("duty", call.clocks, image.bounds.duty);
    assert_eq!(call.effects.len(), 1, "duty must reach one effect store");
    within("duty_effect", call.effects[0] - at, image.bounds.duty_effect);
    assert!(call.effects[0] < call.pushes[0], "duty effect must precede its watch push");
    call
}

// Measure the selected path on a copy before placing its effect on a phase
// boundary. The published effect bound is not an offset for every pin/level.
fn duty_start(cpu: &Cpu, image: &Image, effect: u64, pin: u8, level: i32) -> u64 {
    let mut probe = cpu.clone();
    let at = probe.cycles;
    let offset = duty_at(&mut probe, image, at, pin, level).effects[0] - at;
    effect.checked_sub(offset).expect("effect must leave room for the duty prefix")
}

fn pwm_update(setting: PwmSetting, t0: u64, effect: u64, level: u8) -> u64 {
    let period = setting.period_cycles();
    match setting.mode {
        PwmMode::Fast if level == 0 => effect,
        PwmMode::Fast => t0 + ((effect - t0) / period + 1) * period,
        PwmMode::PhaseCorrect => {
            // The oracle/datasheet compare action follows equality by one
            // timer clock. TOP count is top*N; TOP action is (top+1)*N.
            // A store completing on that action misses this transfer.
            let first = t0 + (u64::from(setting.top) + 1) * u64::from(setting.prescale);
            first + if effect < first { 0 } else { ((effect - first) / period + 1) * period }
        }
    }
}

// Independent period/high-time model. No counter, comparator, or OCR-buffer
// simulation: evaluate the closed-form waveform on its t0-based phase grid.
// Fast reconnect transients are covered separately, below.
fn model_pwm_edges(setting: PwmSetting, t0: u64, changes: &[(u64, u8)], end: u64) -> Vec<(u64, bool)> {
    let updates: Vec<_> = changes.iter().map(|&(effect, level)| (pwm_update(setting, t0, effect, level), level)).collect();
    let period = setting.period_cycles();
    let mut next = 0;
    let mut level = 0;
    let mut before = false;
    let mut edges = Vec::new();
    for cycle in t0 + 1..=end {
        while next < updates.len() && updates[next].0 <= cycle { level = updates[next].1; next += 1; }
        let high_time = setting.high_cycles(level);
        let high = match setting.mode {
            PwmMode::Fast => (cycle - t0) % period < high_time,
            PwmMode::PhaseCorrect => {
                let phase = (cycle - t0 + period - u64::from(setting.prescale)) % period;
                phase < high_time / 2 || phase >= period - high_time / 2
            }
        };
        if high != before { edges.push((cycle, high)); before = high; }
    }
    edges
}

#[test]
fn pwm_waveforms_follow_model() {
    let mut executed = 0;
    for (pin, setting) in [(6, pwm_setting(0, 255, 8)), (9, pwm_setting(1, 255, 1)),
        (10, pwm_setting(1, 320, 1)), (3, pwm_setting(2, 255, 1))] {
        let image = build(&Program { pins: vec![PinDef { pin, role: Role::Pwm(setting) }], ..Program::default() }).unwrap();
        let period = setting.period_cycles();
        for level in [0, 1, 128, 254, 255] {
            let (mut cpu, t0) = booted(&image);
            assert!(pwm_edges(&cpu, pin).is_empty(), "PWM must be low throughout boot");
            let control = cpu.read(if setting.timer == 1 { 0x80 } else if setting.timer == 2 { 0xb0 } else { 0x44 });
            if setting.timer == 1 {
                let mode = (control & 3) | ((cpu.read(0x81) >> 1) & 12);
                assert_eq!(mode, if setting.top == 255 { 1 } else { 10 }, "Timer1 must select phase-correct with the declared TOP");
            }
            let first = duty_at(&mut cpu, &image, t0, pin, 128).effects[0];
            let effect = t0 + 4 * period + if setting.timer == 0 { 256 }
                else { u64::from(setting.top / 2) * u64::from(setting.prescale) };
            let start = duty_start(&cpu, &image, effect, pin, level);
            let observed = duty_at(&mut cpu, &image, start, pin, level).effects[0];
            assert_eq!(observed, effect, "waveform update must hit the chosen phase");
            let end = effect + 3 * period;
            pwm_idle_to(&mut cpu, &image, end);
            let actual = pwm_edges(&cpu, pin);
            assert_eq!(actual, model_pwm_edges(setting, t0, &[(first, 128), (effect, level as u8)], end),
                "t0 phase, old pulse, and update boundary: pin={pin} setting={setting:?} level={level}");
            if setting.timer != 0 {
                assert_eq!(cpu.read(if setting.timer == 1 { 0x80 } else { 0xb0 }), control,
                    "phase-correct COM must stay connected even for zero");
            }
            assert_eq!(cpu.read(0x44) & 3, 3, "duty must preserve the clock's fast WGM bits");
            assert_eq!(cpu.read(0x45), 2, "duty must preserve the clock's prescaler and WGM high bit");
            let steady: Vec<_> = actual.iter().copied().filter(|&(at, _)| at >= effect + period).collect();
            if (1..255).contains(&level) {
                let pulse = steady.windows(3).find(|p| p[0].1 && !p[1].1 && p[2].1).expect("two settled pulses must be sampled");
                assert_eq!(pulse[2].0 - pulse[0].0, period, "edge period must equal PwmSetting");
                assert_eq!(pulse[1].0 - pulse[0].0, setting.high_cycles(level as u8), "edge high time must equal PwmSetting");
            } else { assert!(steady.is_empty(), "extreme duties must be constant after their transfer"); }
            executed += cpu.cycles;
        }
    }
    println!("pwm_waveforms_follow_model: {executed} executed oracle cycles");
}

#[test]
fn fast_pwm_reconnect_exposes_frozen_latch() {
    for pin in [5, 6] {
        let setting = pwm_setting(0, 255, 8);
        let image = build(&Program { pins: vec![PinDef { pin, role: Role::Pwm(setting) }], ..Program::default() }).unwrap();
        for frozen_high in [false, true] {
            let (mut cpu, t0) = booted(&image);
            let old = if frozen_high { 254 } else { 128 };
            let first = duty_at(&mut cpu, &image, t0, pin, old).effects[0];
            // This is the first duty after boot: the latch is low and the
            // entire first pulse follows the model, with no COM-store edge.
            let disconnect = t0 + 2048 + if frozen_high { 256 } else { 1280 };
            let start = duty_start(&cpu, &image, disconnect, pin, 0);
            pwm_idle_to(&mut cpu, &image, start);
            assert_eq!(pwm_edges(&cpu, pin), model_pwm_edges(setting, t0, &[(first, old as u8)], cpu.cycles),
                "first duty after boot must exactly match the model");
            let at = cpu.cycles;
            let observed = duty_at(&mut cpu, &image, at, pin, 0).effects[0];
            assert_eq!(observed, disconnect, "disconnect must hit the chosen latch phase");
            let ocr = if pin == 5 { 0x48 } else { 0x47 };
            assert_eq!(cpu.read(ocr), old as u8, "zero leaves OCR unchanged");
            let reconnect_start = cpu.cycles;
            let reconnect = duty_at(&mut cpu, &image, reconnect_start, pin, 128).effects[0];
            let bottom = pwm_update(setting, t0, reconnect, 128);
            pwm_idle_to(&mut cpu, &image, bottom + 2048 + 1032);
            // Fast's exception is exact: zero exposes PORT low immediately;
            // reconnect exposes the frozen OC latch at the COM store. The old
            // active OCR drives any remaining compares until the next BOTTOM.
            let mut expected = if frozen_high {
                vec![(t0 + 2048, true), (disconnect, false), (reconnect, true), (t0 + 4088, false)]
            } else { vec![(t0 + 2048, true), (t0 + 3080, false)] };
            expected.extend([(bottom, true), (bottom + 1032, false), (bottom + 2048, true), (bottom + 3080, false)]);
            assert_eq!(pwm_edges(&cpu, pin), expected, "frozen latch={frozen_high}, pin={pin}");
            assert_eq!((cpu.read(0x44) & 3, cpu.read(0x45)), (3, 2), "reconnect must never change clock WGM or CS");
        }
    }
}

#[test]
fn pwm_reset_and_prescalers() {
    // Both Timer1 outputs share one ICR1; both Timer2 outputs share its own
    // prescaler table. Checking at t0 rejects moving ICR1 after the release.
    for timer in [1, 2] {
        for (index, &prescale) in pwm::ATMEGA328P.timers[timer as usize].prescalers.iter().enumerate() {
            let settings = [pwm_setting(0, 255, 8), pwm_setting(1, 320, if timer == 1 { prescale } else { 1 }),
                pwm_setting(2, 255, if timer == 2 { prescale } else { 1 })];
            let image = build(&Program { pins: pwm::ATMEGA328P.pins.iter().map(|pin| PinDef {
                pin: pin.pin as u8, role: Role::Pwm(settings[pin.timer as usize]),
            }).collect(), ..Program::default() }).unwrap();
            let (mut cpu, t0) = booted(&image);
            assert_eq!((cpu.read(0x44), cpu.read(0x45)), (3, 2), "Timer0 starts disconnected at fast clk/8");
            assert_eq!((cpu.read(0x80), cpu.read(0xb0)), (0xa2, 0xa1), "phase-correct COM connects at reset, WGM 10/1");
            assert_eq!(cpu.read(if timer == 1 { 0x81 } else { 0xb1 }) & 7, index as u8 + 1, "timer-specific CS encoding");
            assert_eq!(load(&cpu, 0x86, 2), 320, "ICR1 must be committed high-first before t0");
            assert_eq!((cpu.read(0x46), load(&cpu, 0x84, 2), cpu.read(0xb2)), (0, 0, 0), "all counters start at zero together");
            for address in [0x47, 0x48, 0x88, 0x89, 0x8a, 0x8b, 0xb3, 0xb4] {
                assert_eq!(cpu.read(address), 0, "OCR must be zero at boot: {address:#x}");
            }
            assert_eq!((cpu.read(0x24), cpu.read(0x2a)), (0x0e, 0x68), "every PWM pin must be an output");
            assert_eq!((cpu.read(0x25), cpu.read(0x2b)), (0, 0), "every PWM port latch must be low");
            pwm_idle_to(&mut cpu, &image, t0 + u64::from(prescale));
            assert_eq!(if timer == 1 { load(&cpu, 0x84, 2) } else { u64::from(cpu.read(0xb2)) }, 1,
                "first timer tick must use its declared prescaler and t0 phase");
            assert!(!cpu.events.iter().any(|event| matches!(event, Event::Pin { high: true, .. })), "no PWM output may rise before duty");
        }
    }
}

#[test]
fn pwm_declaration_refusals() {
    let fast = pwm_setting(0, 255, 8);
    let phase = pwm_setting(1, 320, 1);
    let cases = [
        (4, fast, vec!["pin 4", "no OC output", "PWM timer"]),
        (9, PwmSetting { timer: 2, ..phase }, vec!["pin 9", "OC1A", "Timer1", "Timer2"]),
        (5, PwmSetting { mode: PwmMode::PhaseCorrect, ..fast }, vec!["pin 5", "OC0B", "Timer0", "Fast, clk/8, TOP 255"]),
        (6, PwmSetting { prescale: 64, ..fast }, vec!["pin 6", "OC0A", "Timer0", "device clock"]),
        (5, PwmSetting { top: 254, ..fast }, vec!["pin 5", "OC0B", "Timer0", "device clock"]),
        (10, PwmSetting { prescale: 32, ..phase }, vec!["pin 10", "OC1B", "Timer1", "prescaler 32"]),
        (3, pwm_setting(2, 255, 16), vec!["pin 3", "OC2B", "Timer2", "prescaler 16"]),
        (9, PwmSetting { top: 2, ..phase }, vec!["pin 9", "OC1A", "Timer1", "TOP 2", "3..=65535"]),
        (11, pwm_setting(2, 254, 1), vec!["pin 11", "OC2A", "Timer2", "TOP 254", "255 only"]),
        (9, PwmSetting { mode: PwmMode::Fast, ..phase }, vec!["pin 9", "OC1A", "Timer1", "PhaseCorrect"]),
    ];
    for (pin, setting, parts) in cases {
        let error = build(&Program { pins: vec![PinDef { pin, role: Role::Pwm(setting) }], ..Program::default() })
            .err().expect("invalid PWM declaration must be refused");
        for part in parts { assert!(error.contains(part), "refusal must contain {part:?}: {error}"); }
    }
    for setting in [PwmSetting { top: 255, ..phase }, PwmSetting { prescale: 8, ..phase }] {
        let error = build(&Program { pins: vec![PinDef { pin: 9, role: Role::Pwm(phase) },
            PinDef { pin: 10, role: Role::Pwm(setting) }], ..Program::default() }).err().expect("one timer cannot have two settings");
        for part in ["pin 9", "OC1A", "pin 10", "OC1B", "Timer1", "differs"] {
            assert!(error.contains(part), "shared-timer refusal must contain {part:?}: {error}");
        }
    }
    for top in [3, 255, 65535] {
        assert!(build(&Program { pins: vec![PinDef { pin: 9, role: Role::Pwm(PwmSetting { top, ..phase }) },
            PinDef { pin: 10, role: Role::Pwm(PwmSetting { top, ..phase }) }], ..Program::default() }).is_ok(),
            "shared valid Timer1 TOP {top} must be accepted");
    }
}

#[test]
fn duty_contract_and_frames() {
    let settings = [pwm_setting(0, 255, 8), pwm_setting(1, 320, 64), pwm_setting(2, 255, 32)];
    let mut roles: Vec<_> = pwm::ATMEGA328P.pins.iter().map(|pin| PinDef {
        pin: pin.pin as u8, role: Role::Pwm(settings[pin.timer as usize]),
    }).collect();
    roles.extend([PinDef { pin: 12, role: Role::Input { pull: true } }, PinDef { pin: 13, role: Role::Output }]);
    let image = build(&Program { pins: roles, ..Program::default() }).unwrap();
    let (mut cpu, t0) = booted(&image);
    let registers = [0x24, 0x25, 0x2a, 0x2b, 0x44, 0x45, 0x47, 0x48,
        0x80, 0x81, 0x88, 0x89, 0x8a, 0x8b, 0xb0, 0xb1, 0xb3, 0xb4];
    let mut cases = Vec::new();
    for pin in [3i32, 5, 6, 9, 10, 11] {
        for level in [i32::MIN, -1, 0, 1, 128, 254, 255, 256, 65536, i32::MAX] {
            cases.push((pin, level, false));
            if matches!(pin, 5 | 6) { cases.push((pin, level, true)); }
        }
    }
    cases.extend([0, 1, 2, 4, 12, 13, 20, 255, 300, 269, -1, 265, 65545, 16777225,
        i32::MIN, i32::MAX].map(|pin| (pin, 256, false)));
    for (pin, level, connected) in cases {
        let elapsed = cpu.cycles - t0 + 32;
        clock_at(&mut cpu, &image, t0, elapsed); cpu.write(0x6e, 0);
        let own_com = if pin == 5 { 0x20 } else { 0x80 };
        if matches!(pin, 5 | 6) {
            // The other Timer0 output is already connected. Its COM and OCR
            // must survive every path, including disconnecting this output.
            cpu.write(0x44, 3 | (0xa0 ^ own_com) | if connected { own_com } else { 0 });
        }
        // Poison shared TEMP: writing Timer1 OCR low-first must now fail even
        // when the new high byte happens to equal the old OCR high byte.
        cpu.write(0x89, 0xa5);
        cpu.write(image.layout.head, 0); cpu.write(image.layout.tail, 0);
        store(&mut cpu, image.layout.lost, 0, 4); cpu.write(image.layout.hello_pending, 0);
        cpu.events.clear();
        for r in 6..13 { cpu.r[r] = 0x91 + r as u8; }
        cpu.r[28] = 0x45; cpu.r[29] = 1;
        cpu.r[16..20].copy_from_slice(&pin.to_le_bytes());
        cpu.r[20..24].copy_from_slice(&level.to_le_bytes());
        cpu.r[24] = 0xcd; cpu.r[25] = 0xab;
        let kept = preserved(&cpu);
        let before: Vec<_> = registers.iter().map(|&r| cpu.read(r)).collect();
        let start = cpu.cycles;
        let call = invoke(&mut cpu, &image, image.symbols.duty);
        within(&format!("duty pin={pin} level={level} connected={connected}"), call.clocks, image.bounds.duty);
        for &effect in &call.effects {
            within("duty_effect", effect - start, image.bounds.duty_effect);
            assert!(effect < call.pushes[0], "duty effect must precede its watch push");
        }
        assert_eq!(preserved(&cpu), kept, "duty preserves r2, r6..r15 and Y");
        assert_eq!(cpu.r[16..20], pin.to_le_bytes(), "duty preserves the full pin argument");
        assert_eq!(call.samples.len(), 1, "duty takes exactly one masked stamp");
        let tick = (call.samples[0] - t0) / 8;
        assert_eq!(load(&cpu, image.layout.stamp, 6), tick, "duty stamp must equal the sampled timer cycle");
        assert_eq!(cpu.read(image.layout.head), 2, "Duty and Fault each consume two ring records");
        let output = pwm::ATMEGA328P.pins.iter().find(|p| p.pin == pin as u32);
        let mut changed = Vec::new();
        let expected_event;
        let effect_address;
        if let Some(output) = output {
            let clamped = level.clamp(0, 255) as u8;
            let ocr = match pin { 3 => 0xb4, 5 => 0x48, 6 => 0x47, 9 => 0x88, 10 => 0x8a, 11 => 0xb3, _ => unreachable!() };
            if output.timer == 0 {
                changed.push(0x44);
                if clamped != 0 { changed.push(ocr); }
                assert_eq!(cpu.read(0x44), 3 | (0xa0 ^ own_com) | if clamped != 0 { own_com } else { 0 },
                    "only this channel's COM may change; clock WGM and sibling COM survive");
                effect_address = Some(if clamped == 0 || !connected { 0x44 } else { ocr });
                if clamped != 0 { assert_eq!(cpu.read(ocr), clamped, "fast duty writes the saturated level"); }
            } else {
                effect_address = Some(ocr);
                changed.push(ocr);
                if output.timer == 1 { changed.push(ocr + 1); }
                assert_eq!(load(&cpu, ocr, if output.timer == 1 { 2 } else { 1 }),
                    u64::from(settings[output.timer as usize].ocr(clamped)), "OCR must contain the model's rounded, saturated value");
            }
            expected_event = watch::Event::Duty { pin: pin as u8, level: clamped, site: 0xabcd };
            assert_eq!(cpu.read(image.layout.ring + 8), watch::DUTY | 0x80, "Duty continuation kind");
        } else {
            effect_address = None;
            expected_event = watch::Event::Fault { code: watch::FAULT_PIN, site: 0xabcd, arg: pin };
            assert_eq!(cpu.read(image.layout.ring), watch::FAULT, "undeclared or wrong-role pin must queue Fault");
        }
        if let Some(effect_address) = effect_address {
            assert_eq!(call.effects.len(), 1, "valid duty must reach one effect store");
            assert_eq!(call.stores.len(), 1, "one effect store must complete");
            assert_eq!(call.stores[0].0, effect_address, "effect store must target the selected OCR or COM");
            assert_eq!(call.stores[0].1, cpu.read(effect_address), "hardware value must be committed at the effect cycle");
        }
        for (i, &r) in registers.iter().enumerate() {
            if !changed.contains(&r) { assert_eq!(cpu.read(r), before[i], "unrelated hardware changed: pin={pin} register={r:#x}"); }
        }
        drain(&mut cpu, &image);
        let got = frames(&cpu);
        assert_eq!(got.len(), 1, "one duty invocation must produce exactly one wire frame");
        assert_eq!((got[0].0.event, got[0].1), (expected_event, tick), "Duty/Fault fields and stamp must survive the ring and wire");
        let mut wire = [0; watch::MAX_WIRE];
        let n = watch::encode(&got[0].0, &mut wire);
        let transmitted: Vec<_> = cpu.events.iter().filter_map(|event| match event { Event::Tx { byte, .. } => Some(*byte), _ => None }).collect();
        assert_eq!(transmitted, wire[..n], "firmware and host must encode the same Duty/Fault bytes");
    }
    // Ring-state variants share the same effect/total budgets. Neither a full
    // ring nor flushing a preceding Lost may suppress the actual OCR write.
    for (full, lost) in [(true, 0), (false, 7)] {
        let elapsed = cpu.cycles - t0 + 32;
        clock_at(&mut cpu, &image, t0, elapsed); cpu.write(0x6e, 0);
        cpu.write(image.layout.head, if full { image.layout.records - 1 } else { 0 }); cpu.write(image.layout.tail, 0);
        store(&mut cpu, image.layout.lost, lost, 4);
        store(&mut cpu, image.layout.lost_first, 1, 4); store(&mut cpu, image.layout.lost_last, 2, 4);
        let start = cpu.cycles;
        duty_at(&mut cpu, &image, start, 9, 254);
        assert_eq!(load(&cpu, 0x88, 2), u64::from(settings[1].ocr(254)), "ring pressure cannot alter the duty effect");
        assert_eq!(load(&cpu, image.layout.lost, 4), if full { 1 } else { 0 }, "full ring counts loss; available room flushes it");
        if !full {
            assert_eq!(cpu.read(image.layout.ring), watch::LOST, "pending Lost precedes Duty");
            assert_eq!(cpu.read(image.layout.ring + 16), watch::DUTY, "Duty remains a distinct two-record event after Lost");
        }
    }
    let image = build(&Program { pins: vec![PinDef { pin: 3, role: Role::Input { pull: false } },
        PinDef { pin: 9, role: Role::Output }], ..Program::default() }).unwrap();
    let (mut cpu, _) = booted(&image);
    for pin in [3, 6, 9] {
        // Physical OC capability is insufficient: these are an input, an
        // undeclared pin, and an ordinary output, respectively.
        cpu.write(image.layout.head, 0); cpu.write(image.layout.tail, 0);
        let before: Vec<_> = registers.iter().map(|&r| cpu.read(r)).collect();
        cpu.sreg = 0;
        cpu.r[16..20].copy_from_slice(&i32::from(pin).to_le_bytes());
        cpu.r[20..24].copy_from_slice(&128i32.to_le_bytes());
        cpu.r[24] = 0xcd; cpu.r[25] = 0xab;
        let call = invoke(&mut cpu, &image, image.symbols.duty);
        within("duty wrong role", call.clocks, image.bounds.duty);
        assert_eq!(registers.iter().map(|&r| cpu.read(r)).collect::<Vec<_>>(), before, "wrong-role duty must change no hardware");
        assert_eq!((cpu.read(image.layout.ring), cpu.read(image.layout.ring + 5), cpu.read(image.layout.ring + 9)),
            (watch::FAULT, watch::FAULT_PIN, pin), "wrong-role duty must fault with its pin argument");
    }
}

#[test]
fn dispatch_exact_under_pwm() {
    let w = make_image(1, true, &blink_entry).layout.w;
    let period = 256 * (u64::from(w) + 64).div_ceil(256) + 1;
    let mut executed = 0;
    // Endpoints around effect insertion (218/219), stamp masking, and the
    // dispatch window (247/248 and 250/251), plus both counter halves/wrap.
    // Interior residues with the same insertion count add no PWM property;
    // now48_at_overflow and the full pass sweep retain cycle-level coverage.
    // 217 inserts nothing, as 218, but makes the eighteenth firing: variant 3
    // pushes 13 records a firing and never drains in the near path, so that
    // firing fills the ring, and its refused events must arrive as one
    // counted Lost frame.
    let residues = [1u64, 127, 128, 217, 218, 219, 236, 237, 242, 243, 244, 247, 248, 249, 250, 251, 255, 0];
    for (variant, duty) in [(0, 0), (1, 1), (1, 128), (1, 255), (2, 128), (3, 128)] {
        let pwm_pins: Vec<_> = pwm::ATMEGA328P.pins.iter()
            .filter(|pin| variant >= 2 || (variant == 1 && pin.timer == 0)).collect();
        let mut roles = vec![PinDef { pin: 13, role: Role::Output }];
        roles.extend(pwm_pins.iter().map(|pin| PinDef { pin: pin.pin as u8,
            role: Role::Pwm(pwm_setting(pin.timer, 255, if pin.timer == 0 { 8 } else { 64 })) }));
        let setup = |a: &mut Asm, s: &Stubs| {
            for pin in &pwm_pins { duty_call(a, s, pin.pin as u8, duty, 2); }
        };
        let entry = |a: &mut Asm, s: &Stubs| {
            blink_entry(a, s);
            if variant == 3 {
                for pin in &pwm_pins { duty_call(a, s, pin.pin as u8, duty, 3); }
            }
        };
        let image = build(&Program {
            image_id: 2, state_init: vec![0], pins: roles,
            timers: vec![TimerDef { entry: 0, period_ticks: period, repeat: true }],
            entries: vec![&entry], setup: Some(&setup), ..Program::default()
        }).unwrap();
        let (mut probe, _) = booted(&image);
        probe.sreg = 0; probe.write(0x6e, 0);
        let at = probe.cycles;
        let prefix = invoke(&mut probe, &image, image.symbols.entries[0]).effects[0] - at;
        // Keep one edge just before the effect even when the write prefix shrinks.
        let prefix_residue = (OVERFLOW + 9 - u64::from(DISPATCH) - prefix) / 8;
        let (mut cpu, t0) = booted(&image);
        // Six real duty calls consume most of one firing interval. Give the
        // first firing room for the full near window too, retaining the owed
        // >2 mutation guard; later firings still use the original period.
        let mut due = period + if variant == 3 { 1024 } else { 0 };
        store(&mut cpu, image.layout.timers, due, 6);
        executed += t0;
        let plan = TracePlan::new(&image);
        let mut trace = Trace::default();
        let mut windows = 0;
        let mut observed_windows = 0;
        let mut inserted = 0;
        for (i, residue) in residues.into_iter().enumerate() {
            let residue = if residue == 219 { prefix_residue } else { residue };
            if i != 0 {
                let next = due + period;
                due = next + (residue + 256 - next % 256) % 256;
                store(&mut cpu, image.layout.timers, due, 6);
            }
            while trace.effects.len() <= i {
                assert!(cpu.cycles < t0 + 8 * due + 2000, "PWM variant {variant} must keep dispatching");
                trace.advance(&mut cpu, &image, t0, &plan);
            }
            let s = trace.entries[i].1;
            let (effect, high) = *pins(&cpu).last().unwrap();
            let expected = entry_start(t0, due);
            assert_eq!(s, expected, "dispatch phase variant={variant} duty={duty} firing={i} due={due}");
            windows += usize::from(expected != t0 + 8 * due + u64::from(DISPATCH));
            observed_windows += usize::from(s != t0 + 8 * due + u64::from(DISPATCH));
            let (expected_effect, k) = effect_cycle(&trace, s);
            inserted += k;
            assert_eq!(effect, expected_effect, "pin effect includes prefix ISR insertions firing={i}");
            assert_eq!(high, i % 2 == 0, "pin values must alternate");
            if residue == prefix_residue {
                let lock = trace.locks[i];
                assert!(lock.delta >= 0 && lock.raw >= i64::from(N_MIN), "prefix ISR case must be on time, without the clamp");
                assert_eq!(s, t0 + 8 * due + u64::from(DISPATCH), "prefix ISR case has no dispatch-window insertion");
                assert_eq!(k, 1, "second due must put one overflow inside the entry prefix");
                within("blink write_effect", effect - s - service_clocks(&trace, s, effect),
                    16 + image.bounds.write_effect);
            }
            while cpu.pc != image.symbols.pass { trace.advance(&mut cpu, &image, t0, &plan); }
            assert_eq!(load(&cpu, image.layout.timers, 6), due + period, "dispatch must preserve the period grid before selecting the next phase");
        }
        assert_eq!(pins(&cpu).len(), residues.len(), "every selected firing must toggle PB5 independently of the agent");
        assert!(windows > 0, "the residue sweep must exercise dispatch's masked window");
        assert_eq!(observed_windows, windows, "observed window insertions must equal the phase model's count");
        assert!(inserted > 0, "the residue sweep must exercise an ISR inside PREFIX");
        assert!(trace.owed.iter().any(|&owed| owed > 2), "PWM sweep must reject truncating owed to three choices");
        store(&mut cpu, image.layout.timers, DISARMED, 6);
        drain_passes(&mut cpu, &image);
        executed += cpu.cycles - t0;
        let decoded = frames(&cpu);
        // Setup's events precede the trace; every traced push is either
        // delivered, in order, or refused into the Lost count, which the next
        // pass with room appends.
        let mut next = 0;
        let mut refused = Vec::new();
        for &(kind, tick) in &trace.pushes {
            match decoded[next..].iter().position(|(f, _)| f.kind == kind && f.tick == tick) {
                Some(k) => next += k + 1,
                None => refused.push((kind, tick)),
            }
        }
        let lost: Vec<_> = decoded.iter().filter_map(|(f, _)| match f.event {
            watch::Event::Lost { count, saturated, first_tick } => Some((count, saturated, first_tick, f.tick)),
            _ => None,
        }).collect();
        if variant == 3 {
            // The longer Hello reserves eight more bytes in watch-on SRAM, so the ring holds one fewer record.
            assert_eq!(refused.len(), 9, "the eighteenth firing must overflow the ring at 13 records a firing");
            assert_eq!(refused.iter().filter(|&&(kind, _)| kind == watch::WRITE).count(), 1,
                "the overflow must refuse exactly the eighteenth firing's Write");
        } else {
            assert!(refused.is_empty(), "variant {variant} must deliver every event");
        }
        match refused.first().zip(refused.last()) {
            None => assert!(lost.is_empty(), "no Lost frame without a refused event"),
            Some((first, last)) => assert_eq!(lost, [(refused.len() as u32, false, first.1, last.1)],
                "one Lost frame must count the refused events with their first and last stamps"),
        }
        let ticks: Vec<_> = trace.samples.iter().map(|&sample| (sample - t0) / 8)
            .filter(|&tick| !refused.contains(&(watch::WRITE, tick as u32))).collect();
        let writes: Vec<_> = decoded.iter().filter_map(|(frame, tick)|
            matches!(frame.event, watch::Event::Write { .. }).then_some(*tick)).collect();
        assert_eq!(writes, ticks, "every delivered Write must retain its own completed sample's tick");
    }
    let image = make_image(500_000, true, &blink_entry);
    let (mut cpu, t0) = booted(&image);
    executed += t0;
    let plan = TracePlan::new(&image);
    // Twelve consecutive firings cover the three-second grid. Skip only the
    // uneventful idle interval, initializing the served clock exactly as in
    // the isolated clock tests; execute each complete near pass and handler.
    for i in 1..=12 {
        let due = i * 500_000;
        assert_eq!(load(&cpu, image.layout.timers, 6), due, "previous firing must supply the next blink due");
        clock_at(&mut cpu, &image, t0, 8 * (due - u64::from(image.layout.w) + 1));
        cpu.pc = image.symbols.pass;
        let start = cpu.cycles;
        let mut trace = Trace::default();
        loop {
            trace.advance(&mut cpu, &image, t0, &plan);
            if !trace.entries.is_empty() && cpu.pc == image.symbols.pass { break; }
        }
        assert_eq!(trace.entries[0].1, entry_start(t0, due), "blink dispatch stays on its absolute grid");
        executed += cpu.cycles - start;
    }
    executed += drain(&mut cpu, &image);
    let writes: Vec<_> = frames(&cpu).into_iter().filter_map(|(frame, tick)| match frame.event {
        watch::Event::Write { pin, level, site } => Some((tick, pin, level, site)), _ => None,
    }).collect();
    assert_eq!(writes.len(), 12, "three-second grid must contain twelve blink frames");
    for (i, &(tick, pin, level, site)) in writes.iter().enumerate() {
        assert_eq!((pin, level, site), (13, i % 2 == 0, 1), "blink frame identity and alternation");
        if i != 0 { assert_eq!(tick - writes[i - 1].0, 500_000, "blink grid must not drift"); }
    }
    println!("dispatch_exact_under_pwm: {executed} executed oracle cycles");
}

#[test]
fn far_overflows_sleep_until_timer_due() {
    use fab_elec_mcu_emit_avr::{build_module, Options};
    use fab_elec_mcu_rt::{module, sched::Device};
    use model::{Model, PROFILE};

    let compiled = fab_elec_mcu::compile("let timer = every(ms(100), fn() {})\n", &PROFILE, &[], &[]).unwrap();
    let m = module::load(&compiled.module, &PROFILE).unwrap();
    let built = build_module(&m, &Options::default()).unwrap();
    let image = &built.image;
    let mut device = Device::new(m, &PROFILE);
    let mut model = Model::default(); device.boot(&mut model);
    let mut starts = Vec::new();
    while device.next_start().is_some_and(|start| start <= 1_000_000_000) {
        starts.push(device.step(&mut model).unwrap().start);
    }
    assert_eq!(starts.len(), 10, "one second contains ten 100 ms firings");
    let (mut cpu, t0) = booted(image);
    // Include the last due's dispatch latency, empty entry's LDI pair 2,
    // RET/JMP 4+3 and the next pass's CLI 1, without another timer period.
    // Timer0 ticks are 500 ns here.
    let end = t0 + 16_000_000 + u64::from(DISPATCH + TOV_INT) + 10;
    let mut passes = BTreeMap::<u64, usize>::new();
    let mut entries = Vec::new();
    while cpu.cycles < end {
        if cpu.will_execute() {
            if cpu.pc == image.symbols.pass { *passes.entry((cpu.cycles - t0) / OVERFLOW).or_default() += 1; }
            if cpu.pc == image.symbols.entries[0] { entries.push(cpu.cycles); }
        }
        if let Err(stop) = cpu.step_until(end) {
            assert_eq!(stop, fab_elec_mcu_avr::sim::Stop::Cycles, "idle run must reach its deadline");
        }
    }
    assert_eq!(entries, starts.iter().map(|ns| entry_start(t0, ns / 500)).collect::<Vec<_>>(),
        "dispatch instants equal sched's starts plus the existing AVR dispatch/IRQ hand count");
    assert_eq!(passes.get(&0), Some(&1), "boot performs one initial pass");
    for overflow in 1..=(end - t0) / OVERFLOW {
        // Admit the two early overflows, the near window and the return's
        // far pass. The latter can cross into the overflow after the due.
        let near = starts.iter().any(|ns| {
            let due = ns / 500;
            ((due - u64::from(image.layout.w)) >> 8) - 2 <= overflow && overflow <= (due >> 8) + 1
        });
        let count = passes.get(&overflow).copied().unwrap_or(0);
        // Shorter real paths may rescan more often inside the near window.
        assert!(near || count == 0,
            "overflow {overflow} has {count} passes: only near dues and their returns may run the pass");
    }
}

#[test]
fn now48_at_overflow() {
    let image = make_image(500_000, true, &blink_entry);
    let (mut cpu, t0) = booted(&image);
    for masked in [false, true] {
        for through_write in [false, true] {
            let mut previous = 0;
            for offset in -40i64..=40 {
                // An unmasked caller cannot begin during service of this
                // overflow. Masked callers cover all 81 requested clocks;
                // unmasked callers cover every reachable entry clock.
                if !masked && (0..i64::from(TOV_INT)).contains(&offset) { continue; }
                if masked {
                    clock_at(&mut cpu, &image, t0, OVERFLOW - 41);
                    cpu.sreg = 0;
                    let pc = scratch(&mut cpu, &image);
                    for _ in 0..offset + 41 { cpu.pc = pc; cpu.step().unwrap(); }
                } else {
                    clock_at(&mut cpu, &image, t0, (OVERFLOW as i64 + offset) as u64);
                }
                cpu.sreg = if masked { 0 } else { 128 };
                cpu.r[16..20].copy_from_slice(&13i32.to_le_bytes());
                cpu.r[20] = 1; cpu.r[24] = 1; cpu.r[25] = 0;
                cpu.write(image.layout.head, 0); cpu.write(image.layout.tail, 0);
                store(&mut cpu, image.layout.lost, 0, 4);
                let target = if through_write { image.symbols.write } else { image.symbols.now48 };
                // Direct now48's calling contract masks before entry; the
                // unmasked caller executes CLI, including its interrupt boundary.
                if !through_write {
                    let pc = scratch(&mut cpu, &image);
                    cpu.flash[pc as usize] = encode(&Bclr(7)).unwrap().0[0]; cpu.pc = pc;
                    while cpu.pc == pc || cpu.pc == 32 || (image.symbols.tov..image.symbols.now48).contains(&cpu.pc) {
                        cpu.step().unwrap();
                    }
                    cpu.sreg &= !128;
                }
                let result = invoke(&mut cpu, &image, target);
                assert_eq!(result.samples.len(), 1, "one stamp per invocation");
                let value = if through_write { load(&cpu, image.layout.stamp, 6) }
                    else { (20..26).map(|r| u64::from(cpu.r[r]) << (8 * (r - 20))).sum() };
                let expected = (result.samples[0] - t0) / 8;
                assert_eq!(value, expected, "overflow stamp masked={masked} write={through_write} offset={offset}");
                assert!(value >= previous, "successive phase entries must never move the stamp backward");
                previous = value;
            }
        }
    }
    // Exercise the high carry independently of a multi-hour simulated run.
    clock_at(&mut cpu, &image, t0, (1u64 << 32) * 8 - 1);
    cpu.sreg = 0;
    let call = invoke(&mut cpu, &image, image.symbols.now48);
    let value: u64 = (20..26).map(|r| u64::from(cpu.r[r]) << (8 * (r - 20))).sum();
    assert_eq!(value, (call.samples[0] - t0) / 8, "pending carry crosses the register count's top byte");
    let interrupted = cpu.pc;
    cpu.sreg = 128;
    cpu.step().unwrap();
    assert_eq!(cpu.pc, 32, "pending high carry must subsequently be served");
    while cpu.pc != interrupted { cpu.step().unwrap(); }
    cpu.sreg = 0;
    for _ in 0..2 {
        let call = invoke(&mut cpu, &image, image.symbols.now48);
        let value: u64 = (20..26).map(|r| u64::from(cpu.r[r]) << (8 * (r - 20))).sum();
        assert_eq!(value, (call.samples[0] - t0) / 8, "served high carry must retain the full tick");
        assert_eq!(load(&cpu, image.layout.clock_hi, 2), 1, "bookkeeping must commit the high carry exactly once");
        assert_eq!(cpu.read(image.layout.clock_b31), 0, "bookkeeping must remember the new bit 31");
    }
}

#[test]
fn delay_exact() {
    let image = make_image(500_000, false, &blink_entry);
    let (mut cpu, t0) = booted(&image);
    let maximum = 8 * image.layout.w + 64;
    let mut values: BTreeSet<_> = (N_MIN..N_MIN + 8).chain(maximum - 7..=maximum).collect();
    let mut q = 2;
    while DELAY_BASE + 4 * q <= maximum {
        // All four low-bit arms on both sides of each counter bit transition,
        // including the 8/16-bit loop-counter and SUBI/SBCI borrow boundaries.
        values.extend(DELAY_BASE + 4 * q - 1..=DELAY_BASE + 4 * q + 4);
        q *= 2;
    }
    let mut byte = 256;
    while byte <= maximum { values.extend(byte - 2..=byte + 3); byte *= 2; }
    let mut executed = t0;
    for n in values.into_iter().filter(|&n| n <= maximum) {
        clock_at(&mut cpu, &image, t0, 0);
        cpu.write(0x6e, 0);
        cpu.r[24] = n as u8; cpu.r[25] = (n >> 8) as u8; cpu.r[23] = (n >> 16) as u8;
        let call = invoke(&mut cpu, &image, image.symbols.delay);
        executed += call.clocks;
        assert_eq!(call.clocks, u64::from(n), "delay remainder/counter boundary n={n}");
    }
    for k in [1u64, 2] {
        let n = k * OVERFLOW;
        clock_at(&mut cpu, &image, t0, 64);
        cpu.r[24] = n as u8; cpu.r[25] = (n >> 8) as u8; cpu.r[23] = (n >> 16) as u8;
        let call = invoke(&mut cpu, &image, image.symbols.delay);
        executed += call.clocks;
        assert_eq!(call.services as u64, k, "chosen start phase puts exactly {k} overflows inside delay");
        assert_eq!(call.clocks, n + k * u64::from(TOV_INT), "interruptions add the exact overflow cost");
    }
    println!("delay_exact: {executed} executed oracle cycles");
}

#[test]
fn masked_stretch() {
    let burst = |a: &mut Asm, s: &Stubs| { for _ in 0..8 { blink_entry(a, s); } };
    let setup = |a: &mut Asm, s: &Stubs| {
        // Exercise every other stamp with interrupts enabled. Timer one is a
        // slow timer; stopping/restarting it leaves the stress timer intact.
        for target in [s.read, s.duty, s.clock_us, s.clock_ms, s.timer_stop, s.timer_start, s.timer_period, s.serial_byte] {
            a.emit(Ldi(16, if target == s.read { 13 } else if target == s.duty { 9 } else { 1 }));
            for r in 17..20 { a.emit(Ldi(r, 0)); }
            for (r, byte) in (20..24).zip(50_000u32.to_le_bytes()) { a.emit(Ldi(r, byte)); }
            a.emit(Ldi(24, 0)); a.emit(Ldi(25, 0)); a.jump(Target::Call, target);
        }
    };
    let mut executed = 0;
    // Four timers exercise a queued Hello and a filling ring. Eighty timers
    // exercise a long bounded scan across clock phases.
    for count in [4, 80] {
        let image = build(&Program {
            state_init: vec![0], pins: vec![PinDef { pin: 13, role: Role::Output },
                PinDef { pin: 9, role: Role::Pwm(pwm_setting(1, 320, 64)) }],
            scratch_bytes: 1280u16.saturating_sub(count * 12),
            timers: (0..count).map(|k| TimerDef { entry: 0, period_ticks: if k == 0 { 4 } else { 100_000 }, repeat: true }).collect(),
            serial: (count == 4).then_some(SerialDef { entry: 1, setting: rx::SETTING, fifo: 3 }),
            entries: vec![&burst, &blink_entry], setup: Some(&setup), ..Program::default()
        }).unwrap();
        let (mut cpu, t0) = booted(&image);
        if count == 4 { for byte in [1, 2, 3, 4] { cpu.rx(byte); } }
        let mut masks = MaskTrace::new(&image);
        while cpu.cycles < t0 + 80 * OVERFLOW {
            masks.step(&mut cpu);
        }
        assert!(load(&cpu, image.layout.lost, 4) != 0, "short timer must fill the bounded ring");
        assert!(!pins(&cpu).is_empty(), "ring loss must not suppress pin writes");
        while cpu.pc != image.symbols.pass { masks.step(&mut cpu); }
        for k in 0..count { store(&mut cpu, image.layout.timers + k * 12, DISARMED, 6); }
        // The saturated near path deliberately keeps UDRIE off. Disarming
        // after the burst exercises the foreground encoder and publication.
        let end = cpu.cycles + 4 * OVERFLOW;
        while cpu.cycles < end { masks.step(&mut cpu); }
        masks.check(count);
        executed += cpu.cycles;
    }
    println!("masked_stretch: {executed} executed oracle cycles");
}

struct MaskTrace {
    starts: BTreeMap<u32, &'static str>,
    expected: BTreeMap<&'static str, (u64, u64)>,
    seen: BTreeMap<&'static str, BTreeSet<u64>>,
    active: Option<(&'static str, u64)>,
    stretch: u64,
    longest: u64,
    chain: Vec<&'static str>,
    longest_chain: Vec<&'static str>,
}

impl MaskTrace {
    fn new(image: &Image) -> Self {
        let mut starts = BTreeMap::from([(image.symbols.pass, "NOW48 + CALL"), (image.symbols.lock, "LOCK")]);
        starts.insert(image.symbols.idle_mask, "idle check");
        starts.insert(image.symbols.tx_publish, "TX publication");
        // Budgets include CLI; measured I-clear clocks begin at its completion.
        // L also includes the four I-set clocks of the SEI-deferred CALL.
        let mut expected = BTreeMap::from([
            ("NOW48 + CALL", (u64::from(NOW48 + 6), u64::from(NOW48 + 5))),
            ("LOCK", (u64::from(LOCK), u64::from(LOCK - 1))),
            ("L", (u64::from(L), u64::from(L - 5))),
            ("TOV_INT", (u64::from(TOV_INT), u64::from(TOV_INT))),
            ("UDRE", (u64::from(image.bounds.udre), u64::from(image.bounds.udre))),
            ("TX publication", (u64::from(TX_PUBLISH), u64::from(TX_PUBLISH - 1))),
        ]);
        for (name, entry) in [("write stamp", image.symbols.write), ("read stamp", image.symbols.read),
            ("duty stamp", image.symbols.duty),
            ("clock_us stamp", image.symbols.clock_us), ("clock_ms stamp", image.symbols.clock_ms),
            ("timer_stop stamp", image.symbols.timer_stop), ("timer_start stamp", image.symbols.timer_start),
            ("timer_period stamp", image.symbols.timer_period), ("serial_byte stamp", image.symbols.serial_byte)] {
            let mut pc = entry;
            loop {
                let (insn, width) = decode(&image.words[pc as usize..]).unwrap();
                if insn == Bclr(7) { starts.insert(pc, name); break; }
                pc += width as u32;
                assert!(pc - entry < 64, "stub must have its bounded stamp prefix");
            }
            expected.insert(name, (u64::from(NOW48 + 8), u64::from(NOW48 + 7)));
        }
        for &cli in &image.symbols.dispatch {
            assert_eq!(decode(&image.words[cli as usize..]), Some((Bclr(7), 1)), "dispatch must start with CLI");
            starts.insert(cli, "L");
        }
        if image.layout.rx_depth != 0 {
            starts.insert(image.symbols.rx_scan, "RX scan");
            starts.insert(image.symbols.rx_take, "RX take");
            starts.insert(image.symbols.rx_defer, "RX defer");
            expected.insert("RX scan", (u64::from(image.bounds.rx_scan_mask), u64::from(image.bounds.rx_scan_mask - 1)));
            expected.insert("RX take", (u64::from(image.bounds.rx_take), u64::from(image.bounds.rx_take - 1)));
            expected.insert("RX_INT", (u64::from(image.bounds.rx_int), u64::from(image.bounds.rx_int)));
            expected.insert("RX defer", (u64::from(RX_DEFER_MASK), u64::from(RX_DEFER_MASK - 1)));
        }
        // Exclude the unmasked fast check and final SLEEP/RJMP from the whole-path
        // bound. I-clear also excludes CLI; early exits may be shorter.
        let idle_mask = image.bounds.idle_check - 3 - if image.layout.rx_depth == 0 { 0 } else { 5 };
        expected.insert("idle check", (u64::from(idle_mask), u64::from(idle_mask - 1)));
        Self { starts, expected, seen: BTreeMap::new(), active: None,
            stretch: 0, longest: 0, chain: Vec::new(), longest_chain: Vec::new() }
    }

    fn step(&mut self, cpu: &mut Cpu) {
        let (pc, start, sp, enabled) = (cpu.pc, cpu.cycles, cpu.sp, cpu.sreg & 128 != 0);
        cpu.step().expect("masked-block instruction must execute");
        let irq = matches!(cpu.pc, 32 | 36 | 38) && cpu.sp + 2 == sp;
        if irq {
            let name = match cpu.pc { 32 => "TOV_INT", 36 => "RX_INT", _ => "UDRE" };
            assert!(enabled && self.active.is_none(), "interrupt cannot nest inside a masked block");
            if cpu.cycles - start > 4 { self.stretch = 0; self.chain.clear(); }
            self.active = Some((name, cpu.cycles - 4));
            self.chain.push(name); self.stretch += 4;
        } else if enabled {
            // Only an actually executed I-set instruction separates stretches.
            // OUT restoring I followed immediately by an IRQ has no such gap.
            self.stretch = 0; self.chain.clear();
            if cpu.sreg & 128 == 0 {
                let name = *self.starts.get(&pc).unwrap_or_else(|| panic!("unnamed masked block at PC {pc}"));
                self.active = Some((name, cpu.cycles)); self.chain.push(name);
            }
        } else {
            if let Some(&name) = self.starts.get(&pc) {
                if self.active.is_some_and(|(active, _)| active != name) { self.chain.push(name); }
                if name == "RX defer" {
                    let (_, begin) = self.active.expect("rollback must begin inside the dispatch mask");
                    self.active = Some((name, begin));
                }
            }
            self.stretch += cpu.cycles - start;
            if cpu.sreg & 128 != 0 {
                if let Some((name, begin)) = self.active.take() {
                    self.seen.entry(name).or_default().insert(cpu.cycles - begin);
                }
            }
        }
        if self.stretch > self.longest {
            self.longest = self.stretch; self.longest_chain.clone_from(&self.chain);
        }
    }

    fn check(&self, timers: u16) {
        let largest = self.expected.values().map(|&(_, clear)| clear).max().unwrap();
        println!("{timers} timers: longest I-clear={} via {:?}; largest named block={largest}", self.longest, self.longest_chain);
        assert!(self.longest <= largest,
            "masked stretch: measured {} exceeds bound {largest} via {:?}", self.longest, self.longest_chain);
        for (&name, &(budget, clear)) in &self.expected {
            assert!(budget < 2040 && clear < 2040, "{name} must have a bound below 2040");
            // This stress has never required a rollback; when one occurs,
            // measure it separately so the clear dispatch stays exactly L.
            if name == "RX defer" && !self.seen.contains_key(name) { continue; }
            let seen = self.seen.get(name).unwrap_or_else(|| panic!("stress did not execute {name}"));
            if matches!(name, "UDRE" | "idle check" | "RX scan" | "RX take" | "RX_INT" | "RX defer") {
                for &n in seen { assert!(n <= clear, "{name}: measured {n} I-clear cycles exceeds bound {clear}"); }
            } else { assert_eq!(seen, &BTreeSet::from([clear]), "{name} must have one constant masked length"); }
            println!("{timers} timers: {name}: budget={budget}, I-clear={seen:?}");
        }
    }
}

fn preserved(cpu: &Cpu) -> Vec<u8> {
    [2usize].into_iter().chain(6..16).chain([28, 29]).map(|r| cpu.r[r]).collect()
}

#[test]
fn clocks_at_cpu_frequencies() {
    for f_clk in [1_000_000, 8_000_000, 14_745_600, 16_000_000, 18_432_000, 20_000_000] {
        let tb = Timebase::new(f_clk);
        if f_clk == 16_000_000 {
            assert_eq!((tb.us, tb.ms), ((1, 2), (1, 2000)), "16 MHz reduced ratios");
        }
        let returns = std::cell::Cell::new([0; 2]);
        let entry = |a: &mut Asm, s: &Stubs| {
            a.jump(Target::Call, s.clock_us);
            let us_return = a.here();
            for r in 16..20 { a.emit(Sts(s.state + u16::from(r - 16), r)); }
            a.jump(Target::Call, s.clock_ms);
            returns.set([us_return, a.here()]);
            for r in 16..20 { a.emit(Sts(s.state + 4 + u16::from(r - 16), r)); }
        };
        let program = Program { f_clk, state_init: vec![0; 8], entries: vec![&entry],
            watch: watch::Level::Off, ..Program::default() };
        let image = build(&program).unwrap_or_else(|why| panic!("clock image at {f_clk} Hz: {why}"));
        let totals = [image.bounds.clock_us, image.bounds.clock_ms];
        let machine = fab_elec_mcu_avr::machine::AvrMachine::new(&image.words, u64::from(f_clk));
        let mut cpu = machine.cpu().clone();
        to_symbol(&mut cpu, image.symbols.t0, 30_000);
        let t0 = cpu.cycles;
        let wrap = ((1u128 << 32) * u128::from(tb.us.1)).div_ceil(u128::from(tb.us.0)) as u64;
        let mut ticks = vec![0, wrap - 1, wrap, wrap + 1, (1u64 << 48) - 1];
        for (n, m) in [tb.us, tb.ms] {
            ticks.extend([u64::from(m) - 1, u64::from(m), u64::from(m) + 1]);
            if n == 1 && !m.is_power_of_two() {
                // The longest narrow-divisor path subtracts for each set quotient
                // bit: choose the largest all-ones quotient that fits a 48-bit tick count.
                let max = ((1u64 << 48) - 1) / u64::from(m);
                let bits = 63 - (max + 1).leading_zeros();
                ticks.push(((1u64 << bits) - 1) * u64::from(m));
            }
        }
        ticks.sort_unstable(); ticks.dedup();
        for ticks in ticks {
            clock_at(&mut cpu, &image, t0, ticks * 8);
            // Freeze the fixture's clock at the exact requested stamp. This
            // also covers 2^48-1 without wrapping during the entry's CALLs;
            // no IRQ service belongs in uninterrupted conversion totals.
            cpu.write(0x43, 0x83); cpu.write(0x6e, 0);
            for r in 3..13 { cpu.r[r] = 0x90 + r as u8; }
            cpu.r[28..30].copy_from_slice(&0x0145u16.to_le_bytes());
            let kept = cpu.r;
            let sp = cpu.sp;
            let done = scratch(&mut cpu, &image) + 1;
            return_to(&mut cpu, done); cpu.pc = image.symbols.entries[0];
            let start = cpu.cycles;
            let mut starts = [None; 2];
            let mut measured = [None; 2];
            let targets = [image.symbols.clock_us, image.symbols.clock_ms];
            while cpu.pc != done || cpu.sp != sp {
                assert!(cpu.cycles - start < u64::from(totals[0] + totals[1]) + 32,
                    "clock entry must return at {f_clk} Hz, ticks={ticks}");
                for k in 0..2 {
                    if cpu.pc == targets[k] { starts[k] = Some(cpu.cycles); }
                    if cpu.pc == returns.get()[k] {
                        measured[k] = starts[k].map(|at| cpu.cycles - at);
                    }
                }
                cpu.step().unwrap_or_else(|why| panic!("clock entry must execute at {f_clk} Hz, ticks={ticks}: {why:?}"));
            }
            for (k, (unit, (n, m))) in [("us", tb.us), ("ms", tb.ms)].into_iter().enumerate() {
                let expected = (u128::from(ticks) * u128::from(n) / u128::from(m)) as u32;
                assert_eq!(load(&cpu, image.layout.state + 4 * k as u16, 4) as u32, expected,
                    "clock_{unit} low 32 bits at {f_clk} Hz, ticks={ticks}");
                within(&format!("clock_{unit} at {f_clk} Hz, ticks={ticks}"),
                    measured[k].expect("clock stub must return"), totals[k]);
            }
            assert_eq!(load(&cpu, image.layout.stamp, 6), ticks,
                "full 48-bit stamp at {f_clk} Hz, ticks={ticks}");
            assert_eq!((&cpu.r[2..16], &cpu.r[28..30]), (&kept[2..16], &kept[28..30]),
                "reserved registers and Y at {f_clk} Hz, ticks={ticks}");
            assert_ne!(cpu.sreg & 128, 0, "clock stubs restore I at {f_clk} Hz, ticks={ticks}");
        }
    }
}

#[test]
fn window_covers_long_passes() {
    let empty = |_: &mut Asm, _: &Stubs| {};
    // A descending timer scan takes every selection arm. Serial covers both
    // an empty scan and a queued byte before the timer, including SerialLost.
    // W-1, W and W+1 take the less/equal/greater far-threshold arms; the
    // W+1 queued byte also takes the overdue arm before redispatching the timer.
    for watch in [watch::Level::Ports, watch::Level::Faults, watch::Level::Off] {
    for count in [1usize, 4, 80] { for shape in 0..3 { for repeat in [false, true] {
        let image = build(&Program { watch,
            timers: (0..count).map(|_| TimerDef { entry: 0, period_ticks: 65536, repeat }).collect(),
            serial: (shape != 0).then_some(SerialDef { entry: 1, fifo: 1, setting: rx::SETTING }),
            entries: vec![&empty, &empty], ..Program::default()
        }).unwrap();
        let l = &image.layout;
        // Room at the ring's end exercises wrapped appends; head < tail
        // exercises wrapped occupancy. Full covers refused Time with both
        // saturated Lost and its longer increment-across-saturation arm.
        for ring in 0..if watch == watch::Level::Off { 1 } else { 4 } { for margin in [-1i64, 0, 1] {
            let (mut cpu, t0) = booted(&image);
            let epoch = (1u64 << 32) + (32 + 256 - u64::from(l.w) % 256) % 256;
            clock_at(&mut cpu, &image, t0, 8 * epoch);
            cpu.pc = image.symbols.pass;
            let head = if l.records == 0 || ring == 1 { 0 } else { l.records - 1 };
            if watch != watch::Level::Off {
                cpu.write(l.hello_pending, 0);
                cpu.write(l.head, head);
                cpu.write(l.tail, if ring == 0 { head } else if ring == 1 { 4 } else { 0 });
                store(&mut cpu, l.lost, if ring == 2 { u64::from(u32::MAX) }
                    else if ring == 3 { 0x7fff_ffff } else { 7 }, 4);
                store(&mut cpu, l.lost_first, epoch - 2, 4); store(&mut cpu, l.lost_last, epoch - 1, 4);
                store(&mut cpu, l.time_sent, 0, 3);
            }
            let mut trace = Trace::default();
            while trace.now_samples.is_empty() { trace.step(&mut cpu, &image, t0); }
            // Set due from the completed pass sample before the timer scan.
            let stamp = (trace.now_samples[0] - t0) / 8;
            let due = (stamp as i64 + i64::from(l.w) + margin) as u64;
            for k in 0..count {
                store(&mut cpu, l.timers + k as u16 * 12, due + (count - k - 1) as u64, 6);
            }
            if shape == 2 {
                store(&mut cpu, l.rx_count, 1, 2);
                cpu.write(l.rx_fifo, 99); store(&mut cpu, l.rx_fifo + 1, stamp - u64::from(margin > 0), 6);
                store(&mut cpu, l.rx_lost, u64::from(u32::MAX), 4); cpu.write(l.rx_overrun, 1);
            }
            let expected = t0 + 8 * due + u64::from(DISPATCH);
            // Check the producer state before a far pass can enable the drain
            // or byte bookkeeping can add records to this same full ring.
            while cpu.pc != image.symbols.lock && cpu.pc != image.symbols.sleep {
                assert!(cpu.cycles <= expected + OVERFLOW, "W fixture must finish its first prefix");
                trace.step(&mut cpu, &image, t0);
            }
            let far = cpu.pc == image.symbols.sleep;
            assert_eq!(far, margin > 0 && shape != 2, "W boundary must take the intended near/far arm");
            let bound = image.bounds.pass_prefix + if shape != 0 { image.bounds.rx_scan } else { 0 }
                + if far { 2 + image.bounds.far - 1 } else { 0 };
            within("first pass through near branch or far SEI",
                cpu.cycles - trace.pass - service_clocks(&trace, trace.pass, cpu.cycles), bound);
            if watch == watch::Level::Off { assert_unobserved_image(&image); }
            else {
                assert_eq!(load(&cpu, l.time_sent, 3), stamp >> 30, "pass must take the new Time arm");
                if ring < 2 {
                    let record = |offset: u16| l.ring + ((u16::from(head) + offset) % u16::from(l.records)) * 8;
                    assert_eq!([cpu.read(record(0)), cpu.read(record(1)), cpu.read(record(2))],
                        [watch::LOST, watch::LOST | 0x80, watch::TIME], "long push flushes Lost before Time");
                } else {
                    assert_eq!(cpu.read(l.head), head, "full ring must refuse records without blocking dispatch");
                    assert_eq!(load(&cpu, l.lost, 4), u64::from(u32::MAX),
                        "refused Time saturates or retains saturated Lost");
                }
            }
            while !trace.entries.iter().any(|e| e.0 == 0) {
                assert!(cpu.cycles <= expected + OVERFLOW, "W fixture must reach its timer");
                trace.step(&mut cpu, &image, t0);
            }
            assert_eq!(trace.entries.iter().map(|e| e.0).collect::<Vec<_>>(),
                if shape == 2 { vec![1, 0] } else { vec![0] }, "W pass shape must be exercised");
            assert_eq!(trace.entries.last().unwrap().1, expected,
                "W must cover count={count} shape={shape} repeat={repeat} ring={ring} margin={margin}, W={}", l.w);
            let lock = trace.locks.last().unwrap();
            assert!(lock.delta >= 0 && lock.raw >= i64::from(N_MIN), "W must leave an on-time delay");
            assert_eq!(lock.selected as usize, count - 1, "descending scan selects its last timer");
            if shape == 2 {
                assert_eq!((load(&cpu, l.rx_report, 4), cpu.read(l.rx_report_overrun)),
                    (u64::from(u32::MAX), 1), "byte prelude must take the SerialLost arm");
            }
        } }
    } } }
    }
    // Cross far_until at the first encode instruction with a drained TX
    // buffer and Hello pending (the longest actual wire payload). The check
    // just missed the threshold, so W must pay for encoding before rescanning.
    for watch in [watch::Level::Faults, watch::Level::Ports] {
    for count in [1usize, 80] { for serial in [false, true] {
        let image = build(&Program { watch,
            timers: (0..count).map(|_| TimerDef { entry: 0, period_ticks: 65536, repeat: false }).collect(),
            serial: serial.then_some(SerialDef { entry: 1, fifo: 1, setting: rx::SETTING }),
            entries: vec![&empty, &empty], ..Program::default()
        }).unwrap();
        let (mut cpu, t0) = booted(&image);
        clock_at(&mut cpu, &image, t0, OVERFLOW - 1);
        let l = &image.layout;
        // far_until = ((due-W)>>8)-2 = 1, reached on the next CPU clock.
        let due = u64::from(l.w) + 3 * 256;
        for k in 0..count { store(&mut cpu, l.timers + k as u16 * 12, due + (count - k - 1) as u64, 6); }
        store(&mut cpu, l.far_until, 1, 3);
        cpu.write(l.tx_pos, 0); cpu.write(l.tx_len, 0); cpu.write(l.hello_pending, 1);
        cpu.write(0xc1, if serial { 0xb8 } else { 0x28 });
        cpu.pc = image.symbols.encode; cpu.r[18] = 1;
        let start = cpu.cycles;
        let mut trace = Trace::default();
        while cpu.pc != image.symbols.idle_check { trace.step(&mut cpu, &image, t0); }
        within("encode across the far threshold", cpu.cycles - start - service_clocks(&trace, start, cpu.cycles), image.bounds.encode);
        assert_eq!(cpu.read(l.hello_pending), 0, "Hello was encoded before the threshold recheck");
        assert!(trace.services.iter().any(|s| s.vector == 38), "an empty pump must interrupt the encoder");
        let expected = t0 + 8 * due + u64::from(DISPATCH);
        while trace.entries.is_empty() {
            assert!(cpu.cycles <= expected + OVERFLOW, "encode case must reach the handler");
            trace.step(&mut cpu, &image, t0);
        }
        assert_eq!(trace.entries, [(0, expected)], "encode must not make count={count}, serial={serial}, {watch:?} late");
        assert!(trace.locks.last().unwrap().raw >= i64::from(N_MIN), "encode leaves an on-time delay");
    } }
    }
}

#[test]
fn push_bounds() {
    for level in [watch::Level::Off, watch::Level::Faults, watch::Level::Ports] {
        let image = build(&Program { watch: level, ..Program::default() }).unwrap();
        let (mut cpu, _) = booted(&image);
        cpu.sreg = 0; cpu.write(0x6e, 0); cpu.write(0xc1, 0);
        let l = &image.layout;
        if level == watch::Level::Off {
            assert_unobserved_image(&image);
            continue;
        }
        for count in 0..=2u8 { for room in [l.records - 1, 1, 0] {
            for lost in [0, 7, 0x7fff_ffff, u32::MAX] { for head in [0, l.records - 1] {
                let case = format!("push {level:?} count={count} room={room} lost={lost} head={head}");
                cpu.write(l.head, head);
                cpu.write(l.tail, ((u16::from(head) + 1 + u16::from(room)) % u16::from(l.records)) as u8);
                store(&mut cpu, l.lost, u64::from(lost), 4);
                store(&mut cpu, l.lost_first, 1, 4); store(&mut cpu, l.lost_last, 2, 4);
                let records = [if count == 2 { watch::FAULT } else { watch::WRITE }, 3, 0, 0, 0,
                    watch::FAULT_PIN, 0x34, 0x12, watch::FAULT | 0x80, 4, 5, 6, 7, 0, 0, 0];
                for (offset, byte) in records.into_iter().enumerate() { cpu.write(l.record + offset as u16, byte); }
                cpu.write(l.record_count, count);
                let call = invoke(&mut cpu, &image, image.symbols.push);
                within(&case, call.clocks, image.bounds.push);
                let needed = count + if lost != 0 { 2 } else { 0 };
                if needed > room {
                    let expected = if count == 0 { lost } else if lost >= 0x7fff_ffff { u32::MAX } else { lost + 1 };
                    assert_eq!(load(&cpu, l.lost, 4), u64::from(expected), "{case}: refused event increments or saturates loss");
                    assert_eq!(cpu.read(l.head), head, "{case}: refusal appends no partial event");
                    assert_eq!(load(&cpu, l.lost_first, 4), if lost == 0 && count != 0 { 3 } else { 1 },
                        "{case}: only the first refused event starts the loss range");
                    assert_eq!(load(&cpu, l.lost_last, 4), if count != 0 { 3 } else { 2 },
                        "{case}: an empty push counts no new loss");
                } else {
                    assert_eq!(load(&cpu, l.lost, 4), 0, "{case}: room flushes Lost first");
                    let record = |offset: u8| l.ring + ((u16::from(head) + u16::from(offset)) % u16::from(l.records)) * 8;
                    assert_eq!(cpu.read(l.head), ((u16::from(head) + u16::from(needed)) % u16::from(l.records)) as u8,
                        "{case}: all records advance head across the wrap");
                    if lost != 0 {
                        assert_eq!(cpu.read(record(0)), watch::LOST, "{case}: Lost precedes the event");
                        assert_eq!(load(&cpu, record(0) + 1, 4), 2, "{case}: Lost carries the last tick");
                        assert_eq!(load(&cpu, record(0) + 5, 3), u64::from(lost & 0xff_ffff), "{case}: Lost count low bytes");
                        assert_eq!(cpu.read(record(1)), watch::LOST | 0x80, "{case}: Lost continuation precedes the event");
                        assert_eq!(load(&cpu, record(1) + 1, 4), 1, "{case}: Lost carries the first tick");
                        assert_eq!(cpu.read(record(1) + 5), (lost >> 24) as u8, "{case}: Lost retains saturation");
                    }
                    for offset in 0..count {
                        let at = record(offset + if lost != 0 { 2 } else { 0 });
                        for byte in 0..8u16 {
                            assert_eq!(cpu.read(at + byte), records[usize::from(offset) * 8 + usize::from(byte)],
                                "{case}: event content and order survive append");
                        }
                    }
                }
            } }
        } }
    }
}

#[test]
fn off_fault_cells_saturate() {
    use fab_elec_mcu_rt::ports;
    // One raising path uses the inline increment; multiple paths share the bounded routine.
    for ports in [1 << ports::LOOP_CAP, Program::default().ports] {
        let image = build(&Program { ports, watch: watch::Level::Off, ..Program::default() }).unwrap();
        assert_unobserved_image(&image);
        assert_eq!(image.symbols.fault != 0, image.layout.fault_cells.len() > 1, "share only when it saves space");
        let (mut cpu, _) = booted(&image);
        let cell = image.layout.fault_cells.iter().find(|cell| cell.code == watch::FAULT_LOOP_CAP).unwrap();
        cpu.sreg = 0;
        for count in [0, 0xfffe, 0xffff] {
            store(&mut cpu, cell.addr as u16, count, 2);
            cpu.r[16..20].copy_from_slice(&0x12345678u32.to_le_bytes());
            cpu.r[24..26].copy_from_slice(&(count as u16 ^ 0x5678).to_le_bytes());
            let saved = cpu.r;
            let call = invoke(&mut cpu, &image, image.symbols.loop_cap);
            within("Off LoopCap", call.clocks, image.bounds.loop_cap);
            assert_eq!(fault_cell(&cpu, &image, watch::FAULT_LOOP_CAP),
                ((count + 1).min(65535) as u16, count as u16 ^ 0x5678), "count saturates; last site still updates");
            assert_eq!(&cpu.r[16..20], &saved[16..20], "fault counting preserves A");
            assert_eq!(&cpu.r[2..16], &saved[2..16], "fault counting preserves runtime registers");
            assert_eq!(&cpu.r[28..30], &saved[28..30], "fault counting preserves Y");
            assert!(call.samples.is_empty(), "LoopCap has no clock effect");
            for other in &image.layout.fault_cells {
                if other.code != cell.code { assert_eq!(fault_cell(&cpu, &image, other.code), (0, 0), "other codes are independent"); }
            }
        }
    }
}

#[test]
fn foreground_frame_bounds() {
    // Lost has the largest ring payload; Fault also consumes a continuation.
    // Rx must shorten the payload even when its unused record bytes are nonzero.
    let cases = [
        (watch::Event::Lost { count: 0x04030201, saturated: false, first_tick: 0x00010200 },
            [[watch::LOST, 3, 2, 1, 0, 1, 2, 3], [watch::LOST | 0x80, 0, 2, 1, 0, 4, 0, 0]], 2),
        (watch::Event::Fault { code: watch::FAULT_PIN, site: 0x1234, arg: 0x01000203 },
            [[watch::FAULT, 3, 2, 1, 0, watch::FAULT_PIN, 0x34, 0x12], [watch::FAULT | 0x80, 3, 2, 0, 1, 0, 0, 0]], 2),
        (watch::Event::Rx { byte: 0xa5 }, [[watch::RX, 3, 2, 1, 0, 0xa5, 0xa5, 0xa5], [0; 8]], 1),
    ];
    for level in [watch::Level::Faults, watch::Level::Ports] {
        let image = build(&Program { watch: level, ..Program::default() }).unwrap();
        let (mut cpu, _) = booted(&image);
        cpu.write(0x6e, 0); cpu.write(0xc4, 0); cpu.write(0xc5, 0); cpu.write(0xc0, 0x42);
        let l = &image.layout;
        for (event, records, count) in cases { for tail in [0, l.records - 1] {
            let case = format!("encode {level:?} {event:?} tail={tail}");
            cpu.sreg = 0; cpu.write(0xc1, 8); cpu.events.clear();
            cpu.write(l.hello_pending, 0); cpu.write(l.seq, 0);
            cpu.write(l.tx_pos, 0); cpu.write(l.tx_len, 0);
            let index = |offset: u8| ((u16::from(tail) + u16::from(offset)) % u16::from(l.records)) as u8;
            cpu.write(l.tail, tail); cpu.write(l.head, index(count));
            for offset in 0..count {
                let at = l.ring + u16::from(index(offset)) * 8;
                for (byte, value) in records[usize::from(offset)].into_iter().enumerate() {
                    cpu.write(at + byte as u16, value);
                }
            }
            let clocks = encode_frame(&mut cpu, &image);
            within(&case, clocks, image.bounds.encode);
            assert_eq!(cpu.read(l.tail), cpu.read(l.head), "{case}: the complete event leaves the ring");
            drain(&mut cpu, &image);
            let frame = watch::Frame { kind: records[0][0], seq: 0, tick: 0x00010203, event };
            assert_eq!(frames(&cpu), [(frame, u64::from(frame.tick))], "{case}: decoded frame retains its fields");
            let mut wire = [0; watch::MAX_WIRE];
            let n = watch::encode(&frame, &mut wire);
            let transmitted: Vec<_> = cpu.events.iter().filter_map(|event| match event {
                Event::Tx { byte, .. } => Some(*byte), _ => None,
            }).collect();
            assert_eq!(transmitted, wire[..n], "{case}: firmware and host wire bytes agree");
        } }
    }
}

#[test]
fn foreground_crc_all_byte_inputs() {
    for level in [watch::Level::Faults, watch::Level::Ports] {
        let image = build(&Program { watch: level, image_id: 0x12345678, ..Program::default() }).unwrap();
        let l = &image.layout;
        let (mut cpu, _) = booted(&image);
        cpu.write(0x6e, 0); cpu.write(0xc1, 8);
        let check = |cpu: &Cpu, frame: watch::Frame| {
            let wire: Vec<_> = (0..cpu.read(l.tx_len)).map(|i| cpu.read(l.tx + u16::from(i))).collect();
            let mut expected = [0; watch::MAX_WIRE];
            let n = watch::encode(&frame, &mut expected);
            assert_eq!(wire, expected[..n], "{level:?}: firmware and host wire bytes agree for {frame:?}");
            let mut body = [0; watch::MAX_BODY];
            let n = watch::cobs_decode(&wire[..wire.len() - 1], &mut body).expect("emitted COBS must decode");
            assert_eq!(body[n - 1], watch::crc8(&body[..n - 1]), "{level:?}: emitted CRC covers the entire body");
            // The last body byte enters the CRC after this prefix. Checking
            // decoded bytes makes coverage depend on what the encoder sent.
            watch::crc8(&body[..n - 2]) ^ body[n - 2]
        };
        let hello = watch::Frame { kind: watch::HELLO, seq: 0, tick: 0,
            event: watch::Event::Hello { proto: watch::PROTO, ports_version: PORTS_VERSION,
                image: 0x12345678, clock_hz: 16_000_000, prescale: PRESCALE,
                ring: u16::from(l.records), tick_hi: 0, mcusr: cpu.read(l.mcusr),
                wdtcsr: cpu.read(l.wdtcsr), level, generation: 0 } };
        let clocks = encode_frame(&mut cpu, &image);
        within("CRC coverage Hello", clocks, image.bounds.encode);
        check(&cpu, hello);
        let mut seen = [false; 256];
        for input in 0..=u8::MAX {
            // Choose Rx's payload so crc(prefix) ^ byte is exactly input.
            // Its seven-byte body also exercises the absent-slot skip after
            // the full Hello, including nonzero unused record bytes.
            let seq = cpu.read(l.seq);
            let byte = watch::crc8(&[watch::RX, seq, 0, 0, 0, 0]) ^ input;
            cpu.sreg = 0; cpu.write(0xc1, 8);
            cpu.write(l.tx_pos, 0); cpu.write(l.tx_len, 0);
            cpu.write(l.tail, 0); cpu.write(l.head, 1);
            for (offset, value) in [watch::RX, 0, 0, 0, 0, byte, 0xa5, 0xa5].into_iter().enumerate() {
                cpu.write(l.ring + offset as u16, value);
            }
            let clocks = encode_frame(&mut cpu, &image);
            within("CRC coverage Rx", clocks, image.bounds.encode);
            let actual = check(&cpu, watch::Frame { kind: watch::RX, seq, tick: 0, event: watch::Event::Rx { byte } });
            assert_eq!(actual, input, "{level:?}: chosen byte reaches the intended CRC input");
            seen[usize::from(actual)] = true;
        }
        assert!(seen.into_iter().all(|hit| hit), "{level:?}: emitted CRC must cover all 256 crc ^ byte values");
    }
}

#[test]
fn pump_at_every_encode_and_publication_instruction() {
    for level in [watch::Level::Faults, watch::Level::Ports] { for hello in [false, true] {
        let image = build(&Program { watch: level, image_id: 0x12345678, ..Program::default() }).unwrap();
        let l = &image.layout;
        let (mut baseline, _) = booted(&image);
        baseline.write(0x6e, 0); baseline.write(0xc1, 8);
        baseline.write(l.tx_pos, 7); baseline.write(l.tx_len, 7);
        baseline.write(l.hello_pending, u8::from(hello));
        let frame = if hello {
            watch::Frame { kind: watch::HELLO, seq: 0, tick: 0,
                event: watch::Event::Hello { proto: watch::PROTO, ports_version: PORTS_VERSION,
                    image: 0x12345678, clock_hz: 16_000_000, prescale: PRESCALE,
                    ring: u16::from(l.records), tick_hi: 0, mcusr: baseline.read(l.mcusr),
                    wdtcsr: baseline.read(l.wdtcsr), level, generation: 0 } }
        } else {
            baseline.write(l.tail, l.records - 1); baseline.write(l.head, 1);
            for (at, record) in [(l.ring + u16::from(l.records - 1) * 8, [watch::LOST, 3, 2, 1, 0, 1, 2, 3]),
                (l.ring, [watch::LOST | 0x80, 0, 2, 1, 0, 4, 0, 0])] {
                for (offset, byte) in record.into_iter().enumerate() { baseline.write(at + offset as u16, byte); }
            }
            watch::Frame { kind: watch::LOST, seq: 0, tick: 0x00010203,
                event: watch::Event::Lost { count: 0x04030201, saturated: false, first_tick: 0x00010200 } }
        };
        let mut wire = [0; watch::MAX_WIRE];
        let n = watch::encode(&frame, &mut wire);
        baseline.pc = image.symbols.encode; baseline.r[18] = u8::from(hello);
        let mut points = 0;
        let mut masked = 0;
        let mut replayed = BTreeSet::new();
        while baseline.pc != image.symbols.idle_check {
            // At least one cycle per instruction: before the next instruction,
            // fewer than the proven cycle bound may have executed.
            assert!(points < image.bounds.encode as usize, "one bounded encode must finish within its cycle bound");
            // The cursors change only in the masked publication: until then a pump
            // sees them equal at every repetition of a CRC/COBS loop instruction,
            // exactly as at its first. Replay each encode instruction once and
            // every boundary of the publication, the non-atomic cursor pair.
            let pc = baseline.pc;
            let publishing = (image.symbols.tx_publish..image.symbols.encode_end).contains(&pc);
            // The premise of the reduced replay, checked at every instruction rather
            // than assumed: before publication both cursors keep their seeded value.
            if !publishing {
                assert_eq!((baseline.read(l.tx_pos), baseline.read(l.tx_len)), (7, 7),
                    "{level:?} Hello={hello} PC={pc}: cursors change only in the masked publication");
            }
            if !replayed.insert(pc) && !publishing {
                baseline.step().unwrap(); points += 1;
                continue;
            }
            let mut cpu = baseline.clone();
            if publishing && cpu.sreg & 128 == 0 { masked += 1; }
            cpu.write(0xc1, 0x28); // UDRE is already pending: empty hardware UDR.
            to_symbol(&mut cpu, image.symbols.idle_check, u64::from(image.bounds.encode + image.bounds.udre));
            assert!(!cpu.events.iter().any(|e| matches!(e, Event::Tx { .. })),
                "{level:?} Hello={hello} PC={pc}: no byte before publication completes");
            assert_eq!((cpu.read(l.tx_pos), cpu.read(l.tx_len)), (0, n as u8), "complete cursor publication at PC={pc}");
            assert_eq!((0..n).map(|i| cpu.read(l.tx + i as u16)).collect::<Vec<_>>(), wire[..n], "complete wire buffer at PC={pc}");
            drain(&mut cpu, &image);
            let sent: Vec<_> = cpu.events.iter().filter_map(|e| match e { Event::Tx { byte, .. } => Some(*byte), _ => None }).collect();
            assert_eq!(sent, wire[..n], "{level:?} Hello={hello} PC={pc}: pump sends exactly one complete frame");
            assert_eq!(frames(&cpu).iter().map(|(f, _)| *f).collect::<Vec<_>>(), [frame]);
            baseline.step().unwrap(); points += 1;
        }
        assert!(masked >= 4, "all masked publication stores and SEI were swept");
    } }
}

#[test]
fn idle_lost_waits_for_room_and_makes_progress() {
    for level in [watch::Level::Faults, watch::Level::Ports] {
        let probe = build(&Program { watch: level, ..Program::default() }).unwrap();
        let scratch_bytes = 0x900 - probe.layout.ring - probe.layout.stack_bytes - 6 * 8;
        let image = build(&Program { watch: level, scratch_bytes, ..Program::default() }).unwrap();
        let l = &image.layout;
        assert_eq!(l.records, 6, "small ring proves both capacity edges");
        for free in 0..=2 { for tail in [0, 3] {
            let (mut cpu, _) = booted(&image);
            cpu.write(0xc4, 0); cpu.write(0xc5, 0); cpu.write(0xc0, 0x42);
            cpu.write(l.hello_pending, 0);
            let used = l.records - 1 - free;
            cpu.write(l.tail, tail); cpu.write(l.head, (tail + used) % l.records);
            for i in 0..used {
                let at = l.ring + u16::from((tail + i) % l.records) * 8;
                for (offset, byte) in [watch::RX, i + 1, 0, 0, 0, i + 20, 0, 0].into_iter().enumerate() {
                    cpu.write(at + offset as u16, byte);
                }
            }
            store(&mut cpu, l.lost, 2, 4); store(&mut cpu, l.lost_first, 7, 4); store(&mut cpu, l.lost_last, 9, 4);
            store(&mut cpu, l.far_until, 64, 3);
            cpu.pc = image.symbols.idle_check; cpu.sreg = 128; cpu.write(0xc1, 0x28);
            let end = cpu.cycles + 20_000;
            let mut encodes = 0;
            while cpu.pc != image.symbols.pass {
                assert!(cpu.cycles < end, "Lost without room must reach an encoder, not stall");
                if cpu.will_execute() && cpu.pc == image.symbols.encode { encodes += 1; }
                cpu.step().unwrap();
            }
            assert_eq!(encodes, 2 - free, "free={free}, tail={tail}: encode first until the pair fits; no pass per wake");
            // Let that first pass append Lost, then free one slot for a newer
            // event while the pair is still queued behind the older records.
            let mut next_encode = false;
            loop {
                assert!(cpu.cycles < end, "the Lost-flushing pass must resume encoding");
                if cpu.will_execute() && cpu.pc == image.symbols.encode { next_encode = true; }
                cpu.step().unwrap();
                if next_encode && cpu.pc == image.symbols.idle_check { break; }
            }
            assert_eq!(load(&cpu, l.lost, 4), 0, "first pass with room flushes Lost");
            cpu.write(0xc1, 8); cpu.sreg = 0;
            for (offset, byte) in [watch::RX, 11, 0, 0, 0, 0xa5, 0, 0].into_iter().enumerate() {
                cpu.write(l.record + offset as u16, byte);
            }
            cpu.write(l.record_count, 1); invoke(&mut cpu, &image, image.symbols.push);
            assert_eq!(load(&cpu, l.lost, 4), 0, "the newer event fits behind the queued Lost pair");
            cpu.pc = image.symbols.pass; cpu.sreg = 128;
            drain_passes(&mut cpu, &image);
            let got = frames(&cpu);
            let mut expected: Vec<_> = (0..used).map(|i| watch::Event::Rx { byte: i + 20 }).collect();
            expected.push(watch::Event::Lost { count: 2, saturated: false, first_tick: 7 });
            expected.push(watch::Event::Rx { byte: 0xa5 });
            assert_eq!(got.iter().map(|(f, _)| f.event).collect::<Vec<_>>(), expected,
                "{level:?} free={free}, tail={tail}: oldest records, Lost pair, then newer event");
            assert_eq!(got[usize::from(used)].0.tick, 9, "Lost retains its last tick");
        } }
    }
}

#[test]
fn stub_bounds_under_ring_pressure() {
    // A full ring exercises refusal; pending Lost plus a fault's two records
    // exercises the longer flush/append path. Cover both head wrap and borrow.
    // At clk/2 the ADC fault is longer than conversion; at clk/128 it is shorter.
    for f_clk in [100_000, 16_000_000] {
        for watch in [watch::Level::Off, watch::Level::Faults, watch::Level::Ports] {
            let image = build(&Program { f_clk, watch, ports: u32::MAX,
                pins: vec![PinDef { pin: 13, role: Role::Output }, PinDef { pin: 14, role: Role::Analog },
                    PinDef { pin: 6, role: Role::Pwm(pwm_setting(0, 255, 8)) },
                    PinDef { pin: 9, role: Role::Pwm(pwm_setting(1, 320, 1)) }],
                timers: vec![TimerDef { entry: 0, period_ticks: 100, repeat: true }],
                serial: Some(SerialDef { entry: 0, fifo: 1, setting: rx::SETTING }),
                entries: vec![&|_, _| {}], ..Program::default() }).unwrap();
            let (boot, _) = booted(&image);
            let (s, b, l) = (&image.symbols, &image.bounds, &image.layout);
            for (name, target, bound, arg, value) in [
                ("write", s.write, b.write, 13i32, 1), ("write fault", s.write, b.write, -1, 1),
                ("read", s.read, b.read, 13, 0), ("read fault", s.read, b.read, -1, 0),
                ("record_write low", s.record_write, b.record_write, 13, 0),
                ("record_write high", s.record_write, b.record_write, 13, 1),
                ("record_read low", s.record_read, b.record_read, 13, 0),
                ("record_read high", s.record_read, b.record_read, 13, 1),
                ("duty upper", s.duty, b.duty, 6, i32::MAX),
                ("duty lower", s.duty, b.duty, 6, i32::MIN),
                ("duty scaled", s.duty, b.duty, 9, i32::MAX),
                ("duty fault", s.duty, b.duty, -1, i32::MAX),
                ("adc", s.adc, b.adc, 14, 0), ("adc fault", s.adc, b.adc, -1, 0),
                ("timer_stop", s.timer_stop, b.timer_stop, 0, 0),
                ("timer_start", s.timer_start, b.timer_start, 0, 0),
                ("timer_period clamp", s.timer_period, b.timer_period, 0, -1),
                ("timer_stop fault", s.timer_stop, b.timer_stop, -1, 0),
                ("timer_start fault", s.timer_start, b.timer_start, -1, 0),
                ("timer_period fault", s.timer_period, b.timer_period, -1, -1),
                ("loop_cap", s.loop_cap, b.loop_cap, 0, 0),
                ("serial_byte fault", s.serial_byte, b.serial_byte, -1, 0),
            ] {
                if target == 0 { assert_eq!(bound, 0, "unlinked {name}"); continue; }
                let pressure = if watch == watch::Level::Off { vec![(0, 0, 0)] }
                    else { vec![(l.records - 1, 0, 0), (l.records - 1, l.records - 1, 7), (0, l.records - 1, 7)] };
                for (head, tail, lost) in pressure {
                    let mut cpu = boot.clone(); cpu.sreg = 0;
                    if watch != watch::Level::Off {
                        cpu.write(l.head, head); cpu.write(l.tail, tail);
                        store(&mut cpu, l.lost, lost, 4);
                        store(&mut cpu, l.lost_first, 1, 4); store(&mut cpu, l.lost_last, 2, 4);
                    }
                    cpu.r[16..20].copy_from_slice(&arg.to_le_bytes());
                    cpu.r[20..24].copy_from_slice(&value.to_le_bytes());
                    cpu.r[24..26].copy_from_slice(&0x1234u16.to_le_bytes());
                    let start = cpu.cycles;
                    let call = invoke(&mut cpu, &image, target);
                    let case = format!("{name} {watch:?} clk={f_clk} head={head} tail={tail} lost={lost}");
                    within(&case, call.clocks, bound);
                    for &effect in &call.effects {
                        within(&case, effect - start, if target == s.write { b.write_effect } else { b.duty_effect });
                    }
                    if arg == -1 && (target == s.write || target == s.duty) {
                        assert!(call.effects.is_empty(), "{case}: invalid pins have no effect store");
                    }
                    if watch == watch::Level::Off {
                        assert_unobserved_image(&image);
                        let fault = if name == "loop_cap" { Some(watch::FAULT_LOOP_CAP) }
                            else if arg != -1 { None }
                            else if name.starts_with("timer_") { Some(watch::FAULT_TIMER) }
                            else if name.starts_with("serial_") { Some(watch::FAULT_SERIAL) }
                            else { Some(watch::FAULT_PIN) };
                        for cell in &l.fault_cells {
                            assert_eq!(fault_cell(&cpu, &image, cell.code),
                                if Some(cell.code) == fault { (1, 0x1234) } else { (0, 0) }, "{case}: fault cell {}", cell.code);
                        }
                        if name != "timer_start" { assert!(call.samples.is_empty(), "{case}: no observation stamp"); }
                        assert_eq!(preserved(&cpu), preserved(&boot), "{case}: runtime registers survive");
                        if name == "read fault" || name == "adc fault" { assert_eq!(cpu.r[16..20], [0; 4], "{case}: bad read returns zero"); }
                        if name == "serial_byte fault" { assert_eq!(cpu.r[16..20], [255; 4], "{case}: bad byte returns -1"); }
                    } else if !call.pushes.is_empty() {
                        if lost == 0 {
                            assert_eq!(load(&cpu, l.lost, 4), u64::from(cpu.read(l.record_count) != 0), "{case}: full ring counts the refused event");
                        } else {
                            assert_eq!(load(&cpu, l.lost, 4), 0, "{case}: room flushes Lost before the new record");
                            assert_eq!(cpu.read(l.ring + u16::from(head) * 8), watch::LOST, "{case}: Lost is first across wrap");
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn routine_bounds() {
    let image = make_image(500_000, true, &blink_entry);
    let (mut cpu, t0) = booted(&image);
    let registers = [0x24, 0x25, 0x27, 0x28, 0x2a, 0x2b, 0x44, 0x45, 0x47, 0x48,
        0x80, 0x81, 0x88, 0x89, 0x8a, 0x8b, 0xb0, 0xb1, 0xb3, 0xb4];
    for state in 0..4 {
        clock_at(&mut cpu, &image, t0, 32);
        cpu.write(0x6e, 0);
        cpu.write(0x25, 0); cpu.events.clear();
        cpu.write(image.layout.head, if state == 1 { image.layout.records - 1 } else { 0 });
        cpu.write(image.layout.tail, 0);
        store(&mut cpu, image.layout.lost, if state == 2 { 7 } else { 0 }, 4);
        store(&mut cpu, image.layout.lost_first, 1, 4);
        store(&mut cpu, image.layout.lost_last, 2, 4);
        cpu.r[16..20].copy_from_slice(&(if state == 3 { 12i32 } else { 13 }).to_le_bytes());
        cpu.r[20] = 1; cpu.r[24] = 1; cpu.r[25] = 0;
        for r in 6..13 { cpu.r[r] = 0x91 + r as u8; }
        cpu.r[28] = 0x45; cpu.r[29] = 1;
        let kept = preserved(&cpu);
        let start = cpu.cycles;
        let call = invoke(&mut cpu, &image, image.symbols.write);
        within(&format!("write state={state}"), call.clocks, image.bounds.write);
        for &effect in &call.effects {
            within("write_effect", effect - start, image.bounds.write_effect);
            assert!(effect < call.pushes[0], "write effect must precede its watch push");
        }
        assert_eq!(preserved(&cpu), kept, "write must preserve reserved registers and Y");
        if state != 3 {
            assert_eq!(call.effects.len(), 1, "valid write must reach one effect store");
            assert_eq!(pins(&cpu), [(call.effects[0], true)], "write effect must precede agent-dependent work");
        } else { assert!(pins(&cpu).is_empty(), "undeclared pin must never be driven"); }
        if state == 1 { assert_eq!(load(&cpu, image.layout.lost, 4), 1, "full ring must count the refused event"); }
        if state == 2 { assert_eq!(load(&cpu, image.layout.lost, 4), 0, "room must flush Lost before Write"); }
    }
    for (name, target, bound) in [("write", image.symbols.write, image.bounds.write), ("read", image.symbols.read, image.bounds.read)] {
        for pin in [300i32, 269, -1, -243, 65549, 16777229, i32::MIN, i32::MAX] {
            let elapsed = cpu.cycles - t0 + 32;
            clock_at(&mut cpu, &image, t0, elapsed); cpu.write(0x6e, 0);
            cpu.write(0x25, 0); cpu.events.clear();
            cpu.write(image.layout.head, 0); cpu.write(image.layout.tail, 0);
            cpu.write(image.layout.hello_pending, 0); store(&mut cpu, image.layout.lost, 0, 4);
            cpu.r[16..20].copy_from_slice(&pin.to_le_bytes());
            cpu.r[20..24].copy_from_slice(&1i32.to_le_bytes());
            cpu.r[24..26].copy_from_slice(&0x1234u16.to_le_bytes());
            let kept = preserved(&cpu);
            let before: Vec<_> = registers.iter().map(|&r| cpu.read(r)).collect();
            let call = invoke(&mut cpu, &image, target);
            within(&format!("{name} pin={pin}"), call.clocks, bound);
            assert_eq!(preserved(&cpu), kept, "invalid pin preserves reserved registers");
            assert_eq!(registers.iter().map(|&r| cpu.read(r)).collect::<Vec<_>>(), before,
                "invalid pin must change no pin, OCR or timer control");
            assert!(pins(&cpu).is_empty(), "invalid pin must never alias an output");
            assert_eq!(cpu.r[16..20], if target == image.symbols.read { [0; 4] } else { pin.to_le_bytes() },
                "invalid read returns zero; write preserves A");
            assert_eq!(cpu.read(image.layout.head), 2, "invalid pin queues both Fault records");
            let tick = (call.samples[0] - t0) / 8;
            drain(&mut cpu, &image);
            let got = frames(&cpu);
            assert_eq!(got.len(), 1, "invalid pin produces one wire frame");
            assert_eq!((got[0].0.event, got[0].1),
                (watch::Event::Fault { code: watch::FAULT_PIN, site: 0x1234, arg: pin }, tick),
                "pin Fault preserves the full argument, site and stamp on the wire");
        }
    }
    for (name, target, bound) in [("read", image.symbols.read, image.bounds.read),
        ("clock_us", image.symbols.clock_us, image.bounds.clock_us), ("clock_ms", image.symbols.clock_ms, image.bounds.clock_ms)] {
        clock_at(&mut cpu, &image, t0, 123_456); cpu.write(0x6e, 0);
        cpu.r[16..20].copy_from_slice(&13i32.to_le_bytes());
        let result = invoke(&mut cpu, &image, target);
        within(name, result.clocks, bound);
        let returned = u32::from_le_bytes(cpu.r[16..20].try_into().unwrap());
        let sample = (result.samples[0] - t0) / 8;
        if target == image.symbols.clock_us { assert_eq!(returned, (sample >> 1) as u32, "clock_us truncates to u32"); }
        if target == image.symbols.clock_ms { assert_eq!(returned, (sample / 2000) as u32, "clock_ms uses full 48-bit dividend"); }
    }
    let off = build(&Program { watch: watch::Level::Off, state_init: vec![0],
        pins: vec![PinDef { pin: 13, role: Role::Output }],
        timers: vec![TimerDef { entry: 0, period_ticks: 500_000, repeat: true }],
        entries: vec![&blink_entry], ..Program::default() }).unwrap();
    assert!(off.layout.w < image.layout.w && off.bounds.pass_prefix < image.bounds.pass_prefix,
        "Off's actual pass bound shrinks W");
    assert!(off.bounds.overrun < image.bounds.overrun, "Off keeps division but drops watch work");
    for image in [&image, &off] {
        let (mut cpu, t0) = booted(image);
        for (kind, name, target, bound) in [(0, "timer_stop", image.symbols.timer_stop, image.bounds.timer_stop),
            (1, "timer_start", image.symbols.timer_start, image.bounds.timer_start),
            (2, "timer_period", image.symbols.timer_period, image.bounds.timer_period)] {
            for (k, us) in [(0i32, -1i32), (0, 0), (0, 7), (1, 7), (255, -1),
                (300, 7), (269, 7), (-1, 7), (256, 7), (65536, 7), (16777216, 7),
                (i32::MIN, 7), (i32::MAX, 7)] {
                clock_at(&mut cpu, &image, t0, 1000); cpu.write(0x6e, 0);
                store(&mut cpu, image.layout.timers, 777, 6); store(&mut cpu, image.layout.timers + 6, 100, 6);
                if image.layout.records != 0 {
                    cpu.write(image.layout.head, 0); cpu.write(image.layout.tail, 0); store(&mut cpu, image.layout.lost, 0, 4);
                    cpu.write(image.layout.hello_pending, 0);
                }
                for cell in &image.layout.fault_cells { store(&mut cpu, cell.addr as u16, 0, 4); }
                cpu.events.clear();
                cpu.r[16..20].copy_from_slice(&k.to_le_bytes());
                cpu.r[20..24].copy_from_slice(&us.to_le_bytes()); cpu.r[24] = 9; cpu.r[25] = 0;
                let kept = preserved(&cpu);
                let before: Vec<_> = registers.iter().map(|&r| cpu.read(r)).collect();
                let result = invoke(&mut cpu, &image, target);
                within(&format!("{name} k={k} us={us}"), result.clocks, bound);
                assert_eq!(preserved(&cpu), kept, "timer stub must preserve reserved registers");
                assert_eq!(cpu.r[16..20], k.to_le_bytes(), "timer stub preserves the full handle");
                assert_eq!(registers.iter().map(|&r| cpu.read(r)).collect::<Vec<_>>(), before,
                    "timer stub must change no pin, OCR or timer control");
                let due = load(&cpu, image.layout.timers, 6);
                let period = load(&cpu, image.layout.timers + 6, 6);
                if k != 0 {
                    assert_eq!((due, period), (777, 100), "invalid timer must change no timer state");
                    if image.layout.records == 0 {
                        assert_eq!(fault_cell(&cpu, image, watch::FAULT_TIMER), (1, 9), "invalid timer counts at its site");
                        assert!(result.samples.is_empty(), "invalid timers need no stamp at Off");
                    } else {
                        assert_eq!(cpu.read(image.layout.ring), watch::FAULT, "invalid timer must queue a Fault");
                        assert_eq!(cpu.read(image.layout.ring + 5), watch::FAULT_TIMER, "invalid timer fault code");
                        assert_eq!(load(&cpu, image.layout.ring + 9, 4) as u32 as i32, k, "invalid timer fault argument");
                        assert_eq!(cpu.read(image.layout.head), 2, "invalid timer queues both Fault records");
                        let tick = (result.samples[0] - t0) / 8;
                        drain(&mut cpu, &image);
                        let got = frames(&cpu);
                        assert_eq!(got.len(), 1, "invalid timer produces one wire frame");
                        assert_eq!((got[0].0.event, got[0].1),
                            (watch::Event::Fault { code: watch::FAULT_TIMER, site: 9, arg: k }, tick),
                            "timer Fault preserves the full argument, site and stamp on the wire");
                    }
                } else {
                    if image.layout.records == 0 {
                        assert_eq!(fault_cell(&cpu, image, watch::FAULT_TIMER), (0, 0), "valid timers do not count faults");
                        if kind != 1 { assert!(result.samples.is_empty(), "stop/period need no stamp at Off"); }
                    }
                    match kind {
                        0 => assert_eq!(due, DISARMED, "stop disarms"),
                        1 => assert_eq!(due, (result.samples[0] - t0) / 8 + 100, "start restarts its grid at the stamp"),
                        _ => {
                            assert_eq!(due, 777, "period change leaves the next firing alone");
                            assert_eq!(period, (us.max(0) as u64 * 2).max(1), "every period clamps zero to one tick");
                        }
                    }
                }
            }
        }
    }
    clock_at(&mut cpu, &image, t0, OVERFLOW - 1);
    let pc = scratch(&mut cpu, &image); cpu.pc = pc; cpu.step().unwrap();
    let start = cpu.cycles; let sp = cpu.sp;
    cpu.step().unwrap(); assert_eq!(cpu.pc, 32, "overflow vector must be word 32");
    while cpu.sp != sp { cpu.step().unwrap(); }
    assert_eq!(cpu.cycles - start, u64::from(TOV_INT), "overflow entry, shortest vector, body and RETI cost");
    clock_at(&mut cpu, &image, t0, 0); cpu.sreg = 0;
    cpu.write(image.layout.hello_pending, 1); cpu.write(image.layout.tx_len, 0); cpu.write(image.layout.tx_pos, 0);
    let kept = preserved(&cpu);
    cpu.write(0xc1, 8); cpu.write(0x6e, 0);
    let clocks = encode_frame(&mut cpu, &image);
    within("foreground Hello including publication/recheck jump", clocks, image.bounds.encode);
    assert_eq!(preserved(&cpu), kept, "encoder must preserve the clock and handler registers");
    cpu.sreg = 0;
    let regs = cpu.r;
    let byte = invoke(&mut cpu, &image, image.symbols.udre);
    within("udre buffered byte including entry/vector", byte.clocks + 7, image.bounds.udre);
    for r in (0..32).filter(|&r| r != 4 && r != 5) {
        assert_eq!(cpu.r[r], regs[r], "pump preserves r{r}");
    }
    cpu.sreg = 0; cpu.write(image.layout.tx_pos, cpu.read(image.layout.tx_len));
    let empty = invoke(&mut cpu, &image, image.symbols.udre);
    within("udre empty buffer including entry/vector", empty.clocks + 7, image.bounds.udre);
    assert_eq!(cpu.read(0xc1) & 0x20, 0, "empty pump disables UDRIE");
    // The interpreted scheduler's loop advances D+P while it is <= now. Use
    // that loop as the independent reference, rather than quotient arithmetic.
    let busy = |a: &mut Asm, s: &Stubs| {
        blink_entry(a, s);
        for _ in 0..1400 { a.emit(Rjmp(0)); }
    };
    for watch in [watch::Level::Off, watch::Level::Faults, watch::Level::Ports] {
        let stress = build(&Program { watch, state_init: vec![0],
            pins: vec![PinDef { pin: 13, role: Role::Output }],
            timers: vec![TimerDef { entry: 0, period_ticks: 200, repeat: true }],
            entries: vec![&busy], ..Program::default() }).unwrap();
        let (mut cpu, t0) = booted(&stress);
        let seed = cpu.clone();
        let mut trace = Trace::default();
        let mut wanted = Vec::new();
        while trace.entries.len() < 6 {
            if cpu.pc == stress.symbols.lock {
                // Save the selected due; stamp is replaced by the lock's T later.
                wanted.push((load(&cpu, stress.layout.due, 6), 0u64));
            }
            if cpu.pc == stress.symbols.lock_end {
                wanted.last_mut().unwrap().1 = load(&cpu, stress.layout.stamp, 6);
            }
            assert!(cpu.cycles < t0 + 200_000, "overrunning handler must remain live");
            if cpu.pc == stress.symbols.entries[0] {
                let &(due, now) = wanted.last().unwrap();
                let mut next = due + 200;
                while next <= now { next += 200; }
                assert_eq!(load(&cpu, stress.layout.timers, 6), next, "division advances to the first unmissed grid point");
            }
            trace.step(&mut cpu, &stress, t0);
        }
        to_symbol(&mut cpu, stress.symbols.pass, 20_000);
        store(&mut cpu, stress.layout.timers, DISARMED, 6); run(&mut cpu, 500_000);
        let actual: Vec<_> = frames(&cpu).into_iter().filter_map(|(frame, _)| match frame.event {
            watch::Event::Fault { code: watch::FAULT_OVERRUN, site: 0, arg } => Some(arg as u32), _ => None,
        }).collect();
        let expected: Vec<_> = wanted.into_iter().filter_map(|(due, now)| {
            let mut next = due + 200; let mut missed = 0u32;
            while next <= now { next += 200; missed = missed.saturating_add(1); }
            (missed != 0).then_some(missed)
        }).collect();
        assert!(!expected.is_empty(), "busy handler must exercise overrun division");
        if watch == watch::Level::Off {
            assert!(actual.is_empty(), "Off has no fault frames");
            assert_eq!(fault_cell(&cpu, &stress, watch::FAULT_OVERRUN), (expected.len() as u16, 0),
                "Off counts overrun occurrences at the timer's index");
        } else { assert_eq!(actual, expected, "missed counts must match sched.rs's grid-advance loop"); }
        // Reuse the busy-handler image, entering bookkeeping with a chosen lock
        // stamp. Mask services only in this measurement; production enters with I
        // set. Measure the arm to its IJMP target, and account for the exact
        // bookkeeping suffix it replaces: Bounds::overrun is the extra cost.
        let l = &stress.layout;
        let mut worst_arm = 0;
        let mut worst_extra = 0;
        for (period, late) in [(200, 200), (200, 37 * 200 + 199),
            (0x0102030405, 37 * 0x0102030405 + 0x0102030404),
            (1, u64::from(u32::MAX)), (1, u64::from(u32::MAX) + 1), (1, DISARMED)] {
            for ring in 0..if watch == watch::Level::Off { 1 } else { 4 } {
                let mut cpu = seed.clone();
                cpu.sreg = 0; cpu.pc = stress.symbols.bookkeep;
                store(&mut cpu, l.due, 0, 6); store(&mut cpu, l.stamp, late, 6);
                store(&mut cpu, l.timers, 0, 6); store(&mut cpu, l.timers + 6, period, 6);
                cpu.write(l.selected, 0);
                if watch != watch::Level::Off {
                    let (head, tail, lost) = match ring {
                        0 => (0, 0, 0), // Empty, no Lost.
                        1 => (l.records - 1, 0, 0), // Full: refuse this Fault.
                        2 => (0, 5, 7), // Lost plus Fault, head below tail.
                        _ => (l.records - 2, l.records - 2, 7), // Append across wrap.
                    };
                    cpu.write(l.head, head); cpu.write(l.tail, tail);
                    store(&mut cpu, l.lost, lost, 4);
                }
                let sp = cpu.sp;
                let start = cpu.cycles;
                to_symbol(&mut cpu, stress.symbols.overrun, 1000);
                let prefix = cpu.cycles - start;
                let start = cpu.cycles;
                to_symbol(&mut cpu, stress.symbols.dispatch[0], u64::from(BOOKKEEP + stress.bounds.overrun) + 1);
                let arm = cpu.cycles - start;
                let extra = prefix + arm - u64::from(BOOKKEEP);
                within(&format!("overrun {watch:?} period={period} late={late} ring={ring}"), extra, stress.bounds.overrun);
                worst_arm = worst_arm.max(arm); worst_extra = worst_extra.max(extra);
                assert_eq!(cpu.sp, sp, "overrun restores the saved timer pointer and call frames");
                assert_eq!(load(&cpu, l.timers, 6), (late - late % period + period) & DISARMED,
                    "wide overrun advances to the first unmissed grid point modulo 48 bits");
                assert_eq!(load(&cpu, l.timers + 6, 6), period, "overrun preserves the timer period");
                assert_eq!(cpu.read(l.state), seed.read(l.state), "bookkeeping leaves handler state alone");
                if watch == watch::Level::Off {
                    assert_eq!(fault_cell(&cpu, &stress, watch::FAULT_OVERRUN), (1, 0),
                        "Off counts one overrun occurrence at the selected timer, even for a wide quotient");
                } else {
                    assert_eq!(load(&cpu, l.record + 9, 4), (late / period).min(u64::from(u32::MAX)),
                        "overrun record saturates the full 48-bit missed count to u32");
                }
            }
        }
        eprintln!("overrun {watch:?}: {} bytes, measured arm {worst_arm} cycles, extra {worst_extra} <= bound {}",
            2 * (stress.symbols.overrun_end - stress.symbols.overrun), stress.bounds.overrun);
    }
}

fn predecessor(a: &mut Asm, extra: usize) {
    let again = a.label(); a.bind(again);
    a.emit(Sbiw(24, 1)); a.jump(Target::Brbc(1), again);
    for _ in 0..extra { a.emit(Nop); }
    // The builder supplies RET: total 4*q + extra + 3, before IRQ insertions.
}

fn instruction_pcs(image: &Image, first: u32, end: u32) -> BTreeSet<WindowPc> {
    let mut pcs = BTreeSet::new();
    let mut pc = first;
    while pc < end {
        pcs.insert(WindowPc { pc, caller: None });
        let (insn, words) = decode(&image.words[pc as usize..]).expect("routine range must decode");
        pc += words as u32;
        if let Insn::Call(target) = insn {
            if (image.symbols.pad..image.symbols.pad_end).contains(&target) {
                // A window's padding includes the called suffix and RET, with this return
                // address: an edge in another caller's use of the sled cannot cover it.
                for mut location in instruction_pcs(image, target, image.symbols.pad_end) {
                    location.caller = Some(pc); pcs.insert(location);
                }
            }
        }
    }
    assert_eq!(pc, end, "routine boundary must follow a whole instruction");
    pcs
}

struct PhaseCase {
    r: u64,
    c: u64,
    pending: bool,
    trace: Trace,
}

struct PhaseFixture<'a> {
    image: &'a Image,
    cpu: Cpu,
    ram: Vec<u8>,
    registers: [u8; 32],
    t0: u64,
    plan: TracePlan,
    executed: u64,
}

impl<'a> PhaseFixture<'a> {
    fn new(image: &'a Image) -> Self {
        let (mut cpu, t0) = booted(image);
        cpu.write(0x24, 0x20); cpu.write(0xc0, 2); cpu.write(0xc2, 6);
        cpu.write(0xc4, 0); cpu.write(0xc5, 0);
        Self { ram: cpu.sram.clone(), registers: cpu.r, cpu, image, t0,
            plan: TracePlan::new(image), executed: t0 }
    }

    fn case(&mut self, due: u64, work: u64) -> PhaseCase {
        let (image, t0, cpu) = (self.image, self.t0, &mut self.cpu);
        // Cpu has no Clone implementation. Reuse its flash, allocation and
        // configured bus; restore the seed's public state. All prior TX work
        // is settled before this reset, and no case ends asleep.
        cpu.sreg = 0;
        cpu.pc = scratch(cpu, image); cpu.step().unwrap();
        cpu.r = self.registers; cpu.sp = image.layout.stack_top;
        cpu.sram.copy_from_slice(&self.ram);
        cpu.write(0x25, 0); cpu.events.clear();
        let q = (work - 3) / 4;
        let extra = (work - 3) % 4;
        assert!(q >= 2, "predecessor needs two final iterations");
        // Earlier iterations are identical four-clock loops, with every edge
        // at a loop boundary and every ISR adding sixteen clocks. Initialize
        // immediately after that prefix's boundary service, then execute its
        // final two iterations, all trailing NOPs and RET in the oracle.
        let prefix = 4 * (q - 2);
        let mut elapsed = prefix;
        loop {
            let next = prefix + (elapsed / OVERFLOW) * u64::from(TOV_INT);
            if next == elapsed { break; }
            elapsed = next;
        }
        clock_at(cpu, image, t0, elapsed);
        store(cpu, image.layout.timers, due, 6);
        store(cpu, image.layout.timers + 6, due, 6);
        cpu.r[24] = 2; cpu.r[25] = 0;
        return_to(cpu, image.symbols.pass);
        cpu.pc = image.symbols.entries[1 + extra as usize];
        let began = cpu.cycles;
        let mut ret_start = None;
        while cpu.pc != image.symbols.pass {
            let before = cpu.cycles;
            let old_sp = cpu.sp;
            let (insn, _) = decode(&cpu.flash[cpu.pc as usize..]).unwrap();
            cpu.step().unwrap();
            if matches!(insn, Ret) && cpu.sp == old_sp + 2 && cpu.pc == image.symbols.pass { ret_start = Some(before); }
            assert!(cpu.cycles < t0 + work + 256, "predecessor may only gain counted overflow service");
        }
        let r = cpu.cycles;
        let ret_start = ret_start.expect("predecessor must return through its RET");
        let pending = cpu.read(0x35) & 1 != 0;
        // With no older pending request, RET's four clocks leave TOV pending
        // exactly for u in (RET_start,r]. Completion edges are included. The
        // boundary services that request before CLI, costing exactly TOV_INT.
        assert_eq!(pending, (ret_start - t0) / OVERFLOW < (r - t0) / OVERFLOW,
            "return-boundary pending convention r={} ret_start={}", r - t0, ret_start - t0);
        if pending {
            cpu.step().unwrap(); assert_eq!(cpu.pc, 32, "boundary must service pending TOV first");
            while cpu.pc != image.symbols.pass { cpu.step().unwrap(); }
        }
        let c = cpu.cycles;
        assert_eq!(c - r, if pending { u64::from(TOV_INT) } else { 0 }, "only the pending boundary ISR separates r and c");
        // If r=u, entry begins at c=u+16. The unavailable interval is [u,u+16),
        // not an interval that excludes this reachable upper endpoint.
        assert!((c - t0) % OVERFLOW >= u64::from(TOV_INT), "pass cannot start during the preceding overflow service");
        let mut trace = Trace { deferred: pending, ..Trace::default() };
        while trace.entries.is_empty() {
            assert!(cpu.cycles < c + 20_000, "negative burn must clamp instead of wrapping");
            trace.advance(cpu, image, t0, &self.plan);
        }
        let lock = *trace.locks.last().expect("every return must reach a measured lock");
        let start = trace.entries[0].1;
        if lock.delta >= 0 && lock.raw >= i64::from(N_MIN) {
            assert_eq!(start, entry_start(t0, due), "on-time phase boundary: due={due} lock={lock:?}");
        } else {
            let services = c - r + service_clocks(&trace, c, start);
            within("late redispatch from predecessor return", start - r - services, image.bounds.redispatch());
            assert!(start >= entry_start(t0, due), "late entry cannot precede its due");
        }
        if lock.delta >= 0 && lock.raw < i64::from(N_MIN) {
            assert_eq!(lock.burn, N_MIN, "nonnegative delta with insufficient burn must clamp");
            within("clamped redispatch", start - lock.pass - service_clocks(&trace, lock.pass, start),
                image.bounds.redispatch());
        } else { assert!(lock.burn >= N_MIN, "every other return must retain a valid delay"); }
        assert_eq!(load(cpu, image.layout.timers, 6), DISARMED, "one-shot must disarm before entry");
        assert_eq!(trace.entries.len(), 1, "one-shot must run exactly once");
        self.executed += 1 + elapsed % 8 + cpu.cycles - began;
        PhaseCase { r, c, pending, trace }
    }

    fn finish(&mut self, case: &mut PhaseCase, check_frame: bool) {
        let start = self.cpu.cycles;
        if check_frame {
            while self.cpu.pc != self.image.symbols.pass {
                case.trace.advance(&mut self.cpu, self.image, self.t0, &self.plan);
            }
            assert_eq!(case.trace.entries.len(), 1, "checked one-shot must return without another entry");
            assert_eq!(case.trace.samples.len(), 1, "checked one-shot must take one write stamp");
            assert_eq!(pins(&self.cpu), [(effect_cycle(&case.trace, case.trace.entries[0].1).0, true)],
                "checked phase pin effect must include exactly the served prefix ISRs");
            drain(&mut self.cpu, self.image);
            let writes: Vec<_> = frames(&self.cpu).into_iter().filter(|(frame, _)| matches!(frame.event, watch::Event::Write { .. })).collect();
            assert_eq!(writes.len(), 1, "phase fixture must emit exactly one Write frame");
            assert_eq!(writes[0].1, (case.trace.samples[0] - self.t0) / 8, "phase fixture frame tick must equal its sampled cycle");
        } else if self.cpu.events.iter().any(|event| matches!(event, Event::Tx { .. })) {
            // A far-path case may have started Hello. Settle at most the
            // shifter plus one buffered byte before restoring an earlier epoch.
            self.cpu.sreg = 0; self.cpu.write(0xc1, 8);
            let idle = scratch(&mut self.cpu, self.image);
            self.cpu.flash[idle as usize] = encode(&Rjmp(-1)).unwrap().0[0]; self.cpu.pc = idle;
            run(&mut self.cpu, 160);
            assert_ne!(self.cpu.read(0xc0) & 0x40, 0, "reused CPU must have no unfinished TX frame");
        }
        self.executed += self.cpu.cycles - start;
    }
}

#[test]
fn dispatch_phase_sweep() {
    let p0 = |a: &mut Asm, _: &Stubs| predecessor(a, 0);
    let p1 = |a: &mut Asm, _: &Stubs| predecessor(a, 1);
    let p2 = |a: &mut Asm, _: &Stubs| predecessor(a, 2);
    let p3 = |a: &mut Asm, _: &Stubs| predecessor(a, 3);
    // Keep the entire widened return interval after the clock release.
    let due = 2048u64;
    let image = build(&Program {
        state_init: vec![0], pins: vec![PinDef { pin: 13, role: Role::Output }],
        timers: vec![TimerDef { entry: 0, period_ticks: due, repeat: false }],
        entries: vec![&blink_entry, &p0, &p1, &p2, &p3], ..Program::default()
    }).unwrap();
    let mut fixture = PhaseFixture::new(&image);
    let t0 = fixture.t0;
    let lo = (8 * (due - u64::from(image.layout.w)) - 64).min(8 * due - 2 * OVERFLOW);
    let hi = 8 * due + 64;
    // The earliest near pass starts at the threshold, or where the overflow
    // service covering it ends: no pass starts inside one (see case()).
    let threshold = 8 * (due - u64::from(image.layout.w)) - 6;
    let first_near = if threshold % OVERFLOW < u64::from(TOV_INT) {
        threshold - threshold % OVERFLOW + u64::from(TOV_INT)
    } else { threshold };
    let mut edges = BTreeSet::new();
    let mut first_lock = None;
    let mut largest_owed = 0;
    let mut longest_burn = 0;
    let mut records = vec![None; (hi - lo + 1) as usize];
    let mut previous = None;
    let mut checked_frames = 0;
    // Include enough predecessor work before lo to account for its earlier
    // ISR insertions. Every reachable return in the requested interval is
    // checked; return/entry cycles occupied by an ISR are classified below.
    let first_work = lo - (hi / OVERFLOW + 2) * u64::from(TOV_INT);
    for work in first_work..=hi {
        let mut case = fixture.case(due, work);
        let lock = *case.trace.locks.last().unwrap();
        if (lo..=hi).contains(&(case.r - t0)) {
            if case.c - t0 == first_near { first_lock = case.trace.lock_starts.first().map(|at| at - t0); }
            records[(case.r - t0 - lo) as usize] = Some((lock.owed, lock.delta, lock.burn));
        }
        let signature = (lock.owed, case.pending);
        let check_frame = work % 64 == 0 || work == first_work || work == hi
            || case.r - t0 == lo || case.r - t0 == hi || previous != Some(signature);
        fixture.finish(&mut case, check_frame);
        checked_frames += usize::from(check_frame);
        previous = Some(signature);
        edges.extend(case.trace.edge_pcs);
        largest_owed = largest_owed.max(lock.owed); longest_burn = longest_burn.max(lock.burn);
    }
    for (offset, record) in records.iter().enumerate() {
        if record.is_none() {
            let r = lo + offset as u64;
            // RET can finish up to three clocks after an edge without first
            // serving it. The next possible RET completion is u+20: sixteen
            // service clocks followed by its four clocks. These are counted
            // predecessor insertions, not missing test cases.
            assert!((4..20).contains(&(r % OVERFLOW)), "unexplained return coverage gap at r={r}");
        }
    }
    assert!(records.iter().any(Option::is_some), "phase sweep must record real returns and their lock values");
    // Keep the exact lock arithmetic; its entry comes from the trace because
    // the bounded pass prefix need not consume its whole allowance.
    let first_lock = first_lock.expect("sweep must execute the earliest near pass");
    let exact_owed = ((due + u64::from(E)) / 256 - first_lock / OVERFLOW) as u8;
    let exact_burn = 8 * (due - (first_lock + 10) / 8) - ((first_lock + 2) % 8 + 1)
        - u64::from(C + TOV_INT * u32::from(exact_owed));
    assert_eq!(largest_owed, exact_owed, "widest sweep must reach the exact maximum owed");
    assert_eq!(u64::from(longest_burn), exact_burn, "widest sweep must reach the longest on-time burn");
    let mut required = instruction_pcs(&image, image.symbols.now48, image.symbols.now48_end);
    required.extend(instruction_pcs(&image, image.symbols.lock, image.symbols.lock_end));
    required.extend(instruction_pcs(&image, image.symbols.delay, image.symbols.delay_end));
    // First choose D so an overflow falls inside delay's on-time RET. Then
    // fill any remaining instruction-edge gaps with eight adjacent returns
    // per residue, stopping as soon as the required coverage is complete.
    let ret_residue = u64::from((BOOKKEEP + L - DISPATCH).div_ceil(8)) % 256;
    let mut coverage_cases = 0;
    for residue in [ret_residue].into_iter().chain((0..256).filter(|&r| r != ret_residue)) {
        if required.is_subset(&edges) { break; }
        let d = due + residue;
        for offset in 0..8 {
            let work = 8 * d - 2 * OVERFLOW - 64 + offset;
            let mut case = fixture.case(d, work);
            let signature = (case.trace.locks.last().unwrap().owed, case.pending);
            let check_frame = work % 64 == 0 || previous != Some(signature);
            fixture.finish(&mut case, check_frame);
            coverage_cases += 1;
            checked_frames += usize::from(check_frame); previous = Some(signature);
            edges.extend(case.trace.edge_pcs);
        }
    }
    let missing: Vec<_> = required.difference(&edges).copied().collect();
    assert!(missing.is_empty(), "overflow-edge coverage missing instruction PCs {missing:?}");
    for pc in image.symbols.samples {
        assert!(edges.contains(&WindowPc { pc, caller: None }), "every one of the nine samples must see an edge");
    }
    assert!(largest_owed > 2, "full-count mutation requires an observed owed value above two");
    println!("dispatch_phase_sweep: r={lo}..={hi}, max owed={largest_owed}, max burn={longest_burn}, frames={checked_frames}, coverage cases={coverage_cases}, {} executed oracle cycles", fixture.executed);
}
