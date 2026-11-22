//! The Strength editor by hand on a generic figure in the Sliced view: Find lists what
//! would break first from the current slice and which way up helps, with turns of the part, of a
//! limb and thickening left unticked; Apply does what's ticked as one undo step, reslices and says
//! before and after; the undo gives the program back byte for byte; and `fab.strengthen` called with
//! the same picks writes the same program as the button did.

mod common;

use makepad_strict_json::Value;
use makepad_test::{makepad_test, NodeMatch, Role, Selector, States, TestApp};
use std::time::Duration;

const FIGURE: &str = r##"use mod.model.*
use mod.fab.*
let m = mod.model
let torso = m.box({size: [30mm 50mm 20mm] at: [0 60mm 0]})
let head = m.sphere({radius: 11mm at: [0 102mm 0]})
let neck = m.capsule({a: [0 80mm 0] b: [0 96mm 0] radius: 3mm})
let armL = m.capsule({a: [-14mm 78mm 0] b: [-50mm 72mm 0] radius: 3mm})
let armR = m.capsule({a: [14mm 78mm 0] b: [50mm 72mm 0] radius: 3mm})
let legL = m.capsule({a: [-8mm 40mm 0] b: [-8mm 2mm 0] radius: 3.5mm})
let legR = m.capsule({a: [8mm 40mm 0] b: [8mm 2mm 0] radius: 3.5mm})
object(m.blend([torso head neck armL armR legL legR], 1mm), name: "figure")
"##;

const HANG: Duration = Duration::from_secs(300);

fn call(app: &TestApp, tool: &str, args: &str) -> Result<String, String> {
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).map_err(|e| format!("connect: {e:?}"))?;
    let argv = vec!["call".to_string(), format!("fab.{tool}"), args.to_string()];
    let mut lines = Vec::new();
    makepad_remote::app::run(&client, &argv, &mut |line| {
        lines.push(line.to_string());
        true
    })
    .map_err(|e| format!("fab.{tool}: {e:?}"))?;
    let line = lines.iter().find(|l| l.contains("\"Result\"")).ok_or_else(|| format!("fab.{tool} returned no result: {lines:?}"))?;
    let v = makepad_strict_json::parse_depth(line.as_bytes(), 32).map_err(|e| format!("{e:?}"))?;
    let r = match v.get("Result").expect("Result") {
        Value::Arr(a) => a[0].clone(),
        other => other.clone(),
    };
    Ok(r.get("text").and_then(Value::as_str).unwrap_or_default().to_string())
}

fn wait<T>(app: &TestApp, what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let mut got = None;
    app.try_wait_until_within(HANG, || {
        got = f();
        got.is_none().then(|| format!("waiting for {what}"))
    })
    .unwrap_or_else(|e| {
        println!("[strength-here] grab {}", app.screenshot().display());
        println!("[strength-here] says {:?}", app.widget_snapshot().nodes().into_iter().filter(|w| w.visible() && w.node.role == Role::StaticText).map(|w| w.node.name.to_string()).filter(|n| !n.is_empty()).collect::<Vec<_>>());
        panic!("timed out: {}", e.message())
    });
    got.expect("the wait ended on an answer")
}

fn head(app: &TestApp) -> String {
    let reply = call(app, "inspect", "{}").expect("inspect");
    let value = makepad_script::syntax::parse_value(&reply).expect("a Splash reply");
    value.field("head").and_then(|f| f.value.string()).expect("a head").to_string()
}

fn sources(app: &TestApp) -> (String, String) {
    let reply = call(app, "inspect", "{}").expect("inspect");
    let value = makepad_script::syntax::parse_value(&reply).expect("a Splash reply");
    let scripts = value.field("scripts").and_then(|f| f.value.items()).expect("the scripts");
    let figure = scripts.iter().find(|s| s.field("file").and_then(|f| f.value.string()) == Some("figure.cad.splash")).expect("the figure's script");
    let source = figure.field("source").and_then(|f| f.value.string()).expect("its source").to_string();
    (source, value.field("project").and_then(|f| f.value.string()).expect("the project").to_string())
}

fn shown(app: &TestApp, role: Role) -> Vec<NodeMatch> {
    app.widget_snapshot().nodes().into_iter().filter(|w| w.visible() && w.node.role == role).collect()
}

fn says(app: &TestApp, starts: &str) -> Option<String> {
    shown(app, Role::StaticText).into_iter().map(|w| w.node.name.to_string()).find(|name| name.starts_with(starts))
}

fn sliced(app: &TestApp) {
    wait(app, "the slice's layers", || (common::slice_state(app) == "Sliced").then_some(()));
}

/// The Apply button, while something's ticked.
fn apply_button(app: &TestApp) -> Option<NodeMatch> {
    shown(app, Role::Button).into_iter().find(|w| w.node.name == "Apply the ticked fixes" && !w.node.states.contains(States::DISABLED))
}

