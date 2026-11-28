//! A design owns a Splash only while producing its next still. A previous
//! whole picture can continue serving both canvas recordings in the meantime.
use super::{view::BoardDesign, BoardItem, BoardView};
use makepad_town_core::event::{Id, TownId};
use makepad_widgets::{gauss_view::CaptureGauss, makepad_platform::script::HostGrants, makepad_script::ScriptVmPolicy, *};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

pub(super) type DesignKey = (TownId, Id);
pub(super) type ThemeStamp = (u64, u64);
const DESIGN_PICTURES_MAX_BYTES: usize = 128 << 20;
const MAX_PICTURE_SIDE: usize = 4096;
/// How long one frame spends on a design's widget registration before handing back.
const REGISTER_SLICE_MS: f64 = 4.;

#[derive(Clone)]
struct RenderSpec {
    src: Arc<str>,
    src_hash: u64,
    size: (i64, i64),
    theme: ThemeStamp,
    /// Picture pixels per design point.
    density: f64,
}

impl RenderSpec {
    fn matches(&self, design: &BoardDesign, size: (i64, i64), theme: ThemeStamp) -> bool {
        self.size == size && self.theme == theme && self.src_hash == design.src_hash
            && (Arc::ptr_eq(&self.src, &design.src) || self.src == design.src)
    }

    /// The card's on-screen density rounded up to a power of two, so the picture is never
    /// softer than the screen and a zoom re-renders it once per doubling; capped so its longer
    /// side fits the GPU's largest texture.
    fn new(design: &BoardDesign, size: (i64, i64), theme: ThemeStamp, scale: f64, dpi: f64, max_side: usize) -> Self {
        let side = (size.0.max(size.1) as f64 / 64.).max(1.);
        let screen = 2f64.powf((scale * dpi).log2().ceil().max(-3.));
        let density = screen.min(max_side as f64 / side);
        Self { src: design.src.clone(), src_hash: design.src_hash, size, theme, density }
    }

    fn same_as(&self, other: &Self) -> bool {
        self.size == other.size && self.theme == other.theme && self.density == other.density
            && self.src_hash == other.src_hash && (Arc::ptr_eq(&self.src, &other.src) || self.src == other.src)
    }

    fn logical(&self) -> DVec2 {
        dvec2(self.size.0 as f64 / 64., self.size.1 as f64 / 64.)
    }

}

#[derive(Debug, PartialEq)]
pub(super) enum PictureState {
    Missing,
    Rendering,
    Ready { w: u32, h: u32 },
    Failed { message: String },
}

struct Picture {
    texture: Texture,
    size: (u32, u32),
    bytes: usize,
}

impl Picture {
    fn release(self, cx: &mut Cx) {
        cx.draw_lists.release_bindings(&[self.texture.texture_id()], &[]);
        self.texture.release(cx);
    }
}

// A board switch or an eviction can release many pictures. Visit the retained
// draw lists once for the entire batch, as the board media cache does.
fn release_pictures(cx: &mut Cx, pictures: Vec<Picture>) {
    if pictures.is_empty() { return }
    let ids: Vec<_> = pictures.iter().map(|p| p.texture.texture_id()).collect();
    cx.draw_lists.release_bindings(&ids, &[]);
    for picture in pictures { picture.texture.release(cx); }
}

struct Entry {
    wanted: RenderSpec,
    settled: Option<RenderSpec>,
    queued: bool,
    displayed: Option<Picture>,
    failure: Option<String>,
    last_draw: u64,
}

impl Entry {
    fn is_settled(&self) -> bool {
        self.settled.as_ref().is_some_and(|spec| spec.same_as(&self.wanted))
    }

