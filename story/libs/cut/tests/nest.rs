//! Nested edits: compound clips, their evaluation inside
//! the parent, cycles, depth and staleness.

use makepad_story_cut::*;
use std::collections::HashMap;

const VIDEO: &str = "ast_yqzlg4xa4mbgpzs6e2qsuqwhsu";
const MUSIC: &str = "ast_qdyytgcolstqfb6rgnbpnwva3m";
const REV: &str = "arev_5ab3f998fd4a7b6860a9337b9924be40ad341d12276e341079ce2093539c715f";
const REV2: &str = "arev_1111111111111111111111111111111111111111111111111111111111111111";

/// A cut asset id per letter.
fn id(letter: char) -> String {
    format!("ast_{}a", letter.to_string().repeat(25))
}

fn edit_ref(letter: char) -> AssetRef {
    AssetRef { id: id(letter), rev: REV.into() }
}

fn header(body: &str, nodes: &str, width: u32, height: u32, duration: f64) -> String {
    format!("use mod.flow.*\nuse mod.cut.*\n{body}\nFlow{{\n    kind: @cut\n    output: {{width: {width} height: {height} fps: 30}}\n    duration: {duration}\n    {nodes}\n}}\n")
}

/// A cut whose only content is a compound clip of `child`.
fn nests(child: char) -> String {
    header(&format!("let c = Clip{{src: cut(\"{}\", \"{REV}\") span: [0.0, 5.0]}}\nlet out = CutOutput{{frame: c.out(@frame)}}", id(child)), "c, out", 1080, 1080, 5.0)
}

/// C: 1920×1080 with a video, music and a captioned voice line.
fn inner() -> String {
    header(
        &format!(
            "let v = Clip{{src: asset(\"{VIDEO}\", \"{REV}\") in: 1.0 span: [0.0, 10.0]}}\nlet music = AudioClip{{src: asset(\"{MUSIC}\", \"{REV}\") span: [0.0, 10.0] gain: 0.5}}\nlet voice = VoiceOver{{lines: [line(\"Deep words here.\", 2.0)]}}\nlet subs = Subtitles{{source: voice.out(@audio)}}\nlet top = Stack{{layers: [v.out(@frame), subs.out(@frame)]}}\nlet out = CutOutput{{frame: top.out(@frame) audio: [conn(1, music.out(@audio))]}}"
        ),
        "v, music, voice, subs, top, out",
        1920, 1080, 10.0,
    )
}

/// B: a square edit nesting C from its 2 s, over [1, 9).
fn middle() -> String {
    middle_with("")
}

fn middle_with(extra: &str) -> String {
    header(&format!("let deep = Clip{{src: cut(\"{}\", \"{REV}\") in: 2.0 span: [1.0, 8.0] fit: @fill {extra}}}\nlet out = CutOutput{{frame: deep.out(@frame)}}", id('c')), "deep, out", 1080, 1080, 10.0)
}

/// A: a vertical edit nesting B from its 0.5 s at double speed.
fn outer() -> String {
    outer_with("", "", "")
}

fn outer_with(extra: &str, body: &str, nodes: &str) -> String {
    header(&format!("let mid = Clip{{src: cut(\"{}\", \"{REV}\") in: 0.5 span: [3.0, 10.0] speed: 2.0 fit: @fit {extra}}}\n{body}\nlet out = CutOutput{{frame: mid.out(@frame)}}", id('b')), &format!("mid, {nodes}out"), 1080, 1920, 20.0)
}

