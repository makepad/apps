//! What `fab.render` shows of a part made from a distance field: lit across in every view, the
//! side, back and underside ones too, and as an exact part of the same shape shows. Those views
//! once came back a flat dark silhouette: one sun fixed in the world lit them, so all they showed
//! was the flat ambient. And every sheet of it comes back at a size it can be read at.
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

#[path = "common/wait.rs"]
mod idle_wait;
use idle_wait::wait_poll;
use makepad_strict_json::Value;
use makepad_test::{makepad_test, Role, Selector, TestApp};
use std::time::Duration;

fn try_call(app: &TestApp, tool: &str, args: &str) -> Result<String, String> {
    let lines = app.try_call_tool(HANG, &format!("fab.{tool}"), args).map_err(|e| format!("fab.{tool}: {e:?}"))?;
    lines.iter().find(|l| l.contains("\"Result\"")).cloned().ok_or_else(|| format!("fab.{tool} returned no result: {lines:?}"))
}

fn result_of(line: &str) -> Value {
    let v = makepad_strict_json::parse_depth(line.as_bytes(), 32).expect("result line is JSON");
    match v.get("Result").expect("Result") {
        Value::Arr(a) => a[0].clone(),
        other => other.clone(),
    }
}

fn text(line: &str) -> String {
    result_of(line).get("text").and_then(Value::as_str).unwrap_or_default().to_string()
}

/// The reply's pictures, decoded.
fn pictures(line: &str) -> Vec<makepad_model::RgbaImage> {
    let limits = makepad_model::Limits::default();
    result_of(line)
        .get("images")
        .and_then(Value::as_arr)
        .map(|a| a.iter().filter_map(Value::as_str).map(|b| makepad_base64::base64_decode(b.as_bytes()).expect("base64")).map(|png| makepad_model::RgbaImage::from_png(&png, &limits).expect("a PNG")).collect())
        .unwrap_or_default()
}

fn head_of(reply: &str) -> String {
    let reply = makepad_script::syntax::parse_value(reply).expect("a Splash reply");
    reply.field("head").and_then(|f| f.value.string()).expect("reply has a head").to_string()
}

/// One view of a 2 x 2 sheet (row-major): its side and its pixels' RGB, row by row.
fn view(sheet: &makepad_model::RgbaImage, k: usize) -> (usize, Vec<[f32; 3]>) {
    let side = sheet.width as usize / 2;
    let (x0, y0) = (k % 2 * side, k / 2 * side);
    let mut px = Vec::with_capacity(side * side);
    for y in y0..y0 + side {
        for x in x0..x0 + side {
            let i = (y * sheet.width as usize + x) * 4;
            px.push([sheet.pixels[i] as f32, sheet.pixels[i + 1] as f32, sheet.pixels[i + 2] as f32]);
        }
    }
    (side, px)
}

/// The ball's face in a view: its grey pixels (the sky is blue to green, the stage dark), as a
/// disc nine tenths of the size they cover, round their middle.
fn face(side: usize, px: &[[f32; 3]]) -> Vec<usize> {
    let grey = |c: &[f32; 3]| (c[0] - c[1]).abs() < 12.0 && (c[1] - c[2]).abs() < 12.0 && c[0] + c[1] + c[2] > 120.0;
    let ball: Vec<usize> = (0..px.len()).filter(|&i| grey(&px[i])).collect();
    assert!(ball.len() > side * side / 20, "the ball covers {} of {} pixels", ball.len(), side * side);
    let (sx, sy) = ball.iter().fold((0.0, 0.0), |(sx, sy), &i| (sx + (i % side) as f64, sy + (i / side) as f64));
    let (cx, cy) = (sx / ball.len() as f64, sy / ball.len() as f64);
    let r = 0.9 * (ball.len() as f64 / std::f64::consts::PI).sqrt();
    (0..px.len()).filter(|&i| ((i % side) as f64 - cx).powi(2) + ((i / side) as f64 - cy).powi(2) < r * r).collect()
}

fn luminance(c: &[f32; 3]) -> f32 {
    (c[0] + c[1] + c[2]) / 3.0
}

/// The plate's head, once the app's tools answer.
fn tools_ready(app: &TestApp) -> String {
    app.locator(Selector::role(Role::Window)).wait_visible();
    let r = wait_poll(app, || try_call(app, "inspect", "{}").map_err(|e| format!("the tools never appeared: {e}")));
    head_of(&text(&r))
}