    fn result(&self) -> (PictureState, Option<&Texture>) {
        if let Some(picture) = &self.displayed {
            let (w, h) = picture.size;
            (PictureState::Ready { w, h }, Some(&picture.texture))
        } else if let Some(message) = self.failure.as_ref().filter(|_| self.is_settled()) {
            (PictureState::Failed { message: message.clone() }, None)
        } else {
            (PictureState::Rendering, None)
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub(super) struct StageTimes {
    pub prepare_ms: f64,
    pub register_ms: f64,
    pub register_max_ms: f64,
    pub register_steps: u32,
    pub eval_ms: f64,
    /// Of `eval_ms`: running the source, and building the widget tree from its value.
    pub eval_source_ms: f64,
    pub eval_instantiate_ms: f64,
    pub fuel: u64,
    pub bytes: usize,
    pub draw_ms: f64,
    /// Drawings made, counting those redrawn while a glyph they asked for was in flight.
    pub draws: u32,
    pub teardown_ms: f64,
}

fn now_ms() -> f64 { makepad_platform::monotonic_seconds() * 1000. }

fn mip_bytes((mut w, mut h): (u32, u32)) -> usize {
    let mut bytes = 0;
    loop {
        bytes += w as usize * h as usize * 4;
        if w == 1 && h == 1 { return bytes }
        w = (w / 2).max(1);
        h = (h / 2).max(1);
    }
}

/// Each candidate lends exactly one slot. Dropping this recording retires it
/// even on cancellation before any GPU paint.
struct Recording {
    passes: PicturePasses,
    pass: DrawPassId,
    _overlay: Overlay,
    _gauss: CaptureGauss,
    picture: Picture,
    #[cfg(test)]
    draw_items: usize,
}

impl Recording {
    fn draw(cx: &mut Cx2d, splash: &WidgetRef, spec: &RenderSpec) -> Self {
        let mut passes = PicturePasses::default();
        let overlay = Overlay { draw_list: DrawList::new(cx) };
        let mut gauss = CaptureGauss::new(cx);
        let logical = spec.logical();
        let dpi = spec.density;
        let texture = passes.begin_scaled(cx, logical, dpi, "board design");
        let pass = cx.current_pass_id();
        let scope = overlay.begin_nested_for_pass(cx, pass);
        let rect = Rect { pos: DVec2::default(), size: cx.turtle().size() };
        gauss.begin(cx, pass, rect.pos, rect.size, rect.size);
        splash.draw_walk_all(cx, &mut Scope::empty(), Walk::fixed(logical.x, logical.y));
        if gauss.end(cx, pass, rect) { cx.repaint_pass_and_child_passes(pass); }
        #[cfg(test)]
        let draw_items = cx.draw_lists[cx.get_current_draw_list_id().unwrap()].draw_item_order_len();
        overlay.end_nested(cx, scope);
        passes.end(cx);
        let (w, h) = Cx::pass_pixel_size(rect.size, dpi);
        let size = (w.max(1) as u32, h.max(1) as u32);
        Self { passes, pass, _overlay: overlay, _gauss: gauss,
            picture: Picture { texture, size, bytes: mip_bytes(size) }, #[cfg(test)] draw_items }
    }

    fn finish(self, cx: &mut Cx) -> Picture {
        CaptureGauss::forget(cx, self.pass);
        let pass = &mut cx.passes[self.pass];
        pass.main_draw_list_id = None;
        pass.paint_dirty = false;
        pass.parent = CxDrawPassParent::None;
        pass.attached_by = None;
        pass.color_textures.clear();
        self.picture
    }
}

#[derive(Debug, PartialEq)]
enum Stage { Register, Eval, Draw, Paint }

struct Candidate {
    key: DesignKey,
    spec: RenderSpec,
    splash: WidgetRef,
    stage: Stage,
    recording: Option<Recording>,
    times: StageTimes,
}

impl Candidate {
    fn prepare(cx: &mut Cx, key: DesignKey, spec: RenderSpec) -> Self {
        let splash = cx.with_vm(|vm| {
            let value = script_eval!(vm, { use mod.widgets.* Splash{} });
            WidgetRef::script_from_value(vm, value)
        });
        let mut inner = splash.borrow_mut::<Splash>().expect("board design Splash");
        inner.set_grants(HostGrants::NONE);
        inner.set_host_prompts(cx, false);
        inner.set_debug_name("board design");
        inner.set_isolate_widgets(IsolateWidgets::Base);
        let start = now_ms();
        inner.prepare_isolate(cx);
        let times = StageTimes { prepare_ms: now_ms() - start, ..Default::default() };
        drop(inner);
        Self { key, spec, splash, stage: Stage::Register, recording: None, times }
    }

    /// The design's first error as its author wrote it: the line in the design's source and
    /// the message, else the reason the isolate stopped (a limit, which has no line).
    fn error(&self) -> Option<String> {
        let splash = self.splash.as_splash();
        if let Some(record) = splash.last_error_records().first() {
            let view = record.view();
            return Some(if view.has_loc { format!("line {}: {}", view.line, view.message) } else { view.message.to_owned() });
        }
        let errors = splash.last_errors();
        let line = errors.iter().flat_map(|error| error.lines()).map(str::trim)
            .find(|line| !line.is_empty() && *line != "this app stopped");
        line.map(str::to_owned).or_else(|| errors.first().cloned())
    }

    fn retire(&mut self, cx: &mut Cx) {
        let start = now_ms();
        if let Some(recording) = self.recording.take() { recording.finish(cx).release(cx); }
        self.splash.as_splash().retire(cx);
        self.times.teardown_ms = now_ms() - start;
    }
}

#[derive(Default)]
pub(super) struct DesignPictures {
    entries: HashMap<DesignKey, Entry>,
    queue: VecDeque<DesignKey>,
    candidate: Option<Candidate>,
    theme: Option<ThemeStamp>,
    last_generation: Option<u64>,
    displayed_bytes: usize,
    over_budget: bool,
    pub(super) renders: u64,
    pub(super) steps_this_frame: u32,
    pub(super) last_times: Option<StageTimes>,
    #[cfg(test)]
    pub(super) painted: Option<fn(&Cx, TextureId) -> bool>,
}

impl DesignPictures {
    pub(super) fn theme_changed(&mut self, cx: &mut Cx) -> bool {
        let theme = (cx.reload_generation, widget_async::splash_theme_generation());
        if self.theme == Some(theme) { return false }
        self.theme = Some(theme);
        self.suspend(cx);
        !self.entries.is_empty()
    }

    pub(super) fn suspend(&mut self, cx: &mut Cx) {
        for key in self.queue.drain(..) { self.entries.get_mut(&key).unwrap().queued = false; }
        self.cancel_candidate(cx);
    }

    fn cancel_candidate(&mut self, cx: &mut Cx) {
        if let Some(mut job) = self.candidate.take() { job.retire(cx); }
    }

    /// Called on fold changes, including section deletion and peer snapshots.
    /// Cancel stale work before a late paint can be examined. Visible requests
    /// replace its wanted spec on the ensuing board draw.
    pub(super) fn reconcile(&mut self, cx: &mut Cx, view: &BoardView) {
        let current = |key: &DesignKey, spec: &RenderSpec| {
            view.get(&key.1).is_some_and(|item| item.design.as_ref()
                .is_some_and(|d| spec.matches(d, (item.w, item.h), spec.theme)))
        };
        if self.candidate.as_ref().is_some_and(|job| !current(&job.key, &job.spec)) {
            self.cancel_candidate(cx);
        }
        self.queue.retain(|key| {
            let entry = self.entries.get_mut(key).unwrap();
            entry.queued = current(key, &entry.wanted);
            entry.queued
        });
        let mut released = Vec::new();
        self.entries.retain(|key, entry| {
            if view.get(&key.1).is_some_and(|i| i.w > 0 && i.h > 0 && i.design.as_ref().is_some_and(|d| !d.src.is_empty())) {
                return true;
            }
            if let Some(picture) = entry.displayed.take() {
                self.displayed_bytes -= picture.bytes;
                released.push(picture);
            }
            false
        });
        release_pictures(cx, released);
    }

    /// `upgrade`: the camera has settled, so a card shown denser than its picture re-renders.
    /// The picture it has keeps drawing until the new one has painted whole.
    pub(super) fn request(&mut self, cx: &mut Cx2d, key: DesignKey, item: &BoardItem, design: &BoardDesign,
        scale: f64, upgrade: bool) -> (PictureState, Option<&Texture>) {
        // reconcile removed any previous version when the source or size became empty.
        if design.src.is_empty() || item.w <= 0 || item.h <= 0 { return (PictureState::Ready { w: 0, h: 0 }, None) }
        let theme = self.theme.expect("design theme set before board recording");
        let size = (item.w, item.h);
        let max_side = cx.gpu_info().max_texture_size.clamp(1, MAX_PICTURE_SIDE);
        let current = self.entries.get(&key).filter(|entry| entry.wanted.matches(design, size, theme));
        // A script error does not depend on the density: a failed card keeps its message.
        if let Some(wanted) = current.filter(|entry| upgrade && entry.failure.is_none()).map(|entry| entry.wanted.density) {
            let spec = RenderSpec::new(design, size, theme, scale, cx.current_dpi_factor(), max_side);
            if spec.density > wanted {
                if self.candidate.as_ref().is_some_and(|job| job.key == key) { self.cancel_candidate(cx); }
                self.entries.get_mut(&key).unwrap().wanted = spec;
            }
        } else if current.is_none() {
            let spec = RenderSpec::new(design, size, theme, scale, cx.current_dpi_factor(), max_side);
            if self.candidate.as_ref().is_some_and(|job| job.key == key) { self.cancel_candidate(cx); }
            let entry = self.entries.entry(key).and_modify(|e| e.wanted = spec.clone())
                .or_insert(Entry { wanted: spec, settled: None, queued: false, displayed: None, failure: None, last_draw: 0 });
            if entry.queued && entry.is_settled() {
                self.queue.retain(|queued| queued != &key);
                entry.queued = false;
            }
        }
        let entry = self.entries.get_mut(&key).unwrap();
        entry.last_draw = cx.redraw_id();
        let active = self.candidate.as_ref().is_some_and(|job| job.key == key);
        if !active && !entry.queued && !entry.is_settled() {
            entry.queued = true;
            self.queue.push_back(key);
        }
        self.result(key)
    }

    fn result(&self, key: DesignKey) -> (PictureState, Option<&Texture>) {
        self.entries.get(&key).map_or((PictureState::Missing, None), Entry::result)
    }

    /// Returns true when a recording's displayed picture or diagnostic changed.
    pub(super) fn step(&mut self, cx: &mut Cx2d) -> bool {
        let generation = cx.redraw_id();
        if self.last_generation == Some(generation) { return false }
        self.last_generation = Some(generation);
        self.steps_this_frame = 0;
        let Some(mut job) = self.candidate.take() else {
            if let Some(key) = self.queue.pop_front() {
                let entry = self.entries.get_mut(&key).unwrap();
                entry.queued = false;
                if !entry.is_settled() {
                    self.steps_this_frame = 1;
                    self.candidate = Some(Candidate::prepare(cx, key, entry.wanted.clone()));
                }
            }
            return false;
        };
        let mut result = None;
        self.steps_this_frame = u32::from(job.stage != Stage::Paint);
        match job.stage {
            Stage::Register => {
                // Registration units are small: run them for a slice of this frame, so a warm
                // isolate registers in a frame or two rather than taking one frame per unit.
                let mut inner = job.splash.borrow_mut::<Splash>().unwrap();
                let slice = now_ms();
                loop {
                    let start = now_ms();
                    let more = inner.step_isolate_ui(cx);
                    let ms = now_ms() - start;
                    job.times.register_steps += 1;
                    job.times.register_ms += ms;
                    job.times.register_max_ms = job.times.register_max_ms.max(ms);
                    if !more {
                        match inner.set_vm_policy(cx, ScriptVmPolicy::untrusted()) {
                            Ok(()) => job.stage = Stage::Eval,
                            Err(error) => result = Some(Err(error.to_string())),
                        }
                        break;
                    }
                    if now_ms() - slice >= REGISTER_SLICE_MS { break }
                }
            }
            Stage::Eval => {
                let start = now_ms();
                job.splash.set_text(cx, &job.spec.src);
                job.times.eval_ms = now_ms() - start;
                let split = job.splash.as_splash().last_eval_times();
                (job.times.eval_source_ms, job.times.eval_instantiate_ms) = (split.source_ms, split.instantiate_ms);
                if let Some(policy) = job.splash.borrow::<Splash>().and_then(|s| s.vm_policy(cx)) {
                    let usage = policy.total();
                    job.times.fuel = usage.fuel;
                    job.times.bytes = usage.bytes;
                }
                if let Some(error) = job.error() { result = Some(Err(error)); }
                else { job.stage = Stage::Draw; }
            }
            Stage::Draw => {
                let start = now_ms();
                let recording = Recording::draw(cx, &job.splash, &job.spec);
                job.times.draw_ms = now_ms() - start;
                job.times.draws += 1;
                if let Some(error) = job.error() {
                    recording.finish(cx).release(cx);
                    result = Some(Err(error));
                } else if cx.pending_async_loads().is_empty() {
                    job.recording = Some(recording);
                    job.stage = Stage::Paint;
                } else {
                    // A glyph or other load this drawing asked for is still in flight, so its
                    // picture would keep a blank in its place: draw it again on a later frame.
                    recording.finish(cx).release(cx);
                }
            }
            Stage::Paint => {
                let recording = job.recording.as_mut().unwrap();
                // This candidate owns the only pass and its original texture until publication.
                let whole = recording.passes.painted(cx, &recording.picture.texture);
                #[cfg(test)]
                let whole = self.painted.map_or(whole, |painted| painted(cx, recording.picture.texture.texture_id()));
                if whole {
                    result = Some(Ok(job.recording.take().unwrap().finish(cx)));
                } else {
                    recording.passes.next_frame(cx);
                }
            }
        }
        let Some(result) = result else { self.candidate = Some(job); return false };
        let entry = self.entries.get_mut(&job.key).unwrap();
        if !entry.wanted.same_as(&job.spec) {
            if let Ok(stale) = result { stale.release(cx); }
            job.retire(cx);
            return false;
        }
        entry.settled = Some(job.spec.clone());
        entry.failure = None;
        if let Some(old) = entry.displayed.take() {
            self.displayed_bytes -= old.bytes;
            old.release(cx);
        }
        match result {
            Ok(picture) => {
                self.displayed_bytes += picture.bytes;
                entry.displayed = Some(picture);
            }
            Err(error) => entry.failure = Some(error),
        }
        job.retire(cx);
        self.renders += 1;
        if makepad_script::makepad_error_log::trace_enabled("town.design") {
            log!("board design prepare {:.3} ms, registration {:.3} ms (maximum {:.3} ms over {} calls), eval {:.3} ms (source {:.3} ms, instantiate {:.3} ms, {} fuel, {} bytes, {} source bytes), draw {:.3} ms (drawing {}), teardown {:.3} ms",
                job.times.prepare_ms, job.times.register_ms, job.times.register_max_ms, job.times.register_steps,
                job.times.eval_ms, job.times.eval_source_ms, job.times.eval_instantiate_ms, job.times.fuel, job.times.bytes, job.spec.src.len(), job.times.draw_ms, job.times.draws,
                job.times.teardown_ms);
        }
        self.last_times = Some(job.times);
        true
    }

    pub(super) fn busy(&self) -> bool { self.candidate.is_some() || !self.queue.is_empty() }

    pub(super) fn clear(&mut self, cx: &mut Cx) {
        self.cancel_candidate(cx);
        self.queue.clear();
        release_pictures(cx, self.entries.drain().filter_map(|(_, entry)| entry.displayed).collect());
        self.displayed_bytes = 0;
        self.over_budget = false;
    }

    pub(super) fn evict_to_budget(&mut self, cx: &mut Cx, keep: &[HashSet<DesignKey>; 2], visibility_changed: bool) {
        let pending = self.candidate.as_ref().and_then(|job| job.recording.as_ref()).map_or(0, |r| r.picture.bytes);
        let mut bytes = self.displayed_bytes + pending;
        if bytes > DESIGN_PICTURES_MAX_BYTES {
            // Remaining over budget means every unseen entry was evicted. Only
            // rerecording either layer can change which entries are now visible.
            if self.over_budget && !visibility_changed { return }
            let mut unseen: Vec<_> = self.entries.iter().filter(|(key, _)| !keep[0].contains(key) && !keep[1].contains(key))
                .map(|(key, e)| (e.last_draw, *key)).collect();
            unseen.sort_unstable();
            let mut released = Vec::new();
            for (_, key) in unseen {
                if self.candidate.as_ref().is_some_and(|job| job.key == key) {
                    self.cancel_candidate(cx);
                    bytes -= pending;
                }
                if let Some(picture) = self.entries.remove(&key).and_then(|e| e.displayed) {
                    self.displayed_bytes -= picture.bytes;
                    bytes -= picture.bytes;
                    released.push(picture);
                }
                if bytes <= DESIGN_PICTURES_MAX_BYTES { break }
            }
            self.queue.retain(|key| self.entries.contains_key(key));
            release_pictures(cx, released);
        }
        if bytes > DESIGN_PICTURES_MAX_BYTES && !self.over_budget {
            log!("Visible board design pictures exceed the cache budget ({bytes} bytes)");
        }
        self.over_budget = bytes > DESIGN_PICTURES_MAX_BYTES;
    }

    #[cfg(test)]
    pub(super) fn state(&self, key: DesignKey) -> PictureState {
        self.result(key).0
    }
    #[cfg(test)]
    pub(super) fn pending_paint(&self) -> Option<(TextureId, usize)> {
        let recording = self.candidate.as_ref()?.recording.as_ref()?;
        Some((recording.picture.texture.texture_id(), recording.draw_items))
    }
    #[cfg(test)]
    pub(super) fn stage(&self) -> String {
        self.candidate.as_ref().map_or_else(|| "Queue".to_owned(), |c| format!("{:?}", c.stage))
    }
    #[cfg(test)]
    pub(super) fn drawings(&self) -> u32 {
        self.candidate.as_ref().map_or(0, |c| c.times.draws)
    }
    #[cfg(test)]
    pub(super) fn registration_steps(&self) -> u32 {
        self.candidate.as_ref().map_or(0, |c| c.times.register_steps)
    }
    #[cfg(test)]
    pub(super) fn heap_before_eval(&self, cx: &mut Cx) -> Option<usize> {
        self.candidate.as_ref()?.splash.as_splash().isolate_heap_key(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_is_as_dense_as_the_screen_rounded_up_and_fits_the_gpu() {
        let design = BoardDesign { revision: None, src: Arc::from("View{}"), src_hash: 0 };
        let desktop = (1440 * 64, 900 * 64);
        let density = |scale, dpi, max_side| RenderSpec::new(&design, desktop, (0, 0), scale, dpi, max_side).density;
        assert_eq!(density(1., 2., 4096), 2., "100 % on a Retina screen: two pixels a point");
        assert_eq!(density(0.13, 2., 4096), 0.5, "13 %: 0.26 rounds up to 0.5");
        assert_eq!(density(1., 1.5, 4096), 2., "a fractional scale factor rounds up");
        assert_eq!(density(0.001, 1., 4096), 0.125, "far out, an eighth of a pixel a point at least");
        let capped = density(1.43, 2., 4096);
        assert_eq!(Cx::pass_pixel_size(dvec2(1440., 900.), capped).0, 4096, "zoomed past the GPU's side, the longer side fills it");
        let device = density(4., 2., 2048);
        assert_eq!(Cx::pass_pixel_size(dvec2(1440., 900.), device).0, 2048, "a smaller device maximum caps it");
    }
}
