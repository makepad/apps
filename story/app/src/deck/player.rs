//! The current slide's live renderer, the next slide's prepared one (its
//! first frame is ready before the key that enters it) and the previous
//! slide's, kept. A slide change shows the entered slide's own engine
//! picture; until it has one the last picture holds -- a still never stands
//! in for the main picture (stills are thumbnails only).

use super::apps::Apps;
use super::camera::Cameras;
use super::code::Codes;
use super::demo::{Demo, DemoKey, RIPPLE_SECONDS};
use super::frames::DeckFrames;
use super::render::SlideRenderer;
use super::thumbnails::SharedThumbnails;
use super::super::cut::store::LazyStoreMedia;
use makepad_story_edit_render::{CutJob, PreviewStatus, SlideMixer};
use makepad_story_edit_render::compose::{ExternalFrame, Painted};
use makepad_story_deck::{AppDecl, Deck, DemoStep, DeckDoc, Link, SlideCut, Transition};
use makepad_story_cut::{CameraSpec, CodeSpec, CutNodeKind};
use makepad_studio_protocol::MouseButton;
use makepad_widgets::*;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

/// Draws a mixed frame may take to report painted before it is shown as it
/// is (shader compiles aside).
pub(super) const PAINT_PATIENCE: u32 = 60;
/// The longest frame step a transition effect is driven with.
const MAX_STEP: f64 = 0.25;
/// How long the picture takes to come back after a demo seek.
const SEEK_FADE: f64 = 0.4;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    Next,
    Previous,
    First,
    Last,
    Black,
    Restart,
    /// The talk's clock (and the slide's) starts again at 0.
    ResetClock,
    Jump(usize),
    /// D: start or resume the slide's demo, or pause a running one at the
    /// next step; shift+D: from step 0 on a freshly launched app.
    Demo { restart: bool },
    /// Go to the start of this demo step.
    DemoSeek(usize),
    /// The play bar: the slide's story at this time (seconds into its cut),
    /// running on from there or standing; the beat follows, so Next goes on
    /// from the chosen step.
    Seek { time: f64, playing: bool },
    /// The play bar: run the slide's story on, or stop it where it is.
    Play(bool),
    /// The play bar's loop: a slide that holds its end starts over.
    Loop(bool),
}

/// What Next asked of the page that the player cannot do itself: enter
/// another deck of the tree (a link slide), or leave this one at its end
/// (back to where a link came from).
#[derive(Clone, Debug, PartialEq)]
pub enum LinkRequest {
    Follow(Link),
    End,
}

/// A pointer event on the picture.
#[derive(Clone, Copy, Debug)]
pub enum Pointer {
    Down,
    Move,
    Up,
    Scroll(Vec2d),
}

/// What became of a pointer event on the picture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PointerUse {
    /// A hosted app took it (a press also gave that app the keys).
    App,
    /// A press outside every app while one held the keys: the keys are the
    /// deck's again, and the press does nothing else.
    Released,
    /// Not the apps': the deck's own (pick, next slide).
    Deck,
}

/// An app layer of the shown slide: where its picture sits in the shown
/// picture, as fractions of its width and height.
struct Layer {
    app: String,
    frac: [f64; 4],
}

/// The demo's synthetic cursor, for the view to draw over the picture: its
/// tip as fractions of the picture, and the press ripple (0 at the press,
/// 1 faded out; None: no ripple).
pub struct Cursor {
    pub at: Vec2d,
    pub ripple: Option<f64>,
}

pub struct Player {
    /// The output window's render in another language (deck-i18n).
    pub(super) output: super::output_lane::SharedOutput,
    /// In front of an audience (ambient motion runs).
    presenting: bool,
    doc: Rc<DeckDoc>,
    deck: Deck,
    sources: HashMap<usize, SlideRenderer>,
    outgoing: Option<(usize, Texture)>,

    thumbnails: SharedThumbnails,

    /// The talk's and the shown slide's clocks.
    clock: super::presenter::TalkClock,
    mixer: SlideMixer,
    frames: Rc<DeckFrames>,
    /// The last draw's time, for the transition effect's frame step.
    last_draw: Option<f64>,
    /// A mixed frame is recorded and painting, and how many draws it has
    /// been; the newest whole one.
    mix_recorded: bool,
    mix_draws: u32,
    shown_mix: Option<Texture>,
    /// The shown slide's render serial last published, including live frames.
    last_shown: Option<(usize, u64)>,
    /// The last slide-moving key: when, and the slide and beat it went to (for
    /// the key -> frame log line).
    key_pressed: Option<(f64, usize, usize)>,
    /// The live app layers: hosting, the demo, and per slide the `AppView`
    /// nodes (node id, app name) its cut has.
    apps: Apps,
    /// The live camera layers, and per slide the `Camera` nodes (node id,
    /// what it asks for) its cut has.
    cameras: Cameras,
    camera_nodes: Vec<Vec<(String, CameraSpec)>>,
    /// The source-code layers, and per slide the `Code` nodes (node id,
    /// what it shows).
    codes: Codes,
    code_nodes: Vec<Vec<(String, CodeSpec)>>,
    demo: Demo,
    demo_selected: Option<DemoKey>,
    /// The replay on its way is one forward step: the picture stays live.
    seek_quiet: bool,
    app_nodes: Vec<Vec<(String, String)>>,
    /// The present serial last handed to each slide's renderer, by app.
    pushed: HashMap<(usize, String), u64>,

    /// The shown slide's app layers (none during a slide change).
    layers: Vec<Layer>,
    /// The presenter's picture in pixels (see `set_view_pixels`).
    view_px: (f64, f64),
    /// The apps were quit with the page: none launches until the picture is
    /// drawn again (the page is shown).
    suspended: bool,
    /// What each slide's engine last said (for the log) and since when the
    /// shown slide has had no picture.
    logged_status: HashMap<usize, String>,
    black_since: Option<(usize, f64)>,
    /// A demo seek is replaying: the shown slide's app pictures are held at
    /// the last whole frame, and when it ended (the fade back in).
    frozen: bool,
    /// The pointer over the picture as fractions 0..1, `None` off it; graphics
    /// that declare `pointer_x` / `pointer_y` of the shown slide read it.
    pointer: Option<[f64; 2]>,
    thawed_at: Option<f64>,
    /// The pixel size the slides are recorded at: the picture's size on
    /// the display (None: the deck's output size).
    raster: Option<(u32, u32)>,
    /// A link to follow or the end of the deck, for the page to act on.
    link: Option<LinkRequest>,
}

impl Player {
    /// Record every slide at `size` pixels (the picture's size on the
    /// display); None: the deck's output size.
    pub fn set_raster(&mut self, size: Option<(u32, u32)>) {
        if self.raster == size { return; }
        trace!("stage.deck", "deck: slides recorded at {}", size.map_or("the output size".to_string(), |(w, h)| format!("{w}x{h} px (the picture's own pixels)")));
        self.raster = size;
        for source in self.sources.values_mut() { source.set_raster(size); }
    }

    pub fn suspend_renderer(&mut self) {
        self.sources.clear();
        self.outgoing = None;
        self.mix_recorded = false;
    }

    pub fn placed(&self) -> Vec<(String, [f64; 4])> {
        self.sources.get(&self.deck.current()).map(SlideRenderer::placed).unwrap_or_default()
    }

