//! The same edits and transport actions in the two score editors.
use makepad_strict_json::{self as json, Value};
use makepad_test::{KeyCode, KeyModifiers, Selector, TestApp};
use std::path::Path;
use std::time::Duration;

fn select_code(app: &TestApp) {
    app.locator(Selector::raw("role=text_area").text_contains("use mod.score")).wait_visible().focus();
    app.locator(Selector::raw("role=text_area state=focused").text_contains("use mod.score")).wait_visible();
    app.press_key_with_modifiers(KeyCode::KeyA, KeyModifiers {
        control: !cfg!(target_os = "macos"), logo: cfg!(target_os = "macos"), ..Default::default()
    });
}

fn edit(app: &TestApp, source: &str) {
    select_code(app);
    app.type_text(source);
}

fn score(title: &str, shader: &str) -> String {
    format!("use mod.score.*\nlet clock = Clock{{bpm: 240}}\nlet lead = Shader{{code: \"{shader}\"}}\nlet line = Loop{{play: lead beats: 1 code: fn(i) {{ play(@c4) sleep(1) }} }}\nScore{{label: \"{title}\" clock lead line}}")
}

fn title(app: &TestApp, name: &str) {
    app.locator(Selector::raw("role=static_text").name(name)).wait_text(name);
}

fn tool(app: &TestApp, name: &str, args: &str) -> Value {
    let rows = app.call_tool(Duration::from_secs(30), name, args);
    for row in &rows {
        let message = json::parse(row.as_bytes()).unwrap();
        if let Some(result) = message.get("Result").and_then(Value::as_arr).and_then(|v| v.first()) {
            assert!(result.get("outcome").and_then(|v| v.get("Ok")).is_some(), "{name}: {row}");
            return json::parse(result.get("text").unwrap().as_str().unwrap().as_bytes()).unwrap();
        }
    }
    panic!("{name}: missing result in {rows:?}");
}

pub fn exercise(app: &TestApp, drafts: &Path, status_tool: &str, read_tool: &str, read_args: &str, has_tracker: bool) {
    const VOICE: &str = "fn voice() { gate * velocity * 0.1 }";
    edit(app, &score("First edit", VOICE));
    title(app, "First edit");
    // The tracker's read-only reason is its grid's description (empty while it edits).
    let banner = || app.locator(Selector::raw("role=grid name=Tracker").text_contains("does not load"));

    // The real init writes a shared table. Wait for its execution guard before
    // replacing it, independently of typing debounce and worker speed.
    let slow = score("Obsolete edit", "let t=[0.0;2] fn init(){ for i in 0..1000000000 { for j in 0..1000000000 { t[0] += float(j) } } } fn voice(){ gate*t[0] }");
    edit(app, &slow);
    title(app, "Still initialising…");
    let log_cursor = app.log_since(0).unwrap().last_seq;
    // Remote reads now wait for the pending revision. The visible title is
    // the applied revision and can be observed without waiting for the load.
    title(app, "First edit");
    if has_tracker {
        assert_eq!(banner().count(), 0, "the tracker must not call a pending load a failed one");
    }
    edit(app, &score("Replacement edit", VOICE));
    title(app, "Replacement edit");
    app.wait_until(|| {
        let log = app.log_since(log_cursor).unwrap();
        (!log.lines.iter().any(|line| line.contains("score: load of revision ") && line.contains(" cancelled")))
            .then(|| "waiting for the obsolete initializer to return cancelled".into())
    });
    app.locator(Selector::raw("role=static_text").name("Still initialising…")).wait_count(0);
    assert!(tool(app, read_tool, read_args).to_json().contains("Replacement edit"));

    let button = |id| app.locator(Selector::id(id)).click();
    button("score_play");
    app.wait_until(|| {
        let status = tool(app, status_tool, "{}");
        assert_eq!(status.get("problems").and_then(Value::as_arr), Some([].as_slice()));
        (status.get("bar").and_then(Value::as_i64).unwrap() < 1)
            .then(|| "the transport has yet to leave its first bar".into())
    });
    button("score_stop");
    app.locator(Selector::raw("role=button").name("Rewind")).click();
    title(app, "1.1");
    // Stop stores a seek on the stopped composer. After the rewind's clock
    // locate the next play handles that seek in Worker::restart_at.
    button("score_stop");
    button("score_play");
    app.wait_until(|| {
        let status = tool(app, status_tool, "{}");
        let sounding = status.get("playing").and_then(Value::as_bool) == Some(true)
            && status.get("loops").and_then(Value::as_arr).unwrap().iter()
                .any(|l| l.get("cycle").and_then(Value::as_i64).unwrap_or(0) > 0);
        (!sounding).then(|| "the restarted loop has yet to play".into())
    });
    button("score_stop");
    // A later invalid revision keeps the playing document and shows its own problem.
    edit(app, &score("Invalid edit", "fn voice() { missing_guard_voice }"));
    app.wait_until(|| {
        let status = tool(app, status_tool, "{}");
        let problems = status.get("problems").unwrap().as_arr().unwrap();
        (!problems.iter().any(|p| p.as_str().is_some_and(|p| p.contains("missing_guard_voice"))))
            .then(|| format!("waiting for the replacement diagnostic, got {problems:?}"))
    });
    assert!(app.locator(Selector::raw("role=static_text").name("Replacement edit")).count() > 0,
        "the invalid document replaced the last good title: {}", tool(app, read_tool, read_args).to_json());
    if has_tracker {
        banner().wait_count(1);
    }
    edit(app, &score("Replacement edit", VOICE));
    app.wait_until(|| {
        let status = tool(app, status_tool, "{}");
        (!status.get("problems").unwrap().as_arr().unwrap().is_empty())
            .then(|| "waiting for the restored score to apply".into())
    });
    exercise_panel(app, status_tool.split_once('.').unwrap().0, has_tracker);
    drop_midi(app, drafts, read_tool, read_args);
    documents(app, drafts, status_tool.split_once('.').unwrap().0, has_tracker);
    let counts = app.command(Duration::from_secs(10), &["worker_violations"], &[]);
    assert_eq!(counts, ["{}"], "work observed on the editor's UI thread");
}

