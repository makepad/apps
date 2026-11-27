//! A wall face the headset draws in the far tier (beads a pixel or less across) stays even: no
//! layers darker than the rest. The far tier's quad once took each corner's joint side from the
//! corner itself, so the joints' swing, sampled past the quad's edges, turned the normal per bead
//! and whole layers of a wall came out darker (the joints' side now comes from the eye,
//! `joint_side` in the viewport's print shaders). The stock shelf bracket in the middle of the
//! plate, sliced, the headset (a synthetic one, still) looking at it at real size: one picture of
//! the print once it has drawn whole (`FAB_XR_GRAB_PRINT`), and the bracket's wall face read off it.
//!
//! The test sets the variables itself and starts its own synthetic headset on the native GPU, its
//! stream's AA and render scale pinned ([`pin_stream`]).

#[path = "common/render_mode.rs"]
mod render_mode;
#[path = "common/wait.rs"]
mod idle_wait;
use idle_wait::wait_poll;
use makepad_strict_json::Value;
use makepad_test::{run_with_config, Role, Selector, TestApp, TestConfig};
use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
use makepad_zune_png::PngDecoder;
use std::path::Path;

const BRACKET: &str = include_str!("../resources/bracket.cad.splash");

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

fn head_of(reply: &str) -> String {
    let reply = makepad_script::syntax::parse_value(reply).expect("a Splash reply");
    reply.field("head").and_then(|f| f.value.string()).expect("reply has a head").to_string()
}

/// `fab.slice`: the slice is current and whole (no live tiles left in it).
fn whole_slice(app: &TestApp) -> Result<(), String> {
    let t = call(app, "slice", "{}").map(|l| text(&l))?;
    let r = makepad_script::syntax::parse_value(&t).map_err(|e| format!("{e:?}"))?;
    let state = r.field("state").and_then(|f| f.value.string()).unwrap_or_default().to_string();
    let whole = r.field("whole").is_some_and(|f| f.value.text(&t) == "true");
    if state == "Sliced" && whole { Ok(()) } else { Err(format!("the slice is {state}, whole {whole}")) }
}

/// The bracket's wall face in the left eye: the largest patch of its bead colour, without the
/// patch's edge pixels.
#[derive(Debug)]
struct Face {
    pixels: usize,
    mean_red: f64,
    /// The red channel's spread over the face: darker layers raise it.
    sd_red: f64,
}

fn face(path: &Path) -> Face {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    let mut dec = PngDecoder::new(ZCursor::new(&bytes));
    let px = dec.decode_raw().unwrap_or_else(|e| panic!("decode {path:?}: {e:?}"));
    let (w, h) = dec.dimensions().expect("png dimensions");
    let n = dec.colorspace().expect("png colorspace").num_components();
    let ew = w / 2;
    let rgb = |x: usize, y: usize| {
        let i = (y * w + x) * n;
        [px[i] as i32, px[i + 1] as i32, px[i + 2] as i32]
    };
    let wall = |x: usize, y: usize| {
        let [r, g, b] = rgb(x, y);
        r > 150 && g > 80 && g < 160 && b < 120
    };
    // 4-connected patches of the wall colour, the largest kept
    let mut patch = vec![u32::MAX; ew * h];
    let (mut best, mut best_len, mut next) = (u32::MAX, 0usize, 0u32);
    for start in 0..ew * h {
        if patch[start] != u32::MAX || !wall(start % ew, start / ew) {
            continue;
        }
        let (mut stack, mut len) = (vec![start], 0usize);
        patch[start] = next;
        while let Some(i) = stack.pop() {
            len += 1;
            let (x, y) = (i % ew, i / ew);
            for (nx, ny) in [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)] {
                if nx < ew && ny < h && patch[ny * ew + nx] == u32::MAX && wall(nx, ny) {
                    patch[ny * ew + nx] = next;
                    stack.push(ny * ew + nx);
                }
            }
        }
        if len > best_len {
            (best, best_len) = (next, len);
        }
        next += 1;
    }
    let inner = |i: usize| {
        let (x, y) = (i % ew, i / ew);
        x > 0 && y > 0 && x + 1 < ew && y + 1 < h && [i - 1, i + 1, i - ew, i + ew].iter().all(|&k| patch[k] == best)
    };
    let reds: Vec<i32> = (0..ew * h).filter(|&i| patch[i] == best && inner(i)).map(|i| rgb(i % ew, i / ew)[0]).collect();
    let count = reds.len().max(1) as f64;
    let mean_red = reds.iter().map(|&r| r as f64).sum::<f64>() / count;
    let sd_red = (reds.iter().map(|&r| (r as f64 - mean_red).powi(2)).sum::<f64>() / count).sqrt();
    Face { pixels: reds.len(), mean_red, sd_red }
}

