//! Sketch slides: the `sketch` field, the ops that write it, and the
//! default layout drawing the words without any AI.

use makepad_asset_data::AssetKind;
use makepad_story_cut::graphic::{motion_inputs, GraphicSrc};
use makepad_story_deck::ops::*;
use makepad_story_deck::sketch::{SketchText, LAYOUT_PATH};
use makepad_story_deck::*;

const LOOK: &[u8] = br#"use mod.motion.*
{
    INK: #173020 INK2: #22412d BONE: #f8fff3 ASH: #dce8cd COOL: #b8db7e
    TITLE: Font{family: "Roboto Flex" weight: 600}
    TEXT: Font{family: "Roboto Flex" weight: 400}
}"#;
const PLAIN: &[u8] = b"use mod.motion.*\nMotion{size: vec2(1920, 1080) duration: 3.0 layers: [Rect{size: vec2(10, 10) fill: #ff0000}]}\n";

fn memory_tree(files: &[(&str, AssetKind, &[u8])]) -> makepad_story_cut::tree::TreeContext {
    use makepad_asset_data::{tree::*, AssetId, AssetRevisionId, AssetRevisionRef};
    let mut memory = MemoryTree::default();
    for (index, (path, kind, bytes)) in files.iter().enumerate() {
        let reference = AssetRevisionRef { asset_id: AssetId::from_bytes([index as u8 + 1; 16]), revision: AssetRevisionId::hash_of(bytes) };
        memory.tree.entries.insert((*path).into(), TreeEntry::File(reference));
        memory.files.insert(reference, (*kind, (*bytes).into()));
    }
    memory.tree.root = Some((TreeRole::Deck, "deck.splash".into()));
    TreeContext { resolver: std::sync::Arc::new(memory), document: "deck.splash".into() }
}

/// A deck tree holding `deck`, a garden look and one plain slide graphic.
fn tree_of(deck: &str) -> makepad_story_cut::tree::TreeContext {
    memory_tree(&[
        ("deck.splash", AssetKind::Deck, deck.as_bytes()),
        ("shared/garden.splash", AssetKind::Motion, LOOK),
        ("slides/plain.splash", AssetKind::Motion, PLAIN),
    ])
}

fn deck(look: &str, slides: &str) -> String {
    format!("use mod.flow.*\nuse mod.cut.*\nuse mod.deck.*\n\nDeck{{\n    title: \"Sketches\"\n{look}    slides: [\n{slides}    ]\n}}\n")
}

fn slide(fields: &str) -> String {
    format!("        Slide{{\n            name: @s\n            {fields}\n        }}\n")
}

fn load(source: &str) -> DeckDoc {
    DeckDoc::load_tree(source, tree_of(source)).unwrap_or_else(|diags| panic!("{diags:#?}\n{source}"))
}

fn errors(source: &str) -> Vec<Diagnostic> {
    DeckDoc::load_tree(source, tree_of(source)).err().unwrap_or_else(|| panic!("should not load:\n{source}"))
}

#[test]
fn a_sketch_alone_is_a_slide_shown_in_the_default_layout() {
    let source = deck("", &slide("sketch: \"Why sketch\\n- Type it\\n- Then fancy it\""));
    let doc = load(&source);
    let slide = &doc.slides[0];
    assert_eq!(slide.sketch.as_deref(), Some("Why sketch\n- Type it\n- Then fancy it"));
    assert!(slide.graphic.is_none() && slide.sketch_only());
    let SlideCut::Inline(cut) = &slide.cut else { panic!("the layout is a cut of one graphic") };
    let GraphicSrc::Tree(context) = &cut.graphics[0].src else { panic!("the layout is a tree document") };
    assert_eq!(context.document, LAYOUT_PATH);
    assert!(cut.duration > 0.0);
    // Without a tree the neutral look serves the same layout.
    let plain = DeckDoc::load(&source).unwrap_or_else(|diags| panic!("{diags:#?}"));
    assert!(matches!(&plain.slides[0].cut, SlideCut::Inline(_)));
}

