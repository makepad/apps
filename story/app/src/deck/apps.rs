//! DECK's live app layers: the children a deck's slides host.
//!
//! The process side is `makepad_app_host::Children`, the lifecycle every host
//! of child apps shares (the browser's pages run on it too): one hub, started
//! on the first slide that has an app; per `App{...}` of a slide one [`Slot`]
//! keyed by (slide, name), whose child is launched with `--stdin-loop`
//! (hidden, muted unless the slide says `audio: true`, its home a scratch
//! folder of this deck) and whose [`HostedApp`] speaks the protocol with it:
//! swapchain, bootstrap, presented frames, tick pacing, input, semantic
//! trees. What is DECK's stays here: only a slide current for a full
//! second may start apps; leaving cancels that debounce; already running
//! children survive the outgoing transition, then receive a protocol
//! shutdown. Posters only render cached frames or the same ready card. Apps
//! outside their AppView span park at 0 fps after setup. A missing release
//! binary builds in a background child, then starts with `--stdin-loop` as
//! it does in the WM. Its progress (build, start, wait) shows only inside
//! its own AppView frame (the status card) and in the presenter's status
//! line ([`Apps::notes`]), never over the slide.

use makepad_app_host::{children::expand_home, children::registry_entry, BuildPolicy, ChildEvent, ChildSpec, Children, Home};
pub use makepad_app_host::Life;
use makepad_story_edit_render::compose::ExternalFrame;
use makepad_story_deck::{AppDecl, DeckDoc};
use makepad_widgets::*;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

const START_DELAY: f64 = 1.0;
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);
/// A picture is settled once it has not changed for this long.
const SETTLE_SECONDS: f64 = 0.25;
/// A new render size is applied once the frame has kept it this long (a
/// window being resized, a transition) -- meanwhile the last frame shows,
/// scaled.
const RESIZE_SETTLE: f64 = 0.3;

/// One hosted app of one slide: its child (`makepad_app_host::Slot`, keyed
/// by slide and name) with DECK's own state, which reads as the slot's
/// fields.
pub type Slot = makepad_app_host::Slot<(usize, String), DeckSlot>;

/// What DECK keeps per hosted app.
pub struct DeckSlot {
    pub decl: AppDecl,
    /// The app's name as the slide shows it.
    pub display_name: String,
    /// Every fresh slot has a new generation: a demo only resumes against
    /// the child it began on, including while that child is still starting.
    pub generation: u64,
    pub semantics_enabled: bool,
    status_frame: Option<StatusFrame>,
    /// A pointer button is down in the app (moves are drags, not hovers).
    pub pressed: bool,
    /// Where the last press was sent and when (the demo cursor's ripple).
    pub pressed_at: Option<f64>,
    /// The mouse cursor the app asked for (shown while the pointer is over
    /// its frame).
    pub cursor: MouseCursor,
    /// Ready, but its previous child is still quitting (the launch waits).
    waits_for_quit: bool,
    /// The points-to-pixels factor the app renders with
    /// ([`AppDecl::render_dpi`] of the pixels its frame covers), and a new
    /// one waiting for the frame to settle (since when).
    pub dpi: f64,
    dpi_wanted: Option<(f64, f64)>,
    /// The present serial last seen, and when it changed.
    serial: u64,
    serial_at: f64,
    /// Whether a live picture is available; transitions invalidate the poster.
    has_live_frame: bool,
    /// Whether the first-frame line was logged, and the picture size last
    /// logged (a size change is logged once).
    logged: bool,
    logged_size: (u32, u32),
}

impl DeckSlot {
    /// The app's window size in points.
    pub fn size(&self) -> Vec2d {
        dvec2(self.decl.size.0, self.decl.size.1)
    }
}

