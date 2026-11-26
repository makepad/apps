//! Real filmstrips for clips whose picture is drawn, not decoded: motion
//! graphics (`Graphic`) and nested cuts (compound clips).
//! Each such clip is rendered alone, as a tiny edit holding only its own
//! node at a small output size, at a few of its local times, through the
//! same cut_render engine as the viewer (locked time, the frame grid).
//!
//! Cheap by construction, and the clips in view first:
//! - each picture is drawn as the preview draws (one pass a frame), not
//!   with a render's sampled shutter;
//! - every clip gets one picture (its middle) before any clip gets more,
//!   the clips the timeline shows before the others; then strips fill in
//!   from their middles, an engine retargeted from time to time;
//! - two clips are drawn at once, from the timeline's own draw;
//! - nothing starts while the page plays;
//! - strips are cached by the mini edit's text and size within the open
//!   tree snapshot; replacing a plate or an imported file invalidates
//!   that snapshot's strips even when the cut text stays the same;
//! - a clip keeps its previous strip until its new one has a picture (a
//!   live edit of the document updates without flashing the placeholder).

use super::source;
use makepad_library_browser::draw::Filmstrip;
use makepad_story_edit_render::engine::{CutJob, EngineOptions};
use makepad_story_edit_render::{CutThumbnail, MediaResolver};
use makepad_story_cut::{CutDoc, CutNodeKind};
use makepad_story_cut::tree::TreeContext;
use makepad_widgets::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The mini edit's output width (its height follows the cut's aspect).
const MINI_WIDTH: u32 = 256;
/// The thumbnails' width.
const THUMB_WIDTH: u32 = 160;

/// One clip's strip to make.
#[derive(Clone, Debug, PartialEq)]
pub struct FilmJob {
    /// The timeline node.
    pub node: String,
    /// Content key: the mini edit's text hashed.
    pub key: String,
    pub source: String,
    /// Edit times to render, and the clip-local time each stands for.
    pub times: Vec<(f64, f64)>,
}

/// The local times a clip of `length` seconds is sampled at: one every half
/// second, 3 to 12, at the middles of equal parts.
pub fn sample_times(length: f64) -> Vec<f64> {
    let count = ((length / 0.5).round() as usize).clamp(3, 12);
    (0..count).map(|index| length * (index as f64 + 0.5) / count as f64).collect()
}

/// The jobs for every graphic and nested clip of `doc` (its `source`).
pub fn jobs(doc: &CutDoc, source: &str) -> Vec<FilmJob> {
    let mut nodes: Vec<(String, f64, f64)> = doc.graphics.iter().filter_map(|graphic| {
        let span = graphic.span?;
        Some((graphic.id.clone(), span.start, span.length))
    }).collect();
    nodes.extend(doc.nodes.iter().filter(|node| matches!(node.kind, CutNodeKind::Clip { nested: true, .. })).filter_map(|node| {
        let span = node.span?;
        Some((node.id.clone(), span.start, span.length))
    }));
    nodes.into_iter().filter_map(|(node, start, length)| {
        let mini = mini_edit(doc, source, &node)?;
        let key = makepad_asset_data::BlobId::hash_of(mini.as_bytes()).to_string();
        let times = sample_times(length).into_iter().map(|local| (start + local, local)).collect();
        Some(FilmJob { node, key, source: mini, times })
    }).collect()
}

