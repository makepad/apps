use makepad_story_cut::reframe::Rect;
use makepad_story_cut::*;
use std::collections::HashMap;

const LAUNCH: &str = include_str!("fixtures/launch.splash");
const REFRAME: &str = include_str!("fixtures/reframe.splash");

fn load(source: &str) -> CutDoc {
    CutDoc::load(source).unwrap_or_else(|diags| panic!("{diags:#?}"))
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn media(doc: &CutDoc, id: &str) -> MediaRef {
    match &doc.node(id).unwrap().kind {
        CutNodeKind::Clip { src, .. } | CutNodeKind::AudioClip { src, .. } | CutNodeKind::Still { src, .. } => src.clone(),
        _ => panic!(),
    }
}

fn run(doc: &CutDoc) -> RunRef {
    let MediaRef::Run(run) = media(doc, "demo") else { panic!() };
    run
}

/// Every op reads only earlier slots.
fn check_order(plan: &FramePlan) {
    for (index, op) in plan.ops.iter().enumerate() {
        let inputs: Vec<Slot> = match &op.kind {
            DrawKind::Effect { input, .. } => input.iter().copied().collect(),
            DrawKind::Transition { a, b, .. } => a.iter().chain(b.iter()).copied().collect(),
            DrawKind::Stack { layers } => layers.iter().map(|(slot, _)| *slot).collect(),
            DrawKind::Reframe { input, .. } => vec![*input],
            _ => vec![],
        };
        assert!(inputs.iter().all(|&slot| slot < index), "op {index} reads a later slot: {plan:#?}");
    }
    assert!(plan.result.map_or(true, |slot| slot < plan.ops.len()));
}

fn op<'p>(plan: &'p FramePlan, node: &str) -> &'p DrawOp {
    plan.ops.iter().find(|op| op.node == node).unwrap_or_else(|| panic!("no op for {node}: {plan:#?}"))
}

#[test]
fn clip_source_time_and_markers() {
    let doc = load(LAUNCH);
    let mut eval = CutEval::new(&doc);
    let intro = doc.node("intro").unwrap();
    assert!(close(eval.source_time(intro, 1.0).unwrap(), 3.0), "in + (t - start) * speed");
    let demo = doc.node("demo").unwrap();
    assert_eq!(eval.source_time(demo, 6.0), None, "a marker needs the run's marks");
    eval.set_marks(run(&doc), HashMap::from([("note_saved".to_string(), 4.0)]));
    assert!(close(eval.source_time(demo, 6.0).unwrap(), 4.0), "mark 4.0 - 1.0 + (6 - 5)");
    assert_eq!(eval.active_nodes(0.5).iter().map(|node| node.id.as_str()).collect::<Vec<_>>(), ["intro", "glow", "top", "music", "out"]);
    assert!(close(eval.beat(1.5), 3.0));
}

#[test]
fn keys_eases_and_mix() {
    let doc = load(LAUNCH);
    let eval = CutEval::new(&doc);
    let CutNodeKind::Fx { params, .. } = &doc.node("glow").unwrap().kind else { panic!() };
    assert!(close(params[0].at1(-1.0), 0.2) && close(params[0].at1(9.0), 0.8), "clamped outside the keys");
    assert!(close(params[0].at1(1.5), 0.2 + 0.6 * 0.75), "ease_out at the segment's middle");
    let wipe = doc.node("wipe").unwrap();
    assert!(close(eval.mix(wipe, 5.25), 0.25));
    assert!(close(eval.mix(wipe, 4.0), 0.0) && close(eval.mix(wipe, 7.0), 1.0));

    let music = doc.node("music").unwrap();
    assert!(close(eval.gain(music, 0.5), 0.5) && close(eval.gain(music, 10.0), 1.0) && close(eval.gain(music, 16.75), 0.25));
    assert_eq!(eval.gain(music, 17.0), 0.0, "silent outside its span");

    let hold = Keys { points: vec![KeyPoint::new(0.0, [1.0], Ease::Linear), KeyPoint::new(1.0, [5.0], Ease::Hold)] };
    assert_eq!((hold.sample(0.999), hold.sample(1.0)), ([1.0], [5.0]));
    let in_out = Keys { points: vec![KeyPoint::new(0.0, [0.0], Ease::Linear), KeyPoint::new(2.0, [1.0], Ease::EaseInOut)] };
    assert!(close(in_out.sample(0.5)[0], 0.125) && close(in_out.sample(1.0)[0], 0.5) && close(in_out.sample(1.5)[0], 0.875));
}

