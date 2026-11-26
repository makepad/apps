//! The CUT page: a timeline editor over one cut's Splash.
//!
//! Top: the library (lanes VIDEO, IMAGE, AUDIO, CUTS, APP RUNS and the
//! bundled effects and transitions, as a thumbnail grid), the viewer at the
//! edit's output aspect with its transport, and the inspector. Bottom,
//! across the width and resizable: the timeline. The open cut is its
//! source text and the `CutDoc` loaded from it; the timeline and the
//! viewer are projections of that document. Every change is a graph edit
//! written through the source-preserving patchers on the cut worker
//! (`worker.rs`) and becomes a new source revision on the undo stack. SAVE
//! publishes the source as an `AssetKind::Cut` revision whose
//! dependencies are every revision it names.
//!
//! The same asset worker answers the VJ grid's REMOVE: who uses an asset.
//!
//! Keys (with the timeline focused): space play/pause, J/K/L shuttle,
//! I/O marks, B blade, delete, cmd-Z / shift-cmd-Z, shift-Z fit, arrows
//! step a frame (shift: ten), +/- zoom, Home to the start, Esc drops an
//! armed effect.
//!
//! Main-loop cost: the page's pump reads at most a handful of replies,
//! uploads at most two thumbnails, and re-plans the viewer only when the
//! playhead, the skimmer or the document changes (`frame_plan` is linear
//! in the cut's nodes: tens). Splash never evaluates on the UI thread.

mod autosave;
pub(crate) mod chat;
mod chat_context;
mod clip_films;
pub(crate) mod controls;
/// The library browser a page shows: lanes, search, the card list (DECK's, CUT's lanes).
pub(crate) mod browser;
/// Decks and cuts that live in a folder (stage/content): opened, saved and
/// reloaded in place, and the song prompt of one that plays without its song.
pub(crate) mod folder_docs;
mod inspector;
mod looks_list;
mod media;
mod invariants;
pub(crate) mod motion_edit;
mod cut_structure;
/// The cut's own editor panel (`let cut_panel`): the whole film's controls.
mod cut_panel;
mod generate;
mod new_video;
// Generative segments: the GENERATE section and its jobs.
mod generate_segments;
pub(crate) mod ops;
pub(crate) mod open_doc;
pub(crate) mod patch;
mod preview;
mod project;
mod render;
mod rates;
mod render_ui;
use makepad_story_cut::source;
// The AI control API's host: a child of this page so it
// drives the page's own session and ops.
#[path = "../studio_api_host.rs"]
mod studio_api_host;
// The storyboard the cut is made from (SB1): a child of this page, like
// the host, so it reads the page's session, library and thumbnails.
#[path = "../storyboard/mod.rs"]
mod storyboard;
pub(crate) mod store;
mod timeline;
mod transitions;
mod voice;
/// WB1: the Web lane (grabs and streams).
mod web_lane;
/// Web export (FB1): the saved cut as a film for the browser (the chat's `export_web` card).
mod web_export_ui;
mod written;
pub(crate) use written::Place;
mod worker;

use super::{StoryApp, StoryPage};
use browser::PageLane;
use makepad_library_browser::drafts;
use makepad_library_browser::list::{CardKind, CutMediaList, ListRow, MediaListAction};
use makepad_story_cut::reframe::SafeZone;
use makepad_asset_data::{AssetKind, AssetRevisionRef};
use makepad_story_edit_render::{CutJob, CutPreview as RenderPreview, CutThumbnail, PreviewSource};
use makepad_story_cut::{dependencies, AssetRef, CutDoc, CutEval, CutNodeKind, MediaRef, RunRef, TakeInfo};
use makepad_widgets::*;
use media::{MediaJob, MediaReply, MediaWorker};
use ops::{free_track, overlap, CutOp, Lane, Media, MediaKind};
use preview::{CutPreview, PreviewAction};
use project::{preset_label, project, sketch, Projection};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use store::{RenderPurpose, SaveRequest, StoreJob, StoreReply};
use makepad_library_browser::lanes::{LibLane, LibRow, RenderState, Thumb};
use motion_edit::{StructureAction, StructureTree};
use timeline::{timecode_full as timecode, DropHint, CutTimeline, TimelineAction, TrackHeight};
use makepad_library_browser::draw::{Filmstrip, Peaks};
use open_doc::Session;
use worker::{Done, CutWorker, Job};
use render::{RenderEvent, RenderJob, RenderRunner};

/// Saved thumbnails: the long side, and the store's shortest side.
const THUMB_LONG: f64 = 512.0;
const THUMB_SHORT_MIN: f64 = 256.0;
/// A saved thumbnail's width for an output of `(width, height)`: the long side
/// THUMB_LONG, widened so the short side reaches the store's THUMB_SHORT_MIN
/// (a 21:9 output at 512 wide would be 216 high, which the store refuses),
/// the aspect kept. The renderer derives the height from the width.
fn thumb_width((ow, oh): (f64, f64)) -> u32 {
    if ow <= 0.0 || oh <= 0.0 {
        return THUMB_LONG as u32;
    }
    let mut scale = THUMB_LONG / ow.max(oh);
    if ow.min(oh) * scale < THUMB_SHORT_MIN {
        scale = THUMB_SHORT_MIN / ow.min(oh);
    }
    // round the width up: the height is rounded from it and must not fall below the minimum
    (ow * scale).ceil() as u32
}

