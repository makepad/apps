//! While an orbit in the main view rests mid-drag, neither the main view nor the underside view
//! (a linked pane: it follows the main view's camera) draws, and neither view's navigation asks
//! for frames once the main one finds the pointer resting; after the release both finish their
//! still averages.
//!
//! Why: every draw is work on the UI thread. While the camera moved, the linked pane counted
//! each pause between two moves as still and recorded jittered still samples there, which the
//! next move threw away (some 150 draws through a 24-move orbit); the dragged view itself asked
//! a frame every refresh while the pointer rested, drawing nothing new, only to slow the
//! velocity a release would coast with (it now slows it by the rest at the next move or release).
//!
//! The tone box demo in Design with View › Underside view open (a Plate view of its own beside
//! Plate, its chip saying so): an orbit of a few moves, a rest
//! of half a second with the button still down (some 30 refreshes), more moves, the release. The
//! rest is counted from when the main view has taken the orbit's last move (its description's
//! `pointer`), however long a busy box takes to get there: before that, the moves' own draws are
//! still landing (a cold JIT's lazy compiles held a bucket run's moves for over 250 ms).
//!
//! The orbit starts once both views are settled, which they say only once the tone box is on the
//! bed with its board drawn: no view says it is settled while a scene or a board drawing is on its
//! way (checked on every read from the open on). The app opens on an empty bed, which the demo
//! replaces: on a loaded box both views were still on the empty bed first, and the demo's arrival
//! (its scene, the board's preparation and art) drew in the rest (9 and 8 draws). Each phase reads both views from one read of the UI, as a read after input
//! draws every window while the bench plays: the rest's draws include one, its closing read's.
//!
//! The orbit also waits for the board's run to have settled (`sim_steady`: no publication follows
//! until a command) and its lights to hold for a rest's length. A run that starts late publishes
//! its first lights into the rest (a bucket run's board compiled its run's plan cold for 1.8 s,
//! and both views drew twice more in the rest), and a quiet half second alone is no rest: a cold
//! box's run crawled while its plan compiled (4.4 s), publishing seldom, then published its way
//! to rest in the orbit's (6 draws asked of each view by publications). A view that draws too
//! often in the rest is named with where its draws came from: the test's reads, publications
//! (the board's live values, a board drawing), or elsewhere.
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

use makepad_strict_json::Value;
use makepad_test::{run_with_config, Role, Selector, TestApp, TestConfig};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);
/// The orbit's moves before and after the rest, each a few points right of the last.
const MOVES: usize = 4;
const STEP: f64 = 6.0;
/// The rest with the button down: a sampling or polling view draws through it.
const REST: Duration = Duration::from_millis(500);
/// Draws a view may take in the rest: a move's last draw landing late.
const REST_DRAWS: u64 = 2;
/// Frames a view's navigation may ask for in the rest: the main view's after the orbit's last
/// move, which finds the pointer resting.
const REST_NAV_FRAMES: u64 = 1;
/// The two Plate views: Plate (its body's tab `tab_viewport`) and the Underside view View ›
/// Underside view opens beside it (a Plate view of its own).
const VIEWS: [&str; 2] = ["Plate", "the Underside view"];

/// The reads of the UI the test has asked for, answered or not: each one the app takes draws both
/// views.
static READS: AtomicU64 = AtomicU64::new(0);

/// The view `i` of [`VIEWS`] among a read's nodes: the one shown `viewport` in Plate's body, or
/// in another one.
fn view_node(nodes: &[makepad_test::NodeMatch], i: usize) -> Result<&makepad_test::NodeMatch, String> {
    let mut found = nodes.iter().filter(|n| n.visible() && n.path.rsplit('.').next() == Some("viewport") && n.path.contains("tab_viewport.") == (i == 0));
    let node = found.next().ok_or_else(|| format!("no {} shown in a read of the UI", VIEWS[i]))?;
    if found.next().is_some() {
        return Err(format!("two of {} shown in a read of the UI", VIEWS[i]));
    }
    Ok(node)
}

fn wait_until(app: &TestApp, what: &str, mut ok: impl FnMut() -> bool) {
    app.try_wait_until_within(HANG, || (!ok()).then(|| format!("waiting for {what}"))).unwrap_or_else(|why| {
        let [a, b] = reports(app);
        panic!("{why}; the views report {} and {}", a.to_json(), b.to_json());
    });
}

