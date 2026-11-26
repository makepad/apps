//! Rendering a cut from the CUT page: the standalone
//! `makepad-edit-render export`, launched hidden with `--parent-pipe`, reads
//! a render job (`makepad_story_edit_render::job`): the cut and every file
//! it reads, named where they already lie (a folder cut's own files, the
//! store's cached blobs; nothing is copied), and writes the MP4 with the
//! render's settings (`--fps`, `--loudness`, `--no-burn`). Its own process
//! keeps its GPU readbacks and decoders apart from Stage's (L5 R3).
//! A long-lived runner thread writes the job, starts the child at once and
//! forwards its progress lines; the page holds the child's stdin, writes
//! `cancel` into it, or drops it when Stage quits, and the renderer then
//! removes its partial file and exits.

use makepad_widgets::*;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TryRecvError};
use std::sync::Arc;

/// The render folder's file: a QuickTime movie for a transparent cut
/// (`output: {alpha: true}`, HEVC with alpha), else MPEG-4.
pub fn file_name(alpha: bool) -> &'static str {
    if alpha { "render.mov" } else { "render.mp4" }
}

/// What a render draws: the cut's source, its tree and the file in it,
/// the folder it lives in (a folder cut), and the revision it is (a saved
/// library cut).
pub struct CutSource {
    pub source: String,
    pub tree: Option<(Arc<makepad_asset_data::tree::TreeDraft>, String)>,
    pub folder: Option<PathBuf>,
    pub saved: Option<makepad_asset_data::AssetRevisionRef>,
}

impl CutSource {
    /// Its render job, every file where it lies; what is nowhere yet (an
    /// unsaved edit) goes under `scratch`. A folder cut reads no store.
    pub fn job(&self, store: &mut super::store::LazyStoreMedia, scratch: &std::path::Path) -> Result<makepad_story_edit_render::job::Job, String> {
        use makepad_story_edit_render::job::Job;
        match &self.tree {
            Some((tree, path)) => {
                let store = if self.folder.is_some() { None } else { Some(store.store()?) };
                Job::from_tree(store, tree, path, &self.source, self.folder.as_deref(), scratch)
            }
            None => {
                let saved = self.saved.map(|saved| makepad_story_cut::AssetRef { id: saved.asset_id.to_string(), rev: saved.revision.to_string() });
                Job::from_cut(store.store()?, saved.as_ref(), &self.source, scratch)
            }
        }
    }
}

/// One render to run: the cut, its folder (the job, the log and the file)
/// and output, and its settings.
pub struct RenderJob {
    pub cut: CutSource,
    pub dir: PathBuf,
    pub out: PathBuf,
    /// The export's frame rate (the cut's own when `None`).
    pub fps: Option<f64>,
    /// The mix's loudness target (LUFS), or none: the mix as authored.
    pub loudness: Option<f64>,
    /// Captions drawn into the picture (their sidecars are written either way).
    pub burn: bool,
    /// Video bits per second; `None`: the renderer's own (by pixel rate).
    pub bitrate: Option<u32>,
    /// Clips at another rate blend their two nearest frames (`--blend`).
    pub blend: bool,
    /// Draft: graphics drawn once a frame, as the preview plays
    /// (`--quality draft`); otherwise final, motion blur sampled.
    pub draft: bool,
}

pub enum RenderEvent {
    /// The renderer runs; its stdin is the page's to cancel it with (a
    /// `cancel` line) or to close.
    Started(ChildStdin),
    Progress { frame: u64, frames: u64 },
    Done,
    Failed(String),
}

pub struct RenderRunner {
    jobs: Option<SyncSender<RenderJob>>,
    events: Receiver<RenderEvent>,
    /// The running child's stdin.
    stdin: Option<ChildStdin>,
    /// A cancel asked before the child started: it is told once it has.
    cancel_asked: bool,
}

/// The renderer binary: `STAGE_EDIT_RENDER`, else next to Stage's own
/// executable, else (a development build whose executable was re-executed
/// from a synthetic app bundle, as the CEF build is) the workspace's own
/// target folder for this profile.
fn binary() -> Result<PathBuf, String> {
    let name = if cfg!(windows) { "makepad-edit-render.exe" } else { "makepad-edit-render" };
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("STAGE_EDIT_RENDER") {
        candidates.push(PathBuf::from(path));
    }
    if let Ok(exe) = std::env::current_exe() {
        candidates.push(exe.with_file_name(name));
    }
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../target").join(profile).join(name));
    candidates.iter().find(|path| path.is_file()).cloned().ok_or_else(|| {
        format!("the renderer is not built (cargo build --release -p makepad-story-edit-render), or set STAGE_EDIT_RENDER; looked in {}", candidates.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join(", "))
    })
}

/// `edit-render: frame 12 / 120 (1.4 s)`.
pub fn parse_progress(line: &str) -> Option<(u64, u64)> {
    let rest = line.strip_prefix("edit-render: frame ")?;
    let (frame, rest) = rest.split_once(" / ")?;
    let frames = rest.split_whitespace().next()?;
    Some((frame.trim().parse().ok()?, frames.parse().ok()?))
}

