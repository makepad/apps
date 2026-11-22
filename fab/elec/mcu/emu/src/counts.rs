//! Chip-neutral code ranges and rewindable execution observations.

/// Dense ids start at one; id zero denotes code outside the map.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodeMap {
    pub blocks: Vec<Block>,
    pub conds: Vec<Cond>,
}

/// A half-open range in the machine's PC unit, including instruction operand words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Block {
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CondKind { Branch, Skip }

/// A conditional and its two successors, in the machine's PC unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cond {
    pub pc: u32,
    pub kind: CondKind,
    pub taken: u32,
    pub fall: u32,
}

/// Sparse counts, sorted by id within each drained batch. The last outcome belongs to
/// the latest execution in the span, even when that span crosses several marks.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExecCounts {
    pub blocks: Vec<(u32, u64)>,
    pub conds: Vec<(u32, u64, u64, bool)>,
}

#[derive(Clone, Copy, Default)]
struct Outcomes {
    counts: [u64; 2], // not taken, taken
    last: bool,
}

#[derive(Default)]
struct Layer {
    blocks: Vec<u64>,
    conds: Vec<Outcomes>,
    block_ids: Vec<u32>,
    cond_ids: Vec<u32>,
}

impl Clone for Layer {
    fn clone(&self) -> Self {
        // Vec::clone retains length, not spare capacity. A cloned machine must also
        // have room for every possible first touch without allocating in its run.
        let mut block_ids = Vec::with_capacity(self.blocks.len());
        block_ids.extend_from_slice(&self.block_ids);
        let mut cond_ids = Vec::with_capacity(self.conds.len());
        cond_ids.extend_from_slice(&self.cond_ids);
        Self { blocks: self.blocks.clone(), conds: self.conds.clone(), block_ids, cond_ids }
    }
}

impl Layer {
    fn new(blocks: usize, conds: usize) -> Self {
        Self {
            blocks: vec![0; blocks + 1], conds: vec![Outcomes::default(); conds + 1],
            block_ids: Vec::with_capacity(blocks + 1), cond_ids: Vec::with_capacity(conds + 1),
        }
    }

    fn clear(&mut self) {
        for id in self.block_ids.drain(..) { self.blocks[id as usize] = 0; }
        for id in self.cond_ids.drain(..) { self.conds[id as usize] = Outcomes::default(); }
    }
}

/// Counts live outside CPU snapshots. Marks fold only touched ids into the permanent
/// layer; rewinds discard only the touched speculative ids. Taking is a commit operation:
/// call immediately before marking, just as for the machine's statement trace.
#[derive(Clone, Default)]
pub struct Tally {
    since: Layer,
    before: Layer,
}

impl Tally {
    /// Installs a rebuilt map's dimensions and discards counts belonging to the old ids.
    /// Both touched lists reserve their full capacity here, so counting never allocates.
    pub fn resize(&mut self, map: &CodeMap) {
        self.since = Layer::new(map.blocks.len(), map.conds.len());
        self.before = Layer::new(map.blocks.len(), map.conds.len());
    }

    #[inline]
    pub fn block(&mut self, id: u32) {
        let count = &mut self.since.blocks[id as usize];
        if *count == 0 { self.since.block_ids.push(id); }
        *count += 1;
    }

    #[inline]
    pub fn cond(&mut self, id: u32, taken: bool) {
        let count = &mut self.since.conds[id as usize];
        if count.counts == [0, 0] { self.since.cond_ids.push(id); }
        count.counts[taken as usize] += 1;
        count.last = taken;
    }

    pub fn mark(&mut self) {
        for id in self.since.block_ids.drain(..) {
            let count = &mut self.before.blocks[id as usize];
            if *count == 0 { self.before.block_ids.push(id); }
            *count += std::mem::take(&mut self.since.blocks[id as usize]);
        }
        for id in self.since.cond_ids.drain(..) {
            let count = &mut self.before.conds[id as usize];
            if count.counts == [0, 0] { self.before.cond_ids.push(id); }
            let new = std::mem::take(&mut self.since.conds[id as usize]);
            count.counts[0] += new.counts[0];
            count.counts[1] += new.counts[1];
            count.last = new.last;
        }
    }

    pub fn rewind(&mut self) { self.since.clear(); }

    /// Appends one sorted batch and drains both layers, retaining all their capacity.
    pub fn take(&mut self, out: &mut ExecCounts) {
        self.mark();
        self.before.block_ids.sort_unstable();
        self.before.cond_ids.sort_unstable();
        for id in self.before.block_ids.drain(..) {
            out.blocks.push((id, std::mem::take(&mut self.before.blocks[id as usize])));
        }
        for id in self.before.cond_ids.drain(..) {
            let c = std::mem::take(&mut self.before.conds[id as usize]);
            out.conds.push((id, c.counts[1], c.counts[0], c.last));
        }
    }
}
