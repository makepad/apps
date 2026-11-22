use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use fab_elec_mcu_avr::{
    asm::{Asm, Target},
    insn::{decode, encode, Insn::*},
    machine::AvrMachine,
    sim::{Cpu, FLASH_WORDS},
};
use fab_elec_mcu_emu::{CodeMap, CondKind, ExecCounts, Machine, PcSet, Ran, Tally, Until};

fn store(a: &mut Asm, addr: u16, value: u8) {
    a.emit(Ldi(16, value)); a.emit(Sts(addr, 16));
}

fn program() -> Vec<u16> {
    let mut a = Asm::new();
    for vector in (0..=50).step_by(2) {
        a.emit(Jmp(match vector { 0 => 64, 28 => 256, _ => 300 }));
    }
    a.org(64);
    for (addr, value) in [(0x24, 1), (0x47, 127), (0x44, 2), (0x6e, 2), (0x45, 1)] {
        store(&mut a, addr, value);
    }
    a.emit(Ldi(16, 0)); a.emit(Ldi(17, 1)); a.emit(Bset(7));
    let top = a.label(); a.bind(top);
    a.emit(Eor(16, 17)); a.emit(Out(5, 16));
    a.emit(Cpse(16, 1)); a.emit(Sts(0x100, 16)); // skip a two-word instruction
    a.emit(Sbrc(16, 0)); a.emit(Nop);
    a.emit(Sbrs(16, 0)); a.emit(Nop);
    a.emit(Sbic(5, 0)); a.emit(Nop);
    a.emit(Sbis(5, 0)); a.emit(Nop);
    a.emit(Cp(16, 1)); a.emit(Brbs(1, 1)); a.emit(Inc(24));
    a.emit(Cp(16, 1)); a.emit(Brbc(1, 1)); a.emit(Inc(25));
    let sub = a.label();
    a.jump(Target::Rcall, sub); a.emit(Call(194));
    a.emit(Ldi(30, 210)); a.emit(Ldi(31, 0)); a.emit(Icall);
    a.emit(Ldi(30, 162)); a.emit(Ijmp);
    a.org(160); a.emit(Nop); a.emit(Nop); // explicit root, dispatch lands inside it
    a.emit(Inc(21));
    a.emit(Ldi(18, 169)); a.emit(Push(18)); // synthetic return into another block's interior
    a.emit(Ldi(18, 0)); a.emit(Push(18)); a.emit(Ret);
    a.emit(Nop); a.emit(Inc(22)); a.jump(Target::Rjmp, top);
    a.org(192); a.bind(sub); a.emit(Inc(26)); a.emit(Ret);
    a.emit(Inc(27)); a.emit(Ret);
    a.org(208); a.emit(Nop); a.emit(Nop); a.emit(Inc(20)); a.emit(Ret);
    a.org(256);
    a.emit(Push(19)); a.emit(In(19, 0x3f)); a.emit(Push(19));
    a.emit(Inc(23)); a.emit(Pop(19)); a.emit(Out(0x3f, 19)); a.emit(Pop(19)); a.emit(Reti);
    a.org(300); a.emit(Break);
    a.finish().unwrap()
}

fn machine(image: &[u16]) -> AvrMachine {
    let mut m = AvrMachine::new(image, 16_000_000);
    m.set_code_roots(&[160]);
    m
}

fn run(m: &mut AvrMachine, tick: u64) {
    assert_eq!(m.run(&Until { tick, marks: &PcSet::default(), stops: &[],
        stops_after: 0, on_drive: false }), Ran::Tick);
}

fn block_at(map: &CodeMap, pc: u32) -> u32 {
    map.blocks.iter().position(|b| b.start <= pc && pc < b.end).map_or(0, |i| i as u32 + 1)
}

fn leader(map: &CodeMap, pc: u32) -> bool {
    map.blocks.binary_search_by_key(&pc, |b| b.start).is_ok()
}

