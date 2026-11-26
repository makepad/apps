//! The generate bar under the ruler, Generate selection (⌃R)
//! and Generate all (⇧⌃R), and the render's settings, file copy and result
//! lines (the settings are the `render_cut` card's).

use super::timeline::{BarSegment, BarState, CutTimeline};
use super::{timecode, VideoCut};
use makepad_widgets::*;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;

#[derive(Default)]
pub struct RenderUi {
    // The last settings used (the Render card's): the Render button (cmd-E)
    // renders with them.
    pub export_high: bool,
    pub export_path: String,
    pub export_publish: bool,
    /// Where the running export goes.
    pub export_to: Option<PathBuf>,
    /// The running render was asked by the control API (`render_cut`):
    /// it writes only the render folder's `render.mp4`, publishes nothing
    /// and copies nothing (no file in the person's Movies folder).
    pub api_file: bool,
    /// The last finished render's file(s), for `render_status`.
    pub last_file: Option<String>,
    copying: Option<Receiver<Result<String, String>>>,
    /// The export's frame rate; `None`: the cut's own.
    pub export_fps: Option<f64>,
    /// Video clips at another rate mix their two nearest frames (P1's
    /// `--blend`) instead of showing the nearest one.
    pub export_blend: bool,
    /// A draft render: graphics drawn once a frame as the preview plays,
    /// near real time; off (final): motion blur sampled in full.
    pub export_draft: bool,
    /// The file the last render wrote (the Open and Show in Finder chips).
    pub exported: Option<PathBuf>,
    /// A cancelled export's working folder, removed once its renderer has
    /// stopped.
    pub cancelled: Option<PathBuf>,
}

impl VideoCut {
    /// Generate bands say their state: generating (with progress),
    /// needs generation, or an old take.
    pub(super) fn with_generate_states(&self, mut projection: super::project::Projection) -> super::project::Projection {
        use super::generate_segments::SegmentState as Gen;
        let segments = self.generate_segments();
        for block in projection.blocks.iter_mut().filter(|block| block.generate) {
            let Some(segment) = segments.iter().find(|segment| segment.node == block.node) else { continue };
            match &segment.state {
                Gen::Generating(done) => {
                    block.progress = Some(*done);
                    block.label = format!("{} · generating {:.0}%", block.label, done * 100.0);
                }
                Gen::NeedsGeneration(makepad_story_cut::generate::GenState::Missing) => block.label = format!("{} · needs generation", block.label),
                Gen::NeedsGeneration(_) => {
                    block.stale = true;
                    block.label = format!("{} · old take · Update", block.label);
                }
                Gen::Generated => {}
            }
        }
        projection
    }

    /// Every pump: the generate bar under the ruler.
    pub(super) fn pump_generate_bar(&mut self, cx: &mut Cx, ui: &WidgetRef, shown: bool) {
        use super::generate_segments::SegmentState as Gen;
        if !shown {
            return;
        }
        let bar: Vec<BarSegment> = self
            .generate_segments()
            .into_iter()
            .map(|segment| BarSegment {
                t0: segment.start,
                t1: segment.end,
                state: match segment.state {
                    Gen::Generating(done) => BarState::Generating(done),
                    Gen::Generated => BarState::Generated,
                    Gen::NeedsGeneration(_) => BarState::NeedsGeneration,
                },
            })
            .collect();
        if let Some(mut timeline) = ui.widget(cx, ids!(cut_timeline)).borrow_mut::<CutTimeline>() {
            timeline.set_render_segments(cx, bar);
        }
    }

