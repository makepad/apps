//! The ⋯ menu's contents (the caption's main menu, `topbar.rs`): File /
//! Edit / View / Help, built from live state, and the action each row fires.
//!
//! **Every row here fires an action that visibly changes the app.** An entry
//! exists only when the action behind it has a reader somewhere that moves
//! pixels; the drawing, hover tracking, flyouts and keyboard live in
//! `menu.rs`, which every other dropdown in the app shares.

use fab_sdf_tiles::trace::DragDetail;
use crate::api::*;
use crate::ui::keymap::{self, ShortcutId as Key};
use crate::ui::popover::MenuItem;
use crate::workspaces::{ListOp, PlatePair};
use makepad_widgets_core::*;

/// Recent-file rows carry their index in the id.
const RECENT_BASE: u64 = 0x6269_6d78_0003_0000;
fn recent_id(i: usize) -> LiveId {
    LiveId(RECENT_BASE | i as u64)
}
fn recent_index(id: LiveId) -> Option<usize> {
    (id.0 >= RECENT_BASE && id.0 < RECENT_BASE + 64).then(|| (id.0 - RECENT_BASE) as usize)
}

/// File > Examples rows carry their demo's index in the id.
const DEMO_BASE: u64 = 0x6269_6d78_0005_0000;
fn demo_id(i: usize) -> LiveId {
    LiveId(DEMO_BASE | i as u64)
}
fn demo_index(id: LiveId) -> Option<usize> {
    (id.0 >= DEMO_BASE && id.0 < DEMO_BASE + 0x1_0000).then(|| (id.0 - DEMO_BASE) as usize)
}


// ===========================================================================
// The menus, built from live state
// ===========================================================================

pub(crate) fn file_menu(state: &AppState) -> Vec<MenuItem> {
    let mut items = vec![
        MenuItem::new(live_id!(file_new_plate), "New Plate").key(&keymap::shortcut(Key::New)),
        MenuItem::new(live_id!(file_open), "Open…").key(&keymap::shortcut(Key::Open)),
        MenuItem::new(live_id!(file_import), "Import…").key(&keymap::shortcut(Key::Import)),
    ];
    // The plates edited last; the plate list has them all with their pictures.
    let rows: Vec<MenuItem> = state.plates.plates.iter().enumerate().take(10)
        .map(|(i, p)| MenuItem::new(recent_id(i), &p.title()).note(&p.when))
        .collect();
    items.push(MenuItem::new(live_id!(file_recent), "Open Recent").flyout(rows).enabled_if(!state.plates.plates.is_empty()));
    items.extend([
        MenuItem::new(live_id!(file_plates), "Browse Plates…").key(&keymap::shortcut(Key::Plates)),
        MenuItem::new(live_id!(file_save_plate), "Save Plate").key(&keymap::shortcut(Key::Save)),
        MenuItem::new(live_id!(file_import_profiles), "Import Profiles…"),
        MenuItem::sep(),
        MenuItem::new(live_id!(file_export_3mf), "Export 3MF").key(&keymap::shortcut(Key::Export3mf)),
        MenuItem::new(live_id!(file_export_stl), "Export STL").key(&keymap::shortcut(Key::ExportStl)),
        MenuItem::new(live_id!(file_reslice), "Slice again from scratch").key(&keymap::shortcut(Key::Slice)),
        MenuItem::sep(),
        MenuItem::new(live_id!(file_examples), "Examples").flyout(
            state.plates.demos.iter().enumerate().map(|(i, d)| {
                let row = MenuItem::new(demo_id(i), &d.name);
                if d.id == "demo" { row.key(&keymap::shortcut(Key::Demo)) } else { row }
            }).collect(),
        ),
    ]);
    let path = current_path(state);
    items.push(
        // A plate reads its file again only when it has no edits the file lacks.
        MenuItem::new(live_id!(file_reload), "Reload").enabled_if(match state.plates.bed() {
            Some(bed) if !bed.demo => bed.file.is_some() && !bed.modified,
            _ => path.is_some(),
        }),
    );
    items.push(MenuItem::sep());
    items.push(MenuItem::new(live_id!(file_quit), "Quit").key(&keymap::shortcut(Key::Quit)));
    items
}

