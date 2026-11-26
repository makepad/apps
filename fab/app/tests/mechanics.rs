//! Mechanics on the native GPU, every kind of body, in two apps side by side: a jointed chain starts moving
//! without Play and a link can be grabbed, and runs again each time Mechanics opens; an ordinary box becomes a free body that can be
//! thrown; a field (SDF) vase rests on the ground; a box beside chainmail stays grabbable; the
//! action bar's Drop and Hang change only the selected part (a cube; on chainmail, its links); a bar
//! held by hand lifts the block on its string with Hang and goes where the pointer carries it; a
//! servo's row in the panel turns its arm and holds it there; and Mechanics opens at the speed chosen
//! before the app started, and keeps a speed chosen. No headless rendering.

#[path = "common/workspace_row.rs"]
mod workspace_row;
use makepad_strict_json::Value;
use makepad_test::{run_with_config, Locator, Role, Selector, TestApp, TestConfig};
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

fn head_of(reply: &str) -> String {
    let reply = makepad_script::syntax::parse_value(reply).expect("a Splash reply");
    reply.field("head").and_then(|f| f.value.string()).expect("reply has a head").to_string()
}

/// Where a visible node of this role and name is on screen (its centre).
fn centre(app: &TestApp, role: Role, name: &str) -> Option<(f64, f64)> {
    app.widget_snapshot().nodes().into_iter().find(|w| w.visible() && w.node.role == role && w.node.name == name).map(|w| ((w.node.bounds.x + w.node.bounds.w * 0.5) as f64, (w.node.bounds.y + w.node.bounds.h * 0.5) as f64))
}

/// The test's own clock, for the log: when each wait ended (where the time between them goes).
fn since_start() -> f64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// A line of the log, named by the app it is about (each app runs on a thread of its name).
fn say(line: std::fmt::Arguments) {
    println!("[{}] {line}", std::thread::current().name().unwrap_or("test"));
}

/// The app main thread's CPU budget per wait (a loaded box spends it later, not sooner); the
/// runtime also detects idle hangs. An app drawing a running sim is never idle: a wait for a
/// state that never comes ends here, in a minute or two, with the app's log.
const HANG: Duration = Duration::from_secs(60);

/// What the app logged and the buttons it shows, for a failed wait: the viewport's lines too, which
/// say what its tracer was doing meanwhile.
fn report(app: &TestApp) {
    if let Ok(log) = app.log_since(0) {
        for l in log.lines.iter().filter(|l| l.contains("fab") || l.contains("mech")).rev().take(60).collect::<Vec<_>>().into_iter().rev() {
            say(format_args!("[app] {l}"));
        }
    }
    for w in app.widget_snapshot().nodes().into_iter().filter(|w| w.visible() && w.node.role == Role::Button).take(60) {
        say(format_args!("[node] {:?} {:?}", w.node.role, w.node.name));
    }
}

fn wait_for<T>(app: &TestApp, what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let started = Instant::now();
    let mut got = None;
    app.try_wait_until_within(HANG, || {
        got = f();
        got.is_none().then(|| format!("waiting for {what}"))
    }).unwrap_or_else(|why| {
        report(app);
        panic!("{why}; timed out waiting for {what}");
    });
    say(format_args!("wait {what}: {:.3} s wall (at {:.2} s)", started.elapsed().as_secs_f64(), since_start()));
    got.expect("the wait ended on an answer")
}

/// What the sim should do within `sim_seconds` of its own clock. A loaded box runs the sim in slow
/// motion (it falls behind real time rather than catch up), so that takes longer, not more of
/// the sim's time; a sim that did not do it in that time is a failure on any box.
fn within_sim<T>(app: &TestApp, what: &str, sim_seconds: f64, mut f: impl FnMut() -> Option<T>) -> T {
    let mut last = sim_clock(app);
    let mut ran = 0.0;
    wait_for(app, what, || {
        // the clock first: what `f` sees is at least as new as the time it counts against
        let now = sim_clock(app);
        if let Some(v) = f() {
            let observed = ran + match (last, now) { (Some(a), Some(b)) => (b - a).max(0.0), _ => 0.0 };
            say(format_args!("sim {what}: {observed:.3} s observed, {sim_seconds:.3} s allowed"));
            return Some(v);
        }
        // (a Reset takes the clock back)
        if let (Some(a), Some(b)) = (last, now) {
            ran += (b - a).max(0.0);
        }
        last = now.or(last);
        if ran > sim_seconds {
            report(app);
            panic!("not in {sim_seconds} s of the sim's own time: {what}");
        }
        None
    })
}

/// The widget once it shows.
fn shown(app: &TestApp, selector: Selector) -> Locator {
    wait_for(app, &format!("{selector:?} to show"), || {
        app.locator(selector.clone()).try_wait_visible().ok()
    });
    app.locator(selector)
}

