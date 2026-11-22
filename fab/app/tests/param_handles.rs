//! Generated parameter editing, driven through the semantic tree, on three parametric parts put
//! on the plate through `fab.script`: a post (two declared parameters, a height with an arrow
//! anchor and a radius with a ring), and the demo plate's bud vase (a field part) and shelf
//! bracket (a mesh part):
//!
//! - the post's generated control for the radius takes a value: the parameter and the part's
//!   footprint follow; in the Edit shape mode (E) its height's handle is dragged up in the view:
//!   the parameter follows the drag and the part is taller (the mesh rebuilt from the
//!   program); one undo puts the height back and keeps the radius;
//! - the post's Mass and balance row weighs nothing until it is opened, then shows its weight
//!   and the tilt that tips it;
//! - a typed vase height shows from the moment it is set, never the old one while the vase
//!   rebuilds, and the vase (drawn from its field) never says "Rebuilding…"; values finer than
//!   the step and a drag past the limit still end the indicator; its height handle grows it;
//! - the bracket's subtracted feature (the counterbore) builds, and its thickness handle
//!   thickens it.
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

#[path = "common/wait.rs"]
mod idle_wait;
use idle_wait::wait_poll;
use makepad_script::syntax::{self, Node};
use makepad_strict_json::Value;
use makepad_test::{makepad_test, Role, Selector, TestApp};
use std::time::Duration;

const POST: &str = r##"use mod.model.*
use mod.fab.*
let m = mod.model
let h = param(30mm, min: 5mm, max: 120mm, step: 0.5mm, label: "Post height")
let r = param(8mm, min: 2mm, max: 40mm, step: 0.1mm, label: "Post radius", kind: "radius")
let post = m.cylinder({radius: r height: h segments: 48})
object(post)
"##;

fn call(app: &TestApp, tool: &str, args: &str) -> Result<String, String> {
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).map_err(|e| format!("connect: {e:?}"))?;
    let argv = vec!["call".to_string(), format!("fab.{tool}"), args.to_string()];
    let mut lines = Vec::new();
    makepad_remote::app::run(&client, &argv, &mut |line| {
        lines.push(line.to_string());
        true
    })
    .map_err(|e| format!("fab.{tool}: {e:?}"))?;
    lines.iter().find(|l| l.contains("\"Result\"")).cloned().ok_or_else(|| format!("fab.{tool} returned no result: {lines:?}"))
}

fn text(line: &str) -> String {
    let v = makepad_strict_json::parse_depth(line.as_bytes(), 32).expect("result line is JSON");
    let r = match v.get("Result").expect("Result") {
        Value::Arr(a) => a[0].clone(),
        other => other.clone(),
    };
    r.get("text").and_then(Value::as_str).unwrap_or_default().to_string()
}

fn splash(text: &str) -> Node {
    syntax::parse_value(text).expect("a Splash reply")
}

fn head_of(reply: &str) -> String {
    splash(reply).field("head").and_then(|f| f.value.string()).expect("reply has a head").to_string()
}

/// Splash parses physical lengths in metres; these checks use millimetres.
fn mm(v: &Node) -> f64 {
    v.number().expect("a length") * 1000.0
}

/// A declared parameter's value (fab.params).
fn param(app: &TestApp, key: &str) -> f64 {
    let v = splash(&text(&call(app, "params", "{}").expect("params")));
    for part in v.field("parts").and_then(|f| f.value.items()).expect("parts") {
        for p in part.field("params").and_then(|f| f.value.items()).expect("params") {
            if p.field("key").and_then(|f| f.value.string()) == Some(key) {
                return mm(&p.field("value").expect("parameter value").value);
            }
        }
    }
    panic!("no parameter {key}");
}

