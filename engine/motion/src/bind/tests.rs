use crate::plain::PrimitiveFrame;
use super::run::*;

/// A 10 fps, 100x200 run: a click on `main.save` at frames 10..14, typing
/// "hi\nyo" into `main.body` over 20..30, cmd+S at 32, marks at 5 and 20.
const RUN: &str = r#"{
  "format": "makepad-app-run/1", "label": "t",
  "output": {"width": 100, "height": 200, "dpi_scale": 1, "fps": 10},
  "frames": 40, "duration": 4,
  "marks": {"second": 20, "first": 5},
  "calls": [
    {"call": "click", "clock": [0, 15], "frames": [0, 15], "target": "main.save"},
    {"call": "type", "clock": [15, 30], "frames": [15, 30], "target": "main.body", "text": "hi\nyo"},
    {"call": "key", "clock": [32, 34], "frames": [32, 34]}
  ],
  "focus": {
    "events": [],
    "frames": [
      {"from": 0, "to": 10, "rects": [[10, 10, 20, 10, 1, "click"]]},
      {"from": 10, "to": 20, "rects": [[10, 10, 20, 10, 1, "click"], [0, 0, 100, 100, 0.4, "layout"]]},
      {"from": 20, "to": 30, "rects": [[0, 50, 100, 40, 1, "type"]]}
    ]
  },
  "bind": {
    "pointer": {"path": [[0, 50, 100], [1, 40, 60], [2, 20, 15]], "presses": [{"frames": [10, 11], "at": [20, 15], "button": 0}]},
    "keys": [
      {"frame": 20, "text": "h"}, {"frame": 22, "text": "i"},
      {"frame": 24, "key": "ReturnKey", "mods": [], "up": 25},
      {"frame": 26, "text": "y"}, {"frame": 28, "text": "o"},
      {"frame": 32, "key": "KeyS", "mods": ["cmd"], "up": 33}
    ],
    "widgets": {
      "main.save#0": [[0, 10, 10, 20, 10]],
      "main.body#0": [[0, 0, 50, 100, 20], [26, 0, 50, 100, 40], [35]],
      "list.save#0": [[0, 0, 150, 10, 10]]
    }
  }
}"#;

fn run() -> RunData {
    RunData::parse(RUN.as_bytes()).unwrap()
}

#[test]
fn marks_are_in_frame_order_and_steps_count_them() {
    let r = run();
    assert_eq!(r.marks[0].0, "first");
    assert_eq!(r.mark("second"), Some(2.0));
    assert!(r.step(0.4).is_none());
    let s = r.step(2.35).unwrap();
    assert_eq!((s.index, s.count, s.name.as_str()), (2, 2, "second"));
    assert!((s.age - 0.35).abs() < 1e-9);
}

#[test]
fn widgets_resolve_by_path_suffix_and_follow_the_listing() {
    let r = run();
    assert_eq!(r.widget("body", 1.0), Some(Rect::new(0.0, 50.0, 100.0, 20.0)));
    assert_eq!(r.widget("@main.body", 2.7), Some(Rect::new(0.0, 50.0, 100.0, 40.0)));
    // Gone from the listing at frame 35.
    assert_eq!(r.widget("main.body", 3.6), None);
    // `save` names two widgets; the path picks one.
    assert_eq!(r.widget_keys("save").len(), 2);
    assert_eq!(r.widget("list.save", 0.0), Some(Rect::new(0.0, 150.0, 10.0, 10.0)));
    assert_eq!(r.widget("ave", 0.0), None);
}

#[test]
fn the_cursor_moves_between_recorded_frames() {
    let r = run();
    assert_eq!(r.cursor(0.0), Some((50.0, 100.0)));
    let (x, y) = r.cursor(0.15).unwrap();
    assert!((x - 30.0).abs() < 1e-9 && (y - 37.5).abs() < 1e-9);
    assert_eq!(r.cursor(3.0), Some((20.0, 15.0)));
}

