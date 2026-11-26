//! Sketch slides on the DECK page (the format: `makepad_story_deck::sketch`).
//!
//! - **Making one.** [`DeckPage::add_sketch_slide`] adds an empty sketch
//!   slide after a slide (the slide list's "New slide" calls it), shows it
//!   and puts the typing on the slide in the main view: the first line is
//!   the title, Enter starts the next line (a bullet). Esc, Cmd/Ctrl+Enter
//!   or a click elsewhere writes the words (one undo step). A click on a
//!   sketch slide's picture types on it again; "Edit sketch" does the same
//!   on a slide that has its graphic.
//! - **Without AI.** Nothing more is needed to present it: the deck shows
//!   the default layout in its look.
//! - **Fancy it up.** [`DeckPage::fancy_up`] (the slide's right-click menu,
//!   the button on the slide in the main view) asks the chat's selected
//!   model, through the DECK tools the chat already has (doc_create /
//!   doc_write, preview, set_slide), to write the slide's graphic in the
//!   deck's look with the person's words, and attach it. The request is one
//!   line in the chat; the sketch, the look and the slides around it go
//!   with it as the chat's context. The top bar's status area says how it
//!   is going. Words changed after that offer "Regenerate"; it never runs
//!   by itself.

use super::view::DeckView;
use super::DeckPage;
use makepad_story_deck::ops::{DeckOp, NewCut, SlideSet};
use makepad_story_deck::{DeckDoc, Slide};
use makepad_widgets::*;

/// What the main view asks of the page about a sketch slide (by its name).
#[derive(Clone, Debug, Default)]
pub enum SketchAction {
    #[default]
    None,
    /// The words typed on the slide, when typing ends.
    Commit { slide: String, text: String },
    /// The slide's "Fancy it up" / "Regenerate" button.
    FancyUp { slide: String },
}

/// The typing on a sketch slide, as a state machine: what is typed on, and
/// the words just committed, which the slide shows (and typing reopens
/// with) until the deck's own copy catches up with the async write.
#[derive(Default)]
pub(super) struct Typing {
    /// The slide typed on (by name), and its words when typing began.
    edit: Option<(String, String)>,
    /// A committed slide, its words, and the deck's words for it then.
    committed: Option<(String, String, Option<String>)>,
}

impl Typing {
    pub fn begin(&mut self, slide: &str, text: &str) { self.edit = Some((slide.to_string(), text.to_string())); }
    pub fn is_typing(&self) -> bool { self.edit.is_some() }
    pub fn slide(&self) -> Option<&str> { self.edit.as_ref().map(|(slide, _)| slide.as_str()) }
    pub fn cancel(&mut self) { self.edit = None; }

    /// Typing ends with the words `typed`; `deck` is the deck's words for
    /// the slide now. Some((slide, text)) when they changed and are to be
    /// written; they are what the slide shows from here on.
    pub fn commit(&mut self, typed: String, deck: Option<&str>) -> Option<(String, String)> {
        let (slide, before) = self.edit.take()?;
        if typed == before { return None; }
        self.committed = Some((slide.clone(), typed.clone(), deck.map(str::to_string)));
        Some((slide, typed))
    }

    /// The slide's words: the committed ones while the deck's still are
    /// the ones they replaced, else the deck's.
    pub fn words(&self, slide: &str, deck: &str) -> String {
        match &self.committed {
            Some((name, text, base)) if name == slide && base.as_deref() == Some(deck) => text.clone(),
            _ => deck.to_string(),
        }
    }
}

/// The words of a neighbour or of the look given to the model, at most.
const MAX_SOURCE: usize = 16_000;
/// How long the last job's outcome stays in the status line.
const OUTCOME_SECONDS: f64 = 20.0;

/// A request for the chat: the line it sends as the person's, and the
/// context that goes with it while the job runs.
pub(crate) struct FancyRequest {
    pub line: String,
}

/// "Fancy it up" under way.
struct FancyJob {
    slide: String,
    /// The graphic the slide had when it was asked (a regenerate writes the
    /// same file), and that file's text then.
    before: Option<String>,
    before_text: Option<String>,
    context: String,
    /// The chat has taken the request and its model is on it.
    sent: bool,
    busy_seen: bool,
    status: String,
}

