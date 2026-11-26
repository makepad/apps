//! Musical and spoken time in cuts: spans written as
//! `beat(n)`, `section(@name)`, `word(@voice, i)` and `beats(x)` load on a
//! steady grid at the cut's `bpm` and re-place against the host's timing
//! with `CutDoc::with_timing` (the resolver Motion documents use).

use makepad_story_cut::timing::{BeatGrid, Timing};
use makepad_story_cut::*;
use makepad_motion::timing::{Section, SpokenTiming, VoiceTiming};

const REV: &str = "arev_5ab3f998fd4a7b6860a9337b9924be40ad341d12276e341079ce2093539c715f";

fn edit(body: &str, nodes: &str) -> String {
    format!("use mod.flow.*\nuse mod.cut.*\n{body}\nFlow{{\n    kind: @cut\n    output: {{width: 1280 height: 720 fps: 30}}\n    duration: 20.0\n    bpm: 100\n    {nodes}\n}}\n")
}

fn graphic(span: &str) -> String {
    format!("let g = Graphic{{src: asset(\"ast_{}\", \"{REV}\") span: {span}}}\nlet out = CutOutput{{frame: g.out(@frame)}}", "a".repeat(26))
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn a_span_on_the_beat_loads_at_the_edit_tempo_and_follows_the_music() {
    let doc = CutDoc::load(&edit(&graphic("[beat(8), beats(4)]"), "g, out")).unwrap_or_else(|d| panic!("{d:#?}"));
    let span = doc.graphic("g").unwrap().span.unwrap();
    // 100 bpm: a beat is 0.6 s.
    assert!(close(span.start, 4.8) && close(span.length, 2.4), "{span:?}");
    assert_eq!(doc.time_bindings.len(), 1);

    // The music's grid: 120 bpm from 0.5 s.
    let timing = Timing { grid: Some(BeatGrid::steady(120.0, 0.5)), ..Default::default() };
    let (synced, problems) = doc.with_timing(&timing);
    assert!(problems.is_empty(), "{problems:?}");
    let span = synced.graphic("g").unwrap().span.unwrap();
    assert!(close(span.start, 4.5) && close(span.length, 2.0), "{span:?}");
    // The original is untouched.
    assert!(close(doc.graphic("g").unwrap().span.unwrap().start, 4.8));
}

#[test]
fn offsets_sections_and_words_resolve_and_missing_ones_say_so() {
    let doc = CutDoc::load(&edit(&graphic("[section(@drop, 0.25), 3.0]"), "g, out")).unwrap_or_else(|d| panic!("{d:#?}"));
    let (_, problems) = doc.with_timing(&Timing::default());
    assert_eq!(problems.len(), 1, "no sections: the span keeps its place and says why");
    let timing = Timing {
        sections: vec![Section { name: "intro".into(), start: 0.0, end: 8.0 }, Section { name: "drop".into(), start: 8.0, end: 16.0 }],
        ..Default::default()
    };
    let (synced, problems) = doc.with_timing(&timing);
    assert!(problems.is_empty(), "{problems:?}");
    let span = synced.graphic("g").unwrap().span.unwrap();
    assert!(close(span.start, 8.25) && close(span.length, 3.0), "{span:?}");

    let doc = CutDoc::load(&edit(&graphic("[word(@vo, 2, -0.1), 1.5]"), "g, out")).unwrap_or_else(|d| panic!("{d:#?}"));
    let timing = Timing {
        voices: vec![("vo".into(), VoiceTiming { lines: vec![SpokenTiming { start: 1.0, words: vec![1.0, 1.4, 1.9, 2.3], texts: Vec::new() }] })],
        ..Default::default()
    };
    let (synced, problems) = doc.with_timing(&timing);
    assert!(problems.is_empty(), "{problems:?}");
    assert!(close(synced.graphic("g").unwrap().span.unwrap().start, 1.8));
}

#[test]
fn a_bad_time_teaches_the_forms() {
    let diags = CutDoc::load(&edit(&graphic("[\"soon\", 2.0]"), "g, out")).unwrap_err();
    assert!(diags.iter().any(|d| d.message.contains("beat(n)")), "{diags:#?}");
}

/// A deck holds a cut at its end: the frame shown is the last one, so a node
/// whose span runs to the end (a live app's `AppView`) is still on it.
#[test]
fn a_held_end_shows_the_last_frame_and_what_is_on_it() {
    let doc = CutDoc::load(&edit(&graphic("[0.0, 20.0]"), "g, out")).unwrap_or_else(|d| panic!("{d:#?}"));
    let g = doc.graphic("g").unwrap();
    assert!(!g.span.unwrap().contains(20.0), "the span itself is half open");
    assert!(close(doc.shown_time(20.0), 20.0 - 1.0 / 30.0));
    assert!(close(doc.shown_time(25.0), 20.0 - 1.0 / 30.0));
    assert!(close(doc.shown_time(7.5), 7.5));
    assert!(g.span.unwrap().contains(doc.shown_time(20.0)));
}