/// What an app's slide holds while the app has no picture of its own: a
/// calm flat frame of the app's declared size, with no text. Its status (the
/// build, the launch, a failure) is the deck view's overlay, UI-sized, in
/// the presenter and editor view only: this frame is the one the output
/// window shows too, and the audience never sees cargo output. It is drawn
/// again only when the declared size changes.
struct StatusFrame {
    target: RenderTarget,
    size: (u32, u32),
    serial: u64,
}

/// Glyphs are being rasterised: text drawn now shows them only when drawn
/// again after they land. A picture drawn once (a card) waits for this.
pub(super) fn glyphs_pending(cx: &Cx) -> bool {
    cx.pending_async_loads().iter().any(|(kind, count)| *kind == "glyph" && *count > 0)
}

/// What a slide reads of a slot.
pub trait SlotView {
    /// The picture for the compositor, once the app has presented one.
    fn external(&self) -> Option<ExternalFrame>;
    /// The current app picture, or its cached launch/build status.
    fn frame(&mut self, cx: &mut Cx2d) -> (ExternalFrame, u64);
    /// The picture has not changed for a moment.
    fn settled(&self, now: f64) -> bool;
    /// The placeholder and presenter read the same state.
    fn note(&self) -> String;
    /// What the deck view says over the slide while the app has no picture
    /// of its own: None once it has one (or has nothing to say).
    fn overlay(&self) -> Option<StatusOverlay>;
}

impl SlotView for Slot {
    fn external(&self) -> Option<ExternalFrame> {
        let presented = self.hosted.presented()?;
        let size = ((presented.tex_scale[0] * presented.tex_size[0]).round() as u32, (presented.tex_scale[1] * presented.tex_size[1]).round() as u32);
        Some(ExternalFrame { texture: presented.texture.clone(), size, scale: presented.tex_scale })
    }

    fn frame(&mut self, cx: &mut Cx2d) -> (ExternalFrame, u64) {
        if matches!(self.life, Life::Starting | Life::Live) {
            if let Some(frame) = self.external() {
                self.status_frame = None;
                return (frame, self.hosted.present_serial().wrapping_mul(2));
            }
        }
        let size = self.card_size();
        self.status_frame.get_or_insert_with(|| StatusFrame::new(cx)).frame(cx, size)
    }

    fn settled(&self, now: f64) -> bool {
        self.hosted.has_frame() && now - self.serial_at >= SETTLE_SECONDS
    }

    fn overlay(&self) -> Option<StatusOverlay> {
        if matches!(self.life, Life::Starting | Life::Live) && self.external().is_some() { return None; }
        let note = self.note();
        if note.is_empty() { return None; }
        Some(StatusOverlay::new(&self.display_name, &note, matches!(self.life, Life::Missing(_) | Life::Exited(_))))
    }

    fn note(&self) -> String {
        match &self.life {
            Life::Ready if self.waits_for_quit => format!("waiting for the previous {} to quit", self.display_name),
            Life::Ready => format!("ready to start {}", self.display_name),
            Life::Building => {
                let progress = self.launch.as_ref().map_or("", |launch| launch.progress());
                if progress.is_empty() { format!("compiling {}…", self.display_name) } else { progress.to_string() }
            }
            Life::Missing(why) => why.lines().find(|line| line.starts_with("error:") || line.starts_with("error["))
                .or_else(|| why.lines().next()).unwrap_or("launch failed").to_string(),
            Life::Exited(why) => format!("ended: {why}"),
            Life::Starting | Life::Live if !self.hosted.has_frame() => self.hosted.status()
                .map(|status| status.label()).unwrap_or_else(|| format!("starting {}…", self.display_name)),
            _ => String::new(),
        }
    }
}

impl DeckSlot {
    /// The status frame's size while the app has no picture.
    fn card_size(&self) -> (u32, u32) {
        (self.decl.size.0 as u32, self.decl.size.1 as u32)
    }
}

/// An app's status over its slide, as the deck view draws it: UI-sized text
/// (the view's own, never the app's render size or dpi), a thin bar when the
/// build reports crates done of total, red when it failed.
#[derive(Clone, Debug, PartialEq)]
pub struct StatusOverlay {
    pub title: String,
    pub note: String,
    /// Crates done of total, 0..1.
    pub progress: Option<f32>,
    pub error: bool,
}