#[test]
fn frame_plan_during_the_transition() {
    let doc = load(LAUNCH);
    let mut eval = CutEval::new(&doc);
    eval.set_source_size(media(&doc, "intro"), 1920, 1080);
    eval.set_source_size(media(&doc, "demo"), 1080, 1920);
    eval.set_marks(run(&doc), HashMap::from([("note_saved".to_string(), 4.0)]));
    let plan = eval.frame_plan(5.5);
    check_order(&plan);
    assert_eq!((plan.size, plan.beat), ((1080, 1920), 11.0));
    assert_eq!(plan.ops.iter().map(|op| op.node.as_str()).collect::<Vec<_>>(), ["intro", "demo", "wipe", "glow", "top"]);
    assert_eq!(plan.result, Some(4));

    let DrawKind::Source { source_time, src_rect, dst_rect, opacity, still: false, .. } = &op(&plan, "intro").kind else { panic!() };
    assert!(close(source_time.unwrap(), 7.5));
    // 16:9 filled into 9:16: the full height, a centred 9:16 column.
    let width = 1080.0 * 1080.0 / 1920.0;
    assert_eq!(*src_rect, Some(Rect::new((1920.0 - width) / 2.0, 0.0, width, 1080.0)));
    assert_eq!(*dst_rect, Some(Rect::new(0.0, 0.0, 1080.0, 1920.0)));
    assert_eq!(*opacity, 1.0);

    let DrawKind::Source { source_time, src_rect, .. } = &op(&plan, "demo").kind else { panic!() };
    assert!(close(source_time.unwrap(), 3.5));
    assert_eq!(*src_rect, Some(Rect::new(0.0, 0.0, 1080.0, 1920.0)), "the output's own size is 1:1");

    let DrawKind::Transition { a: Some(0), b: Some(1), mix, params, local_time, effect, .. } = &op(&plan, "wipe").kind else { panic!() };
    assert!(close(*mix, 0.5) && close(params[3], 0.5) && close(*local_time, 0.5));
    assert_eq!((effect.as_str(), params[0]), ("", -1.0));

    let DrawKind::Effect { input: Some(2), local_time, beat, params, .. } = &op(&plan, "glow").kind else { panic!() };
    assert!(close(*local_time, 5.5) && close(*beat, 11.0) && close(params[0], 0.8));

    let DrawKind::Stack { layers } = &op(&plan, "top").kind else { panic!() };
    assert_eq!(layers.as_slice(), [(3, Blend::Normal)], "the title is outside its span");
}

#[test]
fn transitions_pass_their_inputs_through_outside_their_span() {
    let doc = load(LAUNCH);
    let eval = CutEval::new(&doc);
    let plan = eval.frame_plan(2.0);
    check_order(&plan);
    assert_eq!(plan.ops.iter().map(|op| op.node.as_str()).collect::<Vec<_>>(), ["intro", "glow", "title", "top"]);
    let DrawKind::Effect { input: Some(0), .. } = &op(&plan, "glow").kind else { panic!() };
    let DrawKind::Source { src_rect: None, dst_rect: None, .. } = &op(&plan, "intro").kind else { panic!("sizes unknown") };
    // A title is the built-in "title" Motion document (§10.2).
    let DrawKind::Graphic { src: GraphicSrc::Builtin(name), params, local_time, offset, .. } = &op(&plan, "title").kind else { panic!() };
    assert_eq!(name, "title");
    assert!(params.contains(&("text".into(), graphic::InputValue::Text("Notes, instantly".into()))));
    assert!(params.contains(&("position".into(), graphic::InputValue::Vec2([540.0, 1536.0]))));
    assert!(close(*local_time + *offset, 2.0));

    let plan = eval.frame_plan(10.0);
    assert_eq!(plan.ops.iter().map(|op| op.node.as_str()).collect::<Vec<_>>(), ["demo", "glow", "top"]);
    let DrawKind::Source { source_time: None, .. } = &op(&plan, "demo").kind else { panic!("an unresolved marker has no time") };

    let plan = eval.frame_plan(17.0);
    assert_eq!((plan.ops.len(), plan.result), (0, None), "nothing is active at the end");
}

