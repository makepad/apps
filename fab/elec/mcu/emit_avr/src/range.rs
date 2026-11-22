//! Signed i32 intervals on the reference AIR. No code generation depends on
//! these facts yet. State starts at state_init; external state pokes are not
//! part of the proof. Port bounds are those of the ATmega328P device profile.
use fab_elec_mcu_rt::{analog, format::*, module::Module, ports};

use crate::emit::{analysis_plan, pure, AnalysisPlan, Folded, Instruction, Step};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Interval { pub lo: i32, pub hi: i32 }

impl Interval {
    pub const FULL: Self = Self { lo: i32::MIN, hi: i32::MAX };
    pub const BOOL: Self = Self { lo: 0, hi: 1 };
    // Bottom is internal: a contribution not reached by the fixpoint yet.
    const EMPTY: Self = Self { lo: i32::MAX, hi: i32::MIN };

    pub const fn point(n: i32) -> Self { Self { lo: n, hi: n } }
    pub fn contains(self, n: i32) -> bool { self.lo <= n && n <= self.hi }
    fn empty(self) -> bool { self.lo > self.hi }
    fn constant(self) -> Option<i32> { (self.lo == self.hi).then_some(self.lo) }
    fn join(self, other: Self) -> Self {
        Self { lo: self.lo.min(other.lo), hi: self.hi.max(other.hi) }
    }
    fn bounds(lo: i64, hi: i64) -> Self {
        if lo < i64::from(i32::MIN) || hi > i64::from(i32::MAX) { Self::FULL }
        else { Self { lo: lo as i32, hi: hi as i32 } }
    }
    /// Exact growth: the join, for facts that only follow other facts.
    fn grow(&mut self, next: Self) -> bool {
        let old = *self;
        *self = self.join(next);
        old != *self
    }
    fn widen(&mut self, next: Self) -> bool {
        let old = *self;
        if self.empty() { *self = next; }
        else if !next.empty() {
            if next.lo < self.lo {
                self.lo = [0, -128, -32768, i32::MIN].into_iter().find(|&n| n <= next.lo).unwrap();
            }
            if next.hi > self.hi {
                self.hi = [1, 127, 255, 32767, 65535, i32::MAX].into_iter().find(|&n| n >= next.hi).unwrap();
            }
        }
        old != *self
    }
}

fn unary(op: u32, a: Interval) -> Interval {
    if a.empty() { return a; }
    match op {
        UN_NEG_I => Interval::bounds(-i64::from(a.hi), -i64::from(a.lo)),
        UN_NOT_B => Interval::BOOL,
        _ => Interval::FULL,
    }
}

fn binary(op: u32, a: Interval, b: Interval) -> Interval {
    if a.empty() || b.empty() { return Interval::EMPTY; }
    let (lo, hi, bl, bh) = (i64::from(a.lo), i64::from(a.hi), i64::from(b.lo), i64::from(b.hi));
    match op {
        BIN_ADD_I => Interval::bounds(lo + bl, hi + bh),
        BIN_SUB_I => Interval::bounds(lo - bh, hi - bl),
        BIN_MUL_I => {
            let products = [lo * bl, lo * bh, hi * bl, hi * bh];
            Interval::bounds(*products.iter().min().unwrap(), *products.iter().max().unwrap())
        }
        BIN_DIV_I if b.constant().is_some() => {
            if bl == 0 { Interval::point(0) }
            else { Interval::bounds((lo / bl).min(hi / bl), (lo / bl).max(hi / bl)) }
        }
        BIN_REM_I if b.constant().is_some_and(|k| k > 0 && (k as u32).is_power_of_two()) => {
            Interval { lo: if a.lo >= 0 { 0 } else { -(b.hi - 1) }, hi: b.hi - 1 }
        }
        BIN_AND_I => {
            if let Some(k) = a.constant().filter(|&k| k >= 0).or_else(|| b.constant().filter(|&k| k >= 0)) {
                Interval { lo: 0, hi: k }
            } else if a.lo >= 0 && b.lo >= 0 { Interval { lo: 0, hi: a.hi.min(b.hi) } }
            else { Interval::FULL }
        }
        BIN_OR_I | BIN_XOR_I if a.lo >= 0 && b.lo >= 0 => {
            let top = (a.hi.max(b.hi) as u32 + 1).next_power_of_two() - 1;
            Interval { lo: 0, hi: top as i32 }
        }
        BIN_SHL_I | BIN_SHR_I | BIN_SHR_UI if b.constant().is_some() => {
            let shift = b.lo as u32 & 31;
            match op {
                BIN_SHL_I => Interval::bounds(lo * (1i64 << shift), hi * (1i64 << shift)),
                BIN_SHR_I => Interval { lo: a.lo >> shift, hi: a.hi >> shift },
                _ if a.lo >= 0 => Interval { lo: a.lo >> shift, hi: a.hi >> shift },
                _ => Interval::FULL,
            }
        }
        BIN_AND_B | BIN_OR_B => Interval::BOOL,
        _ => Interval::FULL,
    }
}

