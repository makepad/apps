//! Nothing the AI makes is opaque: every cut and every Motion document is
//! readable, editable Splash, seen two ways that both edit the same
//! document live.
//!
//! - **Timeline**: the cut's own timeline (`timeline.rs`); an opened
//!   Graphic clip shows its Motion document as code on a timeline: its
//!   blocks folded, their values as controls, when each draws, its keys
//!   (`motion_edit/code_timeline.rs`), clicked from the picture
//!   (`motion_edit/picking.rs`).
//! - **Source**: the Splash in Makepad's code editor, applied live when it
//!   loads, diagnostics marked inline where they are, and every change (by
//!   hand, by the timeline, by the AI) kept as a readable diff
//!   (`motion_edit/source_pane.rs`).
//!
//! One source of truth: the page's session (its undo stack and autosave)
//! for a cut, and the Motion session here for an opened graphic. Every
//! view writes through it, so every change is one undo step, whoever made
//! it, and the AI reads exactly what the person sees.
//!
//! This module is a child of the page: it reads and changes the page's
//! state directly, and the page calls it from four small hooks (actions,
//! events, pump, and opening a block).
//!
//! Main-loop cost: the Source view re-tokenizes the document only when it
//! changes (a keystroke or a new revision), the checker loads drafts on its
//! own thread.

pub(crate) mod checker;
mod code_timeline;
/// The AI's way in (lane M3): `desk::source_of` reads the open graphic's
/// live source, `desk::replace_source` revises it as one undo step.
pub(super) mod desk;
mod diff;
mod inspector;
pub(crate) mod picking;
pub(crate) mod session;
pub(crate) mod source_pane;
/// The library side of a graphic (opening, saving revisions), shared with
/// the cut's panel.
pub(super) mod store;
mod editor_panel;
mod palette;
mod deep_make;
mod panel_feed;
mod filmstrip;
mod panel_sweep;
mod selected_text;
mod sung_line;
pub(crate) mod panel_effects;
mod panel_time;
mod scene_context;
pub(crate) mod history;
pub(super) mod structure_panel;
pub(super) use crate::structure::{StructureAction, StructureTree, TreeRow};
mod curve_editor;
mod history_panel;
mod swaps;
/// The font and colour pickers over the inspector (lane PE1).
mod value_pick;
mod variant_strip;
/// Variants of the open graphic asked of the AI (lane VR1).
pub(super) mod variants;
mod viewer;

use super::VideoCut;
use checker::{Checker, DocKind};
use makepad_widgets::*;
use source_pane::SourcePane;