#[derive(Default)]
pub(super) struct Sketches {
    /// A sketch slide just added: once the deck has it, it is shown and typed on.
    open_after_add: Option<String>,
    request: Option<FancyRequest>,
    job: Option<FancyJob>,
    /// Slides whose words changed since their graphic was written.
    edited: Vec<String>,
    /// The last job's outcome, and when.
    outcome: Option<(String, f64)>,
}

fn clipped(text: &str) -> &str {
    if text.len() <= MAX_SOURCE { return text; }
    let mut end = MAX_SOURCE;
    while !text.is_char_boundary(end) { end -= 1; }
    &text[..end]
}

/// A slide as the model is shown it: its record, and its graphic's source.
fn slide_source(doc: &DeckDoc, tree: Option<&makepad_asset_data::tree::TreeDraft>, slide: &Slide) -> String {
    let record = format!("Slide{}", doc.source.get(slide.span.0..slide.span.1).unwrap_or("{}"));
    let mut text = format!("`{}`, its record in deck.splash:\n<<<\n{}\n>>>\n", slide.name, clipped(&record));
    if let (Some(path), Some(tree)) = (&slide.graphic, tree) {
        if let Ok(source) = tree.text(path) {
            text.push_str(&format!("its graphic `{path}`:\n<<<\n{}\n>>>\n", clipped(source)));
        }
    }
    text
}

/// The text of a slide's graphic file now.
fn graphic_text(page: &DeckPage, path: &str) -> Option<String> {
    page.session.as_ref()?.tree.as_ref()?.text(path).ok().map(str::to_string)
}

impl DeckPage {
    /// A new sketch slide after slide `after` (the last one when past the
    /// end): one undo step. Once it is in, the main view shows it with the
    /// typing on it. What the slide list's "New slide" calls.
    pub(crate) fn add_sketch_slide(&mut self, cx: &mut Cx, _ui: &WidgetRef, after: usize) -> Result<(), String> {
        let count = self.doc.as_ref().map_or(0, |doc| doc.slides.len());
        if count == 0 { return Err("Make or open a deck first (+ New deck)".into()); }
        let after = after.min(count - 1);
        let name = self.fresh_slide_name();
        self.submit_op(cx, DeckOp::AddSlide { after: Some(after), name: name.clone(), cut: NewCut::Sketch { text: String::new() } })?;
        self.select = Some(after + 1);
        self.file_slide = None;
        self.sketch.open_after_add = Some(name);
        Ok(())
    }

    /// Ask the chat's selected model to make slide `slide`'s graphic from
    /// its sketch, in the deck's look. The progress is in the top bar's status.
    pub(crate) fn fancy_up(&mut self, _cx: &mut Cx, _ui: &WidgetRef, slide: usize) -> Result<(), String> {
        if self.sketch.job.is_some() { return Err("Fancy it up is already running; wait for it (the chat shows it)".into()); }
        let session = self.session.as_ref().ok_or("no deck is open")?;
        let doc = &session.doc;
        let target = doc.slides.get(slide).ok_or("there is no such slide")?;
        let sketch = target.sketch.clone().ok_or_else(|| format!("slide {} (`{}`) has no sketch to fancy up", slide + 1, target.name))?;
        if sketch.trim().is_empty() { return Err("Type the slide's words first: click the slide and type".into()); }
        let tree = session.tree.as_deref();
        let path = match &target.graphic {
            Some(path) => path.clone(),
            None => match tree {
                Some(tree) => format!("{}.splash", super::super::document_tree::media_stem(tree, &format!("{}_slide", target.name), "slides")),
                None => format!("slides/{}_slide.splash", target.name),
            },
        };
        let exists = tree.is_some_and(|tree| tree.files.contains_key(&path));
        let mut context = format!(
            "FANCY IT UP: slide {} `{}` of the open deck. Write its picture as a Motion document at `{path}` ({}), size vec2({}, {}), in the deck's look; then check it with preview and attach it with set_slide {{index: {slide}, graphic: \"{path}\"}}. Keep the `sketch` field.\n\
             The person's sketch: their exact words. The first line is the title, each other line one bullet. Keep every word, its spelling and the order, unless they ask otherwise:\n<<<\n{sketch}\n>>>\n",
            slide + 1, target.name, if exists { "doc_write: it exists" } else { "doc_create" }, doc.output.0, doc.output.1,
        );
        match (&doc.look, tree) {
            (Some(look), Some(tree)) => {
                let import = look.strip_suffix(".splash").unwrap_or(look);
                context.push_str(&format!("The deck's look module `{look}` (use it: `let look = import(\"{import}\")`):\n<<<\n{}\n>>>\n", clipped(tree.text(look).unwrap_or(""))));
            }
            _ => context.push_str("The deck names no look module: follow the slides around it.\n"),
        }
        for (label, index) in [("before", slide.checked_sub(1)), ("after", Some(slide + 1))] {
            if let Some(near) = index.and_then(|index| doc.slides.get(index)) {
                context.push_str(&format!("The slide {label} it, {}", slide_source(doc, tree, near)));
            }
        }
        let line = format!(
            "Fancy it up: make slide {} (`{}`) a designed slide in this deck's look, from my sketch. Keep my words exactly. Write {path}, check it with preview, then attach it with set_slide.",
            slide + 1, target.name
        );
        self.sketch.edited.retain(|name| *name != target.name);
        self.sketch.outcome = None;
        let before_text = target.graphic.as_deref().and_then(|path| tree.and_then(|tree| tree.text(path).ok()).map(str::to_string));
        self.sketch.job = Some(FancyJob { slide: target.name.clone(), before: target.graphic.clone(), before_text, context, sent: false, busy_seen: false, status: "Fancy it up: asking the chat's model…".into() });
        self.sketch.request = Some(FancyRequest { line });
        makepad_widgets::makepad_platform::thread::SignalToUI::set_ui_signal();
        Ok(())
    }

