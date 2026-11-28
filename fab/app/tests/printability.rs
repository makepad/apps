//! Printability while modelling and after slicing: a mushroom (a wide cap on a
//! thin stem) gets an overhang from the mesh stage through `fab.check`, and
//! `fab.check {"slice":true}` adds the slice stage (estimates, the cap's
//! cantilever). With the Overhangs view on, the parts list still holds only the
//! user's parts, never printability markers.
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

#[path = "common/wait.rs"]
mod idle_wait;
#[path = "common/parts_tab.rs"]
mod parts_tab;
use idle_wait::wait_poll;
use makepad_strict_json::Value;
use makepad_test::{makepad_test, Role, Selector, TestApp};
use std::time::Duration;

fn try_call(app: &TestApp, tool: &str, args: &str) -> Result<String, String> {
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

fn head_of(reply: &str) -> String {
    let reply = makepad_script::syntax::parse_value(reply).expect("a Splash reply");
    reply.field("head").and_then(|f| f.value.string()).expect("reply has a head").to_string()
}

/// The parts list holds the user's parts (the mushroom's row shows), never
/// printability markers (nor the printer's own geometry).
fn no_issue_rows(app: &TestApp) {
    parts_tab::open(app);
    let rows = wait_poll(app, || {
        let nodes = app.widget_snapshot().nodes();
        let in_parts = nodes.iter().any(|n| n.node.role == makepad_test::Role::TreeItem && n.node.path.contains("Parts"));
        let rows: Vec<_> = nodes.into_iter().filter(|n| n.node.role == makepad_test::Role::TreeItem && (!in_parts || n.node.path.contains("Parts"))).collect();
        if rows.iter().any(|n| n.node.name.to_lowercase().contains("mushroom")) {
            return Ok(rows);
        }
        Err(format!("the parts list's rows are in the semantic tree: {:?}", rows.iter().map(|n| &n.node.name).collect::<Vec<_>>()))
    });
    assert!(rows.iter().all(|n| !n.node.name.to_lowercase().contains("overhang") && !n.node.name.starts_with("fab:")),
        "printability markers and printer geometry must not be part rows: {rows:?}");
}

#[makepad_test]
fn check_overhangs_while_modelling_and_after_slicing(app: TestApp) {
    app.locator(Selector::role(makepad_test::Role::Window)).wait_visible();
    let line = wait_poll(&app, || {
        try_call(&app, "inspect", "{}").map_err(|e| format!("the fab tools never appeared: {e}"))
    });
    let head = head_of(&text(&line));
    // Cylinders are centered on Y. Overlap the top 2 mm of the stem with
    // the cap, leaving a 20 mm exposed stem in one connected boolean solid.
    let source = r##"use mod.model.*
use mod.fab.*
let m = mod.model
let stem = m.cylinder({radius: 4mm height: 22mm segments: 48 at: [0mm, 11mm, 0mm]})
let cap = m.cylinder({radius: 18mm height: 4mm segments: 64 at: [0mm, 22mm, 0mm]})
let mushroom = m.union(stem, cap)
object(mushroom)
"##;
    let batch = format!(
        r#"{{"request_id":"mushroom-1","expected":{head:?},"script":"mushroom.cad.splash","source":{}}}"#,
        Value::Str(source.to_string()).to_json(),
    );
    let reply = text(&try_call(&app, "script", &batch).expect("script"));
    let head = head_of(&reply);
    let edits = r#"color("Red", rgb: #xc03030)
place("mushroom", color: "Red")"#;
    let batch = format!(r#"{{"request_id":"mushroom-1-place","expected":{head:?},"edits":{}}}"#, Value::Str(edits.to_string()).to_json());
    let reply = text(&try_call(&app, "place", &batch).expect("place"));
    assert!(reply.contains("printability:"), "the plate summary carries printability: {reply}");

    let check = text(&try_call(&app, "check", "{}").expect("check"));
    assert!(check.contains("kind: @overhang"), "a mushroom cap overhangs: {check}");
    no_issue_rows(&app);

    // View › Overlays › Overhangs.
    app.locator(Selector::role(Role::Button).name("View")).wait_visible().press();
    app.locator(Selector::role(Role::MenuItem).name("Overlays")).wait_visible().press();
    app.locator(Selector::all().name("Overhangs")).wait_visible().press();

    let sliced = text(&try_call(&app, "check", r#"{"slice":true}"#).expect("check slice"));
    assert!(sliced.contains("time_s:"), "the slice stage adds estimates: {sliced}");
    assert!(sliced.contains("kind: @cantilever"), "the slice stage catches the cap: {sliced}");
    no_issue_rows(&app);
}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);