/// A cut holding only `node` (its constructor as written), at a small
/// size of the cut's own aspect, fps and length.
pub fn mini_edit(doc: &CutDoc, source: &str, node: &str) -> Option<String> {
    let (open, close) = source::constructor(source, node).ok()?;
    // `let <node> = Type{ … }`: from `let` to the closing brace.
    let let_at = source[..open].rfind(&format!("let {node}"))?;
    let text = &source[let_at..=close];
    let (w, h) = (doc.output.width.max(1) as f64, doc.output.height.max(1) as f64);
    let width = MINI_WIDTH;
    let height = ((MINI_WIDTH as f64 * h / w / 2.0).round() as u32 * 2).max(2);
    Some(format!(
        "use mod.flow.*\nuse mod.cut.*\n\nlet main = Stack{{layers: [mod.flow.conn(1, mod.flow.wire(@{node}, @frame))]}}\nlet out = CutOutput{{frame: main.out(@frame) audio: []}}\n\n{text}\n\nFlow{{\n    kind: @cut\n    label: \"film\"\n    output: {{width: {width} height: {height} fps: {fps}}}\n    duration: {duration}\n    bpm: {bpm}\n    main, out\n    {node}\n}}\n",
        fps = doc.output.fps, duration = doc.duration.max(1.0), bpm = doc.bpm,
    ))
}

/// One resolver shared by every thumbnail, remembering Motion documents
/// and nested cuts' sources (each is fetched once).
#[derive(Clone)]
pub struct SharedResolver(Arc<Mutex<(Box<dyn MediaResolver>, HashMap<String, String>)>>);

impl SharedResolver {
    pub fn new(inner: Box<dyn MediaResolver>) -> Self {
        Self(Arc::new(Mutex::new((inner, HashMap::new()))))
    }

    fn cached(&self, key: String, fetch: impl FnOnce(&mut dyn MediaResolver) -> Result<String, String>) -> Result<String, String> {
        let mut guard = self.0.lock().map_err(|_| "resolver poisoned".to_string())?;
        if let Some(text) = guard.1.get(&key) {
            return Ok(text.clone());
        }
        let text = fetch(guard.0.as_mut())?;
        guard.1.insert(key, text.clone());
        Ok(text)
    }

    fn with<T>(&self, f: impl FnOnce(&mut dyn MediaResolver) -> Result<T, String>) -> Result<T, String> {
        let mut guard = self.0.lock().map_err(|_| "resolver poisoned".to_string())?;
        f(guard.0.as_mut())
    }
}

impl MediaResolver for SharedResolver {
    fn resolve(&mut self, media: &makepad_story_cut::MediaRef) -> Result<makepad_story_edit_render::ResolvedMedia, String> {
        self.with(|inner| inner.resolve(media))
    }
    fn effect_source(&mut self, effect: &makepad_story_cut::AssetRef) -> Result<String, String> {
        self.cached(format!("fx:{}:{}", effect.id, effect.rev), |inner| inner.effect_source(effect))
    }
    fn cut_source(&mut self, edit: &makepad_story_cut::AssetRef) -> Result<String, String> {
        self.cached(format!("edit:{}:{}", edit.id, edit.rev), |inner| inner.cut_source(edit))
    }
    fn motion_source(&mut self, motion: &makepad_story_cut::AssetRef) -> Result<String, String> {
        self.cached(format!("motion:{}:{}", motion.id, motion.rev), |inner| inner.motion_source(motion))
    }
    fn asset_bytes(&mut self, asset: &makepad_story_cut::AssetRef) -> Result<(Vec<u8>, &'static str), String> {
        self.with(|inner| inner.asset_bytes(asset))
    }
    fn take_info(&mut self, take: &makepad_story_cut::AssetRef) -> Result<Option<makepad_story_cut::TakeInfo>, String> {
        self.with(|inner| inner.take_info(take))
    }
    fn run_data(&mut self, run: &makepad_story_cut::RunRef) -> Result<Option<Vec<u8>>, String> {
        self.with(|inner| inner.run_data(run))
    }
    fn transcript(&mut self, transcript: &makepad_story_cut::AssetRef) -> Result<Option<Vec<makepad_story_cut::SpokenLine>>, String> {
        self.with(|inner| inner.transcript(transcript))
    }
    fn lyrics(&mut self, song: &makepad_story_cut::AssetRef) -> Result<Option<String>, String> {
        self.with(|inner| inner.lyrics(song))
    }
}

