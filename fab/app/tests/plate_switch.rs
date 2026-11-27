//! Plates switched away from and back come back as they were left: the camera, the selected
//! part, the undo history and the slice. A plate opened for the first time in a run comes back as
//! an earlier run left it: the camera and the selection kept in the plate's own view file. Code typed into one plate and not committed yet when
//! another is opened lands in its own plate, never in the next one. At a start, the plate list
//! shows before the plate the last run left on the bed is built, and a plate opened from it
//! meanwhile goes on the bed in its place.

#[path = "common/part_editor.rs"]
mod part_editor;
#[path = "common/render_mode.rs"]
mod render_mode;
#[path = "common/wait.rs"]
mod idle_wait;
use idle_wait::wait_poll;
use fab::api::{Camera, PlateView};
use fab_model::program::{part_id, ScriptEnv};
use fab_model::project::{self, Placement, Project, ScriptRef, Unit};
use makepad_script::syntax::{self, Node};
use makepad_strict_json::Value;
use makepad_test::{run_with_config, KeyCode, KeyModifiers, Role, Selector, States, TestApp, TestConfig};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const BOX: &str = "let block = m.box({size: [10mm 10mm 10mm]})\nobject(block)\n";

/// A plate of the script's objects, the first named "<name> part", the others "<name> <object>".
fn seed_plate(folder: &Path, name: &str, code: &str, objects: &[&str]) {
    std::fs::create_dir_all(folder).unwrap();
    let script = format!("{name}.cad.splash");
    let places = objects
        .iter()
        .enumerate()
        .map(|(i, object)| {
            let mut part = Placement::new(name, object);
            part.name = Some(if i == 0 { format!("{name} part") } else { format!("{name} {object}") });
            part
        })
        .collect();
    let project = Project { scripts: vec![ScriptRef { name: name.into(), file: script.clone(), params: vec![] }], places, ..Default::default() };
    std::fs::write(folder.join(script), format!("{}{code}", fab_model::program::EMPTY)).unwrap();
    std::fs::write(folder.join(format!("{name}.fab.splash")), project::write(&project, &|_, _| Unit::Plain)).unwrap();
}

fn seed_project(folder: &Path, name: &str) {
    seed_plate(folder, name, BOX, &["block"]);
}

fn call(app: &TestApp, tool: &str, args: &str) -> String {
    call_at(&app.remote_endpoint(), tool, args)
}

/// A tool call at the app's remote endpoint (from any thread).
fn call_at(endpoint: &str, tool: &str, args: &str) -> String {
    let client = makepad_remote::Client::connect(endpoint).expect("connect");
    let argv = vec!["call".to_string(), format!("fab.{tool}"), args.to_string()];
    let mut lines = Vec::new();
    makepad_remote::app::run(&client, &argv, &mut |line| {
        lines.push(line.to_string());
        true
    })
    .expect("fab call");
    let line = lines.iter().find(|l| l.contains("\"Result\"")).cloned().unwrap_or_else(|| panic!("fab.{tool} returned no result: {lines:?}"));
    let v = makepad_strict_json::parse_depth(line.as_bytes(), 32).expect("result line is JSON");
    let r = match v.get("Result").expect("Result") {
        Value::Arr(a) => a[0].clone(),
        other => other.clone(),
    };
    r.get("text").and_then(Value::as_str).unwrap_or_default().to_string()
}

/// The source of the plate's one script.
fn source(app: &TestApp, name: &str) -> String {
    let reply: Node = syntax::parse_value(&call(app, "inspect", "{}")).expect("inspect is Splash");
    let file = format!("{name}.cad.splash");
    let scripts = reply.field("scripts").and_then(|f| f.value.items()).expect("the scripts");
    let script = scripts.iter().find(|s| s.field("file").and_then(|f| f.value.string()) == Some(file.as_str())).unwrap_or_else(|| panic!("no {file} on the bed"));
    script.field("source").and_then(|f| f.value.string()).expect("the script source").to_string()
}

fn wait_for(app: &TestApp, what: &str, mut done: impl FnMut() -> bool) {
    wait_poll(app, || {
        if !done() { Err(format!("{what}")) } else { Ok(()) }
    });
}

/// Where a camera stands and looks, as the viewport's inspection writes it.
fn placement(camera: &str) -> (String, String) {
    let value = makepad_strict_json::parse_depth(camera.as_bytes(), 4).expect("camera JSON");
    let field = |name: &str| value.get(name).unwrap_or_else(|| panic!("the camera's {name}")).to_json();
    (field("eye"), field("target"))
}