fn library(sources: Vec<(char, String)>) -> impl FnMut(&AssetRef) -> Result<String, String> {
    let map = sources.into_iter().map(|(letter, source)| (id(letter), source)).collect::<HashMap<_, _>>();
    move |edit: &AssetRef| map.get(&edit.id).cloned().ok_or_else(|| format!("no cut {}", edit.id))
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn a_three_level_nest_plays_inside_the_parent() {
    let tree = CutTree::load(&outer(), Some(&id('a')), &mut library(vec![('b', middle()), ('c', inner())])).unwrap_or_else(|d| panic!("{d:#?}"));
    assert_eq!(tree.edits.len(), 2);
    assert_eq!(dependencies(&tree.root), [Dep::Cut(edit_ref('b'))]);
    let mut eval = tree.eval();
    eval.set_source_size(MediaRef::Asset(AssetRef { id: VIDEO.into(), rev: REV.into() }), 1920, 1080);

    // A 5 s → B 0.5 + (5 - 3) × 2 = 4.5 s → C 2 + (4.5 - 1) = 5.5 s → the
    // video at 1 + 5.5 = 6.5 s.
    let plan = eval.frame_plan(5.0);
    assert!(plan.unresolved.is_empty(), "{plan:#?}");
    for (index, op) in plan.ops.iter().enumerate() {
        let reads: Vec<Slot> = match &op.kind {
            DrawKind::Stack { layers } => layers.iter().map(|(slot, _)| *slot).collect(),
            DrawKind::Reframe { input, .. } => vec![*input],
            _ => vec![],
        };
        assert!(reads.iter().all(|&slot| slot < index), "op {index} reads a later slot");
    }
    let video = plan.ops.iter().find(|op| op.node == "mid/deep/v").expect("the innermost clip");
    let DrawKind::Source { source_time: Some(time), .. } = &video.kind else { panic!() };
    assert!(close(*time, 6.5));
    assert_eq!(video.size, (1920, 1080), "drawn at the inner edit's size");
    // B fills its square with C's middle; A letterboxes B's square.
    let DrawKind::Reframe { crop, dst_rect, .. } = &plan.ops.iter().find(|op| op.node == "mid/deep").unwrap().kind else { panic!() };
    assert_eq!((crop.x, crop.w, dst_rect.w), (420.0, 1080.0, 1080.0));
    let placed = plan.ops.iter().find(|op| op.node == "mid").unwrap();
    let DrawKind::Reframe { input, dst_rect, .. } = &placed.kind else { panic!() };
    assert_eq!((dst_rect.y, dst_rect.w, dst_rect.h, placed.size), (420.0, 1080.0, 1080.0, (1080, 1920)));
    assert_eq!(plan.result, Some(plan.ops.len() - 1));
    assert_eq!(plan.ops[*input].node, "mid/deep");
    // Outside the compound clip's span nothing of it is drawn.
    assert!(eval.frame_plan(2.0).ops.is_empty());

    // Sound: C's music and the video's soundtrack, at A's time and speed.
    let audio = eval.audio_plan(5.0, 5.5);
    let music = audio.segments.iter().find(|s| s.node == "mid/deep/music").expect("nested music");
    assert!(close(music.t0, 5.0) && close(music.t1, 5.5) && music.speed == 2.0);
    assert!(close(music.source_t0.unwrap(), 5.5) && music.source_ticks == Some(55_000_000));
    assert!(close(music.gain.0, 0.5) && close(eval.segment_gain(music, 5.2), 0.5));
    assert!(audio.segments.iter().any(|s| s.node == "mid/deep/v"), "the inner video's own sound");
    assert!(eval.audio_plan(0.0, 2.0).segments.is_empty());

    // A nested cut's own captions are off by default: not in the plan and
    // not burned in (the inner caption op is blanked).
    assert!(eval.subtitle_plan(3.5).is_empty());
    let plan = eval.frame_plan(3.5);
    assert!(!plan.ops.iter().any(|op| matches!(op.kind, DrawKind::Text { karaoke: Some(_), .. })), "{plan:#?}");
    assert!(plan.ops.iter().any(|op| op.node == "mid/deep/subs" && op.kind == DrawKind::Stack { layers: vec![] }));
}

fn captioned(outer_extra: &str, middle_extra: &str, body: &str, nodes: &str) -> CutTree {
    let source = outer_with(outer_extra, body, nodes);
    CutTree::load(&source, Some(&id('a')), &mut library(vec![('b', middle_with(middle_extra)), ('c', inner())])).unwrap_or_else(|d| panic!("{d:#?}"))
}

#[test]
fn nested_captions_show_when_every_clip_on_the_way_opts_in() {
    // Only the outer clip opts in: the inner one still hides them.
    let tree = captioned("subtitles: 1", "", "", "");
    assert!(tree.eval().subtitle_plan(3.5).is_empty());
    // Both opt in. C's line at C 2 s is A 3.5 s. C's anchor (0.5, 0.8) is
    // (960, 864); B's fill crops 420 px each side: (540, 864); A's fit puts
    // B's square at y 420: (540, 1284) of 1920.
    let tree = captioned("subtitles: 1", "subtitles: true", "", "");
    let CutNodeKind::Clip { subtitles, .. } = &tree.root.node("mid").unwrap().kind else { panic!() };
    assert!(*subtitles);
    let eval = tree.eval();
    let frames = eval.subtitle_plan(3.5);
    assert_eq!(frames.len(), 1, "{frames:#?}");
    assert_eq!((frames[0].node.as_str(), frames[0].text.as_str()), ("mid/deep/subs", "Deep words here."));
    assert!(close(frames[0].anchor[0], 0.5) && close(frames[0].anchor[1], 1284.0 / 1920.0) && close(frames[0].font_size, 64.0), "{:?}", frames[0]);
    // Drawn by the parent over the placed picture, once.
    let plan = eval.frame_plan(3.5);
    let captions = plan.ops.iter().filter(|op| matches!(op.kind, DrawKind::Text { karaoke: Some(_), .. })).collect::<Vec<_>>();
    assert_eq!(captions.len(), 1);
    assert_eq!((captions[0].node.as_str(), captions[0].size), ("mid/deep/subs", (1080, 1920)));
    let DrawKind::Text { position, .. } = &captions[0].kind else { panic!() };
    assert!(close(position[1], 1284.0));
    let DrawKind::Stack { layers } = &plan.ops[plan.result.unwrap()].kind else { panic!("{plan:#?}") };
    assert_eq!(layers.len(), 2);
    // The parent's safe zone keeps them clear of the Reels UI.
    let tree = captioned("subtitles: 1", "subtitles: 1", "let v = VoiceOver{}\nlet s = Subtitles{source: v.out(@audio) safe: @reels}", "v, s, ");
    let frames = tree.eval().subtitle_plan(3.5);
    assert!(close(frames[0].anchor[1], 0.65 - 64.0 * 1.3 * 0.5 / 1920.0), "{:?}", frames[0]);
    // `subtitles` takes 1/0 and booleans only.
    let Err(diags) = CutDoc::load(&outer_with("subtitles: 2", "", "")) else { panic!() };
    assert!(diags.iter().any(|d| d.kind == DiagnosticKind::InvalidParam && d.message.contains("subtitles")));
}

#[test]
fn without_its_nested_edit_a_compound_clip_is_unresolved() {
    let doc = CutDoc::load(&outer()).unwrap();
    let eval = CutEval::new(&doc);
    let plan = eval.frame_plan(5.0);
    assert_eq!((plan.result, plan.unresolved.clone()), (None, vec!["mid".to_string()]));
    assert!(eval.audio_plan(3.0, 4.0).segments.is_empty());
}

fn diagnostics(result: Result<CutTree, Vec<Diagnostic>>) -> Vec<(DiagnosticKind, String)> {
    let Err(diags) = result else { panic!("loaded") };
    diags.into_iter().map(|diag| (diag.kind, diag.message)).collect()
}

#[test]
fn cycles_are_load_errors() {
    // A nests B nests A.
    let diags = diagnostics(CutTree::load(&nests('b'), Some(&id('a')), &mut library(vec![('b', nests('a'))])));
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].0, DiagnosticKind::Cycle);
    assert!(diags[0].1.contains(&format!("{} → {} → {}", id('a'), id('b'), id('a'))), "{}", diags[0].1);
    // A nests itself.
    let diags = diagnostics(CutTree::load(&nests('a'), Some(&id('a')), &mut library(vec![('a', nests('a'))])));
    assert_eq!(diags[0].0, DiagnosticKind::Cycle);
    // A cycle further down, even without the root's own id: B → C → B.
    let diags = diagnostics(CutTree::load(&nests('b'), None, &mut library(vec![('b', nests('c')), ('c', nests('b'))])));
    assert_eq!(diags[0].0, DiagnosticKind::Cycle);
    // Reported at the root's compound clip.
    let Err(diags) = CutTree::load(&nests('b'), Some(&id('a')), &mut library(vec![('b', nests('a'))])) else { panic!() };
    assert_eq!((diags[0].line, diags[0].node.as_deref()), (3, Some("c")));
}

