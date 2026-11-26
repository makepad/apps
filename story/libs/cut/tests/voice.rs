//! Audio parameters, voiceovers and subtitles in the model.

use makepad_story_cut::audio::db_to_gain;
use makepad_story_cut::*;
use std::collections::HashMap;

const VOICE: &str = include_str!("fixtures/voice.splash");
const ASSET: &str = "ast_yqzlg4xa4mbgpzs6e2qsuqwhsu";
const REV: &str = "arev_5ab3f998fd4a7b6860a9337b9924be40ad341d12276e341079ce2093539c715f";
const RUN: &str = "run(\"ast_fklvc3bvjnuijdg33d2uuitkbi\", \"arev_e2e0b2ddc168547400e68147bf9e0161866f9cad405c9f94a2a47a5dcfe955ac\", \"ast_rbzhbuglyvqk6nprgjwvl2o33q\", \"arev_925b7b93f54e7cfa717f5f213568db1fdaea73ef483fc3ebbad2ea042a533bd1\")";

fn load(source: &str) -> CutDoc {
    CutDoc::load(source).unwrap_or_else(|diags| panic!("{diags:#?}"))
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn edit(body: &str, nodes: &str) -> String {
    format!("use mod.flow.*\nuse mod.cut.*\n{body}\nFlow{{\n    kind: @cut\n    output: {{width: 1080 height: 1920 fps: 30}}\n    duration: 16.0\n    {nodes}\n}}\n")
}

fn take() -> AssetRef {
    AssetRef { id: "ast_aaaaaaaaaaaaaaaaaaaaaaaaaa".into(), rev: "arev_1111111111111111111111111111111111111111111111111111111111111111".into() }
}

fn word(text: &str, start: f64, end: f64) -> Word {
    Word { text: text.into(), start, end }
}

/// The fixture with its run's marks: `new_note` at 3 s of the render (edit
/// 3 s through the clip's `in`), `typing` at 6 s.
fn eval(doc: &CutDoc) -> CutEval<'_> {
    let mut eval = CutEval::new(doc);
    let CutNodeKind::Clip { src: MediaRef::Run(run), .. } = &doc.node("demo").unwrap().kind else { panic!() };
    eval.set_marks(run.clone(), HashMap::from([("new_note".to_string(), 3.0), ("typing".to_string(), 6.0)]));
    eval.set_take(take(), TakeInfo {
        duration: 2.0,
        words: vec![word("Notes", 0.0, 0.3), word("starts", 0.35, 0.8), word("with", 0.85, 1.0), word("one", 1.05, 1.3), word("tap.", 1.35, 1.8)],
        phones: vec![word("n", 0.0, 0.1), word("oʊ", 0.1, 0.3)],
    });
    eval
}

#[test]
fn the_voiceover_loads_with_its_lines_run_and_loudness() {
    let doc = load(VOICE);
    assert_eq!(doc.output.loudness, Some(-14.0));
    let CutNodeKind::VoiceOver { voice, lines, run, shape, .. } = &doc.node("voice").unwrap().kind else { panic!() };
    assert_eq!(voice, &VoiceSpec::default());
    assert_eq!(lines.iter().map(|line| line.text.as_str()).collect::<Vec<_>>(), ["Notes starts with one tap.", "Type, and it saves as you go.", "That's it."]);
    assert_eq!(lines[1].at, SourceTime::Marker { name: "typing".into(), offset: 0.2 });
    assert_eq!(lines[2].at, SourceTime::Seconds(13.5));
    assert_eq!((lines[0].take.clone(), lines[1].take.clone()), (Some(take()), None));
    assert!(run.is_some(), "the one run the clips play");
    assert_eq!(shape.fade_in, 0.01);
    let CutNodeKind::AudioClip { shape, .. } = &doc.node("music").unwrap().kind else { panic!() };
    assert_eq!((shape.fade_in, shape.fade_out), (1.0, 2.0));
    assert_eq!(shape.duck, Some(Duck { under: vec!["voice".into()], depth_db: -12.0, attack: 0.1, release: 0.5 }));
    let CutNodeKind::Subtitles { source, style, anchor, safe, max_words, transcript, .. } = &doc.node("subs").unwrap().kind else { panic!() };
    assert_eq!((source.as_ref().unwrap().node.as_str(), style.as_str(), *anchor, *safe, *max_words, transcript), ("voice", "karaoke/pop", [0.5, 0.72], SafeZone::Reels, 4, &None));
    let CutNodeKind::Clip { audio, .. } = &doc.node("demo").unwrap().kind else { panic!() };
    assert!(!audio, "an app run's own sound is off unless wired");
    // Voice lines' markers are cut points of the run too.
    assert_eq!(doc.markers().iter().map(|m| (m.node.as_str(), m.name.as_str())).collect::<Vec<_>>(), [("demo", "new_note"), ("voice", "new_note"), ("voice", "typing")]);
    assert_eq!(nodes::animatable("VoiceOver", "gain"), true);
    assert_eq!(nodes::animatable("Subtitles", "font_size"), true);
    assert_eq!(nodes::animatable("Clip", "pan"), true);
}

