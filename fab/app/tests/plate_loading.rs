//! Another plate loading shows on the 3D view's progress card, over the plate still on the bed,
//! which stays usable under it, in one session driven through the semantic tree. The loads are
//! held (`hold-loads on`, the app's remote command): each stops once its build has made its first
//! step, its bar past 0, so the test acts on a load that is deterministically still on its way,
//! on small plates:
//!
//! - a demo clicked in the Plates tab shows "Loading <its name>" in the view once it has run 0.3 s,
//!   never on the caption's job strip, with a Cancel button; its bar moves on to the hold, where it
//!   stays, past 0 and inside the build's half, naming the build's step;
//! - the plate under the card still zooms by the wheel while it loads;
//! - another demo clicked while it loads takes the card over at once;
//! - Cancel stops it: the card goes and the plate on the bed is the one that was there, seen
//!   as it was; each load cancelled (the one taken over, the one Cancel stopped) ended in the step
//!   it was held in, none begun after its cancel;
//! - all the while a load is on its way the app is not `settled` (its `loads` name the build); let
//!   go (`hold-loads off`), the load runs to its end with its bar only moving on, puts its plate on
//!   the bed and takes the card down, and the first `settled` after it finds the new plate on the
//!   bed with its parts drawn: an agent that waits for `settled` never looks at the plate before
//!   (a 31.5 s build read as settled, 11-16).
//!
//! It prints the timings and the cards seen (`plate loading: ...`), and how long each cancelled
//! load built on after its cancel as the jobs board logs it (for people: timings are not checked).
//!
//! HAL offscreen, native GPU, no window.

#[path = "common/wait.rs"]
mod idle_wait;
use idle_wait::wait_poll;
use makepad_strict_json::Value;
use makepad_test::{run_with_config, Role, Selector, TestApp, TestConfig};
use std::time::{Duration, Instant};

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);

fn call_on(endpoint: &str, tool: &str, args: &str) -> Result<String, String> {
    let client = makepad_remote::Client::connect(endpoint).map_err(|e| format!("connect: {e:?}"))?;
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

/// The app's `settled` answer: whether nothing is pending, and the answer line.
fn settled(app: &TestApp) -> (bool, String) {
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).expect("connect to the app's remote");
    let mut lines = Vec::new();
    makepad_remote::app::run(&client, &["settled".to_string()], &mut |line| {
        lines.push(line.to_string());
        true
    })
    .expect("settled");
    let line = lines.into_iter().find(|l| l.contains("\"settled\":")).expect("a settled answer");
    (line.contains("\"settled\":true"), line)
}

/// The document's inspect reply, once the app's tools answer.
fn inspect(app: &TestApp) -> String {
    wait_poll(app, || {
        call_on(&app.remote_endpoint(), "inspect", "{}").map(|line| text(&line)).map_err(|e| format!("the fab tools never appeared: {e}"))
    })
}

fn head(app: &TestApp) -> String {
    let reply = makepad_script::syntax::parse_value(&inspect(app)).expect("a Splash reply");
    reply.field("head").and_then(|f| f.value.string()).expect("reply has a head").to_string()
}

/// What the viewport reports of the recording on display.
fn recording(app: &TestApp) -> Value {
    let viewport = app.locator(Selector::id("viewport")).snapshot();
    makepad_strict_json::parse_depth(viewport.description.as_bytes(), 8).expect("viewport inspection JSON")
}

fn camera(app: &TestApp) -> String {
    recording(app).get("camera").expect("viewport camera").to_json()
}

/// The camera once the view is at rest (no glide or bake left): its own rest (`at_rest`), as a
/// load held on its way keeps it from `settled`, which waits for the scene coming too.
fn settled_camera(app: &TestApp) -> String {
    wait_until(app, "the view at rest", || recording(app).get("at_rest").and_then(Value::as_bool) == Some(true));
    camera(app)
}

fn wait_until(app: &TestApp, what: &str, mut ok: impl FnMut() -> bool) {
    app.try_wait_until_within(HANG, || (!ok()).then(|| format!("waiting for {what}"))).unwrap_or_else(|why| {
        panic!("{why}; timed out waiting for {what}; the window shows {:?}", names(app));
    });
}

/// The window's title: the plate on the bed's name, "Marble block · Fab".
#[track_caller]
fn title(app: &TestApp) -> String {
    let names = names(app);
    let at = std::panic::Location::caller();
    names.iter().find(|n| n.ends_with(" · Fab")).cloned().unwrap_or_else(|| panic!("no window title at {at} among the window's names {names:?}"))
}

/// The visible names of the window's nodes.
fn names(app: &TestApp) -> Vec<String> {
    app.widget_snapshot().nodes().into_iter().filter(|w| w.visible()).map(|w| w.node.name).collect()
}

