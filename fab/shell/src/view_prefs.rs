//! The view's preferences kept per machine: the part look (View › Shading:
//! Field or Mesh), the shading (Solid or Realtime), the lighting and the overlays, whether the view
//! shows the slice, its projection, how fine a drag draws (View › Drag Detail), and the experimental
//! workspaces offered (View › Experimental), as the person chose them last. A GPU's answer belongs
//! to the machine, not to a document. Every run reads them as it starts, so a restart in place
//! brings them back as a new session does.
//!
//! `~/.makepad/fab/view.prefs` (or `$FAB_VIEW_PREFS`; a unit test's own fresh file), one `key=value` per
//! line. No file: nothing was chosen; the field look starts, unless the
//! build is the web's (meshes) or the GPU cannot hold its budget (the
//! viewport then shows meshes and says how to turn the field back on).
//!
//! A lane's test instance (`MAKEPAD_TEST_LANE`) is not the person: it neither
//! reads nor keeps the machine's choice, so a test that picks Mesh leaves no
//! Mesh behind for the next run on that box.

use crate::api::{Lighting, MechContact, Overlays, PartLook, Shading, SlicedDetail, TracedDetail};
use fab_sdf_tiles::trace::DragDetail;
use makepad_math::makepad_micro_serde::{DeJson, SerJson};
use std::path::PathBuf;

fn path() -> Option<PathBuf> {
    // A unit test reads and keeps only its own file, never the machine's or another test's: tests
    // run side by side in one process, and a warm live session runs them again in that process.
    if cfg!(test) {
        return test_path();
    }
    if let Some(p) = std::env::var_os("FAB_VIEW_PREFS") {
        return Some(PathBuf::from(p));
    }
    if std::env::var_os("MAKEPAD_TEST_LANE").is_some() {
        return None;
    }
    Some(makepad_fab_settings::home::user_home().join(".makepad").join("fab").join("view.prefs"))
}

#[cfg(test)]
fn test_path() -> Option<PathBuf> {
    tests::PREFS.with(|p| p.borrow().clone())
}

#[cfg(not(test))]
fn test_path() -> Option<PathBuf> {
    None
}

fn parse_look(s: &str) -> Option<PartLook> {
    match s.trim() {
        "Field" => Some(PartLook::Field),
        "Mesh" => Some(PartLook::Mesh),
        _ => None,
    }
}

fn parse_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
}

fn parse_code(text: &str) -> (bool, Option<f64>) {
    (
        parse_value(text, "code") != Some("0"),
        parse_value(text, "code_width").and_then(|w| w.parse().ok()).filter(|w: &f64| *w > 0.0),
    )
}

/// Every machine view and workspace preference loaded at startup.
#[derive(Clone, Debug)]
pub struct ViewPrefs {
    pub look: Option<PartLook>,
    pub drag_detail: DragDetail,
    pub sliced_detail: SlicedDetail,
    pub traced_detail: TracedDetail,
    pub soldered: bool,
    pub lighting: Option<Lighting>,
    pub board_lighting: Option<Lighting>,
    pub colours: Option<fab_plate::ColourMode>,
    pub shading: Option<Shading>,
    pub overlays: Option<Overlays>,
    pub slice: bool,
    pub surface: bool,
    pub nav: crate::api::NavScheme,
    pub invert_wheel: bool,
    pub part_side_w: Option<f64>,
    pub arm: bool,
    pub scan: bool,
    pub workspaces: crate::workspaces::Workspaces,
    pub scan_colour: bool,
    pub scan_cameras: bool,
    pub ortho: bool,
}

