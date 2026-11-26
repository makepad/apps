//! The lyric-film kit (REF-threejs §5 M1): vocal-locked words with ghost →
//! current → sung states, the new animators, TokenStream / Form /
//! CodeDraw / UI props, escalating numbers, text on another layer's path,
//! and 2D layers following 3D anchors. Everything is a function of time.

use makepad_motion::lyrics::{voice_from_lyrics, voice_words};
use makepad_motion::*;

const LYRICS: &str = r#"{"format":"vj-lyrics","version":5,"digest":"d","backend":"b","model":"m","language":"en","lines":[
 {"t0":0.3,"t1":2.4,"text":"A red thread of sun","w":[0.3,0.55,0.85,1.35,1.6],"e":[0.5,0.8,1.3,1.55,2.3],"c":true},
 {"t0":2.6,"t1":3.9,"text":"on blue silk","w":[2.6,2.9,3.2],"e":[2.85,3.15,3.8],"c":true}]}"#;

fn context(offset: f64) -> MotionContext {
    let mut features = AudioFeatures::default();
    features.voices.push(("song".into(), voice_from_lyrics(LYRICS, 0.0).unwrap()));
    let mut c = MotionContext::analyzed(features, None, 0.0);
    c.offset = offset;
    c
}

fn load(source: &str) -> MotionDoc {
    MotionDoc::load_with(source, &[], context(0.0)).unwrap_or_else(|d| panic!("{}\n{source}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")))
}

fn leaves(items: &[Item], opacity: f64, out: &mut Vec<Item>) {
    for item in items {
        match &item.kind {
            ItemKind::Group { children, .. } => leaves(children, opacity * item.opacity, out),
            ItemKind::Fly { inner, .. } | ItemKind::Anchored { inner, .. } => leaves(std::slice::from_ref(inner.as_ref()), opacity * item.opacity, out),
            _ => {
                let mut leaf = item.clone();
                leaf.opacity *= opacity;
                out.push(leaf);
            }
        }
    }
}

fn texts(doc: &MotionDoc, t: f64) -> Vec<Item> {
    let frame = doc.eval(t);
    assert!(frame.errors.is_empty(), "{:?}", frame.errors);
    let mut out = Vec::new();
    leaves(&frame.items, 1.0, &mut out);
    out.into_iter().filter(|i| matches!(i.kind, ItemKind::Text { .. })).collect()
}

#[test]
fn words_lock_to_the_vocal_ghost_current_sung() {
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(1920, 1080) duration: 4.0 layers: [
        Text{source: voice(@song) line: 0 by: @word color: #ffffff ghost: #ffffff40 highlight: @active highlight_color: #ff5a1f at: vec2(100, 100)}]}"#);
    let colours = |t: f64| -> Vec<(String, [f64; 4])> {
        match &texts(&doc, t)[0].kind {
            ItemKind::Text { text, units, .. } => units.iter().map(|u| (text[u.range.clone()].to_string(), u.color)).collect(),
            _ => Vec::new(),
        }
    };
    let at = colours(0.9); // "thread" is being sung
    assert_eq!(at.iter().map(|(w, _)| w.as_str()).collect::<Vec<_>>(), ["A", "red", "thread", "of", "sun"], "line 0 only");
    let (white, orange) = ([1.0, 1.0, 1.0, 1.0], at[2].1);
    assert!((orange[0] - 1.0).abs() < 1e-6 && orange[1] < 0.5, "the current word in the accent: {orange:?}");
    assert_eq!(at[0].1, white, "sung words in the text's colour");
    assert!(at[4].1[3] < 0.3, "words to come are ghosts");
    // The resolver shared with Text3D: document-clock word times.
    let words = voice_words(&context(10.0), "song", Some(1)).unwrap();
    assert_eq!(words[0].0, "on");
    assert!((words[0].1 - (2.6 - 10.0)).abs() < 1e-9);
    assert!(voice_words(&context(0.0), "song", Some(7)).is_err());
    assert!(voice_words(&context(0.0), "nope", None).is_err());
}

#[test]
fn the_new_animators_load_and_are_exact() {
    for (name, text, extra) in [
        ("sag", "rain bows the reed low", ""),
        ("break_through", "RIPPLES\\nCROSS\\nGLASS", ""),
        ("fill_frame", "WINDOWS WARM THE FOG", " rows: 6"),
        ("unmask", "dawn lifts salt-white veils", ""),
        ("mirror", "backward", " mirror: @vertical"),
        ("spark", "we trim each lamp", ""),
    ] {
        let doc = load(&format!("use mod.motion.*; Motion{{size: vec2(1920, 1080) duration: 3.0 layers: [Text{{text: \"{text}\" size: 90 at: vec2(100, 100) animate: @{name}{extra}}}]}}"));
        for t in [0.0, 0.4, 1.0, 2.2] {
            assert_eq!(format!("{:?}", doc.eval(t).items), format!("{:?}", doc.eval(t).items), "{name} exact at {t}");
            assert!(doc.eval(t).errors.is_empty(), "{name}");
        }
    }
    // sag: the letters end below where they started; mirror ends flipped.
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(1920, 1080) duration: 3.0 layers: [Text{text: "go" size: 90 at: vec2(100, 100) animate: @sag start: 0}]}"#);
    let ItemKind::Text { units, .. } = &texts(&doc, 1.2)[0].kind else { panic!() };
    assert!(units.iter().all(|u| u.offset[1] > 10.0));
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(1920, 1080) duration: 3.0 layers: [Text{text: "back" size: 90 at: vec2(100, 100) animate: @mirror start: 0}]}"#);
    assert!((texts(&doc, 1.0)[0].local.scale[0] + 1.0).abs() < 1e-9);
}

