//! Content-addressed final pictures. Card widgets only consume textures.
//! This idle service owns one finite GPU readback job, never a playback clock.
use super::cut::store::{LazyStoreMedia};
use makepad_library_browser::lanes::{Thumb};
use makepad_asset_data::{AssetKind, BlobId};
use makepad_asset_data::tree::{TreeContext, TreeDraft, TreeLookup};
use makepad_story_cut::CutDoc;
use makepad_story_edit_render::{CutJob, CutThumbnail, Thumbnail};
use makepad_widgets::*;
use std::{collections::{BTreeMap, HashMap, HashSet}, sync::Arc};

const RECIPE: &str = "stage-tree-still-3:last-frame:painted-readback:no-live-inputs";
/// Fingerprint of the renderer's sources (build.rs): a renderer fix changes it,
/// so thumbnails drawn by the old renderer are never found again.
const RENDERER_STAMP: &str = env!("STILL_RENDERER_STAMP");
const SIZE_SETTLE: f64 = 0.3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Size { pub width: u32, pub height: u32, pub dpi: f64 }

impl Size {
    pub fn new(width: f64, dpi: f64, aspect: f64) -> Self {
        let width = (width * dpi).round().max(1.0) as u32;
        Self { width, height: (width as f64 / aspect).round().max(1.0) as u32, dpi }
    }
}

#[derive(Clone)]
pub struct Spec {
    /// Just this slide and its referenced declarations, never another slide.
    pub source: Arc<str>,
    pub context: Option<TreeContext>,
    pub cut: Arc<CutDoc>,
    pub live_name: Option<String>,
}

#[derive(Clone)]
pub struct Demand { pub id: usize, pub spec: Arc<Spec>, pub size: Size }

/// Follow identifiers into top-level declarations by parsed source spans. A
/// custom inline cut can use shared lets without hashing unrelated slides.
pub fn declarations(source: &str, span: (usize, usize)) -> String {
    use makepad_script::syntax::{self, Kind};
    let Ok(tree) = syntax::parse(source) else { return source.to_string() };
    let mut ranges = vec![span.0..span.1];
    let mut names = HashSet::new();
    loop {
        let before = ranges.len();
        tree.walk(&mut |_, node| {
            if ranges.iter().any(|range| range.start <= node.span.start && node.span.end <= range.end) {
                if let Kind::Ident(name) = &node.kind { names.insert(name.split('.').next().unwrap_or(name).to_string()); }
            }
        });
        for node in tree.statements() {
            let used = match &node.kind {
                Kind::Let { names: declared, .. } => declared.iter().any(|name| names.contains(name)),
                Kind::Fn { name: Some(name), .. } => names.contains(name),
                _ => false,
            };
            if used && !ranges.contains(&node.span) { ranges.push(node.span.clone()); }
        }
        if before == ranges.len() { break; }
    }
    ranges.sort_by_key(|range| range.start);
    ranges.into_iter().map(|range| &source[range]).collect::<Vec<_>>().join("\n")
}

fn part(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(bytes);
}

/// Hash parsed transitive references, including optional absence and media bytes.
/// Run on the pool: neither parsing nor hashing a large asset belongs in draw.
pub fn key(spec: &Spec, size: Size) -> Result<String, String> { key_with(spec, size, RENDERER_STAMP) }

