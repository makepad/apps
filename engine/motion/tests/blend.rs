//! Value-driven forms stay pure in time and in each instance's inputs.
//! These are evaluation tests; the lane lead runs them with the native suite.

use makepad_motion::*;
use makepad_motion::character::{self, pose::{self, Pose}};

fn document(layers: &str) -> String {
    format!("use mod.motion.*; Motion{{duration: 8 bpm: 120 layers: [{layers}]}}")
}

fn load(layers: &str) -> MotionDoc {
    MotionDoc::load(&document(layers)).unwrap_or_else(|errors| panic!("{errors:#?}\n{layers}"))
}

fn scalar(form: &str) -> MotionDoc {
    load(&format!("Rect{{size: vec2(20, 20) fill: #fff x: {form}}}"))
}

fn frame_x(frame: &MotionFrame) -> f64 {
    assert!(frame.errors.is_empty(), "{:?}", frame.errors);
    frame.items[0].world.e
}

fn near(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-9, "{a} != {b}");
}

#[test]
fn stops_interpolate_piecewise_and_clamp() {
    let doc = scalar("blend(fn(t) {t}, {at: [0, 0.5, 2] values: [10, 30, 60]})");
    for (t, expected) in [(-2.0, 10.0), (0.0, 10.0), (0.25, 20.0), (0.5, 30.0), (1.0, 40.0), (2.0, 60.0), (9.0, 60.0)] {
        near(frame_x(&doc.eval(t)), expected);
    }
    let list = scalar("blend(fn(t) {t}, [10, 30, 60])");
    for (t, expected) in [(0.25, 20.0), (0.5, 30.0), (0.75, 45.0), (3.0, 60.0)] {
        near(frame_x(&list.eval(t)), expected);
    }
    for (form, expected) in [("blend(-10, [7])", 7.0), ("blend(99, {at: [2] values: [7]})", 7.0), ("blend(0.25, [10, 30])", 15.0)] {
        let doc = scalar(form);
        assert_eq!(doc.layers[0].transform.x.constant(), Some([expected]));
        assert!(!doc.layers[0].animated);
        for t in [0.0, 0.4, 2.0] { near(frame_x(&doc.eval(t)), expected); }
    }
}

#[test]
fn easing_is_per_segment_and_exact_stops_stay_exact() {
    let doc = scalar("blend(fn(t) {t}, {at: [0, 0.5, 1] values: [0, 100, 300]}, ease: @ease_in_quad)");
    near(frame_x(&doc.eval(0.25)), 25.0);
    near(frame_x(&doc.eval(0.75)), 150.0);
    near(frame_x(&doc.eval(0.5)), 100.0);
    near(frame_x(&doc.eval(2.0)), 300.0);
    let spring = scalar("blend(0.25, [0, 100], spring(170, 26))");
    near(frame_x(&spring.eval(0.0)), 100.0 * Ease::Spring { k: 170.0, d: 26.0 }.apply(0.25, 1.0));
}

#[test]
fn vectors_colours_and_angles_use_the_fields_value_kinds() {
    let doc = load("Rect{size: blend(0.25, [vec2(20, 40), vec2(60, 80)]) fill: blend(0.5, [#ff000080, #0000ff]) rotate: blend(0.5, [350, 10])}");
    let frame = doc.eval(0.7);
    assert!(frame.errors.is_empty(), "{:?}", frame.errors);
    let ItemKind::Rect { size, fill: Some(fill), .. } = frame.items[0].kind else { panic!() };
    assert_eq!(size, [30.0, 50.0]);
    assert_eq!(&fill[..3], &[0.5, 0.0, 0.5]);
    near(fill[3], (128.0 / 255.0 + 1.0) * 0.5);
    near(frame.items[0].local.rotate, 180.0);

    let doc = load(r#"Scene3D{model: asset("fixture", "v1") camera: {
        position: blend(0.5, [vec3(0, 1, 4), vec3(2, 3, 8)]) target: vec3(0, 0, 0)
    }}"#);
    let frame = doc.eval(0.0);
    assert!(frame.errors.is_empty(), "{:?}", frame.errors);
    let ItemKind::Scene3D { camera, .. } = &frame.items[0].kind else { panic!() };
    assert_eq!(camera.eye, [1.0, 2.0, 6.0]);
}