#[test]
fn audio_plan_windows_gains_and_order() {
    let doc = load(LAUNCH);
    let mut eval = CutEval::new(&doc);
    let plan = eval.audio_plan(0.5, 1.5);
    // The output's entries first, then the library clip's own sound (§8.1).
    assert_eq!(plan.segments.iter().map(|segment| (segment.node.as_str(), segment.entry)).collect::<Vec<_>>(), [("music", 0), ("intro", 2)]);
    let intro = &plan.segments[1];
    assert_eq!((intro.gain, intro.keyed, intro.line), ((1.0, 1.0), false, None));
    assert!(close(intro.source_t0.unwrap(), 2.5));
    assert_eq!(intro.source_ticks, Some(25_000_000));
    let music = &plan.segments[0];
    assert_eq!((music.node.as_str(), music.entry, music.t0, music.t1, music.keyed), ("music", 0, 0.5, 1.5, true));
    assert!(close(music.source_t0.unwrap(), 30.5));
    assert!(close(music.gain.0, 0.5) && (music.gain.1 - 1.0).abs() < 1e-6);

    let plan = eval.audio_plan(4.5, 5.5);
    assert_eq!(plan.segments.iter().map(|segment| (segment.node.as_str(), segment.t0)).collect::<Vec<_>>(), [("music", 4.5), ("voice", 5.0), ("intro", 4.5)]);
    let voice = &plan.segments[1];
    assert_eq!((voice.gain, voice.keyed, voice.source_t0), ((0.8, 0.8), false, Some(0.0)));

    eval.set_marks(run(&doc), HashMap::new());
    assert!(eval.audio_plan(20.0, 21.0).segments.is_empty());
}

#[test]
fn reframe_plans_crop_the_native_source() {
    let doc = load(REFRAME);
    let mut eval = CutEval::new(&doc);
    eval.set_source_size(media(&doc, "wide"), 3840, 2160);
    let plan = eval.frame_plan(1.0);
    check_order(&plan);
    let wide = op(&plan, "wide");
    assert_eq!(wide.size, (3840, 2160), "a reframed clip is drawn at its own size");
    let DrawKind::Source { src_rect: Some(src), dst_rect: Some(dst), .. } = &wide.kind else { panic!() };
    assert_eq!((*src, *dst), (Rect::new(0.0, 0.0, 3840.0, 2160.0), Rect::new(0.0, 0.0, 3840.0, 2160.0)));
    let tall = op(&plan, "tall");
    let DrawKind::Reframe { input: 0, crop, dst_rect } = &tall.kind else { panic!() };
    assert_eq!((tall.size, *crop, *dst_rect), ((1080, 1920), Rect::new(500.0, 50.0, 1080.0, 1920.0), Rect::new(0.0, 0.0, 1080.0, 1920.0)));

    let crop_at = |t: f64| match &eval.frame_plan(t).ops[1].kind {
        DrawKind::Reframe { crop, .. } => *crop,
        _ => panic!(),
    };
    assert_eq!(crop_at(2.9).x, 1000.0, "@hold keeps the previous key until its time");
    assert_eq!(crop_at(3.0).x, 2000.0);

    // Without keys a reframe takes the largest centred window of its aspect.
    let auto = REFRAME.replace("mode: @keys", "mode: @auto").replace(
        "crop: keys([0.0, [0.0, 0.0, 1080.0, 1920.0]], [2.0, [1000.0, 100.0, 1080.0, 1920.0]], [3.0, [2000.0, 200.0, 1080.0, 1920.0], @hold])",
        "",
    );
    let doc = load(&auto);
    let mut eval = CutEval::new(&doc);
    eval.set_source_size(media(&doc, "wide"), 3840, 2160);
    let DrawKind::Reframe { crop, .. } = &eval.frame_plan(0.0).ops[1].kind else { panic!() };
    let width = 2160.0 * 9.0 / 16.0;
    assert_eq!(*crop, Rect::new((3840.0 - width) / 2.0, 0.0, width, 2160.0));
}

#[test]
fn fit_modes() {
    let (src, dst) = fit_rects((1920, 1080), (1080, 1080), Fit::Fit);
    assert_eq!((src, dst), (Rect::new(0.0, 0.0, 1920.0, 1080.0), Rect::new(0.0, 236.25, 1080.0, 607.5)));
    let (src, dst) = fit_rects((640, 360), (1920, 1080), Fit::None);
    assert_eq!((src, dst), (Rect::new(0.0, 0.0, 640.0, 360.0), Rect::new(640.0, 360.0, 640.0, 360.0)));
    let (src, dst) = fit_rects((3840, 2160), (1920, 1080), Fit::None);
    assert_eq!((src, dst), (Rect::new(960.0, 540.0, 1920.0, 1080.0), Rect::new(0.0, 0.0, 1920.0, 1080.0)));
    let (src, _) = fit_rects((1920, 1080), (1920, 1080), Fit::Fit);
    assert_eq!(src, Rect::new(0.0, 0.0, 1920.0, 1080.0));
}

