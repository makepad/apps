//! MatchMorph: the shared pieces of two layouts fly to their places, the
//! rest fade, deterministically.

use makepad_motion::plain::PrimitiveFrame;
use makepad_motion::*;

fn fixture() -> MotionDoc {
    let source = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/match_morph.splash")).unwrap();
    MotionDoc::load(&source).unwrap_or_else(|d| panic!("{}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")))
}

fn load(source: &str) -> MotionDoc {
    MotionDoc::load(source).unwrap_or_else(|d| panic!("{}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")))
}

fn leaves<'a>(item: &'a Item, out: &mut Vec<&'a Item>) {
    match &item.kind {
        ItemKind::Group { children, .. } => children.iter().for_each(|c| leaves(c, out)),
        _ => out.push(item),
    }
}

/// (layer name, x of its origin, opacity) of the Paths of the group `group` that show at `t`: a
/// transition in `to` order (pairs, then leavers, then arrivals), or a state holding still.
fn pieces(doc: &MotionDoc, t: f64, group: usize) -> Vec<(String, f64, f64)> {
    let frame = doc.eval(t);
    let mut out = Vec::new();
    leaves(&frame.items[group], &mut out);
    out.iter().map(|c| (c.path.rsplit('.').next().unwrap().to_string(), c.world.apply([0.0, 0.0])[0], c.opacity)).collect()
}

#[test]
fn words_swap_places_and_the_extras_fade() {
    let doc = fixture();
    // The text rewrite is 5 words into 5 words: all matched, none fade. Before the start the first state
    // holds, after the end the second.
    let before = pieces(&doc, 0.0, 0);
    let after = pieces(&doc, 3.0, 0);
    assert_eq!(before.len(), 5);
    assert_eq!(after.len(), 5);
    assert!(before.iter().chain(&after).all(|p| (p.2 - 1.0).abs() < 1e-9));
    // The transition lists pieces in `to` order: at its first frame the first (c²) sits where c² was (far
    // right), at its last it has arrived, and in between it is between.
    let (first, mid, last) = (pieces(&doc, 0.5, 0), pieces(&doc, 1.4, 0), pieces(&doc, 2.2999, 0));
    assert_eq!(first.len(), 5);
    for i in 0..5 {
        let (b, m, a) = (first[i].1, mid[i].1, last[i].1);
        assert!(m >= b.min(a) - 1e-6 && m <= b.max(a) + 1e-6, "piece {i}: {b} {m} {a}");
    }
    assert!(first[0].1 > last[0].1 + 100.0, "c² moves left");
    // Nothing moves before the start nor after the end.
    assert_eq!(pieces(&doc, 0.0, 0), pieces(&doc, 0.4, 0));
    assert_eq!(pieces(&doc, 2.5, 0), pieces(&doc, 3.9, 0));
}

#[test]
fn unmatched_pieces_fade_out_then_in() {
    let doc = fixture();
    // The formula gains "+ 2ab": new pieces appear only in the second half.
    let start = pieces(&doc, 1.0, 1);
    let mid = pieces(&doc, 1.9, 1);
    let end = pieces(&doc, 3.0, 1);
    let new: Vec<usize> = (0..start.len()).filter(|i| start[*i].2 < 0.01 && *i >= start.len() - 6).collect();
    assert!(!new.is_empty(), "something is new");
    assert!(new.iter().all(|&i| mid[i].2 < 0.99), "...and not yet in at 1.9: {:?}", new.iter().map(|&i| mid[i].2).collect::<Vec<_>>());
    assert!(end.iter().all(|p| (p.2 - 1.0).abs() < 1e-9), "all in at the end");
    assert!(end.len() >= start.iter().filter(|p| p.2 > 0.99).count() + new.len() - 2);
}

