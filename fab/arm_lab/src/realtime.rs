//! The cell's real-time path: the loop that streams the drives' setpoints and takes in the
//! tracker and the IMUs ([`fab_arms::planner::Planner::run_until`], through the cell link to every device driver it
//! calls) must never wait for a compile while it drives a cell. It runs on the cell's loop
//! thread ([`CellLoop`]), a real-time thread for good; marking the thread's body compiles
//! everything it can reach ahead, in the background. A dry run or a real build arms only once
//! all of it is compiled ([`RtReady::Ready`], the strict ready: never because a wait ran out),
//! and while armed a live edit of that code waits for the disarm (other code patches as usual).
//! The loop thread starts only then, so it never meets a compile; any it does meet counts in
//! [`misses`]. A simulation runs the same loop on the pool and arms nothing. A native build has
//! compiled it all.

use crate::lab::Session;
use fab_robot::safety::{Mode, RtReady};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::OnceLock;

/// The marked real-time path.
pub struct RtPath {
    #[cfg(relax)]
    ticket: std::relax::Realtime,
}

/// While it lives, the real-time path is armed: a live edit of its code is held.
pub struct RtArmed {
    #[cfg(relax)]
    _guard: std::relax::RealtimeArmed,
}

impl RtPath {
    /// Mark the loop as real-time code: compiled ahead from now on, `wake` called (on a thread of
    /// its own) once it is.
    pub fn mark(wake: Option<extern "C" fn()>) -> RtPath {
        #[cfg(relax)]
        {
            RtPath { ticket: std::relax::realtime(&cell_loop, wake) }
        }
        #[cfg(not(relax))]
        {
            let _ = wake;
            RtPath {}
        }
    }

    /// All of it compiled, nothing it can reach left to compile on a call.
    pub fn compiled(&self) -> bool {
        #[cfg(relax)]
        {
            self.ticket.compiled()
        }
        #[cfg(not(relax))]
        {
            true
        }
    }

    /// How many fns it can reach would still compile on their first call.
    pub fn lazy(&self) -> u32 {
        #[cfg(relax)]
        {
            self.ticket.lazy()
        }
        #[cfg(not(relax))]
        {
            0
        }
    }

    /// What the safety gate is told.
    pub fn ready(&self) -> RtReady {
        if self.compiled() {
            RtReady::Ready
        } else {
            RtReady::Lazy
        }
    }

    /// Arm it, once compiled.
    pub fn arm(&self) -> Option<RtArmed> {
        if !self.compiled() {
            return None;
        }
        #[cfg(relax)]
        {
            Some(RtArmed { _guard: self.ticket.arm() })
        }
        #[cfg(not(relax))]
        {
            Some(RtArmed {})
        }
    }

    /// Arm it for a run in `mode`: a simulation arms nothing; a dry run or a real build only once
    /// it is compiled, holding the guard for the run.
    pub fn arm_for(&self, mode: Mode) -> Result<Option<RtArmed>, String> {
        match mode {
            Mode::Sim => Ok(None),
            Mode::DryRun | Mode::Real => {
                // Counted before the check: the compile runs on in the background, and a count
                // taken after a failed check can already be 0.
                let lazy = self.lazy();
                self.arm().map(Some).ok_or_else(|| match lazy {
                    0 => "The real-time path is not compiled ahead yet".to_string(),
                    n => format!("The real-time path is not compiled ahead yet ({n} fns still lazy)"),
                })
            }
        }
    }

    /// The cell's loop thread, once all it can reach is compiled (started then, the first time).
    pub fn loop_thread(&self) -> Option<CellLoop> {
        if !self.compiled() {
            return None;
        }
        static LOOP: OnceLock<Result<Sender<Stretch>, String>> = OnceLock::new();
        let started = LOOP.get_or_init(|| {
            let (tx, rx) = channel();
            std::thread::Builder::new().name("fab cell loop".into()).spawn(move || cell_loop(rx)).map(|_| tx).map_err(|e| format!("The cell's loop thread did not start: {e}"))
        });
        started.as_ref().ok().map(|tx| CellLoop(tx.clone()))
    }

    /// The panel's line: compiled or how much is left, and the lazy compiles real-time threads
    /// met so far.
    pub fn line(&self) -> String {
        let misses = misses();
        let state = if !cfg!(relax) {
            "compiled ahead (a native build)".to_string()
        } else if self.compiled() {
            "compiled ahead".to_string()
        } else {
            format!("compiling, {} fns left", self.lazy())
        };
        format!("{state}; {misses} lazy compiles on real-time threads")
    }
}

/// Lazy compiles real-time threads met so far (each stalled its thread): 0 is the goal.
pub fn misses() -> u64 {
    #[cfg(relax)]
    {
        std::relax::realtime_misses()
    }
    #[cfg(not(relax))]
    {
        0
    }
}

/// Holes Relax found in what the mark reached (code that could meet a lazy compile on a
/// real-time thread before it is walked): 0 is the invariant. A native build has none.
pub fn holes() -> u64 {
    #[cfg(relax)]
    {
        std::relax::realtime_holes()
    }
    #[cfg(not(relax))]
    {
        0
    }
}

/// One stretch of a job for the loop thread: the session run to cell time `until`; it comes
/// back on `done`, with whether the job ended and the lazy compiles the thread met on the way.
struct Stretch {
    session: Box<Session>,
    until: f64,
    done: Sender<(Box<Session>, bool, u64)>,
}

/// The cell's loop thread's body, the marked root: real-time from its first line, it runs each
/// stretch it is handed. It never ends (a thread's exit runs code nothing marked).
fn cell_loop(jobs: Receiver<Stretch>) {
    #[cfg(relax)]
    std::relax::realtime_thread();
    while let Ok(Stretch { mut session, until, done }) = jobs.recv() {
        let before = misses();
        let s = &mut *session;
        let ended = s.planner.run_until(&mut s.sim, until);
        let met = misses() - before;
        let _ = done.send((session, ended, met));
    }
}

/// A handle on the cell's loop thread.
#[derive(Clone)]
pub struct CellLoop(Sender<Stretch>);

impl CellLoop {
    /// Run `session` to cell time `until` on the loop thread: back with whether the job ended
    /// and the lazy compiles the thread met (0 is the goal).
    pub fn run(&self, session: Box<Session>, until: f64) -> Result<(Box<Session>, bool, u64), String> {
        let (done, back) = channel();
        self.0.send(Stretch { session, until, done }).map_err(|_| "The cell's loop thread has stopped".to_string())?;
        back.recv().map_err(|_| "The cell's loop thread stopped in the stretch".to_string())
    }
}
