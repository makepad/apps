//! What the tests that wait on a slice share: the slice's state as the app reports it.

use makepad_strict_json::Value;
use makepad_test::TestApp;

/// The slice's state from `fab.slice`: `Off`, `Waiting`, `Sliced`, `Failed`, or the percentage
/// while a slice runs (`37%`); empty until the app answers.
pub fn slice_state(app: &TestApp) -> String {
    let Ok(client) = makepad_remote::Client::connect(&app.remote_endpoint()) else { return String::new() };
    let mut reply = None;
    let args = ["call".to_string(), "fab.slice".to_string(), "{}".to_string()];
    let ran = makepad_remote::app::run(&client, &args, &mut |line| {
        if line.contains("\"Result\"") {
            reply = Some(line.to_string());
        }
        true
    });
    let Some(line) = reply.filter(|_| ran.is_ok()) else { return String::new() };
    let json = makepad_strict_json::parse_depth(line.as_bytes(), 32).expect("result line is JSON");
    let result = match json.get("Result").expect("Result") {
        Value::Arr(a) => a[0].clone(),
        other => other.clone(),
    };
    let text = result.get("text").and_then(Value::as_str).unwrap_or_default();
    let Ok(slice) = makepad_script::syntax::parse_value(text) else { return String::new() };
    let state = slice.field("state").and_then(|f| f.value.string()).unwrap_or_default().to_string();
    match state.as_str() {
        "Slicing" => format!("{:.0}%", 100.0 * slice.field("fraction").and_then(|f| f.value.number()).unwrap_or(0.0)),
        _ => state,
    }
}
