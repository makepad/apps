//! The storyboard: a 2D board beside the
//! CUT page's timeline where the person and the AI work out the video's
//! story. Time runs across, rows run down (story, visuals, voiceover,
//! music, references); the items are scene cards, notes and references.
//!
//! The board is the input and the cut its output (the user: "just like a
//! codeview has an AI's code output"). The board records which of its
//! revisions and cards made which parts of the cut; when the board
//! changes the page shows what is out of date, and **Update edit** applies
//! the changes incrementally (`run.rs`): mechanical ones at once, the ones
//! that need judgement as scoped tasks for the Generate agent. Hand edits
//! in the cut are kept.
//!
//! The board is the open cut's: EDIT's view switch (Timeline · Graph ·
//! Source · Storyboard) shows it in the timeline's place. It is autosaved
//! beside the cut's draft and published to the library (as an
//! `AssetKind::Storyboard` revision naming its references) whenever the
//! edit is saved (`io.rs`).
//!
//! This module is a child of `video_cut` (declared there with `#[path]`,
//! like the control API's host) so it reads the page's session, library
//! rows and thumbnails without widening their visibility; its own state is
//! a UI-thread-local. Its hooks: the page's actions, events and pump, the
//! library's drag and drop, the app's pump, and the control API's
//! `storyboard_*` tools (`run.rs`).
//!
//! Main-loop cost: the pump reads at most a few worker replies, compares
//! the page's revision and selection, and re-projects the board only when
//! its source changes (loading a board is linear in its source: a few KB).

mod canvas;
mod io;
mod run;

pub(crate) use run::{tool, tool_reply, Answer, TOOL_NAMES};

use super::store::{StoreJob};
use makepad_library_browser::lanes::{LibLane};
use super::VideoCut;
use super::super::StoryApp;
use canvas::{Badges, BoardAction, StoryboardCanvas};
use makepad_asset_data::AssetRevisionRef;
use makepad_story_storyboard::edit::{Header, Op, Patch, Value};
use makepad_story_storyboard::{build, tools as board_tools, Body, Item, Media, Reference, Role, Row, RowKind, Scene, Storyboard};
use makepad_widgets::*;
use std::cell::RefCell;
use std::collections::HashMap;

/// How long after a change the board is written to disk.
const DEBOUNCE: f64 = 1.0;
/// Undo steps kept.
const UNDO: usize = 200;
/// How long a changed board of a saved cut rests before it is published.
const PUBLISH_REST: f64 = 20.0;
/// The timeline pane's height while the board shows.
const BOARD_HEIGHT: f64 = 590.0;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    let BarTool = StageButton{width: Fit height: 22 margin: 0 padding: Inset{left: 8 right: 8} spacing: 6
        icon_walk: Walk{width: 11 height: 11}
        draw_icon +: {color: stage.label}
        draw_text +: {text_style: theme.font_regular{font_size: 9}}}
    let BarToggle = StageIconButton{width: 24 height: 22 icon_walk: Walk{width: 11 height: Fit}}
    let FieldLabel = Label{width: Fill text: "" draw_text +: {color: stage.secondary text_style: theme.font_bold{font_size: 7.5}}}
    let BoardField = View{visible: false width: Fill height: Fit flow: Down spacing: 3 padding: Inset{left: 12 right: 12 top: 5 bottom: 3}
        label := FieldLabel{}
        input := TextInput{width: Fill height: Fit margin: 0 padding: Inset{left: 6 right: 6 top: 4 bottom: 4} empty_text: "" is_multiline: true submit_on_enter: true
            draw_bg +: {color: stage.well color_hover: stage.well color_focus: stage.well border_size: 1.0 border_color: stage.border}
            draw_text +: {color: stage.text_bright text_style: theme.font_regular{font_size: 9}}}}

    mod.widgets.StoryboardPane = View{width: Fill height: Fill flow: Down visible: false
        board_bar := View{width: Fill height: 36 flow: Right spacing: 6 padding: Inset{left: 12 right: 8} align: Align{y: 0.5}
            StageTick{width: Fit text: "STORYBOARD"}
            View{width: 6 height: 1}
            Tip{text: "A scene card on the story row at the playhead (S)" board_add_scene := BarTool{text: "Scene" draw_icon +: {svg: crate_resource("self:resources/icons/plus.svg")}}}
            Tip{text: "A note at the playhead; on the voiceover row it is a line (N). Double-click the board for one anywhere" board_add_note := BarTool{text: "Note" draw_icon +: {svg: crate_resource("self:resources/icons/text.svg")}}}
            Tip{text: "Paste a picture or a link (cmd-V), or drag library media or files onto the board" board_paste := BarTool{text: "Paste" draw_icon +: {svg: crate_resource("self:resources/icons/image.svg")}}}
            SolidView{width: 1 height: 18 draw_bg +: {color: stage.separator}}
            View{width: 4 height: 1}
            board_status := Label{width: Fill max_lines: 1 flow: Flow.Right{wrap: false} text: "" draw_text +: {color: stage.secondary text_style: theme.font_regular{font_size: 8}}}
            SolidView{width: 1 height: 18 draw_bg +: {color: stage.separator}}
            Tip{text: "Snapping to seconds, beats and edges (hold cmd to invert while dragging)" board_snap := BarToggle{draw_icon +: {svg: crate_resource("self:resources/icons/edit/magnet.svg")}}}
            Tip{text: "Zoom out (−)" board_zoom_out := BarToggle{draw_icon +: {svg: crate_resource("self:resources/icons/edit/zoom_out.svg")}}}
            Tip{text: "Zoom in (+)" board_zoom_in := BarToggle{draw_icon +: {svg: crate_resource("self:resources/icons/edit/zoom_in.svg")}}}
            Tip{text: "Show the whole board (shift-Z)" board_fit := BarToggle{draw_icon +: {svg: crate_resource("self:resources/icons/fit.svg")}}}
        }
        SolidView{width: Fill height: 1 draw_bg +: {color: stage.separator}}
        View{width: Fill height: Fill flow: Right
            board_canvas := mod.widgets.StoryboardCanvas{}
            SolidView{width: 1 height: Fill draw_bg +: {color: stage.separator}}
            board_insp := ScrollYView{width: 272 height: Fill flow: Down spacing: 0 padding: Inset{top: 6 bottom: 12}
                show_bg: true draw_bg +: {color: stage.panel}
                View{width: Fill height: 30 flow: Right spacing: 8 padding: Inset{left: 12 right: 12} align: Align{y: 0.5}
                    board_kind := RoundedView{width: Fit height: 18 padding: Inset{left: 6 right: 6} align: Align{y: 0.5} show_bg: true
                        draw_bg +: {color: stage.raised border_radius: 3.0}
                        board_kind_text := Label{width: Fit text: "BOARD" draw_text +: {color: stage.label text_style: theme.font_bold{font_size: 7.5}}}}
                    board_span := Label{width: Fill text: "" draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 8}}}
                }
                // Custom widgets have no visibility of their own: a box each.
                board_legend_box := View{visible: false width: Fill height: Fit board_legend := mod.widgets.StoryboardLegend{}}
                board_frame_box := View{visible: false width: Fill height: Fit board_frame := mod.widgets.StoryboardFrame{}}
                board_role_row := View{visible: false width: Fill height: Fit flow: Down spacing: 4 padding: Inset{left: 12 right: 12 top: 4 bottom: 4}
                    FieldLabel{text: "ROLE"}
                    Tip{text: "Reference: only to look at for style. Use: in the video as it is. Generate: source for a generative segment" board_role := CutSegmented{width: Fill labels: ["Reference" "Use" "Generate"]}}
                }
                board_f_title := BoardField{}
                board_f_skill := BoardField{}
                board_f_text := BoardField{}
                board_f_intent := BoardField{}
                board_f_mood := BoardField{}
                board_f_look := BoardField{}
                board_f_kit := BoardField{}
                board_f_music := BoardField{}
                board_f_media := BoardField{}
                board_f_stack := BoardField{}
                board_f_label := BoardField{}
                board_f_note := BoardField{}
                board_f_url := BoardField{}
                board_f_prompt := BoardField{}
                board_f_negative := BoardField{}
                board_f_flow := BoardField{}
                board_f_seed := BoardField{}
                board_f_from := BoardField{}
                board_f_brief := BoardField{}
                board_f_style := BoardField{}
                board_f_duration := BoardField{}
                board_f_bpm := BoardField{}
                board_f_layout := BoardField{}
                board_f_hero := BoardField{}
                board_f_out := BoardField{}
                board_f_idea := BoardField{}
                board_f_palette := BoardField{}
                board_f_body := BoardField{}
                board_f_refs := BoardField{}
                board_f_bars := BoardField{}
                board_f_camera := BoardField{}
                board_f_why := BoardField{}
                board_f_lyric := BoardField{}
                board_f_band := BoardField{}
                board_f_pun := BoardField{}
                board_f_set := BoardField{}
                board_f_voice := BoardField{}
                board_f_device := BoardField{}
                board_f_name := BoardField{}
                board_f_value := BoardField{}
                board_actions := View{width: Fill height: Fit flow: Right spacing: 6 padding: Inset{left: 12 right: 12 top: 10}
                    Tip{text: "Duplicate the selection (cmd-D)" board_duplicate := BarTool{text: "Duplicate"}}
                    Tip{text: "Delete the selection (⌫)" board_delete := BarTool{text: "Delete"}}
                    Tip{text: "Pick this treatment (the others on its row are un-picked)" board_choose := BarTool{visible: false text: "Choose"}}
                }
                board_help := Label{width: Fill text: "" padding: Inset{left: 12 right: 12 top: 12} draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 8}}}
            }
        }
    }
}

