//! Design's Force render mode shows a part and its state at once, whatever the slicer is doing:
//! switched on during an Export's slice, with two orange parts on the plate and neither selected, the
//! Force panel joins the right column and names the plate's first part and what its study still
//! needs, the plate view goes on drawing the real parts (both still orange in a grab: there is one
//! model), and the study's surfaces go on to their finest cells by themselves, while the slice is
//! still running. A fresh
//! start's demo bracket solves the study its script carries, solves it again as sliced once the
//! plate has been sliced (Force slices nothing itself: Sliced does, on demand), and solves again
//! when a load in the script changes.
//!
//! Fab solves on this machine's AI hub: the demo bracket's test starts one on loopback in this
//! process with this machine's GPU device and registers it as the AI Hub app registers itself, in
//! a home of the app's own (`MAKEPAD_HOME`), so the app finds that hub alone: the code under test,
//! never a hub the box runs already.
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

mod common;
#[path = "common/picture.rs"]
mod picture;
#[path = "common/wait.rs"]
mod idle_wait;
use idle_wait::wait_poll;

use makepad_strict_json::Value;
use makepad_test::{makepad_test, run_with_config, KeyCode, Locator, Role, Selector, TestApp, TestConfig};
use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
use makepad_zune_png::PngDecoder;
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
    lines.iter().find(|l| l.contains("\"Result\"")).cloned().ok_or_else(|| format!("fab.{tool} returned no result: {lines:?}"))
}

/// The app's CPU budget and pool as it reports them (who waited longest for a worker, and on
/// whom), for a test that says why something came late.
fn cpu(app: &TestApp) -> String {
    let Ok(client) = makepad_remote::Client::connect(&app.remote_endpoint()) else { return "no CPU report: no connection".into() };
    let mut lines = Vec::new();
    let _ = makepad_remote::app::run(&client, &["cpu".to_string()], &mut |line| {
        lines.push(line.to_string());
        true
    });
    lines.join("\n")
}

/// A home of the app's own (its `MAKEPAD_HOME`): no hub registered in it, networked AI hub off.
/// Its `run/` is private, as the AI hub keeps it.
fn own_home(name: &str) -> std::path::PathBuf {
    let home = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
    let mut dir = std::fs::DirBuilder::new();
    dir.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut dir, 0o700);
    dir.create(home.join("run")).expect("a home of its own");
    home
}

/// The AI hub for the app under test: one on loopback in this process with this machine's GPU
/// device, registered in the app's own `home` alone, with this machine's token there (the app
/// presents it to the hub). Dropped, the home goes.
struct LocalHub {
    home: std::path::PathBuf,
}

impl LocalHub {
    fn start(home: std::path::PathBuf) -> LocalHub {
        use makepad_widgets::makepad_micro_serde::SerJson;
        let url = fab_force::hub::loopback("fab-force-mode").unwrap_or_else(|e| panic!("{e}"));
        makepad_ai_hub::program_serve::service().set_gpu(makepad_compute_program_gpu::spawn().unwrap_or_else(|e| panic!("a GPU device for the hub: {e}")));
        let port = url.rsplit(':').next().and_then(|p| p.parse().ok()).unwrap_or_else(|| panic!("the hub's port in {url}"));
        let run = home.join("run");
        let token = makepad_ai_hub::machine::machine_token().unwrap_or_else(|e| panic!("this machine's token: {e}"));
        let mut file = std::fs::OpenOptions::new();
        file.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut file, 0o600);
        std::io::Write::write_all(&mut file.open(run.join("machine-token")).expect("the app's token"), token.as_bytes()).expect("the app's token");
        let pid = std::process::id();
        let entry = makepad_ai_hub::machine::NodeEntry { pid: pid as u64, port, pipes_hash: 0 };
        std::fs::write(run.join(format!("fab-force-mode-{pid}.node")), entry.serialize_json()).expect("the hub's entry");
        LocalHub { home }
    }
}

impl Drop for LocalHub {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);

fn text(line: &str) -> String {
    let v = makepad_strict_json::parse_depth(line.as_bytes(), 32).expect("result line is JSON");
    let r = match v.get("Result").expect("Result") {
        Value::Arr(a) => a[0].clone(),
        other => other.clone(),
    };
    r.get("text").and_then(Value::as_str).unwrap_or_default().to_string()
}

