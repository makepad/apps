//! Story's chat: what the window's one chat (the shell's
//! `makepad_app_shell::chat`, the aichat panel docked beside the pages)
//! is told by the pages, with the control API as its tools.
//!
//! - **Services.** The catalogue is registered in process as one AI service
//!   per target (`studio_api::services`: the CUT page, the graphic desk,
//!   DECK), each a family subset, of Story's product only. A call runs through `studio_call` and its answer goes
//!   back when `studio_result` has it.
//! - **Target.** The page (and the open document, the selection, the
//!   playhead) is the panel's target chip.
//! - **Suggestions.** The page's chips open a settings card with no model
//!   turn.
//! - **Pictures.** The picture at the playhead, the shown slide or the
//!   desk's picture.
//! - **Notes.** The pages say what happened (a render finished, an export
//!   started) through their outbox.

use super::{chat_context, VideoCut};
use crate::{StoryApp, StoryPage};
use makepad_ai_services::wire::{ServiceManifest, ToolResult};
use makepad_ai_services::{Attachment, Suggestion, Target as ChatTarget};
use makepad_app_shell::chat::{self as shell_chat, ChatDock, ChatHost, ChatPicture};
use makepad_studio_api::services;
use makepad_strict_json::{obj, s, Value};
use makepad_widgets::*;

// ------------------------------------------------------------------ what the pages ask

/// A line in the chat's transcript (a render finished, an export started).
pub(crate) fn note(page: &mut VideoCut, line: String) {
    page.chat.note(line);
}

/// Open the settings card of a CUT tool, prefilled (an object), and show
/// the chat: what a button does that makes something.
pub(super) fn propose(page: &mut VideoCut, tool: &'static str, prefill: &Value) {
    page.chat.propose(services::CUT, tool, prefill);
}

/// Send a line to the chat as if the person had typed it.
pub(crate) fn say(page: &mut VideoCut, words: String) {
    page.chat.say(words);
}

// ------------------------------------------------------------------ what the chat shows

/// One suggestion: a tool of the CUT page, opened as a card.
fn suggestion(label: &str, tool: &str, prefill: Value) -> Suggestion {
    Suggestion { label: label.to_string(), app: services::CUT.to_string(), tool: tool.to_string(), prefill: prefill.to_json() }
}