    /// The shown slide has a picture to pick on: the elements are laid out
    /// from the document at that picture's time, so a frame that is still
    /// waiting for glyphs at a drifting size (a slow camera zoom never
    /// settles them) picks as well as an exact one.
    pub fn inspection_ready(&self) -> bool {
        self.sources.get(&self.deck.current()).is_some_and(|source| source.frame().is_some())
    }

    pub fn inspect_graphics(&mut self, cx: &mut Cx2d) -> Vec<makepad_story_edit_render::graphic::GraphicInspection> {
        self.sources.get_mut(&self.deck.current()).map(|source| source.inspect_graphics(cx)).unwrap_or_default()
    }

    /// Each slide's app views, cameras and code layers.
    #[allow(clippy::type_complexity)]
    fn slide_nodes(doc: &DeckDoc) -> (Vec<Vec<(String, String)>>, Vec<Vec<(String, CameraSpec)>>, Vec<Vec<(String, CodeSpec)>>) {
        let app_nodes = doc
            .slides
            .iter()
            .map(|slide| match &slide.cut {
                SlideCut::Inline(cut) => cut
                    .nodes
                    .iter()
                    .filter_map(|node| match &node.kind {
                        CutNodeKind::AppView { app, .. } => Some((node.id.clone(), app.clone())),
                        _ => None,
                    })
                    .collect(),
                SlideCut::Library { .. } | SlideCut::Pending => Vec::new(),
            })
            .collect();
        let camera_nodes = doc
            .slides
            .iter()
            .map(|slide| match &slide.cut {
                SlideCut::Inline(cut) => cut
                    .nodes
                    .iter()
                    .filter_map(|node| match &node.kind {
                        CutNodeKind::Camera(spec) => Some((node.id.clone(), spec.clone())),
                        _ => None,
                    })
                    .collect(),
                SlideCut::Library { .. } | SlideCut::Pending => Vec::new(),
            })
            .collect();
        let code_nodes = doc
            .slides
            .iter()
            .map(|slide| match &slide.cut {
                SlideCut::Inline(cut) => cut
                    .nodes
                    .iter()
                    .filter_map(|node| match &node.kind {
                        CutNodeKind::Code(spec) => Some((node.id.clone(), spec.clone())),
                        _ => None,
                    })
                    .collect(),
                SlideCut::Library { .. } | SlideCut::Pending => Vec::new(),
            })
            .collect();
        (app_nodes, camera_nodes, code_nodes)
    }

    /// The full deck in place of its first-slide load (the same slides,
    /// every graphic now loaded): engines, apps and the clock carry on.
    pub fn complete(&mut self, doc: Rc<DeckDoc>) {
        (self.app_nodes, self.camera_nodes, self.code_nodes) = Self::slide_nodes(&doc);
        self.doc = doc;
    }

    pub fn new(cx: &mut Cx, doc: Rc<DeckDoc>, frames: Rc<DeckFrames>, thumbnails: SharedThumbnails) -> Player {
        let now = Cx::time_now();
        let (app_nodes, camera_nodes, code_nodes) = Self::slide_nodes(&doc);
        let apps = Apps::new(cx, &doc);
        Player {
            apps,
            cameras: Cameras::new(&*cx),
            camera_nodes,
            codes: Codes::new(),
            code_nodes,
            demo: Demo::default(),
            demo_selected: None,
            seek_quiet: false,
            app_nodes,
            pushed: HashMap::new(),

            layers: Vec::new(),
            view_px: (0.0, 0.0),
            suspended: true,
            logged_status: HashMap::new(),
            black_since: None,
            frozen: false,
            pointer: None,
            thawed_at: None,
            raster: None,
            link: None,
            deck: Deck::new(doc.slides.len(), now),
            doc,
            sources: HashMap::new(),
            outgoing: None,

            thumbnails,

            clock: super::presenter::TalkClock::new(now),
            mixer: SlideMixer::new(cx),
            output: super::output_lane::shared(),
            presenting: false,
            frames,
            last_draw: None,
            mix_recorded: false,
            mix_draws: 0,
            shown_mix: None,
            last_shown: None,
            key_pressed: None,
        }
    }

    /// The shown slide's engine is still on its way to a first picture.
    pub fn starting(&self) -> bool {
        self.sources.get(&self.deck.current()).is_some_and(|source| source.frame().is_none() && !matches!(source.status(), PreviewStatus::Failed(_)))
    }

    pub fn doc(&self) -> &Rc<DeckDoc> {
        &self.doc
    }

    pub fn current(&self) -> usize {
        self.deck.current()
    }

    /// Replace render state while preserving compatible app processes and
    /// in-flight builds by slide name. Editing a picture never restarts them.
    pub fn replace_doc(&mut self, cx: &mut Cx, doc: Rc<DeckDoc>) {
        let current = self.doc.slides.get(self.deck.current()).and_then(|slide| doc.slides.iter().position(|new| new.name == slide.name));
        let selected = self.demo_selected.as_ref().and_then(|(index, name)| {
            let slide = self.doc.slides.get(*index)?;
            let index = doc.slides.iter().position(|new| new.name == slide.name)?;
            Some((index, name.clone()))
        });
        if let Some((slide, name)) = self.apps.focus().cloned() {
            self.apps.release_input(slide, &name);
            self.apps.flush();
        }
        self.cameras.shutdown(cx);
        self.demo.remap(&self.doc, &doc);
        self.apps.remap(cx, &self.doc, &doc);
        let mut replacement = Self::new(cx, doc, self.frames.clone(), self.thumbnails.clone());
        if let Some(index) = current { replacement.deck.resume(index, &self.deck); }
        replacement.clock = self.clock;
        replacement.clock.enter_slide(replacement.deck.entered_at());
        replacement.suspended = self.suspended;
        replacement.demo = std::mem::take(&mut self.demo);
        replacement.demo_selected = selected;
        replacement.raster = self.raster;
        // The picture shown now holds until the edited slide's engine has
        // its own (no black frame, no still, on every edit).
        replacement.outgoing = self.frames.texture().map(|texture| (replacement.deck.current(), texture));
        std::mem::swap(&mut replacement.apps, &mut self.apps);
        *self = replacement;
        if self.doc.slides.is_empty() { self.apps.shutdown(cx); }
        else { self.reconcile_apps(cx); }
    }

    pub fn take_link(&mut self) -> Option<LinkRequest> {
        self.link.take()
    }

