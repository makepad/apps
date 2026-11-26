//! A take's word times, from the best source that gives a
//! usable answer: the TTS's own timings, forced alignment of the
//! known text against the take, else the take's speech envelope.
//! A source's answer is used only when it has exactly the text's words, in
//! order, inside the take.

use crate::envelope::distribute_words;
use makepad_story_cut::Word;

/// Where a take's word times came from, best first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimingSource {
    /// The TTS engine's own durations.
    Tts,
    /// Forced alignment of the known text.
    Aligned,
    /// Spread over the speech envelope; no model.
    Envelope,
}

impl TimingSource {
    pub fn id(self) -> &'static str {
        match self {
            Self::Tts => "tts",
            Self::Aligned => "aligned",
            Self::Envelope => "envelope",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "tts" => Self::Tts,
            "aligned" => Self::Aligned,
            "envelope" => Self::Envelope,
            _ => return None,
        })
    }
}

/// Aligns known text against audio that speaks it: one word per
/// whitespace word of `text`, seconds from the first sample. `language` is
/// a whisper code (`"en"`).
pub trait ForcedAligner {
    fn align(&mut self, text: &str, pcm: &[f32], rate: u32, language: &str) -> Result<Vec<Word>, String>;
}

/// Whether `words` time exactly `text`'s words in order, start before end,
/// never overlapping, inside `duration` (with 50 ms of slack for rounding).
pub fn usable(words: &[Word], text: &str, duration: f64) -> bool {
    let expected = text.split_whitespace().collect::<Vec<_>>();
    words.len() == expected.len()
        && !words.is_empty()
        && words.iter().zip(&expected).all(|(word, text)| word.text == *text && word.start.is_finite() && word.end.is_finite() && word.start >= -0.05 && word.start <= word.end && word.end <= duration + 0.05)
        && words.windows(2).all(|pair| pair[0].start <= pair[1].start && pair[0].end <= pair[1].start + 0.05)
}

/// Word times for `text` in mono `pcm` at `rate` Hz, spoken in `language`:
/// `tts` when usable, else the `aligner`'s when usable, else
/// [`distribute_words`]. Times are clamped into the take.
pub fn word_timings(text: &str, pcm: &[f32], rate: u32, language: &str, tts: Option<Vec<Word>>, aligner: Option<&mut dyn ForcedAligner>) -> (Vec<Word>, TimingSource) {
    let duration = if rate == 0 { 0.0 } else { pcm.len() as f64 / rate as f64 };
    let clamp = |words: Vec<Word>| words.into_iter().map(|word| Word { start: word.start.clamp(0.0, duration), end: word.end.clamp(0.0, duration), text: word.text }).collect();
    if let Some(words) = tts.filter(|words| usable(words, text, duration)) {
        return (clamp(words), TimingSource::Tts);
    }
    if let Some(aligner) = aligner {
        if let Ok(words) = aligner.align(text, pcm, rate, language) {
            if usable(&words, text, duration) {
                return (clamp(words), TimingSource::Aligned);
            }
        }
    }
    (distribute_words(text, pcm, rate), TimingSource::Envelope)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, start: f64, end: f64) -> Word {
        Word { text: text.into(), start, end }
    }

    struct Fixed(Result<Vec<Word>, String>);

    impl ForcedAligner for Fixed {
        fn align(&mut self, _: &str, _: &[f32], _: u32, _: &str) -> Result<Vec<Word>, String> {
            self.0.clone()
        }
    }

    #[test]
    fn the_best_usable_source_wins() {
        let pcm = vec![0.3f32; 24_000];
        let good = vec![word("hello", 0.1, 0.4), word("there", 0.5, 0.9)];
        let (words, source) = word_timings("hello there", &pcm, 24_000, "en", Some(good.clone()), Some(&mut Fixed(Ok(vec![]))));
        assert_eq!((words, source), (good.clone(), TimingSource::Tts));
        // Wrong word count from the TTS: the aligner answers.
        let (words, source) = word_timings("hello there", &pcm, 24_000, "en", Some(vec![word("hello", 0.0, 1.0)]), Some(&mut Fixed(Ok(good.clone()))));
        assert_eq!((words, source), (good.clone(), TimingSource::Aligned));
        // An aligner error or an answer past the take: the envelope.
        let (_, source) = word_timings("hello there", &pcm, 24_000, "en", None, Some(&mut Fixed(Err("no model".into()))));
        assert_eq!(source, TimingSource::Envelope);
        let late = vec![word("hello", 0.1, 0.4), word("there", 0.5, 1.9)];
        let (words, source) = word_timings("hello there", &pcm, 24_000, "en", None, Some(&mut Fixed(Ok(late))));
        assert_eq!(source, TimingSource::Envelope);
        assert_eq!(words.len(), 2);
    }

    #[test]
    fn usable_checks_words_order_and_bounds() {
        let words = [word("a", 0.0, 0.2), word("b", 0.2, 0.5)];
        assert!(usable(&words, " a  b ", 0.5));
        assert!(!usable(&words, "a c", 0.5));
        assert!(!usable(&[word("a", 0.3, 0.4), word("b", 0.2, 0.5)], "a b", 0.5), "out of order");
        assert!(!usable(&[word("a", 0.0, 0.4), word("b", 0.2, 0.5)], "a b", 0.5), "overlapping");
        assert!(!usable(&[], "", 1.0));
    }
}
