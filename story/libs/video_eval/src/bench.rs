//! The fixed benchmark: briefs a person would type into CUT's Generate
//! panel, with the format and length each one asks for and the parts the
//! result must contain. Changing a brief changes every score measured on
//! it: add new cases with new ids instead of editing old ones.

use makepad_micro_serde::*;

/// Output formats the benchmark uses.
pub const VERTICAL: (u32, u32) = (1080, 1920);
pub const WIDE: (u32, u32) = (1920, 1080);
pub const SQUARE: (u32, u32) = (1080, 1080);

#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct Case {
    pub id: String,
    /// `benchmark` (the comparable 20) or `acceptance` (pass/fail targets,
    /// judged against a checklist; never mixed into benchmark means).
    pub set: String,
    pub category: String,
    pub prompt: String,
    pub width: u32,
    pub height: u32,
    /// The frame rate asked for: 60 for every case (user rule, 2026-09-30:
    /// generated videos run at 60 fps or more; runs before round 5 asked 30).
    pub fps: f64,
    pub length_s: f64,
    /// The brief asks for a voiceover (and so for its subtitles).
    pub voice: bool,
    pub subtitles: bool,
    /// A registry app whose app run the cut must use as a clip.
    pub app: Option<String>,
    /// The brief asks for a chart driven by numbers.
    pub chart: bool,
    /// At least this many motion graphics placed in the cut.
    pub min_graphics: u32,
    /// The brief is made from a given song and its lyrics (`run --song
    /// FILE --lyrics FILE`; the files stay local, never in the repo).
    pub song: bool,
}

impl Case {
    pub fn aspect_label(&self) -> &'static str {
        match (self.width, self.height) {
            (w, h) if w * 16 == h * 9 => "9:16",
            (w, h) if w * 9 == h * 16 => "16:9",
            (w, h) if w == h => "1:1",
            (w, h) if w * 5 == h * 4 => "4:5",
            _ => "custom",
        }
    }
}

struct Spec {
    id: &'static str,
    category: &'static str,
    size: (u32, u32),
    length_s: f64,
    voice: bool,
    subtitles: bool,
    app: Option<&'static str>,
    chart: bool,
    min_graphics: u32,
    prompt: &'static str,
}

