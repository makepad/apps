//! Inputs keyed by identifier and resolved once to an index, written without
//! a reload; a document that is a function of a game's script (a fn image);
//! one document and one VM shared by many instances.

use makepad_motion::*;
use makepad_script::{ScriptFnImage, ScriptMod, ScriptVm, ScriptVmBase, ScriptVmHost};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

/// Allocations made by this thread: a write is measured, not guessed.
struct Counting;

thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.with(|a| a.set(a.get() + 1));
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.with(|a| a.set(a.get() + 1));
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

fn allocs() -> u64 {
    ALLOCS.with(Cell::get)
}

fn id(name: &str) -> LiveId {
    LiveId::from_str(name)
}

/// The body of the document, as a game writes it inline and as a file does.
const BODY: &str = r#"Motion{
    size: vec2(200, 100) duration: 2.0
    inputs: {title: "HELLO" shade: 0.5 accent: #ff4a3d glow: 1.0}
    layers: [
        Rect{size: vec2(50, 50) fill: #3355ff x: fn(t) { in(@shade) * 100.0 + t } opacity: fn(t) { in(@glow) }}
        Text{text: in(@title) at: vec2(100, 80) size: 20 color: in(@accent)}
    ]
}"#;

fn file() -> String {
    format!("use mod.motion.*\n{BODY}")
}

/// A fn image of `|| {BODY}` as the game's script wrote it at `line`
/// (0-based) of `game.splash`.
fn image(prelude: &str) -> ScriptFnImage {
    let host = Box::leak(Box::new(ScriptVmHost::new(0i32, ())));
    let mut vm = ScriptVm { host: host.host_context(), bx: Box::new(ScriptVmBase::new()) };
    vm.capture_errors();
    let code = format!("{prelude}|| {BODY}");
    let f = vm.eval(ScriptMod::new("", "", "game.splash", 0, 0, &code, &[]));
    let errors = vm.take_errors();
    assert!(errors.is_empty(), "{errors:?}");
    vm.fn_image(f.as_object().expect("a fn")).expect("an image")
}

fn texts(frame: &MotionFrame) -> Vec<String> {
    frame.items.iter().filter_map(|item| match &item.kind {
        ItemKind::Text { text, .. } => Some(text.clone()),
        _ => None,
    }).collect()
}

fn rect_x(frame: &MotionFrame) -> f64 {
    frame.items.iter().find(|item| matches!(item.kind, ItemKind::Rect { .. })).map(|item| item.world.apply([0.0, 0.0])[0]).expect("the rect")
}

#[test]
fn a_document_from_a_fn_image_draws_the_same_frame_as_from_a_file() {
    let from_file = MotionDoc::load(&file()).unwrap_or_else(|d| panic!("{d:?}"));
    let from_image = MotionDoc::load_image(&image(""), &[], MotionContext::default()).unwrap_or_else(|d| panic!("{d:?}"));
    assert!(matches!(from_image.source, DocSource::Image(_)));
    assert_eq!(from_image.inputs.iter().map(|i| i.id).collect::<Vec<_>>(), vec![id("title"), id("shade"), id("accent"), id("glow")]);
    for t in [0.0, 0.7, 1.5] {
        let (a, b) = (from_file.eval(t), from_image.eval(t));
        assert_eq!(a, b, "t = {t}");
        assert!(a.errors.is_empty(), "{:?}", a.errors);
    }
    assert_eq!(texts(&from_image.eval(0.0)), ["HELLO"]);
    // Params fill the declared inputs as a file's do.
    let params = [(id("title"), InputValue::Text("LAP 3".into())), (id("shade"), InputValue::Number(1.0))];
    let given = MotionDoc::load_image(&image(""), &params, MotionContext::default()).unwrap();
    assert_eq!(texts(&given.eval(0.0)), ["LAP 3"]);
    assert_eq!(rect_x(&given.eval(0.0)), 100.0);
}

#[test]
fn an_input_written_by_index_changes_the_frame_without_a_reload() {
    for doc in [MotionDoc::load(&file()).unwrap(), MotionDoc::load_image(&image(""), &[], MotionContext::default()).unwrap()] {
        let shade = doc.input_index(id("shade")).expect("declared");
        let title = doc.input_index(id("title")).expect("declared");
        assert!(!doc.structural[shade], "a property function reads it at frame time");
        assert!(doc.structural[title], "the document read it to decide its text");
        assert_eq!(doc.input_index(id("nope")), None);
        let loads = doc.evaluations();
        let mut mine = doc.new_inputs();
        assert_eq!(rect_x(&doc.eval_with(0.0, &mut mine)), 50.0);
        assert_eq!(doc.write_input(&mut mine, shade, InputValue::Number(1.0)), Ok(InputWrite::Applied));
        assert_eq!(rect_x(&doc.eval_with(0.0, &mut mine)), 100.0);
        assert_eq!(doc.write_input(&mut mine, shade, InputValue::Number(1.0)), Ok(InputWrite::Unchanged));
        // The document's own values are not the instance's.
        assert_eq!(rect_x(&doc.eval(0.0)), 50.0);
        assert_eq!(doc.evaluations(), loads, "no re-evaluation");
        // A value of the wrong type is an error, not a write.
        assert!(doc.write_input(&mut mine, shade, InputValue::Text("x".into())).is_err());
        assert_eq!(rect_x(&doc.eval_with(0.0, &mut mine)), 100.0);
        // The structural input says so.
        assert_eq!(doc.write_input(&mut mine, title, InputValue::Text("GO".into())), Ok(InputWrite::Reevaluate));
    }
}

#[test]
fn instances_of_one_document_keep_their_own_values_in_one_vm() {
    let doc = MotionDoc::load(&file()).unwrap();
    let shade = doc.input_index(id("shade")).unwrap();
    let (mut a, mut b) = (doc.new_inputs(), doc.new_inputs());
    doc.write_input(&mut a, shade, InputValue::Number(0.0)).unwrap();
    doc.write_input(&mut b, shade, InputValue::Number(2.0)).unwrap();
    for _ in 0..3 {
        assert_eq!(rect_x(&doc.eval_with(0.0, &mut a)), 0.0);
        assert_eq!(rect_x(&doc.eval_with(0.0, &mut b)), 200.0);
    }
    assert_eq!(a.get(shade), Some(&InputValue::Number(0.0)));
    assert_eq!(b.get(shade), Some(&InputValue::Number(2.0)));
}

#[test]
fn a_structural_input_evaluates_again_and_each_document_keeps_its_own_structure() {
    for doc in [MotionDoc::load(&file()).unwrap(), MotionDoc::load_image(&image(""), &[], MotionContext::default()).unwrap()] {
        let title = doc.input_index(id("title")).unwrap();
        let shade = doc.input_index(id("shade")).unwrap();
        let loads = doc.evaluations();
        let mut mine = doc.new_inputs();
        assert_eq!(doc.write_input(&mut mine, title, InputValue::Text("LAP 2".into())), Ok(InputWrite::Reevaluate));
        doc.write_input(&mut mine, shade, InputValue::Number(1.0)).unwrap();
        let second = doc.reevaluate(&mine).unwrap_or_else(|d| panic!("{d:?}"));
        assert_eq!(second.evaluations(), loads + 1, "the same VM evaluated the document once more");
        assert_eq!(doc.evaluations(), loads + 1);
        // Both documents draw their own text, and keep doing so as the VM collects garbage.
        for _ in 0..30 {
            assert_eq!(texts(&doc.eval(0.0)), ["HELLO"]);
            let frame = second.eval_with(0.0, &mut mine);
            assert_eq!(texts(&frame), ["LAP 2"]);
            assert_eq!(rect_x(&frame), 100.0);
        }
        // The one the instance wrote shade into carries it; the original does not.
        assert_eq!(second.input(id("shade")), Some(&InputValue::Number(1.0)));
        assert_eq!(doc.input(id("shade")), Some(&InputValue::Number(0.5)));
        drop(doc);
        assert_eq!(texts(&second.eval(0.0)), ["LAP 2"]);
    }
}

#[test]
fn a_name_of_the_games_scope_in_an_inline_document_is_an_error_naming_the_game_file() {
    // The game writes `let ACCENT` and the inline document uses it: the
    // document VM has no such name.
    let host = Box::leak(Box::new(ScriptVmHost::new(0i32, ())));
    let mut vm = ScriptVm { host: host.host_context(), bx: Box::new(ScriptVmBase::new()) };
    vm.capture_errors();
    let code = "let ACCENT = #ff4a3d\nlet doc = || Motion{\n    size: vec2(10, 10) duration: 1.0\n    layers: [Rect{size: vec2(5, 5) fill: ACCENT}]\n}\ndoc";
    let f = vm.eval(ScriptMod::new("", "", "lib/race_hud.splash", 0, 0, code, &[]));
    assert!(vm.take_errors().is_empty());
    let image = vm.fn_image(f.as_object().unwrap()).unwrap();
    let errors = MotionDoc::load_image(&image, &[], MotionContext::default()).expect_err("a capture is an error");
    let error = errors.iter().find(|d| d.is_error() && d.message.contains("ACCENT")).expect("an error naming the variable");
    eprintln!("capture errors:\n{}", errors.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n"));
    assert_eq!(error.file.as_deref(), Some("lib/race_hud.splash"));
    assert_eq!(error.loc.line, 4, "the line the name is written on");
    assert!(error.to_string().starts_with("lib/race_hud.splash:4:"), "{error}");
    assert!(error.hint.as_deref().is_some_and(|h| h.contains("input")), "says game values come in as inputs: {error}");
    assert!(errors.iter().filter(|d| d.is_error()).all(|d| d.file.as_deref() == Some("lib/race_hud.splash")));
}

/// 1000 frames of an input write plus an evaluation: no reload, and the write allocates nothing.
#[test]
fn a_thousand_frames_of_input_writes_reload_nothing_and_allocate_nothing_in_the_write() {
    let doc = MotionDoc::load_image(&image(""), &[], MotionContext::default()).unwrap();
    let shade = doc.input_index(id("shade")).unwrap();
    let glow = doc.input_index(id("glow")).unwrap();
    let mut mine = doc.new_inputs();
    let loads = doc.evaluations();
    let (mut write_allocs, mut eval_allocs) = (0u64, 0u64);
    // (A first frame warms what the VM sets up lazily.)
    doc.eval_with(0.0, &mut mine);
    for frame in 0..1000 {
        let t = frame as f64 / 60.0;
        let before = allocs();
        let a = doc.write_input(&mut mine, shade, InputValue::Number(t));
        let b = doc.write_input(&mut mine, glow, InputValue::Number(t.fract()));
        write_allocs += allocs() - before;
        assert!(matches!(a, Ok(InputWrite::Applied | InputWrite::Unchanged)) && matches!(b, Ok(InputWrite::Applied | InputWrite::Unchanged)));
        let before = allocs();
        let shown = doc.eval_with(t, &mut mine);
        eval_allocs += allocs() - before;
        assert!((rect_x(&shown) - (t * 100.0 + t)).abs() < 1e-6);
    }
    println!("1000 frames: {write_allocs} allocations in the writes, {} per frame in eval", eval_allocs as f64 / 1000.0);
    assert_eq!(doc.evaluations(), loads, "0 reloads");
    assert_eq!(write_allocs, 0, "the write allocates nothing");
}

/// An inline document's `use` lines and imports are found from its tokens.
#[test]
fn an_inline_documents_modules_and_globs_are_read_from_its_tokens() {
    let host = Box::leak(Box::new(ScriptVmHost::new(0i32, ())));
    let mut vm = ScriptVm { host: host.host_context(), bx: Box::new(ScriptVmBase::new()) };
    let make = |vm: &mut ScriptVm, code: &str| {
        vm.capture_errors();
        let f = vm.eval(ScriptMod::new("", "", "game.splash", 0, 0, code, &[]));
        vm.fn_image(f.as_object().unwrap()).unwrap()
    };
    // Comments and strings are not tokens.
    let image = make(&mut vm, "|| {\n // use mod.motion.modules.paths.*\n let s = \"mod.motion.modules.words\"\n use mod.motion.modules.numbers.*\n let m = import(asset(\"ast_1\", \"rev_1\"))\n Motion{size: vec2(10, 10) duration: 1.0 layers: []}\n}");
    assert_eq!(makepad_motion::modules::scan_image(&image), vec![
        makepad_motion::modules::ModuleRef::Named("numbers".into()),
        makepad_motion::modules::ModuleRef::Asset { id: "ast_1".into(), rev: "rev_1".into() },
    ]);
    let both = make(&mut vm, "|| { use mod.motion.*; use mod.scene.*; Motion{size: vec2(10, 10) duration: 1.0 layers: []} }");
    let errors = MotionDoc::load_image(&both, &[], MotionContext::default()).expect_err("two globs");
    assert!(errors.iter().any(|d| d.message.contains("globs two vocabularies")), "{errors:?}");
}
