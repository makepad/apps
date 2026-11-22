//! The view bar beside the render switch, as a person works it: in Normal and Force it holds the
//! orthographic toggle alone, in Sliced also what the slice shows (its supports, its travel, the
//! parts under it), and each of its toggles is the View menu's too. With the supports hidden the
//! picture has none of their beads and keeps the part's own toolpaths; shown again (by their key,
//! `U`) they are back as they were, and the slice did not run again. The bar's projection and the
//! View menu's are one.
//!
//! Why (user): "put ortho view / view specific options (like show supports) next to those buttons
//! at the top sliced/force etc".
//!
//! One plate: a cap on a stem, its underside overhanging all round, with tree supports standing
//! under it, framed from the front in the Feature colours (the supports green, the part's outer
//! walls orange). Each step's picture is kept (`DEVHUB_KEEP`). HAL offscreen, native GPU.

use makepad_strict_json::Value;
use makepad_test::{run_with_config, KeyCode, Role, Selector, TestApp, TestConfig};
use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
use makepad_zune_png::PngDecoder;
use std::time::Duration;

/// The CPU time a wait may spend on the app's main thread: a cold or loaded box gets there late,
/// not never. A dead app fails the wait at once, whatever the budget.
const HANG: Duration = Duration::from_secs(300);

const MUSHROOM: &str = "// A cap on a stem: the cap's underside overhangs all round.
use mod.model.*
use mod.fab.*
let m = mod.model
let stem = m.box({size: [8mm 16mm 8mm] at: [0, 8mm, 0]})
let cap = m.box({size: [32mm 3mm 32mm] at: [0, 17.5mm, 0]})
let mushroom = m.union(stem, cap)
object(mushroom)
";

const PLATE: &str = r#"settings({enable_support: "1" support_type: "tree(auto)"})
place("mushroom", object: "mushroom", at: [175mm 160mm], name: "Mushroom")
"#;

/// The window, in logical pixels, at device scale 1.
const WINDOW: (f64, f64) = (1280.0, 860.0);

/// The bar's toggles of the Sliced view, besides the projection.
const SLICED: [&str; 3] = ["show_supports", "show_travel", "show_parts"];

/// A fab tool's reply text; an error when the tool refused (its edit not committed).
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
    let v = makepad_strict_json::parse_depth(line.as_bytes(), 32).map_err(|e| format!("{e:?}"))?;
    let r = match v.get("Result").expect("Result") {
        Value::Arr(a) => a[0].clone(),
        other => other.clone(),
    };
    let text = r.get("text").and_then(Value::as_str).unwrap_or_default().to_string();
    match r.get("outcome").and_then(|o| o.get("Ok")) {
        Some(_) => Ok(text),
        None => Err(format!("fab.{tool} refused: {text}")),
    }
}

fn head_of(reply: &str) -> String {
    makepad_script::syntax::parse_value(reply).expect("a Splash reply").field("head").and_then(|f| f.value.string()).expect("a head").to_string()
}

/// What the viewport reports of the recording on display.
fn recording(app: &TestApp) -> Value {
    let viewport = app.locator(Selector::id("viewport")).snapshot();
    makepad_strict_json::parse_depth(viewport.description.as_bytes(), 8).unwrap_or(Value::Null)
}

fn at<'a>(v: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(v, |v, k| v.get(k))
}

/// The scene's recordings so far: one more each time what the view draws changes.
fn records(app: &TestApp) -> u64 {
    at(&recording(app), &["records", "scene"]).and_then(Value::as_u64).unwrap_or(0)
}

/// Whether the view drew orthographic.
fn camera_ortho(app: &TestApp) -> bool {
    at(&recording(app), &["camera", "ortho"]).and_then(Value::as_bool).unwrap_or(false)
}

/// What the view draws with, as its recording has it: its eye, target and up, and its height
/// when it is orthographic (a perspective view keeps the height it last had, and draws without it).
fn view(app: &TestApp) -> Vec<f64> {
    let r = recording(app);
    let c = r.get("camera").unwrap_or_else(|| panic!("the recording's camera: {}", r.to_json()));
    let num = |v: &Value| match v {
        Value::F64(f) => Some(*f),
        Value::Int(i) => Some(*i as f64),
        _ => None,
    };
    let mut out = Vec::with_capacity(10);
    for k in ["eye", "target", "up"] {
        let a: Vec<f64> = c.get(k).and_then(Value::as_arr).map(|a| a.iter().filter_map(num).collect()).unwrap_or_default();
        assert_eq!(a.len(), 3, "the camera's {k}: {}", c.to_json());
        out.extend(a);
    }
    if c.get("ortho").and_then(Value::as_bool) == Some(true) {
        out.push(c.get("ortho_height").and_then(num).unwrap_or_else(|| panic!("the camera's ortho_height: {}", c.to_json())));
    }
    out
}