#[test]
fn keyed_controls_keyable_values_and_nested_blends_share_layer_time() {
    for form in [
        "blend(keys([0, 0], [2, 1]), {at: [0, 1] values: [tween(10, 30, 0, 2), blend(0.5, [30, 50])]})",
        "blend(keys([0, 0], [2, 1]), [tween(10, 30, 0, 2), blend(0.5, {at: [0, 1] values: [30, 50]})])",
    ] {
        let doc = scalar(form);
        for (t, expected) in [(0.0, 10.0), (1.0, 30.0), (2.0, 40.0)] {
            near(frame_x(&doc.eval(t)), expected);
        }
        let reference = doc.eval(1.0);
        for t in [7.0, -1.0, 1.8, 0.2] { doc.eval(t); }
        assert_eq!(doc.eval(1.0), reference);
        assert!(doc.layers[0].animated);
    }

    let doc = load("Rect{size: vec2(20, 20) fill: #fff span: [1, 3] x: blend(keys([beat(2), 0], [beat(4), 1]), [tween(0, 20, beat(2), beat(4)), 40])}");
    assert!(uses_musical_time(&doc));
    // The absolute beat keys at 1 and 2 seconds become local 0 and 1.
    for (t, expected) in [(1.0, 0.0), (1.5, 25.0), (2.0, 40.0)] {
        near(frame_x(&doc.eval(t)), expected);
    }
    // Repeat and sequence remain ordinary keyable controls.
    let doc = scalar("blend(repeat(sequence([[0, 0], [1, 1]]), 1, @pingpong), [0, 100])");
    near(frame_x(&doc.eval(0.25)), 25.0);
    near(frame_x(&doc.eval(1.25)), 75.0);
}

#[test]
fn grid_is_bilinear_at_corners_centre_and_nonuniform_cells() {
    for (x, y, expected) in [(0.0, 0.0, 0.0), (2.0, 0.0, 20.0), (0.0, 4.0, 40.0), (2.0, 4.0, 100.0), (1.0, 2.0, 40.0), (0.5, 1.0, 17.5), (-1.0, 2.0, 20.0), (3.0, 9.0, 100.0)] {
        let doc = scalar(&format!("blend2({x}, {y}, {{cols: [0, 2] rows: [0, 4] values: [[0, 20], [40, 100]]}})"));
        near(frame_x(&doc.eval(0.0)), expected);
    }
    let doc = scalar("blend2(2, 1, {cols: [0, 1, 3] rows: [0, 2] values: [[0, 10, 50], [100, 110, 150]]})");
    near(frame_x(&doc.eval(0.0)), 80.0);
    let doc = scalar("blend2(0.5, 0.5, {cols: [0, 1] rows: [0, 1] values: [[0, 100], [100, 200]]}, ease: @ease_in_quad)");
    near(frame_x(&doc.eval(0.0)), 50.0);
    let doc = scalar("blend2(99, 0.25, {cols: [2] rows: [0, 1] values: [[10], [50]]})");
    near(frame_x(&doc.eval(0.0)), 20.0);
}

#[test]
fn both_grid_controls_and_grid_values_can_be_keyed() {
    let doc = scalar("blend2(tween(0, 1, 0, 2), keys([0, 0], [2, 1]), {cols: [0, 1] rows: [0, 1] values: [[tween(0, 8, 0, 2), 20], [40, 80]]})");
    near(frame_x(&doc.eval(1.0)), 36.0);
    near(frame_x(&doc.eval(0.0)), 0.0);
    near(frame_x(&doc.eval(2.0)), 80.0);
}