#[test]
fn map_follows_vectors_roots_flow_and_unreached_alignment() {
    let image = program();
    let m = machine(&image);
    let map = m.code_map().unwrap();
    assert!(Arc::ptr_eq(&map, &m.clone().code_map().unwrap()), "clones share the built map");
    assert!(map.blocks.windows(2).all(|b| b[0].end <= b[1].start), "sorted disjoint blocks");
    assert!(map.blocks.iter().all(|b| b.start < b.end && b.end <= image.len() as u32));
    assert_eq!(block_at(&map, FLASH_WORDS as u32 - 1), 0, "erased tail has no block");
    for pc in (0..=50).step_by(2).chain([160]) { assert!(leader(&map, pc), "root {pc}"); }
    let mut pc = 0;
    while pc < image.len() {
        let Some((insn, width)) = decode(&image[pc..]) else { pc += 1; continue; };
        let next = pc as u32 + width as u32;
        let mut required = Vec::new();
        match insn {
            Brbs(_, k) | Brbc(_, k) => {
                required.extend([next, (next as u16).wrapping_add(k as i16 as u16) as u32]);
            }
            Cpse(..) | Sbrc(..) | Sbrs(..) | Sbic(..) | Sbis(..) => {
                let skipped = decode(&image[next as usize..]).unwrap().1 as u32;
                required.extend([next, next + skipped]);
            }
            Rjmp(k) | Rcall(k) => required.extend([next, (next as u16).wrapping_add(k as u16) as u32]),
            Jmp(k) | Call(k) => required.extend([next, k]),
            Ijmp | Icall | Ret | Reti => required.push(next),
            _ => {}
        }
        for dest in required.into_iter().filter(|&p| p < image.len() as u32) {
            assert!(leader(&map, dest), "{insn:?} at {pc}: successor {dest}");
        }
        pc += width;
    }
    assert!(map.conds.windows(2).all(|c| c[0].pc < c[1].pc));
    assert_eq!(map.conds.len(), 7, "both branches and all five skips");
    for cond in &map.conds {
        let block = &map.blocks[block_at(&map, cond.pc) as usize - 1];
        let (insn, width) = decode(&image[cond.pc as usize..]).unwrap();
        assert_eq!(block.end, cond.pc + width as u32, "conditional ends its block");
        assert_eq!(cond.kind, if matches!(insn, Brbs(..) | Brbc(..)) { CondKind::Branch } else { CondKind::Skip });
        assert!(leader(&map, cond.fall) && leader(&map, cond.taken));
    }
    assert!(!leader(&map, 162) && !leader(&map, 169) && !leader(&map, 210), "indirect interior landings");
    assert_ne!(block_at(&map, 210), 0, "the sweep finds the unnamed indirect callee");

    let mut changed = m.clone();
    changed.set_code_roots(&[160, 210, 210]);
    let new_map = changed.code_map().unwrap();
    assert!(!Arc::ptr_eq(&map, &new_map), "roots invalidate the table");
    assert!(leader(&new_map, 210) && !leader(&map, 210), "snapshot retains its roots");
}

#[test]
fn swept_alignment_never_changes_the_execution_entry() {
    let mut image = vec![0; 301];
    let vector = encode(&Jmp(65)).unwrap().0;
    for pc in (0..=50).step_by(2) { image[pc..pc + 2].copy_from_slice(&vector); }
    image[64..66].copy_from_slice(&encode(&Jmp(256)).unwrap().0);
    image[66] = encode(&Ret).unwrap().0[0];
    image[300] = encode(&Break).unwrap().0[0];
    let m = AvrMachine::new(&image, 16_000_000);
    let map = m.code_map().unwrap();
    assert!(leader(&map, 65), "the reached operand is an independent instruction start");
    assert!(!leader(&map, 256), "the swept word at 64 cannot consume reached word 65 or add a jump edge");
    let mut oracle = m.cpu().clone(); oracle.pc = 64;
    let mut deferred = oracle.clone();
    assert_eq!(oracle.step_until(100), deferred.step_deferred(100));
    deferred.catch_up();
    assert_eq!(oracle.pc, 256, "execution still decodes both words at 64");
    assert_eq!(deferred.same_state(&oracle), None);

    let m = AvrMachine::new(&encode(&Lds(16, 0xffff)).unwrap().0, 16_000_000);
    assert_eq!(m.code_map().unwrap().blocks[0].end, 2, "a reached 0xffff operand belongs to its block");
}

