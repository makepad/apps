//! Target-neutral control-flow facts for verified module AIR.
use fab_elec_mcu_rt::{format::*, module::Module};

#[derive(Clone, Copy)]
pub struct Instruction<'a> { pub at: u32, pub op: u32, pub sub: u32, pub args: &'a [u32], pub end: u32 }
#[derive(Clone, Copy)]
pub enum Step<'a> { Op(Instruction<'a>), Else, EndIf, EndLoop }
enum Finish { Root, Then(u32), Else, Loop }

// The same iterative block walk as emit_avr; no Rust recursion on AIR depth.
pub fn steps<'a>(m: &Module<'a>, k: usize) -> Vec<Step<'a>> {
    let code = m.code();
    let e = &m.entries()[k];
    let mut pc = e.body + 1;
    let mut stack = vec![(e.code + e.len, Finish::Root)];
    let mut out = Vec::new();
    while let Some((end, _)) = stack.last() {
        if pc == *end {
            match stack.pop().unwrap().1 {
                Finish::Root => break,
                Finish::Then(end) => { pc += 1; stack.push((end, Finish::Else)); out.push(Step::Else); }
                Finish::Else => out.push(Step::EndIf),
                Finish::Loop => out.push(Step::EndLoop),
            }
            continue;
        }
        let at = pc;
        let word = code[pc as usize];
        let op = word & OP_MASK;
        let count = match op {
            OP_CONST | OP_GET | OP_UN | OP_SET => 2,
            OP_CONST_D | OP_BIN | OP_CMP_F | OP_CMP_I | OP_CMP_D | OP_WRAP => 3,
            OP_LOAD | OP_STORE => 4 + u32::from(word >> SUB_SHIFT == REGION_BUF_LONG),
            OP_SEL | OP_FMA => 4,
            OP_PORT => 4 + code[pc as usize + 3] + code[pc as usize + 4],
            OP_IF | OP_LOOP | OP_BREAK | OP_CONTINUE => 1,
            _ => unreachable!("verified opcode"),
        };
        let args = &code[pc as usize + 1..(pc + count + 1) as usize];
        pc += count + 1;
        let mut end = pc;
        if op == OP_IF {
            let otherwise = pc + 1 + code[pc as usize];
            end = otherwise + 1 + code[otherwise as usize];
            stack.push((otherwise, Finish::Then(end))); pc += 1;
        } else if op == OP_LOOP {
            end = pc + 1 + code[pc as usize];
            stack.push((end, Finish::Loop)); pc += 1;
        }
        out.push(Step::Op(Instruction { at, op, sub: word >> SUB_SHIFT, args, end }));
    }
    out
}

pub fn pure(i: Instruction<'_>) -> bool { (OP_CONST..=OP_FMA).contains(&i.op) }

pub fn reads(i: Instruction<'_>) -> Vec<u32> {
    let a = i.args;
    match i.op {
        OP_SET | OP_UN | OP_WRAP => vec![a[1]],
        OP_BIN | OP_CMP_I | OP_CMP_F | OP_CMP_D => vec![a[1], a[2]],
        OP_SEL | OP_FMA => a[1..4].to_vec(),
        OP_LOAD => if a[3] != NONE { vec![a[3]] } else { vec![] },
        OP_STORE => if a[2] != NONE { vec![a[2], a[3]] } else { vec![a[3]] },
        OP_IF => vec![a[0]],
        OP_PORT => a[4..4 + a[2] as usize].to_vec(),
        _ => vec![],
    }
}

/// The head GET aliases `var`; all eight head steps through SET disappear.
/// `limit` is exclusive, including for a source CMP_GT. No signed wrap is
/// possible at the moved increment. A short cap still needs its old counter.
/// Bounds proofs also depend on recognition requiring a constant limit and
/// rejecting every counter SET except the recognised head increment.
#[derive(Clone, Copy, Debug)]
pub struct CountedLoop {
    pub var: u32,
    pub value: u32,
    pub start: i32,
    pub limit: i32,
    pub trips: u64,
    pub needs_counter: bool,
}

/// A low-bit mask, optionally consuming a single-use constant right shift.
/// Both shift kinds select the same source bits when shift + width <= 32:
/// the mask removes every sign-filled bit of an arithmetic right shift.
/// Width is target-neutral (1..32). A backend accepting this extraction
/// reads `source` at the mask and omits `shift_step`, if present.
#[derive(Clone, Copy, Debug)]
pub struct BitField {
    pub source: u32,
    pub shift: u8,
    pub width: u8,
    pub shift_step: Option<usize>,
}

/// Facts indexed by the original iterative step sequence. Omitted steps do
/// not participate in allocation or emission. A fused IF reads the compare's
/// operands at the IF, never its boolean result. Transfers omit the three
/// following steps (BREAK/CONTINUE, Else, EndIf), leaving no branch scope.
pub struct Analysis<'a> {
    pub constants: Vec<Option<u32>>,
    pub bitfields: Vec<Option<BitField>>,
    pub fused: Vec<Option<Instruction<'a>>>,
    pub transfers: Vec<Option<Instruction<'a>>>,
    pub counted: Vec<Option<CountedLoop>>,
    pub omit: Vec<bool>,
    pub aliases: Vec<Option<u32>>,
}

