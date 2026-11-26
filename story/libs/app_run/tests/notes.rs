//! The text and pointer calls against `apps/notes` (registry id `notes`):
//! type (with a Return), key, scroll, drag, two separate clicks on one
//! spot. Its state lives in the run's own scratch `MAKEPAD_HOME`, so the
//! user's notes are never touched. One test function (one app at a time).

use makepad_story_app_run::*;
use makepad_strict_json::Value;

const RUN: &str = "use mod.apprun.*

AppRun{
    label: \"Notes: write a note\"
    app: \"notes\"
    output: {width: 1080 height: 1920 dpi_scale: 3 fps: 60}
    script: fn(app) {
        app.wait_settled()
        app.click(@root_view.dock.compose.glyph)
        app.wait_visible(@screen.document)
        app.type(@screen.document, \"Groceries\\noat milk, lemons\", {cps: 30})
        app.mark(@typed)
        app.click(@screen.document)
        app.click(@screen.document)
        app.key(@KeyA, {cmd: true})
        app.scroll(@screen.document, 0, 40)
        app.drag(@screen.document, @screen.nav.back)
        app.hold(0.5)
    }
}
";

fn run(name: &str, build: bool) -> RunResult {
    let dir = std::env::temp_dir().join(format!("makepad-app-run-notes-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut opts = RunOptions::new(&dir);
    opts.build = build;
    let result = execute(RUN, opts).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert!(result.ok(), "{name}: {:?}", result.failures);
    assert!(dir.join("home").is_dir(), "the app's MAKEPAD_HOME is the run's own");
    result
}

fn calls(r: &RunResult) -> Vec<Value> {
    r.record.get("calls").and_then(Value::as_arr).unwrap().to_vec()
}

#[test]
fn notes_takes_text_keys_and_pointer_calls_deterministically() {
    let first = run("first", true);
    let calls = calls(&first);
    let names: Vec<&str> = calls.iter().map(|c| c.get("call").and_then(Value::as_str).unwrap()).collect();
    assert_eq!(names, ["wait_settled", "click", "wait_visible", "type", "mark", "click", "click", "key", "scroll", "drag", "hold"]);
    // The second click on the same spot waited out the multi-click window.
    let waited = |i: usize| calls[i].get("waited").and_then(Value::as_u64).unwrap_or(0);
    assert!(waited(6) > 0, "{}", calls[6].to_json());
    // "Groceries\noat milk, lemons" at 30 cps: 26 characters, 52 frames,
    // after the click that focuses the field.
    let frames = |i: usize| {
        let f = calls[i].get("clock").and_then(Value::as_arr).unwrap();
        f[1].as_u64().unwrap() - f[0].as_u64().unwrap()
    };
    assert!(frames(3) >= 25 + 52, "type took {} frames", frames(3));
    let focus = first.record.get("focus").unwrap().get("events").unwrap().to_json();
    for source in ["\"click\"", "\"type\"", "\"wait\"", "\"scroll\"", "\"drag\""] {
        assert!(focus.contains(source), "no {source} focus: {focus}");
    }
    assert_eq!(calls[3].get("text").and_then(Value::as_str), Some("Groceries\noat milk, lemons"));
    let keys = first.record.get("bind").and_then(|b| b.get("keys")).unwrap().to_json();
    assert!(keys.contains(r#""key":"ReturnKey""#) && keys.contains(r#""key":"KeyA","mods":["cmd"]"#), "{keys}");
    assert!(keys.contains(r#""text":"G""#), "{keys}");
    let second = run("second", false);
    assert_eq!(second.frame_hashes, first.frame_hashes);
    assert_eq!(second.marks(), first.marks());
}
