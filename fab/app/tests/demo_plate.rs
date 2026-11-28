//! The demo plate as the dock shows it (Realtime shading), then from every named view:
//!
//! - its parts draw where they are, in their colours: pins the Realtime lane reading a ranked
//!   part's element index with its priority bits (the bracket drew another element's pose: off
//!   the bed, washed out);
//! - Top, Front, Back, Left, Right and Isometric, picked through the caption's View menu and its
//!   Viewpoint flyout (semantic presses, not coordinates), each show the parts' orange inside the
//!   plate view: pins the "top view is one flat blue plane" bug;
//! - the view's export at its own pixels (the rig's `vgrab`: the finished still view read back
//!   whole, a band of rows at a time), asked for the full-width view, comes whole and shows the
//!   parts' orange. The band's shader is made at the export's first step and compiles: its
//!   readiness (`Event::DrawShadersReady`) draws the next step. The step used to ask for a frame
//!   that drew nothing, a pipeline that turned ready while nothing drew it repainted nothing, and
//!   the export never came unless something else happened to draw the view.
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

#[path = "common/picture.rs"]
mod picture;
#[path = "common/view_menu.rs"]
mod view_menu;
use makepad_script::syntax;
use makepad_strict_json::Value;
use makepad_test::{run_with_config, KeyCode, KeyModifiers, Role, Selector, TestApp, TestConfig};
use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
use makepad_zune_png::PngDecoder;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);

fn wait_until(app: &TestApp, what: &str, mut ok: impl FnMut() -> bool) {
    app.try_wait_until_within(HANG, || (!ok()).then(|| format!("waiting for {what}"))).unwrap_or_else(|why| {
        if let Ok(log) = app.log_since(0) {
            for l in log.lines.iter().filter(|l| l.contains("fab")).rev().take(30).collect::<Vec<_>>().into_iter().rev() {
                println!("[app] {l}");
            }
        }
        panic!("{why}; timed out waiting for {what}; the viewport reports {}", app.locator(Selector::id("viewport")).snapshot().description);
    });
}

/// What the viewport reports of the recording on display.
fn recording(app: &TestApp) -> Value {
    let viewport = app.locator(Selector::id("viewport")).snapshot();
    makepad_strict_json::parse_depth(viewport.description.as_bytes(), 8).expect("viewport inspection JSON")
}

fn camera(app: &TestApp) -> String {
    recording(app).get("camera").expect("viewport camera").to_json()
}

fn parts_drawn(app: &TestApp) -> u64 {
    recording(app).get("parts_drawn").and_then(Value::as_u64).expect("viewport parts_drawn")
}

/// The camera of the view's finished picture: no glide, bake or lower-quality recording left.
fn settled_camera(app: &TestApp) -> String {
    wait_until(app, "the view to settle", || recording(app).get("settled").and_then(Value::as_bool).expect("viewport settled"));
    camera(app)
}

/// A PNG's pixels, its size and its channels.
fn png(path: &Path) -> (Vec<u8>, usize, usize, usize) {
    let bytes = std::fs::read(path).expect("the PNG");
    let mut dec = PngDecoder::new(ZCursor::new(&bytes));
    let px = dec.decode_raw().expect("png");
    let (w, h) = dec.dimensions().expect("dims");
    let n = dec.colorspace().expect("colorspace").num_components();
    (px, w, h, n)
}

/// Pixels of `rect` matching `is` (8-bit RGB).
fn pixels(path: &Path, rect: (usize, usize, usize, usize), is: impl Fn(i32, i32, i32) -> bool) -> usize {
    let (px, w, h, n) = png(path);
    let mut count = 0;
    for y in rect.1.min(h)..rect.3.min(h) {
        for x in rect.0.min(w)..rect.2.min(w) {
            let o = (y * w + x) * n;
            if is(px[o] as i32, px[o + 1] as i32, px[o + 2] as i32) {
                count += 1;
            }
        }
    }
    count
}

fn viewport_rect(app: &TestApp) -> (usize, usize, usize, usize) {
    let view = app.locator(Selector::id("viewport")).wait_visible().snapshot();
    (view.x().max(0) as usize, view.y().max(0) as usize, (view.x() + view.width()) as usize, (view.y() + view.height()) as usize)
}

#[test]
fn the_demo_parts_draw_where_they_are_in_every_named_view() {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    // Outside artifacts_dir: TestApp clears that directory before launching.
    let dir = std::env::temp_dir().join(format!("fab-demo-plate-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // the rig's mode file: no export until the test asks for one
    let mode = dir.join("mode");
    std::fs::write(&mode, "").unwrap();
    let name = "demo_plate::the_demo_parts_draw_where_they_are_in_every_named_view";
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), name).unwrap();
    config.env.insert("FAB_XR_FAR_MODE_FILE".into(), mode.display().to_string());
    let result = run_with_config(config, |app| named_views(app, &mode));
    let _ = std::fs::remove_dir_all(&dir);
    result.unwrap();
}