#[test]
fn dependencies_include_takes_and_transcripts() {
    let doc = load(VOICE);
    let deps = dependencies(&doc);
    assert_eq!(deps.len(), 4, "{deps:#?}");
    assert!(matches!(&deps[0], Dep::AppRun(_)) && matches!(&deps[1], Dep::Render { .. }));
    assert_eq!(deps[3], Dep::Asset(take()));
    let body = format!(
        "let m = AudioClip{{src: asset(\"{ASSET}\", \"{REV}\")}}\nlet s = Subtitles{{source: m.out(@audio) transcript: asset(\"ast_bbbbbbbbbbbbbbbbbbbbbbbbba\", \"{REV}\")}}\nlet out = CutOutput{{frame: s.out(@frame)}}"
    );
    let doc = load(&edit(&body, "m, s, out"));
    assert_eq!(dependencies(&doc)[1], Dep::Asset(AssetRef { id: "ast_bbbbbbbbbbbbbbbbbbbbbbbbba".into(), rev: REV.into() }));
}

#[test]
fn lines_are_placed_at_their_markers_and_takes() {
    let doc = load(VOICE);
    let eval = eval(&doc);
    let lines = eval.voice_lines("voice");
    let placed = lines.iter().map(|line| (line.index, line.start, line.end, line.sound_end, line.timed)).collect::<Vec<_>>();
    // Line 0 at the render's 3 s (the clip starts at 2 s showing its 2 s),
    // a 2 s take whose last word ends at 1.8 s; line 1 at 6.2 s, estimated 7 × 0.4 s; line 2 at
    // 13.5 s, estimated, sounding to the cut's end.
    assert_eq!(placed.len(), 3);
    assert_eq!((placed[0].0, placed[0].1, placed[0].2, placed[0].3, placed[0].4), (0, 3.0, 4.8, 5.0, true));
    assert!(lines[0].provisional, "the fixture's key is a placeholder: stale");
    assert!(lines[0].phones.is_empty(), "a stale take's phones would not match its words");
    assert!(lines.iter().all(|line| !line.cut));
    assert!(placed[1].0 == 1 && close(placed[1].1, 6.2) && close(placed[1].2, 9.0) && close(placed[1].3, 13.5) && !placed[1].4);
    assert!(placed[2].0 == 2 && close(placed[2].2, 14.3) && placed[2].3 == 16.0);
    assert_eq!(lines[0].words[1], word("starts", 3.35, 3.8));
    // Without the run's marks only the timed line is placed.
    let bare = CutEval::new(&doc);
    assert_eq!(bare.voice_lines("voice").iter().map(|line| line.index).collect::<Vec<_>>(), [2]);
    // A take not measured yet sounds until the next line.
    let mut unmeasured = CutEval::new(&doc);
    let CutNodeKind::Clip { src: MediaRef::Run(run), .. } = &doc.node("demo").unwrap().kind else { panic!() };
    unmeasured.set_marks(run.clone(), HashMap::from([("new_note".to_string(), 3.0), ("typing".to_string(), 6.0)]));
    let first = &unmeasured.voice_lines("voice")[0];
    assert!(close(first.end, 5.0) && close(first.sound_end, 6.2) && !first.timed);
}

