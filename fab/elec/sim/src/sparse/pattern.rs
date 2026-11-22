//! A square matrix's nonzero pattern in compressed sparse columns.
//!
//! Rows and columns are 0-based here (unknown `u` of the solver is index `u - 1`). Values live in
//! a separate array whose index 0 is a trash cell, so the value of CSC position `p` is at `p + 1`:
//! a stamp aimed at ground writes to the trash and needs no branch.

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Pattern {
    pub n: usize,
    /// `col_ptr[c] .. col_ptr[c + 1]` are column `c`'s positions; rows ascending within a column.
    pub col_ptr: Vec<u32>,
    pub row: Vec<u32>,
}

impl Pattern {
    /// Builds the pattern of `entries` (row, col), duplicates allowed. Returns the pattern and, for
    /// each entry, its value index (CSC position + 1).
    pub fn from_entries(n: usize, entries: &[(u32, u32)]) -> (Pattern, Vec<u32>) {
        let mut count = vec![0u32; n + 1];
        for &(_, c) in entries {
            count[c as usize + 1] += 1;
        }
        for c in 0..n {
            count[c + 1] += count[c];
        }
        // bucket by column, then sort and dedup each column
        let mut fill = count.clone();
        let mut by_col = vec![(0u32, 0u32); entries.len()]; // (row, entry index)
        for (e, &(r, c)) in entries.iter().enumerate() {
            let p = &mut fill[c as usize];
            by_col[*p as usize] = (r, e as u32);
            *p += 1;
        }
        let mut col_ptr = Vec::with_capacity(n + 1);
        let mut row = Vec::with_capacity(entries.len());
        let mut map = vec![0u32; entries.len()];
        col_ptr.push(0u32);
        for c in 0..n {
            let seg = &mut by_col[count[c] as usize..count[c + 1] as usize];
            seg.sort_unstable();
            let mut last = u32::MAX;
            for &(r, e) in seg.iter() {
                if r != last {
                    row.push(r);
                    last = r;
                }
                map[e as usize] = row.len() as u32; // position + 1
            }
            col_ptr.push(row.len() as u32);
        }
        (Pattern { n, col_ptr, row }, map)
    }

    pub fn nnz(&self) -> usize {
        self.row.len()
    }

    /// The value index (position + 1) of (r, c), if present.
    pub fn find(&self, r: u32, c: u32) -> Option<u32> {
        let s = self.col_ptr[c as usize] as usize;
        let e = self.col_ptr[c as usize + 1] as usize;
        self.row[s..e].binary_search(&r).ok().map(|i| (s + i + 1) as u32)
    }

    /// Row and column of every value index (index 0, the trash, maps to (u32::MAX, u32::MAX)).
    pub fn coords(&self) -> (Vec<u32>, Vec<u32>) {
        let mut rows = vec![u32::MAX; self.nnz() + 1];
        let mut cols = vec![u32::MAX; self.nnz() + 1];
        for c in 0..self.n {
            for p in self.col_ptr[c] as usize..self.col_ptr[c + 1] as usize {
                rows[p + 1] = self.row[p];
                cols[p + 1] = c as u32;
            }
        }
        (rows, cols)
    }
}