#[derive(Debug)]
pub struct EntryRanges {
    /// Indexed by SSA value ID. Dead definitions are conservatively full.
    pub values: Vec<Interval>,
    pub vars: Vec<Interval>,
    /// Census membership: live definitions, excluding immediate homes.
    pub live: Vec<bool>,
}

#[derive(Debug)]
pub struct Ranges { pub entries: Vec<EntryRanges>, pub state: Vec<Interval> }

/// Disjoint classes in priority order (unsigned byte first, then signed byte).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Classes { pub u8: usize, pub i8: usize, pub u16: usize, pub i16: usize, pub i32: usize }

impl Classes {
    fn add(&mut self, r: Interval) {
        if r.lo >= 0 && r.hi <= 255 { self.u8 += 1; }
        else if r.lo >= -128 && r.hi <= 127 { self.i8 += 1; }
        else if r.lo >= 0 && r.hi <= 65535 { self.u16 += 1; }
        else if r.lo >= -32768 && r.hi <= 32767 { self.i16 += 1; }
        else { self.i32 += 1; }
    }
}

#[derive(Debug)]
pub struct EntryCensus { pub values: Classes, pub vars: Classes }
#[derive(Debug)]
pub struct Census { pub entries: Vec<EntryCensus>, pub state: Classes }

pub fn census(ranges: &Ranges) -> Census {
    let entries = ranges.entries.iter().map(|e| {
        let mut values = Classes::default();
        for (&r, &live) in e.values.iter().zip(&e.live) { if live { values.add(r); } }
        let mut vars = Classes::default();
        for &r in &e.vars { vars.add(r); }
        EntryCensus { values, vars }
    }).collect();
    let mut state = Classes::default();
    for &r in &ranges.state { state.add(r); }
    Census { entries, state }
}

struct Trip { step: usize, var: usize, delta: i64 }

// Only the unique, direct get(var) + nonnegative constant assignment in a
// loop is accelerated. Nested loops require a dominating reset since the
// parent iteration began; without one the inner cap is not a lifetime cap.
// The get must execute inside the loop too: a captured pre-reset value can
// otherwise carry the counter across repeated inner-loop invocations.
fn trips(plan: &AnalysisPlan<'_>, n_vals: usize) -> Vec<Trip> {
    let mut defs = vec![None; n_vals];
    let mut loops = Vec::<(usize, u32)>::new();
    let mut scopes = Vec::<(usize, bool)>::new();
    let mut contexts = Vec::new();
    let mut sets = Vec::new();
    for (s, step) in plan.steps.iter().enumerate() {
        contexts.push(scopes.clone());
        match *step {
            Step::Op(i) => {
                if pure(&i) { defs[i.args[0] as usize] = Some(i); }
                if i.op == OP_SET { sets.push((s, i, loops.clone())); }
                if i.op == OP_LOOP { loops.push((s, i.args[0])); scopes.push((s, false)); }
                if i.op == OP_IF { scopes.push((s, false)); }
            }
            Step::Else => scopes.last_mut().unwrap().1 = true,
            Step::EndIf => { scopes.pop(); }
            Step::EndLoop => { scopes.pop(); loops.pop(); }
        }
    }
    let mut out = Vec::new();
    for &(s, i, ref nest) in &sets {
        let Some(&(lp, cap)) = nest.last() else { continue };
        let var = i.args[0];
        let Step::Op(loop_op) = plan.steps[lp] else { unreachable!("loop opener") };
        if sets.iter().filter(|(_, j, ls)| j.args[0] == var && ls.iter().any(|&(l, _)| l == lp)).count() != 1 { continue; }
        if nest.len() > 1 {
            let parent = nest[nest.len() - 2].0;
            if !sets.iter().any(|(t, j, _)| *t > parent && *t < lp && j.args[0] == var
                && contexts[lp].starts_with(&contexts[*t])) { continue; }
        }
        let Some(add) = defs[i.args[1] as usize] else { continue };
        if add.op != OP_BIN || add.sub != BIN_ADD_I { continue; }
        for (get, c) in [(add.args[1], add.args[2]), (add.args[2], add.args[1])] {
            let (Some(get), Some(c)) = (defs[get as usize], defs[c as usize]) else { continue };
            if get.op != OP_GET || get.args[1] != var || get.at <= loop_op.at || c.op != OP_CONST || (c.args[1] as i32) < 0 { continue; }
            out.push(Trip { step: s, var: var as usize, delta: i64::from(cap) * i64::from(c.args[1]) });
            break;
        }
    }
    out
}