#[test]
fn a_fourier_series_grows_a_term_keyed_by_id_and_by_char() {
    let doc = fixture();
    // S_3 into S_4, centred (the third layer): `a`, `b`, `c` are named parts of both formulas, `d` only of the second.
    let start = pieces(&doc, 1.5, 2);
    let mid = pieces(&doc, 2.0, 2);
    let end = pieces(&doc, 3.2999, 2);
    // Order: the pairs (in S_4's order), then S_3's leavers, then S_4's arrivals; leavers are gone at the end, arrivals absent at the start.
    let arriving = start.iter().filter(|p| p.2 < 0.01).count();
    let leaving = end.iter().filter(|p| p.2 < 0.01).count();
    let pairs = start.len() - arriving - leaving;
    assert!(pairs >= 12, "the shared pieces pair: {pairs}");
    assert!(arriving >= 1 && leaving >= 1, "the new term and the 4 arrive, the 3 leaves");
    // The new term arrives in the second half only.
    let new = start.len() - arriving..start.len();
    assert!(new.clone().all(|i| mid[i].2 < 0.5 && end[i].2 > 0.99), "{:?}", new.clone().map(|i| mid[i].2).collect::<Vec<_>>());
    // Centred layouts: the formula grows to both sides, so the pieces that stay fly.
    let moved = (0..pairs).filter(|&i| (start[i].1 - end[i].1).abs() > 5.0).count();
    assert!(moved >= 10, "existing pieces fly: {moved}");
    assert!((0..pairs).all(|i| (start[i].2 - 1.0).abs() < 1e-9 && (end[i].2 - 1.0).abs() < 1e-9));

    // By character the same formulas pair too (no \id needed): every shared character pairs with its n-th.
    let by_char = load(r#"use mod.motion.*; Motion{size: vec2(1280, 720) duration: 4.0 layers: [
        MatchMorph{math: true steps: ["S_3(x) = \\frac{4}{\\pi}\\sin x + \\frac{\\sin 3x}{3}", "S_4(x) = \\frac{4}{\\pi}\\sin x + \\frac{\\sin 3x}{3} + \\frac{\\sin 5x}{5}"] times: [0.0] by: @char size: 40 at: vec2(100, 100) duration: 1.0}]}"#);
    let (a, b) = (pieces(&by_char, 0.0, 0), pieces(&by_char, 0.9999, 0));
    let arriving = a.iter().filter(|p| p.2 < 0.01).count();
    let leaving = b.iter().filter(|p| p.2 < 0.01).count();
    assert!(arriving >= 6, "the third term arrives: {arriving}");
    assert!(a.len() - arriving - leaving >= 15, "the shared characters pair");
}

#[test]
fn the_same_document_gives_the_same_frames() {
    let (a, b) = (fixture(), fixture());
    for t in [0.0, 0.9, 1.4, 2.2, 3.0] {
        assert_eq!(a.eval(t).hash(), b.eval(t).hash());
    }
}

#[test]
fn bad_fields_are_errors() {
    for bad in [
        r#"MatchMorph{steps: ["a", "b"] times: [0.0] by: @syllable}"#,
        r#"MatchMorph{steps: ["a"] times: []}"#,
        r#"MatchMorph{times: [0.0]}"#,
        r#"MatchMorph{steps: ["a", "b"]}"#,
        r#"MatchMorph{steps: ["a", "b", "c"] times: [0.0]}"#,
        r#"MatchMorph{steps: ["a", "b", "c"] times: [0.0, 0.5] duration: 1.0}"#,
        r#"MatchMorph{steps: ["a", "b"] times: [0.0] colors: [#ffffff, #ff0000, #00ff00]}"#,
        r#"MatchMorph{steps: ["a", "b"] times: [0.0] by: @word ease: @ease_sideways}"#,
        r#"MatchMorph{math: true steps: ["a", "b"] times: [0.0] by: @word}"#,
    ] {
        assert!(MotionDoc::load(&format!("use mod.motion.*; Motion{{size: vec2(100, 100) duration: 2.0 layers: [{bad}]}}")).is_err(), "{bad}");
    }
    // Lines match by their text.
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(400, 400) duration: 2.0 layers: [MatchMorph{steps: ["one\ntwo", "two\nthree"] times: [0.0] by: @line size: 40 duration: 1.0}]}"#);
    let at = |t: f64| pieces(&doc, t, 0);
    assert_eq!(at(0.0).len(), 3, "two matched + one fading in... plus one fading out");
}

