//! A KNOWN lyric sheet against the song that sings it: every word of the
//! sheet, as written, with when it is sung.
//!
//! The bake ([`crate::bake`]) times what whisper heard, and a heard word
//! can be misheard, split, joined or invented. When the words were written
//! before the song was made (a generated song, a lyric sheet the person
//! gave) the sheet is the truth and only the times are wanted:
//!
//! 1. **Heard pass**: the bake's own pipeline over the whole song (cross-
//!    attention DTW, teacher-forced segments, onset snapping): heard words
//!    with times.
//! 2. **Match**: the sheet's words against the heard ones by an edit-
//!    distance alignment (exact and near spellings match; a sheet word
//!    never sung and a heard word not on the sheet are skips). Each sheet
//!    line gets the heard span its matched words cover, or, with none,
//!    the span its place in the sheet puts it at between its neighbours.
//! 3. **Forced pass**: each sheet line's own text teacher-forced
//!    ([`WhisperState::align_text`]) against a window around that span. The
//!    text cannot change, only the timing.
//! 4. **Check**: the forced words are kept when the model was listening
//!    (attention mass) and they agree with the heard words they matched;
//!    otherwise the line keeps its heard times, its unmatched words spread
//!    between them. A line with no match over a quiet vocal is not sung.
//! 5. **Syllables**: each word's spelling cut by English rules, each
//!    boundary on the vocal onset nearest where the letters put it.
//!
//! Every line reports how it was timed and how far its words sit from the
//! heard ones ([`SungLine::agreement`]): the honest per-line number.

use crate::align::{self, OnsetPreset, PipelineConfig, VocalAnalysis};
use makepad_ai_speech::whisper::{WhisperModel, WhisperState, MAX_ALIGN_SECS};

/// Context either side of a line's estimated span for the forced pass.
const WINDOW_MARGIN_SECS: f64 = 1.0;
/// A sung word's typical length, for spans the heard words do not cover.
const WORD_SECS: f64 = 0.35;
/// Forced words this far (median start difference) from the heard words
/// they matched are not trusted over them.
const AGREE_SECS: f64 = 0.4;
/// Mean attention mass below which a forced line was not listened to.
const SCORE_FLOOR: f32 = 0.15;
/// A line with no heard match is forced only over a vocal at least this
/// loud (fraction of the track's loud level).
const VOCAL_FLOOR: f32 = 0.12;
/// How far a forced word start may be pulled to a vocal onset.
const SNAP_SECS: f64 = 0.08;
/// No two words share an instant.
const MIN_WORD_SECS: f64 = 0.06;
/// The least time per word a sung line takes (a line forced into less was
/// not found).
const MIN_LINE_WORD_SECS: f64 = 0.15;

/// A lyric sheet: section-tagged blocks of lines, as a song generator takes
/// them (`[Verse]`, `[Chorus]`, ...).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sheet {
    pub blocks: Vec<Block>,
}

/// One tagged block. `tag` is the tag's kind in lowercase letters
/// (`verse`, `chorus`, `pre`, `bridge`, `intro`, `outro`, ...); untagged
/// lines before any tag are a block with an empty tag.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Block {
    pub tag: String,
    pub lines: Vec<String>,
}

impl Sheet {
    /// Read a sheet: `[Tag]` lines open blocks, other non-empty lines are
    /// sung lines. A tag's kind is its letters (`[Verse 2]` is `verse`,
    /// `[Pre-Chorus]` is `pre`).
    pub fn parse(text: &str) -> Sheet {
        let mut blocks: Vec<Block> = Vec::new();
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                blocks.push(Block { tag: tag_kind(&line[1..line.len() - 1]), lines: Vec::new() });
                continue;
            }
            if blocks.is_empty() {
                blocks.push(Block::default());
            }
            blocks.last_mut().unwrap().lines.push(line.split_whitespace().collect::<Vec<_>>().join(" "));
        }
        Sheet { blocks }
    }

    pub fn words(&self) -> usize {
        self.blocks.iter().flat_map(|block| &block.lines).map(|line| line.split_whitespace().count()).sum()
    }
}

fn tag_kind(tag: &str) -> String {
    let letters: String = tag.chars().filter(|c| c.is_alphabetic()).flat_map(char::to_lowercase).collect();
    match letters.as_str() {
        "prechorus" | "prehook" | "pre" => "pre".into(),
        "postchorus" => "post".into(),
        "refrain" => "chorus".into(),
        _ => letters,
    }
}

