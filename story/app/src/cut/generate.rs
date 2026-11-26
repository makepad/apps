//! Generate: the AI video maker, driven from the chat.
//!
//! The chat (`chat.rs`) is the one place to ask for something to be made. A
//! request ("a 30 s 9:16 launch video for Notes, voiceover") is answered
//! with a settings card in the conversation (the `make_video` tool's
//! arguments), prefilled from the request; Confirm calls the tool, which
//! starts the model's loop. The transcript of
//! what the model does (its storyboard, every call, every document change
//! as a diff, the pictures it looked at, its critiques) shows in the run
//! view, and `make_video_note` revises it. The loop
//! itself is `makepad-story-video-agent`; this module drives it on the
//! open cut (a new one when none is open) and executes its calls through
//! Stage's control API, so every change is an ordinary undoable edit, and
//! the person can edit anything by hand at any time: the model is told
//! about those edits before it goes on.
//!
//! This module is a child of the page: it reads the page's state directly,
//! and the page calls it from small hooks (register, actions, pump, stop).
//!
//! Main-loop cost: the agent's poll drains channels (no waiting); the
//! transcript rows are rebuilt only when the agent's generation moves
//! (linear in its entries), and pictures decode on the image cache's
//! worker, once each.

#[path = "generate_song.rs"]
pub(super) mod song;

use super::VideoCut;
use makepad_story_video_agent::{
    Agent, Crew, Team, TeamAction, Author, Backend, Brief, CallState, Doc, Entry, Kind, Limits, Outcome, State, ToolDef, DEFAULT_CLAUDE_MODEL,
};
use makepad_widgets::*;
use std::collections::HashSet;
use std::sync::Arc;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    let GenCaps = Label{width: Fit text: "" draw_text +: {color: stage.secondary text_style: theme.font_bold{font_size: 7.5}}}
    let GenMeta = Label{width: Fit text: "" draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 8}}}
    let GenTool = StageButton{width: Fit height: 22 margin: 0 padding: Inset{left: 8 right: 8} spacing: 6
        draw_text +: {text_style: theme.font_regular{font_size: 9}}}
    let GenPick = DropDown{width: Fill height: 22 margin: 0 labels: [""] draw_text +: {text_style: theme.font_regular{font_size: 8}}}
    let GenChip = ButtonFlatter{width: Fill height: Fit margin: 0 padding: Inset{left: 6 right: 6 top: 4 bottom: 4} align: Align{x: 0.0 y: 0.5} text: ""
        draw_text +: {color: stage.label text_style: theme.font_regular{font_size: 8.5}}}
    let GenBody = Label{width: Fill height: Fit text: "" draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 9}}}
    let GenDetail = Label{width: Fill height: Fit visible: false margin: Inset{left: 8 top: 2 bottom: 4} text: ""
        draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 8}}}
    let GenPic = Image{width: Fill height: 150 visible: false fit: ImageFit.Smallest}

    mod.widgets.CutGenerateListBase = #(CutGenerateList::register_widget(vm))
    mod.widgets.CutGenerateList = set_type_default() do mod.widgets.CutGenerateListBase{
        width: Fill height: Fill
        list := PortalList{
            width: Fill height: Fill flow: Down
            drag_scrolling: false auto_tail: true smooth_tail: true selectable: true

            User := RoundedView{width: Fill height: Fit margin: Inset{top: 6 bottom: 4 left: 24 right: 2}
                padding: Inset{left: 10 top: 6 right: 10 bottom: 6} show_bg: true
                draw_bg +: {color: stage.raised border_radius: 4.0}
                body := GenBody{draw_text +: {color: stage.text_bright}}}
            Say := View{width: Fill height: Fit padding: Inset{left: 4 top: 4 right: 4 bottom: 4}
                body := GenBody{}}
            Fold := View{width: Fill height: Fit flow: Down margin: Inset{top: 2 bottom: 2}
                chip := GenChip{draw_text +: {color: stage.muted}}
                detail := GenDetail{}}
            Card := RoundedView{width: Fill height: Fit flow: Down margin: Inset{top: 4 bottom: 4}
                padding: Inset{left: 10 top: 8 right: 10 bottom: 8} show_bg: true spacing: 4
                draw_bg +: {color: stage.well border_radius: 4.0 border_size: 1.0 border_color: stage.line}
                head := Label{width: Fill text: "" draw_text +: {color: stage.text_hi text_style: theme.font_bold{font_size: 8.5}}}
                body := GenBody{draw_text +: {color: stage.label text_style: theme.font_regular{font_size: 8.5}}}}
            Call := View{width: Fill height: Fit flow: Down margin: Inset{top: 1 bottom: 1}
                chip := GenChip{}
                detail := GenDetail{}
                pics := View{width: Fill height: Fit flow: Down spacing: 4 visible: false margin: Inset{left: 6 right: 6 bottom: 4}
                    pic0 := GenPic{} pic1 := GenPic{} pic2 := GenPic{} pic3 := GenPic{}}}
            Note := View{width: Fill height: Fit padding: Inset{left: 6 top: 2 right: 4 bottom: 2}
                body := GenBody{draw_text +: {color: stage.warm text_style: theme.font_regular{font_size: 8}}}}
            End := View{width: Fill height: Fit padding: Inset{left: 4 top: 6 right: 4 bottom: 8}
                body := GenBody{draw_text +: {color: stage.text_hi text_style: theme.font_bold{font_size: 9}}}}
        }
    }

    // The chat's generation run: what the model does for a request made in
    // the chat (its settings card is the chat's).
    mod.widgets.CutGenerateRun = View{width: Fill height: Fill flow: Down visible: false
        View{width: Fill height: 28 flow: Right spacing: 6 padding: Inset{left: 12 right: 8} align: Align{y: 0.5}
            GenCaps{text: "MAKING"}
            gen_phase := GenMeta{text: ""}
            View{width: Fill height: 1}
            Tip{text: "Stop the model; everything it made stays in the cut and can be undone" gen_stop := GenTool{visible: false text: "Stop"}}
            Tip{text: "Clear this run from the chat (the cut keeps what was made)" gen_new := GenTool{visible: false text: "Clear"}}
        }
        gen_agents := View{width: Fill height: Fit visible: false padding: Inset{left: 12 right: 8 bottom: 6}
            Tip{text: "Whose work the transcript shows" gen_agent := GenPick{labels: ["director"]}}}
        gen_story := View{width: Fill height: Fit flow: Down visible: false padding: Inset{left: 12 right: 8 bottom: 4}
            SolidView{width: Fill height: 1 draw_bg +: {color: stage.separator}}
            gen_story_text := Label{width: Fill height: Fit margin: Inset{top: 6} text: "" draw_text +: {color: stage.label text_style: theme.font_regular{font_size: 8}}}}
        SolidView{width: Fill height: 1 draw_bg +: {color: stage.separator}}
        View{width: Fill height: Fill padding: Inset{left: 8 right: 8}
            gen_list := mod.widgets.CutGenerateList{}}
    }
}

pub fn register(vm: &mut ScriptVm) {
    script_mod(vm);
}

/// The formats the brief offers: size and aspect name.
const FORMATS: [(u32, u32, &str); 4] = [(1080, 1920, "9:16"), (1920, 1080, "16:9"), (1080, 1080, "1:1"), (1080, 1350, "4:5")];

/// One transcript row as the list draws it.
#[derive(Clone, Debug, PartialEq)]
enum Row {
    User(String),
    Say(String),
    /// A folded row: reasoning, a document change. `entry` keys its open
    /// state.
    Fold { entry: usize, head: String, detail: String, open: bool },
    Card { head: String, body: String },
    Call { entry: usize, head: String, detail: String, open: bool, pics: Vec<(String, Arc<[u8]>)> },
    Note(String),
    End(String),
}