    /// The request the chat sends next (taken once).
    pub(crate) fn take_fancy_request(&mut self) -> Option<FancyRequest> {
        let request = self.sketch.request.take()?;
        if let Some(job) = &mut self.sketch.job { job.sent = true; }
        Some(request)
    }

    /// What the model is told with each line while a job runs.
    pub(crate) fn fancy_context(&self) -> Option<&str> {
        self.sketch.job.as_ref().map(|job| job.context.as_str())
    }

    pub(crate) fn fancy_running(&self) -> bool {
        self.sketch.job.as_ref().is_some_and(|job| job.sent)
    }

    /// The chat's state while a job runs: `busy` while the model works,
    /// `error` what it reported, `provider` its name.
    pub(crate) fn fancy_progress(&mut self, cx: &Cx, busy: Option<&str>, error: Option<&str>, provider: &str) {
        let Some(job) = &mut self.sketch.job else { return };
        if !job.sent { return; }
        let now = cx.seconds_since_app_start();
        if let Some(error) = error {
            let line = format!("Fancy it up failed ({provider}): {error}");
            self.sketch.job = None;
            self.sketch.outcome = Some((line, now));
            return;
        }
        match busy {
            Some(what) => {
                job.busy_seen = true;
                job.status = format!("Fancy it up · {provider}: {}…", what.trim_end_matches('…'));
            }
            None if job.busy_seen => {
                // The model is done: its graphic landed, or it did not.
                let slide = job.slide.clone();
                let line = self.job_ended().unwrap_or_else(|| format!("Fancy it up: {provider} answered without a new graphic for `{slide}`; see the chat"));
                self.sketch.job = None;
                self.sketch.outcome = Some((line, now));
            }
            None => job.status = format!("Fancy it up · {provider}: sent, waiting…"),
        }
    }

    /// Each pump: the new slide's typing, and the job's end.
    pub(super) fn pump_sketch(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        let now = cx.seconds_since_app_start();
        if let (Some(name), Some(doc)) = (self.sketch.open_after_add.clone(), self.doc.clone()) {
            if let Some(index) = doc.slides.iter().position(|slide| slide.name == name) {
                let widget = ui.widget(cx, ids!(deck_view));
                let opened = match widget.borrow_mut::<DeckView>() {
                    Some(mut view) if view.current() == Some(index) && view.has_slide(&name) => {
                        view.edit_sketch(cx, &name, doc.slides[index].sketch.as_deref().unwrap_or_default());
                        true
                    }
                    _ => false,
                };
                if opened { self.sketch.open_after_add = None; }
            } else if !self.editing && self.pending.is_empty() && !self.load_error.is_empty() {
                // The add was refused (the status says why).
                self.sketch.open_after_add = None;
            }
        }
        let ended = self.job_ended();
        if let Some(line) = ended {
            self.sketch.outcome = Some((line, now));
            self.sketch.job = None;
        }
        if self.sketch.outcome.as_ref().is_some_and(|(_, at)| now - at > OUTCOME_SECONDS) {
            self.sketch.outcome = None;
        }
        let busy = self.sketch.job.as_ref().map(|job| job.slide.clone());
        let regenerate = self.sketch.edited.clone();
        if let Some(mut view) = ui.widget(cx, ids!(deck_view)).borrow_mut::<DeckView>() {
            view.set_sketch_marks(cx, busy, regenerate);
        }
    }