fn key_with(spec: &Spec, size: Size, stamp: &str) -> Result<String, String> {
    let mut pending = vec![(spec.context.clone(), spec.source.clone())];
    let mut files = BTreeMap::new();
    while let Some((context, source)) = pending.pop() {
        let Some(context) = context else { continue };
        let syntax = makepad_script::syntax::parse(&source).map_err(|error| format!("{error:?}"))?;
        for (_, path) in makepad_asset_client::tree::references(&syntax)? {
            match context.lookup(&path)? {
                TreeLookup::MissingOptional(path) => { files.insert(path, "absent".to_string()); }
                TreeLookup::File(file) => {
                    if files.contains_key(&file.path) { continue; }
                    files.insert(file.path.clone(), BlobId::hash_of(&file.bytes).to_string());
                    if file.path.ends_with(".splash") {
                        pending.push((Some(context.at(file.path.clone())), Arc::from(file.text()?)));
                    }
                }
            }
        }
    }
    let mut bytes = Vec::new();
    part(&mut bytes, RECIPE.as_bytes());
    part(&mut bytes, stamp.as_bytes());
    part(&mut bytes, spec.source.as_bytes());
    part(&mut bytes, format!("{}:{}:{}:{}", spec.cut.output.width, spec.cut.output.height, spec.cut.output.fps.to_bits(), spec.cut.duration.to_bits()).as_bytes());
    for (path, digest) in files { part(&mut bytes, path.as_bytes()); part(&mut bytes, digest.as_bytes()); }
    // Standalone cuts pin all library revisions in their source/dependency list.
    part(&mut bytes, format!("{:?}", makepad_story_cut::dependencies(&spec.cut)).as_bytes());
    let hex = |bytes: &[u8]| BlobId::hash_of(bytes).as_bytes().iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    let content = hex(&bytes);
    part(&mut bytes, format!("{}:{}:{}", size.width, size.height, size.dpi.to_bits()).as_bytes());
    Ok(format!("stills/h_{content}/s_{}.png", hex(&bytes)))
}

/// Called on the store worker during a tree save. Copy only variants of the
/// current content hash; PNGs travel with the tree and remain available offline.
pub fn carry_cached(tree: &mut TreeDraft, spec: &Spec) -> Result<(), String> {
    let path = key(spec, Size { width: 0, height: 0, dpi: 0.0 })?;
    let directory = path.rsplit_once('/').unwrap().0;
    let entries = match std::fs::read_dir(disk(directory)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("still cache: {error}")),
    };
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("s_") || !name.ends_with(".png") { continue; }
        let png = std::fs::read(entry.path()).map_err(|error| error.to_string())?;
        tree.write(&format!("{directory}/{name}"), AssetKind::Texture, png.into())?;
    }
    Ok(())
}

/// A publication's preview uses a final frame already carried by this tree.
pub fn cached_thumb(tree: &TreeDraft, spec: &Spec) -> Result<Option<Thumb>, String> {
    let path = key(spec, Size { width: 0, height: 0, dpi: 0.0 })?;
    let prefix = format!("{}/", path.rsplit_once('/').unwrap().0);
    for (_, file) in tree.files.range(prefix.clone()..).take_while(|(path,_)| path.starts_with(&prefix)) {
        if let Ok(image) = ImageBuffer::from_png(&file.bytes) {
            return Ok(Some(Thumb { width: image.width, height: image.height, pixels: Arc::new(image.data) }));
        }
    }
    Ok(None)
}

/// Carry a save's final readback under the same key as a card; no GPU work.
pub fn carry_frame(tree: &mut TreeDraft, spec: &Spec, thumb: &Thumb, dpi: f64) -> Result<(), String> {
    let path = key(spec, Size { width: thumb.width as u32, height: thumb.height as u32, dpi })?;
    let rgba = thumb.pixels.iter().flat_map(|pixel| [(pixel >> 16) as u8, (pixel >> 8) as u8, *pixel as u8, (pixel >> 24) as u8]).collect();
    let fps = makepad_story_edit_render::audio::fps_ratio(spec.cut.output.fps);
    let t = makepad_story_edit_render::audio::frame_time(makepad_story_edit_render::audio::frame_count(spec.cut.duration, fps).saturating_sub(1), fps);
    let frame = Thumbnail { rgba, width: thumb.width as u32, height: thumb.height as u32, t };
    let pixels = encode(&path, &frame)?;
    tree.write(&path, AssetKind::Texture, pixels.png)?;
    Ok(())
}

