//! The device's controls work in its 3D view, one after another, and the bar's board toggle shows
//! and hides the board in place.
//!
//! Why (user, 11-18): "in assembled its ok but on the 3D view it kinds only works once and then
//! not for others": a 3D press was taken only while the bench played, so once a take had stopped
//! no button answered. "im having lots of trouble trying to 'grab' the potentiometer in the 3D
//! view": a press had to land on the shaft itself. And "the show circuit board in place button
//! also doesnt toggle it 'off'": with a design's other boards beside the held one, the board layer
//! never hid.
//!
//! The drum machine demo, which opens in Electronics. In Design the bar's "Show board" toggle starts on with the board drawn;
//! a press turns both off, another back on (the view's description says whether it draws a board).
//! The second press comes as soon as the first shows, before its edit's plate is built: a plate
//! built before that edit landing between the two once undid the hide, the second press read the
//! board as shown and hid it again, and the toggle stayed off (11-25).
//! Then the Device + Scope workspace: the device view's description lists its live controls
//! with where on the screen a press takes each. Two different buttons are pressed in turn, the
//! second a few points off its grip, which a press must still take: each must be held by the
//! bench while the pointer is down and let go after. Then a knob is dragged up a third of the
//! drag's full travel, and its place must follow. Each press and release must be taken by the
//! view within a few of its frames, a lost one failing at once; the bench's answer to it may take
//! as long as its run does.
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

use makepad_strict_json::Value;
use makepad_test::{run_with_config, Role, Selector, TestApp, TestConfig};
use std::time::Duration;

/// The app main thread's CPU budget per wait for what takes long (a workspace's boards and bench
/// set up); the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);
/// The frames the view draws after a press (or its release) by which it must have taken it: the
/// board toggle flips with the plate's flag at once, a press on a control is the view's on its
/// down and lets go on its up. One not taken by then is lost and fails then, not after the whole
/// CPU budget of a view that keeps drawing. What the bench makes of it (a hold that lasts its
/// shortest press in simulated time, a turn) is waited for as long as the run takes.
const PRESS_FRAMES: u64 = 60;
/// How far off its grip (points) the second button is pressed: within the 28 point target.
const OFF_GRIP: f64 = 8.0;
/// The knob's drag (points up), a third of the full travel's 150.
const TURN: f64 = 50.0;

fn wait_until(app: &TestApp, view: &str, what: &str, mut ok: impl FnMut() -> bool) {
    app.try_wait_until_within(HANG, || (!ok()).then(|| format!("waiting for {what}")))
        .unwrap_or_else(|why| fail(app, view, &format!("{why}; timed out waiting for {what}")));
}

/// That `view` took a press (or release): `what`, within [`PRESS_FRAMES`] of its draws.
fn taken(app: &TestApp, view: &str, what: &str, mut ok: impl FnMut() -> bool) {
    let drawn = |app: &TestApp| report(app, view).get("records").and_then(|r| r.get("draws")).and_then(num);
    let start = drawn(app);
    let mut lost = None;
    wait_until(app, view, what, || {
        if ok() {
            return true;
        }
        let frames = drawn(app).zip(start).map_or(0.0, |(now, start)| now - start);
        lost = (frames > PRESS_FRAMES as f64).then_some(frames);
        lost.is_some()
    });
    if let Some(frames) = lost {
        fail(app, view, &format!("{what}: not taken {frames} frames after it was sent, a lost input (one is taken within {PRESS_FRAMES})"));
    }
}

/// Fails the test with `why`, what `view` reports of itself and the window's tabs and toggles.
fn fail(app: &TestApp, view: &str, why: &str) -> ! {
    let shown: Vec<String> = app.try_widget_snapshot().map(|s| s.nodes()).unwrap_or_default().iter()
        .filter(|n| n.visible() && matches!(n.role, Role::Tab | Role::ToggleButton | Role::Canvas))
        .map(|n| format!("{:?} {:?} {:?}", n.role, n.name, n.checked())).collect();
    panic!("{why}; the view reports {:?}; the window shows {shown:?}",
        app.locator(Selector::id(view)).try_snapshot().map_or_else(|err| format!("nothing: its read was not answered ({err})"), |n| n.description.clone()));
}

/// What a viewport reports of itself (`FabViewport::semantics`). A read the busy app does not
/// answer is read again until it answers, never taken for an empty report; a read lost for the
/// whole wait fails, naming it.
fn report(app: &TestApp, view: &str) -> Value {
    let text = match app.locator(Selector::id(view)).try_snapshot() {
        Ok(node) => node.description.clone(),
        Err(_) => {
            let mut text = String::new();
            app.try_wait_until_within(HANG, || match app.locator(Selector::id(view)).try_snapshot() {
                Ok(node) => {
                    text = node.description.clone();
                    None
                }
                Err(err) => Some(format!("a read of {view} the app did not answer ({err})")),
            }).unwrap_or_else(|why| panic!("{why}"));
            text
        }
    };
    makepad_strict_json::parse_depth(text.as_bytes(), 8).unwrap_or_else(|err| panic!("{view} reports no JSON ({err}): {text}"))
}

/// View › Workspace › `name`.
fn workspace(app: &TestApp, name: &str) {
    app.locator(Selector::role(Role::Button).name("View")).wait_visible().press();
    for item in ["Workspace", name] {
        app.locator(Selector::role(Role::MenuItem).name(item)).wait_visible().press();
    }
}

fn boards_drawn(app: &TestApp) -> Option<bool> {
    report(app, "viewport").get("boards_drawn").and_then(Value::as_bool)
}

