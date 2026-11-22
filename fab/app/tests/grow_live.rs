//! A grown part live in the app: placed with its force study it shows a shape, grown on this
//! machine's AI hub (one on loopback in this process with this machine's GPU device, in a home of
//! the app's own, as `force_mode.rs` has it). A change to a load in its script comes back as a new
//! shape in the view. How long that takes is printed: from the edit to the first shape that answers
//! it, to that shape built and to the view taking it.
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

#[path = "common/picture.rs"]
mod picture;
#[path = "common/wait.rs"]
mod idle_wait;
use idle_wait::wait_poll;
use makepad_strict_json::Value;
use makepad_test::{run_with_config, Role, Selector, TestApp, TestConfig};
use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
use makepad_zune_png::PngDecoder;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const SPACE: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet space = m.box({size: [192mm 138mm 156mm] at: [0 69mm 0]})\nlet part = m.grow(space, {mass: 0.3 member: 9mm})\nobject(part)\nm.fixed(Face{at: [-96mm, 69mm, 0]})\nm.force(Face{at: [96mm, 69mm, 0]}, [SIDE, -50, 0])\n";

fn script(side: f64) -> String {
    SPACE.replace("SIDE", &side.to_string())
}

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

/// A home of the app's own, with this machine's AI hub on loopback registered in it alone (as
/// `force_mode.rs`). Dropped, the home goes.
struct LocalHub {
    home: std::path::PathBuf,
}

impl LocalHub {
    fn start(name: &str) -> LocalHub {
        use makepad_widgets::makepad_micro_serde::SerJson;
        let home = std::env::temp_dir().join(format!("fab-grow-live-{name}-{}", std::process::id()));
        let mut dir = std::fs::DirBuilder::new();
        dir.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut dir, 0o700);
        dir.create(home.join("run")).expect("a home of its own");
        // one hub a process, whichever test asks first
        static PORT: OnceLock<u16> = OnceLock::new();
        let port = *PORT.get_or_init(|| {
            let url = fab_force::hub::loopback("fab-grow-live").unwrap_or_else(|e| panic!("{e}"));
            makepad_ai_hub::program_serve::service().set_gpu(makepad_compute_program_gpu::spawn().unwrap_or_else(|e| panic!("a GPU device for the hub: {e}")));
            url.rsplit(':').next().and_then(|p| p.parse().ok()).unwrap_or_else(|| panic!("the hub's port in {url}"))
        });
        let run = home.join("run");
        let token = makepad_ai_hub::machine::machine_token().unwrap_or_else(|e| panic!("this machine's token: {e}"));
        let mut file = std::fs::OpenOptions::new();
        file.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut file, 0o600);
        std::io::Write::write_all(&mut file.open(run.join("machine-token")).expect("the app's token"), token.as_bytes()).expect("the app's token");
        let pid = std::process::id();
        let entry = makepad_ai_hub::machine::NodeEntry { pid: pid as u64, port, pipes_hash: 0 };
        std::fs::write(run.join(format!("fab-grow-live-{pid}.node")), entry.serialize_json()).expect("the hub's entry");
        LocalHub { home }
    }
}