pub(crate) fn edit_menu(state: &AppState) -> Vec<MenuItem> {
    let has_sel = !state.scene_state.selection.set.is_empty();
    let has_scene = !state.scene.elements.is_empty();
    vec![
        MenuItem::new(live_id!(edit_undo), "Undo").key(&keymap::shortcut(Key::Undo)),
        MenuItem::new(live_id!(edit_redo), "Redo").key(&keymap::shortcut(Key::Redo)),
        MenuItem::new(live_id!(edit_undo_turn), "Undo the AI's Last Turn").enabled_if(state.ui.undo_turn.is_some()),
        MenuItem::new(live_id!(edit_chat), "Ask the AI…").key(&keymap::shortcut(Key::Chat)),
        MenuItem::sep(),
        MenuItem::new(live_id!(edit_select_all), "Select All").enabled_if(has_scene),
        MenuItem::new(live_id!(edit_deselect), "Deselect All").enabled_if(has_sel),
        MenuItem::sep(),
        MenuItem::new(live_id!(edit_hide), "Hide Selected")
            .key(&keymap::shortcut(Key::Hide))
            .enabled_if(has_sel),
        MenuItem::new(live_id!(edit_isolate), "Isolate Selected")
            .key(&keymap::shortcut(Key::Isolate))
            .enabled_if(has_sel),
        MenuItem::new(live_id!(edit_unhide), "Unhide All").key(&keymap::shortcut(Key::Unhide)),
        MenuItem::sep(),
        MenuItem::new(live_id!(edit_drop), "Drop to Plate").enabled_if(has_sel),
        MenuItem::new(live_id!(edit_duplicate_linked), "Duplicate (linked)").key(&keymap::shortcut(Key::DuplicateLinked)).enabled_if(has_sel && !state.slice_blocks_edits() && !state.ui.in_mode(PlateMode::Mechanics)),
        MenuItem::new(live_id!(edit_duplicate_copy), "Duplicate as copy").key(&keymap::shortcut(Key::DuplicateCopy)).enabled_if(has_sel && !state.slice_blocks_edits() && !state.ui.in_mode(PlateMode::Mechanics)),
        MenuItem::new(live_id!(edit_arrange), "Arrange Plate").enabled_if(state.plate.parts.len() > 1),
        MenuItem::sep(),
        MenuItem::new(live_id!(edit_model_sources), "3D Model Sources…"),
    ]
}

