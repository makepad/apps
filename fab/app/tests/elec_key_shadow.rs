//! A board takes a part's key shadow: the board under a tower reads darker than
//! its open top under the same studio light.
//!
//! Why (user): the electronics view should look like a product shot, with shiny metal and real
//! shadows. Before the receive hook the realtime board read flat, about three times too bright
//! in a part's shadow. This is the architect's post-land guard for that hook and its caster set.
//!
//! One made-up plate, written into its own temporary folder: a 30 mm square board with an
//! 8 x 8 x 12 mm neutral grey box in its middle, riding in a stand hidden under it. The part
//! defines its own symbol, footprint and generated body; no supplier models or stored fixtures.
//! Open the project by remote drop, select Board in Design, then View > Viewpoint > Top, View >
//! Frame Selected and View > Shading > Bright Studio. The board layer draws the board the same in
//! every view that shows the plate; Electronics' own tabs show none. The test's own preferences
//! make the view orthographic: the board's top square on, without edge walls.
//!
//! Read the view's finished picture, inside the 3D viewport only. Keep green board pixels
//! (G > 1.15 R, G > 1.15 B, G > 16), excluding the bed, grey tower, silk, pads and near-black.
//! Decode each channel with (v / 255)^2.2 and take Rec. 709 linear luma. At least 5,000 board
//! pixels must remain; their 2nd percentile (umbra) divided by their median (lit) must be < 0.85.
//! The studio key is 52 degrees up: the tower's shadow reaches about 9 mm across the board and
//! covers about an eighth of its top, so the 2nd percentile lies inside it and the median lies
//! outside it. A board facing up takes the studio dome's whole fill, which the shadow leaves.
//!
//! Measured on a Linux node's GPU under Bright Studio, the board view's default lighting: board
//! 332,221 px, lit 0.2603, umbra 0.1971, ratio 0.757. Its front scrim fills the shadow where the
//! old Studio default did not (0.598 there), and was dimmed to 3.0 EV to keep this shadow; the
//! limit sits between that and a broken shadow. With the board's receive hook returning 1, with
//! the electronics casters drawing nothing, or with the tower not yet built when the picture is
//! taken, the board's top is one flat face under one light: ratio 0.98 to 0.99.
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

#[path = "common/picture.rs"]
mod picture;
#[path = "common/parts_tab.rs"]
mod parts_tab;
use makepad_strict_json::Value;
use makepad_test::{run_with_config, Role, Selector, TestApp, TestConfig};
use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
use makepad_zune_png::PngDecoder;
use std::time::Duration;

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);
/// The window in logical pixels, at device scale 1.
const WINDOW: (f64, f64, f64) = (1600.0, 1000.0, 1.0);
const BOARD_PIXELS: usize = 5_000;
const SHADOW: f64 = 0.85;

const PROJECT: &str = "use mod.fab.*\nscript(\"guard.cad.splash\")\n";
/// The board rides in a stand hidden under it: an inserted board solid is the case part that holds
/// it, so the host draws the real board in place (an object of its own would print as a plain
/// solid, the board drawn beside it). No case walls: their shadow would fall on the board too.
const MODEL: &str = r#"use mod.model.*
use mod.fab.*
let m = mod.model
let pcb = board("guard")
let f = enclosure(pcb, {}).frame
object(m.box({size: [20mm, f.y0, 20mm] at: [0, f.y0 / 2, 0]}), name: "Stand")
insert(board_solid(pcb, board_bodies("guard"), f), name: "Board", into: "Stand")
"#;
const BOARD: &str = r#"board("guard")
outline([[[0mm, 0mm], [30mm, 0mm], [30mm, 30mm], [0mm, 30mm], [0mm, 0mm]]])
place("tower", [15mm, 15mm])
"#;
const CIRCUIT: &str = r##"use mod.elec.*
let e = mod.elec
let f = e.f
e.define("guard:tower", {
    kind: @resistor
    symbol: f.two_terminal([["1", @passive], ["2", @passive]])
    footprint: f.footprint({
        pads: [
            f.smd("1", [-0.6mm, 0mm], [0.4mm, 0.4mm]),
            f.smd("2", [0.6mm, 0mm], [0.4mm, 0.4mm])
        ]
        body: [[-4mm, -4mm], [4mm, 4mm]]
        mount: @smd
        text: 0.6mm
        graphics: []
    })
    body: {height: 12mm surface: {material: "pla" finish: "vdi_45" colour: "#d0d0d0"}}
    pads: [["1", "1"], ["2", "2"]]
    prefix: "R"
})
let tower = e.part("guard:tower", {})
"##;