#[test]
fn nesting_is_at_most_four_deep_and_missing_edits_are_named() {
    let leaf = header("let t = Title{text: \"leaf\"}\nlet out = CutOutput{frame: t.out(@frame)}", "t, out", 1080, 1080, 5.0);
    // Root → b → c → d → e: four levels load.
    let four = library(vec![('b', nests('c')), ('c', nests('d')), ('d', nests('e')), ('e', leaf.clone())]);
    assert!(CutTree::load(&nests('b'), None, &mut { four }).is_ok());
    // … → f makes five.
    let five = library(vec![('b', nests('c')), ('c', nests('d')), ('d', nests('e')), ('e', nests('f')), ('f', leaf)]);
    let diags = diagnostics(CutTree::load(&nests('b'), None, &mut { five }));
    assert!(diags.iter().any(|(kind, message)| *kind == DiagnosticKind::NestDepth && message.contains("5 deep")), "{diags:#?}");
    let diags = diagnostics(CutTree::load(&nests('b'), None, &mut library(vec![])));
    assert!(diags[0].0 == DiagnosticKind::MissingRef && diags[0].1.contains("cannot be read"));
    let diags = diagnostics(CutTree::load(&nests('b'), None, &mut library(vec![('b', "Flow{}".into())])));
    assert!(diags[0].0 == DiagnosticKind::Eval && diags[0].1.contains("does not load"));
}

