//! Generative segments: the Generate node's loading, its
//! plan, keys and staleness, and the helper edits the host renders.

use makepad_story_cut::generate::*;
use makepad_story_cut::*;

const FIXTURE: &str = include_str!("fixtures/generate.splash");

fn source(key: &str) -> String {
    FIXTURE.replace("KEY", key)
}

fn load(source: &str) -> CutDoc {
    CutDoc::load(source).unwrap_or_else(|diags| panic!("{diags:#?}"))
}

fn diagnostics(source: &str) -> Vec<Diagnostic> {
    CutDoc::load(source).err().unwrap_or_default()
}

/// The fixture with the first take's key set to what the node makes now.
fn keyed() -> String {
    let doc = load(&source("nil"));
    let eval = CutEval::new(&doc);
    let gen = doc.generate("dream").unwrap();
    let key = generate_key(&eval, gen, 7, "minimax-h3-q4-24g");
    source(&format!("\"{key}\""))
}

#[test]
fn a_generate_loads_beside_the_other_nodes() {
    let doc = load(&source("nil"));
    assert!(doc.node("dream").is_none(), "Generate is not an CutNodeKind");
    let gen = doc.generate("dream").unwrap();
    assert_eq!(gen.span, Span { start: 4.0, length: 4.0 });
    assert_eq!(gen.input, Some(Wire { node: "glow".into(), port: "frame".into() }));
    assert_eq!(gen.preset, "bundled:image-to-video");
    assert_eq!((gen.prompt.as_str(), gen.negative.as_str(), gen.seed), ("the hands become birds", "text", 7));
    assert_eq!(gen.number("frames"), Some(124.0));
    assert_eq!(gen.params.iter().map(|(key, _)| key.as_str()).collect::<Vec<_>>(), ["frames", "steps"]);
    assert_eq!(gen.takes.len(), 2);
    assert_eq!(gen.takes[0].seconds, Some(5.0));
    assert_eq!(gen.takes[1].key, None);
    assert_eq!(gen.frame_at, None);
    // Both takes are pinned.
    let deps = dependencies(&doc);
    for take in &gen.takes {
        assert!(deps.contains(&Dep::Asset(take.video.clone())), "{deps:?}");
    }
    assert_eq!(generate_media(&doc), vec![MediaRef::Asset(gen.takes[0].video.clone())]);
    // The graph has it, wired both ways.
    assert!(doc.wiring().any(|(from, _, to, port)| from == "glow" && to == "dream" && port == "input"));
    assert!(doc.wiring().any(|(from, _, to, _)| from == "dream" && to == "top"));
}

#[test]
fn the_take_plays_stretched_over_the_span_and_the_input_elsewhere() {
    let doc = load(&source("nil"));
    let eval = CutEval::new(&doc);
    let take = doc.generate("dream").unwrap().takes[0].video.clone();
    // Inside: the take, 5 s over the 4 s span.
    let plan = eval.frame_plan(6.0);
    let op = plan.ops.iter().find(|op| op.node == "dream").expect("the take is drawn");
    let DrawKind::Source { media, source_time, source_ticks, still, .. } = &op.kind else { panic!("{op:?}") };
    assert_eq!(media, &MediaRef::Asset(take));
    assert!(!still);
    assert!((source_time.unwrap() - 2.5).abs() < 1e-9);
    assert_eq!(*source_ticks, Some(25_000_000));
    assert!(plan.ops.iter().all(|op| op.node != "glow"), "the input is not drawn under a take");
    // Outside: the input's chain.
    let plan = eval.frame_plan(2.0);
    assert!(plan.ops.iter().any(|op| op.node == "glow"));
    assert!(plan.ops.iter().all(|op| op.node != "dream"));
    // Without takes: the input, also inside.
    let start = FIXTURE.find("takes: [").unwrap();
    let end = start + FIXTURE[start..].find("]\n    take: 0").unwrap() + 1;
    let bare = load(&format!("{}takes: []{}", &FIXTURE[..start], &FIXTURE[end..]));
    assert!(bare.generate("dream").unwrap().takes.is_empty());
    let eval = CutEval::new(&bare);
    assert!(eval.frame_plan(6.0).ops.iter().any(|op| op.node == "glow"));
    assert_eq!(generate_state(&eval, bare.generate("dream").unwrap()), GenState::Missing);
}

#[test]
fn staleness_follows_the_input_the_settings_and_the_seed() {
    let fresh = keyed();
    let doc = load(&fresh);
    let eval = CutEval::new(&doc);
    assert_eq!(generate_state(&eval, doc.generate("dream").unwrap()), GenState::Generated);

    let state = |source: String| {
        let doc = load(&source);
        let eval = CutEval::new(&doc);
        generate_state(&eval, doc.generate("dream").unwrap())
    };
    // The prompt.
    assert_eq!(state(fresh.replace("the hands become birds", "the hands become fish")), GenState::Stale(vec![Staleness::Settings]));
    // A param.
    assert_eq!(state(fresh.replace("steps: 30", "steps: 40")), GenState::Stale(vec![Staleness::Settings]));
    // The input's source time, and the effect's keys.
    assert_eq!(state(fresh.replace("in: 2.0", "in: 2.5")), GenState::Stale(vec![Staleness::Input]));
    assert_eq!(state(fresh.replace("[3.0, 0.8, @ease_out]", "[3.0, 0.9, @ease_out]")), GenState::Stale(vec![Staleness::Input]));
    // A seed no take has.
    assert_eq!(state(fresh.replace("seed: 7", "seed: 99")), GenState::Stale(vec![Staleness::Seed]));
    // The other take was written by hand without a key.
    assert_eq!(state(fresh.replace("take: 0", "take: 1")), GenState::Stale(vec![Staleness::Unkeyed]));
    // What the input does outside the span does not matter.
    assert_eq!(state(fresh.replace("text: \"Birds\"", "text: \"Fish\"")), GenState::Generated);
    // Renaming nodes does not either.
    assert_eq!(state(fresh.replace("glow", "shine")), GenState::Generated);
}

