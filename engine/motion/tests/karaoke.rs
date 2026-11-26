//! Karaoke state styles (PDOOM-PARITY F6): each letter of a sung word goes
//! unsung → sung → cooled at its own time, warms before its word starts
//! and never runs ahead of the voice.

use makepad_motion::lyrics::voice_from_lyrics;
use makepad_motion::*;

const LYRICS: &str = r#"{"format":"vj-lyrics","version":5,"digest":"d","backend":"b","model":"m","language":"en","lines":[
 {"t0":0.3,"t1":2.4,"text":"A red thread of sun","w":[0.3,0.55,0.85,1.35,1.6],"e":[0.5,0.8,1.3,1.55,2.3],"c":true}]}"#;

fn load(source: &str) -> MotionDoc {
    let mut features = AudioFeatures::default();
    features.voices.push(("song".into(), voice_from_lyrics(LYRICS, 0.0).unwrap()));
    MotionDoc::load_with(source, &[], MotionContext::analyzed(features, None, 0.0)).unwrap_or_else(|d| panic!("{}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")))
}

fn colours(doc: &MotionDoc, t: f64) -> Vec<(String, [f64; 4])> {
    let frame = doc.eval(t);
    assert!(frame.errors.is_empty(), "{:?}", frame.errors);
    match &frame.items[0].kind {
        ItemKind::Text { text, units, .. } => units.iter().map(|u| (text[u.range.clone()].to_string(), u.color)).collect(),
        _ => panic!("a text"),
    }
}

const WHITE: [f64; 4] = [1.0, 1.0, 1.0, 0.25];
const SUNG: [f64; 4] = [1.0, 0.0, 0.0, 1.0];
const COOL: [f64; 4] = [0.0, 0.0, 1.0, 1.0];

#[test]
fn letters_fill_in_order_and_cool() {
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(1920, 1080) duration: 6.0 layers: [
        Text{source: voice(@song) line: 0 by: @letter color: #ffffff40 at: vec2(100, 100)
             karaoke: {sung: #ff0000 cooled: #0000ff anticipate: 0.4 cool: 1.0}}]}"#);
    // "thread" is sung from 0.85 to 1.3: at 1.0 its first two letters.
    let c = colours(&doc, 1.0);
    assert_eq!(c.iter().map(|(s, _)| s.as_str()).collect::<String>(), "Aredthreadofsun");
    let letter = |k: usize| c[4 + k].1;
    assert!(letter(0)[0] > 0.9 && letter(0)[2] < 0.3, "t is sung: {:?}", letter(0));
    assert!(letter(1)[0] > 0.9, "h is sung");
    assert!(letter(3)[3] < 0.9, "e is not sung yet: {:?}", letter(3));
    // Before the line nothing is sung; the first letter warms up in its lead-in.
    let early = colours(&doc, 0.0);
    assert!(early.iter().all(|(_, c)| c[3] < 0.9), "nothing sung before the voice");
    let warm = colours(&doc, 0.2)[0].1;
    assert!(warm[3] > WHITE[3] && warm[3] < 1.0, "A warms before 0.3: {warm:?}");
    // Long after, everything has cooled.
    let late = colours(&doc, 5.0);
    assert!(late.iter().all(|(_, c)| *c == COOL), "{late:?}");
    assert!(colours(&doc, 1.35).iter().any(|(_, c)| c[0] > 0.5 && c[2] > 0.1 && c[2] < 0.9), "a sung letter on its way to cooled");
    let _ = SUNG;
}

#[test]
fn karaoke_needs_a_voice_and_says_its_shape() {
    let e = MotionDoc::load(r#"use mod.motion.*; Motion{size: vec2(100, 100) duration: 1 layers: [Text{text: "x" karaoke: {sung: #ff0000}}]}"#).map(|_| ()).unwrap_err();
    assert!(e.iter().any(|d| d.to_string().contains("voiceover")), "{e:?}");
    let e = MotionDoc::load(r#"use mod.motion.*; Motion{size: vec2(100, 100) duration: 1 layers: [Text{source: voice(@song) karaoke: 3}]}"#).map(|_| ()).unwrap_err();
    assert!(e.iter().any(|d| d.to_string().contains("karaoke")), "{e:?}");
}
