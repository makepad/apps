//! A part's eye is an icon button named "Hide part" / "Show part" (never a text label).
//! Switching it off keeps the part out of the print and draws it see-through in the plate
//! view: its own colour all but gone, a faint ghost left where it stands. Switching it on
//! brings the part back, and each toggle is one undo step.
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

#[path = "common/picture.rs"]
mod picture;
#[path = "common/wait.rs"]
mod idle_wait;
use idle_wait::wait_poll;
use makepad_strict_json::Value;
use makepad_test::{makepad_test, Role, Selector, TestApp};
use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
use makepad_zune_png::PngDecoder;
use std::time::Duration;

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

/// What a grab of the viewport shows of the part.
struct Look {
    /// The part's own colour: a strong orange.
    solid: usize,
    /// A ghost: the part's hue at a quarter over the grey bed (red about 97 in sRGB, red over blue
    /// about 42, green a third of the way from blue to red). A half share reads red about 128.
    ghost: usize,
    /// The part's hue at any share below its solid colour; at a quarter nearly all of it is ghost.
    tinted: usize,
}

impl Look {
    /// The part is see-through at the eye-off quarter: a ghost, and next to none of the part's
    /// hue at any other share.
    fn ghosted(&self) -> bool {
        self.ghost > 100 && self.ghost * 10 >= self.tinted * 8
    }
}

fn look(app: &TestApp) -> Look {
    let path = picture::finished_screenshot(app);
    let bytes = std::fs::read(&path).expect("the grab");
    let mut dec = PngDecoder::new(ZCursor::new(&bytes));
    let px = dec.decode_raw().expect("png");
    let (w, h) = dec.dimensions().expect("dims");
    let n = dec.colorspace().expect("colorspace").num_components();
    let window = app.locator(Selector::role(Role::Window)).snapshot();
    let view = app.locator(Selector::id("viewport")).wait_visible().snapshot();
    let (sx, sy) = (w as f64 / window.width() as f64, h as f64 / window.height() as f64);
    let x0 = (((view.x() - window.x()) as f64 * sx).floor().max(0.0) as usize).min(w);
    let y0 = (((view.y() - window.y()) as f64 * sy).floor().max(0.0) as usize).min(h);
    let x1 = (((view.x() + view.width() - window.x()) as f64 * sx).ceil().max(0.0) as usize).min(w);
    let y1 = (((view.y() + view.height() - window.y()) as f64 * sy).ceil().max(0.0) as usize).min(h);
    assert!(x1 > x0 && y1 > y0, "missing viewport pixels; see {}", path.display());
    // below the view cube and the axes in the corner
    let y0 = (y0 + (150.0 * sy) as usize).min(y1);
    let mut out = Look { solid: 0, ghost: 0, tinted: 0 };
    for y in y0..y1 {
        for x in x0..x1 {
            let o = (y * w + x) * n;
            let (r, g, b) = (px[o] as i32, px[o + 1] as i32, px[o + 2] as i32);
            out.solid += (r > 150 && r - g > 50 && r - b > 50) as usize;
            out.tinted += (r > 85 && r <= 150 && r - b > 20 && r - g > 20) as usize;
            out.ghost += ((86..=115).contains(&r) && (28..=56).contains(&(r - b)) && (21..=38).contains(&(r - g)) && 5 * (g - b) >= r - b && 2 * (g - b) <= r - b) as usize;
        }
    }
    out
}

/// Looks until `ok` holds of one (the camera glides and the frame after an edit lands a few frames later).
fn look_until(app: &TestApp, what: &str, ok: impl Fn(&Look) -> bool) -> Look {
    wait_poll(app, || {
        let l = look(app);
        if ok(&l) {
            return Ok(l);
        }
        Err(format!("{what}: solid {} ghost {} tinted {}", l.solid, l.ghost, l.tinted))
    })
}

fn eyes(app: &TestApp, name: &str) -> usize {
    app.widget_snapshot().nodes().into_iter().filter(|w| w.visible() && w.node.role == Role::Button && w.node.name == name).count()
}

fn wait_until(app: &TestApp, what: &str, mut ok: impl FnMut() -> bool) {
    wait_poll(app, || {
        if !ok() { Err(format!("timed out waiting for {what}")) } else { Ok(()) }
    });
}