impl Drop for LocalHub {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// The view, as RGBA rows: the window grabbed as it is.
fn pixels(app: &TestApp) -> Vec<u8> {
    view_of(app, &app.screenshot())
}

/// The view's finished picture, as RGBA rows.
fn still(app: &TestApp) -> Vec<u8> {
    still_grab(app).px
}

/// The view's finished picture, with what the view said of itself as it was grabbed.
fn still_grab(app: &TestApp) -> Grab {
    grab_of(app, &picture::finished_screenshot(app))
}

/// The view's part of a grab of the window.
struct Grab {
    /// RGBA rows, `width` pixels each.
    px: Vec<u8>,
    width: usize,
    /// The view's description (its camera, hover, what it draws) as it was grabbed.
    said: String,
}

/// The view's rect of a grab of the window, as RGBA rows: the caption's job strip (a build's words
/// while it runs) is no part of the picture.
fn view_of(app: &TestApp, grab: &std::path::Path) -> Vec<u8> {
    grab_of(app, grab).px
}

fn grab_of(app: &TestApp, grab: &std::path::Path) -> Grab {
    let bytes = std::fs::read(grab).expect("the screenshot");
    let mut dec = PngDecoder::new(ZCursor::new(&bytes));
    let px = dec.decode_raw().expect("a PNG");
    let width = dec.dimensions().expect("the PNG's size").0;
    let view = app.locator(Selector::id("viewport")).snapshot();
    let (x0, y0) = (view.x().max(0) as usize, view.y().max(0) as usize);
    let (x1, y1) = ((view.x() + view.width()) as usize, (view.y() + view.height()) as usize);
    let px = (y0..y1).flat_map(|y| &px[(y * width + x0) * 4..(y * width + x1) * 4]).copied().collect();
    Grab { px, width: x1 - x0, said: view.description.to_string() }
}

/// A log line's place past `from` that has all of `words`.
fn line_with(lines: &[String], from: usize, words: &[&str]) -> Option<usize> {
    (from..lines.len()).find(|&i| words.iter().all(|w| lines[i].contains(w)))
}

/// The key of the shape the view last took past `from`, once its trace is done.
fn traced_shape(lines: &[String], from: usize) -> Option<String> {
    let new = (from..lines.len()).rev().find(|&i| lines[i].contains("fab vp sdf tiles:") && lines[i].contains(" new shape "))?;
    line_with(lines, new, &["fab vp sdf tiles:", " settled at "])?;
    lines[new].rsplit(' ').next().map(str::to_string)
}

/// The shape a fade since `mark` ended on, once the view took it and traced it to the end: what a
/// hand lets go on, and what it comes back to.
fn faded_to(app: &TestApp, mark: u64) -> String {
    let mut got = None;
    app.try_wait_until_within(HANG, || {
        let lines = app.log_since(mark).expect("the app's log").lines;
        got = line_with(&lines, 0, &["fab: part faded in"]).and_then(|faded| traced_shape(&lines, faded));
        got.is_none().then(|| "waiting for the fade's last shape, traced".to_string())
    })
    .unwrap_or_else(|e| panic!("{}", e.message()));
    got.expect("the wait ended on a shape")
}

/// The view back on `shape`, traced to the end. A drag that never moved the view is back already.
fn back_on(app: &TestApp, shape: &str) {
    let mut last = None;
    app.try_wait_until_within(HANG, || {
        last = traced_shape(&app.log_since(0).expect("the app's log").lines, 0);
        (last.as_deref() != Some(shape)).then(|| format!("waiting for the view to come back to shape {shape}: it has {last:?}"))
    })
    .unwrap_or_else(|e| panic!("{}", e.message()));
}

fn differing(a: &[u8], b: &[u8]) -> usize {
    a.chunks(4).zip(b.chunks(4)).filter(|(x, y)| x.iter().zip(*y).any(|(p, q)| p.abs_diff(*q) > 8)).count()
}

/// The pixels whose colour differs at all.
fn changed(a: &[u8], b: &[u8]) -> usize {
    a.chunks(4).zip(b.chunks(4)).filter(|(x, y)| x[..3] != y[..3]).count()
}

/// Why the picture the view came back to is not the one it settled at, worked out for the failure
/// message only: the pixels that differ (where, by how much, the first few either side), whether a
/// grab 3 s on still differs (a picture on its way, or another one), what the view said of itself
/// at each grab, and the view's and the part's events from the let-go (`let_go`) to the settle's
/// grab (`there`), from there to the back's grab, and in the 3 s after it.
fn why_back_differs(app: &TestApp, settled: &Grab, back: &Grab, let_go: u64, there: u64) -> String {
    type Px = (usize, usize, [u8; 3], [u8; 3]);
    let differ: Vec<Px> = settled.px.chunks(4).zip(back.px.chunks(4)).enumerate()
        .filter(|(_, (a, b))| a[..3] != b[..3])
        .map(|(i, (a, b))| (i % back.width, i / back.width, [a[0], a[1], a[2]], [b[0], b[1], b[2]]))
        .collect();
    let step = |d: &Px| (0..3).map(|c| d.2[c].abs_diff(d.3[c])).max().unwrap_or(0);
    let span = |at: fn(&Px) -> usize| (differ.iter().map(at).min().unwrap_or(0), differ.iter().map(at).max().unwrap_or(0));
    let ((x0, x1), (y0, y1)) = (span(|d| d.0), span(|d| d.1));
    let faint = differ.iter().filter(|d| step(d) <= 2).count();
    let largest = differ.iter().map(step).max().unwrap_or(0);
    let first: Vec<String> = differ.iter().take(12).map(|d| format!("({},{}) {:?}->{:?}", d.0, d.1, d.2, d.3)).collect();
    // each line's sequence number: the tail runs without a gap to its last line
    let tail = app.log_since(let_go).expect("the app's log");
    let numbered = |tail: &makepad_test::LogTail| -> Vec<(u64, String)> {
        let n = tail.lines.len() as u64;
        tail.lines.iter().enumerate().map(|(i, l)| (tail.last_seq + 1 + i as u64 - n, l.to_string())).collect()
    };
    let lines = numbered(&tail);
    std::thread::sleep(Duration::from_secs(3));
    let later = still_grab(app);
    let after = numbered(&app.log_since(tail.last_seq).expect("the app's log"));
    // the view's and the part's lines among them, the last 60 of each stretch
    fn events<'a>(what: &str, lines: impl Iterator<Item = &'a (u64, String)>) -> String {
        // (without the line's level and source place)
        let picked: Vec<&str> = lines.map(|(_, l)| l.split_once(" - ").map_or(l.trim(), |(_, said)| said.trim())).filter(|l| l.starts_with("fab vp sdf") || l.starts_with("fab: ")).collect();
        let cut = picked.len().saturating_sub(60);
        let cut_note = if cut > 0 { format!(" ({cut} earlier not shown)") } else { String::new() };
        format!("{what}{cut_note}:\n  {}", picked[cut..].join("\n  "))
    }
    let view = |g: &Grab| {
        let r = makepad_strict_json::parse_depth(g.said.as_bytes(), 8).unwrap_or(Value::Null);
        ["camera", "parts_drawn", "settled", "traced", "still_samples", "screen_ao", "floor", "pointer", "hover", "selected"]
            .iter().map(|k| format!("{k} {}", r.get(k).map_or("-".to_string(), Value::to_json))).collect::<Vec<_>>().join(", ")
    };
    [
        format!("{} differ in x {x0}..{x1}, y {y0}..{y1} of a view {} wide; {faint} by at most 2 a channel, the largest step {largest}; the first: {}", differ.len(), back.width, first.join(" ")),
        format!("3 s on: {} differ from the settle, {} from the back", changed(&settled.px, &later.px), changed(&back.px, &later.px)),
        format!("the view at the settle: {}", view(settled)),
        format!("the view at the back:   {}", view(back)),
        format!("the view 3 s on:        {}", view(&later)),
        events("let go to the settle's grab", lines.iter().filter(|(s, _)| *s <= there)),
        events("there and back to the back's grab", lines.iter().filter(|(s, _)| *s > there)),
        events("the 3 s after the back's grab", after.iter()),
    ]
    .join("\n")
}

