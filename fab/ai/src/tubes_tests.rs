//! `fab.tubes` and `fab.orient` on generic shapes: limbs found named or not, ways up that lay them
//! flat with what stays said out loud, least support, and one undo for a move, measured again from
//! the new slice.

use crate::{RenderRequest, Session};
use fab_model::FabModel;
use fab_plate::machine::Machine;
use makepad_model::json::{self, Value};
use makepad_script::syntax::{parse_value, Node};
use std::collections::BTreeMap;
use std::sync::Arc;

const UPRIGHT: &str = "let m = mod.model\nobject(m.cylinder({radius: 5mm height: 60mm at: [0 30mm 0]}), name: \"post\")\n";
const ELL: &str = "let m = mod.model\nlet up = m.capsule({a: [0 5mm 0] b: [0 60mm 0] radius: 5mm})\nlet out = m.capsule({a: [0 5mm 0] b: [60mm 5mm 0] radius: 5mm})\nobject(m.union(up, out), name: \"ell\")\n";
const JACK: &str = "let m = mod.model\nlet alongx = m.capsule({a: [-40mm, 40mm, 0] b: [40mm 40mm 0] radius: 5mm})\nlet alongy = m.capsule({a: [0 0 0] b: [0 80mm 0] radius: 5mm})\nlet alongz = m.capsule({a: [0, 40mm, -40mm] b: [0 40mm 40mm] radius: 5mm})\nobject(m.union(alongx, m.union(alongy, alongz)), name: \"jack\")\n";
const NAMELESS: &str = "let m = mod.model\nobject(m.union(m.box({size: [40mm 80mm 24mm] at: [0 40mm 0]}), m.capsule({a: [15mm 50mm 0] b: [60mm 80mm 0] radius: 5mm})), name: \"figure\")\n";
const FIGURE: &str = "let m = mod.model
let torso = m.box({size: [30mm 50mm 20mm] at: [0 60mm 0]})
let head = m.sphere({radius: 11mm at: [0 102mm 0]})
let neck = m.cylinder({radius: 4mm height: 14mm at: [0 88mm 0]})
let armL = m.capsule({a: [-14mm 78mm 0] b: [-46mm 56mm 0] radius: 4mm})
let armR = m.capsule({a: [14mm 78mm 0] b: [46mm 56mm 0] radius: 4mm})
let legs = m.blend([m.capsule({a: [-8mm 40mm 0] b: [-10mm 4mm 0] radius: 5mm}) m.capsule({a: [8mm 40mm 0] b: [10mm 4mm 0] radius: 5mm})], 1mm)
object(m.blend([torso head neck armL armR legs], 1mm), name: \"figure\")
";
/// A slim figure with no names at all: at the share that finds a thick limb its whole trunk is thin.
const SLIM: &str = "let m = mod.model
object(m.blend([m.box({size: [30mm 50mm 14mm] at: [0 60mm 0]}) m.sphere({radius: 11mm at: [0 102mm 0]}) m.capsule({a: [0 80mm 0] b: [0 96mm 0] radius: 3mm}) m.capsule({a: [-12mm 80mm 0] b: [-50mm 72mm 0] radius: 3mm}) m.capsule({a: [12mm 80mm 0] b: [50mm 72mm 0] radius: 3mm}) m.capsule({a: [-8mm 40mm 0] b: [-8mm 2mm 0] radius: 3mm}) m.capsule({a: [8mm 40mm 0] b: [8mm 2mm 0] radius: 3mm})], 1mm), name: \"figure\")
";
/// A top on four legs, its legs short.
const TABLE: &str = "let m = mod.model
let top = m.box({size: [60mm 4mm 60mm] at: [0 42mm 0]})
let legs = m.union(m.union(m.box({size: [6mm 40mm 6mm] at: [-25mm, 20mm, -25mm]}), m.box({size: [6mm 40mm 6mm] at: [25mm, 20mm, -25mm]})), m.union(m.box({size: [6mm 40mm 6mm] at: [-25mm, 20mm, 25mm]}), m.box({size: [6mm 40mm 6mm] at: [25mm 20mm 25mm]})))
object(m.union(top, legs), name: \"table\")
";
/// A half ball on its flat side.
const DOME: &str = "let m = mod.model\nobject(m.intersect(m.sphere({radius: 30mm at: [0 0 0]}), m.box({size: [80mm 40mm 80mm] at: [0 20mm 0]})), name: \"dome\")\n";

const SUPPORTS: &str = "enable_support: \"1\" support_type: \"tree(auto)\" support_threshold_angle: \"45\"";

fn plate_with(script: &str, settings: &str) -> Session {
    placed(script, &format!("layer_height: \"0.2\" {settings}"), "")
}

/// Least support plans the supports of every way up it weighs, each on a slice of its own: its
/// plates hold the shape at half its size on 0.3 mm layers, where it weighs the same ways up as
/// at full size.
fn least_support_plate(script: &str) -> Session {
    placed(script, &format!("layer_height: \"0.3\" {SUPPORTS}"), ", scale: 0.5")
}

fn placed(script: &str, settings: &str, place: &str) -> Session {
    let mut session = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    let scripts = BTreeMap::from([("shape.cad.splash".to_string(), format!("use mod.model.*\nuse mod.fab.*\n{script}"))]);
    let project = format!("use mod.fab.*\nsettings({{{settings}}})\nscript(\"shape.cad.splash\")\nplace(\"shape\", at: [170mm 170mm]{place})\n");
    session.model = FabModel::load("tubes", &project, scripts).unwrap_or_else(|f| panic!("{f}"));
    session.rebuild().unwrap_or_else(|e| panic!("{e}"));
    session
}

fn plate(script: &str) -> Session {
    plate_with(script, "")
}

fn tubes(s: &mut Session) -> Node {
    let reply = s.call(super::TUBES_TOOL, &Value::Obj(Vec::new()), None);
    assert!(reply.ok, "{}", reply.text);
    let Some(RenderRequest::Tubes(job)) = &reply.render else { panic!("a find runs off the model worker: {}", reply.text) };
    let answer = job.answer(&|| false).unwrap_or_else(|why| panic!("{why}"));
    parse_value(&crate::out::text(&answer)).unwrap()
}

fn orient(s: &mut Session, text: &str) -> Node {
    let reply = s.call(super::ORIENT_TOOL, &json::parse(text.as_bytes()).unwrap(), None);
    assert!(reply.ok, "{}", reply.text);
    parse_value(&reply.text).unwrap_or_else(|e| panic!("{e:?}: {}", reply.text))
}

fn number(node: &Node, key: &str) -> f64 {
    node.field(key).and_then(|f| f.value.number()).unwrap_or_else(|| panic!("no {key}"))
}

fn text<'a>(node: &'a Node, key: &str) -> &'a str {
    node.field(key).and_then(|f| f.value.string()).unwrap_or_else(|| panic!("no {key}"))
}

fn items<'a>(node: &'a Node, key: &str) -> &'a [Node] {
    node.field(key).and_then(|f| f.value.items()).unwrap_or_else(|| panic!("no {key}"))
}