#[derive(Script, ScriptHook, Widget)]
pub struct CutGenerateList {
    #[deref]
    view: View,
    #[rust]
    rows: Vec<Row>,
}

impl Widget for CutGenerateList {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            let portal = item.as_portal_list();
            let Some(mut list) = portal.borrow_mut() else { continue };
            list.set_item_range(cx, 0, self.rows.len());
            while let Some(index) = list.next_visible_item(cx) {
                let Some(row) = self.rows.get(index) else { continue };
                match row {
                    Row::User(text) | Row::Say(text) | Row::Note(text) | Row::End(text) => {
                        let template = match row {
                            Row::User(_) => id!(User),
                            Row::Say(_) => id!(Say),
                            Row::Note(_) => id!(Note),
                            _ => id!(End),
                        };
                        let widget = list.item(cx, index, template);
                        widget.label(cx, ids!(body)).set_text(cx, text);
                        widget.draw_all_unscoped(cx);
                    }
                    Row::Fold { head, detail, open, .. } => {
                        let widget = list.item(cx, index, id!(Fold));
                        widget.button(cx, ids!(chip)).set_text(cx, &format!("{} {head}", if *open { "[-]" } else { "[+]" }));
                        let label = widget.label(cx, ids!(detail));
                        label.set_visible(cx, *open);
                        if *open {
                            label.set_text(cx, detail);
                        }
                        widget.draw_all_unscoped(cx);
                    }
                    Row::Card { head, body } => {
                        let widget = list.item(cx, index, id!(Card));
                        widget.label(cx, ids!(head)).set_text(cx, head);
                        widget.label(cx, ids!(body)).set_text(cx, body);
                        widget.draw_all_unscoped(cx);
                    }
                    Row::Call { head, detail, open, pics, .. } => {
                        let widget = list.item(cx, index, id!(Call));
                        widget.button(cx, ids!(chip)).set_text(cx, head);
                        let label = widget.label(cx, ids!(detail));
                        label.set_visible(cx, *open && !detail.is_empty());
                        if *open {
                            label.set_text(cx, detail);
                        }
                        widget.view(cx, ids!(pics)).set_visible(cx, !pics.is_empty());
                        for (slot, id) in [ids!(pic0), ids!(pic1), ids!(pic2), ids!(pic3)].into_iter().enumerate() {
                            let image = widget.image(cx, id);
                            match pics.get(slot) {
                                Some((key, png)) => {
                                    image.set_visible(cx, true);
                                    let _ = image.load_image_from_data_async(cx, std::path::Path::new(key), png.clone());
                                }
                                None => image.set_visible(cx, false),
                            }
                        }
                        widget.draw_all_unscoped(cx);
                    }
                }
            }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }
}

impl CutGenerateList {
    fn set_rows(&mut self, cx: &mut Cx, rows: Vec<Row>) {
        if rows != self.rows {
            self.rows = rows;
            self.view.redraw(cx);
        }
    }

    /// The entries whose fold was clicked.
    fn toggled(&self, cx: &mut Cx, actions: &Actions) -> Vec<usize> {
        let list = self.view.portal_list(cx, ids!(list));
        let mut out = Vec::new();
        for (index, item) in list.items_with_actions(actions) {
            if item.button(cx, ids!(chip)).clicked(actions) {
                if let Some(Row::Fold { entry, .. } | Row::Call { entry, .. }) = self.rows.get(index) {
                    out.push(*entry);
                }
            }
        }
        out
    }
}

/// The page's Generate state.
pub struct Generate {
    /// The generation: its transcript, storyboard and every document
    /// change (`revisions()`, which the Source view shows as diffs).
    pub(super) crew: Option<Crew>,
    /// Which agent's transcript the panel shows (0: the director).
    shown_agent: usize,
    backends: Vec<Backend>,
    backend: usize,
    /// Transcript entries shown unfolded.
    unfolded: HashSet<usize>,
    shown: u64,
    /// Why Generate cannot start now, shown under the brief.
    why: String,
    /// A brief waiting for the cut it makes to open.
    waiting: Option<Brief>,
    /// Keeps the page pumping while the model works.
    timer: Timer,
    /// A headless run (`STAGE_GENERATE_REQUEST`): where its results go.
    headless: Option<Headless>,
    headless_checked: bool,
    /// Calls on the control API, waiting for their answers.
    calls: Vec<Call>,
    /// Scene authors under a director (the chat card's Director authorship).
    team: bool,
    /// The storyboard generation last written to the board, and a write
    /// under way.
    board_written: u64,
    board_call: Option<u64>,
    /// A line from the person waiting for the board's changes to go with
    /// it: the text and the `storyboard_changes` call.
    pub(super) pending_say: Option<(String, Option<u64>)>,
}

/// A call on the control API: which agent made it, its id with the model
/// and with the API, the tool and what it looks at.
struct Call {
    agent: String,
    call_id: String,
    id: u64,
    name: String,
    target: String,
}

/// A generation run without a person: Stage started with
/// `STAGE_GENERATE_REQUEST=<request.json>` (the evaluator's generator
/// contract) makes the video on a new cut, writes the cut, its motion
/// documents, the transcript and `result.json` into the request's
/// `out_dir`, and closes.
struct Headless {
    out_dir: std::path::PathBuf,
    result_path: std::path::PathBuf,
    started: std::time::Instant,
    written: bool,
    /// `publish: true` in the request: save the cut to the library
    /// before closing.
    publish: bool,
    epilogue: Epilogue,
    /// The cut revision the library now holds.
    saved: Option<makepad_strict_json::Value>,
    /// The request's song being imported and timed; the brief waits for it.
    song: Option<std::sync::mpsc::Receiver<Result<song::Song, String>>>,
    song_brief: Option<Brief>,
    /// The song's asset id and its timed words (`vj-lyrics`), for `media.txt`.
    song_lyrics: Option<(String, String)>,
}

/// A publishing headless run after the model finished: save the cut to
/// the library.
#[derive(Clone, Copy, PartialEq)]
enum Epilogue {
    Idle,
    Saving(u64),
    Done,
}

impl Default for Generate {
    fn default() -> Self {
        let backends = vec![
            Backend::ClaudeCode { model: format!("{DEFAULT_CLAUDE_MODEL}[1m]") },
            Backend::ClaudeApi { model: DEFAULT_CLAUDE_MODEL.into() },
            Backend::Hub { model: None },
        ];
        Generate { crew: None, shown_agent: 0, backends, backend: 0, unfolded: HashSet::new(), shown: 0, why: String::new(), waiting: None, timer: Timer::default(), headless: None, headless_checked: false, calls: Vec::new(), team: false, board_written: 0, board_call: None, pending_say: None }
    }
}

impl Generate {
    pub fn busy(&self) -> bool {
        self.crew.as_ref().is_some_and(Crew::busy)
    }
}

