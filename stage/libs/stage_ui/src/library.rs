//! Library list. The parent supplies rows and reads [`LibraryAction`].
//! This widget draws and reports only.
//!
//! The operator chooses its columns (COLUMNS, and dragging a head), narrows
//! it to what has been worked out (FILTER), sorts it by any head, and walks
//! it from the keyboard (`library_nav`'s verbs). The rules are in
//! [`crate::track_list`]; the column layout is [`crate::columns`].

use crate::columns::{Column, ColumnLayout};
use crate::library_nav::{self, CursorMove, LibraryCmd};
use crate::model::DeckId;
pub use crate::track_list::LibraryRow;
use crate::track_list::{self, HAS};
use crate::track_menu::{track_menu, TrackVerb};
use makepad_widgets::menu::{menu_picked, MenuAction, MenuPlace, MenuRow};
use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    // The explorer's authored row palette and geometry come from
    // donors/stage/src/music_view.rs::VjTrackList (1617–1682, 1798–1824):
    // 22-point rows with a one-point gap, quiet heads and slate selection.
    let LibraryButton = StageButton{
        height: 22
        draw_bg +: {color_disabled: stage.ghost_face border_color_disabled: stage.line_faint}
        draw_text +: {color_disabled: stage.dim_lo}
    }
    // A set list's switch: lit while it is on (the host sets it).
    let LibraryToggle = CheckBoxCustom{
        width: Fit height: 22 margin: 0 padding: Inset{left: 8 right: 8}
        align: Align{x: 0.5 y: 0.5} label_align: Align{x: 0.5 y: 0.5}
        icon_walk: Walk{width: 0 height: 0}
        label_walk: Walk{width: Fit height: Fit margin: 0}
        draw_bg +: {
            color: stage.control color_hover: stage.control_hover color_down: stage.control_down
            color_active: stage.accent color_disabled: stage.ghost_face
            active_hover: uniform(stage.accent_hover) active_down: uniform(stage.accent_down)
            border_color: stage.border_hi border_color_disabled: stage.line_faint
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                let off = self.color.mix(self.color_hover, self.hover).mix(self.color_down, self.down)
                let on = self.color_active.mix(self.active_hover, self.hover).mix(self.active_down, self.down)
                sdf.box(0.5, 0.5, self.rect_size.x - 1., self.rect_size.y - 1., 2.5)
                sdf.fill(off.mix(on, self.active).mix(self.color_disabled, self.disabled))
                sdf.stroke(self.border_color.mix(self.border_color_disabled, self.disabled), 1.)
                return sdf.result
            }
        }
        draw_text +: {
            color: stage.label color_hover: stage.text_hi color_active: stage.on_accent
            color_disabled: stage.dim_lo text_style: theme.font_regular{font_size: 9}
        }
    }
    // A segmented control: a quiet well around buttons that sit edge to
    // edge; the lit one wears Stage's picked look (the host paints it).
    let LibrarySegments = RoundedView{
        width: Fit height: 22 flow: Right spacing: 1 padding: 1 align: Align{y: 0.5}
        show_bg: true
        draw_bg +: {color: stage.well border_color: stage.border border_size: 1 border_radius: 2.5}
    }
    let LibrarySegment = StageButton{
        width: Fit height: 20 margin: 0 padding: Inset{left: 9 right: 9 top: 0 bottom: 0}
        draw_bg +: {
            color: #x00000000 color_focus: #x00000000 color_hover: stage.control_hover
            border_color: #x00000000 border_size: 1 border_radius: 2
        }
        draw_text +: {text_style: theme.font_bold{font_size: 8}}
    }
    mod.widgets.LibraryViewBase = #(LibraryView::register_widget(vm))
    mod.widgets.LibraryView = set_type_default() do mod.widgets.LibraryViewBase{
        width: Fill height: Fill flow: Down spacing: 4 padding: 0
        show_bg: true
        draw_bg: mod.widgets.RoundedView.draw_bg{
            color: stage.bg border_color: stage.bg border_size: 0 border_radius: 0
        }
        // ONE ROW: the source and the view as two segmented controls, the
        // search (Enter searches), the ⋯ menu (filter, columns, sort,
        // refresh), IMPORT and the count. Every item is 22 high.
        View{width: Fill height: 22 flow: Right spacing: 8 align: Align{y: 0.5}
            dj_scope_bar := LibrarySegments{visible: false
                dj_scope_library := LibrarySegment{text: "LIBRARY"}
                dj_scope_video := LibrarySegment{text: "VIDEO" visible: false}
                dj_scope_web := LibrarySegment{text: "WEB" visible: false}
            }
            view_bar := LibrarySegments{
                Tip{text: "The library: search, sort and load" list_explorer := LibrarySegment{text: "EXPLORER"}}
                Tip{text: "The set list the decks take from" list_queue := LibrarySegment{text: "QUEUE"}}
                Tip{text: "What played tonight, newest first" list_history := LibrarySegment{text: "HISTORY"}}
                Tip{text: "Each deck's loops: a row per stem and the mix, a column per section; press one to loop it" list_loops := LibrarySegment{text: "LOOPS"}}
            }
            search := TextInput{
                width: Fill{min: 96} height: 22
                flow: Flow.Right{wrap: false}
                padding: Inset{left: 6 right: 6 top: 3 bottom: 3}
                empty_text: "search music…  (Up: earlier searches)"
                draw_bg +: {
                    color: stage.well color_hover: stage.well color_focus: stage.well
                    color_down: stage.well color_empty: stage.well
                    color_2: vec4(-1., -1., -1., -1.)
                    border_color: stage.border border_color_empty: stage.border
                    border_color_hover: stage.border_hi border_color_focus: stage.accent
                    border_color_down: stage.accent border_size: 1 border_radius: 2.5
                }
                draw_text +: {
                    color: stage.label color_hover: stage.text_hi color_focus: stage.text
                    color_down: stage.text color_empty: stage.dim
                    text_style: theme.font_regular{font_size: 9}
                }
            }
            Tip{text: "Filter, columns, sort and refresh" more := StageIconButton{width: 26 margin: 0 draw_icon +: {svg: crate_resource("self:resources/icons/more.svg")}}}
            // Pick a folder of music (or drop files on the DJ page).
            Tip{text: "Import music: pick a folder (or drop files on this page)" import := StageIconButton{width: 26 margin: 0 draw_icon +: {svg: crate_resource("self:resources/icons/import.svg")}}}
            status := Label{
                width: Fit height: Fit padding: 0 text: "" max_lines: 1
                flow: Flow.Right{wrap: false}
                draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 9}}
            }
        }
        // The set lists, while the queue shows: which list is on the decks,
        // its name, and its switches.
        setlist_bar := View{width: Fill height: 22 flow: Right spacing: 6 align: Align{y: 0.5} visible: false
            Tip{text: "The set list the decks take from; each keeps its own tracks" setlist_pick := DropDown{width: 140 height: 22 labels: ["SET LIST"]}}
            Tip{text: "Rename this set list (Enter keeps the name)" setlist_name := TextInput{
                width: 140 height: 22
                flow: Flow.Right{wrap: false}
                padding: Inset{left: 6 right: 6 top: 3 bottom: 3}
                empty_text: "name…"
                draw_bg +: {
                    color: stage.well color_hover: stage.well color_focus: stage.well
                    color_down: stage.well color_empty: stage.well
                    color_2: vec4(-1., -1., -1., -1.)
                    border_color: stage.border border_color_empty: stage.border
                    border_color_hover: stage.border_hi border_color_focus: stage.accent
                    border_color_down: stage.accent border_size: 1 border_radius: 2.5
                }
                draw_text +: {
                    color: stage.label color_hover: stage.text_hi color_focus: stage.text
                    color_down: stage.text color_empty: stage.dim
                    text_style: theme.font_regular{font_size: 9}
                }
            }}
            Tip{text: "A new, empty set list" setlist_new := LibraryButton{width: Fit text: "NEW"}}
            Tip{text: "Take this set list off the shelf (the last one stays)" setlist_drop := LibraryButton{width: Fit text: "DROP"}}
            Tip{text: "Empty this set list" setlist_clear := LibraryButton{width: Fit text: "CLEAR"}}
            Tip{text: "A programme: nothing changes it and nothing eats it; it plays through in order" setlist_lock := LibraryToggle{text: "LOCK"}}
            Tip{text: "Start over at the end: a played track goes to the back" setlist_repeat := LibraryToggle{text: "REPEAT"}}
            Tip{text: "Take the next track at random" setlist_shuffle := LibraryToggle{text: "SHUFFLE"}}
            Tip{text: "An empty deck, or one that ran off the end of its record, loads the next track by itself" setlist_auto := LibraryToggle{text: "AUTO"}}
            Tip{text: "Put the library's best fit for the record the room hears at the front of the set list (the reason shows here)" setlist_pick_next := LibraryButton{width: Fit text: "PICK"}}
            Tip{text: "When the set list runs out, AUTO and the Auto DJ pick the next record from the library" setlist_fill := LibraryToggle{text: "FILL"}}
        }
        files := DataGrid{
            width: Fill height: Fill
            rows: 0 cols: 1
            selection: GridSelectMode.Rows
            grab_key_focus: true
            // A carried row reorders the set list or loads where it lands.
            row_drag: true
            show_row_headers: false
            allow_row_resize: false
            allow_col_resize: true
            allow_col_reorder: true
            // Only the sorted head wears a mark, as in the donor list.
            sortable: false
            header_edges: true
            zebra_stripes: true
            default_row_height: 23.0
            default_col_width: 160.0
            col_header_height: 22.0
            min_col_width: 28.0
            cell_pad_x: 6.0
            header_align: 0.0
            cell_cursor: MouseCursor.Hand
            color_bg: stage.bg
            color_cell: stage.row
            color_cell_alt: stage.row_alt
            color_text: stage.label
            color_header: stage.bg
            color_header_active: stage.raised
            color_header_text: stage.secondary
            color_header_sorted: stage.text_hi
            color_header_edge: stage.edge
            // The keyed selection is painted into the cells below.
            color_selection: #x00000000
            color_selection_border: #x00000000
            color_drag_marker: stage.accent
            color_resize_guide: stage.accent_a80
            draw_cell +: {border_color: uniform(stage.bg)}
            draw_text +: {
                color: stage.label
                text_style: theme.font_regular{font_size: 9}
            }
            draw_text_bold +: {
                color: stage.label
                text_style: theme.font_bold{font_size: 9}
            }
            draw_text_header +: {text_style: theme.font_bold{font_size: 8}}
            scroll_bar_h +: {
                draw_bg +: {color: uniform(stage.edge_lo) color_hover: uniform(stage.accent_tint) color_drag: uniform(stage.accent) border_size: 0}
            }
            scroll_bar_v +: {
                draw_bg +: {color: uniform(stage.edge_lo) color_hover: uniform(stage.accent_tint) color_drag: uniform(stage.accent) border_size: 0}
            }
        }
        actions_row := View{width: Fill height: 22 flow: Right spacing: 4 align: Align{y: 0.5}
            // The bake, first so a narrow pane never clips it: the selected
            // track's stems, lyrics and BPM, and every track still without
            // stems; STOP while it runs.
            Tip{text: "Bake the selected track on the AI hub: its stems, lyrics and BPM (STOP while it runs). The bake icon on a deck bakes the track it plays" analyse := LibraryButton{width: Fit text: "BAKE"}}
            Tip{text: "Bake stems, lyrics and BPM for every track without stems, one after another in the background (STOP ALL ends it)" bake_all := LibraryButton{width: Fit text: "BAKE ALL"}}
            load_a := LibraryButton{width: Fit text: "LOAD A"}
            load_b := LibraryButton{width: Fit text: "LOAD B"}
            Tip{text: "Put it at the back of the set list (Q)" enqueue := LibraryButton{width: Fit text: "+ SET LIST"}}
            Tip{text: "Put it at the front of the set list: the next a deck takes (N)" play_next := LibraryButton{width: Fit text: "NEXT"}}
            Tip{text: "Take it off the set list (Delete)" unqueue := LibraryButton{width: Fit text: "REMOVE" visible: false}}
            Tip{text: "One place earlier (or drag the row)" move_up := LibraryButton{width: Fit text: "UP" visible: false}}
            Tip{text: "One place later (or drag the row)" move_down := LibraryButton{width: Fit text: "DOWN" visible: false}}
            analysis_note := Label{
                width: Fill height: Fit padding: 0 text: ""
                draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 8}}
            }
        }
    }
}