impl ViewPrefs {
    /// Parses every startup view preference from `view.prefs` text with pre-built `workspaces`.
    pub fn from_text(text: &str, workspaces: crate::workspaces::Workspaces) -> Self {
        let v = |k: &str| parse_value(text, k);
        Self {
            look: v("look").and_then(parse_look),
            drag_detail: parse_drag_detail(v("drag_detail")),
            sliced_detail: parse_sliced_detail(v("sliced_detail")),
            traced_detail: parse_traced_detail(v("traced_detail")),
            soldered: parse_soldered(v("soldered")),
            lighting: v("lighting").and_then(Lighting::from_key),
            board_lighting: v("board_lighting").and_then(Lighting::from_key),
            colours: v("colours").and_then(fab_plate::ColourMode::from_key),
            shading: v("shading").and_then(Shading::from_key),
            overlays: v("overlays").map(parse_overlays),
            slice: v("slice") == Some("1"),
            surface: v("surface") == Some("1"),
            nav: v("nav").and_then(crate::api::NavScheme::from_key).unwrap_or_default(),
            invert_wheel: v("invert_wheel") == Some("1"),
            part_side_w: v("part_side_w").and_then(parse_part_side_w),
            arm: v("arm") == Some("1"),
            scan: v("scan") == Some("1"),
            workspaces,
            scan_colour: v("scan_colour") != Some("0"),
            scan_cameras: v("scan_cameras") != Some("0"),
            ortho: v("ortho") == Some("1"),
        }
    }

    /// Reads `view.prefs` (at most once) and `view.workspaces.ron` (at most once).
    pub fn load() -> Self {
        let text = read_prefs().unwrap_or_default();
        let code = parse_code(&text);
        let workspaces = crate::workspaces::Workspaces::load_with_code(code);
        Self::from_text(&text, workspaces)
    }
}

fn read_prefs() -> Option<String> {
    if cfg!(target_arch = "wasm32") {
        return None;
    }
    std::fs::read_to_string(path()?).ok()
}

fn load(key: &str) -> Option<String> {
    parse_value(&read_prefs()?, key).map(str::to_owned)
}

fn save(key: &str, value: &str) {
    if cfg!(target_arch = "wasm32") {
        return;
    }
    let Some(p) = path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let start = format!("{key}=");
    let text = std::fs::read_to_string(&p).unwrap_or_default();
    let mut out: String = text.lines().filter(|l| !l.starts_with(start.as_str())).flat_map(|l| [l, "\n"]).collect();
    out.push_str(&format!("{key}={value}\n"));
    let _ = std::fs::write(&p, out);
}

/// The look chosen on this machine, if any.
pub fn load_look() -> Option<PartLook> {
    load("look").as_deref().and_then(parse_look)
}

/// Keeps the person's choice of look for the next start.
pub fn save_look(look: PartLook) {
    save("look", look.label());
}

/// The view showed the slice when last chosen on this machine.
pub fn load_slice() -> bool {
    load("slice").as_deref() == Some("1")
}

/// Design's code split: shown, and its width (None: the starting one).
pub fn load_code() -> (bool, Option<f64>) {
    parse_code(&read_prefs().unwrap_or_default())
}

/// Keeps the code split's state and width for the next start.
pub fn save_code(shown: bool, width: Option<f64>) {
    save("code", if shown { "1" } else { "0" });
    if let Some(w) = width {
        save("code_width", &format!("{w:.0}"));
    }
}

/// Normal shows the printed surface (the layer lines).
pub fn load_surface() -> bool {
    load("surface").as_deref() == Some("1")
}

pub fn save_surface(on: bool) {
    save("surface", if on { "1" } else { "0" });
}

/// Keeps whether the view shows the slice for the next start.
pub fn save_slice(on: bool) {
    save("slice", if on { "1" } else { "0" });
}

/// How the 3D views take the mouse (the caption's Settings › 3D navigation), as the person chose.
pub fn load_nav() -> crate::api::NavScheme {
    load("nav").and_then(|k| crate::api::NavScheme::from_key(&k)).unwrap_or_default()
}

pub fn save_nav(scheme: crate::api::NavScheme) {
    save("nav", scheme.key());
}

/// The Part sidebar's width, as the person dragged it.
pub fn load_part_side_w() -> Option<f64> {
    load("part_side_w").as_deref().and_then(parse_part_side_w)
}

fn parse_part_side_w(value: &str) -> Option<f64> {
    value.trim().parse::<f64>().ok().filter(|w| w.is_finite())
}

pub fn save_part_side_w(width: f64) {
    save("part_side_w", &format!("{width:.0}"));
}

