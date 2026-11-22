//! Fill-reducing ordering, once per topology (E3 1.4 step 2).
//!
//! 1. A maximum transversal matches every column to a row with a structural nonzero, so the
//!    permuted matrix has a zero-free diagonal (MNA's voltage-source rows have none of their own).
//! 2. Tarjan's strongly connected components on the matched graph give a block upper triangular
//!    form; the blocks come out dependencies first, so no pivot search ever leaves its block.
//! 3. Inside each block a minimum-degree ordering on the symmetrised pattern, with exact degrees
//!    on the elimination graph and ties broken by index, so the ordering is deterministic.
//!
//! The factor prefers the matched row of each column as its pivot (threshold pivoting keeps it
//! when it is large enough), so the ordering's fill estimate holds in the common case.

use super::pattern::Pattern;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

pub const NONE: u32 = u32::MAX;

#[derive(Clone, Debug)]
pub struct Ordering {
    /// Step `k` eliminates column `q[k]`.
    pub q: Vec<u32>,
    /// The preferred pivot row of step `k` (the row matched to `q[k]`).
    pub pref: Vec<u32>,
    /// Start step of each diagonal block, plus `n` at the end.
    pub blocks: Vec<u32>,
    /// Columns no row could be matched to: the matrix is structurally singular there.
    pub unmatched: Vec<u32>,
}

pub fn order(p: &Pattern) -> Ordering {
    let n = p.n;
    let (col_match, unmatched) = max_transversal(p);
    let mut row_match = vec![NONE; n];
    for c in 0..n {
        if col_match[c] != NONE {
            row_match[col_match[c] as usize] = c as u32;
        }
    }
    // pair unmatched columns with unmatched rows so the result is a permutation; the factor then
    // reports the singular column by name
    let free: Vec<u32> = (0..n as u32).filter(|&r| row_match[r as usize] == NONE).collect();
    let mut free_rows = free.into_iter();
    let mut col_match = col_match;
    for &c in &unmatched {
        let r = free_rows.next().unwrap();
        col_match[c as usize] = r;
        row_match[r as usize] = c;
    }
    let blocks = tarjan_blocks(p, &row_match);
    let mut q = Vec::with_capacity(n);
    let mut starts = Vec::with_capacity(blocks.len() + 1);
    for block in &blocks {
        starts.push(q.len() as u32);
        if block.len() <= 2 {
            q.extend_from_slice(block);
        } else {
            min_degree_block(p, &row_match, block, &mut q);
        }
    }
    starts.push(n as u32);
    let pref = q.iter().map(|&c| col_match[c as usize]).collect();
    Ordering { q, pref, blocks: starts, unmatched }
}

/// A maximum matching of columns to rows (depth-first augmenting paths after a cheap greedy
/// pass). Returns `col_match[c]` (or NONE) and the unmatched columns.
pub fn max_transversal(p: &Pattern) -> (Vec<u32>, Vec<u32>) {
    let n = p.n;
    let mut row_match = vec![NONE; n];
    let mut col_match = vec![NONE; n];
    for c in 0..n {
        for i in p.col_ptr[c]..p.col_ptr[c + 1] {
            let r = p.row[i as usize] as usize;
            if row_match[r] == NONE {
                row_match[r] = c as u32;
                col_match[c] = r as u32;
                break;
            }
        }
    }
    let mut visited = vec![NONE; n];
    let mut stack: Vec<(u32, u32)> = Vec::new();
    let mut unmatched = Vec::new();
    for c0 in 0..n {
        if col_match[c0] != NONE {
            continue;
        }
        stack.clear();
        stack.push((c0 as u32, p.col_ptr[c0]));
        let mut found = false;
        while let Some(top) = stack.last_mut() {
            let c = top.0 as usize;
            if top.1 < p.col_ptr[c + 1] {
                let r = p.row[top.1 as usize] as usize;
                top.1 += 1;
                if visited[r] == c0 as u32 {
                    continue;
                }
                visited[r] = c0 as u32;
                if row_match[r] == NONE {
                    found = true;
                    break;
                }
                let next = row_match[r];
                stack.push((next, p.col_ptr[next as usize]));
            } else {
                stack.pop();
            }
        }
        if found {
            for &(c, pos) in &stack {
                let r = p.row[pos as usize - 1];
                col_match[c as usize] = r;
                row_match[r as usize] = c;
            }
        } else {
            unmatched.push(c0 as u32);
        }
    }
    (col_match, unmatched)
}