impl StatusOverlay {
    fn new(title: &str, note: &str, error: bool) -> Self {
        Self { title: title.to_string(), note: note.to_string(), progress: progress_of(note), error }
    }
}

/// "compiling 130/132 crates · makepad-scope…" -> 130 of 132.
pub fn progress_of(note: &str) -> Option<f32> {
    for word in note.split_whitespace() {
        if let Some((done, total)) = word.split_once('/') {
            if let (Ok(done), Ok(total)) = (done.parse::<f32>(), total.parse::<f32>()) {
                if total > 0.0 && done <= total { return Some(done / total); }
            }
        }
    }
    None
}

impl StatusFrame {
    /// Drawing the frame at `size` now would change it.
    fn stale(&self, size: (u32, u32)) -> bool {
        self.size != size
    }

    /// The serial `frame` hands out for the frame as it is.
    fn tag(&self) -> u64 {
        self.serial.wrapping_mul(2).wrapping_add(1)
    }

    fn new(cx: &mut Cx2d) -> Self {
        Self { target: RenderTarget::new(cx, "deck_app_status"), size: (0, 0), serial: 0 }
    }

    fn frame(&mut self, cx: &mut Cx2d, size: (u32, u32)) -> (ExternalFrame, u64) {
        if self.size != size || self.serial == 0 {
            if self.target.ensure(cx, size, RenderTargetFormat::Bgra8) {
                self.target.texture.set_transient(cx, false);
            }
            self.target.begin(cx, vec4f(0.04, 0.05, 0.07, 1.0));
            self.target.end(cx);
            self.size = size;
            self.serial = self.serial.wrapping_add(1);
        }
        cx.make_child_pass(&self.target.pass);
        (ExternalFrame { texture: self.target.texture.clone(), size, scale: [1.0, 1.0] }, self.serial.wrapping_mul(2).wrapping_add(1))
    }
}


pub struct Apps {
    children: Children<(usize, String), DeckSlot>,
    slide_names: Vec<String>,
    current: Option<(usize, f64)>,
    placeholders: HashMap<(usize, String), StatusFrame>,
    generations: HashMap<(usize, String), u64>,
    /// Current apps that run at full rate until their own setup is done.
    warm: HashSet<(usize, String)>,
    /// The app holding the keys and pointer: its slide and name.
    focus: Option<(usize, String)>,
    /// The app under the pointer (its cursor shows).
    hover: Option<(usize, String)>,
    /// Status changes refresh the current app frame and its placeholder.
    status_changes: HashSet<usize>,
    /// An app's render size or dpi changed: the next beat tells its window.
    geometry_stale: bool,
}