struct Pixels { thumb: Thumb, png: Arc<[u8]> }
struct Loaded { path: String, pixels: Option<Pixels>, exact: bool }

fn disk(path: &str) -> std::path::PathBuf {
    makepad_asset_client::paths::library_root().join("tree-stills").join(path)
}

/// The still at `size`, else the same final frame at another size (shown
/// scaled at once while the exact one is drawn: `exact` false).
fn load(spec: &Spec, size: Size) -> Result<Loaded, String> {
    let path = key(spec, size)?;
    let read = |path: &str| spec.context.as_ref().and_then(|ctx| ctx.resolver.resolve("", path).ok()).map(|file| file.bytes)
        .or_else(|| std::fs::read(disk(path)).ok().map(Arc::from));
    let decode = |png: Arc<[u8]>| ImageBuffer::from_png(&png).ok().map(|image| Pixels {
        thumb: Thumb { width: image.width, height: image.height, pixels: Arc::new(image.data) }, png,
    });
    if let Some(pixels) = read(&path).and_then(decode) {
        return Ok(Loaded { path, pixels: Some(pixels), exact: true });
    }
    // Every size of one content lives in its folder (`stills/h_<content>/`).
    let directory = path.rsplit_once('/').map_or("", |(directory, _)| directory);
    let other = std::fs::read_dir(disk(directory)).into_iter().flatten().flatten()
        .filter_map(|entry| entry.file_name().to_str().filter(|name| name.starts_with("s_") && name.ends_with(".png")).map(|name| format!("{directory}/{name}")))
        .find_map(|other| read(&other).and_then(decode));
    Ok(Loaded { path, pixels: other, exact: false })
}

fn encode(path: &str, frame: &Thumbnail) -> Result<Pixels, String> {
    let png: Arc<[u8]> = Cx::encode_rgba_as_png(frame.width, frame.height, &frame.rgba).map_err(|error| error.to_string())?.into();
    let target = disk(path);
    if let Some(parent) = target.parent() { if let Err(error) = std::fs::create_dir_all(parent) { log!("still: create cache: {error}"); } }
    // Immutable content address: another writer produces the same pixels.
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let serial = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temporary = target.with_extension(format!("{}.{serial}.tmp", std::process::id()));
    if let Err(error) = std::fs::write(&temporary, &png).and_then(|_| std::fs::rename(&temporary, &target)) { log!("still: persist {path}: {error}"); }
    Ok(Pixels { thumb: Thumb { width: frame.width as usize, height: frame.height as usize, pixels: Arc::new(frame.argb()) }, png })
}



struct Entry {
    spec: Arc<Spec>, size: Size, wanted: Size, changed: f64,
    path: Option<String>, pixels: Option<Pixels>, texture: Option<Texture>, failed: bool, refresh: bool,
}

enum Active {
    Loading { id: usize, size: Size, task: TaskHandle<Result<Loaded, String>> },
    Rendering { id: usize, size: Size, renderer: CutThumbnail },
    Encoding { id: usize, size: Size, frame: Arc<Thumbnail>, task: Option<TaskHandle<Result<Pixels, String>>> },
}

/// A single admitted still job across all visible cards of one host. Tasks use
/// the shared application pool.
#[derive(Default)]
pub struct Stills {
    entries: HashMap<usize, Entry>,
    visible: HashSet<usize>,
    active: Option<Active>,
    timer: Timer,
    presented: bool,
    allowed: bool,
    pub revision: u64,
}

impl Stills {
    pub fn clear(&mut self, cx: &mut Cx) {
        cx.stop_timer(self.timer);
        let revision = self.revision.wrapping_add(1);
        *self = Self { revision, ..Self::default() };
    }

    pub fn begin(&mut self) { self.visible.clear(); }

