use makepad_story_deck::ops::*;
use makepad_story_deck::*;

const TOUR: &str = include_str!("fixtures/custom_deck.splash");

const AST_A: &str = "ast_yqzlg4xa4mbgpzs6e2qsuqwhsu";
const AST_B: &str = "ast_qdyytgcolstqfb6rgnbpnwva3m";
const REV: &str = "arev_5ab3f998fd4a7b6860a9337b9924be40ad341d12276e341079ce2093539c715f";

fn names(doc: &DeckDoc) -> Vec<&str> {
    doc.slides.iter().map(|slide| slide.name.as_str()).collect()
}

fn run(source: &str, op: DeckOp) -> (String, DeckDoc) {
    apply(source, &op).unwrap_or_else(|error| panic!("{op:?}: {error}"))
}

#[test]
fn spans_are_the_written_records() {
    let doc = DeckDoc::load(TOUR).unwrap();
    for slide in &doc.slides {
        assert!(TOUR[..slide.span.0].ends_with("Slide"), "{}", slide.name);
        assert!(TOUR[slide.span.0..slide.span.1].starts_with('{') && TOUR[slide.span.0..slide.span.1].ends_with('}'));
        let flow = slide.cut_span.expect("the tour's edits are written in the file");
        assert!(TOUR[..flow.0].ends_with("Flow"), "{}: {:?}", slide.name, &TOUR[flow.0.saturating_sub(12)..flow.0]);
    }
}

#[test]
fn a_blank_slide_is_added_after_a_slide_and_nothing_else_changes() {
    let (written, doc) = run(TOUR, DeckOp::AddSlide { after: Some(1), name: "extra".into(), cut: NewCut::Blank });
    assert_eq!(names(&doc), ["intro", "title", "extra", "counter", "source_app", "scope", "closing"]);
    assert_eq!(doc.slides[2].cut.duration(), Some(6.0));
    assert!(written.contains("let extra_main = Stack{layers: []}"));
    // Removing what the op wrote gives the file back.
    let (before_lets, after_lets) = written.split_once("let extra_main").unwrap();
    let (_, rest) = after_lets.split_once("Deck{").unwrap();
    let slide_at = rest.find("        Slide{\n            name: @extra").unwrap();
    let slide_end = rest[slide_at..].find("        Slide{\n            name: @counter").unwrap();
    let restored = format!("{before_lets}Deck{{{}{}", &rest[..slide_at - 1], &rest[slide_at + slide_end - 1..]);
    assert_eq!(restored.replace("\n\n\n", "\n\n"), TOUR.replace("\n\n\n", "\n\n"), "{written}");
}

#[test]
fn a_slide_is_added_at_the_end_and_names_are_checked() {
    let (_, doc) = run(TOUR, DeckOp::AddSlide { after: None, name: "last".into(), cut: NewCut::Cut { id: AST_B.into(), rev: REV.into(), seconds: Some(7.0) } });
    assert_eq!(names(&doc).last(), Some(&"last"));
    assert_eq!(doc.slides[6].cut.duration(), Some(7.0));
    assert!(apply(TOUR, &DeckOp::AddSlide { after: None, name: "intro".into(), cut: NewCut::Blank }).is_err());
    assert!(apply(TOUR, &DeckOp::AddSlide { after: None, name: "2bad".into(), cut: NewCut::Blank }).is_err());
    assert!(apply(TOUR, &DeckOp::AddSlide { after: Some(9), name: "x".into(), cut: NewCut::Blank }).is_err());
}

#[test]
fn slides_move_both_ways() {
    let (a, doc) = run(TOUR, DeckOp::MoveSlide { from: 0, to: 2 });
    assert_eq!(names(&doc), ["title", "counter", "intro", "source_app", "scope", "closing"]);
    let (_, doc) = run(&a, DeckOp::MoveSlide { from: 5, to: 0 });
    assert_eq!(names(&doc), ["closing", "title", "counter", "intro", "source_app", "scope"]);
    let (same, _) = run(TOUR, DeckOp::MoveSlide { from: 1, to: 1 });
    assert_eq!(same, TOUR);
    // Moving there and back is the same slides in the same order.
    let (back, doc) = run(&a, DeckOp::MoveSlide { from: 2, to: 0 });
    assert_eq!(names(&doc), ["intro", "title", "counter", "source_app", "scope", "closing"]);
    assert_eq!(back.split_whitespace().collect::<Vec<_>>(), TOUR.split_whitespace().collect::<Vec<_>>());
}

#[test]
fn slide_drop_gaps_map_to_final_indices() {
    assert_eq!(slide_drop_index(4, 1, 1), None); // Above self.
    assert_eq!(slide_drop_index(4, 1, 2), None); // Below self.
    assert_eq!(slide_drop_index(4, 0, 4), Some(3)); // First to last.
    assert_eq!(slide_drop_index(4, 3, 0), Some(0)); // Last to first.
    assert_eq!(slide_drop_index(4, 2, 1), Some(1));
    assert_eq!(slide_drop_index(4, 0, 3), Some(2));
    assert_eq!(slide_drop_index(4, 0, 0), None);
    assert_eq!(slide_drop_index(4, 3, 4), None);
    assert_eq!(slide_drop_index(1, 0, 1), None);
    assert_eq!(slide_drop_index(0, 0, 0), None);
    assert_eq!(slide_drop_index(4, 4, 0), None);
    assert_eq!(slide_drop_index(4, 0, 5), None);
}