/// The viewport's rectangle (logical pixels): x, y, w, h.
fn view_rect(app: &TestApp) -> [f64; 4] {
    let node = app.locator(Selector::id("viewport")).snapshot();
    [node.x() as f64, node.y() as f64, node.width() as f64, node.height() as f64]
}

/// `fab.slice` says the slice is current and whole (no live tiles left in it), served for the
/// newest input.
fn whole_slice(text: &str) -> bool {
    let Ok(reply) = makepad_script::syntax::parse_value(text) else { return false };
    let value = |name: &str| reply.field(name).map(|f| &f.value);
    let number = |name: &str| value(name).map(|v| v.text(text).to_string());
    value("state").and_then(|v| v.string()) == Some("Sliced") && value("whole").is_some_and(|v| v.text(text) == "true") && number("revision").is_some() && number("revision") == number("served")
}

/// The live slice's revisions, as `fab.slice` gives them: its newest input's and the one its slice
/// was served for. A new slice moves them.
fn revisions(app: &TestApp) -> (String, String) {
    let text = call(app, "slice", "{}").expect("fab.slice");
    let reply = makepad_script::syntax::parse_value(&text).expect("a Splash reply");
    let value = |name: &str| reply.field(name).map(|f| f.value.text(&text).to_string()).unwrap_or_default();
    (value("revision"), value("served"))
}

/// A command of the palette (F3), as the person runs it.
fn command(app: &TestApp, name: &str) {
    app.press_key(KeyCode::Escape);
    app.press_key(KeyCode::F3);
    app.type_text(name);
    app.press_key(KeyCode::ReturnKey);
}

/// A row of the View menu, picked.
fn view_menu(app: &TestApp, row: &str) {
    app.locator(Selector::role(Role::Button).name("View")).wait_visible().press();
    app.locator(Selector::role(Role::MenuItem).name(row)).wait_visible().press();
}

/// The bar in `mode` shows the projection, and the Sliced view's toggles if `sliced`, else not.
fn bar_holds(app: &TestApp, mode: &str, sliced: bool, failed: &mut Vec<String>) {
    if app.locator(Selector::id("ortho")).try_wait_visible().is_err() {
        failed.push(format!("{mode}: the bar does not show the projection"));
    }
    for id in SLICED {
        let toggle = app.locator(Selector::id(id));
        if (if sliced { toggle.try_wait_visible() } else { toggle.try_wait_hidden() }).is_err() {
            failed.push(format!("{mode}: the bar {} its {id}", if sliced { "does not show" } else { "shows" }));
        }
    }
}

/// A grab's pixels.
struct Grab {
    width: usize,
    height: usize,
    rgb: Vec<[u8; 3]>,
}

impl Grab {
    fn decode(path: &std::path::Path) -> Grab {
        let bytes = std::fs::read(path).expect("the grab");
        let mut dec = PngDecoder::new(ZCursor::new(&bytes));
        let px = dec.decode_raw().expect("png");
        let (width, height) = dec.dimensions().expect("dims");
        let n = dec.colorspace().expect("colorspace").num_components();
        let rgb = (0..width * height).map(|i| [px[i * n], px[i * n + 1], px[i * n + 2]]).collect();
        Grab { width, height, rgb }
    }

