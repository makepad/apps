//! The Plates panel, the first tab on the left: the plates (the lists of an earlier version
//! folded in) and then the demos, each plate's picture beside it. A click puts a plate on the
//! bed, New plate starts one, the search narrows the list, and a plate's menu renames it (its
//! folder and file with it), takes it off the list (its files stay) and, after a confirm in its
//! row, moves its files to the Trash. The home is seeded the way an earlier version left it (a
//! recent list, an image-to-mesh result) with plain box-script plates, one with a picture.

#[path = "common/render_mode.rs"]
mod render_mode;
#[path = "common/wait.rs"]
mod idle_wait;
use idle_wait::wait_poll;
use fab_model::project::{self, Placement, Project, ScriptRef, Unit};
use makepad_test::{run_with_config, Role, Selector, States, TestApp, TestConfig};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn seed_project(folder: &Path, name: &str) -> PathBuf {
    std::fs::create_dir_all(folder).unwrap();
    let script = format!("{name}.cad.splash");
    let mut part = Placement::new(name, "block");
    part.name = Some(format!("{name} part"));
    let project = Project {
        scripts: vec![ScriptRef { name: name.into(), file: script.clone(), params: vec![] }],
        places: vec![part],
        ..Default::default()
    };
    let source = format!("{}let block = m.box({{size: [10mm 10mm 10mm]}})\nobject(block)\n", fab_model::program::EMPTY);
    std::fs::write(folder.join(script), source).unwrap();
    let path = folder.join(format!("{name}.fab.splash"));
    std::fs::write(&path, project::write(&project, &|_, _| Unit::Plain)).unwrap();
    path
}

fn plate(name: &str) -> Selector {
    Selector::role(Role::ListItem).name(name)
}

/// `fab.plates` lists the plates (`(id, name, on the bed)`), the demos' keys and the demo on the bed.
fn plates_listed(app: &TestApp) -> (Vec<(String, String, bool)>, Vec<String>, Option<String>) {
    use makepad_strict_json::Value;
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).expect("connect");
    let args = ["call".to_string(), "fab.plates".into(), r#"{"action":"list"}"#.into()];
    let mut reply = None;
    makepad_remote::app::run(&client, &args, &mut |line| {
        if line.contains("\"Result\"") {
            reply = Some(line.to_string());
        }
        true
    }).expect("fab.plates");
    let json = makepad_strict_json::parse_depth(reply.expect("fab.plates answers").as_bytes(), 32).expect("result line is JSON");
    let result = match json.get("Result").expect("Result") {
        Value::Arr(a) => a[0].clone(),
        other => other.clone(),
    };
    let text = result.get("text").and_then(Value::as_str).expect("the list");
    let list = makepad_script::syntax::parse_value(text).expect("a Splash reply");
    let field = |n: &makepad_script::syntax::Node, k: &str| n.field(k).and_then(|f| f.value.string()).unwrap_or_else(|| panic!("no {k} in {text}")).to_string();
    let plates = list.field("plates").and_then(|f| f.value.items()).expect("plates").iter().map(|p| (field(p, "id"), field(p, "name"), p.field("on_bed").and_then(|f| f.value.ident()) == Some("true"))).collect();
    let demos = list.field("demos").and_then(|f| f.value.items()).expect("demos").iter().map(|d| field(d, "key")).collect();
    let on_bed = list.field("demo_on_bed").and_then(|f| f.value.string()).map(str::to_string);
    (plates, demos, on_bed)
}

/// The app's log since `at`, every line of it (the app keeps its newest lines only), with the
/// keys of the pictures painted in it added to `painted`.
fn log_painted(app: &TestApp, at: &mut u64, painted: &mut Vec<String>) -> Vec<String> {
    let log = app.log_since(*at).expect("the app's log");
    assert_eq!(log.lines.len() as u64, log.last_seq - *at, "the app's log since line {at} is whole");
    *at = log.last_seq;
    painted.extend(log.lines.iter().filter_map(|l| l.split_once("fab: painted the picture of ")).filter_map(|(_, rest)| rest.rsplit_once(" in ")).map(|(key, _)| key.to_string()));
    log.lines
}

fn wait_for(app: &TestApp, what: &str, mut done: impl FnMut() -> bool) {
    app.wait_until(|| (!done()).then(|| what.to_string()));
}

/// The left column's tab bar owns the Plates tab, at either column width.
fn left_bar(nodes: &[makepad_test::NodeMatch]) -> &makepad_test::NodeMatch {
    let plates = nodes.iter().find(|w| w.visible() && w.role == Role::Tab && w.name == "Plates").expect("the Plates tab");
    nodes.iter().find(|w| w.visible() && w.role == Role::TabList && w.children.contains(&plates.id)).expect("the left column's tab bar")
}