impl RenderRunner {
    pub fn new(cx: &mut Cx) -> Result<Self, String> {
        let (job_tx, job_rx) = sync_channel(1);
        let (event_tx, event_rx) = sync_channel(64);
        let options = ThreadOptions { name: Some(Arc::from("stage-edit-render")), stack_size: None, priority: CxThreadPriority::Utility };
        let handle = cx.thread_spawner().spawn_worker(options, move || run(job_rx, event_tx)).map_err(|error| format!("{error:?}"))?;
        handle.detach();
        Ok(Self { jobs: Some(job_tx), events: event_rx, stdin: None, cancel_asked: false })
    }

    pub fn start(&mut self, job: RenderJob) -> Result<(), String> {
        self.cancel_asked = false;
        self.jobs.as_ref().ok_or("the render runner stopped")?.try_send(job).map_err(|_| "a render is already starting".to_string())
    }

    /// Ask the renderer to stop; it removes its partial file. One still
    /// starting is told as soon as it runs.
    pub fn cancel(&mut self) {
        match &mut self.stdin {
            // One short line into a pipe the child keeps reading.
            Some(stdin) => {
                let _ = writeln!(stdin, "cancel");
                let _ = stdin.flush();
            }
            None => self.cancel_asked = true,
        }
    }

    pub fn try_recv(&mut self) -> Option<RenderEvent> {
        match self.events.try_recv() {
            Ok(RenderEvent::Started(stdin)) => {
                self.stdin = Some(stdin);
                if std::mem::take(&mut self.cancel_asked) {
                    self.cancel();
                }
                None
            }
            Ok(event) => {
                if matches!(event, RenderEvent::Done | RenderEvent::Failed(_)) {
                    self.stdin = None;
                    self.cancel_asked = false;
                }
                Some(event)
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }

    pub fn stop(&mut self) {
        // A closed stdin makes the child exit (`--parent-pipe`); a closed
        // job channel ends the runner.
        self.stdin = None;
        self.jobs.take();
    }
}

fn run(jobs: Receiver<RenderJob>, events: SyncSender<RenderEvent>) {
    let mut store = super::store::LazyStoreMedia::new();
    while let Ok(job) = jobs.recv() {
        let log = job.dir.join("render.log");
        let listing = job.dir.join("render.job");
        let child = binary().and_then(|binary| {
            let started = std::time::Instant::now();
            std::fs::create_dir_all(&job.dir).map_err(|error| format!("{}: {error}", job.dir.display()))?;
            job.cut.job(&mut store, &job.dir)?.write(&listing)?;
            log!("edit: render job written in {:.0} ms", started.elapsed().as_secs_f64() * 1000.0);
            let stderr = std::fs::File::create(&log).map_err(|error| format!("{}: {error}", log.display()))?;
            let mut command = Command::new(binary);
            // In segments, several at once (by the core count), joined
            // without re-encoding.
            command.arg("export").arg("--job").arg(&listing);
            if let Some(fps) = job.fps {
                command.arg("--fps").arg(fps.to_string());
            }
            command.arg("--loudness").arg(job.loudness.map_or_else(|| "off".to_string(), |lufs| lufs.to_string()));
            if !job.burn {
                command.arg("--no-burn");
            }
            if let Some(bitrate) = job.bitrate {
                command.arg("--bitrate").arg(bitrate.to_string());
            }
            if job.blend {
                command.arg("--blend");
            }
            if job.draft {
                command.arg("--quality").arg("draft");
            }
            command
                .arg("--out").arg(&job.out)
                .arg("--parent-pipe")
                .env("MAKEPAD_HIDE_WINDOWS", "1")
                // Offscreen frames back to back, not one per display refresh.
                .env("MAKEPAD_PAINT_INTERVAL", "0.001")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::from(stderr))
                .spawn()
                .map_err(|error| format!("could not start the renderer: {error}"))
        });
        let mut child = match child {
            Ok(child) => child,
            Err(error) => {
                let _ = events.send(RenderEvent::Failed(error));
                continue;
            }
        };
        if let Some(stdin) = child.stdin.take() {
            let _ = events.send(RenderEvent::Started(stdin));
        }
        // Progress until the child closes its output (it has exited, or
        // is about to); a full event queue drops a progress line only.
        if let Some(stdout) = child.stdout.take() {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some((frame, frames)) = parse_progress(&line) {
                    let _ = events.try_send(RenderEvent::Progress { frame, frames });
                }
            }
        }
        let status = child.wait();
        let error = std::fs::read_to_string(&log).ok().and_then(|text| {
            text.lines().rev().find_map(|line| line.split_once("edit-render: FAILED: ").map(|(_, error)| error.to_string()))
        });
        let event = match status {
            Ok(status) if status.success() && job.out.is_file() => RenderEvent::Done,
            Ok(status) => RenderEvent::Failed(error.unwrap_or_else(|| format!("the renderer exited with {status}"))),
            Err(error) => RenderEvent::Failed(error.to_string()),
        };
        let _ = events.send(event);
    }
}

#[cfg(test)]
mod tests {
    use super::parse_progress;

    #[test]
    fn progress_lines_parse() {
        assert_eq!(parse_progress("edit-render: frame 12 / 120 (1.4 s)"), Some((12, 120)));
        assert_eq!(parse_progress("edit-render: captions x.srt"), None);
    }
}
