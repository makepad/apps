use makepad_story_cut::*;

const LAUNCH: &str = include_str!("fixtures/launch.splash");
const ERRORS: &str = include_str!("fixtures/errors.splash");
const REFRAME: &str = include_str!("fixtures/reframe.splash");

const ASSET: &str = "ast_yqzlg4xa4mbgpzs6e2qsuqwhsu";
const REV: &str = "arev_5ab3f998fd4a7b6860a9337b9924be40ad341d12276e341079ce2093539c715f";

fn load(source: &str) -> CutDoc {
    CutDoc::load(source).unwrap_or_else(|diags| panic!("{diags:#?}"))
}

fn errors(source: &str) -> Vec<Diagnostic> {
    CutDoc::load(source).err().expect("the cut should not load")
}

/// A minimal edit around `body` (lets) listing `nodes`.
fn edit(body: &str, nodes: &str, duration: f64) -> String {
    format!(
        "use mod.flow.*\nuse mod.cut.*\n{body}\nFlow{{\n    kind: @cut\n    output: {{width: 1920 height: 1080 fps: 30}}\n    duration: {duration}\n    {nodes}\n}}\n"
    )
}

fn clip(name: &str, start: f64, length: f64) -> String {
    format!("let {name} = Clip{{src: asset(\"{ASSET}\", \"{REV}\") span: [{start}, {length}]}}\n")
}

#[test]
fn loads_the_spec_example() {
    let doc = load(LAUNCH);
    assert_eq!(doc.label, "Notes launch 9:16");
    assert_eq!(doc.output, OutputFormat { width: 1080, height: 1920, fps: 60.0, ..OutputFormat::default() });
    assert_eq!((doc.duration, doc.bpm), (17.0, 120.0));
    assert_eq!(doc.output_node, "out");
    let ids = doc.nodes.iter().map(|node| node.id.as_str()).collect::<Vec<_>>();
    assert_eq!(ids, ["intro", "demo", "wipe", "glow", "title", "top", "music", "voice", "out"]);
    assert!(doc.graph.nodes.iter().all(|node| node.type_name != nodes::METADATA), "the header carrier is not part of the graph");

    let intro = doc.node("intro").unwrap();
    assert_eq!(intro.span, Some(Span { start: 0.0, length: 6.0 }));
    assert_eq!(intro.loc.line, 5, "locations are in the cut's own source");
    let CutNodeKind::Clip { src: MediaRef::Asset(asset), source_in: SourceTime::Seconds(2.0), speed, fit: Fit::Fill, .. } = &intro.kind else { panic!("{:?}", intro.kind) };
    assert_eq!((asset.id.as_str(), *speed), ("ast_yqzlg4xa4mbgpzs6e2qsuqwhsu", 1.0));

    let demo = doc.node("demo").unwrap();
    let CutNodeKind::Clip { src: MediaRef::Run(run), source_in: SourceTime::Marker { name, offset }, .. } = &demo.kind else { panic!() };
    assert_eq!((name.as_str(), *offset), ("note_saved", -1.0));
    assert!(run.render.starts_with("arev_925b"));
    assert_eq!(run.render_id, "ast_rbzhbuglyvqk6nprgjwvl2o33q");

    let CutNodeKind::Transition { a, b, effect, mix: None, .. } = &doc.node("wipe").unwrap().kind else { panic!() };
    assert_eq!((a.as_ref().unwrap().node.as_str(), b.as_ref().unwrap().node.as_str(), effect.as_str()), ("intro", "demo", ""));

    let CutNodeKind::Fx { params, effect: EffectRef::Preset(effect), .. } = &doc.node("glow").unwrap().kind else { panic!() };
    assert_eq!(effect, "kit/bloom");
    let Animated::Keys(keys) = &params[0] else { panic!() };
    assert_eq!(keys.points, [KeyPoint::new(0.0, [0.2], Ease::Linear), KeyPoint::new(3.0, [0.8], Ease::EaseOut)]);

    let CutNodeKind::Title { text, anchor, .. } = &doc.node("title").unwrap().kind else { panic!() };
    assert_eq!((text.as_str(), *anchor), ("Notes, instantly", [0.5, 0.8]));

    let CutNodeKind::Stack { layers } = &doc.node("top").unwrap().kind else { panic!() };
    assert_eq!(layers.iter().map(|(wire, blend)| (wire.node.as_str(), *blend)).collect::<Vec<_>>(), [("glow", Blend::Normal), ("title", Blend::Normal)]);

    let CutNodeKind::Output { frame, audio } = &doc.node("out").unwrap().kind else { panic!() };
    assert_eq!(frame.as_ref().unwrap().node, "top");
    assert_eq!(audio.iter().map(|wire| wire.node.as_str()).collect::<Vec<_>>(), ["music", "voice"]);

    let markers = doc.markers();
    assert_eq!(markers.len(), 1);
    assert_eq!((markers[0].node.as_str(), markers[0].name.as_str()), ("demo", "note_saved"));
    assert!(doc.wiring().any(|edge| edge == ("wipe", "frame", "glow", "input")));
}

