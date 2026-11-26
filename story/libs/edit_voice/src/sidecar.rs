//! Caption sidecars: every render also writes its captions
//! as SRT and WebVTT, from the same captions it burns in
//! (`CutEval::captions`), in cut seconds.

use makepad_story_cut::Caption;

/// `HH:MM:SS` then `separator` and milliseconds; negative times are 0.
fn stamp(secs: f64, separator: char) -> String {
    let ms = (secs.max(0.0) * 1000.0).round() as u64;
    format!("{:02}:{:02}:{:02}{separator}{:03}", ms / 3_600_000, (ms / 60_000) % 60, (ms / 1000) % 60, ms % 1000)
}

/// Text for a sidecar: a blank line would end the cue, so line breaks
/// become spaces; `-->` would read as timing, so it is spaced.
fn cue_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").replace("-->", "- ->")
}

/// Captions that survive millisecond rounding: a cue whose start and end
/// print the same is left out.
fn cues(captions: &[Caption]) -> impl Iterator<Item = &Caption> {
    captions.iter().filter(|caption| (caption.start.max(0.0) * 1000.0).round() < (caption.end.max(0.0) * 1000.0).round())
}

/// SubRip: numbered cues, `00:00:01,000 --> 00:00:02,500`, a blank line
/// after each. Players read `<i>`, `<b>` and `<font>` in SRT, so angle
/// brackets in the words become look-alikes (‹ ›).
pub fn srt(captions: &[Caption]) -> String {
    let mut out = String::new();
    for (index, caption) in cues(captions).enumerate() {
        let text = cue_text(&caption.text()).replace('<', "‹").replace('>', "›");
        out.push_str(&format!("{}\n{} --> {}\n{}\n\n", index + 1, stamp(caption.start, ','), stamp(caption.end, ','), text));
    }
    out
}

/// WebVTT. With `karaoke`, each word after the first carries its start as
/// a cue timestamp (`<00:00:01.250>word`), which players that support it
/// use to highlight word by word; a stamp is written only when it lies
/// strictly after the cue's start and the previous stamp and before its
/// end, as WebVTT requires. `<`, `>` and `&` are escaped.
pub fn vtt(captions: &[Caption], karaoke: bool) -> String {
    let escape = |text: &str| text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let ms = |secs: f64| (secs.max(0.0) * 1000.0).round() as u64;
    let mut out = String::from("WEBVTT\n\n");
    for (index, caption) in cues(captions).enumerate() {
        let text = if karaoke {
            let mut last = ms(caption.start);
            caption.words.iter().filter_map(|word| {
                let text = escape(&cue_text(&word.text));
                if text.is_empty() {
                    return None;
                }
                let at = ms(word.start);
                if at > last && at < ms(caption.end) {
                    last = at;
                    Some(format!("<{}>{text}", stamp(word.start, '.')))
                } else {
                    Some(text)
                }
            }).collect::<Vec<_>>().join(" ")
        } else {
            escape(&cue_text(&caption.text()))
        };
        out.push_str(&format!("{}\n{} --> {}\n{}\n\n", index + 1, stamp(caption.start, '.'), stamp(caption.end, '.'), text));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_story_cut::Word;

    fn caption(start: f64, end: f64, words: &[(&str, f64)]) -> Caption {
        let words = words.iter().map(|(text, at)| Word { text: text.to_string(), start: *at, end: *at + 0.2 }).collect();
        Caption { start, end, words, provisional: false }
    }

    fn captions() -> Vec<Caption> {
        vec![
            caption(3.0, 4.05, &[("Notes", 3.0), ("starts", 3.35), ("with", 3.85)]),
            caption(3725.5, 3727.0004, &[("A", 3725.5), ("<b>&", 3726.0)]),
        ]
    }

    #[test]
    fn srt_is_exact() {
        assert_eq!(srt(&captions()), "1\n00:00:03,000 --> 00:00:04,050\nNotes starts with\n\n2\n01:02:05,500 --> 01:02:07,000\nA ‹b›&\n\n");
        assert_eq!(srt(&[]), "");
    }

    #[test]
    fn vtt_is_exact() {
        assert_eq!(vtt(&captions(), false), "WEBVTT\n\n1\n00:00:03.000 --> 00:00:04.050\nNotes starts with\n\n2\n01:02:05.500 --> 01:02:07.000\nA &lt;b&gt;&amp;\n\n");
        assert_eq!(
            vtt(&captions()[..1], true),
            "WEBVTT\n\n1\n00:00:03.000 --> 00:00:04.050\nNotes <00:00:03.350>starts <00:00:03.850>with\n\n"
        );
        assert_eq!(vtt(&[], false), "WEBVTT\n\n");
    }

    #[test]
    fn cue_text_never_breaks_a_cue() {
        let odd = Caption { start: 0.0, end: 1.0, words: vec![Word { text: "a\n\nb-->".into(), start: 0.0, end: 1.0 }], provisional: false };
        assert_eq!(srt(&[odd.clone()]), "1\n00:00:00,000 --> 00:00:01,000\na b- -›\n\n");
        assert_eq!(vtt(&[odd], true), "WEBVTT\n\n1\n00:00:00.000 --> 00:00:01.000\na b- -&gt;\n\n");
    }

    #[test]
    fn karaoke_stamps_only_increase_inside_the_cue_and_empty_cues_go() {
        // Equal starts (a zero-length word) and a start at the cue's end
        // get no stamp.
        let caption = Caption {
            start: 1.0, end: 2.0, provisional: false,
            words: vec![("a", 1.0), ("b", 1.5), ("—", 1.5), ("c", 2.0)].into_iter().map(|(text, at)| Word { text: text.into(), start: at, end: at }).collect(),
        };
        assert_eq!(vtt(&[caption], true), "WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.000\na <00:00:01.500>b — c\n\n");
        let tiny = Caption { start: 1.0, end: 1.0004, words: vec![Word { text: "x".into(), start: 1.0, end: 1.0 }], provisional: false };
        assert_eq!(srt(&[tiny.clone()]), "");
        assert_eq!(vtt(&[tiny], false), "WEBVTT\n\n");
    }
}
