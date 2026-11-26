//! Where an app-run clip shows in the output, for overlays bound to it
//! (src/bind.rs): the fit, a Reframe's crop, and the run's time.

use makepad_story_cut::bind::{self, BindPlace, BindTarget};
use makepad_story_cut::reframe::Rect;
use makepad_story_cut::*;

const RUN: &str = r#"run("ast_fklvc3bvjnuijdg33d2uuitkbi", "arev_e2e0b2ddc168547400e68147bf9e0161866f9cad405c9f94a2a47a5dcfe955ac", "ast_rbzhbuglyvqk6nprgjwvl2o33q", "arev_925b7b93f54e7cfa717f5f213568db1fdaea73ef483fc3ebbad2ea042a533bd1")"#;

fn doc(body: &str, members: &str) -> CutDoc {
    let source = format!(
        "use mod.flow.*\nuse mod.cut.*\n\n{body}\n\nFlow{{\n    kind: @cut\n    label: \"Bound\"\n    output: {{width: 1080 height: 1920 fps: 30}}\n    duration: 10.0\n    {members}\n}}\n"
    );
    CutDoc::load(&source).unwrap_or_else(|d| panic!("{d:#?}"))
}

fn run_ref(doc: &CutDoc) -> RunRef {
    match &doc.node("demo").unwrap().kind {
        CutNodeKind::Clip { src: MediaRef::Run(run), .. } => run.clone(),
        _ => panic!(),
    }
}

fn place(eval: &CutEval, t: f64, target: &BindTarget) -> BindPlace {
    let plan = eval.frame_plan(t);
    eval.bind_place(&plan, target).unwrap_or_else(|| panic!("not placed at {t}: {plan:#?}"))
}

#[test]
fn a_clip_filling_the_output_places_its_run_by_the_fit() {
    // A 1920x1080 run filling a 1080x1920 output: the middle 607.5 px shows, 3.16x.
    let d = doc(&format!("let demo = Clip{{src: {RUN} in: 2.0 span: [1.0, 5.0] fit: @fill}}\nlet out = CutOutput{{frame: demo.out(@frame)}}"), "demo, out");
    let mut eval = CutEval::new(&d);
    let media = MediaRef::Run(run_ref(&d));
    eval.set_source_size(media, 1920, 1080);
    let p = place(&eval, 3.0, &BindTarget::Clip("demo".into()));
    assert_eq!(p.clip, "demo");
    assert!((p.run_time - 4.0).abs() < 1e-9, "in + (t - start)");
    let w = 1080.0 * 1080.0 / 1920.0;
    assert!((p.crop.x - (1920.0 - w) / 2.0).abs() < 1e-6 && (p.crop.w - w).abs() < 1e-6 && p.crop.h == 1080.0, "{:?}", p.crop);
    assert_eq!(p.dst, Rect::new(0.0, 0.0, 1080.0, 1920.0));
    // The same clip found by its run.
    let by_run = place(&eval, 3.0, &BindTarget::Run(run_ref(&d)));
    assert_eq!(by_run, p);
    // Outside the clip's span nothing plays the run.
    assert!(eval.bind_place(&eval.frame_plan(7.0), &BindTarget::Clip("demo".into())).is_none());
}

#[test]
fn a_reframe_crop_moves_the_placement_with_it() {
    let d = doc(
        &format!(
            "let demo = Clip{{src: {RUN} span: [0.0, 4.0]}}\nlet fx = Fx{{input: demo.out(@frame) effect: \"kit/grain\"}}\nlet tall = Reframe{{input: fx.out(@frame) mode: @keys crop: keys([0.0, [0.0, 0.0, 540.0, 960.0]], [2.0, [1000.0, 100.0, 540.0, 960.0]])}}\nlet out = CutOutput{{frame: tall.out(@frame)}}"
        ),
        "demo, fx, tall, out",
    );
    let mut eval = CutEval::new(&d);
    eval.set_source_size(MediaRef::Run(run_ref(&d)), 3840, 2160);
    let target = BindTarget::Clip("demo".into());
    let p0 = place(&eval, 0.0, &target);
    assert_eq!((p0.crop, p0.dst), (Rect::new(0.0, 0.0, 540.0, 960.0), Rect::new(0.0, 0.0, 1080.0, 1920.0)));
    let p1 = place(&eval, 1.0, &target);
    assert_eq!(p1.crop, Rect::new(500.0, 50.0, 540.0, 960.0));
    assert!((p1.run_time - 1.0).abs() < 1e-9);
}

