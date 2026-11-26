//! Word times without a model (the last of its three sources): the take's
//! speech envelope says where the voice starts, stops and pauses; the words
//! of the known text are spread over the voiced time in proportion to their
//! length, and a word boundary near a pause moves onto it.

use makepad_story_cut::Word;

/// Envelope hop, seconds.
pub const HOP: f64 = 0.01;
/// A frame is voiced above this fraction of the loudest frame's RMS (-26 dB).
const VOICED: f32 = 0.05;
/// Silences shorter than this are inside words (stops, plosives).
pub const MIN_PAUSE: f64 = 0.08;

/// RMS per [`HOP`] of mono `pcm` at `rate` Hz.
pub fn rms_envelope(pcm: &[f32], rate: u32) -> Vec<f32> {
    let hop = ((rate as f64 * HOP).round() as usize).max(1);
    pcm.chunks(hop).map(|chunk| (chunk.iter().map(|v| v * v).sum::<f32>() / chunk.len() as f32).sqrt()).collect()
}

/// The voiced stretches of an envelope, `(start, end)` in seconds, with
/// pauses shorter than [`MIN_PAUSE`] closed.
pub fn voiced_segments(envelope: &[f32]) -> Vec<(f64, f64)> {
    let peak = envelope.iter().copied().fold(0.0f32, f32::max);
    if peak <= 0.0 {
        return Vec::new();
    }
    let threshold = peak * VOICED;
    let mut segments: Vec<(f64, f64)> = Vec::new();
    let mut open: Option<usize> = None;
    for (index, value) in envelope.iter().chain(std::iter::once(&0.0)).enumerate() {
        match (open, *value > threshold) {
            (None, true) => open = Some(index),
            (Some(start), false) => {
                let (start, end) = (start as f64 * HOP, index as f64 * HOP);
                match segments.last_mut() {
                    Some(last) if start - last.1 < MIN_PAUSE - 1e-9 => last.1 = end,
                    _ => segments.push((start, end)),
                }
                open = None;
            }
            _ => {}
        }
    }
    segments
}

