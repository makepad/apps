use fab_elec_mcu_avr::{insn::{decode, encode, Insn::*, Ptr}, sim::{Cpu, Event, Stop, FLASH_WORDS}};
use fab_elec_mcu_emit_avr::Image;
use fab_elec_mcu_rt::watch;

pub fn run(cpu: &mut Cpu, cycles: u64) {
    assert_eq!(cpu.run(cycles), Stop::Cycles, "oracle run must reach its cycle budget");
}

pub fn within(name: &str, measured: u64, bound: u32) {
    assert!(measured <= u64::from(bound), "{name}: measured {measured} cycles exceeds bound {bound}");
}

pub fn to_symbol(cpu: &mut Cpu, symbol: u32, limit: u64) {
    let end = cpu.cycles + limit;
    while cpu.pc != symbol {
        assert!(cpu.cycles < end, "symbol {symbol} was not reached before cycle {end}");
        cpu.step().expect("instruction before symbol must execute");
    }
}

pub fn booted(image: &Image) -> (Cpu, u64) {
    let mut cpu = Cpu::new(&image.words);
    if image.layout.records == 0 {
        while cpu.pc != image.symbols.t0 {
            assert!(cpu.cycles < 30_000, "reset must complete");
            assert_unobserved_uart(&cpu, image.layout.rx_depth != 0);
            cpu.step().expect("reset instruction must execute");
        }
    } else { to_symbol(&mut cpu, image.symbols.t0, 30_000); }
    let t0 = cpu.cycles;
    (cpu, t0)
}

pub fn load(cpu: &Cpu, address: u16, bytes: u16) -> u64 {
    (0..bytes).map(|i| u64::from(cpu.read(address + i)) << (8 * i)).sum()
}

pub fn store(cpu: &mut Cpu, address: u16, value: u64, bytes: u16) {
    for i in 0..bytes { cpu.write(address + i, (value >> (8 * i)) as u8); }
}

pub fn fault_cell(cpu: &Cpu, image: &Image, code: u8) -> (u16, u16) {
    let addr = image.layout.fault_cells.iter().find(|cell| cell.code == code).expect("fault cell").addr as u16;
    (load(cpu, addr, 2) as u16, load(cpu, addr + 2, 2) as u16)
}

pub fn assert_unobserved_image(image: &Image) {
    let l = &image.layout;
    assert_eq!([l.head, l.tail, l.ring, l.record, l.record_count, l.lost, l.lost_first, l.lost_last,
        l.seq, l.tx, l.tx_len, l.tx_pos, l.hello_pending, l.time_sent, l.body, l.sink, l.mcusr, l.wdtcsr],
        [0; 18], "Off allocates no watch RAM");
    assert_eq!(l.records, 0, "Off has no ring capacity");
    assert_eq!((image.symbols.push, image.symbols.udre, image.symbols.encode, image.bounds.push, image.bounds.udre, image.bounds.encode),
        (0, 0, 0, 0, 0, 0), "Off links neither producer, pump nor encoder");
    let table = if image.layout.rx_depth != 0 { 38 } else { 34 };
    assert_eq!(image.symbols.tov, table, "Off ends the vector table at its last enabled vector, before UDRE");
}