#[test]
fn an_empty_sketch_is_still_a_sketch() {
    let doc = load(&deck("", &slide("sketch: \"\"")));
    assert_eq!(doc.slides[0].sketch.as_deref(), Some(""));
    assert_eq!(SketchText::parse(""), SketchText::default());
}

#[test]
fn a_sketch_is_kept_as_written_lines_quotes_and_escapes() {
    let doc = load(&deck("", &slide(r#"sketch: "Say \"hi\" \\ back\n\tindented\n\n- last""#)));
    assert_eq!(doc.slides[0].sketch.as_deref(), Some("Say \"hi\" \\ back\n\tindented\n\n- last"));
    let text = SketchText::parse(doc.slides[0].sketch.as_deref().unwrap());
    assert_eq!(text.title, "Say \"hi\" \\ back");
    assert_eq!(text.lines, ["indented", "last"]);
}

#[test]
fn a_sketch_that_is_not_text_is_reported_at_its_field() {
    for value in ["3", "@words", "[\"a\", \"b\"]", "{title: \"a\"}", "true"] {
        let source = deck("", &slide(&format!("sketch: {value}")));
        let diags = errors(&source);
        let line = source.lines().position(|line| line.contains("sketch:")).unwrap() as u32 + 1;
        assert!(diags.iter().any(|d| d.line == line && d.message.contains("`sketch` is the slide's text") && d.slide.as_deref() == Some("s")), "{value}: {diags:#?}");
        // Its type is the one error: no second complaint about the slide's picture.
        assert_eq!(diags.len(), 1, "{value}: {diags:#?}");
    }
    let diags = errors(&deck("", &slide("notes: \"no picture\"")));
    assert!(diags.iter().any(|d| d.message.contains("or only a sketch")), "{diags:#?}");
}

#[test]
fn an_attached_graphic_is_the_picture_and_the_sketch_stays() {
    let doc = load(&deck("", &slide("sketch: \"Title\\n- point\"\n            graphic: \"slides/plain\"")));
    let fancied = &doc.slides[0];
    assert_eq!(fancied.graphic.as_deref(), Some("slides/plain.splash"));
    assert_eq!(fancied.sketch.as_deref(), Some("Title\n- point"));
    assert!(!fancied.sketch_only());
    assert_eq!(fancied.cut.duration(), Some(3.0));
    let diags = errors(&deck("", &slide("sketch: \"T\"\n            graphic: \"slides/plain\"\n            app: \"calculator\"")));
    assert!(diags.iter().any(|d| d.message.contains("exactly one of graphic, app or cut")), "{diags:#?}");
}

#[test]
fn the_deck_names_its_look_in_its_tree() {
    let doc = load(&deck("    look: \"shared/garden\"\n", &slide("sketch: \"T\"")));
    assert_eq!(doc.look.as_deref(), Some("shared/garden.splash"));
    let source = deck("    look: \"shared/missing_look\"\n", &slide("sketch: \"T\""));
    let diags = errors(&source);
    let line = source.lines().position(|line| line.contains("look:")).unwrap() as u32 + 1;
    assert!(diags.iter().any(|d| d.line == line), "{diags:#?}");
    let diags = errors(&deck("    look: 7\n", &slide("sketch: \"T\"")));
    assert!(diags.iter().any(|d| d.message.contains("`look` is a tree path string")), "{diags:#?}");
}

/// What the slide's picture draws at `t` = 0: the text of every Draw2D text op.
fn drawn_text(doc: &DeckDoc, index: usize) -> Vec<String> {
    let SlideCut::Inline(cut) = &doc.slides[index].cut else { panic!("an inline cut") };
    let graphic = &cut.graphics[0];
    let GraphicSrc::Tree(context) = &graphic.src else { panic!("a tree graphic") };
    let file = context.resolve(&context.document).unwrap();
    let modules = makepad_motion::modules::Modules::from_tree(context.clone());
    let motion = makepad_motion::MotionDoc::load_with(file.text().unwrap(), &motion_inputs(&graphic.params), makepad_motion::MotionContext { modules, ..Default::default() })
        .unwrap_or_else(|diags| panic!("{diags:#?}"));
    assert!(motion.warnings.is_empty(), "{:#?}", motion.warnings);
    let frame = motion.eval(0.0);
    assert!(frame.errors.is_empty(), "{:#?}", frame.errors);
    assert_eq!(frame.size, [doc.output.0 as f64, doc.output.1 as f64]);
    let mut texts = Vec::new();
    for item in &frame.items {
        if let makepad_motion::ItemKind::Draw2d { picture, .. } = &item.kind {
            for op in &picture.list.ops {
                if let makepad_motion::draw2d::Op::Text(text) = op { texts.push(text.text.clone()); }
            }
        }
    }
    texts
}

#[test]
fn a_sketch_slide_draws_its_words_without_ai() {
    let words = "Sketch first\n- Type a title and two points\n- See the slide at once";
    // In the deck's look, and in the neutral one.
    for look in ["    look: \"shared/garden\"\n", ""] {
        let doc = load(&deck(look, &slide(&format!("sketch: {}", makepad_story_cut::source::string_literal(words)))));
        let texts = drawn_text(&doc, 0);
        assert!(texts.iter().any(|text| text == "Sketch first"), "{look}: {texts:?}");
        let body = texts.join(" ");
        assert!(body.contains("Type a title") && body.contains("See the slide"), "{look}: {texts:?}");
        assert!(!body.contains("- "), "the marks are drawn, not written: {texts:?}");
    }
    // A long bullet wraps onto more rows, every word kept.
    let long = "Title\n- one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty";
    let doc = load(&deck("", &slide(&format!("sketch: {}", makepad_story_cut::source::string_literal(long)))));
    let texts = drawn_text(&doc, 0);
    assert!(texts.len() > 2, "{texts:?}");
    assert_eq!(texts[1..].join(" "), long.split_once("- ").unwrap().1);
}

fn tree_run(source: &str, op: DeckOp) -> (String, DeckDoc) {
    apply_in(source, &op, Some(tree_of(source))).unwrap_or_else(|error| panic!("{op:?}: {error}"))
}

#[test]
fn sketch_ops_add_set_and_attach_each_in_one_change() {
    let source = deck("", &slide("graphic: \"slides/plain\""));
    // Added right after the slide, with the typed words.
    let (written, doc) = tree_run(&source, DeckOp::AddSlide { after: Some(0), name: "idea".into(), cut: NewCut::Sketch { text: String::new() } });
    assert_eq!(doc.slides.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["s", "idea"]);
    assert!(written.contains("name: @idea\n            sketch: \"\""), "{written}");
    assert!(doc.slides[1].sketch_only());
    // The words typed later, quotes and lines and all.
    let words = "A \"plan\"\n- first\n- second";
    let (written, doc) = tree_run(&written, DeckOp::SetSlide { index: 1, set: SlideSet { sketch: Some(words.into()), ..SlideSet::default() } });
    assert_eq!(doc.slides[1].sketch.as_deref(), Some(words));
    assert!(written.contains(r#"sketch: "A \"plan\"\n- first\n- second""#), "{written}");
    // A graphic attached: the picture changes, the sketch stays.
    let (written, doc) = tree_run(&written, DeckOp::SetSlide { index: 1, set: SlideSet { graphic: Some(Some("slides/plain".into())), ..SlideSet::default() } });
    assert_eq!(doc.slides[1].graphic.as_deref(), Some("slides/plain.splash"));
    assert_eq!(doc.slides[1].sketch.as_deref(), Some(words));
    // And taken off again: back to the default layout.
    let (_, doc) = tree_run(&written, DeckOp::SetSlide { index: 1, set: SlideSet { graphic: Some(None), ..SlideSet::default() } });
    assert!(doc.slides[1].sketch_only());
    // A slide without a sketch cannot lose its graphic.
    assert!(apply_in(&written, &DeckOp::SetSlide { index: 0, set: SlideSet { graphic: Some(None), ..SlideSet::default() } }, Some(tree_of(&written))).is_err());
}