/// The wheel zooms the other way (Settings › Invert zoom wheel).
pub fn load_invert_wheel() -> bool {
    load("invert_wheel").as_deref() == Some("1")
}

pub fn save_invert_wheel(on: bool) {
    save("invert_wheel", if on { "1" } else { "0" });
}

/// The view is orthographic (View › Orthographic), as the person left it.
pub fn load_ortho() -> bool {
    load("ortho").as_deref() == Some("1")
}

pub fn save_ortho(on: bool) {
    save("ortho", if on { "1" } else { "0" });
}

/// The experimental Arm workspace is offered (View › Experimental › Arm).
pub fn load_arm() -> bool {
    load("arm").as_deref() == Some("1")
}

pub fn save_arm(on: bool) {
    save("arm", if on { "1" } else { "0" });
}

/// Reads the Circuit Library's ordered (library, name) pairs on first use.
pub fn load_library() -> (Vec<(String, String)>, Vec<(String, String)>) {
    load("elec_library").and_then(|line| DeJson::deserialize_json_strict(&line).ok()).unwrap_or_default()
}

/// Writes Pinned and Recent together as one compact JSON value when either changes.
pub fn save_library(pinned: &[(String, String)], recent: &[(String, String)]) {
    save("elec_library", &(pinned, recent).serialize_json());
}

/// The benches chosen in the Electronics recording bar, newest project first.
fn bench_choices() -> Vec<(String, String)> {
    load("elec_benches").and_then(|line| DeJson::deserialize_json_strict(&line).ok()).unwrap_or_default()
}

/// The bench last chosen for `project`, by name.
pub fn load_bench(project: &str) -> Option<String> {
    bench_choices().into_iter().find_map(|(name, bench)| (name == project).then_some(bench))
}

/// Keeps the choice for the most recent projects, as many as the Library's Recent list.
pub fn save_bench(project: &str, bench: &str) {
    if project.is_empty() { return; }
    let mut pairs = bench_choices();
    pairs.retain(|(name, _)| name != project);
    pairs.insert(0, (project.to_owned(), bench.to_owned()));
    pairs.truncate(fab_elec_view::library::RECENT_COUNT);
    save("elec_benches", &pairs.serialize_json());
}

/// View › Experimental › Scan: the Scan workspaces offered; off until chosen.
pub fn load_scan() -> bool {
    load("scan").as_deref() == Some("1")
}

pub fn save_scan(on: bool) {
    save("scan", if on { "1" } else { "0" });
}

/// The keymap's rebindings: each rebound action's name (`ShortcutId`) and its chord, as typed
/// into the Keymap reference.
pub fn load_keymap() -> Vec<(String, String)> {
    load("keymap").and_then(|line| DeJson::deserialize_json_strict(&line).ok()).unwrap_or_default()
}

pub fn save_keymap(pairs: &[(String, String)]) {
    save("keymap", &pairs.serialize_json());
}

/// The workspaces' file beside this one ([`crate::workspaces::Workspaces::load`]); none where
/// nothing is kept (the web, a lane's test instance, a unit test without its own file).
pub fn workspaces_path() -> Option<PathBuf> {
    if cfg!(target_arch = "wasm32") {
        return None;
    }
    path().map(|p| p.with_extension("workspaces.ron"))
}

/// The Scan view draws the scan in the cameras' colours; it starts on.
pub fn load_scan_colour() -> bool {
    load("scan_colour").as_deref() != Some("0")
}

pub fn save_scan_colour(on: bool) {
    save("scan_colour", if on { "1" } else { "0" });
}

/// The Scan view marks where the keyframes were taken from; it starts on.
pub fn load_scan_cameras() -> bool {
    load("scan_cameras").as_deref() != Some("0")
}

pub fn save_scan_cameras(on: bool) {
    save("scan_cameras", if on { "1" } else { "0" });
}

/// How fine each version under the hand is, as chosen on this machine.
pub fn load_drag_detail() -> DragDetail {
    parse_drag_detail(load("drag_detail").as_deref())
}

