//! Dragging a value in the part panel, by real pointer events: press, move, rest, release. The
//! value stays where it was let go and the part is built at it, however long the pointer rested
//! before the release; one undo puts it back. Each gesture is one sample of a race: a preview of
//! the final value may or may not have come back when the button goes up, so a mesh part (the
//! bracket: drafts while it drags), a field part (the vase: no drafts) and an exact part (a shelled
//! housing: a slow rebuild) are each dragged with a release at once and after a rest. A drag
//! away and back, with the rest long enough for the preview and the tool's ghost of the far value,
//! changes nothing and leaves nothing behind (for a control and for a shape handle), and a commit
//! the model refuses goes back to what the model holds and says why.
//!
//! Each test places only the parts it drags. HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`),
//! native GPU, no window.

#[path = "common/wait.rs"]
mod idle_wait;
use idle_wait::wait_poll;
use makepad_script::syntax::{self, Node};
use makepad_strict_json::Value;
use makepad_test::{makepad_test, run_with_config, KeyCode, KeyModifiers, Role, Selector, TestApp, TestConfig};
use std::time::{Duration, Instant};

fn call(app: &TestApp, tool: &str, args: &str) -> Result<String, String> {
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).map_err(|e| format!("connect: {e:?}"))?;
    let argv = vec!["call".to_string(), format!("fab.{tool}"), args.to_string()];
    let mut lines = Vec::new();
    makepad_remote::app::run(&client, &argv, &mut |line| {
        lines.push(line.to_string());
        true
    })
    .map_err(|e| format!("fab.{tool}: {e:?}"))?;
    let line = lines.iter().find(|l| l.contains("\"Result\"")).ok_or_else(|| format!("fab.{tool} returned no result: {lines:?}"))?;
    let v = makepad_strict_json::parse_depth(line.as_bytes(), 32).expect("result line is JSON");
    let r = match v.get("Result").expect("Result") {
        Value::Arr(a) => a[0].clone(),
        other => other.clone(),
    };
    Ok(r.get("text").and_then(Value::as_str).unwrap_or_default().to_string())
}

fn splash(text: &str) -> Node {
    syntax::parse_value(text).expect("a Splash reply")
}

fn head_of(reply: &str) -> String {
    splash(reply).field("head").and_then(|f| f.value.string()).expect("a head").to_string()
}

fn wait_tools(app: &TestApp) -> String {
    app.locator(Selector::role(Role::Window)).wait_visible();
    wait_poll(app, || {
        call(app, "inspect", "{}").map_err(|e| format!("the fab tools never appeared: {e}"))
    })
}

/// A parameter's value in the model (`fab.params`, in millimetres for a length).
fn param(app: &TestApp, key: &str) -> f64 {
    let v = splash(&call(app, "params", "{}").expect("params"));
    for part in v.field("parts").and_then(|f| f.value.items()).expect("parts") {
        for p in part.field("params").and_then(|f| f.value.items()).expect("params") {
            if p.field("key").and_then(|f| f.value.string()) == Some(key) {
                return p.field("value").expect("parameter value").value.number().expect("a number") * 1000.0;
            }
        }
    }
    panic!("no parameter {key}");
}

