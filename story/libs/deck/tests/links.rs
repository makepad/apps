//! Several decks in one tree, and link slides between them.
use makepad_asset_data::AssetKind::Deck as DeckKind;
use makepad_story_deck::*;

const MAIN: &str = r#"use mod.flow.*
use mod.cut.*
use mod.deck.*
Deck{title: "Main" output: {width: 1920 height: 1080} slides: [
    Slide{name: @open sketch: "Main"}
    Slide{name: @inside sketch: "Inside"}
]}
"#;

fn talk(link: &str) -> String {
    format!(r#"use mod.flow.*
use mod.cut.*
use mod.deck.*
Deck{{title: "Talk" output: {{width: 1920 height: 1080}} slides: [
    Slide{{name: @title sketch: "Talk"}}
    Slide{{name: @jump sketch: "Jump" beats: [1.0] {link}}}
    Slide{{name: @close sketch: "Thanks"}}
]}}
"#)
}

fn tree(files: &[(&str, &str)]) -> std::sync::Arc<makepad_asset_data::tree::MemoryTree> {
    use makepad_asset_data::{tree::*, AssetId, AssetRevisionId, AssetRevisionRef};
    let mut memory = MemoryTree::default();
    for (index, (path, text)) in files.iter().enumerate() {
        let reference = AssetRevisionRef { asset_id: AssetId::from_bytes([index as u8 + 1; 16]), revision: AssetRevisionId::hash_of(text.as_bytes()) };
        memory.tree.entries.insert((*path).into(), TreeEntry::File(reference));
        let kind = if path.ends_with("deck.splash") { DeckKind } else { makepad_asset_data::AssetKind::Motion };
        memory.files.insert(reference, (kind, text.as_bytes().into()));
    }
    memory.tree.root = Some((TreeRole::Deck, "deck.splash".into()));
    std::sync::Arc::new(memory)
}

fn load(tree: &std::sync::Arc<makepad_asset_data::tree::MemoryTree>, path: &str, source: &str) -> Result<DeckDoc, Vec<Diagnostic>> {
    DeckDoc::load_tree(source, makepad_story_cut::tree::TreeContext { resolver: tree.clone(), document: path.into() })
}

#[test]
fn a_link_names_another_deck_file_of_the_tree() {
    let source = talk(r#"link: "deck.splash" link_slide: @inside"#);
    let files = tree(&[("deck.splash", MAIN), ("harbour.deck.splash", &source)]);
    let doc = load(&files, "harbour.deck.splash", &source).unwrap_or_else(|d| panic!("{d:#?}"));
    let link = doc.slides[1].link.clone().expect("the jump slide links");
    assert_eq!(link, Link { path: "deck.splash".into(), slide: Some("inside".into()) });
    let main = load(&files, "deck.splash", MAIN).unwrap();
    assert_eq!(link.entry(&main), Ok(1));
    assert!(check_links(&doc, |path| (path == "deck.splash").then_some(&main)).is_empty());
}

#[test]
fn a_link_to_a_missing_slide_or_a_non_deck_is_reported() {
    let source = talk(r#"link: "deck.splash" link_slide: @nowhere"#);
    let files = tree(&[("deck.splash", MAIN), ("harbour.deck.splash", &source), ("shared/look.splash", "use mod.motion.*\n{}")]);
    let doc = load(&files, "harbour.deck.splash", &source).unwrap();
    let main = load(&files, "deck.splash", MAIN).unwrap();
    let problems = check_links(&doc, |path| (path == "deck.splash").then_some(&main));
    assert_eq!(problems.len(), 1);
    assert!(problems[0].message.contains("no slide `nowhere`"), "{problems:?}");
    assert!(!check_links(&doc, |_| None).is_empty());

    let bad = talk(r#"link: "shared/look.splash""#);
    let errors = load(&files, "harbour.deck.splash", &bad).unwrap_err();
    assert!(errors.iter().any(|d| d.message.contains("not a deck file")), "{errors:?}");
    let itself = talk(r#"link: "harbour.deck.splash""#);
    let errors = load(&files, "harbour.deck.splash", &itself).unwrap_err();
    assert!(errors.iter().any(|d| d.message.contains("this deck itself")), "{errors:?}");
    let orphan = talk(r#"link_slide: @open"#);
    let errors = load(&files, "harbour.deck.splash", &orphan).unwrap_err();
    assert!(errors.iter().any(|d| d.message.contains("needs a `link`")), "{errors:?}");
}

#[test]
fn next_leaves_a_slide_only_from_its_end_and_a_jump_enters_from_outside() {
    let source = talk(r#"link: "deck.splash""#);
    let files = tree(&[("deck.splash", MAIN), ("harbour.deck.splash", &source)]);
    let doc = load(&files, "harbour.deck.splash", &source).unwrap();
    let mut deck = Deck::new(doc.slides.len(), 0.0);
    // A slide leaves from its end stop: a press before it sits the slide there.
    assert!(!deck.next_leaves(&doc, 5.0));
    deck.next(&doc, 5.0);
    assert!(!deck.next_leaves(&doc, 5.0), "carried on to the end, not there yet");
    assert!(deck.next_leaves(&doc, 5.0 + RUSH_SECONDS));
    deck.go(&doc, 1, 0.0);
    assert!(!deck.next_leaves(&doc, 5.0), "the jump slide holds at its beat");
    deck.next(&doc, 5.0);
    assert!(!deck.next_leaves(&doc, 5.0), "its beat is released: the story plays out first");
    deck.next(&doc, 5.0);
    assert!(deck.next_leaves(&doc, 5.0 + RUSH_SECONDS), "at its end the next Next leaves");

    let main = load(&files, "deck.splash", MAIN).unwrap();
    let mut entered = Deck::new(main.slides.len(), 10.0);
    entered.enter_from_outside(1, Transition { seconds: 4.0, effect: String::new(), dials: Vec::new() }, 10.0);
    assert_eq!(entered.current(), 1);
    let incoming = entered.incoming().expect("a transition from the left deck's picture");
    assert_eq!((incoming.from, incoming.transition.seconds), (OUTSIDE, MAX_JUMP_SECONDS));
    assert_eq!(entered.keep(), vec![0, 1], "no engine is kept for the other deck's slide");
    entered.tick(10.0 + MAX_JUMP_SECONDS);
    assert!(entered.incoming().is_none());
}
