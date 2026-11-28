//! List-valued inputs and `Repeat{source: …}`: one copy per item, each
//! with its `item()` fields filled, set by the host like any input.

use makepad_motion::plain::PrimitiveFrame;
use makepad_motion::*;

/// The texts of a frame's Repeat copies, in order.
fn texts(doc: &MotionDoc, t: f64) -> Vec<String> {
    fn walk(items: &[Item], out: &mut Vec<String>) {
        for item in items {
            match &item.kind {
                ItemKind::Primitive(PrimitiveFrame::Text { text, .. }) => out.push(text.clone()),
                ItemKind::Group { children, .. } => walk(children, out),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(&doc.eval(t).items, &mut out);
    out
}

const MENU: &str = r#"use mod.motion.*; Motion{size: vec2(640, 360) duration: 2
    inputs: {items: ["START GAME", "OPTIONS", "CREDITS"] focused: 0}
    layers: [
        Repeat{source: in(@items) layout: @grid columns: 1 spacing: vec2(0, 64) at: vec2(320, 100) delay: 0.1
            layer: Text{text: item() size: 32 color: #fff}}
    ]
}"#;

#[test]
fn a_list_input_makes_one_copy_per_item() {
    let doc = MotionDoc::load(MENU).unwrap();
    assert_eq!(doc.inputs.iter().map(|i| (i.name.as_str(), i.ty)).collect::<Vec<_>>(), vec![("items", InputType::List), ("focused", InputType::Number)]);
    assert_eq!(texts(&doc, 1.0), vec!["START GAME", "OPTIONS", "CREDITS"]);
    // The copies sit on the layout, one under the other.
    let frame = doc.eval(1.0);
    let ItemKind::Group { children, .. } = &frame.items[0].kind else { panic!() };
    let ys: Vec<f64> = children.iter().map(|c| c.world.apply([0.0, 0.0])[1]).collect();
    assert_eq!(ys, vec![100.0, 164.0, 228.0]);
    // The host sets the list like any input.
    let two = doc.with_inputs(&[(LiveId::from_str("items"), InputValue::List(vec![InputValue::Text("PLAY".into()), InputValue::Text("QUIT".into())]))]).unwrap();
    assert_eq!(texts(&two, 1.0), vec!["PLAY", "QUIT"]);
    let none = doc.with_inputs(&[(LiveId::from_str("items"), InputValue::List(Vec::new()))]).unwrap();
    assert!(texts(&none, 1.0).is_empty());
}

#[test]
fn records_fill_fields_and_input_reads_a_declared_list() {
    let source = r#"use mod.motion.*; Motion{size: vec2(640, 360) duration: 2
    inputs: {rows: input(@list, [{name: "Ada", score: "12"}, {name: "Lin", score: "9"}])}
    layers: [
        Repeat{source: input(@rows) layout: @grid columns: 1 spacing: vec2(0, 40)
            layer: Group{layers: [Text{text: item(@name) size: 20} Text{text: item(@score) at: vec2(200, 0) size: 20}]}}
    ]
}"#;
    let doc = MotionDoc::load(source).unwrap();
    assert_eq!(texts(&doc, 0.5), vec!["Ada", "12", "Lin", "9"]);
    let rows = InputValue::List(vec![InputValue::Record(vec![("name".into(), InputValue::Text("Max".into())), ("score".into(), InputValue::Text("30".into()))])]);
    let set = doc.with_inputs(&[(LiveId::from_str("rows"), rows)]).unwrap();
    assert_eq!(texts(&set, 0.5), vec!["Max", "30"]);
}

#[test]
fn source_problems_say_what_to_write() {
    let missing = MotionDoc::load(r#"use mod.motion.*; Motion{duration: 1 inputs: {rows: [{name: "a"}, {name: "b"}]}
        layers: [Repeat{source: in(@rows) layer: Text{text: item(@title)}}]}"#).unwrap_err();
    assert!(missing.iter().any(|d| d.message.contains("has no field `title`")), "{missing:?}");
    let stray = MotionDoc::load(r#"use mod.motion.*; Motion{duration: 1 layers: [Repeat{count: 3 layer: Text{text: item()}}]}"#).unwrap_err();
    assert!(stray.iter().any(|d| d.message.contains("`item()` reads the item of a Repeat with a `source`")), "{stray:?}");
    // `in(@title)` is the input's value when the document evaluates.
    let not_list = MotionDoc::load(r#"use mod.motion.*; Motion{duration: 1 inputs: {title: "x"}
        layers: [Repeat{source: in(@title) layer: Text{text: item()}}]}"#).unwrap_err();
    assert_eq!(not_list.len(), 1, "{not_list:?}");
    assert!(not_list[0].message.contains("`source` is a list to make one copy per item of, not the string \"x\""), "{not_list:?}");
    let not_list = MotionDoc::load(r#"use mod.motion.*; Motion{duration: 1 inputs: {title: "x"}
        layers: [Repeat{source: input(@title) layer: Text{text: item()}}]}"#).unwrap_err();
    assert!(not_list.iter().any(|d| d.message.contains("`source` is a list, but input `title` is text")), "{not_list:?}");
    let both = MotionDoc::load(r#"use mod.motion.*; Motion{duration: 1 layers: [Repeat{source: ["a", "b"] count: 4 layer: Text{text: item()}}]}"#).unwrap_err();
    assert!(both.iter().any(|d| d.message.contains("leave `count` out")), "{both:?}");
}

#[test]
fn nested_repeats_keep_their_own_items_and_counted_repeats_are_unchanged() {
    let doc = MotionDoc::load(r#"use mod.motion.*; Motion{duration: 1 inputs: {groups: [{title: "A", names: ["a1", "a2"]}, {title: "B", names: ["b1"]}]}
        layers: [Repeat{source: in(@groups) layer: Group{layers: [
            Text{text: item(@title)}
            Repeat{source: item(@names) layer: Text{text: item()}}
        ]}}]}"#).unwrap();
    assert_eq!(texts(&doc, 0.5), vec!["A", "a1", "a2", "B", "b1"]);
    let counted = MotionDoc::load(r#"use mod.motion.*; Motion{duration: 1 layers: [Repeat{count: 3 layer: Text{text: "x"}}]}"#).unwrap();
    assert_eq!(texts(&counted, 0.5), vec!["x", "x", "x"]);
}