#[test]
fn a_piece_that_shows_another_glyph_cross_fades_instead_of_morphing() {
    // "N = 20" into "N = 25": N and = keep their glyphs and fly (morph); the digits group shows other glyphs,
    // so it is two layers: the old (listed first) fading out, the new fading in.
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(600, 200) duration: 3.0 layers: [
        MatchMorph{math: true steps: ["N = \\id{d}{20}", "N = \\id{d}{25}"] times: [0.0] by: @id size: 60 at: vec2(50, 50) duration: 1.0}]}"#);
    let at = |t: f64| pieces(&doc, t, 0).into_iter().map(|p| p.2).collect::<Vec<_>>();
    assert_eq!(at(0.0), vec![1.0, 1.0, 1.0, 0.0], "N, =, old digits, new digits");
    // Old goes out over the first 60 %, new comes in over the last 60 %: both partly there around the middle.
    let mid = at(0.5);
    assert!(mid[0] == 1.0 && mid[1] == 1.0 && mid[2] > 0.0 && mid[2] < 0.5 && mid[3] > 0.0 && mid[3] < 0.5, "{mid:?}");
    let late = at(0.9999);
    assert!(late[2] == 0.0 && late[3] > 0.99, "{late:?}");
    assert_eq!(at(1.5), vec![1.0, 1.0, 1.0], "then the second state holds");
    // The same glyphs in a group stay a pair: nothing fades.
    let same = load(r#"use mod.motion.*; Motion{size: vec2(600, 200) duration: 3.0 layers: [
        MatchMorph{math: true steps: ["N = \\id{d}{20}", "N = 3 \\id{d}{20}"] times: [0.0] by: @id size: 60 at: vec2(50, 50) duration: 1.0}]}"#);
    assert!(pieces(&same, 0.5, 0).iter().filter(|p| p.2 == 1.0).count() >= 3);
    // A subscript 5 becoming a 6: out and in, not a morph.
    let sub = load(r#"use mod.motion.*; Motion{size: vec2(600, 200) duration: 3.0 layers: [
        MatchMorph{math: true steps: ["S_{\\id{n}{5}}", "S_{\\id{n}{6}}"] times: [0.0] by: @id size: 60 at: vec2(50, 50) duration: 1.0}]}"#);
    let start: Vec<f64> = pieces(&sub, 0.0, 0).iter().map(|p| p.2).collect();
    assert_eq!(start, vec![1.0, 1.0, 0.0], "S, old 5, new 6");
}

/// (opacity, fill rgb) of the Paths in the first group.
fn fills(doc: &MotionDoc, t: f64) -> Vec<(f64, [f64; 3])> {
    let frame = doc.eval(t);
    let mut out = Vec::new();
    leaves(&frame.items[0], &mut out);
    out.iter()
        .map(|c| match &c.kind {
            ItemKind::Primitive(PrimitiveFrame::Path { fill: Some(f), .. }) => (c.opacity, [f[0], f[1], f[2]]),
            other => panic!("not a filled path: {other:?}"),
        })
        .collect()
}

