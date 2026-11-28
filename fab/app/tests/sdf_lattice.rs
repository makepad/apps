//! A custom distance-kernel part on the plate (the gyroid lattice cube, a Splash `fn(p)` field):
//!
//! - level of detail: a part first traced while the view leaves it small (its shape changed far
//!   out) takes far fewer tiles, and more again when the view comes back (`fab vp sdf tiles: ..
//!   settled at N tiles`);
//! - its shape handle shows in the default Select tool, without entering Edit shape or hovering
//!   a parameter, and takes a drag through real mouse events: while the drag is on, the dragged
//!   field is compiled off the UI thread and traced (`fab vp sdf live`: no meshing, no kernel or
//!   shader compile), nothing fails to draw, and the release commits the value;
//! - shape handles stay visible in Sliced and disappear in Move (it has its own grips).
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

#[path = "common/render_mode.rs"]
mod render_mode;
#[path = "common/wait.rs"]
mod idle_wait;
#[path = "common/parts_tab.rs"]
mod parts_tab;
use idle_wait::wait_poll;
use makepad_script::syntax;
use makepad_strict_json::Value;
use makepad_test::{makepad_test, KeyCode, Role, Selector, TestApp};
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
    let reply = syntax::parse_value(reply).expect("a Splash reply");
    reply.field("head").and_then(|f| f.value.string()).expect("reply has a head").to_string()
}

fn wait_until(app: &TestApp, what: &str, mut ok: impl FnMut() -> bool) {
    wait_poll(app, || {
        if !ok() { Err(format!("timed out waiting for {what}")) } else { Ok(()) }
    });
}

fn lines(app: &TestApp, from: u64, needle: &str) -> Vec<String> {
    app.log_since(from).expect("the app's log").lines.into_iter().filter(|l| l.contains(needle)).collect()
}

fn seq(app: &TestApp) -> u64 {
    app.log_since(0).map(|t| t.last_seq).unwrap_or(0)
}

/// After a view change: the tiles the view ended on, once no later view has followed.
fn settled_tiles(app: &TestApp, from: u64, what: &str) -> u64 {
    let settled = || lines(app, from, "fab vp sdf tiles: lattice/lattice settled at ");
    let mut seen = (0, 0);
    wait_until(app, what, || {
        std::thread::sleep(Duration::from_millis(500));
        let now = settled();
        let last = now.last().and_then(|l| l.split(" settled at ").nth(1)?.split(' ').next()?.parse().ok()).unwrap_or(0);
        let same = !now.is_empty() && seen == (now.len(), last);
        seen = (now.len(), last);
        same
    });
    seen.1
}

/// One mouse event through the app's real event path (answered once a frame has drawn it).
fn mouse(app: &TestApp, kind: &str, x: f64, y: f64) {
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).expect("connect");
    let argv: Vec<String> = ["mouse".to_string(), format!("k={kind}"), format!("x={x:.1}"), format!("y={y:.1}"), "b=0".into(), "wait=1".into()].into();
    makepad_remote::app::run(&client, &argv, &mut |_| true).expect("mouse");
}

/// A declared length parameter's value (fab.params), in millimetres.
fn param(app: &TestApp, key: &str) -> f64 {
    let v = syntax::parse_value(&text(&call(app, "params", "{}").expect("params"))).expect("a Splash reply");
    for part in v.field("parts").and_then(|f| f.value.items()).expect("parts") {
        for p in part.field("params").and_then(|f| f.value.items()).expect("params") {
            if p.field("key").and_then(|f| f.value.string()) == Some(key) {
                return p.field("value").expect("parameter value").value.number().expect("a length") * 1000.0;
            }
        }
    }
    panic!("no parameter {key}");
}

/// Visible shape handles and their grip centres, without moving the pointer.
fn handles(app: &TestApp) -> Vec<(String, (f64, f64))> {
    app.widget_snapshot()
        .nodes()
        .into_iter()
        .filter(|w| w.visible() && w.node.role == Role::Slider && w.node.name.ends_with(" handle"))
        .map(|w| {
            let centre = (w.x() as f64 + w.width() as f64 * 0.5, w.y() as f64 + w.height() as f64 * 0.5);
            (w.node.name, centre)
        })
        .collect()
}

const LATTICE: &str = r##"use mod.model.*
use mod.fab.*
let m = mod.model
let lattice_size = param(24mm, min: 10mm, max: 60mm, step: 0.5mm, label: "Size", group: "Lattice cube")
let lattice_period = param(8mm, min: 3mm, max: 20mm, step: 0.5mm, label: "Period", group: "Lattice cube")
let lattice = m.sdf({
    bounds: [[-lattice_size / 2 - 1mm, -1mm, -lattice_size / 2 - 1mm], [lattice_size / 2 + 1mm, lattice_size + 1mm, lattice_size / 2 + 1mm]]
    cell: 0.5mm
    field: fn(p) {
        let cube = sdf_box(p - vec3(0, lattice_size / 2, 0), vec3(lattice_size, lattice_size, lattice_size))
        return sdf_intersection(sdf_gyroid(p, lattice_period, 0.6mm), cube)
    }
})
handle(lattice_size, Arrow{from: [lattice_size / 2, 0, lattice_size / 2] to: [lattice_size / 2, lattice_size, lattice_size / 2]})
object(lattice)
"##;