/// Inspect each executed I/O store, including indirect stores. IRQ entry and sleep do not
/// execute the instruction at PC; will_execute excludes those without changing the CPU model.
pub fn assert_unobserved_uart(cpu: &Cpu, serial: bool) {
    if !cpu.will_execute() { return; }
    let pair = |r: usize| u16::from_le_bytes([cpu.r[r], cpu.r[r + 1]]);
    let (addr, value) = match decode(&cpu.flash[cpu.pc as usize..]).expect("executed instruction").0 {
        Sts(addr, r) => (addr, cpu.r[r as usize]),
        St(ptr, r) => {
            let base = match ptr { Ptr::X | Ptr::XInc | Ptr::XDec => 26,
                Ptr::Y | Ptr::YInc | Ptr::YDec => 28, Ptr::Z | Ptr::ZInc | Ptr::ZDec => 30 };
            (pair(base).wrapping_sub(u16::from(matches!(ptr, Ptr::XDec | Ptr::YDec | Ptr::ZDec))), cpu.r[r as usize])
        }
        Std(y, offset, r) => (pair(if y { 28 } else { 30 }).wrapping_add(u16::from(offset)), cpu.r[r as usize]),
        // OUT/SBI/CBI cannot address USART0 (the extended I/O range).
        _ => return,
    };
    if !(0xc0..=0xc6).contains(&addr) { return; }
    assert!(serial, "Off without serial writes USART register {addr:#x} at PC {}", cpu.pc);
    assert_ne!(addr, 0xc6, "Off must never write UDR0, even with serial");
    if addr == 0xc1 { assert_eq!(value, 0x90, "Off enables only RXEN/RXCIE, never TXEN/UDRIE"); }
}

pub fn frames(cpu: &Cpu) -> Vec<(watch::Frame, u64)> {
    let mut reader = watch::Reader::new();
    let mut unwrap = watch::Unwrap::new();
    let mut frames = Vec::new();
    for event in &cpu.events {
        if let Event::Tx { byte, .. } = event {
            if let Some(frame) = reader.push(*byte) {
                let frame = frame.expect("every complete watch frame must pass COBS and CRC");
                let tick = unwrap.tick(&frame);
                frames.push((frame, tick));
            }
        }
    }
    assert_eq!(reader.gaps, 0, "queued frames must retain their wire sequence");
    frames
}

pub fn scratch(cpu: &mut Cpu, image: &Image) -> u32 {
    let pc = image.words.len() as u32;
    assert!((pc as usize) + 16 < FLASH_WORDS, "fixture needs sixteen scratch words");
    cpu.flash[pc as usize] = encode(&Nop).unwrap().0[0];
    cpu.flash[pc as usize + 1] = encode(&Break).unwrap().0[0];
    pc
}

pub fn return_to(cpu: &mut Cpu, address: u32) {
    cpu.write(cpu.sp, address as u8); cpu.sp -= 1;
    cpu.write(cpu.sp, (address >> 8) as u8); cpu.sp -= 1;
}

#[derive(Default)]
pub struct Call {
    pub clocks: u64,
    pub services: usize,
    pub samples: Vec<u64>,
    pub effects: Vec<u64>,
    pub stores: Vec<(u16, u8)>,
    pub pushes: Vec<u64>,
}

pub fn invoke(cpu: &mut Cpu, image: &Image, target: u32) -> Call {
    let pc = scratch(cpu, image) + 1;
    let sp = cpu.sp;
    return_to(cpu, pc); cpu.pc = target;
    let start = cpu.cycles;
    let mut result = Call::default();
    while cpu.pc != pc || cpu.sp != sp {
        assert!(cpu.cycles - start < 100_000, "routine must return without wrapping into a long delay");
        if image.layout.records == 0 { assert_unobserved_uart(cpu, image.layout.rx_depth != 0); }
        let before = cpu.pc;
        let cycle = cpu.cycles;
        let z = u16::from_le_bytes([cpu.r[30], cpu.r[31]]);
        cpu.step().expect("called routine must execute");
        let interrupt = cpu.pc == 32 && cpu.cycles == cycle + 4;
        if !interrupt && before == image.symbols.push { result.pushes.push(cycle); }
        if interrupt { result.services += 1; }
        else if before == image.symbols.now48 || (image.layout.rx_depth != 0 && before == image.symbols.rx_sample) {
            result.samples.push(cpu.cycles);
        } else if before == image.symbols.duty_effect || before == image.symbols.write_effect {
            result.effects.push(cpu.cycles); result.stores.push((z, cpu.read(z)));
        }
    }
    result.clocks = cpu.cycles - start;
    result
}