#[test]
fn one_reorder_preserves_slide_contents_and_can_be_restored() {
    let before = DeckDoc::load(TOUR).unwrap();
    let original: Vec<_> = names(&before).into_iter().map(str::to_string).collect();
    let mut order = original.clone();
    let to = slide_drop_index(order.len(), 0, order.len()).unwrap();
    let moved = order.remove(0);
    order.insert(to, moved);
    let (written, after) = run(TOUR, DeckOp::ReorderSlides { slides: order.clone() });
    assert_eq!(names(&after), order.iter().map(String::as_str).collect::<Vec<_>>());
    for slide in &after.slides {
        let old = before.slides.iter().find(|old| old.name == slide.name).unwrap();
        assert_eq!(&written[slide.span.0..slide.span.1], &TOUR[old.span.0..old.span.1]);
    }
    let (restored, _) = run(&written, DeckOp::ReorderSlides { slides: original.clone() });
    assert_eq!(restored, TOUR);
    assert_eq!(run(TOUR, DeckOp::ReorderSlides { slides: original.clone() }).0, TOUR);
    order[0] = order[1].clone();
    assert!(apply(TOUR, &DeckOp::ReorderSlides { slides: order }).is_err());
    assert!(apply(TOUR, &DeckOp::ReorderSlides { slides: original[..2].to_vec() }).is_err());
}

#[test]
fn deck_target_rescales_only_timed_slides_and_rounds_on_the_longest() {
    let before = DeckDoc::load(TOUR).unwrap();
    let (written, after) = run(TOUR, DeckOp::SetDeckTarget { seconds: 1800.0 });
    // 20, 30, 60, 90, 120 at 5.625x; the -1 second rounding
    // remainder belongs to the longest slide, which becomes 674.
    assert_eq!(after.slides.iter().map(|slide| slide.target_seconds).collect::<Vec<_>>(),
        [Some(113.0), Some(169.0), Some(338.0), Some(506.0), Some(674.0), None]);
    assert_eq!(after.slides.iter().filter_map(|slide| slide.target_seconds).sum::<f64>(), 1800.0);
    assert_eq!(names(&before), names(&after));
    for (old, slide) in before.slides.iter().zip(&after.slides) {
        assert_eq!(old.notes, slide.notes);
        assert_eq!(old.cut.duration(), slide.cut.duration());
        assert_eq!(old.target_seconds.is_none(), slide.target_seconds.is_none());
    }
    assert_eq!(run(&written, DeckOp::SetDeckTarget { seconds: 1800.0 }).0, written);
    for seconds in [0.0, -1.0, 1.5, f64::NAN, f64::INFINITY] {
        assert!(apply(TOUR, &DeckOp::SetDeckTarget { seconds }).is_err());
    }
    let untimed = new_deck_source("Untimed").unwrap();
    assert!(apply(&untimed, &DeckOp::SetDeckTarget { seconds: 1800.0 }).unwrap_err().contains("speaking target"));
    // Short totals still load, including zero-second shares; no target is lost.
    let (_, short) = run(TOUR, DeckOp::SetDeckTarget { seconds: 1.0 });
    assert_eq!(short.slides.iter().filter_map(|slide| slide.target_seconds).sum::<f64>(), 1.0);
    assert_eq!(short.slides.iter().filter(|slide| slide.target_seconds.is_some()).count(), before.slides.iter().filter(|slide| slide.target_seconds.is_some()).count());
}

#[test]
fn deck_target_uses_the_first_longest_slide_and_keeps_small_totals_nonnegative() {
    let mut source = TOUR.to_string();
    for (index, target) in [35.0, 35.0, 30.0, 30.0, 30.0].into_iter().enumerate() {
        source = run(&source, DeckOp::SetSlide { index, set: SlideSet {
            target_seconds: Some(Some(target)), ..SlideSet::default()
        } }).0;
    }
    let (_, after) = run(&source, DeckOp::SetDeckTarget { seconds: 1800.0 });
    // The tied longest shares round to 394, the others to 338. The
    // first longest absorbs the -2 second remainder.
    assert_eq!(after.slides.iter().map(|slide| slide.target_seconds).collect::<Vec<_>>(),
        [Some(392.0), Some(394.0), Some(338.0), Some(338.0), Some(338.0), None]);
    let (_, short) = run(&source, DeckOp::SetDeckTarget { seconds: 3.0 });
    assert_eq!(short.slides.iter().map(|slide| slide.target_seconds).collect::<Vec<_>>(),
        [Some(0.0), Some(0.0), Some(1.0), Some(1.0), Some(1.0), None]);
}