/// The board toggle's lit state, once it shows.
fn board_toggle(app: &TestApp) -> Option<bool> {
    app.locator(Selector::role(Role::ToggleButton).name("Show board")).try_snapshot().ok().and_then(|n| n.checked())
}

/// A JSON number, whole or not.
fn num(v: &Value) -> Option<f64> {
    match v {
        Value::F64(f) => Some(*f),
        Value::Int(i) => Some(*i as f64),
        _ => None,
    }
}

#[derive(Clone, Debug)]
struct Control {
    name: String,
    knob: bool,
    at: (f64, f64),
    held: bool,
    place: Option<f64>,
}

/// The device view's live controls.
fn controls(app: &TestApp) -> Vec<Control> {
    let r = report(app, "device_viewport");
    let Some(list) = r.get("controls").and_then(Value::as_arr) else { return Vec::new() };
    list.iter().filter_map(|c| {
        let at = match c.get("at")?.as_arr()? { [x, y] => (num(x)?, num(y)?), _ => return None };
        Some(Control {
            name: c.get("name")?.as_str()?.to_string(),
            knob: c.get("knob")?.as_bool()?,
            at,
            held: c.get("held")?.as_bool()?,
            place: c.get("place").and_then(num),
        })
    }).collect()
}

fn control(app: &TestApp, name: &str) -> Option<Control> {
    controls(app).into_iter().find(|c| c.name == name)
}

/// The device view holds a press on one of its controls.
fn claimed(app: &TestApp) -> bool {
    report(app, "device_viewport").get("claimed").and_then(Value::as_bool) == Some(true)
}

#[test]
fn the_device_view_takes_each_press_and_turn_and_the_board_toggle_hides_the_board() {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "elec_device_controls").unwrap();
    config.env.insert("MAKEPAD_HAL_BACKEND".into(), "offscreen".into());
    config.app_args = vec!["--window".into(), "1600x1000@1".into(), "--demo".into(), "drum16".into()];
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("fab-elec-device-controls-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&dir).expect("a fresh folder for the test's preferences");
    let prefs = dir.join("view.prefs");
    std::fs::write(&prefs, "code=0\n").expect("the test's view preferences");
    config.env.insert("FAB_VIEW_PREFS".into(), prefs.display().to_string());
    let result = run_with_config(config, |app: TestApp| {
        app.locator(Selector::role(Role::Window)).wait_visible();
        workspace(&app, "3D modeling");

        // Design: the board in place, its toggle on; off hides it, on shows it again.
        wait_until(&app, "viewport", "the board drawn with its toggle on", || board_toggle(&app) == Some(true) && boards_drawn(&app) == Some(true));
        for on in [false, true] {
            app.locator(Selector::role(Role::ToggleButton).name("Show board")).wait_visible().press();
            taken(&app, "viewport", &format!("the toggle {} and the board {}", if on { "on" } else { "off" }, if on { "drawn" } else { "hidden" }),
                || board_toggle(&app) == Some(on) && boards_drawn(&app) == Some(on));
        }

        // The Device + Scope workspace: the device view and its controls.
        workspace(&app, "Device + Scope");
        wait_until(&app, "device_viewport", "two buttons and a knob in the device view", || {
            let c = controls(&app);
            c.iter().filter(|c| !c.knob).count() >= 2 && c.iter().any(|c| c.knob)
                && report(&app, "device_viewport").get("settled").and_then(Value::as_bool) == Some(true)
        });
        let all = controls(&app);
        let mut buttons: Vec<&Control> = all.iter().filter(|c| !c.knob).collect();
        buttons.sort_by(|a, b| a.name.cmp(&b.name));
        buttons.dedup_by(|a, b| a.name == b.name);
        assert!(buttons.len() >= 2, "two different buttons in the device view: {all:?}");

        // Each button in turn: held while the pointer is down, let go after.
        for (i, button) in buttons[..2].iter().enumerate() {
            let off = if i == 0 { 0.0 } else { OFF_GRIP };
            let press = app.press_at(button.at.0 + off, button.at.1);
            taken(&app, "device_viewport", &format!("the view takes press {} on {} ({off} points off its grip)", i + 1, button.name), || claimed(&app));
            wait_until(&app, "device_viewport", &format!("press {} holds {}", i + 1, button.name), || control(&app, &button.name).is_some_and(|c| c.held));
            press.release();
            taken(&app, "device_viewport", &format!("the view takes the release of {}", button.name), || !claimed(&app));
            wait_until(&app, "device_viewport", &format!("{} let go", button.name), || control(&app, &button.name).is_some_and(|c| !c.held));
        }

        // A knob follows a drag up while the pointer is down.
        let knob = all.iter().find(|c| c.knob).expect("a knob").clone();
        let before = knob.place.expect("a knob reports its place");
        let target = (before + TURN / 150.0).min(1.0);
        let mut drag = app.press_at(knob.at.0, knob.at.1);
        taken(&app, "device_viewport", &format!("the view takes the press on {}", knob.name), || claimed(&app));
        drag.move_by(0.0, -TURN);
        wait_until(&app, "device_viewport", &format!("{} turned from {before:.3} toward {target:.3}", knob.name),
            || control(&app, &knob.name).and_then(|c| c.place).is_some_and(|p| (p - target).abs() < 0.02));
        drag.release();
    });
    let cleanup = std::fs::remove_dir_all(&dir);
    result.unwrap();
    cleanup.expect("remove the test's temporary folder");
}