    /// Show deck `doc` from slide `to`, entered through `transition` from the
    /// picture shown now (a link into another deck of the tree, or the way
    /// back). Hosted apps of slides with the same name carry over, as on an
    /// edit; the others quit. The talk's clock keeps running.
    pub fn jump(&mut self, cx: &mut Cx, doc: Rc<DeckDoc>, to: usize, transition: Transition) {
        let shown = self.frames.texture();
        if let Some((slide, name)) = self.apps.focus().cloned() {
            self.apps.release_input(slide, &name);
            self.apps.flush();
        }
        if let Some(key) = self.demo_selected.take() { self.demo.leave(&key); }
        self.cameras.shutdown(cx);
        self.demo.remap(&self.doc, &doc);
        self.apps.remap(cx, &self.doc, &doc);
        let now = Cx::time_now();
        let mut next = Self::new(cx, doc, self.frames.clone(), self.thumbnails.clone());
        next.deck.enter_from_outside(to, transition, now);
        next.clock = self.clock;
        next.clock.enter_slide(now);
        next.suspended = self.suspended;
        next.demo = std::mem::take(&mut self.demo);
        next.raster = self.raster;
        next.outgoing = shown.map(|texture| (makepad_story_deck::OUTSIDE, texture));
        next.key_pressed = Some((now, next.deck.current(), 0));
        std::mem::swap(&mut next.apps, &mut self.apps);
        *self = next;
        {
            let mut thumbnails = self.thumbnails.borrow_mut();
            thumbnails.current = self.deck.current();
            thumbnails.revision = thumbnails.revision.wrapping_add(1);
        }
        if self.doc.slides.is_empty() { self.apps.shutdown(cx); } else { self.reconcile_apps(cx); }
    }

    /// Present mode starts: the output must run the slide as written, not
    /// stand where the editor's play bar left it or loop.
    pub fn start_presenting(&mut self) {
        self.presenting = true;
        self.deck.set_looped(false);
        if self.deck.paused() { self.deck.play(&self.doc, Cx::time_now()); }
    }

    /// Back in the editor: a slide's ambient clock rests with its timeline.
    pub fn stop_presenting(&mut self) {
        self.presenting = false;
    }

