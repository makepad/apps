//! Loop-scoped literal intervals shared by 32-bit targets. A literal is
//! defined in the innermost containing loop's preheader and lives through
//! its backedge. Repeated uses in that scope share one interval. It takes a
//! register only where `regalloc::fill` finds one free across the loop, and
//! otherwise rematerializes at each use, never in a frame slot.

use std::collections::BTreeMap;

use fab_elec_mcu_rt::format::OP_LOOP;

use crate::{control::Step, regalloc::Interval};

/// Identity is independent of the eventual link address. Frame sizing may
/// precede buffer placement, when distinct buffers still have equal addresses.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Literal {
    BufferBase { buffer: u32, element: u32 },
    Clamp(u32),
}

pub struct LoopLiterals {
    pub intervals: Vec<Interval>,
    /// Interval IDs used by each step. Outside loops this is empty: literals
    /// there are already materialized once at their use.
    pub uses: Vec<Vec<usize>>,
}

impl LoopLiterals {
    pub fn new(steps: &[Step<'_>], literals: &[Vec<(Literal, u32)>], calls: &[u32]) -> Self {
        let mut out = Self { intervals: Vec::new(), uses: vec![Vec::new(); steps.len()] };
        let mut loops = Vec::new();
        let mut ids = BTreeMap::new();
        for (n, &step) in steps.iter().enumerate() {
            match step {
                Step::Op(i) if i.op == OP_LOOP => loops.push(i),
                Step::EndLoop => { loops.pop(); }
                _ => {}
            }
            let Some(scope) = loops.last() else { continue; };
            for &(key, value) in &literals[n] {
                let id = *ids.entry((scope.at, key)).or_insert_with(|| {
                    let id = out.intervals.len();
                    let call = calls.partition_point(|&at| at < scope.at);
                    out.intervals.push(Interval { start: scope.at, end: scope.end - 1, weight: 0,
                        crosses_call: calls.get(call).is_some_and(|&at| at < scope.end),
                        rematerialize: Some(value) });
                    id
                });
                out.intervals[id].weight = out.intervals[id].weight.saturating_add(8u64.saturating_pow(loops.len() as u32));
                out.uses[n].push(id);
            }
        }
        out
    }
}
