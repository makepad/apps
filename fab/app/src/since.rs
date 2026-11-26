//! What the person changed since the AI's last turn, told to the AI with their next line: the
//! model history's steps newer than the history as that turn found it that no AI call made, and
//! the steps of it the person undid. Read from the steps the host's status carries
//! ([`model_host::Status::steps`], oldest first by number); the chat's target carries the words
//! and is refreshed when they change, so a line sent at once has them.
//!
//! Cost: when the history moves, O(its steps x log(the marked steps)); one pointer compare per
//! host status otherwise (an unchanged history's steps are the same `Arc`).

use std::sync::Arc;

/// Steps by number, oldest first, each with its label.
pub(super) type Steps = Arc<Vec<(u64, Arc<str>)>>;

#[derive(Default)]
pub(super) struct Since {
    /// The history as the last turn found it (or as the plate opened).
    mark: Steps,
    /// The mark is a turn's (else the plate's opening).
    turned: bool,
    /// The history the words were read from.
    seen: Steps,
    /// What changed since, as the AI reads it (empty: nothing).
    pub words: String,
}

impl Since {
    /// An AI turn starts (`turn`), or another document is on the bed: later steps are the next
    /// report's.
    pub fn mark(&mut self, steps: &Steps, turn: bool) {
        self.mark = steps.clone();
        self.seen = steps.clone();
        self.turned = turn;
        self.words.clear();
    }

    /// The history is `steps` now; `ai(seq)`: an AI call made step `seq`. True when the words
    /// changed.
    pub fn follow(&mut self, steps: &Steps, ai: &dyn Fn(u64) -> bool) -> bool {
        if Arc::ptr_eq(steps, &self.seen) {
            return false;
        }
        self.seen = steps.clone();
        let words = words(&self.mark, steps, ai, if self.turned { "your last turn" } else { "the plate opened" });
        if words == self.words {
            return false;
        }
        self.words = words;
        true
    }
}

/// The person's steps after `mark` in `steps` and the marked steps they undid, in words; a run of
/// one label (a value dragged again and again) is said once with its count.
fn words(mark: &[(u64, Arc<str>)], steps: &[(u64, Arc<str>)], ai: &dyn Fn(u64) -> bool, since: &str) -> String {
    let marked = |seq: u64| mark.binary_search_by_key(&seq, |(s, _)| *s).is_ok();
    let held = |seq: u64| steps.binary_search_by_key(&seq, |(s, _)| *s).is_ok();
    let made: Vec<&str> = steps.iter().filter(|(seq, _)| !marked(*seq) && !ai(*seq)).map(|(_, l)| &**l).collect();
    // a history past its length drops its oldest steps: those are not undone
    let floor = steps.first().map_or(0, |(seq, _)| *seq);
    let undid: Vec<&str> = mark.iter().rev().filter(|(seq, _)| *seq >= floor && !held(*seq)).map(|(_, l)| &**l).collect();
    let mut out = Vec::new();
    if !made.is_empty() {
        out.push(format!("made {}", runs(&made)));
    }
    if !undid.is_empty() {
        out.push(format!("undid {}", runs(&undid)));
    }
    if out.is_empty() {
        return String::new();
    }
    format!("Since {since} the person, by hand: {}.", out.join("; "))
}

/// `labels` in order, each quoted, a run of one label said once with its count
/// (`` `vase: set vase.vase_blend` x3 ``).
fn runs(labels: &[&str]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < labels.len() {
        let n = labels[i..].iter().take_while(|l| **l == labels[i]).count();
        out.push(if n > 1 { format!("`{}` x{n}", labels[i]) } else { format!("`{}`", labels[i]) });
        i += n;
    }
    out.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steps(xs: &[(u64, &str)]) -> Steps {
        Arc::new(xs.iter().map(|(s, l)| (*s, Arc::from(*l))).collect())
    }

    #[test]
    fn the_person_s_steps_since_the_mark_in_words() {
        let mut since = Since::default();
        since.mark(&steps(&[(1, "vase: set vase.r"), (2, "mount: build on mount")]), true);
        let ai = |seq: u64| seq == 4;
        assert!(!since.follow(&steps(&[(1, "vase: set vase.r"), (2, "mount: build on mount")]), &ai) && since.words.is_empty(), "nothing since the mark says nothing");
        // the person drags a value three times, the AI commits step 4, the person undoes nothing
        let now = steps(&[(1, "vase: set vase.r"), (2, "mount: build on mount"), (3, "vase: set vase.blend"), (4, "mount: build on mount"), (5, "vase: set vase.blend"), (6, "vase: set vase.blend")]);
        assert!(since.follow(&now, &ai));
        assert_eq!(since.words, "Since your last turn the person, by hand: made `vase: set vase.blend` x3.", "the AI's own step is not the person's, a run is said once");
        // the person undoes back below the mark
        assert!(since.follow(&steps(&[(1, "vase: set vase.r")]), &ai));
        assert_eq!(since.words, "Since your last turn the person, by hand: undid `mount: build on mount`.", "an undone marked step is said");
        // a history past its length dropped step 1: not an undo
        assert!(since.follow(&steps(&[(2, "mount: build on mount"), (7, "move mount")]), &ai));
        assert_eq!(since.words, "Since your last turn the person, by hand: made `move mount`.", "a dropped oldest step is not undone");
        since.mark(&steps(&[(2, "mount: build on mount"), (7, "move mount")]), false);
        assert!(since.words.is_empty(), "a new mark starts the next report empty");
        assert!(since.follow(&steps(&[(2, "mount: build on mount"), (7, "move mount"), (8, "move mount")]), &ai));
        assert_eq!(since.words, "Since the plate opened the person, by hand: made `move mount`.", "before any turn it is since the plate opened");
    }
}
