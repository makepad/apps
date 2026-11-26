//! `CutDoc::picture_is_still`: whether a cut's picture can change with time.

use makepad_story_cut::*;

const REV: &str = "arev_5ab3f998fd4a7b6860a9337b9924be40ad341d12276e341079ce2093539c715f";
const ID: &str = "ast_aaaaaaaaaaaaaaaaaaaaaaaaaa";

fn cut(body: &str, nodes: &str) -> CutDoc {
    let source = format!("use mod.flow.*\nuse mod.cut.*\n{body}\nFlow{{\n    kind: @cut\n    output: {{width: 640 height: 360 fps: 30}}\n    duration: 6.0\n    {nodes}\n}}\n");
    CutDoc::load(&source).unwrap_or_else(|errors| panic!("{errors:#?}\n{source}"))
}

const COUNTER: &str = "let title = Title{text: \"Counter\" span: [0.0, 6.0] anchor: vec2(0.5, 0.14) font_size: 84.0}\nlet view = AppView{app: @counter fit: @fit span: [0.0, 6.0]}\nlet s = Stack{layers: [view.out(@frame), title.out(@frame)]}\nlet out = CutOutput{frame: s.out(@frame)}";

#[test]
fn a_static_title_over_an_app_for_the_whole_cut_is_still() {
    let doc = cut(COUNTER, "title, view, s, out");
    assert!(doc.picture_is_still());
    assert_eq!(doc.motion_nodes().collect::<Vec<_>>(), ["title"]);
}

#[test]
fn a_node_without_a_span_is_always_there() {
    assert!(cut(&COUNTER.replace(" span: [0.0, 6.0] anchor", " anchor").replace(" span: [0.0, 6.0]}", "}"), "title, view, s, out").picture_is_still());
}

#[test]
fn a_keyed_title_size_moves() {
    let body = COUNTER.replace("font_size: 84.0", "font_size: keys([0.0, 80.0], [4.0, 90.0])");
    assert!(!cut(&body, "title, view, s, out").picture_is_still());
}

#[test]
fn a_node_that_comes_and_goes_moves() {
    let body = COUNTER.replace("title = Title{text: \"Counter\" span: [0.0, 6.0]", "title = Title{text: \"Counter\" span: [1.0, 2.0]");
    assert!(!cut(&body, "title, view, s, out").picture_is_still());
}

#[test]
fn a_clip_an_effect_or_a_transition_moves() {
    let clip = format!("let a = Clip{{src: asset(\"{ID}\", \"{REV}\")}}\nlet out = CutOutput{{frame: a.out(@frame)}}");
    assert!(!cut(&clip, "a, out").picture_is_still());
    let fx = format!("let a = Still{{src: asset(\"{ID}\", \"{REV}\")}}\nlet f = Fx{{input: a.out(@frame) effect: \"kit/grain\"}}\nlet out = CutOutput{{frame: f.out(@frame)}}");
    assert!(!cut(&fx, "a, f, out").picture_is_still());
    let still = format!("let a = Still{{src: asset(\"{ID}\", \"{REV}\")}}\nlet out = CutOutput{{frame: a.out(@frame)}}");
    assert!(cut(&still, "a, out").picture_is_still());
}

#[test]
fn a_graphic_is_a_motion_node_and_a_keyed_one_moves() {
    let g = format!("let g = Graphic{{src: asset(\"{ID}\", \"{REV}\")}}\nlet out = CutOutput{{frame: g.out(@frame)}}");
    let doc = cut(&g, "g, out");
    assert!(doc.picture_is_still());
    assert_eq!(doc.motion_nodes().collect::<Vec<_>>(), ["g"]);
    let keyed = g.replace(&format!("\"{REV}\")"), &format!("\"{REV}\") opacity: keys([0.0, 0.0], [1.0, 1.0])"));
    assert!(!cut(&keyed, "g, out").picture_is_still());
}