#[makepad_test]
fn a_field_part_is_lit_across_every_view_as_an_exact_one_is(app: TestApp) {
    let head = tools_ready(&app);
    let source = r##"use mod.model.*
use mod.fab.*
let m = mod.model
let field = m.sdf({field: fn(p) { return sdf_sphere(p - vec3(0, 15mm, 0), 15mm) } bounds: [[-16mm, -1mm, -16mm] [16mm, 31mm, 16mm]] cell: 0.5mm})
let exact = m.brep.sphere({radius: 15mm at: [0 15mm 0]})
object(field)
object(exact)
"##;
    let batch = format!(r#"{{"request_id":"balls","expected":{head:?},"script":"balls.cad.splash","source":{}}}"#, Value::Str(source.to_string()).to_json());
    let reply = text(&try_call(&app, "script", &batch).expect("script"));
    assert!(reply.contains("balls/field") && reply.contains("balls/exact"), "{reply}");
    let views = ["iso", "right", "back", "underside"];
    let sheet = |object: &str| {
        let r = try_call(&app, "render", &format!(r#"{{"object":"balls/{object}","views":["iso","right","back","underside"]}}"#)).expect("render");
        let mut sheets = pictures(&r);
        assert_eq!(sheets.len(), 1, "one sheet of four views: {}", text(&r));
        sheets.remove(0)
    };
    // Each sheet as large as the answer's picture budget lets it be: the two may differ a little.
    let (field, exact) = (sheet("field"), sheet("exact"));
    if let Some(dir) = std::env::var_os("DEVHUB_KEEP") {
        for (name, sheet) in [("balls-field.png", &field), ("balls-exact.png", &exact)] {
            std::fs::write(std::path::Path::new(&dir).join(name), sheet.to_png(&makepad_model::Limits::default()).expect("png")).expect("keep the sheet");
        }
    }
    for (k, name) in views.iter().enumerate() {
        let (side, f) = view(&field, k);
        let (side_e, e) = view(&exact, k);
        // the exact view's pixel at the same place in the view
        let at = |i: usize| (i / side * side_e / side) * side_e + i % side * side_e / side;
        let disc = face(side, &f);
        // shaded across: no one value holds much of its face, and its values spread wide
        let mut lum: Vec<f32> = disc.iter().map(|&i| luminance(&f[i])).collect();
        lum.sort_by(f32::total_cmp);
        let mut counts = std::collections::HashMap::new();
        for l in &lum {
            *counts.entry(l.round() as i32).or_insert(0usize) += 1;
        }
        let (flat, most) = counts.iter().max_by_key(|(_, n)| **n).map(|(v, n)| (*v, *n)).unwrap();
        let share = most as f32 / lum.len() as f32;
        let spread = lum[lum.len() * 95 / 100] - lum[lum.len() * 5 / 100];
        println!("{name}: face {} px, top value {flat} holds {:.1}%, spread {spread:.0}", disc.len(), share * 100.0);
        assert!(share <= 0.10, "{name}: {:.0}% of the field ball's face is one value ({flat}/255)", share * 100.0);
        assert!(spread >= 60.0, "{name}: the field ball's face spans only {spread:.0}/255 (5th to 95th percentile)");
        // as the exact ball of the same shape shows
        let diff = disc.iter().map(|&i| (0..3).map(|c| (f[i][c] - e[at(i)][c]).abs()).sum::<f32>() / 3.0).sum::<f32>() / disc.len() as f32;
        println!("{name}: field and exact differ by {diff:.2}/255 on average");
        assert!(diff <= 4.0, "{name}: the field ball differs from the exact one by {diff:.1}/255 on average");
    }
}

/// The default render's two sheets share the answer's picture budget: both come back alike, with
/// their views at least 320 pixels square (of 512 rendered). The first once took the budget and
/// left the second a quarter of its size, or an eighth.
#[makepad_test]
fn a_field_parts_sheets_all_come_back_at_a_readable_size(app: TestApp) {
    let head = tools_ready(&app);
    let source = r##"use mod.model.*
use mod.fab.*
let m = mod.model
let field = m.sdf({field: fn(p) { return smooth_union(sdf_sphere(p - vec3(0, 12mm, 0), 12mm), sdf_capsule(p, vec3(0, 20mm, 0), vec3(9mm, 34mm, 4mm), 4mm), 3mm) } bounds: [[-13mm, -1mm, -13mm] [14mm, 39mm, 13mm]] cell: 0.5mm})
object(field)
"##;
    let batch = format!(r#"{{"request_id":"sheets","expected":{head:?},"script":"sheets.cad.splash","source":{}}}"#, Value::Str(source.to_string()).to_json());
    let reply = text(&try_call(&app, "script", &batch).expect("script"));
    assert!(reply.contains("sheets/field"), "{reply}");
    let r = try_call(&app, "render", r#"{"object":"sheets/field"}"#).expect("render");
    let sheets = pictures(&r);
    assert_eq!(sheets.len(), 2, "two sheets of four views: {}", text(&r));
    let sides: Vec<u32> = sheets.iter().map(|s| s.width / 2).collect();
    println!("views {sides:?} pixels square");
    for sheet in &sheets {
        assert_eq!(sheet.width, sheet.height, "a sheet of 2 x 2 square views");
    }
    assert_eq!(sides[0], sides[1], "the sheets came back at unequal sizes");
    assert!(sides[0] >= 320, "the sheets came back with their views {} pixels square", sides[0]);
}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);