#[test]
fn round_trips_through_its_own_source() {
    let doc = load(LAUNCH);
    let again = load(&doc.source);
    assert_eq!(doc.nodes, again.nodes);
    // Formatting is not meaning: the same edit reflowed loads to the same
    // model apart from where things are written.
    let reflowed = LAUNCH.replace("let ", "\nlet ").replace("    ", "  ");
    let other = load(&reflowed);
    let strip = |doc: &CutDoc| doc.nodes.iter().map(|node| CutNode { loc: Default::default(), ..node.clone() }).collect::<Vec<_>>();
    assert_eq!(strip(&doc), strip(&other));
    assert_ne!(doc.node("intro").unwrap().loc, other.node("intro").unwrap().loc);
}

#[test]
fn dependencies_name_every_revision_once() {
    let deps = dependencies(&load(LAUNCH));
    let run = AssetRef { id: "ast_fklvc3bvjnuijdg33d2uuitkbi".into(), rev: "arev_e2e0b2ddc168547400e68147bf9e0161866f9cad405c9f94a2a47a5dcfe955ac".into() };
    assert_eq!(deps, [
        Dep::Asset(AssetRef { id: ASSET.into(), rev: REV.into() }),
        Dep::AppRun(run.clone()),
        Dep::Render { run, render: AssetRef { id: "ast_rbzhbuglyvqk6nprgjwvl2o33q".into(), rev: "arev_925b7b93f54e7cfa717f5f213568db1fdaea73ef483fc3ebbad2ea042a533bd1".into() } },
        Dep::Asset(AssetRef { id: "ast_qdyytgcolstqfb6rgnbpnwva3m".into(), rev: "arev_b07daf865cbe8c601af1644e4b1168662f5faad1224b8806370e61e13b2270c2".into() }),
    ]);
}

#[test]
fn diagnostics_carry_kinds_and_source_locations() {
    let diags = errors(ERRORS);
    let find = |kind: DiagnosticKind| diags.iter().find(|diag| diag.kind == kind).unwrap_or_else(|| panic!("no {kind:?} in {diags:#?}"));
    // The source's lines, 1-based, for checking the columns.
    let line = |n: u32| ERRORS.lines().nth(n as usize - 1).unwrap();

    let invalid = find(DiagnosticKind::InvalidRef);
    assert_eq!((invalid.node.as_deref(), invalid.line), (Some("a"), 4));
    assert!(line(4)[invalid.col as usize - 1..].starts_with("src:"));

    let span = find(DiagnosticKind::SpanOutOfRange);
    assert_eq!((span.node.as_deref(), span.line), (Some("b"), 5));
    assert!(line(5)[span.col as usize - 1..].starts_with("span:"));

    assert_eq!(find(DiagnosticKind::MissingRef).node.as_deref(), Some("c"));
    assert_eq!(find(DiagnosticKind::TransitionInputs).node.as_deref(), Some("fade"));

    let sorted = find(DiagnosticKind::KeysNotSorted);
    assert_eq!((sorted.node.as_deref(), sorted.line), (Some("glow"), 8));
    assert!(line(8)[sorted.col as usize - 1..].starts_with("p1:"));

    let ease = find(DiagnosticKind::UnknownEase);
    assert_eq!((ease.node.as_deref(), ease.line), (Some("wobble"), 9));
    assert_eq!(diags.len(), 6, "{diags:#?}");
}

