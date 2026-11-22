//! The source map follows splices by their length delta and answers both ways.

use fab_elec_core::{BoardId, ItemId, Sel};
use fab_elec_design::TextEdit;
use fab_elec_lang::{SourceMap, SourceRange, Sources, SpanEdit};

#[test]
fn map_follows_edits_and_answers_both_ways() {
    let text = "place(\"a\", {at: [1mm 2mm]})\nplace(\"b\", {at: [3mm 4mm]})\n";
    let mut sources = Sources::new();
    let doc = sources.add("t.pcb.splash", text).unwrap();
    let (a, b) = (Sel::Item(BoardId(0), ItemId(1)), Sel::Item(BoardId(0), ItemId(2)));
    let (a_span, b_span) = (0u32..27, 28u32..55);
    let mut map = SourceMap::new();
    let range = |span: std::ops::Range<u32>| SourceRange { doc, span };
    map.follow(&SpanEdit { adds: vec![(a.clone(), range(a_span.clone())), (b.clone(), range(b_span.clone()))], ..Default::default() });
    assert_eq!(map.locate(&b), Some(range(b_span.clone())), "locate gives the statement");
    assert_eq!(map.at(doc, 30), Some(b.clone()), "at gives what the text there made");
    assert_eq!(map.at(doc, 27), None, "the newline between statements maps to nothing");

    // Widen a's first literal: `1mm` -> `10.5mm` (+3 bytes) inside a's statement.
    let edit = SpanEdit { edits: vec![TextEdit { doc, span: 17..20, text: "10.5mm".into() }], ..Default::default() };
    sources.apply(&edit.edits).unwrap();
    map.follow(&edit);
    assert_eq!(map.locate(&a), Some(range(0..30)), "an edit inside a statement grows it");
    assert_eq!(map.locate(&b), Some(range(31..58)), "a statement after the edit shifts by the delta");
    assert_eq!(&sources.doc(doc).unwrap().text[31..58], "place(\"b\", {at: [3mm 4mm]})", "the shifted span still covers its statement");

    // Insert a statement before b and remove a: one splice, adds in the text after it.
    let ins = "via(\"GND\", [5mm 5mm])\n";
    let c = Sel::Item(BoardId(0), ItemId(3));
    let edit = SpanEdit {
        edits: vec![TextEdit { doc, span: 0..31, text: String::new() }, TextEdit { doc, span: 31..31, text: ins.into() }],
        adds: vec![(c.clone(), range(0..21))],
        written: vec![],
        removes: vec![a.clone()],
        reevaluate: false,
        ..SpanEdit::default()
    };
    sources.apply(&edit.edits).unwrap();
    map.follow(&edit);
    assert_eq!(map.locate(&a), None, "a removed statement leaves the map");
    assert_eq!(map.locate(&c), Some(range(0..21)), "an added statement enters the map");
    assert_eq!(map.locate(&b), Some(range(22..49)), "an insertion at a statement's start goes before it");
    assert_eq!(map.at(doc, 25), Some(b), "at follows the moved statement");
    assert_eq!(sources.line_col(doc, 22), Some((2, 1)), "line and column are 1-based");

    let bad = [TextEdit { doc, span: 3..9, text: "x".into() }, TextEdit { doc, span: 5..6, text: "y".into() }];
    let before = sources.doc(doc).unwrap().text.clone();
    assert!(sources.apply(&bad).is_err(), "overlapping edits are refused");
    assert_eq!(sources.doc(doc).unwrap().text, before, "a refused apply changes nothing");
}