/// The zoom slider's range, points per second, on a log scale.
const ZOOM_MIN: f64 = 4.0;
const ZOOM_MAX: f64 = 1200.0;
const MAX_SHUTTLE: f64 = 8.0;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    let Pane = RoundedView{width: Fill height: Fill flow: Down spacing: 0 padding: 0 show_bg: true
        draw_bg +: {color: stage.panel border_color: stage.separator border_size: 1 border_radius: 5}}
    let PaneHead = View{width: Fill height: 34 flow: Right spacing: 6 padding: Inset{left: 12 right: 8} align: Align{y: 0.5}}
    let Rule = SolidView{width: Fill height: 1 draw_bg +: {color: stage.separator}}
    let Faint = SolidView{width: Fill height: 1 draw_bg +: {color: stage.line_faint}}
    let Caps = StageTick{width: Fit}
    let SecCaps = Label{width: Fit text: "" draw_text +: {color: stage.secondary text_style: theme.font_bold{font_size: 7.5}}}
    let IconTool = mod.widgets.StageDocumentIconTool
    let Glyph = mod.widgets.StageDocumentIcon
    let Tool = mod.widgets.StageDocumentTool
    let Note = Label{width: Fill height: Fit text: "" draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 8}}}
    let Meta = mod.widgets.StageDocumentMeta
    // The sidebar lane and its row are the library browser's (`browser.rs`).
    let Lane = mod.widgets.StageLane
    let LaneCount = mod.widgets.StageLaneCount
    let LaneRow = mod.widgets.StageLaneRow
    let Field = View{width: Fill height: 26 flow: Right spacing: 0 align: Align{y: 0.5} padding: Inset{left: 12 right: 12}}
    let FieldName = Label{width: 84 text: "" draw_text +: {color: stage.label text_style: theme.font_regular{font_size: 9}}}
    let Section = StageButton{width: Fill height: 28 margin: 0 padding: Inset{left: 28} align: Align{x: 0.0 y: 0.5}
        draw_bg +: {color: stage.panel color_hover: stage.panel color_down: stage.panel border_size: 0.0}
        draw_text +: {color: stage.secondary color_hover: stage.label text_style: theme.font_bold{font_size: 7.5}}}
    let Chevron = Icon{icon_walk: Walk{width: 9 height: Fit} draw_icon +: {color: stage.dim}}
    // A folding section's header: its title and ONE open/closed chevron.
    let SecHead = View{width: Fill height: 28 flow: Overlay
        head := Section{}
        View{width: Fit height: Fill padding: Inset{left: 12} align: Align{y: 0.5}
            open := View{width: Fit height: Fit Chevron{draw_icon +: {svg: crate_resource("self:resources/icons/ui/chevron_down.svg")}}}
            closed := View{width: Fit height: Fit visible: false Chevron{draw_icon +: {svg: crate_resource("self:resources/icons/ui/page_next.svg")}}}
        }
    }
    // Key navigation: previous key, the diamond, next key.
    let KeyNav = View{width: 60 height: 22 flow: Right spacing: 0 align: Align{x: 1.0 y: 0.5}
        prev := ButtonIcon{width: 18 height: 22 padding: 0 icon_walk: Walk{width: 7 height: Fit}
            draw_bg +: {color: #0000 color_hover: stage.control_hover color_down: stage.control_down border_size: 0.0}
            draw_icon +: {color: stage.label svg: crate_resource("self:resources/icons/ui/page_prev.svg")}}
        key := Button{width: 22 height: 22 margin: 0 padding: 0 text: ""
            draw_bg +: {
                on: instance(0.0) keyed: instance(0.0)
                color: #0000 color_hover: stage.control_hover color_down: stage.control_down
                ink: uniform(stage.accent) soft: uniform(stage.accent_soft) rim: uniform(stage.faint)
                pixel: fn() {
                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                    let c = self.rect_size * 0.5
                    sdf.move_to(c.x, c.y - 5.) sdf.line_to(c.x + 5., c.y) sdf.line_to(c.x, c.y + 5.) sdf.line_to(c.x - 5., c.y) sdf.close_path()
                    let rim = self.rim.mix(self.soft, self.keyed).mix(self.ink, self.on)
                    sdf.fill_keep(vec4(self.ink.xyz, self.on))
                    sdf.stroke(rim, 1.2)
                    return sdf.result
                }
            }
        }
        next := ButtonIcon{width: 18 height: 22 padding: 0 icon_walk: Walk{width: 7 height: Fit}
            draw_bg +: {color: #0000 color_hover: stage.control_hover color_down: stage.control_down border_size: 0.0}
            draw_icon +: {color: stage.label svg: crate_resource("self:resources/icons/ui/page_next.svg")}}
    }
    let Param = View{width: Fill height: 26 flow: Right spacing: 0 align: Align{y: 0.5} padding: Inset{left: 12 right: 8}}
    let Text = TextInput{width: Fill height: 22 margin: 0 padding: Inset{left: 6 right: 6 top: 3 bottom: 3} empty_text: ""
        draw_text +: {text_style: theme.font_regular{font_size: 9}}}
    let Info = Label{width: Fill height: Fit text: "" draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 9}}}
    let Dot = mod.widgets.StageDocumentDot
    let KeyCap = RoundedView{width: Fit height: 18 padding: Inset{left: 6 right: 6} align: Align{y: 0.5} show_bg: true
        draw_bg +: {color: stage.raised border_radius: 3.0}
        label := Label{width: Fit text: "" draw_text +: {color: stage.label text_style: theme.font_bold{font_size: 7.5}}}}
    let SumRow = View{visible: false width: Fill height: 18 flow: Right align: Align{y: 0.5}
        key := Label{width: Fill text: "" draw_text +: {color: stage.label text_style: theme.font_regular{font_size: 9}}}
        value := Label{width: Fit margin: Inset{right: 4} text: "" draw_text +: {color: stage.text_bright text_style: theme.font_regular{font_size: 9}}}}
    let TagChip = RoundedView{visible: false width: Fit height: 16 padding: Inset{left: 6 right: 6} align: Align{y: 0.5} show_bg: true
        draw_bg +: {color: stage.raised border_radius: 3.0}
        label := Label{width: Fit text: "" draw_text +: {color: stage.label text_style: theme.font_regular{font_size: 7.5}}}}
    let RoleIcon = View{visible: false width: Fit height: Fit icon := Icon{icon_walk: Walk{width: 12 height: 12} draw_icon +: {color: stage.glass_ink}}}
    let MenuRow = StageButton{width: Fill height: 28 margin: 0 padding: Inset{left: 10 right: 10} align: Align{x: 0.0 y: 0.5}
        draw_bg +: {color: stage.menu color_hover: stage.control_hover border_size: 0.0}
        draw_text +: {text_style: theme.font_regular{font_size: 9}}}
    let Transport = StageIconButton{width: 28 height: 24 icon_walk: Walk{width: 11 height: Fit}}
    let Tc = Label{width: Fit text: "" draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 9}}}
    let TcName = Label{width: Fit text: "" draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 8}}}
    let Toggle = StageIconButton{width: 24 height: 22 icon_walk: Walk{width: 11 height: Fit}}

    mod.widgets.StageCutDesk = View{width: Fill height: Fill flow: Overlay
        View{width: Fill height: Fill flow: Down spacing: 8
            // The project bar (design §3.2).
            cut_project_bar := mod.widgets.StageDocumentBar{
                Glyph{draw_icon +: {svg: crate_resource("self:resources/icons/edit/timeline.svg")}}
                cut_no_edit := Meta{visible: false text: "No cut open"}
                cut_bar_doc := View{width: Fill height: Fill flow: Right spacing: 12 align: Align{y: 0.5}
                    cut_crumb := Tool{visible: false text: "" draw_bg +: {color: #0000 color_hover: stage.control_hover border_size: 0.0} draw_text +: {color: stage.muted}}
                    cut_title_tip := Tip{text: "The cut's name; Enter writes it, Esc reverts"
                        cut_title := TextInput{width: Fit height: 26 margin: 0 padding: Inset{left: 6 right: 6 top: 4 bottom: 4} empty_text: "Untitled"
                            draw_bg +: {color: stage.panel color_hover: stage.well color_focus: stage.well border_size: 0.0}
                            draw_text +: {color: stage.text_hi text_style: theme.font_bold{font_size: 10.5}}}}
                    // Unsaved: a dot (what and when on hover).
                    cut_save_tip := Tip{text: "" View{width: Fit height: 18 align: Align{y: 0.5}
                        cut_save_dot := Dot{}}}
                    // A graphic open: the film is the way back, then the
                    // graphic's name and its own unsaved dot (one bar).
                    cut_scene := View{visible: false width: Fit height: Fill flow: Right spacing: 6 align: Align{y: 0.5}
                        Tip{text: "Back to the film; the graphic is saved (Esc)" m_back := Tool{text: "" draw_bg +: {color: #0000 color_hover: stage.control_hover border_size: 0.0} draw_text +: {color: stage.muted}}}
                        Meta{text: "›"}
                        m_title := mod.widgets.StageDocumentName{}
                        m_state_tip := Tip{text: "" View{width: Fit height: 18 align: Align{y: 0.5}
                            m_dot := RoundedView{width: 6 height: 6 show_bg: true draw_bg +: {color: stage.amber border_radius: 3.0}}}}
                    }
                    View{width: 4 height: 1}
                    Tip{text: "The cut's output: size and frame rate; click for the project's settings"
                      View{width: Fit height: 18 flow: Overlay
                        mod.widgets.StageDocumentChip{
                            View{width: 10 height: 14 align: Align{x: 0.5 y: 0.5}
                                cut_aspect := RoundedView{width: 10 height: 6 show_bg: true draw_bg +: {color: #0000 border_color: stage.label border_size: 1.2 border_radius: 0.5}}}
                            cut_format := Meta{text: "" draw_text +: {color: stage.label}}
                            Meta{text: "·"}
                            cut_duration := Meta{text: ""}}
                        cut_format_btn := Button{width: Fill height: Fill margin: 0 text: "" draw_bg +: {color: #0000 color_hover: #ffffff10 color_down: #0000 color_focus: #0000 border_size: 0.0}}}}
                    // A graphic's state worth saying (a line that does not
                    // load), quietly, in the bar's free space.
                    m_status := Label{width: Fill max_lines: 1 text_overflow: Ellipsis padding: Inset{left: 8} text: "" draw_text +: {color: stage.warm text_style: theme.font_regular{font_size: 8}}}
                    Tip{text: "Timeline, or the cut's Splash source: two views of one document; Storyboard: the plan it is made from" cut_view_mode := CutSegmented{width: 250 labels: ["Timeline" "Source" "Storyboard"]}}
                    View{width: 8 height: 1}
                    Tip{text: "Undo (cmd-Z)" cut_undo := IconTool{icon_walk: Walk{width: 11 height: Fit} draw_icon +: {svg: crate_resource("self:resources/icons/ui/undo.svg")}}}
                    Tip{text: "Redo (shift-cmd-Z)" cut_redo := IconTool{icon_walk: Walk{width: 11 height: Fit} draw_icon +: {svg: crate_resource("self:resources/icons/edit/redo.svg")}}}
                    Tip{text: "Undo in the graphic (cmd-Z)" m_undo := IconTool{visible: false icon_walk: Walk{width: 11 height: Fit} draw_icon +: {svg: crate_resource("self:resources/icons/ui/undo.svg")}}}
                    Tip{text: "Redo in the graphic (shift-cmd-Z)" m_redo := IconTool{visible: false icon_walk: Walk{width: 11 height: Fit} draw_icon +: {svg: crate_resource("self:resources/icons/edit/redo.svg")}}}
                }
                cut_no_edit_fill := View{visible: false width: Fill height: 1}
                View{width: 4 height: 1}
                Tip{text: "A new empty cut: choose its format" cut_new := Tool{text: "New" draw_icon +: {svg: crate_resource("self:resources/icons/plus.svg")}}}
                cut_bar_save := View{width: Fit height: Fit flow: Right spacing: 8 align: Align{y: 0.5}
                    Tip{text: "Publish the cut to the library as a new revision (cmd-S)" cut_save := Tool{text: "Save" draw_icon +: {svg: crate_resource("self:resources/icons/ui/save.svg")}}}
                    cut_render_tip := Tip{text: "Render the saved cut to a video in the library (cmd-E)" cut_render := CutRenderButton{width: 200}}
                    cut_web_tip := Tip{text: "Export the saved cut as a film for the browser (the chat's Web export… sets its options)" cut_web_export := Tool{text: "Web export"}}}
            }
            cut_split_box := View{width: Fill height: Fill
            // The cut's own panel down the whole left side, its sidebar on the
            // right, the picture, library and timeline between (as the
            // graphic desk has its scene's); both side panes fold.
            cut_panel_split := mod.widgets.DockSplitter{width: Fill height: Fill axis: SplitterAxis.Horizontal align: SplitterAlign.FromA(440) size: 8
                fold: SplitterCollapse.A min_vertical: 300 max_vertical: 640

            a: Pane{
                // One column: the library, or the film's own AI editor panel.
                View{width: Fill height: 34 flow: Right spacing: 6 padding: Inset{left: 8 right: 8} align: Align{y: 0.5}
                    Tip{text: "Library: media, looks and makers to add to the cut. Editor: the film's own editor panel" cut_left_tabs := CutSegmented{width: 140 labels: ["Library" "Editor"]}}
                    View{width: Fill height: 1}
                    cut_lib_tools := mod.widgets.StageLibraryTools{height: 28}
                    }
                Rule{}
                cut_left_lib := View{width: Fill height: Fill flow: Down
                        View{width: Fill height: Fill flow: Right
                            cut_sidebar := View{width: 128 height: Fill flow: Down spacing: 2 padding: Inset{left: 4 right: 4 top: 6}
                                cut_lane_video := LaneRow{lane +: {text: "Video" draw_icon +: {svg: crate_resource("self:resources/icons/video.svg")}}}
                                cut_lane_image := LaneRow{lane +: {text: "Images" draw_icon +: {svg: crate_resource("self:resources/icons/image.svg")}}}
                                cut_lane_audio := LaneRow{lane +: {text: "Audio" draw_icon +: {svg: crate_resource("self:resources/icons/audio.svg")}}}
                                cut_lane_scores := LaneRow{lane +: {text: "Scores" draw_icon +: {svg: crate_resource("self:resources/icons/ui/note.svg")}}}
                                cut_lane_runs := LaneRow{lane +: {text: "App runs" draw_icon +: {svg: crate_resource("self:resources/icons/edit/app_run.svg")}}}
                                cut_lane_edits := LaneRow{lane +: {text: "Cuts" draw_icon +: {svg: crate_resource("self:resources/icons/edit/timeline.svg")}}}
                                cut_lane_decks := LaneRow{lane +: {text: "Decks" draw_icon +: {svg: crate_resource("self:resources/icons/output.svg")}}}
                                cut_lane_web := LaneRow{lane +: {text: "Web" draw_icon +: {svg: crate_resource("self:resources/icons/http.svg")}}}
                                View{width: Fill height: 12}
                                View{width: Fill height: 20 padding: Inset{left: 10} align: Align{y: 0.5} cut_caps_looks := SecCaps{text: "LOOKS"}}
                                cut_lane_fx := LaneRow{lane +: {text: "Effects" draw_icon +: {svg: crate_resource("self:resources/icons/ui/sparkle.svg")}}}
                                cut_lane_trans := LaneRow{lane +: {text: "Transitions" draw_icon +: {svg: crate_resource("self:resources/icons/ui/xfade.svg")}}}
                            }
                            SolidView{width: 1 height: Fill draw_bg +: {color: stage.separator}}
                            cut_lib_column := View{width: Fill height: Fill flow: Down
                                lib_head := mod.widgets.StageLibraryHead{}
                                // The Web lane's status line and Stop (WB1): shown only in that lane.
                                cut_web_bar := View{visible: false width: Fill height: 24 flow: Right spacing: 6 align: Align{y: 0.5} padding: Inset{left: 8 right: 8 top: 2 bottom: 4}
                                    cut_web_note := Label{width: Fill max_lines: 1 text: "" draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 8}}}
                                    Tip{text: "Stop the selected grab (a stream is kept as a clip)" cut_web_stop := Tool{text: "Stop"}}}
                                // The grid over the INFO panel of the selected item (design §4.8).
                                cut_lib_split := Splitter{width: Fill height: Fill axis: SplitterAxis.Vertical align: SplitterAlign.FromB(300) size: 8
                                    collapse: SplitterCollapse.B min_horizontal: 120 max_horizontal: 28
                                    draw_bg +: {color_bg: stage.panel color: stage.border_hi color_hover: stage.accent color_drag: stage.accent splitter_pad: 3 bar_size: 24}
                                    a: View{width: Fill height: Fill flow: Overlay
                                        lib_list := CutMediaList{}
                                        // Effects and transitions: the effect pickers'
                                        // own tiles, with their rendered previews.
                                        cut_fx_picker := EffectPicker{width: Fill height: Fill visible: false managed: true category: "" target: "edit.effect"}
                                    }
                                    b: View{width: Fill height: Fill cut_lib_detail := View{width: Fill height: Fill flow: Down
                                        Faint{}
                                        cut_info_head := Button{width: Fill height: 28 margin: 0 padding: Inset{left: 8 right: 8} align: Align{x: 0.0 y: 0.5} text: ""
                                            draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 8}}
                                            draw_bg +: {color: stage.panel color_hover: stage.control_hover color_down: stage.panel color_focus: stage.panel border_size: 0.0}}
                                        View{width: Fill height: 0 flow: Overlay}
                                        cut_info_body := ScrollYView{width: Fill height: Fill flow: Down spacing: 6 padding: Inset{left: 8 right: 8 bottom: 8}
                                            cut_info_top := View{width: Fill height: Fit flow: Down spacing: 6
                                                cut_lib_well := View{width: Fill height: 150 flow: Overlay align: Align{x: 0.5 y: 0.5}
                                                    cut_lib_preview := CutAssetPreview{height: Fill}
                                                    cut_lib_video_box := View{visible: false width: Fill height: Fill align: Align{x: 0.5 y: 0.5}
                                                        cut_lib_video := Video{width: 160 height: 90 is_looping: false}}}
                                                cut_info_text := View{width: Fill height: Fit flow: Down spacing: 6
                                                Tip{width: Fill height: Fit text: "" cut_lib_name_tip := View{width: Fill height: Fit
                                                    cut_lib_name := Label{width: Fill max_lines: 2 text: "" draw_text +: {color: stage.text_hi text_style: theme.font_bold{font_size: 9}}}}}
                                                View{width: Fill height: Fit flow: Right
                                                    cut_lib_keys := Label{width: 76 text: "" draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 8} line_spacing: 1.6}}
                                                    cut_lib_facts := Label{width: Fill text: "" draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 8} line_spacing: 1.6}}}}}
                                            cut_info_rev := View{width: Fill height: 18 flow: Right spacing: 6 align: Align{y: 0.5}
                                                Label{width: 76 text: "Revision" draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 8}}}
                                                cut_lib_rev := Label{width: Fit text: "" draw_text +: {color: stage.dim text_style: theme.font_regular{font_size: 8}}}
                                                Tip{text: "Copy the revision id" cut_lib_copy := IconTool{width: 18 height: 16 icon_walk: Walk{width: 10 height: Fit}
                                                    draw_bg +: {color: #0000 border_size: 0.0} draw_icon +: {color: stage.faint svg: crate_resource("self:resources/icons/list.svg")}}}}
                                            cut_lib_tags := View{width: Fill height: Fit flow: Flow.Right{wrap: true} spacing: 4
                                                cut_tag_0 := TagChip{} cut_tag_1 := TagChip{} cut_tag_2 := TagChip{} cut_tag_3 := TagChip{} cut_tag_4 := TagChip{} cut_tag_5 := TagChip{}}
                                        }
                                        // The actions, pinned under the scrolling facts (review 3, C1).
                                        cut_info_actions := View{width: Fill height: 31 flow: Down
                                            Faint{}
                                            View{width: Fill height: 30 flow: Right spacing: 8 padding: Inset{left: 8 right: 8 top: 4 bottom: 4}
                                                    Tip{text: "Add it at the playhead on the track that fits (E)" cut_lib_add := Tool{text: "Add at playhead" draw_icon +: {svg: crate_resource("self:resources/icons/plus.svg")}}}
                                                    Tip{text: "Open this cut (double-click the card)" cut_lib_open := Tool{visible: false text: "Open"
                                                        draw_bg +: {color: stage.accent_wash color_hover: stage.accent_tint color_focus: stage.accent_wash} draw_text +: {color: stage.text_hi}}}
                                                    Tip{text: "Select where the cut uses it" cut_lib_show := Tool{visible: false text: "Show in timeline"}}}
                                        }
                                    }}
                                }
                                Faint{}
                                View{width: Fill height: 24 padding: Inset{left: 8 right: 8} align: Align{y: 0.5}
                                    cut_lib_status := Label{width: Fill max_lines: 1 flow: Flow.Right{wrap: false} text: "" draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 8}}}}
                            }
                        }
                }
                cut_left_editor := View{visible: false width: Fill height: Fill cut_panel_col := mod.widgets.CutPanelColumn{}}
            }
            b: View{width: Fill height: Fill cut_side_split := mod.widgets.DockSplitter{width: Fill height: Fill axis: SplitterAxis.Horizontal align: SplitterAlign.FromB(360) size: 8
                fold: SplitterCollapse.B min_vertical: 600 max_vertical: 260

            a: View{width: Fill height: Fill
            cut_split := mod.widgets.DockSplitter{width: Fill height: Fill axis: SplitterAxis.Vertical align: SplitterAlign.FromB(404) size: 8
                min_horizontal: 320 max_horizontal: 220

                // The viewer: picture and transport.
                        a: Pane{
                            View{width: Fill height: Fill padding: 1
                                cut_preview := CutPreview{}}
                            Rule{}
                            View{width: Fill height: 44 flow: Right spacing: 6 padding: Inset{left: 12 right: 10} align: Align{y: 0.5}
                                // The marks, narrowing with the viewer (review 2, B3).
                                cut_readouts := View{width: Fit height: Fill flow: Right spacing: 6 align: Align{y: 0.5}
                                    cut_ro_in := View{width: Fit height: Fit flow: Right spacing: 6 TcName{text: "IN"} cut_mark_in := Tc{}}
                                    cut_ro_out := View{width: Fit height: Fit flow: Right spacing: 6 margin: Inset{left: 6} TcName{text: "OUT"} cut_mark_out := Tc{}}
                                    cut_ro_dur := View{width: Fit height: Fit flow: Right spacing: 6 margin: Inset{left: 6} TcName{text: "DUR"} cut_mark_dur := Tc{}}
                                    cut_ro_compact := View{visible: false width: Fit height: Fit flow: Right spacing: 4 align: Align{y: 0.5}
                                        Icon{icon_walk: Walk{width: 10 height: Fit} draw_icon +: {color: stage.muted svg: crate_resource("self:resources/icons/edit/mark_in.svg")}}
                                        cut_ro_compact_text := Tc{}
                                        Icon{icon_walk: Walk{width: 10 height: Fit} draw_icon +: {color: stage.muted svg: crate_resource("self:resources/icons/edit/mark_out.svg")}}}}
                                View{width: Fill height: Fill flow: Right spacing: 4 align: Align{x: 0.5 y: 0.5}
                                    Tip{text: "To the start (Home)" cut_to_start := Transport{draw_icon +: {svg: crate_resource("self:resources/icons/ui/skip_start.svg")}}}
                                    Tip{text: "Play backwards; again for faster (J)" cut_back := Transport{draw_icon +: {svg: crate_resource("self:resources/icons/ui/rewind.svg")}}}
                                    Tip{text: "Play (space)" cut_play := Transport{width: 32 height: 28 icon_walk: Walk{width: 14 height: Fit} draw_icon +: {svg: crate_resource("self:resources/icons/ui/play.svg")}}}
                                    Tip{text: "Pause (space, K)" cut_pause := Transport{visible: false width: 32 height: 28 icon_walk: Walk{width: 14 height: Fit}
                                        draw_bg +: {color: stage.accent color_focus: stage.accent color_hover: stage.accent_hover color_down: stage.accent_down border_color: stage.accent}
                                        draw_icon +: {color: stage.on_accent svg: crate_resource("self:resources/icons/ui/pause.svg")}}}
                                    Tip{text: "Play forwards; again for faster (L)" cut_fwd := Transport{draw_icon +: {svg: crate_resource("self:resources/icons/ui/fast_forward.svg")}}}
                                    Tip{text: "To the end (End)" cut_to_end := Transport{draw_icon +: {svg: crate_resource("self:resources/icons/edit/skip_end.svg")}}}
                                    View{width: 16 height: 1}
                                    View{width: 156 height: 30 flow: Overlay
                                        Tip{text: "Click to type a time: 01:12 is 1 s 12 f, +30 is 30 frames on; Enter seeks"
                                            cut_time := Button{width: 156 height: 30 margin: 0 padding: Inset{left: 4} align: Align{x: 0.0 y: 0.5} text: "00:00:00:00"
                                                draw_bg +: {color: #0000 color_focus: #0000 color_hover: stage.well color_down: stage.well border_size: 0.0}
                                                draw_text +: {color: stage.text_max color_hover: stage.text_max color_focus: stage.text_max text_style: theme.font_bold{font_size: 15}}}}
                                        cut_time_box := View{visible: false width: Fill height: Fill
                                            cut_time_input := TextInput{width: Fill height: 30 margin: 0 padding: Inset{left: 4 right: 4 top: 4 bottom: 4}
                                                draw_text +: {text_style: theme.font_bold{font_size: 15}}}}}
                                }
                                View{width: Fit height: Fill flow: Right spacing: 6 align: Align{y: 0.5}
                                    cut_shuttle := RoundedView{visible: false width: Fit height: 18 padding: Inset{left: 6 right: 6} align: Align{y: 0.5} show_bg: true
                                        draw_bg +: {color: stage.accent_wash border_radius: 3.0}
                                        cut_shuttle_text := Label{width: Fit text: "" draw_text +: {color: stage.text_hi text_style: theme.font_bold{font_size: 7.5}}}}
                                    Tip{text: "Safe zones (')" cut_safe := Toggle{draw_icon +: {svg: crate_resource("self:resources/icons/edit/safe_zone.svg")}}}
                                    Tip{text: "Loop the in/out range" cut_loop := Toggle{draw_icon +: {svg: crate_resource("self:resources/icons/ui/loop.svg")}}}
                                    Tip{text: "The cut's sound level at the playhead, from its sources, while playing" cut_meter := CutMeter{}}
                                }
                            }
                        }
                // The timeline: toolbar, then the canvas.
                b: Pane{
                    cut_tl_bar := View{width: Fill height: 36 flow: Right spacing: 6 padding: Inset{left: 12 right: 8} align: Align{y: 0.5}
                        Caps{text: "TIMELINE"}
                        View{width: 6 height: 1}
                        Tip{text: "Add the chosen library item at the playhead (E)" cut_add := Tool{text: "Add" draw_icon +: {svg: crate_resource("self:resources/icons/plus.svg")}}}
                        Tip{text: "Cut the clip under the playhead in two (B)" cut_blade := Tool{text: "Blade" draw_icon +: {svg: crate_resource("self:resources/icons/edit/blade.svg")}}}
                        Tip{text: "A title at the playhead (T)" cut_title_add := Tool{text: "Title" draw_icon +: {svg: crate_resource("self:resources/icons/edit/title.svg")}}}
                        Tip{text: "A voiceover line at the playhead (V)" cut_voice_add := Tool{text: "Voiceover" draw_icon +: {svg: crate_resource("self:resources/icons/ui/mic.svg")}}}
                        SolidView{width: 1 height: 18 draw_bg +: {color: stage.separator}}
                        Tip{text: "A marker at the playhead (M)" cut_marker := Toggle{icon_walk: Walk{width: 10 height: Fit} draw_icon +: {svg: crate_resource("self:resources/icons/edit/marker.svg")}}}
                        Tip{text: "Mark in at the playhead (I; alt-I clears)" cut_mark_in_btn := Toggle{draw_icon +: {svg: crate_resource("self:resources/icons/edit/mark_in.svg")}}}
                        Tip{text: "Mark out at the playhead (O; alt-O clears)" cut_mark_out_btn := Toggle{draw_icon +: {svg: crate_resource("self:resources/icons/edit/mark_out.svg")}}}
                        View{width: 8 height: 1}
                        cut_status := Label{width: Fill max_lines: 1 flow: Flow.Right{wrap: false} text: "" draw_text +: {color: stage.secondary text_style: theme.font_regular{font_size: 8}}}
                        Tip{text: "Snapping (N; hold cmd to invert while dragging)" cut_snap := Toggle{draw_icon +: {svg: crate_resource("self:resources/icons/edit/magnet.svg")}}}
                        Tip{text: "Skimming: the viewer follows the pointer over the tracks (S)" cut_skim := Toggle{draw_icon +: {svg: crate_resource("self:resources/icons/edit/eye.svg")}}}
                        Tip{text: "Track height: small, medium, large" cut_height := Toggle{draw_icon +: {svg: crate_resource("self:resources/icons/edit/track_height.svg")}}}
                        View{width: 8 height: 1}
                        Tip{text: "Zoom out (−)" cut_zoom_out := Toggle{draw_icon +: {svg: crate_resource("self:resources/icons/edit/zoom_out.svg")}}}
                        cut_zoom := Slider{width: 120 height: 20 text: "" min: 0 max: 1 default: 0.3 margin: 0 padding: 0
                            label_walk: Walk{width: 0 height: 0 margin: 0}
                            text_input: TextInput{width: 0 height: 0}
                            draw_bg +: {
                                track: uniform(stage.groove) fill: uniform(stage.accent) knob: uniform(stage.text_hi)
                                pixel: fn() {
                                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                                    let w = self.rect_size.x
                                    let y = self.rect_size.y * 0.5
                                    sdf.box(6., y - 1.5, w - 12., 3., 1.5)
                                    sdf.fill(self.track)
                                    let x = 6. + self.slide_pos * (w - 12.)
                                    sdf.box(6., y - 1.5, x - 6., 3., 1.5)
                                    sdf.fill(self.fill)
                                    sdf.circle(x, y, 5.)
                                    sdf.fill(self.knob)
                                    return sdf.result
                                }
                            }}
                        Tip{text: "Zoom in (+)" cut_zoom_in := Toggle{draw_icon +: {svg: crate_resource("self:resources/icons/edit/zoom_in.svg")}}}
                        Tip{text: "Show the whole cut (shift-Z)" cut_zoom_fit := Toggle{draw_icon +: {svg: crate_resource("self:resources/icons/fit.svg")}}}
                    }
                    Rule{}
                    cut_timeline_box := View{width: Fill height: Fill cut_timeline := CutTimeline{}}
                    cut_source_box := View{visible: false width: Fill height: Fill flow: Right spacing: 4
                        cut_files_box := ScrollYView{width: 240 height: Fill flow: Down cut_files := StructureTree{}}
                        cut_source_pane := mod.widgets.DocSourcePane{}
                        cut_file_binary := Label{visible: false width: Fill height: Fit}
                    }
                    cut_board_pane := mod.widgets.StoryboardPane{}
                }
            }}
            // The sidebar, as the graphic desk's: the selection's inspector or the
            // edit's structure (cut_structure.rs).
            b: View{width: Fill height: Fill
                // What the model is making for a video asked for in the chat:
                // its transcript, with Stop and Clear.
                gen_run := mod.widgets.CutGenerateRun{}
                Pane{
                    View{width: Fill height: 30 flow: Right spacing: 2 padding: Inset{left: 8 right: 6} align: Align{y: 0.5}
                        Tip{text: "Cut: what is selected (the cut's own settings with nothing selected). Structure: the cut as a tree" cut_side_tabs := CutSegmented{width: 150 labels: ["Cut" "Structure"]}}}
                    Rule{}
                    cut_side_edit := View{width: Fill height: Fill flow: Down
                            View{width: Fill height: 44 flow: Right spacing: 8 padding: Inset{left: 12 right: 8} align: Align{y: 0.5}
                                cut_role_chip := RoundedView{width: 18 height: 18 flow: Overlay align: Align{x: 0.5 y: 0.5} show_bg: true
                                    draw_bg +: {color: stage.raised border_radius: 3.0}
                                    cut_role_video := RoleIcon{icon +: {draw_icon +: {svg: crate_resource("self:resources/icons/video.svg")}}}
                                    cut_role_image := RoleIcon{icon +: {draw_icon +: {svg: crate_resource("self:resources/icons/image.svg")}}}
                                    cut_role_app := RoleIcon{icon +: {draw_icon +: {svg: crate_resource("self:resources/icons/edit/app_run.svg")}}}
                                    cut_role_title := RoleIcon{icon +: {draw_icon +: {svg: crate_resource("self:resources/icons/edit/title.svg")}}}
                                    cut_role_fx := RoleIcon{icon +: {draw_icon +: {svg: crate_resource("self:resources/icons/ui/sparkle.svg")}}}
                                    cut_role_trans := RoleIcon{icon +: {draw_icon +: {svg: crate_resource("self:resources/icons/ui/xfade.svg")}}}
                                    cut_role_audio := RoleIcon{icon +: {draw_icon +: {svg: crate_resource("self:resources/icons/audio.svg")}}}
                                    cut_role_voice := RoleIcon{icon +: {draw_icon +: {svg: crate_resource("self:resources/icons/ui/mic.svg")}}}
                                    cut_role_caps := RoleIcon{icon +: {draw_icon +: {svg: crate_resource("self:resources/icons/edit/subtitles.svg")}}}
                                    cut_role_project := RoleIcon{icon +: {draw_icon +: {svg: crate_resource("self:resources/icons/edit/timeline.svg")}}}
                                }
                                cut_node_tip := Tip{width: Fill height: 30 text: ""
                                    View{width: Fill height: 30 flow: Down spacing: 2
                                        cut_node := Label{width: Fill height: 15 margin: 0 padding: 0 max_lines: 1 text: "" draw_text +: {color: stage.text_hi text_style: theme.font_bold{font_size: 9.5}}}
                                        cut_node_kind := Label{width: Fill height: 13 margin: 0 padding: 0 max_lines: 1 text: "" draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 8}}}}}
                                Tip{text: "Remove the selection (delete)" cut_delete := IconTool{draw_bg +: {color: #0000 border_size: 0.0} draw_icon +: {color: stage.faint svg: crate_resource("self:resources/icons/ui/trash.svg")}}}
                            }
                            Rule{}
                            cut_sections := ScrollYView{width: Fill height: Fill flow: Down
                                cut_s_generate := mod.widgets.CutGenerateSection{}
                                cut_s_project := View{width: Fill height: Fit flow: Down
                                    cut_h_project := SecHead{head +: {text: "CUT"}}
                                    cut_b_project := View{width: Fill height: Fit flow: Down padding: Inset{bottom: 6}
                                        Field{FieldName{text: "Format"} View{width: Fill height: Fit align: Align{x: 1.0} padding: Inset{right: 8} cut_p_format := Info{width: Fit}}}
                                        Field{FieldName{text: "Frame rate"} View{width: Fill height: Fit flow: Right spacing: 6 align: Align{y: 0.5}
                                            Tip{width: Fill text: "The cut's frame rate: everything is drawn at it (a video clip shows its nearest frame)" cut_p_fps := DropDown{width: Fill height: 22 margin: 0 labels: ["23.976 fps" "24 fps" "25 fps" "29.97 fps" "30 fps" "48 fps" "50 fps" "59.94 fps" "60 fps" "90 fps" "120 fps" "144 fps" "Custom…"]
                                                draw_text +: {text_style: theme.font_regular{font_size: 9}}}}
                                            cut_p_fps_box := View{visible: false width: Fit height: Fit cut_p_fps_custom := Text{width: 64 height: 22 empty_text: "fps"}}}}
                                        Field{FieldName{text: "Duration"} cut_p_duration := CutScrub{}}
                                        Field{FieldName{text: "Tempo"} cut_p_bpm := CutScrub{}}
                                        Field{FieldName{text: "Safe zone"} cut_p_safe := CutSegmented{labels: ["None" "Reels" "TikTok" "Shorts"]}}
                                    }
                                    Faint{}
                                }
                                cut_s_summary := View{width: Fill height: Fit flow: Down
                                    cut_h_summary := SecHead{head +: {text: "SUMMARY"}}
                                    cut_b_summary := View{width: Fill height: Fit flow: Down spacing: 4 padding: Inset{left: 12 right: 12 bottom: 8}
                                        cut_summary := Info{visible: false}
                                        cut_sum_0 := SumRow{}
                                        cut_sum_1 := SumRow{}
                                        cut_sum_2 := SumRow{}
                                        cut_sum_3 := SumRow{}
                                        cut_sum_4 := SumRow{}
                                        cut_sum_5 := SumRow{}
                                        cut_sum_6 := SumRow{}
                                        cut_sum_7 := SumRow{}
                                        cut_sum_8 := SumRow{}
                                        cut_sum_9 := SumRow{}
                                        cut_sum_10 := SumRow{}
                                        cut_sum_11 := SumRow{}
                                        cut_sum_12 := SumRow{}
                                        cut_summary_stale := Tool{visible: false text: "" draw_bg +: {color: stage.panel color_hover: stage.control_hover border_size: 0.0} draw_text +: {color: stage.warm color_hover: stage.warm}}
                                    }
                                    Faint{}
                                }
                                cut_s_voice := View{width: Fill height: Fit flow: Down
                                    cut_h_voice := SecHead{head +: {text: "LINE"}}
                                    cut_b_voice := View{width: Fill height: Fit flow: Down spacing: 6 padding: Inset{left: 12 right: 12 bottom: 8}
                                        Tip{text: "The line's words: its take and its subtitles follow them. Enter writes." cut_line_text := Text{height: 48 empty_text: "What the voice says"}}
                                        Note{text: "Enter writes · the take and the captions follow the words"}
                                        View{width: Fill height: 26 flow: Right align: Align{y: 0.5}
                                            FieldName{text: "Starts at"} cut_line_at := CutScrub{}}
                                    }
                                    Faint{}
                                }
                                cut_s_speak := View{width: Fill height: Fit flow: Down
                                    cut_h_speak := SecHead{head +: {text: "VOICE"}}
                                    cut_b_speak := View{width: Fill height: Fit flow: Down spacing: 6 padding: Inset{left: 12 right: 12 bottom: 8}
                                        View{width: Fill height: Fit flow: Right spacing: 6 align: Align{y: 0.5}
                                            cut_voice_dot := Dot{}
                                            cut_voice_state := Label{width: Fill height: Fit text: "" draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 9}}}}
                                        View{width: Fill height: Fit flow: Right spacing: 6
                                            Tip{text: "A new line at the playhead (V)" cut_line_add := Tool{text: "Add line" draw_icon +: {svg: crate_resource("self:resources/icons/plus.svg")}}}
                                        }
                                    }
                                    Faint{}
                                }
                                cut_s_timing := View{width: Fill height: Fit flow: Down
                                    cut_h_timing := SecHead{head +: {text: "TIMING"}}
                                    cut_b_timing := View{width: Fill height: Fit flow: Down padding: Inset{bottom: 6}
                                        Field{FieldName{text: "Start"} cut_start := CutScrub{}}
                                        Field{FieldName{text: "Duration"} cut_length := CutScrub{}}
                                        cut_in_row := Field{FieldName{text: "Source in"} cut_in := CutScrub{}}
                                        cut_track_row := Field{FieldName{text: "Track"} cut_track := CutScrub{}}
                                    }
                                    Faint{}
                                }
                                cut_s_look := View{width: Fill height: Fit flow: Down
                                    cut_h_look := SecHead{head +: {text: "LOOK"}}
                                    cut_b_look := View{width: Fill height: Fit flow: Down padding: Inset{bottom: 6}
                                        cut_value_row := Field{cut_value_name := FieldName{text: "Preset"} cut_value := Text{}}
                                        cut_style_row := Field{FieldName{text: "Style"} cut_style := CutSegmented{labels: ["Movie" "Karaoke"]}}
                                        cut_fit_row := Field{FieldName{text: "Fit"} cut_fit := CutSegmented{labels: ["Fill" "Fit" "None"]}}
                                        cut_nsubs_row := Field{FieldName{text: "Subtitles"} Tip{width: Fill text: "Show the nested cut's own captions here, kept in this cut's safe zone" cut_nsubs := CutSwitch{text: "Show its captions"}}}
                                        cut_r0 := Param{cut_n0 := FieldName{} cut_v0 := CutScrub{} cut_k0 := KeyNav{}}
                                        cut_r1 := Param{cut_n1 := FieldName{} cut_v1 := CutScrub{} cut_k1 := KeyNav{}}
                                        cut_r2 := Param{cut_n2 := FieldName{} cut_v2 := CutScrub{} cut_k2 := KeyNav{}}
                                        cut_r3 := Param{cut_n3 := FieldName{} cut_v3 := CutScrub{} cut_k3 := KeyNav{}}
                                    }
                                    Faint{}
                                }
                                cut_s_audio := View{width: Fill height: Fit flow: Down
                                    cut_h_audio := SecHead{head +: {text: "AUDIO"}}
                                    cut_b_audio := View{width: Fill height: Fit flow: Down padding: Inset{bottom: 6}
                                        cut_r4 := Param{cut_n4 := FieldName{} cut_v4 := CutScrub{} cut_k4 := KeyNav{}}
                                        cut_r5 := Param{cut_n5 := FieldName{} cut_v5 := CutScrub{} cut_k5 := KeyNav{}}
                                        cut_fade_rows := View{width: Fill height: Fit flow: Down
                                            Field{FieldName{text: "Fade in"} cut_fade_in := CutScrub{}}
                                            Field{FieldName{text: "Fade out"} cut_fade_out := CutScrub{}}}
                                        cut_duck_row := Field{FieldName{text: "Ducking"} cut_duck := CutSwitch{text: "Duck under voiceover"}}
                                        cut_depth_row := Field{FieldName{text: "Depth"} cut_depth := CutScrub{}}
                                    }
                                    Faint{}
                                }
                                cut_s_keys := View{width: Fill height: Fit flow: Down
                                    cut_h_keys := SecHead{head +: {text: "KEYFRAMES"}}
                                    cut_b_keys := View{width: Fill height: Fit padding: Inset{left: 12 right: 12 bottom: 8}
                                        cut_keys := Note{}}
                                    Faint{}
                                }
                                cut_s_source := View{width: Fill height: Fit flow: Down
                                    cut_h_source := SecHead{head +: {text: "SOURCE"}}
                                    cut_b_source := View{width: Fill height: Fit flow: Down spacing: 4 padding: Inset{left: 12 right: 12 bottom: 8}
                                        cut_src_title := Info{}
                                        cut_src_detail := Note{}
                                        Tip{text: "Show this media in the library" cut_reveal := Tool{text: "Reveal in library"}}
                                    }
                                }
                            }
                    }
                    cut_side_tree := View{visible: false width: Fill height: Fill ScrollYView{width: Fill height: Fill flow: Down cut_tree := StructureTree{}}}
                }
            }
            }}
            }
            }
            cut_motion := mod.widgets.MotionDesk{}
        }
        // No cut open (design §4.6).
        cut_blank := View{visible: false width: Fill height: Fill padding: Inset{top: 48 left: 448}
          SolidView{width: Fill height: Fill cursor: MouseCursor.Arrow align: Align{x: 0.5 y: 0.4} draw_bg +: {color: stage.bg}
            RoundedView{width: 560 height: Fit flow: Down spacing: 16 padding: 24 show_bg: true
                draw_bg +: {color: stage.panel border_color: stage.separator border_size: 1.0 border_radius: 8.0}
                cut_blank_error := Label{visible: false width: Fill text: "" draw_text +: {color: stage.danger text_style: theme.font_regular{font_size: 9}}}
                Label{width: Fit text: "New cut" draw_text +: {color: stage.text_hi text_style: theme.font_bold{font_size: 11}}}
                View{width: Fill height: Fit flow: Right spacing: 12
                    cut_blank_wide := CutFormatTile{width: 112 height: 112 outline: 56.0 aspect: vec2(16. 9.) label: "16:9" detail: "1920×1080"}
                    cut_blank_tall := CutFormatTile{width: 112 height: 112 outline: 56.0 aspect: vec2(9. 16.) label: "9:16" detail: "1080×1920"}
                    cut_blank_square := CutFormatTile{width: 112 height: 112 outline: 56.0 aspect: vec2(1. 1.) label: "1:1" detail: "1080×1080"}
                    cut_blank_portrait := CutFormatTile{width: 112 height: 112 outline: 56.0 aspect: vec2(4. 5.) label: "4:5" detail: "1080×1350"}
                }
                Faint{}
                View{width: Fill height: Fit flow: Right spacing: 8 align: Align{y: 0.5}
                    SecCaps{text: "RECENT CUTS"}
                    View{width: Fill height: 1}
                    Tip{text: "Browse the Cuts lane of the library" cut_blank_open := Tool{text: "Open from library…"}}}
                cut_recent_box := View{width: Fill height: 196
                    cut_recent := CutMediaList{draw_bg +: {color: stage.panel}}}
                cut_recent_none := Note{text: "No cuts in the library yet"}
            }
          }
        }
        // New ▾: the four formats.
        cut_new_menu := View{visible: false width: Fill height: Fill flow: Overlay
          // A click anywhere outside the rows closes it.
          cut_new_scrim := Button{width: Fill height: Fill margin: 0 text: "" draw_bg +: {color: #0000 color_hover: #0000 color_down: #0000 color_focus: #0000 border_size: 0.0}}
          cut_new_box := View{width: Fill height: Fill align: Align{x: 1.0 y: 0.0} padding: Inset{top: 36 right: 232}
            RoundedView{width: 230 height: Fit flow: Down padding: 4 show_bg: true
                draw_bg +: {color: stage.menu border_color: stage.border_hi border_size: 1.0 border_radius: 4.0}
                cut_new_wide := View{width: Fill height: 28 flow: Overlay row := MenuRow{padding: Inset{left: 32 right: 10} text: "16:9 · 1920×1080"} View{width: 32 height: Fill align: Align{x: 0.5 y: 0.5} RoundedView{width: 12 height: 7 show_bg: true draw_bg +: {color: #0000 border_color: stage.label border_size: 1.2 border_radius: 0.5}}}}
                cut_new_tall := View{width: Fill height: 28 flow: Overlay row := MenuRow{padding: Inset{left: 32 right: 10} text: "9:16 · 1080×1920"} View{width: 32 height: Fill align: Align{x: 0.5 y: 0.5} RoundedView{width: 7 height: 12 show_bg: true draw_bg +: {color: #0000 border_color: stage.label border_size: 1.2 border_radius: 0.5}}}}
                cut_new_square := View{width: Fill height: 28 flow: Overlay row := MenuRow{padding: Inset{left: 32 right: 10} text: "1:1 · 1080×1080"} View{width: 32 height: Fill align: Align{x: 0.5 y: 0.5} RoundedView{width: 10 height: 10 show_bg: true draw_bg +: {color: #0000 border_color: stage.label border_size: 1.2 border_radius: 0.5}}}}
                cut_new_portrait := View{width: Fill height: 28 flow: Overlay row := MenuRow{padding: Inset{left: 32 right: 10} text: "4:5 · 1080×1350"} View{width: 32 height: Fill align: Align{x: 0.5 y: 0.5} RoundedView{width: 9 height: 11 show_bg: true draw_bg +: {color: #0000 border_color: stage.label border_size: 1.2 border_radius: 0.5}}}}
            }
          }
        }
        // A folder cut playing without its song (folder_docs.rs): where to get it.
        cut_song := View{visible: false width: Fill height: Fit align: Align{x: 0.5} padding: Inset{top: 52}
            RoundedView{width: 620 height: Fit flow: Down spacing: 8 padding: 16 show_bg: true
                draw_bg +: {color: stage.panel border_color: stage.border_hi border_size: 1.0 border_radius: 6.0}
                cut_song_title := Label{width: Fill height: Fit text: "" draw_text +: {color: stage.text_hi text_style: theme.font_bold{font_size: 10}}}
                cut_song_credits := Label{width: Fill height: Fit text: "" draw_text +: {color: stage.label text_style: theme.font_regular{font_size: 8}}}
                cut_song_state := Note{}
                View{width: Fill height: Fit flow: Right spacing: 8
                    Tip{text: "Fetch the song from its pinned copy, checked against its size and SHA-256 before it is kept (the song is its authors'; see the credits)" cut_song_download := Tool{text: "Download the song"}}
                    Tip{text: "Stop the download; Download resumes it" cut_song_stop := Tool{text: "Stop"}}
                    Tip{text: "Use a copy of the song you have (MP3 or WAV)" cut_song_pick := Tool{text: "Pick a file…"}}
                    View{width: Fill height: 1}
                    Tip{text: "Play it without the song" cut_song_dismiss := Tool{text: "Not now"}}
                }
            }
        }
        // The shortcut sheet (?).
        cut_keys_sheet := View{visible: false width: Fill height: Fill flow: Overlay align: Align{x: 0.5 y: 0.4}
            // The scrim takes every click outside the card.
            cut_keys_sheet_scrim := Button{width: Fill height: Fill margin: 0 text: "" draw_bg +: {color: stage.scrim color_hover: stage.scrim color_down: stage.scrim color_focus: stage.scrim border_size: 0.0}}
            RoundedView{width: 720 height: Fit flow: Down show_bg: true
                draw_bg +: {color: stage.panel border_color: stage.separator border_size: 1.0 border_radius: 8.0}
                View{width: Fill height: 48 flow: Right padding: Inset{left: 20 right: 12} align: Align{y: 0.5}
                    Label{width: Fill text: "Keys" draw_text +: {color: stage.text_hi text_style: theme.font_bold{font_size: 11}}}
                    Tip{text: "Close (Esc, ?)" cut_keys_close := IconTool{draw_bg +: {color: #0000 border_size: 0.0} draw_icon +: {color: stage.faint svg: crate_resource("self:resources/icons/ui/cross.svg")}}}}
                Rule{}
                View{width: Fill height: Fit flow: Right spacing: 20 padding: 20
                    View{width: 340 height: Fit flow: Down spacing: 4
                        SecCaps{margin: Inset{top: 0 bottom: 2} text: "PLAYBACK"}
                        View{width: Fill height: 20 flow: Right align: Align{y: 0.5} View{width: 110 height: Fit KeyCap{label +: {text: "space / K"}}} Label{width: 230 max_lines: 1 text: "play, pause" draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 8}}}}
                        View{width: Fill height: 20 flow: Right align: Align{y: 0.5} View{width: 110 height: Fit KeyCap{label +: {text: "J / L"}}} Label{width: 230 max_lines: 1 text: "shuttle back, forward" draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 8}}}}
                        View{width: Fill height: 20 flow: Right align: Align{y: 0.5} View{width: 110 height: Fit KeyCap{label +: {text: "← / →"}}} Label{width: 230 max_lines: 1 text: "one frame (shift: ten)" draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 8}}}}
                        View{width: Fill height: 20 flow: Right align: Align{y: 0.5} View{width: 110 height: Fit KeyCap{label +: {text: "Home / End"}}} Label{width: 230 max_lines: 1 text: "start, end" draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 8}}}}
                        SecCaps{margin: Inset{top: 12 bottom: 2} text: "EDITING"}
                        View{width: Fill height: 20 flow: Right align: Align{y: 0.5} View{width: 110 height: Fit KeyCap{label +: {text: "E"}}} Label{width: 230 max_lines: 1 text: "add the chosen library item" draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 8}}}}
                        View{width: Fill height: 20 flow: Right align: Align{y: 0.5} View{width: 110 height: Fit KeyCap{label +: {text: "B"}}} Label{width: 230 max_lines: 1 text: "blade at the playhead" draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 8}}}}
                        View{width: Fill height: 20 flow: Right align: Align{y: 0.5} View{width: 110 height: Fit KeyCap{label +: {text: "T / V"}}} Label{width: 230 max_lines: 1 text: "title, voiceover line" draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 8}}}}
                        View{width: Fill height: 20 flow: Right align: Align{y: 0.5} View{width: 110 height: Fit KeyCap{label +: {text: "delete"}}} Label{width: 230 max_lines: 1 text: "delete the selection" draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 8}}}}
                        View{width: Fill height: 20 flow: Right align: Align{y: 0.5} View{width: 110 height: Fit KeyCap{label +: {text: "cmd-Z"}}} Label{width: 230 max_lines: 1 text: "undo" draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 8}}}}
                        View{width: Fill height: 20 flow: Right align: Align{y: 0.5} View{width: 110 height: Fit KeyCap{label +: {text: "shift-cmd-Z"}}} Label{width: 230 max_lines: 1 text: "redo" draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 8}}}}
                        View{width: Fill height: 20 flow: Right align: Align{y: 0.5} View{width: 110 height: Fit KeyCap{label +: {text: "ctrl-R"}}} Label{width: 230 max_lines: 1 text: "render the selection" draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 8}}}}
                        View{width: Fill height: 20 flow: Right align: Align{y: 0.5} View{width: 110 height: Fit KeyCap{label +: {text: "shift-ctrl-R"}}} Label{width: 230 max_lines: 1 text: "render all" draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 8}}}}
                    }
                    View{width: 340 height: Fit flow: Down spacing: 4
                        SecCaps{margin: Inset{top: 0 bottom: 2} text: "MARKS"}
                        View{width: Fill height: 20 flow: Right align: Align{y: 0.5} View{width: 110 height: Fit KeyCap{label +: {text: "I / O"}}} Label{width: 230 max_lines: 1 text: "mark in, out (alt clears)" draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 8}}}}
                        View{width: Fill height: 20 flow: Right align: Align{y: 0.5} View{width: 110 height: Fit KeyCap{label +: {text: "M"}}} Label{width: 230 max_lines: 1 text: "marker" draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 8}}}}
                        SecCaps{margin: Inset{top: 12 bottom: 2} text: "VIEW"}
                        View{width: Fill height: 20 flow: Right align: Align{y: 0.5} View{width: 110 height: Fit KeyCap{label +: {text: "+ / − / shift-Z"}}} Label{width: 230 max_lines: 1 text: "zoom in, out, fit" draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 8}}}}
                        View{width: Fill height: 20 flow: Right align: Align{y: 0.5} View{width: 110 height: Fit KeyCap{label +: {text: "N / S"}}} Label{width: 230 max_lines: 1 text: "snapping, skimming" draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 8}}}}
                        View{width: Fill height: 20 flow: Right align: Align{y: 0.5} View{width: 110 height: Fit KeyCap{label +: {text: "'"}}} Label{width: 230 max_lines: 1 text: "safe zones" draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 8}}}}
                        View{width: Fill height: 20 flow: Right align: Align{y: 0.5} View{width: 110 height: Fit KeyCap{label +: {text: "Esc"}}} Label{width: 230 max_lines: 1 text: "disarm, close, deselect" draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 8}}}}
                        View{width: Fill height: 20 flow: Right align: Align{y: 0.5} View{width: 110 height: Fit KeyCap{label +: {text: "?"}}} Label{width: 230 max_lines: 1 text: "this sheet" draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 8}}}}
                        SecCaps{margin: Inset{top: 12 bottom: 2} text: "FILE"}
                        View{width: Fill height: 20 flow: Right align: Align{y: 0.5} View{width: 110 height: Fit KeyCap{label +: {text: "cmd-S"}}} Label{width: 230 max_lines: 1 text: "save" draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 8}}}}
                        View{width: Fill height: 20 flow: Right align: Align{y: 0.5} View{width: 110 height: Fit KeyCap{label +: {text: "cmd-E"}}} Label{width: 230 max_lines: 1 text: "render…" draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 8}}}}
                    }
                }
            }
        }
    }
}


