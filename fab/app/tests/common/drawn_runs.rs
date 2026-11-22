//! What frames drew of each slice revision, as `fab.slice` tells it.

use makepad_strict_json::Value;
use makepad_test::TestApp;

/// A tile a frame drew first.
pub struct Tile {
    pub part: usize,
    pub layer: u32,
    /// The newest revision that planned its layer.
    pub revision: u64,
    /// When, after the edit (ms).
    pub ms: f64,
}

/// What frames drew of one revision's previews.
pub struct Run {
    pub revision: u64,
    pub frames: u64,
    /// How many of its own tiles were on screen before its whole slice was in.
    pub before_whole: u64,
    pub tiles: Vec<Tile>,
}

fn slice(app: &TestApp, arguments: &str) -> makepad_script::syntax::Node {
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).expect("the app's tools");
    let mut reply = None;
    let args = ["call".to_string(), "fab.slice".to_string(), arguments.to_string()];
    makepad_remote::app::run(&client, &args, &mut |line| {
        if line.contains("\"Result\"") {
            reply = Some(line.to_string());
        }
        true
    }).expect("fab.slice");
    let line = reply.expect("fab.slice answers");
    let json = makepad_strict_json::parse_depth(line.as_bytes(), 32).expect("result line is JSON");
    let result = match json.get("Result").expect("Result") {
        Value::Arr(a) => a[0].clone(),
        other => other.clone(),
    };
    let text = result.get("text").and_then(Value::as_str).unwrap_or_default();
    makepad_script::syntax::parse_value(text).expect("a Splash reply")
}

/// The runs the app keeps, oldest first.
pub fn runs(app: &TestApp) -> Vec<Run> {
    let reply = slice(app, r#"{"drawn":true}"#);
    let number = |node: &makepad_script::syntax::Node, name: &str| node.field(name).and_then(|f| f.value.number()).unwrap_or_else(|| panic!("no {name}"));
    let items = |node: &makepad_script::syntax::Node, name: &str| node.field(name).and_then(|f| f.value.items()).unwrap_or_else(|| panic!("no {name}")).to_vec();
    items(&reply, "drawn").iter().map(|run| {
        let revision = number(run, "revision") as u64;
        let mut tiles = Vec::new();
        loop {
            let page = slice(app, &format!(r#"{{"drawn":true,"revision":{revision},"from":{}}}"#, tiles.len()));
            tiles.extend(items(&page, "tiles").iter().map(|t| Tile { part: number(t, "part") as usize, layer: number(t, "layer") as u32, revision: number(t, "revision") as u64, ms: number(t, "ms") }));
            if number(&page, "left") == 0.0 { break; }
        }
        Run {
            revision,
            frames: number(run, "frames") as u64,
            before_whole: number(run, "before_whole") as u64,
            tiles,
        }
    }).collect()
}