fn drop_midi(app: &TestApp, drafts: &Path, read_tool: &str, read_args: &str) {
    // Format 0, one track, 96 ticks per quarter. Play C, E and G for one beat each.
    let track = [
        0, 0x90, 60, 96, 96, 0x80, 60, 0,
        0, 0x90, 64, 96, 96, 0x80, 64, 0,
        0, 0x90, 67, 96, 96, 0x80, 67, 0,
        0, 0xff, 0x2f, 0,
    ];
    let mut midi = b"MThd\x00\x00\x00\x06\x00\x00\x00\x01\x00\x60MTrk".to_vec();
    midi.extend_from_slice(&(track.len() as u32).to_be_bytes());
    midi.extend_from_slice(&track);
    let file = drafts.join("Dropped_trio.mid");
    std::fs::write(&file, midi).unwrap();
    let path = file.canonicalize().unwrap();
    // Both editors accept a music file anywhere in the window.
    app.command(Duration::from_secs(10), &["drop"], &[
        ("path", path.to_str().unwrap()), ("x", "0"), ("y", "0"), ("wait", "1"),
    ]);
    title(app, "Dropped trio");
    assert!(tool(app, read_tool, read_args).to_json().contains("A cover imported from MIDI."),
        "the dropped MIDI did not become the applied score");
}

