//! Object scripts dragged over the window from outside: the plate shows them where their drop
//! puts them, listed as parts on their way in, for as long as the drag lasts. A plate committed
//! while the drag is over the window (an edit from elsewhere, or the first build answering after
//! the drag came in) gets the drag's preview again on top of it. Leaving the window puts the
//! plate back.
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

const HANG: Duration = Duration::from_secs(300);

/// A remote command's answer lines, or why it failed.
fn remote(app: &TestApp, argv: &[String]) -> Result<Vec<String>, String> {
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).map_err(|e| format!("connect: {e:?}"))?;
    let mut lines = Vec::new();
    makepad_remote::app::run(&client, argv, &mut |line| {
        lines.push(line.to_string());
        true
    })
    .map_err(|e| format!("{}: {e:?}", argv[0]))?;
    Ok(lines)
}

/// The file held over the window at `(x, y)`: the first call enters, later ones move it.
fn drag(app: &TestApp, path: &std::path::Path, x: f64, y: f64) -> Result<(), String> {
    let argv = vec!["drag".to_string(), format!("path={}", path.display()), format!("x={x}"), format!("y={y}")];
    let lines = remote(app, &argv)?;
    lines.iter().any(|l| l.contains("\"ok\":1")).then_some(()).ok_or_else(|| format!("drag: {lines:?}"))
}

fn call(app: &TestApp, tool: &str, args: &str) -> Result<String, String> {
    let argv = vec!["call".to_string(), format!("fab.{tool}"), args.to_string()];
    let lines = remote(app, &argv)?;
    lines.iter().find(|l| l.contains("\"Result\"")).cloned().ok_or_else(|| format!("fab.{tool} returned no result: {lines:?}"))
}

fn head_of(line: &str) -> String {
    let v = makepad_strict_json::parse_depth(line.as_bytes(), 32).expect("result line is JSON");
    let r = match v.get("Result").expect("Result") {
        Value::Arr(a) => a[0].clone(),
        other => other.clone(),
    };
    let text = r.get("text").and_then(Value::as_str).unwrap_or_default().to_string();
    let reply = makepad_script::syntax::parse_value(&text).expect("a Splash reply");
    reply.field("head").and_then(|f| f.value.string()).expect("reply has a head").to_string()
}

/// The parts the Parts list shows, by name.
fn rows(app: &TestApp) -> Vec<String> {
    app.widget_snapshot().nodes().into_iter().filter(|w| w.visible() && w.node.role == Role::TreeItem).map(|w| w.node.name).collect()
}

fn wait_rows(app: &TestApp, what: &str, ok: impl Fn(&[String]) -> bool) {
    wait_poll(app, || {
        let now = rows(app);
        if ok(&now) { Ok(()) } else { Err(format!("{what}: the list shows {now:?}")) }
    });
}

#[makepad_test]
fn a_drag_keeps_its_preview_when_a_committed_plate_replaces_it(app: TestApp) {
    let dir = std::env::temp_dir().join(format!("fab-file-drag-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a folder for the dragged file");
    let bracket = dir.join("bracket.cad.splash");
    std::fs::write(&bracket, include_str!("../resources/bracket.cad.splash")).expect("the dragged file");

    // In as soon as the app takes a drag, often before its first plate is answered. Whether that
    // answer comes after the drag's preview is up to timing, so this step passes either way; the
    // committed plate below is what proves the preview comes again.
    wait_poll(&app, || drag(&app, &bracket, 300.0, 300.0));
    app.locator(Selector::role(Role::Window)).wait_visible();
    parts_tab::open(&app);
    wait_rows(&app, "the dragged bracket on its way in", |r| r == ["bracket"]);

    // A plate committed while the drag is over the window (a script placed from elsewhere): the
    // drag's preview comes again on top of it.
    let line = wait_poll(&app, || call(&app, "inspect", "{}").map_err(|e| format!("the fab tools never appeared: {e}")));
    let head = head_of(&line);
    let source = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet block = m.box({size: [20mm 20mm 20mm]})\nobject(block)\n";
    let batch = format!(r#"{{"request_id":"drag-1","expected":{head:?},"script":"block.cad.splash","source":{}}}"#, Value::Str(source.to_string()).to_json());
    let head = head_of(&call(&app, "script", &batch).expect("script"));
    let batch = format!(r#"{{"request_id":"drag-1-place","expected":{head:?},"edits":{}}}"#, Value::Str("place(\"block\", name: \"Block\")".to_string()).to_json());
    call(&app, "place", &batch).expect("place");
    wait_rows(&app, "the placed block and, again, the dragged bracket", |r| r.len() == 2 && r.iter().any(|n| n == "Block") && r.iter().any(|n| n == "bracket"));

    // Out of the window: the plate as it is.
    let ended = remote(&app, &["drag_end".to_string()]).expect("drag_end");
    assert!(ended.iter().any(|l| l.contains("\"ok\":1")), "drag_end: {ended:?}");
    wait_rows(&app, "the plate without the bracket", |r| r == ["Block"]);
    let _ = std::fs::remove_dir_all(&dir);
}