#[test]
fn a_slide_is_deleted_with_the_nodes_only_it_used() {
    let (written, doc) = run(TOUR, DeckOp::DeleteSlide { index: 1 });
    assert_eq!(names(&doc), ["intro", "counter", "source_app", "scope", "closing"]);
    assert!(!written.contains("words_glitch") && !written.contains("let words"), "{written}");
    assert!(written.contains("let motion_title"));
    let single = "use mod.flow.*\nuse mod.cut.*\nuse mod.deck.*\nlet t = Title{text: \"a\" span: [0.0, 1.0]}\nlet o = CutOutput{frame: t.out(@frame)}\nDeck{ slides: [ Slide{ name: @a cut: Flow{ kind: @cut output: {width: 640 height: 360 fps: 30} duration: 1.0 t, o } } ] }\n";
    assert!(DeckDoc::load(single).is_ok());
    assert!(apply(single, &DeckOp::DeleteSlide { index: 0 }).unwrap_err().contains("at least one"));
}

#[test]
fn a_slide_is_duplicated_with_its_own_nodes() {
    let (written, doc) = run(TOUR, DeckOp::DuplicateSlide { index: 1, name: None });
    assert_eq!(names(&doc), ["intro", "title", "title_copy", "counter", "source_app", "scope", "closing"]);
    assert!(written.contains("let words2 = Title{") && written.contains("let words_glitch2 = Fx{input: words2.out(@frame)"), "{written}");
    // The original is untouched.
    assert!(written.contains("let words = Title{text: \"A slide is a cut\""));
    // A slide that names a shared `let` edit stays sharing it.
    let (shared, doc) = run(TOUR, DeckOp::DuplicateSlide { index: 0, name: Some("again".into()) });
    assert_eq!(names(&doc), ["intro", "again", "title", "counter", "source_app", "scope", "closing"]);
    assert_eq!(shared.matches("let motion_title").count(), 1);
}

#[test]
fn slide_fields_are_set_and_cleared() {
    let set = SlideSet {
        name: Some("opening".into()),
        play: Some(Loop::Range(1.0, 5.0)),
        enter: Some(Some(Transition { seconds: 0.9, effect: "102_trans_wipe".into(), dials: vec![("p1".into(), 0.4)] })),
        notes: Some("Say \"hello\".".into()),
        target_seconds: Some(Some(45.0)),
        ..SlideSet::default()
    };
    let (written, doc) = run(TOUR, DeckOp::SetSlide { index: 0, set });
    let slide = &doc.slides[0];
    assert_eq!((slide.name.as_str(), slide.looping), ("opening", Loop::Range(1.0, 5.0)));
    assert_eq!(slide.enter.as_ref().and_then(|cut| cut.dial("p1")), Some(0.4));
    assert_eq!(slide.notes, "Say \"hello\".");
    assert_eq!(slide.target_seconds, Some(45.0));
    assert!(written.contains("play: [1.0, 5.0]") && !written.contains("play: [0.0, 8.0]"));
    // Clearing removes the fields, and leaves the other slides alone.
    let clear = SlideSet { play: Some(Loop::Whole), enter: Some(None), notes: Some(String::new()), target_seconds: Some(None), ..SlideSet::default() };
    let (cleared, doc) = run(&written, DeckOp::SetSlide { index: 0, set: clear });
    assert_eq!(doc.slides[0].looping, Loop::Whole);
    assert!(doc.slides[0].enter.is_none() && doc.slides[0].notes.is_empty() && doc.slides[0].target_seconds.is_none());
    assert!(!cleared.contains("opening") || cleared.contains("name: @opening"));
    assert_eq!(doc.slides[1].enter.as_ref().map(|cut| cut.seconds), Some(1.2));
    // A new field is added; a bad value is refused.
    let (hold, doc) = run(TOUR, DeckOp::SetSlide { index: 1, set: SlideSet { play: Some(Loop::Hold), exit: Some(Some(Transition { seconds: 0.3, effect: String::new(), dials: vec![] })), ..SlideSet::default() } });
    assert_eq!(doc.slides[1].looping, Loop::Hold);
    assert_eq!(doc.slides[1].exit.as_ref().map(|cut| cut.seconds), Some(0.3));
    assert!(hold.contains("play: @hold"));
    assert!(apply(TOUR, &DeckOp::SetSlide { index: 0, set: SlideSet { play: Some(Loop::Range(3.0, 2.0)), ..SlideSet::default() } }).is_err());
    assert!(apply(TOUR, &DeckOp::SetSlide { index: 0, set: SlideSet { name: Some("title".into()), ..SlideSet::default() } }).is_err());
    assert!(apply(TOUR, &DeckOp::SetSlide { index: 0, set: SlideSet { play: Some(Loop::Range(0.0, 99.0)), ..SlideSet::default() } }).is_err());
}

fn step(call: &str, args: &[(&str, Lit)]) -> StepSpec {
    StepSpec { call: call.into(), args: args.iter().map(|(name, lit)| (name.to_string(), lit.clone())).collect() }
}

