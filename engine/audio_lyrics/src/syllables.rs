//! Sung syllables inside an aligned word: how many the word's spelling
//! has, and where each starts in the word's time.
//!
//! The aligner times words, not syllables: its cross-attention rows are
//! BPE tokens (`Chat|G|PT`), which do not follow syllables. What the
//! vocal stem does show is where each sung syllable attacks: a new note or
//! a consonant is an onset, the same onsets stage 3 snaps word starts to.
//! So a word's syllables come from two measurements already made:
//!
//! 1. its spelling cut into syllables by English orthographic rules
//!    ([`split`]: vowel groups are nuclei; a silent final `e`, a silent
//!    `-ed`/`-es`, a consonant `-le` syllable; consonants between nuclei
//!    divide V-CV / VC-CV), and
//! 2. the vocal onsets inside the word ([`place`]): each boundary goes to
//!    the onset nearest where the letters put it, or stays there when no
//!    onset is close (a held vowel sliding to the next syllable has no
//!    attack to find).
//!
//! Sung English: a word the rules cannot split (a number, a name in caps
//! with no vowels) is one syllable, and one syllable is what readers
//! assume when a word has none.

use crate::align::Onset;

/// The shortest syllable a boundary may leave, seconds.
const MIN_SYLLABLE_SECS: f64 = 0.05;
/// How far from where the letters put a boundary an onset may pull it,
/// as a fraction of the word's length (and at most this many seconds).
const PULL_FRACTION: f64 = 0.35;
const PULL_MAX_SECS: f64 = 0.35;

/// The syllables of `word` (letters only; punctuation dropped), in order:
/// their letters concatenate to the word's letters. One piece when the
/// word has one nucleus or none.
pub fn split(word: &str) -> Vec<String> {
    // Hyphenated: each part on its own (X-ray, B-twenty).
    let parts: Vec<&str> = word.split('-').filter(|part| part.chars().any(char::is_alphabetic)).collect();
    if parts.len() > 1 {
        return parts.into_iter().flat_map(split).collect();
    }
    let chars: Vec<char> = word.chars().filter(|c| c.is_alphabetic()).collect();
    if chars.is_empty() {
        return vec![word.to_string()];
    }
    // Letters sung one by one: a short all-caps word (UFO, DJ, TV), one
    // with no vowel (HTML), and a
    // capitalised run without vowels ending a word (Sail|GPS).
    let upper = |c: &char| c.is_uppercase();
    let spelled = chars.iter().all(upper) && ((2..=3).contains(&chars.len()) || (chars.len() > 1 && !chars.iter().any(|c| "AEIOUY".contains(*c))));
    if spelled {
        return chars.iter().map(|c| c.to_lowercase().collect()).collect();
    }
    let run = chars.iter().rev().take_while(|c| upper(c)).count();
    if run >= 2 && run < chars.len() && !chars[chars.len() - run..].iter().any(|c| "AEIOUY".contains(*c)) {
        let head: String = chars[..chars.len() - run].iter().collect();
        let mut out = split_word(&head);
        out.extend(chars[chars.len() - run..].iter().map(|c| c.to_lowercase().collect::<String>()));
        return out;
    }
    split_word(word)
}

/// Silent-e words that begin compounds (some|body, every|where, time|less).
const SILENT_E_HEADS: &[&str] = &[
    "some", "one", "time", "home", "where", "there", "here", "love", "life", "care", "hope", "base", "fire",
    "side", "more", "make", "like", "live", "wide", "eye", "face", "name", "game", "space", "whole", "use",
];
/// Suffixes that keep a stem's silent e silent (love|ly, hope|less).
const SUFFIXES: &[&str] = &["ly", "less", "ment", "ful", "ness"];

fn split_word(word: &str) -> Vec<String> {
    let lower: String = word.chars().filter(|c| c.is_alphabetic()).flat_map(char::to_lowercase).collect();
    for head in SILENT_E_HEADS {
        if lower.len() > head.len() + 1 && lower.starts_with(head) {
            let mut out = vec![head.to_string()];
            out.extend(split_word(&lower[head.len()..]));
            return out;
        }
    }
    for suffix in SUFFIXES {
        if let Some(stem) = lower.strip_suffix(suffix) {
            if stem.len() >= 3 && stem.ends_with('e') {
                let mut out = split_word(stem);
                out.push(suffix.to_string());
                return out;
            }
        }
    }
    split_letters(&lower)
}