/// The progress card's words, when it is on show.
#[derive(Clone, Debug, Default, PartialEq)]
struct Card {
    title: String,
    phase: String,
    /// "37% · 4 s"
    elapsed: String,
    button: String,
}

impl Card {
    fn percent(&self) -> Option<u32> {
        self.elapsed.split('%').next()?.trim().parse().ok()
    }
}

fn card(app: &TestApp) -> Option<Card> {
    let nodes: Vec<_> = app.widget_snapshot().nodes().into_iter().filter(|w| w.visible() && w.node.path.contains("progress_card") && !w.node.name.is_empty()).collect();
    let mut card = Card::default();
    for w in &nodes {
        let name = w.node.name.clone();
        if w.node.role == Role::Button {
            card.button = name;
        } else if name.starts_with("Loading ") || name.ends_with(" did not load") {
            card.title = name;
        } else if name.contains("% · ") {
            card.elapsed = name;
        } else {
            card.phase = name;
        }
    }
    (!card.title.is_empty()).then_some(card)
}

fn press_plate(app: &TestApp, name: &str) -> Instant {
    let at = Instant::now();
    // a semantic press: the row may be scrolled out of the panel
    app.locator(Selector::role(Role::ListItem).name(name)).press();
    at
}

/// A load stopped at the hold, as `hold-loads` tells it: its bar's percent as the card shows it,
/// and its step's words.
#[derive(Debug, PartialEq)]
struct Waiting {
    percent: u32,
    stage: String,
}

/// The app's `hold-loads` answer to `word` (on, off, or none: a look), its line and its JSON.
fn hold_answer(app: &TestApp, word: Option<&str>) -> (String, Value) {
    let client = makepad_remote::Client::connect(&app.remote_endpoint()).expect("connect to the app's remote");
    let argv: Vec<String> = std::iter::once("hold-loads").chain(word).map(str::to_string).collect();
    let mut lines = Vec::new();
    makepad_remote::app::run(&client, &argv, &mut |line| {
        lines.push(line.to_string());
        true
    })
    .expect("hold-loads");
    let line = lines.into_iter().find(|l| l.contains("\"hold\":")).expect("a hold-loads answer");
    let v = makepad_strict_json::parse_depth(line.as_bytes(), 8).expect("hold-loads answers JSON");
    (line, v)
}

/// A load the hold held since it went on, as `hold-loads` tells it: its job's title, its step's
/// words at the hold and now, and how it ended.
#[derive(Debug)]
struct Held {
    title: String,
    held_in: String,
    now_in: String,
    end: String,
}

/// The loads the hold held since it went on.
fn loads_held(app: &TestApp) -> Vec<Held> {
    let (line, v) = hold_answer(app, None);
    let Some(Value::Arr(held)) = v.get("held") else { panic!("no loads held in {line}") };
    let word = |h: &Value, k: &str| h.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    held.iter().map(|h| Held { title: word(h, "title"), held_in: word(h, "held_in"), now_in: word(h, "now_in"), end: word(h, "end") }).collect()
}

/// The build step a stage's words name (what runs in the step follows it after a " · ").
fn step(words: &str) -> &str {
    words.split(" · ").next().unwrap_or_default()
}

/// The app's `hold-loads` answer to `word` (on, off, or none: a look): whether the hold is on, and
/// the loads waiting at it.
fn hold_loads(app: &TestApp, word: Option<&str>) -> (bool, Vec<Waiting>) {
    let (line, v) = hold_answer(app, word);
    let Some(Value::Arr(waiting)) = v.get("waiting") else { panic!("no loads waiting in {line}") };
    let waiting = waiting.iter().map(|w| {
        // the card's words for the fraction: its f32 to a whole percent
        let at = match w.get("at") {
            Some(Value::F64(f)) => *f as f32,
            Some(Value::Int(i)) => *i as f32,
            _ => panic!("a waiting load without its bar: {line}"),
        };
        let percent = format!("{:.0}", at * 100.0).parse().unwrap();
        Waiting { percent, stage: w.get("stage").and_then(Value::as_str).unwrap_or_default().to_string() }
    });
    (v.get("hold").and_then(Value::as_bool).expect("hold"), waiting.collect())
}

/// The cards seen until `done` (or the card goes): each distinct one, and when it was first seen.
fn watch(app: &TestApp, since: Instant, mut done: impl FnMut(&Card) -> bool) -> Vec<(Duration, Card)> {
    let mut seen: Vec<(Duration, Card)> = Vec::new();
    wait_poll(app, || {
        let Some(now) = card(app) else { return Ok(std::mem::take(&mut seen)) };
        if seen.last().is_none_or(|(_, c)| *c != now) {
            seen.push((since.elapsed(), now.clone()));
        }
        if done(&now) {
            return Ok(std::mem::take(&mut seen));
        }
        Err(format!("the load never reached the hold: {seen:?}"))
    })
}

