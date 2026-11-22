//! The simulation host (`SimHost`): the live bench of the committed design and everything views
//! read from it. ELEC's host calls `frame(cx, &design)` on UI signals and its own frames after the
//! snapshot drain (it writes the `ElecLive` global's `sim`) and `on_commit(&snap)` per committed
//! generation; the app routes views' `SimAction`s to `act` (or a frame's actions to `handle_actions`).
//!
//! - `host`: lowering on a job, the one long-lived live worker (`run_live`), commands by design id
//!   (a value site's `SetParam` goes out on the pointer sample), the consumer trace store and its
//!   rolling record, scope sinks installed while a scope is visible, scrubbing, the ghost.
//! - `rows`: the four row textures of `shared/live-rows.md` (nets, segments, parts, and pins from
//!   a co-simulated device's state), written once a frame; no view writes anything live into its
//!   instances.
//! - `events`: the firmware event store a co-simulated device's events roll through.
//! - `rate`: the rate the run reaches over its newest publications and what limits it.
//! - `currents`: segment currents from pin currents (`live::net`) on a long-lived worker, per
//!   `LiveValues` and per scrub position, for the trees views register in `SimLive::trees`
//!   (`fab_elec_view::live::segs`, the one segment-row allocator).
//! - `actions`: `elec.sim.*` and `elec.bench.run` in the action catalog.
//!
//! Threads: the UI thread runs `frame` and never waits; the solver and the currents worker are
//! long-lived loops fed by bounded channels; lowering runs as a job. `Spawn` says where each runs.

pub mod actions;
pub mod currents;
pub mod edits;
pub mod events;
pub mod host;
pub mod mini;
pub mod rate;
pub mod rows;
pub mod take;

pub use host::{host_pool, host_rounds, setup_line, warm_ahead_of, LedLive, RunPool, SimHost, SimStatus};
pub use rate::{Limit, Split};

use std::sync::Arc;

/// Where the host's work runs. The app's runs workers on the Cx's thread spawner and jobs on its
/// task pool; a test's runs both on std threads.
pub trait Spawn {
    /// A long-lived loop fed by a channel (the solver, the currents worker): one per loop, never
    /// per job.
    fn worker(&self, name: &'static str, f: Box<dyn FnOnce() + Send>);
    /// A job (a lowering, a bench run); its result comes back through a channel the host drains.
    fn job(&self, f: Box<dyn FnOnce() + Send>);
    /// What a worker calls after it hands the host something new. A UI host signals its event
    /// loop (`Event::Signal`); jobs need none when the spawner's pool signals their completion.
    fn waker(&self) -> Arc<dyn Fn() + Send + Sync> {
        Arc::new(|| {})
    }
    /// The helper cores a live run in rounds posts its blocks to (`LiveSim::set_helpers`), or
    /// none: every block then steps on the run's own worker.
    fn helpers(&self) -> Option<fab_elec_sim::rounds::Helpers> {
        None
    }
    /// The cores a live run's planner makes and tests its other states on beside the thread that
    /// plans (`BlockPlanner::pool`): background jobs, below the run's own packs. None: the planning
    /// thread does it all.
    fn background(&self) -> Option<fab_elec_sim::rounds::Helpers> {
        None
    }
    /// What a live run heard as it is made does on its worker as that starts and ends
    /// (`LiveSim::set_heard_priority`): the app's raises the worker and its packs
    /// (`RunPool::heard_priority`). None: the worker keeps the priority it was spawned with.
    fn heard(&self) -> Option<Box<dyn FnMut(bool) + Send>> {
        None
    }
    /// Where checks go ahead of the planner's work under a budget. With none, checks share
    /// `background`'s posting.
    fn checks(&self) -> Option<fab_elec_sim::rounds::CheckPost> {
        None
    }
    /// Where a Fast run's sparse LUs compile their plans (`Options::plan_jit`): a pool's
    /// background jobs, never the run's worker or the UI thread. None: every plan stays
    /// interpreted.
    fn plan_jit(&self) -> Option<fab_elec_sim::sparse::compiled::PlanJit> {
        None
    }
    /// What `helpers` and `background` run on, in a few words, for a run's set-up line
    /// (`setup_line`).
    fn pool_name(&self) -> String {
        "the spawner's threads".into()
    }
}