/// The part as built: its size and, for a mesh part, volume (`fab.inspect`).
fn geometry(app: &TestApp, name: &str) -> String {
    let v = splash(&call(app, "inspect", r#"{"program":false}"#).expect("inspect"));
    let parts = v.field("plate").and_then(|p| p.value.field("parts")).and_then(|f| f.value.items()).expect("plate parts");
    let part = parts.iter().find(|p| p.field("name").and_then(|f| f.value.string()) == Some(name)).unwrap_or_else(|| panic!("no part {name}"));
    let size: Vec<f64> = part.field("size").and_then(|f| f.value.items()).expect("a size").iter().map(|x| x.number().expect("a length") * 1000.0).collect();
    let volume = part.field("volume_mm3").and_then(|f| f.value.number());
    format!("{size:.3?} {volume:.1?}")
}

/// What the panel's control shows.
fn shown(app: &TestApp, label: &str) -> String {
    app.widget_snapshot()
        .nodes()
        .iter()
        .find(|w| w.visible() && w.node.role == Role::SpinButton && w.node.name == label)
        .map(|w| w.node.value.clone())
        .unwrap_or_else(|| panic!("no control {label}"))
}

fn number(text: &str) -> f64 {
    text.split_whitespace().next().and_then(|x| x.trim_end_matches('°').parse().ok()).unwrap_or_else(|| panic!("not a number: {text:?}"))
}

fn rebuilding(app: &TestApp) -> bool {
    app.widget_snapshot().nodes().iter().any(|w| w.visible() && w.node.role == Role::Status && w.node.name.starts_with("Rebuilding"))
}

/// Nothing rebuilding and the same geometry for `quiet`: the answer to the gesture is in.
fn settle(app: &TestApp, part: &str, quiet: Duration) -> String {
    let mut last = geometry(app, part);
    let mut same = Instant::now();
    wait_poll(app, || {
        let now = geometry(app, part);
        if rebuilding(app) || now != last {
            same = Instant::now();
            last = now;
        } else if same.elapsed() > quiet {
            return Ok(last.clone());
        }
        Err(format!("{part} never settled"))
    })
}

/// After a release that should have changed the value: the model holds another value than
/// `before` (or the runtime ends the wait: it went back), then the part has settled.
fn settle_after_commit(app: &TestApp, part: &str, key: &str, before: f64) -> String {
    if let Err(why) = app.try_wait_until_within(HANG, || {
        ((param(app, key) - before).abs() < 1e-6).then(|| format!("waiting for {key} to change after release"))
    }) {
        println!("{why}");
    }
    settle(app, part, Duration::from_millis(400))
}

fn pick_part(app: &TestApp, name: &str) {
    app.locator(Selector::role(Role::Tab).name("Parts")).click();
    app.locator(Selector::role(Role::TreeItem).name(name)).wait_visible().click();
    app.locator(Selector::id("t_shape")).wait_visible().click();
}

fn primary(shift: bool) -> KeyModifiers {
    if cfg!(target_os = "macos") {
        KeyModifiers { shift, logo: true, ..Default::default() }
    } else {
        KeyModifiers { shift, control: true, ..Default::default() }
    }
}

fn log_tail(app: &TestApp) {
    if let Ok(log) = app.log_since(0) {
        for l in log.lines.iter().filter(|l| l.contains("fab: ") || l.contains("fab param")).rev().take(24).collect::<Vec<_>>().into_iter().rev() {
            println!("[app] {}", &l[l.find(" - ").map_or(0, |i| i + 3)..]);
        }
    }
}

/// A preview of the dragged value has been built and is shown (the plate view's "Plate preview").
fn preview_shown(app: &TestApp) -> bool {
    app.widget_snapshot().nodes().iter().any(|w| w.visible() && w.node.role == Role::Status && w.node.name == "Plate preview")
}

fn tool_ghost(app: &TestApp) -> bool {
    app.widget_snapshot().nodes().iter().any(|w| w.visible() && w.node.role == Role::Status && w.node.name == "Cutting tool shown")
}

/// Nothing of the gesture stays once it is over: no tool ghost, no preview.
fn assert_nothing_left(app: &TestApp, after: &str) {
    let nodes = app.widget_snapshot().nodes();
    assert!(!nodes.iter().any(|w| w.visible() && w.node.role == Role::Status && w.node.name == "Cutting tool shown"), "{after}: the tool's ghost stayed");
    assert!(!nodes.iter().any(|w| w.visible() && w.node.role == Role::Status && w.node.name == "Plate preview"), "{after}: the plate still shows a preview");
}

/// Away and back in one gesture, resting long enough for the preview and the ghost of the far
/// value: the release changes nothing and leaves nothing behind.
fn drag_away_and_back(app: &TestApp, part: &str, label: &str, key: &str) {
    let (before, before_geo) = (param(app, key), settle(app, part, Duration::from_millis(400)));
    let mut drag = app.locator(Selector::role(Role::SpinButton).name(label)).wait_visible().press_and_drag();
    // The value counts from where the drag engages (3 px in).
    drag.move_by(3.0, 0.0);
    drag.move_by(43.0, 0.0);
    wait_poll(app, || {
        if !tool_ghost(app) { Err(format!("{label}: the tool's ghost never showed during the drag")) } else { Ok(()) }
    });
    drag.move_by(3.0, 0.0);
    drag.release();
    let after_geo = settle(app, part, Duration::from_millis(600));
    assert_eq!(param(app, key), before, "{label}: away and back changes nothing");
    assert_eq!(after_geo, before_geo, "{label}: the committed plate is back");
    assert!((number(&shown(app, label)) - before).abs() < 0.06, "{label}: the control does not show {before}");
    assert_nothing_left(app, &format!("{label}: a cancel"));
}

/// A shape handle dragged away and back in one gesture: the release changes nothing and leaves
/// nothing behind (the preview of the far value is gone).
fn handle_away_and_back(app: &TestApp, part: &str, key: &str) {
    let handle = Selector::role(Role::Slider).name("Thickness handle");
    let (before, before_geo) = (param(app, key), settle(app, part, Duration::from_millis(400)));
    let mut drag = app.locator(handle).wait_visible().press_and_drag();
    drag.move_by(0.0, -40.0);
    wait_poll(app, || {
        if !preview_shown(app) { Err("the far value's preview never came".to_string()) } else { Ok(()) }
    });
    drag.move_by(0.0, 0.0);
    drag.release();
    let after_geo = settle(app, part, Duration::from_millis(600));
    assert_eq!(param(app, key), before, "a handle away and back changes nothing");
    assert_eq!(after_geo, before_geo, "the committed plate is back after a handle away and back");
    assert_nothing_left(app, "a handle away and back");
}

/// A release at once, while the preview of the value is still being built (`false`), and a release
/// once the preview of the far value is back and shown (`true`): the two ends of the race, with
/// nothing waited on by the clock. The direction alternates; the gestures that went back are
/// added to `snapped`, `before_geo` follows the part's geometry.
fn drag_holds(app: &TestApp, part: &str, label: &str, key: &str, after_preview: &[bool], before_geo: &mut String, snapped: &mut Vec<String>) {
    let control = Selector::role(Role::SpinButton).name(label);
    let mut dx = 40.0;
    for &after_preview in after_preview {
        let when = if after_preview { "after its preview" } else { "at once" };
        let before = param(app, key);
        let mut drag = app.locator(control.clone()).wait_visible().press_and_drag();
        drag.move_by(dx, 0.0);
        if after_preview {
            wait_poll(app, || {
                if !preview_shown(app) { Err(format!("{label}: the preview of the far value never came while the pointer rested")) } else { Ok(()) }
            });
        }
        drag.release();
        let released = number(&shown(app, label));
        let after_geo = settle_after_commit(app, part, key, before);
        let (after, control_after) = (param(app, key), number(&shown(app, label)));
        let went_back = (after - before).abs() < 1e-6 || (control_after - after).abs() > 0.06 || (released - after).abs() > 0.06 || after_geo == *before_geo;
        if went_back {
            snapped.push(format!("{label} released {when}, dx {dx}: {before} -> released at {released} -> model {after}, control {control_after}, geometry {before_geo} -> {after_geo}"));
        }
        assert_nothing_left(app, &format!("{label}: a commit released {when}"));
        *before_geo = after_geo;
        dx = -dx;
    }
}

fn place_script(app: &TestApp, head: &str, name: &str, file: &str, source: &str, object: &str) -> String {
    let batch = format!(r#"{{"request_id":"{name}-script","expected":{head:?},"script":{file:?},"source":{}}}"#, Value::Str(source.to_string()).to_json());
    let reply = call(app, "script", &batch).expect("script");
    let head = head_of(&reply);
    let stem = file.split('.').next().expect("a file name");
    let edits = format!(r#"place({stem:?}, object: {object:?}, name: {name:?})"#);
    let batch = format!(r#"{{"request_id":"{name}-place","expected":{head:?},"edits":{}}}"#, Value::Str(edits).to_json());
    let reply = call(app, "place", &batch).expect("place");
    assert!(!reply.contains("error:"), "{reply}");
    head_of(&reply)
}

/// Below 7.95 mm the height is negative: every 0.1 mm step under 8 mm is in the control's range
/// and does not build, so the drag has no other value to be capped at and is refused.
const POST: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet r = param(8mm, min: 2mm, max: 12mm, step: 0.1mm, label: \"Post radius\", kind: \"radius\")\nlet post = m.cylinder({radius: r - 6mm height: (r - 7.95mm) * 600 segments: 48})\nobject(post)\n";

/// An exact part that still takes a good part of a second to rebuild: a sketch pulled up, a fillet
/// round its top, a shell. What makes the race is the rebuild being long next to the release.
const EXACT: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet size = param(80mm, min: 60mm, max: 140mm, step: 0.5mm, label: \"Housing size\")\nlet block = m.brep.extrude({sketch: Rect{size: [size size] corner: 10mm} height: 30mm})\nlet rounded = m.brep.fillet(block, 4mm, {edges: {top: true}})\nlet housing = m.brep.shell(rounded, 2.4mm, {open: @bottom})\nobject(housing)\n";

type Part = (&'static str, &'static str, &'static str, &'static str);

const BRACKET: Part = ("Shelf bracket", "bracket.cad.splash", include_str!("../resources/bracket.cad.splash"), "bracket");
const VASE: Part = ("Bud vase", "vase.cad.splash", include_str!("../resources/vase.cad.splash"), "vase");
const EXACT_PART: Part = ("Housing", "housing.cad.splash", EXACT, "housing");
const POST_PART: Part = ("Post", "post.cad.splash", POST, "post");

/// The parts, placed one after the other on the empty plate.
fn place_parts(app: &TestApp, parts: &[Part]) {
    let mut head = head_of(&wait_tools(app));
    for (name, file, source, object) in parts {
        head = place_script(app, &head, name, file, source, object);
    }
}

fn quiet() -> Duration {
    Duration::from_millis(400)
}

/// A mesh part drafts while it drags (released at once, in the middle of a preview, and after the
/// preview is back), a field part has no drafts; one undo takes back the last drag only. The
/// bracket and the vase stand for the demo plate's parts; the gyroid lattice cube is left out.
#[makepad_test]
fn a_dragged_value_stays_where_it_was_let_go_on_the_demo_plate(app: TestApp) {
    place_parts(&app, &[BRACKET, VASE]);
    let mut snapped = Vec::new();
    pick_part(&app, "Shelf bracket");
    let mut geo = settle(&app, "Shelf bracket", quiet());
    drag_holds(&app, "Shelf bracket", "Screw hole", "bracket/hole", &[false, true], &mut geo, &mut snapped);
    drag_holds(&app, "Shelf bracket", "Width", "bracket/width", &[false], &mut geo, &mut snapped);
    // One undo takes back the last drag only: the value and the part go back with it.
    let (last, last_geo) = (param(&app, "bracket/width"), geo.clone());
    app.press_key_with_modifiers(KeyCode::KeyZ, primary(false));
    let undone_geo = settle(&app, "Shelf bracket", quiet());
    let undone = param(&app, "bracket/width");
    assert!((undone - last).abs() > 1e-6, "Width: undo left the value at {last}");
    assert_ne!(undone_geo, last_geo, "Width: undo left the geometry");
    assert!((number(&shown(&app, "Width")) - undone).abs() < 0.06, "Width: the control does not show the undone value {undone}");
    drag_away_and_back(&app, "Shelf bracket", "Screw hole", "bracket/hole");
    pick_part(&app, "Bud vase");
    let mut geo = settle(&app, "Bud vase", quiet());
    drag_holds(&app, "Bud vase", "Height", "vase/vase_height", &[false], &mut geo, &mut snapped);
    if !snapped.is_empty() {
        log_tail(&app);
    }
    assert!(snapped.is_empty(), "{} drags went back:\n{}", snapped.len(), snapped.join("\n"));
}

/// A field part follows the drag on the glass: the shape drawn keeps moving on while the button
/// is down, whatever else the view has to draw. (The new shape alone must make the view record:
/// a drag in the panel changes nothing else the view looks at.)
#[makepad_test]
fn a_field_part_follows_a_panel_drag_on_the_glass(app: TestApp) {
    place_parts(&app, &[VASE]);
    pick_part(&app, "Bud vase");
    settle(&app, "Bud vase", quiet());
    let drawn = || app.log_since(0).map_or(0, |log| log.lines.iter().filter(|l| l.contains("fab vp sdf live:")).count());
    let mut drag = app.locator(Selector::role(Role::SpinButton).name("Belly")).wait_visible().press_and_drag();
    // The value counts from where the drag engages (3 px in).
    drag.move_by(3.0, 0.0);
    // Each step moves the pointer 5 px on (`move_by` goes to an offset from the press point), right
    // for the first half and back for the second; a step reached the glass if a new shape was drawn
    // while it moved.
    const STEPS: usize = 30;
    let (mut x, mut seen, mut shown) = (3.0, drawn(), 0);
    for step in 0..STEPS {
        x += if step < STEPS / 2 { 5.0 } else { -5.0 };
        drag.move_by(x, 0.0);
        let now = drawn();
        if now > seen {
            shown += 1;
            seen = now;
        }
    }
    drag.release();
    settle(&app, "Bud vase", quiet());
    assert!(shown * 5 >= STEPS, "{shown} of {STEPS} steps of the drag reached the glass before the release");
}

/// An exact part rebuilds slowly: the rest before the release is short next to the rebuild.
#[makepad_test]
fn a_dragged_value_stays_where_it_was_let_go_on_an_exact_part(app: TestApp) {
    place_parts(&app, &[EXACT_PART]);
    pick_part(&app, "Housing");
    let mut snapped = Vec::new();
    let mut geo = settle(&app, "Housing", quiet());
    drag_holds(&app, "Housing", "Housing size", "housing/size", &[false, true], &mut geo, &mut snapped);
    if !snapped.is_empty() {
        log_tail(&app);
    }
    assert!(snapped.is_empty(), "{} drags went back:\n{}", snapped.len(), snapped.join("\n"));
}

/// The values of `key` whose previews were shown, oldest first (`FAB_PARAM_LOG`'s "shown" lines).
fn shown_previews(app: &TestApp, key: &str) -> Vec<f64> {
    logged_values(app, &format!("fab param shown {key} = Num("))
}

/// The values of `key` whose preview started building, in order (`FAB_PARAM_LOG`).
fn built_previews(app: &TestApp, key: &str) -> Vec<f64> {
    logged_values(app, &format!("fab param building {key} = Num("))
}

fn logged_values(app: &TestApp, said: &str) -> Vec<f64> {
    let log = app.log_since(0).expect("the app's log");
    log.lines.iter().filter_map(|l| l.split_once(said).and_then(|(_, rest)| rest.split(')').next()).and_then(|x| x.parse().ok())).collect()
}

/// How many previews of `key` a stop ended before they showed (`FAB_PARAM_LOG`).
fn stopped_previews(app: &TestApp, key: &str) -> usize {
    let said = format!("fab param stopped {key} gen");
    app.log_since(0).expect("the app's log").lines.iter().filter(|l| l.contains(said.as_str())).count()
}

/// An exact part dragged on and on at frame rate shows shapes on the way: the preview being built
/// when the pointer moves on is finished and shown, the values asked meanwhile come down to the
/// newest, which is built next. Once the pointer rests (the button still down), the last shape
/// shown is the newest value's.
#[test]
fn an_exact_part_shows_its_shapes_while_it_is_dragged() {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "an_exact_part_shows_its_shapes_while_it_is_dragged").unwrap();
    config.env.insert("FAB_PARAM_LOG".into(), "1".into());
    if let Err(e) = run_with_config(config, |app: TestApp| {
        const LABEL: &str = "Housing size";
        const KEY: &str = "housing/size";
        // Moves a frame apart, a fifth of a pixel each (a tenth of a millimetre): far longer than a
        // rebuild of the housing, and still short of the size's maximum.
        const MOVES: usize = 600;
        const FRAME: Duration = Duration::from_millis(16);
        place_parts(&app, &[EXACT_PART]);
        pick_part(&app, "Housing");
        settle(&app, "Housing", quiet());
        let before = shown_previews(&app, KEY).len();
        let mut drag = app.locator(Selector::role(Role::SpinButton).name(LABEL)).wait_visible().press_and_drag();
        // The value counts from where the drag engages (3 px in).
        drag.move_by(3.0, 0.0);
        let mut moves = 0;
        while shown_previews(&app, KEY).len() == before {
            assert!(moves < MOVES, "{MOVES} moves a frame apart and no shape shown on the way: a newer value stopped every preview");
            moves += 1;
            drag.move_by(3.0 + moves as f64 * 0.2, 0.0);
            std::thread::sleep(FRAME);
        }
        // A few more moves, then the pointer rests on the newest value.
        for _ in 0..20 {
            moves += 1;
            drag.move_by(3.0 + moves as f64 * 0.2, 0.0);
            std::thread::sleep(FRAME);
        }
        let newest = number(&shown(&app, LABEL));
        wait_poll(&app, || {
            if shown_previews(&app, KEY).last().is_none_or(|v| (v - newest).abs() > 0.06) { Err(format!("the newest value {newest} was never shown while the pointer rested: {:?}", &shown_previews(&app, KEY)[before..])) } else { Ok(()) }
        });
        let on_the_way = shown_previews(&app, KEY)[before..].to_vec();
        assert!(on_the_way.len() >= 2 && on_the_way[..on_the_way.len() - 1].iter().any(|v| (v - newest).abs() > 0.06), "no shape before the newest one: {on_the_way:?} (newest {newest}, {moves} moves)");
        let old = param(&app, KEY);
        drag.release();
        settle_after_commit(&app, "Housing", KEY, old);
        assert!((param(&app, KEY) - newest).abs() < 0.06, "the commit is not the newest value {newest}");
        assert_nothing_left(&app, "a drag that showed its shapes");
    }) {
        panic!("{}", e.message());
    }
}

/// An exact part dragged back to a shape it showed shows it at once: the build of a new value
/// under way when the pointer comes back stops, and is never shown.
#[test]
fn an_exact_part_dragged_back_shows_the_shape_it_built_at_once() {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "an_exact_part_dragged_back_shows_the_shape_it_built_at_once").unwrap();
    config.env.insert("FAB_PARAM_LOG".into(), "1".into());
    if let Err(e) = run_with_config(config, |app: TestApp| {
        const LABEL: &str = "Housing size";
        const KEY: &str = "housing/size";
        let near = |a: f64, b: f64| (a - b).abs() < 0.06;
        let wait = |what: &str, done: &dyn Fn() -> bool| {
            wait_poll(&app, || {
                if !done() { Err(format!("{what}: {:?}", shown_previews(&app, KEY))) } else { Ok(()) }
            });
        };
        place_parts(&app, &[EXACT_PART]);
        pick_part(&app, "Housing");
        settle(&app, "Housing", quiet());
        let mut drag = app.locator(Selector::role(Role::SpinButton).name(LABEL)).wait_visible().press_and_drag();
        // The value counts from where the drag engages (3 px in); 20 px on is ten millimetres.
        drag.move_by(3.0, 0.0);
        drag.move_by(23.0, 0.0);
        let back = number(&shown(&app, LABEL));
        wait("the first value dragged to was never shown", &|| shown_previews(&app, KEY).last().is_some_and(|v| near(*v, back)));
        // On to new values: one of them builds, the others wait for it.
        let (shown_before, builds_before) = (shown_previews(&app, KEY).len(), built_previews(&app, KEY).len());
        drag.move_by(63.0, 0.0);
        wait("no new value started building", &|| built_previews(&app, KEY)[builds_before..].iter().any(|v| !near(*v, back)));
        // Back to the shape shown before, while the new one builds (it takes far longer than the
        // move).
        drag.move_by(23.0, 0.0);
        wait("the shape dragged back to was never shown", &|| shown_previews(&app, KEY).len() > shown_before);
        let since = shown_previews(&app, KEY)[shown_before..].to_vec();
        assert!(near(since[0], back), "the build under way showed before the shape dragged back to ({back}): {since:?}");
        assert!(stopped_previews(&app, KEY) >= 1, "no build was stopped for the shape dragged back to");
        let old = param(&app, KEY);
        drag.release();
        settle_after_commit(&app, "Housing", KEY, old);
        assert!(near(param(&app, KEY), back), "the commit is not the value dragged back to ({back})");
        assert!(shown_previews(&app, KEY)[shown_before..].iter().all(|v| near(*v, back)), "a stopped build showed after all: {:?}", &shown_previews(&app, KEY)[shown_before..]);
        assert_nothing_left(&app, "a drag back to a shape built before");
    }) {
        panic!("{}", e.message());
    }
}

/// A commit the model refuses goes back to what the model holds and says why.
#[makepad_test]
fn a_refused_drag_goes_back_and_says_why(app: TestApp) {
    place_parts(&app, &[POST_PART]);
    pick_part(&app, "Post");
    let control = Selector::role(Role::SpinButton).name("Post radius");
    let before = settle(&app, "Post", quiet());
    for hold in ["at once", "after more moves"] {
        let mut drag = app.locator(control.clone()).wait_visible().press_and_drag();
        drag.move_by(-400.0, 0.0);
        if hold != "at once" {
            // frames of the far value pass (each move waits for its frame), then the release
            for step in [-380.0, -400.0, -390.0, -400.0] {
                drag.move_by(step, 0.0);
            }
        }
        drag.release();
        let after = settle(&app, "Post", Duration::from_millis(600));
        assert_eq!(after, before, "a refused value changes nothing");
        assert_eq!(param(&app, "post/r"), 8.0);
        assert_eq!(number(&shown(&app, "Post radius")), 8.0, "the control goes back to what the model holds");
        assert_nothing_left(&app, "a refusal");
        let nodes = app.widget_snapshot().nodes();
        let said: Vec<&str> = nodes.iter().filter(|w| w.visible() && !w.node.name.is_empty()).map(|w| w.node.name.as_str()).filter(|n| n.contains("positive height and radius")).collect();
        assert!(said.iter().any(|n| !n.starts_with('{')), "nothing says in words why the value went back (hold {hold}): {said:?}");
    }
}

#[makepad_test]
fn a_handle_dragged_away_and_back_leaves_nothing(app: TestApp) {
    place_parts(&app, &[BRACKET]);
    pick_part(&app, "Shelf bracket");
    handle_away_and_back(&app, "Shelf bracket", "bracket/thickness");
}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);