#[test]
fn post_uniforms_keep_blended_colours_vectors_and_musical_controls() {
    let doc = MotionDoc::load(r#"use mod.motion.*; use mod.post
        Motion{duration: 4 bpm: 120 layers: [] post: [post.Pass{
            at: @final uniforms: {
                colour: blend(0.5, {at: [0, 1] values: [fn(t) {#f00}, #00f]})
                offset: blend(tween(0, 1, beat(0), beat(4)), {values: [vec2(0, 2), vec2(4, 6)] at: [0, 1]})
                axis: blend(0.25, {at: [0, 1] values: [vec3(0, 4, 8), vec3(4, 8, 12)]})
            }
            pixel: fn() -> vec4 { return self.colour }
        }]}
    "#).unwrap();
    let frame = doc.eval(1.0);
    assert!(frame.errors.is_empty(), "{:?}", frame.errors);
    let (decls, values) = frame.post.unwrap();
    for (name, width, expected) in [("colour", 4, [0.5, 0.0, 0.5, 1.0]), ("offset", 2, [2.0, 4.0, 0.0, 0.0]), ("axis", 3, [1.0, 5.0, 9.0, 0.0])] {
        let i = decls[0].uniforms.iter().position(|u| u.name == name).unwrap();
        assert_eq!(decls[0].uniforms[i].width, width);
        assert_eq!(values[0][i][..4], expected);
    }
}

const INPUT_DOC: &str = r#"use mod.motion.*
Motion{duration: 4 inputs: {pointer_x: 0 pointer_y: 0 mood: 0 accent: #ff0000}
    layers: [Rect{size: vec2(20, 20)
        x: blend2(fn(t) { in(@pointer_x) }, fn(t) { in(@pointer_y) }, {cols: [0, 1] rows: [0, 1] values: [[0, 20], [40, 100]]})
        y: blend(fn(t) { in(@mood) }, {at: [0, 1] values: [10, 30]})
        fill: blend(0.5, {at: [0, 1] values: [fn(t) { in(@accent) }, #0000ff]})
    }]
}"#;

fn set(doc: &MotionDoc, inputs: &mut InstanceInputs, name: &str, value: InputValue) {
    let index = doc.input_index(input_id(name)).unwrap();
    assert!(!doc.structural[index], "{name} is read by the blend at frame time");
    assert!(matches!(doc.write_input(inputs, index, value), Ok(InputWrite::Applied | InputWrite::Unchanged)));
}

fn input_blends(doc: MotionDoc) {
    let loads = doc.evaluations();
    let (mut a, mut b) = (doc.new_inputs(), doc.new_inputs());
    set(&doc, &mut a, "pointer_x", InputValue::Number(0.5));
    set(&doc, &mut a, "pointer_y", InputValue::Number(0.5));
    set(&doc, &mut a, "mood", InputValue::Number(0.25));
    set(&doc, &mut a, "accent", InputValue::Color([0.0, 1.0, 0.0, 1.0]));
    set(&doc, &mut b, "pointer_x", InputValue::Number(1.0));
    for _ in 0..3 {
        let frame = doc.eval_with(1.0, &mut a);
        near(frame_x(&frame), 40.0);
        near(frame.items[0].world.f, 15.0);
        let ItemKind::Rect { fill: Some(fill), .. } = frame.items[0].kind else { panic!() };
        assert_eq!(fill, [0.0, 0.5, 0.5, 1.0]);
        let other = doc.eval_with(1.0, &mut b);
        near(frame_x(&other), 20.0);
        near(other.items[0].world.f, 10.0);
        let ItemKind::Rect { fill: Some(other_fill), .. } = other.items[0].kind else { panic!() };
        assert_eq!(other_fill, [0.5, 0.0, 0.5, 1.0]);
        assert_ne!(frame_x(&frame), frame_x(&other), "the functions read each instance's inputs");
        near(frame_x(&doc.eval(1.0)), 0.0);
    }
    assert_eq!(doc.evaluations(), loads, "instance input writes never reload the document");
}

#[test]
fn instance_inputs_drive_both_forms_through_functions_without_reloading() {
    input_blends(MotionDoc::load(INPUT_DOC).unwrap());
    // Functions read the lent inputs even when params were given at load.
    let params = [(input_id("pointer_x"), InputValue::Number(0.0))];
    input_blends(MotionDoc::load_with(INPUT_DOC, &params, MotionContext::default()).unwrap());
}

#[test]
fn the_same_document_can_use_a_tween_as_its_x_control() {
    let source = INPUT_DOC.replace("fn(t) { in(@pointer_x) }", "tween(0, 1, 0, 2)");
    let doc = MotionDoc::load(&source).unwrap();
    let loads = doc.evaluations();
    let (mut a, mut b) = (doc.new_inputs(), doc.new_inputs());
    set(&doc, &mut a, "pointer_y", InputValue::Number(0.5));
    set(&doc, &mut b, "pointer_y", InputValue::Number(1.0));
    for (t, ax, bx) in [(0.0, 20.0, 40.0), (1.0, 40.0, 70.0), (2.0, 60.0, 100.0), (0.0, 20.0, 40.0)] {
        near(frame_x(&doc.eval_with(t, &mut a)), ax);
        near(frame_x(&doc.eval_with(t, &mut b)), bx);
    }
    assert_eq!(doc.evaluations(), loads);
}

#[test]
fn the_same_document_can_use_a_constant_as_its_x_control() {
    let source = INPUT_DOC.replace("fn(t) { in(@pointer_x) }", "0.25");
    let doc = MotionDoc::load(&source).unwrap();
    let loads = doc.evaluations();
    let (mut a, mut b) = (doc.new_inputs(), doc.new_inputs());
    set(&doc, &mut a, "pointer_x", InputValue::Number(1.0));
    set(&doc, &mut a, "pointer_y", InputValue::Number(0.5));
    set(&doc, &mut b, "pointer_y", InputValue::Number(1.0));
    for t in [0.0, 1.0, 2.0, 0.0] {
        near(frame_x(&doc.eval_with(t, &mut a)), 30.0);
        near(frame_x(&doc.eval_with(t, &mut b)), 55.0);
        near(frame_x(&doc.eval(t)), 5.0);
    }
    assert_eq!(doc.evaluations(), loads);
}

#[test]
fn adopted_document_blend_functions_read_instance_inputs() {
    use makepad_script::{ScriptMod, ScriptVm, ScriptVmBase, ScriptVmHost};
    let mut host = ScriptVmHost::new(0i32, ());
    let mut vm = ScriptVm { host: host.host_context(), bx: Box::new(ScriptVmBase::new()) };
    let source = format!("|| {{ {INPUT_DOC} }}");
    vm.capture_errors();
    let function = vm.eval(ScriptMod::new("", "", "game.splash", 4, 3, &source, &[]));
    assert!(vm.take_errors().is_empty());
    let image = vm.fn_image(function.as_object().unwrap()).unwrap();
    input_blends(MotionDoc::load_image(&image, &[], MotionContext::default()).unwrap());
}

#[test]
fn other_reads_of_a_blended_input_keep_their_structural_meaning() {
    let doc = MotionDoc::load("use mod.motion.*; Motion{duration: 2 inputs: {mood: 0.25} layers: [Rect{size: vec2(20, 20) fill: #fff x: blend(fn(t) { in(@mood) }, [0, 100]) y: in(@mood) * 10}]}").unwrap();
    let index = doc.input_index(input_id("mood")).unwrap();
    assert!(doc.structural[index], "the separate arithmetic expression still needs reload");
    let mut inputs = doc.new_inputs();
    assert_eq!(doc.write_input(&mut inputs, index, InputValue::Number(0.75)), Ok(InputWrite::Reevaluate));
    let frame = doc.eval_with(0.0, &mut inputs);
    near(frame_x(&frame), 75.0);
    near(frame.items[0].world.f, 2.5);
}

#[test]
fn eager_input_controls_and_arithmetic_hold_their_load_time_values() {
    let source = "use mod.motion.*; Motion{duration: 2 inputs: {mood: 0.25} layers: [Rect{size: vec2(20, 20) fill: #fff x: blend(in(@mood), [0, 100]) y: in(@mood) * 10 rotate: blend2(in(@mood), in(@mood), {cols: [0, 1] rows: [0, 1] values: [[0, 20], [40, 100]]})}]}";
    for initial in [0.25, 0.5] {
        let params = [(input_id("mood"), InputValue::Number(initial))];
        let doc = if initial == 0.25 { MotionDoc::load(source).unwrap() } else { MotionDoc::load_with(source, &params, MotionContext::default()).unwrap() };
        assert!(!doc.layers[0].animated);
        assert_eq!(doc.layers[0].transform.x.constant(), Some([initial * 100.0]));
        let index = doc.input_index(input_id("mood")).unwrap();
        assert!(doc.structural[index], "eager in() reads require reevaluation");
        let loads = doc.evaluations();
        let mut inputs = doc.new_inputs();
        assert_eq!(doc.write_input(&mut inputs, index, InputValue::Number(0.75)), Ok(InputWrite::Reevaluate));
        for t in [0.0, 1.0] {
            let frame = doc.eval_with(t, &mut inputs);
            near(frame_x(&frame), initial * 100.0);
            near(frame.items[0].world.f, initial * 10.0);
            near(frame.items[0].local.rotate, 60.0 * initial + 40.0 * initial * initial);
        }
        assert_eq!(doc.evaluations(), loads, "frame sampling does not reevaluate eager reads");
        let changed = doc.reevaluate(&inputs).unwrap();
        let frame = changed.eval_with(0.0, &mut inputs);
        near(frame_x(&frame), 75.0);
        near(frame.items[0].world.f, 7.5);
        near(frame.items[0].local.rotate, 67.5);
    }
}

fn character_doc(form: &str) -> MotionDoc {
    MotionDoc::load(&format!("use mod.motion.*; Motion{{duration: 4 inputs: {{mood: 0}} layers: [Character{{model: @spark at: vec2(0, 0) height: 400 blink: 0 sway: 0 pose: {form}}}]}}")).unwrap_or_else(|errors| panic!("{errors:#?}"))
}

fn character_matches(doc: &MotionDoc, frame: &MotionFrame, expected: Pose) {
    assert!(frame.errors.is_empty(), "{:?}", frame.errors);
    let LayerKind::Character(ch) = &doc.layers[0].kind else { panic!() };
    let rig = ch.rig().unwrap();
    let beat = |t: f64| t * doc.bpm / 60.0;
    let moment = character::Moment { t: frame.t, beat: &beat, voice: None, host: 0.0, fps: doc.fps, parts: &[], reach: &[], pose: Some(expected), instance: 0, arms: 0.0, hop: 0.0, nod: 0.0 };
    let solved = character::solve(ch, rig, &moment);
    let children = character::items(rig, &solved, &doc.layers[0].path, &Affine::IDENTITY);
    let ItemKind::Group { children: actual, .. } = &frame.items[0].kind else { panic!() };
    assert_eq!(actual, &children, "joint transforms, visibility and draw order use the mixed pose");
}

#[test]
fn character_poses_mix_from_instance_mood_and_bilinear_controls() {
    let (idle, cheer) = (pose::preset("idle").unwrap(), pose::preset("cheer").unwrap());
    for form in [
        "blend(fn(t) { in(@mood) }, [@idle, @cheer])",
        "blend(fn(t) { in(@mood) }, {at: [0, 1] values: [@idle, @cheer]})",
    ] {
        let doc = character_doc(form);
        let mut inputs = doc.new_inputs();
        for mood in [0.0, 0.25, 1.0, -1.0, 2.0] {
            set(&doc, &mut inputs, "mood", InputValue::Number(mood));
            character_matches(&doc, &doc.eval_with(0.5, &mut inputs), idle.mix(&cheer, mood));
        }
    }
    let (mic, prop) = (pose::preset("on_mic").unwrap(), pose::preset("hold_prop").unwrap());
    let doc = character_doc("blend(fn(t) { in(@mood) }, {at: [0, 0.25, 1] values: [@idle, @cheer, @hold_prop]})");
    let mut inputs = doc.new_inputs();
    for (mood, expected) in [(-1.0, idle), (0.125, idle.mix(&cheer, 0.5)), (0.25, cheer), (0.625, cheer.mix(&prop, 0.5)), (2.0, prop)] {
        set(&doc, &mut inputs, "mood", InputValue::Number(mood));
        character_matches(&doc, &doc.eval_with(0.5, &mut inputs), expected);
    }
    let doc = character_doc("blend2(fn(t) { in(@mood) }, tween(0, 1, 0, 2), {cols: [0, 1] rows: [0, 1] values: [[@idle, @cheer], [@on_mic, @hold_prop]]})");
    let mut inputs = doc.new_inputs();
    for (x, t) in [(0.0, 0.0), (1.0, 0.0), (0.0, 2.0), (1.0, 2.0), (0.5, 1.0)] {
        set(&doc, &mut inputs, "mood", InputValue::Number(x));
        character_matches(&doc, &doc.eval_with(t, &mut inputs), idle.mix(&cheer, x).mix(&mic.mix(&prop, x), t * 0.5));
    }
}

#[test]
fn only_the_surrounding_values_are_sampled() {
    // The erroring property function is unused at this exact stop.
    for form in ["blend(0, [12, fn(t) { nil }])", "blend(0, {at: [0, 1] values: [12, fn(t) { nil }]})"] {
        let doc = scalar(form);
        near(frame_x(&doc.eval(0.0)), 12.0);
    }
    let doc = scalar("blend2(1, 1, {cols: [0, 1] rows: [0, 1] values: [[fn(t) {nil}, fn(t) {nil}], [fn(t) {nil}, 7]]})");
    near(frame_x(&doc.eval(0.0)), 7.0);
}

#[test]
fn property_functions_cannot_hide_mismatched_kinds() {
    for form in ["blend(fn(t) {vec2(0, 1)}, [0, 1])", "blend(0.5, [0, fn(t) {vec2(1, 2)}])"] {
        let frame = scalar(form).eval(0.0);
        assert!(frame.errors.iter().any(|error| error.message.contains("mismatched blend value kinds")), "{:#?}", frame.errors);
    }
}

#[test]
fn invalid_forms_name_the_field_and_the_problem() {
    for (field, form, message) in [
        ("x", "blend(0, [])", "empty blend stop list"),
        ("x", "blend(0, {at: [] values: []})", "empty blend stop list"),
        ("x", "blend(0, {at: [0, 1] values: [0]})", "`at` and `values` lengths differ"),
        ("x", "blend(0, {at: [0] values: [0, 1]})", "`at` and `values` lengths differ"),
        ("x", "blend(0, {at: [0] values: []})", "`at` and `values` lengths differ"),
        ("x", "blend(0, {at: [1, 0] values: [0, 1]})", "`x.at` blend stops must be strictly increasing"),
        ("x", "blend(0, {at: [0, 0] values: [1, 2]})", "`x.at` blend stops must be strictly increasing"),
        ("x", "blend(0, {})", "`x.at` is a nonempty list"),
        ("x", "blend(0, {values: [0]})", "`x.at` is a nonempty list"),
        ("x", "blend(0, {at: 0 values: [0]})", "`x.at` is a nonempty list"),
        ("x", "blend(0, {at: [0]})", "`x.values` is a nonempty array"),
        ("x", "blend(0, {at: [0] values: 0})", "`x.values` is a nonempty array"),
        ("x", "blend(0, {at: [tween(0, 1, 0, 1)] values: [0]})", "finite constant numbers"),
        ("x", "blend(0, 1)", "blend stops are [a, b] or {at:"),
        ("size", "blend(0, [1, vec2(2, 3)])", "mismatched blend value kinds"),
        ("size", "blend(0, {at: [0, 1] values: [1, vec2(2, 3)]})", "mismatched blend value kinds"),
        ("fill", "blend(0, [#fff, 1])", "mismatched blend value kinds"),
        ("fill", "blend(0, {at: [0, 1] values: [#fff, 1]})", "mismatched blend value kinds"),
        ("x", "blend(vec2(0, 1), [0, 1])", "mismatched blend value kinds"),
        ("x", "blend2(0, 0, {cols: [0, 0] rows: [0] values: [[0, 1]]})", "strictly increasing"),
        ("x", "blend2(0, 0, {cols: [0] rows: [] values: []})", "empty blend stop list"),
        ("x", "blend2(0, 0, {cols: [0, 1] rows: [0, 1] values: [[0, 1], [2]]})", "ragged blend2 grid"),
        ("x", "blend2(0, 0, {cols: [0] rows: [0, 1] values: [[0]]})", "ragged blend2 grid"),
        ("x", "blend2(0, 0, {cols: [tween(0, 1, 0, 1)] rows: [0] values: [[0]]})", "finite constant numbers"),
        ("x", "blend2(0, 0, {cols: [0, 1] rows: [0] values: [[0, #fff]]})", "mismatched blend value kinds"),
    ] {
        let source = document(&format!("Rect{{{field}: {form}}}"));
        let errors = MotionDoc::load(&source).expect_err(&source);
        assert!(errors.iter().any(|e| e.is_error() && e.path.contains(field) && e.message.contains(message)), "{source}\n{errors:#?}");
    }
    for (form, message) in [
        ("blend(0.5, [@idle, @chere])", "no pose @chere"),
        ("blend(0.5, {at: [0, 1] values: [@idle, @chere]})", "no pose @chere"),
        ("blend(0.5, {at: [] values: []})", "empty blend stop list"),
        ("blend(0.5, {at: [0, 1] values: [@idle]})", "`at` and `values` lengths differ"),
        ("blend(0.5, {at: [1, 0] values: [@idle, @cheer]})", "`pose.at` blend stops must be strictly increasing"),
        ("blend(0.5, [@idle, tween(0, 1, 0, 1)])", "mismatched blend value kinds"),
        ("blend(0.5, [fn(t) {@idle}, fn(t) {@cheer}])", "constant named poses"),
    ] {
        let errors = MotionDoc::load(&document(&format!("Character{{model: @spark pose: {form}}}"))).unwrap_err();
        assert!(errors.iter().any(|e| e.path.contains("pose") && e.message.contains(message)), "{errors:#?}");
    }
    let errors = MotionDoc::load(&document("Rect{x: blend(in(@missing), [0, 1])}")).unwrap_err();
    assert!(errors.iter().any(|e| e.path.contains("x") && e.message.contains("names no input")), "{errors:#?}");
}