#[test]
fn music_fades_and_ducks_under_the_voice() {
    let doc = load(VOICE);
    let eval = eval(&doc);
    let music = doc.node("music").unwrap();
    let gain = |t: f64| eval.gain(music, t);
    assert!(close(gain(0.5), 0.4), "half way through the fade in");
    assert!(close(gain(2.5), 0.8), "full before the voice");
    assert!(close(gain(2.95), 0.8 * db_to_gain(-6.0)), "half way down the attack");
    assert!(close(gain(4.0), 0.8 * db_to_gain(-12.0)), "ducked while the voice speaks");
    assert!(close(gain(5.05), 0.8 * db_to_gain(-6.0)), "half way up the release after the last word");
    assert!(close(gain(5.8), 0.8));
    assert!(close(gain(7.0), 0.8), "a line without a take is silence: no duck");
    assert!(close(gain(15.0), 0.4), "half way through the fade out");
    assert!(close(gain(14.55), 0.8 * 0.725), "fading out; the last line has no take");
    assert_eq!(gain(16.0), 0.0);
    // The voice's own fade is per line.
    let voice = doc.node("voice").unwrap();
    assert!(close(eval.gain(voice, 3.005), 0.5));
    assert!(close(eval.gain(voice, 6.205), 0.5));
    assert!(close(eval.gain(voice, 4.0), 1.0));
    assert_eq!(eval.gain(voice, 5.5), 0.0, "between lines the voice is silent");
}

#[test]
fn the_audio_plan_places_each_take_at_its_line() {
    let doc = load(VOICE);
    let eval = eval(&doc);
    let plan = eval.audio_plan(3.5, 4.5);
    assert_eq!(plan.segments.len(), 2);
    let (music, voice) = (&plan.segments[0], &plan.segments[1]);
    assert_eq!((music.node.as_str(), music.entry, music.keyed, music.line), ("music", 0, true, None));
    assert_eq!((voice.node.as_str(), voice.entry, voice.media.clone(), voice.line, voice.speed), ("voice", 1, MediaRef::Asset(take()), Some(0), 1.0));
    assert!(close(voice.source_t0.unwrap(), 0.5) && voice.keyed);
    assert_eq!(voice.source_ticks, Some(5_000_000), "0.5 s of the take in 10 MHz ticks");
    // The take ends at 5 s; lines without a take play nothing.
    let plan = eval.audio_plan(4.5, 7.0);
    let voice = plan.segments.iter().find(|segment| segment.node == "voice").unwrap();
    assert_eq!((voice.t0, voice.t1), (4.5, 5.0));
    assert_eq!(plan.segments.iter().filter(|segment| segment.node == "voice").count(), 1);
}

#[test]
fn subtitles_show_the_voice_script_word_by_word() {
    let doc = load(VOICE);
    let eval = eval(&doc);
    let captions = eval.captions("subs");
    assert_eq!(captions.iter().map(Caption::text).collect::<Vec<_>>(), ["Notes starts with", "one tap.", "Type, and it saves", "as you go.", "That's it."]);
    assert!(close(captions[0].end, 4.05), "cut where the next caption starts");
    assert!(close(captions[1].end, 5.3), "held half a second");
    let frames = eval.subtitle_plan(3.5);
    assert_eq!(frames.len(), 1);
    let frame = &frames[0];
    assert_eq!((frame.text.as_str(), frame.karaoke.caption, frame.karaoke.active), ("Notes starts with", 0, Some(1)));
    assert!(close(frame.karaoke.highlight, 1.0 + 0.15 / 0.45));
    // Reels hides the bottom 35%: the caption rises above it.
    assert!(close(frame.anchor[1], 0.65 - 64.0 * 1.3 * 0.5 / 1920.0) && frame.anchor[0] == 0.5);
    assert!(eval.subtitle_plan(5.5).is_empty(), "nothing between captions");
    // Burned in as a Text op of the stack.
    let plan = eval.frame_plan(3.5);
    let op = plan.ops.iter().find(|op| op.node == "subs").unwrap();
    let DrawKind::Text { text, position, font_size, karaoke: Some(karaoke), style, .. } = &op.kind else { panic!("{op:#?}") };
    assert_eq!((text.as_str(), *font_size, style.as_str(), karaoke.words.len()), ("Notes starts with", 64.0, "karaoke/pop", 3));
    assert!(close(position[1], frame.anchor[1] * 1920.0));
    assert!(eval.frame_plan(5.5).ops.iter().all(|op| op.node != "subs"));
}