fn remote(app: &TestApp, args: &[String]) -> Result<Vec<String>, String> {
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).map_err(|e| format!("connect: {e:?}"))?;
    let mut lines = Vec::new();
    makepad_remote::app::run(&client, args, &mut |line| {
        lines.push(line.to_string());
        true
    })
    .map_err(|e| format!("{args:?}: {e:?}; {lines:?}"))?;
    if lines.iter().any(|l| l.contains("\"err\":")) {
        return Err(format!("{args:?}: {lines:?}"));
    }
    Ok(lines)
}

/// A fab tool's reply text; an error when the tool refused.
fn call(app: &TestApp, tool: &str, args: &str) -> Result<String, String> {
    let lines = remote(app, &["call".into(), format!("fab.{tool}"), args.into()])?;
    let line = lines.iter().find(|l| l.contains("\"Result\"")).ok_or_else(|| format!("fab.{tool}: no result: {lines:?}"))?;
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

fn wait_until(app: &TestApp, what: &str, mut ok: impl FnMut() -> bool) {
    app.try_wait_until_within(HANG, || (!ok()).then(|| format!("waiting for {what}"))).unwrap_or_else(|why| {
        panic!("{why}; timed out waiting for {what}; the viewport reports {}", app.locator(Selector::id("viewport")).snapshot().description);
    });
}

/// What the viewport reports of the recording on display.
fn recording(app: &TestApp) -> Value {
    let viewport = app.locator(Selector::id("viewport")).snapshot();
    makepad_strict_json::parse_depth(viewport.description.as_bytes(), 8).unwrap_or(Value::Null)
}

/// A caption menu and its flyouts, by their item names.
fn view_menu(app: &TestApp, items: &[&str]) {
    app.locator(Selector::role(Role::Button).name("View")).wait_visible().press();
    for name in items {
        app.locator(Selector::role(Role::MenuItem).name(*name)).wait_visible().press();
    }
}

/// The viewport's rectangle in screenshot pixels; no pixels from the electronics canvases.
fn viewport_rect(app: &TestApp, width: usize, height: usize) -> (usize, usize, usize, usize) {
    let b = app.locator(Selector::id("viewport")).wait_visible().snapshot().bounds;
    let scale = width as f64 / WINDOW.0;
    let (x0, y0) = ((b.x as f64 * scale).ceil().max(0.0) as usize, (b.y as f64 * scale).ceil().max(0.0) as usize);
    let (x1, y1) = (((b.x + b.w) as f64 * scale).floor().max(0.0) as usize, ((b.y + b.h) as f64 * scale).floor().max(0.0) as usize);
    (x0.min(width), y0.min(height), x1.min(width), y1.min(height))
}

/// The board's linear light, sorted from its shadow to its open face.
fn check_shadow(app: &TestApp) {
    let path = picture::finished_screenshot(app);
    let bytes = std::fs::read(&path).expect("the grab");
    let mut dec = PngDecoder::new(ZCursor::new(&bytes));
    let px = dec.decode_raw().expect("png");
    let (width, height) = dec.dimensions().expect("dims");
    let n = dec.colorspace().expect("colorspace").num_components();
    let (x0, y0, x1, y1) = viewport_rect(app, width, height);
    let mut light = Vec::new();
    for y in y0..y1 {
        for x in x0..x1 {
            let i = (y * width + x) * n;
            let (r, g, b) = (px[i] as f64, px[i + 1] as f64, px[i + 2] as f64);
            if g > 16.0 && g > 1.15 * r && g > 1.15 * b {
                let linear = |v: f64| (v / 255.0).powf(2.2);
                light.push(0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b));
            }
        }
    }
    let count = light.len();
    assert!(count >= BOARD_PIXELS, "board in view: {count} green pixels in viewport ({x0}, {y0})..({x1}, {y1}), need at least {BOARD_PIXELS}; see {path:?}");
    light.sort_unstable_by(f64::total_cmp);
    let lit = (light[(count - 1) / 2] + light[count / 2]) * 0.5;
    let umbra = light[(count - 1) * 2 / 100];
    let ratio = umbra / lit;
    println!("[fab-elec-key-shadow] board {count} px; lit {lit:.6}; umbra {umbra:.6}; umbra/lit {ratio:.4}; see {path:?}");
    assert!(ratio < SHADOW, "board takes the tower's key shadow: umbra/lit {ratio:.4}, need < {SHADOW}; umbra {umbra:.6}, lit {lit:.6}, board {count} px; see {path:?}");
}