    /// The job's outcome once the deck has it: the slide's new graphic (or
    /// the slide gone). Read from the session, which has every change as
    /// soon as it is installed.
    fn job_ended(&self) -> Option<String> {
        let job = self.sketch.job.as_ref()?;
        let doc = &self.session.as_ref()?.doc;
        let rewritten = job.before.as_deref().is_some_and(|path| graphic_text(self, path) != job.before_text);
        match doc.slides.iter().find(|s| s.name == job.slide) {
            None => Some(format!("Fancy it up: slide `{}` is gone", job.slide)),
            Some(slide) if rewritten || (slide.graphic.is_some() && slide.graphic != job.before) => Some(format!(
                "Fancy it up: `{}` has its new graphic ({}); edit it in Files, or change the sketch and Regenerate",
                job.slide, slide.graphic.as_deref().unwrap_or_default()
            )),
            Some(_) => None,
        }
    }

    /// The top bar's status line of "Fancy it up" while it runs or just
    /// ended (empty otherwise): one of the lines `sync_bar_status` shows.
    pub(super) fn sketch_status(&self) -> String {
        match (&self.sketch.job, &self.sketch.outcome) {
            (Some(job), _) => job.status.clone(),
            (None, Some((line, _))) => line.clone(),
            _ => String::new(),
        }
    }

    /// "Fancy it up" is running: the top bar's spinner turns.
    pub(super) fn sketch_busy(&self) -> bool { self.sketch.job.is_some() }

    /// The main view's typing and buttons.
    pub(super) fn sketch_actions(&mut self, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
        let view = ui.widget(cx, ids!(deck_view));
        for action in actions.filter_widget_actions_cast::<SketchAction>(view.widget_uid()) {
            let Some(doc) = self.doc.clone() else { continue };
            match action {
                SketchAction::Commit { slide, text } => {
                    let Some(index) = doc.slides.iter().position(|s| s.name == slide) else { continue };
                    if doc.slides[index].sketch.as_deref() == Some(text.as_str()) { continue; }
                    let fancied = doc.slides[index].graphic.is_some();
                    match self.submit_op(cx, DeckOp::SetSlide { index, set: SlideSet { sketch: Some(text), ..SlideSet::default() } }) {
                        Ok(()) => {
                            if fancied && !self.sketch.edited.contains(&slide) { self.sketch.edited.push(slide); }
                        }
                        Err(error) => self.load_error = error,
                    }
                }
                SketchAction::FancyUp { slide } => {
                    let Some(index) = doc.slides.iter().position(|s| s.name == slide) else { continue };
                    if let Err(error) = self.fancy_up(cx, ui, index) { self.load_error = error; }
                }
                SketchAction::None => {}
            }
        }
    }

    /// The person is typing on a sketch slide: the deck's keys wait.
    pub(super) fn sketch_typing(&self, cx: &mut Cx, ui: &WidgetRef) -> bool {
        ui.widget(cx, ids!(deck_view)).borrow::<DeckView>().is_some_and(|view| view.sketch_typing())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_while_typing_commits_and_reopen_shows_the_new_words() {
        let mut typing = Typing::default();
        typing.begin("s1", "old");
        assert!(typing.is_typing());
        // The click commits first; the deck still has the old words.
        assert_eq!(typing.commit("new words".into(), Some("old")), Some(("s1".into(), "new words".into())));
        assert!(!typing.is_typing());
        assert_eq!(typing.words("s1", "old"), "new words");
        assert_eq!(typing.words("s2", "old"), "old");
        // The deck caught up, or changed otherwise (undo): its words show.
        assert_eq!(typing.words("s1", "new words"), "new words");
        assert_eq!(typing.words("s1", "other"), "other");
    }

    #[test]
    fn unchanged_words_write_nothing() {
        let mut typing = Typing::default();
        typing.begin("s1", "same");
        assert_eq!(typing.commit("same".into(), Some("same")), None);
        assert_eq!(typing.words("s1", "same"), "same");
    }
}