#[test]
fn pieces_keep_their_own_colours_and_arrivals_settle_from_the_enter_colour() {
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(800, 200) duration: 4.0 layers: [
        MatchMorph{math: true steps: ["\\id{a}{x} + \\id{b}{y}", "\\id{a}{x} + \\id{b}{y} + \\id{c}{z}"] times: [1.0] by: @id size: 60 at: vec2(50, 50)
            colors: [#ffffff, #ffd060] enter_color: #0000ff settle: 1.0 parts: {a: {color: #ff0000} c: {color: #00ff00}} duration: 1.0}]}"#);
    let near = |f: [f64; 3], c: [f64; 3]| (0..3).all(|i| (f[i] - c[i]).abs() < 0.02);
    let (red, green, white, gold) = ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 1.0, 1.0], [1.0, 0.816, 0.376]);
    // Before and after the move, a keeps its red whatever the unstyled colour does.
    let before = fills(&doc, 0.0);
    assert!(before.iter().any(|p| near(p.1, red)) && before.iter().any(|p| near(p.1, white)));
    let after = fills(&doc, 3.9);
    assert!(after.iter().any(|p| near(p.1, red) && p.0 == 1.0), "a stays red");
    assert!(after.iter().any(|p| near(p.1, green) && p.0 == 1.0), "the new term ends in its own green");
    assert!(after.iter().any(|p| near(p.1, gold)), "unstyled pieces took to_color");
    assert!(!after.iter().any(|p| p.0 > 0.0 && p.1[2] > 0.5 && p.1[0] < 0.5), "no enter blue left after the settle: {after:?}");
    // Arriving pieces come in blue (opacity just above 0) ...
    let early = fills(&doc, 1.6);
    assert!(early.iter().any(|p| p.0 > 0.0 && p.0 < 0.5 && p.1[2] > 0.6 && p.1[1] < 0.4), "{early:?}");
    // ... and the pair's own red never wavers.
    assert!(early.iter().any(|p| near(p.1, red) && p.0 == 1.0));
    // Bad colour settings are errors.
    for bad in [r#"parts: {a: {opacity: 0.5}}"#, r#"parts: {a: {color: tween(#000000, #ffffff, 0, 1, @linear)}}"#, r#"enter_color: 3"#] {
        let src = format!(r#"use mod.motion.*; Motion{{size: vec2(100, 100) duration: 2.0 layers: [MatchMorph{{math: true steps: ["\\id{{a}}{{x}}", "\\id{{a}}{{y}}"] times: [0.0] by: @id {bad}}}]}}"#);
        assert!(MotionDoc::load(&src).is_err(), "{bad}");
    }
}

#[test]
fn a_chain_of_states_runs_one_transition_after_another_and_holds_between() {
    // One layer, four states: N = 1 -> 2 -> 3 -> 25, each transition with its own start and duration.
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(600, 200) duration: 9.0 layers: [
        MatchMorph{math: true steps: ["N = \\id{d}{1}", "N = \\id{d}{2}", "N = \\id{d}{3}", "N = \\id{d}{25}"] times: [1.0, 3.0, 5.0] durations: [0.5, 0.5, 1.0]
            by: @id size: 60 at: vec2(50, 50)}]}"#);
    let count = |t: f64| pieces(&doc, t, 0).len();
    let shown = |t: f64| pieces(&doc, t, 0).iter().filter(|p| p.2 > 0.99).count();
    // Holds: the state before the first transition, between transitions, after the last.
    assert_eq!((count(0.0), shown(0.0)), (3, 3));
    assert_eq!(pieces(&doc, 1.6, 0), pieces(&doc, 2.9, 0), "state 2 holds between its transitions");
    assert_eq!(pieces(&doc, 6.0, 0), pieces(&doc, 8.9, 0), "the last state holds to the end");
    // Transitions: N and = pair, the digit swaps (old, new) -> 4 pieces mid-transition; the end has 3 at rest.
    assert_eq!(count(1.25), 4);
    assert_eq!(count(5.5), 4);
    assert_eq!(count(7.0), 3, "N = 25 at rest: N, =, and the digits as one piece");
    // The three transitions are different moments: a piece of the swap is half-faded in each.
    for t in [1.25, 3.25] {
        let o: Vec<f64> = pieces(&doc, t, 0).iter().map(|p| p.2).collect();
        assert!(o[2] < 1.0 && o[2] > 0.0 || o[3] > 0.0 && o[3] < 1.0, "{t}: {o:?}");
    }
    // Times that would start a transition before the last one is over are errors.
    let bad = r#"use mod.motion.*; Motion{size: vec2(600, 200) duration: 9.0 layers: [MatchMorph{steps: ["a", "b", "c"] times: [1.0, 1.5] duration: 1.0}]}"#;
    assert!(MotionDoc::load(bad).is_err());
}

#[test]
fn each_transition_of_a_chain_can_key_its_pieces_on_its_own() {
    // The second state names its first term, the third does not: by \id then by char, the last step pairs every character.
    let chain = |by: &str| load(&format!(r#"use mod.motion.*; Motion{{size: vec2(900, 200) duration: 9.0 layers: [
        MatchMorph{{math: true steps: ["\\id{{p}}{{a + b}} + c", "c + \\id{{p}}{{a + b}}", "c + a + b"] times: [1.0, 4.0] durations: [1.0, 1.0] by: {by} size: 60 at: vec2(50, 50)}}]}}"#));
    let keyed = chain("[@id, @char]");
    let mid = |doc: &MotionDoc| pieces(doc, 4.5, 0);
    // by char the second transition has pairs only, all fully visible while they fly
    assert!(mid(&keyed).iter().all(|p| p.2 > 0.99), "{:?}", mid(&keyed));
    // by id the characters of the unnamed state have no partners: pieces fade
    let by_id = chain("@id");
    assert!(mid(&by_id).iter().any(|p| p.2 < 0.99), "{:?}", mid(&by_id));
    // the first transition is the same either way, holds still at rest
    assert_eq!(pieces(&keyed, 2.5, 0), pieces(&keyed, 3.5, 0));
    assert_eq!(pieces(&keyed, 6.0, 0), pieces(&keyed, 8.9, 0));
    // one keying per transition: a list of the wrong length is an error
    let bad = r#"use mod.motion.*; Motion{size: vec2(600, 200) duration: 9.0 layers: [MatchMorph{math: true steps: ["a", "b", "c"] times: [1.0, 3.0] by: [@id] size: 40}]}"#;
    assert!(MotionDoc::load(bad).is_err());
}
