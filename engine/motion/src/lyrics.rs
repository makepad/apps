//! Sung or spoken words for vocal-locked type: a `vj-lyrics` document
//! (audio_lyrics' aligned lyrics, version 4 or 5) read as a voice, and the
//! one resolver every word-timed layer shares (`voice_words`: Text's
//! `source: voice(@vo) line: i`, T3's Text3D karaoke).

use crate::features::{FeatureWord, VoiceFeatures};
use crate::model::MotionContext;
use makepad_strict_json::{self as json, Value};

fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::F64(f) => Some(*f),
        _ => None,
    }
}

/// A `vj-lyrics` document (`{"format":"vj-lyrics","version":4|5,"lines":[{"t0","t1","text","w":[starts],"e":[ends]?,"s":[[syllable starts]…]?,"c"}]}`)
/// as a voice: one line per lyric line, one word per whitespace word of
/// its text. A word ends at its `e` (version 5), else at the next word's
/// start, else at the line's end. A line without word times spreads its
/// words evenly over the line (it was not confident enough to hop). Times
/// are track seconds, moved by `offset` (where the track starts on the
/// clock the words are read on).
pub fn voice_from_lyrics(text: &str, offset: f64) -> Result<VoiceFeatures, String> {
    let root = json::parse(text.as_bytes()).map_err(|e| format!("lyrics JSON: {e}"))?;
    if root.get("format").and_then(Value::as_str) != Some("vj-lyrics") {
        return Err("lyrics JSON: not a `vj-lyrics` document (its `format` must be \"vj-lyrics\")".into());
    }
    match root.get("version").and_then(Value::as_i64) {
        Some(4) | Some(5) => {}
        other => return Err(format!("lyrics JSON: version {other:?}; this reads versions 4 and 5")),
    }
    let lines = root.get("lines").and_then(Value::as_arr).ok_or("lyrics JSON: no `lines`")?;
    let mut voice = VoiceFeatures::default();
    for (index, line) in lines.iter().enumerate() {
        let t0 = line.get("t0").and_then(number).ok_or_else(|| format!("lyrics JSON: line {index} has no `t0`"))?;
        let t1 = line.get("t1").and_then(number).ok_or_else(|| format!("lyrics JSON: line {index} has no `t1`"))?;
        let text = line.get("text").and_then(Value::as_str).unwrap_or("");
        let words: Vec<&str> = text.split_whitespace().collect();
        if words.is_empty() {
            continue;
        }
        let list = |key: &str| -> Vec<f64> { line.get(key).and_then(Value::as_arr).map(|a| a.iter().filter_map(number).collect()).unwrap_or_default() };
        let (starts, ends) = (list("w"), list("e"));
        // `s`: per word, its sung syllables' starts (the aligner's; a word
        // without them is one syllable).
        let syllable_starts: Vec<Vec<f64>> = line.get("s").and_then(Value::as_arr).map(|words| {
            words.iter().map(|w| w.as_arr().map(|a| a.iter().filter_map(number).collect()).unwrap_or_default()).collect()
        }).unwrap_or_default();
        let even = starts.len() != words.len();
        let n = words.len() as f64;
        let line_index = voice.lines.len() as u32;
        for (k, word) in words.iter().enumerate() {
            let (start, end) = if even {
                (t0 + (t1 - t0) * k as f64 / n, t0 + (t1 - t0) * (k + 1) as f64 / n)
            } else {
                let start = starts[k];
                let end = ends.get(k).copied().or_else(|| starts.get(k + 1).copied()).unwrap_or(t1).max(start);
                (start, end)
            };
            let syllables = match syllable_starts.get(k) {
                Some(s) if !even && s.len() > 1 && s.windows(2).all(|p| p[0] < p[1]) && s[0] >= start - 1e-6 && *s.last().unwrap() < end => {
                    s.iter().enumerate().map(|(i, a)| (a + offset, s.get(i + 1).copied().unwrap_or(end) + offset)).collect()
                }
                _ => Vec::new(),
            };
            voice.words.push(FeatureWord { text: word.to_string(), start: start + offset, end: end + offset, line: line_index, syllables, ..Default::default() });
        }
        voice.lines.push((t0 + offset, t1 + offset));
    }
    Ok(voice)
}

/// The word times of voice `voice` (optionally only line `line`) on the
/// document's clock: `(text, start, end)`. The one resolver for every
/// word-timed layer; an error says what is missing (no such voice, no
/// such line), for the author.
pub fn voice_words(context: &MotionContext, voice: &str, line: Option<u32>) -> Result<Vec<(String, f64, f64)>, String> {
    let features = context.signals.voice(voice).ok_or_else(|| format!("no voice `@{voice}` in this context (an edit's VoiceOver node id, a music clip with lyrics, or `--audio`/`--lyrics` in the CLI)"))?;
    let words: Vec<(String, f64, f64)> = features
        .words
        .iter()
        .filter(|w| line.is_none_or(|l| w.line == l))
        .map(|w| (w.text.clone(), w.start - context.offset, w.end - context.offset))
        .collect();
    if words.is_empty() {
        return Err(match line {
            Some(l) => format!("voice `@{voice}` has no line {l} ({} lines, from 0)", features.lines.len()),
            None => format!("voice `@{voice}` has no words"),
        });
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lyrics_read_as_a_voice() {
        let doc = r#"{"format":"vj-lyrics","version":5,"digest":"x","backend":"w","model":"m","language":"en","lines":[
            {"t0":10.0,"t1":12.0,"text":"A red thread","w":[10.0,10.4,11.1],"e":[10.3,11.0,11.8],"c":true},
            {"t0":12.5,"t1":14.5,"text":"of sun","c":false}]}"#;
        let v = voice_from_lyrics(doc, 2.0).unwrap();
        assert_eq!(v.lines, vec![(12.0, 14.0), (14.5, 16.5)]);
        let w: Vec<(&str, f64, f64, u32)> = v.words.iter().map(|w| (w.text.as_str(), w.start, w.end, w.line)).collect();
        assert_eq!(w[0], ("A", 12.0, 12.3, 0));
        assert_eq!(w[2], ("thread", 13.1, 13.8, 0));
        // An unconfident line without times spreads evenly.
        assert_eq!(w[3], ("of", 14.5, 15.5, 1));
        assert_eq!(w[4], ("sun", 15.5, 16.5, 1));
        // Version 4: ends from the next start, then the line's end.
        let v4 = r#"{"format":"vj-lyrics","version":4,"lines":[{"t0":0,"t1":2,"text":"a b","w":[0.5,1.0]}]}"#;
        let v = voice_from_lyrics(v4, 0.0).unwrap();
        assert_eq!((v.words[0].end, v.words[1].end), (1.0, 2.0));
        assert!(voice_from_lyrics(r#"{"format":"other","version":4,"lines":[]}"#, 0.0).is_err());
        // `s`: a word's sung syllables, to its end; a one-syllable word or
        // an unusable list has none (readers take the word as one).
        let s = r#"{"format":"vj-lyrics","version":4,"lines":[{"t0":0,"t1":3,"text":"harbor light","w":[0.5,1.5],"s":[[0.5,0.9],[1.5]],"c":true},{"t0":3,"t1":4,"text":"bad","w":[3.0],"s":[[3.5,3.2]]}]}"#;
        let v = voice_from_lyrics(s, 10.0).unwrap();
        assert_eq!(v.words[0].syllables, vec![(10.5, 10.9), (10.9, 11.5)]);
        assert!(v.words[1].syllables.is_empty() && v.words[2].syllables.is_empty());
    }
}