fn head_of(reply: &str) -> String {
    let reply = makepad_script::syntax::parse_value(reply).expect("a Splash reply");
    reply.field("head").and_then(|f| f.value.string()).expect("reply has a head").to_string()
}

fn wait_until(app: &TestApp, what: &str, mut ok: impl FnMut() -> bool) {
    wait_poll(app, || if ok() { Ok(()) } else { Err(format!("waiting for {what}")) });
}

/// The widget once it shows.
fn shown(app: &TestApp, selector: Selector) -> Locator {
    wait_until(app, &format!("{selector:?} to show"), || app.locator(selector.clone()).try_wait_visible().is_ok());
    app.locator(selector)
}

/// The slice's state: a percentage while a slice runs.
fn slice_state(app: &TestApp) -> String {
    common::slice_state(app)
}

/// The visible lines of text, from the semantic tree.
fn texts(app: &TestApp) -> Vec<String> {
    app.widget_snapshot().nodes().into_iter().filter(|w| w.visible() && w.node.role == Role::StaticText).map(|w| w.node.name.to_string()).filter(|t| !t.is_empty()).collect()
}

/// The lines shown that ask to select a part, but for the Parameters editor's own note: that
/// editor shows a selected part's parameters, and Force studies a part without selecting it.
fn asks_to_select(app: &TestApp) -> Vec<String> {
    let nodes = app.widget_snapshot().nodes();
    let params = nodes.iter().find(|w| w.visible() && w.node.role == Role::TabPanel && w.node.name == "Parameters").map(|p| (p.x(), p.y(), p.x() + p.width(), p.y() + p.height()));
    let inside = |w: &makepad_test::NodeMatch| params.is_some_and(|(x0, y0, x1, y1)| (x0..x1).contains(&w.x()) && (y0..y1).contains(&w.y()));
    nodes.iter().filter(|w| w.visible() && w.node.role == Role::StaticText && w.node.name.starts_with("Select a part") && !inside(w)).map(|w| w.node.name.to_string()).collect()
}

/// The lines of text drawn, also those a scrolling panel clips away below its fold (a
/// result under a long panel: there to read, not to click).
fn drawn_texts(app: &TestApp) -> Vec<String> {
    app.widget_snapshot().nodes().into_iter().filter(|w| !w.states.contains(makepad_test::States::HIDDEN) && w.node.role == Role::StaticText).map(|w| w.node.name.to_string()).filter(|t| !t.is_empty()).collect()
}

/// Scroll the Force panel to its foot, where the controls under its results sit (the readouts
/// above them can be long enough to push them below the fold).
fn panel_to_foot(app: &TestApp) {
    let panel = shown(app, Selector::widget_type("FabForcePanel")).snapshot().bounds;
    let bar = app.widget_snapshot().nodes().into_iter().find(|n| {
        let (x, y) = n.bounds.center();
        n.role == Role::ScrollBar && n.visible() && panel.contains(x, y)
    });
    if let Some((bar, range)) = bar.and_then(|b| Some((b.id.short(), b.range?))) {
        app.locator(Selector::raw(format!("id={bar}"))).set_value(range.max);
    }
}

/// The shares of the plate view (between the left column's panel and the right column's `right`
/// tab, top to bottom of that tab) that are grey and the parts' orange in a grab of the view's
/// finished picture.
fn shares(app: &TestApp, right: &str) -> (f64, f64, std::path::PathBuf) {
    let path = picture::finished_screenshot(app);
    let bytes = std::fs::read(&path).expect("the grab");
    let mut dec = PngDecoder::new(ZCursor::new(&bytes));
    let px = dec.decode_raw().expect("png");
    let (w, h) = dec.dimensions().expect("dims");
    let n = dec.colorspace().expect("colorspace").num_components();
    let window = app.locator(Selector::role(Role::Window)).snapshot();
    let left = shown(app, Selector::role(Role::TabPanel).name("Parts")).snapshot();
    let panel = shown(app, Selector::role(Role::TabPanel).name(right)).snapshot();
    let (sx, sy) = (w as f64 / window.width() as f64, h as f64 / window.height() as f64);
    let x0 = ((((left.x() + left.width() - window.x()) as f64) * sx) as usize).min(w);
    let x1 = ((((panel.x() - window.x()) as f64) * sx) as usize).min(w);
    let y0 = ((((panel.y() - window.y()) as f64) * sy) as usize).min(h);
    let y1 = ((((panel.y() + panel.height() - window.y()) as f64) * sy) as usize).min(h);
    let (mut grey, mut orange) = (0, 0);
    for y in y0..y1 {
        for x in x0..x1 {
            let o = (y * w + x) * n;
            let (r, g, b) = (px[o] as i32, px[o + 1] as i32, px[o + 2] as i32);
            grey += (r > 80 && (r - g).abs() < 20 && (g - b).abs() < 20) as usize;
            orange += (r > 90 && r > g + 50 && g > b + 20) as usize;
        }
    }
    let all = ((x1.saturating_sub(x0) * (y1 - y0)).max(1)) as f64;
    (grey as f64 / all, orange as f64 / all, path)
}