#[derive(Default)]
struct Coverage {
    entries: usize,
    mid_reti: usize,
    landings: BTreeSet<&'static str>,
}

// Independent of the interpreter's table annotations and Tally: the oracle's PC
// sequence and public map alone select ids, and ordinary ordered maps sum them.
fn oracle_counts(cpu: &mut Cpu, map: &CodeMap, end: u64) -> (ExecCounts, Coverage) {
    let mut blocks = BTreeMap::<u32, u64>::new();
    let mut conds = BTreeMap::<u32, (u64, u64, bool)>::new();
    let mut coverage = Coverage::default();
    while cpu.cycles < end {
        let pc = cpu.pc;
        let executes = cpu.will_execute();
        let insn = executes.then(|| decode(&cpu.flash[pc as usize..]).unwrap().0);
        if executes && leader(map, pc) { *blocks.entry(block_at(map, pc)).or_default() += 1; }
        assert_eq!(cpu.step_until(end), Ok(()));
        if !executes { coverage.entries += 1; continue; }
        let insn = insn.unwrap();
        if let Some(k) = map.conds.iter().position(|c| c.pc == pc) {
            let cond = &map.conds[k];
            assert_ne!(cond.taken, cond.fall, "fixture outcomes distinguishable by PC");
            assert!(cpu.pc == cond.taken || cpu.pc == cond.fall);
            let taken = cpu.pc == cond.taken;
            let c = conds.entry(k as u32 + 1).or_default();
            if taken { c.0 += 1; } else { c.1 += 1; }
            c.2 = taken;
        }
        if matches!(insn, Ijmp | Icall | Ret) && !leader(map, cpu.pc) {
            *blocks.entry(block_at(map, cpu.pc)).or_default() += 1;
            coverage.landings.insert(match insn { Ijmp => "Ijmp", Icall => "Icall", _ => "Ret" });
        }
        if insn == Reti && !leader(map, cpu.pc) { coverage.mid_reti += 1; }
    }
    (ExecCounts { blocks: blocks.into_iter().collect(),
        conds: conds.into_iter().map(|(id, (t, n, last))| (id, t, n, last)).collect() }, coverage)
}

#[test]
fn machine_counts_match_the_oracles_pc_sequence() {
    let image = program();
    let mut m = machine(&image);
    let map = m.code_map().unwrap();
    let mut oracle = m.cpu().clone();
    let (expected, coverage) = oracle_counts(&mut oracle, &map, 4_000);
    run(&mut m, 4_000);
    oracle.events.clear(); // the machine has drained its public events
    assert_eq!(m.cpu().same_state(&oracle), None, "counting changes no CPU state");
    let mut got = ExecCounts::default(); m.take_counts(&mut got);
    assert_eq!(got, expected);
    assert_eq!(got.conds.len(), 7);
    assert!(got.conds.iter().all(|c| c.1 > 0 && c.2 > 0), "every outcome of every conditional runs");
    assert!(coverage.entries > 0 && coverage.mid_reti > 0, "interrupts return into a block");
    assert_eq!(coverage.landings, BTreeSet::from(["Ijmp", "Icall", "Ret"]));
    let mut empty = ExecCounts::default(); m.take_counts(&mut empty);
    assert_eq!(empty, ExecCounts::default(), "counts drain exactly once");
}

fn merge(out: &mut ExecCounts, next: ExecCounts) {
    for (id, n) in next.blocks {
        match out.blocks.binary_search_by_key(&id, |c| c.0) {
            Ok(k) => out.blocks[k].1 += n, Err(k) => out.blocks.insert(k, (id, n)),
        }
    }
    for (id, t, n, last) in next.conds {
        match out.conds.binary_search_by_key(&id, |c| c.0) {
            Ok(k) => { out.conds[k].1 += t; out.conds[k].2 += n; out.conds[k].3 = last; }
            Err(k) => out.conds.insert(k, (id, t, n, last)),
        }
    }
}