    pub fn want(&mut self, id: usize, spec: Arc<Spec>, size: Size) {
        if spec.live_name.is_some() { return; }
        self.visible.insert(id);
        let now = Cx::time_now();
        let entry = self.entries.entry(id).or_insert_with(|| Entry {
            spec: spec.clone(), size, wanted: size, changed: now, path: None, pixels: None, texture: None, failed: false, refresh: false,
        });
        if !Arc::ptr_eq(&entry.spec, &spec) {
            // A completion belongs to the source it started with.
            let active_id = self.active.as_ref().map(|active| match active {
                Active::Loading { id, .. } | Active::Rendering { id, .. } | Active::Encoding { id, .. } => *id,
            });
            if active_id == Some(id) { self.active = None; }
            *entry = Entry { spec, size, wanted: size, changed: now, path: None, pixels: None, texture: None, failed: false, refresh: false };
        } else if entry.wanted != size { entry.wanted = size; entry.changed = now; }
    }

    pub fn texture(&self, id: usize) -> Option<Texture> { self.entries.get(&id)?.texture.clone() }
    pub fn thumb(&self, id: usize) -> Option<Thumb> { Some(self.entries.get(&id)?.pixels.as_ref()?.thumb.clone()) }

    pub fn has_uncarried(&self, tree: &TreeDraft, ids: std::ops::Range<usize>) -> bool {
        ids.filter_map(|id| self.entries.get(&id)).any(|entry| entry.pixels.is_some()
            && entry.path.as_ref().is_some_and(|path| !tree.files.contains_key(path)))
    }

    fn install(&mut self, cx: &mut Cx, id: usize, size: Size, pixels: Pixels) {
        let Some(entry) = self.entries.get_mut(&id) else { return };
        entry.texture = Some(Texture::new_with_format(cx, TextureFormat::VecBGRAu8_32 {
            width: pixels.thumb.width, height: pixels.thumb.height,
            data: Some((*pixels.thumb.pixels).clone()), updated: TextureUpdated::Full,
        }));
        entry.size = size;
        entry.pixels = Some(pixels);
        entry.refresh = false;
        self.revision = self.revision.wrapping_add(1);
    }

    fn fail(&mut self, id: usize, error: String) {
        if let Some(entry) = self.entries.get_mut(&id) { entry.failed = true; }
        log!("still: {id}: {error}");
    }

    pub fn frame_event(&self, event: &Event) -> bool {
        self.timer.is_event(event).is_some() || (self.active.is_some() && matches!(event, Event::Signal))
    }

    pub fn event(&mut self, cx: &mut Cx, event: &Event) -> bool {
        let redraw = self.frame_event(event);
        if let Some(Active::Rendering { renderer, .. }) = &mut self.active { renderer.handle_event(cx, event); }
        if self.timer.is_event(event).is_some() { self.timer = Timer::default(); }
        redraw
    }

