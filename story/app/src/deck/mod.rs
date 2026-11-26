//! DECK opens the head of a document tree. File edits and slide operations
//! share the studio worker, the player follows its immutable working snapshot,
//! and Save publishes all changed files behind one guarded tree manifest.
//! The file list and Notes/Structure inspector use the shared Structure widget.


mod apps;
mod camera;
mod code;
mod demo;
mod frames;
mod player;
mod playbar;
mod present;
mod presenter;
mod render;
mod sorter;
mod thumbnails;
pub(crate) mod perf;
mod shapes;
pub(crate) mod view;
mod files;
mod preview;
mod selection;
mod sketch;
mod slides;
mod duration;
mod i18n;
mod links;
mod local_drop;
pub(crate) mod output_lane;

use makepad_live_frame::{FrameBinding, FrameSource};
use super::cut::motion_edit::checker::{Check, Checker, DocKind, Problem};
use super::cut::motion_edit::source_pane::SourcePane;
use perf::Perf as DeckPerf;
use super::cut::browser::{LibraryBrowser, PageLane};
use makepad_library_browser::list::{CutMediaList, ListRow, MediaListAction};
use super::cut::controls::{CutSegmented, RenderButtonAction, SegmentAction};
use super::cut::folder_docs::{self, Card, Loaded, OpenFolder, OpenJob};
use super::cut::open_doc::Session;
use super::cut::store::{OpenTree, StoreJob, StoreReply, TreeSave};
use makepad_library_browser::lanes::{LibLane, Thumb};
use makepad_asset_data::tree::{TreeDraft, TreeRole};
use makepad_asset_data::{AssetId, AssetKind, AssetRevisionRef};
use makepad_story_deck::DeckDoc;
use makepad_widgets::*;
use makepad_widgets::tip::TipWidgetRefExt;
use makepad_story_deck::ops::{CameraSet, ImageFit, NewCut, DeckOp, SlideSet};
use makepad_story_deck::Transition;
use makepad_story_cut::{CameraDevice, CameraSpec, CutNodeKind};
use makepad_story_deck::SlideCut;
use makepad_widgets::makepad_platform::video::{VideoFormat, VideoInputDesc};
use player::Command;
use presenter::PresenterClock;
use sorter::{DeckSorter, SorterAction};
pub(super) use present::{PresentAsk, PresentMode, PresentOutput};
use std::rc::Rc;
use std::sync::Arc;
use view::{DeckAction, DeckView};
use slides::Nav;

/// A poster the player has not drawn yet (the first slide is still loading):
/// a plain card.
const PLAIN_POSTER: (usize, usize) = (448, 256);

/// The editor and `list_apps` offer the same entries, in registry menu order.
pub(crate) fn app_choices() -> impl Iterator<Item = &'static makepad_app_registry::AppEntry> {
    makepad_app_registry::apps().iter().filter(|entry| entry.menu_visible)
}

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    let Heading = Label{width: Fit text: "" draw_text +: {color: stage.secondary text_style: theme.font_bold{font_size: 7.5}}}
    let Body = Label{width: Fill height: Fit text: "" draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 9}}}
    let Tool = StageButton{width: Fit height: 24 margin: 0 padding: Inset{left: 10 right: 10}
        draw_text +: {text_style: theme.font_regular{font_size: 9}}}
    let Pane = RoundedView{width: Fill height: Fill flow: Down spacing: 0 padding: 0 show_bg: true
        draw_bg +: {color: stage.panel border_color: stage.separator border_size: 1 border_radius: 5}}
    let Column = RoundedView{height: Fill flow: Down spacing: 6 padding: 10 show_bg: true
        draw_bg +: {color: stage.panel border_color: stage.separator border_size: 1 border_radius: 5}}

    mod.widgets.StageDeckDesk = set_type_default() do #(DeckDesk::register_widget(vm)){width: Fill height: Fill flow: Down spacing: 8
      // The bar, the slide column, the side column and the code pane keep
      // their own draw lists: an animating slide redraws the picture, not
      // every widget around it (each is still repainted whole).
      deck_bar := mod.widgets.StageDocumentBar{new_batch: true
        ScrollXView{width: Fill height: Fill flow: Right spacing: 8 align: Align{y: 0.5}
            deck_title := mod.widgets.StageDocumentName{}
            // The talk's length as plain text; a click edits it.
            Tip{text: "The talk's length, what the clock counts down from: click to set it (m:ss or minutes; empty: the sum of the slides' speaking times). It is kept in the deck"
                deck_duration := Button{width: Fit height: 22 margin: 0 padding: Inset{left: 4 right: 4} text: "0 slides · 0:00"
                    draw_bg +: {color: #0000 color_hover: #ffffff0c color_down: #0000 color_focus: #0000 border_size: 0.0}
                    draw_text +: {color: stage.muted color_hover: stage.text color_focus: stage.muted color_down: stage.text text_style: theme.font_regular{font_size: 8.5}}}}
            deck_duration_editor := View{visible: false width: Fit height: Fit flow: Right spacing: 6 align: Align{y: 0.5}
                deck_duration_input := TextInput{width: 100 height: 24 empty_text: "slide times"}
                deck_duration_error := Body{width: Fit flow: Right text: "" draw_text +: {color: stage.warm}}
            }
            deck_bar_actions := View{width: Fit height: Fit flow: Right spacing: 8 align: Align{y: 0.5}
                Tip{text: "Add Slide: a new slide after the current one (also: right-click in the slide list)"
                    deck_add_slide := mod.widgets.StageDocumentIconTool{draw_icon +: {svg: crate_resource("self:resources/icons/plus.svg")}}}
                Tip{text: "Present from the current slide (⌘Enter)"
                    deck_present := CutRenderButton{width: 150 idle_text: "Present · ⌘Enter" icon_render +: {svg: crate_resource("self:resources/icons/play.svg")}}}
                View{width: Fit height: Fit flow: Right spacing: 2 align: Align{y: 0.5}
                    Tip{text: "Present from slide 1"
                        deck_present_first := mod.widgets.StageDocumentIconTool{draw_icon +: {svg: crate_resource("self:resources/icons/edit/mark_in.svg")}}}
                    Tip{text: "Save the deck to the library as a new revision (⌘S). Tinted while it has unsaved changes"
                        deck_save := mod.widgets.StagePanelToggle{draw_icon +: {svg: crate_resource("self:resources/icons/ui/save.svg")}}}
                    Tip{text: "Fullscreen (⇧F)"
                        deck_fullscreen := mod.widgets.StageDocumentIconTool{draw_icon +: {svg: crate_resource("self:resources/icons/fit.svg")}}}
                }
                Tip{text: "Language of the OUTPUT (projector) window only. This screen always shows the deck as written; the output window shows each text in the chosen language where the slide's source gives it (tr({en: … zh: …})), else as written"
                    deck_output_lang := DropDown{width: 112 height: 22 margin: 0 labels: ["English" "中文"]
                        draw_text +: {text_style: theme.font_regular{font_size: 8.5}}}}
            }
            // The deck's status never writes into this bar: a fixed slot (it
            // never changes width) holds the spinner while something is busy,
            // and a red alert, with the full text in its tooltip, when there
            // is an error and the code editor's bar (which says it) is not shown.
            deck_bar_status := View{width: 22 height: 24 flow: Overlay align: Align{x: 0.5 y: 0.5}
                deck_bar_spinner := mod.widgets.LoadingSpinner{visible: false width: 11 height: 11 draw_bg +: {stroke_width: 1.6 color: stage.accent}}
                deck_bar_alert_tip := Tip{text: "" wrap_width: 420
                    deck_bar_alert := StageIconButton{margin: 0 visible: false draw_icon +: {color: stage.danger svg: crate_resource("self:resources/icons/alert.svg")}}}
            }
        // The panels, one tight group.
        deck_panel_toggles := RoundedView{width: Fit height: Fit flow: Right spacing: 1 padding: 1 align: Align{y: 0.5} show_bg: true
            draw_bg +: {color: stage.border border_radius: 3}
            Tip{text: "Show or hide the library (the left column's Library tab)"
                deck_toggle_library := mod.widgets.StagePanelToggle{draw_icon +: {svg: crate_resource("self:resources/icons/edit/grid.svg")}}}
            Tip{text: "Show or hide this deck's slides and files (the left column)"
                deck_toggle_sorter := mod.widgets.StagePanelToggle{draw_icon +: {svg: crate_resource("self:resources/icons/list.svg")}}}
            Tip{text: "Show or hide the stage picture"
                deck_toggle_view := mod.widgets.StagePanelToggle{draw_icon +: {svg: crate_resource("self:resources/icons/edit/eye.svg")}}}
            Tip{text: "Show or hide the presenter (next slide, notes and demo)"
                deck_toggle_side := mod.widgets.StagePanelToggle{draw_icon +: {svg: crate_resource("self:resources/icons/text.svg")}}}
            Tip{text: "Show or hide chat"
                deck_toggle_chat := mod.widgets.StagePanelToggle{draw_icon +: {svg: crate_resource("self:resources/icons/chat.svg")}}}
            Tip{text: "Show or hide the deck's Splash code"
                deck_toggle_code := mod.widgets.StagePanelToggle{draw_icon +: {svg: crate_resource("self:resources/icons/json.svg")}}}
        }
            Tip{text: "Current local time"
                deck_clock := Label{width: Fit height: Fit padding: 0 flow: Right text: "--:--" draw_text +: {color: stage.secondary text_style: theme.font_code{font_size: 11}}}}
        }
        // These are measured without wrapping before the Fill scroller gets
        // the remaining width. Library tools and panel toggles give way first.
        // One line: two stacked lines did not fit the bar's height.
        // The time left in the talk, large; the slide's small beside it.
        // The reset button (or Shift+R) starts the clock again at 0.
        View{width: Fit height: Fit flow: Right spacing: 6 align: Align{y: 0.5}
            Tip{text: "Time left in the talk (past its length: the overrun). Shift+R or the button restarts the clock at 0; T shows it in a corner while presenting"
                View{width: Fit height: Fit flow: Right spacing: 6 align: Align{y: 0.5}
                    deck_total_time := mod.widgets.StageDocumentMeta{width: Fit height: Fit padding: 0 flow: Right text: "0:00 left" draw_text +: {color: stage.text_hi text_style: theme.font_code{font_size: 12}}}
                    deck_slide_time := mod.widgets.StageDocumentMeta{width: Fit height: Fit padding: 0 flow: Right text: "slide 0:00" draw_text +: {color: stage.muted text_style: theme.font_code{font_size: 8.5}}}
                    deck_return := mod.widgets.StageDocumentMeta{visible: false width: 190 height: Fit padding: 0 flow: Right max_lines: 1 text_overflow: Ellipsis text: "" draw_text +: {color: stage.accent text_style: theme.font_code{font_size: 8.5}}}
                }}
            Tip{text: "Restart the talk's clock at 0 (Shift+R)"
                deck_clock_reset := mod.widgets.StageDocumentIconTool{draw_icon +: {svg: crate_resource("self:resources/icons/ui/resync.svg")}}}
        }
      }
      deck_nav_split := mod.widgets.DockSplitter{width: Fill height: Fill axis: SplitterAxis.Horizontal align: SplitterAlign.FromA(320) size: 8
        fold: SplitterCollapse.A min_vertical: 140 max_vertical: 240
        // Navigation is a sibling of the entire preview/source split, so it
        // reaches the bottom edge even when the code editor fills the desk.
        // One full-height column: the library to open decks and take media
        // from, this deck's slides, and its files.
        a: View{width: Fill height: Fill deck_strip := Column{width: Fill new_batch: true
            Tip{text: "Library: the decks to open. Slides: this deck's slides. Files: its files"
                deck_nav_tabs := CutSegmented{width: Fill labels: ["Library" "Slides" "Files"]}}
            deck_library := View{visible: false width: Fill height: Fill flow: Down spacing: 4
                View{width: Fill height: Fit flow: Right spacing: 6 align: Align{y: 0.5}
                    Tip{text: "A new deck from the template: one title slide" deck_new := Tool{text: "+ New deck"}}
                    mod.widgets.StageDeckSearch{}
                }
                lib_list := mod.widgets.CutMediaList{}
                lib_status := Label{width: Fill height: Fit padding: Inset{top: 3 bottom: 3} max_lines: 1 text: "" draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 8}}}
                Tip{text: "Drop an image or video file from your computer onto the slide list to add it as a slide after the current one, or onto the picture to add it as a layer of the current slide"
                    Heading{text: "DROP AN IMAGE OR VIDEO ONTO THE SLIDE · hover for help"}}
            }
            // The slides start right under the tabs; "+" between two slides
            // (on hover) and the tile after the last add one.
            deck_slides := View{width: Fill height: Fill flow: Down
                Tip{text: "Click a slide to show it; drag to reorder; right-click to duplicate, move or delete. Hover between two slides for a + that adds one there, or use the + after the last"
                    deck_sorter := DeckSorter{height: Fill}}
            }
            deck_file_list := ScrollYView{visible: false width: Fill height: Fill flow: Down
                deck_files := mod.widgets.StructureTree{context_menu: true}}
        }}
      b: View{width: Fill height: Fill
      deck_code_split := mod.widgets.DockSplitter{width: Fill height: Fill axis: SplitterAxis.Vertical align: SplitterAlign.FromB(300) size: 8
        fold: SplitterCollapse.B

      a: View{width: Fill height: Fill
      View{width: Fill height: Fill flow: Right spacing: 8
        deck_centre := View{width: Fill height: Fill flow: Down spacing: 6
            deck_view := DeckView{}
        }
        deck_side := Column{width: 296 new_batch: true
            deck_side_tabs := CutSegmented{width: Fill labels: ["Notes" "Structure"]}
            deck_structure := ScrollYView{visible: false width: Fill height: Fill flow: Down
                deck_structure_tree := mod.widgets.StructureTree{}}
            deck_presenter := View{width: Fill height: Fill flow: Down spacing: 6
            deck_slide_settings := View{width: Fill height: Fit flow: Down spacing: 6
                Heading{text: "THIS SLIDE"}
                View{width: Fill height: Fit flow: Right spacing: 6 align: Align{y: 0.5}
                    Tip{text: "The transition into this slide, and its length" Label{width: 44 text: "Enter" draw_text +: {color: stage.dim text_style: theme.font_regular{font_size: 8}}}}
                    deck_trans := DropDown{width: Fill height: 22 margin: 0 labels: ["Deck default"]
                        draw_text +: {text_style: theme.font_regular{font_size: 9}}}
                    deck_trans_len := DropDown{width: 70 height: 22 margin: 0 labels: ["0.3 s" "0.6 s" "1.0 s" "1.5 s" "2.0 s"]
                        draw_text +: {text_style: theme.font_regular{font_size: 9}}}
                }
                View{width: Fill height: Fit flow: Right spacing: 6 align: Align{y: 0.5}
                    Tip{text: "The speaking time for this slide (m:ss or minutes; empty: untimed), what the presenter clock compares against"
                        Label{width: 44 text: "Time" draw_text +: {color: stage.dim text_style: theme.font_regular{font_size: 8}}}}
                    deck_slide_target := TextInput{width: 90 height: 22 empty_text: "untimed"}
                    deck_slide_target_note := Body{width: Fill text: "" draw_text +: {color: stage.warm}}
                }
                deck_camera := View{visible: false width: Fill height: Fit flow: Down spacing: 6
                    Heading{text: "CAMERA"}
                    deck_cam_device := DropDown{width: Fill height: 22 margin: 0 labels: ["Looking for cameras"]
                        draw_text +: {text_style: theme.font_regular{font_size: 9}}}
                    deck_cam_format := DropDown{width: Fill height: 22 margin: 0 labels: ["-"]
                        draw_text +: {text_style: theme.font_regular{font_size: 9}}}
                    View{width: Fill height: Fit flow: Right spacing: 4
                        deck_cam_rotate := Tool{width: Fill text: "Rotate 90"}
                        deck_cam_mirror := Tool{width: Fill text: "Mirror"}
                    }
                    deck_cam_note := Body{}
                }
                deck_fx_picker := EffectPicker{width: 0 height: 0 visible: false managed: true category: "transition" target: "edit.effect"}
            }
            Heading{text: "NEXT"}
            deck_next_preview := View{visible: false width: Fill height: 138 flow: Overlay
                deck_next_app := View{visible: false width: Fill height: Fill flow: Down spacing: 10 align: Align{x: 0.5 y: 0.5}
                    Icon{icon_walk: Walk{width: 30 height: 30} draw_icon +: {svg: crate_resource("self:resources/icons/edit/app_run.svg") color: stage.dim}}
                    name := Label{text: "" draw_text +: {color: stage.text}}
                }
                deck_next_picture := Image{visible: false width: Fill height: Fill fit: ImageFit.Smallest placeholder_width: 240 placeholder_height: 135}
                deck_next_pending := View{visible: false width: Fill height: Fill align: Align{x: 0.5 y: 0.5}
                    Body{width: Fit text: "Preparing preview…"}}
            }
            deck_next := Body{}
            deck_beats := Body{}
            Heading{text: "NOTES"}
            deck_notes_scroll := ScrollYView{width: Fill height: 110 flow: Down
                deck_notes := Body{}
            }
            Tip{text: "Right / PageDown or picture click: release beat, then next slide; Left / PageUp: previous beat or slide; Home / End: first / last; B: black; R: restart slide; D: start, pause or resume demo; on a slide with a demo, Right performs its next step at once (then the next slide from its end), Left goes back a step (then the previous slide from its start), Space plays or pauses it, and the play bar's dots seek; Shift+D: restart demo; Space: next beat or slide on a slide without a demo; click an app to interact: its keys (arrows too) go to it until Esc or a click outside it; Esc: release app focus; F: own window; Shift+F: fullscreen; Esc: leave fullscreen, then own window."
                Heading{text: "KEYS · hover for help"}
            }
            }
        }
      }
      }
      // The whole deck as source, as CUT's Source view is: checked as typed,
      // a draft that loads applies as one undo step.
      b: Pane{new_batch: true
        View{width: Fill height: Fill flow: Right spacing: 4
            deck_source := mod.widgets.DocSourcePane{}
            deck_binary := Body{visible: false}
        }
      }
      }
      }
      }
      deck_file_prompt := Popover{width: 0 height: 0 abs_pos: vec2(0.0, 0.0) trigger: Manual trap_focus: true
        draw_panel +: {color: stage.panel border_color: stage.separator}
        content := View{width: 300 height: Fit flow: Down spacing: 10
            title := Body{}
            name := TextInput{width: Fill height: 28}
            note := Body{}
            View{width: Fill height: Fit flow: Right{wrap: true} spacing: 6 align: Align{x: 1.0}
                cancel := Tool{text: "Cancel"}
                accept := Tool{text: "Rename"}
            }
        }
      }
    }
}