/// The page's widgets, then the page.
pub fn register(vm: &mut ScriptVm) {
    makepad_story_edit_render::script_mod(vm);
    makepad_library_browser::script_mod(vm);
    controls::script_mod(vm);
    timeline::script_mod(vm);
    preview::script_mod(vm);
    makepad_stage_ui::page_dock::script_mod(vm);
    browser::script_mod(vm);
    motion_edit::register(vm);
    generate::register(vm);
    cut_panel::script_mod(vm);
    storyboard::register(vm);
    generate_segments::register(vm);
    script_mod(vm);
}

/// The page's library lanes: the store's, then the bundled effects and
/// transitions (the effect pickers' own tiles).

/// The page's splitters and their default positions (design §3.1): the
/// timeline's height, the left column's width, the sidebar's width.
const SPLITS: [(LiveId, bool, f64); 4] = [
    (live_id!(cut_split), false, 404.0),
    (live_id!(cut_panel_split), true, 440.0),
    (live_id!(cut_side_split), false, 360.0),
    (live_id!(chat_split), false, 360.0),
];

/// Every docked splitter whose place is remembered, by the name the layout
/// record keeps it under: the CUT page's three, the chat beside the pages
/// and the DECK page's code editor and navigation column. Each is `(name, id, from
/// the first pane, default size)`.
const LAYOUT: [(&str, LiveId, bool, f64); 6] = [
    ("cut_split", live_id!(cut_split), false, 404.0),
    ("cut_panel_split", live_id!(cut_panel_split), true, 440.0),
    ("cut_side_split", live_id!(cut_side_split), false, 360.0),
    ("chat_split", live_id!(chat_split), false, 360.0),
    ("deck_code_split", live_id!(deck_code_split), false, 300.0),
    ("deck_nav_split", live_id!(deck_nav_split), true, 280.0),
];

/// The deck's left column (library, slides, files), picture and presenter visibility.
pub(crate) const DECK_COLUMNS: [(&str, LiveId); 3] = [
    ("deck_strip", live_id!(deck_strip)),
    ("deck_centre", live_id!(deck_centre)),
    ("deck_side", live_id!(deck_side)),
];

/// The saved layout: one record of `name=size` words, and `name.fold=A|B` for
/// a pane folded away; `name.visible=0|1` remembers columns without a splitter.
#[derive(Default)]
pub struct DockLayout {
    entries: std::collections::BTreeMap<String, String>,
}

impl DockLayout {
    pub fn parse(text: &str) -> Self {
        Self { entries: text.split_whitespace().filter_map(|word| word.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect() }
    }

    pub fn text(&self) -> String {
        self.entries.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ")
    }

    fn size(&self, name: &str) -> Option<f64> {
        self.entries.get(name).and_then(|v| v.parse().ok())
    }

    fn fold(&self, name: &str) -> SplitterCollapse {
        match self.entries.get(&format!("{name}.fold")).map(String::as_str) {
            Some("A") => SplitterCollapse::A,
            Some("B") => SplitterCollapse::B,
            _ => SplitterCollapse::None,
        }
    }

    /// What the splitters now say, over what the record had (a splitter of
    /// a page not built keeps its entry).
    fn read(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        for (name, id, _, _) in LAYOUT {
            let split = ui.splitter(cx, &[id]);
            if let Some(SplitterAlign::FromA(size) | SplitterAlign::FromB(size)) = split.align() {
                self.entries.insert(name.to_string(), format!("{size:.0}"));
            } else {
                continue;
            }
            let fold = match split.collapse() {
                SplitterCollapse::A => Some("A"),
                SplitterCollapse::B => Some("B"),
                SplitterCollapse::None => None,
            };
            match fold {
                Some(fold) => self.entries.insert(format!("{name}.fold"), fold.to_string()),
                None => self.entries.remove(&format!("{name}.fold")),
            };
        }
        for (name, id) in DECK_COLUMNS {
            let column = ui.widget(cx, &[id]);
            if !column.is_empty() {
                self.entries.insert(format!("{name}.visible"), if column.visible() { "1" } else { "0" }.into());
            }
        }
    }

    /// The record's places and folds, to the splitters that exist.
    pub fn apply(&self, cx: &mut Cx, ui: &WidgetRef, only: &[LiveId]) {
        for (name, id, from_a, _) in LAYOUT {
            if !only.is_empty() && !only.contains(&id) {
                continue;
            }
            let split = ui.splitter(cx, &[id]);
            if let Some(size) = self.size(name) {
                split.set_align(cx, split_align(from_a, size));
            }
            let fold = self.fold(name);
            if split.collapse() != fold {
                split.set_collapse(cx, fold);
            }
        }
        for (name, id) in DECK_COLUMNS {
            if only.is_empty() || only.contains(&id) {
                let visible = self.entries.get(&format!("{name}.visible")).is_none_or(|value| value != "0");
                ui.widget(cx, &[id]).set_visible(cx, visible);
            }
        }
    }
}

fn split_align(from_a: bool, size: f64) -> SplitterAlign {
    if from_a { SplitterAlign::FromA(size) } else { SplitterAlign::FromB(size) }
}

/// New ▾'s rows: size and aspect name.
const NEW_FORMATS: [(LiveId, (u32, u32, &str)); 4] = [
    (live_id!(cut_new_wide), (1920, 1080, "16:9")),
    (live_id!(cut_new_tall), (1080, 1920, "9:16")),
    (live_id!(cut_new_square), (1080, 1080, "1:1")),
    (live_id!(cut_new_portrait), (1080, 1350, "4:5")),
];
/// The blank page's tiles: size and aspect name.
const BLANK_FORMATS: [(LiveId, (u32, u32, &str)); 4] = [
    (live_id!(cut_blank_wide), (1920, 1080, "16:9")),
    (live_id!(cut_blank_tall), (1080, 1920, "9:16")),
    (live_id!(cut_blank_square), (1080, 1080, "1:1")),
    (live_id!(cut_blank_portrait), (1080, 1350, "4:5")),
];

/// A format name's size: 16:9, 9:16, 1:1 or 4:5.
fn format_size(aspect: &str) -> Option<(u32, u32)> {
    BLANK_FORMATS.iter().find(|(_, (_, _, name))| *name == aspect).map(|(_, (width, height, _))| (*width, *height))
}

const LANE_LABELS: [(LiveId, &str); 10] = [
    (live_id!(cut_lane_video), "Video"),
    (live_id!(cut_lane_web), "Web"),
    (live_id!(cut_lane_image), "Images"),
    (live_id!(cut_lane_audio), "Audio"),
    (live_id!(cut_lane_scores), "Scores"),
    (live_id!(cut_lane_edits), "Cuts"),
    (live_id!(cut_lane_runs), "App runs"),
    (live_id!(cut_lane_decks), "Decks"),
    (live_id!(cut_lane_fx), "Effects"),
    (live_id!(cut_lane_trans), "Transitions"),
];

const LANES: [(LiveId, PageLane); 10] = [
    (live_id!(cut_lane_video), PageLane::Store(LibLane::Video)),
    (live_id!(cut_lane_web), PageLane::Web),
    (live_id!(cut_lane_image), PageLane::Store(LibLane::Image)),
    (live_id!(cut_lane_audio), PageLane::Store(LibLane::Audio)),
    (live_id!(cut_lane_scores), PageLane::Store(LibLane::Scores)),
    (live_id!(cut_lane_edits), PageLane::Store(LibLane::Cuts)),
    (live_id!(cut_lane_runs), PageLane::Store(LibLane::AppRuns)),
    (live_id!(cut_lane_decks), PageLane::Store(LibLane::Decks)),
    (live_id!(cut_lane_fx), PageLane::Effects),
    (live_id!(cut_lane_trans), PageLane::Transitions),
];

/// A new cut: one picture stack and the output.
pub fn template(width: u32, height: u32, label: &str) -> String {
    format!(
        "use mod.flow.*\nuse mod.cut.*\n\n// The picture: every video block, bottom first.\nlet main = Stack{{layers: []}}\nlet out = CutOutput{{frame: main.out(@frame) audio: []}}\n\nFlow{{\n    kind: @cut\n    label: {}\n    output: {{width: {width} height: {height} fps: {fps}}}\n    duration: 30.0\n    bpm: 120\n    main, out\n}}\n",
        source::string_literal(label),
        fps = rates::DEFAULT_FPS,
    )
}

/// What a job on the cut worker is for.
enum Pending {
    Op(CutOp),
    Document(Vec<makepad_app_shell::tool_host::Change>),
    Open { source: String, head: Option<AssetRevisionRef>, saved: bool, tree: Option<store::OpenTree>, folder: Option<FolderOpen> },
}

/// A folder cut's tree as read from disk, for the open (`reload`: the open
/// cut again after its files changed, keeping the playhead).
struct FolderOpen {
    doc: &'static folder_docs::FolderDoc,
    draft: Arc<makepad_asset_data::tree::TreeDraft>,
    path: String,
    reload: bool,
}

/// A render of a saved cut revision on its way.
struct Rendering {
    /// The library revision rendered; `None`: a folder cut (its files are
    /// its saved state; its render is a file, never published).
    edit: Option<AssetRevisionRef>,
    file: Option<String>,
    source: String,
    doc: Arc<CutDoc>,
    /// Its folder: the job, the renderer's log and the file.
    dir: std::path::PathBuf,
    progress: (u64, u64),
    publishing: bool,
}

/// The explorer's player: the item, whether it has a picture, and where
/// it is.
struct AssetPlay {
    asset: AssetRevisionRef,
    video: bool,
    started: bool,
    playing: bool,
    t: f64,
    seek: Option<f64>,
}

/// Cards whose picture or waveform is being made at once.
const CARD_MEDIA_AT_ONCE: usize = 2;

/// Playing: since this real time, from this cut time, at this rate.
#[derive(Clone, Copy)]
struct Playing {
    since: f64,
    from: f64,
    rate: f64,
}

#[derive(Default)]
pub struct VideoCut {
    /// A deck picked in the Decks lane: the product opens it on the DECK page.
    pub(crate) deck_request: Option<(AssetRevisionRef, String)>,
    /// The library explorer (shared with the DECK page).
    lib: browser::LibraryBrowser,
    /// The open cut's draft: written a second after each change.
    autosave: autosave::Autosave,
    /// The Frame rate picker is on Custom… (its field shows).
    fps_custom: bool,
    /// The revision the timeline's clip filmstrips were planned for.
    films_rev: Option<u64>,
    /// The render bar, the render settings and the export to a file.
    render_ui: render_ui::RenderUi,
    started: bool,
    stopped: bool,
    worker: Option<CutWorker>,
    media: Option<MediaWorker>,
    session: Option<Session>,
    queue: VecDeque<Pending>,
    in_flight: Option<(u64, Pending)>,
    serial: u64,
    /// Jobs a full queue held back, retried each pump.
    held_media: VecDeque<MediaJob>,
    /// An effect or transition chosen in the picker, waiting for a click
    /// on the clip it goes on: `(preset, transition)`.
    armed: Option<(String, bool)>,
    /// A media asset the library should show once its lane has loaded.
    reveal: Option<String>,
    /// Decoded thumbnails waiting for upload, and uploaded ones, by asset id.
    requested: HashSet<String>,
    titles: HashMap<String, String>,
    /// Loudness outlines by asset id, and those asked for.
    peaks: HashMap<String, Arc<Peaks>>,
    peaks_asked: HashSet<String>,
    /// Takes this session made: their length and words.
    takes: HashMap<AssetRef, TakeInfo>,
    /// The Kokoro voices installed here.
    voices: Vec<String>,
    /// Voiceover lines being spoken now: `(node, line)`.
    speaking: HashSet<(String, usize)>,
    /// The selected block: a node id, or `node#line` / `node#caption`.
    selected: Option<String>,
    /// The node and document revision the inspector shows, so it is only
    /// refilled when they change (a field being dragged keeps its value).
    inspected: Option<(String, u64)>,
    /// The inspector's parameter rows: `(key, min, max)` per row.
    rows_shown: Vec<Option<(String, f64, f64)>>,
    sections: HashMap<LiveId, bool>,
    playhead: f64,
    skim: Option<f64>,
    marks: (Option<f64>, Option<f64>),
    playing: Option<Playing>,
    status: String,
    /// Why the last open did not show a cut (the blank page says it).
    open_error: String,
    projection: Projection,
    /// The document revision the viewer's renderer was made for.
    rendered: Option<u64>,
    /// The renderer process's runner, and the render under way: the cut
    /// revision, its source, and progress.
    runner: Option<RenderRunner>,
    render: Option<Rendering>,
    /// Filmstrips by asset id (a run's by its render), those asked for,
    /// and decoded ones waiting for upload.
    films: HashMap<String, Filmstrip>,
    films_asked: HashSet<String>,
    film_uploads: VecDeque<(String, Vec<(f64, Thumb)>)>,
    /// Markers (M), editor-session times.
    markers: Vec<f64>,
    snap_off: bool,
    skim_off: bool,
    track_height: TrackHeight,
    /// The saved head's render: fresh, stale or none.
    head_render: Option<RenderState>,
    /// When the cut was last saved (`hh:mm`), a save under way, and why
    /// the last one failed.
    saved_at: Option<String>,
    saving: bool,
    saving_tree: Option<Arc<makepad_asset_data::tree::TreeDraft>>,
    save_requested: bool,
    tree_undo: VecDeque<bool>,
    save_error: Option<String>,
    /// The last render's failure, for the Render button.
    render_failed: Option<String>,
    /// The library render the last export published (the control API's
    /// `render_status`).
    last_render: Option<AssetRevisionRef>,
    render_project: Option<(String, String)>,
    render_began: f64,
    /// The last finished render's line (`render_status` says "done").
    render_done: Option<String>,
    /// Items per library lane, and the newest edits (the blank page's
    /// recent list).
    recent: Vec<store::Recent>,
    /// The selected card's item (asset id, `draft:<id>` or a preset), and
    /// the index last painted for it: the selection follows its item.
    /// The LOOKS lanes' list rows (list mode), in the list's order.
    looks_rows: Vec<looks_list::LookRow>,
    /// The Web lane's grabs and requests (WB1).
    web: web_lane::WebLane,
    /// Share's menu and the web export (web_export_ui.rs).
    web_export: web_export_ui::WebExport,
    /// The lane the sidebar was last painted for: the library's paint
    /// asks for a sidebar paint whenever they differ.
    safe_zones: bool,
    loop_on: bool,
    /// The size New makes: the last one the person or `new_cut` chose
    /// (16:9 until then).
    new_format: Option<(u32, u32)>,
    /// New ▾ is open.
    new_menu: bool,
    /// cmd-T's further edit points, one per finished edit.
    pending_cuts: Vec<(String, String)>,
    info_measured: f64,
    /// The selected library item's picture is portrait (side layout).
    lib_portrait: bool,
    thumbnail_of: Option<u64>,
    /// Edits published without a poster (the lists show a placeholder),
    /// those already taken up this session, and the one being drawn.



    /// Filmstrips and loudness outlines the library's cards asked for and
    /// still wait for (`f:<asset>`, `p:<asset>`).
    card_media: HashSet<String>,
    /// A save waiting for the renderer's thumbnail (and the revision it is
    /// of), and the last saved thumbnail (a render's poster).
    pending_thumbnail: Option<(CutThumbnail, u64)>,
    saved_thumb: Option<(u64, Thumb)>,
    /// The cuts a nested one was opened from: their names and heads.
    crumbs: Vec<(String, Option<AssetRevisionRef>)>,
    local_files: Vec<(String, std::path::PathBuf)>,
    /// The folder cut open in place (its watcher), and the song prompt.
    pub(crate) folder: Option<folder_docs::OpenFolder>,
    song: folder_docs::SongDesk,
    /// The folder cards' pictures.
    card_pictures: folder_docs::CardPictures,
    /// The folder cut's autosave: the revision last seen and since when, and
    /// the revision a save was refused for (a file changed on disk too).
    folder_seen: Option<(u64, std::time::Instant)>,
    folder_refused: Option<u64>,
    /// A folder deck card clicked in the Decks lane: DECK opens it.
    pub(crate) deck_card_request: Option<folder_docs::Card>,
    /// The explorer: film facts by asset id, details of selected items,
    /// the INFO panel's folded state and height, and its player.
    film_info: HashMap<String, media::FilmInfo>,
    details: HashMap<AssetRevisionRef, store::Details>,
    details_asked: HashSet<AssetRevisionRef>,
    info_folded: bool,
    info_dragged: bool,
    info_height: f64,
    asset_play: Option<AssetPlay>,
    /// The playhead frame the inspector was filled at.
    inspected_frame: i64,
    /// The sidebar in its icons-only form (a narrow window).
    compact: bool,
    /// The left column shows the film's editor panel (else the library).
    left_editor: bool,
    /// The sidebar's top pane shows the structure (else the inspector).
    side_tree: bool,
    /// The last mouse-down (a double click resets a splitter).
    last_down: Option<(f64, Vec2d)>,
    /// The saved dock layout, and whether the DECK page has been given its part.
    layout: DockLayout,
    deck_layout_applied: bool,
    /// Presentation temporarily folds docks and changes columns; those
    /// changes must not replace the person's saved editing layout.
    deck_layout_suspended: bool,
    /// The timecode being typed, and when a typed time was refused.
    typing_time: bool,
    time_error: Option<f64>,
    /// The shortcut sheet (?).
    keys_sheet: bool,
    /// More selected blocks (a marquee), beyond `selected`.
    also: Vec<String>,
    /// The structure's shut rows.
    tree_folded: HashSet<String>,
    /// The explorer's tag filter, the lane's tags, A–Z order, and who uses
    /// the selected item (library-wide).
    detail_users: Option<(AssetRevisionRef, Option<u64>)>,
    /// Burn-in off for the next render, and the poster shown while it runs.
    burn_off: bool,
    /// The render's loudness target: Off, −14, −16, −23 LUFS.
    loudness: usize,
    /// The name last written into the title field.
    shown_label: String,
    /// The viewer's platform overlay, when the page chose one.
    platform: Option<SafeZone>,
    /// The status line holds an action's message until the next change.
    status_sticky: bool,
    focused: bool,
    repaint: bool,
    repaint_library: bool,
    repaint_preview: bool,
    /// The viewer's next display frame while playing or scrubbing.
    frame: NextFrame,
    /// When the last of those ran, and the display's frame interval.
    last_tick: Option<f64>,
    refresh: f64,
    /// The timecode's ink as last applied.
    time_ink: Option<u32>,
    /// The revision and library counts the viewer's stand-ins were made for.
    stand_ins_seen: Option<(u64, usize, usize)>,
    /// The Source view, and an opened graphic (`motion_edit.rs`).
    motion: motion_edit::MotionEdit,
    /// The cut's own panel (lane TL1).
    cut_panel: cut_panel::State,
    /// The chat's video maker: the model's loop and its transcript (`generate.rs`).
    generate: generate::Generate,
    /// What the page asks of the window's chat (`chat.rs`).
    chat: makepad_app_shell::chat::ChatOutbox,
    /// A song video made from the chat: its steps (`new_video.rs`).
    new_video: new_video::NewVideo,
}

fn ids(reference: &AssetRevisionRef) -> AssetRef {
    AssetRef { id: reference.asset_id.to_string(), rev: reference.revision.to_string() }
}

fn exact(asset: &AssetRef) -> Option<AssetRevisionRef> {
    Some(AssetRevisionRef { asset_id: asset.id.parse().ok()?, revision: asset.rev.parse().ok()? })
}

/// An app run clip's exact reference: the run revision and the render it
/// pins.
fn run_ref(run: &AssetRevisionRef, render: &AssetRevisionRef) -> RunRef {
    RunRef { id: run.asset_id.to_string(), rev: run.revision.to_string(), render_id: render.asset_id.to_string(), render: render.revision.to_string() }
}

fn media_id(media: &MediaRef) -> &str {
    match media {
        MediaRef::Asset(asset) => &asset.id,
        MediaRef::Run(run) => &run.id,
        MediaRef::MissingOptional(path) => path,
    }
}

/// The node a selection names, and its line or caption.
fn split_selection(selected: &str) -> (&str, Option<usize>) {
    match selected.split_once('#') {
        Some((node, index)) => (node, index.parse().ok()),
        None => (selected, None),
    }
}

/// "today 20:39", or the date.
fn added_text(published_ms: u64) -> String {
    let at = makepad_civil_time::local_or_utc((published_ms / 1000) as i64);
    let now = makepad_civil_time::local_now();
    if at.date_text() == now.date_text() { format!("today {}", at.hm()) } else { at.date_text() }
}

/// "modified 14:32" (today's time; the date otherwise).
pub(super) fn modified(published_ms: u64) -> String {
    if published_ms == 0 {
        return "Cut".into();
    }
    let at = makepad_civil_time::local_or_utc((published_ms / 1000) as i64);
    let now = makepad_civil_time::local_now();
    if at.date_text() == now.date_text() { format!("modified {}", at.hm()) } else { format!("modified {}", at.date_text()) }
}

/// A cut's render state as a card badge: text, fill, ink.
fn render_badge(p: &makepad_look::palette::Palette, render: Option<RenderState>) -> (String, u32, u32) {
    match render {
        Some(RenderState::Fresh(_)) => ("● Rendered".to_string(), 0x000000b3, p.live),
        Some(RenderState::Stale(_)) => ("Stale".to_string(), p.amber, p.on_accent),
        _ => ("No render".to_string(), p.raised, p.label),
    }
}

/// A picture with nothing on it (a render's black first frame).
pub(super) fn is_black(thumb: &Thumb) -> bool {
    let step = (thumb.pixels.len() / 997).max(1);
    let (sum, n) = thumb.pixels.iter().step_by(step).fold((0u64, 0u64), |(sum, n), px| {
        let (r, g, b) = ((px >> 16) & 0xff, (px >> 8) & 0xff, px & 0xff);
        (sum + r.max(g).max(b) as u64, n + 1)
    });
    n > 0 && sum / n < 24
}

/// A voice take (the voice role) rather than music: by its tags, or by the
/// title voice takes are published under.
fn is_voice_take(title: &str, details: Option<&store::Details>) -> bool {
    details.is_some_and(|details| details.tags.iter().any(|tag| tag.contains("voice") || tag.contains("take"))) || title.starts_with("Voice")
}

/// Bytes for people: `12 KB`, `2.9 MB`.
fn size_text(bytes: u64) -> String {
    if bytes < 1_000_000 { format!("{} KB", bytes.div_ceil(1000)) } else { format!("{:.1} MB", bytes as f64 / 1e6) }
}

/// An output size's aspect name.
fn aspect_name(width: u32, height: u32) -> String {
    let (w, h) = (width.max(1) as u64, height.max(1) as u64);
    let gcd = |mut a: u64, mut b: u64| {
        while b != 0 {
            (a, b) = (b, a % b);
        }
        a
    };
    let g = gcd(w, h);
    format!("{}:{}", w / g, h / g)
}

fn zoom_to_slider(px_per_sec: f64) -> f64 {
    ((px_per_sec / ZOOM_MIN).ln() / (ZOOM_MAX / ZOOM_MIN).ln()).clamp(0.0, 1.0)
}

fn slider_to_zoom(value: f64) -> f64 {
    ZOOM_MIN * (ZOOM_MAX / ZOOM_MIN).powf(value.clamp(0.0, 1.0))
}

impl VideoCut {
    fn start(&mut self, cx: &mut Cx) {
        self.started = true;
        self.lib.attach(cx, live_id!(cut_surface));
        self.worker = Some(CutWorker::new(cx));
        self.media = Some(MediaWorker::new(cx));
        match RenderRunner::new(cx) {
            Ok(runner) => self.runner = Some(runner),
            Err(error) => self.status = format!("render runner unavailable: {error}"),
        }
        for error in [
            self.lib.store.as_ref().and_then(|store| store.error.clone()),
            self.worker.as_ref().and_then(|worker| worker.error.clone()),
            self.media.as_ref().and_then(|media| media.error.clone()),
        ].into_iter().flatten() {
            self.status = error;
        }
        self.send(StoreJob::Titles);
        self.send(StoreJob::Layout(None));
        self.start_autosave(cx);
        self.send_media(MediaJob::Voices);
    }

    pub(super) fn stop(&mut self) {
        self.stopped = true;
        self.stop_autosave();
        if let Some(store) = &mut self.lib.store {
            store.stop();
        }
        if let Some(worker) = &mut self.worker {
            worker.stop();
        }
        if let Some(media) = &mut self.media {
            media.stop();
        }
        if let Some(runner) = &mut self.runner {
            runner.stop();
        }
        motion_edit::stop(self);
        storyboard::stop();
        generate::stop(self);
    }

    fn fps(&self) -> f64 {
        self.session.as_ref().map_or(60.0, |session| session.doc.output.fps)
    }

    fn duration(&self) -> f64 {
        self.session.as_ref().map_or(0.0, |session| session.doc.duration)
    }

    fn send(&mut self, job: StoreJob) {
        self.lib.send(job);
    }

    fn send_media(&mut self, job: MediaJob) {
        self.held_media.push_back(job);
        self.flush_held();
    }

    fn flush_held(&mut self) {
        self.lib.flush_held();
        if let Some(media) = &self.media {
            while let Some(job) = self.held_media.pop_front() {
                if let Err(job) = media.try_send(job) {
                    self.held_media.push_front(job);
                    break;
                }
            }
        }
    }

    fn submit(&mut self, pending: Pending) {
        self.queue.push_back(pending);
        self.repaint = true;
    }

    fn op(&mut self, op: CutOp) {
        self.submit(Pending::Op(op));
    }

    fn open_source(&mut self, source: String, head: Option<AssetRevisionRef>, saved: bool) {
        self.open_source_context(source, head, saved, None);
    }

    fn open_source_context(&mut self, source: String, head: Option<AssetRevisionRef>, saved: bool, tree: Option<store::OpenTree>) {
        self.submit(Pending::Open { source, head, saved, tree, folder: None });
    }

    fn new_cut(&mut self, (width, height, aspect): (u32, u32, &str)) {
        self.new_format = Some((width, height));
        self.crumbs.clear();
        self.open_source(template(width, height, &format!("Untitled {aspect}")), None, false);
        self.status = match self.kept_label() {
            Some(label) => format!("New {aspect} edit · “{label}” is kept in Cuts › Drafts"),
            None => format!("New {aspect} edit · {width}×{height}"),
        };
        self.repaint = true;
    }

    fn pump_worker(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        let Some(worker) = &self.worker else { return };
        if let Some(Done { serial, result }) = worker.try_recv() {
            if let Some((expected, pending)) = self.in_flight.take() {
                if expected == serial {
                    self.finish(cx, ui, pending, result);
                } else {
                    self.in_flight = Some((expected, pending));
                }
            }
        }
        if self.in_flight.is_some() {
            return;
        }
        if (self.saving || self.save_requested) && matches!(self.queue.front(), Some(Pending::Open { .. })) { return; }
        let Some(pending) = self.queue.pop_front() else { return };
        let job = match &pending {
            Pending::Open { source, tree, folder, .. } => Job::Load { serial: self.serial + 1, source: source.clone(), tree: tree.as_ref().map(|opened| (opened.draft.clone(), opened.path.clone())).or_else(|| folder.as_ref().map(|folder| (folder.draft.clone(), folder.path.clone()))) },
            Pending::Document(changes) => {
                let Some(session) = &self.session else { return };
                let Some(tree) = &session.tree else { return };
                Job::Document { serial: self.serial + 1, tree: tree.clone(), path: session.path.clone(), changes: changes.clone() }
            }
            Pending::Op(op) => match &self.session {
                Some(session) => Job::Apply { serial: self.serial + 1, source: session.source.clone(), op: op.clone(), tree: session.tree.clone().map(|tree| (tree, session.path.clone())) },
                None => {
                    self.status = "Open or start a cut first".into();
                    self.repaint = true;
                    return;
                }
            },
        };
        let Some(worker) = &self.worker else { return };
        match worker.try_send(job) {
            Ok(()) => {
                self.serial += 1;
                self.in_flight = Some((self.serial, pending));
            }
            Err(_) => self.queue.push_front(pending),
        }
    }

