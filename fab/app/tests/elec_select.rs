//! A click on a part in the schematic selects it in place and nothing moves; its value is typed
//! in the inspector card; a double-click opens the projected view around it.
//!
//! Why (user, 11-18): "selecting each item in the schematic view drastically restructures the
//! schematic view", and "clicking something and changing the value of a resistor". A plain click
//! used to open the reveal: the sheet laid out again around the part and the camera flew to it.
//!
//! The Tone box demo in Electronics. Every part on screen is read by its semantic node, whose
//! bounds are its body's centre on screen: placement and camera together. Once the sheet has
//! settled, a click on the 39 k resistor must select it (the schematic says so, and never that it
//! shows the part's relations, which a reveal says the moment it starts) and leave every part
//! where it was. Typing 22k and Enter over the sheet sets the value through the card. Escape
//! clears the selection. Then the control: a double-click on the same part reveals it and the
//! parts move, so the first check could see a move.
//!
//! Then the 3D view rings what the other views select and selects what it is clicked on (user,
//! 11-18: "cross-select with Circuit/Board"): in Board + 3D the device view's recording must name
//! the resistor the schematic still has selected, and after a click on another part in the board,
//! that part. With its case off, a click on the resistor's body in the device view (seen from
//! the top) selects the resistor again.
//!
//! HAL offscreen (`MAKEPAD_HAL_BACKEND=offscreen`), native GPU, no window.

use makepad_strict_json::Value;
#[path = "common/workspace_row.rs"]
mod workspace_row;
use makepad_test::{run_with_config, KeyCode, NodeMatch, Role, Selector, TestApp, TestConfig};
use std::collections::BTreeMap;
use std::time::Duration;

/// The app main thread's CPU budget per wait; the runtime also detects idle hangs.
const HANG: Duration = Duration::from_secs(300);
/// Long enough for a camera flight or a reveal to show (both run for a few hundred ms).
const LOOK: Duration = Duration::from_millis(800);

fn wait_until(app: &TestApp, what: &str, mut ok: impl FnMut() -> bool) {
    app.try_wait_until_within(HANG, || (!ok()).then(|| format!("waiting for {what}")))
        .unwrap_or_else(|why| panic!("{why}; timed out waiting for {what}; the schematic says {:?}", schematic(app)));
}

/// The Schematic area's panel. Its dock tab is named after the area plus the notch's menu mark
/// ("Schematic ▾"); the name is matched by containment so the mark is not pinned here.
fn schematic_panel() -> Selector {
    Selector::raw("role=tab_panel name~=schematic")
}

/// What the schematic (the Schematic area's panel) says of itself: its parts, its nets, the
/// selected or revealed part.
fn schematic(app: &TestApp) -> String {
    app.locator(schematic_panel()).try_snapshot().map(|node| node.value.clone()).unwrap_or_default()
}

/// Every part on screen by its refdes: its semantic node (centre bounds, value). The schematic
/// makes part nodes only for a query naming them, and every refdes holds a digit. A read is a
/// count and a query a node, so one on a busy box can miss a part: compare reads [`settled`].
fn parts(app: &TestApp) -> BTreeMap<String, NodeMatch> {
    let mut out = BTreeMap::new();
    for digit in '0'..='9' {
        let query = || Selector::role(Role::Button).text_contains(digit.to_string());
        for i in 0..app.locator(query()).try_count().unwrap_or(0) {
            if let Ok(node) = app.locator(query().nth(i)).try_snapshot() {
                if node.role_description == "part" {
                    out.insert(node.name.clone(), node);
                }
            }
        }
    }
    out
}

fn places(parts: &BTreeMap<String, NodeMatch>) -> BTreeMap<String, (i64, i64)> {
    parts.iter().map(|(name, node)| (name.clone(), (node.x(), node.y()))).collect()
}

/// The parts' places once the sheet has come to rest (its assembly at load, a reveal's flight).
fn settled(app: &TestApp) -> BTreeMap<String, NodeMatch> {
    let mut last = BTreeMap::new();
    let mut found = BTreeMap::new();
    wait_until(app, "the sheet at rest", || {
        std::thread::sleep(LOOK / 2);
        found = parts(app);
        let now = places(&found);
        let rest = now.len() >= 2 && now == last;
        last = now;
        rest
    });
    found
}

fn moved(before: &BTreeMap<String, (i64, i64)>, after: &BTreeMap<String, (i64, i64)>) -> Vec<String> {
    let mut out: Vec<String> = before.iter().filter(|(name, at)| after.get(*name) != Some(at))
        .map(|(name, at)| format!("{name} {at:?} -> {:?}", after.get(name))).collect();
    out.extend(after.keys().filter(|name| !before.contains_key(*name)).map(|name| format!("{name} came into view")));
    out
}

/// The parts the device view's recording rings as the selection (`FabViewport::semantics`), each
/// with where its body's centre shows (window points), or None before it has recorded.
fn ringed_at(app: &TestApp) -> Option<Vec<(String, Option<(f64, f64)>)>> {
    let node = app.locator(Selector::id("device_viewport")).try_snapshot().ok()?;
    let report = makepad_strict_json::parse_depth(node.description.as_bytes(), 8).ok()?;
    let num = |v: &Value| match v {
        Value::F64(f) => Some(*f),
        Value::Int(i) => Some(*i as f64),
        _ => None,
    };
    let list = report.get("selected").and_then(Value::as_arr)?;
    Some(list.iter().filter_map(|part| {
        let at = match part.get("at").and_then(Value::as_arr) { Some([x, y]) => num(x).zip(num(y)), _ => None };
        Some((part.get("name")?.as_str()?.to_string(), at))
    }).collect())
}

