//! `fab.render` close up on a part made from a distance field: the view traces the field from its
//! own camera, as the plate view does, so a smooth face shades smoothly however near the camera
//! comes and a crease is the field's own line, with the part's paint on it. Drawn from a mesh at
//! the field's cell instead, a close-up showed every cell as a facet (a pebbled face) and a hard
//! union's crease as a cell-sized zigzag. `fab.variants` draws each variant's field alike, in the
//! variant's own paint.
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

/// The reply's one picture, decoded.
fn picture(line: &str) -> makepad_model::RgbaImage {
    let limits = makepad_model::Limits::default();
    let images: Vec<makepad_model::RgbaImage> = result_of(line)
        .get("images")
        .and_then(Value::as_arr)
        .map(|a| a.iter().filter_map(Value::as_str).map(|b| makepad_model::RgbaImage::from_png(&makepad_base64::base64_decode(b.as_bytes()).expect("base64"), &limits).expect("a PNG")).collect())
        .unwrap_or_default();
    assert_eq!(images.len(), 1, "one view: {}", text(line));
    images.into_iter().next().unwrap()
}

fn head_of(reply: &str) -> String {
    let reply = makepad_script::syntax::parse_value(reply).expect("a Splash reply");
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

/// Two balls 30 mm across, their middles 24 mm apart along x, in one field joined hard: their
/// crease is a circle in the plane x = 0, which a camera in that plane sees as a straight line.
/// The left ball is painted red over a small cap up and to the front of it.
const PAIR: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet red = Material{albedo: [0.8 0.05 0.05]}\nlet pair = m.sdf({bounds: [[-27mm, 0, -15mm], [27mm, 30mm, 15mm]] cell: 0.5mm field: fn(p) { return sdf_union(sdf_sphere(p - vec3(-12mm, 15mm, 0), 15mm), sdf_sphere(p - vec3(12mm, 15mm, 0), 15mm)) }})\nobject(pair)\nm.fill(m.sphere({radius: 3mm at: [-6mm 21mm 12.3mm]}), red)\n";

/// A view's pixel's light: the mean of its channels.
fn light(image: &makepad_model::RgbaImage, x: usize, y: usize) -> f64 {
    let i = (y * image.width as usize + x) * 4;
    image.pixels[i..i + 3].iter().map(|&c| c as f64).sum::<f64>() / 3.0
}

fn is_red(image: &makepad_model::RgbaImage, x: usize, y: usize) -> bool {
    let i = (y * image.width as usize + x) * 4;
    let c = &image.pixels[i..i + 3];
    c[0] as i32 > 2 * c[1] as i32 + 20 && c[0] as i32 > 2 * c[2] as i32 + 20
}

/// What a quadric (in x and y) fitted to a window's light leaves, root mean square: a smooth face's
/// shading is near enough a quadric over a window this size, and what is left is the pixels' own
/// rounding; a face of facets leaves their pattern.
fn left_by_quadric(image: &makepad_model::RgbaImage, xs: std::ops::Range<usize>, ys: std::ops::Range<usize>) -> f64 {
    let (cx, cy, half) = ((xs.start + xs.end) as f64 / 2.0, (ys.start + ys.end) as f64 / 2.0, (xs.end - xs.start) as f64 / 2.0);
    let terms = |x: usize, y: usize| {
        let (u, v) = ((x as f64 - cx) / half, (y as f64 - cy) / half);
        [1.0, u, v, u * u, u * v, v * v]
    };
    let mut a = [[0.0f64; 7]; 6];
    for y in ys.clone() {
        for x in xs.clone() {
            let t = terms(x, y);
            let l = light(image, x, y);
            for r in 0..6 {
                for c in 0..6 {
                    a[r][c] += t[r] * t[c];
                }
                a[r][6] += t[r] * l;
            }
        }
    }
    // Gauss-Jordan on the normal equations
    for k in 0..6 {
        let p = (k..6).max_by(|&i, &j| a[i][k].abs().total_cmp(&a[j][k].abs())).unwrap();
        a.swap(k, p);
        for r in 0..6 {
            if r != k {
                let f = a[r][k] / a[k][k];
                for c in k..7 {
                    a[r][c] -= f * a[k][c];
                }
            }
        }
    }
    let w: Vec<f64> = (0..6).map(|k| a[k][6] / a[k][k]).collect();
    let (mut sum, mut n) = (0.0, 0.0);
    for y in ys {
        for x in xs.clone() {
            let fit: f64 = terms(x, y).iter().zip(&w).map(|(t, w)| t * w).sum();
            sum += (light(image, x, y) - fit).powi(2);
            n += 1.0;
        }
    }
    (sum / n).sqrt()
}

/// Close up on the pair's crease from the front, the camera in the crease's plane: the crease is
/// one straight line down the middle of the view (where the light jumps from the left ball's face
/// to the right one's), the right ball's face shades as smoothly as its pixels allow, and the left
/// ball's paint shows where it was put and nowhere else.
#[makepad_test]
fn a_close_up_of_a_field_crease_is_smooth_straight_and_painted(app: TestApp) {
    let head = tools_ready(&app);
    let reply = script(&app, &head, "pair.cad.splash", PAIR);
    let [x, y] = placed_at(&reply, "pair/pair");
    let r = try_call(&app, "render", &format!(r#"{{"views":["front"],"focus":{{"at":[{x}, {y}, 15],"radius":16}}}}"#)).expect("render");
    let image = picture(&r);
    let side = image.width as usize;
    assert_eq!(image.height as usize, side);
    // a run that keeps its outputs keeps the picture
    if let Some(dir) = std::env::var_os("DEVHUB_KEEP") {
        std::fs::write(std::path::Path::new(&dir).join("pair-front.png"), image.to_png(&makepad_model::Limits::default()).expect("png")).expect("keep the picture");
    }
    // The crease: on each row of the middle, the column where the light changes most, within a
    // twentieth of the view of the middle column (the paint's edge is farther out).
    let (from, to) = (side * 3 / 10, side * 7 / 10);
    let near = side / 20;
    let seam: Vec<(f64, f64)> = (from..to)
        .map(|row| {
            let col = (side / 2 - near..side / 2 + near).max_by(|&a, &b| (light(&image, a + 1, row) - light(&image, a - 1, row)).abs().total_cmp(&(light(&image, b + 1, row) - light(&image, b - 1, row)).abs())).unwrap();
            (row as f64, col as f64)
        })
        .collect();
    // the line through them, and how far they stray from it
    let n = seam.len() as f64;
    let (mr, mc) = (seam.iter().map(|s| s.0).sum::<f64>() / n, seam.iter().map(|s| s.1).sum::<f64>() / n);
    let slope = seam.iter().map(|s| (s.0 - mr) * (s.1 - mc)).sum::<f64>() / seam.iter().map(|s| (s.0 - mr).powi(2)).sum::<f64>();
    let stray: Vec<f64> = seam.iter().map(|s| (s.1 - (mc + slope * (s.0 - mr))).abs()).collect();
    let rms = (stray.iter().map(|d| d * d).sum::<f64>() / n).sqrt();
    let most = stray.iter().copied().fold(0.0, f64::max);
    // A smooth patch of the right ball's face, clear of the crease and below its highlight (a
    // highlight's peak is no quadric).
    let (w0, w1) = (side * 62 / 100, side * 80 / 100);
    let (h0, h1) = (side * 64 / 100, side * 78 / 100);
    let noise = left_by_quadric(&image, w0..w1, h0..h1);
    // The paint: up and to the left of the middle, on the left ball, and nowhere on the right.
    let red = |xs: std::ops::Range<usize>, ys: std::ops::Range<usize>| ys.flat_map(|y| xs.clone().map(move |x| (x, y))).filter(|&(x, y)| is_red(&image, x, y)).count();
    let (upper_left, right) = (red(0..side / 2, 0..side / 2), red(side / 2..side, 0..side));
    println!("{side} px: crease at column {mc:.1} (the middle {}), {rms:.2} px rms off its line, {most:.1} px at most; the face's shading {noise:.2} levels rms off smooth; red {upper_left} px up and left, {right} px on the right", side / 2);
    assert!((mc - side as f64 / 2.0).abs() < side as f64 * 0.015, "the crease runs down the middle: column {mc:.1} of {side}");
    assert!(rms < 0.75 && most < 2.5, "the crease is straight: {rms:.2} px rms off its line, {most:.1} px at most");
    assert!(noise < 0.8, "the face shades smoothly: {noise:.2} levels rms off a smooth fit");
    assert!(upper_left > side * side / 400 && right == 0, "the paint shows where it was put: {upper_left} px up and left, {right} on the right");
}

/// A field of view under 20 degrees is the one drawn: from the same distance, 10 degrees shows
/// the pair twice as wide as 20 does.
#[makepad_test]
fn a_narrow_field_of_view_is_drawn_as_asked(app: TestApp) {
    let head = tools_ready(&app);
    let reply = script(&app, &head, "pair.cad.splash", PAIR);
    let [x, y] = placed_at(&reply, "pair/pair");
    let width = |fov: u32| {
        let r = try_call(&app, "render", &format!(r#"{{"views":["front"],"focus":{{"at":[{x}, {y}, 15],"radius":16}},"distance":400,"fov":{fov}}}"#)).expect("render");
        let image = picture(&r);
        let (side, row) = (image.width as usize, image.height as usize / 2);
        let part = |x: usize| {
            let i = (row * side + x) * 4;
            let c = &image.pixels[i..i + 3];
            let (r, g, b) = (c[0] as i32, c[1] as i32, c[2] as i32);
            (r - g).abs() < 12 && (g - b).abs() < 12 && r + g + b > 120
        };
        (0..side).filter(|&x| part(x)).count() as f64 / side as f64
    };
    let (wide, narrow) = (width(20), width(10));
    println!("the pair spans {:.0}% of the view at 20 degrees, {:.0}% at 10", wide * 100.0, narrow * 100.0);
    assert!((narrow / wide - 2.0).abs() < 0.12, "10 degrees shows it {:.2} times as wide as 20", narrow / wide);
    // A misspelt argument is refused by its name, never drawn as if it were not there.
    let misspelt = text(&try_call(&app, "render", r#"{"views":["front"],"fvo":10}"#).expect("render"));
    assert!(misspelt.contains("error") && misspelt.contains("'fvo'") && misspelt.contains("fov"), "{misspelt}");
}

/// A peanut of two balls `r` across blended into one field, sitting on the bed, painted red on
/// its front where a ball 6 mm across stands at its first ball's face.
const PEANUT: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet red = Material{albedo: [0.8 0.05 0.05]}\nlet blue = Material{albedo: [0.05 0.05 0.8]}\nlet r = param(10mm, min: 6mm, max: 20mm)\nlet peanut = m.blend([m.sphere({radius: r at: [0 r 0]}) m.sphere({radius: r at: [r r 0]})], 2mm)\nobject(peanut)\nm.fill(m.sphere({radius: 6mm at: [0 r r]}), red)\n";

fn is_blue(image: &makepad_model::RgbaImage, x: usize, y: usize) -> bool {
    let i = (y * image.width as usize + x) * 4;
    let c = &image.pixels[i..i + 3];
    c[2] as i32 > 2 * c[0] as i32 + 20 && c[2] as i32 > 2 * c[1] as i32 + 20
}

/// Two variants of a painted field part from the front, one smaller and red as painted, one
/// larger with its paint turned blue: each cell shows its own variant, its paint where the
/// variant puts it, on the variant's own shape, never the plate's paint. The plate as it is
/// (between the two in size, red) is painted on neither.
#[makepad_test]
fn two_variants_each_show_their_own_shape_and_paint(app: TestApp) {
    let head = tools_ready(&app);
    script(&app, &head, "peanut.cad.splash", PEANUT);
    let call = r#"{"variants": [{"name": "small", "edits": "set(r, 8mm)"}, {"name": "big blue", "edits": "set(r, 14mm)\nset(fill#1.material, blue)"}], "views": ["front"]}"#;
    let r = try_call(&app, "variants", call).expect("variants");
    let sheet = picture(&r);
    keep("variants.png", &sheet);
    // two across, one down, the label's corner left out
    let cell = sheet.width as usize / 2;
    assert_eq!(sheet.height as usize, cell, "{}", text(&r));
    let count = |k: usize, of: &dyn Fn(usize, usize) -> bool| (cell / 6..cell).flat_map(|y| (k * cell..(k + 1) * cell).map(move |x| (x, y))).filter(|&(x, y)| of(x, y)).count();
    let red = |x: usize, y: usize| is_red(&sheet, x, y);
    let blue = |x: usize, y: usize| is_blue(&sheet, x, y);
    // the part: its own grey, lit, or its paint
    let part = |x: usize, y: usize| {
        let i = (y * sheet.width as usize + x) * 4;
        let (r, g, b) = (sheet.pixels[i] as i32, sheet.pixels[i + 1] as i32, sheet.pixels[i + 2] as i32);
        ((r - g).abs() < 12 && (g - b).abs() < 12 && r + g + b > 120) || red(x, y) || blue(x, y)
    };
    let cells: Vec<(usize, usize, usize)> = (0..2).map(|k| (count(k, &part), count(k, &red), count(k, &blue))).collect();
    println!("{cell} px cells: (part, red, blue) px {cells:?}");
    let [(small, small_red, small_blue), (big, big_red, big_blue)] = [cells[0], cells[1]];
    assert!(big as f64 > 2.0 * small as f64, "each variant is its own size: {small} px small, {big} px big");
    let some = cell * cell / 2000;
    assert!(small_red > some && small_blue == 0, "the small one is painted red as the plate is: {small_red} red, {small_blue} blue px");
    assert!(big_blue > some && big_red == 0, "the big one is painted its own blue, never the plate's red: {big_blue} blue, {big_red} red px");
    // Each paint sits where its variant puts it: round its first ball's front, as high as the
    // ball's middle, which is the part's (the plate's paint, baked round the plate's own ball,
    // sat elsewhere on either).
    let paints: [&dyn Fn(usize, usize) -> bool; 2] = [&red, &blue];
    for (k, paint) in paints.into_iter().enumerate() {
        let rows: Vec<usize> = (cell / 6..cell).filter(|&y| (k * cell..(k + 1) * cell).any(|x| part(x, y))).collect();
        let middle = (rows[0] + rows[rows.len() - 1]) as f64 / 2.0;
        let painted: Vec<usize> = (cell / 6..cell).flat_map(|y| (k * cell..(k + 1) * cell).filter(move |&x| paint(x, y)).map(move |_| y)).collect();
        let at = painted.iter().sum::<usize>() as f64 / painted.len() as f64;
        println!("cell {k}: the paint's middle at row {at:.1}, the part's at {middle:.1}");
        assert!((at - middle).abs() < cell as f64 / 40.0, "cell {k}: the paint's middle is at row {at:.1}, the part's at {middle:.1}");
    }
}

/// Keeps a picture where a run that keeps its outputs asks.
fn keep(name: &str, image: &makepad_model::RgbaImage) {
    if let Some(dir) = std::env::var_os("DEVHUB_KEEP") {
        std::fs::write(std::path::Path::new(&dir).join(name), image.to_png(&makepad_model::Limits::default()).expect("png")).expect("keep the picture");
    }
}

/// `object`'s front close up round `at` (its model frame, mm), `radius` mm round it.
fn front_close_up(app: &TestApp, object: &str, at: [f64; 3], radius: f64) -> makepad_model::RgbaImage {
    let args = format!(r#"{{"views":["front"],"object":"{object}","focus":{{"object":"{object}","at":[{}, {}, {}],"radius":{radius}}}}}"#, at[0], at[1], at[2]);
    picture(&try_call(app, "render", &args).expect("render"))
}

/// `a x = b` by Gauss-Jordan with the largest pivot.
fn solve<const N: usize>(mut a: [[f64; N]; N], mut b: [f64; N]) -> [f64; N] {
    for k in 0..N {
        let p = (k..N).max_by(|&i, &j| a[i][k].abs().total_cmp(&a[j][k].abs())).unwrap();
        a.swap(k, p);
        b.swap(k, p);
        for r in 0..N {
            if r != k {
                let f = a[r][k] / a[k][k];
                for c in k..N {
                    a[r][c] -= f * a[k][c];
                }
                b[r] -= f * b[k];
            }
        }
    }
    std::array::from_fn(|k| b[k] / a[k][k])
}

/// Where the light crosses `level` between neighbouring pixels, to a fraction of a pixel (a pixel's
/// middle at +0.5), along rows where the edge runs across them and down columns where it runs
/// across those: a crossing along an edge's own run says little of where it is.
fn crossings(image: &makepad_model::RgbaImage, level: f64) -> Vec<(f64, f64)> {
    let (w, h) = (image.width as usize, image.height as usize);
    let l = |x: usize, y: usize| light(image, x, y);
    let mut out = Vec::new();
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            for (dx, dy) in [(1usize, 0usize), (0, 1)] {
                let (nx, ny) = (x + dx, y + dy);
                if nx >= w - 1 || ny >= h - 1 {
                    continue;
                }
                let (a, b) = (l(x, y), l(nx, ny));
                if (a - level) * (b - level) >= 0.0 {
                    continue;
                }
                // the change across the scan, beside the pair
                let across = if dx == 1 { (l(x, y + 1) - l(x, y - 1)).abs().max((l(nx, y + 1) - l(nx, y - 1)).abs()) } else { (l(x + 1, y) - l(x - 1, y)).abs().max((l(x + 1, ny) - l(x - 1, ny)).abs()) } * 0.5;
                if (b - a).abs() < across {
                    continue;
                }
                let t = (level - a) / (b - a);
                out.push((x as f64 + 0.5 + t * dx as f64, y as f64 + 0.5 + t * dy as f64));
            }
        }
    }
    out
}

/// A conic `a x² + b xy + c y² + d x + e y = 1` about a middle, in units of a spread.
struct Conic {
    w: [f64; 5],
    middle: (f64, f64),
    spread: f64,
}

impl Conic {
    /// Fitted to `pts` by least squares, then again to those within 2 px of that (a speck's own
    /// edges do not bend it).
    fn fit(pts: &[(f64, f64)]) -> Conic {
        let n = pts.len() as f64;
        let middle = (pts.iter().map(|p| p.0).sum::<f64>() / n, pts.iter().map(|p| p.1).sum::<f64>() / n);
        let spread = (pts.iter().map(|p| (p.0 - middle.0).powi(2) + (p.1 - middle.1).powi(2)).sum::<f64>() / n).sqrt();
        let mut c = Conic { w: [0.0; 5], middle, spread };
        for pass in 0..2 {
            let (mut a, mut b) = ([[0.0; 5]; 5], [0.0; 5]);
            for p in pts.iter().filter(|p| pass == 0 || c.off(p.0, p.1) < 2.0) {
                let t = c.terms(p.0, p.1);
                for r in 0..5 {
                    for k in 0..5 {
                        a[r][k] += t[r] * t[k];
                    }
                    b[r] += t[r];
                }
            }
            c.w = solve(a, b);
        }
        c
    }

    fn terms(&self, x: f64, y: f64) -> [f64; 5] {
        let (u, v) = ((x - self.middle.0) / self.spread, (y - self.middle.1) / self.spread);
        [u * u, u * v, v * v, u, v]
    }

    /// The conic's value less 1 at (x, y): below 0 inside.
    fn value(&self, x: f64, y: f64) -> f64 {
        self.terms(x, y).iter().zip(&self.w).map(|(t, w)| t * w).sum::<f64>() - 1.0
    }

    /// How far (x, y) is from the conic, px (to first order).
    fn off(&self, x: f64, y: f64) -> f64 {
        let (u, v) = ((x - self.middle.0) / self.spread, (y - self.middle.1) / self.spread);
        let w = &self.w;
        let (gu, gv) = (2.0 * w[0] * u + w[1] * v + w[3], w[1] * u + 2.0 * w[2] * v + w[4]);
        self.value(x, y).abs() / (gu * gu + gv * gv).sqrt().max(1e-12) * self.spread
    }
}

/// A dark fill's edge on a light face, as a view shows it: (root mean square, most) px its edge
/// strays from the smooth curve fitted to it, how many of its edge's points stray over a pixel,
/// and the stray pixels: dark outside the curve or light inside it, over 2 px from it (specks
/// and spikes).
fn fill_edge(image: &makepad_model::RgbaImage) -> (f64, f64, usize, usize) {
    let (w, h) = (image.width as usize, image.height as usize);
    let mut lights: Vec<f64> = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| light(image, x, y)).collect();
    lights.sort_by(f64::total_cmp);
    let level = 0.5 * (lights[lights.len() / 20] + lights[lights.len() * 19 / 20]);
    let pts = crossings(image, level);
    assert!(pts.len() > 100, "the fill's edge shows: {} points", pts.len());
    let conic = Conic::fit(&pts);
    let offs: Vec<f64> = pts.iter().map(|p| conic.off(p.0, p.1)).collect();
    let rms = (offs.iter().map(|d| d * d).sum::<f64>() / offs.len() as f64).sqrt();
    let most = offs.iter().copied().fold(0.0, f64::max);
    let over = offs.iter().filter(|&&d| d > 1.0).count();
    let mut stray = 0;
    for y in 2..h - 2 {
        for x in 2..w - 2 {
            let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
            if conic.off(px, py) > 2.0 && (light(image, x, y) < level) != (conic.value(px, py) < 0.0) {
                stray += 1;
            }
        }
    }
    (rms, most, over, stray)
}

/// The light's second difference down the columns `xs`, root mean square over rows `ys`: shading
/// changes evenly down a smooth face or a straight valley, so what is left is scratches.
fn bumps_down(image: &makepad_model::RgbaImage, xs: std::ops::Range<usize>, ys: std::ops::Range<usize>) -> f64 {
    let (mut sum, mut n) = (0.0, 0.0);
    for y in ys {
        for x in xs.clone() {
            sum += (light(image, x, y - 1) - 2.0 * light(image, x, y) + light(image, x, y + 1)).powi(2);
            n += 1.0;
        }
    }
    (sum / n).sqrt()
}

const PAINTS: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet white = Material{albedo: [0.95 0.95 0.95]}\nlet black = Material{albedo: [0.02 0.02 0.02]}\nlet orange = Material{albedo: [0.9 0.45 0.05]}\nlet blue = Material{albedo: [0.1 0.3 0.85]}\n";

/// A white ellipsoid with a black ellipse on its front: a fill of a long ellipsoid through it,
/// whose edge on the face is all but its section's ellipse.
const ELLIPSE: &str = "let body = m.ellipsoid({radii: [20mm 14mm 12mm] at: [0 14mm 0]})\nobject(body)\nm.fill(body, white)\nm.fill(m.ellipsoid({radii: [3mm 2mm 300mm] at: [0 16mm 0]}), black)\n";

/// A big white ball (its face fills the close-up) with a pupil carved into its front and filled
/// black: the fill's edge is the carve's rim, a crease.
const PUPIL: &str = "let ball = m.sphere({radius: 40mm at: [0 40mm 0]})\nlet pupil = m.ellipsoid({radii: [4mm 6mm 4mm] at: [0 42mm 39.6mm]})\nlet eye = m.carve(ball, pupil, 0)\nobject(eye)\nm.fill(ball, white)\nm.fill(pupil, black)\n";

/// Close up, a fill's edge is a smooth curve: on a smooth face and along a carve's rim, it strays
/// from the curve fitted to it by a fraction of a pixel, with no spikes along the triangles the
/// surface is drawn with and no specks.
#[makepad_test]
fn a_close_up_of_a_fill_s_edge_is_a_smooth_curve(app: TestApp) {
    let mut worst = Vec::new();
    // (rms, most) px it may stray: on a smooth face a tenth of a pixel; along a carve's rim the
    // edge is the rim the surface's triangles draw, within a pixel or two
    for (file, body, object, at, radius, (rms_most, most_most)) in [("ellipse.cad.splash", ELLIPSE, "ellipse/body", [0.0, 16.0, 11.9], 4.0, (0.15, 0.5)), ("pupil.cad.splash", PUPIL, "pupil/eye", [0.0, 42.0, 39.6], 7.5, (0.35, 2.0))] {
        let head = tools_ready(&app);
        println!("{file}: {}", script(&app, &head, file, &format!("{PAINTS}{body}")).chars().take(200).collect::<String>());
        let image = front_close_up(&app, object, at, radius);
        keep(&format!("{}-front.png", file.split('.').next().unwrap()), &image);
        let (rms, most, over, stray) = fill_edge(&image);
        println!("{object}: {} px, its fill's edge {rms:.2} px rms off a smooth curve, {most:.2} px at most, {over} points over 1 px; {stray} stray pixels", image.width);
        worst.push((object, rms, most, stray, rms_most, most_most));
    }
    for (object, rms, most, stray, rms_most, most_most) in worst {
        assert!(rms < rms_most && most < most_most && stray == 0, "{object}: the fill's edge strays {rms:.2} px rms, {most:.2} px at most, {stray} stray pixels");
    }
}

/// Two ellipsoids side by side, blended over half a millimetre: a sharp valley down their
/// middle, in the plane x = 0, which a front view through it sees as a straight upright line.
const VALLEY: &str = "let left = m.ellipsoid({radii: [14mm 10mm 10mm] at: [-9mm 10mm 0]})\nlet right = m.ellipsoid({radii: [14mm 10mm 10mm] at: [9mm 10mm 0]})\nlet pair = m.blend([left right], 0.5mm)\nobject(pair)\nm.fill(pair, white)\n";

/// Close up on a sharp blend valley: its shading runs down it as evenly as a smooth face's does,
/// with no scratches where the triangles cross it.
#[makepad_test]
fn a_close_up_of_a_sharp_valley_shades_evenly_down_it(app: TestApp) {
    let head = tools_ready(&app);
    println!("{}", script(&app, &head, "valley.cad.splash", &format!("{PAINTS}{VALLEY}")).chars().take(200).collect::<String>());
    let image = front_close_up(&app, "valley/pair", [0.0, 10.0, 7.6], 3.0);
    keep("valley-front.png", &image);
    let side = image.width as usize;
    let rows = side * 2 / 10..side * 8 / 10;
    let valley = bumps_down(&image, side / 2 - side / 25..side / 2 + side / 25, rows.clone());
    let face = bumps_down(&image, side * 15 / 100..side * 23 / 100, rows);
    println!("{side} px: down the valley {valley:.2} levels rms of bumps, down a face beside it {face:.2}");
    assert!(valley < (2.0 * face).max(0.6), "the valley is scratched: {valley:.2} levels rms of bumps down it against {face:.2} down the face");
}

/// Two ellipsoids blended with a wide fillet, each filled its own colour.
const FILLET: &str = "let left = m.ellipsoid({radii: [14mm 10mm 10mm] at: [-9mm 10mm 0]})\nlet right = m.ellipsoid({radii: [14mm 10mm 10mm] at: [9mm 10mm 0]})\nlet pair = m.blend([left right], 3mm)\nobject(pair)\nm.fill(left, orange)\nm.fill(right, blue)\n";

/// The fillet between two fills takes the nearer operand's colour: orange gives way to blue on a
/// smooth line where the two are equally near (x = 0 for these), with no unpainted rim between.
#[makepad_test]
fn a_fillet_between_two_fills_parts_where_its_operands_are_equally_near(app: TestApp) {
    let head = tools_ready(&app);
    println!("{}", script(&app, &head, "fillet.cad.splash", &format!("{PAINTS}{FILLET}")).chars().take(200).collect::<String>());
    let image = front_close_up(&app, "fillet/pair", [0.0, 10.0, 7.0], 5.0);
    keep("fillet-front.png", &image);
    let side = image.width as usize;
    let warmth = |x: usize, y: usize| {
        let i = (y * side + x) * 4;
        image.pixels[i] as f64 - image.pixels[i + 2] as f64
    };
    // per row, where orange (red over blue) gives way to blue nearest the middle
    let (from, to) = (side * 2 / 10, side * 8 / 10);
    let mut seam = Vec::new();
    let mut rim = 0;
    for row in from..to {
        let col = (side / 4..side * 3 / 4).filter(|&x| warmth(x, row) > 0.0 && warmth(x + 1, row) <= 0.0).min_by_key(|&x| x.abs_diff(side / 2));
        if let Some(x) = col {
            let (a, b) = (warmth(x, row), warmth(x + 1, row));
            seam.push((row as f64, x as f64 + 0.5 + a / (a - b)));
        }
        // neither colour: unpainted (the base's grey), more than a pixel off the seam
        rim += (side / 4..side * 3 / 4).filter(|&x| warmth(x, row).abs() < 25.0 && col.is_none_or(|c| x.abs_diff(c) > 1)).count();
    }
    assert!(seam.len() > (to - from) / 2, "orange meets blue on most rows: {} of {}", seam.len(), to - from);
    let n = seam.len() as f64;
    let (mr, mc) = (seam.iter().map(|s| s.0).sum::<f64>() / n, seam.iter().map(|s| s.1).sum::<f64>() / n);
    let slope = seam.iter().map(|s| (s.0 - mr) * (s.1 - mc)).sum::<f64>() / seam.iter().map(|s| (s.0 - mr).powi(2)).sum::<f64>();
    let stray: Vec<f64> = seam.iter().map(|s| (s.1 - (mc + slope * (s.0 - mr))).abs()).collect();
    let rms = (stray.iter().map(|d| d * d).sum::<f64>() / n).sqrt();
    let most = stray.iter().copied().fold(0.0, f64::max);
    println!("{side} px: orange meets blue at column {mc:.1} (the middle {}), {rms:.2} px rms off its line, {most:.1} px at most; {rim} unpainted pixels", side / 2);
    assert!((mc - side as f64 / 2.0).abs() < side as f64 * 0.015, "orange meets blue where the two are equally near: column {mc:.1} of {side}");
    assert!(rms < 0.75 && most < 2.5, "the seam is a smooth line: {rms:.2} px rms off it, {most:.1} px at most");
    assert!(rim == 0, "{rim} pixels of the fillet show no fill");
}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);