/// Word times for `text` spoken in mono `pcm` at `rate` Hz, in seconds
/// from the take's start. Empty when the text has no words; spread over
/// the whole take when it has no voiced frame.
pub fn distribute_words(text: &str, pcm: &[f32], rate: u32) -> Vec<Word> {
    let words = text.split_whitespace().collect::<Vec<_>>();
    if words.is_empty() {
        return Vec::new();
    }
    let duration = if rate == 0 { 0.0 } else { pcm.len() as f64 / rate as f64 };
    let mut segments = voiced_segments(&rms_envelope(pcm, rate));
    if segments.is_empty() {
        segments.push((0.0, duration));
    }
    let voiced: f64 = segments.iter().map(|(start, end)| end - start).sum();
    // Boundaries in voiced seconds, proportional to length (a character
    // each plus one for the space after).
    let weights = words.iter().map(|word| word.chars().count() as f64 + 1.0).collect::<Vec<_>>();
    let total: f64 = weights.iter().sum();
    let mut bounds = Vec::with_capacity(words.len() + 1);
    let mut at = 0.0;
    bounds.push(0.0);
    for weight in &weights {
        at += voiced * weight / total;
        bounds.push(at);
    }
    bounds[words.len()] = voiced;
    // A pause is where speech breaks: the nearest inner boundary within
    // 40% of a mean word moves onto it.
    let tolerance = 0.4 * voiced / words.len() as f64;
    let mut pause_at = 0.0;
    for (start, end) in &segments[..segments.len() - 1] {
        pause_at += end - start;
        let nearest = (1..words.len()).min_by(|a, b| (bounds[*a] - pause_at).abs().total_cmp(&(bounds[*b] - pause_at).abs()));
        if let Some(index) = nearest {
            if (bounds[index] - pause_at).abs() <= tolerance && bounds[index - 1] < pause_at && pause_at < bounds[index + 1] {
                bounds[index] = pause_at;
            }
        }
    }
    // Voiced seconds to take seconds: a boundary on a pause starts the next
    // word after it and ends the previous one before it.
    let to_time = |voiced_at: f64, starting: bool| {
        let mut before = 0.0;
        for (index, (start, end)) in segments.iter().enumerate() {
            let length = end - start;
            let last = index + 1 == segments.len();
            if voiced_at < before + length - 1e-9 || (voiced_at <= before + length + 1e-9 && (!starting || last)) {
                return start + (voiced_at - before).clamp(0.0, length);
            }
            before += length;
        }
        segments[segments.len() - 1].1
    };
    words.iter().enumerate().map(|(index, word)| Word {
        text: word.to_string(),
        start: to_time(bounds[index], true),
        end: to_time(bounds[index + 1], false),
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tone bursts at `bursts` (start, end) seconds in `length` seconds.
    fn speech(rate: u32, length: f64, bursts: &[(f64, f64)]) -> Vec<f32> {
        let mut pcm = vec![0.0f32; (rate as f64 * length) as usize];
        for (start, end) in bursts {
            for i in (start * rate as f64) as usize..(end * rate as f64) as usize {
                pcm[i] = 0.5 * (i as f32 * 0.2).sin();
            }
        }
        pcm
    }

    #[test]
    fn silence_is_trimmed_and_short_gaps_closed() {
        let rate = 24_000;
        let env = rms_envelope(&speech(rate, 2.0, &[(0.3, 0.8), (0.84, 1.0), (1.3, 1.6)]), rate);
        assert_eq!(env.len(), 200);
        let segments = voiced_segments(&env);
        assert_eq!(segments.len(), 2, "{segments:?}");
        assert!((segments[0].0 - 0.3).abs() < 0.011 && (segments[0].1 - 1.0).abs() < 0.011);
        assert!((segments[1].0 - 1.3).abs() < 0.011 && (segments[1].1 - 1.6).abs() < 0.011);
        assert!(voiced_segments(&rms_envelope(&vec![0.0; 2400], rate)).is_empty());
    }

    #[test]
    fn words_land_on_their_bursts() {
        let rate = 24_000;
        // Three words of equal length, one burst each.
        let pcm = speech(rate, 2.4, &[(0.2, 0.6), (0.9, 1.3), (1.6, 2.0)]);
        let words = distribute_words("one two six", &pcm, rate);
        assert_eq!(words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(), ["one", "two", "six"]);
        for (word, (start, end)) in words.iter().zip([(0.2, 0.6), (0.9, 1.3), (1.6, 2.0)]) {
            assert!((word.start - start).abs() < 0.015 && (word.end - end).abs() < 0.015, "{word:?}");
        }
        // A long word and a short one in one burst share it by length.
        let pcm = speech(rate, 1.2, &[(0.1, 1.1)]);
        let words = distribute_words("a bbbbbbb", &pcm, rate);
        assert!((words[0].start - 0.1).abs() < 0.015 && (words[0].end - 0.3).abs() < 0.015 && (words[1].end - 1.1).abs() < 0.015, "{words:?}");
        // Words stay in order and inside the take.
        let words = distribute_words("Type, and it saves as you go.", &speech(rate, 3.0, &[(0.1, 1.2), (1.5, 2.8)]), rate);
        assert!(words.windows(2).all(|pair| pair[0].end <= pair[1].start + 1e-9 && pair[0].start < pair[0].end));
        assert!(words[0].start >= 0.09 && words[6].end <= 2.81);
    }

    #[test]
    fn a_silent_take_spreads_over_its_length() {
        let words = distribute_words("a b", &vec![0.0; 24_000], 24_000);
        assert_eq!((words[0].start, words[1].end), (0.0, 1.0));
        assert!(distribute_words(" ", &vec![0.1; 100], 24_000).is_empty());
    }
}