/// Waits for a log line with `what` since `mark`: the time from `from`.
fn said(app: &TestApp, mark: u64, what: &str, from: Instant) -> f64 {
    let mut got = None;
    app.try_wait_until_within(HANG, || {
        let log = app.log_since(mark).expect("the app's log");
        if log.lines.iter().any(|l| l.contains(what)) {
            got = Some(from.elapsed().as_secs_f64() * 1e3);
            return None;
        }
        Some(format!("waiting for {what:?}"))
    }).unwrap_or_else(|why| {
        let all = app.log_since(0).expect("the app's log");
        let lines: Vec<_> = all.lines.iter().filter(|l| l.contains("fab: ")).map(|l| l.to_string()).collect();
        panic!("{why}; the app never said {what:?}; what it said of the model: {lines:#?}");
    });
    got.expect("the wait ended on an answer")
}

#[test]
fn a_grown_part_shows_a_shape_and_answers_a_load_change() {
    let hub = LocalHub::start("shape");
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "a_grown_part_shows_a_shape_and_answers_a_load_change").expect("the test's package");
    config.env.insert("MAKEPAD_HOME".into(), hub.home.display().to_string());
    // what `said` times, to the millisecond: its waits look every millisecond
    config.poll_interval = Duration::from_millis(1);
    run_with_config(config, |app| {
        app.locator(Selector::role(Role::Window)).wait_visible();
        let line = wait_poll(&app, || {
            call(&app, "inspect", "{}").map_err(|e| format!("the fab tools never appeared: {e}"))
        });
        let head = head_of(&text(&line));
        let edit = |id: &str, head: &str, source: &str| {
            let batch = format!(r#"{{"request_id":{id:?},"expected":{head:?},"script":"grown.cad.splash","source":{}}}"#, Value::Str(source.to_string()).to_json());
            let reply = text(&call(&app, "script", &batch).expect("script"));
            assert!(!reply.contains("error:"), "{reply}");
            head_of(&reply)
        };
        let mark = app.log_since(0).expect("the app's log").last_seq;
        let head = edit("grown", &head, &script(0.0));
        let place = format!(r#"{{"request_id":"grown-place","expected":{head:?},"edits":{}}}"#, Value::Str("place(\"grown\", at: [150mm 165mm], name: \"Grown\")".to_string()).to_json());
        let placed = text(&call(&app, "place", &place).expect("place"));
        assert!(!placed.contains("error:"), "{placed}");
        let mut head = head_of(&placed);
        let t = Instant::now();
        println!("first shape built after {:.0} ms", said(&app, mark, "part shape built in", t));
        let grown = still(&app);

        for (i, side) in [2.5, 0.0, -2.5, 0.0, 2.5, 0.0].into_iter().enumerate() {
            let mark = app.log_since(0).expect("the app's log").last_seq;
            let t = Instant::now();
            head = edit(&format!("grown-{i}"), &head, &script(side));
            let replied = t.elapsed().as_secs_f64() * 1e3;
            let answered = said(&app, mark, "part shape for its change", t);
            let built = said(&app, mark, "part shape built in", t);
            let viewed = said(&app, mark, "new shape", t);
            let traced = said(&app, mark, " settled at ", t);
            // the next edit from a part at rest: none of this one's lines comes after its mark
            let rested = said(&app, mark, "fab: part settled at", t);
            println!("edit {i} (side {side} N): script {replied:.1} ms, shape answering it {answered:.1} ms, built {built:.1} ms, in the view {viewed:.1} ms, its last shape's tiles settled {traced:.0} ms, the part settled {rested:.0} ms");
        }

        // a load turned far: the view follows
        let mark = app.log_since(0).expect("the app's log").last_seq;
        let t = Instant::now();
        head = edit("grown-turned", &head, &script(50.0));
        said(&app, mark, "part shape built in", t);
        let turned = differing(&grown, &still(&app));
        println!("the view changed in {turned} pixels");
        assert!(turned > 500, "the load turned, the view changed in {turned} pixels");

        // A load moved to other nodes, then the space it grows in made smaller: each grows anew, a
        // new session for the same samples and new samples of the solid.
        let top = script(0.0).replace("Face{at: [96mm, 69mm, 0]}", "Face{at: [0, 138mm, 0]}");
        let front = script(0.0).replace("Face{at: [96mm, 69mm, 0]}", "Face{at: [0, 69mm, 78mm]}");
        let smaller = |depth: u32| top.replace("size: [192mm 138mm 156mm]", &format!("size: [192mm 138mm {depth}mm]"));
        for (name, source) in [("a load moved", top.clone()), ("moved again", front), ("and back", top.clone()), ("its space changed", smaller(150)), ("again", smaller(144)), ("and again", smaller(138))] {
            let mark = app.log_since(0).expect("the app's log").last_seq;
            let t = Instant::now();
            head = edit(name, &head, &source);
            let answered = said(&app, mark, "part shape for its change", t);
            let built = said(&app, mark, "part shape built in", t);
            let viewed = said(&app, mark, "new shape", t);
            // it grows anew: the next edit from where this one settles
            let rested = said(&app, mark, "fab: part settled at", t);
            println!("{name}: first shape {answered:.1} ms, built {built:.1} ms, in the view {viewed:.1} ms, settled {rested:.0} ms");
        }
    })
    .unwrap();
    drop(hub);
}

/// The visible lines of text, from the semantic tree.
fn texts(app: &TestApp) -> Vec<String> {
    app.widget_snapshot().nodes().into_iter().filter(|w| w.visible() && w.node.role == Role::StaticText).map(|w| w.node.name.to_string()).collect()
}

fn own_home(name: &str) -> std::path::PathBuf {
    let home = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
    std::fs::create_dir_all(&home).expect("a home of its own");
    home
}

/// A grown part placed on an app of its own, its panel showing.
fn place_grown(app: &TestApp, source: &str) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    let line = wait_poll(app, || call(app, "inspect", "{}").map_err(|e| format!("the tools never appeared: {e}")));
    let head = head_of(&text(&line));
    let batch = format!(r#"{{"request_id":"grown","expected":{head:?},"script":"grown.cad.splash","source":{}}}"#, Value::Str(source.to_string()).to_json());
    let reply = text(&call(app, "script", &batch).expect("script"));
    assert!(!reply.contains("error:"), "{reply}");
    let head = head_of(&reply);
    let place = format!(r#"{{"request_id":"grown-place","expected":{head:?},"edits":{}}}"#, Value::Str("place(\"grown\", at: [150mm 165mm], name: \"Grown\")".to_string()).to_json());
    let placed = text(&call(app, "place", &place).expect("place"));
    assert!(!placed.contains("error:"), "{placed}");
}

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);

/// Until a line holding `what` shows in the panel: a part that cannot grow looks done, so it says.
fn panel_says(app: &TestApp, what: &str) {
    let started = Instant::now();
    wait_poll(app, || {
        let shown = texts(app);
        if shown.iter().any(|t| t.contains(what)) { Ok(()) } else { Err(format!("the panel never said {what:?}: {shown:?}")) }
    });
    println!("the panel says {what:?} after {:.1} s", started.elapsed().as_secs_f64());
}

/// Placed, a part grows through its levels to the finest its thinnest member asks for, and says how
/// it goes. A change then answers at once from the coarse levels and settles again.
#[test]
fn a_grown_part_refines_to_what_its_member_asks() {
    let hub = LocalHub::start("refines");
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "a_grown_part_refines_to_what_its_member_asks").expect("the test's package");
    config.env.insert("MAKEPAD_HOME".into(), hub.home.display().to_string());
    run_with_config(config, |app| {
        let mark = app.log_since(0).expect("the app's log").last_seq;
        place_grown(&app, &script(0.0));
        app.locator(Selector::role(Role::TreeItem).name("Grown")).wait_visible().click();
        panel_says(&app, "part: settled 3 mm");
        // it settled at the level its member asks for
        said(&app, mark, "fab: part settled at 3 mm", Instant::now());
        let log = app.log_since(mark).expect("the app's log");
        for line in log.lines.iter().filter(|l| l.contains("fab: part")) {
            println!("{line}");
        }
    })
    .unwrap();
    drop(hub);
}

