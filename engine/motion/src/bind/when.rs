//! When an overlay shows, found in the run: "when the save button is
//! clicked", "while typing into the body", "from mark @saved for 2 s". The
//! AI's overlay tools (reports/M4-demo-overlays.md) turn a request into one
//! of these, and the span it resolves to becomes the Graphic's `span`.
//! Resolving again after the app run is updated (§6.5) moves the span with
//! the new timing.

use super::run::RunData;

/// A moment or stretch of the run.
#[derive(Clone, Debug, PartialEq)]
pub enum When {
    /// Every `click` (or `type`, `drag`, …) call, optionally only those on
    /// a widget path (suffix match); `nth` picks one (0-based), `None` all.
    Call { call: String, target: Option<String>, nth: Option<usize> },
    /// From a mark for `hold` seconds.
    Mark { name: String, hold: f64 },
    /// From one mark to another.
    Between { from: String, to: String },
    /// The whole run.
    Always,
}

/// A stretch in seconds of the recording, `[from, to)`.
pub type RunSpan = (f64, f64);

fn path_matches(target: &str, path: &str) -> bool {
    let target = target.trim_start_matches('@');
    path == target || (path.len() > target.len() && path.ends_with(target) && path.as_bytes()[path.len() - target.len() - 1] == b'.')
}

impl When {
    /// The stretches it names, each widened by `lead` before and `tail`
    /// after (so a highlight is up before the click lands and stays a
    /// moment after), clamped to the run and merged where they overlap.
    /// An error says what the run has instead.
    pub fn resolve(&self, run: &RunData, lead: f64, tail: f64) -> Result<Vec<RunSpan>, String> {
        let spans: Vec<RunSpan> = match self {
            When::Always => vec![(0.0, run.duration())],
            When::Mark { name, hold } => {
                let at = run.mark(name).ok_or_else(|| missing_mark(run, name))?;
                vec![(at, at + hold)]
            }
            When::Between { from, to } => {
                let a = run.mark(from).ok_or_else(|| missing_mark(run, from))?;
                let b = run.mark(to).ok_or_else(|| missing_mark(run, to))?;
                if b <= a {
                    return Err(format!("mark @{to} ({b:.2} s) is not after @{from} ({a:.2} s)"));
                }
                vec![(a, b)]
            }
            When::Call { call, target, nth } => {
                let all: Vec<RunSpan> = run
                    .calls
                    .iter()
                    .filter(|c| c.call == *call && target.as_deref().is_none_or(|t| c.target.as_deref().is_some_and(|p| path_matches(t, p))))
                    .map(|c| (c.from as f64 / run.fps, c.to as f64 / run.fps))
                    .collect();
                if all.is_empty() {
                    let seen: Vec<String> = run
                        .calls
                        .iter()
                        .filter(|c| c.target.is_some())
                        .map(|c| format!("{} @{}", c.call, c.target.as_deref().unwrap_or("")))
                        .collect();
                    return Err(format!(
                        "the run has no {call}{} call; its calls with targets: {}",
                        target.as_deref().map(|t| format!(" on @{}", t.trim_start_matches('@'))).unwrap_or_default(),
                        if seen.is_empty() { "none".to_string() } else { seen.join(", ") }
                    ));
                }
                match nth {
                    Some(n) => vec![*all.get(*n).ok_or_else(|| format!("the run has {} matching {call} calls; nth {n} is out of range (0-based)", all.len()))?],
                    None => all,
                }
            }
        };
        let end = run.duration();
        let mut out: Vec<RunSpan> = Vec::new();
        for (a, b) in spans {
            let (a, b) = ((a - lead).max(0.0), (b + tail).min(end));
            if b <= a {
                continue;
            }
            match out.last_mut() {
                Some(last) if a <= last.1 => last.1 = last.1.max(b),
                _ => out.push((a, b)),
            }
        }
        Ok(out)
    }
}

fn missing_mark(run: &RunData, name: &str) -> String {
    let names: Vec<String> = run.marks.iter().map(|m| format!("@{}", m.0)).collect();
    format!("the run has no mark @{name}; its marks: {}", if names.is_empty() { "none".into() } else { names.join(", ") })
}

/// A clip's time mapping: edit time = `start + (run time - source_in) / speed`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClipTiming {
    /// The clip's span in the edit.
    pub start: f64,
    pub length: f64,
    /// Run seconds at the clip's start.
    pub source_in: f64,
    pub speed: f64,
}

impl ClipTiming {
    /// A stretch of the run as edit seconds `[start, length]`, cut to the
    /// clip's span; `None` when the clip does not show it.
    pub fn edit_span(&self, span: RunSpan) -> Option<(f64, f64)> {
        let speed = if self.speed > 0.0 { self.speed } else { 1.0 };
        let a = (self.start + (span.0 - self.source_in) / speed).max(self.start);
        let b = (self.start + (span.1 - self.source_in) / speed).min(self.start + self.length);
        (b > a).then_some((a, b - a))
    }
}