/// A load's card up to the hold: the load waits there, and its card shows the bar and the step
/// the hold names. The cards on the way, the last one the held one's.
fn to_the_hold(app: &TestApp, name: &str, since: Instant) -> Vec<(Duration, Card)> {
    wait_until(app, "the load's card", || card(app).is_some_and(|c| c.title == format!("Loading {name}")));
    let at_hold = |c: &Card| matches!(hold_loads(app, None).1.as_slice(), [w] if c.percent() == Some(w.percent) && c.phase.to_lowercase() == w.stage.to_lowercase());
    let seen = watch(app, since, at_hold);
    let held = &seen.last().expect("the card at the hold").1;
    assert!(at_hold(held), "the card went before its load reached the hold: {seen:?}");
    assert_eq!(held.title, format!("Loading {name}"), "another card at the hold: {seen:?}");
    let percents: Vec<u32> = seen.iter().filter_map(|(_, c)| c.percent()).collect();
    assert!(percents.windows(2).all(|w| w[0] <= w[1]), "the bar ran back on the way to the hold: {percents:?}");
    let at = held.percent().expect("the held card's percent");
    assert!(at > 0 && at < 50, "the hold's bar is not past 0 inside the build's half: {held:?}");
    let phase = held.phase.to_lowercase();
    assert!(["building", "measuring the parts", "laying out the plate"].iter().any(|w| phase.starts_with(w)), "the held card names no build step: {held:?}");
    seen
}

/// The demos clicked: two that are held and never built, and the small one let through.
const FIRST: &str = "Ball in a cage";
const SECOND: &str = "Bistable switch";
const LAST: &str = "Marble block";

