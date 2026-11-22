//! `fab.settings_volume`: a sentence's worth of targets is one undo step, a word that matches nothing
//! changes nothing and lists what there is, a model with no names takes shapes by numbers, and every
//! shape is what its region says.

use crate::Session;
use fab_model::FabModel;
use fab_plate::machine::Machine;
use makepad_model::json::{self, Value};
use makepad_script::syntax::{parse_value, Node};
use std::collections::BTreeMap;
use std::sync::Arc;

/// A figure-like shape, built of named features.
const FIGURE: &str = "let m = mod.model
let torso = m.box({size: [30mm 50mm 20mm] at: [0 60mm 0]})
let head = m.sphere({radius: 11mm at: [0 102mm 0]})
let neck = m.cylinder({radius: 4mm height: 14mm at: [0 88mm 0]})
let armL = m.capsule({a: [-14mm 78mm 0] b: [-46mm 56mm 0] radius: 4mm})
let armR = m.capsule({a: [14mm 78mm 0] b: [46mm 56mm 0] radius: 4mm})
let legs = m.blend([m.capsule({a: [-8mm 40mm 0] b: [-10mm 4mm 0] radius: 5mm}) m.capsule({a: [8mm 40mm 0] b: [10mm 4mm 0] radius: 5mm})], 1mm)
object(m.blend([torso head neck armL armR legs], 1mm), name: \"figure\")
";

/// The same kind of thing with nothing named: a body and an arm on a diagonal.
const NAMELESS: &str = "let m = mod.model
object(m.union(m.box({size: [40mm 80mm 24mm] at: [0 40mm 0]}), m.capsule({a: [15mm 50mm 0] b: [60mm 80mm 0] radius: 5mm})), name: \"figure\")
";

fn plate(script: &str) -> Session {
    let mut session = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    let scripts = BTreeMap::from([("shape.cad.splash".to_string(), format!("use mod.model.*\nuse mod.fab.*\n{script}"))]);
    let project = "use mod.fab.*\nsettings({layer_height: \"0.2\"})\nscript(\"shape.cad.splash\")\nplace(\"shape\", at: [100mm 100mm])\n";
    session.model = FabModel::load("volumes", project, scripts).unwrap_or_else(|f| panic!("{f}"));
    session.rebuild().unwrap_or_else(|e| panic!("{e}"));
    session
}

fn head(s: &Session) -> String {
    s.model.head().text()
}

fn source(s: &Session) -> String {
    s.model.source("shape.cad.splash").unwrap_or_else(|| panic!("the script")).to_string()
}

fn args(text: &str, s: &Session) -> Value {
    json::parse(text.replace("HEAD", &head(s)).as_bytes()).unwrap()
}

fn call(s: &mut Session, text: &str) -> Result<Node, String> {
    let a = args(text, s);
    let reply = s.call(super::TOOL, &a, None);
    if reply.ok {
        Ok(parse_value(&reply.text).unwrap_or_else(|e| panic!("{e:?}: {}", reply.text)))
    } else {
        Err(reply.text)
    }
}

fn text_of(reply: &Node, key: &str) -> String {
    reply.field(key).and_then(|f| f.value.string()).unwrap_or_else(|| panic!("no {key}")).to_string()
}

fn undo(s: &mut Session) {
    let a = args(r#"{"action":"undo","expected":"HEAD"}"#, s);
    let reply = s.call("history", &a, None);
    assert!(reply.ok, "{}", reply.text);
}

/// The sentence: "solid infill the arms and legs and neck": one call, one undo step, said by name
/// and value, its rows listed.
#[test]
fn solid_infill_the_arms_legs_and_neck_is_one_undo_step() {
    let mut s = plate(FIGURE);
    let before = source(&s);
    let tree = s.call("tree", &Value::Obj(Vec::new()), None);
    for name in ["armL", "armR", "legs", "neck", "torso", "head"] {
        assert!(tree.text.contains(&format!("name: \"{name}\"")), "the tree lists {name}: {}", tree.text);
    }
    let reply = call(&mut s, r#"{"expected":"HEAD","targets":["armL","armR","legs","neck"],"settings":{"sparse_infill_density":"100%"}}"#).unwrap();
    assert_eq!(text_of(&reply, "done"), "armL, armR, legs, neck: infill 100%");
    let after = source(&s);
    assert!(after.starts_with(&before), "the program only grows at its end");
    for line in ["let armL_settings = m.offset(armL, 1mm)", "m.settings(armL_settings, {sparse_infill_density: \"100%\"})", "let neck_settings = m.offset(neck, 1mm)", "let legs_settings = m.offset(legs, 1mm)"] {
        assert!(after.contains(line), "'{line}' in {after}");
    }
    let inspected = s.call("inspect", &Value::Obj(Vec::new()), None);
    for name in ["armL_settings", "armR_settings", "legs_settings", "neck_settings"] {
        assert!(inspected.text.contains(&format!("name: \"{name}\"")), "fab.inspect lists {name}: {}", inspected.text);
    }
    assert!(inspected.text.contains("shape: \"follows\""), "{}", inspected.text);
    assert!(inspected.text.contains("target: \"armL\""), "{}", inspected.text);
    let tree = s.call("tree", &Value::Obj(Vec::new()), None);
    assert!(tree.text.contains("kind: \"settings volume\""), "fab.tree lists them as features: {}", tree.text);
    // one step back: the whole program as it was, and there is nothing more of this to undo
    undo(&mut s);
    assert_eq!(source(&s), before, "one undo restores it byte for byte");
    let inspected = s.call("inspect", &Value::Obj(Vec::new()), None);
    assert!(!inspected.text.contains("armL_settings"), "{}", inspected.text);
}

/// A word that matches nothing changes nothing: the answer says which and lists what there is.
#[test]
fn a_word_that_matches_no_feature_changes_nothing_and_lists_the_features() {
    let mut s = plate(FIGURE);
    let before = source(&s);
    let why = call(&mut s, r#"{"expected":"HEAD","targets":["armL","tail"]}"#).unwrap_err();
    assert!(why.contains("tail"), "it names the word: {why}");
    assert!(why.contains("armR") && why.contains("legs"), "it lists the features: {why}");
    assert!(why.contains("Nothing was changed"), "{why}");
    assert_eq!(source(&s), before);
    // the whole part is no feature either
    let why = call(&mut s, r#"{"expected":"HEAD","targets":["figure"]}"#).unwrap_err();
    assert!(why.contains("whole part"), "{why}");
    assert_eq!(source(&s), before);
}

/// A nameless model lists no features, and takes shapes by numbers at any angle, as one undo step.
#[test]
fn a_model_with_no_names_takes_shapes_by_numbers() {
    let mut s = plate(NAMELESS);
    let before = source(&s);
    let tree = s.call("tree", &Value::Obj(Vec::new()), None);
    assert!(tree.text.contains("features: []"), "{}", tree.text);
    let why = call(&mut s, r#"{"expected":"HEAD","targets":["arm"]}"#).unwrap_err();
    assert!(why.contains("no named features") && why.contains("fab.pick"), "it says how to go on: {why}");
    let reply = call(&mut s, r#"{"expected":"HEAD","settings":{"sparse_infill_density":"100%","wall_loops":4},"targets":[
        {"name":"right_arm","shape":"capsule","a":[15,50,0],"b":[60,80,0],"radius":6},
        {"name":"shoulder","shape":"sphere","at":[15,50,0],"radius":8},
        {"name":"tilted","shape":"cylinder","a":[0,0,0],"b":[10,10,0],"radius":3},
        {"name":"plain","shape":"box","at":[0,20,0],"size":[10,10,10]},
        {"name":"turned","shape":"box","at":[0,40,0],"size":[4,10,4],"up":[1,1,0]}
    ]}"#).unwrap();
    assert_eq!(text_of(&reply, "done"), "right_arm, shoulder, tilted, plain, turned: infill 100%, walls 4");
    let after = source(&s);
    assert!(after.contains("let right_arm_settings = m.capsule({a: [15mm, 50mm, 0mm] b: [60mm, 80mm, 0mm] radius: 6mm})"), "{after}");
    assert!(after.contains("m.settings(right_arm_settings, {sparse_infill_density: \"100%\" wall_loops: 4})"), "{after}");
    // each is the region its numbers say
    let at = |s: &Session, name: &str, p: [f64; 3]| -> bool {
        fab_model::paint::volume_region(&s.compiled.as_ref().unwrap().instances[0].script, name).unwrap().distance(p) <= 0.0
    };
    assert!(at(&s, "right_arm_settings", [37.5, 65.0, 0.0]), "the capsule's middle");
    assert!(at(&s, "right_arm_settings", [37.5, 65.0, 5.5]), "within its radius");
    assert!(!at(&s, "right_arm_settings", [37.5, 65.0, 6.5]), "past its radius");
    assert!(!at(&s, "right_arm_settings", [0.0, 65.0, 0.0]), "the body beside it");
    assert!(at(&s, "tilted_settings", [5.0, 5.0, 0.0]), "the tilted cylinder's middle");
    assert!(at(&s, "tilted_settings", [9.0, 9.0, 0.0]), "along its axis, not along Y");
    assert!(!at(&s, "tilted_settings", [5.0, -2.0, 0.0]), "off its axis");
    assert!(at(&s, "shoulder_settings", [15.0, 57.5, 0.0]) && !at(&s, "shoulder_settings", [15.0, 58.5, 0.0]), "the sphere's radius");
    assert!(at(&s, "plain_settings", [4.5, 24.5, 4.5]) && !at(&s, "plain_settings", [5.5, 24.5, 4.5]), "the box's sides");
    let turned = 0.5f64.sqrt();
    assert!(at(&s, "turned_settings", [4.0 * turned, 40.0 + 4.0 * turned, 0.0]), "the turned box's side along its up");
    assert!(!at(&s, "turned_settings", [0.0, 44.0, 0.0]), "not along the plate's up");
    // a number that makes no shape says whose
    let why = call(&mut s, r#"{"expected":"HEAD","targets":[{"name":"flat","shape":"capsule","a":[0,0,0],"b":[1,0,0],"radius":0}]}"#).unwrap_err();
    assert!(why.contains("flat") && why.contains("radius"), "{why}");
    undo(&mut s);
    assert_eq!(source(&s), before);
}

/// A sphere, a cylinder and a capsule fitted round a feature hold it and stay tight, and a fit saves
/// and reloads as it was.
#[test]
fn shapes_fitted_round_a_feature_hold_it_tightly_and_reload() {
    let mut s = plate(FIGURE);
    let reply = call(&mut s, r#"{"expected":"HEAD","targets":[{"feature":"armL","shape":"capsule"},{"feature":"armR","shape":"cylinder"},{"feature":"neck","shape":"sphere"},{"feature":"head","shape":"box"}]}"#).unwrap();
    assert_eq!(text_of(&reply, "done"), "armL, armR, neck, head: infill 100%");
    let program = s.compiled.as_ref().unwrap().instances[0].script.clone();
    let inside = |name: &str, p: [f64; 3]| fab_model::paint::volume_region(&program, name).unwrap().distance(p);
    // on the arm's axis and its surface, not far past them
    assert!(inside("armL_settings", [-30.0, 67.0, 0.0]) < 0.0 && inside("armL_settings", [-30.0, 67.0, 3.9]) < 0.0, "the capsule holds the arm");
    assert!(inside("armL_settings", [-30.0, 67.0, 5.5]) > 0.0, "the capsule is tight: {}", inside("armL_settings", [-30.0, 67.0, 5.5]));
    assert!(inside("armR_settings", [30.0, 67.0, 0.0]) < 0.0 && inside("armR_settings", [30.0, 67.0, 3.9]) < 0.0, "the cylinder holds the arm");
    assert!(inside("armR_settings", [30.0, 67.0, 6.0]) > 0.0, "the cylinder is tight");
    assert!(inside("neck_settings", [0.0, 88.0, 0.0]) < 0.0 && inside("neck_settings", [3.9, 88.0, 0.0]) < 0.0 && inside("neck_settings", [0.0, 94.9, 0.0]) < 0.0, "the sphere holds the neck");
    assert!(inside("neck_settings", [0.0, 97.0, 0.0]) > 0.0, "the sphere is tight");
    assert!(inside("head_settings", [10.9, 102.0, 0.0]) < 0.0 && inside("head_settings", [12.0, 102.0, 0.0]) > 0.0, "the box holds the head, tightly");
    // what it wrote reads back to the same volumes
    let text = source(&s);
    let scripts = BTreeMap::from([("shape.cad.splash".to_string(), text.clone())]);
    let project = "use mod.fab.*\nsettings({layer_height: \"0.2\"})\nscript(\"shape.cad.splash\")\nplace(\"shape\", at: [100mm 100mm])\n";
    let mut again = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    again.model = FabModel::load("volumes", project, scripts).unwrap_or_else(|f| panic!("{f}"));
    again.rebuild().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(source(&again), text);
    let inspected = again.call("inspect", &Value::Obj(Vec::new()), None);
    for shape in ["capsule", "cylinder", "sphere", "box"] {
        assert!(inspected.text.contains(&format!("shape: \"{shape}\"")), "{shape}: {}", inspected.text);
    }
}

/// A whole part: Box as it always was, the others fitted round its bounds; a follow is refused.
#[test]
fn a_whole_part_takes_a_box_or_a_fitted_shape() {
    let mut s = plate(NAMELESS);
    let object = "shape/figure";
    let a = format!(r#"{{"expected":"HEAD","object":"{object}","shape":"sphere"}}"#);
    let reply = call(&mut s, &a).unwrap();
    assert_eq!(text_of(&reply, "done"), "figure: infill 100%");
    assert!(source(&s).contains("let sphere_settings1 = m.sphere("), "{}", source(&s));
    let a = format!(r#"{{"expected":"HEAD","object":"{object}"}}"#);
    call(&mut s, &a).unwrap();
    assert!(source(&s).contains("let settings1 = m.box("), "{}", source(&s));
    let a = format!(r#"{{"expected":"HEAD","object":"{object}","shape":"follows"}}"#);
    assert!(call(&mut s, &a).unwrap_err().contains("whole part"));
}

/// `fab.fit`'s answer's field that's a number.
fn number_of(node: &Node, key: &str) -> f64 {
    node.field(key).and_then(|f| f.value.number()).unwrap_or_else(|| panic!("no {key}"))
}

fn point_of(node: &Node, key: &str) -> [f64; 3] {
    let items = node.field(key).and_then(|f| f.value.items()).unwrap_or_else(|| panic!("no {key}"));
    std::array::from_fn(|k| items[k].number().unwrap_or_else(|| panic!("{key}[{k}]")))
}

/// A point the reply writes in mm: Splash reads a length in metres.
fn point_mm(node: &Node, key: &str) -> [f64; 3] {
    point_of(node, key).map(|x| x * 1000.0)
}

fn fit(s: &mut Session, text: &str) -> Result<Node, String> {
    let reply = s.call(super::FIT_TOOL, &json::parse(text.as_bytes()).unwrap(), None);
    if reply.ok {
        Ok(parse_value(&reply.text).unwrap_or_else(|e| panic!("{e:?}: {}", reply.text)))
    } else {
        Err(reply.text)
    }
}

/// The arm of [`NAMELESS`], from A to B: its axis and radius.
const ARM: ([f64; 3], [f64; 3], f64) = ([15.0, 50.0, 0.0], [60.0, 80.0, 0.0], 5.0);

fn check_arm(answer: &Node) {
    let axis = point_of(answer, "axis");
    let d: [f64; 3] = std::array::from_fn(|k| ARM.1[k] - ARM.0[k]);
    let l = (0..3).map(|k| d[k] * d[k]).sum::<f64>().sqrt();
    let cos = (0..3).map(|k| axis[k] * d[k] / l).sum::<f64>().abs();
    assert!(cos > 3.0f64.to_radians().cos(), "the axis is the arm's, to 3 degrees: {axis:?}");
    assert!((number_of(answer, "radius") - ARM.2).abs() < 0.4, "the radius to a line width: {}", number_of(answer, "radius"));
    let ends = answer.field("ends").and_then(|f| f.value.items()).unwrap();
    let words: Vec<&str> = ends.iter().map(|e| e.field("is").and_then(|f| f.value.string()).unwrap()).collect();
    assert!(words.contains(&"a tip") && words.contains(&"a thicker part"), "one end free, one in the body: {words:?}");
}

/// A limb at an angle on a model with no names, from one picked point: its axis and radius, from
/// sections with no slice and from the layers with one, and a capsule placed from the answer holds
/// it and next to nothing of the body.
#[test]
fn a_limb_at_an_angle_fits_from_one_point_and_its_capsule_holds_it_alone() {
    let mut s = plate(NAMELESS);
    let before = source(&s);
    // the arm's surface, half way along it
    let picked = r#"{"points":[[37.5,65,5]]}"#;
    let answer = fit(&mut s, picked).unwrap();
    assert_eq!(text_of(&answer, "from"), "sections");
    assert!(answer.field("note").is_some(), "it says it had no slice");
    check_arm(&answer);
    assert_eq!(source(&s), before, "a fit places nothing");
    let engine = Arc::new(crate::supports_tests::EngineSlices::default());
    s.slices = Some(engine.clone());
    let (input, _) = crate::supports::SliceSource::input(&*engine, s.last.as_ref().unwrap()).unwrap();
    crate::supports::SliceSource::sliced(&*engine, &input, None).unwrap();
    let answer = fit(&mut s, picked).unwrap();
    assert_eq!(text_of(&answer, "from"), "slice");
    assert!(answer.field("note").is_none());
    check_arm(&answer);
    // its target, named, goes straight to the volume tool
    let target = answer.field("target").unwrap();
    let (a, b, radius) = (point_mm(&target.value, "a"), point_mm(&target.value, "b"), number_of(&target.value, "radius"));
    let call_text = format!(r#"{{"expected":"HEAD","targets":[{{"name":"right_arm","shape":"capsule","a":[{},{},{}],"b":[{},{},{}],"radius":{radius}}}]}}"#, a[0], a[1], a[2], b[0], b[1], b[2]);
    call(&mut s, &call_text).unwrap();
    // the plate changed: this read is of sections until it's sliced
    let answer = fit(&mut s, r#"{"points":[[37.5,65,5]],"volume":"right_arm_settings"}"#).unwrap();
    assert_eq!(text_of(&answer, "from"), "sections");
    let read = answer.field("volume").unwrap();
    let held = number_of(&read.value, "holds_of_this_part");
    let other = number_of(&read.value, "of_what_it_holds_is_something_else");
    assert!(held > 0.9, "it holds the arm: {held}");
    assert!(other < 0.05, "and next to nothing of the body: {other}");
    // one too fat for it says so
    let call_text = format!(r#"{{"expected":"HEAD","targets":[{{"name":"fat_arm","shape":"capsule","a":[{},{},{}],"b":[{},{},{}],"radius":18}}]}}"#, a[0], a[1], a[2], b[0], b[1], b[2]);
    call(&mut s, &call_text).unwrap();
    let answer = fit(&mut s, r#"{"points":[[37.5,65,5]],"volume":"fat_arm_settings"}"#).unwrap();
    let other = number_of(&answer.field("volume").unwrap().value, "of_what_it_holds_is_something_else");
    assert!(other > 0.2, "a capsule that takes in the body reads so: {other}");
}

/// Two points run through what's between them; a point in the air says which, and a fit asks for a
/// part when the plate has several.
#[test]
fn a_fit_says_what_it_cannot_do() {
    let mut s = plate(NAMELESS);
    let why = fit(&mut s, r#"{"points":[[200,200,200]]}"#).unwrap_err();
    assert!(why.contains("on no layer"), "{why}");
    let why = fit(&mut s, r#"{"points":[]}"#).unwrap_err();
    assert!(why.contains("points"), "{why}");
    let answer = fit(&mut s, r#"{"points":[[37.5,65,5],[55,75,5]],"shape":"cylinder"}"#).unwrap();
    check_arm(&answer);
    let target = answer.field("target").unwrap();
    assert_eq!(target.value.field("shape").and_then(|f| f.value.string()), Some("cylinder"));
}

/// The read of a volume after a slice: what's inside it by feature, what it added against the slice
/// before it, and what a volume that changes nothing says; the undo reads back to the same plate.
#[test]
fn a_volume_reads_what_the_slice_printed_inside_it() {
    let mut s = plate(FIGURE);
    s.slices = Some(Arc::new(crate::supports_tests::EngineSlices::default()));
    let read = |s: &mut Session| {
        let reply = s.call("inspect", &json::parse(br#"{"program":false,"read_volumes":true}"#).unwrap(), None);
        assert!(reply.ok, "{}", reply.text);
        let node = parse_value(&reply.text).unwrap();
        node.field("volumes_read").unwrap_or_else(|| panic!("{}", reply.text)).value.clone()
    };
    let base = read(&mut s);
    let plate_mm3 = number_of(&base, "plate_mm3");
    call(&mut s, r#"{"expected":"HEAD","targets":["armL"],"settings":{"sparse_infill_density":"100%"}}"#).unwrap();
    let now = read(&mut s);
    let volumes = now.field("volumes").and_then(|f| f.value.items()).unwrap();
    assert_eq!(volumes.len(), 1);
    let arm = &volumes[0];
    assert_eq!(arm.field("name").and_then(|f| f.value.string()), Some("armL_settings"));
    assert!(number_of(arm, "layers") > 20.0, "it reaches the arm's layers");
    assert!(number_of(arm, "added_mm3") > 0.0, "solid infill adds material");
    assert!(arm.field("changed_nothing").is_none());
    assert!(number_of(arm, "filled") > 0.8, "it prints near solid inside: {}", number_of(arm, "filled"));
    // what the plate's total says it added is what the volume says
    let total = number_of(&now, "plate_mm3") - plate_mm3;
    assert!((total - number_of(arm, "added_mm3")).abs() < 0.2 * total, "the plate added {total}, the volume {}", number_of(arm, "added_mm3"));
    let inside = arm.field("inside").and_then(|f| f.value.items()).unwrap();
    assert_eq!(inside[0].field("solid").and_then(|f| f.value.string()), Some("armL"), "what it holds is the arm");
    assert!(number_of(&inside[0], "share") > 0.7, "most of it: {}", number_of(&inside[0], "share"));
    // one that sets what's there already changes nothing, and says so
    call(&mut s, r#"{"expected":"HEAD","targets":["armR"],"settings":{"wall_loops":"2"}}"#).unwrap();
    let again = read(&mut s);
    let right = again.field("volumes").and_then(|f| f.value.items()).unwrap().iter().find(|v| v.field("name").and_then(|f| f.value.string()) == Some("armR_settings")).unwrap().clone();
    assert!(right.field("changed_nothing").is_some(), "{right:?}");
    // undo: the read says so
    undo(&mut s);
    undo(&mut s);
    let after = read(&mut s);
    assert_eq!(after.field("volumes").and_then(|f| f.value.items()).map(<[Node]>::len), Some(0), "no volumes left");
    assert_eq!(number_of(&after, "plate_mm3"), plate_mm3, "the plate prints as it did");
}

/// A picture aimed at a volume tints the part's surface inside it and nothing outside; a name that
/// isn't a volume lists the ones there are.
#[test]
fn a_render_tints_what_a_volume_holds() {
    let mut s = plate(FIGURE);
    call(&mut s, r#"{"expected":"HEAD","targets":["armL"],"settings":{"sparse_infill_density":"100%"}}"#).unwrap();
    let render = |s: &mut Session, args: &[u8]| s.call("render", &json::parse(args).unwrap(), None);
    let reply = render(&mut s, br#"{"volume":"armL_settings"}"#);
    let Some(crate::RenderRequest::Sheets { extra: Some(tint), .. }) = reply.render else { panic!("no tint: {}", reply.text) };
    assert!(!tint.indices.is_empty());
    let ev = s.last.clone().unwrap();
    let region = fab_model::paint::volume_region(&s.compiled.as_ref().unwrap().instances[0].script, "armL_settings").unwrap();
    let to_model = fab_model::weave::invert(&super::pick_frame(&ev, 0));
    let worst = tint.positions.iter().map(|p| region.distance(fab_model::params::xf_point(&to_model, p.map(f64::from)))).fold(f64::MIN, f64::max);
    assert!(worst < 1.0, "it stays inside the volume, to the size of a facet: {worst}");
    let why = render(&mut s, br#"{"volume":"nope"}"#);
    assert!(!why.ok && why.text.contains("armL_settings"), "{}", why.text);
}

/// The extruded mm3 of a G-code's moves whose middles a test holds, and of all of them. Plate mm;
/// a retract moves backward and its prime forward on the spot, so only moves along the bed with
/// the filament going forward count.
fn gcode_mm3(gcode: &str, filament_area: f64, inside: &dyn Fn([f64; 3]) -> bool) -> (f64, f64) {
    let (mut at, mut e, mut relative) = ([0.0f64; 3], 0.0f64, false);
    let (mut held, mut all) = (0.0, 0.0);
    for line in gcode.lines() {
        let line = line.split(';').next().unwrap_or("").trim();
        let mut words = line.split_whitespace();
        let Some(code) = words.next() else { continue };
        let number = |key: char| line.split_whitespace().skip(1).find(|w| w.starts_with(key)).and_then(|w| w[1..].parse::<f64>().ok());
        match code {
            "M82" => relative = false,
            "M83" => relative = true,
            "G92" => {
                if let Some(v) = number('E') {
                    e = v;
                }
            }
            "G0" | "G1" => {
                let to = [number('X').unwrap_or(at[0]), number('Y').unwrap_or(at[1]), number('Z').unwrap_or(at[2])];
                let fed = number('E').map_or(0.0, |v| if relative { v } else { v - e });
                if let Some(v) = number('E') {
                    e = if relative { e + v } else { v };
                }
                if fed > 0.0 && (to[0] != at[0] || to[1] != at[1]) {
                    let mm3 = fed * filament_area;
                    all += mm3;
                    if inside(std::array::from_fn(|k| 0.5 * (at[k] + to[k]))) {
                        held += mm3;
                    }
                }
                at = to;
            }
            _ => {}
        }
    }
    (held, all)
}

/// What a volume's read says is printed inside it is what the G-code extrudes there, on a shape
/// with nothing but walls and infill to print.
#[test]
fn a_volumes_read_matches_the_gcode_inside_it() {
    let tower = "let m = mod.model\nobject(m.box({size: [30mm 40mm 30mm] at: [0 20mm 0]}), name: \"tower\")\n";
    let mut s = plate(tower);
    let engine = Arc::new(crate::supports_tests::EngineSlices::default());
    s.slices = Some(engine.clone());
    call(&mut s, r#"{"expected":"HEAD","targets":[{"name":"core","shape":"cylinder","a":[0,10,0],"b":[0,30,0],"radius":8}],"settings":{"sparse_infill_density":"100%"}}"#).unwrap();
    let (input, _) = crate::supports::SliceSource::input(&*engine, s.last.as_ref().unwrap()).unwrap();
    crate::supports::SliceSource::sliced(&*engine, &input, None).unwrap();
    let reply = s.call("inspect", &json::parse(br#"{"program":false,"read_volumes":true}"#).unwrap(), None);
    assert!(reply.ok, "{}", reply.text);
    let node = parse_value(&reply.text).unwrap();
    let read = node.field("volumes_read").unwrap_or_else(|| panic!("{}", reply.text)).value.clone();
    let volumes = read.field("volumes").and_then(|f| f.value.items()).unwrap();
    let inside = |k: &str| number_of(&volumes[0], k);
    let read_mm3 = inside("walls_mm3") + inside("sparse_mm3") + inside("solid_fill_mm3");
    let settled = crate::supports_tests::EngineSlices::settled(&input).unwrap();
    let ev = s.last.clone().unwrap();
    let region = fab_model::paint::volume_region(&s.compiled.as_ref().unwrap().instances[0].script, "core_settings").unwrap();
    let to_model = fab_model::weave::invert(&super::slice_frame(&ev, 0));
    let held = |p: [f64; 3]| region.distance(fab_model::params::xf_point(&to_model, p)) <= 0.0;
    let (gcode_inside, gcode_all) = gcode_mm3(&settled.gcode.text().expect("the settled G-code"), settled.job.filament_area(0), &held);
    assert!(gcode_inside > 0.0, "the G-code prints inside it");
    assert!((read_mm3 - gcode_inside).abs() < 0.03 * gcode_inside, "the read says {read_mm3}, the G-code {gcode_inside}");
    assert!((number_of(&read, "plate_mm3") - gcode_all).abs() < 0.05 * gcode_all, "the plate: the read says {}, the G-code {gcode_all}", number_of(&read, "plate_mm3"));
}

fn test_print_call(s: &mut Session, boxes: &str) -> crate::Reply {
    let a = args(&format!(r#"{{"request_id":"cubes-HEAD","expected":"HEAD","action":"add","boxes":{boxes}}}"#), s);
    s.call("test_print", &a, None)
}

/// The sentence: "give me testprint cubes for the left arm, neck and legs": one call, one undo step,
/// each fitted round its feature, named after it and ticked; a size is a size round its centre.
#[test]
fn test_cubes_go_on_features_by_name_in_one_undo_step() {
    let mut s = plate(FIGURE);
    let before = s.model.project_text();
    let r = test_print_call(&mut s, r#"[{"feature":"armL"},{"feature":"neck","size":30},{"feature":"legs"}]"#);
    assert!(r.ok, "{}", r.text);
    let listed: Vec<(String, bool)> = s.model.project().test_prints.iter().map(|t| (t.name.clone(), t.on)).collect();
    assert_eq!(listed, [("armL".to_string(), true), ("neck".to_string(), true), ("legs".to_string(), true)]);
    let ev = s.last.clone().unwrap();
    let placed = |s: &Session, name: &str| ev.test_print_box(s.model.project().test_print(name).unwrap()).unwrap();
    // armL: a capsule's box, half a millimetre clear of it (a model unit is a plate mm here).
    let arm = placed(&s, "armL");
    let long: [f64; 3] = [46.0 - 14.0 + 8.0 + 1.0, 78.0 - 56.0 + 8.0 + 1.0, 8.0 + 1.0];
    let mut got = arm.size;
    got.sort_by(f64::total_cmp);
    let mut want = long;
    want.sort_by(f64::total_cmp);
    for k in 0..3 {
        assert!((got[k] - want[k]).abs() < 0.2, "the arm's box {:?} against {want:?}", arm.size);
    }
    assert_eq!(placed(&s, "neck").size, [30.0; 3]);
    // Round the feature's centre: the same one the fit would have taken.
    let fit = s.feature_box(&s.compiled.clone().unwrap(), &ev, "neck", None).unwrap();
    let neck = placed(&s, "neck");
    assert!((0..3).all(|k| (neck.center[k] - fit.center()[k]).abs() < 1e-3), "{:?} against {:?}", neck.center, fit.center());
    undo(&mut s);
    assert_eq!(s.model.project_text(), before, "one call, one undo step");
}

/// A word that matches nothing is said, with what there is and how to go on, and changes nothing.
#[test]
fn a_test_cube_on_a_word_that_matches_nothing_says_so_and_changes_nothing() {
    let mut s = plate(FIGURE);
    let before = s.model.project_text();
    let r = test_print_call(&mut s, r#"[{"feature":"armL"},{"feature":"tail"},{"feature":"wing"}]"#);
    assert!(!r.ok, "{}", r.text);
    for word in ["tail", "wing", "armR", "legs", "Nothing was changed", "fab.fit"] {
        assert!(r.text.contains(word), "it says {word}: {}", r.text);
    }
    assert_eq!(s.model.project_text(), before);
    // A model with no names says so too, and a box by numbers goes on it.
    let mut s = plate(NAMELESS);
    let r = test_print_call(&mut s, r#"[{"feature":"arm"}]"#);
    assert!(!r.ok && r.text.contains("no named features") && r.text.contains("fab.pick"), "{}", r.text);
    let r = test_print_call(&mut s, r#"[{"object":"shape/figure","at":[100,120,20],"size":30,"name":"Arm"}]"#);
    assert!(r.ok, "{}", r.text);
    // Found by looking: fab.fit's box goes on as it answers it, in the model frame.
    let fit = s.call("fit", &json::parse(br#"{"object":"shape/figure","points":[[40,65,0]],"shape":"box"}"#).unwrap(), None);
    assert!(fit.ok, "{}", fit.text);
    let target = parse_value(&fit.text).unwrap();
    let target = &target.field("target").unwrap().value;
    let nums = |key: &str| -> Vec<f64> { target.field(key).unwrap().value.items().unwrap().iter().map(|v| v.number().unwrap()).collect() };
    let (at, size) = (nums("at"), nums("size"));
    let r = test_print_call(&mut s, &format!(r#"[{{"object":"shape/figure","model":true,"at":{at:?},"size":{size:?},"name":"Looked"}}]"#));
    assert!(r.ok, "{}", r.text);
    let t = s.model.project().test_print("Looked").unwrap();
    assert_eq!((t.at.to_vec(), t.size.to_vec()), (at, size), "it stands where the fit says, in the part's frame");
}

/// A part that is a piece of the plate on its own takes a box over the whole of it.
#[test]
fn a_test_cube_on_a_whole_part_fits_that_part() {
    let mut s = plate(FIGURE);
    let r = test_print_call(&mut s, r#"[{"feature":"figure"}]"#);
    assert!(r.ok, "{}", r.text);
    let ev = s.last.clone().unwrap();
    let t = s.model.project().test_print("figure").unwrap();
    let b = ev.plate.parts[0].bounds();
    let placed = ev.test_print_box(t).unwrap();
    for k in 0..3 {
        assert!((placed.size[k] - (b.max[k] - b.min[k] + 1.0)).abs() < 1e-3, "{:?} over {b:?}", placed.size);
    }
}