#[test]
fn beats_are_written_validated_and_cleared() {
    let beats = vec![1.0, 3.5, 7.0];
    let (written, doc) = run(TOUR, DeckOp::SetSlide { index: 0, set: SlideSet { beats: Some(beats.clone()), ..SlideSet::default() } });
    assert_eq!(doc.slides[0].beats, beats);
    let (_, cleared) = run(&written, DeckOp::SetSlide { index: 0, set: SlideSet { beats: Some(Vec::new()), ..SlideSet::default() } });
    assert!(cleared.slides[0].beats.is_empty());
    for beats in [vec![2.0, 2.0], vec![4.0, 1.0], vec![-1.0], vec![8.0], vec![9.0], vec![f64::NAN], vec![f64::INFINITY]] {
        assert!(apply(TOUR, &DeckOp::SetSlide { index: 0, set: SlideSet { beats: Some(beats), ..SlideSet::default() } }).is_err());
    }
}

#[test]
fn a_demo_is_written_as_calls_and_checked() {
    let steps = vec![
        step("wait_visible", &[("target", Lit::Id("increment_button".into()))]),
        step("click", &[("target", Lit::Id("increment_button".into())), ("count", Lit::Num(2.0))]),
        step("hold", &[("seconds", Lit::Num(0.5))]),
        step("wait_text", &[("target", Lit::Id("counter_label".into())), ("text", Lit::Str("Count: 2".into()))]),
        step("pause", &[]),
    ];
    let (written, doc) = run(TOUR, DeckOp::SetDemo { index: 2, steps });
    let described = doc.slides[2].demo.iter().map(DemoStep::describe).collect::<Vec<_>>();
    assert_eq!(described, ["wait_visible @increment_button", "click @increment_button", "hold 0.5", "wait_text @counter_label", "pause"]);
    assert!(written.contains("click(@increment_button, {count: 2.0})"), "{written}");
    // On a slide without a demo the field is added; an empty list removes it.
    let (added, doc) = run(TOUR, DeckOp::SetDemo { index: 0, steps: vec![step("mark", &[("name", Lit::Id("start".into()))])] });
    assert_eq!(doc.slides[0].demo.len(), 1);
    let (removed, doc) = run(&added, DeckOp::SetDemo { index: 0, steps: vec![] });
    assert!(doc.slides[0].demo.is_empty());
    assert_eq!(removed, TOUR);
    // Unknown calls, missing arguments and bad options are refused.
    assert!(apply(TOUR, &DeckOp::SetDemo { index: 2, steps: vec![step("tap", &[])] }).is_err());
    assert!(apply(TOUR, &DeckOp::SetDemo { index: 2, steps: vec![step("click", &[])] }).unwrap_err().contains("needs `target`"));
    assert!(apply(TOUR, &DeckOp::SetDemo { index: 2, steps: vec![step("click", &[("target", Lit::Id("a".into())), ("count", Lit::Num(99.0))])] }).is_err());
    assert_eq!(step("scroll", &[("target", Lit::Id("list".into())), ("dy", Lit::Num(-3.0))]).text().unwrap(), "scroll(@list, 0.0, -3.0)");
}

#[test]
fn an_app_is_declared_in_the_slide() {
    let (written, doc) = run(TOUR, DeckOp::AddApp { index: 2, name: "calc".into(), app: "makepad-example-calculator".into(), size: (500.0, 700.0), audio: true });
    let apps = &doc.slides[2].apps;
    assert_eq!(apps.iter().map(|app| app.name.as_str()).collect::<Vec<_>>(), ["calc", "counter"]);
    assert!(apps[0].audio && apps[0].size == (500.0, 700.0));
    assert!(written.contains("calc: App{app: \"makepad-example-calculator\""));
    // A slide without apps gets the field.
    let (_, doc) = run(TOUR, DeckOp::AddApp { index: 5, name: "c".into(), app: "x".into(), size: (10.0, 10.0), audio: false });
    assert_eq!(doc.slides[5].apps.len(), 1);
    assert!(apply(TOUR, &DeckOp::AddApp { index: 2, name: "counter".into(), app: "x".into(), size: (10.0, 10.0), audio: false }).is_err());
}

#[test]
fn the_deck_fields_are_set() {
    let (written, doc) = run(TOUR, DeckOp::SetDeck { title: Some("Deck".into()), transition: Some(Transition { seconds: 0.2, effect: "102_trans_wipe".into(), dials: vec![] }), output: Some((1280, 720)) });
    assert_eq!((doc.title.as_str(), doc.output, doc.transition.seconds), ("Deck", (1280, 720), 0.2));
    assert!(written.contains("title: \"Deck\""));
}