#[test]
fn presses_are_found_newest_first_with_their_age() {
    let r = run();
    assert!(r.press(0.9, 0).is_none());
    let (p, age) = r.press(1.25, 0).unwrap();
    assert_eq!(p.at, (20.0, 15.0));
    assert!((age - 0.25).abs() < 1e-9);
    assert!(r.press(1.25, 1).is_none());
}

#[test]
fn typing_reports_the_text_so_far() {
    let r = run();
    assert!(r.typing(1.0).is_none());
    let t = r.typing(2.45).unwrap();
    assert_eq!(t.text, "hi\n");
    assert_eq!(t.target.as_deref(), Some("main.body"));
    assert_eq!(t.rect, Some(Rect::new(0.0, 50.0, 100.0, 20.0)));
    assert!(t.progress > 0.5 && t.progress < 0.6);
    assert_eq!(r.typing(2.9).unwrap().text, "hi\nyo");
}

#[test]
fn shortcuts_show_for_their_hold() {
    let r = run();
    // Return while typing is text, not a badge.
    assert!(r.shortcut(2.45, 1.0).is_none());
    let (k, age) = r.shortcut(3.5, 1.0).unwrap();
    assert_eq!(k.label(), "⌘ S");
    assert!((age - 0.3).abs() < 1e-9);
    assert!(r.shortcut(4.3, 1.0).is_none());
    let k = RunKey::Key { frame: 0, up: 0, code: "KeyZ".into(), mods: vec!["cmd".into(), "shift".into()] };
    assert_eq!(k.label(), "⇧ ⌘ Z");
    assert_eq!(key_name("Escape"), "Esc");
}

#[test]
fn the_focus_eases_between_targets_and_fades_out() {
    let r = run();
    // The weak layout rect is left out of the union.
    assert_eq!(r.focus_raw(1.5), Some(Rect::new(10.0, 10.0, 20.0, 10.0)));
    // Settled on the click rect.
    let (rect, p) = r.focus(1.9, 0.3).unwrap();
    assert_eq!((rect, p), (Rect::new(10.0, 10.0, 20.0, 10.0), 1.0));
    // Halfway (in time) to the typing rect: past halfway in space.
    let (rect, _) = r.focus(2.15, 0.3).unwrap();
    assert!(rect.y > 30.0 && rect.y < 50.0, "{rect:?}");
    assert_eq!(r.focus(2.5, 0.3).unwrap().0, Rect::new(0.0, 50.0, 100.0, 40.0));
    // After the last span it holds and fades.
    let (rect, p) = r.focus(3.15, 0.3).unwrap();
    assert_eq!(rect, Rect::new(0.0, 50.0, 100.0, 40.0));
    assert!(p > 0.4 && p < 0.6, "{p}");
    assert!(r.focus(3.5, 0.3).is_none());
    // Fading in from nothing at the start.
    assert!(r.focus(0.05, 0.3).unwrap().1 < 0.5);
    // Pure: the same answer in any order.
    assert_eq!(r.focus(2.15, 0.3), r.focus(2.15, 0.3));
}

#[test]
fn an_older_run_json_derives_widgets_and_presses() {
    // The notes run from before the bind section (reports/L6-apprun.md).
    let old = r#"{"format": "makepad-app-run/1", "output": {"width": 1080, "height": 1920, "fps": 60}, "frames": 251,
      "marks": {"typed": 221},
      "calls": [{"call": "click", "frames": [14, 39], "target": "root_view.dock.compose"},
                {"call": "wait_visible", "frames": [39, 40], "target": "screen.document"},
                {"call": "type", "frames": [40, 221], "target": "screen.document"}],
      "focus": {"events": [
        {"source": "click", "rect": [540, 960, 492, 936], "weight": 1, "frames": [14, 69]},
        {"source": "wait", "rect": [789, 234, 291, 1494], "weight": 0.8, "frames": [40, 100]},
        {"source": "type", "rect": [0, 234, 1080, 1494], "weight": 1, "frames": [65, 221]}], "frames": []}}"#;
    let r = RunData::parse(old.as_bytes()).unwrap();
    assert!(r.derived);
    assert_eq!(r.widget("screen.document", 2.0), Some(Rect::new(0.0, 234.0, 1080.0, 1494.0)));
    // The type call's click on its field.
    assert_eq!(r.presses.len(), 1);
    assert!(r.cursor(1.0).is_none());
}

