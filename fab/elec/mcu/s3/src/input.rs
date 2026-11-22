//! The input schedule survives rewind; only its cursor rewinds.

#[derive(Clone)]
pub(crate) struct InputSchedule<I> {
    pub entries: Vec<(u64, I)>,
    pub cursor: usize,
}

impl<I: Clone> InputSchedule<I> {
    pub fn next(&self) -> Option<u64> { self.entries.get(self.cursor).map(|entry| entry.0) }

    pub fn pop(&mut self) -> (u64, I) {
        let entry = self.entries[self.cursor].clone();
        self.cursor += 1;
        entry
    }
}
