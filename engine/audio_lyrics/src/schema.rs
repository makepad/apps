//! The lyrics document: timed lines, per-word start times, per-line
//! confidence — one JSON shape for the VJ's on-disk cache AND the `Lyrics`
//! side-channel file on an audio asset.
//!
//! ```json
//! {"format":"vj-lyrics","version":4,"digest":"<64 hex of track_digest>",
//!  "backend":"whisper","model":"ggml-large-v3-turbo.bin","language":"en",
//!  "duration_secs":231.44,
//!  "onset_snapped":12,"onset_mean_ms":83.5,"onset_max_ms":412.0,
//!  "lines":[{"t0":12.34,"t1":15.02,"text":"You can dance",
//!            "w":[12.34,12.9,13.4],"c":true}]}
//! ```
//!
//! * `t0`/`t1` — line start/end in track seconds (3 decimal places).
//! * `text` — the display line; `w` holds one start time per whitespace
//!   word of `text` (may be absent).
//! * `c` — the producer's word-level confidence: only a `true` line may be
//!   hopped word-by-word; `false` renders as a smooth line sweep. Word
//!   times and the right to use them are separate on purpose.
//! * `digest` — [`crate::track_digest`] of the decoded PCM, so a reader can
//!   check the document is about the audio it decoded. Readers of the
//!   server side-channel may skip the digest check (the store's content
//!   addressing already binds file to revision) by passing the document's
//!   own digest back in; the VJ cache reader must not.
//! * `onset_*` — audit stats of the bake, carried so caches can be judged
//!   without re-running the model.
//!
//! A document from another version, format or digest parses to `None`, not
//! an error: it simply is not this track's lyrics, and the caller re-bakes.

use makepad_asset_client::json::{self, Value};

/// Format tag of the document.
pub const LYRICS_FORMAT: &str = "vj-lyrics";
/// Version. Bumping it re-bakes every cached track: v1 was whole whisper
/// segments, v2 cut phrases on the vocal stem's silences, v3 added per-word
/// starts, v4 replaced envelope-guessed times with measured ones
/// (cross-attention DTW + teacher forcing + onset snapping in
/// [`crate::align`]).
pub const LYRICS_VERSION: u32 = 4;

#[derive(Clone, Debug, PartialEq)]
pub struct LyricLine {
    pub start_secs: f64,
    pub end_secs: f64,
    pub text: String,
    /// One start time per whitespace word of `text`.
    pub words: Vec<f64>,
    /// Per word of `words`, its sung syllables' start times (the first is
    /// the word's start; empty: not known, the word is one syllable). JSON
    /// `s`, additive: a reader that does not know it reads the line as
    /// before.
    pub syllables: Vec<Vec<f64>>,
    /// Whether those word times are good enough to HOP through.
    ///
    /// A fill that hops crisply onto the wrong word is worse than one that
    /// sweeps and never claimed to know. False (or no times at all) renders
    /// as the linear sweep.
    pub confident: bool,
}

impl LyricLine {
    pub fn new(start_secs: f64, end_secs: f64, text: impl Into<String>) -> LyricLine {
        LyricLine { start_secs, end_secs, text: text.into(), words: Vec::new(), syllables: Vec::new(), confident: false }
    }

    /// True when the display should hop word by word rather than sweep.
    pub fn hops(&self) -> bool {
        self.confident && self.words.len() >= 2
    }
}

/// What the onset refinement actually moved, kept so a document can be
/// audited without re-running the model.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct OnsetStats {
    pub snapped: usize,
    pub mean_ms: f64,
    pub max_ms: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackLyrics {
    pub backend: String,
    pub model: String,
    pub language: String,
    pub duration_secs: f64,
    pub onset: OnsetStats,
    pub lines: Vec<LyricLine>,
}