/// How a word's time was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How {
    /// The forced pass of the sheet's own line.
    Aligned,
    /// The heard word it matched.
    Heard,
    /// Spread between timed neighbours (neither pass placed it).
    Spread,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SungWord {
    /// The word as written on the sheet.
    pub w: String,
    pub start: f64,
    pub end: f64,
    /// Its sung syllables, `(start, end)`, when there are several.
    pub syl: Vec<(f64, f64)>,
    pub how: How,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SungLine {
    /// The sheet block it is in.
    pub block: usize,
    pub words: Vec<SungWord>,
    /// Sheet words matched to heard words.
    pub matched: usize,
    /// Median |start − heard start| over the matched words, seconds.
    pub agreement: Option<f64>,
    /// The forced pass's timing was kept.
    pub forced: bool,
}

impl SungLine {
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

/// The sheet, timed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SungLyrics {
    /// The sung lines in order (an unsung sheet line is left out).
    pub lines: Vec<SungLine>,
    /// Sheet lines not found in the song: `(block, text)`.
    pub unsung: Vec<(usize, String)>,
    pub sheet_words: usize,
    pub heard_words: usize,
    pub matched_words: usize,
    /// What whisper heard, for the record.
    pub heard_text: String,
    /// The vocal's attacks (seconds), for whoever keys on them.
    pub vocal_onsets: Vec<(f64, f32)>,
}

/// One heard word, its spelling folded for matching.
#[derive(Clone, Debug)]
struct Heard {
    key: String,
    start: f64,
    end: f64,
}

/// Lowercase letters and digits only: what a match compares.
pub fn fold(word: &str) -> String {
    word.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut diagonal = row[0];
        row[0] = i;
        for j in 1..=b.len() {
            let up = row[j];
            row[j] = (row[j] + 1).min(row[j - 1] + 1).min(diagonal + usize::from(a[i - 1] != b[j - 1]));
            diagonal = up;
        }
    }
    row[b.len()]
}

/// The cost of pairing a sheet word with a heard one: 0 exact, below 1 for
/// a near spelling (a match), `None` for no match.
fn pair_cost(sheet: &str, heard: &str) -> Option<f64> {
    if sheet.is_empty() || heard.is_empty() {
        return None;
    }
    if sheet == heard {
        return Some(0.0);
    }
    let distance = edit_distance(sheet, heard);
    let longest = sheet.chars().count().max(heard.chars().count());
    (distance * 3 <= longest).then_some(0.4 + 0.4 * distance as f64 / longest as f64)
}