#[test]
fn transcripts_caption_sources_without_a_script() {
    let body = format!(
        "let m = AudioClip{{src: asset(\"{ASSET}\", \"{REV}\") in: 10.0 span: [2.0, 5.0]}}\nlet s = Subtitles{{source: m.out(@audio) transcript: asset(\"ast_bbbbbbbbbbbbbbbbbbbbbbbbba\", \"{REV}\") max_words: 2}}\nlet out = CutOutput{{frame: s.out(@frame)}}"
    );
    let doc = load(&edit(&body, "m, s, out"));
    let mut eval = CutEval::new(&doc);
    assert!(eval.captions("s").is_empty(), "no transcript yet");
    let lines = vec![
        SpokenLine { start: 10.5, end: 11.4, words: vec![word("hello", 10.5, 10.8), word("there", 10.9, 11.4)] },
        SpokenLine { start: 20.0, end: 20.5, words: vec![word("gone", 20.0, 20.5)] },
    ];
    eval.set_transcript(AssetRef { id: "ast_bbbbbbbbbbbbbbbbbbbbbbbbba".into(), rev: REV.into() }, lines);
    let captions = eval.captions("s");
    assert_eq!(captions.len(), 1, "a word past the clip's span is not shown");
    let times = captions[0].words.iter().map(|word| (word.text.as_str(), word.start, word.end)).collect::<Vec<_>>();
    let expected = [("hello", 2.5, 2.8), ("there", 2.9, 3.4)];
    assert!(times.iter().zip(&expected).all(|(a, b)| a.0 == b.0 && close(a.1, b.1) && close(a.2, b.2)), "{times:?}");
}

#[test]
fn clip_sound_and_staleness() {
    let body = format!(
        "let a = Clip{{src: {RUN} audio: true gain: keys([0.0, 0.0], [1.0, 1.0]) pan: -0.5 span: [0.0, 4.0]}}\nlet b = Clip{{src: asset(\"{ASSET}\", \"{REV}\") audio: false span: [0.0, 4.0]}}\nlet out = CutOutput{{frame: a.out(@frame)}}"
    );
    let doc = load(&edit(&body, "a, b, out"));
    let eval = CutEval::new(&doc);
    let plan = eval.audio_plan(0.0, 1.0);
    assert_eq!(plan.segments.iter().map(|segment| segment.node.as_str()).collect::<Vec<_>>(), ["a"]);
    assert!(plan.segments[0].keyed && close(eval.gain(doc.node("a").unwrap(), 0.5), 0.5) && eval.pan(doc.node("a").unwrap(), 0.5) == -0.5);

    let voice = VoiceSpec::default();
    let key = take_key(&voice, "Notes starts with one tap.");
    assert!(key.starts_with("tk1-") && key.len() == 20);
    assert_eq!(key, take_key(&voice, "  Notes starts  with one tap. "), "spacing is not speech");
    let fresh = VoiceLine { text: "Notes starts with one tap.".into(), at: SourceTime::Seconds(1.0), take: Some(take()), key: Some(key.clone()) };
    assert!(!fresh.stale(&voice));
    assert!(VoiceLine { text: "Notes starts with two taps.".into(), ..fresh.clone() }.stale(&voice));
    assert!(fresh.stale(&VoiceSpec { speed: 1.1, ..voice.clone() }));
    assert!(fresh.stale(&VoiceSpec { id: "af_heart".into(), ..voice.clone() }));
    assert!(VoiceLine { take: None, ..fresh.clone() }.stale(&voice));
    assert!(VoiceLine { key: None, ..fresh.clone() }.stale(&voice));
    // The key is stable across builds: sources store it.
    assert_eq!(take_key(&voice, "That's it."), take_key(&voice, "That's it."));
    let doc = load(VOICE);
    let CutNodeKind::VoiceOver { lines, voice, .. } = &doc.node("voice").unwrap().kind else { panic!() };
    assert!(lines.iter().all(|line| line.stale(voice)), "a placeholder key, and two lines without takes");
}

fn diagnostics(body: &str, nodes: &str) -> Vec<(DiagnosticKind, String)> {
    let Err(diags) = CutDoc::load(&edit(body, nodes)) else { panic!("loaded:\n{body}") };
    diags.into_iter().map(|diag| (diag.kind, diag.message)).collect()
}