impl<'a> Analysis<'a> {
    pub fn new(m: &Module<'a>, k: usize, steps: &[Step<'a>]) -> Self {
        let nv = m.entries()[k].n_vals as usize;
        let mut out = Self {
            constants: vec![None; nv], fused: vec![None; steps.len()],
            bitfields: vec![None; steps.len()],
            transfers: vec![None; steps.len()], counted: vec![None; steps.len()],
            omit: vec![false; steps.len()], aliases: vec![None; nv],
        };
        let mut uses = vec![0usize; nv];
        let mut definitions = vec![None; nv];
        let mut gets = vec![0usize; m.entries()[k].n_vars as usize];
        for (n, &step) in steps.iter().enumerate() {
            if let Step::Op(i) = step {
                if pure(i) { definitions[i.args[0] as usize] = Some(n); }
                if i.op == OP_CONST { out.constants[i.args[0] as usize] = Some(i.args[1]); }
                if i.op == OP_GET { gets[i.args[1] as usize] += 1; }
                for v in reads(i) { uses[v as usize] += 1; }
            }
        }
        // Recognise against the original steps, before marking any fusion.
        for (n, &step) in steps.iter().enumerate() {
            let Step::Op(i) = step else { continue; };
            if i.op != OP_LOOP { continue; }
            let Some(c) = out.counted_loop(steps, n, &uses, &gets) else { continue; };
            out.counted[n] = Some(c);
            out.aliases[c.value as usize] = Some(c.var);
            out.omit[n + 1..n + 9].fill(true);
        }
        for (n, &step) in steps.iter().enumerate() {
            if out.omit[n] { continue; }
            let Step::Op(i) = step else { continue; };
            if i.op == OP_BIN && i.sub == BIN_AND_I {
                out.bitfields[n] = out.bitfield(i, steps, &uses, &definitions);
            }
            if i.op == OP_CMP_I && uses[i.args[0] as usize] == 1 {
                if let Some(Step::Op(next)) = steps.get(n + 1) {
                    if next.op == OP_IF && next.args[0] == i.args[0] {
                        out.fused[n + 1] = Some(i);
                        out.omit[n] = true;
                    }
                }
            }
            if i.op == OP_IF {
                if let Some([Step::Op(transfer), Step::Else, Step::EndIf]) = steps.get(n + 1..n + 4) {
                    if matches!(transfer.op, OP_BREAK | OP_CONTINUE) {
                        out.transfers[n] = Some(*transfer);
                        out.omit[n + 1..n + 4].fill(true);
                    }
                }
            }
        }
        out
    }

    fn bitfield(&self, i: Instruction<'_>, steps: &[Step<'_>], uses: &[usize], definitions: &[Option<usize>]) -> Option<BitField> {
        // Prefer the narrower mask if both operands are constant low masks.
        let (source, width) = [(2, 1), (1, 2)].into_iter().filter_map(|(constant, source)| {
            let mask = self.constants[i.args[constant] as usize]?;
            (mask != 0 && mask & mask.wrapping_add(1) == 0)
                .then_some((i.args[source], mask.count_ones() as u8))
        }).min_by_key(|&(_, width)| width)?;
        let bare = BitField { source, shift: 0, width, shift_step: None };
        if uses[source as usize] != 1 { return Some(bare); }
        let Some(def) = definitions[source as usize] else { return Some(bare); };
        let Step::Op(shift) = steps[def] else { return Some(bare); };
        if self.omit[def] || shift.op != OP_BIN || !matches!(shift.sub, BIN_SHR_I | BIN_SHR_UI) {
            return Some(bare);
        }
        let Some(amount) = self.constants[shift.args[2] as usize] else { return Some(bare); };
        // AIR uses wrapping shifts, including negative counts.
        let amount = (amount & 31) as u8;
        if amount + width > 32 { return Some(bare); }
        Some(BitField { source: shift.args[1], shift: amount, width, shift_step: Some(def) })
    }

    fn counted_loop(&self, steps: &[Step<'a>], n: usize, uses: &[usize], gets: &[usize]) -> Option<CountedLoop> {
        let Step::Op(loop_op) = steps[n] else { return None; };
        let [Step::Op(get), Step::Op(cmp), Step::Op(test), Step::Op(leave),
            Step::Else, Step::EndIf, Step::Op(add), Step::Op(set)] = steps.get(n + 1..n + 9)? else { return None; };
        if get.op != OP_GET || cmp.op != OP_CMP_I || !matches!(cmp.sub, CMP_GE | CMP_GT)
            || cmp.args[1] != get.args[0] || test.op != OP_IF || test.args[0] != cmp.args[0]
            || leave.op != OP_BREAK || leave.args[0] != 0
            || add.op != OP_BIN || add.sub != BIN_ADD_I
            || set.op != OP_SET || set.args != [get.args[1], add.args[0]]
            || uses[cmp.args[0] as usize] != 1 || uses[add.args[0] as usize] != 1
            || gets[get.args[1] as usize] != 1
        { return None; }
        // Unit stride is deliberately the only recognised increment for C1.
        if ![(1, 2), (2, 1)].into_iter().any(|(a, b)|
            add.args[a] == get.args[0] && self.constants[add.args[b] as usize] == Some(1))
        { return None; }
        let bound = self.constants[cmp.args[2] as usize]? as i32;
        let limit = if cmp.sub == CMP_GT { bound.checked_add(1)? } else { bound };
        // Find a dominating SET in this straight-line preheader. Constants
        // and other ordinary operations may have been hoisted between it and
        // LOOP; crossing a block boundary would require dominance analysis.
        let mut initial = None;
        for &step in steps[..n].iter().rev() {
            match step {
                Step::Op(i) if i.op == OP_SET && i.args[0] == get.args[1] => {
                    initial = self.constants[i.args[1] as usize]; break;
                }
                Step::Op(i) if !matches!(i.op, OP_IF | OP_LOOP | OP_BREAK | OP_CONTINUE) => {}
                _ => break,
            }
        }
        let start = initial? as i32;
        let trips = (i64::from(limit) - i64::from(start)).max(0) as u64;
        let mut depth = 0u32;
        for &step in &steps[n + 9..] {
            match step {
                Step::EndLoop if depth == 0 => break,
                Step::EndLoop => depth -= 1,
                Step::Op(i) if i.op == OP_LOOP => depth += 1,
                Step::Op(i) if (i.op == OP_SET && i.args[0] == get.args[1])
                    || (i.op == OP_CONTINUE && i.args[0] == depth) => return None,
                _ => {}
            }
        }
        // Also protect the GET value itself if a future AIR verifier admits
        // uses outside the loop's lexical scope.
        if steps.iter().any(|s| matches!(s, Step::Op(i) if (i.at < loop_op.at || i.at >= loop_op.end)
            && reads(*i).contains(&get.args[0]))) { return None; }
        Some(CountedLoop { var: get.args[1], value: get.args[0], start, limit, trips,
            needs_counter: trips + 1 > u64::from(loop_op.args[0]) })
    }
}