impl Apps {
    pub fn new(cx: &mut Cx, doc: &DeckDoc) -> Apps {
        let slug: String = doc.title.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).take(40).collect();
        let session = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
        let scratch = std::env::temp_dir().join(format!("stage-deck-{}", std::process::id())).join(format!("{slug}-{session}"));
        Apps {
            children: Children::new(cx, "deck app", scratch),
            slide_names: doc.slides.iter().map(|slide| slide.name.clone()).collect(),
            current: None,
            placeholders: HashMap::new(),
            generations: HashMap::new(),
            warm: HashSet::new(),
            focus: None,
            hover: None,
            status_changes: HashSet::new(),
            geometry_stale: false,
        }
    }

    /// The pixels the app's frame covers on the largest picture showing it:
    /// its render size follows, at once before its first picture, else once
    /// the frame has kept the new size for a moment. An app whose frame is
    /// not on the picture now keeps its size (it comes back unchanged).
    pub fn set_frame_pixels(&mut self, slide: usize, name: &str, frame_px: (u32, u32)) {
        let frame_px = Some(frame_px);
        let now = Cx::monotonic_now();
        let Some(slot) = self.slot_mut(slide, name) else { return };
        let dpi = slot.decl.render_dpi(frame_px);
        if dpi == slot.dpi {
            slot.dpi_wanted = None;
            return;
        }
        let since = match slot.dpi_wanted {
            Some((wanted, since)) if wanted == dpi => since,
            _ => now,
        };
        if !slot.hosted.has_frame() || now - since >= RESIZE_SETTLE {
            log!("deck app {}: renders at dpi {dpi} ({}x{} px for a {:?} px frame)", slot.decl.name,
                (slot.decl.size.0 * dpi).ceil(), (slot.decl.size.1 * dpi).ceil(), frame_px);
            slot.dpi = dpi;
            slot.dpi_wanted = None;
            self.geometry_stale = true;
        } else {
            slot.dpi_wanted = Some((dpi, since));
        }
    }

    /// Hold an app at full rate (its setup is running), or
    /// let it be throttled again at the next reconcile.
    pub fn set_warm(&mut self, slide: usize, name: &str, warm: bool) {
        if warm {
            self.warm.insert((slide, name.to_string()));
        } else {
            self.warm.remove(&(slide, name.to_string()));
        }
    }

    pub fn slot(&self, slide: usize, name: &str) -> Option<&Slot> {
        self.children.slot(&(slide, name.to_string()))
    }

    pub fn slot_mut(&mut self, slide: usize, name: &str) -> Option<&mut Slot> {
        self.children.slot_mut(&(slide, name.to_string()))
    }

    /// The status the deck view says over this slide's picture: the first of
    /// its apps that has no picture of its own yet.
    pub fn overlay(&self, slide: usize) -> Option<StatusOverlay> {
        self.slots_of(slide).find_map(SlotView::overlay)
    }

    pub fn slots_of(&self, slide: usize) -> impl Iterator<Item = &Slot> {
        self.children.slots().filter(move |slot| slot.key.0 == slide)
    }

    pub fn take_status_changes(&mut self) -> impl Iterator<Item = usize> + '_ {
        self.status_changes.drain()
    }

    pub fn focus(&self) -> Option<&(usize, String)> {
        self.focus.as_ref()
    }

    /// The app of slide `current` that holds the keys, if one does: only a
    /// click on an app gives it the focus, and it keeps it until Escape or a
    /// click outside it. With none, every key (the arrows too) is the deck's.
    pub fn key_holder(&self, current: usize) -> Option<&str> {
        holder(self.focus.as_ref(), current)
    }

    pub fn set_focus(&mut self, focus: Option<(usize, String)>) {
        self.focus = focus;
    }

    /// The app under the pointer (None: none).
    pub fn set_hover(&mut self, hover: Option<(usize, String)>) {
        self.hover = hover;
    }

    /// The cursor for the pointer: the hovered app's, else the arrow.
    pub fn hover_cursor(&self) -> MouseCursor {
        self.hover.as_ref().and_then(|key| self.children.slot(key)).map_or(MouseCursor::Default, |slot| slot.cursor)
    }

    /// A segment ended during a drag: release the exact hosted pointer
    /// before parking the app, so it cannot retain a pressed control.
    pub fn release_input(&mut self, slide: usize, name: &str) {
        if let Some(slot) = self.slot_mut(slide, name) {
            if slot.pressed {
                slot.hosted.mouse_up(None, makepad_studio_protocol::MouseButton::PRIMARY, Cx::time_now(), &KeyModifiers::default(), true);
                slot.pressed = false;
            }
        }
    }

    /// The label a slot's child is logged and fenced under.
    fn label(&self, slide: usize, name: &str) -> String {
        format!("{}/{name}", self.slide_names.get(slide).map_or("?", String::as_str))
    }

    /// Only the current slide may create slots or begin a launch. Outgoing
    /// slots are retained until their transition ends, never started there.
    pub fn reconcile(&mut self, cx: &mut Cx, slides: &[(usize, &[AppDecl])], current: usize, retained: &[usize], shown: &[(usize, String)]) {
        let now = Cx::monotonic_now();
        if self.current.is_none_or(|(slide, _)| slide != current) {
            self.current = Some((current, now));
        }
        let gone: Vec<_> = self.children.slots().filter(|slot| {
            let (slide, name) = &slot.key;
            !retained.contains(slide)
                || *slide != current && slot.life == Life::Ready
                || !slides.iter().any(|(s, decls)| s == slide && decls.iter().any(|decl| decl.name == *name))
        }).map(|slot| slot.key.clone()).collect();
        for (slide, name) in gone { self.quit(cx, slide, &name); }
        for (slide, decls) in slides {
            for decl in *decls {
                if let Some(slot) = self.slot(*slide, &decl.name) {
                    // Geometry and automation are not process identity. A
                    // recording, resize or setup edit reuses an in-flight build.
                    let same_process = slot.decl.app == decl.app && slot.decl.args == decl.args
                        && slot.decl.env == decl.env && slot.decl.audio == decl.audio;
                    if !same_process { self.quit(cx, *slide, &decl.name); }
                }
                if let Some(slot) = self.slot_mut(*slide, &decl.name) {
                    if slot.decl != *decl {
                        let resized = slot.decl.size != decl.size;
                        slot.decl = decl.clone();
                        if resized {
                            self.status_changes.insert(*slide);
                            self.geometry_stale = true;
                        }
                    }
                } else if *slide == current {
                    self.prepare(*slide, decl);
                }
            }
        }
        let waits: Vec<bool> = self.children.slots().map(|slot| slot.life == Life::Ready && self.children.is_closing(&slot.label)).collect();
        for (slot, waits) in self.children.slots_mut().zip(waits) {
            if slot.waits_for_quit != waits {
                slot.waits_for_quit = waits;
                self.status_changes.insert(slot.key.0);
            }
        }
        if self.current.is_some_and(|(_, since)| now - since >= START_DELAY) {
            let ready: Vec<_> = self.children.slots().filter(|slot| slot.key.0 == current && slot.life == Life::Ready && !self.children.is_closing(&slot.label))
                .map(|slot| slot.key.1.clone()).collect();
            for name in ready { self.launch(cx, current, &name); }
        }
        for slot in self.children.slots_mut() {
            slot.hosted.set_throttled(cx, !shown.contains(&slot.key) && !self.warm.contains(&slot.key));
        }
        self.wake_for_deadlines(cx);
    }

    /// A poster can ask for a card, but cannot allocate a slot or launch.
    pub fn frame(&mut self, cx: &mut Cx2d, slide: usize, decl: &AppDecl) -> (ExternalFrame, u64) {
        if let Some(slot) = self.slot_mut(slide, &decl.name) { return slot.frame(cx); }
        let size = (decl.size.0 as u32, decl.size.1 as u32);
        self.placeholders.entry((slide, decl.name.clone())).or_insert_with(|| StatusFrame::new(cx)).frame(cx, size)
    }

    /// Whether `frame` would hand out another picture of this app than the
    /// one of `drawn` (the serial the slide last recorded): the app
    /// presented since, its status card has new text, or its render size is
    /// waiting to settle. While nothing of the kind is true, a held slide
    /// showing the app has nothing to record: a new present reaches the
    /// view through the apps' beat ([`Apps::handle_event`]).
    pub fn picture_changed(&self, slide: usize, decl: &AppDecl, drawn: Option<u64>) -> bool {
        let Some(slot) = self.slot(slide, &decl.name) else {
            let size = (decl.size.0 as u32, decl.size.1 as u32);
            return self.placeholders.get(&(slide, decl.name.clone())).is_none_or(|card| card.stale(size) || drawn != Some(card.tag()));
        };
        if slot.dpi_wanted.is_some() {
            return true;
        }
        if matches!(slot.life, Life::Starting | Life::Live) && slot.hosted.presented().is_some() {
            return drawn != Some(slot.hosted.present_serial().wrapping_mul(2));
        }
        let size = slot.card_size();
        slot.status_frame.as_ref().is_none_or(|card| card.stale(size) || drawn != Some(card.tag()))
    }

    /// Some app's setup steps are running (it is held at full rate).
    pub fn setting_up(&self) -> bool {
        !self.warm.is_empty()
    }

    pub fn retain_placeholders(&mut self, slides: &[usize]) {
        self.placeholders.retain(|(slide, _), _| slides.contains(slide));
    }

    /// Quit the app and launch it again with an empty home (the demo
    /// restarts from a clean state).
    pub fn relaunch(&mut self, cx: &mut Cx, slide: usize, name: &str) {
        if self.current.is_none_or(|(current, _)| current != slide) { return; }
        let Some(slot) = self.slot(slide, name) else { return };
        if matches!(slot.life, Life::Ready | Life::Building | Life::Starting) || slot.life == Life::Live && !slot.hosted.has_frame() { return; }
        let decl = slot.decl.clone();
        self.quit(cx, slide, name);
        self.prepare(slide, &decl);
    }

    fn prepare(&mut self, slide: usize, decl: &AppDecl) {
        if self.slot(slide, &decl.name).is_some() { return; }
        self.placeholders.remove(&(slide, decl.name.clone()));
        let generation = {
            let count = self.generations.entry((slide, decl.name.clone())).or_insert(0);
            *count += 1;
            *count
        };
        let mut spec = ChildSpec::new(decl.app.clone());
        spec.args = decl.args.iter().map(|arg| expand_home(arg)).collect();
        spec.env = decl.env.iter().map(|(key, value)| (key.clone(), value.clone())).collect();
        spec.audio = decl.audio;
        spec.home = Home::Scratch;
        spec.policy = BuildPolicy::IfMissing;
        let label = self.label(slide, &decl.name);
        let display_name = makepad_app_registry::find(&decl.app).or_else(|| makepad_app_registry::find_by_bin(&decl.app))
            .map_or_else(|| decl.app.clone(), |entry| entry.label.to_string());
        self.children.prepare((slide, decl.name.clone()), spec, label, DeckSlot {
            decl: decl.clone(),
            display_name,
            generation,
            semantics_enabled: false,
            status_frame: None,
            pressed: false,
            pressed_at: None,
            cursor: MouseCursor::Default,
            waits_for_quit: false,
            dpi: decl.render_dpi(None),
            dpi_wanted: None,
            serial: 0,
            serial_at: Cx::monotonic_now(),
            has_live_frame: false,
            logged: false,
            logged_size: (0, 0),
        });
        self.status_changes.insert(slide);
    }

    fn launch(&mut self, cx: &mut Cx, slide: usize, name: &str) {
        let key = (slide, name.to_string());
        // Only apps the WM could host are hosted on a slide.
        let app = self.children.slot(&key).map(|slot| slot.spec.app.clone()).unwrap_or_default();
        if registry_entry(&app).filter(|entry| entry.wm_launchable).is_none() {
            if let Some(slot) = self.children.slot_mut(&key) {
                log!("deck app {name}: not launched: unknown hosted app: {app}");
                slot.life = Life::Missing(format!("unknown hosted app: {app}"));
            }
            self.status_changes.insert(slide);
            return;
        }
        self.children.launch(cx, &key);
        self.status_changes.insert(slide);
    }

    /// Drop its frames immediately; ask the child to run its shutdown handler.
    pub fn quit(&mut self, cx: &mut Cx, slide: usize, name: &str) {
        let key = (slide, name.to_string());
        if self.children.slot(&key).is_none() { return; }
        self.warm.remove(&key);
        self.status_changes.insert(slide);
        if self.focus.as_ref() == Some(&key) { self.focus = None; }
        if self.hover.as_ref() == Some(&key) { self.hover = None; }
        self.children.quit(cx, &key);
    }

    /// Preserve the process identity across document edits and slide reorders.
    pub fn remap(&mut self, cx: &mut Cx, before: &DeckDoc, after: &DeckDoc) {
        let index = |old: usize| before.slides.get(old).and_then(|slide| after.slides.iter().position(|new| new.name == slide.name));
        let removed: Vec<_> = self.children.slots().filter(|slot| index(slot.key.0).is_none()).map(|slot| slot.key.clone()).collect();
        for (slide, name) in removed { self.quit(cx, slide, &name); }
        for slot in self.children.slots_mut() { slot.key.0 = index(slot.key.0).unwrap(); }
        self.slide_names = after.slides.iter().map(|slide| slide.name.clone()).collect();
        self.current = self.current.and_then(|(slide, since)| index(slide).map(|slide| (slide, since)));
        self.focus = None;
        self.hover = None;
        self.warm.clear();
        self.generations = std::mem::take(&mut self.generations).into_iter().filter_map(|((slide, name), count)| Some(((index(slide)?, name), count))).collect();
        self.placeholders.clear();
        self.status_changes = self.children.slots().map(|slot| slot.key.0).collect();
    }

    /// Quit every app (Stage shutdown, the page closing, a new document).
    pub fn shutdown(&mut self, cx: &mut Cx) {
        let all: Vec<_> = self.children.slots().map(|slot| slot.key.clone()).collect();
        for (slide, name) in all { self.quit(cx, slide, &name); }
        self.current = None;
        self.placeholders.clear();
        self.children.shutdown(cx);
    }

    /// Every event: the hosted apps' own beat, and the hub's messages when
    /// it is woken (its signal, or the beat). True when the loop beat.
    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event) -> bool {
        let now = Cx::monotonic_now();
        // A finished build turns into its app only on a slide that has settled.
        let current = self.current;
        for slot in self.children.slots_mut() {
            slot.hold_build = !current.is_some_and(|(current, since)| current == slot.key.0 && now - since >= START_DELAY);
        }
        let beat = self.children.handle_event(cx, event);
        for event in self.children.take_events() {
            match event {
                ChildEvent::Changed((slide, _)) => {
                    self.status_changes.insert(slide);
                }
                // The app's cursor shows at once when the pointer is over it.
                ChildEvent::Message { key, msg: makepad_studio_protocol::AppToStudio::SetCursor(cursor) } => {
                    let hovered = self.hover.as_ref() == Some(&key);
                    if let Some(slot) = self.children.slot_mut(&key) {
                        slot.cursor = cursor.into();
                        if hovered { cx.set_cursor(slot.cursor); }
                    }
                }
                _ => {}
            }
        }
        if beat {
            self.after_beat(cx, now);
            self.children.flush();
            self.wake_for_deadlines(cx);
        }
        beat
    }

    /// The loop sleeps between events: the moments this layer acts on by
    /// the clock (a slide settling, a held build released, a resize or a
    /// picture settling, the first-frame report) wake it once each. A
    /// changed render size wakes it now, to tell the app's window.
    fn wake_for_deadlines(&mut self, cx: &mut Cx) {
        if std::mem::take(&mut self.geometry_stale) {
            SignalToUI::set_ui_signal();
        }
        let now = Cx::monotonic_now();
        let mut due = f64::INFINITY;
        let mut consider = |at: f64| if at > now { due = due.min(at) };
        for slot in self.children.slots() {
            if let Some((current, since)) = self.current {
                if current == slot.key.0 && matches!(slot.life, Life::Ready | Life::Building) {
                    consider(since + START_DELAY);
                }
            }
            if let Some((_, since)) = slot.dpi_wanted {
                consider(since + RESIZE_SETTLE);
            }
            if matches!(slot.life, Life::Starting | Life::Live) {
                if slot.hosted.has_frame() {
                    consider(slot.serial_at + SETTLE_SECONDS);
                }
                if !slot.logged {
                    consider(slot.born + 5.0);
                }
            }
        }
        if due.is_finite() {
            self.children.wake_at(cx, due);
        }
    }

    /// Per app after the hub was pumped: its size, its logged first frame,
    /// when its picture last changed.
    fn after_beat(&mut self, cx: &mut Cx, now: f64) {
        for slot in self.children.slots_mut() {
            if matches!(slot.life, Life::Starting | Life::Live) {
                let size = slot.size();
                let dpi = slot.dpi;
                slot.hosted.set_geometry(cx, Rect { pos: dvec2(0.0, 0.0), size }, dpi);
            }
            // Once per app: whether its picture ever arrives, and as what.
            if !slot.logged && matches!(slot.life, Life::Live | Life::Starting) {
                if let Some(presented) = slot.hosted.presented() {
                    let size = ((presented.tex_scale[0] * presented.tex_size[0]).round() as u32, (presented.tex_scale[1] * presented.tex_size[1]).round() as u32);
                    slot.logged = true;
                    log!("deck app {}: first frame {}x{}", slot.decl.name, size.0, size.1);
                } else if now - slot.born > 5.0 {
                    slot.logged = true;
                    log!("deck app {}: no frame after 5 s (has_frame {}, life {:?})", slot.decl.name, slot.hosted.has_frame(), matches!(slot.life, Life::Live));
                }
            }
            if slot.logged {
                if let Some(presented) = slot.hosted.presented() {
                    let size = ((presented.tex_scale[0] * presented.tex_size[0]).round() as u32, (presented.tex_scale[1] * presented.tex_size[1]).round() as u32);
                    if size != slot.logged_size {
                        slot.logged_size = size;
                        log!("deck app {}: picture {}x{}", slot.decl.name, size.0, size.1);
                    }
                }
            }
            let serial = slot.hosted.present_serial();
            if serial != slot.serial {
                slot.serial = serial;
                slot.serial_at = now;
            }
            let live = matches!(slot.life, Life::Starting | Life::Live) && slot.hosted.has_frame();
            if live != slot.has_live_frame {
                slot.has_live_frame = live;
                self.status_changes.insert(slot.key.0);
            }
        }
    }

    /// Send what the hosted apps queued.
    pub fn flush(&mut self) {
        self.children.flush();
    }

    /// The oldest-first list of the slide's apps' notes, for the
    /// presenter's status line beside the picture.
    pub fn notes(&self, slide: usize) -> Vec<String> {
        self.slots_of(slide).map(|slot| slot.note()).filter(|note| !note.is_empty()).collect()
    }
}

