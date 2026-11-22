use fab_elec_mcu_avr::sim::{Cpu, Event};
use fab_elec_mcu_emit_avr::{Built, Image};
use fab_elec_mcu_rt::{module::{DeclArg, Module}, ports};

use super::oracle::{return_to, scratch, store};

#[derive(Debug, PartialEq, Eq)]
pub struct Effects {
    records: Vec<Vec<u8>>,
    pins: Vec<(char, u8, bool)>,
    outputs: Vec<u8>,
    eeprom: Vec<u8>,
    faults: Vec<(u8, u64)>,
    timers: Vec<u8>,
}

pub fn inputs(cpu: &mut Cpu, image: &Image, m: &Module<'_>, phase: usize) {
    cpu.sreg = 0;
    // Entry semantics take identical clock/input samples in both images. Stop
    // hardware clocks so instruction-count differences cannot change a sample
    // or add autonomous PWM edges. The real port stubs still execute in full.
    for address in [0x45, 0x81, 0xb1] { cpu.write(address, cpu.read(address) & !7); }
    let ticks = phase as u64 * 250_000;
    cpu.write(0x46, ticks as u8);
    cpu.write(0x35, 0xff); // clear pending timer-0 overflow
    cpu.r[13..16].copy_from_slice(&ticks.to_le_bytes()[1..4]);
    store(cpu, image.layout.clock_hi, ticks >> 32, 2);
    cpu.write(image.layout.clock_b31, (ticks >> 24) as u8 & 0x80);
    let high = phase % 2 != 0;
    for decl in m.decls() {
        if decl.form != ports::FORM_INPUT { continue; }
        if let Some(&DeclArg::Int(pin)) = decl.args.first() {
            let (port, bit) = match pin { 0..=7 => ('D', pin), 8..=13 => ('B', pin - 8), _ => ('C', pin - 14) };
            cpu.set_pin(port, bit as u8, high);
        }
    }
    cpu.set_avcc(5.0);
    for channel in 0..8 { cpu.set_analog(channel, if high { 5.0 } else { 0.0 }); }
    if image.layout.rx_depth != 0 {
        cpu.write(image.layout.rx_active, 1);
        cpu.write(image.layout.rx_byte, super::RX_BYTES[phase % super::RX_BYTES.len()]);
    }
    for r in (0..2).chain(16..28).chain(30..32) { cpu.r[r] = 0x5a ^ r as u8; }
    for i in 0..image.layout.scratch_bytes { cpu.write(image.layout.scratch + i, 0xa5); }
}

pub fn entry(cpu: &mut Cpu, built: &Built, k: usize) -> Effects {
    let image = &built.image;
    let l = &image.layout;
    let saved = cpu.r[2..16].to_vec();
    let ret = scratch(cpu, image) + 1;
    let sp = cpu.sp;
    return_to(cpu, ret);
    cpu.pc = built.map.entries[k].0;
    let start = cpu.cycles;
    let events = cpu.events.len();
    let mut records = Vec::new();
    while cpu.pc != ret || cpu.sp != sp {
        assert!(cpu.cycles - start < built.entries[k].max_cycles,
            "entry {k} must return within its cycle bound");
        if cpu.pc == image.symbols.push {
            // Observe every emitted port/fault record and consume the previous
            // batch. This avoids queue loss during a long, interrupt-masked
            // handler without replacing the producer or any port operation.
            let count = cpu.read(l.record_count);
            assert!(count <= 2);
            if count != 0 {
                records.push((0..u16::from(count) * 8).map(|i| cpu.read(l.record + i)).collect());
            }
            cpu.write(l.tail, cpu.read(l.head));
        }
        cpu.step().expect("entry instruction must execute");
    }
    assert_eq!(&cpu.r[2..16], saved, "entry preserves runtime registers");
    assert_eq!(cpu.sreg & 128, 0, "entry preserves the interrupt mask");
    assert_eq!(super::load(cpu, l.lost, 4), 0, "no effect record may be lost");
    Effects {
        records,
        pins: cpu.events[events..].iter().filter_map(|event| match *event {
            Event::Pin { port, bit, high, .. } => Some((port, bit, high)), _ => None,
        }).collect(),
        // GPIO latches/directions and PWM compare values, independent of time.
        outputs: [0x24, 0x25, 0x27, 0x28, 0x2a, 0x2b, 0x88, 0x89, 0x8a, 0x8b, 0xb3, 0xb4]
            .into_iter().map(|address| cpu.read(address)).collect(),
        eeprom: cpu.eeprom().to_vec(),
        faults: l.fault_cells.iter().map(|cell| (cell.code, super::load(cpu, cell.addr as u16, 4))).collect(),
        timers: (0..u16::from(l.timer_count) * 12).map(|i| cpu.read(l.timers + i)).collect(),
    }
}