#[makepad_test]
fn the_force_render_mode_shows_a_part_and_its_state_while_the_slicer_runs(app: TestApp) {
    force_while_the_slicer_runs(app);
}

/// The same on two cores, where the pool has one worker and the slice's long jobs hold it: the
/// view's grabs and the study's own work still come through.
#[test]
fn on_two_cores_the_force_render_mode_shows_a_part_and_its_state_while_the_slicer_runs() {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "on_two_cores_the_force_render_mode_shows_a_part_and_its_state_while_the_slicer_runs").expect("the test's config");
    config.env.insert("MAKEPAD_CPU_CORES".into(), "2".into());
    run_with_config(config, force_while_the_slicer_runs).unwrap();
}

fn force_while_the_slicer_runs(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    let line = wait_poll(&app, || {
        call(&app, "inspect", "{}").map_err(|e| format!("the fab tools never appeared: {e}"))
    });
    let head = head_of(&text(&line));
    let source = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet block = m.box({size: [60mm 60mm 60mm]})\nobject(block)\n";
    let batch = format!(r#"{{"request_id":"force-1","expected":{head:?},"script":"block.cad.splash","source":{}}}"#, Value::Str(source.to_string()).to_json());
    let head = head_of(&text(&call(&app, "script", &batch).expect("script")));
    // two orange parts, neither selected: their 3.3M moves keep the slicer busy for seconds on any
    // box (5 s to settle on twelve cores, 17 s on four), and the settled slice holds a few GB
    let edits = "settings({layer_height: 0.16})\ncolor(\"Orange\", rgb: #xe07020)\nplace(\"block\", name: \"First\", at: [90mm 160mm], color: \"Orange\")\nplace(\"block\", link: 1, name: \"Second\", at: [230mm 160mm], color: \"Orange\")";
    let batch = format!(r#"{{"request_id":"force-1-place","expected":{head:?},"edits":{}}}"#, Value::Str(edits.to_string()).to_json());
    call(&app, "place", &batch).expect("place");
    // Force itself slices nothing: Export, from the command palette, asks for the slice that runs
    app.press_key(KeyCode::Escape);
    app.press_key(KeyCode::F3);
    app.type_text("Export the Print");
    app.press_key(KeyCode::ReturnKey);
    wait_until(&app, "the slice to start", || slice_state(&app).ends_with('%'));
    let (_, orange, _) = shares(&app, "Part");

    shown(&app, Selector::role(Role::Radio).name("Force").nth(0)).select();
    let opened = Instant::now();
    // the study's state: the part read, waiting for its fixtures and loads
    let status = "Add a fixture and a load";
    let shot = wait_poll(&app, || {
        let lines = texts(&app);
        let read = lines.iter().any(|t| t.starts_with(status)) && lines.iter().any(|t| t.starts_with("First · ")) && detail(&app, "Cells").is_some();
        // the real parts, both still orange: no stand-in for the one studied
        let (now_grey, now_orange, shot) = shares(&app, "Force");
        if read && now_orange > 0.5 * orange {
            return Ok(shot);
        }
        Err(format!(
                "Force hung: CPU budget {} s exhausted or the app sat idle and it showed no part and status: {:.2} % of the view orange and {:.2} % grey against {:.2} % orange before (see {shot:?}), the slice at {}; texts {lines:?}; the CPU budget: {}",
                HANG.as_secs(), 100.0 * now_orange, 100.0 * now_grey, 100.0 * orange, slice_state(&app), cpu(&app)
        ))
    });
    let shown_ms = opened.elapsed().as_secs_f64() * 1e3;
    let slicing = slice_state(&app);
    assert!(slicing.ends_with('%'), "the slice ended before Force showed the part ({slicing}, {shown_ms:.0} ms after the mode came on): Force waited for it, or the slice is too short to check this; the CPU budget: {}", cpu(&app));
    let asks = asks_to_select(&app);
    assert!(asks.is_empty(), "the note to select a part is gone: Force studies the plate's first part ({asks:?})");
    println!("force mode: the study's state shown {shown_ms:.0} ms after the mode came on, the slice at {slicing} ({shot:?}); the CPU budget: {}", cpu(&app));
    // nothing holds or loads the part, and its surface still goes on to the study's finest cells:
    // finer than the study's first size over the part's 100 mm box, the size the part is read at.
    // (The finer surface follows the read at once, beside the slice: on a loaded box it showed
    // before the look after the grab, which then waited for cells finer than the finest, 11-10.)
    // The panel prints sizes to 0.01 mm.
    let cells = |app: &TestApp| detail(app, "Cells").and_then(|c| c.rsplit(" of ").next()?.strip_suffix(" mm")?.parse::<f64>().ok());
    let read_at = fab_force::study::steps([0.0; 3], [100.0; 3], 1, 0.0)[0];
    let shown = cells(&app).expect("the cells the part was read at");
    assert!(shown <= read_at + 0.005, "the part shows {shown:.2} mm cells, coarser than the study's first size ({read_at:.2} mm)");
    let began = Instant::now();
    wait_until(&app, "the study's surface to go on to finer cells", || cells(&app).is_some_and(|h| h < read_at - 0.005));
    println!("force mode: read at {read_at:.2} mm cells ({shown:.2} mm when the state showed), then {:.2} mm in {:.1} s, unposed; the slice at {}", cells(&app).unwrap_or(0.0), began.elapsed().as_secs_f64(), slice_state(&app));
}

/// The lines of the tooltips (the Force panel's busy line's: the print settings read and how
/// the study shown was solved; a tip describes the control it wraps).
fn tip_lines(app: &TestApp) -> Vec<String> {
    app.widget_snapshot().nodes().into_iter().filter(|w| w.node.role == Role::Tooltip).flat_map(|w| w.node.description.to_string().lines().map(str::to_string).collect::<Vec<_>>()).collect()
}

/// A line of how the Force panel's study was solved (`Solve`, `Cells`), from its busy line's tip.
fn detail(app: &TestApp, name: &str) -> Option<String> {
    let at = format!("{name}: ");
    tip_lines(app).iter().find_map(|l| l.strip_prefix(&at).map(str::to_string))
}

/// How the study shown was solved, once its job has ended (the busy line's tip shows then).
fn solve_detail(app: &TestApp) -> String {
    wait_until(app, "the busy line's tip to say how the study was solved", || detail(app, "Solve").is_some());
    detail(app, "Solve").unwrap_or_default()
}

/// The value printed beside a stat of the Force panel (`Largest von Mises`), if it shows.
fn stat(lines: &[String], name: &str) -> Option<String> {
    lines.iter().position(|t| t == name).and_then(|i| lines.get(i + 1)).cloned()
}

/// The demo's bracket script carries its study (the wall's screw holes held, the shelf pushed
/// down): placed alone (the slicer, which goes first, has only it to slice), Force mode solves
/// it with no click, then again with the walls and skins of its slice, an edit of a load in the script solves again on the part already sampled,
/// from its last answer, and the infill floor of the edited study is found on request.
/// The demo's bracket script: its study is the wall's screw holes held and the shelf pushed down.
const BRACKET: &str = include_str!("../resources/bracket.cad.splash");

/// The demo's bracket script, placed alone on the plate, and Force mode switched on: the plate's
/// head after the placing.
fn place_bracket(app: &TestApp, extra: &str) -> String {
    app.locator(Selector::role(Role::Window)).wait_visible();
    let line = wait_poll(app, || {
        call(app, "inspect", "{}").map_err(|e| format!("the fab tools never appeared: {e}"))
    });
    let head = head_of(&text(&line));
    let batch = format!(r#"{{"request_id":"force-bracket","expected":{head:?},"script":"bracket.cad.splash","source":{}}}"#, Value::Str(format!("{BRACKET}{extra}")).to_json());
    let head = head_of(&text(&call(app, "script", &batch).expect("script")));
    let batch = format!(r#"{{"request_id":"force-bracket-place","expected":{head:?},"edits":{}}}"#, Value::Str("place(\"bracket\", at: [150mm 165mm], name: \"Shelf bracket\")".to_string()).to_json());
    let head = head_of(&text(&call(app, "place", &batch).expect("place")));
    shown(app, Selector::role(Role::Radio).name("Force").nth(0)).select();
    head
}

/// With no AI hub answering (the app on a home of its own: no hub registered there, networked AI
/// hub off), Force shows the bracket and says it waits for the hub, with a button that starts it:
/// nothing is solved in the app, and nothing spins on.
#[test]
fn with_no_ai_hub_force_says_so_and_offers_to_start_it() {
    let home = own_home("fab-force-no-hub");
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "with_no_ai_hub_force_says_so_and_offers_to_start_it").expect("the test's config");
    config.env.insert("MAKEPAD_HOME".into(), home.display().to_string());
    run_with_config(config, |app| {
        place_bracket(&app, "");
        let opened = Instant::now();
        wait_until(&app, "Force to say no AI hub is running", || texts(&app).iter().any(|t| t == "No AI hub is running"));
        shown(&app, Selector::role(Role::Button).name("Start AI Hub"));
        let lines = texts(&app);
        println!("force study, no AI hub: {:.1} s after the mode came on: {:?}", opened.elapsed().as_secs_f64(), lines.iter().filter(|t| t.starts_with("Shelf bracket") || t.contains("AI hub")).collect::<Vec<_>>());
        assert!(lines.iter().any(|t| t.starts_with("Shelf bracket")), "the part shows: {lines:?}");
        assert!(stat(&lines, "Safety factor").is_none(), "nothing solved without a hub: {lines:?}");
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_demo_bracket_solves_its_saved_study_and_an_edit_solves_again() {
    let hub = LocalHub::start(own_home("fab-force-mode"));
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "the_demo_bracket_solves_its_saved_study_and_an_edit_solves_again").expect("the test's config");
    config.env.insert("MAKEPAD_HOME".into(), hub.home.display().to_string());
    run_with_config(config, the_demo_bracket).unwrap();
}

fn the_demo_bracket(app: TestApp) {
    let extra = "\nm.settings(m.box({size: [8mm 4mm 8mm] at: [0 3mm 8mm]}), {sparse_infill_density: \"100%\"})\n";
    let head = place_bracket(&app, extra);
    let opened = Instant::now();
    // a study solves only with its fixtures and loads: a safety factor means the script's were read
    let solved = |lines: &[String]| lines.iter().any(|t| t.starts_with("Shelf bracket · ")) && stat(lines, "Safety factor").is_some();
    let mut lines = texts(&app);
    wait_poll(&app, || {
        lines = texts(&app);
        if !solved(&lines) { Err(format!("Force did not solve the bracket's saved study: CPU budget {} s exhausted or the app sat idle (the slice at {}); texts {lines:?}", HANG.as_secs(), slice_state(&app))) } else { Ok(()) }
    });
    let first = opened.elapsed();
    let provisional = |lines: &[String]| stat(lines, "Safety factor").filter(|s| s.contains("(provisional: "));
    let checking = |lines: &[String]| lines.iter().find(|t| t.starts_with("Checking at ")).cloned();
    let (sf, status) = wait_poll(&app, || {
        lines = texts(&app);
        provisional(&lines).zip(checking(&lines)).ok_or_else(|| format!("the provisional safety factor and its check did not show; texts {lines:?}"))
    });
    println!("force study: 50 N provisional at {:.1} s: {sf}; {status}", opened.elapsed().as_secs_f64());
    let vm50 = stat(&lines, "Largest von Mises").expect("the provisional step's stress shows");
    let source = format!("{BRACKET}{extra}").replace("[0, -50, 0])", "[0, -60, 0])");
    assert!(source.contains("[0, -60, 0])"), "the bracket's preset pushes 50 N");
    let batch = format!(r#"{{"request_id":"force-check-edit","expected":{head:?},"script":"bracket.cad.splash","source":{}}}"#, Value::Str(source.clone()).to_json());
    lines = texts(&app);
    assert!(checking(&lines).is_some(), "the 50 N study is still checking just before the edit: {lines:?}");
    let edited = Instant::now();
    let head = head_of(&text(&call(&app, "script", &batch).expect("script")));
    let (sf, vm60) = wait_poll(&app, || {
        lines = texts(&app);
        assert!(!lines.iter().any(|t| t.starts_with("Done in")), "the superseded study finished after the edit: {lines:?}");
        let stress = stat(&lines, "Largest von Mises").filter(|s| s != &vm50);
        provisional(&lines).zip(stress).ok_or_else(|| format!("the edited load did not show a new provisional reading; texts {lines:?}"))
    });
    println!("force study: 50 -> 60 N provisional in {:.1} s from the edit: {sf}; largest von Mises {vm50} -> {vm60}", edited.elapsed().as_secs_f64());
    // the safety factor where the last two cell sizes agree on it, said apart where they do not,
    // and what the walls and skins were read from
    let readout = |lines: &[String]| {
        let from = lines.iter().find_map(|t| ["As sliced", "Estimated from settings"].iter().find_map(|w| t.find(w)).map(|i| t[i..].to_string())).unwrap_or_default();
        format!("SF {}; near the fixtures {}; movement {} ({from})", stat(lines, "Safety factor").unwrap_or_default(), stat(lines, "Near the fixtures").unwrap_or_default(), stat(lines, "Largest movement").unwrap_or_default())
    };
    wait_until(&app, "the print settings to say they are estimated", || tip_lines(&app).iter().any(|t| t.starts_with("Estimated from settings")));
    let estimated = tip_lines(&app).into_iter().find(|t| t.starts_with("Estimated from settings")).expect("the estimated settings show");
    assert!(estimated.contains("(Fab 0.20 mm standard)"), "the panel names the profile: {estimated}");
    assert!(estimated.ends_with(" · 1 zone 100%"), "the estimated settings name the zone: {estimated}");
    // its solves reach their tolerance: no Solve row (an answer stopped short of it is marked approximate there)
    lines = texts(&app);
    assert!(stat(&lines, "Solve").is_none(), "the bracket's solves reach their tolerance: {lines:?}");
    println!("force study: {estimated}");
    // Force slices nothing itself: Sliced slices the plate on demand, and back in Force the study
    // reads the bracket's walls and skins from that slice and solves again from the estimate's answer
    shown(&app, Selector::id("rm_slice")).select();
    wait_until(&app, "the plate to be sliced", || slice_state(&app) == "Sliced");
    shown(&app, Selector::role(Role::Radio).name("Force").nth(0)).select();
    wait_poll(&app, || {
        lines = texts(&app);
        if !(lines.iter().any(|t| t.starts_with("Done in")) && lines.iter().any(|t| t.contains("As sliced (slice of "))) { Err(format!("the study did not read the bracket's slice: CPU budget {} s exhausted or the app sat idle (the slice at {}); texts {lines:?}", HANG.as_secs(), slice_state(&app))) } else { Ok(()) }
    });
    // the busy line's tip, once its job has ended
    wait_until(&app, "the print settings to say they are the slice's", || tip_lines(&app).iter().any(|t| t.starts_with("As sliced (slice of ")));
    let sliced_settings = tip_lines(&app).into_iter().find(|t| t.starts_with("As sliced (slice of ")).expect("the sliced settings show");
    assert!(sliced_settings.ends_with(" · 1 zone 100%"), "the sliced settings name the zone: {sliced_settings}");
    println!("force study: {sliced_settings}");
    let sliced = opened.elapsed();
    let vm = stat(&lines, "Largest von Mises").expect("the stress shows");
    let cold = solve_detail(&app);
    assert!(cold.contains("AI hub (GPU)"), "the study solves on an AI hub's GPU: {cold}");
    let at60 = readout(&lines);
    // 60 N down to 80 N down, written in the script: the study solves again
    assert!(source.contains("[0, -60, 0])"), "the edited bracket pushes 60 N");
    let source = source.replace("[0, -60, 0])", "[0, -80, 0])");
    let batch = format!(r#"{{"request_id":"force-edit","expected":{head:?},"script":"bracket.cad.splash","source":{}}}"#, Value::Str(source.clone()).to_json());
    let edited = Instant::now();
    let head = head_of(&text(&call(&app, "script", &batch).expect("script")));
    // the first heatmap of the new load (its coarsest step), then the study refined to its finest
    let mut first_shown = None;
    let vm80 = wait_poll(&app, || {
        let lines = texts(&app);
        if first_shown.is_none() && stat(&lines, "Largest von Mises").is_some_and(|v| v != vm) {
            first_shown = Some(edited.elapsed());
        }
        if let Some(shown) = first_shown.filter(|_| lines.iter().any(|t| t.starts_with("Done in"))) {
            let warm = solve_detail(&app);
            println!(
                "force study: the bracket's saved study answered {:.1} s after the mode came on (the slice at {}; finest solve: {cold}); 60 -> 80 N in its script: its first heatmap in {:.0} ms, refined in {:.0} ms ({vm} -> {}; finest solve: {warm})",
                first.as_secs_f64(), slice_state(&app), shown.as_secs_f64() * 1e3, edited.elapsed().as_secs_f64() * 1e3, stat(&lines, "Largest von Mises").unwrap_or_default()
            );
            println!("force study: 60 N as sliced at {:.1} s: {at60}", sliced.as_secs_f64());
            println!("force study: 80 N: {}", readout(&lines));
            assert!(stat(&lines, "Safety factor").is_some_and(|v| v.starts_with("not converged (") || v.contains("(converged: ")), "two cell sizes read the safety factor: {lines:?}");
            // converged, the value said carries the margin: never over the finest cells' reading
            let sf = stat(&lines, "Safety factor").unwrap_or_default();
            if let Some((said, rest)) = sf.split_once(" (converged: ") {
                let read = rest.split(" at ").next().and_then(|r| r.parse::<f64>().ok());
                assert!(read.is_some_and(|r| said.parse::<f64>().is_ok_and(|s| s <= r)), "the safety factor said is at or under the finest cells' reading: {sf}");
            }
            return Ok(stat(&lines, "Largest von Mises").unwrap_or_default());
        }
        Err(format!("the edited load did not solve again: CPU budget {} s exhausted or the app sat idle; texts {lines:?}", HANG.as_secs()))
    });
    // the load turned, 80 N down and 40 N along: a new direction, so the solves iterate (the
    // last answer helps little); the AI hub's route is measured against this
    let source = source.replace("[0, -80, 0])", "[40, -80, 0])");
    let batch = format!(r#"{{"request_id":"force-turn","expected":{head:?},"script":"bracket.cad.splash","source":{}}}"#, Value::Str(source).to_json());
    let turned = Instant::now();
    call(&app, "script", &batch).expect("script");
    let mut first_shown = None;
    wait_poll(&app, || {
        let lines = texts(&app);
        if first_shown.is_none() && stat(&lines, "Largest von Mises").is_some_and(|v| v != vm80) {
            first_shown = Some(turned.elapsed());
        }
        if let Some(shown) = first_shown.filter(|_| lines.iter().any(|t| t.starts_with("Done in"))) {
            println!(
                "force study: 80 N down -> 80 N down + 40 N along in its script: its first heatmap in {:.0} ms, refined in {:.0} ms ({vm80} -> {}; finest solve: {})",
                shown.as_secs_f64() * 1e3, turned.elapsed().as_secs_f64() * 1e3, stat(&lines, "Largest von Mises").unwrap_or_default(), solve_detail(&app)
            );
            return Ok(());
        }
        Err(format!("the turned load did not solve again: CPU budget {} s exhausted or the app sat idle; texts {lines:?}", HANG.as_secs()))
    });
    // the infill floor of the study as it stands: where the safety factor falls to 1 and to 2
    panel_to_foot(&app);
    shown(&app, Selector::role(Role::Button).name("Find the infill floor")).press();
    let asked = Instant::now();
    let (status, lines) = wait_poll(&app, || {
        let lines = drawn_texts(&app);
        if let Some(status) = lines.iter().find(|t| t.starts_with("Infill floor (")).cloned() {
            return Ok((status, lines));
        }
        Err(format!("the infill floor was not found: CPU budget {} s exhausted or the app sat idle; texts {lines:?}", HANG.as_secs()))
    });
    let at = |sf: &str| status.split(": ").nth(1).and_then(|s| s.split(". ").next()?.split(", ").find_map(|p| p.strip_prefix(&format!("SF {sf} at "))?.strip_suffix(" %")?.parse::<f64>().ok()));
    println!("force study: {status} in {:.1} s; wall loops {}; tried {}; {}", asked.elapsed().as_secs_f64(), stat(&lines, "Wall loops").unwrap_or_default(), stat(&lines, "Tried").unwrap_or_default(), stat(&lines, "Took").unwrap_or_default());
    assert!(status.starts_with("Infill floor (") && status.contains(" mm cells"), "the floor says its resolution: {status}");
    if let (Some(breaks), Some(holds)) = (at("1"), at("2")) {
        assert!(breaks <= holds, "SF 2 takes at least the infill SF 1 does: {status}");
    }
}