pub fn register(vm: &mut ScriptVm) {
    canvas::script_mod(vm);
    script_mod(vm);
}

/// The inspector's fields: `(widget, board field, label)`.
const FIELDS: &[(LiveId, &str, &str)] = &[
    (live_id!(board_f_title), "title", "TITLE"),
    (live_id!(board_f_text), "text", "TEXT"),
    (live_id!(board_f_intent), "intent", "INTENT · THE ONE IDEA"),
    (live_id!(board_f_mood), "mood", "MOOD"),
    (live_id!(board_f_look), "look", "LOOK · WHAT IS ON SCREEN"),
    (live_id!(board_f_kit), "kit", "STYLE KIT"),
    (live_id!(board_f_music), "music", "MUSIC"),
    (live_id!(board_f_media), "media", "MEDIA IT NEEDS"),
    (live_id!(board_f_stack), "stack", "LAYER STACK"),
    (live_id!(board_f_label), "label", "LABEL"),
    (live_id!(board_f_note), "note", "WHY IT IS HERE"),
    (live_id!(board_f_url), "url", "LINK"),
    (live_id!(board_f_prompt), "prompt", "GENERATE · PROMPT"),
    (live_id!(board_f_negative), "negative", "GENERATE · AVOID"),
    (live_id!(board_f_flow), "flow", "GENERATE · FLOW PRESET"),
    (live_id!(board_f_seed), "seed", "GENERATE · SEED"),
    (live_id!(board_f_from), "from", "GENERATE FROM (A PICTURE REFERENCE)"),
    (live_id!(board_f_brief), "b_brief", "BRIEF"),
    (live_id!(board_f_style), "b_style", "STYLE"),
    (live_id!(board_f_duration), "b_duration", "LENGTH · SECONDS"),
    (live_id!(board_f_bpm), "b_bpm", "TEMPO · BPM"),
    (live_id!(board_f_layout), "b_layout", "FRAME LAYOUT · shorts OR EMPTY"),
    (live_id!(board_f_skill), "skill", "SKILL · THE CHAPTER'S NAME"),
    (live_id!(board_f_hero), "hero", "HERO ELEMENT"),
    (live_id!(board_f_out), "out", "TRANSITION OUT"),
    (live_id!(board_f_idea), "idea", "IDEA"),
    (live_id!(board_f_palette), "palette", "PALETTE · #rrggbb …"),
    (live_id!(board_f_body), "body", "PAGE"),
    (live_id!(board_f_refs), "refs", "REFERENCES · ITEM IDS"),
    (live_id!(board_f_bars), "bars", "SHOT · BARS"),
    (live_id!(board_f_camera), "camera", "SHOT · CAMERA"),
    (live_id!(board_f_why), "why", "SHOT · WHY"),
    (live_id!(board_f_lyric), "lyric", "SHOT · LYRIC"),
    (live_id!(board_f_band), "band", "FRAME BAND"),
    (live_id!(board_f_pun), "pun", "PUN · THE JOKE THE LINE MAKES"),
    (live_id!(board_f_set), "set", "SET · THE SCENE THAT MAKES IT"),
    (live_id!(board_f_voice), "voice", "TYPE VOICE"),
    (live_id!(board_f_device), "device", "DEVICE · WHAT THE TYPE DOES"),
    (live_id!(board_f_name), "name", "RUNNING DEVICE"),
    (live_id!(board_f_value), "value", "VALUE HERE"),
];

/// A picture being imported onto the board: where it goes.
struct Landing {
    row: String,
    t: f64,
    name: String,
}

#[derive(Default)]
struct Desk {
    started: bool,
    io: Option<io::Io>,
    /// The open cut's board key (its draft id) and whether its board was
    /// asked for / has answered.
    key: Option<String>,
    asked: Option<String>,
    /// The board's source (`None`: the cut has no board yet).
    source: Option<String>,
    board: Option<Storyboard>,
    undo: Vec<String>,
    redo: Vec<String>,
    selected: Vec<String>,
    written: Option<String>,
    changed_at: Option<f64>,
    /// The board as the AI last read or changed it (`storyboard_changes`).
    ai_seen: Option<String>,
    /// The content key of the source last published, and its library
    /// revision.
    published: Option<(String, AssetRevisionRef)>,
    publishing: bool,
    /// The cut head the board last saw (a new one: the cut was saved).
    head_seen: Option<AssetRevisionRef>,
    status: String,
    shown: bool,
    repaint: bool,
    snap_off: bool,
    tickets: u64,
    /// Clipboard request -> (cut key, reference row, playhead at the request).
    pastes: HashMap<u64, (Option<String>, String, f64)>,
    paste_text_before_key: bool,
    landings: HashMap<u64, Landing>,
    /// The inspector shows this item (or the board) at this source.
    inspected: Option<(Option<String>, u64)>,
    /// Bumped on every change.
    serial: u64,
    /// When the board last changed (for publishing it once it rests).
    last_change: Option<f64>,
    /// The update run in progress, and API answers waiting to be read.
    run: Option<run::Run>,
    replies: HashMap<u64, makepad_studio_api::CallResult>,
    next_request: u64,
    /// Pictures the canvas has: asset ids with a texture handed over.
    pictures_given: usize,
    /// The page's timeline height before the board took more of it.
    split_before: Option<SplitterAlign>,
    /// Web pages pasted as references, to grab (`item`, `url`) once Story
    /// has web grabs; until then the queue is dropped.
    grab_queue: Vec<(String, String)>,
    /// Pictures asked of the library: when, and how often (a store that
    /// was still starting is asked again).
    thumbs_asked: HashMap<String, (f64, u32)>,
}

thread_local! {
    static DESK: RefCell<Option<Box<Desk>>> = const { RefCell::new(None) };
}

fn take() -> Box<Desk> {
    DESK.with(|cell| cell.borrow_mut().take()).unwrap_or_default()
}

fn put(desk: Box<Desk>) {
    DESK.with(|cell| *cell.borrow_mut() = Some(desk));
}

/// The page's view switch: show the board (or not).
pub fn set_shown(cx: &mut Cx, ui: &WidgetRef, shown: bool) {
    let mut desk = take();
    if desk.shown != shown {
        desk.shown = shown;
        desk.repaint = true;
        desk.inspected = None;
        // The board wants height (five rows of cards): the timeline pane
        // grows while it shows, and goes back after.
        let split = ui.splitter(cx, ids!(cut_split));
        if shown {
            let before = split.align();
            if let Some(SplitterAlign::FromB(size)) = before {
                if size < BOARD_HEIGHT {
                    desk.split_before = before;
                    split.set_align(cx, SplitterAlign::FromB(BOARD_HEIGHT));
                }
            }
        } else if let Some(before) = desk.split_before.take() {
            split.set_align(cx, before);
        }
        if shown {
            if let Some(canvas) = ui.widget(cx, ids!(board_canvas)).borrow::<StoryboardCanvas>() {
                canvas.focus(cx);
            }
        }
    }
    put(desk);
}

pub fn stop() {
    DESK.with(|cell| {
        if let Some(desk) = cell.borrow_mut().as_mut() {
            if let (Some(io), Some(key), Some(source)) = (&desk.io, &desk.key, &desk.source) {
                if desk.written.as_ref() != Some(source) {
                    io.send(io::Job::Write { key: key.clone(), source: source.clone() });
                }
            }
            if let Some(io) = &mut desk.io {
                io.stop();
            }
        }
    });
}

fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |since| since.as_secs_f64())
}

