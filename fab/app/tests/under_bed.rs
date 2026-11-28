//! Turning the print over: a drag up in the view orbits on past horizontal and under the bed
//! plane, nothing holding it there, and the floor goes (the bed, the plate and its face, the grid,
//! the ground's shadows) so the print's plate-facing side shows: in the Normal view the part's
//! underside, in Sliced the first layer's beads, each in its filament's colour and lit (never a
//! black underside). Dragged back over the plane, the floor is drawn again.
//!
//! Why (user): "i need to be able to turn the print further then the ground plane, in which case
//! the ground plane should turn off so i can see the print-plate facing output both in sliced and
//! normal".
//!
//! One plate: an 80 mm square slab, 10 mm thick, in orange in the middle of the bed, so the
//! press in the middle of the view is on it (the orbit's pivot, which keeps its pixel).
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

#[path = "common/render_mode.rs"]
mod render_mode;
use makepad_strict_json::Value;
use makepad_test::{run_with_config, Selector, TestApp, TestConfig};
use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
use makepad_zune_png::PngDecoder;
use std::time::Duration;

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);

/// The pitch (degrees, negative: under the bed plane) the drag goes on to, well past horizontal.
const UNDER: f64 = -40.0;

/// How much of the view's middle shows the part's orange at least, from under the bed.
const SHOWN: f64 = 0.8;

const PART: &str = "use mod.model.*
use mod.fab.*
let m = mod.model
let slab = m.box({size: [80mm 10mm 80mm] at: [0, 5mm, 0]})
object(slab)
";

const PLATE: &str = "color(\"Orange\", rgb: #xe07020)\nplace(\"slab\", at: [175mm 160mm], name: \"Slab\", color: \"Orange\")";

/// The window, in logical pixels, at device scale 2.
const WINDOW: (f64, f64, f64) = (1600.0, 1000.0, 2.0);

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

/// A fab tool's reply text; an error when the tool refused (its edit not committed).
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

fn head_of(reply: &str) -> String {
    let reply = makepad_script::syntax::parse_value(reply).expect("a Splash reply");
    reply.field("head").and_then(|f| f.value.string()).expect("reply has a head").to_string()
}