pub(crate) fn view_menu(state: &AppState) -> Vec<MenuItem> {
    let v = &state.view;
    // the Plate views paired with the active one (or with the one it follows, when it is one of
    // them), and the Underside's pane: O(the shown tabs)
    let active = v.plate_number(v.camera_root(v.active_plate()));
    let paired = |pair| active.and_then(|n| state.ui.workspaces.paired(n, pair));
    let underside = paired(PlatePair::Underside).and_then(|(_, i)| v.plate_pane(i.n));
    let has_sel = !state.scene_state.selection.set.is_empty();
    // What the Sliced view draws: its toolpaths (a resin printer's layers are none); the printed
    // surface draws neither travel nor the parts under it.
    let play = &state.print.play;
    let paths = play.active && state.print.resin_shown().is_none();
    let beads = paths && !state.print.finish;
    let viewpoints: Vec<MenuItem> = [
        (live_id!(vp_front), PresetView::Front, Key::Front),
        (live_id!(vp_back), PresetView::Back, Key::Back),
        (live_id!(vp_right), PresetView::Right, Key::Right),
        (live_id!(vp_left), PresetView::Left, Key::Left),
        (live_id!(vp_top), PresetView::Top, Key::Top),
        (live_id!(vp_bottom), PresetView::Bottom, Key::Bottom),
        (live_id!(vp_iso), PresetView::Isometric, Key::Isometric),
    ]
    .iter()
    .map(|(id, p, key)| {
        MenuItem::new(*id, p.label())
            .key(&keymap::shortcut(*key))
            .radio(v.pane(v.active_plate()).preset == Some(*p))
    })
    .collect();

    let shading: Vec<MenuItem> = [
        (live_id!(sh_solid), Shading::Solid),
        (live_id!(sh_realtime), Shading::Realtime),
    ]
    .iter()
    .map(|(id, s)| MenuItem::new(*id, s.label()).radio(v.shading == *s))
    .chain(std::iter::once(MenuItem::sep()))
    // The render modes: the view's own switch shows the same choice.
    // Normal's look (kept on this machine), then Sliced and what it shows.
    .chain([(live_id!(look_field), PartLook::Field), (live_id!(look_mesh), PartLook::Mesh)].iter().map(|(id, l)| MenuItem::new(*id, l.label()).radio(v.look == *l)))
    .chain(std::iter::once(MenuItem::sep()))
    .chain(std::iter::once(MenuItem::new(live_id!(look_slice), "Sliced").key(&keymap::shortcut(Key::SliceView)).checked(state.print.play.active)))
    .chain(std::iter::once(MenuItem::new(live_id!(look_surface), "Printed Surface").checked(state.print.finish).enabled_if(state.print.play.active)))
    .chain(std::iter::once(MenuItem::sep()))
    .chain([(live_id!(li_studio), Lighting::Studio), (live_id!(li_bright), Lighting::BrightStudio), (live_id!(li_sun), Lighting::SunAndSky)].iter().map(|(id, l)| MenuItem::new(*id, l.label()).radio(v.lighting_for(&state.ui) == *l).enabled_if(v.shading == Shading::Realtime)))
    .collect();

    // Only the flags something actually reads: a checkbox that toggles a
    // field nobody looks at is the fake menu this app is not to have. Grid,
    // axes and cavity are Solid's.
    let o = v.overlays;
    let solid = v.shading == Shading::Solid;
    let overlays = vec![
        MenuItem::new(live_id!(ov_grid), "Grid").checked(o.grid).enabled_if(solid),
        MenuItem::new(live_id!(ov_axes), "Axes").checked(o.axes).enabled_if(solid),
        MenuItem::new(live_id!(ov_outlines), "Selection Outline").checked(o.outlines),
        MenuItem::sep(),
        MenuItem::new(live_id!(ov_cavity), "Cavity").checked(o.cavity).enabled_if(solid),
        MenuItem::sep(),
        // Genuinely context-dependent: nothing to draw until the tool ran.
        MenuItem::new(live_id!(ov_measure), "Measurements")
            .checked(o.measurements)
            .enabled_if(!state.measurements.is_empty()),
        MenuItem::new(live_id!(ov_sections), "Section Planes")
            .checked(o.section_planes)
            .enabled_if(state.scene_state.section.enabled),
        MenuItem::sep(),
        MenuItem::new(live_id!(ov_gizmo), "Navigation Gizmo").checked(o.nav_gizmo),
        MenuItem::sep(),
        // The plate's own: where each nozzle reaches, and the parts shaded by overhang.
        MenuItem::new(live_id!(ov_zones), "Nozzle Zones").checked(state.ui.show_zones),
        MenuItem::new(live_id!(ov_overhangs), "Overhangs").checked(state.print.overhangs).enabled_if(!state.print.play.active),
    ];

    // the caption's row, the first nine on Cmd/Ctrl+1..9
    let workspaces: Vec<MenuItem> = state.ui.workspaces.list.iter()
        .filter(|d| state.ui.offered(d))
        .enumerate()
        .map(|(i, d)| {
            let row = MenuItem::new(LiveId(WORKSPACE_BASE | d.id.0 as u64), &d.name).radio(state.ui.workspaces.current == d.id);
            if i < 9 { row.key(&makepad_widgets_core::kbd::shortcut_label(&format!("Cmd/Ctrl+{}", i + 1))) } else { row }
        })
        .collect();

    let measurements = measurement_tools().into_iter().map(|(id, tool)| {
        MenuItem::new(id, tool.label()).radio(state.tool == tool)
    }).collect();

    vec![
        MenuItem::new(live_id!(view_frame_all), "Frame All").key(&keymap::shortcut(Key::FrameAll)),
        MenuItem::new(live_id!(view_frame_sel), "Frame Selected")
            .key(&keymap::shortcut(Key::FrameSelected))
            .enabled_if(has_sel),
        MenuItem::new(live_id!(view_orbit_sel), if has_sel { "Orbit Around Selection" } else { "Orbit Around Plate" }).key(&keymap::shortcut(Key::OrbitSelection)),
        MenuItem::sep(),
        MenuItem::new(live_id!(view_viewpoint), "Viewpoint").flyout(viewpoints),
        MenuItem::new(live_id!(view_ortho), "Orthographic")
            .key(&keymap::shortcut(Key::Ortho))
            .checked(v.camera(v.active_plate()).ortho),
        // the active Plate view's Underside and Traced views: Plate views beside it
        MenuItem::new(live_id!(view_underside), "Underside view").checked(underside.is_some()),
        MenuItem::new(live_id!(view_traced), "Traced view").checked(paired(PlatePair::Traced).is_some()),
        MenuItem::new(live_id!(view_underside_through), "See through (mirrored)")
            .enabled_if(underside.is_some())
            .checked(underside.is_some_and(|pane| v.pane(pane).link.how() == Some(LinkHow::Through))),
        // The view bar's toggles of the Sliced view.
        MenuItem::new(live_id!(view_supports), "Show Supports").key(&keymap::shortcut(Key::ShowSupports)).checked(play.supports).enabled_if(paths),
        MenuItem::new(live_id!(view_travel), "Show Travel").checked(play.travel).enabled_if(beads),
        MenuItem::new(live_id!(view_parts), "Show Parts Under the Toolpaths").checked(play.show_model).enabled_if(beads),
        MenuItem::new(live_id!(view_cut_layer), "Cut Parts at the Layer").checked(play.cut).enabled_if(beads),
        MenuItem::sep(),
        MenuItem::new(live_id!(view_shading), "Shading").flyout(shading),
        MenuItem::new(live_id!(view_drag_detail), "Drag Detail").flyout(vec![
            MenuItem::new(live_id!(dd_realtime), "Realtime").radio(v.drag_detail == DragDetail::Realtime),
            MenuItem::new(live_id!(dd_halfway), "Halfway").radio(v.drag_detail == DragDetail::Halfway),
            MenuItem::new(live_id!(dd_full), "Full (slower)").radio(v.drag_detail == DragDetail::Full),
        ]),
        MenuItem::new(live_id!(view_sliced_detail), "Sliced Detail").flyout(vec![
            MenuItem::new(live_id!(sd_fast), "Fast").radio(v.sliced_detail == SlicedDetail::Fast),
            MenuItem::new(live_id!(sd_exact), "Exact (slower)").radio(v.sliced_detail == SlicedDetail::Exact),
        ]),
        MenuItem::new(live_id!(view_traced_detail), "Traced Detail").flyout(vec![
            MenuItem::new(live_id!(td_fast), "Fast").radio(v.traced_detail == TracedDetail::Fast),
            MenuItem::new(live_id!(td_exact), "Exact (slower)").radio(v.traced_detail == TracedDetail::Exact),
        ]),
        MenuItem::new(live_id!(view_soldered), "Soldered").checked(v.soldered),
        MenuItem::new(live_id!(view_board), "Show Board").checked(state.board_shown() == Some(true)).enabled_if(state.board_shown().is_some()),
        MenuItem::new(live_id!(view_xray), "X-Ray")
            .key(&keymap::shortcut(Key::Xray))
            .checked(v.xray),
        MenuItem::new(live_id!(view_see_through), "See-Through").checked(v.see.on),
        MenuItem::new(live_id!(view_glass), "Translucent materials").checked(v.glass),
        MenuItem::new(live_id!(view_overlays), "Overlays").flyout(overlays),
        MenuItem::new(live_id!(view_measure), "Measure").key(&keymap::shortcut(Key::Measure)).flyout(measurements),
        MenuItem::sep(),
        MenuItem::new(live_id!(view_undo_layout), "Undo layout change").enabled_if(state.ui.workspaces.can_undo(state.ui.workspaces.current)),
        MenuItem::new(live_id!(view_workspace), "Workspace").flyout(workspaces),
        MenuItem::new(live_id!(view_follow_ai), "Follow AI").checked(state.ui.follow_ai),
        MenuItem::new(live_id!(view_code), "Code").key(&keymap::shortcut(Key::Code)).checked(state.ui.code_shown()).enabled_if(state.ui.kind() == Workspace::Design),
        // work that is done enough to keep but not to offer: shown on this machine on request
        MenuItem::new(live_id!(view_experimental), "Experimental").flyout(vec![
            MenuItem::new(live_id!(exp_arm), "Arm").checked(state.ui.arm_shown),
            MenuItem::new(live_id!(exp_scan), "Scan").checked(state.ui.scan_shown),
        ]),
    ]
}