#[test]
fn a_file_that_is_not_a_run_is_refused() {
    assert!(RunData::parse(br#"{"format": "x"}"#).unwrap_err().contains("not an app run"));
    assert!(RunData::parse(b"nope").is_err());
}

use super::expr::*;
use super::place::BindFrame;

fn at(t: f64) -> BindFrame {
    BindFrame::identity(t, (100.0, 200.0))
}

fn sample(subject: Subject, field: Field, frame: &BindFrame) -> BindValue {
    Bound { subject, field }.sample(&run(), frame)
}

#[test]
fn a_crop_moves_and_scales_what_is_bound() {
    // A 50x100 crop from (0, 50) shown on a 200x400 graphic: 4x.
    let frame = BindFrame { run_time: 1.0, crop: Rect::new(0.0, 50.0, 50.0, 100.0), dst: Rect::new(0.0, 0.0, 200.0, 400.0) };
    let widget = |field| sample(Subject::Widget { path: "body".into(), pad: 0.0 }, field, &frame);
    assert_eq!(widget(Field::At), BindValue::Vec2(0.0, 0.0));
    assert_eq!(widget(Field::Size), BindValue::Vec2(400.0, 80.0));
    // Half of it is outside the crop.
    let BindValue::Num(p) = widget(Field::Presence) else { panic!() };
    assert!((p - 0.5f64.sqrt()).abs() < 1e-9);
    // The same through a graphic drawn at half size into the output.
    let outer = frame.then(&Rect::new(0.0, 0.0, 200.0, 400.0), &Rect::new(10.0, 10.0, 100.0, 200.0));
    assert_eq!(outer.point((0.0, 50.0)), (10.0, 10.0));
    assert_eq!(outer.rect(&Rect::new(0.0, 50.0, 10.0, 10.0)).w, 20.0);
}

#[test]
fn clicks_ripple_from_the_press_and_end() {
    let f = |t| at(t);
    assert_eq!(sample(Subject::Click { nth: 0, dur: 0.5 }, Field::Presence, &f(0.5)), BindValue::Num(0.0));
    assert_eq!(sample(Subject::Click { nth: 0, dur: 0.5 }, Field::At, &f(1.1)), BindValue::Vec2(20.0, 15.0));
    let BindValue::Num(p) = sample(Subject::Click { nth: 0, dur: 0.5 }, Field::Progress, &f(1.25)) else { panic!() };
    assert!((p - 0.5).abs() < 1e-9);
    assert_eq!(sample(Subject::Click { nth: 0, dur: 0.5 }, Field::Presence, &f(1.6)), BindValue::Num(0.0));
}

#[test]
fn keys_typing_and_steps_give_text() {
    assert_eq!(sample(Subject::Key { hold: 1.0 }, Field::Text, &at(3.4)), BindValue::Str("⌘ S".into()));
    assert_eq!(sample(Subject::Key { hold: 1.0 }, Field::Presence, &at(3.7)), BindValue::Num(1.0));
    assert_eq!(sample(Subject::Typing { pad: 0.0 }, Field::Text, &at(2.25)), BindValue::Str("hi".into()));
    assert_eq!(sample(Subject::Step, Field::Text, &at(2.0)), BindValue::Str("2 / 2".into()));
    assert_eq!(sample(Subject::Step, Field::Index, &at(0.1)), BindValue::Num(0.0));
    let BindValue::Num(age) = sample(Subject::Mark { name: "first".into(), hold: 1.0 }, Field::Age, &at(0.2)) else { panic!() };
    assert!((age + 0.3).abs() < 1e-9);
}

#[test]
fn a_widget_fades_in_when_it_appears() {
    // main.body is listed until frame 35.
    let presence = |t| match sample(Subject::Widget { path: "main.body".into(), pad: 0.0 }, Field::Presence, &at(t)) {
        BindValue::Num(p) => p,
        _ => panic!(),
    };
    assert_eq!(presence(2.0), 1.0);
    assert!(presence(3.45) < 1.0 && presence(3.45) > 0.0);
    assert_eq!(presence(3.6), 0.0);
}

use super::when::*;

#[test]
fn overlays_find_their_moment_in_the_run() {
    let r = run();
    let click = When::Call { call: "click".into(), target: Some("@save".into()), nth: None };
    assert_eq!(click.resolve(&r, 0.3, 0.5).unwrap(), vec![(0.0, 2.0)]);
    let err = When::Call { call: "click".into(), target: Some("cancel".into()), nth: None }.resolve(&r, 0.0, 0.0).unwrap_err();
    assert!(err.contains("click @main.save"), "{err}");
    assert_eq!(When::Between { from: "first".into(), to: "second".into() }.resolve(&r, 0.0, 0.0).unwrap(), vec![(0.5, 2.0)]);
    assert!(When::Mark { name: "nope".into(), hold: 1.0 }.resolve(&r, 0.0, 0.0).unwrap_err().contains("@first, @second"));
    // A clip playing the run from 1 s at double speed, placed at 10 s.
    let clip = ClipTiming { start: 10.0, length: 1.0, source_in: 1.0, speed: 2.0 };
    assert_eq!(clip.edit_span((0.0, 2.0)), Some((10.0, 0.5)));
    assert_eq!(clip.edit_span((0.0, 0.5)), None);
}

/// Real runs (scratch/M4/runs, recorded with the bind section): counter's
/// three clicks on `increment_button` with marks before each and cmd+S,
/// notes composing and typing a note.
const COUNTER: &str = include_str!("fixtures/counter.run.json");
const NOTES: &str = include_str!("fixtures/notes.run.json");

#[test]
fn the_counter_run_binds_its_button_clicks_and_steps() {
    let r = RunData::parse(COUNTER.as_bytes()).unwrap();
    assert!(!r.derived);
    let button = r.widget("increment_button", 1.0).unwrap();
    // Every press lands inside the button it clicked.
    assert_eq!(r.presses.len(), 3);
    for p in &r.presses {
        assert!(p.at.0 >= button.x && p.at.0 <= button.right() && p.at.1 >= button.y && p.at.1 <= button.bottom(), "{p:?} {button:?}");
    }
    // The pointer arrives at the press point by the press.
    let (x, y) = r.cursor(r.presses[0].down as f64 / r.fps).unwrap();
    assert!((x - r.presses[0].at.0).abs() < 1.0 && (y - r.presses[0].at.1).abs() < 1.0);
    // Steps at the marks, a badge for cmd+S.
    assert_eq!(r.step(1.5).unwrap().index, 2);
    assert_eq!(r.shortcut(142.0 / 60.0 + 0.1, 1.0).unwrap().0.label(), "⌘ S");
    let clicks = When::Call { call: "click".into(), target: Some("increment_button".into()), nth: None }.resolve(&r, 0.0, 0.0).unwrap();
    assert_eq!(clicks.len(), 3, "{clicks:?}");
    assert!((clicks[0].0 - 19.0 / 60.0).abs() < 1e-9);
}

#[test]
fn the_notes_run_binds_its_typing() {
    let r = RunData::parse(NOTES.as_bytes()).unwrap();
    let t = r.typing(2.0).unwrap();
    assert!("Groceries\noat milk, lemons, bread".starts_with(&t.text) && !t.text.is_empty(), "{:?}", t.text);
    assert!(t.rect.is_some());
    assert_eq!(r.typing(2.7).unwrap().text, "Groceries\noat milk, lemons, bread");
    // Selecting all after typing is a badge; the Return while typing is not.
    assert_eq!(r.shortcut(164.0 / 60.0 + 0.05, 1.0).unwrap().0.label(), "⌘ A");
    assert!(r.shortcut(1.5, 1.0).is_none());
    // The focus highlight follows the document while typing.
    let (focus, presence) = r.focus(2.0, 0.3).unwrap();
    assert!(presence > 0.99 && focus.w > 0.0);
    // The done button is clicked last.
    let done = r.widget("screen.nav.done", 3.3).unwrap();
    let last = r.presses.last().unwrap();
    assert!(last.at.0 >= done.x && last.at.0 <= done.right());
}

// Overlay documents bound to the recorded runs, evaluated.

use super::tools::*;
use super::{RunBinding, RunContext};
use crate::{InputValue, ItemKind, LiveId, MotionContext, MotionDoc, MotionFrame};
use std::sync::Arc;

fn counter() -> Arc<RunData> {
    Arc::new(RunData::parse(COUNTER.as_bytes()).unwrap())
}

fn notes() -> Arc<RunData> {
    Arc::new(RunData::parse(NOTES.as_bytes()).unwrap())
}

/// An overlay loaded over a run shown whole (both 1080x1920, so a document
/// unit is a recording pixel).
fn bound(id: &str, data: Arc<RunData>, params: &[(&str, InputValue)]) -> MotionDoc {
    let context = RunContext { runs: vec![RunBinding::standalone("demo", data, [1080.0, 1920.0], 0.0)] }.apply(MotionContext::default());
    let params: Vec<(LiveId, InputValue)> = params.iter().map(|(k, v)| (LiveId::from_str(k), v.clone())).collect();
    let doc = MotionDoc::load_with(overlay(id).unwrap().source, &params, context).unwrap_or_else(|d| panic!("{id}: {d:#?}"));
    assert!(doc.warnings.iter().all(|w| w.severity != crate::Severity::Error), "{id}: {:#?}", doc.warnings);
    doc
}

/// A top-level item by its layer's `name`.
fn item<'a>(doc: &MotionDoc, frame: &'a MotionFrame, name: &str) -> &'a crate::Item {
    let index = doc.layers.iter().position(|l| l.name.as_deref() == Some(name)).unwrap_or_else(|| panic!("no layer {name}"));
    let path = format!("layers[{index}]");
    frame.items.iter().find(|i| i.path == path).unwrap_or_else(|| panic!("{name} not drawn: {:#?}", frame.items.iter().map(|i| &i.path).collect::<Vec<_>>()))
}