impl VideoCut {
    /// The chat's suggestions for the page as it is: what the person could
    /// ask for now, each a tool of the CUT page and its prefill.
    pub(super) fn suggestions(&self) -> Vec<Suggestion> {
        use makepad_strict_json::{obj, s};
        let empty = || Value::Obj(Vec::new());
        let mut chips = vec![suggestion("Make a video", "make_video", empty()), suggestion("Song video", "new_video", empty()), suggestion("New cut…", "new_cut", empty())];
        let Some(session) = &self.session else { return chips };
        chips.push(suggestion("Render…", "render_cut", self.render_settings()));
        // What Stage wrote: show it, open it (both run at once).
        let wrote = self.written();
        if let Some(file) = self.render_ui.exported.as_ref().map(|file| file.display().to_string()).filter(|file| wrote.check(file).is_ok()) {
            chips.push(suggestion("Show in Finder", "reveal_file", obj(vec![("path", s(file.clone()))])));
            chips.push(suggestion("Open", "open_file", obj(vec![("path", s(file))])));
        }
        if let Some(url) = wrote.urls.first() {
            chips.push(suggestion("Open in browser", "open_file", obj(vec![("path", s(url.clone()))])));
        }
        if let Some(dir) = wrote.dirs.first() {
            chips.push(suggestion("Show the export in Finder", "reveal_file", obj(vec![("path", s(dir.display().to_string()))])));
        }
        if session.head.is_some() && !session.dirty() {
            chips.push(suggestion("Reframe to another format…", "new_cut", obj(vec![("reframe", Value::Bool(true))])));
        }
        chips.push(suggestion("Web export…", "export_web", empty()));
        if super::cut_panel::lacks_panel(self) {
            chips.push(suggestion("Make the film's editor", "cut_panel_make", empty()));
        }
        chips.push(suggestion("Project settings…", "set_header", obj(vec![
            ("label", s(session.doc.label.clone())),
            ("duration", Value::F64(session.doc.duration)),
            ("bpm", Value::F64(session.doc.bpm)),
        ])));
        if self.web_export_running() {
            chips.push(suggestion("Stop the web export", "export_web", obj(vec![("stop", Value::Bool(true))])));
        }
        let marked = matches!(self.marks, (Some(a), Some(b)) if b > a);
        if self.selected.is_some() || marked {
            if let Some((tool, prefill)) = self.generate_card(false) {
                chips.push(suggestion("Generate over selection", tool, prefill));
            }
        }
        if let Some(node) = self.generating_node() {
            chips.push(suggestion("Stop generating", "generate_cancel", obj(vec![("node", s(node))])));
        }
        if self.new_video.busy() {
            let label = if self.generating_node().is_some() { "Stop the song video" } else { "Stop generating" };
            chips.push(suggestion(label, "new_video_cancel", empty()));
        }
        let voice = self.selected.as_deref().map(|id| super::split_selection(id).0.to_string()).filter(|node| self.is_voice(node))
            .or_else(|| session.doc.nodes.iter().find(|node| matches!(node.kind, makepad_story_cut::CutNodeKind::VoiceOver { .. })).map(|node| node.id.clone()));
        if let Some(node) = voice {
            chips.push(suggestion("Voice the lines", "speak", obj(vec![("node", s(node.clone()))])));
            chips.push(suggestion("Subtitles", "add_subtitles", obj(vec![("voice", s(node))])));
        }
        if self.motion.mode == super::motion_edit::ViewMode::Storyboard {
            for (label, tool, prefill) in super::storyboard::chips(self) {
                chips.push(suggestion(label, tool, prefill));
            }
            chips.push(suggestion("Update the cut from the board", "storyboard_apply_changes", empty()));
        }
        // The empty chat shows the first few: what stops something running,
        // what works on the selection, then making, rendering and opening.
        chips.sort_by_key(|chip| match chip.tool.as_str() {
            _ if chip.label.starts_with("Stop") || chip.label.starts_with("Freeze") => 0,
            "generate_run" | "generate_add" | "speak" | "add_subtitles" | "storyboard_apply_changes" => 1,
            "make_video" => 2,
            "render_cut" | "reveal_file" | "open_file" => 3,
            "new_video" => 4,
            _ => 5,
        });
        chips
    }

    /// The render settings last used, as the Render… card's prefill (the
    /// Render button, cmd-E, renders with these).
    pub(super) fn render_settings(&self) -> Value {
        let ui = &self.render_ui;
        let mut args = Vec::new();
        if ui.export_draft {
            args.push(("quality", s("draft")));
        }
        if let Some(fps) = ui.export_fps {
            args.push(("fps", Value::F64(fps)));
        }
        if ui.export_blend {
            args.push(("blend", Value::Bool(true)));
        }
        if ui.export_high {
            args.push(("bitrate", s("high")));
        }
        if let Some(name) = [None, Some("social"), Some("podcast"), Some("broadcast")][self.loudness.min(3)] {
            args.push(("loudness", s(name)));
        }
        if self.burn_off {
            args.push(("burn_captions", Value::Bool(false)));
        }
        args.push(("path", s(if ui.export_path.trim().is_empty() { self.default_export_path() } else { ui.export_path.clone() })));
        if ui.export_publish {
            args.push(("publish", Value::Bool(true)));
        }
        obj(args)
    }