fn holder(focus: Option<&(usize, String)>, current: usize) -> Option<&str> {
    focus.filter(|(slide, _)| *slide == current).map(|(_, name)| name.as_str())
}

#[cfg(test)]
mod key_tests {
    use super::holder;

    #[test]
    fn the_arrows_are_the_decks_unless_an_app_of_the_shown_slide_was_clicked() {
        assert_eq!(holder(None, 3), None, "no click: the deck walks the steps");
        let clicked = (3, "counter".to_string());
        assert_eq!(holder(Some(&clicked), 3), Some("counter"), "a clicked app keeps its keys");
        assert_eq!(holder(Some(&clicked), 4), None, "a focus left on another slide is not the shown slide's");
    }
}

#[cfg(test)]
mod overlay_tests {
    use super::*;

    #[test]
    fn a_build_note_gives_a_small_bar_and_a_failure_is_red_text() {
        let build = StatusOverlay::new("Scope", "compiling 130/132 crates · makepad-scope…", false);
        assert_eq!(build.progress, Some(130.0 / 132.0));
        assert!(!build.error);
        assert_eq!(progress_of("starting Scope…"), None);
        assert_eq!(progress_of("ready 5/0"), None);
        let failed = StatusOverlay::new("Scope", "error: could not compile", true);
        assert!(failed.error && failed.progress.is_none());
    }
}
