//! The `vj-lyrics` word timing document, version 5: version 4 (the karaoke
//! cache and the `Lyrics` side-channel, `makepad-audio-lyrics` `schema.rs`)
//! plus every word's END. A voice take stores its word times in it, and a
//! transcript is one.
//!
//! ```json
//! {"format":"vj-lyrics","version":5,"digest":"<64 hex>",
//!  "backend":"kokoro","model":"bm_daniel","language":"en",
//!  "duration_secs":2.4,"timing":"tts",
//!  "lines":[{"t0":0.12,"t1":2.3,"text":"Notes starts with one tap.",
//!            "w":[0.12,0.5,0.9,1.1,1.5],"e":[0.45,0.85,1.05,1.4,2.3],"c":true}]}
//! ```
//!
//! * `w` and `e` hold one start and one end per whitespace word of `text`
//!   (both absent when the line has no word times).
//! * `timing` (optional) says where the word times came from: `tts`,
//!   `aligned`, `envelope` or `whisper`.
//! * `phones` (optional): a take's phonemes, `[["m", 0.12, 0.18], …]`
//!   (IPA, start, end), when the engine timed them (Kokoro): what lip sync
//!   reads.
//! * Version 4 documents are read too: a word ends where the next starts,
//!   the last where its line ends.

use crate::timing::TimingSource;
use makepad_story_cut::subtitle::estimate_words;
use makepad_story_cut::{SpokenLine, TakeInfo, Word};
use makepad_strict_json::{self as json, Value};

pub const LYRICS_FORMAT: &str = "vj-lyrics";
pub const LYRICS_VERSION: u32 = 5;

#[derive(Clone, Debug, PartialEq)]
pub struct TranscriptLine {
    pub start: f64,
    pub end: f64,
    pub text: String,
    /// One per whitespace word of `text`, or none when the line has no
    /// word times.
    pub words: Vec<Word>,
    /// Whether the word times are good enough to hop through word by word.
    pub confident: bool,
}

/// Timed words: a take's (one line, seconds from the take's start) or a
/// source's transcript (seconds of the source).
#[derive(Clone, Debug, PartialEq)]
pub struct Transcript {
    pub backend: String,
    pub model: String,
    pub language: String,
    pub duration_secs: f64,
    /// Where the word times came from (`tts`, `aligned`, `envelope`,
    /// `whisper`); empty when unknown.
    pub timing: String,
    pub lines: Vec<TranscriptLine>,
    /// A take's timed phonemes (text: IPA), when the engine gave them.
    pub phones: Vec<Word>,
}

impl Transcript {
    /// A take's words as a one-line document.
    pub fn of_take(text: &str, words: Vec<Word>, duration_secs: f64, source: TimingSource, backend: &str, model: &str, language: &str) -> Transcript {
        let start = words.first().map_or(0.0, |word| word.start);
        let end = words.last().map_or(duration_secs, |word| word.end);
        Transcript {
            backend: backend.into(),
            model: model.into(),
            language: language.into(),
            duration_secs,
            timing: source.id().into(),
            lines: vec![TranscriptLine { start, end, text: normalize(text), confident: source != TimingSource::Envelope, words }],
            phones: Vec::new(),
        }
    }

    /// What `CutEval::set_take` needs of a take's document.
    pub fn take_info(&self) -> TakeInfo {
        TakeInfo { duration: self.duration_secs, words: self.lines.iter().flat_map(|line| line.words.iter().cloned()).collect(), phones: self.phones.clone() }
    }

    /// What `CutEval::set_transcript` needs: every line with word times;
    /// a line without them gets estimated ones over its span.
    pub fn spoken_lines(&self) -> Vec<SpokenLine> {
        self.lines.iter().filter_map(|line| {
            let words = if line.words.is_empty() { estimate_words(&line.text, line.start, line.end) } else { line.words.clone() };
            (!words.is_empty()).then(|| SpokenLine { start: line.start, end: line.end, words })
        }).collect()
    }