#[makepad_test]
fn the_strength_section_lists_applies_as_one_undo_and_writes_what_the_tool_writes(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    wait(&app, "the fab tools", || call(&app, "inspect", "{}").ok());
    let batch = format!(r#"{{"request_id":"figure","expected":"{}","script":"figure.cad.splash","source":{}}}"#, head(&app), Value::Str(FIGURE.to_string()).to_json());
    let reply = call(&app, "script", &batch).expect("script");
    let value = makepad_script::syntax::parse_value(&reply).expect("a Splash reply");
    let h = value.field("head").and_then(|f| f.value.string()).expect("a head").to_string();
    let edits = Value::Str("place(\"figure\", at: [175mm 160mm], name: \"Figure\")\n".to_string()).to_json();
    call(&app, "place", &format!(r#"{{"request_id":"place","expected":"{h}","edits":{edits}}}"#)).expect("place");
    app.locator(Selector::id("rm_slice")).wait_visible().click();
    app.locator(Selector::id("rm_slice")).wait_checked(true);
    sliced(&app);
    let (script_before, project_before) = sources(&app);

    // the list, from the slice: the part's turn on top, unticked, and the spots with their fixes
    app.locator(Selector::role(Role::Tab).name("Strength")).click();
    let find = Selector::role(Role::Button).name("Find what would break first");
    app.locator(find.clone()).wait_visible().click();
    wait(&app, "the list", || says(&app, "Turn the part: "));
    wait(&app, "the neck, in the list", || says(&app, "Neck: "));
    wait(&app, "the count of weak places", || shown(&app, Role::StaticText).into_iter().map(|w| w.node.name.to_string()).find(|n| n.contains(" under ")));
    // a tick is there to press: drawn, and it takes a press
    let tick = shown(&app, Role::CheckBox).into_iter().find(|w| w.node.name == "Fix this place" && w.checked() == Some(false) && w.width() >= 10);
    let tick = tick.unwrap_or_else(|| panic!("no tick to press: {:?}", shown(&app, Role::CheckBox).iter().filter(|w| w.node.name == "Fix this place").map(|w| (w.width(), w.height())).collect::<Vec<_>>()));
    let tick = Selector::raw(format!("id={}", tick.id.short()));
    app.locator(tick.clone()).click();
    app.locator(tick.clone()).wait_checked(true);
    app.locator(tick.clone()).click();
    app.locator(tick).wait_checked(false);
    // nothing that changes the design is ticked; a spot's own fix is
    let button = wait(&app, "the Apply button, for the spots ticked by default", || apply_button(&app));

    // Apply: one undo step, a reslice, before and after
    app.locator(Selector::raw(format!("id={}", button.id.short()))).press();
    let after = wait(&app, "the before and after", || shown(&app, Role::StaticText).into_iter().map(|w| w.node.name.to_string()).find(|name| name.contains("Plate: ")));
    assert!(after.contains("->"), "{after}");
    sliced(&app);
    let (script_after, project_after) = sources(&app);
    assert!(script_after != script_before || project_after != project_before, "it wrote something");
    assert!(script_after.contains("settings") || script_after.contains("_base"), "as the fixes it picked: {script_after}");

    let undo = format!(r#"{{"action":"undo","expected":"{}"}}"#, head(&app));
    call(&app, "history", &undo).expect("undo");
    let (script_undone, project_undone) = sources(&app);
    assert_eq!(script_undone, script_before, "one undo gives back the script");
    assert_eq!(project_undone, project_before, "and the project");

    // the tool, with what the panel sent (each spot whose own fix is solid infill, the rows that come
    // ticked): the same program
    let list = call(&app, "weak_spots", "{}").expect("weak_spots");
    let value = makepad_script::syntax::parse_value(&list).expect("a Splash reply");
    let mut fixes = Vec::new();
    for spot in value.field("spots").and_then(|f| f.value.items()).unwrap_or(&[]) {
        let Some(offered) = spot.field("fixes").and_then(|f| f.value.items()) else { continue };
        let Some(fix) = offered.iter().find(|f| f.field("default").and_then(|d| d.value.ident()) == Some("true")).or(offered.first()) else { continue };
        let id = spot.field("id").and_then(|f| f.value.string()).expect("an id");
        let name = fix.field("fix").and_then(|f| f.value.string()).expect("a fix");
        if name != "solid_infill" {
            continue;
        }
        let to = fix.field("to").and_then(|f| f.value.number()).map_or_else(String::new, |to| format!(r#","to":{to}"#));
        fixes.push(format!(r#"{{"spot":"{id}","fix":"{name}"{to}}}"#));
    }
    let args = format!(r#"{{"expected":"{}","fixes":[{}]}}"#, head(&app), fixes.join(","));
    call(&app, "strengthen", &args).expect("strengthen");
    let (script_tool, project_tool) = sources(&app);
    assert_eq!(script_tool, script_after, "the tool and the button write the same script");
    assert_eq!(project_tool, project_after, "and the same project");
}