/// The stream's AA and render scale pinned (MSAA 4x, 100%) in the settings file of a folder of
/// the test's own, which `home` names: left on Auto, a box too loaded to keep the headset's rate
/// lowers the render scale, and the eye's picture shrinks with it (the face under the pixels it
/// needs). `home` is the app's test home (`MAKEPAD_TEST_HOME`), below which the app keeps its
/// data in this OS's layout (`app_data_dir`).
fn pin_stream(config: &mut TestConfig, home: &Path) {
    config.env.insert("MAKEPAD_TEST_HOME".into(), home.display().to_string());
    let dir = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Makepad/fab")
    } else if cfg!(windows) {
        home.join("AppData/Roaming/Makepad/fab")
    } else {
        home.join(".local/share/makepad/fab")
    };
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("xr-stream"), "aa=msaa4\nrender_scale=100\n").unwrap();
}

/// The bracket's wall face as the headset shows it.
fn far_face(name: &str) -> Face {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), name).unwrap();
    let grabs = config.artifacts_dir.join("grabs");
    // outside the artifacts, which the harness empties at the start
    let home = std::env::temp_dir().join(format!("fab-xr-face-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    pin_stream(&mut config, &home);
    for (k, v) in [
        ("MAKEPAD_XR_REMOTE_SELFTEST", "1"),
        ("MAKEPAD_XR_REMOTE_SELFTEST_STILL", "1"),
        ("MAKEPAD_XR_REMOTE_SELFTEST_PITCH", "-0.75"),
        ("MAKEPAD_XR_STREAM", "on"),
        ("MAKEPAD_LAN", "1"),
        ("FAB_XR_GRAB_PRINT", "face"),
    ] {
        config.env.insert(k.into(), v.into());
    }
    config.env.insert("FAB_XR_GRAB".into(), grabs.display().to_string());
    let mut seen = None;
    let r = run_with_config(config, |app| {
        app.locator(Selector::role(Role::Window)).wait_visible();
        let head = head_of(&text(&wait_poll(&app, || call(&app, "inspect", "{}"))));
        let batch = format!(r#"{{"request_id":"face","expected":{head:?},"script":"bracket.cad.splash","source":{}}}"#, Value::Str(BRACKET.to_string()).to_json());
        let head = head_of(&text(&call(&app, "script", &batch).expect("script")));
        let edits = Value::Str(r#"place("bracket", at: [175mm 160mm])"#.to_string()).to_json();
        call(&app, "place", &format!(r#"{{"request_id":"face-place","expected":{head:?},"edits":{edits}}}"#)).expect("place");
        render_mode::pick(&app, "Sliced");
        wait_poll(&app, || whole_slice(&app));
        // written on a worker, then said in the log
        let mut seq = 0;
        wait_poll(&app, || {
            let t = app.log_since(seq).map_err(|e| format!("{e:?}"))?;
            seq = t.last_seq;
            match t.lines.iter().find(|l| l.contains("fab xr grab: face:")) {
                Some(l) if l.contains("covered") => Ok(()),
                Some(l) => panic!("the print's picture: {l}"),
                None => Err("waiting for the print's picture".to_string()),
            }
        });
        seen = Some(face(&grabs.join("face.png")));
    });
    let _ = std::fs::remove_dir_all(&home);
    if let Err(e) = r {
        panic!("{}", e.message());
    }
    let f = seen.expect("the picture was read");
    println!("far wall face: {f:?}");
    f
}

/// Measured on a Linux node's GPU: the face even, 473 pixels, red 219 with a spread of 3.4; with
/// the joints' side taken from the corner again, 427 pixels, red 204 with a spread of 7.8 (darker
/// layers across the whole face).
#[test]
fn a_far_wall_face_shows_no_darker_layers_in_the_headset() {
    let f = far_face("a_far_wall_face_shows_no_darker_layers_in_the_headset");
    assert!(f.pixels >= 300, "the bracket's wall face fills a patch of the left eye: {f:?}");
    assert!(f.sd_red <= 5.5, "the far wall face is even, no layer darker than the rest (red {:.0}): {f:?}", f.mean_red);
}

/// The app main thread's CPU budget per wait.
const HANG: std::time::Duration = std::time::Duration::from_secs(300);