/// A force's side as a value in the panel, so a drag steers the grow.
fn steered() -> String {
    script(0.0).replace("use mod.fab.*\n", "use mod.fab.*\nlet side = param(0, min: -40, max: 40, step: 1, label: \"Side force\")\n").replace("[0, -50, 0]", "[side, -50, 0]")
}

/// While a value is dragged the coarse levels follow and the panel says so; nothing refines until
/// the hand lets go, and then the part settles at its finest level again. Let go where it began it
/// goes back to the shape it had.
#[test]
fn a_drag_follows_at_the_coarse_levels_and_refines_on_release() {
    let hub = LocalHub::start("drag");
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "a_drag_follows_at_the_coarse_levels_and_refines_on_release").expect("the test's package");
    config.env.insert("MAKEPAD_HOME".into(), hub.home.display().to_string());
    run_with_config(config, |app| {
        place_grown(&app, &steered());
        app.locator(Selector::role(Role::Tab).name("Parts")).click();
        app.locator(Selector::role(Role::TreeItem).name("Grown")).wait_visible().click();
        app.locator(Selector::id("t_shape")).wait_visible().click();
        panel_says(&app, "part: settled 3 mm");
        let before = still(&app);

        let mark = app.log_since(0).expect("the app's log").last_seq;
        let t = Instant::now();
        let mut drag = app.locator(Selector::role(Role::SpinButton).name("Side force")).wait_visible().press_and_drag();
        // from the press point, the value counting from where the drag engages (3 px in), 6 px a
        // step: eight steps on, in one move whose own steps pass the values between (each step of
        // a move is a round trip to the app's main thread: a move per value made the drag 1.4 s);
        // the moves reach the app in order, and what it shows of them is waited for below
        drag.move_by(3.0, 0.0);
        drag.move_by(3.0 + 6.0 * 8.0, 0.0);
        panel_says(&app, "part: following at 5.9 mm");
        println!("the drag answers at the coarse levels after {:.0} ms", said(&app, mark, "part shape for its change", t));
        let mut dragged = 0;
        app.try_wait_until_within(HANG, || {
            dragged = differing(&before, &pixels(&app));
            (dragged <= 200).then(|| format!("the shape follows the drag: {dragged} pixels"))
        })
        .unwrap_or_else(|e| panic!("{}", e.message()));
        println!("the drag moved the view in {dragged} pixels");
        let log = app.log_since(mark).expect("the app's log");
        let refined: Vec<String> = log.lines.iter().filter(|l| l.contains("fab: part settled at") || l.contains("fab: part refining")).map(|l| l.to_string()).collect();
        assert!(refined.is_empty(), "nothing refines during a drag: {refined:#?}");

        let let_go = app.log_since(0).expect("the app's log").last_seq;
        let t = Instant::now();
        drag.release();
        println!("let go, it settles after {:.0} ms", said(&app, let_go, "fab: part settled at 3 mm", t));
        panel_says(&app, "part: settled 3 mm");
        let shape = faded_to(&app, let_go);
        let settled = still_grab(&app);
        assert!(differing(&before, &settled.px) > 200, "the settled shape answers the value let go at");

        // There and back: what it had is the answer, at once.
        let there = app.log_since(0).expect("the app's log").last_seq;
        let t = Instant::now();
        let mut drag = app.locator(Selector::role(Role::SpinButton).name("Side force")).wait_visible().press_and_drag();
        drag.move_by(3.0, 0.0);
        drag.move_by(3.0 + 6.0 * 4.0, 0.0);
        panel_says(&app, "part: following at 5.9 mm");
        // back where it began; the release comes after this move, so it lets go at that value
        drag.move_by(3.0, 0.0);
        drag.release();
        panel_says(&app, "part: settled 3 mm");
        println!("there and back, it says settled after {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
        back_on(&app, &shape);
        let log = app.log_since(there).expect("the app's log");
        assert!(!log.lines.iter().any(|l| l.contains("fab: part settled at")), "nothing to refine");
        let back_grab = still_grab(&app);
        let back = changed(&settled.px, &back_grab.px);
        // (the message, and its why, are worked out only on a failure)
        assert_eq!(back, 0, "the same shape traced to the end draws the same pixels, whatever way the view came to it: {back} differ\n{}", why_back_differs(&app, &settled, &back_grab, let_go, there));
    })
    .unwrap();
    drop(hub);
}

