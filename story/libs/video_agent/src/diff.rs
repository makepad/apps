//! Line diffs of Splash documents, so every change the model makes (and
//! every change the person makes by hand) is shown as what it is, and the
//! model is told about hand edits exactly.

/// One line of a diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    Same(String),
    Added(String),
    Removed(String),
    /// Unchanged lines left out between hunks: how many.
    Skipped(usize),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diff {
    pub lines: Vec<Line>,
    pub added: usize,
    pub removed: usize,
}

impl Diff {
    pub fn is_empty(&self) -> bool {
        self.added == 0 && self.removed == 0
    }

    /// Unified-diff text: `+`/`-`/` ` lines, `@@ … @@` between hunks.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for line in &self.lines {
            match line {
                Line::Same(text) => out.push_str(&format!("  {text}\n")),
                Line::Added(text) => out.push_str(&format!("+ {text}\n")),
                Line::Removed(text) => out.push_str(&format!("- {text}\n")),
                Line::Skipped(count) => out.push_str(&format!("@@ {count} unchanged lines @@\n")),
            }
        }
        out
    }
}

/// Lines compared exactly; `context` unchanged lines kept round each
/// change. A new document (empty `before`) is all additions.
pub fn diff(before: &str, after: &str, context: usize) -> Diff {
    let a: Vec<&str> = before.lines().collect();
    let b: Vec<&str> = after.lines().collect();
    // The common head and tail cost nothing; only the middle is aligned.
    let head = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let tail = a[head..].iter().rev().zip(b[head..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (am, bm) = (&a[head..a.len() - tail], &b[head..b.len() - tail]);
    let mut ops: Vec<Line> = a[..head].iter().map(|l| Line::Same(l.to_string())).collect();
    ops.extend(align(am, bm));
    ops.extend(a[a.len() - tail..].iter().map(|l| Line::Same(l.to_string())));

    let added = ops.iter().filter(|l| matches!(l, Line::Added(_))).count();
    let removed = ops.iter().filter(|l| matches!(l, Line::Removed(_))).count();
    // Keep `context` lines round each change; fold the rest.
    let changed: Vec<usize> = ops.iter().enumerate().filter(|(_, l)| !matches!(l, Line::Same(_))).map(|(i, _)| i).collect();
    let near = |i: usize| changed.iter().any(|&c| c.abs_diff(i) <= context);
    let mut lines = Vec::new();
    let mut skipped = 0;
    for (i, line) in ops.into_iter().enumerate() {
        if matches!(line, Line::Same(_)) && !near(i) {
            skipped += 1;
            continue;
        }
        if skipped > 0 {
            lines.push(Line::Skipped(skipped));
            skipped = 0;
        }
        lines.push(line);
    }
    if skipped > 0 && !lines.is_empty() {
        lines.push(Line::Skipped(skipped));
    }
    Diff { lines, added, removed }
}

/// The middle of two documents aligned by their longest common
/// subsequence. A middle too large to align line by line (a whole new
/// document against another) is shown as replaced.
fn align(a: &[&str], b: &[&str]) -> Vec<Line> {
    if a.len().saturating_mul(b.len()) > 4_000_000 {
        let mut out: Vec<Line> = a.iter().map(|l| Line::Removed(l.to_string())).collect();
        out.extend(b.iter().map(|l| Line::Added(l.to_string())));
        return out;
    }
    let (n, m) = (a.len(), b.len());
    // lcs[i][j]: the common subsequence length of a[i..] and b[j..].
    let mut lcs = vec![0u32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[at(i, j)] = if a[i] == b[j] { lcs[at(i + 1, j + 1)] + 1 } else { lcs[at(i + 1, j)].max(lcs[at(i, j + 1)]) };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            out.push(Line::Same(a[i].to_string()));
            i += 1;
            j += 1;
        } else if i < n && (j == m || lcs[at(i + 1, j)] >= lcs[at(i, j + 1)]) {
            out.push(Line::Removed(a[i].to_string()));
            i += 1;
        } else {
            out.push(Line::Added(b[j].to_string()));
            j += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_changed_line_with_context_and_folds() {
        let before = (1..=20).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let after = before.replace("line 10", "line ten").replace("line 18\n", "");
        let d = diff(&before, &after, 1);
        assert_eq!((d.added, d.removed), (1, 2));
        let text = d.text();
        assert!(text.starts_with("@@ 8 unchanged lines @@\n  line 9\n- line 10\n+ line ten\n  line 11\n"), "{text}");
        assert!(text.contains("- line 18\n  line 19\n@@ 1 unchanged lines @@"), "{text}");
    }

    #[test]
    fn new_and_same_documents() {
        let d = diff("", "a\nb", 2);
        assert_eq!((d.added, d.removed), (2, 0));
        assert!(diff("a\nb", "a\nb", 2).is_empty());
        assert!(diff("a\nb", "a\nb", 2).lines.is_empty());
    }
}