pub fn register(vm: &mut ScriptVm) {
    shapes::script_mod(vm);
    view::script_mod(vm);
    output_lane::script_mod(vm);
    sorter::script_mod(vm);
    script_mod(vm);
}

/// Hidden picture passes serve active presentations and explicit renders;
/// visible thumbnail cards keep their finite still-cache work independent.
#[derive(Script, ScriptHook, Widget)]
pub struct DeckDesk {
    #[deref]
    view: View,
    #[rust]
    perf: DeckPerf,
}

impl DeckDesk {
    fn picture_hidden(&self, cx: &Cx) -> bool {
        !self.view.widget(cx, ids!(deck_centre)).visible()
            || self.view.splitter(cx, ids!(deck_code_split)).collapse() == SplitterCollapse::A
    }
}

impl Widget for DeckDesk {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.perf.event(event);
        let hidden_frame = self.picture_hidden(cx) && self.view.widget(cx, ids!(deck_view))
            .borrow::<DeckView>().is_some_and(|view| view.frame_event(event));
        self.view.handle_event(cx, event, scope);
        if hidden_frame {
            // The pane's former draw area can be absent after a fold. The
            // desk owns redraws while it records the player's passes.
            self.view.redraw(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let started = self.perf.begin();
        self.view.draw_walk(cx, scope, walk)?;
        if self.picture_hidden(cx) {
            if let Some(mut view) = self.view.widget(cx, ids!(deck_view)).borrow_mut::<DeckView>() {
                view.draw_hidden(cx);
            }
        }
        let picture = self.view.widget(cx, ids!(deck_view));
        let idle = picture.borrow::<DeckView>().is_some_and(|view| view.idle());
        let size = picture.area().rect(cx).size;
        self.perf.end(cx, started, size, idle);
        DrawStep::done()
    }
}

/// The code view's edit on its way into the tree ([`DeckPage::live_write`]).
pub(super) struct LiveWrite {
    path: String,
    text: String,
    /// Taken by the page's edit queue (its outcome is this edit's).
    running: bool,
    /// The outcome: landed, or why it did not load.
    result: Option<Result<(), String>>,
}

#[derive(Default)]
pub(super) struct DeckPage {
    /// The way back from links followed (the last is the next Backspace).
    trail: Vec<links::Return>,
    /// The tree's decks, loaded ahead of a link or a pick in the switcher.
    deck_loads: links::DeckLoads,
    /// A switch waiting for its deck to load.
    pending_switch: Option<links::PendingSwitch>,
    /// The deck whose linked decks were last asked for ahead (path, head).
    switch_seen: Option<(String, Option<AssetRevisionRef>)>,
    opened: bool,
    doc: Option<Rc<DeckDoc>>,
    /// The open deck: its source, document and undo history. The
    /// AI's source tools and slide ops change it through the same session
    /// type EDIT's edit lives in.
    session: Option<Session<DeckDoc>>,
    tree_alias: Option<makepad_asset_data::AssetAlias>,
    save_requested: bool,
    pending: std::collections::VecDeque<super::document_tree::Operation>,
    editing: bool,
    active_file: String,
    file_sources: std::collections::BTreeMap<String, SourcePane>,
    files_seen: Option<(u64, String)>,
    folded: std::collections::HashSet<String>,
    side_structure: bool,
    /// The left column's tab: Library, Slides or Files.
    nav: Nav,
    /// The slide a right-click menu is open for.
    slide_menu: Option<usize>,
    duration_edit: Option<u64>,
    duration_focus: bool,
    structure_seen: Option<u64>,
    element: Option<selection::Element>,
    file_slide: Option<usize>,
    wanted_file: Option<String>,
    file_menu: Option<(String, Rect)>,
    file_prompt: Option<files::FilePrompt>,
    /// A change was installed in the session: the player takes the new
    /// document on the next pump, keeping the slide and its clock.
    reload: bool,
    /// The library browser, the left column's Library tab (made at the
    /// first pump, which has a `Cx`).
    lib: LibraryBrowser,
    /// The library explorer needs painting.
    lib_dirty: bool,
    /// A deck another page asked for, opened once the library is up.
    wanted: Option<(AssetRevisionRef, String)>,
    /// A deck asked for while the open one had changes: it opens once the
    /// open deck's autosave has landed (switching always works).
    open_after_save: Option<(AssetRevisionRef, String)>,
    /// A new deck asked for in the same situation.
    new_after_save: Option<String>,
    /// The revision the autosave last saw, and since when it has not moved.
    autosave_seen: Option<(u64, std::time::Instant)>,
    /// The library deck open now (its card is the selected one).
    open_asset: Option<AssetId>,
    /// The folder deck open in place (its watcher), and one being read.
    folder: Option<OpenFolder>,
    folder_open: Option<OpenJob>,
    /// The folder decks' card pictures.
    card_pictures: folder_docs::CardPictures,
    /// A folder card asked for while the open deck had changes: it opens
    /// once the autosave has landed.
    card_after_save: Option<Card>,
    /// The save under way is the autosave (it never writes over a file
    /// that changed on disk too), and the session revision whose folder
    /// save it was refused for (it does not try again until an edit).
    autosaving: bool,
    folder_refused: Option<u64>,
    /// A new deck waits for the library to publish it.
    publish_new: bool,
    /// The slide to select once the player has taken the new document.
    select: Option<usize>,
    /// The transition presets the picker offers (the dropdown's entries after
    /// "Deck default"), as the catalogue lists them.
    trans_presets: Vec<String>,
    /// Present mode: how it runs, and what the app is asked to do about the
    /// output window.
    presenting: Option<PresentMode>,
    present_ask: Option<PresentAsk>,
    /// The library revision an open job is fetching.
    opening: Option<AssetRevisionRef>,
    /// The open deck shows its first slide while the store (or the folder
    /// reader) loads the other slides' graphics (its head, none for a folder
    /// deck, and session revision); edits and saves wait for the whole deck.
    completing: Option<(Option<AssetRevisionRef>, u64)>,
    /// A save under way.
    saving: Saving,
    own_window: bool,
    /// The docks' folds before own-window mode, to come back to.
    saved_docks: Vec<SplitterCollapse>,
    /// Temporary deck visibility, separate from the saved layout.
    saved_columns: Option<[bool; 3]>,
    /// The six panel buttons and column widths only change with the layout.
    panels_seen: Option<[bool; 6]>,
    clock: PresenterClock,
    /// Recompute document metadata only when opening, editing or saving.
    header_seen: Option<(u64, u64, Option<AssetRevisionRef>)>,
    /// Until the OS acknowledges a fullscreen request, rapid toggles use
    /// the requested state rather than the previous window geometry.
    fullscreen_request: Option<(WindowId, bool)>,
    poster_revision: u64,
    beat_seen: Option<(usize, usize, usize)>,
    /// The camera list the Camera section shows (revision, cameras), and
    /// the formats of the picked camera that its format dropdown lists.
    camera_seen: u64,
    camera_list: Vec<VideoInputDesc>,
    camera_formats: Vec<VideoFormat>,
    load_error: String,
    /// The slide the labels and the strip show.
    synced: Option<usize>,
    stopped: bool,
    /// The whole deck as source, at the bottom: the shared source pane
    /// (live: every edit applies as typed), the checker that finds an edit's
    /// lines when it does not load, and what the pane was last given.
    source: Option<SourcePane>,
    checker: Option<Checker>,
    serial: u64,
    /// The code view's edit queued or running: one at a time, the newest
    /// text next.
    live_write: Option<LiveWrite>,
    /// A failed edit's text sent to the checker for its lines: (serial, path).
    diagnosing: Option<(u64, String)>,
    /// Who made the change the pane is about to be shown.
    author: Option<String>,
    source_seen: Option<u64>,
    /// Bumped when another deck is opened: the pane starts over.
    opened_serial: u64,
    revealed: Option<(usize, u64)>,
    /// The output window's language.
    i18n: i18n::I18n,

    /// The deck's status as last routed (line, tooltip, busy, error), whether
    /// the editor's bar was there to say it, and the timer that turns the
    /// top bar's spinner while busy.
    bar_status: Option<(String, String, bool, bool)>,
    bar_editor: bool,
    bar_timer: Timer,
    /// The slide the bar's clock last read (it refreshes on a new slide).
    clock_slide: Option<usize>,
    /// A one-off answer to an action ("Already saved"), shown until the
    /// next edit.
    bar_notice: String,
    /// The time left shows in a corner of the picture while presenting (T).
    corner_clock: bool,
    /// The app menu of "New app slide…": after which slide (None: after the
    /// last), and where it opens.
    add_after: Option<Option<usize>>,
    add_anchor: Rect,
    /// Sketch slides: the one being made, "Fancy it up" under way.
    sketch: sketch::Sketches,
}

/// The static card a (cloned) card is.
fn card_ref(card: &Card) -> &'static Card {
    folder_docs::deck_cards().iter().find(|other| std::ptr::eq(other.doc, card.doc) && other.path == card.path).expect("a folder deck card")
}

/// The card of the deck the page opens first: the Library's first (the
/// first of a content root's `library.json`).
fn first_card() -> Option<Card> {
    folder_docs::deck_cards().first().cloned()
}

/// The longest status line the editor's bar shows; the rest is in the tooltip.
const BAR_STATUS_CHARS: usize = 80;

/// What a status line says about the deck: still working, or gone wrong.
/// How long an edit stands still before the deck saves itself.
const AUTOSAVE_AFTER: std::time::Duration = std::time::Duration::from_millis(1500);

fn status_kind(line: &str) -> (bool, bool) {
    let lower = line.to_lowercase();
    let error = ["failed", "error", "ended:", "not available", "not played", "cannot"].iter().any(|word| lower.contains(word));
    let busy = !error && (lower.contains('…') || ["loading", "starting", "compiling", "building", "waiting", "looking for", "seeking", "translating", "checking", "preparing", "applying"].iter().any(|word| lower.contains(word)));
    (busy, error)
}