fn row_of<'a>(table: &'a Node, is: &str) -> &'a Node {
    items(table, "rows").iter().find(|r| text(r, "is") == is).unwrap_or_else(|| panic!("no row '{is}'"))
}

/// A tube's angle after a row, by its list name.
fn after(row: &Node, name: &str) -> f64 {
    number(items(row, "tubes").iter().find(|t| text(t, "name") == name).unwrap_or_else(|| panic!("no {name} in the row")), "after")
}

/// A way up as `fab.orient` applies it.
fn apply_text(s: &Session, row: &Node) -> String {
    let up = items(row, "up");
    format!(r#"{{"apply":true,"expected":"{}","up":[{},{},{}],"spin":{}}}"#, s.model.head().text(), up[0].number().unwrap(), up[1].number().unwrap(), up[2].number().unwrap(), number(row, "spin"))
}

/// A cylinder stands up on the plate: its one tube reads as standing, and the first way up lies it flat.
#[test]
fn an_upright_cylinder_comes_out_flat() {
    let mut s = plate(UPRIGHT);
    let list = tubes(&mut s);
    let found = items(&list, "tubes");
    assert_eq!(found.len(), 1, "one tube: {found:?}");
    assert!(number(&found[0], "angle_to_bed") > 85.0, "it stands");
    assert!((number(&found[0], "length") - 60.0).abs() < 3.0, "length {}", number(&found[0], "length"));
    assert!((number(&found[0], "thinnest_radius") - 5.0).abs() < 0.5, "radius {}", number(&found[0], "thinnest_radius"));
    assert!(number(&found[0], "flat_gain_estimate") > 1.0, "lying flat gains");
    let name = text(&found[0], "name").to_string();
    let table = orient(&mut s, "{}");
    let flat = row_of(&table, "tubes flattest");
    assert!(after(flat, &name) < 2.0, "it comes out flat: {}", after(flat, &name));
    assert_eq!(table.field("committed").and_then(|f| f.value.ident()), Some("false"), "a table moves nothing");
}

/// Two tubes square to each other lie flat together.
#[test]
fn two_perpendicular_tubes_both_come_out_flat() {
    let mut s = plate(ELL);
    let table = orient(&mut s, "{}");
    let flat = row_of(&table, "tubes flattest");
    for name in ["up", "out"] {
        assert!(after(flat, name) < 3.0, "{name} comes out flat: {}", after(flat, name));
    }
}

/// Three that can't all lie flat: the words say which stays, and weights change which it is.
#[test]
fn three_perpendicular_tubes_say_which_stays_and_weights_choose() {
    let mut s = plate(JACK);
    let keep = |s: &mut Session, stays: &str, a: &str, b: &str| {
        let table = orient(s, &format!(r#"{{"weights":{{"{a}":10,"{b}":10,"{stays}":1}}}}"#));
        let flat = row_of(&table, "tubes flattest");
        assert!(after(flat, a) < 3.0 && after(flat, b) < 3.0, "{a} {} and {b} {}", after(flat, a), after(flat, b));
        assert!(after(flat, stays) > 85.0, "{stays} {}", after(flat, stays));
        let says = text(&table, "says");
        assert!(says.contains(&format!("{stays} ")) && says.contains("stays"), "{says}");
    };
    keep(&mut s, "alongy", "alongx", "alongz");
    keep(&mut s, "alongx", "alongy", "alongz");
}

/// A limb on a part with no names is found, called by where it is.
#[test]
fn a_limb_with_no_name_is_found() {
    let mut s = plate(NAMELESS);
    let list = tubes(&mut s);
    let found = items(&list, "tubes");
    assert_eq!(found.len(), 1, "the arm: {found:?}");
    assert!(text(&found[0], "name").starts_with("limb near"), "{}", text(&found[0], "name"));
    // its axis is the arm's: 30 up in 45 along
    let expected = 30f64.atan2(45.0).to_degrees();
    assert!((number(&found[0], "angle_to_bed") - expected).abs() < 3.0, "{} not {expected}", number(&found[0], "angle_to_bed"));
    assert!((number(&found[0], "thinnest_radius") - 5.0).abs() < 0.5, "{}", number(&found[0], "thinnest_radius"));
}

/// With no names to go on, a figure's short neck and both its legs are tubes all the same.
#[test]
fn a_nameless_figures_neck_and_legs_are_tubes() {
    let mut s = plate(SLIM);
    let list = tubes(&mut s);
    let found = items(&list, "tubes");
    assert!(found.iter().any(|t| text(t, "name").starts_with("neck near") && number(t, "angle_to_bed") > 80.0), "{found:?}");
    let legs = found.iter().filter(|t| number(t, "angle_to_bed") > 80.0 && number(t, "length") > 30.0).count();
    assert_eq!(legs, 2, "{found:?}");
}

/// A figure on its back: everything long in it lies flat, and a block says it has none.
#[test]
fn a_figure_lies_on_its_back() {
    let mut s = plate(FIGURE);
    let list = tubes(&mut s);
    let names: Vec<&str> = items(&list, "tubes").iter().map(|t| text(t, "name")).collect();
    for name in ["neck", "armL", "armR", "legs"] {
        assert!(names.contains(&name), "{name} in {names:?}");
    }
    for name in ["torso", "head"] {
        assert!(!names.contains(&name), "{name} is no tube: {names:?}");
    }
    let table = orient(&mut s, "{}");
    let flat = row_of(&table, "tubes flattest");
    for name in ["neck", "armL", "armR", "legs"] {
        assert!(after(flat, name) < 5.0, "{name} {}", after(flat, name));
    }
    let chunky = "let m = mod.model\nobject(m.box({size: [60mm 40mm 50mm] at: [0 20mm 0]}), name: \"block\")\n";
    let mut block = plate(chunky);
    let list = tubes(&mut block);
    assert!(items(&list, "tubes").is_empty(), "a block has no tubes");
}

/// A row applied: one undo step, the program as it was after it, and the next slice's list says the
/// angles it predicted, with what they were.
#[test]
fn a_row_applied_is_one_undo_and_the_new_slice_measures_it() {
    let mut s = plate(UPRIGHT);
    let engine = Arc::new(crate::supports_tests::EngineSlices::default());
    s.slices = Some(engine.clone());
    let (input, _) = crate::supports::SliceSource::input(&*engine, s.last.as_ref().unwrap()).unwrap();
    crate::supports::SliceSource::sliced(&*engine, &input, None).unwrap();
    let gcode_before = crate::supports_tests::EngineSlices::settled(&input).unwrap().gcode.text().expect("the settled G-code");
    let project_before = Some(s.model.project_text());
    let list = tubes(&mut s);
    assert_eq!(text(&list, "from"), "slice");
    let name = text(&items(&list, "tubes")[0], "name").to_string();
    let table = orient(&mut s, "{}");
    let flat = row_of(&table, "tubes flattest");
    let predicted = after(flat, &name);
    let apply = apply_text(&s, flat);
    orient(&mut s, &apply);
    // the move's slice
    let (input, _) = crate::supports::SliceSource::input(&*engine, s.last.as_ref().unwrap()).unwrap();
    crate::supports::SliceSource::sliced(&*engine, &input, None).unwrap();
    let list = tubes(&mut s);
    assert_eq!(text(&list, "from"), "slice");
    let row = &items(&list, "tubes")[0];
    assert!((number(row, "angle_to_bed") - predicted).abs() < 3.0, "the layers say {}, the row said {predicted}", number(row, "angle_to_bed"));
    assert!(number(row, "angle_before") > 85.0, "and what it was: {}", number(row, "angle_before"));
    // one undo: the program and the G-code as they were
    let undo = json::parse(format!(r#"{{"action":"undo","expected":"{}"}}"#, s.model.head().text()).as_bytes()).unwrap();
    let reply = s.call("history", &undo, None);
    assert!(reply.ok, "{}", reply.text);
    assert_eq!(Some(s.model.project_text()), project_before, "one undo restores the program");
    let (input, _) = crate::supports::SliceSource::input(&*engine, s.last.as_ref().unwrap()).unwrap();
    assert!(crate::supports_tests::EngineSlices::settled(&input).unwrap().gcode.text().expect("the settled G-code") == gcode_before, "and the G-code");
}

/// A figure whose arm is a little off flat, one far off, and a hand on the near one.
const REACHING: &str = "let m = mod.model
let torso = m.box({size: [30mm 20mm 60mm] at: [0 10mm 0]})
let armL = m.capsule({a: [-14mm 10mm 20mm] b: [-60mm 19mm 20mm] radius: 4mm})
let handL = m.sphere({radius: 6mm at: [-63mm, 20mm, 20mm]})
let armR = m.capsule({a: [14mm 10mm 20mm] b: [44mm 40mm 20mm] radius: 4mm})
object(m.blend([torso armL handL armR], 1mm), name: \"reacher\")
";

fn tube_named<'a>(list: &'a Node, name: &str) -> &'a Node {
    items(list, "tubes").iter().find(|t| text(t, "name") == name).unwrap_or_else(|| panic!("no tube {name}: {list:?}"))
}

/// Align a limb, from the program: the near arm turns flat within the cap at its shoulder with its
/// hand, the far one's past it and says so, nothing else moves, and it's one undo with the orient.
/// The new slice's layers say where it went.
#[test]
fn a_limb_turns_flat_within_its_cap_and_alone() {
    let mut s = plate(REACHING);
    let engine = Arc::new(crate::supports_tests::EngineSlices::default());
    s.slices = Some(engine.clone());
    let (input, _) = crate::supports::SliceSource::input(&*engine, s.last.as_ref().unwrap()).unwrap();
    crate::supports::SliceSource::sliced(&*engine, &input, None).unwrap();
    let gcode_before = crate::supports_tests::EngineSlices::settled(&input).unwrap().gcode.text().expect("the settled G-code");
    let script_before = s.model.source("shape.cad.splash").map(str::to_string);
    let project_before = Some(s.model.project_text());
    let list = tubes(&mut s);
    let near_before = number(tube_named(&list, "armL"), "angle_to_bed");
    assert!(near_before > 5.0 && near_before < 15.0, "{near_before}");

    // the table says what a turn would do under the plate as it stands
    let table = orient(&mut s, r#"{"tubes": ["armL", "armR"], "goal": "strength"}"#);
    let rows = items(&table, "align_as_it_stands");
    assert!(!rows.is_empty(), "{table:?}");

    // past the cap it's told, at the tool too
    let pick = |s: &Session, what: &str| json::parse(format!(r#"{{"expected":"{}","fixes":[{what}]}}"#, s.model.head().text()).as_bytes()).unwrap();
    let far = pick(&s, r#"{"tube":"armR","fix":"align","up":[0,0,1]}"#);
    let refused = s.call(crate::strengthen::TOOL, &far, None);
    assert!(!refused.ok && refused.text.contains("cap"), "{}", refused.text);
    assert_eq!(s.model.source("shape.cad.splash").map(str::to_string), script_before, "and nothing's written");

    let near = pick(&s, r#"{"tube":"armL","fix":"align","up":[0,0,1]}"#);
    let reply = s.call(crate::strengthen::TOOL, &near, None);
    assert!(reply.ok, "{}", reply.text);
    let written = s.model.source("shape.cad.splash").unwrap().to_string();
    let same_line = |line: &str| script_before.as_deref().unwrap().contains(line) && written.contains(line);
    assert!(same_line("let torso = m.box({size: [30mm 20mm 60mm] at: [0 10mm 0]})"), "{written}");
    assert!(same_line("let armR = m.capsule({a: [14mm 10mm 20mm] b: [44mm 40mm 20mm] radius: 4mm})"), "{written}");
    assert!(!written.contains("b: [-60mm 19mm 20mm]"), "the arm's own numbers moved: {written}");
    assert!(!written.contains("at: [-63mm, 20mm, 20mm]"), "and its hand's: {written}");
    assert_eq!(Some(s.model.project_text()), project_before, "the part itself hasn't moved");

    // the new slice measures it
    let (input, _) = crate::supports::SliceSource::input(&*engine, s.last.as_ref().unwrap()).unwrap();
    crate::supports::SliceSource::sliced(&*engine, &input, None).unwrap();
    let list = tubes(&mut s);
    assert_eq!(text(&list, "from"), "slice");
    let arm = tube_named(&list, "armL");
    assert!(number(arm, "angle_to_bed") < 3.0, "the layers say {}", number(arm, "angle_to_bed"));
    assert!((number(arm, "angle_before") - near_before).abs() < 1.0, "and what it was: {}", number(arm, "angle_before"));

    // one undo gives back the program and the G-code
    let undo = json::parse(format!(r#"{{"action":"undo","expected":"{}"}}"#, s.model.head().text()).as_bytes()).unwrap();
    let reply = s.call("history", &undo, None);
    assert!(reply.ok, "{}", reply.text);
    assert_eq!(s.model.source("shape.cad.splash").map(str::to_string), script_before, "one undo restores the program");
    let (input, _) = crate::supports::SliceSource::input(&*engine, s.last.as_ref().unwrap()).unwrap();
    assert!(crate::supports_tests::EngineSlices::settled(&input).unwrap().gcode.text().expect("the settled G-code") == gcode_before, "and the G-code");

    // with an orient: both are one undo
    let list = tubes(&mut s);
    let near = number(tube_named(&list, "armL"), "angle_to_bed");
    assert!((near - near_before).abs() < 1.0, "{near}");
    let both = pick(&s, r#"{"fix":"orient","up":[0,1,0],"spin":0},{"tube":"torso","fix":"align","up":[1,0,0]}"#);
    let reply = s.call(crate::strengthen::TOOL, &both, None);
    assert!(!reply.ok, "the torso's the rest of the part, or no limb at all: {}", reply.text);
    let both = pick(&s, r#"{"fix":"orient","up":[0,1,0],"spin":0},{"tube":"armL","fix":"align","up":[0,0,1]}"#);
    let reply = s.call(crate::strengthen::TOOL, &both, None);
    assert!(reply.ok, "{}", reply.text);
    assert_ne!(Some(s.model.project_text()), project_before, "the part moved");
    assert_ne!(s.model.source("shape.cad.splash").map(str::to_string), script_before, "and the limb");
    let undo = json::parse(format!(r#"{{"action":"undo","expected":"{}"}}"#, s.model.head().text()).as_bytes()).unwrap();
    let reply = s.call("history", &undo, None);
    assert!(reply.ok, "{}", reply.text);
    assert_eq!(Some(s.model.project_text()), project_before, "one undo: the part");
    assert_eq!(s.model.source("shape.cad.splash").map(str::to_string), script_before, "and the limb");
}

/// A plain mesh has no joint in its program: it says so and points at what still works.
#[test]
fn a_limb_with_no_name_cant_turn_and_says_what_can() {
    let mut s = plate(NAMELESS);
    let list = tubes(&mut s);
    let name = text(&items(&list, "tubes")[0], "name").to_string();
    let why = s.call(crate::strengthen::TOOL, &json::parse(format!(r#"{{"expected":"{}","fixes":[{{"tube":"{name}","fix":"align"}}]}}"#, s.model.head().text()).as_bytes()).unwrap(), None);
    assert!(!why.ok && why.text.contains("Solid infill"), "{}", why.text);
}

/// Least support: a table comes out on its top, and the plan agrees with the estimate's first choice.
#[test]
fn least_support_puts_a_table_on_its_top() {
    let mut s = least_support_plate(TABLE);
    s.slices = Some(Arc::new(crate::supports_tests::EngineSlices::default()));
    let table = orient(&mut s, r#"{"goal":"supports"}"#);
    let rows = items(&table, "rows");
    assert!(rows.len() >= 2, "{rows:?}");
    let up: Vec<f64> = items(&rows[1], "up").iter().map(|x| x.number().unwrap()).collect();
    // its legs point up: the model's Y, on the plate its Z, the other way
    assert!(up[2] < -0.95, "the table's own top down: {up:?}");
    assert!(number(&rows[1], "support_planned_mm3") < number(&rows[0], "support_planned_mm3"), "fewer than as it stands");
    let best_estimate = rows[1..].iter().map(|r| number(r, "support_estimate_mm3")).fold(f64::MAX, f64::min);
    assert!(number(&rows[1], "support_estimate_mm3") <= best_estimate * 1.01 + 1.0, "the plan's first is the estimate's");
}

/// A dome stays on its flat base.
#[test]
fn least_support_leaves_a_dome_on_its_base() {
    let mut s = plate_with(DOME, SUPPORTS);
    s.slices = Some(Arc::new(crate::supports_tests::EngineSlices::default()));
    let table = orient(&mut s, r#"{"goal":"supports"}"#);
    let rows = items(&table, "rows");
    let up: Vec<f64> = items(&rows[1], "up").iter().map(|x| x.number().unwrap()).collect();
    assert!(up[2] > 0.95, "it stays on its base: {up:?}");
}

/// "Least support but keep the neck flat": each row holds what it was asked to hold, and the table
/// shows the trade.
#[test]
fn least_support_holds_a_tube_it_is_told_to() {
    let mut s = least_support_plate(FIGURE);
    s.slices = Some(Arc::new(crate::supports_tests::EngineSlices::default()));
    let table = orient(&mut s, r#"{"goal":"supports","hold":[{"tube":"neck","within":10}]}"#);
    let rows = items(&table, "rows");
    assert!(rows.len() >= 2, "{rows:?}");
    for row in &rows[1..] {
        assert!(after(row, "neck") <= 10.5, "neck {}", after(row, "neck"));
    }
}

/// A word that names no tube changes nothing and lists what there is.
#[test]
fn an_unknown_tube_lists_the_tubes() {
    let mut s = plate(ELL);
    let reply = s.call(super::ORIENT_TOOL, &json::parse(br#"{"tubes":["tail"]}"#).unwrap(), None);
    assert!(!reply.ok, "{}", reply.text);
    assert!(reply.text.contains("tail") && reply.text.contains("out") && reply.text.contains("up"), "{}", reply.text);
}