fn split_letters(word: &str) -> Vec<String> {
    let letters: Vec<char> = word.chars().filter(|c| c.is_alphabetic()).flat_map(char::to_lowercase).collect();
    if letters.is_empty() {
        return vec![word.to_string()];
    }
    let n = letters.len();
    let vowel = |i: usize| -> bool {
        match letters[i] {
            'a' | 'e' | 'i' | 'o' | 'u' => true,
            // `y` is a vowel after a consonant or at the end (sky, happy),
            // a consonant before a vowel at the start of a group (you, beyond).
            'y' => i > 0 && !(i + 1 < n && matches!(letters[i + 1], 'a' | 'e' | 'i' | 'o' | 'u')),
            _ => false,
        }
    };
    // Vowel groups: [start, end) of each run of vowels.
    let mut groups: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < n {
        if vowel(i) {
            let start = i;
            while i < n && vowel(i) {
                i += 1;
            }
            groups.push((start, i));
        } else {
            i += 1;
        }
    }
    // Two vowels that are two syllables: i-a (vi-a, di-al), i-o (ra-di-o),
    // e-o (vid-e-o), except when they spell one sound
    // (-tion, -sion, -cian keep "io"/"ia" as one).
    let mut split_groups: Vec<(usize, usize)> = Vec::new();
    for &(a, b) in &groups {
        let mut cut = None;
        for k in a..b.saturating_sub(1) {
            let pair = (letters[k], letters[k + 1]);
            let hiatus = b - a == 2
                && matches!(pair, ('i', 'a') | ('i', 'o') | ('e', 'o'))
                && !(k > 0 && matches!(letters[k - 1], 't' | 's' | 'c' | 'x' | 'g') && matches!(pair, ('i', 'a') | ('i', 'o')));
            if hiatus {
                cut = Some(k + 1);
                break;
            }
        }
        match cut {
            Some(c) => {
                split_groups.push((a, c));
                split_groups.push((c, b));
            }
            None => split_groups.push((a, b)),
        }
    }
    let mut groups = split_groups;
    let ends_with = |s: &str| letters.iter().collect::<String>().ends_with(s);
    let consonant_at = |i: usize| i < n && !vowel(i);
    if groups.len() > 1 {
        let last = *groups.last().unwrap();
        let only_e = last.1 - last.0 == 1 && letters[last.0] == 'e';
        // Silent final e (make, love), but a consonant-le is sung (lit-tle).
        let silent_e = only_e && last.1 == n && !(n >= 3 && letters[n - 2] == 'l' && consonant_at(n - 3));
        // -ed is silent except after t or d (played; wanted).
        let silent_ed = only_e && last.1 == n - 1 && ends_with("ed") && n >= 3 && !matches!(letters[n - 3], 't' | 'd');
        // -es is silent except after a sibilant (makes; kisses, watches).
        let sibilant = n >= 4 && (matches!(letters[n - 3], 's' | 'x' | 'z') || (matches!(letters[n - 3], 'h') && matches!(letters[n - 4], 'c' | 's')) || (letters[n - 3] == 'g' || letters[n - 3] == 'c'));
        let silent_es = only_e && last.1 == n - 1 && ends_with("es") && !sibilant;
        if silent_e || silent_ed || silent_es {
            groups.pop();
        }
    }
    if groups.len() <= 1 {
        return vec![letters.iter().collect()];
    }
    // Cut between consecutive nuclei: one consonant goes with the next
    // syllable (ba-by), two or more split after the first (win-dow), and
    // a digraph stays whole (fa-ther, wi-shing).
    let mut cuts = Vec::new();
    for pair in groups.windows(2) {
        let (from, to) = (pair[0].1, pair[1].0);
        let consonants = to - from;
        let cut = if consonants <= 1 {
            from
        } else {
            let digraph = consonants == 2 && matches!((letters[from], letters[from + 1]), ('t', 'h') | ('s', 'h') | ('c', 'h') | ('p', 'h') | ('w', 'h') | ('c', 'k') | ('n', 'g'));
            // A consonant + l/r cluster starts the next syllable (a-pril).
            let cluster = consonants == 2 && matches!(letters[from + 1], 'l' | 'r') && !matches!(letters[from], 'l' | 'r' | 'n' | 'm');
            match (digraph, cluster) {
                (true, _) if (letters[from], letters[from + 1]) == ('c', 'k') || (letters[from], letters[from + 1]) == ('n', 'g') => to,
                (true, _) | (_, true) => from,
                _ => from + 1,
            }
        };
        cuts.push(cut);
    }
    let mut out = Vec::new();
    let mut at = 0;
    for cut in cuts {
        out.push(letters[at..cut].iter().collect());
        at = cut;
    }
    out.push(letters[at..].iter().collect());
    out
}