/// `fab.slice` says the slice is current and whole (no live tiles left in it).
fn whole_slice(text: &str) -> bool {
    let Ok(reply) = makepad_script::syntax::parse_value(text) else { return false };
    let value = |name: &str| reply.field(name).map(|f| &f.value);
    value("state").and_then(|v| v.string()) == Some("Sliced") && value("whole").is_some_and(|v| v.text(text) == "true")
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

/// The recording's camera: its eye and look point (metres), and its pitch over the bed plane
/// (degrees, negative under it).
fn camera(app: &TestApp) -> ([f64; 3], [f64; 3], f64) {
    let camera = recording(app).get("camera").cloned().unwrap_or(Value::Null);
    let num = |v: &Value| match v {
        Value::F64(f) => Some(*f),
        Value::Int(i) => Some(*i as f64),
        _ => None,
    };
    let at = |k: &str| -> [f64; 3] {
        let v: Vec<f64> = camera.get(k).and_then(Value::as_arr).map(|a| a.iter().filter_map(num).collect()).unwrap_or_default();
        assert_eq!(v.len(), 3, "the camera's {k}: {}", camera.to_json());
        [v[0], v[1], v[2]]
    };
    let (eye, target) = (at("eye"), at("target"));
    let d = [eye[0] - target[0], eye[1] - target[1], eye[2] - target[2]];
    let pitch = (d[2] / (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()).asin().to_degrees();
    (eye, target, pitch)
}

fn floor(app: &TestApp) -> Option<bool> {
    recording(app).get("floor").and_then(Value::as_bool)
}

/// The view's finished picture (settled, its still average complete), two grabs alike.
fn finished_grab(app: &TestApp) -> (std::path::PathBuf, Grab) {
    wait_poll(app, || {
        let r = recording(app);
        if !(r.get("settled").and_then(Value::as_bool) == Some(true) && r.get("still_samples").and_then(Value::as_u64) == Some(makepad_render::STILL_AA_SAMPLES as u64)) {
            return Err("waiting for the finished picture".to_string());
        }
        let first = std::fs::read(app.screenshot()).expect("the grab");
        let path = app.screenshot();
        if std::fs::read(&path).expect("the grab") == first {
            let grab = Grab::decode(&path);
            return Ok((path, grab));
        }
        Err("waiting for two matching grabs".to_string())
    })
}

/// A grab's pixels.
struct Grab {
    width: usize,
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
        Grab { width, rgb }
    }

    /// In a square of half side `r` (device pixels) about `(cx, cy)`: the share of its pixels in
    /// the part's orange (red well over green over blue, not the bed's or the backdrop's greys),
    /// and their mean luma (0..255).
    fn orange(&self, cx: usize, cy: usize, r: usize) -> (f64, f64) {
        let (mut n, mut hit, mut luma) = (0usize, 0usize, 0.0);
        for y in cy - r..cy + r {
            for x in cx - r..cx + r {
                let [red, green, blue] = self.rgb[y * self.width + x].map(|c| c as i32);
                n += 1;
                if red > green + 12 && green > blue && red > blue + 30 {
                    hit += 1;
                    luma += 0.2126 * red as f64 + 0.7152 * green as f64 + 0.0722 * blue as f64;
                }
            }
        }
        (hit as f64 / n as f64, luma / hit.max(1) as f64)
    }
}

/// The view's middle in device pixels, where the drag's pivot keeps the slab.
fn middle(app: &TestApp, grab: &Grab) -> (usize, usize) {
    let b = app.locator(Selector::id("viewport")).snapshot().bounds;
    let scale = grab.width as f64 / WINDOW.0;
    (((b.x + b.w * 0.5) as f64 * scale) as usize, ((b.y + b.h * 0.5) as f64 * scale) as usize)
}

/// Drag from the view's middle, up (`up` true) or down, in steps till the camera's pitch is past
/// `to` degrees, held there and let go. Returns the pitches the drag went through.
fn orbit_to(app: &TestApp, to: f64, up: bool) -> Vec<f64> {
    let past = |pitch: f64| if up { pitch <= to } else { pitch >= to };
    let mut drag = app.locator(Selector::id("viewport")).press_and_drag();
    let mut pitches = vec![camera(app).2];
    let mut dy = 0.0;
    while !past(*pitches.last().unwrap()) {
        assert!(pitches.len() < 40, "the orbit stopped at {:.1} degrees, short of {to}: {pitches:?}", pitches.last().unwrap());
        dy += if up { -20.0 } else { 20.0 };
        let before = camera(app).0;
        drag.move_by(0.0, dy);
        wait_until(app, "the drag's frame", || camera(app).0 != before);
        pitches.push(camera(app).2);
    }
    // rest before letting go, so the drag does not fling on
    std::thread::sleep(Duration::from_millis(300));
    drag.release();
    pitches
}

/// From under the bed, the view's finished picture: the floor not drawn, the middle in the
/// part's orange, lit. `what`: the view's name.
fn check_under(app: &TestApp, what: &str) {
    let (path, grab) = finished_grab(app);
    let (eye, _, pitch) = camera(app);
    assert!(eye[2] < 0.0 && pitch < UNDER + 5.0, "{what}: the camera is not under the bed: eye {eye:?}, pitch {pitch:.1}");
    assert_eq!(floor(app), Some(false), "{what}: the floor is drawn from under the bed; see {path:?}");
    let (cx, cy) = middle(app, &grab);
    let (shown, luma) = grab.orange(cx, cy, 40);
    println!("[fab-under-bed] {what}: pitch {pitch:.1}, eye {eye:?}; the middle {:.0} % orange, luma {luma:.0}; {path:?}", shown * 100.0);
    assert!(shown >= SHOWN, "{what}: from under the bed the middle of the view is {:.0} % the part's orange (the floor in the way?); see {path:?}", shown * 100.0);
    assert!(luma > 30.0, "{what}: the underside is black (luma {luma:.0}); see {path:?}");
}

#[test]
fn an_orbit_turns_on_under_the_bed_and_the_floor_goes() {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "under_bed").unwrap();
    config.env.insert("MAKEPAD_HAL_BACKEND".into(), "offscreen".into());
    config.app_args = vec!["--window".into(), format!("{}x{}@{}", WINDOW.0, WINDOW.1, WINDOW.2)];
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let prefs = std::env::temp_dir().join(format!("fab-under-bed-{}-{nanos}.prefs", std::process::id()));
    // The code split off (the view's full width), the printed surface (each bead in its filament).
    std::fs::write(&prefs, "code=0\nsurface=1\n").unwrap();
    config.env.insert("FAB_VIEW_PREFS".into(), prefs.display().to_string());
    let result = run_with_config(config, |app: TestApp| {
        let mut head = String::new();
        wait_until(&app, "the fab tools", || call(&app, "inspect", "{}").map(|v| head = head_of(&v)).is_ok());
        let batch = format!(r#"{{"request_id":"s","expected":{head:?},"script":"slab.cad.splash","source":{}}}"#, Value::Str(PART.to_string()).to_json());
        head = head_of(&call(&app, "script", &batch).expect("script"));
        let batch = format!(r#"{{"request_id":"p","expected":{head:?},"edits":{}}}"#, Value::Str(PLATE.to_string()).to_json());
        call(&app, "place", &batch).expect("place");
        wait_until(&app, "the part drawn", || recording(&app).get("parts_drawn").and_then(Value::as_u64).is_some_and(|n| n >= 1));
        wait_until(&app, "the camera at rest", || recording(&app).get("settled").and_then(Value::as_bool) == Some(true));
        let (eye, _, pitch) = camera(&app);
        assert!(eye[2] > 0.0 && pitch > 10.0 && floor(&app) == Some(true), "the opening view looks down on the plate, its floor drawn: eye {eye:?}, pitch {pitch:.1}");

        // Normal: up past horizontal, under the bed; every step turned on (none held at the plane).
        let pitches = orbit_to(&app, UNDER, true);
        assert!(pitches.windows(2).all(|w| w[1] < w[0]), "the drag up did not turn the camera down at every step: {pitches:?}");
        check_under(&app, "Normal");

        // Sliced: the same camera; the first layer's beads show from under the bed.
        render_mode::pick(&app, "Sliced");
        wait_until(&app, "the whole slice", || call(&app, "slice", "{}").is_ok_and(|t| whole_slice(&t)));
        check_under(&app, "Sliced");

        // Back over the bed plane: the floor is drawn again.
        orbit_to(&app, 20.0, false);
        let (path, _) = finished_grab(&app);
        let (eye, _, pitch) = camera(&app);
        assert!(eye[2] > 0.0, "back over the bed: eye {eye:?}, pitch {pitch:.1}");
        assert_eq!(floor(&app), Some(true), "back over the bed the floor is not drawn; see {path:?}");
    });
    let _ = std::fs::remove_file(&prefs);
    result.unwrap();
}

fn wait_poll<T>(app: &TestApp, mut look: impl FnMut() -> Result<T, String>) -> T {
    let mut got = None;
    app.try_wait_until_within(HANG, || match look() {
        Ok(value) => {
            got = Some(value);
            None
        }
        Err(why) => Some(why),
    }).unwrap_or_else(|why| {
        panic!("{why}; the viewport reports {}", app.locator(Selector::id("viewport")).snapshot().description);
    });
    got.expect("the wait ended on an answer")
}