fn clean(frame: &MotionFrame) {
    assert!(frame.errors.is_empty(), "{:#?}", frame.errors);
}

#[test]
fn every_overlay_loads_and_evaluates_bound_and_unbound() {
    for o in OVERLAYS {
        let doc = bound(o.id, notes(), &[("target", InputValue::Text("screen.document".into()))]);
        for t in [0.0, 1.0, 2.0, 3.5] {
            clean(&doc.eval(t));
        }
        // Without a run: it loads, and shows nothing.
        let alone = MotionDoc::load(o.source).unwrap_or_else(|d| panic!("{}: {d:#?}", o.id));
        let frame = alone.eval(1.0);
        clean(&frame);
        assert!(frame.items.iter().all(|i| i.opacity == 0.0 || matches!(i.kind, ItemKind::Primitive(PrimitiveFrame::Text { .. }) | ItemKind::Shader { .. })), "{}: {:#?}", o.id, frame.items);
    }
}

#[test]
fn the_highlight_sits_on_the_widget_it_names() {
    let r = counter();
    let doc = bound("highlight", r.clone(), &[("target", InputValue::Text("increment_button".into())), ("pad", InputValue::Number(10.0))]);
    let frame = doc.eval(0.7);
    clean(&frame);
    let b = item(&doc, &frame, "box");
    let w = r.widget("increment_button", 0.7).unwrap();
    assert!((b.world.e - (w.x - 10.0)).abs() < 1e-6 && (b.world.f - (w.y - 10.0)).abs() < 1e-6, "{:?} {w:?}", b.world);
    let ItemKind::Primitive(PrimitiveFrame::Rect { size, .. }) = b.kind else { panic!() };
    assert!((size[0] - (w.w + 20.0)).abs() < 1e-6);
    assert!((b.opacity - 1.0).abs() < 1e-9);
}