const ASSET: &str = "ast_yqzlg4xa4mbgpzs6e2qsuqwhsu";
const REV: &str = "arev_5ab3f998fd4a7b6860a9337b9924be40ad341d12276e341079ce2093539c715f";

fn edit(body: &str, nodes: &str, fps: f64, duration: f64) -> String {
    format!("use mod.flow.*\nuse mod.cut.*\n{body}\nFlow{{\n    kind: @cut\n    output: {{width: 1080 height: 1920 fps: {fps:?}}}\n    duration: {duration:?}\n    {nodes}\n}}\n")
}

/// Every output frame `n` of a clip maps to exactly source frame
/// `in_frames + (n - start_frames) * speed`, picked as "the frame at or
/// before" by integer pts (`k * 10_000_000 * den / num`, as §9 writes them).
#[test]
fn source_frames_are_exact_at_every_output_frame() {
    let mut naive_misses = 0;
    for (num, den) in [(24u64, 1u64), (30, 1), (60, 1), (30000, 1001)] {
        let fps = num as f64 / den as f64;
        for speed in [1u64, 2] {
            for (start_frames, in_frames) in [(0u64, 0u64), (150, 120), (301, 7)] {
                let start = start_frames as f64 / fps;
                let source_in = in_frames as f64 / fps;
                let body = format!(
                    "let a = Clip{{src: asset(\"{ASSET}\", \"{REV}\") in: {source_in:?} speed: {speed}.0 span: [{start:?}, 40.0]}}\nlet out = CutOutput{{frame: a.out(@frame)}}"
                );
                let doc = CutDoc::load(&edit(&body, "a, out", fps, 60.0)).unwrap_or_else(|d| panic!("{d:#?}"));
                let eval = CutEval::new(&doc);
                let clip = doc.node("a").unwrap();
                let pts = |k: u64| (k as u128 * 10_000_000 * den as u128 / num as u128) as i64;
                for n in start_frames..start_frames + 900 {
                    let t = n as f64 / fps;
                    let expected = in_frames + (n - start_frames) * speed;
                    let ticks = eval.source_ticks(clip, t).unwrap();
                    assert!(pts(expected) <= ticks && ticks < pts(expected + 1), "fps {fps} speed {speed} n {n}: {ticks} vs pts {}", pts(expected));
                    let DrawKind::Source { source_ticks, .. } = &eval.frame_plan(t).ops[0].kind else { panic!() };
                    assert_eq!(*source_ticks, Some(ticks));
                    if (eval.source_time(clip, t).unwrap() * fps).floor() as u64 != expected {
                        naive_misses += 1;
                    }
                }
            }
        }
    }
    assert!(naive_misses > 0, "the float time alone does misplace frames; the ticks are what make this exact");
}

fn reframe_doc(clip_span: &str, reframe_span: &str) -> CutDoc {
    let body = format!(
        "let a = Clip{{src: asset(\"{ASSET}\", \"{REV}\") in: 1.0 {clip_span}}}\nlet r = Reframe{{input: a.out(@frame) {reframe_span}}}\nlet out = CutOutput{{frame: r.out(@frame)}}"
    );
    CutDoc::load(&edit(&body, "a, r, out", 30.0, 20.0)).unwrap_or_else(|d| panic!("{d:#?}"))
}

