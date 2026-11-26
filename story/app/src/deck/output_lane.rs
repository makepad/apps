//! The output window's own render when it shows the deck in another
//! language ([`super::i18n`]: the slides write each text in the languages
//! they are shown in and pick by Motion's `lang()`). The player says each frame what the audience
//! sees (the shown slides, their clocks, live times, pointer and app
//! pictures, and the slide change) as an [`OutputRequest`]; the output
//! window's [`DeckOutputView`] renders it in its own draw, with its own
//! renderers (one per shown slide, its graphics loaded for the output's
//! language) and its own transition mixer, into its own
//! passes. Until a slide's render has a picture the view draws nothing and
//! the presenter's picture (as written) shows beneath it.

use super::player::Pointer;
use super::render::SlideRenderer;
use super::super::cut::store::LazyStoreMedia;
use super::view::DrawDeckPicture;
use makepad_story_deck::{DeckDoc, Incoming, SlideCut};
use makepad_story_edit_render::compose::{ExternalFrame, Painted};
use makepad_story_edit_render::{CutJob, PreviewStatus, SlideMixer};
use makepad_widgets::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    mod.widgets.DeckOutputViewBase = #(DeckOutputView::register_widget(vm))
    mod.widgets.DeckOutputView = set_type_default() do mod.widgets.DeckOutputViewBase{
        width: Fill height: Fill
        // Transparent: only gives the view an area for the pointer.
        draw_hit +: {color: #0000}
    }
}

/// One shown slide as the audience sees it this frame.
pub struct SlideDrive {
    pub index: usize,
    pub t: f64,
    pub live: f64,
    pub pointer: Option<[f64; 2]>,
    /// The pictures of its apps, cameras and code views, and whether one
    /// changed since the output last drew.
    pub externals: Option<(HashMap<String, ExternalFrame>, bool)>,
}

/// What the output shows now.
pub struct OutputRequest {
    pub doc: Rc<DeckDoc>,
    pub slides: Vec<SlideDrive>,
    pub current: usize,
    pub incoming: Option<Incoming>,
    pub progress: f64,
    pub now: f64,
    pub step: f64,
    pub black: bool,
}

/// The output's language, the newest request, and the renderers: shared by the
/// player (which writes requests) and the output window's view (which draws).
#[derive(Default)]
pub struct OutputShared {
    lane: OutputLane,
    request: Option<OutputRequest>,
    /// Bumped with every request (the view redraws when it moved).
    pub serial: u64,
    drawn_for: Option<usize>,
    /// The deck's output size (the picture's aspect), from the player.
    output_size: (u32, u32),
    /// Pointer events on the output window's picture (fractions of the
    /// picture), for the player's hosted apps.
    pointer: Vec<(Pointer, Vec2d, f64, KeyModifiers)>,
    /// The output window's picture in pixels, and when it was last drawn.
    shown_px: Option<((f64, f64), f64)>,
}

pub type SharedOutput = Rc<RefCell<OutputShared>>;

thread_local! {
    /// Stage has one output window and one DECK: every player (a view may
    /// be rebuilt and its player replaced) writes the same one.
    static SHARED: SharedOutput = Default::default();
}

/// The output's render in another language, for the players and the
/// output window.
pub fn shared() -> SharedOutput {
    SHARED.with(Rc::clone)
}

impl OutputShared {
    pub fn set_lang(&mut self, lang: Option<Arc<str>>) {
        trace!("stage.deck", "deck output: language {}", lang.as_deref().unwrap_or("as written"));
        if lang.is_none() {
            self.request = None;
        }
        self.lane.set_lang(lang);
        self.serial = self.serial.wrapping_add(1);
    }

    pub fn active(&self) -> bool {
        self.lane.active()
    }

    /// The output window's picture in pixels while it is being drawn
    /// (None: no output window has drawn in the last second).
    pub fn shown_pixels(&self, now: f64) -> Option<(f64, f64)> {
        self.shown_px.filter(|(_, at)| now - at < 1.0).map(|(px, _)| px)
    }

    pub fn set_output_size(&mut self, size: (u32, u32)) {
        self.output_size = size;
    }

    /// The pointer events the output window queued since the last take.
    pub fn take_pointer(&mut self) -> Vec<(Pointer, Vec2d, f64, KeyModifiers)> {
        std::mem::take(&mut self.pointer)
    }

    pub fn busy(&self) -> bool {
        self.lane.busy()
    }

