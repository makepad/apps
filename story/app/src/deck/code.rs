//! DECK's `Code` layers: a source file of a live app, drawn with Makepad's
//! code editor into a picture of its own.
//!
//! One [`Layer`] per `Code` node of a slide that is shown (or being left):
//! a [`CodeView`] widget drawn into an offscreen target, which is the
//! node's picture for the compositor. The target is drawn again only when
//! what it shows changes (the lines, the size, the text size) or when
//! glyphs it drew with stand-ins have landed: a picture redrawn every frame
//! is never finished when the slide samples it, so the slide would never
//! paint whole and its shown frame would stay the first one (an app
//! layer's "ready to start" card, while the app runs). The text is the file
//! as it is on disk, read again only when the filesystem watcher reports a
//! change near it (no per-draw stat, no polling: a layer with nothing
//! changed asks for no frames). `lines: [a, b]` shows only those lines of
//! the file; the pane never writes to it.

use makepad_code_editor::code_view::CodeView;
use makepad_code_editor::{CodeDocument, CodeSession};
use makepad_story_cut::CodeSpec;
use makepad_story_edit_render::compose::ExternalFrame;
use makepad_widgets::*;
use makepad_filesystem_watcher::{FileSystemEvent, FileSystemEventKind, FileSystemWatcher, WatchRoot};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Whether the shown file may have changed since it was last read: set by
/// the watcher's thread, taken by the draw that reads the file again.
#[derive(Clone)]
struct Dirty(Arc<AtomicBool>);

impl Dirty {
    /// Starts dirty: the file has not been read yet.
    fn new() -> Dirty {
        Dirty(Arc::new(AtomicBool::new(true)))
    }

    fn mark(&self) {
        self.0.store(true, Ordering::Release);
    }

    fn is_set(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    /// True once per change.
    fn take(&self) -> bool {
        self.0.swap(false, Ordering::AcqRel)
    }
}

/// Whether a watcher event concerns the file `name`: the file itself (also
/// written as a temporary and renamed over it), a directory created or
/// removed around it, or events lost.
fn concerns(event: &FileSystemEvent, name: &std::ffi::OsStr) -> bool {
    match &event.kind {
        FileSystemEventKind::RescanRequired { .. } => true,
        FileSystemEventKind::Renamed { from } => event.path.file_name() == Some(name) || from.file_name() == Some(name),
        _ => event.path.file_name() == Some(name) || event.is_dir != Some(false),
    }
}

/// Watch the directory of `path` (the nearest one that exists, so a file or
/// folder that is created later is seen); `dirty` is marked on each event
/// that concerns the file and the UI woken.
fn watch_file(path: &Path, dirty: &Dirty) -> Option<FileSystemWatcher> {
    let name = path.file_name()?.to_os_string();
    let mut dir = path.parent()?;
    while !dir.is_dir() {
        dir = dir.parent()?;
    }
    let dirty = dirty.clone();
    let root = WatchRoot { mount: "code".to_string(), path: dir.to_path_buf() };
    FileSystemWatcher::start(vec![root], move |event| {
        if concerns(&event, &name) {
            dirty.mark();
            SignalToUI::set_ui_signal();
        }
    })
    .ok()
}
/// The page the code is drawn on.
const PAGE: Vec4f = Vec4f { x: 0.066, y: 0.075, z: 0.094, w: 1.0 };

struct Layer {
    slide: usize,
    node: String,
    target: RenderTarget,
    view: WidgetRef,
    /// The file's text from disk, and whether it may have changed since.
    disk: Option<String>,
    dirty: Dirty,
    /// Watches the file's directory while the layer lives.
    watcher: Option<FileSystemWatcher>,
    shown: Option<String>,
    /// The text size the editor was given.
    font_size: f64,
    /// Why there is no picture (the file cannot be read).
    problem: Option<String>,
    serial: u64,
    /// What the target holds: the lines, the size and the text size.
    drawn: Option<(String, (u32, u32), f64)>,
    /// Drawn while glyphs were still rasterising: drawn again once they are.
    glyphs_pending: bool,
}

pub struct Codes {
    layers: Vec<Layer>,
    root: Option<PathBuf>,
}

/// The lines `first..=last` (1-based) of `text`.
fn slice(text: &str, lines: Option<(u32, u32)>) -> &str {
    let Some((first, last)) = lines else { return text };
    let mut starts = vec![0usize];
    starts.extend(text.match_indices('\n').map(|(at, _)| at + 1));
    let first = (first as usize - 1).min(starts.len() - 1);
    let start = starts[first];
    let end = starts.get(last as usize).copied().unwrap_or(text.len());
    &text[start..end.max(start)]
}

impl Codes {
    pub fn new() -> Codes {
        Codes { layers: Vec::new(), root: makepad_app_registry::repo_root() }
    }