    /// The pixels over a rectangle (logical pixels: x, y, w, h) that `like` takes.
    fn count(&self, [x, y, w, h]: [f64; 4], like: impl Fn([f64; 3]) -> bool) -> usize {
        let scale = self.width as f64 / WINDOW.0;
        let (x0, y0) = ((x * scale) as usize, (y * scale) as usize);
        let (x1, y1) = ((((x + w) * scale) as usize).min(self.width), (((y + h) * scale) as usize).min(self.height));
        (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter(|&(x, y)| like(self.rgb[y * self.width + x].map(f64::from))).count()
    }
}

/// A support's green in the Feature colours (its beads, its interface's): green well over red and
/// blue alike. The part's inner walls' yellow-green has red near its green, its bridges' and the
/// brim's teal blue near it.
fn support_green([r, g, b]: [f64; 3]) -> bool {
    g >= 60.0 && r <= 0.62 * g && b <= 0.62 * g
}

/// The part's outer walls' orange in the Feature colours: red, about half as much green, less blue.
fn wall_orange([r, g, b]: [f64; 3]) -> bool {
    r >= 110.0 && g >= 0.3 * r && g <= 0.65 * r && b <= 0.45 * r
}

/// The view's finished picture once it has recorded the scene again since `since` (settled, its
/// still average complete, two grabs alike), kept as `name` in a run that keeps its outputs.
fn finished_grab(app: &TestApp, name: &str, since: u64) -> Grab {
    loop {
        app.wait_until_within(HANG, || {
            let r = recording(app);
            let done = at(&r, &["records", "scene"]).and_then(Value::as_u64).is_some_and(|n| n > since)
                && r.get("settled").and_then(Value::as_bool) == Some(true)
                && r.get("still_samples").and_then(Value::as_u64) == Some(makepad_render::STILL_AA_SAMPLES as u64);
            if done { None } else { Some(format!("{name}: the view's finished picture")) }
        });
        let path = app.screenshot();
        let first = std::fs::read(&path).expect("the grab");
        let again = app.screenshot();
        if std::fs::read(&again).expect("the grab") == first {
            if let Some(dir) = std::env::var_os("DEVHUB_KEEP") {
                std::fs::copy(&again, std::path::Path::new(&dir).join(format!("{name}.png"))).expect("keep the grab");
            }
            return Grab::decode(&again);
        }
    }
}

#[test]
fn the_view_bar_holds_each_modes_toggles_and_hides_the_supports_without_a_new_slice() {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "view_bar").unwrap();
    config.env.insert("MAKEPAD_HAL_BACKEND".into(), "offscreen".into());
    config.app_args = vec!["--window".into(), format!("{}x{}@1", WINDOW.0, WINDOW.1)];
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let prefs = std::env::temp_dir().join(format!("fab-view-bar-{}-{nanos}.prefs", std::process::id()));
    // the code split off (the view's full width)
    std::fs::write(&prefs, "code=0\n").unwrap();
    config.env.insert("FAB_VIEW_PREFS".into(), prefs.display().to_string());
    let result = run_with_config(config, |app: TestApp| {
        let mut failed: Vec<String> = Vec::new();
        let mut head = String::new();
        app.wait_until_within(HANG, || {
            if call(&app, "inspect", "{}").map(|v| head = head_of(&v)).is_ok() { None } else { Some("the fab tools".into()) }
        });
        let batch = format!(r#"{{"request_id":"s","expected":{head:?},"script":"mushroom.cad.splash","source":{}}}"#, Value::Str(MUSHROOM.to_string()).to_json());
        head = head_of(&call(&app, "script", &batch).expect("script"));
        let batch = format!(r#"{{"request_id":"p","expected":{head:?},"edits":{}}}"#, Value::Str(PLATE.to_string()).to_json());
        call(&app, "place", &batch).expect("place");
        app.wait_until_within(HANG, || {
            if at(&recording(&app), &["parts_drawn"]).and_then(Value::as_u64).is_some_and(|n| n >= 1) { None } else { Some("the part drawn".into()) }
        });

        // Normal: the projection alone.
        app.locator(Selector::id("rm_normal")).wait_visible().wait_checked(true);
        bar_holds(&app, "Normal", false, &mut failed);
        finished_grab(&app, "1-normal", 0);

        // Framed from the front, the part selected; then Sliced, its trees planned: all four.
        command(&app, "View: Front");
        app.locator(Selector::role(Role::Tab).name("Parts")).click();
        app.locator(Selector::role(Role::TreeItem).name("Mushroom")).wait_visible().click();
        view_menu(&app, "Frame Selected");
        app.locator(Selector::id("rm_slice")).wait_visible().click();
        app.locator(Selector::id("rm_slice")).wait_checked(true);
        app.wait_until_within(HANG, || {
            if call(&app, "slice", "{}").is_ok_and(|t| whole_slice(&t)) { None } else { Some("the slice".into()) }
        });
        bar_holds(&app, "Sliced", true, &mut failed);
        app.locator(Selector::id("show_supports")).wait_checked(true);
        // The part let go: a click above the cap. The pictures are read over the view's middle,
        // where the framed part stands, clear of what draws over the view (the bar, the view
        // cube, the playback).
        let viewport = view_rect(&app);
        app.press_at(viewport[0] + viewport[2] * 0.5, viewport[1] + viewport[3] * 0.1).release();
        let rect = [viewport[0] + viewport[2] * 0.15, viewport[1] + viewport[3] * 0.15, viewport[2] * 0.7, viewport[3] * 0.7];
        let shown = finished_grab(&app, "2-sliced", 0);
        let slice = revisions(&app);
        let (green, orange) = (shown.count(rect, support_green), shown.count(rect, wall_orange));
        println!("[view-bar] Sliced, the supports shown: {green} green pixels, {orange} orange; the slice {slice:?}");
        assert!(green >= 300 && orange >= 300, "the supports and the part's walls do not show: {green} green pixels, {orange} orange");

        // Hidden from the bar: none of their green, the walls' orange kept (more of it: the stem
        // stood behind the trees).
        let since = records(&app);
        app.locator(Selector::id("show_supports")).click();
        app.locator(Selector::id("show_supports")).wait_checked(false);
        let hidden = finished_grab(&app, "3-supports-hidden", since);
        let (green_off, orange_off) = (hidden.count(rect, support_green), hidden.count(rect, wall_orange));
        println!("[view-bar] the supports hidden: {green_off} green pixels, {orange_off} orange");
        if green_off * 50 > green {
            failed.push(format!("with the supports hidden {green_off} green pixels are left of {green}"));
        }
        if orange_off * 10 < orange * 9 {
            failed.push(format!("with the supports hidden the part's walls show {orange_off} orange pixels, {orange} before"));
        }

        // Their key shows them again, as they were.
        let since = records(&app);
        app.locator(Selector::id("viewport")).press_key(KeyCode::KeyU);
        app.locator(Selector::id("show_supports")).wait_checked(true);
        let again = finished_grab(&app, "4-supports-again", since);
        let green_again = again.count(rect, support_green);
        println!("[view-bar] U, the supports shown again: {green_again} green pixels");
        if green_again.abs_diff(green) * 33 > green {
            failed.push(format!("the supports shown again draw {green_again} green pixels, {green} before they were hidden"));
        }

        // The View menu's row hides and shows them too, the bar's toggle with it.
        for on in [false, true] {
            view_menu(&app, "Show Supports");
            if app.locator(Selector::id("show_supports")).try_wait_checked(on).is_err() {
                failed.push(format!("the View menu's Show Supports left the bar's toggle {}", if on { "off" } else { "on" }));
            }
        }
        let after = revisions(&app);
        if after != slice {
            failed.push(format!("hiding and showing the supports sliced again: the slice's revisions {slice:?}, then {after:?}"));
        }

        // The bar's projection and the View menu's are one: the bar turns it, the menu turns it
        // back, the bar's toggle following, the view coming back exactly.
        app.wait_until_within(HANG, || {
            if at(&recording(&app), &["settled"]).and_then(Value::as_bool) == Some(true) { None } else { Some("the view at rest before the bar's projection".into()) }
        });
        let before = view(&app);
        let ortho = camera_ortho(&app);
        app.locator(Selector::id("ortho")).click();
        app.wait_until_within(HANG, || if camera_ortho(&app) != ortho { None } else { Some("the bar's projection".into()) });
        app.locator(Selector::id("ortho")).wait_checked(!ortho);
        view_menu(&app, "Orthographic");
        app.wait_until_within(HANG, || if camera_ortho(&app) == ortho { None } else { Some("the menu's projection".into()) });
        app.wait_until_within(HANG, || {
            if at(&recording(&app), &["settled"]).and_then(Value::as_bool) == Some(true) { None } else { Some("the view at rest after the menu's projection".into()) }
        });
        let after = view(&app);
        let d = before.iter().zip(&after).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
        println!("[view-bar] the projection by the bar and back by the menu: the view moved {d:.2e}");
        if d > 1e-9 {
            failed.push(format!("the bar's projection and back by the menu moved the view {d:.2e}: {before:?} -> {after:?}"));
        }
        if app.locator(Selector::id("ortho")).try_wait_checked(ortho).is_err() {
            failed.push("the View menu's Orthographic left the bar's toggle as it was".into());
        }

        // Force, then Normal again: the projection alone.
        for (mode, segment) in [("Force", "rm_force"), ("Normal", "rm_normal")] {
            app.locator(Selector::id(segment)).wait_visible().click();
            app.locator(Selector::id(segment)).wait_checked(true);
            bar_holds(&app, mode, false, &mut failed);
        }
        finished_grab(&app, "5-normal-again", 0);
        assert!(failed.is_empty(), "{}", failed.join("\n"));
    });
    let _ = std::fs::remove_file(&prefs);
    result.unwrap();
}