/// The post's size on the plate (printer mm: x, y, z) from fab.inspect.
fn post_size(app: &TestApp) -> [f64; 3] {
    let v = splash(&text(&call(app, "inspect", r#"{"program":false}"#).expect("inspect")));
    let parts = v.field("plate").and_then(|p| p.value.field("parts")).and_then(|f| f.value.items()).expect("plate parts");
    let post = parts.iter().find(|p| p.field("solid").and_then(|f| f.value.string()) == Some("post/post")).expect("the post on the plate");
    let s = post.field("size").and_then(|f| f.value.items()).expect("a size");
    [mm(&s[0]), mm(&s[1]), mm(&s[2])]
}

/// What the app said and shows last, for a wait that failed.
fn show_last(app: &TestApp) {
    if let Ok(log) = app.log_since(0) {
        for l in log.lines.iter().filter(|l| l.contains("fab")).rev().take(30).collect::<Vec<_>>().into_iter().rev() {
            println!("[app] {l}");
        }
    }
    for w in app.widget_snapshot().nodes().into_iter().filter(|w| w.visible() && matches!(w.node.role, Role::SpinButton | Role::Slider | Role::ListItem)) {
        println!("[node] {:?} {:?} value={:?}", w.node.role, w.node.name, w.node.value);
    }
}

/// A short wait, on the app's budget ([`TestApp::wait_until`]).
fn wait_for(app: &TestApp, what: &str, mut ok: impl FnMut() -> bool) {
    if let Err(e) = app.try_wait_until(|| (!ok()).then(|| format!("timed out waiting for {what}"))) {
        show_last(app);
        panic!("{}", e.message());
    }
}

/// [`wait_for`] for what takes the app long (a rebuild, a drag's commit): `budget` of its main
/// thread's CPU ([`TestApp::wait_until_within`]).
fn wait_within(app: &TestApp, what: &str, budget: Duration, mut ok: impl FnMut() -> bool) {
    if let Err(e) = app.try_wait_until_within(budget, || (!ok()).then(|| format!("timed out waiting for {what}"))) {
        show_last(app);
        panic!("{}", e.message());
    }
}

/// Wait for a visible node by role and name.
fn wait_node(app: &TestApp, role: Role, name: &str) {
    wait_within(app, &format!("a visible {role:?} {name:?}"), Duration::from_secs(15), || app.widget_snapshot().nodes().iter().any(|w| w.visible() && w.node.role == role && w.node.name == name));
}

/// Pick a part in the parts list, then shape it (its handles).
fn pick_part(app: &TestApp, name: &str) {
    app.locator(Selector::role(Role::Tab).name("Parts")).click();
    app.locator(Selector::role(Role::TreeItem).name(name)).wait_visible().click();
    app.locator(Selector::id("t_shape")).wait_visible().click();
}

/// "Rebuilding…" (the chip and the view's sweep) shows for a mesh part only, and ends once
/// the build is in: never left on.
fn wait_rebuilt(app: &TestApp, after: &str) {
    wait_within(app, &format!("the rebuild indicator to clear after {after}"), Duration::from_secs(20), || {
        !app.widget_snapshot().nodes().iter().any(|w| w.visible() && w.node.role == Role::Status && w.node.name.starts_with("Rebuilding"))
    });
}

#[makepad_test]
fn parameters_are_edited_by_their_generated_control_and_by_dragging_their_handle(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    let mut line = None;
    app.wait_until_within(Duration::from_secs(30), || match call(&app, "inspect", "{}") {
        Ok(l) => {
            line = Some(l);
            None
        }
        Err(e) => Some(format!("the fab tools never appeared: {e}")),
    });
    let mut head = head_of(&text(&line.expect("the wait ends on the tools' answer")));
    let scripts = [("post", POST), ("vase", include_str!("../resources/vase.cad.splash")), ("bracket", include_str!("../resources/bracket.cad.splash"))];
    for (name, source) in scripts {
        let batch = format!(r#"{{"request_id":"handles-{name}","expected":{head:?},"script":"{name}.cad.splash","source":{}}}"#, Value::Str(source.to_string()).to_json());
        head = head_of(&text(&call(&app, "script", &batch).expect("script")));
    }
    let edits = r#"place("post", at: [80mm 165mm], name: "Post")
place("bracket", at: [150mm 165mm], name: "Shelf bracket")
place("vase", at: [222mm 165mm], name: "Bud vase")"#;
    let batch = format!(r#"{{"request_id":"handles-place","expected":{head:?},"edits":{}}}"#, Value::Str(edits.to_string()).to_json());
    let reply = text(&call(&app, "place", &batch).expect("place"));
    assert!(["post/post", "vase/vase", "bracket/bracket"].iter().all(|s| reply.contains(&format!("solid: \"{s}\""))), "the three parts are placed: {reply}");
    let s0 = post_size(&app);
    assert!((s0[2] - 30.0).abs() < 0.01 && (s0[0] - 16.0).abs() < 0.1, "a 30 mm post of radius 8: {s0:?}");

    // ---- the post's generated control: the radius takes a value, the footprint follows ----
    pick_part(&app, "Post");
    app.locator(Selector::role(Role::SpinButton).name("Post radius")).wait_visible().set_value(11);
    wait_within(&app, "the radius to reach 11", Duration::from_secs(20), || (param(&app, "post/r") - 11.0).abs() < 1e-6);
    wait_for(&app, "the footprint to follow the radius", || (post_size(&app)[0] - 22.0).abs() < 0.1);

    // ---- its Mass and balance row: closed until opened, then what it weighs and how it stands ----
    let row = Selector::role(Role::DisclosureButton).name("Mass and balance");
    app.locator(row.clone()).wait_visible();
    let shown = |text: &str| app.widget_snapshot().nodes().iter().find(|w| w.visible() && w.node.name.contains(text)).map(|w| w.node.name.clone());
    assert!(shown("tips over").is_none() && shown("Weighing").is_none(), "nothing is weighed until the row is opened");
    app.locator(row).expand();
    // every figure in view: the row slides open from its last line up, so the weight, its first,
    // shows last (on a loaded box, frames after the answer came)
    wait_within(&app, "the post to be weighed, its weight and footing in view", Duration::from_secs(20), || {
        shown("Yes: it tips over past").is_some() && shown(" g of ").is_some()
    });
    // a post 22 mm across and 30 mm tall, on its end, tips past atan(11 / 15)
    let stands = shown("tips over past").unwrap_or_default();
    assert!(stands.contains("36°"), "{stands}");
    let weight = shown(" g of ").unwrap_or_default();
    assert!(weight.contains("infill"), "{weight}");

    // ---- its handle: Edit shape mode, drag the height's arrow up ----
    app.locator(Selector::id("t_shape")).wait_checked(true);
    let handle = Selector::role(Role::Slider).name("Post height handle");
    app.locator(handle.clone()).wait_visible();
    app.locator(handle).drag_by(0.0, -80.0);
    wait_within(&app, "the height to grow by the handle", Duration::from_secs(20), || param(&app, "post/h") > 30.5);
    let h = param(&app, "post/h");
    assert!(((h * 10.0).round() - h * 10.0).abs() < 1e-9, "a drag sets the height at the label's precision: {h}");
    wait_for(&app, "the post to be as tall as its dragged height", || (post_size(&app)[2] - h).abs() < 0.05);

    // ---- one undo step puts the height back ----
    let head = head_of(&text(&call(&app, "inspect", r#"{"program":false}"#).expect("inspect")));
    let undo = text(&call(&app, "history", &format!(r#"{{"expected":{head:?},"action":"undo"}}"#)).expect("undo"));
    assert!(undo.contains("head:"), "{undo}");
    wait_for(&app, "the height back at 30 after one undo", || (param(&app, "post/h") - 30.0).abs() < 1e-6);
    assert!((param(&app, "post/r") - 11.0).abs() < 1e-6, "undo takes back the drag only");

    // ---- the vase: a typed value shows from the moment it is set, never the old one ----
    pick_part(&app, "Bud vase");
    wait_node(&app, Role::SpinButton, "Height");
    app.locator(Selector::role(Role::SpinButton).name("Height")).set_value(90);
    // Watch the control (UI only) until the indicator has been quiet a while: the vase is a field
    // part, so its view is current all along and nothing says it is rebuilding.
    let (mut seen, mut rebuilding, mut quiet) = (Vec::new(), false, 0);
    wait_poll(&app, || {
        let nodes = app.widget_snapshot().nodes();
        if let Some(n) = nodes.iter().find(|w| w.node.role == Role::SpinButton && w.node.name == "Height") {
            if seen.last() != Some(&n.node.value) {
                seen.push(n.node.value.clone());
            }
        }
        let busy = nodes.iter().any(|w| w.node.role == Role::Status && w.node.name.starts_with("Rebuilding"));
        rebuilding |= busy;
        quiet = if busy { 0 } else { quiet + 1 };
        if quiet > 25 {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(40));
        Err("waiting for the height control to settle".to_string())
    });
    assert_eq!(param(&app, "vase/vase_height"), 90.0, "the typed height is the vase's");
    assert!(!rebuilding, "the vase is drawn from its field: never Rebuilding");
    assert!(seen.iter().all(|v| v.starts_with("90")), "the control went back to an old value while the vase rebuilt: {seen:?}");
    // A value finer than the parameter's step: the model keeps it its own way (snapped or
    // rounded), and the indicator still ends; then back to 90.
    app.locator(Selector::role(Role::SpinButton).name("Height")).set_value(91.2371);
    wait_within(&app, "the height finer than its step to be taken", Duration::from_secs(20), || param(&app, "vase/vase_height") != 90.0);
    wait_rebuilt(&app, "a height finer than its step");
    app.locator(Selector::role(Role::SpinButton).name("Height")).set_value(90);
    wait_within(&app, "the height back at 90", Duration::from_secs(20), || param(&app, "vase/vase_height") == 90.0);
    wait_rebuilt(&app, "setting the height back");

    // ---- its handles: the height's arrow grows it; far past the limit the indicator still ends ----
    app.locator(Selector::id("t_shape")).wait_checked(true);
    let height = Selector::role(Role::Slider).name("Height handle");
    app.locator(height.clone()).wait_visible();
    app.locator(Selector::role(Role::SpinButton).name("Belly")).wait_visible();
    app.locator(height.clone()).drag_by(0.0, -60.0);
    wait_within(&app, "the vase to grow by its handle", Duration::from_secs(30), || param(&app, "vase/vase_height") > 92.0);
    wait_rebuilt(&app, "a handle drag");
    let h1 = param(&app, "vase/vase_height");
    // Far past the parameter's limit: the model clamps the value it builds.
    app.locator(height).drag_by(0.0, -900.0);
    wait_within(&app, "the long drag to be taken", Duration::from_secs(30), || param(&app, "vase/vase_height") > h1);
    wait_rebuilt(&app, "a drag past the limit");

    // ---- the bracket: a subtracted feature's value builds, its thickness handle thickens it ----
    pick_part(&app, "Shelf bracket");
    wait_node(&app, Role::Slider, "Thickness handle");
    wait_node(&app, Role::SpinButton, "Counterbore");
    app.locator(Selector::role(Role::SpinButton).name("Counterbore")).set_value(12);
    wait_within(&app, "the counterbore to build", Duration::from_secs(30), || (param(&app, "bracket/counterbore") - 12.0).abs() < 1e-6);
    wait_rebuilt(&app, "a subtracted feature's value");
    let t0 = param(&app, "bracket/thickness");
    app.locator(Selector::role(Role::Slider).name("Thickness handle")).drag_by(0.0, -25.0);
    wait_within(&app, "the bracket to thicken by its handle", Duration::from_secs(30), || param(&app, "bracket/thickness") > t0 + 0.4);
}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);