fn ringed(app: &TestApp) -> Option<Vec<String>> {
    ringed_at(app).map(|parts| parts.into_iter().map(|(name, _)| name).collect())
}

fn part(app: &TestApp, refdes: &str) -> makepad_test::Locator {
    app.locator(Selector::role(Role::Button).name(refdes))
}

#[test]
fn a_click_selects_a_part_in_place_and_its_value_is_typed_in_the_card() {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "elec_select").unwrap();
    config.env.insert("MAKEPAD_HAL_BACKEND".into(), "offscreen".into());
    config.app_args = vec!["--window".into(), "1400x900@1".into()];
    // The test's own view preferences: Electronics offered (whatever the box's own say), no code
    // panel.
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("fab-elec-select-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&dir).expect("a fresh folder for the test's preferences");
    let prefs = dir.join("view.prefs");
    std::fs::write(&prefs, "code=0\n").expect("the test's view preferences");
    config.env.insert("FAB_VIEW_PREFS".into(), prefs.display().to_string());
    let result = run_with_config(config, |app: TestApp| {
        app.locator(Selector::role(Role::Window)).wait_visible();
        app.locator(Selector::role(Role::Tab).name("Plates")).wait_visible().click();
        // The demo far down the list may be out of view: a semantic press needs no scroll.
        app.try_wait_until_within(HANG, || app.locator(Selector::role(Role::ListItem).name("Tone box")).try_click().err().map(|e| e.to_string()))
            .unwrap_or_else(|why| panic!("open the Tone box demo: {why}"));
        // The Schematic workspace: the schematic over the scope.
        workspace_row::press(&app, "Schematic");
        app.locator(schematic_panel()).wait_visible();
        wait_until(&app, "the tone box's schematic", || !schematic(&app).is_empty() && !schematic(&app).starts_with("0 parts"));
        let before = settled(&app);
        let resistor = before.values().find(|node| node.name.starts_with('R') && node.value.contains("39k"))
            .unwrap_or_else(|| panic!("the tone box's 39 k resistor on screen: {:?}", before.values().map(|n| (&n.name, &n.value)).collect::<Vec<_>>()))
            .name.clone();
        let before = places(&before);

        // A click selects in place: the schematic says so at once, and never that it reveals.
        part(&app, &resistor).click_at_center();
        wait_until(&app, "the resistor selected", || schematic(&app).contains(&format!("{resistor} 39k selected")));
        std::thread::sleep(LOOK);
        let said = schematic(&app);
        assert!(!said.contains("relations"), "a click selects; the projected view waits for a double-click: {said:?}");
        // two like reads, as `before` was: a read a busy box answered short is no move
        let after = places(&settled(&app));
        let shifted = moved(&before, &after);
        assert!(shifted.is_empty(), "a selection moves no part and not the camera: {shifted:?}");

        // Typed over the sheet, the value goes into the card's field; Enter sets it.
        app.type_text("22k");
        app.press_return();
        wait_until(&app, "the resistor's new value", || schematic(&app).contains(&format!("{resistor} 22k selected")));

        // Escape clears the selection.
        app.press_key(KeyCode::Escape);
        wait_until(&app, "the selection cleared", || !schematic(&app).contains("selected"));

        // The control: a double-click reveals the part, and then the parts do move.
        let start = places(&settled(&app));
        part(&app, &resistor).click_at_center();
        part(&app, &resistor).click_at_center();
        wait_until(&app, "the projected view", || schematic(&app).contains(&format!("showing {resistor}'s relations")));
        wait_until(&app, "the projected view's moves", || !moved(&start, &places(&parts(&app))).is_empty());

        // The Board + 3D workspace: the device view rings the part the schematic selected, then
        // the one a click on the board selects.
        workspace_row::press(&app, "Board + 3D");
        wait_until(&app, "the device view ringing the schematic's selection", || ringed(&app) == Some(vec![resistor.clone()]));
        // Case off and seen from the top, where the resistor's body shows in the device view.
        app.locator(Selector::role(Role::ToggleButton).name("Case")).wait_visible().press();
        app.locator(Selector::role(Role::Button).name("Top")).wait_visible().press();
        let mut resistor_at = None;
        wait_until(&app, "the resistor's body still on screen in the device view", || {
            let at = ringed_at(&app).and_then(|parts| parts.first().and_then(|p| p.1));
            let still = at.is_some() && at == resistor_at;
            resistor_at = at;
            if !still { std::thread::sleep(LOOK); }
            still
        });
        let board = settled(&app);
        let other = board.keys().find(|name| **name != resistor && !name.starts_with('R'))
            .unwrap_or_else(|| panic!("another part on the board: {:?}", board.keys().collect::<Vec<_>>())).clone();
        part(&app, &other).click_at_center();
        wait_until(&app, "the ring following the board's selection", || ringed(&app) == Some(vec![other.clone()]));

        // A click on the resistor's body in the device view selects it again.
        let (x, y) = resistor_at.expect("the resistor's place in the device view");
        app.press_at(x, y).release();
        wait_until(&app, "the device view's click selecting the resistor", || ringed(&app) == Some(vec![resistor.clone()]));
    });
    let cleanup = std::fs::remove_dir_all(&dir);
    result.unwrap();
    cleanup.expect("remove the test's temporary folder");
}