#[test]
fn audio_voice_and_subtitle_diagnostics() {
    let music = format!("AudioClip{{src: asset(\"{ASSET}\", \"{REV}\")");
    let cases: Vec<(String, &str, DiagnosticKind, &str)> = vec![
        (format!("let m = {music} duck: {{under: [@nobody]}}}}\nlet out = CutOutput{{frame: nil}}"), "m, out", DiagnosticKind::Graph, "ducks under `nobody`"),
        (format!("let m = {music} duck: {{under: [@m]}}}}"), "m", DiagnosticKind::Graph, "ducks under `m`"),
        (format!("let m = {music} duck: {{depth_db: 3 under: [@m]}}}}"), "m", DiagnosticKind::InvalidParam, "`depth_db`"),
        (format!("let m = {music} fade_in: -1}}"), "m", DiagnosticKind::InvalidParam, "`m.fade_in` is seconds"),
        (format!("let c = Clip{{src: asset(\"{ASSET}\", \"{REV}\") audio: 3}}"), "c", DiagnosticKind::InvalidParam, "`c.audio` is true or false"),
        ("let v = VoiceOver{voice: {engine: @indextts}}".into(), "v", DiagnosticKind::InvalidParam, "clones a `reference"),
        ("let v = VoiceOver{voice: {engine: @espeak}}".into(), "v", DiagnosticKind::InvalidParam, "`engine` is @kokoro or @indextts"),
        ("let v = VoiceOver{lines: [line(\"hi\", marker(@go))]}".into(), "v", DiagnosticKind::InvalidRef, "no clip plays an app run"),
        ("let v = VoiceOver{lines: [line(\"hi\", 1.0, \"take\")]}".into(), "v", DiagnosticKind::InvalidRef, "take is `asset(id, rev)`"),
        ("let v = VoiceOver{lines: [line(\"hi\", 20.0)]}".into(), "v", DiagnosticKind::SpanOutOfRange, "after the cut's 16 seconds"),
        ("let s = Subtitles{style: \"karaoke/pop\"}".into(), "s", DiagnosticKind::Graph, "need their `source` wired"),
        ("let v = VoiceOver{}\nlet s = Subtitles{source: v.out(@audio) max_words: 0}".into(), "v, s", DiagnosticKind::InvalidParam, "`s.max_words`"),
    ];
    for (body, nodes, kind, needle) in cases {
        let diags = diagnostics(&body, nodes);
        assert!(diags.iter().any(|(k, message)| *k == kind && message.contains(needle)), "{body}\nwanted {kind:?} `{needle}` in {diags:#?}");
    }
    let loud = "use mod.flow.*\nuse mod.cut.*\nlet out = CutOutput{}\nFlow{\n    kind: @cut\n    output: {width: 1080 height: 1920 fps: 30 loudness: 5}\n    duration: 16.0\n    out\n}\n";
    let Err(diags) = CutDoc::load(loud) else { panic!() };
    assert!(diags.iter().any(|diag| diag.kind == DiagnosticKind::Header && diag.message.contains("loudness")), "{diags:#?}");
}

/// A voiceover with two takes: `second_at` is where line 1 starts.
fn two_lines(second_at: &str, first_text: &str) -> (CutDoc, AssetRef, AssetRef) {
    let a = take();
    let b = AssetRef { id: "ast_bbbbbbbbbbbbbbbbbbbbbbbbba".into(), rev: a.rev.clone() };
    let voice = VoiceSpec::default();
    let body = format!(
        "let v = VoiceOver{{lines: [line(\"{first_text}\", 1.0, asset(\"{}\", \"{}\"), \"{}\"), line(\"Second line.\", {second_at}, asset(\"{}\", \"{}\"), \"{}\")] fade_in: 0.01}}\nlet s = Subtitles{{source: v.out(@audio) max_words: 6}}\nlet out = CutOutput{{frame: s.out(@frame)}}",
        a.id, a.rev, take_key(&voice, "Hello there."), b.id, b.rev, take_key(&voice, "Second line."),
    );
    (load(&edit(&body, "v, s, out")), a, b)
}