/// A part too slender for its cells: its solves never find the forces, and the panel says so rather
/// than that it is done.
#[test]
fn a_slender_part_says_its_solve_did_not_finish() {
    let hub = LocalHub::start("slender");
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "a_slender_part_says_its_solve_did_not_finish").expect("the test's package");
    config.env.insert("MAKEPAD_HOME".into(), hub.home.display().to_string());
    run_with_config(config, |app| {
        let plate = script(0.0).replace("size: [192mm 138mm 156mm] at: [0 69mm 0]", "size: [200mm 100mm 1.5mm] at: [0 0 0.75mm]").replace("[-96mm, 69mm, 0]", "[-100mm, 0, 0.75mm]").replace("Face{at: [96mm, 69mm, 0]}, [0, -50, 0]", "Face{at: [100mm, 0, 0.75mm]}, [0, 0, -50]");
        place_grown(&app, &plate);
        app.locator(Selector::role(Role::Tab).name("Parts")).click();
        app.locator(Selector::role(Role::TreeItem).name("Grown")).wait_visible().click();
        app.locator(Selector::id("t_shape")).wait_visible().click();
        panel_says(&app, "Its force solve did not finish");
    })
    .unwrap();
    drop(hub);
}

/// A part held and loaded on patches of it: what it grows need not reach the rest of its space.
fn held_by_regions(source: &str) -> String {
    source
        .replace("m.fixed(Face{at: [-96mm, 69mm, 0]})", "let hold = m.box({size: [24mm 24mm 24mm] at: [-84mm 69mm 0]})\nm.fixed(hold)")
        .replace("m.force(Face{at: [96mm, 69mm, 0]}, ", "let pull = m.box({size: [24mm 24mm 24mm] at: [84mm 69mm 0]})\nm.force(pull, ")
}