#[test]
fn a_ripple_starts_on_each_real_press() {
    let r = counter();
    let doc = bound("ripple", r.clone(), &[]);
    for press in &r.presses {
        let t = press.down as f64 / r.fps + 0.1;
        let frame = doc.eval(t);
        clean(&frame);
        let ring = item(&doc, &frame, "ring");
        let ItemKind::Primitive(PrimitiveFrame::Ellipse { size, .. }) = ring.kind else { panic!() };
        let centre = [ring.world.e + size[0] * 0.5, ring.world.f + size[1] * 0.5];
        assert!((centre[0] - press.at.0).abs() < 1e-6 && (centre[1] - press.at.1).abs() < 1e-6);
        assert!(ring.opacity > 0.3);
    }
    // Between clicks nothing shows.
    let quiet = doc.eval(0.2);
    assert!(quiet.items.iter().all(|i| i.opacity == 0.0));
}

#[test]
fn keystroke_badges_and_step_counters_show_the_runs_text() {
    let r = counter();
    let keys = bound("keys", r.clone(), &[]);
    let at = 142.0 / 60.0 + 0.3;
    let frame = keys.eval(at);
    clean(&frame);
    let ItemKind::Primitive(PrimitiveFrame::Text { text, units, .. }) = &item(&keys, &frame, "label").kind else { panic!() };
    assert!(text.contains("⌘ S"), "{text:?}");
    assert!(units.iter().any(|u| u.opacity > 0.5));
    assert!(item(&keys, &frame, "cap").opacity > 0.9);
    let steps = bound("steps", r, &[]);
    let frame = steps.eval(1.5);
    clean(&frame);
    let ItemKind::Primitive(PrimitiveFrame::Text { text, units, .. }) = &item(&steps, &frame, "count").kind else { panic!() };
    let shown: String = units.iter().filter(|u| u.opacity > 0.5).map(|u| &text[u.range.clone()]).collect();
    assert_eq!(shown, "2 / 3");
}

