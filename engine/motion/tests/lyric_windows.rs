//! Lyric lookups from a plate's own start: a whole song repeats its chorus,
//! and a graphic placed later in the edit must find its own occurrence.

use makepad_motion::*;

const SONG: &str = "we trim each lamp tonight then wait till we trim each lamp tonight";

fn doc(src: &str, offset: f64) -> MotionDoc {
    // One word a second from 1.0 (host seconds), one line.
    let features = AudioFeatures::synthetic(SONG, 1.0, 1.0, 120.0, 30.0);
    MotionDoc::load_with(src, &[], MotionContext::analyzed(features, None, offset)).unwrap_or_else(|d| panic!("{d:?}"))
}

/// The x a Rect is placed at: the value under test.
fn x(doc: &MotionDoc) -> f64 {
    doc.eval(0.0).items[0].world.e
}

#[test]
fn lookups_find_the_nth_occurrence_after_a_second() {
    let at = |expr: &str, offset: f64| x(&doc(&format!("use mod.motion.*; Motion{{size: vec2(100, 100) duration: 5 layers: [Rect{{size: vec2(1, 1) at: vec2({expr}, 0)}}]}}"), offset));
    // A plate at song second 9.0: times are the document's (song - 9).
    // Unchanged: the whole song's first "lamp" (song 4.0 = document -5.0).
    assert_eq!(at("lyric_word(voice(@vo), \"lamp\", 0).start", 9.0), -5.0);
    assert_eq!(at("here()", 9.0), 9.0);
    // From the plate's own start: the second one (song 12.0 = document 3.0).
    assert_eq!(at("lyric_word(voice(@vo), \"lamp\", 0, 0.0).start", 9.0), 3.0);
    assert_eq!(at("lyric_line(voice(@vo), \"lamp\", 0, 5.0)[0].start", 0.0), 1.0, "the same single line");
    // The first line starting at or after a second.
    assert_eq!(at("lyric_line_after(voice(@vo), 0.0)[0].start", 0.0), 1.0);
    assert_eq!(at("lyric_line_after(voice(@vo), 2.0)[0].start", 0.0), 2.0, "no line starts after 1.0: the placeholder at t");
}

/// A document at its song's start reads exactly the song's times (every
/// lyric film spanning its edit is unchanged).
#[test]
fn a_document_at_the_songs_start_reads_the_songs_own_times() {
    let features = AudioFeatures::synthetic(SONG, 1.0, 1.0, 120.0, 30.0);
    let words: Vec<f64> = features.voice("vo").unwrap().words.iter().map(|w| w.start).collect();
    let amplitude = features.voice("vo").unwrap().amplitude.sample(6.3);
    let at = |expr: &str| x(&doc(&format!("use mod.motion.*; Motion{{size: vec2(100, 100) duration: 5 layers: [Rect{{size: vec2(1, 1) at: vec2({expr}, 0)}}]}}"), 0.0));
    for (i, start) in words.iter().enumerate() {
        assert_eq!(at(&format!("voice(@vo).words[{i}].start")), *start);
    }
    assert_eq!(at("lyric_line(voice(@vo), \"tonight\", 1)[4].start"), words[4]);
    assert_eq!(at("voice(@vo).amplitude(6.3)"), amplitude);
    assert_eq!(at("voice(@vo).word_at(6.3)"), 5.0);
}

/// Methods of time take the host's (edit) time, the third argument of a
/// property function: a word (or a beat: `music(@m).beat_phase(edit)`) read
/// with `edit` stays on the song wherever the plate is placed.
#[test]
fn methods_of_time_take_the_edit_time_wherever_the_plate_is() {
    let src = "use mod.motion.*; Motion{size: vec2(100, 100) duration: 5 layers: [Rect{size: vec2(1, 1) x: fn(t, i, edit) { voice(@vo).word_at(edit) } y: fn(t, i, edit) { voice(@vo).progress(3, edit) + 10.0 * (edit - t) }}]}";
    for offset in [0.0, 13.5] {
        let d = doc(src, offset);
        let w = d.eval(0.2).items[0].world;
        // Song second offset + 0.2: word 13 when at 13.7 (from 1.0, 1 a second), word -1 (nil 0) before.
        let host = offset + 0.2;
        let word = if host >= 1.0 { (host - 1.0).floor().min(13.0) } else { 0.0 };
        assert_eq!(w.e, word, "offset {offset}");
        // Word 3 ("lamp", song 4.0 to 5.0): upcoming at song 0.2, done at 13.7.
        let progress = if host < 4.0 { -1.0 } else { 2.0 };
        assert!((w.f - (progress + 10.0 * offset)).abs() < 1e-6, "offset {offset}: {}", w.f);
    }
}

/// A plate starting at song second 13.5: its words are on its own clock, so
/// `t >= W.start` turns true exactly when the word is sung.
#[test]
fn a_plate_starting_at_13_5_reads_its_words_on_its_own_clock() {
    let song = "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen";
    let features = AudioFeatures::synthetic(song, 1.0, 1.0, 120.0, 30.0);
    let src = "use mod.motion.*; let W = lyric_word(voice(@vo), \"fifteen\", 0)\nMotion{size: vec2(100, 100) duration: 5 layers: [Rect{size: vec2(1, 1) at: fn(t) { vec2(W.start, if t >= W.start { 1.0 } else { 0.0 }) }}]}";
    let d = MotionDoc::load_with(src, &[], MotionContext::analyzed(features, None, 13.5)).unwrap_or_else(|e| panic!("{e:?}"));
    let at = |t: f64| { let w = d.eval(t).items[0].world; (w.e, w.f) };
    // "fifteen" is sung at song 15.0 = the plate's 1.5.
    assert_eq!(at(0.0).0, 1.5);
    assert_eq!(at(1.49).1, 0.0);
    assert_eq!(at(1.5).1, 1.0);
}

#[test]
fn a_word_time_can_count_from_the_documents_start() {
    // A plate placed at host 9.0: its span on word(@vo, "lamp") is the first
    // "lamp" (host 4.0 = document -5.0) unless it asks for the one after its start.
    let span = |call: &str| {
        let d = doc(&format!("use mod.motion.*; Motion{{size: vec2(100, 100) duration: 8 layers: [Rect{{size: vec2(1, 1) span: [{call}, 1.0]}}]}}"), 9.0);
        d.layers[0].span.as_ref().map(|s| s.start.value).unwrap()
    };
    assert_eq!(span("word(@vo, \"lamp\")"), -5.0);
    assert_eq!(span("word(@vo, \"lamp\", 0.0, 0, 0.0)"), 3.0);
}