#[test]
fn flow_errors_map_back_to_the_edit() {
    // An undeclared field is the flow evaluator's error: its line and
    // column are the cut's, not the prelude-prefixed source's.
    let source = edit(&format!("{}let b = Clip{{sorc: 1}}\nlet out = CutOutput{{frame: a.out(@frame)}}", clip("a", 0.0, 1.0)), "a, b, out", 2.0);
    let diags = errors(&source);
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].kind, DiagnosticKind::Eval);
    let line = source.lines().nth(diags[0].line as usize - 1).unwrap();
    assert!(line.starts_with("let b = Clip{sorc"), "{:?} -> {line:?}", diags[0]);
    assert!(diags[0].message.contains("sorc"));

    // An error on the Flow line itself (after the loader's wrap) keeps its column.
    let source = edit(&format!("{}let out = CutOutput{{frame: a.out(@frame)}}", clip("a", 0.0, 1.0)), "a, out, missing", 2.0);
    let diags = errors(&source);
    let line = source.lines().nth(diags[0].line as usize - 1).unwrap();
    assert!(line.contains("missing"), "{:?} -> {line:?}", diags[0]);
}

#[test]
fn header_is_checked() {
    let source = edit(&format!("{}let out = CutOutput{{frame: a.out(@frame)}}", clip("a", 0.0, 1.0)), "a, out", 2.0);
    let flow = source.replace("kind: @cut", "kind: @flow");
    let diags = errors(&flow);
    assert_eq!((diags.len(), diags[0].kind), (1, DiagnosticKind::Header));
    assert!(flow.lines().nth(diags[0].line as usize - 1).unwrap().contains("kind: @flow"));

    let diags = errors(&source.replace("output: {width: 1920 height: 1080 fps: 30}", "output: {width: 1920 fps: 30}"));
    assert_eq!(diags[0].kind, DiagnosticKind::Header);
    let diags = errors(&source.replace("duration: 2", "duration: 0"));
    assert_eq!(diags[0].kind, DiagnosticKind::Header);
    assert_eq!(load(&source).bpm, 120.0, "bpm defaults to 120");
}

#[test]
fn at_most_six_clips_play_at_once() {
    let names = ["c1", "c2", "c3", "c4", "c5", "c6", "c7"];
    let lets = |count: usize| names[..count].iter().enumerate().map(|(i, name)| clip(name, i as f64 * 0.1, 5.0)).collect::<String>();
    let body = |count: usize| format!("{}let out = CutOutput{{frame: c1.out(@frame)}}", lets(count));
    load(&edit(&body(6), "c1, c2, c3, c4, c5, c6, out", 10.0));
    let diags = errors(&edit(&body(7), "c1, c2, c3, c4, c5, c6, c7, out", 10.0));
    assert_eq!(diags.len(), 1);
    assert_eq!((diags[0].kind, diags[0].node.as_deref()), (DiagnosticKind::TooManyClips, Some("c7")));

    // Back-to-back spans are not concurrent: a span ends where the next starts.
    let chain = names.iter().enumerate().map(|(i, name)| clip(name, i as f64, 1.0)).collect::<String>()
        + &names[..5].iter().map(|name| clip(&format!("{name}b"), 0.0, 10.0)).collect::<String>()
        + "let out = CutOutput{frame: c1.out(@frame)}";
    load(&edit(&chain, "c1, c2, c3, c4, c5, c6, c7, c1b, c2b, c3b, c4b, c5b, out", 10.0));
}