#[test]
fn a_slide_edit_is_extracted_and_put_back() {
    let doc = DeckDoc::load(TOUR).unwrap();
    for index in 0..doc.slides.len() {
        let extracted = extract(TOUR, &doc, index).unwrap();
        // The edit loads on its own, and the file is as it was without a change.
        assert!(makepad_story_cut::CutDoc::load(extracted.text()).is_ok(), "slide {index}: {}", extracted.text());
        assert_eq!(extracted.put_back(extracted.text()).unwrap(), TOUR, "slide {index}");
    }
    // A change of the edit lands in the deck: a node added to the
    // inline edit of `title` (a new let before it, a new member).
    let extracted = extract(TOUR, &doc, 1).unwrap();
    let edited = extracted
        .text()
        .replace("words, words_glitch, words_out", "words, words_glitch, words_out, more")
        .replace("let words_out", "let more = Title{text: \"More\" span: [1.0, 2.0]}\nlet words_out");
    let written = extracted.put_back(&edited).unwrap();
    let next = DeckDoc::load(&written).unwrap_or_else(|diags| panic!("{diags:#?}\n{written}"));
    let SlideCut::Inline(edit) = &next.slides[1].cut else { panic!() };
    assert!(edit.node("more").is_some());
    assert!(written.contains("use mod.deck.*") && written.contains("Deck{"));
    // The slide that is a shared `let` is written back into its `let`.
    let extracted = extract(TOUR, &doc, 0).unwrap();
    let edited = extracted.text().replace("duration: 8.0\n    motion_title", "duration: 9.0\n    motion_title");
    assert_ne!(edited, extracted.text());
    let written = extracted.put_back(&edited).unwrap();
    let next = DeckDoc::load(&written).unwrap();
    assert_eq!(next.slides[0].cut.duration(), Some(9.0));
    assert_eq!(written.matches("duration: 9.0").count(), 1);
}

fn add(source: &str, name: &str, cut: NewCut) -> (String, DeckDoc) {
    run(source, DeckOp::AddSlide { after: None, name: name.into(), cut })
}

#[test]
fn a_new_deck_is_one_title_slide_and_a_dissolve() {
    let source = new_deck_source("Quarterly review").unwrap();
    let doc = DeckDoc::load(&source).unwrap();
    assert_eq!(doc.title, "Quarterly review");
    assert_eq!(names(&doc), ["title"]);
    assert_eq!(doc.transition.effect, "", "the dissolve");
    assert_eq!(doc.transition.seconds, 0.6);
    assert!(source.contains("text: \"Quarterly review\""));
    assert!(new_deck_source("  ").is_err());
    // Quotes and backslashes in a name stay text.
    let odd = new_deck_source("The \"big\" \\ one").unwrap();
    assert_eq!(DeckDoc::load(&odd).unwrap().title, "The \"big\" \\ one");
}

#[test]
fn an_app_slide_uses_a_title_band_and_a_fitted_window() {
    use makepad_story_cut::{CutNodeKind, Fit};
    let source = new_deck_source("Apps").unwrap();
    let (source, _) = add(&source, "last", NewCut::Blank);
    let (written, doc) = run(&source, DeckOp::AddSlide {
        after: Some(0), name: "calculator".into(),
        cut: NewCut::App { app: "calculator".into(), size: AppDecl::DEFAULT_SIZE, demo: vec![] },
    });
    assert_eq!(names(&doc), ["title", "calculator", "last"]);
    assert!(written.contains("app: \"calculator\""), "{written}");
    assert!(!written.contains("AppView{"), "compact app source: {written}");
    let slide = &doc.slides[1];
    assert_eq!(slide.apps[0].app, "calculator");
    assert_eq!(slide.apps[0].size, (900.0, 600.0));
    assert!(!slide.apps[0].audio);
    assert!(slide.demo.is_empty());
    let SlideCut::Inline(cut) = &slide.cut else { panic!("an inline cut") };
    assert!(cut.nodes.iter().any(|node| matches!(&node.kind,
        CutNodeKind::Title { text, anchor, .. } if text == "Calculator" && *anchor == [0.5, 0.08])));
    assert!(cut.nodes.iter().any(|node| matches!(&node.kind,
        CutNodeKind::AppView { app, fit: Fit::Fit, rect: Some(rect) } if app == APP_NAME && *rect == [0.04, 0.18, 0.92, 0.78])));
    let restored = run(&written, DeckOp::DeleteSlide { index: 1 }).0;
    assert_eq!(restored.split_whitespace().collect::<Vec<_>>(), source.split_whitespace().collect::<Vec<_>>());
}

