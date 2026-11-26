//! Lip sync: phones to the nine standard mouth shapes (visemes), picked
//! per sound and opened by the voice's loudness.
//!
//! A Kokoro take carries its phonemes, timed by Kokoro's own duration
//! predictor ([`VoiceFeatures::phones`]): each mouth shape is then the one
//! of the phoneme being spoken. Without them (a song's vocal stem aligned
//! by Whisper, which times words only), each word's phones come from a
//! small English letter-to-sound rule table and share the word's span,
//! vowels longer than consonants.
//! The mouth snaps between shapes (no tweening, as in cut-out animation)
//! and the caller samples it one frame ahead of the audio: a mouth that
//! lands with the sound reads as late.

use crate::features::{FeatureWord, VoiceFeatures};

/// The nine mouth shapes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Viseme {
    Rest,
    Mbp,
    Tdskn,
    Eeiy,
    Aha,
    Oh,
    Oowr,
    Fv,
    Lth,
}

impl Viseme {
    pub const ALL: [Viseme; 9] = [Viseme::Rest, Viseme::Mbp, Viseme::Tdskn, Viseme::Eeiy, Viseme::Aha, Viseme::Oh, Viseme::Oowr, Viseme::Fv, Viseme::Lth];

    /// The part id suffix: `mouth_REST`, `mouth_MBP`, …
    pub fn name(self) -> &'static str {
        match self {
            Viseme::Rest => "REST",
            Viseme::Mbp => "MBP",
            Viseme::Tdskn => "TDSKN",
            Viseme::Eeiy => "EEIY",
            Viseme::Aha => "AHA",
            Viseme::Oh => "OH",
            Viseme::Oowr => "OOWR",
            Viseme::Fv => "FV",
            Viseme::Lth => "LTH",
        }
    }

    pub fn from_name(name: &str) -> Option<Viseme> {
        Viseme::ALL.iter().copied().find(|v| v.name().eq_ignore_ascii_case(name))
    }

    /// Whether the shape is open (its opening follows the loudness).
    pub fn is_open(self) -> bool {
        matches!(self, Viseme::Tdskn | Viseme::Eeiy | Viseme::Aha | Viseme::Oh | Viseme::Oowr | Viseme::Lth)
    }

    /// This shape, then the nearest ones, for a model that lacks some.
    pub fn fallbacks(self) -> &'static [Viseme] {
        use Viseme::*;
        match self {
            Rest => &[Rest],
            Mbp => &[Mbp, Rest],
            Tdskn => &[Tdskn, Eeiy, Aha, Rest],
            Eeiy => &[Eeiy, Tdskn, Aha, Rest],
            Aha => &[Aha, Oh, Eeiy, Tdskn, Rest],
            Oh => &[Oh, Oowr, Aha, Rest],
            Oowr => &[Oowr, Oh, Rest],
            Fv => &[Fv, Mbp, Tdskn, Rest],
            Lth => &[Lth, Tdskn, Aha, Rest],
        }
    }
}

/// The mouth shape of a phone: ARPAbet (`AA1`, `M`, `TH`; stress digits
/// ignored) or IPA (`ɑ`, `θ`, `ʃ`). Silence (`sil`, `sp`, `pau`, empty)
/// and anything unknown rest.
pub fn viseme_of_phone(phone: &str) -> Viseme {
    use Viseme::*;
    let p: String = phone.trim().trim_end_matches(|c: char| c.is_ascii_digit() || c == 'ˈ' || c == 'ˌ' || c == 'ː').to_string();
    let upper = p.to_ascii_uppercase();
    match upper.as_str() {
        "" | "SIL" | "SP" | "PAU" | "SPN" => return Rest,
        "M" | "B" | "P" | "EM" => return Mbp,
        "F" | "V" => return Fv,
        "TH" | "DH" | "L" | "EL" => return Lth,
        "T" | "D" | "S" | "Z" | "K" | "G" | "N" | "NG" | "EN" | "SH" | "ZH" | "CH" | "JH" | "HH" | "DX" | "Q" => return Tdskn,
        "IY" | "IH" | "EY" | "Y" | "IX" => return Eeiy,
        "AA" | "AE" | "AH" | "AY" | "AW" | "EH" | "AX" => return Aha,
        "AO" | "OW" | "OY" => return Oh,
        "UW" | "UH" | "W" | "R" | "ER" | "AXR" | "UX" => return Oowr,
        _ => {}
    }
    // IPA, first symbol decides.
    match p.chars().next() {
        Some('m' | 'b' | 'p' | 'ɱ') => Mbp,
        Some('f' | 'v') => Fv,
        Some('θ' | 'ð' | 'l' | 'ɫ') => Lth,
        Some('t' | 'd' | 's' | 'z' | 'k' | 'g' | 'ɡ' | 'n' | 'ŋ' | 'ʃ' | 'ʒ' | 'ʧ' | 'ʤ' | 'h' | 'ɾ' | 'ʔ' | 'x' | 'ç' | 'ɕ') => Tdskn,
        Some('i' | 'ɪ' | 'e' | 'j' | 'ɨ') => Eeiy,
        Some('a' | 'ɑ' | 'æ' | 'ʌ' | 'ɛ' | 'ə' | 'ɐ' | 'ɜ' | 'ᵊ') => Aha,
        Some('o' | 'ɔ' | 'ɒ') => Oh,
        Some('u' | 'ʊ' | 'w' | 'r' | 'ɹ' | 'ɚ' | 'ɝ' | 'ʉ') => Oowr,
        _ => Rest,
    }
}