#[test]
fn cycles_are_refused() {
    let body = "let a = Fx{input: wire(@b, @frame)}\nlet b = Fx{input: a.out(@frame)}\nlet out = CutOutput{frame: b.out(@frame)}";
    let diags = errors(&edit(body, "a, b, out", 2.0));
    assert!(matches!(diags[0].kind, DiagnosticKind::Eval | DiagnosticKind::Cycle), "{diags:#?}");
    assert!(diags[0].message.contains("cycle") || diags[0].message.contains("loops"), "{diags:#?}");
}

#[test]
fn params_are_checked() {
    let with = |extra: &str| edit(
        &format!("let a = Clip{{src: asset(\"{ASSET}\", \"{REV}\") span: [0.0, 1.0] {extra}}}\nlet out = CutOutput{{frame: a.out(@frame)}}"),
        "a, out", 2.0,
    );
    let diags = errors(&with("speed: keys([0.0, 1.0], [1.0, 2.0])"));
    assert_eq!((diags[0].kind, diags[0].message.contains("not animatable")), (DiagnosticKind::InvalidParam, true));
    assert_eq!(errors(&with("fit: @stretch"))[0].kind, DiagnosticKind::InvalidParam);
    assert_eq!(errors(&with("in: marker(@x, 0.0)"))[0].kind, DiagnosticKind::InvalidRef, "an asset has no markers");
    assert_eq!(errors(&with("opacity: keys()"))[0].kind, DiagnosticKind::InvalidParam);
    assert_eq!(errors(&with("span: [1.0]"))[0].kind, DiagnosticKind::SpanOutOfRange);
    let keyed = load(&with("opacity: keys([0.0, 0.0], [0.5, 1.0, @hold])"));
    let CutNodeKind::Clip { opacity: Animated::Keys(keys), .. } = &keyed.nodes[0].kind else { panic!() };
    assert_eq!(keys.points[1].ease, Ease::Hold);
}

#[test]
fn reframe_and_stack_params() {
    let doc = load(REFRAME);
    let CutNodeKind::Reframe { input, crop: Some(Animated::Keys(keys)), mode: ReframeMode::Keys, safe: SafeZone::Reels, aspect: None, focus: None } = &doc.node("tall").unwrap().kind else { panic!() };
    assert_eq!(input.as_ref().unwrap().node, "wide");
    assert_eq!(keys.points.len(), 3);
    assert_eq!(keys.points[2].v, [2000.0, 200.0, 1080.0, 1920.0]);

    let body = format!(
        "{}{}let s = Stack{{layers: [conn(7, b.out(@frame)), conn(3, a.out(@frame))] blend: [@normal, @screen]}}\nlet out = CutOutput{{frame: s.out(@frame)}}",
        clip("a", 0.0, 1.0), clip("b", 0.0, 1.0),
    );
    let doc = load(&edit(&body, "a, b, s, out", 2.0));
    let CutNodeKind::Stack { layers } = &doc.node("s").unwrap().kind else { panic!() };
    assert_eq!(layers.iter().map(|(wire, blend)| (wire.node.as_str(), *blend)).collect::<Vec<_>>(), [("b", Blend::Normal), ("a", Blend::Screen)]);
    assert!(doc.graph.edges.iter().any(|edge| edge.from_node == "b" && edge.conn == Some(7)), "authored entry ids are kept");
}

