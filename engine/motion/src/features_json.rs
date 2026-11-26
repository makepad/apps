//! [`AudioFeatures`] as JSON (lane CH1): what `makepad-motion --audio
//! <file.json>` reads, so a document can be rendered against a real
//! voiceover or song (a Kokoro take's words and envelope, a vocal stem's
//! aligned lyrics) outside Stage.
//!
//! ```text
//! {"version": 1,
//!  "voices": [{"id": "vo",
//!              "words": [{"text": "yo", "start": 0.4, "end": 0.62, "emphasis": 0.8, "line": 0, "syllables": [[0.4, 0.62]]}],
//!              "lines": [[0.4, 3.1]],
//!              "amplitude": {"start": 0, "rate": 100, "values": [0.0, 0.1, …]},
//!              "pitch": {…}, "pauses": [[1.2, 1.5]], "phones": [["j", 0.4, 0.46], ["oʊ", 0.46, 0.62]]}],
//!  "music": [{"id": "m", "bpm": 92, "beats": [0.0, 0.652, …], "beats_per_bar": 4, "downbeat": 0,
//!             "onsets": [{"t": 0.0, "strength": 1.0}], "energy": {…}, "bands": [{…}],
//!             "sections": [{"name": "intro", "start": 0, "end": 8}]}]}
//! ```
//!
//! Reading is strict: unknown keys and wrong types are errors that name
//! the key. Optional: a word's `emphasis`, `line`, `syllables`; a voice's
//! `pitch`, `pauses`, `phones`; everything of a music track but `id`, `bpm` and `beats`.

use crate::features::{AudioFeatures, FeatureCurve, FeaturePhone, FeatureWord, MusicFeatures, VoiceFeatures};
use crate::timing::{Onset, Section};
use makepad_strict_json::{self as json, Value};

pub const VERSION: i64 = 1;

fn num(v: f64) -> Value {
    Value::F64((v * 10000.0).round() / 10000.0)
}

fn curve_json(c: &FeatureCurve) -> Value {
    json::obj(vec![("start", num(c.start)), ("rate", num(c.rate)), ("values", Value::Arr(c.values.iter().map(|v| num(*v as f64)).collect()))])
}

fn spans_json(spans: &[(f64, f64)]) -> Value {
    Value::Arr(spans.iter().map(|(a, b)| Value::Arr(vec![num(*a), num(*b)])).collect())
}

/// An object without its empty curves (a curve with no samples is left out).
fn obj(pairs: Vec<(&str, Value)>) -> Value {
    json::obj(pairs.into_iter().filter(|(_, v)| !matches!(v, Value::Null)).collect())
}

fn opt_curve(c: &FeatureCurve) -> Value {
    if c.values.is_empty() { Value::Null } else { curve_json(c) }
}

impl AudioFeatures {
    pub fn to_json(&self) -> String {
        let voices = self.voices.iter().map(|(id, v)| {
            obj(vec![
                ("id", json::s(id.clone())),
                ("words", Value::Arr(v.words.iter().map(|w| json::obj(vec![
                    ("text", json::s(w.text.clone())),
                    ("start", num(w.start)),
                    ("end", num(w.end)),
                    ("emphasis", num(w.emphasis)),
                    ("line", Value::Int(w.line as i64)),
                    ("syllables", spans_json(&w.syllables)),
                ])).collect())),
                ("lines", spans_json(&v.lines)),
                ("amplitude", opt_curve(&v.amplitude)),
                ("pitch", opt_curve(&v.pitch)),
                ("pauses", spans_json(&v.pauses)),
                ("phones", if v.phones.is_empty() { Value::Null } else { Value::Arr(v.phones.iter().map(|p| Value::Arr(vec![json::s(p.text.clone()), num(p.start), num(p.end)])).collect()) }),
            ])
        }).collect();
        let music = self.music.iter().map(|(id, m)| {
            obj(vec![
                ("id", json::s(id.clone())),
                ("bpm", num(m.bpm)),
                ("beats", Value::Arr(m.beats.iter().map(|b| num(*b)).collect())),
                ("beats_per_bar", Value::Int(m.beats_per_bar as i64)),
                ("downbeat", Value::Int(m.downbeat as i64)),
                ("onsets", Value::Arr(m.onsets.iter().map(|o| json::obj(vec![("t", num(o.t)), ("strength", num(o.strength))])).collect())),
                ("energy", opt_curve(&m.energy)),
                ("bands", Value::Arr(m.bands.iter().map(curve_json).collect())),
                ("sections", Value::Arr(m.sections.iter().map(|s| json::obj(vec![("name", json::s(s.name.clone())), ("start", num(s.start)), ("end", num(s.end))])).collect())),
            ])
        }).collect();
        json::obj(vec![("version", Value::Int(VERSION)), ("voices", Value::Arr(voices)), ("music", Value::Arr(music))]).to_json()
    }

