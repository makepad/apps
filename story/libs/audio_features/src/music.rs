//! Music features: the beat grid, onsets, energy, bands and sections.

use crate::frames::{measure, normalize, quantile, smooth, smooth_db};
use crate::RATE;
use makepad_beat::onset::Envelopes;
use makepad_beat::offline::analyze_envelopes;
use makepad_beat::OfflineOptions;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Onset {
    pub t: f64,
    /// 0..1 against the track's strong onsets.
    pub strength: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionKind {
    Intro,
    Build,
    Drop,
    Break,
    Verse,
    Outro,
}

impl SectionKind {
    pub fn name(self) -> &'static str {
        match self {
            SectionKind::Intro => "intro",
            SectionKind::Build => "build",
            SectionKind::Drop => "drop",
            SectionKind::Break => "break",
            SectionKind::Verse => "verse",
            SectionKind::Outro => "outro",
        }
    }

    pub fn from_name(name: &str) -> Option<SectionKind> {
        [SectionKind::Intro, SectionKind::Build, SectionKind::Drop, SectionKind::Break, SectionKind::Verse, SectionKind::Outro].into_iter().find(|kind| kind.name() == name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Section {
    pub kind: SectionKind,
    pub start: f64,
    pub end: f64,
    /// Mean energy, 0..1.
    pub energy: f32,
    /// 1 for the first section of its kind, 2 for the second, …
    pub nth: u32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MusicFeatures {
    pub duration: f64,
    /// 0 when the music has no steady beat.
    pub bpm: f64,
    pub grid_confidence: f32,
    pub beats: Vec<f64>,
    pub downbeats: Vec<f64>,
    pub onsets: Vec<Onset>,
    /// Per frame at [`RATE`]: loudness in dBFS, energy 0..1 and the six
    /// band energies 0..1 (sub, bass, low mid, mid, high mid, high).
    pub loudness_db: Vec<f32>,
    pub energy: Vec<f32>,
    pub bands: Vec<[f32; 6]>,
    pub sections: Vec<Section>,
}

pub fn analyze_music(mono: &[f32], sample_rate: f64) -> MusicFeatures {
    analyze_music_with(mono, sample_rate, None)
}

/// [`analyze_music`] with the tempo the music was made at, when it is known
/// (a generated song's brief): it decides the beat's octave, nothing else.
pub fn analyze_music_with(mono: &[f32], sample_rate: f64, prior_bpm: Option<f64>) -> MusicFeatures {
    let sample_rate = sample_rate.max(1.0);
    let duration = mono.len() as f64 / sample_rate;
    let envelopes = Envelopes::from_mono(mono, sample_rate);
    let beat = analyze_envelopes(&envelopes, duration, OfflineOptions { prior_bpm, ..OfflineOptions::default() });
    let frames = measure(mono, sample_rate, RATE);
    // Energy follows the loudness over about a quarter second, so a beat's
    // attack and decay read as one level.
    let energy = normalize(&smooth_db(&frames.loudness_db, 12), 0.05, 0.98);
    let mut bands = vec![[0.0f32; 6]; frames.bands_db.len()];
    for band in 0..6 {
        let column: Vec<f32> = frames.bands_db.iter().map(|b| b[band]).collect();
        for (i, v) in normalize(&column, 0.05, 0.98).into_iter().enumerate() {
            bands[i][band] = v;
        }
    }
    let (beats, downbeats) = if beat.grid.has_grid() {
        (beat.beats().into_iter().filter(|t| *t >= 0.0 && *t <= duration).collect(), beat.downbeats().into_iter().filter(|t| *t >= 0.0 && *t <= duration).collect())
    } else {
        (Vec::new(), Vec::new())
    };
    let onsets = pick_onsets(&envelopes.flux, envelopes.hop_secs());
    let bar = if beat.grid.has_grid() { beat.grid.beat_secs * 4.0 } else { 2.0 };
    let first_bar = downbeats.first().copied().unwrap_or(0.0);
    let sections = sections(&energy, &bands, duration, bar, first_bar, &beat.changes_secs);
    MusicFeatures {
        duration,
        bpm: if beat.grid.has_grid() { beat.grid.bpm } else { 0.0 },
        grid_confidence: beat.grid.confidence,
        beats,
        downbeats,
        onsets,
        loudness_db: frames.loudness_db,
        energy,
        bands,
        sections,
    }
}

/// Local maxima of the flux that stand out from their neighbourhood.
fn pick_onsets(flux: &[f32], hop_secs: f64) -> Vec<Onset> {
    if flux.is_empty() || hop_secs <= 0.0 {
        return Vec::new();
    }
    let window = (0.25 / hop_secs).round().max(1.0) as usize;
    let local = smooth(flux, window);
    let near = (0.05 / hop_secs).round().max(1.0) as usize;
    let strong = quantile(flux, 0.99).max(1e-6);
    let floor = quantile(flux, 0.5);
    let mut out: Vec<Onset> = Vec::new();
    for i in 0..flux.len() {
        let v = flux[i];
        if v <= local[i] * 1.5 + floor * 0.5 || v <= 0.0 {
            continue;
        }
        let a = i.saturating_sub(near);
        let b = (i + near + 1).min(flux.len());
        if flux[a..b].iter().any(|w| *w > v) {
            continue;
        }
        let t = i as f64 * hop_secs;
        if out.last().is_some_and(|last| t - last.t < 0.05) {
            continue;
        }
        out.push(Onset { t, strength: (v / strong).min(1.0) });
    }
    out
}

/// Section boundaries on bar lines where the music changes, labelled by
/// their energy and its direction.
fn sections(energy: &[f32], bands: &[[f32; 6]], duration: f64, bar: f64, first_bar: f64, changes: &[f64]) -> Vec<Section> {
    if energy.is_empty() || duration <= 0.0 {
        return Vec::new();
    }
    let frame = |t: f64| ((t * RATE) as usize).min(energy.len() - 1);
    // Bar lines, and each bar's features: energy and band profile.
    let mut lines = Vec::new();
    let mut t = first_bar.rem_euclid(bar);
    while t < duration {
        lines.push(t);
        t += bar;
    }
    if lines.first().map_or(true, |first| *first > 0.0) {
        lines.insert(0, 0.0);
    }
    let bars: Vec<[f32; 7]> = lines
        .iter()
        .map(|start| {
            let (a, b) = (frame(*start), frame((start + bar).min(duration)).max(frame(*start) + 1).min(energy.len()));
            let n = (b - a).max(1) as f32;
            let mut f = [0.0f32; 7];
            for i in a..b {
                f[0] += energy[i];
                for band in 0..6 {
                    f[band + 1] += bands[i][band];
                }
            }
            f.map(|v| v / n)
        })
        .collect();
    // Novelty at each bar line: how different the four bars after it are
    // from the four before it (energy counts double).
    let span = 4usize;
    let mean = |from: usize, to: usize| {
        let mut m = [0.0f32; 7];
        let n = (to - from).max(1) as f32;
        for f in &bars[from..to] {
            for k in 0..7 {
                m[k] += f[k] / n;
            }
        }
        m
    };
    let mut novelty = vec![0.0f32; bars.len()];
    for i in 1..bars.len() {
        let before = mean(i.saturating_sub(span), i);
        let after = mean(i, (i + span).min(bars.len()));
        let mut d = 2.0 * (after[0] - before[0]).abs();
        for k in 1..7 {
            d += (after[k] - before[k]).abs() / 6.0;
        }
        novelty[i] = d;
    }
    // The beat analysis's own phrase changes vote too.
    for change in changes {
        if let Some((i, _)) = lines.iter().enumerate().min_by(|a, b| (a.1 - change).abs().total_cmp(&(b.1 - change).abs())) {
            if (lines[i] - change).abs() < bar * 0.5 {
                novelty[i] += 0.15;
            }
        }
    }
    let mut ranked: Vec<usize> = (1..bars.len()).collect();
    ranked.sort_by(|a, b| novelty[*b].total_cmp(&novelty[*a]));
    let threshold = quantile(&novelty[1..], 0.5).max(0.08);
    let min_gap = 4;
    let mut cuts: Vec<usize> = Vec::new();
    for i in ranked {
        if novelty[i] < threshold {
            break;
        }
        if cuts.iter().all(|c| c.abs_diff(i) >= min_gap) && i >= min_gap.min(2) && bars.len() - i >= 2 {
            cuts.push(i);
        }
    }
    cuts.sort();
    let mut bounds: Vec<(usize, usize)> = Vec::new();
    let mut start = 0;
    for cut in cuts.into_iter().chain(std::iter::once(bars.len())) {
        if cut > start {
            bounds.push((start, cut));
            start = cut;
        }
    }
    // Label by energy against the track's own range.
    let energies: Vec<f32> = bounds.iter().map(|(a, b)| mean(*a, *b)[0]).collect();
    let (min_e, max_e) = energies.iter().fold((f32::MAX, f32::MIN), |(lo, hi), e| (lo.min(*e), hi.max(*e)));
    let relative = |e: f32| if max_e - min_e < 0.05 { 1.0 } else { (e - min_e) / (max_e - min_e) };
    let n = bounds.len();
    let mut out: Vec<Section> = Vec::new();
    for (index, ((a, b), e)) in bounds.iter().zip(&energies).enumerate() {
        let r = relative(*e);
        let rising = bars[b - 1][0] - bars[*a][0] > 0.15;
        let next_drop = energies.get(index + 1).is_some_and(|next| relative(*next) >= 0.75 && *next > *e + 0.1);
        let after_drop = out.last().is_some_and(|prev: &Section| prev.kind == SectionKind::Drop);
        let kind = if n > 1 && index == 0 && r < 0.6 {
            SectionKind::Intro
        } else if n > 2 && index + 1 == n && r < 0.6 {
            SectionKind::Outro
        } else if r >= 0.75 {
            SectionKind::Drop
        } else if rising || next_drop {
            SectionKind::Build
        } else if after_drop && r < 0.5 {
            SectionKind::Break
        } else {
            SectionKind::Verse
        };
        let nth = out.iter().filter(|s| s.kind == kind).count() as u32 + 1;
        let end = if *b < lines.len() { lines[*b] } else { duration };
        out.push(Section { kind, start: lines[*a], end, energy: *e, nth });
    }
    out
}

impl MusicFeatures {
    /// Seconds of beat `n`, counting from 0 as `beat(n)` does in Splash.
    pub fn beat(&self, n: u32) -> Option<f64> {
        self.beats.get(n as usize).copied()
    }

    /// Seconds of beat `beat` of bar `bar`, both from 0 (`bar(n, beat)`).
    pub fn bar(&self, bar: u32, beat: u32) -> Option<f64> {
        let down = *self.downbeats.get(bar as usize)?;
        let index = self.beats.iter().position(|b| (b - down).abs() < 1e-3)?;
        self.beats.get(index + beat as usize).copied()
    }

    /// The `nth` section of a kind (1-based).
    pub fn section(&self, kind: SectionKind, nth: u32) -> Option<&Section> {
        self.sections.iter().find(|s| s.kind == kind && s.nth == nth.max(1))
    }

    /// A per-frame value at `t`, linearly interpolated.
    pub fn energy_at(&self, t: f64) -> f32 {
        crate::sample(&self.energy, t)
    }

    pub fn band_at(&self, band: usize, t: f64) -> f32 {
        let pos = (t * RATE).max(0.0);
        let i = pos.floor() as usize;
        match (self.bands.get(i), self.bands.get(i + 1)) {
            (Some(a), Some(b)) => {
                let f = (pos - i as f64) as f32;
                a[band.min(5)] * (1.0 - f) + b[band.min(5)] * f
            }
            (Some(a), None) => a[band.min(5)],
            _ => 0.0,
        }
    }

    /// Where `t` is in its beat, 0..1 (0 on the beat).
    pub fn beat_phase(&self, t: f64) -> f64 {
        let next = self.beats.partition_point(|b| *b <= t);
        match (next.checked_sub(1).and_then(|i| self.beats.get(i)), self.beats.get(next)) {
            (Some(a), Some(b)) if b > a => (t - a) / (b - a),
            _ => 0.0,
        }
    }

    /// The nearest beat (or downbeat, or onset of at least `strength`) to `t`.
    pub fn snap(&self, t: f64, to: Snap) -> f64 {
        let candidates: Vec<f64> = match to {
            Snap::Beat => self.beats.clone(),
            Snap::Downbeat => self.downbeats.clone(),
            Snap::Onset(min) => self.onsets.iter().filter(|o| o.strength >= min).map(|o| o.t).collect(),
        };
        candidates.into_iter().min_by(|a, b| (a - t).abs().total_cmp(&(b - t).abs())).unwrap_or(t)
    }
}

/// Each section's character in words, for a model directing to it: what
/// comes in and drops out against the section before (bass and kick,
/// highs, the beat), how full it is, and how the song ends.
pub fn describe_sections(m: &MusicFeatures) -> Vec<String> {
    let span = |a: f64, b: f64| ((a * RATE) as usize).min(m.energy.len())..((b * RATE) as usize).min(m.energy.len());
    let mean = |values: &mut dyn Iterator<Item = f32>| {
        let (sum, n) = values.fold((0.0f32, 0usize), |(s, n), v| (s + v, n + 1));
        if n == 0 { 0.0 } else { sum / n as f32 }
    };
    let facts = |s: &Section| {
        let r = span(s.start, s.end);
        let low = mean(&mut m.bands[r.clone()].iter().map(|b| (b[0] + b[1]) * 0.5));
        let high = mean(&mut m.bands[r.clone()].iter().map(|b| (b[4] + b[5]) * 0.5));
        let hits = m.onsets.iter().filter(|o| o.t >= s.start && o.t < s.end && o.strength >= 0.3).count() as f32 / (s.end - s.start).max(0.1) as f32;
        (low, high, hits)
    };
    let mut out = Vec::new();
    let mut prev: Option<(f32, f32, f32)> = None;
    for (i, section) in m.sections.iter().enumerate() {
        let (low, high, hits) = facts(section);
        let mut words: Vec<String> = Vec::new();
        words.push(match section.energy {
            e if e >= 0.75 => "full and loud".into(),
            e if e >= 0.45 => "mid energy".into(),
            _ => "sparse and quiet".into(),
        });
        match prev {
            None => {
                if m.energy.get((0.15 * RATE) as usize).is_some_and(|e| *e > 0.5) {
                    words.push("starts cold, at full level from the first beat".into());
                }
            }
            Some((plow, phigh, phits)) => {
                if low < plow - 0.2 {
                    words.push("the bass and kick drop out".into());
                } else if low > plow + 0.2 {
                    words.push("the bass and kick come in".into());
                }
                if high < phigh - 0.2 {
                    words.push("the highs fall away".into());
                } else if high > phigh + 0.2 {
                    words.push("the highs open up".into());
                }
                if phits > 0.5 && hits < phits * 0.5 {
                    words.push("the beat cuts out".into());
                } else if hits > (phits * 1.8).max(0.5) {
                    words.push("the beat comes back, busier".into());
                }
            }
        }
        if i + 1 == m.sections.len() {
            let tail = |secs: f64| m.energy.get(((m.duration - secs).max(0.0) * RATE) as usize).copied().unwrap_or(0.0);
            if tail(0.15) > 0.45 {
                words.push("ends dead, no fade: cut to black on the last hit".into());
            } else if tail(2.0) > 0.45 {
                words.push("fades out over its last seconds".into());
            }
        }
        out.push(format!("{} {} ({:.1}–{:.1} s): {}", section.kind.name(), section.nth, section.start, section.end, words.join("; ")));
        prev = Some((low, high, hits));
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Snap {
    Beat,
    Downbeat,
    Onset(f32),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 120 BPM track: 8 quiet bars of kicks, 8 loud bars of kicks and
    /// hats, 4 quiet bars.
    fn track(rate: f64) -> Vec<f32> {
        let beat = 0.5;
        let bars = [(8, 0.15f32, false), (8, 0.9, true), (4, 0.15, false)];
        let total: usize = bars.iter().map(|b| b.0).sum::<usize>() * 4;
        let mut out = vec![0.0f32; (total as f64 * beat * rate) as usize];
        let mut beat_index = 0usize;
        for (count, level, hats) in bars {
            for _ in 0..count * 4 {
                let start = (beat_index as f64 * beat * rate) as usize;
                for i in 0..(0.12 * rate) as usize {
                    let t = i as f64 / rate;
                    let kick = (2.0 * std::f64::consts::PI * 55.0 * t).sin() * (-t * 30.0).exp();
                    if let Some(s) = out.get_mut(start + i) {
                        *s += (kick as f32) * level;
                    }
                }
                if hats {
                    let off = start + (0.25 * rate) as usize;
                    let mut seed = 12345u32.wrapping_add(beat_index as u32);
                    for i in 0..(0.04 * rate) as usize {
                        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                        let noise = (seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0;
                        if let Some(s) = out.get_mut(off + i) {
                            *s += noise * 0.3 * (-(i as f32) / (0.01 * rate as f32)).exp();
                        }
                    }
                }
                beat_index += 1;
            }
        }
        out
    }

    #[test]
    fn a_synthetic_track_has_its_tempo_onsets_and_a_drop() {
        let rate = 22_050.0;
        let features = analyze_music(&track(rate), rate);
        assert!((features.bpm - 120.0).abs() < 1.5, "bpm {}", features.bpm);
        assert!(features.beats.len() > 60, "{}", features.beats.len());
        assert!(features.onsets.len() >= 60, "{} onsets", features.onsets.len());
        let drop = features.section(SectionKind::Drop, 1).unwrap_or_else(|| panic!("{:?}", features.sections));
        assert!((drop.start - 16.0).abs() < 2.1 && (drop.end - 32.0).abs() < 2.1, "{:?}", features.sections);
        assert_eq!(features.sections.first().unwrap().kind, SectionKind::Intro, "{:?}", features.sections);
        assert!(features.energy_at(20.0) > features.energy_at(8.0) + 0.25, "{} {}", features.energy_at(20.0), features.energy_at(8.0));
        let words = describe_sections(&features);
        assert_eq!(words.len(), features.sections.len());
        assert!(words.iter().any(|w| w.starts_with("drop 1") && w.contains("full and loud")), "{words:?}");
        let snapped = features.snap(16.1, Snap::Beat);
        assert!((snapped - 16.0).abs() < 0.06, "{snapped}");
        assert!(features.beat_phase(features.beats[10] + 0.25) > 0.4);
    }
}