    /// What the person sees on the CUT page, for the model: the cut, the
    /// moment, the clips under the playhead, the song's section and the
    /// word sung there.
    fn cut_context(&self) -> String {
        let run = if self.generate.busy() { "A video is being made right now (make_video): the person's words about it go to the crew as make_video_note, not as edits.\n" } else { "" };
        let Some(session) = &self.session else { return format!("The CUT page has no cut open.\n{run}") };
        let t = self.playhead;
        let doc = &session.doc;
        let mut out = format!("The CUT page: the cut \"{}\", {:.1} s at {:.1} bpm, revision {}{}.\n", doc.label, doc.duration, doc.bpm, session.revision, if session.dirty() { ", with changes not saved yet" } else { "" });
        out.push_str(&format!("The moment: {t:.2} s{}.\n", if self.playing.is_some() { " (playing)" } else { "" }));
        out.push_str(run);
        if let Some(selected) = &self.selected {
            out.push_str(&format!("Selected: `{}`.\n", super::split_selection(selected).0));
        }
        if let (Some(a), Some(b)) = self.marks {
            if b > a {
                out.push_str(&format!("The marked range: {a:.2}–{b:.2} s.\n"));
            }
        }
        let under: Vec<String> = self.projection.blocks.iter()
            .filter(|b| t >= b.start && t < b.start + b.length && b.line.is_none())
            .filter(|b| matches!(b.kind, super::project::BlockKind::Clip | super::project::BlockKind::Still | super::project::BlockKind::Title | super::project::BlockKind::Effect))
            .map(|b| format!("`{}` ({} {:?}, {:.2}–{:.2} s, {:.2} s in)", b.node, b.label, b.kind, b.start, b.start + b.length, t - b.start))
            .collect();
        out.push_str(&if under.is_empty() { "Under the playhead: no clip.\n".to_string() } else { format!("Under the playhead: {}.\n", under.join("; ")) });
        if let Some(timing) = &self.cut_panel.timing {
            if let Some((name, a, b)) = timing.sections.iter().find(|(_, a, b)| t >= *a && t < *b) {
                out.push_str(&format!("The song's section: {name} ({a:.2}–{b:.2} s).\n"));
            }
            if let Some(sung) = chat_context::sung_at(&timing.words, t) {
                out.push_str(&sung);
                out.push('\n');
            }
        }
        out
    }

    /// What the picture is of, so it is taken once per look: the cut's
    /// revision and the playhead.
    fn cut_look(&self) -> Option<String> {
        let session = self.session.as_ref()?;
        Some(format!("cut:{}:{:.2}:{}", session.revision, self.playhead, self.selected.as_deref().unwrap_or("")))
    }
}

/// The DECK page's starting points: lines for the model about the slide in
/// view and the deck, or a new deck when none is open.
pub(crate) fn deck_suggestions(open: bool) -> Vec<Suggestion> {
    if !open {
        return vec![
            page_chip(services::DECK, "New deck…", "new_deck", Value::Obj(Vec::new())),
            Suggestion::prompt("Make a short deck", "Make a new deck of about five slides. Ask me what it is about first."),
        ];
    }
    vec![
        Suggestion::prompt("Tighten this slide's text", "Tighten the wording on the slide in view. Keep its meaning and its layout."),
        Suggestion::prompt("Add a slide after this one", "Add a slide after the one in view, in the style of the slides around it. Ask me what it should say first."),
        Suggestion::prompt("Check the deck's layout", "Check the open deck for layout problems (overlaps, text running off a slide) and list what you find. Change nothing yet."),
        Suggestion::prompt("Summarise the deck", "Summarise the open deck in a few lines: what each part says, and what is missing."),
    ]
}

fn page_chip(app: &str, label: &str, tool: &str, prefill: Value) -> Suggestion {
    Suggestion { label: label.to_string(), app: app.to_string(), tool: tool.to_string(), prefill: prefill.to_json() }
}

impl StoryApp {
    /// The target the chat is scoped to now, and what it suggests. Its
    /// label is the context line over the composer ("Editing · DECK —
    /// makepad · slide 2 of 11"): the page and what is open on it.
    fn page_target(&mut self, cx: &mut Cx) -> (ChatTarget, Vec<Suggestion>) {
        let target = |id: &str, label: &str, context: String| ChatTarget { label: label.to_string(), app: id.to_string(), context, attachments: Vec::new() };
        let page = &self.video_cut;
        match self.page {
            StoryPage::Cut if super::motion_edit::scene_open(page) => {
                let (mut context, label) = match super::motion_edit::variants::open_graphic(page) {
                    Some((node, title, duration)) => (
                        format!("The graphic desk is open on the graphic `{title}`{} ({duration:.1} s).\n", node.map(|node| format!(", the Graphic clip `{node}` of the cut")).unwrap_or_default()),
                        format!("Graphic desk — {title}"),
                    ),
                    None => ("The graphic desk is open.\n".to_string(), "Graphic desk".to_string()),
                };
                context.push_str(&super::motion_edit::chat_context(page));
                // The node picked in the desk's structure is a chip on the composer
                // (its details are in the context already).
                let selection = super::motion_edit::chat_selection(page);
                let attachments = selection.into_iter().map(|s| Attachment { id: s.clone(), label: s, kind: "selected".into(), detail: String::new() }).collect();
                (ChatTarget { attachments, ..target(services::MOTION, &label, context) }, super::motion_edit::chat_suggestions(page))
            }
            StoryPage::Cut => {
                let label = match &page.session {
                    Some(session) if !session.doc.label.trim().is_empty() => format!("CUT — {}", session.doc.label.trim()),
                    Some(_) => "CUT — untitled cut".to_string(),
                    None => "CUT — no cut open".to_string(),
                };
                (target(services::CUT, &label, page.cut_context()), page.suggestions())
            }
            StoryPage::Deck => {
                let shown = super::super::deck::shown_slide(cx, &self.ui);
                // the deck page's own context (deckux), under the panel's label and suggestions
                let context = self.deck.chat_context();
                let label = match self.deck.session() {
                    Some(session) => {
                        let count = session.doc.slides.len();
                        match shown.and_then(|index| session.doc.slides.get(index).map(|_| index)) {
                            Some(index) => format!("DECK — {} · slide {} of {}", session.doc.title, index + 1, count),
                            None => format!("DECK — {} · {} slides", session.doc.title, count),
                        }
                    }
                    None => "DECK — no deck open".to_string(),
                };
                let open = self.deck.session().is_some();
                (target(services::DECK, &label, context), deck_suggestions(open))
            }
        }
    }