    pub fn from_json(text: &str) -> Result<AudioFeatures, String> {
        let root = json::parse(text.as_bytes()).map_err(|e| format!("features JSON: {e}"))?;
        let r = Reader { path: String::new() };
        r.keys(&root, &["version", "voices", "music"], &["version"])?;
        match root.get("version").and_then(Value::as_i64) {
            Some(VERSION) => {}
            _ => return Err(format!("features JSON: `version` is {VERSION}")),
        }
        let mut out = AudioFeatures::default();
        for (i, v) in r.arr(&root, "voices", true)?.iter().enumerate() {
            let r = r.at(&format!("voices[{i}]"));
            r.keys(v, &["id", "words", "lines", "amplitude", "pitch", "pauses", "phones"], &["id", "words", "lines"])?;
            let mut words = Vec::new();
            for (k, w) in r.arr(v, "words", false)?.iter().enumerate() {
                let r = r.at(&format!("words[{k}]"));
                r.keys(w, &["text", "start", "end", "emphasis", "line", "syllables"], &["text", "start", "end"])?;
                words.push(FeatureWord {
                    text: r.str(w, "text")?,
                    start: r.num(w, "start")?,
                    end: r.num(w, "end")?,
                    emphasis: r.opt_num(w, "emphasis")?.unwrap_or(0.5),
                    line: r.opt_num(w, "line")?.unwrap_or(0.0).max(0.0) as u32,
                    syllables: r.spans(w, "syllables", true)?,
                });
            }
            out.voices.push((r.str(v, "id")?, VoiceFeatures {
                words,
                lines: r.spans(v, "lines", false)?,
                amplitude: r.curve(v, "amplitude")?,
                pitch: r.curve(v, "pitch")?,
                pauses: r.spans(v, "pauses", true)?,
                phones: r.arr(v, "phones", true)?.iter().map(|p| match p.as_arr() {
                    Some([text, start, end]) => Ok(FeaturePhone {
                        text: text.as_str().ok_or_else(|| r.err("phones", "holds [\"phone\", start, end] triples"))?.to_string(),
                        start: r.at("phones").number(start)?,
                        end: r.at("phones").number(end)?,
                    }),
                    _ => Err(r.err("phones", "holds [\"phone\", start, end] triples")),
                }).collect::<Result<Vec<_>, String>>()?,
            }));
        }
        for (i, m) in r.arr(&root, "music", true)?.iter().enumerate() {
            let r = r.at(&format!("music[{i}]"));
            r.keys(m, &["id", "bpm", "beats", "beats_per_bar", "downbeat", "onsets", "energy", "bands", "sections"], &["id", "bpm", "beats"])?;
            let mut onsets = Vec::new();
            for (k, o) in r.arr(m, "onsets", true)?.iter().enumerate() {
                let r = r.at(&format!("onsets[{k}]"));
                r.keys(o, &["t", "strength"], &["t"])?;
                onsets.push(Onset { t: r.num(o, "t")?, strength: r.opt_num(o, "strength")?.unwrap_or(1.0), name: None });
            }
            let mut sections = Vec::new();
            for (k, s) in r.arr(m, "sections", true)?.iter().enumerate() {
                let r = r.at(&format!("sections[{k}]"));
                r.keys(s, &["name", "start", "end"], &["name", "start", "end"])?;
                sections.push(Section { name: r.str(s, "name")?, start: r.num(s, "start")?, end: r.num(s, "end")? });
            }
            let mut bands = Vec::new();
            for (k, b) in r.arr(m, "bands", true)?.iter().enumerate() {
                bands.push(r.at(&format!("bands[{k}]")).curve_value(b)?);
            }
            let beats: Vec<f64> = r.arr(m, "beats", false)?.iter().enumerate().map(|(k, b)| r.at(&format!("beats[{k}]")).number(b)).collect::<Result<_, _>>()?;
            if beats.windows(2).any(|w| w[1] <= w[0]) {
                return Err(format!("features JSON: {}.beats must increase", r.path));
            }
            out.music.push((r.str(m, "id")?, MusicFeatures {
                bpm: r.num(m, "bpm")?,
                beats,
                beats_per_bar: r.opt_num(m, "beats_per_bar")?.unwrap_or(4.0).max(1.0) as u32,
                downbeat: r.opt_num(m, "downbeat")?.unwrap_or(0.0).max(0.0) as u32,
                onsets,
                energy: r.curve(m, "energy")?,
                bands,
                sections,
                // The sound itself is not JSON: the host attaches it.
                signal: None,
            }));
        }
        Ok(out)
    }
}

struct Reader {
    path: String,
}