/// The building blocks' names, compact, for the model to pick from (the
/// guide explains each). Taken from the crates' own tables, so the menus
/// never drift from what loads.
fn menus() -> String {
    use makepad_motion::{style, text3d, typography};
    use makepad_stage_motion3d::{kits, seq3d};
    let names = |items: Vec<String>| items.join(" ");
    let mut out = String::new();
    out.push_str("- Style kits (`Motion{style: @kit}`): ");
    out.push_str(&style::kits().iter().map(|kit| format!("@{} ({})", kit.name, kit.suits)).collect::<Vec<_>>().join("; "));
    out.push_str(&format!("\n- Text animators (`Text{{animate: @name}}`): {}", names(typography::animators().iter().map(|(name, _, _)| format!("@{name}")).collect())));
    // The Text3D presets come with a host's text pack; without one there are none to offer.
    let text3d_presets = text3d::presets();
    if !text3d_presets.is_empty() {
        out.push_str(&format!("\n- Text3D presets (`Text3D{{preset: @name text: …}}`): {}", names(text3d_presets.iter().map(|preset| format!("@{}", preset.name)).collect())));
    }
    out.push_str(&format!("\n- Seq3D templates (`Seq3D{{template: @name text: …}}`): {}", names(seq3d::templates().iter().map(|(name, _)| format!("@{name}")).collect())));
    out.push_str(&format!("\n- 3D Fx presets (`Fx{{preset: @name}}` in a Scene3D): {}", names(kits::FX.iter().map(|name| format!("@{name}")).collect())));
    out.push_str(&format!("\n- Device mockups (`Device{{kind: @name screen: …}}`): {}; 3D fields: {}", names(kits::DEVICES.iter().map(|n| format!("@{n}")).collect()), names(kits::FIELDS.iter().map(|n| format!("@{n}")).collect())));
    out.push_str("\n- Charts (`Chart{type: @name data: …}`): @bar @line @area @donut @pie @scatter @bubble @network @treemap @map @kpi @counter @timeline");
    out.push_str("\n- Masks and mattes: `Group{mask: … mask_mode: @alpha|@inverse|@luma}`, `Text{inside: <footage or pattern>}`, animators @mask_up @outline_fill");
    out.push_str("\n- Detail: `Vector{icon: \"set/name\" draw_on: …}` (600+ icons; `search_library`/`motion_catalog`), `Repeat{layer: … count layout: @grid|@radial|@along}`, `Pattern`");
    out
}

/// The open cut, as the agent reads it.
fn open_doc(page: &VideoCut) -> Option<Doc> {
    let session = page.session.as_ref()?;
    Some(Doc { key: "cut.splash".into(), label: format!("the cut “{}”", session.doc.label), source: session.source.clone() })
}

// ------------------------------------------------------------------ the make_video tool

fn text_of<'a>(req: &'a makepad_strict_json::Value, key: &str) -> &'a str {
    req.get(key).and_then(makepad_strict_json::Value::as_str).unwrap_or("")
}

fn number_of(req: &makepad_strict_json::Value, key: &str) -> Option<f64> {
    match req.get(key) {
        Some(makepad_strict_json::Value::Int(n)) => Some(*n as f64),
        Some(makepad_strict_json::Value::F64(n)) => Some(*n),
        _ => None,
    }
}

/// A video brief from the `make_video` tool's arguments. `open` is the open
/// cut's size and frame rate (it keeps its own format); `voices` are the
/// library's voices as (name to match, value for the brief). Returns the
/// brief and whether scene authors work under a director.
fn brief_from_args(args: &makepad_strict_json::Value, open: Option<(u32, u32, f64)>, voices: &[(String, String)]) -> Result<(Brief, bool), String> {
    let prompt = text_of(args, "prompt").trim();
    if prompt.is_empty() {
        return Err("Say what to make first".into());
    }
    let (width, height, _) = match text_of(args, "aspect") {
        "" => FORMATS[0],
        aspect => FORMATS.iter().copied().find(|format| format.2 == aspect).ok_or_else(|| format!("`{aspect}` is not a shape: 9:16, 16:9, 1:1 or 4:5"))?,
    };
    let (width, height) = open.map_or((width, height), |(w, h, _)| (w, h));
    let voice = match text_of(args, "voice").trim().to_lowercase().as_str() {
        "" | "none" | "no" | "false" => String::new(),
        "yes" | "true" | "voiceover" => voices.first().map(|voice| voice.1.clone()).unwrap_or_default(),
        name => voices.iter().find(|voice| voice.0.to_lowercase().contains(name)).or(voices.first()).map(|voice| voice.1.clone()).unwrap_or_default(),
    };
    let music = match text_of(args, "music") {
        "library" => "find a fitting track in the library (search_library kind audio) and cut to it".to_string(),
        "none" => "none".to_string(),
        _ => "generate".to_string(),
    };
    let brief = Brief {
        kind: Kind::Cut,
        prompt: prompt.to_string(),
        width,
        height,
        fps: open.map_or(60, |(_, _, fps)| (fps.round() as u32).max(60)),
        length: number_of(args, "seconds").unwrap_or(30.0),
        style: text_of(args, "style").to_string(),
        voice,
        music,
        generate_media: text_of(args, "media") != "library",
        menus: menus(),
        interactive: true,
    };
    Ok((brief, text_of(args, "authorship") == "director"))
}

/// The `make_video` tool: start the model's loop on the open cut (a new
/// one when none is open) from the card's arguments.
fn make_video(page: &mut VideoCut, args: &makepad_strict_json::Value) -> Result<(), String> {
    if page.generate.busy() {
        return Err("A video is being made: stop it first, or add to it in the chat".into());
    }
    let open = page.session.as_ref().map(|session| (session.doc.output.width as u32, session.doc.output.height as u32, session.doc.output.fps));
    let voices: Vec<(String, String)> = page.voices.iter().map(|name| (name.clone(), name.clone())).collect();
    let (brief, team) = brief_from_args(args, open, &voices)?;
    let backend = &page.generate.backends[page.generate.backend];
    if let Some(reason) = backend.unavailable() {
        return Err(format!("{}: {reason}", backend.label()));
    }
    page.generate.why.clear();
    page.generate.team = team;
    if page.session.is_none() {
        // The work lands in a cut on this page: make one in the brief's
        // format, and start when it is open.
        let (w, h, aspect) = FORMATS.iter().copied().find(|(w, h, _)| (*w, *h) == (brief.width, brief.height)).unwrap_or(FORMATS[0]);
        page.new_cut((w, h, aspect));
        page.generate.why = "Opening a new cut…".into();
    }
    page.generate.waiting = Some(brief);
    page.repaint = true;
    Ok(())
}

pub(super) fn handle_actions(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
    // The run controls live in the chat's run view.
    if ui.button(cx, ids!(gen_stop)).clicked(actions) {
        if let Some(crew) = &mut page.generate.crew {
            crew.cancel();
        }
    }
    if let Some(index) = ui.drop_down(cx, ids!(gen_agent)).changed(actions) {
        page.generate.shown_agent = index;
        page.generate.shown = 0;
    }
    if ui.button(cx, ids!(gen_new)).clicked(actions) && !page.generate.busy() {
        page.generate.crew = None;
        page.generate.shown_agent = 0;
        page.generate.unfolded.clear();
        page.generate.shown = 0;
        page.generate.why.clear();
    }
    if let Some(list) = ui.widget(cx, ids!(gen_list)).borrow::<CutGenerateList>() {
        for entry in list.toggled(cx, actions) {
            if !page.generate.unfolded.remove(&entry) {
                page.generate.unfolded.insert(entry);
            }
            page.generate.shown = 0;
        }
    }
}

/// A line from the chat's input while a run is on screen: sent to the
/// model on the next pump, with the person's storyboard edits.
pub(super) fn say(page: &mut VideoCut, words: String) -> bool {
    if page.generate.crew.is_none() || words.trim().is_empty() {
        return false;
    }
    page.generate.pending_say = Some((words, None));
    true
}

/// A change to the open graphic that its agent said is structural (new
/// scenes or documents): a generation by one author with the full guide
/// and tools (the graphic desk's escalation).
pub(super) fn ask_scene(page: &mut VideoCut, ask: &str) -> Result<(), String> {
    if page.generate.busy() || page.generate.waiting.is_some() {
        return Err("the agent is still working".into());
    }
    let session = page.session.as_ref().ok_or("no cut is open")?;
    let (node, title, duration) = super::motion_edit::variants::open_graphic(page).ok_or("no graphic is open")?;
    let node = node.map(|node| format!(" (the Graphic clip `{node}` of this cut)")).unwrap_or_default();
    let prompt = format!(
        "The person is iterating on one graphic, `{}`{node}, open in the graphic desk. Their ask: {}\nChange only that graphic (its Motion document, and the modules it imports when the change belongs there) with small patches, check it, and finish with one line saying what you changed. No storyboard: this is a revision.",
        title, ask.trim(),
    );
    page.generate.team = false;
    page.generate.why.clear();
    page.generate.waiting = Some(Brief {
        kind: Kind::Cut,
        prompt,
        width: session.doc.output.width as u32,
        height: session.doc.output.height as u32,
        fps: (session.doc.output.fps.round() as u32).max(60),
        length: duration,
        style: String::new(),
        voice: String::new(),
        music: "none".into(),
        generate_media: false,
        menus: menus(),
        interactive: true,
    });
    Ok(())
}

