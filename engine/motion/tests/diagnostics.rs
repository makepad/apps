//! Problems are reported where they are written, name the property, and
//! teach the fix.

use makepad_motion::*;

fn errors(source: &str) -> Vec<Diagnostic> {
    match MotionDoc::load(source) {
        Ok(doc) => panic!("loaded; warnings: {:?}", doc.warnings),
        Err(diags) => diags,
    }
}

fn has(diags: &[Diagnostic], needle: &str) -> Diagnostic {
    diags.iter().find(|d| d.to_string().contains(needle)).cloned().unwrap_or_else(|| {
        panic!("no diagnostic with {needle:?} in:\n{}", diags.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n"))
    })
}

const OK: &str = "use mod.motion.*; Motion{size: vec2(100, 100) duration: 2.0 layers: [Rect{size: vec2(10, 10) fill: #fff}]}";

#[test]
fn a_minimal_document_loads() {
    let doc = MotionDoc::load(OK).unwrap();
    assert_eq!(doc.size, [100.0, 100.0]);
    assert_eq!(doc.layers.len(), 1);
    assert!(!doc.layers[0].animated);
    let frame = doc.eval(1.0);
    assert!(frame.items[0].is_static);
    assert_eq!(frame.static_prefix(), 1);
}

#[test]
fn misspelt_fields_eases_and_inputs_suggest_the_right_name() {
    let source = r#"use mod.motion.*; Motion{
    size: vec2(100, 100) duration: 2.0
    inputs: {headline: "Hi"}
    layers: [
        Rect{size: vec2(10, 10) fil: #fff}
        Text{text: in(@headlin) opacity: tween(0, 1, 0, 1, @ease_out_bak)}
    ]
}"#;
    let diags = errors(source);
    // The Rect component says it has no `fil`, at the field, with the
    // nearest names and (in the hint) all of its fields.
    let fil = has(&diags, "property fil not defined on type Rect");
    assert!(fil.message.contains("Did you mean: fill"), "{fil}");
    assert_eq!(fil.loc.line, 5);
    assert!(fil.hint.as_deref().unwrap().starts_with("Rect fields: "), "{fil}");
    let input = has(&diags, "`in(@headlin)` names no input");
    assert!(input.message.contains("Did you mean `headline`"), "{input}");
    assert_eq!(input.loc.line, 6);
    let ease = has(&diags, "`@ease_out_bak` is not an ease");
    assert!(ease.hint.as_deref().unwrap().contains("@ease_out_back"), "{ease}");
    assert_eq!(ease.path, "layers[1].opacity");
}

#[test]
fn shapes_of_values_are_explained() {
    let source = r#"use mod.motion.*; Motion{
    size: vec2(100, 100) duration: 2.0
    layers: [
        Rect{size: 10 fill: "red"}
        Path{svg: "L0 0" stroke: #fff}
        Rect{size: vec2(1, 1) fill: #fff opacity: tween(0, 1, 1.0, 0.5)}
        Rect{size: vec2(1, 1) fill: #fff x: keys([1, 0], [0.5, 10])}
        Blob{}
    ]
}"#;
    let diags = errors(source);
    has(&diags, "`fill` is a colour");
    has(&diags, "path data starts with a move");
    let tween = has(&diags, "`tween` ends at 0.5 s, not after its start at 1 s");
    assert!(tween.hint.as_deref().unwrap().contains("(from, to, start, end, ease)"));
    has(&diags, "key times must increase: 0.5 s comes after 1 s");
    let blob = has(&diags, "variable Blob not found");
    assert_eq!(blob.loc.line, 8);
    // Sorted by position.
    assert!(diags.windows(2).all(|w| w[0].loc <= w[1].loc));
}

#[test]
fn a_file_that_is_not_a_motion_says_what_to_write() {
    let diags = errors("use mod.motion.*; Rect{size: vec2(1, 1)}");
    let d = has(&diags, "one `Motion{...}`");
    assert!(d.hint.as_deref().unwrap().contains("Motion{ size:"));
    let diags = errors("use mod.motion.*; Motion{layers: []}");
    has(&diags, "declares its `duration`");
    assert!(!diags.iter().any(|d| d.message.contains("size")), "size may be left out: {diags:?}");
    let diags = errors("use mod.motion.*; Motion{size: vec2(10, 10) duration: 1 layers: [Rect{size: vec2(1, 1) fill: @spring(1, 2)}]}");
    let d = has(&diags, "call target is not a function");
    assert!(d.hint.as_deref().unwrap().contains("spring(170, 26)"));
}

#[test]
fn warnings_do_not_stop_a_document() {
    let source = r#"use mod.motion.*; Motion{
    size: vec2(100, 100) duration: 2.0
    layers: [
        Rect{size: vec2(10, 10)}
        Rect{size: vec2(1, 1) fill: #fff opacity: tween(0, 1, 0, 3)}
    ]
}"#;
    let doc = MotionDoc::load(source).unwrap();
    let text: Vec<String> = doc.warnings.iter().map(|d| d.to_string()).collect();
    assert!(text.iter().any(|t| t.contains("neither `fill` nor `stroke`")), "{text:#?}");
    assert!(text.iter().any(|t| t.contains("after the document's end")), "{text:#?}");
}

/// A font that is neither bundled nor installable stops the document: no
/// other face draws in its place.
#[test]
fn an_unknown_font_is_an_error() {
    let errors = MotionDoc::load(r#"use mod.motion.*; Motion{size: vec2(100, 100) duration: 2.0 layers: [Text{text: "x" font: Font{family: "Comic Sans"}}]}"#).unwrap_err();
    assert!(errors.iter().any(|d| d.is_error() && d.message.contains("\"Comic Sans\" is no document font")), "{errors:?}");
}

#[test]
fn inputs_are_typed_and_host_values_checked() {
    let source = r#"use mod.motion.*; Motion{
    size: vec2(100, 100) duration: 2.0
    inputs: {title: "Hi" tint: #ff0000 logo: input(@image) size: 12}
    layers: [
        Text{text: in(@title) color: in(@tint) size: in(@size)}
        Image{src: in(@logo) size: vec2(10, 10)}
        Rect{size: vec2(1, 1) fill: in(@title)}
    ]
}"#;
    let diags = errors(source);
    // Inputs are values when the document evaluates, so a wrong one is
    // reported as the value it is.
    has(&diags, "`fill` is a colour (`#rrggbb`, `#rrggbbaa` or `rgba(r, g, b, a)` in 0..1), not the string \"Hi\"");
    let source = source.replace("        Rect{size: vec2(1, 1) fill: in(@title)}\n", "");
    let doc = MotionDoc::load(&source).unwrap();
    assert_eq!(doc.inputs.iter().map(|i| (i.name.as_str(), i.ty)).collect::<Vec<_>>(), vec![
        ("title", InputType::Text), ("tint", InputType::Color), ("logo", InputType::Image), ("size", InputType::Number),
    ]);
    let with = doc.with_inputs(&[(LiveId::from_str("logo"), InputValue::Asset(AssetRef { id: "ast_1".into(), rev: "arev_1".into() }))]).unwrap();
    assert_eq!(with.dependencies(), vec![AssetRef { id: "ast_1".into(), rev: "arev_1".into() }]);
    let bad = doc.with_inputs(&[(LiveId::from_str("tint"), InputValue::Text("red".into()))]).unwrap_err();
    has(&bad, "gives input `tint` text but it is @color");
    let unknown = doc.with_inputs(&[(LiveId::from_str_with_lut("titel").unwrap(), InputValue::Text("x".into()))]).unwrap();
    assert!(unknown.warnings.iter().any(|w| w.message.contains("Did you mean `title`")), "{:?}", unknown.warnings);
}

#[test]
fn inputs_compute_with_splash_vectors_and_math() {
    let doc = MotionDoc::load(r#"use mod.motion.*; Motion{size: vec2(200, 100) duration: 1
        inputs: {target: vec2(100, 50) box: vec2(40, 20) gain: 3}
        layers: [Rect{at: in(@target) - in(@box) * 0.5 size: in(@box) * in(@gain) / 3 fill: #fff rotate: sin(0.5) * 0}]}"#).unwrap();
    let frame = doc.eval(0.0);
    assert_eq!(frame.items[0].world.apply([0.0, 0.0]), [80.0, 40.0]);
    let ItemKind::Rect { size, .. } = &frame.items[0].kind else { panic!() };
    assert_eq!(*size, [40.0, 20.0]);
    let moved = doc.with_inputs(&[(LiveId::from_str("target"), InputValue::Vec2([10.0, 10.0]))]).unwrap();
    assert_eq!(moved.eval(0.0).items[0].world.apply([0.0, 0.0]), [-10.0, 0.0]);
}

#[test]
fn a_runaway_loop_is_an_error_not_a_hang() {
    let diags = errors("use mod.motion.*; let i = 0\nfor k in 0..100000000 { i = i + 1 }\nMotion{size: vec2(1, 1) duration: 1 layers: []}");
    assert!(!diags.is_empty());
}

#[test]
fn a_document_without_size_is_1920_by_1080() {
    let doc = MotionDoc::load("use mod.motion.*; Motion{duration: 2 layers: [Rect{size: [10, 10] fill: #fff}]}").unwrap();
    assert_eq!(doc.size, [1920.0, 1080.0]);
    let written = MotionDoc::load("use mod.motion.*; Motion{size: vec2(1920, 1080) duration: 2 layers: [Rect{size: [10, 10] fill: #fff}]}").unwrap();
    assert_eq!(doc.eval(1.0), written.eval(1.0));
}

// `mod.scene` is the scene3d feature's vocabulary
#[cfg(feature = "scene3d")]
#[test]
fn a_file_globs_one_vocabulary() {
    // Two globs: an error naming both (and how to fix it).
    let diags = errors("use mod.motion.*; use mod.scene.*\nMotion{size: vec2(10, 10) duration: 1 layers: []}");
    let d = has(&diags, "globs two vocabularies");
    assert!(d.message.contains("mod.motion.*") && d.message.contains("mod.scene.*"), "{}", d.message);
    // A named import is not a glob.
    MotionDoc::load("use mod.motion.*; use mod.scene; use mod.post; use mod.kernel\nMotion{size: vec2(10, 10) duration: 1 layers: []}").unwrap();
    // A glob in a string or a comment is nothing.
    MotionDoc::load("use mod.motion.*\n// use mod.scene.*\nlet s = \"use mod.post.*\"\nMotion{size: vec2(10, 10) duration: 1 layers: []}").unwrap();
}

#[cfg(feature = "scene3d")]
#[test]
fn the_forms_are_one_implementation_in_every_vocabulary() {
    // `tween` of `mod.motion`, of `mod.scene` and of `mod.forms` are the identical value, and a scene
    // reads a form made with either.
    let doc = MotionDoc::load("use mod.motion.*; use mod.scene\nlet same = tween == scene.tween && keys == scene.keys && repeat == scene.repeat && rgb == scene.rgb\nMotion{size: vec2(10, 10) duration: 1 layers: [Rect{size: vec2(10, 10) fill: #ffffff opacity: if same { 1.0 } else { 0.25 }}]}").unwrap_or_else(|d| panic!("{d:?}"));
    let frame = doc.eval(0.0);
    assert!(frame.errors.is_empty(), "{:?}", frame.errors);
    assert_eq!(frame.items[0].opacity, 1.0, "mod.motion and mod.scene carry the same forms");
}

/// A document from before modules (no `use` lines) fails with the vocabulary
/// each name is in and the `use` line that brings it, not a blank picture.
// `mod.scene` is the scene3d feature's vocabulary
#[cfg(feature = "scene3d")]
#[test]
fn a_document_without_its_use_lines_names_the_vocabulary_it_needs() {
    let diags = errors("Motion{size: vec2(100, 100) duration: 2.0 layers: [Scene3D{camera: Camera{} children: [Mesh{}] post: [Glow{}]}]}");
    let motion = has(&diags, "variable Motion not found");
    assert!(motion.to_string().contains("starts with `use mod.motion.*`"), "{motion}");
    assert!(!motion.to_string().contains("Did you mean"), "{motion}");
    let scene = errors("use mod.motion.*; Motion{size: vec2(100, 100) duration: 2.0 layers: [Scene3D{camera: Camera{} children: [Mesh{}]}]}");
    let mesh = has(&scene, "variable Mesh not found");
    assert!(mesh.to_string().contains("add `use mod.scene` and write `scene.Mesh`"), "{mesh}");
}

#[test]
fn a_module_problem_met_many_times_is_said_once() {
    // Two problems per call, called per word: a module's errors all sit at
    // the document's home, interleaved; each is said once.
    let modules = modules::Modules::from_sources([("words_lib", "r1", "let f = fn(x) { let a = x.nope let b = x.other.width a }\n{f: f}")]);
    let context = MotionContext { modules, ..Default::default() };
    let src = "use mod.motion.*; let m = import(asset(\"words_lib\", \"r1\"))\nlet w = [] for i in 0..40 { w.push(m.f(i)) }\nMotion{duration: 1.0 layers: []}";
    let errors = MotionDoc::load_with(src, &[], context).unwrap_err();
    let messages: Vec<&str> = errors.iter().filter(|d| d.is_error()).map(|d| d.message.as_str()).collect();
    // `x.other.width` is one mistake: the VM reports the missing `other` and the
    // read of `width` on that already-reported error stays quiet, so the two
    // problems give two messages (`nope`, `other`), not three.
    assert_eq!(messages.len(), 2, "{messages:#?}");
    let mut unique = messages.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), messages.len(), "{messages:#?}");
}

/// Words other tools use for our fields are errors that name our field and
/// how its value is written, worded so a model can retry from them; they
/// are never read as the field. A plain misspelling keeps its nearest names.
#[test]
fn other_tools_words_are_errors_that_name_our_field() {
    let source = r#"use mod.motion.*; Motion{
    size: vec2(100, 100) duration: 2.0 frameRate: 30
    layers: [
        Rect{size: vec2(10, 10) fill: #fff position: vec2(50, 50)}
        Text{text: "Hi" fontSize: 24 fill: #fff}
        Rect{size: vec2(10, 10) fil: #fff}
    ]
}"#;
    let diags = errors(source);
    let at = has(&diags, "Rect has no `position`");
    assert_eq!(at.message, "Rect has no `position`; write `at` here: vec2(x, y) in px, the layer's origin (a box's top-left); or `x:` and `y:` alone");
    assert_eq!(at.loc.line, 4);
    assert!(at.hint.as_deref().unwrap().starts_with("Rect fields: "), "{at}");
    let size = has(&diags, "Text has no `fontSize`");
    assert_eq!(size.message, "Text has no `fontSize`; write `size` here: the font size in points");
    // A Text's own word wins over the shapes' (`fill` is its `color`).
    let color = has(&diags, "Text has no `fill`");
    assert_eq!(color.message, "Text has no `fill`; write `color` here: a colour (#rrggbb, #rrggbbaa) or an animation of one");
    let fps = has(&diags, "Motion has no `frameRate`");
    assert_eq!(fps.message, "Motion has no `frameRate`; write `fps` here: frames per second");
    assert!(fps.hint.as_deref().unwrap().starts_with("Motion fields: "), "{fps}");
    let fil = has(&diags, "property fil not defined on type Rect");
    assert!(fil.message.contains("Did you mean: fill"), "{fil}");
}
