//! Animation over time: tracks, loops, staggers, spans, property functions,
//! musical and spoken time, time domains and both signal providers.

use makepad_motion::*;

fn load(body: &str) -> MotionDoc {
    let source = format!("use mod.motion.*; Motion{{size: vec2(100, 100) duration: 8.0 bpm: 120\n layers: [{body}]}}");
    match MotionDoc::load(&source) {
        Ok(doc) => doc,
        Err(diags) => panic!("{}", diags.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")),
    }
}

fn x_at(doc: &MotionDoc, t: f64) -> f64 {
    doc.eval(t).items[0].world.e
}

#[test]
fn sequences_loops_and_ping_pong() {
    let doc = load("Rect{size: vec2(1, 1) fill: #fff x: sequence([[0, 0.5], [10, 1.0], [10, 1.0], [0, 0.5]])}");
    assert_eq!(x_at(&doc, 0.0), 0.0);
    assert_eq!(x_at(&doc, 0.5), 0.0);
    assert_eq!(x_at(&doc, 1.0), 5.0);
    assert_eq!(x_at(&doc, 2.0), 10.0);
    assert_eq!(x_at(&doc, 2.75), 5.0);
    assert_eq!(x_at(&doc, 3.25), 0.0);
    let doc = load("Rect{size: vec2(1, 1) fill: #fff x: repeat(tween(0, 10, 0, 1))}");
    assert!((x_at(&doc, 2.25) - 2.5).abs() < 1e-9);
    let doc = load("Rect{size: vec2(1, 1) fill: #fff x: repeat(tween(0, 10, 0, 1), 1, @pingpong)}");
    assert!((x_at(&doc, 1.25) - 7.5).abs() < 1e-9);
    let doc = load("Rect{size: vec2(1, 1) fill: #fff x: repeat(tween(0, 10, 0, 1), 2, @restart, 2)}");
    assert!((x_at(&doc, 2.5) - 5.0).abs() < 1e-9);
    assert_eq!(x_at(&doc, 7.0), 10.0, "rests where the last cycle ended");
}

#[test]
fn spans_start_a_layers_clock_and_hide_it_outside() {
    let doc = load("Rect{size: vec2(1, 1) fill: #fff span: [2, 3] x: tween(0, 10, 0, 1)}");
    assert!(doc.eval(1.9).items.is_empty());
    assert_eq!(doc.eval(2.5).items[0].world.e, 5.0);
    assert!(doc.eval(5.0).items.is_empty(), "the span is half-open");
}

#[test]
fn group_staggers_shift_each_childs_clock() {
    let doc = load("Group{stagger: stagger(0.5, @reverse) layers: [
        Rect{size: vec2(1, 1) fill: #fff x: tween(0, 10, 0, 1)}
        Rect{size: vec2(1, 1) fill: #fff x: tween(0, 10, 0, 1)}]}");
    let ItemKind::Group { children, .. } = &doc.eval(0.5).items[0].kind else { panic!() };
    assert_eq!(children[0].world.e, 0.0, "reversed: the first child starts last");
    assert_eq!(children[1].world.e, 5.0);
}

#[test]
fn property_functions_see_local_time_the_unit_and_inputs() {
    let doc = MotionDoc::load(r#"use mod.motion.*; Motion{size: vec2(100, 100) duration: 4 inputs: {gain: 3}
        layers: [
            Rect{size: vec2(1, 1) fill: #fff span: [1, 2] x: fn(t) { t * in(@gain) }}
            Text{text: "abc" by: @letter y: fn(t, i) { i * 10 } color: fn(t) { #00ff00 }}
        ]}"#).unwrap();
    assert!(doc.has_functions);
    let frame = doc.eval(1.5);
    assert!(frame.errors.is_empty(), "{:?}", frame.errors);
    assert_eq!(frame.items[0].world.e, 1.5);
    let ItemKind::Text { units, .. } = &frame.items[1].kind else { panic!() };
    assert_eq!(units.iter().map(|u| u.offset[1]).collect::<Vec<_>>(), vec![0.0, 10.0, 20.0]);
    assert_eq!(units[0].color, [0.0, 1.0, 0.0, 1.0]);
    // A failing function keeps the default and says why, at the function.
    let doc = load("Rect{size: vec2(1, 1) fill: #fff x: fn(t) { nope(t) }}");
    let frame = doc.eval(1.0);
    assert_eq!(frame.items[0].world.e, 0.0);
    assert!(frame.errors[0].message.contains("nope"), "{:?}", frame.errors);
}

#[test]
fn musical_time_resolves_against_the_grid_and_resyncs() {
    let doc = load("Rect{size: vec2(1, 1) fill: #fff x: tween(0, 10, beat(2), bar(1, 0)) span: [downbeat(1), beats(8)]}");
    // No grid: the document's 120 bpm, with a warning.
    assert!(doc.warnings.iter().any(|w| w.message.contains("no beat grid")), "{:?}", doc.warnings);
    let LayerKind::Rect { .. } = &doc.layers[0].kind else { panic!() };
    let span = doc.layers[0].span.clone().unwrap();
    assert_eq!((span.start.value, span.length.value), (2.0, 4.0));
    // Keys are on the layer's clock: beat 2 (1 s) is -1 s local, bar 1 (2 s) is 0.
    assert_eq!(x_at(&doc, 2.0), 10.0);
    // Re-synced to a grid at 60 bpm starting at 0.5 s: downbeat(1) = 4.5 s.
    let features = AudioFeatures { music: vec![("m".into(), MusicFeatures { bpm: 60.0, beats: (0..20).map(|b| 0.5 + b as f64).collect(), beats_per_bar: 4, ..Default::default() })], ..Default::default() };
    let (resynced, warnings) = doc.with_context(MotionContext::analyzed(features, None, 0.0)).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let span = resynced.layers[0].span.clone().unwrap();
    assert_eq!((span.start.value, span.length.value), (4.5, 8.0));
    assert!(resynced.eval(4.4).items.is_empty());
    assert_eq!(resynced.eval(4.5).items[0].world.e, 10.0);
}

#[test]
fn unresolvable_references_warn_and_teach() {
    let doc = load("Rect{size: vec2(1, 1) fill: #fff x: tween(0, 10, section(@drop), section(@drop, 1.0))}");
    let text: Vec<String> = doc.warnings.iter().map(|d| d.to_string()).collect();
    assert!(text.iter().any(|t| t.contains("section(@drop) needs the music's sections")), "{text:#?}");
    assert_eq!(x_at(&doc, 0.5), 5.0, "taken as the clock's start plus the offset");
}

#[test]
fn a_beat_pulse_stays_on_the_grid_when_the_clip_moves() {
    let body = "Rect{size: vec2(1, 1) fill: #fff x: fn(t, i, edit) { music(@m).beat_phase(edit) * 100 }}";
    let doc = load(body);
    let features = AudioFeatures::synthetic("a b c", 0.0, 2.0, 120.0, 20.0);
    for start in [0.0, 0.3, 1.1, 2.75] {
        let (moved, _) = doc.with_context(MotionContext::analyzed(features.clone(), None, start)).unwrap();
        for edit_t in [3.0, 3.1, 3.25, 4.4] {
            let x = x_at(&moved, edit_t - start);
            let phase = ((edit_t * 2.0) as f64).fract() * 100.0;
            assert!((x - phase).abs() < 1e-6, "start {start}, edit {edit_t}: {x} vs {phase}");
        }
    }
}

#[test]
fn the_same_document_runs_on_analyzed_and_live_signals() {
    let doc = load("Rect{size: vec2(1, 1) fill: #fff x: fn(t, i, edit) { music(@m).bar_phase(edit) * 100 } y: tween(0, 10, beat(0), beat(4))}");
    let analyzed = doc.with_context(MotionContext::analyzed(AudioFeatures::synthetic("a", 0.0, 2.0, 120.0, 20.0), None, 1.0)).unwrap().0;
    let live = doc.with_context(MotionContext::live(LiveSignals { bpm: 120.0, anchor: 10.0, beat: 20.0, ..Default::default() }, 1.0)).unwrap().0;
    for t in [0.0, 0.4, 1.3, 2.9] {
        let (a, l) = (analyzed.eval(t), live.eval(t));
        assert!((a.items[0].world.e - l.items[0].world.e).abs() < 1e-9, "{t}");
        assert!((a.items[0].world.f - l.items[0].world.f).abs() < 1e-9, "{t}");
    }
}

#[test]
fn voice_bound_text_follows_the_words() {
    let features = AudioFeatures::synthetic("say it on the beat", 1.0, 2.0, 120.0, 8.0);
    let doc = load("Text{source: voice(@vo) show: @spoken highlight: @active highlight_color: #ffcc00 pop: @on_word y: tween(20, 0, 0, 0.2)}");
    let doc = doc.with_context(MotionContext::analyzed(features, None, 0.0)).unwrap().0;
    let frame = doc.eval(1.6);
    let ItemKind::Text { text, units, .. } = &frame.items[0].kind else { panic!() };
    assert_eq!(text, "say it on the beat");
    assert_eq!(units.len(), 5);
    // Word 1 ("it") starts at 1.5: spoken, lit and popping; 2.. upcoming (hidden).
    assert_eq!(units[0].opacity, 1.0);
    assert_eq!(units[1].color, [1.0, 0.8, 0.0, 1.0]);
    assert!(units[1].scale[0] > 1.0);
    assert!(units[1].offset[1] > 0.0 && units[1].offset[1] < 20.0, "rising from its own start");
    assert_eq!(units[2].opacity, 0.0);
    assert_eq!(units[0].color, [1.0, 1.0, 1.0, 1.0], "said, no longer active");
    // Word timing references resolve too.
    let doc = load("Rect{size: vec2(1, 1) fill: #fff span: [word(@vo, 3), 1]}");
    let doc = doc.with_context(MotionContext::analyzed(AudioFeatures::synthetic("say it on the beat", 1.0, 2.0, 120.0, 8.0), None, 0.0)).unwrap().0;
    assert_eq!(doc.layers[0].span.as_ref().unwrap().start.value, 2.5);
}

#[test]
fn eases_include_springs_and_the_standard_set() {
    let doc = load("Rect{size: vec2(1, 1) fill: #fff x: tween(0, 100, 0, 1, @ease_out_back)}");
    assert!((0..20).any(|k| x_at(&doc, k as f64 * 0.05) > 100.0));
    let doc = load("Rect{size: vec2(1, 1) fill: #fff x: tween(0, 100, 0, 2, spring(100, 5))}");
    assert!((0..40).any(|k| x_at(&doc, k as f64 * 0.05) > 110.0));
    assert_eq!(x_at(&doc, 2.0), 100.0);
}

