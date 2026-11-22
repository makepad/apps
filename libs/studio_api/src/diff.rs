//! Diff events: every change to the cut, from any front end or the page,
//! as a unified line diff of its source, numbered so a viewer (the
//! Generate panel, a source view, an MCP client) reads what is new.

use makepad_strict_json::{self as json, Value};

/// A unified diff of `before` → `after` with `context` lines around each
/// change; empty when they are equal. Myers' O(ND) line diff.
pub fn unified(before: &str, after: &str, context: usize) -> String {
    if before == after {
        return String::new();
    }
    let a: Vec<&str> = before.lines().collect();
    let b: Vec<&str> = after.lines().collect();
    let ops = myers(&a, &b);
    // Hunks: runs of changes with their context, merged when close.
    let mut out = String::new();
    let mut index = 0;
    while index < ops.len() {
        if matches!(ops[index], Op::Same(..)) {
            index += 1;
            continue;
        }
        let start = index.saturating_sub(context);
        let mut end = index;
        loop {
            while end < ops.len() && !matches!(ops[end], Op::Same(..)) {
                end += 1;
            }
            let same = ops[end..].iter().take_while(|op| matches!(op, Op::Same(..))).count();
            if end + same < ops.len() && same <= context * 2 {
                end += same;
            } else {
                end = (end + same.min(context)).min(ops.len());
                break;
            }
        }
        let (mut a_start, mut b_start) = (usize::MAX, usize::MAX);
        let (mut a_len, mut b_len) = (0, 0);
        let mut body = String::new();
        for op in &ops[start..end] {
            match *op {
                Op::Same(i, j) => {
                    a_start = a_start.min(i);
                    b_start = b_start.min(j);
                    a_len += 1;
                    b_len += 1;
                    body.push_str(&format!(" {}\n", a[i]));
                }
                Op::Del(i, j) => {
                    a_start = a_start.min(i);
                    b_start = b_start.min(j);
                    a_len += 1;
                    body.push_str(&format!("-{}\n", a[i]));
                }
                Op::Ins(i, j) => {
                    a_start = a_start.min(i);
                    b_start = b_start.min(j);
                    b_len += 1;
                    body.push_str(&format!("+{}\n", b[j]));
                }
            }
        }
        let line = |start: usize, len: usize| if len == 0 { format!("{},0", start) } else { format!("{},{len}", start + 1) };
        out.push_str(&format!("@@ -{} +{} @@\n{body}", line(a_start, a_len), line(b_start, b_len)));
        index = end;
    }
    out
}

/// One line operation, with the positions in `a` and `b` it sits at.
#[derive(Clone, Copy, Debug)]
enum Op {
    Same(usize, usize),
    Del(usize, usize),
    Ins(usize, usize),
}

fn myers(a: &[&str], b: &[&str]) -> Vec<Op> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let max = (n + m) as usize;
    let offset = max as isize + 1;
    let mut v = vec![0isize; 2 * max + 3];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    'outer: for d in 0..=max as isize {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let index = (k + offset) as usize;
            let mut x = if k == -d || (k != d && v[index - 1] < v[index + 1]) { v[index + 1] } else { v[index - 1] + 1 };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[index] = x;
            if x >= n && y >= m {
                break 'outer;
            }
            k += 2;
        }
    }
    // Walk the trace back from (n, m).
    let (mut x, mut y) = (n, m);
    let mut ops = Vec::new();
    for (d, v) in trace.iter().enumerate().rev() {
        let d = d as isize;
        let k = x - y;
        let index = (k + offset) as usize;
        let prev_k = if k == -d || (k != d && v[index - 1] < v[index + 1]) { k + 1 } else { k - 1 };
        let prev_x = v[(prev_k + offset) as usize];
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            x -= 1;
            y -= 1;
            ops.push(Op::Same(x as usize, y as usize));
        }
        if d > 0 {
            if x == prev_x {
                y -= 1;
                ops.push(Op::Ins(x as usize, y as usize));
            } else {
                x -= 1;
                ops.push(Op::Del(x as usize, y as usize));
            }
        }
    }
    ops.reverse();
    ops
}

/// A change to the open cut.
#[derive(Clone, Debug, PartialEq)]
pub struct ChangeEvent {
    pub seq: u64,
    /// The cut's revision after the change.
    pub revision: u64,
    /// The tool that made it, or `page` for the person's own edits.
    pub tool: String,
    /// Who called: `mcp`, `cli`, `generate`, `page`.
    pub origin: String,
    /// One line: what happened.
    pub label: String,
    pub diff: String,
}

impl ChangeEvent {
    pub fn to_json(&self) -> Value {
        json::obj(vec![
            ("seq", Value::Int(self.seq as i64)),
            ("revision", Value::Int(self.revision as i64)),
            ("tool", json::s(self.tool.clone())),
            ("origin", json::s(self.origin.clone())),
            ("label", json::s(self.label.clone())),
            ("diff", json::s(self.diff.clone())),
        ])
    }
}

/// The session's changes, numbered from 1. Readers keep the last number
/// they saw; everything is kept for the session (a change is one diff of a
/// text file, and a cut session makes hundreds, not millions).
#[derive(Default)]
pub struct ChangeLog {
    events: Vec<ChangeEvent>,
}

impl ChangeLog {
    pub fn push(&mut self, revision: u64, tool: &str, origin: &str, label: String, diff: String) -> u64 {
        let seq = self.events.last().map_or(1, |event| event.seq + 1);
        self.events.push(ChangeEvent { seq, revision, tool: tool.to_string(), origin: origin.to_string(), label, diff });
        seq
    }

    pub fn since(&self, seq: u64) -> &[ChangeEvent] {
        let start = self.events.partition_point(|event| event.seq <= seq);
        &self.events[start..]
    }

    pub fn last(&self) -> u64 {
        self.events.last().map_or(0, |event| event.seq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_changed_line_is_one_hunk_with_context() {
        let before = "a\nb\nc\nd\ne\nf\ng\n";
        let after = "a\nb\nc\nD\ne\nf\ng\n";
        assert_eq!(unified(before, after, 2), "@@ -2,5 +2,5 @@\n b\n c\n-d\n+D\n e\n f\n");
        assert_eq!(unified(before, before, 2), "");
    }

    #[test]
    fn inserts_deletes_and_far_apart_hunks() {
        let before = (1..=20).map(|n| n.to_string()).collect::<Vec<_>>().join("\n");
        let mut lines: Vec<String> = (1..=20).map(|n| n.to_string()).collect();
        lines.insert(2, "new".into());
        lines.remove(16);
        let after = lines.join("\n");
        let diff = unified(&before, &after, 1);
        assert_eq!(diff.matches("@@ -").count(), 2, "{diff}");
        assert!(diff.contains("+new") && diff.contains("-16"), "{diff}");
        // Every kept line of `after` is in the diff's `+`/` ` lines of its hunks.
        let empty_to_text = unified("", "x\ny", 0);
        assert_eq!(empty_to_text, "@@ -0,0 +1,2 @@\n+x\n+y\n");
    }

    #[test]
    fn the_log_reads_since_a_number() {
        let mut log = ChangeLog::default();
        assert_eq!(log.push(3, "trim_block", "mcp", "trim".into(), "d".into()), 1);
        assert_eq!(log.push(4, "page", "page", "move".into(), "d".into()), 2);
        assert_eq!(log.since(0).len(), 2);
        assert_eq!(log.since(1)[0].tool, "page");
        assert!(log.since(2).is_empty());
        assert_eq!(log.last(), 2);
    }
}