#[test]
fn the_overlays_follow_a_moving_crop() {
    // A Reframe that pans right by 200 px over 2 s at 2x: the highlight
    // moves left by 400 document units and doubles in size.
    let r = counter();
    let w = r.widget("increment_button", 0.0).unwrap();
    let crop0 = Rect::new(600.0, 0.0, 480.0, 960.0);
    let crop1 = Rect::new(400.0, 0.0, 480.0, 960.0);
    let dst = Rect::new(0.0, 0.0, 960.0, 1920.0);
    let frames: Vec<BindFrame> = (0..=20).map(|k| {
        let u = k as f64 / 20.0;
        BindFrame { run_time: 2.0 * u, crop: crop0.lerp(&crop1, u), dst }
    }).collect();
    let binding = RunBinding { name: "demo".into(), data: r, start: 0.0, rate: 10.0, frames };
    let context = RunContext { runs: vec![binding] }.apply(MotionContext::default());
    let doc = MotionDoc::load_with(overlay("highlight").unwrap().source, &[(LiveId::from_str("target"), InputValue::Text("increment_button".into())), (LiveId::from_str("pad"), InputValue::Number(0.0))], context).unwrap();
    let x = |t: f64| item(&doc, &doc.eval(t), "box").world.e;
    assert!((x(0.0) - (w.x - 600.0) * 2.0).abs() < 1e-6);
    assert!((x(1.0) - (w.x - 500.0) * 2.0).abs() < 1e-6);
    let frame = doc.eval(1.0);
    let ItemKind::Primitive(PrimitiveFrame::Rect { size, .. }) = item(&doc, &frame, "box").kind else { panic!() };
    assert!((size[0] - w.w * 2.0).abs() < 1e-6);
}