const VOWELS: &[&str] = &["AA", "AE", "AH", "AO", "AW", "AY", "EH", "ER", "EY", "IH", "IY", "OW", "OY", "UH", "UW"];

fn is_vowel_letter(c: u8) -> bool {
    matches!(c, b'a' | b'e' | b'i' | b'o' | b'u' | b'y')
}

/// A word's phones (ARPAbet) from spelling: a small rule table for English
/// (digraphs, a silent final e that lengthens the vowel before it, double
/// letters as one). Good enough for mouth shapes; it is not a lexicon.
/// Digits are spelled out ("5.5" → five point five).
pub fn phones_of_word(word: &str) -> Vec<&'static str> {
    const DIGITS: [&str; 10] = ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine"];
    let mut spelled = String::new();
    for c in word.chars() {
        if let Some(d) = c.to_digit(10) {
            spelled.push(' ');
            spelled.push_str(DIGITS[d as usize]);
            spelled.push(' ');
        } else if c == '.' && word.chars().any(|c| c.is_ascii_digit()) {
            spelled.push_str(" point ");
        } else if c == '%' {
            spelled.push_str(" percent ");
        } else if c == '&' {
            spelled.push_str(" and ");
        } else if c.is_alphabetic() {
            spelled.extend(c.to_lowercase());
        } else {
            spelled.push(' ');
        }
    }
    let mut out = Vec::new();
    for part in spelled.split_whitespace() {
        out.extend(phones_of_letters(part.as_bytes()));
    }
    out
}

fn phones_of_letters(w: &[u8]) -> Vec<&'static str> {
    let n = w.len();
    // A final silent e after a consonant (not "the", "be"): the vowel
    // before that consonant is long.
    let silent_e = n > 2 && w[n - 1] == b'e' && !is_vowel_letter(w[n - 2]) && !w[..n - 2].is_empty() && w[..n - 2].iter().any(|&c| is_vowel_letter(c));
    let long_at = if silent_e { w[..n - 2].iter().rposition(|&c| is_vowel_letter(c)) } else { None };
    let end = if silent_e { n - 1 } else { n };
    let mut out: Vec<&'static str> = Vec::new();
    let mut i = 0;
    while i < end {
        let rest = &w[i..end];
        let starts = |s: &str| rest.starts_with(s.as_bytes());
        let (phones, used): (&[&'static str], usize) = if starts("tch") {
            (&["CH"], 3)
        } else if starts("igh") {
            (&["AY"], 3)
        } else if starts("ough") {
            (&["OW"], 4)
        } else if starts("augh") {
            (&["AO"], 4)
        } else if starts("tion") {
            (&["SH", "AH", "N"], 4)
        } else if i == 0 && starts("kn") {
            (&["N"], 2)
        } else if i == 0 && starts("wr") {
            (&["R"], 2)
        } else if starts("th") {
            (&["TH"], 2)
        } else if starts("sh") {
            (&["SH"], 2)
        } else if starts("ch") {
            (&["CH"], 2)
        } else if starts("ph") {
            (&["F"], 2)
        } else if starts("wh") {
            (&["W"], 2)
        } else if starts("ck") {
            (&["K"], 2)
        } else if starts("ng") {
            (&["NG"], 2)
        } else if starts("qu") {
            (&["K", "W"], 2)
        } else if starts("gh") {
            (&[], 2)
        } else if starts("ee") || starts("ea") || starts("ie") {
            (&["IY"], 2)
        } else if starts("oo") {
            (&["UW"], 2)
        } else if starts("ou") {
            (&["AW"], 2)
        } else if starts("ow") {
            (&["OW"], 2)
        } else if starts("oa") {
            (&["OW"], 2)
        } else if starts("oi") || starts("oy") {
            (&["OY"], 2)
        } else if starts("ai") || starts("ay") || starts("ey") {
            (&["EY"], 2)
        } else if starts("au") || starts("aw") {
            (&["AO"], 2)
        } else if starts("ew") || starts("ue") {
            (&["UW"], 2)
        } else if starts("er") || starts("ir") || starts("ur") {
            (&["ER"], 2)
        } else if starts("ar") {
            (&["AA", "R"], 2)
        } else if starts("or") {
            (&["AO", "R"], 2)
        } else {
            let c = w[i];
            let next = w.get(i + 1).copied();
            let long = long_at == Some(i);
            let one: &'static [&'static str] = match c {
                b'a' => if long { &["EY"] } else { &["AE"] },
                b'e' => if long { &["IY"] } else if i + 1 == end && end > 3 { &[] } else { &["EH"] },
                b'i' => if long { &["AY"] } else { &["IH"] },
                b'o' => if long { &["OW"] } else { &["AA"] },
                b'u' => if long { &["UW"] } else { &["AH"] },
                b'y' => if i == 0 { &["Y"] } else if long || i + 1 == end { &["IY"] } else { &["IH"] },
                b'b' => &["B"],
                b'c' => if matches!(next, Some(b'e' | b'i' | b'y')) { &["S"] } else { &["K"] },
                b'd' => &["D"],
                b'f' => &["F"],
                b'g' => &["G"],
                b'h' => &["HH"],
                b'j' => &["JH"],
                b'k' => &["K"],
                b'l' => &["L"],
                b'm' => &["M"],
                b'n' => &["N"],
                b'p' => &["P"],
                b'q' => &["K"],
                b'r' => &["R"],
                b's' => &["S"],
                b't' => &["T"],
                b'v' => &["V"],
                b'w' => &["W"],
                b'x' => &["K", "S"],
                b'z' => &["Z"],
                _ => &[],
            };
            // A doubled consonant is one sound.
            let used = if !is_vowel_letter(c) && next == Some(c) { 2 } else { 1 };
            (one, used)
        };
        out.extend_from_slice(phones);
        i += used;
    }
    out
}