    /// F10, the bridge's `/ai` routes and the toolbar button reach the one
    /// panel: the window's own overlay never gets the request.
    pub(crate) fn take_chat_requests(&mut self, cx: &mut Cx) {
        shell_chat::take_requests(self, cx);
    }

    pub(crate) fn toggle_chat(&mut self, cx: &mut Cx) {
        shell_chat::toggle(self, cx);
    }

    /// Escape in the chat hides it; clearing the selection under the
    /// context line deselects it on the page.
    pub(crate) fn chat_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        shell_chat::handle_actions(self, cx, actions);
    }

    /// The chat's turn in the app pump: the pages' notes, DECK's
    /// "Fancy it up", then the shell's chat.
    pub(crate) fn pump_chat(&mut self, cx: &mut Cx) {
        let outs = self.video_cut.chat.take();
        self.chat.post(outs);
        self.pump_fancy(cx);
        shell_chat::pump(self, cx);
    }

    /// DECK's "Fancy it up": its request goes to the selected model as the
    /// person's line (the chat opens on DECK, so the deck's tools and the
    /// job's context go with it), and the page hears how the model is doing.
    fn pump_fancy(&mut self, cx: &mut Cx) {
        use makepad_ai_services::state::Status;
        use makepad_app_aichat::AiChatPanel;
        if let Some(request) = self.deck.take_fancy_request() {
            shell_chat::set_open(self, cx, true);
            let Some(panel) = shell_chat::retarget(self, cx) else { return };
            let tools_only = match panel.borrow_mut::<AiChatPanel>() {
                Some(mut chat) => {
                    // "Tools only" takes a line and never answers: say so instead of waiting.
                    let tools_only = chat.state().is_some_and(|state| state.provider.slug() == "none");
                    if !tools_only { chat.say(cx, request.line); }
                    tools_only
                }
                None => false,
            };
            if tools_only {
                self.deck.fancy_progress(cx, None, Some("the chat's model is \"Tools only\", which writes nothing: pick a model on the chat's model chip, then Fancy it up again"), "Tools only");
                return;
            }
        }
        if !self.deck.fancy_running() { return; }
        let window = self.ui.clone();
        let Some(panel) = self.chat.panel(cx, &window) else { return };
        let Some(chat) = panel.borrow::<AiChatPanel>() else { return };
        let Some(state) = chat.state() else { return };
        let busy = match &state.status {
            Status::Loading { phase, .. } => Some(if phase.is_empty() { "loading the model…".to_string() } else { format!("loading the model ({phase})…") }),
            Status::Thinking => Some("thinking…".into()),
            Status::Streaming => Some("writing…".into()),
            Status::WaitingForTool => Some("running a tool…".into()),
            Status::Idle | Status::Error(_) => None,
        };
        let error = match &state.status { Status::Error(error) => Some(error.clone()), _ => None };
        let provider = if state.provider_label.is_empty() { state.provider.slug() } else { state.provider_label.clone() };
        drop(chat);
        self.deck.fancy_progress(cx, busy.as_deref(), error.as_deref(), &provider);
    }

    /// The scene the graphic desk shows, when it is open on CUT: what the
    /// picture is of and its texture.
    fn desk_look(&mut self, cx: &mut Cx) -> Option<(String, Option<Texture>)> {
        if self.page == StoryPage::Cut { super::motion_edit::chat_look(&self.video_cut, cx, &self.ui) } else { None }
    }
}