/// The analysis controls for the selected row, as the parent sees them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LibraryAnalysis {
    /// `BAKE`, `BAKED` or `STOP`; empty hides the controls.
    pub action: String,
    pub enabled: bool,
    /// Shown while stems are switched on: what the separator's weights
    /// allow, and whether they are installed.
    pub stems_note: String,
    /// What the analysis is doing, or its last verdict.
    pub status: String,
    /// `BAKE ALL (n)` or `STOP ALL`; empty hides it.
    pub bake_all: String,
}

/// What the list shows: the catalogue, the active set list, or what played.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LibraryMode {
    #[default]
    Explorer,
    Queue,
    History,
    /// The decks' loop splats: the host shows its loops view instead.
    Loops,
}

/// The set lists as the bar shows them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SetListsView {
    pub names: Vec<String>,
    pub active: usize,
    pub locked: bool,
    pub repeat: bool,
    pub shuffle: bool,
    pub auto: bool,
    pub fill: bool,
}

/// A set list verb. Rows are named by the parent's row index.
#[derive(Clone, Debug, PartialEq)]
pub enum SetListAsk {
    Switch(usize),
    New,
    Drop,
    Rename(String),
    Clear,
    Lock(bool),
    Repeat(bool),
    Shuffle(bool),
    Auto(bool),
    Fill(bool),
    /// The library's best fit to the front of the set list.
    Pick,
    /// The selected row to the back of the set list.
    Enqueue,
    /// The selected row to its front.
    PlayNext,
    /// The selected row off the set list.
    Unqueue,
    /// The set list's row `from` moved to stand before row `to` (the
    /// list's length: at the end).
    Move { from: usize, to: usize },
}