    fn finish(&mut self, cx: &mut Cx, ui: &WidgetRef, pending: Pending, result: Result<(String, Arc<CutDoc>, Option<Arc<makepad_asset_data::tree::TreeDraft>>), String>) {
        let timeline = ui.widget(cx, ids!(cut_timeline));
        match (pending, result) {
            (Pending::Open { folder: Some(folder), .. }, Ok((source, doc, _))) if folder.reload => {
                // The open folder cut's files changed on disk: its new tree,
                // at the same playhead (only a cut with no changes of its own).
                let duration = doc.duration;
                if let Some(session) = &mut self.session {
                    session.saved = source.clone();
                    session.saved_doc = Some(doc.clone());
                    session.saved_tree = Some(folder.draft.clone());
                    session.saved_path = folder.path.clone();
                    session.source = source;
                    session.doc = doc;
                    session.tree = Some(folder.draft);
                    session.path = folder.path;
                    session.undo.clear();
                    session.redo.clear();
                    session.revision += 1;
                }
                self.playhead = self.playhead.min(duration);
                self.tree_undo.clear();
                self.status = format!("“{}” changed on disk: reloaded", folder.doc.name);
                log!("edit: {} changed on disk: reloaded at {:.2} s", folder.doc.name, self.playhead);
            }
            (Pending::Open { head, saved, tree, folder, .. }, Ok((source, doc, _))) => {
                // The cut being replaced is written first: nothing is lost
                // (a folder cut into its folder, a library cut as its draft).
                self.leave_folder();
                self.write_draft(cx, ui);
                self.open_error.clear();
                let revision = self.session.as_ref().map_or(0, |session| session.revision) + 1;
                self.session = Some(Session::open(source, doc, revision, head, saved));
                self.folder = None;
                if let Some(folder) = folder {
                    // In place: no library revision, the folder's files are the saved state.
                    let session = self.session.as_mut().unwrap();
                    session.path = folder.path;
                    session.saved_path = session.path.clone();
                    session.saved_tree = Some(folder.draft.clone());
                    session.tree = Some(folder.draft);
                    self.folder = Some(folder_docs::OpenFolder::open(folder.doc));
                }
                if let Some(opened) = tree {
                    let session = self.session.as_mut().unwrap();
                    session.head = Some(opened.head);
                    session.history_head = Some(opened.head);
                    session.path = opened.path;
                    session.saved_path = session.path.clone();
                    session.alias = Some(opened.alias);
                    session.saved_tree = saved.then_some(opened.draft.clone());
                    session.tree = Some(opened.draft);
                }
                self.selected = None;
                self.playhead = 0.0;
                self.playing = None;
                self.marks = (None, None);
                self.markers.clear();
                self.platform = None;
                self.saved_at = None;
                self.save_error = None;
                self.render_failed = None;
                self.render_done = None;
                self.check_head_render();
                self.tree_undo.clear();
                motion_edit::opened(self, cx, ui);
                if !self.opened_draft(cx, ui) {
                    self.fit_after_paint(cx, ui);
                }
            }
            (Pending::Op(op), Ok((source, doc, tree))) => {
                // A move that pulled two clips apart took their transition.
                if let (CutOp::Move { .. }, Some(old)) = (&op, self.session.as_ref().map(|session| session.doc.clone())) {
                    let count = |doc: &CutDoc| doc.nodes.iter().filter(|node| matches!(node.kind, CutNodeKind::Transition { .. })).count();
                    if count(&doc) < count(&old) {
                        self.status = "Transition removed: the clips no longer meet (cmd-Z brings it back)".into();
                        self.status_sticky = true;
                    }
                }
                // What an add made is selected: the new block (a new
                // voiceover's first line).
                if matches!(op, CutOp::Add { .. } | CutOp::AddTitle { .. } | CutOp::AddVoiceOver { .. } | CutOp::Split { .. }) {
                    if let Some(old) = self.session.as_ref().map(|session| session.doc.clone()) {
                        let fresh = doc.nodes.iter().find(|node| old.node(&node.id).is_none() && (node.span.is_some() || matches!(node.kind, CutNodeKind::VoiceOver { .. })));
                        if let Some(node) = fresh {
                            self.selected = Some(if matches!(node.kind, CutNodeKind::VoiceOver { .. }) { format!("{}#0", node.id) } else { node.id.clone() });
                        }
                    }
                }
                if let Some(session) = &mut self.session {
                    session.install_tree(source, doc, tree);
                }
                if let CutOp::Delete { node } = &op {
                    if self.selected.as_deref().is_some_and(|selected| split_selection(selected).0 == node) {
                        self.selected = None;
                    }
                }
                if !std::mem::take(&mut self.status_sticky) {
                    self.status.clear();
                }
            }
            (Pending::Open { .. }, Err(error)) => {
                self.status = format!("The cut does not load: {error}");
                self.open_error = self.status.clone();
            }
            (Pending::Document(changes), Ok((source, doc, tree))) => {
                if let Some(session) = &mut self.session {
                    session.install_tree(source, doc, tree);
                    for change in changes { if let makepad_app_shell::tool_host::Change::Rename { from, to } = change { if session.path == from { session.path = to; } } }
                }
                self.status.clear();
            }
            (Pending::Op(_) | Pending::Document(_), Err(error)) => {
                self.save_requested = false;
                self.save_error = Some(error.clone());
                self.status = format!("Refused: {error}");
            }
        }
        if let Some(mut timeline) = timeline.borrow_mut::<CutTimeline>() {
            timeline.settle(cx);
        }
        self.document_changed();
    }

    fn fit_after_paint(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        if let Some(mut timeline) = ui.widget(cx, ids!(cut_timeline)).borrow_mut::<CutTimeline>() {
            timeline.fit(cx);
        }
    }

    fn document_changed(&mut self) {
        self.repaint = true;
        self.repaint_preview = true;
        self.inspected = None;
        let Some(session) = &self.session else { return };
        let doc = session.doc.clone();
        if self.selected.as_ref().is_some_and(|id| doc.node(split_selection(id).0).is_none()) {
            self.selected = None;
        }
        // Thumbnails of media this cut names that no lane has shown, and
        // the loudness outline of every sound it plays.
        let mut refs = Vec::new();
        let mut sounds = Vec::new();
        let mut films = Vec::new();
        for node in &doc.nodes {
            match &node.kind {
                CutNodeKind::Clip { src, nested: true, .. } => refs.push(src.clone()),
                CutNodeKind::Clip { src, audio, .. } => {
                    refs.push(src.clone());
                    films.push(src.clone());
                    if *audio {
                        sounds.push(src.clone());
                    }
                }
                CutNodeKind::Still { src, .. } => refs.push(src.clone()),
                CutNodeKind::AudioClip { src, .. } => sounds.push(src.clone()),
                CutNodeKind::VoiceOver { lines, .. } => sounds.extend(lines.iter().filter_map(|line| line.take.clone()).map(MediaRef::Asset)),
                _ => {}
            }
        }
        // Motion graphics' thumbnails (their timeline look).
        for graphic in &doc.graphics {
            if let makepad_story_cut::GraphicSrc::Asset(asset) = &graphic.src {
                refs.push(MediaRef::Asset(asset.clone()));
            }
        }
        let mut thumbs = Vec::new();
        for media in refs {
            let asset = match &media {
                MediaRef::Asset(asset) => asset.clone(),
                MediaRef::Run(run) => AssetRef { id: run.id.clone(), rev: run.rev.clone() },
                MediaRef::MissingOptional(_) => continue,
            };
            if !self.lib.thumbs.contains_key(&asset.id) && self.requested.insert(asset.id.clone()) {
                thumbs.extend(exact(&asset));
            }
        }
        if !thumbs.is_empty() {
            self.send(StoreJob::Thumbs { refs: thumbs });
        }
        for media in films {
            let asset = match &media {
                MediaRef::Asset(asset) => asset.id.clone(),
                MediaRef::Run(run) => run.render_id.clone(),
                MediaRef::MissingOptional(_) => continue,
            };
            if self.films_asked.insert(asset) {
                self.send_media(MediaJob::Film { media });
            }
        }
        for media in sounds {
            let asset = match &media {
                MediaRef::Asset(asset) => asset.id.clone(),
                MediaRef::Run(run) => run.render_id.clone(),
                MediaRef::MissingOptional(_) => continue,
            };
            if self.peaks_asked.insert(asset) {
                self.send_media(MediaJob::Peaks { media });
            }
        }
    }

    /// The library changed under the page (a save, a published render): the
    /// lane's rows, badges, counts and the INFO facts are asked again, so
    /// no two places disagree (review 3, C4).
    fn refresh_library(&mut self) {
        self.details.clear();
        self.details_asked.clear();
        self.detail_users = None;
        self.send(StoreJob::Titles);
        if let Some(asset) = self.lib.list_selected.and_then(|index| self.lib.rows.get(index)).map(|row| row.asset_id()) {
            self.reveal = Some(asset.to_string());
        }
        self.query();
    }

    /// Filmstrips of the open cut's pictures: decoded ones, else the
    /// library thumbnail as a one-picture strip (stills, and until the
    /// frames arrive).
    fn films(&self) -> HashMap<String, Filmstrip> {
        let mut films: HashMap<String, Filmstrip> = self.lib.thumbs.iter().map(|(id, (_, texture))| (id.clone(), Arc::new(vec![(0.0, texture.clone())]))).collect();
        films.extend(self.films.iter().map(|(id, film)| (id.clone(), film.clone())));
        films
    }

    /// The ruler's render bar: fresh (true) or stale (false) for a saved
    /// edit with a render.
    fn render_bar(&self) -> Option<bool> {
        match self.head_render? {
            RenderState::Fresh(_) => self.session.as_ref().map(|session| !session.dirty()),
            RenderState::Stale(_) => Some(false),
            RenderState::None => None,
        }
    }

    /// Ask for the saved head's render state.
    fn check_head_render(&mut self) {
        self.head_render = None;
        if let Some(head) = self.session.as_ref().and_then(|session| session.head) {
            let path = self.session.as_ref().filter(|session| session.tree.is_some()).map(|session| session.path.clone());
            self.send(StoreJob::RenderOf { purpose: RenderPurpose::Head, source: head, path });
        }
    }

    fn undo(&mut self, redo: bool) {
        if self.session.as_ref().is_some_and(|session| session.tree.is_some()) {
            self.tree_undo.push_back(redo);
            return;
        }
        let Some(session) = &mut self.session else { return };
        if !session.step(redo) {
            return;
        }
        self.status = if redo { "Redone".into() } else { "Undone".into() };
        self.document_changed();
    }

    fn set_lane(&mut self, cx: &mut Cx, ui: &WidgetRef, lane: PageLane) {
        // Which view shows is decided from the lane on every library paint
        // (`looks_view`), never only here.
        let _ = (cx, ui);
        if self.lib.set_lane(lane) {
            self.repaint_library = true;
            self.repaint = true;
        }
    }

    fn query(&mut self) {
        self.lib.query();
        self.repaint_library = true;
    }

    fn list_rows(&self, cx: &mut Cx) -> Vec<ListRow> {
        let mut rows = self.library_rows(cx);
        rows.extend(self.draft_rows().into_iter().map(|(_, row)| row));
        rows.extend(self.folder_rows());
        rows.extend(self.web_rows());
        rows
    }

    fn library_rows(&self, cx: &mut Cx) -> Vec<ListRow> {
        let p = makepad_look::palette::get(cx);
        let lane = match self.lib.lane {
            Some(PageLane::Store(lane)) => lane,
            Some(PageLane::Web) => LibLane::Video,
            _ => return Vec::new(),
        };
        self.lib.rows.iter().map(|row| {
            let id = row.reference.asset_id.to_string();
            let kind = lane.card();
            // The meta is the picture and the size; the duration is the
            // badge's (§4.8).
            let bytes = (row.bytes > 0).then(|| size_text(row.bytes));
            let dims = self.film_info.get(&id).filter(|info| info.width > 0).map(|info| format!("{}×{}", info.width, info.height))
                .or_else(|| row.format.split(" · ").nth(1).map(str::to_string));
            let join = |parts: Vec<Option<String>>| parts.into_iter().flatten().collect::<Vec<_>>().join(" · ");
            let detail = match kind {
                CardKind::Cut => {
                    let aspect = self.details.get(&row.reference).and_then(|details| details.edit.as_ref()).map(|((w, h, _), _, _)| aspect_name(*w, *h));
                    join(vec![aspect, Some(modified(row.published))])
                }
                CardKind::Audio => join(vec![Some(row.format.split(" · ").next().unwrap_or("Audio").to_uppercase()), bytes]),
                CardKind::AppRun => join(vec![Some("App run".into()), dims, bytes]),
                _ => join(vec![dims, bytes]),
            };
            let draft_id = row.asset_id().to_string();
            let badge = (kind == CardKind::Cut).then(|| if self.edited(&draft_id) { ("EDITED".to_string(), 0x5a4312ff, 0xffd58aff) } else { render_badge(&p, row.render) });
            ListRow {
                title: row.title.clone(),
                detail: self.lib.live_name(row).map(|name| format!("App · {name}")).unwrap_or(detail),
                kind,
                aspect: self.lib.card_aspect(row),
                texture: self.lib.live_name(row).is_none().then(|| self.lib.thumbs.get(&id).map(|(_, texture)| texture.clone())).flatten(),
                film: self.films.get(&id).cloned(),
                seconds: row.seconds,
                used: self.used_ranges(&id, row.seconds),
                badge,
                peaks: (kind == CardKind::Audio).then(|| self.peaks.get(&id).cloned()).flatten(),
                voice: kind == CardKind::Audio && is_voice_take(&row.title, self.details.get(&row.reference)),
            }
        }).collect()
    }

    
    
    
    

    
    
    

    /// One poster at a time, once the page is shown: read the cut, have the
    /// renderer draw its frame in the preview's pass, publish it.
    

    /// The source of the cut whose poster is next: the renderer draws it.
    

    /// The poster is the cut's head: its rows show it.
    

    /// The cards shown (and the one under the pointer) get their picture or
    /// waveform made, a couple at a time; scrolling asks for the next ones.
    fn pump_card_media(&mut self, cx: &mut Cx, ui: &WidgetRef, shown: bool) {
        if !shown || self.card_media.len() >= CARD_MEDIA_AT_ONCE {
            return;
        }
        let lane = match self.lib.lane {
            Some(PageLane::Store(lane @ (LibLane::Video | LibLane::Audio))) => lane,
            Some(PageLane::Web) => LibLane::Video,
            _ => return,
        };
        let Some((hover, drawn)) = self.lib.list(cx, ui).borrow::<CutMediaList>().map(|list| list.wanted()) else { return };
        // The selected video's INFO panel plays from its film as well.
        let selected = self.lib.list_selected.filter(|_| lane == LibLane::Video);
        for index in hover.into_iter().chain(selected).chain(drawn) {
            if self.card_media.len() >= CARD_MEDIA_AT_ONCE {
                break;
            }
            let Some(row) = self.lib.rows.get(index) else { continue };
            let asset = ids(&row.reference);
            if lane == LibLane::Audio {
                // The waveform is the card's picture.
                if self.peaks_asked.insert(asset.id.clone()) {
                    self.card_media.insert(format!("p:{}", asset.id));
                    self.send_media(MediaJob::Peaks { media: MediaRef::Asset(asset) });
                }
            } else if (hover == Some(index) || selected == Some(index) || !row.pictured) && self.films_asked.insert(asset.id.clone()) {
                // The skim reads the film under the pointer; a video without
                // a picture of its own shows its film's frame.
                self.card_media.insert(format!("f:{}", asset.id));
                self.send_media(MediaJob::Film { media: MediaRef::Asset(asset) });
            }
        }
    }

    /// A lane's rows arrived in the explorer: what CUT adds to them (titles,
    /// the lane's own status, posters to draw, the item to reveal).
    fn rows_arrived(&mut self, lane: LibLane) {
        if self.lib.status.starts_with("The library") {
            return;
        }
        for row in &self.lib.rows {
            self.titles.insert(row.reference.asset_id.to_string(), row.title.clone());
        }
        self.lib.status = match (self.lib.rows.len(), lane) {
            (0, _) => format!("No {} in the library", lane.label().to_lowercase()),
            (n, LibLane::Cuts) => format!("{n} edits · click one to open it"),
            (n, LibLane::AppRuns) => format!("{n} runs · drag one onto the timeline"),
            (n, _) => format!("{n} items · drag one onto the timeline"),
        };
        // Edits without a poster draw it in the background.
        
        if let Some(asset) = self.reveal.take() {
            self.lib.list_selected = self.lib.rows.iter().position(|row| row.asset_id().to_string() == asset);
            if let Some(index) = self.lib.list_selected {
                self.explore(index);
            }
        }
    }

    fn pump_store(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        self.flush_held();
        let pumped = self.lib.pump(cx);
        if pumped.uploaded {
            self.repaint_library = true;
            self.repaint_preview = true;
            self.repaint = true;
        }
        if let Some(lane) = pumped.rows_of {
            self.rows_arrived(lane);
            self.repaint_library = true;
            self.repaint = true;
        }
        for reply in pumped.others {
            match reply {
                StoreReply::Opened { reference, title, result } => match result {
                    Ok(opened) => {
                        self.status = match self.kept_label() {
                            Some(label) => format!("Opened “{title}” · “{label}” is kept in Cuts › Drafts"),
                            None => format!("Opened “{title}”"),
                        };
                        self.open_source_context(opened.source, Some(reference), true, opened.tree);
                    }
                    Err(error) => {
                        self.status = format!("Could not open “{title}”: {error}");
                        self.open_error = self.status.clone();
                        self.repaint = true;
                    }
                },
                StoreReply::Saved { result } => {
                    self.saving = false;
                    match result {
                        Ok(reference) => {
                            if let Some(session) = &mut self.session {
                                session.head = Some(reference);
                                session.saved = session.source.clone();
                            }
                            self.saved_at = Some(makepad_civil_time::local_now().hm());
                            self.save_error = None;
                            self.status = format!("Saved as a new revision of {}", reference.asset_id);
                            self.check_head_render();
                            self.refresh_library();
                        }
                        Err(error) => {
                            self.status = format!("Not saved: {error}");
                            self.save_error = Some(error);
                        }
                    }
                    self.repaint = true;
                }
                StoreReply::TreeSaved { result } => {
                    self.saving = false;
                    let submitted = self.saving_tree.take();
                    match result {
                        Ok(opened) => match opened.doc.into_cut() {
                            Ok(doc) => {
                                if let (Some(session), Some(submitted)) = (&mut self.session, submitted) {
                                    session.published(&submitted, opened.draft, opened.path, doc, opened.head);
                                    session.alias = Some(opened.alias);
                                }
                                // A folder cut saved as the person's copy is a library cut from here.
                                self.folder = None;
                                self.saved_at = Some(makepad_civil_time::local_now().hm());
                                self.save_error = None;
                                self.status = "Saved tree revision".into();
                                self.check_head_render();
                                self.refresh_library();
                            }
                            Err(error) => { self.status = format!("Not saved: {error}"); self.save_error = Some(error); }
                        },
                        Err(error) => { self.status = format!("Not saved: {error}"); self.save_error = Some(error); }
                    }
                    self.repaint = true;
                }
                StoreReply::TreeRestored { result } => match result {
                    Ok((opened, saved)) => {
                        let source = opened.draft.text(&opened.path).map(str::to_string);
                        let doc = opened.doc.clone().into_cut();
                        match (source, doc) {
                            (Ok(source), Ok(doc)) => {
                                self.finish(cx, ui, Pending::Open { source: source.clone(), head: Some(opened.head), saved: false, tree: Some(opened), folder: None }, Ok((source, doc, None)));
                                if let Some(session) = &mut self.session {
                                    let saved_doc = saved.doc.into_cut().expect("restored cut baseline was validated");
                                    session.saved = saved.draft.text(&saved.path).expect("restored cut path was validated").into();
                                    if session.tree.as_ref() != Some(&saved.draft) {
                                        session.undo.push(open_doc::Snapshot { path: saved.path.clone(), source: session.saved.clone(), doc: saved_doc.clone(), tree: Some(saved.draft.clone()) });
                                    }
                                    session.saved_path = saved.path;
                                    session.saved_tree = Some(saved.draft);
                                    session.saved_doc = Some(saved_doc);
                                }
                            }
                            (Err(error), _) | (_, Err(error)) => self.status = format!("Cannot reopen tree draft: {error}"),
                        }
                    }
                    Err(error) => { self.status = format!("Cannot reopen tree draft: {error}"); self.open_error = self.status.clone(); }
                },


                StoreReply::Render { purpose, source, result } => self.rendered(purpose, source, result),
                StoreReply::Dependents { reference, result } => {
                    // The explorer's "used by".
                    if self.detail_users.as_ref().is_some_and(|(asked, users)| *asked == reference && users.is_none()) {
                        self.detail_users = Some((reference, Some(result.map_or(0, |dto| dto.total))));
                        self.repaint_library = true;
                    }
                }
                StoreReply::RenderPublished { edit, result } => {
                    let doc = self.render.take().map(|render| render.doc);
                    match result {
                        Ok(render) => {
                            self.status = "Rendered to the library".into();
                            self.last_render = Some(render);
                            if let Some(doc) = doc {
                                let bytes = std::fs::metadata(self.render_dir(Some(&edit)).join(render::file_name(doc.output.alpha))).map_or(0, |meta| meta.len());
                                self.render_finished(format!("Rendered to the library · {}×{} · {} · {:.0} MB", doc.output.width, doc.output.height, timecode(doc.duration, doc.output.fps), bytes as f64 / 1e6));
                            }
                        }
                        Err(error) => {
                            self.status = format!("Render not published: {error}");
                            self.render_error(format!("The render finished but was not published: {error}"));
                        }
                    }
                    self.check_head_render();
                    self.refresh_library();
                    self.repaint = true;
                }
                StoreReply::Titles { titles, counts, recent } => {
                    for (id, title) in titles {
                        self.titles.entry(id).or_insert(title);
                    }
                    self.lib.counts = counts.into_iter().collect();
                    // Their thumbnails, for the blank page.
                    let refs = recent.iter().filter(|recent| !recent.poster_pending).map(|recent| recent.reference).filter(|reference| self.requested.insert(reference.asset_id.to_string())).collect::<Vec<_>>();
                    
                    if !refs.is_empty() {
                        self.send(StoreJob::Thumbs { refs });
                    }
                    self.recent = recent;
                    self.inspected = None;
                    self.repaint = true;
                }
                StoreReply::Details { reference, details } => {
                    self.details.insert(reference, details);
                    self.repaint_library = true;
                }
                StoreReply::Layout(text) => {
                    self.layout = DockLayout::parse(&text);
                    self.deck_layout_applied = false;
                    if self.deck_layout_suspended {
                        self.layout.apply(cx, ui, &[live_id!(cut_split), live_id!(cut_panel_split), live_id!(cut_side_split)]);
                    } else {
                        self.layout.apply(cx, ui, &[]);
                    }
                }
                StoreReply::Stopped => {
                    if !self.stopped {
                        self.status = "The cut library worker stopped; restart Stage to browse".into();
                        self.repaint = true;
                    }
                }
                // The DECK page's tree replies come through its own library worker.
                StoreReply::Rows { .. } | StoreReply::Thumb { .. } | StoreReply::CardSource { .. } | StoreReply::TreeOpened { .. } | StoreReply::TreeCompleted { .. } => {}
            }
        }
        // And one decoded filmstrip.
        if let Some((asset, frames)) = self.film_uploads.pop_front() {
            let film = frames.into_iter().map(|(t, thumb)| (t, Texture::new_with_format(cx, TextureFormat::VecBGRAu8_32 {
                width: thumb.width, height: thumb.height, data: Some((*thumb.pixels).clone()), updated: TextureUpdated::Full,
            }).label(cx, "edit/film-frame"))).collect::<Vec<_>>();
            self.films.insert(asset, Arc::new(film));
            self.repaint = true;
            self.repaint_library = true;
        }
        let _ = ui;
    }

    /// The Render button (cmd-E): cancel the render under way, else render
    /// the saved cut with the settings last used (the chat's Render… card
    /// sets them; a first render writes ~/Movies/<name>.mp4).
    fn render_button(&mut self) {
        if self.render.is_some() {
            self.cancel_render();
        } else {
            self.start_render();
        }
    }

    fn cancel_render(&mut self) {
        let Some(render) = &self.render else { return };
        if render.publishing {
            return;
        }
        if let Some(runner) = &mut self.runner {
            // The renderer removes its partial file and exits.
            runner.cancel();
        }
        let dir = render.dir.clone();
        self.render = None;
        self.render_ui.export_to = None;
        self.render_ui.cancelled = Some(dir);
        self.status = "Render cancelled".into();
        chat::note(self, "Render cancelled".to_string());
        self.repaint = true;
    }

    /// A render's folder: a library revision's, else the open folder cut's.
    fn render_dir(&self, edit: Option<&AssetRevisionRef>) -> std::path::PathBuf {
        let renders = makepad_ai_hub::home::makepad_home().join("stage-product").join("edit-renders");
        match edit {
            Some(edit) => renders.join(format!("{}-{}", edit.asset_id, &edit.revision.to_string()[5..17])),
            None => renders.join(format!("folder-{}", self.folder.as_ref().map_or("cut", |folder| folder.doc.name.as_str()))),
        }
    }

    /// Start a render of the saved cut (a library revision, or a folder
    /// cut's files): the renderer starts at once and reads every file where
    /// it lies.
    fn start_render(&mut self) {
        if self.render.is_some() {
            return;
        }
        let Some(session) = &self.session else { return };
        let saved = !session.dirty() && !motion_edit::has_drafts(self) && self.in_flight.is_none() && self.queue.is_empty() && !self.saving && !self.save_requested;
        let folder = self.folder.as_ref().map(|folder| folder.doc.dir.clone());
        if !saved || (session.head.is_none() && folder.is_none()) {
            self.status = "Save the cut first: a render is of a saved revision".into();
            self.repaint = true;
            return;
        }
        let edit = session.head;
        // A render is locked-time: live web streams are grabbed first (WB1).
        let streams = self.stream_blockers();
        if !streams.is_empty() {
            self.status = web_lane::stream_reason(streams.len());
            chat::note(self, self.status.clone());
            self.repaint = true;
            return;
        }
        let Some(session) = &self.session else { return };
        let doc = session.doc.clone();
        let alpha = doc.output.alpha;
        let dir = self.render_dir(edit.as_ref());
        self.render_failed = None;
        self.render_done = None;
        self.render_ui.exported = None;
        self.render_began = Cx::time_now();
        let source = session.source.clone();
        let tree = session.tree.clone().map(|tree| (tree, session.path.clone()));
        self.render_project = session.alias.as_ref().filter(|_| tree.is_some()).map(|alias| (alias.to_string(), session.path.clone()));
        self.last_render = None;
        self.render = Some(Rendering { edit, file: tree.as_ref().map(|(_, path)| path.clone()), source: source.clone(), doc, dir: dir.clone(), progress: (0, 0), publishing: false });
        // The last settings (the Render card's): the renderer draws the cut
        // with them; the file goes to the last path.
        let loudness = [None, Some(-14.0), Some(-16.0), Some(-23.0)][self.loudness.min(3)];
        self.render_ui.export_to = Some(self.export_path());
        let fps = self.render_ui.export_fps;
        let line = self.render.as_ref().map(|render| format!("Rendering “{}” · {} · {} fps", render.doc.label, if self.render_ui.export_draft { "draft" } else { "final" }, rates::label(fps.unwrap_or(render.doc.output.fps)))).unwrap_or_default();
        let bitrate = self.render.as_ref().and_then(|render| self.export_bitrate(render.doc.output.width, render.doc.output.height, fps.unwrap_or(render.doc.output.fps)));
        let job = RenderJob {
            cut: render::CutSource { source, tree, folder, saved: edit },
            out: dir.join(render::file_name(alpha)), dir, fps, loudness, burn: !self.burn_off,
            bitrate, blend: self.render_ui.export_blend, draft: self.render_ui.export_draft,
        };
        match self.runner.as_mut().ok_or("the render runner is not running".to_string()).and_then(|runner| runner.start(job)) {
            Ok(()) => {
                chat::note(self, line);
                self.status = "Rendering…".into();
            }
            Err(error) => {
                self.render = None;
                self.status = format!("Cannot render: {error}");
                self.render_error(error);
            }
        }
        self.repaint = true;
    }

