//! The Timing step's output: the song timing module, the data a lyric
//! film's scenes key on (`use mod.motion.modules.song.*`), in the one
//! normalised shape (a film folder's `shared/<song>_timing.splash`):
//!
//! ```text
//! { beats: [t, …] downbeats: [t, …] sections: [{name start end}, …]
//!   onsets: {kind: [{time strength}, …], …}
//!   lines: [{words: [{w start end syl: [{start end}, …]}, …]}, …] }
//! ```
//!
//! A line is only its words; a word's `syl` is there only when it is sung
//! over several syllables; times are song seconds to the millisecond.
//! Sections are named from the lyric sheet's tags (`verse1`, `chorus2`),
//! spanning their sung lines and starting on the bar line before them;
//! the stretches between (an intro, a break, an outro) are sections too.

use crate::plan::{LineSpan, Section};

#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub w: String,
    pub start: f64,
    pub end: f64,
    pub syl: Vec<(f64, f64)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// The lyric sheet block it is sung in.
    pub block: usize,
    pub words: Vec<Word>,
}

impl Line {
    pub fn text(&self) -> String {
        self.words.iter().map(|word| word.w.as_str()).collect::<Vec<_>>().join(" ")
    }
    pub fn start(&self) -> f64 {
        self.words.first().map_or(0.0, |word| word.start)
    }
    pub fn end(&self) -> f64 {
        self.words.last().map_or(0.0, |word| word.end)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SongTiming {
    pub duration: f64,
    pub beats: Vec<f64>,
    pub downbeats: Vec<f64>,
    pub sections: Vec<Section>,
    /// Hits by kind (`kick`, `hat`, `vocal`, …): `(time, strength 0..1)`.
    pub onsets: Vec<(String, Vec<(f64, f32)>)>,
    pub lines: Vec<Line>,
}

/// A stretch shorter than this is not a section of its own.
const MIN_SECTION_SECS: f64 = 1.5;
/// A gap between two sung blocks longer than this is a break.
const BREAK_SECS: f64 = 6.0;

/// The bar line at or before `t` (within a bar), else `t` itself.
fn bar_before(downbeats: &[f64], t: f64) -> f64 {
    let bar = bar_secs(downbeats);
    downbeats.iter().rev().find(|d| **d <= t + 0.05).copied().filter(|d| t - d <= bar).unwrap_or(t)
}

/// The bar line at or after `t` (within a bar), else `t` itself.
fn bar_after(downbeats: &[f64], t: f64) -> f64 {
    let bar = bar_secs(downbeats);
    downbeats.iter().find(|d| **d >= t - 0.05).copied().filter(|d| d - t <= bar).unwrap_or(t)
}

fn bar_secs(downbeats: &[f64]) -> f64 {
    if downbeats.len() < 2 {
        return 2.0;
    }
    (downbeats[downbeats.len() - 1] - downbeats[0]) / (downbeats.len() - 1) as f64
}

/// The song's sections from its sung lines and the sheet's block tags
/// (`tags[block]`, `verse`, `chorus`, …): each tagged block that was sung
/// is a section from the bar line before its first word to where the next
/// begins; a long gap is a `break`, the start an `intro`, the end an
/// `outro`. Names count per kind: `verse1`, `chorus1`, `verse2`.
pub fn sections(tags: &[String], lines: &[Line], downbeats: &[f64], duration: f64) -> Vec<Section> {
    // The sung blocks in order, with their span and lines.
    let mut blocks: Vec<(usize, f64, f64, Vec<LineSpan>)> = Vec::new();
    for line in lines.iter().filter(|line| !line.words.is_empty()) {
        match blocks.last_mut() {
            Some(last) if last.0 == line.block => {
                last.2 = line.end();
                last.3.push(LineSpan { text: line.text(), start: line.start(), end: line.end() });
            }
            _ => blocks.push((line.block, line.start(), line.end(), vec![LineSpan { text: line.text(), start: line.start(), end: line.end() }])),
        }
    }
    let mut out: Vec<Section> = Vec::new();
    let mut counts: Vec<(String, usize)> = Vec::new();
    let mut name_of = |kind: &str| -> String {
        let kind = if kind.is_empty() { "part" } else { kind };
        let n = match counts.iter_mut().find(|(k, _)| k == kind) {
            Some((_, n)) => {
                *n += 1;
                *n
            }
            None => {
                counts.push((kind.to_string(), 1));
                1
            }
        };
        format!("{kind}{n}")
    };
    let mut at = 0.0f64;
    for (index, (block, start, end, texts)) in blocks.iter().enumerate() {
        let mut begin = bar_before(downbeats, *start - 0.1).max(at).min(*start);
        if out.is_empty() {
            if begin >= MIN_SECTION_SECS {
                let intro = name_of("intro");
                out.push(Section { name: intro, start: 0.0, end: begin, lines: Vec::new() });
            } else {
                begin = 0.0;
            }
        } else if begin - at >= MIN_SECTION_SECS {
            let gap = name_of("break");
            out.push(Section { name: gap, start: at, end: begin, lines: Vec::new() });
        } else if let Some(last) = out.last_mut() {
            last.end = begin;
        }
        let kind = tags.get(*block).map(String::as_str).unwrap_or("");
        let name = name_of(kind);
        // Ends where the next block's section will begin, or past its last
        // word on the next bar line; a long gap after it becomes a break.
        let next_start = blocks.get(index + 1).map(|next| next.1);
        let sung_end = bar_after(downbeats, end + 0.15).min(duration);
        let end_at = match next_start {
            Some(next) if next - end > BREAK_SECS => sung_end,
            Some(next) => bar_before(downbeats, next - 0.1).max(*end).min(next),
            None => sung_end,
        };
        out.push(Section { name, start: begin, end: end_at.max(begin), lines: texts.clone() });
        at = end_at.max(begin);
    }
    if duration - at >= MIN_SECTION_SECS {
        let outro = name_of("outro");
        out.push(Section { name: outro, start: at, end: duration, lines: Vec::new() });
    } else if let Some(last) = out.last_mut() {
        last.end = duration;
    }
    if out.is_empty() {
        out.push(Section { name: "song1".into(), start: 0.0, end: duration, lines: Vec::new() });
    }
    // A sliver is not a scene: it joins the section before it (its lines too).
    let mut merged: Vec<Section> = Vec::with_capacity(out.len());
    for section in out {
        match merged.last_mut() {
            Some(last) if section.end - section.start < MIN_SECTION_SECS => {
                last.end = section.end;
                last.lines.extend(section.lines);
            }
            _ => merged.push(section),
        }
    }
    merged
}

fn num(x: f64) -> String {
    let rounded = (x * 1000.0).round() / 1000.0;
    if rounded == rounded.trunc() {
        format!("{rounded:.1}")
    } else {
        let text = format!("{rounded:.3}");
        text.trim_end_matches('0').to_string()
    }
}

fn text(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn span(a: f64, b: f64) -> String {
    format!("start: {} end: {}", num(a), num(b))
}

fn rows(items: &[String], per: usize, indent: usize) -> String {
    if items.is_empty() {
        return "[]".into();
    }
    let pad = format!("\n{}", " ".repeat(indent));
    let chunks: Vec<String> = items.chunks(per).map(|chunk| chunk.join(", ")).collect();
    format!("[{pad}{}]", chunks.join(&format!(",{pad}")))
}

impl SongTiming {
    /// The module's Splash source. `source` says where the data came from
    /// (the header's first line).
    pub fn module_source(&self, source: &str) -> String {
        let beats: Vec<String> = self.beats.iter().map(|t| num(*t)).collect();
        let downbeats: Vec<String> = self.downbeats.iter().map(|t| num(*t)).collect();
        let sections: Vec<String> = self.sections.iter().map(|s| format!("{{name: {} {}}}", text(&s.name), span(s.start, s.end))).collect();
        let onsets: Vec<String> = self
            .onsets
            .iter()
            .map(|(kind, hits)| {
                let hits: Vec<String> = hits.iter().map(|(t, s)| format!("{{time: {} strength: {}}}", num(*t), num(*s as f64))).collect();
                format!("        {kind}: {}", rows(&hits, 4, 12))
            })
            .collect();
        let lines: Vec<String> = self
            .lines
            .iter()
            .filter(|line| !line.words.is_empty())
            .map(|line| {
                let words: Vec<String> = line
                    .words
                    .iter()
                    .map(|word| {
                        let mut record = format!("{{w: {} {}", text(&word.w), span(word.start, word.end));
                        if word.syl.len() > 1 {
                            let syl: Vec<String> = word.syl.iter().map(|(a, b)| format!("{{{}}}", span(*a, *b))).collect();
                            record.push_str(&format!(" syl: [{}]", syl.join(", ")));
                        }
                        record + "}"
                    })
                    .collect();
                format!("        {{words: [\n            {}]}}", words.join(",\n            "))
            })
            .collect();
        format!(
            "// The song's timing ({source}): beats, downbeats, sections, hits, sung lines and their words (song seconds).\n\
             // Read it with `use mod.motion.modules.song.*` (song_beat, song_hit, song_line, line_text, ...).\n\
             // A line is its words; a word is {{w start end}} and, sung over several syllables, syl: [{{start end}}, ...].\n\
             {{\n    beats: {}\n    downbeats: {}\n    sections: {}\n    onsets: {{\n{}\n    }}\n    lines: [\n{}\n    ]\n}}\n",
            rows(&beats, 16, 8),
            rows(&downbeats, 16, 8),
            rows(&sections, 4, 8),
            onsets.join("\n"),
            lines.join(",\n"),
        )
    }
}

/// Peaks of a per-frame level's rises: a hit where the level jumps more
/// than the track's strong rises, at least `gap` seconds apart.
/// `levels` is sampled every `hop` seconds; strengths are 0..1 against
/// the strongest rise.
pub fn rises(levels: &[f32], hop: f64, gap: f64) -> Vec<(f64, f32)> {
    if levels.len() < 3 {
        return Vec::new();
    }
    let rise: Vec<f32> = levels.windows(2).map(|w| (w[1] - w[0]).max(0.0)).collect();
    let mut sorted = rise.clone();
    sorted.sort_by(f32::total_cmp);
    let floor = sorted[(sorted.len() as f64 * 0.9) as usize].max(1e-4);
    let top = sorted.last().copied().unwrap_or(1.0).max(1e-4);
    let mut out: Vec<(f64, f32)> = Vec::new();
    for i in 1..rise.len().saturating_sub(1) {
        if rise[i] >= floor && rise[i] >= rise[i - 1] && rise[i] > rise[i + 1] {
            let t = (i + 1) as f64 * hop;
            match out.last_mut() {
                Some(last) if t - last.0 < gap => {
                    if rise[i] / top > last.1 {
                        *last = (t, rise[i] / top);
                    }
                }
                _ => out.push((t, rise[i] / top)),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(block: usize, words: &[(&str, f64, f64)]) -> Line {
        Line { block, words: words.iter().map(|(w, a, b)| Word { w: w.to_string(), start: *a, end: *b, syl: Vec::new() }).collect() }
    }

    #[test]
    fn sections_follow_the_sheet_on_bar_lines_with_intro_break_and_outro() {
        let downbeats: Vec<f64> = (0..40).map(|k| 0.5 + 2.0 * k as f64).collect();
        let tags = vec!["verse".to_string(), "chorus".to_string(), "verse".to_string()];
        let lines = vec![
            line(0, &[("we", 4.6, 5.0), ("go", 5.0, 6.0)]),
            line(0, &[("on", 7.0, 8.0)]),
            line(1, &[("hold", 9.0, 9.5), ("on", 9.5, 12.2)]),
            line(2, &[("again", 22.7, 24.0)]),
        ];
        let s = sections(&tags, &lines, &downbeats, 30.0);
        let names: Vec<&str> = s.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["intro1", "verse1", "chorus1", "break1", "verse2", "outro1"], "{s:?}");
        assert_eq!(s[0].start, 0.0);
        assert_eq!(s[1].start, 4.5, "a section starts on the bar line before its first word");
        assert_eq!(s[1].end, s[2].start);
        assert_eq!(s[2].end, 12.5);
        assert_eq!(s[4].start, 22.5);
        assert_eq!(s.last().unwrap().end, 30.0);
        assert!(s.windows(2).all(|w| (w[0].end - w[1].start).abs() < 1e-9), "{s:?}");
        assert_eq!(s[2].texts(), vec!["hold on"]);
        // A last line sung in the song's final second joins the section before.
        let lines = vec![line(0, &[("we", 4.6, 5.0), ("go", 28.0, 29.0)]), line(1, &[("end", 29.6, 29.9)])];
        let s = sections(&tags, &lines, &downbeats, 30.0);
        assert_eq!(s.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), vec!["intro1", "verse1"], "{s:?}");
        assert_eq!(s.last().unwrap().texts(), vec!["we go", "end"]);
        assert_eq!(s.last().unwrap().end, 30.0);
    }

    #[test]
    fn the_module_has_the_normalised_shape() {
        let timing = SongTiming {
            duration: 10.0,
            beats: vec![0.5, 1.0],
            downbeats: vec![0.5],
            sections: vec![Section { name: "verse1".into(), start: 0.0, end: 10.0, lines: Vec::new() }],
            onsets: vec![("kick".into(), vec![(0.5, 1.0), (1.25, 0.5)])],
            lines: vec![Line { block: 0, words: vec![Word { w: "say \"hi\"".into(), start: 1.0, end: 1.5, syl: Vec::new() }, Word { w: "window".into(), start: 1.5, end: 2.1234, syl: vec![(1.5, 1.8), (1.8, 2.1234)] }] }],
        };
        let source = timing.module_source("test");
        assert!(source.contains("beats: [\n        0.5, 1.0]"), "{source}");
        assert!(source.contains("sections: [\n        {name: \"verse1\" start: 0.0 end: 10.0}]"), "{source}");
        assert!(source.contains("kick: [\n            {time: 0.5 strength: 1.0}, {time: 1.25 strength: 0.5}]"), "{source}");
        assert!(source.contains("{w: \"say \\\"hi\\\"\" start: 1.0 end: 1.5}"), "{source}");
        assert!(source.contains("{w: \"window\" start: 1.5 end: 2.123 syl: [{start: 1.5 end: 1.8}, {start: 1.8 end: 2.123}]}"), "{source}");
    }

    #[test]
    fn rises_are_the_level_jumps() {
        let levels = [0.0, 0.0, 1.0, 0.9, 0.8, 0.0, 0.0, 0.7, 0.6, 0.5, 0.4, 0.3];
        let hits = rises(&levels, 0.1, 0.05);
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert!((hits[0].0 - 0.2).abs() < 1e-9 && hits[0].1 == 1.0);
    }
}