/// Reports from the view. Setters do not emit these.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum LibraryAction {
    #[default]
    None,
    Select(String),
    /// Text changed; the parent debounces the request on its existing tick.
    SearchChanged(String),
    /// Enter or the Search button submits immediately.
    Search(String),
    /// Up or Down in the search box brought back an earlier query (or the
    /// text being typed): search it, it is already remembered.
    Recall(String),
    Refresh,
    /// The list's order changed: `true` is newest on top (the parent's own
    /// order), `false` a sort by a column. The parent remembers it.
    Sort(bool),
    /// A view pill pressed: the list the library should show.
    List(LibraryMode),
    /// `0` is Load A, `1` is Load B.
    Load(usize),
    /// A row was carried off the list and let go at this point (window
    /// coordinates): the parent loads it onto the deck it was dropped on.
    Dropped(DVec2),
    /// Enter on the list: load the selected row onto a deck that is not
    /// playing (the parent knows which).
    LoadIdle,
    /// BAKE on the selected row: its stems, lyrics and BPM on the AI hub,
    /// or stop the bake that runs.
    Bake,
    /// BAKE ALL: every listed track without stems to the AI hub, in the
    /// background (or stop the bake that runs).
    BakeAll,
    /// The column layout changed; its settings line, for the parent to keep.
    Columns(String),
    /// IMPORT: pick music to bring into the library.
    Import,
    SetList(SetListAsk),
}

/// What a carried row let go does.
#[derive(Clone, Copy, Debug, PartialEq)]
enum DragEnd {
    /// Off the list: the parent loads it where it landed.
    Load,
    /// Inside the set list: it moves to the gap under the pointer.
    Reorder,
    Nothing,
}

fn drag_end(inside_list: bool, mode: LibraryMode, can_load: bool) -> DragEnd {
    match (inside_list, mode) {
        (false, _) if can_load => DragEnd::Load,
        (true, LibraryMode::Queue) => DragEnd::Reorder,
        _ => DragEnd::Nothing,
    }
}

/// The track menu's rows by verb.
const MENU_VERBS: [(LiveId, TrackVerb); 5] = [
    (live_id!(track_load_a), TrackVerb::LoadA),
    (live_id!(track_load_b), TrackVerb::LoadB),
    (live_id!(track_enqueue), TrackVerb::Enqueue),
    (live_id!(track_play_next), TrackVerb::PlayNext),
    (live_id!(track_unqueue), TrackVerb::Unqueue),
];

fn menu_id(verb: TrackVerb) -> LiveId {
    MENU_VERBS.iter().find(|(_, held)| *held == verb).map_or(LiveId(0), |(id, _)| *id)
}

/// The view pills and the list each shows.
const VIEW_TABS: [(LiveId, LibraryMode); 4] = [
    (live_id!(list_explorer), LibraryMode::Explorer),
    (live_id!(list_queue), LibraryMode::Queue),
    (live_id!(list_history), LibraryMode::History),
    (live_id!(list_loops), LibraryMode::Loops),
];

/// A ⋯ menu row's id: `<name>_<index>` under the menu's own prefix.
fn more_id(name: &str, index: usize) -> LiveId {
    LiveId::from_str(&format!("more_{name}_{index}"))
}