const SPECS: &[Spec] = &[
    Spec {
        id: "launch-notes-vertical",
        category: "launch",
        size: VERTICAL,
        length_s: 30.0,
        voice: true,
        subtitles: true,
        app: None,
        chart: false,
        min_graphics: 3,
        prompt: "30 s launch video for Notes, 9:16, bold kinetic type, our brand colours (ink #0b0d12, accent #ff5a36, paper #f4f1ea), voiceover. Hook in the first two seconds, three benefits, end card with \"Notes — write it down, instantly\".",
    },
    Spec {
        id: "launch-stage-wide",
        category: "launch",
        size: WIDE,
        length_s: 20.0,
        voice: false,
        subtitles: false,
        app: None,
        chart: false,
        min_graphics: 3,
        prompt: "20 s teaser for Stage, a live VJ and video editing app, 16:9. No voice, music-driven energy: fast word punches (\"Mix.\" \"Cut.\" \"Perform.\"), a logo reveal drawn with strokes, end on the wordmark. Dark background, electric cyan #00e5ff accents.",
    },
    Spec {
        id: "feature-explainer-wide",
        category: "explainer",
        size: WIDE,
        length_s: 30.0,
        voice: true,
        subtitles: true,
        app: None,
        chart: false,
        min_graphics: 4,
        prompt: "30 s feature explainer for \"Smart folders\" in Files, 16:9, calm and clear. Voiceover explains the problem (files everywhere), the idea (folders that fill themselves by rules), and three example rules shown as animated cards. Light theme, one accent colour.",
    },
    Spec {
        id: "feature-explainer-vertical",
        category: "explainer",
        size: VERTICAL,
        length_s: 25.0,
        voice: true,
        subtitles: true,
        app: None,
        chart: false,
        min_graphics: 3,
        prompt: "25 s vertical explainer: \"How beat sync works\" in Stage. Show a waveform, beat markers popping in on the beat, and clips snapping to them. Friendly voiceover, big captions, keep everything inside the Reels safe zone.",
    },
    Spec {
        id: "kinetic-quote",
        category: "kinetic type",
        size: SQUARE,
        length_s: 12.0,
        voice: false,
        subtitles: false,
        app: None,
        chart: false,
        min_graphics: 1,
        prompt: "12 s kinetic typography quote, square 1:1: \"Simplicity is the ultimate sophistication.\" — Leonardo da Vinci. Word by word, the key word \"simplicity\" gets the biggest moment, elegant serif, cream on deep green.",
    },
    Spec {
        id: "kinetic-manifesto",
        category: "kinetic type",
        size: VERTICAL,
        length_s: 18.0,
        voice: false,
        subtitles: false,
        app: None,
        chart: false,
        min_graphics: 3,
        prompt: "18 s manifesto in kinetic type, 9:16: \"We build tools. Not traps. Your files stay yours. Your data stays home. Software that respects you.\" Each sentence its own scene, letter cascades and split reveals, black and white with one red highlight word per scene.",
    },
    Spec {
        id: "data-story-bars",
        category: "data story",
        size: WIDE,
        length_s: 20.0,
        voice: true,
        subtitles: true,
        app: None,
        chart: true,
        min_graphics: 3,
        prompt: "20 s data story, 16:9: \"Our build got 3x faster\". Animated bar chart of clean build times: January 96 s, April 61 s, July 38 s, September 31 s, bars growing in order with count-up labels, then the headline number \"3.1x\" big. Short voiceover.",
    },
    Spec {
        id: "data-story-donut-vertical",
        category: "data story",
        size: VERTICAL,
        length_s: 15.0,
        voice: false,
        subtitles: false,
        app: None,
        chart: true,
        min_graphics: 2,
        prompt: "15 s vertical data graphic: where a musician's streaming revenue goes. Donut chart that draws on: platform 30 %, label 45 %, artist 16 %, publishing 9 %, then the artist slice pulls out and zooms with \"16 %\" counting up. Minimal, bold.",
    },
    Spec {
        id: "app-demo-calculator",
        category: "app demo",
        size: WIDE,
        length_s: 30.0,
        voice: true,
        subtitles: true,
        app: Some("calculator"),
        chart: false,
        min_graphics: 3,
        prompt: "30 s product demo of the Calculator app, 16:9: record an app run that types 12 × 7 and 1234 ÷ 4, show it with a title card, callout bubbles pointing at the keys, a keystroke badge for each press, and an end card. Voiceover narrates each step.",
    },
    Spec {
        id: "app-demo-notes-vertical",
        category: "app demo",
        size: VERTICAL,
        length_s: 25.0,
        voice: true,
        subtitles: true,
        app: Some("notes"),
        chart: false,
        min_graphics: 3,
        prompt: "25 s vertical demo of the Notes app for TikTok: an app run that creates a note and types a shopping list, reframed to 9:16 following the typing, with a hook title, highlight boxes on what matters and a CTA end card.",
    },
    Spec {
        id: "lyric-video",
        category: "lyric video",
        size: WIDE,
        length_s: 20.0,
        voice: false,
        subtitles: false,
        app: None,
        chart: false,
        min_graphics: 2,
        prompt: "20 s lyric video, 16:9, for a track from the library that has timed lyrics (pick one). Lines appear on the beat with word-by-word highlights, a background of slow morphing blobs in the song's mood colours.",
    },
    Spec {
        id: "lyric-video-vertical",
        category: "lyric video",
        size: VERTICAL,
        length_s: 15.0,
        voice: false,
        subtitles: false,
        app: None,
        chart: false,
        min_graphics: 2,
        prompt: "15 s vertical lyric snippet for Reels from any library track with lyrics: the chorus only, huge words punching in on the beat, one word per beat, high contrast.",
    },
    Spec {
        id: "event-promo",
        category: "event promo",
        size: VERTICAL,
        length_s: 15.0,
        voice: false,
        subtitles: false,
        app: None,
        chart: false,
        min_graphics: 3,
        prompt: "15 s event promo, 9:16: \"Riverside Makers Night — 14 November, 19:00, The Old Pump House\". Countdown energy, date and venue as a lower third, grid/dot background, end card with \"RSVP at makepad.nl\".",
    },
    Spec {
        id: "event-promo-wide",
        category: "event promo",
        size: WIDE,
        length_s: 12.0,
        voice: false,
        subtitles: false,
        app: None,
        chart: false,
        min_graphics: 2,
        prompt: "12 s widescreen bumper for a live DJ night \"NIGHT SHIFT\": strobing stripe wipes between the title, the line-up (Aya, K-Lo, Marten) and the date \"Sat 7 Dec\". Neon magenta and black.",
    },
    Spec {
        id: "lower-third-interview",
        category: "titles",
        size: WIDE,
        length_s: 10.0,
        voice: false,
        subtitles: false,
        app: None,
        chart: false,
        min_graphics: 1,
        prompt: "10 s clean editorial lower third over a neutral background for an interview: \"Jordan Vale — Founder, Northwind Studio\", slides in at 1 s, holds, leaves at 8 s. Tasteful, broadcast-safe margins.",
    },
    Spec {
        id: "title-sequence",
        category: "titles",
        size: WIDE,
        length_s: 15.0,
        voice: false,
        subtitles: false,
        app: None,
        chart: false,
        min_graphics: 2,
        prompt: "15 s cinematic opening title sequence for a documentary called \"THE LAST FRAME\": slow mask wipes, generous letter spacing, film grain, one line of credits \"a film by Mira Hale\".",
    },
    Spec {
        id: "social-tip",
        category: "social",
        size: VERTICAL,
        length_s: 20.0,
        voice: true,
        subtitles: true,
        app: None,
        chart: false,
        min_graphics: 3,
        prompt: "20 s vertical \"3 keyboard shortcuts that save you an hour a week\" tip video: each shortcut as a keystroke badge animation (cmd-K, cmd-shift-P, cmd-T) with a one-line explanation, step counter 1/3 2/3 3/3, energetic voiceover.",
    },
    Spec {
        id: "logo-reveal",
        category: "shape motion",
        size: SQUARE,
        length_s: 6.0,
        voice: false,
        subtitles: false,
        app: None,
        chart: false,
        min_graphics: 1,
        prompt: "6 s logo reveal, square: a hexagon outline draws on, fills, the wordmark \"hexa\" types in beside it, subtle icon burst at the end. Use SVG path draw-on and an overshoot settle.",
    },
    Spec {
        id: "product-changelog",
        category: "explainer",
        size: WIDE,
        length_s: 25.0,
        voice: false,
        subtitles: false,
        app: None,
        chart: false,
        min_graphics: 4,
        prompt: "25 s \"What's new in 2.0\" changelog video, 16:9, no voice: five features as animated cards with icons (Offline sync, Dark mode, 3x faster search, Shared folders, Version history), progress bar across the top, end card with version number.",
    },
    Spec {
        id: "testimonial-card",
        category: "social",
        size: (1080, 1350),
        length_s: 12.0,
        voice: false,
        subtitles: false,
        app: None,
        chart: false,
        min_graphics: 2,
        prompt: "12 s 4:5 customer testimonial card for Instagram: a five-star row filling in, the quote \"I cut my editing time in half.\" appearing phrase by phrase, name \"Jonas, video producer\" as a small caption, brand accent #7c5cff.",
    },
];