    /// The audience's frame. An app picture that changed and was not drawn
    /// yet stays marked changed.
    pub fn request(&mut self, mut request: OutputRequest) {
        if let Some(old) = &self.request {
            for slide in &mut request.slides {
                let was = old.slides.iter().find(|o| o.index == slide.index).and_then(|o| o.externals.as_ref()).is_some_and(|(_, changed)| *changed);
                if let Some((_, changed)) = &mut slide.externals {
                    *changed |= was;
                }
            }
        }
        if self.request.as_ref().map(|r| r.current) != Some(request.current) {
            log!("deck output: request slide {}", request.current);
        }
        self.request = Some(request);
        self.serial = self.serial.wrapping_add(1);
    }

    /// Render the request (in the output window's draw). The picture and
    /// the deck's output size; `None`: nothing of its own to show yet.
    fn draw(&mut self, cx: &mut Cx2d) -> Option<(Texture, (u32, u32))> {
        if !self.lane.active() {
            return None;
        }
        let request = self.request.as_mut()?;
        if self.drawn_for != Some(request.current) {
            log!("deck output: the output window draws slide {}", request.current);
            self.drawn_for = Some(request.current);
        }
        let shown: Vec<usize> = request.slides.iter().map(|slide| slide.index).collect();
        self.lane.retain(&shown);
        for slide in &mut request.slides {
            if request.doc.slides.get(slide.index).is_none() { continue };
            let externals = slide.externals.as_mut().map(|(frames, changed)| {
                let out = (frames.clone(), *changed);
                *changed = false;
                out
            });
            self.lane.drive(cx, &request.doc, slide.index, slide.t, slide.live, slide.pointer, externals);
        }
        let (picture, _) = self.lane.picture(cx, request.current, request.incoming.as_ref(), request.progress, request.doc.output, request.now, request.step, None);
        if request.black {
            return None;
        }
        picture.map(|picture| (picture, request.doc.output))
    }
}

/// The output window's picture of the deck in another language: drawn
/// over the window's frame view, from this widget's own pass.
#[derive(Script, ScriptHook, Widget)]
pub struct DeckOutputView {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_picture: DrawDeckPicture,
    #[live]
    draw_hit: DrawColor,
    #[rust]
    shared: Option<SharedOutput>,
    /// Where the picture sits in the window (letterboxed at the deck's
    /// aspect, as the frame view under this one fits it).
    #[rust]
    picture: Rect,
    #[rust]
    seen: u64,
    #[rust]
    next_frame: NextFrame,
}

impl DeckOutputView {
    pub fn set_shared(&mut self, cx: &mut Cx, shared: Option<SharedOutput>) {
        self.shared = shared;
        self.redraw(cx);
    }
}

impl Widget for DeckOutputView {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        let Some(shared) = &self.shared else { return };
        shared.borrow_mut().lane.handle_event(cx, event);
        // The pointer on the audience's picture reaches the slide's apps
        // as it does on the presenter's (the player hit-tests it).
        let pointer = match event.hits(cx, self.draw_hit.area()) {
            Hit::FingerDown(fe) if fe.is_primary_hit() => Some((Pointer::Down, fe.abs, fe.time, fe.modifiers)),
            Hit::FingerMove(fe) => Some((Pointer::Move, fe.abs, fe.time, fe.modifiers)),
            Hit::FingerHoverIn(fe) | Hit::FingerHoverOver(fe) => Some((Pointer::Move, fe.abs, fe.time, fe.modifiers)),
            Hit::FingerUp(fe) if fe.is_primary_hit() => Some((Pointer::Up, fe.abs, fe.time, fe.modifiers)),
            Hit::FingerScroll(fe) => Some((Pointer::Scroll(fe.scroll), fe.abs, fe.time, fe.modifiers)),
            _ => None,
        };
        let picture = self.picture;
        if let Some((kind, abs, time, modifiers)) = pointer.filter(|_| picture.size.x > 0.0 && picture.size.y > 0.0) {
            let at = dvec2((abs.x - picture.pos.x) / picture.size.x, (abs.y - picture.pos.y) / picture.size.y);
            shared.borrow_mut().pointer.push((kind, at, time, modifiers));
        }
        let serial = shared.borrow().serial;
        if serial != self.seen || self.next_frame.is_event(event).is_some() {
            self.redraw(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        let Some(shared) = self.shared.clone() else { return DrawStep::done() };
        let mut shared = shared.borrow_mut();
        self.seen = shared.serial;
        self.draw_hit.draw_abs(cx, rect);
        let (ow, oh) = (shared.output_size.0.max(1) as f64, shared.output_size.1.max(1) as f64);
        let scale = (rect.size.x / ow).min(rect.size.y / oh);
        let size = dvec2(ow * scale, oh * scale);
        self.picture = Rect { pos: rect.pos + (rect.size - size) * 0.5, size };
        let dpi = cx.current_dpi_factor();
        shared.shown_px = Some(((size.x * dpi, size.y * dpi), Cx::monotonic_now()));
        if let Some((texture, (ow, oh))) = shared.draw(cx) {
            let (ow, oh) = (ow.max(1) as f64, oh.max(1) as f64);
            let scale = (rect.size.x / ow).min(rect.size.y / oh);
            let size = dvec2((ow * scale).floor(), (oh * scale).floor());
            let origin = dvec2((rect.pos.x + (rect.size.x - size.x) * 0.5).round(), (rect.pos.y + (rect.size.y - size.y) * 0.5).round());
            self.draw_picture.draw_vars.set_texture(0, &texture);
            self.draw_picture.opacity = 1.0;
            self.draw_picture.draw_abs(cx, Rect { pos: origin, size });
        }
        if shared.active() && shared.busy() {
            self.next_frame = cx.new_next_frame();
        }
        DrawStep::done()
    }
}

pub struct OutputLane {
    lang: Option<Arc<str>>,
    /// Bumped with every change of the output's language.
    revision: u64,
    /// Slide -> its renderer, the language revision it draws with, and the
    /// source of the cut it renders (another source: a new renderer).
    sources: HashMap<usize, (SlideRenderer, u64, u64)>,
    /// A slide's renderer in a newer language, until it has a picture.
    next: HashMap<usize, (SlideRenderer, u64, u64)>,
    mixer: Option<SlideMixer>,
    mix_recorded: bool,
    mix_draws: u32,
    shown_mix: Option<Texture>,
    last: Option<(usize, u64)>,
    /// Slide -> the deck document and language revision its source key was
    /// read for, and the key.
    keys: HashMap<usize, (Rc<DeckDoc>, u64, u64)>,
}

impl Default for OutputLane {
    fn default() -> Self {
        Self::new()
    }
}

impl OutputLane {
    pub fn new() -> Self {
        Self { lang: None, revision: 0, sources: HashMap::new(), next: HashMap::new(), mixer: None, mix_recorded: false, mix_draws: 0, shown_mix: None, last: None, keys: HashMap::new() }
    }