#[test]
fn only_a_clip_nests_an_edit_and_pinned_revisions_can_go_stale() {
    let body = format!("let m = AudioClip{{src: cut(\"{}\", \"{REV}\")}}\nlet t = Title{{text: \"x\"}}\nlet out = CutOutput{{frame: t.out(@frame)}}", id('b'));
    let Err(diags) = CutDoc::load(&header(&body, "m, t, out", 1080, 1080, 5.0)) else { panic!() };
    assert!(diags.iter().any(|d| d.kind == DiagnosticKind::InvalidRef && d.message.contains("only a Clip")), "{diags:#?}");

    let doc = CutDoc::load(&outer()).unwrap();
    assert_eq!(doc.nested_edits(), [("mid".to_string(), edit_ref('b'))]);
    assert_eq!(doc.node("mid").unwrap().kind.nested_edit(), Some(&edit_ref('b')));
    let CutNodeKind::Clip { audio, .. } = &doc.node("mid").unwrap().kind else { panic!() };
    assert!(*audio, "a nested cut's sound plays");
    assert!(doc.stale_nested(&|_| Some(REV.to_string())).is_empty());
    assert_eq!(doc.stale_nested(&|_| Some(REV2.to_string())), [("mid".to_string(), edit_ref('b'), REV2.to_string())]);
    assert!(doc.stale_nested(&|_| None).is_empty(), "an unknown head is not reported");
}