    pub fn draw(&mut self, cx: &mut Cx2d, allowed: bool) {
        self.presented |= cx.passes.id_iter().any(|id| matches!(cx.passes[id].parent, CxDrawPassParent::Window(_)) && cx.passes[id].painted_serial > 0);
        self.allowed = allowed && self.presented;
        let active = self.active.take().filter(|active| match active {
            Active::Rendering { id, .. } => self.allowed && self.visible.contains(id) && self.entries.get(id).is_some_and(|entry| Cx::time_now() - entry.changed >= SIZE_SETTLE),
            _ => true,
        });
        self.active = match active {
            Some(Active::Loading { id, size, mut task }) => match task.try_take().map(|result| result.map_err(|error| format!("{error:?}")).and_then(|result| result)) {
                None => Some(Active::Loading { id, size, task }),
                Some(Err(error)) => { self.fail(id, error); None }
                Some(Ok(loaded)) => {
                    if let Some(entry) = self.entries.get_mut(&id) { entry.path = Some(loaded.path); }
                    if let Some(pixels) = loaded.pixels { self.install(cx, id, size, pixels); }
                    // Not at this size yet: drawn when stills may draw.
                    if !loaded.exact { if let Some(entry) = self.entries.get_mut(&id) { entry.refresh = true; } }
                    None
                }
            },
            Some(Active::Rendering { id, size, mut renderer }) => {
                if self.allowed && self.visible.contains(&id) { renderer.draw(cx); }
                match renderer.result() {
                    Some(Ok(frame)) => {
                        trace!("stage.still", "still: final frame id={id} pixels={}x{} dpi={} t={:.3}", size.width, size.height, size.dpi, frame.t);
                        Some(Active::Encoding { id, size, frame: Arc::new(frame.clone()), task: None })
                    },
                    Some(Err(error)) => { self.fail(id, error.clone()); None }
                    None => Some(Active::Rendering { id, size, renderer }),
                }
            }
            Some(Active::Encoding { id, size, frame, mut task }) => {
                if let Some(result) = task.as_mut().and_then(TaskHandle::try_take).map(|result| result.map_err(|error| format!("{error:?}")).and_then(|result| result)) {
                    match result { Ok(pixels) => self.install(cx, id, size, pixels), Err(error) => self.fail(id, error) }
                    None
                } else {
                    if task.is_none() {
                        let path = self.entries.get(&id).and_then(|entry| entry.path.clone());
                        if let Some(path) = path {
                            let pixels = frame.clone();
                            match cx.task_pool().submit(Class::Background, move || encode(&path, &pixels)) {
                                Ok(job) => task = Some(job),
                                Err(error) => { self.fail(id, error.to_string()); return; }
                            }
                        }
                    }
                    Some(Active::Encoding { id, size, frame, task })
                }
            }
            None => None,
        };
        if self.active.is_none() {
            let now = Cx::time_now();
            let wanted = self.visible.iter().copied().filter(|id| self.entries.get(id).is_some_and(|entry| !entry.failed
                && now - entry.changed >= SIZE_SETTLE && (entry.refresh || entry.pixels.is_none() || entry.size != entry.wanted))).min();
            if let Some(id) = wanted {
                let entry = self.entries.get_mut(&id).unwrap();
                let size = entry.wanted;
                if entry.path.is_none() || entry.size != size {
                    let spec = entry.spec.clone();
                    match cx.task_pool().submit(Class::Background, move || load(&spec, size)) {
                        Ok(task) => {
                            entry.size = size;
                            entry.path = None;
                            self.active = Some(Active::Loading { id, size, task });
                        }
                        Err(error) => self.fail(id, error.to_string()),
                    }
                } else if self.allowed {
                    // Resize demand settles before hashing. The settled card
                    // gets its exact pixel size, including the current DPI.
                    let size = entry.size;
                    let cut = entry.spec.cut.clone();
                    let t = cut.duration; // CutThumbnail clamps to the final frame on its frame grid.
                    let job = CutJob { doc: cut, marks: HashMap::new(), resolver: Box::new(LazyStoreMedia::new()), audio: false };
                    self.active = Some(Active::Rendering { id, size, renderer: CutThumbnail::new(cx, job, t, size.width) });
                }
            }
        }
        let needs_work = self.active.is_some() || self.visible.iter().any(|id| self.entries.get(id).is_some_and(|entry|
            !entry.failed && (entry.refresh || entry.pixels.is_none() || entry.size != entry.wanted)));
        if needs_work && (self.allowed || !self.presented) && self.timer.0 == 0 {
            let delay = match &self.active {
                Some(Active::Loading { .. } | Active::Encoding { task: Some(_), .. }) => None,
                Some(_) => Some(1.0 / 30.0),
                None => {
                    let now = Cx::time_now();
                    Some(self.visible.iter().filter_map(|id| self.entries.get(id)).filter(|entry| !entry.failed)
                        .map(|entry| (entry.changed + SIZE_SETTLE - now).max(1.0 / 30.0))
                        .fold(f64::INFINITY, f64::min).min(SIZE_SETTLE))
                },
            };
            // Pool completion signals wake loads/encodes. Only settle and
            // finite readback progress need a timer, never an animation frame.
            if let Some(delay) = delay { self.timer = cx.start_timeout(delay); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (The library's card list is the library browser's: that crate has no
    /// renderer to start.)
    #[test]
    fn thumbnail_surfaces_cannot_start_playback_or_request_animation_frames() {
        for source in [include_str!("deck/thumbnails.rs"), include_str!("deck/sorter.rs"), include_str!("deck/presenter.rs"), include_str!("cut/browser.rs")] {
            for forbidden in ["CutEngine::", "MotionDraw::", "SlideRenderer::", "CutThumbnail::", "new_next_frame("] {
                assert!(!source.contains(forbidden), "thumbnail view introduced {forbidden}");
            }
        }
    }

    #[test]
    fn still_key_changes_with_the_renderer_stamp() {
        assert!(!RENDERER_STAMP.is_empty());
        let doc = makepad_story_deck::DeckDoc::load("use mod.deck.*\nDeck{slides: [Slide{name: @app app: \"scope\"}]}").unwrap();
        let makepad_story_deck::SlideCut::Inline(cut) = &doc.slides[0].cut else { panic!("lowered app") };
        let spec = Spec { source: "Slide{}".into(), context: None, cut: Arc::new((**cut).clone()), live_name: None };
        let size = Size::new(240.0, 2.0, 16.0/9.0);
        let a = key_with(&spec, size, "stamp-a").unwrap();
        assert_eq!(a, key_with(&spec, size, "stamp-a").unwrap());
        assert_ne!(a, key_with(&spec, size, "stamp-b").unwrap());
        assert_eq!(key(&spec, size).unwrap(), key_with(&spec, size, RENDERER_STAMP).unwrap());
    }

    #[test]
    fn still_key_tracks_transitive_files_and_pixels_without_unrelated_slides() {
        use makepad_asset_data::tree::TreeRole;
        let source = "Slide{name: @title graphic: \"slides/title\"}";
        let mut tree = TreeDraft::new("test".into(), TreeRole::Deck, "deck.splash".into(), AssetKind::Deck, source.into()).unwrap();
        tree.write("slides/title.splash", AssetKind::Motion, b"let look = import(\"shared/look\")\nMotion{layers: []}".as_slice().into()).unwrap();
        tree.write("shared/look.splash", AssetKind::Motion, b"{color: #fff}".as_slice().into()).unwrap();
        let doc = makepad_story_deck::DeckDoc::load("use mod.deck.*\nDeck{slides: [Slide{name: @app app: \"scope\"}]}").unwrap();
        let makepad_story_deck::SlideCut::Inline(cut) = &doc.slides[0].cut else { panic!("lowered app") };
        let make = |tree: TreeDraft| Spec { source: source.into(), context: Some(Arc::new(tree).context("deck.splash")), cut: Arc::new((**cut).clone()), live_name: None };
        let size = Size::new(240.0, 2.0, 16.0/9.0);
        let before = key(&make(tree.clone()), size).unwrap();
        makepad_asset_data::tree::validate_path(&before).unwrap();
        tree.write("slides/unrelated.splash", AssetKind::Motion, b"Motion{layers: []}".as_slice().into()).unwrap();
        assert_eq!(before, key(&make(tree.clone()), size).unwrap());
        tree.write("shared/look.splash", AssetKind::Motion, b"{color: #000}".as_slice().into()).unwrap();
        assert_ne!(before, key(&make(tree.clone()), size).unwrap());
        assert_ne!(key(&make(tree.clone()), size).unwrap(), key(&make(tree), Size::new(480.0, 2.0, 16.0/9.0)).unwrap());
    }
}