#[test]
fn text_voices_sit_on_the_documents_clock() {
    // The run starts 1 s into the document: its steps start 1 s later too.
    let r = counter();
    let binding = RunBinding::standalone("", r.clone(), [1080.0, 1920.0], -1.0);
    let voices = binding.voices(1.2);
    let steps = &voices.iter().find(|(id, _)| id == "run_steps").unwrap().1;
    assert_eq!(steps.words.len(), 3);
    assert!((steps.words[1].start - (68.0 / 60.0 + 1.0)).abs() < 1e-9);
    let typed = RunBinding::standalone("", notes(), [1080.0, 1920.0], 0.0).voices(1.0);
    let words: Vec<&str> = typed.iter().find(|(id, _)| id == "run_typed").unwrap().1.words.iter().map(|w| w.text.as_str()).collect();
    assert_eq!(words, ["Groceries", "oat", "milk,", "lemons,", "bread"]);
}

#[test]
fn the_ai_places_a_highlight_when_save_is_clicked() {
    let r = counter();
    // "highlight the increment button when it's clicked"
    let args = makepad_strict_json::parse(br#"{"overlay": "highlight", "clip": "demo", "target": "increment_button"}"#).unwrap();
    let request = request_from_json(&args).unwrap();
    let clip = ClipTiming { start: 4.0, length: 3.0, source_in: 0.0, speed: 1.0 };
    let placed = place(&request, &r, clip).unwrap();
    // Three clicks 0.8 s apart: with the lead and tail they make one stretch.
    // The first click is 0.32 s into the run: its lead is cut at the start.
    assert_eq!(placed.spans.len(), 1);
    assert!((placed.spans[0].0 - 4.0).abs() < 1e-9);
    // It would end 0.8 s after the last click (7.17 s); the clip ends at 7.
    assert!((placed.spans[0].0 + placed.spans[0].1 - 7.0).abs() < 1e-9);
    // Only the second click, without a lead.
    let second = OverlayRequest { when: Some(When::Call { call: "click".into(), target: Some("increment_button".into()), nth: Some(1) }), lead: Some(0.0), ..request.clone() };
    let placed2 = place(&second, &r, clip).unwrap();
    assert!((placed2.spans[0].0 - (4.0 + 68.0 / 60.0)).abs() < 1e-9);
    assert_eq!(placed.params, vec![("target".to_string(), "\"body.increment_button\"".to_string())]);
    let node = graphic_splash("hl1", "builtin(\"overlay/highlight\")", "demo", placed.spans[0], &placed.params);
    assert!(node.starts_with("let hl1 = Graphic{src: builtin(\"overlay/highlight\") span: [") && node.contains("bind: @demo params: {target: \"body.increment_button\"}"), "{node}");
    // A wrong target names what the run has.
    let bad = OverlayRequest { target: Some("save".into()), ..request.clone() };
    let err = place(&bad, &r, clip).unwrap_err();
    assert!(err.contains("increment_button"), "{err}");
    // A callout needs a target.
    let callout = OverlayRequest { overlay: "callout".into(), target: None, ..request };
    assert!(place(&callout, &r, clip).unwrap_err().contains("needs a target"));
    // The facts the AI reads first.
    let facts = run_facts(&r).to_json();
    assert!(facts.contains("\"step2\"") && facts.contains("\"⌘ S\"") && facts.contains("body.increment_button"), "{facts}");
    assert_eq!(tool_defs().len(), 2);
    for (_, _, schema) in tool_defs() {
        makepad_strict_json::parse(schema.as_bytes()).unwrap();
    }
}
