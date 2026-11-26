//! A sung line's words rewritten in the song's timing: the line is the list
//! of its word records (`{w: "see" start: 2.35 end: 2.739}`, a long word
//! with its `syl:` too), and new words are laid over it. A word still there
//! (in order) keeps its record as written; words typed in place of others
//! share the span those took (by their letters); a word typed between two
//! takes its part of its neighbour's span; a word taken out gives its span
//! to its neighbour (the one before, else the one after). A scene that
//! names the line's words by place keeps its records instead
//! (`rewrite_in_place`).
//!
//! Main-loop cost: per keystroke a parse of the line (its few records) and
//! a word alignment (line length squared).

/// A word record of the line as written.
#[derive(Clone, Debug)]
pub struct Rec {
    pub text: String,
    pub w: String,
    pub start: f64,
    pub end: f64,
}

/// The inside of a line's word list (between its `[` and `]`).
#[derive(Clone, Debug)]
pub struct Line {
    prefix: String,
    sep: String,
    suffix: String,
    pub recs: Vec<Rec>,
}

/// A string literal's end (its closing quote) from just past its opening
/// one, and its words unescaped.
fn string_at(s: &str, from: usize) -> Option<(usize, String)> {
    let b = s.as_bytes();
    let mut i = from;
    let mut out = String::new();
    while i < b.len() {
        match b[i] {
            b'"' => return Some((i, out)),
            b'\\' => {
                let c = s[i + 1..].chars().next()?;
                out.push(match c {
                    'n' => '\n',
                    c => c,
                });
                i += 1 + c.len_utf8();
            }
            _ => {
                let c = s[i..].chars().next()?;
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    None
}

/// A number written after `key: ` at the record's own level (not inside
/// its `syl:` list).
fn field(rec: &str, key: &str) -> Option<f64> {
    let b = rec.as_bytes();
    let pat = format!("{key}:");
    let mut depth = 0i32;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => i = string_at(rec, i + 1)?.0,
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth -= 1,
            _ if depth == 1 && rec[i..].starts_with(&pat) && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')) => {
                let rest = rec[i + pat.len()..].trim_start();
                let n: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-').collect();
                return n.parse().ok();
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The words kept in order between `old` and `new` (index pairs, the
/// longest such run), ended by (old.len(), new.len()).
fn align(old: &[&str], new: &[&str]) -> Vec<(usize, usize)> {
    let (n, m) = (old.len(), new.len());
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old[i] == new[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
        }
    }
    let mut pairs = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if old[i] == new[j] {
            pairs.push((i, j));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs.push((n, m));
    pairs
}

/// A word string as written inside a record's quotes.
fn quoted(w: &str) -> String {
    w.replace('\\', "\\\\").replace('"', "\\\"")
}

impl Line {
    /// The records of a list's inside; None when it is not a list of word
    /// records.
    pub fn parse(inner: &str) -> Option<Line> {
        let b = inner.as_bytes();
        let mut spans = Vec::new();
        let (mut depth, mut open) = (0, 0);
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'"' => i = string_at(inner, i + 1)?.0,
                b'{' | b'[' => {
                    if depth == 0 {
                        if b[i] != b'{' {
                            return None;
                        }
                        open = i;
                    }
                    depth += 1;
                }
                b'}' | b']' => {
                    depth -= 1;
                    if depth == 0 {
                        spans.push(open..i + 1);
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let first = spans.first()?.clone();
        let last = spans.last()?.clone();
        let sep = if spans.len() > 1 { inner[spans[0].end..spans[1].start].to_string() } else { ", ".to_string() };
        let recs = spans.iter().map(|r| {
            let text = &inner[r.clone()];
            let w_at = text.find("w: \"")? + 4;
            Some(Rec { text: text.to_string(), w: string_at(text, w_at)?.1, start: field(text, "start")?, end: field(text, "end")? })
        }).collect::<Option<Vec<_>>>()?;
        Some(Line { prefix: inner[..first.start].to_string(), sep, suffix: inner[last.end..].to_string(), recs })
    }

    /// The list's inside with `words` sung over the line, keeping its
    /// records (a scene that names the line's words by place, `sung(LINE,
    /// 4)`, finds each where it was): a word typed over another takes its
    /// record (its timing and syllables kept); words typed between two join
    /// the word before them ("see you" over see's time), or the first
    /// word's; a word taken out leaves its record empty (its time silent).
    pub fn rewrite_in_place(&self, words: &[&str]) -> Option<String> {
        if words.is_empty() {
            return None;
        }
        let old = self.words();
        let mut out: Vec<Vec<String>> = vec![Vec::new(); old.len()];
        let mut lead: Vec<String> = Vec::new();
        let (mut i, mut j) = (0, 0);
        for (pi, pj) in align(&old, words) {
            let gone = i..pi;
            let typed: Vec<String> = words[j..pj].iter().map(|w| w.to_string()).collect();
            if gone.is_empty() {
                // Typed between: onto the word before (else the next one).
                match i.checked_sub(1) {
                    Some(prev) => out[prev].extend(typed),
                    None => lead.extend(typed),
                }
            } else {
                // Over the records gone, one each; the rest onto the last.
                let mut typed = typed.into_iter();
                for k in gone.clone() {
                    if let Some(w) = typed.next() {
                        out[k].push(w);
                    }
                }
                out[gone.end - 1].extend(typed);
            }
            if pi < old.len() {
                out[pi].push(old[pi].to_string());
            }
            i = pi + 1;
            j = pj + 1;
        }
        if !lead.is_empty() {
            let first = std::mem::take(&mut out[0]);
            out[0] = lead.into_iter().chain(first).collect();
        }
        let recs: Vec<String> = self.recs.iter().zip(&out).map(|(r, ws)| {
            let w = ws.join(" ");
            if w == r.w {
                return r.text.clone();
            }
            // The record as written, its `w` replaced.
            let at = r.text.find("w: \"").map(|k| k + 4).unwrap_or(0);
            let end = string_at(&r.text, at).map_or(at, |(e, _)| e);
            format!("{}{}{}", &r.text[..at], quoted(&w), &r.text[end..])
        }).collect();
        Some(format!("{}{}{}", self.prefix, recs.join(&self.sep), self.suffix))
    }

    pub fn words(&self) -> Vec<&str> {
        self.recs.iter().map(|r| r.w.as_str()).collect()
    }

    /// The list's inside with `words` sung over the line's time (None: no
    /// words, a line keeps one).
    pub fn rewrite(&self, words: &[&str]) -> Option<String> {
        if words.is_empty() {
            return None;
        }
        let old = self.words();
        let pairs = align(&old, words);
        let n = old.len();
        // (word, start, end, kept record)
        let mut out: Vec<(String, f64, f64, Option<usize>)> = Vec::new();
        // Inserted before any record: split from the next one emitted.
        let mut front: Vec<String> = Vec::new();
        // Taken out before any record: its start goes to the next one.
        let mut front_start: Option<f64> = None;
        let weight = |w: &str| w.chars().count() as f64 + 1.0;
        let split = |words: &[String], start: f64, end: f64| -> Vec<(String, f64, f64)> {
            let total: f64 = words.iter().map(|w| weight(w)).sum();
            let mut at = start;
            words.iter().enumerate().map(|(k, w)| {
                let to = if k + 1 == words.len() { end } else { at + (end - start) * weight(w) / total };
                let piece = (w.clone(), at, to);
                at = to;
                piece
            }).collect()
        };
        let (mut i, mut j) = (0, 0);
        for (pi, pj) in pairs {
            let gone = i..pi;
            let typed: Vec<String> = words[j..pj].iter().map(|w| w.to_string()).collect();
            if !gone.is_empty() && !typed.is_empty() {
                let (s, e) = (self.recs[gone.start].start, self.recs[gone.end - 1].end);
                for (w, s, e) in split(&typed, s, e) {
                    out.push((w, s, e, None));
                }
            } else if !gone.is_empty() {
                let (s, e) = (self.recs[gone.start].start, self.recs[gone.end - 1].end);
                match out.last_mut() {
                    Some(prev) => {
                        prev.2 = e;
                        prev.3 = None;
                    }
                    None => front_start = Some(front_start.unwrap_or(s)),
                }
            } else if !typed.is_empty() {
                match out.pop() {
                    Some((w, s, e, _)) => {
                        let all: Vec<String> = std::iter::once(w).chain(typed).collect();
                        for (w, s, e) in split(&all, s, e) {
                            out.push((w, s, e, None));
                        }
                    }
                    None => front = typed,
                }
            }
            if pi < n {
                let r = &self.recs[pi];
                out.push((r.w.clone(), front_start.take().unwrap_or(r.start), r.end, Some(pi)));
                if out.last().is_some_and(|o| o.1 != r.start) {
                    out.last_mut().unwrap().3 = None;
                }
            }
            i = pi + 1;
            j = pj + 1;
        }
        if !front.is_empty() {
            // Words before the first: split from the line's first word.
            let (w, s, e, _) = out.remove(0);
            let all: Vec<String> = front.into_iter().chain(std::iter::once(w)).collect();
            for (k, (w, s, e)) in split(&all, s, e).into_iter().enumerate() {
                out.insert(k, (w, s, e, None));
            }
        }
        let ms = |x: f64| makepad_motion::patch::fmt_num((x * 1000.0).round() / 1000.0);
        let recs: Vec<String> = out.iter().map(|(w, s, e, kept)| match kept {
            Some(k) => self.recs[*k].text.clone(),
            None => format!("{{w: \"{}\" start: {} end: {}}}", quoted(w), ms(*s), ms(*e)),
        }).collect();
        Some(format!("{}{}{}", self.prefix, recs.join(&self.sep), self.suffix))
    }
}

#[cfg(test)]
mod tests {
    use super::Line;

    const LINE: &str = "\n    {w: \"I\" start: 1.0 end: 2.0},\n    {w: \"see\" start: 2.0 end: 3.0},\n    {w: \"AGI\" start: 3.0 end: 4.5 syl: [{start: 3.0 end: 4.0}, {start: 4.0 end: 4.5}]},\n    {w: \"now\" start: 4.6 end: 5.0}";

    fn timed(inner: &str) -> Vec<(String, f64, f64)> {
        Line::parse(inner).unwrap().recs.into_iter().map(|r| (r.w, r.start, r.end)).collect()
    }

    #[test]
    fn the_same_words_write_the_same_text() {
        let line = Line::parse(LINE).unwrap();
        assert_eq!(line.words(), ["I", "see", "AGI", "now"]);
        assert_eq!(line.rewrite(&["I", "see", "AGI", "now"]).unwrap(), LINE);
    }

    #[test]
    fn a_word_typed_over_another_takes_its_span() {
        let out = Line::parse(LINE).unwrap().rewrite(&["I", "saw", "AGI", "now"]).unwrap();
        assert_eq!(timed(&out)[1], ("saw".to_string(), 2.0, 3.0));
        // The others as written (AGI keeps its syllables).
        assert!(out.contains("{w: \"AGI\" start: 3.0 end: 4.5 syl: [{start: 3.0 end: 4.0}, {start: 4.0 end: 4.5}]}"));
    }

    #[test]
    fn an_added_word_splits_its_neighbour() {
        let out = Line::parse(LINE).unwrap().rewrite(&["I", "see", "you", "AGI", "now"]).unwrap();
        let t = timed(&out);
        assert_eq!(t.len(), 5);
        // "see" and "you" share see's second (by letters: 4 and 4).
        assert_eq!(t[1], ("see".to_string(), 2.0, 2.5));
        assert_eq!(t[2], ("you".to_string(), 2.5, 3.0));
        assert_eq!(t[3].1, 3.0);
        // At the front: split from the first word.
        let t = timed(&Line::parse(LINE).unwrap().rewrite(&["Oh", "I", "see", "AGI", "now"]).unwrap());
        assert_eq!((t[0].1, t[1].2), (1.0, 2.0));
        assert_eq!(t[1].0, "I");
    }

    #[test]
    fn in_place_keeps_every_record_where_it_was() {
        let line = Line::parse(LINE).unwrap();
        let same = |out: &str| Line::parse(out).unwrap().recs.into_iter().map(|r| (r.start, r.end)).collect::<Vec<_>>();
        let times = same(LINE);
        // An added word joins the one before; the count and times stay.
        let out = line.rewrite_in_place(&["I", "see", "wow", "AGI", "now"]).unwrap();
        assert_eq!(Line::parse(&out).unwrap().words(), ["I", "see wow", "AGI", "now"]);
        assert_eq!(same(&out), times);
        // A word typed over another: its record, syllables kept.
        let out = line.rewrite_in_place(&["I", "see", "ASI", "now"]).unwrap();
        assert!(out.contains("{w: \"ASI\" start: 3.0 end: 4.5 syl: [{start: 3.0 end: 4.0}, {start: 4.0 end: 4.5}]}"), "{out}");
        // A removed word leaves its record silent.
        let out = line.rewrite_in_place(&["I", "AGI", "now"]).unwrap();
        assert_eq!(Line::parse(&out).unwrap().words(), ["I", "", "AGI", "now"]);
        // At the front: onto the first word.
        let out = line.rewrite_in_place(&["Oh", "I", "see", "AGI", "now"]).unwrap();
        assert_eq!(Line::parse(&out).unwrap().words(), ["Oh I", "see", "AGI", "now"]);
        assert_eq!(line.rewrite_in_place(&["I", "see", "AGI", "now"]).unwrap(), LINE);
    }

    #[test]
    fn a_removed_word_merges_into_its_neighbour() {
        let t = timed(&Line::parse(LINE).unwrap().rewrite(&["I", "AGI", "now"]).unwrap());
        assert_eq!(t[0], ("I".to_string(), 1.0, 3.0));
        assert_eq!(t[1].1, 3.0);
        // The first word removed: the next one starts where it did.
        let t = timed(&Line::parse(LINE).unwrap().rewrite(&["see", "AGI", "now"]).unwrap());
        assert_eq!(t[0], ("see".to_string(), 1.0, 3.0));
        assert!(Line::parse(LINE).unwrap().rewrite(&[]).is_none());
    }
}
