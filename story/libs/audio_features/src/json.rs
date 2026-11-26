//! Features as JSON: whole (the cache beside the asset) and summarised
//! (what a tool answers a model with).
//!
//! Per-frame series are stored as integers in thousandths (energy, bands,
//! envelope) or tenths (dB, Hz), which keeps a three-minute track under a
//! megabyte and loses nothing an animation can see.

use crate::music::{MusicFeatures, Onset, Section, SectionKind};
use crate::voice::{Phone, VoiceFeatures, WordFeature};
use makepad_strict_json::{self as json, Value};

pub const VERSION: i64 = 1;

fn num(v: f64) -> Value {
    Value::F64((v * 1000.0).round() / 1000.0)
}

fn scaled(series: &[f32], scale: f32) -> Value {
    Value::Arr(series.iter().map(|v| Value::Int((v * scale).round() as i64)).collect())
}

fn unscaled(value: Option<&Value>, scale: f32) -> Result<Vec<f32>, String> {
    let items = value.and_then(Value::as_arr).ok_or("a per-frame series is missing")?;
    items.iter().map(|v| float(v).map(|v| v as f32 / scale)).collect::<Option<Vec<f32>>>().ok_or_else(|| "a per-frame value is not a number".to_string())
}

fn float(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::F64(f) => Some(*f),
        _ => None,
    }
}

fn times(values: &[f64]) -> Value {
    Value::Arr(values.iter().map(|t| num(*t)).collect())
}

fn read_times(value: Option<&Value>) -> Result<Vec<f64>, String> {
    value.and_then(Value::as_arr).ok_or("a time list is missing")?.iter().map(float).collect::<Option<Vec<f64>>>().ok_or_else(|| "a time is not a number".into())
}

fn field(v: &Value, key: &str) -> Result<f64, String> {
    v.get(key).and_then(float).ok_or_else(|| format!("`{key}` is missing"))
}

fn section_json(s: &Section) -> Value {
    json::obj(vec![("kind", json::s(s.kind.name())), ("nth", Value::Int(s.nth as i64)), ("start", num(s.start)), ("end", num(s.end)), ("energy", num(s.energy as f64))])
}

pub fn music_to_json(m: &MusicFeatures) -> Value {
    let mut bands = Vec::new();
    for band in 0..6 {
        let column: Vec<f32> = m.bands.iter().map(|b| b[band]).collect();
        bands.push(scaled(&column, 1000.0));
    }
    json::obj(vec![
        ("version", Value::Int(VERSION)),
        ("rate", num(crate::RATE)),
        ("duration", num(m.duration)),
        ("bpm", num(m.bpm)),
        ("grid_confidence", num(m.grid_confidence as f64)),
        ("beats", times(&m.beats)),
        ("downbeats", times(&m.downbeats)),
        ("onsets", Value::Arr(m.onsets.iter().map(|o| Value::Arr(vec![num(o.t), num(o.strength as f64)])).collect())),
        ("sections", Value::Arr(m.sections.iter().map(section_json).collect())),
        ("loudness_db", scaled(&m.loudness_db, 10.0)),
        ("energy", scaled(&m.energy, 1000.0)),
        ("bands", Value::Arr(bands)),
    ])
}