/// What each view reports of itself (`FabViewport::semantics`), both from one read of the UI: a
/// read after input draws every window while the bench plays, so each read is a draw of its own
/// for both views. A read the busy app does not answer is read again, never taken for an empty
/// report; one lost for the whole wait fails, naming it.
fn reports(app: &TestApp) -> [Value; 2] {
    let read = || -> Result<[Value; 2], String> {
        READS.fetch_add(1, Ordering::Relaxed);
        let nodes = app.try_widget_snapshot().map_err(|err| format!("a read of the UI the app did not answer ({err})"))?.nodes();
        let report = |i: usize| -> Result<Value, String> {
            let node = view_node(&nodes, i)?;
            makepad_strict_json::parse_depth(node.description.as_bytes(), 8).map_err(|err| format!("{} reports no JSON ({err}): {}", VIEWS[i], node.description))
        };
        Ok([report(0)?, report(1)?])
    };
    read().unwrap_or_else(|_| {
        let mut answered = None;
        app.try_wait_until_within(HANG, || match read() {
            Ok(reports) => {
                answered = Some(reports);
                None
            }
            Err(why) => Some(why),
        }).unwrap_or_else(|why| panic!("{why}"));
        answered.expect("the answered read")
    })
}

/// Each view's draws so far, the frames its navigation asked for after one it took, and the
/// draws publications asked of it.
fn counts(reports: &[Value; 2]) -> [(u64, u64, u64); 2] {
    [0, 1].map(|i| {
        let r = &reports[i];
        let records = r.get("records");
        let draws = records.and_then(|r| r.get("draws")).and_then(Value::as_u64);
        let asked = r.get("nav_frames").and_then(Value::as_u64);
        let published = records.and_then(|r| r.get("published")).and_then(Value::as_u64);
        match (draws, asked, published) {
            (Some(draws), Some(asked), Some(published)) => (draws, asked, published),
            _ => panic!("{} reports its draws, its navigation's frames and its publications' draws: {}", VIEWS[i], r.to_json()),
        }
    })
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::F64(f) => Some(*f),
        Value::Int(i) => Some(*i as f64),
        _ => None,
    }
}

/// The board's run has stepped (`sim_t`, the run's simulated time, past 0) and settled
/// (`sim_steady`): what it shows now holds until a command.
fn run_settled(report: &Value) -> bool {
    report.get("sim_t").and_then(num).is_some_and(|t| t > 0.0) && report.get("sim_steady").and_then(Value::as_bool) == Some(true)
}

/// Where the last pointer move over the view was (window points), once one came.
fn pointer(report: &Value) -> Option<(f64, f64)> {
    match report.get("pointer")?.as_arr()? {
        [x, y] => Some((num(x)?, num(y)?)),
        _ => None,
    }
}

/// The tone box on the bed, its board drawn in both views: the app opens on an empty bed, which
/// the demo replaces.
fn tone_box_shown(reports: &[Value; 2]) -> bool {
    reports.iter().all(|r| r.get("boards_drawn").and_then(Value::as_bool) == Some(true)
        && r.get("parts_drawn").and_then(Value::as_u64).is_some_and(|parts| parts > 0))
}

/// Both views settled with their whole still average.
fn both_still(reports: &[Value; 2]) -> bool {
    reports.iter().all(|r| {
        r.get("settled").and_then(Value::as_bool) == Some(true)
            && r.get("still_samples").and_then(Value::as_u64) == Some(u64::from(makepad_render::STILL_AA_SAMPLES))
    })
}

/// View › `path`.
fn view_menu(app: &TestApp, path: &[&str]) {
    app.locator(Selector::role(Role::Button).name("View")).wait_visible().press();
    for item in path {
        app.locator(Selector::role(Role::MenuItem).name(*item)).wait_visible().press();
    }
}