/// Workspace rows carry their workspace's id in the id's low half.
const WORKSPACE_BASE: u64 = 0x6269_6d77_0000_0000;

fn measurement_tools() -> [(LiveId, Tool); 7] {
    [
        (live_id!(measure_distance), Tool::Measure(MeasureKind::Distance)),
        (live_id!(measure_area), Tool::Measure(MeasureKind::Area)),
        (live_id!(measure_angle), Tool::Measure(MeasureKind::Angle)),
        (live_id!(measure_edge), Tool::Measure(MeasureKind::EdgeLength)),
        (live_id!(measure_radius), Tool::Measure(MeasureKind::Radius)),
        (live_id!(measure_faces), Tool::Measure(MeasureKind::FaceDistance)),
        (live_id!(measure_fit), Tool::MeasureFit),
    ]
}

/// A workspace row's workspace (its id is in the row's).
fn workspace_of(id: LiveId) -> Option<crate::workspaces::WorkspaceId> {
    (id.0 & !0xffff_ffff == WORKSPACE_BASE).then(|| crate::workspaces::WorkspaceId(id.0 as u32))
}

pub(crate) fn help_menu() -> Vec<MenuItem> {
    vec![
        MenuItem::new(live_id!(help_keymap), "Keymap Reference").key(&keymap::shortcut(Key::Help)),
        MenuItem::new(live_id!(help_palette), "Command Palette").key(&keymap::shortcut(Key::Palette)),
        MenuItem::sep(),
        MenuItem::new(live_id!(help_licences), "External Resources and Licences…"),
        MenuItem::new(live_id!(help_about), "About Fab"),
    ]
}