/// Where each syllable of a word sung over `[start, end]` starts: the
/// first at `start`; each later boundary where the letters of the pieces
/// before it put it (vowels weigh most: they are what is held), pulled to
/// the nearest vocal onset close enough, never closer than 50 ms to its
/// neighbours. `pieces` is [`split`]'s output.
pub fn place(start: f64, end: f64, pieces: &[String], onsets: &[Onset]) -> Vec<f64> {
    let n = pieces.len();
    if n <= 1 || end - start < MIN_SYLLABLE_SECS * n as f64 {
        return vec![start];
    }
    let weight = |piece: &str| -> f64 {
        piece.chars().map(|c| if matches!(c, 'a' | 'e' | 'i' | 'o' | 'u' | 'y') { 2.0 } else { 1.0 }).sum::<f64>().max(1.0)
    };
    let weights: Vec<f64> = pieces.iter().map(|p| weight(p)).collect();
    let total: f64 = weights.iter().sum();
    let length = end - start;
    let pull = (length * PULL_FRACTION).min(PULL_MAX_SECS);
    let mut out = vec![start];
    let mut acc = 0.0;
    for k in 1..n {
        acc += weights[k - 1];
        let expected = start + length * acc / total;
        let lo = out[k - 1] + MIN_SYLLABLE_SECS;
        let hi = end - MIN_SYLLABLE_SECS * (n - k) as f64;
        // The nearest onset inside the room this boundary has.
        let found = onsets
            .iter()
            .map(|o| o.time)
            .filter(|t| *t >= lo && *t <= hi && (t - expected).abs() <= pull)
            .min_by(|a, b| (a - expected).abs().total_cmp(&(b - expected).abs()));
        out.push(found.unwrap_or(expected).clamp(lo, hi.max(lo)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(word: &str) -> usize {
        split(word).len()
    }

    #[test]
    fn english_words_split_into_their_sung_syllables() {
        for (word, n) in [
            ("I", 1), ("see", 1), ("boats", 1), ("eyes", 1), ("make", 1), ("love", 1), ("played", 1), ("makes", 1),
            ("sky", 1), ("you", 1), ("through", 1), ("tie", 1), ("strength", 1),
            ("SailGPS,", 4), ("UFO", 3), ("HTML", 4), ("X-ray", 2), ("B-twenty", 3), ("ROOM", 1), ("room", 1), ("little", 2), ("wanted", 2), ("kisses", 2), ("happy", 2), ("window", 2),
            ("father", 2), ("nervous,", 2), ("sudden", 2), ("garden", 2), ("harbour", 2), ("surprise", 2),
            ("beautiful", 3), ("radio", 3), ("video", 3), ("tomorrow", 3), ("computer", 3), ("everything", 4),
            ("alive", 2), ("please", 1), ("midnight", 2), ("somebody", 3), ("watermelon", 4), ("caterpillar", 4),
        ] {
            assert_eq!(count(word), n, "{word}: {:?}", split(word));
        }
        assert_eq!(split("window"), ["win", "dow"]);
        assert_eq!(split("baby"), ["ba", "by"]);
        assert_eq!(split("father"), ["fa", "ther"]);
        assert_eq!(split("little"), ["lit", "tle"]);
        assert_eq!(split("1984"), ["1984"]);
    }

    #[test]
    fn boundaries_go_to_the_onsets_inside_the_word() {
        let pieces = split("window");
        let onset = |time: f64| Onset { time, strength: 1.0 };
        // An onset near where the letters put the boundary takes it.
        let starts = place(10.0, 11.0, &pieces, &[onset(9.9), onset(10.62), onset(11.2)]);
        assert_eq!(starts, vec![10.0, 10.62]);
        // None close: the letters' own place.
        let starts = place(10.0, 11.0, &pieces, &[onset(9.9)]);
        assert!((starts[1] - 10.5).abs() < 0.01, "{starts:?}");
        // Always increasing, inside the word, and never closer than 50 ms.
        let pieces = split("watermelon");
        let starts = place(2.0, 2.4, &pieces, &[onset(2.01), onset(2.02), onset(2.03)]);
        assert_eq!(starts.len(), 4);
        assert!(starts.windows(2).all(|w| w[1] - w[0] >= MIN_SYLLABLE_SECS - 1e-9) && *starts.last().unwrap() < 2.4, "{starts:?}");
        // A word too short to hold its syllables is one.
        assert_eq!(place(1.0, 1.08, &split("beautiful"), &[]), vec![1.0]);
    }
}
