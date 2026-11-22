//! `fab.strength_plan` reads only: it says the way up, the limb turns and what stays weak, as the
//! tools it's built from say them.

use crate::{RenderRequest, Session};
use fab_model::FabModel;
use fab_plate::machine::Machine;
use makepad_model::json;
use makepad_script::syntax::{parse_value, Node};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Standing up: a thin neck carrying a head, and arms a little off flat under a turn onto its back.
const FIGURE: &str = "let m = mod.model
let torso = m.box({size: [30mm 50mm 20mm] at: [0 60mm 0]})
let head = m.sphere({radius: 11mm at: [0 102mm 0]})
let neck = m.capsule({a: [0 80mm 0] b: [0 96mm 0] radius: 3mm})
let armL = m.capsule({a: [-14mm 78mm 0] b: [-50mm 78mm 6mm] radius: 3mm})
let armR = m.capsule({a: [14mm 78mm 0] b: [50mm 78mm 6mm] radius: 3mm})
let legL = m.capsule({a: [-8mm 40mm 0] b: [-8mm 2mm 0] radius: 3.5mm})
let legR = m.capsule({a: [8mm 40mm 0] b: [8mm 2mm 0] radius: 3.5mm})
object(m.blend([torso head neck armL armR legL legR], 1mm), name: \"figure\")
";

fn plate(script: &str) -> Session {
    let mut session = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    let scripts = BTreeMap::from([("shape.cad.splash".to_string(), format!("use mod.model.*\nuse mod.fab.*\n{script}"))]);
    let project = "use mod.fab.*\nsettings({layer_height: \"0.2\"})\nscript(\"shape.cad.splash\")\nplace(\"shape\", at: [170mm 170mm])\n".to_string();
    session.model = FabModel::load("plan", &project, scripts).unwrap_or_else(|f| panic!("{f}"));
    session.rebuild().unwrap_or_else(|e| panic!("{e}"));
    session
}

fn items<'a>(node: &'a Node, key: &str) -> &'a [Node] {
    node.field(key).and_then(|f| f.value.items()).unwrap_or_else(|| panic!("no {key}: {node:?}"))
}

fn text<'a>(node: &'a Node, key: &str) -> &'a str {
    node.field(key).and_then(|f| f.value.string()).unwrap_or_else(|| panic!("no {key}"))
}

fn number(node: &Node, key: &str) -> f64 {
    node.field(key).and_then(|f| f.value.number()).unwrap_or_else(|| panic!("no {key}"))
}