/// The compact status from every status line: the first busy or failing
/// line (shortened), the rest behind the tooltip. The first `failures` lines
/// being the page's error line,
/// where anything that is not plainly work in progress is an error ("Could
/// not open ...", "Not saved: ...", a file operation's refusal).
fn bar_status(lines: &[String], failures: usize) -> Option<(String, String, bool, bool)> {
    let mut kinds: Vec<(bool, bool)> = Vec::new();
    let mut texts: Vec<&str> = Vec::new();
    for (at, text) in lines.iter().enumerate() {
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
            let (busy, error) = status_kind(line);
            kinds.push((busy, error || (at < failures && !busy)));
            texts.push(line);
        }
    }
    let lines = texts;
    if lines.is_empty() { return None; }
    let lead = kinds.iter().position(|(_, error)| *error).or_else(|| kinds.iter().position(|(busy, _)| *busy)).unwrap_or(0);
    let busy = kinds.iter().any(|(busy, _)| *busy);
    let error = kinds.iter().any(|(_, error)| *error);
    let mut line: String = lines[lead].chars().take(BAR_STATUS_CHARS).collect();
    if lines[lead].chars().count() > BAR_STATUS_CHARS { line.push('…'); }
    if lines.len() > 1 { line.push_str(&format!("  +{}", lines.len() - 1)); }
    Some((line, lines.join("\n"), busy, error))
}

/// Where the deck's status goes.
#[derive(Debug, PartialEq)]
struct StatusRoute {
    /// The editor bar's line, tooltip and error flag (None: no status, or no
    /// editor bar to say it).
    editor: Option<(String, String, bool)>,
    /// The top bar's alert tooltip: an error with no editor bar to show it.
    alert: Option<String>,
    /// The top bar's spinner (fixed size).
    busy: bool,
}

/// The one rule for status text: it is the editor bar's, never the top
/// bar's. Without the editor bar an error is an alert in the top bar's
/// fixed slot, its full text in the tooltip.
fn route_status(status: Option<&(String, String, bool, bool)>, editor_visible: bool) -> StatusRoute {
    let Some((line, tip, busy, error)) = status else { return StatusRoute { editor: None, alert: None, busy: false } };
    StatusRoute {
        editor: editor_visible.then(|| (line.clone(), tip.clone(), *error)),
        alert: (*error && !editor_visible).then(|| tip.clone()),
        busy: *busy,
    }
}

/// The picture the player shows now, and the shown slide (index, name).
pub(crate) fn picture(cx: &mut Cx, ui: &WidgetRef) -> Option<Texture> {
    ui.widget(cx, ids!(deck_view)).borrow_mut::<DeckView>().and_then(|mut view| view.shown_texture())
}

pub(crate) fn shown_slide(cx: &mut Cx, ui: &WidgetRef) -> Option<usize> {
    ui.widget(cx, ids!(deck_view)).borrow::<DeckView>().and_then(|view| view.current())
}

/// Where a save is: the poster being read back, then the publish.
#[derive(Default)]
enum Saving {
    #[default]
    Idle,
    Publishing { tree: Arc<TreeDraft> },
}

impl DeckPage {
    /// The folder decks the library lists first (matching the search).
    fn folder_cards(&self) -> Vec<Card> {
        folder_docs::deck_cards().iter().filter(|card| card.matches(&self.lib.search)).cloned().collect()
    }

    /// The folder of the deck open in place.
    pub(crate) fn folder_dir(&self) -> Option<String> {
        self.folder.as_ref().map(|folder| folder.doc.dir.display().to_string())
    }

    pub(crate) fn session(&self) -> Option<&Session<DeckDoc>> {
        self.session.as_ref()
    }

    pub(crate) fn session_mut(&mut self) -> Option<&mut Session<DeckDoc>> {
        self.session.as_mut()
    }

    pub(crate) fn history_busy(&self) -> bool { !matches!(self.saving, Saving::Idle) }

    pub(crate) fn has_file_draft(&self, path: &str) -> bool {
        if path == self.active_file { self.source.as_ref().is_some_and(SourcePane::has_draft) } else { self.file_sources.get(path).is_some_and(SourcePane::has_draft) }
    }

    pub(crate) fn history_head(&self) -> Option<(AssetRevisionRef, makepad_asset_data::AssetAlias)> {
        Some((self.session.as_ref()?.history_head?, self.tree_alias.clone()?))
    }

    pub(crate) fn restore_previous(&mut self, previous: OpenTree) -> Result<(), String> {
        let session = self.session.as_mut().ok_or("no deck open")?;
        session.restore_previous(previous.draft, previous.path, previous.doc.into_deck()?, previous.head)?;
        self.document_changed("Undo tree revision".into());
        Ok(())
    }

    /// The session changed (a tool installed a change, or undid one): show
    /// it on the next pump.
    pub(crate) fn document_changed(&mut self, status: String) {
        self.reload = true;
        self.bar_notice.clear();
        self.author = Some(status);
        // The page reloads on its pump, which ran before this landed: run
        // the next one now instead of on the next 30 Hz tick.
        makepad_widgets::makepad_platform::thread::SignalToUI::set_ui_signal();
    }

    /// The first time the page is shown the main talk opens, in place from its
    /// folder, unless another deck was asked for.
    pub(crate) fn ensure_open(&mut self) {
        if !self.opened {
            self.opened = true;
            if let Some(card) = first_card().filter(|_| self.session.is_none() && self.opening.is_none() && self.folder_open.is_none()) {
                self.folder_open = Some(OpenJob::start(card));
            }
        }
    }

    /// The main talk, read and opened now: for a call that needs a deck open.
    pub(crate) fn open_bundled(&mut self, cx: &mut Cx, ui: &WidgetRef) -> Result<(), String> {
        let card = first_card().ok_or("this build has no folder decks")?;
        let tree = card.doc.read()?;
        let doc = super::document_tree::load_deck_at(&tree, &card.path)?;
        self.folder_open = None;
        self.open_folder_tree(cx, ui, card, tree, doc, false)
    }

    /// A folder deck read from disk, opened in place on its card's deck
    /// (`pending`: the other slides are still loading).
    fn open_folder_tree(&mut self, cx: &mut Cx, ui: &WidgetRef, card: Card, tree: Arc<TreeDraft>, doc: Arc<DeckDoc>, pending: bool) -> Result<(), String> {
        if !matches!(self.saving, Saving::Idle) || self.editing || !self.pending.is_empty() || self.source.as_ref().is_some_and(SourcePane::has_draft) || self.file_sources.values().any(SourcePane::has_draft) {
            return Err("The current tree is still being saved or edited".into());
        }
        if self.session.as_ref().is_some_and(|session| session.dirty()) { return Err("Save the current tree before opening another deck".into()); }
        self.opened = true;
        self.opening = None;
        self.tree_alias = None;
        self.open_tree(cx, ui, tree, doc, None, &card.path)?;
        self.folder = Some(OpenFolder::open(card.doc));
        self.completing = pending.then(|| (None, self.session.as_ref().map_or(0, |session| session.revision)));
        self.open_asset = None;
        self.lib_dirty = true;
        trace!("stage.deck", "deck: opened “{}” in place from {}", card.title, card.doc.dir.join(&card.path).display());
        if let Some(doc) = &self.doc { perf::open_installed(&doc.title, doc.slides.len(), pending); }
        Ok(())
    }

    /// Open a folder deck's card: another deck of the open tree is a switch
    /// (its edits stay); another folder is read from disk on a worker.
    pub(crate) fn open_card(&mut self, cx: &mut Cx, ui: &WidgetRef, card: Card) {
        let same_tree = self.folder.as_ref().is_some_and(|folder| std::ptr::eq(folder.doc, card.doc));
        if same_tree {
            if self.session.as_ref().is_some_and(|session| session.path != card.path) { self.switch_deck(cx, ui, card.path.clone()); }
            self.lib_dirty = true;
            return;
        }
        // The second click of a double-click: that deck is on its way.
        if self.folder_open.as_ref().is_some_and(|job| std::ptr::eq(job.card.doc, card.doc) && job.card.path == card.path) { return; }
        if self.session.as_ref().is_some_and(|session| session.dirty()) || self.editing || !self.pending.is_empty() || self.source.as_ref().is_some_and(SourcePane::has_draft) || self.file_sources.values().any(SourcePane::has_draft) {
            // Switching always works: the open deck autosaves first.
            self.card_after_save = Some(card);
            return;
        }
        self.load_error.clear();
        self.bar_notice = format!("Opening “{}”…", card.title);
        self.opening = None;
        self.folder_open = Some(OpenJob::start(card));
    }

    /// A tool's open by folder path (`motion-deck/deck.splash`, or `motion-deck`
    /// for its root deck): read and opened now. None: not a folder deck.
    pub(crate) fn open_folder_path(&mut self, cx: &mut Cx, ui: &WidgetRef, path: &str) -> Option<Result<(), String>> {
        let (folder, file) = path.trim_matches('/').split_once('/').unwrap_or((path, ""));
        let card = folder_docs::deck_cards().iter().find(|card| card.doc.name == folder && (file.is_empty() || card.path == file))?.clone();
        let same_tree = self.folder.as_ref().is_some_and(|open| std::ptr::eq(open.doc, card.doc));
        if same_tree {
            if self.session.as_ref().is_some_and(|session| session.path != card.path) { self.switch_deck(cx, ui, card.path.clone()); }
            return Some(Ok(()));
        }
        Some(card.doc.read().and_then(|tree| {
            let doc = super::document_tree::load_deck_at(&tree, &card.path)?;
            self.folder_open = None;
            self.open_folder_tree(cx, ui, card, tree, doc, false)
        }))
    }

    /// Stage is closing: the folder watcher (and its thread) stops.
    pub(crate) fn close_folder(&mut self) {
        if let Some(folder) = &mut self.folder { folder.stop_watching(); }
    }

    /// The folder read and loaded on its worker, and the open folder's files
    /// changed on disk.
    fn pump_folder(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        loop {
            let Some(job) = &self.folder_open else { break };
            let card = job.card.clone();
            let Some(loaded) = job.poll() else { break };
            match loaded {
                Loaded::First(tree, doc) => {
                    let opened = doc.into_deck().and_then(|doc| {
                        let pending = doc.pending();
                        self.open_folder_tree(cx, ui, card.clone(), tree, doc, pending).map(|()| pending)
                    });
                    match opened {
                        Ok(true) => {}
                        Ok(false) => self.folder_open = None,
                        Err(error) => {
                            self.load_error = format!("Could not open “{}”: {error}", card.title);
                            self.folder_open = None;
                        }
                    }
                    self.bar_notice.clear();
                }
                Loaded::Whole(doc) => {
                    self.folder_open = None;
                    self.completed(cx, ui, None, &card.path, Ok(doc));
                }
                Loaded::Failed(error) => {
                    self.folder_open = None;
                    self.bar_notice.clear();
                    self.load_error = format!("Could not open “{}”: {error}", card.title);
                }
            }
        }
        if let Some(read) = self.folder.as_ref().and_then(OpenFolder::poll) {
            self.folder_changed(read);
        }
    }

    /// The open folder as read again after a change on disk: the deck shown
    /// reloads on the slide it is on (only the slides whose files changed
    /// are evaluated again). A deck with changes of its own keeps them; Save
    /// writes the files it changed.
    fn folder_changed(&mut self, read: Result<Arc<TreeDraft>, String>) {
        let Some(name) = self.folder.as_ref().map(|folder| folder.doc.name.clone()) else { return };
        let tree = match read {
            Ok(tree) => tree,
            Err(error) => {
                self.load_error = format!("{name} changed on disk but does not read: {error}");
                return;
            }
        };
        let Some(session) = &self.session else { return };
        // What is open is what the disk holds (Stage's own save).
        if session.tree.as_ref() == Some(&tree) { return; }
        let busy = session.dirty() || self.editing || !self.pending.is_empty() || !matches!(self.saving, Saving::Idle) || self.completing.is_some()
            || self.folder_open.is_some() || self.source.as_ref().is_some_and(SourcePane::has_draft) || self.file_sources.values().any(SourcePane::has_draft);
        if busy {
            log!("deck: {name} changed on disk while the deck has unsaved changes: they stay");
            self.bar_notice = format!("{name} changed on disk: your unsaved changes stay; Save writes the files you changed");
            return;
        }
        let Some(before) = session.tree.clone() else { return };
        let shown = tree.files.get(&session.path).is_some_and(|file| file.kind == AssetKind::Deck);
        let path = if shown { session.path.clone() } else { tree.root.1.clone() };
        let loaded = if shown { super::document_tree::reload_deck(&tree, &path, &before, &session.doc) } else { super::document_tree::load_deck_at(&tree, &path) };
        let (doc, source) = match loaded.and_then(|doc| Ok((doc, tree.text(&path)?.to_string()))) {
            Ok(loaded) => loaded,
            Err(error) => {
                self.load_error = format!("{name} changed on disk and does not load: {error}");
                return;
            }
        };
        let changed = super::document_tree::changed_files(&before, &tree);
        let session = self.session.as_mut().unwrap();
        session.saved = source.clone();
        session.source = source;
        session.saved_doc = Some(doc.clone());
        session.doc = doc;
        session.saved_tree = Some(tree.clone());
        session.tree = Some(tree);
        session.saved_path = path.clone();
        session.path = path;
        // The files are the truth now: undo does not reach behind them.
        session.undo.clear();
        session.redo.clear();
        session.revision += 1;
        self.load_error.clear();
        self.bar_notice = format!("Reloaded from disk: {}", changed.join(", "));
        log!("deck: {name} changed on disk ({}): reloaded on the same slide", changed.join(", "));
        self.source_seen = None;
        self.files_seen = None;
        self.structure_seen = None;
        self.header_seen = None;
        self.file_sources.clear();
        self.reload = true;
    }

    /// Open a library deck's working tree, replacing the open deck
    /// (a tool's open: the error says why it did not load).
    pub(crate) fn open_library(&mut self, cx: &mut Cx, ui: &WidgetRef, opened: OpenTree) -> Result<(), String> {
        if !matches!(self.saving, Saving::Idle) || self.editing || !self.pending.is_empty() || self.source.as_ref().is_some_and(SourcePane::has_draft) || self.file_sources.values().any(SourcePane::has_draft) {
            return Err("The current tree is still being saved or edited".into());
        }
        if self.session.as_ref().is_some_and(|session| session.dirty()) { return Err("Save the current tree before opening another deck".into()); }
        self.opened = true;
        self.opening = None;
        self.tree_alias = Some(opened.alias);
        let doc = opened.doc.into_deck()?;
        let pending = doc.pending();
        self.folder_open = None;
        let path = opened.draft.root.1.clone();
        self.open_tree(cx, ui, opened.draft, doc, Some(opened.head), &path)?;
        self.completing = pending.then(|| (Some(opened.head), self.session.as_ref().map_or(0, |session| session.revision)));
        self.open_asset = Some(opened.head.asset_id);
        self.lib_dirty = true;
        if let Some(doc) = &self.doc { perf::open_installed(&doc.title, doc.slides.len(), pending); }
        Ok(())
    }