/// A segment of a segmented control: Stage's picked look (the accent's
/// wash inside an accent edge) when lit, bare ink on the well when not.
pub fn paint_segment(cx: &mut Cx, mut button: ButtonRef, on: bool) {
    let p = makepad_look::palette::get(cx);
    let (fill, edge, ink) = if on { (p.accent_wash, p.accent, p.text_hi) } else { (0, 0, p.label) };
    let (fill, edge, ink) = (Vec4f::from_u32(fill), Vec4f::from_u32(edge), Vec4f::from_u32(ink));
    let hover = Vec4f::from_u32(if on { p.accent_wash } else { p.control_hover });
    script_apply_eval!(cx, button, {
        draw_bg +: {color: #(fill) color_focus: #(fill) color_hover: #(hover) color_down: #(hover) border_color: #(edge)}
        draw_text +: {color: #(ink) color_focus: #(ink) color_hover: #(ink) color_down: #(ink)}
    });
    button.redraw(cx);
}

/// The narrowest a column is drawn.
const MIN_COL: f64 = 28.0;

#[derive(Script, ScriptHook, Widget)]
pub struct LibraryView {
    #[deref]
    view: View,
    #[rust]
    rows: Vec<LibraryRow>,
    /// Display order indexes the parent rows; selection always names a key.
    #[rust]
    row_order: Vec<usize>,
    #[rust]
    sort: Option<(Column, bool)>,
    /// Newest on top: no column sort, the parent's order.
    #[rust(true)]
    newest: bool,
    /// The parent's last word on `newest`.
    #[rust]
    pushed_newest: Option<bool>,
    #[rust(ColumnLayout::explorer_default())]
    layout: ColumnLayout,
    /// The columns drawn, in order: the layout's shown ones Stage fills.
    #[rust]
    shown: Vec<Column>,
    /// The lit FILTER ticks, a bit per [`HAS`].
    #[rust]
    filter: u32,
    #[rust]
    grid_initialized: bool,
    #[rust]
    fitted_width: f64,
    /// Widths explicitly resized by the operator, per column.
    #[rust]
    sized: Vec<(Column, f64)>,
    #[rust]
    selected: Option<String>,
    #[rust]
    can_load: bool,
    #[rust]
    load_buttons_set: bool,
    /// The view pills painted once for the mode the list opened in.
    #[rust]
    segments_set: bool,
    #[rust]
    status_text: Option<String>,
    #[rust]
    analysis: LibraryAnalysis,
    #[rust]
    analysis_set: bool,
    #[rust]
    mode: LibraryMode,
    #[rust]
    set_lists: SetListsView,
    /// The bar shows `set_lists`.
    #[rust]
    set_lists_applied: bool,
    /// The parent row a drag carries.
    #[rust]
    carried: Option<usize>,
    /// The queries this catalogue ran, newest first, and where Up/Down
    /// stand among them (`None`: the text being typed, kept in `draft`).
    #[rust]
    recent: Vec<String>,
    #[rust]
    recall_at: Option<usize>,
    #[rust]
    draft: String,
}

impl LibraryView {
    pub fn set_rows(&mut self, cx: &mut Cx, rows: &[LibraryRow]) {
        if self.rows == rows {
            return;
        }
        self.rows.clear();
        self.rows.extend_from_slice(rows);
        self.reindex();
        if self.filter != 0 {
            self.sync_status(cx);
        }
        if self.selected.as_ref().is_some_and(|key| !self.rows.iter().any(|row| &row.key == key)) {
            self.selected = None;
        }
        // Even when the key stays selected, refresh or sort can move its row.
        self.sync_grid_selection(cx);
        self.sync_load_buttons(cx);
        self.view.redraw(cx);
    }

    /// The catalogue, the set list or the history. A set list or the
    /// history keeps its own order: no sort, and its rows are dragged in
    /// the set list.
    pub fn set_mode(&mut self, cx: &mut Cx, mode: LibraryMode) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        self.sync_mode(cx);
    }

    pub fn mode(&self) -> LibraryMode {
        self.mode
    }

    fn sync_mode(&mut self, cx: &mut Cx) {
        let mode = self.mode;
        let queue = mode == LibraryMode::Queue;
        let explorer = mode == LibraryMode::Explorer;
        self.view(cx, ids!(setlist_bar)).set_visible(cx, queue);
        for (id, shown) in [
            (ids!(enqueue), !queue),
            (ids!(play_next), true),
            (ids!(unqueue), queue),
            (ids!(move_up), queue),
            (ids!(move_down), queue),
            (ids!(more), explorer),
            (ids!(import), explorer),
            (ids!(search), mode != LibraryMode::Loops),
            (ids!(files), mode != LibraryMode::Loops),
            (ids!(actions_row), mode != LibraryMode::Loops),
        ] {
            self.widget(cx, id).set_visible(cx, shown);
        }
        // The loops page sits under the header: the library is just its row.
        self.view.walk.height = if mode == LibraryMode::Loops { Size::fit() } else { Size::fill() };
        for (id, tab) in VIEW_TABS {
            paint_segment(cx, self.button(cx, &[id]), tab == mode);
        }
        if !explorer {
            self.sort = None;
            self.filter = 0;
        } else {
            self.sort = if self.newest { None } else { self.sort.or(Some((Column::Title, true))) };
        }
        let grid = self.data_grid(cx, ids!(files));
        grid.set_sort_indicator(self.sort_indicator());
        self.reindex();
        self.sync_grid_selection(cx);
        self.sync_load_buttons(cx);
        self.sync_status(cx);
        self.sync_analysis(cx);
        self.view.redraw(cx);
    }

    pub fn set_set_lists(&mut self, cx: &mut Cx, lists: &SetListsView) {
        // The name follows the active list unless it is being typed.
        let name = self.text_input(cx, ids!(setlist_name));
        let active_name = lists.names.get(lists.active).map(String::as_str).unwrap_or("");
        if !cx.has_key_focus(name.area()) && name.text() != active_name {
            name.set_text(cx, active_name);
        }
        if self.set_lists == *lists && self.set_lists_applied {
            return;
        }
        let pick = self.drop_down(cx, ids!(setlist_pick));
        if self.set_lists.names != lists.names || !self.set_lists_applied {
            pick.set_labels(cx, lists.names.clone());
        }
        pick.set_selected_item(cx, lists.active);
        self.set_lists = lists.clone();
        for (id, on) in [
            (ids!(setlist_lock), lists.locked),
            (ids!(setlist_repeat), lists.repeat),
            (ids!(setlist_shuffle), lists.shuffle),
            (ids!(setlist_auto), lists.auto),
            (ids!(setlist_fill), lists.fill),
        ] {
            self.check_box(cx, id).set_active(cx, on, Animate::No);
        }
        self.widget(cx, ids!(setlist_drop)).set_disabled(cx, lists.names.len() <= 1);
        self.set_lists_applied = true;
        self.view.redraw(cx);
    }

    fn ask(&self, cx: &mut Cx, ask: SetListAsk) {
        cx.widget_action(self.widget_uid(), LibraryAction::SetList(ask));
    }

    /// The selected row's parent index.
    fn selected_index(&self) -> Option<usize> {
        let key = self.selected.as_deref()?;
        self.rows.iter().position(|row| row.key == key)
    }

    /// The selected set list row one place earlier or later.
    fn nudge(&mut self, cx: &mut Cx, later: bool) {
        let (Some(row), Some(from)) = (self.selected_row(), self.selected_index()) else { return };
        let to = match later {
            true if row + 1 < self.row_order.len() => self.row_order.get(row + 2).copied().unwrap_or(self.rows.len()),
            false if row > 0 => self.row_order[row - 1],
            _ => return,
        };
        self.ask(cx, SetListAsk::Move { from, to });
    }

    /// The catalogue's earlier queries, newest first.
    pub fn set_recent(&mut self, recent: &[String]) {
        if self.recent != recent {
            self.recent = recent.to_vec();
            self.recall_at = None;
        }
    }

    /// Up (`older`) or Down in the search box.
    fn recall(&mut self, cx: &mut Cx, older: bool) {
        let search = self.text_input(cx, ids!(search));
        if self.recall_at.is_none() {
            self.draft = search.text();
        }
        let at = crate::recent_searches::step(self.recall_at, self.recent.len(), older);
        if at == self.recall_at && at.is_some() {
            return;
        }
        self.recall_at = at;
        let text = at.and_then(|at| self.recent.get(at)).cloned().unwrap_or_else(|| self.draft.clone());
        search.set_text(cx, &text);
        search.set_cursor(cx, makepad_widgets::makepad_draw::text::selection::Cursor { index: text.len(), prefer_next_row: false }, false);
        cx.widget_action(self.widget_uid(), LibraryAction::Recall(text));
    }

    pub fn set_status(&mut self, cx: &mut Cx, status: &str) {
        if self.status_text.as_deref() != Some(status) {
            self.status_text = Some(status.into());
            self.sync_status(cx);
        }
    }

    /// The parent's status, and how many rows a lit FILTER lets through.
    fn sync_status(&mut self, cx: &mut Cx) {
        let status = self.status_text.clone().unwrap_or_default();
        let text = match self.filter {
            0 => status,
            _ => format!("{status} · {} pass FILTER", self.row_order.len()),
        };
        self.label(cx, ids!(status)).set_text(cx, &text);
    }

    pub fn set_selected(&mut self, cx: &mut Cx, key: Option<&str>) {
        let next = key.map(|key| key.to_string()).filter(|key| self.rows.iter().any(|row| &row.key == key));
        if self.selected == next {
            return;
        }
        self.selected = next;
        self.sync_grid_selection(cx);
        self.sync_load_buttons(cx);
        self.view.redraw(cx);
    }

    /// Newest on top, or sorted (A–Z by title unless a head holds the
    /// sort); the parent's remembered choice. Only a change of the parent's
    /// value counts: the list itself moves first when a head or the toggle
    /// is pressed, and the parent's value from before it heard of that is
    /// not a new choice.
    pub fn set_newest(&mut self, cx: &mut Cx, newest: bool) {
        if self.pushed_newest == Some(newest) {
            return;
        }
        self.pushed_newest = Some(newest);
        self.apply_newest(cx, newest);
    }

    fn apply_newest(&mut self, cx: &mut Cx, newest: bool) {
        if self.newest == newest {
            return;
        }
        self.newest = newest;
        if self.mode != LibraryMode::Explorer {
            return;
        }
        self.sort = if newest { None } else { self.sort.or(Some((Column::Title, true))) };
        self.reindex();
        self.sync_grid_selection(cx);
        self.view.redraw(cx);
    }

    /// The column layout from the parent's settings line.
    pub fn set_columns(&mut self, cx: &mut Cx, text: &str) {
        let layout = ColumnLayout::from_text(text);
        if layout == self.layout {
            return;
        }
        self.layout = layout;
        self.columns_changed(cx);
    }

    /// The rows the list shows, in order: what a test or the parent reads.
    pub fn shown_keys(&self) -> Vec<&str> {
        self.row_order.iter().map(|index| self.rows[*index].key.as_str()).collect()
    }

    pub fn set_can_load(&mut self, cx: &mut Cx, can_load: bool) {
        if self.can_load == can_load && self.load_buttons_set {
            return;
        }
        self.can_load = can_load;
        self.sync_load_buttons(cx);
    }

    pub fn set_analysis(&mut self, cx: &mut Cx, analysis: &LibraryAnalysis) {
        if self.analysis == *analysis && self.analysis_set {
            return;
        }
        self.analysis = analysis.clone();
        self.sync_analysis(cx);
    }

    fn sync_analysis(&mut self, cx: &mut Cx) {
        let shown = !self.analysis.action.is_empty() && self.mode != LibraryMode::Loops;
        let analyse = self.button(cx, ids!(analyse));
        analyse.set_visible(cx, shown);
        analyse.set_text(cx, &self.analysis.action);
        self.widget(cx, ids!(analyse)).set_disabled(cx, !self.analysis.enabled);
        let bake_all = self.button(cx, ids!(bake_all));
        bake_all.set_visible(cx, shown && !self.analysis.bake_all.is_empty());
        bake_all.set_text(cx, &self.analysis.bake_all);
        let stems_note = self.analysis.stems_note.as_str();
        let note = match (stems_note.is_empty(), self.analysis.status.is_empty()) {
            _ if !shown => String::new(),
            (false, false) => format!("{stems_note} · {}", self.analysis.status),
            (false, true) => stems_note.to_string(),
            (true, _) => self.analysis.status.clone(),
        };
        self.label(cx, ids!(analysis_note)).set_text(cx, &note);
        self.analysis_set = true;
        self.view.redraw(cx);
    }

    fn reindex(&mut self) {
        self.row_order = track_list::filter_sort(&self.rows, self.filter, self.sort);
    }

    /// The offered columns in the layout's order: the COLUMNS menu's rows.
    fn menu_columns(&self) -> Vec<Column> {
        self.layout.order().iter().copied().filter(|column| track_list::offered(*column)).collect()
    }

    /// The layout moved: what is drawn, the menu, the heads and the widths.
    fn columns_changed(&mut self, cx: &mut Cx) {
        self.shown = self.layout.visible().into_iter().filter(|column| track_list::offered(*column)).collect();
        if self.sort.is_some_and(|(column, _)| !self.shown.contains(&column)) {
            self.sort = None;
            self.newest = true;
            self.reindex();
        }
        self.grid_initialized = false;
        self.fitted_width = 0.0;
        self.sync_grid_selection(cx);
        self.view.redraw(cx);
    }

    fn selected_row(&self) -> Option<usize> {
        let key = self.selected.as_deref()?;
        self.row_order.iter().position(|index| self.rows[*index].key == key)
    }

    fn grid_cols(&self) -> usize {
        self.shown.len().max(1)
    }

    fn sync_grid_selection(&mut self, cx: &mut Cx) {
        let grid = self.data_grid(cx, ids!(files));
        let cols = self.grid_cols();
        grid.set_grid_size(self.row_order.len(), cols);
        let col = grid.selection().map_or(0, |selection| selection.head.1.min(cols - 1));
        let selection = self.selected_row().map(|row| GridSelection {
            kind: GridSelectKind::Rows,
            anchor: (row, col),
            head: (row, col),
        });
        if grid.selection() != selection {
            grid.set_selection(cx, selection);
        }
    }

    fn key_for(&self, row: usize) -> Option<String> {
        self.row_order.get(row).and_then(|index| self.rows.get(*index)).map(|row| row.key.clone())
    }

    fn sync_load_buttons(&mut self, cx: &mut Cx) {
        let enabled = self.can_load && self.selected.is_some();
        self.widget(cx, ids!(load_a)).set_disabled(cx, !enabled);
        self.widget(cx, ids!(load_b)).set_disabled(cx, !enabled);
        let picked = self.selected.is_some();
        for id in [ids!(enqueue), ids!(play_next), ids!(unqueue), ids!(move_up), ids!(move_down)] {
            self.widget(cx, id).set_disabled(cx, !picked);
        }
        self.load_buttons_set = true;
    }

    fn sort_indicator(&self) -> Option<(usize, bool)> {
        let (column, ascending) = self.sort?;
        Some((self.shown.iter().position(|shown| *shown == column)?, ascending))
    }

    /// The ⋯ menu: filter, columns, sort and refresh in one place.
    fn open_more_menu(&mut self, cx: &mut Cx) {
        let filter = HAS
            .iter()
            .enumerate()
            .map(|(i, has)| MenuRow::new(more_id("has", i), has.label()).checked(self.filter & (1 << i) != 0))
            .collect();
        let columns = self
            .menu_columns()
            .iter()
            .enumerate()
            .map(|(i, column)| MenuRow::new(more_id("col", i), column.label()).checked(self.layout.is_visible(*column)))
            .collect();
        let newest = self.sort.is_none();
        let rows = vec![
            MenuRow::new(more_id("filter", 0), "Filter").submenu(filter),
            MenuRow::new(more_id("columns", 0), "Columns").submenu(columns),
            MenuRow::new(more_id("sort", 0), "Sort").submenu(vec![
                MenuRow::new(more_id("newest", 0), "Newest first").radio(newest),
                MenuRow::new(more_id("az", 0), "Title A–Z").radio(!newest),
            ]),
            MenuRow::separator(),
            MenuRow::new(more_id("refresh", 0), "Refresh"),
        ];

        let anchor = self.button(cx, ids!(more)).area().rect(cx);
        cx.action(MenuAction::Open { owner: LiveId(self.widget_uid().0), rows, anchor, place: MenuPlace::BelowRight });
    }

    fn more_picked(&mut self, cx: &mut Cx, id: LiveId) {
        let uid = self.widget_uid();
        for i in 0..HAS.len() {
            if id == more_id("has", i) {
                self.filter ^= 1 << i;
                self.reindex();
                self.sync_grid_selection(cx);
                self.sync_status(cx);
                self.view.redraw(cx);
            }
        }
        for (i, column) in self.menu_columns().into_iter().enumerate() {
            if id == more_id("col", i) {
                let on = !self.layout.is_visible(column);
                self.layout.set_visible(column, on);
                self.columns_changed(cx);
                self.emit_columns(cx);
            }
        }
        if id == more_id("newest", 0) || id == more_id("az", 0) {
            let newest = id == more_id("newest", 0);
            self.apply_newest(cx, newest);
            self.data_grid(cx, ids!(files)).set_sort_indicator(self.sort_indicator());
            cx.widget_action(uid, LibraryAction::Sort(newest));
        }
        if id == more_id("refresh", 0) {
            cx.widget_action(uid, LibraryAction::Refresh);
        }

    }

    fn emit_columns(&self, cx: &mut Cx) {
        cx.widget_action(self.widget_uid(), LibraryAction::Columns(self.layout.to_text()));
    }

    /// One of the library's named verbs, from a key.
    fn run(&mut self, cx: &mut Cx, cmd: LibraryCmd) {
        let load_ok = self.can_load && self.selected.is_some();
        match cmd {
            LibraryCmd::Cursor { step, .. } => self.step_cursor(cx, step),
            LibraryCmd::LoadDeck(deck) if load_ok => {
                let deck = match deck {
                    DeckId::A => 0,
                    DeckId::B => 1,
                };
                cx.widget_action(self.widget_uid(), LibraryAction::Load(deck));
            }
            LibraryCmd::LoadTarget if load_ok => cx.widget_action(self.widget_uid(), LibraryAction::LoadIdle),
            LibraryCmd::FocusSearch => self.text_input(cx, ids!(search)).set_key_focus(cx),
            LibraryCmd::Enqueue if self.selected.is_some() && self.mode != LibraryMode::Queue => self.ask(cx, SetListAsk::Enqueue),
            LibraryCmd::PlayNext if self.selected.is_some() => self.ask(cx, SetListAsk::PlayNext),
            LibraryCmd::Unqueue if self.selected.is_some() && self.mode == LibraryMode::Queue => self.ask(cx, SetListAsk::Unqueue),
            LibraryCmd::LoadDeck(_) | LibraryCmd::LoadTarget | LibraryCmd::Enqueue | LibraryCmd::PlayNext | LibraryCmd::Unqueue => {}
        }
    }

    /// The selection is where the keys stand: one step of it.
    fn step_cursor(&mut self, cx: &mut Cx, step: CursorMove) {
        let grid = self.data_grid(cx, ids!(files));
        let page = library_nav::page_rows(grid.visible_counts().0);
        let Some(row) = library_nav::step_cursor(self.selected_row(), self.row_order.len(), page, step) else { return };
        let Some(key) = self.key_for(row) else { return };
        self.selected = Some(key.clone());
        self.sync_grid_selection(cx);
        self.sync_load_buttons(cx);
        grid.scroll_cell_into_view(cx, row, 0);
        self.view.redraw(cx);
        cx.widget_action(self.widget_uid(), LibraryAction::Select(key));
    }

    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        if let Some(text) = self.text_input(cx, ids!(search)).changed(actions) {
            self.recall_at = None;
            cx.widget_action(self.widget_uid(), LibraryAction::SearchChanged(text));
        }
        if let Some((text, _)) = self.text_input(cx, ids!(search)).returned(actions) {
            cx.widget_action(self.widget_uid(), LibraryAction::Search(text));
        }
        if self.button(cx, ids!(import)).clicked(actions) {
            cx.widget_action(self.widget_uid(), LibraryAction::Import);
        }
        for (id, tab) in VIEW_TABS {
            if self.button(cx, &[id]).clicked(actions) {
                cx.widget_action(self.widget_uid(), LibraryAction::List(tab));
            }
        }
        if self.button(cx, ids!(more)).clicked(actions) {
            self.open_more_menu(cx);
        }
        let grid = self.data_grid(cx, ids!(files));
        for action in grid.actions(actions) {
            match action {
                DataGridAction::SelectionChanged { selection: Some(selection) } => {
                    if let Some(key) = self.key_for(selection.head.0) {
                        // The public library API selects one track. Shift or
                        // drag navigation still follows the head, not a range.
                        grid.set_selection(cx, Some(GridSelection {
                            kind: GridSelectKind::Rows,
                            anchor: selection.head,
                            head: selection.head,
                        }));
                        if self.selected.as_deref() != Some(key.as_str()) {
                            self.selected = Some(key.clone());
                            self.sync_load_buttons(cx);
                        }
                        cx.widget_action(self.widget_uid(), LibraryAction::Select(key));
                    }
                }
                // A set list and the history keep their own order.
                DataGridAction::HeaderClicked { .. } if self.mode != LibraryMode::Explorer => {}
                DataGridAction::HeaderClicked { col, .. } => {
                    let Some(column) = self.shown.get(col).copied() else { continue };
                    self.sort = track_list::sort_after_head(self.sort, column);
                    self.reindex();
                    self.sync_grid_selection(cx);
                    grid.set_sort_indicator(self.sort_indicator());
                            if self.newest != self.sort.is_none() {
                        self.newest = self.sort.is_none();
                        cx.widget_action(self.widget_uid(), LibraryAction::Sort(self.newest));
                    }
                    self.view.redraw(cx);
                }
                // A right click: the track menu for that row, which it picks.
                DataGridAction::CellContextMenu { row, abs, .. } => {
                    let Some(key) = self.key_for(row) else { continue };
                    if self.selected.as_deref() != Some(key.as_str()) {
                        self.selected = Some(key.clone());
                        self.sync_grid_selection(cx);
                        self.sync_load_buttons(cx);
                        self.view.redraw(cx);
                    }
                    cx.widget_action(self.widget_uid(), LibraryAction::Select(key));
                    let rows = track_menu(self.mode, self.can_load)
                        .into_iter()
                        .flat_map(|row| {
                            let item = MenuRow::new(menu_id(row.verb), row.label).key(row.key);
                            if row.separator { vec![MenuRow::separator(), item] } else { vec![item] }
                        })
                        .collect();
                    cx.action(MenuAction::Open {
                        owner: LiveId(self.widget_uid().0),
                        rows,
                        anchor: Rect { pos: abs, size: dvec2(0.0, 0.0) },
                        place: MenuPlace::At,
                    });
                }
                // Enter's rule for a double click: the selected row onto the
                // first deck that is not playing.
                DataGridAction::CellDoubleClicked { row, .. } => {
                    let Some(key) = self.key_for(row) else { continue };
                    if self.selected.as_deref() != Some(key.as_str()) {
                        self.selected = Some(key.clone());
                        self.sync_load_buttons(cx);
                        cx.widget_action(self.widget_uid(), LibraryAction::Select(key));
                    }
                    if self.can_load {
                        cx.widget_action(self.widget_uid(), LibraryAction::LoadIdle);
                    }
                }
                DataGridAction::RowDragStarted { row, .. } => {
                    self.carried = self.row_order.get(row).copied();
                    // The carried row is the one a drop loads.
                    if let Some(key) = self.key_for(row) {
                        if self.selected.as_deref() != Some(key.as_str()) {
                            self.selected = Some(key.clone());
                            self.sync_load_buttons(cx);
                            cx.widget_action(self.widget_uid(), LibraryAction::Select(key));
                        }
                    }
                }
                DataGridAction::RowDragEnded { abs, .. } => {
                    let Some(from) = self.carried.take() else { continue };
                    match drag_end(grid.area().rect(cx).contains(abs), self.mode, self.can_load) {
                        DragEnd::Load => cx.widget_action(self.widget_uid(), LibraryAction::Dropped(abs)),
                        DragEnd::Reorder => {
                            let Some(gap) = grid.row_gap_at(abs) else { continue };
                            let to = self.row_order.get(gap).copied().unwrap_or(self.rows.len());
                            if to != from && to != from + 1 {
                                self.ask(cx, SetListAsk::Move { from, to });
                            }
                        }
                        DragEnd::Nothing => {}
                    }
                }
                DataGridAction::ColumnResized { col, width, .. } => {
                    if let Some(column) = self.shown.get(col).copied() {
                        self.sized.retain(|(held, _)| *held != column);
                        self.sized.push((column, width));
                        self.fitted_width = 0.0;
                    }
                }
                DataGridAction::ColumnOrderChanged { order } => {
                    // The grid's display order names data columns, which are
                    // the drawn columns: the layout takes the new order (its
                    // shown columns this list cannot fill keep their slots)
                    // and the grid goes back to drawing it straight.
                    let mut moved = order.iter().filter_map(|col| self.shown.get(*col).copied());
                    let visible: Vec<Column> = self
                        .layout
                        .visible()
                        .into_iter()
                        .map(|column| if track_list::offered(column) { moved.next().unwrap_or(column) } else { column })
                        .collect();
                    if self.layout.set_visible_order(&visible) {
                        self.columns_changed(cx);
                        self.emit_columns(cx);
                    }
                }
                _ => {}
            }
        }
        if let Some(id) = menu_picked(actions, LiveId(self.widget_uid().0)) {
            self.more_picked(cx, id);
            let load_ok = self.can_load && self.selected.is_some();
            match MENU_VERBS.iter().find(|(held, _)| *held == id).map(|(_, verb)| *verb) {
                Some(TrackVerb::LoadA) if load_ok => cx.widget_action(self.widget_uid(), LibraryAction::Load(0)),
                Some(TrackVerb::LoadB) if load_ok => cx.widget_action(self.widget_uid(), LibraryAction::Load(1)),
                Some(TrackVerb::Enqueue) => self.ask(cx, SetListAsk::Enqueue),
                Some(TrackVerb::PlayNext) => self.ask(cx, SetListAsk::PlayNext),
                Some(TrackVerb::Unqueue) => self.ask(cx, SetListAsk::Unqueue),
                _ => {}
            }
        }
        if let Some(index) = self.drop_down(cx, ids!(setlist_pick)).selected(actions) {
            self.ask(cx, SetListAsk::Switch(index));
        }
        if let Some((text, _)) = self.text_input(cx, ids!(setlist_name)).returned(actions) {
            self.ask(cx, SetListAsk::Rename(text));
        }
        for (id, ask) in [(ids!(setlist_new), SetListAsk::New), (ids!(setlist_drop), SetListAsk::Drop), (ids!(setlist_clear), SetListAsk::Clear), (ids!(setlist_pick_next), SetListAsk::Pick)] {
            if self.button(cx, id).clicked(actions) {
                self.ask(cx, ask);
            }
        }
        let switches: [(_, fn(bool) -> SetListAsk); 5] = [
            (ids!(setlist_fill), SetListAsk::Fill),
            (ids!(setlist_lock), SetListAsk::Lock),
            (ids!(setlist_repeat), SetListAsk::Repeat),
            (ids!(setlist_shuffle), SetListAsk::Shuffle),
            (ids!(setlist_auto), SetListAsk::Auto),
        ];
        for (id, ask) in switches {
            if let Some(on) = self.check_box(cx, id).changed(actions) {
                self.ask(cx, ask(on));
            }
        }
        let picked = self.selected.is_some();
        if picked && self.button(cx, ids!(enqueue)).clicked(actions) {
            self.ask(cx, SetListAsk::Enqueue);
        }
        if picked && self.button(cx, ids!(play_next)).clicked(actions) {
            self.ask(cx, SetListAsk::PlayNext);
        }
        if picked && self.button(cx, ids!(unqueue)).clicked(actions) {
            self.ask(cx, SetListAsk::Unqueue);
        }
        if picked && self.button(cx, ids!(move_up)).clicked(actions) {
            self.nudge(cx, false);
        }
        if picked && self.button(cx, ids!(move_down)).clicked(actions) {
            self.nudge(cx, true);
        }
        let load_ok = self.can_load && self.selected.is_some();
        if self.button(cx, ids!(load_a)).clicked(actions) && load_ok {
            cx.widget_action(self.widget_uid(), LibraryAction::Load(0));
        }
        if self.button(cx, ids!(load_b)).clicked(actions) && load_ok {
            cx.widget_action(self.widget_uid(), LibraryAction::Load(1));
        }
        if self.button(cx, ids!(analyse)).clicked(actions) && self.analysis.enabled && self.selected.is_some() {
            cx.widget_action(self.widget_uid(), LibraryAction::Bake);
        }
        if self.button(cx, ids!(bake_all)).clicked(actions) {
            cx.widget_action(self.widget_uid(), LibraryAction::BakeAll);
        }
    }
}

