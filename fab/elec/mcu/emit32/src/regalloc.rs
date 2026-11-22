//! Target-neutral, weighted linear scan. Intervals have inclusive endpoints:
//! sources and destinations at the same instruction cannot alias. Eviction
//! spills the entire interval; there are no moves or interval splits to emit.

#[derive(Clone, Copy, Debug)]
pub struct Interval {
    pub start: u32,
    pub end: u32,
    pub weight: u64,
    pub crosses_call: bool,
    /// Literal bits to re-emit at each use if this interval loses its register.
    /// Constants, buffer addresses and bounds need no frame storage.
    pub rematerialize: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Location {
    Register(u8),
    Spill(usize),
    Rematerialize(u32),
}

#[derive(Clone, Debug)]
pub struct Allocation {
    /// One location per input interval, in the original input order.
    pub locations: Vec<Location>,
    pub spill_slots: usize,
}

/// Registers are opaque target-supplied numbers, in preference order. Prefer
/// volatile registers for intervals without calls, leaving preserved registers
/// available for intervals that need them. Lower weight / inclusive length
/// spills first; on ties the higher interval index spills. Eviction applies to
/// the whole interval, including earlier uses. Only nonliteral spills get slots,
/// and slots reuse nonoverlapping intervals.
pub fn allocate(intervals: &[Interval], volatile: &[u8], preserved: &[u8]) -> Allocation {
    let mut order: Vec<_> = (0..intervals.len()).collect();
    order.sort_unstable_by_key(|&i| (intervals[i].start, i));
    let fallback = |i: usize| intervals[i].rematerialize
        .map_or(Location::Spill(0), Location::Rematerialize);
    let mut locations: Vec<_> = (0..intervals.len()).map(fallback).collect();
    let mut active = Vec::<usize>::new();
    for &i in &order {
        let interval = intervals[i];
        assert!(interval.start <= interval.end, "live interval must not be reversed");
        active.retain(|&j| intervals[j].end >= interval.start);
        let eligible = |reg| !interval.crosses_call || preserved.contains(&reg);
        let free = volatile.iter().chain(preserved).copied().find(|&reg| {
            eligible(reg) && !active.iter().any(|&j| locations[j] == Location::Register(reg))
        });
        if let Some(reg) = free {
            locations[i] = Location::Register(reg);
            active.push(i);
            continue;
        }
        // Cross-products compare the exact ratios without rounding. u128
        // holds a u64 weight times even the full inclusive u32 address range.
        let priority = |a: usize, b: usize| {
            let length = |j: usize| u128::from(intervals[j].end) - u128::from(intervals[j].start) + 1;
            (u128::from(intervals[a].weight) * length(b))
                .cmp(&(u128::from(intervals[b].weight) * length(a)))
                .then_with(|| b.cmp(&a))
        };
        let victim = active.iter().copied().filter(|&j| match locations[j] {
            Location::Register(reg) => eligible(reg),
            Location::Spill(_) | Location::Rematerialize(_) => unreachable!("active intervals have registers"),
        }).min_by(|&a, &b| priority(a, b));
        if let Some(j) = victim.filter(|&j| priority(j, i).is_lt()) {
            locations[i] = locations[j];
            locations[j] = fallback(j);
            active.retain(|&k| k != j);
            active.push(i);
        }
    }
    let mut slots = Vec::<u32>::new();
    for i in order {
        if matches!(locations[i], Location::Spill(_)) {
            let slot = slots.iter().position(|&end| end < intervals[i].start)
                .unwrap_or_else(|| { slots.push(0); slots.len() - 1 });
            slots[slot] = intervals[i].end;
            locations[i] = Location::Spill(slot);
        }
    }
    Allocation { locations, spill_slots: slots.len() }
}

/// Places rematerializable intervals after `allocate`, only in registers that
/// no allocated or earlier-filled interval holds anywhere in their range, so
/// they can never push a value onto the stack. Losing one costs a MOVI or L32R
/// per use, what it cost before it had an interval; a spilled value costs a
/// load per use and a store per definition. Denser intervals choose first
/// (ties to the lower index), volatile registers before preserved ones, and
/// an interval crossing a call takes only a preserved register.
pub fn fill(allocated: &[Interval], locations: &[Location], fills: &[Interval], volatile: &[u8], preserved: &[u8]) -> Vec<Location> {
    let mut held: Vec<(Interval, u8)> = allocated.iter().zip(locations).filter_map(|(&interval, &location)| match location {
        Location::Register(reg) => Some((interval, reg)),
        Location::Spill(_) | Location::Rematerialize(_) => None,
    }).collect();
    let mut out: Vec<_> = fills.iter().map(|f| Location::Rematerialize(
        f.rematerialize.expect("a filled interval rematerializes when it gets no register"))).collect();
    let length = |f: &Interval| u128::from(f.end) - u128::from(f.start) + 1;
    let mut order: Vec<_> = (0..fills.len()).collect();
    order.sort_by(|&a, &b| (u128::from(fills[b].weight) * length(&fills[a]))
        .cmp(&(u128::from(fills[a].weight) * length(&fills[b]))).then(a.cmp(&b)));
    for i in order {
        let f = fills[i];
        assert!(f.start <= f.end, "live interval must not be reversed");
        let free = volatile.iter().chain(preserved).copied().find(|&reg| (!f.crosses_call || preserved.contains(&reg))
            && !held.iter().any(|&(h, r)| r == reg && h.start <= f.end && f.start <= h.end));
        if let Some(reg) = free {
            out[i] = Location::Register(reg);
            held.push((f, reg));
        }
    }
    out
}