#[test]
fn reframe_keys_map_source_time_to_the_reframes_clock() {
    use makepad_story_cut::reframe::{CropKey, PinnedKey};
    let key = |t: f64, x: f64| CropKey { t, crop: Rect::new(x, 0.0, 1080.0, 1920.0), ease: Ease::Linear, pinned: false };
    let solved = [key(0.5, 100.0), key(1.0, 200.0), key(3.0, 600.0)];

    // A clip at 5 s (in 1.0) under an unspanned reframe: source 3.0 is edit 7.0.
    let doc = reframe_doc("span: [5.0, 10.0]", "");
    let eval = CutEval::new(&doc);
    let keys = eval.reframe_keys("r", &solved).unwrap();
    assert_eq!(keys.points.iter().map(|p| p.t).collect::<Vec<_>>(), [4.5, 5.0, 7.0]);
    let mut doc = doc.clone();
    let CutNodeKind::Reframe { crop, .. } = &mut doc.nodes[1].kind else { panic!() };
    *crop = Some(Animated::Keys(keys.clone()));
    let mut eval = CutEval::new(&doc);
    let MediaRef::Asset(asset) = media(&doc, "a") else { panic!() };
    eval.set_source_size(MediaRef::Asset(asset), 3840, 2160);
    for (t, x) in [(5.0, 200.0), (6.0, 400.0), (7.0, 600.0)] {
        let plan = eval.frame_plan(t);
        let DrawKind::Reframe { crop, .. } = &plan.ops[1].kind else { panic!("{plan:#?}") };
        assert_eq!(crop.x, x, "t {t}");
    }
    // The inverse gives back the source frames the `@pin` keys sit on.
    assert!(eval.pinned_keys("r", 30.0).unwrap().is_empty(), "no key is pinned yet");
    let mut doc = doc.clone();
    let CutNodeKind::Reframe { crop: Some(Animated::Keys(keys)), .. } = &mut doc.nodes[1].kind else { panic!() };
    keys.points[1].pinned = true;
    keys.points[2].pinned = true;
    let eval = CutEval::new(&doc);
    let pins = eval.pinned_keys("r", 30.0).unwrap();
    assert_eq!(pins, [PinnedKey { frame: 30, crop: solved[1].crop }, PinnedKey { frame: 90, crop: solved[2].crop }]);

    // A spanned reframe counts from its own start; keys before it keep the last one.
    let doc = reframe_doc("span: [5.0, 10.0]", "span: [6.0, 4.0]");
    let keys = CutEval::new(&doc).reframe_keys("r", &solved).unwrap();
    assert_eq!(keys.points.iter().map(|p| p.t).collect::<Vec<_>>(), [-1.0, 1.0]);

    let not_a_clip = CutEval::new(&doc).reframe_keys("a", &solved);
    assert!(not_a_clip.is_err());
}

#[test]
fn non_finite_and_negative_times_plan_nothing() {
    let doc = load(LAUNCH);
    let eval = CutEval::new(&doc);
    for t in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
        let plan = eval.frame_plan(t);
        assert_eq!((plan.ops.len(), plan.result), (0, None), "t {t}");
        assert!(eval.audio_plan(t, t + 1.0).segments.is_empty() || t == -1.0);
    }
    assert!(eval.audio_plan(f64::NAN, 1.0).segments.is_empty());
    assert!(eval.audio_plan(1.0, 0.5).segments.is_empty());
    let keys = Keys { points: vec![KeyPoint::new(0.0, [1.0], Ease::Linear), KeyPoint::new(1.0, [2.0], Ease::Linear)] };
    assert_eq!(keys.sample(f64::NAN), [1.0]);
    // An unspanned keyed node at NaN (the case that used to panic).
    let body = format!("let a = Clip{{src: asset(\"{ASSET}\", \"{REV}\")}}\nlet f = Fx{{input: a.out(@frame) effect: \"bloom\" p1: keys([0.0, 0.0], [1.0, 1.0])}}\nlet out = CutOutput{{frame: f.out(@frame)}}");
    let doc = CutDoc::load(&edit(&body, "a, f, out", 30.0, 2.0)).unwrap();
    assert!(CutEval::new(&doc).frame_plan(f64::NAN).ops.is_empty());
}

#[test]
fn reframes_need_their_input_size_and_crop_at_the_crops_aspect() {
    let doc = load(REFRAME);
    let eval = CutEval::new(&doc);
    let plan = eval.frame_plan(1.0);
    assert_eq!((plan.ops.len(), plan.result, plan.unresolved.as_slice()), (0, None, ["tall".to_string()].as_slice()), "no source size: said, not guessed");

    // A square authored crop letterboxes in the 9:16 output instead of stretching.
    let square = REFRAME.replace("mode: @keys", "mode: @keys aspect: [9, 16]").replace(
        "crop: keys([0.0, [0.0, 0.0, 1080.0, 1920.0]], [2.0, [1000.0, 100.0, 1080.0, 1920.0]], [3.0, [2000.0, 200.0, 1080.0, 1920.0], @hold])",
        "crop: [100.0, 100.0, 1080.0, 1080.0]",
    );
    let doc = load(&square);
    let mut eval = CutEval::new(&doc);
    eval.set_source_size(media(&doc, "wide"), 3840, 2160);
    let plan = eval.frame_plan(1.0);
    let DrawKind::Reframe { dst_rect, .. } = &plan.ops[1].kind else { panic!() };
    assert_eq!(*dst_rect, Rect::new(0.0, 420.0, 1080.0, 1080.0));
}