    /// The shown slide as the play bar reads it.
    pub fn playbar(&self, presenting: bool) -> Option<super::playbar::Input<'_>> {
        use super::playbar::{Input, Story};
        let current = self.deck.current();
        let slide = self.doc.slides.get(current)?;
        let now = Cx::time_now();
        let duration = slide.cut.duration();
        let time = self.deck.slide_time(&self.doc, current, now);
        let story = if self.deck.paused() {
            Story::Paused
        } else if self.deck.holding(&self.doc, now) {
            if self.deck.beat() < slide.beats.len() { Story::Holding } else { Story::Ended }
        } else {
            Story::Moving
        };
        let source = self.sources.get(&current).filter(|source| source.status() == PreviewStatus::Idle);
        let demo = self.demo_key().and_then(|key| self.demo.playbar(&key, self.demo_steps().len()));
        Some(Input {
            duration, beats: &slide.beats, time, story, looped: self.deck.looped(),
            independent: source.map(SlideRenderer::time_independent),
            app: !slide.apps.is_empty(), demo, presenting,
        })
    }

    pub fn beats(&self) -> (usize, usize) {
        let count = self.doc.slides.get(self.current()).map_or(0, |slide| slide.beats.len());
        (self.deck.beat().min(count), count)
    }

    pub fn elapsed(&self) -> (f64, f64) {
        let now = Cx::time_now();
        self.clock.elapsed(now)
    }

    pub fn command(&mut self, cx: &mut Cx, command: Command) {
        let now = Cx::time_now();
        let before = self.deck.current();
        // A live-coding app that takes Next (`next: true`) has it while its demo runs.
        if command == Command::Next && self.deck.incoming().is_none() && self.next_to_app(cx) {
            return;
        }
        // On an app slide the arrows walk the demo's steps first: its stops
        // are the start, after every step and the end, and only from the end
        // (or back at the start) do they leave the slide.
        if matches!(command, Command::Next | Command::Previous) && self.walk_demo(cx, command == Command::Next) {
            return;
        }
        // Next past a link slide's last beat jumps into the linked deck; past
        // the last slide it may return to where a link came from. The page
        // switches decks (it holds the tree and the way back).
        if command == Command::Next && self.deck.incoming().is_none() && self.deck.next_leaves(&self.doc, now) {
            if let Some(link) = self.doc.slides.get(before).and_then(|slide| slide.link.clone()) {
                self.link = Some(LinkRequest::Follow(link));
                return;
            }
            if before + 1 >= self.doc.slides.len() {
                self.link = Some(LinkRequest::End);
                return;
            }
        }
        match command {
            Command::Next => self.deck.next(&self.doc, now),
            Command::Previous => {
                // The walk found no stop before: the demo is at its start. A
                // demo slide's story time is not where it stands (its stops are
                // the demo's), so the slide is at its start and Left leaves it.
                if self.demo_key().is_some() { self.deck.restart(now); }
                self.deck.previous(&self.doc, now)
            }
            Command::First => self.deck.first(&self.doc, now),
            Command::Last => self.deck.last(&self.doc, now),
            Command::Jump(to) => self.deck.go(&self.doc, to, now),
            Command::Black => self.deck.toggle_black(),
            Command::Restart => self.deck.restart(now),
            Command::ResetClock => self.clock.reset(now),
            Command::Demo { restart } => self.toggle_demo(cx, restart),
            Command::DemoSeek(to) => {
                let keep_playing = self.demo_key().is_some_and(|key| self.demo.running(&key));
                self.seek_demo(cx, to, keep_playing);
            }
            Command::Seek { time, playing } => self.deck.seek(&self.doc, time, playing, now),
            Command::Play(true) => self.deck.play(&self.doc, now),
            Command::Play(false) => self.deck.pause(&self.doc, now),
            Command::Loop(on) => self.deck.set_looped(on),
        }
        // The play bar's commands move the clock of the slide shown: no slide
        // changed, so nothing else (the slide list's cards) is told.
        if matches!(command, Command::Seek { .. } | Command::Play(_) | Command::Loop(_)) { return; }
        let after = self.deck.current();
        if after != before {
            self.outgoing = self.sources.get(&before).and_then(|source| source.frame()).map(|(_, frame)| (before, frame)).or_else(|| self.frames.texture().map(|texture| (before, texture)));
            let keep = self.deck.keep();
            self.sources.retain(|index, _| keep.contains(index));
        }
        if matches!(command, Command::Next | Command::Previous | Command::First | Command::Last | Command::Jump(_)) {
            self.key_pressed = Some((now, after, self.deck.beat()));
        }
        if after != before || command == Command::Restart {
            self.clock.enter_slide(now);
        }
        {
            let mut thumbnails = self.thumbnails.borrow_mut();
            thumbnails.current = after;
            thumbnails.revision = thumbnails.revision.wrapping_add(1);
        }
        if after != before {
            // Leaving a slide stops its demo (it resumes at the step it
            // reached) and the app no longer holds the keys.
            if let Some(key) = self.demo_selected.take() {
                self.demo.leave(&key);
                self.apps.release_input(key.0, &key.1);
            }
            self.apps.set_focus(None);
            self.layers.clear();
            self.reconcile_apps(cx);
            // Left into a slide with a demo arrives at its end: the app as the
            // demo leaves it (a fast replay of every step).
            if command == Command::Previous && self.has_demo() {
                self.seek_demo(cx, self.demo_steps().len(), false);
                self.seek_quiet = false;
            }
        }
    }

    /// Per-app demos follow the active AppView's story span. Older decks
    /// keep their slide-level demo on the first app.
    fn demo_app(&self) -> Option<&AppDecl> {
        let slide = self.doc.slides.get(self.deck.current())?;
        if slide.apps.iter().any(|app| !app.demo.is_empty()) {
            let SlideCut::Inline(cut) = &slide.cut else { return None };
            let t = cut.shown_time(self.deck.slide_time(&self.doc, self.deck.current(), Cx::time_now()));
            cut.nodes.iter().rev().find_map(|node| match &node.kind {
                CutNodeKind::AppView { app, .. } if node.active(t) =>
                    slide.apps.iter().find(|decl| decl.name == *app && !slide.demo_for(decl).is_empty()),
                _ => None,
            })
        } else {
            slide.apps.first()
        }
    }

    fn demo_key(&self) -> Option<DemoKey> {
        Some((self.deck.current(), self.demo_app()?.name.clone()))
    }

    pub fn demo_steps(&self) -> &[DemoStep] {
        let Some(app) = self.demo_app() else { return &[] };
        self.doc.slides[self.deck.current()].demo_for(app)
    }

    fn toggle_demo(&mut self, cx: &mut Cx, restart: bool) {
        let current = self.deck.current();
        let Some(name) = self.demo_app().map(|decl| decl.name.clone()) else { return };
        let key = (current, name.clone());
        let steps = self.demo_steps().len();
        // A demo that ran through starts over on a fresh app: its steps
        // expect the state it began in.
        let restart = restart || self.demo.finished(&key) || self.demo.interrupted(&key);
        if restart {
            self.apps.relaunch(cx, current, &name);
            self.pushed.remove(&key);
            self.demo.relaunched(&key);
        }
        let Some(generation) = self.apps.slot(current, &name).map(|slot| slot.generation) else { return };
        self.demo.toggle(&key, steps, generation, restart);
    }

    /// Next is the app's (`next: true`) while the slide's demo has begun and
    /// not finished: the app is sent `deck.next`, a demo the presenter paused
    /// plays on, and the slide stays. True when the app took it.
    fn next_to_app(&mut self, cx: &mut Cx) -> bool {
        let current = self.deck.current();
        let Some(name) = self.demo_app().filter(|decl| decl.next).map(|decl| decl.name.clone()) else { return false };
        let key = (current, name.clone());
        if !self.demo.unfinished(&key) { return false; }
        if let Some(slot) = self.apps.slot_mut(current, &name) {
            slot.hosted.send(vec![makepad_studio_protocol::StudioToApp::Custom("deck.next".into())]);
        }
        if self.demo.paused(&key) { self.toggle_demo(cx, false); }
        self.apps.flush();
        true
    }

    /// An arrow on a slide with a demo: go to the next stop (forward: the
    /// step in flight, or the next, performed at once -- no cursor travel,
    /// typing or holds, and its waits end as soon as the app shows what they
    /// wait for) or the one before (back: the app relaunched and the steps
    /// before replayed the same way). The picture is not veiled for a
    /// forward step: the step is the thing to watch. False: no stop that
    /// way, the deck's own next / previous slide.
    fn walk_demo(&mut self, cx: &mut Cx, forward: bool) -> bool {
        let Some(key) = self.demo_key() else { return false };
        let steps = self.demo_steps().len();
        let Some(to) = self.demo.arrow_stop(&key, steps, forward) else { return false };
        let relaunch = self.demo.needs_relaunch(&key, to);
        self.seek_demo(cx, to, false);
        self.seek_quiet = forward && !relaunch;
        true
    }

    /// Put the slide's app as it is when demo step `to` is about to begin: a
    /// seek back relaunches it into a clean home, and either way the steps
    /// before `to` that it has not run are replayed as fast as it goes. The
    /// picture holds its last frame meanwhile. `then_run`: play on from `to`.
    fn seek_demo(&mut self, cx: &mut Cx, to: usize, then_run: bool) {
        let current = self.deck.current();
        let Some(name) = self.demo_app().map(|decl| decl.name.clone()) else { return };
        let key = (current, name.clone());
        let steps = self.demo_steps().len();
        let to = to.min(steps);
        if self.demo.needs_relaunch(&key, to) {
            self.apps.relaunch(cx, current, &name);
            self.pushed.remove(&key);
            self.demo.relaunched(&key);
        }
        let Some(generation) = self.apps.slot(current, &name).map(|slot| slot.generation) else { return };
        self.demo.seek(&key, to, steps, generation, then_run);
    }

    /// Whether the shown slide has a demo the transport can play.
    pub fn has_demo(&self) -> bool {
        !self.demo_steps().is_empty()
    }

    /// How dark the picture is held while a seek replays, and how it fades
    /// back in after (1 down to 0); None: not at all.
    pub fn veil(&self) -> Option<f32> {
        if self.frozen {
            return Some(1.0);
        }
        let fade = (Cx::monotonic_now() - self.thawed_at?) / SEEK_FADE;
        (fade < 1.0).then(|| (1.0 - fade) as f32)
    }

    /// The shown slide's app status (building, starting, failed), for the
    /// presenter and editor view to draw over the picture; never part of
    /// the picture itself.
    pub fn status_overlay(&self) -> Option<super::apps::StatusOverlay> {
        if self.deck.incoming().is_some() { return None; }
        self.apps.overlay(self.deck.current())
    }

    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event) -> bool {
        let mut changed = false;
        // The output window's pointer on its picture: the apps' only (a
        // press there never moves the deck on).
        let queued = self.output.borrow_mut().take_pointer();
        if !queued.is_empty() {
            for (kind, at, time, modifiers) in queued {
                changed |= self.pointer(kind, at, time, &modifiers) != PointerUse::Deck;
            }
            cx.set_cursor(self.pointer_cursor());
        }

        for source in self.sources.values_mut() {
            source.handle_event(cx, event);
        }
        self.mixer.handle_event(cx, event);
        let transitioning = self.deck.incoming().is_some();
        self.deck.tick(Cx::time_now());
        if transitioning && self.deck.incoming().is_none() {
            changed |= self.reconcile_apps(cx);
        }
        if self.apps.handle_event(cx, event) {
            self.poll_demo();
            changed |= self.reconcile_apps(cx);
            // A held slide sleeps: a new present of its apps is drawn now.
            changed |= !self.suspended && self.doc.slides.get(self.deck.current()).is_some() && self.apps_moving(self.deck.current());
        }
        if self.cameras.handle_event(event) {
            changed |= self.reconcile_apps(cx);
        }
        // A shown source file changed on disk (the watcher signalled).
        if matches!(event, Event::Signal) && self.codes.moving(self.deck.current()) {
            changed = true;
        }
        changed
    }

    /// Apps with a currently active AppView run, including during slide
    /// transitions. Only the current slide may start apps, after a full
    /// second there; outgoing apps quit when the transition ends.
    /// Run on every draw and on the apps' beat
    /// (a picture that is not being redrawn still keeps its apps honest).
    fn reconcile_apps(&mut self, cx: &mut Cx) -> bool {
        if self.suspended || self.doc.slides.is_empty() { return false; }
        let doc = self.doc.clone();
        let (current, active) = self.lifecycle();
        let declared: Vec<(usize, &[AppDecl])> = active.iter().map(|&index| (index, doc.slides[index].apps.as_slice())).collect();
        let now = Cx::time_now();
        let mut shown = Vec::new();
        for &index in &active {
            let slide = &doc.slides[index];
            let t = self.deck.slide_time(&doc, index, now);
            for app in &slide.apps {
                // Apps with no AppView run for the whole active slide.
                let running = match &slide.cut {
                    SlideCut::Inline(cut) => {
                        let mut views = cut.nodes.iter().filter(|node| matches!(&node.kind, CutNodeKind::AppView { app: name, .. } if *name == app.name)).peekable();
                        // A held end shows the last frame: its apps stay live.
                        let t = cut.shown_time(t);
                        views.peek().is_none() || views.any(|node| node.active(t))
                    }
                    SlideCut::Library { .. } | SlideCut::Pending => true,
                };
                if running { shown.push((index, app.name.clone())); }
            }
        }
        if let Some((index, name)) = self.apps.focus().cloned() {
            if !shown.contains(&(index, name.clone())) {
                self.apps.release_input(index, &name);
                self.apps.set_focus(None);
            }
        }
        self.apps.reconcile(cx, &declared, current, &active, &shown);
        let mut changed = false;
        for index in self.apps.take_status_changes() {
            if !active.contains(&index) {
                // Deck::keep retains nearby picture engines. A retired app's
                // engine must release the swapchain textures it sampled too.
                self.sources.remove(&index);
            }
            changed = true;
            if self.app_nodes[index].is_empty() { continue; }

            // A relaunched app can have the same serial as its previous child.
            self.pushed.retain(|(slide, _), _| *slide != index);
        }
        let cameras: Vec<(usize, Vec<(String, CameraSpec)>)> = active.iter().map(|&index| (index, self.camera_nodes[index].clone())).collect();
        self.cameras.reconcile(cx, &cameras, &active, &[]);
        changed
    }

    /// The sole slide allowed to start apps, and the slides allowed to
    /// retain resources. The outgoing transition holds only its final texture.
    fn lifecycle(&self) -> (usize, Vec<usize>) { (self.deck.current(), vec![self.deck.current()]) }

    /// Set up only current-slide apps. Only the app shown in this story
    /// segment receives demo input.
    fn poll_demo(&mut self) {
        if self.suspended { return; }
        let selected = self.demo_key();
        if self.demo_selected != selected {
            if let Some(key) = self.demo_selected.take() {
                self.demo.leave(&key);
                self.apps.release_input(key.0, &key.1);
            }
            self.apps.set_focus(None);
            self.demo_selected = selected.clone();
        }
        let current = self.deck.current();
        let now = Cx::monotonic_now();
        let mut setting_up = false;
        if let Some(slide) = self.doc.slides.get(current) {
            let index = current;
            for app in &slide.apps {
                let key = (index, app.name.clone());
                let pending = if let Some(slot) = self.apps.slot_mut(index, &app.name) {
                    let dpi = slot.dpi;
                    self.demo.poll_setup(&key, slide.setup_for(app), slot, dpi, now)
                } else { false };
                if selected.as_ref() == Some(&key) { setting_up = pending; }
                self.apps.set_warm(index, &app.name, pending);
            }
        }
        if let Some(key) = selected {
            if !setting_up {
                let slide = &self.doc.slides[key.0];
                if let Some(app) = slide.apps.iter().find(|app| app.name == key.1) {
                    if let Some(slot) = self.apps.slot_mut(key.0, &key.1) {
                        let dpi = slot.dpi;
                        self.demo.poll(&key, slide.demo_for(app), slot, dpi, now);
                    }
                }
            }
        }
        self.apps.flush();
    }

    /// The system's cameras as the platform lists them (None: not yet) and
    /// a number that changes when the list does.
    pub fn camera_devices(&self) -> (u64, Option<&[makepad_widgets::makepad_platform::video::VideoInputDesc]>) {
        (self.cameras.revision(), self.cameras.devices())
    }

    /// Quit every hosted app (the page closes, the document changes, Stage
    /// shuts down).
    pub fn shutdown(&mut self, cx: &mut Cx) {
        self.suspended = true;
        self.apps.set_focus(None);
        self.apps.shutdown(cx);
        self.cameras.shutdown(cx);
        self.sources.clear();
        self.outgoing = None;
        self.pushed.clear();
    }

    /// The pointer over the picture (`None` off it). True when a graphic on
    /// the shown slide reads it and it moved: the picture needs a repaint.
    pub fn set_pointer(&mut self, pointer: Option<[f64; 2]>) -> bool {
        if pointer.is_none() { self.apps.set_hover(None); }
        if self.pointer == pointer {
            return false;
        }
        self.pointer = pointer;
        self.sources.get(&self.deck.current()).is_some_and(|source| source.reads_pointer())
    }

    /// A pointer event on the picture at `at` (fractions of the shown
    /// picture, letterboxing and scale already taken out): hit-tested
    /// against where each app's frame sits on the shown slide.
    pub fn pointer(&mut self, kind: Pointer, at: Vec2d, time: f64, mods: &KeyModifiers) -> PointerUse {
        let current = self.deck.current();
        let hit = self.layers.iter().rev().find(|layer| contains(&layer.frac, at)).map(|layer| layer.app.clone());
        let focus = self.apps.focus().filter(|(slide, _)| *slide == current).map(|(_, name)| name.clone());
        // The app under the pointer (or the one a press is down in) shows its cursor.
        self.apps.set_hover(self.pressed_app(current).or_else(|| hit.clone()).map(|app| (current, app)));
        let used = match kind {
            Pointer::Down => match hit {
                Some(app) => {
                    if focus.as_ref().is_some_and(|focus| *focus != app) { self.release_app_focus(); }
                    self.apps.set_focus(Some((current, app.clone())));
                    self.send_pointer(current, &app, kind, at, time, mods);
                    PointerUse::App
                }
                // A click outside the app gives the keys back to the deck.
                None if self.release_app_focus() => PointerUse::Released,
                None => PointerUse::Deck,
            },
            // A press keeps its app until the release; hovers go to the
            // app under the pointer.
            Pointer::Move | Pointer::Up => match self.pressed_app(current).or(hit) {
                Some(app) => {
                    self.send_pointer(current, &app, kind, at, time, mods);
                    PointerUse::App
                }
                None => PointerUse::Deck,
            },
            Pointer::Scroll(_) => match hit {
                Some(app) => {
                    self.send_pointer(current, &app, kind, at, time, mods);
                    PointerUse::App
                }
                None => PointerUse::Deck,
            },
        };
        if used == PointerUse::App && !matches!(kind, Pointer::Move) {
            if let Some(key) = self.demo_key() { self.demo.presenter_input(&key); }
        }
        self.apps.flush();
        used
    }

    /// The presenter's picture as drawn, in pixels (its size times the
    /// window's dpi); zero while it is not drawn.
    pub fn set_view_pixels(&mut self, px: (f64, f64)) {
        self.view_px = px;
    }

    /// The largest picture the shown slide is on, in pixels: the deck's
    /// output size, the presenter's picture, the output window's.
    fn picture_pixels(&self, output: (u32, u32)) -> (f64, f64) {
        let window = self.output.borrow().shown_pixels(Cx::monotonic_now());
        let pictures = [(output.0 as f64, output.1 as f64), self.view_px, window.unwrap_or_default()];
        // The same aspect throughout: the widest wins.
        pictures.into_iter().fold((0.0, 0.0), |best, px| if px.0 > best.0 { px } else { best })
    }

    /// The cursor for the pointer on the picture: the app's under it (what
    /// the app asked for), else the arrow.
    pub fn pointer_cursor(&self) -> MouseCursor {
        self.apps.hover_cursor()
    }

    /// The shown slide's app a pointer press is down in.
    fn pressed_app(&self, current: usize) -> Option<String> {
        self.apps.slots_of(current).find(|slot| slot.pressed).map(|slot| slot.key.1.clone())
    }

    /// The keys are the deck's again (Escape, a click outside the app): a
    /// press still down in the app is released first. True when an app
    /// held them.
    pub fn release_app_focus(&mut self) -> bool {
        let Some((slide, name)) = self.apps.focus().cloned() else { return false };
        self.apps.release_input(slide, &name);
        self.apps.set_focus(None);
        self.apps.flush();
        true
    }

    fn send_pointer(&mut self, slide: usize, app: &str, kind: Pointer, at: Vec2d, time: f64, mods: &KeyModifiers) {
        let Some(frac) = self.layers.iter().find(|layer| layer.app == app).map(|layer| layer.frac) else { return };
        let Some(slot) = self.apps.slot_mut(slide, app) else { return };
        let size = slot.size();
        let local = dvec2((at.x - frac[0]) / frac[2].max(1e-9) * size.x, (at.y - frac[1]) / frac[3].max(1e-9) * size.y);
        match kind {
            Pointer::Down => {
                slot.pressed = true;
                slot.hosted.mouse_down(local, MouseButton::PRIMARY, time, mods);
            }
            Pointer::Move if slot.pressed => slot.hosted.mouse_move(local, time, mods),
            Pointer::Move => slot.hosted.hover_move(local, time, mods),
            Pointer::Up => {
                if slot.pressed {
                    slot.pressed = false;
                    slot.hosted.mouse_up(Some(local), MouseButton::PRIMARY, time, mods, false);
                }
            }
            Pointer::Scroll(delta) => slot.hosted.scroll(local, delta, true, makepad_studio_protocol::ScrollPhase::None, time, mods),
        }
    }

    /// Keys and text while an app has the focus: Escape lets go (and does
    /// nothing else), anything else -- the arrows too -- goes to the app.
    /// True when the event was the app's.
    pub fn app_input(&mut self, event: &Event) -> bool {
        let current = self.deck.current();
        let Some(app) = self.apps.key_holder(current).map(str::to_string) else { return false };
        if let Event::KeyDown(key) = event {
            if key.key_code == KeyCode::Escape {
                self.release_app_focus();
                return true;
            }
        }
        let Some(slot) = self.apps.slot_mut(current, &app) else { return false };
        match event {
            Event::KeyDown(key) => slot.hosted.key_down(key.clone()),
            Event::KeyUp(key) => slot.hosted.key_up(key.clone()),
            Event::TextInput(input) => slot.hosted.text_input(input.clone()),
            Event::TextCopy(_) => slot.hosted.text_copy(),
            Event::TextCut(_) => slot.hosted.text_cut(),
            _ => return false,
        }
        self.demo.presenter_input(&(current, app));
        self.apps.flush();
        true
    }

    /// The demo's cursor over the shown slide's app, if it plays.
    pub fn cursor(&self) -> Option<Cursor> {
        let current = self.deck.current();
        let name = &self.demo_app()?.name;
        let (x, y) = self.demo.cursor(&(current, name.clone()))?;
        let layer = self.layers.iter().find(|layer| layer.app == *name)?;
        let slot = self.apps.slot(current, name)?;
        let size = slot.size();
        let at = dvec2(layer.frac[0] + x / size.x.max(1.0) * layer.frac[2], layer.frac[1] + y / size.y.max(1.0) * layer.frac[3]);
        let ripple = slot.pressed_at.map(|pressed| (Cx::monotonic_now() - pressed) / RIPPLE_SECONDS).filter(|age| *age < 1.0);
        Some(Cursor { at, ripple })
    }

    /// What the renderers say is wrong or still loading, for the status line.
    pub fn status(&self) -> String {
        let current = self.deck.current();
        let mut lines = Vec::new();
        if let Some(slide) = self.doc.slides.get(current) {
            if matches!(slide.cut, SlideCut::Library { .. }) {
                return format!("slide `{}`: library cuts are not played yet", slide.name);
            }
        }
        match self.sources.get(&current).map(|source| source.status()) {
            Some(PreviewStatus::Failed(error)) => lines.push(format!("slide failed: {error}")),
            // A player that is not drawn (no room for its picture) loads
            // nothing: saying so turned the bar's spinner for ever.
            Some(PreviewStatus::Loading) | None if !self.suspended => lines.push("loading the slide".into()),
            _ => {}
        }
        lines.extend(self.apps.notes(current));
        lines.extend(self.cameras.notes(current));
        lines.extend(self.codes.notes(current));
        if let Some(key) = self.demo_key() {
            if let Some(line) = self.demo.status(&key, self.demo_steps(), Cx::monotonic_now()) {
                lines.push(line);
            }
        }
        if let Some(slot) = self.apps.focus().and_then(|(slide, name)| self.apps.slot(*slide, name)) {
            lines.push(format!("keys go to {} (Escape or a click outside it lets go)", slot.display_name));
        }
        lines.join("\n")
    }

    fn make_source(cx: &mut Cx, cut: &SlideCut, raster: Option<(u32, u32)>) -> Option<SlideRenderer> {
        let SlideCut::Inline(doc) = cut else { return None };
        let job = CutJob { doc: Arc::new((**doc).clone()), marks: HashMap::new(), resolver: Box::new(LazyStoreMedia::new()), audio: false };
        let mut source = SlideRenderer::new(cx, job);
        source.set_raster(raster);
        Some(source)
    }

    /// Nothing on the shown picture is moving or loading: no slide change,
    /// no live app, a held story with no live
    /// readers and an engine with nothing left to draw. The view then
    /// stops asking for frames until something happens.
    pub fn idle(&self) -> bool {
        let current = self.deck.current();
        let Some(slide) = self.doc.slides.get(current) else { return true };
        if self.deck.incoming().is_some() || self.mix_recorded || self.output.borrow().busy() || self.suspended || self.apps_moving(current) || !self.camera_nodes[current].is_empty() || self.codes.moving(current) {
            return false;
        }
        let Some(source) = self.sources.get(&current) else { return matches!(slide.cut, SlideCut::Library { .. }) };
        // A prepared neighbour still on its way to its first frame.
        if self.sources.values().any(|source| !matches!(source.status(), PreviewStatus::Idle | PreviewStatus::Failed(_))) { return false; }
        // Never from "nothing happened for a while": the slide sleeps only when
        // its timeline has reached a hold (a beat, or the end of a cut that
        // holds) and the picture drawn is the picture of that hold.
        let now = Cx::time_now();
        // Or because nothing on it can change with time: the cut keeps playing,
        // its picture stays (a still title over a live app on a looping cut).
        source.status() == PreviewStatus::Idle && !ambient_runs(source.reads_live(), self.presenting)
            && picture_sleeps(source.time_independent(), self.deck.holding(&self.doc, now) && source.wants_frame_of(self.deck.slide_time(&self.doc, current, now)))
    }

    /// The shown slide's apps change its picture: one has a new frame or
    /// status card to record or a render size settling, or a demo or an
    /// app's setup runs (their steps are timed). Apps that are still leave
    /// the slide idle; their next present wakes it through the beat.
    fn apps_moving(&self, current: usize) -> bool {
        let slide = &self.doc.slides[current];
        if slide.apps.is_empty() {
            return false;
        }
        if self.frozen || self.apps.setting_up() {
            return true;
        }
        if self.demo_key().is_some_and(|key| self.demo.running(&key) || self.demo.seeking(&key).is_some()) {
            return true;
        }
        self.app_nodes[current].iter().any(|(_, app)| {
            slide.apps.iter().find(|decl| decl.name == *app)
                .is_some_and(|decl| self.apps.picture_changed(current, decl, self.pushed.get(&(current, app.clone())).copied()))
        })
    }

    /// The shown slide plays (its timeline runs) and its picture is the
    /// frame of now, with nothing else to draw: the next picture is due
    /// this many seconds from now, at the render's frame rate. A view asks
    /// for that frame then, not for every display frame in between (a
    /// 30 fps slide on a 240 Hz screen recorded nothing new on seven of
    /// eight). None while anything else needs drawing, or the slide holds
    /// (then the player is idle).
    pub fn next_picture_in(&self) -> Option<f64> {
        let current = self.deck.current();
        self.doc.slides.get(current)?;
        if self.deck.incoming().is_some() || self.mix_recorded || self.output.borrow().busy() || self.suspended || self.apps_moving(current) || !self.camera_nodes[current].is_empty() || self.codes.moving(current) {
            return None;
        }
        if self.sources.values().any(|source| !matches!(source.status(), PreviewStatus::Idle | PreviewStatus::Failed(_))) {
            return None;
        }
        let source = self.sources.get(&current)?;
        let now = Cx::time_now();
        if source.reads_live() || picture_sleeps(source.time_independent(), self.deck.holding(&self.doc, now)) {
            return None;
        }
        Some(source.next_frame_in(self.deck.slide_time(&self.doc, current, now)))
    }

    /// Record and finish this display frame's work under the pass being
    /// drawn. Returns the picture to show (None: black). A deck is
    /// always playing: the host asks for the next frame after this one.
    pub fn draw(&mut self, cx: &mut Cx2d) -> Option<Texture> {
        let now = Cx::time_now();
        let step = self.last_draw.map_or(1.0 / 60.0, |last| (now - last).clamp(1.0 / 240.0, MAX_STEP));
        self.last_draw = Some(now);
        let doc = self.doc.clone();

        if doc.slides.is_empty() {
            self.frames.show(None, false);
            return None;
        }

        self.suspended = false;
        self.deck.tick(now);
        self.deck.wrap(&doc, now);
        self.reconcile_apps(cx);
        self.poll_demo();
        // A demo seek replays out of sight: the shown slide keeps the app
        // pictures it had until the replay is over.
        let seeking = self.demo_key().is_some_and(|key| self.demo.seeking(&key).is_some());
        if !seeking { self.seek_quiet = false; }
        let seeking = seeking && !self.seek_quiet;
        if seeking != self.frozen {
            self.frozen = seeking;
            self.thawed_at = (!seeking).then(Cx::monotonic_now);
            // The app may be a new launch whose present serials start over.
            let current = self.deck.current();
            self.pushed.retain(|(index, _), _| *index != current);
        }
        self.cameras.poll(cx);

        let current = self.deck.current();
        // A key press never waits for the entered slide: the next slide's
        // engine is prepared ahead, the previous one kept; any other slide
        // holds the last picture until its engine has its first frame.
        let selected = vec![current];
        self.apps.retain_placeholders(&selected);
        let keep = self.deck.keep();
        self.sources.retain(|index, _| keep.contains(index));
        self.codes.retain(&selected);
        self.pushed.retain(|(index, _), _| *index == current);
        if !self.sources.contains_key(&current) {
            if let Some(source) = Self::make_source(cx, &doc.slides[current].cut, self.raster) {
                self.sources.insert(current, source);
            }
        }
        // The next slide is prepared once the shown one has its picture
        // (the shown slide's first frame goes first).
        let next = current + 1;
        if next < doc.slides.len() && !self.sources.contains_key(&next) && self.sources.get(&current).is_some_and(|source| source.frame().is_some()) {
            if let Some(source) = Self::make_source(cx, &doc.slides[next].cut, self.raster) {
                self.sources.insert(next, source);
            }
        }
        assert!(self.sources.len() <= 4, "engines: the shown slide, the slide being left, the previous and the next");
        let mut audience = Vec::new();
        for &index in &selected {
            let t = self.deck.slide_time(&doc, index, now);
            // In the editor the ambient clock is the timeline's: at a hold
            // the picture is settled and nothing draws.
            let live = if self.presenting { self.deck.live_time(index, now) } else { t };
            let mut output_externals = None;
            if let Some(source) = self.sources.get_mut(&index) {
                // The pictures of this slide's apps, for the frame recorded now.
                let mut externals: HashMap<String, ExternalFrame> = HashMap::new();
                let mut changed = false;
                let held = self.frozen && index == current;
                for (node, app) in self.app_nodes[index].iter().filter(|_| !held) {
                    let Some(decl) = doc.slides[index].apps.iter().find(|decl| decl.name == *app) else { continue };
                    let (frame, serial) = self.apps.frame(cx, index, decl);
                    if self.pushed.insert((index, app.clone()), serial) != Some(serial) {
                        changed = true;
                    }
                    externals.insert(node.clone(), frame);
                }
                for (node, _) in &self.camera_nodes[index] {
                    let Some(frame) = self.cameras.external(index, node) else { continue };
                    let serial = self.cameras.serial(index, node);
                    if self.pushed.insert((index, node.clone()), serial) != Some(serial) {
                        changed = true;
                    }
                    externals.insert(node.clone(), frame);
                }
                for (node, spec) in &self.code_nodes[index] {
                    let Some((frame, serial)) = self.codes.frame(cx, index, node, spec) else { continue };
                    if self.pushed.insert((index, node.clone()), serial) != Some(serial) {
                        changed = true;
                    }
                    externals.insert(node.clone(), frame);
                }
                if (!self.app_nodes[index].is_empty() && !held) || !self.camera_nodes[index].is_empty() || !self.code_nodes[index].is_empty() {
                    output_externals = Some((externals.clone(), changed));
                    source.set_externals(externals, changed);
                }
                source.seek(t);
                source.set_live(live);
                source.set_pointer(if index == current { self.pointer } else { None });
                source.draw(cx);
                audience.push(super::output_lane::SlideDrive { index, t, live, pointer: self.pointer, externals: output_externals });
            }
        }
        // The kept and prepared neighbours: their entry frame (every slide
        // is entered at its start), recorded once and then left alone. The
        // slide being left holds its last picture (`outgoing`) instead.
        let leaving = self.deck.incoming().map(|incoming| incoming.from);
        for (&index, source) in self.sources.iter_mut() {
            if index == current || Some(index) == leaving { continue; }
            source.seek(self.deck.slide_time(&doc, index, now));
            source.set_live(0.0);
            source.set_pointer(None);
            source.draw(cx);
        }
        // Where the shown slide's app pictures sit, for the pointer.
        self.layers.clear();
        if self.deck.incoming().is_none() {
            if let (Some(source), SlideCut::Inline(cut)) = (self.sources.get(&current), &doc.slides[current].cut) {
                let (ow, oh) = (cut.output.width.max(1) as f64, cut.output.height.max(1) as f64);
                for (node, rect) in source.placed() {
                    if let Some((_, app)) = self.app_nodes[current].iter().find(|(id, _)| *id == node) {
                        self.layers.push(Layer { app: app.clone(), frac: [rect[0] / ow, rect[1] / oh, rect[2] / ow, rect[3] / oh] });
                    }
                }
            }
            // Each app renders at least at the pixels its frame covers on
            // the largest picture showing the slide: the deck's output, the
            // presenter's picture at its dpi, the output window.
            let picture_px = self.picture_pixels(doc.output);
            for app in &doc.slides[current].apps {
                let frame = self.layers.iter().filter(|layer| layer.app == app.name)
                    .map(|layer| makepad_story_deck::frame_pixels([layer.frac[2], layer.frac[3]], picture_px))
                    .reduce(|a, b| (a.0.max(b.0), a.1.max(b.1)));
                if let Some(frame) = frame { self.apps.set_frame_pixels(current, &app.name, frame); }
            }
        }

        let frame_of = |sources: &HashMap<usize, SlideRenderer>, index: usize| sources.get(&index).and_then(|source| source.frame());
        let incoming = self.deck.incoming().cloned();
        let shown = match &incoming {
            None => {
                self.mix_recorded = false;
                self.shown_mix = None;
                // The entered slide's own picture; until it has one, the
                // last picture holds (never a still).
                match frame_of(&self.sources, current) {
                    Some((_, texture)) => {
                        self.outgoing = None;
                        Some(texture)
                    }
                    None => self.outgoing.as_ref().map(|(_, texture)| texture.clone()),
                }
            }
            Some(incoming) => {
                let (a, b) = (self.outgoing.as_ref().filter(|(index, _)| *index == incoming.from).map(|(_, texture)| (0.0, texture.clone())), frame_of(&self.sources, current));
                match (a, b) {
                    (Some((_, a)), Some((_, b))) => {
                        self.mixer.prepare(cx, &incoming.transition.effect);
                        if self.mix_recorded {
                            if self.mixer.painted(cx) == Painted::Whole {
                                self.shown_mix = self.mixer.output().cloned();
                                self.mix_recorded = false;
                            } else {
                                self.mix_draws += 1;
                                if self.mix_draws < PAINT_PATIENCE && !self.mixer.stale(cx) {
                                    self.mixer.reattach(cx);
                                } else {
                                    self.mix_recorded = false;
                                }
                            }
                        }
                        if !self.mix_recorded {
                            let progress = self.deck.progress(now).unwrap_or(1.0);
                            let dial = |name: &str| incoming.transition.dial(name).unwrap_or(-1.0);
                            let dials = [dial("p1"), dial("p2"), dial("p3")];
                            self.mixer.hold_output(self.shown_mix.clone());
                            self.mixer.mix(cx, &a, &b, &incoming.transition.effect, progress, dials, doc.output, now - incoming.started, step);
                            self.mix_recorded = true;
                            self.mix_draws = 0;
                        }
                        Some(self.shown_mix.clone().unwrap_or(a))
                    }
                    (Some((_, a)), None) => Some(a),
                    (None, Some((_, b))) => Some(b),
                    (None, None) => None,
                }
            }
        };
        for &index in &selected {
            if let Some(source) = self.sources.get(&index) {
                // The state, not its time: one line per change, not per frame.
                let state = match source.status() { PreviewStatus::Drawing(_) => "Drawing".to_string(), status => format!("{status:?}") };
                let status = format!("{state}{}", if source.frame().is_some() { " with a picture" } else { "" });
                if self.logged_status.get(&index) != Some(&status) {
                    trace!("stage.deck", "deck: slide {index} engine: {status} (current {current}, incoming {})", incoming.is_some());
                    super::perf::open_mark(&format!("slide {index} engine: {status}"));
                    self.logged_status.insert(index, status);
                }
            }
        }
        let picture = frame_of(&self.sources, current).is_some();
        if picture && super::perf::opening() { super::perf::open_end("first picture of the slide"); }
        match (picture, self.black_since) {
            (true, _) => self.black_since = None,
            (false, Some((slide, since))) if slide == current && now - since > 1.0 => {
                trace!("stage.deck", "deck: slide {current} has had no picture for {:.1}s (engine {:?}, shaders pending {}, incoming {})", now - since, self.sources.get(&current).map(|source| source.status()), cx.draw_shaders_pending(), incoming.is_some());
                self.black_since = Some((current, now + 4.0));
            }
            (false, Some((slide, _))) if slide == current => {}
            (false, _) => self.black_since = Some((current, now)),
        }
        if let Some((pressed, slide, beat)) = self.key_pressed {
            if slide == current && beat == self.deck.beat() && frame_of(&self.sources, current).is_some() {
                log!("deck: key -> frame {:.1} ms (slide {current}, beat {beat}, engine picture)", (now - pressed) * 1000.0);
                self.key_pressed = None;
            }
        }
        let key = self.sources.get(&current).filter(|source| source.frame().is_some()).map(|source| (current, source.serial()));
        let repainted = incoming.is_some() || key != self.last_shown;
        self.last_shown = key;
        let shown = if self.deck.black() { None } else { shown };
        // The output window: the presenter's picture, or its own render in
        // the output's language.
        // The output window draws the deck in another language itself
        // (its own renderers, in its own draw), from what is shown now.
        // Where the output window's picture sits, for its pointer.
        self.output.borrow_mut().set_output_size(doc.output);
        if self.output.borrow().active() {
            let progress = self.deck.progress(now).unwrap_or(1.0);
            let black = self.deck.black();
            self.output.borrow_mut().request(super::output_lane::OutputRequest { doc: doc.clone(), slides: audience, current, incoming: incoming.clone(), progress, now, step, black });
        }
        self.frames.show(shown.clone(), repainted);
        shown
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // Readers of the picture let it go before its renderers stop.
        self.frames.show(None, true);
    }
}