/// For each sheet word, the heard word it matched: an edit-distance
/// alignment where a sheet word left out or a heard word not on the sheet
/// costs 1 and a pair costs [`pair_cost`].
pub fn match_words(sheet: &[String], heard: &[String]) -> Vec<Option<usize>> {
    let (n, m) = (sheet.len(), heard.len());
    let mut cost = vec![0.0f64; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in 0..=n {
        cost[at(i, 0)] = i as f64;
    }
    for j in 0..=m {
        cost[at(0, j)] = j as f64;
    }
    for i in 1..=n {
        for j in 1..=m {
            let mut best = (cost[at(i - 1, j)] + 1.0).min(cost[at(i, j - 1)] + 1.0);
            if let Some(pair) = pair_cost(&sheet[i - 1], &heard[j - 1]) {
                best = best.min(cost[at(i - 1, j - 1)] + pair);
            }
            cost[at(i, j)] = best;
        }
    }
    let mut out = vec![None; n];
    let (mut i, mut j) = (n, m);
    while i > 0 && j > 0 {
        let here = cost[at(i, j)];
        if let Some(pair) = pair_cost(&sheet[i - 1], &heard[j - 1]) {
            if (here - (cost[at(i - 1, j - 1)] + pair)).abs() < 1e-9 {
                out[i - 1] = Some(j - 1);
                i -= 1;
                j -= 1;
                continue;
            }
        }
        if (here - (cost[at(i - 1, j)] + 1.0)).abs() < 1e-9 {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    out
}

/// One sheet line with its place in the flat word list.
struct LineRef {
    block: usize,
    text: String,
    first: usize,
    count: usize,
}

/// Time `sheet` in a song whose vocal (or mix) is `mono` at `rate` Hz. The
/// heard pass runs the bake's pipeline with `model`/`state`.
pub fn align_sheet(
    state: &mut WhisperState,
    model: &WhisperModel,
    mono: &[f32],
    rate: f64,
    duration_secs: f64,
    sheet: &Sheet,
    language: &str,
) -> SungLyrics {
    let samples_16k = align::resample(mono, rate, align::WHISPER_RATE);
    let analysis = align::analyze_vocals(mono, rate, OnsetPreset::Snapping);
    let mut params = makepad_ai_speech::whisper::WhisperParams::default();
    params.language = language.to_string();
    params.no_timestamps = false;
    params.single_segment = false;
    params.temperature = 0.0;
    params.suppress_blank = true;
    let aligned = state.transcribe_aligned(model, &samples_16k, &params);
    let config = PipelineConfig { language: language.to_string(), force: true, snap: true, max_tokens: 0 };
    let (segments, _) = align::refine(state, model, &samples_16k, aligned, &analysis, duration_secs, &config);
    let heard: Vec<Heard> = segments
        .iter()
        .flat_map(|segment| segment.words.iter())
        .flat_map(|word| {
            // A heard "hold-on" or "two words" token pair is split like the sheet's words.
            word.text.split(|c: char| c.is_whitespace() || c == '-').filter(|part| !fold(part).is_empty()).map(|part| Heard { key: fold(part), start: word.start, end: word.end }).collect::<Vec<_>>()
        })
        .collect();
    let heard_text = segments.iter().map(|segment| segment.text.trim()).collect::<Vec<_>>().join(" ");
    let mut out = time_sheet(state, model, &samples_16k, &analysis, &heard, sheet, duration_secs, language);
    out.heard_text = heard_text;
    out
}

#[allow(clippy::too_many_arguments)]
fn time_sheet(
    state: &mut WhisperState,
    model: &WhisperModel,
    samples_16k: &[f32],
    analysis: &VocalAnalysis,
    heard: &[Heard],
    sheet: &Sheet,
    duration: f64,
    language: &str,
) -> SungLyrics {
    let mut lines: Vec<LineRef> = Vec::new();
    let mut words: Vec<String> = Vec::new();
    for (block, b) in sheet.blocks.iter().enumerate() {
        for text in &b.lines {
            let first = words.len();
            words.extend(text.split_whitespace().map(str::to_string));
            lines.push(LineRef { block, text: text.clone(), first, count: words.len() - first });
        }
    }
    let keys: Vec<String> = words.iter().map(|word| fold(word)).collect();
    let heard_keys: Vec<String> = heard.iter().map(|word| word.key.clone()).collect();
    let map = match_words(&keys, &heard_keys);
    let matched_words = map.iter().filter(|m| m.is_some()).count();

    // Where each sheet word is expected: its heard time, else interpolated
    // by word count between the nearest matched words around it.
    let anchors: Vec<(usize, f64, f64)> = map.iter().enumerate().filter_map(|(i, m)| m.map(|j| (i, heard[j].start, heard[j].end))).collect();
    let expect = |i: usize| -> (f64, f64) {
        if let Some(j) = map[i] {
            return (heard[j].start, heard[j].end);
        }
        let before = anchors.iter().rev().find(|a| a.0 < i);
        let after = anchors.iter().find(|a| a.0 > i);
        match (before, after) {
            (Some(b), Some(a)) => {
                let u = (i - b.0) as f64 / (a.0 - b.0) as f64;
                let t = b.2 + (a.1 - b.2) * u;
                (t, (t + WORD_SECS).min(a.1))
            }
            (Some(b), None) => {
                let t = b.2 + (i - b.0 - 1) as f64 * WORD_SECS;
                (t.min(duration), (t + WORD_SECS).min(duration))
            }
            (None, Some(a)) => {
                let t = (a.1 - (a.0 - i) as f64 * WORD_SECS).max(0.0);
                (t, (t + WORD_SECS).min(a.1))
            }
            (None, None) => (0.0, 0.0),
        }
    };

    let mut out = SungLyrics { sheet_words: words.len(), heard_words: heard.len(), matched_words, ..SungLyrics::default() };
    out.vocal_onsets = analysis.onsets.iter().map(|onset| (onset.time, onset.strength)).collect();
    let mut floor = 0.0f64;
    for (index, line) in lines.iter().enumerate() {
        let range = line.first..line.first + line.count;
        let matched: Vec<(usize, usize)> = range.clone().filter_map(|i| map[i].map(|j| (i, j))).collect();
        let (est_start, _) = expect(line.first);
        let (_, est_end) = expect(line.first + line.count - 1);
        // A line no heard word matched is looked for only where the song
        // still has room for it and someone sings there.
        let no_room = est_start >= duration - WORD_SECS * line.count as f64 * 0.5;
        if anchors.is_empty() || (matched.is_empty() && (no_room || analysis.peak_between(est_start, est_end.max(est_start + 0.5)) < analysis.loud * VOCAL_FLOOR)) {
            out.unsung.push((line.block, line.text.clone()));
            continue;
        }
        // The window: the estimated span with context, never reaching back
        // over the previous line's words nor past the next line's first
        // heard word.
        let next_heard = lines[index + 1..].iter().find_map(|next| (next.first..next.first + next.count).find_map(|i| map[i]).map(|j| heard[j].start));
        let from = (est_start - WINDOW_MARGIN_SECS).max(floor).max(0.0);
        let mut to = (est_end + WINDOW_MARGIN_SECS).min(duration);
        if let Some(next) = next_heard {
            to = to.min(next + 0.1).max(est_end.min(duration));
        }
        to = to.min(from + MAX_ALIGN_SECS - 0.5);
        let forced = if to - from >= 0.4 {
            let a = (from * align::WHISPER_RATE) as usize;
            let b = ((to * align::WHISPER_RATE) as usize).min(samples_16k.len());
            if b > a { state.align_text(model, &samples_16k[a..b], &line.text, language).ok() } else { None }
        } else {
            None
        };
        let heard_start = |i: usize| map[i].map(|j| heard[j].start);
        let agreement_of = |starts: &[f64]| -> Option<f64> {
            let mut diffs: Vec<f64> = range.clone().zip(starts).filter_map(|(i, start)| heard_start(i).map(|h| (start - h).abs())).collect();
            if diffs.is_empty() {
                return None;
            }
            diffs.sort_by(f64::total_cmp);
            Some(diffs[diffs.len() / 2])
        };
        let mut timed: Option<(Vec<(f64, f64)>, How)> = None;
        if let Some(forced) = forced.filter(|forced| forced.len() == line.count) {
            let spans: Vec<(f64, f64)> = forced.iter().map(|word| (from + word.start, from + word.end)).collect();
            let score = forced.iter().map(|word| word.score).sum::<f32>() / forced.len().max(1) as f32;
            let starts: Vec<f64> = spans.iter().map(|span| span.0).collect();
            // Forced words with nothing heard to check them against must at
            // least have been listened to and take the time words take.
            let span = spans.last().map_or(0.0, |last| last.1) - spans.first().map_or(0.0, |first| first.0);
            let agrees = match agreement_of(&starts) {
                Some(median) => median <= AGREE_SECS,
                None => score >= SCORE_FLOOR * 2.0 && span >= MIN_LINE_WORD_SECS * line.count as f64,
            };
            if score >= SCORE_FLOOR && agrees {
                timed = Some((spans, How::Aligned));
            }
        }
        let (spans, how) = match timed {
            Some(found) => found,
            None if !matched.is_empty() => (spread(range.clone().map(|i| map[i].map(|j| (heard[j].start, heard[j].end))).collect(), floor), How::Heard),
            None => {
                out.unsung.push((line.block, line.text.clone()));
                continue;
            }
        };
        let mut sung: Vec<SungWord> = Vec::with_capacity(line.count);
        for (k, (text, (start, end))) in line.text.split_whitespace().zip(spans).enumerate() {
            let mut start = start;
            if how == How::Aligned {
                if let Some(onset) = analysis.onset_near(start, SNAP_SECS) {
                    start = onset.time;
                }
            }
            let word_how = if how == How::Heard && map[line.first + k].is_none() { How::Spread } else { how };
            sung.push(SungWord { w: text.to_string(), start, end, syl: Vec::new(), how: word_how });
        }
        // In order, inside the song, never overlapping, after the last line.
        let mut at = floor;
        for word in sung.iter_mut() {
            word.start = word.start.max(at).min(duration);
            word.end = word.end.max(word.start + MIN_WORD_SECS).min(duration.max(word.start));
            at = word.start + MIN_WORD_SECS;
        }
        for k in 0..sung.len() {
            if let Some(next) = sung.get(k + 1).map(|word| word.start) {
                sung[k].end = sung[k].end.min(next).max(sung[k].start);
            }
        }
        for word in sung.iter_mut() {
            let pieces = crate::syllables::split(&word.w);
            if pieces.len() > 1 {
                let starts = crate::syllables::place(word.start, word.end, &pieces, &analysis.onsets);
                if starts.len() > 1 {
                    word.syl = starts.iter().enumerate().map(|(i, s)| (*s, starts.get(i + 1).copied().unwrap_or(word.end))).collect();
                }
            }
        }
        let starts: Vec<f64> = sung.iter().map(|word| word.start).collect();
        floor = sung.last().map_or(floor, |word| word.end);
        out.lines.push(SungLine { block: line.block, agreement: agreement_of(&starts), matched: matched.len(), forced: how == How::Aligned, words: sung });
    }
    out
}

/// A line's word spans from the heard words it matched; each unmatched
/// word is spread evenly over the gap between its timed neighbours (or a
/// typical word length beyond the first/last timed one).
fn spread(known: Vec<Option<(f64, f64)>>, floor: f64) -> Vec<(f64, f64)> {
    let n = known.len();
    let mut out = vec![(0.0, 0.0); n];
    let mut i = 0;
    while i < n {
        if let Some(span) = known[i] {
            out[i] = span;
            i += 1;
            continue;
        }
        let run_start = i;
        while i < n && known[i].is_none() {
            i += 1;
        }
        let run = i - run_start;
        let before = run_start.checked_sub(1).and_then(|k| known[k]).map(|span| span.1);
        let after = known.get(i).copied().flatten().map(|span| span.0);
        let (from, to) = match (before, after) {
            (Some(b), Some(a)) if a > b => (b, a),
            (Some(b), _) => (b, b + run as f64 * WORD_SECS),
            (None, Some(a)) => ((a - run as f64 * WORD_SECS).max(floor), a),
            (None, None) => (floor, floor + run as f64 * WORD_SECS),
        };
        let step = (to - from) / run as f64;
        for k in 0..run {
            out[run_start + k] = (from + step * k as f64, from + step * (k + 1) as f64);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sheet_reads_its_tags_and_lines() {
        let sheet = Sheet::parse("[Verse 1]\nI run   through the night\n\n[Pre-Chorus]\nhold on\n[Chorus]\nwe fly\nwe fall\n");
        assert_eq!(sheet.blocks.len(), 3);
        assert_eq!(sheet.blocks[0].tag, "verse");
        assert_eq!(sheet.blocks[0].lines, vec!["I run through the night"]);
        assert_eq!(sheet.blocks[1].tag, "pre");
        assert_eq!(sheet.blocks[2].lines.len(), 2);
        assert_eq!(sheet.words(), 11);
    }

    #[test]
    fn sheet_words_match_heard_words_through_mishearing_and_skips() {
        let sheet: Vec<String> = "don't let the toy wake every lake is high tonight".split(' ').map(fold).collect();
        let heard: Vec<String> = "uh don't let the wake wake every wake is high tonight yeah".split(' ').map(fold).collect();
        let map = match_words(&sheet, &heard);
        assert_eq!(map[0], Some(1));
        assert_eq!(map[2], Some(3));
        assert_eq!(map[4], Some(5)); // "wake" sung where it is
        assert_eq!(map[9], Some(10));
        assert_eq!(map[6], Some(7)); // "lake" heard as "wake": a near spelling, sung there
        // Near spellings pair: "rivers" heard for "river".
        let map = match_words(&["river".into()], &["rivers".into()]);
        assert_eq!(map, vec![Some(0)]);
    }

    #[test]
    fn unmatched_words_spread_between_their_neighbours() {
        let spans = spread(vec![Some((1.0, 1.2)), None, None, Some((2.0, 2.3))], 0.0);
        assert!((spans[1].0 - 1.2).abs() < 1e-9 && (spans[2].0 - 1.6).abs() < 1e-9 && (spans[2].1 - 2.0).abs() < 1e-9, "{spans:?}");
        let spans = spread(vec![None, Some((5.0, 5.4))], 0.0);
        assert!((spans[0].0 - (5.0 - WORD_SECS)).abs() < 1e-9);
    }
}
