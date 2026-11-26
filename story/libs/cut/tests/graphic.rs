//! Motion graphics in cuts: `Graphic` nodes load, pin
//! their documents, and draw as `DrawKind::Graphic` on their own clock.

use makepad_story_cut::graphic::InputValue;
use makepad_story_cut::*;

const REV: &str = "arev_5ab3f998fd4a7b6860a9337b9924be40ad341d12276e341079ce2093539c715f";

fn id(letter: char) -> String {
    format!("ast_{}a", letter.to_string().repeat(25))
}

fn edit(body: &str, nodes: &str) -> String {
    format!("use mod.flow.*\nuse mod.cut.*\n{body}\nFlow{{\n    kind: @cut\n    output: {{width: 1280 height: 720 fps: 30}}\n    duration: 10.0\n    bpm: 100\n    {nodes}\n}}\n")
}

#[test]
fn a_graphic_loads_with_its_params_and_pins_its_document() {
    let body = format!(
        "let g = Graphic{{src: asset(\"{a}\", \"{REV}\") span: [2.0, 4.0] params: {{headline: \"Hi\" accent: \"#ff5a36\" size: 3 at: [1, 2] logo: asset(\"{b}\", \"{REV}\")}} opacity: keys([0.0, 0.0], [1.0, 1.0])}}\nlet out = CutOutput{{frame: g.out(@frame)}}",
        a = id('a'), b = id('b'),
    );
    let doc = CutDoc::load(&edit(&body, "g, out")).unwrap_or_else(|diags| panic!("{diags:#?}"));
    assert!(doc.nodes.iter().all(|node| node.id != "g"), "graphics live beside the nodes");
    let g = doc.graphic("g").unwrap();
    assert_eq!(g.src, GraphicSrc::Asset(AssetRef { id: id('a'), rev: REV.into() }));
    assert_eq!(g.fit, Fit::Fit, "a graphic letterboxes by default");
    assert_eq!(g.span, Some(Span { start: 2.0, length: 4.0 }));
    assert!(g.params.contains(&("headline".into(), InputValue::Text("Hi".into()))));
    assert!(g.params.contains(&("accent".into(), InputValue::Text("#ff5a36".into()))));
    assert!(g.params.contains(&("at".into(), InputValue::Vec2([1.0, 2.0]))));
    let deps = dependencies(&doc);
    assert!(deps.contains(&Dep::Asset(AssetRef { id: id('a'), rev: REV.into() })));
    assert!(deps.contains(&Dep::Asset(AssetRef { id: id('b'), rev: REV.into() })), "asset params are pinned too");

    let eval = CutEval::new(&doc);
    assert_eq!(eval.frame_plan(1.0).result, None, "not before its span");
    let plan = eval.frame_plan(2.5);
    let op = &plan.ops[plan.result.unwrap()];
    assert_eq!(op.size, (1280, 720));
    let DrawKind::Graphic { src, local_time, offset, bpm, opacity, fit, .. } = &op.kind else { panic!("{plan:#?}") };
    assert_eq!(src, &g.src);
    assert!((local_time - 0.5).abs() < 1e-9 && (offset - 2.0).abs() < 1e-9);
    assert_eq!((*bpm, *fit), (100.0, Fit::Fit));
    assert!((opacity - 0.5).abs() < 1e-9, "opacity is keyed on the span's clock");
}

#[test]
fn graphic_problems_are_reported_where_they_are_written() {
    let body = "let g = Graphic{src: cut(\"ast_x\", \"arev_y\") params: {x: {y: 1}}}\nlet out = CutOutput{frame: g.out(@frame)}";
    let diags = CutDoc::load(&edit(body, "g, out")).unwrap_err();
    assert!(diags.iter().any(|d| d.node.as_deref() == Some("g") && d.line == 3), "{diags:#?}");
    let body = format!("let g = Graphic{{src: asset(\"{}\", \"{REV}\") params: {{x: {{y: 1}}}}}}\nlet out = CutOutput{{frame: g.out(@frame)}}", id('a'));
    let diags = CutDoc::load(&edit(&body, "g, out")).unwrap_err();
    assert!(diags.iter().any(|d| d.message.contains("`g.params.x`")), "{diags:#?}");
}

#[test]
fn a_graphic_layers_in_a_stack_and_a_title_lowers_to_the_title_document() {
    let body = format!(
        "let g = Graphic{{src: asset(\"{}\", \"{REV}\")}}\nlet t = Title{{text: \"Hello\" anchor: vec2(0.25, 0.75) font_size: 40}}\nlet s = Stack{{layers: [g.out(@frame), t.out(@frame)]}}\nlet out = CutOutput{{frame: s.out(@frame)}}",
        id('a'),
    );
    let doc = CutDoc::load(&edit(&body, "g, t, s, out")).unwrap_or_else(|diags| panic!("{diags:#?}"));
    let plan = CutEval::new(&doc).frame_plan(3.0);
    assert_eq!(plan.ops.iter().map(|op| op.node.as_str()).collect::<Vec<_>>(), ["g", "t", "s"]);
    let DrawKind::Graphic { src: GraphicSrc::Builtin(name), params, fit, .. } = &plan.ops[1].kind else { panic!() };
    assert_eq!((name.as_str(), *fit), ("title", Fit::None));
    assert_eq!(params, &graphic::title_params("Hello", [0.25, 0.75], 40.0, 1280, 720));
}