/// How long a phone is relative to the others in its word.
fn weight(phone: &str) -> f64 {
    if VOWELS.contains(&phone) {
        2.2
    } else if matches!(phone, "R" | "L" | "W" | "Y" | "M" | "N" | "NG") {
        1.2
    } else if phone == "HH" {
        0.6
    } else {
        1.0
    }
}

/// A mouth shape over a span of time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timed {
    pub start: f64,
    pub end: f64,
    pub viseme: Viseme,
}

/// A word's mouth shapes over its span (its phones, vowels longer).
/// Neighbouring phones with the same shape merge.
pub fn word_visemes(word: &FeatureWord) -> Vec<Timed> {
    let phones = phones_of_word(&word.text);
    let total: f64 = phones.iter().map(|p| weight(p)).sum();
    let span = (word.end - word.start).max(0.0);
    let mut out: Vec<Timed> = Vec::new();
    if phones.is_empty() || total <= 0.0 || span <= 0.0 {
        if span > 0.0 {
            out.push(Timed { start: word.start, end: word.end, viseme: Viseme::Aha });
        }
        return out;
    }
    let mut at = word.start;
    for phone in phones {
        let length = span * weight(phone) / total;
        let viseme = viseme_of_phone(phone);
        match out.last_mut() {
            Some(last) if last.viseme == viseme => last.end = at + length,
            _ => out.push(Timed { start: at, end: at + length, viseme }),
        }
        at += length;
    }
    out
}

/// The mouth at host time `t` (the caller adds the lead): its shape, and
/// how far open (1: as drawn) from the voice's loudness. Rests between
/// words and where the voice is silent.
pub fn mouth_at(voice: &VoiceFeatures, t: f64) -> (Viseme, f64) {
    let viseme = if voice.phones.is_empty() {
        let Some(index) = voice.word_at(t) else { return (Viseme::Rest, 1.0) };
        word_visemes(&voice.words[index]).into_iter().find(|v| t >= v.start && t < v.end).map_or(Viseme::Rest, |v| v.viseme)
    } else {
        // The phoneme being spoken; none (a pause) rests.
        let next = voice.phones.partition_point(|p| p.start <= t);
        match next.checked_sub(1).map(|i| &voice.phones[i]) {
            Some(phone) if t < phone.end => viseme_of_phone(&phone.text),
            _ => return (Viseme::Rest, 1.0),
        }
    };
    let loud = voice.amplitude.sample(t);
    if voice.amplitude.values.is_empty() {
        return (viseme, 1.0);
    }
    if viseme.is_open() && loud < 0.03 {
        return (Viseme::Rest, 1.0);
    }
    (viseme, (0.55 + 0.9 * loud).clamp(0.55, 1.35))
}

/// The mouth shapes of `voice` from `from` to `to` at `fps`, each frame's
/// shape sampled `lead` frames ahead: `(frame time, shape)` where it
/// changes. What tools show and tests check.
pub fn track(voice: &VoiceFeatures, from: f64, to: f64, fps: f64, lead: f64) -> Vec<(f64, Viseme)> {
    let mut out: Vec<(f64, Viseme)> = Vec::new();
    let step = 1.0 / fps.max(1.0);
    let mut k = 0usize;
    loop {
        let t = from + k as f64 * step;
        if t > to {
            break;
        }
        let (viseme, _) = mouth_at(voice, t + lead * step);
        if out.last().is_none_or(|(_, v)| *v != viseme) {
            out.push((t, viseme));
        }
        k += 1;
    }
    out
}