/// Strips made at once (each its own engine on the GPU).
const PARALLEL: usize = 2;

/// One clip's strip in the making: the pictures made so far, and which of
/// its times are still to make (the middle one first, then filling in).
struct Making {
    job: FilmJob,
    done: Vec<(f64, Texture)>,
    todo: Vec<usize>,
}

/// One engine drawing one clip's next time.
struct Running {
    node: String,
    key: String,
    index: usize,
    thumb: CutThumbnail,
}

/// The order a clip's times are made in: the middle, then the halves'
/// middles, and so on, so a strip fills in evenly.
fn fill_order(count: usize) -> Vec<usize> {
    let mut order: Vec<usize> = Vec::with_capacity(count);
    let mut spans = std::collections::VecDeque::from([(0usize, count)]);
    while let Some((a, b)) = spans.pop_front() {
        if a >= b {
            continue;
        }
        let mid = (a + b) / 2;
        order.push(mid);
        spans.push_back((a, mid));
        spans.push_back((mid + 1, b));
    }
    order
}

/// The timeline's strip maker (see the module doc). The timeline calls
/// `handle_event` and `draw`; the page sets the jobs.
#[derive(Default)]
pub struct ClipFilms {
    /// The same immutable snapshot and document path as the open cut.
    tree: Option<TreeContext>,
    /// Strips still being made, in timeline order.
    making: Vec<Making>,
    running: Vec<Running>,
    /// Finished strips by content key.
    cache: HashMap<String, Filmstrip>,
    /// Keys that failed (not tried again).
    failed: Vec<String>,
    /// What each node shows: its latest strip, whole or filling in.
    shown: HashMap<String, Filmstrip>,
    wanted: HashMap<String, String>,
    /// When the jobs were set (for the log).
    since: f64,
    /// The clips the timeline drew last (they are made first), and this draw's.
    visible: std::collections::HashSet<String>,
    drawn: std::collections::HashSet<String>,
    resolver: Option<SharedResolver>,
    pub paused: bool,
}

impl ClipFilms {
    pub fn set_resolver(&mut self, resolver: SharedResolver) {
        if self.resolver.is_none() {
            self.resolver = Some(resolver);
        }
    }

    /// The clips of the open cut now. Returns whether anything is to do.
    pub fn set_jobs(&mut self, jobs: Vec<FilmJob>, tree: Option<TreeContext>) -> bool {
        if self.tree != tree {
            self.running.clear();
            self.making.clear();
            self.cache.clear();
            self.failed.clear();
            self.tree = tree;
        }
        self.wanted = jobs.iter().map(|job| (job.node.clone(), job.key.clone())).collect();
        self.since = Cx::time_now();
        // A node's cached strip shows at once; a node gone shows nothing.
        self.shown.retain(|node, _| self.wanted.contains_key(node));
        for (node, key) in &self.wanted {
            if let Some(film) = self.cache.get(key) {
                self.shown.insert(node.clone(), film.clone());
            }
        }
        // A strip no longer wanted stops; one still wanted keeps what it made.
        let wanted = &self.wanted;
        self.running.retain(|running| wanted.get(&running.node) == Some(&running.key));
        let mut kept: Vec<Making> = std::mem::take(&mut self.making);
        self.making = jobs.into_iter().filter(|job| !self.cache.contains_key(&job.key) && !self.failed.contains(&job.key)).map(|job| {
            match kept.iter().position(|making| making.job.key == job.key && making.job.node == job.node) {
                Some(at) => kept.swap_remove(at),
                None => Making { todo: fill_order(job.times.len()), job, done: Vec::new() },
            }
        }).collect();
        self.busy()
    }

    pub fn busy(&self) -> bool {
        !self.running.is_empty() || self.making.iter().any(|making| !making.todo.is_empty())
    }