impl TrackLyrics {
    pub fn to_json(&self, digest: &str) -> String {
        let lines: Vec<Value> = self
            .lines
            .iter()
            .map(|line| {
                let mut fields = vec![
                    ("t0", Value::F64(round_ms(line.start_secs))),
                    ("t1", Value::F64(round_ms(line.end_secs))),
                    ("text", json::s(line.text.clone())),
                    (
                        "w",
                        Value::Arr(
                            line.words.iter().map(|at| Value::F64(round_ms(*at))).collect(),
                        ),
                    ),
                    ("c", Value::Bool(line.confident)),
                ];
                // `s`: one list of syllable starts per word, when a word
                // has more than one.
                if line.syllables.len() == line.words.len() && line.syllables.iter().any(|s| s.len() > 1) {
                    let words = line.syllables.iter().map(|starts| Value::Arr(starts.iter().map(|at| Value::F64(round_ms(*at))).collect()));
                    fields.push(("s", Value::Arr(words.collect())));
                }
                json::obj(fields)
            })
            .collect();
        json::obj(vec![
            ("format", json::s(LYRICS_FORMAT)),
            ("version", Value::Int(LYRICS_VERSION as i64)),
            ("digest", json::s(digest)),
            ("backend", json::s(self.backend.clone())),
            ("model", json::s(self.model.clone())),
            ("language", json::s(self.language.clone())),
            ("duration_secs", Value::F64(round_ms(self.duration_secs))),
            ("onset_snapped", Value::Int(self.onset.snapped as i64)),
            ("onset_mean_ms", Value::F64(round_ms(self.onset.mean_ms))),
            ("onset_max_ms", Value::F64(round_ms(self.onset.max_ms))),
            ("lines", Value::Arr(lines)),
        ])
        .to_json()
    }

    /// Parse a document. Another version, format or digest is not an error —
    /// it simply is not this track's lyrics, and the caller re-bakes.
    pub fn from_json(bytes: &[u8], digest: &str) -> Option<TrackLyrics> {
        let value = json::parse(bytes).ok()?;
        if value.get("format")?.as_str()? != LYRICS_FORMAT {
            return None;
        }
        // Version 5 (voiceover takes) adds word ends (`e`), which karaoke
        // does not use; its starts read the same as version 4's.
        let version = value.get("version")?.as_i64()?;
        if version != LYRICS_VERSION as i64 && version != 5 {
            return None;
        }
        if value.get("digest")?.as_str()? != digest {
            return None;
        }
        let mut lines = Vec::new();
        for item in value.get("lines")?.as_arr()? {
            let start = number(item.get("t0")?)?;
            let end = number(item.get("t1")?)?;
            let text = item.get("text")?.as_str()?.to_string();
            if !start.is_finite() || !end.is_finite() || end < start {
                return None;
            }
            let mut words = Vec::new();
            if let Some(list) = item.get("w").and_then(|v| v.as_arr()) {
                for at in list {
                    words.push(number(at)?);
                }
            }
            let confident = item.get("c").and_then(|v| v.as_bool()).unwrap_or(false);
            let syllables = syllables_of(item, words.len());
            lines.push(LyricLine { start_secs: start, end_secs: end, text, words, syllables, confident });
        }
        Some(TrackLyrics {
            backend: value.get("backend")?.as_str()?.to_string(),
            model: value.get("model")?.as_str()?.to_string(),
            language: value.get("language")?.as_str()?.to_string(),
            duration_secs: value.get("duration_secs").and_then(number).unwrap_or(0.0),
            onset: OnsetStats {
                snapped: value
                    .get("onset_snapped")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0)
                    .max(0) as usize,
                mean_ms: value.get("onset_mean_ms").and_then(number).unwrap_or(0.0),
                max_ms: value.get("onset_max_ms").and_then(number).unwrap_or(0.0),
            },
            lines,
        })
    }

    /// The document's own digest field, for consumers that trust the
    /// container's content addressing instead of re-deriving the PCM digest.
    pub fn digest_of(bytes: &[u8]) -> Option<String> {
        let value = json::parse(bytes).ok()?;
        Some(value.get("digest")?.as_str()?.to_string())
    }
}