#[test]
fn image_to_video_keys_one_frame() {
    let doc = load(&keyed().replace("seed: 7 params", "frame_at: 5.0 seed: 7 params"));
    let gen = doc.generate("dream").unwrap();
    assert_eq!(gen.frame_at, Some(5.0));
    assert_eq!(key_times(gen, 25.0), vec![5.0]);
    let eval = CutEval::new(&doc);
    // One frame instead of the span: the input and the request changed.
    assert_eq!(generate_state(&eval, gen), GenState::Stale(vec![Staleness::Input, Staleness::Settings]));
    assert_eq!(key_times(load(&keyed()).generate("dream").unwrap(), 25.0).len(), 100);
}

#[test]
fn next_seeds_skip_the_ones_taken() {
    let doc = load(&source("nil"));
    let gen = doc.generate("dream").unwrap();
    assert_eq!(next_seeds(gen, 3), vec![9, 10, 11]);
    let doc = load(&source("nil").replace("seed: 7 params", "seed: 3 params"));
    assert_eq!(next_seeds(doc.generate("dream").unwrap(), 2), vec![3, 4]);
}

#[test]
fn bad_generates_say_how_to_fix_them() {
    let base = source("nil");
    let has = |source: String, needle: &str| {
        let diags = diagnostics(&source);
        assert!(diags.iter().any(|diag| diag.message.contains(needle)), "{needle}: {diags:#?}");
    };
    has(base.replace("flow: preset(\"bundled:image-to-video\")", ""), "needs `flow: preset(");
    has(base.replace("take: 0 track", "take: 5 track"), "`dream.take` is the index");
    has(base.replace("input: glow.out(@frame) ", ""), "needs its `input` wired");
    has(base.replace("span: [4.0, 4.0] flow", "flow"), "needs a `span");
    has(base.replace("seed: 7 params", "seed: -1 params"), "`dream.seed` is a whole number");
    has(base.replace("params: {frames: 124 steps: 30}", "params: 3"), "`dream.params` is");
    has(base.replace("take(asset(\"ast_qdy", "take(asset(\"bad_qdy"), "`dream.takes[0]` is");
    has(base.replace("seed: 7 params", "frame_at: 50.0 seed: 7 params"), "`dream.frame_at`");
}

#[test]
fn the_input_edit_shows_only_the_input_at_the_models_size() {
    let doc = load(&source("nil"));
    let gen = doc.generate("dream").unwrap();
    let text = input_edit_source(&doc, gen, 640, 352, 24.0).unwrap();
    let input = load(&text);
    assert_eq!((input.output.width, input.output.height, input.output.fps), (640, 352, 24.0));
    assert_eq!(input.duration, doc.duration);
    let mut ids = input.nodes.iter().map(|node| node.id.as_str()).collect::<Vec<_>>();
    ids.sort();
    assert_eq!(ids, ["generate_input", "glow", "hands"]);
    assert!(input.generates.is_empty());
    // Its picture at a time of the span is the input's, not the take.
    let eval = CutEval::new(&input);
    let plan = eval.frame_plan(6.0);
    assert!(plan.ops.iter().any(|op| op.node == "glow") && plan.ops.iter().any(|op| op.node == "hands"));
}

#[test]
fn chunks_blend_at_their_seams() {
    let parts = chunks(12.0, 5.0, 0.5);
    let videos = ["ast_yqzlg4xa4mbgpzs6e2qsuqwhsu", "ast_qdyytgcolstqfb6rgnbpnwva3m", "ast_fklvc3bvjnuijdg33d2uuitkbi"];
    let blend = parts.iter().zip(videos).map(|(chunk, id)| BlendPart {
        video: AssetRef { id: id.into(), rev: "arev_5ab3f998fd4a7b6860a9337b9924be40ad341d12276e341079ce2093539c715f".into() },
        chunk: *chunk,
        seconds: 5.17,
    }).collect::<Vec<_>>();
    let text = blend_edit_source(&blend, 12.0, 640, 352, 24.0, Fit::Fill);
    let doc = load(&text);
    let eval = CutEval::new(&doc);
    let seams = seams(&parts);
    assert_eq!(seams.len(), 2);
    // Inside the first seam both chunks are drawn, mixed; before it only the first.
    let (at, length) = seams[0];
    let plan = eval.frame_plan(at + length * 0.5);
    let mix = plan.ops.iter().find_map(|op| match &op.kind {
        DrawKind::Transition { mix, .. } => Some(*mix),
        _ => None,
    }).expect("a dissolve at the seam");
    assert!((mix - 0.5).abs() < 1e-6, "{mix}");
    let plan = eval.frame_plan(at * 0.5);
    let sources = plan.ops.iter().filter(|op| matches!(op.kind, DrawKind::Source { .. })).count();
    assert_eq!(sources, 1);
    // A chunk is stretched: 5.17 s of video over 5 s.
    let DrawKind::Source { source_time, .. } = &plan.ops.iter().find(|op| op.node == "chunk0").unwrap().kind else { panic!() };
    assert!((source_time.unwrap() - at * 0.5 * 5.17 / 5.0).abs() < 1e-9);
}
