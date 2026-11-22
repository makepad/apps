//! The image's step map: where its statements, handler entries and handler returns are, as an
//! emitter states them and a co-simulated device consumes them.

use crate::pcset::PcSet;

/// The source body owning a statement or condition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner { Handler(u32), Func(u32) }

/// The image's addresses that stepping and the observations need, in the machine's PC unit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StepMap {
    /// Statement-start addresses, ascending and unique.
    pub statements: Vec<u32>,
    /// (address, handler) of each handler's entry, ascending by address.
    pub entries: Vec<(u32, u32)>,
    /// Addresses after calls into handlers, ascending and unique. The running handler returns
    /// here; handlers run to completion and never nest.
    pub returns: Vec<u32>,
    /// (start, end, function), ascending by start, in machine PC units.
    /// No branch may target a function start; use a separate entry instruction.
    pub funcs: Vec<(u32, u32, u32)>,
    /// (return address, callee), ascending by return address. These are the
    /// chip call's return PCs; no branch may target them.
    pub calls: Vec<(u32, u32)>,
    /// Parallel to statements; empty while the emitter has no function support.
    pub owners: Vec<Owner>,
}

impl StepMap {
    /// Checks that each list's addresses are ascending and unique.
    pub fn check(&self) -> Result<(), String> {
        if !self.statements.windows(2).all(|w| w[0] < w[1]) {
            return Err("statement addresses are not ascending and unique".into());
        }
        if !self.entries.windows(2).all(|w| w[0].0 < w[1].0) {
            return Err("entries are not ascending and unique by address".into());
        }
        if !self.returns.windows(2).all(|w| w[0] < w[1]) {
            return Err("returns are not ascending and unique by address".into());
        }
        if !self.funcs.windows(2).all(|w| w[0].0 < w[1].0 && w[0].1 <= w[1].0)
            || self.funcs.iter().any(|f| f.0 >= f.1) {
            return Err("function ranges are not ascending and disjoint".into());
        }
        if !self.calls.windows(2).all(|w| w[0].0 < w[1].0) {
            return Err("call returns are not ascending and unique by address".into());
        }
        if !self.owners.is_empty() && self.owners.len() != self.statements.len() {
            return Err("statement owners are not parallel to statements".into());
        }
        Ok(())
    }

    /// Every address the map names, as the marks a run stops on.
    pub fn marks(&self) -> PcSet {
        let all = || self.statements.iter().copied().chain(self.entries.iter().map(|e| e.0)).chain(self.returns.iter().copied())
            .chain(self.funcs.iter().map(|f| f.0)).chain(self.calls.iter().map(|c| c.0));
        let (Some(lo), Some(hi)) = (all().min(), all().max()) else { return PcSet::default() };
        let mut marks = PcSet::new(lo, hi - lo + 1);
        all().for_each(|pc| marks.insert(pc));
        marks
    }
}