    /// The output's language (`None`: the output shows the presenter's picture).
    pub fn set_lang(&mut self, lang: Option<Arc<str>>) {
        if lang == self.lang {
            return;
        }
        if lang.is_none() {
            self.sources.clear();
            self.next.clear();
            self.keys.clear();
            self.shown_mix = None;
            self.mix_recorded = false;
        }
        self.lang = lang;
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        for (source, _, _) in self.sources.values_mut().chain(self.next.values_mut()) {
            source.handle_event(cx, event);
        }
        if let Some(mixer) = &mut self.mixer {
            mixer.handle_event(cx, event);
        }
    }

    pub fn active(&self) -> bool {
        self.lang.is_some()
    }

    /// Still loading or painting a slide.
    pub fn busy(&self) -> bool {
        self.mix_recorded || !self.next.is_empty() || self.sources.values().any(|(source, _, _)| !matches!(source.status(), PreviewStatus::Idle | PreviewStatus::Failed(_)))
    }

    /// Only the shown slides keep a renderer.
    pub fn retain(&mut self, shown: &[usize]) {
        self.sources.retain(|index, _| shown.contains(index));
        self.next.retain(|index, _| shown.contains(index));
        self.keys.retain(|index, _| shown.contains(index));
    }

    /// Draw slide `index` of `deck` as the presenter's renderer of it was
    /// just drawn.
    #[allow(clippy::too_many_arguments)]
    pub fn drive(&mut self, cx: &mut Cx2d, deck: &Rc<DeckDoc>, index: usize, t: f64, live: f64, pointer: Option<[f64; 2]>, externals: Option<(HashMap<String, ExternalFrame>, bool)>) {
        let Some(lang) = self.lang.clone() else { return };
        let Some(SlideCut::Inline(cut)) = deck.slides.get(index).map(|slide| &slide.cut) else { return };
        // What the slide renders, read once per deck document and language
        // (a frame of the same document reuses it: reading it hashes the
        // slide's whole source).
        let source_key = match self.keys.get(&index) {
            Some((seen, revision, key)) if Rc::ptr_eq(seen, deck) && *revision == self.revision => *key,
            _ => {
                let key = cut_key(cut);
                self.keys.insert(index, (deck.clone(), self.revision, key));
                key
            }
        };
        if self.sources.get(&index).is_some_and(|(_, _, key)| *key != source_key) {
            // The slide itself changed (an edit, another deck).
            self.sources.remove(&index);
            self.next.remove(&index);
        }
        let make = |cx: &mut Cx2d| {
            let job = CutJob { doc: Arc::new((**cut).clone()), marks: HashMap::new(), resolver: Box::new(LazyStoreMedia::new()), audio: false };
            let mut source = SlideRenderer::new(cx, job);
            source.set_lang(Some(lang.clone()));
            log!("deck output: slide {index} renders for the output in {lang}");
            source
        };
        match self.sources.get(&index) {
            None => {
                let source = make(cx);
                self.sources.insert(index, (source, self.revision, source_key));
            }
            // Another language: a renderer of its own, shown once it has a
            // picture; the one before shows until then.
            Some((_, revision, _)) if *revision != self.revision && self.next.get(&index).is_none_or(|(_, next, _)| *next != self.revision) => {
                let source = make(cx);
                self.next.insert(index, (source, self.revision, source_key));
            }
            Some(_) => {}
        }
        for lane in [&mut self.sources, &mut self.next] {
            let Some((source, _, _)) = lane.get_mut(&index) else { continue };
            if let Some((frames, changed)) = externals.clone() {
                source.set_externals(frames, changed);
            }
            source.seek(t);
            source.set_live(live);
            source.set_pointer(pointer);
            source.draw(cx);
        }
        if self.next.get(&index).is_some_and(|(source, _, _)| source.frame().is_some() && source.exact()) {
            let next = self.next.remove(&index).unwrap();
            self.sources.insert(index, next);
        }
    }