impl Desk {
    /// Replace the board's source (one undo step). `ai`: the AI made it
    /// (it is not reported back to it as the person's change).
    fn change(&mut self, source: String, ai: bool) -> Result<(), String> {
        let (board, _) = Storyboard::load(&source).map_err(|diags| diags.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))?;
        if self.source.as_ref() == Some(&source) {
            return Ok(());
        }
        if let Some(previous) = self.source.take() {
            self.undo.push(previous);
            if self.undo.len() > UNDO {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.selected.retain(|id| board.item(id).is_some());
        self.source = Some(source.clone());
        self.board = Some(board);
        if ai {
            self.ai_seen = Some(source);
        }
        self.changed_at = Some(now());
        self.last_change = self.changed_at;
        self.serial += 1;
        self.repaint = true;
        Ok(())
    }

    fn op(&mut self, op: Op) -> Result<Vec<String>, String> {
        let source = self.source.clone().ok_or("the cut has no storyboard yet")?;
        let applied = board_tools::op(&source, op)?;
        let changed = applied.changed.clone();
        self.change(applied.source, false)?;
        Ok(changed)
    }

    fn ops(&mut self, ops: Vec<Op>) -> Result<Vec<String>, String> {
        let source = self.source.clone().ok_or("the cut has no storyboard yet")?;
        let applied = makepad_story_storyboard::edit::apply_all(&source, &ops)?;
        let changed = applied.changed.clone();
        self.change(applied.source, false)?;
        Ok(changed)
    }

    fn step_history(&mut self, redo: bool) {
        let (from, to) = if redo { (&mut self.redo, &mut self.undo) } else { (&mut self.undo, &mut self.redo) };
        let Some(source) = from.pop() else { return };
        if let Some(current) = self.source.take() {
            to.push(current);
        }
        if let Ok((board, _)) = Storyboard::load(&source) {
            self.selected.retain(|id| board.item(id).is_some());
            self.board = Some(board);
        }
        self.source = Some(source);
        self.changed_at = Some(now());
        self.last_change = self.changed_at;
        self.serial += 1;
        self.repaint = true;
        self.status = if redo { "Redo".into() } else { "Undo".into() };
    }

    /// Start a board for the open cut (a new one, from its label and
    /// length).
    fn ensure(&mut self, page: &VideoCut) -> Result<(), String> {
        if self.source.is_some() {
            return Ok(());
        }
        let session = page.session.as_ref().ok_or("open or start a cut first: the storyboard is the cut's")?;
        let doc = &session.doc;
        let source = board_tools::new_board(&doc.label, doc.duration.max(1.0), (doc.bpm > 0.0).then_some(doc.bpm), "", "", "");
        let source = board_tools::op(&source, Op::Header(Header { format: Some((doc.output.width, doc.output.height)), ..Default::default() }))?.source;
        self.change(source, false)
    }

    fn board(&self) -> Option<&Storyboard> {
        self.board.as_ref()
    }

    /// A row of `kind` (added when the board has none).
    fn row_of(&mut self, kind: RowKind) -> Result<String, String> {
        if let Some(row) = self.board().and_then(|board| board.row_of(kind)) {
            return Ok(row.id.clone());
        }
        self.op(Op::AddRow(Row { id: String::new(), kind, label: kind.label().to_string() }))?;
        self.board().and_then(|board| board.row_of(kind)).map(|row| row.id.clone()).ok_or_else(|| "the row was not added".to_string())
    }

    /// Add an item and select it.
    fn add(&mut self, item: Item) -> Result<String, String> {
        let changed = self.op(Op::Add(item))?;
        let id = changed.first().cloned().unwrap_or_default();
        self.selected = vec![id.clone()];
        Ok(id)
    }
}

/// The default length of a new item on a row.
fn default_length(kind: RowKind, bpm: Option<f64>) -> f64 {
    let bar = bpm.map(|bpm| 240.0 / bpm);
    match kind {
        RowKind::Story => bar.map_or(4.0, |bar| bar * 2.0),
        RowKind::Voice => 3.0,
        RowKind::Music => 8.0,
        // A chapter is a bar long by default.
        RowKind::Chapters => bar.unwrap_or(2.0),
        RowKind::Treatments | RowKind::Bible => 8.0,
        RowKind::Lyrics => 3.0,
        _ => bar.unwrap_or(2.0).max(2.0),
    }
}

fn new_item(board: &Storyboard, row: &str, t: f64) -> Item {
    let kind = board.row(row).map_or(RowKind::Notes, |row| row.kind);
    let length = default_length(kind, board.bpm);
    let body = match kind {
        RowKind::Story => Body::Scene(Scene { title: format!("Scene {}", board.scenes().len() + 1), ..Default::default() }),
        RowKind::Chapters => Body::Chapter(makepad_story_storyboard::Chapter { skill: format!("CHAPTER {}", board.chapters().len() + 1), ..Default::default() }),
        RowKind::Treatments => Body::Treatment(makepad_story_storyboard::Treatment { title: "Treatment".into(), ..Default::default() }),
        RowKind::Bible => Body::Page(makepad_story_storyboard::Page { title: "Page".into(), ..Default::default() }),
        RowKind::Lyrics => Body::Lyric(makepad_story_storyboard::Lyric::default()),
        // A new occurrence carries on the last device's name.
        RowKind::Devices => Body::Device(makepad_story_storyboard::Device { name: board.devices().last().map(|(name, _)| name.clone()).unwrap_or_else(|| "Device".into()), ..Default::default() }),
        RowKind::Refs | RowKind::Music => Body::Ref(Reference { media: Some(Media::Url), label: "Reference".into(), ..Default::default() }),
        _ => Body::Note { text: String::new() },
    };
    Item { id: String::new(), row: row.to_string(), start: t.max(0.0), length, color: None, band: None, body }
}

/// The key of the open cut's board: its draft id.
fn key_of(page: &VideoCut) -> Option<String> {
    page.session.as_ref()?;
    page.autosave.id.clone()
}

// ---------------------------------------------------------------- page hooks

/// Every pump of the page: the board follows the open cut, drafts are
/// written, a saved cut publishes its board, and the widgets follow.
pub fn pump(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef, shown: bool) {
    let mut desk = take();
    pump_desk(&mut desk, page, cx, ui, shown);
    put(desk);
}

fn pump_desk(desk: &mut Desk, page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef, shown: bool) {
    if !desk.started {
        desk.started = true;
        match io::Io::new(cx) {
            Ok(io) => desk.io = Some(io),
            Err(error) => desk.status = format!("storyboard: {error}"),
        }
    }
    // The open cut's board.
    let key = key_of(page);
    if key != desk.key {
        let head = page.session.as_ref().and_then(|session| session.head);
        // An untitled edit saved for the first time keeps its board.
        let carried = match (&desk.key, &key, &desk.head_seen, head) {
            (Some(old), Some(new), None, Some(head)) if old.starts_with("untitled-") && *new == head.asset_id.to_string() && desk.source.is_some() => {
                if let Some(io) = &desk.io {
                    io.send(io::Job::Rename { from: old.clone(), to: new.clone() });
                }
                true
            }
            _ => false,
        };
        if !carried {
            // Another cut: its own board (or none), history and selection.
            if let (Some(io), Some(old), Some(source)) = (&desk.io, &desk.key, &desk.source) {
                if desk.written.as_ref() != Some(source) {
                    io.send(io::Job::Write { key: old.clone(), source: source.clone() });
                }
            }
            desk.source = None;
            desk.board = None;
            desk.undo.clear();
            desk.redo.clear();
            desk.selected.clear();
            desk.written = None;
            desk.ai_seen = None;
            desk.published = None;
            desk.asked = None;
            desk.inspected = None;
            desk.run = None;
        }
        desk.pastes.clear();
        desk.paste_text_before_key = false;
        desk.key = key.clone();
        desk.head_seen = head;
        desk.repaint = true;
    }
    if let (Some(key), None) = (&desk.key, &desk.asked) {
        if let Some(io) = &desk.io {
            io.send(io::Job::Load { key: key.clone() });
            desk.asked = Some(key.clone());
        }
    }
    // Worker replies.
    for _ in 0..4 {
        let Some(reply) = desk.io.as_ref().and_then(io::Io::try_recv) else { break };
        match reply {
            io::Reply::Loaded { key, source, published } => {
                if desk.key.as_ref() == Some(&key) && desk.published.is_none() {
                    desk.published = published;
                }
                if desk.key.as_ref() == Some(&key) && desk.source.is_none() {
                    if let Some(source) = source {
                        match Storyboard::load(&source) {
                            Ok((board, _)) => {
                                desk.board = Some(board);
                                desk.written = Some(source.clone());
                                desk.ai_seen = Some(source.clone());
                                // Changed since it was last published: it goes
                                // out soon (once the cut has a library head).
                                if desk.published.as_ref().is_none_or(|(key, _)| *key != io::content_key(&source)) {
                                    desk.last_change = Some(now() - PUBLISH_REST + 3.0);
                                }
                                desk.source = Some(source);
                                desk.serial += 1;
                            }
                            Err(diags) => desk.status = format!("The saved storyboard does not load: {}", diags.first().map(ToString::to_string).unwrap_or_default()),
                        }
                    }
                    desk.repaint = true;
                }
            }
            io::Reply::Published { key, result } => {
                desk.publishing = false;
                match result {
                    Ok(reference) if desk.key.as_ref() == Some(&key) => {
                        if let Some(source) = desk.source.clone() {
                            desk.published = Some((io::content_key(&source), reference));
                        }
                        desk.status = "Storyboard saved to the library beside the cut".into();
                    }
                    Ok(_) => {}
                    Err(error) => desk.status = format!("The storyboard was not saved to the library: {error}"),
                }
                desk.repaint = true;
            }
            io::Reply::Picture { ticket, result } => {
                let landing = desk.landings.remove(&ticket);
                match (result, landing) {
                    (Ok((asset, title, _portrait)), Some(landing)) => {
                        let reference = Reference { media: Some(Media::Image), src: Some(makepad_story_storyboard::AssetRef { asset_id: asset.asset_id.to_string(), revision: asset.revision.to_string() }), label: title, ..Default::default() };
                        let length = desk.board().map_or(4.0, |board| default_length(RowKind::Refs, board.bpm));
                        let item = Item { id: String::new(), row: landing.row, start: landing.t, length, color: None, band: None, body: Body::Ref(reference) };
                        match desk.add(item) {
                            Ok(_) => desk.status = format!("Added {} to the board and the library", landing.name),
                            Err(error) => desk.status = format!("Not added: {error}"),
                        }
                    }
                    (Err(error), _) => desk.status = format!("The picture was not added: {error}"),
                    _ => {}
                }
                desk.repaint = true;
            }
            io::Reply::Failed(error) => desk.status = error,
        }
    }
    // Autosave.
    if let (Some(changed), Some(io), Some(key), Some(source)) = (desk.changed_at, &desk.io, &desk.key, &desk.source) {
        if now() - changed >= DEBOUNCE && desk.written.as_ref() != Some(source) {
            io.send(io::Job::Write { key: key.clone(), source: source.clone() });
            desk.written = Some(source.clone());
            desk.changed_at = None;
        }
    }
    // The cut was saved: its board goes to the library beside it. A
    // board changed later follows once it has rested a while (the cut
    // being saved already, there is nothing else to wait for).
    let head = page.session.as_ref().and_then(|session| session.head);
    if head != desk.head_seen {
        desk.head_seen = head;
        if let (Some(head), Some(source)) = (head, desk.source.clone()) {
            publish(desk, &source, Some(head.asset_id.to_string()));
        }
    } else if let (Some(head), Some(source), Some(changed)) = (head, desk.source.clone(), desk.last_change) {
        if now() - changed >= PUBLISH_REST && !desk.publishing {
            desk.last_change = None;
            publish(desk, &source, Some(head.asset_id.to_string()));
        }
    }
    if !shown {
        return;
    }
    paint(desk, page, cx, ui);
}

/// Publish the board (a new revision of its asset when it has one), with
/// the cut's asset id written into it.
fn publish(desk: &mut Desk, source: &str, edit: Option<String>) {
    let mut source = source.to_string();
    if let Some(edit) = &edit {
        if desk.board().is_some_and(|board| board.edit.as_deref() != Some(edit.as_str())) {
            if let Ok(applied) = board_tools::op(&source, Op::Header(Header { edit: Some(Some(edit.clone())), ..Default::default() })) {
                let _ = desk.change(applied.source.clone(), false);
                source = applied.source;
            }
        }
    }
    if desk.publishing || desk.published.as_ref().is_some_and(|(published, _)| *published == io::content_key(&source)) {
        return;
    }
    let Some(board) = desk.board.clone() else { return };
    let dependencies = board.dependencies().into_iter().filter_map(|asset| exact(&asset)).collect();
    let asset = desk.published.as_ref().map(|(_, reference)| reference.asset_id);
    if let (Some(io), Some(key)) = (&desk.io, &desk.key) {
        io.send(io::Job::Publish { key: key.clone(), title: board.title.clone(), source, dependencies, asset, edit });
        desk.publishing = true;
    }
}

fn exact(asset: &makepad_story_storyboard::AssetRef) -> Option<AssetRevisionRef> {
    super::exact(&makepad_story_cut::AssetRef { id: asset.asset_id.clone(), rev: asset.revision.clone() })
}

/// The widgets follow the board, the page and the selection.
fn paint(desk: &mut Desk, page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef) {
    let canvas = ui.widget(cx, ids!(board_canvas));
    // Pictures: every reference's thumbnail the page has (asked for once).
    if let Some(board) = desk.board.clone() {
        let mut asks = Vec::new();
        let now = now();
        for asset in board.dependencies() {
            if page.lib.thumbs.contains_key(&asset.asset_id) {
                continue;
            }
            // Not in after a while: ask again (three times at most).
            if let Some((when, tries)) = desk.thumbs_asked.get(&asset.asset_id).copied() {
                if now - when > 8.0 && tries < 3 {
                    page.requested.remove(&asset.asset_id);
                }
            }
            if page.requested.insert(asset.asset_id.clone()) {
                let tries = desk.thumbs_asked.get(&asset.asset_id).map_or(0, |(_, tries)| *tries);
                desk.thumbs_asked.insert(asset.asset_id.clone(), (now, tries + 1));
                asks.extend(exact(&asset));
            }
        }
        if !asks.is_empty() {
            page.send(StoreJob::Thumbs { refs: asks });
        }
        let pictures: HashMap<String, Texture> = board.dependencies().iter().filter_map(|asset| page.lib.thumbs.get(&asset.asset_id).map(|(_, texture)| (asset.asset_id.clone(), texture.clone()))).collect();
        if pictures.len() != desk.pictures_given {
            desk.pictures_given = pictures.len();
            desk.repaint = true;
        }
        if desk.repaint {
            if let Some(mut canvas) = canvas.borrow_mut::<StoryboardCanvas>() {
                canvas.set_pictures(cx, pictures);
            }
        }
    }
    if let Some(mut canvas) = canvas.borrow_mut::<StoryboardCanvas>() {
        canvas.set_playhead(cx, page.playhead);
    }
    // The frame layout, with what is in each band at the playhead, and the
    // type-voice legend.
    if let Some(board) = desk.board.as_ref() {
        let voiced = !board.voices.is_empty();
        let legend_box = ui.widget(cx, ids!(board_legend_box));
        if legend_box.visible() != voiced {
            legend_box.set_visible(cx, voiced);
        }
        if voiced {
            if let Some(mut legend) = ui.widget(cx, ids!(board_legend)).borrow_mut::<canvas::StoryboardLegend>() {
                legend.set(cx, board);
            }
        }
        let framed = board.frame().is_some();
        let frame_box = ui.widget(cx, ids!(board_frame_box));
        if frame_box.visible() != framed {
            frame_box.set_visible(cx, framed);
        }
        if framed {
            if let Some(mut frame) = ui.widget(cx, ids!(board_frame)).borrow_mut::<canvas::StoryboardFrame>() {
                frame.set(cx, board, page.playhead);
            }
        }
    }
    if !desk.repaint {
        return;
    }
    desk.repaint = false;
    let open = page.session.is_some();
    let board = desk.board.clone().unwrap_or_else(|| Storyboard { duration: page.session.as_ref().map_or(30.0, |session| session.doc.duration), ..Default::default() }.with_default_rows());
    let (stale, unbuilt) = build::stale(&board);
    if let Some(mut canvas) = canvas.borrow_mut::<StoryboardCanvas>() {
        canvas.set_board(cx, &board, Badges { stale: stale.clone(), unbuilt: unbuilt.clone() });
        canvas.set_selected(cx, desk.selected.clone());
        canvas.set_snap(cx, !desk.snap_off);
        let empty = if !open {
            "Open or start a cut: the storyboard is the plan the cut is made from"
        } else if desk.source.is_none() {
            "Double-click to add a note or a scene · drag media here · cmd-V pastes a picture · or ask the chat and it drafts the board"
        } else {
            "Double-click to add a note or a scene · drag library media or files here · cmd-V pastes a picture or a link"
        };
        canvas.set_empty_text(cx, empty);
    }
    // The bar: what is out of date and what Update would do.
    let status = if !desk.status.is_empty() {
        desk.status.clone()
    } else if desk.source.is_none() {
        "No storyboard yet".to_string()
    } else if board.built.is_none() {
{
            // What the board holds, naming only what it has.
            let vo = board.items.iter().filter(|item| board.row(&item.row).is_some_and(|row| row.kind == RowKind::Voice)).count();
            let refs = board.items.iter().filter(|item| matches!(item.body, Body::Ref(_))).count();
            let parts: Vec<String> = [
                (board.scenes().len(), "scene", "scenes"),
                (board.chapters().len(), "chapter", "chapters"),
                (board.lyrics().len(), "lyric line", "lyric lines"),
                (vo, "voiceover line", "voiceover lines"),
                (board.devices().len(), "running device", "running devices"),
                (refs, "reference", "references"),
            ].into_iter().filter(|(n, _, _)| *n > 0).map(|(n, one, many)| format!("{n} {}", if n == 1 { one } else { many })).collect();
            format!("{} · the cut is not made from it yet", if parts.is_empty() { "empty".to_string() } else { parts.join(" · ") })
        }
    } else if stale.is_empty() && unbuilt.is_empty() {
        "The cut follows the board".to_string()
    } else {
        format!("{} out of date · {} not in the cut yet", stale.len(), unbuilt.len())
    };
    ui.widget(cx, ids!(board_status)).set_text(cx, &status);
    let p = makepad_look::palette::get(cx);
    let lit = |on: bool| if on { (p.accent, p.accent_wash) } else { (p.label, 0x00000000) };
    let (ink, wash) = lit(!desk.snap_off);
    let mut snap = ui.widget(cx, ids!(board_snap));
    script_apply_eval!(cx, snap, {draw_icon +: {color: #(Vec4f::from_u32(ink))} draw_bg +: {color: #(Vec4f::from_u32(wash))}});
    inspect(desk, cx, ui, &board, &stale);
}

/// Fill the inspector for the selection (one item), or the board.
fn inspect(desk: &mut Desk, cx: &mut Cx, ui: &WidgetRef, board: &Storyboard, stale: &[String]) {
    let one = (desk.selected.len() == 1).then(|| desk.selected[0].clone());
    let key = (one.clone(), desk.serial);
    if desk.inspected.as_ref() == Some(&key) {
        return;
    }
    desk.inspected = Some(key);
    let item = one.as_ref().and_then(|id| board.item(id)).cloned();
    let values: Vec<(&str, String)> = match &item {
        Some(item) => {
            let mut values: Vec<(&str, String)> = makepad_story_storyboard::diff::content(item).into_iter().map(|(key, value)| (key, value)).collect();
            if let Body::Scene(scene) = &item.body {
                values.push(("from", scene.gen.from.clone().unwrap_or_default()));
            }
            if let Some(band) = &item.band {
                values.retain(|(key, _)| *key != "band");
                values.push(("band", band.clone()));
            }
            if let Some((_, value)) = values.iter_mut().find(|(k, _)| *k == "voice") {
                *value = value.trim_start_matches('@').to_string();
            }
            // Lists as the fields take them.
            for key in ["palette", "refs"] {
                if let Some((_, value)) = values.iter_mut().find(|(k, _)| *k == key) {
                    *value = value.trim_matches(|c| c == '[' || c == ']').replace(['@', ','], " ").split_whitespace().collect::<Vec<_>>().join(" ");
                }
            }
            values
        }
        None => vec![("title", board.title.clone()), ("b_brief", board.brief.clone()), ("b_style", board.style.clone()), ("kit", board.kit.clone()), ("b_duration", makepad_story_storyboard::splash::number(board.duration)), ("b_bpm", board.bpm.map(|bpm| makepad_story_storyboard::splash::number(bpm).trim_end_matches(".0").to_string()).unwrap_or_default()), ("b_layout", board.layout.clone().unwrap_or_default())],
    };
    let framed = board.frame().is_some();
    let fields: &[&str] = match (item.as_ref().map(|item| &item.body), framed) {
        (Some(Body::Scene(_)), false) => &["title", "intent", "mood", "look", "kit", "music", "media", "stack", "bars", "camera", "why", "lyric", "from", "prompt", "flow", "seed"],
        (Some(Body::Scene(_)), true) => &["title", "band", "intent", "mood", "look", "kit", "music", "media", "stack", "bars", "camera", "why", "lyric", "from", "prompt", "flow", "seed"],
        (Some(Body::Chapter(_)), false) => &["skill", "look", "hero", "out"],
        (Some(Body::Chapter(_)), true) => &["skill", "band", "look", "hero", "out"],
        (Some(Body::Treatment(_)), _) => &["title", "idea", "look", "palette"],
        (Some(Body::Page(_)), _) => &["title", "body", "refs"],
        (Some(Body::Lyric(_)), false) => &["text", "pun", "set", "voice", "device"],
        (Some(Body::Lyric(_)), true) => &["text", "band", "pun", "set", "voice", "device"],
        (Some(Body::Device(_)), _) => &["name", "value", "note"],
        (Some(Body::Note { .. }), false) => &["text"],
        (Some(Body::Note { .. }), true) => &["text", "band"],
        (Some(Body::Ref(reference)), framed) => match (reference.role, framed) {
            (Role::Generate, false) => &["label", "note", "url", "prompt", "negative", "flow", "seed"],
            (Role::Generate, true) => &["label", "band", "note", "url", "prompt", "negative", "flow", "seed"],
            (_, false) => &["label", "note", "url"],
            (_, true) => &["label", "band", "note", "url"],
        },
        (None, _) if desk.source.is_some() => &["title", "b_brief", "b_style", "kit", "b_duration", "b_bpm", "b_layout"],
        (None, _) => &[],
    };
    for (id, field, label) in FIELDS {
        let show = fields.contains(field);
        let row = ui.widget(cx, &[*id]);
        row.set_visible(cx, show);
        if show {
            let label = match (*field, item.as_ref().map(|item| &item.body)) {
                ("title", None) => "WORKING TITLE",
                ("kit", None) => "STYLE KIT",
                _ => label,
            };
            ui.widget(cx, &[*id, live_id!(label)]).set_text(cx, label);
            let value = values.iter().find(|(key, _)| key == field).map(|(_, value)| value.clone()).unwrap_or_default();
            ui.widget(cx, &[*id, live_id!(input)]).set_text(cx, &value);
        }
    }
    let (kind, span) = match &item {
        Some(item) => {
            let kind = match &item.body {
                Body::Scene(_) => format!("SCENE {}", board.scene_number(&item.id).unwrap_or(0)),
                Body::Note { .. } if board.row(&item.row).is_some_and(|row| row.kind == RowKind::Voice) => "VOICEOVER LINE".into(),
                Body::Note { .. } => "NOTE".into(),
                Body::Ref(reference) => format!("{} REFERENCE", reference.media.map_or("", Media::tag).to_uppercase()).trim().to_string(),
                Body::Chapter(_) => format!("CHAPTER {:02}/{:02}", board.chapter_number(&item.id).unwrap_or(0), board.chapters().len()),
                Body::Treatment(treatment) => if treatment.chosen { "TREATMENT · CHOSEN".into() } else { "TREATMENT".into() },
                Body::Page(_) => "BIBLE PAGE".into(),
                Body::Lyric(_) => format!("LINE {:02}", board.line_number(&item.id).unwrap_or(0)),
                Body::Device(device) => format!("DEVICE · {}", device.name.to_uppercase()),
            };
            let state = if stale.contains(&item.id) { " · out of date" } else { "" };
            (kind, format!("{} – {} · {} s{state}", makepad_story_storyboard::layout::time_label(item.start, true), makepad_story_storyboard::layout::time_label(item.end(), true), makepad_story_storyboard::splash::number(item.length)))
        }
        None if desk.selected.len() > 1 => ("SELECTION".into(), format!("{} items", desk.selected.len())),
        None => ("BOARD".into(), if desk.source.is_some() { format!("{} items · {} rows", board.items.len(), board.rows.len()) } else { "no board yet".into() }),
    };
    ui.widget(cx, ids!(board_kind_text)).set_text(cx, &kind);
    ui.widget(cx, ids!(board_span)).set_text(cx, &span);
    let reference = matches!(item.as_ref().map(|item| &item.body), Some(Body::Ref(_)));
    ui.widget(cx, ids!(board_role_row)).set_visible(cx, reference);
    if let Some(Body::Ref(reference)) = item.as_ref().map(|item| &item.body) {
        if let Some(mut segmented) = ui.widget(cx, ids!(board_role)).borrow_mut::<super::controls::CutSegmented>() {
            segmented.set_active(cx, match reference.role {
                Role::Reference => 0,
                Role::Use => 1,
                Role::Generate => 2,
            });
        }
    }
    let choosable = matches!(item.as_ref().map(|item| &item.body), Some(Body::Treatment(treatment)) if !treatment.chosen);
    ui.widget(cx, ids!(board_choose)).set_visible(cx, choosable);
    ui.widget(cx, ids!(board_actions)).set_visible(cx, !desk.selected.is_empty());
    let help = match &item {
        Some(_) => "Enter edits the words in place · drag to move, drag an end to change its length · alt-drag pans",
        None => "Scene cards are the story, one idea each. Notes on the voiceover row are the lines the voice says. References are what the AI looks at (or uses, or generates from). The cut is made from this board; Update edit applies what changed.",
    };
    ui.widget(cx, ids!(board_help)).set_text(cx, help);
}

/// The page's actions: the canvas, the bar and the inspector.
pub fn handle_actions(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
    let mut desk = take();
    handle(&mut desk, page, cx, ui, actions);
    put(desk);
}

fn handle(desk: &mut Desk, page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
    // The page's Save publishes the board beside the cut too, even when
    // the cut itself had nothing new.
    if ui.button(cx, ids!(cut_save)).clicked(actions) {
        if let (Some(head), Some(source)) = (page.session.as_ref().and_then(|session| session.head), desk.source.clone()) {
            publish(desk, &source, Some(head.asset_id.to_string()));
        }
    }
    if !desk.shown {
        return;
    }
    let canvas_ref = ui.widget(cx, ids!(board_canvas));
    let mut result: Result<(), String> = Ok(());
    for action in actions.filter_widget_actions_cast::<BoardAction>(canvas_ref.widget_uid()) {
        desk.status.clear();
        result = result.and(match action {
            BoardAction::Select(ids) => {
                desk.selected = ids;
                desk.repaint = true;
                Ok(())
            }
            BoardAction::Place(places) => desk.op(Op::Place(places)).map(|_| ()),
            BoardAction::Add { row, t } => desk.ensure(page).and_then(|_| {
                let board = desk.board().cloned().unwrap_or_default();
                let item = new_item(&board, &row, t);
                let words = !matches!(item.body, Body::Ref(_));
                let id = desk.add(item)?;
                if words {
                    cut_in_place(cx, ui, &id);
                }
                Ok(())
            }),
            BoardAction::Text { id, text } => desk.board().and_then(|board| board.item(&id)).cloned().map_or(Ok(()), |item| {
                let field = match item.body {
                    Body::Note { .. } => "text",
                    Body::Scene(_) | Body::Treatment(_) | Body::Page(_) => "title",
                    Body::Lyric(_) => "text",
                    Body::Device(_) => "value",
                    Body::Chapter(_) => "skill",
                    Body::Ref(_) => "label",
                };
                if makepad_story_storyboard::diff::content(&item).iter().any(|(key, value)| *key == field && *value == text) {
                    return Ok(());
                }
                desk.op(Op::Update { id, patch: Patch { fields: vec![(field.into(), Value::Text(text))] } }).map(|_| ())
            }),
            BoardAction::Scrub(t) => {
                page.playhead = t;
                page.repaint = true;
                page.repaint_preview = true;
                Ok(())
            }
            BoardAction::Snapped(what) => {
                desk.status = format!("Snapped to {what}");
                desk.repaint = true;
                Ok(())
            }
            BoardAction::Key(key) => keys(desk, page, cx, ui, &key),
            BoardAction::Paste(text) => paste_text(desk, page, &text, page.playhead),
            BoardAction::PasteClipboard => {
                // macOS and Windows deliver the native text paste before its KeyV.
                let result = paste(desk, page, cx);
                desk.paste_text_before_key = result.is_ok();
                result
            }
            BoardAction::None => Ok(()),
        });
    }
    // The bar.
    let bar: Vec<LiveId> = [live_id!(board_add_scene), live_id!(board_add_note), live_id!(board_paste), live_id!(board_snap), live_id!(board_zoom_out), live_id!(board_zoom_in), live_id!(board_fit), live_id!(board_duplicate), live_id!(board_delete), live_id!(board_choose)]
        .into_iter().filter(|id| ui.button(cx, &[*id]).clicked(actions)).collect();
    let clicked = |id: &[LiveId]| bar.contains(&id[0]);
    if clicked(ids!(board_add_scene)) {
        result = result.and(add_at_playhead(desk, page, cx, ui, RowKind::Story));
    }
    if clicked(ids!(board_add_note)) {
        result = result.and(add_at_playhead(desk, page, cx, ui, RowKind::Notes));
    }
    if clicked(ids!(board_paste)) {
        result = result.and(paste(desk, page, cx));
    }
    if clicked(ids!(board_snap)) {
        desk.snap_off = !desk.snap_off;
        desk.repaint = true;
    }
    for (id, factor) in [(live_id!(board_zoom_out), 0.8), (live_id!(board_zoom_in), 1.25)] {
        if clicked(&[id]) {
            if let Some(mut canvas) = canvas_ref.borrow_mut::<StoryboardCanvas>() {
                canvas.zoom(cx, factor);
            }
        }
    }
    if clicked(ids!(board_fit)) {
        if let Some(mut canvas) = canvas_ref.borrow_mut::<StoryboardCanvas>() {
            canvas.fit(cx);
        }
    }
    if clicked(ids!(board_duplicate)) {
        result = result.and(duplicate(desk));
    }
    if clicked(ids!(board_delete)) {
        let ids = desk.selected.clone();
        if !ids.is_empty() {
            result = result.and(desk.op(Op::Delete(ids)).map(|_| ()));
        }
    }
    if clicked(ids!(board_choose)) {
        if let Some(id) = desk.selected.first().cloned() {
            let source = desk.source.clone().unwrap_or_default();
            let args = makepad_strict_json::obj(vec![("id", makepad_strict_json::s(id)), ("chosen", makepad_strict_json::Value::Bool(true))]);
            result = result.and(match board_tools::call("storyboard_update", &args, &source) {
                Some(Ok(called)) => called.source.map_or(Ok(()), |source| desk.change(source, false)),
                Some(Err(error)) => Err(error),
                None => Ok(()),
            });
        }
    }
    // The inspector's fields: written on Enter or when they lose focus.
    for (id, field, _) in FIELDS {
        let input = ui.text_input(cx, &[*id, live_id!(input)]);
        let committed = input.returned(actions).map(|(text, _)| text).or_else(|| input.key_focus_lost(actions).then(|| input.text()));
        if let Some(text) = committed {
            result = result.and(set_field(desk, page, field, text.trim_end_matches('\n').to_string()));
        }
    }
    let role = ui.widget(cx, ids!(board_role));
    for action in actions.filter_widget_actions_cast::<super::controls::SegmentAction>(role.widget_uid()) {
        if let super::controls::SegmentAction::Selected(index) = action {
            if let Some(id) = desk.selected.first().cloned() {
                let tag = ["reference", "use", "generate"][index.min(2)];
                result = result.and(desk.op(Op::Update { id, patch: Patch { fields: vec![("role".into(), Value::Tag(tag.into()))] } }).map(|_| ()));
            }
        }
    }
    if let Err(error) = result {
        desk.status = format!("Not changed: {error}");
        desk.repaint = true;
    }
}

fn cut_in_place(cx: &mut Cx, ui: &WidgetRef, id: &str) {
    if let Some(mut canvas) = ui.widget(cx, ids!(board_canvas)).borrow_mut::<StoryboardCanvas>() {
        canvas.cut_text(cx, id);
    }
}

fn add_at_playhead(desk: &mut Desk, page: &VideoCut, cx: &mut Cx, ui: &WidgetRef, kind: RowKind) -> Result<(), String> {
    desk.ensure(page)?;
    let row = desk.row_of(kind)?;
    let board = desk.board().cloned().unwrap_or_default();
    let item = new_item(&board, &row, page.playhead.min(board.duration));
    let id = desk.add(item)?;
    cut_in_place(cx, ui, &id);
    Ok(())
}

fn duplicate(desk: &mut Desk) -> Result<(), String> {
    let Some(board) = desk.board().cloned() else { return Ok(()) };
    let items: Vec<Item> = desk.selected.iter().filter_map(|id| board.item(id)).cloned().collect();
    if items.is_empty() {
        return Ok(());
    }
    // After the selection, keeping their offsets.
    let end = items.iter().map(Item::end).fold(0.0, f64::max);
    let start = items.iter().map(|item| item.start).fold(f64::MAX, f64::min);
    let ops: Vec<Op> = items.into_iter().map(|mut item| {
        item.id.clear();
        item.start += end - start;
        Op::Add(item)
    }).collect();
    let changed = desk.ops(ops)?;
    desk.selected = changed;
    Ok(())
}

fn set_field(desk: &mut Desk, page: &VideoCut, field: &str, text: String) -> Result<(), String> {
    let one = (desk.selected.len() == 1).then(|| desk.selected[0].clone());
    match one {
        Some(id) => {
            let Some(item) = desk.board().and_then(|board| board.item(&id)).cloned() else { return Ok(()) };
            let current = if field == "from" {
                match &item.body {
                    Body::Scene(scene) => scene.gen.from.clone().unwrap_or_default(),
                    _ => String::new(),
                }
            } else {
                makepad_story_storyboard::diff::content(&item).into_iter().find(|(key, _)| *key == field).map(|(_, value)| value).unwrap_or_default()
            };
            if current == text {
                return Ok(());
            }
            let value = match field {
                "seed" if text.trim().is_empty() => Value::Clear,
                "seed" => Value::Int(text.trim().parse().map_err(|_| "the seed is a whole number".to_string())?),
                "from" if text.trim().is_empty() => Value::Clear,
                "from" => Value::Tag(text.trim().trim_start_matches('@').to_string()),
                "band" if text.trim().is_empty() => Value::Clear,
                "band" => Value::Tag(text.trim().trim_start_matches('@').to_string()),
                "voice" if text.trim().is_empty() => Value::Clear,
                "voice" => Value::Tag(text.trim().trim_start_matches('@').to_string()),
                "palette" => Value::Colors(text.split(|c: char| c.is_whitespace() || c == ',').filter(|w| !w.is_empty()).map(|w| makepad_story_storyboard::splash::parse_color(w.trim_start_matches('#')).ok_or_else(|| format!("`{w}` is not a colour: write #rrggbb"))).collect::<Result<_, _>>()?),
                "refs" => Value::Tags(text.split(|c: char| c.is_whitespace() || c == ',').filter(|w| !w.is_empty()).map(|w| w.trim_start_matches('@').to_string()).collect()),
                _ if text.is_empty() => Value::Clear,
                _ => Value::Text(text),
            };
            desk.op(Op::Update { id, patch: Patch { fields: vec![(field.into(), value)] } }).map(|_| ())
        }
        None => {
            desk.ensure(page)?;
            let board = desk.board().cloned().unwrap_or_default();
            let mut header = Header::default();
            match field {
                "title" if text != board.title => header.title = Some(text),
                "b_brief" if text != board.brief => header.brief = Some(text),
                "b_style" if text != board.style => header.style = Some(text),
                "kit" if text != board.kit => header.kit = Some(text),
                "b_duration" => {
                    let seconds: f64 = text.trim().trim_end_matches('s').trim().parse().map_err(|_| "the length is seconds".to_string())?;
                    if (seconds - board.duration).abs() > 1e-9 {
                        header.duration = Some(seconds);
                    }
                }
                "b_layout" => {
                    let layout = text.trim().trim_start_matches('@');
                    let layout = (!layout.is_empty() && layout != "none").then(|| layout.to_string());
                    if layout != board.layout {
                        header.layout = Some(layout);
                    }
                }
                "b_bpm" => {
                    let bpm = if text.trim().is_empty() { None } else { Some(text.trim().parse::<f64>().map_err(|_| "the tempo is a number of beats a minute".to_string())?) };
                    if bpm != board.bpm {
                        header.bpm = Some(bpm);
                    }
                }
                _ => {}
            }
            if header == Header::default() {
                return Ok(());
            }
            desk.op(Op::Header(header)).map(|_| ())
        }
    }
}

/// Keys the canvas passes up.
fn keys(desk: &mut Desk, page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef, key: &KeyEvent) -> Result<(), String> {
    let paste_text_before_key = std::mem::take(&mut desk.paste_text_before_key);
    let command = key.modifiers.logo || key.modifiers.control;
    match key.key_code {
        KeyCode::Delete | KeyCode::Backspace if !desk.selected.is_empty() => {
            let ids = desk.selected.clone();
            let ripple = key.modifiers.shift;
            let mut ops = vec![Op::Delete(ids.clone())];
            if ripple {
                if let Some(board) = desk.board() {
                    ops.extend(board_tools::close_gap_ops(board, &ids));
                }
            }
            desk.ops(ops).map(|_| ())
        }
        KeyCode::KeyZ if command => {
            desk.step_history(key.modifiers.shift);
            Ok(())
        }
        KeyCode::KeyD if command => duplicate(desk),
        KeyCode::KeyV if command => {
            if paste_text_before_key { Ok(()) }
            else { paste(desk, page, cx) }
        },
        KeyCode::KeyS if !command => add_at_playhead(desk, page, cx, ui, RowKind::Story),
        KeyCode::KeyN if !command => add_at_playhead(desk, page, cx, ui, RowKind::Notes),
        KeyCode::KeyU if !command => run::update(desk, page, false, None),
        KeyCode::Space => {
            page.key(cx, ui, key);
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Capture the destination now; the clipboard reply arrives through the ordinary event loop.
fn paste(desk: &mut Desk, page: &VideoCut, cx: &mut Cx) -> Result<(), String> {
    desk.ensure(page)?;
    let row = desk.row_of(RowKind::Refs)?;
    desk.pastes.insert(cx.read_clipboard(), (key_of(page), row, page.playhead));
    Ok(())
}

fn pasted(desk: &mut Desk, page: &VideoCut, row: String, t: f64, content: &makepad_widgets::makepad_platform::clipboard_read::ClipboardContent) -> Result<(), String> {
    use makepad_widgets::makepad_platform::clipboard_read::ClipboardImage;
    if !content.files.is_empty() {
        for (index, file) in content.files.iter().enumerate() {
            drop_file(desk, &row, t + index as f64 * 0.5, std::path::PathBuf::from(&file.path))?;
        }
        desk.repaint = true;
    } else if let Some(ClipboardImage::Png(png)) = &content.image {
        let name = format!("pasted-{}.png", makepad_library_browser::drafts::now_ms());
        import(desk, row, t, name, Some(png.clone()), None);
        desk.status = "Adding the pasted picture…".into();
        desk.repaint = true;
    } else if let Some(text) = &content.text {
        paste_text(desk, page, text, t)?;
    }
    Ok(())
}

fn import(desk: &mut Desk, row: String, t: f64, name: String, bytes: Option<std::sync::Arc<Vec<u8>>>, path: Option<std::path::PathBuf>) {
    desk.tickets += 1;
    let ticket = desk.tickets;
    desk.landings.insert(ticket, Landing { row, t, name: name.clone() });
    if let Some(io) = &desk.io {
        io.send(io::Job::Picture { ticket, name, bytes, path });
    }
}

/// Where a reference dropped on `row` goes: references stay on the rows
/// that hold them (references, visuals, notes; sound on music), the rest
/// go to the references row at the same time.
fn ref_row(desk: &mut Desk, row: &str, audio: bool) -> Result<String, String> {
    let kind = desk.board().and_then(|board| board.row(row)).map(|row| row.kind);
    match kind {
        Some(RowKind::Refs | RowKind::Visuals | RowKind::Notes) => Ok(row.to_string()),
        Some(RowKind::Music) if audio => Ok(row.to_string()),
        _ => desk.row_of(RowKind::Refs),
    }
}

/// A file dropped (or pasted) onto the board: pictures go to the library
/// and onto the board; anything else is a file reference.
fn drop_file(desk: &mut Desk, row: &str, t: f64, path: std::path::PathBuf) -> Result<(), String> {
    let row = &ref_row(desk, row, false)?;
    let name = path.file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or_default();
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".png") || lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        import(desk, row.to_string(), t, name, None, Some(path));
        desk.status = "Adding the picture…".into();
        return Ok(());
    }
    let media = if [".mp4", ".mov", ".m4v", ".webm"].iter().any(|ext| lower.ends_with(ext)) { Media::Video } else if [".wav", ".mp3", ".m4a", ".aac", ".flac"].iter().any(|ext| lower.ends_with(ext)) { Media::Audio } else { Media::File };
    let length = desk.board().map_or(4.0, |board| default_length(RowKind::Refs, board.bpm));
    let reference = Reference { media: Some(media), file: path.to_string_lossy().to_string(), label: name, note: "Not in the library yet: import it to use it in the cut".into(), ..Default::default() };
    desk.add(Item { id: String::new(), row: row.to_string(), start: t, length, color: None, band: None, body: Body::Ref(reference) }).map(|_| ())
}

/// Text typed or pasted onto the board: a link becomes a reference, other
/// text a note.
fn paste_text(desk: &mut Desk, page: &VideoCut, text: &str, t: f64) -> Result<(), String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(());
    }
    desk.ensure(page)?;
    if text.starts_with("http://") || text.starts_with("https://") {
        let row = desk.row_of(RowKind::Refs)?;
        let length = desk.board().map_or(4.0, |board| default_length(RowKind::Refs, board.bpm));
        let label = text.trim_start_matches("https://").trim_start_matches("http://").split('/').next().unwrap_or(text).to_string();
        let reference = Reference { media: Some(Media::Url), url: text.to_string(), label, ..Default::default() };
        let id = desk.add(Item { id: String::new(), row, start: t, length, color: None, band: None, body: Body::Ref(reference) })?;
        // A video page (YouTube and the like) is grabbed into the library
        // and the reference becomes that video.
        desk.grab_queue.push((id, text.to_string()));
        return Ok(());
    }
    let row = desk.row_of(RowKind::Notes)?;
    desk.add(Item { id: String::new(), row, start: t, length: 3.0, color: None, band: None, body: Body::Note { text: text.to_string() } }).map(|_| ())
}

/// A library item dragged from the explorer: while the board is shown it
/// lands on the board (as a reference), not on the timeline. True when
/// the board took it.
pub fn media_drag(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef, index: usize, abs: Vec2d, dropped: bool) -> bool {
    let mut desk = take();
    let taken = desk.shown;
    if taken {
        let landing = ui.widget(cx, ids!(board_canvas)).borrow::<StoryboardCanvas>().and_then(|canvas| canvas.drop_at(abs));
        let row = page.row_media(index).map(|(kind, row)| (kind, row.reference, row.title.clone(), row.seconds));
        let hint = match (&landing, &row) {
            (Some((board_row, t)), Some((_, _, title, seconds))) => Some((board_row.clone(), *t, seconds.unwrap_or(4.0).min(30.0), title.clone())),
            _ => None,
        };
        if let Some(mut canvas) = ui.widget(cx, ids!(board_canvas)).borrow_mut::<StoryboardCanvas>() {
            canvas.set_drop_hint(cx, if dropped { None } else { hint });
        }
        if dropped {
            let result = match (landing, row) {
                (Some((board_row, t)), Some((kind, reference, title, seconds))) => desk.ensure(page).and_then(|_| {
                    let audio = kind == super::ops::MediaKind::Audio;
                    let board_row = ref_row(&mut desk, &board_row, audio)?;
                    let media = match kind {
                        super::ops::MediaKind::Video | super::ops::MediaKind::AppRun => Media::Video,
                        super::ops::MediaKind::Image => Media::Image,
                        super::ops::MediaKind::Audio => Media::Audio,
                        super::ops::MediaKind::Cut => Media::Cut,
                    };
                    let src = makepad_story_storyboard::AssetRef { asset_id: reference.asset_id.to_string(), revision: reference.revision.to_string() };
                    let music_row = desk.board().and_then(|board| board.row(&board_row)).is_some_and(|row| row.kind == RowKind::Music);
                    let length = if music_row { seconds.unwrap_or(30.0) } else { seconds.unwrap_or(4.0).min(8.0) };
                    let role = if music_row && media == Media::Audio { Role::Use } else { Role::Reference };
                    let reference = Reference { media: Some(media), src: Some(src), label: title.clone(), role, ..Default::default() };
                    let id = desk.add(Item { id: String::new(), row: board_row, start: t, length, color: None, band: None, body: Body::Ref(reference) })?;
                    if music_row && media == Media::Audio && desk.board().is_some_and(|board| board.music.is_none()) {
                        desk.op(Op::Header(Header { music: Some(Some(id)), ..Default::default() }))?;
                    }
                    desk.status = format!("Added {title} to the board");
                    Ok(())
                }),
                (None, _) => Err("dropped outside the board".into()),
                (_, None) => Err("choose a library item".into()),
            };
            if let Err(error) = result {
                desk.status = format!("Not added: {error}");
            }
            desk.repaint = true;
        }
        let _ = LibLane::Video;
    }
    put(desk);
    taken
}

/// Page events: files dragged in from the Finder.
pub fn handle_event(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef, event: &Event) {
    let mut desk = take();
    if matches!(event, Event::WindowLostFocus(_)) { desk.paste_text_before_key = false; }
    if let Event::Clipboard(reply) = event {
        if let Some((key, row, t)) = desk.pastes.remove(&reply.request) {
            if desk.shown && key == key_of(page) {
                if let Err(error) = pasted(&mut desk, page, row, t, &reply.content) {
                    desk.status = format!("Not pasted: {error}");
                    desk.repaint = true;
                }
                ui.redraw(cx);
            }
        }
    }
    if desk.shown {
        match event {
            Event::Drag(drag) => {
                if ui.widget(cx, ids!(board_canvas)).borrow::<StoryboardCanvas>().and_then(|canvas| canvas.drop_at(drag.abs)).is_some() && drag.items.iter().any(|item| matches!(item, DragItem::FilePath { .. })) {
                    *drag.response.lock().unwrap() = DragResponse::Copy;
                }
            }
            Event::Drop(dropped) => {
                let landing = ui.widget(cx, ids!(board_canvas)).borrow::<StoryboardCanvas>().and_then(|canvas| canvas.drop_at(dropped.abs));
                if let Some((row, t)) = landing {
                    if desk.ensure(page).is_ok() {
                        for (index, item) in dropped.items.iter().enumerate() {
                            if let DragItem::FilePath { path, .. } = item {
                                if let Err(error) = drop_file(&mut desk, &row, t + index as f64 * 0.5, std::path::PathBuf::from(path)) {
                                    desk.status = format!("Not added: {error}");
                                }
                            }
                        }
                        desk.repaint = true;
                    }
                }
            }
            _ => {}
        }
    }
    put(desk);
}

/// The chat's suggestions from the board: the `make_video` card with the
/// board as its brief (the AI builds the cut from it) while the cut is not
/// made from it, and, with a scene card selected that the cut was made
/// from, the card to rebuild just that scene. Each is a label, a tool and
/// its prefill.
pub fn chips(page: &VideoCut) -> Vec<(&'static str, &'static str, makepad_strict_json::Value)> {
    use makepad_strict_json::{obj, s as text};
    let desk = take();
    let mut chips = Vec::new();
    if let Some(board) = desk.board() {
        if board.built.is_none() {
            let request = format!("Make the video from the storyboard \"{}\": read it with storyboard_read, follow its scene cards, voiceover lines and references, build the cut, then storyboard_mark_built.", board.title);
            let brief = if board.brief.is_empty() { request } else { format!("{}\n\n{request}", board.brief) };
            chips.push(("Make the video from the board", "make_video", obj(vec![("prompt", text(brief))])));
        }
        if let Some(id) = desk.selected.first() {
            if let Some(ask) = rebuild_request(&desk, page, id) {
                chips.push(("Rebuild this scene", "make_video", obj(vec![("prompt", text(ask))])));
            }
        }
    }
    put(desk);
    chips
}

/// The task for rebuilding scene `id` alone: its card, with only its context.
fn rebuild_request(desk: &Desk, page: &VideoCut, id: &str) -> Option<String> {
    let board = desk.board()?;
    let item = board.items.iter().find(|item| item.id == id)?;
    if !matches!(item.body, Body::Scene(_)) {
        return None;
    }
    let mut built = board.built.clone()?;
    if !built.parts.iter().any(|part| part.item == id) {
        return None;
    }
    for part in &mut built.parts {
        if part.item == id {
            part.sig.clear();
        }
    }
    let blocks = run::blocks_of(page);
    let plan = build::plan(board, &built, &blocks, build::Gap::Keep);
    let ask = plan.asks.iter().find(|ask| ask.item == id).cloned()?;
    let only = build::Plan { asks: vec![ask], ..Default::default() };
    Some(format!("Rebuild this scene of the cut from its card.\n\n{}", only.ask_text()))
}

// ---------------------------------------------------------------- app hooks

impl StoryApp {
    /// The board's own pump at the app level: an update run's calls on
    /// the control API.
    pub(crate) fn pump_storyboard(&mut self, cx: &mut Cx) {
        let mut desk = take();
        run::pump(&mut desk, self, cx);
        put(desk);
    }
}
