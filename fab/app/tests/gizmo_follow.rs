//! The move gizmo stays on the part, one boot on a plate of a bracket and a vase: a drag of its
//! centre square moves the drawn vase (a field part) in the frame it moves the gizmo, the Part
//! panel's position follows before any commit, nothing is rebuilt, re-meshed or re-baked while it
//! drags, and after letting go the gizmo stays where the vase is through the commit (it once went
//! back to the old place until the committed plate came, ~3 s); the Scale handle makes the drawn
//! vase and the panel follow the same way, all three axes alike; a Scale along X handle stretches
//! the vase along X alone, every frame from letting go to its committed field shows it whole and
//! as the drag showed it (never gone for a frame, no dark specks in it), and one undo takes that
//! drag back; a click on the vase in the view
//! selects it; Move, Turn and Scale keep their choice through the workspaces and Edit shape swaps
//! the controls.
//!
//! The vase is found by its dark green around the gizmo.
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window; screenshot
//! pixels are layout points.

#[path = "common/part_editor.rs"]
mod part_editor;
#[path = "common/picture.rs"]
mod picture;
#[path = "common/workspace_row.rs"]
mod workspace_row;
#[path = "common/parts_tab.rs"]
mod parts_tab;
use makepad_script::syntax::{self, Node};
use makepad_strict_json::Value;
use makepad_test::{makepad_test, Role, Selector, TestApp};
use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
use makepad_zune_png::PngDecoder;
use std::time::{Duration, Instant};

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

fn splash(text: &str) -> Node {
    syntax::parse_value(text).expect("a Splash reply")
}

fn head_of(reply: &str) -> String {
    splash(reply).field("head").and_then(|f| f.value.string()).expect("reply has a head").to_string()
}