#[test]
fn every_slide_kind_loads_and_says_what_was_asked() {
    let mut source = new_deck_source("Kinds").unwrap();
    let kinds = [
        ("t", NewCut::Title { title: "Hello".into(), subtitle: Some("World".into()) }),
        ("t2", NewCut::Title { title: "No sub".into(), subtitle: None }),
        ("x", NewCut::Text { title: "Plan".into(), bullets: vec!["one".into(), "two \"quoted\"".into(), "three".into()] }),
        ("i", NewCut::Image { id: AST_A.into(), rev: REV.into(), fit: ImageFit::Contain }),
        ("v", NewCut::Video { id: AST_B.into(), rev: REV.into(), play: Some((2.0, 5.5)), seconds: None, audio: false }),
        ("v2", NewCut::Video { id: AST_B.into(), rev: REV.into(), play: None, seconds: Some(12.0), audio: true }),
        ("c", NewCut::Cut { id: AST_B.into(), rev: REV.into(), seconds: Some(12.5) }),
        ("a", NewCut::App { app: "makepad-example-calculator".into(), size: (400.0, 600.0), demo: vec![step("click", &[("target", Lit::Id("button_1".into()))]), step("hold", &[("seconds", Lit::Num(0.5))])] }),
    ];
    for (name, cut) in kinds {
        let (next, doc) = add(&source, name, cut);
        assert_eq!(names(&doc).last(), Some(&name));
        source = next;
    }
    let doc = DeckDoc::load(&source).unwrap();
    assert_eq!(doc.slides.len(), 9);
    // Text bullets appear one by one then hold: 3 bullets, 0.8 s apart.
    let text = &doc.slides[3];
    assert_eq!(text.looping, Loop::Hold);
    assert_eq!(text.cut.duration(), Some(0.8 + 2.4 + 3.0));
    // The video loops its range.
    let video = &doc.slides[5];
    assert_eq!(video.cut.duration(), Some(3.5));
    assert!(source.contains("in: 2.0"));
    assert!(source.contains("audio: false"));
    assert_eq!(doc.slides[6].cut.duration(), Some(12.0));
    assert_eq!(doc.slides[7].cut.duration(), Some(12.5));
    let app = &doc.slides[8];
    assert_eq!(app.apps.len(), 1);
    assert_eq!(app.demo.len(), 2);
    // Media are dependencies of the deck.
    assert!(doc.dependencies().len() >= 3, "{:?}", doc.dependencies());
    // Every kind survives duplicate, move and delete.
    for index in 0..doc.slides.len() {
        let (_, copied) = run(&source, DeckOp::DuplicateSlide { index, name: None });
        assert_eq!(copied.slides.len(), 10);
    }
    let (moved, _) = run(&source, DeckOp::MoveSlide { from: 8, to: 0 });
    let (mut rest, mut rest_doc) = (moved, DeckDoc::load(&source).unwrap());
    while rest_doc.slides.len() > 1 {
        (rest, rest_doc) = run(&rest, DeckOp::DeleteSlide { index: 0 });
    }
    // The last slide cannot be deleted, and the message says why.
    let error = apply(&rest, &DeckOp::DeleteSlide { index: 0 }).unwrap_err();
    assert!(error.contains("at least one slide"), "{error}");
}

#[test]
fn bad_slide_kinds_are_refused_with_a_reason() {
    let source = new_deck_source("Bad").unwrap();
    let refused = |cut: NewCut| apply(&source, &DeckOp::AddSlide { after: None, name: "x".into(), cut }).unwrap_err();
    assert!(refused(NewCut::Title { title: " ".into(), subtitle: None }).contains("title"));
    assert!(refused(NewCut::Text { title: "T".into(), bullets: vec!["a".into(); 13] }).contains("at most"));
    assert!(refused(NewCut::Text { title: "T".into(), bullets: vec!["".into()] }).contains("bullet"));
    assert!(refused(NewCut::Video { id: AST_A.into(), rev: REV.into(), play: Some((5.0, 2.0)), seconds: None, audio: false }).contains("play"));
    assert!(refused(NewCut::App { app: String::new(), size: (1.0, 1.0), demo: vec![] }).contains("registry"));
}

#[test]
fn fifty_slides_and_very_long_notes_load() {
    let mut source = new_deck_source("Big").unwrap();
    for n in 0..49 {
        let cut = if n % 2 == 0 { NewCut::Text { title: format!("S{n}"), bullets: vec!["a".into(), "b".into()] } } else { NewCut::Blank };
        source = add(&source, &format!("s{n}"), cut).0;
    }
    let doc = DeckDoc::load(&source).unwrap();
    assert_eq!(doc.slides.len(), 50);
    let notes = "word ".repeat(20_000);
    let (_, doc) = run(&source, DeckOp::SetSlide { index: 7, set: SlideSet { notes: Some(notes.clone()), ..SlideSet::default() } });
    assert_eq!(doc.slides[7].notes, notes);
}

#[test]
fn a_slide_transition_is_set_from_the_catalogue_and_cleared() {
    let source = new_deck_source("T").unwrap();
    let (source, _) = add(&source, "two", NewCut::Blank);
    let t = Transition { seconds: 1.0, effect: "520_trans_split_slide".into(), dials: vec![] };
    let (source, doc) = run(&source, DeckOp::SetSlide { index: 1, set: SlideSet { enter: Some(Some(t.clone())), ..SlideSet::default() } });
    assert_eq!(doc.slides[1].enter, Some(t));
    let (_, doc) = run(&source, DeckOp::SetSlide { index: 1, set: SlideSet { enter: Some(None), ..SlideSet::default() } });
    assert_eq!(doc.slides[1].enter, None);
}