/// What the scene agent's generation is doing: working, and the summary it
/// finished with.
pub(super) fn scene_state(page: &VideoCut) -> (bool, Option<String>) {
    let working = page.generate.busy() || page.generate.waiting.is_some();
    let summary = page.generate.crew.as_ref().and_then(|crew| crew.lead().summary().map(str::to_string));
    (working, summary)
}

/// Scene authors for a production's scenes (`new_video.rs`): one per
/// `(id, motion, brief)`, all reading `style`, run as this panel's crew so
/// their work shows here like a director's authors'.
pub(super) fn start_authors(page: &mut VideoCut, backend: &Backend, tools: &[ToolDef], style: &str, tasks: &[(String, String, String)], length: f64) -> Result<(), String> {
    if page.generate.busy() {
        return Err("the agent is still working".into());
    }
    let docs: Vec<Doc> = open_doc(page).into_iter().collect();
    let authors = makepad_story_video_agent::Authors::start(backend, tools, style, tasks, docs, Limits::for_length(length))?;
    page.generate.crew = Some(Crew::Authors(authors));
    page.generate.shown_agent = 0;
    page.generate.unfolded.clear();
    page.generate.shown = 0;
    page.generate.why.clear();
    page.repaint = true;
    Ok(())
}

/// Start the waiting brief once the cut is open, on the control API's
/// tools.
fn begin(page: &mut VideoCut, tools: &[ToolDef]) {
    let Some(doc) = open_doc(page) else { return };
    let Some(brief) = page.generate.waiting.take() else { return };
    let backend = page.generate.backends[page.generate.backend].clone();
    page.generate.unfolded.clear();
    page.generate.shown = 0;
    let started = if page.generate.team {
        Team::start(&backend, tools.to_vec(), &brief, vec![doc], Limits::for_length(brief.length)).map(Crew::Team)
    } else {
        Agent::start_seeing(&backend, tools.to_vec(), &brief, vec![doc], Limits::for_length(brief.length), Vec::new()).map(Crew::Solo)
    };
    match started {
        Ok(crew) => {
            page.generate.crew = Some(crew);
            page.generate.shown_agent = 0;
            page.generate.why.clear();
        }
        Err(error) => page.generate.why = format!("{}: {error}", backend.label()),
    }
    page.repaint = true;
}

/// Drive the loop: start a waiting brief, keep the documents current, and
/// hand back the model's calls for the control API to run (their answers
/// come back through [`answer`]).
pub(super) fn pump(page: &mut VideoCut, cx: &mut Cx, tools: &[ToolDef]) -> Vec<TeamAction> {
    let open = page.session.is_some();
    if page.generate.waiting.is_some() && open && page.queue.is_empty() && page.in_flight.is_none() {
        begin(page, tools);
    }
    let doc = open_doc(page);
    let Some(crew) = page.generate.crew.as_mut() else { return Vec::new() };
    if let Some(doc) = doc {
        crew.observe(vec![doc]);
    }
    let actions = crew.poll();
    if crew.busy() {
        page.generate.timer = cx.start_timeout(0.05);
    }
    actions
}

/// A call's answer from the control API.
pub(super) fn answer(page: &mut VideoCut, agent: &str, call_id: &str, outcome: Outcome) {
    if let Some(crew) = &mut page.generate.crew {
        crew.result(agent, call_id, outcome);
    }
}

pub(super) fn handle_event(page: &mut VideoCut, _cx: &mut Cx, event: &Event) {
    if page.generate.timer.is_event(event).is_some() {
        page.generate.timer = Timer::default();
    }
}

pub(super) fn stop(page: &mut VideoCut) {
    if let Some(crew) = &mut page.generate.crew {
        crew.cancel();
    }
}

pub(super) fn paint(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef) {
    let running = page.generate.crew.is_some() || page.generate.waiting.is_some();
    // A run on the deck shows in DECK's chat, not here.
    let shown = running;
    ui.view(cx, ids!(gen_run)).set_visible(cx, shown);
    if !shown {
        return;
    }
    let generation = page.generate.crew.as_ref().map_or(1, Crew::generation);
    let busy = page.generate.busy();
    ui.button(cx, ids!(gen_stop)).set_visible(cx, busy);
    ui.button(cx, ids!(gen_new)).set_visible(cx, !busy && page.generate.crew.is_some());
    let phase = match page.generate.crew.as_ref().map(Crew::lead) {
        None => page.generate.why.clone(),
        Some(agent) => {
            let what = match agent.state() {
                State::Thinking | State::Working => agent.phase().label().to_string(),
                State::Done => "Done".into(),
                State::Stopped(why) => format!("Stopped: {why}"),
                State::Failed(_) => "Failed".into(),
            };
            format!("{what} · round {} · fixes {} · {}", agent.round(), agent.iterations(), agent.label())
        }
    };
    ui.label(cx, ids!(gen_phase)).set_text(cx, &phase);
    if generation == page.generate.shown {
        return;
    }
    page.generate.shown = generation;
    let Some(crew) = page.generate.crew.as_ref() else {
        ui.view(cx, ids!(gen_story)).set_visible(cx, false);
        ui.view(cx, ids!(gen_agents)).set_visible(cx, false);
        if let Some(mut list) = ui.widget(cx, ids!(gen_list)).borrow_mut::<CutGenerateList>() {
            list.set_rows(cx, Vec::new());
        }
        return;
    };
    let story = crew.lead().storyboard().map(|board| {
        // The scenes at a glance; the style and each scene's detail are in
        // the transcript's storyboard card.
        let mut text = format!("{} · {:.1} s", if board.title.is_empty() { "Storyboard" } else { &board.title }, board.duration());
        for scene in &board.scenes {
            text.push_str(&format!("\n{:>5.1}–{:<5.1} {} · {}", scene.start, scene.start + scene.length, scene.id, scene.idea));
        }
        text
    });
    ui.view(cx, ids!(gen_story)).set_visible(cx, story.is_some());
    ui.label(cx, ids!(gen_story_text)).set_text(cx, story.as_deref().unwrap_or(""));
    // With scene authors, a picker shows whose work the transcript is.
    let agents = crew.agents();
    ui.view(cx, ids!(gen_agents)).set_visible(cx, agents.len() > 1);
    let labels: Vec<String> = agents
        .iter()
        .map(|(id, agent)| {
            let state = match agent.state() {
                State::Thinking | State::Working => agent.phase().label().to_string(),
                State::Done => "done".into(),
                State::Stopped(_) => "stopped".into(),
                State::Failed(_) => "failed".into(),
            };
            format!("{id} · {state}")
        })
        .collect();
    ui.drop_down(cx, ids!(gen_agent)).set_labels(cx, labels);
    let shown = page.generate.shown_agent.min(agents.len() - 1);
    ui.drop_down(cx, ids!(gen_agent)).set_selected_item(cx, shown);
    let rows = rows(agents[shown].1, &page.generate.unfolded);
    if let Some(mut list) = ui.widget(cx, ids!(gen_list)).borrow_mut::<CutGenerateList>() {
        list.set_rows(cx, rows);
    }
}

fn first_lines(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.trim().lines().collect();
    let tail = &all[all.len().saturating_sub(lines)..];
    tail.join("\n")
}