pub fn music_from_json(text: &str) -> Result<MusicFeatures, String> {
    let v = json::parse_depth(text.as_bytes(), 8).map_err(|e| format!("music features: {e}"))?;
    if v.get("version").and_then(Value::as_i64) != Some(VERSION) {
        return Err("music features: another version".into());
    }
    let onsets = v
        .get("onsets")
        .and_then(Value::as_arr)
        .ok_or("`onsets` is missing")?
        .iter()
        .map(|o| {
            let pair = o.as_arr()?;
            Some(Onset { t: float(pair.first()?)?, strength: float(pair.get(1)?)? as f32 })
        })
        .collect::<Option<Vec<_>>>()
        .ok_or("an onset is malformed")?;
    let sections = v
        .get("sections")
        .and_then(Value::as_arr)
        .ok_or("`sections` is missing")?
        .iter()
        .map(|s| {
            Some(Section {
                kind: SectionKind::from_name(s.get("kind")?.as_str()?)?,
                nth: s.get("nth")?.as_i64()? as u32,
                start: float(s.get("start")?)?,
                end: float(s.get("end")?)?,
                energy: float(s.get("energy")?)? as f32,
            })
        })
        .collect::<Option<Vec<_>>>()
        .ok_or("a section is malformed")?;
    let band_columns = v.get("bands").and_then(Value::as_arr).ok_or("`bands` is missing")?;
    let columns = band_columns.iter().map(|c| unscaled(Some(c), 1000.0)).collect::<Result<Vec<_>, _>>()?;
    let energy = unscaled(v.get("energy"), 1000.0)?;
    let mut bands = vec![[0.0f32; 6]; energy.len()];
    for (band, column) in columns.iter().enumerate().take(6) {
        for (i, value) in column.iter().enumerate().take(bands.len()) {
            bands[i][band] = *value;
        }
    }
    Ok(MusicFeatures {
        duration: field(&v, "duration")?,
        bpm: field(&v, "bpm")?,
        grid_confidence: field(&v, "grid_confidence")? as f32,
        beats: read_times(v.get("beats"))?,
        downbeats: read_times(v.get("downbeats"))?,
        onsets,
        loudness_db: unscaled(v.get("loudness_db"), 10.0)?,
        energy,
        bands,
        sections,
    })
}

/// What a model needs to cut to the music: tempo, bars, sections, the
/// strongest onsets and the energy per bar.
pub fn music_summary(m: &MusicFeatures) -> Value {
    let mut strong: Vec<Onset> = m.onsets.iter().copied().filter(|o| o.strength >= 0.5).collect();
    strong.sort_by(|a, b| b.strength.total_cmp(&a.strength));
    strong.truncate(32);
    strong.sort_by(|a, b| a.t.total_cmp(&b.t));
    let bar_energy: Vec<Value> = m
        .downbeats
        .iter()
        .enumerate()
        .map(|(i, start)| {
            let end = m.downbeats.get(i + 1).copied().unwrap_or(m.duration);
            let a = (start * crate::RATE) as usize;
            let b = ((end * crate::RATE) as usize).clamp(a + 1, m.energy.len().max(a + 1));
            let slice = m.energy.get(a..b.min(m.energy.len())).unwrap_or(&[]);
            num(if slice.is_empty() { 0.0 } else { slice.iter().sum::<f32>() as f64 / slice.len() as f64 })
        })
        .collect();
    json::obj(vec![
        ("duration", num(m.duration)),
        ("bpm", num(m.bpm)),
        ("beat_seconds", num(if m.bpm > 0.0 { 60.0 / m.bpm } else { 0.0 })),
        ("bars", Value::Int(m.downbeats.len() as i64)),
        ("first_beat", m.beats.first().map_or(Value::Null, |t| num(*t))),
        ("downbeats", times(&m.downbeats)),
        ("sections", Value::Arr(m.sections.iter().map(section_json).collect())),
        ("sections_in_words", Value::Arr(crate::music::describe_sections(m).into_iter().map(json::s).collect())),
        ("strong_onsets", Value::Arr(strong.iter().map(|o| Value::Arr(vec![num(o.t), num(o.strength as f64)])).collect())),
        ("bar_energy", Value::Arr(bar_energy)),
    ])
}

fn word_json(w: &WordFeature) -> Value {
    json::obj(vec![("text", json::s(w.text.clone())), ("start", num(w.start)), ("end", num(w.end)), ("loudness_db", num(w.loudness_db as f64)), ("pitch_hz", num(w.pitch_hz as f64)), ("emphasis", num(w.emphasis as f64))])
}

pub fn voice_to_json(v: &VoiceFeatures) -> Value {
    json::obj(vec![
        ("version", Value::Int(VERSION)),
        ("rate", num(crate::RATE)),
        ("duration", num(v.duration)),
        ("envelope", scaled(&v.envelope, 1000.0)),
        ("pitch_hz", scaled(&v.pitch_hz, 10.0)),
        ("pauses", Value::Arr(v.pauses.iter().map(|(a, b)| Value::Arr(vec![num(*a), num(*b)])).collect())),
        ("syllables", times(&v.syllables)),
        ("words", Value::Arr(v.words.iter().map(word_json).collect())),
        ("phones", Value::Arr(v.phones.iter().map(|p| json::obj(vec![("text", json::s(p.text.clone())), ("start", num(p.start)), ("end", num(p.end))])).collect())),
    ])
}

