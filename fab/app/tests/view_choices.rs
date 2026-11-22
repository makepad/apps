//! The shading a person picks stands when Fab starts again.
//!
//! Why (user): their Fab was in Solid, and an Update restart brought the shadows and the screen AO
//! back. A restart in place is a fresh run of the app, and starts as a new session does, from the
//! machine's view preferences; only a change the JIT cannot patch in asks a run for one, so this
//! starts the app again over one preferences file (the shell's `a_chosen_look_comes_back` keeps
//! the overlays).
//!
//! Each run reads the finished picture's own report: Realtime takes the screen AO, Solid does not.
//! The first run shows Realtime can take it here, so a run reporting none is in Solid.
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

use makepad_strict_json::Value;
use makepad_test::{run_with_config, KeyCode, Selector, TestApp, TestConfig};
use std::path::Path;
use std::time::{Duration, Instant};

/// Only a hang fails a wait: a cold or loaded box gets there late, not never.
const HANG: Duration = Duration::from_secs(300);

fn wait_until(app: &TestApp, what: &str, mut ok: impl FnMut() -> bool) {
    let started = Instant::now();
    while !ok() {
        if started.elapsed() > HANG {
            panic!("timed out waiting for {what}; the viewport reports {}", app.locator(Selector::id("viewport")).snapshot().description);
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// What the viewport reports of the recording on display.
fn recording(app: &TestApp) -> Value {
    let viewport = app.locator(Selector::id("viewport")).snapshot();
    makepad_strict_json::parse_depth(viewport.description.as_bytes(), 8).unwrap_or(Value::Null)
}

/// Whether the view's finished picture took the screen AO.
fn finished_with_ao(app: &TestApp) -> bool {
    wait_until(app, "the view's finished picture", || {
        let r = recording(app);
        r.get("settled").and_then(Value::as_bool) == Some(true) && r.get("still_samples").and_then(Value::as_u64) == Some(makepad_render::STILL_AA_SAMPLES as u64)
    });
    recording(app).get("screen_ao").and_then(Value::as_bool).expect("the report says whether it took the screen AO")
}

/// A command of the palette (F3).
fn command(app: &TestApp, name: &str) {
    app.press_key(KeyCode::Escape);
    app.press_key(KeyCode::F3);
    app.type_text(name);
    app.press_key(KeyCode::ReturnKey);
}

/// Picks a shading, and waits for the view to show it and the machine to have it.
fn pick(app: &TestApp, prefs: &Path, name: &str, key: &str, ao: bool) {
    command(app, name);
    wait_until(app, &format!("{name}: the view"), || recording(app).get("screen_ao").and_then(Value::as_bool) == Some(ao));
    wait_until(app, &format!("{name}: saved"), || std::fs::read_to_string(prefs).unwrap_or_default().contains(&format!("shading={key}\n")));
}

#[test]
fn the_chosen_shading_comes_back_on_the_next_run() {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let prefs = std::env::temp_dir().join(format!("fab-view-choices-{}-{nanos}.prefs", std::process::id()));
    std::fs::write(&prefs, "code=0\n").unwrap();
    let config = || {
        let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "view_choices").unwrap();
        config.env.insert("MAKEPAD_HAL_BACKEND".into(), "offscreen".into());
        config.app_args = vec!["--window".into(), "800x600@1".into()];
        config.env.insert("FAB_VIEW_PREFS".into(), prefs.display().to_string());
        config
    };
    let result = run_with_config(config(), |app: TestApp| {
        assert!(finished_with_ao(&app), "nothing chosen: Realtime, its screen AO on");
        pick(&app, &prefs, "Shading: Solid", "solid", false);
    })
    .and_then(|_| {
        run_with_config(config(), |app: TestApp| {
            assert!(!finished_with_ao(&app), "Solid was chosen: the next run took the screen AO, Realtime's");
            pick(&app, &prefs, "Shading: Realtime", "realtime", true);
        })
    })
    .and_then(|_| {
        run_with_config(config(), |app: TestApp| {
            assert!(finished_with_ao(&app), "Realtime was chosen last: the next run has no screen AO");
        })
    });
    let _ = std::fs::remove_file(&prefs);
    result.unwrap();
}