    /// The output's picture: the current slide (or the change into it) in
    /// the output's language, the presenter's `english` until it has one.
    /// The bool says the picture is new since the last call.
    pub fn picture(&mut self, cx: &mut Cx2d, current: usize, incoming: Option<&Incoming>, progress: f64, output: (u32, u32), now: f64, step: f64, english: Option<Texture>) -> (Option<Texture>, bool) {
        let frame = |index: usize| self.sources.get(&index).and_then(|(source, _, _)| source.frame()).map(|(_, texture)| texture);
        let key = self.sources.get(&current).filter(|(source, _, _)| source.frame().is_some()).map(|(source, _, _)| (current, source.serial()));
        let fresh = key != self.last;
        if self.last.is_some() != key.is_some() {
            let status = self.sources.get(&current).map(|(source, _, _)| source.status());
            log!("deck output: slide {current} shows {}", if key.is_some() { "its own render".to_string() } else { format!("the presenter's picture (own render {status:?})") });
        }
        self.last = key;
        let shown = match incoming {
            None => {
                self.mix_recorded = false;
                self.shown_mix = None;
                frame(current).or(english)
            }
            Some(incoming) => match (frame(incoming.from), frame(current)) {
                (Some(a), Some(b)) => {
                    let mixer = self.mixer.get_or_insert_with(|| SlideMixer::new(cx));
                    mixer.prepare(cx, &incoming.transition.effect);
                    if self.mix_recorded {
                        if mixer.painted(cx) == Painted::Whole {
                            self.shown_mix = mixer.output().cloned();
                            self.mix_recorded = false;
                        } else {
                            self.mix_draws += 1;
                            if self.mix_draws < super::player::PAINT_PATIENCE && !mixer.stale(cx) {
                                mixer.reattach(cx);
                            } else {
                                self.mix_recorded = false;
                            }
                        }
                    }
                    if !self.mix_recorded {
                        let dial = |name: &str| incoming.transition.dial(name).unwrap_or(-1.0);
                        let dials = [dial("p1"), dial("p2"), dial("p3")];
                        mixer.hold_output(self.shown_mix.clone());
                        mixer.mix(cx, &a, &b, &incoming.transition.effect, progress, dials, output, now - incoming.started, step);
                        self.mix_recorded = true;
                        self.mix_draws = 0;
                    }
                    Some(self.shown_mix.clone().unwrap_or(a))
                }
                (None, Some(b)) => Some(b),
                _ => english,
            },
        };
        (shown, fresh || incoming.is_some())
    }
}

/// What a slide's cut renders: its label and length, and each graphic with
/// the text of the document it draws (a slide written elsewhere, or edited,
/// has another key).
fn cut_key(doc: &makepad_story_cut::CutDoc) -> u64 {
    use makepad_story_cut::graphic::GraphicSrc;
    let mut key = format!("{}|{}|{}|{}", doc.label, doc.duration, doc.source.len(), doc.nodes.len());
    for graphic in &doc.graphics {
        key.push('|');
        key.push_str(&graphic.id);
        match &graphic.src {
            GraphicSrc::Tree(tree) => {
                key.push_str(&tree.document);
                if let Ok(text) = tree.resolve(&tree.document).and_then(|file| file.text().map(str::to_string)) {
                    key.push_str(&format!("#{:x}", LiveId::from_str(&text).0));
                }
            }
            other => key.push_str(&format!("{other:?}")),
        }
    }
    LiveId::from_str(&key).0
}
