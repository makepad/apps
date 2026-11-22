use fab_elec_mcu_avr::{asm::{Asm, Target}, insn::{decode, Insn::*}};

#[test]
fn relative_edges_relax_only_outside_the_range() {
    for target in [Target::Far, Target::FarCall] {
        for distance in [-2049i32, -2048, 2047, 2048] {
            let mut a = Asm::new();
            let dest = a.label();
            let (from, to) = if distance < 0 {
                a.bind(dest);
                for _ in 0..-distance - 1 { a.emit(Nop); }
                let from = a.here(); a.jump(target, dest);
                (from, 0)
            } else {
                a.jump(target, dest);
                for _ in 0..distance { a.emit(Nop); }
                a.bind(dest);
                (0, a.here())
            };
            assert_eq!(a.relaxable(), 1);
            let end = a.here();
            let (words, relocation) = a.finish_relocated().unwrap();
            let long = !(-2048..=2047).contains(&distance);
            let expected = match (target, long) {
                (Target::Far, false) => Rjmp(distance as i16),
                (Target::FarCall, false) => Rcall(distance as i16),
                (Target::Far, true) => Jmp(relocation.at(to)),
                (Target::FarCall, true) => Call(relocation.at(to)),
                _ => unreachable!(),
            };
            assert_eq!(decode(&words[relocation.at(from) as usize..]), Some((expected, if long { 2 } else { 1 })),
                "{target:?} at displacement {distance}");
            assert_eq!(relocation.at(end), words.len() as u32, "the final boundary maps to the end");
        }
    }
}

#[test]
fn relocation_tracks_cascaded_growth() {
    let mut a = Asm::new();
    let first = a.label(); let second = a.label();
    a.jump(Target::Far, first); // Initially +2047; the next call pushes it out of reach.
    let call = a.here(); a.jump(Target::FarCall, second);
    let after = a.here();
    for _ in 0..2046 { a.emit(Nop); }
    let first_at = a.here(); a.bind(first);
    a.ldi_label(16, 17, first); a.emit(Nop);
    let end = a.here(); a.bind(second);
    assert_eq!(a.relaxable(), 2, "only Far and FarCall count");
    let (words, relocation) = a.finish_relocated().unwrap();
    assert_eq!(relocation.at(0), 0);
    assert_eq!(relocation.at(call), call + 1, "one preceding growth");
    for p in [after, first_at, end] { assert_eq!(relocation.at(p), p + 2, "two preceding growths at {p}"); }
    assert_eq!(decode(&words), Some((Jmp(relocation.at(first_at)), 2)));
    assert_eq!(decode(&words[relocation.at(call) as usize..]), Some((Call(relocation.at(end)), 2)));
    assert_eq!(relocation.at(end), words.len() as u32);
}

#[test]
fn org_absorbs_growth_without_moving_later_boundaries() {
    let mut a = Asm::new();
    let dest = a.label();
    a.jump(Target::Far, dest); a.jump(Target::FarCall, dest);
    let before = a.here(); a.emit(Nop);
    let padding = a.here(); a.org(16);
    let fixed = a.here(); a.org(16); // Duplicate provisional starts select the first item.
    a.jump(Target::Far, dest);
    let after = a.here(); a.emit(Nop);
    a.org(4096); a.bind(dest); a.word(0x1234);
    let end = a.here(); a.org(end); // A zero-width final item also maps the final end.
    let (words, relocation) = a.finish_relocated().unwrap();
    assert_eq!(relocation.at(before), before + 2);
    assert_eq!(relocation.at(padding), padding + 2, "padding begins after grown instructions");
    assert_eq!(relocation.at(fixed), 16, "org consumes earlier growth");
    assert_eq!(relocation.at(after), after + 1, "only growth since org moves this boundary");
    assert_eq!(relocation.at(4096), 4096);
    assert_eq!(words[4096], 0x1234);
    assert_eq!(relocation.at(end), words.len() as u32);
    let (_, empty) = Asm::new().finish_relocated().unwrap();
    assert_eq!(empty.at(0), 0, "an empty image still has an end boundary");
}

#[test]
#[should_panic(expected = "word address 1 is not an item boundary")]
fn relocation_rejects_an_unknown_boundary() {
    let mut a = Asm::new(); a.emit(Jmp(0));
    let (_, relocation) = a.finish_relocated().unwrap();
    relocation.at(1);
}