    /// A new deck from the template (one title slide, a dissolve between
    /// slides): open, its first slide selected in the strip, and published as
    /// a library asset as soon as the library is up. Refused while the open
    /// deck has changes that are not saved.
    pub(crate) fn new_deck(&mut self, cx: &mut Cx, ui: &WidgetRef, title: &str) -> Result<(), String> {
        if self.editing || !self.pending.is_empty() || self.history_busy() || self.source.as_ref().is_some_and(SourcePane::has_draft) || self.file_sources.values().any(SourcePane::has_draft) || self.session.as_ref().is_some_and(|session| session.dirty()) {
            // The open deck autosaves first; the new one follows.
            self.new_after_save = Some(title.to_string());
            return Ok(());
        }
        let source = makepad_story_deck::ops::new_deck_source(title)?;
        self.opened = true;
        self.opening = None;
        self.folder_open = None;
        self.open_source(cx, ui, &source, title)?;
        self.open_asset = None;
        if let Some(session) = &mut self.session {
            // Nothing is in the library yet: the deck is unsaved until it is.
            session.saved.clear();
        }
        self.publish_new = true;
        self.set_nav(cx, ui, Nav::Slides);
        if let Some(mut view) = ui.widget(cx, ids!(deck_view)).borrow_mut::<DeckView>() {
            view.command(cx, Command::Jump(0));
        }
        self.lib_dirty = true;
        Ok(())
    }

    /// Open library deck `reference` (another page's request).
    pub(crate) fn open_request(&mut self, reference: AssetRevisionRef, title: String) {
        self.opened = true;
        self.wanted = Some((reference, title));
    }

    /// Ask the library for deck `reference`; it opens when its tree arrives.
    /// The autosave: an edit that has stood still for a moment is saved as
    /// a library revision on its own (no Save action), and a deck asked for
    /// while it was unsaved opens once that save landed.
    fn pump_autosave(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        let Some(session) = &self.session else { self.autosave_seen = None; return };
        let (dirty, revision) = (session.dirty(), session.revision);
        let waiting = self.open_after_save.is_some() || self.new_after_save.is_some();
        let drafts = self.source.as_ref().is_some_and(SourcePane::has_draft) || self.file_sources.values().any(SourcePane::has_draft);
        let waiting = waiting || self.card_after_save.is_some();
        if !dirty && waiting && drafts && matches!(self.saving, Saving::Idle) && !self.save_requested {
            // Typed text not yet in the tree: save() flushes it first.
            self.autosaving = true;
            self.save(cx, ui);
            self.autosaving = false;
            return;
        }
        if !dirty {
            self.autosave_seen = None;
            if !drafts && matches!(self.saving, Saving::Idle) && self.pending.is_empty() && !self.editing {
                if let Some(title) = self.new_after_save.take() {
                    if let Err(error) = self.new_deck(cx, ui, &title) { self.load_error = error; }
                } else if let Some((reference, title)) = self.open_after_save.take() {
                    self.fetch(reference, title);
                } else if let Some(card) = self.card_after_save.take() {
                    self.open_card(cx, ui, card);
                }
            }
            return;
        }
        let now = std::time::Instant::now();
        let since = match self.autosave_seen {
            Some((seen, since)) if seen == revision => since,
            _ => { self.autosave_seen = Some((revision, now)); now }
        };
        if !matches!(self.saving, Saving::Idle) || self.save_requested { return; }
        if waiting || now.duration_since(since) >= AUTOSAVE_AFTER {
            self.autosaving = true;
            self.save(cx, ui);
            self.autosaving = false;
        }
    }

    fn fetch(&mut self, reference: AssetRevisionRef, _title: String) {
        self.opening = Some(reference);
        let alias = self.lib.rows.iter().find(|row| row.asset_id() == reference.asset_id).and_then(|row| row.alias.clone());
        self.lib.send(StoreJob::OpenTree { reference, alias });
    }

    /// The player takes the session's document, on the slide it was on.
    fn reload_view(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        self.reload = false;
        let Some(session) = &self.session else { return };
        let doc = Rc::new((*session.doc).clone());
        self.doc = Some(doc.clone());
        if let Some(mut view) = ui.widget(cx, ids!(deck_view)).borrow_mut::<DeckView>() {
            view.replace_doc(cx, doc);
            if let Some(index) = self.select.take() {
                view.command(cx, Command::Jump(index));
            }
        }
        self.synced = None;
    }

    /// The picture as the output window's source while DECK is shown.
    pub(super) fn feed(&self, cx: &mut Cx, ui: &WidgetRef) -> Option<(Rc<dyn FrameSource>, FrameBinding)> {
        let view = ui.widget(cx, ids!(deck_view));
        let mut view = view.borrow_mut::<DeckView>()?;
        let frames = view.frames();
        let binding = frames.binding();
        Some((frames, binding))
    }

    /// The output window's size in pixels while it shows the deck (None:
    /// closed): the slides are recorded at least that large.
    pub(super) fn set_output_pixels(&self, cx: &mut Cx, ui: &WidgetRef, pixels: Option<(u32, u32)>) {
        if let Some(mut view) = ui.widget(cx, ids!(deck_view)).borrow_mut::<DeckView>() {
            view.frames().set_reader_pixels(pixels);
        }
    }

    /// What the output window draws the deck in another language with.
    pub(super) fn output_lane(&self) -> output_lane::SharedOutput {
        output_lane::shared()
    }

    /// The deck keys, from the page's events while DECK is
    /// shown: not while the library's search field has the key focus.
    pub(super) fn handle_event(&mut self, cx: &mut Cx, ui: &WidgetRef, event: &Event, output_window: Option<WindowId>) {
        if self.bar_timer.is_event(event).is_some() {
            ui.widget(cx, ids!(deck_bar_spinner)).redraw(cx);
        }
        if !self.stopped && self.clock.handle_event(cx, ui, event) {
            self.sync_clock(cx, ui);
        }
        if self.stopped || self.doc.is_none() {
            return;
        }
        self.handle_file_drop(cx, ui, event);
        // While the inline editor owns the keys, Return and Escape belong
        // to that edit, never the presentation or the selected slide.
        if self.duration_edit.is_some() && matches!(event, Event::KeyDown(_) | Event::KeyUp(_) | Event::TextInput(_) | Event::TextCopy(_) | Event::TextCut(_)) { return; }
        if let Event::KeyDown(key) = event {
            if let Some(mut sorter) = ui.widget(cx, ids!(deck_sorter)).borrow_mut::<DeckSorter>() {
                if sorter.dragging() {
                    if key.key_code == KeyCode::Escape { sorter.cancel_drag(cx); }
                    return;
                }
            }
        }
        match event {
            Event::WindowGeomChange(change) if self.fullscreen_request == Some((change.window_id, change.new_geom.is_fullscreen)) => self.fullscreen_request = None,
            Event::WindowClosed(closed) if self.fullscreen_request.is_some_and(|(id, _)| id == closed.window_id) => self.fullscreen_request = None,
            _ => {}
        }
        // Typing in the chat or the source is not a deck key (while
        // presenting both are folded away: a focus they kept does not hold
        // the deck's keys, Esc included).
        if self.presenting.is_none() && has_focus(cx, ui.widget(cx, ids!(ai_chat)).widget(cx, ids!(input)).area()) {
            return;
        }
        if ui.menu_layer(cx, ids!(menus)).open_owner() == Some(live_id!(deck_file_menu)) || self.slide_menu_open(cx, ui)
            || ui.popover(cx, ids!(deck_file_prompt)).is_open() { return; }
        if let Event::KeyDown(key) = event {
            if key.key_code == KeyCode::KeyS && (key.modifiers.logo || key.modifiers.control) && !key.is_repeat { return self.save(cx, ui); }
        }
        if let Some(pane) = self.source.as_ref().filter(|_| self.presenting.is_none()) {
            if pane.has_focus(cx, ui) {
                return;
            }
        }
        if self.sketch_typing(cx, ui) { return; }
        // While an app of the slide holds the focus (a click on it gave it
        // the keys) its keys and text are its own, the arrows too. Escape
        // only lets go of the app; the next Escape is the deck's.
        if matches!(event, Event::KeyDown(_) | Event::KeyUp(_) | Event::TextInput(_) | Event::TextCopy(_) | Event::TextCut(_)) {
            if let Some(mut view) = ui.widget(cx, ids!(deck_view)).borrow_mut::<DeckView>() {
                if view.app_input(cx, event) {
                    return;
                }
            }
        }
        // Otherwise Esc ends present mode, whatever else holds the focus.
        if let (Event::KeyDown(key), Some(_)) = (event, self.presenting) {
            if key.key_code == KeyCode::Escape && !key.is_repeat && !key.modifiers.control {
                return self.end_present(cx, ui);
            }
        }
        let Event::KeyDown(key) = event else { return };
        if key.is_repeat && matches!(key.key_code, KeyCode::KeyF | KeyCode::Escape | KeyCode::ReturnKey) {
            return;
        }
        // Typing in the library's search is not a deck key.
        if has_focus(cx, ui.widget(cx, ids!(lib_search)).area()) {
            return;
        }
        let demo = ui.widget(cx, ids!(deck_view)).borrow::<DeckView>().is_some_and(|view| view.has_demo());
        let current = ui.widget(cx, ids!(deck_view)).borrow::<DeckView>().and_then(|view| view.current()).unwrap_or(0);
        let command = match key.key_code {
            KeyCode::KeyZ if key.modifiers.logo || key.modifiers.control => {
                self.pending.push_back(super::document_tree::Operation::Undo { redo: key.modifiers.shift });
                cx.redraw_all();
                return;
            }
            KeyCode::ReturnKey | KeyCode::NumpadEnter if key.modifiers.logo || key.modifiers.control => return self.start_present(cx, ui, key.modifiers.shift, output_window),
            // On a slide with a demo Space plays or pauses it (the arrows walk
            // its steps).
            KeyCode::Space if demo => Command::Demo { restart: false },
            // Right/Space/click step through the slide's beats, then on to the
            // next slide; Down/PageDown and Up/PageUp go a whole slide at once
            // (flipping through a deck never waits on its beats).
            KeyCode::ArrowRight | KeyCode::Space => Command::Next,
            KeyCode::ArrowLeft => Command::Previous,
            KeyCode::ArrowDown | KeyCode::PageDown => Command::Jump(current + 1),
            KeyCode::ArrowUp | KeyCode::PageUp => Command::Jump(current.saturating_sub(1)),
            KeyCode::Home => Command::First,
            KeyCode::End => Command::Last,
            KeyCode::KeyB => Command::Black,
            // Back to where the last link came from (another deck of the tree).
            KeyCode::Backspace => {
                self.go_back(cx, ui);
                return;
            }
            KeyCode::KeyR if key.modifiers.shift => return self.reset_clock(cx, ui),
            KeyCode::KeyR => Command::Restart,
            KeyCode::KeyT => {
                self.corner_clock = !self.corner_clock;
                return self.sync_clock(cx, ui);
            }
            KeyCode::KeyD => Command::Demo { restart: key.modifiers.shift },
            KeyCode::KeyF if key.modifiers.shift => return self.toggle_fullscreen(cx, ui, output_window),
            KeyCode::KeyF => {
                if self.own_window { self.leave_fullscreen(cx, ui, None); }
                return self.set_own_window(cx, ui, !self.own_window);
            }
            KeyCode::Escape => {
                if self.presenting.is_none() {
                    if let Some(mut view) = ui.widget(cx, ids!(deck_view)).borrow_mut::<DeckView>() { view.select_element(cx, None); }
                    self.element = None;
                }
                if !self.leave_fullscreen(cx, ui, output_window) && self.own_window {
                    self.set_own_window(cx, ui, false);
                }
                return;
            }
            _ => return,
        };
        if let Some(mut view) = ui.widget(cx, ids!(deck_view)).borrow_mut::<DeckView>() {
            view.command(cx, command);
        }
    }

    /// The page is left: the picture alone in the window ends, and the
    /// hosted apps quit (they launch again when the page is shown).
    /// The picture alone in the window.
    pub(super) fn own_window(&self) -> bool {
        self.own_window
    }

    pub(super) fn left(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        self.close_file_menu(cx, ui);
        self.close_slide_menus(cx, ui);
        self.cancel_duration(cx, ui);
        if let Some(mut sorter) = ui.widget(cx, ids!(deck_sorter)).borrow_mut::<DeckSorter>() { sorter.cancel_drag(cx); }
        self.clock.stop(cx);
        if self.presenting.is_some() {
            self.end_present(cx, ui);
        }
        ui.widget(cx, ids!(output_full)).set_visible(cx, false);
        if self.own_window {
            self.leave_fullscreen(cx, ui, None);
            self.set_own_window(cx, ui, false);
        }
        if let Some(mut view) = ui.widget(cx, ids!(deck_view)).borrow_mut::<DeckView>() {
            view.cancel_preview();
            view.shutdown_apps(cx);
        }
    }

    pub(super) fn stop(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        self.close_file_menu(cx, ui);
        self.close_slide_menus(cx, ui);
        self.cancel_duration(cx, ui);
        if let Some(mut sorter) = ui.widget(cx, ids!(deck_sorter)).borrow_mut::<DeckSorter>() { sorter.cancel_drag(cx); }
        self.stopped = true;
        self.clock.stop(cx);
        self.lib.stop();
        self.checker = None;
        self.pending.clear();
        if let Some(mut view) = ui.widget(cx, ids!(deck_view)).borrow_mut::<DeckView>() {
            view.cancel_preview();
            view.set_doc(cx, None);
        }
        self.doc = None;
    }