#[test]
fn the_board_takes_a_parts_key_shadow() {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "elec_key_shadow").unwrap();
    config.env.insert("MAKEPAD_HAL_BACKEND".into(), "offscreen".into());
    config.app_args = vec!["--window".into(), format!("{}x{}@{}", WINDOW.0, WINDOW.1, WINDOW.2)];
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("fab-elec-key-shadow-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&dir).expect("a fresh folder for the guard");
    let project = dir.join("guard");
    std::fs::create_dir_all(project.join("guard.elec")).expect("the electronics folder");
    let plate = project.join("guard.fab.splash");
    std::fs::write(&plate, PROJECT).expect("the project");
    std::fs::write(project.join("guard.cad.splash"), MODEL).expect("the board object");
    std::fs::write(project.join("guard.elec/guard.pcb.splash"), BOARD).expect("the board");
    std::fs::write(project.join("guard.elec/guard.ckt.splash"), CIRCUIT).expect("the tower's circuit");
    let prefs = dir.join("view.prefs");
    std::fs::write(&prefs, "code=0\northo=1\n").expect("the test's view preferences");
    config.env.insert("FAB_VIEW_PREFS".into(), prefs.display().to_string());
    let result = run_with_config(config, |app: TestApp| {
        app.locator(Selector::role(Role::Window)).wait_visible();
        wait_until(&app, "the fab tools", || call(&app, "inspect", "{}").is_ok());
        let b = app.locator(Selector::id("viewport")).wait_visible().snapshot().bounds;
        let scene = recording(&app).get("records").and_then(|r| r.get("scene")).and_then(Value::as_u64).unwrap_or(0);
        let lines = remote(&app, &["drop".into(), format!("path={}", plate.display()), format!("x={}", b.x + b.w * 0.5), format!("y={}", b.y + b.h * 0.5)]).expect("drop the guard project");
        assert!(lines.iter().any(|l| l.contains("\"ok\":1")), "open the guard project: {lines:?}");
        parts_tab::open(&app);
        app.locator(Selector::role(Role::TreeItem).name("Board")).wait_visible();
        // A linked board draws in the board layer, outside the recording's parts_drawn count.
        // Its built Parts row and a new scene recording precede the finished-picture wait.
        wait_until(&app, "the board's scene recorded", || recording(&app).get("records").and_then(|r| r.get("scene")).and_then(Value::as_u64).is_some_and(|n| n > scene));
        wait_until(&app, "the opened plate's finished picture", || picture::finished(&app));

        // Select and frame the board in Design, square on from the top.
        app.locator(Selector::role(Role::TreeItem).name("Board")).wait_visible().click();
        view_menu(&app, &["Viewpoint", "Top"]);
        // A semantic press waits for its action's frame. Wait out the glide even when the
        // camera was already at its destination (framing one part can leave it unchanged).
        wait_until(&app, "the Top camera at rest", || recording(&app).get("settled").and_then(Value::as_bool) == Some(true));
        view_menu(&app, &["Frame Selected"]);
        wait_until(&app, "the framed camera at rest", || recording(&app).get("settled").and_then(Value::as_bool) == Some(true));
        // Electronics is full-page tabs with no plate view: the board layer draws the same board
        // in Design, here under the light the board view uses.
        view_menu(&app, &["Shading", "Bright Studio"]);
        wait_until(&app, "the board's finished picture under Bright Studio", || picture::finished(&app));
        check_shadow(&app);
    });
    let cleanup = std::fs::remove_dir_all(&dir);
    result.unwrap();
    cleanup.expect("remove the guard's temporary folder");
}