    /// Keep the layers of the `slides` shown now; the others are dropped
    /// (their pictures and widgets with them).
    pub fn retain(&mut self, slides: &[usize]) {
        self.layers.retain(|layer| slides.contains(&layer.slide));
    }

    /// Whether a layer of `slide` has a change to draw: its file may have
    /// changed, or glyphs it drew with stand-ins are still landing.
    pub fn moving(&self, slide: usize) -> bool {
        self.layers.iter().any(|layer| layer.slide == slide && layer.moving())
    }

    /// What the presenter reads while a layer shows nothing.
    pub fn notes(&self, slide: usize) -> Vec<String> {
        self.layers.iter().filter(|layer| layer.slide == slide).filter_map(|layer| layer.problem.clone()).collect()
    }

    /// The node's picture for this frame (recorded now, under the pass being
    /// drawn) and its serial. None: nothing to show.
    pub fn frame(&mut self, cx: &mut Cx2d, slide: usize, node: &str, spec: &CodeSpec) -> Option<(ExternalFrame, u64)> {
        let at = match self.layers.iter().position(|layer| layer.slide == slide && layer.node == node) {
            Some(at) => at,
            None => {
                let view = cx.with_vm(CodeView::script_new_with_default);
                self.layers.push(Layer {
                    slide,
                    node: node.to_string(),
                    target: RenderTarget::new(cx, "deck_code"),
                    view: WidgetRef::new_with_inner(Box::new(view)),
                    disk: None,
                    dirty: Dirty::new(),
                    watcher: None,
                    shown: None,
                    font_size: 0.0,
                    problem: None,
                    serial: 0,
                    drawn: None,
                    glyphs_pending: false,
                });
                self.layers.len() - 1
            }
        };
        let layer = &mut self.layers[at];
        if layer.font_size != spec.font_size {
            layer.font_size = spec.font_size;
            if let Some(mut view) = layer.view.borrow_mut::<CodeView>() {
                view.editor.set_font_size(spec.font_size as f32);
                view.redraw(cx);
            }
        }
        let rel = spec.file.as_str();
        layer.watch(self.root.as_deref(), rel);
        let want = slice(layer.disk.as_deref()?, spec.lines).to_string();
        layer.apply(cx, rel, want);
        let size = spec.size;
        let key = (layer.shown.clone().unwrap_or_default(), size, layer.font_size);
        let glyphs_landed = layer.glyphs_pending && !super::apps::glyphs_pending(cx);
        if layer.drawn.as_ref() != Some(&key) || glyphs_landed {
            if layer.target.ensure(cx, size, RenderTargetFormat::Bgra8) {
                layer.target.texture.set_transient(cx, false);
            }
            layer.target.begin(cx, PAGE);
            let walk = Walk { width: Size::Fixed(size.0 as f64), height: Size::Fixed(size.1 as f64), ..Walk::default() };
            layer.view.draw_walk_all(cx, &mut Scope::empty(), walk);
            layer.target.end(cx);
            layer.serial += 1;
            layer.drawn = Some(key);
            layer.glyphs_pending = super::apps::glyphs_pending(cx);
        } else {
            cx.make_child_pass(&layer.target.pass);
        }
        Some((ExternalFrame { texture: layer.target.texture.clone(), size, scale: [1.0, 1.0] }, layer.serial))
    }
}

impl Layer {
    fn moving(&self) -> bool {
        self.dirty.is_set() || self.glyphs_pending
    }