impl ChatHost for StoryApp {
    fn chat_dock(&mut self) -> &mut ChatDock {
        &mut self.chat
    }

    fn chat_window(&self) -> WidgetRef {
        self.ui.clone()
    }

    /// The window's products' targets only.
    fn chat_services(&mut self) -> Vec<ServiceManifest> {
        let specs = self.studio_specs();
        services::targets_of(&[services::Product::Story])
            .filter_map(|target| match target.manifest(specs.iter()) {
                Ok(manifest) => Some(manifest),
                Err(error) => {
                    error!("chat: the {} service is refused: {error}", target.id);
                    None
                }
            })
            .collect()
    }

    fn chat_call(&mut self, cx: &mut Cx, tool: &str, args: Value) -> u64 {
        self.studio_call(cx, tool, args, "chat")
    }

    fn chat_answer(&mut self, call_id: &str, id: u64) -> Option<ToolResult> {
        let result = self.studio_result(id)?;
        Some(services::answer(call_id, &result).0)
    }

    fn chat_progress(&self, id: u64) -> Option<(String, u16)> {
        self.studio_progress(id)
    }

    fn chat_target(&mut self, cx: &mut Cx) -> (ChatTarget, Vec<Suggestion>) {
        self.page_target(cx)
    }

    fn chat_look(&mut self, cx: &mut Cx) -> Option<String> {
        if let Some((key, _)) = self.desk_look(cx) {
            return Some(key);
        }
        match self.page {
            StoryPage::Cut => self.video_cut.cut_look(),
            StoryPage::Deck => self.deck.session().map(|session| {
                let slide = super::super::deck::shown_slide(cx, &self.ui).unwrap_or(0);
                format!("deck:{}:{slide}", session.revision)
            }),
        }
    }

    fn chat_picture(&mut self, cx: &mut Cx) -> ChatPicture {
        if let Some((_, texture)) = self.desk_look(cx) {
            return ChatPicture { texture, label: "the graphic's frame at the desk's playhead".to_string() };
        }
        match self.page {
            StoryPage::Deck => {
                let slide = super::super::deck::shown_slide(cx, &self.ui).and_then(|index| self.deck.session().and_then(|session| session.doc.slides.get(index).map(|slide| (index, slide.name.clone()))));
                let label = match slide {
                    Some((index, name)) => format!("the deck's picture now: slide {} `{name}`", index + 1),
                    None => "the deck's picture now".to_string(),
                };
                ChatPicture { texture: super::super::deck::picture(cx, &self.ui), label }
            }
            StoryPage::Cut => {
                let texture = self.ui.widget(cx, ids!(cut_preview)).borrow::<super::preview::CutPreview>().and_then(|preview| preview.shown_frame()).map(|(_, texture)| texture);
                ChatPicture { texture, label: format!("the film's frame at the playhead, {:.2} s", self.video_cut.playhead) }
            }
        }
    }

    fn chat_shown(&mut self, cx: &mut Cx) {
        self.paint_navigation(cx);
        self.video_cut.keep_layout(cx, &self.ui);
    }