#[makepad_test]
fn a_kernel_part_levels_its_detail_and_its_handle_drags_it_live(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    let mut head = String::new();
    wait_until(&app, "the fab tools", || call(&app, "inspect", "{}").map(|l| head = head_of(&text(&l))).is_ok());
    let from = seq(&app);
    let batch = format!(r#"{{"request_id":"lattice-script","expected":{head:?},"script":"lattice.cad.splash","source":{}}}"#, Value::Str(LATTICE.to_string()).to_json());
    let head = head_of(&text(&call(&app, "script", &batch).expect("script")));
    let edits = r#"place("lattice", at: [150mm, 165mm])"#;
    let batch = format!(r#"{{"request_id":"lattice-place","expected":{head:?},"edits":{}}}"#, Value::Str(edits.to_string()).to_json());
    let head = head_of(&text(&call(&app, "place", &batch).expect("place")));
    wait_until(&app, "the lattice drawn by the engine", || !lines(&app, from, "drawn by the engine").is_empty());
    render_mode::wait_shown(&app, "Normal");

    // Far out, the part a few pixels wide (each wheel step dollies 1.4x out; framed it is about
    // 30 px, and under a pixel no ray need land on it): few tiles. Home frames the plate: many again.
    // A part's surface keeps what every look at it found, and its first look holds the view it
    // began from, which can be the framed one when the zoom comes while it runs: once the view is
    // out, a change of period starts the part over there.
    let from = seq(&app);
    for _ in 0..6 {
        app.locator(Selector::id("viewport")).wheel(0.0, 120.0);
    }
    settled_tiles(&app, from, "level of detail: the first shape's tiles from far out");
    let from = seq(&app);
    let edits = r#"params("lattice", {lattice_period: 8.5mm})"#;
    let batch = format!(r#"{{"request_id":"lattice-period","expected":{head:?},"edits":{}}}"#, Value::Str(edits.to_string()).to_json());
    call(&app, "place", &batch).expect("place params");
    wait_until(&app, "level of detail: the changed period traced", || !lines(&app, from, "fab vp sdf tiles: lattice/lattice new shape").is_empty());
    let far = settled_tiles(&app, from, "level of detail: the zoomed-out tiles");
    let from = seq(&app);
    app.locator(Selector::id("viewport")).press_key(KeyCode::Home);
    let back = settled_tiles(&app, from, "level of detail: the framed tiles");
    assert!(far > 0 && far * 4 <= back, "level of detail: far out the part must take at most a quarter of the tiles it takes framed: far {far}, framed {back}");

    // The default Select tool shows the selected part's handle: no Edit shape, tool switch or hover.
    parts_tab::open(&app);
    app.locator(Selector::role(Role::TreeItem).name("lattice")).wait_visible().click();
    app.locator(Selector::id("t_select")).wait_checked(true);
    wait_until(&app, "handles in Select: a visible Size handle without hovering", || handles(&app).iter().any(|(name, _)| name == "Size handle"));
    let (_, h) = handles(&app).into_iter().find(|(name, _)| name == "Size handle").expect("the visible Size handle");
    let before = param(&app, "lattice/lattice_size");

    // Drag it by real mouse events and hold: the dragged field reaches the glass while the drag is on.
    let from = seq(&app);
    mouse(&app, "down", h.0, h.1);
    for i in 1..=4 {
        mouse(&app, "move", h.0, h.1 - 10.0 * i as f64);
    }
    wait_until(&app, "live drag: a dragged field on the glass while the drag is on (`fab vp sdf live`)", || !lines(&app, from, "fab vp sdf live:").is_empty());
    mouse(&app, "up", h.0, h.1 - 40.0);
    wait_until(&app, "handle drag: lattice/lattice_size to change after the release", || (param(&app, "lattice/lattice_size") - before).abs() > 1e-6);
    let fails = lines(&app, from, "cannot draw its field");
    assert!(fails.is_empty(), "live drag: a field could not be drawn: {fails:?}");

    // Sliced keeps the shape handles; Move replaces them with its own grips.
    render_mode::pick(&app, "Sliced");
    app.locator(Selector::id("t_select")).wait_checked(true);
    wait_until(&app, "handles in Sliced: visible shape handles in Select", || !handles(&app).is_empty());
    app.locator(Selector::id("t_move")).wait_visible().click();
    app.locator(Selector::id("t_move")).wait_checked(true);
    wait_until(&app, "handles in Move: no shape handles", || handles(&app).is_empty());
}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);