    /// Every pump: open the main talk on the first visit; labels follow the
    /// shown slide.
    pub(super) fn pump(&mut self, cx: &mut Cx, ui: &WidgetRef, shown: bool) {
        if self.stopped {
            return;
        }
        if !shown {
            return;
        }
        // The main talk opens the first time the page is shown.
        self.ensure_open();
        self.focus_duration(cx, ui);
        let started = self.clock.start(cx, ui);
        let ticking = self.clock.set_ticking(cx, self.own_window || self.presenting.is_some());
        let slide = ui.widget(cx, ids!(deck_view)).borrow::<DeckView>().and_then(|view| view.current());
        if started || ticking || slide != self.clock_slide {
            self.clock_slide = slide;
            self.sync_clock(cx, ui);
        }
        self.sync_columns(cx, ui);
        ui.widget(cx, ids!(output_full)).set_visible(cx, true);
        if self.lib.store.is_none() {
            self.lib.attach(cx, live_id!(deck_surface));
            self.lib.set_lane(PageLane::Store(LibLane::Decks));
            self.lib_dirty = true;
        }
        if let Some((reference, title)) = self.wanted.take() {
            self.fetch(reference, title);
        }
        self.pump_library(cx, ui);
        self.pump_autosave(cx, ui);
        // The folder decks' pictures: kept or placeholder at once, their
        // first slide's still drawn by the same host after first present.
        self.lib_dirty |= self.card_pictures.pump(cx);
        let demands = if self.panels_seen.is_some_and(|panels| panels[0]) {
            let cards = self.folder_cards();
            let (dpi, visible) = self.lib.visible_cards(cx, ui);
            let mut demands = self.card_pictures.demands(&visible, dpi, |index| cards.get(index).map(|card| card_ref(card)));
            demands.extend(self.lib.still_demands(cx, ui, cards.len()));
            demands
        } else { Vec::new() };
        if let Some(mut view) = ui.widget(cx, ids!(deck_view)).borrow_mut::<DeckView>() {
            self.lib_dirty |= self.lib.accept_stills(view.library_stills());
            self.lib_dirty |= self.card_pictures.accept(view.library_stills());
            view.set_library_stills(cx, demands);
        }

        self.fill_transitions(cx, ui);
        if self.publish_new && self.lib.store.is_some() && matches!(self.saving, Saving::Idle) {
            self.publish_new = false;
            if let Some(tree) = self.session.as_ref().filter(|session| session.dirty()).and_then(|session| session.tree.clone()) {
                self.publish(plain_poster(), tree);
            }
        }
        if self.reload {
            self.reload_view(cx, ui);
        } else if !self.editing && self.pending.is_empty() {
            // No edit queued or running: a dropped order that did not land goes back.
            if let Some(mut sorter) = ui.widget(cx, ids!(deck_sorter)).borrow_mut::<DeckSorter>() { sorter.settle_reorder(cx); }
        }
        self.pump_links(cx, ui);
        self.sync_header(cx, ui);
        self.preload_links();
        let current = ui.widget(cx, ids!(deck_view)).borrow::<DeckView>().and_then(|view| view.current());
        if current != self.synced {
            self.sync(cx, ui, current);
        } else if let Some(view) = ui.widget(cx, ids!(deck_view)).borrow::<DeckView>() {
            // The platform listed the cameras (or they changed).
            let revision = view.camera_devices().0;
            drop(view);
            if revision != self.camera_seen {
                self.sync_camera(cx, ui, current);
            }
        }
        self.pump_sketch(cx, ui);
        self.sync_bar_status(cx, ui);
        let opening = perf::opening() && self.source_seen.is_none();
        if opening { perf::open_mark("pump: header, slide sync"); }
        self.sync_presenter(cx, ui);
        if opening { perf::open_mark("pump: presenter"); }
        self.pump_i18n(cx, ui);
        self.pump_source(cx, ui, current);
        if opening { perf::open_mark("pump: source pane"); }
        self.sync_structure(cx, ui);
        if opening { perf::open_mark("pump: structure"); }
        if let Some(mut view) = ui.widget(cx, ids!(deck_view)).borrow_mut::<DeckView>() { view.set_presenting(self.presenting.is_some()); }
        if self.save_requested && !self.editing && self.pending.is_empty() && self.completing.is_none() {
            self.save_requested = false;
            if !self.source.as_ref().is_some_and(SourcePane::has_draft) && !self.file_sources.values().any(SourcePane::has_draft) { self.save(cx, ui); }
        }
    }

    /// The library: its replies, the folder deck being read, the deck being
    /// fetched, the save being published.
    fn pump_library(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        self.pump_folder(cx, ui);
        self.pump_dropped(cx);
        let pumped = self.lib.pump(cx);
        if pumped.rows_of.is_some() || pumped.uploaded {
            self.lib_dirty = true;
        }
        if self.lib_dirty {
            self.paint_library(cx, ui);
        }
        for reply in pumped.others {
            match reply {
                StoreReply::TreeOpened { requested, result } => {
                    if self.opening != Some(requested) { continue; }
                    perf::open_mark("tree reply on the UI thread");
                    self.opening = None;
                    match result {
                        Ok(opened) => if let Err(error) = self.open_library(cx, ui, opened) { self.load_error = error; },
                        Err(error) => self.load_error = format!("Could not open the deck: {error}"),
                    }
                }
                StoreReply::TreeSaved { result } => self.saved(result),
                StoreReply::TreeCompleted { head, result } => {
                    // The store completes the tree's root deck.
                    let root = self.session.as_ref().and_then(|session| session.tree.as_ref()).map(|tree| tree.root.1.clone()).unwrap_or_default();
                    self.completed(cx, ui, Some(head), &root, result);
                }
                _ => {}
            }
        }
    }

    pub(super) fn handle_actions(&mut self, cx: &mut Cx, ui: &WidgetRef, actions: &Actions, output_window: Option<WindowId>) {
        if self.stopped {
            return;
        }
        self.file_actions(cx, ui, actions);
        self.duration_actions(cx, ui, actions);
        self.i18n_actions(cx, ui, actions);
        if let Some(pane) = self.source.as_mut() {
            pane.handle_actions(cx, ui, actions);
        }
        let view = ui.widget(cx, ids!(deck_view));
        for action in actions.filter_widget_actions_cast::<DeckAction>(view.widget_uid()) {
            match action {
                DeckAction::Moved => self.sync(cx, ui, view.borrow::<DeckView>().and_then(|view| view.current())),
                DeckAction::Selected => self.sync_structure(cx, ui),
                DeckAction::None => {}
            }
        }
        // Typed sketch words are committed before any click's action.
        self.sketch_actions(cx, ui, actions);
        self.nav_actions(cx, ui, actions);
        self.slide_actions(cx, ui, actions);
        if self.lib.handle_search(cx, ui, actions) {
            self.lib_dirty = true;
        }
        let list = self.lib.list(cx, ui);
        for action in actions.filter_widget_actions_cast::<MediaListAction>(list.widget_uid()) {
            match action {
                // The library lists decks only: a deck card is opened, not dragged anywhere.
                MediaListAction::Drag(..) | MediaListAction::Drop(..) | MediaListAction::Cancel => continue,
                _ => {}
            }
            // A click picks a deck card; a double-click opens it.
            if let MediaListAction::Click(index) | MediaListAction::Open(index) = action {
                // The folder decks come first, then the library's.
                let cards = self.folder_cards();
                if let Some(card) = cards.get(index) {
                    self.open_card(cx, ui, card.clone());
                    cx.set_key_focus(Area::Empty);
                    continue;
                }
                let Some(row) = self.lib.rows.get(index - cards.len()) else { continue };
                let (reference, title) = (row.reference, row.title.clone());
                if self.session.as_ref().is_some_and(|session| session.head != Some(reference) && (session.dirty() || self.editing || !self.pending.is_empty() || self.source.as_ref().is_some_and(SourcePane::has_draft) || self.file_sources.values().any(SourcePane::has_draft))) {
                    // Switching always works: the open deck autosaves first.
                    self.open_after_save = Some((reference, title));
                } else if self.opening != Some(reference) {
                    // The second click of a double-click: that deck is on its way.
                    self.load_error.clear();
                    perf::open_begin(if matches!(action, MediaListAction::Open(_)) { "library double-click" } else { "library click" });
                    self.fetch(reference, title);
                }
                // The arrows belong to the deck from here: a library grid that
                // kept the key focus would move its selection (and open the
                // neighbouring deck) on every slide key.
                cx.set_key_focus(Area::Empty);
            }
        }
        if ui.button(cx, ids!(deck_add_slide)).clicked(actions) {
            let current = ui.widget(cx, ids!(deck_view)).borrow::<DeckView>().and_then(|view| view.current());
            // The one add-slide action: a sketch slide after the shown one,
            // typed on at once (same as the list's "New slide").
            if let Err(error) = self.add_sketch_slide(cx, ui, current.unwrap_or(usize::MAX)) { self.load_error = error; }
        }
        if ui.button(cx, ids!(deck_clock_reset)).clicked(actions) {
            self.reset_clock(cx, ui);
        }
        let present = ui.widget(cx, ids!(deck_present));
        if actions.filter_widget_actions_cast::<RenderButtonAction>(present.widget_uid()).any(|action| matches!(action, RenderButtonAction::Clicked)) {
            self.start_present(cx, ui, false, output_window);
        }
        if ui.button(cx, ids!(deck_present_first)).clicked(actions) {
            self.start_present(cx, ui, true, output_window);
        }
        self.transition_actions(cx, ui, actions);
        self.camera_actions(cx, ui, actions);
        if ui.button(cx, ids!(deck_new)).clicked(actions) {
            let _ = self.new_deck(cx, ui, "New deck");
        }
        if ui.button(cx, ids!(deck_save)).clicked(actions) {
            self.save(cx, ui);
        }
        if ui.button(cx, ids!(output_full)).clicked(actions) {
            self.set_own_window(cx, ui, true);
        }
        if ui.button(cx, ids!(deck_fullscreen)).clicked(actions) {
            self.toggle_fullscreen(cx, ui, output_window);
        }
        let sorter = ui.widget(cx, ids!(deck_sorter));
        for action in actions.filter_widget_actions_cast::<SorterAction>(sorter.widget_uid()) {
            match action {
                SorterAction::Jump(index) => if let Some(mut view) = view.borrow_mut::<DeckView>() {
                    self.file_slide = None;
                    self.synced = None;
                    view.command(cx, Command::Jump(index));
                },
                SorterAction::Edit { op, select } => {
                    match self.submit_op(cx, op) {
                        Ok(()) => { self.select = Some(select); self.file_slide = None; }
                        Err(error) => self.load_error = error,
                    }
                }
                SorterAction::Menu(index, abs) => self.open_slide_menu(cx, ui, index, abs),
                SorterAction::None => {}
            }
        }
    }

    /// Where a dragged card would land: the insertion line in the slide list
    /// (None: nowhere).
    fn drop_hint(&mut self, cx: &mut Cx, ui: &WidgetRef, at: Option<Vec2d>) {
        if let Some(mut sorter) = ui.widget(cx, ids!(deck_sorter)).borrow_mut::<DeckSorter>() {
            sorter.set_drop_hint(cx, at);
        }
    }

    /// The transition dropdowns: the catalogue's presets once it has
    /// loaded, and the choice of the shown slide.
    fn fill_transitions(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        if !self.trans_presets.is_empty() {
            return;
        }
        let mut listed = Vec::new();
        let picker = ui.widget(cx, ids!(deck_fx_picker));
        if !makepad_content_packs::picker::with_picker(&picker, &mut |picker| {
            picker.set_category(cx, "transition");
            picker.ensure_loaded(cx);
            listed = picker.listed();
        }) {
            return;
        }
        let usable: Vec<(String, String)> = listed.into_iter().filter(|row| row.why.is_none()).map(|row| (row.preset, row.title)).collect();
        if usable.is_empty() {
            return;
        }
        let mut labels = vec!["Deck default".to_string()];
        labels.extend(usable.iter().map(|(_, title)| title.clone()));
        self.trans_presets = usable.into_iter().map(|(preset, _)| preset).collect();
        ui.widget(cx, ids!(deck_trans)).as_drop_down().set_labels(cx, labels);
        self.synced = None;
    }

    /// The first `Camera` node of the slide's own cut.
    fn camera_of(&self, slide: usize) -> Option<CameraSpec> {
        match &self.doc.as_ref()?.slides.get(slide)?.cut {
            SlideCut::Inline(cut) => cut.nodes.iter().find_map(|node| match &node.kind {
                CutNodeKind::Camera(spec) => Some(spec.clone()),
                _ => None,
            }),
            SlideCut::Library { .. } | SlideCut::Pending => None,
        }
    }

    /// The Camera section: shown for a slide with a camera, listing the
    /// system's cameras and the picked camera's formats.
    fn sync_camera(&mut self, cx: &mut Cx, ui: &WidgetRef, current: Option<usize>) {
        if let Some(view) = ui.widget(cx, ids!(deck_view)).borrow::<DeckView>() {
            let (revision, devices) = view.camera_devices();
            self.camera_seen = revision;
            self.camera_list = devices;
        }
        let spec = current.and_then(|index| self.camera_of(index));
        ui.widget(cx, ids!(deck_camera)).set_visible(cx, spec.is_some());
        let Some(spec) = spec else { return };
        let names: Vec<&str> = self.camera_list.iter().map(|desc| desc.name.as_str()).collect();
        let picked = spec.device.pick(names.iter().copied());
        let device = ui.widget(cx, ids!(deck_cam_device)).as_drop_down();
        let format = ui.widget(cx, ids!(deck_cam_format)).as_drop_down();
        if names.is_empty() {
            device.set_labels(cx, vec!["No camera listed".to_string()]);
            format.set_labels(cx, vec!["-".to_string()]);
            self.camera_formats.clear();
        } else {
            device.set_labels(cx, names.iter().map(|name| name.to_string()).collect());
            if let Some(at) = picked {
                device.set_selected_item(cx, at);
                self.camera_formats = camera::usable_formats(&self.camera_list[at].formats);
                format.set_labels(cx, self.camera_formats.iter().map(camera::format_label).collect());
                let best = camera::best_format(&self.camera_formats, spec.width, spec.height, spec.fps);
                if let Some(at) = best.and_then(|best| self.camera_formats.iter().position(|f| f.width == best.width && f.height == best.height && f.frame_rate == best.frame_rate && f.pixel_format == best.pixel_format)) {
                    format.set_selected_item(cx, at);
                }
            } else {
                self.camera_formats.clear();
                format.set_labels(cx, vec!["-".to_string()]);
            }
        }
        let note = match picked {
            Some(at) => format!("{} · turned {}°{}", self.camera_list[at].name, spec.rotate, if spec.mirror { " · mirrored" } else { "" }),
            None if names.is_empty() => format!("No camera is listed; the slide asks for {}", spec.device.label()),
            None => format!("No camera matches {}; pick one", spec.device.label()),
        };
        ui.label(cx, ids!(deck_cam_note)).set_text(cx, &note);
    }