    fn pump_render(&mut self, cx: &mut Cx) {
        let mut events = Vec::new();
        if let Some(runner) = &mut self.runner {
            while let Some(event) = runner.try_recv() {
                events.push(event);
            }
        }
        for event in events {
            match event {
                RenderEvent::Progress { frame, frames } => {
                    if let Some(render) = &mut self.render {
                        render.progress = (frame, frames);
                    }
                    self.status = format!("Rendering frame {frame} / {frames}");
                }
                // A cancelled export ended anyway: its files go.
                RenderEvent::Done if self.render.is_none() => self.remove_cancelled(),
                RenderEvent::Done => {
                    // The file first; the library only when asked.
                    let dir = self.render.as_ref().map(|render| render.dir.clone()).unwrap_or_default();
                    if std::mem::take(&mut self.render_ui.api_file) {
                        // A control-API render: the render folder's file is
                        // the result.
                        let doc = self.render.take().map(|render| render.doc);
                        let file = dir.join(render::file_name(doc.as_ref().is_some_and(|doc| doc.output.alpha))).display().to_string();
                        self.render_finished(match doc {
                            Some(doc) => format!("Rendered · {}×{} · {} · {file}", doc.output.width, doc.output.height, timecode(doc.duration, doc.output.fps)),
                            None => format!("Rendered · {file}"),
                        });
                        self.render_ui.exported = Some(std::path::PathBuf::from(&file));
                        self.render_ui.last_file = Some(file.clone());
                        self.status = format!("Rendered: {file}");
                        self.repaint = true;
                        continue;
                    }
                    if !self.export_finished(&dir) {
                        self.status = "Writing the file…".into();
                        self.repaint = true;
                        continue;
                    }
                    let revision = self.session.as_ref().map_or(0, |session| session.revision);
                    let poster = match &self.saved_thumb {
                        Some((of, thumb)) if *of == revision => thumb.clone(),
                        _ => self.compose_thumbnail(cx),
                    };
                    let Some(render) = &mut self.render else { continue };
                    let doc = render.doc.clone();
                    let fps = makepad_story_edit_render::audio::fps_ratio(doc.output.fps);
                    let dir = render.dir.clone();
                    let result = makepad_story_edit_render::RenderResult {
                        path: dir.join(render::file_name(doc.output.alpha)),
                        cut_source: makepad_asset_data::BlobId::hash_of(render.source.as_bytes()),
                        frames: makepad_story_edit_render::audio::frame_count(doc.duration, fps),
                        size: (doc.output.width, doc.output.height),
                        fps,
                        duration_ms: (doc.duration * 1000.0).round() as u32,
                        audio: true,
                        frame_hashes: Vec::new(),
                        poster: Some(((*poster.pixels).clone(), poster.width as u32, poster.height as u32)),
                        sidecars: Vec::new(),
                    };
                    // Only a library cut's render is published (a folder cut's is its file).
                    let Some(edit) = render.edit else { continue };
                    render.publishing = true;
                    let (title, path) = (doc.label.clone(), render.file.clone());
                    self.send(StoreJob::PublishRender { edit, path, render: Box::new(result), title });
                    self.status = "Publishing the render…".into();
                }
                RenderEvent::Failed(_) if self.render.is_none() => self.remove_cancelled(),
                RenderEvent::Failed(error) => {
                    self.render = None;
                    self.status = format!("Render failed: {}", preview::plain_reason(&error));
                    self.render_error(error);
                }
                RenderEvent::Started(_) => {}
            }
            self.repaint = true;
        }
    }

    fn pump_media(&mut self) {
        self.flush_held();
        let mut replies = Vec::new();
        if let Some(media) = &self.media {
            for _ in 0..4 {
                match media.try_recv() {
                    Some(reply) => replies.push(reply),
                    None => break,
                }
            }
        }
        for reply in replies {
            match &reply {
                MediaReply::Peaks { asset, .. } => {
                    self.card_media.remove(&format!("p:{asset}"));
                }
                MediaReply::Film { asset, .. } => {
                    self.card_media.remove(&format!("f:{asset}"));
                }
                _ => {}
            }
            match reply {
                MediaReply::Peaks { asset, result } => match result {
                    Ok(peaks) => {
                        self.peaks.insert(asset, peaks);
                        self.repaint = true;
                        self.repaint_library = true;
                    }
                    Err(error) => log!("edit: no waveform for {asset}: {error}"),
                },
                MediaReply::Local { asset, result } => match result {
                    Ok(path) => self.local_files.push((asset, path)),
                    Err(error) => {
                        self.lib.status = format!("Cannot play it: {error}");
                        self.asset_play = None;
                        self.repaint = true;
                    }
                },
                MediaReply::Film { asset, result } => match result {
                    Ok((frames, info)) => {
                        self.film_info.insert(asset.clone(), info);
                        self.film_uploads.push_back((asset, frames));
                    }
                    Err(error) => log!("edit: no filmstrip for {asset}: {error}"),
                },
                MediaReply::Voice { node, index, text, result } => self.spoken(node, index, text, result),
                MediaReply::Voices(voices) => {
                    self.voices = voices;
                    self.inspected = None;
                    self.repaint = true;
                }
            }
        }
    }

    fn rendered(&mut self, purpose: RenderPurpose, source: AssetRevisionRef, result: Result<RenderState, String>) {
        if let RenderPurpose::Score { start, track } = purpose {
            self.score_rendered(source, start, track, result);
            return;
        }
        let RenderPurpose::Clip { start, track } = purpose else {
            // The open cut's render.
            if self.session.as_ref().and_then(|session| session.head).is_some_and(|head| head.asset_id == source.asset_id) {
                self.head_render = result.ok();
                self.repaint = true;
            }
            return;
        };
        match result {
            Ok(state) => match state.render() {
                Some(render) => {
                    let seconds = self.lib.rows.iter().find(|row| row.reference == source).and_then(|row| row.seconds);
                    let media = Media { kind: MediaKind::AppRun, src: MediaRef::Run(run_ref(&source, &render)), seconds };
                    self.op(CutOp::Add { media, start, track });
                }
                None => self.status = "That app run has no render yet; run it first".into(),
            },
            Err(error) => self.status = format!("App run: {error}"),
        }
        self.repaint = true;
    }

    /// A score dropped on the timeline: its latest render (EDIT keeps
    /// renders, never scores, so a cut renders without a synthesizer).
    /// An older render still goes in, and the status says so.
    fn score_rendered(&mut self, source: AssetRevisionRef, start: f64, track: Option<u32>, result: Result<RenderState, String>) {
        let title = self.lib.rows.iter().find(|row| row.reference == source).map_or("The score".to_string(), |row| format!("“{}”", row.title));
        match result {
            Ok(state) => match (state, state.render()) {
                (_, None) => self.status = format!("{title} has no render yet: render it in SCORE, then drop it again"),
                (state, Some(render)) => {
                    let media = Media { kind: MediaKind::Audio, src: MediaRef::Asset(ids(&render)), seconds: None };
                    self.op(CutOp::Add { media, start, track });
                    self.status = match state {
                        RenderState::Stale(_) => format!("{title}: its latest render is of an older version; render it again in SCORE to update"),
                        _ => format!("{title} added (its render)"),
                    };
                }
            },
            Err(error) => self.status = format!("Score: {error}"),
        }
        self.status_sticky = true;
        self.repaint = true;
    }

    /// The media a store row adds, if it is media.
    fn row_media(&self, index: usize) -> Option<(MediaKind, &LibRow)> {
        let kind = match self.lib.lane? {
            PageLane::Store(LibLane::Video) | PageLane::Web => MediaKind::Video,
            PageLane::Store(LibLane::Image) => MediaKind::Image,
            // A score plays as its latest render (found at the drop).
            PageLane::Store(LibLane::Audio | LibLane::Scores) => MediaKind::Audio,
            PageLane::Store(LibLane::AppRuns) => MediaKind::AppRun,
            PageLane::Store(LibLane::Cuts) => MediaKind::Cut,
            _ => return None,
        };
        Some((kind, self.lib.rows.get(index)?))
    }

    /// Where a dragged or added row would go at `t` on `(lane, track)`, as
    /// the op to run (`None` for an app run, whose render comes first). The
    /// track under the drop picks the node: a picture track takes a `Clip`
    /// (video, app run, a nested cut) or a `Still`, a sound track an
    /// `AudioClip` (of a sound, or a video's soundtrack).
    fn plan_media(&self, index: usize, t: f64, lane_track: Option<(Lane, u32)>) -> Result<(DropHint, Option<CutOp>), String> {
        let session = self.session.as_ref().ok_or("Start or open a cut first")?;
        let doc = &session.doc;
        let fps = doc.output.fps.max(1.0);
        let t = (t * fps).round() / fps;
        if let Some(why) = self.web_drop_refusal(index) {
            return Err(why);
        }
        let (kind, row) = self.row_media(index).ok_or("Choose a library item")?;
        let over = lane_track.map(|(lane, _)| lane);
        let (kind, lane) = match (kind, over) {
            (MediaKind::Video, Some(Lane::Audio)) => (MediaKind::Audio, Lane::Audio),
            (MediaKind::Audio, _) => (MediaKind::Audio, Lane::Audio),
            (MediaKind::Image, Some(Lane::Audio)) => return Err("An image goes on a video track".into()),
            (MediaKind::AppRun, Some(Lane::Audio)) => return Err("An app run goes on a video track".into()),
            (MediaKind::Cut, Some(Lane::Audio)) => return Err("A nested cut goes on a video track; its sound comes with it".into()),
            (kind, _) => (kind, Lane::Video),
        };
        if kind == MediaKind::Cut && session.head.is_some_and(|head| head.asset_id == row.reference.asset_id) {
            return Err("A cut cannot contain itself".into());
        }
        let length = row.seconds.unwrap_or(match kind { MediaKind::Image => 4.0, MediaKind::Audio => 10.0, _ => 5.0 });
        let preferred = lane_track.filter(|(hit_lane, _)| *hit_lane == lane).map(|(_, track)| track);
        let track = free_track(doc, lane, t, t + length, preferred, None);
        let hint = DropHint { lane, track, start: t, length, targets: Vec::new(), label: row.title.clone(), invalid: None, tab: false };
        // App runs and scores add their render, found first.
        let score = self.lib.lane == Some(PageLane::Store(LibLane::Scores));
        let op = (kind != MediaKind::AppRun && !score).then(|| CutOp::Add { media: Media { kind, src: MediaRef::Asset(ids(&row.reference)), seconds: row.seconds }, start: t, track: Some(track) });
        Ok((hint, op))
    }

    /// An effect or transition preset on the clip `block` at `t`.
    fn plan_preset(&self, preset: &str, transition: bool, t: f64, block: Option<&str>) -> Result<CutOp, String> {
        let session = self.session.as_ref().ok_or("Start or open a cut first")?;
        let doc = &session.doc;
        let video_block = |id: &str| doc.node(id).filter(|node| node.span.is_some() && matches!(node.kind, CutNodeKind::Clip { .. } | CutNodeKind::Still { .. } | CutNodeKind::Title { .. } | CutNodeKind::Fx { .. })).cloned();
        if !transition {
            let node = block.and_then(video_block).ok_or("Click a video clip for the effect")?;
            return Ok(CutOp::Effect { node: node.id.clone(), effect: preset.to_string() });
        }
        let a = block.and_then(|id| ops::timed_block(doc, id).or_else(|| video_block(id))).filter(|node| !matches!(node.kind, CutNodeKind::Fx { .. })).ok_or("Click where two video clips overlap")?;
        let span_a = a.span.unwrap();
        // A clip plays in one transition at a time.
        let free = |id: &str| ops::joined(doc, id).is_empty();
        if !free(&a.id) {
            return Err("That clip is already in a transition".into());
        }
        // The other clip overlapping this one on another track, the one
        // under `t` first.
        let mut others = ops::video_blocks(doc).into_iter().filter(|node| node.id != a.id && node.track != a.track)
            .filter_map(|node| { let over = overlap(span_a, node.span?)?; Some((node, over)) })
            .collect::<Vec<_>>();
        if !others.is_empty() && others.iter().all(|(node, _)| !free(&node.id)) {
            return Err("The clip it overlaps is already in a transition".into());
        }
        others.retain(|(node, _)| free(&node.id));
        others.sort_by_key(|(_, over)| if t >= over.start && t < over.end() { 0 } else { 1 });
        let (b, _) = others.first().ok_or("That clip overlaps no other video clip; drag one over it first")?;
        Ok(CutOp::Transition { a: a.id.clone(), b: b.id.clone(), effect: preset.to_string() })
    }

    fn drop_row(&mut self, index: usize, t: f64, lane_track: Option<(Lane, u32)>) {
        match self.plan_media(index, t, lane_track) {
            Ok((_, Some(op))) => self.op(op),
            Ok((hint, None)) => {
                // An app run or a score: its render comes first.
                if let Some((_, row)) = self.row_media(index) {
                    let source = row.reference;
                    let score = self.lib.lane == Some(PageLane::Store(LibLane::Scores));
                    let purpose = if score { RenderPurpose::Score { start: hint.start, track: Some(hint.track) } } else { RenderPurpose::Clip { start: hint.start, track: Some(hint.track) } };
                    self.send(StoreJob::RenderOf { purpose, source, path: None });
                    self.status = if score { "Finding the score's render…".into() } else { "Finding the app run's render…".into() };
                }
            }
            Err(error) => {
                let mut chars = error.chars();
                self.status = format!("Not added: {}", chars.next().map(|first| first.to_lowercase().chain(chars).collect::<String>()).unwrap_or_default());
                self.status_sticky = true;
            }
        }
        self.repaint = true;
    }

    /// A card clicked: it is selected and the explorer shows it (a sound's
    /// waveform, who uses it).
    fn click_row(&mut self, index: usize) {
        // A deck opens on the DECK page: the product switches there.
        if self.lib.lane == Some(PageLane::Store(LibLane::Decks)) {
            if let Some(row) = self.lib.rows.get(index) {
                self.deck_request = Some((row.reference, row.title.clone()));
            }
            return;
        }
        self.lib.list_selected = Some(index);
        self.explore(index);
        self.repaint = true;
        self.repaint_library = true;
    }

    /// The explorer shows row `index`: a sound's waveform, who uses it.
    fn explore(&mut self, index: usize) {
        if let Some(row) = self.lib.rows.get(index).cloned() {
            if self.lib.lane == Some(PageLane::Store(LibLane::Audio)) {
                let asset = ids(&row.reference);
                if self.peaks_asked.insert(asset.id.clone()) {
                    self.send_media(MediaJob::Peaks { media: MediaRef::Asset(asset) });
                }
            }
            if self.details_asked.insert(row.reference) {
                self.send(StoreJob::Details { reference: row.reference, edit: self.lib.lane == Some(PageLane::Store(LibLane::Cuts)), tree: row.tree.clone() });
            }
            if self.detail_users.as_ref().is_none_or(|(asked, _)| *asked != row.reference) {
                self.detail_users = Some((row.reference, None));
                self.send(StoreJob::Dependents { reference: row.reference });
            }
        }
    }

    /// Open the cut a compound clip nests; the bar shows the way back.
    fn open_nested(&mut self, node: &str) {
        let Some(session) = &self.session else { return };
        let Some(edit) = session.doc.node(node).and_then(|node| node.kind.nested_edit()).cloned() else { return };
        if session.dirty() {
            self.status = "Save this cut first, then open the one it nests".into();
            self.repaint = true;
            return;
        }
        let Some(reference) = exact(&edit) else { return };
        let title = self.titles.get(&edit.id).cloned().unwrap_or_else(|| "the nested cut".into());
        self.crumbs.push((session.doc.label.clone(), session.head));
        self.status = format!("Opening “{title}”…");
        self.send(StoreJob::Open { reference, title, kind: AssetKind::Cut, tree: None });
        self.repaint = true;
    }

    /// Back to the cut a nested one was opened from.
    fn open_parent(&mut self) {
        let Some((title, head)) = self.crumbs.pop() else { return };
        match head {
            Some(reference) => {
                self.status = format!("Opening “{title}”…");
                self.send(StoreJob::Open { reference, title, kind: AssetKind::Cut, tree: None });
            }
            None => self.status = "The parent edit was never saved".into(),
        }
        self.repaint = true;
    }

    /// Open the cut a row is.
    fn open_row(&mut self, index: usize) {
        if self.lib.lane != Some(PageLane::Store(LibLane::Cuts)) {
            return;
        }
        if let Some(row) = self.lib.rows.get(index).cloned() {
            self.crumbs.clear();
            // A cut with unsaved changes (EDITED) opens as its draft.
            let draft_id = row.asset_id().to_string();
            let draft = self.edited(&draft_id).then(|| drafts::read(&drafts::dir(), &draft_id)).flatten();
            if let Some(draft) = draft {
                self.status = format!("Opened “{}” with its unsaved changes", row.title);
                self.reopen_draft(draft);
                return;
            }
            self.status = format!("Opening “{}”…", row.title);
            self.send(StoreJob::Open { reference: row.reference, title: row.title, kind: AssetKind::Cut, tree: row.tree });
        }
    }

    /// An effect or transition chosen, dragged or dropped: from the
    /// picker's tiles, or from the library's list mode (the same actions).
    fn picker_action(&mut self, cx: &mut Cx, ui: &WidgetRef, action: makepad_content_packs::picker::PickerAction) {
        match action {
            makepad_content_packs::picker::PickerAction::Choose { preset, title, category, .. } => {
                let transition = category == "transition";
                // With a clip selected, an effect goes on it at once.
                let selected = self.selected.clone();
                let placed = if transition {
                    self.transition_on_selected(&preset)
                } else {
                    self.armed = Some((preset.clone(), transition));
                    selected.is_some_and(|selected| self.place_armed(&selected, self.playhead))
                };
                if !placed {
                    self.armed = Some((preset, transition));
                    self.status = format!("{title} armed — click {} · Esc", if transition { "the cut between two clips" } else { "the clip it goes on" });
                }
                self.repaint = true;
            }
            // Dragged from the tiles onto the timeline (Final Cut's way).
            makepad_content_packs::picker::PickerAction::Drag { preset, title, category, abs } => {
                let timeline = ui.widget(cx, ids!(cut_timeline));
                let hit = timeline.borrow::<CutTimeline>().and_then(|timeline| timeline.hit(abs).map(|hit| (hit, 10.0 / timeline.px_per_sec().max(1e-3))));
                let hint = hit.map(|(hit, tolerance)| self.preset_hint(&preset, category == "transition", &title, &hit, tolerance));
                if let Some(mut timeline) = timeline.borrow_mut::<CutTimeline>() {
                    timeline.set_drop_hint(cx, hint);
                };
            }
            makepad_content_packs::picker::PickerAction::Drop { preset, title, category, abs, why } => {
                let timeline = ui.widget(cx, ids!(cut_timeline));
                let hit = timeline.borrow::<CutTimeline>().and_then(|timeline| timeline.hit(abs).map(|hit| (hit, 10.0 / timeline.px_per_sec().max(1e-3))));
                if let Some(mut timeline) = timeline.borrow_mut::<CutTimeline>() {
                    timeline.set_drop_hint(cx, None);
                    timeline.focus(cx);
                };
                let transition = category == "transition";
                match hit {
                    // An unavailable effect is let go like any other, and
                    // refused here.
                    _ if why.is_some() => self.status = format!("Not added: {title}: {}", why.unwrap_or_default()),
                    None => self.status = format!("Not added: {title} was dropped outside the timeline"),
                    Some((hit, tolerance)) if transition => match self.plan_transition_at(&preset, hit.t, hit.track, hit.block.as_deref(), tolerance) {
                        Ok(plan) => self.apply_cut_plan(plan, &preset),
                        Err(error) => self.status = format!("Not added: {error}"),
                    },
                    Some((hit, _)) => match self.plan_preset(&preset, false, hit.t, hit.block.as_deref().map(|block| split_selection(block).0)) {
                        Ok(op) => {
                            self.op(op);
                            self.status = format!("Effect {title}");
                            self.status_sticky = true;
                        }
                        Err(error) => self.status = format!("Not added: {error}"),
                    },
                }
                self.repaint = true;
            }
            _ => {}
        }
    }

    /// Apply the armed effect or transition to the clip clicked at `t`.
    fn place_armed(&mut self, node: &str, t: f64) -> bool {
        let Some((preset, transition)) = self.armed.clone() else { return false };
        if transition {
            let tolerance = 0.25_f64.max(self.duration() * 0.01);
            match self.plan_transition_at(&preset, t, 0, Some(node), tolerance) {
                Ok(plan) => {
                    self.armed = None;
                    self.apply_cut_plan(plan, &preset);
                }
                Err(error) => self.status = error,
            }
            self.repaint = true;
            return true;
        }
        match self.plan_preset(&preset, transition, t, Some(split_selection(node).0)) {
            Ok(op) => {
                self.op(op);
                self.armed = None;
                self.status = format!("{} {}", if transition { "Transition" } else { "Effect" }, preset_label(&preset));
            }
            Err(error) => self.status = error,
        }
        self.repaint = true;
        true
    }

    fn save(&mut self, cx: &mut Cx) {
        if self.saving { return; }
        if !self.save_requested && self.session.as_ref().is_some_and(|session| session.tree.is_some()) {
            if let Err(error) = motion_edit::save_sources(self) {
                self.status = format!("Not saved: {error}");
                self.save_error = Some(error);
                self.repaint = true;
                return;
            }
        }
        if self.in_flight.is_some() || self.queue.front().is_some_and(|pending| !matches!(pending, Pending::Open { .. })) {
            self.save_requested = true;
            self.status = "Applying edits before saving the tree".into();
            return;
        }
        self.save_requested = false;
        if self.folder.as_ref().is_some_and(|folder| folder.doc.writable) {
            self.save_in_place(true);
            return;
        }
        let Some(session) = &self.session else { return };
        if session.tree.is_none() && session.doc.graphics.is_empty() && session.doc.generates.is_empty()
            && session.doc.nodes.iter().all(|node| node.span.is_none() && !matches!(node.kind, CutNodeKind::VoiceOver { .. })) {
            self.status = "Nothing to save yet: add a clip first".into();
            return;
        }
        // The renderer draws the thumbnail (nests, effects, titles and
        // all); the save goes out when it is in (review 3, C3).
        let (doc, revision) = self.session.as_ref().map(|session| (session.doc.clone(), session.revision)).unwrap();
        self.pending_thumbnail = Some((self.cut_poster(cx, doc), revision));
        self.saving = true;
        self.status = "Saving…".into();
        self.repaint = true;
    }

    /// The cut's settled final frame, rendered once at the save picture's size.
    fn cut_poster(&self, cx: &mut Cx, doc: Arc<CutDoc>) -> CutThumbnail {
        let t = doc.duration; // The thumbnail chooses the final valid frame.
        let job_output = (doc.output.width as f64, doc.output.height as f64);
        
        let job = CutJob { doc, marks: HashMap::new(), resolver: Box::new(store::LazyStoreMedia::new()), audio: false };
        let options = makepad_story_edit_render::engine::EngineOptions { max_loads: 4, budget: None };
        CutThumbnail::with_options(cx, job, t, thumb_width(job_output), options)
    }

    /// The save, with its thumbnail (the renderer's, else the sketch's).
    fn finish_save(&mut self, cx: &mut Cx, rendered: Option<Thumb>) {
        let settled = rendered.is_some();
        let thumbnail = rendered.unwrap_or_else(|| self.compose_thumbnail(cx));
        let Some(session) = self.session.as_ref() else { return };
        self.saved_thumb = Some((session.revision, thumbnail.clone()));
        if let Some(tree) = &session.tree {
            // A read-only folder cut's first save is the person's own copy in the store.
            let tree = if self.folder.is_some() { folder_docs::for_store(tree) } else { tree.clone() };
            let request = store::TreeSave { draft: tree.clone(), head: session.head, alias: session.alias.clone(), thumbnail, path: session.path.clone(), settled, dpi: 1.0 };
            self.saving_tree = Some(tree.clone());
            self.send(StoreJob::SaveTree(Box::new(request)));
            return;
        }
        let request = SaveRequest {
            kind: AssetKind::Cut,
            source: session.source.clone(),
            title: session.doc.label.clone(),
            deps: dependencies(&session.doc),
            asset: session.head.map(|head| head.asset_id),
            thumbnail,
            alias: None,
        };
        self.send(StoreJob::Save(Box::new(request)));
    }

    /// The cut's picture at the playhead (or its first clip), from the
    /// thumbnails, for the saved revision's preview image.
    fn compose_thumbnail(&self, _cx: &mut Cx) -> Thumb {
        let Some(session) = &self.session else { return Thumb { width: 256, height: 256, pixels: Arc::new(vec![0xff10_1010; 256 * 256]) } };
        let doc = &session.doc;
        let (ow, oh) = (doc.output.width as f64, doc.output.height as f64);
        let mut scale = THUMB_LONG / ow.max(oh);
        if ow.min(oh) * scale < THUMB_SHORT_MIN {
            scale = THUMB_SHORT_MIN / ow.min(oh);
        }
        let (w, h) = ((ow * scale).round() as usize, (oh * scale).round() as usize);
        let mut pixels = vec![0xff10_1010u32; w * h];
        let (eval, sizes) = self.eval(doc);
        let first = doc.nodes.iter().filter(|node| ops::lane_of(node) == Some(Lane::Video)).filter_map(|node| node.span).map(|span| span.start + span.length * 0.25).fold(f64::INFINITY, f64::min);
        let mut items = sketch(&eval.frame_plan(self.playhead), &|media| sizes.get(media).copied());
        let pictured = |items: &[project::SketchItem]| items.iter().any(|item| matches!(item, project::SketchItem::Picture { .. }));
        if !pictured(&items) && first.is_finite() {
            items = sketch(&eval.frame_plan(first), &|media| sizes.get(media).copied());
        }
        // A compound edit: the sketch cannot see into a nest, so the first
        // nested cut's (or leaf clip's) saved thumbnail fills the frame
        // (review 3, C3).
        if !pictured(&items) {
            let nested = doc.nodes.iter().filter(|node| node.span.is_some())
                .filter_map(|node| match &node.kind {
                    CutNodeKind::Clip { src, .. } | CutNodeKind::Still { src, .. } => Some((node.span.map_or(0.0, |span| span.start), src.clone())),
                    _ => None,
                })
                .filter(|(_, src)| self.lib.thumbs.contains_key(media_id(src)))
                .min_by(|a, b| a.0.total_cmp(&b.0));
            if let Some((_, src)) = nested {
                let (tw, th) = self.lib.thumbs.get(media_id(&src)).map_or((1, 1), |(thumb, _)| (thumb.width, thumb.height));
                let aspect = tw as f64 / th.max(1) as f64;
                let (fw, fh) = if aspect > ow / oh { (ow, ow / aspect) } else { (oh * aspect, oh) };
                items = vec![project::SketchItem::Picture { node: String::new(), media: src, dst: [(ow - fw) * 0.5, (oh - fh) * 0.5, fw, fh], uv: [0.0, 0.0, 1.0, 1.0], opacity: 1.0, still: true, source_time: None }];
            }
        }
        for item in items {
            let project::SketchItem::Picture { media, dst, uv, opacity, .. } = item else { continue };
            let Some((thumb, _)) = self.lib.thumbs.get(media_id(&media)) else { continue };
            let (x0, y0) = ((dst[0] * scale).max(0.0) as usize, (dst[1] * scale).max(0.0) as usize);
            let (x1, y1) = (((dst[0] + dst[2]) * scale).min(w as f64) as usize, ((dst[1] + dst[3]) * scale).min(h as f64) as usize);
            let alpha = (opacity.clamp(0.0, 1.0) * 255.0) as u32;
            for y in y0..y1 {
                let v = uv[1] + (uv[3] - uv[1]) * ((y as f64 / scale - dst[1]) / dst[3]);
                let sy = ((v * thumb.height as f64) as usize).min(thumb.height - 1);
                for x in x0..x1 {
                    let u = uv[0] + (uv[2] - uv[0]) * ((x as f64 / scale - dst[0]) / dst[2]);
                    let sx = ((u * thumb.width as f64) as usize).min(thumb.width - 1);
                    let (s, d) = (thumb.pixels[sy * thumb.width + sx], pixels[y * w + x]);
                    let mix = |s: u32, d: u32| (s * alpha + d * (255 - alpha)) / 255;
                    pixels[y * w + x] = 0xff00_0000 | (mix((s >> 16) & 0xff, (d >> 16) & 0xff) << 16) | (mix((s >> 8) & 0xff, (d >> 8) & 0xff) << 8) | mix(s & 0xff, d & 0xff);
                }
            }
        }
        Thumb { width: w, height: h, pixels: Arc::new(pixels) }
    }

    /// An evaluator knowing every source's size from its thumbnail's aspect
    /// (at the output's height) and the takes this session made, and those
    /// sizes.
    fn eval<'a>(&self, doc: &'a CutDoc) -> (CutEval<'a>, HashMap<MediaRef, (u32, u32)>) {
        let mut eval = CutEval::new(doc);
        let mut sizes = HashMap::new();
        for node in &doc.nodes {
            let media = match &node.kind {
                CutNodeKind::Clip { src, .. } | CutNodeKind::Still { src, .. } => src,
                _ => continue,
            };
            if let Some((thumb, _)) = self.lib.thumbs.get(media_id(media)) {
                let height = doc.output.height.max(1);
                let width = ((thumb.width as f64 / thumb.height.max(1) as f64) * height as f64).round().max(1.0) as u32;
                eval.set_source_size(media.clone(), width, height);
                sizes.insert(media.clone(), (width, height));
            }
        }
        for (take, info) in &self.takes {
            eval.set_take(take.clone(), info.clone());
        }
        (eval, sizes)
    }

    /// B: cut the selected clip at the playhead, else the topmost clip
    /// under it.
    fn blade(&mut self) {
        let Some(session) = &self.session else { return };
        let t = self.playhead;
        let under = |id: &str| session.doc.node(id).filter(|node| node.span.is_some_and(|span| span.contains(t)) && matches!(node.kind, CutNodeKind::Clip { .. } | CutNodeKind::Still { .. } | CutNodeKind::AudioClip { .. } | CutNodeKind::Title { .. }));
        let target = self.selected.as_deref().and_then(|selected| under(split_selection(selected).0)).or_else(|| {
            session.doc.nodes.iter().filter(|node| under(&node.id).is_some()).max_by_key(|node| (ops::lane_of(node) == Some(Lane::Video), node.track))
        });
        match target {
            Some(node) => {
                let node = node.id.clone();
                self.op(CutOp::Split { node, t });
            }
            None => self.status = "No clip under the playhead to cut".into(),
        }
        self.repaint = true;
    }

