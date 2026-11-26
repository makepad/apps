//! `fab.weak_spots` and `fab.strengthen`: the list reads a figure's limbs and stays quiet on a block,
//! a fix that would change nothing isn't offered, and every pick goes in as one undo step that gives
//! back the same program and the same G-code.

use crate::supports::SliceSource;
use crate::supports_tests::EngineSlices;
use crate::{out, RenderRequest, Session};
use fab_model::FabModel;
use fab_plate::machine::Machine;
use makepad_model::json::{self, Value};
use makepad_script::syntax::{parse_value, Node};
use std::collections::BTreeMap;
use std::sync::Arc;

/// A figure built of named features.
pub(crate) const FIGURE: &str = "let m = mod.model
let torso = m.box({size: [30mm 50mm 20mm] at: [0 60mm 0]})
let head = m.sphere({radius: 11mm at: [0 102mm 0]})
let neck = m.cylinder({radius: 4mm height: 14mm at: [0 88mm 0]})
let armL = m.capsule({a: [-14mm 78mm 0] b: [-46mm 56mm 0] radius: 4mm})
let armR = m.capsule({a: [14mm 78mm 0] b: [46mm 56mm 0] radius: 4mm})
let legL = m.capsule({a: [-8mm 40mm 0] b: [-10mm 4mm 0] radius: 5mm})
let legR = m.capsule({a: [8mm 40mm 0] b: [10mm 4mm 0] radius: 5mm})
let figure = m.blend([torso head neck armL armR legL legR], 1mm)
object(figure, name: \"figure\")
";
const BLOCK: &str = "let m = mod.model\nobject(m.box({size: [40mm 40mm 40mm] at: [0 20mm 0]}), name: \"block\")\n";
/// A body and an arm on a diagonal, with no names.
const NAMELESS: &str = "let m = mod.model
object(m.blend([m.box({size: [40mm 80mm 24mm] at: [0 40mm 0]}) m.capsule({a: [15mm 50mm 0] b: [60mm 80mm 0] radius: 5mm})], 1mm), name: \"figure\")
";

pub(crate) fn plate(script: &str) -> (Session, Arc<EngineSlices>) {
    let mut session = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    let scripts = BTreeMap::from([("shape.cad.splash".to_string(), format!("use mod.model.*\nuse mod.fab.*\n{script}"))]);
    let project = "use mod.fab.*\nsettings({layer_height: \"0.2\"})\nscript(\"shape.cad.splash\")\nplace(\"shape\", at: [150mm 150mm])\n";
    session.model = FabModel::load("strength", project, scripts).unwrap_or_else(|f| panic!("{f}"));
    session.rebuild().unwrap_or_else(|e| panic!("{e}"));
    let engine = Arc::new(EngineSlices::default());
    session.slices = Some(engine.clone());
    (session, engine)
}

pub(crate) fn slice(s: &Session, engine: &EngineSlices) {
    let (input, _) = engine.input(s.last.as_ref().unwrap()).unwrap();
    engine.sliced(&input, None).unwrap();
}

fn gcode(s: &Session, engine: &EngineSlices) -> String {
    let (input, _) = engine.input(s.last.as_ref().unwrap()).unwrap();
    EngineSlices::settled(&input).unwrap().gcode.text().expect("the settled G-code").to_string()
}

/// The find as a host runs it: off the worker, then the answer.
pub(crate) fn find(s: &mut Session, args: &str) -> Node {
    let reply = s.call(super::FIND_TOOL, &json::parse(args.as_bytes()).unwrap(), None);
    assert!(reply.ok, "{}", reply.text);
    let text = match reply.render {
        Some(RenderRequest::WeakSpots(job)) => out::text(&job.answer(&|| false).unwrap()),
        _ => reply.text,
    };
    parse_value(&text).unwrap_or_else(|e| panic!("{e:?}: {text}"))
}

pub(crate) fn text_of<'a>(n: &'a Node, key: &str) -> &'a str {
    n.field(key).and_then(|f| f.value.string()).unwrap_or_else(|| panic!("no {key} in {n:?}"))
}

fn number_of(n: &Node, key: &str) -> f64 {
    n.field(key).and_then(|f| f.value.number()).unwrap_or_else(|| panic!("no {key} in {n:?}"))
}

pub(crate) fn spots(answer: &Node) -> Vec<Node> {
    answer.field("spots").and_then(|f| f.value.items()).unwrap_or_else(|| panic!("no spots: {answer:?}")).to_vec()
}

fn fixes_of(spot: &Node) -> Vec<String> {
    spot.field("fixes").and_then(|f| f.value.items()).unwrap().iter().map(|f| text_of(f, "fix").to_string()).collect()
}

fn head(s: &Session) -> String {
    s.model.head().text()
}

fn source(s: &Session) -> String {
    s.source("shape.cad.splash").unwrap_or_default().to_string()
}