    /// The selection the chat shows, taken off the page: the graphic desk's
    /// picked node. (DECK's click-to-select clears here too once it lands.)
    fn chat_attachment_removed(&mut self, _cx: &mut Cx, _id: &str) {
        if self.page == StoryPage::Cut && super::motion_edit::scene_open(&self.video_cut) {
            super::motion_edit::clear_chat_selection(&mut self.video_cut);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_ai_services::card::Card;
    use makepad_studio_api::TOOLS;

    /// What a chip opens is a tool of the CUT page's service and a card its
    /// schema makes, filled from the chip's prefill (no model turn).
    #[test]
    fn every_suggestion_opens_a_tool_of_its_service() {
        let cut = services::target(services::CUT).unwrap();
        let manifest = cut.manifest(TOOLS.iter()).unwrap();
        let page = VideoCut::default();
        let chips = page.suggestions();
        assert!(chips.len() >= 3, "{chips:?}");
        for chip in chips {
            assert_eq!(chip.app, services::CUT);
            let tool = manifest.tool(&chip.tool).unwrap_or_else(|| panic!("{}: not a tool of the CUT page's service", chip.tool));
            let prefill = makepad_strict_json::parse(chip.prefill.as_bytes()).unwrap_or_else(|e| panic!("{}: {e}", chip.tool));
            Card::for_schema(&chip.tool, &tool.description, &tool.parameters, tool.layout.as_ref(), &prefill).unwrap_or_else(|e| panic!("{}: {e}", chip.tool));
        }
    }

    /// Every tool a chip of any state names (the chips that need a cut, a
    /// selection, a stream or the graphic desk too) is a tool of its page's
    /// service, and opens a card with the prefill empty.
    #[test]
    fn every_tool_a_chip_can_name_is_in_its_service() {
        let cut = [
            "make_video", "new_video", "new_cut", "web_grab_start", "render_cut", "export_web", "set_header", "web_stream_snapshot", "web_grab_cancel", "generate_run", "generate_add",
            "generate_cancel", "new_video_cancel", "speak", "add_subtitles", "storyboard_apply_changes", "cut_panel_make",
        ];
        for (service, tools) in [(services::CUT, &cut[..]), (services::MOTION, &["editor_make"][..])] {
            let manifest = services::target(service).unwrap().manifest(TOOLS.iter()).unwrap();
            for tool in tools {
                let def = manifest.tool(tool).unwrap_or_else(|| panic!("{tool}: not a tool of {service}"));
                Card::for_schema(tool, &def.description, &def.parameters, def.layout.as_ref(), &Value::Obj(Vec::new())).unwrap_or_else(|e| panic!("{tool}: {e}"));
            }
        }
    }

    /// DECK's starting points are lines for the model, and with no deck
    /// open a New deck… card of the DECK service.
    #[test]
    fn deck_suggestions_are_prompts_or_deck_tools() {
        let deck = services::target(services::DECK).unwrap();
        let manifest = deck.manifest(TOOLS.iter()).unwrap();
        for open in [false, true] {
            let chips = deck_suggestions(open);
            assert!(!chips.is_empty());
            for chip in chips {
                if chip.is_prompt() {
                    assert!(!chip.prefill.trim().is_empty(), "{}", chip.label);
                    continue;
                }
                assert_eq!(chip.app, services::DECK);
                let tool = manifest.tool(&chip.tool).unwrap_or_else(|| panic!("{}: not a tool of the DECK page's service", chip.tool));
                Card::for_schema(&chip.tool, &tool.description, &tool.parameters, tool.layout.as_ref(), &Value::Obj(Vec::new())).unwrap_or_else(|e| panic!("{}: {e}", chip.tool));
            }
        }
    }

    /// Show and Open run at once (Act), and the CUT page offers them only for
    /// a file Stage wrote. DECK is not among the services that carry them: its
    /// tool list is the small, explicit one the local models edit decks with.
    #[test]
    fn show_in_finder_and_open_are_act_chips_for_what_stage_wrote() {
        use makepad_ai_services::wire::Risk;
        for service in [services::CUT, services::MOTION, services::PAGE] {
            let manifest = services::target(service).unwrap().manifest(TOOLS.iter()).unwrap_or_else(|e| panic!("{service}: {e}"));
            for tool in ["reveal_file", "open_file"] {
                assert_eq!(manifest.tool(tool).unwrap_or_else(|| panic!("{service}: {tool}")).risk, Risk::Act);
            }
        }
        let mut page = VideoCut::default();
        let open_chips = |page: &VideoCut| page.suggestions().into_iter().filter(|chip| chip.tool == "open_file" || chip.tool == "reveal_file").count();
        assert_eq!(open_chips(&page), 0, "no render, no Open chip");
        // With a cut open the chip carries the written file and is valid for its service.
        page.render_ui.exported = Some(std::path::PathBuf::from("/m/Film.mp4"));
        assert!(page.written().check("/m/Film.mp4").is_ok());
    }

    /// With no cut open there is nothing to look at: no picture is taken and
    /// the model is told so.
    #[test]
    fn a_page_without_a_cut_has_no_picture_and_says_so() {
        let page = VideoCut::default();
        assert_eq!(page.cut_look(), None, "no cut, no picture");
        assert!(page.cut_context().contains("no cut open"));
    }
}