    pub fn to_json(&self, digest: &str) -> String {
        let lines = self.lines.iter().map(|line| {
            let mut fields = vec![("t0", Value::F64(round_ms(line.start))), ("t1", Value::F64(round_ms(line.end))), ("text", json::s(line.text.clone()))];
            if !line.words.is_empty() {
                fields.push(("w", Value::Arr(line.words.iter().map(|word| Value::F64(round_ms(word.start))).collect())));
                fields.push(("e", Value::Arr(line.words.iter().map(|word| Value::F64(round_ms(word.end))).collect())));
            }
            fields.push(("c", Value::Bool(line.confident)));
            json::obj(fields)
        }).collect();
        let mut fields = vec![
            ("format", json::s(LYRICS_FORMAT)),
            ("version", Value::Int(LYRICS_VERSION as i64)),
            ("digest", json::s(digest)),
            ("backend", json::s(self.backend.clone())),
            ("model", json::s(self.model.clone())),
            ("language", json::s(self.language.clone())),
            ("duration_secs", Value::F64(round_ms(self.duration_secs))),
        ];
        if !self.timing.is_empty() {
            fields.push(("timing", json::s(self.timing.clone())));
        }
        fields.push(("lines", Value::Arr(lines)));
        if !self.phones.is_empty() {
            fields.push(("phones", Value::Arr(self.phones.iter().map(|p| Value::Arr(vec![json::s(p.text.clone()), Value::F64(round_ms(p.start)), Value::F64(round_ms(p.end))])).collect())));
        }
        json::obj(fields).to_json()
    }

    /// Read a version 5 or 4 document. With `digest`, a document about
    /// other audio is `None`; without, the container vouches for it. A
    /// line whose word lists do not match its words keeps its text and
    /// loses its times.
    pub fn from_json(bytes: &[u8], digest: Option<&str>) -> Option<Transcript> {
        let value = json::parse(bytes).ok()?;
        if value.get("format")?.as_str()? != LYRICS_FORMAT {
            return None;
        }
        let version = value.get("version")?.as_i64()?;
        if version != 4 && version != 5 {
            return None;
        }
        if let Some(digest) = digest {
            if value.get("digest")?.as_str()? != digest {
                return None;
            }
        }
        let mut lines = Vec::new();
        for item in value.get("lines")?.as_arr()? {
            let start = number(item.get("t0")?)?;
            let end = number(item.get("t1")?)?;
            let text = item.get("text")?.as_str()?.to_string();
            if !start.is_finite() || !end.is_finite() || end < start {
                return None;
            }
            let list = |key: &str| -> Option<Vec<f64>> { item.get(key)?.as_arr()?.iter().map(number).collect() };
            let texts = text.split_whitespace().collect::<Vec<_>>();
            let starts = list("w").unwrap_or_default();
            let ends = match (version, list("e")) {
                (5, Some(ends)) => ends,
                // Version 4: each word lasts until the next one starts.
                _ => (0..starts.len()).map(|i| starts.get(i + 1).copied().unwrap_or(end)).collect(),
            };
            let words = if starts.len() == texts.len() && ends.len() == texts.len() {
                texts.iter().zip(starts.iter().zip(&ends)).map(|(text, (start, end))| Word { text: text.to_string(), start: *start, end: end.max(*start) }).collect()
            } else {
                Vec::new()
            };
            let confident = item.get("c").and_then(|v| v.as_bool()).unwrap_or(false);
            lines.push(TranscriptLine { start, end, text, words, confident });
        }
        Some(Transcript {
            backend: value.get("backend").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            model: value.get("model").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            language: value.get("language").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            duration_secs: value.get("duration_secs").and_then(number).unwrap_or(0.0),
            timing: value.get("timing").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            lines,
            // Optional; a malformed entry drops the phones, not the document.
            phones: value.get("phones").and_then(|v| v.as_arr()).and_then(|items| {
                items.iter().map(|item| match item.as_arr()? {
                    [text, start, end] => Some(Word { text: text.as_str()?.to_string(), start: number(start)?, end: number(end)? }),
                    _ => None,
                }).collect::<Option<Vec<Word>>>()
            }).unwrap_or_default(),
        })
    }
}

fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Times travel at millisecond precision, as in version 4.
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

    fn word(text: &str, start: f64, end: f64) -> Word {
        Word { text: text.into(), start, end }
    }

    #[test]
    fn version_5_round_trips_word_ends() {
        let take = Transcript::of_take(" That's  it. ", vec![word("That's", 0.1, 0.4), word("it.", 0.45, 0.8123)], 1.0, TimingSource::Tts, "kokoro", "bm_daniel", "en");
        let json = take.to_json("d1");
        assert_eq!(
            json,
            r#"{"format":"vj-lyrics","version":5,"digest":"d1","backend":"kokoro","model":"bm_daniel","language":"en","duration_secs":1.0,"timing":"tts","lines":[{"t0":0.1,"t1":0.812,"text":"That's it.","w":[0.1,0.45],"e":[0.4,0.812],"c":true}]}"#
        );
        let back = Transcript::from_json(json.as_bytes(), Some("d1")).unwrap();
        assert_eq!(back.lines[0].words, [word("That's", 0.1, 0.4), word("it.", 0.45, 0.812)]);
        assert_eq!(back.timing, "tts");
        assert!(Transcript::from_json(json.as_bytes(), Some("other")).is_none());
        assert!(Transcript::from_json(json.as_bytes(), None).is_some(), "the container vouches");
        assert_eq!(back.take_info().words.len(), 2);
        assert_eq!(back.take_info().duration, 1.0);
    }

    #[test]
    fn version_4_is_read_with_ends_from_the_next_start() {
        // The shape makepad-audio-lyrics writes (schema.rs).
        let v4 = r#"{"format":"vj-lyrics","version":4,"digest":"abc","backend":"whisper","model":"ggml-large-v3-turbo.bin","language":"en","duration_secs":20.0,"onset_snapped":1,"onset_mean_ms":3.0,"onset_max_ms":3.0,"lines":[{"t0":12.34,"t1":15.02,"text":"You can dance","w":[12.34,12.9,13.4],"c":true},{"t0":16.0,"t1":18.5,"text":"if you want to","w":[],"c":false}]}"#;
        let doc = Transcript::from_json(v4.as_bytes(), Some("abc")).unwrap();
        assert_eq!(doc.lines[0].words, [word("You", 12.34, 12.9), word("can", 12.9, 13.4), word("dance", 13.4, 15.02)]);
        assert!(doc.lines[1].words.is_empty() && doc.timing.is_empty());
        // A line without word times is estimated for captions.
        let spoken = doc.spoken_lines();
        assert_eq!(spoken.len(), 2);
        assert_eq!(spoken[1].words.len(), 4);
        assert!((spoken[1].words[0].start - 16.0).abs() < 1e-9 && (spoken[1].words[3].end - 18.5).abs() < 1e-9);
        assert!(Transcript::from_json(v4.replace("\"version\":4", "\"version\":3").as_bytes(), None).is_none());
        assert!(Transcript::from_json(v4.replace("vj-lyrics", "other").as_bytes(), None).is_none());
    }

    #[test]
    fn mismatched_word_lists_keep_the_text() {
        let doc = r#"{"format":"vj-lyrics","version":5,"digest":"x","lines":[{"t0":0,"t1":1,"text":"a b","w":[0.0],"e":[0.5],"c":true}]}"#;
        let doc = Transcript::from_json(doc.as_bytes(), None).unwrap();
        assert!(doc.lines[0].words.is_empty() && doc.lines[0].text == "a b");
    }

    #[test]
    fn a_take_keeps_its_phones() {
        let mut take = Transcript::of_take("mm", vec![word("mm", 0.1, 0.4)], 1.0, TimingSource::Tts, "kokoro", "af_heart", "en");
        take.phones = vec![word("m", 0.1, 0.25), word("ə", 0.25, 0.4)];
        let json = take.to_json("d");
        assert!(json.contains(r#""phones":[["m",0.1,0.25],["ə",0.25,0.4]]"#), "{json}");
        let back = Transcript::from_json(json.as_bytes(), Some("d")).unwrap();
        assert_eq!(back.phones, take.phones);
        assert_eq!(back.take_info().phones, take.phones);
    }
}