/// Execute the actual foreground encoder through publication and its recheck.
/// The caller supplies an empty TX buffer and a queued frame, as the idle guard does.
pub fn encode_frame(cpu: &mut Cpu, image: &Image) -> u64 {
    let l = &image.layout;
    assert_eq!(cpu.read(l.tx_pos), cpu.read(l.tx_len), "encode requires a drained buffer");
    assert!(cpu.read(l.hello_pending) != 0 || cpu.read(l.head) != cpu.read(l.tail), "encode requires work");
    cpu.r[18] = cpu.read(l.hello_pending); cpu.pc = image.symbols.encode;
    let start = cpu.cycles;
    to_symbol(cpu, image.symbols.idle_check, u64::from(image.bounds.encode) + 2048);
    cpu.cycles - start
}

/// Drain without repeatedly executing the scheduler's empty scan. The actual
/// foreground encoder, UDRE pump and serial shifter still run. Wait for an idle
/// shifter as well as UDRIE-off so a reused CPU has no private transmit state
/// left outstanding; an empty ring with nothing in flight is drained at once.
pub fn drain(cpu: &mut Cpu, image: &Image) -> u64 {
    let start = cpu.cycles;
    if image.layout.records == 0 { return 0; }
    // A caller may stop mid-frame with I set now. Finish any in-flight ISR
    // and foreground encode before borrowing the CPU; otherwise tail/seq
    // could have advanced while the corresponding wire buffer is unfinished.
    let sy = &image.symbols;
    while matches!(cpu.pc, 32 | 36 | 38)
        || (sy.tov..sy.now48).contains(&cpu.pc) || (sy.rx..sy.rx_end).contains(&cpu.pc)
        || (sy.udre..sy.pass).contains(&cpu.pc) || (sy.encode..sy.encode + 2).contains(&cpu.pc)
        || (sy.encode_body..sy.encode_end).contains(&cpu.pc) {
        assert!(cpu.cycles - start < 4096, "in-flight watch work must finish before borrowing the CPU");
        cpu.step().expect("in-flight watch work must complete");
    }
    let sp = cpu.sp;
    let parked = scratch(cpu, image);
    cpu.flash[parked as usize] = encode(&Rjmp(-1)).unwrap().0[0]; cpu.pc = parked;
    cpu.write(0xc4, 0); cpu.write(0xc5, 0); cpu.write(0xc0, 0x42);
    cpu.write(0xc1, if image.layout.rx_depth == 0 { 0x28 } else { 0xb8 }); cpu.sreg |= 128;
    // Every ring record (plus Hello) yields at most one frame: its foreground
    // build, then each wire byte's pump and 80-clock shift. No pass runs here,
    // so a pending Lost count stays pending (see drain_passes in the runtime
    // tests).
    let frame = u64::from(image.bounds.encode + image.bounds.udre) + watch::MAX_WIRE as u64 * (u64::from(image.bounds.udre) + 80);
    let bound = (u64::from(image.layout.records) + 2) * frame;
    // The shifter is idle once its last started byte has a TxDone (or none
    // ever started): TXC alone cannot say so when nothing was left to send.
    let idle = |cpu: &Cpu| !matches!(cpu.events.iter().rev().find(|e| matches!(e, Event::Tx { .. } | Event::TxDone { .. })), Some(Event::Tx { .. }));
    loop {
        if cpu.pc == parked && cpu.sp == sp && cpu.will_execute() {
            let l = &image.layout;
            if cpu.read(l.tx_pos) == cpu.read(l.tx_len)
                && (cpu.read(l.head) != cpu.read(l.tail) || cpu.read(l.hello_pending) != 0) {
                encode_frame(cpu, image);
                cpu.pc = parked;
            }
            if cpu.read(l.head) == cpu.read(l.tail) && cpu.read(l.hello_pending) == 0
                && cpu.read(l.tx_pos) == cpu.read(l.tx_len) && cpu.read(0xc1) & 0x20 == 0 && idle(cpu) { break; }
        }
        cpu.step().expect("queued frames must drain");
        assert!(cpu.cycles - start < bound, "bounded queued frames must finish draining within {bound} cycles");
    }
    cpu.cycles - start
}