/// The transcript as rows: what the person reads.
fn rows(agent: &Agent, unfolded: &HashSet<usize>) -> Vec<Row> {
    let mut out = Vec::new();
    for (index, entry) in agent.entries().iter().enumerate() {
        let open = unfolded.contains(&index);
        match entry {
            Entry::User(text) => out.push(Row::User(text.clone())),
            Entry::Say(text) if !text.trim().is_empty() => out.push(Row::Say(text.trim().to_string())),
            Entry::Say(_) | Entry::Status(_) => {}
            Entry::Thinking(text) => out.push(Row::Fold {
                entry: index,
                head: format!("Thinking · {}", first_lines(text, 1).chars().take(90).collect::<String>()),
                detail: text.trim().to_string(),
                open,
            }),
            Entry::Storyboard(board) => {
                let mut body = String::new();
                if !board.kit.is_empty() {
                    body.push_str(&format!("kit {}\n", board.kit));
                }
                if !board.style.is_empty() {
                    body.push_str(&format!("{}\n", board.style));
                }
                for scene in &board.scenes {
                    body.push_str(&format!("{:.1}–{:.1} s  {} — {}", scene.start, scene.start + scene.length, scene.id, scene.idea));
                    for (what, text) in [("stack", &scene.stack), ("look", &scene.look), ("voice", &scene.voice), ("music", &scene.music), ("media", &scene.media)] {
                        if !text.is_empty() {
                            body.push_str(&format!("\n    {what}: {text}"));
                        }
                    }
                    body.push('\n');
                }
                out.push(Row::Card { head: format!("Storyboard · {} scenes · {:.1} s", board.scenes.len(), board.duration()), body: body.trim_end().to_string() });
            }
            Entry::Call { call_id, name, summary, state, images } => {
                let (mark, result) = match state {
                    CallState::Running => ("…", String::new()),
                    CallState::Done(text) => ("✓", text.clone()),
                    CallState::Failed(text) => ("✗", text.clone()),
                };
                let what = if summary.is_empty() { name.clone() } else { summary.clone() };
                let head = if result.is_empty() { format!("{mark} {what}") } else { format!("{mark} {what} — {}", result.chars().take(120).collect::<String>()) };
                let pics = images.iter().enumerate().map(|(n, image)| (format!("generate/{call_id}/{n}.png"), image.png.clone())).collect();
                out.push(Row::Call { entry: index, head, detail: result, open, pics });
            }
            Entry::Change(revision) => {
                let Some(revision) = agent.revisions().get(*revision) else { continue };
                let by = if revision.by == Author::Person { "your cut" } else { "changed" };
                out.push(Row::Fold {
                    entry: index,
                    head: format!("Δ {by}: {} · +{} −{}", revision.label, revision.diff.added, revision.diff.removed),
                    detail: revision.diff.text().trim_end().to_string(),
                    open,
                });
            }
            Entry::Critique(critique) => {
                let body = if critique.issues.is_empty() { "No issues.".to_string() } else { critique.issues.iter().map(|issue| format!("• {issue}")).collect::<Vec<_>>().join("\n") };
                out.push(Row::Card { head: format!("Critique · {} · {}", critique.subject, critique.verdict.to_uppercase()), body });
            }
            Entry::Ask { question, options } => {
                let body = if options.is_empty() { "Answer below.".to_string() } else { options.iter().enumerate().map(|(i, option)| format!("{}. {option}", i + 1)).collect::<Vec<_>>().join("\n") };
                out.push(Row::Card { head: format!("Question · {question}"), body });
            }
            Entry::Note(text) => out.push(Row::Note(text.clone())),
            Entry::Done(text) => out.push(Row::End(format!("Done. {text}"))),
            Entry::Stopped(text) => out.push(Row::End(text.clone())),
            Entry::Failed(text) => out.push(Row::End(format!("Failed: {text}"))),
        }
    }
    out
}

/// A fingerprint of a storyboard plan (non-zero), to write it once per change.
fn board_key(board: &makepad_story_video_agent::Storyboard) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let text = format!("{:?}", board);
    for byte in text.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash | 1
}

/// A picture no larger than the models accept (1024 px a side): larger
/// ones are scaled down here, since a provider refuses the whole turn
/// otherwise. Pictures that fit, or that do not decode, pass unchanged.
fn fit_png(png: Vec<u8>) -> Vec<u8> {
    const EDGE: u32 = 1024;
    let side = |at: usize| png.get(at..at + 4).map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    if png.len() < 24 || !png.starts_with(b"\x89PNG") || (side(16) <= EDGE && side(20) <= EDGE) {
        return png;
    }
    let Ok(image) = makepad_widgets::makepad_draw::image_cache::decode_image_from_data(&png) else { return png };
    let mut rgba = Vec::with_capacity(image.data.len() * 4);
    for pixel in &image.data {
        rgba.extend_from_slice(&[(pixel >> 16) as u8, (pixel >> 8) as u8, *pixel as u8, (pixel >> 24) as u8]);
    }
    let picture = makepad_story_edit_tools::image::Picture { width: image.width as u32, height: image.height as u32, rgba };
    makepad_story_edit_tools::image::encode_png(&picture.fit_within(EDGE)).unwrap_or(png)
}

/// The inspect warnings the loop does not finish over: anything outside the
/// safe area or the frame, text that overflows, is clipped or overlaps text.
fn blocking(report: &makepad_strict_json::Value) -> usize {
    const CODES: [&str; 6] = ["[safe_zone]", "[text.overflow]", "[layer.outside_frame]", "[text.final_clipped]", "[text.final_outside_safe]", "[text.overlap]"];
    report.get("warnings").and_then(makepad_strict_json::Value::as_arr).map_or(0, |warnings| {
        warnings.iter().filter_map(makepad_strict_json::Value::as_str).filter(|w| CODES.iter().any(|code| w.contains(code))).count()
    })
}

/// A control API answer as the agent's outcome: the JSON as text, the
/// pictures, the documents it changed, and an inspect's counts.
fn outcome_of(name: &str, target: &str, result: makepad_studio_api::CallResult) -> Outcome {
    use makepad_story_video_agent::{BrainImage, DocChange, InspectFacts};
    match result {
        Ok(output) => {
            let count = |key: &str| output.json.get(key).and_then(makepad_strict_json::Value::as_arr).map_or(0, |items| items.len());
            Outcome {
                text: output.json.to_json(),
                is_error: false,
                images: output.images.into_iter().map(|image| BrainImage { label: image.label, png: Arc::from(fit_png(image.png)) }).collect(),
                inspect: (name == "inspect").then(|| InspectFacts { target: target.to_string(), errors: count("errors"), warnings: count("warnings"), blocking: blocking(&output.json) }),
                changes: output.changes.into_iter().map(|change| DocChange { key: change.key, label: change.label, before: change.before, after: change.after }).collect(),
            }
        }
        Err(error) => Outcome::error(error.to_json().to_json()),
    }
}

impl crate::StoryApp {
    /// The `make_video` tool (the chat card's Confirm).
    pub(crate) fn make_video_call(&mut self, args: &makepad_strict_json::Value) -> Result<(), String> {
        make_video(&mut self.video_cut, args)
    }

    /// The `make_video_note` tool: a line for the model that is making the
    /// video.
    pub(crate) fn make_video_note_call(&mut self, args: &makepad_strict_json::Value) -> Result<(), String> {
        let text = text_of(args, "text").trim().to_string();
        if say(&mut self.video_cut, text) {
            Ok(())
        } else {
            Err("no video is being made: make_video starts one".into())
        }
    }

    /// The control API's tools this Stage implements, as the table and the
    /// host's own additions describe them (the chat's services are made
    /// from these).
    pub(crate) fn studio_specs(&self) -> Vec<makepad_studio_api::ToolSpec> {
        self.studio_tools().into_iter().filter(|(_, available)| *available).map(|(spec, _)| spec.clone()).collect()
    }