    fn now_t(&self) -> f64 {
        match self.playing {
            Some(playing) => playing.from + (Cx::time_now() - playing.since) * playing.rate,
            None => self.playhead,
        }
    }

    /// Play at `rate` (negative: backwards), from where the playhead is.
    fn shuttle(&mut self, rate: f64) {
        self.playhead = self.now_t().clamp(0.0, self.duration());
        self.playing = (rate != 0.0).then(|| Playing { since: Cx::time_now(), from: self.playhead, rate });
        self.repaint = true;
        self.repaint_preview = true;
    }

    fn seek(&mut self, t: f64) {
        self.playhead = t.clamp(0.0, self.duration());
        if let Some(playing) = self.playing {
            self.playing = Some(Playing { since: Cx::time_now(), from: self.playhead, rate: playing.rate });
        }
        self.repaint_preview = true;
        self.repaint = true;
    }

    fn delete_selected(&mut self) {
        for node in std::mem::take(&mut self.also) {
            if split_selection(&node).1.is_none() {
                self.op(CutOp::Delete { node });
            }
        }
        let Some(selected) = self.selected.clone() else { return };
        match split_selection(&selected) {
            (node, Some(index)) if self.is_voice(node) => self.remove_line(node, index),
            (node, _) => {
                let node = node.to_string();
                self.op(CutOp::Delete { node });
            }
        }
    }

    fn key(&mut self, cx: &mut Cx, ui: &WidgetRef, key: &KeyEvent) {
        let cmd = key.modifiers.logo || key.modifiers.control;
        let alt = key.modifiers.alt;
        let rate = self.playing.map_or(0.0, |playing| playing.rate);
        let frame = 1.0 / self.fps().max(1.0);
        match key.key_code {
            KeyCode::Space => self.shuttle(if rate != 0.0 { 0.0 } else { 1.0 }),
            KeyCode::KeyK => self.shuttle(0.0),
            KeyCode::KeyL => self.shuttle(if rate > 0.0 { (rate * 2.0).min(MAX_SHUTTLE) } else { 1.0 }),
            KeyCode::KeyJ => self.shuttle(if rate < 0.0 { (rate * 2.0).max(-MAX_SHUTTLE) } else { -1.0 }),
            KeyCode::KeyI => self.mark(true, alt),
            KeyCode::KeyO => self.mark(false, alt),
            KeyCode::KeyB => self.blade(),
            KeyCode::KeyE if cmd => self.render_button(),
            KeyCode::KeyE => self.add_chosen(),
            KeyCode::KeyS if cmd => self.save(cx),
            KeyCode::KeyS => self.skim_off = !self.skim_off,
            KeyCode::KeyT if cmd => self.add_default_transitions(),
            // ⌃R: generate the selection's takes (in/out), ⇧⌃R: generate all.
            KeyCode::KeyR if key.modifiers.control && !key.modifiers.logo => self.generate_selection(key.modifiers.shift),
            KeyCode::KeyT => self.op(CutOp::AddTitle { start: self.playhead, text: "Title".into() }),
            KeyCode::KeyV => self.add_voice_line(),
            KeyCode::KeyM => self.add_marker(),
            KeyCode::Slash if key.modifiers.shift => self.keys_sheet = !self.keys_sheet,
            KeyCode::KeyN => self.snap_off = !self.snap_off,
            KeyCode::Quote => self.safe_zones = !self.safe_zones,
            KeyCode::KeyZ if cmd => self.undo(key.modifiers.shift),
            KeyCode::KeyZ if key.modifiers.shift => self.fit_after_paint(cx, ui),
            KeyCode::Escape => {
                if self.armed.take().is_none() && !std::mem::take(&mut self.keys_sheet) && !std::mem::take(&mut self.new_menu) {
                    self.selected = None;
                }
            }
            KeyCode::Delete | KeyCode::Backspace => self.delete_selected(),
            KeyCode::ArrowLeft => self.seek(self.playhead - frame * if key.modifiers.shift { 10.0 } else { 1.0 }),
            KeyCode::ArrowRight => self.seek(self.playhead + frame * if key.modifiers.shift { 10.0 } else { 1.0 }),
            KeyCode::Home => self.seek(0.0),
            KeyCode::End => self.seek(self.duration()),
            KeyCode::Equals | KeyCode::NumpadAdd | KeyCode::Minus | KeyCode::NumpadSubtract => {
                let factor = if matches!(key.key_code, KeyCode::Equals | KeyCode::NumpadAdd) { 1.5 } else { 1.0 / 1.5 };
                self.zoom_by(cx, ui, factor);
            }
            _ => {}
        }
        self.repaint = true;
    }

    fn zoom_by(&mut self, cx: &mut Cx, ui: &WidgetRef, factor: f64) {
        if let Some(mut timeline) = ui.widget(cx, ids!(cut_timeline)).borrow_mut::<CutTimeline>() {
            let zoom = timeline.zoom(cx, factor);
            ui.slider(cx, ids!(cut_zoom)).set_value(cx, zoom_to_slider(zoom));
        }
    }

    /// I / O: mark in or out at the playhead; with alt, clear it.
    fn mark(&mut self, in_point: bool, clear: bool) {
        let value = (!clear).then_some(self.playhead);
        if in_point { self.marks.0 = value } else { self.marks.1 = value }
        self.repaint = true;
    }

    /// M: a marker at the playhead (again on one removes it).
    fn add_marker(&mut self) {
        let half = 0.5 / self.fps().max(1.0);
        match self.markers.iter().position(|marker| (marker - self.playhead).abs() <= half) {
            Some(index) => {
                self.markers.remove(index);
            }
            None => {
                self.markers.push(self.playhead);
                self.markers.sort_by(f64::total_cmp);
            }
        }
        self.repaint = true;
    }

    /// E: the chosen library item at the playhead.
    fn add_chosen(&mut self) {
        match self.lib.list_selected {
            Some(index) => self.drop_row(index, self.playhead, None),
            None => self.status = "Choose a library item first".into(),
        }
        self.repaint = true;
    }

    /// A block dragged on the timeline: a voiceover line moves its `at`,
    /// everything else its span and track.
    fn moved(&mut self, node: String, start: f64, track: u32) {
        match split_selection(&node) {
            (owner, Some(index)) if self.is_voice(owner) => self.move_line(owner, index, start),
            _ => self.op(CutOp::Move { node, start, track }),
        }
    }

    pub(super) fn handle_actions(&mut self, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
        self.handle_web_actions(cx, ui, actions);
        motion_edit::handle_actions(self, cx, ui, actions);
        cut_panel::handle_actions(self, cx, ui, actions);
        storyboard::handle_actions(self, cx, ui, actions);
        generate::handle_actions(self, cx, ui, actions);
        generate_segments::handle_actions(self, cx, ui, actions);
        if !self.started || self.stopped {
            return;
        }
        if LAYOUT.iter().any(|(_, id, _, _)| ui.splitter(cx, &[*id]).settled(actions).is_some() || ui.splitter(cx, &[*id]).collapsed(actions).is_some()) {
            self.keep_layout(cx, ui);
        }
        self.song_prompt_actions(cx, ui, actions);
        let lane_before = self.lib.lane;
        if self.lib.handle_actions(cx, ui, actions, &LANES) {
            self.repaint = true;
            self.repaint_library = true;
            if self.lib.lane != lane_before {
                if let Some(mut list) = self.lib.list(cx, ui).borrow_mut::<CutMediaList>() {
                    list.reset_scroll(cx);
                }
            }
        }
        let asset_preview = ui.widget(cx, ids!(cut_lib_preview));
        for action in actions.filter_widget_actions_cast::<makepad_library_browser::list::AssetPreviewAction>(asset_preview.widget_uid()) {
            if let makepad_library_browser::list::AssetPreviewAction::Seek(t) = action {
                match &mut self.asset_play {
                    Some(play) if play.started => play.seek = Some(t),
                    _ => {
                        self.toggle_asset_play(cx, ui);
                        if let Some(play) = &mut self.asset_play {
                            play.seek = Some(t);
                        }
                    }
                }
            }
        }
        if ui.button(cx, ids!(cut_info_head)).clicked(actions) {
            self.info_folded = !self.info_folded;
            self.info_dragged = false;
            self.repaint_library = true;
        }
        if ui.splitter(cx, ids!(cut_lib_split)).settled(actions).is_some() {
            self.info_dragged = true;
        }
        if ui.button(cx, ids!(cut_lib_copy)).clicked(actions) {
            if let Some(row) = self.lib.list_selected.and_then(|index| self.lib.rows.get(index)) {
                cx.copy_to_clipboard(&row.reference.revision.to_string());
                self.lib.status = "Copied the revision id".into();
                self.repaint = true;
            }
        }
        if ui.button(cx, ids!(cut_lib_show)).clicked(actions) {
            if let Some(row) = self.lib.list_selected.and_then(|index| self.lib.rows.get(index)).cloned() {
                let id = row.reference.asset_id.to_string();
                let first = self.session.as_ref().and_then(|session| session.doc.nodes.iter().find(|node| match &node.kind {
                    CutNodeKind::Clip { src, .. } | CutNodeKind::Still { src, .. } | CutNodeKind::AudioClip { src, .. } => media_id(src) == id,
                    _ => false,
                }).map(|node| (node.id.clone(), node.span.map_or(0.0, |span| span.start))));
                if let Some((node, start)) = first {
                    self.selected = Some(node);
                    self.seek(start);
                }
            }
        }
        if ui.button(cx, ids!(cut_lib_add)).clicked(actions) {
            self.add_chosen();
        }
        if ui.button(cx, ids!(cut_lib_open)).clicked(actions) {
            if let Some(index) = self.lib.list_selected {
                self.open_row(index);
            }
        }
        let picker = ui.widget(cx, ids!(cut_fx_picker));
        for action in actions.filter_widget_actions_cast::<makepad_content_packs::picker::PickerAction>(picker.widget_uid()) {
            self.picker_action(cx, ui, action);
        }
        // The project bar.
        if ui.button(cx, ids!(cut_format_btn)).clicked(actions) {
            // The project's settings are the inspector with nothing selected.
            self.selected = None;
            self.also.clear();
            self.sections.insert(live_id!(cut_h_project), true);
            self.inspected = None;
            self.status = "Project settings are in the inspector".into();
            self.repaint = true;
        }
        if ui.button(cx, ids!(cut_new)).clicked(actions) {
            self.new_menu = !self.new_menu;
            self.repaint = true;
        }
        if ui.button(cx, ids!(cut_new_scrim)).clicked(actions) {
            self.new_menu = false;
            self.repaint = true;
        }
        for (id, format) in NEW_FORMATS {
            if ui.button(cx, &[id, live_id!(row)]).clicked(actions) {
                self.new_menu = false;
                self.new_cut(format);
            }
        }
        // Web export with the options last confirmed (defaults until then);
        // the same call the chat's export_web card makes. Again stops it.
        if ui.button(cx, ids!(cut_web_export)).clicked(actions) {
            if self.web_export_running() {
                self.stop_web_export();
            } else if let Err(error) = self.web_export_api_start(cx, &makepad_strict_json::Value::Obj(Vec::new())) {
                self.status = format!("Web export: {error}");
                self.repaint = true;
            }
        }
        for (id, format) in BLANK_FORMATS {
            let tile = ui.widget(cx, &[id]);
            if actions.filter_widget_actions_cast::<controls::TileAction>(tile.widget_uid()).any(|action| matches!(action, controls::TileAction::Clicked)) {
                self.new_cut(format);
            }
        }
        let recent = ui.widget(cx, ids!(cut_recent));
        for action in actions.filter_widget_actions_cast::<MediaListAction>(recent.widget_uid()) {
            if let MediaListAction::Click(index) | MediaListAction::Open(index) = action {
                if let Some(recent) = self.recent.get(index).cloned() {
                    self.crumbs.clear();
                    self.status = format!("Opening “{}”…", recent.title);
                    self.send(StoreJob::Open { reference: recent.reference, title: recent.title, kind: AssetKind::Cut, tree: recent.tree });
                }
            }
        }
        if ui.button(cx, ids!(cut_blank_open)).clicked(actions) {
            self.set_lane(cx, ui, PageLane::Store(LibLane::Cuts));
            self.status = "Click a cut in the library to open it".into();
        }
        if let Some((label, _)) = ui.text_input(cx, ids!(cut_title)).returned(actions) {
            if self.session.as_ref().is_some_and(|session| session.doc.label != label) {
                self.op(CutOp::Label(label));
            }
        }
        if ui.text_input(cx, ids!(cut_title)).escaped(actions) {
            if let Some(session) = &self.session {
                ui.text_input(cx, ids!(cut_title)).set_text(cx, &session.doc.label);
            }
        }
        if ui.button(cx, ids!(cut_undo)).clicked(actions) {
            self.undo(false);
        }
        if ui.button(cx, ids!(cut_redo)).clicked(actions) {
            self.undo(true);
        }
        if ui.button(cx, ids!(cut_save)).clicked(actions) {
            self.save(cx);
        }
        let render_button = ui.widget(cx, ids!(cut_render));
        if actions.filter_widget_actions_cast::<controls::RenderButtonAction>(render_button.widget_uid()).any(|action| matches!(action, controls::RenderButtonAction::Clicked)) {
            self.render_button();
        }
        // The editable timecode.
        if ui.button(cx, ids!(cut_time)).clicked(actions) && self.session.is_some() {
            self.typing_time = true;
            ui.view(cx, ids!(cut_time_box)).set_visible(cx, true);
            let input = ui.text_input(cx, ids!(cut_time_input));
            input.set_text(cx, &timecode(self.playhead, self.fps()));
            input.take_key_focus(cx);
        }
        if self.typing_time {
            let input = ui.text_input(cx, ids!(cut_time_input));
            if let Some((text, _)) = input.returned(actions) {
                match (controls::Format::Time { fps: self.fps() }).parse(&text, self.playhead) {
                    Some(t) => self.seek(t),
                    None => {
                        self.time_error = Some(Cx::time_now());
                        self.status = format!("“{text}” is not a time: type 00:00:01:12, 1:12 or +30");
                    }
                }
                self.typing_time = false;
            } else if input.escaped(actions) || input.key_focus_lost(actions) {
                self.typing_time = false;
            }
            if !self.typing_time {
                ui.view(cx, ids!(cut_time_box)).set_visible(cx, false);
                self.repaint = true;
                self.repaint_preview = true;
            }
        }
        if ui.button(cx, ids!(cut_crumb)).clicked(actions) {
            self.open_parent();
        }
        if ui.button(cx, ids!(cut_keys_close)).clicked(actions) || ui.button(cx, ids!(cut_keys_sheet_scrim)).clicked(actions) {
            self.keys_sheet = false;
            self.repaint = true;
        }
        // The transport.
        if ui.button(cx, ids!(cut_play)).clicked(actions) {
            self.shuttle(1.0);
        }
        if ui.button(cx, ids!(cut_pause)).clicked(actions) {
            self.shuttle(0.0);
        }
        if ui.button(cx, ids!(cut_fwd)).clicked(actions) {
            let rate = self.playing.map_or(0.0, |playing| playing.rate);
            self.shuttle(if rate > 0.0 { (rate * 2.0).min(MAX_SHUTTLE) } else { 1.0 });
        }
        if ui.button(cx, ids!(cut_back)).clicked(actions) {
            let rate = self.playing.map_or(0.0, |playing| playing.rate);
            self.shuttle(if rate < 0.0 { (rate * 2.0).max(-MAX_SHUTTLE) } else { -1.0 });
        }
        if ui.button(cx, ids!(cut_to_start)).clicked(actions) {
            self.seek(0.0);
        }
        if ui.button(cx, ids!(cut_to_end)).clicked(actions) {
            self.seek(self.duration());
        }
        if ui.button(cx, ids!(cut_safe)).clicked(actions) {
            self.safe_zones = !self.safe_zones;
            self.repaint = true;
        }
        if ui.button(cx, ids!(cut_loop)).clicked(actions) {
            self.loop_on = !self.loop_on;
            self.repaint = true;
        }
        let preview = ui.widget(cx, ids!(cut_preview));
        for action in actions.filter_widget_actions_cast::<PreviewAction>(preview.widget_uid()) {
            if let PreviewAction::Select(node) = action {
                self.selected = Some(node);
                self.repaint = true;
            }
        }
        // The timeline's toolbar.
        if ui.button(cx, ids!(cut_add)).clicked(actions) {
            self.add_chosen();
        }
        if ui.button(cx, ids!(cut_blade)).clicked(actions) {
            self.blade();
        }
        if ui.button(cx, ids!(cut_title_add)).clicked(actions) {
            self.op(CutOp::AddTitle { start: self.playhead, text: "Title".into() });
        }
        if ui.button(cx, ids!(cut_voice_add)).clicked(actions) {
            self.add_voice_line();
        }
        if ui.button(cx, ids!(cut_marker)).clicked(actions) {
            self.add_marker();
        }
        if ui.button(cx, ids!(cut_mark_in_btn)).clicked(actions) {
            self.mark(true, false);
        }
        if ui.button(cx, ids!(cut_mark_out_btn)).clicked(actions) {
            self.mark(false, false);
        }
        if ui.button(cx, ids!(cut_snap)).clicked(actions) {
            self.snap_off = !self.snap_off;
            self.repaint = true;
        }
        if ui.button(cx, ids!(cut_skim)).clicked(actions) {
            self.skim_off = !self.skim_off;
            self.repaint = true;
        }
        if ui.button(cx, ids!(cut_height)).clicked(actions) {
            self.track_height = self.track_height.next();
            self.repaint = true;
        }
        if ui.button(cx, ids!(cut_zoom_in)).clicked(actions) {
            self.zoom_by(cx, ui, 1.5);
        }
        if ui.button(cx, ids!(cut_zoom_out)).clicked(actions) {
            self.zoom_by(cx, ui, 1.0 / 1.5);
        }
        if ui.button(cx, ids!(cut_delete)).clicked(actions) {
            self.delete_selected();
        }
        self.inspector_actions(cx, ui, actions);
        let timeline = ui.widget(cx, ids!(cut_timeline));
        if let Some(value) = ui.slider(cx, ids!(cut_zoom)).slided(actions) {
            if let Some(mut timeline) = timeline.borrow_mut::<CutTimeline>() {
                timeline.set_zoom(cx, slider_to_zoom(value));
            }
        }
        if ui.button(cx, ids!(cut_zoom_fit)).clicked(actions) {
            self.fit_after_paint(cx, ui);
        }
        // The two tabbed columns.
        for (id, right) in [(live_id!(cut_left_tabs), false), (live_id!(cut_side_tabs), true)] {
            let tabs = ui.widget(cx, &[id]);
            for action in actions.filter_widget_actions_cast::<controls::SegmentAction>(tabs.widget_uid()) {
                if let controls::SegmentAction::Selected(index) = action {
                    if right {
                        self.side_tree = index == 1;
                    } else {
                        self.left_editor = index == 1;
                    }
                    self.show_tabs(cx, ui);
                }
            }
        }
        // The structure: a row selects its block, as the timeline does.
        let tree = ui.widget(cx, ids!(cut_tree));
        for action in actions.filter_widget_actions_cast::<StructureAction>(tree.widget_uid()) {
            match action {
                StructureAction::Select(path) => {
                    if let Some(node) = cut_structure::selects(&path) {
                        self.selected = node;
                        self.also.clear();
                        self.repaint = true;
                    }
                }
                StructureAction::Fold(path) => {
                    if !self.tree_folded.remove(&path) {
                        self.tree_folded.insert(path);
                    }
                    self.repaint = true;
                }
                _ => {}
            }
        }
        for action in actions.filter_widget_actions_cast::<TimelineAction>(timeline.widget_uid()) {
            match action {
                TimelineAction::Select(node, t) => {
                    if let Some(node) = &node {
                        if self.place_armed(node, t) {
                            continue;
                        }
                    }
                    self.selected = node;
                    self.also.clear();
                    self.repaint = true;
                }
                // Playing keeps playing from the new position; paused stays paused.
                TimelineAction::Scrub(t) => self.seek(t),
                TimelineAction::Skim(t) => {
                    self.skim = t;
                    self.repaint_preview = true;
                }
                TimelineAction::Move { node, start, track } => {
                    let lane = self.projection.block(&node).map(|block| block.lane);
                    let name = self.projection.block(&node).map(|block| block.label.clone()).unwrap_or_else(|| node.clone());
                    self.moved(node, start, track);
                    let row = match lane { Some(Lane::Audio) => format!("A{}", track + 1), Some(Lane::Voice) => "VO".into(), _ => format!("V{}", track + 1) };
                    self.status = format!("Moved {name} to {row} at {}", timecode(start, self.fps()));
                    self.status_sticky = true;
                }
                TimelineAction::Trim { node, start, length } => {
                    let is_transition = self.session.as_ref().and_then(|session| session.doc.node(&node)).is_some_and(|node| matches!(node.kind, CutNodeKind::Transition { .. }));
                    if is_transition { self.retime_transition(&node, start, length) } else { self.op(CutOp::Trim { node, start, length }) }
                }
                TimelineAction::Fade { node, fade_in, fade_out } => self.op(CutOp::Fade { node, fade_in, fade_out }),
                TimelineAction::Key(key) => self.key(cx, ui, &key),
                TimelineAction::Paste(text) => {
                    if !self.paste_web_url(&text) {
                        self.status = "Paste a web address here to grab it (a YouTube link, or a page with a video)".into();
                        self.repaint = true;
                    }
                }
                TimelineAction::Zoomed(px) => ui.slider(cx, ids!(cut_zoom)).set_value(cx, zoom_to_slider(px)),
                TimelineAction::Open(node) => {
                    if !motion_edit::open(self, &node) {
                        self.open_nested(&node);
                    }
                }
                TimelineAction::SelectMany(nodes) => {
                    self.selected = nodes.first().cloned();
                    self.also = nodes.into_iter().skip(1).collect();
                    if !self.also.is_empty() {
                        self.status = format!("{} blocks selected", self.also.len() + 1);
                    }
                    self.repaint = true;
                }
                TimelineAction::Snapped(name) => {
                    self.status = format!("Snapped to {name}");
                    self.status_sticky = true;
                    self.repaint = true;
                }
                TimelineAction::None => {}
            }
        }
        let list = self.lib.list(cx, ui);
        for action in actions.filter_widget_actions_cast::<MediaListAction>(list.widget_uid()) {
            match action {
                MediaListAction::Click(index) if self.looks_action(cx, ui, index, None) => {}
                MediaListAction::Open(index) if self.looks_action(cx, ui, index, None) => {}
                MediaListAction::Drag(index, abs) if self.looks_action(cx, ui, index, Some((abs, false))) => {}
                MediaListAction::Drop(index, abs) if self.looks_action(cx, ui, index, Some((abs, true))) => {}
                // With the storyboard shown, media lands on the board.
                MediaListAction::Drag(index, abs) if storyboard::media_drag(self, cx, ui, index, abs, false) => {}
                MediaListAction::Drop(index, abs) if storyboard::media_drag(self, cx, ui, index, abs, true) => {}
                MediaListAction::Click(index) => match (self.draft_at(index), self.folder_at(index)) {
                    (Some(draft), _) => {
                        self.lib.list_selected = Some(index);
                        self.repaint_library = true;
                        self.click_draft(draft);
                    }
                    (None, Some(card)) if card.is_deck() => self.deck_card_request = Some(card.clone()),
                    (None, Some(card)) => {
                        self.lib.list_selected = Some(index);
                        self.repaint_library = true;
                        self.status = format!("“{}” · double-click opens it", card.title);
                        self.repaint = true;
                    }
                    (None, None) => self.click_row(index),
                },
                MediaListAction::Open(index) => match (self.draft_at(index), self.folder_at(index)) {
                    (Some(draft), _) => self.open_draft(cx, ui, draft),
                    (None, Some(card)) if card.is_deck() => self.deck_card_request = Some(card.clone()),
                    (None, Some(card)) => self.open_folder_cut(card),
                    (None, None) => self.open_row(index),
                },
                MediaListAction::Deselect => {
                    self.lib.list_selected = None;
                    self.stop_asset_play(cx, ui);
                    self.repaint_library = true;
                }
                MediaListAction::Key(key) => match key.key_code {
                    KeyCode::Space => self.toggle_asset_play(cx, ui),
                    KeyCode::Escape => {
                        self.lib.list_selected = None;
                        self.stop_asset_play(cx, ui);
                        self.repaint_library = true;
                    }
                    KeyCode::KeyE => self.add_chosen(),
                    KeyCode::Delete | KeyCode::Backspace => {
                        if let Some(draft) = self.lib.list_selected.and_then(|index| self.draft_at(index)) {
                            self.delete_draft(draft);
                        }
                    }
                    _ => {}
                },
                MediaListAction::Drag(index, abs) => {
                    if self.lib.list_selected != Some(index) {
                        self.lib.list_selected = Some(index);
                        self.explore(index);
                        self.repaint_library = true;
                    }
                    let hit = timeline.borrow_mut::<CutTimeline>().and_then(|mut timeline| timeline.hit_snapped(abs));
                    let hint = hit.map(|hit| match self.plan_media(index, hit.t, Some((hit.lane, hit.track))) {
                        Ok((mut hint, _)) => {
                            if timeline.borrow::<CutTimeline>().is_some_and(|timeline| timeline.is_locked(hint.lane, hint.track)) {
                                hint.invalid = Some("Locked track".into());
                            }
                            hint
                        }
                        Err(reason) => DropHint { lane: hit.lane, track: hit.track, start: hit.t, length: 2.0, targets: Vec::new(), label: String::new(), invalid: Some(reason), tab: false },
                    });
                    if let Some(mut timeline) = timeline.borrow_mut::<CutTimeline>() {
                        timeline.set_drop_hint(cx, hint);
                    }
                }
                MediaListAction::Drop(index, abs) => {
                    let hit = timeline.borrow_mut::<CutTimeline>().and_then(|mut timeline| timeline.hit_snapped(abs));
                    if let Some(mut timeline) = timeline.borrow_mut::<CutTimeline>() {
                        timeline.set_drop_hint(cx, None);
                        timeline.focus(cx);
                    }
                    match hit {
                        Some(hit) => self.drop_row(index, hit.t, Some((hit.lane, hit.track))),
                        None => {
                            self.status = "Not added: dropped outside the timeline".into();
                            self.repaint = true;
                        }
                    }
                }
                MediaListAction::Cancel => {
                    if let Some(mut timeline) = timeline.borrow_mut::<CutTimeline>() {
                        timeline.set_drop_hint(cx, None);
                    }
                }
                MediaListAction::None => {}
            }
        }
    }

    /// The left column's page (library or the film's editor) and the
    /// sidebar's (the selection's inspector or the structure).
    fn show_tabs(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        ui.widget(cx, ids!(cut_left_lib)).set_visible(cx, !self.left_editor);
        ui.widget(cx, ids!(cut_lib_tools)).set_visible(cx, !self.left_editor);
        ui.widget(cx, ids!(cut_left_editor)).set_visible(cx, self.left_editor);
        ui.widget(cx, ids!(cut_side_edit)).set_visible(cx, self.session.is_some() && !self.side_tree);
        ui.widget(cx, ids!(cut_side_tree)).set_visible(cx, self.side_tree);
        for (id, index) in [(live_id!(cut_left_tabs), self.left_editor as usize), (live_id!(cut_side_tabs), self.side_tree as usize)] {
            if let Some(mut tabs) = ui.widget(cx, &[id]).borrow_mut::<controls::CutSegmented>() {
                tabs.set_active(cx, index);
            }
        }
        ui.redraw(cx);
    }

    /// Keep the pane sizes for the next start.
    pub(crate) fn keep_layout(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        if self.deck_layout_suspended {
            return;
        }
        self.layout.read(cx, ui);
        self.send(StoreJob::Layout(Some(self.layout.text())));
    }

    pub(crate) fn suspend_deck_layout(&mut self, suspended: bool) {
        self.deck_layout_suspended = suspended;
    }

    /// The DECK page's splitters, once its page is built: where they were
    /// left. Called every pump while the page is shown; it applies once.
    pub(crate) fn apply_deck_layout(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        if !self.deck_layout_applied && self.started && !self.deck_layout_suspended {
            self.deck_layout_applied = true;
            self.layout.apply(cx, ui, &[
                live_id!(deck_code_split), live_id!(deck_nav_split),
                live_id!(deck_strip), live_id!(deck_centre), live_id!(deck_side),
            ]);
        }
    }