    /// Read the file again when the watcher said it may have changed (the
    /// first time: start watching, then read).
    fn watch(&mut self, root: Option<&Path>, rel: &str) {
        let Some(root) = root else {
            self.problem = Some(format!("code {rel}: no checkout to read it from"));
            self.dirty.take();
            return;
        };
        let path = root.join(rel);
        if self.watcher.is_none() {
            // Watching starts before the first read: a change in between marks it dirty again.
            self.watcher = watch_file(&path, &self.dirty);
        }
        if !self.dirty.take() {
            return;
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                self.disk = Some(text);
                self.problem = None;
            }
            Err(error) => {
                self.problem = Some(format!("code {rel}: {error}"));
                self.disk = None;
            }
        }
    }

    /// Bring the editor to the file's selected lines, only when they change.
    fn apply(&mut self, cx: &mut Cx, path: &str, want: String) {
        if self.shown.as_ref() == Some(&want) {
            return;
        }
        let Some(mut view) = self.view.borrow_mut::<CodeView>() else { return };
        let prepared = CodeDocument::prepare_for_path(path, want.as_str().into());
        view.session = Some(CodeSession::new(CodeDocument::from_prepared(prepared)));
        view.redraw(cx);
        drop(view);
        self.shown = Some(want);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn a_layer_is_dirty_until_read_then_idle_until_the_watcher_marks_it() {
        let dirty = Dirty::new();
        assert!(dirty.is_set(), "unread");
        assert!(dirty.take());
        assert!(!dirty.is_set() && !dirty.take(), "nothing changed: idle");
        dirty.mark();
        dirty.mark();
        assert!(dirty.take(), "an event: one redraw");
        assert!(!dirty.take());
    }

    fn event(path: &str, kind: FileSystemEventKind, is_dir: Option<bool>) -> FileSystemEvent {
        FileSystemEvent { mount: "code".into(), path: path.into(), kind, seq: 0, epoch: 0, received_at: Instant::now(), wall_time: std::time::SystemTime::now(), is_dir }
    }

    #[test]
    fn events_concern_the_file_its_replacement_and_lost_events_only() {
        let name = std::ffi::OsStr::new("a.rs");
        assert!(concerns(&event("/d/a.rs", FileSystemEventKind::Changed, Some(false)), name));
        assert!(!concerns(&event("/d/b.rs", FileSystemEventKind::Changed, Some(false)), name));
        assert!(concerns(&event("/d/a.rs", FileSystemEventKind::Renamed { from: "/d/.a.rs.tmp".into() }, Some(false)), name));
        assert!(concerns(&event("/d/a.rs", FileSystemEventKind::Created, Some(false)), name));
        assert!(concerns(&event("/d/sub", FileSystemEventKind::Removed, None), name));
    }

    fn wait_for(dirty: &Dirty) -> bool {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            if dirty.is_set() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn the_watcher_marks_an_edit_an_atomic_replace_and_a_late_file() {
        let dir = std::env::temp_dir().join(format!("deck_code_watch_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("sub").join("shown.rs");
        let dirty = Dirty::new();
        let _watcher = watch_file(&file, &dirty).expect("watch");
        // The folder made just above can still reach a new watcher (macOS reports it late).
        std::thread::sleep(Duration::from_millis(500));
        dirty.take();
        std::thread::sleep(Duration::from_millis(500));
        assert!(!dirty.is_set(), "quiet");
        // The file does not exist yet (nor its folder): creating it is seen.
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "one").unwrap();
        assert!(wait_for(&dirty), "created");
        std::thread::sleep(Duration::from_millis(500));
        dirty.take();
        std::fs::write(&file, "two").unwrap();
        assert!(wait_for(&dirty), "edited");
        std::thread::sleep(Duration::from_millis(500));
        dirty.take();
        let temp = file.with_file_name(".shown.rs.tmp");
        std::fs::write(&temp, "three").unwrap();
        std::fs::rename(&temp, &file).unwrap();
        assert!(wait_for(&dirty), "replaced atomically");
        drop(_watcher);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
