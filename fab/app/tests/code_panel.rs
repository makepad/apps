//! The Design workspace edits the model as code: the feature tree lists the
//! program, the code panel holds its text, and a typed edit rebuilds the
//! plate and commits one undo step. The Add menu inserts a ready solid as one
//! step. A text that does not build commits nothing (the panel says where it
//! fails), and undo takes back the last committed step while the panel keeps
//! the text typed into it.
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

#[path = "common/wait.rs"]
mod idle_wait;
use idle_wait::wait_poll;
use makepad_script::syntax::{self, Node};
use makepad_strict_json::Value;
use makepad_test::{makepad_test, KeyCode, KeyModifiers, Role, Selector, TestApp};
use std::time::Duration;

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

fn head_of(reply: &str) -> String {
    let reply = makepad_script::syntax::parse_value(reply).expect("a Splash reply");
    reply.field("head").and_then(|f| f.value.string()).expect("reply has a head").to_string()
}

fn inspect(app: &TestApp) -> Node {
    let reply = text(&call(app, "inspect", "{}").expect("inspect"));
    syntax::parse_value(&reply).expect("inspect is Splash")
}

fn script(v: &Node) -> &Node {
    v.field("scripts").and_then(|f| f.value.items()).and_then(|scripts| scripts.iter().find(|s| s.field("file").and_then(|f| f.value.string()) == Some("block.cad.splash"))).expect("the block script")
}

/// The built box's height: model Y becomes printer Z, reported in millimetres.
fn block_height(v: &Node) -> Option<f64> {
    let parts = v.field("plate")?.value.field("parts")?.value.items()?;
    let block = parts.iter().find(|p| p.field("solid").and_then(|f| f.value.string()) == Some("block/block"))?;
    block.field("size")?.value.items()?.get(2)?.number().map(|h| h * 1000.0)
}

fn wait_height(app: &TestApp, want: f64, what: &str) {
    wait_poll(app, || {
        let h = block_height(&inspect(app));
        if h.is_some_and(|h| (h - want).abs() < 0.01) {
            return Ok(());
        }
        Err(format!("{what}: the box is {h:?} high, expected {want}"))
    })
}

fn wait_name(app: &TestApp, prefix: &str) -> String {
    wait_poll(app, || {
        let found = app.widget_snapshot().nodes().into_iter().filter(|w| w.visible()).map(|w| w.node.name).find(|n| n.starts_with(prefix));
        if let Some(n) = found {
            return Ok(n);
        }
        Err(format!("nothing says \"{prefix}…\""))
    })
}

/// Replace the whole code with `code`, as typed (the code is an area of its own).
fn retype(app: &TestApp, code: &str) {
    app.locator(Selector::id("code")).wait_visible().click();
    app.press_key_with_modifiers(KeyCode::KeyA, KeyModifiers { logo: cfg!(target_os = "macos"), control: !cfg!(target_os = "macos"), ..Default::default() });
    app.type_text(code);
}

/// Bring a dock panel forward: a tab of its area's stack (Features shares one with Plates and Parts).
fn show_tab(app: &TestApp, name: &str) {
    app.locator(Selector::role(Role::Tab).name(name)).wait_visible().click();
}

#[makepad_test]
fn the_model_is_edited_as_code(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    let line = wait_poll(&app, || {
        call(&app, "inspect", "{}").map_err(|e| format!("the fab tools never appeared: {e}"))
    });
    let reply = text(&line);
    let head = head_of(&reply);
    let source = r##"use mod.model.*
use mod.fab.*
let m = mod.model
let block = m.box({size: [40mm 30mm 40mm]})
object(block)
"##;
    let batch = format!(
        r#"{{"request_id":"code-1","expected":{head:?},"script":"block.cad.splash","source":{}}}"#,
        Value::Str(source.to_string()).to_json(),
    );
    let reply = text(&call(&app, "script", &batch).expect("script"));
    let head = head_of(&reply);
    let edits = r#"place("block", name: "Block")"#;
    let batch = format!(r#"{{"request_id":"code-1-place","expected":{head:?},"edits":{}}}"#, Value::Str(edits.to_string()).to_json());
    let reply = text(&call(&app, "place", &batch).expect("place"));
    assert!(reply.contains("solid: \"block/block\""), "{reply}");

    // ---- the tree lists the program; picking a feature selects it ----
    // Design is the workspace the app opens in.
    show_tab(&app, "Features");
    let row = Selector::role(Role::ListItem).name("base box block level 1");
    app.locator(row.clone()).wait_visible().select();
    wait_name(&app, "1 feature");

    // ---- a typed edit rebuilds and commits ----
    let code = script(&inspect(&app)).field("source").and_then(|f| f.value.string()).expect("the script source").to_string();
    let taller = code.replace("[40mm 30mm 40mm]", "[40mm 45mm 40mm]");
    assert_ne!(taller, code, "the box's size is in the code: {code}");
    retype(&app, &taller);
    wait_height(&app, 45.0, "after typing");
    let built = inspect(&app);
    assert_eq!(script(&built).field("source").and_then(|f| f.value.string()), Some(taller.as_str()), "the typed code is the script's source");
    assert_eq!(built.field("can_undo").and_then(|f| f.value.ident()), Some("true"), "a typed edit commits an undo step");

    // ---- the Add menu inserts a ready solid (sketch + extrude) as one step ----
    show_tab(&app, "Features");
    app.locator(Selector::role(Role::Button).name("Add feature")).wait_visible().press();
    app.locator(Selector::role(Role::MenuItem).name("From a sketch")).wait_visible().press();
    app.locator(Selector::role(Role::MenuItem).name("Extrude")).wait_visible().press();
    let extruded = |v: &Node| script(v).field("solids").and_then(|f| f.value.items()).expect("the solids").iter().any(|s| s.field("type").and_then(|f| f.value.name()) == Some("Extrude"));
    let added = wait_poll(&app, || {
        let reply = inspect(&app);
        if extruded(&reply) {
            return Ok(reply);
        }
        Err("no Extrude solid was added".to_string())
    });
    wait_name(&app, "2 features");
    let added_code = script(&added).field("source").and_then(|f| f.value.string()).expect("the script source").to_string();
    let added_height = block_height(&added);

    // ---- a text that does not build commits nothing and says so ----
    // ("Not built" is said once its build has failed: a commit would have come before it)
    retype(&app, &added_code.replace("m.box({", "m.boxx({"));
    wait_name(&app, "Not built");
    let failed = inspect(&app);
    assert_eq!(block_height(&failed), added_height, "a failing text must not commit");
    assert_eq!(script(&failed).field("source").and_then(|f| f.value.string()), Some(added_code.as_str()), "the failed source was not committed");

    // ---- undo takes back the last committed step (the Add); the panel keeps the typed text ----
    app.locator(Selector::id("undo").nth(0)).press();
    let undone = wait_poll(&app, || {
        let reply = inspect(&app);
        if !extruded(&reply) {
            return Ok(reply);
        }
        Err("undo did not take back the Add".to_string())
    });
    assert_eq!(script(&undone).field("source").and_then(|f| f.value.string()), Some(taller.as_str()), "undo goes back to the typed edit's program");
    assert!(block_height(&undone).is_some_and(|h| (h - 45.0).abs() < 0.01), "undo keeps the typed edit's 45 mm box");
    wait_name(&app, "Not built");
    // typing the program again leaves nothing uncommitted
    retype(&app, &taller);
    wait_name(&app, "1 feature");
}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);