#[test]
fn overlapping_lines_keep_their_times_and_the_earlier_take_is_cut_without_a_click() {
    let (doc, a, b) = two_lines("1.3", "Hello there.");
    let mut eval = CutEval::new(&doc);
    eval.set_takes([
        (a.clone(), TakeInfo { duration: 1.0, words: vec![word("Hello", 0.0, 0.4), word("there.", 0.45, 0.9)], phones: vec![word("h", 0.0, 0.1), word("ə", 0.1, 0.2), word("ð", 0.45, 0.6), word("ɛ", 0.6, 0.9)] }),
        (b.clone(), TakeInfo { duration: 1.0, words: vec![word("Second", 0.0, 0.4), word("line.", 0.45, 0.9)], phones: Vec::new() }),
    ]);
    let lines = eval.voice_lines("v");
    assert_eq!((lines[0].start, lines[1].start), (1.0, 1.3), "each line starts at its own time");
    assert!(lines[0].cut && !lines[1].cut && close(lines[0].sound_end, 1.3));
    let overlaps = eval.voice_overlaps("v");
    assert_eq!(overlaps.len(), 1);
    assert!(overlaps[0].0 == 0 && overlaps[0].1 == 1 && close(overlaps[0].2, 0.7));
    // One take at a time: the first ends where the second starts.
    let plan = eval.audio_plan(1.0, 2.5);
    let spans = plan.segments.iter().map(|s| (s.line, s.t0, s.t1)).collect::<Vec<_>>();
    assert_eq!(spans, [(Some(0), 1.0, 1.3), (Some(1), 1.3, 2.3)]);
    // The cut take fades out over DECLICK into its cut: the gain is
    // continuous from line 0's tail through line 1's fade in.
    let v = doc.node("v").unwrap();
    let mut last = eval.gain(v, 1.2);
    for i in 1..=200 {
        let t = 1.2 + i as f64 * 0.0005;
        let now = eval.gain(v, t);
        assert!((now - last).abs() <= 0.0005 / 0.01 + 1e-9, "step at {t}: {last} -> {now}");
        last = now;
    }
    assert!(close(eval.gain(v, 1.295), 0.5), "half way through the declick");
    // Line 0's words are cut with it, and so are its phones (placed in cut seconds).
    assert!(eval.voice_lines("v")[0].words.iter().all(|w| w.end <= 1.3 + 1e-9));
    let phones = &eval.voice_lines("v")[0].phones;
    assert_eq!(phones.iter().map(|p| p.text.as_str()).collect::<Vec<_>>(), ["h", "ə"], "phones past the cut are dropped");
    assert!(close(phones[0].start, 1.0) && close(phones[1].end, 1.2));
}

#[test]
fn a_changed_line_shows_its_new_text_provisionally() {
    let (doc, a, _) = two_lines("5.0", "Hello there.");
    let info = TakeInfo { duration: 1.0, words: vec![word("Hello", 0.0, 0.4), word("there.", 0.45, 0.9)], phones: Vec::new() };
    let mut eval = CutEval::new(&doc);
    eval.set_take(a.clone(), info.clone());
    assert!(!eval.voice_lines("v")[0].provisional);
    assert_eq!(eval.captions("s")[0].text(), "Hello there.");
    assert!(!eval.captions("s")[0].provisional);
    // A fixed typo, same word count: the take's times, the new words.
    let (doc, a, _) = two_lines("5.0", "Hello their.");
    let mut eval = CutEval::new(&doc);
    eval.set_take(a.clone(), info.clone());
    let line = &eval.voice_lines("v")[0];
    assert!(line.provisional && line.timed);
    assert_eq!(line.words[1], word("their.", 1.45, 1.9));
    assert_eq!(eval.captions("s")[0].text(), "Hello their.");
    assert!(eval.captions("s")[0].provisional && eval.subtitle_plan(1.2)[0].provisional);
    // A rewrite: the new text spread over what the take speaks.
    let (doc, a, _) = two_lines("5.0", "Good morning to you all.");
    let mut eval = CutEval::new(&doc);
    eval.set_take(a, info);
    let line = &eval.voice_lines("v")[0];
    assert!(line.provisional && !line.timed && line.words.len() == 5);
    assert!(close(line.words[0].start, 1.0) && close(line.words[4].end, 1.9));
    assert_eq!(eval.captions("s")[0].text(), "Good morning to you all.");
}

