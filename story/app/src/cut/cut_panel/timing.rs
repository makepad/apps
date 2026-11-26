//! The song's own timing, as the film's graphics read it: the shared
//! module that writes the song's beats, downbeats, sections and sung words
//! (`{beats: [...] downbeats: [...] sections: [{name start end}] lines:
//! [{words: [{w start end}]}]}`), in the cut's seconds (where the cut
//! plays the song). Cuts land on its beats; a panel's `when:` names its
//! sections, beats, words or seconds; the instruments read its beats and
//! words.
//!
//! Nothing is evaluated: the module's text is read once per revision.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SongTiming {
    pub beats: Vec<f64>,
    pub downbeats: Vec<f64>,
    pub sections: Vec<(String, f64, f64)>,
    pub words: Vec<(String, f64, f64)>,
}

/// The numbers of the list written after `key: [`.
fn list_after(text: &str, key: &str) -> Vec<f64> {
    let Some(at) = text.find(&format!("{key}: [")) else { return Vec::new() };
    let rest = &text[at + key.len() + 3..];
    let end = rest.find(']').unwrap_or(rest.len());
    rest[..end].split(',').filter_map(|n| n.trim().parse::<f64>().ok()).collect()
}

/// Each `{<label>: "x" start: a end: b ...}` record from `from` on: (x, a, b).
fn records(text: &str, label: &str) -> Vec<(String, f64, f64)> {
    let mut out = Vec::new();
    let open = format!("{{{label}: \"");
    let mut from = 0;
    while let Some(at) = text[from..].find(&open).map(|i| from + i) {
        let rest = &text[at + open.len()..];
        let Some(q) = rest.find('"') else { break };
        let name = rest[..q].to_string();
        let tail = &rest[q..rest.find('}').unwrap_or(rest.len())];
        let num = |key: &str| tail.find(&format!("{key}: ")).and_then(|i| tail[i + key.len() + 2..].split(|c: char| c == ' ' || c == ',' || c == '}').next()?.parse::<f64>().ok());
        if let (Some(s), Some(e)) = (num("start"), num("end")) {
            out.push((name, s, e));
        }
        from = at + open.len() + q;
    }
    out
}

impl SongTiming {
    /// The timing a module's text writes; `None` when it writes no beats.
    pub fn parse(text: &str) -> Option<SongTiming> {
        let beats = list_after(text, "beats");
        if beats.len() < 4 {
            return None;
        }
        Some(SongTiming { beats, downbeats: list_after(text, "downbeats"), sections: records(text, "name"), words: records(text, "w") })
    }

    /// The same timing `by` seconds later (the song placed in the cut).
    pub fn shifted(mut self, by: f64) -> Self {
        self.beats.iter_mut().chain(self.downbeats.iter_mut()).for_each(|t| *t += by);
        self.sections.iter_mut().chain(self.words.iter_mut()).for_each(|(_, a, b)| {
            *a += by;
            *b += by;
        });
        self
    }

    /// Where a `when:` says something acts: seconds (`"12s-30s"`), song
    /// sections (`"chorus1"`, `"verse1-pre1"`, `"chorus1, chorus2"`), beats
    /// (`"beats 9-16"`, 1-based) or a sung word (`"the lyric 'AGI'"`);
    /// `None` when it says none of these (it acts throughout).
    pub fn when(&self, when: &str) -> Option<Vec<(f64, f64)>> {
        let w = when.trim();
        if w.is_empty() {
            return None;
        }
        let mut out = Vec::new();
        for part in w.split(',').map(str::trim) {
            out.extend(self.one(part)?);
        }
        (!out.is_empty()).then_some(out)
    }

    fn one(&self, part: &str) -> Option<Vec<(f64, f64)>> {
        let lower = part.to_lowercase();
        // Seconds.
        let secs = |t: &str| t.trim().trim_end_matches('s').trim().parse::<f64>().ok();
        if let Some((a, b)) = lower.split_once('-') {
            if let (Some(a), Some(b)) = (secs(a), secs(b)) {
                if lower.contains('s') && !lower.starts_with("beat") {
                    return Some(vec![(a, b)]);
                }
            }
        }
        // Beats.
        if let Some(rest) = lower.strip_prefix("beats ").or_else(|| lower.strip_prefix("beat ")) {
            let (a, b) = rest.split_once('-').unwrap_or((rest, rest));
            let (a, b) = (a.trim().parse::<usize>().ok()?, b.trim().parse::<usize>().ok()?);
            let start = *self.beats.get(a.max(1) - 1)?;
            let end = self.beats.get(b).copied().unwrap_or_else(|| self.beats.last().copied().unwrap_or(start) + 0.5);
            return Some(vec![(start, end)]);
        }
        // A sung word.
        if lower.contains("lyric") || lower.contains('\'') {
            let word = part.split('\'').nth(1).unwrap_or(part.rsplit(' ').next().unwrap_or("")).to_lowercase();
            let spans: Vec<(f64, f64)> = self.words.iter().filter(|(w, ..)| w.to_lowercase().trim_matches(|c: char| !c.is_alphanumeric()) == word).map(|(_, a, b)| (*a, *b)).collect();
            return (!spans.is_empty()).then_some(spans);
        }
        // Sections, one or a run of them.
        let section = |name: &str| self.sections.iter().position(|(n, ..)| n.eq_ignore_ascii_case(name.trim().trim_start_matches("section ").trim()));
        if let Some(k) = section(part) {
            return Some(vec![(self.sections[k].1, self.sections[k].2)]);
        }
        let (a, b) = part.split_once('-')?;
        let (a, b) = (section(a)?, section(b)?);
        Some(vec![(self.sections[a.min(b)].1, self.sections[a.max(b)].2)])
    }
}