/// Strongly connected components of the column graph (column `c` depends on column `c2` when a
/// row of `c` is matched to `c2`), emitted dependencies first.
fn tarjan_blocks(p: &Pattern, row_match: &[u32]) -> Vec<Vec<u32>> {
    let n = p.n;
    let mut index = vec![NONE; n];
    let mut low = vec![0u32; n];
    let mut on_stack = vec![false; n];
    let mut scc_stack: Vec<u32> = Vec::new();
    let mut call: Vec<(u32, u32)> = Vec::new();
    let mut next_index = 0u32;
    let mut out = Vec::new();
    for root in 0..n {
        if index[root] != NONE {
            continue;
        }
        call.push((root as u32, p.col_ptr[root]));
        index[root] = next_index;
        low[root] = next_index;
        next_index += 1;
        scc_stack.push(root as u32);
        on_stack[root] = true;
        while let Some(top) = call.last_mut() {
            let v = top.0 as usize;
            if top.1 < p.col_ptr[v + 1] {
                let r = p.row[top.1 as usize] as usize;
                top.1 += 1;
                let w = row_match[r] as usize;
                if w == v {
                    continue;
                }
                if index[w] == NONE {
                    index[w] = next_index;
                    low[w] = next_index;
                    next_index += 1;
                    scc_stack.push(w as u32);
                    on_stack[w] = true;
                    call.push((w as u32, p.col_ptr[w]));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
            } else {
                call.pop();
                if let Some(parent) = call.last() {
                    let u = parent.0 as usize;
                    low[u] = low[u].min(low[v]);
                }
                if low[v] == index[v] {
                    let mut comp = Vec::new();
                    loop {
                        let w = scc_stack.pop().unwrap();
                        on_stack[w as usize] = false;
                        comp.push(w);
                        if w as usize == v {
                            break;
                        }
                    }
                    comp.sort_unstable();
                    out.push(comp);
                }
            }
        }
    }
    out
}

/// Minimum degree on the symmetrised pattern of one block, appended to `q`.
fn min_degree_block(p: &Pattern, row_match: &[u32], block: &[u32], q: &mut Vec<u32>) {
    let m = block.len();
    // local index of each column of the block
    let mut local = std::collections::HashMap::with_capacity(m);
    for (i, &c) in block.iter().enumerate() {
        local.insert(c, i as u32);
    }
    // B(i, j) = A(match[i], j): column j's row r is matched to column row_match[r]
    let mut adj: Vec<Vec<u32>> = vec![Vec::new(); m];
    for (j, &c) in block.iter().enumerate() {
        for k in p.col_ptr[c as usize]..p.col_ptr[c as usize + 1] {
            let r = p.row[k as usize] as usize;
            if let Some(&i) = local.get(&row_match[r]) {
                if i as usize != j {
                    adj[j].push(i);
                    adj[i as usize].push(j as u32);
                }
            }
        }
    }
    for a in adj.iter_mut() {
        a.sort_unstable();
        a.dedup();
    }
    let mut alive = vec![true; m];
    let mut heap: BinaryHeap<Reverse<(u32, u32)>> = (0..m).map(|v| Reverse((adj[v].len() as u32, v as u32))).collect();
    let mut mark = vec![NONE; m];
    let mut stamp = 0u32;
    let mut scratch: Vec<u32> = Vec::new();
    while let Some(Reverse((d, v))) = heap.pop() {
        let v = v as usize;
        if !alive[v] || adj[v].len() as u32 != d {
            continue;
        }
        alive[v] = false;
        q.push(block[v]);
        let nbrs = std::mem::take(&mut adj[v]);
        for &u in &nbrs {
            let u = u as usize;
            // adj[u] = (adj[u] - {v}) + (nbrs - {u})
            scratch.clear();
            stamp += 1;
            for &w in &adj[u] {
                if w as usize != v {
                    mark[w as usize] = stamp;
                    scratch.push(w);
                }
            }
            for &w in &nbrs {
                if w as usize != u && mark[w as usize] != stamp {
                    mark[w as usize] = stamp;
                    scratch.push(w);
                }
            }
            scratch.sort_unstable();
            std::mem::swap(&mut adj[u], &mut scratch);
            heap.push(Reverse((adj[u].len() as u32, u as u32)));
        }
    }
}