#[test]
fn transcript_words_are_clamped_to_the_span_and_captions_to_the_subtitles() {
    let body = format!(
        "let m = AudioClip{{src: asset(\"{ASSET}\", \"{REV}\") in: 10.0 span: [2.0, 5.0]}}\nlet s = Subtitles{{source: m.out(@audio) transcript: asset(\"ast_bbbbbbbbbbbbbbbbbbbbbbbbba\", \"{REV}\") span: [2.0, 0.5]}}\nlet out = CutOutput{{frame: s.out(@frame)}}"
    );
    let doc = load(&edit(&body, "m, s, out"));
    let mut eval = CutEval::new(&doc);
    // "cut" straddles the clip's cut-in at edit 2.0 (source 10.0).
    eval.set_transcript(AssetRef { id: "ast_bbbbbbbbbbbbbbbbbbbbbbbbba".into(), rev: REV.into() }, vec![
        SpokenLine { start: 9.8, end: 10.8, words: vec![word("cut", 9.8, 10.2), word("in", 10.3, 10.8)] },
    ]);
    let captions = eval.captions("s");
    assert_eq!(doc.node("s").unwrap().span, Some(Span { start: 2.0, length: 0.5 }));
    assert_eq!(captions.len(), 1);
    assert_eq!(captions[0].text(), "cut in");
    assert!(close(captions[0].words[0].start, 2.0), "clamped to the clip's start");
    assert!(close(captions[0].start, 2.0) && close(captions[0].end, 2.5), "cut to the Subtitles node's span: {:?}", captions[0]);
}

#[test]
fn clip_audio_takes_1_and_0_and_voices_know_their_language() {
    let body = format!("let c = Clip{{src: {RUN} audio: 1}}\nlet d = Clip{{src: asset(\"{ASSET}\", \"{REV}\") audio: 0}}\nlet out = CutOutput{{frame: c.out(@frame)}}");
    let doc = load(&edit(&body, "c, d, out"));
    let CutNodeKind::Clip { audio: c, .. } = &doc.node("c").unwrap().kind else { panic!() };
    let CutNodeKind::Clip { audio: d, .. } = &doc.node("d").unwrap().kind else { panic!() };
    assert!(*c && !*d);
    assert_eq!(VoiceSpec::default().language(), "en");
    assert_eq!(VoiceSpec { id: "ff_siwis".into(), ..VoiceSpec::default() }.language(), "fr");
}

#[test]
fn burned_in_captions_stay_inside_each_presets_safe_zone() {
    use makepad_story_cut::subtitle::{CAPTION_CHAR_EM, CAPTION_LINE_EM};
    for (preset, safe) in [("reels", SafeZone::Reels), ("tiktok", SafeZone::TikTok), ("shorts", SafeZone::Shorts)] {
        // The spec's anchor, which points into every preset's bottom band,
        // and a line long enough to wrap.
        let body = format!(
            "let v = VoiceOver{{lines: [line(\"Meet the dancing bunny, who never stops moving\", 1.0)]}}\nlet s = Subtitles{{source: v.out(@audio) style: @movie anchor: vec2(0.5, 0.86) safe: @{preset} max_words: 8}}\nlet out = CutOutput{{frame: s.out(@frame)}}"
        );
        let doc = load(&edit(&body, "v, s, out"));
        let eval = CutEval::new(&doc);
        let bands = safe.bands();
        let mut seen = 0;
        for i in 0..40 {
            let t = 1.0 + i as f64 * 0.1;
            for frame in eval.subtitle_plan(t) {
                seen += 1;
                let chars = frame.text.chars().count() as f64;
                let half_h = frame.karaoke.lines as f64 * frame.font_size * CAPTION_LINE_EM / 1920.0 / 2.0;
                let half_w = (chars * frame.font_size * CAPTION_CHAR_EM / 1080.0).min(frame.karaoke.safe[2]) / 2.0;
                let eps = 1e-9;
                assert!(frame.anchor[1] - half_h >= bands.top - eps && frame.anchor[1] + half_h <= 1.0 - bands.bottom + eps, "{preset}: {frame:?}");
                assert!(frame.anchor[0] - half_w >= bands.left - eps && frame.anchor[0] + half_w <= 1.0 - bands.right + eps, "{preset}: {frame:?}");
                assert_eq!(frame.karaoke.safe, [bands.left, bands.top, 1.0 - bands.left - bands.right, 1.0 - bands.top - bands.bottom]);
            }
            // The burned-in op carries the same placement and safe region.
            if let Some(op) = eval.frame_plan(t).ops.iter().find(|op| op.node == "s") {
                let DrawKind::Text { position, karaoke: Some(karaoke), .. } = &op.kind else { panic!() };
                assert!(position[1] / 1920.0 <= 1.0 - bands.bottom && karaoke.safe[1] == bands.top, "{preset}");
            }
        }
        assert!(seen > 0, "{preset}: no caption shown");
    }
}