#[test]
fn stacks_under_a_reframe_share_the_first_known_native_size() {
    let body = format!(
        "let a = Clip{{src: asset(\"{ASSET}\", \"{REV}\")}}\nlet b = Still{{src: asset(\"ast_stufm3o7yer634iv7s5tliljom\", \"arev_77b67e882ccb22ad58b4377ee0ec21efa7bdca35e9dd558aa40f02dec9ea157f\")}}\nlet s = Stack{{layers: [a.out(@frame), b.out(@frame)]}}\nlet r = Reframe{{input: s.out(@frame) mode: @keys crop: [0.0, 0.0, 1080.0, 1920.0]}}\nlet out = CutOutput{{frame: r.out(@frame)}}"
    );
    let doc = CutDoc::load(&edit(&body, "a, b, s, r, out", 30.0, 2.0)).unwrap_or_else(|d| panic!("{d:#?}"));
    let mut eval = CutEval::new(&doc);
    eval.set_source_size(media(&doc, "a"), 3840, 2160);
    eval.set_source_size(media(&doc, "b"), 1920, 1080);
    let plan = eval.frame_plan(0.5);
    check_order(&plan);
    assert!(plan.ops[..3].iter().all(|op| op.size == (3840, 2160)), "{plan:#?}");
    let DrawKind::Source { src_rect, dst_rect, .. } = &op(&plan, "b").kind else { panic!() };
    assert_eq!((*src_rect, *dst_rect), (Some(Rect::new(0.0, 0.0, 1920.0, 1080.0)), Some(Rect::new(0.0, 0.0, 3840.0, 2160.0))));
    assert_eq!(op(&plan, "r").size, (1080, 1920));
}

#[test]
fn keyed_mix_without_span_and_clip_audio_at_speed() {
    let body = format!(
        "let a = Clip{{src: asset(\"{ASSET}\", \"{REV}\") speed: 2.0 span: [0.0, 4.0]}}\nlet b = Clip{{src: asset(\"{ASSET}\", \"{REV}\") in: 3.0}}\nlet x = Transition{{a: a.out(@frame) b: b.out(@frame) mix: keys([1.0, 0.0], [3.0, 1.0])}}\nlet m = AudioClip{{src: asset(\"{ASSET}\", \"{REV}\") pan: keys([0.0, -1.0], [2.0, 1.0])}}\nlet out = CutOutput{{frame: x.out(@frame) audio: [conn(1, a.out(@audio)), conn(2, m.out(@audio))]}}"
    );
    let doc = CutDoc::load(&edit(&body, "a, b, x, m, out", 30.0, 8.0)).unwrap_or_else(|d| panic!("{d:#?}"));
    let eval = CutEval::new(&doc);
    let x = doc.node("x").unwrap();
    assert_eq!((eval.mix(x, 0.0), eval.mix(x, 2.0), eval.mix(x, 5.0)), (0.0, 0.5, 1.0));
    let DrawKind::Transition { mix, a: Some(_), b: Some(_), .. } = &eval.frame_plan(2.0).ops[2].kind else { panic!() };
    assert_eq!(*mix, 0.5);
    let plan = eval.audio_plan(1.0, 2.0);
    let clip = &plan.segments[0];
    assert_eq!((clip.node.as_str(), clip.speed, clip.source_t0, clip.gain), ("a", 2.0, Some(2.0), (1.0, 1.0)));
    let music = &plan.segments[1];
    assert!(music.keyed && music.pan.0 == 0.0 && (music.pan.1 - 1.0).abs() < 1e-6);
}