fn exercise_panel(app: &TestApp, namespace: &str, synth: bool) {
    let call = |name: &str, args: &str| tool(app, &format!("{namespace}.{name}"), args);
    let click = |x: f32, y: f32| {
        app.command(Duration::from_secs(10), &["mouse"], &[("k", "click"), ("x", &x.to_string()), ("y", &y.to_string()), ("wait", "1")]);
    };
    if synth {
        // Panel is the last segment in the side-pane tab strip.
        let tabs = app.locator(Selector::id("side_tabs_tip")).wait_visible().snapshot().bounds;
        click(tabs.x + tabs.w - 16.0, tabs.y + tabs.h / 2.0);
    }
    let source = r#"use mod.score.*
let clock = Clock{bpm: 240}
let lead = Shader{code: "fn voice() { gate * velocity * 0.1 }"}
let line = Loop{play: lead beats: 4 code: fn(i) {play(@c4) sleep(1) play(@e4) sleep(1) play(@g4) sleep(2)}}
let editor_panel = fn() { View{flow: Down width: Fill Label{text: "Phrase controls"} ScoreTree{} SynthTrack{} SynthGen{node: "line" gen: "arp"} } }
Score{label: "Panel score" clock lead line}"#;
    edit(app, source);
    title(app, "Panel score");
    let heading = app.locator(Selector::raw("role=static_text").name("Phrase controls")).wait_visible().snapshot().bounds;
    // The tree follows the heading. Press the name in its first voice row.
    app.wait_until(|| {
        let state = call("score_panel_status", "{}");
        (state.get("tracks_revision") != state.get("score_revision"))
            .then(|| format!("waiting for the voice tree: {state:?}"))
    });
    click(heading.x + 18.0, heading.y + heading.h + 39.0);
    let notes = || call("score_vary_status", r#"{"voice":"line"}"#);
    app.wait_until(|| {
        let state = notes();
        (!state.get("cards").and_then(Value::as_arr).is_some_and(|c| !c.is_empty()))
            .then(|| format!("waiting for the selected voice's phrase: {state:?}"))
    });

    call("score_vary", r#"{"voice":"line","kind":"answer","n":1,"bars":[1,1]}"#);
    let made = notes();
    assert_eq!(made.get("region_beats").unwrap().as_arr().unwrap().len(), 2);
    assert!(matches!(&made.get("region_beats").unwrap().as_arr().unwrap()[1], Value::Int(4) | Value::F64(4.0)));
    assert_eq!(made.get("cards").and_then(Value::as_arr).unwrap().len(), 2);
    call("score_vary", r#"{"voice":"line","action":"audition","card":1}"#);
    assert_eq!(notes().get("live_is_card").and_then(Value::as_bool), Some(true));
    call("score_vary", r#"{"voice":"line","action":"apply","card":1}"#);
    app.wait_until(|| {
        let state = call("score_status", "{}");
        let revision = state.get("revision").and_then(Value::as_i64);
        let read = if synth { call("score_read", "{}") } else { call("doc_read", r#"{"path":"score.splash"}"#) };
        (!read.to_json().contains("vary_cycles")).then(|| format!("waiting for the written variation at {revision:?}"))
    });
    // Both generator buttons belong to the actual generated panel. The preview
    // publishes its render measurement; Write submits the validated source.
    app.locator(Selector::raw("role=button").name("Hear arp for line")).wait_visible().click();
    app.wait_until(|| {
        let state = call("score_panel_status", "{}");
        (state.get("gen_previews").and_then(Value::as_i64).unwrap_or(0) == 0)
            .then(|| format!("waiting for the rendered preview: {state:?}"))
    });
    app.locator(Selector::raw("role=button").name("Write arp for line")).wait_visible().click();
    app.wait_until(|| {
        let state = call("score_panel_status", "{}");
        (state.get("gen_writes").and_then(Value::as_i64).unwrap_or(0) == 0)
            .then(|| format!("waiting for the generator write: {state:?}"))
    });
}

fn documents(app: &TestApp, drafts: &Path, service: &str, synth: bool) {
    let call = |name: &str, args: Value| tool(app, &format!("{service}.{name}"), &args.to_json());
    let before = score("Tool edit", "fn voice() { gate * 0.1 }");
    let write = if synth { "score_write" } else { "doc_write" };
    let patch = if synth { "score_patch" } else { "doc_patch" };
    let mut fields = if synth { vec![("source", json::s(&before))] }
        else { vec![("path", json::s("score.splash")), ("text", json::s(&before))] };
    let written = call(write, json::obj(fields.clone()));
    let base = written.get("revision").and_then(Value::as_i64).unwrap();
    let edits = Value::Arr(vec![json::obj(vec![("old", json::s("Clock{bpm: 240}")), ("new", json::s("Clock{bpm: 200}"))])]);
    let mut patch_fields = vec![("base_revision", Value::Int(base)), ("edits", edits)];
    if !synth { patch_fields.push(("path", json::s("score.splash"))); }
    let patched = call(patch, json::obj(patch_fields));
    assert!(patched.get("revision").and_then(Value::as_i64).unwrap() > base);
    fields.push(("base_revision", Value::Int(base)));
    let stale = app.call_tool(Duration::from_secs(30), &format!("{service}.{write}"), &json::obj(fields).to_json());
    assert!(stale.iter().any(|line| line.contains("changed") && line.contains("again")), "stale write: {stale:?}");
    let invalid = score("Check only", "fn voice() { invalid_check_voice }");
    let checked = call("score_check", json::obj(vec![("source", json::s(invalid))]));
    assert_eq!(checked.get("ok").and_then(Value::as_bool), Some(false));
    assert!(checked.to_json().contains("invalid_check_voice"));

    let read = if synth { call("score_read", json::obj(vec![])) }
        else { call("doc_read", json::obj(vec![("path", json::s("score.splash"))])) };
    assert!(read.to_json().contains("Clock{bpm: 200}"), "the stale write changed the patch: {read:?}");
    let heard = call("score_listen", json::obj(vec![("bars", Value::Int(1))]));
    assert_eq!(heard.get("bars").and_then(Value::as_i64), Some(1), "the listen job answered for the requested bar: {heard:?}");
    let transcription = call("score_text", json::obj(vec![]));
    assert!(transcription.get("text").and_then(Value::as_str).is_some_and(|s| s.contains("Tool edit")), "{transcription:?}");

    // A ready song in the test's own history makes the director read score
    // metadata when it starts. Stop as soon as the editor accepts the batch.
    let previous = drafts.join("batch-history/batches/b001");
    std::fs::create_dir_all(&previous).unwrap();
    std::fs::write(previous.join("request.txt"), "Brief: prior song").unwrap();
    std::fs::write(previous.join("s1.splash"), &before).unwrap();
    let started = call("score_batch", json::obj(vec![("prompt", json::s("a single note")), ("count", Value::Int(1)), ("seconds", Value::Int(15))]));
    let stopped = call("score_batch_stop", json::obj(vec![]));
    assert!(started.get("batch").and_then(Value::as_str).is_some(), "{started:?}");
    assert!(stopped.get("stopped").and_then(Value::as_bool).is_some(), "{stopped:?}");
}