    /// The strip a node shows, when one is made (or filling in).
    pub fn film(&self, node: &str) -> Option<&Filmstrip> {
        self.shown.get(node)
    }

    /// The timeline draws `node`'s strip now: it is made before the others.
    pub fn mark_visible(&mut self, node: &str) {
        if !self.drawn.contains(node) {
            self.drawn.insert(node.to_string());
        }
    }

    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event) -> bool {
        if self.paused {
            return false;
        }
        let mut changed = false;
        let mut at = 0;
        while at < self.running.len() {
            let running = &mut self.running[at];
            running.thumb.handle_event(cx, event);
            let Some(result) = running.thumb.result().cloned() else {
                at += 1;
                continue;
            };
            changed = true;
            let running = self.running.swap_remove(at);
            let Some(making) = self.making.iter_mut().find(|making| making.job.key == running.key && making.job.node == running.node) else { continue };
            match result {
                Ok(thumb) => {
                    let texture = Texture::new_with_format(cx, TextureFormat::VecBGRAu8_32 {
                        width: thumb.width as usize, height: thumb.height as usize, data: Some(thumb.argb()), updated: TextureUpdated::Full,
                    }).label(cx, "edit/clip-film");
                    making.done.push((making.job.times[running.index].1, texture));
                    making.done.sort_by(|a, b| a.0.total_cmp(&b.0));
                    let film: Filmstrip = Arc::new(making.done.clone());
                    self.shown.insert(making.job.node.clone(), film.clone());
                    if making.done.len() == 1 || making.done.len() == making.job.times.len() {
                        log!("edit: filmstrip `{}`: {} of {} pictures {:.1} s after the cut's strips were asked for", making.job.node, making.done.len(), making.job.times.len(), Cx::time_now() - self.since);
                    }
                    if making.todo.is_empty() && making.done.len() == making.job.times.len() {
                        self.cache.insert(making.job.key.clone(), film);
                        let key = making.job.key.clone();
                        self.making.retain(|making| making.job.key != key);
                    } else if let Some(&next) = making.todo.first().filter(|_| making.done.len() > 1) {
                        // Filling in: the same engine, the next time (P1's retarget).
                        let mut running = running;
                        if running.thumb.retarget(making.job.times[next].0) {
                            making.todo.remove(0);
                            running.index = next;
                            self.running.push(running);
                        }
                    }
                }
                Err(error) => {
                    log!("edit: no filmstrip for `{}`: {error}", making.job.node);
                    let key = making.job.key.clone();
                    self.failed.push(key.clone());
                    self.making.retain(|making| making.job.key != key);
                }
            }
        }
        changed
    }

    /// The next clip time to start: a clip with no picture yet before one
    /// filling in, a clip the timeline shows before one it does not, then
    /// the timeline's order.
    fn next_work(&mut self) -> Option<(usize, usize)> {
        let busy: Vec<(String, String)> = self.running.iter().map(|running| (running.node.clone(), running.key.clone())).collect();
        let rank = |making: &Making| (!making.done.is_empty(), !self.visible.contains(&making.job.node));
        let best = self.making.iter().enumerate()
            .filter(|(_, making)| !making.todo.is_empty() && !busy.iter().any(|(node, key)| *node == making.job.node && *key == making.job.key))
            .min_by_key(|(index, making)| (rank(making), *index))
            .map(|(index, _)| index)?;
        let index = self.making[best].todo.remove(0);
        Some((best, index))
    }

    /// From the timeline's draw (inside its window pass): start strips up
    /// to [`PARALLEL`], and let the running thumbnails record and read back.
    pub fn draw(&mut self, cx: &mut Cx2d) {
        self.visible = std::mem::take(&mut self.drawn);
        if self.paused || self.resolver.is_none() {
            return;
        }
        while self.running.len() < PARALLEL {
            let Some((at, index)) = self.next_work() else { break };
            let job = &self.making[at].job;
            let loaded = match &self.tree {
                Some(tree) => CutDoc::load_tree(&job.source, tree.clone()),
                None => CutDoc::load(&job.source),
            };
            match loaded {
                Ok(doc) => {
                    let doc = Arc::new(doc);
                    let mut thumb = thumbnail(cx, self.resolver.clone().expect("a resolver before jobs"), &doc, job.times[index].0);
                    // A timeline tile is drawn as the preview draws (one pass a
                    // frame), not with a render's sampled shutter.
                    thumb.set_draft(true);
                    self.running.push(Running { node: job.node.clone(), key: job.key.clone(), index, thumb });
                }
                Err(diags) => {
                    log!("edit: `{}` cannot be drawn alone: {}", job.node, super::patch::diagnostics(diags));
                    let key = job.key.clone();
                    self.failed.push(key.clone());
                    self.making.retain(|making| making.job.key != key);
                }
            }
        }
        for running in &mut self.running {
            running.thumb.draw(cx);
        }
    }
}

