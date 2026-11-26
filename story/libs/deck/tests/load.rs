use makepad_story_deck::*;

const TOUR: &str = include_str!("fixtures/custom_deck.splash");

fn memory_tree(files: &[(&str, makepad_asset_data::AssetKind, &[u8])]) -> makepad_story_cut::tree::TreeContext {
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

fn load(source: &str) -> DeckDoc {
    DeckDoc::load(source).unwrap_or_else(|diags| panic!("{diags:#?}"))
}

fn errors(source: &str) -> Vec<Diagnostic> {
    DeckDoc::load(source).err().expect("the deck should not load")
}

#[test]
fn loops_wrap_the_slide_clock() {
    assert_eq!(loop_wrap(&Loop::Whole, 8.0, 3.0), 3.0);
    assert_eq!(loop_wrap(&Loop::Whole, 8.0, 19.0), 3.0);
    assert_eq!(loop_wrap(&Loop::Range(2.0, 6.0), 8.0, 0.0), 2.0);
    assert_eq!(loop_wrap(&Loop::Range(2.0, 6.0), 8.0, 5.0), 3.0);
    assert_eq!(loop_wrap(&Loop::Hold, 3.0, 1.0), 1.0);
    assert_eq!(loop_wrap(&Loop::Hold, 3.0, 99.0), 3.0);
    assert_eq!(loop_wrap(&Loop::Whole, 0.0, 5.0), 0.0);
    assert_eq!(loop_wrap(&Loop::Whole, 8.0, -1.0), 0.0);
}

#[test]
fn bad_slides_are_reported_where_they_are_written() {
    let diags = errors(&TOUR.replace("name: @title", "name: @intro"));
    assert!(diags.iter().any(|d| d.message.contains("two slides are named `intro`")), "{diags:#?}");
    let diags = errors(&TOUR.replace("play: [0.0, 8.0]", "play: [0.0, 9.0]"));
    let loop_line = TOUR.lines().position(|line| line.contains("play: [0.0, 8.0]")).unwrap() as u32 + 1;
    assert!(diags.iter().any(|d| d.line == loop_line && d.message.contains("past the cut")), "{diags:#?}");

    let diags = errors(&TOUR.replace("name: @closing", "colour: 1\n            name: @closing"));
    assert!(diags.iter().any(|d| d.message.contains("`colour` is not a Slide field")), "{diags:#?}");

    let diags = errors(&TOUR.replace("effect: \"366_slice_glitch\"", "effect: 7"));
    let glitch_line = TOUR.lines().position(|line| line.contains("let words_glitch")).unwrap() as u32 + 1;
    assert!(diags.iter().any(|d| d.slide.as_deref() == Some("title") && d.line == glitch_line), "{diags:#?}");

    let diags = errors("use mod.deck.*\nDeck{ slides: [] }");
    assert!(diags.iter().any(|d| d.message.contains("at least one slide")), "{diags:#?}");
    assert!(!errors("use mod.deck.*\nSlide{}").is_empty());
    assert!(!errors("use mod.deck.*\nDeck{ slides: [Slide{ name: @a cut: 3 }] }").is_empty());
}

#[test]
fn demo_steps_are_checked_like_app_run_calls() {
    let diags = errors(&TOUR.replace("click(@increment_button),\n                hold(0.5),\n                click(@increment_button),", "click(@increment_button, {count: 9}),\n                hold(0.5),\n                click(@increment_button),"));
    assert!(diags.iter().any(|d| d.slide.as_deref() == Some("counter") && d.message.contains("from 1 to 3")), "{diags:#?}");
    let diags = errors(&TOUR.replace("wait_text(@counter_label, \"Count: 3\")", "wait_text(@counter_label)"));
    assert!(diags.iter().any(|d| d.message.contains("demo[")), "{diags:#?}");
}

#[test]
fn an_app_view_names_an_app_of_its_slide() {
    let diags = errors(&TOUR.replace("AppView{app: @counter", "AppView{app: @missing"));
    assert!(diags.iter().any(|d| d.slide.as_deref() == Some("counter") && d.message.contains("`missing`") && d.message.contains("counter")), "{diags:#?}");
}

#[test]
fn an_app_only_needs_its_registry_id() {
    let source = r#"use mod.flow.*
use mod.cut.*
use mod.deck.*
let view = AppView{app: @app fit: @fit span: [0.0, 6.0]}
let out = CutOutput{frame: view.out(@frame)}
Deck{slides: [Slide{
    name: @calculator
    cut: Flow{kind: @cut output: {width: 1920 height: 1080 fps: 30} duration: 6.0 view, out}
    apps: {app: App{app: "calculator"}}
}]}
"#;
    let expected = AppDecl {
        name: "app".into(), app: "calculator".into(), size: (900.0, 600.0), audio: false,
        args: vec![], env: vec![], dpi: None, resolution: None, next: false, setup: vec![], demo: vec![],
    };
    assert_eq!(load(source).slides[0].apps, [expected.clone()]);
    let with = |fields: &str| source.replace("App{app: \"calculator\"}", &format!("App{{app: \"calculator\" {fields}}}"));
    assert_eq!(load(&with("size: nil audio: nil args: nil env: nil")).slides[0].apps, [expected.clone()]);
    assert_eq!(load(&with("size: [480, 320] audio: true args: [\"--demo\"] env: {MODE: \"demo\"}")).slides[0].apps, [AppDecl {
        size: (480.0, 320.0), audio: true, args: vec!["--demo".into()], env: vec![("MODE".into(), "demo".into())], ..expected.clone()
    }]);
    assert_eq!(load(&with("dpi: 1.5")).slides[0].apps[0].dpi, Some(1.5));
    assert!(load(&with("next: true")).slides[0].apps[0].next);
    assert_eq!(load(&with("resolution: [3840, 2160]")).slides[0].apps[0].resolution, Some((3840, 2160)));
    assert_eq!(load(&with("dpi: nil resolution: nil")).slides[0].apps, [expected.clone()]);
    for (fields, field) in [("size: [0, 600]", "size"), ("size: [900]", "size"), ("audio: 1", "audio"), ("args: [1]", "args"), ("env: {MODE: 1}", "env"),
        ("dpi: 0", "dpi"), ("dpi: 20", "dpi"), ("dpi: \"2\"", "dpi"), ("resolution: [1920]", "resolution"), ("resolution: [0, 1080]", "resolution"),
        ("resolution: [1920.5, 1080]", "resolution"), ("resolution: 1080", "resolution"), ("next: 1", "next")] {
        let diags = errors(&with(fields));
        assert!(diags.iter().any(|d| d.message.contains(&format!("apps.app.{field}"))), "{fields}: {diags:#?}");
    }
    let diags = errors(&with("dpi: 2 resolution: [1920, 1080]"));
    assert!(diags.iter().any(|d| d.message.contains("not both")), "{diags:#?}");
}

/// A hosted app renders at least at the pixels its frame covers on the
/// output, whatever the editor's dpi, a fractional frame or Present's size.
#[test]
fn a_hosted_app_renders_at_least_at_its_frames_output_pixels() {
    let decl = AppDecl { name: "a".into(), app: "calculator".into(), size: (900.0, 600.0), audio: false, args: vec![], env: vec![], dpi: None, resolution: None, next: false, setup: vec![], demo: vec![] };
    let frac = [0.61875, 0.7];
    let covers = |dpi: f64, px: (u32, u32)| (decl.size.0 * dpi).ceil() as u32 >= px.0 && (decl.size.1 * dpi).ceil() as u32 >= px.1;
    for picture in [(1920.0, 1080.0), (1531.0, 861.0), (3062.0, 1722.0), (3840.0, 2160.0), (1599.5, 899.7), (640.0, 360.0)] {
        let px = frame_pixels(frac, picture);
        assert!(px.0 as f64 >= frac[0] * picture.0 && px.1 as f64 >= frac[1] * picture.1, "{picture:?} {px:?}");
        let dpi = decl.render_dpi(Some(px));
        assert!(covers(dpi, px), "{picture:?}: dpi {dpi} for {px:?}");
        assert!(dpi >= 1.0 && (dpi / DPI_STEP).fract() == 0.0, "{dpi}");
    }
    // The editor at dpi 2 needs twice the pixels of dpi 1.
    let one = decl.render_dpi(Some(frame_pixels(frac, (1531.0, 861.0))));
    let two = decl.render_dpi(Some(frame_pixels(frac, (3062.0, 1722.0))));
    assert!(two >= 2.0 * one - DPI_STEP, "{one} {two}");
    assert_eq!(frame_pixels([0.5, 0.5], (100.0, 100.0)), (50, 50), "exact sizes do not grow");
    assert_eq!(decl.render_dpi(None), 1.0);
    assert_eq!(AppDecl { dpi: Some(1.5), ..decl.clone() }.render_dpi(Some((4000, 4000))), 1.5);
    assert_eq!(AppDecl { resolution: Some((1800, 1200)), ..decl.clone() }.render_dpi(Some((100, 100))), 2.0);
}

#[test]
fn library_edits_are_references() {
    let source = "use mod.cut.*\nuse mod.deck.*\nDeck{ slides: [Slide{ name: @a cut: cut(\"ast_x\", \"arev_y\") }] }";
    let doc = load(source);
    assert!(matches!(&doc.slides[0].cut, SlideCut::Library { id, rev } if id == "ast_x" && rev == "arev_y"));
}

#[test]
fn a_deck_steps_through_the_slides_with_the_documents_changes() {
    let mut doc = load(TOUR);
    // Keep exercising uninterrupted slide clocks as well as beat clocks.
    doc.slides[0].beats.clear();
    let mut deck = Deck::new(doc.slides.len(), 100.0);
    assert_eq!(deck.keep(), [0, 1]);
    // A slide without beats has its start and its end as stops: the first press
    // sits it at the end, the next one goes on.
    deck.next(&doc, 101.0);
    assert_eq!(deck.current(), 0);
    assert!(deck.incoming().is_none());
    deck.next(&doc, 101.0 + RUSH_SECONDS);
    assert_eq!(deck.current(), 1);
    let incoming = deck.incoming().unwrap();
    // The document asks 1.2 s; a key press's change is at most MAX_CHANGE_SECONDS.
    assert_eq!((incoming.from, incoming.transition.seconds, incoming.transition.effect.as_str()), (0, MAX_CHANGE_SECONDS, "520_trans_split_slide"));
    assert_eq!(deck.keep(), [0, 1, 2]);
    // Two slides on, the slide before the one left is torn down: only the
    // shown slide, the one being left and the next remain.
    let mut later = deck.clone();
    later.go(&doc, 2, 200.0);
    assert!(!later.keep().contains(&0), "a previous slide is not kept: {:?}", later.keep());
    assert!(later.keep().contains(&1) && later.keep().contains(&2) && later.keep().contains(&3.min(doc.slides.len() - 1)));
    // The slide left sits at its end (the press finished its cut, which loops over
    // [0, 8)); the one entered starts at its start.
    assert!((deck.slide_time(&doc, 0, 108.5) - 8.0).abs() < 1e-9, "the slide left sits at its end");
    assert!((deck.slide_time(&doc, 1, 101.75) - 0.5).abs() < 1e-9);
    assert_eq!(deck.progress(101.0 + RUSH_SECONDS), Some(0.0));
    let half = deck.progress(101.0 + RUSH_SECONDS + MAX_CHANGE_SECONDS / 2.0).unwrap();
    assert!((half - 0.5).abs() < 1e-9, "smoothstep is 0.5 at the middle");
    // The change never waits for the entered slide's picture.
    deck.tick(101.1);
    assert!(deck.incoming().is_some());
    deck.tick(101.01 + RUSH_SECONDS + MAX_CHANGE_SECONDS);
    assert!(deck.incoming().is_none());
    // Presses in quick succession (a held key) cut without a transition.
    let mut held = deck.clone();
    held.next(&doc, 102.0); // sits slide 1 at its end
    held.next(&doc, 102.01 + RUSH_SECONDS); // on to slide 2, through the change
    assert!(held.incoming().is_some());
    held.next(&doc, 102.03 + RUSH_SECONDS);
    assert!(held.incoming().is_none(), "the next press of a held key ends the change");
    held.next(&doc, 102.04 + 2.0 * RUSH_SECONDS);
    held.next(&doc, 102.05 + 2.0 * RUSH_SECONDS);
    assert_eq!(held.current(), 3);
    assert!(held.incoming().is_none(), "the second slide change of a held key cuts");
    // Going to the slide that shows does nothing; an instant change has no change.
    deck.go(&doc, 1, 104.0);
    assert!(deck.incoming().is_none());
    deck.last(&doc, 105.0);
    assert_eq!(deck.current(), 5);
    deck.next(&doc, 105.0);
    assert_eq!(deck.current(), 5, "the last slide is the last");
    // Hold keeps the last frame of the closing slide.
    assert_eq!(deck.slide_time(&doc, 5, 500.0), 3.0);
    deck.previous(&doc, 506.0);
    assert_eq!((deck.current(), deck.slide_time(&doc, 5, 506.0)), (5, 0.0), "Previous from a slide's end goes to its start first");
    deck.previous(&doc, 506.0);
    assert_eq!(deck.current(), 4);
    deck.toggle_black();
    assert!(deck.black());
    deck.restart(507.0);
    assert_eq!(deck.slide_time(&doc, 4, 507.5), 0.5);
}

#[test]
fn a_deck_names_the_library_cuts_it_plays_as_dependencies() {
    use makepad_story_cut::{AssetRef, Dep};
    // The tour is made of inline cuts: nothing in the library.
    assert!(load(TOUR).dependencies().is_empty());
    let source = "use mod.cut.*\nuse mod.deck.*\nDeck{ slides: [Slide{ name: @a cut: cut(\"ast_x\", \"arev_y\") } Slide{ name: @b cut: cut(\"ast_x\", \"arev_y\") }] }";
    assert_eq!(load(source).dependencies(), [Dep::Cut(AssetRef { id: "ast_x".into(), rev: "arev_y".into() })]);
}

fn beat_doc(looping: Loop) -> DeckDoc {
    let mut doc = load(TOUR);
    doc.slides[0].beats = vec![2.0, 4.5, 7.0];
    doc.slides[0].looping = looping;
    doc
}

#[test]
fn a_story_holds_until_each_beat_is_released() {
    let doc = beat_doc(Loop::Whole);
    let mut deck = Deck::new(doc.slides.len(), 100.0);
    assert_eq!(deck.slide_time(&doc, 0, 101.0), 1.0);
    assert!(!deck.holding(&doc, 101.0));
    assert_eq!(deck.slide_time(&doc, 0, 102.0), 2.0);
    assert_eq!(deck.slide_time(&doc, 0, 1000.0), 2.0);
    assert!(deck.holding(&doc, 1000.0));
    assert_eq!(deck.live_time(0, 1000.0), 900.0);
    assert_eq!(deck.beat(), 0);
    assert_eq!(deck.next_preview(&doc, 1000.0), Some((0, 4.5)));

    deck.next(&doc, 1000.0);
    assert_eq!((deck.current(), deck.beat()), (0, 1));
    assert_eq!(deck.slide_time(&doc, 0, 1000.0), 2.0);
    assert_eq!(deck.slide_time(&doc, 0, 1001.0), 3.0);
    assert!(!deck.holding(&doc, 1001.0));
    assert_eq!(deck.slide_time(&doc, 0, 1100.0), 4.5);
    assert_eq!(deck.next_preview(&doc, 1100.0), Some((0, 7.0)));

    deck.next(&doc, 1100.0);
    assert_eq!(deck.slide_time(&doc, 0, 1101.0), 5.5);
    assert_eq!(deck.slide_time(&doc, 0, 1200.0), 7.0);
    assert_eq!(deck.next_preview(&doc, 1200.0), Some((0, 8.0)));
}

#[test]
fn after_the_last_beat_the_cut_plays_out_then_uses_play() {
    // The release of the last beat plays on by itself: a looping slide loops on
    // its own, with no key pressed; a holding one rests at its end.
    for (looping, after_end) in [(Loop::Whole, 0.5), (Loop::Range(1.0, 5.0), 1.5), (Loop::Hold, 8.0)] {
        let doc = beat_doc(looping);
        let mut deck = Deck::new(doc.slides.len(), 0.0);
        // Even a range with a nonzero start enters the story at zero.
        assert_eq!(deck.slide_time(&doc, 0, 0.0), 0.0);
        deck.next(&doc, 10.0);
        deck.next(&doc, 20.0);
        deck.next(&doc, 30.0);
        assert_eq!((deck.current(), deck.beat()), (0, 3));
        assert_eq!(deck.slide_time(&doc, 0, 30.5), 7.5);
        assert_eq!(deck.slide_time(&doc, 0, 31.5), after_end);
        assert_eq!(deck.live_time(0, 31.5), 31.5);
        assert_eq!(deck.holding(&doc, 31.5), looping == Loop::Hold);
        assert!(deck.slide_time(&doc, 0, 31.5 + 800.0) < 8.0 || looping == Loop::Hold, "still looping long after");
        assert_eq!(deck.next_preview(&doc, 30.5), Some((0, 8.0)), "the end of the cut is the next stop");
        // Pressed in the play-out, the slide sits at its end; the press after it goes on.
        let mut early = deck.clone();
        early.next(&doc, 30.5);
        assert_eq!((early.current(), early.beat()), (0, 3), "the press sits the slide at its end");
        assert_eq!(early.slide_time(&doc, 0, 30.5 + RUSH_SECONDS), 8.0);
        assert_eq!(early.next_preview(&doc, 30.5 + RUSH_SECONDS), Some((1, 0.0)));
        early.next(&doc, 30.6 + RUSH_SECONDS);
        assert_eq!((early.current(), early.beat()), (1, 0));
        // A holding slide played out to its end is at its end stop: the press goes on.
        // A looping one is mid-loop: the press sits it at the end, the next goes on.
        deck.next(&doc, 32.0);
        if looping == Loop::Hold {
            assert_eq!((deck.current(), deck.beat()), (1, 0));
        } else {
            assert_eq!((deck.current(), deck.slide_time(&doc, 0, 33.0)), (0, 8.0));
            deck.next(&doc, 33.0);
            assert_eq!(deck.current(), 1);
        }
    }
}

#[test]
fn an_app_slide_without_play_keeps_looping_its_cut() {
    let mut doc = load(TOUR);
    let at = doc.slides.iter().position(|slide| !slide.apps.is_empty() && slide.beats.is_empty() && slide.cut.duration().is_some()).expect("an app slide with an inline cut");
    let duration = doc.slides[at].cut.duration().unwrap();
    doc.slides[at].looping = Loop::Whole;
    let mut deck = Deck::new(doc.slides.len(), 0.0);
    deck.go(&doc, at, 0.0);
    deck.tick(10_000.0);
    // The cut is a backdrop that moves: it plays again after its end, never holds.
    assert!(!deck.holding(&doc, duration * 0.5));
    assert!(!deck.holding(&doc, duration + 0.5), "the cut wraps and the picture keeps moving behind the apps");
    assert!((deck.slide_time(&doc, at, duration * 3.5) - duration * 0.5).abs() < 1e-9, "the clock wraps");
    // `play: @hold` still holds it.
    doc.slides[at].looping = Loop::Hold;
    assert!(deck.holding(&doc, duration + 0.5));
}

#[test]
fn previous_jumps_to_a_beat_hold_and_zero_goes_to_the_previous_slide() {
    let mut doc = beat_doc(Loop::Hold);
    doc.slides[1].beats = vec![1.0, 3.0, 5.0];
    let mut deck = Deck::new(doc.slides.len(), 0.0);
    deck.go(&doc, 1, 10.0);
    deck.tick(12.0);
    deck.next(&doc, 20.0);
    deck.next(&doc, 30.0);
    deck.previous(&doc, 30.5); // In flight toward beat 2.
    assert_eq!((deck.current(), deck.beat()), (1, 1));
    assert_eq!(deck.slide_time(&doc, 1, 30.5), 3.0);
    assert_eq!(deck.slide_time(&doc, 1, 100.0), 3.0);
    assert!(deck.holding(&doc, 100.0));
    deck.previous(&doc, 101.0);
    assert_eq!(deck.slide_time(&doc, 1, 101.0), 1.0);
    assert_eq!(deck.beat(), 0);
    assert_eq!(deck.entered_at(), 10.0);
    deck.previous(&doc, 102.0); // At the first beat: the start of the slide.
    assert_eq!((deck.current(), deck.beat(), deck.slide_time(&doc, 1, 102.0)), (1, 0, 0.0));
    deck.previous(&doc, 102.0); // At the start: the previous slide.
    assert_eq!(deck.current(), 0);
    assert_eq!(deck.slide_time(&doc, 0, 102.0), doc.slides[0].cut.duration().unwrap(), "the slide before is entered at its end");
    assert_eq!(deck.live_time(0, 102.0), 0.0);
}

#[test]
fn previous_from_play_out_returns_to_the_last_hold_and_restart_resets_both_clocks() {
    let doc = beat_doc(Loop::Whole);
    let mut deck = Deck::new(doc.slides.len(), 0.0);
    for now in [10.0, 20.0, 30.0] { deck.next(&doc, now); }
    deck.previous(&doc, 40.0);
    assert_eq!(deck.beat(), 2);
    assert_eq!(deck.slide_time(&doc, 0, 40.0), 7.0);
    assert!(deck.holding(&doc, 50.0));
    deck.restart(60.0);
    assert_eq!((deck.current(), deck.beat()), (0, 0));
    assert_eq!(deck.entered_at(), 60.0);
    assert_eq!(deck.slide_time(&doc, 0, 60.5), 0.5);
    assert_eq!(deck.live_time(0, 60.5), 0.5);
    assert_eq!(deck.slide_time(&doc, 0, 90.0), 2.0);
}

#[test]
fn transitions_keep_the_outgoing_beat_and_never_wait_for_the_picture() {
    let mut doc = beat_doc(Loop::Whole);
    doc.slides[1].beats = vec![1.0, 3.0];
    let mut deck = Deck::new(doc.slides.len(), 100.0);
    deck.next(&doc, 110.0);
    deck.go(&doc, 1, 120.0);
    assert_eq!(deck.keep(), [0, 1, 2]);
    assert_eq!(deck.slide_time(&doc, 0, 121.0), 4.5);
    assert_eq!(deck.live_time(0, 121.0), 21.0);
    // The entered slide's clocks run from the press, picture or not.
    assert_eq!(deck.slide_time(&doc, 1, 120.5), 0.5);
    assert_eq!(deck.live_time(1, 122.0), 2.0);
    assert_eq!(deck.entered_at(), 120.0);
    deck.tick(120.1);
    assert!(deck.incoming().is_some());
    deck.tick(120.01 + MAX_CHANGE_SECONDS);
    assert!(deck.incoming().is_none());
    assert_eq!(deck.slide_time(&doc, 1, 123.0), 1.0);
    deck.go(&doc, 0, 130.0);
    assert_eq!((deck.current(), deck.beat()), (0, 0));
    assert_eq!(deck.slide_time(&doc, 0, 130.0), 0.0);
}

#[test]
fn reloading_a_slide_keeps_its_beat_and_live_clock() {
    let doc = beat_doc(Loop::Hold);
    let mut previous = Deck::new(doc.slides.len(), 0.0);
    previous.next(&doc, 10.0);
    let mut deck = Deck::new(doc.slides.len(), 20.0);
    deck.resume(0, &previous);
    assert_eq!(deck.beat(), 1);
    assert_eq!(deck.slide_time(&doc, 0, 20.0), 4.5);
    assert_eq!(deck.live_time(0, 20.0), 20.0);
}

#[test]
fn a_comment_line_before_the_deck_is_ignored() {
    let marked = format!("// A deck with an introductory comment.\n{TOUR}");
    assert_eq!(load(&marked).slides.len(), load(TOUR).slides.len());
}

#[test]
fn code_layers_are_checked_at_load() {
    let doc = load(TOUR);
    let slide = doc.slides.iter().find(|slide| slide.name == "source_app").unwrap();
    let SlideCut::Inline(cut) = &slide.cut else { panic!("an inline cut") };
    let code = cut.nodes.iter().find_map(|node| match &node.kind {
        makepad_story_cut::CutNodeKind::Code(spec) => Some(spec.clone()),
        _ => None,
    });
    let code = code.expect("a Code node");
    assert_eq!((code.app.as_str(), code.file.as_str(), code.lines, code.size), ("counter", "examples/counter/src/main.rs", Some((24, 56)), (1000, 800)));
    assert_eq!(code.rect, Some([0.03, 0.16, 0.5, 0.8]));
    let diags = errors(&TOUR.replace("Code{app: @counter", "Code{app: @nope"));
    assert!(diags.iter().any(|d| d.message.contains("names `nope`")), "{diags:#?}");
    let diags = errors(&TOUR.replace("lines: [24, 56]", "lines: [56, 24]"));
    assert!(diags.iter().any(|d| d.message.contains("`live_code.lines`")), "{diags:#?}");
    let diags = errors(&TOUR.replace("file: \"examples/counter/src/main.rs\"", "file: \"../outside.rs\""));
    assert!(diags.iter().any(|d| d.message.contains("`live_code.file`")), "{diags:#?}");
}

#[test]
fn setup_steps_are_parsed_like_demo_steps_and_checked_at_load() {
    let with = |setup: &str| TOUR.replacen("            demo: [\n                wait_visible(@increment_button),", &format!("            setup: [{setup}]\n            demo: [\n                wait_visible(@increment_button),"), 1);
    let doc = load(&with("scroll(@increment_button, 0, -250), wait_settled({max: 10})"));
    let counter = doc.slides.iter().find(|slide| slide.name == "counter").unwrap();
    let steps = counter.setup.iter().map(DemoStep::describe).collect::<Vec<_>>();
    assert_eq!(steps, ["scroll @increment_button", "wait_settled"]);
    assert!(matches!(&counter.setup[1].step, Step::Call(makepad_story_app_run::call::Call::WaitSettled { .. })));
    assert_eq!(counter.demo.len(), 8, "the demo is its own list");
    assert!(load(TOUR).slides.iter().all(|slide| slide.setup.is_empty()));

    let diags = errors(&with("wait_text(@counter_label)"));
    assert!(diags.iter().any(|d| d.slide.as_deref() == Some("counter") && d.message.contains("setup[")), "{diags:#?}");
    let diags = errors(&with("pause()"));
    assert!(diags.iter().any(|d| d.message.contains("cannot `pause()`")), "{diags:#?}");
    let diags = errors(&TOUR.replacen("            name: @title", "            setup: [hold(1.0)]\n            name: @title", 1));
    assert!(diags.iter().any(|d| d.message.contains("slide has no `apps`")), "{diags:#?}");
}

#[test]
fn early_next_always_lands_at_the_next_beat() {
    let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
    let doc = beat_doc(Loop::Hold); // beats 2.0, 4.5, 7.0
    let mut deck = Deck::new(doc.slides.len(), 100.0);
    // 0.5 s into the intro (25 % of the segment toward 2.0): the story runs on
    // faster and arrives at the hold in RUSH_SECONDS, continuously.
    deck.next(&doc, 100.5);
    assert_eq!((deck.current(), deck.beat()), (0, 0));
    assert!(close(deck.slide_time(&doc, 0, 100.5), 0.5), "no cut at the press");
    let mid = deck.slide_time(&doc, 0, 100.5 + RUSH_SECONDS / 2.0);
    assert!(mid > 0.5 && mid < 2.0, "half way through the rush: {mid}");
    assert!(close(deck.slide_time(&doc, 0, 100.5 + RUSH_SECONDS), 2.0));
    assert!(deck.holding(&doc, 101.0));
    // At the hold Next releases the beat; the press 80 % into the next segment
    // still lands at the beat (4.5), it does not skip past it.
    deck.next(&doc, 101.0);
    assert_eq!(deck.beat(), 1);
    let late = 101.0 + (4.5 - 2.0) * 0.8;
    assert!(close(deck.slide_time(&doc, 0, late), 2.0 + 2.5 * 0.8));
    deck.next(&doc, late);
    assert_eq!(deck.beat(), 1, "the beat is not released by the press that lands at it");
    assert!(close(deck.slide_time(&doc, 0, late), 2.0 + 2.5 * 0.8), "continuous at the press");
    assert!(close(deck.slide_time(&doc, 0, late + RUSH_SECONDS), 4.5));
    assert!(close(deck.slide_time(&doc, 0, late + 100.0), 4.5), "and stays there");
    assert!(deck.holding(&doc, late + 100.0));
    // The next press leaves it.
    deck.next(&doc, late + 100.0);
    assert_eq!(deck.beat(), 2);
    // A press while being carried does not stretch the arrival nor skip.
    let mut held = Deck::new(doc.slides.len(), 0.0);
    held.next(&doc, 0.2);
    held.next(&doc, 0.25);
    assert_eq!(held.beat(), 0);
    assert!(close(held.slide_time(&doc, 0, 0.2 + RUSH_SECONDS), 2.0));
    // A slide entered by a key: Next during the change ends it and carries the
    // entered slide's story to its first hold.
    let mut doc2 = doc.clone();
    doc2.slides[1].beats = vec![1.0];
    let mut deck2 = Deck::new(doc2.slides.len(), 0.0);
    deck2.go(&doc2, 1, 10.0);
    assert!(deck2.incoming().is_some());
    deck2.next(&doc2, 10.05);
    assert!(deck2.incoming().is_none());
    assert!(close(deck2.slide_time(&doc2, 1, 10.05 + RUSH_SECONDS), 1.0));
}

#[test]
fn tree_paths_and_forbidden_library_refs_report_the_referring_file() {
    use makepad_asset_data::AssetKind::{Deck, Motion};
    let source = r#"use mod.deck.*
Deck{slides: [Slide{name: @rain graphic: "slides/rain"}]}"#;
    let plate = b"use mod.motion.*\nMotion{duration: 1.0 layers: [Rect{size: vec2(8, 8) fill: #446677}]}";
    let tree = |deck: &str| memory_tree(&[
        ("deck.splash", Deck, deck.as_bytes()),
        ("slides/rain.splash", Motion, plate),
    ]);
    DeckDoc::load_tree(source, tree(source)).unwrap();
    let missing = source.replace("slides/rain", "slides/missing_slide");
    let errors = DeckDoc::load_tree(&missing, tree(&missing)).unwrap_err();
    assert!(errors.iter().any(|d| d.message.contains("deck.splash") && d.message.contains("missing_slide") && d.line == 2), "{errors:#?}");
    let forbidden = format!("use mod.cut.*\nlet forbidden = asset(\"id\", \"revision\")\n{source}");
    let errors = DeckDoc::load_tree(&forbidden, tree(&forbidden)).unwrap_err();
    assert!(errors.iter().any(|d| d.message.contains("forbidden") && d.line == 2), "{errors:#?}");
}

#[test]
fn a_slide_can_play_a_motion_plate_directly_from_its_tree() {
    let source = r#"use mod.cut.*
use mod.deck.*
Deck{output: {width: 1920 height: 1080} slides: [Slide{name: @opening cut: graphic("slides/opening")}]}
"#;
    let graphic = b"use mod.motion.*\nlet look = import(\"../shared/look\")\nMotion{duration: look.seconds layers: [Image{src: look.hero() size: vec2(10, 10)}]}";
    let tree = memory_tree(&[
        ("deck.splash", makepad_asset_data::AssetKind::Deck, source.as_bytes()),
        ("slides/opening.splash", makepad_asset_data::AssetKind::Motion, graphic),
        ("shared/look.splash", makepad_asset_data::AssetKind::Motion, b"use mod.motion.*\nlet timing = import(\"./timing\")\n{seconds: timing.seconds hero: fn(){graphic(\"../media/hero\")}}"),
        ("shared/timing.splash", makepad_asset_data::AssetKind::Motion, b"{seconds: 4.0}"),
        // Loading resolves the texture revision; decoding belongs to rendering.
        ("media/hero.png", makepad_asset_data::AssetKind::Texture, b"texture bytes"),
    ]);
    let deck = DeckDoc::load_tree(source, tree).unwrap_or_else(|diags| panic!("{diags:#?}"));
    assert_eq!(deck.slides[0].cut.duration(), Some(4.0));
    assert!(deck.slides[0].cut_span.is_none());
    let SlideCut::Inline(cut) = &deck.slides[0].cut else { panic!() };
    let makepad_story_cut::GraphicSrc::Tree(tree) = &cut.graphics[0].src else { panic!("the graphic resolves through the deck tree") };
    assert_eq!(tree.document, "slides/opening.splash");
}

#[test]
fn inline_stills_and_external_cuts_share_the_decks_tree() {
    use makepad_asset_data::AssetKind::{Cut, Deck, Texture};
    use makepad_story_cut::{CutNodeKind, MediaRef};
    let source = r#"use mod.flow.*
use mod.cut.*
use mod.deck.*
let picture = Still{src: graphic("media/hero")}
let out = CutOutput{frame: picture.out(@frame)}
Deck{slides: [
    Slide{name: @inline cut: Flow{kind: @cut output: {width: 1920 height: 1080 fps: 30} duration: 2.0 picture, out}}
    Slide{name: @external cut: graphic("slides/picture")}
]}
"#;
    let cut = b"use mod.flow.*\nuse mod.cut.*\nlet picture = Still{src: \"../media/hero.png\"}\nlet out = CutOutput{frame: picture.out(@frame)}\nFlow{kind: @cut output: {width: 1920 height: 1080 fps: 30} duration: 2.0 picture, out}";
    let tree = memory_tree(&[
        ("deck.splash", Deck, source.as_bytes()),
        ("slides/picture.cut.splash", Cut, cut),
        ("media/hero.png", Texture, b"texture bytes"),
    ]);
    let expected = tree.resolve("media/hero").unwrap().reference.unwrap();
    let deck = DeckDoc::load_tree(source, tree).unwrap_or_else(|diags| panic!("{diags:#?}"));
    for slide in &deck.slides {
        let SlideCut::Inline(cut) = &slide.cut else { panic!() };
        let CutNodeKind::Still { src: MediaRef::Asset(asset), .. } = &cut.node("picture").unwrap().kind else { panic!() };
        assert_eq!((&asset.id, &asset.rev), (&expected.asset_id.to_string(), &expected.revision.to_string()));
    }
}

#[test]
fn a_tree_module_cannot_hide_a_library_reference() {
    use makepad_asset_data::AssetKind::{Deck, Motion};
    let source = "use mod.deck.*\nDeck{slides: [Slide{name: @opening cut: graphic(\"slides/opening\")}]}";
    let tree = memory_tree(&[
        ("deck.splash", Deck, source.as_bytes()),
        ("slides/opening.splash", Motion, b"use mod.motion.*\nlet look = import(\"shared/look\")\nMotion{duration: 4.0 layers: []}"),
        ("shared/look.splash", Motion, b"use mod.motion.*\n{image: asset(\"id\", \"revision\")}"),
    ]);
    let errors = DeckDoc::load_tree(source, tree).unwrap_err();
    assert!(errors.iter().any(|d| d.message.contains("shared/look.splash:2:") && d.message.contains("asset()")), "{errors:#?}");
}

#[test]
fn compact_graphic_inherits_output_and_explicit_duration() {
    use makepad_asset_data::AssetKind::{Deck, Motion};
    let source = r#"use mod.deck.*
Deck{output: {width: 1280 height: 720 fps: 24}
slides: [Slide{name: @title graphic: "slides/title" duration: 6.0 beats: [1.0, 4.0] play: @hold}]}"#;
    let tree = memory_tree(&[
        ("deck.splash", Deck, source.as_bytes()),
        ("slides/title.splash", Motion, b"use mod.motion.*\nMotion{size: vec2(1920,1080) duration: 4.0 layers: [Text{text: \"Title\"}]}"),
    ]);
    let doc = DeckDoc::load_tree(source, tree).unwrap();
    assert_eq!(doc.slides[0].graphic.as_deref(), Some("slides/title.splash"));
    let SlideCut::Inline(cut) = &doc.slides[0].cut else { panic!("lowered graphic") };
    assert_eq!((cut.output.width, cut.output.height, cut.output.fps, cut.duration), (1280,720,24.0,6.0));
    assert_eq!(cut.graphics.len(), 1);
    assert_eq!(cut.nodes.len(), 1);
    assert_eq!(cut.graphics[0].fit, makepad_story_cut::Fit::Fill);
}

#[test]
fn compact_slide_rejects_ambiguous_content_and_old_reference_call() {
    for fields in [
        "app: \"scope\" graphic: \"slides/title\"",
        "app: \"scope\" duration: -1.0",
        "cut: plate(\"slides/title\")",
    ] {
        assert!(DeckDoc::load(&format!("use mod.deck.*\nDeck{{slides: [Slide{{name: @bad {fields}}}]}}")).is_err());
    }
}

#[test]
fn first_slide_load_leaves_later_graphics_pending() {
    use makepad_asset_data::AssetKind::{Deck, Motion};
    let source = r#"use mod.deck.*
Deck{slides: [
    Slide{name: @one graphic: "slides/one" notes: "first"}
    Slide{name: @two graphic: "slides/two" notes: "second" target_seconds: 30}
    Slide{name: @three cut: graphic("slides/two")}
]}"#;
    let plate = b"use mod.motion.*\nMotion{size: vec2(1920,1080) duration: 4.0 layers: [Text{text: \"Plate\"}]}";
    let tree = || memory_tree(&[
        ("deck.splash", Deck, source.as_bytes()),
        ("slides/one.splash", Motion, plate),
        ("slides/two.splash", Motion, plate),
    ]);
    let first = DeckDoc::load_tree_first(source, tree()).unwrap_or_else(|diags| panic!("{diags:#?}"));
    let whole = DeckDoc::load_tree(source, tree()).unwrap_or_else(|diags| panic!("{diags:#?}"));
    assert!(first.pending() && !whole.pending());
    assert!(matches!(first.slides[0].cut, SlideCut::Inline(_)));
    assert!(matches!(first.slides[1].cut, SlideCut::Pending) && matches!(first.slides[2].cut, SlideCut::Pending));
    // Everything the deck file itself says is there before the graphics are.
    for (first, whole) in first.slides.iter().zip(&whole.slides) {
        assert_eq!((&first.name, &first.graphic, &first.notes, first.target_seconds, first.span), (&whole.name, &whole.graphic, &whole.notes, whole.target_seconds, whole.span));
    }
    assert_eq!(first.slides[1].graphic.as_deref(), Some("slides/two.splash"));
}

/// A two-slide Motion deck whose slides both read a shared look module.
fn motion_tree(title: &str, look: &str) -> makepad_story_cut::tree::TreeContext {
    use makepad_asset_data::AssetKind::{Deck, Motion};
    let deck = "use mod.flow.*\nuse mod.cut.*\nuse mod.deck.*\nDeck{\n    title: \"Live\"\n    slides: [\n        Slide{name: @title graphic: \"slides/title\" duration: 3.0}\n        Slide{name: @other graphic: \"slides/other\" duration: 3.0}\n    ]\n}\n";
    let title = format!("use mod.motion.*\nlet look = import(\"shared/look\")\nMotion{{size: vec2(1920, 1080) duration: 3.0 layers: [Text{{name: \"title\" text: \"{title}\" size: 40 color: look.INK}}]}}\n");
    let other = "use mod.motion.*\nlet look = import(\"shared/look\")\nMotion{size: vec2(1920, 1080) duration: 3.0 layers: [Rect{size: vec2(10, 10) fill: look.INK}]}\n";
    let look = format!("let INK = {look}\n{{INK: INK}}\n");
    let files: Vec<(&str, makepad_asset_data::AssetKind, Vec<u8>)> = vec![
        ("deck.splash", Deck, deck.as_bytes().to_vec()),
        ("slides/title.splash", Motion, title.into_bytes()),
        ("slides/other.splash", Motion, other.as_bytes().to_vec()),
        ("shared/look.splash", Motion, look.into_bytes()),
    ];
    let files: Vec<(&str, makepad_asset_data::AssetKind, &[u8])> = files.iter().map(|(path, kind, bytes)| (*path, *kind, bytes.as_slice())).collect();
    memory_tree(&files)
}

fn deck_source(tree: &makepad_story_cut::tree::TreeContext) -> String {
    tree.resolve("deck.splash").unwrap().text().unwrap().to_string()
}

#[test]
fn a_code_edit_of_one_slide_evaluates_that_slide_and_takes_the_others_again() {
    let before = motion_tree("Motion, in Splash.", "#ffffff");
    let prior = DeckDoc::load_tree(&deck_source(&before), before).unwrap_or_else(|diags| panic!("{diags:#?}"));
    assert_eq!(prior.graphics.len(), 2);
    let after = motion_tree("Motion, in HELLO.", "#ffffff");
    let doc = DeckDoc::reload_tree(&deck_source(&after), after, &prior, &["slides/title.splash".into()]).unwrap_or_else(|diags| panic!("{diags:#?}"));
    // The edited slide is its new text, loaded in the new tree.
    let SlideCut::Inline(title) = &doc.slides[0].cut else { panic!("the title slide is a cut") };
    assert!(title.source.contains("Motion, in HELLO."), "{}", title.source);
    let makepad_story_cut::GraphicSrc::Tree(context) = &title.graphics[0].src else { panic!("a tree graphic") };
    assert!(context.resolve(&context.document).unwrap().text().unwrap().contains("Motion, in HELLO."));
    // The other slide is the one loaded before, not evaluated again.
    assert!(std::sync::Arc::ptr_eq(&doc.graphics["slides/other.splash"].1, &prior.graphics["slides/other.splash"].1));
    assert!(!std::sync::Arc::ptr_eq(&doc.graphics["slides/title.splash"].1, &prior.graphics["slides/title.splash"].1));
}

#[test]
fn a_code_edit_of_a_shared_module_evaluates_every_slide_again() {
    let before = motion_tree("Motion, in Splash.", "#ffffff");
    let prior = DeckDoc::load_tree(&deck_source(&before), before).unwrap_or_else(|diags| panic!("{diags:#?}"));
    let after = motion_tree("Motion, in Splash.", "#ff0000");
    let doc = DeckDoc::reload_tree(&deck_source(&after), after, &prior, &["shared/look.splash".into()]).unwrap_or_else(|diags| panic!("{diags:#?}"));
    for path in ["slides/title.splash", "slides/other.splash"] {
        assert!(!std::sync::Arc::ptr_eq(&doc.graphics[path].1, &prior.graphics[path].1), "{path} was taken again after its look changed");
    }
}

#[test]
fn a_slide_edit_that_does_not_load_names_the_slide_and_keeps_nothing_half_loaded() {
    let before = motion_tree("Motion, in Splash.", "#ffffff");
    let prior = DeckDoc::load_tree(&deck_source(&before), before).unwrap_or_else(|diags| panic!("{diags:#?}"));
    let broken = motion_tree("Motion, in Splash.\" size: oops(", "#ffffff");
    let errors = DeckDoc::reload_tree(&deck_source(&broken), broken, &prior, &["slides/title.splash".into()]).err().expect("a broken slide does not load");
    assert!(errors.iter().any(|diag| diag.slide.as_deref() == Some("title")), "{errors:#?}");
}

/// A playing slide is never "held" before its timeline reaches a hold, however
/// long nothing looked at it (a pump that found nothing for a second, a draw
/// that came late): holding is a function of the story clock alone, so a view
/// asking `holding` after any gap sees the motion still running.
#[test]
fn a_slide_whose_timeline_is_running_never_holds_before_its_hold() {
    let doc = beat_doc(Loop::Hold);
    let end = doc.slides[0].cut.duration().expect("inline cut");
    assert!(end > 7.0, "the cut outlasts its last beat");
    let mut deck = Deck::new(doc.slides.len(), 100.0);
    // Looked at once at the start, then not for 1.7 s: still playing.
    assert!(!deck.holding(&doc, 100.0));
    assert!(!deck.holding(&doc, 101.7));
    assert!((deck.slide_time(&doc, 0, 101.7) - 1.7).abs() < 1e-9);
    // The first beat is the first hold, and the frame there is the beat's.
    assert!(!deck.holding(&doc, 101.999));
    assert!(deck.holding(&doc, 102.0));
    // A release plays on to the next beat, and again no hold before it.
    deck.next(&doc, 150.0);
    assert!(!deck.holding(&doc, 150.0));
    assert!(!deck.holding(&doc, 152.4));
    assert!(deck.holding(&doc, 152.5));
    deck.next(&doc, 160.0);
    assert!(!deck.holding(&doc, 162.4));
    assert!(deck.holding(&doc, 162.5));
    // After the last beat the cut plays out to its end, and only then holds.
    deck.next(&doc, 170.0);
    assert!(!deck.holding(&doc, 170.0 + (end - 7.0) - 0.01));
    assert!(deck.holding(&doc, 170.0 + (end - 7.0) + 0.01));
}

/// The looping cut of an app slide asks for frames only while its picture
/// can change with time: a still title over a live app is one picture for the
/// whole cut (the app's own frames arrive by event), a title that glides is not.
#[test]
fn an_app_slide_over_a_still_title_has_a_picture_that_does_not_depend_on_time() {
    let source = |title: &str| format!(r#"use mod.flow.*
use mod.cut.*
use mod.deck.*
let title = Title{{text: "Counter" span: [0.0, 6.0] anchor: vec2(0.5, 0.14) {title}}}
let view = AppView{{app: @app fit: @fit span: [0.0, 6.0]}}
let s = Stack{{layers: [view.out(@frame), title.out(@frame)]}}
let out = CutOutput{{frame: s.out(@frame)}}
Deck{{slides: [Slide{{
    name: @counter
    cut: Flow{{kind: @cut output: {{width: 1920 height: 1080 fps: 30}} duration: 6.0 title, view, s, out}}
    apps: {{app: App{{app: "counter"}}}}
}}]}}
"#);
    let still = load(&source("font_size: 84.0"));
    assert_eq!(still.slides[0].play(), Loop::Whole, "an app slide without play loops its cut");
    let SlideCut::Inline(cut) = &still.slides[0].cut else { panic!("inline cut") };
    assert!(cut.picture_is_still());
    let gliding = load(&source("font_size: keys([0.0, 80.0], [3.0, 96.0])"));
    let SlideCut::Inline(cut) = &gliding.slides[0].cut else { panic!("inline cut") };
    assert!(!cut.picture_is_still());
}

#[test]
fn a_seek_puts_the_story_at_a_time_and_its_beat_follows() {
    let doc = beat_doc(Loop::Hold); // beats 2.0, 4.5, 7.0
    let mut deck = Deck::new(doc.slides.len(), 100.0);
    // Between beats: running toward the next one.
    deck.seek(&doc, 3.0, false, 200.0);
    assert_eq!(deck.beat(), 1);
    assert!(deck.paused());
    assert_eq!(deck.slide_time(&doc, 0, 500.0), 3.0, "paused: the clock stands");
    assert!(deck.holding(&doc, 500.0), "a paused story lets the picture sleep");
    // On a hold: that beat is waiting to be released.
    deck.seek(&doc, 4.5, true, 200.0);
    assert_eq!((deck.beat(), deck.paused()), (1, false));
    assert!(deck.holding(&doc, 200.0));
    // Past the last beat: the play-out.
    deck.seek(&doc, 7.5, true, 200.0);
    assert_eq!(deck.beat(), 3);
    // Before the first beat, and beyond the cut: clamped inside it.
    deck.seek(&doc, -4.0, false, 200.0);
    assert_eq!((deck.beat(), deck.slide_time(&doc, 0, 200.0)), (0, 0.0));
    deck.seek(&doc, 1.0e6, false, 200.0);
    let duration = doc.slides[0].cut.duration().unwrap();
    assert!(deck.slide_time(&doc, 0, 200.0) < duration && deck.slide_time(&doc, 0, 200.0) > duration - 0.01);
}

#[test]
fn next_after_a_seek_resumes_from_the_chosen_step() {
    let doc = beat_doc(Loop::Hold);
    let mut deck = Deck::new(doc.slides.len(), 100.0);
    // Jump to the hold of beat 1 (4.5): Next releases beat 1 and plays on toward 7.0.
    deck.seek(&doc, 4.5, true, 300.0);
    deck.next(&doc, 300.0);
    assert_eq!((deck.current(), deck.beat()), (0, 2));
    assert_eq!(deck.slide_time(&doc, 0, 300.0), 4.5);
    assert_eq!(deck.slide_time(&doc, 0, 301.0), 5.5);
    // Previous from a chosen step goes to the hold before it.
    deck.seek(&doc, 5.0, false, 400.0);
    deck.previous(&doc, 400.0);
    assert_eq!((deck.beat(), deck.slide_time(&doc, 0, 400.0)), (1, 4.5));
    // A scrub left the story paused short of its beat: Next carries it to that beat.
    deck.seek(&doc, 1.0, false, 500.0);
    deck.next(&doc, 500.0);
    assert_eq!(deck.beat(), 0);
    assert!(!deck.paused(), "the carry plays");
    assert_eq!(deck.slide_time(&doc, 0, 501.0), 2.0);
}

#[test]
fn pause_and_play_stop_and_run_the_clock_and_the_loop_restarts_a_held_end() {
    let doc = beat_doc(Loop::Hold);
    let duration = doc.slides[0].cut.duration().unwrap();
    let mut deck = Deck::new(doc.slides.len(), 100.0);
    deck.pause(&doc, 101.0);
    assert_eq!(deck.slide_time(&doc, 0, 150.0), 1.0);
    deck.play(&doc, 150.0);
    assert_eq!(deck.slide_time(&doc, 0, 150.5), 1.5);
    // The end of a hold slide holds; with the loop on it starts over.
    deck.seek(&doc, duration, true, 200.0);
    assert!(deck.holding(&doc, 200.0 + duration) || deck.beat() == 3);
    let far = 200.0 + duration + 5.0;
    assert!(deck.holding(&doc, far));
    deck.set_looped(true);
    assert!(!deck.holding(&doc, far), "a looping slide never rests at its end");
    deck.wrap(&doc, far);
    assert_eq!((deck.beat(), deck.slide_time(&doc, 0, far)), (0, 0.0));
}

#[test]
fn previous_goes_to_the_beat_before_the_time_after_a_scrub_or_while_playing() {
    let mut doc = beat_doc(Loop::Hold);
    doc.slides[1].beats = vec![1.0, 2.0, 3.0, 4.5];
    let mut deck = Deck::new(doc.slides.len(), 0.0);
    deck.go(&doc, 1, 10.0);
    deck.tick(12.0);
    // A scrub into the middle of a segment, paused: Left holds the beat before.
    deck.seek(&doc, 5.5, false, 100.0);
    deck.previous(&doc, 100.0);
    assert_eq!((deck.slide_time(&doc, 1, 100.0), deck.beat(), deck.paused()), (4.5, 3, true));
    // Playing between beats: the playhead is at 3.9, Left goes to 3.0, then 2.0, 1.0, the start.
    deck.seek(&doc, 3.4, true, 200.0);
    deck.previous(&doc, 200.5);
    assert_eq!(deck.slide_time(&doc, 1, 200.5), 3.0);
    for (now, want) in [(200.51, 2.0), (200.52, 1.0), (200.53, 0.0)] {
        deck.previous(&doc, now);
        assert_eq!(deck.slide_time(&doc, 1, now), want);
    }
    assert_eq!(deck.current(), 1);
    // Past the last beat (the play-out) Left goes to the last beat.
    deck.seek(&doc, 5.6, true, 300.0);
    deck.previous(&doc, 300.0);
    assert_eq!(deck.slide_time(&doc, 1, 300.0), 4.5);
    // At the start, the previous slide.
    deck.seek(&doc, 0.0, false, 400.0);
    deck.previous(&doc, 400.0);
    assert_eq!(deck.current(), 0);
}

#[test]
fn next_while_the_story_plays_out_finishes_it_and_sits_at_the_end() {
    let mut doc = beat_doc(Loop::Hold);
    doc.slides[0].beats = vec![1.0, 3.0];
    let duration = doc.slides[0].cut.duration().unwrap();
    let mut deck = Deck::new(doc.slides.len(), 0.0);
    deck.seek(&doc, 5.9, true, 100.0);
    assert!(5.9 < duration);
    deck.next(&doc, 100.0);
    assert_eq!(deck.current(), 0, "the slide is unchanged");
    assert!(deck.slide_time(&doc, 0, 100.0) < duration, "carried on, not cut");
    assert_eq!(deck.slide_time(&doc, 0, 100.0 + RUSH_SECONDS), duration);
    assert!(deck.holding(&doc, 100.0 + RUSH_SECONDS));
    deck.next(&doc, 101.0);
    assert_eq!(deck.current(), 1, "a press at the end goes on");
    // At a hold a press releases toward the next step.
    deck.go(&doc, 0, 200.0);
    deck.seek(&doc, 3.0, false, 200.0);
    deck.next(&doc, 200.0);
    assert!(!deck.paused() && deck.slide_time(&doc, 0, 200.5) > 3.0);
    assert_eq!(deck.current(), 0);
}

#[test]
fn the_arrows_walk_every_stop_of_every_slide_forward_and_back() {
    let mut doc = beat_doc(Loop::Hold);
    doc.slides[0].beats = vec![2.0, 4.5];
    doc.slides[1].beats = vec![1.0];
    doc.slides[1].looping = Loop::Hold;
    let d0 = doc.slides[0].cut.duration().unwrap();
    let d1 = doc.slides[1].cut.duration().unwrap();
    let mut deck = Deck::new(doc.slides.len(), 0.0);
    let mut now = 100.0;
    let at = |deck: &Deck, now: f64| (deck.current(), deck.slide_time(&doc, deck.current(), now));
    let mut forward = vec![at(&deck, now + 20.0)];
    for _ in 0..5 {
        deck.next(&doc, now + 20.0);
        now += 40.0;
        forward.push(at(&deck, now));
        deck.tick(now);
    }
    assert_eq!(forward[..5], [(0, 2.0), (0, 4.5), (0, d0), (1, 1.0), (1, d1)]);
    assert_eq!(forward[5].0, 2, "a press at the end goes on to the next slide");
    // Back from the start of slide 2: the end of slide 1, its beat, its start, the end of slide 0, its beats, its start.
    let mut deck = Deck::new(doc.slides.len(), 0.0);
    deck.go(&doc, 2, 500.0);
    deck.tick(510.0);
    let mut back = vec![];
    let mut now = 600.0;
    deck.seek(&doc, 0.0, false, now);
    for _ in 0..7 {
        deck.previous(&doc, now);
        back.push(at(&deck, now));
        now += 0.01;
    }
    assert_eq!(back, [(1, d1), (1, 1.0), (1, 0.0), (0, d0), (0, 4.5), (0, 2.0), (0, 0.0)]);
    // From mid-animation, also after a scrub: the stop before the playhead.
    deck.seek(&doc, 3.3, true, 700.0);
    deck.previous(&doc, 700.4);
    assert_eq!(at(&deck, 700.4), (0, 2.0));
    deck.seek(&doc, d0 - 1.0, false, 800.0);
    deck.previous(&doc, 800.0);
    assert_eq!(at(&deck, 800.0), (0, 4.5));
}

/// Slide 0 holds (beats 2.0 and 4.5), slide 1 loops (beat 1.0), slide 2 is the tour's third.
fn two_kinds_doc(looping: Loop) -> DeckDoc {
    let mut doc = beat_doc(Loop::Hold);
    doc.slides[0].beats = vec![2.0, 4.5];
    doc.slides[1].beats = vec![1.0];
    doc.slides[1].looping = looping;
    doc
}

#[test]
fn the_stops_of_a_slide_are_its_start_beats_and_end() {
    let doc = two_kinds_doc(Loop::Whole);
    let d0 = doc.slides[0].cut.duration().unwrap();
    assert_eq!(stops(&doc.slides[0]), [0.0, 2.0, 4.5, d0]);
    // A looping slide has an end stop too: the end of its first pass.
    let d1 = doc.slides[1].cut.duration().unwrap();
    assert_eq!(stops(&doc.slides[1]), [0.0, 1.0, d1]);
}

#[test]
fn the_arrows_walk_every_stop_across_a_holding_and_a_looping_slide() {
    let doc = two_kinds_doc(Loop::Whole);
    let d0 = doc.slides[0].cut.duration().unwrap();
    let d1 = doc.slides[1].cut.duration().unwrap();
    let mut now = 100.0;
    let mut deck = Deck::new(doc.slides.len(), now);
    let at = |deck: &Deck, now: f64| (deck.current(), deck.slide_time(&doc, deck.current(), now));
    let mut walk = vec![at(&deck, now)];
    for _ in 0..7 {
        let before = deck.current();
        deck.next(&doc, now);
        // A release from the last beat of a looping slide plays on and loops by itself;
        // the next press, still in the pass, sits it at the end.
        if deck.current() == before && deck.beat() == doc.slides[before].beats.len() && doc.slides[before].play() != Loop::Hold && !deck.holding(&doc, now) {
            now += 0.5;
            deck.next(&doc, now);
        }
        if deck.current() != before {
            // A new slide is entered at its start.
            walk.push(at(&deck, now));
            deck.tick(now + 5.0);
            now -= 39.99;
        } else {
            // Every press lands at its stop, and time passing never takes the story off it.
            walk.push(at(&deck, now + 30.0));
            assert_eq!(at(&deck, now + 500.0), *walk.last().unwrap());
            assert!(deck.holding(&doc, now + 500.0), "{walk:?}");
        }
        now += 40.0;
    }
    assert_eq!(walk, [(0, 0.0), (0, 2.0), (0, 4.5), (0, d0), (1, 0.0), (1, 1.0), (1, d1), (2, 0.0)]);
    // Back: slide 2's start goes to the looping slide 1's end (sitting there), then its stops,
    // then the holding slide 0's end and its stops.
    let mut back = vec![];
    deck.seek(&doc, 0.0, false, now);
    for _ in 0..7 {
        deck.previous(&doc, now);
        deck.tick(now + 5.0);
        back.push(at(&deck, now));
        now += 0.01;
    }
    assert_eq!(back, [(1, d1), (1, 1.0), (1, 0.0), (0, d0), (0, 4.5), (0, 2.0), (0, 0.0)]);
    // Left at the very first start stays.
    deck.previous(&doc, now);
    assert_eq!(deck.current(), 0);
    assert!(deck.slide_time(&doc, 0, now) < 0.05);
}

#[test]
fn a_looping_slide_sits_at_its_end_until_played_and_then_loops() {
    let doc = two_kinds_doc(Loop::Whole);
    let d1 = doc.slides[1].cut.duration().unwrap();
    let mut deck = Deck::new(doc.slides.len(), 0.0);
    deck.go(&doc, 1, 10.0);
    deck.tick(20.0);
    // Left from the next slide's start: the end of slide 1, sitting, not looping.
    deck.go(&doc, 2, 30.0);
    deck.previous(&doc, 30.0);
    assert_eq!(deck.current(), 1);
    assert_eq!(deck.slide_time(&doc, 1, 30.0), d1);
    assert_eq!(deck.slide_time(&doc, 1, 30.0 + d1 * 3.5), d1);
    assert!(deck.holding(&doc, 31.0) && !deck.paused());
    assert!(deck.next_leaves(&doc, 31.0));
    // Played, it loops again.
    deck.play(&doc, 40.0);
    assert!(deck.slide_time(&doc, 1, 40.0 + d1 * 0.5) < d1 * 0.6);
    assert!(!deck.holding(&doc, 40.0 + d1 * 0.5));
    // Right in the middle of a loop finishes the pass and sits at the end; again, on to slide 2.
    deck.seek(&doc, 1.5, true, 50.0);
    assert!(!deck.next_leaves(&doc, 50.0));
    deck.next(&doc, 50.0);
    assert_eq!((deck.current(), deck.slide_time(&doc, 1, 50.0 + d1)), (1, d1));
    deck.next(&doc, 51.0);
    assert_eq!(deck.current(), 2);
}

#[test]
fn a_looping_slide_loops_by_itself_after_its_last_beat_without_key_presses() {
    let doc = two_kinds_doc(Loop::Whole);
    let d1 = doc.slides[1].cut.duration().unwrap();
    let mut deck = Deck::new(doc.slides.len(), 0.0);
    deck.go(&doc, 1, 0.0);
    deck.tick(5.0);
    // Release the last beat (1.0) at 100 s and press nothing more.
    deck.next(&doc, 100.0);
    assert_eq!(deck.beat(), 1);
    let at = |t: f64| deck.slide_time(&doc, 1, 100.0 + t);
    assert!((at(0.5) - 1.5).abs() < 1e-9);
    assert!((at(d1 - 1.0 + 0.5) - 0.5).abs() < 1e-9, "wrapped past the end on its own");
    assert!((at(3.0 * d1 - 1.0 + 0.5) - 0.5).abs() < 1e-9, "and on and on");
    assert!(!deck.holding(&doc, 100.0 + d1 + 1.0));
}

#[test]
fn left_from_a_start_goes_to_the_end_of_a_holding_or_a_looping_previous_slide() {
    for looping in [Loop::Hold, Loop::Whole, Loop::Range(1.0, 5.0)] {
        let mut doc = two_kinds_doc(looping);
        doc.slides[0].looping = looping;
        let d0 = doc.slides[0].cut.duration().unwrap();
        let mut deck = Deck::new(doc.slides.len(), 0.0);
        deck.go(&doc, 1, 10.0);
        deck.tick(20.0);
        deck.seek(&doc, 0.0, false, 30.0);
        deck.previous(&doc, 30.0);
        assert_eq!((deck.current(), deck.slide_time(&doc, 0, 31.0)), (0, d0), "{looping:?}");
        assert!(deck.holding(&doc, 31.0));
    }
}

#[test]
fn right_to_the_end_stop_rushes_like_a_beat_and_then_holds() {
    for looping in [Loop::Hold, Loop::Whole] {
        let doc = two_kinds_doc(looping);
        let d = doc.slides[1].cut.duration().unwrap();
        let mut deck = Deck::new(doc.slides.len(), 0.0);
        deck.go(&doc, 1, 100.0);
        deck.next(&doc, 100.0 + 1.0); // to the beat 1.0
        let t0 = deck.slide_time(&doc, 1, 102.5);
        deck.next(&doc, 102.5); // toward the end stop
        assert!(deck.slide_time(&doc, 1, 102.5) - t0 < 1e-9, "no jump at the press");
        let mid = deck.slide_time(&doc, 1, 102.5 + RUSH_SECONDS / 2.0);
        assert!(mid > t0 && mid < d, "mid-rush {mid} in ({t0}, {d})");
        assert_eq!(deck.slide_time(&doc, 1, 102.5 + RUSH_SECONDS + 0.01), d);
        assert_eq!(deck.slide_time(&doc, 1, 500.0), d);
        assert!(!deck.holding(&doc, 102.5 + RUSH_SECONDS / 2.0));
        assert!(deck.holding(&doc, 103.5));
        deck.next(&doc, 104.0);
        assert_eq!(deck.current(), 2);
    }
}