impl LibraryViewRef {
    pub fn set_rows(&self, cx: &mut Cx, rows: &[LibraryRow]) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_rows(cx, rows);
        }
    }

    pub fn set_status(&self, cx: &mut Cx, status: &str) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_status(cx, status);
        }
    }

    pub fn set_selected(&self, cx: &mut Cx, key: Option<&str>) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_selected(cx, key);
        }
    }

    pub fn set_can_load(&self, cx: &mut Cx, can_load: bool) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_can_load(cx, can_load);
        }
    }

    pub fn set_newest(&self, cx: &mut Cx, newest: bool) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_newest(cx, newest);
        }
    }

    pub fn set_columns(&self, cx: &mut Cx, text: &str) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_columns(cx, text);
        }
    }

    pub fn set_analysis(&self, cx: &mut Cx, analysis: &LibraryAnalysis) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_analysis(cx, analysis);
        }
    }

    pub fn set_recent(&self, recent: &[String]) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_recent(recent);
        }
    }

    pub fn set_mode(&self, cx: &mut Cx, mode: LibraryMode) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_mode(cx, mode);
        }
    }

    pub fn set_set_lists(&self, cx: &mut Cx, lists: &SetListsView) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_set_lists(cx, lists);
        }
    }
}

impl Widget for LibraryView {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let reapplied = matches!(event, Event::ScriptReapply);
        if reapplied {
            // Theme reloads replace live widget properties, not the parent's
            // library, selected key, column widths or chosen sort order.
            self.grid_initialized = false;
            self.fitted_width = 0.0;
            self.load_buttons_set = false;
            self.segments_set = false;
            self.analysis_set = false;
            self.set_lists_applied = false;
        }
        if self.shown.is_empty() && !reapplied {
            self.columns_changed(cx);
        }
        if !self.segments_set && !reapplied {
            self.segments_set = true;
            // The field asks for its text's height; the row is 22 high.
            self.text_input(cx, ids!(search)).set_height(cx, Size::Fixed(22.0));
            for (id, tab) in VIEW_TABS {
                paint_segment(cx, self.button(cx, &[id]), tab == self.mode);
            }
        }
        if !self.load_buttons_set && !reapplied {
            self.sync_load_buttons(cx);
        }
        // The list's own keys while it holds the keyboard: the library's
        // named verbs, taken before the grid sees them.
        if let Event::KeyDown(key) = event {
            let plain = !(key.modifiers.shift || key.modifiers.control || key.modifiers.alt || key.modifiers.logo);
            let search = self.text_input(cx, ids!(search));
            if plain && cx.has_key_focus(search.area()) && matches!(key.key_code, KeyCode::ArrowUp | KeyCode::ArrowDown) {
                self.recall(cx, key.key_code == KeyCode::ArrowUp);
                return;
            }
            let grid = self.data_grid(cx, ids!(files));
            if cx.has_key_focus(grid.area()) {
                if let Some(cmd) = library_nav::command_for_key(key.key_code, key.modifiers) {
                    self.run(cx, cmd);
                    return;
                }
            }
        }
        self.view.handle_event(cx, event, scope);
        if reapplied {
            self.columns_changed(cx);
            self.sync_grid_selection(cx);
            self.sync_load_buttons(cx);
            self.sync_status(cx);
            self.sync_analysis(cx);
            let lists = self.set_lists.clone();
            self.set_set_lists(cx, &lists);
            self.sync_mode(cx);
            self.view.redraw(cx);
        }
        if let Event::Actions(actions) = event {
            self.handle_actions(cx, actions);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut grid) = step.borrow_mut::<DataGrid>() {
                let cols = self.shown.len().max(1);
                grid.set_grid_size(self.row_order.len(), cols);
                if !self.grid_initialized {
                    let identity: Vec<usize> = (0..cols).collect();
                    grid.set_col_order(cx, &identity);
                    grid.set_col_labels(self.shown.iter().map(|column| column.label().to_string()).collect());
                    grid.set_header_tips(
                        self.shown
                            .iter()
                            .map(|column| format!("Sort by {}; drag a header to move it or its edge to resize", column.label().to_lowercase()))
                            .collect(),
                    );
                    self.grid_initialized = true;
                }
                grid.set_sort_indicator(self.sort_indicator());
                let width = grid.data_width();
                if width > 0.0 && (width - self.fitted_width).abs() > 0.5 {
                    let sized = &self.sized;
                    let widths = track_list::fit_widths(
                        &self.shown,
                        width,
                        &|column| sized.iter().find(|(held, _)| *held == column).map(|(_, w)| *w),
                        MIN_COL,
                    );
                    let displayed: Vec<f64> = grid.col_order().iter().map(|col| widths.get(*col).copied().unwrap_or(MIN_COL)).collect();
                    grid.set_col_widths(cx, &displayed);
                    self.fitted_width = width;
                }
                let palette = makepad_look::palette::get(cx);
                while let Some(cell) = grid.next_cell(cx) {
                    let Some(row) = self.row_order.get(cell.row).and_then(|index| self.rows.get(*index)) else {
                        continue;
                    };
                    let Some(column) = self.shown.get(cell.col).copied() else { continue };
                    let selected = self.selected.as_deref() == Some(row.key.as_str());
                    let text = track_list::cell_text(column, row);
                    let color = if selected {
                        palette.text_hi
                    } else {
                        match column {
                            Column::Title => palette.label,
                            Column::Bpm => palette.accent,
                            Column::Stem | Column::Krk => palette.live,
                            _ => palette.secondary,
                        }
                    };
                    grid.cell_text_styled(cx, &cell, &text, CellStyle {
                        bg: selected.then(|| Vec4f::from_u32(palette.row_picked)),
                        color: Some(Vec4f::from_u32(color)),
                        ..Default::default()
                    });
                }
                if let Some(row) = self.selected_row() {
                    grid.outline_row(row, Vec4f::from_u32(palette.cursor));
                }
            }
        }
        DrawStep::done()
    }
}

#[cfg(test)]
mod drag_tests {
    use super::*;

    #[test]
    fn a_row_carried_off_the_list_loads_and_one_let_go_inside_only_reorders_the_set_list() {
        for mode in [LibraryMode::Explorer, LibraryMode::Queue, LibraryMode::History] {
            assert_eq!(drag_end(false, mode, true), DragEnd::Load, "{mode:?} off the list");
            assert_eq!(drag_end(false, mode, false), DragEnd::Nothing, "{mode:?} with nowhere to load");
        }
        assert_eq!(drag_end(true, LibraryMode::Queue, true), DragEnd::Reorder);
        assert_eq!(drag_end(true, LibraryMode::Explorer, true), DragEnd::Nothing);
        assert_eq!(drag_end(true, LibraryMode::History, true), DragEnd::Nothing);
    }
}