/// `body` placed as the orange "Block", its row and its eye up.
fn place_block(app: &TestApp, body: &str) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    let line = wait_poll(&app, || {
        call(&app, "inspect", "{}").map_err(|e| format!("the fab tools never appeared: {e}"))
    });
    let head = head_of(&text(&line));
    let source = format!("use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet block = {body}\nobject(block)\n");
    let batch = format!(r#"{{"request_id":"eye-1","expected":{head:?},"script":"block.cad.splash","source":{}}}"#, Value::Str(source).to_json());
    let head = head_of(&text(&call(app, "script", &batch).expect("script")));
    let edits = r#"color("Orange", rgb: #xff6a10)
place("block", name: "Block", color: "Orange")"#;
    let batch = format!(r#"{{"request_id":"eye-1-place","expected":{head:?},"edits":{}}}"#, Value::Str(edits.to_string()).to_json());
    call(app, "place", &batch).expect("place");
    app.locator(Selector::role(Role::Tab).name("Parts")).click();
    app.locator(Selector::role(Role::TreeItem).name("Block")).wait_visible();
}

/// The part as it draws once the view has stopped: the camera glides to frame a part just placed
/// (a finished picture is one at rest), and every later look is measured against this one.
fn shown_at_rest(app: &TestApp) -> Look {
    look_until(app, "the part draws in its colour at rest", |l| l.solid > 100)
}

#[makepad_test]
fn the_eye_is_an_icon_that_ghosts_the_part_and_undoes(app: TestApp) {
    place_block(&app, "m.box({size: [60mm 40mm 40mm]})");

    // an icon button with an accessible name, no text label
    wait_until(&app, "the eye", || eyes(&app, "Hide part") == 1);
    assert_eq!(eyes(&app, "Show part"), 0);
    assert_eq!(app.widget_snapshot().nodes().into_iter().filter(|w| w.node.name.starts_with("Eye ")).count(), 0, "the eye has no text label");
    let shown = shown_at_rest(&app);

    // off: out of the print, see-through in the view
    app.locator(Selector::role(Role::Button).name("Hide part")).wait_visible().press();
    wait_until(&app, "the closed eye", || eyes(&app, "Show part") == 1 && eyes(&app, "Hide part") == 0);
    let ghosted = look_until(&app, "the part's colour is gone and a ghost left", |l| l.solid * 20 < shown.solid && l.ghosted());
    assert!(ghosted.ghosted());
    // the title counts it, and says it is hidden
    let text = |id: &str| app.widget_snapshot().nodes().into_iter().find(|n| n.visible() && n.node.path.rsplit('.').next() == Some(id)).map(|n| n.node.name).unwrap_or_default();
    wait_until(&app, "the counts to say the part is hidden", || text("doc_state").starts_with("1 part (1 hidden) · "));

    // one undo step brings it back, one redo ghosts it again
    app.locator(Selector::id("undo").nth(0)).press();
    wait_until(&app, "the undo", || eyes(&app, "Hide part") == 1);
    look_until(&app, "the part is back", |l| l.solid * 10 > shown.solid * 7);
    app.locator(Selector::id("redo").nth(0)).press();
    wait_until(&app, "the redo", || eyes(&app, "Show part") == 1);

    // on again
    app.locator(Selector::role(Role::Button).name("Show part")).wait_visible().press();
    wait_until(&app, "the open eye", || eyes(&app, "Hide part") == 1);
    look_until(&app, "the part shows again", |l| l.solid * 10 > shown.solid * 7);
}

/// A part drawn from its field has no mesh to fall back on: its ghost is the field, and the
/// volume it baked comes back with the eye.
#[makepad_test]
fn the_eye_ghosts_a_field_part_as_its_field(app: TestApp) {
    place_block(&app, "m.blend([m.box({size: [60mm 40mm 40mm]}) m.sphere({radius: 24mm})], 1mm)");
    wait_until(&app, "the eye", || eyes(&app, "Hide part") == 1);
    let shown = shown_at_rest(&app);

    app.locator(Selector::role(Role::Button).name("Hide part")).wait_visible().press();
    wait_until(&app, "the closed eye", || eyes(&app, "Show part") == 1);
    look_until(&app, "the part's colour is gone and a ghost left", |l| l.solid * 20 < shown.solid && l.ghosted());

    app.locator(Selector::role(Role::Button).name("Show part")).wait_visible().press();
    wait_until(&app, "the open eye", || eyes(&app, "Hide part") == 1);
    look_until(&app, "the part shows again", |l| l.solid * 10 > shown.solid * 7);
}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);