#[test]
fn registration_is_idempotent_and_marks_animatable_params() {
    use makepad_flow::document::Registry;
    let mut registry = Registry::with_builtins();
    let mut executors = makepad_flow::engine::providers::ExecutorRegistry::default();
    register(&mut registry, &mut executors).unwrap();
    register_document(&mut registry).unwrap();
    for (node, param) in [("Clip", "opacity"), ("Fx", "p4"), ("Transition", "mix"), ("Reframe", "crop"), ("AudioClip", "pan"), ("Title", "font_size")] {
        assert!(animatable(node, param));
        assert!(registry.descriptor(node).unwrap().params.iter().any(|p| p.name == param), "{node}.{param}");
    }
    assert!(!animatable("Clip", "speed"));
    assert!(PRELUDE.contains("mod.cut.keys"));
}

#[test]
fn title_is_the_text_node_and_its_anchor_is_not_the_canvas_position() {
    let body = "let t = Title{text: \"hi\" anchor: vec2(0.25, 0.75) font_size: keys([0.0, 40.0], [1.0, 60.0]) at: vec2(300, 200)}\nlet out = CutOutput{frame: t.out(@frame)}";
    let doc = load(&edit(body, "t, out", 2.0));
    let CutNodeKind::Title { text, anchor, font_size, .. } = &doc.node("t").unwrap().kind else { panic!() };
    assert_eq!((text.as_str(), *anchor, font_size.at1(0.5)), ("hi", [0.25, 0.75], 50.0));
    assert_eq!(doc.graph.nodes[0].at, Some((300.0, 200.0)), "`at` stays the canvas position");
    // Moving the node on the canvas changes nothing about the picture.
    let moved = load(&edit(&body.replace("vec2(300, 200)", "vec2(0.5, 0.5)"), "t, out", 2.0));
    let CutNodeKind::Title { anchor, .. } = &moved.node("t").unwrap().kind else { panic!() };
    assert_eq!(*anchor, [0.25, 0.75]);
    let plain = load(&edit(&body.replace(" anchor: vec2(0.25, 0.75)", ""), "t, out", 2.0));
    let CutNodeKind::Title { anchor, .. } = &plain.node("t").unwrap().kind else { panic!() };
    assert_eq!(*anchor, [0.5, 0.5], "no anchor centres it");
    // `Text{}` is the flow's text input, not a cut node.
    let diags = errors(&edit("let t = Text{}\nlet out = CutOutput{}", "t, out", 2.0));
    assert!(diags.iter().any(|diag| diag.kind == DiagnosticKind::Graph && diag.node.as_deref() == Some("t")), "{diags:#?}");
}