    /// The control API's tools this Stage implements, as the model sees
    /// them (the agent adds its own storyboard, critique and finish).
    fn generate_tools(&self) -> Vec<ToolDef> {
        self.studio_tools()
            .into_iter()
            .filter(|(_, available)| *available)
            .map(|(spec, _)| ToolDef {
                name: spec.name.to_string(),
                description: spec.description.to_string(),
                parameters: spec.input.to_string(),
                mutates: spec.mutates,
                images: spec.images,
                inspects: spec.name == "inspect",
            })
            .collect()
    }

    /// The storyboard is the input and the cut its output: the plan the
    /// model records goes onto the board (`storyboard_write`), and a line
    /// from the person goes to the model with the board edits they made
    /// since (`storyboard_changes`), like hand edits of the cut.
    fn pump_board(&mut self, cx: &mut Cx) {
        use makepad_strict_json::{obj, s as text, Value};
        if let Some(id) = self.video_cut.generate.board_call {
            if let Some(result) = self.studio_result(id) {
                self.video_cut.generate.board_call = None;
                if let Err(error) = result {
                    log!("generate: the storyboard was not written: {}", error.to_json().to_json());
                }
            }
        }
        let plan = self.video_cut.generate.crew.as_ref().and_then(|crew| crew.lead().storyboard().cloned());
        if let Some(board) = plan {
            let fresh = self.video_cut.generate.board_written == 0 || self.video_cut.generate.board_written != board_key(&board);
            if fresh && self.video_cut.generate.board_call.is_none() {
                let scenes: Vec<makepad_story_storyboard::PlannedScene> = board
                    .scenes
                    .iter()
                    .map(|scene| makepad_story_storyboard::PlannedScene {
                        id: scene.id.clone(),
                        start: scene.start,
                        length: scene.length,
                        idea: scene.idea.clone(),
                        look: scene.look.clone(),
                        voice: scene.voice.clone(),
                        music: scene.music.clone(),
                        media: scene.media.clone(),
                        stack: scene.stack.clone(),
                    })
                    .collect();
                let source = makepad_story_storyboard::from_scenes(&board.title, &board.style, &board.kit, &scenes);
                let id = self.studio_call(cx, "storyboard_write", obj(vec![("source", text(source))]), "generate");
                self.video_cut.generate.board_call = Some(id);
                self.video_cut.generate.board_written = board_key(&board);
            }
        }
        match self.video_cut.generate.pending_say.take() {
            Some((line, None)) => {
                let id = self.studio_call(cx, "storyboard_changes", Value::Obj(Vec::new()), "generate");
                self.video_cut.generate.pending_say = Some((line, Some(id)));
            }
            Some((line, Some(id))) => match self.studio_result(id) {
                None => self.video_cut.generate.pending_say = Some((line, Some(id))),
                Some(result) => {
                    let changes: Vec<String> = result
                        .ok()
                        .and_then(|output| output.json.get("changes").and_then(Value::as_arr).map(|items| items.iter().filter_map(Value::as_str).map(str::to_string).collect()))
                        .unwrap_or_default();
                    let mut said = line;
                    if !changes.is_empty() {
                        said.push_str(&format!("\n\n[the person edited the storyboard]\n{}\nFollow the board: `storyboard_apply_changes`, do its tasks with only the nodes each names, then `storyboard_mark_built`.", changes.join("\n")));
                    }
                    let doc = open_doc(&self.video_cut);
                    if let Some(crew) = self.video_cut.generate.crew.as_mut() {
                        // The model reads the cut as it is now, hand edits included.
                        if let Some(doc) = doc {
                            crew.observe(vec![doc]);
                        }
                        crew.say(said);
                    }
                }
            },
            None => {}
        }
    }

    /// A publishing headless run's save; `true` once there is nothing
    /// (more) to do.
    fn headless_epilogue(&mut self, cx: &mut Cx) -> bool {
        let Some(epilogue) = self.video_cut.generate.headless.as_ref().filter(|h| h.publish && !h.written).map(|h| h.epilogue) else { return true };
        let empty = || makepad_strict_json::Value::Obj(Vec::new());
        let set = |app: &mut Self, next: Epilogue| {
            if let Some(h) = app.video_cut.generate.headless.as_mut() {
                h.epilogue = next;
            }
        };
        match epilogue {
            Epilogue::Idle => {
                let id = self.studio_call(cx, "save_cut", empty(), "generate");
                set(self, Epilogue::Saving(id));
            }
            Epilogue::Saving(id) => match self.studio_result(id) {
                Some(Ok(output)) => {
                    log!("generate: saved {}", output.json.to_json());
                    self.video_cut.generate.headless.as_mut().expect("headless").saved = Some(output.json);
                    // The MP4 comes from the staged output with the cut
                    // tools (`render`), not the page's render: in a headless
                    // Stage that one ends without a file (reported).
                    set(self, Epilogue::Done);
                }
                Some(Err(error)) => {
                    log!("generate: save failed: {}", error.to_json().to_json());
                    set(self, Epilogue::Done);
                }
                None => {}
            },
            Epilogue::Done => return true,
        }
        self.video_cut.generate.timer = cx.start_timeout(0.2);
        false
    }

    /// The Generate loop's turn in the page pump: its calls go to Stage's
    /// control API, the same tools Claude Code reaches
    /// over MCP, so every change is an undoable, autosaved edit.
    pub(crate) fn pump_generate(&mut self, cx: &mut Cx) {
        if !self.video_cut.generate.headless_checked {
            self.video_cut.generate.headless_checked = true;
            if let Err(error) = headless_start(&mut self.video_cut) {
                log!("generate: headless request: {error}");
                self.begin_close(cx);
            }
        }
        song_ready(&mut self.video_cut);
        if self.video_cut.generate.headless.as_ref().is_some_and(|headless| headless.song.is_some()) {
            self.video_cut.generate.timer = cx.start_timeout(0.5);
        }
        // The graphic desk's library calls (saved palettes).
        for (tag, tool, args) in super::motion_edit::variants::take_calls(&mut self.video_cut) {
            let id = self.studio_call(cx, tool, args, "desk");
            super::motion_edit::variants::track_call(&mut self.video_cut, tag, id);
        }
        for (_, id) in super::motion_edit::variants::running_calls(&self.video_cut) {
            if let Some(result) = self.studio_result(id) {
                let result = result.map(|out| out.json).map_err(|error| error.message);
                super::motion_edit::variants::call_done(&mut self.video_cut, cx, id, result);
            }
        }
        // The tool table is built only when a brief is about to start.
        let tools = if self.video_cut.generate.waiting.is_some() { self.generate_tools() } else { Vec::new() };
        self.pump_board(cx);
        self.pump_chat(cx);
        for TeamAction { agent, action } in pump(&mut self.video_cut, cx, &tools) {
            let target = action.args.get("target").and_then(makepad_strict_json::Value::as_str).filter(|target| !target.is_empty()).unwrap_or("cut").to_string();
            let origin = if agent == makepad_story_video_agent::DIRECTOR { "generate".to_string() } else { format!("generate:{agent}") };
            let id = self.studio_call(cx, &action.name, action.args.clone(), &origin);
            self.video_cut.generate.calls.push(Call { agent, call_id: action.call_id, id, name: action.name, target });
        }
        let pending = std::mem::take(&mut self.video_cut.generate.calls);
        for call in pending {
            match self.studio_result(call.id) {
                Some(result) => answer(&mut self.video_cut, &call.agent, &call.call_id, outcome_of(&call.name, &call.target, result)),
                None => self.video_cut.generate.calls.push(call),
            }
        }
        paint(&mut self.video_cut, cx, &self.ui);
        let ended = self.video_cut.generate.crew.as_ref().is_some_and(|crew| !crew.busy());
        let failed = self.video_cut.generate.headless.is_some() && self.video_cut.generate.crew.is_none() && self.video_cut.generate.waiting.is_none() && !self.video_cut.generate.why.is_empty();
        let failure = failed.then(|| self.video_cut.generate.why.clone());
        if ended && !failed && !self.headless_epilogue(cx) {
            return;
        }
        if (ended || failed) && headless_finish(&mut self.video_cut, failure) {
            self.begin_close(cx);
        }
    }
}