    /// A DECK splitter was dragged or folded: the layout is kept.
    pub(crate) fn deck_layout_actions(&mut self, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
        if self.deck_layout_suspended {
            return;
        }
        let mut changed = false;
        let navigation = ui.splitter(cx, ids!(deck_nav_split));
        if navigation.collapsed(actions).is_some() {
            ui.widget(cx, ids!(deck_strip)).set_visible(cx, navigation.collapse() != SplitterCollapse::A);
            changed = true;
        }
        // The library and slides buttons open the left column on a tab: the
        // DECK page does that; the layout keeps what they left.
        if ui.button(cx, ids!(deck_toggle_library)).clicked(actions) || ui.button(cx, ids!(deck_toggle_sorter)).clicked(actions) {
            changed = true;
        }
        for (toggle, column) in [
            (live_id!(deck_toggle_view), live_id!(deck_centre)),
            (live_id!(deck_toggle_side), live_id!(deck_side)),
        ] {
            if ui.button(cx, &[toggle]).clicked(actions) {
                let column = ui.widget(cx, &[column]);
                column.set_visible(cx, !column.visible());
                changed = true;
            }
        }
        // The code editor folds away and comes back.
        if ui.button(cx, ids!(deck_toggle_code)).clicked(actions) {
            let split = ui.splitter(cx, ids!(deck_code_split));
            let next = if split.collapse() == SplitterCollapse::B { SplitterCollapse::None } else { SplitterCollapse::B };
            split.set_collapse(cx, next);
            changed = true;
        }
        if changed {
            // When only code is open it gets the whole desk. Bringing any
            // column back restores the remembered code height.
            let any_column = [live_id!(deck_centre), live_id!(deck_side)].iter().any(|id| ui.widget(cx, &[*id]).visible());
            let split = ui.splitter(cx, ids!(deck_code_split));
            if split.collapse() != SplitterCollapse::B {
                split.set_collapse(cx, if any_column { SplitterCollapse::None } else { SplitterCollapse::A });
            }
            ui.redraw(cx);
        }
        let moved = [live_id!(deck_code_split), live_id!(deck_nav_split)].into_iter().any(|id| {
            let split = ui.splitter(cx, &[id]);
            split.settled(actions).is_some() || split.collapsed(actions).is_some()
        });
        if changed || moved {
            self.keep_layout(cx, ui);
        }
    }

    /// Page events: a double click on a splitter's bar puts it back.
    pub(super) fn handle_event(&mut self, cx: &mut Cx, ui: &WidgetRef, event: &Event) {
        motion_edit::handle_event(self, event);
        storyboard::handle_event(self, cx, ui, event);
        generate::handle_event(self, cx, event);
        new_video::handle_event(self, cx, event);
        if self.frame.is_event(event).is_some() {
            self.tick(cx, ui);
        } else if (self.repaint_preview || self.playing.is_some()) && self.started && !self.stopped {
            // A scrub, skim, seek or play since the last event: the viewer
            // follows on the next display frame.
            self.frame = cx.new_next_frame();
        }
        let Event::MouseDown(down) = event else { return };
        let double = self.last_down.is_some_and(|(time, at)| down.time - time < 0.4 && (down.abs - at).length() < 5.0);
        self.last_down = Some((down.time, down.abs));
        if !double {
            return;
        }
        for (id, from_a, default) in SPLITS {
            let hit = ui.widget(cx, &[id]).borrow::<Splitter>().is_some_and(|split| split.bar_grab_rect(cx).contains(down.abs));
            if hit {
                ui.splitter(cx, &[id]).set_align(cx, split_align(from_a, default));
                self.keep_layout(cx, ui);
                break;
            }
        }
    }

    /// Every pump: replies from the workers, the next queued edit, the
    /// playing playhead, and the page's widgets when something changed.
    pub(super) fn pump(&mut self, cx: &mut Cx, ui: &WidgetRef, shown: bool) {
        if self.stopped {
            // The viewer's renderer holds decoder leases and reader
            // threads: let them go so Story can close.
            if self.rendered.take().is_some() {
                if let Some(mut preview) = ui.widget(cx, ids!(cut_preview)).borrow_mut::<CutPreview>() {
                    preview.set_source(cx, None);
                }
            }
            return;
        }
        if !self.started {
            self.start(cx);
        }
        self.pump_store(cx, ui);
        self.pump_song();
        self.pump_folder();
        self.pump_media();
        for (_, path) in std::mem::take(&mut self.local_files) {
            self.asset_file(cx, ui, path);
        }
        self.pump_asset_play(cx, ui);
        if let Some((thumbnail, revision)) = self.pending_thumbnail.take() {
            // The preview draws one thumbnail: a save's goes first, the
            // poster is drawn again afterwards.

            if let Some(mut preview) = ui.widget(cx, ids!(cut_preview)).borrow_mut::<CutPreview>() {
                preview.set_thumbnail(cx, thumbnail);
            }
            self.thumbnail_of = Some(revision);
        }
        if self.thumbnail_of.is_some() {
            let done = ui.widget(cx, ids!(cut_preview)).borrow_mut::<CutPreview>().and_then(|mut preview| preview.take_thumbnail());
            if let Some(done) = done {
                self.thumbnail_of = None;
                let rendered = match done {
                    Ok(frame) => {
                        let pixels = frame.argb();
                        Some(Thumb { width: frame.width as usize, height: frame.height as usize, pixels: Arc::new(pixels) })
                    }
                    Err(error) => {
                        log!("edit: the renderer's thumbnail failed ({error}); using the sketch");
                        None
                    }
                };
                self.finish_save(cx, rendered);
            }
        }
        if shown {
            // The folder cards' pictures (after the library rows and the drafts).
            self.repaint_library |= self.card_pictures.pump(cx);
            let demands = if !self.left_editor {
                let first = self.lib.rows.len() + self.draft_rows().len();
                let cards: Vec<&'static folder_docs::Card> = self.folder_cards().collect();
                let (dpi, visible) = self.lib.visible_cards(cx, ui);
                let mut demands = self.card_pictures.demands(&visible, dpi, |index| index.checked_sub(first).and_then(|at| cards.get(at).copied()));
                demands.extend(self.lib.still_demands(cx, ui, 0));
                demands
            } else { Vec::new() };
            if let Some(mut preview) = ui.widget(cx, ids!(cut_preview)).borrow_mut::<CutPreview>() {
                self.repaint_library |= self.lib.accept_stills(preview.library_stills());
                self.repaint_library |= self.card_pictures.accept(preview.library_stills());
                preview.set_library_stills(cx, demands);
            }
        }
        self.pump_card_media(cx, ui, shown);
        self.pump_render(cx);
        self.pump_web_export(cx);
        self.pump_worker(cx, ui);
        if self.save_requested && self.in_flight.is_none() && self.queue.front().is_none_or(|pending| matches!(pending, Pending::Open { .. })) { self.save(cx); }
        self.next_pending_cut();
        self.pump_autosave(cx, ui);
        self.pump_export_copy();
        self.pump_generate_bar(cx, ui, shown);
        if shown && self.lib.lane.is_none() {
            self.set_lane(cx, ui, PageLane::Store(LibLane::Cuts));
        }
        if shown && !self.focused {
            self.focused = true;
            if let Some(timeline) = ui.widget(cx, ids!(cut_timeline)).borrow::<CutTimeline>() {
                timeline.focus(cx);
            }
        }
        self.advance_playhead(cx, ui);
        // Clip filmstrips wait while the cut plays.
        if let Some(mut timeline) = ui.widget(cx, ids!(cut_timeline)).borrow_mut::<CutTimeline>() {
            let playing = self.playing.is_some();
            if timeline.clip_films.paused != playing {
                timeline.clip_films.paused = playing;
                if !playing {
                    timeline.redraw(cx);
                }
            }
        }
        // The board follows the open cut on any page (the AI's tools use
        // it); it paints only when shown.
        storyboard::pump(self, cx, ui, shown);
        if !shown {
            return;
        }
        if self.repaint {
            self.paint(cx, ui);
        }
        if self.repaint_library {
            self.paint_library(cx, ui);
        }
        if self.repaint_preview {
            self.paint_preview(cx, ui);
        }
        generate_segments::update(self, cx, ui);
        motion_edit::pump(self, cx, ui, shown);
        cut_panel::pump(self, cx, ui, shown);
    }

    /// The playing playhead, from the clock. Called on every display frame
    /// while playing ([`Self::tick`]) and from the pump.
    fn advance_playhead(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        let Some(playing) = self.playing else { return };
        let (lo, hi) = (0.0, self.marks.1.filter(|out| playing.rate > 0.0 && *out > playing.from).unwrap_or(self.duration()));
        let t = self.now_t();
        if let (true, Some(start), true) = (self.loop_on, self.marks.0, t >= hi && playing.rate > 0.0) {
            // Loop the in/out range.
            self.playing = Some(Playing { since: Cx::time_now(), from: start, rate: playing.rate });
            self.playhead = start;
        } else if t >= hi || t <= lo && playing.rate < 0.0 {
            self.playing = None;
            self.playhead = t.clamp(lo, hi);
            self.repaint = true;
        } else {
            self.playhead = t;
        }
        self.repaint_preview = true;
        if let Some(mut timeline) = ui.widget(cx, ids!(cut_timeline)).borrow_mut::<CutTimeline>() {
            timeline.follow(cx);
        }
    }

    /// One display frame while playing, or after the playhead or the skim
    /// moved: the playhead advances by the clock and the viewer is asked
    /// for its frame now, not at the next pump (which runs at 30 Hz).
    fn tick(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        if self.stopped || self.session.is_none() {
            return;
        }
        let now = Cx::time_now();
        if let Some(last) = self.last_tick.filter(|last| now - last < 0.1) {
            // The display's frame interval, smoothed.
            self.refresh = if self.refresh > 0.0 { self.refresh * 0.9 + (now - last) * 0.1 } else { now - last };
        }
        self.last_tick = Some(now);
        let was_playing = self.playing.is_some();
        self.advance_playhead(cx, ui);
        if was_playing && self.playing.is_none() {
            // Playback ended: the page shows it now (everything else the
            // pump repaints at its own pace, never per display frame).
            self.paint(cx, ui);
        }
        if self.repaint_preview {
            self.paint_preview(cx, ui);
        }
        if self.playing.is_some() {
            self.frame = cx.new_next_frame();
        } else {
            self.last_tick = None;
        }
    }

    /// The platform whose UI the viewer outlines: the page's choice, else
    /// the cut's own (a Reframe's or the subtitles' `safe`).
    fn platform(&self) -> SafeZone {
        if let Some(platform) = self.platform {
            return platform;
        }
        let Some(session) = &self.session else { return SafeZone::None };
        session.doc.nodes.iter().find_map(|node| match &node.kind {
            CutNodeKind::Reframe { safe, .. } | CutNodeKind::Subtitles { safe, .. } if *safe != SafeZone::None => Some(*safe),
            _ => None,
        }).unwrap_or(SafeZone::None)
    }