    fn camera_actions(&mut self, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
        let Some(current) = ui.widget(cx, ids!(deck_view)).borrow::<DeckView>().and_then(|view| view.current()) else { return };
        let Some(spec) = self.camera_of(current) else { return };
        let mut set = CameraSet::default();
        if let Some(at) = ui.widget(cx, ids!(deck_cam_device)).as_drop_down().changed(actions) {
            let Some(desc) = self.camera_list.get(at) else { return };
            set.device = Some(CameraDevice::Name(desc.name.clone()));
        }
        if let Some(at) = ui.widget(cx, ids!(deck_cam_format)).as_drop_down().changed(actions) {
            let Some(format) = self.camera_formats.get(at) else { return };
            set.size = Some((format.width as u32, format.height as u32, format.frame_rate.unwrap_or(spec.fps)));
        }
        if ui.button(cx, ids!(deck_cam_rotate)).clicked(actions) {
            set.rotate = Some((spec.rotate + 90) % 360);
        }
        if ui.button(cx, ids!(deck_cam_mirror)).clicked(actions) {
            set.mirror = Some(!spec.mirror);
        }
        if set != CameraSet::default() {
            let _ = self.submit_op(cx, DeckOp::SetCamera { index: current, set });
        }
    }

    const TRANSITION_LENGTHS: [f64; 5] = [0.3, 0.6, 1.0, 1.5, 2.0];

    /// The dropdowns show the shown slide's own `enter`.
    fn sync_transition(&mut self, cx: &mut Cx, ui: &WidgetRef, enter: Option<&Transition>) {
        let preset = enter.and_then(|enter| self.trans_presets.iter().position(|preset| *preset == enter.effect)).map_or(0, |at| at + 1);
        ui.widget(cx, ids!(deck_trans)).as_drop_down().set_selected_item(cx, preset);
        let length = enter.map_or(1, |enter| {
            Self::TRANSITION_LENGTHS.iter().enumerate().min_by(|a, b| (a.1 - enter.seconds).abs().total_cmp(&(b.1 - enter.seconds).abs())).map_or(1, |(at, _)| at)
        });
        ui.widget(cx, ids!(deck_trans_len)).as_drop_down().set_selected_item(cx, length);
    }

    fn transition_actions(&mut self, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
        let picked = ui.widget(cx, ids!(deck_trans)).as_drop_down().changed(actions);
        let length = ui.widget(cx, ids!(deck_trans_len)).as_drop_down().changed(actions);
        if picked.is_none() && length.is_none() {
            return;
        }
        let Some(current) = ui.widget(cx, ids!(deck_view)).borrow::<DeckView>().and_then(|view| view.current()) else { return };
        let preset = picked.unwrap_or_else(|| ui.widget(cx, ids!(deck_trans)).as_drop_down().selected_item());
        let length = length.unwrap_or_else(|| ui.widget(cx, ids!(deck_trans_len)).as_drop_down().selected_item());
        let enter = match preset.checked_sub(1).and_then(|at| self.trans_presets.get(at)) {
            Some(effect) => Some(Transition { seconds: Self::TRANSITION_LENGTHS[length.min(Self::TRANSITION_LENGTHS.len() - 1)], effect: effect.clone(), dials: Vec::new() }),
            None => None,
        };
        let _ = self.submit_op(cx, DeckOp::SetSlide { index: current, set: SlideSet { enter: Some(enter), ..SlideSet::default() } });
    }

    /// A slide name no slide has: `slide<n>`.
    fn fresh_slide_name(&self) -> String {
        let names: Vec<&str> = self.doc.as_ref().map(|doc| doc.slides.iter().map(|slide| slide.name.as_str()).collect()).unwrap_or_default();
        (1..).map(|n| format!("slide{n}")).find(|name| !names.contains(&name.as_str())).unwrap()
    }

    /// One seam for every change a person makes in the sorter: the same
    /// `DeckOp` the AI's slide tools run, installed in the deck session as
    /// one undo step; the player takes it on the next pump. A refused op
    /// leaves the deck alone and says why.
    pub(crate) fn submit_op(&mut self, _cx: &mut Cx, op: DeckOp) -> Result<(), String> {
        if self.session.is_none() { return Err("no deck is open".into()); }
        self.pending.push_back(super::document_tree::Operation::Deck(op));
        // The edit starts on the next pump; wake it now rather than at the
        // next 30 Hz tick (nothing changes on screen until it lands).
        makepad_widgets::makepad_platform::thread::SignalToUI::set_ui_signal();
        Ok(())
    }

    pub(crate) fn next_operation(&mut self) -> Option<super::document_tree::Operation> {
        if self.editing || self.completing.is_some() || (self.history_busy() && matches!(self.pending.front(), Some(super::document_tree::Operation::Undo { .. }))) { return None; }
        let operation = self.pending.pop_front()?;
        self.editing = true;
        if let (Some(live), super::document_tree::Operation::Document(makepad_app_shell::tool_host::Change::Write { path, text, .. })) = (&mut self.live_write, &operation) {
            live.running |= live.path == *path && live.text == *text;
        }
        Some(operation)
    }

    pub(crate) fn edit_finished(&mut self, error: Option<String>) {
        self.editing = false;
        // The code view's edit: its outcome is the pane's (a problem marked on
        // its line, the last picture kept), not the bar's.
        if let Some(live) = self.live_write.as_mut().filter(|live| live.running && live.result.is_none()) {
            live.result = Some(error.map_or(Ok(()), Err));
            return;
        }
        if error.is_some() { self.wanted_file = None; self.select = None; }
        self.load_error = error.unwrap_or_default();
    }

    /// A new deck's template starts as a one-file tree.
    /// Library decks always arrive through OpenTree with all sibling files.
    fn open_source(&mut self, cx: &mut Cx, ui: &WidgetRef, source: &str, name: &str) -> Result<(), String> {
        let doc = DeckDoc::load(source).map_err(|errors| makepad_story_deck::ops::describe(&errors))?;
        let cut = makepad_story_deck::ops::extract(source, &doc, 0)?;
        let source = makepad_story_deck::ops::reference_cut(source, &doc, 0, "slides/title.cut.splash")?;
        let mut tree = TreeDraft::new(name.into(), TreeRole::Deck, "deck.splash".into(), AssetKind::Deck, source)?;
        tree.write("slides/title.cut.splash", AssetKind::Cut, cut.text().as_bytes().into())?;
        let tree = Arc::new(tree);
        let doc = super::document_tree::load_deck(&tree)?;
        self.tree_alias = None;
        self.open_tree(cx, ui, tree, doc, None, "deck.splash")
    }

    /// Open deck file `path` of `tree` (saved as it is).
    fn open_tree(&mut self, cx: &mut Cx, ui: &WidgetRef, tree: Arc<TreeDraft>, doc: Arc<DeckDoc>, head: Option<AssetRevisionRef>, path: &str) -> Result<(), String> {
        self.close_file_menu(cx, ui);
        self.cancel_duration(cx, ui);
        self.completing = None;
        self.folder = None;
        let revision = self.session.as_ref().map_or(0, |session| session.revision) + 1;
        let mut session = Session::open(tree.text(path)?.into(), doc.clone(), revision, head, true);
        session.tree = Some(tree.clone());
        session.saved_tree = Some(tree.clone());
        session.path = path.to_string();
        session.saved_path = session.path.clone();
        session.alias = self.tree_alias.clone();
        self.session = Some(session);
        self.trail.clear();
        self.pending_switch = None;
        self.switch_seen = None;
        self.opened_serial += 1;
        self.active_file.clear();
        self.source_seen = None;
        self.revealed = None;
        self.file_sources.clear();
        self.file_slide = None;
        self.element = None;
        self.structure_seen = None;
        self.wanted_file = None;
        self.source = None;
        self.files_seen = None;
        self.pending.clear();
        self.save_requested = false;
        perf::open_mark("session open");
        let doc = Rc::new((*doc).clone());
        self.doc = Some(doc.clone());
        perf::open_mark("doc clone");
        if let Some(mut view) = ui.widget(cx, ids!(deck_view)).borrow_mut::<DeckView>() {
            view.set_doc(cx, Some(doc));
            perf::open_mark("view set_doc (player)");
            let thumbnails = view.thumbnails();
            if let Some(mut sorter) = ui.widget(cx, ids!(deck_sorter)).borrow_mut::<DeckSorter>() { sorter.bind(cx, thumbnails); }
        }
        self.load_error.clear();
        self.synced = None;
        self.set_columns(cx, ui);
        perf::open_mark("columns");
        Ok(())
    }

    fn sync_header(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        let Some(session) = &self.session else { return };
        let seen = (self.opened_serial, session.revision, session.head);
        if self.header_seen == Some(seen) {
            return;
        }
        self.header_seen = Some(seen);
        let doc = &session.doc;
        ui.label(cx, ids!(deck_title)).set_text(cx, &doc.title);
        // Unsaved changes tint the save button.
        if let Some(mut save) = ui.widget(cx, ids!(deck_save)).borrow_mut::<Button>() { save.set_open_with(cx, session.dirty(), Animate::No); }
        // Targets describe the speaker's allotted time. A looping cut's
        // duration is not a slide's allotted speaking time.
        self.clock.target_seconds = doc.talk_target();
        let seconds = self.clock.target_seconds.max(0.0) as u64;
        ui.button(cx, ids!(deck_duration)).set_text(cx, &format!("{} slides · {}:{:02}", doc.slides.len(), seconds / 60, seconds % 60));
        self.sync_clock(cx, ui);
    }

    fn sync_clock(&self, cx: &mut Cx, ui: &WidgetRef) {
        let view = ui.widget(cx, ids!(deck_view));
        let Some(mut view) = view.borrow_mut::<DeckView>() else { return };
        let (total, elapsed, _) = view.presenter().unwrap_or_default();
        let target = self.doc.as_ref().and_then(|doc| view.current().and_then(|index| doc.slides.get(index))).and_then(|slide| slide.target_seconds);
        let line = self.clock.update(cx, ui, total, elapsed, target);
        let back = self.return_line();
        ui.label(cx, ids!(deck_return)).set_text(cx, &back);
        ui.widget(cx, ids!(deck_return)).set_visible(cx, !back.is_empty());
        // While presenting, the speaker still sees the time left: a quiet
        // corner readout over the picture (T), never in the audience's output.
        view.set_clock_readout(cx, (self.presenting.is_some() && self.corner_clock).then_some(line));
    }

    /// Shift+R or the bar's button: the talk's clock starts again at 0.
    fn reset_clock(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        if let Some(mut view) = ui.widget(cx, ids!(deck_view)).borrow_mut::<DeckView>() {
            view.command(cx, Command::ResetClock);
        }
        log!("deck: talk clock reset");
        self.sync_clock(cx, ui);
    }

    /// The strip, the next slide and the notes for `current`.
    fn sync(&mut self, cx: &mut Cx, ui: &WidgetRef, current: Option<usize>) {
        self.synced = current;
        // Another slide: the "changed elsewhere" chip has had its moment.
        if let Some(pane) = &mut self.source { pane.dismiss_chip(); }
        for pane in self.file_sources.values_mut() { pane.dismiss_chip(); }
        let Some(doc) = self.doc.clone() else { return };
        if let Some(index) = current {
            if let Some(mut sorter) = ui.widget(cx, ids!(deck_sorter)).borrow_mut::<DeckSorter>() {
                sorter.follow(cx, index);
            }
        }
        let slide = current.and_then(|index| doc.slides.get(index));
        if self.file_slide != current {
            self.file_slide = current;
            if let Some(path) = slide.and_then(|slide| slide.graphic.clone()) {
                let _ = self.open_file(cx, ui, &path);
            }
        }
        ui.label(cx, ids!(deck_notes)).set_text(cx, &slide.map_or_else(String::new, |slide| notes_shown(&slide.notes)));
        let enter = slide.and_then(|slide| slide.enter.clone());
        self.sync_transition(cx, ui, enter.as_ref());
        self.sync_camera(cx, ui, current);
        self.sync_target(cx, ui, slide.and_then(|slide| slide.target_seconds));
        self.poster_revision = u64::MAX;
        self.beat_seen = None;
        self.sync_clock(cx, ui);
        self.sync_presenter(cx, ui);
    }