pub fn voice_from_json(text: &str) -> Result<VoiceFeatures, String> {
    let v = json::parse_depth(text.as_bytes(), 8).map_err(|e| format!("voice features: {e}"))?;
    if v.get("version").and_then(Value::as_i64) != Some(VERSION) {
        return Err("voice features: another version".into());
    }
    let pair = |p: &Value| Some((float(p.as_arr()?.first()?)?, float(p.as_arr()?.get(1)?)?));
    let words = v
        .get("words")
        .and_then(Value::as_arr)
        .ok_or("`words` is missing")?
        .iter()
        .map(|w| {
            Some(WordFeature {
                text: w.get("text")?.as_str()?.to_string(),
                start: float(w.get("start")?)?,
                end: float(w.get("end")?)?,
                loudness_db: float(w.get("loudness_db")?)? as f32,
                pitch_hz: float(w.get("pitch_hz")?)? as f32,
                emphasis: float(w.get("emphasis")?)? as f32,
            })
        })
        .collect::<Option<Vec<_>>>()
        .ok_or("a word is malformed")?;
    let phones = v
        .get("phones")
        .and_then(Value::as_arr)
        .ok_or("`phones` is missing")?
        .iter()
        .map(|p| Some(Phone { text: p.get("text")?.as_str()?.to_string(), start: float(p.get("start")?)?, end: float(p.get("end")?)? }))
        .collect::<Option<Vec<_>>>()
        .ok_or("a phone is malformed")?;
    Ok(VoiceFeatures {
        duration: field(&v, "duration")?,
        envelope: unscaled(v.get("envelope"), 1000.0)?,
        pitch_hz: unscaled(v.get("pitch_hz"), 10.0)?,
        pauses: v.get("pauses").and_then(Value::as_arr).ok_or("`pauses` is missing")?.iter().map(pair).collect::<Option<Vec<_>>>().ok_or("a pause is malformed")?,
        syllables: read_times(v.get("syllables"))?,
        words,
        phones,
    })
}

/// What a model needs to time type to the voice: words with emphasis,
/// pauses and the syllable count.
pub fn voice_summary(v: &VoiceFeatures) -> Value {
    json::obj(vec![
        ("duration", num(v.duration)),
        ("words", Value::Arr(v.words.iter().map(word_json).collect())),
        ("pauses", Value::Arr(v.pauses.iter().map(|(a, b)| Value::Arr(vec![num(*a), num(*b)])).collect())),
        ("syllables", Value::Int(v.syllables.len() as i64)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn music_and_voice_round_trip() {
        let music = MusicFeatures {
            duration: 4.0,
            bpm: 120.0,
            grid_confidence: 0.9,
            beats: vec![0.0, 0.5, 1.0],
            downbeats: vec![0.0],
            onsets: vec![Onset { t: 0.5, strength: 0.75 }],
            loudness_db: vec![-12.3, -10.0],
            energy: vec![0.25, 0.5],
            bands: vec![[0.1, 0.2, 0.3, 0.4, 0.5, 0.6], [0.0; 6]],
            sections: vec![Section { kind: SectionKind::Drop, start: 0.0, end: 4.0, energy: 0.5, nth: 1 }],
        };
        let back = music_from_json(&music_to_json(&music).to_json()).unwrap();
        assert_eq!(back, music);
        assert_eq!(music_summary(&music).get("sections").and_then(Value::as_arr).map(|s| s.len()), Some(1));
        let voice = VoiceFeatures {
            duration: 1.0,
            envelope: vec![0.5],
            pitch_hz: vec![120.5],
            pauses: vec![(0.5, 0.75)],
            syllables: vec![0.25],
            words: vec![WordFeature { text: "one".into(), start: 0.0, end: 0.5, loudness_db: -20.0, pitch_hz: 120.0, emphasis: 0.625 }],
            phones: vec![Phone { text: "w".into(), start: 0.0, end: 0.125 }],
        };
        assert_eq!(voice_from_json(&voice_to_json(&voice).to_json()).unwrap(), voice);
    }
}
