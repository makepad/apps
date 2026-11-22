//! Entry cycle bounds from emitted words and the emitter's structured ranges.
use std::ops::Range;

use fab_elec_mcu_avr::insn::{cycles, decode, Insn::*};

use crate::image::Image;

/// Ranges close in emission order: children precede their containing range.
pub(crate) enum Region {
    Branch { then: Range<u32>, otherwise: Range<u32> },
    Repeat { body: Range<u32>, count: u32 },
}

pub(crate) fn entry(image: &Image, words: Range<u32>, regions: &[Region]) -> u64 {
    let s = &image.symbols;
    let b = &image.bounds;
    // Published stub bounds include RET, but exclude the caller's CALL/RCALL.
    // A new stub needs its symbol and bound here; unknown targets fail below.
    let mut calls = vec![
        (s.mul32, b.mul32),
        (s.udivmod32, b.udivmod32),
        (s.sdiv32, b.sdiv32),
        (s.srem32, b.srem32),
        (s.shl32, b.shl32),
        (s.shr32, b.shr32),
        (s.sar32, b.sar32),
        (s.write, b.write), (s.read, b.read), (s.duty, b.duty),
        (s.adc, b.adc), (s.adc_convert, b.adc_convert),
        (s.record_write, b.record_write), (s.record_read, b.record_read),
        (s.clock_us, b.clock_us), (s.clock_ms, b.clock_ms),
        (s.timer_stop, b.timer_stop), (s.timer_start, b.timer_start),
        (s.timer_period, b.timer_period), (s.serial_byte, b.serial_byte),
        (s.loop_cap, b.loop_cap), (s.shift_spi, b.shift_spi),
        (s.eeprom_read, b.eeprom_read), (s.eeprom_write, b.eeprom_write),
    ];
    calls.extend(s.shift_bitbang.iter().map(|&(_, start, _)| (start, b.shift_bitbang)));
    calls.retain(|&(address, _)| address != 0);
    let call_bound = |pc, target| {
        let bound = calls.iter().find(|&&(address, _)| address == target)
            .unwrap_or_else(|| panic!("entry CALL at {pc:#x} has no bound for target {target:#x}")).1;
        assert_ne!(bound, 0, "entry CALL target {target:#x} has no published bound");
        u64::from(bound)
    };

    // Prefix sums count every instruction once, including unreachable tails
    // after break/continue. Control ranges below replace sums by max/repeat.
    let mut prefix = vec![0u64; (words.end - words.start + 1) as usize];
    let mut pc = words.start;
    let mut total = 0;
    while pc < words.end {
        let (insn, len) = decode(&image.words[pc as usize..]).expect("emitted entry instruction");
        let (base, extra) = cycles(&insn);
        total += u64::from(base + extra);
        match insn {
            Cpse(..) | Sbrc(..) | Sbrs(..) | Sbic(..) | Sbis(..) => {
                let (_, skipped) = decode(&image.words[pc as usize + len..]).expect("skipped instruction");
                total += (skipped - 1) as u64;
            }
            Call(target) => total += call_bound(pc, target),
            Rcall(k) => total += call_bound(pc, (i64::from(pc) + 1 + i64::from(k)) as u32),
            Icall => panic!("entry call at {pc:#x} needs a target and a published bound"),
            _ => {}
        }
        pc += len as u32;
        prefix[(pc - words.start) as usize] = total;
    }
    assert_eq!(pc, words.end, "entry range ends on an instruction");
    let raw = |r: &Range<u32>| prefix[(r.end - words.start) as usize] - prefix[(r.start - words.start) as usize];
    let mut reduced: Vec<(Range<u32>, u64)> = Vec::new();
    let sum = |r: &Range<u32>, reduced: &mut Vec<(Range<u32>, u64)>| {
        let mut total = raw(r);
        while reduced.last().is_some_and(|(child, _)| child.start >= r.start) {
            let (child, bound) = reduced.pop().unwrap();
            assert!(child.end <= r.end, "emitted control ranges must nest");
            total = total - raw(&child) + bound;
        }
        total
    };
    for region in regions {
        let (range, bound) = match region {
            Region::Branch { then, otherwise } => {
                assert_eq!(then.end, otherwise.start, "branch arms are adjacent");
                let no = sum(otherwise, &mut reduced);
                let yes = sum(then, &mut reduced);
                (then.start..otherwise.end, yes.max(no))
            }
            Region::Repeat { body, count } => (body.clone(), sum(body, &mut reduced) * u64::from(*count)),
        };
        reduced.push((range, bound));
    }
    sum(&words, &mut reduced)
}

/// Longest handler plus the JMP back to pass and rx_redispatch(): the computed
/// pass/scan/lock/bookkeeping/dispatch bound, with the minimum late delay.
/// Interrupts are charged only by the fixed point, never in the entry bound.
pub(crate) fn stretch(image: &Image, longest: u64, setting: fab_elec_mcu_rt::serial::SerialSetting)
    -> Result<(u32, u64), String> {
    let byte = setting.byte_cycles(fab_elec_mcu_rt::serial::ATMEGA328P.frame_bits);
    let tov = u64::from(crate::runtime::TOV_INT);
    let uart = u64::from(image.bounds.rx_int) + u64::from(image.bounds.udre);
    if tov * byte + uart * 2048 >= 2048 * byte {
        return Err(format!("serial {} baud: interrupt service load reaches or exceeds one", setting.asked));
    }
    let base = u128::from(longest) + 3 + u128::from(image.bounds.rx_redispatch());
    let mut t = base;
    loop {
        let next = base + u128::from(tov) * t.div_ceil(2048)
            + u128::from(uart) * t.div_ceil(u128::from(byte));
        if next == t { break; }
        t = next;
    }
    let overflow = || format!("serial {} baud: RX FIFO depth or stretch exceeds its numeric range", setting.asked);
    let depth = u32::try_from(t.div_ceil(u128::from(byte)) + 2).map_err(|_| overflow())?;
    Ok((depth, u64::try_from(t).map_err(|_| overflow())?))
}