impl Reader {
    fn at(&self, key: &str) -> Reader {
        Reader { path: if self.path.is_empty() { key.to_string() } else { format!("{}.{key}", self.path) } }
    }

    fn err(&self, key: &str, what: &str) -> String {
        let at = if self.path.is_empty() { key.to_string() } else { format!("{}.{key}", self.path) };
        format!("features JSON: `{at}` {what}")
    }

    fn keys(&self, v: &Value, allowed: &[&str], required: &[&str]) -> Result<(), String> {
        let Value::Obj(pairs) = v else { return Err(format!("features JSON: `{}` is an object", if self.path.is_empty() { "(root)" } else { &self.path })) };
        for (k, _) in pairs {
            if !allowed.contains(&k.as_str()) {
                return Err(self.err(k, &format!("is not a key here (keys: {})", allowed.join(", "))));
            }
        }
        for k in required {
            if v.get(k).is_none() {
                return Err(self.err(k, "is missing"));
            }
        }
        Ok(())
    }

    fn number(&self, v: &Value) -> Result<f64, String> {
        match v {
            Value::Int(i) => Ok(*i as f64),
            Value::F64(f) => Ok(*f),
            _ => Err(format!("features JSON: `{}` is a number", self.path)),
        }
    }

    fn num(&self, v: &Value, key: &str) -> Result<f64, String> {
        self.opt_num(v, key)?.ok_or_else(|| self.err(key, "is missing"))
    }

    fn opt_num(&self, v: &Value, key: &str) -> Result<Option<f64>, String> {
        match v.get(key) {
            None => Ok(None),
            Some(Value::Int(i)) => Ok(Some(*i as f64)),
            Some(Value::F64(f)) => Ok(Some(*f)),
            Some(_) => Err(self.err(key, "is a number")),
        }
    }

    fn str(&self, v: &Value, key: &str) -> Result<String, String> {
        v.get(key).and_then(Value::as_str).map(str::to_string).ok_or_else(|| self.err(key, "is a string"))
    }

    fn arr<'v>(&self, v: &'v Value, key: &str, optional: bool) -> Result<&'v [Value], String> {
        match v.get(key) {
            None if optional => Ok(&[]),
            Some(value) => value.as_arr().ok_or_else(|| self.err(key, "is an array")),
            None => Err(self.err(key, "is missing")),
        }
    }

    fn spans(&self, v: &Value, key: &str, optional: bool) -> Result<Vec<(f64, f64)>, String> {
        self.arr(v, key, optional)?.iter().map(|pair| match pair.as_arr() {
            Some([a, b]) => Ok((self.at(key).number(a)?, self.at(key).number(b)?)),
            _ => Err(self.err(key, "is a list of [start, end] pairs")),
        }).collect()
    }

    fn curve(&self, v: &Value, key: &str) -> Result<FeatureCurve, String> {
        match v.get(key) {
            None => Ok(FeatureCurve::default()),
            Some(c) => self.at(key).curve_value(c),
        }
    }

    fn curve_value(&self, c: &Value) -> Result<FeatureCurve, String> {
        self.keys(c, &["start", "rate", "values"], &["rate", "values"])?;
        let rate = self.num(c, "rate")?;
        if !(rate > 0.0) {
            return Err(self.err("rate", "is positive (samples a second)"));
        }
        let values = self.arr(c, "values", false)?.iter().map(|x| self.at("values").number(x).map(|x| x as f32)).collect::<Result<Vec<f32>, String>>()?;
        Ok(FeatureCurve::new(self.opt_num(c, "start")?.unwrap_or(0.0), rate, values))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn features_round_trip_and_mistakes_name_the_key() {
        let a = AudioFeatures::synthetic("two words", 0.5, 2.0, 100.0, 3.0);
        let b = AudioFeatures::from_json(&a.to_json()).unwrap();
        assert_eq!(a.voices[0].1.words.len(), b.voices[0].1.words.len());
        assert!((a.voices[0].1.amplitude.sample(0.7) - b.voices[0].1.amplitude.sample(0.7)).abs() < 1e-3);
        assert_eq!(a.music[0].1.beats.len(), b.music[0].1.beats.len());
        assert_eq!(a.timing(None).grid.unwrap().beat_at(1.3), b.timing(None).grid.unwrap().beat_at(1.3));
        let e = AudioFeatures::from_json(r#"{"version": 1, "voices": [{"id": "vo", "words": [{"txt": "a", "start": 0, "end": 1}], "lines": []}]}"#).unwrap_err();
        assert!(e.contains("voices[0].words[0].txt"), "{e}");
        let e = AudioFeatures::from_json(r#"{"version": 1, "music": [{"id": "m", "bpm": 90, "beats": [1, 0.5]}]}"#).unwrap_err();
        assert!(e.contains("increase"), "{e}");
    }
}