fn button(app: &TestApp, name: &str) -> Locator {
    shown(app, Selector::role(Role::Button).name(name))
}

const CHAIN: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet links = for i in 0..6 { m.box({size: [12mm 6mm 5mm] at: [i * 12mm + 6mm 3mm 0]}) }\nlet chain = m.chain(links)\nm.anchor(links[0], {at: [2.5mm 3mm 0]})\nobject(links[0])\n";

const BOX: &str = "use mod.model.*\nuse mod.fab.*\nlet block = mod.model.box({size: [20mm 20mm 20mm]})\nobject(block)\n";

const OTHER: &str = "\nlet other = mod.model.box({size: [20mm 20mm 20mm] at: [80mm 0 0]})\nobject(other)\n";

// A small shelf supplies an overhang and a removable support piece.
const SHELF: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet wall = m.box({size: [2.5mm 12.5mm 10mm] at: [1.25mm 6.25mm 5mm]})\nlet slab = m.box({size: [20mm 2.5mm 10mm] at: [10mm 13.75mm 5mm]})\nlet shelf = m.union(wall, slab)\nobject(shelf)\n";

// A small connected sheet exercises interlocks, pin/release and the adjacent free body.
const MAIL: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet mail = m.mail(Rect{size: [20mm 20mm]})\nobject(mail)\n";

// A bar held by hand and a block beside it on a string, each its own object: hung, the block swings in
// under the bar only a little.
const STRUNG: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet bar = m.box({size: [60mm 6mm 8mm] at: [0 3mm 0]})\nlet block = m.box({size: [12mm 6mm 12mm] at: [0 3mm 12mm]})\nlet s = m.string(bar, block, {from: [0 3mm 0] to: [0 3mm 12mm] length: 60mm})\nm.anchor(bar, {at: [0 3mm 0] held: true})\nobject(bar)\nobject(block)\n";

// A base held fixed and an arm on a hinge with a position servo, lying on the bed.
const SERVO: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet base = m.union(m.box({size: [20mm 9mm 16mm] radius: 1.5mm at: [0 4.5mm 0]}), m.cylinder({radius: 3.6mm height: 9mm at: [0 4.5mm 8.75mm]}))\nlet arm = m.union(m.box({size: [9mm 9mm 30mm] radius: 1.5mm at: [0 4.5mm 24.5mm]}), m.cylinder({radius: 3.6mm height: 9mm at: [0 4.5mm 8.75mm]}))\nm.anchor(base, {at: [0 4.5mm 0] fixed: true})\nlet shoulder = m.hinge(base, arm, {at: [0 0 8.75mm] pin: 1.4mm lower: -90° upper: 90° motor: @position})\nobject(base)\n";