fn parse_drag_detail(value: Option<&str>) -> DragDetail {
    match value.map(str::trim) {
        Some("halfway") => DragDetail::Halfway,
        Some("full") => DragDetail::Full,
        _ => DragDetail::Realtime,
    }
}

/// Keeps the person's choice of drag detail for the next start.
pub fn save_drag_detail(detail: DragDetail) {
    save(
        "drag_detail",
        match detail {
            DragDetail::Realtime => "realtime",
            DragDetail::Halfway => "halfway",
            DragDetail::Full => "full",
        },
    );
}

/// How the Sliced view draws its far beads on the desktop, as chosen on this machine.
pub fn load_sliced_detail() -> SlicedDetail {
    parse_sliced_detail(load("sliced_detail").as_deref())
}

fn parse_sliced_detail(value: Option<&str>) -> SlicedDetail {
    match value.map(str::trim) {
        Some("exact") => SlicedDetail::Exact,
        _ => SlicedDetail::Fast,
    }
}

/// Keeps the person's choice of sliced detail for the next start.
pub fn save_sliced_detail(detail: SlicedDetail) {
    save(
        "sliced_detail",
        match detail {
            SlicedDetail::Fast => "fast",
            SlicedDetail::Exact => "exact",
        },
    );
}

/// Traced sample detail, Fast until Exact is chosen.
pub fn load_traced_detail() -> TracedDetail {
    parse_traced_detail(load("traced_detail").as_deref())
}

fn parse_traced_detail(value: Option<&str>) -> TracedDetail {
    match value.map(str::trim) {
        Some("exact") => TracedDetail::Exact,
        _ => TracedDetail::Fast,
    }
}

/// Keeps the person's choice of traced detail for the next start.
pub fn save_traced_detail(detail: TracedDetail) {
    save(
        "traced_detail",
        match detail {
            TracedDetail::Fast => "fast",
            TracedDetail::Exact => "exact",
        },
    );
}

/// Solder joints show unless the person turned Soldered off (View menu).
pub fn load_soldered() -> bool {
    parse_soldered(load("soldered").as_deref())
}

fn parse_soldered(value: Option<&str>) -> bool {
    !matches!(value.map(str::trim), Some("off"))
}

pub fn save_soldered(on: bool) {
    save("soldered", if on { "on" } else { "off" });
}