/// Every benchmark case, in a fixed order.
pub fn cases() -> Vec<Case> {
    SPECS
        .iter()
        .map(|s| Case {
            id: s.id.to_string(),
            set: "benchmark".to_string(),
            category: s.category.to_string(),
            prompt: s.prompt.to_string(),
            width: s.size.0,
            height: s.size.1,
            fps: 60.0,
            length_s: s.length_s,
            voice: s.voice,
            subtitles: s.subtitles,
            app: s.app.map(str::to_string),
            chart: s.chart,
            min_graphics: s.min_graphics,
            song: false,
        })
        .collect()
}

/// The acceptance set (design/REF-SYNTHESIS.md §2): one prompt makes a
/// 15 s 16:9 motion-design showreel with code-made music and the 30 s
/// 9:16 Short around it. Judged against the acceptance checklist in the
/// rubric and run three times (`--repeat 3`: 3/3 must finish).
pub fn acceptance() -> Vec<Case> {
    vec![Case {
        id: "showreel-short".into(),
        set: "acceptance".into(),
        category: "acceptance".into(),
        prompt: "Make a 15 second motion-design showreel, 16:9 (1920x1080), as its own edit, with music you make yourself in code at a set BPM, and then the 30 second vertical Short (9:16) around it. The showreel has at least 8 chapters, each named after a motion-design skill (easing, squash & stretch, kinetic type, particles, patterns, 3D, data, transitions...), each with one hero element in its own colour world; a persistent HUD (chapter counter, BPM, timecode, corner brackets); chapter cuts on the beat; one montage of one word per eighth note; every transition motivated by the outgoing element (it grows into, dives into or collapses into the next); and it ends on a logo sting that collapses to a point. The Short places the showreel as a 16:9 inset between a headline band above and a meta band below with a live progress bar or counter; all type stays between 8% and 74% of the frame height, decoration only in the platform UI bands.".into(),
        width: 1080,
        height: 1920,
        fps: 60.0,
        length_s: 30.0,
        voice: false,
        subtitles: false,
        app: None,
        chart: false,
        min_graphics: 3,
        song: false,
    },
    Case {
        // design/REF-SYNTHESIS.md §2 (primary) and the three.js lyric film,
        // REF-threejs-2104029135790072196.md §2. `length_s` is the excerpt
        // the song input is cut to (a 2–3 min film also passes).
        id: "lyric-film".into(),
        set: "acceptance".into(),
        category: "acceptance".into(),
        prompt: "Given this song and its lyrics, make a lyric film in the three.js reference's grammar: every sung word on its syllable with the current word in the accent colour; a new set for every sung line, each a researched visual pun on its line; one line-art world (wireframe, engineering diagram) with camera moves as the transitions between sets, hard cuts only on chorus flashes; one accent colour on near-black, inverting to paper or full accent on the choruses; a running device that recurs each section and escalates (a meter or a number); tiny mono footnotes as punchlines; and an absurd escalation for the outro. Use the whole song if it is 2–3 minutes, else the given excerpt.".into(),
        width: 1920,
        height: 1080,
        fps: 60.0,
        length_s: 60.0,
        voice: false,
        subtitles: false,
        app: None,
        chart: false,
        min_graphics: 3,
        song: true,
    }]
}