fn thumbnail(cx: &mut Cx, resolver: SharedResolver, doc: &Arc<CutDoc>, t: f64) -> CutThumbnail {
    let job = CutJob { doc: doc.clone(), marks: HashMap::new(), resolver: Box::new(resolver), audio: false };
    let options = EngineOptions { max_loads: 1, budget: None };
    CutThumbnail::with_options(cx, job, t, THUMB_WIDTH, options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_strip_fills_in_from_its_middle() {
        assert_eq!(fill_order(1), vec![0]);
        assert_eq!(fill_order(4), vec![2, 1, 3, 0]);
        let mut all = fill_order(12);
        assert_eq!(all[0], 6);
        all.sort();
        assert_eq!(all, (0..12).collect::<Vec<_>>());
    }

    #[test]
    fn samples_cover_the_clip_evenly() {
        assert_eq!(sample_times(1.0).len(), 3);
        assert_eq!(sample_times(4.0).len(), 8);
        assert_eq!(sample_times(60.0).len(), 12);
        let times = sample_times(4.0);
        assert!((times[0] - 0.25).abs() < 1e-9 && (times[7] - 3.75).abs() < 1e-9);
    }

    #[test]
    fn a_graphic_alone_is_a_loadable_edit() {
        let source = "use mod.flow.*\nuse mod.cut.*\n\nlet main = Stack{layers: [mod.flow.conn(1, mod.flow.wire(@g1, @frame))]}\nlet out = CutOutput{frame: main.out(@frame) audio: []}\n\nlet g1 = Graphic{\n    src: asset(\"ast_daadjrvgo5tlkvhuj5fxaaw3d4\", \"arev_3553e75234419788aeecdef2373db81584194ea7b20b0397b4b3e22036205220\")\n    span: [2.0, 4.0]\n    track: 0\n}\n\nFlow{\n    kind: @cut\n    label: \"T\"\n    output: {width: 1920 height: 1080 fps: 60}\n    duration: 10.0\n    bpm: 120\n    main, out\n    g1\n}\n";
        let doc = CutDoc::load(source).unwrap();
        let jobs = jobs(&doc, source);
        assert_eq!(jobs.len(), 1);
        let mini = CutDoc::load(&jobs[0].source).unwrap();
        assert_eq!((mini.output.width, mini.output.height), (256, 144));
        assert_eq!(mini.graphics.len(), 1);
        assert_eq!(jobs[0].times.len(), 8);
        assert!((jobs[0].times[0].0 - 2.25).abs() < 1e-9 && (jobs[0].times[0].1 - 0.25).abs() < 1e-9);
        // The same content keys the same; another revision another key.
        assert_eq!(jobs[0].key, super::jobs(&doc, source)[0].key);
        let other = source.replace("arev_3553", "arev_4553");
        assert_ne!(jobs[0].key, super::jobs(&CutDoc::load(&other).unwrap(), &other)[0].key);
    }
}