/// Puts a program on the plate (the Fab tools are linked on the window's first frames).
fn load(app: &TestApp, source: &str, request: &str) -> String {
    app.locator(Selector::role(Role::Window)).wait_visible();
    let line = wait_for(app, "the fab tools", || call(app, "inspect", "{}").ok());
    let head = head_of(&text(&line));
    let batch = format!(r#"{{"request_id":{request:?},"expected":{head:?},"script":"mechanics.cad.splash","source":{}}}"#, Value::Str(source.to_string()).to_json());
    let reply = text(&call(app, "script", &batch).expect("script"));
    say(format_args!("loaded {request} (at {:.2} s)", since_start()));
    reply
}

fn distance(a: (f64, f64), b: (f64, f64)) -> f64 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

/// The Mechanics panel's sim clock ("1.25 s", maybe followed by a note), in seconds.
fn sim_clock(app: &TestApp) -> Option<f64> {
    app.widget_snapshot().nodes().into_iter().filter(|w| w.visible()).find_map(|w| {
        let mut words = w.node.name.split_whitespace();
        let t = words.next()?.parse::<f64>().ok()?;
        (words.next() == Some("s")).then_some(t)
    })
}

/// Into Mechanics with the program just loaded (`tip` is a grab target of the new program only): where
/// the tip's target first shows.
fn enter(app: &TestApp, tip: &str) -> (f64, f64) {
    workspace_row::press(app, "Mechanics");
    let at = wait_for(app, "a live body's grab target", || centre(app, Role::Button, tip));
    // Play and Pause are separate buttons; the running sim shows Pause.
    button(app, "Pause");
    // The sim is built and stepping: its clock advances (the targets exist before the bodies do).
    let t0 = wait_for(app, "the sim clock", || sim_clock(app));
    wait_for(app, "the sim clock to advance", || sim_clock(app).filter(|t| (*t - t0).abs() > 0.05));
    at
}

/// The parts the viewport's recording on display drew.
fn parts_drawn(app: &TestApp) -> Option<u64> {
    let viewport = app.locator(Selector::id("viewport")).try_snapshot().ok()?;
    makepad_strict_json::parse_depth(viewport.description.as_bytes(), 8).ok()?.get("parts_drawn")?.as_u64()
}

/// The target's centre once it has not moved for 0.4 s of the sim's own time: a frame that comes
/// late is no rest.
fn settled(app: &TestApp, tip: &str) -> (f64, f64) {
    settled_all(app, &[tip])[0]
}

/// The targets' centres once none of them has moved for 0.4 s of the sim's own time (they come to
/// rest together, in one wait).
fn settled_all(app: &TestApp, tips: &[&str]) -> Vec<(f64, f64)> {
    let mut still: Vec<(f64, f64)> = tips.iter().map(|tip| centre(app, Role::Button, tip).expect("part target")).collect();
    let mut since = wait_for(app, "the sim clock", || sim_clock(app));
    within_sim(app, if tips.len() == 1 { "the part to settle" } else { "the parts to settle" }, 30.0, || {
        let t = sim_clock(app)?;
        let now = tips.iter().map(|tip| centre(app, Role::Button, tip)).collect::<Option<Vec<_>>>()?;
        if now.iter().zip(&still).any(|(p, s)| distance(*p, *s) > 1.0) || t < since {
            still = now.clone();
            since = t;
        }
        (t - since >= 0.4).then_some(now)
    })
}

/// The speed button shows `from`; pick `to` in its menu.
fn set_speed(app: &TestApp, from: &str, to: &str) {
    button(app, from).click();
    shown(app, Selector::role(Role::MenuItem).any_window().name(to)).click();
    shown(app, Selector::id("speed").name(to));
}

/// Select with a press and release, then watch the action move only this part. A dropped part
/// falls its 60 mm in about a tenth of a second at 1x: at 0.1x the lift stays in view long enough
/// to be seen on a loaded box; its fall back is watched at 1x. Leaves the speed at 0.1x.
fn drop_and_hang(app: &TestApp, tip: &str) {
    app.locator(Selector::role(Role::Button).name(tip)).click_at_center();
    // The actions apply to the selection (to every part when there is none): wait for it to be taken.
    shown(app, Selector::role(Role::Button).name(tip).state("selected"));
    let rests = settled_all(app, &["Grab other", tip]);
    let (other_rest, rest) = (rests[0], rests[1]);
    let other_stays = || {
        let p = centre(app, Role::Button, "Grab other").expect("unselected box stays visible");
        if distance(p, other_rest) > 3.0 {
            if let Ok(log) = app.log_since(0) {
                log.lines.iter().filter(|l| l.contains("fab mech:")).for_each(|l| say(format_args!("[app] {l}")));
            }
        }
        assert!(distance(p, other_rest) <= 3.0, "acting on {tip} moved the unselected box: {other_rest:?} -> {p:?}");
    };

    set_speed(app, "1×", "0.1×");
    button(app, "Drop").click();
    within_sim(app, "Drop to lift the selected part", 1.0, || {
        other_stays();
        centre(app, Role::Button, tip).filter(|p| p.1 <= rest.1 - 30.0)
    });
    set_speed(app, "0.1×", "1×");
    let mut near_since = None;
    within_sim(app, "the dropped part to return near rest", 2.0, || {
        other_stays();
        let (p, t) = (centre(app, Role::Button, tip)?, sim_clock(app)?);
        if (p.1 - rest.1).abs() > 15.0 {
            near_since = None;
            return None;
        }
        (t - *near_since.get_or_insert(t) >= 0.04).then_some(p)
    });

    button(app, "Hang").click();
    within_sim(app, &format!("Hang to keep {tip} at least 30 px above rest"), 1.0, || {
        other_stays();
        centre(app, Role::Button, tip).filter(|p| p.1 <= rest.1 - 30.0)
    });
    set_speed(app, "1×", "0.1×");
}

/// Lifts a body by its grab target and lets go; returns where it was before. The press goes where
/// the settle last saw the target (the app takes the body under the pointer): looking the target
/// up again can come back empty for a frame on a loaded box, which is not what the phases test.
fn lift(app: &TestApp, tip: &str, what: &str, dy: f64, by: f64) -> ((f64, f64), makepad_test::PointerDrag) {
    let rest = settled(app, tip);
    let mut hold = app.press_at(rest.0, rest.1);
    hold.move_by(0.0, dy);
    within_sim(app, what, 5.0, || centre(app, Role::Button, tip).filter(|p| p.1 < rest.1 - by));
    (rest, hold)
}

fn chain(app: &TestApp) {
    let reply = load(app, CHAIN, "mech-chain");
    assert!(reply.contains("links[5]"), "{reply}");
    let tip = "Grab links[0] links[5]";
    // A piece on a pin shows from its first frame in Mechanics where the sim hangs it (turned on its pin,
    // not where it printed), its targets with it, and it hangs there at rest: a second of the sim leaves
    // the tip's target where it first showed.
    let at0 = enter(app, tip);
    // it opens at the speed chosen before this launch
    set_speed(app, "0.1×", "1×");
    let t0 = wait_for(app, "the sim clock", || sim_clock(app));
    within_sim(app, "the sim to run at load", 5.0, || {
        let p = centre(app, Role::Button, tip)?;
        assert!(distance(p, at0) < 3.0, "the pinned chain moved by itself at load: {at0:?} -> {p:?}");
        (sim_clock(app)? - t0 >= 0.3).then_some(())
    });
    // Pushed aside and let go, it swings without Play.
    wait_for(app, "the engine to draw the chain", || parts_drawn(app).filter(|n| *n > 0));
    let mut push = app.locator(Selector::role(Role::Button).name(tip)).press_and_drag();
    push.move_by(80.0, 0.0);
    within_sim(app, "the link to follow the push", 5.0, || centre(app, Role::Button, tip).filter(|p| distance(*p, at0) > 15.0));
    push.release();
    let pushed = centre(app, Role::Button, tip).expect("tip at release");
    within_sim(app, "the chain to swing without Play", 10.0, || {
        let p = centre(app, Role::Button, tip)?;
        (distance(p, pushed) > 10.0).then_some(p)
    });

    // Pause to aim at a stationary link. Taking it resumes the sim itself.
    button(app, "Pause").click();
    button(app, "Play");
    let before = centre(app, Role::Button, tip).expect("paused tip");
    std::thread::sleep(Duration::from_millis(150));
    assert!(distance(before, centre(app, Role::Button, tip).expect("paused tip")) < 0.5, "a paused chain moves");
    // A semantic grab target can precede its mesh; a press must hit the drawn chain.
    wait_for(app, "the engine to draw the chain", || parts_drawn(app).filter(|n| *n > 0));
    let mut hold = app.locator(Selector::role(Role::Button).name(tip)).press_and_drag();
    hold.move_by(100.0, -60.0);
    within_sim(app, "the link to follow its grab", 5.0, || {
        let p = centre(app, Role::Button, tip)?;
        (distance(p, before) > 15.0).then_some(())
    });
    hold.release();

    // The drag selected the chain; Reset while paused must reset and resume it, too.
    button(app, "Pause").click();
    button(app, "Play");
    button(app, "Reset").click();
    button(app, "Pause");
    let reset = centre(app, Role::Button, tip).expect("reset tip");
    within_sim(app, "Reset to keep running", 10.0, || {
        let p = centre(app, Role::Button, tip)?;
        (distance(p, reset) > 10.0).then_some(())
    });
}

/// Mechanics opens on a running sim every time: Pause shows and the sim clock moves, four times over.
/// Each opening builds a new sim, whose first news comes whenever its thread gets there, and only that
/// news may wake the view: a view that waits for a frame it never asked for shows the sim paused.
fn reopen(app: &TestApp) {
    let tip = "Grab links[0] links[5]";
    for _ in 0..4 {
        workspace_row::press(app, "Design");
        workspace_row::wait_shown(app, "Design");
        enter(app, tip);
    }
}

/// A plain box is a free body that keeps flying after a throw; Reset restores it; Drop and Hang
/// act on it alone. Leaves the speed at 0.1x.
fn cube(app: &TestApp) {
    let reply = load(app, &format!("{BOX}{OTHER}"), "mech-cube");
    assert!(reply.contains("block") && reply.contains("other"), "{reply}");
    let tip = "Grab block";
    enter(app, tip);
    // Throw is the default tool. The tiny drop to the floor settles; the original free body's
    // position is the reset target.
    let rest = settled(app, tip);
    let mut hold = app.press_at(rest.0, rest.1);
    hold.move_by(0.0, -150.0);
    within_sim(app, "the ordinary box to lift", 5.0, || centre(app, Role::Button, tip).filter(|p| p.1 < rest.1 - 70.0));
    hold.fling_by(420.0, -190.0, 0.03);
    let a = centre(app, Role::Button, tip).expect("tip at release");
    // It goes on the way it was thrown, to the right: a box let go without a throw just falls,
    // and doesn't come this far on screen.
    within_sim(app, "the thrown box to fly on to the right", 1.0, || centre(app, Role::Button, tip).filter(|p| p.0 > a.0 + 150.0));
    // The thrown box is still selected, so Reset restores just that part.
    button(app, "Reset").click();
    within_sim(app, "the free box to reset", 5.0, || centre(app, Role::Button, tip).filter(|p| distance(*p, rest) < 3.0));

    drop_and_hang(app, tip);
}

/// A strung piece loads as it printed, its string slack. Hang lifts the hand-held bar until the block hangs
/// under it, clear of the bed (by 10 mm: a few pixels). Taken and carried, the bar stays under the pointer
/// (its grab target, about the bar's centre, within 3 px of the pointer's path) and the block comes along
/// on its string, across and up; let go, the bar stays where it was let go while the block swings on.
fn strings(app: &TestApp) {
    let reply = load(app, STRUNG, "mech-strings");
    assert!(reply.contains("bar") && reply.contains("block"), "{reply}");
    let (bar, block) = ("Grab bar", "Grab block");
    enter(app, bar);
    let rests = settled_all(app, &[block, bar]);
    let (block_rest, bar_rest) = (rests[0], rests[1]);
    // nothing is selected: Hang acts on every part, and on a strung piece it lifts the held bar
    button(app, "Hang").click();
    within_sim(app, "Hang to lift the bar", 5.0, || centre(app, Role::Button, bar).filter(|p| p.1 < bar_rest.1 - 30.0));
    within_sim(app, "the block to leave the bed", 5.0, || centre(app, Role::Button, block).filter(|p| p.1 < block_rest.1 - 3.0));
    // the bar stops once the block hangs clear; the block swings on under it (the air barely damps it)
    let start = settled(app, bar);
    let hung = centre(app, Role::Button, block).expect("the block's target");
    assert!(hung.1 > start.1 + 20.0, "the block hangs under the bar: bar {start:?}, block {hung:?}");
    let mut hold = app.press_at(start.0, start.1);
    let mut worst = 0.0_f64;
    let under = |want: (f64, f64), worst: &mut f64| {
        let p = centre(app, Role::Button, bar)?;
        *worst = worst.max(distance(p, want));
        Some(p)
    };
    for (dx, dy) in [(40.0, -10.0), (80.0, -20.0), (120.0, -30.0)] {
        hold.move_by(dx, dy);
        let want = (start.0 + dx, start.1 + dy);
        within_sim(app, "the bar to come under the pointer", 1.0, || centre(app, Role::Button, bar).filter(|p| distance(*p, want) <= 3.0));
        let t0 = sim_clock(app).expect("the sim clock");
        within_sim(app, "the bar to stay under the pointer", 1.0, || {
            under(want, &mut worst)?;
            (sim_clock(app)? - t0 >= 0.2).then_some(())
        });
    }
    let want = (start.0 + 120.0, start.1 - 30.0);
    let (mut across, mut up) = (false, false);
    within_sim(app, "the block to come along on its string, across and up", 3.0, || {
        under(want, &mut worst)?;
        let p = centre(app, Role::Button, block)?;
        across |= p.0 > hung.0 + 60.0;
        up |= p.1 < hung.1 - 20.0;
        (across && up).then_some(())
    });
    assert!(worst <= 3.0, "the carried bar left the pointer by {worst:.1} px");
    hold.release();
    let let_go = centre(app, Role::Button, bar).expect("the bar's target");
    let t0 = sim_clock(app).expect("the sim clock");
    within_sim(app, "the bar to stay where it was let go", 2.0, || {
        let p = centre(app, Role::Button, bar)?;
        assert!(distance(p, let_go) <= 1.0, "the bar moved after it was let go: {let_go:?} -> {p:?}");
        (sim_clock(app)? - t0 >= 0.5).then_some(())
    });
}

/// A joint's angle as the panel's joint list says it ("shoulder hinge  +60.0°  -90°..90°"), in degrees.
fn joint_angle(app: &TestApp, joint: &str) -> Option<f64> {
    let lines = app.widget_snapshot().nodes().into_iter().find(|w| w.visible() && w.node.name.contains(&format!("{joint} hinge")))?.node.name;
    let line = lines.lines().find(|l| l.starts_with(&format!("{joint} hinge")))?;
    line.split_whitespace().nth(2)?.trim_end_matches('°').parse().ok()
}

/// What the joint list reads of a hinge: the force through it (N), and what its motor pushes and at
/// most (N mm).
fn joint_readings(app: &TestApp, joint: &str) -> Option<(f64, f64, f64)> {
    let lines = app.widget_snapshot().nodes().into_iter().find(|w| w.visible() && w.node.name.contains(&format!("{joint} hinge")))?.node.name;
    let words: Vec<&str> = lines.lines().find(|l| l.starts_with(&format!("{joint} hinge")))?.split_whitespace().collect();
    let at = |word: &str, back: usize| words.iter().position(|w| *w == word).and_then(|i| words.get(i.checked_sub(back)?)?.parse().ok());
    let after = |word: &str, ahead: usize| words.iter().position(|w| *w == word).and_then(|i| words.get(i + ahead)?.parse().ok());
    Some((at("N", 1)?, after("motor", 1)?, after("motor", 3)?))
}

/// What a motor's row is set to, in degrees.
fn motor_set(app: &TestApp, row: &str) -> Option<f64> {
    let w = app.widget_snapshot().nodes().into_iter().find(|w| w.visible() && w.node.role == Role::Slider && w.node.name == row)?;
    w.node.value.trim_end_matches('°').parse().ok()
}

/// A servo's row shows in the panel, set where the arm printed. Its track dragged a third of the
/// way up sets about 60°: the servo turns the arm there (the joint list reads it within 2°, its
/// grab target moves) and holds it; a press in the track's middle sets 0° and the servo brings the
/// arm back where it printed.
fn motors(app: &TestApp) {
    let reply = load(app, SERVO, "mech-servo");
    assert!(reply.contains("arm"), "{reply}");
    let tip = "Grab base arm";
    let (servo, row) = ("shoulder", "shoulder motor");
    enter(app, tip);
    let track = Selector::role(Role::Slider).name(row);
    shown(app, track.clone()).assert_value("0°");
    let printed = settled(app, tip);
    let width = app.widget_snapshot().nodes().into_iter().find(|w| w.visible() && w.node.role == Role::Slider && w.node.name == row).expect("the servo's row").node.bounds.w as f64;
    let mut drag = app.locator(track.clone()).press_and_drag();
    drag.move_by(width / 3.0, 0.0);
    drag.release();
    let set = wait_for(app, "the row to take the drag", || motor_set(app, row).filter(|v| *v > 30.0 && *v < 90.0));
    within_sim(app, "the servo to turn the arm to its row's angle", 2.0, || joint_angle(app, servo).filter(|a| (a - set).abs() < 2.0));
    let turned = settled(app, tip);
    assert!(distance(turned, printed) > 3.0, "the arm's target did not move: {printed:?} -> {turned:?}");
    let held = joint_angle(app, servo).expect("the shoulder's angle");
    assert!((held - set).abs() < 2.0, "the servo did not hold the arm at {set}°: {held}°");
    // holding the arm off upright, the shoulder carries it and its servo pushes against its weight, within its limit
    let (load, effort, limit) = joint_readings(app, servo).expect("the shoulder's load and its motor's effort");
    assert!(load > 0.0 && effort > 0.0 && effort < limit && limit == 150.0, "the panel reads no load or effort for the held arm: {load} N, motor {effort} of {limit} N mm");
    app.locator(track).press_and_drag().release();
    wait_for(app, "the row to read 0°", || motor_set(app, row).filter(|v| v.abs() < 0.5));
    within_sim(app, "the servo to bring the arm back", 2.0, || joint_angle(app, servo).filter(|a| a.abs() < 2.0));
    let back = settled(app, tip);
    assert!(distance(back, printed) < 3.0, "the arm did not come back where it printed: {printed:?} -> {back:?}");
}

/// An SDF vase comes to rest on the ground, not through it.
fn vase(app: &TestApp) {
    let reply = load(app, include_str!("../resources/vase.cad.splash"), "mech-vase");
    assert!(reply.contains("vase"), "{reply}");
    let tip = "Grab vase";
    enter(app, tip);
    // The vase is its field, with no mesh standing in: a press finds nothing until the engine
    // draws it, and on a cold or loaded box the sim runs well before that.
    wait_for(app, "the engine to draw the vase", || parts_drawn(app).filter(|n| *n == 1));
    // Lift the vase and let it fall: it comes to rest on the ground, not through it. It may lie
    // on its side and have slid about the plate, but it is on the plate, not under it.
    let (rest, hold) = lift(app, tip, "the vase to lift", -140.0, 60.0);
    let held = settled(app, tip);
    assert!(held.1 < rest.1 - 60.0, "the held vase is not above its rest: {rest:?} -> {held:?}");
    hold.release();
    let landed = settled(app, tip);
    assert!(landed.1 < rest.1 + 80.0 && landed.1 > rest.1 - 80.0, "the SDF vase did not come to rest on the ground: {rest:?} -> {landed:?}");
}

/// A box beside chainmail joins the links' world, so it can be picked up (and the links collide
/// with it); the sim comes up with the links' verdict; Hang pins the chainmail's links and Drop
/// lets them go, with nothing selected (every part) and with the chainmail selected, and the
/// verdict keeps showing their interlocks. The links have no targets of their own: the panel
/// counts the pinned ones.
fn chainmail(app: &TestApp) {
    let reply = load(app, &format!("{MAIL}{OTHER}"), "mech-mail");
    assert!(reply.contains("mail") && reply.contains("other"), "{reply}");
    enter(app, "Grab mail");
    let named = |what: &str| app.widget_snapshot().nodes().into_iter().find(|w| w.visible() && w.node.name.contains(what)).map(|w| w.node.name);
    let verdict = wait_for(app, "the links' verdict", || named("interlocks"));
    assert!(verdict.contains("1 piece"), "{verdict}");
    // Hang pins the chainmail's links and Drop lets them go: with nothing selected (every part)
    // and with the chainmail selected (a press and release on it).
    let hang_and_drop = |selected: bool| {
        button(app, "Hang").click();
        wait_for(app, "Hang to pin the chainmail", || named("pinned"));
        assert_eq!(named("interlocks").as_deref(), Some(verdict.as_str()), "Hang changed the interlock verdict (selected: {selected})");
        button(app, "Drop").click();
        wait_for(app, "Drop to let the chainmail go", || named("pinned").is_none().then_some(()));
        assert_eq!(named("interlocks").as_deref(), Some(verdict.as_str()), "Drop changed the interlock verdict (selected: {selected})");
    };
    hang_and_drop(false);

    // The box beside the chainmail can be picked up and, held still, comes to hang still.
    let (_, hold) = lift(app, "Grab other", "the box beside the chainmail to lift", -120.0, 50.0);
    settled(app, "Grab other");
    hold.release();

    app.locator(Selector::role(Role::Button).name("Grab mail")).click_at_center();
    hang_and_drop(true);
}

/// Since `mark` the plate was sliced once, and the check was built from that slice (it plans
/// nothing of its own): one live input, one whole slice of the shelf, one check.
fn sliced_once(app: &TestApp, mark: u64, what: &str) {
    let lines = wait_for(app, &format!("the check {what} to be built"), || {
        let lines = app.log_since(mark).expect("the app's log").lines;
        lines.iter().any(|l| l.contains("fab loose: ") && l.contains(" pieces, ")).then_some(lines)
    });
    let count = |s: &str| lines.iter().filter(|l| l.contains(s)).count();
    let said: Vec<&String> = lines.iter().filter(|l| l.contains("fab live input:") || l.contains("fab live work:") || l.contains("fab loose:")).collect();
    said.iter().filter(|l| l.contains("fab loose:")).for_each(|l| say(format_args!("[app] {l}")));
    assert_eq!((count("fab live input: revision="), count("fab live work: part 0 "), count("fab loose: the plate's slice was whole")), (1, 1, 1), "the check {what} slices the plate once: {said:#?}");
}

/// The Supports toggle: a shelf's planned supports as a loose piece, from the plate's one slice.
/// The shelf rises off the plate and leaves it standing, it says it comes out, and a pull takes it
/// away. A change of shape starts it again, on that shape's one slice. Shake and Supports turn off
/// again.
fn supports(app: &TestApp) {
    let reply = load(app, SHELF, "mech-shelf");
    assert!(reply.contains("shelf"), "{reply}");
    let head = head_of(&text(&call(app, "inspect", "{}").expect("inspect")));
    let edits = Value::Str(r#"settings({enable_support: "1" support_threshold_angle: "45"})"#.to_string()).to_json();
    call(app, "place", &format!(r#"{{"request_id":"mech-shelf-settings","expected":{head:?},"edits":{edits}}}"#)).expect("settings");
    let tip = "Grab shelf";
    enter(app, tip);
    let toggle = |id: &str| Selector::id(id).name(if id == "shake" { "Shake" } else { "Supports" });
    let low = settled(app, tip);
    let mark = app.log_since(0).expect("the app's log").last_seq;
    shown(app, toggle("supports")).click();
    // Its row names the piece and says what the check found.
    let rows = || app.widget_snapshot().nodes().into_iter().find(|w| w.visible() && w.node.name == "Support pieces").map(|w| w.node.value);
    let row = wait_for(app, "the check to answer", || rows().filter(|v| v.contains(": ") && !v.contains("checking")));
    let (piece, words) = row.trim().split_once(": ").expect("a row");
    assert_eq!(words, "comes out whole", "{row}");
    sliced_once(app, mark, "on opening");
    let target = format!("Grab {piece}");
    let stands = wait_for(app, "the piece's grab target", || centre(app, Role::Button, &target));
    let raised = within_sim(app, "the shelf to rise off the plate", 5.0, || centre(app, Role::Button, tip).filter(|p| p.1 < low.1 - 10.0));
    let rests = settled_all(app, &[tip, &target]);
    let (high, left) = (rests[0], rests[1]);
    assert!(high.1 < raised.1 + 1.0, "the shelf went back down: {raised:?} -> {high:?}");
    assert!(distance(left, stands) < 10.0, "the piece came along: {stands:?} -> {left:?}");
    let mut hold = app.press_at(left.0, left.1);
    hold.move_by(150.0, 0.0);
    within_sim(app, "the piece to come away", 5.0, || centre(app, Role::Button, &target).filter(|p| distance(*p, left) > 60.0));
    hold.release();
    // Another shape starts the check again: the piece is back in the shelf.
    let mark = app.log_since(0).expect("the app's log").last_seq;
    load(app, &SHELF.replace("size: [2.5mm 12.5mm 10mm]", "size: [2.5mm 12.5mm 11mm]"), "mech-shelf-edited");
    wait_for(app, "the check to start again", || centre(app, Role::Button, &target).filter(|p| distance(*p, left) < 20.0));
    sliced_once(app, mark, "after the edit");
    for name in ["shake", "supports"] {
        for state in ["selected", ""] {
            if name == "supports" && state == "selected" {
                continue;
            }
            shown(app, toggle(name)).click();
            wait_for(app, &format!("{name} to go to {state:?}"), || {
                let on = app.widget_snapshot().nodes().into_iter().any(|w| w.visible() && w.node.name.to_lowercase() == name && w.checked() == Some(true));
                (on == !state.is_empty()).then_some(())
            });
        }
    }
    wait_for(app, "the pieces to go", || rows().is_none_or(|v| v.is_empty()).then_some(()));
}

/// One app's phases, in order.
type Phases = &'static [(&'static str, fn(&TestApp))];

/// An app of the group `name` (on a thread of that name), launched with `speed` as the speed
/// chosen before it started, runs `phases`.
fn run_app(name: &str, speed: &str, phases: Phases) -> Result<(), String> {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), format!("mechanics_{name}")).map_err(|e| e.to_string())?;
    // every wait counts the app's own time: a box loaded by the others slows it, it does not fail it
    config.concurrent = true;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let prefs = std::env::temp_dir().join(format!("fab-mech-{name}-{}-{nanos}.prefs", std::process::id()));
    std::fs::write(&prefs, format!("mech_speed={speed}\n")).map_err(|e| format!("the prefs file: {e}"))?;
    config.env.insert("FAB_VIEW_PREFS".into(), prefs.to_string_lossy().into_owned());
    let result = run_with_config(config, |app| {
        say(format_args!("app up (at {:.2} s)", since_start()));
        let saved = |speed: &str| wait_for(&app, &format!("the speed {speed} to be saved"), || std::fs::read_to_string(&prefs).ok()?.lines().any(|line| line == format!("mech_speed={speed}")).then_some(()));
        for (phase, run) in phases {
            let started = Instant::now();
            run(&app);
            say(format_args!("phase {phase}: {:.3} s wall (at {:.2} s)", started.elapsed().as_secs_f64(), since_start()));
            // the chain set 1x, the cube left 0.1x
            match *phase {
                "chain" => saved("1"),
                "cube" => saved("0.1"),
                _ => {}
            }
        }
    });
    let _ = std::fs::remove_file(&prefs);
    result.map_err(|e| e.to_string())
}