fn loading(app: TestApp) {
    app.locator(Selector::role(Role::Window)).wait_visible();
    // the plate on the bed, at rest
    let first = head(&app);
    wait_until(&app, "the first plate's card gone", || card(&app).is_none());
    let first_title = title(&app);
    settled_camera(&app);
    app.locator(Selector::role(Role::Tab).name("Plates")).click();
    // a line with the load's job title: the strip's, or the view's rebuilding words
    let strip_has = |app: &TestApp, words: &str| names(app).iter().any(|n| n.contains(&format!("Opening {words}")));
    // from here each load stops at the hold
    assert_eq!(hold_loads(&app, Some("on")), (true, Vec::new()), "the hold did not go on, or a load waits at it");

    // ---- a load: the card shows after 0.3 s, never the strip, with Cancel; its bar to the hold ----
    let pressed = press_plate(&app, FIRST);
    wait_until(&app, "the first load's card", || card(&app).is_some_and(|c| c.title == format!("Loading {FIRST}")));
    let shown_after = pressed.elapsed();
    assert!(shown_after >= Duration::from_millis(300), "the card showed before 0.3 s: {shown_after:?}");
    assert!(!strip_has(&app, FIRST), "the load is on the strip: {:?}", names(&app));
    let first_load = to_the_hold(&app, FIRST, pressed);
    let held = first_load.last().unwrap().1.clone();
    assert_eq!(held.button, "Cancel", "the card has no Cancel: {held:?}");
    assert!(!strip_has(&app, FIRST), "the load is on the strip: {:?}", names(&app));

    // ---- the plate under it still zooms ----
    let was = camera(&app);
    let view = app.locator(Selector::id("viewport")).snapshot().bounds;
    let wheeled = Instant::now();
    app.scroll_at((view.x + view.w * 0.15) as f64, (view.y + view.h * 0.5) as f64, 0.0, -120.0, None);
    wait_until(&app, "the wheel's zoom under the card", || camera(&app) != was);
    let zoomed_in = wheeled.elapsed();
    assert_eq!(card(&app).and_then(|c| c.percent()), held.percent(), "the held load's bar moved");

    // ---- another demo takes the card over, to the hold in its turn ----
    let asked = press_plate(&app, SECOND);
    wait_until(&app, "the second load's card", || card(&app).is_some_and(|c| c.title == format!("Loading {SECOND}")));
    let taken_over = asked.elapsed();
    to_the_hold(&app, SECOND, asked);

    // ---- Cancel: the plate on the bed stays as it was ----
    let before = settled_camera(&app);
    let cancelled = Instant::now();
    app.locator(Selector::role(Role::Button).name("Cancel")).wait_visible().press();
    wait_until(&app, "the card gone after Cancel", || card(&app).is_none());
    let stopped = cancelled.elapsed();
    assert_eq!(head(&app), first, "Cancel changed the document");
    assert_eq!(title(&app), first_title, "a cancelled load is on the bed");
    assert_eq!(settled_camera(&app), before, "Cancel moved the view");
    wait_until(&app, "the cancelled load gone from the hold", || hold_loads(&app, None) == (true, Vec::new()));
    // each load cancelled ended in the step it was held in: none began after its cancel
    wait_until(&app, "the cancelled loads ended", || loads_held(&app).iter().filter(|h| h.end != "running").count() == 2);
    let held_loads = loads_held(&app);
    for name in [FIRST, SECOND] {
        let h = held_loads.iter().find(|h| h.title == format!("Opening {name}")).unwrap_or_else(|| panic!("the hold never held {name}: {held_loads:?}"));
        assert_eq!(h.end, "cancelled", "the load of {name} did not end cancelled: {h:?}");
        assert_eq!(step(&h.now_in), step(&h.held_in), "the load of {name} began a step after its cancel: {h:?}");
    }

    // ---- a load let go: its bar only moves on, its plate on the bed, the card down ----
    let pressed = press_plate(&app, LAST);
    let last_load = to_the_hold(&app, LAST, pressed);
    let held_last = last_load.last().unwrap().1.clone();
    // while it is on its way, `settled` says so, each time it is asked, and the bar stays
    for _ in 0..3 {
        let (now, line) = settled(&app);
        assert!(!now && line.contains("\"build\":"), "settled, or no build named, while a load was on its way: {line}");
        assert_eq!(card(&app).and_then(|c| c.percent()), held_last.percent(), "the held load's bar moved");
    }
    let released = Instant::now();
    assert!(!hold_loads(&app, Some("off")).0, "the hold stayed on");
    let after = watch(&app, released, |_| false);
    let percents: Vec<u32> = std::iter::once(&held_last).chain(after.iter().map(|(_, c)| c)).filter_map(Card::percent).collect();
    assert!(percents.windows(2).all(|w| w[0] <= w[1]), "the bar ran back after the hold: {percents:?}");
    // the first settled answer after it: the plate on the bed, drawn
    wait_until(&app, "the app settled after the load", || settled(&app).0);
    assert_eq!(title(&app), format!("{LAST} · Fab"), "settled before the plate was on the bed");
    let drawn = recording(&app).get("parts_drawn").and_then(Value::as_u64).expect("viewport parts_drawn");
    assert!(drawn > 0, "settled before the plate's parts were drawn");
    let loaded = released.elapsed();
    assert!(card(&app).is_none(), "the card stayed up after the load");

    println!("plate loading: card shown after {:.0} ms; wheel zoom under it in {:.0} ms; the second load took the card over in {:.0} ms; Cancel took the card down in {:.0} ms; the last load on the bed and settled {:.0} ms after it was let go, {drawn} parts drawn", shown_after.as_secs_f64() * 1000.0, zoomed_in.as_secs_f64() * 1000.0, taken_over.as_secs_f64() * 1000.0, stopped.as_secs_f64() * 1000.0, loaded.as_secs_f64() * 1000.0);
    for (what, seen) in [("first load to the hold", &first_load), ("last load to the hold", &last_load), ("last load let go", &after)] {
        for (at, c) in seen {
            println!("plate loading: {what} {:>6.0} ms  {} | {} | {}", at.as_secs_f64() * 1000.0, c.phase, c.elapsed, c.button);
        }
    }
}

#[test]
fn a_plate_loading_shows_on_the_view_cancels_and_leaves_the_bed_usable() {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "a_plate_loading_shows_on_the_view_cancels_and_leaves_the_bed_usable").unwrap();
    // when the card shows and each of its steps, seen to 15 ms: its waits look that often
    config.poll_interval = Duration::from_millis(15);
    let stderr = config.artifacts_dir.join("app-stderr.txt");
    if let Err(e) = run_with_config(config, loading) {
        panic!("{}", e.message());
    }
    // How long each cancelled load built on after its cancel, as the jobs board logs it ("Opening
    // <plate> stopped <ms> ms after cancel"): for people, the wall clock is not checked.
    let log = std::fs::read_to_string(&stderr).unwrap_or_default();
    let ran_on = |plate: &str| log.lines().find_map(|l| l.split(&format!("Opening {plate} stopped ")).nth(1)?.strip_suffix(" ms after cancel")?.parse::<f64>().ok());
    println!("plate loading: the load taken over stopped {:?} ms after its cancel, the one cancelled {:?} ms after Cancel", ran_on(FIRST), ran_on(SECOND));
}