/// A line's `s` (per word, syllable starts), when it has one list per
/// word of numbers; otherwise none.
fn syllables_of(line: &Value, words: usize) -> Vec<Vec<f64>> {
    let Some(list) = line.get("s").and_then(|v| v.as_arr()) else { return Vec::new() };
    let out: Option<Vec<Vec<f64>>> = list.iter().map(|word| word.as_arr().and_then(|starts| starts.iter().map(number).collect())).collect();
    match out {
        Some(out) if out.len() == words => out,
        _ => Vec::new(),
    }
}

/// Times travel at millisecond precision: enough for a 20 ms alignment
/// grid, and it keeps the JSON stable across float formatting.
fn round_ms(secs: f64) -> f64 {
    (secs * 1000.0).round() / 1000.0
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::F64(v) => Some(*v),
        Value::Int(v) => Some(*v as f64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> TrackLyrics {
        TrackLyrics {
            backend: "whisper".into(),
            model: "ggml-large-v3-turbo.bin".into(),
            language: "en".into(),
            duration_secs: 231.4444,
            onset: OnsetStats { snapped: 12, mean_ms: 83.5, max_ms: 412.0 },
            lines: vec![
                LyricLine {
                    start_secs: 12.34,
                    end_secs: 15.02,
                    text: "You can dancing".into(),
                    words: vec![12.34, 12.9, 13.4],
                    syllables: vec![vec![12.34], vec![12.9], vec![13.4, 13.75]],
                    confident: true,
                },
                LyricLine::new(16.0, 18.5, "if you want to"),
            ],
        }
    }

    #[test]
    fn round_trips_with_digest() {
        let d = doc();
        let json = d.to_json("abc123");
        let back = TrackLyrics::from_json(json.as_bytes(), "abc123").unwrap();
        assert_eq!(back.lines.len(), 2);
        assert!(back.lines[0].hops());
        assert!(!back.lines[1].hops());
        assert_eq!(back.lines[0].words.len(), 3);
        assert_eq!(back.backend, "whisper");
        assert_eq!(TrackLyrics::digest_of(json.as_bytes()).as_deref(), Some("abc123"));
        // Syllables travel as `s`, one list per word, and only when a
        // word has more than one.
        assert_eq!(back.lines[0].syllables, d.lines[0].syllables);
        assert!(json.contains(r#""s":[[12.34],[12.9],[13.4,13.75]]"#), "{json}");
        assert!(back.lines[1].syllables.is_empty());
        // A document without `s` (every one baked before) reads as before.
        let old = json.replace(r#","s":[[12.34],[12.9],[13.4,13.75]]"#, "");
        let back = TrackLyrics::from_json(old.as_bytes(), "abc123").unwrap();
        assert!(back.lines[0].syllables.is_empty() && back.lines[0].words.len() == 3);
    }

    #[test]
    fn wrong_digest_version_or_format_is_none_not_error() {
        let json = doc().to_json("abc123");
        assert!(TrackLyrics::from_json(json.as_bytes(), "zzz").is_none());
        let old = json.replace("\"version\":4", "\"version\":3");
        assert!(TrackLyrics::from_json(old.as_bytes(), "abc123").is_none());
        let alien = json.replace("vj-lyrics", "other");
        assert!(TrackLyrics::from_json(alien.as_bytes(), "abc123").is_none());
        assert!(TrackLyrics::from_json(b"not json", "abc123").is_none());
    }

    #[test]
    fn a_backwards_line_refuses_the_document() {
        let mut d = doc();
        d.lines[0].end_secs = 1.0;
        let json = d.to_json("abc123");
        assert!(TrackLyrics::from_json(json.as_bytes(), "abc123").is_none());
    }
}