    /// ⌃R (the in/out range, else the selection, else the segment under the
    /// playhead) or ⇧⌃R (the whole edit): the generate card opens in the
    /// chat, prefilled for it (no model turn).
    pub(super) fn generate_selection(&mut self, all: bool) {
        let opened = match self.generate_card(all) {
            Some((tool, prefill)) => {
                super::chat::propose(self, tool, &prefill);
                Ok(())
            }
            None if all => Err("Nothing to generate here: every take is made".to_string()),
            None => Err("Select a clip, or mark an in–out range (I, O), to generate over".to_string()),
        };
        if let Err(why) = opened {
            self.status = why;
            self.status_sticky = true;
        }
        self.repaint = true;
    }

    /// A render finished: its line in the chat.
    pub(super) fn render_finished(&mut self, line: String) {
        self.render_done = Some(line.clone());
        self.render_failed = None;
        super::chat::note(self, line);
    }

    /// A render failed: the Render button shows it, the chat says why.
    pub(super) fn render_error(&mut self, why: String) {
        super::chat::note(self, format!("Render failed: {}", super::preview::plain_reason(&why)));
        self.render_failed = Some(why);
    }

    /// The export's file extension: `mov` for a transparent cut (HEVC with
    /// alpha is written into a QuickTime movie), else `mp4`.
    fn export_extension(&self) -> &'static str {
        if self.session.as_ref().is_some_and(|session| session.doc.output.alpha) { "mov" } else { "mp4" }
    }

    /// Where an export goes when nobody chose: ~/Movies/<edit>.mp4 (.mov
    /// for a transparent cut).
    pub(super) fn default_export_path(&self) -> String {
        let label = self.session.as_ref().map_or("Cut".to_string(), |session| session.doc.label.clone());
        let name: String = label.chars().map(|c| if c.is_alphanumeric() || " -_.·()".contains(c) { c } else { '-' }).collect();
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        home.join("Movies").join(format!("{}.{}", name.trim(), self.export_extension())).to_string_lossy().to_string()
    }

    pub(super) fn export_path(&self) -> PathBuf {
        let path = if self.render_ui.export_path.trim().is_empty() { self.default_export_path() } else { self.render_ui.export_path.trim().to_string() };
        let path = match path.strip_prefix("~/") {
            Some(rest) => std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(rest),
            None => PathBuf::from(path),
        };
        let ext = self.export_extension();
        if path.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)) { path } else { path.with_extension(ext) }
    }

    /// `new_cut` with `reframe`: a new cut in another format that nests this
    /// one (its saved revision), filled to the new frame; check the framing
    /// there, then render it. Err says why not.
    pub(crate) fn reframe_to(&mut self, width: u32, height: u32, aspect: &str) -> Result<(), String> {
        use super::ops::{CutOp, Media, MediaKind};
        let Some(session) = &self.session else { return Err("no cut is open".into()) };
        let Some(head) = session.head.filter(|_| !session.dirty()) else {
            return Err("save the cut first (save_cut): the new format nests its saved revision".into());
        };
        let (label, duration) = (session.doc.label.clone(), session.doc.duration);
        let media = Media {
            kind: MediaKind::Cut,
            src: makepad_story_cut::MediaRef::Asset(makepad_story_cut::AssetRef { id: head.asset_id.to_string(), rev: head.revision.to_string() }),
            seconds: Some(duration),
        };
        self.crumbs.clear();
        self.open_source(super::template(width, height, &format!("{label} · {aspect}")), None, false);
        self.op(CutOp::Header { key: "duration".into(), value: super::patch::Value::num(duration.max(1.0)) });
        self.op(CutOp::Add { media, start: 0.0, track: None });
        self.status = format!("A {aspect} edit of “{label}”: check the framing, then render it");
        self.status_sticky = true;
        self.repaint = true;
        Ok(())
    }

    /// Video bits per second for the render's bitrate setting: `None` is the
    /// renderer's standard (0.12 bits per pixel), High twice that.
    pub(super) fn export_bitrate(&self, width: u32, height: u32, fps: f64) -> Option<u32> {
        self.render_ui.export_high.then(|| (width as f64 * height as f64 * fps * 0.24).max(4_000_000.0) as u32)
    }

    /// The render finished in `dir`: copy it (and its captions) to the
    /// chosen file, off the UI thread. True when the library should get it
    /// too (Publish to library).
    pub(super) fn export_finished(&mut self, dir: &Path) -> bool {
        let Some(to) = self.render_ui.export_to.clone() else { return true };
        let from = dir.join(super::render::file_name(self.render.as_ref().is_some_and(|render| render.doc.output.alpha)));
        let sidecars: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|entry| entry.path())
            .filter(|path| path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with("render.") && (name.ends_with(".srt") || name.ends_with(".vtt"))))
            .collect();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = (|| {
                if let Some(parent) = to.parent() {
                    std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
                }
                std::fs::copy(&from, &to).map_err(|error| format!("{}: {error}", to.display()))?;
                // `render.<node>.srt` → `<file>.srt` (one track) or `<file>.<node>.srt`.
                let single = sidecars.iter().filter(|path| path.extension().is_some_and(|ext| ext == "srt")).count() == 1;
                let mut written = vec![to.display().to_string()];
                for sidecar in &sidecars {
                    let name = sidecar.file_name().and_then(|name| name.to_str()).unwrap_or_default();
                    let parts: Vec<&str> = name.split('.').collect();
                    let (node, ext) = (parts.get(1).copied().unwrap_or("captions"), parts.last().copied().unwrap_or("srt"));
                    let target = if single { to.with_extension(ext) } else { to.with_extension(format!("{node}.{ext}")) };
                    std::fs::copy(sidecar, &target).map_err(|error| format!("{}: {error}", target.display()))?;
                    written.push(target.display().to_string());
                }
                Ok(written.join(", "))
            })();
            let _ = tx.send(result);
        });
        self.render_ui.copying = Some(rx);
        self.publishes_render()
    }

    /// Whether the render under way goes to the library too: when asked,
    /// and a library cut's (a folder cut's render is its file).
    pub(super) fn publishes_render(&self) -> bool {
        self.render_ui.export_publish && self.render.as_ref().is_some_and(|render| render.edit.is_some())
    }

    /// Remove a cancelled export's working folder (its partial render, its
    /// job listing and log), off the UI thread.
    pub(super) fn remove_cancelled(&mut self) {
        let Some(dir) = self.render_ui.cancelled.take() else { return };
        // A computed `…/edit-renders/<asset>-<rev>` or `…/folder-<name>` folder, never empty.
        if dir.file_name().is_some_and(|name| name.to_string_lossy().starts_with("ast_") || name.to_string_lossy().starts_with("folder-")) && dir.parent().is_some_and(|parent| parent.ends_with("edit-renders")) {
            std::thread::spawn(move || {
                let _ = std::fs::remove_dir_all(&dir);
            });
        }
    }

    pub(super) fn pump_export_copy(&mut self) {
        let Some(result) = self.render_ui.copying.as_ref().and_then(|rx| rx.try_recv().ok()) else { return };
        self.render_ui.copying = None;
        self.render_ui.export_to = None;
        match result {
            Ok(written) => {
                self.render_ui.exported = written.split(", ").next().map(PathBuf::from);
                self.status = format!("Exported: {written}");
                self.render_ui.last_file = Some(written.clone());
                let doc = self.render.as_ref().map(|render| render.doc.clone());
                if !self.publishes_render() {
                    self.render = None;
                    self.render_finished(match doc {
                        Some(doc) => format!("Exported · {}×{} · {} · {written}", doc.output.width, doc.output.height, timecode(doc.duration, doc.output.fps)),
                        None => format!("Exported · {written}"),
                    });
                } else {
                    super::chat::note(self, format!("Exported: {written}"));
                }
            }
            Err(error) => {
                self.render = None;
                self.status = format!("Export failed: {error}");
                self.render_error(format!("The render finished but was not written: {error}"));
            }
        }
        self.repaint = true;
    }
}