/// The part's size on the plate, in mm (`fab.inspect`).
fn size_of(app: &TestApp, name: &str) -> Vec<f64> {
    let reply = text(&call(app, "inspect", r#"{"program":false}"#).expect("inspect"));
    let v = makepad_script::syntax::parse_value(&reply).expect("a Splash reply");
    let parts = v.field("plate").and_then(|p| p.value.field("parts")).and_then(|f| f.value.items()).expect("plate parts");
    let part = parts.iter().find(|p| p.field("name").and_then(|f| f.value.string()) == Some(name)).unwrap_or_else(|| panic!("no part {name}"));
    part.field("size").and_then(|f| f.value.items()).expect("a size").iter().map(|x| x.number().expect("a length") * 1000.0).collect()
}

/// Settled and saved, a part opens as it settled, on a machine with no AI hub: nothing grows and its
/// panel says it settled. Its script edited outside the app, it says its inputs changed, grows and
/// keeps what it settles at again.
#[test]
fn a_saved_part_opens_as_it_settled() {
    let hub = LocalHub::start("saved");
    let folder = own_home("fab-grow-saved");
    let project = folder.join("saved.fab.splash");
    let kept = folder.join("grown.part.grow");
    let package = |test: &str, home: &std::path::Path, open: bool| {
        let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), test).expect("the test's package");
        config.env.insert("MAKEPAD_HOME".into(), home.display().to_string());
        if open {
            config.app_args.push(format!("--open={}", project.display()));
        }
        config
    };
    let show = |app: &TestApp| {
        app.locator(Selector::role(Role::Window)).wait_visible();
        app.locator(Selector::role(Role::Tab).name("Parts")).click();
        app.locator(Selector::role(Role::TreeItem).name("Grown")).wait_visible().click();
        app.locator(Selector::id("t_shape")).wait_visible().click();
    };

    let mut grown = Vec::new();
    run_with_config(package("a_saved_part_settles", &hub.home, false), |app| {
        place_grown(&app, &held_by_regions(&script(0.0)));
        show(&app);
        panel_says(&app, "part: settled 3 mm");
        let saved = text(&call(&app, "save", &format!(r#"{{"path":{:?}}}"#, project.display().to_string())).expect("save"));
        assert!(saved.contains("saved"), "{saved}");
        // what it settled at reaches the model after the panel: the save keeps it beside the
        // script, or the model does as it gets it, the project's folder known by then
        app.wait_until(|| (!kept.exists()).then(|| "the shape it settled at kept beside its script".to_string()));
        grown = size_of(&app, "Grown");
    })
    .unwrap();
    let first = std::fs::read(&kept).expect("the shape kept beside its script");
    println!("the part's shape keeps in {:.0} kB, its size {grown:.1?}", first.len() as f64 / 1e3);
    assert!(grown[1] < 156.0 - 10.0, "it is not its whole space: {grown:?}");

    let no_hub = own_home("fab-grow-saved-no-hub");
    run_with_config(package("a_saved_part_opens", &no_hub, true), |app| {
        show(&app);
        // the panel says it settled only from the shape kept, which answers what is asked: with
        // no AI hub nothing else could settle, and nothing grows after
        panel_says(&app, "part: settled 3 mm");
        let shown = texts(&app);
        assert!(!shown.iter().any(|t| t.contains("No AI hub") || t.contains("regrowing") || t.contains("growing")), "nothing grows: {shown:?}");
        // the plate the panel's run started from holds the shape kept
        app.wait_until(|| {
            let opened = size_of(&app, "Grown");
            (!grown.iter().zip(&opened).all(|(a, b)| (a - b).abs() < 1.0)).then(|| format!("the shape it was saved with: {grown:?} and {opened:?}"))
        });
    })
    .unwrap();
    assert_eq!(std::fs::read(&kept).expect("the shape kept"), first, "opening it writes nothing");

    let script_file = folder.join("grown.cad.splash");
    let source = std::fs::read_to_string(&script_file).expect("the script");
    std::fs::write(&script_file, source.replace("[0, -50, 0]", "[30, -50, 0]")).expect("the script edited");
    run_with_config(package("a_saved_part_regrows", &hub.home, true), |app| {
        show(&app);
        // The panel says it regrows only while it does, which can be over before the panel shows
        // (a second and a half): the log keeps that it did.
        app.wait_for_log_contains("fab: part inputs changed: regrowing");
        app.wait_for_log_contains("fab: part settled at 3 mm");
        panel_says(&app, "part: settled 3 mm");
        // what it settled at reaches the model, which keeps it beside the script, after the panel
        app.wait_until(|| (std::fs::read(&kept).ok().as_deref() == Some(&first[..])).then(|| "the shape it settled at for the edit kept".to_string()));
    })
    .unwrap();
    drop(hub);
    let _ = std::fs::remove_dir_all(&folder);
    let _ = std::fs::remove_dir_all(&no_hub);
}

#[test]
fn with_no_ai_hub_a_grown_part_says_so() {
    let home = own_home("fab-grow-no-hub");
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "with_no_ai_hub_a_grown_part_says_so").expect("the test's package");
    config.env.insert("MAKEPAD_HOME".into(), home.display().to_string());
    run_with_config(config, |app| {
        place_grown(&app, &script(0.0));
        app.locator(Selector::role(Role::TreeItem).name("Grown")).wait_visible().click();
        panel_says(&app, "No AI hub is running");
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(&home);
}

/// A study that holds nothing of the part: the run fails, and the panel says why. Without a study
/// the panel says what it lacks.
#[test]
fn a_run_that_fails_says_why_in_the_panel() {
    let hub = LocalHub::start("fails");
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "a_run_that_fails_says_why_in_the_panel").expect("the test's package");
    config.env.insert("MAKEPAD_HOME".into(), hub.home.display().to_string());
    run_with_config(config, |app| {
        let far = script(0.0).replace("m.fixed(Face{at: [-96mm, 69mm, 0]})", "let nowhere = m.box({size: [1mm 1mm 1mm] at: [900mm 0 0]})\nm.fixed(nowhere)");
        place_grown(&app, &far);
        app.locator(Selector::role(Role::TreeItem).name("Grown")).wait_visible().click();
        panel_says(&app, "could not grow");
    })
    .unwrap();
}
