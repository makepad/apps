//! Line diffs of two revisions of a document, for the Source view's
//! "what changed" (every change, by hand or by the AI, is shown as the
//! lines it added and removed).
//!
//! The common prefix and suffix are trimmed first, so a one-value change in
//! a long document costs the length of the document once. The changed middle
//! is aligned by longest common subsequence; its table grows with the
//! product of the two middles' line counts, so a middle too large for that
//! (a whole document rewritten) is shown as removed-then-added instead,
//! which is exact, just coarser.

use makepad_code_editor::diff::{DiffRowKind, DiffRowSpec};

/// Largest LCS table (old lines × new lines) aligned line by line.
const ALIGN_CELLS: usize = 4_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Equal { old: usize, new: usize },
    Added { new: usize },
    Removed { old: usize },
}

fn lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = text.split('\n').collect();
    // A final newline ends the last line; it does not start another.
    if text.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// Rows in display order: equal, removed and added lines, zero-based.
pub fn rows(old: &str, new: &str) -> Vec<Row> {
    let (a, b) = (lines(old), lines(new));
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..].iter().rev().zip(b[prefix..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (a_mid, b_mid) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);
    let mut out: Vec<Row> = (0..prefix).map(|i| Row::Equal { old: i, new: i }).collect();
    if a_mid.len().saturating_mul(b_mid.len()) <= ALIGN_CELLS {
        // lcs[i][j]: the common subsequence of a_mid[i..] and b_mid[j..].
        let (n, m) = (a_mid.len(), b_mid.len());
        let mut lcs = vec![0u32; (n + 1) * (m + 1)];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                lcs[i * (m + 1) + j] = if a_mid[i] == b_mid[j] {
                    lcs[(i + 1) * (m + 1) + j + 1] + 1
                } else {
                    lcs[(i + 1) * (m + 1) + j].max(lcs[i * (m + 1) + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n || j < m {
            if i < n && j < m && a_mid[i] == b_mid[j] {
                out.push(Row::Equal { old: prefix + i, new: prefix + j });
                i += 1;
                j += 1;
            } else if j < m && (i == n || lcs[i * (m + 1) + j + 1] >= lcs[(i + 1) * (m + 1) + j]) {
                out.push(Row::Added { new: prefix + j });
                j += 1;
            } else {
                out.push(Row::Removed { old: prefix + i });
                i += 1;
            }
        }
        // Removed lines read better before the lines that replace them.
        let mut start = prefix;
        while start < out.len() {
            if matches!(out[start], Row::Equal { .. }) {
                start += 1;
                continue;
            }
            let end = out[start..].iter().position(|row| matches!(row, Row::Equal { .. })).map_or(out.len(), |len| start + len);
            out[start..end].sort_by_key(|row| matches!(row, Row::Added { .. }));
            start = end;
        }
    } else {
        out.extend((0..a_mid.len()).map(|i| Row::Removed { old: prefix + i }));
        out.extend((0..b_mid.len()).map(|j| Row::Added { new: prefix + j }));
    }
    out.extend((0..suffix).map(|k| Row::Equal { old: a.len() - suffix + k, new: b.len() - suffix + k }));
    out
}

/// The rows as the code editor's diff document wants them, with hunk ids.
pub fn specs(rows: &[Row]) -> Vec<DiffRowSpec> {
    let mut hunk = 0u32;
    let mut in_hunk = false;
    rows.iter().map(|row| {
        let changed = !matches!(row, Row::Equal { .. });
        if changed && !in_hunk {
            hunk += 1;
        }
        in_hunk = changed;
        let (kind, old_line, new_line) = match *row {
            Row::Equal { old, new } => (DiffRowKind::Equal, Some(old as u32), Some(new as u32)),
            Row::Added { new } => (DiffRowKind::Added, None, Some(new as u32)),
            Row::Removed { old } => (DiffRowKind::Removed, Some(old as u32), None),
        };
        DiffRowSpec { kind, old_line, new_line, hunk: if changed { hunk } else { 0 } }
    }).collect()
}

/// Lines added and removed.
pub fn counts(rows: &[Row]) -> (usize, usize) {
    rows.iter().fold((0, 0), |(added, removed), row| match row {
        Row::Added { .. } => (added + 1, removed),
        Row::Removed { .. } => (added, removed + 1),
        Row::Equal { .. } => (added, removed),
    })
}

/// The new-side lines that are new or changed (for marks in the editable
/// view).
pub fn added_lines(rows: &[Row]) -> Vec<usize> {
    rows.iter().filter_map(|row| match row {
        Row::Added { new } => Some(*new),
        _ => None,
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_changed_line_is_one_removed_and_one_added() {
        let rows = rows("a\nb\nc\n", "a\nB\nc\n");
        assert_eq!(rows, vec![Row::Equal { old: 0, new: 0 }, Row::Removed { old: 1 }, Row::Added { new: 1 }, Row::Equal { old: 2, new: 2 }]);
        assert_eq!(counts(&rows), (1, 1));
        let specs = specs(&rows);
        assert_eq!(specs[1].hunk, 1);
        assert_eq!(specs[2].hunk, 1);
        assert_eq!(specs[3].hunk, 0);
    }

    #[test]
    fn insertions_and_deletions_align() {
        let rows = rows("a\nb\nc\nd", "a\nx\nb\nd\ne");
        assert_eq!(counts(&rows), (2, 1));
        assert_eq!(added_lines(&rows), vec![1, 4]);
        // Every old and new line appears exactly once.
        let olds: Vec<usize> = rows.iter().filter_map(|row| match row { Row::Equal { old, .. } | Row::Removed { old } => Some(*old), _ => None }).collect();
        let news: Vec<usize> = rows.iter().filter_map(|row| match row { Row::Equal { new, .. } | Row::Added { new } => Some(*new), _ => None }).collect();
        assert_eq!(olds, vec![0, 1, 2, 3]);
        assert_eq!(news, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn identical_and_empty() {
        assert!(rows("a\nb", "a\nb").iter().all(|row| matches!(row, Row::Equal { .. })));
        assert_eq!(rows("", "x\n"), vec![Row::Added { new: 0 }]);
        assert_eq!(rows("x", ""), vec![Row::Removed { old: 0 }]);
    }
}