#[test]
fn library_transitions_and_effects_are_asset_revisions() {
    const FX: &str = "ast_bbbbbbbbbbbbbbbbbbbbbbbbba";
    let body = format!(
        "let a = Clip{{src: asset(\"{ASSET}\", \"{REV}\") span: [0.0, 4.0]}}\nlet b = Clip{{src: asset(\"{ASSET}\", \"{REV}\") in: 3.0 span: [2.0, 4.0]}}\nlet x = Transition{{a: a.out(@frame) b: b.out(@frame) effect: asset(\"{FX}\", \"{REV}\") span: [2.0, 2.0]}}\nlet g = Fx{{input: x.out(@frame) effect: asset(\"{FX}\", \"{REV}\") span: [0.0, 6.0]}}\nlet out = CutOutput{{frame: g.out(@frame)}}"
    );
    let doc = CutDoc::load(&edit(&body, "a, b, x, g, out", 30.0, 6.0)).unwrap_or_else(|d| panic!("{d:#?}"));
    let fx = AssetRef { id: FX.into(), rev: REV.into() };
    let CutNodeKind::Transition { effect, effect_asset, .. } = &doc.node("x").unwrap().kind else { panic!() };
    assert_eq!((effect.as_str(), effect_asset.clone()), ("", Some(fx.clone())));
    assert_eq!(doc.node("x").unwrap().kind.effect_ref(), Some(EffectRef::Asset(fx.clone())));
    assert_eq!(doc.node("g").unwrap().kind.effect_ref(), Some(EffectRef::Asset(fx.clone())));
    // Recorded once as a dependency, next to the clip's source.
    let deps = dependencies(&doc);
    assert_eq!(deps, [Dep::Asset(AssetRef { id: ASSET.into(), rev: REV.into() }), Dep::Asset(fx.clone())]);
    let plan = CutEval::new(&doc).frame_plan(3.0);
    let DrawKind::Transition { effect, effect_asset, mix, .. } = &op(&plan, "x").kind else { panic!() };
    assert_eq!((effect.as_str(), effect_asset.clone(), *mix), ("", Some(fx.clone()), 0.5));
    // Presets are unchanged; a run() or edit() is not an effect.
    let preset = body.replace(&format!("effect: asset(\"{FX}\", \"{REV}\") span: [2.0, 2.0]"), "effect: \"kit/zoom_blur\" span: [2.0, 2.0]");
    let doc = CutDoc::load(&edit(&preset, "a, b, x, g, out", 30.0, 6.0)).unwrap();
    assert_eq!(doc.node("x").unwrap().kind.effect_ref(), Some(EffectRef::Preset("kit/zoom_blur".into())));
    let bad = body.replace(&format!("effect: asset(\"{FX}\", \"{REV}\") span: [2.0, 2.0]"), &format!("effect: cut(\"{FX}\", \"{REV}\") span: [2.0, 2.0]"));
    let Err(diags) = CutDoc::load(&edit(&bad, "a, b, x, g, out", 30.0, 6.0)) else { panic!() };
    assert!(diags.iter().any(|d| d.kind == DiagnosticKind::InvalidRef && d.node.as_deref() == Some("x")), "{diags:#?}");
}

#[test]
fn an_app_view_plans_an_app_op_for_its_span() {
    let doc = load(
        "use mod.flow.*\nuse mod.cut.*\n\
         let view = AppView{app: @counter fit: @fit span: [1.0, 2.0]}\n\
         let out = CutOutput{frame: view.out(@frame)}\n\
         Flow{ kind: @cut output: {width: 640 height: 360 fps: 30} duration: 4.0 view, out }",
    );
    assert!(matches!(&doc.node("view").unwrap().kind, CutNodeKind::AppView { app, fit: Fit::Fit, .. } if app == "counter"));
    let eval = CutEval::new(&doc);
    let plan = eval.frame_plan(1.5);
    check_order(&plan);
    let app = op(&plan, "view");
    assert_eq!(app.size, (640, 360));
    assert!(matches!(&app.kind, DrawKind::App { app, fit: Fit::Fit, .. } if app == "counter"));
    assert!(eval.frame_plan(3.5).ops.iter().all(|op| op.node != "view"), "outside its span there is no layer");
    let diags = CutDoc::load("use mod.flow.*\nuse mod.cut.*\nlet view = AppView{span: [0.0, 1.0]}\nlet out = CutOutput{frame: view.out(@frame)}\nFlow{ kind: @cut output: {width: 64 height: 36 fps: 30} duration: 1.0 view, out }").err().expect("an AppView without an app does not load");
    assert!(diags.iter().any(|d| d.message.contains("app")), "{diags:#?}");
}

const CAMERA_CUT: &str = "use mod.flow.*\nuse mod.cut.*\n\
     let cam = Camera{device: \"OBSBOT\" width: 1920 height: 1080 fps: 60 crop: [0.1, 0.0, 0.8, 1.0] rotate: 90 mirror: true adjust: {brightness: 0.1 contrast: 1.2 saturation: 0.5} span: [0.0, 4.0]}\n\
     let out = CutOutput{frame: cam.out(@frame)}\n\
     Flow{ kind: @cut output: {width: 640 height: 360 fps: 30} duration: 4.0 cam, out }";