/// The backend a request's `model` names: `claude-cli:<id>`,
/// `claude-api:<id>`, `hub` or `hub:<model>`.
pub(super) fn backend_named(model: &str) -> Backend {
    match model.split_once(':') {
        Some(("claude-cli", id)) => Backend::ClaudeCode { model: id.to_string() },
        Some(("claude-api", id)) => Backend::ClaudeApi { model: id.to_string() },
        Some(("hub", id)) => Backend::Hub { model: Some(id.to_string()).filter(|id| !id.is_empty()) },
        _ if model == "hub" => Backend::Hub { model: None },
        _ => Backend::ClaudeCode { model: format!("{DEFAULT_CLAUDE_MODEL}[1m]") },
    }
}

/// Read `STAGE_GENERATE_REQUEST` once and queue its brief.
fn headless_start(page: &mut VideoCut) -> Result<(), String> {
    let Ok(path) = std::env::var("STAGE_GENERATE_REQUEST") else { return Ok(()) };
    let text = std::fs::read_to_string(&path).map_err(|error| format!("{path}: {error}"))?;
    let v = makepad_strict_json::parse_depth(text.as_bytes(), 8).map_err(|error| format!("{path}: {error}"))?;
    let text_of = |key: &str| v.get(key).and_then(makepad_strict_json::Value::as_str).unwrap_or("").to_string();
    let num_of = |key: &str| match v.get(key) {
        Some(makepad_strict_json::Value::Int(i)) => *i as f64,
        Some(makepad_strict_json::Value::F64(f)) => *f,
        _ => 0.0,
    };
    let out_dir = std::path::PathBuf::from(text_of("out_dir"));
    std::fs::create_dir_all(&out_dir).map_err(|error| format!("{}: {error}", out_dir.display()))?;
    let result_path = match text_of("result_path") {
        path if path.is_empty() => out_dir.join("result.json"),
        path => path.into(),
    };
    let (width, height) = (num_of("width").max(16.0) as u32, num_of("height").max(16.0) as u32);
    let voice = if v.get("voice").and_then(makepad_strict_json::Value::as_bool).unwrap_or(false) {
        page.voices.first().cloned().unwrap_or_else(|| makepad_story_cut::VoiceSpec::default().id)
    } else {
        String::new()
    };
    let brief = Brief {
        kind: Kind::Cut,
        prompt: text_of("prompt"),
        width,
        height,
        // Generated videos run at 60 fps or more; a request without a rate gets 60.
        fps: num_of("fps").max(60.0).round() as u32,
        length: num_of("length_s").max(1.0),
        style: String::new(),
        voice,
        music: "generate".into(),
        generate_media: true,
        menus: menus(),
        interactive: false,
    };
    let backend = backend_named(&text_of("model"));
    // Long videos are directed; a request may say so itself.
    page.generate.team = v.get("team").and_then(makepad_strict_json::Value::as_bool).unwrap_or(brief.length > 45.0);
    page.generate.backends.insert(0, backend);
    page.generate.backend = 0;
    let aspect = FORMATS.iter().find(|(w, h, _)| (*w, *h) == (width, height)).map_or("custom", |(_, _, aspect)| aspect);
    page.new_cut((width, height, aspect));
    // A song to set (a lyric film): import and time it first.
    let song_path = text_of("song");
    let (song, song_brief) = if song_path.is_empty() {
        page.generate.waiting = Some(brief);
        (None, None)
    } else {
        let lyrics = Some(text_of("lyrics")).filter(|path| !path.is_empty()).map(std::path::PathBuf::from);
        (Some(song::start(song_path.into(), lyrics)), Some(brief))
    };
    let publish = v.get("publish").and_then(makepad_strict_json::Value::as_bool).unwrap_or(false);
    page.generate.headless = Some(Headless { out_dir, result_path, started: std::time::Instant::now(), written: false, publish, epilogue: Epilogue::Idle, saved: None, song, song_brief, song_lyrics: None });
    log!("generate: headless request {path}");
    Ok(())
}

/// The request's song is in the library and timed: the brief goes on
/// with it. A song that cannot be imported fails the run.
fn song_ready(page: &mut VideoCut) {
    let Some(headless) = page.generate.headless.as_mut() else { return };
    let Some(receiver) = &headless.song else { return };
    let prepared = match receiver.try_recv() {
        Ok(prepared) => prepared,
        Err(std::sync::mpsc::TryRecvError::Empty) => return,
        Err(std::sync::mpsc::TryRecvError::Disconnected) => Err("the song import stopped".into()),
    };
    headless.song = None;
    let brief = headless.song_brief.take();
    match (prepared, brief) {
        (Ok(song), Some(mut brief)) => {
            log!("generate: the song is {} {} ({:.1} s), words {}", song.asset, song.revision, song.seconds, if song.lyrics_json.is_some() { "timed" } else { "untimed" });
            brief.prompt.push_str(&song.brief);
            brief.music = "the song below; do not generate or add other music.".into();
            headless.song_lyrics = song.lyrics_json.map(|json| (song.asset, json));
            page.generate.waiting = Some(brief);
        }
        (Err(error), _) => page.generate.why = format!("the song: {error}"),
        (Ok(_), None) => {}
    }
}

/// Every agent's transcript as plain text.
fn transcript_text(crew: &Crew) -> String {
    let mut out = String::new();
    for (id, agent) in crew.agents() {
        out.push_str(&format!("===== {id} =====\n\n"));
        for row in rows(agent, &(0..agent.entries().len()).collect()) {
            let text = match row {
                Row::User(text) => format!("> {text}"),
                Row::Say(text) | Row::Note(text) | Row::End(text) => text,
                Row::Fold { head, detail, .. } | Row::Call { head, detail, .. } => if detail.is_empty() { head } else { format!("{head}\n{detail}") },
                Row::Card { head, body } => format!("{head}\n{body}"),
            };
            out.push_str(&text);
            out.push_str("\n\n");
        }
    }
    out
}