script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    let BarTool = StageButton{width: Fit height: 22 margin: 0 padding: Inset{left: 8 right: 8} spacing: 6
        draw_text +: {text_style: theme.font_regular{font_size: 9}}}
    let BarIcon = StageIconButton{width: 22 height: 22 margin: 0 icon_walk: Walk{width: 7 height: Fit}}
    let BarMeta = Label{width: Fit text: "" draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 8}}}

    // The Source view of a document: its bar (file, state, the changes and
    // their diff) over the code editor.
    mod.widgets.DocSourcePane = View{width: Fill height: Fill flow: Down visible: false
        View{width: Fill height: 36 flow: Right spacing: 8 padding: Inset{left: 12 right: 8} align: Align{y: 0.5}
            file := Label{width: Fit text: "" draw_text +: {color: stage.secondary text_style: theme.font_bold{font_size: 8}}}
            View{width: 4 height: 1}
            state_tip := Tip{text: "" wrap_width: 420 View{width: Fit height: 22 flow: Overlay
                View{width: Fit height: 22 flow: Right spacing: 6 align: Align{y: 0.5}
                    state_dot := RoundedView{width: 6 height: 6 show_bg: true draw_bg +: {color: stage.live border_radius: 3.0}}
                    state := Label{width: Fit max_lines: 1 text: "" draw_text +: {color: stage.secondary text_style: theme.font_regular{font_size: 8}}}}
                state_button := Button{width: Fill height: Fill margin: 0 text: "" draw_bg +: {color: #0000 color_hover: #ffffff08 color_down: #0000 color_focus: #0000 border_size: 0.0}}}}
            Tip{text: "Copy the full messages" copy := BarTool{visible: false text: "Copy details"}}
            Tip{text: "Take the version written elsewhere; your draft is dropped" take := BarTool{visible: false text: "Take theirs"}}
            Tip{text: "Keep typing your draft; the other version stays in the history" keep := BarTool{visible: false text: "Keep mine"}}
            View{width: Fill height: 1}
            nav := View{visible: false width: Fit height: 22 flow: Right spacing: 6 align: Align{y: 0.5}
                Tip{text: "The previous change" change_prev := BarIcon{draw_icon +: {svg: crate_resource("self:resources/icons/ui/page_prev.svg")}}}
                change := BarMeta{}
                author_chip := RoundedView{width: Fit height: 16 padding: Inset{left: 6 right: 6} align: Align{y: 0.5} show_bg: true draw_bg +: {color: stage.raised border_radius: 3.0}
                    author := Label{width: Fit text: "" draw_text +: {color: stage.text text_style: theme.font_bold{font_size: 7}}}}
                what := BarMeta{}
                plus := Label{width: Fit text: "" draw_text +: {color: stage.live text_style: theme.font_regular{font_size: 8}}}
                minus := Label{width: Fit text: "" draw_text +: {color: stage.danger text_style: theme.font_regular{font_size: 8}}}
                Tip{text: "The next change" change_next := BarIcon{draw_icon +: {svg: crate_resource("self:resources/icons/ui/page_next.svg")}}}
            }
            Tip{text: "Show this change as a diff: the lines it removed and added" diff_toggle := BarTool{visible: false text: "Changes"}}
            Tip{text: "Someone else changed this file (the AI, a tool): see what changed" foreign_chip := BarTool{visible: false text: ""}}
            Tip{text: "Drop what you typed and show the document as it is" revert := BarTool{visible: false text: "Revert draft"}}
        }
        SolidView{width: Fill height: 1 draw_bg +: {color: stage.separator}}
        code_box := mod.widgets.SourceEditor{
            code := CodeView{
                editor +: {
                    width: Fill height: Fill
                    read_only: false show_gutter: true word_wrap: false
                    draw_bg +: {color: stage.well}
                    draw_text +: {text_style: theme.font_code{font_size: 9}}
                }
            }
        }
        diff_box := View{width: Fill height: Fill visible: false
            diff := CodeView{
                editor +: {
                    width: Fill height: Fill
                    read_only: true show_gutter: true word_wrap: false
                    draw_bg +: {color: stage.well}
                    draw_text +: {text_style: theme.font_code{font_size: 9}}
                }
            }
        }
    }

    let DeskPane = RoundedView{width: Fill height: Fill flow: Down spacing: 0 padding: 0 show_bg: true
        draw_bg +: {color: stage.panel border_color: stage.separator border_size: 1 border_radius: 5}}
    let DeskSplit = Splitter{size: 8 draw_bg +: {color_bg: stage.bg color: stage.border_hi color_hover: stage.accent color_drag: stage.accent splitter_pad: 3 bar_size: 24}}

    // An opened graphic: its viewer and inspector, and its code
    // timeline or source below (the project bar's toggle chooses).
    mod.widgets.MotionDesk = View{width: Fill height: Fill flow: Down spacing: 8 visible: false
        // (Its name, state, way back and undo are in the project bar: one
        // bar for the film and the graphic open in it.)
        // The scene's editor panel down the whole left side; beside it the
        // picture and the scene's chat, the code under them with its
        // scrubber over it. Every bar drags, the side panes fold (dragged
        // shut or a double click on the bar).
        m_split_left := DeskSplit{width: Fill height: Fill axis: SplitterAxis.Horizontal align: SplitterAlign.FromA(520) fold: SplitterCollapse.A min_vertical: 300 max_vertical: 420
            a: DeskPane{
                // The scene's editor panel (VR1's), the desk's whole height.
                View{width: Fill height: Fill flow: Down
                    m_selected_text := mod.widgets.SceneSelectedText{}
                    m_editor_panel := mod.widgets.EditorPanel{visible: false}
                    m_panel_making := mod.widgets.ScenePanelMaking{}
                    m_panel_sweep := mod.widgets.PanelSweep{}}
            }
            b: View{width: Fill height: Fill m_split := DeskSplit{width: Fill height: Fill axis: SplitterAxis.Vertical align: SplitterAlign.FromB(380) fold: SplitterCollapse.B min_horizontal: 200 max_horizontal: 140
                a: View{width: Fill height: Fill m_split_chat := DeskSplit{width: Fill height: Fill axis: SplitterAxis.Horizontal align: SplitterAlign.FromB(340) fold: SplitterCollapse.B min_vertical: 300 max_vertical: 240
                    a: DeskPane{
                        View{width: Fill height: Fill padding: 1 m_viewer := MotionViewer{}}
                        m_variants := mod.widgets.MotionVariantStrip{}
                    }
                    // The right column: the scene's structure and history
                    // (STRUCTURE.md: the tree, the selected node's stacks and
                    // parameters).
                    b: DeskPane{
                        View{width: Fill height: 30 flow: Right spacing: 2 padding: Inset{left: 8 right: 6} align: Align{y: 0.5}
                            Tip{text: "Structure: the scene's post, layers and what is attached to them. History: every state the scene has been in, a click goes back there (nothing is ever lost)" m_tab_top := CutSegmented{width: 150 labels: ["Structure" "History"]}}
                            View{width: Fill height: 1}}
                        SolidView{width: Fill height: 1 draw_bg +: {color: stage.separator}}
                        m_structure_page := View{width: Fill height: Fill m_structure := mod.widgets.StructureColumn{}}
                        m_history_page := View{width: Fill height: Fill visible: false m_history := mod.widgets.HistoryColumn{}}
                    }
                }}
                b: DeskPane{
                    m_layers_box := View{width: Fill height: Fill flow: Overlay
                        // Two pages, each the whole pane (Tab flips them);
                        // their one header line starts with the switch,
                        // laid over its left end.
                        // (A hidden page gets no events: its wrapper says so.)
                        m_code_page := View{width: Fill height: Fill m_code := MotionCodeTimeline{}}
                        m_curves := mod.widgets.CurvesPage{}
                        View{width: Fit height: 30 flow: Right spacing: 0 padding: Inset{left: 8 top: 5}
                            Tip{text: "The scene's code, or its animated values as curves (Tab)" m_tab_layers := CutSegmented{width: 124 height: 20 labels: ["Code" "Curves"]}}}}
                    m_source_pane := mod.widgets.DocSourcePane{}
                }
            }}
        }
    }
}

pub fn register(vm: &mut ScriptVm) {
    makepad_code_editor::script_mod(vm);
    source_pane::script_mod(vm);
    code_timeline::script_mod(vm);
    inspector::script_mod(vm);
    value_pick::script_mod(vm);
    viewer::script_mod(vm);
    variant_strip::script_mod(vm);
    variants::script_mod(vm);
    scene_context::script_mod(vm);
    panel_sweep::script_mod(vm);
    selected_text::script_mod(vm);
    structure_panel::script_mod(vm);
    curve_editor::script_mod(vm);
    history_panel::script_mod(vm);
    script_mod(vm);
}

/// Which view of the open document the page shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewMode {
    #[default]
    Timeline,
    Source,
    /// The storyboard the cut is made from (`storyboard/`, SB1).
    Storyboard,
}

impl ViewMode {
    fn of(index: usize) -> Self {
        match index {
            1 => Self::Source,
            2 => Self::Storyboard,
            _ => Self::Timeline,
        }
    }
}

/// The views' state (a field of the page).
pub struct MotionEdit {
    started: bool,
    pub(super) mode: ViewMode,
    checker: Option<Checker>,
    serial: u64,
    cut_source: SourcePane,
    active_file: String,
    file_sources: std::collections::HashMap<String, SourcePane>,
    files_seen: Option<(u64, String)>,
    folded: std::collections::HashSet<String>,
    recovered: Vec<makepad_library_browser::drafts::DraftSource>,
    /// The page revision the views last showed, and who made the change
    /// that the next revision is (set just before it is submitted).
    seen: Option<u64>,
    author: Option<String>,
    /// The asset the open cut was opened as, so a new document resets the
    /// views' history and layout.
    document: Option<String>,
    /// An opened graphic.
    desk: desk::Desk,
    /// Who made the cut's last change (a graphic it re-points follows it).
    last_author: String,
    /// Variants of the open graphic (`variants.rs`).
    variants: variants::Variants,
    /// The scene's editor panel (`editor_panel.rs`).
    panel: editor_panel::Panel,
    /// The editor panel's slot while there is no panel (`scene_context.rs`).
    slot: scene_context::Slot,
    /// Text picked in the picture, edited where it comes from.
    selected_text: selected_text::SelectedText,
    /// The deep make of the scene's panel (`deep_make.rs`).
    deep: Option<deep_make::Deep>,
    /// A font or a colour being picked (`value_pick.rs`).
    value_pick: value_pick::ValuePick,
    /// The scene's structure, right of the picture (`structure_panel.rs`).
    structure: structure_panel::Structure,
    /// The bottom pane's CURVES page (`curve_editor.rs`).
    curves: curve_editor::Curves,
    /// The structure pane's HISTORY tab (`history_panel.rs`).
    history: history_panel::History,
}

impl Default for MotionEdit {
    fn default() -> Self {
        Self {
            started: false,
            mode: ViewMode::Timeline,
            checker: None,
            serial: 0,
            cut_source: SourcePane::new(DocKind::Cut, live_id!(cut_source_pane), "cut.splash"),
            active_file: "cut.splash".into(),
            file_sources: Default::default(),
            files_seen: None,
            folded: Default::default(),
            recovered: Vec::new(),
            seen: None,
            author: None,
            document: None,
            desk: desk::Desk::default(),
            last_author: String::new(),
            variants: Default::default(),
            panel: Default::default(),
            slot: Default::default(),
            selected_text: Default::default(),
            deep: None,
            value_pick: Default::default(),
            structure: Default::default(),
            curves: Default::default(),
            history: Default::default(),
        }
    }
}

/// Open a block's document when it is a Graphic (the timeline's double
/// click); false leaves it to the page (a compound clip).
/// Whether a graphic is open (its desk covers the cut's viewer).
pub fn is_open(page: &VideoCut) -> bool {
    page.motion.desk.session.is_some()
}

pub fn open(page: &mut VideoCut, node: &str) -> bool {
    desk::open_graphic(page, node)
}

/// Name who makes the next change to the open cut ("AI · iteration 2"),
/// so the Source view's history shows it. The AI loop calls this before it
/// submits its ops.
pub fn note_author(page: &mut VideoCut, author: &str) {
    page.motion.author = Some(author.to_string());
}

pub(super) fn save_sources(page: &mut VideoCut) -> Result<(), String> { desk::stage_tree_sources(page) }

pub(super) fn selected_file(page: &VideoCut) -> &str {
    page.motion.desk.session.as_ref().and_then(|session| session.tree_path.as_deref()).unwrap_or(&page.motion.active_file)
}

pub(super) fn draft_stamp(page: &VideoCut) -> u64 {
    page.motion.cut_source.draft_revision
        + page.motion.file_sources.values().map(|pane| pane.draft_revision).sum::<u64>()
        + page.motion.desk.source.as_ref().map_or(0, |pane| pane.draft_revision)
        + page.motion.desk.session.as_ref().map_or(0, |session| session.revision)
}

pub(super) fn capture_draft(page: &VideoCut, draft: &mut makepad_library_browser::drafts::DraftTree) {
    draft.pending.clear();
    for change in source_changes(page) {
        if let makepad_app_shell::tool_host::Change::Write { path, text, expected } = change {
            draft.pending.push(makepad_library_browser::drafts::DraftSource { path, text, base: expected });
        }
    }
    draft.selected = selected_file(page).into();
}

pub(super) fn opened(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef) {
    let path = page.session.as_ref().filter(|session| session.tree.is_some()).map_or("cut.splash", |session| session.path.as_str()).to_string();
    page.motion.cut_source.reset(cx, ui, &path);
    page.motion.active_file = path;
    page.motion.document = page.session.as_ref().map(|session| session.head.map(|head| head.asset_id.to_string()).unwrap_or_else(|| session.doc.label.clone()));
    page.motion.file_sources.clear();
    page.motion.recovered.clear();
    page.motion.files_seen = None;
    page.motion.seen = None;
    desk::reset(page);
}

pub(super) fn restore_draft(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef, draft: makepad_library_browser::drafts::DraftTree) {
    for source in draft.pending {
        if draft.draft.files.get(&source.path).is_some_and(|file| file.kind == makepad_asset_data::AssetKind::Motion && !makepad_motion::modules::is_module(file.text().unwrap_or(""))) {
            page.motion.recovered.push(source);
            continue;
        }
        let mut pane = SourcePane::new(DocKind::Cut, live_id!(cut_source_pane), &source.path);
        pane.tree = Some(draft.draft.clone());
        if let Ok(text) = draft.draft.text(&source.path) { pane.sync_detached(text, "Recovered draft"); }
        pane.restore_draft(source.text, source.base);
        if source.path == page.motion.active_file { page.motion.cut_source = pane; }
        else { page.motion.file_sources.insert(source.path, pane); }
    }
    let _ = open_file(page, cx, ui, &draft.selected);
    page.motion.cut_source.resume(cx, ui);
}

pub(super) fn source_changes(page: &VideoCut) -> Vec<makepad_app_shell::tool_host::Change> {
    use makepad_app_shell::tool_host::Change;
    let mut changes: Vec<_> = std::iter::once((&page.motion.active_file, &page.motion.cut_source)).chain(page.motion.file_sources.iter())
        .filter_map(|(path, pane)| Some(Change::Write { path: path.clone(), text: pane.draft_text()?.into(), expected: pane.draft_base().map(str::to_string) }))
        .chain(page.motion.recovered.iter().map(|source| Change::Write { path: source.path.clone(), text: source.text.clone(), expected: source.base.clone() })).collect();
    if let Some(session) = &page.motion.desk.session {
        if let Some(path) = &session.tree_path {
            let pane = page.motion.desk.source.as_ref();
            let text = pane.and_then(SourcePane::draft_text).unwrap_or(&session.source);
            if text != session.saved || pane.is_some_and(SourcePane::has_draft) {
                changes.push(Change::Write { path: path.clone(), text: text.into(), expected: Some(session.saved.clone()) });
            }
            for module in session.modules.iter().filter(|module| !module.builtin() && module.source != module.saved) {
                changes.push(Change::Write { path: module.id.clone(), text: module.source.clone(), expected: Some(module.saved.clone()) });
            }
        }
    }
    changes
}

pub(super) fn has_file_draft(page: &VideoCut, path: &str) -> bool {
    page.motion.recovered.iter().any(|source| source.path == path)
        || page.motion.desk.session.as_ref().is_some_and(|session| session.tree_path.as_deref() == Some(path) && (session.dirty() || page.motion.desk.source.as_ref().is_some_and(SourcePane::has_draft)))
        || if path == page.motion.active_file { page.motion.cut_source.has_draft() } else { page.motion.file_sources.get(path).is_some_and(SourcePane::has_draft) }
}

pub(super) fn has_drafts(page: &VideoCut) -> bool {
    page.motion.cut_source.has_draft() || page.motion.file_sources.values().any(SourcePane::has_draft)
        || !page.motion.recovered.is_empty() || page.motion.desk.source.as_ref().is_some_and(SourcePane::has_draft)
        || page.motion.desk.session.as_ref().is_some_and(|session| session.tree_path.is_some() && session.dirty())
}

pub(super) fn open_file(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef, path: &str) -> Result<(), String> {
    let tree = page.session.as_ref().and_then(|session| session.tree.clone()).ok_or("no cut tree is open")?;
    let file = tree.files.get(path).ok_or_else(|| format!("no file `{path}`"))?;
    if file.kind == makepad_asset_data::AssetKind::Motion && !makepad_motion::modules::is_module(file.text()?) { return desk::open_tree_file(page, path, None); }
    if page.motion.active_file != path {
        let kind = if file.kind == makepad_asset_data::AssetKind::Deck { DocKind::Deck } else { DocKind::Cut };
        let pane = page.motion.file_sources.remove(path).unwrap_or_else(|| SourcePane::new(kind, live_id!(cut_source_pane), path));
        let mut old = std::mem::replace(&mut page.motion.cut_source, pane);
        old.suspend();
        page.motion.file_sources.insert(std::mem::replace(&mut page.motion.active_file, path.into()), old);
        page.motion.cut_source.resume(cx, ui);
        page.motion.seen = None;
        page.motion.files_seen = None;
    }
    Ok(())
}

fn start(page: &mut VideoCut, cx: &mut Cx) {
    let motion = &mut page.motion;
    motion.started = true;
    let checker = Checker::new(cx);
    if let Some(error) = &checker.error {
        page.status = error.clone();
    }
    motion.checker = Some(checker);
}

pub fn stop(page: &mut VideoCut) {
    if let Some(checker) = &mut page.motion.checker {
        checker.stop();
    }
    if let Some(store) = &mut page.motion.desk.store {
        store.stop();
    }
}

/// Page events: the Source views' settle timers, the desk's frames.
/// Whether a graphic is open in the desk.
pub(super) fn scene_open(page: &VideoCut) -> bool {
    page.motion.desk.session.is_some()
}

/// What the one chat is told of the graphic desk (`scene_context.rs`).
pub(super) fn chat_context(page: &VideoCut) -> String {
    scene_context::context(page)
}

/// The desk's chips in the one chat.
pub(super) fn chat_suggestions(page: &VideoCut) -> Vec<makepad_ai_services::Suggestion> {
    scene_context::suggestions(page)
}

/// The node picked in the desk's structure, as the chat's selection.
pub(super) fn chat_selection(page: &VideoCut) -> Option<String> {
    page.motion.structure.selected.clone()
}

/// The chat's clear button on the selection: nothing picked in the desk.
pub(super) fn clear_chat_selection(page: &mut VideoCut) {
    page.motion.structure.selected = None;
}

/// The picture the desk's viewer shows, for the model: its texture and a
/// key that changes when the picture does (the moment, the revision, the
/// selection).
pub(super) fn chat_look(page: &VideoCut, cx: &mut Cx, ui: &WidgetRef) -> Option<(String, Option<Texture>)> {
    let session = page.motion.desk.session.as_ref()?;
    let key = format!("scene:{}:{:.2}:{}", session.revision, session.playhead, page.motion.structure.selected.as_deref().unwrap_or(""));
    let texture = ui.widget(cx, ids!(cut_motion)).widget(cx, ids!(m_viewer)).borrow::<viewer::MotionViewer>().and_then(|v| v.shown_picture()).map(|(t, _)| t);
    Some((key, texture))
}

pub fn handle_event(page: &mut VideoCut, event: &Event) {
    let motion = &mut page.motion;
    if motion.cut_source.handle_event(event) {
        if let Some(checker) = &motion.checker {
            motion.cut_source.pump(checker, &mut motion.serial);
        }
    }
    desk::handle_event(page, event);
}

/// Page actions: the view toggle, the Source view.
pub fn handle_actions(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
    use crate::structure::StructureAction;
    for action in actions.filter_widget_actions_cast::<StructureAction>(ui.widget(cx, ids!(cut_files)).widget_uid()) {
        match action {
            StructureAction::Select(path) if !path.ends_with('/') => {
                if let Err(error) = open_file(page, cx, ui, &path) { page.status = error; }
            }
            StructureAction::Select(path) | StructureAction::Fold(path) => {
                if !page.motion.folded.remove(&path) { page.motion.folded.insert(path); }
                page.motion.files_seen = None;
            }
            _ => {}
        }
    }
    let view_mode = ui.widget(cx, ids!(cut_view_mode));
    for action in actions.filter_widget_actions_cast::<super::controls::SegmentAction>(view_mode.widget_uid()) {
        if let super::controls::SegmentAction::Selected(index) = action {
            set_mode(page, cx, ui, ViewMode::of(index));
        }
    }
    if page.motion.desk.is_open() {
        desk::handle_actions(page, cx, ui, actions);
        return;
    }
    // Enter on a selected Graphic opens it, as a double click does.
    let timeline = ui.widget(cx, ids!(cut_timeline));
    for action in actions.filter_widget_actions_cast::<super::timeline::TimelineAction>(timeline.widget_uid()) {
        if let super::timeline::TimelineAction::Key(key) = action {
            if matches!(key.key_code, KeyCode::ReturnKey | KeyCode::NumpadEnter) {
                if let Some(node) = page.selected.clone() {
                    open(page, super::split_selection(&node).0);
                }
            }
        }
    }
    page.motion.cut_source.handle_actions(cx, ui, actions);
}

fn set_mode(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef, mode: ViewMode) {
    let motion = &mut page.motion;
    if motion.mode == mode {
        return;
    }
    motion.mode = mode;
    // A view that was hidden catches up on what it missed.
    motion.seen = None;
    motion.desk.repaint = true;
    if let Some(mut segmented) = ui.widget(cx, ids!(cut_view_mode)).borrow_mut::<super::controls::CutSegmented>() {
        segmented.set_active(cx, mode as usize);
    }
    page.repaint = true;
}

/// Every pump, after the page's own: the views follow the document.
pub fn pump(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef, shown: bool) {
    if page.stopped {
        return;
    }
    if !page.motion.started {
        start(page, cx);
    }
    // Checked drafts and loaded graphics; a proven edit draft is applied
    // as one change.
    while let Some(checked) = page.motion.checker.as_ref().and_then(Checker::try_recv) {
        if page.motion.cut_source.checked(cx, ui, &checked) || desk::checked(page, &checked) {
            continue;
        }
        if let Some(mut pane) = page.motion.desk.source.take() {
            pane.checked(cx, ui, &checked);
            page.motion.desk.source = Some(pane);
        }
    }
    if let Some(source) = page.motion.cut_source.take_proven() {
        if page.session.as_ref().is_some_and(|session| session.tree.is_some()) {
            let path = page.motion.active_file.clone();
            let expected = page.motion.cut_source.draft_base().map(str::to_string);
            page.submit(super::Pending::Document(vec![makepad_app_shell::tool_host::Change::Write { path, text: source, expected }]));
        } else if page.session.as_ref().is_some_and(|session| session.source != source) {
            note_author(page, "You · source");
            page.op(super::ops::CutOp::Source { text: source });
        }
    }
    {
        let motion = &mut page.motion;
        if let Some(checker) = &motion.checker {
            motion.cut_source.pump(checker, &mut motion.serial);
            if let Some(pane) = &mut motion.desk.source {
                pane.pump(checker, &mut motion.serial);
            }
        }
    }
    let revision = page.session.as_ref().map(|session| session.revision);
    let document = page.session.as_ref().map(|session| session.head.map(|head| head.asset_id.to_string()).unwrap_or_else(|| session.doc.label.clone()));
    if document != page.motion.document {
        // Another cut: its own history and layout.
        page.motion.document = document.clone();
        let path = page.session.as_ref().filter(|session| session.tree.is_some()).map_or("cut.splash", |session| session.path.as_str()).to_string();
        page.motion.cut_source.reset(cx, ui, &path);
        page.motion.active_file = path;
        page.motion.file_sources.clear();
        page.motion.files_seen = None;
        page.motion.seen = None;
    }
    if revision != page.motion.seen {
        if let Some(path) = page.session.as_ref().filter(|session| session.tree.as_ref().is_some_and(|tree| !tree.files.contains_key(&page.motion.active_file))).map(|session| session.path.clone()) {
            let _ = open_file(page, cx, ui, &path);
        }
        page.motion.seen = revision;
        let author = page.motion.author.take().unwrap_or_else(|| "Cut".into());
        page.motion.last_author = author.clone();
        if let Some(session) = &page.session {
            if let Some(tree) = &session.tree {
                page.motion.recovered.retain(|source| tree.text(&source.path).ok() != Some(source.text.as_str()));
                page.motion.cut_source.tree = Some(tree.clone());
                for (path, pane) in &mut page.motion.file_sources { pane.tree = Some(tree.clone()); if let Ok(text) = tree.text(path) { pane.sync_detached(text, &author); } }
                if let Ok(source) = tree.text(&page.motion.active_file) { page.motion.cut_source.sync(cx, ui, source, &author); }
            } else {
                page.motion.cut_source.tree = None;
                page.motion.cut_source.sync(cx, ui, &session.source, &author);
            }
        }
    }
    desk::pump(page, cx, ui, shown);
    if !shown || page.motion.desk.is_open() { return; }
    let mode = page.motion.mode;
    let open = page.session.is_some();
    ui.widget(cx, ids!(cut_timeline_box)).set_visible(cx, mode == ViewMode::Timeline);
    ui.widget(cx, ids!(cut_tl_bar)).set_visible(cx, open && mode == ViewMode::Timeline);
    ui.widget(cx, ids!(cut_source_box)).set_visible(cx, open && mode == ViewMode::Source);
    ui.widget(cx, ids!(cut_board_pane)).set_visible(cx, mode == ViewMode::Storyboard);
    super::storyboard::set_shown(cx, ui, mode == ViewMode::Storyboard);
    let tree = page.session.as_ref().and_then(|session| session.tree.as_ref());
    ui.widget(cx, ids!(cut_files_box)).set_visible(cx, tree.is_some());
    let binary = tree.and_then(|tree| tree.files.get(&page.motion.active_file)).is_some_and(|file| matches!(file.kind, makepad_asset_data::AssetKind::Texture | makepad_asset_data::AssetKind::Video | makepad_asset_data::AssetKind::Audio));
    ui.widget(cx, ids!(cut_source_pane)).set_visible(cx, !binary);
    ui.widget(cx, ids!(cut_file_binary)).set_visible(cx, binary);
    if let Some(session) = &page.session {
        if let Some(tree) = &session.tree {
            let key = (session.revision, page.motion.active_file.clone());
            if page.motion.files_seen.as_ref() != Some(&key) {
                page.motion.files_seen = Some(key);
                let rows = crate::document_tree::file_rows(tree, session.saved_tree.as_deref(), &page.motion.folded);
                if let Some(mut widget) = ui.widget(cx, ids!(cut_files)).borrow_mut::<crate::structure::StructureTree>() { widget.set_model(cx, rows, Vec::new(), Some(page.motion.active_file.clone())); }
                if binary { ui.label(cx, ids!(cut_file_binary)).set_text(cx, &format!("{} · imported media", page.motion.active_file)); }
            }
        }
    }
    let p = makepad_look::palette::get(cx);
    page.motion.cut_source.paint(cx, ui, &p);
}