#[test]
fn speculation_and_stops_count_only_execution() {
    let image = program();
    let mut straight = machine(&image); run(&mut straight, 4_000);
    let mut expected = ExecCounts::default(); straight.take_counts(&mut expected);
    let mut m = machine(&image);
    let mut actual = ExecCounts::default();
    m.take_counts(&mut actual); m.mark(); // before the table is built
    run(&mut m, 1_000); m.take_counts(&mut actual); m.mark();
    run(&mut m, 4_000); m.rewind(); run(&mut m, 4_000);
    let mut last = ExecCounts::default(); m.take_counts(&mut last); m.mark();
    merge(&mut actual, last);
    assert_eq!(actual, expected, "a replay contributes exactly once");
    // `drive_at` is the first drive change since the run began (each run clears it), so it
    // differs between runs split at different ticks; everything else must be equal.
    let (mut sliced, mut whole) = (m.cpu().clone(), straight.cpu().clone());
    let _ = (sliced.take_drive_at(), whole.take_drive_at());
    assert_eq!(sliced.same_state(&whole), None);

    let mut m = machine(&image);
    let mut marks = PcSet::new(0, 65); marks.insert(64);
    assert_eq!(m.run(&Until { tick: 100, marks: &marks, stops: &[64], stops_after: 0,
        on_drive: false }), Ran::Pc);
    let map = m.code_map().unwrap(); let id = block_at(&map, 64);
    let mut before = ExecCounts::default(); m.take_counts(&mut before); m.mark();
    assert!(!before.blocks.iter().any(|c| c.0 == id), "stopped instruction has not counted");
    let next = m.now() + 1; run(&mut m, next);
    let mut after = ExecCounts::default(); m.take_counts(&mut after);
    assert_eq!(after.blocks, [(id, 1)], "resuming counts the leader once");
}

#[test]
fn tally_keeps_untaken_marks_and_latest_outcome() {
    let mut tally = Tally::default();
    let map = machine(&program()).code_map().unwrap(); tally.resize(&map);
    tally.block(3); tally.block(0); tally.cond(2, true); tally.mark();
    tally.block(1); tally.cond(2, false); tally.mark();
    tally.block(3); tally.cond(2, true); tally.rewind();
    tally.block(3); tally.cond(1, true);
    let mut got = ExecCounts::default(); tally.take(&mut got);
    assert_eq!(got, ExecCounts { blocks: vec![(0, 1), (1, 1), (3, 2)],
        conds: vec![(1, 1, 0, true), (2, 1, 1, false)] });
    tally.mark(); tally.rewind();
    let mut empty = ExecCounts::default(); tally.take(&mut empty);
    assert_eq!(empty, ExecCounts::default());
}

#[test]
fn coincident_successors_sleep_and_unmapped_landings() {
    let mut a = Asm::new(); a.emit(Jmp(64)); a.org(64);
    a.emit(Bset(1)); a.emit(Brbs(1, 0)); a.emit(Brbc(1, 0));
    a.emit(Cpse(0, 0)); a.word(0xffff); a.emit(Sleep);
    let mut m = machine(&a.finish().unwrap()); run(&mut m, 100);
    let mut counts = ExecCounts::default(); m.take_counts(&mut counts); m.mark();
    assert_eq!(counts.conds, [(1, 1, 0, true), (2, 0, 1, false), (3, 1, 0, true)],
        "actual outcomes survive identical branch successors and a skipped bad encoding");
    run(&mut m, 200);
    let mut asleep = ExecCounts::default(); m.take_counts(&mut asleep);
    assert_eq!(asleep, ExecCounts::default(), "sleep waits execute no instruction");

    let mut a = Asm::new(); a.emit(Jmp(64)); a.org(64);
    a.emit(Ldi(30, 0xfe)); a.emit(Ldi(31, 0x3f)); a.emit(Ijmp);
    let mut m = machine(&a.finish().unwrap()); run(&mut m, 7);
    assert_eq!(m.pc(), FLASH_WORDS as u32 - 2);
    let mut counts = ExecCounts::default(); m.take_counts(&mut counts);
    assert_eq!(counts.blocks.first(), Some(&(0, 1)), "unmapped landing is counted at the transfer");
}