#[test]
fn stack_layer_ids_are_reported_and_never_collide() {
    let body = format!(
        "{}{}{}let s = Stack{{layers: [a.out(@frame), conn(1, b.out(@frame)), c.out(@frame)]}}\nlet out = CutOutput{{frame: s.out(@frame)}}",
        clip("a", 0.0, 1.0), clip("b", 0.0, 1.0), clip("c", 0.0, 1.0),
    );
    let doc = load(&edit(&body, "a, b, c, s, out", 2.0));
    assert_eq!(doc.synthetic_layers, [("s".to_string(), 2), ("s".to_string(), 3)]);
    let conns = doc.graph.edges.iter().filter(|edge| edge.to_node == "s").map(|edge| (edge.from_node.as_str(), edge.conn)).collect::<Vec<_>>();
    assert_eq!(conns, [("a", Some(2)), ("b", Some(1)), ("c", Some(3))]);
    let CutNodeKind::Stack { layers } = &doc.node("s").unwrap().kind else { panic!() };
    assert_eq!(layers.iter().map(|(wire, _)| wire.node.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
    assert!(load(LAUNCH).synthetic_layers.iter().all(|(stack, _)| stack == "top"));
}

#[test]
fn wiring_and_effect_diagnostics() {
    let a = clip("a", 0.0, 1.0);
    let kinds = |body: &str, nodes: &str| errors(&edit(body, nodes, 2.0)).iter().map(|diag| (diag.kind, diag.node.clone())).collect::<Vec<_>>();
    let node = |id: &str| Some(id.to_string());
    assert_eq!(kinds(&format!("{a}let f = Fx{{effect: \"bloom\"}}\nlet out = CutOutput{{frame: f.out(@frame)}}"), "a, f, out"), [(DiagnosticKind::Graph, node("f"))]);
    assert_eq!(kinds(&format!("{a}let f = Fx{{input: a.out(@frame)}}\nlet out = CutOutput{{frame: f.out(@frame)}}"), "a, f, out"), [(DiagnosticKind::InvalidParam, node("f"))]);
    assert_eq!(kinds(&format!("{a}let out = CutOutput{{}}"), "a, out"), [(DiagnosticKind::Graph, node("out"))]);
    assert_eq!(kinds(&a, "a"), [(DiagnosticKind::Graph, None)], "no CutOutput");
    assert_eq!(
        kinds(&format!("{a}let out = CutOutput{{frame: a.out(@frame)}}\nlet out2 = CutOutput{{frame: a.out(@frame)}}"), "a, out, out2"),
        [(DiagnosticKind::Graph, node("out2"))],
    );
    assert_eq!(
        kinds(&format!("{a}let s = Stack{{layers: [a.out(@frame)] blend: [@add, @screen]}}\nlet out = CutOutput{{frame: s.out(@frame)}}"), "a, s, out"),
        [(DiagnosticKind::InvalidParam, node("s"))],
    );
    // An @auto camera reframes exactly one clip.
    assert_eq!(
        kinds(&format!("{a}let f = Fx{{input: a.out(@frame) effect: \"bloom\"}}\nlet r = Reframe{{input: f.out(@frame)}}\nlet out = CutOutput{{frame: r.out(@frame)}}"), "a, f, r, out"),
        [(DiagnosticKind::Graph, node("r"))],
    );
    // A crop window has a size.
    assert_eq!(
        kinds(&format!("{a}let r = Reframe{{input: a.out(@frame) mode: @keys crop: [0.0, 0.0, 0.0, 0.0]}}\nlet out = CutOutput{{frame: r.out(@frame)}}"), "a, r, out"),
        [(DiagnosticKind::InvalidParam, node("r"))],
    );
}

#[test]
fn columns_count_characters_after_non_ascii_text() {
    let body = format!(
        "let t = Title{{text: \"Überschrift ✨\" span: [0.0, 9.0]}}\n{}let out = CutOutput{{frame: t.out(@frame)}}",
        clip("a", 0.0, 1.0),
    );
    let source = edit(&body, "t, a, out", 2.0);
    let diags = errors(&source);
    let line = source.lines().nth(diags[0].line as usize - 1).unwrap();
    assert!(line.chars().skip(diags[0].col as usize - 1).collect::<String>().starts_with("span:"), "{:?} in {line:?}", diags[0]);
    // FlowVm's own errors keep their line; their column is FlowVm's (for an
    // undeclared field it points near the object's end, see the report).
    let source = edit("let t = Title{text: \"Überschrift ✨\" sorc: 1}\nlet out = CutOutput{frame: t.out(@frame)}", "t, out", 2.0);
    let diags = errors(&source);
    assert!(source.lines().nth(diags[0].line as usize - 1).unwrap().contains("sorc"), "{:?}", diags[0]);
}

#[test]
fn dependencies_include_effects_and_focus_tracks() {
    let fx = "asset(\"ast_nflodx75nbi47ifviylxfujpdu\", \"arev_a3e5c7a236fb1386f50b89688763ff6b5c5a548bbd556639de9e81094ac9c852\")";
    let run = "run(\"ast_fklvc3bvjnuijdg33d2uuitkbi\", \"arev_e2e0b2ddc168547400e68147bf9e0161866f9cad405c9f94a2a47a5dcfe955ac\", \"ast_rbzhbuglyvqk6nprgjwvl2o33q\", \"arev_925b7b93f54e7cfa717f5f213568db1fdaea73ef483fc3ebbad2ea042a533bd1\")";
    let body = format!(
        "{}let f = Fx{{input: a.out(@frame) effect: {fx}}}\nlet r = Reframe{{input: a.out(@frame) focus: {run}}}\nlet s = Stack{{layers: [f.out(@frame), r.out(@frame)]}}\nlet out = CutOutput{{frame: s.out(@frame)}}",
        clip("a", 0.0, 1.0),
    );
    let deps = dependencies(&load(&edit(&body, "a, f, r, s, out", 2.0)));
    assert_eq!(deps.len(), 4, "{deps:#?}");
    assert!(matches!(&deps[1], Dep::Asset(asset) if asset.id == "ast_nflodx75nbi47ifviylxfujpdu"));
    assert!(matches!(&deps[2], Dep::AppRun(_)) && matches!(&deps[3], Dep::Render { .. }));
}

#[test]
fn runs_name_their_render_asset_and_keys_carry_pins() {
    // The three-argument `run()` of the first spec draft no longer loads.
    let three = LAUNCH.replacen("\"ast_rbzhbuglyvqk6nprgjwvl2o33q\", ", "", 1);
    let diags = errors(&three);
    assert!(diags.iter().any(|diag| diag.kind == DiagnosticKind::InvalidRef && diag.node.as_deref() == Some("demo")), "{diags:#?}");

    let with = |crop: &str| edit(
        &format!("{}let r = Reframe{{input: a.out(@frame) mode: @keys crop: {crop}}}\nlet out = CutOutput{{frame: r.out(@frame)}}", clip("a", 0.0, 2.0)),
        "a, r, out", 2.0,
    );
    let doc = load(&with("keys([0.0, [0.0, 0.0, 1080.0, 1920.0]], [0.5, [10.0, 0.0, 1080.0, 1920.0], @pin], [1.0, [20.0, 0.0, 1080.0, 1920.0], @hold, @pin], [1.5, [30.0, 0.0, 1080.0, 1920.0], @ease_in])"));
    let CutNodeKind::Reframe { crop: Some(Animated::Keys(keys)), .. } = &doc.node("r").unwrap().kind else { panic!() };
    assert_eq!(
        keys.points.iter().map(|point| (point.ease, point.pinned)).collect::<Vec<_>>(),
        [(Ease::Linear, false), (Ease::Linear, true), (Ease::Hold, true), (Ease::EaseIn, false)],
    );
    for bad in ["[0.5, [10.0, 0.0, 1080.0, 1920.0], @pin, @hold]", "[0.5, [10.0, 0.0, 1080.0, 1920.0], @pin, @pin]", "[0.5, [10.0, 0.0, 1080.0, 1920.0], @bounce, @pin]"] {
        let diags = errors(&with(&format!("keys([0.0, [0.0, 0.0, 1080.0, 1920.0]], {bad})")));
        assert_eq!(diags[0].kind, DiagnosticKind::UnknownEase, "{bad}");
    }
}

#[test]
fn output_colour_is_read_and_checked() {
    let with = |colour: &str| {
        let out = "let out = CutOutput{frame: a.out(@frame)}\n";
        edit(&format!("{}{out}", clip("a", 0.0, 1.0)), "a, out", 1.0).replace("fps: 30}", &format!("fps: 30 {colour}}}"))
    };
    assert_eq!(load(&with("")).output.matrix, OutputMatrix::Bt709);
    assert_eq!(load(&with("")).output.range, OutputRange::Limited);
    let doc = load(&with("matrix: @bt2020 range: @full"));
    assert_eq!((doc.output.matrix, doc.output.range), (OutputMatrix::Bt2020, OutputRange::Full));
    assert_eq!(load(&with("matrix: @bt601")).output.matrix, OutputMatrix::Bt601);
    let diags = errors(&with("matrix: @p3 range: 2"));
    assert!(diags.iter().any(|d| d.message.contains("output.matrix")), "{diags:#?}");
    assert!(diags.iter().any(|d| d.message.contains("output.range")), "{diags:#?}");
}