#[test]
fn a_slide_is_added_before_the_first() {
    let (_, doc) = run(TOUR, DeckOp::AddSlideFirst { name: "zero".into(), cut: NewCut::Title { title: "Cover".into(), subtitle: None } });
    assert_eq!(names(&doc), ["zero", "intro", "title", "counter", "source_app", "scope", "closing"]);
}

#[test]
fn media_is_layered_on_a_slides_picture() {
    let source = new_deck_source("Layers").unwrap();
    let (source, _) = add(&source, "text", NewCut::Text { title: "Plan".into(), bullets: vec!["a".into()] });
    let (source, _) = add(&source, "blank", NewCut::Blank);
    for (index, layer) in [
        (0, NewCut::Image { id: AST_A.into(), rev: REV.into(), fit: ImageFit::Fill }),
        (1, NewCut::Video { id: AST_B.into(), rev: REV.into(), play: Some((1.0, 3.0)), seconds: None, audio: true }),
        (2, NewCut::Cut { id: AST_B.into(), rev: REV.into(), seconds: None }),
    ] {
        let (written, doc) = run(&source, DeckOp::AddLayer { index, layer });
        let SlideCut::Inline(cut) = &doc.slides[index].cut else { panic!() };
        assert!(cut.nodes.len() >= 3);
        assert_eq!(names(&doc), names(&DeckDoc::load(&source).unwrap()));
        // A layer is picture only, whatever the video says.
        assert!(!written.contains("audio: true"), "{written}");
        // And another on top of that.
        let (_, again) = run(&written, DeckOp::AddLayer { index, layer: NewCut::Image { id: AST_A.into(), rev: REV.into(), fit: ImageFit::Contain } });
        let SlideCut::Inline(again) = &again.slides[index].cut else { panic!() };
        assert!(again.nodes.len() > cut.nodes.len());
    }
    // A slide that plays a library cut (written by hand) has no picture to layer on; it says so.
    let library = format!("use mod.cut.*\nuse mod.deck.*\nDeck{{ title: \"L\" slides: [Slide{{ name: @lib cut: cut(\"{AST_B}\", \"{REV}\") }}] }}\n");
    let error = apply(&library, &DeckOp::AddLayer { index: 0, layer: NewCut::Image { id: AST_A.into(), rev: REV.into(), fit: ImageFit::Fill } }).unwrap_err();
    assert!(error.contains("library cut"), "{error}");
    assert!(apply(&source, &DeckOp::AddLayer { index: 0, layer: NewCut::Blank }).is_err());
    assert!(apply(&source, &DeckOp::AddLayer { index: 9, layer: NewCut::Blank }).is_err());
}

#[test]
fn a_layer_goes_on_the_tour_slides_too() {
    for index in 0..6 {
        let (_, doc) = run(TOUR, DeckOp::AddLayer { index, layer: NewCut::Image { id: AST_A.into(), rev: REV.into(), fit: ImageFit::Fill } });
        assert_eq!(doc.slides.len(), 6);
    }
}

#[test]
fn a_camera_slide_and_a_camera_layer_are_written_and_read_back() {
    use makepad_story_cut::{CameraAdjust, CameraDevice, CameraSpec, CutNodeKind, Fit};
    let source = new_deck_source("Cameras").unwrap();
    // A bare camera slide: the defaults are written as the capture size and rate.
    let (bare, doc) = run(&source, DeckOp::AddSlide { after: None, name: "desk".into(), cut: NewCut::Camera(CameraSpec::default()) });
    assert_eq!(names(&doc), ["title", "desk"]);
    assert!(bare.contains("Camera{width: 1280 height: 720 fps: 30.0 span: [0.0, 6.0]}"), "{bare}");
    // A document camera: named, 1080p60, turned, mirrored, a corrected crop.
    let spec = CameraSpec {
        device: CameraDevice::Name("OBSBOT".into()),
        width: 1920,
        height: 1080,
        fps: 60.0,
        crop: [0.1, 0.05, 0.8, 0.9],
        rotate: 180,
        mirror: true,
        adjust: CameraAdjust { brightness: 0.1, contrast: 1.25, saturation: 0.8 },
        fit: Fit::Fill,
        rect: None,
    };
    let (written, doc) = run(&bare, DeckOp::AddSlide { after: None, name: "phone".into(), cut: NewCut::Camera(spec.clone()) });
    let SlideCut::Inline(cut) = &doc.slides[2].cut else { panic!("inline") };
    let camera = cut.nodes.iter().find_map(|node| match &node.kind {
        CutNodeKind::Camera(read) => Some(read.clone()),
        _ => None,
    });
    assert_eq!(camera, Some(spec.clone()), "{written}");
    // Over a slide's picture, as a layer.
    let (layered, doc) = run(&written, DeckOp::AddLayer { index: 0, layer: NewCut::Camera(CameraSpec { device: CameraDevice::Index(1), ..CameraSpec::default() }) });
    let SlideCut::Inline(cut) = &doc.slides[0].cut else { panic!("inline") };
    assert!(cut.nodes.iter().any(|node| matches!(&node.kind, CutNodeKind::Camera(read) if read.device == CameraDevice::Index(1))), "{layered}");
    // Nonsense is refused with a reason.
    let refused = |spec: CameraSpec| apply(&source, &DeckOp::AddSlide { after: None, name: "x".into(), cut: NewCut::Camera(spec) }).unwrap_err();
    assert!(refused(CameraSpec { width: 0, ..CameraSpec::default() }).contains("positive"));
    assert!(refused(CameraSpec { device: CameraDevice::Name(" ".into()), ..CameraSpec::default() }).contains("device"));
    assert!(refused(CameraSpec { rotate: 45, ..CameraSpec::default() }).contains("rotate"));
}