// The attribute launches before the test body and has no configuration hook: a configured run sets
// the speed chosen before the launch.
#[test]
fn mechanics_bodies_grab_throw_rest_drop_hang_and_the_speed_is_kept() {
    // Two apps side by side, each its own phases: the chain's opens at the speed chosen before it
    // started (0.1x) and the cube leaves 0.1x (last), the other opens at 1x. (Each app warms its
    // own JIT code: more apps than this repeat that work on a shared box more than they save.)
    let groups: [(&str, &str, Phases); 2] = [
        ("chain", "0.1", &[("chain", chain), ("reopen", reopen), ("vase", vase), ("motors", motors), ("cube", cube)]),
        ("supports", "1", &[("supports", supports), ("chainmail", chainmail), ("strings", strings)]),
    ];
    since_start();
    let runs: Vec<_> = groups.into_iter().map(|(name, speed, phases)| (name, std::thread::Builder::new().name(name.into()).spawn(move || run_app(name, speed, phases)).expect("a thread per app"))).collect();
    let failed: Vec<String> = runs.into_iter().filter_map(|(name, run)| match run.join() {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(format!("{name}: {e}")),
        Err(_) => Some(format!("{name}: panicked outside its app")),
    }).collect();
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

#[test]
fn throw_tracker_uses_the_time_window_and_ages_out_a_stopped_pointer() {
    let mut track = fab::VelocityTrack::default();
    for i in 0..=100 {
        let t = i as f64 * 0.001;
        track.push(t, makepad_widgets::vec3f((t * t) as f32, 0.0, 0.0));
    }
    // Even with more than eight samples, the window stays 40 ms: (0.1^2 - 0.06^2) / 0.04.
    assert!((track.velocity().x - 0.16).abs() < 1e-4);
    track.push(0.2, makepad_widgets::vec3f(0.01, 0.0, 0.0));
    assert!(track.velocity().length() < 1e-4);
}
