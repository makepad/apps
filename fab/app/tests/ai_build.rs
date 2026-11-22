//! The live AI build, end to end, the way an outside agent drives it: the
//! `fab.*` tools through the app's remote `app` service (what
//! `makepad-remote mcp <pid>` speaks). A program builds a bracket,
//! `fab.render` returns review sheets, `fab.export` writes a 3MF,
//! undo empties the plate again.
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

#[path = "common/wait.rs"]
mod idle_wait;
use idle_wait::wait_poll;
use makepad_strict_json::Value;
use makepad_test::{makepad_test, Selector, TestApp};

/// One `fab.<tool>` call; its result line (the app service's `Result`).
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

fn call(app: &TestApp, tool: &str, args: &str) -> String {
    try_call(app, tool, args).unwrap_or_else(|e| panic!("{e}"))
}

/// The tools are linked on the window's first frames: wait for them.
fn wait_for_tools(app: &TestApp) -> String {
    wait_poll(app, || {
        try_call(app, "inspect", "{}").map_err(|e| format!("the fab tools never appeared: {e}"))
    })
}

fn result_of(line: &str) -> Value {
    let v = makepad_strict_json::parse_depth(line.as_bytes(), 32).expect("result line is JSON");
    match v.get("Result").expect("Result") {
        Value::Arr(a) => a[0].clone(),
        other => other.clone(),
    }
}

/// The tool's own reply text out of the result line.
fn text(line: &str) -> String {
    result_of(line).get("text").and_then(Value::as_str).unwrap_or_default().to_string()
}

fn images(line: &str) -> Vec<Vec<u8>> {
    result_of(line)
        .get("images")
        .and_then(Value::as_arr)
        .map(|a| a.iter().filter_map(Value::as_str).filter_map(|b| makepad_base64::base64_decode(b.as_bytes()).ok()).collect())
        .unwrap_or_default()
}

/// A tool's pictures as the chat's provider takes them: each no more than its side limit a side,
/// and the set passing its check of a tool's pictures.
fn the_provider_takes(pictures: &[Vec<u8>], what: &str) {
    use makepad_ai_hub::providers::provider::{validate_tool_images, ToolImage, MAX_TOOL_IMAGE_SIDE};
    for png in pictures {
        assert_eq!(&png[1..4], b"PNG", "{what}");
        let side = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
        assert!(side(16).max(side(20)) <= MAX_TOOL_IMAGE_SIDE, "{what}: {} x {} px", side(16), side(20));
    }
    let images: Vec<ToolImage> = pictures.iter().map(|png| ToolImage { label: what.into(), png: std::sync::Arc::from(png.as_slice()) }).collect();
    validate_tool_images(&images).unwrap_or_else(|e| panic!("{what}: {e}"));
}

fn head_of(reply: &str) -> String {
    let reply = makepad_script::syntax::parse_value(reply).expect("a Splash reply");
    reply.field("head").and_then(|f| f.value.string()).expect("reply has a head").to_string()
}

#[makepad_test]
fn an_agent_builds_renders_exports_and_undoes(app: TestApp) {
    app.locator(Selector::role(makepad_test::Role::Window)).wait_visible();
    let r = wait_for_tools(&app);
    let reply = text(&r);
    let head = head_of(&reply);

    let source = r##"use mod.model.*
use mod.fab.*
let m = mod.model
let plate = m.box({size: [60mm 6mm 40mm]})
let hole_tool = m.cylinder({radius: 4mm height: 20mm segments: 48})
let bracket = m.subtract(plate, hole_tool)
object(bracket)
"##;
    let batch = format!(
        r#"{{"request_id":"bracket-1","expected":{head:?},"script":"bracket.cad.splash","source":{}}}"#,
        Value::Str(source.to_string()).to_json(),
    );
    let reply = text(&call(&app, "script", &batch));
    let head = head_of(&reply);
    let edits = "color(\"Orange\", rgb: #xe07020)\nplace(\"bracket\", color: \"Orange\")";
    let batch = format!(r#"{{"request_id":"bracket-1-place","expected":{head:?},"edits":{}}}"#, Value::Str(edits.to_string()).to_json());
    let reply = text(&call(&app, "place", &batch));
    assert!(reply.contains("closed: true") && reply.contains("solid: \"bracket/bracket\""), "{reply}");
    let head = head_of(&reply);

    let r = call(&app, "render", "{}");
    let sheets = images(&r);
    assert_eq!(sheets.len(), 2, "two review sheets: {}", &r[..r.len().min(400)]);
    the_provider_takes(&sheets, "the review sheets");

    // A picture aimed at a place comes back alone, one view, with what it shows; a support check
    // answers after its search, off the model worker.
    let r = call(&app, "overhang_view", r#"{"box":[[0,0,0],[400,400,100]],"views":["iso"]}"#);
    let look = images(&r);
    assert_eq!(look.len(), 1, "{}", &r[..r.len().min(400)]);
    the_provider_takes(&look, "one view");
    assert!(text(&r).contains("frame:"), "{r}");
    let reply = text(&call(&app, "supports", r#"{"verdicts":true}"#));
    assert!(reply.contains("verdicts:") && reply.contains("type: @none"), "{reply}");

    let out = std::env::temp_dir().join(format!("fab-ai-build-{}.3mf", std::process::id()));
    let reply = text(&call(&app, "export", &format!(r#"{{"format":"3mf","path":{:?}}}"#, out.display().to_string())));
    assert!(reply.contains("exported:"), "{reply}");
    let bytes = std::fs::read(&out).expect("the 3MF was written");
    assert_eq!(&bytes[..2], b"PK");
    let _ = std::fs::remove_file(&out);

    let reply = text(&call(&app, "history", &format!(r#"{{"expected":{head:?},"action":"undo"}}"#)));
    let head = head_of(&reply);
    let reply = text(&call(&app, "history", &format!(r#"{{"expected":{head:?},"action":"undo"}}"#)));
    assert!(reply.contains("parts: []"), "{reply}");
}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: std::time::Duration = std::time::Duration::from_secs(300);
