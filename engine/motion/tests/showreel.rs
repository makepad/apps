//! The showreel grammar (REF-Ac7SuDYTQDc): slams, exits and transitions,
//! the fly-through window, the re-layout morph, word-per-beat looks, the
//! HUD, clocks and time formats, playhead binding, labels, typewriter
//! rate, onomatopoeia and the `edit(s)` time.

use makepad_motion::*;

fn load(source: &str) -> MotionDoc {
    MotionDoc::load(source).unwrap_or_else(|d| panic!("{}\n{source}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")))
}

fn doc(layers: &str) -> MotionDoc {
    load(&format!("use mod.motion.*; Motion{{size: vec2(1920, 1080) duration: 4.0 bpm: 120 layers: [{layers}]}}"))
}

/// Every item with its groups' opacity multiplied in (Fly looked into).
fn leaves(items: &[Item]) -> Vec<Item> {
    fn walk(items: &[Item], opacity: f64, out: &mut Vec<Item>) {
        for item in items {
            match &item.kind {
                ItemKind::Group { children, mask, .. } => {
                    walk(children, opacity * item.opacity, out);
                    if let Some(mask) = mask {
                        walk(std::slice::from_ref(mask.as_ref()), opacity * item.opacity, out);
                    }
                }
                ItemKind::Fly { inner, .. } => walk(std::slice::from_ref(inner.as_ref()), opacity * item.opacity, out),
                _ => {
                    let mut leaf = item.clone();
                    leaf.opacity *= opacity;
                    out.push(leaf);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(items, 1.0, &mut out);
    out
}

fn text_items(d: &MotionDoc, t: f64) -> Vec<Item> {
    let frame = d.eval(t);
    assert!(frame.errors.is_empty(), "{:?}", frame.errors);
    leaves(&frame.items).into_iter().filter(|i| matches!(i.kind, ItemKind::Text { .. })).collect()
}

fn text_of(item: &Item) -> String {
    match &item.kind {
        ItemKind::Text { text, .. } => text.clone(),
        _ => String::new(),
    }
}

#[test]
fn slams_start_big_blurred_or_wide_and_land() {
    let d = doc(r#"Text{text: "PROMPT." size: 200 at: vec2(960, 540) align: @center animate: @zoom_slam start: 0.5}"#);
    let at = |t: f64| text_items(&d, t)[0].clone();
    let (a, b) = (at(0.5), at(0.8));
    assert!(a.local.scale[0] > 2.4 && a.blur > 10.0, "starts at 250%, blurred: {:?} {}", a.local.scale, a.blur);
    assert!((b.local.scale[0] - 1.0).abs() < 0.02 && b.blur < 0.5, "lands in a quarter second");
    let d = doc(r#"Text{text: "FINISHED." size: 180 tracking: 4 at: vec2(960, 540) animate: @tracking_slam start: 0.5}"#);
    let tracking = |t: f64| match &text_items(&d, t)[0].kind {
        ItemKind::Text { tracking, .. } => *tracking,
        _ => 0.0,
    };
    assert!(tracking(0.5) > 180.0, "spread past the frame");
    assert!((tracking(0.9) - 4.0).abs() < 0.5, "back to its own spacing");
}

#[test]
fn exits_wrap_any_layer() {
    let d = doc(r#"Text{text: "BYE" size: 200 at: vec2(960, 540) exit: @defocus exit_at: 2.0}"#);
    let frame = |t: f64| d.eval(t).items[0].clone();
    assert!(frame(1.9).opacity > 0.99 && frame(1.9).blur < 1e-6);
    assert!(frame(2.4).opacity < 0.01 && frame(2.3).blur > 10.0);
    let d = doc(r#"Ellipse{at: vec2(900, 480) size: vec2(120, 120) fill: #ff5a1f exit: @fill exit_at: 1.0}"#);
    let scale = d.eval(1.6).items[0].local.scale[0];
    assert!(120.0 * scale > 2.0 * (960.0f64.powi(2) + 540.0f64.powi(2)).sqrt(), "the ball grows past the frame's corners: {scale}");
    let d = doc(r#"Rect{size: vec2(1920, 1080) fill: #111 exit: @collapse exit_at: 1.0 exit_to: vec2(100, 100)}"#);
    assert!(d.eval(1.5).items[0].local.scale[0].abs() < 1e-6);
    for exit in ["iris", "curtain", "speed_lines"] {
        let d = doc(&format!(r#"Group{{exit: @{exit} exit_at: 1.0 layers: [Rect{{size: vec2(1920, 1080) fill: #ff5a1f}}]}}"#));
        for t in [0.5, 1.2, 1.9] {
            assert!(d.eval(t).errors.is_empty(), "{exit}");
        }
    }
    assert!(MotionDoc::load(r#"use mod.motion.*; Motion{size: vec2(1920, 1080) duration: 4.0 layers: [Rect{size: vec2(10, 10) fill: #fff exit: @vanish}]}"#).is_err());
}

#[test]
fn freezes_and_polaroids_compose_with_exits() {
    // A flash-freeze that then irises away: the freeze still sees the
    // layers under the wrapper that the exit made.
    let d = doc(r#"Rect{size: vec2(1920, 1080) fill: #1d2a44}
        Ellipse{at: vec2(200, 300) x: tween(0, 1200, 0, 4) size: vec2(400, 400) fill: #ffc23a}
        Polaroid{time: 1.0 span: [1.0, 3.0] caption: "then" exit: @iris exit_at: 2.4}"#);
    for t in [1.0, 1.2, 2.6, 3.9] {
        let frame = d.eval(t);
        assert!(frame.errors.is_empty());
        assert_eq!(format!("{:?}", frame.items), format!("{:?}", d.eval(t).items), "exact at {t}");
    }
    let ellipses = leaves(&d.eval(2.0).items).iter().filter(|i| matches!(i.kind, ItemKind::Ellipse { .. })).count();
    assert!(ellipses >= 3, "the live sun, the full-frame freeze and the photo's: {ellipses}");
}

#[test]
fn a_glyph_window_and_a_fly_through() {
    let d = doc(r#"FlyThrough{into: "move" letter: 1 time: 1.0 duration: 1.0 zoom: 50 layers: [
        Rect{size: vec2(1920, 1080) fill: #ff5a1f}
        Text{name: "move" text: "MOVE." size: 300 at: vec2(960, 540) align: @center valign: @center window: 1
             inside: Pattern{kind: @checker color: #1f3dff color2: #f0ebe0 cell: 40 size: vec2(1920, 1080)}}]}"#);
    let zoom = |t: f64| match &d.eval(t).items[0].kind {
        ItemKind::Fly { zoom, target, letter, .. } => {
            assert_eq!(*letter, 1);
            assert!(!target.is_empty());
            *zoom
        }
        other => panic!("{other:?}"),
    };
    assert!((zoom(0.5) - 1.0).abs() < 1e-9);
    assert!((zoom(2.5) - 50.0).abs() < 1e-6);
    assert!(zoom(1.5) > 1.0 && zoom(1.5) < 50.0);
    // The text without its window letter, and the matte with only it.
    let texts = text_items(&d, 0.5);
    let shown = |item: &Item| match &item.kind {
        ItemKind::Text { units, .. } => units.iter().filter(|u| u.opacity > 0.0).count(),
        _ => 0,
    };
    let mut counts: Vec<usize> = texts.iter().map(shown).collect();
    counts.sort();
    assert_eq!(counts, vec![1, 4], "M V E . as text, O as the window");
}

/// The ink of every visible letter path at `t`: (left, top, right, bottom) in document units.
fn letters(d: &MotionDoc, t: f64) -> Vec<[f64; 4]> {
    let frame = d.eval(t);
    assert!(frame.errors.is_empty(), "{:?}", frame.errors);
    leaves(&frame.items)
        .into_iter()
        .filter(|i| i.opacity > 0.01)
        .filter_map(|i| match &i.kind {
            ItemKind::Path { path, .. } => {
                let b = geometry::bounds(path)?;
                let o = i.world.apply([0.0, 0.0]);
                Some([o[0] + b[0], o[1] + b[1], o[0] + b[0] + b[2], o[1] + b[1] + b[3]])
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_text_re_lays_out_into_another() {
    // One morph path: the letters' outlines morph (paths, never a face swap).
    let d = doc(r#"Text{text: "I MAKE\nTHINGS\nMOVE." size: 150 at: vec2(100, 100)
        morph: {text: "MOVE." size: 400 at: vec2(960, 540) align: @center time: 1.0 duration: 1.0}}"#);
    assert!(text_items(&d, 0.5).is_empty(), "no Text item draws: the letters are paths");
    let before = letters(&d, 0.5);
    assert_eq!(before.len(), 16, "I MAKE THINGS MOVE.: every letter with ink");
    assert!(before.iter().all(|l| l[0] > 90.0 && l[2] < 1100.0), "left aligned at x 100");
    // After: the five letters of MOVE. (the first M, O, V, E and the stop), centred on x 960.
    let after = letters(&d, 2.5);
    assert_eq!(after.len(), 5);
    let (x0, x1) = (after.iter().map(|l| l[0]).fold(f64::MAX, f64::min), after.iter().map(|l| l[2]).fold(f64::MIN, f64::max));
    assert!(((x0 + x1) * 0.5 - 960.0).abs() < 60.0, "centred about the anchor: {x0}..{x1}");
    assert!(after.iter().all(|l| l[3] - l[1] > 200.0 || l[3] - l[1] > 30.0), "letters at size 400");
    // Midway the letters that stay are between their places, the rest fading out.
    let mid = letters(&d, 1.5);
    assert!(mid.len() >= 5 && mid.len() < before.len(), "leavers fade by the half: {}", mid.len());
    // The matched letters come first, in the target's order: the M is halfway at the middle of the move.
    let (m0, m1, m2) = (before[0][0], mid[0][0], after[0][0]);
    assert!(m1 > m0.min(m2) && m1 < m0.max(m2), "the M on its way: {m0} {m1} {m2}");
}

#[test]
fn a_morph_layer_of_two_texts_and_its_errors() {
    let d = doc(r#"Morph{time: 0.5 duration: 1.0
        from: Text{text: "ab" size: 100 at: vec2(100, 200)}
        to: Text{text: "ba" size: 100 at: vec2(100, 200)}}"#);
    // a and b swap places (listed in the target's order: b, then a).
    let (start, end) = (letters(&d, 0.0), letters(&d, 2.0));
    assert_eq!((start.len(), end.len()), (2, 2));
    assert!(start[0][0] - end[0][0] > 30.0 && end[1][0] - start[1][0] > 30.0, "b moves left, a right: {start:?} {end:?}");
    assert!(((start[0][2] - start[0][0]) - (end[0][2] - end[0][0])).abs() < 1.0, "the same letter");
    for bad in [r#"Morph{from: Text{text: "a" wrap: 300} to: Text{text: "b"}}"#, r#"Morph{from: Rect{size: vec2(10, 10)} to: Text{text: "b"}}"#] {
        assert!(MotionDoc::load(&format!("use mod.motion.*; Motion{{size: vec2(400, 400) duration: 2.0 layers: [{bad}]}}")).is_err(), "{bad}");
    }
}

#[test]
fn words_step_on_the_beat_each_in_its_look() {
    let d = doc(r#"Words{start: 0.5 every: beats(0.5) words: ["TIMING", {text: "SPACING" look: @blur}, {text: "DEPTH" look: @echo},
        {text: "CONTRAST" look: @split}, {text: "TEXTURE" look: @halftone}, {text: "SHAPE" look: @diamond}, {text: "CLOCK" look: @clock}, {text: "LINE" look: @outline}]}"#);
    let word = |t: f64| text_items(&d, t).iter().filter(|i| i.opacity > 0.0).map(text_of).find(|s| !s.is_empty()).unwrap_or_default();
    // At 120 BPM half a beat is 0.25 s.
    assert_eq!(word(0.6), "TIMING");
    assert_eq!(word(0.85), "SPACING");
    assert_eq!(word(1.1), "DEPTH");
    assert_eq!(word(1.8), "SHAPE");
    let err = MotionDoc::load(r#"use mod.motion.*; Motion{size: vec2(1920, 1080) duration: 4.0 layers: [Words{words: [{text: "X" look: @glow}]}]}"#).unwrap_err();
    assert!(err.iter().any(|e| e.message.contains("not a word look")), "{err:?}");
}

#[test]
fn clocks_formats_hud_and_playheads() {
    assert_eq!(format_time(3.5, TimeFormat::Timecode, 30.0, 0), "00:00:03:15");
    assert_eq!(format_time(2710.0, TimeFormat::MmSs, 30.0, 0), "45:10");
    assert_eq!(format_time(6310.0, TimeFormat::HhMmSs, 30.0, 0), "01:45:10");
    assert_eq!(format_time(3.52, TimeFormat::Seconds { total: Some(15.0) }, 30.0, 0), "3.5 / 15.0 s");
    let d = doc(r#"Hud{title: "Story  motion designer" tag: "Showreel '26" chapters: [[0, "EASING"], [2.0, "TYPOGRAPHY", #111111]]}
        Group{name: "reel" span: [1.0, 2.0] layers: [Rect{size: vec2(10, 10) fill: #fff}]}
        Text{number: playhead("reel") format: @seconds total: 2 at: vec2(10, 10)}
        Rect{name: "bar" size: vec2(1000, 8) fill: #ff5a1f anchor: vec2(0, 0.5) scale: progress("reel", vec2(0, 1), vec2(1, 1))}"#);
    let texts = |t: f64| text_items(&d, t).into_iter().filter(|i| i.opacity > 0.0).map(|i| text_of(&i)).collect::<Vec<_>>();
    let at1 = texts(1.5);
    assert!(at1.iter().any(|t| t == "01/02  EASING"), "{at1:?}");
    assert!(at1.iter().any(|t| t == "00:00:01:30"), "the timecode reads the time (frames at the default 60 fps): {at1:?}");
    assert!(at1.iter().any(|t| t == "120 BPM"));
    assert!(at1.iter().any(|t| t == "0.5 / 2.0 s"), "the playhead of the clip: {at1:?}");
    assert!(texts(2.5).iter().any(|t| t == "02/02  TYPOGRAPHY"));
    let bar = |t: f64| d.eval(t).items.iter().find(|i| matches!(i.kind, ItemKind::Rect { size, .. } if size[0] == 1000.0)).map(|i| i.local.scale[0]).unwrap();
    assert!((bar(2.0) - 0.5).abs() < 1e-9 && (bar(0.5) - 0.0).abs() < 1e-9 && (bar(3.5) - 1.0).abs() < 1e-9);
    assert!(MotionDoc::load(r#"use mod.motion.*; Motion{size: vec2(100, 100) duration: 1.0 layers: [Text{number: playhead("nope")}]}"#).is_err());
}

#[test]
fn labels_typewriters_and_onomatopoeia() {
    let d = doc(r#"Text{text: "showreel '26" role: @label at: vec2(10, 10)}
        Text{text: "make a dynamic reel" animate: @typewriter cps: 40 caret_style: @block at: vec2(10, 200) start: 0.0}
        Text{text: "RING RING" animate: @sfx at: vec2(960, 540) align: @center start: 1.0}"#);
    let items = text_items(&d, 0.25);
    let label = items.iter().find(|i| text_of(i) == "SHOWREEL '26").expect("upper-cased label");
    assert!(matches!(&label.kind, ItemKind::Text { font, .. } if font == "JetBrains Mono") && (label.opacity - 0.6).abs() < 1e-9);
    let typed = items.iter().find(|i| text_of(i).starts_with("make")).unwrap();
    // 19 characters at 40 a second: 10 shown by 0.25 s.
    assert!(matches!(&typed.kind, ItemKind::Text { reveal: Some(n), caret: Some((_, _, true)), .. } if (9..=11).contains(n)), "{:?}", typed.kind);
    let sfx: Vec<Item> = text_items(&d, 1.5).into_iter().filter(|i| text_of(i) == "RING RING").collect();
    assert_eq!(sfx.len(), 13, "twelve outline copies and the fill");
    assert!(text_items(&d, 1.0).iter().filter(|i| text_of(i) == "RING RING").all(|i| i.local.scale[0] < 0.01), "pops from nothing");
}

#[test]
fn edit_seconds_are_the_hosts() {
    let context = MotionContext { offset: 10.0, ..Default::default() };
    let d = MotionDoc::load_with(r#"use mod.motion.*; Motion{size: vec2(100, 100) duration: 5.0 layers: [Rect{size: vec2(10, 10) fill: #fff opacity: tween(0, 1, edit(12.0), edit(13.0))}]}"#, &[], context).unwrap();
    // Host second 12 is the graphic's second 2.
    assert!(d.eval(1.9).items[0].opacity < 1e-9);
    assert!((d.eval(2.5).items[0].opacity - 0.5).abs() < 1e-6);
}