/// A render job's files linked into `out` by name, as the evaluation's
/// contract has them (`makepad-video-eval`, the cut tools): a standalone
/// cut as `cut.splash` with `media.txt` (`<id>.<ext>`, `motion-<id>.splash`,
/// `edit:`/`effect:`/`lyrics:` entries); a tree's files under `files/`.
fn link_beside(job: &makepad_story_edit_render::job::Job, source: &str, out: &std::path::Path) -> Result<(), String> {
    let link = |to: &std::path::Path, name: &std::path::Path| -> Result<(), String> {
        if let Some(parent) = name.parent() {
            std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        if name.symlink_metadata().is_ok() {
            std::fs::remove_file(name).map_err(|error| format!("{}: {error}", name.display()))?;
        }
        #[cfg(unix)]
        return std::os::unix::fs::symlink(to, name).map_err(|error| format!("{}: {error}", name.display()));
        #[cfg(not(unix))]
        return std::fs::copy(to, name).map(|_| ()).map_err(|error| format!("{}: {error}", name.display()));
    };
    if let Some(tree) = &job.tree {
        for file in &tree.files {
            let name = out.join("files").join(&file.path);
            if file.location != name {
                link(&file.location, &name)?;
            }
        }
        return Ok(());
    }
    let mut table = String::from("# id=file, written by Stage's CUT page\n");
    for media in &job.media {
        let name = if media.ext == "splash" { format!("motion-{}.splash", media.key) } else { format!("{}.{}", media.key, media.ext) };
        link(&media.location, &out.join(&name))?;
        table.push_str(&format!("{}={name}\n", media.key));
    }
    for (what, list) in [("edit", &job.edits), ("effect", &job.effects)] {
        for (key, location) in list {
            let name = format!("{what}-{key}.splash");
            link(location, &out.join(&name))?;
            table.push_str(&format!("{what}:{key}={name}\n"));
        }
    }
    for (key, location) in &job.lyrics {
        let name = format!("lyrics-{key}.json");
        link(location, &out.join(&name))?;
        table.push_str(&format!("lyrics:{key}={name}\n"));
    }
    std::fs::write(out.join("media.txt"), table).map_err(|error| error.to_string())?;
    std::fs::write(out.join("cut.splash"), source).map_err(|error| error.to_string())
}

/// A finished headless run: its files, then `result.json`. `true` when
/// Stage should close.
fn headless_finish(page: &mut VideoCut, failure: Option<String>) -> bool {
    let Some(headless) = page.generate.headless.as_mut().filter(|headless| !headless.written) else { return false };
    headless.written = true;
    let out = headless.out_dir.clone();
    let result_path = headless.result_path.clone();
    let song_lyrics = headless.song_lyrics.clone();
    let secs = headless.started.elapsed().as_secs_f64();
    let mut motions = Vec::new();
    let mut cut_file = None;
    if let Some(session) = &page.session {
        let path = if session.tree.is_some() { out.join("files").join(&session.path) } else { out.join("cut.splash") };
        // The result as the evaluation reads it: the cut, and every file it
        // uses linked beside it with `media.txt` (the cut tools' media
        // table), so strips and renders of it work without Stage; and its
        // render job. Blocking library reads, once, at the end of a
        // headless run.
        let folder = page.folder.as_ref().map(|folder| folder.doc.dir.clone());
        let prepared = (|| {
            let mut store = super::store::LazyStoreMedia::new();
            let job = super::render::CutSource { source: session.source.clone(), tree: session.tree.clone().map(|tree| (tree, session.path.clone())), folder, saved: None }.job(&mut store, &out)?;
            job.write(&out.join("render.job"))?;
            link_beside(&job, &session.source, &out)
        })();
        if let Err(error) = &prepared {
            log!("generate: the cut's media could not be staged beside it: {error}");
            if session.tree.is_none() { let _ = std::fs::write(&path, &session.source); }
        }
        if path.is_file() {
            cut_file = Some(path);
        }
    }
    // The song's timed words beside the cut (`lyrics:<id>=<file>`), for
    // the cut tools and the evaluator's word sync.
    if let Some((asset, json)) = &song_lyrics {
        let name = format!("lyrics-{asset}.json");
        if std::fs::write(out.join(&name), json).is_ok() {
            let table = out.join("media.txt");
            let mut text = std::fs::read_to_string(&table).unwrap_or_else(|_| "# id=file, written by Stage's CUT page\n".into());
            text.push_str(&format!("lyrics:{asset}={name}\n"));
            let _ = std::fs::write(&table, text);
        }
    }
    let (success, error, iterations, model, transcript) = match &page.generate.crew {
        Some(crew) => {
            let agent = crew.lead();
            // The latest source of every motion document the model wrote.
            let mut latest: Vec<(String, String)> = Vec::new();
            for revision in crew.revisions().into_iter().filter(|revision| revision.key.starts_with("motion:")) {
                match latest.iter_mut().find(|(key, _)| *key == revision.key) {
                    Some(entry) => entry.1 = revision.after.clone(),
                    None => latest.push((revision.key.clone(), revision.after.clone())),
                }
            }
            for (index, (key, source)) in latest.iter().enumerate() {
                let path = out.join(format!("motion{}.splash", index + 1));
                // A document already on the timeline is updated by re-pointing
                // the cut (its change reads as the cut's), so the log keeps
                // its first version: the revision the cut plays, staged
                // beside it, is the latest.
                let played = std::fs::read_to_string(out.join(format!("motion-{}.splash", key.trim_start_matches("motion:"))));
                let source = played.as_deref().unwrap_or(source);
                if std::fs::write(&path, source).is_ok() {
                    motions.push((path, key.trim_start_matches("motion:").to_string()));
                }
            }
            let done = *agent.state() == State::Done;
            let error = failure.or(match agent.state() {
                State::Stopped(why) | State::Failed(why) => Some(why.clone()),
                _ => None,
            });
            (done, error, agent.iterations(), agent.label(), transcript_text(crew))
        }
        None => (false, failure.or(Some("the generation did not start".into())), 0, String::new(), String::new()),
    };
    let transcript_path = out.join("transcript.txt");
    let _ = std::fs::write(&transcript_path, &transcript);
    use makepad_strict_json::{obj, s as text, Value};
    let path_text = |path: &std::path::Path| text(path.display().to_string());
    let result = obj(vec![
        ("success", Value::Bool(success)),
        ("error", error.map_or(Value::Null, text)),
        ("iterations", Value::Int(iterations as i64)),
        ("model_used", text(model)),
        ("seconds", Value::F64((secs * 10.0).round() / 10.0)),
        ("edit", cut_file.as_deref().map_or(Value::Null, path_text)),
        ("motions", Value::Arr(motions.iter().map(|(path, asset)| obj(vec![("path", path_text(path)), ("asset", text(asset.clone()))])).collect())),
        ("transcript", path_text(&transcript_path)),
        ("library_edit", page.generate.headless.as_ref().and_then(|h| h.saved.clone()).unwrap_or(Value::Null)),
    ]);
    if let Err(error) = std::fs::write(&result_path, result.to_json()) {
        log!("generate: could not write {}: {error}", result_path.display());
    }
    log!("generate: headless run finished ({secs:.0} s), result in {}", result_path.display());
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_strict_json::{obj, s as text, Value};

    fn args(fields: Vec<(&str, Value)>) -> Value {
        obj(fields)
    }

    #[test]
    fn make_video_args_become_a_brief() {
        let voices = vec![("Ada (en)".to_string(), "ada".to_string()), ("Bo (nl)".to_string(), "bo".to_string())];
        let given = args(vec![
            ("prompt", text(" a launch video ")),
            ("aspect", text("16:9")),
            ("seconds", Value::Int(45)),
            ("voice", text("bo")),
            ("music", text("library")),
            ("media", text("library")),
            ("authorship", text("director")),
            ("style", text("bold type")),
        ]);
        let (brief, team) = brief_from_args(&given, None, &voices).unwrap();
        assert!(team);
        assert_eq!((brief.prompt.as_str(), brief.width, brief.height, brief.fps, brief.length), ("a launch video", 1920, 1080, 60, 45.0));
        assert_eq!((brief.voice.as_str(), brief.style.as_str(), brief.generate_media), ("bo", "bold type", false));
        assert!(brief.music.contains("search_library"), "{}", brief.music);

        // Only the prompt: the defaults of the old card.
        let (brief, team) = brief_from_args(&args(vec![("prompt", text("x"))]), None, &voices).unwrap();
        assert!(!team);
        assert_eq!((brief.width, brief.height, brief.length, brief.voice.as_str(), brief.music.as_str(), brief.generate_media), (1080, 1920, 30.0, "", "generate", true));

        // The open cut keeps its own format and frame rate; "none" is no voice; "yes" the first voice.
        let open = Some((1080, 1080, 90.0));
        let (brief, _) = brief_from_args(&args(vec![("prompt", text("x")), ("aspect", text("9:16")), ("voice", text("none")), ("music", text("none"))]), open, &voices).unwrap();
        assert_eq!((brief.width, brief.height, brief.fps, brief.voice.as_str(), brief.music.as_str()), (1080, 1080, 90, "", "none"));
        let (brief, _) = brief_from_args(&args(vec![("prompt", text("x")), ("voice", text("yes"))]), None, &voices).unwrap();
        assert_eq!(brief.voice, "ada");

        assert!(brief_from_args(&args(vec![("prompt", text("  "))]), None, &voices).is_err());
        assert!(brief_from_args(&args(vec![("prompt", text("x")), ("aspect", text("2:1"))]), None, &voices).is_err());
    }
}