    /// A toolbar toggle lit (accent glyph on the accent wash) or not.
    fn lit(cx: &mut Cx, ui: &WidgetRef, id: LiveId, on: bool) {
        let p = makepad_look::palette::get(cx);
        let mut button = ui.widget(cx, &[id]);
        let (bg, hover, icon) = if on { (p.accent_wash, p.accent_wash, p.accent) } else { (p.control, p.control_hover, p.label) };
        let (bg, hover, icon) = (Vec4f::from_u32(bg), Vec4f::from_u32(hover), Vec4f::from_u32(icon));
        script_apply_eval!(cx, button, {
            draw_bg +: {color: #(bg) color_focus: #(bg) color_hover: #(hover)}
            draw_icon +: {color: #(icon)}
        });
    }

    fn paint(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        self.repaint = false;
        let fps = self.fps();
        let p = makepad_look::palette::get(cx);
        ui.view(cx, ids!(cut_blank)).set_visible(cx, self.session.is_none() && self.queue.is_empty() && self.in_flight.is_none());
        ui.view(cx, ids!(cut_new_menu)).set_visible(cx, self.new_menu);
        if self.new_menu {
            // The menu hangs under New, right-aligned to it.
            let new = laid_out_rect(cx, ui.widget(cx, ids!(cut_new)).area());
            let menu = ui.widget(cx, ids!(cut_new_box));
            // The bar spans the desk, so it is the menu's frame (the menu
            // itself has no rect until it has been drawn once).
            let area = laid_out_rect(cx, ui.widget(cx, ids!(cut_project_bar)).area());
            let right = (area.pos.x + area.size.x - (new.pos.x + new.size.x)).max(0.0);
            let top = (new.pos.y + new.size.y + 4.0 - area.pos.y).max(0.0);
            if let Some(mut view) = menu.borrow_mut::<View>() {
                view.layout.padding.top = top;
                view.layout.padding.right = right;
            }
            menu.redraw(cx);
        }
        // A web export is of the last saved revision: none until the cut is saved once.
        let running = self.web_export_running();
        let saved = self.session.as_ref().is_some_and(|session| session.head.is_some());
        let web = ui.button(cx, ids!(cut_web_export));
        web.set_text(cx, if running { "Stop web export" } else { "Web export" });
        web.set_enabled(cx, running || saved);
        self.paint_song_prompt(cx, ui);
        ui.view(cx, ids!(cut_tl_bar)).set_visible(cx, self.session.is_some());
        let open = self.session.is_some();
        ui.view(cx, ids!(cut_bar_doc)).set_visible(cx, open);
        ui.button(cx, ids!(cut_crumb)).set_visible(cx, open && !self.crumbs.is_empty());
        if let Some((parent, _)) = self.crumbs.last() {
            ui.button(cx, ids!(cut_crumb)).set_text(cx, &format!("{parent}  ›"));
        }
        ui.view(cx, ids!(cut_bar_save)).set_visible(cx, open);
        ui.label(cx, ids!(cut_no_edit)).set_visible(cx, !open);
        ui.view(cx, ids!(cut_no_edit_fill)).set_visible(cx, !open);
        // Nothing under the blank page's card takes its clicks.
        ui.widget(cx, ids!(cut_side_edit)).set_visible(cx, open && !self.side_tree);
        if let Some(mut split) = ui.widget(cx, ids!(cut_split)).borrow_mut::<Splitter>() {
            let collapse = if open { SplitterCollapse::None } else { SplitterCollapse::B };
            if split.collapse() != collapse {
                split.set_collapse(cx, collapse);
            }
        }
        if let Some(mut timeline) = ui.widget(cx, ids!(cut_timeline)).borrow_mut::<CutTimeline>() {
            timeline.set_blank(cx, self.session.is_none());
        }
        if let Some(session) = &self.session {
            let doc = session.doc.clone();
            let (eval, _) = self.eval(&doc);
            let projection = self.with_generate_states(project(&eval, &self.titles, &|_| false));
            self.projection = projection.clone();
            let films = self.films();
            let render_bar = self.render_bar();
            if let Some(mut timeline) = ui.widget(cx, ids!(cut_timeline)).borrow_mut::<CutTimeline>() {
                timeline.set_projection(cx, projection);
                timeline.set_media(cx, films, self.peaks.clone());
                // Graphics' and nested cuts' own filmstrips, once per
                // revision (clip_films.rs).
                if self.films_rev != Some(session.revision) {
                    self.films_rev = Some(session.revision);
                    let resolver = clip_films::SharedResolver::new(Box::new(store::LazyStoreMedia::new()));
                    timeline.clip_films.set_resolver(resolver);
                    let tree = session.tree.as_ref().map(|tree| tree.context(&session.path));
                    if timeline.clip_films.set_jobs(clip_films::jobs(&doc, &session.source), tree) {
                        timeline.redraw(cx);
                    }
                }
                timeline.set_selected(cx, self.selected.clone(), self.also.clone());
            }
            if let Some(mut tree) = ui.widget(cx, ids!(cut_tree)).borrow_mut::<StructureTree>() {
                let labels = self.projection.blocks.iter().map(|b| (b.node.clone(), b.label.clone())).collect();
                let rows = cut_structure::rows(&doc, &labels, &self.tree_folded);
                let selected = Some(self.selected.as_deref().map_or(cut_structure::FILM.to_string(), |s| split_selection(s).0.to_string()));
                tree.set_model(cx, rows, Vec::new(), selected);
            }
            if let Some(mut timeline) = ui.widget(cx, ids!(cut_timeline)).borrow_mut::<CutTimeline>() {
                timeline.set_playhead(cx, self.playhead);
                timeline.set_marks(cx, self.marks, self.markers.clone(), fps, render_bar);
                timeline.set_modes(cx, !self.snap_off, !self.skim_off, self.track_height);
            }
            let session = self.session.as_ref().unwrap();
            let busy = self.in_flight.is_some() || !self.queue.is_empty();
            let clips = self.projection.blocks.iter().filter(|block| !matches!(block.kind, project::BlockKind::Caption | project::BlockKind::Effect)).count();
            let status = match (&self.armed, self.status.is_empty()) {
                (Some((preset, transition)), _) => format!("{} armed — click {} · Esc", preset_label(preset), if *transition { "where two clips overlap" } else { "the clip it goes on" }),
                (_, true) if clips == 0 => "Drag media from the library onto the tracks, or choose an item and press E".to_string(),
                (_, true) => format!("{clips} clip{} · {}", if clips == 1 { "" } else { "s" }, timecode(session.doc.duration, fps)),
                (_, false) => self.status.clone(),
            };
            ui.label(cx, ids!(cut_status)).set_text(cx, &if busy { format!("Writing…  {status}") } else { status });
            // The project bar: save state, format, duration.
            // A folder cut is saved in its folder; a cut with no library revision yet is not saved.
            let in_folder = self.folder.as_ref().map(|folder| folder.doc.dir.display().to_string());
            let unsaved = session.dirty() || (session.head.is_none() && in_folder.is_none());
            let (dot, state, tip) = match (&self.save_error, self.saving, unsaved, &self.saved_at) {
                (_, true, _, _) => (Some(p.muted), "Saving…".to_string(), String::new()),
                (Some(error), _, true, _) => (Some(p.danger), "Save failed".to_string(), error.clone()),
                (_, _, true, _) => (Some(p.amber), "Edited".to_string(), "Not saved since the last change (cmd-S saves)".to_string()),
                (_, _, false, Some(at)) => (None, format!("Saved {at}"), in_folder.as_ref().map_or_else(|| "Saved as a library revision".to_string(), |dir| format!("Saved in {dir}"))),
                (_, _, false, None) => (None, "Saved".to_string(), in_folder.as_ref().map_or_else(|| "Opened from the library".to_string(), |dir| format!("Opened in place from {dir}"))),
            };
            ui.view(cx, ids!(cut_save_dot)).set_visible(cx, dot.is_some());
            if let Some(dot) = dot {
                let mut view = ui.widget(cx, ids!(cut_save_dot));
                let color = Vec4f::from_u32(dot);
                script_apply_eval!(cx, view, {draw_bg +: {color: #(color)}});
            }
            if let Some(mut save_tip) = ui.widget(cx, ids!(cut_save_tip)).borrow_mut::<makepad_widgets::tip::Tip>() {
                save_tip.text = if tip.is_empty() { state } else { format!("{state} · {tip}") };
            }
            let output = session.doc.output;
            ui.label(cx, ids!(cut_format)).set_text(cx, &format!("{}×{} · {} fps", output.width, output.height, source::number(output.fps).trim_end_matches(".0")));
            // The output's aspect inside 10 × 14 (a portrait frame reads).
            let (aw, ah) = if output.width >= output.height { (10.0, (10.0 * output.height as f64 / output.width.max(1) as f64).round().max(5.0)) } else { ((14.0 * output.width as f64 / output.height.max(1) as f64).round().max(5.0), 14.0) };
            let mut aspect = ui.widget(cx, ids!(cut_aspect));
            script_apply_eval!(cx, aspect, {width: #(aw) height: #(ah)});
            ui.label(cx, ids!(cut_duration)).set_text(cx, &timecode(session.doc.duration, fps));
            if self.shown_label != session.doc.label {
                self.shown_label = session.doc.label.clone();
                ui.text_input(cx, ids!(cut_title)).set_text(cx, &session.doc.label);
            }
            if let Some(mut tip) = ui.widget(cx, ids!(cut_title_tip)).borrow_mut::<makepad_widgets::tip::Tip>() {
                tip.text = format!("{} — Enter writes the name, Esc reverts", session.doc.label);
            }
            ui.button(cx, ids!(cut_undo)).set_enabled(cx, !session.undo.is_empty());
            ui.button(cx, ids!(cut_redo)).set_enabled(cx, !session.redo.is_empty());
            ui.button(cx, ids!(cut_save)).set_enabled(cx, unsaved);
        } else {
            ui.label(cx, ids!(cut_status)).set_text(cx, &self.status);
            ui.label(cx, ids!(cut_blank_error)).set_text(cx, &self.open_error);
            ui.label(cx, ids!(cut_blank_error)).set_visible(cx, !self.open_error.is_empty());
            ui.view(cx, ids!(cut_save_dot)).set_visible(cx, false);
            ui.label(cx, ids!(cut_format)).set_text(cx, "No cut open");
            ui.button(cx, ids!(cut_save)).set_enabled(cx, false);
            ui.button(cx, ids!(cut_undo)).set_enabled(cx, false);
            ui.button(cx, ids!(cut_redo)).set_enabled(cx, false);
            ui.label(cx, ids!(cut_duration)).set_text(cx, "");
            let rows = self.recent.iter().map(|recent| ListRow {
                title: recent.title.clone(),
                detail: modified(recent.published),
                kind: CardKind::Cut,
                texture: self.lib.thumbs.get(&recent.reference.asset_id.to_string()).map(|(_, texture)| texture.clone()),
                badge: Some(render_badge(&p, Some(recent.render))),
                ..ListRow::default()
            }).collect::<Vec<_>>();
            if let Some(mut list) = ui.widget(cx, ids!(cut_recent)).borrow_mut::<CutMediaList>() {
                list.set_list_mode(cx, true);
                list.set_rows(cx, rows, String::new(), CardKind::Cut);
            }
            ui.widget(cx, ids!(cut_recent_box)).set_visible(cx, !self.recent.is_empty());
            let mut recent = ui.widget(cx, ids!(cut_recent_box));
            let height = self.recent.len() as f64 * 48.0 + 12.0;
            script_apply_eval!(cx, recent, {height: #(height)});
            ui.label(cx, ids!(cut_recent_none)).set_visible(cx, self.recent.is_empty());
        }
        // Render: the button's looks.
        let look = self.render_look();
        if let Some(mut button) = ui.widget(cx, ids!(cut_render)).borrow_mut::<controls::CutRenderButton>() {
            button.set_look(cx, look.clone());
        }
        if let Some(mut tip) = ui.widget(cx, ids!(cut_render_tip)).borrow_mut::<makepad_widgets::tip::Tip>() {
            tip.text = match &look {
                controls::RenderLook::Failed => format!("Render failed: {}", self.render_failed.clone().unwrap_or_default()),
                controls::RenderLook::Progress(..) => "Click to cancel the render".into(),
                controls::RenderLook::Idle => "Render the cut with the last settings (cmd-E); Render… in the chat sets them".into(),
                controls::RenderLook::Disabled => "Open or start a cut to render it".into(),
            };
        }
        ui.view(cx, ids!(cut_keys_sheet)).set_visible(cx, self.keys_sheet);
        // The transport.
        let playing = self.playing.is_some();
        ui.button(cx, ids!(cut_play)).set_visible(cx, !playing);
        ui.button(cx, ids!(cut_pause)).set_visible(cx, playing);
        let rate = self.playing.map_or(0.0, |playing| playing.rate);
        let shuttling = rate != 0.0 && rate != 1.0;
        ui.view(cx, ids!(cut_shuttle)).set_visible(cx, shuttling);
        ui.label(cx, ids!(cut_shuttle_text)).set_text(cx, &format!("{}{}×", if rate < 0.0 { "−" } else { "" }, rate.abs()));
        let mark = |t: Option<f64>| t.map_or("--:--:--:--".to_string(), |t| timecode(t, fps));
        ui.label(cx, ids!(cut_mark_in)).set_text(cx, &mark(self.marks.0));
        ui.label(cx, ids!(cut_mark_out)).set_text(cx, &mark(self.marks.1));
        let duration = match self.marks {
            (Some(a), Some(b)) => Some((b - a).abs()),
            _ => None,
        };
        ui.label(cx, ids!(cut_mark_dur)).set_text(cx, &mark(duration));
        // Narrow viewers keep fewer readouts; nothing overlaps.
        let viewer = laid_out_rect(cx, ui.widget(cx, ids!(cut_preview)).area()).size.x;
        let (full, some) = (viewer >= 900.0 || viewer == 0.0, viewer >= 760.0 || viewer == 0.0);
        ui.view(cx, ids!(cut_ro_in)).set_visible(cx, some);
        ui.view(cx, ids!(cut_ro_out)).set_visible(cx, some);
        ui.view(cx, ids!(cut_ro_dur)).set_visible(cx, full);
        let compact = match self.marks {
            (Some(a), Some(b)) => format!("{} – {}", timecode(a.min(b), fps), timecode(a.max(b), fps)),
            (Some(a), None) | (None, Some(a)) => format!("{}", timecode(a, fps)),
            _ => String::new(),
        };
        ui.view(cx, ids!(cut_ro_compact)).set_visible(cx, !some && !compact.is_empty());
        ui.label(cx, ids!(cut_ro_compact_text)).set_text(cx, &compact);
        for (id, set) in [(live_id!(cut_mark_in), self.marks.0.is_some()), (live_id!(cut_mark_out), self.marks.1.is_some()), (live_id!(cut_mark_dur), duration.is_some())] {
            let mut label = ui.widget(cx, &[id]);
            let ink = Vec4f::from_u32(if set { p.text } else { p.dim });
            script_apply_eval!(cx, label, {draw_text +: {color: #(ink)}});
        }
        // Toggles.
        Self::lit(cx, ui, live_id!(cut_safe), self.safe_zones);
        Self::lit(cx, ui, live_id!(cut_loop), self.loop_on);
        Self::lit(cx, ui, live_id!(cut_snap), !self.snap_off);
        Self::lit(cx, ui, live_id!(cut_skim), !self.skim_off);
        // The sidebar, the toggles, the search hint and the caption: the explorer's.
        let used = self.lib.rows.iter().filter(|row| self.used_ranges(&row.reference.asset_id.to_string(), row.seconds).len() > 0).count();
        self.lib.paint_chrome(cx, ui, &LANES, self.compact, used);
        // Below 1680 the sidebar shows icons only (design §3.1).
        let window = laid_out_rect(cx, ui.widget(cx, ids!(cut_split_box)).area()).size.x + 16.0;
        let compact = window > 16.0 && window < 1680.0;
        if compact != self.compact {
            self.compact = compact;
            let mut sidebar = ui.widget(cx, ids!(cut_sidebar));
            let width = if compact { 44.0 } else { 128.0 };
            script_apply_eval!(cx, sidebar, {width: #(width)});
            for (id, label) in LANE_LABELS {
                ui.button(cx, &[id, live_id!(lane)]).set_text(cx, if compact { "" } else { label });
            }
            ui.label(cx, ids!(cut_caps_looks)).set_text(cx, if compact { "" } else { "LOOKS" });
        }
        let lib_status = match (&self.armed, self.lib.lane) {
            (Some((preset, transition)), _) => format!("{} armed — click {} · Esc", preset_label(preset), if *transition { "an overlap" } else { "a clip" }),
            (_, Some(PageLane::Store(LibLane::Cuts))) => "Click a cut to open it".to_string(),
            (_, Some(PageLane::Store(LibLane::Decks))) => "Click a deck to open it on the DECK page".to_string(),
            (_, Some(PageLane::Store(_))) => "Drag onto the timeline, or E to add at the playhead".to_string(),
            _ => self.lib.status.clone(),
        };
        ui.label(cx, ids!(cut_lib_status)).set_text(cx, &lib_status);
        self.inspector(cx, ui);
        self.repaint_preview = true;
        self.repaint_library = true;
        ui.redraw(cx);
    }

    /// The Render button's look now.
    fn render_look(&self) -> controls::RenderLook {
        if let Some(render) = &self.render {
            let (frame, frames) = render.progress;
            if render.publishing {
                return controls::RenderLook::Progress(1.0, "Publishing…".into());
            }
            let done = if frames == 0 { 0.0 } else { frame as f64 / frames as f64 };
            let elapsed = Cx::time_now() - self.render_began;
            let left = if done > 0.02 { format!("\t{} left", makepad_library_browser::list::short_time(elapsed / done - elapsed)) } else { String::new() };
            return controls::RenderLook::Progress(done, format!("Rendering {:.0}%{left}", done * 100.0));
        }
        if self.session.is_none() {
            return controls::RenderLook::Disabled;
        }
        if self.render_failed.is_some() {
            return controls::RenderLook::Failed;
        }
        // Renders are ephemeral: the button shares.
        controls::RenderLook::Idle
    }

    /// Where the open cut uses `asset`, as fractions of its `seconds`.
    fn used_ranges(&self, asset: &str, seconds: Option<f64>) -> Vec<(f64, f64)> {
        let Some(session) = &self.session else { return Vec::new() };
        let mut ranges = Vec::new();
        for node in &session.doc.nodes {
            let Some(span) = node.span else { continue };
            match &node.kind {
                CutNodeKind::Clip { src, source_in, speed, .. } if media_id(src) == asset => {
                    let start = match source_in { makepad_story_cut::SourceTime::Seconds(s) => *s, _ => 0.0 };
                    match seconds.filter(|seconds| *seconds > 0.0) {
                        Some(total) => ranges.push(((start / total).clamp(0.0, 1.0), ((start + span.length * speed) / total).clamp(0.0, 1.0))),
                        None => ranges.push((0.0, 1.0)),
                    }
                }
                CutNodeKind::Still { src, .. } | CutNodeKind::AudioClip { src, .. } if media_id(src) == asset => ranges.push((0.0, 1.0)),
                _ => {}
            }
        }
        ranges
    }

    fn paint_library(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        self.repaint_library = false;
        if self.lib.painted_lane != self.lib.lane {
            self.repaint = true;
        }
        let looks = self.looks_lane().is_some() && self.lib.list_mode;
        let rows = match self.looks_view(cx, ui) {
            Some(rows) => rows,
            None => self.list_rows(cx),
        };
        let (empty, kind) = match self.lib.lane {
            Some(PageLane::Store(lane)) if self.lib.rows.is_empty() && rows.is_empty() => {
                let kind = lane.card();
                let text = if self.lib.status.starts_with("Loading") || self.lib.status.starts_with("The library") {
                    self.lib.status.clone()
                } else if !self.lib.search.trim().is_empty() {
                    format!("Nothing matches “{}” in {}", self.lib.search.trim(), lane.label().to_lowercase())
                } else {
                    match lane {
                        LibLane::Video => "No videos yet".into(),
                        LibLane::Image => "No images yet".into(),
                        LibLane::Audio => "No audio yet".into(),
                        LibLane::Cuts => "No cuts yet · Save one to see it here".into(),
                        LibLane::AppRuns => "No app runs yet".into(),
                        LibLane::Decks => "No decks yet".into(),
                        LibLane::Scores | LibLane::Samples | LibLane::SoundFonts | LibLane::Voices => format!("No {} yet", lane.label().to_lowercase()),
                    }
                };
                (text, kind)
            }
            Some(PageLane::Web) if rows.is_empty() => (
                if self.lib.status.starts_with("Loading") || self.lib.status.starts_with("The library") { self.lib.status.clone() } else { "No web grabs yet · paste a YouTube or web video address above".into() },
                CardKind::Video,
            ),
            _ => (String::new(), CardKind::Video),
        };
        self.paint_web_bar(cx, ui);
        self.paint_detail(cx, ui);
        if let Some(mut list) = self.lib.list(cx, ui).borrow_mut::<CutMediaList>() {
            list.set_rows(cx, rows, empty, kind);
            // The selection is an item, not a place: after the rows change
            // it follows its item, and stays in view.
            let keys: Vec<String> = if looks {
                self.looks_rows.iter().map(|row| row.preset.clone()).collect()
            } else {
                self.lib.rows.iter().map(|row| row.asset_id().to_string()).chain(self.draft_rows().into_iter().map(|(meta, _)| format!("draft:{}", meta.id)))
                    .chain(self.folder_cards().map(|card| format!("folder:{}/{}", card.doc.name, card.path))).collect()
            };
            let key = if self.lib.list_selected != self.lib.list_painted {
                self.lib.list_selected.and_then(|index| keys.get(index).cloned())
            } else {
                self.lib.list_key.clone()
            };
            let index = key.as_ref().and_then(|key| keys.iter().position(|k| k == key));
            if index != self.lib.list_selected {
                if let Some(index) = index {
                    list.ensure_visible(cx, index);
                }
            }
            self.lib.list_selected = index;
            self.lib.list_key = key.filter(|_| index.is_some());
            self.lib.list_painted = index;
            list.set_selected(cx, self.lib.list_selected);
            list.set_list_mode(cx, self.lib.list_mode);
        }
        let thumbs = self.lib.thumbs.iter().map(|(id, (_, texture))| (id.clone(), texture.clone())).collect();
        if let Some(mut preview) = ui.widget(cx, ids!(cut_preview)).borrow_mut::<CutPreview>() {
            preview.set_media(cx, thumbs, self.titles.clone());
        }
    }

    /// The explorer's head (tags, order) and the selected item's preview
    /// and details.
    fn paint_detail(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        let store = matches!(self.lib.lane, Some(PageLane::Store(_) | PageLane::Web));
        self.lib.paint_head(cx, ui);
        let row = self.lib.list_selected.filter(|_| store).and_then(|index| self.lib.rows.get(index)).cloned();
        self.lay_out_info(cx, ui, row.is_some());
        let Some(row) = row else {
            self.stop_asset_play(cx, ui);
            return;
        };
        let Some(PageLane::Store(lane)) = self.lib.lane else { return };
        let fps = self.fps();
        let id = row.reference.asset_id.to_string();
        let kind = lane.card();
        let details = self.details.get(&row.reference).cloned().unwrap_or_default();
        let info = self.film_info.get(&id).copied();
        // A cut's picture plays only as its fresh render.
        let note = match (kind, row.render) {
            (CardKind::Cut, Some(RenderState::Fresh(_))) => "",
            (CardKind::Cut, Some(RenderState::Stale(_))) => "Stale render",
            (CardKind::Cut, _) => "No render",
            _ => "",
        };
        let seconds = details.edit.as_ref().map(|edit| edit.1).or(row.seconds).or(info.map(|info| info.seconds)).unwrap_or(0.0);
        let aspect = info.filter(|info| info.height > 0).map(|info| info.width as f64 / info.height as f64);
        let texture_aspect = self.lib.thumbs.get(&id).map(|(thumb, _)| thumb.width as f64 / thumb.height.max(1) as f64);
        let portrait = aspect.or(texture_aspect).is_some_and(|aspect| aspect < 1.0);
        if portrait != self.lib_portrait {
            self.lib_portrait = portrait;
            self.repaint_library = true;
        }
        let voice = kind == CardKind::Audio && is_voice_take(&row.title, self.details.get(&row.reference));
        if let Some(mut preview) = ui.widget(cx, ids!(cut_lib_preview)).borrow_mut::<makepad_library_browser::list::CutAssetPreview>() {
            preview.voice = voice;
            preview.set_item(cx, kind, self.films.get(&id).cloned(), self.lib.thumbs.get(&id).map(|(_, texture)| texture.clone()), self.peaks.get(&id).cloned(), seconds, note, aspect);
        }
        ui.label(cx, ids!(cut_lib_name)).set_text(cx, &row.title);
        let mut info_head = ui.widget(cx, ids!(cut_info_head));
        let name = format!("INFO   {}", row.title);
        script_apply_eval!(cx, info_head, {text: #(name)});
        // The facts of its kind (§4.8); an unknown one is left out.
        let used_here = self.used_ranges(&id, row.seconds).len();
        let others = match &self.detail_users {
            Some((asked, Some(n))) if *asked == row.reference => {
                let mine = u64::from(used_here > 0 && self.session.as_ref().is_some_and(|session| session.head.is_some()));
                Some(n.saturating_sub(mine))
            }
            _ => None,
        };
        let used = match (used_here, others) {
            (0, Some(0)) => Some("not used".to_string()),
            (0, Some(n)) => Some(format!("{n} other edit{}", if n == 1 { "" } else { "s" })),
            (here, Some(0)) => Some(format!("{here}× in this cut")),
            (here, Some(n)) => Some(format!("{here}× in this cut · {n} other edit{}", if n == 1 { "" } else { "s" })),
            (here, None) if here > 0 => Some(format!("{here}× in this cut")),
            _ => None,
        };
        let file = (!row.format.is_empty()).then(|| {
            let media = row.format.split(" · ").next().unwrap_or("").to_uppercase();
            if row.bytes > 0 { format!("{media} · {}", size_text(row.bytes)) } else { media }
        });
        let picture = info.map(|info| format!("{}×{} · {} fps", info.width, info.height, source::number((info.fps * 100.0).round() / 100.0).trim_end_matches(".0")))
            .or_else(|| row.format.split(" · ").nth(1).map(str::to_string));
        let added = (row.published > 0).then(|| added_text(row.published));
        let duration = (seconds > 0.0).then(|| timecode(seconds, fps));
        let mut facts: Vec<(&str, Option<String>)> = match kind {
            CardKind::Video => vec![
                ("Duration", duration),
                ("Picture", picture),
                ("File", file),
                ("Sound", info.map(|info| match info.sound { Some((channels, rate)) => format!("{} {} kHz", if channels >= 2 { "stereo" } else { "mono" }, rate / 1000), None => "none".into() })),
                ("Added", added),
                ("Used", used),
            ],
            CardKind::Image => vec![("Picture", picture), ("File", file), ("Added", added), ("Used", used)],
            CardKind::Deck => vec![("Added", added)],
            CardKind::Audio => vec![
                ("Duration", duration),
                ("Kind", Some(if voice { "voice take".into() } else { "music".into() })),
                ("File", file),
                ("Added", added),
                ("Used", used),
            ],
            CardKind::AppRun => vec![
                ("App", Some(row.detail.clone()).filter(|detail| !detail.is_empty() && !detail.contains("render"))),
                ("Duration", duration),
                ("Picture", picture),
                ("Render", row.render.map(|render| match render { RenderState::Fresh(_) => "fresh".into(), RenderState::Stale(_) => "app changed".into(), RenderState::None => "none".into() })),
                ("Used", used),
            ],
            CardKind::Cut => {
                // One line per fact: keys and values are two columns, so a value
                // never wraps (review 4, D1).
                let format = details.edit.as_ref().map(|((w, h, _), _, _)| format!("{} · {w}×{h}", aspect_name(*w, *h)));
                let rate = details.edit.as_ref().map(|((_, _, rate), _, _)| format!("{} fps", source::number(*rate).trim_end_matches(".0")));
                let render = row.render.map(|render| match render {
                    RenderState::Fresh(_) => "fresh".to_string(),
                    RenderState::Stale(_) => "stale".into(),
                    RenderState::None => "none".into(),
                });
                vec![
                    ("Format", format),
                    ("Rate", rate),
                    ("Duration", duration),
                    ("Render", render),
                    ("Contains", details.edit.as_ref().map(|edit| edit.2.clone())),
                    ("Used in", others.map(|n| if n == 0 { "not used".to_string() } else { format!("{n} edit{}", if n == 1 { "" } else { "s" }) })),
                    ("Modified", (row.published > 0).then(|| added_text(row.published))),
                ]
            }
        };
        facts.retain(|(_, value)| value.is_some());
        ui.label(cx, ids!(cut_lib_keys)).set_text(cx, &facts.iter().map(|(key, _)| *key).collect::<Vec<_>>().join("\n"));
        // Each value on one line: cut to the column (a wrapped value would
        // put the next values beside the wrong keys).
        let value_w = laid_out_rect(cx, ui.widget(cx, ids!(cut_lib_facts)).area()).size.x;
        let chars = if value_w > 10.0 { (value_w / 5.6).floor() as usize } else { 60 };
        let fit = |value: String| if value.chars().count() > chars.max(4) { format!("{}…", value.chars().take(chars.max(4) - 1).collect::<String>()) } else { value };
        ui.label(cx, ids!(cut_lib_facts)).set_text(cx, &facts.iter().map(|(_, value)| fit(value.clone().unwrap_or_default())).collect::<Vec<_>>().join("\n"));
        let revision = row.reference.revision.to_string();
        let short = if revision.len() > 18 { format!("{}…{}", &revision[..9], &revision[revision.len() - 4..]) } else { revision };
        ui.label(cx, ids!(cut_lib_rev)).set_text(cx, &short);
        for (index, id) in [live_id!(cut_tag_0), live_id!(cut_tag_1), live_id!(cut_tag_2), live_id!(cut_tag_3), live_id!(cut_tag_4), live_id!(cut_tag_5)].into_iter().enumerate() {
            let tag = details.tags.get(index);
            ui.view(cx, &[id]).set_visible(cx, tag.is_some());
            if let Some(tag) = tag {
                ui.label(cx, &[id, live_id!(label)]).set_text(cx, tag);
            }
        }
        ui.view(cx, ids!(cut_lib_tags)).set_visible(cx, !details.tags.is_empty());
        ui.button(cx, ids!(cut_lib_open)).set_visible(cx, lane == LibLane::Cuts);
        ui.button(cx, ids!(cut_lib_show)).set_visible(cx, used_here > 0);
        // The preview well: its height from the column, the player over the
        // picture while it plays.
        let (t, playing) = self.asset_play.as_ref().map_or((0.0, false), |play| (play.t, play.playing));
        if let Some(mut preview) = ui.widget(cx, ids!(cut_lib_preview)).borrow_mut::<makepad_library_browser::list::CutAssetPreview>() {
            preview.set_position(cx, t, playing);
        }
        let show_video = self.asset_play.as_ref().is_some_and(|play| play.video && play.started);
        ui.view(cx, ids!(cut_lib_video_box)).set_visible(cx, show_video);
    }

    /// The INFO panel's place: hidden with nothing selected, collapsed to
    /// its head when the grid would keep fewer than two rows (five in the
    /// list), else sized to its content within 60 % of the column.
    fn lay_out_info(&mut self, cx: &mut Cx, ui: &WidgetRef, selected: bool) {
        // The splitter's own area is its bar: measure the column round it.
        let outer = laid_out_rect(cx, ui.widget(cx, ids!(cut_lib_column)).area());
        let column = Rect { pos: outer.pos, size: dvec2(outer.size.x, outer.size.y - 28.0 - 25.0) };
        if selected && column.size.y <= 1.0 {
            // Not laid out yet: again after the next draw.
            self.repaint_library = true;
            ui.splitter(cx, ids!(cut_lib_split)).set_collapse(cx, SplitterCollapse::None);
            return;
        }
        let grid_min = if self.lib.list_mode { 5.0 * 46.0 + 8.0 } else {
            let w = column.size.x;
            let columns = (((w - 8.0) / 140.0).floor()).max(1.0);
            let card = (w - 8.0) / columns - 8.0;
            2.0 * (card * 9.0 / 16.0 + 34.0 + 8.0) + 8.0
        };
        // The panel sizes to what it shows, measured from the last layout
        // (review 4, D1): stacked (well over the facts) when that fits, else
        // the well beside the title and facts; never a row cut through.
        let room = column.size.y - 8.0 - grid_min;
        let chrome = 1.0 + 28.0 + 31.0;
        let text = laid_out_rect(cx, ui.widget(cx, ids!(cut_info_text)).area()).size.y.max(36.0 + 6.0 * 16.0);
        let tail = {
            let rev = laid_out_rect(cx, ui.widget(cx, ids!(cut_info_rev)).area());
            let tags = laid_out_rect(cx, ui.widget(cx, ids!(cut_lib_tags)).area());
            6.0 + rev.size.y.max(18.0) + if tags.size.y > 0.0 { 6.0 + tags.size.y } else { 0.0 }
        };
        let stacked_well = ((column.size.x - 16.0) * 9.0 / 16.0).min(180.0);
        let stacked = chrome + stacked_well + 6.0 + text + tail + 8.0;
        let side_w = ((column.size.x - 16.0) * 0.42).floor();
        let side_well = if self.lib_portrait { text } else { (side_w * 9.0 / 16.0).max(96.0) };
        let side_content = chrome + side_well.max(text) + tail + 8.0;
        let side = stacked > room.max(0.0) || self.lib_portrait;
        let (well, content) = if side { (side_well, side_content) } else { (stacked_well, stacked) };
        let collapsed = selected && (self.info_folded || room < 140.0);
        // What does not fit scrolls, and the panel ends on a whole row.
        let height = if collapsed { 29.0 } else if content <= room { content } else {
            let fixed = chrome + 8.0;
            fixed + ((room - fixed) / 16.0).floor().max(0.0) * 16.0
        };
        let top = ui.widget(cx, ids!(cut_info_top));
        let mut well_view = ui.widget(cx, ids!(cut_lib_well));
        if side {
            let w = side_w;
            if let Some(mut view) = top.borrow_mut::<View>() {
                view.layout.flow = Flow::right();
            }
            script_apply_eval!(cx, well_view, {width: #(w) height: #(well)});
        } else {
            if let Some(mut view) = top.borrow_mut::<View>() {
                view.layout.flow = Flow::Down;
            }
            script_apply_eval!(cx, well_view, {height: #(well)});
            // `Fill` is not a name inside an applied script; set the walk.
            if let Some(mut view) = well_view.borrow_mut::<View>() {
                view.walk.width = Size::fill();
            }
        }
        let key_w = if side { 58.0 } else { 76.0 };
        let mut keys = ui.widget(cx, ids!(cut_lib_keys));
        script_apply_eval!(cx, keys, {width: #(key_w)});
        ui.view(cx, ids!(cut_info_body)).set_visible(cx, !collapsed);
        ui.view(cx, ids!(cut_info_actions)).set_visible(cx, !collapsed);
        let collapse = if selected { SplitterCollapse::None } else { SplitterCollapse::B };
        let split = ui.splitter(cx, ids!(cut_lib_split));
        if split.collapse() != collapse {
            split.set_collapse(cx, collapse);
        }
        if (self.info_measured - content).abs() > 0.5 {
            // Measured again after the next layout.
            self.info_measured = content;
            self.repaint_library = true;
        }
        if selected && column.size.y > 0.0 && (self.info_height - height).abs() > 0.5 && !self.info_dragged {
            self.info_height = height;
            split.set_align(cx, SplitterAlign::FromB(height));
        }
    }

    /// Stop the explorer's player.
    fn stop_asset_play(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        if let Some(play) = self.asset_play.take() {
            if play.started {
                ui.video(cx, ids!(cut_lib_video)).stop_and_cleanup_resources(cx);
            }
            ui.view(cx, ids!(cut_lib_video_box)).set_visible(cx, false);
        }
    }

    /// Space in the library: play or pause the selected item (a video, an
    /// app run's render, a sound, a cut's fresh render).
    fn toggle_asset_play(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        let Some(row) = self.lib.list_selected.and_then(|index| self.lib.rows.get(index)).cloned() else { return };
        if let Some(play) = &mut self.asset_play {
            if play.asset == row.reference && play.started {
                let video = ui.video(cx, ids!(cut_lib_video));
                if play.playing { video.pause_playback(cx) } else { video.resume_playback(cx) }
                play.playing = !play.playing;
                self.repaint_library = true;
                return;
            }
        }
        self.stop_asset_play(cx, ui);
        let media = match self.lib.lane {
            Some(PageLane::Store(LibLane::Video | LibLane::Audio)) => MediaRef::Asset(ids(&row.reference)),
            Some(PageLane::Store(LibLane::AppRuns)) => match row.render {
                Some(RenderState::Fresh(render)) => MediaRef::Asset(ids(&render)),
                _ => {
                    self.lib.status = "No fresh render to play".into();
                    self.repaint = true;
                    return;
                }
            },
            _ => return,
        };
        let video = self.lib.lane != Some(PageLane::Store(LibLane::Audio));
        self.asset_play = Some(AssetPlay { asset: row.reference, video, started: false, playing: false, t: 0.0, seek: None });
        self.send_media(MediaJob::Local { media });
    }

    /// A local file for the explorer's player arrived: start it.
    fn asset_file(&mut self, cx: &mut Cx, ui: &WidgetRef, path: std::path::PathBuf) {
        let Some(play) = &mut self.asset_play else { return };
        let video = ui.video(cx, ids!(cut_lib_video));
        video.set_source(VideoDataSource::Filesystem { path: path.to_string_lossy().into_owned() });
        video.begin_playback(cx);
        // Hidden test runs stay silent.
        if std::env::var_os("MAKEPAD_HIDE_WINDOWS").is_some() {
            video.mute_playback(cx);
        }
        play.started = true;
        play.playing = true;
        // The player over the fitted picture.
        let well = laid_out_rect(cx, ui.widget(cx, ids!(cut_lib_well)).area());
        let id = play.asset.asset_id.to_string();
        let aspect = self.film_info.get(&id).filter(|info| info.height > 0).map_or(16.0 / 9.0, |info| info.width as f64 / info.height as f64);
        let pic = makepad_library_browser::list::CutAssetPreview::fit(Rect { pos: well.pos + dvec2(1.0, 1.0), size: well.size - dvec2(2.0, 4.0) }, aspect);
        let (w, h) = if play.video { (pic.size.x, pic.size.y) } else { (1.0, 1.0) };
        let mut widget = ui.widget(cx, ids!(cut_lib_video));
        script_apply_eval!(cx, widget, {width: #(w) height: #(h)});
        self.repaint_library = true;
    }

    /// The explorer's player: follow its position.
    fn pump_asset_play(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        let Some(play) = &mut self.asset_play else { return };
        if !play.started {
            return;
        }
        let video = ui.video(cx, ids!(cut_lib_video));
        if let Some(seek) = play.seek.take() {
            video.seek_to(cx, (seek * 1000.0) as u64);
        }
        let t = video.current_position_ms() as f64 / 1000.0;
        if video.has_completed() {
            play.playing = false;
        }
        if (t - play.t).abs() > 0.02 || !play.playing {
            play.t = t;
            self.repaint_library = true;
        }
    }

    fn paint_preview(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        self.repaint_preview = false;
        // An open graphic covers the viewer: no renderer is made (a whole
        // edit's load) nor sketch evaluated for it; closing it repaints.
        if motion_edit::is_open(self) {
            return;
        }
        let fps = self.fps();
        let skimming = self.skim.is_some() && self.playing.is_none() && !self.skim_off;
        let shown_t = if skimming { self.skim.unwrap_or(self.playhead) } else { self.playhead };
        ui.button(cx, ids!(cut_time)).set_text(cx, &if skimming { format!("SKIM  {}", timecode(shown_t, fps)) } else { timecode(shown_t, fps) });
        let p = makepad_look::palette::get(cx);
        let ink = if self.time_error.is_some_and(|since| Cx::time_now() - since < 0.6) { p.danger } else if skimming { p.glass_ink_soft } else { p.text_max };
        if self.time_ink != Some(ink) {
            // Not per frame: a script apply is costly beside a frame.
            self.time_ink = Some(ink);
            let mut time = ui.widget(cx, ids!(cut_time));
            let ink = Vec4f::from_u32(ink);
            script_apply_eval!(cx, time, {draw_text +: {color: #(ink) color_hover: #(ink) color_focus: #(ink)}});
        }
        let levels = if self.playing.is_some() { self.meter_levels() } else { [0.0, 0.0] };
        if let Some(mut meter) = ui.widget(cx, ids!(cut_meter)).borrow_mut::<controls::CutMeter>() {
            meter.set_levels(cx, levels);
        }
        if let Some(mut timeline) = ui.widget(cx, ids!(cut_timeline)).borrow_mut::<CutTimeline>() {
            timeline.set_playhead(cx, self.playhead);
            // A key or the transport moved the playhead out of view.
            timeline.follow(cx);
        }
        let Some(session) = &self.session else { return };
        let doc = session.doc.clone();
        let revision = session.revision;
        let t = shown_t;
        // The sketch stands in only until the renderer has a frame: while
        // it has, playing and scrubbing need no evaluation here.
        let has_frame = self.rendered == Some(revision)
            && ui.widget(cx, ids!(cut_preview)).borrow::<CutPreview>().is_some_and(|preview| preview.has_frame());
        let items = (!has_frame).then(|| {
            let (eval, sizes) = self.eval(&doc);
            sketch(&eval.frame_plan(t), &|media| sizes.get(media).copied())
        });
        // A new revision is a new renderer (a render is of one exact revision).
        let renderer = (self.rendered != Some(revision)).then(|| {
            let job = CutJob { doc: doc.clone(), marks: HashMap::new(), resolver: Box::new(store::LazyStoreMedia::new()), audio: false };
            Box::new(RenderPreview::new(cx, job).with_sound()) as Box<dyn PreviewSource>
        });
        self.rendered = Some(revision);
        // The banner may name any node; effects first.
        let mut nodes: Vec<String> = doc.nodes.iter().filter(|node| matches!(node.kind, CutNodeKind::Fx { .. })).map(|node| node.id.clone()).collect();
        nodes.extend(doc.nodes.iter().filter(|node| !matches!(node.kind, CutNodeKind::Fx { .. })).map(|node| node.id.clone()));
        let platform = self.platform();
        if let Some(mut preview) = ui.widget(cx, ids!(cut_preview)).borrow_mut::<CutPreview>() {
            preview.set_output(cx, doc.output.width, doc.output.height);
            if let Some(items) = items {
                preview.set_sketch(cx, items);
            }
            preview.set_overlays(cx, self.safe_zones, platform, skimming, nodes);
            if let Some(renderer) = renderer {
                // The cut's panel saved what the shown picture holds
                // already: the new revision loads behind it.
                if cut_panel::take_held_switch(&mut self.cut_panel) {
                    preview.set_source_held(cx, renderer);
                } else {
                    preview.set_source(cx, Some(renderer));
                }
            }
            // Library thumbnails stand in for a clip the viewer has no
            // frame of yet (a fast scrub onto a clip not read yet).
            let seen = (revision, self.lib.thumbs.len(), self.film_info.len());
            if self.stand_ins_seen != Some(seen) {
                self.stand_ins_seen = Some(seen);
                let mut pictures = HashMap::new();
                for node in &doc.nodes {
                    let CutNodeKind::Clip { src: MediaRef::Asset(asset), .. } = &node.kind else { continue };
                    let Some((thumb, texture)) = self.lib.thumbs.get(&asset.id) else { continue };
                    // The source's size when known, else the thumbnail's
                    // (the same aspect): the fit is right either way.
                    let size = self.film_info.get(&asset.id).map_or((thumb.width as u32, thumb.height as u32), |info| (info.width, info.height));
                    pictures.insert(MediaRef::Asset(asset.clone()), (texture.clone(), size.0, size.1));
                }
                preview.set_stand_ins(pictures);
            }
            // A frame asked for now is recorded in this display frame and
            // shows in the next: a playing viewer asks for the time it will
            // be on screen, so the picture keeps step with the playhead.
            let playing = self.playing.filter(|_| !skimming);
            let ahead = playing.map_or(0.0, |playing| self.refresh * playing.rate);
            preview.set_playing(cx, playing.map(|playing| (playing.rate, doc.output.fps, t)));
            preview.seek(cx, (t + ahead).clamp(0.0, doc.duration));
        }
    }

    /// The cut's sound at the playhead, from its sources' loudness: every
    /// block that sounds there, at its fade and ducking, summed by power.
    fn meter_levels(&self) -> [f64; 2] {
        let t = self.playhead;
        let mut power = 0.0;
        for block in &self.projection.blocks {
            let (Some(sound), true) = (&block.sound, t >= block.start && t < block.start + block.length) else { continue };
            let Some(peaks) = sound.media.as_ref().and_then(|media| self.peaks.get(media)) else { continue };
            let local = t - block.start;
            let at = sound.source_in + local * sound.speed;
            let fade = if local < sound.fade_in { local / sound.fade_in.max(1e-6) } else if local > block.length - sound.fade_out { (block.length - local) / sound.fade_out.max(1e-6) } else { 1.0 };
            let duck = if sound.ducked.iter().any(|(a, b)| t >= *a && t < *b) { 10f64.powf(sound.duck_db / 20.0) } else { 1.0 };
            let level = peaks.rms_over(at, at + 0.05) * fade.clamp(0.0, 1.0) * duck;
            power += level * level;
        }
        let level = power.sqrt().min(1.0);
        [level, level]
    }

}

impl StoryApp {
    /// The page's pump.
    pub(super) fn pump_video_cut(&mut self, cx: &mut Cx) {
        if let Some((reference, title)) = self.video_cut.deck_request.take() {
            self.set_page(cx, StoryPage::Deck);
            self.deck.open_request(reference, title);
        }
        if let Some(card) = self.video_cut.deck_card_request.take() {
            self.set_page(cx, StoryPage::Deck);
            let ui = self.ui.clone();
            self.deck.open_card(cx, &ui, card);
        }
        self.video_cut.pump(cx, &self.ui, self.page == StoryPage::Cut);
        self.pump_generate(cx);
        self.pump_new_video(cx);
        self.pump_generate_segments(cx);
        self.pump_storyboard(cx);
    }
}

#[cfg(test)]
mod thumb_size_tests {
    use super::{thumb_width, THUMB_SHORT_MIN};

    /// The renderer's height for a width (thumbnail.rs scale: rounded from the aspect).
    fn height(ow: f64, oh: f64, width: u32) -> f64 {
        (oh * width as f64 / ow).round()
    }

    #[test]
    fn the_short_side_reaches_the_store_minimum_with_the_aspect_kept() {
        for (ow, oh) in [(1920.0, 1080.0), (2560.0, 1080.0), (3440.0, 1440.0), (1080.0, 1920.0), (1000.0, 1000.0), (4096.0, 1716.0)] {
            let w = thumb_width((ow, oh));
            let h = height(ow, oh, w);
            assert!((w as f64).min(h) >= THUMB_SHORT_MIN, "{ow}x{oh} -> {w}x{h}");
        }
        assert_eq!(thumb_width((1920.0, 1080.0)), 512);
        assert_eq!(thumb_width((1080.0, 1920.0)), 288);
    }
}

#[cfg(test)]
mod dock_layout_tests {
    use super::*;

    #[test]
    fn the_layout_is_one_record_of_named_entries() {
        let layout = DockLayout::parse("cut_split=404 chat_split=360 chat_split.fold=B bogus deck_code_split=x");
        assert_eq!(layout.size("cut_split"), Some(404.0));
        assert_eq!(layout.size("chat_split"), Some(360.0));
        assert_eq!(layout.size("deck_code_split"), None);
        assert_eq!(layout.size("cut_panel_split"), None);
        assert!(layout.fold("chat_split") == SplitterCollapse::B && layout.fold("cut_split") == SplitterCollapse::None);
        assert_eq!(DockLayout::parse(&layout.text()).text(), layout.text());
        // Every docked splitter has one name, once.
        let mut names: Vec<&str> = LAYOUT.iter().map(|(name, ..)| *name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), LAYOUT.len());
    }
}

#[cfg(test)]
mod format_tests {
    use super::format_size;

    #[test]
    fn the_new_cut_formats_are_the_four_shapes() {
        assert_eq!(format_size("16:9"), Some((1920, 1080)));
        assert_eq!(format_size("9:16"), Some((1080, 1920)));
        assert_eq!(format_size("1:1"), Some((1080, 1080)));
        assert_eq!(format_size("4:5"), Some((1080, 1350)));
        assert_eq!(format_size("3:2"), None);
    }
}

/// The rect a widget was last laid out at, or an empty one while it has not drawn (hidden, or
/// not yet reached): `Area::rect` reports an area with no instances as a misuse, which a
/// measuring pass over widgets that may be hidden must not trip.
fn laid_out_rect(cx: &Cx, area: Area) -> Rect {
    if area.is_valid(cx) { area.rect(cx) } else { Rect::default() }
}