/// A comb of many thin teeth has more to say than a reply holds: the way up and the words are all
/// there, and the list of spots says what it left out.
#[test]
fn a_plan_fits_a_reply_and_keeps_its_words() {
    let mut script = String::from("let m = mod.model\nlet bar = m.box({size: [150mm 16mm 16mm] at: [0 8mm 0]})\n");
    let mut names = vec!["bar".to_string()];
    for i in 0..24 {
        let x = -69.0 + 6.0 * i as f64;
        script.push_str(&format!("let tooth{i} = m.capsule({{a: [{x}mm 16mm 0] b: [{x}mm 40mm 0] radius: 1.6mm}})\n"));
        names.push(format!("tooth{i}"));
    }
    script.push_str(&format!("object(m.blend([{}], 0.5mm), name: \"comb\")\n", names.join(" ")));
    let mut s = plate(&script);
    let engine = Arc::new(crate::supports_tests::EngineSlices::default());
    s.slices = Some(engine.clone());
    let reply = s.call(super::TOOL, &json::parse(b"{}").unwrap(), None);
    assert!(reply.ok, "{}", reply.text);
    let Some(RenderRequest::StrengthPlan(job)) = &reply.render else { panic!("it runs off the model worker: {}", reply.text) };
    let answer = crate::out::text(&job.answer(&|| false).unwrap_or_else(|why| panic!("{why}")));
    assert!(answer.len() <= makepad_ai_services::wire::MAX_RESULT_BYTES, "{} bytes", answer.len());
    let plan = parse_value(&answer).unwrap_or_else(|e| panic!("{e:?}: {answer}"));
    assert!(!text(&plan, "says").is_empty(), "{answer}");
    assert!(plan.field("orientation").is_some(), "{answer}");
    assert!(!items(&plan, "spots").is_empty() && !items(&plan, "tubes").is_empty(), "{answer}");
    assert!(text(&plan, "left_out").contains("fab.weak_spots"), "{answer}");
    let found = s.weak_memo.lock().unwrap().clone().expect("the plan's list of spots");
    // the Slicer tab reads every row, of the same plan: not the same find and search again
    let reply = s.call(super::TOOL, &json::parse(b"{\"whole\":true}").unwrap(), None);
    let Some(RenderRequest::StrengthPlan(job)) = &reply.render else { panic!("it runs off the model worker: {}", reply.text) };
    let whole = crate::out::text(&job.answer(&|| false).unwrap_or_else(|why| panic!("{why}")));
    assert!(s.weak_memo.lock().unwrap().as_ref().is_some_and(|again| Arc::ptr_eq(again, &found)), "whole: the first plan's list, not a second find");
    let all = parse_value(&whole).unwrap_or_else(|e| panic!("{e:?}: {whole}"));
    assert!(all.field("left_out").is_none(), "whole: nothing left out");
    assert!(items(&all, "spots").len() > items(&plan, "spots").len(), "whole: every spot");
}

/// With no slice it waits for one; it changes nothing and has the whole plan in it.
#[test]
fn a_plan_reads_the_part_and_changes_nothing() {
    let mut s = plate(FIGURE);
    let engine = Arc::new(crate::supports_tests::EngineSlices::default());
    s.slices = Some(engine.clone());
    let script = s.model.source("shape.cad.splash").map(str::to_string);
    let project = s.model.source("plan.fab.splash").map(str::to_string);
    let head = s.model.head();
    let reply = s.call(super::TOOL, &json::parse(br#"{"min_thickness": 9}"#).unwrap(), None);
    assert!(reply.ok, "{}", reply.text);
    let Some(RenderRequest::StrengthPlan(job)) = &reply.render else { panic!("it runs off the model worker: {}", reply.text) };
    let answer = crate::out::text(&job.answer(&|| false).unwrap_or_else(|why| panic!("{why}")));
    let plan = parse_value(&answer).unwrap_or_else(|e| panic!("{e:?}: {answer}"));
    assert_eq!(s.model.head(), head, "a read: {answer}");
    assert_eq!(s.model.source("shape.cad.splash").map(str::to_string), script);
    assert_eq!(s.model.source("plan.fab.splash").map(str::to_string), project);
    // the tubes, the neck and legs stood up
    let tubes = items(&plan, "tubes");
    let tube = |name: &str| tubes.iter().find(|t| text(t, "name") == name).unwrap_or_else(|| panic!("no tube {name}: {answer}"));
    assert!(number(tube("neck"), "angle_now") > 80.0, "{answer}");
    assert!(number(tube("legL"), "angle_now") > 80.0, "{answer}");
    assert!(number(tube("neck"), "angle_after") < 15.0, "the way up lays them flat: {answer}");
    assert!(number(tube("neck"), "gain_estimate") > 1.0, "and says what that gains: {answer}");
    assert!(!items(&plan, "spots").is_empty(), "the weak spots are in it: {answer}");
    // the table and its turns
    let orientation = &plan.field("orientation").expect("the table").value;
    assert!(items(orientation, "rows").len() >= 2, "{answer}");
    assert!(orientation.field("align").is_some() && orientation.field("align_as_it_stands").is_some(), "{answer}");
    assert!(plan.field("stays_weak").is_some(), "{answer}");
    assert!(!text(&plan, "says").is_empty(), "{answer}");
}