// The reference interprets the index as u32 and clamps it to extent - 1,
// including negative i32 indices. The bounds of a computed index are not
// guard refinement: B1 deliberately joins its entire extent.
fn extent(base: u32, len: u32, index: u32, values: &[Interval]) -> std::ops::Range<usize> {
    let offset = if index == NONE { Some(0) } else { values[index as usize].constant().map(|k| k as u32) };
    if let Some(off) = offset {
        let at = base as usize + off.min(len - 1) as usize;
        at..at + 1
    } else { base as usize..(base + len) as usize }
}

fn value(m: &Module<'_>, k: usize, i: Instruction<'_>, plan: &AnalysisPlan<'_>, e: &EntryRanges, state: &[Interval]) -> Interval {
    let a = i.args;
    let ty = m.ty(k, a[0], false);
    if ty == TY_BOOL { return Interval::BOOL; }
    if ty != TY_I32 { return Interval::FULL; }
    // An existing Imm home is a constant even when the emitter obtained it
    // by folding. Other float conversions stay full, including folded selects.
    if let Some(Folded::K(n)) = plan.folded[a[0] as usize] { return Interval::point(n as i32); }
    let v = |n: u32| e.values[n as usize];
    match i.op {
        OP_CONST => Interval::point(a[1] as i32),
        OP_GET => e.vars[a[1] as usize],
        OP_UN => unary(i.sub, v(a[1])),
        OP_BIN => binary(i.sub, v(a[1]), v(a[2])),
        OP_WRAP => Interval::bounds(0, i64::from(a[2]) - 1),
        OP_SEL => v(a[2]).join(v(a[3])),
        OP_FMA => binary(BIN_ADD_I, binary(BIN_MUL_I, v(a[1]), v(a[2])), v(a[3])),
        OP_LOAD if matches!(i.sub, REGION_STATE | REGION_SHARED) => {
            let words = extent(a[1], a[2], a[3], &e.values);
            match i.sub {
                REGION_STATE => words.map(|n| state[n]).fold(Interval::EMPTY, Interval::join),
                REGION_SHARED => words.map(|n| Interval::point(m.shared()[n] as i32)).fold(Interval::EMPTY, Interval::join),
                _ => Interval::FULL,
            }
        }
        _ => Interval::FULL,
    }
}

