//! `fab.render`'s camera: a focus frames every view on a region of the plate, fewer views come
//! back larger, within the answer's picture budget, each sheet saying its views' size, and an
//! orthographic view draws all at one scale, with a mm grid behind.
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

#[path = "common/wait.rs"]
mod idle_wait;
use idle_wait::wait_poll;
use makepad_ai_services::wire::MAX_RESULT_IMAGE_BYTES;
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

/// The reply's pictures: their base64 bytes, and decoded.
fn pictures(line: &str) -> (usize, Vec<makepad_model::RgbaImage>) {
    let limits = makepad_model::Limits::default();
    let data: Vec<String> = result_of(line).get("images").and_then(Value::as_arr).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
    let bytes = data.iter().map(String::len).sum();
    (bytes, data.iter().map(|b| makepad_model::RgbaImage::from_png(&makepad_base64::base64_decode(b.as_bytes()).expect("base64"), &limits).expect("a PNG")).collect())
}

fn head_of(reply: &str) -> String {
    let reply = makepad_script::syntax::parse_value(reply).unwrap_or_else(|e| panic!("not a Splash reply ({e:?}): {reply}"));
    reply.field("head").and_then(|f| f.value.string()).expect("reply has a head").to_string()
}

/// The plate's head, once the app's tools answer.
fn tools_ready(app: &TestApp) -> String {
    app.locator(Selector::role(Role::Window)).wait_visible();
    let r = wait_poll(app, || try_call(app, "inspect", "{}").map_err(|e| format!("the tools never appeared: {e}")));
    head_of(&text(&r))
}