#[test]
fn templates_token_stream_form_code_and_props() {
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(1920, 1080) duration: 4.0 layers: [
        TokenStream{prompt: "Lantern, shine!" tokens: ["Lant", "ern, ", "shine!"] every: 0.5 start: 0.2 enter: true}
        Form{title: "Form 7-B" fields: [["Harbor", "East quay"]] checks: {options: ["closed", "open"] pick: 1} signature: "We" stamp: "open" stamp2: "filed"}
        CodeDraw{code: "\\draw (0,0) circle (1); % head\n\\draw (0,1) -- (0,2); % horn" layers: [Ellipse{size: vec2(100, 100) stroke: #fff width: 3} Path{svg: "M 0 0 L 0 100" stroke: #fff width: 3}]}
        UiWindow{title: "Auto-reply" lines: ["Harbor gates open at dawn."] start: 1.0 button: "OK"}
        UiButton{label: "Relight" click: 3.0}]}"#);
    let typed = |t: f64| texts(&doc, t).iter().find_map(|i| match &i.kind {
        ItemKind::Text { text, reveal: Some(n), caret: Some(_), .. } if text.starts_with("Lantern") => Some(*n),
        _ => None,
    }).unwrap();
    assert_eq!((typed(0.1), typed(0.3), typed(0.8), typed(1.3)), (0, 4, 9, 15), "token by token");
    for t in [0.0, 1.0, 2.5, 3.9] {
        assert_eq!(format!("{:?}", doc.eval(t).items), format!("{:?}", doc.eval(t).items));
    }
    let wrong = MotionDoc::load(r#"use mod.motion.*; Motion{size: vec2(100, 100) duration: 1.0 layers: [TokenStream{prompt: "abc" tokens: ["a", "x"]}]}"#);
    assert!(wrong.is_err(), "tokens must spell the prompt");
    // Vocal-locked tokens: one per sung word of the line.
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(1920, 1080) duration: 4.0 layers: [TokenStream{prompt: "on blue silk" words: voice(@song) line: 1}]}"#);
    let n = |t: f64| texts(&doc, t).iter().find_map(|i| match &i.kind { ItemKind::Text { reveal: Some(n), caret: Some(_), .. } => Some(*n), _ => None }).unwrap();
    assert_eq!((n(2.7), n(3.0), n(3.3)), (3, 8, 12));
}

#[test]
fn numbers_escalate_and_overflow() {
    assert_eq!(format_escalate(999_999.4, 0), "999,999");
    assert_eq!(format_escalate(12_400_000.0, 0), "12.4 million");
    assert_eq!(format_escalate(1e30, 0), "1 nonillion");
    assert_eq!(format_escalate(1e100, 0), "googol");
    assert_eq!(format_escalate(3.2e42, 0), "3.2e42");
    assert_eq!(format_escalate(f64::INFINITY, 0), "∞");
    assert_eq!(format_escalate(f64::NAN, 0), "NaN");
    assert_eq!(format_escalate_exp(1000.0, 0), "1.0e1000");
    assert_eq!(format_time(1e9, TimeFormat::Escalate, 30.0, 0), "1 billion");
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(1920, 1080) duration: 2.0 layers: [
        Text{number: tween(0, 1000, 0, 1) log: true format: @escalate size: 60 at: vec2(10, 10)}
        Text{number: 1000 log: true overflow: @wall size: 60 at: vec2(10, 200)}]}"#);
    let shown: Vec<String> = texts(&doc, 0.3).iter().map(|i| match &i.kind { ItemKind::Text { text, .. } => text.clone(), _ => String::new() }).collect();
    assert_eq!(shown[0], "1.0e300");
    assert!(shown[1].starts_with("1000") && shown[1].matches('0').count() == 1000 && shown[1].contains('\n'), "a wall of zeros");
}

#[test]
fn text_rides_another_layers_path() {
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(1920, 1080) duration: 2.0 layers: [
        Group{at: vec2(100, 50) layers: [Path{name: "loss" svg: "M 0 0 L 400 300" at: vec2(10, 20) stroke: #fff width: 2}]}
        Text{text: "falling tide" size: 40 at: vec2(60, 40) path: layer("loss")}
        Chart{name: "c" type: @line at: vec2(0, 400) size: vec2(1200, 500) csv: "x,loss\n1,9\n2,5\n3,2"}
        Text{text: "silver road" size: 30 at: vec2(0, 0) path: layer("c")}
        Text{text: "named" size: 30 at: vec2(0, 0) path: layer("c", "Series 1")}]}"#);
    let paths: Vec<_> = texts(&doc, 1.0).iter().filter_map(|i| match &i.kind { ItemKind::Text { path: Some((path, _)), .. } => Some(path.clone()), _ => None }).collect();
    // The Path's world start (110, 70) in the Text's coordinates (at 60, 40).
    assert_eq!(paths[0].cmds.first(), Some(&geometry::PathCmd::MoveTo(50.0, 30.0)), "{:?}", paths[0]);
    assert!(!paths[1].cmds.is_empty() && paths[1] == paths[2], "the chart's (first, or named) series line");
    assert!(MotionDoc::load(r#"use mod.motion.*; Motion{size: vec2(100, 100) duration: 1.0 layers: [Text{text: "x" path: layer("nope")}]}"#).is_err());
}

// (A document with a full 3D scene evaluates its Mesh and Camera in its own VM: the scene vocabulary is motion3d's.)
#[cfg(feature = "scene3d")]
#[test]
fn layers_follow_3d_anchors() {
    let doc = load(r#"use mod.motion.*; use mod.scene; Motion{size: vec2(1920, 1080) duration: 2.0 layers: [
        Scene3D{name: "set" size: vec2(1920, 1080) camera: Camera{position: vec3(0, 0, 6) target: vec3(0, 0, 0)} children: [scene.Mesh{name: "eye" geometry: scene.SphereGeometry{} position: vec3(1, 0, 0)}]}
        Rect{name: "box" at: anchor("set", "eye") x: -50 y: -50 size: vec2(100, 100) stroke: #fff width: 2}]}"#);
    let frame = doc.eval(1.0);
    let anchored = frame.items.iter().find_map(|i| match &i.kind { ItemKind::Anchored { scene, node, inner } => Some((scene.clone(), node.clone(), inner.clone())), _ => None }).expect("anchored");
    assert_eq!((anchored.0.as_str(), anchored.1.as_str()), ("layers[0]", "eye"));
    assert_eq!(anchored.2.world.apply([0.0, 0.0]), [-50.0, -50.0], "its own offset from the node");
    assert!(MotionDoc::load(r#"use mod.motion.*; Motion{size: vec2(100, 100) duration: 1.0 layers: [Rect{size: vec2(1, 1) fill: #fff at: anchor("", "")}]}"#).is_err());
}


#[test]
fn the_kit_finds_sung_words_times_hits_and_rolls_an_odometer() {
    // lyric_line / lyric_word find words by content; prog, pulse and ease(@name)
    // work in property functions; a document's own name wins.
    let doc = load(r#"use mod.motion.*; use mod.motion.modules.film.*
let L = lyric_line(voice(@song), "silk", 0)
let S = lyric_word(voice(@song), "thread", 0)
let hash01 = fn(a, b) { return 0.25 }
Motion{size: vec2(1920, 1080) duration: 4.0 layers: [
    Text{name: "first" number: fn(t) { L[0].start } decimals: 2 at: vec2(0, 0)}
    Text{name: "count" number: fn(t) { L.len() } decimals: 0 at: vec2(0, 100)}
    Text{name: "thread" number: fn(t) { S.end } decimals: 2 at: vec2(0, 200)}
    Text{name: "ease" number: fn(t) { ease(@ease_out_expo, prog(t, 1.0, 2.0)) + pulse(t, 1.0, 0.5) + hash01(0.5, 1.0) } decimals: 3 at: vec2(0, 300)}
    odometer({value: fn(t) { mix(0.04, 0.15, prog(t, 1.0, 2.0)) } decimals: 2 at: vec2(0, 500) size: 100
        font: Font{family: "JetBrains Mono" weight: 400} color: #ff4d12 extra: nil digit: "9"})
]}"#);
    let shown = |t: f64| -> Vec<String> {
        texts(&doc, t).into_iter().filter(|i| i.opacity > 0.5).filter_map(|i| match &i.kind { ItemKind::Text { text, .. } => Some(text.clone()), _ => None }).collect()
    };
    let at0 = shown(0.0);
    assert_eq!(&at0[..4], ["2.60", "3", "1.30", "0.250"], "{at0:?}");
    // 1.5 s: halfway; the eases, the hit (0.5 after a half-life) and the document's own ease.
    let eased: f64 = shown(1.5)[3].parse().unwrap();
    assert!((eased - (1.0 - 2f64.powf(-5.0) + 0.5 + 0.25)).abs() < 0.002, "{eased}");
    // The odometer rests on 0.04, then on 0.15 (its wheels showing the digits).
    assert_eq!(shown(0.0)[4..].concat(), "0.04");
    assert_eq!(shown(3.0)[4..].concat(), "0.15");
}
