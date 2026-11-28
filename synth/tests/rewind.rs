//! The transport's Rewind in the running app, driven over `--remote` by
//! makepad_test: the button, found by its accessible name, and Shift+Space
//! leave a stopped score at the top. What a rewind does to the sound is
//! `a_rewind_sounds_as_the_first_start` (makepad-score-live).

use makepad_test::{run_with_config, KeyCode, KeyModifiers, Selector, TestApp, TestConfig};

#[path = "support/score_thread.rs"]
mod score_thread;

#[test]
fn rewind_leaves_a_stopped_score_at_the_top() {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "rewind").unwrap();
    let data = config.artifacts_dir.join(format!("data-{}", std::process::id()));
    config.env.insert("SYNTH_DATA_DIR".into(), data.display().to_string());
    config.env.insert("MUSIC_ITER_DIR".into(), data.join("drafts/batch-history").display().to_string());
    run_with_config(config, |app: TestApp| {
        let named = |name: &str| app.locator(Selector::all().any_window().name(name)).wait_text(name);
        named("Rewind");
        named("1.1");
        app.locator(Selector::all().any_window().name("Rewind")).click();
        named("1.1");
        app.press_key_with_modifiers(KeyCode::Space, KeyModifiers { shift: true, ..Default::default() });
        named("1.1");
        score_thread::exercise(&app, &data.join("drafts"), "synth.score_status", "synth.score_read", "{}", true);
        outputs(&app, &data);
    }).unwrap();
    std::fs::remove_dir_all(data).unwrap();
}

/// A zero in an editor test is meaningful only if an actual UI-thread call
/// through the shader compiler boundary reaches the same marker and counter.
#[test]
fn the_cx_thread_counts_a_real_document_and_shader_compile() {
    use makepad_core_util::ui_thread;
    use makepad_score_live::{compose::Composer, model};
    use makepad_score_player::shader;
    let source = "use mod.score.*\nlet clock = Clock{}\nScore{clock}";
    assert!(!ui_thread::is_ui_thread());
    std::thread::spawn(move || {
        let _cx = makepad_widgets::Cx::new(Box::new(|_, _| {}));
        assert!(ui_thread::is_ui_thread());
        let before = ui_thread::violations();
        let stopped = makepad_score_live::PrepareControl::default();
        stopped.cancel();
        assert!(matches!(model::load_controlled(source, &stopped), Err(makepad_score_live::LoadError::Cancelled)));
        assert!(matches!(makepad_score_live::vm::evaluate_controlled(source, &stopped), Err(makepad_score_live::LoadError::Cancelled)));
        assert_eq!(ui_thread::violations(), before, "a request cancelled before either blocking driver records no UI work");
        let slice = std::time::Duration::ZERO;
        let word = makepad_score_live::PrepareControl::default();
        let model::LoadStep::Pending(pending) = model::load_begin("loop {}", &word, slice) else {
            panic!("the cooperative load yields to the UI");
        };
        assert!(matches!(pending.step(slice), model::LoadStep::Pending(_)), "the next UI slice also yields");
        assert_eq!(ui_thread::violations(), before, "starting and resuming a cooperative load records no blocking evaluation");
        let loaded = model::load_controlled(source, &makepad_score_live::PrepareControl::default()).unwrap();
        Composer::new(loaded.into_evaluated()).pump(1.0, &mut Vec::new());
        for _ in 0..2 {
            assert!(shader::compile("!").is_err());
        }
    }).join().unwrap();
    let counts = ui_thread::violations();
    for (kind, n) in [("a score evaluation", 1), ("a score document compile", 1),
        ("a score shader compile", 2), ("a score composer construction", 1), ("a score composer pump", 1)] {
        assert!(counts.contains(&(kind, n)), "{kind} expected {n} in {counts:?}");
    }
}

/// Open the remote calls on separate channels so a second request can arrive
/// while the first one is still preparing or writing.
fn outputs(app: &TestApp, data: &std::path::Path) {
    use makepad_strict_json::{self as json, Value};
    use std::time::Duration;
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).unwrap();
    let start = |tool: &str, args: Value| {
        let cmd = format!("call synth.{tool} {}", args.to_json());
        let request = makepad_remote::app::app_request_of(&cmd).unwrap().unwrap();
        client.open("app", &request).unwrap()
    };
    let finish = |ch: makepad_remote::Channel| {
        let mut answer = None;
        app.wait_until_within(Duration::from_secs(30), || loop {
            match ch.try_recv() {
                Ok(makepad_remote::Event::Data(bytes)) => {
                    for row in String::from_utf8_lossy(&bytes).lines() {
                        let msg = json::parse(row.as_bytes()).unwrap();
                        if let Some(result) = msg.get("Result").and_then(Value::as_arr).and_then(|a| a.first()) {
                            answer = Some(result.clone());
                            return None;
                        }
                    }
                }
                Err(true) => return Some("waiting for the file request".into()),
                Ok(makepad_remote::Event::Opened(_)) => {}
                event => panic!("file request ended without a reply: {event:?}"),
            }
        });
        answer.unwrap()
    };
    let success = |result: Value| {
        assert!(result.get("outcome").and_then(|o| o.get("Ok")).is_some(), "{result:?}");
        json::parse(result.get("text").unwrap().as_str().unwrap().as_bytes()).unwrap()
    };
    let first = start("score_save", json::obj(vec![]));
    let source = "use mod.score.*\nlet clock = Clock{bpm: 160}\nlet bass = Synth{preset: @sub}\nlet line = Loop{play: bass beats: 1 code: fn(i) { play(@a2) sleep(1) }}\nScore{label: \"Saved second\" clock bass line}";
    success(finish(start("score_write", json::obj(vec![("source", json::s(source))]))));
    let second = start("score_save", json::obj(vec![]));
    let older = finish(first);
    let saved = success(finish(second));
    let path = std::path::Path::new(saved.get("path").unwrap().as_str().unwrap());
    if !older.to_json().contains("superseded") {
        let earlier = success(older);
        assert_eq!(earlier.get("path"), saved.get("path"), "both saves target the open document");
    }
    assert_eq!(std::fs::read_to_string(path).unwrap(), source);

    let listing = success(finish(start("score_library", json::obj(vec![]))));
    let item = listing.get("scores").and_then(Value::as_arr).unwrap().iter()
        .find(|row| row.get("title").and_then(Value::as_str) == Some("Saved second")).expect("the saved score's evaluated title");
    success(finish(start("score_open", json::obj(vec![("name", item.get("name").unwrap().clone())]))));
    let read = success(finish(start("score_read", json::obj(vec![]))));
    assert_eq!(read.get("source").and_then(Value::as_str), Some(source));

    let render = |name: &str| start("score_render", json::obj(vec![
        ("path", json::s(data.join(name).display().to_string())), ("seconds", Value::F64(0.5)),
    ]));
    let first = render("first.wav");
    let second = render("second.wav");
    for (name, channel) in [("first.wav", first), ("second.wav", second)] {
        success(finish(channel));
        let wav = std::fs::read(data.join(name)).unwrap();
        assert!(wav.starts_with(b"RIFF") && wav.get(8..12) == Some(b"WAVE"), "{name}");
    }
    assert_eq!(app.command(Duration::from_secs(10), &["worker_violations"], &[]), ["{}"]);
}