/// Every visible tab of the left stack fits, including the Slice editor brought forward here.
fn left_tabs_whole(app: &TestApp, when: &str) {
    let nodes = app.widget_snapshot().nodes();
    let owner = left_bar(&nodes);
    let bar = owner.bounds;
    let tabs: Vec<_> = nodes.iter().filter(|w| w.visible() && w.role == Role::Tab && owner.children.contains(&w.id)).collect();
    for name in ["Plates", "Parts", "Slice"] {
        assert!(tabs.iter().any(|w| w.name == name), "no {name} tab in the left bar {when}");
    }
    for w in tabs {
        let (name, tab) = (&w.name, w.bounds);
        assert!(tab.y == bar.y && tab.x >= bar.x - 0.5 && tab.x + tab.w <= bar.x + bar.w + 0.5, "the {name} tab shows whole {when}: {tab:?} in the bar {bar:?}");
    }
}

fn listed(app: &TestApp, name: &str) -> bool {
    app.widget_snapshot().nodes().into_iter().any(|w| w.visible() && w.node.role == Role::ListItem && w.node.name == name)
}

#[test]
fn the_plates_panel_lists_opens_makes_finds_renames_removes_and_trashes_plates() {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    // Outside artifacts_dir: TestApp clears that directory before launching.
    let home = std::env::temp_dir().join(format!("fab-plates-panel-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&home).unwrap();
    let fab = home.join(".makepad/fab");
    let lattice = seed_project(&home.join("Documents/Fab/lattice"), "lattice");
    let frame: Vec<u8> = (0..64 * 32).flat_map(|i| [(i % 64 * 4) as u8, 90, 160, 255]).collect();
    std::fs::write(project::thumbnail_path(&lattice), fab_ai::plates::thumbnail_png(64, 32, &frame).unwrap()).unwrap();
    let figure = seed_project(&fab.join("made/1793560900-figure"), "figure");
    let saved = seed_project(&home.join("elsewhere/saved"), "saved");
    // a plate file this Fab cannot read (an earlier format's, here plain garbage)
    let old = home.join("elsewhere/old/old.fab.splash");
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::write(&old, "this is no plate (\n").unwrap();
    std::fs::write(fab.join("recent"), format!("{}\n{}\n{}\n", saved.display(), lattice.display(), old.display())).unwrap();

    // Explicit configuration is the seeded-home equivalent of #[makepad_test]. The harness owns
    // a --remote instance; FAB_PLATES gives it the plate store of the seeded home.
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "plates_panel::plates").unwrap();
    config.env.insert("MAKEPAD_TEST_HOME".into(), home.display().to_string());
    config.env.insert("FAB_PLATES".into(), "1".into());
    // two light demos: the guard below measures the panel, not the heaviest bundled plate's build
    config.env.insert("FAB_DEMOS".into(), "demo,noise_vase".into());
    let result = run_with_config(config, |app| {
        app.locator(Selector::role(Role::Window)).wait_visible();
        // Parts is forward at the start; Plates is the tab before it.
        app.locator(Selector::role(Role::Tab).name("Plates")).click();
        let y = |s: Selector| app.locator(s).wait_visible().snapshot().node.bounds.y;
        let plates = y(Selector::id("title").name("Plates"));
        let demos = y(Selector::id("title").name("Demos"));
        for name in ["saved", "lattice", "figure"] {
            let at = y(plate(name));
            assert!(plates < at && at < demos, "{name} is a plate");
        }
        for name in ["Demo plate", "Noise vase"] {
            assert!(demos < y(plate(name)), "{name} is a demo");
        }
        assert!(app.locator(plate("figure")).wait_visible().snapshot().node.description.contains("made from a picture"));
        let images: Vec<String> = app.widget_snapshot().nodes().into_iter().filter(|w| w.visible() && w.node.role == Role::Image).map(|w| w.node.path.clone()).collect();
        assert!(images.iter().any(|p| p.contains("pic.thumb")), "the plate saved with a picture shows it: {images:?}");
        // The rest are painted in the background, the demos too, several at once where the CPU
        // budget lets more than one background job run: every row shows one, those scrolled to as
        // well.
        // where the first plate's row sits with the list at its top
        let top_at = app.locator(plate("lattice")).wait_visible().snapshot().node.bounds.y;
        let started = Instant::now();
        // Each picture's end is in the app's log, painted or not. A row waits for its picture
        // while the worker paints; only 60 s without another picture or a change in the bare
        // rows ends the wait as hung, even when the app's main thread is idle.
        let (mut log_at, mut painted) = (0, Vec::new());
        // The plates' and demos' rows (the list's other lines, its headings, have no name); a
        // file this Fab cannot read has nothing to paint.
        let card = |w: &makepad_test::NodeMatch| w.node.role == Role::ListItem && !w.node.name.is_empty() && !w.node.description.contains("cannot read");
        let cards = app.widget_snapshot().nodes().iter().filter(|w| card(w)).count();
        let mut pictured = std::collections::BTreeSet::new();
        while pictured.len() < cards {
            // the rows the list shows whole, once each shows its picture, and the list
            let mut last_progress = Instant::now();
            let mut last_bare = Vec::new();
            let (shown, within, nodes) = loop {
                let painted_before = painted.len();
                let lines = log_painted(&app, &mut log_at, &mut painted);
                if let Some(failed) = lines.iter().find(|l| l.contains("fab: no picture of ")) {
                    panic!("every row shows a picture: {failed}");
                }
                let nodes = app.widget_snapshot().nodes();
                let thumbs: Vec<makepad_test::Rect> = nodes.iter().filter(|w| w.visible() && w.node.role == Role::Image && w.node.path.contains("pic.thumb")).map(|w| w.node.bounds).collect();
                // The rows' list (a thumbnail's), and what it shows: a row is whole inside it.
                let rows_list = nodes.iter().find(|w| w.visible() && w.node.path.contains(".list.") && w.node.path.contains("pic.thumb")).map(|w| w.node.path[..w.node.path.find(".list.").unwrap() + 5].to_string()).expect("a thumbnail in the list");
                let within = nodes.iter().find(|w| w.node.path == rows_list).map(|w| w.node.bounds).unwrap_or_else(|| {
                    let near: Vec<String> = nodes.iter().filter(|w| rows_list.starts_with(&w.node.path) || w.node.path.starts_with(&rows_list)).take(40).map(|w| format!("{:?} {:?} {} {:?}", w.node.role, w.node.name, w.node.path, w.node.bounds)).collect();
                    panic!("no node for the list {rows_list}: {near:#?}")
                });
                // A row's bounds are what of it shows, clipped by the list: one cut by its edge
                // is shorter than a whole one (the rows are all as tall).
                let on_screen: Vec<(String, makepad_test::Rect)> = nodes
                    .iter()
                    .filter(|w| card(w) && !w.node.states.contains(States::OFFSCREEN))
                    .map(|w| (w.node.name.clone(), w.node.bounds))
                    .filter(|(_, r)| r.y >= within.y - 0.5 && r.y + r.h <= within.y + within.h + 0.5)
                    .collect();
                let row_h = on_screen.iter().map(|(_, r)| r.h).fold(0.0, f32::max);
                let shown: Vec<(String, makepad_test::Rect)> = on_screen.into_iter().filter(|(_, r)| r.h >= row_h - 0.5).collect();
                let bare: Vec<String> = shown.iter().filter(|(_, r)| !thumbs.iter().any(|t| {
                    let (cx, cy) = (t.x + t.w * 0.5, t.y + t.h * 0.5);
                    r.x <= cx && cx <= r.x + r.w && r.y <= cy && cy <= r.y + r.h
                })).map(|(name, _)| name.clone()).collect();
                if !bare.is_empty() {
                    if painted.len() != painted_before || bare != last_bare {
                        last_progress = Instant::now();
                    }
                    let stalled = last_progress.elapsed();
                    if stalled >= Duration::from_secs(60) {
                        panic!("hang guard (60 s, not a timing check): every row shows a picture: {bare:?} have none ({} painted, the last progress {:.0} s ago)", painted.len(), stalled.as_secs_f64());
                    }
                    last_bare = bare;
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
                break (shown.into_iter().map(|(name, _)| name).collect::<Vec<String>>(), within, nodes);
            };
            let bottom = shown.last().cloned();
            pictured.extend(shown);
            if pictured.len() < cards {
                // a wheel's positive y scrolls toward the list's end; the rows move up by as much
                // as there is below (a short way when little is). The bottom row moves up inside
                // the list; the top one can stay clipped at its edge.
                let last = bottom.expect("a row shown");
                let row_y = |nodes: &[makepad_test::NodeMatch]| nodes.iter().find(|w| w.node.role == Role::ListItem && w.node.name == last).map(|w| w.node.bounds.y);
                let before = row_y(&nodes);
                app.locator(Selector::role(Role::ListItem).name(last.clone())).wheel(0.0, 240.0);
                app.wait_until(|| {
                    let nodes = app.widget_snapshot().nodes();
                    if row_y(&nodes) != before {
                        return None;
                    }
                    let rows: Vec<(String, f32, f32, bool)> = nodes.iter().filter(|w| card(w)).map(|w| (w.node.name.clone(), w.node.bounds.y, w.node.bounds.h, w.node.states.contains(States::OFFSCREEN))).collect();
                    Some(format!("the list scrolls to the rows below: {last:?} stays at {before:?}; {} of {cards} pictured ({pictured:?}); the list {within:?}; the rows (name, y, h, offscreen) {rows:?}", pictured.len()))
                });
            }
        }
        println!("every row shows a picture after {:.1} s", started.elapsed().as_secs_f64());
        // Each picture is painted once: every demo's but the one on the bed (the view shows
        // that), and every plate's but the one on the bed, lattice's (saved with its own) and the
        // file this Fab cannot read.
        log_painted(&app, &mut log_at, &mut painted);
        let (plates, demos, demo_on_bed) = plates_listed(&app);
        let mut wanted: Vec<String> = demos.into_iter().filter(|k| demo_on_bed.as_ref() != Some(k)).collect();
        wanted.extend(plates.into_iter().filter(|(_, name, on_bed)| !on_bed && name != "lattice" && name != "old").map(|(id, _, _)| id));
        for key in &wanted {
            assert_eq!(painted.iter().filter(|p| *p == key).count(), 1, "{key}'s picture is painted once: painted {painted:?}");
        }
        assert!(painted.iter().all(|p| wanted.contains(p)), "only the rows without a picture are painted: painted {painted:?}, wanted {wanted:?}");
        // back up, a wheel at a time, until the first row stands where it started: each wheel
        // moves the list
        let lattice_at = |nodes: &[makepad_test::NodeMatch]| nodes.iter().find(|w| w.node.role == Role::ListItem && w.node.name == "lattice").map(|w| (w.node.bounds.y, w.node.states.contains(States::OFFSCREEN)));
        let at_top = |lattice: Option<(f32, bool)>| lattice.is_some_and(|(y, off)| !off && (y - top_at).abs() < 0.5);
        loop {
            let nodes = app.widget_snapshot().nodes();
            if at_top(lattice_at(&nodes)) {
                break;
            }
            // a row on screen, the first at or below the list's top (where lattice started)
            let rows: Vec<(String, f32, bool)> = nodes.iter().filter(|w| card(w)).map(|w| (w.node.name.clone(), w.node.bounds.y, w.node.states.contains(States::OFFSCREEN))).collect();
            let (shown_row, before) = rows.iter().find(|r| !r.2 && r.1 >= top_at - 0.5).map(|r| (r.0.clone(), r.1)).expect("a row in the list");
            app.locator(Selector::role(Role::ListItem).name(shown_row.clone())).wheel(0.0, -1200.0);
            app.wait_until(|| {
                let nodes = app.widget_snapshot().nodes();
                let lattice = lattice_at(&nodes);
                let row = nodes.iter().find(|w| w.node.role == Role::ListItem && w.node.name == shown_row).map(|w| w.node.bounds.y);
                if at_top(lattice) || row != Some(before) {
                    return None;
                }
                Some(format!("the list scrolls back to the top: {shown_row:?} stays at {before}; lattice (y, offscreen) {lattice:?}, at {top_at} to start; the rows (name, y, offscreen) {rows:?}"))
            });
        }

        // The file this Fab cannot read says so in its row, and in full, with what it met, in
        // the tooltip of the row's line.
        assert!(app.locator(plate("old")).wait_visible().snapshot().node.description.contains("this Fab cannot read its file"));
        // where the row's line is once the list has come to rest
        let line_at = || app.widget_snapshot().nodes().into_iter().find(|w| w.visible() && w.node.role == Role::StaticText && w.node.name.ends_with("this Fab cannot read its file")).map(|w| w.node.bounds);
        let mut line = line_at().expect("the row's line");
        let mut since = Instant::now();
        wait_poll(&app, || {
            let now = line_at().expect("the row's line");
            if now != line {
                line = now;
                since = Instant::now();
            }
            if since.elapsed() >= Duration::from_millis(300) { Ok(()) } else { Err("waiting for the row to stand still".to_string()) }
        });
        let (x, y) = ((line.x + 20.0) as f64, (line.y + line.h * 0.5) as f64);
        app.move_to(x - 4.0, y);
        app.move_to(x, y);
        app.wait_until(|| {
            let nodes = app.widget_snapshot().nodes();
            if nodes.iter().any(|w| w.node.role == Role::Tooltip && w.node.name.contains("this Fab cannot read its file\n") && w.node.name.contains("old.fab.splash:1:18: this `(` is never closed")) {
                return None;
            }
            let tips: Vec<String> = nodes.iter().filter(|w| (w.node.role == Role::Tooltip && !w.node.name.is_empty()) || w.node.name.contains("cannot") || w.node.description.contains("cannot")).map(|w| format!("{} {:?} {:?} {:?} {:?} {}", w.visible(), w.node.role, w.node.name, w.node.description, w.node.bounds, w.node.path)).collect();
            Some(format!("the reason shows in full on hover at {x},{y}: {tips:#?}"))
        });

        // A click puts lattice on the bed.
        app.locator(plate("lattice")).click();
        app.locator(Selector::role(Role::Tab).name("Parts")).click();
        app.locator(Selector::role(Role::TreeItem).name("lattice part")).wait_visible();
        app.locator(Selector::role(Role::Tab).name("Plates")).click();

        // New plate: an empty one on the list, on the bed.
        app.locator(Selector::role(Role::Button).name("New plate")).click();
        app.locator(plate("Untitled")).wait_visible();
        app.locator(Selector::id("doc_name").name("Untitled")).wait_visible();

        // The search keeps the plates and demos whose names hold what is typed.
        app.locator(Selector::role(Role::Button).name("Search plates")).wait_visible().press();
        app.locator(Selector::role(Role::TextField).name("Search plates")).wait_visible();
        app.type_text("lat");
        wait_for(&app, "the search narrows the list", || listed(&app, "lattice") && !listed(&app, "saved") && !listed(&app, "Noise vase"));
        for _ in 0..3 {
            app.press_key(makepad_test::KeyCode::Backspace);
        }
        wait_for(&app, "an empty search lists everything", || listed(&app, "saved") && listed(&app, "Noise vase"));

        // Rename lattice: its folder of its own in the plates home and its file go with it.
        app.locator(plate("lattice")).show_menu();
        app.locator(Selector::role(Role::MenuItem).name("Rename")).wait_visible().click();
        app.type_text("lattice two");
        app.press_return();
        app.locator(plate("lattice two")).wait_visible();
        let renamed = home.join("Documents/Fab/lattice two/lattice-two.fab.splash");
        wait_for(&app, "the folder and file take the new name", || renamed.is_file() && !lattice.exists());

        // Remove saved from the list: its files stay where they are.
        app.locator(plate("saved")).show_menu();
        app.locator(Selector::role(Role::MenuItem).name("Remove from List")).wait_visible().click();
        wait_for(&app, "saved is off the list", || !listed(&app, "saved"));
        assert!(saved.is_file(), "a plate taken off the list keeps its file");

        // Move figure to the Trash, after the confirm in its row.
        app.locator(plate("figure")).show_menu();
        app.locator(Selector::role(Role::MenuItem).name("Move to Trash…")).wait_visible().click();
        assert!(figure.is_file(), "nothing moves before the confirm");
        app.locator(Selector::role(Role::Button).name("Move to Trash")).wait_visible().click();
        wait_for(&app, "figure is off the list and its file gone", || !listed(&app, "figure") && !figure.exists());
        let trash = if cfg!(target_os = "macos") { home.join(".Trash") } else { home.join(".local/share/Trash/files") };
        assert!(trash.join("figure.fab.splash").is_file(), "its file is in the Trash");

        // The left tabs, the Slice view's among them, show whole at the column's width and with
        // the column dragged as narrow as it goes.
        render_mode::pick(&app, "Sliced");
        app.locator(Selector::role(Role::Tab).name("Slice")).wait_visible();
        left_tabs_whole(&app, "at the column's width");
        let bar = left_bar(&app.widget_snapshot().nodes()).bounds;
        let mut drag = app.press_at((bar.x + bar.w) as f64 + 1.5, bar.y as f64 + 300.0);
        drag.move_by(-400.0, 0.0);
        drag.release();
        wait_for(&app, "the column narrows", || left_bar(&app.widget_snapshot().nodes()).bounds.w < bar.w - 10.0);
        left_tabs_whole(&app, "with the column as narrow as it goes");
    });
    std::fs::remove_dir_all(&home).expect("remove seeded test home after the owned app exits");
    result.unwrap();
}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);