/// A settled picture needs no more frames when nothing on it can change with
/// time (the cut keeps playing; the picture stays), or when its timeline has
/// reached a hold and the picture is the hold's.
fn picture_sleeps(time_independent: bool, on_hold: bool) -> bool {
    time_independent || on_hold
}

/// A slide's ambient clock keeps it drawing only in front of an audience.
fn ambient_runs(reads_live: bool, presenting: bool) -> bool {
    reads_live && presenting
}

#[cfg(test)]
mod picture_tests {
    use super::{ambient_runs, picture_sleeps};

    #[test]
    fn ambient_motion_keeps_drawing_only_while_presenting() {
        assert!(ambient_runs(true, true));
        assert!(!ambient_runs(true, false));
        assert!(!ambient_runs(false, true));
    }

    #[test]
    fn a_picture_sleeps_when_it_cannot_change_or_when_its_timeline_holds() {
        assert!(picture_sleeps(true, false), "a still picture over a looping cut");
        assert!(picture_sleeps(false, true), "a timeline at its hold");
        assert!(!picture_sleeps(false, false), "a moving picture on a running timeline keeps its frames");
    }
}

fn contains(frac: &[f64; 4], at: Vec2d) -> bool {
    at.x >= frac[0] && at.x < frac[0] + frac[2] && at.y >= frac[1] && at.y < frac[1] + frac[3]
}
