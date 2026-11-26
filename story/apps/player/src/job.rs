//! Work off the UI thread: a worker of the task pool, natively and in the
//! threaded browser build. A plate's load, its scenes' kernel compiles,
//! the song's and the images' decodes and an environment's prefilter never
//! hold up a frame (a document's load was up to half a second of the
//! browser's main thread, the song's decode three). They take locks the UI
//! thread also takes (ids, kernels, fonts); the browser's main thread may
//! not block on one, and there the film's waits spin instead (the web
//! export's optimiser, `waits`). Without workers (a single-threaded browser
//! build) a job runs on the UI thread, one per display frame
//! ([`new_frame`]), when its result is first asked for.

use makepad_widgets::*;

/// A display frame starts: the next queued job may run.
pub fn new_frame() {
    frame_jobs_new_frame();
    FRAME.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

static FRAME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The display frame (counted by [`new_frame`]): work that takes one slice
/// per frame keys on it.
pub fn frame() -> u64 {
    FRAME.load(std::sync::atomic::Ordering::Relaxed)
}

pub struct Job<T>(FrameJob<T>);

impl<T: Send + 'static> Job<T> {
    pub fn run(cx: &mut Cx, f: impl FnOnce() -> T + Send + 'static) -> Job<T> {
        Job(cx.task_pool().submit_frame_job(Class::Background, f).expect("player task pool is open"))
    }

    pub fn try_take(&mut self) -> Option<T> {
        self.0.try_take().and_then(Result::ok)
    }

    pub fn is_finished(&mut self) -> bool {
        self.0.is_finished()
    }
}