    fn sync_presenter(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        if self.own_window || self.side_structure || !self.panels_seen.is_some_and(|panels| panels[3]) { return; }
        let view = ui.widget(cx, ids!(deck_view));
        let Some(mut view) = view.borrow_mut::<DeckView>() else { return };
        let Some((_, _, (beat, beats))) = view.presenter() else { return };
        let current = view.current().unwrap_or(0);
        if self.beat_seen != Some((current, beat, beats)) {
            self.beat_seen = Some((current, beat, beats));
            ui.label(cx, ids!(deck_beats)).set_text(cx, &presenter::beats(beat, beats));
            let doc = self.doc.as_ref();
            
            let next = doc.and_then(|doc| doc.slides.get(current + 1));
            let label = next.map_or_else(|| "The last slide.".to_string(), |next| format!("Next slide · {}", next.name));
            ui.label(cx, ids!(deck_next)).set_text(cx, &label);
            self.poster_revision = u64::MAX;
        }
        let thumbnails = view.thumbnails();
        let mut thumbnails = thumbnails.borrow_mut();
        let output = thumbnails.doc.as_ref().map_or((16, 9), |doc| doc.output);
        let next_apps = thumbnails.doc.as_ref().and_then(|doc| doc.slides.get(current + 1)).and_then(thumbnails::live_label).unwrap_or_default();
        let has_next = thumbnails.doc.as_ref().is_some_and(|doc| current + 1 < doc.slides.len());
        let mut preview = ui.widget(cx, ids!(deck_next_preview));
        preview.set_visible(cx, has_next);
        let area = preview.area();
        let rect = area.rect(cx);
        let dpi = cx.get_dpi_factor_of(&area);
        let size = (has_next && rect.size.x > 0.0)
            .then(|| super::still_cache::Size::new(rect.size.x, dpi, output.0 as f64 / output.1.max(1) as f64));
        let resized = thumbnails.presenter_size != size;
        thumbnails.presenter_size = size;
        if self.poster_revision != thumbnails.revision || resized {
            self.poster_revision = thumbnails.revision;
            let (poster_w, poster_h) = (output.0 as u64, output.1 as u64);
            let height = rect.size.x * output.1 as f64 / output.0.max(1) as f64;
            if rect.size.x > 0.0 { script_apply_eval!(cx, preview, {height: #(height)}); }
            let mut picture = ui.widget(cx, ids!(deck_next_picture));
            script_apply_eval!(cx, picture, {placeholder_width: #(poster_w) placeholder_height: #(poster_h)});
            // Both maps contain cached final frames. Use the strip's still
            // immediately while the presenter-size variant is being prepared.
            let texture = thumbnails.presenter_textures.get(&(current + 1))
                .or_else(|| thumbnails.textures.get(&(current + 1))).cloned();
            let ready = texture.is_some();
            ui.image(cx, ids!(deck_next_picture)).set_texture(cx, texture);
            picture.set_visible(cx, has_next && next_apps.is_empty() && ready);
            ui.widget(cx, ids!(deck_next_pending)).set_visible(cx, has_next && next_apps.is_empty() && !ready);
            ui.widget(cx, ids!(deck_next_app)).set_visible(cx, has_next && !next_apps.is_empty());
            ui.label(cx, ids!(deck_next_app.name)).set_text(cx, &next_apps);
        }
        if resized { view.redraw(cx); }
    }

    /// The explorer: its chrome, and the lane's rows as cards.
    fn paint_library(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        self.lib_dirty = false;
        // Only a wait or a failure is worth a line; the count is not.
        let status = if self.lib.status.starts_with("Loading") || self.lib.status.starts_with("The library") { self.lib.status.as_str() } else { "" };
        ui.label(cx, ids!(lib_status)).set_text(cx, status);
        let kind = LibLane::Decks.card();
        let cards = self.folder_cards();
        let folder_rows = cards.iter().map(|card| ListRow {
            title: card.title.clone(),
            detail: self.card_pictures.detail(card),
            kind,
            texture: self.card_pictures.texture(card),
            aspect: self.card_pictures.aspect(card),
            ..ListRow::default()
        });
        let rows: Vec<ListRow> = folder_rows.chain(self.lib.rows.iter().map(|row| ListRow {
            title: row.title.clone(),
            detail: self.lib.live_name(row).map(|name| format!("App · {name}")).unwrap_or_else(|| if row.published == 0 { row.detail.clone() } else { super::cut::modified(row.published) }),
            aspect: self.lib.card_aspect(row),
            kind,
            texture: self.lib.live_name(row).is_none().then(|| self.lib.thumbs.get(&row.reference.asset_id.to_string()).map(|(_, texture)| texture.clone())).flatten(),
            seconds: row.seconds,
            ..ListRow::default()
        })).collect();
        let empty = if rows.is_empty() && !self.lib.status.starts_with("Loading") && !self.lib.status.starts_with("The library") {
            if self.lib.searching() { "No decks match".to_string() } else { "No decks yet".to_string() }
        } else {
            String::new()
        };
        let open_card = self.folder.as_ref().zip(self.session.as_ref()).and_then(|(folder, session)| cards.iter().position(|card| std::ptr::eq(card.doc, folder.doc) && card.path == session.path));
        let selected = open_card.or_else(|| self.open_asset.and_then(|asset| self.lib.rows.iter().position(|row| row.asset_id() == asset)).map(|index| index + cards.len()));
        if let Some(mut list) = self.lib.list(cx, ui).borrow_mut::<CutMediaList>() {
            list.set_rows(cx, rows, empty, kind);
            list.set_selected(cx, selected);
        }
        ui.redraw(cx);
    }

    /// Save the tree with its cached first-slide final picture and derived stills.
    fn save(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        if self.editing || self.completing.is_some() || !self.pending.is_empty() || self.source.as_ref().is_some_and(SourcePane::has_draft) || self.file_sources.values().any(SourcePane::has_draft) {
            if !self.save_requested {
                let mut drafts: Vec<_> = self.file_sources.iter().filter_map(|(path, pane)| pane.draft_text().map(|text| (path.clone(), text.to_string(), pane.draft_base().map(str::to_string)))).collect();
                if let Some(pane) = &self.source {
                    if let Some(text) = pane.draft_text() { drafts.push((self.active_file.clone(), text.into(), pane.draft_base().map(str::to_string))); }
                }
                for (path, text, expected) in drafts { self.pending.push_back(super::document_tree::Operation::Document(makepad_app_shell::tool_host::Change::Write { path, text, expected })); }
            }
            self.save_requested = true;
            self.load_error = "Checking file drafts before saving the tree".into();
            cx.redraw_all();
            return;
        }
        if !matches!(self.saving, Saving::Idle) {
            return;
        }
        let Some(session) = &self.session else { return };
        if let Some(folder) = &mut self.folder {
            if folder.doc.writable {
                // In place: the changed files go back into the folder.
                let (Some(saved), Some(tree)) = (session.saved_tree.clone(), session.tree.clone()) else { return };
                // Another deck is waiting to open: switching never waits, so what
                // the folder cannot take now is kept aside, said in the bar.
                let switching = self.open_after_save.is_some() || self.new_after_save.is_some() || self.card_after_save.is_some();
                if self.autosaving && switching {
                    self.bar_notice = folder.save_or_keep(&saved, &tree);
                    let session = self.session.as_mut().unwrap();
                    session.saved = session.source.clone();
                    session.saved_doc = Some(session.doc.clone());
                    session.saved_path = session.path.clone();
                    session.saved_tree = Some(tree);
                    self.load_error.clear();
                    return;
                }
                // Refused for a file changed on disk too: the autosave waits for an edit or a Save.
                if self.autosaving && self.folder_refused == Some(session.revision) { return; }
                match folder.save(&saved, &tree, !self.autosaving) {
                    Ok(0) => { self.load_error.clear(); self.bar_notice = "Already saved".into(); }
                    Ok(written) => {
                        let dir = folder.doc.dir.display().to_string();
                        let session = self.session.as_mut().unwrap();
                        session.saved = session.source.clone();
                        session.saved_doc = Some(session.doc.clone());
                        session.saved_path = session.path.clone();
                        session.saved_tree = Some(tree);
                        self.load_error.clear();
                        self.bar_notice = format!("Saved {written} file{} to {dir}", if written == 1 { "" } else { "s" });
                        self.header_seen = None;
                        self.files_seen = None;
                    }
                    Err(error) => {
                        log!("deck: {error}");
                        self.folder_refused = Some(session.revision);
                        self.load_error = error;
                    }
                }
                return;
            }
            // A read-only folder deck: its first save is the person's own copy in the store.
            if !session.dirty() {
                self.load_error.clear();
                self.bar_notice = "Already saved".into();
                return;
            }
        }
        let carries_stills = session.tree.as_ref().is_some_and(|tree| ui.widget(cx, ids!(deck_view)).borrow::<DeckView>()
            .is_some_and(|view| view.has_uncarried_stills(tree)));
        if !session.dirty() && session.head.is_some() && !carries_stills && !session.internal_changes() {
            self.load_error.clear();
            self.bar_notice = "Already saved".into();
            return;
        }
        let Some(tree) = session.tree.clone() else { return };
        let poster = ui.widget(cx, ids!(deck_view)).borrow::<DeckView>().and_then(|view| view.first_still());
        self.publish(poster.unwrap_or_else(plain_poster), tree);
    }

    
    

    fn publish(&mut self, thumbnail: Thumb, tree: Arc<TreeDraft>) {
        let Some(session) = &self.session else { return };
        // Folder handles are not store revisions: a folder deck's copy goes in as its bytes.
        let tree = if self.folder.is_some() { folder_docs::for_store(&tree) } else { tree };
        self.lib.send(StoreJob::SaveTree(Box::new(TreeSave { draft: tree.clone(), head: session.head, alias: self.tree_alias.clone(), thumbnail, path: tree.root.1.clone(), settled: false, dpi: 1.0 })));
        self.saving = Saving::Publishing { tree };
    }

    /// Every slide of the open deck is loaded: the session, the page and the
    /// player take the whole deck (an edit or another deck since: dropped).
    fn completed(&mut self, cx: &mut Cx, ui: &WidgetRef, head: Option<AssetRevisionRef>, path: &str, result: Result<super::cut::open_doc::TreeDoc, String>) {
        // Another deck of the tree shown since: this completion is dropped.
        let Some(session) = self.session.as_mut().filter(|session| self.completing == Some((head, session.revision)) && session.path == path) else { return };
        self.completing = None;
        let doc = match result.and_then(|doc| doc.into_deck()) {
            Ok(doc) => doc,
            Err(error) => { self.load_error = format!("The deck's slides did not load: {error}"); return; }
        };
        if session.saved_doc.as_ref().is_some_and(|saved| Arc::ptr_eq(saved, &session.doc)) { session.saved_doc = Some(doc.clone()); }
        session.doc = doc.clone();
        let doc = Rc::new((*doc).clone());
        self.doc = Some(doc.clone());
        if let Some(mut view) = ui.widget(cx, ids!(deck_view)).borrow_mut::<DeckView>() { view.complete_doc(cx, doc); }
        self.synced = None;
        self.header_seen = None;
        self.structure_seen = None;
        perf::open_late("whole deck installed");
        cx.redraw_all();
    }

    fn saved(&mut self, result: Result<OpenTree, String>) {
        let Saving::Publishing { tree } = std::mem::take(&mut self.saving) else { return };
        match result {
            Ok(opened) => {
                let doc = match opened.doc.into_deck() { Ok(doc) => doc, Err(error) => { self.load_error = error; return; } };
                if let Some(session) = &mut self.session {
                    let (shown, shown_doc) = (session.path.clone(), session.doc.clone());
                    session.published(&tree, opened.draft, opened.path, doc, opened.head);
                    session.alias = Some(opened.alias.clone());
                    // The publication answers with the root deck; another deck
                    // of the tree stays the one shown (its content is unchanged).
                    if let Some(published) = session.tree.clone().filter(|published| shown != published.root.1) {
                        session.path = shown;
                        session.doc = shown_doc;
                        if let Ok(text) = published.text(&session.path) { session.source = text.into(); }
                    }
                }
                self.tree_alias = Some(opened.alias);
                // A read-only folder deck saved as the person's copy is a library deck from here.
                self.folder = None;
                self.load_error.clear();
                self.open_asset = Some(opened.head.asset_id);
                self.lib.query();
                self.lib_dirty = true;
                self.header_seen = None;
                self.files_seen = None;
                if let Some((reference, title)) = self.open_after_save.take() { self.fetch(reference, title); }
            }
            Err(error) => { self.open_after_save = None; self.load_error = format!("Not saved: {error}"); }
        }
    }

    /// The deck's one status function: the page's error or notice, what the
    /// player waits for (the slide loading, apps starting, the demo), the
    /// output translation. Routed by [`route_status`]: the code editor's bar
    /// says it (an error in red, the full text in its tooltip); with no
    /// editor bar, a fixed alert in the top bar carries the error. The top
    /// bar never holds status text. Painted only when it changes; the
    /// spinner turns on a timer while something is busy.
    fn sync_bar_status(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        let player = ui.widget(cx, ids!(deck_view)).borrow::<DeckView>().map(|view| view.status()).unwrap_or_default();
        let lines = [self.load_error.clone(), self.bar_notice.clone(), self.sketch_status(), player];
        let mut status = bar_status(&lines, 1);
        // "Fancy it up" is still running even when its line reads well.
        if self.sketch_busy() { if let Some(status) = &mut status { status.2 = true; } }
        let editor = self.source.is_some() && self.presenting.is_none() && !self.own_window
            && ui.splitter(cx, ids!(deck_code_split)).collapse() != SplitterCollapse::B
            && ui.widget(cx, ids!(deck_source)).visible();
        let route = route_status(status.as_ref(), editor);
        if let Some(pane) = self.source.as_mut() { pane.set_notice(route.editor.clone()); }
        if self.bar_status == status && self.bar_editor == editor { return; }
        ui.widget(cx, ids!(deck_bar_spinner)).set_visible(cx, route.busy);
        ui.widget(cx, ids!(deck_bar_alert)).set_visible(cx, route.alert.is_some());
        ui.widget(cx, ids!(deck_bar_alert_tip)).as_tip().set_text(route.alert.as_deref().unwrap_or_default());
        if route.busy && self.bar_timer.is_empty() { self.bar_timer = cx.start_interval(1.0 / 15.0); }
        if !route.busy && !self.bar_timer.is_empty() { cx.stop_timer(self.bar_timer); self.bar_timer = Timer::default(); }
        self.bar_status = status;
        self.bar_editor = editor;
        ui.widget(cx, ids!(deck_bar)).redraw(cx);
    }

    fn set_columns(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        let presenting = self.own_window || self.presenting.is_some();
        if presenting {
            self.cancel_duration(cx, ui);
            if self.saved_columns.is_none() {
                self.saved_columns = Some(super::cut::DECK_COLUMNS.map(|(_, id)| ui.widget(cx, &[id]).visible()));
            }
            ui.widget(cx, ids!(deck_strip)).set_visible(cx, false);
            ui.widget(cx, ids!(deck_centre)).set_visible(cx, true);
            ui.widget(cx, ids!(deck_side)).set_visible(cx, !self.own_window);
        } else if let Some(columns) = self.saved_columns.take() {
            for ((_, id), visible) in super::cut::DECK_COLUMNS.into_iter().zip(columns) {
                ui.widget(cx, &[id]).set_visible(cx, visible);
            }
        }
        // The dual-display presenter keeps the header and its clock.
        ui.widget(cx, ids!(deck_bar)).set_visible(cx, !self.own_window);
        ui.widget(cx, ids!(deck_bar_actions)).set_visible(cx, !presenting);
        ui.widget(cx, ids!(deck_panel_toggles)).set_visible(cx, !presenting);
        self.set_nav(cx, ui, self.nav);
        ui.redraw(cx);
    }

    /// Constant-size state comparisons on the pump; only a layout change
    /// updates button styles, column widths and thumbnail demand.
    fn sync_columns(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        let top = ui.splitter(cx, ids!(deck_code_split)).collapse() != SplitterCollapse::A;
        let strip = ui.widget(cx, ids!(deck_strip)).visible();
        let navigation = ui.splitter(cx, ids!(deck_nav_split));
        let collapse = if strip { SplitterCollapse::None } else { SplitterCollapse::A };
        if navigation.collapse() != collapse { navigation.set_collapse(cx, collapse); }
        let centre = top && ui.widget(cx, ids!(deck_centre)).visible();
        let side = top && ui.widget(cx, ids!(deck_side)).visible();
        // The library button is lit while the left column shows the
        // library; the slides button while it shows the slides or files.
        let states = [
            strip && self.nav == Nav::Library,
            strip && self.nav != Nav::Library,
            centre,
            side,
            ui.splitter(cx, ids!(chat_split)).collapse() != SplitterCollapse::B,
            ui.splitter(cx, ids!(deck_code_split)).collapse() != SplitterCollapse::B,
        ];
        if self.panels_seen == Some(states) {
            return;
        }
        self.panels_seen = Some(states);
        for (id, open) in [
            live_id!(deck_toggle_library), live_id!(deck_toggle_sorter), live_id!(deck_toggle_view),
            live_id!(deck_toggle_side), live_id!(deck_toggle_chat), live_id!(deck_toggle_code),
        ].into_iter().zip(states) {
            if let Some(mut button) = ui.widget(cx, &[id]).borrow_mut::<Button>() {
                button.set_open_with(cx, open, Animate::No);
            }
        }
        // The presenter column fills the space vacated by the picture; the
        // saved splitter sizes are never overwritten by this reflow.
        let mut column = ui.widget(cx, ids!(deck_side));
        if centre {
            let width = if self.presenting == Some(PresentMode::Dual) { 560.0 } else { 296.0 };
            script_apply_eval!(cx, column, {width: #(width)});
        } else {
            script_apply_eval!(cx, column, {width: Fill});
        }
        if let Some(mut view) = ui.widget(cx, ids!(deck_view)).borrow_mut::<DeckView>() {
            let thumbnails = view.thumbnails();
            let mut thumbnails = thumbnails.borrow_mut();
            thumbnails.sorter_visible = strip && self.nav == Nav::Slides;
            thumbnails.presenter_visible = side && !self.side_structure;
        }
        if !strip {
            if let Some(mut sorter) = ui.widget(cx, ids!(deck_sorter)).borrow_mut::<DeckSorter>() { sorter.cancel_drag(cx); }
        }
        ui.redraw(cx);
    }

    /// The chat and the code editor fold away with their bars, and come back
    /// as they were.
    fn fold_docks(&mut self, cx: &mut Cx, ui: &WidgetRef, on: bool) {
        let docks = [live_id!(chat_split), live_id!(deck_code_split)];
        if on {
            self.saved_docks = docks.iter().map(|id| ui.splitter(cx, &[*id]).collapse()).collect();
            for id in docks {
                let split = ui.splitter(cx, &[id]);
                split.set_collapse(cx, SplitterCollapse::B);
                let mut widget = ui.widget(cx, &[id]);
                script_apply_eval!(cx, widget, {use mod.prelude.widgets.*; size: 0.0});
            }
        } else {
            for (id, collapse) in docks.iter().zip(std::mem::take(&mut self.saved_docks)) {
                let mut widget = ui.widget(cx, &[*id]);
                script_apply_eval!(cx, widget, {use mod.prelude.widgets.*; size: 8.0});
                ui.splitter(cx, &[*id]).set_collapse(cx, collapse);
            }
        }
        let mut page = ui.widget(cx, ids!(deck_page));
        let padding = if on { 0.0 } else { 8.0 };
        script_apply_eval!(cx, page, {use mod.prelude.widgets.*; padding: #(padding)});
    }

    /// The presenter column in big type (the presenter view), or as it is.
    fn style_presenter(&mut self, cx: &mut Cx, ui: &WidgetRef, big: bool) {
        let (width, picture, notes, notes_height, time) = if big { (560.0, 250.0, 22.0, 360.0, 16.0) } else { (296.0, 138.0, 9.0, 110.0, 9.0) };
        let mut side = ui.widget(cx, ids!(deck_side));
        script_apply_eval!(cx, side, {use mod.prelude.widgets.*; width: #(width)});
        let mut next = ui.widget(cx, ids!(deck_next_preview));
        script_apply_eval!(cx, next, {use mod.prelude.widgets.*; height: #(picture)});
        let mut scroll = ui.widget(cx, ids!(deck_notes_scroll));
        script_apply_eval!(cx, scroll, {use mod.prelude.widgets.*; height: #(notes_height)});
        for (id, size) in [(live_id!(deck_notes), notes), (live_id!(deck_beats), time)] {
            let mut label = ui.widget(cx, &[id]);
            script_apply_eval!(cx, label, {use mod.prelude.widgets.*; draw_text +: {text_style +: {font_size: #(size)}}});
        }
    }

    /// Present: from the current slide, or from slide 1. With a second
    /// display the output window goes fullscreen there and Stage's window is
    /// the presenter view; with one, the picture alone fills Stage's window.
    /// Esc ends it.
    fn start_present(&mut self, cx: &mut Cx, ui: &WidgetRef, from_start: bool, output_window: Option<WindowId>) {
        if self.presenting.is_some() {
            return;
        }
        if self.doc.is_none() {
            self.load_error = "There is nothing to present: make a deck (+ New deck) or open one".into();
            return;
        }
        let main = ui.window(cx, ids!(main_window));
        let at = main.window_id().map_or(dvec2(0.0, 0.0), |_| main.get_position(cx));
        let (mode, screen) = present::plan(&screens(), at);
        self.load_error.clear();
        if let Some(mut view) = ui.widget(cx, ids!(deck_view)).borrow_mut::<DeckView>() {
            if from_start {
                view.command(cx, Command::First);
            }
        }
        self.presenting = Some(mode);
        // The keys are the deck's while it presents (not the source's or
        // the chat's, which are folded away).
        cx.set_key_focus(Area::Empty);
        log!("deck: present ({mode:?}) from {}", if from_start { "slide 1" } else { "the current slide" });
        match (mode, screen) {
            // One display with the output window open (dragged to a
            // projector, or a second window): it is the audience's, and this
            // window stays the presenter view.
            (PresentMode::Single, _) if output_window.is_some() => {
                self.presenting = Some(PresentMode::Dual);
                self.fold_docks(cx, ui, true);
                self.set_columns(cx, ui);
                self.style_presenter(cx, ui, true);
            }
            (PresentMode::Dual, Some(screen)) => {
                self.fold_docks(cx, ui, true);
                self.set_columns(cx, ui);
                self.style_presenter(cx, ui, true);
                self.present_ask = Some(PresentAsk::OpenOutput { screen });
            }
            _ => {
                self.presenting = Some(PresentMode::Single);
                self.set_own_window(cx, ui, true);
                if let Some(id) = main.window_id() {
                    self.request_fullscreen(cx, id, true);
                }
            }
        }
        ui.redraw(cx);
    }

    /// Esc: everything present mode changed is put back.
    fn end_present(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        let Some(mode) = self.presenting.take() else { return };
        log!("deck: present ended ({mode:?})");
        match mode {
            PresentMode::Single => {
                self.leave_fullscreen(cx, ui, None);
                self.set_own_window(cx, ui, false);
            }
            PresentMode::Dual => {
                self.style_presenter(cx, ui, false);
                self.fold_docks(cx, ui, false);
                self.present_ask = Some(PresentAsk::CloseOutput);
                self.set_columns(cx, ui);
            }
        }
        if let Some(mut view) = ui.widget(cx, ids!(deck_view)).borrow_mut::<DeckView>() {
            view.set_bare(cx, false);
        }
        ui.redraw(cx);
    }

    /// What present mode needs the app to do with the output window.
    pub(super) fn take_present_ask(&mut self) -> Option<PresentAsk> {
        self.present_ask.take()
    }

    pub(super) fn presenting(&self) -> Option<PresentMode> {
        self.presenting
    }

    /// The output window could not be opened: present on one display.
    pub(super) fn present_failed(&mut self, cx: &mut Cx, ui: &WidgetRef, why: &str) {
        if self.presenting != Some(PresentMode::Dual) {
            return;
        }
        log!("deck: the output window did not open ({why}); presenting in Stage's window");
        self.style_presenter(cx, ui, false);
        self.fold_docks(cx, ui, false);
        self.presenting = Some(PresentMode::Single);
        self.set_own_window(cx, ui, true);
        if let Some(id) = ui.window(cx, ids!(main_window)).window_id() {
            self.request_fullscreen(cx, id, true);
        }
        self.load_error = format!("The second display is not available ({why}): presenting in this window");
    }

    /// The picture alone fills the window (F), or the page again (Esc).
    fn set_own_window(&mut self, cx: &mut Cx, ui: &WidgetRef, on: bool) {
        if self.own_window == on {
            return;
        }
        self.own_window = on;
        log!("deck: own-window picture {}", if on { "on" } else { "off" });
        self.set_columns(cx, ui);
        self.fold_docks(cx, ui, on);
        let mut window = ui.widget(cx, ids!(main_window));
        // The picture alone has no caption: the window drags from anywhere then.
        if on {
            script_apply_eval!(cx, window, {use mod.prelude.widgets.*; show_caption_bar: false drag: WindowDrag.Window});
        } else {
            script_apply_eval!(cx, window, {use mod.prelude.widgets.*; show_caption_bar: true drag: WindowDrag.Caption});
        }
        if let Some(mut view) = ui.widget(cx, ids!(deck_view)).borrow_mut::<DeckView>() {
            view.set_bare(cx, on);
        }
        ui.redraw(cx);
    }

    fn is_fullscreen(&self, cx: &Cx, id: WindowId) -> bool {
        if !cx.windows.is_valid(id) { return false; }
        self.fullscreen_request.filter(|(window, _)| *window == id).map_or(cx.windows[id].window_geom.is_fullscreen, |(_, on)| on)
    }

    fn request_fullscreen(&mut self, cx: &mut Cx, id: WindowId, on: bool) {
        // The generic operations used by WindowHandle::fullscreen/normal.
        cx.push_unique_platform_op(if on { CxOsOp::FullscreenWindow(id) } else { CxOsOp::NormalizeWindow(id) });
        self.fullscreen_request = Some((id, on));
    }

    fn toggle_fullscreen(&mut self, cx: &mut Cx, ui: &WidgetRef, output_window: Option<WindowId>) {
        if output_window.is_none() && !self.own_window {
            self.set_own_window(cx, ui, true);
        }
        if let Some(id) = output_window.or_else(|| ui.window(cx, ids!(main_window)).window_id()) {
            let on = !self.is_fullscreen(cx, id);
            self.request_fullscreen(cx, id, on);
        }
    }

    /// Escape leaves OS fullscreen first; another Escape leaves own-window.
    fn leave_fullscreen(&mut self, cx: &mut Cx, ui: &WidgetRef, output_window: Option<WindowId>) -> bool {
        let main = ui.window(cx, ids!(main_window)).window_id().filter(|_| self.own_window);
        for id in output_window.into_iter().chain(main) {
            if self.is_fullscreen(cx, id) {
                self.request_fullscreen(cx, id, false);
                return true;
            }
        }
        false
    }
}

/// Whether a field that is on the page has the key focus. A field that was
/// never drawn (the chat folded away) has no area; "nothing is focused" must
/// not count as that field being focused, or the deck keys would be dead.
fn has_focus(cx: &Cx, area: Area) -> bool {
    area != Area::Empty && cx.has_key_focus(area)
}

/// The longest notes the presenter column lays out; the rest is in the source.
const NOTES_SHOWN: usize = 6000;

/// A slide's notes as the presenter column shows them: very long notes are cut
/// (at a character boundary) so laying them out never stalls the page.
fn notes_shown(notes: &str) -> String {
    match notes.char_indices().nth(NOTES_SHOWN) {
        None => notes.to_string(),
        Some((at, _)) => format!("{}\n... ({} more characters: the rest is in the source)", &notes[..at], notes[at..].chars().count()),
    }
}

/// The poster of a deck whose first slide has not been drawn: a dark card.
fn plain_poster() -> Thumb {
    Thumb { width: PLAIN_POSTER.0, height: PLAIN_POSTER.1, pixels: std::sync::Arc::new(vec![0xff10_1010; PLAIN_POSTER.0 * PLAIN_POSTER.1]) }
}







#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_notes_are_cut_on_a_character_boundary() {
        assert_eq!(notes_shown("short"), "short");
        let long = "é".repeat(NOTES_SHOWN + 10);
        let shown = notes_shown(&long);
        assert!(shown.starts_with(&"é".repeat(NOTES_SHOWN)));
        assert!(shown.contains("10 more characters"));
        let huge = notes_shown(&"word ".repeat(20_000));
        assert!(huge.len() < 6200, "{}", huge.len());
        assert_eq!(notes_shown(&"x".repeat(NOTES_SHOWN)), "x".repeat(NOTES_SHOWN));
    }

    fn status(text: &str) -> (String, String, bool, bool) {
        bar_status(&[text.to_string()], 1).expect("a status")
    }

    #[test]
    fn an_error_goes_to_the_editor_bar_when_it_is_there_and_to_an_alert_otherwise() {
        let error = status("Could not open the deck: not found: server object 12");
        assert!(error.3);
        let with_editor = route_status(Some(&error), true);
        let (line, tip, red) = with_editor.editor.expect("the editor bar says it");
        assert!(line.starts_with("Could not open the deck") && tip.contains("server object") && red);
        assert_eq!(with_editor.alert, None);
        let without = route_status(Some(&error), false);
        assert_eq!(without.editor, None);
        assert_eq!(without.alert.as_deref(), Some(error.1.as_str()));
    }

    #[test]
    fn a_notice_is_the_editor_bars_and_never_an_alert_and_none_is_silent() {
        let notice = bar_status(&["Saved 3 files to /x".to_string()], 0).unwrap();
        assert!(!notice.3);
        assert!(route_status(Some(&notice), true).editor.is_some());
        let hidden = route_status(Some(&notice), false);
        assert_eq!((hidden.editor, hidden.alert), (None, None));
        let busy = bar_status(&["Opening “A”…".to_string()], 0).unwrap();
        assert!(route_status(Some(&busy), false).busy);
        assert_eq!(route_status(None, true), StatusRoute { editor: None, alert: None, busy: false });
    }

    #[test]
    fn a_long_status_line_is_bounded_with_the_whole_text_in_the_tip() {
        let long = status(&format!("Could not open the deck: {}", "x".repeat(400)));
        assert!(long.0.chars().count() <= BAR_STATUS_CHARS + 1);
        assert!(long.1.chars().count() > 400);
    }
}