/// Puts `source` on the plate as `file`; the reply's text.
fn script(app: &TestApp, head: &str, file: &str, source: &str) -> String {
    let batch = format!(r#"{{"request_id":"{file}","expected":{head:?},"script":{file:?},"source":{}}}"#, Value::Str(source.to_string()).to_json());
    text(&try_call(app, "script", &batch).expect("script"))
}

/// Where a part's model origin sits on the bed (plate mm), from the plate the reply lists.
fn placed_at(reply: &str, solid: &str) -> [f64; 2] {
    let from = reply.find(&format!("solid: \"{solid}\"")).unwrap_or_else(|| panic!("{solid} is not on the plate: {reply}"));
    let at = &reply[from..][reply[from..].find("at: [").expect("the part's place") + 5..];
    let mut mm = at[..at.find(']').unwrap()].split(',').map(|v| v.trim().trim_end_matches("mm").parse::<f64>().expect("mm"));
    [mm.next().unwrap(), mm.next().unwrap()]
}

/// How many rows of a view are mostly grey (the part; the sky is blue to green, the stage dark),
/// over its height.
fn grey_rows(sheet: &makepad_model::RgbaImage) -> f32 {
    let (w, h) = (sheet.width as usize, sheet.height as usize);
    let grey = |i: usize| {
        let c = &sheet.pixels[i * 4..i * 4 + 3];
        let (r, g, b) = (c[0] as i32, c[1] as i32, c[2] as i32);
        (r - g).abs() < 12 && (g - b).abs() < 12 && r + g + b > 120
    };
    (0..h).filter(|y| (0..w).filter(|x| grey(y * w + x)).count() * 4 > w).count() as f32 / h as f32
}

/// A bar 100 mm long and 10 mm square: from the front, the whole of it is a thin band across the
/// view; focused on a box round its last 10 mm, the band fills most of the view's height.
#[makepad_test]
fn a_focus_box_frames_its_region(app: TestApp) {
    let head = tools_ready(&app);
    let reply = script(&app, &head, "cam.cad.splash", "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet bar = m.sdf({bounds: [[-51mm, -1mm, -6mm], [51mm, 11mm, 6mm]] cell: 0.5mm field: fn(p) { return sdf_box(p - vec3(0, 5mm, 0), vec3(100mm, 10mm, 10mm)) }})\nobject(bar)\n");
    let [x, y] = placed_at(&reply, "cam/bar");
    let render = |args: String| {
        let r = try_call(&app, "render", &args).expect("render");
        let (_, sheets) = pictures(&r);
        assert_eq!(sheets.len(), 1, "one view: {}", text(&r));
        grey_rows(&sheets[0])
    };
    let whole = render(r#"{"views":["front"]}"#.to_string());
    let end = render(format!(r#"{{"views":["front"],"focus":[[{}, {}, 0], [{}, {}, 10]]}}"#, x + 40.0, y - 5.0, x + 50.0, y + 5.0));
    let ball = render(format!(r#"{{"views":["front"],"focus":{{"at":[{}, {y}, 5],"radius":5}}}}"#, x + 45.0));
    println!("the bar's band: {:.0}% of the view's height whole, {:.0}% focused on its end, {:.0}% on a ball round it", whole * 100.0, end * 100.0, ball * 100.0);
    assert!(whole < 0.2, "the whole bar is a thin band: {whole}");
    assert!(end > 0.6, "focused on its end, the bar fills most of the view: {end}");
    assert!(ball > 0.45, "focused on a ball round its end, the bar fills much of the view: {ball}");
}

/// One view comes back larger than two or four, and two no smaller than four (a sheet of two
/// across is as wide as the chat takes a picture: two and four come back alike), all within the
/// picture budget, each sheet's label saying its views' size.
#[makepad_test]
fn fewer_views_come_back_larger(app: TestApp) {
    let head = tools_ready(&app);
    script(&app, &head, "ball.cad.splash", "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet ball = m.sdf({bounds: [[-16mm, -1mm, -16mm], [16mm, 31mm, 16mm]] cell: 0.5mm field: fn(p) { return sdf_sphere(p - vec3(0, 15mm, 0), 15mm) }})\nobject(ball)\n");
    let side = |views: &[&str], across: u32| {
        let views: Vec<String> = views.iter().map(|v| format!("{v:?}")).collect();
        let r = try_call(&app, "render", &format!(r#"{{"object":"ball/ball","views":[{}]}}"#, views.join(","))).expect("render");
        let (bytes, sheets) = pictures(&r);
        assert_eq!(sheets.len(), 1, "{}", text(&r));
        assert!(bytes <= MAX_RESULT_IMAGE_BYTES, "{bytes} bytes of pictures");
        let side = sheets[0].width / across;
        assert!(text(&r).contains(&format!("each view {side} x {side} px")), "the label says the views' size: {}", text(&r));
        side
    };
    let (one, two, four) = (side(&["iso"], 1), side(&["iso", "back"], 2), side(&["iso", "right", "back", "underside"], 2));
    println!("views {one}, {two} and {four} px square");
    assert!(one as f32 >= 1.8 * four as f32 && one > two && two >= four, "one view {one}, two {two}, four {four} px square");
}

/// The grey runs along a row of a picture (the parts: the sky is blue to green), as (start, length).
fn grey_runs(image: &makepad_model::RgbaImage, row: usize) -> Vec<(usize, usize)> {
    let w = image.width as usize;
    let grey = |x: usize| {
        let c = &image.pixels[(row * w + x) * 4..(row * w + x) * 4 + 3];
        let (r, g, b) = (c[0] as i32, c[1] as i32, c[2] as i32);
        (r - g).abs() < 12 && (g - b).abs() < 12 && r + g + b > 120
    };
    let mut runs = Vec::new();
    let mut x = 0;
    while x < w {
        if grey(x) {
            let start = x;
            while x < w && grey(x) {
                x += 1;
            }
            runs.push((start, x - start));
        }
        x += 1;
    }
    runs
}

/// Where a plate point (mm) shows in a view of the answer, in the pixels of the picture as it
/// came back, by the camera the answer tells for the view (`cameras`, in the views' order).
fn told_pixel(answer: &str, view: usize, p: [f64; 3]) -> (f64, f64) {
    let answer = makepad_script::syntax::parse_value(answer).expect("a Splash answer");
    let cameras = answer.field("cameras").and_then(|f| f.value.items()).unwrap_or_else(|| panic!("the answer tells its cameras: {answer:?}"));
    let told = &cameras[view];
    let num = |k: &str| told.field(k).and_then(|f| f.value.number());
    let v3 = |k: &str| -> [f64; 3] {
        let items = told.field(k).and_then(|f| f.value.items()).unwrap_or_else(|| panic!("the camera has no '{k}'"));
        std::array::from_fn(|a| items[a].number().unwrap())
    };
    let d: Vec<f64> = (0..3).map(|a| p[a] - v3("eye")[a]).collect();
    let dot = |v: [f64; 3]| d[0] * v[0] + d[1] * v[1] + d[2] * v[2];
    let (x, y) = match num("across") {
        Some(across) => (dot(v3("right")) / (across / 2.0), dot(v3("up")) / (across / 2.0)),
        None => {
            let t = (num("fov").expect("a fov or an across").to_radians() / 2.0).tan() * dot(v3("forward"));
            (dot(v3("right")) / t, dot(v3("up")) / t)
        }
    };
    let px = num("px").expect("the view's px");
    ((x + 1.0) / 2.0 * px, (1.0 - y) / 2.0 * px)
}

/// From the front, orthographic: two 20 mm cubes of one field part, one 100 mm behind the other,
/// and an exact 20 mm cube come out the same width, 20 mm by the view's stated width; the grid's
/// lines cross the sky at the stated spacing and none crosses a part. The camera the answer tells
/// finds the exact cube's middle where the picture shows it, orthographic and in perspective.
#[makepad_test]
fn an_orthographic_view_draws_all_at_one_scale_on_a_mm_grid(app: TestApp) {
    let head = tools_ready(&app);
    let source = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet pair = m.sdf({bounds: [[-41mm, -1mm, -11mm], [41mm, 21mm, 111mm]] cell: 0.5mm field: fn(p) { return min(sdf_box(p - vec3(-30mm, 10mm, 0), vec3(20mm, 20mm, 20mm)), sdf_box(p - vec3(30mm, 10mm, 100mm), vec3(20mm, 20mm, 20mm))) }})\nlet cube = m.box({size: [20mm 20mm 20mm] at: [0 10mm 0]})\nobject(pair)\nobject(cube)\n";
    let head = head_of(&script(&app, &head, "ortho.cad.splash", source));
    let edits = "place(\"ortho\", object: \"pair\", at: [120mm 160mm])\nplace(\"ortho\", object: \"cube\", at: [230mm 160mm])";
    let batch = format!(r#"{{"request_id":"ortho-place","expected":{head:?},"edits":{}}}"#, Value::Str(edits.to_string()).to_json());
    let placed = text(&try_call(&app, "place", &batch).expect("place"));
    assert!(!placed.trim_start().starts_with("{error"), "{placed}");
    let r = try_call(&app, "render", r#"{"views":["front"],"ortho":true}"#).expect("render");
    let label = text(&r);
    let (_, sheets) = pictures(&r);
    assert_eq!(sheets.len(), 1, "one view: {label}");
    let image = &sheets[0];
    let [cx, cy] = placed_at(&placed, "ortho/cube");
    // the cube's middle, in a picture of the answer: grey, as the cube is
    let shows_cube = |answer: &str, image: &makepad_model::RgbaImage| {
        let (x, y) = told_pixel(answer, 0, [cx, cy, 10.0]);
        let i = (y as usize * image.width as usize + x as usize) * 4;
        let c = &image.pixels[i..i + 3];
        println!("the cube's middle shows at ({x:.1}, {y:.1}) of {} px: {c:?}", image.width);
        assert!((c[0] as i32 - c[1] as i32).abs() < 12 && (c[1] as i32 - c[2] as i32).abs() < 12 && c[0] > 40, "the told camera finds the cube's middle at ({x:.1}, {y:.1}), where the picture shows {c:?}");
    };
    shows_cube(&label, image);
    let perspective = try_call(&app, "render", r#"{"views":["iso"]}"#).expect("render");
    shows_cube(&text(&perspective), &pictures(&perspective).1[0]);
    if let Some(dir) = std::env::var_os("DEVHUB_KEEP") {
        std::fs::write(std::path::Path::new(&dir).join("ortho-front.png"), image.to_png(&makepad_model::Limits::default()).expect("png")).expect("keep the picture");
    }
    let number_after = |words: &str| -> f64 {
        let at = label.find(words).unwrap_or_else(|| panic!("the label says '{words}': {label}")) + words.len();
        label[at..].split(' ').next().unwrap().parse().unwrap_or_else(|_| panic!("a number after '{words}': {label}"))
    };
    let (across, grid) = (number_after("orthographic, every view "), number_after("grid lines every "));
    let side = image.width as usize;
    let mm_px = across / side as f64;
    // the cubes' middle row: three grey runs, the near and far field cubes and the exact one
    let rows: Vec<usize> = (0..image.height as usize).filter(|&y| grey_runs(image, y).len() == 3).collect();
    assert!(!rows.is_empty(), "no row crosses three cubes: {label}");
    let row = rows[rows.len() / 2];
    let widths: Vec<f64> = grey_runs(image, row).iter().map(|&(_, n)| n as f64 * mm_px).collect();
    println!("{side} px, {across} mm across, grid {grid} mm: the cubes are {widths:.2?} mm wide");
    for w in &widths {
        assert!((w - 20.0).abs() <= 2.0 * mm_px + 0.05, "a 20 mm cube is {w:.2} mm wide in the view: {widths:?}");
    }
    assert!((widths[0] - widths[1]).abs() <= 1.5 * mm_px, "the far cube is drawn as wide as the near one: {widths:?}");
    // no grid line across a part: each cube's run is one shade
    let lum = |x: usize, y: usize| { let i = (y * side + x) * 4; (image.pixels[i] as f64 + image.pixels[i + 1] as f64 + image.pixels[i + 2] as f64) / 3.0 };
    for &(start, n) in &grey_runs(image, row) {
        let values: Vec<f64> = (start + 2..start + n - 2).map(|x| lum(x, row)).collect();
        let spread = values.iter().cloned().fold(f64::MIN, f64::max) - values.iter().cloned().fold(f64::MAX, f64::min);
        assert!(spread < 8.0, "a cube's face is one shade, no grid line on it: {spread:.1} levels");
    }
    // the sky's row near the top: a line every grid spacing, lighter than the sky beside it (the
    // picture is shrunk to fit the answer, so a line's two pixels may share three)
    let top = side / 20;
    let sky: Vec<f64> = (0..side).map(|x| lum(x, top)).collect();
    let lines = (3..side - 3).filter(|&x| sky[x] >= sky[x - 1] && sky[x] > sky[x + 1] && sky[x] - (sky[x - 3] + sky[x + 3]) / 2.0 > 5.0).count();
    let expected = across / grid;
    println!("{lines} grid lines across the sky, {expected:.1} expected");
    assert!((lines as f64 - expected).abs() <= 2.0, "{lines} grid lines across the view, {expected:.1} expected");
}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);