#[test]
fn a_camera_node_reads_its_fields_and_plans_a_camera_op() {
    let doc = load(CAMERA_CUT);
    let CutNodeKind::Camera(spec) = &doc.node("cam").unwrap().kind else { panic!("a Camera node") };
    assert_eq!(spec.device, makepad_story_cut::CameraDevice::Name("OBSBOT".into()));
    assert_eq!((spec.width, spec.height, spec.fps), (1920, 1080, 60.0));
    assert_eq!(spec.crop, [0.1, 0.0, 0.8, 1.0]);
    assert_eq!((spec.rotate, spec.mirror), (90, true));
    assert_eq!((spec.adjust.brightness, spec.adjust.contrast, spec.adjust.saturation), (0.1, 1.2, 0.5));
    assert_eq!(spec.fit, Fit::Fit, "a camera letterboxes unless told otherwise");
    let eval = CutEval::new(&doc);
    let plan = eval.frame_plan(1.0);
    check_order(&plan);
    let camera = op(&plan, "cam");
    assert_eq!(camera.size, (640, 360));
    assert!(matches!(&camera.kind, DrawKind::Camera { spec: planned } if planned == spec));
    assert!(eval.frame_plan(5.0).ops.iter().all(|op| op.node != "cam"), "outside its span there is no layer");

    let pattern = load(&CAMERA_CUT.replace(r#""OBSBOT""#, "@test_pattern"));
    assert!(matches!(&pattern.node("cam").unwrap().kind, CutNodeKind::Camera(spec) if spec.device == makepad_story_cut::CameraDevice::TestPattern));
}

#[test]
fn a_bare_camera_asks_for_the_first_camera_at_720p30() {
    let doc = load(&CAMERA_CUT.replace("Camera{device: \"OBSBOT\" width: 1920 height: 1080 fps: 60 crop: [0.1, 0.0, 0.8, 1.0] rotate: 90 mirror: true adjust: {brightness: 0.1 contrast: 1.2 saturation: 0.5} span: [0.0, 4.0]}", "Camera{span: [0.0, 4.0]}"));
    let CutNodeKind::Camera(spec) = &doc.node("cam").unwrap().kind else { panic!("a Camera node") };
    assert_eq!(*spec, makepad_story_cut::CameraSpec::default());
    assert_eq!((spec.width, spec.height, spec.fps), (1280, 720, 30.0));
}

#[test]
fn camera_fields_that_make_no_sense_are_refused_naming_the_field() {
    for (bad, field) in [
        ("rotate: 45", "rotate"),
        ("crop: [0.5, 0.0, 0.8, 1.0]", "crop"),
        ("fps: 0", "fps"),
        ("width: 12.5", "width"),
        ("device: true", "device"),
        ("adjust: {contrast: 9}", "adjust"),
        ("adjust: {gamma: 1}", "adjust"),
        ("mirror: \"yes\"", "mirror"),
    ] {
        let source = CAMERA_CUT.replace("span: [0.0, 4.0]}", &format!("{bad} span: [0.0, 4.0]}}"));
        let diags = CutDoc::load(&source).err().unwrap_or_else(|| panic!("`{bad}` loads"));
        assert!(diags.iter().any(|d| d.message.contains(&format!(".{field}`")) || d.message.contains(&format!(".{field}."))), "{bad}: {diags:#?}");
    }
}

#[test]
fn a_camera_device_picks_by_name_or_number() {
    use makepad_story_cut::CameraDevice::*;
    let names = ["ASUS FHD webcam", "OBSBOT Meet 2"];
    assert_eq!(First.pick(names), Some(0));
    assert_eq!(TestPattern.pick(names), None, "a pattern never selects a system camera");
    assert_eq!(Index(1).pick(names), Some(1));
    assert_eq!(Index(2).pick(names), None);
    assert_eq!(Name("obsbot".into()).pick(names), Some(1), "any case, part of the name");
    assert_eq!(Name("elgato".into()).pick(names), Some(0), "no match: the first camera");
    assert_eq!(Name("Obs".into()).pick(["ASUS FHD webcam", "OBSBOT Meet 2"]), Some(1));
    assert_eq!(Name("OBSBOT".into()).pick(Vec::<&str>::new()), None);
    assert_eq!(First.pick(Vec::<&str>::new()), None);
}