#[test]
fn neither_view_draws_while_an_orbit_rests_and_both_finish_their_still_average_after() {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "linked_pane_orbit").unwrap();
    config.env.insert("MAKEPAD_HAL_BACKEND".into(), "offscreen".into());
    config.app_args = vec!["--window".into(), "1600x1000@1".into(), "--demo".into(), "tonebox".into()];
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("fab-linked-pane-orbit-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&dir).expect("a fresh folder for the test's preferences");
    let prefs = dir.join("view.prefs");
    std::fs::write(&prefs, "code=0\n").expect("the test's view preferences");
    config.env.insert("FAB_VIEW_PREFS".into(), prefs.display().to_string());
    let result = run_with_config(config, |app: TestApp| {
        app.locator(Selector::role(Role::Window)).wait_visible();
        view_menu(&app, &["Workspace", "3D modeling"]);
        view_menu(&app, &["Underside view"]);
        // its chip says what the view is
        app.locator(Selector::role(Role::StaticText).name("Underside of Plate")).wait_visible();
        // From the open on, a view that says it is settled has its board drawn: until then the
        // demo, its electronics' scenes and the board's art are on their way.
        let mut seen = None;
        wait_until(&app, "both views still with their averages whole", || {
            let r = reports(&app);
            for (view, r) in VIEWS.iter().zip(&r) {
                assert!(r.get("settled").and_then(Value::as_bool) != Some(true) || r.get("boards_drawn").and_then(Value::as_bool) == Some(true),
                    "{view} says it is settled before the tone box's board is drawn: {}", r.to_json());
            }
            let still = both_still(&r);
            seen = Some(r);
            still
        });
        let seen = seen.expect("a read of both views");
        assert!(tone_box_shown(&seen), "both views still with the tone box shown: {} and {}", seen[0].to_json(), seen[1].to_json());
        // The board's run settled and its lights holding: no publication asks either view to draw
        // for a rest's length.
        wait_until(&app, "the board's run settled, its lights holding for a rest's length", || {
            let r = reports(&app);
            if !run_settled(&r[0]) {
                return false;
            }
            let from = counts(&r);
            std::thread::sleep(REST);
            let to = counts(&reports(&app));
            (0..2).all(|i| to[i].2 == from[i].2)
        });
        wait_until(&app, "both views still before the orbit", || both_still(&reports(&app)));

        let nodes = app.try_widget_snapshot().expect("a read of the UI").nodes();
        let node = view_node(&nodes, 0).unwrap_or_else(|why| panic!("{why}"));
        let (x, y) = (node.x() as f64 + node.width() as f64 * 0.5, node.y() as f64 + node.height() as f64 * 0.5);
        let mut drag = app.press_at(x, y);
        for i in 1..=MOVES {
            drag.move_by(STEP * i as f64, 0.0);
        }
        let last = (x + STEP * MOVES as f64, y);
        // The rest's draws from two reads, one at each end: each read draws both views once.
        let mut rest_from = [(0, 0, 0); 2];
        wait_until(&app, "the main view taking the orbit's last move", || {
            let r = reports(&app);
            rest_from = counts(&r);
            pointer(&r[0]).is_some_and(|(px, py)| (px - last.0).abs() < 0.5 && (py - last.1).abs() < 0.5)
        });
        std::thread::sleep(REST);
        let reads_from = READS.load(Ordering::Relaxed);
        let rest_to = counts(&reports(&app));
        // the closing read, and the reads again of one the busy app did not answer
        let reads = READS.load(Ordering::Relaxed) - reads_from;
        let rested: Vec<(&str, u64, u64, u64)> = (0..2).map(|i| (VIEWS[i], rest_to[i].0 - rest_from[i].0, rest_to[i].1 - rest_from[i].1, rest_to[i].2 - rest_from[i].2)).collect();
        for i in MOVES + 1..=2 * MOVES {
            drag.move_by(STEP * i as f64, 0.0);
        }
        drag.release();
        wait_until(&app, "both views still after the orbit, their averages whole", || both_still(&reports(&app)));

        let busy: Vec<String> = rested.iter().flat_map(|(view, draws, asked, published)| {
            let drew = (*draws > REST_DRAWS).then(|| {
                let other = draws.saturating_sub(reads + published);
                format!("{view} drew {draws} times while the orbit rested (at most {REST_DRAWS}): {reads} for the test's reads of the UI, {published} asked by publications (the board's live values, a board drawing), {other} from elsewhere")
            });
            let ask = (*asked > REST_NAV_FRAMES).then(|| format!("{view}'s navigation asked for {asked} frames while the orbit rested (at most {REST_NAV_FRAMES})"));
            drew.into_iter().chain(ask)
        }).collect();
        assert!(busy.is_empty(), "{}", busy.join("; "));
    });
    let cleanup = std::fs::remove_dir_all(&dir);
    result.unwrap();
    cleanup.expect("remove the test's temporary folder");
}