/// Cases added after the baseline, in their own set so the 20 benchmark
/// means stay comparable across rounds (`--cases extra`).
pub fn extra() -> Vec<Case> {
    vec![Case {
        id: "showreel-resume".into(),
        set: "extra".into(),
        category: "showreel".into(),
        // Verbatim from the reference video REF-Ac7SuDYTQDc.
        prompt: "make a dynamic 15-second motion graphics video that shows what an incredible motion designer you are, like it's your showreel for a résumé.".into(),
        width: 1920,
        height: 1080,
        fps: 60.0,
        length_s: 15.0,
        voice: false,
        subtitles: false,
        app: None,
        chart: false,
        min_graphics: 3,
        song: false,
    }]
}

/// Every case of every set, for lookups by id.
pub fn all_cases() -> Vec<Case> {
    let mut all = cases();
    all.extend(extra());
    all.extend(acceptance());
    all
}

/// The cases named by a comma-separated list of ids, categories or sets.
/// `all` or empty means the 20 benchmark cases; the acceptance set is
/// chosen by name (`acceptance`) or id. An unknown name is an error that
/// lists the known ids.
pub fn select(filter: &str) -> Result<Vec<Case>, String> {
    if filter.is_empty() || filter == "all" {
        return Ok(cases());
    }
    let all = all_cases();
    let mut out = Vec::new();
    for id in filter.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let mut matched: Vec<&Case> = all.iter().filter(|c| c.id == id).collect();
        if matched.is_empty() {
            matched = all.iter().filter(|c| c.category == id || c.set == id).collect();
        }
        if matched.is_empty() {
            let known: Vec<_> = all.iter().map(|c| c.id.as_str()).collect();
            return Err(format!("unknown case or category `{id}`; cases: {}", known.join(", ")));
        }
        for case in matched {
            if !out.iter().any(|o: &Case| o.id == case.id) {
                out.push(case.clone());
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_kebab() {
        let all = cases();
        assert!(all.len() >= 20);
        for (i, c) in all.iter().enumerate() {
            assert!(c.id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'), "{}", c.id);
            assert!(!all[..i].iter().any(|o| o.id == c.id), "duplicate {}", c.id);
            assert!(c.length_s > 0.0 && c.width > 0 && c.height > 0);
            if c.voice {
                assert!(c.subtitles, "{}: a voiceover brief also asks for subtitles", c.id);
            }
        }
    }

    #[test]
    fn select_by_id_and_category() {
        assert_eq!(select("kinetic-quote").unwrap().len(), 1);
        let demos = select("app demo").unwrap();
        assert!(demos.len() == 2 && demos.iter().all(|c| c.app.is_some()));
        assert_eq!(select("").unwrap().len(), cases().len());
        assert!(select("nope").unwrap_err().contains("launch-notes-vertical"));
    }

    #[test]
    fn acceptance_is_its_own_set() {
        assert!(select("").unwrap().iter().all(|c| c.set == "benchmark"));
        let acc = select("acceptance").unwrap();
        assert_eq!(acc.len(), 2);
        assert!(acc[1].song && !acc[0].song);
        assert_eq!((acc[0].width, acc[0].height, acc[0].length_s), (1080, 1920, 30.0));
        assert_eq!(select("showreel-short").unwrap()[0].set, "acceptance");
        assert_eq!(select("extra").unwrap()[0].id, "showreel-resume");
    }

    #[test]
    fn every_case_asks_sixty_fps() {
        assert!(all_cases().iter().all(|c| c.fps >= 60.0));
    }

    #[test]
    fn aspect_labels() {
        let all = cases();
        let label = |id: &str| all.iter().find(|c| c.id == id).unwrap().aspect_label();
        assert_eq!(label("launch-notes-vertical"), "9:16");
        assert_eq!(label("launch-stage-wide"), "16:9");
        assert_eq!(label("kinetic-quote"), "1:1");
        assert_eq!(label("testimonial-card"), "4:5");
    }
}