/// Proves intervals for the AVR reference profile. Variables, their sets and
/// state words carry facts from one round to the next, so their growth is
/// rounded outwards to a storage threshold; no iteration budget affects the
/// result. A value is a forward function of those facts (operands precede uses)
/// and joins exactly: it settles in the round after they do.
pub fn analyse(m: &Module<'_>) -> Ranges {
    let plans: Vec<_> = (0..m.entries().len()).map(|k| analysis_plan(m, k)).collect();
    let trips: Vec<_> = plans.iter().zip(m.entries()).map(|(p, e)| trips(p, e.n_vals as usize)).collect();
    let mut ranges = Ranges {
        entries: plans.iter().zip(m.entries()).enumerate().map(|(k, (p, e))| {
            let mut values = vec![Interval::FULL; e.n_vals as usize];
            let mut live = vec![false; values.len()];
            for (s, step) in p.steps.iter().enumerate() {
                let Step::Op(i) = step else { continue };
                if p.dead[s] { continue; }
                if pure(i) {
                    let v = i.args[0] as usize;
                    values[v] = Interval::EMPTY;
                    let immediate = matches!(m.ty(k, v as u32, false), TY_I32 | TY_BOOL)
                        && (i.op == OP_CONST || matches!(p.folded[v], Some(Folded::K(_))));
                    live[v] = !immediate;
                }
                if i.op == OP_PORT {
                    for &v in &i.args[4 + i.args[2] as usize..] { values[v as usize] = Interval::EMPTY; live[v as usize] = true; }
                }
            }
            EntryRanges { values, live, vars: p.zero_vars.iter().map(|&zero| if zero { Interval::point(0) } else { Interval::EMPTY }).collect() }
        }).collect(),
        state: m.state_init().iter().map(|&n| Interval::point(n as i32)).collect(),
    };
    // A contribution per set is needed to exclude precisely the trip's own
    // backedge, while retaining every other assignment (including other loops).
    let mut contributions: Vec<Vec<Interval>> = plans.iter().map(|p| vec![Interval::EMPTY; p.steps.len()]).collect();
    loop {
        let mut changed = false;
        let mut state = ranges.state.clone();
        for (k, plan) in plans.iter().enumerate() {
            let e = &mut ranges.entries[k];
            for (s, step) in plan.steps.iter().enumerate() {
                if plan.dead[s] { continue; }
                let Step::Op(i) = *step else { continue };
                let a = i.args;
                if pure(&i) {
                    let next = value(m, k, i, plan, e, &ranges.state);
                    changed |= e.values[a[0] as usize].grow(next);
                } else if i.op == OP_PORT {
                    for (ret, &v) in a[4 + a[2] as usize..].iter().enumerate() {
                        let (lo, hi) = ports::ret_interval(a[0] as u16, ret, analog::ATMEGA328P.max());
                        changed |= e.values[v as usize].grow(Interval { lo, hi });
                    }
                } else if i.op == OP_SET && !trips[k].iter().any(|t| t.step == s) {
                    changed |= contributions[k][s].widen(e.values[a[1] as usize]);
                } else if i.op == OP_STORE && i.sub == REGION_STATE {
                    let v = e.values[a[3] as usize];
                    for word in extent(a[0], a[1], a[2], &e.values) { state[word] = state[word].join(v); }
                }
            }
            for trip in &trips[k] {
                let mut base = if plan.zero_vars[trip.var] { Interval::point(0) } else { Interval::EMPTY };
                for (s, step) in plan.steps.iter().enumerate() {
                    if let Step::Op(i) = step {
                        if i.op == OP_SET && i.args[0] as usize == trip.var && s != trip.step { base = base.join(contributions[k][s]); }
                    }
                }
                if !base.empty() {
                    changed |= contributions[k][trip.step].widen(Interval::bounds(i64::from(base.lo), i64::from(base.hi) + trip.delta));
                }
            }
            let mut vars = e.vars.clone();
            for (s, step) in plan.steps.iter().enumerate() {
                if let Step::Op(i) = step {
                    if i.op == OP_SET {
                        let var = &mut vars[i.args[0] as usize];
                        *var = var.join(contributions[k][s]);
                    }
                }
            }
            for (old, next) in e.vars.iter_mut().zip(vars) { changed |= old.widen(next); }
        }
        for (old, next) in ranges.state.iter_mut().zip(state) { changed |= old.widen(next); }
        if !changed { break; }
    }
    // Unreachable / unused slots have no facts, and are never an empty public
    // promise. The reference still evaluates dead definitions, which stay full.
    for e in &mut ranges.entries {
        for r in e.values.iter_mut().chain(&mut e.vars) { if r.empty() { *r = Interval::FULL; } }
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_rules_at_storage_edges_and_overflow() {
        let r = |lo, hi| Interval { lo, hi };
        let p = Interval::point;
        for (a, b, sum) in [(127, 1, 128), (255, 1, 256), (32767, 1, 32768), (65535, 1, 65536), (-128, -1, -129), (-32768, -1, -32769)] {
            assert_eq!(binary(BIN_ADD_I, p(a), p(b)), p(sum), "addition stays exact across storage edges");
            assert_eq!(binary(BIN_SUB_I, p(sum), p(b)), p(a));
        }
        assert_eq!(binary(BIN_ADD_I, p(i32::MAX), p(1)), Interval::FULL);
        assert_eq!(binary(BIN_SUB_I, p(i32::MIN), p(1)), Interval::FULL);
        assert_eq!(unary(UN_NEG_I, r(-128, 127)), r(-127, 128));
        assert_eq!(unary(UN_NEG_I, r(i32::MIN, 0)), Interval::FULL);
        assert_eq!(binary(BIN_MUL_I, r(-3, 4), r(-5, 6)), r(-20, 24));
        assert_eq!(binary(BIN_MUL_I, p(65536), p(65536)), Interval::FULL);
        assert_eq!(binary(BIN_DIV_I, r(-7, 8), p(-2)), r(-4, 3));
        assert_eq!(binary(BIN_DIV_I, Interval::FULL, p(0)), p(0));
        assert_eq!(binary(BIN_DIV_I, p(i32::MIN), p(-1)), Interval::FULL);
        assert_eq!(binary(BIN_DIV_I, r(0, 10), r(1, 2)), Interval::FULL);
        assert_eq!(binary(BIN_REM_I, Interval::FULL, p(128)), r(-127, 127));
        assert_eq!(binary(BIN_REM_I, r(0, i32::MAX), p(256)), r(0, 255));
        assert_eq!(binary(BIN_REM_I, Interval::FULL, p(3)), Interval::FULL);
        assert_eq!(binary(BIN_AND_I, Interval::FULL, p(255)), r(0, 255));
        assert_eq!(binary(BIN_AND_I, r(1, 7), r(2, 12)), r(0, 7));
        for op in [BIN_OR_I, BIN_XOR_I] {
            for hi in [0, 1, 127, 128, 255, 256, 32767, 32768, 65535, i32::MAX] {
                let expected = ((hi as u32 + 1).next_power_of_two() - 1) as i32;
                assert_eq!(binary(op, r(0, hi), r(0, hi)), r(0, expected));
            }
            assert_eq!(binary(op, r(-1, 1), r(0, 3)), Interval::FULL);
        }
        assert_eq!(binary(BIN_SHL_I, r(-128, 127), p(24)), r(i32::MIN, 127 << 24));
        assert_eq!(binary(BIN_SHL_I, r(0, 128), p(24)), Interval::FULL);
        assert_eq!(binary(BIN_SHL_I, r(-2, 3), p(33)), r(-4, 6));
        assert_eq!(binary(BIN_SHR_I, r(-129, 128), p(1)), r(-65, 64));
        assert_eq!(binary(BIN_SHR_UI, r(-1, 128), p(1)), Interval::FULL);
        assert_eq!(binary(BIN_SHR_UI, r(0, 65535), p(8)), r(0, 255));
        assert_eq!(binary(BIN_SHR_I, Interval::FULL, p(-1)), r(-1, 0));
        assert_eq!(unary(UN_F2I, r(0, 1)), Interval::FULL);
        assert_eq!(r(-5, 1).join(r(2, 7)), r(-5, 7), "select joins both arms");
        assert_eq!(binary(BIN_ADD_I, Interval::EMPTY, p(1)), Interval::EMPTY);
    }

    #[test]
    fn widening_crosses_only_finitely_many_thresholds() {
        let mut upper = Interval::point(0);
        for threshold in [1, 127, 255, 32767, 65535, i32::MAX] {
            assert!(upper.widen(Interval::point(upper.hi + 1)));
            assert_eq!(upper.hi, threshold);
        }
        let mut lower = Interval::point(1);
        for threshold in [0, -128, -32768, i32::MIN] {
            assert!(lower.widen(Interval::point(lower.lo - 1)));
            assert_eq!(lower.lo, threshold);
        }
        assert!(!upper.widen(Interval::BOOL));
        assert!(!lower.widen(Interval::BOOL));
        let mut empty = Interval::EMPTY;
        assert!(empty.widen(Interval { lo: -7, hi: 19 }));
        assert_eq!(empty, Interval { lo: -7, hi: 19 }, "the first fact is exact");
        assert!(!empty.widen(Interval::EMPTY));
    }
}