fn head(app: &TestApp) -> String {
    let v = splash(&text(&call(app, "inspect", r#"{"program":false}"#).expect("inspect")));
    v.field("head").and_then(|f| f.value.string()).expect("inspect has a head").to_string()
}

/// One mouse event through the app's real event path (answered once a frame has drawn it).
fn mouse(app: &TestApp, kind: &str, x: f64, y: f64) {
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).expect("connect");
    let argv: Vec<String> = ["mouse".to_string(), format!("k={kind}"), format!("x={x:.1}"), format!("y={y:.1}"), "b=0".into(), "wait=1".into()].into();
    makepad_remote::app::run(&client, &argv, &mut |_| true).expect("mouse");
}

/// A short wait, on the app's budget ([`TestApp::wait_until`]).
fn wait_for(app: &TestApp, what: &str, mut ok: impl FnMut() -> bool) {
    app.wait_until(|| (!ok()).then(|| format!("timed out waiting for {what}")));
}

/// [`wait_for`] for what takes the app long (the first view, a commit): `budget` of its main
/// thread's CPU ([`TestApp::wait_until_within`]).
fn wait_within(app: &TestApp, what: &str, budget: Duration, mut ok: impl FnMut() -> bool) {
    app.wait_until_within(budget, || (!ok()).then(|| format!("timed out waiting for {what}")));
}

/// A grab's RGB pixels.
struct Grab {
    w: usize,
    h: usize,
    px: Vec<[i32; 3]>,
}

fn grab(path: &std::path::Path) -> Grab {
    let bytes = std::fs::read(path).expect("the grab");
    let mut dec = PngDecoder::new(ZCursor::new(&bytes));
    let raw = dec.decode_raw().expect("png");
    let (w, h) = dec.dimensions().expect("dims");
    let n = dec.colorspace().expect("colorspace").num_components();
    let px = (0..w * h).map(|i| [raw[i * n] as i32, raw[i * n + 1] as i32, raw[i * n + 2] as i32]).collect();
    Grab { w, h, px }
}

impl Grab {
    /// The vase's pixels (its dark green: green over red and blue, darker than the gizmo's
    /// green arrow) in a box of half size `r` about `at`, the gizmo's readout tag (right of
    /// and above its centre `g`) left out: count, centroid.
    fn vase(&self, at: (f64, f64), r: f64, g: (f64, f64)) -> (usize, (f64, f64)) {
        let (mut n, mut sx, mut sy) = (0usize, 0.0, 0.0);
        let (x0, x1) = ((at.0 - r).max(0.0) as usize, ((at.0 + r) as usize).min(self.w));
        let (y0, y1) = ((at.1 - r).max(0.0) as usize, ((at.1 + r) as usize).min(self.h));
        for y in y0..y1 {
            for x in x0..x1 {
                let (fx, fy) = (x as f64, y as f64);
                if fx > g.0 + 8.0 && fy > g.1 - 32.0 && fy < g.1 && fx < g.0 + 400.0 {
                    continue;
                }
                let p = self.px[y * self.w + x];
                if p[1] > p[0] + 6 && p[1] > p[2] + 6 && p[1] < 150 {
                    n += 1;
                    sx += fx;
                    sy += fy;
                }
            }
        }
        (n, if n > 0 { (sx / n as f64, sy / n as f64) } else { at })
    }

    /// The dark specks in the vase in a box of half size `r` about `at` that `before` did not
    /// have: dark teal pixels (a hole in its wall shows the vase's dark inside, the gizmo's lines
    /// are grey) under 70 % of the least bright of the four 2 px away, those all the vase's
    /// teal, where `before` was plain vase 2 px about (no rim crease, outline or shading edge
    /// there to move a pixel between traces): nothing within 2 px under 85 % of the second least
    /// bright of `before`'s four. A crease through one of the four is not plain vase: the drag's
    /// still average draws it softer than a frame's single sample, which can land on it (the
    /// lip's crease under the studio key at the map's strength). Each speck as its first pixel
    /// (x, y), the dark pixels within 2 px of another one speck; and how many pixels one could be
    /// at.
    fn specks(&self, before: &Grab, at: (f64, f64), r: f64) -> (Vec<(usize, usize)>, usize) {
        let max = |p: [i32; 3]| p[0].max(p[1]).max(p[2]);
        let teal = |p: [i32; 3]| p[1] * 2 > p[0] * 3 + 20 && p[1] >= p[2] && p[1] > 60;
        let hue = |p: [i32; 3]| p[1] > p[0] + 20 && p[1] >= p[2];
        let four = |g: &Grab, x: usize, y: usize| [(x - 2, y), (x + 2, y), (x, y - 2), (x, y + 2)].map(|(i, j)| g.px[j * g.w + i]);
        let (x0, x1) = ((at.0 - r).max(2.0) as usize, ((at.0 + r) as usize).min(self.w - 2));
        let (y0, y1) = ((at.1 - r).max(2.0) as usize, ((at.1 + r) as usize).min(self.h - 2));
        let (mut found, mut seen) = (Vec::new(), 0);
        for y in y0..y1 {
            for x in x0..x1 {
                let (was, around) = (four(before, x, y), four(self, x, y));
                if !was.iter().chain(&around).all(|p| teal(*p)) {
                    continue;
                }
                let mut was_max = was.map(max);
                was_max.sort_unstable();
                if (y - 2..=y + 2).any(|j| (x - 2..=x + 2).any(|i| max(before.px[j * before.w + i]) * 20 < was_max[1] * 17)) {
                    continue;
                }
                seen += 1;
                let p = self.px[y * self.w + x];
                if !hue(p) || max(p) * 10 >= around.iter().map(|p| max(*p)).min().unwrap_or(0) * 7 {
                    continue;
                }
                let near = |f: &Vec<(usize, usize)>| f.iter().any(|q| q.0.abs_diff(x) <= 2 && q.1.abs_diff(y) <= 2);
                match found.iter_mut().find(|f| near(f)) {
                    Some(f) => f.push((x, y)),
                    None => found.push(vec![(x, y)]),
                }
            }
        }
        (found.into_iter().map(|f| f[0]).collect(), seen)
    }
}

/// The centre of the gizmo's "Move freely" square (none when no gizmo shows).
fn gizmo(app: &TestApp) -> Option<(f64, f64)> {
    let n = app.widget_snapshot().nodes().into_iter().find(|w| w.visible() && w.node.role == Role::Button && w.node.name == "Move freely")?;
    Some((n.x() as f64 + n.width() as f64 * 0.5, n.y() as f64 + n.height() as f64 * 0.5))
}

/// App log lines since `from` that rebuild, re-mesh or trace a new shape.
fn rebuilds(app: &TestApp, from: u64) -> Vec<String> {
    let lines = app.log_since(from).expect("the app's log").lines;
    lines.into_iter().filter(|l| l.contains("fab: placed") || l.contains("fab: edit:") || l.contains("fab: plate scene") || (l.contains("fab vp sdf tiles:") && l.contains("new shape"))).collect()
}

fn log_end(app: &TestApp) -> u64 {
    app.log_since(0).map(|t| t.last_seq).unwrap_or(0)
}

fn dist(a: (f64, f64), b: (f64, f64)) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}


/// The Part panel's number `name`, wherever its panel has scrolled it.
fn panel_number(app: &TestApp, name: &str) -> f64 {
    let nodes = app.widget_snapshot().nodes();
    let n = nodes.into_iter().find(|w| w.node.role == Role::SpinButton && w.node.name == name).unwrap_or_else(|| panic!("no number {name}"));
    n.node.range.as_ref().expect("a range").now
}

/// A gizmo handle's centre by name (none when it does not show).
fn handle(app: &TestApp, name: &str) -> Option<(f64, f64)> {
    let n = app.widget_snapshot().nodes().into_iter().find(|w| w.visible() && w.node.role == Role::Button && w.node.name == name)?;
    Some((n.x() as f64 + n.width() as f64 * 0.5, n.y() as f64 + n.height() as f64 * 0.5))
}

/// Nothing in the view is moving or still being traced (the viewport's own record of it).
fn settled(app: &TestApp) -> bool {
    let viewport = app.locator(Selector::id("viewport")).snapshot();
    makepad_strict_json::parse_depth(viewport.description.as_bytes(), 8).ok().and_then(|v| v.get("settled").and_then(Value::as_bool)) == Some(true)
}

/// The gizmo once it has stayed put for two reads (the camera and the panel have settled).
fn steady_gizmo(app: &TestApp) -> (f64, f64) {
    let mut last = None;
    let mut steady = None;
    wait_within(app, "the move gizmo on the vase", Duration::from_secs(20), || {
        let now = gizmo(app);
        steady = now.filter(|n| last.is_some_and(|l| dist(l, *n) < 0.5));
        last = now;
        steady.is_some()
    });
    steady.expect("the gizmo")
}

fn set_up(app: &TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    let mut line = None;
    app.wait_until_within(Duration::from_secs(30), || match call(app, "inspect", "{}") {
        Ok(l) => {
            line = Some(l);
            None
        }
        Err(e) => Some(format!("the fab tools never appeared: {e}")),
    });
    let mut head = head_of(&text(&line.expect("the wait ends on the tools' answer")));
    for (file, source, edits) in [
        ("bracket.cad.splash", include_str!("../resources/bracket.cad.splash"), "color(\"Orange\", rgb: #xe2672b)\nplace(\"bracket\", at: [150mm 165mm], name: \"Shelf bracket\", color: \"Orange\")"),
        ("vase.cad.splash", include_str!("../resources/vase.cad.splash"), "color(\"Teal\", rgb: #x2a9d8f)\nplace(\"vase\", at: [222mm 165mm], name: \"Bud vase\", color: \"Teal\")"),
    ] {
        let batch = format!(r#"{{"request_id":"gizmo-{file}","expected":{head:?},"script":{file:?},"source":{}}}"#, Value::Str(source.to_string()).to_json());
        head = head_of(&text(&call(app, "script", &batch).expect("script")));
        let batch = format!(r#"{{"request_id":"gizmo-place-{file}","expected":{head:?},"edits":{}}}"#, Value::Str(edits.to_string()).to_json());
        head = head_of(&text(&call(app, "place", &batch).expect("place")));
    }
    // The vase is drawn by the engine, and lit, before it can be found by its colour.
    wait_within(app, "the view lit", Duration::from_secs(60), || app.log_since(0).map(|t| t.lines).unwrap_or_default().iter().any(|l| l.contains("fab studio: the view is lit")));
    wait_within(app, "the vase drawn", Duration::from_secs(60), || app.log_since(0).map(|t| t.lines).unwrap_or_default().iter().any(|l| l.contains("drawn by the engine")));
}

fn panel_xy(app: &TestApp) -> (f64, f64) {
    (panel_number(app, "X"), panel_number(app, "Y"))
}

#[makepad_test]
fn the_gizmo_and_the_panel_follow_the_part_through_drags_and_commits(app: TestApp) {
    set_up(&app);

    // Turn chosen in Design is still chosen after Monitor; Move is a button there too.
    app.locator(Selector::id("t_rotate")).wait_visible().click();
    workspace_row::press(&app, "Monitor");
    workspace_row::press(&app, "3D modeling");
    app.locator(Selector::id("t_rotate")).wait_checked(true);

    // Pick the vase in the parts list (a narrow window stacks it behind its tab).
    parts_tab::open(&app);
    app.locator(Selector::role(Role::TreeItem).name("Bud vase")).wait_visible().click();
    part_editor::open(&app);
    app.locator(Selector::id("t_move")).wait_visible().click();
    app.locator(Selector::id("t_move")).wait_checked(true);
    let g0 = steady_gizmo(&app);
    let before = grab(&picture::finished_screenshot(&app));
    // The vase stands over the gizmo (which sits at its foot).
    let (n0, v0) = before.vase(g0, 120.0, g0);
    assert!(n0 > 400, "no vase over the gizmo ({n0} px)");
    let head0 = head(&app);
    let (x0, y0) = panel_xy(&app);

    // ---- Move: drag the centre square sideways in steps and hold ----
    let from = log_end(&app);
    let (dx, dy) = (90.0, 10.0);
    mouse(&app, "down", g0.0, g0.1);
    for i in 1..=10 {
        let f = i as f64 / 10.0;
        mouse(&app, "move", g0.0 + dx * f, g0.1 + dy * f);
    }
    // The frame after the last move: the vase and the gizmo moved by the same amount.
    let g1 = gizmo(&app).expect("the gizmo while dragging");
    let (n1, v1) = grab(&app.screenshot()).vase((v0.0 + g1.0 - g0.0, v0.1 + g1.1 - g0.1), 120.0, g1);
    assert!(dist(g0, g1) > 40.0, "the gizmo did not follow the drag: {g0:?} -> {g1:?}");
    let shift_g = (g1.0 - g0.0, g1.1 - g0.1);
    let shift_v = (v1.0 - v0.0, v1.1 - v0.1);
    assert!(dist(shift_g, shift_v) < 10.0, "the drawn vase and the gizmo moved apart during the drag: gizmo by {shift_g:?}, vase by {shift_v:?} ({n0} -> {n1} px)");
    let (x1, y1) = panel_xy(&app);
    assert!((x1 - x0).abs() + (y1 - y0).abs() > 1.0, "the panel kept the old position during the drag: ({x0}, {y0})");
    let during = rebuilds(&app, from);
    assert!(during.is_empty(), "a drag rebuilt or re-baked: {during:?}");
    assert_eq!(head(&app), head0, "a drag committed before it was let go");

    // Let go: the gizmo stays on the vase from the next frame through the commit.
    mouse(&app, "up", g0.0 + dx, g0.1 + dy);
    let let_go = Instant::now();
    let mut committed = None;
    app.wait_until_within(Duration::from_secs(20), || {
        let g = gizmo(&app).expect("the gizmo after the drag");
        assert!(dist(g, g1) < 6.0, "{:.0} ms after letting go the gizmo is at {g:?}, not on the vase at {g1:?}", let_go.elapsed().as_secs_f64() * 1000.0);
        if committed.is_none() && head(&app) != head0 {
            committed = Some(let_go.elapsed());
        }
        match committed {
            None => Some("the move never committed".to_string()),
            Some(t) if let_go.elapsed() <= t + Duration::from_millis(300) => Some("the gizmo watched 300 ms past the commit".to_string()),
            Some(_) => None,
        }
    });
    let g2 = gizmo(&app).expect("the gizmo");
    let (_, v2) = grab(&picture::finished_screenshot(&app)).vase(v1, 120.0, g2);
    assert!(dist(v2, v1) < 6.0, "the committed vase is not where the drag left it: {v1:?} -> {v2:?}");
    let (x2, y2) = panel_xy(&app);
    assert!((x2 - x1).abs() + (y2 - y1).abs() < 1.0, "the committed place is not where the drag left it: ({x1}, {y1}) -> ({x2}, {y2})");

    // ---- Scale: the part, its handle and the panel follow, before any commit ----
    app.locator(Selector::role(Role::Radio).name("Scale")).click();
    wait_for(&app, "the scale handle", || handle(&app, "Scale").is_some());
    let h0 = handle(&app, "Scale").expect("the handle");
    let g = (h0.0, h0.1 + 120.0);
    let (n0, _) = grab(&picture::finished_screenshot(&app)).vase(g, 200.0, (-1000.0, -1000.0));
    let (s0, head1) = (panel_number(&app, "Scale X"), head(&app));
    mouse(&app, "down", h0.0, h0.1);
    for i in 1..=10 {
        mouse(&app, "move", h0.0 + 3.0 * i as f64, h0.1 - 5.0 * i as f64);
    }
    let s1 = panel_number(&app, "Scale X");
    let h1 = handle(&app, "Scale").expect("the handle while dragging");
    let (n1, _) = grab(&app.screenshot()).vase(g, 200.0, (-1000.0, -1000.0));
    assert!(s1 > s0 + 5.0, "the panel kept the old scale during the drag: {s0} -> {s1}");
    assert!(dist(h0, h1) > 10.0, "the handle stayed behind the drag: {h0:?} -> {h1:?}");
    assert!(n1 as f64 > n0 as f64 * 1.1, "the drawn vase did not grow during the drag: {n0} -> {n1} px");
    assert_eq!(head(&app), head1, "a drag committed before it was let go");
    mouse(&app, "up", h0.0 + 30.0, h0.1 - 50.0);
    wait_within(&app, "the scale's commit", Duration::from_secs(20), || head(&app) != head1);
    let s2 = panel_number(&app, "Scale X");
    assert!((s2 - s1).abs() < 1.0, "the committed scale is not what the drag showed: {s1} -> {s2}");
    let (y2, z2) = (panel_number(&app, "Scale Y"), panel_number(&app, "Scale Z"));
    assert!((y2 - s2).abs() < 0.05 && (z2 - s2).abs() < 0.05, "the centre handle scaled the axes apart: {s2} {y2} {z2}");

    // ---- Scale along X: that axis alone follows the drag, and one undo takes it back ----
    wait_for(&app, "the X scale handle", || handle(&app, "Scale along X").is_some());
    let keep = |name: &str| {
        if let Some(dir) = std::env::var_os("DEVHUB_KEEP") {
            std::fs::copy(app.screenshot(), std::path::Path::new(&dir).join(name)).expect("keep the grab");
        }
    };
    keep("scale-handles.png");
    let a0 = handle(&app, "Scale along X").expect("the X handle");
    let len = dist(a0, g2).max(1.0);
    let out = ((a0.0 - g2.0) / len, (a0.1 - g2.1) / len);
    let (n0, _) = grab(&picture::finished_screenshot(&app)).vase(g, 200.0, (-1000.0, -1000.0));
    let head2 = head(&app);
    mouse(&app, "down", a0.0, a0.1);
    for i in 1..=10 {
        let d = 6.0 * i as f64;
        mouse(&app, "move", a0.0 + out.0 * d, a0.1 + out.1 * d);
    }
    let (x3, y3, z3) = (panel_number(&app, "Scale X"), panel_number(&app, "Scale Y"), panel_number(&app, "Scale Z"));
    // The drag's own frame: each move gives the vase a new shape, traced tile by tile, so a frame
    // before the last shape is traced whole shows only part of the stretch (on a loaded box, less
    // of it). Held there till it is traced whole; its picture shows the widening.
    wait_within(&app, "the stretched vase traced", Duration::from_secs(20), || settled(&app));
    let shot = picture::finished_screenshot(&app);
    if let Some(dir) = std::env::var_os("DEVHUB_KEEP") {
        std::fs::copy(&shot, std::path::Path::new(&dir).join("scale-along-x-drag.png")).expect("keep the grab");
    }
    let preview = grab(&shot);
    let (nd, _) = preview.vase(g, 200.0, (-1000.0, -1000.0));
    let (_, inside) = preview.specks(&preview, g, 200.0);
    println!("[fab-gizmo] scale along X: {s2:.1} % -> X {x3:.1} Y {y3:.1} Z {z3:.1} %, vase {n0} -> {nd} px");
    assert!(x3 > s2 + 5.0, "the panel's X kept the old scale during the drag: {s2} -> {x3}");
    assert!((y3 - y2).abs() < 0.05 && (z3 - z2).abs() < 0.05, "the X handle moved another axis: Y {y2} -> {y3}, Z {z2} -> {z3}");
    assert!(nd as f64 > n0 as f64 * 1.03, "the drawn vase did not widen during the drag: {n0} -> {nd} px");
    assert_eq!(head(&app), head2, "a drag committed before it was let go");
    mouse(&app, "up", a0.0 + out.0 * 60.0, a0.1 + out.1 * 60.0);
    // Each frame till the committed field is drawn and settled: the vase as the drag left it,
    // the preview kept till the field is whole, the field without holes
    let (mut frames, mut least, mut most_dark) = (0, usize::MAX, 0);
    wait_within(&app, "the X scale's commit, drawn", Duration::from_secs(20), || {
        let shot = app.screenshot();
        let now = grab(&shot);
        let (n, _) = now.vase(g, 200.0, (-1000.0, -1000.0));
        let (dark, _) = now.specks(&preview, g, 200.0);
        frames += 1;
        (least, most_dark) = (least.min(n), most_dark.max(dark.len()));
        let whole = n as f64 >= nd as f64 * 0.9;
        if !whole || !dark.is_empty() {
            if let Some(dir) = std::env::var_os("DEVHUB_KEEP") {
                std::fs::copy(&shot, std::path::Path::new(&dir).join(format!("let-go-frame-{frames}.png"))).expect("keep the grab");
            }
        }
        assert!(whole, "frame {frames} after letting go shows {n} px of the vase, the drag {nd}: the preview went before its field was whole");
        assert!(dark.is_empty(), "frame {frames} after letting go has {} dark specks in the stretched vase where its drag had none (of {inside} pixels there), at {:?}", dark.len(), &dark[..dark.len().min(12)]);
        head(&app) != head2 && settled(&app)
    });
    println!("[fab-gizmo] letting go: {frames} frames to the committed field, vase at least {least} px of the drag's {nd}, at most {most_dark} new dark specks (of {inside} pixels)");
    let x4 = panel_number(&app, "Scale X");
    assert!((x4 - x3).abs() < 1.0 && (panel_number(&app, "Scale Y") - y2).abs() < 0.05, "the committed scale is not what the drag showed: X {x3} -> {x4}");
    keep("scale-along-x-done.png");
    let head3 = head(&app);
    app.locator(Selector::id("undo").nth(0)).press();
    wait_within(&app, "the undo", Duration::from_secs(20), || head(&app) != head3 && (panel_number(&app, "Scale X") - s2).abs() < 0.05);
    assert!((panel_number(&app, "Scale Y") - s2).abs() < 0.05 && (panel_number(&app, "Scale Z") - s2).abs() < 0.05, "one undo did not take the X stretch back to {s2} %");

    // ---- A click on the vase in the view selects it too: another part first, then the click ----
    app.locator(Selector::id("t_move")).click();
    let g3 = steady_gizmo(&app);
    let (_, v3) = grab(&picture::finished_screenshot(&app)).vase(g3, 120.0, g3);
    parts_tab::open(&app);
    app.locator(Selector::role(Role::TreeItem).name("Shelf bracket")).wait_visible().click();
    wait_within(&app, "the gizmo on the other part", Duration::from_secs(20), || gizmo(&app).is_some_and(|g| dist(g, g3) > 20.0));
    mouse(&app, "down", v3.0, v3.1);
    mouse(&app, "up", v3.0, v3.1);
    wait_within(&app, "the gizmo back on the clicked vase", Duration::from_secs(20), || gizmo(&app).is_some_and(|g| dist(g, g3) < 6.0));

    // ---- Edit shape swaps the controls, and Move swaps them back ----
    let shape_handle = |app: &TestApp| app.widget_snapshot().nodes().into_iter().any(|w| w.visible() && w.node.role == Role::Slider && w.node.name == "Height handle");
    assert!(!shape_handle(&app), "a shape handle shows in Move mode");
    app.locator(Selector::id("t_shape")).wait_visible().click();
    wait_for(&app, "the shape handles", || shape_handle(&app));
    assert!(gizmo(&app).is_none(), "the move gizmo shows in Edit shape mode");
    app.locator(Selector::id("t_move")).click();
    wait_for(&app, "the move gizmo back", || gizmo(&app).is_some() && !shape_handle(&app));
}