#[test]
fn a_bound_graphic_gets_its_runs_placement_on_its_own_clock() {
    use makepad_motion::bind::run::RunData;
    use std::sync::Arc;
    // The clip plays the run from 2 s at [1, 5); the graphic, a 540x960
    // document letterboxed into the 1080x1920 output (2x), spans [2, 3).
    let graphic = r#"let hl = Graphic{src: asset("ast_stufm3o7yer634iv7s5tliljom", "arev_77b67e882ccb22ad58b4377ee0ec21efa7bdca35e9dd558aa40f02dec9ea157f") span: [2.0, 3.0] bind: @demo}"#;
    let d = doc(&format!("let demo = Clip{{src: {RUN} in: 2.0 span: [1.0, 5.0] fit: @fill}}\n{graphic}\nlet out = CutOutput{{frame: demo.out(@frame)}}"), "demo, hl, out");
    assert!(bind::check(&d).is_empty());
    let g = d.graphic("hl").unwrap();
    assert_eq!(g.bind, Some(BindTarget::Clip("demo".into())));
    let mut eval = CutEval::new(&d);
    eval.set_source_size(MediaRef::Run(run_ref(&d)), 1080, 1920);
    let data = Arc::new(RunData { fps: 60.0, frames: 600, size: (1080.0, 1920.0), ..Default::default() });
    let binding = eval.run_binding(g, [540.0, 960.0], data, 30.0).unwrap();
    assert_eq!(binding.name, "demo");
    let at = binding.at(0.5);
    assert!((at.run_time - 3.5).abs() < 1e-9, "graphic 0.5 s = edit 2.5 s = run 2 + 1.5");
    // A recording pixel is half a document unit.
    assert!((at.scale().0 - 0.5).abs() < 1e-9);
    assert_eq!(at.point((100.0, 200.0)), (50.0, 100.0));
    // A bind that names no clip is reported.
    let wrong = doc(&format!("let demo = Clip{{src: {RUN} span: [1.0, 5.0]}}\n{}\nlet out = CutOutput{{frame: demo.out(@frame)}}", graphic.replace("@demo", "@nope")), "demo, hl, out");
    let problems = bind::check(&wrong);
    assert_eq!(problems.len(), 1);
    assert!(problems[0].1.contains("app-run clips: demo"), "{problems:?}");
    // A bind that is not a run.
    let bad = format!("let demo = Clip{{src: {RUN} span: [1.0, 5.0]}}\n{}\nlet out = CutOutput{{frame: demo.out(@frame)}}", graphic.replace("@demo", "3"));
    let source = format!("use mod.flow.*\nuse mod.cut.*\n{bad}\nFlow{{kind: @cut label: \"x\" output: {{width: 1080 height: 1920 fps: 30}} duration: 10.0 demo, hl, out}}");
    let errors = CutDoc::load(&source).unwrap_err();
    assert!(errors.iter().any(|e| e.message.contains("hl.bind")), "{errors:#?}");
}

#[test]
fn a_renderer_binds_a_graphic_from_the_edit_and_the_runs_alone() {
    use makepad_motion::bind::run::RunData;
    use std::collections::HashMap;
    use std::sync::Arc;
    let graphic = r#"let hl = Graphic{src: asset("ast_stufm3o7yer634iv7s5tliljom", "arev_77b67e882ccb22ad58b4377ee0ec21efa7bdca35e9dd558aa40f02dec9ea157f") span: [2.0, 3.0] bind: @demo}"#;
    let d = doc(&format!("let demo = Clip{{src: {RUN} in: marker(@go, 0.5) span: [1.0, 5.0]}}\n{graphic}\nlet out = CutOutput{{frame: demo.out(@frame)}}"), "demo, hl, out");
    let data = Arc::new(RunData { fps: 60.0, frames: 600, size: (1080.0, 1920.0), marks: vec![("go".into(), 60)], ..Default::default() });
    let runs = HashMap::from([(run_ref(&d), data)]);
    let context = bind::graphic_runs(&d, "hl", [1080.0, 1920.0], &runs, 30.0).unwrap();
    // Graphic 0 s = edit 2 s = clip 1 s in = run 1 s (mark) + 0.5 + 1.
    assert!((context.runs[0].at(0.0).run_time - 2.5).abs() < 1e-9);
    assert!(bind::graphic_runs(&d, "hl", [1080.0, 1920.0], &HashMap::new(), 30.0).is_none());
}