/// The look a start without a choice shows: meshes on the web, else the field.
pub fn default_look() -> PartLook {
    if cfg!(target_arch = "wasm32") {
        PartLook::Mesh
    } else {
        PartLook::Field
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{AppState, ShellAction};
    use std::cell::RefCell;

    thread_local! {
        /// The file this test thread's prefs live in; none: nothing is read or kept.
        pub(super) static PREFS: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    }

    /// A file no run has used, this thread's until the guard drops (which removes it).
    struct FreshPrefs(PathBuf);

    impl FreshPrefs {
        fn new() -> Self {
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
            let p = std::env::temp_dir().join(format!("fab-view-prefs-{}-{nanos}", std::process::id()));
            assert!(!p.exists(), "a fresh prefs file: {} is new", p.display());
            PREFS.with(|cell| *cell.borrow_mut() = Some(p.clone()));
            FreshPrefs(p)
        }
    }

    impl Drop for FreshPrefs {
        fn drop(&mut self) {
            PREFS.with(|cell| *cell.borrow_mut() = None);
            let _ = std::fs::remove_file(&self.0);
            let _ = std::fs::remove_file(self.0.with_extension("workspaces.ron"));
        }
    }

    #[test]
    fn library_shortcuts_round_trip() {
        let prefs = FreshPrefs::new();
        let pinned = vec![("models\t\n=;\\".into(), "R\t1\n=;\\n".into()), (String::new(), "µF".into())];
        let recent = vec![pinned[1].clone(), pinned[0].clone()];
        save_library(&pinned, &recent);
        let text = std::fs::read_to_string(&prefs.0).expect("the Library preference was written");
        assert_eq!(text.lines().count(), 1, "both lists occupy one preference line");
        assert_eq!(load_library(), (pinned.clone(), recent), "both lists keep their exact keys and order");
        save_library(&pinned, &[]);
        assert_eq!(load_library(), (pinned, Vec::new()), "Recent can be empty while Pinned survives");
        save("elec_library", r#"[[["","R"]],["incomplete pair"]]"#);
        assert_eq!(load_library(), (Vec::new(), Vec::new()), "a malformed list discards the whole preference");
    }

    #[test]
    fn app_state_default_reads_no_prefs_and_apply_loaded_view_prefs_restores_all() {
        use crate::api::NavScheme;
        use crate::ui::stage::PART_SIDE_W;

        let _prefs = FreshPrefs::new();
        save_drag_detail(DragDetail::Full);
        save_sliced_detail(SlicedDetail::Exact);
        save_traced_detail(TracedDetail::Exact);
        save_soldered(false);
        save_nav(NavScheme::Blender);
        save_invert_wheel(true);
        save_part_side_w(420.0);
        save_code(false, Some(520.0));
        save_look(PartLook::Mesh);
        save_ortho(true);

        let mut state = AppState::default();
        let pane = state.view.lead_plate();
        assert_eq!(state.view.drag_detail, DragDetail::Realtime);
        assert_eq!(state.view.sliced_detail, SlicedDetail::Fast);
        assert_eq!(state.view.traced_detail, TracedDetail::Fast);
        assert!(state.view.soldered);
        assert_eq!(state.ui.nav, NavScheme::Fab);
        assert!(!state.ui.invert_wheel);
        assert_eq!(state.ui.part_side_w, PART_SIDE_W);
        assert!(state.ui.code_shown(), "workspace construction ignores the saved code split");
        assert_eq!(state.view.look, default_look());
        assert!(!state.view.camera(pane).ortho);
        assert_eq!(state.model_sources, Default::default());
        assert_eq!(state.firmware_net, Default::default());
        assert_eq!(state.firmware_tokens, Ok(Vec::new()));

        let prefs = ViewPrefs::load();
        state.apply_loaded_view_prefs(prefs.clone(), false);
        assert_eq!(state.view.drag_detail, DragDetail::Full);
        assert_eq!(state.view.sliced_detail, SlicedDetail::Exact);
        assert_eq!(state.view.traced_detail, TracedDetail::Exact);
        assert!(!state.view.soldered);
        assert_eq!(state.ui.nav, NavScheme::Blender);
        assert!(state.ui.invert_wheel);
        assert_eq!(state.ui.part_side_w, 420.0);
        assert!(!state.ui.code_shown(), "Design restores the saved code split");
        assert_eq!(state.view.look, PartLook::Mesh);
        assert_eq!(state.ui.workspaces.rev, 1);
        assert_eq!(state.ui.workspaces.instance_rev, 1);
        assert!(state.ui.workspaces.list.iter().all(|def| def.shape == 1));
        assert!(!state.view.camera(pane).ortho, "a restored camera keeps its projection");
        state.apply_loaded_view_prefs(prefs, true);
        assert!(state.view.camera(pane).ortho, "a fresh camera takes the saved projection");
    }

    #[test]
    fn a_chosen_look_comes_back() {
        let _prefs = FreshPrefs::new();
        assert_eq!(load_look(), None);
        save_look(PartLook::Mesh);
        assert_eq!(load_look(), Some(PartLook::Mesh));
        save_look(PartLook::Field);
        assert_eq!(load_look(), Some(PartLook::Field));
        assert!(!load_slice());
        save_slice(true);
        assert!(load_slice());
        assert_eq!(load_look(), Some(PartLook::Field), "the slice keeps the look");
        assert_eq!(load_mech_speed(), None);
        save_mech_speed(0.25);
        assert_eq!(load_mech_speed(), Some(0.25));
        assert_eq!(load_look(), Some(PartLook::Field), "the speed keeps the look");
        assert!(!load_arm(), "the experimental Arm workspace starts hidden");
        save_arm(true);
        assert!(load_arm());
        assert!(!load_scan(), "the experimental Scan workspace starts hidden, independently of Arm");
        save_scan(true);
        assert!(load_scan() && load_arm());
        save_scan(false);
        assert!(!load_scan() && load_arm(), "hiding Scan keeps Arm offered");
        save_scan(true);
        assert!(load_scan_colour(), "the scan starts in colour");
        save_scan_colour(false);
        assert!(!load_scan_colour(), "the scan's colour can be turned off and stays off");
        save_scan_colour(true);
        assert!(load_scan_colour());
        assert!(load_scan_cameras(), "the scan starts with its cameras marked");
        save_scan_cameras(false);
        assert!(!load_scan_cameras() && load_scan_colour(), "the cameras' marks can be turned off, alone");
        save_scan_cameras(true);
        assert!(!load_ortho(), "the view starts in perspective");
        save_ortho(true);
        assert!(load_ortho());
        assert_eq!(load_look(), Some(PartLook::Field), "the projection keeps the look");

        // Nothing chosen: the view starts as it always did.
        assert_eq!(load_shading(), None);
        assert_eq!(load_colours(), None);
        assert_eq!(load_overlays(), None);
        let mut fresh = AppState::default();
        fresh.apply_view_prefs();
        assert!(fresh.ui.scan_shown, "the Scan choice comes back on the next start");
        assert_eq!(fresh.view.shading, Shading::Realtime, "the default stands");
        assert_eq!(fresh.view.overlays, Overlays::default(), "the default stands");
        // What a person picks comes back on the next run, a restart's or a new session's: each
        // starts from nothing but this file.
        let overlays = Overlays { grid: false, cavity: false, nav_gizmo: false, ..Overlays::default() };
        fresh.apply_core(&ShellAction::SetShading(Shading::Solid));
        assert_eq!(fresh.view.colours, fab_plate::ColourMode::Model);
        fresh.apply_core(&ShellAction::SetColourMode(fab_plate::ColourMode::Material));
        fresh.apply_core(&ShellAction::SetOverlays(overlays));
        let mut next = AppState::default();
        next.apply_view_prefs();
        assert_eq!(next.view.shading, Shading::Solid);
        assert_eq!(next.view.overlays, overlays);
        assert_eq!(next.view.colours, fab_plate::ColourMode::Material);
        next.apply_core(&ShellAction::SetColourMode(fab_plate::ColourMode::Model));
        assert_eq!(load_look(), Some(PartLook::Field), "the shading keeps the look");
        // Picked back, it comes back too: a saved Realtime is not "nothing chosen".
        next.apply_core(&ShellAction::SetShading(Shading::Realtime));
        next.apply_core(&ShellAction::SetOverlays(Overlays::default()));
        let mut last = AppState::default();
        last.apply_view_prefs();
        assert_eq!(last.view.shading, Shading::Realtime);
        assert_eq!(last.view.colours, fab_plate::ColourMode::Model);
        assert_eq!(last.view.overlays, Overlays::default());
        // View › Sliced Detail: Fast until Exact is picked, and a pick comes back on the next run
        // (the far tier's beads as quads or every bead whole, as the person left them).
        assert_eq!(last.view.sliced_detail, SlicedDetail::Fast, "the far beads start fast");
        last.apply_core(&ShellAction::SetSlicedDetail(SlicedDetail::Exact));
        let mut restarted = AppState::default();
        assert_eq!(restarted.view.sliced_detail, SlicedDetail::Fast, "construction uses the default detail");
        restarted.apply_view_prefs();
        assert_eq!(restarted.view.sliced_detail, SlicedDetail::Exact, "a restart keeps Exact");
        assert_eq!(load_look(), Some(PartLook::Field), "the sliced detail keeps the look");
        last.apply_core(&ShellAction::SetSlicedDetail(SlicedDetail::Fast));
        restarted.apply_view_prefs();
        assert_eq!(restarted.view.sliced_detail, SlicedDetail::Fast, "Fast picked back comes back");
        // View › Traced Detail keeps the same default and restart behaviour.
        assert_eq!(last.view.traced_detail, TracedDetail::Fast, "traced samples start fast");
        last.apply_core(&ShellAction::SetTracedDetail(TracedDetail::Exact));
        let mut restarted = AppState::default();
        assert_eq!(restarted.view.traced_detail, TracedDetail::Fast, "construction uses the default detail");
        restarted.apply_view_prefs();
        assert_eq!(restarted.view.traced_detail, TracedDetail::Exact, "a restart keeps Exact");
        assert_eq!(load_look(), Some(PartLook::Field), "the traced detail keeps the look");
        last.apply_core(&ShellAction::SetTracedDetail(TracedDetail::Fast));
        restarted.apply_view_prefs();
        assert_eq!(restarted.view.traced_detail, TracedDetail::Fast, "Fast picked back comes back");
    }
}

/// The Mechanics sim speed (sim seconds per wall second) chosen on this machine, if it was.
pub fn load_mech_speed() -> Option<f64> {
    load("mech_speed")?.trim().parse().ok().filter(|s: &f64| s.is_finite() && *s > 0.0)
}

/// Keeps the Mechanics sim speed for the next start.
pub fn save_mech_speed(speed: f64) {
    save("mech_speed", &format!("{speed}"));
}

/// The contact setting chosen on this machine (Fast when none was).
pub fn load_mech_contact() -> MechContact {
    match load("mech_contact").as_deref().map(str::trim) {
        Some("exact") => MechContact::Exact,
        _ => MechContact::Fast,
    }
}

/// Keeps the contact setting for the next start.
pub fn save_mech_contact(contact: MechContact) {
    save("mech_contact", match contact {
        MechContact::Fast => "fast",
        MechContact::Exact => "exact",
    });
}

/// Realtime's lighting outside Electronics as chosen on this machine, if it was.
pub fn load_lighting() -> Option<Lighting> {
    Lighting::from_key(&load("lighting")?)
}

/// Keeps the person's choice of lighting for the next start.
pub fn save_lighting(lighting: Lighting) {
    save("lighting", lighting.key());
}

/// Electronics' lighting as chosen on this machine, if it was.
pub fn load_board_lighting() -> Option<Lighting> {
    Lighting::from_key(&load("board_lighting")?)
}

/// Keeps the board view's lighting independently of the other workspaces.
pub fn save_board_lighting(lighting: Lighting) {
    save("board_lighting", lighting.key());
}

pub fn load_colours() -> Option<fab_plate::ColourMode> {
    fab_plate::ColourMode::from_key(&load("colours")?)
}

pub fn save_colours(mode: fab_plate::ColourMode) {
    save("colours", mode.key());
}

/// The shading as chosen on this machine, if it was.
pub fn load_shading() -> Option<Shading> {
    Shading::from_key(&load("shading")?)
}

/// Keeps the person's choice of shading for the next start.
pub fn save_shading(shading: Shading) {
    save("shading", shading.key());
}

/// The overlays by the names the file gives them.
fn overlay_flags(overlays: &mut Overlays) -> [(&'static str, &mut bool); 7] {
    [
        ("grid", &mut overlays.grid),
        ("axes", &mut overlays.axes),
        ("outlines", &mut overlays.outlines),
        ("cavity", &mut overlays.cavity),
        ("sections", &mut overlays.section_planes),
        ("measurements", &mut overlays.measurements),
        ("gizmo", &mut overlays.nav_gizmo),
    ]
}

/// The overlays as chosen on this machine, if they were; one the file does not name starts as it
/// always does.
pub fn load_overlays() -> Option<Overlays> {
    load("overlays").as_deref().map(parse_overlays)
}

fn parse_overlays(text: &str) -> Overlays {
    let mut overlays = Overlays::default();
    for (name, on) in overlay_flags(&mut overlays) {
        if let Some(value) = text.split(',').find_map(|kv| kv.strip_prefix(name)?.strip_prefix(':')) {
            *on = value.trim() == "1";
        }
    }
    overlays
}

/// Keeps the person's choice of overlays for the next start.
pub fn save_overlays(mut overlays: Overlays) {
    let text: Vec<String> = overlay_flags(&mut overlays).into_iter().map(|(name, on)| format!("{name}:{}", *on as u8)).collect();
    save("overlays", &text.join(","));
}
