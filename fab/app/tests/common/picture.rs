//! The view's finished picture, for the tests that compare its pixels. Settled is not finished:
//! the still view's average takes its samples (`STILL_AA_SAMPLES`) after the view settles, and a
//! grab between them catches every edge part way; and a field part's scan goes on from a settled
//! view once the user rests, each of its views changing the shadow and the edges a little. A
//! finished picture repeats exactly, whatever way the view came to it.

use makepad_strict_json::Value;
use makepad_test::{Selector, TestApp};

/// The view's description of the frame it drew last (null before it has one). A snapshot can
/// describe a frame drawn before the newest change reached the view: what the frame shows is in
/// the same description, so check both in one.
pub fn view(app: &TestApp) -> Value {
    let Ok(viewport) = app.locator(Selector::id("viewport")).try_snapshot() else { return Value::Null };
    makepad_strict_json::parse_depth(viewport.description.as_bytes(), 8).unwrap_or(Value::Null)
}

/// Whether a view's description is of its finished picture: settled, traced to the end (scans
/// too), its still average complete.
pub fn finished_in(r: &Value) -> bool {
    let yes = |field: &str| r.get(field).and_then(Value::as_bool) == Some(true);
    yes("settled") && yes("traced") && r.get("still_samples").and_then(Value::as_u64) == Some(makepad_render::STILL_AA_SAMPLES as u64)
}

/// Whether the view shows its finished picture.
pub fn finished(app: &TestApp) -> bool {
    finished_in(&view(app))
}

/// The window grabbed once the view shows its finished picture: the grab's path.
pub fn finished_screenshot(app: &TestApp) -> std::path::PathBuf {
    app.wait_until(|| (!finished(app)).then(|| "waiting for the view's finished picture".to_string()));
    app.screenshot()
}