fn placement_of(c: &Camera) -> (String, String) {
    placement(&format!(r#"{{"eye":{:?},"target":{:?}}}"#, [c.eye.x, c.eye.y, c.eye.z], [c.target.x, c.target.y, c.target.z]))
}

fn camera(app: &TestApp) -> String {
    let viewport = app.locator(Selector::id("viewport")).snapshot();
    let value = makepad_strict_json::parse_depth(viewport.description.as_bytes(), 8).expect("viewport inspection JSON");
    value.get("camera").expect("viewport camera").to_json()
}

/// The camera once it has not changed for longer than a glide.
fn settled_camera(app: &TestApp) -> String {
    let mut previous = camera(app);
    let mut since = Instant::now();
    wait_for(app, "the camera to settle", || {
        let current = camera(app);
        if current != previous {
            previous = current;
            since = Instant::now();
        }
        since.elapsed() >= Duration::from_millis(400)
    });
    previous
}

fn tab(app: &TestApp, name: &str) {
    app.locator(Selector::role(Role::Tab).name(name)).wait_visible().click();
}

/// Put a plate on the bed from the Plates panel, and wait for its part in the Parts tab.
fn open(app: &TestApp, name: &str) {
    tab(app, "Plates");
    let rows = || app.widget_snapshot().nodes().into_iter().filter(|w| w.visible() && w.node.role == Role::ListItem).map(|w| w.node.name.clone()).collect::<Vec<_>>();
    // a plate with edits its file lacks is listed with a *
    let edited = format!("{name} *");
    let row = wait_poll(app, || {
        if let Some(row) = rows().into_iter().find(|r| r == name || *r == edited) {
            return Ok(row);
        }
        Err(format!("no plate {name} listed: {:?}", rows()))
    });
    app.locator(Selector::role(Role::ListItem).name(row)).click();
    tab(app, "Parts");
    let items = || app.widget_snapshot().nodes().into_iter().filter(|w| w.visible() && w.node.role == Role::TreeItem).map(|w| w.node.name.clone()).collect::<Vec<_>>();
    let part = format!("{name} part");
    wait_poll(app, || {
        if !items().contains(&part) { Err(format!("{name} is not on the bed: the parts are {:?}", items())) } else { Ok(()) }
    });
}

fn selected(app: &TestApp, item: &str) -> bool {
    app.widget_snapshot().nodes().into_iter().any(|w| w.visible() && w.node.role == Role::TreeItem && w.node.name == item && w.node.states.contains(States::SELECTED))
}

#[test]
fn a_plate_switched_away_from_and_back_is_as_it_was_left() {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    // Outside artifacts_dir: TestApp clears that directory before launching.
    let home = std::env::temp_dir().join(format!("fab-plate-switch-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&home).unwrap();
    let documents = home.join("Documents/Fab");
    seed_project(&documents.join("alpha"), "alpha");
    seed_project(&documents.join("beta"), "beta");
    // alpha as an earlier run left it: its view in its own folder, written as Fab writes it
    let fab = home.join(".makepad/fab");
    let store = fab_ai::plates::PlateStore::under(&fab, &documents);
    store.migrate(&fab, &|_| None, &|file| project::load(file, &ScriptEnv::default()).ok().map(|m| m.head().content)).expect("the plates are listed");
    let alpha = store.list().into_iter().find(|p| p.name == "alpha").expect("alpha is listed").id;
    let alpha_view = store.view_file(&alpha);
    let mut earlier = Camera::default();
    (earlier.eye.x, earlier.eye.y, earlier.eye.z) = (0.6, -1.0, 0.8);
    let block = part_id("alpha", "block");
    let saved = PlateView { camera: earlier, preset: None, plates: Vec::new(), device: None, sheet: None, board: None, scopes: Vec::new(), selected: vec![block.clone()], active: Some(block.clone()), viewed: None };
    std::fs::write(&alpha_view, saved.to_file_text()).unwrap();

    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "plate_switch::switch").unwrap();
    config.env.insert("MAKEPAD_TEST_HOME".into(), home.display().to_string());
    config.env.insert("FAB_PLATES".into(), "1".into());
    let result = run_with_config(config, |app| {
        app.locator(Selector::role(Role::Window)).wait_visible();
        open(&app, "alpha");

        // alpha opens as the earlier run left it: that camera, its part selected
        let framed = settled_camera(&app);
        assert_eq!(placement(&framed), placement_of(&earlier), "alpha's camera from its view file");
        wait_for(&app, "alpha's part is selected from its view file", || selected(&app, "alpha part"));

        // alpha as it is left: the view dollied in
        app.locator(Selector::id("viewport")).wheel(0.0, -240.0);
        let left = settled_camera(&app);
        assert_ne!(left, framed, "the wheel moves the camera");

        // typed into alpha (the code is an area of its own), and beta opened before the typing rests
        let code = source(&app, "alpha");
        let typed = code.replace("[10mm 10mm 10mm]", "[12mm 10mm 10mm]");
        assert_ne!(typed, code, "the box's size is in the code: {code}");
        app.locator(Selector::id("code")).wait_visible().click();
        app.press_key_with_modifiers(KeyCode::KeyA, KeyModifiers { logo: cfg!(target_os = "macos"), control: !cfg!(target_os = "macos"), ..Default::default() });
        app.type_text(&typed);
        open(&app, "beta");
        // the switch keeps alpha's view in its folder: the camera and the part it was left with
        let written = || std::fs::read_to_string(&alpha_view).ok().and_then(|text| PlateView::from_file_text(&text)).filter(|v| v.camera != earlier);
        wait_for(&app, "alpha's view is written on the switch", || written().is_some());
        let view = written().unwrap();
        assert_eq!(placement_of(&view.camera), placement(&left), "alpha's view file has its camera as left");
        assert_eq!(view.selected, vec![block.clone()], "alpha's view file has its part selected");
        assert!(!source(&app, "beta").contains("12mm"), "what was typed into alpha is not beta's");
        assert!(!selected(&app, "beta part"), "alpha's selection is not beta's");

        // alpha back: as it was left, with what was typed, and its undo history
        open(&app, "alpha");
        wait_for(&app, "the typing is alpha's", || source(&app, "alpha").contains("[12mm 10mm 10mm]"));
        assert_eq!(settled_camera(&app), left, "alpha's camera as it was left");
        assert!(selected(&app, "alpha part"), "alpha's part is selected again");
        app.locator(Selector::id("undo").nth(0)).press();
        wait_for(&app, "undo takes back the typing on alpha", || source(&app, "alpha").contains("[10mm 10mm 10mm]"));

        // what the switch cost the main thread is logged once it has settled (on an event after)
        wait_for(&app, "the switch's cost is logged", || {
            app.locator(Selector::id("viewport")).wheel(0.0, 0.0);
            app.log_since(0).expect("the app's log").lines.iter().any(|l| l.contains("fab switch: shown "))
        });
    });
    std::fs::remove_dir_all(&home).expect("remove seeded test home after the owned app exits");
    result.unwrap();
}

/// Where the app's log is now.
fn log_mark(app: &TestApp) -> u64 {
    app.log_since(0).expect("the app's log").last_seq
}

/// What each of a plate's `parts` cost in the first whole slice logged after `from`: one line per
/// part (`traced N planned M`).
fn slice_cost_after(app: &TestApp, from: u64, parts: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut seq = from;
    wait_poll(app, || {
        let tail = app.log_since(seq).expect("the app's log");
        seq = seq.max(tail.last_seq);
        lines.extend(tail.lines.into_iter().filter(|l| l.contains("fab live work: part ")));
        if lines.len() >= parts {
            return Ok(lines[..parts].to_vec());
        }
        Err(format!("a whole slice after the switch: {lines:?}"))
    })
}

#[test]
fn a_plate_switched_back_to_in_the_slice_view_is_not_sliced_again() {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let home = std::env::temp_dir().join(format!("fab-plate-reslice-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&home).unwrap();
    // Two parts each, none alike: every part's slice is its own.
    let pair = |a: u32, b: u32| format!("let block = m.box({{size: [{a}mm {a}mm {a}mm]}})\nlet peg = m.box({{size: [6mm 6mm {b}mm]}})\nobject(block)\nobject(peg)\n");
    seed_plate(&home.join("Documents/Fab/alpha"), "alpha", &pair(10, 14), &["block", "peg"]);
    seed_plate(&home.join("Documents/Fab/beta"), "beta", &pair(12, 9), &["block", "peg"]);

    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "plate_switch::reslice").unwrap();
    config.env.insert("MAKEPAD_TEST_HOME".into(), home.display().to_string());
    config.env.insert("FAB_PLATES".into(), "1".into());
    let result = run_with_config(config, |app| {
        app.locator(Selector::role(Role::Window)).wait_visible();
        let from = log_mark(&app);
        open(&app, "alpha");
        render_mode::pick(&app, "Sliced");
        let first = slice_cost_after(&app, from, 2);
        assert!(first.iter().all(|l| !l.contains("traced 0 ")), "alpha is sliced in the Slice view: {first:?}");

        let from = log_mark(&app);
        open(&app, "beta");
        let other = slice_cost_after(&app, from, 2);
        assert!(other.iter().all(|l| !l.contains("traced 0 ")), "beta is sliced: {other:?}");

        // alpha back, unchanged: its slice is still good
        let from = log_mark(&app);
        open(&app, "alpha");
        let back = slice_cost_after(&app, from, 2);
        assert!(back.iter().all(|l| l.contains("traced 0 planned 0 ")), "alpha back is not sliced again: {back:?}");
    });
    std::fs::remove_dir_all(&home).expect("remove seeded test home after the owned app exits");
    result.unwrap();
}

const SIZED: &str = "let s = param(10mm, min: 5mm, max: 40mm, step: 0.5mm, label: \"Size\")\nlet block = m.box({size: [s s s]})\nobject(block)\n";

/// A plate's id in the AI's plate list.
fn plate_id(app: &TestApp, name: &str) -> String {
    let reply: Node = syntax::parse_value(&call(app, "plates", "{}")).expect("fab.plates is Splash");
    let plates = reply.field("plates").and_then(|f| f.value.items()).expect("the plates");
    let plate = plates.iter().find(|p| p.field("name").and_then(|f| f.value.string()) == Some(name)).unwrap_or_else(|| panic!("no plate {name} listed"));
    plate.field("id").and_then(|f| f.value.string()).expect("a plate id").to_string()
}

/// A declared parameter's value in millimetres (fab.params).
fn param_mm(app: &TestApp, key: &str) -> f64 {
    let reply: Node = syntax::parse_value(&call(app, "params", "{}")).expect("fab.params is Splash");
    for part in reply.field("parts").and_then(|f| f.value.items()).expect("parts") {
        for p in part.field("params").and_then(|f| f.value.items()).expect("params") {
            if p.field("key").and_then(|f| f.value.string()) == Some(key) {
                return p.field("value").and_then(|f| f.value.number()).expect("a length") * 1000.0;
            }
        }
    }
    panic!("no parameter {key}");
}

#[test]
fn an_ai_switch_waits_for_a_value_drag_and_the_drag_lands_on_its_own_plate() {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let home = std::env::temp_dir().join(format!("fab-plate-held-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&home).unwrap();
    seed_plate(&home.join("Documents/Fab/alpha"), "alpha", SIZED, &["block"]);
    seed_project(&home.join("Documents/Fab/beta"), "beta");

    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "plate_switch::held").unwrap();
    config.env.insert("MAKEPAD_TEST_HOME".into(), home.display().to_string());
    config.env.insert("FAB_PLATES".into(), "1".into());
    let result = run_with_config(config, |app| {
        app.locator(Selector::role(Role::Window)).wait_visible();
        open(&app, "alpha");
        app.locator(Selector::role(Role::TreeItem).name("alpha part")).click();
        part_editor::open(&app);
        assert_eq!(param_mm(&app, "alpha/s"), 10.0, "alpha starts at 10 mm");
        let beta = plate_id(&app, "beta");

        // the AI asks for beta while the size is being dragged: the switch waits for the release
        let mut drag = app.locator(Selector::role(Role::SpinButton).name("Size")).wait_visible().press_and_drag();
        drag.move_by(3.0, 0.0);
        drag.move_by(40.0, 0.0);
        let asked = {
            let endpoint = app.remote_endpoint();
            std::thread::spawn(move || call_at(&endpoint, "plates", &format!(r#"{{"action":"open","id":{beta:?}}}"#)))
        };
        std::thread::sleep(Duration::from_millis(1000));
        assert!(!asked.is_finished(), "the AI's switch waits while the value is dragged");
        assert!(source(&app, "alpha").contains("param(10mm"), "alpha is still on the bed during the drag");
        drag.release();
        let reply = asked.join().expect("the AI's switch answers");
        assert!(reply.contains("beta"), "the AI's switch puts beta on the bed once the drag is let go: {reply}");

        // the drag's value is alpha's
        open(&app, "alpha");
        app.locator(Selector::role(Role::TreeItem).name("alpha part")).click();
        wait_for(&app, "alpha keeps the value it was dragged to", || param_mm(&app, "alpha/s") > 10.4);
    });
    std::fs::remove_dir_all(&home).expect("remove seeded test home after the owned app exits");
    result.unwrap();
}

/// A plate slow to build from scratch: a slab with a grid of `side` x `side` holes drilled one
/// after the other (each cut is on the one before: nothing to share out).
fn seed_slow_plate(folder: &Path, name: &str, side: usize) {
    // (all of it at positive coordinates: `[a -b]` reads as one number, a minus b)
    let mut code = String::from("let s0 = m.box({size: [100mm 100mm 8mm] at: [50mm 50mm 0mm]})\n");
    let pitch = 90.0 / side as f64;
    for i in 0..side * side {
        let (x, y) = (5.0 + (i % side) as f64 * pitch + pitch / 2.0, 5.0 + (i / side) as f64 * pitch + pitch / 2.0);
        code.push_str(&format!("let s{} = m.subtract(s{i}, m.cylinder({{radius: 1mm height: 20mm segments: 96 at: [{x:.3}mm {y:.3}mm 0mm]}}))\n", i + 1));
    }
    let slab = format!("s{}", side * side);
    code.push_str(&format!("object({slab})\n"));
    seed_plate(folder, name, &code, &[slab.as_str()]);
}

/// The plates in the list (the Plates tab's rows, demos aside).
fn listed(app: &TestApp, names: &[&str]) -> Vec<String> {
    app.widget_snapshot().nodes().into_iter().filter(|w| w.visible() && w.node.role == Role::ListItem).map(|w| w.node.name.clone()).filter(|n| names.contains(&n.trim_end_matches(" *"))).collect()
}

fn parts(app: &TestApp) -> Vec<String> {
    app.widget_snapshot().nodes().into_iter().filter(|w| w.visible() && w.node.role == Role::TreeItem).map(|w| w.node.name.clone()).collect()
}

#[test]
fn the_plate_list_shows_before_the_plate_left_on_the_bed_is_built_and_a_plate_opened_meanwhile_lands() {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let home = std::env::temp_dir().join(format!("fab-plate-start-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&home).unwrap();
    let documents = home.join("Documents/Fab");
    seed_slow_plate(&documents.join("slow"), "slow", 24);
    seed_project(&documents.join("alpha"), "alpha");
    seed_project(&documents.join("beta"), "beta");
    // the last run left the slow plate on the bed: its plates already listed (as a start folds
    // them in), it named on the bed
    let fab = home.join(".makepad/fab");
    let store = fab_ai::plates::PlateStore::under(&fab, &documents);
    store.migrate(&fab, &|_| None, &|file| project::load(file, &ScriptEnv::default()).ok().map(|m| m.head().content)).expect("the plates are listed");
    let slow = store.list().into_iter().find(|p| p.name == "slow").expect("the slow plate is listed").id;
    store.set_current(Some(&fab_ai::plates::Current::Plate(slow))).unwrap();

    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "plate_switch::start").unwrap();
    config.env.insert("MAKEPAD_TEST_HOME".into(), home.display().to_string());
    config.env.insert("FAB_PLATES".into(), "1".into());
    let result = run_with_config(config, |app| {
        app.locator(Selector::role(Role::Window)).wait_visible();
        // all three listed while the slow plate is still being built: nothing on the bed yet
        tab(&app, "Plates");
        wait_for(&app, "the three plates are listed", || listed(&app, &["slow", "alpha", "beta"]).len() == 3);
        tab(&app, "Parts");
        assert!(!parts(&app).iter().any(|p| p == "slow part"), "the slow plate is still being built when the list shows: {:?}", parts(&app));

        // a plate opened from the list meanwhile lands, and the slow build gives way to it
        // (beta's job runs once the start's is over, so its log is in)
        open(&app, "beta");
        let said: Vec<String> = app.log_since(0).expect("the app's log").lines.into_iter().filter(|l| l.contains("fab: ")).collect();
        assert!(said.iter().any(|l| l.contains("the plate left on the bed gave way")), "the slow plate's build gave way to beta: {said:?}");
    });
    std::fs::remove_dir_all(&home).expect("remove seeded test home after the owned app exits");
    result.unwrap();
}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);