fn current_path(state: &AppState) -> Option<std::path::PathBuf> {
    match &state.load {
        LoadStatus::Loaded { path: Some(p) } => Some(p.clone()),
        _ => state.scene.source_path.clone(),
    }
}

/// One picked row → one action. Everything here has a reader that moves
/// pixels; see the module doc for what is deliberately absent.
pub(crate) fn action_for(state: &AppState, id: LiveId) -> Option<ShellAction> {
    if let Some((_, tool)) = measurement_tools().into_iter().find(|(key, _)| *key == id) {
        return Some(ShellAction::SetTool(tool));
    }
    if let Some(i) = recent_index(id) {
        return state.plates.plates.get(i).map(|p| ShellAction::OpenPlate(p.id.clone()));
    }
    if let Some(i) = demo_index(id) {
        return state.plates.demos.get(i).map(|d| ShellAction::OpenDemo(d.id.clone()));
    }
    if let Some(ws) = workspace_of(id) {
        return Some(ShellAction::SwitchWorkspace(ws));
    }
    {
        let mut overlays = state.view.overlays;
        match id {
            // ---- File ----
            _ if id == live_id!(file_open) => Some(ShellAction::OpenDialog),
            _ if id == live_id!(file_import) => Some(ShellAction::ImportDialog),
            _ if id == live_id!(file_plates) => Some(ShellAction::ShowPlates),
            // Profiles open through the same dialog: a preset file, bundle or project imports.
            _ if id == live_id!(file_import_profiles) => Some(ShellAction::ImportProfilesDialog),
            // The plate's commands: the app runs them by name (main.rs `run_command`).
            _ if id == live_id!(file_new_plate) => Some(ShellAction::Command("new plate".into())),
            _ if id == live_id!(file_save_plate) => Some(ShellAction::Command("save plate".into())),
            _ if id == live_id!(file_export_3mf) => Some(ShellAction::Command("export 3mf".into())),
            _ if id == live_id!(file_export_stl) => Some(ShellAction::Command("export stl".into())),
            _ if id == live_id!(file_reslice) => Some(ShellAction::Command("slice".into())),
            _ if id == live_id!(edit_undo) => Some(ShellAction::Command("undo".into())),
            _ if id == live_id!(edit_redo) => Some(ShellAction::Command("redo".into())),
            _ if id == live_id!(edit_undo_turn) => Some(ShellAction::Command("undo ai turn".into())),
            _ if id == live_id!(edit_chat) => Some(ShellAction::Command("toggle chat".into())),
            _ if id == live_id!(file_reload) => match state.plates.bed() {
                Some(bed) if !bed.demo => Some(ShellAction::Command("reload plate".into())),
                _ => current_path(state).map(ShellAction::OpenFile),
            },
            _ if id == live_id!(file_quit) => Some(ShellAction::Quit),
            // ---- Edit ----
            _ if id == live_id!(edit_select_all) => Some(ShellAction::SelectSet(
                state
                    .scene
                    .elements
                    .iter()
                    .map(|e| e.id)
                    .filter(|id| state.is_visible(*id) && !state.plate.is_context(*id))
                    .collect(),
            )),
            _ if id == live_id!(edit_deselect) => Some(ShellAction::ClearSelection),
            _ if id == live_id!(edit_hide) => Some(ShellAction::HideSelected),
            _ if id == live_id!(edit_isolate) => Some(ShellAction::IsolateSelected),
            _ if id == live_id!(edit_unhide) => Some(ShellAction::UnhideAll),
            // ---- View ----
            _ if id == live_id!(view_undo_layout) => Some(ShellAction::EditWorkspaces(ListOp::UndoLayout(state.ui.workspaces.current))),
            _ if id == live_id!(view_frame_all) => Some(ShellAction::FrameAll),
            _ if id == live_id!(view_frame_sel) => Some(ShellAction::FrameSelected),
            _ if id == live_id!(view_orbit_sel) => Some(ShellAction::OrbitAroundSelection),
            _ if id == live_id!(view_ortho) => Some(ShellAction::ToggleOrtho),
            _ if id == live_id!(view_underside) || id == live_id!(view_traced) => {
                let pair = if id == live_id!(view_underside) { PlatePair::Underside } else { PlatePair::Traced };
                let source = state.view.plate_number(state.view.camera_root(state.view.active_plate()))?;
                Some(ShellAction::EditWorkspaces(ListOp::Pair { source, pair }))
            }
            _ if id == live_id!(view_underside_through) => {
                let root = state.view.camera_root(state.view.active_plate());
                let (_, under) = state.ui.workspaces.paired(state.view.plate_number(root)?, PlatePair::Underside)?;
                let pane = state.view.plate_pane(under.n)?;
                let how = if state.view.pane(pane).link.how() == Some(LinkHow::Through) { LinkHow::Flipped } else { LinkHow::Through };
                Some(ShellAction::SetPaneLink { pane, link: PaneLink::Linked { how, to: root } })
            }
            _ if id == live_id!(view_supports) => Some(ShellAction::Command("show supports".into())),
            _ if id == live_id!(view_travel) => Some(ShellAction::Command("show travel".into())),
            _ if id == live_id!(view_parts) => Some(ShellAction::Command("show parts".into())),
            _ if id == live_id!(view_cut_layer) => Some(ShellAction::Command("cut at layer".into())),
            _ if id == live_id!(view_xray) => Some(ShellAction::ToggleXray),
            _ if id == live_id!(view_see_through) => Some(ShellAction::ToggleSeeThrough),
            _ if id == live_id!(view_glass) => Some(ShellAction::ToggleGlass),
            _ if id == live_id!(vp_front) => Some(ShellAction::PresetView(PresetView::Front)),
            _ if id == live_id!(vp_back) => Some(ShellAction::PresetView(PresetView::Back)),
            _ if id == live_id!(vp_left) => Some(ShellAction::PresetView(PresetView::Left)),
            _ if id == live_id!(vp_right) => Some(ShellAction::PresetView(PresetView::Right)),
            _ if id == live_id!(vp_top) => Some(ShellAction::PresetView(PresetView::Top)),
            _ if id == live_id!(vp_bottom) => Some(ShellAction::PresetView(PresetView::Bottom)),
            _ if id == live_id!(vp_iso) => {
                Some(ShellAction::PresetView(PresetView::Isometric))
            }
            _ if id == live_id!(sh_solid) => Some(ShellAction::SetShading(Shading::Solid)),
            _ if id == live_id!(dd_realtime) => Some(ShellAction::SetDragDetail(DragDetail::Realtime)),
            _ if id == live_id!(dd_halfway) => Some(ShellAction::SetDragDetail(DragDetail::Halfway)),
            _ if id == live_id!(dd_full) => Some(ShellAction::SetDragDetail(DragDetail::Full)),
            _ if id == live_id!(sd_fast) => Some(ShellAction::SetSlicedDetail(SlicedDetail::Fast)),
            _ if id == live_id!(sd_exact) => Some(ShellAction::SetSlicedDetail(SlicedDetail::Exact)),
            _ if id == live_id!(td_fast) => Some(ShellAction::SetTracedDetail(TracedDetail::Fast)),
            _ if id == live_id!(td_exact) => Some(ShellAction::SetTracedDetail(TracedDetail::Exact)),
            _ if id == live_id!(view_soldered) => Some(ShellAction::SetSoldered(!state.view.soldered)),
            _ if id == live_id!(view_board) => state.board_shown().map(|shown| ShellAction::ShowBoard(!shown)),
            _ if id == live_id!(li_studio) => Some(ShellAction::SetLighting(Lighting::Studio)),
            _ if id == live_id!(li_bright) => Some(ShellAction::SetLighting(Lighting::BrightStudio)),
            _ if id == live_id!(li_sun) => Some(ShellAction::SetLighting(Lighting::SunAndSky)),
            _ if id == live_id!(view_code) => Some(ShellAction::ShowCode(!state.ui.code_shown())),
            _ if id == live_id!(exp_arm) => Some(ShellAction::ShowArm(!state.ui.arm_shown)),
            _ if id == live_id!(exp_scan) => Some(ShellAction::ShowScan(!state.ui.scan_shown)),
            _ if id == live_id!(view_follow_ai) => Some(ShellAction::FollowAi(!state.ui.follow_ai)),
            _ if id == live_id!(edit_duplicate_linked) => Some(ShellAction::Command("duplicate linked".into())),
            _ if id == live_id!(edit_duplicate_copy) => Some(ShellAction::Command("duplicate as copy".into())),
            _ if id == live_id!(edit_drop) => Some(ShellAction::Command("drop to plate".into())),
            _ if id == live_id!(edit_arrange) => Some(ShellAction::Command("arrange plate".into())),
            _ if id == live_id!(ov_zones) => Some(ShellAction::Command("toggle zones".into())),
            _ if id == live_id!(ov_overhangs) => Some(ShellAction::Command("toggle overhangs".into())),
            _ if id == live_id!(look_field) => Some(ShellAction::SetPartLook(PartLook::Field)),
            _ if id == live_id!(look_mesh) => Some(ShellAction::SetPartLook(PartLook::Mesh)),
            _ if id == live_id!(look_surface) => Some(ShellAction::Command("printed surface".into())),
            _ if id == live_id!(look_slice) => Some(ShellAction::Command("slice view".into())),
            _ if id == live_id!(sh_realtime) => {
                Some(ShellAction::SetShading(Shading::Realtime))
            }
            _ if id == live_id!(ov_grid) => {
                overlays.grid = !overlays.grid;
                Some(ShellAction::SetOverlays(overlays))
            }
            _ if id == live_id!(ov_axes) => {
                overlays.axes = !overlays.axes;
                Some(ShellAction::SetOverlays(overlays))
            }
            _ if id == live_id!(ov_outlines) => {
                overlays.outlines = !overlays.outlines;
                Some(ShellAction::SetOverlays(overlays))
            }
            _ if id == live_id!(ov_cavity) => {
                overlays.cavity = !overlays.cavity;
                Some(ShellAction::SetOverlays(overlays))
            }
            _ if id == live_id!(ov_measure) => {
                overlays.measurements = !overlays.measurements;
                Some(ShellAction::SetOverlays(overlays))
            }
            _ if id == live_id!(ov_sections) => {
                overlays.section_planes = !overlays.section_planes;
                Some(ShellAction::SetOverlays(overlays))
            }
            _ if id == live_id!(ov_gizmo) => {
                overlays.nav_gizmo = !overlays.nav_gizmo;
                Some(ShellAction::SetOverlays(overlays))
            }
            // ---- Help ----
            _ if id == live_id!(help_keymap) => Some(ShellAction::ShowKeymapHelp(true)),
            _ if id == live_id!(help_palette) => Some(ShellAction::ToggleCommandPalette),
            _ if id == live_id!(help_about) => Some(ShellAction::ShowAbout(true)),
            _ if id == live_id!(help_licences) => Some(ShellAction::ShowModelSources(Some(SourcesPage::Licences))),
            _ if id == live_id!(edit_model_sources) => Some(ShellAction::ShowModelSources(Some(SourcesPage::Settings))),
            _ => None,
        }
    }
}