#[test]
fn a_cameras_device_format_and_turn_are_set_in_place() {
    use makepad_story_cut::{CameraDevice, CameraSpec, CutNodeKind};
    let source = new_deck_source("Set").unwrap();
    let (source, _) = run(&source, DeckOp::AddSlide { after: None, name: "desk".into(), cut: NewCut::Camera(CameraSpec::default()) });
    let camera = |doc: &DeckDoc| match &doc.slides[1].cut {
        SlideCut::Inline(cut) => cut.nodes.iter().find_map(|node| match &node.kind {
            CutNodeKind::Camera(spec) => Some(spec.clone()),
            _ => None,
        }),
        _ => None,
    }
    .unwrap();
    let (named, doc) = run(&source, DeckOp::SetCamera { index: 1, set: CameraSet { device: Some(CameraDevice::Name("OBSBOT Meet 2".into())), ..CameraSet::default() } });
    assert_eq!(camera(&doc).device, CameraDevice::Name("OBSBOT Meet 2".into()));
    let (turned, doc) = run(&named, DeckOp::SetCamera { index: 1, set: CameraSet { size: Some((1920, 1080, 60.0)), rotate: Some(90), mirror: Some(true), ..CameraSet::default() } });
    let spec = camera(&doc);
    assert_eq!((spec.width, spec.height, spec.fps, spec.rotate, spec.mirror), (1920, 1080, 60.0, 90, true));
    assert_eq!(spec.device, CameraDevice::Name("OBSBOT Meet 2".into()), "other fields stay");
    // Back to the first camera, and everything else in the file is as it was.
    let (first, doc) = run(&turned, DeckOp::SetCamera { index: 1, set: CameraSet { device: Some(CameraDevice::First), ..CameraSet::default() } });
    assert_eq!(camera(&doc).device, CameraDevice::First);
    assert!(first.contains("device: nil"), "{first}");
    assert!(apply(&source, &DeckOp::SetCamera { index: 0, set: CameraSet::default() }).unwrap_err().contains("no camera"));
    assert!(apply(&source, &DeckOp::SetCamera { index: 1, set: CameraSet { rotate: Some(45), ..CameraSet::default() } }).unwrap_err().contains("rotate"));
}

#[test]
fn compact_app_survives_duplicate_move_set_and_delete() {
    let source = "use mod.deck.*\nDeck{slides: [Slide{name: @scope app: \"scope\" duration: 6.0}]}";
    let (copied, _) = run(source, DeckOp::DuplicateSlide { index: 0, name: Some("second".into()) });
    let (moved, _) = run(&copied, DeckOp::MoveSlide { from: 1, to: 0 });
    let (changed, doc) = run(&moved, DeckOp::SetSlide { index: 0, set: SlideSet { duration: Some(8.0), notes: Some("Changed".into()), ..Default::default() } });
    assert_eq!(doc.slides[0].cut.duration(), Some(8.0));
    assert!(!changed.contains("Flow{"));
    let (deleted, doc) = run(&changed, DeckOp::DeleteSlide { index: 1 });
    assert_eq!(doc.slides.len(), 1);
    assert!(deleted.contains("app: \"scope\""));
}

#[test]
fn talk_length_is_stored_on_the_deck_and_defaults_to_the_slide_targets() {
    let before = DeckDoc::load(TOUR).unwrap();
    let sum: f64 = before.slides.iter().filter_map(|slide| slide.target_seconds).sum();
    assert_eq!(before.talk_seconds, None);
    assert_eq!(before.talk_target(), sum);
    let (written, after) = run(TOUR, DeckOp::SetTalk { seconds: Some(1800.0) });
    assert_eq!(after.talk_seconds, Some(1800.0));
    assert_eq!(after.talk_target(), 1800.0);
    assert!(written.contains("talk: 1800"));
    // The slides keep their own targets.
    assert_eq!(after.slides.iter().map(|slide| slide.target_seconds).collect::<Vec<_>>(), before.slides.iter().map(|slide| slide.target_seconds).collect::<Vec<_>>());
    // Back to the default: the field goes.
    let (cleared, again) = run(&written, DeckOp::SetTalk { seconds: None });
    assert_eq!(again.talk_seconds, None);
    assert!(!cleared.contains("talk:"));
    assert!(apply(TOUR, &DeckOp::SetTalk { seconds: Some(0.0) }).is_err());
}