fn named_views(app: TestApp, mode: &Path) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    wait_until(&app, "the fab tools", || call(&app, "inspect", "{}").is_ok());
    let primary = if cfg!(target_os = "macos") { KeyModifiers { shift: true, logo: true, ..Default::default() } } else { KeyModifiers { shift: true, control: true, ..Default::default() } };
    app.press_key_with_modifiers(KeyCode::KeyO, primary);
    wait_until(&app, "the demo plate's three parts", || {
        let v = syntax::parse_value(&text(&call(&app, "inspect", r#"{"program":false}"#).expect("inspect"))).expect("a Splash reply");
        v.field("plate").and_then(|p| p.value.field("parts")).and_then(|f| f.value.items()).is_some_and(|p| p.len() == 3)
    });
    // The grab waits for a recording that drew all three, and for the view's finished picture.
    wait_until(&app, "a recording of the three parts", || parts_drawn(&app) == 3);
    settled_camera(&app);
    if app.locator(Selector::id("code")).count() == 0 {
        view_menu::pick(&app, "Code");
    }
    let rect = viewport_rect(&app);
    let shot = picture::finished_screenshot(&app);
    // The bracket and the lattice are a strong orange, the vase a teal. (The view gizmo alone has
    // about 220 px of the orange and 250 of the teal.)
    let orange = pixels(&shot, rect, |r, g, b| r > 140 && r - g > 50 && r - b > 80);
    let teal = pixels(&shot, rect, |r, g, b| g > 90 && g - r > 40 && b > 70);
    assert!(orange > 1500, "parts where they are: the bracket and the lattice do not draw in their colour ({orange} px) in a settled recording of {} parts; see {shot:?}", parts_drawn(&app));
    assert!(teal > 1500, "parts where they are: the vase does not draw in its colour ({teal} px) in a settled recording of {} parts; see {shot:?}", parts_drawn(&app));

    // The views at the view's full width (the code split off), as the views were made for. Its
    // finished view is exported: the mode file is read at each sample of the still view.
    std::fs::write(mode, "vgrab").unwrap();
    let narrow = rect.2 - rect.0;
    view_menu::pick(&app, "Code");
    wait_until(&app, "the view at its full width", || {
        let rect = viewport_rect(&app);
        rect.2 - rect.0 > narrow
    });
    // The file is written beside it and renamed: once it is there, it is whole. The view at its
    // own pixels: its logical size at one dpi factor (an export of the narrower view, had a sample
    // come after the picture above, is not that, and the full-width one follows it).
    let export = PathBuf::from(format!("{}.vgrab.png", mode.display()));
    let view = app.locator(Selector::id("viewport")).wait_visible().snapshot();
    let (vw, vh) = (view.width() as f64, view.height() as f64);
    let at_own_pixels = |w: usize, h: usize| w > 0 && (h as f64 - vh * w as f64 / vw).abs() <= 1.0;
    wait_until(&app, "the full-width view's export", || export.exists() && {
        let (_, w, h, _) = png(&export);
        at_own_pixels(w, h)
    });
    std::fs::write(mode, "").unwrap();
    let (px, w, h, n) = png(&export);
    assert_eq!(px.len(), w * h * n, "export: every row is there");
    let dpi = w as f64 / vw;
    let orange = pixels(&export, (0, 0, w, h), |r, g, b| r > 140 && r - g > 50 && r - b > 80);
    assert!(orange as f64 > 1500.0 * dpi * dpi, "export: the parts' orange does not show in it ({orange} px of {w}x{h}); see {export:?}");
    let mut camera_was = settled_camera(&app);
    let rect = viewport_rect(&app);
    for name in ["Top", "Front", "Back", "Left", "Right", "Isometric"] {
        app.locator(Selector::role(Role::Button).name("View")).wait_visible().press();
        app.locator(Selector::role(Role::MenuItem).name("Viewpoint")).wait_visible().press();
        app.locator(Selector::role(Role::MenuItem).name(name)).wait_visible().press();
        wait_until(&app, &format!("named views: the {name} view's camera"), || camera(&app) != camera_was);
        camera_was = settled_camera(&app);
        let shot = picture::finished_screenshot(&app);
        // The parts' orange by hue, lit or in shade (the back faces away from the light).
        let seen = pixels(&shot, rect, |r, g, b| r > 60 && r - g > 30 && r - b > 40);
        assert!(seen > 200, "named views: the {name} view does not show the parts ({seen} pixels of their orange); see {shot:?}");
    }
}