pub(crate) fn strengthen(s: &mut Session, fixes: &str) -> crate::Reply {
    let args = format!(r#"{{"expected":"{}","fixes":{fixes}}}"#, head(s));
    s.call(super::TOOL, &json::parse(args.as_bytes()).unwrap(), None)
}

fn undo(s: &mut Session) {
    let h = head(s);
    let args = Value::Obj(vec![("expected".into(), Value::Str(h)), ("action".into(), Value::Str("undo".into()))]);
    let reply = s.call("history", &args, None);
    assert!(reply.ok, "{}", reply.text);
}

/// "What would break first on this?": a figure's arms, legs and neck, worst first, each saying what
/// it is, how thin, what prints there and what can be done; a block has nothing to say.
#[test]
fn a_figures_limbs_are_listed_worst_first_and_a_block_says_nothing() {
    let (mut s, _) = plate(FIGURE);
    let answer = find(&mut s, r#"{"min_thickness":12}"#);
    let rows = spots(&answer);
    let names: Vec<&str> = rows.iter().map(|r| text_of(r, "where")).collect();
    for wanted in ["armL", "armR", "neck", "legL", "legR"] {
        assert!(names.contains(&wanted), "{wanted} in {names:?}");
    }
    let scores: Vec<f64> = rows.iter().map(|r| number_of(r, "score")).collect();
    assert!(scores.windows(2).all(|w| w[0] >= w[1]), "worst first: {scores:?}");
    let neck = rows.iter().find(|r| text_of(r, "where") == "neck").unwrap();
    assert!((number_of(neck, "thickness") - 8.0).abs() < 1.0, "the neck's 8 mm: {neck:?}");
    let there = &neck.field("there").unwrap().value;
    assert!(!text_of(there, "walls").is_empty(), "what's there now: {there:?}");
    assert_eq!(fixes_of(neck), ["solid_infill", "thicken"], "both can be done here");
    let (mut block, engine) = plate(BLOCK);
    slice(&block, &engine);
    let quiet = find(&mut block, r#"{"min_thickness":12}"#);
    assert!(spots(&quiet).is_empty(), "no false alarm: {quiet:?}");
}

/// A part that's solid there already isn't offered solid infill, and asking anyway says why.
#[test]
fn a_fix_that_would_change_nothing_is_not_offered() {
    let (mut s, engine) = plate(&format!("{FIGURE}m.settings(neck, {{sparse_infill_density: \"100%\"}})\n"));
    slice(&s, &engine);
    let answer = find(&mut s, r#"{"min_thickness":12}"#);
    let rows = spots(&answer);
    let neck = rows.iter().find(|r| text_of(r, "where") == "neck").unwrap();
    assert_eq!(text_of(&neck.field("there").unwrap().value, "infill"), "100%", "{neck:?}");
    let id = text_of(neck, "id");
    assert!(!fixes_of(neck).contains(&"solid_infill".to_string()), "{neck:?}");
    let why = strengthen(&mut s, &format!(r#"[{{"spot":"{id}","fix":"solid_infill"}}]"#));
    assert!(!why.ok && why.text.contains("changes nothing"), "{}", why.text);
}

/// Solid infill and thicken together are one undo step: the program comes back byte for byte and so
/// does the G-code. The new slice says what they did.
#[test]
fn fixes_are_one_undo_and_say_what_they_did() {
    let (mut s, engine) = plate(FIGURE);
    slice(&s, &engine);
    let (before, code) = (source(&s), gcode(&s, &engine));
    let answer = find(&mut s, r#"{"min_thickness":12}"#);
    let rows = spots(&answer);
    let id = |word: &str| text_of(rows.iter().find(|r| text_of(r, "where") == word).unwrap(), "id").to_string();
    let picks = format!(r#"[{{"spot":"{}","fix":"solid_infill"}},{{"spot":"{}","fix":"solid_infill"}},{{"spot":"{}","fix":"thicken","to":12}}]"#, id("armL"), id("armR"), id("neck"));
    let reply = strengthen(&mut s, &picks);
    assert!(reply.ok, "{}", reply.text);
    let written = source(&s);
    assert!(written.contains("m.settings(armL_settings") && written.contains("let figure_base = m.blend(") && written.contains("let figure = m.blend([figure_base m.capsule("), "{written}");
    slice(&s, &engine);
    let after = find(&mut s, r#"{"min_thickness":12}"#);
    let fixed = after.field("fixed").unwrap_or_else(|| panic!("no before and after: {after:?}")).value.clone();
    let lines = fixed.field("spots").and_then(|f| f.value.items()).unwrap();
    let neck = lines.iter().find(|l| text_of(l, "where") == "neck").unwrap();
    assert!((number_of(neck, "thickness_before") - 8.0).abs() < 1.0);
    let arm = lines.iter().find(|l| text_of(l, "where") == "armL").unwrap();
    assert_eq!(text_of(arm, "after"), "solid infill placed", "solid infill claims no thickness: {arm:?}");
    if text_of(neck, "after") == "still under the minimum" {
        assert!(number_of(neck, "thickness_after") >= 11.4, "thickened to its target, to a line width: {neck:?}");
    }
    assert!(number_of(&fixed, "plate_mm3_after") > number_of(&fixed, "plate_mm3_before"), "it adds material");
    undo(&mut s);
    assert_eq!(source(&s), before, "one undo: the program as it was");
    assert!(gcode(&s, &engine) == code, "and the G-code");
}

/// A place is named by the feature it's in. An ellipsoid's field reads short far from it, so a point
/// deep in a leg reads nearer an ear than the head the ear sits on: all a feature's painted region
/// asks there, and the smallest thing to hold every point of the part was the ear.
#[test]
fn a_spot_is_named_by_its_own_feature_not_the_smallest() {
    let eared = FIGURE.replace("let figure = m.blend([torso head neck armL armR legL legR], 1mm)", "let earR = m.ellipsoid({radii: [1.2mm 3.4mm 2.2mm] at: [11mm 102mm 0]})\nlet figure = m.blend([torso head neck armL armR legL legR earR], 1mm)");
    let (mut s, engine) = plate(&eared);
    slice(&s, &engine);
    let list = spots(&find(&mut s, r#"{"min_thickness":12}"#));
    let names: Vec<&str> = list.iter().map(|spot| text_of(spot, "where")).collect();
    for name in ["neck", "armL", "armR", "legL", "legR"] {
        assert!(names.contains(&name), "{name} in {names:?}");
    }
    assert!(!names.contains(&"earR"), "{names:?}");
}

/// Thickening a part that lies at an angle leaves it on the bed: what's added is geometry, and its
/// bounds are as tight as the rest of the body's.
#[test]
fn a_turned_part_thickened_stays_on_the_bed() {
    let (mut s, engine) = plate(FIGURE);
    let args = format!(r#"{{"apply":true,"expected":"{}","up":[0.5,0.3,0.81],"spin":0}}"#, head(&s));
    let reply = s.call(crate::tubes::ORIENT_TOOL, &json::parse(args.as_bytes()).unwrap(), None);
    assert!(reply.ok, "{}", reply.text);
    slice(&s, &engine);
    let list = spots(&find(&mut s, r#"{"min_thickness":12}"#));
    let neck = list.iter().find(|spot| text_of(spot, "where") == "neck").unwrap_or_else(|| panic!("no neck in {list:?}"));
    let reply = strengthen(&mut s, &format!(r#"[{{"spot":"{}","fix":"thicken","to":12}}]"#, text_of(neck, "id")));
    assert!(reply.ok, "{}", reply.text);
    let ev = s.last.as_ref().unwrap();
    let gap = ev.fields[0].as_ref().and_then(|f| f.bottom_gap);
    assert!(gap.is_some_and(|gap| gap < 0.1), "its bottom is proven close: {gap:?}");
    assert!(!ev.warnings.iter().any(|w| w.contains("into the bed")), "{:?}", ev.warnings);
}

/// A model with no names: a spot's fix places a capsule round the limb and holds to it.
#[test]
fn a_nameless_limb_takes_a_capsule() {
    let (mut s, engine) = plate(NAMELESS);
    slice(&s, &engine);
    let answer = find(&mut s, r#"{"min_thickness":14}"#);
    let rows = spots(&answer);
    assert_eq!(rows.len(), 1, "the arm: {rows:?}");
    assert!(text_of(&rows[0], "where").starts_with("limb near"), "{rows:?}");
    let id = text_of(&rows[0], "id").to_string();
    let reply = strengthen(&mut s, &format!(r#"[{{"spot":"{id}","fix":"solid_infill"}}]"#));
    assert!(reply.ok, "{}", reply.text);
    assert!(source(&s).contains("m.capsule({a: ["), "{}", source(&s));
}

/// A stale list isn't acted on, and a word it doesn't know says what it knows.
#[test]
fn a_stale_list_and_an_unknown_spot_say_what_to_do() {
    let (mut s, engine) = plate(FIGURE);
    slice(&s, &engine);
    find(&mut s, r#"{"min_thickness":12}"#);
    let why = strengthen(&mut s, r#"[{"spot":"spot99","fix":"solid_infill"}]"#);
    assert!(!why.ok && why.text.contains("spot1"), "{}", why.text);
    let edit = format!(r#"{{"expected":"{}","targets":["armL"]}}"#, head(&s));
    assert!(s.call(crate::volumes::TOOL, &json::parse(edit.as_bytes()).unwrap(), None).ok);
    let why = strengthen(&mut s, r#"[{"spot":"spot1","fix":"solid_infill"}]"#);
    assert!(!why.ok && why.text.contains("older slice"), "{}", why.text);
}
